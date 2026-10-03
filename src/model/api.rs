//! The JSON API between the Teleplot server and its browser client.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{PanelData, Query};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardSummary {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub panels: usize,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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
pub struct DataResponse {
    /// The resolved range, epoch milliseconds.
    pub from_ms: i64,
    pub to_ms: i64,
    /// IANA zone the data was bucketed in; dates display in it.
    pub time_zone: String,
    pub panels: HashMap<String, PanelData>,
}

/// `POST /api/dashboards/{id}/controls/{control}`: values to choose from.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ControlValues {
    pub values: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
