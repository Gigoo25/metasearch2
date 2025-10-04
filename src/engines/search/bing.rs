use base64::Engine;
use eyre::eyre;
use scraper::{ElementRef, Html, Selector};
use tracing::warn;
use url::Url;

use crate::{
    engines::{EngineImageResult, EngineImagesResponse, EngineResponse, SearchQuery, CLIENT},
    parse::{parse_html_response_with_opts, ParseOpts, QueryMethod},
};

use std::sync::LazyLock;

fn language_to_country(lang: &str) -> &'static str {
    match lang {
        "en" => "US",
        "de" => "DE",
        "fr" => "FR",
        "es" => "ES",
        "it" => "IT",
        "pt" => "PT",
        "ru" => "RU",
        "ja" => "JP",
        "ko" => "KR",
        "zh" => "CN",
        "pl" => "PL",
        "nl" => "NL",
        "sv" => "SE",
        "da" => "DK",
        "no" => "NO",
        "fi" => "FI",
        "cs" => "CZ",
        "sk" => "SK",
        "hu" => "HU",
        "tr" => "TR",
        "ar" => "SA",
        "he" => "IL",
        "hi" => "IN",
        "th" => "TH",
        "vi" => "VN",
        _ => "US",
    }
}

static RESULT_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("#b_results > li.b_algo").unwrap());
static TITLE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".b_algo h2 > a").unwrap());
static HREF_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a[href]").unwrap());
static DESC_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".b_caption > p, p.b_algoSlug, .b_caption .ipText").unwrap());
static IMAGE_CONTAINER_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".imgpt").unwrap());
static IMAGE_EL_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse(".iusc").unwrap());
static SIZE_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\d+)\s*[×x]\s*(\d+)").unwrap());

pub fn request(query: &SearchQuery) -> reqwest::RequestBuilder {
    let modified_query = if !query.config.language.is_empty() {
        let parts: Vec<&str> = query.config.language.split('-').collect();
        let lang = parts.first().unwrap_or(&"en").to_lowercase();
        let country = if parts.len() >= 2 {
            parts.last().unwrap().to_uppercase()
        } else {
            language_to_country(&lang).to_uppercase()
        };
        format!("{} language:{} loc:{}", query.query, lang, country)
    } else {
        query.query.clone()
    };
    let mut request = CLIENT.get(
        Url::parse_with_params(
            "https://www.bing.com/search",
            // filters=rcrse:"1" makes it not try to autocorrect
            &[("q", modified_query.as_str()), ("filters", "rcrse:\"1\"")],
        )
        .unwrap(),
    );

    if !query.config.language.is_empty() {
        let parts: Vec<&str> = query.config.language.split('-').collect();
        let lang = parts.first().unwrap_or(&"en").to_lowercase();
        let region = if parts.len() >= 2 {
            query.config.language.clone()
        } else {
            format!("{}-{}", lang, language_to_country(&lang))
        };
        let cookie = format!(
            "_EDGE_CD=m={}&u={}; _EDGE_S=mkt={}&ui={}",
            region, lang, region, lang
        );
        request = request.header("Cookie", cookie);
    }

    request
}

pub fn parse_response(body: &str) -> eyre::Result<EngineResponse> {
    parse_html_response_with_opts(
        body,
        ParseOpts::new()
            .result(RESULT_SELECTOR.clone())
            .title(TITLE_SELECTOR.clone())
            .href(QueryMethod::Manual(Box::new(|el: &ElementRef| {
                let url = el
                    .select(&HREF_SELECTOR)
                    .next()
                    .and_then(|n| n.value().attr("href"))
                    .unwrap_or_default();
                clean_url(url)
            })))
            .description(QueryMethod::Manual(Box::new(|el: &ElementRef| {
                let mut description = String::new();
                for inner_node in el
                    .select(&DESC_SELECTOR)
                    .next()
                    .map(|n| n.children().collect::<Vec<_>>())
                    .unwrap_or_default()
                {
                    match inner_node.value() {
                        scraper::Node::Text(t) => {
                            description.push_str(&t.text);
                        }
                        scraper::Node::Element(inner_el) => {
                            if !inner_el
                                .has_class("algoSlug_icon", scraper::CaseSensitivity::CaseSensitive)
                            {
                                let element_ref = ElementRef::wrap(inner_node).unwrap();
                                description.push_str(&element_ref.text().collect::<String>());
                            }
                        }
                        _ => {}
                    }
                }

                Ok(description)
            }))),
    )
}

pub fn request_images(query: &SearchQuery) -> reqwest::RequestBuilder {
    let modified_query = if !query.config.language.is_empty() {
        let parts: Vec<&str> = query.config.language.split('-').collect();
        let lang = parts.first().unwrap_or(&"en").to_lowercase();
        let country = if parts.len() >= 2 {
            parts.last().unwrap().to_uppercase()
        } else {
            language_to_country(&lang).to_uppercase()
        };
        format!("{} language:{} loc:{}", query.query, lang, country)
    } else {
        query.query.clone()
    };
    let mut request = CLIENT.get(
        Url::parse_with_params(
            "https://www.bing.com/images/async",
            &[
                ("q", modified_query.as_str()),
                ("async", "content"),
                ("first", "1"),
                ("count", "35"),
            ],
        )
        .unwrap(),
    );

    if !query.config.language.is_empty() {
        let parts: Vec<&str> = query.config.language.split('-').collect();
        let lang = parts.first().unwrap_or(&"en").to_lowercase();
        let region = if parts.len() >= 2 {
            query.config.language.clone()
        } else {
            format!("{}-{}", lang, language_to_country(&lang))
        };
        let cookie = format!(
            "_EDGE_CD=m={}&u={}; _EDGE_S=mkt={}&ui={}",
            region, lang, region, lang
        );
        request = request.header("Cookie", cookie);
    }

    request
}

#[tracing::instrument(skip(body))]
pub fn parse_images_response(body: &str) -> eyre::Result<EngineImagesResponse> {
    let dom = Html::parse_document(body);

    let mut image_results = Vec::new();

    for image_container_el in dom.select(&IMAGE_CONTAINER_SELECTOR) {
        let image_el = image_container_el
            .select(&IMAGE_EL_SELECTOR)
            .next()
            .ok_or_else(|| eyre!("no image element found"))?;

        // parse the "m" attribute as json
        let Some(data) = image_el.value().attr("m") else {
            // this is normal, i think
            continue;
        };
        let data = serde_json::from_str::<serde_json::Value>(data)?;
        let page_url = data
            .get("purl")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let image_url = data
            // short for media url, probably
            .get("murl")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let page_title = data
            .get("t")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            // bing adds these unicode characters around matches
            .replace(['', ''], "");

        // the text looks like "1200 x 1600 · jpegWikipedia" or "1500×1013fity.club"
        let text = image_container_el.text().collect::<String>();
        if text.trim().is_empty() {
            continue;
        }
        let (width, height) = if let Some(captures) = SIZE_REGEX.captures(&text) {
            let w: u64 = captures
                .get(1)
                .unwrap()
                .as_str()
                .parse()
                .unwrap_or_default();
            let h: u64 = captures
                .get(2)
                .unwrap()
                .as_str()
                .parse()
                .unwrap_or_default();
            (w, h)
        } else if text.contains(':') || text.contains('>') {
            // Skip video/duration entries
            continue;
        } else {
            warn!("couldn't get width and height from text \"{text}\"");
            continue;
        };

        image_results.push(EngineImageResult {
            page_url: page_url.to_string(),
            image_url: image_url.to_string(),
            title: page_title.to_string(),
            width,
            height,
        });
    }

    Ok(EngineImagesResponse { image_results })
}

fn clean_url(url: &str) -> eyre::Result<String> {
    // clean up bing's tracking urls
    if url.starts_with("https://www.bing.com/ck/a?") {
        // get the u param
        let url = Url::parse(url)?;
        let u = url
            .query_pairs()
            .find(|(key, _)| key == "u")
            .unwrap_or_default()
            .1;
        // cut off the "a1" and base64 decode
        let u = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&u[2..])
            .unwrap_or_default();
        // convert to utf8
        Ok(String::from_utf8_lossy(&u).to_string())
    } else {
        Ok(url.to_string())
    }
}
