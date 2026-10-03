//! Teleplot's dashboard model, shared by server and client.
//!
//! Kibana exports are converted into this model; its serde form is also the
//! native YAML dashboard format. It describes *what* to show. Building
//! Elasticsearch requests from it is the query engine's job.

pub mod api;
mod data;
mod interval;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use data::{Cell, LayerData, PanelData, Series};
pub use interval::{Interval, IntervalUnit};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Dashboard {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default time range; `None` means use the viewer's current range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeRange>,
    /// Auto-refresh interval in milliseconds, if enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Query::is_empty")]
    pub query: Query,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<Filter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<Control>,
    /// Buckets with fewer documents than this are drawn as unreliable
    /// (small samples make medians and rates noisy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_samples: Option<u32>,
    pub panels: Vec<Panel>,
}

/// Kibana date math, e.g. `now-90d/d` or an ISO timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct TimeRange {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum QueryLanguage {
    #[default]
    Kql,
    Lucene,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Query {
    #[serde(default)]
    pub language: QueryLanguage,
    #[serde(default)]
    pub text: String,
}

impl Query {
    pub fn kql(text: impl Into<String>) -> Self {
        Self {
            language: QueryLanguage::Kql,
            text: text.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A filter pill. The query is Elasticsearch DSL, applied as-is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Filter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The field the pill is about, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub negate: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    pub query: Value,
}

/// A dashboard control, offered to the viewer as a filter on one field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Control {
    pub id: String,
    pub title: String,
    pub field: String,
    pub data_view: DataView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct DataView {
    /// Index pattern, e.g. `delivery-*`.
    pub index: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Panel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Position on a 48-column grid; `h` is in grid rows.
    pub grid: Grid,
    /// Overrides the dashboard time range for this panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeRange>,
    pub content: PanelContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Grid {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PanelContent {
    Chart(Chart),
    Markdown { text: String },
    Placeholder(Placeholder),
}

/// A panel Teleplot cannot render, shown as a labelled card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Placeholder {
    /// What the source called it, e.g. `vega`, `map`, `lens:lnsHeatmap`.
    pub kind: String,
    pub reason: PlaceholderReason,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PlaceholderReason {
    /// Deliberately not supported (see the MVP scope).
    OutOfScope,
    /// The export refers to an object it does not contain.
    BrokenReference,
    /// In scope but not understood: a Teleplot bug or an unknown variant.
    Unparseable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Chart {
    pub kind: ChartKind,
    #[serde(default, skip_serializing_if = "Query::is_empty")]
    pub query: Query,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<Filter>,
    pub layers: Vec<Layer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChartKind {
    Xy,
    Metric,
    Pie,
    Donut,
    Table,
}

/// One query against one data view, and how its columns are drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Layer {
    pub id: String,
    pub data_view: DataView,
    pub columns: Vec<Column>,
    pub encoding: Encoding,
    /// Fetch individual documents (one row each) instead of aggregating.
    /// Columns are then [`Operation::Value`]s and computed columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<Documents>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Documents {
    /// At most this many documents (newest first, then shown in order).
    pub limit: u32,
    /// Field to order by; usually the time field.
    pub sort: String,
}

impl Layer {
    pub fn column(&self, id: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(tag = "as", rename_all = "snake_case")]
pub enum Encoding {
    Xy {
        series: SeriesType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<String>,
        y: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split: Option<String>,
    },
    Metric {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secondary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        breakdown: Option<String>,
    },
    Pie {
        groups: Vec<String>,
        values: Vec<String>,
    },
    Table {
        columns: Vec<String>,
    },
    /// A middle line with a shaded band between two other columns, e.g.
    /// median cycle time inside its 25th–75th percentile band.
    Band {
        x: String,
        low: String,
        mid: String,
        high: String,
    },
    /// One point per row (document layers), with an optional trend line.
    Scatter {
        x: String,
        y: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trend: Option<String>,
        /// Log scale for y: long-tailed durations stay readable.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        log_scale: bool,
    },
}

impl Encoding {
    /// Every column the encoding draws.
    pub fn columns(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        match self {
            Encoding::Xy { x, y, split, .. } => {
                ids.extend(x.as_deref());
                ids.extend(y.iter().map(String::as_str));
                ids.extend(split.as_deref());
            }
            Encoding::Metric {
                value,
                secondary,
                max,
                breakdown,
            } => {
                ids.push(value);
                ids.extend(
                    [secondary, max, breakdown]
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );
            }
            Encoding::Pie { groups, values } => {
                ids.extend(groups.iter().chain(values).map(String::as_str))
            }
            Encoding::Table { columns } => ids.extend(columns.iter().map(String::as_str)),
            Encoding::Band { x, low, mid, high } => {
                ids.extend([x, low, mid, high].map(String::as_str))
            }
            Encoding::Scatter {
                x, y, split, trend, ..
            } => {
                ids.extend([x.as_str(), y.as_str()]);
                ids.extend([split, trend].into_iter().flatten().map(String::as_str));
            }
        }
        ids
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SeriesType {
    Line,
    Area,
    AreaStacked,
    AreaPercentageStacked,
    Bar,
    BarStacked,
    BarPercentageStacked,
    BarHorizontal,
    BarHorizontalStacked,
    BarHorizontalPercentageStacked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Column {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(flatten)]
    pub op: Operation,
    /// Restricts the documents this column sees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Query>,
    /// Kibana time shift, e.g. `1w` or `previous`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_shift: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
    /// Only an input to another column (a formula part); not drawn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub helper: bool,
    /// Benchmark bands for this column's values, e.g. DORA-style
    /// Elite/High/Medium/Low lead time. Drawn behind charts and named on tiles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<Threshold>,
}

/// A labelled range of values: `from` inclusive, `to` exclusive; open-ended
/// when absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Threshold {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<f64>,
    /// CSS colour; a default per position otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl Threshold {
    pub fn contains(&self, v: f64) -> bool {
        self.from.is_none_or(|f| v >= f) && self.to.is_none_or(|t| v < t)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    // Metrics. `field: None` on count means "documents".
    Count {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    Sum {
        field: String,
    },
    Average {
        field: String,
    },
    Min {
        field: String,
    },
    Max {
        field: String,
    },
    Median {
        field: String,
    },
    StandardDeviation {
        field: String,
    },
    UniqueCount {
        field: String,
    },
    Percentile {
        field: String,
        percent: f64,
    },
    PercentileRank {
        field: String,
        value: f64,
    },
    LastValue {
        field: String,
        sort_field: String,
    },
    /// A field's value in each document (document layers only).
    Value {
        field: String,
    },
    StaticValue {
        value: f64,
    },
    /// Milliseconds in the panel's time range.
    TimeRange,

    // Buckets.
    DateHistogram {
        field: String,
        interval: Interval,
        #[serde(default)]
        include_empty: bool,
    },
    Terms {
        fields: Vec<String>,
        size: u32,
        #[serde(default)]
        order: TermsOrder,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        other_bucket: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        missing_bucket: bool,
    },
    Filters {
        filters: Vec<NamedQuery>,
    },
    Histogram {
        field: String,
        /// Fixed bucket width; `None` lets the engine choose.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval: Option<f64>,
    },
    Ranges {
        field: String,
        ranges: Vec<Range>,
    },

    // Computed from other columns of the same layer.
    Formula {
        text: String,
        expr: Expr,
    },
    MovingAverage {
        of: String,
        window: u32,
    },
    CumulativeSum {
        of: String,
    },
    Differences {
        of: String,
    },
    CounterRate {
        of: String,
    },
    /// The median of the input over the last `window` rows (this one
    /// included), e.g. a trend line through a cycle-time scatter.
    RollingMedian {
        of: String,
        window: u32,
    },
    /// One value across all buckets of the input (Lens `overall_*`), e.g.
    /// the total that a percentage-of-total formula divides by.
    Overall {
        of: String,
        function: OverallFunction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OverallFunction {
    Sum,
    Average,
    Min,
    Max,
}

impl Operation {
    pub fn is_bucket(&self) -> bool {
        matches!(
            self,
            Operation::DateHistogram { .. }
                | Operation::Terms { .. }
                | Operation::Filters { .. }
                | Operation::Histogram { .. }
                | Operation::Ranges { .. }
        )
    }

    /// Columns this one is computed from.
    pub fn inputs(&self) -> Vec<&str> {
        match self {
            Operation::Formula { expr, .. } => expr.columns(),
            Operation::MovingAverage { of, .. }
            | Operation::CumulativeSum { of }
            | Operation::Differences { of }
            | Operation::CounterRate { of }
            | Operation::Overall { of, .. }
            | Operation::RollingMedian { of, .. } => vec![of],
            _ => vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum TermsOrder {
    /// Descending by a metric column.
    Column {
        column: String,
        #[serde(default)]
        ascending: bool,
    },
    #[default]
    Count,
    Alphabetical {
        #[serde(default)]
        ascending: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct NamedQuery {
    pub label: String,
    pub query: Query,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Range {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A formula expression over the layer's columns (Kibana's tinymath AST).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Expr {
    Number(f64),
    Column(String),
    Call { call: String, args: Vec<Expr> },
}

impl Expr {
    pub fn columns(&self) -> Vec<&str> {
        match self {
            Expr::Number(_) => vec![],
            Expr::Column(c) => vec![c],
            Expr::Call { args, .. } => args.iter().flat_map(Expr::columns).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Format {
    pub kind: FormatKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FormatKind {
    Number,
    Percent,
    Bytes,
    Duration,
    Currency,
}
