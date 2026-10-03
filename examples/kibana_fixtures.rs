//! Dev-only: builds the delivery-analytics Kibana dashboards, imports them into
//! a running Kibana, and exports them back as fixtures.
//!
//! ```text
//! cargo run --example kibana_fixtures -- --kibana http://localhost:5608 --out fixtures/kibana/8.19/delivery.ndjson
//! ```
//!
//! The panel structures copy what real Kibana exports contain (see the
//! `sample-data.ndjson` fixtures). Kibana validates and migrates them on
//! import, and the committed fixture is Kibana's own export, not this
//! program's output.
//!
//! Coverage: Lens XY (line, bar, stacked, percentage, area), metric (with a
//! time-shifted secondary), pie/donut, datatable; operations count, sum,
//! average, median, percentile, unique_count, last_value, date_histogram,
//! terms, filters, range, formula, moving_average, cumulative_sum,
//! differences; per-column KQL filters; by-value and by-reference Lens;
//! by-reference legacy line, pie, table and metric; by-value legacy markdown;
//! dashboard query, filter pill, time restore and a team control.

use serde_json::{Value, json};

const LENS_VERSION: &str = "8.9.0";
const DASHBOARD_MIGRATION: &str = "10.3.0";
const LENS_MIGRATION: &str = "8.9.0";
const VIS_MIGRATION: &str = "8.5.0";
const CORE_MIGRATION: &str = "8.8.0";

const DV_PRS: &str = "teleplot-dv-prs";
const DV_DEPLOYS: &str = "teleplot-dv-deployments";
const DV_INCIDENTS: &str = "teleplot-dv-incidents";
const DV_ITEMS: &str = "teleplot-dv-workitems";

// ---------------------------------------------------------------- columns

fn date_histogram(field: &str, interval: &str) -> Value {
    json!({
        "label": field, "dataType": "date", "operationType": "date_histogram",
        "sourceField": field, "isBucketed": true, "scale": "interval",
        "params": { "interval": interval, "includeEmptyRows": true, "dropPartials": false }
    })
}

fn count(label: &str) -> Value {
    json!({
        "label": label, "dataType": "number", "operationType": "count",
        "isBucketed": false, "scale": "ratio", "sourceField": "___records___",
        "params": { "emptyAsNull": false }, "customLabel": true
    })
}

fn metric(op: &str, field: &str, label: &str) -> Value {
    json!({
        "label": label, "dataType": "number", "operationType": op,
        "sourceField": field, "isBucketed": false, "scale": "ratio",
        "params": { "emptyAsNull": true }, "customLabel": true
    })
}

fn percentile(field: &str, p: u8, label: &str) -> Value {
    json!({
        "label": label, "dataType": "number", "operationType": "percentile",
        "sourceField": field, "isBucketed": false, "scale": "ratio",
        "params": { "percentile": p }, "customLabel": true
    })
}

fn last_value(field: &str, sort_field: &str, label: &str) -> Value {
    json!({
        "label": label, "dataType": "string", "operationType": "last_value",
        "sourceField": field, "isBucketed": false, "scale": "ordinal",
        "params": { "sortField": sort_field, "showArrayValues": false }, "customLabel": true
    })
}

fn terms(field: &str, size: u32, order_by: &str) -> Value {
    json!({
        "label": format!("Top {size} values of {field}"), "dataType": "string",
        "operationType": "terms", "sourceField": field, "isBucketed": true, "scale": "ordinal",
        "params": {
            "size": size,
            "orderBy": { "type": "column", "columnId": order_by },
            "orderDirection": "desc", "otherBucket": true, "missingBucket": false,
            "parentFormat": { "id": "terms" }
        }
    })
}

fn filters(label: &str, kql: &[(&str, &str)]) -> Value {
    let filters: Vec<Value> = kql
        .iter()
        .map(|(l, q)| json!({ "label": l, "input": { "query": q, "language": "kuery" } }))
        .collect();
    json!({
        "label": label, "dataType": "string", "operationType": "filters",
        "isBucketed": true, "scale": "ordinal", "customLabel": true,
        "params": { "filters": filters }
    })
}

fn histogram(field: &str, label: &str) -> Value {
    json!({
        "label": label, "dataType": "number", "operationType": "range",
        "sourceField": field, "isBucketed": true, "scale": "interval", "customLabel": true,
        "params": {
            "includeEmptyRows": true, "type": "histogram", "maxBars": "auto",
            "ranges": [{ "from": 0, "to": 1000, "label": "" }]
        }
    })
}

fn reference_op(op: &str, of: &str, label: &str, params: Value) -> Value {
    json!({
        "label": label, "dataType": "number", "operationType": op,
        "isBucketed": false, "scale": "ratio", "references": [of],
        "params": params, "customLabel": true
    })
}

fn with_filter(mut column: Value, kql: &str) -> Value {
    column["filter"] = json!({ "query": kql, "language": "kuery" });
    column
}

fn with_time_shift(mut column: Value, shift: &str) -> Value {
    column["timeShift"] = json!(shift);
    column
}

/// Lens "normalize by unit": `s`, `m`, `h` or `d`.
fn with_time_scale(mut column: Value, unit: &str) -> Value {
    column["timeScale"] = json!(unit);
    column
}

fn with_format(mut column: Value, format: Value) -> Value {
    column["params"]["format"] = format;
    column
}

fn percent() -> Value {
    json!({ "id": "percent", "params": { "decimals": 1 } })
}

fn hours_format() -> Value {
    json!({ "id": "number", "params": { "decimals": 1, "suffix": " h" } })
}

/// A formula column with the helper columns Lens stores beside it.
/// `parts` are the leaf columns (X0, X1, ...), `ast` the tinymath tree over
/// their ids, `text` the formula as typed.
fn formula(
    id: &str,
    label: &str,
    text: &str,
    parts: Vec<Value>,
    ast: Value,
    format: Value,
) -> Vec<(String, Value)> {
    let n = parts.len();
    let mut cols: Vec<(String, Value)> = parts
        .into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            c["label"] = json!(format!("Part of {text}"));
            c["customLabel"] = json!(true);
            (format!("{id}X{i}"), c)
        })
        .collect();
    let leaf_ids: Vec<String> = (0..n).map(|i| format!("{id}X{i}")).collect();
    cols.push((
        format!("{id}X{n}"),
        json!({
            "label": format!("Part of {text}"), "dataType": "number", "operationType": "math",
            "isBucketed": false, "scale": "ratio", "customLabel": true,
            "params": { "tinymathAst": ast }, "references": leaf_ids
        }),
    ));
    cols.push((
        id.to_string(),
        json!({
            "label": label, "dataType": "number", "operationType": "formula",
            "isBucketed": false, "scale": "ratio", "customLabel": true,
            "params": { "formula": text, "isFormulaBroken": false, "format": format },
            "references": [format!("{id}X{n}")]
        }),
    ));
    cols
}

fn divide(a: Value, b: Value, text: &str) -> Value {
    json!({ "type": "function", "name": "divide", "args": [a, b], "location": { "min": 0, "max": text.len() }, "text": text })
}

/// `count(kql='…') / count()` — the shape of change fail rate and friends.
fn ratio_formula(id: &str, label: &str, kql: &str) -> Vec<(String, Value)> {
    let text = format!("count(kql='{kql}') / count()");
    formula(
        id,
        label,
        &text,
        vec![with_filter(count(""), kql), count("")],
        divide(json!(format!("{id}X0")), json!(format!("{id}X1")), &text),
        percent(),
    )
}

/// `count() / (time_range() / 604800000)` — events per week over the range.
fn per_week_formula(id: &str, label: &str) -> Vec<(String, Value)> {
    let text = "count() / (time_range() / 604800000)";
    let time_range = json!({
        "label": "", "dataType": "number", "operationType": "time_range",
        "isBucketed": false, "scale": "ratio", "references": []
    });
    formula(
        id,
        label,
        text,
        vec![count(""), time_range],
        divide(
            json!(format!("{id}X0")),
            divide(
                json!(format!("{id}X1")),
                json!(604800000),
                "time_range() / 604800000",
            ),
            text,
        ),
        json!({ "id": "number", "params": { "decimals": 1 } }),
    )
}

// ---------------------------------------------------------------- panels

struct Grid(u32, u32, u32, u32);

struct Dashboard {
    panels: Vec<Value>,
    references: Vec<Value>,
}

impl Dashboard {
    fn new() -> Self {
        Self {
            panels: Vec::new(),
            references: Vec::new(),
        }
    }

    fn grid(index: &str, Grid(x, y, w, h): Grid) -> Value {
        json!({ "x": x, "y": y, "w": w, "h": h, "i": index })
    }

    /// A by-value Lens panel with one data layer.
    #[allow(clippy::too_many_arguments)]
    fn lens(
        &mut self,
        index: &str,
        grid: Grid,
        title: &str,
        vis_type: &str,
        data_view: &str,
        columns: Vec<(String, Value)>,
        visualization: Value,
        query: &str,
    ) {
        let layer = format!("layer-{index}");
        let (attributes, refs) = lens_attributes(
            title,
            vis_type,
            data_view,
            &layer,
            columns,
            visualization,
            query,
        );
        for r in refs {
            self.references.push(json!({ "type": r["type"], "id": r["id"], "name": format!("{index}:{}", r["name"].as_str().unwrap()) }));
        }
        self.panels.push(json!({
            "version": LENS_VERSION, "type": "lens", "panelIndex": index,
            "gridData": Self::grid(index, grid),
            "embeddableConfig": { "attributes": attributes, "enhancements": {} }
        }));
    }

    /// Adds a reference-line layer to the Lens XY panel added last: one line
    /// per `(column id, column, yConfig)`, over the panel's data view.
    fn reference_lines(&mut self, index: &str, lines: Vec<(&str, Value, Value)>) {
        let layer = format!("ref-{index}");
        let panel = self.panels.last_mut().expect("a panel");
        let attributes = &mut panel["embeddableConfig"]["attributes"];
        let data_view = attributes["references"][0]["id"].clone();
        let reference = json!({
            "type": "index-pattern", "id": data_view,
            "name": format!("indexpattern-datasource-layer-{layer}")
        });
        attributes["references"]
            .as_array_mut()
            .unwrap()
            .push(reference.clone());
        self.references.push(json!({
            "type": "index-pattern", "id": data_view,
            "name": format!("{index}:indexpattern-datasource-layer-{layer}")
        }));
        let state = &mut attributes["state"];
        let mut columns = serde_json::Map::new();
        let mut order = Vec::new();
        let mut y_config = Vec::new();
        for (id, column, mut config) in lines {
            columns.insert(id.to_string(), column);
            order.push(id.to_string());
            config["forAccessor"] = json!(id);
            y_config.push(config);
        }
        state["datasourceStates"]["formBased"]["layers"][&layer] = json!({
            "columns": columns, "columnOrder": order, "incompleteColumns": {}
        });
        state["visualization"]["layers"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "layerId": layer, "layerType": "referenceLine",
                "accessors": order, "yConfig": y_config
            }));
    }

    /// Adds a by-value annotation layer to the Lens XY panel added last; its
    /// query annotations search `data_view`.
    fn annotations(&mut self, index: &str, data_view: &str, annotations: Vec<Value>) {
        let layer = format!("events-{index}");
        let panel = self.panels.last_mut().expect("a panel");
        let attributes = &mut panel["embeddableConfig"]["attributes"];
        let name = format!("xy-visualization-layer-{layer}");
        attributes["references"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "index-pattern", "id": data_view, "name": name }));
        self.references.push(json!({
            "type": "index-pattern", "id": data_view, "name": format!("{index}:{name}")
        }));
        attributes["state"]["visualization"]["layers"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "layerId": layer, "layerType": "annotations",
                "ignoreGlobalFilters": true, "annotations": annotations
            }));
    }

    /// A panel that references a saved object (Lens or legacy visualization).
    fn by_ref(&mut self, index: &str, grid: Grid, kind: &str, id: &str) {
        let name = format!("panel_{index}");
        self.references
            .push(json!({ "type": kind, "id": id, "name": format!("{index}:{name}") }));
        self.panels.push(json!({
            "version": LENS_VERSION, "type": kind, "panelIndex": index,
            "gridData": Self::grid(index, grid), "panelRefName": name,
            "embeddableConfig": { "enhancements": {} }
        }));
    }

    fn markdown(&mut self, index: &str, grid: Grid, text: &str) {
        self.panels.push(json!({
            "version": LENS_VERSION, "type": "visualization", "panelIndex": index,
            "gridData": Self::grid(index, grid),
            "embeddableConfig": {
                "savedVis": {
                    "title": "", "description": "", "type": "markdown",
                    "params": { "fontSize": 12, "openLinksInNewTab": false, "markdown": text },
                    "uiState": {}, "data": { "aggs": [], "searchSource": {} }
                },
                "hidePanelTitles": true, "enhancements": {}
            }
        }));
    }
}

fn lens_attributes(
    title: &str,
    vis_type: &str,
    data_view: &str,
    layer: &str,
    columns: Vec<(String, Value)>,
    visualization: Value,
    query: &str,
) -> (Value, Vec<Value>) {
    // Lens lists breakdown buckets, then x-axis buckets, then metrics. A
    // panel whose columnOrder differs fails to render.
    let rank = |c: &Value| match (c["isBucketed"] == json!(true), c["operationType"].as_str()) {
        (true, Some("date_histogram" | "range")) => 1,
        (true, _) => 0,
        (false, _) => 2,
    };
    let mut order: Vec<&String> = columns.iter().map(|(id, _)| id).collect();
    order.sort_by_key(|id| rank(&columns.iter().find(|(c, _)| c == *id).unwrap().1));
    let mut cols = serde_json::Map::new();
    for (id, c) in &columns {
        cols.insert(id.clone(), c.clone());
    }
    let refs = vec![json!({
        "type": "index-pattern", "id": data_view,
        "name": format!("indexpattern-datasource-layer-{layer}")
    })];
    let attributes = json!({
        "title": title, "visualizationType": vis_type, "type": "lens",
        "references": refs,
        "state": {
            "visualization": visualization,
            "query": { "query": query, "language": "kuery" },
            "filters": [],
            "datasourceStates": { "formBased": { "layers": { layer: {
                "columns": cols, "columnOrder": order, "incompleteColumns": {}
            } } } },
            "internalReferences": [], "adHocDataViews": {}
        }
    });
    (attributes, refs)
}

fn xy(layer: &str, series: &str, x: &str, ys: &[&str], split: Option<&str>) -> Value {
    let mut layer_spec = json!({
        "layerId": layer, "layerType": "data", "seriesType": series,
        "xAccessor": x, "accessors": ys, "palette": { "type": "palette", "name": "default" }
    });
    if let Some(s) = split {
        layer_spec["splitAccessor"] = json!(s);
    }
    json!({
        "legend": { "isVisible": true, "position": "right" },
        "valueLabels": "hide", "fittingFunction": "Linear", "fillOpacity": 0.3,
        "preferredSeriesType": series,
        "axisTitlesVisibilitySettings": { "x": false, "yLeft": true, "yRight": true },
        "layers": [layer_spec]
    })
}

/// A Lens static value (reference lines use these).
fn static_value(value: f64, label: &str) -> Value {
    json!({
        "label": label, "dataType": "number", "operationType": "static_value",
        "isStaticValue": true, "isBucketed": false, "scale": "ratio",
        "params": { "value": value.to_string() }, "references": [], "customLabel": true
    })
}

/// A Lens heatmap with Kibana's defaults (temperature palette), and cell
/// labels on so the reference screenshot shows every number.
fn heatmap_vis(layer: &str, x: &str, y: &str, value: &str) -> Value {
    json!({
        "shape": "heatmap", "layerId": layer, "layerType": "data",
        "xAccessor": x, "yAccessor": y, "valueAccessor": value,
        "legend": { "isVisible": true, "position": "right", "maxLines": 1, "type": "heatmap_legend" },
        "gridConfig": {
            "type": "heatmap_grid", "isCellLabelVisible": true,
            "isYAxisLabelVisible": true, "isXAxisLabelVisible": true,
            "isYAxisTitleVisible": false, "isXAxisTitleVisible": false
        }
    })
}

fn metric_vis(layer: &str, metric: &str, secondary: Option<&str>) -> Value {
    let mut v = json!({ "layerId": layer, "layerType": "data", "metricAccessor": metric });
    if let Some(s) = secondary {
        v["secondaryMetricAccessor"] = json!(s);
    }
    v
}

fn layer_of(index: &str) -> String {
    format!("layer-{index}")
}

fn col(id: &str, c: Value) -> (String, Value) {
    (id.to_string(), c)
}

// ---------------------------------------------------------------- saved objects

fn data_view(id: &str, index: &str, time_field: &str, name: &str) -> Value {
    json!({
        "type": "index-pattern", "id": id,
        "typeMigrationVersion": "7.11.0", "coreMigrationVersion": CORE_MIGRATION,
        "attributes": {
            "title": index, "name": name, "timeFieldName": time_field,
            "fields": "[]", "fieldFormatMap": "{}", "runtimeFieldMap": "{}",
            "fieldAttrs": "{}", "typeMeta": "{}", "sourceFilters": "[]", "allowNoIndex": false
        },
        "references": []
    })
}

fn legacy_vis(
    id: &str,
    title: &str,
    vis_type: &str,
    data_view: &str,
    aggs: Value,
    params: Value,
) -> Value {
    let vis_state = json!({ "title": title, "type": vis_type, "aggs": aggs, "params": params });
    json!({
        "type": "visualization", "id": id,
        "typeMigrationVersion": VIS_MIGRATION, "coreMigrationVersion": CORE_MIGRATION,
        "attributes": {
            "title": title, "description": "", "version": 1, "uiStateJSON": "{}",
            "visState": vis_state.to_string(),
            "kibanaSavedObjectMeta": { "searchSourceJSON": json!({
                "query": { "query": "", "language": "kuery" }, "filter": [],
                "indexRefName": "kibanaSavedObjectMeta.searchSourceJSON.index"
            }).to_string() }
        },
        "references": [{ "name": "kibanaSavedObjectMeta.searchSourceJSON.index", "type": "index-pattern", "id": data_view }]
    })
}

fn lens_object(
    id: &str,
    title: &str,
    vis_type: &str,
    data_view: &str,
    columns: Vec<(String, Value)>,
    vis: impl Fn(&str) -> Value,
) -> Value {
    let layer = format!("layer-{id}");
    let (mut attributes, refs) =
        lens_attributes(title, vis_type, data_view, &layer, columns, vis(&layer), "");
    attributes.as_object_mut().unwrap().remove("references");
    attributes.as_object_mut().unwrap().remove("type");
    json!({
        "type": "lens", "id": id,
        "typeMigrationVersion": LENS_MIGRATION, "coreMigrationVersion": CORE_MIGRATION,
        "attributes": attributes, "references": refs
    })
}

#[allow(clippy::too_many_arguments)]
fn dashboard_object(
    id: &str,
    title: &str,
    description: &str,
    d: Dashboard,
    query: &str,
    filters: Value,
    time: Option<(&str, &str)>,
    controls: Option<Value>,
    filter_refs: Vec<Value>,
) -> Value {
    let mut attributes = json!({
        "title": title, "description": description, "version": 1,
        "panelsJSON": Value::Array(d.panels).to_string(),
        "optionsJSON": json!({
            "useMargins": true, "syncColors": false, "syncCursor": true,
            "syncTooltips": false, "hidePanelTitles": false
        }).to_string(),
        "timeRestore": time.is_some(),
        "kibanaSavedObjectMeta": { "searchSourceJSON": json!({
            "query": { "query": query, "language": "kuery" }, "filter": filters
        }).to_string() }
    });
    if let Some((from, to)) = time {
        attributes["timeFrom"] = json!(from);
        attributes["timeTo"] = json!(to);
        attributes["refreshInterval"] = json!({ "pause": true, "value": 60000 });
    }
    if let Some(c) = controls {
        attributes["controlGroupInput"] = c;
    }
    let mut references = d.references;
    references.extend(filter_refs);
    json!({
        "type": "dashboard", "id": id,
        "typeMigrationVersion": DASHBOARD_MIGRATION, "coreMigrationVersion": CORE_MIGRATION,
        "attributes": attributes, "references": references
    })
}

// ---------------------------------------------------------------- dashboards

fn delivery_overview() -> Value {
    let mut d = Dashboard::new();
    d.markdown(
        "intro",
        Grid(0, 0, 48, 4),
        "## Delivery overview\nDORA metrics across all teams. Medians and percentiles, not averages: delivery durations are long-tailed.",
    );

    let mut cols = per_week_formula("dpw", "Deployments / week");
    let l = layer_of("deploys-per-week");
    cols.push(col(
        "dpw-prev",
        with_time_shift(count("Deployments in previous period"), "previous"),
    ));
    d.lens(
        "deploys-per-week",
        Grid(0, 4, 12, 7),
        "Deployment frequency",
        "lnsMetric",
        DV_DEPLOYS,
        cols,
        metric_vis(&l, "dpw", Some("dpw-prev")),
        "is_rework : false",
    );

    let l = layer_of("lead-time");
    d.lens(
        "lead-time",
        Grid(12, 4, 12, 7),
        "Change lead time (median)",
        "lnsMetric",
        DV_DEPLOYS,
        vec![col(
            "lt-median",
            with_format(
                metric("median", "change_lead_time_hours", "Median lead time"),
                hours_format(),
            ),
        )],
        metric_vis(&l, "lt-median", None),
        "is_rework : false",
    );

    let l = layer_of("cfr");
    d.lens(
        "cfr",
        Grid(24, 4, 12, 7),
        "Change fail rate",
        "lnsMetric",
        DV_DEPLOYS,
        ratio_formula("cfr-f", "Change fail rate", "result : \"failure\""),
        metric_vis(&l, "cfr-f", None),
        "is_rework : false",
    );

    let l = layer_of("recovery");
    d.lens(
        "recovery",
        Grid(36, 4, 12, 7),
        "Failed deployment recovery time",
        "lnsMetric",
        DV_INCIDENTS,
        vec![col(
            "rec-median",
            with_format(
                metric("median", "recovery_hours", "Median recovery"),
                hours_format(),
            ),
        )],
        metric_vis(&l, "rec-median", None),
        "caused_by_deployment_id : *",
    );

    let l = layer_of("deploys-weekly");
    d.lens(
        "deploys-weekly",
        Grid(0, 11, 24, 12),
        "Deployments per week by result",
        "lnsXY",
        DV_DEPLOYS,
        vec![
            col("dw-x", date_histogram("deployed_at", "1w")),
            col("dw-y", count("Deployments")),
            col("dw-split", terms("result", 3, "dw-y")),
        ],
        xy(&l, "bar_stacked", "dw-x", &["dw-y"], Some("dw-split")),
        "",
    );

    let l = layer_of("lead-percentiles");
    d.lens(
        "lead-percentiles",
        Grid(24, 11, 24, 12),
        "Lead time percentiles (weekly)",
        "lnsXY",
        DV_DEPLOYS,
        vec![
            col("lp-x", date_histogram("deployed_at", "1w")),
            col("lp-50", percentile("change_lead_time_hours", 50, "p50")),
            col("lp-75", percentile("change_lead_time_hours", 75, "p75")),
            col("lp-95", percentile("change_lead_time_hours", 95, "p95")),
        ],
        xy(&l, "line", "lp-x", &["lp-50", "lp-75", "lp-95"], None),
        "is_rework : false",
    );

    let mut cols = vec![col("cw-x", date_histogram("deployed_at", "1w"))];
    cols.extend(ratio_formula(
        "cw-f",
        "Change fail rate",
        "result : \"failure\"",
    ));
    cols.push(col(
        "cw-avg",
        reference_op(
            "moving_average",
            "cw-f",
            "4-week moving average",
            json!({ "window": 4, "format": percent() }),
        ),
    ));
    let l = layer_of("cfr-weekly");
    d.lens(
        "cfr-weekly",
        Grid(0, 23, 24, 12),
        "Change fail rate (weekly, 4-week moving average)",
        "lnsXY",
        DV_DEPLOYS,
        cols,
        xy(&l, "line", "cw-x", &["cw-f", "cw-avg"], None),
        "is_rework : false",
    );

    let mut cols = vec![col("rw-x", date_histogram("deployed_at", "1M"))];
    cols.extend(ratio_formula(
        "rw-f",
        "Deployment rework rate",
        "is_rework : true",
    ));
    let l = layer_of("rework");
    d.lens(
        "rework",
        Grid(24, 23, 24, 12),
        "Deployment rework rate (monthly)",
        "lnsXY",
        DV_DEPLOYS,
        cols,
        xy(&l, "bar", "rw-x", &["rw-f"], None),
        "",
    );

    let mut cols = vec![
        col("tt-team", terms("team", 20, "tt-n")),
        col("tt-n", count("Deployments")),
        col(
            "tt-lead",
            with_format(
                metric("median", "change_lead_time_hours", "Median lead time"),
                hours_format(),
            ),
        ),
    ];
    cols.extend(ratio_formula(
        "tt-cfr",
        "Change fail rate",
        "result : \"failure\"",
    ));
    let l = layer_of("teams");
    let table = json!({
        "layerId": l, "layerType": "data",
        "columns": [{ "columnId": "tt-team" }, { "columnId": "tt-n" }, { "columnId": "tt-lead" }, { "columnId": "tt-cfr" }]
    });
    d.lens(
        "teams",
        Grid(0, 35, 48, 14),
        "DORA by team",
        "lnsDatatable",
        DV_DEPLOYS,
        cols,
        table,
        "is_rework : false",
    );

    // Normalized by unit: calendar months of different lengths, and a last
    // month the time range cuts short, are each divided by their own length.
    let l = layer_of("per-day-monthly");
    let table = json!({
        "layerId": l, "layerType": "data",
        "columns": [{ "columnId": "pd-x" }, { "columnId": "pd-n" }]
    });
    d.lens(
        "per-day-monthly",
        Grid(0, 49, 24, 12),
        "Deployments per day (monthly)",
        "lnsDatatable",
        DV_DEPLOYS,
        vec![
            col("pd-x", date_histogram("deployed_at", "1M")),
            col("pd-n", with_time_scale(count("Deployments per day"), "d")),
        ],
        table,
        "",
    );

    let l = layer_of("per-day");
    d.lens(
        "per-day",
        Grid(24, 49, 24, 12),
        "Deployments per day",
        "lnsMetric",
        DV_DEPLOYS,
        vec![col(
            "pt-n",
            with_time_scale(count("Deployments per day"), "d"),
        )],
        metric_vis(&l, "pt-n", None),
        "",
    );

    // Reference lines: a fixed target (shaded below) and a quick function
    // computed over the whole range.
    let l = layer_of("lead-target");
    d.lens(
        "lead-target",
        Grid(0, 61, 24, 12),
        "Lead time against target (weekly median)",
        "lnsXY",
        DV_DEPLOYS,
        vec![
            col("lg-x", date_histogram("deployed_at", "1w")),
            col(
                "lg-y",
                with_format(
                    metric("median", "change_lead_time_hours", "Median lead time"),
                    hours_format(),
                ),
            ),
        ],
        xy(&l, "line", "lg-x", &["lg-y"], None),
        "is_rework : false",
    );
    d.reference_lines(
        "lead-target",
        vec![
            (
                "lg-target",
                static_value(24.0, "Target: one day"),
                json!({ "axisMode": "left", "color": "#54B399", "lineStyle": "dashed", "lineWidth": 2, "fill": "below", "textVisibility": true }),
            ),
            (
                "lg-avg",
                metric("average", "change_lead_time_hours", "Average over the range"),
                json!({ "axisMode": "left", "color": "#D6BF57", "lineStyle": "dotted", "lineWidth": 2, "fill": "none", "textVisibility": true }),
            ),
        ],
    );

    d.annotations(
        "lead-target",
        DV_INCIDENTS,
        vec![
            json!({
                "type": "query", "id": "sev1", "label": "Sev1 incidents",
                "key": { "type": "point_in_time" }, "timeField": "created_at",
                "filter": { "type": "kibana_query", "query": "severity : sev1", "language": "kuery" },
                "textField": "team", "textVisibility": true,
                "color": "#B877D9", "lineStyle": "dashed", "lineWidth": 2, "icon": "alert"
            }),
            json!({
                "type": "manual", "id": "freeze", "label": "Release freeze",
                "key": { "type": "range", "timestamp": "2026-07-01T00:00:00.000Z", "endTimestamp": "2026-07-08T00:00:00.000Z" },
                "color": "#D6BF5733", "outside": false
            }),
            json!({
                "type": "manual", "id": "reorg", "label": "Reorg",
                "key": { "type": "point_in_time", "timestamp": "2026-08-17T00:00:00.000Z" },
                "color": "#54B399", "lineStyle": "solid", "lineWidth": 2, "icon": "triangle",
                "textVisibility": true
            }),
        ],
    );

    let l = layer_of("team-target");
    d.lens(
        "team-target",
        Grid(24, 61, 24, 12),
        "Median lead time by team against target",
        "lnsXY",
        DV_DEPLOYS,
        vec![
            col("tg-team", terms("team", 10, "tg-y")),
            col(
                "tg-y",
                with_format(
                    metric("median", "change_lead_time_hours", "Median lead time"),
                    hours_format(),
                ),
            ),
        ],
        xy(&l, "bar_horizontal", "tg-team", &["tg-y"], None),
        "is_rework : false",
    );
    d.reference_lines(
        "team-target",
        vec![(
            "tg-target",
            static_value(24.0, "One day"),
            json!({ "axisMode": "left", "color": "#E7664C", "lineStyle": "solid", "lineWidth": 2, "fill": "none", "textVisibility": true }),
        )],
    );

    // A heatmap: deployments per month and team.
    let l = layer_of("team-heatmap");
    d.lens(
        "team-heatmap",
        Grid(0, 73, 48, 14),
        "Deployments per month by team",
        "lnsHeatmap",
        DV_DEPLOYS,
        vec![
            col("hm-team", terms("team", 10, "hm-n")),
            col("hm-month", date_histogram("deployed_at", "1M")),
            col("hm-n", count("Deployments")),
        ],
        heatmap_vis(&l, "hm-month", "hm-team", "hm-n"),
        "",
    );

    dashboard_object(
        "teleplot-delivery-overview",
        "[Delivery] Overview (DORA)",
        "DORA throughput and instability metrics",
        d,
        "",
        json!([]),
        Some(("now-90d/d", "now")),
        None,
        vec![],
    )
}

fn pr_flow() -> Vec<Value> {
    let mut d = Dashboard::new();

    let l = layer_of("contributors");
    d.lens(
        "contributors",
        Grid(0, 0, 12, 7),
        "Active contributors",
        "lnsMetric",
        DV_PRS,
        vec![col("ac", metric("unique_count", "author", "Contributors"))],
        metric_vis(&l, "ac", None),
        "",
    );

    let l = layer_of("throughput");
    d.lens(
        "throughput",
        Grid(12, 0, 12, 7),
        "PRs merged",
        "lnsMetric",
        DV_PRS,
        vec![
            col("tp", count("Merged PRs")),
            col(
                "tp-prev",
                with_time_shift(count("Previous period"), "previous"),
            ),
        ],
        metric_vis(&l, "tp", Some("tp-prev")),
        "state : \"merged\"",
    );

    let l = layer_of("ai-share");
    d.lens(
        "ai-share",
        Grid(24, 0, 12, 14),
        "AI-assisted PRs",
        "lnsPie",
        DV_PRS,
        vec![col("ai-t", terms("ai_assisted", 2, "ai-n")), col("ai-n", count("PRs"))],
        json!({
            "shape": "donut",
            "layers": [{
                "layerId": l, "layerType": "data", "primaryGroups": ["ai-t"], "metrics": ["ai-n"],
                "numberDisplay": "percent", "categoryDisplay": "default", "legendDisplay": "default", "nestedLegend": false
            }]
        }),
        "",
    );

    let l = layer_of("last-pr");
    d.lens(
        "last-pr",
        Grid(36, 0, 12, 7),
        "Most recent merged PR",
        "lnsMetric",
        DV_PRS,
        vec![col("lv", last_value("pr_id", "merged_at", "Latest PR"))],
        metric_vis(&l, "lv", None),
        "state : \"merged\"",
    );

    let l = layer_of("phases");
    d.lens(
        "phases",
        Grid(0, 7, 24, 12),
        "Cycle time by phase (weekly median)",
        "lnsXY",
        DV_PRS,
        vec![
            col("ph-x", date_histogram("merged_at", "1w")),
            col("ph-c", metric("median", "coding_hours", "Coding")),
            col("ph-p", metric("median", "pickup_hours", "Pickup")),
            col("ph-r", metric("median", "review_hours", "Review")),
            col("ph-d", metric("median", "deploy_hours", "Deploy")),
        ],
        xy(
            &l,
            "bar_stacked",
            "ph-x",
            &["ph-c", "ph-p", "ph-r", "ph-d"],
            None,
        ),
        "",
    );

    let l = layer_of("cycle-pct");
    d.lens(
        "cycle-pct",
        Grid(36, 7, 12, 7),
        "Cycle time p85 change",
        "lnsXY",
        DV_PRS,
        vec![
            col("cp-x", date_histogram("merged_at", "1M")),
            col("cp-85", percentile("cycle_time_hours", 85, "p85")),
            col(
                "cp-diff",
                reference_op("differences", "cp-85", "Change in p85", json!({})),
            ),
        ],
        xy(&l, "bar", "cp-x", &["cp-diff"], None),
        "",
    );

    let l = layer_of("by-team");
    d.lens(
        "by-team",
        Grid(24, 14, 24, 12),
        "Merged PRs per week by team",
        "lnsXY",
        DV_PRS,
        vec![
            col("bt-x", date_histogram("merged_at", "1w")),
            col("bt-y", count("PRs")),
            col("bt-split", terms("team", 10, "bt-y")),
        ],
        xy(&l, "area_stacked", "bt-x", &["bt-y"], Some("bt-split")),
        "",
    );

    let l = layer_of("size-mix");
    d.lens(
        "size-mix",
        Grid(0, 19, 24, 12),
        "PR size mix",
        "lnsXY",
        DV_PRS,
        vec![
            col("sm-x", date_histogram("merged_at", "1M")),
            col("sm-y", count("PRs")),
            col(
                "sm-split",
                filters(
                    "Size",
                    &[
                        ("Small", "lines_changed < 100"),
                        ("Medium", "lines_changed >= 100 and lines_changed < 500"),
                        ("Large", "lines_changed >= 500"),
                    ],
                ),
            ),
        ],
        xy(
            &l,
            "bar_percentage_stacked",
            "sm-x",
            &["sm-y"],
            Some("sm-split"),
        ),
        "",
    );

    let l = layer_of("cumulative");
    d.lens(
        "cumulative",
        Grid(24, 26, 24, 10),
        "Cumulative PRs merged (AI-assisted vs all)",
        "lnsXY",
        DV_PRS,
        vec![
            col("cu-x", date_histogram("merged_at", "1d")),
            col("cu-n", count("")),
            col(
                "cu-sum",
                reference_op("cumulative_sum", "cu-n", "All PRs", json!({})),
            ),
            col("cu-ai", with_filter(count(""), "ai_assisted : true")),
            col(
                "cu-ai-sum",
                reference_op("cumulative_sum", "cu-ai", "AI-assisted", json!({})),
            ),
        ],
        xy(&l, "line", "cu-x", &["cu-sum", "cu-ai-sum"], None),
        "",
    );

    let l = layer_of("distribution");
    d.lens(
        "distribution",
        Grid(0, 31, 24, 12),
        "Cycle time distribution",
        "lnsXY",
        DV_PRS,
        vec![
            col("ds-x", histogram("cycle_time_hours", "Cycle time (h)")),
            col("ds-y", count("PRs")),
        ],
        xy(&l, "bar", "ds-x", &["ds-y"], None),
        "",
    );

    d.by_ref(
        "lens-ref",
        Grid(24, 36, 24, 12),
        "lens",
        "teleplot-lens-review-load",
    );
    d.by_ref(
        "legacy-line",
        Grid(0, 43, 24, 12),
        "visualization",
        "teleplot-vis-cycle-line",
    );

    let lens_ref = lens_object(
        "teleplot-lens-review-load",
        "Review load by team (by reference)",
        "lnsXY",
        DV_PRS,
        vec![
            col("rl-team", terms("team", 10, "rl-n")),
            col("rl-n", count("PRs awaiting review")),
        ],
        |layer| xy(layer, "bar_horizontal", "rl-team", &["rl-n"], None),
    );

    let legacy_line = legacy_vis(
        "teleplot-vis-cycle-line",
        "Cycle time p50/p85 (legacy line)",
        "line",
        DV_PRS,
        json!([
            { "id": "1", "enabled": true, "type": "percentiles", "schema": "metric", "params": { "field": "cycle_time_hours", "percents": [50, 85] } },
            { "id": "2", "enabled": true, "type": "date_histogram", "schema": "segment", "params": {
                "field": "merged_at", "interval": "w", "min_doc_count": 1, "extended_bounds": {},
                "useNormalizedEsInterval": true, "scaleMetricValues": false, "drop_partials": false
            } }
        ]),
        legacy_line_params("Cycle time (h)"),
    );

    let dash = dashboard_object(
        "teleplot-pr-flow",
        "[Delivery] PR flow",
        "Pull request throughput, cycle time and AI assistance",
        d,
        "",
        json!([]),
        None,
        None,
        vec![],
    );
    vec![lens_ref, legacy_line, dash]
}

fn investment() -> Vec<Value> {
    let mut d = Dashboard::new();

    let l = layer_of("allocation");
    d.lens(
        "allocation",
        Grid(0, 0, 32, 14),
        "Investment allocation (monthly)",
        "lnsXY",
        DV_ITEMS,
        vec![
            col("al-x", date_histogram("done_at", "1M")),
            col("al-y", metric("sum", "story_points", "Story points")),
            col("al-split", terms("type", 4, "al-y")),
        ],
        xy(
            &l,
            "bar_percentage_stacked",
            "al-x",
            &["al-y"],
            Some("al-split"),
        ),
        "",
    );

    d.by_ref(
        "legacy-metric",
        Grid(32, 0, 16, 7),
        "visualization",
        "teleplot-vis-items-done",
    );
    d.by_ref(
        "legacy-pie",
        Grid(32, 7, 16, 14),
        "visualization",
        "teleplot-vis-type-pie",
    );

    let l = layer_of("efficiency");
    d.lens(
        "efficiency",
        Grid(0, 14, 32, 12),
        "Flow efficiency and flow time",
        "lnsXY",
        DV_ITEMS,
        vec![
            col("ef-x", date_histogram("done_at", "1w")),
            col(
                "ef-e",
                with_format(
                    metric("average", "flow_efficiency", "Flow efficiency"),
                    percent(),
                ),
            ),
            col(
                "ef-t",
                metric("median", "flow_time_days", "Median flow time (days)"),
            ),
        ],
        xy(&l, "line", "ef-x", &["ef-e", "ef-t"], None),
        "",
    );

    d.by_ref(
        "legacy-table",
        Grid(0, 26, 48, 12),
        "visualization",
        "teleplot-vis-team-table",
    );

    let items_done = legacy_vis(
        "teleplot-vis-items-done",
        "Items done (legacy metric)",
        "metric",
        DV_ITEMS,
        json!([{ "id": "1", "enabled": true, "type": "count", "schema": "metric", "params": {} }]),
        json!({
            "addTooltip": true, "addLegend": false, "type": "metric",
            "metric": {
                "percentageMode": false, "useRanges": false, "colorSchema": "Green to Red",
                "metricColorMode": "None", "colorsRange": [{ "from": 0, "to": 10000 }],
                "labels": { "show": true }, "invertColors": false,
                "style": { "bgFill": "#000", "bgColor": false, "labelColor": false, "subText": "", "fontSize": 60 }
            }
        }),
    );
    let type_pie = legacy_vis(
        "teleplot-vis-type-pie",
        "Work by type (legacy pie)",
        "pie",
        DV_ITEMS,
        json!([
            { "id": "1", "enabled": true, "type": "count", "schema": "metric", "params": {} },
            { "id": "2", "enabled": true, "type": "terms", "schema": "segment", "params": {
                "field": "type", "size": 5, "order": "desc", "orderBy": "1",
                "otherBucket": false, "otherBucketLabel": "Other", "missingBucket": false, "missingBucketLabel": "Missing"
            } }
        ]),
        json!({
            "type": "pie", "addTooltip": true, "legendDisplay": "show", "legendPosition": "right",
            "isDonut": true, "distinctColors": true, "nestedLegend": false,
            "palette": { "type": "palette", "name": "default" },
            "labels": { "show": false, "values": true, "last_level": true, "truncate": 100 }
        }),
    );
    let team_table = legacy_vis(
        "teleplot-vis-team-table",
        "Flow by team (legacy table)",
        "table",
        DV_ITEMS,
        json!([
            { "id": "1", "enabled": true, "type": "count", "schema": "metric", "params": { "customLabel": "Items" } },
            { "id": "3", "enabled": true, "type": "avg", "schema": "metric", "params": { "field": "flow_efficiency", "customLabel": "Avg flow efficiency" } },
            { "id": "4", "enabled": true, "type": "median", "schema": "metric", "params": { "field": "flow_time_days", "customLabel": "Median flow time (days)" } },
            { "id": "2", "enabled": true, "type": "terms", "schema": "bucket", "params": {
                "field": "team", "size": 20, "order": "desc", "orderBy": "1",
                "otherBucket": false, "otherBucketLabel": "Other", "missingBucket": false, "missingBucketLabel": "Missing"
            } }
        ]),
        json!({
            "perPage": 10, "showPartialRows": false, "showMetricsAtAllLevels": false,
            "showTotal": false, "totalFunc": "sum", "percentageCol": "", "autoFitRowToContent": false, "row": true
        }),
    );

    let filter = json!([{
        "meta": {
            "alias": null, "negate": true, "disabled": false, "type": "phrase", "key": "type",
            "params": { "query": "risk" },
            "indexRefName": "kibanaSavedObjectMeta.searchSourceJSON.filter[0].meta.index"
        },
        "query": { "match_phrase": { "type": "risk" } },
        "$state": { "store": "appState" }
    }]);
    let filter_refs = vec![json!({
        "name": "kibanaSavedObjectMeta.searchSourceJSON.filter[0].meta.index",
        "type": "index-pattern", "id": DV_ITEMS
    })];
    let controls = json!({
        "chainingSystem": "HIERARCHICAL", "controlStyle": "oneLine", "ignoreParentSettingsJSON":
            json!({ "ignoreFilters": false, "ignoreQuery": false, "ignoreTimerange": false, "ignoreValidations": false }).to_string(),
        "panelsJSON": json!({
            "team-control": {
                "order": 0, "width": "medium", "grow": true, "type": "optionsListControl",
                "explicitInput": { "id": "team-control", "title": "Team", "fieldName": "team", "dataViewId": DV_ITEMS, "enhancements": {} }
            }
        }).to_string()
    });

    let dash = dashboard_object(
        "teleplot-investment",
        "[Delivery] Investment and flow",
        "Where effort goes, and how efficiently work flows",
        d,
        "NOT status : \"todo\"",
        filter,
        Some(("now-6M/M", "now")),
        Some(controls),
        filter_refs,
    );
    vec![items_done, type_pie, team_table, dash]
}

fn legacy_line_params(axis_title: &str) -> Value {
    json!({
        "type": "line", "grid": { "categoryLines": false },
        "categoryAxes": [{
            "id": "CategoryAxis-1", "type": "category", "position": "bottom", "show": true, "style": {},
            "scale": { "type": "linear" }, "labels": { "show": true, "filter": true, "truncate": 100 }, "title": {}
        }],
        "valueAxes": [{
            "id": "ValueAxis-1", "name": "LeftAxis-1", "type": "value", "position": "left", "show": true, "style": {},
            "scale": { "type": "linear", "mode": "normal" },
            "labels": { "show": true, "rotate": 0, "filter": false, "truncate": 100 },
            "title": { "text": axis_title }
        }],
        "seriesParams": [{
            "show": true, "type": "line", "mode": "normal", "data": { "label": axis_title, "id": "1" },
            "valueAxis": "ValueAxis-1", "drawLinesBetweenPoints": true, "lineWidth": 2,
            "interpolate": "linear", "showCircles": true
        }],
        "addTooltip": true, "addLegend": true, "legendPosition": "right", "times": [],
        "addTimeMarker": false, "labels": {}, "detailedTooltip": true,
        "thresholdLine": { "show": false, "value": 10, "width": 1, "style": "full", "color": "#E7664C" },
        "palette": { "type": "palette", "name": "default" }, "isVislibVis": false,
        "fittingFunction": "linear", "radiusRatio": 9
    })
}

// ---------------------------------------------------------------- main

fn objects() -> Vec<Value> {
    let mut objects = vec![
        data_view(
            DV_PRS,
            "delivery-prs",
            "merged_at",
            "Delivery: pull requests",
        ),
        data_view(
            DV_DEPLOYS,
            "delivery-deployments",
            "deployed_at",
            "Delivery: deployments",
        ),
        data_view(
            DV_INCIDENTS,
            "delivery-incidents",
            "created_at",
            "Delivery: incidents",
        ),
        data_view(
            DV_ITEMS,
            "delivery-workitems",
            "done_at",
            "Delivery: work items",
        ),
        delivery_overview(),
    ];
    objects.extend(pr_flow());
    objects.extend(investment());
    objects
}

#[tokio::main]
async fn main() {
    let mut kibana = "http://localhost:5608".to_string();
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--kibana" => kibana = value.trim_end_matches('/').into(),
            "--out" => out = Some(value),
            other => panic!("unknown flag {other}"),
        }
    }
    let out = out.expect("--out <path> is required");

    let objects = objects();
    let ndjson: String = objects.iter().map(|o| format!("{o}\n")).collect();
    let client = reqwest::Client::new();

    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::text(ndjson)
            .file_name("delivery.ndjson")
            .mime_str("application/ndjson")
            .unwrap(),
    );
    let response = client
        .post(format!("{kibana}/api/saved_objects/_import?overwrite=true"))
        .header("kbn-xsrf", "true")
        .multipart(form)
        .send()
        .await
        .expect("reach Kibana");
    let result: Value = response.json().await.unwrap();
    if result["success"] != json!(true) {
        panic!(
            "import failed: {}",
            serde_json::to_string_pretty(&result).unwrap()
        );
    }
    println!("imported {} objects", result["successCount"]);

    let dashboards: Vec<Value> = objects
        .iter()
        .filter(|o| o["type"] == "dashboard")
        .map(|o| json!({ "type": "dashboard", "id": o["id"] }))
        .collect();
    let export = client
        .post(format!("{kibana}/api/saved_objects/_export"))
        .header("kbn-xsrf", "true")
        .json(&json!({ "objects": dashboards, "includeReferencesDeep": true }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    std::fs::write(&out, &export).expect("write fixture");
    println!("exported {} lines to {out}", export.lines().count());
}
