use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A date histogram interval as Kibana writes it: `auto`, `1w`, `12h`, or a
/// bare unit (`w`) in legacy visualizations, which means one of that unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
    Auto,
    Every { count: u32, unit: IntervalUnit },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalUnit {
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl IntervalUnit {
    fn suffix(self) -> &'static str {
        match self {
            IntervalUnit::Millisecond => "ms",
            IntervalUnit::Second => "s",
            IntervalUnit::Minute => "m",
            IntervalUnit::Hour => "h",
            IntervalUnit::Day => "d",
            IntervalUnit::Week => "w",
            IntervalUnit::Month => "M",
            IntervalUnit::Quarter => "q",
            IntervalUnit::Year => "y",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ms" => IntervalUnit::Millisecond,
            "s" => IntervalUnit::Second,
            "m" => IntervalUnit::Minute,
            "h" => IntervalUnit::Hour,
            "d" => IntervalUnit::Day,
            "w" => IntervalUnit::Week,
            "M" => IntervalUnit::Month,
            "q" => IntervalUnit::Quarter,
            "y" => IntervalUnit::Year,
            _ => return None,
        })
    }
}

impl FromStr for Interval {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() || s == "auto" {
            return Ok(Interval::Auto);
        }
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (digits, unit) = s.split_at(split);
        let count = if digits.is_empty() {
            1
        } else {
            digits.parse().map_err(|_| format!("bad interval {s:?}"))?
        };
        let unit =
            IntervalUnit::parse(unit).ok_or_else(|| format!("unknown interval unit in {s:?}"))?;
        if count == 0 {
            return Err(format!("zero interval {s:?}"));
        }
        Ok(Interval::Every { count, unit })
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Interval::Auto => f.write_str("auto"),
            Interval::Every { count, unit } => write!(f, "{count}{}", unit.suffix()),
        }
    }
}

impl Serialize for Interval {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Interval {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kibana_intervals() {
        let week = Interval::Every {
            count: 1,
            unit: IntervalUnit::Week,
        };
        assert_eq!("1w".parse::<Interval>().unwrap(), week);
        assert_eq!("w".parse::<Interval>().unwrap(), week, "legacy bare unit");
        assert_eq!("auto".parse::<Interval>().unwrap(), Interval::Auto);
        assert_eq!(
            "12h".parse::<Interval>().unwrap(),
            Interval::Every {
                count: 12,
                unit: IntervalUnit::Hour
            }
        );
        assert_eq!(
            "1M".parse::<Interval>().unwrap(),
            Interval::Every {
                count: 1,
                unit: IntervalUnit::Month
            },
            "M is month, m is minute"
        );
        assert!("3x".parse::<Interval>().is_err());
        assert!("0d".parse::<Interval>().is_err());
    }

    #[test]
    fn round_trips_through_its_string_form() {
        for s in ["auto", "30s", "5m", "12h", "1d", "1w", "1M", "1q", "1y"] {
            assert_eq!(s.parse::<Interval>().unwrap().to_string(), s);
        }
    }
}
