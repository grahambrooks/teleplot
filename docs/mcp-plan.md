# Plan: MCP endpoint for agent-authored dashboards, and the feature-gap roadmap

Status: decided and in progress, 2026-10-03. The research behind this plan was carried out
the same day; sources are linked inline. Gaps 2 (inspector and CSV), 3 (annotations), 4 (ES|QL panels; not variable controls), 5 (chained and ad-hoc variables) and 6 (BubbleUp) are done. The remaining gaps are ordered,
alongside the charting work, in [`roadmap.md`](roadmap.md).

## Context

Teleplot's dashboards are YAML files (`docs/dashboards.md`) that are
reloaded when they change. We want an AI agent to be able to define
dashboards and visualizations and have Teleplot save them, through an MCP
endpoint. Alongside that, this plan records which features competing tools
have that technical users would value, and orders them.

### The gate: why MCP and not just a file

The vault records a past lesson. On tropism, an MCP server was billed as the
headline feature until it turned out agents could do the same job by reading
a file: *"an agent doesn't need a protocol to read it, it needs `Read`."*
Teleplot dashboards are files too, so an agent can already write YAML.

The MCP endpoint is only worth building if it gives the agent what a file
cannot:

1. **Discovery.** Which indexes exist, which fields they have, the type of each field, and typical values. A file contains none of this.
2. **Validation against the real data.** A field that doesn't exist, a date histogram on a keyword field, or a formula referring to a missing column are all syntactically valid YAML. Only the cluster can say they are wrong.
3. **Previewing numbers before saving.** The agent sees "change fail rate by team: checkout 4.3%, data 31.6%…" and can check that this is what the user asked for before anything is written.
4. **A safe save.** Atomic, versioned, and checked again before writing, so the change is never a half-written file.

**Gate check.** The best question for the endpoint is "build me a dashboard
of change fail rate by team for last quarter, with the DORA benchmark bands."
It is answerable: `describe_index` finds `result` and `team`,
`preview_panel` shows the per-team numbers, and `save_dashboard` writes the
file, which then renders. This plan's done-when puts that question to a real
agent.

## Part 1: MCP endpoint

### Transport and protocol

- **The official Rust SDK, `rmcp` 3.5** (decided 2026-10-03). The protocol is still evolving, and `rmcp` evolves with it: its 3.0 release shipped the same day as the 2026-07-28 spec. Hand-rolling would mean tracking every spec change ourselves.
  - `rmcp` serves both spec generations. It answers 2026-07-28 clients statelessly (`server/discover`, per-request `_meta`), and pre-2026 clients through `initialize`, with sessions.
  - It checks the `Host` header against loopback names by default, and checks `Origin` against our own address when configured.
- **Streamable HTTP at `/mcp`** on the existing axum server, mounted with `nest_service`, with plain JSON responses.
- **`teleplot mcp` (stdio)** shares the same dispatcher, for clients that prefer to launch a process. Logs go to stderr.
- **Security:**
  - `Host` and `Origin` validation (through `rmcp`) blocks DNS-rebinding attacks.
  - **Writes are enabled only while Teleplot is bound to a loopback address.** On any other address the endpoint is read-only (decided 2026-10-03).
  - No OAuth or tokens while Teleplot is localhost-only; the spec makes authorization optional.

### Tools

There are seven tools, all prefixed `teleplot_`. Each declares an
`outputSchema` and returns `structuredContent` together with a short text
summary. Outputs are compact by default (about 25k tokens maximum, with
pagination), following Anthropic's
[guidance on writing tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents).
The tool count stays small on purpose: community Metabase servers expose more
than 100 tools.

| Tool | Hints | What it does | Reuses |
|---|---|---|---|
| `list_indices` | read-only | Indexes and data streams, with document counts and time fields | `es` client |
| `describe_index` | read-only | Fields and their types; which fields are aggregatable and which are dates; top 10 values of keyword fields; one sample document | `field_caps`, `query::terms_search` |
| `list_dashboards` | read-only | Id, title, panel count, source (YAML, Kibana export, or agent-written) and file | `Catalog` |
| `get_dashboard` | read-only | A summary by default (panels: title, type, metrics, index); `format: yaml` for the full file; a content `hash` for safe overwrites | `Catalog`, `serde_yaml_ng` |
| `validate_dashboard` | read-only, idempotent | Parses YAML or JSON into the model, then runs semantic checks against the live field caps. Returns every problem with its path (`panels[3].content.layers[0].columns[2]`) and a suggested fix | **new `src/lint`**, shared with `teleplot lint` (Part 2) |
| `preview_panel` | read-only | Runs one panel spec, or a whole dashboard, through the engine over a time range. Returns at most 50 rows per layer, column summaries (min/max/last, n), warnings, and a URL that opens it in Teleplot | `Engine::dashboard_data` |
| `save_dashboard` | idempotent; destructive when overwriting | Validates again, refuses on errors, writes `dashboards/<id>.yaml` atomically (temporary file + rename), and keeps the previous version. Returns the URL and the new hash | new `src/store` |

There are two resources:
- `teleplot://schema/dashboard`, the JSON Schema of the YAML format;
- `teleplot://docs/dashboards`, the contents of `docs/dashboards.md`, so agents learn the format from its own documentation.

The JSON Schema is generated from `src/model` with `schemars` and becomes each
tool's `inputSchema`. So the model, the agent and editor tooling (Part 2's
`lint`) all describe dashboards the same way.

There is no delete tool in v1. A person removes files; an agent can only add
or replace them.

### Saving: rules that keep agent writes safe

- **Location.** Agent dashboards live in their own folder, `dashboards/agents/<id>.yaml` (decided 2026-10-03). The catalog loads that folder too, and marks each dashboard as hand-written, Kibana export, or agent.
- **Ids** must match `^[a-z0-9][a-z0-9-]{0,63}$`. No paths are accepted from the agent, so there is no path traversal. An id already used by a dashboard outside `agents/` is refused.
- **Hand-maintained and Kibana dashboards are never overwritten.** Files Teleplot did not write, including every `.ndjson` export, are read-only to MCP. Saving a changed copy of one creates a new id: `<id>-copy` or an id the agent chooses.
- **Overwrites need proof of what was read.** Overwriting a dashboard the agent wrote earlier requires the `expected_hash` from `get_dashboard`. If the file changed since the agent read it, the save is refused with a message to re-read and merge.
- **History.** The previous version moves to `dashboards/agents/.history/<id>/<timestamp>.yaml`, keeping the last 20, so undo is "copy it back". A `.history/` folder is enough version history (decided 2026-10-03).
- **Size limits:** at most 200 panels and 1 MB of YAML.


### Code layout

Teleplot stays a single crate with directory modules (vault convention).

```
src/mcp.rs      # rmcp server: the seven tools and two resources; mounted at /mcp and run by `teleplot mcp`
src/lint/       # parse with error paths, then semantic checks (also `teleplot lint`)
src/store.rs    # atomic save into dashboards/agents, history, hash checks
```

### Testing

1. **Protocol tests** over HTTP against a running server, for both spec generations: a 2026-07-28 stateless `tools/call`, and a legacy `initialize` followed by `tools/list`. These check the tools as clients see them, including schemas and annotations.
2. **Lint tests.** Seeded mistakes must each be reported with the right path:
   - a missing field;
   - a date histogram on a keyword field;
   - a formula referring to an absent column;
   - an encoding naming an unknown column;
   - a bad interval;
   - an unknown index.
   All fixture dashboards must lint clean.
3. **Store tests:**
   - the write is atomic;
   - a stale `expected_hash` is refused;
   - history is kept;
   - Kibana and hand-maintained files are untouched;
   - malicious ids are rejected.
4. **Integration** against ES 8.19 and 9.5, in the existing CI job: `preview_panel` returns the same numbers as the parity tests.
5. **End to end with a real agent:**
   - Add the server with `claude mcp add --transport http teleplot http://127.0.0.1:3000/mcp`.
   - Prompt the agent to build the gate-check dashboard.
   - Check the saved dashboard in Chrome and its numbers against the integration tests.

### Done when

- CI passes, including both protocol generations and lint, store and integration tests.
- Claude Code, connected over HTTP and over stdio, builds and saves "change fail rate by team for last quarter with DORA benchmark bands" using only the MCP tools.
- The saved dashboard renders in the browser with the parity numbers.
- A deliberately broken request is caught by `validate_dashboard`, and the agent fixes it from the error message.

### Acceptance results (2026-10-03)

**Status: Part 1 is done.**

| Run | Request | Tools used | Result |
|---|---|---|---|
| HTTP, first attempt | Change fail rate by team for last quarter, with benchmark bands | All seven | Saved and correct, but `preview` and `describe_index` failed the client's output-schema check (fields omitted when empty were declared required). The agent fell back to the shell. A table ordered by a formula column also failed at query time |
| HTTP, after fixes (MCP tools only) | Same | list_indices, describe_index, get_dashboard, validate, preview ×2, save | Saved in 10 turns; per-team rates correct (checkout 3.1% … data 27.3%). One client error on `resources/read` (missing `ttlMs`), now fixed |
| stdio (MCP tools only) | Director dashboard: weekly deployments by team, lead-time median within p25–p75, team table | All seven | Saved in 11 turns, zero tool errors; band chart, team control and drill-down table; observations match the data |

Fixes that came out of these runs:
- **Output schemas are honest:** optional fields are no longer declared required, and every MCP test now validates results against the tool's declared output schema.
- **Ordering by a formula works:** terms ordered by a computed column fall back to document count, and lint warns about it.
- **Resources carry cache hints,** as 2026-07-28 clients require.
- **Categorical bars take benchmark band colours.**

## Part 1b: insight data and hand-written dashboards (2026-10-05)

The software-analytics package keeps its dashboards as hand-written YAML under version control,
and charts insight data rather than Elasticsearch. Two of Part 1's rules didn't fit: discovery
only knew Elasticsearch, and agents could never change a hand-written file.

- **Discovery.** With `insight: { url }` set, `teleplot_list_indices` also lists insight's record
  types (`source: insight`). `teleplot_describe_index` describes one from insight's series schema,
  with field types, units and the top values of keyword and boolean fields over a year. It
  makes one series request per such field.
- **Editing hand-written dashboards** is opt-in: `mcp: { edit_dashboards: true }` in
  `teleplot.yaml`, for a dashboards directory where every change is reviewed as a diff. Kibana
  exports stay read-only.
  - `teleplot_update_panel` replaces one panel, or adds one, by id.
  - Only the panel's changed fields are rewritten. Comments, layout and every other line stay
    byte for byte the same: a title change is a one-line diff.
  - The result is parsed again and must equal the intended dashboard, or nothing is written.
  - A panel that defines a YAML anchor is refused, since other panels may refer to it.
  - It needs the file's hash from `teleplot_get_dashboard`, validates the whole dashboard
    against live data, writes atomically, and returns the diff.
  - `teleplot_save_dashboard` then creates new dashboards in the dashboards directory, not
    `agents/`. It refuses to overwrite a hand-written one and points to `update_panel`.
  - `teleplot_list_dashboards` says which dashboards are editable.
- **Verified** on the package's seven dashboards (62 panels). Updating every panel with itself
  changed nothing, and a title change produced a one-line diff.

## Part 2: feature gaps for technical users

Teleplot already treats dashboards as code. The highest-value gaps are the
ones that make that trustworthy, and the ones that link delivery metrics to
cause.

| # | Gap | Who has it | Priority | Effort | Notes |
|---|---|---|---|---|---|
| 1 | **`teleplot lint` and the published JSON Schema**: offline and `--online` (field checks), usable in CI, with editor autocomplete through the YAML language server | Perses `percli lint` ([cli](https://github.com/perses/perses/blob/main/docs/tooling/cli.md)), Grafana schema v2 and the Foundation SDK ([docs](https://grafana.com/docs/grafana/latest/as-code/observability-as-code/foundation-sdk/)) | Must | Low | **Built as part of Part 1** (`src/lint`, `schemars`) |
| 2 | **Query inspector and CSV per panel**: generated DSL, response, `took`, hit count, download | Grafana panel inspector ([docs](https://grafana.com/docs/grafana/latest/panels-visualizations/panel-inspector/)), Kibana Inspect | Must | Low | Technical users don't trust a number they can't trace to its query. Shares code with `preview_panel` |
| 3 | **Event annotations**: deploy, incident and release markers from a second query, drawn on every time series | Grafana ES annotations ([docs](https://grafana.com/docs/grafana/latest/datasources/elasticsearch/annotations/)), Lens annotation layers, DevLake | Must | Low–Medium | The main way to connect cause and effect in DORA ("did this deploy move cycle time?"). One query per dashboard, drawn with the existing uPlot hook. Kibana annotation layers, skipped today, become importable |
| 4 | **ES\|QL panels and controls** | Kibana 9.x ([docs](https://www.elastic.co/docs/explore-analyze/query-filter/languages/esql-kibana)); Grafana 13, experimental ([blog](https://www.elastic.co/search-labs/blog/esql-grafana-elasticsearch-plugin)) | Must | Medium | `POST /_query` returns columns, which map straight onto `LayerData`. Kibana's ES\|QL panels, placeholders today, would render. An editor with autocomplete is out of scope |
| 5 | **Chained and ad-hoc variables**: team → repo → service, and free-form key=value filters | Grafana ([variables](https://grafana.com/docs/grafana/v12.4/dashboards/variables/add-template-variables/)), Kibana controls | Strong | Medium | Chaining already works in the server (control values honour the other selections); this adds UI ordering and ad-hoc filter pills |
| 6 | **Compare a selection with the baseline** (BubbleUp): drag-select slow PRs and rank the fields and values that differ most | Honeycomb BubbleUp ([docs](https://docs.honeycomb.io/investigate/analyze/identify-outliers/)) | Strong, and **distinctive** | Medium | No Elasticsearch dashboard tool has it. `significant_terms` / `significant_text` do the statistics, and the scatter's drag selection already exists. Answers "why is this team's cycle time long?" |
| 7 | **Digests and working-agreement alerts** posted to a Slack, Teams or generic webhook | Grafana alerting, Kibana rules, Swarmia digests ([docs](https://help.swarmia.com/notifications/team-notifications)), LinearB WorkerB | Strong, later | Medium | Conflicts with today's "localhost, view-only" scope, and alert fatigue is the top pain in Grafana's 2025 survey. If built, start with a weekly digest, not alerting |
| 8 | **Joins and transformations across queries**, e.g. deploys ⋈ incidents | Grafana "Join by field" ([docs](https://grafana.com/docs/learning-paths/data-transformation/join-by-field/)); ES\|QL `LOOKUP JOIN` | Nice | Medium | Elasticsearch can't join across indexes. Do this after ES\|QL, which may cover most of it |
| 9 | **Static snapshot**: self-contained HTML with the data embedded and the queries removed | Grafana snapshots, Evidence static builds | Nice | Medium | The safe way to share beyond PNG/PDF while there is no auth |
| 10 | **Metric library**: define "lead time" once and reference it from many panels | Lightdash metrics in dbt ([bi-as-code](https://www.lightdash.com/bi-as-code)), Grafana library panels | Nice | Low–Medium | Consistency across dashboards; small extension to the YAML format |

Not pursued, and why:
- **A full Explore/Discover with split view:** the inspector, raw scatter and ES|QL cover most of it.
- **Git Sync with "open a PR from the UI":** Teleplot is already file-native; hot reload and lint are enough.
- **Notebooks and Canvas:** high cost, and they don't fit a view-only single binary.
- **OpenSearch PPL:** only if OpenSearch support becomes a goal, and it is outside the MVP scope.

## Recommended order

1. **MCP endpoint and lint/schema (Part 1 and gap 1).** They share `src/lint` and the schema, and lint makes the agent's saves safe.
2. **Query inspector and CSV (gap 2).** Low effort, and builds trust.
3. **Annotations (gap 3).** This is the delivery-analytics payoff. Done 2026-10-03.
4. **ES|QL (gap 4).** It also brings more Kibana dashboards into scope. Panels done 2026-10-03; variable controls not.
5. **Baseline comparison (gap 6),** the feature that sets Teleplot apart, then chained/ad-hoc variables (gap 5). Both done 2026-10-03.
6. **Revisit 7–10** once there is a decision about sharing and authentication.

## Decisions (2026-10-03)

1. Agent writes are allowed only while Teleplot is bound to localhost.
2. `.history/` is enough version history.
3. Agent dashboards go in `dashboards/agents/`.
4. Use `rmcp` rather than a hand-written protocol layer, because the protocol is still evolving.
