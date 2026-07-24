//! Parser for OpenAI Codex CLI `rollout-*.jsonl` session logs.
//!
//! Codex writes one JSON object per line under
//! `${CODEX_HOME:-~/.codex}/sessions/YYYY/MM/DD/` (and `archived_sessions/`).
//! The format is an undocumented internal that changes without notice, so the
//! prime directive here is version tolerance: an unparseable line is skipped,
//! unknown fields are ignored, a file with no usable samples yields
//! `Ok(vec![])`, and nothing panics on any input. Only failing to *open* the
//! file is an error.
//!
//! # Token model mapping (read this before trusting the numbers)
//!
//! Every kept `token_count` event carries a `last_token_usage` block with
//! OpenAI's fields: `input_tokens`, `cached_input_tokens`, `output_tokens`,
//! `reasoning_output_tokens`, `total_tokens`. Two facts drive the mapping onto
//! tasqx's four-field [`UsageSample`] schema:
//!
//! * `cached_input_tokens` is a **subset** of `input_tokens` (a cache *read*),
//!   not an addition to it.
//! * Codex has **no cache-write concept** — it never reports cache creation.
//!
//! So each sample is mapped as:
//!
//! | tasqx field             | Codex source                              |
//! |-------------------------|-------------------------------------------|
//! | `input_tokens`          | `input_tokens - cached_input_tokens` (fresh input) |
//! | `cache_read_tokens`     | `cached_input_tokens`                      |
//! | `cache_creation_tokens` | `0` (Codex has none)                       |
//! | `output_tokens`         | `output_tokens` (includes reasoning)      |
//!
//! Subtracting the cached portion out of `input_tokens` is what keeps the
//! four-field schema comparable across tools: everywhere in tasqx,
//! `input_tokens` means *fresh* input and cache reads live in their own field.
//!
//! # The dedup rule (empirically verified — see docs/research/token-accounting.md)
//!
//! `token_count` events are duplicated (~2x) and each carries both a cumulative
//! `total_token_usage` and a per-request `last_token_usage`, with the invariant
//! `Δtotal_token_usage == last_token_usage` at every distinct step. We keep only
//! events whose `total_token_usage.total_tokens` changed from the previously
//! kept event (plus the first), and emit one sample per kept event. When the
//! invariant is violated (`Δtotal != last`) we treat the cumulative totals as
//! authoritative and emit the delta instead — that path is a format-drift
//! tripwire and logs a line to stderr.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::ApiError;
use crate::tokens::UsageSample;

/// Session-level context from the first `session_meta` line. The attribution
/// engine matches Codex sessions to tasqx tasks by `cwd`, so that field is the
/// reason this is exposed at all; `id` and `cli_version` come along because they
/// are cheap and useful for debugging. Every field is optional because old CLI
/// builds omit some of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexSessionMeta {
    pub id: Option<String>,
    pub cwd: Option<String>,
    pub cli_version: Option<String>,
}

/// A cumulative or per-request token reading, as Codex spells it. `total` is the
/// dedup key; `cached` is a subset of `input`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CodexUsage {
    input: u64,
    cached: u64,
    output: u64,
    total: u64,
}

/// Read a `token_count` usage block (`total_token_usage` / `last_token_usage`).
/// Missing numeric fields default to 0; `total_tokens` absent means the block is
/// unusable for the dedup rule, so the caller skips the event.
fn read_usage(block: &Value) -> Option<CodexUsage> {
    let total = block.get("total_tokens").and_then(Value::as_u64)?;
    Some(CodexUsage {
        input: block
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached: block
            .get("cached_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: block
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total,
    })
}

/// Map a Codex usage triple onto the four tasqx fields. See the module docs for
/// why `input` loses its cached portion and cache-creation is always 0.
fn map_fields(u: CodexUsage) -> (u64, u64, u64) {
    let fresh_input = u.input.saturating_sub(u.cached);
    (fresh_input, u.cached, u.output)
}

/// Parse every usable per-request sample out of one rollout file.
///
/// Opening the file is the only hard error. Everything after is best-effort:
/// malformed lines, events without a usable timestamp, and events with no
/// changed total are all skipped, and a file with nothing usable returns
/// `Ok(vec![])`.
pub fn samples_from_file(path: &Path) -> Result<Vec<UsageSample>, ApiError> {
    // Read bytes and decode lossily: only *opening* the file is a hard error, so
    // a stray non-UTF8 byte must not sink an otherwise-parseable file. The bad
    // byte becomes U+FFFD, that one line fails to parse as JSON, and every other
    // line is still processed.
    let bytes = std::fs::read(path).map_err(|e| {
        ApiError::internal(format!(
            "cannot read Codex rollout file {}: {e}",
            path.display()
        ))
    })?;
    let content = String::from_utf8_lossy(&bytes);

    let mut samples = Vec::new();
    // The last kept event's cumulative totals; `None` until the first kept
    // event, which the dedup rule always keeps.
    let mut prev_total: Option<CodexUsage> = None;
    // The model from the most recent `turn_context` line, stamped onto every
    // sample that follows it.
    let mut current_model: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            // A malformed line is skipped, never fatal — later lines still parse.
            continue;
        };

        match value.get("type").and_then(Value::as_str) {
            Some("turn_context") => {
                if let Some(model) = value
                    .pointer("/payload/model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                {
                    current_model = Some(model.to_string());
                }
            }
            Some("event_msg") => {
                if value.pointer("/payload/type").and_then(Value::as_str) != Some("token_count") {
                    continue;
                }
                let Some(info) = value.pointer("/payload/info") else {
                    continue;
                };
                let Some(cumulative) = info.get("total_token_usage").and_then(read_usage) else {
                    continue;
                };

                // THE dedup rule: keep only events whose cumulative total moved.
                let changed = prev_total.is_none_or(|p| p.total != cumulative.total);
                if !changed {
                    continue;
                }

                // A sample with usage but no usable RFC3339 timestamp is skipped.
                let Some(ts) = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .filter(|s| crate::util::parse_ts(s).is_some())
                else {
                    // Do not advance `prev_total`: a dropped event must not make
                    // the next real one look unchanged.
                    continue;
                };

                // Per-request usage: normally `last_token_usage`, but the totals
                // are authoritative. When the verified invariant
                // `Δtotal == last` breaks, emit the delta and trip the wire.
                let baseline = prev_total.unwrap_or_default();
                let delta = CodexUsage {
                    input: cumulative.input.saturating_sub(baseline.input),
                    cached: cumulative.cached.saturating_sub(baseline.cached),
                    output: cumulative.output.saturating_sub(baseline.output),
                    total: cumulative.total.saturating_sub(baseline.total),
                };
                let last = info.get("last_token_usage").and_then(read_usage);
                let usage = match last {
                    Some(last) if last == delta => last,
                    Some(last) => {
                        eprintln!(
                            "codex token-count drift in {}: Δtotal {:?} != last_token_usage {:?}; \
                             using the authoritative delta",
                            path.display(),
                            delta,
                            last
                        );
                        delta
                    }
                    // No `last_token_usage` at all: fall back to the delta.
                    None => delta,
                };

                let (input_tokens, cache_read_tokens, output_tokens) = map_fields(usage);
                samples.push(UsageSample {
                    ts: ts.to_string(),
                    model: current_model.clone(),
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_tokens: 0,
                });
                prev_total = Some(cumulative);
            }
            _ => {}
        }
    }

    Ok(samples)
}

/// Read the session-level metadata from the first `session_meta` line.
///
/// Returns `Ok(None)` when no `session_meta` line is present (e.g. a truncated
/// or non-Codex file); opening the file is the only hard error.
pub fn session_meta(path: &Path) -> Result<Option<CodexSessionMeta>, ApiError> {
    // See `samples_from_file`: lossy decode so a non-UTF8 byte cannot turn a
    // best-effort read into a hard error.
    let bytes = std::fs::read(path).map_err(|e| {
        ApiError::internal(format!(
            "cannot read Codex rollout file {}: {e}",
            path.display()
        ))
    })?;
    let content = String::from_utf8_lossy(&bytes);

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        let str_field = |k: &str| {
            payload
                .get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        return Ok(Some(CodexSessionMeta {
            id: str_field("id"),
            cwd: str_field("cwd"),
            cli_version: str_field("cli_version"),
        }));
    }

    Ok(None)
}

/// The directories Codex writes session data into: `sessions/` and
/// `archived_sessions/` under `$CODEX_HOME` (or `~/.codex` when unset).
///
/// Returns an empty vec when neither `$CODEX_HOME` nor a home directory can be
/// resolved — the caller has no roots to scan, which is not an error.
pub fn default_roots() -> Vec<PathBuf> {
    let Some(home) = codex_home() else {
        return Vec::new();
    };
    vec![home.join("sessions"), home.join("archived_sessions")]
}

/// `$CODEX_HOME` if set and non-empty, else `~`/`.codex`.
fn codex_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".codex"))
}

/// Best-effort home directory without pulling in a dependency: `$HOME` on Unix,
/// `%USERPROFILE%` on Windows.
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

    /// Write `lines` (already newline-joined) to a temp file and return it. The
    /// file lives in the process temp dir with a pid+nonce name so parallel test
    /// runs never collide.
    fn temp_rollout(lines: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let n = NONCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("tasqx-codex-test-{}-{n}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create temp rollout");
        f.write_all(lines.as_bytes()).expect("write temp rollout");
        path
    }

    /// One `token_count` line: `total_*` are the cumulative totals, `last_*` the
    /// per-request block. Kept minimal and synthetic — never real log content.
    fn token_count_line(
        ts: &str,
        total: (u64, u64, u64, u64),
        last: (u64, u64, u64, u64),
    ) -> String {
        let block = |t: (u64, u64, u64, u64)| {
            format!(
                r#"{{"input_tokens":{},"cached_input_tokens":{},"output_tokens":{},"reasoning_output_tokens":0,"total_tokens":{}}}"#,
                t.0, t.1, t.2, t.3
            )
        };
        format!(
            r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{},"last_token_usage":{}}}}}}}"#,
            block(total),
            block(last)
        )
    }

    #[test]
    fn happy_path_keeps_one_sample_per_distinct_total() {
        // Two turns. Turn 1: fresh 9355 input (12811-3456 cached), 341 out.
        // Turn 2 cumulative total moves 13152 -> 15489; last reports the turn.
        let l1 = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (12811, 3456, 341, 13152),
            (12811, 3456, 341, 13152),
        );
        let l2 = token_count_line(
            "2026-03-10T10:48:26.248Z",
            (24702, 6912, 483, 15489),
            (11891, 3456, 142, 2337),
        );
        let path = temp_rollout(&format!("{l1}\n{l2}\n"));
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();

        assert_eq!(samples.len(), 2);
        // fresh input = input - cached; cache_read = cached; no cache creation.
        assert_eq!(samples[0].input_tokens, 12811 - 3456);
        assert_eq!(samples[0].cache_read_tokens, 3456);
        assert_eq!(samples[0].cache_creation_tokens, 0);
        assert_eq!(samples[0].output_tokens, 341);
        assert_eq!(samples[1].input_tokens, 11891 - 3456);
        assert_eq!(samples[1].output_tokens, 142);
        assert_eq!(samples[0].ts, "2026-03-10T10:47:41.050Z");
    }

    #[test]
    fn duplicate_totals_are_deduped() {
        // The same totals emitted twice (Codex's ~2x duplication): one sample.
        let l = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        let path = temp_rollout(&format!("{l}\n{l}\n"));
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples.len(), 1);
    }

    #[test]
    fn malformed_line_is_skipped_and_later_lines_still_parse() {
        let good = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        let path = temp_rollout(&format!("{{not json\n{good}\n"));
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].output_tokens, 5);
    }

    #[test]
    fn non_utf8_bytes_do_not_sink_the_file() {
        // A non-UTF8 byte on one line must decode lossily and skip only that
        // line, not turn the whole best-effort read into a hard error.
        let good = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\xff not utf8}\n");
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        let path =
            std::env::temp_dir().join(format!("tasqx-codex-utf8-{}.jsonl", std::process::id()));
        std::fs::write(&path, &bytes).expect("write temp rollout");
        let samples = samples_from_file(&path).expect("non-utf8 must not error");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].output_tokens, 5);
    }

    #[test]
    fn irrelevant_file_returns_empty() {
        // Lines with no token_count events at all.
        let path = temp_rollout(
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n\
             {\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n",
        );
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert!(samples.is_empty());
    }

    #[test]
    fn empty_file_returns_empty() {
        let path = temp_rollout("");
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert!(samples.is_empty());
    }

    #[test]
    fn missing_file_is_an_error_naming_the_path() {
        let path = std::env::temp_dir().join("tasqx-codex-does-not-exist-xyz.jsonl");
        let err = samples_from_file(&path).expect_err("open failure must error");
        assert!(
            err.message.contains("tasqx-codex-does-not-exist-xyz"),
            "message must name the path: {}",
            err.message
        );
    }

    #[test]
    fn event_without_usable_timestamp_is_skipped() {
        // First event has a garbage timestamp (skipped, does not advance state);
        // second is well-formed and its total is genuinely new, so it survives.
        let bad_ts = r#"{"timestamp":"not-a-date","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":5,"total_tokens":105},"last_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":5,"total_tokens":105}}}}"#;
        let good = token_count_line(
            "2026-03-10T10:48:00.000Z",
            (250, 0, 12, 262),
            (250, 0, 12, 262),
        );
        let path = temp_rollout(&format!("{bad_ts}\n{good}\n"));
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples.len(), 1);
        // The surviving sample is the second one, with its full totals intact
        // (the dropped first event did not become its baseline).
        assert_eq!(samples[0].input_tokens, 250);
        assert_eq!(samples[0].output_tokens, 12);
    }

    #[test]
    fn timestamp_is_kept_as_rfc3339() {
        let l = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        let path = temp_rollout(&format!("{l}\n"));
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples[0].ts, "2026-03-10T10:47:41.050Z");
        assert!(crate::util::parse_ts(&samples[0].ts).is_some());
    }

    #[test]
    fn model_from_turn_context_is_stamped_on_following_samples() {
        let ctx = r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#;
        let l = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        // A token_count BEFORE any turn_context has no model; one AFTER does.
        let before = token_count_line("2026-03-10T10:47:40.000Z", (50, 0, 2, 52), (50, 0, 2, 52));
        let path = temp_rollout(&format!("{before}\n{ctx}\n{l}\n"));
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].model, None);
        assert_eq!(samples[1].model.as_deref(), Some("gpt-5.4"));
    }

    #[test]
    fn drift_tripwire_emits_the_authoritative_delta() {
        // last_token_usage disagrees with the cumulative delta. The totals win:
        // first event delta = 100/10/5, second event cumulative moves by
        // 200/10/8 but last claims a wrong 999/0/999. We must emit the delta.
        let l1 = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        let l2 = token_count_line(
            "2026-03-10T10:48:00.000Z",
            (300, 20, 13, 313),
            (999, 0, 999, 1998),
        );
        let path = temp_rollout(&format!("{l1}\n{l2}\n"));
        let samples = samples_from_file(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert_eq!(samples.len(), 2);
        // Second sample comes from the delta (300-100, 20-10, 13-5), mapped:
        // fresh input = 200-10 = 190, cache_read = 10, output = 8.
        assert_eq!(samples[1].input_tokens, 190);
        assert_eq!(samples[1].cache_read_tokens, 10);
        assert_eq!(samples[1].output_tokens, 8);
        assert_eq!(samples[1].cache_creation_tokens, 0);
    }

    #[test]
    fn session_meta_reads_id_cwd_and_version() {
        let meta = r#"{"timestamp":"2026-03-10T10:47:33.148Z","type":"session_meta","payload":{"id":"abc-123","cwd":"/home/u/proj","cli_version":"0.112.0","originator":"codex_cli_rs"}}"#;
        let l = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        let path = temp_rollout(&format!("{meta}\n{l}\n"));
        let got = session_meta(&path).expect("parse").expect("has meta");
        std::fs::remove_file(&path).ok();
        assert_eq!(got.id.as_deref(), Some("abc-123"));
        assert_eq!(got.cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(got.cli_version.as_deref(), Some("0.112.0"));
    }

    #[test]
    fn session_meta_absent_returns_none() {
        let l = token_count_line(
            "2026-03-10T10:47:41.050Z",
            (100, 10, 5, 105),
            (100, 10, 5, 105),
        );
        let path = temp_rollout(&format!("{l}\n"));
        let got = session_meta(&path).expect("parse");
        std::fs::remove_file(&path).ok();
        assert_eq!(got, None);
    }

    #[test]
    fn default_roots_honors_codex_home_override() {
        // Set CODEX_HOME to a known dir and assert both roots hang off it.
        // SAFETY: single-threaded within this test; we restore afterward.
        let prev = std::env::var_os("CODEX_HOME");
        unsafe { std::env::set_var("CODEX_HOME", "/custom/codex") };
        let roots = default_roots();
        match prev {
            Some(v) => unsafe { std::env::set_var("CODEX_HOME", v) },
            None => unsafe { std::env::remove_var("CODEX_HOME") },
        }
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/custom/codex/sessions"),
                PathBuf::from("/custom/codex/archived_sessions"),
            ]
        );
    }
}
