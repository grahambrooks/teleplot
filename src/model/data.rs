//! Query results, as sent from server to client: one table per chart layer,
//! stored column by column because that is what charts consume.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PanelData {
    pub layers: Vec<LayerData>,
    /// Things the viewer should know, e.g. an interval that was scaled up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LayerData {
    pub layer: String,
    pub rows: usize,
    /// One series per drawn column (helpers omitted), in layer order.
    pub columns: Vec<Series>,
    /// Set when the layer's query failed; `columns` is then empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LayerData {
    pub fn series(&self, column: &str) -> Option<&Series> {
        self.columns.iter().find(|s| s.id == column)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub id: String,
    pub values: Vec<Cell>,
}

/// A table cell. Date histogram keys are epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
