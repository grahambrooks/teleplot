//! Teleplot: Kibana-compatible, Grafana-styled dashboards over Elasticsearch.
//!
//! One crate builds twice: the server binary (`ssr`) and the browser client
//! (`hydrate`, compiled to WASM). Modules shared by both live outside `server`.

// Leptos view types nest deeply.
#![recursion_limit = "256"]

pub mod kql;
pub mod model;
pub mod ui;

#[cfg(feature = "ssr")]
pub mod compare;
#[cfg(feature = "ssr")]
pub mod config;
#[cfg(feature = "ssr")]
pub mod dashboards;
#[cfg(feature = "ssr")]
pub mod edit;
#[cfg(feature = "ssr")]
pub mod engine;
#[cfg(feature = "ssr")]
pub mod es;
#[cfg(feature = "ssr")]
pub mod kibana;
#[cfg(feature = "ssr")]
pub mod lint;
#[cfg(feature = "ssr")]
pub mod mcp;
#[cfg(feature = "ssr")]
pub mod metrics;
#[cfg(feature = "ssr")]
pub mod query;
#[cfg(feature = "ssr")]
pub mod report;
#[cfg(all(feature = "ssr", not(target_arch = "wasm32")))]
pub mod spans;
#[cfg(feature = "ssr")]
pub mod store;
/// Reports requests and CLI runs to habit as traces (server only).
#[cfg(all(feature = "ssr", not(target_arch = "wasm32")))]
pub mod telemetry;
#[cfg(feature = "ssr")]
pub mod time;
#[cfg(feature = "ssr")]
pub mod yaml;

#[cfg(feature = "ssr")]
pub mod server;

/// WASM entry point: attaches the reactive app to the server-rendered page.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(ui::App);
}
