use std::collections::HashMap;

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension,
};
use tracing::error;
use wreq::header;

use crate::config::Config;

pub async fn route(
    Query(params): Query<HashMap<String, String>>,
    Extension(config): Extension<Config>,
) -> Response {
    let image_search_config = &config.image_search;
    let proxy_config = &image_search_config.proxy;
    if !image_search_config.enabled || !proxy_config.enabled {
        return (StatusCode::FORBIDDEN, "Image proxy is disabled").into_response();
    };
    let url = params.get("url").cloned().unwrap_or_default();
    if url.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing `url` parameter").into_response();
    }

    // ssrf protection. i sure hope this is good enough!
    let Ok(v) = url_jail::validate(&url, url_jail::Policy::PublicOnly).await else {
        return (StatusCode::BAD_REQUEST, "Invalid URL").into_response();
    };
    drop(url);
    let Ok(client) = wreq::Client::builder()
        .resolve(&v.host, v.to_socket_addr())
        .build()
    else {
        return (StatusCode::BAD_REQUEST, "Failed to build client").into_response();
    };

    let mut res = match client.get(&v.url).header("accept", "image/*").send().await {
        Ok(res) => res,
        Err(err) => {
            error!("Image proxy error for {}: {err}", v.url);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Image proxy error").into_response();
        }
    };

    let max_size = proxy_config.max_download_size;

    if res.content_length().unwrap_or_default() > max_size {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Image too large").into_response();
    }

    const ALLOWED_IMAGE_TYPES: &[&str] = &["apng", "avif", "gif", "jpeg", "png", "webp"];

    // validate content-type
    let content_type = res
        .headers()
        .get(wreq::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    let Some((base_type, subtype)) = content_type.split_once("/") else {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Invalid Content-Type").into_response();
    };
    if base_type != "image" {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Not an image").into_response();
    }
    if !ALLOWED_IMAGE_TYPES.contains(&subtype) {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Image type not allowed").into_response();
    }

    let mut image_bytes = Vec::new();
    while let Ok(Some(chunk)) = res.chunk().await {
        image_bytes.extend_from_slice(&chunk);
        if image_bytes.len() as u64 > max_size {
            return (StatusCode::PAYLOAD_TOO_LARGE, "Image too large").into_response();
        }
    }

    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=31536000".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CONTENT_DISPOSITION, "attachment".to_owned()),
        ],
        image_bytes,
    )
        .into_response()
}
