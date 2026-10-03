//! JSON API for the browser client.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::AppState;
use crate::engine::Request;
use crate::model::api::{
    ApiError, Catalog, ControlValues, DashboardSummary, DataRequest, DataResponse,
};
use crate::model::{Dashboard, TimeRange};

/// `now`, rounded down to 30 seconds. Ranges ending "now" then produce the
/// same searches for half a minute, so Elasticsearch's request cache and
/// Teleplot's own response cache can answer repeat views and refreshes.
fn cache_friendly_now() -> jiff::Timestamp {
    let ms = jiff::Timestamp::now().as_millisecond();
    jiff::Timestamp::from_millisecond(ms - ms.rem_euclid(30_000))
        .unwrap_or_else(|_| jiff::Timestamp::now())
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
        .into_response()
}

pub async fn list(State(state): State<AppState>) -> Json<Catalog> {
    let dashboards = state.app.catalog.dashboards();
    let mut summaries: Vec<DashboardSummary> = dashboards
        .iter()
        .map(|d| DashboardSummary {
            id: d.id.clone(),
            title: d.title.clone(),
            description: d.description.clone(),
            panels: d.panels.len(),
        })
        .collect();
    summaries.sort_by_key(|s| s.title.to_lowercase());
    Json(Catalog {
        dashboards: summaries,
        errors: state.app.catalog.errors(),
        problem: state.app.engine.as_ref().err().cloned(),
    })
}

pub async fn dashboard(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.app.catalog.get(&id) {
        Some(d) => Json(d).into_response(),
        None => error(StatusCode::NOT_FOUND, format!("no dashboard {id}")),
    }
}

/// Translates the client's request (control ids) into the engine's (fields).
fn engine_request(d: &Dashboard, body: DataRequest) -> Request {
    let time = match (body.from, body.to) {
        (Some(from), Some(to)) => Some(TimeRange { from, to }),
        _ => None,
    };
    let selections = d
        .controls
        .iter()
        .filter_map(|c| {
            let values = body.selections.get(&c.id)?.clone();
            (!values.is_empty()).then(|| (c.field.clone(), values))
        })
        .collect();
    Request {
        time,
        query: body.query.filter(|q| !q.is_empty()),
        selections,
    }
}

pub async fn data(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<DataRequest>,
) -> Response {
    let Some(d) = state.app.catalog.get(&id) else {
        return error(StatusCode::NOT_FOUND, format!("no dashboard {id}"));
    };
    let engine = match &state.app.engine {
        Ok(e) => e,
        Err(problem) => return error(StatusCode::SERVICE_UNAVAILABLE, problem.clone()),
    };
    let request = engine_request(&d, body);
    let now = cache_friendly_now();
    let range = match engine.range(&d, &request, now) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    match engine.dashboard_data(&d, &request, now).await {
        Ok(panels) => Json(DataResponse {
            from_ms: range.from_ms,
            to_ms: range.to_ms,
            time_zone: engine.time_zone_name().to_string(),
            panels,
        })
        .into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, e),
    }
}

pub async fn control_values(
    State(state): State<AppState>,
    Path((id, control)): Path<(String, String)>,
    Json(body): Json<DataRequest>,
) -> Response {
    let Some(d) = state.app.catalog.get(&id) else {
        return error(StatusCode::NOT_FOUND, format!("no dashboard {id}"));
    };
    let Some(control) = d.controls.iter().find(|c| c.id == control).cloned() else {
        return error(StatusCode::NOT_FOUND, format!("no control {control}"));
    };
    let engine = match &state.app.engine {
        Ok(e) => e,
        Err(problem) => return error(StatusCode::SERVICE_UNAVAILABLE, problem.clone()),
    };
    let request = engine_request(&d, body);
    match engine
        .control_values(&d, &control, &request, cache_friendly_now())
        .await
    {
        Ok(values) => Json(ControlValues { values }).into_response(),
        Err(e) => error(StatusCode::BAD_GATEWAY, e),
    }
}
