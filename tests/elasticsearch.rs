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
use teleplot::model::{Cell, Dashboard, PanelContent, PanelData, TimeRange};

/// The range Kibana's reference screenshots were rendered over, in UTC.
fn kibana_range() -> Request {
    Request {
        time: Some(TimeRange {
            from: "2026-04-01T00:00:00.000Z".into(),
            to: "2026-10-02T00:00:00.000Z".into(),
        }),
        ..Default::default()
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

        // Normalized by unit: each calendar month divided by its own length,
        // and October by the one day of it inside the range.
        let per_day = column(&data, "per-day-monthly", "pd-n");
        let shown = [6.133, 5.71, 6.467, 5.71, 5.645, 6.467, 9.0];
        assert_eq!(per_day.len(), shown.len(), "{version}");
        for (i, (cell, shown)) in per_day.iter().zip(shown).enumerate() {
            assert_shown(cell.number().unwrap(), shown, 3, &format!("month {i}"));
        }
        // Without a date histogram: the whole range (184 days).
        assert_shown(number(&data, "per-day", "pt-n"), 6.033, 3, "per day");

        // Reference lines come from their own layer: a static target, and a
        // quick function over the whole range, which Kibana draws just under
        // the 100 h gridline (Elasticsearch's own avg: 95.71).
        let line = |panel: &str, id: &str| {
            let layer = &data[panel].layers[1];
            assert_eq!(layer.error, None, "{version} {panel}");
            layer.series(id).unwrap().values[0].number().unwrap()
        };
        assert_eq!(line("lead-target", "lg-target"), 24.0, "{version}");
        assert_shown(line("lead-target", "lg-avg"), 95.7, 1, "average lead time");
        assert_eq!(line("team-target", "tg-target"), 24.0, "{version}");
        assert!(data["lead-target"].warnings.is_empty(), "{version}");

        // The Lens heatmap: deployments per month (April–September, and the
        // one day of October) by team, rows top-down as Kibana draws them.
        let dashboards = dashboards(&version);
        let overview = dashboards
            .iter()
            .find(|d| d.id == "teleplot-delivery-overview")
            .unwrap();
        let panel = overview
            .panels
            .iter()
            .find(|p| p.id == "team-heatmap")
            .unwrap();
        let PanelContent::Chart(chart) = &panel.content else {
            panic!(
                "{version}: the heatmap did not convert: {:?}",
                panel.content
            )
        };
        let hm = teleplot::ui::series::heatmap(chart, &data["team-heatmap"]);
        let shown: [(&str, [f64; 7]); 8] = [
            ("checkout", [66.0, 63.0, 77.0, 64.0, 76.0, 62.0, 5.0]),
            ("infra", [38.0, 33.0, 46.0, 38.0, 25.0, 34.0, 1.0]),
            ("billing", [33.0, 19.0, 22.0, 27.0, 26.0, 22.0, 0.0]),
            ("identity", [18.0, 24.0, 17.0, 24.0, 20.0, 26.0, 1.0]),
            ("onboarding", [12.0, 16.0, 14.0, 11.0, 14.0, 25.0, 2.0]),
            ("data", [5.0, 9.0, 7.0, 5.0, 8.0, 15.0, 0.0]),
            ("observability", [6.0, 10.0, 10.0, 6.0, 3.0, 7.0, 0.0]),
            ("experiments", [6.0, 3.0, 1.0, 2.0, 3.0, 3.0, 0.0]),
        ];
        let top_down: Vec<&str> = hm.y.iter().rev().map(String::as_str).collect();
        let teams: Vec<&str> = shown.iter().map(|(t, _)| *t).collect();
        assert_eq!(top_down, teams, "{version}");
        assert_eq!(hm.cells.len(), 7, "{version}: April to October");
        for (row, (team, months)) in shown.iter().enumerate() {
            let j = hm.y.len() - 1 - row;
            for (month, n) in months.iter().enumerate() {
                assert_eq!(
                    hm.cells[month][j],
                    Some(*n),
                    "{version} {team} month {month}"
                );
            }
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

async fn run_native(
    client: &Client,
    selections: Vec<(String, Vec<String>)>,
) -> HashMap<String, PanelData> {
    let engine = Engine::with_client(client.clone(), TimeZone::UTC, "UTC".into());
    let path = format!(
        "{}/dashboards/delivery-overview.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let d = teleplot::dashboards::load_file(path.as_ref())
        .unwrap()
        .remove(0);
    let request = Request {
        selections,
        ..kibana_range()
    };
    engine
        .dashboard_data(&d, &request, jiff::Timestamp::now())
        .await
        .unwrap()
}

#[tokio::test]
async fn native_delivery_overview_works_end_to_end() {
    for (version, client) in clusters() {
        let data = run_native(&client, Vec::new()).await;
        for (panel, d) in &data {
            for layer in &d.layers {
                assert_eq!(layer.error, None, "{version} {panel}");
            }
        }
        // The same DORA figures Kibana shows for its own dashboard.
        assert_shown(
            number(&data, "deploy-frequency", "per-week"),
            38.2,
            1,
            "deployments/week",
        );
        assert_shown(
            number(&data, "lead-time", "median"),
            20.6,
            1,
            "median lead time",
        );
        assert_shown(
            number(&data, "change-fail-rate", "rate") * 100.0,
            10.2,
            1,
            "change fail rate",
        );
        assert_shown(number(&data, "recovery", "median"), 9.3, 1, "recovery");
        assert_eq!(
            data["lead-time"].layers[0].counts,
            [1004.0],
            "{version}: sample size"
        );

        // Scatter: individual PRs, oldest first, with a trend for each.
        let merged = column(&data, "cycle-scatter", "merged");
        let trend = column(&data, "cycle-scatter", "trend");
        assert!(
            merged.len() > 1000 && merged.len() <= 3000,
            "{version}: {}",
            merged.len()
        );
        let times: Vec<f64> = merged.iter().filter_map(Cell::number).collect();
        assert!(
            times.windows(2).all(|w| w[0] <= w[1]),
            "{version}: scatter not in time order"
        );
        assert!(trend.iter().all(|c| c.number().is_some()));

        // Band: p25 ≤ median ≤ p75 wherever the week has data.
        let band = &data["lead-time-band"].layers[0];
        let (lo, mid, hi) = (
            &band.series("p25").unwrap().values,
            &band.series("p50").unwrap().values,
            &band.series("p75").unwrap().values,
        );
        for i in 0..band.rows {
            if let (Some(a), Some(b), Some(c)) = (lo[i].number(), mid[i].number(), hi[i].number()) {
                assert!(a <= b && b <= c, "{version} week {i}: {a} {b} {c}");
            }
        }

        // Selecting a team (what clicking its row does) narrows every panel.
        let focused = run_native(&client, vec![("team".into(), vec!["data".into()])]).await;
        assert_eq!(
            column(&focused, "teams", "team"),
            [Cell::Text("data".into())],
            "{version}"
        );
        assert_eq!(number(&focused, "teams", "n"), 38.0, "{version}");
    }
}
