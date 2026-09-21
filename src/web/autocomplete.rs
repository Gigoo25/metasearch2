use std::{
    collections::HashMap,
    sync::LazyLock,
    time::{Duration, Instant},
};

use axum::{extract::Query, http::StatusCode, response::IntoResponse, Extension, Json};
use parking_lot::Mutex;
use tracing::error;

use crate::{config::Config, engines};

/// Autocomplete results are cached briefly so typing doesn't hammer the
/// suggestion endpoints.
const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_MAX: usize = 256;

static CACHE: LazyLock<Mutex<HashMap<String, (Instant, Vec<String>)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_get(query: &str) -> Option<Vec<String>> {
    let mut cache = CACHE.lock();
    if let Some((at, results)) = cache.get(query) {
        if at.elapsed() < CACHE_TTL {
            return Some(results.clone());
        }
        cache.remove(query);
    }
    None
}

fn cache_put(query: &str, results: &[String]) {
    let mut cache = CACHE.lock();
    if cache.len() >= CACHE_MAX {
        let now = Instant::now();
        cache.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL);
        if cache.len() >= CACHE_MAX {
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            }
        }
    }
    cache.insert(query.to_string(), (Instant::now(), results.to_vec()));
}

pub async fn route(
    Query(params): Query<HashMap<String, String>>,
    Extension(config): Extension<Config>,
) -> impl IntoResponse {
    let query = params
        .get("q")
        .cloned()
        .unwrap_or_default()
        .replace('\n', " ");

    if let Some(cached) = cache_get(&query) {
        return (StatusCode::OK, Json((query, cached)));
    }

    let res = match engines::autocomplete(&config, &query).await {
        Ok(res) => res,
        Err(err) => {
            error!("Autocomplete error for {query}: {err}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json((query, vec![])));
        }
    };
    cache_put(&query, &res);

    (StatusCode::OK, Json((query, res)))
}
