//! The metric library: metrics defined once and used by name, so "change
//! fail rate" or "lead time" means the same thing on every panel and every
//! dashboard.
//!
//! Shared metrics live in `metrics.yaml` in the dashboards directory (the
//! `agents/` folder uses its parent's). A dashboard can define its own
//! under `metrics:`, which take precedence. A column uses one with
//! `op: metric, name: <name>`.
//!
//! References are expanded when a dashboard is loaded, into the plain
//! columns the query engine and the client already understand. The files
//! keep the reference, so an agent editing a panel keeps it too. A formula
//! metric becomes its formula column plus one helper column per input, with
//! ids `<column>__<input>`, appended to the layer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lint::{Problem, Severity};
use crate::model::{
    Column, Dashboard, Expr, Layer, Metric, Operation, PanelContent, Placeholder,
    PlaceholderReason, Query, QueryLanguage,
};

/// The shared library's file name, in the dashboards directory.
pub const LIBRARY_FILE: &str = "metrics.yaml";

/// A `metrics.yaml`: metrics by name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Library {
    #[serde(default)]
    pub metrics: BTreeMap<String, Metric>,
}

/// Whether `path` is a metric library rather than a dashboard.
pub fn is_library(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == LIBRARY_FILE)
}

/// The directory whose `metrics.yaml` applies to the dashboard file at
/// `path`: its own, or for the `agents/` folder, the dashboards directory.
pub fn library_dir(path: &Path) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new("."));
    let in_agents = dir
        .file_name()
        .is_some_and(|n| n == crate::server::AGENTS_DIR);
    if in_agents && !dir.join(LIBRARY_FILE).exists() {
        return dir.parent().unwrap_or(dir).to_path_buf();
    }
    dir.to_path_buf()
}

/// The library in `dir`; empty when there is no `metrics.yaml`.
pub fn load(dir: &Path) -> Result<Library, String> {
    let path = dir.join(LIBRARY_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Library::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn parse(text: &str) -> Result<Library, String> {
    if text.trim().is_empty() {
        return Ok(Library::default());
    }
    crate::yaml::from_str(text).map_err(|e| e.to_string())
}

/// Problems with the library's own definitions: each is expanded on its own,
/// as a column would use it.
pub fn check_library(library: &Library) -> Vec<Problem> {
    let defs = Defs {
        local: &BTreeMap::new(),
        shared: &library.metrics,
    };
    let mut problems = Vec::new();
    for (name, metric) in &library.metrics {
        let at = format!("metrics.{name}");
        if let Err(e) = expand(&probe(name), metric, &defs, &mut vec![name.clone()]) {
            problems.push(error(&at, e));
        }
        for (field, query) in filters(metric) {
            if query.language == QueryLanguage::Kql
                && !query.is_empty()
                && let Err(e) = crate::kql::parse(&query.text)
            {
                problems.push(error(&format!("{at}{field}"), format!("KQL: {e}")));
            }
        }
    }
    problems
}

/// Every filter in a metric and its inputs, with where it is.
fn filters(metric: &Metric) -> Vec<(String, &Query)> {
    let mut out: Vec<(String, &Query)> = metric
        .filter
        .iter()
        .map(|q| (".filter.text".to_string(), q))
        .collect();
    for (name, input) in &metric.inputs {
        out.extend(
            filters(input)
                .into_iter()
                .map(|(at, q)| (format!(".inputs.{name}{at}"), q)),
        );
    }
    out
}

fn probe(name: &str) -> Column {
    Column {
        id: name.to_string(),
        label: None,
        op: Operation::Metric { name: name.into() },
        filter: None,
        time_shift: None,
        per: None,
        format: None,
        helper: false,
        thresholds: Vec::new(),
        shorten: false,
    }
}

/// Expands every metric reference in `d`, using its own `metrics` and then
/// `shared`. A reference that can't be expanded is left as it is and
/// reported, with the path of the column that made it.
pub fn resolve(d: &Dashboard, shared: &Library) -> (Dashboard, Vec<Problem>) {
    let mut out = d.clone();
    let mut problems = Vec::new();
    for name in d.metrics.keys() {
        if shared.metrics.contains_key(name) {
            problems.push(Problem {
                path: format!("metrics.{name}"),
                severity: Severity::Warning,
                message: format!(
                    "{name:?} is also in the shared {LIBRARY_FILE}: this dashboard's definition is used here, so its numbers may not match other dashboards'"
                ),
            });
        }
    }
    let defs = Defs {
        local: &d.metrics,
        shared: &shared.metrics,
    };
    for (p, panel) in out.panels.iter_mut().enumerate() {
        let PanelContent::Chart(chart) = &mut panel.content else {
            continue;
        };
        for (l, layer) in chart.layers.iter_mut().enumerate() {
            let at = format!("panels[{p}].content.layers[{l}]");
            resolve_layer(layer, &defs, &at, &mut problems);
        }
    }
    (out, problems)
}

/// Like [`resolve`], for serving: a panel whose metrics can't be expanded
/// becomes a placeholder that says why, and the rest of the dashboard works.
pub fn resolve_for_display(d: &Dashboard, shared: Result<&Library, &str>) -> Dashboard {
    let empty = Library::default();
    let (mut out, problems) = resolve(d, shared.unwrap_or(&empty));
    for (p, panel) in out.panels.iter_mut().enumerate() {
        let prefix = format!("panels[{p}].");
        let reasons: Vec<&str> = problems
            .iter()
            .filter(|pr| pr.severity == Severity::Error && pr.path.starts_with(&prefix))
            .map(|pr| pr.message.as_str())
            .collect();
        if reasons.is_empty() {
            continue;
        }
        let mut detail = reasons.join("; ");
        if let Err(e) = shared {
            detail = format!("{detail} (the shared metrics could not be read: {e})");
        }
        panel.content = PanelContent::Placeholder(Placeholder {
            kind: "metric".into(),
            reason: PlaceholderReason::BrokenReference,
            detail,
        });
    }
    out
}

struct Defs<'a> {
    local: &'a BTreeMap<String, Metric>,
    shared: &'a BTreeMap<String, Metric>,
}

impl Defs<'_> {
    fn get(&self, name: &str) -> Result<&Metric, String> {
        self.local
            .get(name)
            .or_else(|| self.shared.get(name))
            .ok_or_else(|| {
                let mut known: Vec<&str> = self
                    .local
                    .keys()
                    .chain(self.shared.keys())
                    .map(String::as_str)
                    .collect();
                known.sort_unstable();
                known.dedup();
                if known.is_empty() {
                    format!("no metric {name:?}: define it in {LIBRARY_FILE} or the dashboard's `metrics`")
                } else {
                    format!("no metric {name:?}; known metrics: {}", known.join(", "))
                }
            })
    }
}

fn resolve_layer(layer: &mut Layer, defs: &Defs, at: &str, problems: &mut Vec<Problem>) {
    let mut helpers = Vec::new();
    for c in 0..layer.columns.len() {
        let Operation::Metric { name } = &layer.columns[c].op else {
            continue;
        };
        let name = name.clone();
        let result = defs
            .get(&name)
            .and_then(|def| expand(&layer.columns[c], def, defs, &mut vec![name.clone()]));
        match result {
            Ok(mut columns) => {
                let main = columns.pop().expect("expand returns the column last");
                helpers.extend(columns);
                layer.columns[c] = main;
            }
            Err(e) => problems.push(error(&format!("{at}.columns[{c}]"), e)),
        }
    }
    for h in helpers {
        if layer.columns.iter().any(|c| c.id == h.id) {
            problems.push(error(
                &format!("{at}.columns"),
                format!(
                    "the metric's input column {:?} has the id of another column in this layer; rename that column",
                    h.id
                ),
            ));
            continue;
        }
        layer.columns.push(h);
    }
}

fn error(path: &str, message: impl Into<String>) -> Problem {
    Problem {
        path: path.to_string(),
        severity: Severity::Error,
        message: message.into(),
    }
}

/// `column` (which refers to `def`) as plain columns: helper columns for a
/// formula's inputs first, then the column itself, last.
fn expand(
    column: &Column,
    def: &Metric,
    defs: &Defs,
    stack: &mut Vec<String>,
) -> Result<Vec<Column>, String> {
    let merged = Column {
        id: column.id.clone(),
        label: column.label.clone().or_else(|| def.label.clone()),
        op: def.op.clone(),
        filter: both(column.filter.as_ref(), def.filter.as_ref())?,
        time_shift: column.time_shift.clone().or_else(|| def.time_shift.clone()),
        per: column.per.or(def.per),
        format: column.format.clone().or_else(|| def.format.clone()),
        helper: column.helper,
        thresholds: if column.thresholds.is_empty() {
            def.thresholds.clone()
        } else {
            column.thresholds.clone()
        },
        shorten: column.shorten,
    };
    if !def.inputs.is_empty() && !matches!(def.op, Operation::Formula { .. }) {
        return Err("only formula metrics have `inputs`".into());
    }
    match &def.op {
        // Another metric under a new name, perhaps narrowed.
        Operation::Metric { name } => {
            if stack.contains(name) {
                stack.push(name.clone());
                return Err(format!(
                    "metrics refer to each other: {}",
                    stack.join(" → ")
                ));
            }
            let target = defs.get(name)?;
            stack.push(name.clone());
            let out = expand(&merged, target, defs, stack);
            stack.pop();
            out
        }
        Operation::Formula { text, expr } => {
            let mut out = Vec::new();
            let mut ids = BTreeMap::new();
            for (input, input_def) in &def.inputs {
                let id = format!("{}__{input}", column.id);
                let template = Column {
                    id: id.clone(),
                    label: None,
                    op: Operation::Metric {
                        name: input.clone(),
                    },
                    // The column's filter and time shift reach every input.
                    filter: merged.filter.clone(),
                    time_shift: merged.time_shift.clone(),
                    per: None,
                    format: None,
                    helper: true,
                    thresholds: Vec::new(),
                    shorten: false,
                };
                out.extend(
                    expand(&template, input_def, defs, stack)
                        .map_err(|e| format!("input {input:?}: {e}"))?,
                );
                ids.insert(input.clone(), id);
            }
            let expr = rename(expr, &ids)?;
            out.push(Column {
                op: Operation::Formula {
                    text: text.clone(),
                    expr,
                },
                filter: None,
                time_shift: None,
                ..merged
            });
            Ok(out)
        }
        op if op.is_bucket() => Err(format!(
            "a metric can't be a bucket ({}); put buckets in the panel",
            op_name(op)
        )),
        op if !op.inputs().is_empty() => Err(format!(
            "{} refers to a column by id, which a shared metric can't: use a formula with `inputs`",
            op_name(op)
        )),
        _ => Ok(vec![merged]),
    }
}

/// The formula's column names replaced by the helper columns' ids.
fn rename(expr: &Expr, ids: &BTreeMap<String, String>) -> Result<Expr, String> {
    Ok(match expr {
        Expr::Number(n) => Expr::Number(*n),
        Expr::Column(name) => Expr::Column(ids.get(name).cloned().ok_or_else(|| {
            format!("the formula uses {name:?}, which is not one of its `inputs`")
        })?),
        Expr::Call { call, args } => Expr::Call {
            call: call.clone(),
            args: args
                .iter()
                .map(|a| rename(a, ids))
                .collect::<Result<_, _>>()?,
        },
    })
}

/// Both filters apply: the metric's, and the column's narrowing of it.
fn both(a: Option<&Query>, b: Option<&Query>) -> Result<Option<Query>, String> {
    Ok(
        match (a.filter(|q| !q.is_empty()), b.filter(|q| !q.is_empty())) {
            (None, None) => None,
            (Some(q), None) | (None, Some(q)) => Some(q.clone()),
            (Some(a), Some(b)) if a.language == b.language => {
                let and = match a.language {
                    QueryLanguage::Kql => "and",
                    QueryLanguage::Lucene => "AND",
                };
                Some(Query {
                    language: a.language,
                    text: format!("({}) {and} ({})", a.text, b.text),
                })
            }
            _ => {
                return Err(
                "the column's filter and the metric's are in different query languages; use KQL for both"
                    .into(),
            );
            }
        },
    )
}

fn op_name(op: &Operation) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|v| v["op"].as_str().map(String::from))
        .unwrap_or_else(|| "this operation".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(yaml: &str) -> Library {
        parse(yaml).unwrap()
    }

    fn dashboard(columns: &str) -> Dashboard {
        crate::yaml::from_str(&format!(
            "id: d\ntitle: D\npanels:\n  - id: p\n    grid: {{ x: 0, y: 0, w: 12, h: 6 }}\n    content:\n      type: chart\n      kind: metric\n      layers:\n        - id: l\n          data_view: {{ index: delivery-deployments, time_field: deployed_at }}\n          columns:\n{columns}          encoding: {{ as: metric, value: rate }}\n"
        ))
        .unwrap()
    }

    fn columns(d: &Dashboard) -> &[Column] {
        let PanelContent::Chart(c) = &d.panels[0].content else {
            panic!("not a chart")
        };
        &c.layers[0].columns
    }

    const CFR: &str = "\
metrics:
  failed_deployments:
    op: count
    filter: { language: kql, text: 'result : failure' }
  change_fail_rate:
    label: Change fail rate
    description: Share of deployments that failed.
    op: formula
    text: failed / all
    expr: { call: divide, args: [failed, all] }
    inputs:
      failed: { op: metric, name: failed_deployments }
      all: { op: count }
    format: { kind: percent, decimals: 1 }
    thresholds:
      - { label: Elite, to: 0.05 }
      - { label: Low, from: 0.05 }
";

    #[test]
    fn a_formula_metric_becomes_its_formula_and_helper_inputs() {
        let d = dashboard("            - { id: rate, op: metric, name: change_fail_rate }\n");
        let (d, problems) = resolve(&d, &library(CFR));
        assert!(problems.is_empty(), "{problems:?}");
        let c = columns(&d);
        let ids: Vec<&str> = c.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["rate", "rate__all", "rate__failed"]);
        assert_eq!(c[0].label.as_deref(), Some("Change fail rate"));
        assert_eq!(c[0].thresholds.len(), 2);
        let Operation::Formula { expr, .. } = &c[0].op else {
            panic!("{:?}", c[0].op)
        };
        assert_eq!(expr.columns(), ["rate__failed", "rate__all"]);
        assert!(c[1].helper && c[2].helper);
        assert_eq!(
            c[2].filter.as_ref().map(|q| q.text.as_str()),
            Some("result : failure")
        );
    }

    #[test]
    fn the_columns_settings_win_and_its_filter_and_shift_reach_the_inputs() {
        let d = dashboard(
            "            - { id: rate, op: metric, name: change_fail_rate, label: Before, time_shift: previous, filter: { text: 'team : payments' } }\n",
        );
        let (d, problems) = resolve(&d, &library(CFR));
        assert!(problems.is_empty(), "{problems:?}");
        let c = columns(&d);
        assert_eq!(c[0].label.as_deref(), Some("Before"));
        assert_eq!(c[0].time_shift, None, "shifts apply to the inputs");
        for input in &c[1..] {
            assert_eq!(input.time_shift.as_deref(), Some("previous"));
        }
        assert_eq!(
            c[2].filter.as_ref().map(|q| q.text.as_str()),
            Some("(team : payments) and (result : failure)")
        );
        assert_eq!(
            c[1].filter.as_ref().map(|q| q.text.as_str()),
            Some("team : payments")
        );
    }

    #[test]
    fn the_dashboards_own_metrics_take_precedence_with_a_warning() {
        let mut d = dashboard("            - { id: rate, op: metric, name: failed_deployments }\n");
        d.metrics.insert(
            "failed_deployments".into(),
            library("metrics:\n  x: { op: count, label: Local }\n").metrics["x"].clone(),
        );
        let (d, problems) = resolve(&d, &library(CFR));
        assert_eq!(columns(&d)[0].label.as_deref(), Some("Local"));
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].severity, Severity::Warning);
        assert_eq!(problems[0].path, "metrics.failed_deployments");
    }

    #[test]
    fn unknown_metrics_and_cycles_are_reported_where_they_are_used() {
        let d = dashboard(
            "            - { id: n, op: count }\n            - { id: rate, op: metric, name: nope }\n",
        );
        let (_, problems) = resolve(&d, &library(CFR));
        assert_eq!(problems[0].path, "panels[0].content.layers[0].columns[1]");
        assert!(
            problems[0].message.contains("change_fail_rate"),
            "{}",
            problems[0].message
        );

        let lib = library("metrics:\n  a: { op: metric, name: b }\n  b: { op: metric, name: a }\n");
        let (_, problems) = resolve(
            &dashboard("            - { id: rate, op: metric, name: a }\n"),
            &lib,
        );
        assert!(problems[0].message.contains("a → b → a"), "{problems:?}");
    }

    #[test]
    fn a_broken_panel_becomes_a_placeholder_and_the_rest_still_work() {
        let d = dashboard("            - { id: rate, op: metric, name: nope }\n");
        let shown = resolve_for_display(&d, Ok(&library(CFR)));
        assert!(matches!(
            &shown.panels[0].content,
            PanelContent::Placeholder(p) if p.reason == PlaceholderReason::BrokenReference
        ));
    }

    #[test]
    fn library_definitions_are_checked_on_their_own() {
        let lib = library(
            "metrics:\n  bad: { op: formula, text: x, expr: { call: divide, args: [x, y] }, inputs: { x: { op: count } } }\n  buckets: { op: terms, fields: [team], size: 5 }\n  kql: { op: count, filter: { text: 'result :' } }\n",
        );
        let problems = check_library(&lib);
        let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            ["metrics.bad", "metrics.buckets", "metrics.kql.filter.text"]
        );
        assert!(problems[0].message.contains("\"y\""), "{problems:?}");
        assert!(check_library(&library(CFR)).is_empty());
    }

    #[test]
    fn agents_use_the_dashboards_directorys_library() {
        let dir = std::env::temp_dir().join(format!("teleplot-metrics-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        assert_eq!(library_dir(&dir.join("agents/x.yaml")), dir);
        assert_eq!(library_dir(&dir.join("x.yaml")), dir);
        assert!(load(&dir).unwrap().metrics.is_empty(), "no file: empty");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
