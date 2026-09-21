use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Extension, Form,
};
use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde::{Deserialize, Serialize};

use crate::{config::Config, safe_search::SafeSearch, web::head_html};

pub async fn get(Extension(config): Extension<Config>) -> impl IntoResponse {
    let theme_option = |value: &str, name: &str| -> Markup {
        let selected = config.ui.stylesheet_url == value;
        html! {
            option value=(value) selected[selected] {
                { (name) }
            }
        }
    };

    let html = html! {
        (PreEscaped("<!-- source code: https://github.com/mat-1/metasearch2 -->\n"))
        (DOCTYPE)
        html lang="en" {
            {(head_html(Some("settings"), &config))}
            body {
                div.main-container.settings-page {
                    main {
                        a.back-to-index-button href="/" { "Back" }
                        h1 { "Settings" }

                        h2 { "Appearance" }
                        form #settings-form.settings-form method="post" {
                            label for="theme" { "Theme" }
                            select name="stylesheet-url" selected=(config.ui.stylesheet_url) {
                                { (theme_option("", "Ayu Dark")) }
                                { (theme_option("/themes/catppuccin-mocha.css", "Catppuccin Mocha")) }
                                { (theme_option("/themes/catppuccin-macchiato.css", "Catppuccin Macchiato")) }
                                { (theme_option("/themes/catppuccin-latte.css", "Catppuccin Latte")) }
                                { (theme_option("/themes/everforest.css", "Everforest Dark")) }
                                { (theme_option("/themes/everforest-light.css", "Everforest Light")) }
                                { (theme_option("/themes/nord-bluish.css", "Nord Bluish")) }
                                { (theme_option("/themes/discord.css", "Discord")) }
                            }

                            br;

                            // custom css textarea
                            details #custom-css-details {
                                summary { "Custom CSS" }
                                textarea #custom-css name="stylesheet-str"  {
                                    { (config.ui.stylesheet_str) }
                                }
                            }

                            h2 { "Search" }
                            label for="safe-search" { "Safe search" }
                            select #safe-search name="safe-search" {
                                option value="off" selected[config.safe_search == SafeSearch::Off] { "Off" }
                                option value="moderate" selected[config.safe_search == SafeSearch::Moderate] { "Moderate" }
                                option value="strict" selected[config.safe_search == SafeSearch::Strict] { "Strict" }
                            }
                            p { "Applies to engine requests and to results from every engine, including the local index." }
                        }

                        div.ranking {
                            h2 { "Ranking" }
                            @if config.site_rules.is_empty() {
                                p { "Use the ranking menu under a search result, or add a domain below. Kiwix books use kiwix:<book>." }
                            } @else {
                                table.site-rules {
                                    @for rule in &config.site_rules {
                                        tr {
                                            td.site-rule-host { (rule.host) }
                                            td.site-rule-weight { (rule_action(rule.weight)) }
                                            td {
                                                form method="post" action="/settings/site-rule" {
                                                    input type="hidden" name="host" value=(rule.host);
                                                    input type="hidden" name="clear" value="true";
                                                    input type="submit" value="undo";
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            form.site-rule-add method="post" action="/settings/site-rule" {
                                input type="text" name="host" placeholder="example.com or kiwix:<book>" required;
                                select name="weight" {
                                    option value="2" { "raise" }
                                    option value="0.5" { "lower" }
                                    option value="0" { "hide" }
                                }
                                input type="submit" value="Add";
                            }
                            @if !config.site_rules.is_empty() {
                                form method="post" action="/settings/site-rule" {
                                    input type="hidden" name="clear_all" value="true";
                                    input type="submit" value="Clear all";
                                }
                            }
                        }

                        input #save-settings-button type="submit" form="settings-form" value="Save";
                    }
                }
            }
        }
    }
    .into_string();

    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Settings {
    #[serde(default)]
    pub stylesheet_url: String,
    #[serde(default)]
    pub stylesheet_str: String,
    #[serde(default)]
    pub safe_search: SafeSearch,
}

#[derive(Deserialize)]
pub struct SiteRuleForm {
    #[serde(default)]
    pub host: String,
    pub weight: Option<f64>,
    #[serde(default)]
    pub clear: bool,
    #[serde(default)]
    pub clear_all: bool,
}

fn rule_action(weight: f64) -> String {
    if weight <= 0. {
        "hide".to_string()
    } else if weight > 1. {
        format!("raise ×{weight}")
    } else if weight < 1. {
        format!("lower ×{weight}")
    } else {
        "default".to_string()
    }
}

fn origin_matches_host(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let Some(host) = headers.get("host").and_then(|h| h.to_str().ok()) else {
        return false;
    };
    origin == format!("http://{host}") || origin == format!("https://{host}")
}

/// The path of the page a form was submitted from, if it was this instance.
fn return_path(headers: &HeaderMap) -> Option<String> {
    let referer = headers.get(header::REFERER)?.to_str().ok()?;
    let url = url::Url::parse(referer).ok()?;
    let host = headers.get(header::HOST)?.to_str().ok()?;

    let referer_host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str()?),
        None => url.host_str()?.to_string(),
    };
    if referer_host != host {
        return None;
    }

    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    Some(path)
}

pub async fn site_rule(headers: HeaderMap, Form(form): Form<SiteRuleForm>) -> Response {
    if !origin_matches_host(&headers) {
        return (StatusCode::BAD_REQUEST, "Origin does not match Host").into_response();
    }

    let result = if form.clear_all {
        crate::db::clear_site_rules()
    } else if form.clear {
        crate::db::remove_site_rule(&form.host)
    } else {
        crate::db::set_site_rule(&form.host, form.weight.unwrap_or(1.0))
    };

    match result {
        Ok(()) => {
            let target = return_path(&headers).unwrap_or_else(|| "/settings".to_string());
            Redirect::to(&target).into_response()
        }
        Err(err) => (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    }
}

pub async fn post(headers: HeaderMap, Form(settings): Form<Settings>) -> Response {
    if !origin_matches_host(&headers) {
        return (StatusCode::BAD_REQUEST, "Origin does not match Host").into_response();
    }

    let json = match serde_json::to_string(&settings) {
        Ok(json) => json,
        Err(err) => return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    };

    match crate::db::set_setting("ui", &json) {
        Ok(()) => Redirect::to("/settings").into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}
