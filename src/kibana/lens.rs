//! Lens visualizations (by value or by reference) → [`Chart`].

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::objects::{
    Export, Reference, as_object, bool_field, data_view_from, find_ref, json_attr, number,
    str_field,
};
use super::{broken, out_of_scope, search, unparseable};
use crate::model::{
    Annotation, AnnotationEvent, BadgeShow, Better, Chart, ChartKind, ColorByValue, ColorMode,
    ColorStop, Column, ColumnStyle, DataView, Encoding, Esql, Expr, Format, FormatKind, GaugeShape,
    Interpolation, Interval, Layer, LegendStat, LineStyle, Missing, NamedQuery, Operation,
    OverallFunction, PerUnit, Placeholder, Range, ReferenceAxis, ReferenceFill, ReferenceLine,
    SeriesType, Summary, TermsOrder, TrendBadge, XyOptions,
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
        "lnsHeatmap" => ChartKind::Heatmap,
        "lnsGauge" => ChartKind::Gauge,
        "lnsPie" => match str_field(vis, "shape").unwrap_or("pie") {
            "pie" => ChartKind::Pie,
            "donut" => ChartKind::Donut,
            "treemap" => ChartKind::Treemap,
            "waffle" => ChartKind::Waffle,
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
    // ES|QL panels keep their layers under `textBased`.
    let esql_layers = sources["textBased"]["layers"]
        .as_object()
        .filter(|l| !l.is_empty())
        .cloned();
    if layers.is_none() && esql_layers.is_none() {
        return Err(unparseable(&kind_name, "no datasource layers"));
    }
    let datasource_layers = layers.unwrap_or_default();

    let resolver = DataViews {
        export,
        refs,
        internal: serde_json::from_value(state["internalReferences"].clone()).unwrap_or_default(),
        ad_hoc: as_object(&state["adHocDataViews"]),
    };

    let (encodings, mut warnings, annotation_layers) =
        encodings(kind, vis).map_err(|e| unparseable(&kind_name, e))?;
    let mut annotations = Vec::new();
    for layer in &annotation_layers {
        let data_view =
            str_field(layer, "layerId").and_then(|id| resolver.annotation_layer(id, layer));
        annotations.extend(annotation_layer(layer, data_view, &mut warnings));
    }
    let mut layers = Vec::new();
    for (layer_id, encoding) in encodings {
        let Some(source) = datasource_layers.get(&layer_id) else {
            if let Some(source) = esql_layers.as_ref().and_then(|l| l.get(&layer_id)) {
                layers.push(esql_layer(
                    &kind_name,
                    layer_id,
                    source,
                    &sources["textBased"],
                    encoding,
                )?);
                continue;
            }
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
        let mut columns = columns(source, &encoding).map_err(|e| unparseable(&kind_name, e))?;
        // Lens labels the secondary metric itself: `secondaryLabel`, or
        // "Difference" when a trend badge compares it with the primary.
        if let Encoding::Metric {
            secondary: Some(id),
            badge,
            ..
        } = &encoding
        {
            let label = match vis.get("secondaryLabel").and_then(Value::as_str) {
                Some(label) => Some(label.to_string()),
                None if badge.as_ref().is_some_and(|b| b.baseline.is_none()) => {
                    Some("Difference".into())
                }
                None => None,
            };
            if let (Some(label), Some(c)) = (label, columns.iter_mut().find(|c| &c.id == id)) {
                c.label = Some(label);
            }
        }
        layers.push(Layer {
            insight: None,
            id: layer_id,
            data_view,
            columns,
            encoding,
            documents: None,
            esql: None,
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
        annotations,
        warnings,
    })
}

/// A Lens ES|QL (`textBased`) layer: its query, the data view it reads (for
/// the time field), and its result columns.
fn esql_layer(
    kind_name: &str,
    id: String,
    source: &Value,
    text_based: &Value,
    encoding: Encoding,
) -> Result<Layer, Placeholder> {
    let query = str_field(&source["query"], "esql")
        .ok_or_else(|| unparseable(kind_name, "ES|QL layer without a query"))?
        .to_string();
    let variables = crate::query::esql_variables(&query);
    if !variables.is_empty() {
        let names: Vec<String> = variables.iter().map(|v| format!("?{v}")).collect();
        return Err(out_of_scope(
            kind_name,
            format!(
                "uses ES|QL control variables ({}), which Teleplot does not support",
                names.join(", ")
            ),
        ));
    }
    // The data view: Lens's reference for the layer, else the query's `FROM`.
    let reference = text_based["indexPatternRefs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| str_field(r, "id") == str_field(source, "index"));
    let index = reference
        .and_then(|r| str_field(r, "title"))
        .map(String::from)
        .or_else(|| esql_from(&query))
        .ok_or_else(|| unparseable(kind_name, "ES|QL layer without an index"))?;
    let time_field = str_field(source, "timeField")
        .or_else(|| reference.and_then(|r| str_field(r, "timeField")))
        .map(String::from);
    let columns = source["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let column_id = str_field(c, "columnId")?;
            let field = str_field(c, "fieldName").unwrap_or(column_id);
            Some(Column {
                id: column_id.to_string(),
                label: str_field(c, "label").or(Some(field)).map(String::from),
                op: Operation::Value {
                    field: field.to_string(),
                },
                filter: None,
                time_shift: None,
                per: None,
                format: format(&c["params"]["format"]),
                helper: false,
                thresholds: Vec::new(),
            })
        })
        .collect();
    Ok(Layer {
        insight: None,
        id,
        data_view: DataView { index, time_field },
        columns,
        encoding,
        documents: None,
        esql: Some(Esql { query }),
    })
}

/// The index pattern after `FROM` in an ES|QL query.
fn esql_from(query: &str) -> Option<String> {
    let mut words = query.split_whitespace();
    words
        .find(|w| w.eq_ignore_ascii_case("from"))
        .and_then(|_| words.next())
        .map(|w| w.trim_end_matches(['|', ',']).to_string())
        .filter(|w| !w.is_empty())
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

    /// An annotation layer's data view: referenced, or by id (ad hoc).
    fn annotation_layer(&self, layer_id: &str, layer: &Value) -> Option<DataView> {
        let name = format!("xy-visualization-layer-{layer_id}");
        let id = find_ref(self.refs, &name)
            .or_else(|| find_ref(&self.internal, &name))
            .map(|r| r.id.clone())
            .or_else(|| str_field(layer, "indexPatternId").map(String::from))?;
        self.export
            .data_view(&id)
            .or_else(|| data_view_from(self.ad_hoc.get(&id)?))
    }
}

/// Data layers and how each is drawn, from the visualization state;
/// warnings for the layers Teleplot does not draw; and annotation layers.
type Encodings = (Vec<(String, Encoding)>, Vec<String>, Vec<Value>);

fn encodings(kind: ChartKind, vis: &Value) -> Result<Encodings, String> {
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

    let mut warnings = Vec::new();
    let mut annotation_layers = Vec::new();
    let layers = match kind {
        ChartKind::Xy => {
            let mut out = Vec::new();
            for layer in vis["layers"].as_array().ok_or("XY without layers")? {
                match str_field(layer, "layerType").unwrap_or("data") {
                    "data" => {}
                    "referenceLine" => {
                        out.push((layer_id(layer)?, reference_lines(layer)));
                        continue;
                    }
                    "annotations" => {
                        annotation_layers.push(layer.clone());
                        continue;
                    }
                    other => {
                        warnings.push(format!("Not shown: a Lens {other} layer."));
                        continue;
                    }
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
                        options: xy_options(vis, &mut warnings),
                    },
                ));
            }
            out
        }
        ChartKind::Metric => {
            let mut layers = vec![(
                layer_id(vis)?,
                Encoding::Metric {
                    value: s(vis, "metricAccessor").ok_or("metric without metricAccessor")?,
                    secondary: s(vis, "secondaryMetricAccessor"),
                    max: s(vis, "maxAccessor"),
                    breakdown: s(vis, "breakdownByAccessor"),
                    progress: bool_field(vis, "showBar") && s(vis, "maxAccessor").is_some(),
                    progress_direction: match str_field(vis, "progressDirection") {
                        Some("horizontal") => crate::model::ProgressDirection::Horizontal,
                        _ => crate::model::ProgressDirection::Vertical,
                    },
                    badge: trend_badge(&vis["secondaryTrend"]),
                },
            )];
            // The background trend line is a layer of its own in Lens.
            if let Some(trend) = s(vis, "trendlineLayerId") {
                match (s(vis, "trendlineTimeAccessor"), s(vis, "trendlineMetricAccessor")) {
                    (Some(x), Some(y)) => layers.push((
                        trend,
                        Encoding::Sparkline {
                            x,
                            y,
                            by: s(vis, "trendlineBreakdownByAccessor"),
                        },
                    )),
                    _ => warnings.push("Not shown: the metric's background trend line, which has no time or value.".into()),
                }
            }
            layers
        }
        ChartKind::Gauge => vec![(
            layer_id(vis)?,
            Encoding::Gauge {
                value: s(vis, "metricAccessor").ok_or("gauge without metricAccessor")?,
                min: s(vis, "minAccessor"),
                max: s(vis, "maxAccessor"),
                goal: s(vis, "goalAccessor"),
                shape: match str_field(vis, "shape") {
                    Some("semiCircle") => GaugeShape::SemiCircle,
                    Some("circle") => GaugeShape::Circle,
                    Some("horizontalBullet") => GaugeShape::HorizontalBullet,
                    Some("verticalBullet") => GaugeShape::VerticalBullet,
                    _ => GaugeShape::Arc,
                },
                bands: (str_field(vis, "colorMode") == Some("palette"))
                    .then(|| palette(&vis["palette"]["params"]))
                    .flatten(),
            },
        )],
        ChartKind::Calendar => return Err("calendars are not a Lens chart".into()),
        ChartKind::Pie | ChartKind::Donut | ChartKind::Treemap | ChartKind::Waffle => {
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
            let styles = vis["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| !bool_field(c, "hidden"))
                .filter_map(column_style)
                .collect();
            vec![(
                layer_id(vis)?,
                Encoding::Table {
                    columns,
                    styles,
                    sparkline: None,
                },
            )]
        }
        // Teleplot colours cells with its own sequential scale; a Lens
        // palette changes colours only, never which cells are drawn.
        ChartKind::Heatmap => vec![(
            layer_id(vis)?,
            Encoding::Heatmap {
                x: s(vis, "xAccessor").ok_or("heatmap without xAccessor")?,
                y: s(vis, "yAccessor"),
                value: s(vis, "valueAccessor").ok_or("heatmap without valueAccessor")?,
            },
        )],
    };
    Ok((layers, warnings, annotation_layers))
}

/// A Lens annotation layer: each visible annotation, manual (a moment or a
/// range) or a query over the layer's data view.
fn annotation_layer(
    layer: &Value,
    data_view: Option<DataView>,
    warnings: &mut Vec<String>,
) -> Vec<Annotation> {
    let Some(list) = layer["annotations"].as_array() else {
        // A layer linked to an annotation group in the library.
        warnings.push(
            "Not shown: a Lens annotation layer linked from the library. Teleplot converts annotation layers saved with the visualization.".into(),
        );
        return Vec::new();
    };
    // Lens ignores the dashboard's filters unless told otherwise.
    let ignore_global_filters = layer["ignoreGlobalFilters"].as_bool().unwrap_or(true);
    let mut out = Vec::new();
    for (i, a) in list.iter().enumerate() {
        if bool_field(a, "isHidden") {
            continue;
        }
        let id = str_field(a, "id")
            .map(String::from)
            .unwrap_or_else(|| format!("annotation-{i}"));
        let label = str_field(a, "label").map(String::from);
        let mut annotation = Annotation {
            id,
            label: label.clone(),
            data_view: None,
            query: Default::default(),
            time: None,
            end: None,
            text: None,
            limit: 500,
            events: Vec::new(),
            ignore_global_filters,
            color: str_field(a, "color").map(String::from),
            style: match str_field(a, "lineStyle") {
                Some("dashed") => LineStyle::Dashed,
                Some("dotted") => LineStyle::Dotted,
                _ => LineStyle::Solid,
            },
        };
        let key = &a["key"];
        match str_field(a, "type").unwrap_or("manual") {
            "query" => {
                let Some(view) = data_view.clone() else {
                    warnings.push(format!(
                        "Not shown: annotation {:?}, whose data view is not in this export.",
                        label.unwrap_or_default()
                    ));
                    continue;
                };
                annotation.query = search::query(&a["filter"]);
                annotation.time = str_field(a, "timeField")
                    .filter(|f| view.time_field.as_deref() != Some(*f))
                    .map(String::from);
                annotation.text = str_field(a, "textField")
                    .map(String::from)
                    .or_else(|| a["extraFields"][0].as_str().map(String::from));
                annotation.data_view = Some(view);
            }
            _ => {
                let Some(at) = key["timestamp"].as_str() else {
                    warnings.push(format!(
                        "Not shown: annotation {:?}, which has no time.",
                        label.unwrap_or_default()
                    ));
                    continue;
                };
                annotation.events.push(AnnotationEvent {
                    at: at.to_string(),
                    to: (str_field(key, "type") == Some("range"))
                        .then(|| key["endTimestamp"].as_str().map(String::from))
                        .flatten(),
                    text: None,
                });
            }
        }
        out.push(annotation);
    }
    out
}

/// Lens XY settings that apply to every layer: how lines are drawn and gaps
/// filled (`curveType`, `fittingFunction`), and legend statistics.
fn xy_options(vis: &Value, warnings: &mut Vec<String>) -> XyOptions {
    let interpolation = match str_field(vis, "curveType") {
        Some("CURVE_MONOTONE_X") => Interpolation::Smooth,
        Some("CURVE_STEP_AFTER") => Interpolation::Step,
        _ => Interpolation::Linear,
    };
    // Lens leaves gaps unless told how to fit them.
    let missing = match str_field(vis, "fittingFunction") {
        Some("Linear") => Missing::Connect,
        Some("Zero") => Missing::Zero,
        Some("Carry") => Missing::Carry,
        Some("Lookahead") => Missing::Lookahead,
        _ => Missing::Gap,
    };
    let mut legend_stats = Vec::new();
    for stat in vis["legend"]["legendStats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let known = match stat {
            "average" => Some(LegendStat::Average),
            "median" => Some(LegendStat::Median),
            "min" => Some(LegendStat::Min),
            "max" => Some(LegendStat::Max),
            "range" => Some(LegendStat::Range),
            "firstValue" | "firstNonNullValue" => Some(LegendStat::First),
            "lastValue" | "lastNonNullValue" | "currentAndLastValue" => Some(LegendStat::Last),
            "total" => Some(LegendStat::Total),
            "count" => Some(LegendStat::Count),
            "difference" => Some(LegendStat::Difference),
            "differencePercent" => Some(LegendStat::DifferencePercent),
            "stdDeviation" => Some(LegendStat::StdDeviation),
            _ => None,
        };
        match known {
            Some(s) if !legend_stats.contains(&s) => legend_stats.push(s),
            Some(_) => {}
            None => warnings.push(format!("Not shown: the legend statistic {stat:?}.")),
        }
    }
    XyOptions {
        interpolation,
        missing,
        legend_stats,
        facet: None,
    }
}

/// Lens 9.x's `secondaryTrend`: a dynamic trend becomes a badge. Its palette
/// says which direction is good: `compare_to` is green when rising,
/// `temperature` red when rising; `reversed` swaps them.
fn trend_badge(trend: &Value) -> Option<TrendBadge> {
    if str_field(trend, "type") != Some("dynamic") {
        return None;
    }
    let reversed = bool_field(trend, "reversed");
    let better = match (str_field(trend, "paletteId"), reversed) {
        (Some("compare_to") | None, false) | (Some("temperature"), true) => Better::Up,
        (Some("compare_to") | None, true) | (Some("temperature"), false) => Better::Down,
        _ => Better::Neither,
    };
    Some(TrendBadge {
        better,
        baseline: number(&trend["baselineValue"]),
        show: match str_field(trend, "visuals") {
            Some("icon") => BadgeShow::Icon,
            Some("value") => BadgeShow::Value,
            _ => BadgeShow::Both,
        },
    })
}

/// A Lens datatable column's colour by value and summary row.
fn column_style(c: &Value) -> Option<ColumnStyle> {
    let column = str_field(c, "columnId")?.to_string();
    let color = match str_field(c, "colorMode") {
        Some(mode @ ("cell" | "text" | "badge")) => {
            palette(&c["palette"]["params"]).map(|mut p| {
                p.mode = match mode {
                    "text" => ColorMode::Text,
                    "badge" => ColorMode::Badge,
                    _ => ColorMode::Cell,
                };
                p
            })
        }
        _ => None,
    };
    let summary = match str_field(c, "summaryRow") {
        Some("sum") => Some(Summary::Sum),
        Some("avg") => Some(Summary::Avg),
        Some("count") => Some(Summary::Count),
        Some("min") => Some(Summary::Min),
        Some("max") => Some(Summary::Max),
        _ => None,
    };
    (color.is_some() || summary.is_some()).then(|| ColumnStyle {
        column,
        color,
        summary,
        summary_label: summary.and(str_field(c, "summaryLabel").map(String::from)),
    })
}

/// Kibana's custom palette params: `colorStops` are where each colour
/// starts (or `stops`, when there are none); `rangeType: percent` makes them
/// shares of the column's range; `continuity` extends the first and last.
fn palette(params: &Value) -> Option<ColorByValue> {
    let stops = |key: &str| -> Vec<ColorStop> {
        params[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                Some(ColorStop {
                    from: number(&s["stop"])?,
                    color: str_field(s, "color")?.to_string(),
                })
            })
            .collect()
    };
    let mut from = stops("colorStops");
    if from.is_empty() {
        from = stops("stops");
    }
    if from.is_empty() {
        return None;
    }
    let continuity = str_field(params, "continuity").unwrap_or("above");
    Some(ColorByValue {
        mode: ColorMode::Cell,
        stops: from,
        percent: str_field(params, "rangeType") == Some("percent"),
        extend_below: matches!(continuity, "below" | "all"),
        extend_above: matches!(continuity, "above" | "all"),
        to: number(&params["rangeMax"]),
    })
}

/// A Lens reference-line layer: one line per accessor, styled by its `yConfig`.
fn reference_lines(layer: &Value) -> Encoding {
    let configs = layer["yConfig"].as_array().cloned().unwrap_or_default();
    let lines = layer["accessors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|column| {
            let config = configs
                .iter()
                .find(|c| str_field(c, "forAccessor") == Some(column))
                .cloned()
                .unwrap_or_default();
            ReferenceLine {
                column: column.to_string(),
                axis: match str_field(&config, "axisMode") {
                    Some("right") => ReferenceAxis::Right,
                    Some("bottom" | "x") => ReferenceAxis::Bottom,
                    _ => ReferenceAxis::Left,
                },
                color: str_field(&config, "color").map(String::from),
                style: match str_field(&config, "lineStyle") {
                    Some("dashed") => LineStyle::Dashed,
                    Some("dotted") => LineStyle::Dotted,
                    _ => LineStyle::Solid,
                },
                fill: match str_field(&config, "fill") {
                    Some("above") => ReferenceFill::Above,
                    Some("below") => ReferenceFill::Below,
                    _ => ReferenceFill::None,
                },
                // Kibana hides the label unless it is asked for.
                show_label: bool_field(&config, "textVisibility"),
            }
        })
        .collect();
    Encoding::ReferenceLines { lines }
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
