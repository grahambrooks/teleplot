# Roadmap: the remaining work, in order

Status: current as of 2026-10-03. This is the one place that orders the work.
The research and rationale stay in the source plans:

- [`mvp-plan.md`](mvp-plan.md): M0–M5 and M6 parts 1–2 (export, performance) are done.
- [`mcp-plan.md`](mcp-plan.md): Part 1 (MCP endpoint) and gaps 1–6 (lint/schema, inspector and CSV, annotations, ES|QL panels, chained and ad-hoc variables, BubbleUp) are done.
- [`charting-plan.md`](charting-plan.md): A1 (reference lines), A2 (`timeScale` → `per`), A3 (metric chart), A5 (heatmap), A8 (datatable) and B1–B3 (the distribution charts) are done.

Items carry their source IDs (`mcp gap 2`, `charting A1`). A phase starts once
the one before it is done. Items inside a phase can go in parallel. Every item
also follows the [verification rules](#verification-rules) below.

## Phase 0: housekeeping (done 2026-10-03)

- **Vault entity page.** Write `wiki/entities/teleplot.md` in `~/knowledge-base` with the scope decisions from mvp-plan and the decisions recorded in mcp-plan and charting-plan. It goes through the `claude-obsidian` `save` transaction, then the index is rebuilt and `claude-obsidian checkpoint` is run.
- *Done when:* the page exists, links to the related conventions pages, and `retrieve.py "teleplot"` finds it. Written as operation `save-20261003-teleplot-entity` and checkpointed.

## Phase 1: trust and honesty (done 2026-10-03)

- **`charting A1`: reference lines** (`Xy.reference_lines`), drawn by a uPlot hook beside `shade_background` in `src/ui/uplot.rs`. In the same change, Lens annotation layers produce a visible warning instead of being skipped without a word (`src/kibana/lens.rs`, the `layerType != "data"` skip).
- **`mcp gap 2`: query inspector and per-panel CSV**: generated DSL, response, `took`, hit count, download. Shares code with `preview_panel` (`src/mcp.rs`) and `Engine::dashboard_data` (`src/engine.rs`).
- *Done when:* Lens reference lines from the 8.19 and 9.5 fixtures render with parity, no Lens layer is dropped silently, and every panel's numbers can be traced to its query and downloaded as CSV.
- *Result:* the delivery fixtures gained two reference-line panels, exported from Kibana 8.19 and 9.5, with parity tests (`tests/elasticsearch.rs`). Annotation and other non-data Lens layers become chart `warnings`, shown on the panel and by `teleplot check`. **Inspect** shows each panel's table and its searches (`POST /api/dashboards/{id}/panels/{panel}/inspect`), and **CSV** downloads it. MCP `teleplot_preview` takes `inspect: true`. A layer holding only static values sends no search.

## Phase 2: cause and effect (done 2026-10-03, after Phase 3)

- **`mcp gap 3`: event annotations.** Deploy, incident and release markers come from one extra query per dashboard and are drawn on every time series with A1's hook. Kibana annotation layers become importable, replacing the Phase 1 warning.
- *Done when:* `delivery-overview.yaml` shows deploy markers over cycle time, and a Kibana annotation-layer fixture converts.
- *Result:*
  - **Model.** `annotations` on the dashboard (every time-series chart) and on a chart (its own). Each searches a data view, with optional `query`, `end` and `text` fields, or lists fixed `events`.
  - **Queries.** Annotation searches run in the dashboard's `_msearch`, and an identical search is sent once however many charts show it. The dashboard's filters and controls apply unless `ignore_global_filters` is set.
  - **Drawing.** Lines, or shaded ranges, are drawn by the uPlot draw hook on line, band, scatter and heatmap charts. A legend entry per annotation shows the event under the cursor and is kept in PNG export.
  - **Reference dashboard.** `delivery-overview.yaml` marks failed deployments and sev1/sev2 incidents (opened to resolved) over cycle time, lead time, throughput and the heatmap. Marking every deployment would be about 600 lines over 90 days.
  - **Kibana.** Lens annotation layers convert: query and manual annotations, and point and range. The delivery fixtures gained a layer (sev1 incidents, a release freeze, a reorg) exported from 8.19 and 9.5. Its parity test matches Kibana's 12 incident markers and their teams. Kibana's three sample dashboards with annotation layers now convert instead of warning.

## Phase 3: the distribution story (done 2026-10-03, ahead of Phase 2)

- **`charting A5`: heatmap** (`lnsHeatmap`), with uPlot custom paths.
- **`charting B1`: distribution with SLE markers** (log-spaced `Ranges`; p50/p85/p95 drawn with A1).
- **`charting B2`: duration heatmap over time** (`date_histogram` › log `Ranges`, drawn by A5).
- **`charting B3`: percentile lines on the scatter** (`Scatter.percentiles`, computed client-side).
- *Done when:* the Lens heatmap fixture renders with parity, and B1–B3 appear in `delivery-overview.yaml`.
- *Result:*
  - **Heatmap.** `kind: heatmap` draws cells on uPlot, with a one-hue sequential scale and a colour key on the canvas, so PNG export includes the key. Lens `lnsHeatmap` panels convert. The palette is not carried over: it changes only colours.
  - **Fixture.** The delivery fixtures gained a heatmap of deployments per month by team, exported from Kibana 8.19 and 9.5, with a cell-by-cell parity test.
  - **`log_histogram`.** A new bucket with log-spaced, round bounds. It runs as a `range` aggregation with open-ended first and last buckets, so no document is dropped. On `xy` charts it draws as bins on a log x axis, with p50/p85/p95 as `axis: bottom` reference lines (B1). A weekly heatmap over it is B2.
  - **Scatter percentiles.** `percentiles` on the scatter are nearest-rank and computed client-side over the points shown (B3).

## Phase 4: ES|QL (done 2026-10-03)

- **`mcp gap 4`: ES|QL panels and controls.** `POST /_query` columns map onto `LayerData`. Kibana `textBased` panels stop being placeholders. There's no editor with autocomplete.
- **Scope change.** CLAUDE.md lists "Lens ES|QL" as not in MVP. mcp-plan decided to do it after the MVP, so update that line in CLAUDE.md when this phase starts.
- *Done when:* the ES|QL fixtures from 9.5 render with parity.
- *Result:*
  - **Layers.** A layer with an `esql` query runs through `POST /_query`, alongside the dashboard's `_msearch`. Its `value` columns name result columns, and dates come back as times, so `BUCKET` charts get a time axis.
  - **Filters and controls.** As in Kibana, the time range (on the data view's time field), the dashboard query, filter pills and field controls go in as `_query`'s `filter`. `?_tstart`/`?_tend` are filled in.
  - **Kibana.** Lens `textBased` panels convert: all seven ES|QL panels in 9.5's sample Logs dashboard run, and its "Visits" metric matches a DSL count over the same range. The delivery fixtures gained four ES|QL panels (two metrics, a table, a weekly `BUCKET` chart) exported from 8.19 and 9.5, with parity tests against Kibana's screenshots.
  - **Reference dashboard.** `delivery-overview.yaml` has a native ES|QL panel: median cycle time of AI-assisted PRs against the rest, by team.
  - **Not done:** Kibana's ES|QL *variable* controls (`?team` set from a dropdown) need a single-value control Teleplot doesn't have. Panels that use them convert to placeholders, and lint reports them. CLAUDE.md lists them as out of scope.

## Phase 5: what sets Teleplot apart (done 2026-10-03)

- **`mcp gap 6`: compare a selection with the baseline** (BubbleUp). Drag-select on B2 or the scatter, then rank fields and values with `significant_terms`.
- **`mcp gap 5`: chained and ad-hoc variables.** UI ordering for team → repo → service, plus free-form key=value filter pills.
- *Done when:* selecting slow PRs on the seeded data ranks the fields that set them apart, and the chained controls narrow each other.
- *Result:*
  - **Compare (BubbleUp).** Panels with a time x axis have a **Compare** button: a drag then selects a box (time, plus values on a scatter or log-heatmap), and a panel ranks the fields whose values are spread differently in the selection, with numeric medians alongside. On the seeded data, the slow PRs (cycle time ≥ 200 h on the scatter) rank team and repo first, with data at 47% of the selection against 13% overall, and their review and coding times are about five times the median. An integration test checks this on 8.19 and 9.5.
  - **Ranking, not `significant_terms`.** Fields are ranked by total variation distance between the selection's and the baseline's value shares, from exact `terms` counts (as Honeycomb does). `significant_terms`' JLH scores were harder for a leader to read than "47% vs 13%". People are never ranked, and fields with more than 30 values are left out.
  - **Chained controls.** Controls chain left to right, as Kibana's hierarchical chaining does: each control's options honour only the controls before it, and changing one clears the ones after it. `chain_controls: false`, or Kibana's `chainingSystem: NONE`, turns it off.
  - **Ad-hoc filters.** **+ Filter** adds "field is (not) value" pills, kept in the URL, with values suggested under the other filters. A pill applies only to indexes that have its field. Fields about people are never offered or applied: no per-engineer views.

## Phase 6: the sparkline family (done 2026-10-03)

- **`charting A3`: metric chart, 9.x parity.** Background trend, progress bar to `max`, trend badge.
- **`charting A8`: datatable.** Colour by value, summary row, sparkline cells.
- *Result:*
  - **Sparkline layers.** A `sparkline` layer encoding (date `x`, value `y`, optional `by`) is drawn inside another layer's tiles or rows, not on its own. Lens's separate trend-line layer converts to it. Sparklines are small inline SVG, not uPlot instances: they're static and there's one per tile or row.
  - **Metric tiles.** A background trend; `progress: true` for a bar to `max` (vertical by default, as in Kibana; `progress_direction`); and a 9.x trend `badge`. The badge follows Kibana's rules: primary − secondary when comparing with the primary, ↑/↓/=, and green or red by `better: up | down | neither`. It is labelled "Difference" when Lens would be. Lens `trendlineLayerId`, `showBar`/`progressDirection` and `secondaryTrend` convert, replacing the "trend line not shown" warning.
  - **Tables.** Per-column `styles` for colour by value (Kibana palette stops, number or percent, cell/text/badge; text contrast chosen by WCAG) and a summary row (sum/avg/count/min/max over non-empty values, as Kibana computes it). A `sparkline` column from a sparkline layer keyed by the row's first column. Lens `colorMode` + palette and `summaryRow`/`summaryLabel` convert.
  - **Fixtures.** Three panels exported from Kibana 8.19 and 9.5: a lead-time metric with trend line and "Difference" badge, a deployments metric with a progress bar to a target, and a datatable coloured by DORA band with a total and average. Parity tests check Kibana's 20.6 h and 0.9 h, 1,004 of 1,200, the total and average, and every cell's colour. The sample data's datatable palettes now convert too.
  - **Reference dashboard.** The deployment-frequency tile has a trend and a progress bar to 50 a week. The lead-time tile has a trend and a badge. The team table is coloured by band, has a total, and has a deployments-per-week sparkline for each team.

## Phase 7: remaining parity and polish

- **`charting A4`** gauge, **`A6`** legend statistics, **`A7`** interpolation and missing-value fill, **`A9`** treemap and waffle.
- **`charting B6`** calendar heatmap, **`B8`** small multiples.

## Phase 8: comparing teams

- **`charting B4`: box plot** per team or repo, together with **`charting B9`: PR beeswarm** (grouped by team or repo, never by author).
- **`charting B5`: state timeline** for incidents, environments and pipelines.

## Ongoing

- **`charting B7`** (cycle-time phase breakdown) and the other recipes go into `dashboards/delivery-overview.yaml` as each chart lands.
- Every new YAML feature is documented in [`dashboards.md`](dashboards.md) and appears in `teleplot schema`.

## Not scheduled

- **Release (MVP M6 part 3).** Pin the toolchain, write a CalVer release workflow, add an in-repo Homebrew formula, and verify `brew install` on this machine. The done-when is in mvp-plan's milestone table. It is recorded here so it isn't lost, but no phase includes it.
- **Waiting on a decision about sharing and auth: `mcp gaps 7–10`**: digests and alerts, joins across queries, static snapshots, a metric library. Gap 7 conflicts with today's view-only, localhost scope.

## Out of scope

Unchanged:

- the "Deliberately NOT in MVP" list in `CLAUDE.md`;
- the "Not pursued" lists in [`mcp-plan.md`](mcp-plan.md#part-2-feature-gaps-for-technical-users) and [`charting-plan.md`](charting-plan.md#not-pursued-and-why).

## Why this order

- **Phase 1:** A1 and the inspector are both low effort and both fix trust problems. A1 also closes a silent-drop bug.
- **Phase 2:** annotations reuse A1's draw hook.
- **Phases 3–5:** the distribution charts come before BubbleUp, because B2 and the scatter are where its selection starts. ES|QL stays ahead of BubbleUp, as mcp-plan ordered.
- **Phases 6–8:** follow charting-plan's own order, except that the remaining parity and polish (Phase 7) now comes before comparing teams (Phase 8), reordered 2026-10-03.

## Verification rules

- **Kibana-facing items:**
  - a fixture exported from Kibana 8.19 and 9.5 with `make fixtures`, with its screenshots reviewed;
  - an `insta` snapshot in `tests/kibana.rs`;
  - a parity test in `tests/elasticsearch.rs`;
  - no placeholder in `teleplot check`.
- **Native charts:**
  - a golden request in `tests/query.rs`;
  - unit tests for the client-side maths;
  - a panel in `delivery-overview.yaml` drawn over `seed` data.
- **Everything:**
  - checked in Chrome under `make dev`, in light and dark themes, with PNG export and print;
  - `cargo run --release --example bench` stays within [`performance.md`](performance.md);
  - the MCP tests still validate against the declared output schemas;
  - `make pre-commit` passes before each commit.
