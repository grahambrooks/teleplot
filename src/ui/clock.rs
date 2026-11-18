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

/// The browser's IANA time zone, e.g. `America/New_York`; `None` on the server.
pub fn browser_time_zone() -> Option<String> {
    #[cfg(feature = "hydrate")]
    {
        let format =
            js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new());
        js_sys::Reflect::get(&format.resolved_options(), &"timeZone".into())
            .ok()?
            .as_string()
    }
    #[cfg(not(feature = "hydrate"))]
    {
        None
    }
}

/// Epoch ms → `2026-11-16 14:05 EST` (or with `:09` seconds) in `tz`, with
/// the zone's abbreviation so a reader never has to guess.
#[cfg(feature = "hydrate")]
pub fn local(ms: f64, tz: &str, seconds: bool) -> String {
    let date = js_sys::Date::new(&ms.into());
    let options = js_sys::Object::new();
    let mut fields = vec![
        ("timeZone", tz),
        ("year", "numeric"),
        ("month", "2-digit"),
        ("day", "2-digit"),
        ("hour", "2-digit"),
        ("minute", "2-digit"),
        ("hourCycle", "h23"),
        ("timeZoneName", "short"),
    ];
    if seconds {
        fields.push(("second", "2-digit"));
    }
    for (k, v) in fields {
        let _ = js_sys::Reflect::set(&options, &k.into(), &v.into());
    }
    let text: String = date.to_locale_string("en-CA", &options).into();
    text.replace(',', "")
}

/// Epoch ms → `2026-11-16 14:05 EST` (or with `:09` seconds) in `tz`, with
/// the zone's abbreviation so a reader never has to guess.
#[cfg(not(feature = "hydrate"))]
pub fn local(ms: f64, tz: &str, seconds: bool) -> String {
    let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let format = if seconds {
        "%Y-%m-%d %H:%M:%S %Z"
    } else {
        "%Y-%m-%d %H:%M %Z"
    };
    jiff::Timestamp::from_millisecond(ms as i64)
        .map(|t| t.to_zoned(zone).strftime(format).to_string())
        .unwrap_or_default()
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
        assert_eq!(
            local(1_775_001_600_000.0, "UTC", false),
            "2026-04-01 00:00 UTC"
        );
        assert_eq!(
            local(1_775_001_600_000.0, "America/New_York", true),
            "2026-03-31 20:00:00 EDT",
            "local time, labelled with its zone"
        );
        assert_eq!(parse_iso("2026-04-01T00:00:00Z"), Some(1_775_001_600_000.0));
        assert_eq!(parse_iso("now-90d/d"), None);
    }
}
