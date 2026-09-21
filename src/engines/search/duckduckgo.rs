use url::Url;

use crate::{
    engines::{EngineResponse, RequestResponse, SearchQuery, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts, QueryMethod},
    safe_search::SafeSearch,
};

pub async fn request(query: &SearchQuery) -> RequestResponse {
    CLIENT
        .get(search_url(&query.query, query.config.safe_search))
        .into()
}

fn search_url(query: &str, safe_search: SafeSearch) -> Url {
    Url::parse_with_params(
        "https://html.duckduckgo.com/html/",
        &[("q", query), ("kp", safe_search.duckduckgo())],
    )
    .unwrap()
}

pub fn parse_response(body: &str) -> eyre::Result<EngineResponse> {
    parse_html_response_with_opts(
        body,
        ParseOpts::new()
            .result("div.result")
            .title("a.result__a")
            .href(QueryMethod::Manual(Box::new(|el| {
                let href = el
                    .select(&scraper::Selector::parse("a.result__a").unwrap())
                    .next()
                    .and_then(|node| node.value().attr("href"))
                    .unwrap_or_default();
                Ok(clean_url(href))
            })))
            .description(".result__snippet"),
    )
}

/// DuckDuckGo wraps result urls in `//duckduckgo.com/l/?uddg=<urlencoded url>`.
fn clean_url(href: &str) -> String {
    let href = match href.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => href.to_string(),
    };

    let Ok(url) = Url::parse(&href) else {
        return href;
    };

    if url.host_str() == Some("duckduckgo.com") && url.path() == "/l/" {
        if let Some(target) = url
            .query_pairs()
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value)
        {
            return target.to_string();
        }
    }

    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_search_is_sent_to_duckduckgo() {
        let params = |safe_search| {
            search_url("q", safe_search)
                .query_pairs()
                .find(|(key, _)| key == "kp")
                .map(|(_, value)| value.into_owned())
        };
        assert_eq!(params(SafeSearch::Off), Some("-2".to_string()));
        assert_eq!(params(SafeSearch::Moderate), Some("-1".to_string()));
        assert_eq!(params(SafeSearch::Strict), Some("1".to_string()));
    }

    const BODY: &str = r#"<html><body>
        <div class="result results_links web-result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage%3Fa%3D1&amp;rut=xyz">Example title</a>
            <a class="result__snippet">A snippet about the result.</a>
        </div>
        <div class="result results_links web-result">
            <a class="result__a" href="https://direct.example.org/page">Direct result</a>
            <a class="result__snippet">Another snippet.</a>
        </div>
        <div class="result result--ad">
            <a class="result__a" href="https://ads.example.com">Ad without snippet</a>
        </div>
    </body></html>"#;

    #[test]
    fn parses_results_and_cleans_redirect_urls() {
        let response = parse_response(BODY).unwrap();
        assert_eq!(response.search_results.len(), 2);
        assert_eq!(response.search_results[0].title, "Example title");
        assert_eq!(
            response.search_results[0].url,
            "https://example.com/page?a=1"
        );
        assert_eq!(
            response.search_results[0].description,
            "A snippet about the result."
        );
        assert_eq!(
            response.search_results[1].url,
            "https://direct.example.org/page"
        );
    }

    #[test]
    fn keeps_unknown_urls_as_is() {
        assert_eq!(clean_url("not a url"), "not a url");
    }
}
