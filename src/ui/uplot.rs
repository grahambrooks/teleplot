//! Bindings to the vendored uPlot (`public/vendor/uplot`), and building a
//! chart from [`Xy`] series. Browser only.

use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::format;
use super::series::{Heatmap, RefLine, Style, XAxis, Xy, XySeries, stacked};
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
    pub dark: bool,
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
    // Log-histogram bars are bins from each x to its end, on a log x axis.
    let bins = !xy.bin_ends.is_empty() && !xs.is_empty();
    let mut bin_series: Vec<(Vec<Option<f64>>, String, &'static str)> = Vec::new();
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
        if bins && s.kind.is_bar() {
            set(&spec, "paths", no_path(&mut callbacks));
            bin_series.push((
                drawn[i].clone(),
                format!("{color}cc"),
                if on_right(s) { "y2" } else { "y" },
            ));
        } else if s.kind.is_bar() {
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
    let x_axis = axis(&[]);
    if bins {
        let x_ticks = tick_formatter(xy.x_format.clone());
        set(&x_axis, "values", function(&x_ticks));
        callbacks.push(x_ticks);
    }
    let axes = Array::of2(
        &x_axis.into(),
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
    let x_scale = obj(&[("time", is_time.into())]);
    if bins {
        let end = xy.bin_ends.last().copied().unwrap_or(xs[xs.len() - 1]);
        set(&x_scale, "distr", 3);
        set(&x_scale, "range", Array::of2(&xs[0].into(), &end.into()));
    }
    let scales = obj(&[("x", x_scale.into()), ("y", y_scale.into())]);
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
    let bin_starts = xs.clone();
    let bin_ends = xy.bin_ends.clone();
    let on_top = Callback::new(move |u: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
        draw_bins(&u, &bin_series, &bin_starts, &bin_ends);
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
        time_axis(&opts, options.time_zone, &mut callbacks);
        if let Some(callback) = options.on_zoom.clone() {
            zoom_on_select(&hooks, callback, &mut callbacks);
        }
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

/// Epoch-millisecond x values, shown in the dashboard's time zone.
fn time_axis(opts: &Object, time_zone: &str, callbacks: &mut Vec<Callback>) {
    set(opts, "ms", 1);
    let tz = time_zone.to_string();
    let tz_date = Callback::new(move |ts: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
        let date = js_sys::Date::new(&ts);
        UPlot::tz_date(&date, &tz).into()
    });
    set(
        opts,
        "tzDate",
        tz_date.as_ref().unchecked_ref::<Function>().clone(),
    );
    callbacks.push(tz_date);
}

/// Drag to zoom: report the selected range instead of zooming locally,
/// so the whole dashboard moves to it (as in Grafana).
fn zoom_on_select(hooks: &Object, callback: Rc<dyn Fn(f64, f64)>, callbacks: &mut Vec<Callback>) {
    let on_select = Callback::new(move |u: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
        let select = Reflect::get(&u, &"select".into()).unwrap_or(JsValue::NULL);
        let (left, width) = (number(&select, "left"), number(&select, "width"));
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
        hooks,
        "setSelect",
        Array::of1(on_select.as_ref().unchecked_ref::<Function>()),
    );
    callbacks.push(on_select);
}

fn function(callback: &Callback) -> Function {
    callback.as_ref().unchecked_ref::<Function>().clone()
}

/// A series drawn by a hook rather than by uPlot (bins, heatmap cells).
fn no_path(callbacks: &mut Vec<Callback>) -> Function {
    let none = Callback::new(|_: JsValue, _: JsValue, _: JsValue, _: JsValue| JsValue::NULL);
    let f = function(&none);
    callbacks.push(none);
    f
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

/// Canvas, plot area (left, top, width, height) and a value → pixel
/// function, for drawing hooks.
struct Plot<'a> {
    u: &'a JsValue,
    ctx: web_sys::CanvasRenderingContext2d,
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    val_to_pos: Function,
}

impl<'a> Plot<'a> {
    fn of(u: &'a JsValue) -> Option<Self> {
        let ctx = Reflect::get(u, &"ctx".into())
            .ok()?
            .dyn_into::<web_sys::CanvasRenderingContext2d>()
            .ok()?;
        let bbox = Reflect::get(u, &"bbox".into()).ok()?;
        let val_to_pos = Reflect::get(u, &"valToPos".into())
            .ok()?
            .dyn_into::<Function>()
            .ok()?;
        Some(Plot {
            u,
            ctx,
            left: number(&bbox, "left"),
            top: number(&bbox, "top"),
            width: number(&bbox, "width"),
            height: number(&bbox, "height"),
            val_to_pos,
        })
    }

    /// Canvas pixels for a value on a scale.
    fn pos(&self, v: f64, scale: &str) -> Option<f64> {
        self.val_to_pos
            .call3(self.u, &v.into(), &scale.into(), &true.into())
            .ok()
            .and_then(|p| p.as_f64())
            .filter(|p| p.is_finite())
    }

    /// Keeps drawing inside the plot area until `restore`.
    fn clip(&self) {
        self.ctx.save();
        self.ctx.begin_path();
        self.ctx.rect(self.left, self.top, self.width, self.height);
        self.ctx.clip();
    }
}

fn dpr() -> f64 {
    web_sys::window().map_or(1.0, |w| w.device_pixel_ratio())
}

/// Histogram bins, each from its start to its end on the (log) x axis,
/// with a small gap between neighbours.
fn draw_bins(
    u: &JsValue,
    series: &[(Vec<Option<f64>>, String, &str)],
    starts: &[f64],
    ends: &[f64],
) {
    if series.is_empty() {
        return;
    }
    let Some(plot) = Plot::of(u) else { return };
    let gap = dpr();
    let bottom = plot.top + plot.height;
    plot.clip();
    for (values, color, scale) in series {
        plot.ctx.set_fill_style_str(color);
        for (i, v) in values.iter().enumerate() {
            let (Some(v), Some(start), Some(end)) = (v, starts.get(i), ends.get(i)) else {
                continue;
            };
            let (Some(x0), Some(x1), Some(y)) = (
                plot.pos(*start, "x"),
                plot.pos(*end, "x"),
                plot.pos(*v, scale),
            ) else {
                continue;
            };
            let base = plot.pos(0.0, scale).map_or(bottom, |b| b.min(bottom));
            plot.ctx
                .fill_rect(x0 + gap, y, (x1 - x0 - 2.0 * gap).max(1.0), base - y);
        }
    }
    plot.ctx.restore();
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
    // Labels of vertical lines step down the chart, so close lines (p85 and
    // p95, say) don't print over each other.
    let mut vertical_labels = 0.0;
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
                vertical_labels += 1.0;
                (p + 4.0 * dpr, top + (vertical_labels * 15.0 - 3.0) * dpr)
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

/// Room on the right of a heatmap for its colour key.
const KEY_WIDTH: f64 = 64.0;

/// Draws a heatmap into `target`: cells coloured on the sequential ramp,
/// with the colour key on the right. Time x axes drag-select like line
/// charts; the legend reads the cell under the cursor.
pub fn heatmap(target: &web_sys::HtmlElement, hm: &Heatmap, options: Options) -> Chart {
    let colors = theme::chart_colors();
    let mut callbacks = Vec::new();
    let (nx, ny) = (hm.x.len(), hm.y.len().max(1));
    // Cells span [start, end) on a time axis, or [i, i + 1) for categories;
    // uPlot's x values are their middles, so the cursor snaps to cells.
    let (starts, ends, is_time): (Vec<f64>, Vec<f64>, bool) = match &hm.x {
        XAxis::Time(starts) => (starts.clone(), hm.x_ends.clone(), true),
        _ => (
            (0..nx).map(|i| i as f64).collect(),
            (1..=nx).map(|i| i as f64).collect(),
            false,
        ),
    };
    let middles: Vec<f64> = starts
        .iter()
        .zip(&ends)
        .map(|(s, e)| (s + e) / 2.0)
        .collect();
    let data = Array::of2(
        &js_sys::Float64Array::from(middles.as_slice()),
        &(0..nx).map(|_| JsValue::NULL).collect::<Array>(),
    );

    // The legend: x as its bucket, the value as "row: value" under the cursor.
    let cells = Rc::new(hm.cells.clone());
    let rows = Rc::new(hm.y.clone());
    let format = hm.format.clone();
    let value = Callback::new(move |u: JsValue, _v: JsValue, _s: JsValue, idx: JsValue| {
        let cursor = Reflect::get(&u, &"cursor".into()).unwrap_or(JsValue::NULL);
        let top = number(&cursor, "top");
        let row = Reflect::get(&u, &"posToVal".into())
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
            .and_then(|f| f.call2(&u, &top.into(), &"y".into()).ok())
            .and_then(|v| v.as_f64())
            .filter(|v| *v >= 0.0)
            .map(|v| v.floor() as usize);
        let text = idx
            .as_f64()
            .zip(row)
            .and_then(|(i, j)| {
                let label = rows.get(j)?;
                let v = cells.get(i as usize)?.get(j).copied().flatten();
                Some(format!(
                    "{label}: {}",
                    v.map_or("–".into(), |v| format::value(v, format.as_ref()))
                ))
            })
            .unwrap_or_else(|| "–".into());
        JsValue::from_str(&text)
    });
    // The legend names the cell's bucket: its category, or the date it starts.
    let x_labels: Vec<String> = match &hm.x {
        XAxis::Category(labels) => labels.clone(),
        _ => starts
            .iter()
            .map(|ms| date(*ms, options.time_zone))
            .collect(),
    };
    let x_value = Callback::new(move |_u: JsValue, _v: JsValue, _s: JsValue, idx: JsValue| {
        let label = idx.as_f64().and_then(|i| x_labels.get(i as usize).cloned());
        JsValue::from_str(&label.unwrap_or_default())
    });
    let x_series = obj(&[("value", function(&x_value).into())]);
    callbacks.push(x_value);
    let series = Array::of2(
        &x_series.into(),
        &obj(&[
            ("label", hm.label.clone().into()),
            ("stroke", theme::sequential(0.75, false).into()),
            ("fill", theme::sequential(0.75, false).into()),
            ("paths", no_path(&mut callbacks).into()),
            ("points", obj(&[("show", false.into())]).into()),
            ("value", function(&value).into()),
        ])
        .into(),
    );
    callbacks.push(value);

    // Category axes put a label in the middle of each cell.
    let mut category_axis = |labels: Vec<String>, count: usize, extra: &[(&str, JsValue)]| {
        let splits = Callback::new(move |_u: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
            let out = Array::new();
            for i in 0..count {
                out.push(&(i as f64 + 0.5).into());
            }
            out.into()
        });
        let values = Callback::new(
            move |_u: JsValue, splits: JsValue, _a: JsValue, _b: JsValue| {
                let out = Array::new();
                for v in Array::from(&splits).iter() {
                    let label = v
                        .as_f64()
                        .and_then(|v| labels.get(v.floor() as usize).cloned());
                    out.push(&JsValue::from_str(&label.unwrap_or_default()));
                }
                out.into()
            },
        );
        let a = obj(&[
            ("stroke", colors.text.clone().into()),
            ("grid", obj(&[("show", false.into())]).into()),
            ("ticks", obj(&[("show", false.into())]).into()),
            ("splits", function(&splits).into()),
            ("values", function(&values).into()),
        ]);
        for (k, v) in extra {
            set(&a, k, v.clone());
        }
        callbacks.push(splits);
        callbacks.push(values);
        a
    };
    let y_axis = category_axis(
        hm.y.clone(),
        ny,
        &[("scale", "y".into()), ("size", 80.into())],
    );
    let x_axis = match &hm.x {
        XAxis::Category(labels) => category_axis(labels.clone(), nx, &[]),
        _ => obj(&[
            ("stroke", colors.text.clone().into()),
            ("grid", obj(&[("show", false.into())]).into()),
            (
                "ticks",
                obj(&[("stroke", colors.grid.clone().into()), ("width", 1.into())]).into(),
            ),
        ]),
    };
    let axes = Array::of2(&x_axis.into(), &y_axis.into());

    let x_range = Array::of2(
        &starts.first().copied().unwrap_or(0.0).into(),
        &ends.last().copied().unwrap_or(1.0).into(),
    );
    let scales = obj(&[
        (
            "x",
            obj(&[("time", is_time.into()), ("range", x_range.into())]).into(),
        ),
        (
            "y",
            obj(&[("range", Array::of2(&0.into(), &(ny as f64).into()).into())]).into(),
        ),
    ]);

    let cells = hm.cells.clone();
    let extent = hm.extent();
    let format = hm.format.clone();
    let (dark, text) = (options.dark, colors.text.clone());
    let draw_cells = Callback::new(move |u: JsValue, _a: JsValue, _b: JsValue, _c: JsValue| {
        draw_heatmap(&u, &cells, &starts, &ends, extent, dark);
        draw_color_key(&u, extent, format.as_ref(), dark, &text);
        JsValue::UNDEFINED
    });
    let hooks = obj(&[("draw", Array::of1(&function(&draw_cells)).into())]);
    callbacks.push(draw_cells);

    let opts = obj(&[
        ("width", options.width.into()),
        ("height", (options.height - 30.0).max(60.0).into()),
        ("series", series.into()),
        ("axes", axes.into()),
        ("scales", scales.into()),
        (
            "padding",
            Array::of4(&8.into(), &KEY_WIDTH.into(), &0.into(), &0.into()).into(),
        ),
        (
            "legend",
            obj(&[("show", true.into()), ("live", true.into())]).into(),
        ),
        ("hooks", hooks.clone().into()),
        (
            "cursor",
            obj(&[
                ("sync", obj(&[("key", options.sync_key.into())]).into()),
                ("points", obj(&[("show", false.into())]).into()),
                (
                    "drag",
                    obj(&[
                        ("x", is_time.into()),
                        ("y", false.into()),
                        ("setScale", options.on_zoom.is_none().into()),
                    ])
                    .into(),
                ),
            ])
            .into(),
        ),
    ]);
    if is_time {
        time_axis(&opts, options.time_zone, &mut callbacks);
        if let Some(callback) = options.on_zoom.clone() {
            zoom_on_select(&hooks, callback, &mut callbacks);
        }
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

/// A date (`2026-04-06`) in the dashboard's time zone.
fn date(ms: f64, time_zone: &str) -> String {
    let options = obj(&[
        ("timeZone", time_zone.into()),
        ("year", "numeric".into()),
        ("month", "2-digit".into()),
        ("day", "2-digit".into()),
    ]);
    js_sys::Date::new(&ms.into())
        .to_locale_date_string("en-CA", &options)
        .into()
}

/// Where `v` sits between the extent's ends, 0–1.
fn share(v: f64, extent: Option<(f64, f64)>) -> f64 {
    match extent {
        Some((lo, hi)) if hi > lo => (v - lo) / (hi - lo),
        _ => 1.0,
    }
}

fn draw_heatmap(
    u: &JsValue,
    cells: &[Vec<Option<f64>>],
    starts: &[f64],
    ends: &[f64],
    extent: Option<(f64, f64)>,
    dark: bool,
) {
    let Some(plot) = Plot::of(u) else { return };
    let gap = dpr();
    plot.clip();
    for (i, column) in cells.iter().enumerate() {
        let (Some(x0), Some(x1)) = (
            starts.get(i).and_then(|v| plot.pos(*v, "x")),
            ends.get(i).and_then(|v| plot.pos(*v, "x")),
        ) else {
            continue;
        };
        for (j, v) in column.iter().enumerate() {
            let (Some(v), Some(y0), Some(y1)) =
                (v, plot.pos(j as f64 + 1.0, "y"), plot.pos(j as f64, "y"))
            else {
                continue;
            };
            plot.ctx
                .set_fill_style_str(theme::sequential(share(*v, extent), dark));
            plot.ctx.fill_rect(
                x0 + gap / 2.0,
                y0 + gap / 2.0,
                (x1 - x0 - gap).max(1.0),
                (y1 - y0 - gap).max(1.0),
            );
        }
    }
    plot.ctx.restore();
}

/// The colour key: the ramp from the smallest to the largest cell, labelled,
/// in the padding to the right of the plot (so PNG exports include it).
fn draw_color_key(
    u: &JsValue,
    extent: Option<(f64, f64)>,
    format: Option<&Format>,
    dark: bool,
    text: &str,
) {
    let Some((lo, hi)) = extent else { return };
    let Some(plot) = Plot::of(u) else { return };
    let dpr = dpr();
    let x = plot.left + plot.width + 10.0 * dpr;
    let (w, steps) = (10.0 * dpr, 13);
    let step = plot.height / steps as f64;
    plot.ctx.save();
    for k in 0..steps {
        let t = k as f64 / (steps - 1) as f64;
        plot.ctx.set_fill_style_str(theme::sequential(t, dark));
        // Largest at the top.
        let y = plot.top + plot.height - (k + 1) as f64 * step;
        plot.ctx.fill_rect(x, y, w, step + 0.5);
    }
    plot.ctx.set_fill_style_str(text);
    plot.ctx
        .set_font(&format!("{}px sans-serif", (10.0 * dpr).round()));
    plot.ctx.set_text_align("left");
    let label_x = x + w + 4.0 * dpr;
    let _ = plot
        .ctx
        .fill_text(&format::value(hi, format), label_x, plot.top + 9.0 * dpr);
    let _ = plot
        .ctx
        .fill_text(&format::value(lo, format), label_x, plot.top + plot.height);
    plot.ctx.restore();
}
