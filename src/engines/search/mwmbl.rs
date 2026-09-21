//! [Mwmbl](https://mwmbl.org) is a community-maintained small-web search
//! index. It has a JSON API, so no html scraping is needed.

use serde::Deserialize;
use url::Url;

use crate::engines::{EngineResponse, EngineSearchResult, RequestResponse, CLIENT};

pub async fn request(query: &str) -> RequestResponse {
    CLIENT
        .get(
            Url::parse_with_params("https://api.mwmbl.org/api/v1/search/", &[("s", query)])
                .unwrap(),
        )
        .into()
}

#[derive(Deserialize)]
struct MwmblText {
    value: String,
}

#[derive(Deserialize)]
struct MwmblResult {
    url: String,
    #[serde(default)]
    title: Vec<MwmblText>,
    #[serde(default)]
    extract: Vec<MwmblText>,
}

pub fn parse_response(body: &str) -> eyre::Result<EngineResponse> {
    let results: Vec<MwmblResult> = serde_json::from_str(body)?;

    let mut response = EngineResponse::new();
    for result in results {
        let title = result
            .title
            .into_iter()
            .map(|part| part.value)
            .collect::<String>();
        let title = title.trim().to_string();
        // results without a title are mostly just urls mwmbl has seen
        if title.is_empty() {
            continue;
        }

        let description = result
            .extract
            .into_iter()
            .map(|part| part.value)
            .collect::<String>();

        response.search_results.push(EngineSearchResult {
            url: result.url,
            title,
            description: description.trim().to_string(),
        });
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_results() {
        let body = r#"[
            {"url": "https://example.com/a", "title": [{"value": "Example", "is_bold": true}], "extract": [{"value": "A small web page."}], "source": "user"},
            {"url": "https://example.com/b", "title": [], "extract": [], "source": "crawl"},
            {"url": "http://http-only.example.com/", "title": [{"value": "HTTP site", "is_bold": false}], "extract": [], "source": "user"}
        ]"#;
        let response = parse_response(body).unwrap();
        assert_eq!(response.search_results.len(), 2);
        assert_eq!(response.search_results[0].url, "https://example.com/a");
        assert_eq!(response.search_results[0].title, "Example");
        assert_eq!(response.search_results[0].description, "A small web page.");
        assert_eq!(
            response.search_results[1].url,
            "http://http-only.example.com/"
        );
    }
}
