# Verifying a dashboard

A dashboard that renders isn't necessarily right. Before calling one done,
take it through these four steps in order. Each step catches mistakes the
earlier ones can't.

## 1. Lint against the cluster

```sh
teleplot lint dashboards/my.yaml --online --config teleplot.yaml
```

This checks the format, references between columns, KQL syntax, and that each
index and field exists with a usable type. Exit code 1 means errors; CI can run
the same command. Common fixes:

- **No such field.** It's probably `text`; group by `field.keyword`.
- **Wrong type.** Durations stored as strings can't be averaged. Report this
  rather than working around it.
- **Unknown column.** A formula or `ref` points at an id that doesn't exist.

## 2. Look at every panel's numbers

```sh
teleplot query dashboards/my.yaml --config teleplot.yaml --from now-90d --to now
```

This prints each panel's table. Read it with the question in mind:

- **Magnitude.** Ten deployments a day for a team of six is plausible;
  10,000 isn't. Lead times should be in hours or days, not seconds, unless
  the field is in seconds. Check the unit.
- **Bucket count.** A weekly chart over 90 days has 13–14 rows. Far more
  means the interval is wrong. Far fewer means the data is sparse or the time
  field is wrong.
- **Nulls and zeros.** An empty bucket is a fact about the data; a column
  that's null everywhere is a mistake.
- **Shares add up.** Pie, treemap and waffle shares should sum to the total.
  "Other" should be small; if it isn't, raise the `size`.
- **Small samples.** Medians over three items mean little. Set `min_samples`
  and check that the flagged buckets are the ones you'd expect.

## 3. Reproduce one headline number independently

Pick the number people will quote, such as deployment frequency or median
lead time, and compute it a different way: a direct Elasticsearch request
with the same filter and time range.

```sh
curl -s "$ES/deployments/_count" -H 'content-type: application/json' -d '{
  "query": {"bool": {"filter": [
    {"range": {"deployed_at": {"gte": "now-90d", "lt": "now"}}},
    {"term": {"environment": "production"}}
  ]}}}'
```

For a median, use a `percentiles` aggregation with `"percents": [50]`. If
the numbers differ, find out why before going on. Usually it's one of these:

| Difference | Cause |
|---|---|
| Off at the edges of each day or week | Time zone. Teleplot buckets in `time_zone` from `teleplot.yaml` (or the dashboard). Pass the same `time_zone` to the aggregation |
| Last bucket is low | It's partial: the range ends mid-week. That's correct, not a bug |
| Percentiles differ slightly | Elasticsearch percentiles are approximate (TDigest). Teleplot and Kibana agree with each other, not necessarily with an exact calculation |
| Rate is off by a constant factor | `per` divides by the bucket length. Partial buckets at the edges count only the part inside the range |
| "Previous period" differs | `time_shift: previous` uses the range immediately before, of the same length |
| Every value is a little high | A filter or the dashboard query is missing from your direct request |

In the UI, **Inspect** on any panel shows the exact searches Teleplot sent and
the responses. Over MCP, `teleplot_preview` with `inspect: true` returns the
same information. Comparing that DSL with your own request settles most
disagreements.

## 4. Look at it

Open it with `teleplot serve`. Check that the titles say what's measured and
over what period, that units are shown, that the colours mean what the legend
says, and that nothing has been replaced by a placeholder or a warning badge
without explanation. Check dark mode too.

## Converted Kibana dashboards

The aim is parity, not a resemblance. When the user has Kibana:

1. Open the same dashboard in Kibana and in Teleplot with the same time range
   and time zone.
2. Compare the headline tiles and a few points on each chart.
3. Run `teleplot check` on the export and report every placeholder and its
   reason.

Known differences, which are fine to mention but aren't bugs: Teleplot draws
with uPlot rather than Elastic Charts, so styling differs. Out-of-scope panel
types (TSVB, Vega, Maps, saved searches) show as placeholders.

## Report what you verified

When handing a dashboard over, say what you checked:

> "Lint is clean against the cluster. Deploy frequency (4.2/day over 90 days)
> matches a direct `_count`. Lead-time median is within 1% of a direct
> percentiles aggregation, which is expected since both are approximate. Two
> panels from the Kibana export are placeholders: a Vega chart and a saved
> search."

Statements like that are what let the people reading the dashboard trust it.
