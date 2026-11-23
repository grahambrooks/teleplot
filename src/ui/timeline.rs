//! A trace timeline: the spans of one trace as nested bars on a time axis,
//! like an OpenTelemetry trace view. One row per span arrives (a records
//! layer); rows are grouped into traces, newest first.
//!
//! Finding the trace, after Jaeger and Tempo: a name filter (matching any
//! span's label), a minimum length, a scatter plot of every trace's length
//! over time to click outliers in, and a list that is newest first, slowest
//! first, or one row per name with counts and typical lengths.
//!
//! Reading it: the axis is at least [`MIN_AXIS_MS`] long so short traces
//! look short ("Fit trace" stretches one). Large traces open folded to the
//! deepest level that fits in [`FOLD_AT`] rows, and spans matching the
//! filter are highlighted and never folded away. Below the bars, each
//! `color` value's own time: a span's duration less the time its children
//! cover. When sibling spans don't overlap, the totals add up to the
//! trace's length.

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

/// The spans folded when a trace opens: none if it fits in [`FOLD_AT`]
/// rows; otherwise every parent from the shallowest depth that does fit,
/// so as many levels show as fit. Parents of spans matching `query` stay
/// open, so a match is never hidden.
pub fn initial_folds(trace: &Trace, query: &str) -> BTreeSet<String> {
    if trace.spans.len() <= FOLD_AT {
        return BTreeSet::new();
    }
    let below = descendants(trace);
    let mut keep = BTreeSet::new();
    if !query.is_empty() {
        let mut path: Vec<usize> = vec![];
        for (i, (s, depth)) in trace.spans.iter().enumerate() {
            path.truncate(*depth);
            if s.label.to_lowercase().contains(query) {
                keep.extend(path.iter().copied());
            }
            path.push(i);
        }
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
    // Records come newest first up to the layer's limit, so a limit that's
    // nearly used up means older traces in the range were left out.
    let capped = layer
        .insight
        .as_ref()
        .and_then(|i| i.limit)
        .filter(|&limit| d.rows as f64 >= 0.9 * limit as f64)
        .map(|limit| {
            let since = all.iter().map(|t| t.start).fold(f64::INFINITY, f64::min);
            format!(
                "{} spans, near the layer's limit of {limit}: only traces since {} are here. Raise `limit` for older ones.",
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
    // Folds the viewer changed, for the trace they were changed in.
    let folds: RwSignal<Option<(String, BTreeSet<String>)>> = RwSignal::new(None);
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
    let pick = move |i: usize| selected.set(all.with_value(|a| a[i].id.clone()));

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
            let mut rows: Vec<(usize, f64, String, f64, usize)> = all.with_value(|a| {
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
                        )
                    })
                    .collect()
            });
            if order.get() == Order::Slowest {
                rows.sort_by(|a, b| b.3.total_cmp(&a.3));
            }
            rows.into_iter()
                .map(|(i, start, name, length, spans)| {
                    let class = if Some(i) == current {
                        "timeline-trace selected"
                    } else {
                        "timeline-trace"
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
                .collect_view()
                .into_any()
        }
    };

    // Jaeger's view: every trace as a dot, start time across, length up (log).
    let plot = {
        let time_zone = time_zone.clone();
        move || {
            let current = current.get();
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
                        let title = format!(
                            "{}\n{}\n{} · {} spans",
                            t.name(),
                            date_time(t.start, &time_zone),
                            duration(len),
                            t.spans.len()
                        );
                        view! {
                            <circle cx=x(t.start) cy=y(len) r=if chosen { 5.0 } else { 3.0 }
                                class=if chosen { "timeline-dot selected" } else { "timeline-dot" }
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

    let body = move || {
        let Some(index) = current.get() else {
            return ().into_any();
        };
        let trace = all.with_value(|a| a[index].clone());
        // Spans are highlighted only when the filter matched inside the
        // trace: a filter matching the trace's own name says nothing about
        // its spans.
        let q = Some(needle.get())
            .filter(|q| !trace.name().to_lowercase().contains(q.as_str()))
            .unwrap_or_default();
        let colors = colors.get_value();
        let folded = folds
            .get()
            .filter(|(id, _)| *id == trace.id)
            .map(|(_, f)| f)
            .unwrap_or_else(|| initial_folds(&trace, &q));
        let below = descendants(&trace);
        let rows_shown = visible(&trace, &folded);
        let len = trace.end - trace.start;
        let axis = axis_ms(len, fit.get());
        let scale = (WIDTH - LABELS - 10.0) / axis;
        let step = tick_step(axis);
        let ticks: Vec<_> = (0..)
            .map(|i| i as f64 * step)
            .take_while(|t| *t <= axis * 1.0001)
            .map(|t| {
                let x = LABELS + t * scale;
                // A tick at the right edge is labelled to its left, so it isn't cut off.
                let anchor = if x > WIDTH - 30.0 { "end" } else { "middle" };
                view! {
                    <line x1=x x2=x y1=AXIS - 4.0 y2=AXIS + rows_shown.len() as f64 * ROW class="timeline-grid"/>
                    <text x=x y=AXIS - 8.0 class="timeline-tick" text-anchor=anchor>{tick_label(t)}</text>
                }
            })
            .collect();
        let own = self_times(&trace);
        let rows: Vec<_> = rows_shown
            .iter()
            .enumerate()
            .map(|(row, &i)| {
                let (s, depth) = &trace.spans[i];
                let y = AXIS + row as f64 * ROW;
                let x = LABELS + (s.start - trace.start) * scale;
                let w = ((s.end - s.start) * scale).max(1.5);
                let fill = s.color.as_ref().and_then(|c| colors.get(c).copied()).unwrap_or(palette[0]);
                let title = format!(
                    "{}\n{} (own time {})\nstarts at +{}{}",
                    s.label,
                    duration(s.end - s.start),
                    duration(own[i]),
                    duration(s.start - trace.start),
                    s.color.as_ref().map(|c| format!("\n{c}")).unwrap_or_default()
                );
                let indent = 4.0 + *depth as f64 * 12.0;
                let dur = duration(s.end - s.start);
                // The duration sits after the bar, or inside it near the right edge.
                let (tx, anchor) = if x + w + 60.0 < WIDTH { (x + w + 4.0, "start") } else { (x + w - 4.0, "end") };
                let is_folded = folded.contains(&s.id);
                let hidden = below[i];
                let class = match (q.is_empty(), s.label.to_lowercase().contains(&q)) {
                    (true, _) => "timeline-row",
                    (false, true) => "timeline-row hit",
                    (false, false) => "timeline-row dim",
                };
                let room = ((LABELS - indent - 14.0) / 6.2).max(4.0) as usize;
                let label = if is_folded {
                    format!("{} (+{hidden})", clip(&s.label, room.saturating_sub(7)))
                } else {
                    clip(&s.label, room)
                };
                let toggle = (hidden > 0).then(|| {
                    let (span, trace_id, folded) = (s.id.clone(), trace.id.clone(), folded.clone());
                    view! {
                        <text x=indent y=y + 14.0 class="timeline-fold"
                            on:click=move |_| {
                                let mut f = folded.clone();
                                if !f.remove(&span) {
                                    f.insert(span.clone());
                                }
                                folds.set(Some((trace_id.clone(), f)));
                            }>
                            {if is_folded { "▸" } else { "▾" }}
                        </text>
                    }
                });
                view! {
                    <g class=class>
                        <title>{title}</title>
                        {toggle}
                        <text x=indent + 12.0 y=y + 14.0 class="timeline-label">{label}</text>
                        <rect x=x y=y + 3.0 width=w height=ROW - 6.0 rx=2 fill=fill/>
                        <text x=tx y=y + 14.0 class="timeline-duration" text-anchor=anchor>{dur}</text>
                    </g>
                }
            })
            .collect();
        let height = AXIS + rows_shown.len() as f64 * ROW + 4.0;
        let by_color = time_by_color(&trace);
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
            .into_iter()
            .map(|(value, ms)| {
                let fill = colors.get(&value).copied().unwrap_or(palette[0]);
                let share = 100.0 * ms / work.max(1e-9);
                view! {
                    <span class="timeline-key">
                        <span class="timeline-swatch" style=format!("background:{fill}")></span>
                        {format!("{value} {} ({share:.0}%)", duration(ms))}
                    </span>
                }
            })
            .collect();
        let folded_any = !folded.is_empty();
        let (trace_id, all_parents): (String, BTreeSet<String>) = (
            trace.id.clone(),
            trace
                .spans
                .iter()
                .zip(&below)
                .filter(|(_, n)| **n > 0)
                .map(|((s, _), _)| s.id.clone())
                .collect(),
        );
        view! {
            <div class="timeline-title">
                <strong>{trace.name().to_string()}</strong>
                <span class="muted small">{format!("{} · {} · {} spans", date_time(trace.start, &time_zone), duration(len), trace.spans.len())}</span>
                <span class="timeline-sort">
                    <button class=move || if fit.get() { "" } else { "selected" } on:click=move |_| fit.set(false)
                        title=format!("Axis at least {}, so short traces look short", duration(MIN_AXIS_MS))>"True scale"</button>
                    <button class=move || if fit.get() { "selected" } else { "" } on:click=move |_| fit.set(true)
                        title="Stretch this trace across the width">"Fit trace"</button>
                </span>
                <span class="timeline-sort">
                    <button on:click={let id = trace_id.clone(); move |_| folds.set(Some((id.clone(), BTreeSet::new())))}
                        disabled=!folded_any>"Expand all"</button>
                    <button on:click={let id = trace_id.clone(); move |_| folds.set(Some((id.clone(), all_parents.clone())))}
                        disabled=all_parents_empty(&below)>"Collapse all"</button>
                </span>
            </div>
            <svg viewBox=format!("0 0 {WIDTH} {height}") class="timeline-svg" preserveAspectRatio="xMinYMin meet">
                {ticks}
                {rows}
            </svg>
            <div class="timeline-legend muted small">"Own time: "{legend}{parallel}</div>
        }
        .into_any()
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

/// Whether no span in a trace has children.
fn all_parents_empty(below: &[usize]) -> bool {
    below.iter().all(|n| *n == 0)
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
        assert!(initial_folds(&t, "").is_empty());
        assert_eq!(visible(&t, &BTreeSet::new()).len(), 9);
    }

    #[test]
    fn large_traces_fold_to_the_deepest_level_that_fits() {
        let t = fan_out(6, 70);
        assert_eq!(descendants(&t)[0], 6 * 71);
        let folds = initial_folds(&t, "");
        assert_eq!(folds.len(), 6, "every step folds, the root stays open");
        assert_eq!(visible(&t, &folds).len(), 7);
    }

    #[test]
    fn folds_never_hide_a_span_matching_the_filter() {
        let t = fan_out(6, 70);
        let folds = initial_folds(&t, "step3-leaf42");
        assert!(!folds.contains("step3"));
        let shown = visible(&t, &folds);
        assert!(shown.iter().any(|&i| t.spans[i].0.id == "step3-leaf42"));
        assert_eq!(shown.len(), 7 + 70);
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
