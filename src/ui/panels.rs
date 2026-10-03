//! Dashboard panels: the frame, and a body per kind of content.

use leptos::prelude::*;

use super::format;
use super::series::{self, XAxis, cell_text};
use super::theme::palette;
use crate::model::{
    Cell, Chart, ChartKind, Column, Encoding, Operation, Panel, PanelContent, PanelData,
    Placeholder, PlaceholderReason,
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
    let warnings = move || {
        data.get()
            .map(|d| d.warnings.join("\n"))
            .filter(|w| !w.is_empty())
    };
    let title = panel.title.clone();
    let body = match panel.content.clone() {
        PanelContent::Chart(chart) => view! { <ChartBody chart data ctx/> }.into_any(),
        PanelContent::Markdown { text } => {
            view! { <div class="markdown" inner_html=markdown(&text)></div> }.into_any()
        }
        PanelContent::Placeholder(p) => view! { <PlaceholderBody placeholder=p/> }.into_any(),
    };
    view! {
        <section class="panel" style=style>
            {title.map(|t| view! {
                <header class="panel-title">
                    <span>{t}</span>
                    {move || warnings().map(|w| view! { <span class="panel-warning" title=w>"!"</span> })}
                </header>
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
        match c.kind {
            ChartKind::Metric => view! { <MetricTiles chart=c data=d/> }.into_any(),
            ChartKind::Pie | ChartKind::Donut => {
                view! { <PieChart chart=c data=d ctx=ctx.clone()/> }.into_any()
            }
            ChartKind::Table => view! { <DataTable chart=c data=d ctx=ctx.clone()/> }.into_any(),
            ChartKind::Xy => {
                let xy = series::xy(&c, &d);
                match xy.x {
                    XAxis::Category(_) => view! { <CategoryBars xy/> }.into_any(),
                    _ => view! { <TimeChart xy ctx=ctx.clone()/> }.into_any(),
                }
            }
        }
    }
}

/// A cell for display, using the column's format and the dashboard time zone for dates.
fn display(cell: &Cell, column: Option<&Column>, tz: &str) -> String {
    match (cell, column.map(|c| &c.op)) {
        (Cell::Number(ms), Some(Operation::DateHistogram { .. })) => date(*ms, tz),
        (Cell::Number(n), _) => format::value(*n, column.and_then(|c| c.format.as_ref())),
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

#[component]
fn MetricTiles(chart: Chart, data: PanelData) -> impl IntoView {
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
                    (col.clone(), s)
                });
                metric_tile(label, &v, value_col, second)
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
    secondary: Option<(Column, Cell)>,
) -> AnyView {
    let main = match value {
        Cell::Number(n) => format::value(*n, column.format.as_ref()),
        other => cell_text(other),
    };
    let compare = secondary.map(|(col, cell)| {
        let shown = match &cell {
            // A time-shifted count beside a formula shares its format only if it has one.
            Cell::Number(n) => format::value(*n, col.format.as_ref()),
            other => cell_text(other),
        };
        // A change is only meaningful between the same measure, shifted in
        // time (a count per week beside a raw count is not).
        let same_measure = std::mem::discriminant(&col.op) == std::mem::discriminant(&column.op);
        let delta = match (value.number(), cell.number(), col.time_shift.is_some() && same_measure) {
            (Some(now), Some(before), true) => format::change(now, before),
            _ => None,
        };
        view! {
            <div class="metric-secondary">
                <span class="muted">{col.label.unwrap_or_default()}" "</span>
                <span>{shown}</span>
                {delta.map(|d| {
                    let (arrow, class) = if d >= 0.0 { ("▲", "delta up") } else { ("▼", "delta down") };
                    view! { <span class=class>{format!(" {arrow} {:.1}%", d.abs())}</span> }
                })}
            </div>
        }
    });
    view! {
        <div class="metric">
            <div class="metric-label">{label}</div>
            <div class="metric-value">{main}</div>
            {compare}
        </div>
    }
    .into_any()
}

#[component]
fn PieChart(chart: Chart, data: PanelData, ctx: PanelContext) -> impl IntoView {
    let tz = ctx.time_zone.get_untracked();
    let slices: Vec<(String, f64)> = chart
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
            (0..d.rows)
                .filter_map(|row| {
                    let v = vs.get(row)?.number()?;
                    let label = gs
                        .as_ref()
                        .map(|g| display(&g[row], group_col, &tz))
                        .unwrap_or_else(|| "Total".into());
                    (v > 0.0).then_some((label, v))
                })
                .collect()
        })
        .collect();
    let total: f64 = slices.iter().map(|(_, v)| v).sum();
    let donut = chart.kind == ChartKind::Donut;
    let colors = palette();
    let mut angle = -std::f64::consts::FRAC_PI_2;
    let paths: Vec<_> = slices
        .iter()
        .enumerate()
        .map(|(i, (label, v))| {
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
            view! { <path d=d fill=colors[i % colors.len()]><title>{title}</title></path> }
        })
        .collect();
    let legend: Vec<_> = slices
        .iter()
        .enumerate()
        .map(|(i, (label, v))| {
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
    let rows: Vec<Vec<(String, Option<f64>)>> = (0..d.rows)
        .map(|row| {
            ids.iter()
                .map(|id| {
                    let cell = &d.series(id).unwrap().values[row];
                    (display(cell, layer.column(id), &tz), cell.number())
                })
                .collect()
        })
        .collect();

    let sort = RwSignal::new(None::<(usize, bool)>);
    let rows = StoredValue::new(rows);
    let sorted = move || {
        let mut rows = rows.get_value();
        if let Some((col, desc)) = sort.get() {
            rows.sort_by(|a, b| {
                let ord = match (a[col].1, b[col].1) {
                    (Some(x), Some(y)) => x.total_cmp(&y),
                    _ => a[col].0.cmp(&b[col].0),
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
    view! {
        <div class="table-wrap">
            <table class="data-table">
                <thead><tr>{header_cells}</tr></thead>
                <tbody>
                    {move || sorted().into_iter().map(|row| view! {
                        <tr>{row.into_iter().enumerate().map(|(i, (text, _))| {
                            let class = if numeric.get_value()[i] { "num" } else { "" };
                            view! { <td class=class>{text}</td> }
                        }).collect_view()}</tr>
                    }).collect_view()}
                </tbody>
            </table>
        </div>
    }
    .into_any()
}

/// Bars over categories (terms, filters, ranges), drawn with HTML.
#[component]
fn CategoryBars(xy: series::Xy) -> impl IntoView {
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
        .fold(0.0, f64::max)
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
                    let style = format!("{dim}:{size:.2}%;background:{}", colors[s % colors.len()]);
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
            view! {
                <div class="cat-row">
                    <span class="cat-label" title=label.clone()>{label.clone()}</span>
                    <div class=track>{bars}</div>
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
    view! { <div class=class>{rows}</div> }.into_any()
}

/// Time and numeric x axes, drawn by uPlot in the browser.
#[component]
fn TimeChart(xy: series::Xy, ctx: PanelContext) -> impl IntoView {
    let node = NodeRef::<leptos::html::Div>::new();
    #[cfg(feature = "hydrate")]
    {
        use std::rc::Rc;
        use wasm_bindgen::JsCast;
        use wasm_bindgen::prelude::Closure;

        let chart = StoredValue::new_local(None::<super::uplot::Chart>);
        let observer =
            StoredValue::new_local(None::<(web_sys::ResizeObserver, Closure<dyn FnMut()>)>);
        let xy = StoredValue::new_local(xy);
        let ctx = ctx.clone();
        Effect::new(move |_| {
            let _ = ctx.dark.get(); // redraw with the new theme's colours
            let Some(el) = node.get() else { return };
            let el: web_sys::HtmlElement = el.unchecked_into();
            let size =
                |el: &web_sys::HtmlElement| (el.client_width() as f64, el.client_height() as f64);
            let (width, height) = size(&el);
            let zoom = ctx.on_zoom;
            let options = super::uplot::Options {
                width,
                height,
                sync_key: &ctx.sync_key,
                time_zone: &ctx.time_zone.get_untracked(),
                on_zoom: Some(Rc::new(move |from, to| zoom.run((from, to)))),
            };
            chart.set_value(None);
            el.set_inner_html("");
            chart.set_value(Some(
                xy.with_value(|xy| super::uplot::draw(&el, xy, options)),
            ));
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
    let _ = (xy, ctx);
    view! { <div class="timechart" node_ref=node></div> }
}
