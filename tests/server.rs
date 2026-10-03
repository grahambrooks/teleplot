#![cfg(feature = "ssr")]

use std::sync::Arc;

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use teleplot::model::api::Catalog;
use teleplot::server::Teleplot;
use tower::ServiceExt;

/// A server over the 8.19 delivery fixtures, with no Elasticsearch configured.
fn app() -> axum::Router {
    let dir = format!("{}/fixtures/kibana/8.19", env!("CARGO_MANIFEST_DIR"));
    let teleplot = Teleplot::new(Err("no Elasticsearch configured".into()), Some(dir.into()));
    teleplot::server::router("127.0.0.1:0".parse().unwrap(), Arc::new(teleplot))
}

async fn send(request: Request<Body>) -> (u16, String) {
    let response = app().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn get(path: &str) -> (u16, String) {
    send(Request::get(path).body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn pages_render_the_shell() {
    let (status, body) = get("/").await;
    assert_eq!(status, 200);
    assert!(body.contains("<title>Teleplot</title>"), "{body}");
    assert!(
        body.contains("/pkg/teleplot.wasm"),
        "hydration scripts missing: {body}"
    );
    let (status, _) = get("/d/teleplot-pr-flow").await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn lists_dashboards_and_reports_the_missing_cluster() {
    let (status, body) = get("/api/dashboards").await;
    assert_eq!(status, 200);
    let catalog: Catalog = serde_json::from_str(&body).unwrap();
    let titles: Vec<&str> = catalog
        .dashboards
        .iter()
        .map(|d| d.title.as_str())
        .collect();
    assert!(titles.contains(&"[Delivery] PR flow"), "{titles:?}");
    assert!(
        titles.contains(&"[eCommerce] Revenue Dashboard"),
        "sample data loads too: {titles:?}"
    );
    assert_eq!(
        catalog.problem.as_deref(),
        Some("no Elasticsearch configured")
    );
}

#[tokio::test]
async fn serves_dashboard_models() {
    let (status, body) = get("/api/dashboards/teleplot-investment").await;
    assert_eq!(status, 200);
    let d: teleplot::model::Dashboard = serde_json::from_str(&body).unwrap();
    assert_eq!(d.controls[0].field, "team");
    assert_eq!(get("/api/dashboards/nope").await.0, 404);
}

#[tokio::test]
async fn data_requests_explain_why_they_cannot_run() {
    let request = Request::post("/api/dashboards/teleplot-pr-flow/data")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let (status, body) = send(request).await;
    assert_eq!(status, 503);
    assert!(body.contains("no Elasticsearch configured"), "{body}");
}

#[tokio::test]
async fn unknown_paths_are_not_found() {
    let (status, _) = get("/pkg/does-not-exist.js").await;
    assert_eq!(status, 404);
}
