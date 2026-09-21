//! Google search and image search are removed: google only serves results to
//! javascript-capable clients now. The autocomplete endpoint still works and
//! is the only real suggestion source, so it lives on here.

use url::Url;

use crate::engines::CLIENT;

pub fn request_autocomplete(query: &str) -> wreq::RequestBuilder {
    CLIENT.get(
        Url::parse_with_params(
            "https://suggestqueries.google.com/complete/search",
            &[
                ("output", "firefox"),
                ("client", "firefox"),
                ("hl", "US-en"),
                ("q", query),
            ],
        )
        .unwrap(),
    )
}

pub fn parse_autocomplete_response(body: &str) -> eyre::Result<Vec<String>> {
    let res = serde_json::from_str::<Vec<serde_json::Value>>(body)?;
    Ok(res
        .into_iter()
        .nth(1)
        .unwrap_or_default()
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect())
}
