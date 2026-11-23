//! Vendored from habit-otel 0.2.1 (habit/habit-otel/src/lib.rs). Keep in step with it; don't edit here.
//!
//! Report a tool's work to habit (design §5.3) as OTLP/JSON traces.
//!
//! - [`run`]: one CLI run, the root of a trace whose resource carries
//!   `habit.kind=command`, which habit maps to a `command` record.
//! - [`trace`] and [`mcp`]: the root of one unit of work in a long-running
//!   process (an editor action, an HTTP request, an MCP tool call). They
//!   export on a background thread, so the caller never waits.
//! - [`span`], [`Span::child`] and [`Parent::child`]: the steps inside a
//!   trace. habit keeps them as `span` records, so its timeline shows where
//!   a run's time went and its "Slowest steps" table ranks them by own time.
//!
//! A trace is exported whole, in one request, when its root ends: habit
//! computes a span's own time from the children in the same request.
//!
//! This file is self-contained (std and `serde_json`), so tools that can't
//! depend on habit-otel vendor it as their `telemetry` module. Keep the
//! copies in step with this one; each copy's header says which version it is.
//! See README.md.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// The export budget for one request.
pub const BUDGET: Duration = Duration::from_millis(100);
/// The spool cap, as in habit.
const SPOOL_CAP: u64 = 50 * 1024 * 1024;
/// Spans kept per trace; the rest are counted on the root as
/// `habit.dropped_spans`.
pub const MAX_SPANS: usize = 2000;
/// The version of this file, for vendored copies.
pub const VERSION: &str = "0.2.1";

/// An attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum Attr {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Strs(Vec<String>),
}

impl From<Vec<String>> for Attr {
    fn from(v: Vec<String>) -> Self {
        Attr::Strs(v)
    }
}
impl From<&str> for Attr {
    fn from(v: &str) -> Self {
        Attr::Str(v.to_string())
    }
}
impl From<String> for Attr {
    fn from(v: String) -> Self {
        Attr::Str(v)
    }
}
impl From<i64> for Attr {
    fn from(v: i64) -> Self {
        Attr::Int(v)
    }
}
impl From<i32> for Attr {
    fn from(v: i32) -> Self {
        Attr::Int(v.into())
    }
}
impl From<u32> for Attr {
    fn from(v: u32) -> Self {
        Attr::Int(v.into())
    }
}
impl From<u64> for Attr {
    fn from(v: u64) -> Self {
        Attr::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}
impl From<usize> for Attr {
    fn from(v: usize) -> Self {
        Attr::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}
impl From<f64> for Attr {
    fn from(v: f64) -> Self {
        Attr::Float(v)
    }
}
impl From<bool> for Attr {
    fn from(v: bool) -> Self {
        Attr::Bool(v)
    }
}

/// What a span does, in OpenTelemetry's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Work inside the process.
    Internal,
    /// Handling a request from a client (an MCP call, an HTTP request).
    Server,
    /// A request to another process or service (HTTP, a subprocess).
    Client,
}

/// How a root exports its trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Export {
    /// On the dropping thread, within [`BUDGET`]: a CLI at exit.
    Blocking,
    /// On the background exporter thread.
    Background,
    /// Nowhere: tests, which must never reach a real habit or its spool.
    #[cfg(test)]
    Discard,
}

struct Root {
    endpoint: Endpoint,
    service: String,
    version: String,
    habit_kind: Option<&'static str>,
    export: Export,
}

struct Finished {
    span_id: String,
    parent: Option<String>,
    name: String,
    kind: Kind,
    start: SystemTime,
    end: SystemTime,
    attrs: Vec<(String, Attr)>,
    error: Option<String>,
}

struct Open {
    spans: Vec<Finished>,
    dropped: usize,
}

/// Traces whose root is still open, by trace id: their finished spans.
fn open_traces() -> &'static Mutex<HashMap<String, Open>> {
    static OPEN: OnceLock<Mutex<HashMap<String, Open>>> = OnceLock::new();
    OPEN.get_or_init(Default::default)
}

thread_local! {
    /// The spans entered on this thread, innermost last, as (trace, span).
    static STACK: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// A span. Ends, and records itself, when dropped. Every method is a no-op
/// when reporting is off.
#[must_use = "a span ends when dropped"]
pub struct Span(Option<Box<Inner>>);

struct Inner {
    trace: String,
    id: String,
    parent: Option<String>,
    name: String,
    kind: Kind,
    start: SystemTime,
    clock: Instant,
    attrs: Vec<(String, Attr)>,
    error: Option<String>,
    /// Pushed on this thread's stack, so [`span`] finds it.
    entered: bool,
    root: Option<Root>,
}

/// A handle on an open span, to start children from another thread or task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parent {
    trace: String,
    span: String,
}

impl Span {
    fn off() -> Self {
        Span(None)
    }

    fn new(name: &str, trace: String, parent: Option<String>, root: Option<Root>) -> Self {
        Span(Some(Box::new(Inner {
            trace,
            id: hex(8),
            parent,
            name: name.to_string(),
            kind: Kind::Internal,
            start: SystemTime::now(),
            clock: Instant::now(),
            attrs: vec![],
            error: None,
            entered: false,
            root,
        })))
    }

    fn new_root(name: &str, root: Root) -> Self {
        let trace = hex(16);
        if let Ok(mut m) = open_traces().lock() {
            m.insert(
                trace.clone(),
                Open {
                    spans: vec![],
                    dropped: 0,
                },
            );
        }
        let mut s = Span::new(name, trace, None, Some(root));
        if let Some(repo) = std::env::current_dir().ok().and_then(|d| repo_name(&d)) {
            s.attr("vcs.repository.name", repo);
        }
        s
    }

    fn entered(mut self) -> Self {
        if let Some(i) = self.0.as_mut() {
            STACK.with(|s| s.borrow_mut().push((i.trace.clone(), i.id.clone())));
            i.entered = true;
        }
        self
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Set an attribute. Never put paths, arguments, text or identities in
    /// one: habit drops attributes it doesn't know, but other collectors may
    /// not.
    pub fn attr(&mut self, key: &str, value: impl Into<Attr>) -> &mut Self {
        if let Some(i) = self.0.as_mut() {
            let value = value.into();
            match i.attrs.iter_mut().find(|(k, _)| k == key) {
                Some((_, v)) => *v = value,
                None => i.attrs.push((key.to_string(), value)),
            }
        }
        self
    }

    /// What kind of step this is, for habit's span types: `build`, `http`,
    /// `exec`, `render`, `parse`… Without one, habit infers it from the
    /// span's kind and attributes.
    pub fn set_type(&mut self, span_type: &str) -> &mut Self {
        self.attr("habit.span_type", span_type)
    }

    pub fn set_kind(&mut self, kind: Kind) -> &mut Self {
        if let Some(i) = self.0.as_mut() {
            i.kind = kind;
        }
        self
    }

    /// Mark the span failed. `error_type` is a short class (`timeout`,
    /// `exit_1`, `io`), never a message that could carry a path.
    pub fn fail(&mut self, error_type: &str) -> &mut Self {
        if let Some(i) = self.0.as_mut() {
            i.error = Some(error_type.to_string());
        }
        self.attr("error.type", error_type)
    }

    /// A child of this span, not entered on the thread: for async code,
    /// where a task can move between threads.
    pub fn child(&self, name: &str) -> Span {
        match self.handle() {
            Some(p) => p.child(name),
            None => Span::off(),
        }
    }

    /// A handle for starting children elsewhere.
    pub fn handle(&self) -> Option<Parent> {
        self.0.as_ref().map(|i| Parent {
            trace: i.trace.clone(),
            span: i.id.clone(),
        })
    }

    /// Run `f` with this span entered, so [`span`] inside it is a child.
    pub fn scope<T>(&self, f: impl FnOnce() -> T) -> T {
        match self.handle() {
            Some(p) => p.scope(f),
            None => f(),
        }
    }
}

impl Parent {
    pub fn child(&self, name: &str) -> Span {
        let open = open_traces()
            .lock()
            .is_ok_and(|m| m.contains_key(&self.trace));
        if !open {
            return Span::off();
        }
        Span::new(name, self.trace.clone(), Some(self.span.clone()), None)
    }

    /// Run `f` with this span entered on the current thread.
    pub fn scope<T>(&self, f: impl FnOnce() -> T) -> T {
        STACK.with(|s| s.borrow_mut().push((self.trace.clone(), self.span.clone())));
        struct Pop;
        impl Drop for Pop {
            fn drop(&mut self) {
                STACK.with(|s| s.borrow_mut().pop());
            }
        }
        let _pop = Pop;
        f()
    }
}

/// A child of the innermost span entered on this thread (a [`run`], or a
/// span made by this function), entered in turn. Off when there is none.
pub fn span(name: &str) -> Span {
    match current() {
        Some(p) => p.child(name).entered(),
        None => Span::off(),
    }
}

/// The innermost span entered on this thread.
pub fn current() -> Option<Parent> {
    STACK.with(|s| {
        s.borrow().last().map(|(t, id)| Parent {
            trace: t.clone(),
            span: id.clone(),
        })
    })
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(i) = self.0.take() else { return };
        let end = i.start + i.clock.elapsed();
        if i.entered {
            STACK.with(|s| {
                let mut s = s.borrow_mut();
                if let Some(at) = s.iter().rposition(|(_, id)| *id == i.id) {
                    s.remove(at);
                }
            });
        }
        let Inner {
            trace,
            id,
            parent,
            name,
            kind,
            start,
            mut attrs,
            error,
            root,
            ..
        } = *i;
        let Ok(mut traces) = open_traces().lock() else {
            return;
        };
        match root {
            None => {
                // A straggler whose root already ended is discarded.
                if let Some(open) = traces.get_mut(&trace) {
                    if open.spans.len() < MAX_SPANS {
                        open.spans.push(Finished {
                            span_id: id,
                            parent,
                            name,
                            kind,
                            start,
                            end,
                            attrs,
                            error,
                        });
                    } else {
                        open.dropped += 1;
                    }
                }
            }
            Some(root) => {
                let open = traces.remove(&trace);
                drop(traces);
                let (mut spans, dropped) = open.map_or((vec![], 0), |o| (o.spans, o.dropped));
                if dropped > 0 {
                    attrs.push(("habit.dropped_spans".into(), Attr::from(dropped)));
                }
                spans.push(Finished {
                    span_id: id,
                    parent: None,
                    name,
                    kind,
                    start,
                    end,
                    attrs,
                    error,
                });
                let body = request(&root, &trace, &spans);
                match root.export {
                    Export::Blocking => export(&root.endpoint, &body, &spool_dir(), BUDGET),
                    Export::Background => background(root.endpoint, body),
                    #[cfg(test)]
                    Export::Discard => drop(body),
                }
            }
        }
    }
}

/// The root of one unit of work in a long-running process. Exports in the
/// background when dropped. Not entered: use [`Span::scope`] for sync work
/// under it, or [`Span::child`] in async code.
pub fn trace(service: &str, version: &str, name: &str) -> Span {
    root(service, version, name, None, Kind::Server)
}

/// The root of one MCP tool call (`habit.kind=mcp`), named for the tool.
pub fn mcp(service: &str, version: &str, tool: &str) -> Span {
    let mut s = root(service, version, tool, Some("mcp"), Kind::Server);
    s.attr("mcp.tool.name", tool);
    s
}

fn root(service: &str, version: &str, name: &str, kind: Option<&'static str>, k: Kind) -> Span {
    let Some(endpoint) = endpoint() else {
        return Span::off();
    };
    let mut s = Span::new_root(
        name,
        Root {
            endpoint,
            service: service.to_string(),
            version: version.to_string(),
            habit_kind: kind,
            export: Export::Background,
        },
    );
    s.set_kind(k);
    s
}

/// One run of a CLI: the root of its trace, entered on the calling thread
/// so [`span`] calls under it are its children. Exports when dropped.
pub struct Run {
    root: Span,
    exit: Option<i32>,
}

/// Start recording a run. `flags` may be raw arguments such as
/// `--out=/tmp/x`; only names are kept.
pub fn run(tool: &str, version: &str, subcommand: Option<&str>, flags: &[&str]) -> Run {
    let Some(endpoint) = endpoint() else {
        // Disabled: nothing else is computed.
        return Run {
            root: Span::off(),
            exit: None,
        };
    };
    let subcommand = subcommand.filter(|s| !s.is_empty());
    let name = match subcommand {
        Some(sc) => format!("{tool} {sc}"),
        None => tool.to_string(),
    };
    let mut root = Span::new_root(
        &name,
        Root {
            endpoint,
            service: tool.to_string(),
            version: version.to_string(),
            habit_kind: Some("command"),
            export: Export::Blocking,
        },
    );
    if let Some(sc) = subcommand {
        root.attr("habit.subcommand", sc);
    }
    let flags: Vec<String> = flags.iter().filter_map(|f| flag_name(f)).collect();
    if !flags.is_empty() {
        root.attr("habit.flags", flags);
    }
    Run {
        root: root.entered(),
        exit: None,
    }
}

impl Run {
    /// The exit code to record (default 0, or 101 when dropped while panicking).
    pub fn exit_code(&mut self, code: i32) {
        self.exit = Some(code);
    }

    pub fn enabled(&self) -> bool {
        self.root.enabled()
    }

    /// The run's root span, for attributes or children on other threads.
    pub fn span(&mut self) -> &mut Span {
        &mut self.root
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if !self.root.enabled() {
            return;
        }
        let exit = self
            .exit
            .unwrap_or(if std::thread::panicking() { 101 } else { 0 });
        self.root.attr("process.exit.code", exit);
        if exit != 0 {
            self.root.fail(&format!("exit_{exit}"));
        }
        drop(std::mem::replace(&mut self.root, Span::off()));
    }
}

fn nanos(t: SystemTime) -> String {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_string()
}

fn kv(key: &str, value: &Attr) -> Value {
    let value = match value {
        Attr::Str(s) => json!({ "stringValue": s }),
        // OTLP/JSON carries 64-bit integers as strings.
        Attr::Int(n) => json!({ "intValue": n.to_string() }),
        Attr::Float(f) => json!({ "doubleValue": f }),
        Attr::Bool(b) => json!({ "boolValue": b }),
        Attr::Strs(v) => {
            json!({ "arrayValue": { "values": v.iter().map(|s| json!({ "stringValue": s })).collect::<Vec<_>>() } })
        }
    };
    json!({ "key": key, "value": value })
}

/// The OTLP/JSON request for one trace.
fn request(root: &Root, trace: &str, spans: &[Finished]) -> Value {
    let s = |k: &str, v: &str| kv(k, &Attr::Str(v.to_string()));
    let mut resource = vec![
        s("service.name", &root.service),
        s("service.version", &root.version),
    ];
    if let Some(k) = root.habit_kind {
        resource.push(s("habit.kind", k));
    }
    let spans: Vec<Value> = spans
        .iter()
        .map(|sp| {
            let attrs: Vec<Value> = sp.attrs.iter().map(|(k, v)| kv(k, v)).collect();
            let mut out = json!({
                "traceId": trace,
                "spanId": sp.span_id,
                "name": sp.name,
                // SPAN_KIND_INTERNAL / SERVER / CLIENT
                "kind": match sp.kind { Kind::Internal => 1, Kind::Server => 2, Kind::Client => 3 },
                "startTimeUnixNano": nanos(sp.start),
                "endTimeUnixNano": nanos(sp.end.max(sp.start)),
                "attributes": attrs,
                "status": { "code": if sp.error.is_some() { 2 } else { 1 } },
            });
            if let Some(p) = &sp.parent {
                out["parentSpanId"] = json!(p);
            }
            out
        })
        .collect();
    json!({
        "resourceSpans": [{
            "resource": { "attributes": resource },
            "scopeSpans": [{
                "scope": { "name": "habit-otel", "version": VERSION },
                "spans": spans,
            }]
        }]
    })
}

// ---- export

/// An OTLP/HTTP endpoint on loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    host: String,
    port: u16,
    path: String,
}

impl Endpoint {
    /// `http://` on loopback only: nothing leaves the machine.
    fn parse(url: &str) -> Option<Self> {
        let rest = url.trim().strip_prefix("http://")?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
            None => (rest, ""),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h, p.parse().ok()?),
            _ => (authority, 80),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let loopback = host == "localhost"
            || host == "::1"
            || host
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|a| a.is_loopback());
        if !loopback {
            return None;
        }
        // 4317 is the OTLP gRPC port; habit-otel speaks OTLP/HTTP.
        let port = if port == 4317 { 4318 } else { port };
        let path = if path.ends_with("/v1/traces") {
            path.to_string()
        } else {
            format!("{path}/v1/traces")
        };
        Some(Endpoint {
            host: host.to_string(),
            port,
            path,
        })
    }

    fn post(&self, body: &str, budget: Duration) -> std::io::Result<u16> {
        let deadline = Instant::now() + budget;
        let left = || {
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1))
        };
        let address = (self.host.as_str(), self.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::other("the endpoint's host does not resolve"))?;
        let mut stream = TcpStream::connect_timeout(&address, left())?;
        stream.set_write_timeout(Some(left()))?;
        let head = format!(
            "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.host,
            self.port,
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        stream.set_read_timeout(Some(left()))?;
        let mut status = [0u8; 12];
        stream.read_exact(&mut status)?;
        // `HTTP/1.1 200`
        std::str::from_utf8(&status[9..12])
            .ok()
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| std::io::Error::other("the endpoint did not answer HTTP"))
    }
}

/// POST within the budget, or spool.
fn export(endpoint: &Endpoint, body: &Value, spool: &Path, budget: Duration) {
    let body = body.to_string();
    let ok = endpoint
        .post(&body, budget)
        .is_ok_and(|c| (200..300).contains(&c));
    if !ok {
        let _ = spool_write(spool, &body);
    }
}

/// Hand a trace to the exporter thread, starting it on first use.
fn background(endpoint: Endpoint, body: Value) {
    static TX: OnceLock<Mutex<Sender<(Endpoint, Value)>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<(Endpoint, Value)>();
        let _ = std::thread::Builder::new()
            .name("habit-otel".into())
            .spawn(move || {
                let spool = spool_dir();
                for (e, b) in rx {
                    export(&e, &b, &spool, BUDGET);
                }
            });
        Mutex::new(tx)
    });
    if let Ok(tx) = tx.lock() {
        let _ = tx.send((endpoint, body));
    }
}

/// Where to report, or `None` (off). In order:
///
/// - off when `HABIT_TELEMETRY=off` or `OTEL_SDK_DISABLED=true`;
/// - `HABIT_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`,
///   `OTEL_EXPORTER_OTLP_ENDPOINT`;
/// - `endpoint = "…"` in `$XDG_CONFIG_HOME/habit/client.toml` (else
///   `~/.config/habit/client.toml`), for processes that don't inherit the
///   environment: Claude Code passes its `OTEL_*` settings neither to MCP
///   servers nor to Bash commands. `enabled = false` there turns it off.
///
/// An endpoint that isn't plain HTTP on loopback turns it off too.
fn endpoint() -> Option<Endpoint> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if var("HABIT_TELEMETRY").is_some_and(|v| v.eq_ignore_ascii_case("off"))
        || var("OTEL_SDK_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return None;
    }
    let file = client_file();
    if file.as_ref().is_some_and(|f| f.enabled == Some(false)) {
        return None;
    }
    let url = var("HABIT_ENDPOINT")
        .or_else(|| var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"))
        .or_else(|| var("OTEL_EXPORTER_OTLP_ENDPOINT"))
        .or_else(|| file.and_then(|f| f.endpoint))?;
    Endpoint::parse(&url)
}

#[derive(Debug, Default, PartialEq)]
struct ClientFile {
    endpoint: Option<String>,
    enabled: Option<bool>,
}

fn client_file() -> Option<ClientFile> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    let text = std::fs::read_to_string(dir.join("habit/client.toml")).ok()?;
    Some(parse_client_file(&text))
}

/// The two keys of client.toml, read without a TOML parser.
fn parse_client_file(text: &str) -> ClientFile {
    let mut out = ClientFile::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"');
        match k.trim() {
            "endpoint" if !v.is_empty() => out.endpoint = Some(v.to_string()),
            "enabled" => out.enabled = v.parse().ok(),
            _ => {}
        }
    }
    out
}

/// A flag's name: `--out=/tmp/x` → `out`. Not a flag → `None`.
pub fn flag_name(f: &str) -> Option<String> {
    if !f.starts_with('-') {
        return None;
    }
    let name = f.trim_start_matches('-').split('=').next()?.trim();
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| name.to_string())
}

fn hex(bytes: usize) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut out = String::new();
    while out.len() < bytes * 2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        h.write_u64(SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(bytes * 2);
    out
}

// ---- spool: the same format as habit's src/spool.rs; keep them in step.

fn spool_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("HABIT_SPOOL_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let data = if let Some(d) = std::env::var_os("HABIT_DATA_DIR") {
        PathBuf::from(d)
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/habit")
    } else {
        match std::env::var_os("XDG_DATA_HOME") {
            Some(d) if !d.is_empty() => PathBuf::from(d).join("habit"),
            _ => home.join(".local/share/habit"),
        }
    };
    data.join("spool")
}

fn spool_write(dir: &Path, body: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let line = format!("{{\"signal\":\"traces\",\"body\":{body}}}\n");
    let mut files: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.len())))
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|(_, n)| n).sum();
    let mut dropped = 0u64;
    for (path, len) in &files {
        if total + line.len() as u64 <= SPOOL_CAP {
            break;
        }
        dropped += std::fs::read_to_string(path).map_or(0, |t| t.lines().count() as u64);
        if std::fs::remove_file(path).is_ok() {
            total -= len;
        }
    }
    if dropped > 0 {
        let p = dir.join("dropped");
        let prev: u64 = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let _ = std::fs::write(p, (prev + dropped).to_string());
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = format!("{ms:013}-{}-{}.jsonl", std::process::id(), hex(4));
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(line.as_bytes())?;
    drop(f);
    std::fs::rename(tmp, dir.join(name))
}

// ---- the repo name for the cwd, as habit's src/gitrepo.rs.

fn repo_name(dir: &Path) -> Option<String> {
    let git = dir
        .ancestors()
        .map(|d| d.join(".git"))
        .find(|g| g.is_dir())?;
    let text = std::fs::read_to_string(git.join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin
            && let Some(url) = line
                .strip_prefix("url")
                .and_then(|l| l.trim_start().strip_prefix('='))
        {
            let last = url
                .trim()
                .trim_end_matches('/')
                .rsplit(['/', ':'])
                .next()?
                .trim_end_matches(".git");
            return (!last.is_empty()).then(|| last.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    #[test]
    fn flag_names_only() {
        assert_eq!(flag_name("--out=/tmp/x").as_deref(), Some("out"));
        assert_eq!(flag_name("-v").as_deref(), Some("v"));
        assert_eq!(flag_name("positional"), None);
        assert_eq!(flag_name("--bad name"), None);
    }

    #[test]
    fn endpoints_are_plain_http_on_loopback() {
        let e = Endpoint::parse("http://127.0.0.1:4317").unwrap();
        assert_eq!((e.port, e.path.as_str()), (4318, "/v1/traces"));
        let e = Endpoint::parse("http://localhost:4318/v1/traces").unwrap();
        assert_eq!(
            (e.host.as_str(), e.path.as_str()),
            ("localhost", "/v1/traces")
        );
        assert_eq!(Endpoint::parse("http://[::1]:4318").unwrap().host, "::1");
        assert!(Endpoint::parse("https://127.0.0.1:4318").is_none());
        assert!(Endpoint::parse("http://collector.example.com:4318").is_none());
        assert!(Endpoint::parse("http://10.0.0.2:4318").is_none());
    }

    #[test]
    fn the_client_file_has_two_keys() {
        let f = parse_client_file(
            "# habit\nendpoint = \"http://127.0.0.1:4318\"\nenabled = true # on\nother = 1\n",
        );
        assert_eq!(f.endpoint.as_deref(), Some("http://127.0.0.1:4318"));
        assert_eq!(f.enabled, Some(true));
        assert_eq!(parse_client_file("enabled = false").enabled, Some(false));
    }

    fn test_root(export: Export) -> Root {
        Root {
            endpoint: Endpoint::parse("http://127.0.0.1:9").unwrap(),
            service: "brake".into(),
            version: "2026.11.1".into(),
            habit_kind: Some("command"),
            export,
        }
    }

    fn finished(id: &str, parent: Option<&str>, error: Option<&str>) -> Finished {
        Finished {
            span_id: id.into(),
            parent: parent.map(str::to_string),
            name: id.into(),
            kind: Kind::Internal,
            start: SystemTime::now(),
            end: SystemTime::now(),
            attrs: vec![("process.exit.code".into(), Attr::Int(3))],
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn the_request_carries_what_habit_maps() {
        let body = request(
            &test_root(Export::Blocking),
            "t",
            &[
                finished("child", Some("root"), None),
                finished("root", None, Some("exit_3")),
            ],
        );
        let text = body.to_string();
        assert!(text.contains(r#""key":"habit.kind","value":{"stringValue":"command"}"#));
        assert!(text.contains(r#""intValue":"3""#));
        assert!(text.contains(r#""status":{"code":2}"#));
        let spans = &body["resourceSpans"][0]["scopeSpans"][0]["spans"];
        assert_eq!(spans[0]["parentSpanId"], "root");
        assert!(spans[1].get("parentSpanId").is_none());
    }

    #[test]
    fn spans_nest_under_the_innermost_entered_span() {
        // Off: no root, so nothing is open and spans are no-ops.
        assert!(!span("x").enabled());

        let root = Span::new_root("root", test_root(Export::Discard)).entered();
        let trace = root.0.as_ref().unwrap().trace.clone();
        let root_id = root.0.as_ref().unwrap().id.clone();
        {
            let mut a = span("a");
            a.set_type("build").attr("count", 3usize);
            let b = span("b");
            assert_eq!(
                b.0.as_ref().unwrap().parent.as_deref(),
                Some(a.0.as_ref().unwrap().id.as_str())
            );
            drop(b);
            let c = root.child("c");
            assert_eq!(
                c.0.as_ref().unwrap().parent.as_deref(),
                Some(root_id.as_str())
            );
            let h = c.handle().unwrap();
            std::thread::spawn(move || drop(h.child("on another thread")))
                .join()
                .unwrap();
            c.scope(|| {
                let d = span("d");
                assert_eq!(d.0.as_ref().unwrap().parent, c.handle().map(|p| p.span));
            });
        }
        let names: Vec<String> = open_traces().lock().unwrap()[&trace]
            .spans
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, ["b", "on another thread", "d", "c", "a"]);
        assert_eq!(current().map(|p| p.span), Some(root_id));
        drop(root);
        assert!(current().is_none());
        assert!(!open_traces().lock().unwrap().contains_key(&trace));
    }

    #[test]
    fn exports_to_a_listener_and_spools_when_down() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut len = 0;
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                .unwrap();
            (first, String::from_utf8(body).unwrap())
        });
        let dir = std::env::temp_dir().join(format!("habit-otel-test-{}", std::process::id()));
        export(
            &Endpoint::parse(&format!("http://{addr}")).unwrap(),
            &json!({"resourceSpans": []}),
            &dir,
            Duration::from_secs(2),
        );
        let (line, body) = server.join().unwrap();
        assert!(line.starts_with("POST /v1/traces"));
        assert_eq!(body, r#"{"resourceSpans":[]}"#);
        assert!(!dir.exists(), "nothing spooled on success");

        let t = Instant::now();
        export(
            &Endpoint::parse("http://127.0.0.1:9").unwrap(),
            &json!({}),
            &dir,
            BUDGET,
        );
        assert!(t.elapsed() < Duration::from_millis(250));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
