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
    /// Ad-hoc filter pills the viewer added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<AdHocFilter>,
}

/// A filter pill the viewer added: `field` is one of `values` (or, with
/// `negate`, none of them). It narrows the panels whose data has the field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct AdHocFilter {
    pub field: String,
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub negate: bool,
}

impl AdHocFilter {
    /// As a filter pill (`terms` on the field).
    pub fn to_filter(&self) -> super::Filter {
        super::Filter {
            label: None,
            field: Some(self.field.clone()),
            negate: self.negate,
            disabled: false,
            query: serde_json::json!({ "terms": { &self.field: self.values } }),
        }
    }
}

/// `GET /api/dashboards/{id}/fields`: fields an ad-hoc filter can use.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct FieldList {
    /// Name and Elasticsearch types, sorted by name.
    pub fields: Vec<(String, Vec<String>)>,
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

/// `POST /api/dashboards/{id}/panels/{panel}/compare`: how a selection on a
/// scatter or heatmap differs from the rest of the panel's documents.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct CompareRequest {
    #[serde(flatten)]
    pub request: DataRequest,
    pub selection: Selection,
}

/// A box on a chart: an x range (epoch ms on a time axis) and an optional
/// value range on y. Missing ends are open.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Selection {
    pub x: (f64, f64),
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<(Option<f64>, Option<f64>)>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Comparison {
    /// Documents in the selection, and in the whole panel (the baseline).
    pub selected: u64,
    pub baseline: u64,
    /// Fields whose values are spread differently in the selection, most
    /// different first.
    pub fields: Vec<FieldDiff>,
    /// Numeric fields whose median differs, most different first.
    pub measures: Vec<MeasureDiff>,
    /// Fields left out, and why (people, IDs, too many values).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct FieldDiff {
    pub field: String,
    /// How differently the values are spread: half the summed differences
    /// in share, 0 (the same) to 1 (nothing in common).
    pub score: f64,
    /// The values whose share changed most, over-represented first.
    pub values: Vec<ValueDiff>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct ValueDiff {
    pub value: String,
    pub selected: u64,
    pub baseline: u64,
    /// Share of the selection (0–1), and of the baseline.
    pub selected_share: f64,
    pub baseline_share: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct MeasureDiff {
    pub field: String,
    pub selected_median: f64,
    pub baseline_median: f64,
}
