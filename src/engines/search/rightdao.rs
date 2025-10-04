use reqwest::Url;
use scraper::Selector;
use std::sync::LazyLock;

use crate::{
    engines::{EngineResponse, RequestResponse, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts},
};

static RESULT_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("div.item").unwrap());
static TITLE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("div.title").unwrap());
static HREF_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a[href]").unwrap());
static DESCRIPTION_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div.description").unwrap());

pub fn request(query: &str) -> RequestResponse {
    CLIENT
        .get(Url::parse_with_params("https://rightdao.com/search", &[("q", query)]).unwrap())
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
