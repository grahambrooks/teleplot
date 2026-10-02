# Teleplot — MVP Plan

## Context

`/Users/graham/dev/Teleplot` is empty apart from `CLAUDE.md`, which states the vision: Kibana's simplicity plus Grafana's styling, built for performance. It must visualise Elasticsearch data, accept Kibana dashboard and panel exports read from disk, and ship as a single binary. This plan turns that into an MVP based on four research tracks: Kibana saved-object formats, Grafana UX and competing tools, the Rust/ES performance stack, and delivery-analytics practice. It also follows Graham's recorded conventions from the vault: pure Rust, a single crate, design first, CalVer with an in-repo Homebrew formula, trunk-based development, and explicit done-when criteria.

**Decisions from clarifying questions**

| Topic | Decision |
|---|---|
| Audience | Senior tech leaders doing **software delivery analytics** (DORA, cycle time, throughput, investment) |
| Versions | ES/Kibana **8.x and 9.x**; exports are of **mixed/unknown version**, so parse defensively |
| Mode | **View-only**. Dashboards come from files on disk and reload when the file changes. No UI editor |
| Frontend | **Leptos (Rust/WASM)** with a JS chart library (uPlot) called through `wasm-bindgen` |
| Data | Already in ES. **Ingestion is out of scope**; Teleplot only reads |
| Kibana import | **Must render existing exports.** Those exports live on another machine, so we **manufacture realistic fixtures** |
| Access | **Localhost only, no auth** (same model as `attribution`) |
| Sharing | **PNG/PDF export** |

## Research findings that drive the design

1. **No tool fills this gap.** Grafana has no KQL support and no Kibana importer, and there is no Kibana→Grafana converter ([Grafana community](https://community.grafana.com/t/replicating-kibana-dashboards-in-grafana-rather-than-creating-manually/55129)). Perses has no ES plugin ([perses/plugins](https://github.com/perses/plugins)). OpenObserve's Kibana migrator only targets its own storage. Kibana itself recommends 2 GB and users report RAM growth in 9.4 ([discuss](https://discuss.elastic.co/t/kibana-9-4-3-high-ram-consumption/388326)). **Teleplot's differentiators:** it renders Kibana exports, supports KQL, ships as one small binary, and uses a permissive licence.
2. **Modern exports are mostly by-value Lens.** Elastic's guidelines say "add visualizations by value" and "always use Lens" ([dashboard guidelines](https://www.elastic.co/docs/extend/integrations/dashboard-guidelines)). Older exports still contain legacy `visState` panels. Lens datasource state is `indexpattern` before 8.6 and `formBased` after. `panelsJSON`, `visState` and `searchSourceJSON` are JSON stored as strings, so they need a second parse. The layout uses a 48-column `gridData`.
3. **KQL:** ES has a native `kql` query only from **9.1** ([docs](https://www.elastic.co/docs/reference/query-languages/query-dsl/query-dsl-kql-query)). For 8.x we need a translator anyway. Kibana's Peggy grammar (`kbn-es-query/src/kuery/grammar/grammar.peggy`) is the reference to port. **There is no Rust KQL crate**; the crates named "kql" are for Kusto.
4. **ES client:** the official `elasticsearch` crate is still alpha (last release 2025-08). Use **reqwest + serde against the REST API** (`_msearch`, `_field_caps`, `_search`).
5. **Performance techniques:**
   - Batch all panels into one `_msearch`.
   - Send `size:0`, `track_total_hits:false` and `filter_path`.
   - Convert `now` to absolute times rounded to the bucket, so the **shard request cache** gets hits ([docs](https://www.elastic.co/docs/reference/elasticsearch/rest-apis/shard-request-cache)).
   - Keep an in-process `moka` cache in which closed buckets never change.
   - Stream each panel to the browser as soon as its result arrives.
   - Cap bucket counts below `search.max_buckets`, which avoids Grafana's documented "too many buckets" problem.
6. **Charting:** uPlot is about 48 KB and renders around 100k points/ms; Grafana adopted it for time series ([uPlot](https://github.com/leeoniya/uPlot)). Its input is columnar `Float64Array`, which suits `js_sys`. Its weakness is large scatter plots, but delivery scatter plots hold at most a few thousand PRs.
7. **What delivery analytics needs:**
   - **DORA now has five metrics**: rework rate was added. The 2025 report also **dropped the elite/high/medium/low clusters** ([dora.dev](https://dora.dev/guides/dora-metrics/)), so benchmark bands should be a configurable overlay, not presented as DORA's standard.
   - Executives see KPI tiles with change vs the previous period, weekly or monthly trends with rolling averages, **percentiles rather than averages** (LinearB p75, DevLake and GitLab median), cycle-time scatter plots (Swarmia), investment allocation as stacked bars (Jellyfish), and team comparison tables.
   - Pitfalls to design against: timezone and DST in calendar buckets, noise from small samples (always show n), and ranking individuals (keep everything at team level).
8. **Lens features delivery dashboards depend on:** `percentile`/`median`, `formula` for ratios such as change fail rate (`count(kql='…')/count()`), `moving_average`, and metric "time shift" for comparing with the previous period. These map onto ES `percentiles`, `filter` + `bucket_script`, `moving_fn` and `date_range`.

## MVP scope

### In scope
- **CLI:** `teleplot serve --config teleplot.yaml` opens `http://localhost:<port>`. Other commands:
  - `teleplot check <file>` validates an export and lists any unsupported panels.
  - `teleplot convert <ndjson>` writes a native YAML dashboard.
- **Config** (`teleplot.yaml`): ES URL, auth (API key, basic auth, or Cloud ID; secrets from env vars), CA file, dashboards directory, org timezone, and the benchmark band set.
- **Dashboard inputs:**
  - Kibana NDJSON from 8.x/9.x. Supported object types: `dashboard`, `index-pattern`, `lens`, `visualization`, `search` (for references only), and `markdown`.
  - Both by-reference panels (`panelRefName` → `references[]`) and by-value panels (`embeddableConfig.attributes` / `savedVis`).
  - **Native YAML**, which is also the internal model.
  - Unknown panel types render as a labelled placeholder card and never fail the dashboard.
- **Lens support:**
  - Chart types: `lnsXY` (line, area, bar, stacked bar, percentage bar), `lnsMetric` (with trend and comparison), `lnsPie`/donut, `lnsDatatable`.
  - Operations: `count`, `sum`, `average`, `min`, `max`, `median`, `percentile`, `unique_count`, `last_value`, `date_histogram`, `terms`, `filters`, `range`.
  - Formula: arithmetic over those operations plus per-metric `kql=` filters.
  - Also `moving_average`, `differences`, `cumulative_sum`, and time shift.
- **Legacy aggregation-based visualizations:** line, area, histogram, pie, metric and table.
- **Teleplot-native panels** for leaders that Kibana does poorly:
  - percentile band chart (p50/p75/p95)
  - cycle-time scatter with a rolling median
  - KPI tile with sparkline and Δ vs the previous period
  - benchmark band overlay
- **Query bar and filters:**
  - KQL is translated to DSL in Rust and used for both 8.x and 9.x, so behaviour is the same on both.
  - Lucene is passed through as `query_string`.
  - Kibana filter pills are passed through as raw DSL.
  - Dashboard-level query and filters come from `searchSourceJSON`.
- **Time:**
  - A Grafana-style picker with `now-30d` and `now/w` syntax, quick ranges and drag-to-zoom.
  - Calendar week/month buckets in the configured timezone.
  - `timeRestore` defaults are honoured.
  - All state lives in the URL, which also makes views bookmarkable.
- **Variables:**
  - A `terms`-backed team/repo selector that filters every panel.
  - Drill-down from org → team → repo by clicking a table row or series, which sets the variable.
- **Styling:**
  - Grafana dark theme tokens (canvas `#111217`, panel `#181b1f`; series colours `#73BF69`, `#F2495C`, `#5794F2`, …) plus a light theme.
  - Shared crosshair, a legend with min/max/avg/last, units (duration, %, count) and thresholds.
- **Export:**
  - Per-panel PNG from the uPlot canvas.
  - Dashboard PDF from a print stylesheet plus `window.print()`, with a landscape page and a title and time-range header.
  - No headless browser, to keep the "no runtime dependencies" rule.
- **Small-sample guard:** panels show n, and buckets below a configurable n are drawn de-emphasised.

### Explicitly NOT in MVP (record so a later session does not "complete" these)
- Ingestion or ETL of GitHub, Jira or CI data
- Writing to ES. The only exception is the **dev-only** fixture seeder
- UI dashboard editing
- Authentication and multi-user access
- Alerting
- Scheduled reports
- TSVB, Timelion, Vega and Maps panels
- Lens ES|QL (`textBased`) panels
- The 9.4+ typed Dashboards API format
- OpenSearch
- Cumulative flow and WIP charts, which need snapshot documents
- Saved-search document tables
- SPACE/DevEx survey metrics
- Ranking individual engineers. This is excluded **by design**

## Architecture

**A single crate, using cargo-leptos's standard `ssr`/`hydrate` feature split.**
- One `Cargo.toml` honours the single-crate rule.
- Server and WASM client share the dashboard model and result types.
- backstage-rs used a separate Trunk SPA served from disk at runtime, which broke the single-binary goal. Teleplot instead embeds the cargo-leptos site directory with **rust-embed** in release builds; debug builds read from disk.

```
src/
  main.rs            # clap CLI: serve | check | convert   (ssr)
  config/            # teleplot.yaml, env-secret resolution, Cloud ID decode
  model/             # Dashboard, Panel, Query, Viz, Layout, TimeRange — shared ssr+hydrate
  kibana/            # NDJSON reader; double-parse stringified JSON; refs resolver;
                     #   lens.rs, legacy_vis.rs, search_source.rs → model/
  kql/               # chumsky parser (port of grammar.peggy) → AST → ES DSL
  query/             # model::Query → ES request; interval sizing; time rounding;
                     #   formula → filter+bucket_script; percentiles; moving_fn; time shift
  es/                # reqwest client (rustls + OS native roots, as backstage-rs), _msearch,
                     #   _field_caps, response decoding with filter_path
  cache/             # moka LRU keyed on (normalized request, rounded range)
  server/            # axum 0.8 routes: /api/dashboards, /api/panels (SSE stream), assets
  ui/                # Leptos components: shell, time picker, query bar, grid, panels/*
  ui/chart/          # wasm-bindgen bindings to vendored uPlot (assets/vendor/uplot.min.js)
assets/vendor/       # uPlot (MIT) — vendored, embedded
fixtures/            # see Test data
```

**Request flow**
1. The browser loads a dashboard from `/api/dashboards/:id`; the server returns the model.
2. The client posts its time range, query and variables to `/api/dashboards/:id/data`.
3. The server builds one `_msearch` covering every panel.
4. Each panel's result is streamed back over SSE as soon as it is decoded.
5. The client draws each panel as its result arrives.

**Interval sizing**
- Fixed intervals are `(to−from)/min(px_width, max_points)`, snapped to a "nice" interval and capped by `max_buckets`.
- When a Lens column says "auto", calendar week/month is used for ranges of 30 days or more.

## Test data and fixtures (manufactured)

The real exports live on another machine, so we manufacture both the data and the exports in a way that resembles real ones.

1. **Synthetic delivery dataset.** A dev-only example, `cargo run --example seed`, writes to ES:
   - indexes `delivery-prs`, `delivery-deployments`, `delivery-incidents` and `delivery-workitems`;
   - fields modelled on the DevLake domain layer (`team`, `repo`, `created_at`, `merged_at`, `deployed_at`, cycle-phase durations, `ai_assisted`, `work_type`, `result`, `is_rework`);
   - long-tailed (log-normal) cycle times, team-size variance and a seeded RNG, so runs are reproducible.
2. **Real-Kibana exports.** `fixtures/docker-compose.yml` runs ES and Kibana for **8.19** and **9.x**.
   - A scripted step creates data views and representative Lens and legacy dashboards over the seeded data through Kibana's API, then exports them with the Saved Objects export API into `fixtures/kibana/<version>/*.ndjson`.
   - The exports are committed, so these fixtures are real Kibana output, not hand-guessed.
3. **Hand-built edge cases:** pre-8.6 `indexpattern` layers, by-reference panels, missing `fields`, unknown panel types, and broken references.
4. **Golden tests** (`insta`):
   - NDJSON → model
   - model → ES request JSON
   - KQL → DSL, including cases ported from Kibana's own kuery test cases
   - ES response → panel series

## Milestones (each ends with a verifiable done-when)

| # | Milestone | Done when |
|---|---|---|
| M0 | Scaffold: `git init` on `main`, cargo-leptos crate, `make pre-commit` (fmt, clippy `-D warnings`, test), CI matching it, `deny.toml`, `unsafe_code = "deny"` | CI is green on a push to `main`; `teleplot serve` shows an empty shell from the **release** binary with no external files |
| M1 | Fixtures: seeder, docker-compose with ES and Kibana 8.19 and 9.x, scripted dashboard creation and export, NDJSON committed | `fixtures/kibana/{8.19,9.x}/` holds at least 3 dashboards covering every in-scope panel type |
| M2 | Kibana reader, model, `teleplot check` / `convert` | `teleplot check` on every fixture reports 100% of in-scope panels parsed, with unsupported ones listed; golden tests pass |
| M3 | KQL parser and query engine; ES client; `_msearch`; cache | Golden request tests pass; an integration test against dockerised ES 8.19 and 9.x returns correct series for each fixture panel, matching Kibana's own numbers within percentile approximation |
| M4 | UI: grid layout, time picker, query bar, theme; XY, metric, pie and table panels drawn with uPlot or DOM | Every fixture dashboard renders in a browser with no placeholder for in-scope panels; screenshots are checked side by side against Kibana |
| M5 | Leader features: KPI Δ tile, percentile band, scatter, benchmark overlay, team variable and drill-down, small-n guard; native YAML "Delivery Overview" dashboard | The Delivery Overview dashboard works end to end on the seeded data |
| M6 | Export (PNG/PDF), performance pass, release | PNG and PDF export work; a 12-panel dashboard over 1M documents loads its first panel in under 500 ms and finishes in under 2 s on a warm cache, measured; the CalVer release is cut through GitHub Actions; `brew install` of the in-repo formula is **verified on this machine** |

## Verification
- `make pre-commit` before every commit, matching CI.
- Integration tests run against ES 8.19 and 9.x as GitHub Actions service containers.
- For each fixture dashboard, compare each panel's numbers with the same panel in Kibana (via the inspect/request data) for the same time range, and record any differences.
- Drive the running app with Claude in Chrome: load each fixture dashboard, change the time range and KQL, drill down, export a PDF, and capture screenshots and a GIF.
- Benchmark the performance numbers with a seeded 1M-document dataset, and record the results in the repo.
- Install the release through Homebrew and run it, per the vault rule to verify the published artifact rather than the local build.

## Risks and open items
- **Fidelity of Lens formula and time shift.** Formulas are only a subset (arithmetic plus a `kql` filter). Anything else renders a placeholder that shows the formula text.
- **Unknown versions in the real exports.** Mitigation: `teleplot check` produces a support report when run on the other machine, and that report decides what to prioritise after the MVP.
- **Interaction between cargo-leptos and rust-embed** in a single binary. Mitigation: spike this in M0 before anything else.
- **Drift in the Kibana KQL grammar.** Mitigation: port Kibana's test cases as goldens.

## Housekeeping
- Update the project `CLAUDE.md`: fix the markdown link syntax, which is currently `(Kibana)[url]`, and add the scope decisions and the "not in MVP" list.
- After approval, record a vault entity page for `teleplot` with these scope decisions, through the `claude-obsidian` transaction (`save` skill).
