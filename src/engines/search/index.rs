//! Matches the query against the personal result index: everything this
//! instance has seen in previous searches.

use crate::{
    db,
    engines::{EngineResponse, EngineSearchResult, RequestResponse, SearchQuery},
};

pub async fn request(query: &SearchQuery) -> RequestResponse {
    let query_text = query.query.clone();
    let results = tokio::task::spawn_blocking(move || db::search(&query_text, 20)).await;

    let Ok(Ok(results)) = results else {
        return RequestResponse::None;
    };

    let mut response = EngineResponse::new();
    for result in results {
        response.search_results.push(EngineSearchResult {
            url: result.url,
            title: result.title,
            description: result.description,
        });
    }

    RequestResponse::Instant(Box::new(response))
}
