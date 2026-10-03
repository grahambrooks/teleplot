# Teleplot over MCP

Teleplot's MCP server gives an agent seven tools: explore indexes, read
dashboards, validate and preview a draft, and save it. Nothing in it writes to
Elasticsearch.

## Connect

**stdio** (the agent launches Teleplot; nothing else needs to run):

```sh
claude mcp add teleplot -- teleplot mcp --config /path/to/teleplot.yaml --dashboards /path/to/dashboards
```

`--es http://localhost:9200` replaces `--config` for an unsecured local
cluster. `--url` sets the UI address used in returned links (default
`http://127.0.0.1:3000`), so point it at wherever `teleplot serve` runs.

**HTTP** (Teleplot is already serving): `teleplot serve` exposes the same tools
at `http://127.0.0.1:3000/mcp` (Streamable HTTP).

```sh
claude mcp add --transport http teleplot http://127.0.0.1:3000/mcp
```

Two resources carry the format: `teleplot://docs/dashboards` (the guide) and
`teleplot://schema/dashboard` (JSON Schema). Read the guide once before writing
a dashboard from scratch.

## The flow

1. **`teleplot_list_indices`**: which indexes exist and how many documents
   each holds.
2. **`teleplot_describe_index`**: fields and types, which are dates, which
   can be grouped by, top values of keyword fields, and a sample document. Do
   this for every index you plan to chart. Guessing field names is the
   commonest cause of empty panels.
3. **`teleplot_list_dashboards`** / **`teleplot_get_dashboard`**: what
   already exists. Extend an existing dashboard instead of duplicating it.
   `format: full` returns the whole dashboard plus its `hash`.
4. **`teleplot_validate_dashboard`**: format, column references, KQL, and
   field existence and types against the cluster. Every problem has a path and
   says what to fix. Iterate until it is clean.
5. **`teleplot_preview`**: run the draft (or one `panel`) without saving.
   You get the first rows of each layer and min/max/last per numeric column.
   Read the numbers and ask whether they answer the question: plausible
   magnitudes, the expected number of buckets, no surprising nulls. Pass
   `inspect: true` to see the exact Elasticsearch searches when a number needs
   explaining. `from`/`to` take Kibana date math (`now-90d`).
6. **`teleplot_save_dashboard`**: validates again and refuses on errors.
   The result includes the dashboard's URL. Give it to the user.

## Saving rules (enforced, not advisory)

- Saves go only to `<dashboards>/agents/<id>.yaml`. Ids are slugs, not paths.
- Saving works only while Teleplot runs on localhost (loopback or stdio).
- Dashboards written by people or exported from Kibana can't be replaced.
  Save a copy under a new id and tell the user.
- Replacing an agent dashboard needs `expected_hash` from
  `teleplot_get_dashboard`. A mismatch means someone changed it since you
  read it: read it again, merge, then save. Don't drop their change.
- The replaced version is kept in `agents/.history/<id>/` (the last 20).

## Habits that make agent dashboards trustworthy

- Preview every panel you add, every time. A dashboard that validates can
  still answer the wrong question.
- Say which numbers you checked and how (see `verification.md`).
- List any panel you couldn't build, and why. Don't leave it out quietly.
- Group by team, service or repo, never by person. The compare and filter
  features refuse person fields; don't route around that with raw terms
  panels.
