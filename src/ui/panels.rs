//! Dashboard panels: the frame, and a body per kind of content.

use leptos::prelude::*;

use super::format;
use super::series::{self, XAxis, cell_text, threshold_for};
use super::theme::{palette, threshold_color};
use crate::model::api::Selection;
use crate::model::{
    Cell, Chart, ChartKind, Column, Encoding, Layer, LineStyle, Operation, Panel, PanelContent,
    PanelData, Placeholder, PlaceholderReason, ReferenceAxis,
};

/// Things every panel on a dashboard shares. Some are only read by the
/// browser build (charts are drawn there).
#[derive(Clone)]
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub struct PanelContext {
    pub time_zone: Signal<String>,
    /// Shared crosshair group.
    pub sync_key: String,
    /// A drag-selected time range, epoch ms.
    pub on_zoom: Callback<(f64, f64)>,
    pub dark: Signal<bool>,
    /// Buckets with fewer documents are flagged as unreliable.
    pub min_samples: Option<u32>,
    /// Fields that have a dashboard control: clicking one of their values
    /// focuses the dashboard on it.
    pub drill_fields: Vec<String>,
    pub drill: Callback<(String, String)>,
    /// Opens the inspector on a panel (by id).
    pub inspect: Callback<String>,
    /// Compares a box selected on a panel (by id) with the panel's baseline.
    pub compare: Callback<(String, Selection)>,
    /// Set per panel: its id, and whether a drag selects a box to compare
    /// (instead of zooming the dashboard).
    pub comparing: Option<(String, RwSignal<bool>)>,
}

impl PanelContext {
    /// The field a click on this bucket column should drill into, if any.
    fn drill_field(&self, column: Option<&Column>) -> Option<String> {
        match column.map(|c| &c.op) {
            Some(Operation::Terms { fields, .. })
                if fields.len() == 1 && self.drill_fields.contains(&fields[0]) =>
            {
                Some(fields[0].clone())
            }
            _ => None,
        }
    }
}

/// Whether a panel is drawn on a canvas (time and numeric axes), and so can
/// be saved as PNG.
fn drawn_on_canvas(content: &PanelContent) -> bool {
    let PanelContent::Chart(chart) = content else {
        return false;
    };
    if chart.kind == ChartKind::Heatmap {
        return true;
    }
    if chart.kind != ChartKind::Xy {
        return false;
    }
    chart.layers.iter().any(|l| match &l.encoding {
        Encoding::Band { .. } | Encoding::Scatter { .. } => true,
        // ES|QL x columns are dates or numbers more often than not.
        Encoding::Xy { x: Some(_), .. } if l.esql.is_some() => true,
        Encoding::Xy { x: Some(x), .. } => {
            matches!(
                l.column(x).map(|c| &c.op),
                Some(
                    Operation::DateHistogram { .. }
                        | Operation::Histogram { .. }
                        | Operation::LogHistogram { .. }
                )
            )
        }
        _ => false,
    })
}

#[component]
pub fn PanelView(
    panel: Panel,
    data: Signal<Option<PanelData>>,
    ctx: PanelContext,
) -> impl IntoView {
    let g = panel.grid;
    let style = format!(
        "grid-column: {} / span {}; grid-row: {} / span {};",
        g.x + 1,
        g.w.min(48),
        g.y + 1,
        g.h
    );
    // A chart's own warnings (from conversion) show before its data arrives.
    let own_warnings = match &panel.content {
        PanelContent::Chart(c) => c.warnings.clone(),
        _ => Vec::new(),
    };
    let warnings = move || {
        let mut all = own_warnings.clone();
        for w in data.get().map(|d| d.warnings).unwrap_or_default() {
            if !all.contains(&w) {
                all.push(w);
            }
        }
        (!all.is_empty()).then(|| all.join("\n"))
    };
    let is_chart = matches!(panel.content, PanelContent::Chart(_));
    // Chart panels always get a header: it carries their CSV and Inspect actions.
    let title = panel.title.clone().or_else(|| is_chart.then(String::new));
    let exportable = drawn_on_canvas(&panel.content);
    let csv_chart = match &panel.content {
        PanelContent::Chart(c) => Some(c.clone()),
        _ => None,
    };
    let inspect = ctx.inspect;
    let panel_id = panel.id.clone();
    // Charts over time can compare a drag-selected box with the rest (BubbleUp).
    let comparable = matches!(&panel.content, PanelContent::Chart(c)
        if c.has_time_axis() && c.layers.iter().all(|l| l.esql.is_none()));
    let comparing = RwSignal::new(false);
    let mut ctx = ctx;
    if comparable {
        ctx.comparing = Some((panel.id.clone(), comparing));
    }
    let section = NodeRef::<leptos::html::Section>::new();
    let body = match panel.content.clone() {
        PanelContent::Chart(chart) => view! { <ChartBody chart data ctx/> }.into_any(),
        PanelContent::Markdown { text } => {
            view! { <div class="markdown" inner_html=markdown(&text)></div> }.into_any()
        }
        PanelContent::Placeholder(p) => view! { <PlaceholderBody placeholder=p/> }.into_any(),
    };
    view! {
        <section class="panel" style=style node_ref=section>
            {title.map(|t| {
                let name = t.clone();
                view! {
                    <header class="panel-title">
                        <span>{t}</span>
                        {move || warnings().map(|w| view! { <span class="panel-warning" title=w>"!"</span> })}
                        <span class="spacer"></span>
                        {csv_chart.map(|chart| {
                            let name = name.clone();
                            view! {
                                <button class="panel-action" title="Download the data as CSV" on:click=move |_| {
                                    if let Some(d) = data.get_untracked() {
                                        super::inspector::download_csv(&chart, &d, &name);
                                    }
                                }>"CSV"</button>
                            }
                        })}
                        {comparable.then(|| view! {
                            <button class="panel-action" class:active=move || comparing.get()
                                title="Compare: drag a box on the chart to see what sets those documents apart from the rest"
                                on:click=move |_| comparing.update(|c| *c = !*c)>"Compare"</button>
                        })}
                        {is_chart.then(|| {
                            let id = panel_id.clone();
                            view! {
                                <button class="panel-action" title="Inspect the data and the Elasticsearch queries"
                                    on:click=move |_| inspect.run(id.clone())>"Inspect"</button>
                            }
                        })}
                        {exportable.then(|| view! {
                            <button class="panel-action" title="Save as PNG" on:click=move |_| {
                                #[cfg(feature = "hydrate")]
                                if let Some(el) = section.get() {
                                    use wasm_bindgen::JsCast;
                                    let el: web_sys::HtmlElement = el.unchecked_into();
                                    if let Err(e) = super::export::png(&el, &name) {
                                        leptos::logging::warn!("PNG export: {e}");
                                    }
                                }
                                #[cfg(not(feature = "hydrate"))]
                                let _ = (&name, section);
                            }>"PNG"</button>
                        })}
                    </header>
                }
            })}
            <div class="panel-body">{body}</div>
        </section>
    }
}

#[component]
fn PlaceholderBody(placeholder: Placeholder) -> impl IntoView {
    let reason = match placeholder.reason {
        PlaceholderReason::OutOfScope => "Not supported yet",
        PlaceholderReason::BrokenReference => "Missing from the export",
        PlaceholderReason::Unparseable => "Could not be converted",
    };
    view! {
        <div class="placeholder">
            <strong>{reason}</strong>
            <span class="muted">{placeholder.kind}</span>
            <span class="muted small">{placeholder.detail}</span>
        </div>
    }
}

/// Markdown to HTML. Raw HTML in the source is dropped, not rendered.
fn markdown(text: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, html};
    let parser = Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH)
        .filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)));
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

#[component]
fn ChartBody(chart: Chart, data: Signal<Option<PanelData>>, ctx: PanelContext) -> impl IntoView {
    let chart = StoredValue::new(chart);
    move || {
        let Some(d) = data.get() else {
            return view! { <div class="loading"></div> }.into_any();
        };
        if let Some(e) = d.layers.iter().find_map(|l| l.error.clone()) {
            let title = e.clone();
            return view! { <div class="panel-error" title=title>{e}</div> }.into_any();
        }
        if d.layers.iter().all(|l| l.rows == 0) {
            return view! { <div class="no-data">"No results"</div> }.into_any();
        }
        let c = chart.get_value();
        let min = ctx.min_samples;
        match (c.kind, series::main_layer(&c).map(|l| &l.encoding)) {
            (ChartKind::Metric, _) => {
                view! { <MetricTiles chart=c data=d ctx=ctx.clone()/> }.into_any()
            }
            (ChartKind::Pie | ChartKind::Donut, _) => {
                view! { <PieChart chart=c data=d ctx=ctx.clone()/> }.into_any()
            }
            (ChartKind::Table, _) => {
                view! { <DataTable chart=c data=d ctx=ctx.clone()/> }.into_any()
            }
            (ChartKind::Heatmap, _) => {
                let drawing = Drawing::Heatmap(series::heatmap(&c, &d));
                view! { <CanvasChart drawing ctx=ctx.clone()/> }.into_any()
            }
            (_, Some(Encoding::Band { .. })) => {
                let xy = series::band(&c, &d, min);
                let note = low_sample_note(&xy, min);
                view! { <CanvasChart drawing=Drawing::Xy(xy) ctx=ctx.clone()/>{note} }.into_any()
            }
            (_, Some(Encoding::Scatter { .. })) => {
                view! { <CanvasChart drawing=Drawing::Xy(series::scatter(&c, &d)) ctx=ctx.clone()/> }.into_any()
            }
            _ => {
                let xy = series::xy(&c, &d, min);
                match xy.x {
                    XAxis::Category(_) => {
                        let drill = series::main_layer(&c).and_then(|l| match &l.encoding {
                            Encoding::Xy { x: Some(x), .. } => ctx.drill_field(l.column(x)),
                            _ => None,
                        });
                        view! { <CategoryBars xy drill_field=drill ctx=ctx.clone()/> }.into_any()
                    }
                    _ => {
                        let note = low_sample_note(&xy, min);
                        view! { <CanvasChart drawing=Drawing::Xy(xy) ctx=ctx.clone()/>{note} }.into_any()
                    }
                }
            }
        }
    }
}

/// Says what grey buckets mean, when there are any.
fn low_sample_note(xy: &series::Xy, min: Option<u32>) -> Option<AnyView> {
    let min = min?;
    xy.low_sample.iter().any(|l| *l).then(|| {
        view! { <div class="chart-note" title="Medians and rates over few documents are noisy">{format!("Shaded: fewer than {min} documents")}</div> }.into_any()
    })
}

/// A cell for display, using the column's format and the dashboard time zone for dates.
fn display(cell: &Cell, column: Option<&Column>, is_date: bool, tz: &str) -> String {
    match (cell, column.map(|c| &c.op)) {
        (Cell::Number(ms), Some(Operation::DateHistogram { .. })) => date(*ms, tz),
        (Cell::Number(ms), _) if is_date => date(*ms, tz),
        (Cell::Number(n), _) => format::value(*n, column.and_then(|c| c.display_format()).as_ref()),
        (other, _) => cell_text(other),
    }
}

#[cfg(feature = "hydrate")]
fn date(ms: f64, tz: &str) -> String {
    let date = js_sys::Date::new(&ms.into());
    let options = js_sys::Object::new();
    for (k, v) in [
        ("timeZone", tz),
        ("year", "numeric"),
        ("month", "2-digit"),
        ("day", "2-digit"),
    ] {
        let _ = js_sys::Reflect::set(&options, &k.into(), &v.into());
    }
    date.to_locale_date_string("en-CA", &options).into()
}

#[cfg(not(feature = "hydrate"))]
fn date(ms: f64, _tz: &str) -> String {
    jiff::Timestamp::from_millisecond(ms as i64)
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Whether a column, or anything it is computed from, is time-shifted.
fn shifted(layer: &Layer, column: &Column) -> bool {
    column.time_shift.is_some()
        || column
            .op
            .inputs()
            .iter()
            .any(|id| layer.column(id).is_some_and(|c| shifted(layer, c)))
}

/// Strips time shifts from the comparison so two measures can be compared:
/// the same operation on both sides (formulas included).
fn same_measure(a: &Column, b: &Column) -> bool {
    std::mem::discriminant(&a.op) == std::mem::discriminant(&b.op)
}

#[component]
fn MetricTiles(chart: Chart, data: PanelData, ctx: PanelContext) -> impl IntoView {
    let tiles: Vec<_> = chart
        .layers
        .iter()
        .zip(&data.layers)
        .filter_map(|(layer, d)| {
            let Encoding::Metric {
                value,
                secondary,
                breakdown,
                ..
            } = &layer.encoding
            else {
                return None;
            };
            let value_col = layer.column(value)?;
            let secondary_col = secondary.as_deref().and_then(|s| layer.column(s));
            let min = ctx.min_samples;
            Some((0..d.rows).map(move |row| {
                let v = d
                    .series(value)
                    .and_then(|s| s.values.get(row).cloned())
                    .unwrap_or(Cell::Null);
                let label = breakdown
                    .as_deref()
                    .and_then(|b| d.series(b))
                    .map(|s| cell_text(&s.values[row]))
                    .or_else(|| value_col.label.clone());
                let second = secondary_col.map(|col| {
                    let s = d
                        .series(&col.id)
                        .and_then(|s| s.values.get(row).cloned())
                        .unwrap_or(Cell::Null);
                    let comparable = shifted(layer, col) && same_measure(col, value_col);
                    (col.clone(), s, comparable)
                });
                let count = d.counts.get(row).copied();
                metric_tile(label, &v, value_col, second, count, min)
            }))
        })
        .flatten()
        .collect();
    view! { <div class="metrics">{tiles}</div> }
}

fn metric_tile(
    label: Option<String>,
    value: &Cell,
    column: &Column,
    secondary: Option<(Column, Cell, bool)>,
    count: Option<f64>,
    min_samples: Option<u32>,
) -> AnyView {
    let main = match value {
        Cell::Number(n) => format::value(*n, column.display_format().as_ref()),
        other => cell_text(other),
    };
    let band = value
        .number()
        .and_then(|v| threshold_for(&column.thresholds, v))
        .map(|(i, t)| {
            let style = format!("background:{}", threshold_color(t, i));
            view! { <span class="badge" style=style>{t.label.clone()}</span> }
        });
    let compare = secondary.map(|(col, cell, comparable)| {
        let shown = match &cell {
            Cell::Number(n) => format::value(*n, col.display_format().as_ref()),
            Cell::Null => "–".to_string(),
            other => cell_text(other),
        };
        let delta = match (value.number(), cell.number(), comparable) {
            (Some(now), Some(before), true) => format::change(now, before),
            _ => None,
        };
        view! {
            <div class="metric-secondary">
                <span class="muted">{col.label.unwrap_or_default()}" "</span>
                <span>{shown}</span>
                {delta.map(|d| {
                    let arrow = if d >= 0.0 { "▲" } else { "▼" };
                    view! { <span class="delta">{format!(" {arrow} {:.1}%", d.abs())}</span> }
                })}
            </div>
        }
    });
    // Medians and rates over a handful of documents mislead: say so.
    let low = matches!((count, min_samples), (Some(n), Some(min)) if n < min as f64);
    let sample = (min_samples.is_some()).then_some(count).flatten().map(|n| {
        let text = if low {
            format!("only {n:.0} documents: low sample")
        } else {
            format!("n = {}", format::plain(n))
        };
        view! { <div class=if low { "sample low" } else { "sample" }>{text}</div> }
    });
    let class = if low { "metric low-sample" } else { "metric" };
    view! {
        <div class=class>
            <div class="metric-label">{label}{band}</div>
            <div class="metric-value">{main}</div>
            {compare}
            {sample}
        </div>
    }
    .into_any()
}

#[component]
fn PieChart(chart: Chart, data: PanelData, ctx: PanelContext) -> impl IntoView {
    let tz = ctx.time_zone.get_untracked();
    let mut drill = None;
    let slices: Vec<(String, Option<String>, f64)> = chart
        .layers
        .iter()
        .zip(&data.layers)
        .flat_map(|(layer, d)| {
            let Encoding::Pie { groups, values } = &layer.encoding else {
                return Vec::new();
            };
            let (Some(value), group) = (values.first(), groups.first()) else {
                return Vec::new();
            };
            let vs = d
                .series(value)
                .map(|s| s.values.clone())
                .unwrap_or_default();
            let gs = group.and_then(|g| d.series(g)).map(|s| s.values.clone());
            let group_col = group.and_then(|g| layer.column(g));
            drill = drill.clone().or_else(|| ctx.drill_field(group_col));
            (0..d.rows)
                .filter_map(|row| {
                    let v = vs.get(row)?.number()?;
                    let raw = gs.as_ref().map(|g| cell_text(&g[row]));
                    let label = gs
                        .as_ref()
                        .map(|g| {
                            display(
                                &g[row],
                                group_col,
                                group.is_some_and(|g| d.dates.contains(g)),
                                &tz,
                            )
                        })
                        .unwrap_or_else(|| "Total".into());
                    (v > 0.0).then_some((label, raw, v))
                })
                .collect()
        })
        .collect();
    let total: f64 = slices.iter().map(|(_, _, v)| v).sum();
    let donut = chart.kind == ChartKind::Donut;
    let colors = palette();
    let mut angle = -std::f64::consts::FRAC_PI_2;
    let paths: Vec<_> = slices
        .iter()
        .enumerate()
        .map(|(i, (label, raw, v))| {
            let sweep = v / total * std::f64::consts::TAU;
            let d = arc(
                50.0,
                50.0,
                48.0,
                if donut { 28.0 } else { 0.0 },
                angle,
                angle + sweep,
            );
            angle += sweep;
            let title = format!("{label}: {} ({:.1}%)", format::plain(*v), v / total * 100.0);
            let click = drill.clone().zip(raw.clone());
            let on_drill = ctx.drill;
            view! {
                <path d=d fill=colors[i % colors.len()] class:drillable=click.is_some()
                    on:click=move |_| if let Some((f, v)) = click.clone() { on_drill.run((f, v)) }>
                    <title>{title}</title>
                </path>
            }
        })
        .collect();
    let legend: Vec<_> = slices
        .iter()
        .enumerate()
        .map(|(i, (label, _, v))| {
            let swatch = format!("background:{}", colors[i % colors.len()]);
            view! {
                <li>
                    <span class="swatch" style=swatch></span>
                    <span class="legend-label">{label.clone()}</span>
                    <span class="legend-value">{format!("{:.1}%", v / total * 100.0)}</span>
                </li>
            }
        })
        .collect();
    view! {
        <div class="pie">
            <svg viewBox="0 0 100 100" class="pie-svg">{paths}</svg>
            <ul class="legend">{legend}</ul>
        </div>
    }
}

/// An SVG path for an annular sector (a pie slice when `inner` is 0).
fn arc(cx: f64, cy: f64, outer: f64, inner: f64, start: f64, end: f64) -> String {
    let full = end - start >= std::f64::consts::TAU - 1e-9;
    let end = if full { end - 1e-4 } else { end };
    let large = if end - start > std::f64::consts::PI {
        1
    } else {
        0
    };
    let p = |r: f64, a: f64| (cx + r * a.cos(), cy + r * a.sin());
    let (x0, y0) = p(outer, start);
    let (x1, y1) = p(outer, end);
    if inner <= 0.0 {
        return format!(
            "M{cx},{cy} L{x0:.3},{y0:.3} A{outer},{outer} 0 {large} 1 {x1:.3},{y1:.3} Z"
        );
    }
    let (x2, y2) = p(inner, end);
    let (x3, y3) = p(inner, start);
    format!(
        "M{x0:.3},{y0:.3} A{outer},{outer} 0 {large} 1 {x1:.3},{y1:.3} L{x2:.3},{y2:.3} A{inner},{inner} 0 {large} 0 {x3:.3},{y3:.3} Z"
    )
}

/// One table cell: display text, sort value, and the benchmark band colour.
#[derive(Clone)]
struct TableCell {
    text: String,
    sort: Option<f64>,
    band: Option<(String, String)>,
}

#[component]
fn DataTable(chart: Chart, data: PanelData, ctx: PanelContext) -> impl IntoView {
    let tz = ctx.time_zone.get_untracked();
    let Some((layer, d)) = chart.layers.iter().zip(&data.layers).next() else {
        return ().into_any();
    };
    let ids: Vec<String> = match &layer.encoding {
        Encoding::Table { columns } => columns.clone(),
        _ => d.columns.iter().map(|s| s.id.clone()).collect(),
    };
    let ids: Vec<String> = ids
        .into_iter()
        .filter(|id| d.series(id).is_some())
        .collect();
    let headers: Vec<String> = ids
        .iter()
        .map(|id| {
            layer
                .column(id)
                .and_then(|c| c.label.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect();
    let numeric: Vec<bool> = ids
        .iter()
        .map(|id| {
            d.series(id)
                .is_some_and(|s| s.values.iter().all(|v| !matches!(v, Cell::Text(_))))
        })
        .collect();
    // The first column that can drill into a dashboard control.
    let drill: Option<(usize, String)> = ids
        .iter()
        .enumerate()
        .find_map(|(i, id)| ctx.drill_field(layer.column(id)).map(|f| (i, f)));
    let low: Vec<bool> = (0..d.rows)
        .map(|row| matches!((d.counts.get(row), ctx.min_samples), (Some(n), Some(min)) if *n < min as f64))
        .collect();
    let rows: Vec<(Vec<TableCell>, Option<String>, bool)> = (0..d.rows)
        .map(|row| {
            let cells: Vec<TableCell> = ids
                .iter()
                .map(|id| {
                    let cell = &d.series(id).unwrap().values[row];
                    let column = layer.column(id);
                    let band = column.zip(cell.number()).and_then(|(c, v)| {
                        threshold_for(&c.thresholds, v)
                            .map(|(i, t)| (t.label.clone(), threshold_color(t, i)))
                    });
                    TableCell {
                        text: display(cell, column, d.dates.contains(id), &tz),
                        sort: cell.number(),
                        band,
                    }
                })
                .collect();
            let drill_value = drill
                .as_ref()
                .map(|(i, _)| cell_text(&d.series(&ids[*i]).unwrap().values[row]));
            (cells, drill_value, low[row])
        })
        .collect();

    let sort = RwSignal::new(None::<(usize, bool)>);
    let rows = StoredValue::new(rows);
    let sorted = move || {
        let mut rows = rows.get_value();
        if let Some((col, desc)) = sort.get() {
            rows.sort_by(|a, b| {
                let ord = match (a.0[col].sort, b.0[col].sort) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    _ => a.0[col].text.cmp(&b.0[col].text),
                };
                if desc { ord.reverse() } else { ord }
            });
        }
        rows
    };
    let header_cells: Vec<_> = headers
        .into_iter()
        .enumerate()
        .map(|(i, h)| {
            let class = if numeric[i] { "num" } else { "" };
            let marker = move || match sort.get() {
                Some((c, true)) if c == i => " ↓",
                Some((c, false)) if c == i => " ↑",
                _ => "",
            };
            view! {
                <th class=class on:click=move |_| sort.update(|s| *s = match *s {
                    Some((c, true)) if c == i => Some((i, false)),
                    _ => Some((i, true)),
                })>{h}{marker}</th>
            }
        })
        .collect();
    let numeric = StoredValue::new(numeric);
    let drill_field = drill.map(|(_, f)| f);
    let on_drill = ctx.drill;
    let min = ctx.min_samples;
    view! {
        <div class="table-wrap">
            <table class="data-table">
                <thead><tr>{header_cells}</tr></thead>
                <tbody>
                    {move || sorted().into_iter().map(|(cells, drill_value, low)| {
                        let click = drill_field.clone().zip(drill_value);
                        let title = (low).then(|| format!("Fewer than {} documents: treat with care", min.unwrap_or_default()));
                        view! {
                            <tr class:drillable=click.is_some() class:low-sample=low title=title
                                on:click=move |_| if let Some((f, v)) = click.clone() { on_drill.run((f, v)) }>
                                {cells.into_iter().enumerate().map(|(i, cell)| {
                                    let class = if numeric.get_value()[i] { "num" } else { "" };
                                    let dot = cell.band.map(|(label, color)| view! {
                                        <span class="band-dot" style=format!("background:{color}") title=label></span>
                                    });
                                    view! { <td class=class>{dot}{cell.text}</td> }
                                }).collect_view()}
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
        </div>
    }
    .into_any()
}

/// Bars over categories (terms, filters, ranges), drawn with HTML.
#[component]
fn CategoryBars(xy: series::Xy, drill_field: Option<String>, ctx: PanelContext) -> impl IntoView {
    let XAxis::Category(labels) = &xy.x else {
        return ().into_any();
    };
    let horizontal = xy.series.first().is_some_and(|s| s.kind.is_horizontal());
    let stacked = xy.series.iter().any(|s| s.kind.is_stacked());
    let drawn = series::stacked(&xy);
    let colors = palette();
    let max = (0..labels.len())
        .map(|i| {
            if stacked {
                drawn.iter().filter_map(|s| s[i]).fold(0.0, f64::max)
            } else {
                xy.series
                    .iter()
                    .filter_map(|s| s.values[i])
                    .fold(0.0, f64::max)
            }
        })
        .fold(0.0, f64::max);
    // Horizontal reference lines run across the bars; the axis stretches to show them.
    let lines: Vec<series::RefLine> = xy
        .reference_lines
        .iter()
        .filter(|l| l.axis != ReferenceAxis::Bottom && l.value >= 0.0)
        .cloned()
        .collect();
    let max = lines
        .iter()
        .map(|l| l.value)
        .fold(max, f64::max)
        .max(f64::MIN_POSITIVE);
    let rows: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let bars: Vec<_> = xy
                .series
                .iter()
                .enumerate()
                .filter_map(|(s, series)| {
                    let v = series.values[i]?;
                    let size = v / max * 100.0;
                    let dim = if horizontal { "width" } else { "height" };
                    // With benchmark bands, a bar takes the colour of the band its value is in.
                    let color = threshold_for(&xy.thresholds, v)
                        .map(|(i, t)| threshold_color(t, i))
                        .unwrap_or_else(|| colors[s % colors.len()].to_string());
                    let style = format!("{dim}:{size:.2}%;background:{color}");
                    let title = format!(
                        "{}: {}",
                        series.label,
                        format::value(v, series.format.as_ref())
                    );
                    Some(view! { <div class="cat-bar" style=style title=title></div> })
                })
                .collect();
            let value = xy
                .series
                .iter()
                .filter_map(|s| s.values[i].map(|v| format::value(v, s.format.as_ref())))
                .next();
            let track = if stacked {
                "cat-track stacked"
            } else {
                "cat-track"
            };
            let click = drill_field.clone().map(|f| (f, label.clone()));
            let on_drill = ctx.drill;
            let marks: Vec<_> = lines
                .iter()
                .map(|l| {
                    let side = if horizontal { "left" } else { "bottom" };
                    let border = match l.style {
                        LineStyle::Solid => "solid",
                        LineStyle::Dashed => "dashed",
                        LineStyle::Dotted => "dotted",
                    };
                    let color = l.color.clone().unwrap_or_else(|| "var(--text)".into());
                    let style = format!(
                        "{side}:{:.2}%;border-color:{color};border-style:{border}",
                        l.value / max * 100.0
                    );
                    let title = format!(
                        "{}{}",
                        l.label
                            .as_deref()
                            .map(|t| format!("{t}: "))
                            .unwrap_or_default(),
                        format::value(l.value, l.format.as_ref())
                    );
                    view! { <div class="cat-ref" style=style title=title></div> }
                })
                .collect();
            view! {
                <div class="cat-row" class:drillable=click.is_some()
                    on:click=move |_| if let Some((f, v)) = click.clone() { on_drill.run((f, v)) }>
                    <span class="cat-label" title=label.clone()>{label.clone()}</span>
                    <div class=track>{bars}{marks}</div>
                    {horizontal.then(|| view! { <span class="cat-value">{value}</span> })}
                </div>
            }
        })
        .collect();
    let class = if horizontal {
        "cat-chart horizontal"
    } else {
        "cat-chart vertical"
    };
    let key: Vec<_> = lines
        .iter()
        .filter_map(|l| {
            let label = l.label.clone()?;
            let style = format!(
                "border-color:{}",
                l.color.clone().unwrap_or_else(|| "var(--text)".into())
            );
            Some(view! {
                <span class="cat-ref-key"><span class="swatch" style=style></span>{label} " " {format::value(l.value, l.format.as_ref())}</span>
            })
        })
        .collect();
    view! { <div class=class>{rows}</div>{(!key.is_empty()).then(|| view! { <div class="chart-note ref-key">{key}</div> })} }.into_any()
}

/// What a canvas chart draws.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
enum Drawing {
    Xy(series::Xy),
    Heatmap(series::Heatmap),
}

/// Time and numeric x axes, and heatmaps, drawn by uPlot in the browser.
#[component]
fn CanvasChart(drawing: Drawing, ctx: PanelContext) -> impl IntoView {
    let node = NodeRef::<leptos::html::Div>::new();
    #[cfg(feature = "hydrate")]
    {
        use std::rc::Rc;
        use wasm_bindgen::JsCast;
        use wasm_bindgen::prelude::Closure;

        let chart = StoredValue::new_local(None::<super::uplot::Chart>);
        let observer =
            StoredValue::new_local(None::<(web_sys::ResizeObserver, Closure<dyn FnMut()>)>);
        let drawing = StoredValue::new_local(drawing);
        let ctx = ctx.clone();
        Effect::new(move |_| {
            let dark = ctx.dark.get(); // redraw with the new theme's colours
            let Some(el) = node.get() else { return };
            let el: web_sys::HtmlElement = el.unchecked_into();
            let size =
                |el: &web_sys::HtmlElement| (el.client_width() as f64, el.client_height() as f64);
            let (width, height) = size(&el);
            let zoom = ctx.on_zoom;
            // In compare mode a drag selects a box (x and y) to compare.
            let on_box = ctx.comparing.as_ref().filter(|(_, on)| on.get()).map(|(panel, _)| {
                let (panel, compare) = (panel.clone(), ctx.compare);
                let rows = drawing.with_value(|d| match d {
                    Drawing::Heatmap(hm) => Some(hm.y_edges.clone()),
                    Drawing::Xy(_) => None,
                });
                let points = drawing.with_value(|d| matches!(d, Drawing::Xy(xy) if xy.series.iter().any(|s| s.style == series::Style::Points)));
                let on_box: super::uplot::OnBox = Rc::new(move |x0, x1, y0, y1| {
                    let y = match &rows {
                        // Heatmap rows: from the bottom row's start to the top row's end.
                        Some(edges) if !edges.is_empty() => {
                            let last = edges.len() - 1;
                            let low = (y0.floor().max(0.0) as usize).min(last);
                            let high = (y1.ceil().max(0.0) as usize).min(last);
                            Some((edges[low], edges[high]))
                        }
                        Some(_) => None,
                        None if points => Some((Some(y0), Some(y1))),
                        None => None,
                    };
                    compare.run((panel.clone(), Selection { x: (x0, x1), y }));
                });
                on_box
            });
            let options = super::uplot::Options {
                width,
                height,
                sync_key: &ctx.sync_key,
                time_zone: &ctx.time_zone.get_untracked(),
                on_zoom: Some(Rc::new(move |from, to| zoom.run((from, to)))),
                on_box,
                dark,
            };
            chart.set_value(None);
            el.set_inner_html("");
            chart.set_value(Some(drawing.with_value(|d| match d {
                Drawing::Xy(xy) => super::uplot::draw(&el, xy, options),
                Drawing::Heatmap(hm) => super::uplot::heatmap(&el, hm, options),
            })));
            // uPlot's legend has no layout until the browser's next frame;
            // fit again then, so the plot takes the space the legend leaves.
            let target = el.clone();
            request_animation_frame(move || {
                let (w, h) = size(&target);
                chart.with_value(|c| {
                    if let Some(c) = c {
                        c.fit(w, h);
                    }
                });
            });

            if observer.with_value(Option::is_none) {
                let target = el.clone();
                let on_resize = Closure::<dyn FnMut()>::new(move || {
                    let (w, h) = size(&target);
                    chart.with_value(|c| {
                        if let Some(c) = c {
                            c.fit(w, h);
                        }
                    });
                });
                if let Ok(ro) = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) {
                    ro.observe(&el);
                    observer.set_value(Some((ro, on_resize)));
                }
            }
        });
        on_cleanup(move || {
            observer.with_value(|o| {
                if let Some((ro, _)) = o {
                    ro.disconnect();
                }
            });
            chart.set_value(None);
        });
    }
    #[cfg(not(feature = "hydrate"))]
    let _ = (drawing, ctx);
    view! { <div class="timechart" node_ref=node></div> }
}
