use reqwest::Url;
use scraper::Selector;
use serde::Deserialize;
use std::sync::LazyLock;
use tracing::error;

use crate::{
    engines::{Engine, EngineResponse, RequestResponse, SearchQuery, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts},
};

static RESULT_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("section.search-result").unwrap());
static TITLE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("h2").unwrap());
static HREF_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a[href]").unwrap());
static DESCRIPTION_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("p.description").unwrap());

#[derive(Deserialize)]
pub struct MarginaliaConfig {
    pub args: MarginaliaArgs,
}
#[derive(Deserialize)]
pub struct MarginaliaArgs {
    pub profile: String,
    pub js: String,
    pub adtech: String,
}

pub fn request(query: &SearchQuery) -> RequestResponse {
    // if the query is more than 3 words or has any special characters then abort
    if query.split_whitespace().count() > 3
        || !query.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ')
    {
        return RequestResponse::None;
    }

    let config_toml = query.config.engines.get(Engine::Marginalia).extra.clone();
    let config: MarginaliaConfig = match toml::Value::Table(config_toml).try_into() {
        Ok(args) => args,
        Err(err) => {
            error!("Failed to parse Marginalia config: {err}");
            return RequestResponse::None;
        }
    };

    CLIENT
        .get(
            Url::parse_with_params(
                "https://old-search.marginalia.nu/search",
                &[
                    ("query", query.query.as_str()),
                    ("profile", config.args.profile.as_str()),
                    ("js", config.args.js.as_str()),
                    ("adtech", config.args.adtech.as_str()),
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
