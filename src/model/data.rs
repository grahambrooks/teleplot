//! Query results, as sent from server to client: one table per chart layer,
//! stored column by column because that is what charts consume.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct PanelData {
    pub layers: Vec<LayerData>,
    /// Things the viewer should know, e.g. an interval that was scaled up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Events to mark on the chart: its own annotations and the dashboard's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<AnnotationData>,
}

/// One annotation's events, with how to draw them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct AnnotationData {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: super::LineStyle,
    /// Oldest first.
    pub events: Vec<Event>,
    /// Set when the search failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// An event: epoch milliseconds, with an end for a range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Event {
    pub at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct LayerData {
    pub layer: String,
    pub rows: usize,
    /// One series per drawn column (helpers omitted), in layer order.
    pub columns: Vec<Series>,
    /// Documents behind each row (aggregating layers), for small-sample warnings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counts: Vec<f64>,
    /// Set when the layer's query failed; `columns` is then empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Columns holding dates (as epoch milliseconds) whose type the model
    /// can't tell, e.g. an ES|QL `BUCKET` of a date.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dates: Vec<String>,
    /// Every bucket start (epoch ms) of a bucketed query, including empty
    /// buckets: a time axis spans them all, so bars keep the interval's width
    /// when only a few buckets have data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<f64>,
}

impl LayerData {
    pub fn series(&self, column: &str) -> Option<&Series> {
        self.columns.iter().find(|s| s.id == column)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Series {
    pub id: String,
    pub values: Vec<Cell>,
}

/// A table cell. Date histogram keys are epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Cell {
    Number(f64),
    Text(String),
    Null,
}

impl Cell {
    pub fn number(&self) -> Option<f64> {
        match self {
            Cell::Number(n) => Some(*n),
            _ => None,
        }
    }
}

impl From<Option<f64>> for Cell {
    fn from(v: Option<f64>) -> Self {
        v.filter(|n| n.is_finite()).map_or(Cell::Null, Cell::Number)
    }
}
