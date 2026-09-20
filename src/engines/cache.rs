//! Disk-persisted cache of merged search results.
//!
//! Entries are keyed by the normalized query, the search tab and a fingerprint
//! of the config that affects result ranking (engines and url rules), so
//! editing the config doesn't serve results ranked with the old settings.
//!
//! Fresh entries are served without contacting any engine. Expired entries may
//! still be served when every engine fails (which is what happens when the
//! machine has no internet access).

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{Engine, Infobox, ResponseForTab, SearchTab};
use crate::config::{CacheConfig, Config};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    pub query: String,
    pub tab: SearchTab,
    pub config_hash: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CachedSearch {
    pub stored_at: u64,
    pub response: ResponseForTab,
    pub infobox: Option<Infobox>,
}

/// Normalizes a query so that queries that only differ in whitespace share a
/// cache entry.
#[must_use]
pub fn normalize_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Hashes the parts of the config that affect the contents of a response. The
/// result must be deterministic across restarts, otherwise old cache entries
/// would either never be hit or (worse) be hit after a config change.
#[must_use]
pub fn config_fingerprint(config: &Config) -> u64 {
    let mut hasher = DefaultHasher::new();

    let mut engines: Vec<_> = config.engines.map.iter().collect();
    engines.sort_by_key(|(engine, _)| **engine);
    for (engine, engine_config) in engines {
        engine.id().hash(&mut hasher);
        engine_config.enabled.hash(&mut hasher);
        engine_config.weight.to_bits().hash(&mut hasher);
        // Table is ordered, but format it to avoid depending on its Hash impl
        format!("{:?}", engine_config.extra).hash(&mut hasher);
    }

    for (from, to) in &config.urls.replace {
        from.host.hash(&mut hasher);
        from.path.hash(&mut hasher);
        to.host.hash(&mut hasher);
        to.path.hash(&mut hasher);
    }
    for (url, weight) in &config.urls.weight {
        url.host.hash(&mut hasher);
        url.path.hash(&mut hasher);
        weight.to_bits().hash(&mut hasher);
    }

    hasher.finish()
}

/// Whether a response can be safely served to a different request later.
///
/// Answers from engines that depend on the requester (their ip or user-agent)
/// must never be cached, and answers that contain the current time or a random
/// value would be stale immediately.
#[must_use]
pub fn is_cacheable(response: &ResponseForTab) -> bool {
    fn is_volatile(engine: &Engine) -> bool {
        matches!(
            engine,
            Engine::Ip | Engine::Useragent | Engine::Timezone | Engine::Fend
        )
    }

    match response {
        ResponseForTab::All(response) => {
            !response
                .answer
                .as_ref()
                .is_some_and(|a| is_volatile(&a.engine))
                && !response
                    .infobox
                    .as_ref()
                    .is_some_and(|i| is_volatile(&i.engine))
                && !response
                    .featured_snippet
                    .as_ref()
                    .is_some_and(|s| is_volatile(&s.engine))
        }
        ResponseForTab::Images(_) => true,
    }
}

#[must_use]
pub fn age_secs(entry: &CachedSearch) -> u64 {
    now_unix().saturating_sub(entry.stored_at)
}

pub async fn load(config: &CacheConfig, key: &CacheKey) -> Option<CachedSearch> {
    if !config.enabled {
        return None;
    }

    let path = path_for(&config.resolved_dir(), key);
    let bytes = tokio::fs::read(&path).await.ok()?;
    match serde_json::from_slice::<CachedSearch>(&bytes) {
        Ok(entry) => Some(entry),
        Err(err) => {
            warn!("Failed to read cached search {path:?}: {err}");
            let _ = tokio::fs::remove_file(&path).await;
            None
        }
    }
}

pub async fn store(
    config: &CacheConfig,
    key: &CacheKey,
    response: ResponseForTab,
    infobox: Option<Infobox>,
) {
    if !config.enabled {
        return;
    }

    let dir = config.resolved_dir();
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        warn!("Failed to create cache directory {dir:?}: {err}");
        return;
    }

    let entry = CachedSearch {
        stored_at: now_unix(),
        response,
        infobox,
    };
    let json = match serde_json::to_vec(&entry) {
        Ok(json) => json,
        Err(err) => {
            warn!("Failed to serialize cached search: {err}");
            return;
        }
    };

    // write to a temporary file first so a crash doesn't leave a half-written
    // entry behind
    let path = path_for(&dir, key);
    let tmp_path = path.with_extension("json.tmp");
    if let Err(err) = tokio::fs::write(&tmp_path, &json).await {
        warn!("Failed to write cached search {tmp_path:?}: {err}");
        return;
    }
    if let Err(err) = tokio::fs::rename(&tmp_path, &path).await {
        warn!("Failed to move cached search into place {path:?}: {err}");
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return;
    }

    evict(&dir, config.max_entries).await;
}

async fn evict(dir: &Path, max_entries: usize) {
    if max_entries == 0 {
        return;
    }

    let mut entries = Vec::new();
    let Ok(mut read_dir) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            let modified = entry
                .metadata()
                .await
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            entries.push((modified, path));
        }
    }

    if entries.len() <= max_entries {
        return;
    }

    entries.sort_by_key(|(modified, _)| *modified);
    for (_, path) in entries.iter().take(entries.len() - max_entries) {
        if let Err(err) = tokio::fs::remove_file(path).await {
            warn!("Failed to evict cached search {path:?}: {err}");
        }
    }
}

fn path_for(dir: &Path, key: &CacheKey) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    dir.join(format!("{:016x}.json", hasher.finish()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use maud::PreEscaped;

    use super::*;
    use crate::engines::{Answer, EngineSearchResult, Response, SearchResult};

    fn test_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("metasearch-cache-test-{name}-{unique}"))
    }

    fn test_config(dir: &Path) -> CacheConfig {
        CacheConfig {
            enabled: true,
            dir: dir.to_string_lossy().into_owned(),
            fresh_ttl_secs: 600,
            stale_ttl_secs: 604800,
            max_entries: 10,
        }
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(normalize_query("  ray \n charles "), "ray charles");
        assert_eq!(normalize_query("ray charles"), "ray charles");
    }

    #[test]
    fn fingerprint_depends_on_ranking_config() {
        let config = Config::default();
        let fingerprint = config_fingerprint(&config);
        assert_eq!(fingerprint, config_fingerprint(&Config::default()));

        let mut changed = Config::default();
        let mut engines = changed.engines.as_ref().clone();
        engines.map.get_mut(&Engine::Google).unwrap().weight = 99.0;
        changed.engines = Arc::new(engines);
        assert_ne!(fingerprint, config_fingerprint(&changed));
    }

    #[tokio::test]
    async fn store_and_load_roundtrip() {
        let dir = test_dir("roundtrip");
        let config = test_config(&dir);
        let key = CacheKey {
            query: "ray charles".to_string(),
            tab: SearchTab::All,
            config_hash: 1,
        };
        let response = ResponseForTab::All(Response {
            search_results: vec![SearchResult {
                result: EngineSearchResult {
                    url: "http://localhost:8080/content/zimfile/A/Article".to_string(),
                    title: "Article".to_string(),
                    description: "description".to_string(),
                },
                engines: [Engine::Kiwix].into_iter().collect(),
                score: 1.0,
            }],
            featured_snippet: None,
            answer: Some(Answer {
                html: PreEscaped("<b>42</b>".to_string()),
                engine: Engine::Numbat,
            }),
            infobox: None,
            config: Arc::new(Config::default()),
        });

        assert!(load(&config, &key).await.is_none());
        store(&config, &key, response, None).await;

        let entry = load(&config, &key).await.expect("cache entry");
        assert!(age_secs(&entry) <= 1);
        let ResponseForTab::All(response) = entry.response else {
            panic!("expected an 'all' response");
        };
        assert_eq!(response.search_results.len(), 1);
        assert!(response.search_results[0].engines.contains(&Engine::Kiwix));
        assert_eq!(response.answer.expect("answer").engine, Engine::Numbat);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn evicts_old_entries() {
        let dir = test_dir("evict");
        let mut config = test_config(&dir);
        config.max_entries = 1;

        for i in 0..3u64 {
            let key = CacheKey {
                query: format!("query {i}"),
                tab: SearchTab::All,
                config_hash: i,
            };
            let response = ResponseForTab::All(Response {
                search_results: vec![],
                featured_snippet: None,
                answer: None,
                infobox: None,
                config: Arc::new(Config::default()),
            });
            store(&config, &key, response, None).await;
        }

        let mut count = 0;
        let mut read_dir = tokio::fs::read_dir(&dir).await.unwrap();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            if entry.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
                count += 1;
            }
        }
        assert_eq!(count, 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
