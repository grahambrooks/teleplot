//! Golden tests for the searches Teleplot sends: every chart in the delivery
//! fixtures, planned over a fixed range with real `_field_caps` output.

#![cfg(feature = "ssr")]

use std::collections::HashMap;

use serde_json::{Value, json};
use teleplot::kql::Field;
use teleplot::model::{Dashboard, PanelContent};
use teleplot::query::{self, Context, Settings};
use teleplot::time::Range;

fn root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

fn field_caps() -> HashMap<String, Vec<Field>> {
    [
        "delivery-prs",
        "delivery-deployments",
        "delivery-incidents",
        "delivery-workitems",
    ]
    .into_iter()
    .map(|index| {
        let text = std::fs::read_to_string(format!("{}/fixtures/field-caps/{index}.json", root()))
            .unwrap();
        let caps: Value = serde_json::from_str(&text).unwrap();
        (index.to_string(), teleplot::es::fields_from_caps(&caps))
    })
    .collect()
}

fn dashboards() -> Vec<Dashboard> {
    teleplot::dashboards::load_file(
        format!("{}/fixtures/kibana/8.19/delivery.ndjson", root()).as_ref(),
    )
    .unwrap()
}

/// 2026-04-01T00:00Z .. 2026-10-02T00:00Z, as in Kibana's reference screenshots.
const RANGE: Range = Range {
    from_ms: 1_775_001_600_000,
    to_ms: 1_790_899_200_000,
};

/// Every search for a dashboard, keyed by panel.
fn searches(d: &Dashboard, stats: &HashMap<(String, String), (f64, f64)>) -> Value {
    let fields = field_caps();
    let ctx = Context {
        range: RANGE,
        time_zone: "UTC",
        queries: std::slice::from_ref(&d.query),
        filters: &d.filters,
        selections: &[],
        fields: &fields,
        stats,
        settings: Settings::default(),
    };
    let mut out = serde_json::Map::new();
    for panel in &d.panels {
        let PanelContent::Chart(chart) = &panel.content else {
            continue;
        };
        let plan = query::plan(chart, &ctx);
        let searches: Vec<Value> = plan
            .searches()
            .into_iter()
            .map(|s| json!({ "index": s.index, "body": s.body }))
            .collect();
        out.insert(panel.id.clone(), Value::Array(searches));
    }
    Value::Object(out)
}

#[test]
fn delivery_requests_match_snapshots() {
    let stats = HashMap::from([(
        ("layer-distribution".to_string(), "ds-x".to_string()),
        (0.5, 3400.0),
    )]);
    for d in dashboards() {
        insta::assert_json_snapshot!(d.id.clone(), searches(&d, &stats));
    }
}

fn find<'a>(all: &'a [Dashboard], id: &str) -> &'a Dashboard {
    all.iter().find(|d| d.id == id).unwrap()
}

#[test]
fn shifted_columns_get_their_own_search_over_the_earlier_range() {
    let all = dashboards();
    let s = searches(find(&all, "teleplot-delivery-overview"), &HashMap::new());
    let deploys = s["deploys-per-week"].as_array().unwrap();
    assert_eq!(deploys.len(), 2, "main range, then the previous period");
    let range =
        |i: usize| deploys[i]["body"]["query"]["bool"]["filter"][0]["range"]["deployed_at"].clone();
    assert_eq!(range(0)["gte"], json!(RANGE.from_ms));
    assert_eq!(range(1)["lte"], json!(RANGE.from_ms));
}

#[test]
fn kql_uses_field_types_and_dashboard_filters_apply() {
    let all = dashboards();
    let s = searches(find(&all, "teleplot-investment"), &HashMap::new());
    let query = &s["allocation"][0]["body"]["query"]["bool"];
    // `NOT status : "todo"` on a keyword field → term; the negated pill → must_not.
    assert_eq!(
        query["filter"][1],
        json!({ "bool": { "must_not": { "bool": { "should": [{ "term": { "status": { "value": "todo" } } }], "minimum_should_match": 1 } } } })
    );
    assert_eq!(
        query["must_not"],
        json!([{ "match_phrase": { "type": "risk" } }])
    );
}

#[test]
fn terms_ordered_by_a_metric_carry_that_metric() {
    let all = dashboards();
    let s = searches(find(&all, "teleplot-investment"), &HashMap::new());
    let terms = &s["allocation"][0]["body"]["aggs"]["root"]["aggs"]["b0"];
    assert_eq!(terms["terms"]["order"], json!({ "o": "desc" }));
    assert_eq!(
        terms["aggs"]["o"],
        json!({ "sum": { "field": "story_points" } })
    );
}

fn native() -> Dashboard {
    let path = format!("{}/dashboards/delivery-overview.yaml", root());
    let mut all = teleplot::dashboards::load_file(path.as_ref()).unwrap();
    assert_eq!(all.len(), 1);
    all.remove(0)
}

#[test]
fn native_dashboard_requests_match_snapshot() {
    insta::assert_json_snapshot!(
        "native-delivery-overview",
        searches(&native(), &HashMap::new())
    );
}

#[test]
fn document_layers_fetch_raw_documents_newest_first() {
    let s = searches(&native(), &HashMap::new());
    let body = &s["cycle-scatter"][0]["body"];
    assert_eq!(body["size"], json!(3000));
    assert_eq!(body["sort"], json!([{ "merged_at": { "order": "desc" } }]));
    // Dates come back as epoch milliseconds; every plotted field must exist.
    assert_eq!(
        body["fields"][0],
        json!({ "field": "merged_at", "format": "epoch_millis" })
    );
    let filters = body["query"]["bool"]["filter"].as_array().unwrap();
    assert!(filters.contains(&json!({ "exists": { "field": "cycle_time_hours" } })));
    assert!(body.get("aggs").is_none());
}

#[test]
fn ordering_terms_by_a_formula_falls_back_to_document_count() {
    let mut d = native();
    let PanelContent::Chart(chart) = &mut d
        .panels
        .iter_mut()
        .find(|p| p.id == "teams")
        .unwrap()
        .content
    else {
        panic!()
    };
    if let teleplot::model::Operation::Terms { order, .. } = &mut chart.layers[0].columns[0].op {
        *order = teleplot::model::TermsOrder::Column {
            column: "cfr".into(),
            ascending: false,
        };
    }
    let s = searches(&d, &HashMap::new());
    let terms = &s["teams"][0]["body"]["aggs"]["root"]["aggs"]["b0"]["terms"];
    assert_eq!(terms["order"], json!({ "_count": "desc" }), "{terms}");
    let problems = teleplot::lint::check(&d, None);
    assert!(
        problems
            .iter()
            .any(|p| p.path.ends_with(".order") && p.message.contains("document count")),
        "{problems:?}"
    );
}
