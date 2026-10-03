# Dashboard cookbook

Working YAML for the panels delivery dashboards need. Each `yaml` block below
is a complete panel (Teleplot's test suite parses and lints every one), written
against a typical delivery schema:

| Index | Time field | Fields used |
|---|---|---|
| `delivery-deployments` | `deployed_at` | `team`, `repo`, `result` (`success`/`failure`), `is_rework`, `change_lead_time_hours` |
| `delivery-prs` | `merged_at` | `team`, `repo`, `ai_assisted`, `cycle_time_hours`, `review_hours` |
| `delivery-incidents` | `created_at` | `team`, `severity`, `recovery_hours`, `resolved_at` |

Replace index and field names with what you found in the cluster. Every
option is described in `docs/dashboards.md`; `teleplot schema` has the full
format.

## Contents

- [The dashboard around the panels](#the-dashboard-around-the-panels)
- [DORA tiles](#dora-tiles)
- [Durations: band, scatter, distribution, heatmap](#durations)
- [Trends over time](#trends-over-time)
- [Tables](#tables)
- [Gauges, calendars, shares](#gauges-calendars-shares)
- [ES|QL](#esql)

## The dashboard around the panels

```text
id: delivery                     # the URL: /d/delivery
title: Delivery
time: { from: now-90d/d, to: now }
min_samples: 5                   # flag buckets with fewer documents
controls:                        # chained left to right: team narrows repo
  - { id: team, title: Team, field: team, data_view: { index: delivery-prs, time_field: merged_at } }
  - { id: repo, title: Repo, field: repo, data_view: { index: delivery-prs, time_field: merged_at } }
annotations:                     # marked on every time chart
  - id: failed-deploys
    label: Failed deployments
    data_view: { index: delivery-deployments, time_field: deployed_at }
    query: { language: kql, text: "result : failure" }
    text: team
panels:
  - ...                          # the panels below
```

Grid: 48 columns wide, `h` in rows of about 20 px. Tiles are about
`w: 12, h: 7`; charts `w: 24, h: 12`.

## DORA tiles

Deployment frequency per week, with the previous period and a progress bar to
a target:

```yaml
id: deploy-frequency
title: Deployment frequency
grid: { x: 0, y: 0, w: 12, h: 7 }
content:
  type: chart
  kind: metric
  query: { language: kql, text: "is_rework : false" }
  layers:
    - id: deploys
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: n, op: count, helper: true }
        - { id: range, op: time_range, helper: true }
        - id: per-week
          label: Deployments per week
          op: formula
          text: "count() / (time_range() / 604800000)"
          expr: { call: divide, args: [n, { call: divide, args: [range, 604800000.0] }] }
          format: { kind: number, decimals: 1 }
        - { id: n-prev, op: count, time_shift: previous, helper: true }
        - id: per-week-prev
          label: Previous period
          op: formula
          text: "count(shift=previous) / (time_range() / 604800000)"
          expr: { call: divide, args: [n-prev, { call: divide, args: [range, 604800000.0] }] }
          format: { kind: number, decimals: 1 }
        - { id: target, op: static_value, value: 50 }
      encoding: { as: metric, value: per-week, secondary: per-week-prev, max: target, progress: true }
```

Lead time: a median, benchmark bands, a weekly trend behind the value, and a
trend badge (lower is better):

```yaml
id: lead-time
title: Change lead time
grid: { x: 12, y: 0, w: 12, h: 7 }
content:
  type: chart
  kind: metric
  query: { language: kql, text: "is_rework : false" }
  layers:
    - id: lead
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - id: median
          label: Median, hours
          op: median
          field: change_lead_time_hours
          format: { kind: number, decimals: 1, suffix: " h" }
          thresholds:
            - { label: Elite, to: 24, color: "#73BF69" }
            - { label: High, from: 24, to: 168, color: "#5794F2" }
            - { label: Medium, from: 168, to: 720, color: "#FF9830" }
            - { label: Low, from: 720, color: "#F2495C" }
        - id: median-prev
          label: Change on previous period
          op: median
          field: change_lead_time_hours
          time_shift: previous
          format: { kind: number, decimals: 1, suffix: " h" }
      encoding: { as: metric, value: median, secondary: median-prev, badge: { better: down } }
    - id: weekly
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: week, op: date_histogram, field: deployed_at, interval: 1w }
        - { id: median, op: median, field: change_lead_time_hours }
      encoding: { as: sparkline, x: week, y: median }
```

Change fail rate: a ratio of two filtered counts:

```yaml
id: change-fail-rate
title: Change fail rate
grid: { x: 24, y: 0, w: 12, h: 7 }
content:
  type: chart
  kind: metric
  query: { language: kql, text: "is_rework : false" }
  layers:
    - id: cfr
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: failed, op: count, filter: { language: kql, text: "result : failure" }, helper: true }
        - { id: all, op: count, helper: true }
        - id: rate
          label: Failed deployments
          op: formula
          text: "count(kql='result : failure') / count()"
          expr: { call: divide, args: [failed, all] }
          format: { kind: percent, decimals: 1 }
      encoding: { as: metric, value: rate }
```

## Durations

Durations are long-tailed: show the middle and the tail, not an average.

Weekly median inside its 25th–75th percentile band:

```yaml
id: lead-time-band
title: Lead time, weekly median and middle 50%
grid: { x: 0, y: 7, w: 24, h: 12 }
content:
  type: chart
  kind: xy
  layers:
    - id: band
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: week, op: date_histogram, field: deployed_at, interval: 1w, include_empty: true }
        - { id: p25, label: 25th percentile, op: percentile, field: change_lead_time_hours, percent: 25.0 }
        - { id: p50, label: Median, op: median, field: change_lead_time_hours }
        - { id: p75, label: 75th percentile, op: percentile, field: change_lead_time_hours, percent: 75.0 }
      encoding: { as: band, x: week, low: p25, mid: p50, high: p75 }
```

Every PR as a point, a rolling median, and service-level expectation lines
(compare: drag a box over slow PRs to see what sets them apart):

```yaml
id: cycle-scatter
title: PR cycle time
grid: { x: 24, y: 7, w: 24, h: 12 }
content:
  type: chart
  kind: xy
  query: { language: kql, text: "cycle_time_hours : *" }
  layers:
    - id: prs
      data_view: { index: delivery-prs, time_field: merged_at }
      documents: { limit: 3000, sort: merged_at }
      columns:
        - { id: merged, label: Merged, op: value, field: merged_at }
        - { id: hours, label: Cycle time, op: value, field: cycle_time_hours, format: { kind: number, decimals: 1, suffix: " h" } }
        - { id: trend, label: Rolling median (50 PRs), op: rolling_median, of: hours, window: 50 }
      encoding: { as: scatter, x: merged, y: hours, trend: trend, log_scale: true, percentiles: [50, 85, 95] }
```

The distribution on log-spaced buckets, with p50/p85/p95 as vertical lines:

```yaml
id: cycle-distribution
title: PR cycle time distribution
grid: { x: 0, y: 19, w: 24, h: 12 }
content:
  type: chart
  kind: xy
  layers:
    - id: prs
      data_view: { index: delivery-prs, time_field: merged_at }
      columns:
        - { id: hours, label: Cycle time, op: log_histogram, field: cycle_time_hours, min: 2, max: 2000, format: { kind: number, suffix: " h" } }
        - { id: n, label: Merged PRs, op: count }
      encoding: { as: xy, series: bar, x: hours, y: [n] }
    - id: sle
      data_view: { index: delivery-prs, time_field: merged_at }
      columns:
        - { id: p50, label: p50, op: percentile, field: cycle_time_hours, percent: 50.0 }
        - { id: p85, label: p85, op: percentile, field: cycle_time_hours, percent: 85.0 }
        - { id: p95, label: p95, op: percentile, field: cycle_time_hours, percent: 95.0 }
      encoding:
        as: reference_lines
        lines:
          - { column: p50, axis: bottom, style: dashed }
          - { column: p85, axis: bottom, style: dashed }
          - { column: p95, axis: bottom, style: dashed }
```

The same buckets week by week:

```yaml
id: cycle-heatmap
title: PR cycle time by week
grid: { x: 24, y: 19, w: 24, h: 12 }
content:
  type: chart
  kind: heatmap
  layers:
    - id: prs
      data_view: { index: delivery-prs, time_field: merged_at }
      columns:
        - { id: week, op: date_histogram, field: merged_at, interval: 1w }
        - { id: hours, label: Cycle time, op: log_histogram, field: cycle_time_hours, min: 2, max: 2000, format: { kind: number, suffix: " h" } }
        - { id: n, label: Merged PRs, op: count }
      encoding: { as: heatmap, x: week, y: hours, value: n }
```

## Trends over time

Throughput as bars with a share as a line on its own axis:

```yaml
id: throughput
title: Merged PRs per week, and the AI-assisted share
grid: { x: 0, y: 31, w: 24, h: 12 }
content:
  type: chart
  kind: xy
  layers:
    - id: merged
      data_view: { index: delivery-prs, time_field: merged_at }
      columns:
        - { id: week, op: date_histogram, field: merged_at, interval: 1w, include_empty: true }
        - { id: merged, label: Merged PRs, op: count }
      encoding: { as: xy, series: bar, x: week, y: [merged], legend_stats: [average, last] }
    - id: ai
      data_view: { index: delivery-prs, time_field: merged_at }
      columns:
        - { id: week, op: date_histogram, field: merged_at, interval: 1w, include_empty: true }
        - { id: ai, op: count, filter: { language: kql, text: "ai_assisted : true" }, helper: true }
        - { id: all, op: count, helper: true }
        - id: share
          label: AI-assisted
          op: formula
          text: "count(kql='ai_assisted : true') / count()"
          expr: { call: divide, args: [ai, all] }
          format: { kind: percent, decimals: 0 }
      encoding: { as: xy, series: line, x: week, y: [share] }
```

Small multiples: one chart per team on a shared scale (never per person):

```yaml
id: team-multiples
title: Weekly median cycle time, team by team
grid: { x: 24, y: 31, w: 24, h: 16 }
content:
  type: chart
  kind: xy
  layers:
    - id: weekly
      data_view: { index: delivery-prs, time_field: merged_at }
      columns:
        - { id: team, op: terms, fields: [team], size: 8 }
        - { id: week, op: date_histogram, field: merged_at, interval: 1w }
        - { id: median, label: Median cycle time, op: median, field: cycle_time_hours }
      encoding: { as: xy, series: line, x: week, y: [median], facet: team, missing: gap }
```

## Tables

DORA by team: coloured by band, a total, and a weekly sparkline per team.
Clicking a team selects it in the Team control:

```yaml
id: teams
title: DORA by team
grid: { x: 0, y: 47, w: 48, h: 14 }
content:
  type: chart
  kind: table
  query: { language: kql, text: "is_rework : false" }
  layers:
    - id: teams
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: team, label: Team, op: terms, fields: [team], size: 25, order: { by: column, column: n } }
        - { id: n, label: Deployments, op: count }
        - { id: lead, label: Median lead time, op: median, field: change_lead_time_hours, format: { kind: number, decimals: 1, suffix: " h" } }
      encoding:
        as: table
        columns: [team, n, lead]
        styles:
          - { column: n, summary: sum, summary_label: Total }
          - column: lead
            color:
              mode: cell
              stops:
                - { from: 0, color: "#73BF69" }
                - { from: 24, color: "#5794F2" }
                - { from: 168, color: "#FF9830" }
                - { from: 720, color: "#F2495C" }
        sparkline: { layer: weekly, label: Deployments per week }
    - id: weekly
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: team, op: terms, fields: [team], size: 25 }
        - { id: week, op: date_histogram, field: deployed_at, interval: 1w }
        - { id: n, op: count }
      encoding: { as: sparkline, x: week, y: n, by: team }
```

## Gauges, calendars, shares

```yaml
id: cfr-gauge
title: Change fail rate against the DORA bands
grid: { x: 0, y: 61, w: 12, h: 10 }
content:
  type: chart
  kind: gauge
  layers:
    - id: cfr
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: failed, op: count, filter: { language: kql, text: "result : failure" }, helper: true }
        - { id: all, op: count, helper: true }
        - id: rate
          label: Failed deployments
          op: formula
          text: "count(kql='result : failure') / count()"
          expr: { call: divide, args: [failed, all] }
          format: { kind: percent, decimals: 1 }
        - { id: max, op: static_value, value: 0.3 }
      encoding:
        as: gauge
        value: rate
        max: max
        bands:
          stops:
            - { from: 0, color: "#73BF69" }
            - { from: 0.05, color: "#5794F2" }
            - { from: 0.15, color: "#F2495C" }
```

```yaml
id: deploy-calendar
title: Deployments per day
grid: { x: 12, y: 61, w: 24, h: 10 }
content:
  type: chart
  kind: calendar
  layers:
    - id: days
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: day, op: date_histogram, field: deployed_at, interval: 1d, include_empty: true }
        - { id: n, label: deployments, op: count }
      encoding: { as: calendar, date: day, value: n }
```

```yaml
id: results
title: Deployments by result
grid: { x: 36, y: 61, w: 12, h: 10 }
content:
  type: chart
  kind: waffle
  layers:
    - id: results
      data_view: { index: delivery-deployments, time_field: deployed_at }
      columns:
        - { id: result, op: terms, fields: [result], size: 5 }
        - { id: n, label: Deployments, op: count }
      encoding: { as: pie, groups: [result], values: [n] }
```

`kind: pie`, `donut` and `treemap` (up to two `groups`) take the same encoding.

## ES|QL

For comparisons the query DSL can't make in one search. The dashboard's time
range, query, filters and controls still apply (sent as the request's
`filter`); `?_tstart`/`?_tend` are the range's ends:

```yaml
id: ai-cycle
title: Does AI assistance shorten cycle time?
grid: { x: 0, y: 71, w: 48, h: 12 }
content:
  type: chart
  kind: table
  layers:
    - id: ai
      data_view: { index: delivery-prs, time_field: merged_at }
      esql:
        query: |
          FROM delivery-prs
          | WHERE cycle_time_hours IS NOT NULL
          | STATS ai = MEDIAN(CASE(ai_assisted, cycle_time_hours)),
                  other = MEDIAN(CASE(NOT ai_assisted, cycle_time_hours)) BY team
          | EVAL change = ai / other - 1
          | SORT change
      columns:
        - { id: team, label: Team, op: value, field: team }
        - { id: change, label: Change, op: value, field: change, format: { kind: percent, decimals: 0 } }
      encoding: { as: table, columns: [team, change] }
```

Quote labels that contain a comma (`label: "Median, AI-assisted"`): in a
`{ … }` mapping an unquoted comma ends the value.
