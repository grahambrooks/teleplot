//! HTTP server: Leptos SSR routes plus the embedded site assets.

mod assets;

use std::net::SocketAddr;

use axum::Router;
use leptos::prelude::*;
use leptos_axum::{LeptosRoutes, generate_route_list};

use crate::ui::{App, shell};

/// Name cargo-leptos gives the client bundle (`pkg/<name>.js`, `.wasm`, `.css`).
const OUTPUT_NAME: &str = "teleplot";

/// Builds the application router. Assets are served from the binary, so the
/// result needs no files on disk.
pub fn router(addr: SocketAddr) -> Router {
    let options = LeptosOptions::builder()
        .output_name(OUTPUT_NAME)
        .site_root("target/site")
        .site_pkg_dir("pkg")
        .site_addr(addr)
        .build();
    let routes = generate_route_list(App);

    Router::new()
        .leptos_routes(&options, routes, {
            let options = options.clone();
            move || shell(options.clone())
        })
        .fallback(assets::serve)
        .with_state(options)
}

/// Serves until Ctrl-C.
pub async fn serve(addr: SocketAddr) -> std::io::Result<()> {
    if !assets::client_bundle_present(OUTPUT_NAME) {
        tracing::warn!("client bundle not embedded; the UI will not be interactive");
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Teleplot listening on http://{}", listener.local_addr()?);
    axum::serve(listener, router(addr))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
