//! Charts drawn as SVG: gauges, treemaps, waffles and calendars.

use leptos::prelude::*;

use super::format;
use super::panels::{arc, date};
use super::series::{cell_text, first};
use super::theme::{palette, sequential};
use crate::model::{Chart, ColorByValue, Encoding, Format, GaugeShape, PanelData};

/// A round number at or above `v` (1, 2, 2.5, 5 × 10ⁿ): a gauge's default max.
pub fn nice_ceiling(v: f64) -> f64 {
    if v <= 0.0 || !v.is_finite() {
        return 1.0;
    }
    let magnitude = 10f64.powf(v.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * magnitude)
        .find(|n| *n >= v)
        .unwrap_or(10.0 * magnitude)
}

/// Where a gauge's colour bands run, as (from, to, colour) within min–max.
pub fn bands(by: &ColorByValue, min: f64, max: f64) -> Vec<(f64, f64, String)> {
    let at = |stop: f64| {
        if by.percent {
            min + (max - min) * stop / 100.0
        } else {
            stop
        }
    };
    let mut out = Vec::new();
    for (i, stop) in by.stops.iter().enumerate() {
        let from = if i == 0 && by.extend_below {
            min
        } else {
            at(stop.from).max(min)
        };
        let to = by
            .stops
            .get(i + 1)
            .map(|next| at(next.from))
            .or_else(|| (!by.extend_above).then(|| by.to.map(at)).flatten())
            .unwrap_or(max)
            .min(max);
        if to > from {
            out.push((from, to, stop.color.clone()));
        }
    }
    out
}

#[component]
pub fn GaugeChart(chart: Chart, data: PanelData) -> impl IntoView {
    let Some((layer, d)) = chart
        .layers
        .iter()
        .zip(&data.layers)
        .find(|(l, _)| matches!(l.encoding, Encoding::Gauge { .. }))
    else {
        return ().into_any();
    };
    let Encoding::Gauge {
        value,
        min,
        max,
        goal,
        shape,
        bands: band_spec,
    } = &layer.encoding
    else {
        return ().into_any();
    };
    let number = |id: &Option<String>| {
        id.as_deref()
            .and_then(|c| first(d, c))
            .and_then(|c| c.number())
    };
    let Some(v) = first(d, value).and_then(|c| c.number()) else {
        return view! { <div class="no-data">"No value"</div> }.into_any();
    };
    let goal = number(goal);
    let min = number(min).unwrap_or(0.0);
    let max = number(max)
        .unwrap_or_else(|| nice_ceiling(v.max(goal.unwrap_or(v))))
        .max(min + f64::MIN_POSITIVE);
    let format: Option<Format> = layer.column(value).and_then(|c| c.display_format());
    let label = layer
        .column(value)
        .and_then(|c| c.label.clone())
        .unwrap_or_default();
    let bands = band_spec
        .as_ref()
        .map(|b| bands(b, min, max))
        .unwrap_or_default();
    let color = band_spec
        .as_ref()
        .and_then(|b| b.color_for(v, min, max).map(String::from))
        .unwrap_or_else(|| "var(--accent)".into());
    let share = |x: f64| ((x - min) / (max - min)).clamp(0.0, 1.0);
    let text = format::value(v, format.as_ref());
    let title = format!(
        "{label}: {text} (scale {} to {}{})",
        format::value(min, format.as_ref()),
        format::value(max, format.as_ref()),
        goal.map(|g| format!(", goal {}", format::value(g, format.as_ref())))
            .unwrap_or_default()
    );

    let svg = match shape {
        GaugeShape::HorizontalBullet | GaugeShape::VerticalBullet => {
            let vertical = *shape == GaugeShape::VerticalBullet;
            // Lay out along x (0–100), then turn it upright for vertical bullets.
            let rect = |from: f64, to: f64, y: f64, h: f64, fill: String, opacity: f64| {
                view! { <rect x=format!("{:.2}", from * 100.0) y=y width=format!("{:.2}", (to - from) * 100.0) height=h fill=fill opacity=opacity></rect> }
            };
            let track: Vec<_> = if bands.is_empty() {
                vec![rect(0.0, 1.0, 8.0, 14.0, "var(--border)".into(), 1.0)]
            } else {
                bands
                    .iter()
                    .map(|(f, t, c)| rect(share(*f), share(*t), 8.0, 14.0, c.clone(), 0.35))
                    .collect()
            };
            let goal_tick = goal.map(|g| {
                let x = share(g) * 100.0;
                view! { <line x1=x x2=x y1=4 y2=26 stroke="var(--text)" stroke-width=1.5></line> }
            });
            let transform = vertical.then_some("rotate(-90 50 50) translate(0 35)");
            view! {
                <svg viewBox="0 0 100 30" class="gauge-svg" class:vertical=vertical preserveAspectRatio="none">
                    <g transform=transform>
                        {track}
                        {rect(0.0, share(v), 12.0, 6.0, color.clone(), 1.0)}
                        {goal_tick}
                    </g>
                </svg>
            }
            .into_any()
        }
        _ => {
            let (start, sweep) = match shape {
                GaugeShape::SemiCircle => (std::f64::consts::PI, std::f64::consts::PI),
                GaugeShape::Circle => (-std::f64::consts::FRAC_PI_2, std::f64::consts::TAU),
                _ => (0.75 * std::f64::consts::PI, 1.5 * std::f64::consts::PI),
            };
            let angle = |x: f64| start + share(x) * sweep;
            let ring = |from: f64, to: f64, outer: f64, inner: f64, fill: String, opacity: f64| {
                view! { <path d=arc(50.0, 50.0, outer, inner, angle(from), angle(to)) fill=fill opacity=opacity></path> }
            };
            let track: Vec<_> = if bands.is_empty() {
                vec![ring(min, max, 46.0, 36.0, "var(--border)".into(), 1.0)]
            } else {
                bands
                    .iter()
                    .map(|(f, t, c)| ring(*f, *t, 46.0, 36.0, c.clone(), 0.35))
                    .collect()
            };
            let goal_tick = goal.map(|g| {
                let a = angle(g);
                let (x1, y1) = (50.0 + 32.0 * a.cos(), 50.0 + 32.0 * a.sin());
                let (x2, y2) = (50.0 + 49.0 * a.cos(), 50.0 + 49.0 * a.sin());
                view! { <line x1=x1 y1=y1 x2=x2 y2=y2 stroke="var(--text)" stroke-width=1.5></line> }
            });
            let value_ring =
                (v > min).then(|| ring(min, v.min(max), 46.0, 36.0, color.clone(), 1.0));
            let text_y = if *shape == GaugeShape::SemiCircle {
                46.0
            } else {
                54.0
            };
            view! {
                <svg viewBox="0 0 100 100" class="gauge-svg">
                    {track}
                    {value_ring}
                    {goal_tick}
                    <text x=50 y=text_y text-anchor="middle" class="gauge-value">{text.clone()}</text>
                </svg>
            }
            .into_any()
        }
    };
    let bullet = matches!(
        shape,
        GaugeShape::HorizontalBullet | GaugeShape::VerticalBullet
    );
    view! {
        <div class="gauge" title=title>
            <div class="metric-label">{label}</div>
            {bullet.then(|| view! { <div class="metric-value">{text.clone()}</div> })}
            {svg}
            <div class="gauge-scale muted small">
                <span>{format::value(min, format.as_ref())}</span>
                <span>{format::value(max, format.as_ref())}</span>
            </div>
        </div>
    }
    .into_any()
}

/// Groups and values of a pie-encoded layer: one (path, value) per row.
fn slices(chart: &Chart, data: &PanelData) -> (Vec<(Vec<String>, f64)>, Option<Format>) {
    for (layer, d) in chart.layers.iter().zip(&data.layers) {
        let Encoding::Pie { groups, values } = &layer.encoding else {
            continue;
        };
        let Some(value) = values.first() else {
            continue;
        };
        let format = layer.column(value).and_then(|c| c.display_format());
        let rows = (0..d.rows)
            .filter_map(|row| {
                let v = d.series(value)?.values.get(row)?.number()?;
                let path = groups
                    .iter()
                    .map(|g| {
                        d.series(g)
                            .map(|s| cell_text(&s.values[row]))
                            .unwrap_or_default()
                    })
                    .collect();
                (v > 0.0).then_some((path, v))
            })
            .collect();
        return (rows, format);
    }
    (Vec::new(), None)
}

/// Squarified treemap layout (Bruls et al.): rectangles (x, y, w, h) for
/// `values` (largest first) in the given box, close to square.
pub fn squarify(values: &[f64], x: f64, y: f64, w: f64, h: f64) -> Vec<(f64, f64, f64, f64)> {
    let total: f64 = values.iter().sum();
    if values.is_empty() || total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return vec![(x, y, 0.0, 0.0); values.len()];
    }
    let scale = w * h / total;
    let areas: Vec<f64> = values.iter().map(|v| v * scale).collect();
    let mut out = Vec::with_capacity(values.len());
    let (mut x, mut y, mut w, mut h) = (x, y, w, h);
    let mut i = 0;
    while i < areas.len() {
        let side = w.min(h);
        let worst = |row: &[f64]| {
            let sum: f64 = row.iter().sum();
            let (lo, hi) = row.iter().fold((f64::INFINITY, 0.0f64), |(lo, hi), a| {
                (lo.min(*a), hi.max(*a))
            });
            ((side * side * hi) / (sum * sum)).max((sum * sum) / (side * side * lo))
        };
        let mut end = i + 1;
        while end < areas.len() && worst(&areas[i..=end]) <= worst(&areas[i..end]) {
            end += 1;
        }
        let row = &areas[i..end];
        let sum: f64 = row.iter().sum();
        let thickness = sum / side;
        let mut offset = 0.0;
        for a in row {
            let length = a / thickness;
            if w >= h {
                out.push((x, y + offset, thickness, length));
            } else {
                out.push((x + offset, y, length, thickness));
            }
            offset += length;
        }
        if w >= h {
            x += thickness;
            w -= thickness;
        } else {
            y += thickness;
            h -= thickness;
        }
        i = end;
    }
    out
}

/// A treemap's top-level group: name, total, and its (name, value) parts.
type Group = (String, f64, Vec<(String, f64)>);

#[component]
pub fn TreemapChart(chart: Chart, data: PanelData) -> impl IntoView {
    let (rows, format) = slices(&chart, &data);
    let total: f64 = rows.iter().map(|(_, v)| v).sum();
    if total <= 0.0 {
        return view! { <div class="no-data">"No results"</div> }.into_any();
    }
    // Top level, largest first; each holds its second-level parts.
    let mut top: Vec<Group> = Vec::new();
    for (path, v) in &rows {
        let name = path.first().cloned().unwrap_or_else(|| "Total".into());
        let part = path.get(1).cloned();
        let entry = match top.iter_mut().position(|t| t.0 == name) {
            Some(i) => &mut top[i],
            None => {
                top.push((name, 0.0, Vec::new()));
                top.last_mut().unwrap()
            }
        };
        entry.1 += v;
        if let Some(part) = part {
            entry.2.push((part, *v));
        }
    }
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    let colors = palette();
    let (width, height) = (300.0, 180.0);
    let boxes = squarify(
        &top.iter().map(|t| t.1).collect::<Vec<_>>(),
        0.0,
        0.0,
        width,
        height,
    );
    let cells: Vec<_> = top
        .iter()
        .zip(boxes)
        .enumerate()
        .flat_map(|(i, ((name, v, parts), (x, y, w, h)))| {
            let color = colors[i % colors.len()];
            let mut parts = parts.clone();
            parts.sort_by(|a, b| b.1.total_cmp(&a.1));
            let inner = if parts.is_empty() {
                vec![(name.clone(), *v, (x, y, w, h))]
            } else {
                let rects = squarify(&parts.iter().map(|p| p.1).collect::<Vec<_>>(), x, y, w, h);
                parts.into_iter().zip(rects).map(|((p, pv), r)| (format!("{name} › {p}"), pv, r)).collect()
            };
            inner
                .into_iter()
                .map(|(label, value, (x, y, w, h))| {
                    let title = format!("{label}: {} ({:.1}%)", format::value(value, format.as_ref()), value / total * 100.0);
                    let short = label.rsplit(" › ").next().unwrap_or(&label).to_string();
                    let show = w > 40.0 && h > 18.0;
                    view! {
                        <g>
                            <rect x=x y=y width=w height=h fill=color stroke="var(--panel)" stroke-width=1.5><title>{title}</title></rect>
                            {show.then(|| view! {
                                <text x=x + 4.0 y=y + 12.0 class="treemap-label">{short}</text>
                                <text x=x + 4.0 y=y + 24.0 class="treemap-value">{format!("{:.1}%", value / total * 100.0)}</text>
                            })}
                        </g>
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();
    // The key: colour names the top level, which the cells' labels don't.
    let key = (top.iter().any(|t| !t.2.is_empty())).then(|| {
        top.iter()
            .enumerate()
            .map(|(i, (name, v, _))| {
                let swatch = format!("background:{}", colors[i % colors.len()]);
                view! {
                    <li>
                        <span class="swatch" style=swatch></span>
                        <span class="legend-label">{name.clone()}</span>
                        <span class="legend-value">{format!("{:.1}%", v / total * 100.0)}</span>
                    </li>
                }
            })
            .collect_view()
    });
    view! {
        <div class="treemap">
            {key.map(|key| view! { <ul class="treemap-key">{key}</ul> })}
            <svg viewBox=format!("0 0 {width} {height}") class="treemap-svg" preserveAspectRatio="none">{cells}</svg>
        </div>
    }
    .into_any()
}

/// Shares of 100 squares by largest remainder, so they always add up.
pub fn waffle_counts(values: &[f64]) -> Vec<usize> {
    let total: f64 = values.iter().sum();
    if total <= 0.0 {
        return vec![0; values.len()];
    }
    let exact: Vec<f64> = values.iter().map(|v| v / total * 100.0).collect();
    let mut counts: Vec<usize> = exact.iter().map(|e| e.floor() as usize).collect();
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| {
        (exact[*b] - exact[*b].floor()).total_cmp(&(exact[*a] - exact[*a].floor()))
    });
    let missing = 100 - counts.iter().sum::<usize>();
    for i in order.into_iter().take(missing) {
        counts[i] += 1;
    }
    counts
}

#[component]
pub fn WaffleChart(chart: Chart, data: PanelData) -> impl IntoView {
    let (rows, format) = slices(&chart, &data);
    let labels: Vec<String> = rows.iter().map(|(p, _)| p.join(" › ")).collect();
    let values: Vec<f64> = rows.iter().map(|(_, v)| *v).collect();
    let total: f64 = values.iter().sum();
    let counts = waffle_counts(&values);
    let colors = palette();
    let mut squares = Vec::new();
    let mut n = 0;
    for (i, count) in counts.iter().enumerate() {
        for _ in 0..*count {
            let (x, y) = ((n % 10) as f64 * 10.0, (n / 10) as f64 * 10.0);
            let title = format!(
                "{}: {}",
                labels[i],
                format::value(values[i], format.as_ref())
            );
            squares.push(view! {
                <rect x=x + 0.5 y=y + 0.5 width=9 height=9 rx=1 fill=colors[i % colors.len()]><title>{title}</title></rect>
            });
            n += 1;
        }
    }
    let legend: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let swatch = format!("background:{}", colors[i % colors.len()]);
            view! {
                <li>
                    <span class="swatch" style=swatch></span>
                    <span class="legend-label">{label.clone()}</span>
                    <span class="legend-value">{format!("{:.1}%", values[i] / total * 100.0)}</span>
                </li>
            }
        })
        .collect();
    view! {
        <div class="pie">
            <svg viewBox="0 0 100 100" class="pie-svg">{squares}</svg>
            <ul class="legend">{legend}</ul>
        </div>
    }
}

/// Monday = 0, in the dashboard's time zone.
fn weekday(ms: f64, tz: &str) -> usize {
    #[cfg(feature = "hydrate")]
    {
        let options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&options, &"timeZone".into(), &tz.into());
        let _ = js_sys::Reflect::set(&options, &"weekday".into(), &"short".into());
        let name: String = js_sys::Date::new(&ms.into())
            .to_locale_date_string("en-US", &options)
            .into();
        ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            .iter()
            .position(|d| name.starts_with(d))
            .unwrap_or(0)
    }
    #[cfg(not(feature = "hydrate"))]
    {
        let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
        jiff::Timestamp::from_millisecond(ms as i64)
            .map(|t| t.to_zoned(zone).weekday().to_monday_zero_offset() as usize)
            .unwrap_or(0)
    }
}

#[component]
pub fn CalendarChart(
    chart: Chart,
    data: PanelData,
    time_zone: String,
    dark: bool,
) -> impl IntoView {
    let Some((layer, d)) = chart
        .layers
        .iter()
        .zip(&data.layers)
        .find(|(l, _)| matches!(l.encoding, Encoding::Calendar { .. }))
    else {
        return ().into_any();
    };
    let Encoding::Calendar { date: day, value } = &layer.encoding else {
        return ().into_any();
    };
    let (Some(days), Some(values)) = (d.series(day), d.series(value)) else {
        return ().into_any();
    };
    let mut cells: Vec<(f64, Option<f64>)> = days
        .values
        .iter()
        .zip(&values.values)
        .filter_map(|(k, v)| Some((k.number()?, v.number())))
        .collect();
    cells.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some(&(first_day, _)) = cells.first() else {
        return view! { <div class="no-data">"No results"</div> }.into_any();
    };
    let format = layer.column(value).and_then(|c| c.display_format());
    let label = layer
        .column(value)
        .and_then(|c| c.label.clone())
        .unwrap_or_default();
    let max = cells.iter().filter_map(|c| c.1).fold(0.0, f64::max);
    let start = weekday(first_day, &time_zone);
    const DAY: f64 = 86_400_000.0;
    let size = 11.0;
    let squares: Vec<_> = cells
        .iter()
        .map(|(ms, v)| {
            // Days count from the first; rounding absorbs daylight-saving hours.
            let index = start + ((ms - first_day) / DAY).round() as usize;
            let (week, dow) = (index / 7, index % 7);
            let fill = match v {
                Some(v) if *v > 0.0 => sequential(0.15 + 0.85 * v / max.max(f64::MIN_POSITIVE), dark).to_string(),
                _ => "var(--border)".into(),
            };
            let title = format!("{}: {} {label}", date(*ms, &time_zone), v.map_or("–".into(), |v| format::value(v, format.as_ref())));
            view! {
                <rect x=30.0 + week as f64 * size y=14.0 + dow as f64 * size width=size - 2.0 height=size - 2.0 rx=2 fill=fill><title>{title}</title></rect>
            }
        })
        .collect();
    let weeks = (start + cells.len()).div_ceil(7);
    let rows: Vec<_> = ["Mon", "", "Wed", "", "Fri", "", ""]
        .iter()
        .enumerate()
        .map(|(i, name)| view! { <text x=0 y=14.0 + i as f64 * size + 8.0 class="calendar-label">{*name}</text> })
        .collect();
    let width = 30.0 + weeks as f64 * size;
    view! {
        <div class="calendar">
            <svg viewBox=format!("0 0 {width} {}", 14.0 + 7.0 * size) class="calendar-svg">
                {rows}
                {squares}
            </svg>
            <div class="muted small">{format!("{label}: darker is more, up to {}", format::value(max, format.as_ref()))}</div>
        </div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waffles_always_have_a_hundred_squares() {
        assert_eq!(waffle_counts(&[1.0, 1.0, 1.0]), [34, 33, 33]);
        assert_eq!(waffle_counts(&[60.0, 40.0]), [60, 40]);
        assert_eq!(waffle_counts(&[]).len(), 0);
    }

    #[test]
    fn treemaps_fill_the_box_with_areas_in_proportion() {
        let rects = squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], 0.0, 0.0, 6.0, 4.0);
        let area: f64 = rects.iter().map(|r| r.2 * r.3).sum();
        assert!((area - 24.0).abs() < 1e-9, "{area}");
        assert!((rects[0].2 * rects[0].3 - 6.0).abs() < 1e-9);
    }

    #[test]
    fn gauge_scales_round_up_and_bands_clip_to_the_scale() {
        assert_eq!(nice_ceiling(38.2), 50.0);
        assert_eq!(nice_ceiling(1004.0), 2000.0);
        assert_eq!(nice_ceiling(0.12), 0.2);
        let by = ColorByValue {
            mode: Default::default(),
            stops: vec![
                crate::model::ColorStop {
                    from: 0.0,
                    color: "g".into(),
                },
                crate::model::ColorStop {
                    from: 0.1,
                    color: "o".into(),
                },
                crate::model::ColorStop {
                    from: 0.15,
                    color: "r".into(),
                },
            ],
            percent: false,
            extend_below: false,
            extend_above: true,
            to: None,
        };
        assert_eq!(
            bands(&by, 0.0, 0.3),
            [
                (0.0, 0.1, "g".into()),
                (0.1, 0.15, "o".into()),
                (0.15, 0.3, "r".into())
            ]
        );
    }
}
