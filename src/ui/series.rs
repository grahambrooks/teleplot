//! Turning layer tables into chart series.

use crate::model::{Cell, Chart, Encoding, Format, LayerData, Operation, PanelData, SeriesType};

/// What an XY chart draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Xy {
    pub x: XAxis,
    pub series: Vec<XySeries>,
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
pub fn xy(chart: &Chart, data: &PanelData) -> Xy {
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
                        });
                        series.len() - 1
                    }
                };
                points.push(Point {
                    x: xs[row].clone(),
                    series: index,
                    value: ys[row].number(),
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
    for p in points {
        let slot = match &x {
            XAxis::Time(keys) | XAxis::Numeric(keys) => {
                p.x.number().and_then(|v| keys.iter().position(|k| *k == v))
            }
            XAxis::Category(keys) => keys.iter().position(|k| *k == cell_text(&p.x)),
        };
        if let Some(slot) = slot {
            series[p.series].values[slot] = p.value;
        }
    }
    Xy { x, series }
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
            }],
        }
    }

    fn data(columns: Vec<(&str, Vec<Cell>)>) -> PanelData {
        let rows = columns[0].1.len();
        PanelData {
            layers: vec![LayerData {
                layer: "l".into(),
                rows,
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
        let xy = xy(&chart(true), &d);
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
        assert_eq!(xy(&chart(false), &d).series[0].label, "Y");
    }
}
