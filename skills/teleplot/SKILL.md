---
name: teleplot
description: Build, check and serve dashboards over Elasticsearch with Teleplot — a single-binary, read-only dashboard tool that renders Kibana exports and its own YAML dashboards. Use this skill whenever someone wants to see their Elasticsearch data as dashboards outside Kibana, move or check Kibana dashboards (NDJSON exports), write dashboards as code (YAML in a repo, linted in CI), build delivery or DORA analytics (deployment frequency, lead time, change fail rate, recovery, cycle time), or give an agent dashboard tools over MCP — even if they don't name Teleplot, as long as the data lives in Elasticsearch.
---

# Teleplot

Teleplot serves Grafana-styled dashboards over Elasticsearch from one binary.
It reads dashboards from files — Kibana saved-object exports (`.ndjson`) and
its own YAML — and never writes to Elasticsearch. Everything it shows can be
traced to the query behind it.

Use it on a real project in this order: **get the binary → connect → look at
the data → get dashboards (convert or write) → verify the numbers → serve.**
Don't skip verification: a dashboard that renders is not the same as one that
is right, and the people reading delivery dashboards make decisions on them.

## 1. Get the binary

Check first: `teleplot --version`. If it isn't installed and you have the
Teleplot source, build the release binary (it embeds the UI and runs from any
directory):

```sh
make release            # in the Teleplot repo; needs Rust and cargo-leptos
cp target/release/teleplot ~/.local/bin/   # or anywhere on PATH
```

Debug builds served with plain `cargo run` show a blank page (a hydration
mismatch). Use the release binary, or `make dev` when working on Teleplot itself.

## 2. Connect to Elasticsearch

For a quick look at an unsecured local cluster, pass `--es http://host:9200`
to any command. For a real cluster, write `teleplot.yaml` next to where you run
it, keep secrets in environment variables, and keep the file out of git:

```yaml
elasticsearch:
  url: https://es.example.com:9200      # or cloud_id: <Elastic Cloud id>
  api_key: ${ES_API_KEY}                # or username/password: ${ES_USER} / ${ES_PASSWORD}
  ca_file: /etc/ssl/corp-ca.pem         # only if a corporate CA signs the cluster
dashboards: ./dashboards                # .ndjson exports and .yaml dashboards
time_zone: Europe/London                # calendar buckets and "today" follow it
```

Ask the user for the URL and how they authenticate; never print or commit a
key. Teleplot only reads: it needs `read` and `view_index_metadata` on the
indexes (plus `monitor` for listing indexes over MCP), nothing more. Confirm the connection with
`teleplot lint --online <a dashboard>` (it checks indexes and fields against
the cluster) or, over MCP, `teleplot_list_indices`.

## 3. Look at the data before building anything

Find the indexes, their time field, the fields you can group by and the
numeric fields you can measure. Over MCP use `teleplot_list_indices` then
`teleplot_describe_index` (fields, types, top values, a sample document). From
a shell, read-only requests are fine:

```sh
curl -s "$ES/_cat/indices?v&s=index"                      # what exists
curl -s "$ES/<index>/_field_caps?fields=*" | jq '.fields | keys'
```

Note which field is the event time (it becomes the data view's `time_field`),
which keyword fields identify teams, services or repos (controls and
breakdowns), and which numbers are durations (lead time, cycle time —
long-tailed, so use medians and percentiles, not averages).

## 4. Get dashboards

**From Kibana.** Export in Kibana (Stack Management → Saved Objects → Export,
include related objects), then:

```sh
teleplot check export.ndjson          # which panels render; exits 1 if a supported panel fails
cp export.ndjson dashboards/          # served as-is, reloaded on change
teleplot convert export.ndjson --out dashboards/   # optional: YAML to edit and review
```

`check` names every panel that becomes a placeholder and why: out of scope
(Vega, TSVB, Maps, saved searches, ES|QL control variables), a broken
reference, or not understood. Report those to the user rather than hiding
them. Convert to YAML when the dashboards will be maintained as code; keep the
NDJSON when Kibana stays the source.

**As YAML.** Write dashboards in `dashboards/*.yaml`. Get the exact format
from `teleplot schema` (JSON Schema; point the YAML language server at it for
autocomplete) and the guide at `docs/dashboards.md` in the Teleplot repo.
Start from a recipe in `references/cookbook.md` — DORA tiles, distributions
with service-level expectations, heatmaps, annotations, ES|QL, tables, small
multiples — and adapt field names to the cluster you looked at in step 3.

Keep panels honest: medians and percentiles for durations, `min_samples` so
small buckets are flagged, a time range wide enough to mean something.

## 5. Verify the numbers

Do this for every dashboard you write or convert, before telling anyone it is
done. Details and checks per chart type are in `references/verification.md`.

```sh
teleplot lint dashboards/my.yaml --online            # format, references, KQL, fields — exits 1 on errors
teleplot query dashboards/my.yaml --from now-90d --to now   # every panel's table
```

Then pick at least one headline number per dashboard and reproduce it
independently with a direct Elasticsearch query (a `_count` or an aggregation
with the same filters and time range). If they differ, find out why before
going on — usually the time range, the time zone, a filter, or approximate
percentiles. In the UI, every panel's **Inspect** shows the exact searches and
responses.

## 6. Serve and share

```sh
teleplot serve --config teleplot.yaml        # http://127.0.0.1:3000
```

Teleplot listens on localhost and has no login: put it behind the
organisation's usual access controls before anyone else can reach it, and
don't expose it to the internet. Views are shareable as URLs (time range,
query, control selections and filters are all in the URL). Panels export as
PNG and CSV; the dashboard prints to PDF.

## Agents: the MCP tools

`teleplot serve` exposes MCP at `/mcp`; `teleplot mcp` serves it over stdio.
The tools let an agent explore indexes, validate and preview a dashboard, and
save it — into `dashboards/agents/` only, only on localhost, and replacing an
existing agent dashboard requires its hash. See `references/mcp.md` for setup
and the tool-by-tool flow. Preview before saving, every time: it is the cheap
way to see the numbers answer the question.

## Rules that are not negotiable

- **Read-only.** Never write, delete or reindex in Elasticsearch for a
  dashboard. If data is missing, say so; don't create it.
- **No per-engineer views.** Don't build panels that rank, compare or plot
  individual people (author, reviewer, assignee). Group by team, repo or
  service. Teleplot's compare and filter features refuse person fields for the
  same reason; don't work around them.
- **Say what doesn't render.** Placeholders and panel warnings exist so nothing
  is dropped silently; pass them on.

## When something is wrong

| Symptom | Likely cause |
|---|---|
| Blank page in the browser | A debug server from plain `cargo run`; use the release binary |
| "Queries cannot run" | No `teleplot.yaml` where you ran it, or `elasticsearch.url` missing |
| Every panel "No results" | Time range outside the data — try `--from now-1y`; or the wrong `time_field` |
| A number differs from Kibana | Time zone (`time_zone`), `previous` time shifts, or percentiles (both approximate) — see `references/verification.md` |
| `lint --online`: no field | The field is `text`, not `keyword` — group by `field.keyword` |
| TLS errors | Set `ca_file` to the corporate CA; `insecure: true` only for throwaway local clusters |
