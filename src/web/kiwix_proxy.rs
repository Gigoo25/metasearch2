//! Reverse proxy for the kiwix-serve instance that ships in the container
//! image.
//!
//! kiwix-serve listens on loopback and is started with
//! `--urlRootLocation=/kiwix`, so every link it generates is already prefixed
//! with `/kiwix`. Proxying those requests through metasearch keeps the whole
//! deployment on a single port and makes ZIM links same-origin.

use async_stream::stream;
use axum::{
    body::Body,
    extract::{Path, RawQuery},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Extension,
};
use tracing::error;

use crate::{
    config::Config,
    engines::{search::kiwix, Engine},
};

pub async fn route(
    path: Option<Path<String>>,
    RawQuery(query): RawQuery,
    Extension(config): Extension<Config>,
) -> Response {
    if !config.engines.get(Engine::Kiwix).enabled {
        return (StatusCode::NOT_FOUND, "kiwix is not enabled").into_response();
    }
    let Some(base) = kiwix::internal_url(&config) else {
        return (StatusCode::NOT_FOUND, "kiwix is not configured").into_response();
    };

    let mut target = base;
    if let Some(Path(path)) = path {
        if !path.is_empty() {
            target.push('/');
            target.push_str(&path);
        }
    }
    if let Some(query) = query {
        target.push('?');
        target.push_str(&query);
    }

    let mut res = match kiwix::KIWIX_CLIENT.get(&target).send().await {
        Ok(res) => res,
        Err(err) => {
            error!("kiwix proxy error for {target}: {err}");
            return (StatusCode::BAD_GATEWAY, "kiwix proxy error").into_response();
        }
    };

    let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    let mut headers = HeaderMap::new();
    for name in [
        header::CONTENT_TYPE,
        header::ETAG,
        header::LAST_MODIFIED,
        header::CACHE_CONTROL,
        header::CONTENT_DISPOSITION,
    ] {
        if let Some(value) = res.headers().get(&name) {
            if let Ok(value) = HeaderValue::from_bytes(value.as_bytes()) {
                headers.insert(name, value);
            }
        }
    }

    let body = Body::from_stream(stream! {
        loop {
            match res.chunk().await {
                Ok(Some(chunk)) => yield Ok::<_, std::io::Error>(chunk),
                Ok(None) => break,
                Err(err) => {
                    error!("kiwix proxy stream error: {err}");
                    break;
                }
            }
        }
    });

    (status, headers, body).into_response()
}
