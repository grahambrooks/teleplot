# Roadmap: the remaining work, in order

Status: current as of 2026-10-03. This is the one place that orders the work.
The research and rationale stay in the source plans:

- [`mvp-plan.md`](mvp-plan.md): M0–M5 and M6 parts 1–2 (export, performance) are done.
- [`mcp-plan.md`](mcp-plan.md): Part 1 (MCP endpoint) and gap 1 (lint/schema) are done.
- [`charting-plan.md`](charting-plan.md): A2 (`timeScale` → `per`) is done.

Items carry their source IDs (`mcp gap 2`, `charting A1`). A phase starts once
the one before it is done. Items inside a phase can go in parallel. Every item
also follows the [verification rules](#verification-rules) below.

## Phase 0: housekeeping

- **Vault entity page.** Write `wiki/entities/teleplot.md` in `~/knowledge-base` with the scope decisions from mvp-plan and the decisions recorded in mcp-plan and charting-plan. It goes through the `claude-obsidian` `save` transaction, then the index is rebuilt and `claude-obsidian checkpoint` is run.
- *Done when:* the page exists, links to the related conventions pages, and `retrieve.py "teleplot"` finds it.

## Phase 1: trust and honesty

- **`charting A1`: reference lines** (`Xy.reference_lines`), drawn by a uPlot hook beside `shade_background` in `src/ui/uplot.rs`. In the same change, Lens annotation layers produce a visible warning instead of being skipped without a word (`src/kibana/lens.rs`, the `layerType != "data"` skip).
- **`mcp gap 2`: query inspector and per-panel CSV**: generated DSL, response, `took`, hit count, download. Shares code with `preview_panel` (`src/mcp.rs`) and `Engine::dashboard_data` (`src/engine.rs`).
- *Done when:* Lens reference lines from the 8.19 and 9.5 fixtures render with parity, no Lens layer is dropped silently, and every panel's numbers can be traced to its query and downloaded as CSV.

## Phase 2: cause and effect

- **`mcp gap 3`: event annotations.** Deploy, incident and release markers come from one extra query per dashboard and are drawn on every time series with A1's hook. Kibana annotation layers become importable, replacing the Phase 1 warning.
- *Done when:* `delivery-overview.yaml` shows deploy markers over cycle time, and a Kibana annotation-layer fixture converts.

## Phase 3: the distribution story

- **`charting A5`: heatmap** (`lnsHeatmap`), with uPlot custom paths.
- **`charting B1`: distribution with SLE markers** (log-spaced `Ranges`; p50/p85/p95 drawn with A1).
- **`charting B2`: duration heatmap over time** (`date_histogram` › log `Ranges`, drawn by A5).
- **`charting B3`: percentile lines on the scatter** (`Scatter.percentiles`, computed client-side).
- *Done when:* the Lens heatmap fixture renders with parity, and B1–B3 appear in `delivery-overview.yaml`.

## Phase 4: ES|QL

- **`mcp gap 4`: ES|QL panels and controls.** `POST /_query` columns map onto `LayerData`. Kibana `textBased` panels stop being placeholders. There's no editor with autocomplete.
- **Scope change.** CLAUDE.md lists "Lens ES|QL" as not in MVP. mcp-plan decided to do it after the MVP, so update that line in CLAUDE.md when this phase starts.
- *Done when:* the ES|QL fixtures from 9.5 render with parity.

## Phase 5: what sets Teleplot apart

- **`mcp gap 6`: compare a selection with the baseline** (BubbleUp). Drag-select on B2 or the scatter, then rank fields and values with `significant_terms`.
- **`mcp gap 5`: chained and ad-hoc variables.** UI ordering for team → repo → service, plus free-form key=value filter pills.
- *Done when:* selecting slow PRs on the seeded data ranks the fields that set them apart, and the chained controls narrow each other.

## Phase 6: the sparkline family

- **`charting A3`: metric chart, 9.x parity.** Background trend, progress bar to `max`, trend badge.
- **`charting A8`: datatable.** Colour by value, summary row, sparkline cells.

## Phase 7: comparing teams

- **`charting B4`: box plot** per team or repo, together with **`charting B9`: PR beeswarm** (grouped by team or repo, never by author).
- **`charting B5`: state timeline** for incidents, environments and pipelines.

## Phase 8: remaining parity and polish

- **`charting A4`** gauge, **`A6`** legend statistics, **`A7`** interpolation and missing-value fill, **`A9`** treemap and waffle.
- **`charting B6`** calendar heatmap, **`B8`** small multiples.

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
- **Phases 6–8:** follow charting-plan's own order.

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
