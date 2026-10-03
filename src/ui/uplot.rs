//! Bindings to the vendored uPlot (`public/vendor/uplot`), and building a
//! chart from [`Xy`] series. Browser only.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use super::format;
use super::series::{XAxis, Xy, stacked};
use super::theme;

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
    _callbacks: Vec<Callback>,
}

impl Drop for Chart {
    fn drop(&mut self) {
        self.plot.destroy();
    }
}

impl Chart {
    pub fn resize(&self, width: f64, height: f64) {
        let size = Object::new();
        set(&size, "width", width);
        set(&size, "height", height);
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
    pub height: f64,
    /// Charts with the same key share a crosshair (Grafana's shared tooltip).
    pub sync_key: &'a str,
    pub time_zone: &'a str,
    /// Called with the selected x range (epoch ms) after a drag on a time chart.
    pub on_zoom: Option<Rc<dyn Fn(f64, f64)>>,
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
        ]);
        if s.kind.is_bar()
            || s.kind.is_stacked()
            || matches!(s.kind, crate::model::SeriesType::Area)
        {
            set(
                &spec,
                "fill",
                format!("{color}{}", if s.kind.is_bar() { "cc" } else { "40" }),
            );
        }
        if s.kind.is_bar() {
            let opts = obj(&[
                ("size", Array::of2(&0.7.into(), &60.into()).into()),
                ("align", 0.into()),
            ]);
            set(&spec, "paths", bars(&opts));
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

    // Y axis ticks use the first series' format; percentage stacks are 0–100%.
    let percentage = xy.series.iter().any(|s| s.kind.is_percentage());
    let y_format = if percentage {
        Some(crate::model::Format {
            kind: crate::model::FormatKind::Percent,
            decimals: Some(0),
            suffix: None,
            compact: false,
        })
    } else {
        xy.series.first().and_then(|s| s.format.clone())
    };
    let ticks = Callback::new(
        move |_u: JsValue, splits: JsValue, _a: JsValue, _b: JsValue| {
            let out = Array::new();
            for v in Array::from(&splits).iter() {
                let text = v
                    .as_f64()
                    .map_or(String::new(), |v| format::value(v, y_format.as_ref()));
                out.push(&JsValue::from_str(&text));
            }
            out.into()
        },
    );

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
    let axes = Array::of2(
        &axis(&[]).into(),
        &axis(&[
            (
                "values",
                ticks.as_ref().unchecked_ref::<Function>().clone().into(),
            ),
            ("size", 64.into()),
        ])
        .into(),
    );
    callbacks.push(ticks);

    let y_scale = obj(&[]);
    if percentage {
        set(&y_scale, "range", Array::of2(&0.into(), &1.into()));
    }
    let opts = obj(&[
        ("width", options.width.into()),
        ("height", options.height.into()),
        ("series", series.into()),
        ("axes", axes.into()),
        (
            "scales",
            obj(&[
                ("x", obj(&[("time", is_time.into())]).into()),
                ("y", y_scale.into()),
            ])
            .into(),
        ),
        (
            "legend",
            obj(&[("show", true.into()), ("live", true.into())]).into(),
        ),
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
    let selection = Rc::new(RefCell::new(options.on_zoom.clone()));
    if is_time && options.on_zoom.is_some() {
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
                if let (Some(from), Some(to), Some(callback)) =
                    (at(left), at(left + width), selection.borrow().clone())
                {
                    callback(from, to);
                }
            }
            JsValue::UNDEFINED
        });
        let hooks = obj(&[(
            "setSelect",
            Array::of1(on_select.as_ref().unchecked_ref::<Function>()).into(),
        )]);
        set(&opts, "hooks", hooks);
        callbacks.push(on_select);
    }

    let plot = UPlot::new(&opts, &data, target);
    Chart {
        plot,
        _callbacks: callbacks,
    }
}
