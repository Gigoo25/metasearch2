use reqwest::Url;
use scraper::Selector;
use std::sync::LazyLock;

use crate::{
    engines::{EngineResponse, RequestResponse, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts},
};

static RESULT_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("div.gs_r").unwrap());
static TITLE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("h3").unwrap());
static HREF_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("h3 > a[href]").unwrap());
static DESCRIPTION_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div.gs_rs").unwrap());

pub fn request(query: &str) -> RequestResponse {
    CLIENT
        .get(
            Url::parse_with_params(
                "https://scholar.google.com/scholar",
                &[("hl", "en"), ("as_sdt", "0,5"), ("q", query), ("btnG", "")],
            )
            .unwrap(),
        )
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
