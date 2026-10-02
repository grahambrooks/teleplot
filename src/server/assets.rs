//! The cargo-leptos site directory, compiled into the binary.
//!
//! Release builds embed `target/site`; debug builds read it from disk, so
//! `cargo leptos watch` picks up rebuilt assets without recompiling the server.

use axum::{
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "target/site/"]
#[allow_missing = true]
struct Site;

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    match Site::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

/// True when the WASM client was built before this binary embedded the site.
pub fn client_bundle_present(output_name: &str) -> bool {
    ["js", "wasm"]
        .iter()
        .all(|ext| Site::get(&format!("pkg/{output_name}.{ext}")).is_some())
}
