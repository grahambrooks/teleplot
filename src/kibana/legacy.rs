//! Legacy aggregation-based visualizations and markdown → panel content.

use serde_json::Value;

use super::objects::{Export, Reference, bool_field, find_ref, json_attr, number, str_field};
use super::{broken, out_of_scope, search, unparseable};
use crate::model::{
    Chart, ChartKind, Column, DataView, Encoding, Interval, Layer, NamedQuery, Operation,
    PanelContent, Placeholder, Query, Range, SeriesType, TermsOrder,
};

/// The parts of a visualization, whichever form it was stored in.
struct Vis {
    kind: String,
    params: Value,
    aggs: Vec<Value>,
    search_source: Value,
}

/// A `visualization` saved object (by reference).
pub fn from_saved_object(
    export: &Export,
    attrs: &Value,
    refs: &[Reference],
) -> Result<PanelContent, Placeholder> {
    let state = json_attr(attrs.get("visState"));
    let vis = Vis {
        kind: str_field(&state, "type").unwrap_or("unknown").to_string(),
        params: state["params"].clone(),
        aggs: state["aggs"].as_array().cloned().unwrap_or_default(),
        search_source: json_attr(attrs["kibanaSavedObjectMeta"].get("searchSourceJSON")),
    };
    convert(export, vis, refs)
}

/// A by-value `savedVis` embedded in a dashboard panel.
pub fn from_saved_vis(
    export: &Export,
    saved_vis: &Value,
    refs: &[Reference],
) -> Result<PanelContent, Placeholder> {
    let vis = Vis {
        kind: str_field(saved_vis, "type")
            .unwrap_or("unknown")
            .to_string(),
        params: saved_vis["params"].clone(),
        aggs: saved_vis["data"]["aggs"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        search_source: saved_vis["data"]["searchSource"].clone(),
    };
    convert(export, vis, refs)
}

fn convert(export: &Export, vis: Vis, refs: &[Reference]) -> Result<PanelContent, Placeholder> {
    let kind_name = vis.kind.as_str();
    let chart_kind = match kind_name {
        "markdown" => {
            let text = str_field(&vis.params, "markdown")
                .unwrap_or_default()
                .to_string();
            return Ok(PanelContent::Markdown { text });
        }
        "line" | "area" | "histogram" | "horizontal_bar" => ChartKind::Xy,
        "pie" if bool_field(&vis.params, "isDonut") => ChartKind::Donut,
        "pie" => ChartKind::Pie,
        "table" => ChartKind::Table,
        "metric" => ChartKind::Metric,
        "metrics" => return Err(out_of_scope("tsvb", "TSVB is not in the MVP")),
        other => {
            return Err(out_of_scope(
                other,
                format!("legacy {other} visualizations are not in the MVP"),
            ));
        }
    };

    let data_view =
        data_view(export, &vis.search_source, refs).map_err(|e| broken(kind_name, e))?;
    let (columns, roles) = columns(&vis.aggs).map_err(|e| unparseable(kind_name, e))?;
    let metrics: Vec<String> = roles
        .iter()
        .filter(|(_, r)| r == "metric")
        .map(|(id, _)| id.clone())
        .collect();
    let with_role = |role: &str| {
        roles
            .iter()
            .filter(|(_, r)| r == role)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>()
    };
    let segments = with_role("segment");
    let groups = with_role("group");
    let rows = with_role("bucket");
    let splits = with_role("split");

    if metrics.is_empty() {
        return Err(unparseable(kind_name, "no metric aggregations"));
    }

    let encoding = match chart_kind {
        ChartKind::Xy => Encoding::Xy {
            series: series_type(kind_name, &vis.params),
            x: segments.first().cloned(),
            y: metrics,
            split: groups.first().or(splits.first()).cloned(),
        },
        ChartKind::Pie | ChartKind::Donut => Encoding::Pie {
            groups: splits.into_iter().chain(segments).collect(),
            values: metrics.into_iter().take(1).collect(),
        },
        ChartKind::Table => Encoding::Table {
            columns: splits.into_iter().chain(rows).chain(metrics).collect(),
        },
        ChartKind::Metric => Encoding::Metric {
            value: metrics[0].clone(),
            secondary: None,
            max: None,
            breakdown: groups.into_iter().chain(rows).chain(splits).next(),
        },
    };

    Ok(PanelContent::Chart(Chart {
        kind: chart_kind,
        query: search::query(&vis.search_source["query"]),
        filters: search::filters(&vis.search_source["filter"]),
        layers: vec![Layer {
            id: "layer".into(),
            data_view,
            columns,
            encoding,
            documents: None,
        }],
    }))
}

fn data_view(export: &Export, source: &Value, refs: &[Reference]) -> Result<DataView, String> {
    let id = if let Some(name) = str_field(source, "indexRefName") {
        find_ref(refs, name)
            .map(|r| r.id.clone())
            .ok_or_else(|| format!("reference {name} is missing"))?
    } else if let Some(id) = str_field(source, "index") {
        id.to_string()
    } else {
        return Err(
            "visualization has no data view (saved-search sources are not supported)".into(),
        );
    };
    export
        .data_view(&id)
        .ok_or_else(|| format!("data view {id} is not in this export"))
}

fn series_type(kind: &str, params: &Value) -> SeriesType {
    let first = &params["seriesParams"][0];
    let stacked =
        str_field(first, "mode").unwrap_or(if kind == "line" { "normal" } else { "stacked" })
            == "stacked";
    let drawn_as = str_field(first, "type").unwrap_or(match kind {
        "histogram" | "horizontal_bar" => "histogram",
        other => other,
    });
    match (kind, drawn_as, stacked) {
        ("horizontal_bar", _, true) => SeriesType::BarHorizontalStacked,
        ("horizontal_bar", _, false) => SeriesType::BarHorizontal,
        (_, "histogram", true) => SeriesType::BarStacked,
        (_, "histogram", false) => SeriesType::Bar,
        (_, "area", true) => SeriesType::AreaStacked,
        (_, "area", false) => SeriesType::Area,
        _ => SeriesType::Line,
    }
}

/// Each column's id and its aggregation `schema` (metric, segment, group, ...).
type Roles = Vec<(String, String)>;

/// Aggregations → columns, with each column's role.
fn columns(aggs: &[Value]) -> Result<(Vec<Column>, Roles), String> {
    let mut columns = Vec::new();
    let mut roles = Vec::new();
    for agg in aggs {
        if agg.get("enabled").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let id = match &agg["id"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return Err("aggregation without an id".into()),
        };
        let kind = str_field(agg, "type").ok_or_else(|| format!("aggregation {id} has no type"))?;
        let schema = str_field(agg, "schema").unwrap_or("metric").to_string();
        let params = &agg["params"];
        let label = str_field(params, "customLabel").map(String::from);
        let field = || {
            str_field(params, "field")
                .map(String::from)
                .ok_or_else(|| format!("{kind} aggregation {id} has no field"))
        };

        let mut push = |id: String, op: Operation, label: Option<String>| {
            roles.push((id.clone(), schema.clone()));
            columns.push(Column {
                id,
                label,
                op,
                filter: None,
                time_shift: None,
                per: None,
                format: None,
                helper: false,
                thresholds: Vec::new(),
            });
        };

        match kind {
            // Multi-value: one column per percent.
            "percentiles" => {
                let percents: Vec<f64> = params["percents"]
                    .as_array()
                    .map(|p| p.iter().filter_map(number).collect())
                    .unwrap_or_default();
                for p in percents {
                    let label = label.clone().or_else(|| {
                        Some(format!(
                            "{} percentile of {}",
                            ordinal(p),
                            field().unwrap_or_default()
                        ))
                    });
                    push(
                        format!("{id}.{p}"),
                        Operation::Percentile {
                            field: field()?,
                            percent: p,
                        },
                        label,
                    );
                }
            }
            _ => {
                let op = match kind {
                    "count" => Operation::Count { field: None },
                    "avg" => Operation::Average { field: field()? },
                    "sum" => Operation::Sum { field: field()? },
                    "min" => Operation::Min { field: field()? },
                    "max" => Operation::Max { field: field()? },
                    "median" => Operation::Median { field: field()? },
                    "std_dev" => Operation::StandardDeviation { field: field()? },
                    "cardinality" => Operation::UniqueCount { field: field()? },
                    "top_hits" => Operation::LastValue {
                        field: field()?,
                        sort_field: str_field(params, "sortField")
                            .ok_or("top hit without a sort field")?
                            .to_string(),
                    },
                    "date_histogram" => Operation::DateHistogram {
                        field: field()?,
                        interval: str_field(params, "interval")
                            .unwrap_or("auto")
                            .parse::<Interval>()?,
                        include_empty: number(&params["min_doc_count"]) == Some(0.0),
                    },
                    "terms" => Operation::Terms {
                        fields: vec![field()?],
                        size: number(&params["size"]).unwrap_or(5.0) as u32,
                        order: match str_field(params, "orderBy") {
                            Some("_key") => TermsOrder::Alphabetical {
                                ascending: str_field(params, "order") == Some("asc"),
                            },
                            Some(column) if column != "_count" => TermsOrder::Column {
                                column: column.into(),
                                ascending: str_field(params, "order") == Some("asc"),
                            },
                            _ => TermsOrder::Count,
                        },
                        other_bucket: bool_field(params, "otherBucket"),
                        missing_bucket: bool_field(params, "missingBucket"),
                    },
                    "filters" => Operation::Filters {
                        filters: params["filters"]
                            .as_array()
                            .ok_or("filters aggregation without filters")?
                            .iter()
                            .map(|f| {
                                let query: Query = search::query(&f["input"]);
                                let label = str_field(f, "label")
                                    .map(String::from)
                                    .unwrap_or_else(|| query.text.clone());
                                NamedQuery { label, query }
                            })
                            .collect(),
                    },
                    "histogram" => Operation::Histogram {
                        field: field()?,
                        interval: number(&params["interval"]),
                    },
                    "range" => Operation::Ranges {
                        field: field()?,
                        ranges: params["ranges"]
                            .as_array()
                            .map(|rs| {
                                rs.iter()
                                    .map(|r| Range {
                                        from: number(&r["from"]),
                                        to: number(&r["to"]),
                                        label: None,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    other => return Err(format!("unsupported aggregation {other}")),
                };
                let label = label.or_else(|| default_label(&op, params));
                push(id, op, label);
            }
        }
    }
    Ok((columns, roles))
}

/// The label Kibana gives an aggregation without a custom label.
fn default_label(op: &Operation, params: &Value) -> Option<String> {
    Some(match op {
        Operation::Count { .. } => "Count".into(),
        Operation::Average { field } => format!("Average {field}"),
        Operation::Sum { field } => format!("Sum of {field}"),
        Operation::Min { field } => format!("Min {field}"),
        Operation::Max { field } => format!("Max {field}"),
        Operation::Median { field } => format!("Median {field}"),
        Operation::StandardDeviation { field } => format!("Standard deviation of {field}"),
        Operation::UniqueCount { field } => format!("Unique count of {field}"),
        Operation::LastValue { field, .. } => format!("Last {field}"),
        Operation::DateHistogram {
            field, interval, ..
        } => match interval {
            Interval::Auto => format!("{field} per auto interval"),
            Interval::Every { count: 1, unit } => format!("{field} per {}", unit_name(*unit)),
            other => format!("{field} per {other}"),
        },
        Operation::Terms { fields, .. } => {
            let direction = if str_field(params, "order") == Some("asc") {
                "Ascending"
            } else {
                "Descending"
            };
            format!("{}: {direction}", fields.join(", "))
        }
        Operation::Filters { .. } => "Filters".into(),
        Operation::Histogram { field, .. } => field.clone(),
        Operation::Ranges { field, .. } => format!("{field} ranges"),
        _ => return None,
    })
}

fn unit_name(unit: crate::model::IntervalUnit) -> &'static str {
    use crate::model::IntervalUnit::*;
    match unit {
        Millisecond => "millisecond",
        Second => "second",
        Minute => "minute",
        Hour => "hour",
        Day => "day",
        Week => "week",
        Month => "month",
        Quarter => "quarter",
        Year => "year",
    }
}

/// 50 → "50th", 1 → "1st", 99.9 → "99.9th".
fn ordinal(p: f64) -> String {
    if p.fract() != 0.0 {
        return format!("{p}th");
    }
    let n = p as i64;
    let suffix = match (n % 10, n % 100) {
        (1, r) if r != 11 => "st",
        (2, r) if r != 12 => "nd",
        (3, r) if r != 13 => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}
