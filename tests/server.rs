#![cfg(feature = "ssr")]

use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get(path: &str) -> (u16, String) {
    let app = teleplot::server::router("127.0.0.1:0".parse().unwrap());
    let response = app
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn home_renders_the_empty_shell() {
    let (status, body) = get("/").await;
    assert_eq!(status, 200);
    assert!(body.contains("<title>Teleplot</title>"), "{body}");
    assert!(body.contains("No dashboards loaded"), "{body}");
    assert!(
        body.contains("/pkg/teleplot.wasm"),
        "hydration scripts missing: {body}"
    );
}

#[tokio::test]
async fn unknown_paths_are_not_found() {
    let (status, _) = get("/pkg/does-not-exist.js").await;
    assert_eq!(status, 404);
}
