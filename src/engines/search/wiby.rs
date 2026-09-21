//! [Wiby](https://wiby.me) is a search engine for the small web (personal
//! pages, old-school sites). Its results page is plain html.

use url::Url;

use crate::{
    engines::{EngineResponse, RequestResponse, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts},
};

pub async fn request(query: &str) -> RequestResponse {
    CLIENT
        .get(Url::parse_with_params("https://wiby.me/", &[("q", query)]).unwrap())
        .into()
}

pub fn parse_response(body: &str) -> eyre::Result<EngineResponse> {
    parse_html_response_with_opts(
        body,
        ParseOpts::new()
            .result("blockquote")
            .title("a.tlink")
            .href("a.tlink")
            .description("p:not(.url)"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"<html><body>
        <blockquote>
            <a class="tlink" href="http://boredmob.com/dump/gnu/">GNU plus Linux</a><br>
            <p class="url">http://boredmob.com/dump/gnu/</p>
            <p> I'd just like to interject for a moment. </p>
        </blockquote>
        <blockquote>
            <a class="tlink" href="https://example.org/page">An example page</a><br>
            <p class="url">https://example.org/page</p>
            <p>Some description here.</p>
        </blockquote>
    </body></html>"#;

    #[test]
    fn parses_results() {
        let response = parse_response(BODY).unwrap();
        assert_eq!(response.search_results.len(), 2);
        assert_eq!(response.search_results[0].title, "GNU plus Linux");
        assert_eq!(
            response.search_results[0].url,
            "http://boredmob.com/dump/gnu"
        );
        assert_eq!(
            response.search_results[0].description.trim(),
            "I'd just like to interject for a moment."
        );
        assert_eq!(response.search_results[1].url, "https://example.org/page");
    }
}
