//! MCP server: lets an agent discover data, design dashboards, check and
//! preview them against the real cluster, and save them for Teleplot to serve.
//!
//! Built on `rmcp`, which tracks the evolving protocol (2026-07-28 stateless
//! requests and the earlier session-based versions). Served at `/mcp` by
//! `teleplot serve` and over stdio by `teleplot mcp`.
//!
//! Problems an agent can fix (a missing field, a stale hash) come back as
//! ordinary results it can read and act on; protocol errors are kept for
//! requests that cannot be understood at all.

use std::sync::Arc;

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    Implementation, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::Request;
use crate::lint::{self, Problem};
use crate::model::{Cell, Dashboard, Operation, PanelContent, TimeRange};
use crate::server::{Source, Teleplot};

const SCHEMA_URI: &str = "teleplot://schema/dashboard";
const DOCS_URI: &str = "teleplot://docs/dashboards";

const INSTRUCTIONS: &str = "Teleplot draws dashboards over Elasticsearch. To build one: \
find data with teleplot_list_indices and teleplot_describe_index; write a dashboard \
(format: resource teleplot://docs/dashboards, schema: teleplot://schema/dashboard; \
dashboards/delivery-overview is a complete example via teleplot_get_dashboard); \
check it with teleplot_validate_dashboard; look at the numbers with teleplot_preview \
and make sure they answer the question; then teleplot_save_dashboard. Saved dashboards \
appear in Teleplot immediately at the returned URL.";

#[derive(Clone)]
pub struct TeleplotMcp {
    app: Arc<Teleplot>,
    /// Saving is allowed only while Teleplot is local (loopback or stdio).
    writes: bool,
    /// Where Teleplot's UI is, for links (e.g. `http://127.0.0.1:3000`).
    base_url: String,
}

// ---------------------------------------------------------------- inputs and outputs

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IndexParams {
    /// Index name or pattern, e.g. `delivery-prs` or `logs-*`.
    pub index: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DashboardIdParams {
    /// Dashboard id, as listed by teleplot_list_dashboards.
    pub id: String,
    /// `summary` (default): panels and what they show. `full`: the whole dashboard, to edit and save.
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Summary,
    Full,
}

fn dashboard_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    generator.subschema_for::<Dashboard>()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DashboardParams {
    /// The dashboard (see resource teleplot://docs/dashboards for the format).
    #[schemars(schema_with = "dashboard_schema")]
    pub dashboard: Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PreviewParams {
    #[schemars(schema_with = "dashboard_schema")]
    pub dashboard: Value,
    /// Only this panel (by id); all chart panels when absent.
    #[serde(default)]
    pub panel: Option<String>,
    /// Time range in Kibana date math (default: the dashboard's, else the last 90 days).
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// Rows shown per layer (default 20, at most 200). Summaries cover all rows.
    #[serde(default)]
    pub max_rows: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SaveParams {
    #[schemars(schema_with = "dashboard_schema")]
    pub dashboard: Value,
    /// Required to replace an existing agent dashboard: the `hash` teleplot_get_dashboard returned.
    #[serde(default)]
    pub expected_hash: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Indices {
    pub indices: Vec<IndexInfo>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct IndexInfo {
    pub name: String,
    pub documents: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct IndexDescription {
    pub index: String,
    /// Date fields; use one as the data view's `time_field`.
    pub time_fields: Vec<String>,
    pub fields: Vec<FieldInfo>,
    /// The newest document, for a feel of the data.
    pub sample: Option<Value>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FieldInfo {
    pub name: String,
    pub types: Vec<String>,
    /// Whether terms/unique_count/controls can use it (not full text).
    pub aggregatable: bool,
    /// Most common values with document counts (keyword and boolean fields).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_values: Vec<(String, u64)>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DashboardList {
    pub dashboards: Vec<DashboardListing>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DashboardListing {
    pub id: String,
    pub title: String,
    /// kibana / yaml (people's) / agent (saved through MCP; the only ones agents may replace).
    pub source: Source,
    pub panels: usize,
    pub url: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DashboardDetail {
    pub id: String,
    pub title: String,
    pub source: Source,
    pub url: String,
    /// Pass back as `expected_hash` to replace this dashboard (agent dashboards only).
    pub hash: String,
    pub panels: Vec<PanelSummary>,
    /// The whole dashboard, with format=full.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<Value>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PanelSummary {
    pub id: String,
    pub title: Option<String>,
    /// chart:xy, chart:metric, markdown, placeholder:… (unsupported in Teleplot).
    pub kind: String,
    pub indexes: Vec<String>,
    /// Labels of the columns it draws.
    pub shows: Vec<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Validation {
    /// True when there are no errors (warnings may remain).
    pub valid: bool,
    pub problems: Vec<Problem>,
    /// Set when fields could not be checked against Elasticsearch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Preview {
    /// Errors stop the preview; fix them and try again.
    pub problems: Vec<Problem>,
    /// The range that was queried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<(String, String)>,
    pub panels: Vec<PanelPreview>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PanelPreview {
    pub id: String,
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub layers: Vec<LayerPreview>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct LayerPreview {
    pub columns: Vec<String>,
    /// The first rows (dates as ISO 8601).
    pub rows: Vec<Vec<Value>>,
    pub total_rows: usize,
    /// Per numeric column, over all rows.
    pub summary: Vec<ColumnSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ColumnSummary {
    pub column: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub last: Option<f64>,
    pub non_empty: usize,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SaveResult {
    pub saved: bool,
    /// Why not, when not saved: errors to fix, or a hash/ownership conflict.
    pub problems: Vec<Problem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<bool>,
}

fn internal(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

fn problem(message: impl Into<String>) -> Problem {
    Problem {
        path: String::new(),
        severity: lint::Severity::Error,
        message: message.into(),
    }
}

// ---------------------------------------------------------------- tools

#[tool_router]
impl TeleplotMcp {
    pub fn new(app: Arc<Teleplot>, writes: bool, base_url: String) -> Self {
        Self {
            app,
            writes,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    fn url(&self, id: &str) -> String {
        format!("{}/d/{}", self.base_url, id)
    }

    fn engine(&self) -> Result<&crate::engine::Engine, ErrorData> {
        self.app
            .engine
            .as_ref()
            .map_err(|e| internal(format!("Elasticsearch is not available: {e}")))
    }

    /// Lints against live field caps when Elasticsearch is reachable.
    async fn check(&self, d: &Dashboard) -> (Vec<Problem>, Option<String>) {
        let Ok(engine) = &self.app.engine else {
            let note = self
                .app
                .engine
                .as_ref()
                .err()
                .map(|e| format!("fields not checked: {e}"));
            return (lint::check(d, None), note);
        };
        let mut indexes: Vec<String> = d
            .controls
            .iter()
            .map(|c| c.data_view.index.clone())
            .collect();
        for p in &d.panels {
            if let PanelContent::Chart(c) = &p.content {
                indexes.extend(c.layers.iter().map(|l| l.data_view.index.clone()));
            }
        }
        let fields = engine.fields_for(&indexes).await;
        (lint::check(d, Some(&fields)), None)
    }

    #[tool(
        name = "teleplot_list_indices",
        description = "List Elasticsearch indices with document counts. Start here to find data.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_indices(&self) -> Result<Json<Indices>, ErrorData> {
        let indices = self.engine()?.client().indices().await.map_err(internal)?;
        Ok(Json(Indices {
            indices: indices
                .into_iter()
                .map(|(name, documents)| IndexInfo { name, documents })
                .collect(),
        }))
    }

    #[tool(
        name = "teleplot_describe_index",
        description = "Describe an index: its fields and their types, which are dates and which can be grouped by, the top values of keyword fields, and a sample document.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn describe_index(
        &self,
        Parameters(p): Parameters<IndexParams>,
    ) -> Result<Json<IndexDescription>, ErrorData> {
        let engine = self.engine()?;
        let fields = engine
            .fields_for(std::slice::from_ref(&p.index))
            .await
            .remove(&p.index)
            .unwrap_or_default();
        if fields.is_empty() {
            return Err(ErrorData::invalid_params(
                format!("no index matches {:?}; see teleplot_list_indices", p.index),
                None,
            ));
        }
        let is =
            |f: &crate::kql::Field, t: &[&str]| f.es_types.iter().all(|x| t.contains(&x.as_str()));
        let time_fields: Vec<String> = fields
            .iter()
            .filter(|f| is(f, &["date", "date_nanos"]))
            .map(|f| f.name.clone())
            .collect();
        let grouped: Vec<&crate::kql::Field> = fields
            .iter()
            .filter(|f| is(f, &["keyword", "boolean"]))
            .take(30)
            .collect();

        // One search: the newest document, and top values of each keyword field.
        let mut aggs = serde_json::Map::new();
        for (i, f) in grouped.iter().enumerate() {
            aggs.insert(
                format!("f{i}"),
                json!({ "terms": { "field": f.name, "size": 5 } }),
            );
        }
        let sort = time_fields
            .first()
            .map(|t| json!([{ t.clone(): { "order": "desc" } }]))
            .unwrap_or(json!([]));
        let body = json!({ "size": 1, "track_total_hits": false, "sort": sort, "aggs": aggs });
        let search = crate::es::Search {
            index: p.index.clone(),
            body,
        };
        let response = engine
            .client()
            .msearch_full(&[search])
            .await
            .map_err(internal)?
            .remove(0)
            .map_err(internal)?;

        let top = |i: usize| -> Vec<(String, u64)> {
            response["aggregations"][format!("f{i}")]["buckets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| {
                    let key = b["key_as_string"].as_str().map(String::from).or_else(
                        || match &b["key"] {
                            Value::String(s) => Some(s.clone()),
                            other => Some(other.to_string()),
                        },
                    )?;
                    Some((key, b["doc_count"].as_u64().unwrap_or(0)))
                })
                .collect()
        };
        let infos = fields
            .iter()
            .map(|f| {
                let index = grouped.iter().position(|g| g.name == f.name);
                FieldInfo {
                    name: f.name.clone(),
                    types: f.es_types.clone(),
                    aggregatable: !f.es_types.iter().any(|t| t == "text"),
                    top_values: index.map(top).unwrap_or_default(),
                }
            })
            .collect();
        Ok(Json(IndexDescription {
            index: p.index,
            time_fields,
            fields: infos,
            sample: response["hits"]["hits"][0]["_source"]
                .as_object()
                .map(|o| Value::Object(o.clone())),
        }))
    }

    #[tool(
        name = "teleplot_list_dashboards",
        description = "List Teleplot's dashboards: id, title, where it came from (only `agent` dashboards can be replaced by agents), and its URL.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_dashboards(&self) -> Json<DashboardList> {
        self.app.catalog.reload_if_changed();
        let dashboards = self
            .app
            .catalog
            .entries()
            .iter()
            .map(|e| DashboardListing {
                id: e.dashboard.id.clone(),
                title: e.dashboard.title.clone(),
                source: e.source,
                panels: e.dashboard.panels.len(),
                url: self.url(&e.dashboard.id),
            })
            .collect();
        Json(DashboardList { dashboards })
    }

    #[tool(
        name = "teleplot_get_dashboard",
        description = "Get a dashboard: a summary of its panels, or with format=full the whole dashboard to edit and save. Returns the hash needed to replace an agent dashboard.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_dashboard(
        &self,
        Parameters(p): Parameters<DashboardIdParams>,
    ) -> Result<Json<DashboardDetail>, ErrorData> {
        self.app.catalog.reload_if_changed();
        let entry = self.app.catalog.entry(&p.id).ok_or_else(|| {
            ErrorData::invalid_params(
                format!("no dashboard {:?}; see teleplot_list_dashboards", p.id),
                None,
            )
        })?;
        let bytes = std::fs::read(&entry.path).map_err(internal)?;
        let d = &entry.dashboard;
        let panels = d
            .panels
            .iter()
            .map(|panel| {
                let (kind, indexes, shows) = match &panel.content {
                    PanelContent::Chart(c) => (
                        format!(
                            "chart:{}",
                            serde_json::to_value(c.kind)
                                .ok()
                                .and_then(|v| v.as_str().map(String::from))
                                .unwrap_or_default()
                        ),
                        c.layers.iter().map(|l| l.data_view.index.clone()).collect(),
                        c.layers
                            .iter()
                            .flat_map(|l| {
                                l.encoding.columns().into_iter().filter_map(|id| {
                                    l.column(id)
                                        .map(|c| c.label.clone().unwrap_or_else(|| c.id.clone()))
                                })
                            })
                            .collect(),
                    ),
                    PanelContent::Markdown { .. } => ("markdown".into(), vec![], vec![]),
                    PanelContent::Placeholder(ph) => {
                        (format!("placeholder:{}", ph.kind), vec![], vec![])
                    }
                };
                PanelSummary {
                    id: panel.id.clone(),
                    title: panel.title.clone(),
                    kind,
                    indexes,
                    shows,
                }
            })
            .collect();
        Ok(Json(DashboardDetail {
            id: d.id.clone(),
            title: d.title.clone(),
            source: entry.source,
            url: self.url(&d.id),
            hash: crate::store::hash(&bytes),
            panels,
            dashboard: (p.format == Format::Full)
                .then(|| serde_json::to_value(d))
                .transpose()
                .map_err(internal)?,
        }))
    }

    #[tool(
        name = "teleplot_validate_dashboard",
        description = "Check a dashboard without saving it: the format, references between columns, KQL, and (against Elasticsearch) that indexes and fields exist with suitable types. Every problem has a path and says what to fix.",
        annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false)
    )]
    async fn validate_dashboard(
        &self,
        Parameters(p): Parameters<DashboardParams>,
    ) -> Json<Validation> {
        match lint::parse(p.dashboard) {
            Err(problem) => Json(Validation {
                valid: false,
                problems: vec![problem],
                note: None,
            }),
            Ok(d) => {
                let (problems, note) = self.check(&d).await;
                Json(Validation {
                    valid: !lint::has_errors(&problems),
                    problems,
                    note,
                })
            }
        }
    }

    #[tool(
        name = "teleplot_preview",
        description = "Run a dashboard (or one panel) without saving it and return its data: the first rows of each layer and min/max/last per numeric column. Use it to check the numbers answer the question before saving.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn preview(
        &self,
        Parameters(p): Parameters<PreviewParams>,
    ) -> Result<Json<Preview>, ErrorData> {
        let mut d = match lint::parse(p.dashboard) {
            Ok(d) => d,
            Err(problem) => {
                return Ok(Json(Preview {
                    problems: vec![problem],
                    range: None,
                    panels: vec![],
                }));
            }
        };
        let (problems, _) = self.check(&d).await;
        if lint::has_errors(&problems) {
            return Ok(Json(Preview {
                problems,
                range: None,
                panels: vec![],
            }));
        }
        if let Some(only) = &p.panel {
            d.panels.retain(|panel| &panel.id == only);
            if d.panels.is_empty() {
                return Ok(Json(Preview {
                    problems: vec![problem(format!("no panel {only:?}"))],
                    range: None,
                    panels: vec![],
                }));
            }
        }
        let engine = self.engine()?;
        let time = match (p.from, p.to) {
            (Some(from), Some(to)) => Some(TimeRange { from, to }),
            _ => d.time.clone().or(Some(TimeRange {
                from: "now-90d/d".into(),
                to: "now".into(),
            })),
        };
        let request = Request {
            time,
            ..Default::default()
        };
        let now = jiff::Timestamp::now();
        let range = engine
            .range(&d, &request, now)
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        let data = engine
            .dashboard_data(&d, &request, now)
            .await
            .map_err(internal)?;
        let max_rows = p.max_rows.unwrap_or(20).min(200);
        let iso = |ms: f64| {
            jiff::Timestamp::from_millisecond(ms as i64)
                .map(|t| t.to_string())
                .unwrap_or_default()
        };

        let mut panels = Vec::new();
        for panel in &d.panels {
            let PanelContent::Chart(chart) = &panel.content else {
                continue;
            };
            let Some(pd) = data.get(&panel.id) else {
                continue;
            };
            let layers = chart
                .layers
                .iter()
                .zip(&pd.layers)
                .map(|(layer, ld)| {
                    let is_date = |id: &str| match layer.column(id).map(|c| &c.op) {
                        Some(Operation::DateHistogram { .. }) => true,
                        Some(Operation::Value { field }) => {
                            layer.data_view.time_field.as_ref() == Some(field)
                        }
                        _ => false,
                    };
                    let columns = ld
                        .columns
                        .iter()
                        .map(|s| {
                            layer
                                .column(&s.id)
                                .and_then(|c| c.label.clone())
                                .unwrap_or_else(|| s.id.clone())
                        })
                        .collect();
                    let rows = (0..ld.rows.min(max_rows))
                        .map(|r| {
                            ld.columns
                                .iter()
                                .map(|s| match &s.values[r] {
                                    Cell::Number(n) if is_date(&s.id) => json!(iso(*n)),
                                    Cell::Number(n) => json!(n),
                                    Cell::Text(t) => json!(t),
                                    Cell::Null => Value::Null,
                                })
                                .collect()
                        })
                        .collect();
                    let summary = ld
                        .columns
                        .iter()
                        .filter(|s| {
                            !is_date(&s.id) && s.values.iter().any(|v| matches!(v, Cell::Number(_)))
                        })
                        .map(|s| {
                            let numbers: Vec<f64> =
                                s.values.iter().filter_map(Cell::number).collect();
                            ColumnSummary {
                                column: layer
                                    .column(&s.id)
                                    .and_then(|c| c.label.clone())
                                    .unwrap_or_else(|| s.id.clone()),
                                min: numbers.iter().copied().reduce(f64::min),
                                max: numbers.iter().copied().reduce(f64::max),
                                last: s.values.iter().rev().find_map(Cell::number),
                                non_empty: numbers.len(),
                            }
                        })
                        .collect();
                    LayerPreview {
                        columns,
                        rows,
                        total_rows: ld.rows,
                        summary,
                        error: ld.error.clone(),
                    }
                })
                .collect();
            panels.push(PanelPreview {
                id: panel.id.clone(),
                title: panel.title.clone(),
                warnings: pd.warnings.clone(),
                layers,
            });
        }
        Ok(Json(Preview {
            problems,
            range: Some((iso(range.from_ms as f64), iso(range.to_ms as f64))),
            panels,
        }))
    }

    #[tool(
        name = "teleplot_save_dashboard",
        description = "Save a dashboard so Teleplot serves it (in dashboards/agents/). Validates first and refuses on errors. To replace an agent dashboard, pass expected_hash from teleplot_get_dashboard; dashboards written by people or exported from Kibana cannot be replaced — save a copy under a new id.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn save_dashboard(&self, Parameters(p): Parameters<SaveParams>) -> Json<SaveResult> {
        let refused = |problems: Vec<Problem>| {
            Json(SaveResult {
                saved: false,
                problems,
                url: None,
                hash: None,
                created: None,
            })
        };
        if !self.writes {
            return refused(vec![problem(
                "saving is disabled: Teleplot only accepts agent writes when it runs on localhost",
            )]);
        }
        let d = match lint::parse(p.dashboard) {
            Ok(d) => d,
            Err(problem) => return refused(vec![problem]),
        };
        let (problems, _) = self.check(&d).await;
        if lint::has_errors(&problems) {
            return refused(problems);
        }
        self.app.catalog.reload_if_changed();
        let others = self.app.catalog.entries();
        match crate::store::save(
            &self.app.catalog.agents_dir(),
            &d,
            p.expected_hash.as_deref(),
            &others,
        ) {
            Err(e) => refused(vec![problem(e)]),
            Ok(saved) => {
                self.app.catalog.reload_if_changed();
                Json(SaveResult {
                    saved: true,
                    problems,
                    url: Some(self.url(&d.id)),
                    hash: Some(saved.hash),
                    created: Some(saved.created),
                })
            }
        }
    }
}

// ---------------------------------------------------------------- server

#[tool_handler]
impl ServerHandler for TeleplotMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("teleplot", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(SCHEMA_URI, "Dashboard JSON Schema"),
            Resource::new(DOCS_URI, "Dashboard format (docs/dashboards.md)"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let text = match request.uri.as_str() {
            SCHEMA_URI => {
                serde_json::to_string_pretty(&schemars::schema_for!(Dashboard)).map_err(internal)?
            }
            DOCS_URI => include_str!("../docs/dashboards.md").to_string(),
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("no resource {other}"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into())
    }
}

/// The JSON Schema of the dashboard format, for `teleplot lint` and editors.
pub fn dashboard_schema_json() -> Value {
    serde_json::to_value(schemars::schema_for!(Dashboard)).unwrap_or(Value::Null)
}
