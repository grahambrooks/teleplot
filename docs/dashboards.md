# Teleplot dashboards (YAML)

Teleplot serves every dashboard file in its dashboards directory and reloads
it when a file changes:

- **Kibana exports (`.ndjson`).** Supported panels render as they do in Kibana; anything else shows as a labelled card. Run `teleplot check` to see which is which.
- **Teleplot YAML (`.yaml`).** Covered below. `teleplot convert export.ndjson --out dashboards/` turns a Kibana export into YAML to start from.

[`dashboards/delivery-overview.yaml`](../dashboards/delivery-overview.yaml) is
a complete example and uses every feature described here.

## Shape

```yaml
id: delivery-overview            # used in the URL: /d/delivery-overview
title: Delivery Overview
time: { from: now-90d/d, to: now }   # default range; Kibana date math
min_samples: 5                   # flag buckets with fewer documents
query: { language: kql, text: "" }   # applies to every panel (optional)
controls:                        # viewer-selectable filters, shown as dropdowns
  - { id: team, title: Team, field: team, data_view: { index: delivery-prs, time_field: merged_at } }
annotations: [...]               # events marked on every time-series chart (below)
panels:
  - id: lead-time
    title: Change lead time
    description: Median hours from commit to production.   # optional; shown as ⓘ by the title
    grid: { x: 0, y: 0, w: 12, h: 7 }   # Kibana's 48-column grid, 20px rows
    content: { type: chart, kind: metric, layers: [...] }
```

A panel's `description` says what it measures and how to read it: the
definition behind a number, its unit, what counts. It shows as an ⓘ beside
the title, and hovering or focusing it reveals the text. Kibana panel
descriptions convert as Kibana shows them: the dashboard's panel setting, or
else a library visualization's own description. Descriptions don't appear in printed PDFs.

Panel `content` is one of:

- `type: chart`, which has a `kind` (`xy`, `metric`, `pie`, `donut`, `treemap`, `waffle`, `table`, `heatmap`, `gauge` or `calendar`), an optional `query` and `filters`, `layers`, and `annotations` of its own. Charts converted from Kibana can also carry `warnings`, naming parts of the original that Teleplot does not draw (a Lens annotation group linked from the library, for example). The panel shows them.
- `type: markdown` with `text`.

## Layers and columns

A layer queries one data view. Its columns are either:

- **Buckets:** `date_histogram`, `terms`, `filters`, `histogram`, `ranges`, `log_histogram`;
- **Per row** (document and ES|QL layers): `value`;
- **Metrics:** `count`, `sum`, `average`, `min`, `max`, `median`, `percentile`, `unique_count`, `last_value`, `standard_deviation`, `static_value`, `time_range`;
- **Computed from other columns:** `formula`, `moving_average`, `cumulative_sum`, `differences`, `counter_rate`, `overall`, `rolling_median`.

Buckets nest in the order listed. Any column can also have:

| Key | Meaning |
|---|---|
| `label` | Shown in legends, tables and tiles |
| `format` | `{ kind: number \| percent \| bytes \| duration \| currency, decimals, suffix, compact }` |
| `filter` | `{ language: kql, text: ... }`, restricting just this column |
| `time_shift` | `previous` (the range before this one) or a duration such as `1w` |
| `per` | `second`, `minute`, `hour`, `day` or `week`: a rate, with each value divided by the length of its date-histogram bucket, or by the whole range when there is no date histogram. Partial buckets at the range's edges count only the part inside the range. Shown with a suffix such as `/d`, as Kibana's "normalize by unit". Metric and computed columns only |
| `helper: true` | An input to another column, not drawn |
| `thresholds` | Benchmark bands, below |

The `encoding` (`as:`) says how columns are drawn:

| `as` | Fields |
|---|---|
| `xy` | `series` (line, area, bar, …, `bar_percentage_stacked`), `x`, `y: [...]`, `split`; and `interpolation`, `missing`, `legend_stats`, `facet` (below) |
| `metric` | `value`, `secondary`, `breakdown`, `max`, `progress`, `badge` (below) |
| `pie` | `groups`, `values`: for `pie`, `donut`, `treemap` (up to two levels of groups) and `waffle` charts |
| `gauge` | `value`, `min`, `max`, `goal`, `shape` (`arc`, `semi_circle`, `circle`, `horizontal_bullet`, `vertical_bullet`), `bands` (below) |
| `calendar` | `date` (a 1-day date histogram), `value`: one square a day, in weeks |
| `table` | `columns`, `styles`, `sparkline` (below) |
| `sparkline` | `x` (a date histogram), `y`, `by`: a small line drawn inside the chart's metric tiles or table rows |
| `band` | `x`, `low`, `mid`, `high`: a line inside a shaded band, e.g. median within p25–p75 |
| `scatter` | `x`, `y`, `split`, `trend`, `log_scale`, `percentiles`: needs a document layer |
| `heatmap` | `x`, `y`, `value`: cells coloured by `value` (for `kind: heatmap`) |
| `reference_lines` | `lines`: lines at fixed values across the chart's other layers (below) |

### Formulas

A formula combines other columns of its layer:

```yaml
- { id: failed, op: count, filter: { language: kql, text: "result : failure" }, helper: true }
- { id: all, op: count, helper: true }
- id: rate
  op: formula
  text: "count(kql='result : failure') / count()"     # shown to people
  expr: { call: divide, args: [failed, all] }         # what is computed
  format: { kind: percent, decimals: 1 }
```

`expr` is a number, a column id, or `{ call, args }`. The available calls are:

- arithmetic: `add`, `subtract`, `multiply`, `divide`
- functions: `abs`, `pow`, `sqrt`, `round`, `floor`, `ceil`, `log`, `exp`, `mod`, `clamp`, `pick_min`, `pick_max`, `defaults`

Division by zero leaves the cell empty.

### Comparing with the previous period

Give a metric tile a `secondary` column that measures the same thing with
`time_shift: previous`. That can be the same operation, or a formula over
shifted parts. The tile then shows the change as a percentage. When the two
measure different things, the tile shows both values but no percentage.

### Benchmark bands

```yaml
thresholds:
  - { label: Elite, to: 24 }              # from inclusive, to exclusive
  - { label: High, from: 24, to: 168 }
  - { label: Low, from: 168, color: "#F2495C" }
```

Where the bands appear:

- **Metric tiles:** a badge naming the band the value falls in.
- **Tables:** a coloured dot beside the value.
- **Line, band and scatter charts:** shading behind the data.

Default colours run green → blue → orange → red. Choose bands deliberately.
DORA's 2025 report no longer publishes elite/high/medium/low levels, and
whether a lower or a higher value is better depends on the metric.

### Reference lines

A layer encoded as `reference_lines` draws a line across an `xy` chart for
each of its columns: a target, or a value computed over the whole range,
such as a percentile. Its columns must not be buckets, so each has one value.

```yaml
- id: sle
  data_view: { index: delivery-prs, time_field: merged_at }
  columns:
    - { id: target, label: Target, op: static_value, value: 24 }
    - { id: p85, label: 85% of PRs within, op: percentile, field: cycle_time_hours, percent: 85 }
  encoding:
    as: reference_lines
    lines:
      - { column: target, color: "#73BF69", fill: below }
      - { column: p85, style: dashed }
```

Each line can set:

- `axis`: `left` (the default) or `right` for a horizontal line, `bottom` for a vertical line at an x value;
- `color`;
- `style`: `solid`, `dashed` or `dotted`;
- `fill`: `above` or `below` to shade one side;
- `show_label: false` to hide its label and value.

The value axis stretches so every line is visible. Kibana's Lens
reference-line layers convert to this.

### Distributions: log histograms, percentiles and heatmaps

Durations such as cycle time have long tails, so a few slow PRs squash an
evenly bucketed histogram. A `log_histogram` buckets them on a log scale:

```yaml
- { id: hours, op: log_histogram, field: cycle_time_hours, min: 2, max: 2000, format: { kind: number, suffix: " h" } }
```

- `per_decade` sets how many buckets per factor of ten. With 1, 2 or 3 the bounds are round: 1, 3, or 1, 2, 5 (the default is 3). With more, they are evenly spaced.
- The first and last buckets are open. Values below `min` count in the first, and values above `max` count in the last, so no document is left out.
- The search is a `range` aggregation, so no scripts are involved.
- On an `xy` chart, the buckets are drawn as bins on a log x axis. A `reference_lines` layer with `axis: bottom` and `percentile` columns marks service-level expectations, for example "85% of PRs merge within 254 h".

A scatter's `percentiles: [50, 85, 95]` draws dashed lines at those
percentiles of the points shown. They are computed in the browser, by
nearest rank: the smallest value with at least that share of points at or
below it. So they cover the document layer's `limit`, not every document.
Use a `reference_lines` layer of `percentile` columns to cover every
document; Elasticsearch computes those approximately.

A `heatmap` chart colours one cell per pair of buckets:

```yaml
kind: heatmap
layers:
  - id: prs
    data_view: { index: delivery-prs, time_field: merged_at }
    columns:
      - { id: week, op: date_histogram, field: merged_at, interval: 1w }
      - { id: hours, op: log_histogram, field: cycle_time_hours, min: 2, max: 2000 }
      - { id: n, label: Merged PRs, op: count }
    encoding: { as: heatmap, x: week, y: hours, value: n }
```

- `x` and `y` are bucket columns, and `y` is optional.
- Numeric rows run upwards in value order, and log-histogram rows are labelled as ranges. Other rows keep their table order from the top down, as in Kibana.
- Colours run from light (smallest) to dark (largest) on one hue, with a key beside the chart. In the dark theme, small values are dark instead.
- Kibana's Lens heatmaps convert to this. Their colour palette is not carried over, because it changes only the colours.

### Annotations

Annotations mark events on time-series charts. A failed deployment, an
incident or a release freeze then sits on the same axis as the metric it
might explain. Dashboard `annotations` are marked on every chart with a time
x axis: line, bar and area charts over a date histogram, bands, scatters
over the data view's time field, and heatmaps. A chart can also have
`annotations` of its own.

```yaml
annotations:
  - id: failed-deploys
    label: Failed deployments
    data_view: { index: delivery-deployments, time_field: deployed_at }
    query: { language: kql, text: "result : failure" }
    text: team                     # shown on hover
    color: "#F2495C"
  - id: incidents
    label: Major incidents
    data_view: { index: delivery-incidents, time_field: created_at }
    query: { language: kql, text: "severity : (sev1 or sev2)" }
    end: resolved_at               # a range: shaded from created to resolved
  - id: freeze
    label: Release freeze
    events:
      - { at: "2026-07-01T00:00:00Z", to: "2026-07-08T00:00:00Z", text: Q3 freeze }
```

- **Two kinds.** An annotation either searches a `data_view` (optionally narrowed by `query`), or lists fixed `events` with ISO 8601 times.
- **Fields.** `time` is the field holding each event's time; it defaults to the data view's time field. `end` turns events into ranges. `limit` caps the events at the most recent ones (500 by default).
- **Filters.** The dashboard's query, filter pills and control selections apply to annotation searches too, so choosing a team shows that team's events. A control on a field the annotated documents don't have is skipped. Set `ignore_global_filters: true` to mark every event, which is what Kibana does by default.
- **Drawing.** Each event is a thin line with a tick at the foot of the chart, and ranges are shaded. `color` and `style` (`solid`, `dashed`, `dotted`) set how. The legend names each annotation; hovering near an event shows its date and `text`.
- **Searches.** Each annotation is one search, sent once per dashboard however many charts show it. **Inspect** lists it with the panel's other searches.
- **Kibana.** Lens annotation layers saved with the visualization convert, both query and manual annotations. Annotation groups linked from Kibana's library are reported as a warning.

### Document layers

```yaml
documents: { limit: 3000, sort: merged_at }   # newest 3,000, drawn oldest first
columns:
  - { id: merged, op: value, field: merged_at }
  - { id: hours, op: value, field: cycle_time_hours }
  - { id: trend, op: rolling_median, of: hours, window: 50 }
```

A document layer draws one row per document. Only `value` columns and
computed columns are allowed in it.

### ES|QL layers

A layer with an `esql` query runs it with `POST /_query` instead of
building a search. Each result row is a row of the layer, and its columns
are `value` columns that name result columns:

```yaml
- id: ai
  data_view: { index: delivery-prs, time_field: merged_at }
  esql:
    query: |
      FROM delivery-prs
      | STATS ai = MEDIAN(CASE(ai_assisted, cycle_time_hours)),
              other = MEDIAN(CASE(NOT ai_assisted, cycle_time_hours)) BY team
      | EVAL change = ai / other - 1
  columns:
    - { id: team, op: value, field: team }
    - { id: change, label: Change, op: value, field: change, format: { kind: percent } }
  encoding: { as: table, columns: [team, change] }
```

- **Filters.** As in Kibana, the dashboard's time range (on the data view's `time_field`), its query, the filter pills and the control selections narrow the documents the query reads. They are sent as `_query`'s `filter`, so the query itself needs no `WHERE` for them.
- **Time range.** `?_tstart` and `?_tend` in the query are the range's start and end. Kibana's ES|QL control variables (`?team`, set from a dropdown) are not supported: lint reports them, and Kibana panels that use them become placeholders.
- **Columns.** Aggregate in the query, not with column operations. Computed columns (formulas, moving averages and the rest) work over the rows.
- **Types.** Date columns (for example from `BUCKET(merged_at, 1 week)`) are drawn on a time axis; numbers on a numeric axis.
- **Kibana.** Lens ES|QL panels (`textBased`) convert, with their data view's time field and column formats.

### Small samples and drill-down

With `min_samples` set:

- **Metric tiles** show their sample size (n), and dim the value when it is below the minimum.
- **Charts** shade the buckets that fall below it.
- **Tables** dim those rows.

Clicking a value of a field that has a control (a table row, a bar, a pie
slice) selects it in that control. This is how drill-down works: from org to
team to repo, using `group`, `team` and `repo` controls.

### Metric tiles: trend, progress and badge

A metric tile can show three more things, as Kibana 9.x's metric chart does:

```yaml
layers:
  - id: lead
    columns:
      - { id: median, op: median, field: change_lead_time_hours }
      - { id: median-prev, op: median, field: change_lead_time_hours, time_shift: previous }
      - { id: target, op: static_value, value: 24 }
    encoding:
      as: metric
      value: median
      secondary: median-prev
      max: target
      progress: true              # a bar of the value against max
      badge: { better: down }     # the secondary as a trend badge
  - id: weekly                    # the background trend line
    columns:
      - { id: week, op: date_histogram, field: deployed_at, interval: 1w }
      - { id: median, op: median, field: change_lead_time_hours }
    encoding: { as: sparkline, x: week, y: median }
```

- **Background trend:** a `sparkline` layer in the same chart, drawn behind the tile. With a `breakdown`, give the sparkline the same column as `by` and each tile gets its own line.
- **Progress bar:** `progress: true` draws the value as a share of `max`.
- **Trend badge.** The badge compares the secondary value with the primary (or with a fixed `baseline`).
  - **Against the primary:** it shows the difference, primary − secondary, signed. This suits "this period against the last".
  - **Against a baseline:** it shows the secondary value itself.
  - **Arrow and colour:** an arrow gives the direction. `better` says which direction is good: `up`, `down` (lead time, failure rates) or `neither`. Good is green and bad is red.
  - **Contents:** `show` chooses `icon`, `value` or `both`.
  - Without a badge, a time-shifted secondary shows its percentage change.
- **Kibana:** Lens trend lines (`trendlineLayerId`), `showBar` and 9.x's `secondaryTrend` convert. The `compare_to` palette means up is good; `temperature` means up is bad; `reversed` swaps them.

### Tables: colour by value, summary row and sparklines

```yaml
encoding:
  as: table
  columns: [team, n, lead]
  styles:
    - { column: n, summary: sum, summary_label: Total }
    - column: lead
      summary: avg
      color:
        mode: cell                # or text, or badge
        stops:                    # each colour from its value up to the next
          - { from: 0, color: "#73BF69" }
          - { from: 24, color: "#5794F2" }
          - { from: 168, color: "#FF9830" }
  sparkline: { layer: weekly, label: Deployments per week }
```

- **Colour by value:** `percent: true` makes the stops percentages of the column's range. `extend_below` colours values under the first stop, and `extend_above: false` with `to` stops colouring past an end.
- **Summary row:** `sum`, `avg`, `count`, `min` or `max` of the column's non-empty values, as Kibana computes it (a count is left unformatted).
- **Sparkline column:** a last column of small lines, from a `sparkline` layer whose `by` matches the table's first column (for example each team's deployments per week). Kibana's datatable has no sparklines; Grafana's has.
- **Kibana:** Lens datatable columns' `colorMode` with a custom palette, and `summaryRow` with `summaryLabel`, convert.

### Lines, gaps, legend statistics and small multiples

```yaml
encoding:
  as: xy
  series: line
  x: week
  y: [median]
  interpolation: smooth           # linear (default), smooth or step
  missing: gap                    # connect (default), gap, zero, carry, lookahead
  legend_stats: [average, last]   # beside each series in the legend
  facet: team                     # one small chart per team, same y scale
```

- **`missing`** decides what an empty bucket does to a line or area: draw across it, leave a gap, or fill it with zero, the previous value (`carry`) or the next (`lookahead`). Bars are never filled. Lens's `fittingFunction` converts, and a Lens chart without one leaves gaps, as Kibana does.
- **`legend_stats`**: `average`, `median`, `min`, `max`, `range`, `first`, `last`, `total`, `count`, `difference`, `difference_percent` or `std_deviation`, over the series' non-empty values. They are added to the series' legend label, so they also appear in PNG exports. Lens 9.4+ `legendStats` convert.
- **`facet`** names a terms column. The panel draws one chart per value, in a grid, all on the same y range, so the charts compare at a glance. With one series, each chart's title carries its legend statistics. Dashboard annotations aren't drawn on facets, because they aren't split by facet.

### Gauges

```yaml
kind: gauge
encoding:
  as: gauge
  value: rate
  max: max          # a column, e.g. a static value; 0 and a round number above the value otherwise
  goal: goal        # marked on the scale
  shape: arc
  bands:            # coloured bands along the scale; the value takes its band's colour
    stops:
      - { from: 0, color: "#73BF69" }
      - { from: 0.05, color: "#5794F2" }
      - { from: 0.15, color: "#F2495C" }
```

The bands use the same stops as a table's colour by value (`percent`,
`extend_below`, `extend_above`, `to`). Lens gauges (`lnsGauge`, every shape)
convert with their palette.

### Treemaps, waffles and calendars

- A **treemap** sizes rectangles by value: the first group, then the second inside it. A **waffle** shows shares as 100 squares. Both use the `pie` encoding, and Lens's treemap and waffle shapes convert. Mosaic stays a placeholder.
- A **calendar** shows one square per day, darker for more, in weeks from Monday, in the dashboard's time zone. Use a `date_histogram` with `interval: 1d`. It suits deployment cadence: weekends, freezes and busy weeks show at a glance.

### Chained controls

Controls narrow the ones after them, as Kibana's do by default: with
`group`, `team` and `repo` in that order, the team control offers only the
chosen group's teams, and the repo control only the chosen teams' repos.
Changing a control clears the ones after it, because their choices were
made under the old selection. Set `chain_controls: false` on the dashboard
to make every control independent (Kibana's `chainingSystem: NONE`).

### Ad-hoc filters

**+ Filter** in the toolbar adds a pill: a field, one or more values, and
"is" or "is not". The values offered are the field's most common ones under
the other filters. A pill narrows only the panels whose data has its field,
so a filter on `ai_assisted` narrows the PR panels and leaves the deployment
panels alone. Click a pill to invert it, or × to remove it. Pills are kept
in the URL (`f=`), so a filtered view can be shared. Fields about people
(author, reviewer, assignee and the like) are not offered: Teleplot has no
per-engineer views.

### Compare a selection (BubbleUp)

Charts with a time x axis have a **Compare** button. With it on, a drag
selects a box instead of zooming: a time range, and on a scatter or a
heatmap of log-histogram rows, a range of values too. Teleplot then
compares the documents in the box with all of the panel's documents (under
the same filters):

- **What sets them apart:** each keyword-like field, ranked by how differently its values are spread, half the summed differences in share (0% the same, 100% nothing in common). For example, "data: 54% of the selection, 13% overall".
- **Medians:** each numeric field's median in the selection and overall.
- **Left out:** fields about people (never ranked), and fields with more than 30 values (IDs).

On the seeded data, a box around the slow PRs on the cycle-time scatter
shows that they come mostly from the data and experiments teams, and that
their pickup, coding and review times are several times the median,
while their size is not different. With fewer than 10 documents selected
(or the dashboard's `min_samples`), the panel warns that differences may be
chance. Compare does not work on ES|QL panels.
