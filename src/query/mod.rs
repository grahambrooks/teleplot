//! Charts → Elasticsearch searches → tables.
//!
//! Each chart layer becomes one search per distinct time shift. Bucket
//! columns nest in layer order (outermost first) and metric columns sit at
//! the innermost level, so the flattened response is the same row-per-bucket
//! table Lens builds. Formulas and pipeline functions (moving average,
//! cumulative sum, differences, overall_*) are then evaluated here over those
//! rows, following Kibana's expression functions exactly.

mod compute;
mod interval;

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::es::Search;
use crate::kql::{self, Field};
use crate::model::{
    Annotation, AnnotationData, Cell, Chart, Column, DataView, Event, Filter, Layer, LayerData,
    Operation, PanelData, Query, QueryLanguage, Series, TermsOrder,
};
use crate::time::Range;

/// Kibana's `histogram:barTarget` and `histogram:maxBars`.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub bar_target: u32,
    pub max_bars: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            bar_target: 50,
            max_bars: 100,
        }
    }
}

/// Everything outside the chart that shapes its queries.
pub struct Context<'a> {
    pub range: Range,
    /// IANA zone for calendar buckets and date matches.
    pub time_zone: &'a str,
    /// Dashboard query, the viewer's query bar, and filter pills.
    pub queries: &'a [Query],
    pub filters: &'a [Filter],
    /// Values chosen in dashboard controls, per field.
    pub selections: &'a [(String, Vec<String>)],
    /// Ad-hoc filter pills: applied only where the index has their field.
    pub adhoc: &'a [Filter],
    /// Fields of each index pattern, from `_field_caps`.
    pub fields: &'a HashMap<String, Vec<Field>>,
    /// Min/max per (layer, column) for auto-interval histograms; see [`stats_searches`].
    pub stats: &'a HashMap<(String, String), (f64, f64)>,
    pub settings: Settings,
    /// The dashboard's annotations, marked on every time-series chart.
    pub annotations: &'a [Annotation],
}

/// Searches for the min/max of fields bucketed by auto-interval histograms
/// (Kibana fetches these first, too). Keyed by (layer, column).
pub fn stats_searches(chart: &Chart, ctx: &Context) -> Vec<((String, String), Search)> {
    let mut out = Vec::new();
    for layer in &chart.layers {
        for column in &layer.columns {
            if let Operation::Histogram {
                field,
                interval: None,
            } = &column.op
            {
                let Ok(query) = base_query(chart, layer, ctx, ctx.range) else {
                    continue;
                };
                let body = json!({
                    "size": 0, "track_total_hits": false, "query": query,
                    "aggs": { "min": { "min": { "field": field } }, "max": { "max": { "field": field } } }
                });
                out.push((
                    (layer.id.clone(), column.id.clone()),
                    Search::dsl(layer.data_view.index.clone(), body),
                ));
            }
        }
    }
    out
}

/// The top values of a field, for dashboard controls. Read the buckets from
/// `aggregations.values.buckets`.
pub fn terms_search(
    view: &DataView,
    field: &str,
    size: u32,
    ctx: &Context,
) -> Result<Search, String> {
    let mut filter = Vec::new();
    let mut must_not = Vec::new();
    let fields = fields(ctx, view);
    let options = kql::Options {
        time_zone: Some(ctx.time_zone.to_string()),
    };
    if let Some(time_field) = &view.time_field {
        filter.push(json!({ "range": { time_field: { "gte": ctx.range.from_ms, "lte": ctx.range.to_ms, "format": "epoch_millis" } } }));
    }
    for q in ctx.queries {
        if let Some(dsl) = query_dsl(q, &fields, &options)? {
            filter.push(dsl);
        }
    }
    for f in ctx.filters.iter().filter(|f| !f.disabled) {
        if f.negate {
            must_not.push(f.query.clone())
        } else {
            filter.push(f.query.clone())
        }
    }
    adhoc(ctx, &fields, &mut filter, &mut must_not);
    for (f, values) in ctx.selections.iter().filter(|(_, v)| !v.is_empty()) {
        filter.push(json!({ "terms": { f: values } }));
    }
    Ok(Search::dsl(
        view.index.clone(),
        json!({
            "size": 0, "track_total_hits": false,
            "query": { "bool": { "filter": filter, "must_not": must_not } },
            "aggs": { "values": { "terms": { "field": field, "size": size } } }
        }),
    ))
}

/// Reads a stats search response.
pub fn read_stats(response: &Value) -> Option<(f64, f64)> {
    let aggs = &response["aggregations"];
    Some((
        aggs["min"]["value"].as_f64()?,
        aggs["max"]["value"].as_f64()?,
    ))
}

/// Stands in for the response to a group that needs no search.
static NO_SEARCH: std::sync::LazyLock<Result<Value, String>> =
    std::sync::LazyLock::new(|| Ok(json!({ "aggregations": { "root": {} } })));

/// The searches for one chart, and how to turn their responses into data.
#[derive(Debug)]
pub struct Plan {
    layers: Vec<LayerPlan>,
    warnings: Vec<String>,
    annotations: Vec<AnnotationPlan>,
}

#[derive(Debug)]
struct AnnotationPlan {
    data: AnnotationData,
    /// The search for its documents; `None` for fixed events or an error.
    search: Option<(Search, AnnotationFields)>,
}

/// Which document fields hold an annotation's time, end and text.
#[derive(Debug)]
struct AnnotationFields {
    time: String,
    end: Option<String>,
    text: Option<String>,
}

#[derive(Debug)]
struct LayerPlan {
    layer: Layer,
    built: Result<Built, String>,
}

#[derive(Debug)]
struct Built {
    buckets: Vec<BucketSpec>,
    groups: Vec<ShiftGroup>,
    /// Set for document layers: which field fills which column.
    documents: Option<Vec<DocField>>,
    /// Set for ES|QL layers: which result column fills which column.
    esql: Option<Vec<EsqlField>>,
    /// The time range and zone, for normalizing by unit ([`Column::per`]).
    range: Range,
    time_zone: jiff::tz::TimeZone,
}

#[derive(Debug)]
struct EsqlField {
    column: String,
    name: String,
}

#[derive(Debug)]
struct DocField {
    column: String,
    field: String,
    date: bool,
}

#[derive(Debug)]
struct BucketSpec {
    column: String,
    kind: BucketKind,
}

#[derive(Debug)]
enum BucketKind {
    /// A date histogram, with the length of one bucket.
    Date(jiff::Span),
    Terms,
    Filters(Vec<String>),
    Numeric,
    Ranges,
    /// A log histogram: bucket `i` is keyed by its lower bound.
    LogRanges(Vec<f64>),
}

#[derive(Debug)]
struct ShiftGroup {
    /// Milliseconds the searched range lies before the main range.
    shift_ms: i64,
    metrics: Vec<MetricSpec>,
    /// `None` when nothing needs Elasticsearch: no buckets, and only
    /// constants (a static reference line).
    search: Option<Search>,
}

#[derive(Debug)]
struct MetricSpec {
    column: String,
    agg: String,
    read: Read,
}

#[derive(Debug, Clone)]
enum Read {
    DocCount,
    Value,
    FirstOfValues,
    StdDeviation,
    TopHit(String),
    Constant(f64),
    /// A `filter` agg wrapping the real metric.
    Filtered(Box<Read>),
}

impl Plan {
    pub fn searches(&self) -> Vec<Search> {
        self.described_searches()
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    /// [`Plan::searches`], each with what it is for (for the inspector).
    pub fn described_searches(&self) -> Vec<(String, Search)> {
        self.layers
            .iter()
            .filter_map(|l| Some((&l.layer, l.built.as_ref().ok()?)))
            .flat_map(|(layer, b)| {
                b.groups.iter().filter_map(move |g| {
                    let search = g.search.clone()?;
                    let mut what = format!("Layer {}", layer.id);
                    if b.documents.is_some() {
                        what.push_str(", documents");
                    }
                    if layer.insight.is_some() {
                        what.push_str(", insight series");
                    } else if b.esql.is_some() {
                        what.push_str(", ES|QL");
                    }
                    if g.shift_ms != 0 {
                        what.push_str(&format!(", shifted {} earlier", duration(g.shift_ms)));
                    }
                    Some((what, search))
                })
            })
            .chain(self.annotations.iter().filter_map(|a| {
                let (search, _) = a.search.as_ref()?;
                Some((format!("Annotation {}", a.data.label), search.clone()))
            }))
            .collect()
    }

    /// Turns the responses to [`Plan::searches`] (same order) into data.
    pub fn evaluate(&self, responses: &[Result<Value, String>]) -> PanelData {
        let mut responses = responses.iter();
        let mut data = PanelData {
            layers: Vec::new(),
            warnings: self.warnings.clone(),
            annotations: Vec::new(),
        };
        for plan in &self.layers {
            let built = match &plan.built {
                Ok(b) => b,
                Err(e) => {
                    data.layers.push(LayerData {
                        layer: plan.layer.id.clone(),
                        error: Some(e.clone()),
                        ..Default::default()
                    });
                    continue;
                }
            };
            let group_responses: Vec<_> = built
                .groups
                .iter()
                .map(|g| match g.search {
                    Some(_) => responses.next(),
                    None => Some(&*NO_SEARCH),
                })
                .collect();
            let layer = match evaluate_layer(plan, built, &group_responses) {
                Ok(mut layer) => {
                    shorten_columns(&plan.layer, &mut layer);
                    layer
                }
                Err(e) => LayerData {
                    layer: plan.layer.id.clone(),
                    error: Some(e),
                    ..Default::default()
                },
            };
            data.layers.push(layer);
        }
        for plan in &self.annotations {
            let mut annotation = plan.data.clone();
            if let Some((_, fields)) = &plan.search {
                match responses.next() {
                    Some(Ok(response)) => annotation.events = read_events(response, fields),
                    Some(Err(e)) => annotation.error = Some(e.clone()),
                    None => annotation.error = Some("missing response".into()),
                }
            }
            data.annotations.push(annotation);
        }
        data
    }
}

/// Annotation events from a search's hits, oldest first.
fn read_events(response: &Value, fields: &AnnotationFields) -> Vec<Event> {
    let ms = |hit: &Value, field: &str| match &hit["fields"][field][0] {
        Value::String(s) => s.parse::<f64>().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    let mut events: Vec<Event> = response["hits"]["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            Some(Event {
                at: ms(hit, &fields.time)?,
                end: fields.end.as_deref().and_then(|f| ms(hit, f)),
                text: fields
                    .text
                    .as_deref()
                    .and_then(|f| match &hit["fields"][f][0] {
                        Value::String(s) => Some(s.clone()),
                        Value::Null => None,
                        other => Some(other.to_string()),
                    }),
            })
        })
        .collect();
    events.reverse();
    events
}

/// Plans the searches for a chart.
pub fn plan(chart: &Chart, ctx: &Context) -> Plan {
    let mut warnings = chart.warnings.clone();
    let layers = chart
        .layers
        .iter()
        .map(|layer| LayerPlan {
            layer: layer.clone(),
            built: build_layer(chart, layer, ctx, &mut warnings),
        })
        .collect();
    // Only time axes can place events.
    let annotations = if chart.has_time_axis() {
        chart
            .annotations
            .iter()
            .chain(ctx.annotations)
            .map(|a| plan_annotation(a, ctx))
            .collect()
    } else {
        Vec::new()
    };
    Plan {
        layers,
        warnings,
        annotations,
    }
}

/// An annotation's search (or its fixed events, already resolved).
fn plan_annotation(annotation: &Annotation, ctx: &Context) -> AnnotationPlan {
    let mut data = AnnotationData {
        id: annotation.id.clone(),
        label: annotation
            .label
            .clone()
            .unwrap_or_else(|| annotation.id.clone()),
        color: annotation.color.clone(),
        style: annotation.style,
        events: Vec::new(),
        error: None,
    };
    let Some(view) = &annotation.data_view else {
        match fixed_events(annotation, ctx.range) {
            Ok(events) => data.events = events,
            Err(e) => data.error = Some(e),
        }
        return AnnotationPlan { data, search: None };
    };
    match annotation_search(annotation, view, ctx) {
        Ok(search) => AnnotationPlan {
            data,
            search: Some(search),
        },
        Err(e) => {
            data.error = Some(e);
            AnnotationPlan { data, search: None }
        }
    }
}

/// Fixed events that overlap the range, oldest first.
fn fixed_events(annotation: &Annotation, range: Range) -> Result<Vec<Event>, String> {
    let ms = |s: &str| {
        s.parse::<jiff::Timestamp>()
            .map(|t| t.as_millisecond() as f64)
            .map_err(|e| format!("annotation {}: time {s:?}: {e}", annotation.id))
    };
    let mut events = Vec::new();
    for e in &annotation.events {
        let at = ms(&e.at)?;
        let end = e.to.as_deref().map(ms).transpose()?;
        let last = end.unwrap_or(at);
        if last >= range.from_ms as f64 && at <= range.to_ms as f64 {
            events.push(Event {
                at,
                end,
                text: e.text.clone(),
            });
        }
    }
    events.sort_by(|a, b| a.at.total_cmp(&b.at));
    Ok(events)
}

/// The most recent documents an annotation marks within the range.
fn annotation_search(
    annotation: &Annotation,
    view: &DataView,
    ctx: &Context,
) -> Result<(Search, AnnotationFields), String> {
    let time = annotation
        .time
        .clone()
        .or_else(|| view.time_field.clone())
        .ok_or_else(|| {
            format!(
                "annotation {}: its data view has no time field; set `time`",
                annotation.id
            )
        })?;
    let fields = fields(ctx, view);
    let options = kql::Options {
        time_zone: Some(ctx.time_zone.to_string()),
    };
    let mut filter = vec![json!({ "range": { &time: {
        "gte": ctx.range.from_ms, "lte": ctx.range.to_ms, "format": "epoch_millis"
    } } })];
    let mut must_not = Vec::new();
    if let Some(dsl) = query_dsl(&annotation.query, &fields, &options)? {
        filter.push(dsl);
    }
    if !annotation.ignore_global_filters {
        for q in ctx.queries {
            if let Some(dsl) = query_dsl(q, &fields, &options)? {
                filter.push(dsl);
            }
        }
        for f in ctx.filters.iter().filter(|f| !f.disabled) {
            if f.negate {
                must_not.push(f.query.clone())
            } else {
                filter.push(f.query.clone())
            }
        }
        adhoc(ctx, &fields, &mut filter, &mut must_not);
        // A control on a field these documents don't have would hide them all.
        for (field, values) in ctx.selections.iter().filter(|(_, v)| !v.is_empty()) {
            if fields.is_empty() || fields.iter().any(|f| &f.name == field) {
                filter.push(json!({ "terms": { field: values } }));
            }
        }
    }
    let wanted = AnnotationFields {
        time: time.clone(),
        end: annotation.end.clone(),
        text: annotation.text.clone(),
    };
    let mut requested = vec![json!({ "field": time, "format": "epoch_millis" })];
    if let Some(end) = &wanted.end {
        requested.push(json!({ "field": end, "format": "epoch_millis" }));
    }
    if let Some(text) = &wanted.text {
        requested.push(json!(text));
    }
    let body = json!({
        "size": annotation.limit,
        "track_total_hits": false,
        "query": { "bool": { "filter": filter, "must_not": must_not } },
        "sort": [{ &time: { "order": "desc" } }],
        "_source": false,
        "fields": requested,
    });
    Ok((Search::dsl(view.index.clone(), body), wanted))
}

fn build_layer(
    chart: &Chart,
    layer: &Layer,
    ctx: &Context,
    warnings: &mut Vec<String>,
) -> Result<Built, String> {
    let fields = fields(ctx, &layer.data_view);
    let kql_options = kql::Options {
        time_zone: Some(ctx.time_zone.to_string()),
    };
    if let Some(docs) = &layer.documents {
        return build_documents(chart, layer, docs, ctx, &fields);
    }
    if let Some(esql) = &layer.esql {
        return build_esql(chart, layer, esql, ctx);
    }
    if let Some(series) = &layer.insight {
        return build_insight(layer, series, ctx);
    }

    // Bucket aggregations, outermost first. A date histogram's interval
    // depends on the range, which is the same for every shift group.
    let mut buckets = Vec::new();
    let mut bucket_aggs = Vec::new();
    for column in layer.columns.iter().filter(|c| c.op.is_bucket()) {
        let (kind, agg) = bucket_agg(column, layer, ctx, &fields, &kql_options, warnings)?;
        buckets.push(BucketSpec {
            column: column.id.clone(),
            kind,
        });
        bucket_aggs.push(agg);
    }

    // Leaf metrics, grouped by time shift. Computed columns need no agg.
    let mut by_shift: Vec<(i64, Vec<&Column>)> = Vec::new();
    for column in layer
        .columns
        .iter()
        .filter(|c| !c.op.is_bucket() && c.op.inputs().is_empty())
    {
        let shift = shift_ms(column.time_shift.as_deref(), ctx.range)?;
        match by_shift.iter_mut().find(|(s, _)| *s == shift) {
            Some((_, cols)) => cols.push(column),
            None => by_shift.push((shift, vec![column])),
        }
    }
    if by_shift.is_empty() {
        by_shift.push((0, Vec::new()));
    }
    // The unshifted group comes first: its rows define the table.
    by_shift.sort_by_key(|(s, _)| (*s != 0, *s));

    let mut groups = Vec::new();
    for (shift, columns) in by_shift {
        let range = ctx.range.shifted(shift);
        let mut metric_aggs = Map::new();
        let mut metrics = Vec::new();
        for (n, column) in columns.iter().enumerate() {
            let name = format!("m{n}");
            let (read, agg) = metric_agg(column, range, &fields, &kql_options)?;
            if let Some(agg) = agg {
                metric_aggs.insert(name.clone(), agg);
            }
            metrics.push(MetricSpec {
                column: column.id.clone(),
                agg: name,
                read,
            });
        }

        // Terms ordered by a metric need that metric at their own level.
        let mut tree = Value::Object(metric_aggs);
        for (i, (spec, agg)) in buckets.iter().zip(&bucket_aggs).enumerate().rev() {
            let mut agg = agg.clone();
            if let BucketKind::Date(_) = spec.kind {
                agg["date_histogram"]["extended_bounds"] =
                    json!({ "min": range.from_ms, "max": range.to_ms });
            }
            if let Some(order) = terms_order(layer, &spec.column, &fields, &kql_options, range)? {
                let (key, order_agg) = order;
                if let Some(order_agg) = order_agg {
                    tree["o"] = order_agg;
                }
                agg["terms"]["order"] = key;
            }
            if !tree.as_object().is_some_and(Map::is_empty) {
                agg["aggs"] = tree;
            }
            tree = json!({ format!("b{i}"): agg });
        }

        let constant = buckets.is_empty()
            && !metrics.is_empty()
            && metrics.iter().all(|m| matches!(m.read, Read::Constant(_)));
        let search = if constant {
            None
        } else {
            let query = base_query(chart, layer, ctx, range)?;
            let body = json!({
                "size": 0,
                "track_total_hits": false,
                "query": query,
                "aggs": { "root": { "filter": { "match_all": {} }, "aggs": tree } }
            });
            Some(Search::dsl(layer.data_view.index.clone(), body))
        };
        groups.push(ShiftGroup {
            shift_ms: shift,
            metrics,
            search,
        });
    }
    Ok(Built {
        buckets,
        groups,
        documents: None,
        esql: None,
        range: ctx.range,
        time_zone: time_zone(ctx),
    })
}

/// A layer of individual documents: their field values, newest first up to
/// the limit (they are put back in ascending order when read).
fn build_documents(
    chart: &Chart,
    layer: &Layer,
    docs: &crate::model::Documents,
    ctx: &Context,
    fields: &[Field],
) -> Result<Built, String> {
    let is_date = |name: &str| {
        fields
            .iter()
            .any(|f| f.name == name && f.es_types.iter().all(|t| t == "date" || t == "date_nanos"))
    };
    let mut doc_fields = Vec::new();
    for column in &layer.columns {
        match &column.op {
            Operation::Value { field } => doc_fields.push(DocField {
                column: column.id.clone(),
                field: field.clone(),
                date: is_date(field),
            }),
            op if op.inputs().is_empty() => {
                return Err(format!(
                    "column {} aggregates, so it cannot be in a document layer",
                    column.id
                ));
            }
            _ => {}
        }
    }
    let requested: Vec<Value> = doc_fields
        .iter()
        .map(|d| {
            if d.date {
                json!({ "field": d.field, "format": "epoch_millis" })
            } else {
                json!(d.field)
            }
        })
        .collect();
    let mut query = base_query(chart, layer, ctx, ctx.range)?;
    if let Some(filter) = query["bool"]["filter"].as_array_mut() {
        for d in &doc_fields {
            filter.push(json!({ "exists": { "field": d.field } }));
        }
    }
    let body = json!({
        "size": docs.limit,
        "track_total_hits": false,
        "query": query,
        "sort": [{ docs.sort.clone(): { "order": "desc" } }],
        "_source": false,
        "fields": requested,
    });
    Ok(Built {
        buckets: Vec::new(),
        groups: vec![ShiftGroup {
            shift_ms: 0,
            metrics: Vec::new(),
            search: Some(Search::dsl(layer.data_view.index.clone(), body)),
        }],
        documents: Some(doc_fields),
        esql: None,
        range: ctx.range,
        time_zone: time_zone(ctx),
    })
}

/// An insight layer: one `POST /api/series`. The record type is the data
/// view's index and the time range the dashboard's. The response is
/// reshaped like an ES|QL one (`es::series_as_esql`), so it is evaluated
/// the same way. The dashboard's query, filter pills and controls don't
/// apply: they are Elasticsearch queries.
fn build_insight(
    layer: &Layer,
    series: &crate::model::InsightSeries,
    ctx: &Context,
) -> Result<Built, String> {
    let time_field = layer
        .data_view
        .time_field
        .as_deref()
        .ok_or("an insight layer needs data_view.time_field")?;
    let mut wanted = Vec::new();
    for column in &layer.columns {
        match &column.op {
            Operation::Value { field } => wanted.push(EsqlField {
                column: column.id.clone(),
                name: field.clone(),
            }),
            op if op.inputs().is_empty() => {
                return Err(format!(
                    "column {} aggregates; in an insight layer, aggregate in `insight.columns` and name the result column with `op: value`",
                    column.id
                ));
            }
            _ => {}
        }
    }
    let iso = |ms: i64| {
        jiff::Timestamp::from_millisecond(ms)
            .map(|t| t.to_string())
            .unwrap_or_default()
    };
    let mut time = json!({
        "field": time_field,
        "from": iso(ctx.range.from_ms),
        "to": iso(ctx.range.to_ms),
    });
    if let Some(interval) = &series.interval {
        time["interval"] = json!(interval);
    }
    let mut body = json!({
        "record_type": layer.data_view.index,
        "time": time,
        "columns": series.columns,
    });
    if !series.filters.is_empty() {
        body["filters"] = json!(series.filters);
    }
    if !series.group_by.is_empty() {
        body["group_by"] = json!(series.group_by);
    }
    if let Some(key) = &series.latest_by {
        body["latest_by"] = json!(key);
    }
    Ok(Built {
        buckets: Vec::new(),
        groups: vec![ShiftGroup {
            shift_ms: 0,
            metrics: Vec::new(),
            search: Some(Search {
                index: layer.data_view.index.clone(),
                body,
                endpoint: crate::es::Endpoint::Insight,
            }),
        }],
        documents: None,
        esql: Some(wanted),
        range: ctx.range,
        time_zone: time_zone(ctx),
    })
}

/// An ES|QL layer: one `_query`. Like Kibana's `esql` expression, the
/// dashboard's query, filter pills, control selections and the time range
/// (on the data view's time field) go in as a DSL `filter`, and the range's
/// ends are offered as `?_tstart` and `?_tend`.
fn build_esql(
    chart: &Chart,
    layer: &Layer,
    esql: &crate::model::Esql,
    ctx: &Context,
) -> Result<Built, String> {
    let mut wanted = Vec::new();
    for column in &layer.columns {
        match &column.op {
            Operation::Value { field } => wanted.push(EsqlField {
                column: column.id.clone(),
                name: field.clone(),
            }),
            op if op.inputs().is_empty() => {
                return Err(format!(
                    "column {} aggregates; in an ES|QL layer, aggregate in the query and name the result column with `op: value`",
                    column.id
                ));
            }
            _ => {}
        }
    }
    let filter = base_query(chart, layer, ctx, ctx.range)?;
    let mut body = json!({ "query": esql.query, "filter": filter });
    let iso = |ms: i64| {
        jiff::Timestamp::from_millisecond(ms)
            .map(|t| t.to_string())
            .unwrap_or_default()
    };
    let mut params = Vec::new();
    for (name, ms) in [("_tstart", ctx.range.from_ms), ("_tend", ctx.range.to_ms)] {
        if esql.query.contains(&format!("?{name}")) {
            params.push(json!({ name: iso(ms) }));
        }
    }
    if !params.is_empty() {
        body["params"] = json!(params);
    }
    Ok(Built {
        buckets: Vec::new(),
        groups: vec![ShiftGroup {
            shift_ms: 0,
            metrics: Vec::new(),
            search: Some(Search {
                index: layer.data_view.index.clone(),
                body,
                endpoint: crate::es::Endpoint::Esql,
            }),
        }],
        documents: None,
        esql: Some(wanted),
        range: ctx.range,
        time_zone: time_zone(ctx),
    })
}

/// The ES|QL control variables a query uses (`?team`), other than the time
/// range's `?_tstart` and `?_tend`, which Teleplot fills in.
pub fn esql_variables(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut quoted = false;
    let chars: Vec<char> = query.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        match c {
            '"' => quoted = !quoted,
            '?' if !quoted => {
                let name: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '_')
                    .collect();
                // `??name` is a field or function variable; report it bare.
                let name = name.trim_start_matches('?').to_string();
                if !name.is_empty()
                    && !["_tstart", "_tend"].contains(&name.as_str())
                    && !out.contains(&name)
                {
                    out.push(name);
                }
            }
            _ => {}
        }
    }
    out
}

/// The context's zone; UTC if it is not a known IANA name.
fn time_zone(ctx: &Context) -> jiff::tz::TimeZone {
    jiff::tz::TimeZone::get(ctx.time_zone).unwrap_or(jiff::tz::TimeZone::UTC)
}

/// [`bucket_lengths`], when some column is normalized by unit.
fn per_lengths(
    plan: &LayerPlan,
    built: &Built,
    columns: &HashMap<String, Vec<Cell>>,
    rows: usize,
) -> Vec<f64> {
    if plan.layer.columns.iter().any(|c| c.per.is_some()) {
        bucket_lengths(built, columns, rows)
    } else {
        Vec::new()
    }
}

/// The length in milliseconds of each row's time bucket, for [`Column::per`]:
/// the date histogram bucket clipped to the time range, or the whole range
/// when there is no date histogram (Kibana's `lens_time_scale`).
fn bucket_lengths(built: &Built, columns: &HashMap<String, Vec<Cell>>, rows: usize) -> Vec<f64> {
    let (from, to) = (built.range.from_ms, built.range.to_ms);
    let date = built.buckets.iter().find_map(|b| match b.kind {
        BucketKind::Date(span) => Some((&columns[&b.column], span)),
        _ => None,
    });
    let Some((starts, span)) = date else {
        return vec![(to - from) as f64; rows];
    };
    starts
        .iter()
        .map(|start| {
            let Some(start) = start.number().map(|ms| ms as i64) else {
                return f64::NAN;
            };
            let end = jiff::Timestamp::from_millisecond(start)
                .ok()
                .and_then(|t| t.to_zoned(built.time_zone.clone()).checked_add(span).ok())
                .map_or(start, |z| z.timestamp().as_millisecond());
            (end.min(to) - start.max(from)) as f64
        })
        .collect()
}

fn fields(ctx: &Context, view: &DataView) -> Vec<Field> {
    ctx.fields.get(&view.index).cloned().unwrap_or_default()
}

/// Kibana time shifts: `previous` (the range before this one) or a duration.
fn shift_ms(shift: Option<&str>, range: Range) -> Result<i64, String> {
    match shift {
        None => Ok(0),
        Some("previous") => Ok(range.duration_ms()),
        Some(s) => match s.parse::<crate::model::Interval>() {
            Ok(crate::model::Interval::Every { count, unit }) => {
                Ok(count as i64 * interval::unit_ms(unit))
            }
            _ => Err(format!("unsupported time shift {s:?}")),
        },
    }
}

/// The query for a layer's documents over the context's range: everything
/// that narrows them (time, queries, filters, controls).
pub fn layer_query(chart: &Chart, layer: &Layer, ctx: &Context) -> Result<Value, String> {
    base_query(chart, layer, ctx, ctx.range)
}

/// Everything that restricts the documents a layer sees.
fn base_query(chart: &Chart, layer: &Layer, ctx: &Context, range: Range) -> Result<Value, String> {
    let fields = fields(ctx, &layer.data_view);
    let options = kql::Options {
        time_zone: Some(ctx.time_zone.to_string()),
    };
    let mut filter = Vec::new();
    let mut must_not = Vec::new();

    if let Some(time_field) = &layer.data_view.time_field {
        filter.push(json!({ "range": { time_field: {
            "gte": range.from_ms, "lte": range.to_ms, "format": "epoch_millis"
        } } }));
    }
    for q in ctx.queries.iter().chain([&chart.query]) {
        if let Some(dsl) = query_dsl(q, &fields, &options)? {
            filter.push(dsl);
        }
    }
    for f in ctx
        .filters
        .iter()
        .chain(&chart.filters)
        .filter(|f| !f.disabled)
    {
        if f.negate {
            must_not.push(f.query.clone())
        } else {
            filter.push(f.query.clone())
        }
    }
    adhoc(ctx, &fields, &mut filter, &mut must_not);
    for (field, values) in ctx.selections.iter().filter(|(_, v)| !v.is_empty()) {
        filter.push(json!({ "terms": { field: values } }));
    }
    Ok(json!({ "bool": { "filter": filter, "must_not": must_not } }))
}

/// The viewer's ad-hoc filter pills, for an index with these fields. A pill
/// on a field the index doesn't have is skipped, so it narrows the panels it
/// is about and leaves the others alone.
fn adhoc(ctx: &Context, fields: &[Field], filter: &mut Vec<Value>, must_not: &mut Vec<Value>) {
    for f in ctx.adhoc.iter().filter(|f| !f.disabled) {
        let applies = match &f.field {
            Some(name) => fields.is_empty() || fields.iter().any(|field| &field.name == name),
            None => true,
        };
        if applies {
            if f.negate {
                must_not.push(f.query.clone());
            } else {
                filter.push(f.query.clone());
            }
        }
    }
}

/// A query bar's DSL, or `None` when empty.
fn query_dsl(
    query: &Query,
    fields: &[Field],
    options: &kql::Options,
) -> Result<Option<Value>, String> {
    if query.is_empty() {
        return Ok(None);
    }
    Ok(Some(match query.language {
        QueryLanguage::Kql => kql::translate(&query.text, fields, options)
            .map_err(|e| format!("KQL {:?}: {e}", query.text))?,
        QueryLanguage::Lucene => {
            json!({ "query_string": { "query": query.text, "analyze_wildcard": true } })
        }
    }))
}

fn bucket_agg(
    column: &Column,
    layer: &Layer,
    ctx: &Context,
    fields: &[Field],
    options: &kql::Options,
    warnings: &mut Vec<String>,
) -> Result<(BucketKind, Value), String> {
    Ok(match &column.op {
        Operation::DateHistogram {
            field,
            interval,
            include_empty,
        } => {
            let resolved = interval::resolve(
                *interval,
                ctx.range.duration_ms(),
                ctx.settings.bar_target,
                ctx.settings.max_bars,
            );
            if let Some(from) = resolved.scaled_from {
                warnings.push(format!(
                    "{from} buckets would be too many for this range; using a larger interval"
                ));
            }
            let mut agg = json!({ "field": field, "time_zone": ctx.time_zone, "min_doc_count": if *include_empty { 0 } else { 1 } });
            for (k, v) in resolved.es.as_object().unwrap() {
                agg[k] = v.clone();
            }
            (
                BucketKind::Date(interval::span(&resolved.es)),
                json!({ "date_histogram": agg }),
            )
        }
        Operation::Terms {
            fields: terms_fields,
            size,
            missing_bucket,
            ..
        } => {
            let agg = if terms_fields.len() == 1 {
                let mut t = json!({ "field": terms_fields[0], "size": size });
                if *missing_bucket {
                    t["missing"] = json!("(missing value)");
                }
                json!({ "terms": t })
            } else {
                let terms: Vec<Value> =
                    terms_fields.iter().map(|f| json!({ "field": f })).collect();
                json!({ "multi_terms": { "terms": terms, "size": size } })
            };
            (BucketKind::Terms, agg)
        }
        Operation::Filters { filters } => {
            let dsl: Vec<Value> = filters
                .iter()
                .map(|f| {
                    query_dsl(&f.query, fields, options)
                        .map(|q| q.unwrap_or(json!({ "match_all": {} })))
                })
                .collect::<Result<_, _>>()?;
            let labels = filters.iter().map(|f| f.label.clone()).collect();
            (
                BucketKind::Filters(labels),
                json!({ "filters": { "filters": dsl } }),
            )
        }
        Operation::Histogram { field, interval } => {
            let interval = match interval {
                Some(i) => *i,
                None => {
                    let (min, max) = ctx
                        .stats
                        .get(&(layer.id.clone(), column.id.clone()))
                        .copied()
                        .unwrap_or((0.0, 0.0));
                    nice_interval((max - min) / ctx.settings.max_bars as f64)
                }
            };
            (
                BucketKind::Numeric,
                json!({ "histogram": { "field": field, "interval": interval, "min_doc_count": 0 } }),
            )
        }
        Operation::Ranges { field, ranges } => {
            let ranges: Vec<Value> = ranges
                .iter()
                .map(|r| {
                    let mut v = json!({});
                    if let Some(from) = r.from {
                        v["from"] = json!(from);
                    }
                    if let Some(to) = r.to {
                        v["to"] = json!(to);
                    }
                    if let Some(label) = &r.label {
                        v["key"] = json!(label);
                    }
                    v
                })
                .collect();
            (
                BucketKind::Ranges,
                json!({ "range": { "field": field, "ranges": ranges } }),
            )
        }
        Operation::LogHistogram {
            field,
            min,
            max,
            per_decade,
        } => {
            let bounds = crate::model::log_bounds(*min, *max, *per_decade);
            if bounds.len() < 2 {
                return Err(format!(
                    "log histogram {}: needs 0 < min < max (got {min} and {max})",
                    column.id
                ));
            }
            // The first and last buckets are open, so no document is left out.
            let last = bounds.len() - 2;
            let ranges: Vec<Value> = (0..=last)
                .map(|i| {
                    let mut r = json!({});
                    if i > 0 {
                        r["from"] = json!(bounds[i]);
                    }
                    if i < last {
                        r["to"] = json!(bounds[i + 1]);
                    }
                    r
                })
                .collect();
            (
                BucketKind::LogRanges(bounds[..=last].to_vec()),
                json!({ "range": { "field": field, "ranges": ranges } }),
            )
        }
        other => return Err(format!("{other:?} is not a bucket")),
    })
}

/// The smallest of 1, 2 or 5 × 10ⁿ at least `raw` (Kibana's histogram auto interval).
fn nice_interval(raw: f64) -> f64 {
    if raw <= 0.0 || !raw.is_finite() {
        return 1.0;
    }
    let magnitude = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * magnitude)
        .find(|v| *v >= raw)
        .unwrap_or(10.0 * magnitude)
}

/// How a terms column orders its buckets: `(order, extra agg)`.
fn terms_order(
    layer: &Layer,
    column: &str,
    fields: &[Field],
    options: &kql::Options,
    range: Range,
) -> Result<Option<(Value, Option<Value>)>, String> {
    let Some(Column {
        op: Operation::Terms { order, .. },
        ..
    }) = layer.column(column)
    else {
        return Ok(None);
    };
    let dir = |asc: bool| if asc { "asc" } else { "desc" };
    Ok(Some(match order {
        TermsOrder::Count => (json!({ "_count": "desc" }), None),
        TermsOrder::Alphabetical { ascending } => (json!({ "_key": dir(*ascending) }), None),
        TermsOrder::Column {
            column: by,
            ascending,
        } => {
            let Some(target) = layer.column(by) else {
                return Ok(Some((json!({ "_count": "desc" }), None)));
            };
            // Elasticsearch can only order buckets by an aggregation. Kibana
            // orders by formulas and pipeline columns in the browser; here
            // they fall back to document count (lint warns about it).
            if !target.op.inputs().is_empty() {
                return Ok(Some((json!({ "_count": dir(*ascending) }), None)));
            }
            match (&target.op, &target.filter) {
                (Operation::Count { field: None }, None) => {
                    (json!({ "_count": dir(*ascending) }), None)
                }
                _ => match metric_agg(target, range, fields, options)? {
                    (Read::Value, Some(agg)) => (json!({ "o": dir(*ascending) }), Some(agg)),
                    (Read::FirstOfValues, Some(agg)) => {
                        let key = percentile_key(&agg);
                        (json!({ format!("o[{key}]"): dir(*ascending) }), Some(agg))
                    }
                    // A column with its own filter is a `filter` agg around the
                    // metric; Elasticsearch orders through it by path.
                    (Read::Filtered(inner), Some(agg)) => {
                        let path = match *inner {
                            Read::DocCount => Some("o>_count".to_string()),
                            Read::Value => Some("o>v".to_string()),
                            Read::FirstOfValues => {
                                Some(format!("o>v[{}]", percentile_key(&agg["aggs"]["v"])))
                            }
                            _ => None,
                        };
                        match path {
                            Some(path) => (json!({ path: dir(*ascending) }), Some(agg)),
                            None => (json!({ "_count": dir(*ascending) }), None),
                        }
                    }
                    // Kibana orders by formulas and pipeline columns client-side;
                    // falling back to document count keeps the top-N stable.
                    _ => (json!({ "_count": dir(*ascending) }), None),
                },
            }
        }
    }))
}

fn percentile_key(agg: &Value) -> String {
    let p = agg["percentiles"]["percents"][0]
        .as_f64()
        .or_else(|| agg["percentile_ranks"]["values"][0].as_f64())
        .unwrap_or(50.0);
    format!("{p:?}")
}

/// A leaf metric's aggregation (`None` when no agg is needed) and how to read it.
fn metric_agg(
    column: &Column,
    range: Range,
    fields: &[Field],
    options: &kql::Options,
) -> Result<(Read, Option<Value>), String> {
    let (read, agg) = match &column.op {
        Operation::Count { field: None } => (Read::DocCount, None),
        Operation::Count { field: Some(f) } => {
            (Read::Value, Some(json!({ "value_count": { "field": f } })))
        }
        Operation::Sum { field } => (Read::Value, Some(json!({ "sum": { "field": field } }))),
        Operation::Average { field } => (Read::Value, Some(json!({ "avg": { "field": field } }))),
        Operation::Min { field } => (Read::Value, Some(json!({ "min": { "field": field } }))),
        Operation::Max { field } => (Read::Value, Some(json!({ "max": { "field": field } }))),
        Operation::UniqueCount { field } => (
            Read::Value,
            Some(json!({ "cardinality": { "field": field } })),
        ),
        Operation::Median { field } => (
            Read::FirstOfValues,
            Some(json!({ "percentiles": { "field": field, "percents": [50.0] } })),
        ),
        Operation::Percentile { field, percent } => (
            Read::FirstOfValues,
            Some(json!({ "percentiles": { "field": field, "percents": [percent] } })),
        ),
        Operation::PercentileRank { field, value } => (
            Read::FirstOfValues,
            Some(json!({ "percentile_ranks": { "field": field, "values": [value] } })),
        ),
        Operation::StandardDeviation { field } => (
            Read::StdDeviation,
            Some(json!({ "extended_stats": { "field": field } })),
        ),
        Operation::LastValue { field, sort_field } => (
            Read::Filtered(Box::new(Read::TopHit(field.clone()))),
            Some(
                json!({ "filter": { "exists": { "field": field } }, "aggs": { "v": { "top_hits": {
                "size": 1, "sort": [{ sort_field: { "order": "desc" } }], "_source": false, "fields": [field]
            } } } }),
            ),
        ),
        Operation::StaticValue { value } => (Read::Constant(*value), None),
        Operation::TimeRange => (Read::Constant(range.duration_ms() as f64), None),
        other => return Err(format!("{other:?} is not a leaf metric")),
    };

    // Per-column filters wrap the metric in a `filter` agg.
    let Some(filter) = &column.filter else {
        return Ok((read, agg));
    };
    let dsl = query_dsl(filter, fields, options)?.unwrap_or(json!({ "match_all": {} }));
    Ok(match (read, agg) {
        (read @ Read::Constant(_), _) => (read, None),
        (Read::DocCount, None) => (
            Read::Filtered(Box::new(Read::DocCount)),
            Some(json!({ "filter": dsl })),
        ),
        (Read::Filtered(inner), Some(mut agg)) => {
            // Already a filter agg (last value): combine both conditions.
            let existing = agg["filter"].take();
            agg["filter"] = json!({ "bool": { "filter": [existing, dsl] } });
            (Read::Filtered(inner), Some(agg))
        }
        (read, Some(agg)) => (
            Read::Filtered(Box::new(read)),
            Some(json!({ "filter": dsl, "aggs": { "v": agg } })),
        ),
        (read, None) => (read, None),
    })
}

/// Flattens the responses into a table and evaluates computed columns.
fn evaluate_layer(
    plan: &LayerPlan,
    built: &Built,
    responses: &[Option<&Result<Value, String>>],
) -> Result<LayerData, String> {
    if let Some(fields) = &built.esql {
        return evaluate_esql(plan, built, fields, responses.first().copied().flatten());
    }
    if let Some(doc_fields) = &built.documents {
        return evaluate_documents(
            plan,
            built,
            doc_fields,
            responses.first().copied().flatten(),
        );
    }
    let mut tables = Vec::new();
    for (group, response) in built.groups.iter().zip(responses) {
        let response = response
            .ok_or("missing response")?
            .as_ref()
            .map_err(Clone::clone)?;
        let root = &response["aggregations"]["root"];
        let mut rows = Vec::new();
        flatten(
            root,
            &built.buckets,
            &group.metrics,
            0,
            &mut Vec::new(),
            &mut rows,
        );
        tables.push((group, rows));
    }

    // Join shifted groups onto the main table by bucket keys; shifted date
    // keys are moved forward by the shift to line up.
    let (main_group, main_rows) = tables.remove(0);
    let mut columns: HashMap<String, Vec<Cell>> = HashMap::new();
    let n = main_rows.len();
    for (i, spec) in built.buckets.iter().enumerate() {
        columns.insert(
            spec.column.clone(),
            main_rows.iter().map(|r| r.keys[i].clone()).collect(),
        );
    }
    for (j, m) in main_group.metrics.iter().enumerate() {
        columns.insert(
            m.column.clone(),
            main_rows.iter().map(|r| r.values[j].clone()).collect(),
        );
    }
    for (group, rows) in &tables {
        let index: HashMap<Vec<String>, &Row> = rows
            .iter()
            .map(|r| (key_of(&r.keys, &built.buckets, group.shift_ms), r))
            .collect();
        for (j, m) in group.metrics.iter().enumerate() {
            let values = main_rows
                .iter()
                .map(|r| {
                    index
                        .get(&key_of(&r.keys, &built.buckets, 0))
                        .map_or(Cell::Null, |row| row.values[j].clone())
                })
                .collect();
            columns.insert(m.column.clone(), values);
        }
    }

    let lengths = per_lengths(plan, built, &columns, n);
    compute::computed_columns(
        &plan.layer,
        &built_bucket_kinds(built),
        n,
        &lengths,
        &mut columns,
    )?;

    let series = plan
        .layer
        .columns
        .iter()
        .filter(|c| !c.helper)
        .map(|c| Series {
            id: c.id.clone(),
            values: columns.remove(&c.id).unwrap_or_else(|| vec![Cell::Null; n]),
        })
        .collect();
    Ok(LayerData {
        layer: plan.layer.id.clone(),
        rows: n,
        columns: series,
        counts: main_rows.iter().map(|r| r.count).collect(),
        error: None,
        dates: Vec::new(),
        buckets: Vec::new(),
    })
}

/// Columns marked `shorten` drop an `owner/` prefix that every text value
/// shares.
fn shorten_columns(layer: &Layer, data: &mut LayerData) {
    for column in layer.columns.iter().filter(|c| c.shorten) {
        if let Some(series) = data.columns.iter_mut().find(|s| s.id == column.id) {
            shorten_values(&mut series.values);
        }
    }
}

fn shorten_values(values: &mut [Cell]) {
    let owner = |c: &Cell| match c {
        Cell::Text(t) => t.split_once('/').map(|(o, _)| o.to_string()),
        _ => None,
    };
    let mut owners = values
        .iter()
        .filter(|c| !matches!(c, Cell::Null))
        .map(owner);
    let Some(Some(first)) = owners.next() else {
        return;
    };
    if !owners.all(|o| o.as_deref() == Some(first.as_str())) {
        return;
    }
    let prefix = format!("{first}/");
    for cell in values.iter_mut() {
        if let Cell::Text(t) = cell
            && let Some(rest) = t.strip_prefix(&prefix)
        {
            *t = rest.to_string();
        }
    }
}

/// One cell of a document row, from the `fields` API's array of values. A
/// field with several values (a keyword array) shows them all, as Kibana's
/// Discover does; numbers and dates take the first.
fn document_cell(values: &Value, date: bool) -> Cell {
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    match &values[0] {
        Value::Number(v) => v.as_f64().map_or(Cell::Null, Cell::Number),
        Value::String(s) if date => s.parse::<f64>().map_or(Cell::Null, Cell::Number),
        Value::String(_) | Value::Bool(_) => {
            let all: Vec<String> = values
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(text)
                .collect();
            Cell::Text(all.join(", "))
        }
        _ => Cell::Null,
    }
}

fn evaluate_documents(
    plan: &LayerPlan,
    built: &Built,
    doc_fields: &[DocField],
    response: Option<&Result<Value, String>>,
) -> Result<LayerData, String> {
    let response = response
        .ok_or("missing response")?
        .as_ref()
        .map_err(Clone::clone)?;
    let mut hits = response["hits"]["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    hits.reverse(); // fetched newest first; shown oldest first
    let n = hits.len();
    let mut columns: HashMap<String, Vec<Cell>> = HashMap::new();
    for d in doc_fields {
        let values = hits
            .iter()
            .map(|hit| document_cell(&hit["fields"][&d.field], d.date))
            .collect();
        columns.insert(d.column.clone(), values);
    }
    let lengths = per_lengths(plan, built, &columns, n);
    compute::computed_columns(&plan.layer, &[], n, &lengths, &mut columns)?;
    let series = plan
        .layer
        .columns
        .iter()
        .filter(|c| !c.helper)
        .map(|c| Series {
            id: c.id.clone(),
            values: columns.remove(&c.id).unwrap_or_else(|| vec![Cell::Null; n]),
        })
        .collect();
    Ok(LayerData {
        layer: plan.layer.id.clone(),
        rows: n,
        columns: series,
        counts: Vec::new(),
        error: None,
        dates: Vec::new(),
        buckets: Vec::new(),
    })
}

/// An ES|QL result (`columns` and row `values`) as the layer's columns.
/// Dates become epoch milliseconds and are listed in [`LayerData::dates`].
fn evaluate_esql(
    plan: &LayerPlan,
    built: &Built,
    wanted: &[EsqlField],
    response: Option<&Result<Value, String>>,
) -> Result<LayerData, String> {
    let response = response
        .ok_or("missing response")?
        .as_ref()
        .map_err(Clone::clone)?;
    let names: Vec<&str> = response["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| c["name"].as_str().unwrap_or_default())
        .collect();
    let types: Vec<&str> = response["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| c["type"].as_str().unwrap_or_default())
        .collect();
    let rows = response["values"].as_array().cloned().unwrap_or_default();
    let n = rows.len();
    let mut columns: HashMap<String, Vec<Cell>> = HashMap::new();
    let mut dates = Vec::new();
    for w in wanted {
        let Some(i) = names.iter().position(|name| *name == w.name) else {
            return Err(format!(
                "the ES|QL result has no column {:?} (it has {})",
                w.name,
                names.join(", ")
            ));
        };
        let is_date = matches!(types[i], "date" | "date_nanos" | "datetime");
        if is_date {
            dates.push(w.column.clone());
        }
        let values = rows.iter().map(|row| esql_cell(&row[i], is_date)).collect();
        columns.insert(w.column.clone(), values);
    }
    let lengths = per_lengths(plan, built, &columns, n);
    compute::computed_columns(&plan.layer, &[], n, &lengths, &mut columns)?;
    let series = plan
        .layer
        .columns
        .iter()
        .filter(|c| !c.helper)
        .map(|c| Series {
            id: c.id.clone(),
            values: columns.remove(&c.id).unwrap_or_else(|| vec![Cell::Null; n]),
        })
        .collect();
    // insight's bucket starts (RFC 3339), for a time axis without holes.
    let buckets = response["buckets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b.as_str())
        .filter_map(|b| b.parse::<jiff::Timestamp>().ok())
        .map(|t| t.as_millisecond() as f64)
        .collect();
    Ok(LayerData {
        layer: plan.layer.id.clone(),
        rows: n,
        columns: series,
        buckets,
        counts: Vec::new(),
        error: None,
        dates,
    })
}

/// One ES|QL value: numbers stay numbers, dates become epoch milliseconds,
/// and multi-valued fields are joined.
fn esql_cell(value: &Value, is_date: bool) -> Cell {
    match value {
        Value::Null => Cell::Null,
        Value::Number(n) => n.as_f64().map_or(Cell::Null, Cell::Number),
        Value::String(s) if is_date => s
            .parse::<jiff::Timestamp>()
            .map(|t| Cell::Number(t.as_millisecond() as f64))
            .unwrap_or_else(|_| Cell::Text(s.clone())),
        Value::String(s) => Cell::Text(s.clone()),
        Value::Bool(b) => Cell::Text(b.to_string()),
        Value::Array(items) => Cell::Text(
            items
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        other => Cell::Text(other.to_string()),
    }
}

fn built_bucket_kinds(built: &Built) -> Vec<(String, bool)> {
    built
        .buckets
        .iter()
        .map(|b| (b.column.clone(), matches!(b.kind, BucketKind::Date(_))))
        .collect()
}

fn key_of(keys: &[Cell], specs: &[BucketSpec], shift_ms: i64) -> Vec<String> {
    keys.iter()
        .zip(specs)
        .map(|(k, spec)| match (k, &spec.kind) {
            (Cell::Number(ms), BucketKind::Date(_)) => format!("{}", *ms as i64 + shift_ms),
            (Cell::Number(n), _) => format!("{n}"),
            (Cell::Text(t), _) => t.clone(),
            (Cell::Null, _) => String::new(),
        })
        .collect()
}

#[derive(Debug)]
struct Row {
    keys: Vec<Cell>,
    values: Vec<Cell>,
    /// Documents in the innermost bucket.
    count: f64,
}

/// Walks nested buckets depth-first, emitting a row per innermost bucket.
fn flatten(
    agg: &Value,
    buckets: &[BucketSpec],
    metrics: &[MetricSpec],
    depth: usize,
    keys: &mut Vec<Cell>,
    rows: &mut Vec<Row>,
) {
    if depth == buckets.len() {
        let values = metrics.iter().map(|m| read_metric(agg, m)).collect();
        rows.push(Row {
            keys: keys.clone(),
            values,
            count: agg["doc_count"].as_f64().unwrap_or(0.0),
        });
        return;
    }
    let spec = &buckets[depth];
    let level = &agg[format!("b{depth}")];
    let Some(list) = level["buckets"].as_array() else {
        return;
    };
    for (i, bucket) in list.iter().enumerate() {
        let key = match &spec.kind {
            BucketKind::Date(_) | BucketKind::Numeric => {
                bucket["key"].as_f64().map_or(Cell::Null, Cell::Number)
            }
            BucketKind::Filters(labels) => Cell::Text(labels.get(i).cloned().unwrap_or_default()),
            // Elasticsearch returns ranges in ascending order, as requested.
            BucketKind::LogRanges(starts) => {
                starts.get(i).copied().map_or(Cell::Null, Cell::Number)
            }
            BucketKind::Terms | BucketKind::Ranges => {
                match (&bucket["key_as_string"], &bucket["key"]) {
                    (Value::String(s), _) => Cell::Text(s.clone()),
                    (_, Value::String(s)) => Cell::Text(s.clone()),
                    (_, Value::Number(n)) => Cell::Number(n.as_f64().unwrap_or_default()),
                    (_, Value::Array(parts)) => Cell::Text(
                        parts
                            .iter()
                            .map(|p| {
                                p.as_str()
                                    .map(String::from)
                                    .unwrap_or_else(|| p.to_string())
                            })
                            .collect::<Vec<_>>()
                            .join(" › "),
                    ),
                    _ => Cell::Null,
                }
            }
        };
        keys.push(key);
        flatten(bucket, buckets, metrics, depth + 1, keys, rows);
        keys.pop();
    }
}

fn read_metric(bucket: &Value, metric: &MetricSpec) -> Cell {
    read(bucket, &bucket[&metric.agg], &metric.read)
}

fn read(bucket: &Value, agg: &Value, how: &Read) -> Cell {
    match how {
        Read::DocCount => bucket["doc_count"].as_f64().into(),
        Read::Value => agg["value"].as_f64().into(),
        Read::FirstOfValues => agg["values"]
            .as_object()
            .and_then(|v| v.values().next())
            .and_then(Value::as_f64)
            .into(),
        Read::StdDeviation => agg["std_deviation"].as_f64().into(),
        Read::TopHit(field) => match &agg["hits"]["hits"][0]["fields"][field][0] {
            Value::Number(n) => Cell::Number(n.as_f64().unwrap_or_default()),
            Value::String(s) => Cell::Text(s.clone()),
            Value::Bool(b) => Cell::Text(b.to_string()),
            _ => Cell::Null,
        },
        Read::Constant(v) => Cell::Number(*v),
        Read::Filtered(inner) => match inner.as_ref() {
            Read::DocCount => agg["doc_count"].as_f64().into(),
            Read::TopHit(_) => read(agg, &agg["v"], inner),
            other => read(agg, &agg["v"], other),
        },
    }
}

/// Milliseconds as the largest whole unit, e.g. `30d` or `90m`.
fn duration(ms: i64) -> String {
    for (unit, size) in [
        ("d", 86_400_000),
        ("h", 3_600_000),
        ("m", 60_000),
        ("s", 1_000),
    ] {
        if ms % size == 0 {
            return format!("{}{unit}", ms / size);
        }
    }
    format!("{ms}ms")
}

#[cfg(test)]
mod tests {
    #[test]
    fn shortened_values_drop_a_shared_owner_only() {
        let text = |v: &[&str]| {
            v.iter()
                .map(|t| Cell::Text(t.to_string()))
                .collect::<Vec<_>>()
        };
        let mut one_owner = text(&["acme/payments", "acme/billing"]);
        one_owner.push(Cell::Null);
        super::shorten_values(&mut one_owner);
        assert_eq!(one_owner[..2], text(&["payments", "billing"]));
        let mut two_owners = text(&["acme/payments", "globex/billing"]);
        super::shorten_values(&mut two_owners);
        assert_eq!(two_owners, text(&["acme/payments", "globex/billing"]));
        let mut no_owner = text(&["payments", "acme/billing"]);
        super::shorten_values(&mut no_owner);
        assert_eq!(no_owner, text(&["payments", "acme/billing"]));
    }

    use super::*;
    use jiff::ToSpan;

    const DAY: f64 = 86_400_000.0;

    fn ms(s: &str) -> f64 {
        s.parse::<jiff::Timestamp>().unwrap().as_millisecond() as f64
    }

    fn built(span: Option<jiff::Span>, from: &str, to: &str, tz: &str) -> Built {
        Built {
            buckets: span
                .map(|span| BucketSpec {
                    column: "x".into(),
                    kind: BucketKind::Date(span),
                })
                .into_iter()
                .collect(),
            groups: Vec::new(),
            documents: None,
            esql: None,
            range: Range {
                from_ms: ms(from) as i64,
                to_ms: ms(to) as i64,
            },
            time_zone: jiff::tz::TimeZone::get(tz).unwrap(),
        }
    }

    #[test]
    fn bucket_lengths_are_calendar_months_clipped_to_the_range() {
        let b = built(
            Some(1.month()),
            "2026-04-01T00:00:00Z",
            "2026-10-02T00:00:00Z",
            "UTC",
        );
        let starts = [
            "2026-04-01T00:00:00Z",
            "2026-05-01T00:00:00Z",
            "2026-10-01T00:00:00Z",
        ];
        let columns = HashMap::from([(
            "x".to_string(),
            starts.iter().map(|s| Cell::Number(ms(s))).collect(),
        )]);
        assert_eq!(
            bucket_lengths(&b, &columns, 3),
            vec![30.0 * DAY, 31.0 * DAY, DAY]
        );
    }

    #[test]
    fn bucket_lengths_follow_daylight_saving_in_the_zone() {
        // Europe/London springs forward on 2026-03-29: that day is 23 hours.
        let b = built(
            Some(1.day()),
            "2026-03-01T00:00:00Z",
            "2026-04-01T00:00:00Z",
            "Europe/London",
        );
        let columns = HashMap::from([(
            "x".to_string(),
            vec![Cell::Number(ms("2026-03-29T00:00:00Z"))],
        )]);
        assert_eq!(bucket_lengths(&b, &columns, 1), vec![23.0 * 3_600_000.0]);
    }

    #[test]
    fn without_a_date_histogram_the_bucket_is_the_whole_range() {
        let b = built(None, "2026-04-01T00:00:00Z", "2026-04-08T00:00:00Z", "UTC");
        assert_eq!(bucket_lengths(&b, &HashMap::new(), 2), vec![7.0 * DAY; 2]);
    }

    fn annotation(view: Option<&str>) -> Annotation {
        Annotation {
            id: "a".into(),
            label: Some("Failed".into()),
            data_view: view.map(|index| DataView {
                index: index.into(),
                time_field: Some("deployed_at".into()),
            }),
            query: Query::kql("result : failure"),
            time: None,
            end: None,
            text: Some("team".into()),
            limit: 500,
            events: Vec::new(),
            ignore_global_filters: false,
            color: None,
            style: Default::default(),
        }
    }

    fn with_context<T>(
        selections: &[(String, Vec<String>)],
        annotations: &[Annotation],
        f: impl FnOnce(&Context) -> T,
    ) -> T {
        let fields = HashMap::from([(
            "deploys".to_string(),
            ["team", "result", "deployed_at"]
                .iter()
                .map(|n| Field {
                    name: n.to_string(),
                    es_types: vec![
                        if *n == "deployed_at" {
                            "date"
                        } else {
                            "keyword"
                        }
                        .into(),
                    ],
                })
                .collect(),
        )]);
        let stats = HashMap::new();
        let queries = [Query::kql("team : checkout")];
        let ctx = Context {
            range: Range {
                from_ms: ms("2026-07-01T00:00:00Z") as i64,
                to_ms: ms("2026-08-01T00:00:00Z") as i64,
            },
            time_zone: "UTC",
            queries: &queries,
            filters: &[],
            selections,
            adhoc: &[],
            fields: &fields,
            stats: &stats,
            settings: Settings::default(),
            annotations,
        };
        f(&ctx)
    }

    #[test]
    fn annotations_search_their_documents_newest_first() {
        let a = annotation(Some("deploys"));
        let selections = [
            ("team".to_string(), vec!["checkout".to_string()]),
            ("service".to_string(), vec!["cart".to_string()]),
        ];
        let (search, fields) = with_context(&selections, &[], |ctx| {
            annotation_search(&a, a.data_view.as_ref().unwrap(), ctx)
        })
        .unwrap();
        assert_eq!(search.index, "deploys");
        assert_eq!(
            search.body["sort"],
            json!([{ "deployed_at": { "order": "desc" } }])
        );
        assert_eq!(search.body["size"], json!(500));
        let filter = search.body["query"]["bool"]["filter"].as_array().unwrap();
        // Range, own query, the dashboard query, and the team selection. The
        // control on `service`, which deployments don't have, is left out.
        assert_eq!(filter.len(), 4, "{filter:#?}");
        assert!(filter[3]["terms"]["team"].is_array());
        assert_eq!(fields.time, "deployed_at");
        assert_eq!(fields.text.as_deref(), Some("team"));
    }

    #[test]
    fn annotations_can_ignore_the_dashboards_filters() {
        let mut a = annotation(Some("deploys"));
        a.ignore_global_filters = true;
        let selections = [("team".to_string(), vec!["checkout".to_string()])];
        let (search, _) = with_context(&selections, &[], |ctx| {
            annotation_search(&a, a.data_view.as_ref().unwrap(), ctx)
        })
        .unwrap();
        let filter = search.body["query"]["bool"]["filter"].as_array().unwrap();
        assert_eq!(filter.len(), 2, "range and own query only: {filter:#?}");
    }

    #[test]
    fn document_cells_show_every_value_of_a_keyword_array() {
        assert_eq!(
            document_cell(&json!(["claude-code", "github-copilot"]), false),
            Cell::Text("claude-code, github-copilot".into())
        );
        assert_eq!(
            document_cell(&json!([true]), false),
            Cell::Text("true".into())
        );
        assert_eq!(document_cell(&json!([3, 4]), false), Cell::Number(3.0));
        assert_eq!(document_cell(&json!(["1000"]), true), Cell::Number(1000.0));
        assert_eq!(document_cell(&Value::Null, false), Cell::Null);
    }

    #[test]
    fn annotation_hits_become_events_oldest_first() {
        let fields = AnnotationFields {
            time: "at".into(),
            end: Some("to".into()),
            text: Some("team".into()),
        };
        let response = json!({ "hits": { "hits": [
            { "fields": { "at": ["2000"], "to": ["3000"], "team": ["infra"] } },
            { "fields": { "at": ["1000"], "team": [7] } },
            { "fields": { "team": ["no time"] } }
        ] } });
        let events = read_events(&response, &fields);
        assert_eq!(
            events,
            [
                Event {
                    at: 1000.0,
                    end: None,
                    text: Some("7".into())
                },
                Event {
                    at: 2000.0,
                    end: Some(3000.0),
                    text: Some("infra".into())
                },
            ]
        );
    }

    #[test]
    fn fixed_events_outside_the_range_are_left_out() {
        let mut a = annotation(None);
        a.events = vec![
            crate::model::AnnotationEvent {
                at: "2026-07-10T00:00:00Z".into(),
                to: None,
                text: Some("freeze".into()),
            },
            crate::model::AnnotationEvent {
                at: "2026-06-20T00:00:00Z".into(),
                to: Some("2026-07-02T00:00:00Z".into()),
                text: None,
            },
            crate::model::AnnotationEvent {
                at: "2026-09-01T00:00:00Z".into(),
                to: None,
                text: None,
            },
        ];
        let range = Range {
            from_ms: ms("2026-07-01T00:00:00Z") as i64,
            to_ms: ms("2026-08-01T00:00:00Z") as i64,
        };
        let events = fixed_events(&a, range).unwrap();
        assert_eq!(
            events.len(),
            2,
            "September is outside; June's range reaches into July"
        );
        assert_eq!(events[0].end, Some(ms("2026-07-02T00:00:00Z")));
        a.events[0].at = "July".into();
        assert!(fixed_events(&a, range).is_err());
    }

    #[test]
    fn only_time_series_charts_get_annotations() {
        use crate::model::{ChartKind, Column, Encoding, Layer, Operation, SeriesType, TermsOrder};
        let column = |id: &str, op| Column {
            id: id.into(),
            label: None,
            op,
            filter: None,
            time_shift: None,
            per: None,
            format: None,
            helper: false,
            thresholds: Vec::new(),
            shorten: false,
        };
        let chart = |x: Operation| Chart {
            kind: ChartKind::Xy,
            query: Default::default(),
            filters: vec![],
            layers: vec![Layer {
                insight: None,
                id: "l".into(),
                data_view: DataView {
                    index: "deploys".into(),
                    time_field: Some("deployed_at".into()),
                },
                columns: vec![
                    column("x", x),
                    column("n", Operation::Count { field: None }),
                ],
                encoding: Encoding::Xy {
                    series: SeriesType::Bar,
                    x: Some("x".into()),
                    y: vec!["n".into()],
                    split: None,
                    options: Default::default(),
                },
                documents: None,
                esql: None,
            }],
            annotations: vec![annotation(Some("deploys"))],
            warnings: vec![],
        };
        let dashboard = [annotation(Some("deploys"))];
        let time = chart(Operation::DateHistogram {
            field: "deployed_at".into(),
            interval: "1w".parse().unwrap(),
            include_empty: false,
        });
        let teams = chart(Operation::Terms {
            fields: vec!["team".into()],
            size: 5,
            order: TermsOrder::Count,
            other_bucket: false,
            missing_bucket: false,
        });
        with_context(&[], &dashboard, |ctx| {
            let p = plan(&time, ctx);
            let described = p.described_searches();
            assert_eq!(
                described.len(),
                3,
                "the layer, then its own and the dashboard's annotation"
            );
            assert_eq!(described[1].0, "Annotation Failed");
            assert!(plan(&teams, ctx).described_searches().len() == 1);
            // Responses: the layer's, then one per annotation.
            let hit = json!({ "hits": { "hits": [{ "fields": { "deployed_at": ["1000"] } }] } });
            let data = p.evaluate(&[
                Ok(json!({ "aggregations": { "root": {} } })),
                Ok(hit.clone()),
                Err("boom".into()),
            ]);
            assert_eq!(data.annotations.len(), 2);
            assert_eq!(data.annotations[0].events.len(), 1);
            assert_eq!(data.annotations[1].error.as_deref(), Some("boom"));
        });
    }

    fn esql_chart(query: &str, columns: Vec<crate::model::Column>) -> Chart {
        use crate::model::{ChartKind, Encoding, Esql, Layer};
        Chart {
            kind: ChartKind::Table,
            query: Default::default(),
            filters: vec![],
            layers: vec![Layer {
                insight: None,
                id: "l".into(),
                data_view: DataView {
                    index: "deploys".into(),
                    time_field: Some("deployed_at".into()),
                },
                encoding: Encoding::Table {
                    columns: columns.iter().map(|c| c.id.clone()).collect(),
                    styles: Vec::new(),
                    sparkline: None,
                },
                columns,
                documents: None,
                esql: Some(Esql {
                    query: query.into(),
                }),
            }],
            annotations: vec![],
            warnings: vec![],
        }
    }

    fn value(id: &str, field: &str) -> crate::model::Column {
        crate::model::Column {
            id: id.into(),
            label: None,
            op: crate::model::Operation::Value {
                field: field.into(),
            },
            filter: None,
            time_shift: None,
            per: None,
            format: None,
            helper: false,
            thresholds: Vec::new(),
            shorten: false,
        }
    }

    #[test]
    fn esql_layers_send_one_query_filtered_like_kibana() {
        let chart = esql_chart(
            "FROM deploys | WHERE deployed_at >= ?_tstart | STATS n = COUNT(*) BY team",
            vec![value("team", "team"), value("n", "n")],
        );
        let selections = [("team".to_string(), vec!["checkout".to_string()])];
        with_context(&selections, &[], |ctx| {
            let searches = plan(&chart, ctx).searches();
            assert_eq!(searches.len(), 1);
            let s = &searches[0];
            assert_eq!(s.endpoint, crate::es::Endpoint::Esql);
            assert_eq!(
                s.body["query"],
                json!(chart.layers[0].esql.as_ref().unwrap().query)
            );
            let filter = s.body["filter"]["bool"]["filter"].as_array().unwrap();
            // The time range on the data view's time field, the dashboard
            // query, and the control selection.
            assert!(filter[0]["range"]["deployed_at"].is_object(), "{filter:#?}");
            assert_eq!(filter.len(), 3, "{filter:#?}");
            assert_eq!(
                s.body["params"],
                json!([{ "_tstart": "2026-07-01T00:00:00Z" }])
            );
        });
    }

    #[test]
    fn esql_results_become_columns_with_dates_as_epoch_millis() {
        let chart = esql_chart(
            "FROM deploys",
            vec![
                value("week", "week"),
                value("n", "n"),
                value("teams", "teams"),
            ],
        );
        with_context(&[], &[], |ctx| {
            let p = plan(&chart, ctx);
            let response = json!({
                "columns": [
                    { "name": "n", "type": "long" },
                    { "name": "week", "type": "date" },
                    { "name": "teams", "type": "keyword" }
                ],
                "values": [
                    [3, "2026-07-06T00:00:00.000Z", ["a", "b"]],
                    [null, "2026-07-13T00:00:00.000Z", "c"]
                ]
            });
            let data = p.evaluate(&[Ok(response)]);
            let layer = &data.layers[0];
            assert_eq!(layer.error, None);
            assert_eq!(layer.rows, 2);
            assert_eq!(layer.dates, ["week"]);
            assert_eq!(
                layer.series("week").unwrap().values,
                [
                    Cell::Number(ms("2026-07-06T00:00:00Z")),
                    Cell::Number(ms("2026-07-13T00:00:00Z"))
                ]
            );
            assert_eq!(
                layer.series("n").unwrap().values,
                [Cell::Number(3.0), Cell::Null]
            );
            assert_eq!(
                layer.series("teams").unwrap().values,
                [Cell::Text("a, b".into()), Cell::Text("c".into())]
            );

            let missing = p.evaluate(&[Ok(
                json!({ "columns": [{ "name": "x", "type": "long" }], "values": [] }),
            )]);
            let error = missing.layers[0].error.as_deref().unwrap();
            assert!(error.contains("no column \"week\""), "{error}");
        });
    }

    #[test]
    fn esql_layers_aggregate_in_the_query_not_in_columns() {
        let mut count = value("n", "n");
        count.op = crate::model::Operation::Count { field: None };
        let chart = esql_chart("FROM deploys", vec![count]);
        with_context(&[], &[], |ctx| {
            let data = plan(&chart, ctx).evaluate(&[]);
            assert!(
                data.layers[0]
                    .error
                    .as_deref()
                    .unwrap()
                    .contains("aggregate in the query")
            );
        });
    }

    #[test]
    fn esql_variables_are_found_outside_strings() {
        assert_eq!(
            esql_variables(
                "FROM x | WHERE team == ?team AND t >= ?_tstart | WHERE msg == \"why?\" | EVAL ??field"
            ),
            ["team", "field"]
        );
        assert!(esql_variables("FROM x | WHERE t <= ?_tend").is_empty());
    }

    fn insight_chart(columns: Vec<crate::model::Column>) -> Chart {
        use crate::model::{
            ChartKind, Encoding, InsightColumn, InsightGroupBy, InsightSeries, Layer,
        };
        Chart {
            kind: ChartKind::Table,
            query: Default::default(),
            filters: vec![],
            layers: vec![Layer {
                id: "l".into(),
                data_view: DataView {
                    index: "duplication_repo".into(),
                    time_field: Some("changed_at".into()),
                },
                encoding: Encoding::Table {
                    columns: columns.iter().map(|c| c.id.clone()).collect(),
                    styles: Vec::new(),
                    sparkline: None,
                },
                columns,
                documents: None,
                esql: None,
                insight: Some(InsightSeries {
                    interval: Some("1w".into()),
                    filters: vec![],
                    group_by: vec![InsightGroupBy {
                        field: "team".into(),
                        size: None,
                        order_by: None,
                        rest: None,
                    }],
                    columns: vec![InsightColumn {
                        id: "dup".into(),
                        op: "sum".into(),
                        field: Some("duplicate_lines".into()),
                        p: None,
                    }],
                    latest_by: Some("repo".into()),
                }),
            }],
            annotations: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn insight_layers_send_one_series_request() {
        let chart = insight_chart(vec![value("week", "t"), value("dup", "dup")]);
        with_context(&[], &[], |ctx| {
            let searches = plan(&chart, ctx).searches();
            assert_eq!(searches.len(), 1);
            let s = &searches[0];
            assert_eq!(s.endpoint, crate::es::Endpoint::Insight);
            assert_eq!(s.body["record_type"], "duplication_repo");
            assert_eq!(s.body["time"]["field"], "changed_at");
            assert_eq!(s.body["time"]["interval"], "1w");
            assert!(s.body["time"]["from"].as_str().unwrap().ends_with('Z'));
            assert_eq!(s.body["latest_by"], "repo");
            assert_eq!(s.body["group_by"][0]["field"], "team");
            assert_eq!(s.body["columns"][0]["op"], "sum");
        });
    }

    #[test]
    fn insight_responses_are_read_like_esql() {
        let chart = insight_chart(vec![
            value("week", "t"),
            value("team", "team"),
            value("dup", "dup"),
        ]);
        with_context(&[], &[], |ctx| {
            let p = plan(&chart, ctx);
            let series = json!({
                "columns": [
                    { "id": "t", "type": "date" },
                    { "id": "team", "type": "keyword" },
                    { "id": "dup", "type": "number" }
                ],
                "rows": [
                    ["2026-09-28T00:00:00+00:00", "personal:o", 30.0],
                    ["2026-10-05T00:00:00+00:00", "personal:o", 50.0]
                ],
                "coverage": { "matched": 4, "with_inputs": 4 },
                "query": "SELECT ...",
                "schema_version": "1.0"
            });
            let data = p.evaluate(&[Ok(crate::es::series_as_esql(&series))]);
            let layer = &data.layers[0];
            assert_eq!(layer.error, None);
            assert_eq!(layer.dates, ["week"]);
            assert_eq!(
                layer.series("week").unwrap().values,
                [
                    Cell::Number(ms("2026-09-28T00:00:00Z")),
                    Cell::Number(ms("2026-10-05T00:00:00Z"))
                ]
            );
            assert_eq!(
                layer.series("dup").unwrap().values,
                [Cell::Number(30.0), Cell::Number(50.0)]
            );
        });
    }

    #[test]
    fn insight_layers_aggregate_in_the_request_not_in_columns() {
        let mut count = value("n", "n");
        count.op = crate::model::Operation::Count { field: None };
        let chart = insight_chart(vec![count]);
        with_context(&[], &[], |ctx| {
            let data = plan(&chart, ctx).evaluate(&[]);
            let error = data.layers[0].error.as_deref().unwrap();
            assert!(error.contains("insight.columns"), "{error}");
        });
    }
}
