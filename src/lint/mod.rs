//! Checking dashboards before they are used: parsing with exact error paths,
//! then semantic checks a YAML parser cannot make — references between
//! columns, field types, query syntax — against live field capabilities
//! when they are available.
//!
//! Used by `teleplot lint` and by the MCP `validate`/`save` tools, so a
//! person in CI and an agent get the same answers.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;

use crate::kql::{self, Field};
use crate::model::{
    Annotation, Chart, ChartKind, Dashboard, Encoding, Expr, Operation, PanelContent, Query,
    QueryLanguage,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
pub struct Problem {
    /// Where in the dashboard, e.g. `panels[3].content.layers[0].columns[2]`.
    pub path: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ssr", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The dashboard cannot work as written.
    Error,
    /// It works, but probably not as intended.
    Warning,
}

impl Problem {
    fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Problem {
            path: path.into(),
            severity: Severity::Error,
            message: message.into(),
        }
    }

    fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Problem {
            path: path.into(),
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

pub fn has_errors(problems: &[Problem]) -> bool {
    problems.iter().any(|p| p.severity == Severity::Error)
}

/// Parses a dashboard from JSON, reporting the exact path of a mistake.
pub fn parse(value: Value) -> Result<Dashboard, Problem> {
    match serde_path_to_error::deserialize::<_, Dashboard>(value.clone()) {
        Ok(d) => Ok(d),
        Err(e) => {
            let path = e.path().to_string();
            let path = if path == "." { String::new() } else { path };
            // Serde buffers internally tagged enums (`content: { type: chart }`),
            // which hides where inside them the mistake is: look again from there.
            Err(deeper(&value, &path)
                .unwrap_or_else(|| Problem::error(path, e.inner().to_string())))
        }
    }
}

fn deeper(root: &Value, path: &str) -> Option<Problem> {
    let mut node = root;
    for part in path.split('.').filter(|p| !p.is_empty()) {
        let (name, index) = match part.split_once('[') {
            Some((name, rest)) => (name, rest.trim_end_matches(']').parse::<usize>().ok()),
            None => (part, None),
        };
        node = node.get(name)?;
        if let Some(i) = index {
            node = node.get(i)?;
        }
    }
    if node.get("type")?.as_str()? != "chart" {
        return None;
    }
    let mut chart = node.as_object()?.clone();
    chart.remove("type");
    let e = serde_path_to_error::deserialize::<_, Chart>(Value::Object(chart)).err()?;
    Some(Problem::error(
        format!("{path}.{}", e.path()),
        e.inner().to_string(),
    ))
}

/// Fields per index pattern. `None` skips the checks that need a cluster.
pub type Fields<'a> = Option<&'a HashMap<String, Vec<Field>>>;

pub fn check(d: &Dashboard, fields: Fields) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut seen = HashSet::new();

    if let Some(t) = &d.time {
        for (name, expr) in [("from", &t.from), ("to", &t.to)] {
            if let Err(e) = crate::time::parse(
                expr,
                jiff::Timestamp::now(),
                &jiff::tz::TimeZone::UTC,
                name == "to",
            ) {
                problems.push(Problem::error(format!("time.{name}"), e));
            }
        }
    }
    query(&d.query, "query", &mut problems);

    for (i, c) in d.controls.iter().enumerate() {
        field(
            fields,
            &c.data_view.index,
            &c.field,
            Need::Aggregatable,
            &format!("controls[{i}].field"),
            &mut problems,
        );
    }

    for (i, a) in d.annotations.iter().enumerate() {
        annotation(a, &format!("annotations[{i}]"), fields, &mut problems);
    }

    for (i, p) in d.panels.iter().enumerate() {
        let at = format!("panels[{i}]");
        if !seen.insert(p.id.clone()) {
            problems.push(Problem::error(
                format!("{at}.id"),
                format!("panel id {:?} is used twice", p.id),
            ));
        }
        if p.grid.x + p.grid.w > 48 {
            problems.push(Problem::error(
                format!("{at}.grid"),
                format!(
                    "x + w is {}; the grid is 48 columns wide",
                    p.grid.x + p.grid.w
                ),
            ));
        }
        if p.grid.w == 0 || p.grid.h == 0 {
            problems.push(Problem::error(
                format!("{at}.grid"),
                "w and h must be at least 1",
            ));
        }
        if let PanelContent::Chart(chart) = &p.content {
            check_chart(chart, &format!("{at}.content"), fields, &mut problems);
        }
    }
    problems
}

fn query(q: &Query, at: &str, problems: &mut Vec<Problem>) {
    if q.language == QueryLanguage::Kql
        && !q.is_empty()
        && let Err(e) = kql::parse(&q.text)
    {
        problems.push(Problem::error(format!("{at}.text"), format!("KQL: {e}")));
    }
}

/// An annotation needs either a data view (to search) or fixed events, and
/// fields of the right types.
fn annotation(a: &Annotation, at: &str, fields: Fields, problems: &mut Vec<Problem>) {
    let Some(view) = &a.data_view else {
        if a.events.is_empty() {
            problems.push(Problem::error(
                at,
                "an annotation needs a data_view to search, or fixed events",
            ));
        }
        for (i, e) in a.events.iter().enumerate() {
            for (name, time) in [("at", Some(&e.at)), ("to", e.to.as_ref())] {
                if let Some(time) = time
                    && let Err(err) = time.parse::<jiff::Timestamp>()
                {
                    problems.push(Problem::error(
                        format!("{at}.events[{i}].{name}"),
                        format!("not an ISO 8601 time: {err}"),
                    ));
                }
            }
        }
        return;
    };
    if !a.events.is_empty() {
        problems.push(Problem::error(
            format!("{at}.events"),
            "an annotation searches a data view or lists fixed events, not both",
        ));
    }
    query(&a.query, &format!("{at}.query"), problems);
    let index = &view.index;
    match a.time.as_ref().or(view.time_field.as_ref()) {
        Some(time) => field(
            fields,
            index,
            time,
            Need::Date,
            &format!("{at}.time"),
            problems,
        ),
        None => problems.push(Problem::error(
            format!("{at}.time"),
            "the data view has no time field: say which field holds each event's time",
        )),
    }
    if let Some(end) = &a.end {
        field(
            fields,
            index,
            end,
            Need::Date,
            &format!("{at}.end"),
            problems,
        );
    }
    if let Some(text) = &a.text {
        field(
            fields,
            index,
            text,
            Need::Any,
            &format!("{at}.text"),
            problems,
        );
    }
}

fn check_chart(chart: &Chart, at: &str, fields: Fields, problems: &mut Vec<Problem>) {
    query(&chart.query, &format!("{at}.query"), problems);
    for (i, a) in chart.annotations.iter().enumerate() {
        annotation(a, &format!("{at}.annotations[{i}]"), fields, problems);
    }
    if !chart.annotations.is_empty() && !chart.has_time_axis() {
        problems.push(Problem::warning(
            format!("{at}.annotations"),
            "only charts with a time x axis show annotations",
        ));
    }
    if chart.layers.is_empty() {
        problems.push(Problem::error(
            format!("{at}.layers"),
            "a chart needs at least one layer",
        ));
    }
    for (l, layer) in chart.layers.iter().enumerate() {
        let at = format!("{at}.layers[{l}]");
        let index = &layer.data_view.index;
        // An insight layer's index is a record type, not an Elasticsearch
        // index, so Elasticsearch's field list doesn't describe it.
        let fields = if layer.insight.is_some() {
            None
        } else {
            fields
        };
        if let Some(all) = fields
            && all.get(index).is_none_or(Vec::is_empty)
        {
            problems.push(Problem::error(
                format!("{at}.data_view.index"),
                format!("no index matches {index:?} (or it has no fields)"),
            ));
        }
        if let Some(time_field) = &layer.data_view.time_field {
            field(
                fields,
                index,
                time_field,
                Need::Date,
                &format!("{at}.data_view.time_field"),
                problems,
            );
        }

        let ids: HashSet<&str> = layer.columns.iter().map(|c| c.id.as_str()).collect();
        let mut seen = HashSet::new();
        for (c, column) in layer.columns.iter().enumerate() {
            let at = format!("{at}.columns[{c}]");
            if !seen.insert(column.id.as_str()) {
                problems.push(Problem::error(
                    format!("{at}.id"),
                    format!("column id {:?} is used twice in this layer", column.id),
                ));
            }
            if let Some(filter) = &column.filter {
                query(filter, &format!("{at}.filter"), problems);
            }
            for input in column.op.inputs() {
                if !ids.contains(input) {
                    problems.push(Problem::error(
                        at.clone(),
                        format!("refers to column {input:?}, which this layer does not have"),
                    ));
                }
            }
            if let Operation::Formula { expr, .. } = &column.op {
                functions(expr, &at, problems);
            }
            if column.per.is_some() && column.op.is_bucket() {
                problems.push(Problem::error(
                    format!("{at}.per"),
                    "only metric and computed columns can be normalized by unit",
                ));
            }
            if let Operation::Terms {
                order: crate::model::TermsOrder::Column { column: by, .. },
                ..
            } = &column.op
                && layer.column(by).is_some_and(|c| !c.op.inputs().is_empty())
            {
                problems.push(Problem::warning(
                    format!("{at}.order"),
                    format!(
                        "{by:?} is computed from other columns, so buckets are ordered by document count instead; \
                         order by a plain metric (count, sum, median, …) to rank by value"
                    ),
                ));
            }
            // ES|QL and insight layers' `value` columns name result columns,
            // not Elasticsearch fields.
            if let Some((name, need)) = needs(&column.op)
                && layer.esql.is_none()
                && layer.insight.is_none()
            {
                field(fields, index, name, need, &format!("{at}.field"), problems);
            }
            let rows = layer.documents.is_some() || layer.esql.is_some() || layer.insight.is_some();
            match (&column.op, rows) {
                (Operation::Value { .. }, false) => problems.push(Problem::error(
                    at.clone(),
                    "`value` columns need a document layer (`documents: { limit, sort }`), an `esql` query or an `insight` series",
                )),
                (op, true) if op.inputs().is_empty() && !matches!(op, Operation::Value { .. }) => {
                    problems.push(Problem::error(
                        at.clone(),
                        "document, ES|QL and insight layers can only have `value` and computed columns; aggregate in the ES|QL query or `insight.columns`",
                    ))
                }
                _ => {}
            }
            for t in &column.thresholds {
                if let (Some(from), Some(to)) = (t.from, t.to)
                    && from >= to
                {
                    problems.push(Problem::error(
                        format!("{at}.thresholds"),
                        format!("{:?}: from must be below to", t.label),
                    ));
                }
            }
        }
        if let Some(esql) = &layer.esql {
            if layer.documents.is_some() {
                problems.push(Problem::error(
                    format!("{at}.esql"),
                    "a layer runs an ES|QL query or fetches documents, not both",
                ));
            }
            if esql.query.trim().is_empty() {
                problems.push(Problem::error(
                    format!("{at}.esql.query"),
                    "empty ES|QL query",
                ));
            }
            let variables = crate::query::esql_variables(&esql.query);
            if !variables.is_empty() {
                problems.push(Problem::error(
                    format!("{at}.esql.query"),
                    format!(
                        "uses ES|QL variables ({}); Teleplot fills in only ?_tstart and ?_tend",
                        variables
                            .iter()
                            .map(|v| format!("?{v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        }
        if layer.insight.is_some() {
            if layer.esql.is_some() || layer.documents.is_some() {
                problems.push(Problem::error(
                    format!("{at}.insight"),
                    "a layer queries insight, runs an ES|QL query or fetches documents: only one",
                ));
            }
            if layer.data_view.time_field.is_none() {
                problems.push(Problem::error(
                    format!("{at}.data_view.time_field"),
                    "an insight layer needs the record type's time field, e.g. `changed_at`",
                ));
            }
        }
        if let Some(docs) = &layer.documents {
            field(
                fields,
                index,
                &docs.sort,
                Need::Sortable,
                &format!("{at}.documents.sort"),
                problems,
            );
        }

        for id in layer.encoding.columns() {
            if !ids.contains(id) {
                problems.push(Problem::error(
                    format!("{at}.encoding"),
                    format!("draws column {id:?}, which this layer does not have"),
                ));
            }
        }
        let fits = matches!(
            (&layer.encoding, chart.kind),
            (Encoding::Metric { .. }, ChartKind::Metric)
                | (
                    Encoding::Pie { .. },
                    ChartKind::Pie | ChartKind::Donut | ChartKind::Treemap | ChartKind::Waffle
                )
                | (Encoding::Gauge { .. }, ChartKind::Gauge)
                | (Encoding::Calendar { .. }, ChartKind::Calendar)
                | (Encoding::Table { .. }, ChartKind::Table)
                | (Encoding::Heatmap { .. }, ChartKind::Heatmap)
                | (
                    Encoding::Sparkline { .. },
                    ChartKind::Metric | ChartKind::Table
                )
                | (
                    Encoding::Xy { .. }
                        | Encoding::Band { .. }
                        | Encoding::Scatter { .. }
                        | Encoding::ReferenceLines { .. },
                    ChartKind::Xy
                )
        );
        if !fits {
            problems.push(Problem::error(
                format!("{at}.encoding"),
                format!("encoding does not suit a {:?} chart", chart.kind),
            ));
        }
        // One point per row: documents, or the rows an insight or ES|QL
        // query returns.
        if matches!(layer.encoding, Encoding::Scatter { .. })
            && layer.documents.is_none()
            && layer.insight.is_none()
            && layer.esql.is_none()
        {
            problems.push(Problem::error(
                format!("{at}.encoding"),
                "scatter charts need a document, insight or ES|QL layer",
            ));
        }
        if let Encoding::Table {
            columns,
            styles,
            sparkline,
        } = &layer.encoding
        {
            for (i, style) in styles.iter().enumerate() {
                if !columns.contains(&style.column) {
                    problems.push(Problem::error(
                        format!("{at}.encoding.styles[{i}].column"),
                        format!("{:?} is not one of the table's columns", style.column),
                    ));
                }
            }
            if let Some(spark) = sparkline
                && !chart.layers.iter().any(|l| {
                    l.id == spark.layer && matches!(l.encoding, Encoding::Sparkline { .. })
                })
            {
                problems.push(Problem::error(
                    format!("{at}.encoding.sparkline.layer"),
                    format!("no sparkline layer {:?} in this chart", spark.layer),
                ));
            }
        }
        if let Encoding::Xy { options, .. } = &layer.encoding
            && let Some(facet) = &options.facet
            && !matches!(
                layer.column(facet).map(|c| &c.op),
                Some(Operation::Terms { .. }) | None
            )
        {
            problems.push(Problem::error(
                format!("{at}.encoding.facet"),
                format!("facet {facet:?} must be a terms column: one small chart per value"),
            ));
        }
        // insight and ES|QL layers bucket in their query: their result
        // columns are `value`s naming the buckets.
        let result_buckets = layer.insight.is_some() || layer.esql.is_some();
        let is_result_value =
            |c: &crate::model::Column| result_buckets && matches!(c.op, Operation::Value { .. });
        if let Encoding::Calendar { date, .. } = &layer.encoding
            && !layer.column(date).is_none_or(|c| {
                matches!(c.op, Operation::DateHistogram { .. }) || is_result_value(c)
            })
        {
            problems.push(Problem::error(
                format!("{at}.encoding.date"),
                format!("calendar date {date:?} must be a date histogram (interval 1d)"),
            ));
        }
        if let Encoding::Heatmap { x, y, value } = &layer.encoding {
            for axis in [Some(x), y.as_ref()].into_iter().flatten() {
                if layer
                    .column(axis)
                    .is_some_and(|c| !c.op.is_bucket() && !is_result_value(c))
                {
                    problems.push(Problem::error(
                        format!("{at}.encoding"),
                        format!("heatmap axis {axis:?} must be a bucket column"),
                    ));
                }
            }
            if layer.column(value).is_some_and(|c| c.op.is_bucket()) {
                problems.push(Problem::error(
                    format!("{at}.encoding"),
                    format!("heatmap value {value:?} must be a metric, not a bucket column"),
                ));
            }
        }
        if let Encoding::Scatter { percentiles, .. } = &layer.encoding
            && let Some(p) = percentiles.iter().find(|p| !(**p > 0.0 && **p <= 100.0))
        {
            problems.push(Problem::error(
                format!("{at}.encoding.percentiles"),
                format!("percentile {p} is outside 0–100"),
            ));
        }
        for (c, column) in layer.columns.iter().enumerate() {
            if let Operation::LogHistogram { min, max, .. } = column.op
                && !(min > 0.0 && max > min)
            {
                problems.push(Problem::error(
                    format!("{at}.columns[{c}]"),
                    format!("log_histogram needs 0 < min < max (got {min} and {max})"),
                ));
            }
        }
        if matches!(layer.encoding, Encoding::ReferenceLines { .. }) {
            if layer.columns.iter().any(|c| c.op.is_bucket()) || layer.documents.is_some() {
                problems.push(Problem::error(
                    format!("{at}.columns"),
                    "a reference-line layer has one value per line: no bucket columns or documents",
                ));
            }
            if !chart
                .layers
                .iter()
                .any(|l| !matches!(l.encoding, Encoding::ReferenceLines { .. }))
            {
                problems.push(Problem::error(
                    format!("{at}.encoding"),
                    "reference lines need a data layer to draw over",
                ));
            }
        }
        // ES|QL and insight layers group in the query.
        if matches!(layer.encoding, Encoding::Xy { .. } | Encoding::Band { .. })
            && layer.esql.is_none()
            && layer.insight.is_none()
            && !layer.columns.iter().any(|c| c.op.is_bucket())
        {
            problems.push(Problem::warning(
                format!("{at}.columns"),
                "no bucket column: the chart will have a single point",
            ));
        }
    }
}

const FUNCTIONS: &[&str] = &[
    "add", "subtract", "multiply", "divide", "abs", "cbrt", "ceil", "floor", "cube", "square",
    "sqrt", "exp", "log", "log10", "mod", "pow", "round", "clamp", "pick_max", "pick_min",
    "defaults",
];

fn functions(expr: &Expr, at: &str, problems: &mut Vec<Problem>) {
    if let Expr::Call { call, args } = expr {
        if !FUNCTIONS.contains(&call.as_str()) {
            problems.push(Problem::error(
                format!("{at}.expr"),
                format!(
                    "unknown function {call:?}; available: {}",
                    FUNCTIONS.join(", ")
                ),
            ));
        }
        for a in args {
            functions(a, at, problems);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    Date,
    Numeric,
    Aggregatable,
    Sortable,
    Any,
}

/// The field an operation reads and what kind of field it needs.
fn needs(op: &Operation) -> Option<(&str, Need)> {
    Some(match op {
        Operation::Count { field: Some(f) } => (f, Need::Any),
        Operation::Sum { field }
        | Operation::Average { field }
        | Operation::Median { field }
        | Operation::StandardDeviation { field }
        | Operation::Percentile { field, .. }
        | Operation::PercentileRank { field, .. }
        | Operation::Histogram { field, .. }
        | Operation::Ranges { field, .. }
        | Operation::LogHistogram { field, .. } => (field, Need::Numeric),
        Operation::Min { field } | Operation::Max { field } => (field, Need::Sortable),
        Operation::UniqueCount { field } => (field, Need::Aggregatable),
        Operation::LastValue { field, .. } | Operation::Value { field } => (field, Need::Any),
        Operation::DateHistogram { field, .. } => (field, Need::Date),
        Operation::Terms { fields, .. } => (fields.first()?, Need::Aggregatable),
        _ => return None,
    })
}

const NUMERIC: &[&str] = &[
    "long",
    "integer",
    "short",
    "byte",
    "double",
    "float",
    "half_float",
    "scaled_float",
    "unsigned_long",
];

fn field(
    fields: Fields,
    index: &str,
    name: &str,
    need: Need,
    at: &str,
    problems: &mut Vec<Problem>,
) {
    let Some(all) = fields.and_then(|f| f.get(index)) else {
        return;
    };
    if all.is_empty() {
        return; // the index itself is reported
    }
    let Some(f) = all.iter().find(|f| f.name == name) else {
        let near: Vec<&str> = all
            .iter()
            .map(|f| f.name.as_str())
            .filter(|n| n.contains(name) || name.contains(*n) || similar(n, name))
            .take(5)
            .collect();
        let hint = if near.is_empty() {
            String::new()
        } else {
            format!("; did you mean {}?", near.join(", "))
        };
        problems.push(Problem::error(
            at,
            format!("{index} has no field {name:?}{hint}"),
        ));
        return;
    };
    let is = |types: &[&str]| f.es_types.iter().all(|t| types.contains(&t.as_str()));
    let ok = match need {
        Need::Any => true,
        Need::Date => is(&["date", "date_nanos"]),
        Need::Numeric => is(NUMERIC),
        Need::Sortable => !f.es_types.iter().any(|t| t == "text"),
        Need::Aggregatable => !f.es_types.iter().any(|t| t == "text"),
    };
    if !ok {
        let wanted = match need {
            Need::Date => "a date",
            Need::Numeric => "a number",
            _ => "an aggregatable (e.g. keyword) field",
        };
        problems.push(Problem::error(
            at,
            format!(
                "{name} is {}, but this needs {wanted}",
                f.es_types.join("/")
            ),
        ));
    }
}

/// Edit distance at most 2, for "did you mean" suggestions.
fn similar(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 2 {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()] <= 2
}

#[cfg(test)]
mod tests;
