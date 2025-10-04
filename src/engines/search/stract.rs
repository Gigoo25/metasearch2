use reqwest::Url;
use scraper::Selector;
use std::sync::LazyLock;

use crate::{
    engines::{EngineResponse, RequestResponse, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts},
};

static RESULT_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("div.grid.w-full.grid-cols-1.space-y-10.place-self-start > div > div.flex.min-w-0.grow.flex-col").unwrap()
});
static TITLE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a[title]").unwrap());
static HREF_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a[href]").unwrap());
static DESCRIPTION_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("#snippet-text").unwrap());

pub fn request(query: &str) -> RequestResponse {
    CLIENT
        .get(
            Url::parse_with_params(
                "https://stract.com/search",
                &[
                    ("ss", "false"),
                    // this is not a tracking parameter or token
                    // this is stract's default value for the search rankings parameter
                    ("sr", "N4IgNglg1gpgJiAXAbQLoBoRwgZ0rBFDEAIzAHsBjApNAXyA"),
                    ("q", query),
                    ("optic", ""),
                ],
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
