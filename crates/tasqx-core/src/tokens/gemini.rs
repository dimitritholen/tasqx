//! Gemini CLI telemetry-outfile parser (backlog #16a).
//!
//! # SCHEMA-FROM-DOCS — NOT EMPIRICALLY VERIFIED
//!
//! Unlike the Claude Code / Codex parsers, this module was written from the
//! documented telemetry schema (docs/research/token-accounting.md and the
//! upstream gemini-cli telemetry docs), NOT from real session files — this
//! machine has no Gemini install to sample. Every field name, envelope shape,
//! and timestamp encoding below is a documented expectation, not an observed
//! fact. Treat the fixtures as hypotheses and re-verify against a real
//! `.gemini/telemetry.log` before trusting the numbers. The parser is written
//! defensively precisely because the exact envelope is unconfirmed.
//!
//! # Format
//!
//! When `telemetry.outfile` is configured, Gemini CLI writes OTLP-shaped JSON
//! log records to that file (default under `~/.gemini`). The file is a *stream*
//! of pretty-printed JSON objects — not strict JSONL — so we split on
//! brace-balanced top-level objects; that split also handles one-object-per-line
//! and a whole-file JSON array as degenerate cases. The records we care about
//! are `gemini_cli.api_response` log events carrying per-request token counts.
//!
//! # Mapping (see docs/research/token-accounting.md and #16a)
//!
//! - `input_tokens`  = `input_token_count`.
//! - `output_tokens` = `output_token_count` + `thoughts_token_count`. Thought
//!   ("thinking") tokens are billed as output, so they belong in the output
//!   bucket rather than being dropped.
//! - `cache_read_tokens` = `cached_content_token_count`, taken as-is. Gemini's
//!   docs do not state whether cached tokens are a *subset* of
//!   `input_token_count` or a disjoint bucket. We DO NOT subtract; if a future
//!   empirical check shows cached is a subset of input, input would be
//!   double-counting the cached portion and this needs revisiting.
//! - `cache_creation_tokens` = 0. Gemini exposes no cache-creation counter.
//! - `tool_token_count` is deliberately LEFT OUT. It is not clearly additive
//!   with `input_token_count` (documented as a separate breakdown, likely a
//!   subset of the prompt), so folding it in risks double-counting. Revisit
//!   once verified against a real install.

use crate::error::ApiError;
use crate::tokens::UsageSample;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The log-event discriminator we key on. Only records naming this event carry
/// the per-request token breakdown; other telemetry events are ignored.
const API_RESPONSE_EVENT: &str = "gemini_cli.api_response";

/// Directories where Gemini CLI writes session/telemetry data.
///
/// Honors `$GEMINI_DATA_DIR` (the documented override that relocates the whole
/// `.gemini` data dir, under which both the `tmp/` session dir and a default
/// `telemetry.log` live); otherwise `~/.gemini`. Returns an empty vec only if
/// neither the override nor a home directory can be resolved.
pub fn default_roots() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("GEMINI_DATA_DIR").filter(|v| !v.is_empty()) {
        return vec![PathBuf::from(dir)];
    }
    match home_dir() {
        Some(home) => vec![home.join(".gemini")],
        None => vec![],
    }
}

/// Parse every usable `gemini_cli.api_response` sample from one telemetry
/// outfile. Opening the file is the only hard error (it names the path);
/// unparseable documents and records without a usable timestamp are skipped so
/// one corrupt record never sinks the whole file.
pub fn samples_from_file(path: &Path) -> Result<Vec<UsageSample>, ApiError> {
    // Read bytes and decode lossily: only *opening* the file is a hard error, so
    // a stray non-UTF8 byte must not sink an otherwise-parseable file. The bad
    // byte becomes U+FFFD and only that object fails to parse; the rest survive.
    let bytes = std::fs::read(path).map_err(|e| {
        ApiError::internal(format!(
            "failed to read Gemini telemetry file {}: {e}",
            path.display()
        ))
    })?;
    let content = String::from_utf8_lossy(&bytes);

    let mut samples = Vec::new();
    for chunk in split_json_objects(&content) {
        let Ok(value) = serde_json::from_str::<Value>(chunk) else {
            continue;
        };
        if let Some(sample) = sample_from_record(&value) {
            samples.push(sample);
        }
    }
    Ok(samples)
}

/// Split a byte string into its top-level brace-balanced `{...}` slices.
///
/// The outfile concatenates pretty-printed objects with no separator, so we
/// scan for balanced brace depth while respecting string literals and escapes.
/// Anything outside a balanced object (whitespace, array brackets, commas, a
/// stray closing brace) is ignored, which is what makes the same routine eat
/// JSONL, concatenated pretty JSON, and an array-wrapped file alike.
fn split_json_objects(content: &str) -> Vec<&str> {
    let bytes = content.as_bytes();
    let mut chunks = Vec::new();
    let mut depth: u32 = 0;
    let mut start = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            // A stray closing brace (depth already 0) is noise between
            // objects; the guard ignores it rather than underflowing.
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    chunks.push(&content[start..=i]);
                }
            }
            _ => {}
        }
    }
    chunks
}

/// Turn one parsed record into a sample, or `None` if it is not an
/// api_response event or has no usable timestamp.
fn sample_from_record(value: &Value) -> Option<UsageSample> {
    let record = value.as_object()?;
    // OTLP log records nest event fields under `attributes`; some serializers
    // flatten them onto the record. Prefer the attributes bag, fall back to the
    // record itself, so both shapes parse.
    let attrs = record
        .get("attributes")
        .and_then(Value::as_object)
        .unwrap_or(record);

    if event_name(record, attrs)? != API_RESPONSE_EVENT {
        return None;
    }

    let ts = event_timestamp(record, attrs)?;

    let input = u64_field(attrs, "input_token_count");
    let output = u64_field(attrs, "output_token_count")
        .saturating_add(u64_field(attrs, "thoughts_token_count"));
    let cache_read = u64_field(attrs, "cached_content_token_count");
    let model = attrs
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    Some(UsageSample {
        id: None,
        ts,
        model,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: 0,
    })
}

/// Locate the event name across the shapes the exporter might emit.
fn event_name<'a>(
    record: &'a serde_json::Map<String, Value>,
    attrs: &'a serde_json::Map<String, Value>,
) -> Option<&'a str> {
    ["event.name", "name"]
        .iter()
        .find_map(|k| attrs.get(*k).and_then(Value::as_str))
        .or_else(|| {
            ["event.name", "name", "body"]
                .iter()
                .find_map(|k| record.get(*k).and_then(Value::as_str))
        })
}

/// Resolve the event timestamp as RFC3339, or `None` when unusable.
///
/// OTel event records carry an ISO-8601 `event.timestamp`; the record envelope
/// may also carry `timestamp`/`observedTimestamp`. A string is parsed as
/// RFC3339 and re-emitted canonically. A number is read as `timeUnixNano`
/// (nanoseconds since the Unix epoch, the OTLP wire convention) — UNCERTAIN:
/// some SDK JSON serializers emit micro- or milliseconds instead, which this
/// would misread by a factor of 1000; re-verify the numeric encoding against a
/// real outfile.
fn event_timestamp(
    record: &serde_json::Map<String, Value>,
    attrs: &serde_json::Map<String, Value>,
) -> Option<String> {
    let raw = attrs
        .get("event.timestamp")
        .or_else(|| attrs.get("timestamp"))
        .or_else(|| record.get("timestamp"))
        .or_else(|| record.get("observedTimestamp"))?;

    match raw {
        Value::String(s) => s.parse::<jiff::Timestamp>().ok().map(|t| t.to_string()),
        Value::Number(n) => n
            .as_i128()
            .and_then(|nanos| jiff::Timestamp::from_nanosecond(nanos).ok())
            .map(|t| t.to_string()),
        _ => None,
    }
}

/// Read a token counter tolerantly: JSON integer, non-negative float, or a
/// numeric string all count; anything else (or absent, or negative) is 0.
fn u64_field(attrs: &serde_json::Map<String, Value>, key: &str) -> u64 {
    match attrs.get(key) {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite() && *f >= 0.0)
                    .map(|f| f as u64)
            })
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<u64>().unwrap_or(0),
        _ => 0,
    }
}

/// Home directory from the environment, no external crate.
fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Write `content` to a uniquely named temp file and return its path; the
    /// caller drives `samples_from_file`, which needs a real path on disk.
    fn write_fixture(content: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("tasqx-gemini-{}-{n}.log", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create fixture");
        f.write_all(content.as_bytes()).expect("write fixture");
        path
    }

    /// One synthetic api_response record in the nested-`attributes` shape — the
    /// documented OTLP envelope this module was written against (with the
    /// SCHEMA-FROM-DOCS caveat at the top of the file). The flattened variant
    /// has its own fixture in `flattened_attributes_shape_also_parses`, and
    /// both must stay: they exercise the two halves of `sample_from_record`'s
    /// attributes-then-record fallback, so relabelling either fixture into the
    /// other's shape would leave one half untested while the suite stayed
    /// green.
    fn record(ts: &str, input: u64, output: u64, thoughts: u64, cached: u64) -> String {
        format!(
            r#"{{
  "attributes": {{
    "event.name": "gemini_cli.api_response",
    "event.timestamp": "{ts}",
    "model": "gemini-2.5-pro",
    "input_token_count": {input},
    "output_token_count": {output},
    "thoughts_token_count": {thoughts},
    "cached_content_token_count": {cached},
    "tool_token_count": 99,
    "total_token_count": {total}
  }}
}}"#,
            total = input + output + thoughts + cached
        )
    }

    #[test]
    fn happy_path_parses_multiple_concatenated_pretty_objects() {
        // Two pretty-printed objects with no separator — the outfile's real
        // shape — must both parse.
        let content = format!(
            "{}\n{}\n",
            record("2026-07-24T10:00:00Z", 100, 40, 10, 5),
            record("2026-07-24T10:01:00Z", 200, 80, 20, 0)
        );
        let path = write_fixture(&content);
        let samples = samples_from_file(&path).expect("parse");

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].ts, "2026-07-24T10:00:00Z");
        assert_eq!(samples[0].input_tokens, 100);
        // output = output_token_count + thoughts_token_count.
        assert_eq!(samples[0].output_tokens, 50);
        // cached taken as-is, NOT subtracted from input.
        assert_eq!(samples[0].cache_read_tokens, 5);
        assert_eq!(samples[0].cache_creation_tokens, 0);
        assert_eq!(samples[0].model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(samples[1].input_tokens, 200);
        assert_eq!(samples[1].output_tokens, 100);
    }

    #[test]
    fn tool_token_count_is_never_folded_into_input() {
        // tool_token_count is 99 in every fixture record; it must not leak into
        // any bucket (double-count risk).
        let path = write_fixture(&record("2026-07-24T10:00:00Z", 100, 40, 0, 0));
        let samples = samples_from_file(&path).expect("parse");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 100);
        assert_eq!(samples[0].output_tokens, 40);
        assert_eq!(samples[0].cache_read_tokens, 0);
    }

    #[test]
    fn one_object_per_line_also_parses() {
        // Compact JSONL is the other accepted shape.
        let content = concat!(
            r#"{"attributes":{"event.name":"gemini_cli.api_response","event.timestamp":"2026-07-24T10:00:00Z","input_token_count":10,"output_token_count":5,"thoughts_token_count":0,"cached_content_token_count":0}}"#,
            "\n",
            r#"{"attributes":{"event.name":"gemini_cli.api_response","event.timestamp":"2026-07-24T10:05:00Z","input_token_count":20,"output_token_count":7,"thoughts_token_count":3,"cached_content_token_count":0}}"#,
            "\n",
        );
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].output_tokens, 10);
    }

    #[test]
    fn malformed_object_is_skipped_and_later_records_still_parse() {
        // A brace-balanced but invalid-JSON chunk sits between two good
        // records; only the good ones survive.
        let content = format!(
            "{}\n{{ this is not valid json, but braces balance }}\n{}\n",
            record("2026-07-24T10:00:00Z", 100, 40, 0, 0),
            record("2026-07-24T10:02:00Z", 300, 60, 0, 0)
        );
        let path = write_fixture(&content);
        let samples = samples_from_file(&path).expect("parse");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].input_tokens, 100);
        assert_eq!(samples[1].input_tokens, 300);
    }

    #[test]
    fn non_api_response_events_are_ignored() {
        // A different telemetry event with token-ish fields must not become a
        // sample; only gemini_cli.api_response counts.
        let content = format!(
            "{}\n{}\n",
            r#"{"attributes":{"event.name":"gemini_cli.user_prompt","event.timestamp":"2026-07-24T09:00:00Z","input_token_count":5}}"#,
            record("2026-07-24T10:00:00Z", 100, 40, 0, 0)
        );
        let path = write_fixture(&content);
        let samples = samples_from_file(&path).expect("parse");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 100);
    }

    #[test]
    fn record_without_timestamp_is_skipped() {
        // Usage present, timestamp absent -> unbucketable -> skipped.
        let content = r#"{"attributes":{"event.name":"gemini_cli.api_response","input_token_count":100,"output_token_count":40}}"#;
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        assert!(samples.is_empty());
    }

    #[test]
    fn numeric_unix_nano_timestamp_is_converted_to_rfc3339() {
        // 2026-07-24T10:00:00Z == 1784887200 s == 1784887200000000000 ns.
        let content = r#"{"attributes":{"event.name":"gemini_cli.api_response","input_token_count":1,"output_token_count":1},"timestamp":1784887200000000000}"#;
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].ts, "2026-07-24T10:00:00Z");
    }

    #[test]
    fn empty_and_irrelevant_files_yield_no_samples() {
        let empty = write_fixture("");
        assert!(samples_from_file(&empty).expect("parse").is_empty());

        let noise = write_fixture("not json at all\n[]\n{ }\n\n");
        assert!(samples_from_file(&noise).expect("parse").is_empty());
    }

    #[test]
    fn non_utf8_bytes_do_not_sink_the_file() {
        // A non-UTF8 byte in the stream must decode lossily and cost at most the
        // one object it corrupts, not turn the read into a hard error.
        let good = record("2026-07-24T10:00:00Z", 100, 40, 0, 0);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\xff not utf8}\n");
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("tasqx-gemini-utf8-{}-{n}.log", std::process::id()));
        std::fs::write(&path, &bytes).expect("write fixture");
        let samples = samples_from_file(&path).expect("non-utf8 must not error");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 100);
    }

    #[test]
    fn missing_file_is_an_error_naming_the_path() {
        let path = std::env::temp_dir().join("tasqx-gemini-does-not-exist-xyz.log");
        let err = samples_from_file(&path).expect_err("io error");
        assert!(
            err.message.contains("tasqx-gemini-does-not-exist-xyz.log"),
            "message must name the path: {}",
            err.message
        );
    }

    #[test]
    fn flattened_attributes_shape_also_parses() {
        // Some exporters flatten event fields onto the record instead of
        // nesting them under `attributes`; both must work.
        let content = r#"{"event.name":"gemini_cli.api_response","event.timestamp":"2026-07-24T10:00:00Z","input_token_count":42,"output_token_count":8,"thoughts_token_count":2}"#;
        let path = write_fixture(content);
        let samples = samples_from_file(&path).expect("parse");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].input_tokens, 42);
        assert_eq!(samples[0].output_tokens, 10);
    }

    #[test]
    fn default_roots_honors_the_env_override() {
        // Guard the process-global env with serialized set/restore.
        let prev = std::env::var_os("GEMINI_DATA_DIR");
        // SAFETY: single-threaded test body; restored before returning.
        unsafe { std::env::set_var("GEMINI_DATA_DIR", "/custom/gemini/data") };
        let roots = default_roots();
        match prev {
            Some(v) => unsafe { std::env::set_var("GEMINI_DATA_DIR", v) },
            None => unsafe { std::env::remove_var("GEMINI_DATA_DIR") },
        }
        assert_eq!(roots, vec![PathBuf::from("/custom/gemini/data")]);
    }
}
