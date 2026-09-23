//! Shared scaffolding for the `gemini` and `copilot` OTEL-record parsers:
//! both read a telemetry file lossily and pull short strings and `u64`
//! counts out of a JSON attribute map, and previously hand-rolled the same
//! prelude and coercions under different names (`u64_field`/`attr_number`,
//! `event_name`/`first_attr`). This file is that shared layer only — the
//! record-splitting, timestamp-decoding and dedup logic stay in each parser,
//! since those genuinely differ between the two envelope shapes (#736 part A).

use serde_json::{Map, Value};
use std::path::Path;

use crate::error::ApiError;

/// Read a telemetry/otel file lossily: only *opening* it is a hard error
/// (named via `source` in the message); a stray non-UTF8 byte becomes U+FFFD
/// rather than sinking the whole file, so a later per-record parse failure
/// costs only that one record.
pub(crate) fn read_lossy(path: &Path, source: &str) -> Result<String, ApiError> {
    let bytes = std::fs::read(path).map_err(|e| {
        ApiError::internal(format!(
            "failed to read {source} file {}: {e}",
            path.display()
        ))
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// First of `keys` present in `map` as a string, or `None`. No trimming or
/// emptiness filtering here — callers that need that (copilot's
/// `attr_string`) add it on top; gemini's `event_name` does not.
pub(crate) fn first_str<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| map.get(*k).and_then(Value::as_str))
}

/// A single JSON-object attribute coerced to `u64`, `0` when absent or not a
/// recognized numeric shape: a JSON number or a numeric string. `accept_float`
/// preserves each caller's own pre-existing tolerance for the non-`u64`
/// numeric fallback — gemini's `u64_field` accepted a non-negative finite
/// float, copilot's `attr_number` (via `value_to_u64`) instead accepted a
/// non-negative `i64` — rather than widening or narrowing either parser.
pub(crate) fn attr_u64(map: &Map<String, Value>, key: &str, accept_float: bool) -> u64 {
    match map.get(key) {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| {
                if accept_float {
                    n.as_f64()
                        .filter(|f| f.is_finite() && *f >= 0.0)
                        .map(|f| f as u64)
                } else {
                    n.as_i64().and_then(|i| u64::try_from(i).ok())
                }
            })
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}
