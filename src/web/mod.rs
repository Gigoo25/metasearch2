mod autocomplete;
mod image_proxy;
mod index;
mod kiwix_proxy;
mod opensearch;
mod search;
mod settings;

use std::sync::OnceLock;
use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Redirect, Response},
    routing::{get, post, MethodRouter},
    Router,
};
use maud::{html, Markup, PreEscaped};
use tracing::{error, info};

use crate::config::Config;

const BASE_COMMIT_URL: &str = "https://github.com/mat-1/metasearch2/commit/";
pub const VERSION: &str = std::env!("CARGO_PKG_VERSION");
const COMMIT_HASH: &str = std::env!("GIT_HASH");
const COMMIT_HASH_SHORT: &str = std::env!("GIT_HASH_SHORT");

/// Cache-busting value for static assets. It is the hash of the embedded asset
/// contents (set up in `register_static_routes!`), so it changes whenever any
/// asset changes, but not on unrelated rebuilds. Before the routes are set up
/// (which never serves html) it falls back to the commit hash/version.
static ASSET_VERSION: OnceLock<String> = OnceLock::new();

#[must_use]
pub fn asset_version() -> &'static str {
    ASSET_VERSION.get().map_or_else(
        || {
            if commit_available() {
                COMMIT_HASH_SHORT
            } else {
                VERSION
            }
        },
        String::as_str,
    )
}

/// Whether the build knows the commit it was built from. Container builds
/// don't have `.git`, so the hash is `unknown` (or empty from an older
/// build script).
fn commit_available() -> bool {
    !COMMIT_HASH.is_empty()
        && COMMIT_HASH != "unknown"
        && !COMMIT_HASH_SHORT.is_empty()
        && COMMIT_HASH_SHORT != "unknown"
}

/// Adds the cache-busting version to same-origin asset paths; external and
/// `data:` urls are returned unchanged.
#[must_use]
pub(crate) fn asset_url(url: &str) -> String {
    if !url.starts_with('/') || url.starts_with("//") {
        return url.to_string();
    }

    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}v={}", asset_version())
}

/// A small version marker shown in the bottom right corner of every page, so
/// it's visible without being intrusive.
#[must_use]
pub fn version_info() -> Markup {
    let has_commit = commit_available();

    html! {
        span.version-info {
            @if !has_commit {
                "Version "
                (VERSION)
            } @else {
                "Version "
                (VERSION)
                " ("
                a href=(format!("{BASE_COMMIT_URL}{COMMIT_HASH}")) { (COMMIT_HASH_SHORT) }
                ")"
            }
        }
    }
}

macro_rules! register_static_routes {
    ( $app:ident, $( $x:expr ),* ) => {{
        {
            use std::hash::{Hash, Hasher};

            // the asset version is a hash of every embedded asset, so a change
            // to any of them busts the browser cache
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            $(
                $x.hash(&mut hasher);
                include_str!(concat!("assets/", $x)).hash(&mut hasher);
            )*
            let _ = ASSET_VERSION.set(format!("{:016x}", hasher.finish()));
        }

        $(
            let $app = $app.route(
                concat!("/", $x),
                static_route(
                    include_str!(concat!("assets/", $x)),
                    guess_mime_type($x)
                ),
            );
        )*

        $app
    }};
}

pub async fn run(config: Config) {
    let bind_addr = config.bind;

    if let Err(err) = crate::db::init(
        &config.database.resolved_path(),
        config.database.max_results,
        config.database.max_age_days,
        &config.database.synchronous,
        config.database.quick_check,
    ) {
        error!("Failed to open the index database: {err}");
    }

    let config = Arc::new(config);

    // bring kiwix up in the background; searches wait on this without blocking
    // the web ui from starting
    tokio::spawn({
        let config = config.clone();
        async move { crate::engines::search::kiwix::ensure_ready(&config).await }
    });

    fn static_route<S>(
        content: &'static str,
        content_type: &'static str,
    ) -> MethodRouter<S, Infallible>
    where
        S: Clone + Send + Sync + 'static,
    {
        let response = (
            [
                (header::CONTENT_TYPE, content_type),
                // assets are baked into the binary and requested with a version
                // query, so the browser can cache them forever
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            content,
        );
        get(|| async { response })
    }

    let app = Router::new()
        .route("/", get(index::get))
        .route("/search", get(search::get))
        .route("/settings", get(settings::get))
        .route("/settings", post(settings::post))
        .route("/settings/site-rule", post(settings::site_rule))
        .route("/opensearch.xml", get(opensearch::route))
        .route("/autocomplete", get(autocomplete::route))
        .route("/image-proxy", get(image_proxy::route))
        .route(
            "/favicon.ico",
            get(|| async { Redirect::permanent("/favicon.svg") }),
        )
        .route("/kiwix", get(kiwix_proxy::route))
        .route("/kiwix/{*path}", get(kiwix_proxy::route))
        .layer(middleware::from_fn_with_state(
            config.clone(),
            config_middleware,
        ))
        .with_state(config);
    let app = register_static_routes![
        app,
        "style.css",
        "script.js",
        "robots.txt",
        "favicon.svg",
        "scripts/colorpicker.js",
        "themes/catppuccin-mocha.css",
        "themes/catppuccin-macchiato.css",
        "themes/catppuccin-latte.css",
        "themes/everforest.css",
        "themes/everforest-light.css",
        "themes/nord-bluish.css",
        "themes/discord.css"
    ];

    info!("Listening on http://{bind_addr}");

    let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

fn guess_mime_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
}

async fn config_middleware(
    State(config): State<Arc<Config>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let mut config = config.clone().as_ref().clone();

    if let Some(settings_json) = crate::db::setting("ui") {
        if let Ok(settings) = serde_json::from_str::<settings::Settings>(&settings_json) {
            config.ui.stylesheet_url = settings.stylesheet_url;
            config.ui.stylesheet_str = settings.stylesheet_str;
            config.safe_search = settings.safe_search;
        }
    }

    config.site_rules = crate::db::site_rules();

    // modify the state
    req.extensions_mut().insert(config);

    Ok(next.run(req).await)
}

pub fn head_html(title: Option<&str>, config: &Config) -> Markup {
    html! {
        head {
            meta charset="UTF-8";
            meta name="viewport" content="width=device-width, initial-scale=1.0";
            title {
                @if let Some(title) = title {
                    { (title) }
                    { " - " }
                }
                {(config.ui.site_name)}
            }
            link rel="stylesheet" href=(asset_url("/style.css"));
            @if !config.ui.stylesheet_url.is_empty() {
                link rel="stylesheet" href=(asset_url(&config.ui.stylesheet_url));
            }
            @if !config.ui.stylesheet_str.is_empty() {
                style { (PreEscaped(html_escape::encode_style(&config.ui.stylesheet_str))) }
            }
            @if !config.ui.favicon_url.is_empty() {
                link rel="icon" href=(asset_url(&config.ui.favicon_url));
            }
            script src=(asset_url("/script.js")) defer {}
            link rel="search" type="application/opensearchdescription+xml" title="metasearch" href="/opensearch.xml";
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_urls_are_versioned() {
        assert!(asset_url("/style.css").starts_with("/style.css?v="));
        assert!(
            asset_url("/themes/everforest.css?v=1").starts_with("/themes/everforest.css?v=1&v=")
        );

        // external and data urls are left alone
        assert_eq!(
            asset_url("https://example.com/style.css"),
            "https://example.com/style.css"
        );
        assert_eq!(
            asset_url("//cdn.example.com/style.css"),
            "//cdn.example.com/style.css"
        );
        assert_eq!(asset_url("data:text/css,"), "data:text/css,");
    }

    #[test]
    fn asset_version_is_never_empty() {
        assert!(!asset_version().is_empty());
    }

    #[test]
    fn version_info_always_renders() {
        let html = version_info().into_string();
        assert!(html.contains("version-info"));
        assert!(html.contains(VERSION));
        assert!(html.contains("Version"));
    }
}
