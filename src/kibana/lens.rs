//! Lens visualizations (by value or by reference) → [`Chart`].

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::objects::{
    Export, Reference, as_object, bool_field, data_view_from, find_ref, json_attr, number,
    str_field,
};
use super::{broken, out_of_scope, search, unparseable};
use crate::model::{
    Chart, ChartKind, Column, DataView, Encoding, Expr, Format, FormatKind, Interval, Layer,
    NamedQuery, Operation, OverallFunction, PerUnit, Placeholder, Range, SeriesType, TermsOrder,
};

/// Converts Lens saved-object attributes. `refs` are the references visible
/// to this visualization: its own, plus the dashboard's for by-value panels.
pub fn convert(export: &Export, attrs: &Value, refs: &[Reference]) -> Result<Chart, Placeholder> {
    let vis_type = str_field(attrs, "visualizationType").unwrap_or("unknown");
    let kind_name = format!("lens:{vis_type}");
    let state = json_attr(attrs.get("state"));
    let vis = &state["visualization"];

    let kind = match vis_type {
        "lnsXY" => ChartKind::Xy,
        "lnsMetric" => ChartKind::Metric,
        "lnsDatatable" => ChartKind::Table,
        "lnsPie" => match str_field(vis, "shape").unwrap_or("pie") {
            "pie" => ChartKind::Pie,
            "donut" => ChartKind::Donut,
            shape => {
                return Err(out_of_scope(
                    &kind_name,
                    format!("{shape} charts are not in the MVP"),
                ));
            }
        },
        _ => {
            return Err(out_of_scope(
                &kind_name,
                format!("Lens {vis_type} is not in the MVP"),
            ));
        }
    };

    let sources = &state["datasourceStates"];
    let layers = ["formBased", "indexpattern"]
        .iter()
        .find_map(|k| sources[k]["layers"].as_object().filter(|l| !l.is_empty()))
        .cloned();
    let Some(datasource_layers) = layers else {
        if sources.get("textBased").is_some() {
            return Err(out_of_scope(&kind_name, "ES|QL panels are not in the MVP"));
        }
        return Err(unparseable(&kind_name, "no form-based datasource layers"));
    };

    let resolver = DataViews {
        export,
        refs,
        internal: serde_json::from_value(state["internalReferences"].clone()).unwrap_or_default(),
        ad_hoc: as_object(&state["adHocDataViews"]),
    };

    let encodings = encodings(kind, vis).map_err(|e| unparseable(&kind_name, e))?;
    let mut layers = Vec::new();
    for (layer_id, encoding) in encodings {
        let Some(source) = datasource_layers.get(&layer_id) else {
            return Err(unparseable(
                &kind_name,
                format!("layer {layer_id} has no datasource"),
            ));
        };
        let data_view = resolver.layer(&layer_id).ok_or_else(|| {
            broken(
                &kind_name,
                format!("data view for layer {layer_id} is not in this export"),
            )
        })?;
        let columns = columns(source, &encoding).map_err(|e| unparseable(&kind_name, e))?;
        layers.push(Layer {
            id: layer_id,
            data_view,
            columns,
            encoding,
            documents: None,
        });
    }
    if layers.is_empty() {
        return Err(unparseable(&kind_name, "no data layers"));
    }

    Ok(Chart {
        kind,
        query: search::query(&state["query"]),
        filters: search::filters(&state["filters"]),
        layers,
    })
}

struct DataViews<'a> {
    export: &'a Export,
    refs: &'a [Reference],
    internal: Vec<Reference>,
    ad_hoc: Map<String, Value>,
}

impl DataViews<'_> {
    fn layer(&self, layer_id: &str) -> Option<DataView> {
        let name = format!("indexpattern-datasource-layer-{layer_id}");
        let id = &find_ref(self.refs, &name)
            .or_else(|| find_ref(&self.internal, &name))?
            .id;
        self.export
            .data_view(id)
            .or_else(|| data_view_from(self.ad_hoc.get(id)?))
    }
}

/// Data layers and how each is drawn, from the visualization state.
fn encodings(kind: ChartKind, vis: &Value) -> Result<Vec<(String, Encoding)>, String> {
    let s = |v: &Value, k: &str| str_field(v, k).map(String::from);
    let ids = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let layer_id = |v: &Value| s(v, "layerId").ok_or("layer without layerId");

    Ok(match kind {
        ChartKind::Xy => {
            let mut out = Vec::new();
            for layer in vis["layers"].as_array().ok_or("XY without layers")? {
                // Annotation and reference-line layers draw no data series.
                if str_field(layer, "layerType").unwrap_or("data") != "data" {
                    continue;
                }
                let series_name = s(layer, "seriesType").or_else(|| s(vis, "preferredSeriesType"));
                let series = series_type(series_name.as_deref().unwrap_or("bar_stacked"))?;
                out.push((
                    layer_id(layer)?,
                    Encoding::Xy {
                        series,
                        x: s(layer, "xAccessor"),
                        y: ids(&layer["accessors"]),
                        split: s(layer, "splitAccessor"),
                    },
                ));
            }
            out
        }
        ChartKind::Metric => vec![(
            layer_id(vis)?,
            Encoding::Metric {
                value: s(vis, "metricAccessor").ok_or("metric without metricAccessor")?,
                secondary: s(vis, "secondaryMetricAccessor"),
                max: s(vis, "maxAccessor"),
                breakdown: s(vis, "breakdownByAccessor"),
            },
        )],
        ChartKind::Pie | ChartKind::Donut => {
            let layer = vis["layers"].get(0).ok_or("pie without layers")?;
            let mut values = ids(&layer["metrics"]);
            if values.is_empty() {
                values.extend(s(layer, "metric")); // before 8.6, one metric
            }
            let mut groups = ids(&layer["primaryGroups"]);
            groups.extend(ids(&layer["secondaryGroups"]));
            vec![(layer_id(layer)?, Encoding::Pie { groups, values })]
        }
        ChartKind::Table => {
            let columns = vis["columns"]
                .as_array()
                .ok_or("datatable without columns")?
                .iter()
                .filter(|c| !bool_field(c, "hidden"))
                .filter_map(|c| s(c, "columnId"))
                .collect();
            vec![(layer_id(vis)?, Encoding::Table { columns })]
        }
    })
}

fn series_type(name: &str) -> Result<SeriesType, String> {
    Ok(match name {
        "line" => SeriesType::Line,
        "area" => SeriesType::Area,
        "area_stacked" => SeriesType::AreaStacked,
        "area_percentage_stacked" => SeriesType::AreaPercentageStacked,
        "bar" => SeriesType::Bar,
        "bar_stacked" => SeriesType::BarStacked,
        "bar_percentage_stacked" => SeriesType::BarPercentageStacked,
        "bar_horizontal" => SeriesType::BarHorizontal,
        "bar_horizontal_stacked" => SeriesType::BarHorizontalStacked,
        "bar_horizontal_percentage_stacked" => SeriesType::BarHorizontalPercentageStacked,
        other => return Err(format!("unknown series type {other}")),
    })
}

fn columns(layer: &Value, encoding: &Encoding) -> Result<Vec<Column>, String> {
    let all = layer["columns"]
        .as_object()
        .ok_or("layer without columns")?;
    let order: Vec<String> = layer["columnOrder"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| all.keys().cloned().collect());

    let mut columns = Vec::new();
    for id in &order {
        let Some(spec) = all.get(id) else { continue };
        if let Some(column) = column(id, spec, all)? {
            columns.push(column);
        }
    }

    // Columns only consumed by others (formula parts) are helpers.
    let drawn: HashSet<&str> = encoding.columns().into_iter().collect();
    let consumed: HashSet<String> = columns
        .iter()
        .flat_map(|c| c.op.inputs())
        .map(String::from)
        .collect();
    for c in &mut columns {
        c.helper = consumed.contains(&c.id) && !drawn.contains(c.id.as_str());
    }
    Ok(columns)
}

/// One Lens column. `None` for the `math` columns a formula absorbs.
fn column(id: &str, c: &Value, all: &Map<String, Value>) -> Result<Option<Column>, String> {
    let op_name =
        str_field(c, "operationType").ok_or_else(|| format!("column {id} has no operationType"))?;
    let params = &c["params"];
    let field = || -> Result<String, String> {
        str_field(c, "sourceField")
            .map(String::from)
            .ok_or_else(|| format!("{op_name} column {id} has no field"))
    };
    let reference = || -> Result<String, String> {
        c["references"][0]
            .as_str()
            .map(String::from)
            .ok_or_else(|| format!("{op_name} column {id} has no input"))
    };

    let op = match op_name {
        "math" => return Ok(None),
        "count" => Operation::Count {
            field: str_field(c, "sourceField")
                .filter(|f| *f != "___records___")
                .map(String::from),
        },
        "sum" => Operation::Sum { field: field()? },
        "average" => Operation::Average { field: field()? },
        "min" => Operation::Min { field: field()? },
        "max" => Operation::Max { field: field()? },
        "median" => Operation::Median { field: field()? },
        "standard_deviation" => Operation::StandardDeviation { field: field()? },
        "unique_count" => Operation::UniqueCount { field: field()? },
        "percentile" => Operation::Percentile {
            field: field()?,
            percent: number(&params["percentile"]).ok_or("percentile without a percent")?,
        },
        "percentile_rank" => Operation::PercentileRank {
            field: field()?,
            value: number(&params["value"]).ok_or("percentile rank without a value")?,
        },
        "last_value" => Operation::LastValue {
            field: field()?,
            sort_field: str_field(params, "sortField")
                .ok_or("last value without a sort field")?
                .to_string(),
        },
        "static_value" => Operation::StaticValue {
            value: number(&params["value"]).unwrap_or(0.0),
        },
        "time_range" => Operation::TimeRange,
        "date_histogram" => Operation::DateHistogram {
            field: field()?,
            interval: str_field(params, "interval")
                .unwrap_or("auto")
                .parse::<Interval>()?,
            include_empty: bool_field(params, "includeEmptyRows"),
        },
        "terms" => {
            let mut fields = vec![field()?];
            if let Some(more) = params["secondaryFields"].as_array() {
                fields.extend(more.iter().filter_map(|f| f.as_str().map(String::from)));
            }
            let ascending = str_field(params, "orderDirection") == Some("asc");
            let order = match str_field(&params["orderBy"], "type") {
                Some("column") => match str_field(&params["orderBy"], "columnId") {
                    Some(column) => TermsOrder::Column {
                        column: column.into(),
                        ascending,
                    },
                    None => TermsOrder::Count,
                },
                Some("alphabetical") => TermsOrder::Alphabetical { ascending },
                _ => TermsOrder::Count,
            };
            Operation::Terms {
                fields,
                size: number(&params["size"]).unwrap_or(5.0) as u32,
                order,
                other_bucket: bool_field(params, "otherBucket"),
                missing_bucket: bool_field(params, "missingBucket"),
            }
        }
        "filters" => Operation::Filters {
            filters: params["filters"]
                .as_array()
                .ok_or("filters column without filters")?
                .iter()
                .map(|f| {
                    let query = search::query(&f["input"]);
                    let label = str_field(f, "label")
                        .map(String::from)
                        .unwrap_or_else(|| query.text.clone());
                    NamedQuery { label, query }
                })
                .collect(),
        },
        "range" => match str_field(params, "type") {
            Some("range") => Operation::Ranges {
                field: field()?,
                ranges: params["ranges"]
                    .as_array()
                    .map(|rs| {
                        rs.iter()
                            .map(|r| Range {
                                from: number(&r["from"]),
                                to: number(&r["to"]),
                                label: str_field(r, "label").map(String::from),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            _ => Operation::Histogram {
                field: field()?,
                interval: None,
            },
        },
        "formula" => {
            let math = all
                .get(&reference()?)
                .ok_or_else(|| format!("formula {id} refers to a missing column"))?;
            Operation::Formula {
                text: str_field(params, "formula").unwrap_or_default().to_string(),
                expr: expr(&math["params"]["tinymathAst"])?,
            }
        }
        "moving_average" => Operation::MovingAverage {
            of: reference()?,
            window: number(&params["window"]).unwrap_or(5.0) as u32,
        },
        "cumulative_sum" => Operation::CumulativeSum { of: reference()? },
        "differences" => Operation::Differences { of: reference()? },
        "counter_rate" => Operation::CounterRate { of: reference()? },
        "overall_sum" | "overall_average" | "overall_min" | "overall_max" => Operation::Overall {
            of: reference()?,
            function: match op_name {
                "overall_sum" => OverallFunction::Sum,
                "overall_average" => OverallFunction::Average,
                "overall_min" => OverallFunction::Min,
                _ => OverallFunction::Max,
            },
        },
        other => return Err(format!("unsupported Lens operation {other}")),
    };

    let filter = c
        .get("filter")
        .filter(|f| f.is_object())
        .map(search::query)
        .filter(|q| !q.is_empty());
    Ok(Some(Column {
        id: id.to_string(),
        label: str_field(c, "label").map(String::from),
        op,
        filter,
        time_shift: str_field(c, "timeShift").map(String::from),
        per: match str_field(c, "timeScale") {
            None => None,
            Some("s") => Some(PerUnit::Second),
            Some("m") => Some(PerUnit::Minute),
            Some("h") => Some(PerUnit::Hour),
            Some("d") => Some(PerUnit::Day),
            Some(other) => return Err(format!("unsupported Lens time scale {other:?}")),
        },
        format: format(&params["format"]),
        helper: false,
        thresholds: Vec::new(),
    }))
}

/// Kibana's tinymath AST: numbers, column ids, and `{type: function}` calls.
fn expr(ast: &Value) -> Result<Expr, String> {
    match ast {
        Value::Number(n) => Ok(Expr::Number(n.as_f64().unwrap_or(0.0))),
        Value::String(column) => Ok(Expr::Column(column.clone())),
        Value::Object(_) => match str_field(ast, "type") {
            Some("function") => Ok(Expr::Call {
                call: str_field(ast, "name")
                    .ok_or("formula function without a name")?
                    .to_string(),
                args: ast["args"]
                    .as_array()
                    .map(|a| a.iter().map(expr).collect())
                    .transpose()?
                    .unwrap_or_default(),
            }),
            Some("variable") => Ok(Expr::Column(
                str_field(ast, "value").unwrap_or_default().to_string(),
            )),
            other => Err(format!("unknown formula node {other:?}")),
        },
        other => Err(format!("unknown formula node {other}")),
    }
}

/// Lens `params.format`: `{ id, params: { decimals, suffix, compact } }`.
pub fn format(value: &Value) -> Option<Format> {
    let kind = match str_field(value, "id")? {
        "number" => FormatKind::Number,
        "percent" => FormatKind::Percent,
        "bytes" => FormatKind::Bytes,
        "duration" => FormatKind::Duration,
        "currency" => FormatKind::Currency,
        _ => return None,
    };
    let params = &value["params"];
    Some(Format {
        kind,
        decimals: number(&params["decimals"]).map(|d| d as u32),
        suffix: str_field(params, "suffix").map(String::from),
        compact: bool_field(params, "compact"),
    })
}
