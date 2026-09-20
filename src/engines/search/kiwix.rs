//! Searches a local [kiwix-serve](https://github.com/kiwix/kiwix-tools)
//! instance, which serves ZIM archives (offline Wikipedia and friends).
//!
//! kiwix-serve doesn't have a JSON search API, but its `/search` endpoint can
//! return RSS XML (`format=xml`), which is what this engine parses.

use std::{sync::LazyLock, time::Duration};

use eyre::{eyre, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;
use tracing::error;
use url::Url;

use crate::{
    config::Config,
    engines::{
        Engine, EngineResponse, EngineSearchResult, HttpResponse, RequestResponse, SearchQuery,
    },
};

/// kiwix-serve can take a while to answer the first search, since it has to
/// open the fulltext searcher for every book, so it gets a longer timeout than
/// the web engines.
pub static KIWIX_CLIENT: LazyLock<wreq::Client> = LazyLock::new(|| {
    wreq::ClientBuilder::new()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap()
});

/// Set once kiwix-serve is up and its searchers have been used at least once.
/// The container starts metasearch immediately (so the web ui doesn't have to
/// wait for kiwix), and searches wait on this before querying kiwix.
static READY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// Waits until kiwix-serve is reachable and warms up its fulltext searchers.
/// Safe to call from multiple tasks; the work happens once.
pub async fn ensure_ready(config: &Config) {
    READY
        .get_or_init(|| async {
            if !config.engines.get(Engine::Kiwix).enabled {
                return;
            }
            let Some(base) = internal_url(config) else {
                return;
            };

            // wait for kiwix to accept requests; loading a big library can take
            // a moment, especially from slow disks
            for _ in 0..240 {
                let catalog = format!("{base}/catalog/v2/entries");
                if KIWIX_CLIENT
                    .get(&catalog)
                    .send()
                    .await
                    .is_ok_and(|res| res.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            // open the searchers once so the first real search is less cold
            let warmup = format!("{base}/search?pattern=a&format=xml&pageLength=1");
            let _ = KIWIX_CLIENT.get(&warmup).send().await;
        })
        .await;
}

#[derive(Deserialize)]
pub struct KiwixConfig {
    #[serde(default = "default_url")]
    pub url: String,
    /// The url used when building links to kiwix content. Defaults to `url`,
    /// which only makes sense when the same address is reachable from both the
    /// server and the browser. An empty string keeps kiwix's own relative
    /// links (e.g. `/kiwix/content/...`) untouched, which is what the bundled
    /// container uses.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Restrict the search to one book, by its kiwix book name (the `name`
    /// attribute in `/catalog/v2/entries`). Empty means all books.
    #[serde(default)]
    pub book: String,
    #[serde(default = "default_page_length")]
    pub page_length: usize,
}

fn default_url() -> String {
    "http://127.0.0.1:8080".to_string()
}

fn default_page_length() -> usize {
    25
}

fn kiwix_config(config: &Config) -> Option<KiwixConfig> {
    let table = config.engines.get(Engine::Kiwix).extra.clone();
    match toml::Value::Table(table).try_into() {
        Ok(config) => Some(config),
        Err(err) => {
            error!("Failed to parse Kiwix config: {err}");
            None
        }
    }
}

/// The url that kiwix-serve is reachable at from metasearch itself. This is
/// used for search requests and by the `/kiwix` reverse proxy, unlike the url
/// that appears in links shown to the browser.
pub fn internal_url(config: &Config) -> Option<String> {
    kiwix_config(config)
        .map(|config| config.url.trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

pub async fn request(query: &SearchQuery) -> RequestResponse {
    // searches may run while the container is still bringing kiwix up
    ensure_ready(&query.config).await;

    let Some(config) = kiwix_config(&query.config) else {
        return RequestResponse::None;
    };

    let page_length = config.page_length.to_string();
    let mut params = vec![
        ("pattern", query.query.as_str()),
        ("format", "xml"),
        ("start", "0"),
        ("pageLength", page_length.as_str()),
    ];
    if !config.book.is_empty() {
        params.push(("books.filter.name", config.book.as_str()));
    }

    match Url::parse_with_params(
        &format!("{}/search", config.url.trim_end_matches('/')),
        &params,
    ) {
        Ok(url) => KIWIX_CLIENT.get(url).into(),
        Err(err) => {
            error!("Failed to build Kiwix request url: {err}");
            RequestResponse::None
        }
    }
}

pub fn parse_with_config(res: &HttpResponse) -> Result<EngineResponse> {
    let Some(config) = kiwix_config(&res.config) else {
        return Ok(EngineResponse::new());
    };

    let link_base = config
        .public_url
        .as_deref()
        .unwrap_or(&config.url)
        .trim_end_matches('/');

    parse_xml(&res.body, link_base)
}

#[derive(Default)]
struct Item {
    title: String,
    url: String,
    description: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Title,
    Link,
    Description,
}

fn parse_xml(body: &str, base_url: &str) -> Result<EngineResponse> {
    let mut reader = Reader::from_str(body);
    let mut response = EngineResponse::new();
    let mut item: Option<Item> = None;
    let mut field: Option<Field> = None;
    let mut in_book = false;

    loop {
        match reader.read_event()? {
            Event::Start(element) => match element.name().as_ref() {
                "item" => item = Some(Item::default()),
                "book" => in_book = true,
                "title" if item.is_some() && !in_book => field = Some(Field::Title),
                "link" if item.is_some() => field = Some(Field::Link),
                "description" if item.is_some() => field = Some(Field::Description),
                _ => {}
            },
            Event::End(element) => match element.name().as_ref() {
                "item" => {
                    if let Some(item) = item.take() {
                        if let Some(result) = into_search_result(item, base_url) {
                            response.search_results.push(result);
                        }
                    }
                }
                "book" => in_book = false,
                "title" | "link" | "description" => field = None,
                _ => {}
            },
            Event::Text(text) => {
                let text = match quick_xml::escape::unescape(&text) {
                    Ok(text) => text,
                    Err(err) => return Err(eyre!("invalid kiwix xml text: {err}")),
                };
                push_text(item.as_mut(), field, &text);
            }
            Event::CData(text) => push_text(item.as_mut(), field, &text),
            Event::GeneralRef(reference) => {
                if reference.is_char_ref() {
                    match reference.resolve_char_ref() {
                        Ok(Some(character)) => {
                            let mut buf = [0; 4];
                            push_text(item.as_mut(), field, character.encode_utf8(&mut buf));
                        }
                        Ok(None) => {}
                        Err(err) => {
                            return Err(eyre!("invalid kiwix xml character reference: {err}"))
                        }
                    }
                } else {
                    let replacement = match reference.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    };
                    push_text(item.as_mut(), field, replacement);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(response)
}

fn push_text(item: Option<&mut Item>, field: Option<Field>, text: &str) {
    let Some(item) = item else {
        return;
    };
    match field {
        Some(Field::Title) => item.title.push_str(text),
        Some(Field::Link) => item.url.push_str(text),
        Some(Field::Description) => item.description.push_str(text),
        None => {}
    }
}

fn into_search_result(item: Item, base_url: &str) -> Option<EngineSearchResult> {
    let title = item.title.trim().to_string();
    let url = item.url.trim();
    if title.is_empty() || url.is_empty() {
        return None;
    }

    let url = if url.starts_with('/') {
        format!("{base_url}{url}")
    } else if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("{base_url}/{url}")
    };

    Some(EngineSearchResult {
        url,
        title,
        description: item
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Search: ray charles</title>
    <link>http://127.0.0.1:8080/search?pattern=ray+charles&amp;format=xml</link>
    <item>
      <title>Ray Charles</title>
      <link>/content/wikipedia_en_ray_charles/A/Ray_Charles</link>
      <description>Ray Charles was an <b>American</b> singer &amp; songwriter.</description>
      <book><title>Wikipedia</title></book>
      <wordCount>42</wordCount>
    </item>
    <item>
      <title>Ray</title>
      <link>/content/wikipedia_en_ray_charles/A/Ray</link>
      <description>A ray is a line.</description>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn keeps_relative_links_with_empty_public_url() {
        let response = parse_xml(BODY, "").unwrap();
        assert_eq!(
            response.search_results[0].url,
            "/content/wikipedia_en_ray_charles/A/Ray_Charles"
        );
    }

    #[test]
    fn parses_search_results() {
        let response = parse_xml(BODY, "http://127.0.0.1:8080").unwrap();
        assert_eq!(response.search_results.len(), 2);
        assert_eq!(response.search_results[0].title, "Ray Charles");
        assert_eq!(
            response.search_results[0].url,
            "http://127.0.0.1:8080/content/wikipedia_en_ray_charles/A/Ray_Charles"
        );
        assert_eq!(
            response.search_results[0].description,
            "Ray Charles was an American singer & songwriter."
        );
        assert_eq!(response.search_results[1].title, "Ray");
    }

    #[test]
    fn ignores_empty_items() {
        let body = "<rss><channel><item><title></title><link></link></item></channel></rss>";
        let response = parse_xml(body, "http://localhost:8080").unwrap();
        assert!(response.search_results.is_empty());
    }
}
