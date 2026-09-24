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
//! dedupe on (message id, `requestId`) keeping the LAST occurrence so a
//! streamed response counts once; a line without a `requestId` keys on the
//! message id alone. ccusage additionally treats a sidechain replay — same
//! message id, a NEW `requestId`, but the same timestamp — as the same
//! duplicate, so that pairing is folded in as a fallback key.
//!
//! A session also spans more than one file: a subagent's own turns land in
//! `<session>/subagents/*.jsonl` beside the main `<session>.jsonl`
//! ([`session_files`]), and [`contains_task_call`] is how a session file is
//! matched to the task whose start or done call it recorded (D188).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ApiError;
use crate::tokens::{env_path, home_dir, UsageSample};

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

/// A session's own file plus every subagent transcript it spawned:
/// `<dir>/<session-id>/subagents/*.jsonl`, sorted for determinism. Each is
/// counted once — the caller must not also add these files as separate
/// top-level candidates from a directory walk (D188).
pub fn session_files(main: &Path) -> Vec<PathBuf> {
    let mut files = vec![main.to_path_buf()];
    if let (Some(dir), Some(stem)) = (main.parent(), main.file_stem().and_then(|s| s.to_str())) {
        let subagents_dir = dir.join(stem).join("subagents");
        if let Ok(entries) = std::fs::read_dir(&subagents_dir) {
            let mut subs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                .collect();
            subs.sort();
            files.extend(subs);
        }
    }
    files
}

/// How close a start or done call's own line `timestamp` must sit to the
/// window boundary it names, in the direction it names (a start call near
/// `window_start`, a done call near `window_end`). Real hook latency is
/// milliseconds, so this is generous slack for clock skew and queuing, not a
/// tolerance anyone should need to rely on — it exists so the check has SOME
/// bound rather than none. Without one, `contains_task_call` matched a ref
/// anywhere in a transcript's whole history: this machine's own Claude Code
/// history holds a REAL `tasqx_complete_task` call for ref "1" from a wholly
/// unrelated session, so a `TASQX_DB=/scratch tasqx --no-daemon done 1` run
/// today would otherwise have been "located" as that real task's transcript.
const LOCATE_SLACK_SECS: i64 = 10 * 60;

/// Whether this transcript file's own records hold the tool call that started
/// or completed `task_ref` inside its task's window — the evidence D188 uses
/// to locate the session that measured a task, rather than guessing from
/// time-window overlap alone. A match is either an MCP `tool_use` named
/// `*tasqx_start_timer` / `*tasqx_complete_task` whose `input.ref` equals
/// `short_id` or `uuid`, or a `Bash` `tool_use` running `tasqx start <id>` /
/// `tasqx done <id>` against either spelling — in both cases only when the
/// call's own line `timestamp` falls within `LOCATE_SLACK_SECS` of the
/// window edge it names (start near `window_start`, done near `window_end`).
/// A Bash command that sets `TASQX_DB=` before invoking `tasqx` targets a
/// scratch store (`CONTRIBUTING.md`'s dev-build rule) and never matches: a
/// developer's own `tasqx start`/`done` against a throwaway database must
/// never be mistaken for the real daemon completing this task. Best-effort
/// like every other read here: an unreadable file, or a window bound that
/// does not parse, answers `false`, never an error.
pub fn contains_task_call(
    path: &Path,
    short_id: &str,
    uuid: &str,
    window_start: &str,
    window_end: &str,
) -> bool {
    let (Ok(start), Ok(end)) = (
        window_start.parse::<Timestamp>(),
        window_end.parse::<Timestamp>(),
    ) else {
        return false;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let content = String::from_utf8_lossy(&bytes);
    content
        .lines()
        .any(|line| line_targets_task(line, short_id, uuid, start, end))
}

fn line_targets_task(
    line: &str,
    short_id: &str,
    uuid: &str,
    start: Timestamp,
    end: Timestamp,
) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return false;
    };
    let Some(ts) = value
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|s| s.parse::<Timestamp>().ok())
    else {
        return false;
    };
    let Some(blocks) = value.pointer("/message/content").and_then(|c| c.as_array()) else {
        return false;
    };
    blocks
        .iter()
        .any(|block| block_targets_task(block, short_id, uuid, ts, start, end))
}

fn block_targets_task(
    block: &Value,
    short_id: &str,
    uuid: &str,
    ts: Timestamp,
    window_start: Timestamp,
    window_end: Timestamp,
) -> bool {
    if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
        return false;
    }
    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let input = block.get("input");
    if name.ends_with("tasqx_start_timer") || name.ends_with("tasqx_complete_task") {
        let Some(r) = input.and_then(|i| i.get("ref")) else {
            return false;
        };
        let matches_ref = r.as_str() == Some(short_id)
            || r.as_str() == Some(uuid)
            || r.as_i64().map(|n| n.to_string()).as_deref() == Some(short_id);
        if !matches_ref {
            return false;
        }
        let anchor = if name.ends_with("tasqx_start_timer") {
            window_start
        } else {
            window_end
        };
        return near(ts, anchor);
    }
    if name == "Bash" {
        let command = input
            .and_then(|i| i.get("command"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if bash_command_targets_a_scratch_store(command) {
            return false;
        }
        if bash_command_names("start", command, short_id)
            || bash_command_names("start", command, uuid)
        {
            return near(ts, window_start);
        }
        if bash_command_names("done", command, short_id)
            || bash_command_names("done", command, uuid)
        {
            return near(ts, window_end);
        }
        return false;
    }
    false
}

/// Whether `ts` sits within [`LOCATE_SLACK_SECS`] of `anchor`, in either
/// direction — `Timestamp::duration_since` is signed, so the ordering of the
/// two arguments does not matter here.
fn near(ts: Timestamp, anchor: Timestamp) -> bool {
    ts.duration_since(anchor).as_secs().abs() <= LOCATE_SLACK_SECS
}

/// Whether `command` runs `tasqx <verb> <task_ref>` as whole words — a
/// substring match alone would let ref `4` match a command naming task `42`.
fn bash_command_names(verb: &str, command: &str, task_ref: &str) -> bool {
    if task_ref.is_empty() {
        return false;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    words.windows(3).any(|w| {
        w[0].ends_with("tasqx")
            && w[1] == verb
            && w[2].trim_matches(|c: char| !c.is_alphanumeric() && c != '-') == task_ref
    })
}

/// Whether `command` sets `TASQX_DB=` as a word anywhere before the `tasqx`
/// word it invokes — the dev-build shape `CONTRIBUTING.md` requires
/// (`TASQX_DB=<scratch>/tasks.db tasqx --no-daemon done 42`). Such a run
/// targets a scratch store, never the real one this task's own history
/// lives in, so it must never be read as evidence of this task's real
/// completion. No `tasqx` word at all answers `false`; the caller has
/// already established the command names one before checking this.
fn bash_command_targets_a_scratch_store(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let Some(tasqx_idx) = words.iter().position(|w| w.ends_with("tasqx")) else {
        return false;
    };
    words[..tasqx_idx]
        .iter()
        .any(|w| w.starts_with("TASQX_DB="))
}

/// A transcript line. Unknown fields are ignored on purpose (version
/// tolerance), but a line is dropped when it is not valid JSON, when it carries
/// no `message.usage` (where user and tool lines land), or when its timestamp is
/// missing or not something `jiff` can parse — a reading with no instant cannot
/// be placed in any attribution window, so counting it would put tokens in a
/// period nobody can name.
#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
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
    // Primary dedupe key: (message id, requestId). A line with no requestId
    // keys on the message id alone (both share the constant `None` second
    // component), which is today's plain per-message dedupe. The value at a
    // slot is overwritten by later occurrences so the LAST wins.
    let mut slot_by_key: HashMap<(String, Option<String>), usize> = HashMap::new();
    // ccusage's fallback: a sidechain replay reuses one message id under a NEW
    // requestId but the SAME timestamp, and that pair is also one duplicate
    // even when the primary key above misses.
    let mut slot_by_id_ts: HashMap<(String, String), usize> = HashMap::new();

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
            // The message id doubles as the sample's cross-tick identity: the
            // stamp of a streamed message can move between reads, the id cannot.
            id: message.id.clone(),
            ts: ts.clone(),
            model: message.model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_creation_tokens: usage.cache_creation_input_tokens,
        };

        match message.id {
            Some(id) => {
                let primary = (id.clone(), parsed.request_id.clone());
                let fallback = (id, ts);
                let existing = slot_by_key
                    .get(&primary)
                    .or_else(|| slot_by_id_ts.get(&fallback))
                    .copied();
                let idx = match existing {
                    Some(idx) => {
                        samples[idx] = sample;
                        idx
                    }
                    None => {
                        let idx = samples.len();
                        samples.push(sample);
                        idx
                    }
                };
                // Register the slot under both keys so a later line matching
                // EITHER the requestId or the timestamp finds it.
                slot_by_key.insert(primary, idx);
                slot_by_id_ts.insert(fallback, idx);
            }
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

    /// A synthetic assistant line carrying an explicit `requestId`, the field
    /// [`assistant_line`] never sets.
    fn assistant_line_with_request(
        ts: &str,
        id: &str,
        request_id: &str,
        input: u64,
        output: u64,
    ) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","requestId":"{request_id}","message":{{"id":"{id}","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
        )
    }

    #[test]
    fn same_message_id_a_new_request_id_is_a_second_sample() {
        // Two genuinely distinct responses can share one message id under a
        // different requestId and timestamp — that must NOT collapse to one.
        let content = [
            assistant_line_with_request("2026-07-24T10:00:00Z", "m", "req-1", 1, 10),
            assistant_line_with_request("2026-07-24T10:00:05Z", "m", "req-2", 1, 20),
        ]
        .join("\n");
        let out = parse_samples(&content);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].output_tokens, 10);
        assert_eq!(out[1].output_tokens, 20);
    }

    #[test]
    fn a_replayed_message_same_id_new_request_id_same_timestamp_counts_once() {
        // ccusage's sidechain-replay fallback: message id AND timestamp match
        // even though requestId does not, so this is still one sample.
        let content = [
            assistant_line_with_request("2026-07-24T10:00:00Z", "m", "req-1", 1, 10),
            assistant_line_with_request("2026-07-24T10:00:00Z", "m", "req-2", 1, 999),
        ]
        .join("\n");
        let out = parse_samples(&content);
        assert_eq!(out.len(), 1, "a replay must dedupe: {out:?}");
        assert_eq!(out[0].output_tokens, 999, "last occurrence wins");
    }

    #[test]
    fn contains_task_call_matches_mcp_start_and_complete_by_ref() {
        let dir = std::env::temp_dir().join(format!("tasqx-cc-call-{}", crate::clock::uuid_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"assistant","timestamp":"2026-07-24T10:00:00Z","message":{"id":"m","content":[{"type":"tool_use","name":"mcp__tasqx__tasqx_start_timer","input":{"ref":42}}]}}"#,
        )
        .unwrap();

        // A start call is checked against `window_start`.
        let (start, end) = ("2026-07-24T10:00:05Z", "2026-07-24T11:00:00Z");
        assert!(contains_task_call(&path, "42", "uuid-x", start, end));
        assert!(!contains_task_call(&path, "7", "uuid-x", start, end));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contains_task_call_matches_bash_start_and_done_as_whole_words() {
        let dir = std::env::temp_dir().join(format!("tasqx-cc-bash-{}", crate::clock::uuid_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"assistant","timestamp":"2026-07-24T10:00:00Z","message":{"id":"m","content":[{"type":"tool_use","name":"Bash","input":{"command":"tasqx done 42"}}]}}"#,
        )
        .unwrap();

        // A done call is checked against `window_end`.
        let (start, end) = ("2026-07-24T09:00:00Z", "2026-07-24T10:00:05Z");
        assert!(contains_task_call(&path, "42", "uuid-x", start, end));
        // "4" must not match a command naming task 42.
        assert!(!contains_task_call(&path, "4", "uuid-x", start, end));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contains_task_call_refuses_a_ref_match_far_outside_the_window() {
        // The false-match this whole check exists to close: a real
        // `tasqx_complete_task` call for this ref, but hours away from the
        // task's own window, must not be read as evidence for THIS task.
        let dir = std::env::temp_dir().join(format!("tasqx-cc-far-{}", crate::clock::uuid_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"assistant","timestamp":"2026-07-24T10:00:00Z","message":{"id":"m","content":[{"type":"tool_use","name":"mcp__tasqx__tasqx_complete_task","input":{"ref":1}}]}}"#,
        )
        .unwrap();

        assert!(!contains_task_call(
            &path,
            "1",
            "uuid-x",
            "2026-07-24T08:00:00Z",
            "2026-07-24T08:05:00Z",
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contains_task_call_refuses_a_bash_run_against_a_scratch_db() {
        let dir =
            std::env::temp_dir().join(format!("tasqx-cc-scratch-{}", crate::clock::uuid_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"assistant","timestamp":"2026-07-24T10:00:00Z","message":{"id":"m","content":[{"type":"tool_use","name":"Bash","input":{"command":"TASQX_DB=/tmp/x tasqx --no-daemon done 42"}}]}}"#,
        )
        .unwrap();

        let (start, end) = ("2026-07-24T09:00:00Z", "2026-07-24T10:00:05Z");
        assert!(!contains_task_call(&path, "42", "uuid-x", start, end));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_files_includes_subagent_transcripts_once_each() {
        let dir = std::env::temp_dir().join(format!("tasqx-cc-sess-{}", crate::clock::uuid_v7()));
        let subagents = dir.join("sess-1").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let main = dir.join("sess-1.jsonl");
        std::fs::write(&main, "").unwrap();
        std::fs::write(subagents.join("agent-1.jsonl"), "").unwrap();
        std::fs::write(subagents.join("agent-2.jsonl"), "").unwrap();
        std::fs::write(subagents.join("not-jsonl.txt"), "").unwrap();

        let mut files = session_files(&main);
        files.sort();
        let mut want = vec![
            main.clone(),
            subagents.join("agent-1.jsonl"),
            subagents.join("agent-2.jsonl"),
        ];
        want.sort();
        assert_eq!(files, want);

        let _ = std::fs::remove_dir_all(&dir);
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
        let path = std::env::temp_dir().join(format!("tasqx-cc-{}.jsonl", crate::clock::uuid_v7()));
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
            std::env::temp_dir().join(format!("tasqx-cc-utf8-{}.jsonl", crate::clock::uuid_v7()));
        std::fs::write(&path, &bytes).expect("write temp transcript");

        let out = samples_from_file(&path).expect("non-utf8 must not error");
        let _ = std::fs::remove_file(&path);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].input_tokens, 9);
    }

    #[test]
    fn samples_from_file_missing_path_is_an_error_naming_the_path() {
        let path = std::env::temp_dir().join(format!(
            "tasqx-cc-missing-{}.jsonl",
            crate::clock::uuid_v7()
        ));
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
