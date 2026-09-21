//! SQLite storage for site rules and the personal result index.
//!
//! The database is opened once at startup and shared through a global mutex.
//! It runs in WAL mode (readers do not block the writer) and uses an
//! external-content FTS5 table with triggers, so text search stays fast with
//! hundreds of thousands of results. Schema changes are appended to
//! [`MIGRATIONS`]; the `user_version` pragma tracks what has been applied, so
//! upgrading is just adding a new string to that array.
//!
//! Write volume is low (one small transaction per search), and the periodic
//! retention prune keeps the table bounded by `[database] max_results`.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        LazyLock, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eyre::{eyre, Result};
use parking_lot::{Mutex, RwLock};
use rusqlite::{params, Connection};
use tracing::{info, warn};

use crate::config::SiteRule;

/// Append new migrations here; they run in order and bump `user_version`.
const MIGRATIONS: &[&str] = &[
    r#"
    CREATE TABLE site_rules (
        host TEXT PRIMARY KEY,
        weight REAL NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE TABLE results (
        url TEXT PRIMARY KEY,
        host TEXT NOT NULL,
        title TEXT NOT NULL,
        description TEXT NOT NULL,
        engines TEXT NOT NULL,
        first_seen INTEGER NOT NULL,
        last_seen INTEGER NOT NULL,
        seen_count INTEGER NOT NULL DEFAULT 1
    );
    CREATE INDEX results_host_idx ON results(host);
    CREATE INDEX results_last_seen_idx ON results(last_seen);

    CREATE TABLE queries (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        query TEXT NOT NULL UNIQUE,
        first_seen INTEGER NOT NULL,
        last_seen INTEGER NOT NULL,
        seen_count INTEGER NOT NULL DEFAULT 1
    );

    CREATE TABLE result_queries (
        url TEXT NOT NULL REFERENCES results(url) ON DELETE CASCADE,
        query_id INTEGER NOT NULL REFERENCES queries(id) ON DELETE CASCADE,
        PRIMARY KEY (url, query_id)
    ) WITHOUT ROWID;
    CREATE INDEX result_queries_query_idx ON result_queries(query_id);

    CREATE VIRTUAL TABLE results_fts USING fts5(
        title,
        description,
        content='results',
        content_rowid='rowid'
    );
    CREATE TRIGGER results_fts_ai AFTER INSERT ON results BEGIN
        INSERT INTO results_fts(rowid, title, description)
        VALUES (new.rowid, new.title, new.description);
    END;
    CREATE TRIGGER results_fts_ad AFTER DELETE ON results BEGIN
        INSERT INTO results_fts(results_fts, rowid, title, description)
        VALUES ('delete', old.rowid, old.title, old.description);
    END;
    CREATE TRIGGER results_fts_au AFTER UPDATE ON results BEGIN
        INSERT INTO results_fts(results_fts, rowid, title, description)
        VALUES ('delete', old.rowid, old.title, old.description);
        INSERT INTO results_fts(rowid, title, description)
        VALUES (new.rowid, new.title, new.description);
    END;
    "#,
    // v2: the result cache moved from json files into this database
    r#"
    CREATE TABLE response_cache (
        key TEXT PRIMARY KEY,
        stored_at INTEGER NOT NULL,
        response TEXT NOT NULL
    );
    CREATE INDEX response_cache_stored_at_idx ON response_cache(stored_at);
    "#,
    // v3: ui settings moved from the settings cookie into the database
    r#"
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    );
    "#,
];

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();
static RULES: RwLock<Vec<SiteRule>> = RwLock::new(Vec::new());
static SETTINGS: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static MAX_RESULTS: AtomicU64 = AtomicU64::new(200_000);
static MAX_AGE_DAYS: AtomicU64 = AtomicU64::new(180);
static WRITES: AtomicU64 = AtomicU64::new(0);

/// A result as stored in (and returned from) the personal index.
#[derive(Debug, Clone)]
pub struct IndexedResult {
    pub url: String,
    pub title: String,
    pub description: String,
    pub engines: Vec<String>,
}

/// Opens (creating if needed) the database and applies pending migrations.
pub fn init(
    path: &Path,
    max_results: u64,
    max_age_days: u64,
    synchronous: &str,
    quick_check: bool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    info!(
        "database: {} (synchronous={synchronous}, quick_check={quick_check})",
        path.display()
    );

    let mut connection = open_recovered(path, synchronous, quick_check)?;

    if let Some(backup) = backup_before_migration(path, &connection)? {
        info!("backed up the database before migrating to {backup:?}");
    }
    migrate(&mut connection)?;

    MAX_RESULTS.store(max_results.max(1_000), Ordering::Relaxed);
    MAX_AGE_DAYS.store(max_age_days, Ordering::Relaxed);
    *RULES.write() = load_rules(&connection)?;
    *SETTINGS.write() = load_settings(&connection)?;
    let _ = DB.set(Mutex::new(connection));
    Ok(())
}

fn open(path: &Path, synchronous: &str) -> Result<Connection> {
    let synchronous = if synchronous.eq_ignore_ascii_case("normal") {
        "NORMAL"
    } else {
        "FULL"
    };
    let connection = Connection::open(path)?;
    // don't fail instantly when another writer is busy, and keep WAL so the
    // index engine can read while a search is being recorded
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", synchronous)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    // keep the write-ahead log from growing without bound
    connection.pragma_update(None, "journal_size_limit", 8 * 1024 * 1024)?;
    Ok(connection)
}

/// Opens the database, moving a corrupt file aside instead of refusing to
/// start. The corrupt copy is kept for manual recovery.
fn open_recovered(path: &Path, synchronous: &str, quick_check: bool) -> Result<Connection> {
    if let Ok(connection) = open(path, synchronous) {
        if !quick_check || integrity_ok(&connection) {
            return Ok(connection);
        }
        drop(connection);
    }

    warn!(
        "database failed to open or failed its integrity check; moving it aside and starting fresh"
    );
    quarantine(path)?;
    open(path, synchronous)
}

/// `PRAGMA quick_check` reads every page but skips the slower cross-checks of
/// `integrity_check`; it catches corruption without delaying startup too much.
fn integrity_ok(connection: &Connection) -> bool {
    let mut statement = match connection.prepare("PRAGMA quick_check(1)") {
        Ok(statement) => statement,
        Err(err) => {
            warn!("failed to run the database integrity check: {err}");
            return false;
        }
    };
    match statement.query_row([], |row| row.get::<_, String>(0)) {
        Ok(result) => result == "ok",
        Err(err) => {
            warn!("database integrity check failed: {err}");
            false
        }
    }
}

/// Moves a corrupt database (and its wal/shm files) aside so the instance can
/// start with a fresh one and the old file can be recovered manually.
fn quarantine(path: &Path) -> Result<()> {
    let suffix = format!("corrupt-{}", now());
    for candidate in [path.to_path_buf(), wal_path(path), shm_path(path)] {
        if candidate.exists() {
            let moved = candidate.with_extension(format!(
                "{}.{}",
                candidate
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("db"),
                suffix
            ));
            if let Err(err) = std::fs::rename(&candidate, &moved) {
                warn!("failed to move corrupt database file {candidate:?}: {err}");
            }
        }
    }
    Ok(())
}

fn wal_path(path: &Path) -> std::path::PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push("-wal");
    std::path::PathBuf::from(path)
}

fn shm_path(path: &Path) -> std::path::PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push("-shm");
    std::path::PathBuf::from(path)
}

/// Copies the database with sqlite's backup api before pending migrations run,
/// so a bad migration can be rolled back by hand. Returns the backup path when
/// one was written.
fn backup_before_migration(
    path: &Path,
    connection: &Connection,
) -> Result<Option<std::path::PathBuf>> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version as usize >= MIGRATIONS.len() || !path.exists() {
        return Ok(None);
    }

    let backup_path = path.with_extension("pre-migration.bak");
    let mut backup_connection = Connection::open(&backup_path)?;
    {
        let backup = rusqlite::backup::Backup::new(connection, &mut backup_connection)?;
        backup.run_to_completion(64, Duration::from_millis(20), None)?;
    }
    Ok(Some(backup_path))
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", (index + 1) as i64)?;
        transaction.commit()?;
    }
    Ok(())
}

//

pub fn site_rules() -> Vec<SiteRule> {
    RULES.read().clone()
}

pub fn set_site_rule(host: &str, weight: f64) -> Result<()> {
    with_conn(|connection| set_rule(connection, host, weight))?;
    refresh_rules()
}

pub fn remove_site_rule(host: &str) -> Result<()> {
    with_conn(|connection| remove_rule(connection, host))?;
    refresh_rules()
}

pub fn clear_site_rules() -> Result<()> {
    with_conn(|connection| {
        connection.execute("DELETE FROM site_rules", [])?;
        Ok(())
    })?;
    refresh_rules()
}

fn refresh_rules() -> Result<()> {
    *RULES.write() = with_conn(|connection| load_rules(connection))?;
    Ok(())
}

fn load_rules(connection: &Connection) -> Result<Vec<SiteRule>> {
    let mut statement =
        connection.prepare_cached("SELECT host, weight FROM site_rules ORDER BY host")?;
    let rules = statement
        .query_map([], |row| {
            Ok(SiteRule {
                host: row.get(0)?,
                weight: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rules)
}

fn set_rule(connection: &Connection, host: &str, weight: f64) -> Result<()> {
    let host = normalize_host(host)?;
    let weight = weight.clamp(-100., 100.);
    connection
        .prepare_cached(
            "INSERT INTO site_rules (host, weight, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(host) DO UPDATE SET weight=excluded.weight, updated_at=excluded.updated_at",
        )?
        .execute(params![host, weight, now()])?;
    Ok(())
}

fn remove_rule(connection: &Connection, host: &str) -> Result<()> {
    let host = normalize_host(host)?;
    connection
        .prepare_cached("DELETE FROM site_rules WHERE host = ?1")?
        .execute([host])?;
    Ok(())
}

fn normalize_host(host: &str) -> Result<String> {
    let host = host.trim().trim_start_matches('.').to_lowercase();
    let valid = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']' | '_'));
    if !valid {
        return Err(eyre!("invalid host"));
    }
    Ok(host)
}

//

/// Stores every result of a search, deduplicated by url.
pub fn record_search(query: &str, results: &[IndexedResult]) -> Result<()> {
    if results.is_empty() && query.is_empty() {
        return Ok(());
    }

    with_conn(|connection| {
        record(connection, query, results)?;
        if WRITES.fetch_add(1, Ordering::Relaxed).is_multiple_of(64) {
            prune(
                connection,
                MAX_RESULTS.load(Ordering::Relaxed),
                MAX_AGE_DAYS.load(Ordering::Relaxed),
            )?;
        }
        Ok(())
    })
}

fn record(connection: &mut Connection, query: &str, results: &[IndexedResult]) -> Result<()> {
    let now = now();
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    {
        transaction
            .prepare_cached(
                "INSERT INTO queries (query, first_seen, last_seen, seen_count) VALUES (?1, ?2, ?2, 1)
                 ON CONFLICT(query) DO UPDATE SET last_seen=excluded.last_seen, seen_count=seen_count+1",
            )?
            .execute(params![query, now])?;

        let mut result_statement = transaction.prepare_cached(
            "INSERT INTO results (url, host, title, description, engines, first_seen, last_seen, seen_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1)
             ON CONFLICT(url) DO UPDATE SET title=excluded.title, description=excluded.description,
                 engines=excluded.engines, last_seen=excluded.last_seen, seen_count=seen_count+1",
        )?;
        let mut link_statement = transaction.prepare_cached(
            "INSERT OR IGNORE INTO result_queries (url, query_id)
             SELECT ?1, id FROM queries WHERE query = ?2",
        )?;

        for result in results {
            result_statement.execute(params![
                result.url,
                host_of(&result.url),
                result.title,
                result.description,
                result.engines.join(","),
                now
            ])?;
            link_statement.execute(params![result.url, query])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Full-text search over everything the instance has seen.
pub fn search(query: &str, limit: usize) -> Result<Vec<IndexedResult>> {
    with_conn(|connection| search_inner(connection, query, limit))
}

fn search_inner(connection: &Connection, query: &str, limit: usize) -> Result<Vec<IndexedResult>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|term| term.chars().count() >= 2)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut statement = connection.prepare_cached(
        "SELECT results.url, results.title, results.description, results.engines
         FROM results_fts
         JOIN results ON results.rowid = results_fts.rowid
         WHERE results_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![terms.join(" OR "), limit as i64], |row| {
        Ok(IndexedResult {
            url: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            engines: row
                .get::<_, String>(3)?
                .split(',')
                .filter(|engine| !engine.is_empty())
                .map(str::to_string)
                .collect(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Deletes results not seen for `max_age_days` days (0 disables age expiry),
/// then the oldest results until at most `max_results` are left.
fn prune(connection: &mut Connection, max_results: u64, max_age_days: u64) -> Result<u64> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut removed: i64 = 0;

    if max_age_days > 0 {
        let cutoff = now() - (max_age_days as i64) * 86_400;
        removed += transaction
            .prepare_cached("DELETE FROM results WHERE last_seen < ?1")?
            .execute([cutoff])? as i64;
    }

    let max_results = max_results as i64;
    let count: i64 = transaction.query_row("SELECT COUNT(*) FROM results", [], |row| row.get(0))?;
    if count > max_results {
        let mut statement = transaction.prepare_cached(
            "DELETE FROM results WHERE rowid IN (
                SELECT rowid FROM results ORDER BY last_seen ASC LIMIT ?1
            )",
        )?;
        let mut count_removed: i64 = 0;
        while count - count_removed > max_results {
            let batch = (count - count_removed - max_results).clamp(1, 10_000);
            count_removed += statement.execute([batch])? as i64;
        }
        removed += count_removed;
    }

    if removed > 0 {
        // queries that no longer have any result associated with them
        transaction.execute(
            "DELETE FROM queries WHERE NOT EXISTS (
                SELECT 1 FROM result_queries WHERE result_queries.query_id = queries.id
            )",
            [],
        )?;
    }
    transaction.commit()?;

    if removed > 0 {
        connection.execute_batch("PRAGMA optimize")?;
        warn!("pruned {removed} results from the personal index");
    }
    Ok(removed as u64)
}

//

/// A raw settings value (for example the `ui` json blob).
pub fn setting(key: &str) -> Option<String> {
    SETTINGS.read().get(key).cloned()
}

pub fn set_setting(key: &str, value: &str) -> Result<()> {
    with_conn(|connection| {
        connection
            .prepare_cached(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            )?
            .execute(params![key, value, now()])?;
        Ok(())
    })?;
    SETTINGS.write().insert(key.to_string(), value.to_string());
    Ok(())
}

fn load_settings(connection: &Connection) -> Result<HashMap<String, String>> {
    let mut statement = connection.prepare_cached("SELECT key, value FROM settings")?;
    let settings = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(settings)
}

//

/// A cached merged response serialized as json.
pub struct CachedResponse {
    pub stored_at: i64,
    pub response: String,
}

pub fn cache_get(key: &str) -> Result<Option<CachedResponse>> {
    with_conn(|connection| cache_get_inner(connection, key))
}

fn cache_get_inner(connection: &Connection, key: &str) -> Result<Option<CachedResponse>> {
    let mut statement = connection
        .prepare_cached("SELECT stored_at, response FROM response_cache WHERE key = ?1")?;
    let mut rows = statement.query([key])?;
    match rows.next()? {
        Some(row) => Ok(Some(CachedResponse {
            stored_at: row.get(0)?,
            response: row.get(1)?,
        })),
        None => Ok(None),
    }
}

pub fn cache_put(key: &str, response: &str) -> Result<()> {
    with_conn(|connection| cache_put_inner(connection, key, response, now()))
}

fn cache_put_inner(
    connection: &Connection,
    key: &str,
    response: &str,
    stored_at: i64,
) -> Result<()> {
    connection
        .prepare_cached(
            "INSERT INTO response_cache (key, stored_at, response) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET stored_at=excluded.stored_at, response=excluded.response",
        )?
        .execute(params![key, stored_at, response])?;
    Ok(())
}

pub fn cache_delete(key: &str) -> Result<()> {
    with_conn(|connection| {
        connection
            .prepare_cached("DELETE FROM response_cache WHERE key = ?1")?
            .execute([key])?;
        Ok(())
    })
}

/// Drops cached responses older than `max_age_secs`, then the oldest ones
/// beyond `max_entries`.
pub fn cache_evict(max_entries: u64, max_age_secs: u64) -> Result<u64> {
    with_conn(|connection| cache_evict_inner(connection, max_entries, max_age_secs))
}

fn cache_evict_inner(
    connection: &mut Connection,
    max_entries: u64,
    max_age_secs: u64,
) -> Result<u64> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut removed: i64 = 0;

    if max_age_secs > 0 {
        let cutoff = now() - max_age_secs as i64;
        removed += transaction
            .prepare_cached("DELETE FROM response_cache WHERE stored_at < ?1")?
            .execute([cutoff])? as i64;
    }

    let max_entries = max_entries as i64;
    let count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM response_cache", [], |row| row.get(0))?;
    if count > max_entries {
        let mut statement = transaction.prepare_cached(
            "DELETE FROM response_cache WHERE key IN (
                SELECT key FROM response_cache ORDER BY stored_at ASC LIMIT ?1
            )",
        )?;
        let mut count_removed: i64 = 0;
        while count - count_removed > max_entries {
            let batch = (count - count_removed - max_entries).clamp(1, 10_000);
            count_removed += statement.execute([batch])? as i64;
        }
        removed += count_removed;
    }

    transaction.commit()?;
    Ok(removed as u64)
}

//

fn with_conn<T>(function: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
    let database = DB
        .get()
        .ok_or_else(|| eyre!("index database is not initialized"))?;
    let mut connection = database.lock();
    function(&mut connection)
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .unwrap_or_default()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        migrate(&mut connection).unwrap();
        connection
    }

    fn result(url: &str, title: &str, description: &str) -> IndexedResult {
        IndexedResult {
            url: url.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            engines: vec!["brave".to_string()],
        }
    }

    #[test]
    fn migrations_set_user_version() {
        let mut connection = test_db();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        // running again is a no-op
        migrate(&mut connection).unwrap();
    }

    #[test]
    fn record_and_search() {
        let mut connection = test_db();
        let results = vec![
            result(
                "https://example.com/a",
                "Rust async runtime",
                "all about tokio",
            ),
            result("https://example.com/b", "Gardening", "growing tomatoes"),
        ];
        record(&mut connection, "rust async", &results).unwrap();

        let found = search_inner(&connection, "rust async", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url, "https://example.com/a");
        assert_eq!(found[0].engines, vec!["brave".to_string()]);

        // repeated searches bump seen_count
        record(&mut connection, "rust async", &results).unwrap();
        let seen: i64 = connection
            .query_row(
                "SELECT seen_count FROM results WHERE url = ?1",
                ["https://example.com/a"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(seen, 2);

        // the update triggers keep the fts table in sync
        record(
            &mut connection,
            "rust async",
            &[result("https://example.com/a", "Replaced", "no keywords")],
        )
        .unwrap();
        assert!(search_inner(&connection, "tokio", 10).unwrap().is_empty());
        assert_eq!(search_inner(&connection, "replaced", 10).unwrap().len(), 1);
    }

    #[test]
    fn site_rules_roundtrip() {
        let connection = test_db();
        set_rule(&connection, "Example.com", 2.0).unwrap();
        set_rule(&connection, ".spam.example", 0.0).unwrap();
        let rules = load_rules(&connection).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].host, "example.com");

        set_rule(&connection, "example.com", 0.5).unwrap();
        let rules = load_rules(&connection).unwrap();
        assert_eq!(rules.len(), 2);
        assert!(rules
            .iter()
            .any(|rule| rule.host == "example.com" && rule.weight == 0.5));

        remove_rule(&connection, "example.com").unwrap();
        assert_eq!(load_rules(&connection).unwrap().len(), 1);
        assert!(normalize_host(" ").is_err());
        assert!(normalize_host("bad host!").is_err());
    }

    #[test]
    fn prunes_oldest_results() {
        let mut connection = test_db();
        for i in 0..10 {
            record(
                &mut connection,
                "q",
                &[result(&format!("https://example.com/{i}"), "x", "y")],
            )
            .unwrap();
        }
        let removed = prune(&mut connection, 4, 0).unwrap();
        assert_eq!(removed, 6);
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM results", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 4);
        // the cascade removed the links too
        let links: i64 = connection
            .query_row("SELECT COUNT(*) FROM result_queries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(links, 4);
    }

    #[test]
    fn prunes_results_older_than_max_age() {
        let mut connection = test_db();
        let old = now() - 40 * 86_400;
        connection
            .execute(
                "INSERT INTO results (url, host, title, description, engines, first_seen, last_seen)
                 VALUES ('https://old.example.com', 'old.example.com', 'old', 'old', 'brave', ?1, ?1)",
                [old],
            )
            .unwrap();
        record(
            &mut connection,
            "new",
            &[result("https://new.example.com", "new", "new")],
        )
        .unwrap();

        let removed = prune(&mut connection, 100, 30).unwrap();
        assert_eq!(removed, 1);
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM results", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn response_cache_roundtrip_and_eviction() {
        let mut connection = test_db();
        assert!(cache_get_inner(&connection, "a").unwrap().is_none());

        cache_put_inner(&connection, "a", "first", now()).unwrap();
        let entry = cache_get_inner(&connection, "a").unwrap().unwrap();
        assert_eq!(entry.response, "first");

        // overwriting replaces the response and timestamp
        cache_put_inner(&connection, "a", "second", now() + 10).unwrap();
        assert_eq!(
            cache_get_inner(&connection, "a").unwrap().unwrap().response,
            "second"
        );

        cache_put_inner(&connection, "b", "b", now() - 100).unwrap();
        cache_put_inner(&connection, "c", "c", now() - 50).unwrap();
        let removed = cache_evict_inner(&mut connection, 2, 0).unwrap();
        assert_eq!(removed, 1);
        assert!(cache_get_inner(&connection, "b").unwrap().is_none());

        // age expiry drops everything older than a second (a is in the future)
        let removed = cache_evict_inner(&mut connection, 100, 1).unwrap();
        assert_eq!(removed, 1);
        assert!(cache_get_inner(&connection, "c").unwrap().is_none());
    }

    #[test]
    fn integrity_check_passes_on_fresh_database() {
        let connection = test_db();
        assert!(integrity_ok(&connection));
    }

    #[test]
    fn quarantines_a_corrupt_database() {
        let dir = std::env::temp_dir().join(format!("metasearch-db-corrupt-{}", now()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("metasearch.db");
        std::fs::write(&path, b"this is not a sqlite database").unwrap();

        let connection = open_recovered(&path, "full", true).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
        drop(connection);

        let quarantined = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_name().to_string_lossy().contains("corrupt"));
        assert!(quarantined);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backs_up_before_pending_migrations() {
        let dir = std::env::temp_dir().join(format!("metasearch-db-backup-{}", now()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("metasearch.db");
        let mut connection = Connection::open(&path).unwrap();

        // a fresh database has migrations pending, so it gets a backup
        let backup = backup_before_migration(&path, &connection).unwrap();
        assert!(backup.as_ref().is_some_and(|backup| backup.exists()));

        migrate(&mut connection).unwrap();
        // and nothing is pending after migrating
        assert!(backup_before_migration(&path, &connection)
            .unwrap()
            .is_none());

        drop(connection);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handles_many_results() {
        let connection = test_db();
        let transaction = connection.unchecked_transaction().unwrap();
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO results (url, host, title, description, engines, first_seen, last_seen)
                     VALUES (?1, 'example.com', ?2, 'a description', 'brave', 0, 0)",
                )
                .unwrap();
            for i in 0..20_000 {
                statement
                    .execute(params![
                        format!("https://example.com/{i}"),
                        format!("page {i} about rust")
                    ])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();

        let started = std::time::Instant::now();
        let found = search_inner(&connection, "rust", 20).unwrap();
        assert_eq!(found.len(), 20);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
