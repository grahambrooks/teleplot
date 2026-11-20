//! Charts of rows against a continuous axis: box plots and beeswarms (a
//! value axis), and state timelines (a time axis). One row per team, repo,
//! service or environment. Everything here is plain Rust, so the layouts —
//! the beeswarm's dodge and the timeline's lanes — are unit tested; uPlot
//! draws the result (`uplot::rows`).

use super::series::{RefLine, SeriesColor, cell_text, reference_lines};
use crate::model::{
    AnnotationData, Cell, Chart, ChartKind, Encoding, Format, LayerData, PanelData, SeriesStyle,
};

/// What a rows chart draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Rows {
    /// Row labels, top to bottom.
    pub rows: Vec<String>,
    /// The axis is time (epoch ms), not values.
    pub time: bool,
    pub log_scale: bool,
    /// The axis's extent.
    pub range: (f64, f64),
    /// How values read (box plots and beeswarms).
    pub format: Option<Format>,
    /// What the values are, e.g. "Cycle time".
    pub label: String,
    /// What the rows are, e.g. "Team".
    pub row_label: String,
    pub boxes: Vec<BoxMark>,
    pub dots: Vec<Dot>,
    pub bars: Vec<StateBar>,
    /// Lanes per row, so overlapping bars sit one above the other.
    pub lanes: Vec<usize>,
    /// What the colours mean: dot colours, or states. Empty when every dot
    /// is the same colour.
    pub key: Vec<(String, SeriesColor)>,
    pub reference_lines: Vec<RefLine>,
    /// Events to mark (time axes only).
    pub annotations: Vec<AnnotationData>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoxMark {
    pub row: usize,
    pub low: Option<f64>,
    pub q1: f64,
    pub median: f64,
    pub q3: f64,
    pub high: Option<f64>,
    /// Documents behind the box.
    pub count: Option<f64>,
    /// Fewer documents than the dashboard's `min_samples`: drawn faintly.
    pub thin: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dot {
    pub row: usize,
    pub value: f64,
    /// Index into [`Rows::key`] (0 when there is no key).
    pub key: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateBar {
    pub row: usize,
    pub start: f64,
    pub end: f64,
    /// No end yet: it runs to the end of the range.
    pub open: bool,
    /// Index into [`Rows::key`].
    pub key: usize,
    /// Its lane within the row (0 at the top).
    pub lane: usize,
    pub text: Option<String>,
}

fn empty() -> Rows {
    Rows {
        rows: Vec::new(),
        time: false,
        log_scale: false,
        range: (0.0, 1.0),
        format: None,
        label: String::new(),
        row_label: String::new(),
        boxes: Vec::new(),
        dots: Vec::new(),
        bars: Vec::new(),
        lanes: Vec::new(),
        key: Vec::new(),
        reference_lines: Vec::new(),
        annotations: Vec::new(),
    }
}

fn column_label(layer: &crate::model::Layer, id: &str) -> String {
    layer
        .column(id)
        .and_then(|c| c.label.clone())
        .unwrap_or_else(|| id.to_string())
}

fn numbers(d: &LayerData, id: &str) -> Vec<Option<f64>> {
    d.series(id)
        .map(|s| s.values.iter().map(Cell::number).collect())
        .unwrap_or_default()
}

fn texts(d: &LayerData, id: &str) -> Vec<Option<String>> {
    d.series(id)
        .map(|s| {
            s.values
                .iter()
                .map(|c| match c {
                    Cell::Null => None,
                    other => Some(cell_text(other)),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The index of `name` in `rows`, added at the end when new.
fn row_of(rows: &mut Vec<String>, name: &str) -> usize {
    match rows.iter().position(|r| r == name) {
        Some(i) => i,
        None => {
            rows.push(name.to_string());
            rows.len() - 1
        }
    }
}

/// Colours for `values` (in first-seen order): their `series_styles`
/// colour, else palette slots in order. Labels come from the styles too.
fn key(
    values: Vec<String>,
    styles: &std::collections::BTreeMap<String, SeriesStyle>,
) -> Vec<(String, SeriesColor)> {
    values
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            let style = styles.get(&v);
            let color = match style.and_then(|s| s.color.as_deref()) {
                Some("accent") => SeriesColor::Slot(0),
                Some(c) => SeriesColor::Css(
                    super::theme::status(c)
                        .map(str::to_string)
                        .unwrap_or_else(|| c.to_string()),
                ),
                None => SeriesColor::Slot(i),
            };
            let label = style.and_then(|s| s.label.clone()).unwrap_or(v);
            (label, color)
        })
        .collect()
}

/// A box plot's boxes and dots, on one value axis. Rows come in the box
/// layer's order (its terms order), then any rows only the dots have.
pub fn box_plot(chart: &Chart, data: &PanelData, min_samples: Option<u32>) -> Rows {
    let mut out = empty();
    let mut formats = Vec::new();
    for (layer, d) in chart.layers.iter().zip(&data.layers) {
        match &layer.encoding {
            Encoding::Quartiles {
                group,
                q1,
                median,
                q3,
                low,
                high,
                count,
                log_scale,
            } => {
                out.log_scale |= *log_scale;
                let groups = texts(d, group);
                let (q1s, medians, q3s) = (numbers(d, q1), numbers(d, median), numbers(d, q3));
                let lows = low.as_deref().map(|c| numbers(d, c));
                let highs = high.as_deref().map(|c| numbers(d, c));
                let counts = count.as_deref().map(|c| numbers(d, c));
                if out.row_label.is_empty() {
                    out.row_label = column_label(layer, group);
                }
                if let Some(c) = layer.column(median) {
                    formats.push(c.display_format());
                    if out.label.is_empty() {
                        out.label = c.label.clone().unwrap_or_else(|| median.clone());
                    }
                }
                for i in 0..d.rows {
                    let (Some(Some(name)), Some(Some(q1)), Some(Some(m)), Some(Some(q3))) =
                        (groups.get(i), q1s.get(i), medians.get(i), q3s.get(i))
                    else {
                        continue;
                    };
                    let at = |v: &Option<Vec<Option<f64>>>| {
                        v.as_ref().and_then(|v| v.get(i).copied().flatten())
                    };
                    let count = at(&counts);
                    let row = row_of(&mut out.rows, name);
                    out.boxes.push(BoxMark {
                        row,
                        low: at(&lows),
                        q1: *q1,
                        median: *m,
                        q3: *q3,
                        high: at(&highs),
                        count,
                        thin: matches!((count, min_samples), (Some(n), Some(min)) if n < min as f64),
                    });
                }
            }
            Encoding::Swarm {
                group,
                value,
                color,
                log_scale,
                series_styles,
            } => {
                out.log_scale |= *log_scale;
                let groups = texts(d, group);
                let values = numbers(d, value);
                let colors = color.as_deref().map(|c| texts(d, c));
                if out.row_label.is_empty() {
                    out.row_label = column_label(layer, group);
                }
                if let Some(c) = layer.column(value) {
                    formats.push(c.display_format());
                    if out.label.is_empty() {
                        out.label = c.label.clone().unwrap_or_else(|| value.clone());
                    }
                }
                let mut seen: Vec<String> = Vec::new();
                for i in 0..d.rows {
                    let (Some(Some(name)), Some(Some(v))) = (groups.get(i), values.get(i)) else {
                        continue;
                    };
                    let key = match &colors {
                        Some(c) => {
                            let k = c
                                .get(i)
                                .cloned()
                                .flatten()
                                .unwrap_or_else(|| "(empty)".into());
                            row_of(&mut seen, &k)
                        }
                        None => 0,
                    };
                    let row = row_of(&mut out.rows, name);
                    out.dots.push(Dot {
                        row,
                        value: *v,
                        key,
                    });
                }
                if color.is_some() {
                    out.key = key(seen, series_styles);
                }
            }
            _ => {}
        }
    }
    out.format = formats.into_iter().flatten().next();
    out.reference_lines = reference_lines(chart, data, &[]);
    // Lines without a format of their own read like the values.
    for line in &mut out.reference_lines {
        if line.format.is_none() {
            line.format = out.format.clone();
        }
    }
    let values = out
        .boxes
        .iter()
        .flat_map(|b| [b.low, Some(b.q1), Some(b.q3), b.high])
        .flatten()
        .chain(out.dots.iter().map(|d| d.value))
        .chain(out.reference_lines.iter().map(|l| l.value));
    out.range = extent(values, out.log_scale);
    out
}

/// A state timeline's bars, by row, in lanes where they overlap. Rows are
/// in order of their first bar. `now` ends bars that are still open; the
/// axis runs from the first start to the last end.
pub fn state_timeline(chart: &Chart, data: &PanelData, now: f64) -> Rows {
    let mut out = empty();
    out.time = true;
    let Some((layer, d)) = chart
        .layers
        .iter()
        .zip(&data.layers)
        .find(|(l, _)| matches!(l.encoding, Encoding::States { .. }))
    else {
        return out;
    };
    let Encoding::States {
        row,
        start,
        end,
        state,
        text,
        series_styles,
    } = &layer.encoding
    else {
        return out;
    };
    out.label = column_label(layer, state);
    out.row_label = column_label(layer, row);
    let (rows, starts, ends, states) = (
        texts(d, row),
        numbers(d, start),
        numbers(d, end),
        texts(d, state),
    );
    let notes = text.as_deref().map(|t| texts(d, t));
    let mut seen: Vec<String> = Vec::new();
    let mut found: Vec<(String, StateBar)> = Vec::new();
    for i in 0..d.rows {
        let (Some(Some(name)), Some(Some(s))) = (rows.get(i), starts.get(i)) else {
            continue;
        };
        let state = states
            .get(i)
            .cloned()
            .flatten()
            .unwrap_or_else(|| "(empty)".into());
        let finish = ends.get(i).copied().flatten();
        found.push((
            name.clone(),
            StateBar {
                row: 0,
                start: *s,
                end: finish.unwrap_or(now).max(*s),
                open: finish.is_none(),
                key: row_of(&mut seen, &state),
                lane: 0,
                text: notes.as_ref().and_then(|n| n.get(i).cloned().flatten()),
            },
        ));
    }
    // States in a stable order: by name, so `sev1` comes before `sev2`.
    let mut order: Vec<usize> = (0..seen.len()).collect();
    order.sort_by(|&a, &b| seen[a].cmp(&seen[b]));
    let rank: Vec<usize> = (0..seen.len())
        .map(|k| order.iter().position(|&o| o == k).unwrap_or(k))
        .collect();
    out.key = key(
        order.iter().map(|&k| seen[k].clone()).collect(),
        series_styles,
    );
    found.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));
    for (name, mut bar) in found {
        bar.row = row_of(&mut out.rows, &name);
        bar.key = rank[bar.key];
        out.bars.push(bar);
    }
    out.lanes = vec![1; out.rows.len()];
    for r in 0..out.rows.len() {
        let mine: Vec<usize> = (0..out.bars.len())
            .filter(|&i| out.bars[i].row == r)
            .collect();
        let spans: Vec<(f64, f64)> = mine
            .iter()
            .map(|&i| (out.bars[i].start, out.bars[i].end))
            .collect();
        let (lanes, count) = lanes(&spans);
        for (&i, lane) in mine.iter().zip(lanes) {
            out.bars[i].lane = lane;
        }
        out.lanes[r] = count.max(1);
    }
    let lo = out
        .bars
        .iter()
        .map(|b| b.start)
        .fold(f64::INFINITY, f64::min);
    let hi = out
        .bars
        .iter()
        .map(|b| b.end)
        .fold(f64::NEG_INFINITY, f64::max);
    out.range = if lo.is_finite() && hi > lo {
        (lo, hi)
    } else if lo.is_finite() {
        (lo - 3_600_000.0, lo + 3_600_000.0)
    } else {
        (now - 86_400_000.0, now)
    };
    if chart.kind == ChartKind::StateTimeline {
        out.annotations = data.annotations.clone();
    }
    out
}

/// The smallest and largest values, padded a little. On a log axis only
/// positive values count, and the padding is a ratio.
pub fn extent(values: impl Iterator<Item = f64>, log: bool) -> (f64, f64) {
    let (lo, hi) = values
        .filter(|v| v.is_finite() && (!log || *v > 0.0))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v), hi.max(v))
        });
    if !lo.is_finite() {
        return if log { (1.0, 10.0) } else { (0.0, 1.0) };
    }
    if log {
        let (lo, hi) = if hi > lo {
            (lo, hi)
        } else {
            (lo / 2.0, hi * 2.0)
        };
        let pad = (hi / lo).powf(0.04);
        return (lo / pad, hi * pad);
    }
    if hi == lo {
        let pad = lo.abs().max(1.0) * 0.1;
        return (lo - pad, hi + pad);
    }
    let pad = (hi - lo) * 0.04;
    // Durations and counts can't be negative: don't pad below zero.
    let low = if lo >= 0.0 {
        (lo - pad).max(0.0)
    } else {
        lo - pad
    };
    (low, hi + pad)
}

/// Lanes for intervals in one row, so none overlap: each takes the first
/// lane free at its start (intervals sorted by start). Returns each
/// interval's lane and how many lanes there are.
pub fn lanes(spans: &[(f64, f64)]) -> (Vec<usize>, usize) {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| spans[a].0.total_cmp(&spans[b].0));
    let mut ends: Vec<f64> = Vec::new();
    let mut out = vec![0; spans.len()];
    for i in order {
        let (start, end) = spans[i];
        let lane = match ends.iter().position(|e| *e <= start) {
            Some(l) => l,
            None => {
                ends.push(f64::NEG_INFINITY);
                ends.len() - 1
            }
        };
        ends[lane] = end;
        out[i] = lane;
    }
    let n = ends.len();
    (out, n)
}

/// A beeswarm's offsets: each dot (at pixel `xs[i]`, radius `r`) moves
/// up or down from the row's middle as little as it can without overlapping
/// a dot already placed, smallest x first. Offsets are clamped to `max`,
/// where dots may then overlap (a crowded row stays inside its band).
pub fn dodge(xs: &[f64], r: f64, max: f64) -> Vec<f64> {
    let mut order: Vec<usize> = (0..xs.len()).collect();
    order.sort_by(|&a, &b| xs[a].total_cmp(&xs[b]));
    let d = 2.0 * r;
    let mut placed: Vec<(f64, f64)> = Vec::new();
    let mut out = vec![0.0; xs.len()];
    // Placed dots more than a diameter to the left can't collide again.
    let mut from = 0;
    for i in order {
        let x = xs[i];
        while from < placed.len() && x - placed[from].0 > d {
            from += 1;
        }
        let near = &placed[from..];
        let free = |y: f64| {
            near.iter()
                .all(|(px, py)| (px - x).powi(2) + (py - y).powi(2) >= d * d - 1e-9)
        };
        let mut candidates = vec![0.0];
        for (px, py) in near {
            let dy = (d * d - (px - x).powi(2)).max(0.0).sqrt();
            candidates.push(py + dy);
            candidates.push(py - dy);
        }
        candidates.sort_by(|a: &f64, b: &f64| a.abs().total_cmp(&b.abs()).then(b.total_cmp(a)));
        let y = candidates
            .into_iter()
            .find(|y| y.abs() <= max + 1e-9 && free(*y))
            .unwrap_or_else(|| {
                // Crowded: the least crowded spot inside the band.
                let mut best = (0.0, usize::MAX);
                let steps = 8;
                for k in 0..=2 * steps {
                    let y = -max + max * k as f64 / steps as f64;
                    let hits = near
                        .iter()
                        .filter(|(px, py)| (px - x).powi(2) + (py - y).powi(2) < d * d)
                        .count();
                    if hits < best.1 {
                        best = (y, hits);
                    }
                }
                best.0
            });
        out[i] = y;
        placed.push((x, y));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Series;

    fn chart(yaml: &str) -> Chart {
        crate::yaml::from_str(yaml).unwrap()
    }

    fn layer(id: &str, rows: usize, columns: Vec<(&str, Vec<Cell>)>) -> LayerData {
        LayerData {
            layer: id.into(),
            rows,
            columns: columns
                .into_iter()
                .map(|(id, values)| Series {
                    id: id.into(),
                    values,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn t(s: &str) -> Cell {
        Cell::Text(s.into())
    }

    fn n(v: f64) -> Cell {
        Cell::Number(v)
    }

    const BOX: &str = "
kind: box_plot
layers:
  - id: q
    data_view: { index: prs }
    columns:
      - { id: team, op: terms, fields: [team], size: 10 }
      - { id: p25, op: percentile, field: h, percent: 25 }
      - { id: p50, label: Cycle time, op: median, field: h, format: { kind: number, suffix: ' h' } }
      - { id: p75, op: percentile, field: h, percent: 75 }
      - { id: n, op: count }
    encoding: { as: box, group: team, q1: p25, median: p50, q3: p75, count: n }
  - id: dots
    data_view: { index: prs }
    documents: { limit: 100, sort: merged_at }
    columns:
      - { id: team, op: value, field: team }
      - { id: h, op: value, field: h }
      - { id: ai, op: value, field: ai }
    encoding: { as: swarm, group: team, value: h, color: ai, series_styles: { 'true': { label: AI-assisted } } }
";

    #[test]
    fn box_plots_keep_the_box_order_and_flag_thin_boxes() {
        let c = chart(BOX);
        let data = PanelData {
            layers: vec![
                layer(
                    "q",
                    2,
                    vec![
                        ("team", vec![t("payments"), t("search")]),
                        ("p25", vec![n(10.0), n(4.0)]),
                        ("p50", vec![n(20.0), n(8.0)]),
                        ("p75", vec![n(40.0), n(12.0)]),
                        ("n", vec![n(120.0), n(3.0)]),
                    ],
                ),
                layer(
                    "dots",
                    3,
                    vec![
                        ("team", vec![t("search"), t("payments"), t("mobile")]),
                        ("h", vec![n(9.0), n(30.0), n(2.0)]),
                        ("ai", vec![Cell::Text("true".into()), t("false"), t("true")]),
                    ],
                ),
            ],
            ..Default::default()
        };
        let rows = box_plot(&c, &data, Some(5));
        assert_eq!(rows.rows, ["payments", "search", "mobile"]);
        assert_eq!(rows.label, "Cycle time");
        assert_eq!(
            rows.row_label, "team",
            "the group column's id, without a label"
        );
        assert_eq!(rows.boxes.len(), 2);
        assert!(!rows.boxes[0].thin && rows.boxes[1].thin, "3 PRs < 5");
        assert_eq!(
            rows.dots.iter().map(|d| d.row).collect::<Vec<_>>(),
            [1, 0, 2]
        );
        assert_eq!(rows.key[0].0, "AI-assisted");
        assert_eq!(rows.key[1].0, "false");
        assert_eq!(rows.dots[1].key, 1);
        let (lo, hi) = rows.range;
        assert!((0.0..2.0).contains(&lo) && hi > 40.0, "{:?}", rows.range);
        assert_eq!(rows.format.and_then(|f| f.suffix).as_deref(), Some(" h"));
    }

    #[test]
    fn state_timelines_put_overlaps_in_lanes_and_run_open_states_to_now() {
        let c = chart(
            "
kind: state_timeline
layers:
  - id: incidents
    data_view: { index: incidents, time_field: created_at }
    documents: { limit: 100, sort: created_at }
    columns:
      - { id: team, op: value, field: team }
      - { id: from, op: value, field: created_at }
      - { id: to, op: value, field: resolved_at }
      - { id: sev, op: value, field: severity }
    encoding: { as: states, row: team, start: from, end: to, state: sev, series_styles: { sev1: { color: critical } } }
",
        );
        let data = PanelData {
            layers: vec![layer(
                "incidents",
                4,
                vec![
                    (
                        "team",
                        vec![t("search"), t("payments"), t("search"), t("payments")],
                    ),
                    ("from", vec![n(30.0), n(10.0), n(40.0), n(60.0)]),
                    ("to", vec![n(50.0), n(20.0), n(45.0), Cell::Null]),
                    ("sev", vec![t("sev2"), t("sev1"), t("sev3"), t("sev2")]),
                ],
            )],
            ..Default::default()
        };
        let rows = state_timeline(&c, &data, 100.0);
        assert_eq!(rows.rows, ["payments", "search"], "by first start");
        assert_eq!(
            rows.key.iter().map(|k| k.0.as_str()).collect::<Vec<_>>(),
            ["sev1", "sev2", "sev3"]
        );
        assert_eq!(rows.key[0].1, SeriesColor::Css("#d03b3b".into()));
        assert_eq!(rows.lanes, [1, 2], "search's sev3 overlaps its sev2");
        let open = rows.bars.iter().find(|b| b.open).unwrap();
        assert_eq!((open.start, open.end), (60.0, 100.0));
        assert_eq!(rows.range, (10.0, 100.0));
        let sev3 = rows.bars.iter().find(|b| b.key == 2).unwrap();
        assert_eq!(sev3.lane, 1);
    }

    #[test]
    fn lanes_reuse_the_first_free_one() {
        let (l, n) = lanes(&[(0.0, 10.0), (5.0, 8.0), (9.0, 12.0), (10.0, 11.0)]);
        assert_eq!(l, [0, 1, 1, 0]);
        assert_eq!(n, 2);
        assert_eq!(lanes(&[]).1, 0);
    }

    #[test]
    fn dodged_dots_never_overlap_while_there_is_room() {
        let xs = [10.0, 10.0, 10.0, 11.0, 30.0];
        let ys = dodge(&xs, 2.0, 20.0);
        for i in 0..xs.len() {
            for j in 0..i {
                let dist = ((xs[i] - xs[j]).powi(2) + (ys[i] - ys[j]).powi(2)).sqrt();
                assert!(dist >= 4.0 - 1e-6, "{i} and {j} overlap: {ys:?}");
            }
        }
        assert_eq!(ys[0], 0.0, "the first dot sits on the middle line");
        assert_eq!(ys[4], 0.0, "a dot with room stays in the middle");
        assert!(ys.iter().all(|y| y.abs() <= 20.0));
    }

    #[test]
    fn crowded_rows_stay_inside_their_band() {
        let xs = vec![5.0; 50];
        let ys = dodge(&xs, 2.0, 6.0);
        assert!(ys.iter().all(|y| y.abs() <= 6.0 + 1e-9), "{ys:?}");
    }

    #[test]
    fn extents_pad_without_going_below_zero_or_to_zero_on_log_axes() {
        let (lo, hi) = extent([2.0, 100.0].into_iter(), false);
        assert_eq!(lo, 0.0f64.max(2.0 - 3.92));
        assert!((hi - 103.92).abs() < 1e-9);
        let (lo, hi) = extent([0.0, 2.0, 2000.0].into_iter(), true);
        assert!(lo > 1.0 && lo < 2.0 && hi > 2000.0, "{lo} {hi}");
        assert_eq!(extent(std::iter::empty(), false), (0.0, 1.0));
    }
}
