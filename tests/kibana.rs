//! Kibana export → model conversion, against real exports in `fixtures/`.

#![cfg(feature = "ssr")]

use teleplot::kibana;
use teleplot::model::{
    ChartKind, Dashboard, Encoding, Expr, LineStyle, Operation, Panel, PanelContent,
    PlaceholderReason, QueryLanguage, ReferenceAxis, ReferenceFill,
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
fn reference_line_layers_convert_with_their_styles() {
    for version in ["8.19", "9.5"] {
        let dashboards = read(&format!("{version}/delivery.ndjson"));
        let target = chart(panel(
            dashboard(&dashboards, "teleplot-delivery-overview"),
            "lead-target",
        ));
        assert_eq!(target.layers.len(), 2, "{version}");
        let refs = &target.layers[1];
        let Encoding::ReferenceLines { lines } = &refs.encoding else {
            panic!("{version}: {:?}", refs.encoding)
        };
        let styles: Vec<_> = lines
            .iter()
            .map(|l| (l.column.as_str(), l.axis, l.style, l.fill, l.show_label))
            .collect();
        assert_eq!(
            styles,
            [
                (
                    "lg-target",
                    ReferenceAxis::Left,
                    LineStyle::Dashed,
                    ReferenceFill::Below,
                    true
                ),
                (
                    "lg-avg",
                    ReferenceAxis::Left,
                    LineStyle::Dotted,
                    ReferenceFill::None,
                    true
                ),
            ],
            "{version}"
        );
        assert_eq!(lines[0].color.as_deref(), Some("#54B399"));
        assert_eq!(
            refs.column("lg-target").unwrap().op,
            Operation::StaticValue { value: 24.0 }
        );
        assert!(target.warnings.is_empty(), "{version}");
    }
}

#[test]
fn lens_annotation_layers_convert() {
    for version in ["8.19", "9.5"] {
        let dashboards = read(&format!("{version}/sample-data.ndjson"));
        let annotated: Vec<(&str, &teleplot::model::Annotation)> = dashboards
            .iter()
            .flat_map(|d| &d.panels)
            .filter_map(|p| match &p.content {
                PanelContent::Chart(c) if !c.annotations.is_empty() => {
                    assert!(c.warnings.is_empty(), "{:?}", c.warnings);
                    assert!(c.has_time_axis(), "{:?}", p.title);
                    Some((p.title.as_deref().unwrap_or_default(), &c.annotations[0]))
                }
                _ => None,
            })
            .collect();
        let titles: Vec<&str> = annotated.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            titles,
            [
                "[eCommerce] Promotion Tracking",
                "[Flights] Delays & Cancellations",
                "[Logs] Response Codes Over Time + Annotations"
            ],
            "{version}"
        );
        // Query annotations over their layer's data view, Lucene kept as Lucene.
        let (_, ecommerce) = annotated[0];
        assert_eq!(
            ecommerce.data_view.as_ref().map(|v| v.index.as_str()),
            Some("kibana_sample_data_ecommerce")
        );
        assert_eq!(ecommerce.query.text, "taxful_total_price:>250");
        assert_eq!(
            ecommerce.query.language,
            teleplot::model::QueryLanguage::Lucene
        );
        assert!(ecommerce.ignore_global_filters);
    }
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

#[test]
fn lens_esql_panels_convert_to_esql_layers() {
    let dashboards = read("9.5/sample-data.ndjson");
    let logs = dashboards
        .iter()
        .find(|d| d.title == "[Logs] Web Traffic")
        .unwrap();
    let esql: Vec<&teleplot::model::Layer> = logs
        .panels
        .iter()
        .filter_map(|p| match &p.content {
            PanelContent::Chart(c) => c.layers.iter().find(|l| l.esql.is_some()),
            _ => None,
        })
        .collect();
    assert_eq!(esql.len(), 7);
    for layer in esql {
        // The ad hoc data view gives the index and time field.
        assert_eq!(layer.data_view.index, "kibana_sample_data_logs");
        assert_eq!(layer.data_view.time_field.as_deref(), Some("@timestamp"));
        assert!(
            layer
                .columns
                .iter()
                .all(|c| matches!(c.op, teleplot::model::Operation::Value { .. }))
        );
    }
}
