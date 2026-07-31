//! GitHub Copilot CLI token-usage parser: `~/.copilot/otel/*.jsonl`.
//!
//! SCHEMA-FROM-DOCS (not empirically verified): no `~/.copilot/otel` directory
//! existed on the dev machine when this was written. The record shapes below
//! are derived from the OTEL GenAI semantic conventions and cross-checked
//! against ccusage's Copilot adapter (which *is* built from real exports), but
//! they were never observed against a live local file here. Treat the exact
//! field spellings as best-effort and keep the version tolerance strict.
//!
//! Copilot emits OTEL records as one JSON object per line. The token counts
//! live on an `attributes` map addressed by dotted OTEL keys
//! (`gen_ai.usage.input_tokens`, …). Timestamps show up in several exporter
//! encodings (JS `hrTime` `[seconds, nanos]` arrays, epoch scalars, RFC3339
//! strings), so a record without any resolvable timestamp is skipped rather
//! than guessed at.
//!
//! What we accept: any line whose `attributes` carry a positive
//! `gen_ai.usage.input_tokens`/`output_tokens` (or a cache count) plus a
//! usable timestamp — regardless of whether the record is a span, an inference
//! log, or an agent-turn log. We do NOT gate on the record kind the way
//! ccusage does; instead we collapse the redundant copies Copilot writes for a
//! single model response by deduplicating on `gen_ai.response.id`. That is the
//! reason ccusage needs its span/log precedence lattice, and a response-id
//! dedup is the minimal correct defense against double counting while staying
//! tolerant of record shapes we have never seen.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::ApiError;
use crate::tokens::UsageSample;

/// File-level exporter override. When set, Copilot writes its OTEL export to
/// this exact path instead of the default directory (ccusage honors the same
/// variable). It names a *file*, so `default_roots` contributes its parent
/// directory — the callers scan roots for `*.jsonl`.
const OTEL_EXPORTER_PATH_ENV: &str = "COPILOT_OTEL_FILE_EXPORTER_PATH";

/// OTEL attribute keys carrying the model name, most specific first.
const MODEL_ATTRS: &[&str] = &["gen_ai.response.model", "gen_ai.request.model"];

/// Cache-creation is spelled two ways across Copilot versions; accept both.
const CACHE_CREATION_ATTRS: &[&str] = &[
    "gen_ai.usage.cache_write.input_tokens",
    "gen_ai.usage.cache_creation.input_tokens",
];

/// Directories where Copilot CLI writes session token data. `~/.copilot/otel`
/// plus, if the file-level exporter override is set, that file's parent dir.
pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".copilot").join("otel"));
    }
    if let Some(dir) = exporter_override_dir() {
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    roots
}

/// Parse one Copilot OTEL JSONL file into per-request usage samples.
///
/// Version tolerance is the prime directive: an unreadable file is an error
/// (internal, naming the path), but every *line-level* problem — malformed
/// JSON, no `attributes`, no usage, no timestamp — skips just that line. A file
/// with nothing usable returns `Ok(vec![])`.
pub fn samples_from_file(path: &Path) -> Result<Vec<UsageSample>, ApiError> {
    // Read bytes and decode lossily: only *opening* the file is a hard error, so
    // a stray non-UTF8 byte must not sink an otherwise-parseable file. The bad
    // byte becomes U+FFFD and only that line fails to parse; the rest survive.
    let bytes = std::fs::read(path).map_err(|e| {
        ApiError::internal(format!(
            "failed to read Copilot otel file {}: {e}",
            path.display()
        ))
    })?;
    let content = String::from_utf8_lossy(&bytes);

    // Copilot re-emits one model response under several record shapes (a chat
    // span, an inference-details log, an agent-turn log) and the copies expose
    // DIFFERENT subsets of the usage breakdown — e.g. a lean span with no cache
    // fields written before a detailed log that carries the cache_read /
    // cache_creation split. Collapsing to the first-seen copy would silently
    // drop that detail and misattribute fresh vs. cached input. Instead we
    // accumulate the RAW counts per `gen_ai.response.id`, keeping the largest
    // value seen for each field, and derive fresh input once at the end. This is
    // order-independent (whichever copy arrives first, the merged result is the
    // same) and still counts each response exactly once.
    let mut accs: Vec<ResponseAcc> = Vec::new();
    let mut slot_by_response: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue; // malformed JSON: skip this line, never fail the file
        };
        let Some(attributes) = record.get("attributes").and_then(Value::as_object) else {
            continue; // not a usage-bearing OTEL record
        };

        let cache_read = attr_number(attributes, "gen_ai.usage.cache_read.input_tokens");
        let cache_creation = attr_number_first(attributes, CACHE_CREATION_ATTRS);
        let raw_input = attr_number(attributes, "gen_ai.usage.input_tokens");
        let output = attr_number(attributes, "gen_ai.usage.output_tokens");
        if raw_input == 0 && output == 0 && cache_read == 0 && cache_creation == 0 {
            continue; // no token usage on this record
        }

        // Skip before touching the dedup map so a later, timestamped copy of the
        // same response can still win if this one lacks a usable timestamp.
        let Some(ts) = resolve_timestamp(&record) else {
            continue;
        };

        let incoming = ResponseAcc {
            ts,
            model: first_attr(attributes, MODEL_ATTRS),
            raw_input,
            output,
            cache_read,
            cache_creation,
        };

        match attr_string(attributes, "gen_ai.response.id") {
            Some(id) => match slot_by_response.get(&id) {
                // Same model response under another record shape: merge, keeping
                // the richest value for each field so no breakdown is lost.
                Some(&idx) => accs[idx].merge_max(incoming),
                None => {
                    slot_by_response.insert(id, accs.len());
                    accs.push(incoming);
                }
            },
            // No response id to dedupe on: keep every such record on its own.
            None => accs.push(incoming),
        }
    }

    Ok(accs.into_iter().map(ResponseAcc::into_sample).collect())
}

/// Raw per-response usage accumulated across the redundant record shapes Copilot
/// writes for one model response. Holds RAW `input_tokens` (which still includes
/// the cache-read tokens); fresh input is derived only in [`into_sample`].
struct ResponseAcc {
    ts: String,
    model: Option<String>,
    raw_input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

impl ResponseAcc {
    /// Fold another copy of the same response in, keeping the largest value seen
    /// for each count. `ts` stays first-seen; `model` fills in if still unknown.
    fn merge_max(&mut self, other: ResponseAcc) {
        self.raw_input = self.raw_input.max(other.raw_input);
        self.output = self.output.max(other.output);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_creation = self.cache_creation.max(other.cache_creation);
        if self.model.is_none() {
            self.model = other.model;
        }
    }

    /// Copilot follows OpenAI-style accounting: `input_tokens` INCLUDES the
    /// cache-read tokens. `UsageSample` keeps the four counts non-overlapping, so
    /// subtract the cache reads back out to get fresh input (matches ccusage's
    /// Copilot adapter).
    fn into_sample(self) -> UsageSample {
        let input = self
            .raw_input
            .saturating_sub(self.raw_input.min(self.cache_read));
        UsageSample {
            id: None,
            ts: self.ts,
            model: self.model,
            input_tokens: input,
            output_tokens: self.output,
            cache_read_tokens: self.cache_read,
            cache_creation_tokens: self.cache_creation,
        }
    }
}

/// Home directory from `HOME` (Unix) or `USERPROFILE` (Windows). No `dirs`
/// dependency in this crate, so resolve the env vars directly.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()))
        .map(PathBuf::from)
}

/// Parent directory of the file-level exporter override, if configured.
fn exporter_override_dir() -> Option<PathBuf> {
    let raw = std::env::var(OTEL_EXPORTER_PATH_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // A bare filename (`export.jsonl`) has an empty parent; its directory is the
    // current one, so map that to `.` rather than injecting an empty path.
    Path::new(trimmed).parent().map(|p| {
        if p.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            p.to_path_buf()
        }
    })
}

/// Resolve a record's timestamp to an RFC3339 string, or `None` if no field
/// yields a usable instant. Priority follows the exporter's own field order:
/// span end/start times, high-resolution `hrTime` arrays, then epoch scalars.
fn resolve_timestamp(record: &Value) -> Option<String> {
    // JS OTEL SDK `[seconds, nanos]` arrays.
    for key in ["endTime", "startTime", "hrTime", "_hrTime", "time"] {
        if let Some(millis) = record.get(key).and_then(hrtime_to_millis) {
            return millis_to_rfc3339(millis);
        }
    }
    // RFC3339 strings (already an instant): normalize to UTC `Z`.
    for key in ["time", "timestamp", "observedTimestamp"] {
        if let Some(text) = record.get(key).and_then(Value::as_str) {
            if let Some(ts) = normalize_rfc3339(text) {
                return Some(ts);
            }
        }
    }
    // Epoch scalars in unknown units, plus explicit nanos.
    for key in ["timestamp", "observedTimestamp"] {
        if let Some(millis) = record.get(key).and_then(scalar_epoch_to_millis) {
            return millis_to_rfc3339(millis);
        }
    }
    record
        .get("timeUnixNano")
        .and_then(unix_nanos_to_millis)
        .and_then(millis_to_rfc3339)
}

/// `[seconds, nanos]` (JS `hrTime`) to epoch millis.
fn hrtime_to_millis(value: &Value) -> Option<i64> {
    let parts = value.as_array()?;
    let seconds = value_to_u64(parts.first()?)?;
    let nanos = value_to_u64(parts.get(1)?)?;
    let millis = seconds.checked_mul(1_000)?.checked_add(nanos / 1_000_000)?;
    i64::try_from(millis).ok()
}

/// Epoch scalar in an unknown unit to millis, disambiguated by magnitude
/// (same heuristic ccusage uses): ns, µs, ms, or s.
fn scalar_epoch_to_millis(value: &Value) -> Option<i64> {
    let raw = value_to_u64(value)?;
    let millis = if raw >= 100_000_000_000_000_000 {
        raw / 1_000_000
    } else if raw >= 100_000_000_000_000 {
        raw / 1_000
    } else if raw >= 100_000_000_000 {
        raw
    } else {
        raw.checked_mul(1_000)?
    };
    i64::try_from(millis).ok()
}

/// Explicit `timeUnixNano` to millis; zero means "unset".
fn unix_nanos_to_millis(value: &Value) -> Option<i64> {
    let raw = value_to_u64(value)?;
    (raw > 0)
        .then_some(raw / 1_000_000)
        .and_then(|m| i64::try_from(m).ok())
}

/// Epoch millis to an RFC3339 UTC string via jiff.
fn millis_to_rfc3339(millis: i64) -> Option<String> {
    jiff::Timestamp::from_millisecond(millis)
        .ok()
        .map(|t| t.to_string())
}

/// Validate/normalize an already-formatted timestamp string to RFC3339 UTC.
/// Anything jiff cannot read as an instant (e.g. an offset-less local time) is
/// rejected — an ambiguous timestamp is worse than none for time bucketing.
fn normalize_rfc3339(text: &str) -> Option<String> {
    text.trim()
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.to_string())
}

/// Coerce a JSON value to `u64`, accepting numbers and numeric strings (OTEL
/// exporters render attribute values as both).
fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_i64().and_then(|i| u64::try_from(i).ok())),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// A single attribute as `u64`, defaulting to 0 when absent or non-numeric.
fn attr_number(attributes: &Map<String, Value>, key: &str) -> u64 {
    attributes.get(key).and_then(value_to_u64).unwrap_or(0)
}

/// First of `keys` that yields a positive count, else 0.
fn attr_number_first(attributes: &Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter()
        .map(|key| attr_number(attributes, key))
        .find(|&value| value > 0)
        .unwrap_or(0)
}

/// A single attribute as a trimmed non-empty string.
fn attr_string(attributes: &Map<String, Value>, key: &str) -> Option<String> {
    attributes
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// First of `keys` present as a non-empty string.
fn first_attr(attributes: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| attr_string(attributes, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `content` to a unique temp file and return its path. Synthetic,
    /// hand-written fixtures only — never real session logs (private data).
    fn write_fixture(content: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tasqx-copilot-test-{}-{}.jsonl",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::write(&path, content).expect("write temp fixture");
        path
    }

    fn millis_of(ts: &str) -> i64 {
        ts.parse::<jiff::Timestamp>()
            .expect("sample ts is RFC3339")
            .as_millisecond()
    }

    #[test]
    fn happy_path_yields_one_sample_per_response() {
        // Two distinct model responses, each an inference log with the OTEL
        // GenAI usage attributes and an hrTime `[seconds, nanos]` timestamp.
        let content = concat!(
            r#"{"attributes":{"gen_ai.response.id":"r1","gen_ai.response.model":"gpt-5","gen_ai.usage.input_tokens":1000,"gen_ai.usage.output_tokens":200,"gen_ai.usage.cache_read.input_tokens":300},"hrTime":[1700000000,0]}"#,
            "\n",
            r#"{"attributes":{"gen_ai.response.id":"r2","gen_ai.request.model":"gpt-5-mini","gen_ai.usage.input_tokens":50,"gen_ai.usage.output_tokens":10},"hrTime":[1700000100,0]}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 2);
        // input reported as 1000 but includes 300 cache-read → fresh input 700.
        assert_eq!(samples[0].input_tokens, 700);
        assert_eq!(samples[0].output_tokens, 200);
        assert_eq!(samples[0].cache_read_tokens, 300);
        assert_eq!(samples[0].cache_creation_tokens, 0);
        assert_eq!(samples[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(millis_of(&samples[0].ts), 1_700_000_000_000);

        assert_eq!(samples[1].input_tokens, 50);
        assert_eq!(samples[1].model.as_deref(), Some("gpt-5-mini"));
    }

    #[test]
    fn malformed_line_is_skipped_and_later_lines_still_parse() {
        let content = concat!(
            r#"{"attributes":{"gen_ai.response.id":"r1","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5},"timeUnixNano":1700000000000000000}"#,
            "\n",
            "{ this is not valid json \n",
            r#"{"attributes":{"gen_ai.response.id":"r2","gen_ai.usage.input_tokens":20,"gen_ai.usage.output_tokens":6},"timeUnixNano":1700000001000000000}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].input_tokens, 10);
        assert_eq!(samples[1].input_tokens, 20);
    }

    #[test]
    fn irrelevant_or_empty_file_yields_no_samples() {
        // Blank lines, a record without attributes, and a record whose
        // attributes carry no token usage — none should produce a sample.
        let content = concat!(
            "\n",
            r#"{"name":"some span","startTime":[1700000000,0]}"#,
            "\n",
            r#"{"attributes":{"http.method":"GET"},"hrTime":[1700000000,0]}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert!(samples.is_empty());
    }

    #[test]
    fn timestamps_convert_from_every_supported_encoding() {
        // hrTime array with sub-second nanos, an epoch-millis scalar, and an
        // RFC3339 string with a non-UTC offset (must normalize to Z).
        let content = concat!(
            r#"{"attributes":{"gen_ai.response.id":"a","gen_ai.usage.output_tokens":1},"hrTime":[1700000000,500000000]}"#,
            "\n",
            r#"{"attributes":{"gen_ai.response.id":"b","gen_ai.usage.output_tokens":1},"timestamp":1700000000500}"#,
            "\n",
            r#"{"attributes":{"gen_ai.response.id":"c","gen_ai.usage.output_tokens":1},"time":"2026-07-24T09:30:00+02:00"}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 3);
        assert_eq!(millis_of(&samples[0].ts), 1_700_000_000_500);
        assert_eq!(millis_of(&samples[1].ts), 1_700_000_000_500);
        // 09:30 +02:00 == 07:30:00Z
        assert_eq!(samples[2].ts, "2026-07-24T07:30:00Z");
    }

    #[test]
    fn usage_without_a_usable_timestamp_is_skipped() {
        let content = concat!(
            r#"{"attributes":{"gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5}}"#,
            "\n",
            // A timestamp string with no offset is ambiguous → rejected.
            r#"{"attributes":{"gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5},"timestamp":"2026-07-24T09:30:00"}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert!(samples.is_empty());
    }

    #[test]
    fn duplicate_response_id_is_collapsed_to_one_sample() {
        // The same response reported twice (e.g. as a chat span and again as an
        // inference log) must count once.
        let content = concat!(
            r#"{"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.id":"dup","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":40},"endTime":[1700000000,0]}"#,
            "\n",
            r#"{"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.response.id":"dup","gen_ai.usage.input_tokens":100,"gen_ai.usage.output_tokens":40},"timeUnixNano":1700000000000000000}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 100);
        assert_eq!(samples[0].output_tokens, 40);
    }

    #[test]
    fn both_cache_creation_spellings_and_numeric_strings_are_accepted() {
        // cache-creation under the `cache_creation` spelling, and every count
        // rendered as a numeric string (some exporters stringify attributes).
        let content = concat!(
            r#"{"attributes":{"gen_ai.response.id":"s","gen_ai.usage.input_tokens":"800","gen_ai.usage.output_tokens":"30","gen_ai.usage.cache_read.input_tokens":"200","gen_ai.usage.cache_creation.input_tokens":"64"},"hrTime":[1700000000,0]}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 600); // 800 - 200 cache-read
        assert_eq!(samples[0].output_tokens, 30);
        assert_eq!(samples[0].cache_read_tokens, 200);
        assert_eq!(samples[0].cache_creation_tokens, 64);
    }

    #[test]
    fn cache_only_record_is_kept_with_zero_input() {
        // A record whose only usage is cache-read (input == cache-read) still
        // carries real, billable tokens and must not be dropped.
        let content = concat!(
            r#"{"attributes":{"gen_ai.response.id":"c","gen_ai.usage.input_tokens":500,"gen_ai.usage.cache_read.input_tokens":500},"hrTime":[1700000000,0]}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 0);
        assert_eq!(samples[0].cache_read_tokens, 500);
    }

    #[test]
    fn duplicate_response_merges_the_richer_breakdown() {
        // A lean chat span (no cache fields) is written BEFORE the detailed
        // inference log carrying the cache_read / cache_creation split. First-
        // wins dedup would keep the span and report fresh input as the full
        // 1000; the merge must instead pick up the cache breakdown and derive
        // fresh input = 1000 - 300 = 700. Order-independent by construction.
        let content = concat!(
            r#"{"attributes":{"gen_ai.operation.name":"chat","gen_ai.response.id":"r","gen_ai.usage.input_tokens":1000,"gen_ai.usage.output_tokens":200},"endTime":[1700000000,0]}"#,
            "\n",
            r#"{"attributes":{"event.name":"gen_ai.client.inference.operation.details","gen_ai.response.id":"r","gen_ai.usage.input_tokens":1000,"gen_ai.usage.output_tokens":200,"gen_ai.usage.cache_read.input_tokens":300,"gen_ai.usage.cache_creation.input_tokens":64},"timeUnixNano":1700000000000000000}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 700);
        assert_eq!(samples[0].output_tokens, 200);
        assert_eq!(samples[0].cache_read_tokens, 300);
        assert_eq!(samples[0].cache_creation_tokens, 64);
    }

    #[test]
    fn non_utf8_bytes_do_not_sink_the_file() {
        // One line carries an invalid UTF-8 byte; it must decode lossily, fail
        // JSON parsing on its own, and leave the valid record untouched.
        let good = concat!(
            r#"{"attributes":{"gen_ai.response.id":"ok","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5},"#,
            r#""timeUnixNano":1700000000000000000}"#,
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\xff not utf8}\n");
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tasqx-copilot-utf8-{}-{}.jsonl",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::write(&path, &bytes).expect("write fixture");
        let samples = samples_from_file(&path).expect("non-utf8 must not error");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 10);
        assert_eq!(samples[0].output_tokens, 5);
    }

    #[test]
    fn missing_file_is_an_error_naming_the_path() {
        let path = PathBuf::from("/nonexistent/tasqx/copilot/does-not-exist.jsonl");
        let err = samples_from_file(&path).expect_err("io error");
        assert!(
            err.message.contains("does-not-exist.jsonl"),
            "message must name the path: {}",
            err.message
        );
    }

    #[test]
    fn default_roots_include_the_otel_dir() {
        // Exercised only when HOME is set (it is, in CI and locally); assert the
        // canonical `.copilot/otel` root appears.
        if home_dir().is_some() {
            let roots = default_roots();
            assert!(
                roots
                    .iter()
                    .any(|r| r.ends_with("otel")
                        && r.parent().is_some_and(|p| p.ends_with(".copilot"))),
                "roots must include ~/.copilot/otel: {roots:?}"
            );
        }
    }
}
