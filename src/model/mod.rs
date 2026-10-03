//! Teleplot's dashboard model, shared by server and client.
//!
//! Kibana exports are converted into this model; its serde form is also the
//! native YAML dashboard format. It describes *what* to show. Building
//! Elasticsearch requests from it is the query engine's job.

pub mod api;
mod data;
mod interval;
pub mod table;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use data::{AnnotationData, Cell, Event, LayerData, PanelData, Series};
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
    /// Controls narrow the ones after them (Kibana's hierarchical
    /// chaining): a repo control offers only the chosen team's repos, and
    /// changing the team clears the repo. `false` makes them independent.
    #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
    pub chain_controls: bool,
    /// Buckets with fewer documents than this are drawn as unreliable
    /// (small samples make medians and rates noisy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_samples: Option<u32>,
    /// Events marked on every time-series chart, e.g. failed deployments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
    pub panels: Vec<Panel>,
}

/// Events marked across time-series charts: one search for documents (their
/// time, and optionally an end time and a text field), or fixed `events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Annotation {
    pub id: String,
    /// Shown in the legend, e.g. "Failed deployments".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Where the documents come from. Leave out for fixed `events`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_view: Option<DataView>,
    /// Which documents to mark.
    #[serde(default, skip_serializing_if = "Query::is_empty")]
    pub query: Query,
    /// The field holding each event's time; the data view's time field otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// A field holding each event's end, to mark a range (e.g. an incident
    /// from opened to resolved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// A field shown with each event on hover, e.g. the team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// At most this many events (the most recent).
    #[serde(
        default = "annotation_limit",
        skip_serializing_if = "is_annotation_limit"
    )]
    pub limit: u32,
    /// Fixed events instead of a search, e.g. a release freeze.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AnnotationEvent>,
    /// The dashboard's query, filter pills and control selections apply
    /// unless this is set (Kibana ignores them by default).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_global_filters: bool,
    /// CSS colour; the theme's warning colour otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: LineStyle,
}

fn annotation_limit() -> u32 {
    500
}

fn is_annotation_limit(v: &u32) -> bool {
    *v == annotation_limit()
}

/// A fixed event: a moment, or a range when `to` is set. ISO 8601 times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct AnnotationEvent {
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
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
    /// Events marked on this chart only (Lens annotation layers convert to
    /// these). Only charts with a time axis draw annotations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
    /// Parts of the source Teleplot does not draw. Shown on the panel and by
    /// `teleplot check`, never dropped silently.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Chart {
    /// Whether the chart has a time x axis, and so can show annotations.
    pub fn has_time_axis(&self) -> bool {
        self.layers.iter().any(|l| {
            let is_date = |id: &str| {
                matches!(
                    l.column(id).map(|c| &c.op),
                    Some(Operation::DateHistogram { .. })
                )
            };
            match &l.encoding {
                // An ES|QL column's type is known only from its results:
                // annotations are fetched, and drawn if it holds dates.
                Encoding::Xy { x: Some(_), .. } if l.esql.is_some() => true,
                Encoding::Xy { x: Some(x), .. } | Encoding::Band { x, .. } => is_date(x),
                Encoding::Heatmap { x, .. } => self.kind == ChartKind::Heatmap && is_date(x),
                Encoding::Scatter { x, .. } => matches!(
                    l.column(x).map(|c| &c.op),
                    Some(Operation::Value { field }) if l.data_view.time_field.as_ref() == Some(field)
                ),
                _ => false,
            }
        })
    }
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
    /// A grid of cells: one bucket column across, one down, coloured by a metric.
    Heatmap,
}

/// One query against one data view, and how its columns are drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Layer {
    pub id: String,
    pub data_view: DataView,
    /// Run an ES|QL query instead: one row per result row. Columns are then
    /// [`Operation::Value`]s naming result columns, and computed columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub esql: Option<Esql>,
    pub columns: Vec<Column>,
    pub encoding: Encoding,
    /// Fetch individual documents (one row each) instead of aggregating.
    /// Columns are then [`Operation::Value`]s and computed columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<Documents>,
}

/// An ES|QL query (`POST /_query`). The dashboard's time range (on the data
/// view's time field), query, filter pills and control selections narrow
/// the documents it reads, as in Kibana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Esql {
    /// E.g. `FROM delivery-prs | STATS merged = COUNT(*) BY team`. It can use
    /// `?_tstart` and `?_tend`, the time range's start and end.
    pub query: String,
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
        /// Draw a bar of the value against `max` (Kibana's progress bar).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        progress: bool,
        /// `vertical` (up the tile's left edge, Kibana's default) or `horizontal`.
        #[serde(default, skip_serializing_if = "is_default")]
        progress_direction: ProgressDirection,
        /// Show `secondary` as a trend badge (Kibana 9.x "secondary trend").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        badge: Option<TrendBadge>,
    },
    /// A small line over time, drawn inside another layer's tiles or table
    /// rows rather than on its own: behind a metric tile (matched by
    /// `by` to the tile's breakdown), or in a table's sparkline column
    /// (matched to each row's first column).
    Sparkline {
        x: String,
        y: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    Pie {
        groups: Vec<String>,
        values: Vec<String>,
    },
    Table {
        columns: Vec<String>,
        /// Per-column colour by value and summary row.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        styles: Vec<ColumnStyle>,
        /// A last column of sparklines, from this chart's sparkline layer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sparkline: Option<TableSparkline>,
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
        /// Percentiles of `y` drawn as lines, e.g. `[50, 85, 95]` for service
        /// level expectations. Computed over the points shown (nearest rank).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        percentiles: Vec<f64>,
    },
    /// Cells coloured by `value`, `x` buckets across and `y` buckets up, e.g.
    /// PR cycle time (log buckets) per week.
    Heatmap {
        x: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<String>,
        value: String,
    },
    /// Lines at fixed values across an XY chart, e.g. a target or a
    /// percentile. Each value is one of this layer's unbucketed columns.
    ReferenceLines { lines: Vec<ReferenceLine> },
}

/// How a metric's secondary value is shown as a trend: an arrow (and the
/// difference) coloured by whether the change is good.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct TrendBadge {
    /// Which direction is good: `up` (green when rising), `down` (green
    /// when falling, e.g. lead time) or `neither` (no colour).
    #[serde(default)]
    pub better: Better,
    /// A fixed number to compare the secondary value with. Without one,
    /// it is compared with the primary value, and the badge shows primary
    /// − secondary, signed (e.g. this period against the previous one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    /// `icon`, `value` or `both` (the default).
    #[serde(default)]
    pub show: BadgeShow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Better {
    #[default]
    Up,
    Down,
    Neither,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProgressDirection {
    #[default]
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BadgeShow {
    Icon,
    Value,
    #[default]
    Both,
}

/// A table column's colour by value and summary row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct ColumnStyle {
    pub column: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorByValue>,
    /// A summary row value: `sum`, `avg`, `count`, `min` or `max` of the
    /// column's non-empty values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Summary {
    Sum,
    Avg,
    Count,
    Min,
    Max,
}

/// Colour cells by their value: each stop's colour applies from its `from`
/// up to the next stop (Kibana's palette stops).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct ColorByValue {
    /// `cell` (background), `text` or `badge`.
    #[serde(default)]
    pub mode: ColorMode,
    pub stops: Vec<ColorStop>,
    /// Stops are percentages of the column's range (min to max over the
    /// rows) rather than values.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub percent: bool,
    /// Values below the first stop take its colour (otherwise none).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub extend_below: bool,
    /// Values above the last band's end (`to`) take the last colour.
    #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
    pub extend_above: bool,
    /// Where the last band ends, when `extend_above` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct ColorStop {
    pub from: f64,
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    #[default]
    Cell,
    Text,
    Badge,
}

impl ColorByValue {
    /// The colour for `v`, given the column's range (for percent stops).
    pub fn color_for(&self, v: f64, min: f64, max: f64) -> Option<&str> {
        let at = |stop: f64| {
            if self.percent {
                min + (max - min) * stop / 100.0
            } else {
                stop
            }
        };
        let first = self.stops.first()?;
        if v < at(first.from) {
            return self.extend_below.then_some(first.color.as_str());
        }
        if !self.extend_above
            && let Some(to) = self.to
            && v > at(to)
        {
            return None;
        }
        self.stops
            .iter()
            .rev()
            .find(|s| v >= at(s.from))
            .map(|s| s.color.as_str())
    }
}

impl Summary {
    /// Kibana's summary row: over the non-empty values; `None` when there
    /// are none (and for min/max/avg of nothing).
    pub fn of(self, values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            return (self == Summary::Count).then_some(0.0);
        }
        let sum: f64 = values.iter().sum();
        Some(match self {
            Summary::Sum => sum,
            Summary::Avg => sum / values.len() as f64,
            Summary::Count => values.len() as f64,
            Summary::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
            Summary::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        })
    }

    /// Kibana's default label.
    pub fn label(self) -> &'static str {
        match self {
            Summary::Sum => "Sum",
            Summary::Avg => "Average",
            Summary::Count => "Value count",
            Summary::Min => "Minimum",
            Summary::Max => "Maximum",
        }
    }
}

/// A sparkline column in a table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct TableSparkline {
    /// The chart's sparkline layer.
    pub layer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct ReferenceLine {
    /// The column holding the value (a metric, static value or formula).
    pub column: String,
    /// `left` and `right` are value axes (a horizontal line); `bottom` is
    /// the x axis (a vertical line).
    #[serde(default, skip_serializing_if = "is_default")]
    pub axis: ReferenceAxis,
    /// CSS colour; the theme's text colour otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub style: LineStyle,
    /// Shade the area above or below the line.
    #[serde(default, skip_serializing_if = "is_default")]
    pub fill: ReferenceFill,
    /// Show the column's label beside the line.
    #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
    pub show_label: bool,
}

fn yes() -> bool {
    true
}

fn three() -> u32 {
    3
}

/// The bounds of a [`Operation::LogHistogram`], ascending: from `min` up to
/// the first bound at or above `max`. Bucket `i` runs from bound `i` to bound
/// `i + 1`. Empty if `min` is not positive or `max` is not above it.
pub fn log_bounds(min: f64, max: f64, per_decade: u32) -> Vec<f64> {
    if !(min > 0.0 && max > min && min.is_finite() && max.is_finite()) {
        return Vec::new();
    }
    let per_decade = per_decade.clamp(1, 20);
    let mantissas: Vec<f64> = match per_decade {
        1 => vec![1.0],
        2 => vec![1.0, 3.0],
        3 => vec![1.0, 2.0, 5.0],
        n => (0..n)
            .map(|k| round_to(10f64.powf(k as f64 / n as f64), 2))
            .collect(),
    };
    // Start at the largest round bound at or below `min`.
    let mut decade = 10f64.powf(min.log10().floor());
    let mut start = 0;
    for (i, m) in mantissas.iter().enumerate() {
        if m * decade <= min * (1.0 + 1e-9) {
            start = i;
        }
    }
    let mut out = Vec::new();
    let mut i = start;
    loop {
        let b = round_to(mantissas[i] * decade, 12);
        out.push(b);
        if b >= max * (1.0 - 1e-9) || out.len() > 400 {
            return out;
        }
        i += 1;
        if i == mantissas.len() {
            i = 0;
            decade *= 10.0;
        }
    }
}

/// `v` to `digits` significant digits (keeps bounds like 0.2 exact).
fn round_to(v: f64, digits: i32) -> f64 {
    if v == 0.0 {
        return 0.0;
    }
    let scale = 10f64.powi(digits - 1 - v.abs().log10().floor() as i32);
    (v * scale).round() / scale
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReferenceAxis {
    #[default]
    Left,
    Right,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LineStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ReferenceFill {
    #[default]
    None,
    Above,
    Below,
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
                ..
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
            Encoding::Table { columns, .. } => ids.extend(columns.iter().map(String::as_str)),
            Encoding::Sparkline { x, y, by } => {
                ids.extend([x.as_str(), y.as_str()]);
                ids.extend(by.as_deref());
            }
            Encoding::Band { x, low, mid, high } => {
                ids.extend([x, low, mid, high].map(String::as_str))
            }
            Encoding::Scatter {
                x, y, split, trend, ..
            } => {
                ids.extend([x.as_str(), y.as_str()]);
                ids.extend([split, trend].into_iter().flatten().map(String::as_str));
            }
            Encoding::ReferenceLines { lines } => {
                ids.extend(lines.iter().map(|l| l.column.as_str()))
            }
            Encoding::Heatmap { x, y, value } => {
                ids.push(x);
                ids.extend(y.as_deref());
                ids.push(value);
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
    /// Kibana's "normalize by unit": the value per this unit of time, scaled
    /// by the length of its date histogram bucket (the whole time range when
    /// there is none). Partial buckets at the range's edges count only the
    /// part inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per: Option<PerUnit>,
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

/// The unit a [`Column::per`] value is normalized to. Kibana offers second
/// to day; week is Teleplot's, for DORA-style "per week" rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PerUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
}

impl Column {
    /// The format to show values in: [`Column::format`], with Kibana's unit
    /// suffix (`/d`) when the column is normalized by unit.
    pub fn display_format(&self) -> Option<Format> {
        let Some(per) = self.per else {
            return self.format.clone();
        };
        let mut format = self.format.clone().unwrap_or(Format {
            kind: FormatKind::Number,
            decimals: None,
            suffix: None,
            compact: false,
        });
        format.suffix = Some(format!(
            "{}/{}",
            format.suffix.unwrap_or_default(),
            per.suffix()
        ));
        Some(format)
    }
}

impl PerUnit {
    fn suffix(self) -> &'static str {
        match self {
            PerUnit::Second => "s",
            PerUnit::Minute => "m",
            PerUnit::Hour => "h",
            PerUnit::Day => "d",
            PerUnit::Week => "w",
        }
    }

    pub fn ms(self) -> f64 {
        match self {
            PerUnit::Second => 1e3,
            PerUnit::Minute => 60e3,
            PerUnit::Hour => 3_600e3,
            PerUnit::Day => 86_400e3,
            PerUnit::Week => 604_800e3,
        }
    }
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
    /// Log-spaced buckets from `min` to `max`, for long-tailed durations.
    /// Values below `min` count in the first bucket and above `max` in the
    /// last. Keys are each bucket's lower bound; see [`log_bounds`].
    LogHistogram {
        field: String,
        min: f64,
        max: f64,
        /// Buckets per factor of ten: 1, 2 or 3 give round bounds (1, 3 or
        /// 1, 2, 5); more give evenly spaced ones. 3 by default.
        #[serde(default = "three")]
        per_decade: u32,
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
                | Operation::LogHistogram { .. }
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

#[cfg(test)]
mod tests {
    use super::log_bounds;

    #[test]
    fn log_bounds_are_round_for_up_to_three_per_decade() {
        assert_eq!(
            log_bounds(2.0, 2000.0, 3),
            [
                2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0
            ]
        );
        assert_eq!(log_bounds(1.0, 100.0, 1), [1.0, 10.0, 100.0]);
        assert_eq!(log_bounds(0.5, 40.0, 2), [0.3, 1.0, 3.0, 10.0, 30.0, 100.0]);
        // `min` between round bounds starts at the one below it.
        assert_eq!(log_bounds(3.0, 10.0, 3), [2.0, 5.0, 10.0]);
    }

    #[test]
    fn log_bounds_with_more_per_decade_are_evenly_spaced() {
        let b = log_bounds(1.0, 10.0, 4);
        assert_eq!(b, [1.0, 1.8, 3.2, 5.6, 10.0]);
    }

    #[test]
    fn log_bounds_need_a_positive_range() {
        assert!(log_bounds(0.0, 10.0, 3).is_empty());
        assert!(log_bounds(5.0, 5.0, 3).is_empty());
        assert!(log_bounds(-1.0, 10.0, 3).is_empty());
    }

    use super::{ColorByValue, ColorMode, ColorStop, Summary};

    fn palette(percent: bool, below: bool, above: bool, to: Option<f64>) -> ColorByValue {
        ColorByValue {
            mode: ColorMode::Cell,
            stops: vec![
                ColorStop {
                    from: 24.0,
                    color: "green".into(),
                },
                ColorStop {
                    from: 168.0,
                    color: "orange".into(),
                },
                ColorStop {
                    from: 720.0,
                    color: "red".into(),
                },
            ],
            percent,
            extend_below: below,
            extend_above: above,
            to,
        }
    }

    #[test]
    fn colours_by_value_follow_the_stops() {
        let p = palette(false, false, true, None);
        assert_eq!(p.color_for(10.0, 0.0, 0.0), None, "below the first stop");
        assert_eq!(p.color_for(24.0, 0.0, 0.0), Some("green"));
        assert_eq!(p.color_for(500.0, 0.0, 0.0), Some("orange"));
        assert_eq!(p.color_for(5000.0, 0.0, 0.0), Some("red"));
        let p = palette(false, true, false, Some(1000.0));
        assert_eq!(p.color_for(10.0, 0.0, 0.0), Some("green"), "extends below");
        assert_eq!(p.color_for(5000.0, 0.0, 0.0), None, "past the end");
    }

    #[test]
    fn percent_stops_are_shares_of_the_columns_range() {
        let mut p = palette(true, false, true, None);
        p.stops = vec![
            ColorStop {
                from: 0.0,
                color: "low".into(),
            },
            ColorStop {
                from: 50.0,
                color: "high".into(),
            },
        ];
        assert_eq!(p.color_for(10.0, 0.0, 200.0), Some("low"));
        assert_eq!(p.color_for(100.0, 0.0, 200.0), Some("high"), "50% of 0–200");
    }

    #[test]
    fn summaries_cover_the_non_empty_values() {
        let values = [3.0, 1.0, 2.0];
        assert_eq!(Summary::Sum.of(&values), Some(6.0));
        assert_eq!(Summary::Avg.of(&values), Some(2.0));
        assert_eq!(Summary::Count.of(&values), Some(3.0));
        assert_eq!(Summary::Min.of(&values), Some(1.0));
        assert_eq!(Summary::Max.of(&values), Some(3.0));
        assert_eq!(Summary::Avg.of(&[]), None);
        assert_eq!(Summary::Count.of(&[]), Some(0.0));
    }
}
