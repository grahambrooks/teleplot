//! Date histogram intervals, chosen the way Kibana's `TimeBuckets` does so
//! imported dashboards bucket identically (`histogram:barTarget` 50,
//! `histogram:maxBars` 100 by default).

use serde_json::{Value, json};

use crate::model::{Interval, IntervalUnit};

const SECOND: i64 = 1000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
/// moment.js durations: a month is 30 days, a year 365.
const MONTH: i64 = 30 * DAY;
const YEAR: i64 = 365 * DAY;

/// Kibana's `boundsDescendingRaw`: (bound, interval) in milliseconds.
const BOUNDS: [(i64, i64); 17] = [
    (i64::MAX, YEAR),
    (YEAR, MONTH),
    (3 * WEEK, WEEK),
    (WEEK, DAY),
    (DAY, 12 * HOUR),
    (6 * HOUR, 3 * HOUR),
    (2 * HOUR, HOUR),
    (45 * MINUTE, 30 * MINUTE),
    (20 * MINUTE, 10 * MINUTE),
    (9 * MINUTE, 5 * MINUTE),
    (3 * MINUTE, MINUTE),
    (45 * SECOND, 30 * SECOND),
    (15 * SECOND, 10 * SECOND),
    (7500, 5 * SECOND),
    (5 * SECOND, SECOND),
    (500, 100),
    (0, 0), // sentinel: never chosen as a bound
];

/// The interval Kibana picks for "auto": nearest nice interval for `target` buckets.
fn auto_near(target: u32, duration_ms: i64) -> i64 {
    let per_bucket = duration_ms as f64 / target as f64;
    let index = BOUNDS[..16]
        .iter()
        .position(|(bound, _)| (*bound as f64) <= per_bucket);
    match index {
        Some(i) if i > 0 => BOUNDS[i - 1].1,
        _ => (per_bucket.floor() as i64).max(1),
    }
}

/// The smallest nice interval producing at most `max` buckets.
fn auto_less_than(max: u32, duration_ms: i64) -> i64 {
    let per_bucket = duration_ms as f64 / max as f64;
    BOUNDS[..16]
        .iter()
        .map(|(_, interval)| *interval)
        .find(|interval| (*interval as f64) <= per_bucket)
        .unwrap_or_else(|| (per_bucket.floor() as i64).max(1))
}

/// A resolved interval: its length, and the Elasticsearch parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub ms: i64,
    pub es: Value,
    /// Set when the requested interval was too fine and was scaled up.
    pub scaled_from: Option<String>,
}

pub fn resolve(interval: Interval, duration_ms: i64, bar_target: u32, max_bars: u32) -> Resolved {
    let requested = match interval {
        Interval::Auto => auto_near(bar_target, duration_ms),
        Interval::Every { count, unit } => count as i64 * unit_ms(unit),
    };
    let minimum = auto_less_than(max_bars, duration_ms);
    let (ms, scaled_from) = if duration_ms > 0 && requested < minimum {
        (
            minimum,
            matches!(interval, Interval::Every { .. }).then(|| interval.to_string()),
        )
    } else {
        (requested, None)
    };
    Resolved {
        ms,
        es: es_interval(ms),
        scaled_from,
    }
}

pub(super) fn unit_ms(unit: IntervalUnit) -> i64 {
    match unit {
        IntervalUnit::Millisecond => 1,
        IntervalUnit::Second => SECOND,
        IntervalUnit::Minute => MINUTE,
        IntervalUnit::Hour => HOUR,
        IntervalUnit::Day => DAY,
        IntervalUnit::Week => WEEK,
        IntervalUnit::Month => MONTH,
        IntervalUnit::Quarter => 3 * MONTH,
        IntervalUnit::Year => YEAR,
    }
}

/// Kibana's `convertDurationToNormalizedEsInterval` then `dateHistogramInterval`:
/// the largest unit that divides evenly (multiples of y/M/w are not allowed),
/// as `calendar_interval` for exactly one unit and `fixed_interval` otherwise.
fn es_interval(ms: i64) -> Value {
    const UNITS: [(&str, i64, bool); 8] = [
        ("y", YEAR, true),
        ("M", MONTH, true),
        ("w", WEEK, true),
        ("d", DAY, false),
        ("h", HOUR, false),
        ("m", MINUTE, false),
        ("s", SECOND, false),
        ("ms", 1, false),
    ];
    for (unit, size, large) in UNITS {
        if ms >= size && ms % size == 0 {
            let n = ms / size;
            if large && n != 1 {
                continue;
            }
            return if n == 1 && unit != "s" && unit != "ms" {
                json!({ "calendar_interval": format!("1{unit}") })
            } else {
                json!({ "fixed_interval": format!("{n}{unit}") })
            };
        }
    }
    json!({ "fixed_interval": format!("{}ms", ms.max(1)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn week() -> Interval {
        Interval::Every {
            count: 1,
            unit: IntervalUnit::Week,
        }
    }

    #[test]
    fn auto_matches_kibana() {
        // 184 days / 50 bars ≈ 3.7 days per bucket → 1 day.
        let r = resolve(Interval::Auto, 184 * DAY, 50, 100);
        assert_eq!(r.es, json!({ "calendar_interval": "1d" }));
        // 7 days → 3.4h per bucket → 3 hours.
        assert_eq!(
            resolve(Interval::Auto, 7 * DAY, 50, 100).es,
            json!({ "fixed_interval": "3h" })
        );
        // 15 minutes → 18s per bucket: the first bound under that is 15s,
        // and Kibana takes the interval of the bound above it → 30 seconds.
        assert_eq!(
            resolve(Interval::Auto, 15 * MINUTE, 50, 100).es,
            json!({ "fixed_interval": "30s" })
        );
        // 5 years → ~36 days → 1 month.
        assert_eq!(
            resolve(Interval::Auto, 5 * YEAR, 50, 100).es,
            json!({ "calendar_interval": "1M" })
        );
    }

    #[test]
    fn explicit_intervals_are_kept_unless_too_fine() {
        let r = resolve(week(), 184 * DAY, 50, 100);
        assert_eq!(r.es, json!({ "calendar_interval": "1w" }));
        assert_eq!(r.scaled_from, None);
        // 1 day over 184 days is 184 buckets, but the scaling floor is the
        // nice interval under 1.84 days, which is 1 day: no scaling.
        assert_eq!(
            resolve("1d".parse().unwrap(), 184 * DAY, 50, 100).scaled_from,
            None
        );
        // 1 hour over 30 days would be 720 buckets. Kibana's floor is the
        // largest nice interval at or under 30d / 100 = 7.2h, which is 3h.
        let r = resolve("1h".parse().unwrap(), 30 * DAY, 50, 100);
        assert_eq!(r.es, json!({ "fixed_interval": "3h" }));
        assert_eq!(r.scaled_from.as_deref(), Some("1h"));
    }

    #[test]
    fn multiples_of_large_units_become_fixed_days() {
        assert_eq!(es_interval(2 * WEEK), json!({ "fixed_interval": "14d" }));
        assert_eq!(es_interval(12 * HOUR), json!({ "fixed_interval": "12h" }));
        assert_eq!(es_interval(MONTH), json!({ "calendar_interval": "1M" }));
        assert_eq!(es_interval(30 * SECOND), json!({ "fixed_interval": "30s" }));
    }
}
