//! HTTP server: the JSON API, Leptos SSR routes, and the embedded site assets.

mod api;
mod assets;
mod catalog;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::FromRef;
use axum::routing::{get, post};
use leptos::prelude::*;
use leptos_axum::{LeptosRoutes, generate_route_list};

pub use catalog::Catalog;

use crate::config::Config;
use crate::engine::Engine;
use crate::ui::{App, shell};

/// Name cargo-leptos gives the client bundle (`pkg/<name>.js`, `.wasm`, `.css`).
const OUTPUT_NAME: &str = "teleplot";

/// What the server works with.
pub struct Teleplot {
    pub catalog: Arc<Catalog>,
    /// `Err` explains why queries cannot run (e.g. no Elasticsearch configured);
    /// dashboards still list and render their layout.
    pub engine: Result<Engine, String>,
}

impl Teleplot {
    pub fn new(config: Result<Config, String>, dashboards: Option<PathBuf>) -> Self {
        let dir = dashboards
            .or_else(|| config.as_ref().ok().map(|c| c.dashboards.clone()))
            .unwrap_or_else(|| PathBuf::from("dashboards"));
        let engine = config.and_then(|c| Engine::new(&c));
        Teleplot {
            catalog: Catalog::load(dir),
            engine,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub leptos: LeptosOptions,
    pub app: Arc<Teleplot>,
}

impl FromRef<AppState> for LeptosOptions {
    fn from_ref(state: &AppState) -> Self {
        state.leptos.clone()
    }
}

/// Builds the application router. Assets are served from the binary, so the
/// result needs no files on disk.
pub fn router(addr: SocketAddr, app: Arc<Teleplot>) -> Router {
    let leptos = LeptosOptions::builder()
        .output_name(OUTPUT_NAME)
        .site_root("target/site")
        .site_pkg_dir("pkg")
        .site_addr(addr)
        .build();
    let routes = generate_route_list(App);
    let state = AppState {
        leptos: leptos.clone(),
        app,
    };

    Router::new()
        .route("/api/dashboards", get(api::list))
        .route("/api/dashboards/{id}", get(api::dashboard))
        .route("/api/dashboards/{id}/data", post(api::data))
        .route(
            "/api/dashboards/{id}/controls/{control}",
            post(api::control_values),
        )
        .leptos_routes(&state, routes, {
            let leptos = leptos.clone();
            move || shell(leptos.clone())
        })
        .fallback(assets::serve)
        .with_state(state)
}

/// Serves until Ctrl-C.
pub async fn serve(addr: SocketAddr, app: Teleplot) -> std::io::Result<()> {
    if !assets::client_bundle_present(OUTPUT_NAME) {
        tracing::warn!(
            "client bundle not found; the UI will not be interactive. \
             Build it with `cargo leptos build` (or use `make dev`)"
        );
    }
    if let Err(problem) = &app.engine {
        tracing::warn!("queries will not run: {problem}");
    }
    let app = Arc::new(app);
    app.catalog.clone().watch();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Teleplot listening on http://{}", listener.local_addr()?);
    axum::serve(listener, router(addr, app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
