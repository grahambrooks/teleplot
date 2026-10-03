# Plan: how Teleplot's charts should evolve

Status: decided, 2026-10-03. The research behind this plan was carried out the
same day, and sources are linked inline. It sits alongside `docs/mcp-plan.md`
and does not repeat items that plan owns: the query inspector (gap 2),
annotations (gap 3), ES|QL (gap 4) and baseline comparison/BubbleUp (gap 6).
A2 is done. The remaining items are ordered, with the mcp-plan gaps, in
[`roadmap.md`](roadmap.md).

## Context

Teleplot draws five chart kinds: `xy`, `metric`, `pie`/`donut` and `table`.
On `xy` it also has `band` and `scatter` encodings (`src/model/mod.rs`). The
research looked at three things: Kibana 9.0–9.5; Grafana 12/13, the
Prometheus 3 UI and Perses; and the delivery-analytics tools. It found two
kinds of gap.

1. **Kibana parity.** Some Kibana panels become placeholders, and some are dropped without a word.
   - Lens heatmap and gauge, and the treemap, mosaic and waffle shapes, become placeholders.
   - Lens annotation and reference-line layers are skipped silently.
   - Lens "normalize by unit" (`timeScale`) is never read, so those panels show raw counts per bucket.
   - Kibana 9.1–9.5 kept adding to the metric chart: trend badges in 9.1, layout in 9.2, and a progress bar in 9.5. It also added XY legend statistics in 9.4. ([metric charts](https://www.elastic.co/docs/explore-analyze/visualize/charts/metric-charts), [line charts](https://www.elastic.co/docs/explore-analyze/visualize/charts/line-charts))
   - Lens added no new chart type in 9.x ([Lens](https://www.elastic.co/docs/explore-analyze/visualize/lens)).
2. **Delivery analytics.** Leaders read flow data through a handful of charts that neither Kibana nor Grafana does well:
   - distributions with service-level expectation (SLE) percentiles, e.g. "85% of PRs merge within N days" ([Swarmia](https://swarmia.com/changelog/2021-03-31-new-pr-insights), [ActionableAgile scatterplot](https://55degrees.atlassian.net/wiki/spaces/AAS/pages/2324987905/Cycle+Time+Scatterplot));
   - duration heatmaps over time ([Grafana heatmap](https://grafana.com/docs/grafana/latest/panels-visualizations/visualizations/heatmap/), Honeycomb);
   - distributions per team;
   - incident state timelines ([Grafana state timeline](https://grafana.com/docs/grafana/v13.0/panels-visualizations/visualizations/));
   - deploy-cadence calendars (Superset, Evidence).

## Constraints

- **Out of scope, unchanged:** CFD/WIP, TSVB/Timelion/Vega/Maps, and the Kibana 9.4+ Dashboards API format.
- **No per-engineer views.** No chart ranks or plots individual engineers. Beeswarms (B9) show PRs grouped by team or repo, never by author, and their tooltip never shows the author.

## Design rules

- **Compose before adding.** Several of the "new" charts are new encodings over operations Teleplot already has:
  - `Histogram` and `Ranges` buckets;
  - `Percentile` metrics;
  - document layers;
  - threshold shading (`shade_background` in `src/ui/uplot.rs`).
- **Choose the renderer by axis.**
  - uPlot draws anything with a continuous axis. The heatmap, box plot, state timeline and sparklines all have upstream demos to port ([uPlot demos](https://leeoniya.github.io/uPlot/demos/index.html)).
  - SVG/DOM draws partition and radial charts, following `PieChart` in `src/ui/panels.rs`.
  - No new JavaScript dependency.
- **Every Kibana-facing item gets a fixture.** It is exported from Kibana 8.19 and 9.5 with `make fixtures`, then gets a snapshot in `tests/kibana.rs` and a parity test in `tests/elasticsearch.rs`.
- **Placeholders stay honest.** A Lens feature Teleplot can't draw becomes a visible placeholder or warning. It is never dropped silently.

## Wave A: Kibana parity

| # | Item | Model | Render | Notes |
|---|---|---|---|---|
| A1 | **Reference lines** in Lens XY: a static value, or a formula/quick function | `Xy.reference_lines` | uPlot draw hook beside `shade_background` | B1 and B3 reuse them for percentile markers. Annotation layers are converted under mcp-plan gap 3; until then they produce a warning instead of being skipped silently |
| A2 | **`timeScale` normalization** (per second, minute, hour or day) | `Column.per` | Scaled after bucketing | Today this is a correctness bug. It is also Prometheus-style rate normalization: changing the interval no longer changes the y axis |
| A3 | **Metric chart, 9.x parity**: background trend, progress bar to `max`, trend badge | `Metric.trend` | A small uPlot sparkline; DOM badge and bar | The trend adds a `date_histogram` search with the same filters |
| A4 | **Gauge** (`lnsGauge`: arc, circle, bullet) | `ChartKind::Gauge` | SVG, reusing `arc()` | Shows a DORA tier at a glance |
| A5 | **Heatmap** (`lnsHeatmap`) | `ChartKind::Heatmap` | uPlot custom paths (`latency-heatmap` demo) | Generic, and reused by B2 |
| A6 | **Legend statistics** (avg, min, max, last, sum, diff %, …) | `Xy.legend_stats` | Client-side | Kibana 9.4 |
| A7 | **Interpolation and missing-value fill** | `Xy.interpolation`, `Xy.fill_missing` | `paths.stepped`/`spline`, `spanGaps` | |
| A8 | **Datatable**: colour by value, summary row, sparkline cells | Table column options | DOM | Grafana has sparkline cells and Kibana doesn't ([Grafana table](https://grafana.com/docs/grafana/next/panels-visualizations/visualizations/table/)) |
| A9 | **Treemap and waffle** | `ChartKind::Treemap`/`Waffle` | SVG | The treemap suits investment allocation. Mosaic, tag cloud and region map stay placeholders |

## Wave B: charts native to delivery analytics

| # | Chart | Built from | Why |
|---|---|---|---|
| B1 | **Distribution with SLE markers**: log-spaced buckets, p50/p85/p95 lines | Log-spaced bounds as a `Ranges` op (no scripts); markers from A1 | A median hides the tail; an SLE ("85% within N days") is a commitment you can actually make |
| B2 | **Duration heatmap over time** | `date_histogram` › log `Ranges`, drawn by A5 | Shows a distribution shifting week to week. Drag-select is where BubbleUp (mcp-plan gap 6) starts |
| B3 | **Percentile lines on the scatter** | `Scatter.percentiles`, computed client-side | The industry-standard cycle-time scatter |
| B4 | **Box plot per team or repo** | `terms` › `percentiles` | Compares distributions without ranking people. Kibana can only do this through Vega |
| B5 | **State timeline**: incidents, environment or pipeline state | Document layer with start, end and state | Makes MTTR and overlapping incidents visible |
| B6 | **Calendar heatmap**: deploys per day | `date_histogram 1d`, SVG | Deploy cadence, weekend work, release freezes |
| B7 | **Cycle-time phase breakdown** | **No new code:** `bar_horizontal_stacked` with average columns | A recipe in `delivery-overview.yaml` ([LinearB](https://linearb.io/blog/cycle-time)) |
| B8 | **Small multiples**: `facet_by: <terms>` on a shared y scale | One outer `terms`; grid layout | Team trends side by side |
| B9 | **PR beeswarm by team or repo**: one dot per PR, dodged so none overlap | Document layer; the dodge layout is done in Rust | Honest at small n, where a box plot of 8 PRs looks precise but isn't. Shows bimodal work and outliers. Pairs with B4 |

## Not pursued, and why

- **Sankey and funnel.** uPlot is a poor fit, and the treemap plus 100%-stacked bars already cover investment flow.
- **CFD/WIP and Monte Carlo forecasting.** Out of the MVP because they need snapshot documents. Revisit them together.
- **Node graph, geomap/region map, tag cloud, candlestick.** Little value for delivery data.
- **Beeswarm grouped or coloured by author.** Excluded by design.
- **Vega-Lite.** Out of scope. ES|QL charts belong to mcp-plan gap 4.

## Order

1. **A2**, the correctness bug, and **A1**, which B1 and B3 reuse.
2. **A5**, then **B1, B2 and B3**: the distribution story, which also sets up BubbleUp.
3. **A3 and A8**, which share the sparkline.
4. **B4 with B9**, then **B5**.
5. **A4, A6, A7 and A9**, then **B6 and B8**.
6. **B7 and the other recipes**, added to `delivery-overview.yaml` as each chart lands.

## Verification

- **Wave A**, for each item:
  - a fixture from Kibana 8.19 and 9.5, with its screenshots reviewed;
  - `teleplot check` shows no placeholder;
  - an `insta` snapshot;
  - a parity test against Kibana's numbers.
- **Wave B**, for each item:
  - a golden request in `tests/query.rs`;
  - unit tests for the client-side maths;
  - a panel in `delivery-overview.yaml` drawn over `seed` data.
- **Everything:**
  - checked in Chrome under `make dev`, in light and dark themes, with PNG export and print;
  - `cargo run --release --example bench` stays within the budgets in `docs/performance.md`;
  - `make pre-commit` passes before each commit.
