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
panels:
  - id: lead-time
    title: Change lead time
    grid: { x: 0, y: 0, w: 12, h: 7 }   # Kibana's 48-column grid, 20px rows
    content: { type: chart, kind: metric, layers: [...] }
```

Panel `content` is one of:

- `type: chart`, which has a `kind` (`xy`, `metric`, `pie`, `donut` or `table`), an optional `query` and `filters`, and `layers`.
- `type: markdown` with `text`.

## Layers and columns

A layer queries one data view. Its columns are either:

- **Buckets:** `date_histogram`, `terms`, `filters`, `histogram`, `ranges`;
- **Metrics:** `count`, `sum`, `average`, `min`, `max`, `median`, `percentile`, `unique_count`, `last_value`, `standard_deviation`, `static_value`, `time_range`;
- **Computed from other columns:** `formula`, `moving_average`, `cumulative_sum`, `differences`, `counter_rate`, `overall`, `rolling_median`.

Buckets nest in the order listed. Any column can also have:

| Key | Meaning |
|---|---|
| `label` | Shown in legends, tables and tiles |
| `format` | `{ kind: number \| percent \| bytes, decimals, suffix, compact }` |
| `filter` | `{ language: kql, text: ... }`, restricting just this column |
| `time_shift` | `previous` (the range before this one) or a duration such as `1w` |
| `helper: true` | An input to another column, not drawn |
| `thresholds` | Benchmark bands, below |

The `encoding` (`as:`) says how columns are drawn:

| `as` | Fields |
|---|---|
| `xy` | `series` (line, area, bar, …, `bar_percentage_stacked`), `x`, `y: [...]`, `split` |
| `metric` | `value`, `secondary`, `breakdown` |
| `pie` | `groups`, `values` |
| `table` | `columns` |
| `band` | `x`, `low`, `mid`, `high`: a line inside a shaded band, e.g. median within p25–p75 |
| `scatter` | `x`, `y`, `split`, `trend`, `log_scale`: needs a document layer |

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

### Small samples and drill-down

With `min_samples` set:

- **Metric tiles** show their sample size (n), and dim the value when it is below the minimum.
- **Charts** shade the buckets that fall below it.
- **Tables** dim those rows.

Clicking a value of a field that has a control (a table row, a bar, a pie
slice) selects it in that control. This is how drill-down works: from org to
team to repo, using `group`, `team` and `repo` controls.
