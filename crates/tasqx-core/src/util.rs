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

/// True when `s` parses to a timestamp strictly after `now`.
///
/// `now` is a parameter, not `Timestamp::now()`, because the one rule that reads
/// this — [`crate::types::effective_status`] — is a time-driven state transition
/// and has to be testable on both sides of its boundary without sleeping.
pub fn is_future_at(s: Option<&str>, now: Timestamp) -> bool {
    match s.and_then(parse_ts) {
        Some(t) => t > now,
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

// ---- typed params extraction ------------------------------------------------
//
// D32. Every params value the engine reads comes through this layer, and the
// rule it enforces is one line: a PRESENT value of the WRONG TYPE is a
// `bad_request`; an ABSENT value keeps its default.
//
// The shape it replaces was `p.get(key).and_then(Value::as_i64)` — an accessor
// that answers `None` for "not given" and "given as the wrong type" alike, so
// every caller's `.unwrap_or(default)` silently swallowed the second case. That
// is not a lint about tidiness: `expected_rev: "1"` skipped the optimistic
// concurrency guard entirely and the stale write landed, which is a lost update
// caused by a guard failing OPEN. A stringified number is exactly what a
// JavaScript client sends, so the protection evaporated for the callers most
// likely to be relying on it.
//
// `null` counts as ABSENT, not as a type error: it is how a JS client spells
// "no value", and treating it as a mistake would refuse requests that work today.

/// Human name for the JSON type of `v`, for use in an error message.
pub fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(n) if n.is_f64() => "a fractional number",
        Value::Number(_) => "an integer",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The one error this layer raises: names the param, the type received, the
/// type expected, and the way out.
fn wrong_type(key: &str, expected: &str, got: &Value) -> ApiError {
    ApiError::bad_request(format!(
        "`{key}` must be {expected}, but {} was given ({got}) — send {expected} or omit `{key}`",
        type_of(got)
    ))
}

/// Look up `key`, mapping both "missing" and "null" to `None`.
fn present<'a>(p: &'a Value, key: &str) -> Option<&'a Value> {
    p.get(key).filter(|v| !v.is_null())
}

/// Extract a required string field from a params object.
pub fn req_str(p: &Value, key: &str) -> Result<String, ApiError> {
    // An empty string is "missing" rather than a type error: it is the shape
    // `use "$UNSET"` produces, and D23 already fixed that wording in place.
    opt_str(p, key)?
        .ok_or_else(|| ApiError::bad_request(format!("missing or empty required field: {key}")))
}

/// Extract an optional string field. An empty string reads as absent, which is
/// what every caller already assumed; a non-string is refused.
pub fn opt_str(p: &Value, key: &str) -> Result<Option<String>, ApiError> {
    match present(p, key) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some("") => Ok(None),
            Some(s) => Ok(Some(s.to_string())),
            None => Err(wrong_type(key, "a string", v)),
        },
    }
}

/// Extract an optional integer field.
pub fn opt_i64(p: &Value, key: &str) -> Result<Option<i64>, ApiError> {
    match present(p, key) {
        None => Ok(None),
        Some(v) => v.as_i64().map(Some).ok_or_else(|| wrong_type(key, "an integer", v)),
    }
}

/// Extract a required integer field.
pub fn req_i64(p: &Value, key: &str) -> Result<i64, ApiError> {
    opt_i64(p, key)?
        .ok_or_else(|| ApiError::bad_request(format!("missing required field: {key}")))
}

/// Extract an optional non-negative integer field. A negative or fractional
/// number is refused here rather than dropped: `limit: -1` used to mean "no
/// limit", i.e. the opposite of what it says.
pub fn opt_u64(p: &Value, key: &str) -> Result<Option<u64>, ApiError> {
    match present(p, key) {
        None => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| match v.as_i64() {
            // "but an integer was given (-1)" would name the right type and
            // still not say what is wrong with it.
            Some(n) => ApiError::bad_request(format!(
                "`{key}` must be a non-negative integer, but {n} was given — send 0 or more, or omit `{key}`"
            )),
            None => wrong_type(key, "a non-negative integer", v),
        }),
    }
}

/// Extract an optional boolean field. `"true"` is a string, not a truth.
pub fn opt_bool(p: &Value, key: &str) -> Result<Option<bool>, ApiError> {
    match present(p, key) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| wrong_type(key, "a boolean", v)),
    }
}

/// Extract an optional array field, borrowed so the caller can inspect entries.
pub fn opt_array<'a>(p: &'a Value, key: &str) -> Result<Option<&'a Vec<Value>>, ApiError> {
    match present(p, key) {
        None => Ok(None),
        Some(v) => v.as_array().map(Some).ok_or_else(|| wrong_type(key, "an array", v)),
    }
}

/// Extract a required array field.
pub fn req_array<'a>(p: &'a Value, key: &str) -> Result<&'a Vec<Value>, ApiError> {
    opt_array(p, key)?
        .ok_or_else(|| ApiError::bad_request(format!("missing required field: {key} (an array)")))
}

/// Extract a required object field.
pub fn req_object<'a>(
    p: &'a Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, Value>, ApiError> {
    match present(p, key) {
        None => Err(ApiError::bad_request(format!("missing required field: {key} (an object)"))),
        Some(v) => v.as_object().ok_or_else(|| wrong_type(key, "an object", v)),
    }
}

/// Extract an optional array-of-strings field (absent => empty vec).
///
/// All three of its old silent drops are now refusals, and both callers get the
/// same strictness: `tag.add` used to reject a non-array while `task.add` took
/// one and stored no tags at all, so the same mistake had two answers.
pub fn opt_str_array(p: &Value, key: &str) -> Result<Vec<String>, ApiError> {
    let Some(arr) = opt_array(p, key)? else { return Ok(Vec::new()) };
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let Some(s) = v.as_str() else {
            return Err(ApiError::bad_request(format!(
                "`{key}` entry {v} must be a string, but {} was given",
                type_of(v)
            )));
        };
        if s.is_empty() {
            return Err(ApiError::bad_request(format!(
                "`{key}` contains an empty string — drop the entry rather than sending \"\""
            )));
        }
        out.push(s.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// D32's drift guard, and the reason this is one decision rather than a
    /// fourteenth fix: every hole in the family had the same *syntax*, so the
    /// syntax is what gets banned. A keyed read chained straight into a raw
    /// `serde_json` accessor answers `None` for "absent" and "wrong type"
    /// alike, and the caller's `.unwrap_or(default)` then swallows the second
    /// case — which is how `expected_rev: "1"` skipped the concurrency guard.
    ///
    /// Derived, not hand-maintained (D30): it reads the source and bans the
    /// shape, so a param added tomorrow is covered the day it is written.
    #[test]
    fn no_engine_param_is_read_with_a_raw_json_accessor() {
        // `//` lines are stripped first: the prose in engine.rs *quotes* the
        // banned shape when explaining why it is banned.
        let code: String = include_str!("engine.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // Collapse whitespace so a chain split across lines reads as one.
        let flat: String = code.chars().filter(|c| !c.is_whitespace()).collect();

        let mut holes = Vec::new();
        for (i, _) in flat.match_indices(".get(\"") {
            let rest = &flat[i + ".get(".len()..];
            let Some(close) = rest.find(')') else { continue };
            let (key, after) = (&rest[..close], &rest[close + 1..]);
            if after.starts_with(".and_then(") || after.starts_with(".as_") {
                holes.push(format!(".get({key}){}", &after[..after.len().min(24)]));
            }
        }
        assert!(
            holes.is_empty(),
            "engine.rs reads a param with a raw accessor, which cannot tell an absent value \
             from a wrong-typed one — route it through util's typed layer (opt_i64, opt_bool, \
             opt_u64, opt_str, opt_str_array, opt_array, req_*) instead:\n  {}",
            holes.join("\n  ")
        );
    }

    /// The layer's whole contract in one test: PRESENT + wrong type is refused
    /// and the message names the param; ABSENT (missing or `null`) is absent.
    #[test]
    fn a_present_wrong_type_is_refused_and_an_absent_one_is_not() {
        let bad = json!({ "n": "1", "b": "true", "s": 5, "u": -1, "a": "x", "f": 2.5 });
        for (key, err) in [
            ("n", opt_i64(&bad, "n").err()),
            ("b", opt_bool(&bad, "b").err()),
            ("s", opt_str(&bad, "s").err()),
            ("u", opt_u64(&bad, "u").err()),
            ("a", opt_str_array(&bad, "a").err()),
            ("f", opt_i64(&bad, "f").err()),
        ] {
            let e = err.unwrap_or_else(|| panic!("`{key}` of the wrong type must be refused"));
            assert!(e.message.contains(key), "message must name `{key}`: {}", e.message);
        }

        for absent in [json!({}), json!({ "n": null, "b": null, "s": null, "a": null })] {
            assert_eq!(opt_i64(&absent, "n").unwrap(), None);
            assert_eq!(opt_bool(&absent, "b").unwrap(), None);
            assert_eq!(opt_str(&absent, "s").unwrap(), None);
            assert_eq!(opt_u64(&absent, "u").unwrap(), None);
            assert_eq!(opt_str_array(&absent, "a").unwrap(), Vec::<String>::new());
        }

        // Right-typed values still arrive intact.
        let good = json!({ "n": -3, "b": true, "s": "hi", "u": 7, "a": ["x", "y"] });
        assert_eq!(opt_i64(&good, "n").unwrap(), Some(-3));
        assert_eq!(opt_bool(&good, "b").unwrap(), Some(true));
        assert_eq!(opt_str(&good, "s").unwrap(), Some("hi".to_string()));
        assert_eq!(opt_u64(&good, "u").unwrap(), Some(7));
        assert_eq!(opt_str_array(&good, "a").unwrap(), vec!["x".to_string(), "y".to_string()]);
    }

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
