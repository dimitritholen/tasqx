//! Async token attribution (docs/research/token-accounting.md, backlog #17).
//!
//! When a task is completed, the daemon reconstructs the tokens spent during the
//! task's time window by parsing the AI tool's own local transcript, and stores
//! them as a *measured* (`source=log-parse`) `token_usage` row — asynchronously,
//! after `task.done` commits, so the completion itself never blocks on a
//! multi-second JSONL parse and never touches the client's `expected_rev`.
//!
//! This module is the pure/testable half: it turns a task's correlation metadata
//! plus a window into a [`AttributionResult`], and knows how to select a per-tool
//! parser ([`parser_for`]), which files to read, and how confident to be. The
//! daemon (`daemon::attribution_tick`) owns the thread, the clock, the engine
//! lock discipline, and the retry policy; the idempotent *write* lives in
//! `engine::tokens` ([`crate::engine::Engine::token_attribute`]). The event log
//! is the dedupe record, exactly like reminders: a `tokens.attributed` event
//! means "this task is done being attributed", so unknown-client and zero-sample
//! tasks terminate (they still get the marker) and catch-up after downtime is
//! free.
//!
//! ## Confidence rule (stored on every measurement)
//!  * [`crate::tokens::CONFIDENCE_HIGH`] — an explicit `transcript_path` was
//!    parsed *and* a session id correlated the completion to that transcript.
//!  * [`crate::tokens::CONFIDENCE_MEDIUM`] — an explicit `transcript_path` was
//!    parsed, but no session id was supplied.
//!  * [`crate::tokens::CONFIDENCE_LOW`] — no path was supplied, so the transcript
//!    was *discovered* by scanning the tool's default roots (best-effort; it may
//!    find nothing, which is a legitimate zero-sample result).
//!
//! ## Discovery is best-effort
//! Without a `transcript_path` there is no reliable anchor from a task to a
//! specific session file. We scan the tool's default roots, bounded, and for
//! Codex narrow to the file whose `session_meta.id` matches the correlation
//! `session_id` when one is known. No task `cwd` is captured today, so cwd-based
//! matching is not available; discovery therefore stays low-confidence and may
//! legitimately attribute nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::{json, Value};

use crate::engine::Engine;
use crate::error::ApiError;
use crate::tokens::{
    self, codex, TokenTotals, UsageSample, CONFIDENCE_HIGH, CONFIDENCE_LOW, CONFIDENCE_MEDIUM,
    SOURCE_LOG_PARSE, SOURCE_OTEL,
};

/// Upper bound on transcript files inspected during discovery (no
/// `transcript_path`). Discovery is best-effort and low-confidence; this keeps a
/// pathological session directory from turning one tick into a long scan while
/// the engine lock is *not* held.
const MAX_DISCOVERY_FILES: usize = 256;

/// How deep the discovery walk recurses into a tool's roots. Real layouts are
/// shallow (`<root>/<project>/<file>.jsonl`, `<root>/YYYY/MM/DD/<file>.jsonl`);
/// a bound stops a symlink loop or a surprise deep tree from wedging a tick.
const MAX_DISCOVERY_DEPTH: usize = 6;

/// How long after completion an explicit-but-absent `transcript_path` keeps
/// being retried before the task terminates with an empty marker. Transcripts
/// are flushed asynchronously and lag the completion hook, so a brief retry is
/// correct — but a file that has not appeared a full day later is never coming
/// (deleted, rotated, or a wrong path), and retrying it forever forces a full
/// pending-set rebuild every tick for the life of the daemon.
const TRANSCRIPT_GIVE_UP_SECS: i64 = 24 * 60 * 60;

/// A per-tool transcript parser. Internal enum wrapping the free functions each
/// `crate::tokens::<tool>` module exposes, so the attribution engine has one
/// uniform seam instead of a `match` at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parser {
    ClaudeCode,
    Codex,
    Gemini,
    Copilot,
}

/// Pick the parser for a client/tool string by lowercased substring, so
/// `"claude-code"`, `"Claude Code"` and `"claude"` all resolve. Returns `None`
/// for a tool tasqx has no parser for (e.g. Cursor), which the caller turns into
/// a terminating zero-sample marker rather than an error.
pub fn parser_for(client: &str) -> Option<Parser> {
    let c = client.to_lowercase();
    if c.contains("claude") {
        Some(Parser::ClaudeCode)
    } else if c.contains("codex") {
        Some(Parser::Codex)
    } else if c.contains("gemini") {
        Some(Parser::Gemini)
    } else if c.contains("copilot") {
        Some(Parser::Copilot)
    } else {
        None
    }
}

impl Parser {
    /// Parse one transcript file into per-request usage samples.
    fn samples_from_file(self, path: &Path) -> Result<Vec<UsageSample>, ApiError> {
        match self {
            Parser::ClaudeCode => tokens::claude_code::samples_from_file(path),
            Parser::Codex => tokens::codex::samples_from_file(path),
            Parser::Gemini => tokens::gemini::samples_from_file(path),
            Parser::Copilot => tokens::copilot::samples_from_file(path),
        }
    }

    /// Whether `session_id` provably identifies `path` — the correlation the
    /// HIGH-confidence rule requires. Best-effort per tool: Claude Code and Codex
    /// stamp a session id into the transcript (filename / `session_meta`), so a
    /// supplied id can be confirmed or refuted; Gemini and Copilot expose no
    /// per-session anchor, so an explicit path with a session id can never be
    /// *proven* correlated and stays at MEDIUM rather than claiming HIGH.
    fn session_matches(self, path: &Path, session_id: &str) -> bool {
        match self {
            Parser::ClaudeCode => tokens::claude_code::session_matches(path, session_id),
            Parser::Codex => matches!(
                tokens::codex::session_meta(path),
                Ok(Some(meta)) if meta.id.as_deref() == Some(session_id)
            ),
            Parser::Gemini | Parser::Copilot => false,
        }
    }

    /// The directories this tool writes session transcripts into.
    fn default_roots(self) -> Vec<PathBuf> {
        match self {
            Parser::ClaudeCode => tokens::claude_code::default_roots(),
            Parser::Codex => tokens::codex::default_roots(),
            Parser::Gemini => tokens::gemini::default_roots(),
            Parser::Copilot => tokens::copilot::default_roots(),
        }
    }
}

/// Sum the usage samples whose timestamp falls inside `[start, done]` (inclusive
/// on both ends) into a [`TokenTotals`], returning the count of samples that
/// landed in the window.
///
/// Pure and tolerant: an unparseable sample timestamp is skipped (never
/// counted), and if either window bound fails to parse the window is empty — a
/// bad window must never silently attribute *everything*.
pub fn totals_in_window(samples: &[UsageSample], start: &str, done: &str) -> (TokenTotals, usize) {
    let (Ok(lo), Ok(hi)) = (start.parse::<Timestamp>(), done.parse::<Timestamp>()) else {
        return (TokenTotals::default(), 0);
    };
    // A window whose start is after its end has no interior; treat it as empty
    // rather than as "everything is out of range in a confusing way".
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };

    let mut totals = TokenTotals::default();
    let mut n = 0;
    for s in samples {
        let Ok(ts) = s.ts.parse::<Timestamp>() else {
            continue;
        };
        if ts >= lo && ts <= hi {
            totals.add_sample(s);
            n += 1;
        }
    }
    (totals, n)
}

/// The confidence to stamp on a measurement, per the module's confidence rule.
/// `session_correlated` means a supplied session id was *verified* against the
/// parsed transcript (not merely that some id was present); HIGH is earned only
/// when the explicit path was parsed AND that correlation actually held.
pub fn confidence_for(transcript_path_parsed: bool, session_correlated: bool) -> &'static str {
    match (transcript_path_parsed, session_correlated) {
        (true, true) => CONFIDENCE_HIGH,
        (true, false) => CONFIDENCE_MEDIUM,
        (false, _) => CONFIDENCE_LOW,
    }
}

/// One task awaiting attribution: everything the engine needs to reconstruct its
/// token window, built from the store (the `done` event's correlation plus the
/// `start`/`created` window) and carried out of the engine lock so the transcript
/// parse runs unlocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAttribution {
    pub task_id: String,
    pub short_id: i64,
    /// Window start (RFC3339): the earliest `start` event's `interval_started`,
    /// falling back to the task's `created` when it was completed without ever
    /// being timed.
    pub window_start: String,
    /// Window end (RFC3339): the completion instant from the `done` event.
    pub window_end: String,
    pub client: Option<String>,
    pub transcript_path: Option<String>,
    pub session_id: Option<String>,
    /// Buffered OTLP samples (#18) whose `session_id` matched this task's, read
    /// from the store during the pending-set build. When non-empty and in-window,
    /// they are preferred over log-parsing (source `otel`), so a task is measured
    /// from EITHER telemetry OR a transcript, never both.
    pub otel_samples: Vec<UsageSample>,
    /// The tool that emitted the buffered OTLP samples, used only to label the
    /// stored measurement when the completion carried no `client`.
    pub otel_tool: Option<String>,
}

/// The outcome of attributing one task: the four-way totals, how many samples
/// landed in the window, the tool name to store, and the confidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionResult {
    pub totals: TokenTotals,
    pub samples: usize,
    pub tool: String,
    /// Where these tokens came from: [`SOURCE_OTEL`] when buffered telemetry won,
    /// [`SOURCE_LOG_PARSE`] otherwise. Stored on the measurement so a report can
    /// tell the two trust stories apart forever.
    pub source: &'static str,
    pub confidence: &'static str,
    /// True when the window held real spend (total > 0), so a `token_usage` row
    /// should be written. When false only the terminating `tokens.attributed`
    /// marker is written.
    pub found: bool,
}

impl AttributionResult {
    /// A terminating "nothing to store" result (unknown client, or discovery
    /// found nothing). Still gets a `tokens.attributed` marker so the task never
    /// re-enters the pending set.
    fn empty(tool: String) -> Self {
        AttributionResult {
            totals: TokenTotals::default(),
            samples: 0,
            tool,
            source: SOURCE_LOG_PARSE,
            confidence: CONFIDENCE_LOW,
            found: false,
        }
    }
}

/// Compute the attribution for one pending task. Does file I/O and MUST run with
/// no engine lock held.
///
/// Returns `Err` only for a *transient* condition — an explicit `transcript_path`
/// that is not present yet (transcripts are written asynchronously and lag the
/// completion hook, research doc) or unreadable. The daemon treats that as a
/// retry, never a fatal error and never a stored marker. An unknown client or a
/// discovery scan that finds nothing is `Ok` with `found == false`: those
/// terminate. A transcript still absent [`TRANSCRIPT_GIVE_UP_SECS`] after
/// completion also terminates (`now` is used only for that cutoff), so one stuck
/// task cannot force a full pending rebuild every tick forever.
pub fn compute_attribution(
    pa: &PendingAttribution,
    now: Timestamp,
) -> Result<AttributionResult, ApiError> {
    let tool = pa.client.clone().unwrap_or_default();

    // Prefer buffered OTLP telemetry (#18) when it correlated to this task's
    // session and lands in the window: it is per-request, timestamped, and needs
    // no file I/O. Because we return here, a task measured from telemetry is
    // never ALSO log-parsed — one source per task, so no double-count. The buffer
    // was matched by `session_id` during the pending-set build, so a hit is a
    // verified correlation => HIGH confidence. This runs even for a client tasqx
    // has no transcript parser for (telemetry needs none).
    if !pa.otel_samples.is_empty() {
        let (totals, n) = totals_in_window(&pa.otel_samples, &pa.window_start, &pa.window_end);
        if totals.total() > 0 {
            let otel_tool = pa
                .client
                .clone()
                .or_else(|| pa.otel_tool.clone())
                .unwrap_or_default();
            return Ok(AttributionResult {
                totals,
                samples: n,
                tool: otel_tool,
                source: SOURCE_OTEL,
                confidence: CONFIDENCE_HIGH,
                found: true,
            });
        }
    }

    // No client, or a client tasqx has no parser for: terminate with a marker.
    let Some(parser) = pa.client.as_deref().and_then(parser_for) else {
        return Ok(AttributionResult::empty(tool));
    };

    let (samples, transcript_parsed, session_correlated) = match pa.transcript_path.as_deref() {
        Some(path) if !path.is_empty() => {
            let file = Path::new(path);
            if !file.exists() {
                // A transcript that has not been flushed yet is transient, not
                // "no data": retry on a later tick rather than writing a wrong
                // zero-sample marker that would suppress the real numbers — but
                // only until the completion is old enough that the file is
                // never coming, then terminate so the task leaves the queue.
                if transcript_gave_up(now, &pa.window_end) {
                    return Ok(AttributionResult::empty(tool));
                }
                return Err(ApiError::internal(format!(
                    "transcript not available yet: {path}"
                )));
            }
            let samples = parser.samples_from_file(file)?;
            // HIGH is earned only when the supplied session id is *verified*
            // against this transcript, not merely present: a stale or wrong id
            // must not masquerade as a high-trust correlation.
            let correlated = pa
                .session_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .is_some_and(|sid| parser.session_matches(file, sid));
            (samples, true, correlated)
        }
        _ => (discover_samples(parser, pa), false, false),
    };

    let (totals, n) = totals_in_window(&samples, &pa.window_start, &pa.window_end);
    let found = totals.total() > 0;
    Ok(AttributionResult {
        totals,
        samples: n,
        tool,
        source: SOURCE_LOG_PARSE,
        confidence: confidence_for(transcript_parsed, session_correlated),
        found,
    })
}

/// Whether an absent explicit transcript has been retried long enough to give
/// up: true once `now` is more than [`TRANSCRIPT_GIVE_UP_SECS`] past the
/// completion instant. An unparseable `window_end` never gives up (keeps the
/// old retry-forever behavior for that pathological case rather than discarding
/// a possibly-real completion).
fn transcript_gave_up(now: Timestamp, window_end: &str) -> bool {
    match window_end.parse::<Timestamp>() {
        Ok(end) => now.duration_since(end).as_secs() > TRANSCRIPT_GIVE_UP_SECS,
        Err(_) => false,
    }
}

/// Best-effort discovery when no `transcript_path` was supplied: scan the tool's
/// default roots, bounded, and aggregate every readable file's samples. For
/// Codex, when the session id is known, restrict to the rollout whose
/// `session_meta.id` matches — the one anchor discovery has.
fn discover_samples(parser: Parser, pa: &PendingAttribution) -> Vec<UsageSample> {
    let mut out = Vec::new();
    for file in discover_candidates(&parser.default_roots(), MAX_DISCOVERY_FILES) {
        if parser == Parser::Codex {
            if let Some(want) = pa.session_id.as_deref().filter(|s| !s.is_empty()) {
                match codex::session_meta(&file) {
                    Ok(Some(meta)) if meta.id.as_deref() == Some(want) => {}
                    // Known-but-different session, or unreadable: skip it.
                    _ => continue,
                }
            }
        }
        if let Ok(mut samples) = parser.samples_from_file(&file) {
            out.append(&mut samples);
        }
    }
    out
}

/// Collect candidate transcript files under `roots`, newest first, capped at
/// `max`. Newest-first because a just-completed task's transcript is almost
/// always among the most recently written files, so the cap rarely bites.
fn discover_candidates(roots: &[PathBuf], max: usize) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for root in roots {
        walk(root, 0, &mut found);
    }
    found.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    found.truncate(max);
    found.into_iter().map(|(_, p)| p).collect()
}

/// Recursive directory walk collecting `*.jsonl` files with their mtime. Bounded
/// in depth and total collection so a huge or looping tree cannot stall a tick.
fn walk(dir: &Path, depth: usize, out: &mut Vec<(std::time::SystemTime, PathBuf)>) {
    if depth > MAX_DISCOVERY_DEPTH || out.len() >= MAX_DISCOVERY_FILES * 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            walk(&path, depth + 1, out);
        } else if ft.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((mtime, path));
        }
        if out.len() >= MAX_DISCOVERY_FILES * 8 {
            return;
        }
    }
}

/// Production write seam: turn one computed result into the idempotent
/// `Engine::token_attribute` call. Kept here (not in the daemon) so the exact
/// param shape lives next to the code that builds it.
pub fn attribute_one(
    engine: &Engine,
    pa: &PendingAttribution,
    result: &AttributionResult,
) -> Result<bool, ApiError> {
    engine.token_attribute(&json!({
        "ref": pa.short_id,
        "source": result.source,
        "tool": result.tool,
        "confidence": result.confidence,
        "samples": result.samples,
        "input_tokens": result.totals.input,
        "output_tokens": result.totals.output,
        "cache_read_tokens": result.totals.cache_read,
        "cache_creation_tokens": result.totals.cache_creation,
    }))
}

/// The correlation facts carried by one `done` event, extracted tolerantly.
struct DoneInfo {
    completed: String,
    client: Option<String>,
    transcript_path: Option<String>,
    session_id: Option<String>,
}

/// The durable pending queue (store-as-queue, reminder precedent): every task
/// with a `done` event carrying correlation but no `tokens.attributed` event
/// yet. Reads the store; call it under a short engine lock and drop the lock
/// before parsing any transcript.
///
/// Catch-up after daemon downtime is free: because the queue is derived from the
/// store on every call, a task completed by a one-shot CLI while no daemon ran is
/// picked up on the next tick exactly like a reminder missed while down.
pub fn pending_attributions(engine: &Engine) -> Result<Vec<PendingAttribution>, ApiError> {
    let conn = engine.conn();

    // 1. The latest `tokens.attributed` rowid per task — the dedupe record
    //    (reminded_keys precedent). Keyed by rowid, not mere presence, so a
    //    marker written for an *earlier* completion does not suppress a later
    //    one: a reopen + re-complete appends a fresh `done` past this marker and
    //    must re-enter the queue (task_reopen leaves the old marker in place, as
    //    the event log is append-only).
    let attributed: HashMap<String, i64> = {
        let mut stmt = conn.prepare(
            "SELECT entity_id, MAX(rowid) FROM events \
             WHERE op = 'tokens.attributed' GROUP BY entity_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut map = HashMap::new();
        for r in rows {
            let (id, rowid) = r?;
            map.insert(id, rowid);
        }
        map
    };

    // 2. Latest `done` per task carrying correlation, not yet attributed. Rowid
    //    order so a reopened-then-redone task's most recent completion wins — and
    //    a `done` is "not yet attributed" when no `tokens.attributed` marker
    //    exists *after* it (rowid strictly greater), so tokens spent between a
    //    reopen and the next completion are attributed rather than silently lost.
    let mut candidates: HashMap<String, DoneInfo> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT entity_id, payload, ts, rowid FROM events WHERE op = 'done' ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for r in rows {
            let (task_id, payload, ts, done_rowid) = r?;
            if attributed
                .get(&task_id)
                .is_some_and(|&attr_rowid| attr_rowid > done_rowid)
            {
                continue;
            }
            let v = payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<Value>(p).ok())
                .unwrap_or(Value::Null);
            let field = |k: &str| {
                v.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let client = field("client");
            let transcript_path = field("transcript_path");
            let session_id = field("session_id");
            // "Carrying correlation" = at least one correlation key present. A
            // human's `tasqx done 4` has none and is never attributed.
            if client.is_none() && transcript_path.is_none() && session_id.is_none() {
                continue;
            }
            let completed = field("completed").unwrap_or(ts);
            candidates.insert(
                task_id,
                DoneInfo {
                    completed,
                    client,
                    transcript_path,
                    session_id,
                },
            );
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // 3. Earliest `start` instant per candidate (window start). First by rowid
    //    wins, so a start/stop/start task uses the beginning of its first interval.
    let mut starts: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT entity_id, payload, ts FROM events WHERE op = 'start' ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (task_id, payload, ts) = r?;
            if !candidates.contains_key(&task_id) {
                continue;
            }
            let started = payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<Value>(p).ok())
                .and_then(|v| {
                    v.get("interval_started")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or(ts);
            starts.entry(task_id).or_insert(started);
        }
    }

    // 4. short_id + created for the window-start fallback. One query, mapped.
    let mut meta: HashMap<String, (i64, String)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, short_id, created FROM tasks")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (id, short_id, created) = r?;
            if candidates.contains_key(&id) {
                meta.insert(id, (short_id, created));
            }
        }
    }

    let mut out = Vec::new();
    for (task_id, info) in candidates {
        // A candidate with no task row is impossible (the done event references
        // it), but tolerate it rather than panicking a background thread.
        let Some((short_id, created)) = meta.get(&task_id).cloned() else {
            continue;
        };
        let window_start = starts.get(&task_id).cloned().unwrap_or(created);
        // Buffered OTLP telemetry (#18) for this session, read here under the
        // short engine lock (a cheap indexed query — no file I/O) so the compute
        // step can prefer it over log-parsing. An absent session id matches
        // nothing, which is the common (log-parse-only) case.
        let (otel_samples, otel_tool) = match info.session_id.as_deref() {
            Some(sid) => engine.otlp_samples_for_session(sid)?,
            None => (Vec::new(), None),
        };
        out.push(PendingAttribution {
            task_id,
            short_id,
            window_start,
            window_end: info.completed,
            client: info.client,
            transcript_path: info.transcript_path,
            session_id: info.session_id,
            otel_samples,
            otel_tool,
        });
    }
    // Deterministic order for tests and for stable log lines.
    out.sort_by_key(|p| p.short_id);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse an RFC3339 instant for the `now` argument of `compute_attribution`.
    fn ts(s: &str) -> Timestamp {
        s.parse().expect("valid RFC3339 test timestamp")
    }

    fn sample(ts: &str, input: u64, output: u64) -> UsageSample {
        UsageSample {
            ts: ts.to_string(),
            model: None,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }
    }

    #[test]
    fn parser_for_matches_on_lowercased_substring() {
        assert_eq!(parser_for("claude-code"), Some(Parser::ClaudeCode));
        assert_eq!(parser_for("Claude Code"), Some(Parser::ClaudeCode));
        assert_eq!(parser_for("codex-cli"), Some(Parser::Codex));
        assert_eq!(parser_for("Gemini CLI"), Some(Parser::Gemini));
        assert_eq!(parser_for("github-copilot"), Some(Parser::Copilot));
        assert_eq!(parser_for("cursor"), None);
        assert_eq!(parser_for(""), None);
    }

    #[test]
    fn window_includes_both_endpoints_and_excludes_outside() {
        let samples = [
            sample("2026-07-24T09:59:59Z", 1, 1),       // before window
            sample("2026-07-24T10:00:00Z", 10, 20),     // exactly at start (inclusive)
            sample("2026-07-24T10:30:00Z", 100, 200),   // inside
            sample("2026-07-24T11:00:00Z", 1000, 2000), // exactly at end (inclusive)
            sample("2026-07-24T11:00:01Z", 1, 1),       // after window
        ];
        let (totals, n) =
            totals_in_window(&samples, "2026-07-24T10:00:00Z", "2026-07-24T11:00:00Z");
        assert_eq!(n, 3, "only the three in-window samples count");
        assert_eq!(totals.input, 1110);
        assert_eq!(totals.output, 2220);
    }

    #[test]
    fn a_bad_window_attributes_nothing_rather_than_everything() {
        let samples = [sample("2026-07-24T10:30:00Z", 10, 20)];
        let (totals, n) = totals_in_window(&samples, "not-a-timestamp", "2026-07-24T11:00:00Z");
        assert_eq!(n, 0);
        assert_eq!(totals.total(), 0);
    }

    #[test]
    fn an_unparseable_sample_timestamp_is_skipped_not_counted() {
        let samples = [
            sample("garbage", 999, 999),
            sample("2026-07-24T10:30:00Z", 10, 20),
        ];
        let (totals, n) =
            totals_in_window(&samples, "2026-07-24T10:00:00Z", "2026-07-24T11:00:00Z");
        assert_eq!(n, 1);
        assert_eq!(totals.input, 10);
    }

    #[test]
    fn confidence_follows_the_documented_rule() {
        assert_eq!(confidence_for(true, true), CONFIDENCE_HIGH);
        assert_eq!(confidence_for(true, false), CONFIDENCE_MEDIUM);
        assert_eq!(confidence_for(false, true), CONFIDENCE_LOW);
        assert_eq!(confidence_for(false, false), CONFIDENCE_LOW);
    }

    #[test]
    fn a_client_with_no_parser_terminates_with_a_zero_sample_result() {
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("cursor".into()),
            transcript_path: None,
            session_id: None,
            otel_samples: Vec::new(),
            otel_tool: None,
        };
        let r = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert!(!r.found);
        assert_eq!(r.samples, 0);
        assert_eq!(r.tool, "cursor");
    }

    #[test]
    fn a_missing_transcript_path_is_a_transient_error() {
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: Some("/no/such/transcript.jsonl".into()),
            session_id: None,
            otel_samples: Vec::new(),
            otel_tool: None,
        };
        // Minutes after completion: still transient (the file may yet be flushed).
        let err = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap_err();
        assert!(err.message.contains("not available yet"), "{}", err.message);
    }

    #[test]
    fn a_long_absent_transcript_path_gives_up_and_terminates() {
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: Some("/no/such/transcript.jsonl".into()),
            session_id: None,
            otel_samples: Vec::new(),
            otel_tool: None,
        };
        // Two days later the file is never coming: terminate with an empty marker
        // (found == false) rather than retrying — and forcing a rebuild — forever.
        let r = compute_attribution(&pa, ts("2026-07-26T11:05:00Z")).unwrap();
        assert!(!r.found);
        assert_eq!(r.samples, 0);
        assert_eq!(r.tool, "claude-code");
    }

    #[test]
    fn an_explicit_transcript_is_parsed_and_bucketed_to_the_window() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-compute-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Claude Code names each transcript `<session-id>.jsonl`, so a file named
        // for the completion's session id is a *verified* correlation => HIGH.
        let path = dir.join("sess-1.jsonl");
        // Two in-window assistant lines and one after the window.
        let content = [
            r#"{"timestamp":"2026-07-24T10:10:00.000Z","message":{"id":"a","model":"claude-opus-4-8","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}}"#,
            r#"{"timestamp":"2026-07-24T10:20:00.000Z","message":{"id":"b","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"timestamp":"2026-07-24T12:00:00.000Z","message":{"id":"c","usage":{"input_tokens":9999,"output_tokens":9999}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: Some(path.to_string_lossy().into_owned()),
            session_id: Some("sess-1".into()),
            otel_samples: Vec::new(),
            otel_tool: None,
        };
        let r = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert!(r.found);
        assert_eq!(r.samples, 2, "the out-of-window line is excluded");
        assert_eq!(r.totals.input, 110);
        assert_eq!(r.totals.output, 220);
        assert_eq!(r.totals.cache_read, 3);
        assert_eq!(r.totals.cache_creation, 4);
        assert_eq!(
            r.confidence, CONFIDENCE_HIGH,
            "explicit path + verified session id => high"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_uncorrelated_session_id_is_medium_not_high() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-uncorr-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // The transcript neither is named for the session id nor carries it on a
        // line, so the supplied id is a stale/wrong hook argument that cannot be
        // verified — the parse still happened, so MEDIUM, never HIGH.
        let path = dir.join("some-other-file.jsonl");
        let content = r#"{"timestamp":"2026-07-24T10:10:00.000Z","message":{"id":"a","model":"claude-opus-4-8","usage":{"input_tokens":10,"output_tokens":20}}}"#;
        std::fs::write(&path, content).unwrap();

        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: Some(path.to_string_lossy().into_owned()),
            session_id: Some("wrong-or-stale-id".into()),
            otel_samples: Vec::new(),
            otel_tool: None,
        };
        let r = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert!(r.found, "the transcript was parsed and had in-window spend");
        assert_eq!(
            r.confidence, CONFIDENCE_MEDIUM,
            "an unverifiable session id downgrades to medium"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn buffered_otel_is_preferred_over_a_transcript_and_never_double_counted() {
        // A transcript path is present AND buffered OTLP samples correlated by
        // session. OTEL must win: source `otel`, HIGH confidence, and the numbers
        // come from the buffer — the transcript is never even read (its path here
        // does not exist, which would be a transient error on the log-parse path).
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: Some("/no/such/transcript.jsonl".into()),
            session_id: Some("sess-1".into()),
            otel_samples: vec![
                sample("2026-07-24T10:15:00Z", 100, 200),
                sample("2026-07-24T12:00:00Z", 9999, 9999), // out of window: excluded
            ],
            otel_tool: Some("claude-code".into()),
        };
        let r = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert!(r.found);
        assert_eq!(r.source, SOURCE_OTEL, "telemetry outranks log-parse");
        assert_eq!(
            r.confidence, CONFIDENCE_HIGH,
            "session-matched buffer is high"
        );
        assert_eq!(r.samples, 1, "only the in-window telemetry sample counts");
        assert_eq!(r.totals.input, 100);
        assert_eq!(r.totals.output, 200);
    }

    #[test]
    fn otel_buffered_but_out_of_window_falls_back_to_log_parse() {
        // The session has telemetry, but none of it lands in the task's window,
        // so log-parse remains the fallback (here: an absent transcript => the
        // ordinary transient error, proving we fell through rather than using otel).
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: Some("/no/such/transcript.jsonl".into()),
            session_id: Some("sess-1".into()),
            otel_samples: vec![sample("2026-07-24T12:30:00Z", 100, 200)],
            otel_tool: Some("claude-code".into()),
        };
        let err = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap_err();
        assert!(
            err.message.contains("not available yet"),
            "fell back to the log-parse path: {}",
            err.message
        );
    }

    #[test]
    fn pending_set_reads_the_otel_buffer_and_the_write_stamps_source_otel() {
        // End-to-end over a real store: telemetry buffered for a session, a task
        // completed with that session id, and no transcript anywhere. Attribution
        // must reconstruct the tokens from the OTLP buffer and stamp the stored
        // measurement `source=otel` — with never a log-parse for this task.
        let engine = Engine::open_in_memory().unwrap();
        let sid = engine.task_add(&json!({ "title": "t" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        // window_start defaults to `created`; use it as the sample time so the
        // sample lands on the inclusive lower bound without a clock dependency.
        let created: String = engine
            .conn()
            .query_row("SELECT created FROM tasks", [], |r| r.get(0))
            .unwrap();
        engine
            .otlp_ingest(&[crate::otlp::OtlpSample {
                tool: "claude-code".into(),
                session_id: Some("sess-42".into()),
                sample: UsageSample {
                    ts: created,
                    model: Some("claude-opus-4-8".into()),
                    input_tokens: 111,
                    output_tokens: 222,
                    cache_read_tokens: 5,
                    cache_creation_tokens: 0,
                },
            }])
            .unwrap();
        engine
            .task_done(&json!({ "ref": sid, "client": "claude-code", "session_id": "sess-42" }))
            .unwrap();

        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "the completed task is pending attribution"
        );
        let pa = &pending[0];
        assert_eq!(
            pa.otel_samples.len(),
            1,
            "the pending-set build read the OTLP buffer by session id"
        );

        // `now` is irrelevant on the otel path (no absent-transcript give-up).
        let r = compute_attribution(pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert_eq!(r.source, SOURCE_OTEL);
        assert_eq!(r.confidence, CONFIDENCE_HIGH);
        assert!(r.found);
        assert_eq!(r.totals.input, 111);
        assert_eq!(r.totals.output, 222);

        assert!(
            attribute_one(&engine, pa, &r).unwrap(),
            "first write performs it"
        );
        let source: String = engine
            .conn()
            .query_row("SELECT source FROM token_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            source, SOURCE_OTEL,
            "the stored measurement is otel-sourced"
        );
    }
}
