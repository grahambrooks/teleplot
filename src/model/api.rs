//! The JSON API between the Teleplot server and its browser client.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{PanelData, Query};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct DashboardSummary {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub panels: usize,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Catalog {
    pub dashboards: Vec<DashboardSummary>,
    /// Files that failed to load, and why.
    #[serde(default)]
    pub errors: Vec<String>,
    /// Set when Elasticsearch is not configured or unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// `POST /api/dashboards/{id}/data`
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct DataRequest {
    /// Kibana date math; the dashboard default when absent.
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// The viewer's query bar, combined with the dashboard's own query.
    #[serde(default)]
    pub query: Option<Query>,
    /// Control selections: control id → values.
    #[serde(default)]
    pub selections: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct DataResponse {
    /// The resolved range, epoch milliseconds.
    pub from_ms: i64,
    pub to_ms: i64,
    /// IANA zone the data was bucketed in; dates display in it.
    pub time_zone: String,
    pub panels: HashMap<String, PanelData>,
}

/// `POST /api/dashboards/{id}/panels/{panel}/inspect` (same body as `data`):
/// how a panel's numbers were made. The searches run again, past
/// Teleplot's response cache, so `took_ms` is current.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Inspection {
    pub requests: Vec<InspectedRequest>,
    /// The panel's data, from these responses.
    pub data: PanelData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct InspectedRequest {
    /// What the search is for, e.g. a layer, or a time-shifted copy of one.
    pub description: String,
    pub index: String,
    /// The search body sent to Elasticsearch.
    pub request: Value,
    /// Elasticsearch's response, whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Elasticsearch's own time for the search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub took_ms: Option<u64>,
    /// Documents the search matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hits: Option<u64>,
}

/// `POST /api/dashboards/{id}/controls/{control}`: values to choose from.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct ControlValues {
    pub values: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct ApiError {
    pub error: String,
}
