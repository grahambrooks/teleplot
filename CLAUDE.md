# Teleplot

A combination of [Kibana](https://github.com/elastic/kibana) simplicity, [Grafana](https://github.com/grafana/grafana) styling built for performance

## Key Goals

* Compelling visualizations of timeseries and non timeseries data stored in one or more Elasticsearch indexes
* Accepts Kibana Dashboard, panel views (read from disk)
* Single binary for ease of deployment

## MVP

The full plan, with research and citations, is in `docs/mvp-plan.md`. Its scope decisions:

* **Audience:** senior tech leaders doing software delivery analytics (DORA, cycle time, throughput, investment)
* **Targets** ES/Kibana 8.x and 9.x. Exports can be of mixed version, so parse defensively
* **View-only:** dashboards come from files (Kibana NDJSON or native YAML) and reload when the file changes
* **Read-only against ES:** ingestion is out of scope
* **Localhost only, no auth**
* **Stack:**
  * Single crate built with cargo-leptos (`ssr`/`hydrate`); axum on the server; Leptos UI
  * uPlot through wasm-bindgen for charts
  * reqwest + serde against the ES REST API, not the alpha `elasticsearch` crate
  * KQL translated to DSL in Rust
  * Site assets embedded with rust-embed
* **Export:** PNG per panel, and PDF through the browser's print stylesheet (no headless browser)

### Deliberately NOT in MVP — do not "complete" these

Ingestion/ETL · writing to ES (except the dev-only fixture seeder) · UI editing · auth/multi-user · alerting · scheduled reports · TSVB/Timelion/Vega/Maps · Lens ES|QL · Kibana 9.4+ Dashboards API format · OpenSearch · cumulative flow/WIP · saved-search tables · survey metrics · ranking individual engineers (excluded by design)

## Development

* `make dev` — `cargo leptos watch` with live reload on http://127.0.0.1:3000
* `make pre-commit` — fmt, clippy (server and WASM client), tests. Run before every commit; CI runs the same
* `make smoke` — release build, then checks that the binary serves the whole UI from an empty directory
* `make deny` — cargo-deny supply-chain checks
* Hooks: `prek install --hook-type pre-commit --hook-type pre-push`

Build gotchas, learned in M0:

* The server embeds `target/site` with rust-embed. In a single `cargo leptos build` the client and server compile concurrently, so the server can embed a missing bundle. `make release` therefore builds `--frontend-only` and then `--server-only`, and `build.rs` re-embeds whenever `target/site` changes
* Leptos takes the client asset names from `LEPTOS_OUTPUT_NAME` at compile time; without it, it falls back to wasm-pack's `_bg` suffix. `.cargo/config.toml` sets it, so plain `cargo` builds and tests match cargo-leptos
