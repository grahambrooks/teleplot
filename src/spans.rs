//! Where the current trace is, for code deep under a request or CLI run.
//!
//! [`crate::telemetry`] tracks entered spans per thread, but a request's
//! future can move between tokio's worker threads, so the parent of new
//! spans travels with the task instead: [`within`] sets it for a future,
//! and [`step`] starts a child of it. With reporting off every span is a
//! no-op, and so is all of this.
//!
//! Span names say what a step does; types come from a small vocabulary:
//! `request`, `load`, `parse`, `query`, `render`, `io`. Never put query
//! bodies, filter values, URLs with query strings, paths or identities in a
//! name or an attribute.

use std::future::Future;

use crate::telemetry::{self, Kind, Parent, Span};

tokio::task_local! {
    static CURRENT: Parent;
}

/// The span new steps nest under: this task's, else the innermost entered
/// on this thread (a CLI run's sync code), else none.
pub fn current() -> Option<Parent> {
    CURRENT
        .try_with(Parent::clone)
        .ok()
        .or_else(telemetry::current)
}

/// A child of the current span, with a type. Off when there is none.
pub fn step(name: &str, span_type: &str) -> Span {
    let mut span = match current() {
        Some(parent) => parent.child(name),
        None => return telemetry::span(name),
    };
    span.set_type(span_type);
    span
}

/// A request to a backend (Elasticsearch, insight or habit's series API),
/// as a Client span of type `query`. `host` is the backend's host name, and
/// `name` its method and path *template*: never a query string.
pub fn backend(name: &str, method: &str, host: Option<&str>) -> Span {
    let mut span = step(name, "query");
    span.set_kind(Kind::Client)
        .attr("http.request.method", method);
    if let Some(host) = host {
        span.attr("server.address", host);
    }
    span
}

/// Runs `fut` with `span` as the parent of the steps under it.
pub async fn within<F: Future>(span: &Span, fut: F) -> F::Output {
    under(span.handle(), fut).await
}

/// Runs `fut` with `parent` (from [`current`], perhaps on another task) as
/// the parent of the steps under it.
pub async fn under<F: Future>(parent: Option<Parent>, fut: F) -> F::Output {
    match parent {
        Some(parent) => CURRENT.scope(parent, fut).await,
        None => fut.await,
    }
}

/// Records an HTTP status on a span, failing it for 4xx and 5xx.
pub fn status(span: &mut Span, code: u16) {
    span.attr("http.response.status_code", i64::from(code));
    if code >= 400 {
        span.fail(&format!("http_{code}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn without_a_trace_steps_are_off() {
        assert!(current().is_none());
        assert!(!step("load dashboard", "load").enabled());
        let span = step("outer", "request");
        within(&span, async {
            assert!(current().is_none());
        })
        .await;
    }
}
