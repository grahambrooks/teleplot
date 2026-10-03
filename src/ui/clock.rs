//! UTC timestamps for the UI: the browser's `Date` in WASM (keeping a date
//! library out of the bundle), jiff on the server.

/// Epoch ms → `2026-04-01T00:00:00.000Z`.
pub fn iso(ms: f64) -> String {
    #[cfg(feature = "hydrate")]
    {
        js_sys::Date::new(&ms.into()).to_iso_string().into()
    }
    #[cfg(not(feature = "hydrate"))]
    {
        jiff::Timestamp::from_millisecond(ms as i64)
            .map(|t| t.to_string())
            .unwrap_or_default()
    }
}

/// Epoch ms → `2026-04-01 00:00` (UTC).
pub fn minutes(ms: f64) -> String {
    let iso = iso(ms);
    match (iso.get(..10), iso.get(11..16)) {
        (Some(date), Some(time)) => format!("{date} {time}"),
        _ => iso,
    }
}

/// An ISO 8601 timestamp (with offset or `Z`) → epoch ms. Date math is not a timestamp.
pub fn parse_iso(s: &str) -> Option<f64> {
    if !s.chars().next()?.is_ascii_digit() || !s.contains('T') {
        return None;
    }
    #[cfg(feature = "hydrate")]
    {
        let ms = js_sys::Date::parse(s);
        (!ms.is_nan()).then_some(ms)
    }
    #[cfg(not(feature = "hydrate"))]
    {
        s.parse::<jiff::Timestamp>()
            .ok()
            .map(|t| t.as_millisecond() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses() {
        assert_eq!(iso(1_775_001_600_000.0), "2026-04-01T00:00:00Z");
        assert_eq!(minutes(1_775_001_600_000.0), "2026-04-01 00:00");
        assert_eq!(parse_iso("2026-04-01T00:00:00Z"), Some(1_775_001_600_000.0));
        assert_eq!(parse_iso("now-90d/d"), None);
    }
}
