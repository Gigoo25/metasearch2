use scraper::Selector;
use std::sync::LazyLock;
use url::Url;

use crate::{
    engines::{EngineResponse, RequestResponse, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts},
};

static RESULT_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("#results > .snippet[data-pos]:not(.standalone)").unwrap());
static TITLE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".title").unwrap());
static HREF_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a").unwrap());
static DESCRIPTION_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse(".snippet-content, .video-snippet > .snippet-description").unwrap()
});

pub fn request(query: &str) -> RequestResponse {
    // brave search doesn't support exact matching anymore, so disable it to not
    // pollute the results
    if query.chars().any(|c| c == '"') {
        return RequestResponse::None;
    }

    CLIENT
        .get(Url::parse_with_params("https://search.brave.com/search", &[("q", query)]).unwrap())
        .into()
}

pub fn parse_response(body: &str) -> eyre::Result<EngineResponse> {
    parse_html_response_with_opts(
        body,
        ParseOpts::new()
            .result(RESULT_SELECTOR.clone())
            .title(TITLE_SELECTOR.clone())
            .href(HREF_SELECTOR.clone())
            .description(DESCRIPTION_SELECTOR.clone()),
    )
}
