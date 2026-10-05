//! Formatting values for display, following a column's [`Format`].

use crate::model::{Format, FormatKind};

/// Formats a number for display.
pub fn value(v: f64, format: Option<&Format>) -> String {
    let Some(format) = format else {
        return plain(v);
    };
    let decimals = format.decimals.map(|d| d as usize);
    let text = match format.kind {
        FormatKind::Percent => format!("{}%", fixed(v * 100.0, decimals.unwrap_or(1))),
        FormatKind::Bytes => bytes(v),
        FormatKind::Number | FormatKind::Duration | FormatKind::Currency if format.compact => {
            compact(v)
        }
        FormatKind::Number | FormatKind::Duration | FormatKind::Currency => match decimals {
            Some(d) => grouped(v, d),
            None => plain(v),
        },
    };
    match &format.suffix {
        Some(suffix) => format!("{text}{suffix}"),
        None => text,
    }
}

/// Axis tick labels in the column's format, with more decimals when it would
/// print two ticks the same: over a narrow range such as 11.50%–11.54%, one
/// decimal labels every tick "11.5%".
pub fn ticks(values: &[f64], format: Option<&Format>) -> Vec<String> {
    let render = |f: Option<&Format>| values.iter().map(|v| value(*v, f)).collect::<Vec<_>>();
    let distinct = |t: &[String]| {
        let mut sorted = t.to_vec();
        sorted.sort();
        sorted.dedup();
        sorted.len() == t.len()
    };
    let first = render(format);
    let Some(base) = format else {
        return first;
    };
    if distinct(&first) || base.compact || base.kind == FormatKind::Bytes {
        return first;
    }
    let start = base
        .decimals
        .unwrap_or(if base.kind == FormatKind::Percent {
            1
        } else {
            0
        });
    for decimals in start + 1..=start + 4 {
        let f = Format {
            decimals: Some(decimals),
            ..base.clone()
        };
        let t = render(Some(&f));
        if distinct(&t) {
            return t;
        }
    }
    first
}

/// Kibana's default number format: up to three decimals, thousands grouped.
pub fn plain(v: f64) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    let rounded = (v * 1000.0).round() / 1000.0;
    let decimals = if rounded.fract() == 0.0 {
        0
    } else if (rounded * 10.0).fract().abs() < 1e-9 {
        1
    } else if (rounded * 100.0).fract().abs() < 1e-9 {
        2
    } else {
        3
    };
    grouped(rounded, decimals)
}

/// `1234.5` → `1,234.5` with a fixed number of decimals.
pub fn grouped(v: f64, decimals: usize) -> String {
    let text = fixed(v.abs(), decimals);
    let (int, frac) = text
        .split_once('.')
        .map_or((text.as_str(), None), |(i, f)| (i, Some(f)));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if let Some(frac) = frac {
        out.push('.');
        out.push_str(frac);
    }
    if v < 0.0 && out.chars().any(|c| c.is_ascii_digit() && c != '0') {
        out.insert(0, '-');
    }
    out
}

/// `v` with `decimals` places; a value that rounds to zero has no sign.
fn fixed(v: f64, decimals: usize) -> String {
    let text = format!("{v:.decimals$}");
    match text.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => text,
    }
}

/// `1234567` → `1.2M`.
pub fn compact(v: f64) -> String {
    let abs = v.abs();
    let (scaled, unit) = match abs {
        a if a >= 1e12 => (v / 1e12, "T"),
        a if a >= 1e9 => (v / 1e9, "B"),
        a if a >= 1e6 => (v / 1e6, "M"),
        a if a >= 1e3 => (v / 1e3, "k"),
        _ => return plain(v),
    };
    let text = if scaled.abs() >= 100.0 {
        fixed(scaled, 0)
    } else {
        fixed(scaled, 1)
    };
    format!("{}{unit}", text.trim_end_matches(".0"))
}

fn bytes(v: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = v;
    let mut unit = 0;
    while v.abs() >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    let text = if unit == 0 { fixed(v, 0) } else { fixed(v, 1) };
    format!("{text}{}", UNITS[unit])
}

/// Change from `previous` to `current`, as a signed percentage, if meaningful.
pub fn change(current: f64, previous: f64) -> Option<f64> {
    (previous != 0.0 && current.is_finite() && previous.is_finite())
        .then(|| (current - previous) / previous.abs() * 100.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ticks_gain_decimals_until_they_differ() {
        let percent = Format {
            kind: FormatKind::Percent,
            decimals: Some(1),
            suffix: None,
            compact: false,
        };
        let narrow = [0.1150, 0.1151, 0.1152, 0.1153];
        assert_eq!(
            super::ticks(&narrow, Some(&percent)),
            ["11.50%", "11.51%", "11.52%", "11.53%"]
        );
        let wide = [0.0, 0.1, 0.2];
        assert_eq!(
            super::ticks(&wide, Some(&percent)),
            ["0.0%", "10.0%", "20.0%"],
            "unchanged when distinct"
        );
        let lines = Format {
            kind: FormatKind::Number,
            decimals: Some(0),
            suffix: Some(" lines".into()),
            compact: false,
        };
        assert_eq!(
            super::ticks(&[0.0, 0.5, 1.0], Some(&lines)),
            ["0.0 lines", "0.5 lines", "1.0 lines"]
        );
    }

    use super::*;

    fn fmt(kind: FormatKind) -> Format {
        Format {
            kind,
            decimals: None,
            suffix: None,
            compact: false,
        }
    }

    #[test]
    fn numbers() {
        assert_eq!(plain(1234.5), "1,234.5");
        assert_eq!(plain(38.195714), "38.196");
        assert_eq!(plain(2724.0), "2,724");
        assert_eq!(plain(-0.25), "-0.25");
        assert_eq!(grouped(1234567.891, 2), "1,234,567.89");
        assert_eq!(compact(1234567.0), "1.2M");
        assert_eq!(compact(950.0), "950");
        assert_eq!(compact(12_500.0), "12.5k");
    }

    #[test]
    fn column_formats() {
        let hours = Format {
            decimals: Some(1),
            suffix: Some(" h".into()),
            ..fmt(FormatKind::Number)
        };
        assert_eq!(value(20.615, Some(&hours)), "20.6 h");
        assert_eq!(value(0.1016, Some(&fmt(FormatKind::Percent))), "10.2%");
        let pct0 = Format {
            decimals: Some(0),
            ..fmt(FormatKind::Percent)
        };
        assert_eq!(value(0.3913, Some(&pct0)), "39%");
        assert_eq!(value(1536.0, Some(&fmt(FormatKind::Bytes))), "1.5KB");
        assert_eq!(value(5.0, None), "5");
    }

    #[test]
    fn normalized_columns_carry_kibanas_unit_suffix() {
        let column = |per: &str, format: &str| -> crate::model::Column {
            serde_yaml_ng::from_str(&format!("{{ id: c, op: count, per: {per}{format} }}")).unwrap()
        };
        let shown = |c: crate::model::Column, v| value(v, c.display_format().as_ref());
        assert_eq!(shown(column("day", ""), 6.1333333), "6.133/d");
        assert_eq!(
            shown(
                column(
                    "week",
                    ", format: { kind: number, decimals: 1, suffix: ' h' }"
                ),
                3.04
            ),
            "3.0 h/w"
        );
    }

    #[test]
    fn changes() {
        assert_eq!(change(110.0, 100.0), Some(10.0));
        assert_eq!(change(5.0, 0.0), None);
    }

    #[test]
    fn values_that_round_to_zero_have_no_sign() {
        let percent = Format {
            kind: FormatKind::Percent,
            decimals: Some(0),
            suffix: None,
            compact: false,
        };
        assert_eq!(value(-0.0021, Some(&percent)), "0%");
        assert_eq!(value(-0.12, Some(&percent)), "-12%");
    }
}
