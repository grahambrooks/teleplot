//! Dev-only: times a 12-panel dashboard against a large dataset.
//!
//! ```text
//! cargo run --release --example seed -- --es http://localhost:9208 --end 2026-10-01 --scale 185 --prefix perf-
//! cargo run --release --example bench -- --es http://localhost:9208 --prefix perf-
//! ```
//!
//! The dashboard is `dashboards/delivery-overview.yaml` (10 panels) plus
//! copies of two of its charts, over the `perf-` indexes, for the 90 days to
//! 2026-10-02. Three cases, each timed several times:
//!
//! - cold: Elasticsearch caches cleared and a fresh Teleplot engine
//! - warm: Elasticsearch has seen the queries; a fresh Teleplot engine
//! - cached: the same Teleplot engine again (its response cache answers)
//!
//! `--write <file.yaml>` saves the dashboard instead, for timing in a browser.

use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use teleplot::config::Elasticsearch;
use teleplot::engine::{Engine, Request};
use teleplot::es::Client;
use teleplot::model::{Dashboard, PanelContent, TimeRange};

fn dashboard(prefix: &str) -> Dashboard {
    let path = format!(
        "{}/dashboards/delivery-overview.yaml",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut d = teleplot::dashboards::load_file(path.as_ref())
        .unwrap()
        .remove(0);
    for c in &mut d.controls {
        c.data_view.index = format!("{prefix}{}", c.data_view.index);
    }
    for v in d
        .annotations
        .iter_mut()
        .filter_map(|a| a.data_view.as_mut())
    {
        v.index = format!("{prefix}{}", v.index);
    }
    for p in &mut d.panels {
        if let PanelContent::Chart(chart) = &mut p.content {
            for l in &mut chart.layers {
                l.data_view.index = format!("{prefix}{}", l.data_view.index);
                // ES|QL names its index in the query.
                if let Some(esql) = &mut l.esql {
                    esql.query = esql
                        .query
                        .replace("FROM delivery-", &format!("FROM {prefix}delivery-"));
                }
            }
        }
    }
    for id in ["throughput", "investment"] {
        let mut copy = d.panels.iter().find(|p| p.id == id).unwrap().clone();
        copy.id = format!("{id}-copy");
        copy.grid.y += 60;
        d.panels.push(copy);
    }
    d
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn summary(name: &str, mut times: Vec<f64>) {
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    let max = times[times.len() - 1];
    println!(
        "{name:8} median {median:7.1} ms   max {max:7.1} ms   ({} runs)",
        times.len()
    );
}

#[tokio::main]
async fn main() {
    let mut es = "http://localhost:9208".to_string();
    let mut prefix = "perf-".to_string();
    let mut runs = 5;
    let mut write = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--es" => es = value,
            "--prefix" => prefix = value,
            "--runs" => runs = value.parse().expect("--runs: integer"),
            "--write" => write = Some(value),
            other => panic!("unknown flag {other}"),
        }
    }

    let mut d = dashboard(&prefix);
    if let Some(path) = write {
        // For timing in a browser: `teleplot serve --dashboards <dir of path>`.
        d.id = "perf".into();
        d.title = format!("{} ({prefix} indexes)", d.title);
        std::fs::write(&path, serde_yaml_ng::to_string(&d).unwrap()).unwrap();
        println!("wrote {path}");
        return;
    }
    let charts = d
        .panels
        .iter()
        .filter(|p| matches!(p.content, PanelContent::Chart(_)))
        .count();
    let client = Client::new(&Elasticsearch {
        url: Some(es.clone()),
        ..Default::default()
    })
    .unwrap();
    let request = Request {
        time: Some(TimeRange {
            from: "2026-07-04T00:00:00Z".into(),
            to: "2026-10-02T00:00:00Z".into(),
        }),
        ..Default::default()
    };
    let http = reqwest::Client::new();
    let docs: u64 = http
        .get(format!("{es}/_cat/count/{prefix}delivery-*?format=json"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()[0]["count"]
        .as_str()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    println!(
        "{} panels ({charts} charts) over {docs} documents, 90 days\n",
        d.panels.len()
    );

    let fresh = || Engine::with_client(client.clone(), TimeZone::UTC, "UTC".into());
    let now = jiff::Timestamp::now();

    let mut cold = Vec::new();
    for _ in 0..runs {
        http.post(format!("{es}/{prefix}delivery-*/_cache/clear"))
            .send()
            .await
            .unwrap();
        let engine = fresh();
        let t = Instant::now();
        let data = engine.dashboard_data(&d, &request, now).await.unwrap();
        cold.push(ms(t.elapsed()));
        assert_eq!(data.len(), charts);
        let errors: Vec<_> = data
            .values()
            .flat_map(|p| p.layers.iter().filter_map(|l| l.error.clone()))
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
    }

    let mut warm = Vec::new();
    for _ in 0..runs {
        let engine = fresh();
        let t = Instant::now();
        engine.dashboard_data(&d, &request, now).await.unwrap();
        warm.push(ms(t.elapsed()));
    }

    let engine = fresh();
    engine.dashboard_data(&d, &request, now).await.unwrap();
    let mut cached = Vec::new();
    for _ in 0..runs {
        let t = Instant::now();
        engine.dashboard_data(&d, &request, now).await.unwrap();
        cached.push(ms(t.elapsed()));
    }

    summary("cold", cold);
    summary("warm", warm);
    summary("cached", cached);
}
