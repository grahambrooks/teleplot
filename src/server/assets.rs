//! The cargo-leptos site directory, compiled into the binary.
//!
//! Release builds embed `target/site`; debug builds read it from disk, so
//! `cargo leptos watch` picks up rebuilt assets without recompiling the server.

use axum::{
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "target/site/"]
#[allow_missing = true]
struct Site;

/// Serves an embedded file. Release builds also embed brotli and gzip
/// copies (`cargo leptos build --precompress`); the best one the browser
/// accepts is sent, which cuts the WASM bundle to about a third.
pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let accepts = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    for (encoding, suffix) in [("br", "br"), ("gzip", "gz")] {
        if accepts
            .split(',')
            .any(|e| e.trim().split(';').next() == Some(encoding))
            && let Some(file) = Site::get(&format!("{path}.{suffix}"))
        {
            return (
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (header::CONTENT_ENCODING, encoding),
                    (header::VARY, "accept-encoding"),
                ],
                file.data,
            )
                .into_response();
        }
    }
    match Site::get(path) {
        Some(file) => ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response(),
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

/// True when the WASM client was built before this binary embedded the site.
pub fn client_bundle_present(output_name: &str) -> bool {
    ["js", "wasm"]
        .iter()
        .all(|ext| Site::get(&format!("pkg/{output_name}.{ext}")).is_some())
}
