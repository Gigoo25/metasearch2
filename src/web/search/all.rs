//! Rendering results in the "all" tab.

use maud::{html, Markup, PreEscaped};
use url::Url;

use crate::{
    config::Config,
    engines::{self, EngineSearchResult, Infobox, Response},
    web::search::render_engine_list,
};

pub fn render_results(response: Response) -> PreEscaped<String> {
    let mut html = String::new();
    if let Some(answer) = &response.answer {
        html.push_str(
            &html! {
                div.answer {
                    (answer.html)
                    (render_engine_list(&[answer.engine], &response.config))
                }
            }
            .into_string(),
        );
    }
    if let Some(infobox) = &response.infobox {
        html.push_str(
            &html! {
                div.infobox {
                    (infobox.html)
                    (render_engine_list(&[infobox.engine], &response.config))
                }
            }
            .into_string(),
        );
    }
    if let Some(featured_snippet) = &response.featured_snippet {
        html.push_str(&render_featured_snippet(featured_snippet, &response.config).into_string());
    }
    for result in &response.search_results {
        html.push_str(&render_search_result(result, &response.config).into_string());
    }

    if html.is_empty() {
        html.push_str(
            &html! {
                p { "No results." }
            }
            .into_string(),
        );
    }

    PreEscaped(html)
}

fn render_search_result(
    result: &engines::SearchResult<EngineSearchResult>,
    config: &Config,
) -> PreEscaped<String> {
    html! {
        div.search-result {
            a.search-result-anchor rel="noreferrer" href=(result.result.url) {
                span.search-result-url { (result.result.url) }
                h3.search-result-title { (result.result.title) }
            }
            p.search-result-description { (result.result.description) }
            div.search-result-footer {
                (render_engine_list(&result.engines.iter().copied().collect::<Vec<_>>(), config))
                (site_rule_controls(&result.result.url, config))
            }
        }
    }
}

/// HTML-only ranking menu for the result's domain (or ZIM book for kiwix
/// content). Each action is a small form that posts to /settings/site-rule,
/// which redirects back to this page. When a rule already exists the menu
/// shows it and offers an undo. Kiwix books can only be sorted, not hidden.
fn site_rule_controls(url: &str, config: &Config) -> Markup {
    let Some((key, is_kiwix)) = site_key(url) else {
        return html! {};
    };

    let current = config.site_rule_weight(&key);
    let summary = match current {
        Some(weight) if weight <= 0. && !is_kiwix => "ranking: hidden".to_string(),
        Some(weight) if weight > 1. => format!("ranking: raised ×{weight}"),
        Some(weight) if weight < 1. => format!("ranking: lowered ×{weight}"),
        Some(_) => "ranking: default".to_string(),
        None => "ranking".to_string(),
    };

    html! {
        details.site-rule-menu {
            summary { (summary) }
            div.site-rule-menu-options {
                (site_rule_form(&key, "2", "raise"))
                (site_rule_form(&key, "0.5", "lower"))
                @if !is_kiwix {
                    (site_rule_form(&key, "0", "hide"))
                }
                @if current.is_some() {
                    form method="post" action="/settings/site-rule" {
                        input type="hidden" name="host" value=(key);
                        input type="hidden" name="clear" value="true";
                        input type="submit" value="undo";
                    }
                }
            }
        }
    }
}

/// The rule key for a result url, plus whether it is local kiwix content.
fn site_key(url: &str) -> Option<(String, bool)> {
    if let Ok(url) = Url::parse(url) {
        return url.host_str().map(|host| (host.to_string(), false));
    }

    let path = url.strip_prefix('/').unwrap_or(url);
    let path = path.strip_prefix("kiwix/").unwrap_or(path);
    let book = path.strip_prefix("content/")?.split('/').next()?;
    (!book.is_empty()).then(|| (format!("kiwix:{book}"), true))
}

fn site_rule_form(host: &str, weight: &str, label: &str) -> Markup {
    html! {
        form method="post" action="/settings/site-rule" {
            input type="hidden" name="host" value=(host);
            input type="hidden" name="weight" value=(weight);
            input type="submit" value=(label);
        }
    }
}

fn render_featured_snippet(
    featured_snippet: &engines::FeaturedSnippet,
    config: &Config,
) -> PreEscaped<String> {
    html! {
        div.featured-snippet {
            p.search-result-description { (featured_snippet.description) }
            a.search-result-anchor rel="noreferrer" href=(featured_snippet.url) {
                span.search-result-url { (featured_snippet.url) }
                h3.search-result-title { (featured_snippet.title) }
            }
            (render_engine_list(&[featured_snippet.engine], config))
        }
    }
}

pub fn render_infobox(infobox: &Infobox, config: &Config) -> PreEscaped<String> {
    html! {
        div.infobox.postsearch-infobox {
            (infobox.html)
            (render_engine_list(&[infobox.engine], config))
        }
    }
}
