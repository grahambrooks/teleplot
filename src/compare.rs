//! Comparing a selection with its baseline, as Honeycomb's BubbleUp does:
//! which values are over-represented among the PRs a viewer drag-selected
//! on a scatter or heatmap, against all of the panel's PRs.
//!
//! Two searches (the selection, and the baseline: the panel's documents
//! under every dashboard filter) count each categorical field's top values
//! and each numeric field's median. Fields are ranked by how differently
//! their values are spread: half the summed differences in share (total
//! variation distance), which reads as "38% of the selection, 7% overall".
//! People are never ranked, and fields with many values (IDs) are skipped.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::es::Search;
use crate::model::api::{Comparison, FieldDiff, MeasureDiff, Selection, ValueDiff};
use crate::model::{Chart, Encoding, Layer, Operation};
use crate::query::{self, Context};

/// Fields with more values than this are left out: IDs, free text.
const MAX_VALUES: f64 = 30.0;

/// Name parts that mark a field about a person. Teleplot never ranks people.
const PEOPLE: &[&str] = &[
    "author",
    "authors",
    "assignee",
    "assignees",
    "reviewer",
    "reviewers",
    "committer",
    "creator",
    "owner",
    "user",
    "username",
    "login",
    "email",
    "approver",
    "requester",
    "person",
    "engineer",
    "developer",
    "member",
];

const CATEGORICAL: &[&str] = &["keyword", "constant_keyword", "boolean", "ip", "version"];
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

/// The two searches, and what their aggregations are.
#[derive(Debug)]
pub struct Plan {
    /// The selection, then the baseline.
    pub searches: Vec<Search>,
    categorical: Vec<String>,
    numeric: Vec<String>,
    skipped: Vec<String>,
}

/// Is this field about a person? Matches whole name parts: `author`,
/// `pr.reviewer`, `assignee_name`.
pub fn about_people(field: &str) -> bool {
    field
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| PEOPLE.contains(&part.to_ascii_lowercase().as_str()))
}

/// The layer a selection is made on, and the fields its x and y axes show.
fn axes(chart: &Chart) -> Result<(&Layer, String, Option<String>), String> {
    let layer = chart
        .layers
        .iter()
        .find(|l| !matches!(l.encoding, Encoding::ReferenceLines { .. }))
        .ok_or("the chart has no data layer")?;
    let field = |id: &str| -> Option<(&Operation, String)> {
        let op = &layer.column(id)?.op;
        let f = match op {
            Operation::Value { field }
            | Operation::DateHistogram { field, .. }
            | Operation::Histogram { field, .. }
            | Operation::LogHistogram { field, .. }
            | Operation::Ranges { field, .. } => field.clone(),
            _ => return None,
        };
        Some((op, f))
    };
    let time = |op: &Operation, f: &str| match op {
        Operation::DateHistogram { .. } => true,
        Operation::Value { .. } => layer.data_view.time_field.as_deref() == Some(f),
        _ => false,
    };
    let numeric_y = |op: &Operation| {
        matches!(
            op,
            Operation::Value { .. }
                | Operation::Histogram { .. }
                | Operation::LogHistogram { .. }
                | Operation::Ranges { .. }
        )
    };
    let (x, y) = match &layer.encoding {
        Encoding::Scatter { x, y, .. } => (x.clone(), Some(y.clone())),
        Encoding::Heatmap { x, y, .. } => (x.clone(), y.clone()),
        Encoding::Xy { x: Some(x), .. } | Encoding::Band { x, .. } => (x.clone(), None),
        _ => return Err("compare works on charts with a time x axis".into()),
    };
    let (x_op, x_field) = field(&x).ok_or("compare needs a field on the x axis")?;
    if !time(x_op, &x_field) {
        return Err("compare works on charts with a time x axis".into());
    }
    let y_field = y
        .as_deref()
        .and_then(field)
        .filter(|(op, _)| numeric_y(op))
        .map(|(_, f)| f);
    Ok((layer, x_field, y_field))
}

/// Plans the comparison of `selection` on `chart` (under `ctx`).
pub fn plan(chart: &Chart, selection: &Selection, ctx: &Context) -> Result<Plan, String> {
    if chart.layers.iter().any(|l| l.esql.is_some()) {
        return Err("compare does not work on ES|QL panels".into());
    }
    let (layer, x_field, y_field) = axes(chart)?;
    let baseline = query::layer_query(chart, layer, ctx)?;

    let mut selected = baseline.clone();
    let filter = selected["bool"]["filter"]
        .as_array_mut()
        .ok_or("unexpected query shape")?;
    filter.push(json!({ "range": { &x_field: {
        "gte": selection.x.0.floor() as i64, "lte": selection.x.1.ceil() as i64, "format": "epoch_millis"
    } } }));
    if let (Some(field), Some((low, high))) = (&y_field, selection.y) {
        let mut range = json!({});
        if let Some(low) = low {
            range["gte"] = json!(low);
        }
        if let Some(high) = high {
            range["lt"] = json!(high);
        }
        if range.as_object().is_some_and(|r| !r.is_empty()) {
            filter.push(json!({ "range": { field: range } }));
        }
    }

    // Candidate fields: every keyword-like and numeric field but the axes.
    let fields = ctx
        .fields
        .get(&layer.data_view.index)
        .cloned()
        .unwrap_or_default();
    let mut categorical = Vec::new();
    let mut numeric = Vec::new();
    let mut skipped = Vec::new();
    for f in &fields {
        if f.name == x_field || Some(&f.name) == y_field.as_ref() {
            continue;
        }
        let is = |kinds: &[&str]| f.es_types.iter().all(|t| kinds.contains(&t.as_str()));
        if !is(CATEGORICAL) && !is(NUMERIC) {
            continue;
        }
        if about_people(&f.name) {
            skipped.push(format!(
                "{}: about people, which Teleplot never ranks",
                f.name
            ));
        } else if is(CATEGORICAL) {
            categorical.push(f.name.clone());
        } else {
            numeric.push(f.name.clone());
        }
    }

    let mut aggs = serde_json::Map::new();
    for (i, f) in categorical.iter().enumerate() {
        // Every value of a field that is compared at all, so shares are exact.
        aggs.insert(
            format!("c{i}"),
            json!({ "terms": { "field": f, "size": MAX_VALUES as u32 } }),
        );
        aggs.insert(format!("k{i}"), json!({ "cardinality": { "field": f } }));
    }
    for (i, f) in numeric.iter().enumerate() {
        aggs.insert(
            format!("n{i}"),
            json!({ "percentiles": { "field": f, "percents": [50.0] } }),
        );
    }
    let body = |q: Value| json!({ "size": 0, "track_total_hits": true, "query": q, "aggs": aggs });
    let index = layer.data_view.index.clone();
    Ok(Plan {
        searches: vec![
            Search::dsl(index.clone(), body(selected)),
            Search::dsl(index, body(baseline)),
        ],
        categorical,
        numeric,
        skipped,
    })
}

fn total(response: &Value) -> u64 {
    response["hits"]["total"]["value"].as_u64().unwrap_or(0)
}

fn counts(response: &Value, agg: &str) -> HashMap<String, u64> {
    response["aggregations"][agg]["buckets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let key =
                b["key_as_string"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| match &b["key"] {
                        Value::String(s) => Some(s.clone()),
                        Value::Null => None,
                        other => Some(other.to_string()),
                    })?;
            Some((key, b["doc_count"].as_u64().unwrap_or(0)))
        })
        .collect()
}

/// Ranks the fields from the two responses (selection, baseline).
pub fn rank(plan: &Plan, selected: &Value, baseline: &Value) -> Comparison {
    let (n_selected, n_baseline) = (total(selected), total(baseline));
    let share = |count: u64, n: u64| if n == 0 { 0.0 } else { count as f64 / n as f64 };
    let mut skipped = plan.skipped.clone();

    let mut fields = Vec::new();
    for (i, field) in plan.categorical.iter().enumerate() {
        let values = baseline["aggregations"][format!("k{i}")]["value"]
            .as_f64()
            .unwrap_or(0.0);
        if values > MAX_VALUES {
            skipped.push(format!("{field}: {values:.0} values, too many to compare"));
            continue;
        }
        let sel = counts(selected, &format!("c{i}"));
        let base = counts(baseline, &format!("c{i}"));
        let mut keys: Vec<&String> = base.keys().chain(sel.keys()).collect();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            continue;
        }
        let mut diffs: Vec<ValueDiff> = keys
            .into_iter()
            .map(|k| {
                let (s, b) = (
                    sel.get(k).copied().unwrap_or(0),
                    base.get(k).copied().unwrap_or(0),
                );
                ValueDiff {
                    value: k.clone(),
                    selected: s,
                    baseline: b,
                    selected_share: share(s, n_selected),
                    baseline_share: share(b, n_baseline),
                }
            })
            .collect();
        // Documents without the field, or beyond the top values, are one
        // more "value", so the score covers everything.
        let rest = |shares: f64| (1.0 - shares).max(0.0);
        let listed_sel: f64 = diffs.iter().map(|d| d.selected_share).sum();
        let listed_base: f64 = diffs.iter().map(|d| d.baseline_share).sum();
        let score = 0.5
            * (diffs
                .iter()
                .map(|d| (d.selected_share - d.baseline_share).abs())
                .sum::<f64>()
                + (rest(listed_sel) - rest(listed_base)).abs());
        diffs.sort_by(|a, b| {
            let change = |d: &ValueDiff| (d.selected_share - d.baseline_share).abs();
            change(b).total_cmp(&change(a))
        });
        diffs.truncate(5);
        diffs.sort_by(|a, b| {
            (b.selected_share - b.baseline_share).total_cmp(&(a.selected_share - a.baseline_share))
        });
        fields.push(FieldDiff {
            field: field.clone(),
            score,
            values: diffs,
        });
    }
    fields.sort_by(|a, b| b.score.total_cmp(&a.score));

    let median = |r: &Value, i: usize| -> Option<f64> {
        r["aggregations"][format!("n{i}")]["values"]
            .as_object()?
            .values()
            .next()?
            .as_f64()
    };
    let mut measures: Vec<MeasureDiff> = plan
        .numeric
        .iter()
        .enumerate()
        .filter_map(|(i, field)| {
            let (s, b) = (median(selected, i)?, median(baseline, i)?);
            (b != 0.0 && s.is_finite()).then(|| MeasureDiff {
                field: field.clone(),
                selected_median: s,
                baseline_median: b,
            })
        })
        .collect();
    let distance = |m: &MeasureDiff| {
        (m.selected_median.max(1e-9) / m.baseline_median.max(1e-9))
            .ln()
            .abs()
    };
    measures.sort_by(|a, b| distance(b).total_cmp(&distance(a)));

    Comparison {
        selected: n_selected,
        baseline: n_baseline,
        fields,
        measures,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_fields_are_recognised_by_name_part() {
        assert!(about_people("author"));
        assert!(about_people("pr.reviewer"));
        assert!(about_people("assignee_name"));
        assert!(!about_people("team"));
        assert!(!about_people("authorized")); // a whole part, not a prefix
    }

    fn response(total: u64, aggs: Value) -> Value {
        json!({ "hits": { "total": { "value": total } }, "aggregations": aggs })
    }

    fn plan_of(categorical: &[&str], numeric: &[&str]) -> Plan {
        Plan {
            searches: Vec::new(),
            categorical: categorical.iter().map(|s| s.to_string()).collect(),
            numeric: numeric.iter().map(|s| s.to_string()).collect(),
            skipped: vec!["author: about people, which Teleplot never ranks".into()],
        }
    }

    #[test]
    fn fields_rank_by_how_differently_their_values_spread() {
        let plan = plan_of(&["team", "state", "pr_id"], &["lines_changed"]);
        // The selection (10 PRs) is mostly team "data"; overall it's 10%.
        let selected = response(
            10,
            json!({
                "c0": { "buckets": [{ "key": "data", "doc_count": 8 }, { "key": "infra", "doc_count": 2 }] },
                "k0": { "value": 2 },
                "c1": { "buckets": [{ "key": "merged", "doc_count": 10 }] },
                "k1": { "value": 1 },
                "c2": { "buckets": [] },
                "k2": { "value": 10 },
                "n0": { "values": { "50.0": 400.0 } }
            }),
        );
        let baseline = response(
            100,
            json!({
                "c0": { "buckets": [{ "key": "infra", "doc_count": 50 }, { "key": "data", "doc_count": 10 }, { "key": "billing", "doc_count": 40 }] },
                "k0": { "value": 3 },
                "c1": { "buckets": [{ "key": "merged", "doc_count": 100 }] },
                "k1": { "value": 1 },
                "c2": { "buckets": [] },
                "k2": { "value": 100 },
                "n0": { "values": { "50.0": 100.0 } }
            }),
        );
        let c = rank(&plan, &selected, &baseline);
        assert_eq!((c.selected, c.baseline), (10, 100));
        assert_eq!(c.fields[0].field, "team");
        // |0.8-0.1| + |0.2-0.5| + |0-0.4| = 1.4, halved.
        assert!(
            (c.fields[0].score - 0.7).abs() < 1e-9,
            "{}",
            c.fields[0].score
        );
        assert_eq!(
            c.fields[0].values[0].value, "data",
            "over-represented first"
        );
        assert_eq!(c.fields[1].field, "state");
        assert_eq!(c.fields[1].score, 0.0);
        // IDs (100 values) are skipped, and so are people (from the plan).
        assert!(c.skipped.iter().any(|s| s.starts_with("pr_id")));
        assert!(c.skipped.iter().any(|s| s.starts_with("author")));
        assert_eq!(c.measures[0].field, "lines_changed");
        assert_eq!(c.measures[0].selected_median, 400.0);
    }
}
