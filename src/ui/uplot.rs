//! Bindings to the vendored uPlot (`public/vendor/uplot`), and building a
//! chart from [`Xy`] series. Browser only.

use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::format;
use super::series::{RefLine, Style, XAxis, Xy, XySeries, stacked};
use super::theme;
use super::theme::threshold_color;
use crate::model::{
    Format, FormatKind, LineStyle, ReferenceAxis, ReferenceFill, SeriesType, Threshold,
};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = uPlot)]
    type UPlot;

    #[wasm_bindgen(constructor, js_class = "uPlot")]
    fn new(opts: &JsValue, data: &JsValue, target: &web_sys::HtmlElement) -> UPlot;

    #[wasm_bindgen(method)]
    fn destroy(this: &UPlot);

    #[wasm_bindgen(method, js_name = setSize)]
    fn set_size(this: &UPlot, size: &JsValue);

    #[wasm_bindgen(js_namespace = ["uPlot", "paths"], js_name = bars)]
    fn bars(opts: &JsValue) -> JsValue;

    #[wasm_bindgen(static_method_of = UPlot, js_class = "uPlot", js_name = tzDate)]
    fn tz_date(date: &js_sys::Date, tz: &str) -> js_sys::Date;
}

/// uPlot calls back with up to four arguments; unused ones are ignored.
type Callback = Closure<dyn FnMut(JsValue, JsValue, JsValue, JsValue) -> JsValue>;

/// A live chart. Dropping it destroys the chart and frees its callbacks.
pub struct Chart {
    plot: UPlot,
    target: web_sys::HtmlElement,
    _callbacks: Vec<Callback>,
}

impl Drop for Chart {
    fn drop(&mut self) {
        self.plot.destroy();
    }
}

impl Chart {
    /// Fits the chart, legend included, into `width` × `height` pixels.
    pub fn fit(&self, width: f64, height: f64) {
        let legend = self
            .target
            .query_selector(".u-legend")
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
            .map_or(0.0, |e| e.offset_height() as f64);
        let size = Object::new();
        set(&size, "width", width);
        set(&size, "height", (height - legend - 4.0).max(60.0));
        self.plot.set_size(&size);
    }
}

fn set(target: &Object, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(target, &JsValue::from_str(key), &value.into());
}

fn obj(pairs: &[(&str, JsValue)]) -> Object {
    let o = Object::new();
    for (k, v) in pairs {
        set(&o, k, v.clone());
    }
    o
}

pub struct Options<'a> {
    pub width: f64,
    /// Total height, legend included.
    pub height: f64,
    /// Charts with the same key share a crosshair (Grafana's shared tooltip).
    pub sync_key: &'a str,
    pub time_zone: &'a str,
    /// Called with the selected x range (epoch ms) after a drag on a time chart.
    pub on_zoom: Option<Rc<dyn Fn(f64, f64)>>,
}

/// Formats differ in kind or suffix: series that differ get their own axis
/// (as Lens does by default), e.g. a percentage beside a duration in days.
fn format_key(f: Option<&Format>) -> (Option<FormatKind>, Option<String>) {
    (f.map(|f| f.kind), f.and_then(|f| f.suffix.clone()))
}

/// Draws `xy` (time or numeric x axis) into `target`.
pub fn draw(target: &web_sys::HtmlElement, xy: &Xy, options: Options) -> Chart {
    let palette = theme::palette();
    let colors = theme::chart_colors();
    let (xs, is_time) = match &xy.x {
        XAxis::Time(v) => (v.clone(), true),
        XAxis::Numeric(v) => (v.clone(), false),
        XAxis::Category(_) => (Vec::new(), false),
    };
    let drawn = stacked(xy);
    let originals: Rc<Vec<Vec<Option<f64>>>> =
        Rc::new(xy.series.iter().map(|s| s.values.clone()).collect());
    let mut callbacks = Vec::new();
    let percentage = xy.series.iter().any(|s| s.kind.is_percentage());

    // Left axis: the first series' format. Right axis: the first other format.
    let left_key = format_key(xy.series.first().and_then(|s| s.format.as_ref()));
    let right_series = (!percentage)
        .then(|| {
            xy.series
                .iter()
                .find(|s| format_key(s.format.as_ref()) != left_key)
        })
        .flatten();
    let on_right =
        |s: &XySeries| right_series.is_some() && format_key(s.format.as_ref()) != left_key;

    // Stacked series are drawn tallest first so each band stays visible.
    let mut order: Vec<usize> = (0..xy.series.len()).collect();
    if xy.series.iter().any(|s| s.kind.is_stacked()) {
        order.reverse();
    }

    let data = Array::new();
    data.push(&js_sys::Float64Array::from(xs.as_slice()));
    let series = Array::new();
    series.push(&Object::new());
    for &i in &order {
        let s = &xy.series[i];
        let values = Array::new();
        for v in &drawn[i] {
            values.push(&v.map_or(JsValue::NULL, JsValue::from_f64));
        }
        data.push(&values);

        let color = palette[i % palette.len()];
        let spec = obj(&[
            ("label", s.label.clone().into()),
            ("stroke", color.into()),
            ("width", if s.kind.is_bar() { 0.0 } else { 2.0 }.into()),
            ("spanGaps", (!s.kind.is_bar()).into()),
            ("points", obj(&[("show", false.into())]).into()),
            ("scale", if on_right(s) { "y2" } else { "y" }.into()),
        ]);
        match s.style {
            Style::Points => {
                let no_path =
                    Callback::new(|_: JsValue, _: JsValue, _: JsValue, _: JsValue| JsValue::NULL);
                set(
                    &spec,
                    "paths",
                    no_path.as_ref().unchecked_ref::<Function>().clone(),
                );
                callbacks.push(no_path);
                set(
                    &spec,
                    "points",
                    obj(&[
                        ("show", true.into()),
                        ("size", 4.into()),
                        ("fill", format!("{color}aa").into()),
                        ("width", 0.into()),
                    ]),
                );
                set(&spec, "spanGaps", false);
            }
            Style::BandEdge => {
                set(&spec, "stroke", format!("{color}66"));
                set(&spec, "width", 1);
            }
            Style::Normal => {}
        }
        if s.style == Style::Normal
            && (s.kind.is_bar() || s.kind.is_stacked() || matches!(s.kind, SeriesType::Area))
        {
            set(
                &spec,
                "fill",
                format!("{color}{}", if s.kind.is_bar() { "cc" } else { "40" }),
            );
        }
        if s.kind.is_bar() {
            let size = Array::of2(&0.8.into(), &f64::INFINITY.into());
            set(
                &spec,
                "paths",
                bars(&obj(&[("size", size.into()), ("align", 0.into())])),
            );
        }
        // Legend values show the series' own numbers, not stacked totals.
        let originals = originals.clone();
        let fmt = s.format.clone();
        let value = Callback::new(
            move |_u: JsValue, _v: JsValue, _series: JsValue, idx: JsValue| {
                let v = idx
                    .as_f64()
                    .and_then(|idx| originals[i].get(idx as usize).copied().flatten());
                JsValue::from_str(&v.map_or("–".into(), |v| format::value(v, fmt.as_ref())))
            },
        );
        set(
            &spec,
            "value",
            value.as_ref().unchecked_ref::<Function>().clone(),
        );
        callbacks.push(value);
        series.push(&spec);
    }

    // Axis ticks keep a format's unit but not its fixed decimals.
    let tick_formatter = |fmt: Option<Format>| {
        let fmt = fmt.map(|f| Format {
            decimals: if f.kind == FormatKind::Percent {
                f.decimals
            } else {
                None
            },
            ..f
        });
        Callback::new(
            move |_u: JsValue, splits: JsValue, _a: JsValue, _b: JsValue| {
                let out = Array::new();
                for v in Array::from(&splits).iter() {
                    let text = v
                        .as_f64()
                        .map_or(String::new(), |v| format::value(v, fmt.as_ref()));
                    out.push(&JsValue::from_str(&text));
                }
                out.into()
            },
        )
    };
    let left_format = if percentage {
        Some(Format {
            kind: FormatKind::Percent,
            decimals: Some(0),
            suffix: None,
            compact: false,
        })
    } else {
        xy.series.first().and_then(|s| s.format.clone())
    };
    let axis = |extra: &[(&str, JsValue)]| {
        let a = obj(&[
            ("stroke", colors.text.clone().into()),
            (
                "grid",
                obj(&[("stroke", colors.grid.clone().into()), ("width", 1.into())]).into(),
            ),
            (
                "ticks",
                obj(&[("stroke", colors.grid.clone().into()), ("width", 1.into())]).into(),
            ),
        ]);
        for (k, v) in extra {
            set(&a, k, v.clone());
        }
        a
    };
    let left_ticks = tick_formatter(left_format);
    let axes = Array::of2(
        &axis(&[]).into(),
        &axis(&[
            ("scale", "y".into()),
            (
                "values",
                left_ticks
                    .as_ref()
                    .unchecked_ref::<Function>()
                    .clone()
                    .into(),
            ),
            ("size", 64.into()),
        ])
        .into(),
    );
    callbacks.push(left_ticks);
    if let Some(right) = right_series {
        let right_ticks = tick_formatter(right.format.clone());
        axes.push(&axis(&[
            ("scale", "y2".into()),
            ("side", 1.into()),
            ("grid", obj(&[("show", false.into())]).into()),
            (
                "values",
                right_ticks
                    .as_ref()
                    .unchecked_ref::<Function>()
                    .clone()
                    .into(),
            ),
            ("size", 64.into()),
        ]));
        callbacks.push(right_ticks);
    }

    // Reference lines on the right axis need one; otherwise they join the left.
    let line_scale = |line: &RefLine| match line.axis {
        ReferenceAxis::Bottom => "x",
        ReferenceAxis::Right if right_series.is_some() => "y2",
        _ => "y",
    };
    // Bars and percentage stacks start at zero; lines may float. The axis
    // stretches to show its reference lines, as in Lens.
    let range = |on_right_axis: bool| -> Option<Array> {
        if percentage && !on_right_axis {
            return Some(Array::of2(&0.into(), &1.into()));
        }
        let mine: Vec<usize> = (0..xy.series.len())
            .filter(|&i| on_right(&xy.series[i]) == on_right_axis)
            .collect();
        let has_bars = mine.iter().any(|&i| xy.series[i].kind.is_bar());
        let lines: Vec<f64> = xy
            .reference_lines
            .iter()
            .filter(|l| line_scale(l) == if on_right_axis { "y2" } else { "y" })
            .map(|l| l.value)
            .collect();
        let values: Vec<f64> = mine
            .iter()
            .flat_map(|&i| drawn[i].iter().flatten().copied())
            .chain(lines.iter().copied())
            .collect();
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        if has_bars && min >= 0.0 && max > 0.0 {
            return Some(Array::of2(&0.into(), &(max * 1.05).into()));
        }
        if lines.is_empty() || xy.log_scale || !min.is_finite() || !max.is_finite() {
            return None;
        }
        let pad = ((max - min) * 0.05)
            .max(max.abs() * 0.05)
            .max(f64::MIN_POSITIVE);
        Some(Array::of2(&(min - pad).into(), &(max + pad).into()))
    };
    let y_scale = obj(&[]);
    if xy.log_scale {
        set(&y_scale, "distr", 3);
    }
    if let Some(r) = range(false) {
        set(&y_scale, "range", r);
    }
    let scales = obj(&[
        ("x", obj(&[("time", is_time.into())]).into()),
        ("y", y_scale.into()),
    ]);
    if right_series.is_some() {
        let y2 = obj(&[]);
        if let Some(r) = range(true) {
            set(&y2, "range", r);
        }
        set(&scales, "y2", y2);
    }

    // uPlot series index of each of ours (0 is the x axis).
    let uplot_index = |i: usize| order.iter().position(|&o| o == i).map(|p| p + 1);
    let bands = Array::new();
    if let Some((low, high)) = xy.band
        && let (Some(lo), Some(hi)) = (uplot_index(low), uplot_index(high))
    {
        let color = palette[low % palette.len()];
        bands.push(&obj(&[
            ("series", Array::of2(&hi.into(), &lo.into()).into()),
            ("fill", format!("{color}30").into()),
        ]));
    }

    // Benchmark bands, low-sample buckets and reference-line fills are
    // painted behind the data; reference lines on top of it.
    let thresholds = xy.thresholds.clone();
    let low_sample = xy.low_sample.clone();
    let xs_for_shading = xs.clone();
    let lines: Rc<Vec<(RefLine, &'static str)>> = Rc::new(
        xy.reference_lines
            .iter()
            .map(|l| (l.clone(), line_scale(l)))
            .collect(),
    );
    let fills = lines.clone();
    let line_color = colors.text.clone();
    let panel = colors.panel.clone();
    let shade = Callback::new(move |u: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
        shade_background(&u, &thresholds, &low_sample, &xs_for_shading);
        draw_reference_lines(&u, &fills, &line_color, &panel, true);
        JsValue::UNDEFINED
    });
    let line_color = colors.text.clone();
    let panel = colors.panel.clone();
    let on_top = Callback::new(move |u: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
        draw_reference_lines(&u, &lines, &line_color, &panel, false);
        JsValue::UNDEFINED
    });
    let hooks = obj(&[
        (
            "drawClear",
            Array::of1(shade.as_ref().unchecked_ref::<Function>()).into(),
        ),
        (
            "draw",
            Array::of1(on_top.as_ref().unchecked_ref::<Function>()).into(),
        ),
    ]);
    callbacks.push(shade);
    callbacks.push(on_top);

    let opts = obj(&[
        ("width", options.width.into()),
        ("height", (options.height - 30.0).max(60.0).into()),
        ("series", series.into()),
        ("axes", axes.into()),
        ("scales", scales.into()),
        (
            "legend",
            obj(&[("show", true.into()), ("live", true.into())]).into(),
        ),
        ("bands", bands.into()),
        ("hooks", hooks.clone().into()),
        (
            "cursor",
            obj(&[
                ("sync", obj(&[("key", options.sync_key.into())]).into()),
                (
                    "drag",
                    obj(&[
                        ("x", is_time.into()),
                        ("y", false.into()),
                        // A drag changes the dashboard's range; don't zoom just this chart.
                        ("setScale", options.on_zoom.is_none().into()),
                    ])
                    .into(),
                ),
            ])
            .into(),
        ),
    ]);
    if is_time {
        set(&opts, "ms", 1);
        let tz = options.time_zone.to_string();
        let tz_date = Callback::new(move |ts: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
            let date = js_sys::Date::new(&ts);
            UPlot::tz_date(&date, &tz).into()
        });
        set(
            &opts,
            "tzDate",
            tz_date.as_ref().unchecked_ref::<Function>().clone(),
        );
        callbacks.push(tz_date);
    }

    // Drag to zoom: report the selected range instead of zooming locally,
    // so the whole dashboard moves to it (as in Grafana).
    if let (true, Some(callback)) = (is_time, options.on_zoom.clone()) {
        let on_select = Callback::new(move |u: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
            let select = Reflect::get(&u, &"select".into()).unwrap_or(JsValue::NULL);
            let left = Reflect::get(&select, &"left".into())
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let width = Reflect::get(&select, &"width".into())
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if width > 2.0 {
                let pos_to_val: Function = Reflect::get(&u, &"posToVal".into())
                    .unwrap()
                    .unchecked_into();
                let at = |px: f64| {
                    pos_to_val
                        .call2(&u, &px.into(), &"x".into())
                        .ok()
                        .and_then(|v| v.as_f64())
                };
                if let (Some(from), Some(to)) = (at(left), at(left + width)) {
                    callback(from, to);
                }
            }
            JsValue::UNDEFINED
        });
        set(
            &hooks,
            "setSelect",
            Array::of1(on_select.as_ref().unchecked_ref::<Function>()),
        );
        callbacks.push(on_select);
    }

    let plot = UPlot::new(&opts, &data, target);
    let chart = Chart {
        plot,
        target: target.clone(),
        _callbacks: callbacks,
    };
    chart.fit(options.width, options.height);
    chart
}

fn number(target: &JsValue, key: &str) -> f64 {
    Reflect::get(target, &key.into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

/// Paints benchmark bands (with their labels) and shades buckets with too
/// few documents, under everything else on the canvas.
fn shade_background(u: &JsValue, thresholds: &[Threshold], low_sample: &[bool], xs: &[f64]) {
    let Ok(ctx) = Reflect::get(u, &"ctx".into())
        .and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>())
    else {
        return;
    };
    let bbox = Reflect::get(u, &"bbox".into()).unwrap_or(JsValue::NULL);
    let (left, top, width, height) = (
        number(&bbox, "left"),
        number(&bbox, "top"),
        number(&bbox, "width"),
        number(&bbox, "height"),
    );
    let Ok(val_to_pos) = Reflect::get(u, &"valToPos".into()).and_then(|f| f.dyn_into::<Function>())
    else {
        return;
    };
    let pos = |v: f64, scale: &str| {
        val_to_pos
            .call3(u, &v.into(), &scale.into(), &true.into())
            .ok()
            .and_then(|p| p.as_f64())
            .unwrap_or(0.0)
    };
    let scales = Reflect::get(u, &"scales".into()).unwrap_or(JsValue::NULL);
    let y = Reflect::get(&scales, &"y".into()).unwrap_or(JsValue::NULL);
    let (y_min, y_max) = (number(&y, "min"), number(&y, "max"));
    let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());

    ctx.save();
    for (i, t) in thresholds.iter().enumerate() {
        let lo = t.from.unwrap_or(y_min).max(y_min);
        let hi = t.to.unwrap_or(y_max).min(y_max);
        if hi <= lo {
            continue;
        }
        let (y0, y1) = (pos(hi, "y"), pos(lo, "y"));
        let color = threshold_color(t, i);
        ctx.set_global_alpha(0.07);
        ctx.set_fill_style_str(&color);
        ctx.fill_rect(left, y0, width, y1 - y0);
        ctx.set_global_alpha(0.55);
        ctx.set_font(&format!("{}px sans-serif", (10.0 * dpr).round()));
        ctx.set_text_align("right");
        let _ = ctx.fill_text(&t.label, left + width - 4.0 * dpr, y0 + 12.0 * dpr);
    }
    if low_sample.iter().any(|l| *l) && xs.len() > 1 {
        ctx.set_global_alpha(0.28);
        ctx.set_fill_style_str("#7f7f8f");
        let half = width / xs.len() as f64 / 2.0;
        for (i, low) in low_sample.iter().enumerate() {
            if *low && let Some(x) = xs.get(i) {
                let px = pos(*x, "x");
                ctx.fill_rect(px - half, top, half * 2.0, height);
            }
        }
    }
    ctx.restore();
}

/// Reference lines: their fills (`fills`, drawn behind the data) or the
/// lines and labels themselves (drawn over it). Each comes with its scale.
fn draw_reference_lines(
    u: &JsValue,
    lines: &[(RefLine, &str)],
    default_color: &str,
    background: &str,
    fills: bool,
) {
    if lines.is_empty() {
        return;
    }
    let Ok(ctx) = Reflect::get(u, &"ctx".into())
        .and_then(|c| c.dyn_into::<web_sys::CanvasRenderingContext2d>())
    else {
        return;
    };
    let bbox = Reflect::get(u, &"bbox".into()).unwrap_or(JsValue::NULL);
    let (left, top, width, height) = (
        number(&bbox, "left"),
        number(&bbox, "top"),
        number(&bbox, "width"),
        number(&bbox, "height"),
    );
    let Ok(val_to_pos) = Reflect::get(u, &"valToPos".into()).and_then(|f| f.dyn_into::<Function>())
    else {
        return;
    };
    let pos = |v: f64, scale: &str| {
        val_to_pos
            .call3(u, &v.into(), &scale.into(), &true.into())
            .ok()
            .and_then(|p| p.as_f64())
    };
    let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());

    ctx.save();
    ctx.begin_path();
    ctx.rect(left, top, width, height);
    ctx.clip();
    for (line, scale) in lines {
        let Some(p) = pos(line.value, scale).filter(|p| p.is_finite()) else {
            continue;
        };
        let color = line.color.as_deref().unwrap_or(default_color);
        let vertical = *scale == "x";
        if fills {
            // "Above" is towards larger values: up, or right on the x axis.
            let rect = match (line.fill, vertical) {
                (ReferenceFill::None, _) => continue,
                (ReferenceFill::Above, false) => (left, top, width, p - top),
                (ReferenceFill::Below, false) => (left, p, width, top + height - p),
                (ReferenceFill::Above, true) => (p, top, left + width - p, height),
                (ReferenceFill::Below, true) => (left, top, p - left, height),
            };
            ctx.set_global_alpha(0.12);
            ctx.set_fill_style_str(color);
            ctx.fill_rect(rect.0, rect.1, rect.2, rect.3);
            continue;
        }
        let dash: &[f64] = match line.style {
            LineStyle::Solid => &[],
            LineStyle::Dashed => &[6.0, 4.0],
            LineStyle::Dotted => &[1.5, 3.0],
        };
        let pattern = Array::new();
        for d in dash {
            pattern.push(&(d * dpr).into());
        }
        let _ = ctx.set_line_dash(&pattern);
        ctx.set_global_alpha(0.9);
        ctx.set_stroke_style_str(color);
        ctx.set_line_width(1.5 * dpr);
        ctx.begin_path();
        if vertical {
            ctx.move_to(p, top);
            ctx.line_to(p, top + height);
        } else {
            ctx.move_to(left, p);
            ctx.line_to(left + width, p);
        }
        ctx.stroke();
        if let Some(label) = &line.label {
            let label = format!(
                "{label} ({})",
                format::value(line.value, line.format.as_ref())
            );
            let label = label.as_str();
            ctx.set_font(&format!("{}px sans-serif", (11.0 * dpr).round()));
            ctx.set_text_align("left");
            let (x, y) = if vertical {
                (p + 4.0 * dpr, top + 12.0 * dpr)
            } else {
                (left + 4.0 * dpr, p - 4.0 * dpr)
            };
            // A backing keeps the label readable over dense data.
            let width = ctx.measure_text(label).map_or(0.0, |m| m.width());
            ctx.set_global_alpha(0.8);
            ctx.set_fill_style_str(background);
            ctx.fill_rect(x - 2.0 * dpr, y - 10.0 * dpr, width + 4.0 * dpr, 13.0 * dpr);
            ctx.set_global_alpha(1.0);
            ctx.set_fill_style_str(color);
            let _ = ctx.fill_text(label, x, y);
        }
    }
    ctx.restore();
}
