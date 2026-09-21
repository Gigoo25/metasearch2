//! Cache of merged search responses, stored in the sqlite database alongside
//! the personal index.
//!
//! Entries are keyed by the normalized query, the search tab and a fingerprint
//! of the config that affects result ranking (engines, url rules and site
//! rules), so editing the config doesn't serve results ranked with the old
//! settings.
//!
//! Fresh entries are served without contacting any engine. Expired entries may
//! still be served when every engine fails (which is what happens when the
//! machine has no internet access), and entries older than the stale window
//! are dropped along with the index's own retention pass.

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{Engine, Infobox, ResponseForTab, SearchTab};
use crate::{
    config::{CacheConfig, Config},
    db,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    pub query: String,
    pub tab: SearchTab,
    pub config_hash: u64,
}

impl CacheKey {
    fn database_key(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
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

/// Bump this whenever the ranking/scoring logic changes, so cached responses
/// scored with the old logic are not served.
const RANKING_VERSION: u32 = 4;

/// Hashes the parts of the config that affect the contents of a response. The
/// result must be deterministic across restarts, otherwise old cache entries
/// would either never be hit or (worse) be hit after a config change.
#[must_use]
pub fn config_fingerprint(config: &Config) -> u64 {
    let mut hasher = DefaultHasher::new();

    RANKING_VERSION.hash(&mut hasher);

    config.safe_search.hash(&mut hasher);

    for rule in &config.site_rules {
        rule.host.hash(&mut hasher);
        rule.weight.to_bits().hash(&mut hasher);
    }

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

    let database_key = key.database_key();
    let cleanup_key = database_key.clone();
    let stored = tokio::task::spawn_blocking(move || db::cache_get(&database_key))
        .await
        .ok()?
        .ok()??;

    match serde_json::from_str::<CachedSearch>(&stored.response) {
        Ok(entry) => Some(entry),
        Err(err) => {
            warn!("Failed to read cached search: {err}");
            let _ = db::cache_delete(&cleanup_key);
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

    let entry = CachedSearch {
        stored_at: now_unix(),
        response,
        infobox,
    };
    let json = match serde_json::to_string(&entry) {
        Ok(json) => json,
        Err(err) => {
            warn!("Failed to serialize cached search: {err}");
            return;
        }
    };

    let database_key = key.database_key();
    let max_entries = config.max_entries as u64;
    let stale_ttl_secs = config.stale_ttl_secs;
    let result = tokio::task::spawn_blocking(move || {
        db::cache_put(&database_key, &json)?;
        db::cache_evict(max_entries, stale_ttl_secs)
    })
    .await;

    match result {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => warn!("Failed to store cached search: {err}"),
        Err(err) => warn!("Cache task failed: {err}"),
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(normalize_query("  ray \n charles "), "ray charles");
        assert_eq!(normalize_query("ray charles"), "ray charles");
    }

    #[test]
    fn database_keys_are_stable() {
        let key = CacheKey {
            query: "ray charles".to_string(),
            tab: SearchTab::All,
            config_hash: 1,
        };
        assert_eq!(key.database_key(), key.database_key());
        assert_ne!(
            key.database_key(),
            CacheKey {
                query: "ray charles".to_string(),
                tab: SearchTab::Images,
                config_hash: 1,
            }
            .database_key()
        );
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
}
