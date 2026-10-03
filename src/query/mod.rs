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
    Cell, Chart, Column, DataView, Filter, Layer, LayerData, Operation, PanelData, Query,
    QueryLanguage, Series, TermsOrder,
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
    /// Fields of each index pattern, from `_field_caps`.
    pub fields: &'a HashMap<String, Vec<Field>>,
    /// Min/max per (layer, column) for auto-interval histograms; see [`stats_searches`].
    pub stats: &'a HashMap<(String, String), (f64, f64)>,
    pub settings: Settings,
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
                    Search {
                        index: layer.data_view.index.clone(),
                        body,
                    },
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
    for (f, values) in ctx.selections.iter().filter(|(_, v)| !v.is_empty()) {
        filter.push(json!({ "terms": { f: values } }));
    }
    Ok(Search {
        index: view.index.clone(),
        body: json!({
            "size": 0, "track_total_hits": false,
            "query": { "bool": { "filter": filter, "must_not": must_not } },
            "aggs": { "values": { "terms": { "field": field, "size": size } } }
        }),
    })
}

/// Reads a stats search response.
pub fn read_stats(response: &Value) -> Option<(f64, f64)> {
    let aggs = &response["aggregations"];
    Some((
        aggs["min"]["value"].as_f64()?,
        aggs["max"]["value"].as_f64()?,
    ))
}

/// The searches for one chart, and how to turn their responses into data.
#[derive(Debug)]
pub struct Plan {
    layers: Vec<LayerPlan>,
    warnings: Vec<String>,
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
    /// The time range and zone, for normalizing by unit ([`Column::per`]).
    range: Range,
    time_zone: jiff::tz::TimeZone,
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
}

#[derive(Debug)]
struct ShiftGroup {
    /// Milliseconds the searched range lies before the main range.
    shift_ms: i64,
    metrics: Vec<MetricSpec>,
    search: Search,
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
        self.layers
            .iter()
            .filter_map(|l| l.built.as_ref().ok())
            .flat_map(|b| b.groups.iter().map(|g| g.search.clone()))
            .collect()
    }

    /// Turns the responses to [`Plan::searches`] (same order) into data.
    pub fn evaluate(&self, responses: &[Result<Value, String>]) -> PanelData {
        let mut responses = responses.iter();
        let mut data = PanelData {
            layers: Vec::new(),
            warnings: self.warnings.clone(),
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
            let group_responses: Vec<_> = built.groups.iter().map(|_| responses.next()).collect();
            let layer = match evaluate_layer(plan, built, &group_responses) {
                Ok(layer) => layer,
                Err(e) => LayerData {
                    layer: plan.layer.id.clone(),
                    error: Some(e),
                    ..Default::default()
                },
            };
            data.layers.push(layer);
        }
        data
    }
}

/// Plans the searches for a chart.
pub fn plan(chart: &Chart, ctx: &Context) -> Plan {
    let mut warnings = Vec::new();
    let layers = chart
        .layers
        .iter()
        .map(|layer| LayerPlan {
            layer: layer.clone(),
            built: build_layer(chart, layer, ctx, &mut warnings),
        })
        .collect();
    Plan { layers, warnings }
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

        let query = base_query(chart, layer, ctx, range)?;
        let body = json!({
            "size": 0,
            "track_total_hits": false,
            "query": query,
            "aggs": { "root": { "filter": { "match_all": {} }, "aggs": tree } }
        });
        groups.push(ShiftGroup {
            shift_ms: shift,
            metrics,
            search: Search {
                index: layer.data_view.index.clone(),
                body,
            },
        });
    }
    Ok(Built {
        buckets,
        groups,
        documents: None,
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
            search: Search {
                index: layer.data_view.index.clone(),
                body,
            },
        }],
        documents: Some(doc_fields),
        range: ctx.range,
        time_zone: time_zone(ctx),
    })
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
    for (field, values) in ctx.selections.iter().filter(|(_, v)| !v.is_empty()) {
        filter.push(json!({ "terms": { field: values } }));
    }
    Ok(json!({ "bool": { "filter": filter, "must_not": must_not } }))
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
    })
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
            .map(|hit| match &hit["fields"][&d.field][0] {
                Value::Number(v) => v.as_f64().map_or(Cell::Null, Cell::Number),
                Value::String(s) if d.date => s.parse::<f64>().map_or(Cell::Null, Cell::Number),
                Value::String(s) => Cell::Text(s.clone()),
                Value::Bool(b) => Cell::Text(b.to_string()),
                _ => Cell::Null,
            })
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
    })
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

#[cfg(test)]
mod tests {
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
}
