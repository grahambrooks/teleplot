//! Turning layer tables into chart series.

use crate::model::{
    Cell, Chart, Column, Encoding, Format, Layer, LayerData, Operation, PanelData, SeriesType,
    Threshold,
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
    // The x axis kind comes from the first layer with an x column.
    let x_kind = chart.layers.iter().find_map(|l| match &l.encoding {
        Encoding::Xy { x: Some(x), .. } => l.column(x).map(|c| match c.op {
            Operation::DateHistogram { .. } => 't',
            Operation::Histogram { .. } => 'n',
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
            let format = layer.column(y_id).and_then(|c| c.format.clone());
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
    Xy {
        x,
        series,
        thresholds,
        low_sample,
        band: None,
        log_scale: false,
    }
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
    let (Some(layer), Some(d)) = (chart.layers.first(), data.layers.first()) else {
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
            format: column.and_then(|c| c.format.clone()),
            style,
        }
    };
    let low_sample = match min_samples {
        Some(min) => d.counts.iter().map(|n| *n < min as f64).collect(),
        None => vec![false; xs.len()],
    };
    Xy {
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
    }
}

/// One point per document, with an optional trend line. Points are split
/// into one series per `split` value when given.
pub fn scatter(chart: &Chart, data: &PanelData) -> Xy {
    let (Some(layer), Some(d)) = (chart.layers.first(), data.layers.first()) else {
        return empty();
    };
    let Encoding::Scatter {
        x,
        y,
        split,
        trend,
        log_scale,
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
    let format = y_column.and_then(|c| c.format.clone());
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
    Xy {
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
    }
}

fn empty() -> Xy {
    Xy {
        x: XAxis::Time(Vec::new()),
        series: Vec::new(),
        thresholds: Vec::new(),
        low_sample: Vec::new(),
        band: None,
        log_scale: false,
    }
}

/// The benchmark band a value falls in.
pub fn threshold_for(thresholds: &[Threshold], v: f64) -> Option<(usize, &Threshold)> {
    thresholds.iter().enumerate().find(|(_, t)| t.contains(v))
}

/// The layer a chart is drawn from, for single-layer panels.
pub fn first_layer(chart: &Chart) -> Option<&Layer> {
    chart.layers.first()
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
            }],
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
            }],
            warnings: vec![],
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
}
