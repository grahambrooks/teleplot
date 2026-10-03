//! Turning layer tables into chart series.

use crate::model::{
    AnnotationData, Cell, Chart, Column, Encoding, Format, Layer, LayerData, LineStyle, Operation,
    PanelData, ReferenceAxis, ReferenceFill, SeriesType, Threshold,
};

/// What an XY chart draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Xy {
    pub x: XAxis,
    pub series: Vec<XySeries>,
    /// Benchmark bands to shade behind the left axis.
    pub thresholds: Vec<Threshold>,
    /// Per x position: too few documents to trust (shaded on the chart).
    pub low_sample: Vec<bool>,
    /// A filled band between two series (indices into `series`).
    pub band: Option<(usize, usize)>,
    pub log_scale: bool,
    pub reference_lines: Vec<RefLine>,
    /// Log-histogram x axes: where each bucket ends (it starts at its x).
    /// Bars are drawn as bins between the two, on a log x scale.
    pub bin_ends: Vec<f64>,
    /// How x values read, when they are numbers in a unit (log histograms).
    pub x_format: Option<Format>,
    /// Events to mark (time axes only).
    pub annotations: Vec<AnnotationData>,
}

/// A reference line with its value resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct RefLine {
    pub value: f64,
    pub label: Option<String>,
    pub axis: ReferenceAxis,
    pub color: Option<String>,
    pub style: LineStyle,
    pub fill: ReferenceFill,
    pub format: Option<Format>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum XAxis {
    /// Epoch milliseconds, ascending.
    Time(Vec<f64>),
    /// Numbers (histogram buckets), ascending.
    Numeric(Vec<f64>),
    /// Categories (terms, filters, ranges, or a single unbucketed point), in table order.
    Category(Vec<String>),
}

impl XAxis {
    pub fn len(&self) -> usize {
        match self {
            XAxis::Time(v) | XAxis::Numeric(v) => v.len(),
            XAxis::Category(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct XySeries {
    pub label: String,
    pub kind: SeriesType,
    pub values: Vec<Option<f64>>,
    pub format: Option<Format>,
    pub style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    #[default]
    Normal,
    /// Points only (scatter).
    Points,
    /// An edge of a filled band: no line of its own.
    BandEdge,
}

impl SeriesType {
    pub fn is_bar(self) -> bool {
        matches!(
            self,
            SeriesType::Bar
                | SeriesType::BarStacked
                | SeriesType::BarPercentageStacked
                | SeriesType::BarHorizontal
                | SeriesType::BarHorizontalStacked
                | SeriesType::BarHorizontalPercentageStacked
        )
    }

    pub fn is_horizontal(self) -> bool {
        matches!(
            self,
            SeriesType::BarHorizontal
                | SeriesType::BarHorizontalStacked
                | SeriesType::BarHorizontalPercentageStacked
        )
    }

    pub fn is_stacked(self) -> bool {
        matches!(
            self,
            SeriesType::AreaStacked
                | SeriesType::AreaPercentageStacked
                | SeriesType::BarStacked
                | SeriesType::BarPercentageStacked
                | SeriesType::BarHorizontalStacked
                | SeriesType::BarHorizontalPercentageStacked
        )
    }

    pub fn is_percentage(self) -> bool {
        matches!(
            self,
            SeriesType::AreaPercentageStacked
                | SeriesType::BarPercentageStacked
                | SeriesType::BarHorizontalPercentageStacked
        )
    }
}

fn label_of(chart: &Chart, layer: usize, column: &str) -> String {
    chart.layers[layer]
        .column(column)
        .and_then(|c| c.label.clone())
        .unwrap_or_else(|| column.to_string())
}

pub fn cell_text(cell: &Cell) -> String {
    match cell {
        Cell::Text(t) => t.clone(),
        Cell::Number(n) => crate::ui::format::plain(*n),
        Cell::Null => "(empty)".into(),
    }
}

/// Pivots every XY layer into series over one shared x axis.
pub fn xy(chart: &Chart, data: &PanelData, min_samples: Option<u32>) -> Xy {
    // The x axis kind comes from the first layer with an x column. An ES|QL
    // column's kind comes from its values: dates, numbers or text.
    let x_kind = chart
        .layers
        .iter()
        .zip(&data.layers)
        .find_map(|(l, d)| match &l.encoding {
            Encoding::Xy { x: Some(x), .. } => l.column(x).map(|c| match c.op {
                Operation::DateHistogram { .. } => 't',
                Operation::Histogram { .. } | Operation::LogHistogram { .. } => 'n',
                Operation::Value { .. } if d.dates.contains(x) => 't',
                Operation::Value { .. }
                    if l.esql.is_some()
                        && d.series(x).is_some_and(|s| {
                            s.values
                                .iter()
                                .all(|v| matches!(v, Cell::Number(_) | Cell::Null))
                        }) =>
                {
                    'n'
                }
                _ => 'c',
            }),
            _ => None,
        });

    // Gather (x key, series key, value) triples per layer.
    struct Point {
        x: Cell,
        series: usize,
        value: Option<f64>,
        count: Option<f64>,
        /// (layer, row) — a row's documents count once.
        row: Option<(usize, usize)>,
    }
    let mut series: Vec<XySeries> = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    for (i, (layer, layer_data)) in chart.layers.iter().zip(&data.layers).enumerate() {
        let Encoding::Xy {
            series: kind,
            x,
            y,
            split,
        } = &layer.encoding
        else {
            continue;
        };
        if layer_data.error.is_some() {
            continue;
        }
        let column = |id: &str| layer_data.series(id).map(|s| s.values.as_slice());
        let xs: Vec<Cell> = match x.as_deref().and_then(column) {
            Some(values) => values.to_vec(),
            None => vec![Cell::Text(String::new()); layer_data.rows],
        };
        let splits: Option<&[Cell]> = split.as_deref().and_then(column);
        for y_id in y {
            let Some(ys) = column(y_id) else { continue };
            let format = layer.column(y_id).and_then(|c| c.display_format());
            for row in 0..layer_data.rows {
                let label = match splits {
                    Some(s) if y.len() > 1 => {
                        format!("{} › {}", cell_text(&s[row]), label_of(chart, i, y_id))
                    }
                    Some(s) => cell_text(&s[row]),
                    None => label_of(chart, i, y_id),
                };
                let index = match series.iter().position(|s| s.label == label) {
                    Some(n) => n,
                    None => {
                        series.push(XySeries {
                            label,
                            kind: *kind,
                            values: Vec::new(),
                            format: format.clone(),
                            style: Style::Normal,
                        });
                        series.len() - 1
                    }
                };
                points.push(Point {
                    x: xs[row].clone(),
                    series: index,
                    value: ys[row].number(),
                    count: layer_data.counts.get(row).copied(),
                    row: Some((i, row)),
                });
            }
        }
    }

    let x = match x_kind {
        Some('t') | Some('n') => {
            let mut keys: Vec<f64> = points.iter().filter_map(|p| p.x.number()).collect();
            keys.sort_by(f64::total_cmp);
            keys.dedup();
            if x_kind == Some('t') {
                XAxis::Time(keys)
            } else {
                XAxis::Numeric(keys)
            }
        }
        _ => {
            let mut keys: Vec<String> = Vec::new();
            for p in &points {
                let key = cell_text(&p.x);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            XAxis::Category(keys)
        }
    };

    for s in &mut series {
        s.values = vec![None; x.len()];
    }
    // A bucket's sample is everything in it, across its breakdown (and
    // counted once per row, not once per y column).
    let mut totals: Vec<Option<f64>> = vec![None; x.len()];
    let mut counted = std::collections::HashSet::new();
    for p in points {
        let slot = match &x {
            XAxis::Time(keys) | XAxis::Numeric(keys) => {
                p.x.number().and_then(|v| keys.iter().position(|k| *k == v))
            }
            XAxis::Category(keys) => keys.iter().position(|k| *k == cell_text(&p.x)),
        };
        if let Some(slot) = slot {
            series[p.series].values[slot] = p.value;
            if let (Some(n), Some(row)) = (p.count, p.row)
                && counted.insert(row)
            {
                *totals[slot].get_or_insert(0.0) += n;
            }
        }
    }
    let low_sample = totals
        .iter()
        .map(|t| matches!((min_samples, t), (Some(min), Some(n)) if *n < min as f64))
        .collect();
    let thresholds = chart
        .layers
        .iter()
        .find_map(|l| match &l.encoding {
            Encoding::Xy { y, .. } => y
                .iter()
                .filter_map(|id| l.column(id))
                .find(|c| !c.thresholds.is_empty()),
            _ => None,
        })
        .map(|c| c.thresholds.clone())
        .unwrap_or_default();
    let reference_lines = reference_lines(chart, data, &series);
    let x_format = chart.layers.iter().find_map(|l| match &l.encoding {
        Encoding::Xy { x: Some(x), .. } => l
            .column(x)
            .filter(|c| matches!(c.op, Operation::LogHistogram { .. }))
            .and_then(|c| c.display_format()),
        _ => None,
    });
    let bin_ends = match (&x, log_histogram(chart)) {
        (XAxis::Numeric(keys), Some(bounds)) => keys
            .iter()
            .map(|k| {
                let next = bounds.iter().position(|b| b == k).map(|i| i + 1);
                next.and_then(|i| bounds.get(i)).copied().unwrap_or(*k)
            })
            .collect(),
        _ => Vec::new(),
    };
    let annotations = match x {
        XAxis::Time(_) => data.annotations.clone(),
        _ => Vec::new(),
    };
    // A bin's height is its document count: shading small ones adds nothing.
    let low_sample = if bin_ends.is_empty() {
        low_sample
    } else {
        vec![false; x.len()]
    };
    Xy {
        x,
        series,
        thresholds,
        low_sample,
        band: None,
        log_scale: false,
        reference_lines,
        bin_ends,
        x_format,
        annotations,
    }
}

/// The bounds of the x axis's log histogram, when the chart has one.
fn log_histogram(chart: &Chart) -> Option<Vec<f64>> {
    chart.layers.iter().find_map(|l| match &l.encoding {
        Encoding::Xy { x: Some(x), .. } => match l.column(x).map(|c| &c.op) {
            Some(Operation::LogHistogram {
                min,
                max,
                per_decade,
                ..
            }) => Some(crate::model::log_bounds(*min, *max, *per_decade)),
            _ => None,
        },
        _ => None,
    })
}

/// The reference lines of every reference-line layer, with their values.
/// Lines whose value is missing (no data, a failed query) are left out. A
/// line without a format of its own takes the chart's (as in Lens).
pub fn reference_lines(chart: &Chart, data: &PanelData, series: &[XySeries]) -> Vec<RefLine> {
    let fallback = series.iter().find_map(|s| s.format.clone());
    let mut out = Vec::new();
    for (layer, d) in chart.layers.iter().zip(&data.layers) {
        let Encoding::ReferenceLines { lines } = &layer.encoding else {
            continue;
        };
        for line in lines {
            let Some(value) = first(d, &line.column).and_then(|c| c.number()) else {
                continue;
            };
            let column = layer.column(&line.column);
            out.push(RefLine {
                value,
                label: line.show_label.then(|| label(column, &line.column)),
                axis: line.axis,
                color: line.color.clone(),
                style: line.style,
                fill: line.fill,
                format: column
                    .and_then(|c| c.display_format())
                    .or_else(|| fallback.clone()),
            });
        }
    }
    out
}

/// The layer that decides how a chart is drawn: the first that draws data.
pub fn main_layer(chart: &Chart) -> Option<&Layer> {
    chart.layers.iter().find(|l| draws_itself(l))
}

/// Reference lines and sparklines are drawn inside another layer's chart.
fn draws_itself(layer: &Layer) -> bool {
    !matches!(
        layer.encoding,
        Encoding::ReferenceLines { .. } | Encoding::Sparkline { .. }
    )
}

/// A chart's sparkline points, `(time, value)` oldest first, by the value
/// of the layer's `by` column (`""` when it has none). Empty when the chart
/// has no sparkline layer.
pub fn sparklines(
    chart: &Chart,
    data: &PanelData,
) -> std::collections::HashMap<String, Vec<(f64, f64)>> {
    let mut out: std::collections::HashMap<String, Vec<(f64, f64)>> = Default::default();
    for (layer, d) in chart.layers.iter().zip(&data.layers) {
        let Encoding::Sparkline { x, y, by } = &layer.encoding else {
            continue;
        };
        let (Some(xs), Some(ys)) = (d.series(x), d.series(y)) else {
            continue;
        };
        let keys = by.as_deref().and_then(|b| d.series(b));
        for row in 0..d.rows {
            let (Some(t), Some(v)) = (xs.values[row].number(), ys.values[row].number()) else {
                continue;
            };
            let key = keys.map_or(String::new(), |k| cell_text(&k.values[row]));
            out.entry(key).or_default().push((t, v));
        }
    }
    for points in out.values_mut() {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    out
}

/// How a trend badge reads: the arrow, the text, and whether the change is good.
#[derive(Debug, Clone, PartialEq)]
pub struct BadgeView {
    pub arrow: Option<&'static str>,
    pub text: Option<String>,
    pub tone: Tone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Bad,
    Flat,
}

/// Kibana's secondary trend: the change is secondary − baseline, or, when
/// compared with the primary value, primary − secondary (shown signed). Its
/// sign picks the arrow; `better` says whether that direction is good.
pub fn trend_badge(
    badge: &crate::model::TrendBadge,
    primary: Option<f64>,
    secondary: Option<f64>,
    format: Option<&Format>,
) -> BadgeView {
    use crate::model::{BadgeShow, Better};
    let to_primary = badge.baseline.is_none();
    let delta = match (to_primary, primary, secondary, badge.baseline) {
        (true, Some(p), Some(s), _) => p - s,
        (false, _, Some(s), Some(b)) => s - b,
        _ => f64::NAN,
    };
    let (arrow, tone) = if delta.is_nan() {
        (None, Tone::Flat)
    } else if delta == 0.0 {
        (Some("="), Tone::Flat)
    } else {
        let up = delta > 0.0;
        let tone = match (badge.better, up) {
            (Better::Neither, _) => Tone::Flat,
            (Better::Up, true) | (Better::Down, false) => Tone::Good,
            _ => Tone::Bad,
        };
        (Some(if up { "↑" } else { "↓" }), tone)
    };
    let text = match badge.show {
        BadgeShow::Icon => None,
        _ if delta.is_nan() && to_primary => Some("N/A".to_string()),
        _ if to_primary => {
            let shown = crate::ui::format::value(delta.abs(), format);
            Some(format!("{}{shown}", if delta < 0.0 { "−" } else { "+" }))
        }
        _ => secondary.map(|s| crate::ui::format::value(s, format)),
    };
    BadgeView {
        arrow: (badge.show != BadgeShow::Value).then_some(arrow).flatten(),
        text,
        tone,
    }
}

/// An SVG path for a sparkline in a `width` × `height` box: the line, or
/// the area under it when `area` is set. `None` with fewer than two points.
pub fn sparkline_path(
    points: &[(f64, f64)],
    width: f64,
    height: f64,
    area: bool,
) -> Option<String> {
    if points.len() < 2 {
        return None;
    }
    let (t0, t1) = (points[0].0, points[points.len() - 1].0);
    let lo = points
        .iter()
        .map(|p| p.1)
        .fold(f64::INFINITY, f64::min)
        .min(0.0);
    let hi = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let x = |t: f64| {
        if t1 > t0 {
            (t - t0) / (t1 - t0) * width
        } else {
            0.0
        }
    };
    let y = |v: f64| {
        if hi > lo {
            height - (v - lo) / (hi - lo) * height
        } else {
            height / 2.0
        }
    };
    let mut d = String::new();
    for (i, (t, v)) in points.iter().enumerate() {
        d.push_str(&format!(
            "{}{:.2},{:.2} ",
            if i == 0 { "M" } else { "L" },
            x(*t),
            y(*v)
        ));
    }
    if area {
        d.push_str(&format!("L{width:.2},{height:.2} L0,{height:.2} Z"));
    }
    Some(d.trim_end().to_string())
}

fn label(column: Option<&Column>, fallback: &str) -> String {
    column
        .and_then(|c| c.label.clone())
        .unwrap_or_else(|| fallback.to_string())
}

fn numbers(layer: &LayerData, id: &str) -> Vec<Option<f64>> {
    layer
        .series(id)
        .map(|s| s.values.iter().map(Cell::number).collect())
        .unwrap_or_default()
}

/// A median line inside a shaded low–high band, e.g. p25–p75.
pub fn band(chart: &Chart, data: &PanelData, min_samples: Option<u32>) -> Xy {
    let Some((layer, d)) = main_layer_data(chart, data) else {
        return empty();
    };
    let Encoding::Band { x, low, mid, high } = &layer.encoding else {
        return empty();
    };
    let xs: Vec<f64> = numbers(d, x)
        .into_iter()
        .map(|v| v.unwrap_or_default())
        .collect();
    let series = |id: &str, style| {
        let column = layer.column(id);
        XySeries {
            label: label(column, id),
            kind: SeriesType::Line,
            values: numbers(d, id),
            format: column.and_then(|c| c.display_format()),
            style,
        }
    };
    let low_sample = match min_samples {
        Some(min) => d.counts.iter().map(|n| *n < min as f64).collect(),
        None => vec![false; xs.len()],
    };
    let mut xy = Xy {
        x: XAxis::Time(xs),
        series: vec![
            series(low, Style::BandEdge),
            series(mid, Style::Normal),
            series(high, Style::BandEdge),
        ],
        thresholds: layer
            .column(mid)
            .map(|c| c.thresholds.clone())
            .unwrap_or_default(),
        low_sample,
        band: Some((0, 2)),
        log_scale: false,
        reference_lines: Vec::new(),
        bin_ends: Vec::new(),
        x_format: None,
        annotations: Vec::new(),
    };
    xy.reference_lines = reference_lines(chart, data, &xy.series);
    xy.annotations = data.annotations.clone();
    xy
}

/// One point per document, with an optional trend line. Points are split
/// into one series per `split` value when given.
pub fn scatter(chart: &Chart, data: &PanelData) -> Xy {
    let Some((layer, d)) = main_layer_data(chart, data) else {
        return empty();
    };
    let Encoding::Scatter {
        x,
        y,
        split,
        trend,
        log_scale,
        percentiles,
    } = &layer.encoding
    else {
        return empty();
    };
    let xs: Vec<f64> = numbers(d, x)
        .into_iter()
        .map(|v| v.unwrap_or_default())
        .collect();
    let ys = numbers(d, y);
    let y_column = layer.column(y);
    let format = y_column.and_then(|c| c.display_format());
    let mut series: Vec<XySeries> = Vec::new();
    let groups: Vec<String> = match split.as_deref().and_then(|s| d.series(s)) {
        Some(s) => s.values.iter().map(cell_text).collect(),
        None => vec![label(y_column, y); xs.len()],
    };
    for (i, group) in groups.iter().enumerate() {
        let index = match series.iter().position(|s| s.label == *group) {
            Some(n) => n,
            None => {
                series.push(XySeries {
                    label: group.clone(),
                    kind: SeriesType::Line,
                    values: vec![None; xs.len()],
                    format: format.clone(),
                    style: Style::Points,
                });
                series.len() - 1
            }
        };
        series[index].values[i] = ys[i];
    }
    if let Some(t) = trend {
        series.push(XySeries {
            label: label(layer.column(t), t),
            kind: SeriesType::Line,
            values: numbers(d, t),
            format: format.clone(),
            style: Style::Normal,
        });
    }
    let time = matches!(layer.column(x).map(|c| &c.op), Some(Operation::Value { field }) if layer.data_view.time_field.as_ref() == Some(field));
    let n = xs.len();
    let mut xy = Xy {
        x: if time {
            XAxis::Time(xs)
        } else {
            XAxis::Numeric(xs)
        },
        series,
        thresholds: y_column.map(|c| c.thresholds.clone()).unwrap_or_default(),
        low_sample: vec![false; n],
        band: None,
        log_scale: *log_scale,
        reference_lines: Vec::new(),
        bin_ends: Vec::new(),
        x_format: None,
        annotations: Vec::new(),
    };
    xy.reference_lines = reference_lines(chart, data, &xy.series);
    if time {
        xy.annotations = data.annotations.clone();
    }
    // Percentiles of the points shown, e.g. "85% of PRs merged within …".
    let shown: Vec<f64> = ys.iter().flatten().copied().collect();
    for p in percentiles {
        if let Some(value) = percentile(&shown, *p) {
            xy.reference_lines.push(RefLine {
                value,
                label: Some(format!("p{}", crate::ui::format::plain(*p))),
                axis: ReferenceAxis::Left,
                color: None,
                style: LineStyle::Dashed,
                fill: ReferenceFill::None,
                format: format.clone(),
            });
        }
    }
    xy
}

/// The nearest-rank percentile: the smallest value with at least `p`% of
/// the values at or below it, so "85% within N" holds exactly for N.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() || !(p > 0.0 && p <= 100.0) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    // Rounded first, so 85% of 20 is rank 17 and not 18 (0.85 × 20 = 17.000000000000004).
    let rank = ((p / 100.0 * sorted.len() as f64 * 1e9).round() / 1e9).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// What a heatmap draws: cells over two bucket axes.
#[derive(Debug, Clone, PartialEq)]
pub struct Heatmap {
    /// Time buckets (starts, ascending) or categories, left to right.
    pub x: XAxis,
    /// When `x` is time: the end of each bucket.
    pub x_ends: Vec<f64>,
    /// Row labels, bottom to top.
    pub y: Vec<String>,
    /// For log-histogram rows: where each row's values start and end
    /// (`ny + 1` edges; `None` where a bucket is open-ended). Empty otherwise.
    pub y_edges: Vec<Option<f64>>,
    /// `cells[x][y]`.
    pub cells: Vec<Vec<Option<f64>>>,
    pub label: String,
    pub format: Option<Format>,
    /// Events to mark (time axes only).
    pub annotations: Vec<AnnotationData>,
}

impl Heatmap {
    /// The smallest and largest cell values.
    pub fn extent(&self) -> Option<(f64, f64)> {
        let values = self.cells.iter().flatten().flatten().copied();
        values.fold(None, |acc, v| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
    }
}

/// Distinct keys in first-seen order.
fn distinct(keys: &[Cell]) -> Vec<Cell> {
    let mut out: Vec<Cell> = Vec::new();
    for k in keys {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

fn by_number(keys: &mut [Cell]) {
    keys.sort_by(|a, b| {
        a.number()
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&b.number().unwrap_or(f64::NEG_INFINITY))
    });
}

fn is_numeric_bucket(column: Option<&Column>) -> bool {
    matches!(
        column.map(|c| &c.op),
        Some(
            Operation::DateHistogram { .. }
                | Operation::Histogram { .. }
                | Operation::LogHistogram { .. }
        )
    )
}

/// Pivots a heatmap layer. Numeric rows (histograms) run upwards in value
/// order; categories keep table order from the top down, as in Lens.
pub fn heatmap(chart: &Chart, data: &PanelData) -> Heatmap {
    let mut out = Heatmap {
        x: XAxis::Category(Vec::new()),
        x_ends: Vec::new(),
        y: Vec::new(),
        y_edges: Vec::new(),
        cells: Vec::new(),
        label: String::new(),
        format: None,
        annotations: Vec::new(),
    };
    let Some((layer, d)) = main_layer_data(chart, data) else {
        return out;
    };
    let Encoding::Heatmap { x, y, value } = &layer.encoding else {
        return out;
    };
    let value_column = layer.column(value);
    out.label = label(value_column, value);
    out.format = value_column.and_then(|c| c.display_format());
    let keys = |id: &str| d.series(id).map(|s| s.values.clone()).unwrap_or_default();
    let xs = keys(x);
    let ys = y.as_deref().map(keys);
    let values = numbers(d, value);

    let x_column = layer.column(x);
    let mut x_keys = distinct(&xs);
    if is_numeric_bucket(x_column) {
        by_number(&mut x_keys);
    }
    let y_column = y.as_deref().and_then(|id| layer.column(id));
    let mut y_keys = ys.as_deref().map(distinct).unwrap_or_default();
    if is_numeric_bucket(y_column) {
        by_number(&mut y_keys);
    } else {
        y_keys.reverse();
    }
    if ys.is_none() {
        y_keys.push(Cell::Text(out.label.clone()));
    }

    out.cells = vec![vec![None; y_keys.len()]; x_keys.len()];
    for row in 0..d.rows {
        let Some(i) = xs.get(row).and_then(|k| x_keys.iter().position(|x| x == k)) else {
            continue;
        };
        let j = match &ys {
            Some(ys) => match ys.get(row).and_then(|k| y_keys.iter().position(|y| y == k)) {
                Some(j) => j,
                None => continue,
            },
            None => 0,
        };
        out.cells[i][j] = values.get(row).copied().flatten();
    }
    out.y = y_keys.iter().map(|k| bucket_label(k, y_column)).collect();
    if let Some(Operation::LogHistogram {
        min,
        max,
        per_decade,
        ..
    }) = y_column.map(|c| &c.op)
    {
        let bounds = crate::model::log_bounds(*min, *max, *per_decade);
        // Rows are the buckets in order; the first and last are open.
        let n = y_keys.len();
        out.y_edges = (0..=n)
            .map(|j| (j > 0 && j < n).then(|| bounds.get(j).copied()).flatten())
            .collect();
        let format = y_column.and_then(|c| c.display_format());
        for (label, key) in out.y.iter_mut().zip(&y_keys) {
            if let Some(range) = key
                .number()
                .and_then(|k| log_range_label(k, &bounds, format.as_ref()))
            {
                *label = range;
            }
        }
    }
    if matches!(
        x_column.map(|c| &c.op),
        Some(Operation::DateHistogram { .. })
    ) {
        let starts: Vec<f64> = x_keys.iter().filter_map(Cell::number).collect();
        out.x_ends = bucket_ends(&starts, x_column);
        out.x = XAxis::Time(starts);
        out.annotations = data.annotations.clone();
    } else {
        out.x = XAxis::Category(x_keys.iter().map(|k| bucket_label(k, x_column)).collect());
    }
    out
}

/// A log-histogram bucket as a range, e.g. `5–10 h`. The first and last
/// buckets are open-ended (`< 5 h`, `≥ 1000 h`), as the search is.
pub fn log_range_label(start: f64, bounds: &[f64], format: Option<&Format>) -> Option<String> {
    use crate::ui::format::{plain, value};
    let i = bounds.iter().position(|b| *b == start)?;
    let last = bounds.len().checked_sub(2)?;
    Some(match i {
        0 if last == 0 => "all".into(),
        0 => format!("< {}", value(*bounds.get(1)?, format)),
        i if i == last => format!("≥ {}", value(start, format)),
        i => format!("{}–{}", plain(start), value(*bounds.get(i + 1)?, format)),
    })
}

/// A bucket key as an axis label: numbers in the column's format.
fn bucket_label(key: &Cell, column: Option<&Column>) -> String {
    match key {
        Cell::Number(n) => {
            crate::ui::format::value(*n, column.and_then(|c| c.display_format()).as_ref())
        }
        other => cell_text(other),
    }
}

/// Where each time bucket ends: the next one's start, unless buckets are
/// missing in between. A fixed interval is exact; calendar and auto
/// intervals are capped a little above the shortest gap (months vary).
fn bucket_ends(starts: &[f64], column: Option<&Column>) -> Vec<f64> {
    let fixed = match column.map(|c| &c.op) {
        Some(Operation::DateHistogram {
            interval: crate::model::Interval::Every { count, unit },
            ..
        }) => fixed_ms(*count, *unit),
        _ => None,
    };
    let shortest = starts
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(f64::INFINITY, f64::min);
    let shortest = if shortest.is_finite() {
        shortest
    } else {
        86_400_000.0
    };
    let cap = fixed.unwrap_or(shortest * 1.15);
    starts
        .iter()
        .enumerate()
        .map(|(i, s)| match starts.get(i + 1) {
            Some(next) if next - s <= cap => *next,
            _ => s + fixed.unwrap_or(shortest),
        })
        .collect()
}

/// The length of a fixed interval; `None` for months and longer.
fn fixed_ms(count: u32, unit: crate::model::IntervalUnit) -> Option<f64> {
    use crate::model::IntervalUnit as U;
    let ms = match unit {
        U::Millisecond => 1.0,
        U::Second => 1e3,
        U::Minute => 60e3,
        U::Hour => 3_600e3,
        U::Day => 86_400e3,
        U::Week => 604_800e3,
        _ => return None,
    };
    Some(count as f64 * ms)
}

fn main_layer_data<'a>(
    chart: &'a Chart,
    data: &'a PanelData,
) -> Option<(&'a Layer, &'a LayerData)> {
    chart
        .layers
        .iter()
        .zip(&data.layers)
        .find(|(l, _)| draws_itself(l))
}

fn empty() -> Xy {
    Xy {
        x: XAxis::Time(Vec::new()),
        series: Vec::new(),
        thresholds: Vec::new(),
        low_sample: Vec::new(),
        band: None,
        log_scale: false,
        reference_lines: Vec::new(),
        bin_ends: Vec::new(),
        x_format: None,
        annotations: Vec::new(),
    }
}

/// The benchmark band a value falls in.
pub fn threshold_for(thresholds: &[Threshold], v: f64) -> Option<(usize, &Threshold)> {
    thresholds.iter().enumerate().find(|(_, t)| t.contains(v))
}

/// Values as drawn: stacked series accumulate (in order), percentage
/// stacks are normalised to 0–1 per x. Unstacked series pass through.
pub fn stacked(xy: &Xy) -> Vec<Vec<Option<f64>>> {
    let n = xy.x.len();
    let mut totals = vec![0.0; n];
    for s in xy.series.iter().filter(|s| s.kind.is_percentage()) {
        for (i, v) in s.values.iter().enumerate() {
            totals[i] += v.unwrap_or(0.0);
        }
    }
    let mut running = vec![0.0; n];
    xy.series
        .iter()
        .map(|s| {
            if !s.kind.is_stacked() {
                return s.values.clone();
            }
            s.values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let v = v.unwrap_or(0.0);
                    let v = if s.kind.is_percentage() {
                        if totals[i] == 0.0 { 0.0 } else { v / totals[i] }
                    } else {
                        v
                    };
                    running[i] += v;
                    Some(running[i])
                })
                .collect()
        })
        .collect()
}

/// The first value of a column, for metric tiles.
pub fn first(layer: &LayerData, column: &str) -> Option<Cell> {
    layer.series(column).and_then(|s| s.values.first().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, DataView, Interval, Layer, Series, TermsOrder};

    fn col(id: &str, op: Operation) -> Column {
        Column {
            id: id.into(),
            label: Some(id.to_uppercase()),
            op,
            filter: None,
            time_shift: None,
            per: None,
            format: None,
            helper: false,
            thresholds: Vec::new(),
        }
    }

    fn chart(split: bool) -> Chart {
        let mut columns = vec![
            col(
                "x",
                Operation::DateHistogram {
                    field: "t".into(),
                    interval: Interval::Auto,
                    include_empty: true,
                },
            ),
            col("y", Operation::Count { field: None }),
        ];
        if split {
            columns.insert(
                0,
                col(
                    "s",
                    Operation::Terms {
                        fields: vec!["team".into()],
                        size: 3,
                        order: TermsOrder::Count,
                        other_bucket: false,
                        missing_bucket: false,
                    },
                ),
            );
        }
        Chart {
            kind: crate::model::ChartKind::Xy,
            query: Default::default(),
            filters: vec![],
            layers: vec![Layer {
                id: "l".into(),
                data_view: DataView {
                    index: "i".into(),
                    time_field: None,
                },
                columns,
                encoding: Encoding::Xy {
                    series: SeriesType::BarStacked,
                    x: Some("x".into()),
                    y: vec!["y".into()],
                    split: split.then(|| "s".into()),
                },
                documents: None,
                esql: None,
            }],
            annotations: vec![],
            warnings: vec![],
        }
    }

    fn data(columns: Vec<(&str, Vec<Cell>)>) -> PanelData {
        let rows = columns[0].1.len();
        PanelData {
            layers: vec![LayerData {
                layer: "l".into(),
                rows,
                counts: Vec::new(),
                columns: columns
                    .into_iter()
                    .map(|(id, values)| Series {
                        id: id.into(),
                        values,
                    })
                    .collect(),
                error: None,
                dates: Vec::new(),
            }],
            warnings: vec![],
            annotations: vec![],
        }
    }

    use Cell::{Number as N, Text as T};

    #[test]
    fn splits_pivot_into_one_series_each_over_a_shared_axis() {
        let d = data(vec![
            ("s", vec![T("a".into()), T("a".into()), T("b".into())]),
            ("x", vec![N(1.0), N(2.0), N(2.0)]),
            ("y", vec![N(10.0), N(20.0), N(5.0)]),
        ]);
        let xy = xy(&chart(true), &d, None);
        assert_eq!(xy.x, XAxis::Time(vec![1.0, 2.0]));
        assert_eq!(xy.series.len(), 2);
        assert_eq!(xy.series[0].label, "a");
        assert_eq!(xy.series[0].values, [Some(10.0), Some(20.0)]);
        assert_eq!(
            xy.series[1].values,
            [None, Some(5.0)],
            "b has no bucket at x=1"
        );
        assert_eq!(
            stacked(&xy),
            [vec![Some(10.0), Some(20.0)], vec![Some(10.0), Some(25.0)]]
        );
    }

    #[test]
    fn unsplit_series_use_the_column_label() {
        let d = data(vec![("x", vec![N(1.0)]), ("y", vec![N(3.0)])]);
        assert_eq!(xy(&chart(false), &d, None).series[0].label, "Y");
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&values, 50.0), Some(10.0));
        assert_eq!(percentile(&values, 85.0), Some(17.0), "17 of 20 are ≤ 17");
        assert_eq!(percentile(&values, 95.0), Some(19.0));
        assert_eq!(percentile(&values, 100.0), Some(20.0));
        assert_eq!(
            percentile(&[3.0, 1.0, 2.0], 1.0),
            Some(1.0),
            "unsorted input"
        );
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&values, 0.0), None);
    }

    fn heatmap_chart(y_op: Operation) -> Chart {
        let mut c = chart(false);
        let layer = &mut c.layers[0];
        layer.columns.insert(1, col("h", y_op));
        layer.encoding = Encoding::Heatmap {
            x: "x".into(),
            y: Some("h".into()),
            value: "y".into(),
        };
        c.kind = crate::model::ChartKind::Heatmap;
        c
    }

    #[test]
    fn heatmaps_pivot_rows_into_cells_with_numeric_rows_upwards() {
        let c = heatmap_chart(Operation::LogHistogram {
            field: "f".into(),
            min: 1.0,
            max: 100.0,
            per_decade: 1,
        });
        let d = data(vec![
            ("x", vec![N(2.0), N(2.0), N(1.0)]),
            ("h", vec![N(10.0), N(1.0), N(1.0)]),
            ("y", vec![N(4.0), N(5.0), N(6.0)]),
        ]);
        let hm = heatmap(&c, &d);
        assert_eq!(hm.x, XAxis::Time(vec![1.0, 2.0]));
        assert_eq!(
            hm.y,
            ["< 10", "≥ 10"],
            "bounds 1, 10, 100: two open buckets"
        );
        assert_eq!(
            hm.cells,
            [vec![Some(6.0), None], vec![Some(5.0), Some(4.0)]]
        );
        assert_eq!(hm.extent(), Some((4.0, 6.0)));
        assert_eq!(hm.label, "Y");
    }

    #[test]
    fn heatmap_categories_keep_table_order_from_the_top() {
        let c = heatmap_chart(Operation::Terms {
            fields: vec!["team".into()],
            size: 3,
            order: TermsOrder::Count,
            other_bucket: false,
            missing_bucket: false,
        });
        let d = data(vec![
            ("x", vec![N(1.0), N(1.0)]),
            ("h", vec![T("big".into()), T("small".into())]),
            ("y", vec![N(9.0), N(1.0)]),
        ]);
        let hm = heatmap(&c, &d);
        assert_eq!(hm.y, ["small", "big"], "bottom to top");
        assert_eq!(hm.cells, [vec![Some(1.0), Some(9.0)]]);
    }

    #[test]
    fn time_buckets_end_at_the_next_or_after_one_interval() {
        const WEEK: f64 = 604_800_000.0;
        let weekly = col(
            "x",
            Operation::DateHistogram {
                field: "t".into(),
                interval: "1w".parse().unwrap(),
                include_empty: false,
            },
        );
        // A missing week between the second and third.
        let starts = [0.0, WEEK, 3.0 * WEEK];
        assert_eq!(
            bucket_ends(&starts, Some(&weekly)),
            [WEEK, 2.0 * WEEK, 4.0 * WEEK]
        );
        // Auto intervals use the shortest gap.
        assert_eq!(bucket_ends(&starts, None), [WEEK, 2.0 * WEEK, 4.0 * WEEK]);
    }

    #[test]
    fn log_buckets_read_as_ranges() {
        let bounds = crate::model::log_bounds(2.0, 2000.0, 3);
        let hours = Some(Format {
            kind: crate::model::FormatKind::Number,
            decimals: None,
            suffix: Some(" h".into()),
            compact: false,
        });
        let label = |k| log_range_label(k, &bounds, hours.as_ref()).unwrap();
        assert_eq!(label(2.0), "< 5 h");
        assert_eq!(label(5.0), "5–10 h");
        assert_eq!(label(1000.0), "≥ 1,000 h");
        assert_eq!(log_range_label(3.0, &bounds, None), None);
    }

    #[test]
    fn log_histogram_bars_are_bins_to_the_next_bound() {
        let mut c = chart(false);
        c.layers[0].columns[0] = col(
            "x",
            Operation::LogHistogram {
                field: "f".into(),
                min: 1.0,
                max: 100.0,
                per_decade: 3,
            },
        );
        let d = data(vec![
            ("x", vec![N(1.0), N(2.0), N(50.0)]),
            ("y", vec![N(1.0); 3]),
        ]);
        let xy = xy(&c, &d, None);
        assert_eq!(xy.x, XAxis::Numeric(vec![1.0, 2.0, 50.0]));
        assert_eq!(xy.bin_ends, [2.0, 5.0, 100.0]);
    }

    #[test]
    fn trend_badges_follow_kibana() {
        use crate::model::{BadgeShow, Better, TrendBadge};
        let lead_time = TrendBadge {
            better: Better::Down,
            baseline: None,
            show: BadgeShow::Both,
        };
        // Compared with the primary: primary − secondary, signed. Lead time
        // fell from 24 to 20: down, which is good for lead time.
        let b = trend_badge(&lead_time, Some(20.0), Some(24.0), None);
        assert_eq!(
            (b.arrow, b.text.as_deref(), b.tone),
            (Some("↓"), Some("−4"), Tone::Good)
        );
        let b = trend_badge(&lead_time, Some(30.0), Some(24.0), None);
        assert_eq!((b.arrow, b.tone), (Some("↑"), Tone::Bad));
        let b = trend_badge(&lead_time, Some(24.0), Some(24.0), None);
        assert_eq!((b.arrow, b.tone), (Some("="), Tone::Flat));
        let b = trend_badge(&lead_time, Some(24.0), None, None);
        assert_eq!((b.arrow, b.text.as_deref()), (None, Some("N/A")));
        // Against a fixed baseline the badge shows the secondary value itself.
        let target = TrendBadge {
            better: Better::Up,
            baseline: Some(10.0),
            show: BadgeShow::Value,
        };
        let b = trend_badge(&target, Some(1.0), Some(12.0), None);
        assert_eq!(
            (b.arrow, b.text.as_deref(), b.tone),
            (None, Some("12"), Tone::Good)
        );
    }

    #[test]
    fn sparklines_group_by_key_and_scale_into_the_box() {
        let mut c = chart(true);
        c.kind = crate::model::ChartKind::Metric;
        c.layers[0].encoding = Encoding::Sparkline {
            x: "x".into(),
            y: "y".into(),
            by: Some("s".into()),
        };
        let d = data(vec![
            ("s", vec![T("a".into()), T("a".into()), T("b".into())]),
            ("x", vec![N(2.0), N(1.0), N(1.0)]),
            ("y", vec![N(10.0), N(5.0), N(7.0)]),
        ]);
        let lines = sparklines(&c, &d);
        assert_eq!(lines["a"], [(1.0, 5.0), (2.0, 10.0)], "oldest first");
        assert_eq!(lines["b"], [(1.0, 7.0)]);
        assert_eq!(
            sparkline_path(&lines["a"], 100.0, 30.0, false).unwrap(),
            "M0.00,15.00 L100.00,0.00"
        );
        assert_eq!(sparkline_path(&lines["b"], 100.0, 30.0, false), None);
    }
}
