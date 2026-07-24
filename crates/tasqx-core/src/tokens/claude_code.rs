//! Claude Code transcript parser (backlog #14).
//!
//! Claude Code writes one JSON object per line to
//! `~/.claude/projects/<munged-cwd>/*.jsonl` (or `~/.config/claude/projects/`,
//! and `$CLAUDE_CONFIG_DIR/projects/` when that override is set). Assistant
//! messages carry `message.usage` with the four token counts and a top-level
//! RFC3339 `timestamp`; the model, when present, is in `message.model`.
//!
//! Log formats are undocumented internals (research rule: version tolerance is
//! the prime directive), so this parser ignores unknown fields, skips any
//! malformed or usage-less line rather than failing the whole file, and never
//! panics on input. Only an io error opening the file is a hard error.
//!
//! Streaming rewrites the same assistant `message.id` several times as a
//! response is produced, re-emitting the cumulative usage each time. Verified
//! on a real local transcript on 2026-07-24: a 9-assistant-line file held only
//! 4 distinct message ids, and every duplicate carried identical usage. We
//! dedupe by message id keeping the LAST occurrence so a streamed response
//! counts once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;

use crate::error::ApiError;
use crate::tokens::UsageSample;

/// Read and parse one transcript file. An io error opening the file is a hard
/// error (it names the path); every per-line problem is skipped, so a file with
/// no usable samples returns `Ok(vec![])`.
pub fn samples_from_file(path: &Path) -> Result<Vec<UsageSample>, ApiError> {
    // Read bytes and decode lossily: only *opening* the file is a hard error, so
    // a stray non-UTF8 byte must not sink an otherwise-parseable file. The bad
    // byte becomes U+FFFD and only that line fails to parse; the rest survive.
    let bytes = std::fs::read(path).map_err(|e| {
        ApiError::internal(format!(
            "failed to read Claude Code transcript {}: {e}",
            path.display()
        ))
    })?;
    let content = String::from_utf8_lossy(&bytes);
    Ok(parse_samples(&content))
}

/// Whether `session_id` provably identifies this transcript — the correlation
/// the attribution confidence rule requires to earn HIGH. Claude Code names each
/// transcript file `<session-id>.jsonl` and stamps that same id on every line's
/// `sessionId`, so matching either the file stem or an embedded id confirms the
/// completion belongs to this file. An empty id never matches; a supplied id
/// that matches neither is a mismatch (stale/wrong hook argument) and must not
/// be trusted as correlated.
pub fn session_matches(path: &Path, session_id: &str) -> bool {
    if session_id.is_empty() {
        return false;
    }
    if path.file_stem().and_then(|s| s.to_str()) == Some(session_id) {
        return true;
    }
    // The file may have been renamed; fall back to the id stamped on each line.
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let content = String::from_utf8_lossy(&bytes);
    content.lines().any(|line| {
        serde_json::from_str::<Line>(line)
            .ok()
            .and_then(|l| l.session_id)
            .as_deref()
            == Some(session_id)
    })
}

/// The directories Claude Code writes session transcripts into, whether or not
/// they currently exist. `$CLAUDE_CONFIG_DIR` (its config-dir override) comes
/// first when set; the two standard locations always follow.
pub fn default_roots() -> Vec<PathBuf> {
    roots_from(
        env_path("CLAUDE_CONFIG_DIR").as_deref(),
        home_dir().as_deref(),
    )
}

/// Best-effort home directory without a dependency: `$HOME` on Unix,
/// `%USERPROFILE%` on Windows (matching the sibling parsers).
fn home_dir() -> Option<PathBuf> {
    env_path(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
}

/// A transcript line. Unknown fields are ignored on purpose (version
/// tolerance); only user/tool lines lacking a `message.usage` are dropped.
#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

/// A missing individual count is 0; a fully absent `usage` block (`None` above)
/// yields no sample at all.
#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

fn parse_samples(content: &str) -> Vec<UsageSample> {
    let mut samples: Vec<UsageSample> = Vec::new();
    // First-seen position per message id; the value at that slot is overwritten
    // by later occurrences so the LAST wins while chronological order holds.
    let mut slot_by_id: HashMap<String, usize> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A malformed line is skipped, never an error for the whole file.
        let Ok(parsed) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        let Some(message) = parsed.message else {
            continue;
        };
        // Fully absent usage block => no sample (user/tool lines land here).
        let Some(usage) = message.usage else {
            continue;
        };
        // A sample with usage but no usable timestamp is skipped. `jiff` both
        // validates the value is a real instant and normalizes it to RFC3339.
        let Some(ts) = parsed
            .timestamp
            .and_then(|raw| raw.parse::<Timestamp>().ok())
            .map(|t| t.to_string())
        else {
            continue;
        };

        let sample = UsageSample {
            ts,
            model: message.model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_creation_tokens: usage.cache_creation_input_tokens,
        };

        match message.id {
            Some(id) => match slot_by_id.get(&id) {
                Some(&idx) => samples[idx] = sample,
                None => {
                    slot_by_id.insert(id, samples.len());
                    samples.push(sample);
                }
            },
            // No id to dedupe on: keep every such sample as its own reading.
            None => samples.push(sample),
        }
    }

    samples
}

/// Pure core of [`default_roots`], taking the two env values so it is testable
/// without mutating process env.
fn roots_from(config_dir: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = config_dir {
        roots.push(dir.join("projects"));
    }
    if let Some(home) = home {
        roots.push(home.join(".claude").join("projects"));
        roots.push(home.join(".config").join("claude").join("projects"));
    }
    let mut seen = HashSet::new();
    roots.retain(|p| seen.insert(p.clone()));
    roots
}

/// A non-empty environment path, or `None` (an empty variable means "unset").
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal synthetic assistant line. Hand-written to avoid ever committing
    /// real transcript content (session logs are private conversation data).
    fn assistant_line(ts: &str, id: &str, model: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
        )
    }

    #[test]
    fn happy_path_yields_one_sample_per_message() {
        let content = [
            assistant_line(
                "2026-07-24T10:00:00.000Z",
                "msg_a",
                "claude-opus-4-7",
                10,
                20,
            ),
            assistant_line(
                "2026-07-24T10:01:00.000Z",
                "msg_b",
                "claude-opus-4-7",
                5,
                40,
            ),
        ]
        .join("\n");

        let out = parse_samples(&content);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].input_tokens, 10);
        assert_eq!(out[0].output_tokens, 20);
        assert_eq!(out[0].model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(out[1].input_tokens, 5);
        assert_eq!(out[1].output_tokens, 40);
    }

    #[test]
    fn cache_fields_map_from_input_named_keys() {
        let content = r#"{"type":"assistant","timestamp":"2026-07-24T10:00:00Z","message":{"id":"m","model":"claude-opus-4-7","usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":31651,"cache_creation_input_tokens":5196}}}"#;
        let out = parse_samples(content);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].cache_read_tokens, 31651);
        assert_eq!(out[0].cache_creation_tokens, 5196);
    }

    #[test]
    fn malformed_line_is_skipped_and_later_lines_still_parse() {
        let content = [
            assistant_line("2026-07-24T10:00:00Z", "msg_a", "claude-opus-4-7", 10, 20),
            "{ this is not valid json".to_string(),
            assistant_line("2026-07-24T10:02:00Z", "msg_c", "claude-opus-4-7", 7, 8),
        ]
        .join("\n");

        let out = parse_samples(&content);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].input_tokens, 10);
        assert_eq!(out[1].input_tokens, 7);
    }

    #[test]
    fn user_and_tool_lines_without_usage_are_skipped() {
        let content = [
            r#"{"type":"user","timestamp":"2026-07-24T10:00:00Z","message":{"role":"user","content":"hi"}}"#.to_string(),
            r#"{"type":"assistant","timestamp":"2026-07-24T10:00:01Z","message":{"id":"m","model":"claude-opus-4-7"}}"#.to_string(),
            assistant_line("2026-07-24T10:00:02Z", "msg_a", "claude-opus-4-7", 3, 4),
        ]
        .join("\n");

        let out = parse_samples(&content);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].input_tokens, 3);
    }

    #[test]
    fn empty_and_irrelevant_content_yields_no_samples() {
        assert!(parse_samples("").is_empty());
        assert!(parse_samples("\n   \n").is_empty());
        assert!(parse_samples(r#"{"type":"summary","summary":"nothing to see"}"#).is_empty());
    }

    #[test]
    fn missing_individual_count_fields_default_to_zero() {
        // Only output_tokens present; the other three counts default to 0.
        let content = r#"{"type":"assistant","timestamp":"2026-07-24T10:00:00Z","message":{"id":"m","usage":{"output_tokens":42}}}"#;
        let out = parse_samples(content);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].input_tokens, 0);
        assert_eq!(out[0].output_tokens, 42);
        assert_eq!(out[0].cache_read_tokens, 0);
        assert_eq!(out[0].cache_creation_tokens, 0);
        assert_eq!(out[0].model, None);
    }

    #[test]
    fn line_with_usage_but_no_usable_timestamp_is_skipped() {
        let missing = r#"{"type":"assistant","message":{"id":"m","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        let empty = r#"{"type":"assistant","timestamp":"","message":{"id":"n","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        let garbage = r#"{"type":"assistant","timestamp":"not-a-date","message":{"id":"o","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        assert!(parse_samples(missing).is_empty());
        assert!(parse_samples(empty).is_empty());
        assert!(parse_samples(garbage).is_empty());
    }

    #[test]
    fn timestamp_is_normalized_to_rfc3339() {
        // A zero-offset numeric offset normalizes to the canonical `Z` form.
        let content = r#"{"type":"assistant","timestamp":"2026-07-24T12:00:00+00:00","message":{"id":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let out = parse_samples(content);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ts, "2026-07-24T12:00:00Z");
    }

    #[test]
    fn duplicate_message_ids_dedupe_keeping_last_occurrence() {
        // Streaming re-emits one id three times with growing cumulative usage;
        // the LAST occurrence is the final total and must be the one kept.
        let content = [
            assistant_line("2026-07-24T10:00:00Z", "stream", "claude-opus-4-7", 1, 10),
            assistant_line("2026-07-24T10:00:01Z", "stream", "claude-opus-4-7", 1, 55),
            assistant_line("2026-07-24T10:00:02Z", "stream", "claude-opus-4-7", 1, 120),
            assistant_line("2026-07-24T10:00:03Z", "other", "claude-opus-4-7", 2, 2),
        ]
        .join("\n");

        let out = parse_samples(&content);
        assert_eq!(out.len(), 2);
        // Slot 0 keeps first-seen chronological position but last-seen values.
        assert_eq!(out[0].output_tokens, 120);
        assert_eq!(out[0].ts, "2026-07-24T10:00:02Z");
        assert_eq!(out[1].output_tokens, 2);
    }

    #[test]
    fn samples_without_id_are_all_kept() {
        let content = [
            r#"{"type":"assistant","timestamp":"2026-07-24T10:00:00Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":1,"output_tokens":1}}}"#.to_string(),
            r#"{"type":"assistant","timestamp":"2026-07-24T10:00:01Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":2,"output_tokens":2}}}"#.to_string(),
        ]
        .join("\n");
        let out = parse_samples(&content);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn samples_from_file_reads_a_real_file() {
        let path = std::env::temp_dir().join(format!("tasqx-cc-{}.jsonl", uuid::Uuid::now_v7()));
        let content = assistant_line("2026-07-24T10:00:00Z", "msg_a", "claude-opus-4-7", 9, 9);
        std::fs::write(&path, content).expect("write temp transcript");

        let out = samples_from_file(&path).expect("parse temp transcript");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].input_tokens, 9);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn samples_from_file_tolerates_non_utf8_bytes() {
        // A non-UTF8 byte on one line must decode lossily and skip only that
        // line, not turn the whole best-effort read into a hard error.
        let good = assistant_line("2026-07-24T10:00:00Z", "msg_a", "claude-opus-4-7", 9, 9);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\xff not utf8}\n");
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        let path =
            std::env::temp_dir().join(format!("tasqx-cc-utf8-{}.jsonl", uuid::Uuid::now_v7()));
        std::fs::write(&path, &bytes).expect("write temp transcript");

        let out = samples_from_file(&path).expect("non-utf8 must not error");
        let _ = std::fs::remove_file(&path);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].input_tokens, 9);
    }

    #[test]
    fn samples_from_file_missing_path_is_an_error_naming_the_path() {
        let path =
            std::env::temp_dir().join(format!("tasqx-cc-missing-{}.jsonl", uuid::Uuid::now_v7()));
        let err = samples_from_file(&path).expect_err("missing file must error");
        assert!(
            err.message.contains(&path.display().to_string()),
            "error must name the path: {}",
            err.message
        );
    }

    #[test]
    fn default_roots_include_both_standard_locations() {
        let home = Path::new("/home/someone");
        let roots = roots_from(None, Some(home));
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/home/someone/.claude/projects"),
                PathBuf::from("/home/someone/.config/claude/projects"),
            ]
        );
    }

    #[test]
    fn config_dir_override_comes_first_and_roots_dedupe() {
        let cfg = Path::new("/custom/cfg");
        let home = Path::new("/home/someone");
        let roots = roots_from(Some(cfg), Some(home));
        assert_eq!(roots[0], PathBuf::from("/custom/cfg/projects"));
        assert_eq!(roots.len(), 3);

        // When the override points at ~/.claude the duplicate collapses.
        let roots = roots_from(Some(Path::new("/home/someone/.claude")), Some(home));
        assert_eq!(roots.len(), 2);
    }
}
