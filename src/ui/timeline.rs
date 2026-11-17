//! A trace timeline: the spans of one trace as nested bars on a time axis,
//! like an OpenTelemetry trace view. One row per span arrives (a records
//! layer); rows are grouped into traces, newest first, and a picker chooses
//! the trace. Below the bars, each `color` value's own time: a span's
//! duration less the time its children cover. When sibling spans don't
//! overlap, the totals add up to the trace's length.

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

/// Epoch ms → `2026-11-16 14:05:09` in the dashboard's time zone.
#[cfg(feature = "hydrate")]
pub fn date_time(ms: f64, tz: &str) -> String {
    let date = js_sys::Date::new(&ms.into());
    let options = js_sys::Object::new();
    for (k, v) in [
        ("timeZone", tz),
        ("year", "numeric"),
        ("month", "2-digit"),
        ("day", "2-digit"),
        ("hour", "2-digit"),
        ("minute", "2-digit"),
        ("second", "2-digit"),
        ("hourCycle", "h23"),
    ] {
        let _ = js_sys::Reflect::set(&options, &k.into(), &v.into());
    }
    let text: String = date.to_locale_string("en-CA", &options).into();
    text.replace(',', "")
}

/// Epoch ms → `2026-11-16 14:05:09` in the dashboard's time zone.
#[cfg(not(feature = "hydrate"))]
pub fn date_time(ms: f64, tz: &str) -> String {
    let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
    jiff::Timestamp::from_millisecond(ms as i64)
        .map(|t| t.to_zoned(zone).strftime("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
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

/// A round tick step for a span of `ms`, giving about five ticks.
fn tick_step(ms: f64) -> f64 {
    let raw = (ms / 5.0).max(1.0);
    let mag = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag)
}

const ROW: f64 = 20.0;
const LABELS: f64 = 260.0;
const WIDTH: f64 = 1000.0;
const AXIS: f64 = 22.0;

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
    let count = all.len();
    let all = StoredValue::new(all);
    let colors = StoredValue::new(colors);
    // The selection is a trace id, so re-sorting the list keeps it.
    let selected = RwSignal::new(all.with_value(|a| a[0].id.clone()));
    let slowest = RwSignal::new(false);
    let list = {
        let time_zone = time_zone.clone();
        move || {
            let mut rows: Vec<(String, f64, String, f64, usize)> = all.with_value(|a| {
                a.iter()
                    .map(|t| {
                        (
                            t.id.clone(),
                            t.start,
                            t.name().to_string(),
                            t.end - t.start,
                            t.spans.len(),
                        )
                    })
                    .collect()
            });
            if slowest.get() {
                rows.sort_by(|a, b| b.3.total_cmp(&a.3));
            }
            let current = selected.get();
            rows.into_iter()
                .map(|(id, start, name, length, spans)| {
                    let class = if id == current {
                        "timeline-trace selected"
                    } else {
                        "timeline-trace"
                    };
                    view! {
                        <button class=class on:click=move |_| selected.set(id.clone())>
                            <span class="timeline-trace-when">{date_time(start, &time_zone)}</span>
                            <span class="timeline-trace-name">{name}</span>
                            <span class="timeline-trace-length">{duration(length)}</span>
                            <span class="muted">{format!("{spans} spans")}</span>
                        </button>
                    }
                })
                .collect_view()
        }
    };
    let body = move || {
        let id = selected.get();
        let trace = all.with_value(|a| a.iter().find(|t| t.id == id).unwrap_or(&a[0]).clone());
        let colors = colors.get_value();
        let total = (trace.end - trace.start).max(1.0);
        let scale = (WIDTH - LABELS - 10.0) / total;
        let step = tick_step(total);
        let ticks: Vec<_> = (0..)
            .map(|i| i as f64 * step)
            .take_while(|t| *t <= total)
            .map(|t| {
                let x = LABELS + t * scale;
                view! {
                    <line x1=x x2=x y1=AXIS - 4.0 y2=AXIS + trace.spans.len() as f64 * ROW class="timeline-grid"/>
                    <text x=x y=AXIS - 8.0 class="timeline-tick" text-anchor="middle">{duration(t)}</text>
                }
            })
            .collect();
        let own = self_times(&trace);
        let rows: Vec<_> = trace
            .spans
            .iter()
            .zip(own)
            .enumerate()
            .map(|(i, ((s, depth), own))| {
                let y = AXIS + i as f64 * ROW;
                let x = LABELS + (s.start - trace.start) * scale;
                let w = ((s.end - s.start) * scale).max(1.5);
                let fill = s.color.as_ref().and_then(|c| colors.get(c).copied()).unwrap_or(palette[0]);
                let title = format!(
                    "{}\n{} (own time {})\nstarts at +{}{}",
                    s.label,
                    duration(s.end - s.start),
                    duration(own),
                    duration(s.start - trace.start),
                    s.color.as_ref().map(|c| format!("\n{c}")).unwrap_or_default()
                );
                let indent = 4.0 + *depth as f64 * 12.0;
                let dur = duration(s.end - s.start);
                // The duration sits after the bar, or inside it near the right edge.
                let (tx, anchor) = if x + w + 60.0 < WIDTH { (x + w + 4.0, "start") } else { (x + w - 4.0, "end") };
                view! {
                    <g class="timeline-row">
                        <title>{title}</title>
                        <text x=indent y=y + 14.0 class="timeline-label">{s.label.clone()}</text>
                        <rect x=x y=y + 3.0 width=w height=ROW - 6.0 rx=2 fill=fill/>
                        <text x=tx y=y + 14.0 class="timeline-duration" text-anchor=anchor>{dur}</text>
                    </g>
                }
            })
            .collect();
        let height = AXIS + trace.spans.len() as f64 * ROW + 4.0;
        let legend: Vec<_> = time_by_color(&trace)
            .into_iter()
            .map(|(value, ms)| {
                let fill = colors.get(&value).copied().unwrap_or(palette[0]);
                let share = 100.0 * ms / total;
                view! {
                    <span class="timeline-key">
                        <span class="timeline-swatch" style=format!("background:{fill}")></span>
                        {format!("{value} {} ({share:.0}%)", duration(ms))}
                    </span>
                }
            })
            .collect();
        view! {
            <svg viewBox=format!("0 0 {WIDTH} {height}") class="timeline-svg" preserveAspectRatio="xMinYMin meet">
                {ticks}
                {rows}
            </svg>
            <div class="timeline-legend muted small">"Own time: "{legend}</div>
        }
    };
    view! {
        <div class="timeline">
            <div class="timeline-head">
                <span class="muted small">{format!("{count} {} in range: pick one", if count == 1 { "trace" } else { "traces" })}</span>
                <span class="timeline-sort">
                    <button class=move || if slowest.get() { "" } else { "selected" } on:click=move |_| slowest.set(false)>"Newest"</button>
                    <button class=move || if slowest.get() { "selected" } else { "" } on:click=move |_| slowest.set(true)>"Slowest"</button>
                </span>
            </div>
            <div class="timeline-traces">{list}</div>
            {body}
        </div>
    }
    .into_any()
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
        assert_eq!(date_time(1_794_805_910_744.0, "UTC"), "2026-11-16 05:11:50");
    }

    #[test]
    fn durations_and_ticks_read_well() {
        assert_eq!(duration(850.0), "850 ms");
        assert_eq!(duration(1250.0), "1.25 s");
        assert_eq!(duration(144_000.0), "2.4 min");
        assert_eq!(tick_step(7301.0), 2000.0);
        assert_eq!(tick_step(40.0), 10.0);
    }
}
