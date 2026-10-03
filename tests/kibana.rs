//! Kibana export → model conversion, against real exports in `fixtures/`.

#![cfg(feature = "ssr")]

use teleplot::kibana;
use teleplot::model::{
    ChartKind, Dashboard, Encoding, Expr, Operation, Panel, PanelContent, PlaceholderReason,
    QueryLanguage,
};

fn read(path: &str) -> Vec<Dashboard> {
    let text = std::fs::read_to_string(format!(
        "{}/fixtures/kibana/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("{path}: {e}"));
    kibana::read(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn dashboard<'a>(dashboards: &'a [Dashboard], id: &str) -> &'a Dashboard {
    dashboards
        .iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("no dashboard {id}"))
}

fn panel<'a>(d: &'a Dashboard, id: &str) -> &'a Panel {
    d.panels
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("no panel {id} in {}", d.id))
}

fn chart(p: &Panel) -> &teleplot::model::Chart {
    match &p.content {
        PanelContent::Chart(c) => c,
        other => panic!("panel {} is not a chart: {other:?}", p.id),
    }
}

fn placeholder_reasons(d: &Dashboard) -> Vec<PlaceholderReason> {
    d.panels
        .iter()
        .filter_map(|p| match &p.content {
            PanelContent::Placeholder(ph) => Some(ph.reason),
            _ => None,
        })
        .collect()
}

fn yaml(dashboards: &[Dashboard]) -> String {
    dashboards
        .iter()
        .map(|d| format!("---\n{}", serde_yaml_ng::to_string(d).unwrap()))
        .collect()
}

const EXPORTS: [&str; 4] = [
    "8.19/delivery.ndjson",
    "8.19/sample-data.ndjson",
    "9.5/delivery.ndjson",
    "9.5/sample-data.ndjson",
];

#[test]
fn every_supported_panel_in_the_real_exports_converts() {
    for path in EXPORTS {
        for d in read(path) {
            for p in &d.panels {
                if let PanelContent::Placeholder(ph) = &p.content {
                    assert_eq!(
                        ph.reason,
                        PlaceholderReason::OutOfScope,
                        "{path}: {} / {}: {ph:?}",
                        d.title,
                        p.id
                    );
                }
            }
        }
    }
}

#[test]
fn delivery_dashboards_render_completely_on_both_versions() {
    for version in ["8.19", "9.5"] {
        let dashboards = read(&format!("{version}/delivery.ndjson"));
        assert_eq!(dashboards.len(), 3);
        for d in &dashboards {
            assert!(
                placeholder_reasons(d).is_empty(),
                "{version} {}: {:?}",
                d.title,
                placeholder_reasons(d)
            );
        }
    }
}

#[test]
fn conversion_does_not_depend_on_the_kibana_version() {
    let mut v8 = read("8.19/delivery.ndjson");
    let mut v9 = read("9.5/delivery.ndjson");
    v8.sort_by(|a, b| a.id.cmp(&b.id));
    v9.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(v8, v9);
}

#[test]
fn native_yaml_round_trips() {
    for path in EXPORTS.iter().chain(&["edge-cases/edge-cases.ndjson"]) {
        for d in read(path) {
            let text = serde_yaml_ng::to_string(&d).unwrap();
            let back: Dashboard = serde_yaml_ng::from_str(&text)
                .unwrap_or_else(|e| panic!("{path} {}: {e}\n{text}", d.id));
            assert_eq!(back, d, "{path} {}", d.id);
        }
    }
}

#[test]
fn formula_columns_keep_their_expression_and_hide_their_parts() {
    let dashboards = read("8.19/delivery.ndjson");
    let cfr = chart(panel(
        dashboard(&dashboards, "teleplot-delivery-overview"),
        "cfr",
    ));
    let layer = &cfr.layers[0];
    let formula = layer.column("cfr-f").unwrap();
    let Operation::Formula { text, expr } = &formula.op else {
        panic!("{formula:?}")
    };
    assert_eq!(text, "count(kql='result : \"failure\"') / count()");
    assert_eq!(
        expr,
        &Expr::Call {
            call: "divide".into(),
            args: vec![
                Expr::Column("cfr-fX0".into()),
                Expr::Column("cfr-fX1".into())
            ]
        }
    );
    let failures = layer.column("cfr-fX0").unwrap();
    assert!(failures.helper && !formula.helper);
    assert_eq!(
        failures.filter.as_ref().unwrap().text,
        "result : \"failure\""
    );
}

#[test]
fn by_reference_panels_resolve_to_their_saved_objects() {
    let dashboards = read("8.19/delivery.ndjson");
    let flow = dashboard(&dashboards, "teleplot-pr-flow");

    let lens = panel(flow, "lens-ref");
    assert_eq!(
        lens.title.as_deref(),
        Some("Review load by team (by reference)")
    );
    assert_eq!(chart(lens).layers[0].data_view.index, "delivery-prs");

    // A legacy percentiles aggregation becomes one column per percent.
    let legacy = chart(panel(flow, "legacy-line"));
    let Encoding::Xy { y, x, .. } = &legacy.layers[0].encoding else {
        panic!()
    };
    assert_eq!(y, &["1.50", "1.85"]);
    assert_eq!(x.as_deref(), Some("2"));
}

#[test]
fn dashboard_settings_come_through() {
    let dashboards = read("8.19/delivery.ndjson");
    let investment = dashboard(&dashboards, "teleplot-investment");
    assert_eq!(investment.time.as_ref().unwrap().from, "now-6M/M");
    assert_eq!(investment.query.text, "NOT status : \"todo\"");
    assert_eq!(investment.filters.len(), 1);
    assert!(investment.filters[0].negate);
    assert_eq!(investment.controls.len(), 1);
    assert_eq!(investment.controls[0].field, "team");
    assert_eq!(investment.controls[0].data_view.index, "delivery-workitems");

    let pie = chart(panel(investment, "legacy-pie"));
    assert_eq!(pie.kind, ChartKind::Donut);
}

#[test]
fn sample_dashboard_controls_resolve_through_references() {
    let dashboards = read("9.5/sample-data.ndjson");
    let ecommerce = dashboard(&dashboards, "722b74f0-b882-11e8-a6d9-e546fe2bba5f");
    let fields: Vec<&str> = ecommerce
        .controls
        .iter()
        .map(|c| c.field.as_str())
        .collect();
    assert_eq!(
        fields,
        ["manufacturer.keyword", "category.keyword"],
        "range slider skipped"
    );
}

#[test]
fn edge_cases_degrade_as_intended() {
    let dashboards = read("edge-cases/edge-cases.ndjson");
    let delivery = read("8.19/delivery.ndjson");

    // Pre-8.6 `indexpattern` Lens state converts exactly like `formBased`.
    let legacy = &dashboard(&dashboards, "edge-legacy-indexpattern").panels[0];
    let modern = panel(
        dashboard(&delivery, "teleplot-delivery-overview"),
        "lead-time",
    );
    assert_eq!(legacy.content, modern.content);

    assert_eq!(
        placeholder_reasons(dashboard(&dashboards, "edge-broken-references")),
        [
            PlaceholderReason::BrokenReference,
            PlaceholderReason::BrokenReference
        ]
    );
    assert_eq!(
        placeholder_reasons(dashboard(&dashboards, "edge-unsupported-panels")),
        [PlaceholderReason::OutOfScope, PlaceholderReason::OutOfScope]
    );

    let timed = &dashboard(&dashboards, "edge-panel-time-range").panels[0];
    assert_eq!(timed.time.as_ref().unwrap().from, "now-7d");

    let lucene = dashboard(&dashboards, "edge-lucene-and-filters");
    assert_eq!(lucene.query.language, QueryLanguage::Lucene);
    let flags: Vec<(bool, bool)> = lucene
        .filters
        .iter()
        .map(|f| (f.negate, f.disabled))
        .collect();
    assert_eq!(
        flags,
        [(true, false), (false, true), (false, false), (false, false)]
    );
    assert_eq!(lucene.filters[2].label.as_deref(), Some("custom DSL"));
}

#[test]
fn converted_dashboards_match_snapshots() {
    for path in EXPORTS.iter().chain(&["edge-cases/edge-cases.ndjson"]) {
        let name = path.replace(['/', '.'], "_");
        insta::assert_snapshot!(name, yaml(&read(path)));
    }
}
