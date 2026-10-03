//! Teleplot: Kibana-compatible, Grafana-styled dashboards over Elasticsearch.
//!
//! One crate builds twice: the server binary (`ssr`) and the browser client
//! (`hydrate`, compiled to WASM). Modules shared by both live outside `server`.

pub mod model;
pub mod ui;

#[cfg(feature = "ssr")]
pub mod kibana;
#[cfg(feature = "ssr")]
pub mod report;

#[cfg(feature = "ssr")]
pub mod server;

/// WASM entry point: attaches the reactive app to the server-rendered page.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(ui::App);
}
