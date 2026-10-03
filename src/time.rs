//! Kibana date math (`now-90d/d`, `now-6M/M`, ISO timestamps) → instants.
//!
//! Follows `@kbn/datemath`: an anchor (`now` or a date, optionally followed
//! by `||`), then operations `+N<unit>`, `-N<unit>` and `/<unit>` (round).
//! Units: `y M w d h H m s ms`. Rounding is down for range starts and up
//! (to the end of the unit) for range ends, in the dashboard's time zone.
//! Weeks start on Sunday, Kibana's default (`dateFormat:dow`).

use jiff::{
    RoundMode, Span, Timestamp, ToSpan, Unit, Zoned, ZonedRound, civil::Weekday, tz::TimeZone,
};

/// A resolved time range: epoch milliseconds, `from` inclusive, `to` inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub from_ms: i64,
    pub to_ms: i64,
}

impl Range {
    pub fn duration_ms(&self) -> i64 {
        self.to_ms - self.from_ms
    }

    /// The same-length range immediately before this one (Kibana's
    /// `previous` time shift).
    pub fn previous(&self) -> Range {
        Range {
            from_ms: self.from_ms - self.duration_ms(),
            to_ms: self.from_ms,
        }
    }

    pub fn shifted(&self, back_ms: i64) -> Range {
        Range {
            from_ms: self.from_ms - back_ms,
            to_ms: self.to_ms - back_ms,
        }
    }
}

/// Resolves a `from`/`to` pair as Kibana's time picker does.
pub fn resolve(from: &str, to: &str, now: Timestamp, tz: &TimeZone) -> Result<Range, String> {
    let from_ms = parse(from, now, tz, false)?;
    let to_ms = parse(to, now, tz, true)?;
    if from_ms > to_ms {
        return Err(format!("time range starts ({from}) after it ends ({to})"));
    }
    Ok(Range { from_ms, to_ms })
}

/// One date-math expression, as epoch milliseconds.
pub fn parse(expr: &str, now: Timestamp, tz: &TimeZone, round_up: bool) -> Result<i64, String> {
    let expr = expr.trim();
    let (anchor, math) = if let Some(rest) = expr.strip_prefix("now") {
        (now.to_zoned(tz.clone()), rest)
    } else if let Some((date, rest)) = expr.split_once("||") {
        (parse_date(date, tz)?, rest)
    } else {
        // A plain date, no math.
        return Ok(parse_date(expr, tz)?.timestamp().as_millisecond());
    };
    let zoned = apply(anchor, math, round_up).map_err(|e| format!("{expr:?}: {e}"))?;
    Ok(zoned.timestamp().as_millisecond())
}

/// ISO 8601 with or without offset, or epoch milliseconds.
fn parse_date(s: &str, tz: &TimeZone) -> Result<Zoned, String> {
    let s = s.trim();
    if let Ok(ms) = s.parse::<i64>() {
        let ts = Timestamp::from_millisecond(ms).map_err(|e| e.to_string())?;
        return Ok(ts.to_zoned(tz.clone()));
    }
    if let Ok(ts) = s.parse::<Timestamp>() {
        return Ok(ts.to_zoned(tz.clone()));
    }
    if let Ok(dt) = s.parse::<jiff::civil::DateTime>() {
        return dt.to_zoned(tz.clone()).map_err(|e| e.to_string());
    }
    if let Ok(d) = s.parse::<jiff::civil::Date>() {
        return d.to_zoned(tz.clone()).map_err(|e| e.to_string());
    }
    Err(format!("not a date: {s:?}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateUnit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
}

fn unit(chars: &[char], i: &mut usize) -> Result<DateUnit, String> {
    let c = *chars.get(*i).ok_or("missing unit")?;
    *i += 1;
    Ok(match c {
        'y' => DateUnit::Year,
        'M' => DateUnit::Month,
        'w' => DateUnit::Week,
        'd' => DateUnit::Day,
        'h' | 'H' => DateUnit::Hour,
        'm' if chars.get(*i) == Some(&'s') => {
            *i += 1;
            DateUnit::Millisecond
        }
        'm' => DateUnit::Minute,
        's' => DateUnit::Second,
        other => return Err(format!("unknown unit {other:?}")),
    })
}

fn apply(mut at: Zoned, math: &str, round_up: bool) -> Result<Zoned, String> {
    let chars: Vec<char> = math.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let op = chars[i];
        i += 1;
        match op {
            '/' => at = round(&at, unit(&chars, &mut i)?, round_up)?,
            '+' | '-' => {
                let start = i;
                while chars.get(i).is_some_and(char::is_ascii_digit) {
                    i += 1;
                }
                let n: i64 = if start == i {
                    1
                } else {
                    chars[start..i]
                        .iter()
                        .collect::<String>()
                        .parse()
                        .map_err(|_| "bad number")?
                };
                let n = if op == '-' { -n } else { n };
                let span = match unit(&chars, &mut i)? {
                    DateUnit::Year => n.years(),
                    DateUnit::Month => n.months(),
                    DateUnit::Week => n.weeks(),
                    DateUnit::Day => n.days(),
                    DateUnit::Hour => n.hours(),
                    DateUnit::Minute => n.minutes(),
                    DateUnit::Second => n.seconds(),
                    DateUnit::Millisecond => n.milliseconds(),
                };
                at = at.checked_add(span).map_err(|e| e.to_string())?;
            }
            other => return Err(format!("unexpected {other:?}")),
        }
    }
    Ok(at)
}

/// Start of the unit containing `at`, or its last millisecond when rounding up.
fn round(at: &Zoned, unit: DateUnit, round_up: bool) -> Result<Zoned, String> {
    let start = match unit {
        DateUnit::Year => at
            .start_of_day()
            .map_err(|e| e.to_string())?
            .first_of_year()
            .map_err(|e| e.to_string())?,
        DateUnit::Month => at
            .start_of_day()
            .map_err(|e| e.to_string())?
            .first_of_month()
            .map_err(|e| e.to_string())?,
        DateUnit::Week => {
            let back = at.weekday().since(Weekday::Sunday) as i64;
            at.start_of_day()
                .map_err(|e| e.to_string())?
                .checked_sub(back.days())
                .map_err(|e| e.to_string())?
        }
        DateUnit::Day => at.start_of_day().map_err(|e| e.to_string())?,
        DateUnit::Hour => trunc(at, Unit::Hour)?,
        DateUnit::Minute => trunc(at, Unit::Minute)?,
        DateUnit::Second => trunc(at, Unit::Second)?,
        DateUnit::Millisecond => trunc(at, Unit::Millisecond)?,
    };
    if !round_up {
        return Ok(start);
    }
    let next: Span = match unit {
        DateUnit::Year => 1.year(),
        DateUnit::Month => 1.month(),
        DateUnit::Week => 1.week(),
        DateUnit::Day => 1.day(),
        DateUnit::Hour => 1.hour(),
        DateUnit::Minute => 1.minute(),
        DateUnit::Second => 1.second(),
        DateUnit::Millisecond => 1.millisecond(),
    };
    start
        .checked_add(next)
        .and_then(|z| z.checked_sub(1.millisecond()))
        .map_err(|e| e.to_string())
}

fn trunc(at: &Zoned, unit: Unit) -> Result<Zoned, String> {
    at.round(ZonedRound::new().smallest(unit).mode(RoundMode::Trunc))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-10-02T15:45:30.250Z".parse().unwrap()
    }

    fn at(expr: &str, tz: &str, round_up: bool) -> String {
        let tz = TimeZone::get(tz).unwrap();
        let ms = parse(expr, now(), &tz, round_up).unwrap();
        Timestamp::from_millisecond(ms).unwrap().to_string()
    }

    #[test]
    fn now_and_offsets() {
        assert_eq!(at("now", "UTC", false), "2026-10-02T15:45:30.25Z");
        assert_eq!(at("now-90d", "UTC", false), "2026-07-04T15:45:30.25Z");
        assert_eq!(at("now+1h-30m", "UTC", false), "2026-10-02T16:15:30.25Z");
    }

    #[test]
    fn rounding_down_and_up() {
        assert_eq!(at("now/d", "UTC", false), "2026-10-02T00:00:00Z");
        assert_eq!(at("now/d", "UTC", true), "2026-10-02T23:59:59.999Z");
        assert_eq!(at("now-6M/M", "UTC", false), "2026-04-01T00:00:00Z");
        assert_eq!(at("now-90d/d", "UTC", false), "2026-07-04T00:00:00Z");
        // 2026-10-02 is a Friday; Kibana weeks start on Sunday.
        assert_eq!(at("now/w", "UTC", false), "2026-09-27T00:00:00Z");
        assert_eq!(at("now/y", "UTC", false), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn rounding_happens_in_the_dashboard_time_zone() {
        // 15:45 UTC is 08:45 in Los Angeles (PDT, UTC-7) on the same day.
        assert_eq!(
            at("now/d", "America/Los_Angeles", false),
            "2026-10-02T07:00:00Z"
        );
        // 15:45 UTC is 21:15 in Kolkata (UTC+5:30): that day began at 18:30 UTC the day before.
        assert_eq!(at("now/d", "Asia/Kolkata", false), "2026-10-01T18:30:00Z");
    }

    #[test]
    fn absolute_dates() {
        assert_eq!(
            at("2026-04-01T00:00:00.000Z", "UTC", false),
            "2026-04-01T00:00:00Z"
        );
        assert_eq!(
            at("2026-04-01", "Europe/London", false),
            "2026-03-31T23:00:00Z"
        );
        assert_eq!(
            at("2026-04-01T00:00:00Z||+1M/M", "UTC", true),
            "2026-05-31T23:59:59.999Z"
        );
        assert_eq!(at("1775001600000", "UTC", false), "2026-04-01T00:00:00Z");
    }

    #[test]
    fn ranges() {
        let r = resolve("now-7d/d", "now/d", now(), &TimeZone::UTC).unwrap();
        assert_eq!(r.duration_ms(), 8 * 86_400_000 - 1);
        assert_eq!(r.previous().to_ms, r.from_ms);
        assert!(resolve("now", "now-1d", now(), &TimeZone::UTC).is_err());
        assert!(parse("now-3x", now(), &TimeZone::UTC, false).is_err());
    }
}
