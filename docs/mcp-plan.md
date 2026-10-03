# Plan: MCP endpoint for agent-authored dashboards, and the feature-gap roadmap

Status: proposed, 2026-10-03. The research behind this plan was carried out
the same day; sources are linked inline.

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

- **Hand-rolled, not `rmcp`.** This follows the precedent in the vault: kaizen-ai's server is about 160 lines of serde JSON-RPC around a pure `handle_message` function. `rmcp` went through three major versions between March and July 2026 (1.0 → 2.0 → 3.0, [crates.io](https://crates.io/api/v1/crates/rmcp/versions)), which confirms the worry about churn.
- **Streamable HTTP at `POST /mcp`** on the existing axum server.
  - Responses are plain JSON: no server-sent events and no sessions.
  - Notifications get 202; GET and DELETE get 405.
- **Both spec generations are supported.** Clients will speak both for a while. The research reports that 2026-07-28 removed sessions and the `initialize` handshake.
  - **2026-07-28:** `server/discover`, per-request `_meta`, the `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers checked against the body, `resultType` on every result, and `ttlMs` on list results ([changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog), [transport](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)).
  - **2025-06-18 and 2025-11-25:** `initialize`, `notifications/initialized` and `ping` ([2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)).
  - **Verification:** these spec details came from research and must be checked against the spec text before implementation. The tests below pin both flows.
- **`teleplot mcp` (stdio)** shares the same dispatcher, for clients that prefer to launch a process. Logs go to stderr.
- **Security:**
  - Validate the `Origin` header; return 403 for a foreign origin (this blocks DNS-rebinding attacks).
  - Keep the existing default of binding to 127.0.0.1.
  - An optional static bearer token: `mcp.token: ${TELEPLOT_MCP_TOKEN}`.
  - No OAuth while Teleplot is localhost-only; the spec makes authorization optional ([auth](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)).

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

- **Ids** must match `^[a-z0-9][a-z0-9-]{0,63}$` and map to exactly one file, `dashboards/<id>.yaml`. No paths are accepted from the agent, so there is no path traversal.
- **Hand-maintained and Kibana dashboards are never overwritten.** Files Teleplot did not write, including every `.ndjson` export, are read-only to MCP. Saving a changed copy of one creates a new id: `<id>-copy` or an id the agent chooses.
- **Overwrites need proof of what was read.** Overwriting a dashboard the agent wrote earlier requires the `expected_hash` from `get_dashboard`. If the file changed since the agent read it, the save is refused with a message to re-read and merge.
- **History.** The previous version moves to `dashboards/.history/<id>/<timestamp>.yaml`, keeping the last 20, so undo is "copy it back". `list_dashboards` marks agent-written dashboards with `written_by: mcp` in the file.
- **Size limits:** at most 200 panels and 1 MB of YAML.
- **Writes can be turned off:** `mcp: { enabled: true, writes: true }`, or `--mcp-read-only`.

### Code layout

Teleplot stays a single crate with directory modules (vault convention).

```
src/mcp/
  protocol.rs   # pure dispatch: (&Server, request JSON, headers) → response; both spec generations
  http.rs       # axum handler: Origin/token/header checks → protocol
  stdio.rs      # `teleplot mcp`
  tools.rs      # the seven tools; each takes typed input and returns typed output
src/lint/       # semantic validation (also `teleplot lint`)
src/store.rs    # atomic save, history, hash checks
```

### Testing

1. **Protocol unit tests** on the pure dispatcher, using recorded exchanges for both spec generations:
   - discover / initialize, tools/list, tools/call;
   - malformed input, unknown tool, and headers that disagree with the body;
   - an unsupported protocol version.
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
3. **Annotations (gap 3).** This is the delivery-analytics payoff.
4. **ES|QL (gap 4).** It also brings more Kibana dashboards into scope.
5. **Baseline comparison (gap 6),** the feature that sets Teleplot apart, then chained/ad-hoc variables (gap 5).
6. **Revisit 7–10** once there is a decision about sharing and authentication.

## Open questions

- **Writes on by default?** The plan enables writes when bound to localhost, and makes `--mcp-read-only` available. The alternative is read-only by default, with writes opted in.
- **Is `.history/` enough as version history,** or should agent saves commit to git when the dashboards directory is a repository? That would make them reviewable, but it adds a git dependency (the vault says not to shell out).
- **Where do agent-written dashboards go?** The plan puts them beside hand-written ones, marked `written_by: mcp`. The alternative is a separate `dashboards/agent/` directory, which would need recursive loading.
