use maud::{html, PreEscaped};
use scraper::{Html, Selector};
use serde::Deserialize;
use tracing::error;

use crate::engines::{postsearch::RESULT_WINDOW, Engine, HttpResponse, Response, CLIENT};

#[derive(Deserialize)]
pub struct MdnConfig {
    pub max_sections: usize,
}

pub async fn request(response: &Response) -> Option<wreq::RequestBuilder> {
    for search_result in response.search_results.iter().take(RESULT_WINDOW) {
        let url = &search_result.result.url;
        // any mdn page works; locale-less urls (e.g. /Web/API/Fetch_API)
        // redirect to the real page
        if url.starts_with("https://developer.mozilla.org/")
            && url.len() > "https://developer.mozilla.org/".len()
        {
            return Some(CLIENT.get(url.as_str()));
        }
    }

    None
}

pub fn parse_response(
    HttpResponse { res, body, config }: &HttpResponse,
) -> Option<PreEscaped<String>> {
    let config_toml = config.engines.get(Engine::Mdn).extra.clone();
    let config: MdnConfig = match toml::Value::Table(config_toml).try_into() {
        Ok(args) => args,
        Err(err) => {
            error!("Failed to parse Mdn config: {err}");
            return None;
        }
    };

    let url = res.url().clone();

    let dom = Html::parse_document(body);

    let page_title = dom
        .select(&Selector::parse("h1").unwrap())
        .next()?
        .text()
        .collect::<String>()
        .trim()
        .to_string();

    // mdn switched the article body from .section-content to
    // section.content-section; support both
    let mut sections: Vec<_> = dom
        .select(&Selector::parse("section.content-section").unwrap())
        .collect();
    if sections.is_empty() {
        sections = dom
            .select(&Selector::parse(".section-content").unwrap())
            .collect();
    }

    let max_sections = if config.max_sections == 0 {
        usize::MAX
    } else {
        config.max_sections
    };

    let doc_html = sections
        .iter()
        .map(|doc| doc.inner_html())
        .take(max_sections)
        .collect::<Vec<_>>()
        .join("<br>");

    let doc_html = ammonia::Builder::default()
        .link_rel(None)
        .url_relative(ammonia::UrlRelative::RewriteWithBase(url.clone()))
        .clean(&doc_html)
        .to_string();

    Some(html! {
        h2 {
            a href=(url) { (page_title) }
        }
        div.infobox-mdn-article {
            (PreEscaped(doc_html))
        }
    })
}
