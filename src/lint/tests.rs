use std::collections::HashMap;

use serde_json::json;

use super::*;

fn root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

fn caps() -> HashMap<String, Vec<Field>> {
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
        (
            index.to_string(),
            crate::es::fields_from_caps(&serde_json::from_str(&text).unwrap()),
        )
    })
    .collect()
}

fn reference() -> serde_json::Value {
    let path = format!("{}/dashboards/delivery-overview.yaml", root());
    let d = crate::dashboards::load_file(path.as_ref())
        .unwrap()
        .remove(0);
    serde_json::to_value(d).unwrap()
}

fn problems(value: serde_json::Value) -> Vec<Problem> {
    let d = parse(value).unwrap_or_else(|p| panic!("{p:?}"));
    check(&d, Some(&caps()))
}

#[test]
fn the_reference_dashboard_is_clean() {
    assert_eq!(problems(reference()), []);
}

#[test]
fn kibana_fixtures_are_clean() {
    let caps = caps();
    for version in ["8.19", "9.5"] {
        let path = format!("{}/fixtures/kibana/{version}/delivery.ndjson", root());
        for d in crate::dashboards::load_file(path.as_ref()).unwrap() {
            let p = check(&d, Some(&caps));
            assert!(!has_errors(&p), "{version} {}: {p:?}", d.id);
        }
    }
}

#[test]
fn parse_errors_say_where() {
    let mut d = reference();
    d["panels"][1]["content"]["layers"][0]["columns"][0]["op"] = json!("medianish");
    let err = parse(d).unwrap_err();
    assert_eq!(err.path, "panels[1].content.layers[0].columns[0]");
    assert!(err.message.contains("medianish"), "{}", err.message);
}

/// Mutates the lead-time tile (panel 1, layer 0, column 0: median of change_lead_time_hours).
fn lead_time(mutate: impl FnOnce(&mut serde_json::Value)) -> Vec<Problem> {
    let mut d = reference();
    mutate(&mut d["panels"][1]["content"]["layers"][0]);
    problems(d)
}

fn only(problems: &[Problem]) -> &Problem {
    assert_eq!(problems.len(), 1, "{problems:?}");
    &problems[0]
}

#[test]
fn missing_fields_are_named_with_suggestions() {
    let p = lead_time(|l| l["columns"][0]["field"] = json!("change_lead_time_hour"));
    let p = only(&p);
    assert_eq!(p.path, "panels[1].content.layers[0].columns[0].field");
    assert!(
        p.message.contains("did you mean change_lead_time_hours"),
        "{}",
        p.message
    );
}

#[test]
fn field_types_must_suit_the_operation() {
    let p = lead_time(|l| l["columns"][0]["field"] = json!("team"));
    assert!(
        only(&p)
            .message
            .contains("team is keyword, but this needs a number"),
        "{p:?}"
    );

    let mut d = reference();
    d["panels"][5]["content"]["layers"][0]["columns"][0]["field"] = json!("team");
    let p = problems(d);
    assert!(only(&p).message.contains("needs a date"), "{p:?}");
}

#[test]
fn references_between_columns_must_resolve() {
    let mut d = reference();
    let formula = &mut d["panels"][2]["content"]["layers"][0]["columns"][2];
    formula["expr"]["args"][0] = json!("nope");
    let p = problems(d);
    assert!(
        only(&p).message.contains("refers to column \"nope\""),
        "{p:?}"
    );

    let p = lead_time(|l| l["encoding"]["value"] = json!("ghost"));
    assert_eq!(only(&p).path, "panels[1].content.layers[0].encoding");
}

#[test]
fn unknown_functions_list_the_known_ones() {
    let mut d = reference();
    d["panels"][2]["content"]["layers"][0]["columns"][2]["expr"]["call"] = json!("devide");
    let p = problems(d);
    assert!(
        only(&p)
            .message
            .contains("unknown function \"devide\"; available: add"),
        "{p:?}"
    );
}

#[test]
fn indexes_queries_and_layout_are_checked() {
    let p = lead_time(|l| l["data_view"]["index"] = json!("delivery-nope"));
    assert!(
        p.iter()
            .any(|p| p.message.contains("no index matches \"delivery-nope\"")),
        "{p:?}"
    );

    let mut d = reference();
    d["panels"][1]["content"]["query"]["text"] = json!("team : (a or");
    assert!(
        problems(d)
            .iter()
            .any(|p| p.path == "panels[1].content.query.text")
    );

    let mut d = reference();
    d["panels"][1]["grid"]["x"] = json!(40);
    assert!(
        only(&problems(d))
            .message
            .contains("the grid is 48 columns wide")
    );

    let mut d = reference();
    d["panels"][2]["id"] = json!("lead-time");
    assert!(only(&problems(d)).message.contains("used twice"));
}

#[test]
fn document_layers_only_take_values() {
    let mut d = reference();
    d["panels"][6]["content"]["layers"][0]["columns"][1] =
        json!({ "id": "hours", "op": "median", "field": "cycle_time_hours" });
    assert!(
        problems(d)
            .iter()
            .any(|p| p.message.contains("document layers can only have"))
    );
}

#[test]
fn offline_checks_skip_the_cluster() {
    let mut d = reference();
    d["panels"][1]["content"]["layers"][0]["columns"][0]["field"] = json!("whatever");
    let d = parse(d).unwrap();
    assert_eq!(check(&d, None), [], "field checks need field caps");
}

#[test]
fn only_metrics_are_normalized_by_unit() {
    let mut d = reference();
    let (p, c) = d["panels"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .find_map(|(p, panel)| {
            let columns = panel["content"]["layers"][0]["columns"].as_array()?;
            let c = columns.iter().position(|c| c["op"] == "date_histogram")?;
            Some((p, c))
        })
        .unwrap();
    let column = &mut d["panels"][p]["content"]["layers"][0]["columns"][c];
    column["per"] = json!("day");
    let found = problems(d.clone());
    let problem = only(&found);
    assert_eq!(
        problem.path,
        format!("panels[{p}].content.layers[0].columns[{c}].per")
    );

    d["panels"][p]["content"]["layers"][0]["columns"][c]
        .as_object_mut()
        .unwrap()
        .remove("per");
    d["panels"][p]["content"]["layers"][0]["columns"][c + 1]["per"] = json!("week");
    assert_eq!(problems(d), []);
}

#[test]
fn reference_lines_need_single_values_and_a_chart_to_cross() {
    // The cycle-time distribution has a reference-line layer of SLEs.
    let mut d = reference();
    let panel = panel_index(&d, "cycle-distribution");
    let layers = &mut d["panels"][panel]["content"]["layers"];
    assert_eq!(layers[1]["encoding"]["as"], json!("reference_lines"));
    layers[1]["columns"].as_array_mut().unwrap().insert(
        0,
        json!({ "id": "t", "op": "terms", "fields": ["team"], "size": 3 }),
    );
    let p = problems(d.clone());
    assert!(only(&p).message.contains("no bucket columns"), "{p:?}");

    let layers = &mut d["panels"][panel]["content"]["layers"];
    layers.as_array_mut().unwrap().remove(0);
    layers[0]["columns"].as_array_mut().unwrap().remove(0);
    let p = problems(d);
    assert!(only(&p).message.contains("need a data layer"), "{p:?}");
}

fn panel_index(d: &serde_json::Value, id: &str) -> usize {
    d["panels"]
        .as_array()
        .unwrap()
        .iter()
        .position(|p| p["id"] == id)
        .unwrap()
}

#[test]
fn heatmaps_need_bucket_axes_and_a_metric_value() {
    let mut d = reference();
    let panel = panel_index(&d, "cycle-heatmap");
    d["panels"][panel]["content"]["layers"][0]["encoding"]["value"] = json!("hours");
    let p = problems(d.clone());
    assert!(only(&p).message.contains("must be a metric"), "{p:?}");

    d["panels"][panel]["content"]["layers"][0]["encoding"] =
        json!({ "as": "heatmap", "x": "n", "value": "n" });
    let p = problems(d.clone());
    assert!(
        p.iter()
            .any(|p| p.message.contains("must be a bucket column")),
        "{p:?}"
    );

    d["panels"][panel]["content"]["kind"] = json!("xy");
    d["panels"][panel]["content"]["layers"][0]["encoding"] =
        json!({ "as": "heatmap", "x": "week", "y": "hours", "value": "n" });
    let p = problems(d);
    assert!(only(&p).message.contains("does not suit"), "{p:?}");
}

#[test]
fn log_histograms_need_a_positive_range_and_percentiles_a_percent() {
    let mut d = reference();
    let panel = panel_index(&d, "cycle-distribution");
    d["panels"][panel]["content"]["layers"][0]["columns"][0]["min"] = json!(0);
    let p = problems(d);
    assert_eq!(
        only(&p).path,
        format!("panels[{panel}].content.layers[0].columns[0]")
    );
    assert!(only(&p).message.contains("0 < min < max"), "{p:?}");

    let mut d = reference();
    let panel = panel_index(&d, "cycle-scatter");
    d["panels"][panel]["content"]["layers"][0]["encoding"]["percentiles"] = json!([85, 150]);
    let p = problems(d);
    assert!(only(&p).message.contains("150"), "{p:?}");
}
