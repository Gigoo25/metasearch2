use url::Url;

use crate::{
    engines::{EngineResponse, RequestResponse, SearchQuery, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts},
    safe_search::SafeSearch,
};

pub async fn request(query: &SearchQuery) -> RequestResponse {
    CLIENT
        .get(search_url(&query.query, query.config.safe_search))
        .into()
}

fn search_url(query: &str, safe_search: SafeSearch) -> Url {
    Url::parse_with_params(
        "https://search.brave.com/search",
        &[("q", query), ("safesearch", safe_search.brave())],
    )
    .unwrap()
}

pub fn parse_response(body: &str) -> eyre::Result<EngineResponse> {
    parse_html_response_with_opts(
        body,
        ParseOpts::new()
            .result("main .snippet[data-pos]:not(.standalone)")
            .title(".title")
            .href("a")
            .description(".generic-snippet, .video-snippet > .snippet-description"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_search_is_sent_to_brave() {
        assert_eq!(
            search_url("q", SafeSearch::Strict)
                .query_pairs()
                .find(|(key, _)| key == "safesearch"),
            Some(("safesearch".into(), "strict".into()))
        );
        assert_eq!(
            search_url("q", SafeSearch::Off)
                .query_pairs()
                .find(|(key, _)| key == "safesearch")
                .map(|(_, value)| value.into_owned()),
            Some("off".to_string())
        );
    }
}
