//! End-to-end against real Elasticsearch: the fixture dashboards must produce
//! the numbers Kibana itself rendered (see fixtures/kibana/*/screenshots).
//!
//! Needs clusters seeded by `fixtures/make-fixtures.sh` (or the `seed`
//! example with `--end 2026-10-01`), named by version:
//!
//! ```text
//! TELEPLOT_TEST_ES="8.19=http://localhost:9208,9.5=http://localhost:9209" cargo test --test elasticsearch
//! ```
//!
//! Without `TELEPLOT_TEST_ES` the tests pass without running, so `cargo test`
//! works anywhere; CI sets it.

#![cfg(feature = "ssr")]

use std::collections::HashMap;

use jiff::tz::TimeZone;
use serde_json::json;
use teleplot::config::Elasticsearch;
use teleplot::engine::{Engine, Request};
use teleplot::es::{Client, Search};
use teleplot::model::{Cell, Dashboard, PanelData, TimeRange};

/// The range Kibana's reference screenshots were rendered over, in UTC.
fn kibana_range() -> Request {
    Request {
        time: Some(TimeRange {
            from: "2026-04-01T00:00:00.000Z".into(),
            to: "2026-10-02T00:00:00.000Z".into(),
        }),
        selections: Vec::new(),
    }
}

fn clusters() -> Vec<(String, Client)> {
    let Ok(spec) = std::env::var("TELEPLOT_TEST_ES") else {
        eprintln!("TELEPLOT_TEST_ES not set: skipping Elasticsearch tests");
        return Vec::new();
    };
    spec.split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|entry| {
            let (version, url) = entry
                .split_once('=')
                .expect("TELEPLOT_TEST_ES entries are version=url");
            let config = Elasticsearch {
                url: Some(url.trim().into()),
                ..Default::default()
            };
            (version.trim().to_string(), Client::new(&config).unwrap())
        })
        .collect()
}

fn dashboards(version: &str) -> Vec<Dashboard> {
    let path = format!(
        "{}/fixtures/kibana/{version}/delivery.ndjson",
        env!("CARGO_MANIFEST_DIR")
    );
    teleplot::dashboards::load_file(path.as_ref()).unwrap()
}

async fn run(version: &str, client: &Client, dashboard: &str) -> HashMap<String, PanelData> {
    let engine = Engine::with_client(client.clone(), TimeZone::UTC, "UTC".into());
    let all = dashboards(version);
    let d = all.iter().find(|d| d.id == dashboard).unwrap();
    engine
        .dashboard_data(d, &kibana_range(), jiff::Timestamp::now())
        .await
        .unwrap()
}

/// Rows of one layer as label → value for two columns.
fn column(data: &HashMap<String, PanelData>, panel: &str, id: &str) -> Vec<Cell> {
    let layer = &data[panel].layers[0];
    assert_eq!(layer.error, None, "{panel}");
    layer
        .series(id)
        .unwrap_or_else(|| panic!("{panel} has no column {id}"))
        .values
        .clone()
}

fn number(data: &HashMap<String, PanelData>, panel: &str, id: &str) -> f64 {
    column(data, panel, id)[0]
        .number()
        .unwrap_or_else(|| panic!("{panel}.{id} is empty"))
}

/// Kibana shows these rounded; compare at the precision it displayed.
fn assert_shown(actual: f64, shown: f64, decimals: i32, what: &str) {
    let scale = 10f64.powi(decimals);
    assert_eq!((actual * scale).round() / scale, shown, "{what}: {actual}");
}

#[tokio::test]
async fn dora_overview_matches_kibana() {
    for (version, client) in clusters() {
        let data = run(&version, &client, "teleplot-delivery-overview").await;
        assert_shown(
            number(&data, "deploys-per-week", "dpw"),
            38.2,
            1,
            "deployments/week",
        );
        assert_eq!(
            number(&data, "deploys-per-week", "dpw-prev"),
            0.0,
            "previous period"
        );
        assert_shown(
            number(&data, "lead-time", "lt-median"),
            20.6,
            1,
            "median lead time",
        );
        assert_shown(
            number(&data, "cfr", "cfr-f") * 100.0,
            10.2,
            1,
            "change fail rate %",
        );
        assert_shown(
            number(&data, "recovery", "rec-median"),
            9.3,
            1,
            "median recovery",
        );

        let teams = column(&data, "teams", "tt-team");
        let deploys = column(&data, "teams", "tt-n");
        let lead = column(&data, "teams", "tt-lead");
        let cfr = column(&data, "teams", "tt-cfr");
        let expected = [
            ("checkout", 395.0, 6.3, 4.3),
            ("infra", 189.0, 35.0, 12.7),
            ("billing", 139.0, 32.7, 7.2),
            ("identity", 119.0, 27.4, 9.2),
            ("onboarding", 76.0, 106.4, 22.4),
            ("data", 38.0, 451.1, 31.6),
            ("observability", 34.0, 128.7, 20.6),
            ("experiments", 14.0, 822.6, 28.6),
        ];
        assert_eq!(teams.len(), expected.len(), "{version}");
        for (i, (team, n, median, pct)) in expected.into_iter().enumerate() {
            assert_eq!(teams[i], Cell::Text(team.into()), "{version} row {i}");
            assert_eq!(deploys[i], Cell::Number(n), "{version} {team}");
            assert_shown(lead[i].number().unwrap(), median, 1, team);
            assert_shown(cfr[i].number().unwrap() * 100.0, pct, 1, team);
        }
    }
}

#[tokio::test]
async fn pr_flow_matches_kibana() {
    for (version, client) in clusters() {
        let data = run(&version, &client, "teleplot-pr-flow").await;
        assert_eq!(number(&data, "contributors", "ac"), 48.0, "{version}");
        assert_eq!(number(&data, "throughput", "tp"), 2724.0, "{version}");
        assert_eq!(
            column(&data, "last-pr", "lv")[0],
            Cell::Text("pr-001822".into()),
            "{version}"
        );

        let groups = column(&data, "ai-share", "ai-t");
        let counts = column(&data, "ai-share", "ai-n");
        let ai = groups
            .iter()
            .position(|g| *g == Cell::Text("true".into()))
            .expect("an ai_assisted=true slice");
        let total: f64 = counts.iter().filter_map(Cell::number).sum();
        assert_shown(
            counts[ai].number().unwrap() / total * 100.0,
            39.13,
            2,
            "AI-assisted share %",
        );

        // Cumulative sums end at the totals, and the legacy chart has both percentiles.
        let all = column(&data, "cumulative", "cu-sum");
        assert_eq!(all.last().unwrap().number(), Some(2724.0), "{version}");
        assert!(data["legacy-line"].layers[0].series("1.85").is_some());
        assert!(
            data.values()
                .all(|p| p.layers.iter().all(|l| l.error.is_none()))
        );
    }
}

#[tokio::test]
async fn investment_applies_dashboard_query_and_filters_like_kibana() {
    for (version, client) in clusters() {
        let data = run(&version, &client, "teleplot-investment").await;
        // NOT status:"todo" (query) and NOT type:risk (negated pill) both apply.
        assert_eq!(number(&data, "legacy-metric", "1"), 1065.0, "{version}");
        let types = column(&data, "legacy-pie", "2");
        assert!(
            !types.contains(&Cell::Text("risk".into())),
            "{version}: {types:?}"
        );

        let teams = column(&data, "legacy-table", "2");
        let items = column(&data, "legacy-table", "1");
        let efficiency = column(&data, "legacy-table", "3");
        let flow = column(&data, "legacy-table", "4");
        let expected = [
            ("checkout", 188.0, 0.387, 2.725),
            ("identity", 164.0, 0.396, 5.11),
            ("data", 156.0, 0.4, 16.655),
            ("onboarding", 144.0, 0.394, 9.805),
            ("billing", 130.0, 0.4, 4.675),
            ("infra", 117.0, 0.416, 5.47),
            ("observability", 96.0, 0.398, 9.11),
        ];
        for (i, (team, n, eff, days)) in expected.into_iter().enumerate() {
            assert_eq!(teams[i], Cell::Text(team.into()), "{version} row {i}");
            assert_eq!(items[i], Cell::Number(n), "{version} {team}");
            assert_shown(efficiency[i].number().unwrap(), eff, 3, team);
            assert_shown(flow[i].number().unwrap(), days, 3, team);
        }
    }
}

/// Elasticsearch 9.1+ runs KQL natively; Teleplot's translation must select
/// exactly the same documents. Only the syntax both share is compared:
/// Elasticsearch's own KQL (as of 9.5) has no value lists (`team:(a or b)`),
/// which Kibana, and so Teleplot, supports; those are covered by unit tests.
#[tokio::test]
async fn kql_translation_agrees_with_native_kql() {
    for (version, client) in clusters() {
        let caps = client.field_caps("delivery-*").await.unwrap();
        let queries = [
            "result : failure",
            "result : \"failure\"",
            "is_rework : true",
            "team : check*",
            "(team : checkout OR team : billing) AND NOT result : failure",
            "change_lead_time_hours >= 24 AND change_lead_time_hours < 100",
            "caused_by_deployment_id : *",
            "NOT status : \"todo\"",
            "ai_assisted : true AND lines_changed > 500",
            "group : payments OR severity : sev1",
            "repo : *api*",
            "deployed_at >= 2026-09-01",
            "deployed_at : 2026-09-01",
            "nonexistent_field : x",
            "",
        ];
        let mut searches = Vec::new();
        for q in queries {
            let ours = teleplot::kql::translate(
                q,
                &caps,
                &teleplot::kql::Options {
                    time_zone: Some("UTC".into()),
                },
            )
            .unwrap();
            for query in [ours, json!({ "kql": { "query": q } })] {
                searches.push(Search {
                    index: "delivery-*".into(),
                    body: json!({ "size": 0, "track_total_hits": true, "query": query }),
                });
            }
        }
        let responses = client.msearch(&searches).await.unwrap();
        if responses[1].is_err() {
            eprintln!("{version}: no native kql query (before 9.1), skipping comparison");
            continue;
        }
        for (i, q) in queries.iter().enumerate() {
            let hits = |r: &Result<serde_json::Value, String>| {
                r.as_ref().unwrap_or_else(|e| panic!("{q:?}: {e}"))["hits"]["total"]["value"]
                    .as_u64()
            };
            assert_eq!(
                hits(&responses[2 * i]),
                hits(&responses[2 * i + 1]),
                "{version}: {q:?}"
            );
        }
    }
}
