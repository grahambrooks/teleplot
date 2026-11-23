//! One trace per routed request (the API and the pages), reported to habit.
//!
//! The root is named for the route *template* (`POST /api/dashboards/{id}/data`),
//! never the raw path or query string. Dashboard, panel and control ids are
//! recorded only when the dashboard defines them: they are configuration,
//! but a hand-typed URL could put anything in the path. The static assets
//! (the fallback) and the MCP endpoint are not traced.

use axum::extract::{MatchedPath, RawPathParams, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use super::AppState;
use crate::spans;
use crate::telemetry;

pub async fn traced(
    State(state): State<AppState>,
    params: Result<RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    request: Request,
    next: Next,
) -> Response {
    let Some(route) = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .filter(|r| !r.starts_with("/mcp"))
    else {
        return next.run(request).await;
    };
    let method = request.method().as_str().to_string();
    let mut root = telemetry::trace(
        "teleplot",
        env!("CARGO_PKG_VERSION"),
        &format!("{method} {route}"),
    );
    if !root.enabled() {
        return next.run(request).await;
    }
    root.set_type("request")
        .attr("http.request.method", method.as_str())
        .attr("http.route", route.as_str());
    if let Ok(params) = &params {
        let get = |name: &str| params.iter().find(|(k, _)| *k == name).map(|(_, v)| v);
        if let Some(d) = get("id").and_then(|id| state.app.catalog.get(id)) {
            root.attr("dashboard", d.id.as_str());
            if let Some(p) = get("panel").filter(|p| d.panels.iter().any(|x| x.id == *p)) {
                root.attr("panel", p);
            }
            if let Some(c) = get("control").filter(|c| d.controls.iter().any(|x| x.id == *c)) {
                root.attr("control", c);
            }
        }
    }
    let response = spans::within(&root, next.run(request)).await;
    // A client error (an unknown dashboard) is not the server failing.
    let code = response.status().as_u16();
    root.attr("http.response.status_code", i64::from(code));
    if code >= 500 {
        root.fail(&format!("http_{code}"));
    }
    response
}
