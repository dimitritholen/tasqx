//! Small time/JSON helpers shared by the engine.
//!
//! Timestamps are RFC3339 strings; `jiff` is used only to generate "now" and
//! to compute differences (duration tracked, is-future checks). Parsing is
//! best-effort — an unparseable client-supplied date never panics, it simply
//! fails the comparison it feeds.

use jiff::Timestamp;
use serde_json::Value;

use crate::error::ApiError;

/// Current instant as an RFC3339 (UTC) string, e.g. `2026-07-15T11:06:10Z`.
pub fn now() -> String {
    Timestamp::now().to_string()
}

/// Parse an RFC3339 timestamp; returns None on anything jiff can't read.
pub fn parse_ts(s: &str) -> Option<Timestamp> {
    s.parse::<Timestamp>().ok()
}

/// Parse an ISO-8601 duration (`PT4H`, `P1DT2H30M`, `PT90M`) into whole
/// seconds. Calendar units use fixed conventions (day=86400s, week=604800s,
/// month≈30d, year≈365d) — good enough for estimate roll-ups. Returns None on
/// anything not matching the `P[nY][nM][nW][nD]T[nH][nM][nS]` shape.
pub fn duration_secs(iso: &str) -> Option<i64> {
    let s = iso.trim();
    let mut chars = s.chars();
    if chars.next() != Some('P') {
        return None;
    }
    let mut secs: i64 = 0;
    let mut num = String::new();
    let mut in_time = false;
    let mut saw_any = false;
    for c in chars {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                saw_any = true;
                // Checked throughout: an estimate large enough to overflow is
                // client-supplied (import, JSON API, MCP) and must fail the
                // read, not abort the process. Unchecked `*`/`+=` here made
                // `report` panic in debug and print a wrapped total in release.
                let part = match c {
                    'Y' => n.checked_mul(365 * 86_400)?,
                    'W' => n.checked_mul(604_800)?,
                    'D' => n.checked_mul(86_400)?,
                    'H' if in_time => n.checked_mul(3_600)?,
                    'M' if in_time => n.checked_mul(60)?,
                    'M' => n.checked_mul(30 * 86_400)?, // months (date position)
                    'S' if in_time => n,
                    _ => return None,
                };
                secs = secs.checked_add(part)?;
            }
        }
    }
    if !num.is_empty() || !saw_any {
        return None;
    }
    Some(secs)
}

/// True when `s` parses to a timestamp strictly in the future.
pub fn is_future(s: &Option<String>) -> bool {
    match s {
        Some(v) => match parse_ts(v) {
            Some(t) => t > Timestamp::now(),
            None => false,
        },
        None => false,
    }
}

/// Seconds elapsed between two RFC3339 instants (`later - earlier`), clamped at
/// zero. Returns 0 if either side is missing or unparseable.
pub fn seconds_between(earlier: &Option<String>, later: &str) -> i64 {
    let (Some(a), Some(b)) = (earlier.as_deref().and_then(parse_ts), parse_ts(later)) else {
        return 0;
    };
    let secs = b.as_second() - a.as_second();
    secs.max(0)
}

/// Format a whole number of seconds as an ISO-8601 duration (`PT1H52M`, `PT0S`).
pub fn iso_duration(seconds: i64) -> String {
    if seconds <= 0 {
        return "PT0S".to_string();
    }
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    let mut out = String::from("PT");
    if h > 0 {
        out.push_str(&format!("{h}H"));
    }
    if m > 0 {
        out.push_str(&format!("{m}M"));
    }
    if s > 0 || (h == 0 && m == 0) {
        out.push_str(&format!("{s}S"));
    }
    out
}

/// Extract a required string field from a params object.
pub fn req_str(p: &Value, key: &str) -> Result<String, ApiError> {
    p.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(format!("missing or empty required field: {key}"))
        })
}

/// Extract an optional non-empty string field.
pub fn opt_str(p: &Value, key: &str) -> Option<String> {
    p.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Extract an optional array-of-strings field (missing => empty vec).
pub fn opt_str_array(p: &Value, key: &str) -> Vec<String> {
    p.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `duration_secs` promises `Option` — it must USE it on overflow rather
    /// than aborting the process. A value this large is reachable: it is what
    /// `parse_duration` used to emit for `1000000000000000000w`, and `report`
    /// read it back on every roll-up, panicking in debug and silently wrapping
    /// to garbage in release. The contract is total: no input aborts.
    #[test]
    fn an_overflowing_duration_returns_none_rather_than_panicking() {
        for bad in [
            "P7000000000000000000D",
            "PT999999999999999999H",
            "P9223372036854775807Y",
            "P9223372036854775807W",
            "P4000000000000000000DT9223372036854775807S",
        ] {
            assert_eq!(duration_secs(bad), None, "{bad:?} must be None, not a panic");
        }
    }

    /// The guard must not narrow the range of durations that legitimately work.
    #[test]
    fn ordinary_durations_still_read_back() {
        assert_eq!(duration_secs("PT4H"), Some(14_400));
        assert_eq!(duration_secs("P1DT2H30M"), Some(95_400));
        assert_eq!(duration_secs("P1W"), Some(604_800));
    }
}
