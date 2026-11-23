//! A trace timeline: the spans of one trace as nested bars on a time axis,
//! like an OpenTelemetry trace view. One row per span arrives (a records
//! layer); rows are grouped into traces, newest first.
//!
//! Finding the trace, after Jaeger and Tempo: a name filter (matching any
//! span's label), a minimum length, a scatter plot of every trace's length
//! over time to click outliers in, and a list that is newest first, slowest
//! first, or one row per name with counts and typical lengths.
//!
//! Reading it: a waterfall (one row per span) or a flame graph (spans
//! stacked by depth, with lanes for siblings that ran side by side). The
//! axis is at least [`MIN_AXIS_MS`] long so short traces look short ("Fit
//! trace" stretches one). Large traces open folded to the deepest level that
//! fits in [`FOLD_AT`] rows. Clicking a span highlights every span named
//! like it, as the filter does for matching spans; highlighted spans are
//! never folded away. The critical path (Jaeger's) marks the chain of spans
//! the trace's end waited on. Below the bars, each `color` value's own time:
//! a span's duration less the time its children cover.
//!
//! Comparing: keep one trace as the baseline and pick another. Both draw
//! on one axis, with the span names whose own time changed most.

use std::collections::{BTreeSet, HashMap};

use leptos::prelude::*;

use super::theme::categorical;
use crate::model::{Cell, Chart, Encoding, LayerData, PanelData};

/// One span, as read from a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub trace: String,
    pub id: String,
    pub parent: Option<String>,
    pub start: f64,
    pub end: f64,
    pub label: String,
    pub color: Option<String>,
}

/// A trace's spans in drawing order (depth first, children by start), with
/// each span's depth.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub id: String,
    pub start: f64,
    pub end: f64,
    pub spans: Vec<(Span, usize)>,
}

impl Trace {
    /// The label of the first top-level span.
    pub fn name(&self) -> &str {
        self.spans.first().map_or("", |(s, _)| s.label.as_str())
    }
}

fn text(cell: Option<&Cell>) -> Option<String> {
    match cell? {
        Cell::Text(s) if !s.is_empty() => Some(s.clone()),
        Cell::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The spans in a layer's rows; rows without a trace, id or times are skipped.
pub fn spans(layer: &LayerData, encoding: &Encoding) -> Vec<Span> {
    let Encoding::Timeline {
        trace,
        span,
        parent,
        start,
        end,
        label,
        color,
    } = encoding
    else {
        return vec![];
    };
    let col = |id: &str| layer.series(id).map(|s| s.values.as_slice()).unwrap_or(&[]);
    let (traces, ids, parents, starts, ends, labels) = (
        col(trace),
        col(span),
        col(parent),
        col(start),
        col(end),
        col(label),
    );
    let colors = color.as_deref().map(col).unwrap_or(&[]);
    (0..layer.rows)
        .filter_map(|i| {
            let start = starts.get(i)?.number()?;
            let end = ends.get(i)?.number()?.max(start);
            Some(Span {
                trace: text(traces.get(i))?,
                id: text(ids.get(i))?,
                parent: text(parents.get(i)),
                start,
                end,
                label: text(labels.get(i)).unwrap_or_default(),
                color: text(colors.get(i)),
            })
        })
        .collect()
}

/// Spans grouped into traces, newest first. A span whose parent isn't in
/// its trace is drawn at the top level.
pub fn traces(spans: Vec<Span>) -> Vec<Trace> {
    let mut by_trace: HashMap<String, Vec<Span>> = HashMap::new();
    for s in spans {
        by_trace.entry(s.trace.clone()).or_default().push(s);
    }
    let mut out: Vec<Trace> = by_trace
        .into_iter()
        .map(|(id, mut spans)| {
            spans.sort_by(|a, b| a.start.total_cmp(&b.start).then(b.end.total_cmp(&a.end)));
            let ids: BTreeSet<&str> = spans.iter().map(|s| s.id.as_str()).collect();
            let mut children: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
            for (i, s) in spans.iter().enumerate() {
                let parent = s
                    .parent
                    .as_deref()
                    .filter(|p| ids.contains(p) && *p != s.id);
                children.entry(parent).or_default().push(i);
            }
            let mut order = vec![];
            let mut seen = vec![false; spans.len()];
            // Depth first; `seen` guards against parent cycles in bad data.
            let mut stack: Vec<(usize, usize)> = children
                .get(&None)
                .into_iter()
                .flatten()
                .rev()
                .map(|&i| (i, 0))
                .collect();
            while let Some((i, depth)) = stack.pop() {
                if std::mem::replace(&mut seen[i], true) {
                    continue;
                }
                order.push((i, depth));
                if let Some(kids) = children.get(&Some(spans[i].id.as_str())) {
                    stack.extend(kids.iter().rev().map(|&k| (k, depth + 1)));
                }
            }
            // Spans only reachable through a cycle still get drawn.
            order.extend((0..spans.len()).filter(|&i| !seen[i]).map(|i| (i, 0)));
            let start = spans.iter().map(|s| s.start).fold(f64::INFINITY, f64::min);
            let end = spans
                .iter()
                .map(|s| s.end)
                .fold(f64::NEG_INFINITY, f64::max);
            let spans = order
                .into_iter()
                .map(|(i, d)| (spans[i].clone(), d))
                .collect();
            Trace {
                id,
                start,
                end,
                spans,
            }
        })
        .collect();
    out.sort_by(|a, b| b.start.total_cmp(&a.start));
    out
}

/// Each span's own time: its duration less the union of its children's
/// intervals (clipped to the span).
pub fn self_times(trace: &Trace) -> Vec<f64> {
    let mut kids: HashMap<&str, Vec<(f64, f64)>> = HashMap::new();
    for (s, _) in &trace.spans {
        if let Some(p) = &s.parent {
            kids.entry(p.as_str()).or_default().push((s.start, s.end));
        }
    }
    trace
        .spans
        .iter()
        .map(|(s, _)| {
            let mut iv: Vec<(f64, f64)> = kids
                .get(s.id.as_str())
                .into_iter()
                .flatten()
                .map(|&(a, b)| (a.max(s.start), b.min(s.end)))
                .filter(|(a, b)| b > a)
                .collect();
            iv.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (mut covered, mut cursor) = (0.0, f64::NEG_INFINITY);
            for (a, b) in iv {
                let a = a.max(cursor);
                if b > a {
                    covered += b - a;
                    cursor = b;
                }
            }
            (s.end - s.start - covered).max(0.0)
        })
        .collect()
}

/// Own time per `color` value, largest first.
pub fn time_by_color(trace: &Trace) -> Vec<(String, f64)> {
    let mut by: HashMap<String, f64> = HashMap::new();
    for ((s, _), own) in trace.spans.iter().zip(self_times(trace)) {
        *by.entry(s.color.clone().unwrap_or_else(|| "other".into()))
            .or_default() += own;
    }
    let mut out: Vec<(String, f64)> = by.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Epoch ms → `2026-11-16 14:05:09 EST` in the dashboard's time zone.
pub fn date_time(ms: f64, tz: &str) -> String {
    super::clock::local(ms, tz, true)
}

/// Milliseconds for reading: `850 ms`, `1.25 s`, `2.4 min`.
pub fn duration(ms: f64) -> String {
    match ms.abs() {
        a if a < 1000.0 => format!("{ms:.0} ms"),
        a if a < 60_000.0 => format!("{:.2} s", ms / 1000.0),
        a if a < 3_600_000.0 => format!("{:.1} min", ms / 60_000.0),
        _ => format!("{:.1} h", ms / 3_600_000.0),
    }
}

/// The shortest axis a trace is drawn on, unless the viewer asks to fit
/// it. Short traces then look short: a 0 ms request is a hairline, not a
/// full-width bar like a minute-long one.
pub const MIN_AXIS_MS: f64 = 100.0;

/// The axis length for a trace of `len` ms: rounded up to a fifth of a
/// tick, so little of the width is empty, and at least [`MIN_AXIS_MS`]
/// unless `fit`.
pub fn axis_ms(len: f64, fit: bool) -> f64 {
    let span = len.max(if fit { 1.0 } else { MIN_AXIS_MS });
    let minor = tick_step(span) / 5.0;
    (span / minor).ceil() * minor
}

/// Whether a trace matches a lower-case name filter: its name or any of
/// its spans' labels contains it.
pub fn matches(trace: &Trace, query: &str) -> bool {
    query.is_empty()
        || trace
            .spans
            .iter()
            .any(|(s, _)| s.label.to_lowercase().contains(query))
}

/// The traces sharing one name, for the by-name list.
#[derive(Debug, Clone, PartialEq)]
pub struct NameStats {
    pub name: String,
    pub count: usize,
    pub median: f64,
    pub max: f64,
    pub total: f64,
}

/// One row per trace name, most total time first.
pub fn by_name<'a>(traces: impl IntoIterator<Item = &'a Trace>) -> Vec<NameStats> {
    let mut by: HashMap<&str, Vec<f64>> = HashMap::new();
    for t in traces {
        by.entry(t.name()).or_default().push(t.end - t.start);
    }
    let mut out: Vec<NameStats> = by
        .into_iter()
        .map(|(name, mut lengths)| {
            lengths.sort_by(f64::total_cmp);
            let n = lengths.len();
            let median = if n % 2 == 1 {
                lengths[n / 2]
            } else {
                (lengths[n / 2 - 1] + lengths[n / 2]) / 2.0
            };
            NameStats {
                name: name.to_string(),
                count: n,
                median,
                max: lengths[n - 1],
                total: lengths.iter().sum(),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.total
            .total_cmp(&a.total)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// How many spans follow each span in drawing order at a greater depth:
/// what folding it hides.
pub fn descendants(trace: &Trace) -> Vec<usize> {
    let s = &trace.spans;
    (0..s.len())
        .map(|i| s[i + 1..].iter().take_while(|(_, d)| *d > s[i].1).count())
        .collect()
}

/// The spans drawn when the spans in `folded` hide their descendants.
pub fn visible(trace: &Trace, folded: &BTreeSet<String>) -> Vec<usize> {
    let mut out = vec![];
    let mut hide_below: Option<usize> = None;
    for (i, (s, depth)) in trace.spans.iter().enumerate() {
        if let Some(h) = hide_below {
            if *depth > h {
                continue;
            }
            hide_below = None;
        }
        out.push(i);
        if folded.contains(&s.id) {
            hide_below = Some(*depth);
        }
    }
    out
}

/// Spans with more rows than this open folded.
pub const FOLD_AT: usize = 50;

/// Which spans stand out: those named like a clicked span, or those
/// matching the name filter. The rest are dimmed.
#[derive(Debug, Clone, PartialEq)]
pub enum Highlight {
    None,
    /// Spans with exactly this label.
    Label(String),
    /// Spans whose label contains this lower-case text.
    Contains(String),
}

impl Highlight {
    /// Whether a span stands out; `None` when nothing is highlighted.
    pub fn hit(&self, label: &str) -> Option<bool> {
        match self {
            Highlight::None => None,
            Highlight::Label(l) => Some(label == l),
            Highlight::Contains(q) => Some(label.to_lowercase().contains(q.as_str())),
        }
    }
}

/// The spans folded when a trace opens: none if it fits in [`FOLD_AT`]
/// rows; otherwise every parent from the shallowest depth that does fit,
/// so as many levels show as fit. Parents of highlighted spans stay open,
/// so a highlighted span is never hidden.
pub fn initial_folds(trace: &Trace, highlight: &Highlight) -> BTreeSet<String> {
    if trace.spans.len() <= FOLD_AT {
        return BTreeSet::new();
    }
    let below = descendants(trace);
    let mut keep = BTreeSet::new();
    let mut path: Vec<usize> = vec![];
    for (i, (s, depth)) in trace.spans.iter().enumerate() {
        path.truncate(*depth);
        if highlight.hit(&s.label) == Some(true) {
            keep.extend(path.iter().copied());
        }
        path.push(i);
    }
    let deepest = trace.spans.iter().map(|(_, d)| *d).max().unwrap_or(0);
    let fold_from = |level: usize| -> BTreeSet<String> {
        trace
            .spans
            .iter()
            .enumerate()
            .filter(|(i, (_, d))| *d >= level && below[*i] > 0 && !keep.contains(i))
            .map(|(_, (s, _))| s.id.clone())
            .collect()
    };
    (0..=deepest)
        .rev()
        .map(fold_from)
        .find(|f| visible(trace, f).len() <= FOLD_AT)
        .unwrap_or_else(|| fold_from(0))
}

/// Jaeger's critical path: the chain of work the trace's end waited on, as
/// (span index, from, to) segments that don't overlap. Walking back from a
/// span's end, the child that finished last is on the path; before that
/// child started, the next child to finish is; and so on. Time in a span
/// that none of those children covers is the span's own. Siblings that ran
/// alongside a child on the path are off it: finishing them sooner
/// wouldn't have finished the trace sooner.
///
/// Unlike Jaeger, a child isn't cut off at its parent's end: a Claude Code
/// interaction can end while the subagents it started work on for an hour.
/// A span reaches as far as its last descendant, and only time within the
/// span's own start and end counts as its own.
pub fn critical_path(trace: &Trace) -> Vec<(usize, f64, f64)> {
    let spans = &trace.spans;
    let index: HashMap<&str, usize> = spans
        .iter()
        .enumerate()
        .map(|(i, (s, _))| (s.id.as_str(), i))
        .collect();
    let mut kids: Vec<Vec<usize>> = vec![vec![]; spans.len()];
    let mut roots = vec![];
    for (i, (s, _)) in spans.iter().enumerate() {
        match s.parent.as_deref().and_then(|p| index.get(p)) {
            Some(&p) if p != i => kids[p].push(i),
            _ => roots.push(i),
        }
    }
    // How far each span's work reaches: its end, or a later descendant's.
    // Depth-first order puts descendants after their span, so walking it
    // backwards finishes every child before its parent.
    let mut reach: Vec<f64> = spans.iter().map(|(s, _)| s.end).collect();
    for i in (0..spans.len()).rev() {
        for &k in &kids[i] {
            reach[i] = reach[i].max(reach[k]);
        }
    }
    struct Walk<'a> {
        spans: &'a [(Span, usize)],
        kids: &'a [Vec<usize>],
        reach: &'a [f64],
        used: Vec<bool>,
        out: Vec<(usize, f64, f64)>,
    }
    impl Walk<'_> {
        /// `o`'s own time from `a` to `b`, within its own start and end.
        fn own(&mut self, o: usize, a: f64, b: f64) {
            let s = &self.spans[o].0;
            let (a, b) = (a.max(s.start), b.min(s.end));
            if b > a {
                self.out.push((o, a, b));
            }
        }
    }
    impl Walk<'_> {
        /// Walks `children` back from `end` down to `start`; `own` is the
        /// span whose uncovered time is on the path (none for the trace).
        fn back(&mut self, own: Option<usize>, children: &[usize], start: f64, end: f64) {
            let mut cursor = end;
            while cursor > start {
                let next = children
                    .iter()
                    .copied()
                    .filter(|&k| !self.used[k] && self.spans[k].0.start < cursor)
                    .max_by(|&a, &b| {
                        self.reach[a]
                            .min(cursor)
                            .total_cmp(&self.reach[b].min(cursor))
                            .then(self.spans[a].0.start.total_cmp(&self.spans[b].0.start))
                    });
                let Some(k) = next else { break };
                self.used[k] = true;
                let child_end = self.reach[k].min(cursor);
                if let Some(o) = own
                    && cursor > child_end
                {
                    self.own(o, child_end, cursor);
                }
                let child_start = self.spans[k].0.start.max(start);
                let grandchildren = self.kids[k].clone();
                self.back(Some(k), &grandchildren, child_start, child_end);
                cursor = child_start;
            }
            if let Some(o) = own
                && cursor > start
            {
                self.own(o, start, cursor);
            }
        }
    }
    let mut walk = Walk {
        spans,
        kids: &kids,
        reach: &reach,
        used: vec![false; spans.len()],
        out: vec![],
    };
    walk.back(None, &roots, trace.start, trace.end);
    walk.out.sort_by(|a, b| a.1.total_cmp(&b.1));
    walk.out
}

/// Time on the critical path per `color` value, largest first.
pub fn critical_by_color(trace: &Trace, path: &[(usize, f64, f64)]) -> Vec<(String, f64)> {
    let mut by: HashMap<String, f64> = HashMap::new();
    for &(i, from, to) in path {
        let color = trace.spans[i]
            .0
            .color
            .clone()
            .unwrap_or_else(|| "other".into());
        *by.entry(color).or_default() += to - from;
    }
    let mut out: Vec<(String, f64)> = by.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The flame graph's rows: each depth gets as many lanes as its
/// overlapping spans need (siblings running side by side stack up), and
/// deeper levels sit below. Returns each span's row and the row count.
pub fn flame_rows(trace: &Trace) -> (Vec<usize>, usize) {
    let spans = &trace.spans;
    let deepest = spans.iter().map(|(_, d)| *d).max().unwrap_or(0);
    let mut lanes: Vec<Vec<f64>> = vec![vec![]; deepest + 1];
    let mut lane = vec![0; spans.len()];
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (&spans[a], &spans[b]);
        sa.1.cmp(&sb.1).then(sa.0.start.total_cmp(&sb.0.start))
    });
    for i in order {
        let (s, depth) = &spans[i];
        let ends = &mut lanes[*depth];
        let l = ends
            .iter()
            .position(|&end| end <= s.start)
            .unwrap_or(ends.len());
        if l == ends.len() {
            ends.push(s.end);
        } else {
            ends[l] = s.end;
        }
        lane[i] = l;
    }
    let mut offset = vec![0; deepest + 2];
    for d in 0..=deepest {
        offset[d + 1] = offset[d] + lanes[d].len();
    }
    let rows = spans
        .iter()
        .zip(&lane)
        .map(|((_, d), l)| offset[*d] + l)
        .collect();
    (rows, offset[deepest + 1])
}

/// One span name across two traces, for comparing them.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanDiff {
    pub label: String,
    /// Spans with this name in the baseline and in the other trace.
    pub count: (usize, usize),
    /// Their own time in each.
    pub own: (f64, f64),
}

/// Span names in either trace, the biggest change in own time first.
pub fn compare(baseline: &Trace, other: &Trace) -> Vec<SpanDiff> {
    let mut by: HashMap<String, SpanDiff> = HashMap::new();
    for (side, trace) in [(0, baseline), (1, other)] {
        for ((s, _), own) in trace.spans.iter().zip(self_times(trace)) {
            let d = by.entry(s.label.clone()).or_insert_with(|| SpanDiff {
                label: s.label.clone(),
                count: (0, 0),
                own: (0.0, 0.0),
            });
            if side == 0 {
                d.count.0 += 1;
                d.own.0 += own;
            } else {
                d.count.1 += 1;
                d.own.1 += own;
            }
        }
    }
    let mut out: Vec<SpanDiff> = by.into_values().collect();
    out.sort_by(|a, b| {
        let change = |d: &SpanDiff| (d.own.1 - d.own.0).abs();
        change(b)
            .total_cmp(&change(a))
            .then_with(|| a.label.cmp(&b.label))
    });
    out
}

/// `label`, cut with `…` to `chars` characters.
fn clip(label: &str, chars: usize) -> String {
    if label.chars().count() <= chars {
        label.to_string()
    } else {
        let cut: String = label.chars().take(chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// A round tick step for a span of `ms`, giving about five ticks.
/// From ten seconds up, steps are clock units (15 s, 1 min, 5 min, 1 h…),
/// so ticks land on times people read, not on 8.3 min.
fn tick_step(ms: f64) -> f64 {
    const CLOCK: [f64; 14] = [
        10e3, 15e3, 30e3, 60e3, 120e3, 300e3, 600e3, 900e3, 1800e3, 3600e3, 7200e3, 10800e3,
        21600e3, 43200e3,
    ];
    let raw = (ms / 5.0).max(1.0);
    if raw > 5e3
        && let Some(step) = CLOCK.iter().copied().find(|s| *s >= raw)
    {
        return step;
    }
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag)
}

/// A tick label: [`duration`] without trailing zeros (`5 s`, `1.5 min`).
fn tick_label(ms: f64) -> String {
    let (value, unit) = match ms.abs() {
        a if a < 1000.0 => return format!("{ms:.0} ms"),
        a if a < 60_000.0 => (ms / 1000.0, "s"),
        a if a < 3_600_000.0 => (ms / 60_000.0, "min"),
        _ => (ms / 3_600_000.0, "h"),
    };
    let text = format!("{value:.2}");
    format!(
        "{} {unit}",
        text.trim_end_matches('0').trim_end_matches('.')
    )
}

const ROW: f64 = 20.0;
const LABELS: f64 = 260.0;
const WIDTH: f64 = 1000.0;
const AXIS: f64 = 22.0;
const PLOT_H: f64 = 110.0;
const PLOT_LEFT: f64 = 56.0;

/// Traces listed before the rest are summarised; the plot shows them all.
const LIST_ROWS: usize = 300;

/// How the trace list is ordered, or grouped.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Order {
    Newest,
    Slowest,
    ByName,
}

/// Minimum-length choices: label and milliseconds.
const MIN_LENGTHS: [(&str, f64); 4] = [
    ("Any length", 0.0),
    ("≥ 10 ms", 10.0),
    ("≥ 1 s", 1000.0),
    ("≥ 1 min", 60_000.0),
];

/// Gridlines for the scatter plot's log axis, with their labels.
const PLOT_TICKS: [(f64, &str); 8] = [
    (1.0, "1 ms"),
    (10.0, "10 ms"),
    (100.0, "100 ms"),
    (1000.0, "1 s"),
    (10_000.0, "10 s"),
    (60_000.0, "1 min"),
    (600_000.0, "10 min"),
    (3_600_000.0, "1 h"),
];

#[component]
pub fn TimelineChart(
    chart: Chart,
    data: PanelData,
    time_zone: String,
    dark: bool,
) -> impl IntoView {
    let Some((layer, d)) = chart
        .layers
        .iter()
        .zip(&data.layers)
        .find(|(l, _)| matches!(l.encoding, Encoding::Timeline { .. }))
    else {
        return ().into_any();
    };
    let all = traces(spans(d, &layer.encoding));
    if all.is_empty() {
        return view! { <div class="no-data">"No spans"</div> }.into_any();
    }
    // Records come newest first up to the layer's limit, so when more
    // matched than arrived, older traces in the range were left out. Without
    // a count from the backend, a limit that's nearly used up is the sign.
    let limit = layer.insight.as_ref().and_then(|i| i.limit);
    let cut = match d.matched {
        Some(matched) => matched > d.rows,
        None => limit.is_some_and(|l| d.rows as f64 >= 0.9 * l as f64),
    };
    let capped = cut.then(|| {
        let since = all.iter().map(|t| t.start).fold(f64::INFINITY, f64::min);
        let of = d.matched.map(|m| format!(" of {m}")).unwrap_or_default();
        let limit = limit
            .map(|l| format!(" (`limit: {l}`)"))
            .unwrap_or_default();
        format!(
            "{}{of} spans: the layer's limit{limit} left out traces before {}.",
            d.rows,
            date_time(since, &time_zone)
        )
    });
    // Colours by value, stable across traces.
    let values: BTreeSet<String> = all
        .iter()
        .flat_map(|t| t.spans.iter().filter_map(|(s, _)| s.color.clone()))
        .collect();
    let palette = categorical(dark);
    let colors: HashMap<String, &'static str> = values
        .into_iter()
        .enumerate()
        .map(|(i, v)| (v, palette[i % palette.len()]))
        .collect();
    let names: BTreeSet<String> = all.iter().map(|t| t.name().to_string()).collect();
    // The scatter plot's axes cover every trace, so filtering doesn't move them.
    let t0 = all.iter().map(|t| t.start).fold(f64::INFINITY, f64::min);
    let t1 = all
        .iter()
        .map(|t| t.start)
        .fold(f64::NEG_INFINITY, f64::max);
    let longest = all.iter().map(|t| t.end - t.start).fold(10.0, f64::max);
    let count = all.len();
    let all = StoredValue::new(all);
    let colors = StoredValue::new(colors);

    let query = RwSignal::new(String::new());
    let min_ms = RwSignal::new(0.0);
    let order = RwSignal::new(Order::Newest);
    let fit = RwSignal::new(false);
    // The selection is a trace id, so re-sorting or filtering keeps it.
    let selected = RwSignal::new(all.with_value(|a| a[0].id.clone()));
    // Folds the viewer changed, per trace.
    let folds: RwSignal<HashMap<String, BTreeSet<String>>> = RwSignal::new(HashMap::new());
    let flame = RwSignal::new(false);
    let critical = RwSignal::new(false);
    // A clicked span's label: every span named like it is highlighted.
    let focus: RwSignal<Option<String>> = RwSignal::new(None);
    // The trace other traces are compared with.
    let baseline: RwSignal<Option<String>> = RwSignal::new(None);
    let needle = Memo::new(move |_| query.get().trim().to_lowercase());

    // Indices of the traces that pass the filters, newest first.
    let shown = Memo::new(move |_| {
        let (q, min) = (needle.get(), min_ms.get());
        all.with_value(|a| {
            a.iter()
                .enumerate()
                .filter(|(_, t)| t.end - t.start >= min && matches(t, &q))
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        })
    });
    // The trace drawn: the selection while it passes the filters, else the first that does.
    let current = Memo::new(move |_| {
        let id = selected.get();
        shown.with(|s| {
            all.with_value(|a| {
                s.iter()
                    .copied()
                    .find(|&i| a[i].id == id)
                    .or(s.first().copied())
            })
        })
    });
    let pick = move |i: usize| {
        focus.set(None);
        selected.set(all.with_value(|a| a[i].id.clone()));
    };

    let list = {
        let time_zone = time_zone.clone();
        move || {
            let current = current.get();
            let indices = shown.get();
            if indices.is_empty() {
                return view! { <div class="no-data small">"No traces match"</div> }.into_any();
            }
            if order.get() == Order::ByName {
                let stats = all.with_value(|a| by_name(indices.iter().map(|&i| &a[i])));
                return stats
                    .into_iter()
                    .map(|s| {
                        let name = s.name.clone();
                        view! {
                            <button class="timeline-trace timeline-name" title="Show only these traces"
                                on:click=move |_| { query.set(name.clone()); order.set(Order::Newest); }>
                                <span class="timeline-trace-name">{s.name}</span>
                                <span class="timeline-trace-length">{format!("{}×", s.count)}</span>
                                <span class="timeline-trace-length">{format!("median {}", duration(s.median))}</span>
                                <span class="timeline-trace-length">{format!("max {}", duration(s.max))}</span>
                                <span class="timeline-trace-length muted">{format!("total {}", duration(s.total))}</span>
                            </button>
                        }
                    })
                    .collect_view()
                    .into_any();
            }
            let base = baseline.get();
            let mut rows: Vec<(usize, f64, String, f64, usize, bool)> = all.with_value(|a| {
                indices
                    .iter()
                    .map(|&i| {
                        let t = &a[i];
                        (
                            i,
                            t.start,
                            t.name().to_string(),
                            t.end - t.start,
                            t.spans.len(),
                            base.as_deref() == Some(t.id.as_str()),
                        )
                    })
                    .collect()
            });
            if order.get() == Order::Slowest {
                rows.sort_by(|a, b| b.3.total_cmp(&a.3));
            }
            let more = rows.len().saturating_sub(LIST_ROWS);
            let footer = (more > 0).then(|| {
                view! { <div class="muted small timeline-more">{format!("{more} more: filter or pick from the plot")}</div> }
            });
            let rows = rows
                .into_iter()
                .take(LIST_ROWS)
                .map(|(i, start, name, length, spans, is_base)| {
                    let class = match (Some(i) == current, is_base) {
                        (true, _) => "timeline-trace selected",
                        (false, true) => "timeline-trace baseline",
                        (false, false) => "timeline-trace",
                    };
                    view! {
                        <button class=class on:click=move |_| pick(i)>
                            <span class="timeline-trace-when">{date_time(start, &time_zone)}</span>
                            <span class="timeline-trace-name">{name}</span>
                            <span class="timeline-trace-length">{duration(length)}</span>
                            <span class="muted">{format!("{spans} spans")}</span>
                        </button>
                    }
                })
                .collect_view();
            view! { {rows}{footer} }.into_any()
        }
    };

    // Jaeger's view: every trace as a dot, start time across, length up (log).
    let plot = {
        let time_zone = time_zone.clone();
        move || {
            let current = current.get();
            let base = baseline.get();
            let (top, bottom) = (8.0, PLOT_H - 18.0);
            let ymax = longest.log10();
            let y = move |ms: f64| bottom - (ms.max(1.0).log10() / ymax) * (bottom - top);
            let x = move |at: f64| {
                PLOT_LEFT
                    + 6.0
                    + if t1 > t0 {
                        (at - t0) / (t1 - t0) * (WIDTH - PLOT_LEFT - 12.0)
                    } else {
                        0.0
                    }
            };
            let grid: Vec<_> = PLOT_TICKS
                .iter()
                .filter(|(ms, _)| ms.log10() <= ymax)
                .map(|&(ms, label)| {
                    let gy = y(ms);
                    view! {
                        <line x1=PLOT_LEFT x2=WIDTH y1=gy y2=gy class="timeline-grid"/>
                        <text x=PLOT_LEFT - 6.0 y=gy + 3.0 class="timeline-tick" text-anchor="end">{label}</text>
                    }
                })
                .collect();
            let dots: Vec<_> = all.with_value(|a| {
                let mut order: Vec<usize> = shown.get();
                // The selected dot is drawn last, on top.
                order.sort_by_key(|&i| Some(i) == current);
                order
                    .into_iter()
                    .map(|i| {
                        let t = &a[i];
                        let len = t.end - t.start;
                        let chosen = Some(i) == current;
                        let class = match (chosen, base.as_deref() == Some(t.id.as_str())) {
                            (true, _) => "timeline-dot selected",
                            (false, true) => "timeline-dot baseline",
                            (false, false) => "timeline-dot",
                        };
                        let title = format!(
                            "{}\n{}\n{} · {} spans",
                            t.name(),
                            date_time(t.start, &time_zone),
                            duration(len),
                            t.spans.len()
                        );
                        view! {
                            <circle cx=x(t.start) cy=y(len) r=if chosen { 5.0 } else { 3.0 }
                                class=class
                                on:click=move |_| pick(i)>
                                <title>{title}</title>
                            </circle>
                        }
                    })
                    .collect()
            });
            view! {
                <svg viewBox=format!("0 0 {WIDTH} {PLOT_H}") class="timeline-plot" preserveAspectRatio="xMinYMin meet">
                    {grid}
                    <text x=PLOT_LEFT + 6.0 y=PLOT_H - 4.0 class="timeline-tick">{date_time(t0, &time_zone)}</text>
                    <text x=WIDTH - 6.0 y=PLOT_H - 4.0 class="timeline-tick" text-anchor="end">{date_time(t1, &time_zone)}</text>
                    {dots}
                </svg>
            }
        }
    };

    let body = {
        let time_zone = time_zone.clone();
        move || {
            let Some(index) = current.get() else {
                return ().into_any();
            };
            let trace = all.with_value(|a| a[index].clone());
            let base = baseline
                .get()
                .and_then(|id| all.with_value(|a| a.iter().find(|t| t.id == id).cloned()));
            let other = base.as_ref().filter(|b| b.id != trace.id);
            // A clicked span wins over the filter. The filter highlights
            // spans only when it matched inside the trace: matching the
            // trace's own name says nothing about its spans.
            let highlight = match focus.get() {
                Some(label) => Highlight::Label(label),
                None => Some(needle.get())
                    .filter(|q| !q.is_empty() && !trace.name().to_lowercase().contains(q.as_str()))
                    .map_or(Highlight::None, Highlight::Contains),
            };
            let len = other
                .map_or(0.0, |b| b.end - b.start)
                .max(trace.end - trace.start);
            let draw = Draw {
                colors: colors.get_value(),
                fallback: palette[0],
                highlight,
                critical: critical.get(),
                flame: flame.get(),
                axis: axis_ms(len, fit.get()),
                folds,
                focus,
                time_zone: time_zone.clone(),
            };
            let pressed = |on: bool| if on { "selected" } else { "" };
            let compare_controls = match (&base, other) {
                (None, _) => {
                    let id = trace.id.clone();
                    view! {
                        <button on:click=move |_| baseline.set(Some(id.clone()))
                            title="Keep this trace as the baseline, then pick another to compare it with">"Compare…"</button>
                    }
                    .into_any()
                }
                (Some(_), None) => view! {
                    <span class="muted small">"Baseline set: pick another trace to compare with it"</span>
                    <button on:click=move |_| baseline.set(None)>"Cancel"</button>
                }
                .into_any(),
                (Some(b), Some(_)) => {
                    let (this, that) = (trace.id.clone(), b.id.clone());
                    view! {
                        <button title="Make this trace the baseline"
                            on:click=move |_| { baseline.set(Some(this.clone())); selected.set(that.clone()); }>"Swap"</button>
                        <button on:click=move |_| baseline.set(None)>"Stop comparing"</button>
                    }
                    .into_any()
                }
            };
            let focused = focus.get().map(|label| {
                view! {
                    <span class="timeline-focus small">
                        {format!("Highlighting “{label}”")}
                        <button title="Stop highlighting" on:click=move |_| focus.set(None)>"×"</button>
                    </span>
                }
            });
            view! {
                <div class="timeline-controls">
                    <span class="timeline-sort">
                        <button class=pressed(!draw.flame) on:click=move |_| flame.set(false)
                            title="One row per span, nested">"Waterfall"</button>
                        <button class=pressed(draw.flame) on:click=move |_| flame.set(true)
                            title="Spans stacked by depth; siblings that ran side by side get extra lanes">"Flame graph"</button>
                    </span>
                    <span class="timeline-sort">
                        <button class=move || pressed(!fit.get()) on:click=move |_| fit.set(false)
                            title=format!("Axis at least {}, so short traces look short", duration(MIN_AXIS_MS))>"True scale"</button>
                        <button class=move || pressed(fit.get()) on:click=move |_| fit.set(true)
                            title="Stretch the trace across the width">"Fit trace"</button>
                    </span>
                    <span class="timeline-sort">
                        <button class=pressed(draw.critical) on:click=move |_| critical.update(|c| *c = !*c)
                            title="The chain of spans the trace's end waited on: making anything else faster wouldn't have made the trace shorter">"Critical path"</button>
                    </span>
                    <span class="timeline-sort">{compare_controls}</span>
                    {focused}
                </div>
                {other.map(|b| compare_view(b, &trace))}
                {other.map(|b| trace_block(b, &draw, Some("Baseline")))}
                {trace_block(&trace, &draw, other.map(|_| "This trace"))}
            }
            .into_any()
        }
    };
    let total_label = move || {
        let n = shown.with(Vec::len);
        if n == count {
            format!("{count} {}", if count == 1 { "trace" } else { "traces" })
        } else {
            format!("{n} of {count} traces")
        }
    };
    view! {
        <div class="timeline">
            <div class="timeline-head">
                <input class="timeline-filter" type="search" list=format!("timeline-names-{}", chart_key(&names))
                    placeholder="Filter by trace or span name"
                    prop:value=move || query.get()
                    on:input=move |ev| query.set(event_target_value(&ev))/>
                <datalist id=format!("timeline-names-{}", chart_key(&names))>
                    {names.iter().map(|n| view! { <option value=n.clone()/> }).collect_view()}
                </datalist>
                <span class="timeline-sort">
                    {MIN_LENGTHS.iter().map(|&(label, ms)| view! {
                        <button class=move || if min_ms.get() == ms { "selected" } else { "" } on:click=move |_| min_ms.set(ms)>{label}</button>
                    }).collect_view()}
                </span>
                <span class="muted small timeline-count">{total_label}</span>
                <span class="timeline-sort">
                    {[(Order::Newest, "Newest"), (Order::Slowest, "Slowest"), (Order::ByName, "By name")].into_iter().map(|(o, label)| view! {
                        <button class=move || if order.get() == o { "selected" } else { "" } on:click=move |_| order.set(o)>{label}</button>
                    }).collect_view()}
                </span>
            </div>
            {capped.map(|note| view! { <div class="timeline-capped small">{note}</div> })}
            {plot}
            <div class="timeline-traces">{list}</div>
            {body}
        </div>
    }
    .into_any()
}

/// What traces are drawn with: shared by compared traces, so they use one
/// scale.
#[derive(Clone)]
struct Draw {
    colors: HashMap<String, &'static str>,
    fallback: &'static str,
    highlight: Highlight,
    critical: bool,
    flame: bool,
    /// The axis length in ms.
    axis: f64,
    /// Folds the viewer changed, per trace.
    folds: RwSignal<HashMap<String, BTreeSet<String>>>,
    focus: RwSignal<Option<String>>,
    time_zone: String,
}

impl Draw {
    fn fill(&self, s: &Span) -> &'static str {
        s.color
            .as_ref()
            .and_then(|c| self.colors.get(c).copied())
            .unwrap_or(self.fallback)
    }

    fn class(&self, label: &str) -> &'static str {
        match self.highlight.hit(label) {
            None => "timeline-row",
            Some(true) => "timeline-row hit",
            Some(false) => "timeline-row dim",
        }
    }

    /// Clicking a span highlights every span named like it; clicking one
    /// of them again stops.
    fn focus_on(&self, label: &str) -> impl Fn(leptos::ev::MouseEvent) + 'static {
        let (focus, label) = (self.focus, label.to_string());
        move |_| {
            focus.update(|f| {
                *f = if f.as_deref() == Some(label.as_str()) {
                    None
                } else {
                    Some(label.clone())
                }
            })
        }
    }

    /// Gridlines and labels from `left`, down to `bottom`.
    fn ticks(&self, left: f64, scale: f64, bottom: f64) -> Vec<AnyView> {
        let step = tick_step(self.axis);
        (0..)
            .map(|i| i as f64 * step)
            .take_while(|t| *t <= self.axis * 1.0001)
            .map(|t| {
                let x = left + t * scale;
                // Ticks at the edges are labelled inwards, so they aren't cut off.
                let anchor = match x {
                    x if x > WIDTH - 30.0 => "end",
                    x if x < 30.0 => "start",
                    _ => "middle",
                };
                view! {
                    <line x1=x x2=x y1=AXIS - 4.0 y2=bottom class="timeline-grid"/>
                    <text x=x y=AXIS - 8.0 class="timeline-tick" text-anchor=anchor>{tick_label(t)}</text>
                }
                .into_any()
            })
            .collect()
    }
}

/// A span's tooltip.
fn span_title(trace: &Trace, s: &Span, own: f64) -> String {
    format!(
        "{}\n{} (own time {})\nstarts at +{}{}\nClick to highlight spans with this name",
        s.label,
        duration(s.end - s.start),
        duration(own),
        duration(s.start - trace.start),
        s.color
            .as_ref()
            .map(|c| format!("\n{c}"))
            .unwrap_or_default()
    )
}

/// Critical-path segments by span index.
fn segments_by_span(path: &[(usize, f64, f64)]) -> HashMap<usize, Vec<(f64, f64)>> {
    let mut by: HashMap<usize, Vec<(f64, f64)>> = HashMap::new();
    for &(i, a, b) in path {
        by.entry(i).or_default().push((a, b));
    }
    by
}

/// The critical path as a line through the bars of one row.
fn critical_marks(
    segments: &[(f64, f64)],
    origin: f64,
    left: f64,
    scale: f64,
    middle: f64,
) -> Vec<AnyView> {
    segments
        .iter()
        .map(|&(a, b)| {
            let x = left + (a - origin) * scale;
            let w = ((b - a) * scale).max(1.0);
            view! { <rect x=x y=middle - 2.0 width=w height=4.0 rx=1 class="timeline-critical"/> }
                .into_any()
        })
        .collect()
}

/// One trace: a heading, its waterfall or flame graph, and the time legends.
fn trace_block(trace: &Trace, draw: &Draw, heading: Option<&'static str>) -> AnyView {
    let len = trace.end - trace.start;
    let path = if draw.critical {
        critical_path(trace)
    } else {
        vec![]
    };
    let below = descendants(trace);
    let folded = draw
        .folds
        .with(|f| f.get(&trace.id).cloned())
        .unwrap_or_else(|| initial_folds(trace, &draw.highlight));
    let chart = if draw.flame {
        flame_view(trace, draw, &path)
    } else {
        waterfall_view(trace, draw, &path, &folded, &below)
    };
    let fold_buttons = (!draw.flame && below.iter().any(|n| *n > 0)).then(|| {
        let (folds, id) = (draw.folds, trace.id.clone());
        let parents: BTreeSet<String> = trace
            .spans
            .iter()
            .zip(&below)
            .filter(|(_, n)| **n > 0)
            .map(|((s, _), _)| s.id.clone())
            .collect();
        let (open, shut) = (id.clone(), id);
        view! {
            <span class="timeline-sort">
                <button disabled=folded.is_empty()
                    on:click=move |_| folds.update(|f| { f.insert(open.clone(), BTreeSet::new()); })>"Expand all"</button>
                <button on:click=move |_| folds.update(|f| { f.insert(shut.clone(), parents.clone()); })>"Collapse all"</button>
            </span>
        }
    });
    let key = |value: &str, ms: f64, share: f64| {
        let fill = draw.colors.get(value).copied().unwrap_or(draw.fallback);
        view! {
            <span class="timeline-key">
                <span class="timeline-swatch" style=format!("background:{fill}")></span>
                {format!("{value} {} ({share:.0}%)", duration(ms))}
            </span>
        }
    };
    let by_color = time_by_color(trace);
    // Shares of all own time, which add up to 100% even when spans ran
    // side by side and own time adds up to more than the trace's length.
    let work: f64 = by_color.iter().map(|(_, ms)| ms).sum();
    let parallel = (len > 0.0 && work > len * 1.05).then(|| {
        format!(
            " Spans overlapped: own time adds up to {:.1}× the trace's length.",
            work / len
        )
    });
    let legend: Vec<_> = by_color
        .iter()
        .map(|(value, ms)| key(value, *ms, 100.0 * ms / work.max(1e-9)))
        .collect();
    let critical_legend = draw.critical.then(|| {
        let keys: Vec<_> = critical_by_color(trace, &path)
            .iter()
            .map(|(value, ms)| key(value, *ms, 100.0 * ms / len.max(1e-9)))
            .collect();
        view! { <div class="timeline-legend muted small">"Critical path: "{keys}</div> }
    });
    view! {
        <div class="timeline-title">
            {heading.map(|h| view! { <span class="timeline-badge">{h}</span> })}
            <strong>{trace.name().to_string()}</strong>
            <span class="muted small">{format!("{} · {} · {} spans", date_time(trace.start, &draw.time_zone), duration(len), trace.spans.len())}</span>
            {fold_buttons}
        </div>
        {chart}
        <div class="timeline-legend muted small">"Own time: "{legend}{parallel}</div>
        {critical_legend}
    }
    .into_any()
}

/// One row per span, nested, with folding.
fn waterfall_view(
    trace: &Trace,
    draw: &Draw,
    path: &[(usize, f64, f64)],
    folded: &BTreeSet<String>,
    below: &[usize],
) -> AnyView {
    let rows_shown = visible(trace, folded);
    let scale = (WIDTH - LABELS - 10.0) / draw.axis;
    let bottom = AXIS + rows_shown.len() as f64 * ROW;
    let ticks = draw.ticks(LABELS, scale, bottom);
    let own = self_times(trace);
    let segments = segments_by_span(path);
    let rows: Vec<_> = rows_shown
        .iter()
        .enumerate()
        .map(|(row, &i)| {
            let (s, depth) = &trace.spans[i];
            let y = AXIS + row as f64 * ROW;
            let x = LABELS + (s.start - trace.start) * scale;
            let w = ((s.end - s.start) * scale).max(1.5);
            let indent = 4.0 + *depth as f64 * 12.0;
            let dur = duration(s.end - s.start);
            // The duration sits after the bar, or inside it near the right edge.
            let (tx, anchor) = if x + w + 60.0 < WIDTH {
                (x + w + 4.0, "start")
            } else {
                (x + w - 4.0, "end")
            };
            let is_folded = folded.contains(&s.id);
            let hidden = below[i];
            let room = ((LABELS - indent - 14.0) / 6.2).max(4.0) as usize;
            let label = if is_folded {
                format!("{} (+{hidden})", clip(&s.label, room.saturating_sub(7)))
            } else {
                clip(&s.label, room)
            };
            // A folded span carries its hidden descendants' part of the path.
            let reach = if is_folded { i + hidden } else { i };
            let marks: Vec<_> = (i..=reach)
                .filter_map(|k| segments.get(&k))
                .flat_map(|segs| critical_marks(segs, trace.start, LABELS, scale, y + ROW / 2.0))
                .collect();
            let toggle = (hidden > 0).then(|| {
                let (folds, span, trace_id, folded) =
                    (draw.folds, s.id.clone(), trace.id.clone(), folded.clone());
                view! {
                    <text x=indent y=y + 14.0 class="timeline-fold"
                        on:click=move |_| {
                            let mut f = folded.clone();
                            if !f.remove(&span) {
                                f.insert(span.clone());
                            }
                            folds.update(|all| { all.insert(trace_id.clone(), f); });
                        }>
                        {if is_folded { "▸" } else { "▾" }}
                    </text>
                }
            });
            view! {
                <g class=draw.class(&s.label)>
                    <title>{span_title(trace, s, own[i])}</title>
                    {toggle}
                    <text x=indent + 12.0 y=y + 14.0 class="timeline-label" on:click=draw.focus_on(&s.label)>{label}</text>
                    <rect x=x y=y + 3.0 width=w height=ROW - 6.0 rx=2 fill=draw.fill(s) on:click=draw.focus_on(&s.label)/>
                    {marks}
                    <text x=tx y=y + 14.0 class="timeline-duration" text-anchor=anchor>{dur}</text>
                </g>
            }
        })
        .collect();
    let height = bottom + 4.0;
    view! {
        <svg viewBox=format!("0 0 {WIDTH} {height}") class="timeline-svg" preserveAspectRatio="xMinYMin meet">
            {ticks}
            {rows}
        </svg>
    }
    .into_any()
}

const FLAME_ROW: f64 = 18.0;
const FLAME_LEFT: f64 = 4.0;

/// Spans stacked by depth across the whole width, labelled inside the bars.
fn flame_view(trace: &Trace, draw: &Draw, path: &[(usize, f64, f64)]) -> AnyView {
    let (rows, count) = flame_rows(trace);
    let scale = (WIDTH - 2.0 * FLAME_LEFT) / draw.axis;
    let bottom = AXIS + count as f64 * FLAME_ROW;
    let ticks = draw.ticks(FLAME_LEFT, scale, bottom);
    let own = self_times(trace);
    let segments = segments_by_span(path);
    let bars: Vec<_> = trace
        .spans
        .iter()
        .enumerate()
        .map(|(i, (s, _))| {
            let y = AXIS + rows[i] as f64 * FLAME_ROW;
            let x = FLAME_LEFT + (s.start - trace.start) * scale;
            let w = ((s.end - s.start) * scale).max(1.0);
            let label = (w > 30.0).then(|| {
                view! {
                    <text x=x + 4.0 y=y + 12.0 class="timeline-flame-label">{clip(&s.label, ((w - 8.0) / 6.0) as usize)}</text>
                }
            });
            let marks = segments
                .get(&i)
                .map(|segs| critical_marks(segs, trace.start, FLAME_LEFT, scale, y + FLAME_ROW - 4.0));
            view! {
                <g class=draw.class(&s.label)>
                    <title>{span_title(trace, s, own[i])}</title>
                    <rect x=x y=y + 1.0 width=w height=FLAME_ROW - 2.0 rx=1 fill=draw.fill(s)
                        class="timeline-flame-bar" on:click=draw.focus_on(&s.label)/>
                    {label}
                    {marks}
                </g>
            }
        })
        .collect();
    let height = bottom + 4.0;
    view! {
        <svg viewBox=format!("0 0 {WIDTH} {height}") class="timeline-svg" preserveAspectRatio="xMinYMin meet">
            {ticks}
            {bars}
        </svg>
    }
    .into_any()
}

/// Rows of the comparison table before the rest are summarised.
const DIFF_ROWS: usize = 12;

/// `+1.20 s` or `−850 ms`.
fn signed(ms: f64) -> String {
    let sign = if ms < 0.0 { "−" } else { "+" };
    format!("{sign}{}", duration(ms.abs()))
}

/// How a trace differs from the baseline: its length, and the span names
/// whose own time changed most.
fn compare_view(baseline: &Trace, other: &Trace) -> AnyView {
    let (was, is) = (baseline.end - baseline.start, other.end - other.start);
    let pct = if was > 0.0 {
        format!(" ({:+.0}%)", 100.0 * (is - was) / was).replace('-', "−")
    } else {
        String::new()
    };
    let verdict = if is > was { "slower" } else { "faster" };
    let diff = compare(baseline, other);
    let more = diff.len().saturating_sub(DIFF_ROWS);
    let rows: Vec<_> = diff
        .into_iter()
        .take(DIFF_ROWS)
        .map(|d| {
            let change = d.own.1 - d.own.0;
            let class = match change {
                c if c > 1.0 => "slower",
                c if c < -1.0 => "faster",
                _ => "muted",
            };
            let count = if d.count.0 == d.count.1 {
                d.count.0.to_string()
            } else {
                format!("{} → {}", d.count.0, d.count.1)
            };
            view! {
                <tr>
                    <td>{d.label}</td>
                    <td class="num">{count}</td>
                    <td class="num">{duration(d.own.0)}</td>
                    <td class="num">{duration(d.own.1)}</td>
                    <td class=format!("num {class}")>{signed(change)}</td>
                </tr>
            }
        })
        .collect();
    view! {
        <div class="timeline-compare">
            <div class="small">
                {format!("This trace took {} against the baseline's {}: ", duration(is), duration(was))}
                <strong class=verdict>{format!("{}{pct}", signed(is - was))}</strong>
            </div>
            <table class="timeline-diff small">
                <thead>
                    <tr><th>"Span"</th><th class="num">"Count"</th><th class="num">"Own time, baseline"</th><th class="num">"Own time, this trace"</th><th class="num">"Change"</th></tr>
                </thead>
                <tbody>{rows}</tbody>
            </table>
            {(more > 0).then(|| view! { <div class="muted small">{format!("and {more} more span names with smaller changes")}</div> })}
        </div>
    }
    .into_any()
}

/// A short id for a set of names, so two timeline panels' datalists don't clash.
fn chart_key(names: &BTreeSet<String>) -> u64 {
    names.iter().fold(1469598103934665603u64, |h, n| {
        n.bytes()
            .fold(h, |h, b| (h ^ b as u64).wrapping_mul(1099511628211))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(
        trace: &str,
        id: &str,
        parent: Option<&str>,
        start: f64,
        end: f64,
        color: &str,
    ) -> Span {
        Span {
            trace: trace.into(),
            id: id.into(),
            parent: parent.map(String::from),
            start,
            end,
            label: id.into(),
            color: Some(color.into()),
        }
    }

    #[test]
    fn traces_are_newest_first_and_depth_first() {
        let t = traces(vec![
            span("a", "tool2", Some("root"), 60.0, 90.0, "tool"),
            span("a", "root", None, 0.0, 100.0, "interaction"),
            span("a", "llm", Some("root"), 0.0, 50.0, "llm"),
            span("a", "exec", Some("tool2"), 70.0, 90.0, "exec"),
            span("b", "later", None, 500.0, 510.0, "interaction"),
            span("a", "orphan", Some("missing"), 95.0, 99.0, "x"),
        ]);
        assert_eq!(
            t.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        let order: Vec<(&str, usize)> = t[1]
            .spans
            .iter()
            .map(|(s, d)| (s.id.as_str(), *d))
            .collect();
        assert_eq!(
            order,
            [
                ("root", 0),
                ("llm", 1),
                ("tool2", 1),
                ("exec", 2),
                ("orphan", 0)
            ]
        );
        assert_eq!(t[1].name(), "root");
        assert_eq!((t[1].start, t[1].end), (0.0, 100.0));
    }

    #[test]
    fn own_time_excludes_children_and_adds_up() {
        let t = &traces(vec![
            span("a", "root", None, 0.0, 100.0, "interaction"),
            span("a", "llm", Some("root"), 0.0, 50.0, "llm"),
            // Overlapping children are counted once.
            span("a", "tool", Some("root"), 40.0, 80.0, "tool"),
            span("a", "exec", Some("tool"), 45.0, 75.0, "exec"),
        ])[0];
        assert_eq!(self_times(t), [20.0, 50.0, 10.0, 30.0]);
        let by = time_by_color(t);
        assert_eq!(by[0], ("llm".into(), 50.0));
        assert_eq!(
            by.iter().map(|(_, ms)| ms).sum::<f64>(),
            110.0,
            "overlapping siblings both count"
        );
    }

    #[test]
    fn cycles_in_bad_data_still_draw_every_span_once() {
        let t = &traces(vec![
            span("a", "x", Some("y"), 0.0, 1.0, "c"),
            span("a", "y", Some("x"), 0.0, 1.0, "c"),
        ])[0];
        assert_eq!(t.spans.len(), 2);
    }

    #[test]
    fn trace_times_show_the_time_of_day() {
        assert_eq!(
            date_time(1_794_805_910_744.0, "UTC"),
            "2026-11-16 05:11:50 UTC"
        );
        assert_eq!(
            date_time(1_794_805_910_744.0, "America/New_York"),
            "2026-11-16 00:11:50 EST"
        );
    }

    #[test]
    fn short_traces_keep_a_true_scale_unless_fitted() {
        assert_eq!(axis_ms(0.0, false), 100.0, "a 0 ms trace is a hairline");
        assert_eq!(axis_ms(5.0, false), 100.0);
        assert_eq!(axis_ms(5.0, true), 5.0);
        assert_eq!(
            axis_ms(1060.0, false),
            1100.0,
            "rounded up to a fifth of a tick"
        );
        assert_eq!(axis_ms(7301.0, true), 7600.0);
    }

    #[test]
    fn the_name_filter_matches_any_span_ignoring_case() {
        let t = &traces(vec![
            span("a", "ggh · rescan", None, 0.0, 10.0, "run"),
            span("a", "git status", Some("ggh · rescan"), 1.0, 2.0, "git"),
        ])[0];
        assert!(matches(t, ""));
        assert!(matches(t, "rescan"));
        assert!(matches(t, "git status"));
        assert!(!matches(t, "interaction"));
        // The caller lower-cases the query; labels are compared lower-cased.
        assert!(matches(t, "ggh"));
    }

    #[test]
    fn traces_group_by_name_with_most_total_time_first() {
        let t = traces(vec![
            span("a", "poll", None, 0.0, 10.0, "c"),
            span("b", "poll", None, 100.0, 130.0, "c"),
            span("c", "poll", None, 200.0, 220.0, "c"),
            span("d", "interaction", None, 300.0, 400.0, "c"),
        ]);
        let by = by_name(&t);
        assert_eq!(by[0].name, "interaction");
        assert_eq!(
            (
                by[1].name.as_str(),
                by[1].count,
                by[1].median,
                by[1].max,
                by[1].total
            ),
            ("poll", 3, 20.0, 30.0, 60.0)
        );
    }

    /// A fan-out like `ggh · rescan`: one root, `steps` steps, each with `leaves` leaves.
    fn fan_out(steps: usize, leaves: usize) -> Trace {
        let mut spans = vec![span("t", "root", None, 0.0, 1000.0, "run")];
        for s in 0..steps {
            let step = format!("step{s}");
            spans.push(span(
                "t",
                &step,
                Some("root"),
                s as f64,
                s as f64 + 10.0,
                "step",
            ));
            for l in 0..leaves {
                spans.push(span(
                    "t",
                    &format!("{step}-leaf{l}"),
                    Some(&step),
                    s as f64,
                    s as f64 + 1.0,
                    "leaf",
                ));
            }
        }
        traces(spans).remove(0)
    }

    #[test]
    fn small_traces_open_unfolded() {
        let t = fan_out(2, 3);
        assert!(initial_folds(&t, &Highlight::None).is_empty());
        assert_eq!(visible(&t, &BTreeSet::new()).len(), 9);
    }

    #[test]
    fn large_traces_fold_to_the_deepest_level_that_fits() {
        let t = fan_out(6, 70);
        assert_eq!(descendants(&t)[0], 6 * 71);
        let folds = initial_folds(&t, &Highlight::None);
        assert_eq!(folds.len(), 6, "every step folds, the root stays open");
        assert_eq!(visible(&t, &folds).len(), 7);
    }

    #[test]
    fn folds_never_hide_a_span_matching_the_filter() {
        let t = fan_out(6, 70);
        let folds = initial_folds(&t, &Highlight::Contains("step3-leaf42".into()));
        assert!(!folds.contains("step3"));
        let shown = visible(&t, &folds);
        assert!(shown.iter().any(|&i| t.spans[i].0.id == "step3-leaf42"));
        assert_eq!(shown.len(), 7 + 70);
    }

    #[test]
    fn clicking_a_span_highlights_its_namesakes_and_keeps_them_unfolded() {
        let t = fan_out(6, 70);
        let h = Highlight::Label("step5-leaf3".into());
        assert_eq!(h.hit("step5-leaf3"), Some(true));
        assert_eq!(h.hit("step5-leaf30"), Some(false), "exact, not a prefix");
        assert_eq!(Highlight::None.hit("x"), None);
        assert!(!initial_folds(&t, &h).contains("step5"));
    }

    /// The critical path of `trace` as (span id, from, to).
    fn path(t: &Trace) -> Vec<(&str, f64, f64)> {
        critical_path(t)
            .into_iter()
            .map(|(i, a, b)| (t.spans[i].0.id.as_str(), a, b))
            .collect()
    }

    #[test]
    fn the_critical_path_follows_the_last_child_to_finish() {
        // root 0–100: llm 0–40, then two tools side by side, 40–90 and 45–70.
        let t = &traces(vec![
            span("a", "root", None, 0.0, 100.0, "interaction"),
            span("a", "llm", Some("root"), 0.0, 40.0, "llm"),
            span("a", "slow", Some("root"), 40.0, 90.0, "tool"),
            span("a", "fast", Some("root"), 45.0, 70.0, "tool"),
            span("a", "exec", Some("slow"), 50.0, 80.0, "exec"),
        ])[0];
        assert_eq!(
            path(t),
            [
                ("llm", 0.0, 40.0),
                ("slow", 40.0, 50.0),
                ("exec", 50.0, 80.0),
                ("slow", 80.0, 90.0),
                ("root", 90.0, 100.0),
            ],
            "`fast` ran alongside `slow`, so it isn't on the path"
        );
        let total: f64 = critical_path(t).iter().map(|(_, a, b)| b - a).sum();
        assert_eq!(total, 100.0, "the path covers the whole trace");
        assert_eq!(
            critical_by_color(t, &critical_path(t)),
            [
                ("llm".into(), 40.0),
                ("exec".into(), 30.0),
                ("tool".into(), 20.0),
                ("interaction".into(), 10.0)
            ]
        );
    }

    #[test]
    fn work_that_outlives_its_parent_stays_on_the_critical_path() {
        // The interaction ends at 10, but the subagent it started runs to 100.
        let t = &traces(vec![
            span("a", "interaction", None, 0.0, 10.0, "interaction"),
            span("a", "agent", Some("interaction"), 5.0, 8.0, "tool"),
            span("a", "model", Some("agent"), 20.0, 100.0, "llm"),
        ])[0];
        assert_eq!(
            path(t),
            [
                ("interaction", 0.0, 5.0),
                ("agent", 5.0, 8.0),
                ("model", 20.0, 100.0),
            ],
            "only time inside a span counts as its own; 8–20 waited on nothing recorded"
        );
    }

    #[test]
    fn a_gap_between_children_is_the_parents_own_time() {
        let t = &traces(vec![
            span("a", "root", None, 0.0, 30.0, "r"),
            span("a", "x", Some("root"), 0.0, 10.0, "c"),
            span("a", "y", Some("root"), 20.0, 30.0, "c"),
        ])[0];
        assert_eq!(
            path(t),
            [("x", 0.0, 10.0), ("root", 10.0, 20.0), ("y", 20.0, 30.0)]
        );
    }

    #[test]
    fn flame_rows_stack_parallel_siblings_into_lanes() {
        let t = &traces(vec![
            span("a", "root", None, 0.0, 100.0, "r"),
            span("a", "p1", Some("root"), 0.0, 50.0, "c"),
            span("a", "p2", Some("root"), 10.0, 60.0, "c"),
            span("a", "after", Some("root"), 60.0, 90.0, "c"),
            span("a", "leaf", Some("p1"), 5.0, 6.0, "c"),
        ])[0];
        let (rows, n) = flame_rows(t);
        let row = |id: &str| rows[t.spans.iter().position(|(s, _)| s.id == id).unwrap()];
        assert_eq!(
            (row("root"), row("p1"), row("p2"), row("after")),
            (0, 1, 2, 1)
        );
        assert_eq!(row("leaf"), 3, "the next depth sits below both lanes");
        assert_eq!(n, 4);
    }

    #[test]
    fn comparing_traces_ranks_span_names_by_change_in_own_time() {
        let a = &traces(vec![
            span("a", "root", None, 0.0, 100.0, "r"),
            span("a", "git", Some("root"), 0.0, 10.0, "c"),
            span("a", "gh", Some("root"), 10.0, 40.0, "c"),
        ])[0];
        let mut b = traces(vec![
            span("b", "root", None, 0.0, 200.0, "r"),
            span("b", "git", Some("root"), 0.0, 12.0, "c"),
            span("b", "gh", Some("root"), 12.0, 140.0, "c"),
            span("b", "gh2", Some("root"), 140.0, 150.0, "c"),
        ]);
        // Same label for both gh spans, as in a fan-out.
        b[0].spans[3].0.label = "gh".into();
        let diff = compare(a, &b[0]);
        assert_eq!(diff[0].label, "gh");
        assert_eq!((diff[0].count, diff[0].own), ((1, 2), (30.0, 138.0)));
        let names: Vec<&str> = diff.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(names, ["gh", "root", "git"]);
    }

    #[test]
    fn long_labels_are_clipped() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(
            clip("gh search issues → api.github.com", 12),
            "gh search i…"
        );
    }

    #[test]
    fn durations_and_ticks_read_well() {
        assert_eq!(duration(850.0), "850 ms");
        assert_eq!(duration(1250.0), "1.25 s");
        assert_eq!(duration(144_000.0), "2.4 min");
        assert_eq!(tick_step(7301.0), 2000.0);
        assert_eq!(tick_step(40.0), 10.0);
        assert_eq!(
            tick_step(19.7 * 60_000.0),
            300_000.0,
            "a 20 min trace ticks every 5 min"
        );
        assert_eq!(tick_step(30.4 * 60_000.0), 600_000.0);
        assert_eq!(tick_step(14_980.0), 5000.0);
        assert_eq!(tick_label(0.0), "0 ms");
        assert_eq!(tick_label(5000.0), "5 s");
        assert_eq!(tick_label(300_000.0), "5 min");
        assert_eq!(tick_label(90_000.0), "1.5 min");
    }
}
