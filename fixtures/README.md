# Fixtures

Real Kibana exports and the data behind them. The production exports Teleplot
must render live on another machine, so these are manufactured to resemble
them, and wherever possible are Kibana's own output rather than hand-written.

| Path | What | Made by |
|---|---|---|
| `kibana/<version>/sample-data.ndjson` | Kibana's eCommerce, Flights and Logs sample dashboards, with every object they reference | Kibana's sample data, exported |
| `kibana/<version>/delivery.ndjson` | Three delivery-analytics dashboards (DORA overview, PR flow, investment & flow) | `examples/kibana_fixtures.rs`, imported into Kibana and **re-exported** |
| `kibana/<version>/screenshots/*.png` | How Kibana renders the delivery dashboards over the seeded data (2026-04-01 – 2026-10-02) | Kibana reporting |
| `kibana/edge-cases/edge-cases.ndjson` | Inputs a parser must survive: pre-8.6 `indexpattern` Lens state, broken and missing references, unsupported panel types, per-panel time ranges, Lucene queries with negated/disabled/custom/multi-value filters, a 7.x data view with a `fields` list | Derived by hand from the 8.19 delivery export |

Versions: `8.19` is Kibana 8.19.22, `9.5` is Kibana 9.5.3.

## What the exports cover

- **Lens, by value:**
  - Chart types: XY (line, bar, stacked bar, percentage bar, stacked area, horizontal bar), metric (including a time-shifted secondary metric), pie/donut, datatable and heatmap (deployments per month by team, with cell labels).
  - Operations: count, sum, average, median, percentile, unique_count, last_value, date_histogram, terms, filters, range (histogram), formula (with its expanded `math`/`tinymathAst` helper columns), moving_average, cumulative_sum and differences.
  - Per-column KQL and Lucene filters, `timeShift`, and `timeScale` (normalize by unit: a monthly table and a whole-range tile).
  - Reference-line layers: a static target shaded below, and an average over the whole range (line chart); a static line across horizontal bars.
- **Lens, by reference:** one panel in the delivery set and one in the sample set.
- **Legacy visualizations:** line, pie, table and metric by reference; markdown by value and by reference.
- **Dashboard-level settings:** KQL query, a negated phrase filter, `timeRestore`, refresh interval, and an options-list control (`controlGroupInput`).
- **Lens metric, 9.x:** a background trend line, a `secondaryTrend` badge comparing with the primary ("Difference"), and a progress bar to a static `max`.
- **Lens datatable styling:** colour by value (a custom palette in cell mode) and summary rows (sum with a label, average).
- **Lens gauge, treemap, waffle and line options:** a semicircle gauge with palette bands, a static max and a goal; a waffle; a two-level treemap; and a step line with `fittingFunction: Carry` and legend statistics (average, max, last value).
- **Lens ES|QL (`textBased`):** two metrics (one percent-formatted), a table, and a weekly `BUCKET` bar chart over ad hoc data views.
- **Lens annotation layers:** a query annotation (sev1 incidents, labelled by team), a manual range (a release freeze) and a manual point (a reorg) on the lead-time chart; and the query annotations in the sample data.
- **Panels outside the MVP, which must render as placeholders:** Vega, Maps and saved searches. (The 9.5 sample data's Lens ES|QL panels now convert.)

## Data

`cargo run --example seed -- --es http://localhost:9208 --end 2026-10-01` writes
about 5,400 documents to `delivery-prs`, `delivery-deployments`,
`delivery-incidents` and `delivery-workitems`:

- Eight teams with Elite → Low performance profiles.
- Log-normal durations.
- One team that improves over the period.
- AI-assisted share rising from 15% to 65%.
- Teams in four time zones.

The output is deterministic for a given `--seed`, `--days` and `--end`. `--scale N` multiplies the volume for performance tests.

## Rebuilding

```sh
fixtures/make-fixtures.sh        # both stacks (Docker); v8 or v9 for one
```

The script starts the stacks, seeds them, installs the sample data, imports
and exports the delivery dashboards, and renders the screenshots. Look at
the screenshots before committing: they are the evidence that Kibana
renders every panel.
