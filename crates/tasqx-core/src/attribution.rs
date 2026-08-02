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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::{json, Value};

use crate::engine::Engine;
use crate::error::ApiError;
use crate::tokens::{
    self, codex, TokenTotals, UsageSample, CONFIDENCE_HIGH, CONFIDENCE_LOW, CONFIDENCE_MEDIUM,
    SOURCE_LOG_PARSE, SOURCE_OTEL, SOURCE_SELF_REPORT,
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

/// How long after completion an explicit `transcript_path` that cannot be turned
/// into samples — absent, or present but unreadable — keeps being retried before
/// the task terminates with an empty marker. Transcripts are flushed
/// asynchronously and lag the completion hook, so a brief retry is correct — but
/// a path that is still unusable a full day later never will be (deleted,
/// rotated, wrong path, a directory, or owned by another user), and retrying it
/// forever forces a full pending-set rebuild every tick for the life of the
/// daemon.
const TRANSCRIPT_GIVE_UP_SECS: i64 = 24 * 60 * 60;

/// A per-tool transcript parser. Internal enum wrapping the free functions each
/// `crate::tokens::<tool>` module exposes, so the attribution engine has one
/// uniform seam instead of a `match` at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parser {
    /// Claude Code's `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`
    /// transcripts. The file is *named* for the session, so a supplied session id
    /// can be proven against it — one of the only two variants that can reach
    /// [`CONFIDENCE_HIGH`] on the log-parse path.
    ClaudeCode,
    /// Codex CLI `rollout-*.jsonl` session logs under `${CODEX_HOME:-~/.codex}`.
    /// Each carries a `session_meta.id`, which both verifies a supplied session id
    /// and is the single anchor `discover_samples` has when no path was given.
    Codex,
    /// Gemini CLI's `telemetry.outfile` records. Exposes no per-session anchor, so
    /// an explicit path can never be *proven* to belong to the completing session
    /// and stays at [`CONFIDENCE_MEDIUM`].
    Gemini,
    /// GitHub Copilot CLI's OTEL export (`~/.copilot/otel/*.jsonl`). No
    /// per-session anchor either, so the same MEDIUM ceiling applies.
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
    let (totals, counted, _) = totals_in_window_excluding(samples, start, done, &[]);
    (totals, counted)
}

/// Parse one `[start, end]` window's bounds. `None` when either bound fails to
/// parse — an unusable window must never match anything. An inverted pair is
/// normalized rather than treated as "everything is out of range in a confusing
/// way".
fn parse_window(start: &str, end: &str) -> Option<(Timestamp, Timestamp)> {
    let (Ok(lo), Ok(hi)) = (start.parse::<Timestamp>(), end.parse::<Timestamp>()) else {
        return None;
    };
    Some(if lo <= hi { (lo, hi) } else { (hi, lo) })
}

/// [`totals_in_window`] with the D50 refusal rule: an in-window sample that ALSO
/// falls inside at least one `foreign` window — another task's `[window_start,
/// window_end]` over the same sample source — is *contested* and banked for no
/// one. Returns `(totals, counted, contested)` where `counted` covers only the
/// uncontested in-window samples summed into `totals`.
///
/// Foreign windows use the same inclusive-bounds semantics as the task's own
/// window, and an unparseable foreign bound makes THAT window empty (it contests
/// nothing) — symmetrical with the main window, where a bad bound attributes
/// nothing.
pub fn totals_in_window_excluding(
    samples: &[UsageSample],
    start: &str,
    done: &str,
    foreign: &[(String, String)],
) -> (TokenTotals, usize, usize) {
    let (totals, counted, contested, _) =
        totals_in_window_refusing(samples, start, done, foreign, &HashSet::new());
    (totals, counted, contested)
}

/// [`totals_in_window_excluding`] plus the identity half of the refusal rule:
/// an in-window sample whose [`UsageSample::id`] is in `consumed` — already
/// banked by another task on an earlier tick — is contested *regardless of what
/// its current stamp says*, because a streamed re-emission can move a deduped
/// stamp across a window edge between reads while the id never changes.
/// Samples with no id keep the window-only contest semantics (only the Claude
/// Code parser observes moving stamps, and it always has ids).
///
/// The fourth return is the ids of the samples actually summed, so the caller
/// can persist which samples this measurement consumed.
pub fn totals_in_window_refusing(
    samples: &[UsageSample],
    start: &str,
    done: &str,
    foreign: &[(String, String)],
    consumed: &HashSet<String>,
) -> (TokenTotals, usize, usize, Vec<String>) {
    let Some((lo, hi)) = parse_window(start, done) else {
        return (TokenTotals::default(), 0, 0, Vec::new());
    };
    let foreign: Vec<(Timestamp, Timestamp)> = foreign
        .iter()
        .filter_map(|(s, e)| parse_window(s, e))
        .collect();

    let mut totals = TokenTotals::default();
    let mut counted = 0;
    let mut contested = 0;
    let mut counted_ids = Vec::new();
    for s in samples {
        let Ok(ts) = s.ts.parse::<Timestamp>() else {
            continue;
        };
        if ts < lo || ts > hi {
            continue;
        }
        let already_banked = s.id.as_deref().is_some_and(|id| consumed.contains(id));
        if already_banked || foreign.iter().any(|&(flo, fhi)| ts >= flo && ts <= fhi) {
            contested += 1;
        } else {
            totals.add_sample(s);
            counted += 1;
            if let Some(id) = &s.id {
                counted_ids.push(id.clone());
            }
        }
    }
    (totals, counted, contested, counted_ids)
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
    /// The task's store id (`tasks.id` / `events.entity_id`), the key every query
    /// in [`pending_attributions`] joins on and the id the `tokens.attributed`
    /// marker is written against. Nothing downstream of the build reads it — the
    /// write addresses the task by [`short_id`](Self::short_id) — so it is
    /// effectively the entry's identity for tests and debugging.
    pub task_id: String,
    /// The user-facing `#n`. This is what the write path actually resolves the
    /// task by ([`attribute_one`] passes it as `ref`), and what the daemon's
    /// per-task error throttle is keyed on, so one task failing forever cannot
    /// suppress another task's log line.
    pub short_id: i64,
    /// Window start (RFC3339): the earliest `start` event's `interval_started`,
    /// falling back to the task's `created` when it was completed without ever
    /// being timed.
    pub window_start: String,
    /// Window end (RFC3339): the completion instant from the `done` event.
    pub window_end: String,
    /// The completing tool as `"<name> <version>"`, either passed on `task.done`
    /// or filled in from the MCP `clientInfo` handshake (#12). Free-form by
    /// design — new coding agents ship faster than tasqx releases — which is why
    /// [`parser_for`] matches a lowercased substring instead of a fixed set, and
    /// why this doubles as the `tool` label stored on the measurement. `None`
    /// means the completion carried one of the *other* correlation keys but no
    /// tool label; a `done` with none of the three never enters this queue.
    pub client: Option<String>,
    /// Absolute path to the tool's session transcript, as supplied on `task.done`.
    /// Its presence picks the whole strategy: an explicit path is parsed directly
    /// (MEDIUM, or HIGH once the session id verifies) and an unusable one is
    /// *retried* until `TRANSCRIPT_GIVE_UP_SECS`, whereas `None` falls back to
    /// best-effort discovery, which is always LOW and may legitimately find
    /// nothing.
    pub transcript_path: Option<String>,
    /// The agent's session/conversation id from the `done` event. It does two
    /// separate jobs: it is what `Engine::otlp_samples_for_session` looked the
    /// buffered telemetry up by when building this entry, and it is
    /// what must *verify* against the parsed transcript for the measurement to
    /// earn [`CONFIDENCE_HIGH`] — a present but unverifiable id stays MEDIUM.
    pub session_id: Option<String>,
    /// Buffered OTLP samples (#18) whose `session_id` matched this task's, read
    /// from the store during the pending-set build. When non-empty and in-window,
    /// they are preferred over log-parsing (source `otel`), so a task is measured
    /// from EITHER telemetry OR a transcript, never both.
    pub otel_samples: Vec<UsageSample>,
    /// The tool that emitted the buffered OTLP samples, used only to label the
    /// stored measurement when the completion carried no `client`.
    pub otel_tool: Option<String>,
    /// True when this task already self-reported its token spend — either on
    /// `task.done` (#13: a `token_usage` row written in the SAME transaction and
    /// echoed as the done payload's `tokens` key) or via a later `token.add`
    /// with `source=self-report` (D50: one task never mixes channels). That
    /// self-report is the authoritative measurement for the task, so async
    /// attribution must NOT
    /// reconstruct a second measurement for the same window — doing so would
    /// double-count identical spend in every report. Such a task is still carried
    /// through the pending set so it receives a terminating `tokens.attributed`
    /// marker, but with no measurement.
    pub self_reported: bool,
    /// The `(window_start, window_end)` pair of every OTHER task that shares
    /// this task's sample source — an equal non-null `transcript_path` in the
    /// done payload, or an equal non-null `session_id` (D50). A sample inside
    /// this task's window that also falls inside any of these is *contested*
    /// and banked for no one ([`totals_in_window_excluding`]).
    ///
    /// Built from a scan of ALL correlated `done` events in the store —
    /// including tasks attributed on an earlier tick. Co-pending entries alone
    /// would be race-dependent: a task attributed last tick has left the queue,
    /// but its window still claims the same samples.
    pub foreign_windows: Vec<(String, String)>,
    /// The union of sample ids already consumed by ALL other tasks in the
    /// store, read from their `tokens.attributed` payloads. A banked decision
    /// is final: a sample whose id is in here is contested no matter where its
    /// re-parsed stamp currently lands, because the Claude Code parser keeps
    /// the LAST occurrence's timestamp for a deduped message id and a
    /// mid-write re-emission can move that stamp across a window edge between
    /// ticks.
    ///
    /// Unlike [`foreign_windows`](Self::foreign_windows) this is NOT joined
    /// through `shares_sample_source` (D50 Decision 2): that join re-derives
    /// source identity from the live filesystem, so a claim banked under a
    /// spelling that stops resolving (dangling symlink deleted between ticks)
    /// would silently dissolve. Identity contest is exact — message ids are
    /// unique per API response — so global is strictly safer.
    pub consumed_sample_ids: HashSet<String>,
}

/// The outcome of attributing one task: the four-way totals, how many samples
/// landed in the window, the tool name to store, and the confidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionResult {
    /// The four token buckets summed over the in-window samples. Kept four-way to
    /// the last hop (research rule #5): cache tokens cost a fraction of fresh ones
    /// and every tool defines them differently, so blending them into one number
    /// would destroy exactly what a cost report needs.
    pub totals: TokenTotals,
    /// How many samples landed *inside* the window — not how many the transcript
    /// or the buffer held. It is evidence for how [`totals`](Self::totals) was
    /// reached and is written only into the `tokens.attributed` event payload; the
    /// `token_usage` row has no such column.
    pub samples: usize,
    /// The tool label to store on the measurement: the completion's `client`,
    /// falling back on the telemetry path to the tool that emitted the buffered
    /// samples when the completion named none, and empty when neither is known.
    /// Free-form, unlike [`source`](Self::source) and
    /// [`confidence`](Self::confidence), which are closed vocabularies.
    pub tool: String,
    /// Where these tokens came from: [`SOURCE_OTEL`] when buffered telemetry won,
    /// [`SOURCE_LOG_PARSE`] otherwise. Stored on the measurement so a report can
    /// tell the two trust stories apart forever.
    pub source: &'static str,
    /// How much to trust these numbers, one of [`crate::tokens::TOKEN_CONFIDENCE`]
    /// and always one of the three constants named in the module's confidence
    /// rule. It grades *how the samples were found* — explicit path versus
    /// discovery, session id verified or not — never how large or plausible the
    /// totals are.
    pub confidence: &'static str,
    /// True when the window held real spend (total > 0), so a `token_usage` row
    /// should be written. When false only the terminating `tokens.attributed`
    /// marker is written.
    pub found: bool,
    /// The ids of the samples summed into [`totals`](Self::totals), for the
    /// samples that carried one. Persisted in the `tokens.attributed` payload
    /// so later ticks can refuse these samples by identity, whatever their
    /// re-parsed stamps then say. Empty when no counted sample had an id.
    pub sample_ids: Vec<String>,
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
            sample_ids: Vec::new(),
        }
    }
}

/// Compute the attribution for one pending task. Does file I/O and MUST run with
/// no engine lock held.
///
/// Returns `Err` only for a *transient* condition — an explicit `transcript_path`
/// that is not present yet (transcripts are written asynchronously and lag the
/// completion hook, research doc), one that is present but could not be read this
/// time, or a window whose every in-window sample is contested by another task's
/// window (D50 — banked for no one). The daemon treats that as a retry, never a
/// fatal error and never a stored marker. An unknown client or a discovery scan that finds nothing is
/// `Ok` with `found == false`: those terminate. So does EITHER transcript
/// failure — absent or unreadable — once the completion is more than
/// `TRANSCRIPT_GIVE_UP_SECS` old (`now` is used only for that cutoff): the two
/// share one deadline, because an unreadable path (a directory, a root-owned
/// file) repeats forever just as reliably as an absent one, and each repeat
/// costs a full pending-set rebuild on the next tick. The lone remaining
/// retry-forever case is a `window_end` that does not parse — deliberate, see
/// `transcript_gave_up`.
pub fn compute_attribution(
    pa: &PendingAttribution,
    now: Timestamp,
) -> Result<AttributionResult, ApiError> {
    let tool = pa.client.clone().unwrap_or_default();

    // A completion that already self-reported its spend (#13) is the
    // authoritative measurement for this task — a `token_usage` row was written
    // atomically with `task.done`. Reconstructing a second measurement (from the
    // OTLP buffer or a transcript) for the same window would double-count the
    // identical tokens in every roll-up. Terminate with a marker only, before the
    // telemetry and log-parse paths below, so neither can re-measure it.
    if pa.self_reported {
        return Ok(AttributionResult::empty(tool));
    }

    // Prefer buffered OTLP telemetry (#18) when it correlated to this task's
    // session and lands in the window: it is per-request, timestamped, and needs
    // no file I/O. Because we return here, a task measured from telemetry is
    // never ALSO log-parsed — one source per task, so no double-count. The buffer
    // was matched by `session_id` during the pending-set build, so a hit is a
    // verified correlation => HIGH confidence. This runs even for a client tasqx
    // has no transcript parser for (telemetry needs none).
    //
    // The D50 refusal applies here too: OTLP samples are keyed by session, so
    // two tasks with overlapping windows over one session would double-count
    // identically to the log-parse case. Contested telemetry banks for no one.
    if !pa.otel_samples.is_empty() {
        let (totals, n, contested, sample_ids) = totals_in_window_refusing(
            &pa.otel_samples,
            &pa.window_start,
            &pa.window_end,
            &pa.foreign_windows,
            &pa.consumed_sample_ids,
        );
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
                sample_ids,
            });
        }
        // D50 symmetry: telemetry whose every in-window sample is claimed by
        // another task's window is contested, banked for no one — and stays
        // TRANSIENT on the shared give-up deadline, exactly like a contested
        // transcript. Falling through here would reach the terminal empty
        // paths and turn the contest into a permanent zero.
        if n == 0 && contested > 0 && !transcript_gave_up(now, &pa.window_end) {
            return Err(ApiError::internal(format!(
                "otlp usage in window is contested: session {}",
                pa.session_id.as_deref().unwrap_or_default()
            )));
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
            let samples = match parser.samples_from_file(file) {
                Ok(samples) => samples,
                // Present but unreadable — a directory at that path, a
                // root-owned session file, a torn read of a file being written.
                // A read that failed once may succeed on the next tick, so this
                // is transient exactly like the absent case above and gets the
                // SAME deadline: without it the read fails identically on every
                // tick forever, and each failure makes `attribution_tick` return
                // -1, rebuilding the whole pending set at the tick rate for the
                // life of the daemon while the task never terminates.
                Err(e) => {
                    if transcript_gave_up(now, &pa.window_end) {
                        return Ok(AttributionResult::empty(tool));
                    }
                    return Err(e);
                }
            };
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

    let (totals, n, contested, sample_ids) = totals_in_window_refusing(
        &samples,
        &pa.window_start,
        &pa.window_end,
        &pa.foreign_windows,
        &pa.consumed_sample_ids,
    );
    let found = totals.total() > 0;

    // D50: the window held samples, but every one is also claimed by at least
    // one other task's window — contested, banked for no one. Transient on the
    // SAME give-up deadline as the empty-window case below: transcript
    // timestamps are non-monotonic mid-write, so an uncontested line can still
    // arrive, and a terminal marker would permanently suppress it. The distinct
    // message keeps daemon stderr diagnosable ("contested" versus "no usage").
    //
    // Explicit-transcript path ONLY (`transcript_parsed`): discovery keeps its
    // deliberate terminal-on-first-tick doctrine — an empty discovery result
    // means "no candidate carried anything", and a contested one means the
    // same spend is already spoken for; neither has a named file to wait on,
    // so retrying for 24h would keep the task in the pending set for no gain.
    // (Contested TELEMETRY stays transient via the OTLP arm above.)
    if transcript_parsed && n == 0 && contested > 0 && !transcript_gave_up(now, &pa.window_end) {
        return Err(ApiError::internal(format!(
            "usage in window is contested: {}",
            pa.transcript_path.as_deref().unwrap_or_default()
        )));
    }

    // #73: an EXPLICIT transcript that parsed but holds nothing in the window is
    // "not yet", not "nothing". `tasqx done` runs inside the agent turn whose
    // usage it wants to count, and that turn's usage line is not written until the
    // turn ends — so for any task living entirely inside one turn, this is the
    // normal state at the only moment we look, not an edge case. Storing `found:
    // false` here made `has_attributed_event` terminal on a zero the real numbers
    // could never replace; in the field two of five tasks were lost that way
    // inside a single 500 ms tick.
    //
    // Same deadline as absent and unreadable, and for the same reason: a task
    // that genuinely cost nothing must still leave the pending set, or every tick
    // rebuilds it forever. Only the explicit-path branch qualifies —
    // `transcript_parsed` is false for discovery, which has no named file to wait
    // on and whose empty result means "no candidate carried anything".
    if transcript_parsed && !found && !transcript_gave_up(now, &pa.window_end) {
        return Err(ApiError::internal(format!(
            "no usage in window yet: {}",
            pa.transcript_path.as_deref().unwrap_or_default()
        )));
    }

    Ok(AttributionResult {
        totals,
        samples: n,
        tool,
        source: SOURCE_LOG_PARSE,
        confidence: confidence_for(transcript_parsed, session_correlated),
        found,
        sample_ids,
    })
}

/// Whether an unusable explicit transcript — absent, or present but unreadable —
/// has been retried long enough to give up: true once `now` is more than
/// [`TRANSCRIPT_GIVE_UP_SECS`] past the completion instant. An unparseable
/// `window_end` never gives up (keeps the old retry-forever behavior for that
/// pathological case rather than discarding a possibly-real completion), so this
/// bound is not a guarantee that retries always terminate.
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
        "sample_ids": result.sample_ids,
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
    self_reported: bool,
    /// True when a `tokens.attributed` marker already sits past this task's
    /// latest correlated `done`: the task is no longer pending, but its window
    /// still contests samples on a shared source, so it is retained in the scan
    /// as a foreign window rather than dropped.
    attributed: bool,
    /// [`transcript_path`](Self::transcript_path) canonicalized (filesystem
    /// identity when the file exists, lexical normalization when it does not),
    /// filled in one memoized pass after the done-event scan. Sharing a sample
    /// source is a question about the FILE, so `dir/t.jsonl` and
    /// `dir/./t.jsonl` — or a symlink spelling — must compare equal; a string
    /// comparison of the raw path let two spellings of one file bank the same
    /// spend twice.
    canon_path: Option<PathBuf>,
}

/// Whether two completions draw their samples from the same source: the same
/// transcript FILE (canonicalized paths, so two spellings of one file match),
/// or an equal non-null `session_id`. Two `None`s are NOT a match — an absent
/// key identifies nothing.
fn shares_sample_source(a: &DoneInfo, b: &DoneInfo) -> bool {
    (a.canon_path.is_some() && a.canon_path == b.canon_path)
        || (a.session_id.is_some() && a.session_id == b.session_id)
}

/// One path string's identity for [`shares_sample_source`]:
/// `std::fs::canonicalize` when the file exists (resolves symlinks and dot
/// segments against the real filesystem), falling back to a purely lexical
/// normalization when it does not — a transcript that has not been flushed yet
/// must still contest its own other spellings.
fn canonical_path(raw: &str) -> PathBuf {
    let p = Path::new(raw);
    std::fs::canonicalize(p).unwrap_or_else(|_| lexical_normalize(p))
}

/// Resolve `.` and `..` segments without touching the filesystem. `..` pops a
/// preceding normal component; at a root it is dropped (`/..` is `/`), and in
/// a relative path with nothing left to pop it is kept, so distinct locations
/// stay distinct.
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let popped =
                    matches!(out.components().next_back(), Some(Component::Normal(_))) && out.pop();
                if !popped && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The sample ids each task's attribution(s) consumed, read from every
/// `tokens.attributed` payload (all markers, not just the latest: a reopen +
/// re-complete can bank twice, and each banked id stays consumed forever).
/// Shared by the live pending build ([`pending_attributions`], where it feeds
/// `consumed_sample_ids`) and the D50 Decision 3 recompute
/// (`Engine::token_recompute`, which rebuilds these claims in marker order) —
/// both must read the same record or the migration and the live tick disagree
/// about what is banked. Markers written before sample ids were persisted
/// carry no claim here; that residual is exactly what the recompute closes.
pub(crate) fn consumed_sample_ids_by_task(
    engine: &Engine,
) -> Result<HashMap<String, Vec<String>>, ApiError> {
    let mut stmt = engine.conn().prepare(
        "SELECT entity_id, payload FROM events \
         WHERE op = 'tokens.attributed' AND payload LIKE '%sample_ids%'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut consumed: HashMap<String, Vec<String>> = HashMap::new();
    for r in rows {
        let (task_id, payload) = r?;
        let ids: Vec<String> = payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<Value>(p).ok())
            .and_then(|v| {
                v.get("sample_ids").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
            })
            .unwrap_or_default();
        if !ids.is_empty() {
            consumed.entry(task_id).or_default().extend(ids);
        }
    }
    Ok(consumed)
}

/// Every task with a correlated `done` event, keyed by task id — the first
/// half of the window scan, cheap enough to run per tick so
/// [`pending_attributions`] can return early on an all-attributed store
/// before any path canonicalization or window assembly happens.
fn correlated_done_scan(engine: &Engine) -> Result<HashMap<String, DoneInfo>, ApiError> {
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

    // 1b. Tasks that already carry a stored self-report measurement. The
    //     done-payload `tokens` key only covers a self-report made ON
    //     `task.done`; a `token.add` with `source=self-report` after the
    //     completion is the same claim through the other door, and log-parse
    //     reconstructing a second measurement for it would double-count the
    //     identical spend. One indexed query (idx_token_usage_task's table,
    //     filtered on source) per pending build.
    let self_report_rows: HashSet<String> = {
        let mut stmt =
            conn.prepare("SELECT DISTINCT task_id FROM token_usage WHERE source = ?1")?;
        let rows = stmt.query_map([SOURCE_SELF_REPORT], |r| r.get::<_, String>(0))?;
        let mut set = HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        set
    };

    // 2. Latest `done` per task carrying correlation. Rowid order so a
    //    reopened-then-redone task's most recent completion wins — and a `done`
    //    is "not yet attributed" when no `tokens.attributed` marker exists
    //    *after* it (rowid strictly greater), so tokens spent between a reopen
    //    and the next completion are attributed rather than silently lost.
    //    Already-attributed tasks are RETAINED (flagged, not skipped): they are
    //    no longer pending, but their windows still contest samples on a shared
    //    transcript or session (D50), so every pending entry needs them.
    let mut correlated: HashMap<String, DoneInfo> = HashMap::new();
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
            // A self-report on `task.done` (#13) echoes its measurement into the
            // done payload's `tokens` key. Its presence means the spend is already
            // recorded, so this task must terminate with a marker only — never a
            // second, double-counting measurement. A stored self-report row
            // (`token.add` after the done) means exactly the same thing (D50:
            // one task never mixes channels).
            let self_reported = v.get("tokens").is_some_and(|t| !t.is_null())
                || self_report_rows.contains(&task_id);
            let completed = field("completed").unwrap_or(ts);
            let is_attributed = attributed
                .get(&task_id)
                .is_some_and(|&attr_rowid| attr_rowid > done_rowid);
            correlated.insert(
                task_id,
                DoneInfo {
                    completed,
                    client,
                    transcript_path,
                    session_id,
                    self_reported,
                    attributed: is_attributed,
                    canon_path: None,
                },
            );
        }
    }
    Ok(correlated)
}

/// The correlated-window map: every task with a correlated `done` event, its
/// `[window_start, window_end]`, and which OTHER tasks share its sample
/// source. ONE builder, shared by the live pending build
/// ([`pending_attributions`]) and the D50 Decision 3 recompute
/// (`Engine::token_recompute`), so the migration can never disagree with the
/// live tick about what a window is or who contests it.
pub(crate) struct WindowScan {
    correlated: HashMap<String, DoneInfo>,
    /// Earliest `start` instant per correlated task.
    starts: HashMap<String, String>,
    /// task_id -> (short_id, created) — `created` is the window-start fallback.
    meta: HashMap<String, (i64, String)>,
    /// Every correlated task's `(task_id, window_start, window_end)`, sorted
    /// for stable foreign-window order.
    windows: Vec<(String, String, String)>,
}

impl WindowScan {
    /// The full scan. [`pending_attributions`] runs the two halves itself so
    /// it can return early on an all-attributed store; everyone else takes
    /// this one door.
    pub(crate) fn build(engine: &Engine) -> Result<Self, ApiError> {
        Self::finish(engine, correlated_done_scan(engine)?)
    }

    /// The second half: canonicalize paths and assemble the windows for an
    /// already-scanned correlated map.
    fn finish(
        engine: &Engine,
        mut correlated: HashMap<String, DoneInfo>,
    ) -> Result<Self, ApiError> {
        let conn = engine.conn();

        // 2b. Canonicalize each DISTINCT transcript path string once (a stat per
        //     path, memoized — never per pair), so `shares_sample_source` compares
        //     file identity instead of spelling.
        {
            let mut canon_memo: HashMap<String, PathBuf> = HashMap::new();
            for info in correlated.values_mut() {
                if let Some(tp) = &info.transcript_path {
                    let canon = canon_memo
                        .entry(tp.clone())
                        .or_insert_with(|| canonical_path(tp));
                    info.canon_path = Some(canon.clone());
                }
            }
        }

        // 3. Earliest `start` instant per correlated task (window start) — attributed
        //    neighbours included, their windows are needed as foreign. First by rowid
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
                if !correlated.contains_key(&task_id) {
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
                if correlated.contains_key(&id) {
                    meta.insert(id, (short_id, created));
                }
            }
        }

        // 5. Every correlated task's window, pending or not, sorted for stable
        //    foreign_windows order: `(task_id, window_start, window_end)`.
        let mut windows: Vec<(String, String, String)> = correlated
            .keys()
            .filter_map(|task_id| {
                let (_, created) = meta.get(task_id)?;
                let window_start = starts.get(task_id).unwrap_or(created).clone();
                let window_end = correlated[task_id].completed.clone();
                Some((task_id.clone(), window_start, window_end))
            })
            .collect();
        windows.sort();

        Ok(WindowScan {
            correlated,
            starts,
            meta,
            windows,
        })
    }

    /// The `(window_start, window_end)` of one correlated task; `None` when
    /// the task has no correlated `done` (or, degenerately, no task row).
    pub(crate) fn window_for(&self, task_id: &str) -> Option<(&str, &str)> {
        self.windows
            .iter()
            .find(|(id, _, _)| id == task_id)
            .map(|(_, ws, we)| (ws.as_str(), we.as_str()))
    }

    /// The windows of every OTHER task drawing samples from the same
    /// transcript or session — contested-sample refusal needs them all,
    /// attributed neighbours included (D50).
    pub(crate) fn foreign_windows_for(&self, task_id: &str) -> Vec<(String, String)> {
        let Some(info) = self.correlated.get(task_id) else {
            return Vec::new();
        };
        self.windows
            .iter()
            .filter(|(other_id, _, _)| {
                other_id != task_id && shares_sample_source(info, &self.correlated[other_id])
            })
            .map(|(_, ws, we)| (ws.clone(), we.clone()))
            .collect()
    }
}

/// One task's log-parse measurement, re-derived from today's transcript under
/// the refusal rule — the compute half of the D50 Decision 3 recompute.
pub(crate) struct RecomputedMeasurement {
    /// The four buckets summed over the surviving (in-window, uncontested,
    /// unclaimed) samples.
    pub(crate) totals: TokenTotals,
    /// How many samples survived into `totals`.
    pub(crate) samples: usize,
    /// The completing `client` from the done event, when it carried one; the
    /// caller falls back to the stored row's tool label.
    pub(crate) tool: Option<String>,
    /// Re-earned per the module's confidence rule (explicit path parsed;
    /// session verified or not).
    pub(crate) confidence: &'static str,
    /// The ids of the samples summed, for the samples that carried one — the
    /// claims this measurement re-establishes.
    pub(crate) sample_ids: Vec<String>,
    /// How many of the transcript's samples were CONTESTED away from this
    /// task: in-window samples refused by claim or foreign-window overlap,
    /// plus samples whose current stamp left this task's window but landed
    /// inside another task's window (or under another task's claim) — taken,
    /// not lost. The caller's keep-vs-rewrite policy hangs off this count
    /// (D50 Decision 3 as amended: only contest removes tokens).
    pub(crate) contested: usize,
}

/// Re-evaluate one banked task against its transcript as it reads today.
/// Mirrors [`compute_attribution`]'s explicit-path branch — same parser
/// selection, same session verification, same refusal via
/// [`totals_in_window_refusing`] — minus the transient retry machinery: the
/// recompute is a one-shot pass over history, so `None` (no correlated
/// window, no parser, no explicit path, or an unreadable file) is the
/// caller's terminal *downgrade* signal, never a retry. Discovery-sourced
/// measurements also land on `None` deliberately: a root scan is not
/// re-runnable deterministically, so their counts are kept, not re-derived.
/// `own_claims` is the set of sample ids the task's own banked markers
/// recorded (empty for a pre-upgrade bank): it scopes the out-of-window
/// contest check below to the samples that were actually this task's
/// evidence, when that is knowable.
pub(crate) fn recompute_measurement(
    scan: &WindowScan,
    task_id: &str,
    claims: &HashSet<String>,
    own_claims: &HashSet<String>,
) -> Option<RecomputedMeasurement> {
    let info = scan.correlated.get(task_id)?;
    let (window_start, window_end) = scan.window_for(task_id)?;
    let parser = info.client.as_deref().and_then(parser_for)?;
    let path = info.transcript_path.as_deref().filter(|s| !s.is_empty())?;
    let file = Path::new(path);
    let samples = parser.samples_from_file(file).ok()?;
    let correlated = info
        .session_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .is_some_and(|sid| parser.session_matches(file, sid));
    let foreign = scan.foreign_windows_for(task_id);
    let (totals, counted, mut contested, sample_ids) =
        totals_in_window_refusing(&samples, window_start, window_end, &foreign, claims);
    // The other half of the contest count: a sample whose CURRENT stamp sits
    // outside this task's window is invisible to the window sum above, but
    // when that stamp lands inside another task's window — or its id is under
    // another task's claim — the spend was TAKEN by that task, not lost to
    // drift, and the caller must be allowed to remove it here (single-count).
    // A stamp that drifted outside every window and every claim stays out of
    // this count: that is evidence drift, which never deletes (D50 Decision 3
    // as amended). When the task's own bank recorded which samples it
    // consumed, only those samples can testify to a taking; a bank with no
    // recorded identity (pre-upgrade marker) gets the broad reading — closing
    // exactly the upgrade window the full replay exists for.
    if let Some((lo, hi)) = parse_window(window_start, window_end) {
        let foreign: Vec<(Timestamp, Timestamp)> = foreign
            .iter()
            .filter_map(|(s, e)| parse_window(s, e))
            .collect();
        for s in &samples {
            let Ok(ts) = s.ts.parse::<Timestamp>() else {
                continue;
            };
            if ts >= lo && ts <= hi {
                continue; // already settled by the window sum above
            }
            if !own_claims.is_empty() && !s.id.as_deref().is_some_and(|id| own_claims.contains(id))
            {
                continue; // provably never this task's evidence
            }
            let claimed_elsewhere = s.id.as_deref().is_some_and(|id| claims.contains(id));
            if claimed_elsewhere || foreign.iter().any(|&(flo, fhi)| ts >= flo && ts <= fhi) {
                contested += 1;
            }
        }
    }
    Some(RecomputedMeasurement {
        totals,
        samples: counted,
        tool: info.client.clone(),
        confidence: confidence_for(true, correlated),
        sample_ids,
        contested,
    })
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
    let correlated = correlated_done_scan(engine)?;
    if correlated.values().all(|info| info.attributed) {
        return Ok(Vec::new());
    }
    // Feeds `consumed_sample_ids` below, so a sample banked on an earlier tick
    // is refused by identity even after its re-parsed stamp moves out of the
    // banked task's window.
    let consumed_by_task = consumed_sample_ids_by_task(engine)?;
    let scan = WindowScan::finish(engine, correlated)?;

    let mut out = Vec::new();
    for (task_id, info) in scan.correlated.iter() {
        if info.attributed {
            continue;
        }
        // A candidate with no task row is impossible (the done event references
        // it), but tolerate it rather than panicking a background thread.
        let Some((short_id, created)) = scan.meta.get(task_id).cloned() else {
            continue;
        };
        let window_start = scan.starts.get(task_id).cloned().unwrap_or(created);
        // Buffered OTLP telemetry (#18) for this session, read here under the
        // short engine lock (a cheap indexed query — no file I/O) so the compute
        // step can prefer it over log-parsing. An absent session id matches
        // nothing, which is the common (log-parse-only) case.
        let (otel_samples, otel_tool) = match info.session_id.as_deref() {
            Some(sid) => engine.otlp_samples_for_session(sid)?,
            None => (Vec::new(), None),
        };
        let foreign_windows = scan.foreign_windows_for(task_id);
        // And the sample ids ALL other tasks already consumed: banked by
        // identity, refused by identity — whatever the samples' re-parsed
        // stamps say on this tick. Deliberately GLOBAL, with no
        // `shares_sample_source` join (D50 Decision 2): source identity is
        // re-derived from the live filesystem each build, so a claim joined
        // through it dissolves when the banked spelling stops resolving (a
        // dangling symlink, a deleted transcript) — and a message id is unique
        // per API response, so a byte-copied transcript sharing ids SHOULD be
        // refused as the same spend. Residual, accepted until slice 3: a
        // marker banked before sample ids were persisted carries no claim, so
        // a moved-stamp theft against a pre-upgrade bank is closed only by the
        // full ordered recompute D50 Decision 3 mandates
        // (docs/specs/2026-07-31-attribution-direction-design.md).
        let consumed_sample_ids: HashSet<String> = consumed_by_task
            .iter()
            .filter(|(other_id, _)| *other_id != task_id)
            .flat_map(|(_, ids)| ids.iter().cloned())
            .collect();
        out.push(PendingAttribution {
            task_id: task_id.clone(),
            short_id,
            window_start,
            window_end: info.completed.clone(),
            client: info.client.clone(),
            transcript_path: info.transcript_path.clone(),
            session_id: info.session_id.clone(),
            otel_samples,
            otel_tool,
            self_reported: info.self_reported,
            foreign_windows,
            consumed_sample_ids,
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

    /// Serialises every test that touches the DISCOVERY branch.
    ///
    /// Discovery scans process-global roots, and one of its tests
    /// (`contested_discovery_samples_stay_terminal_rather_than_retrying`) plants
    /// a transcript and points `$CLAUDE_CONFIG_DIR` at it with `set_var` — which
    /// every other thread in this binary sees, because environment variables are
    /// per-process and `cargo test` runs tests as threads.
    ///
    /// That was not a theoretical hazard. Its planted sample is stamped
    /// `2020-01-01T10:10:00Z`, and the sibling
    /// `discovery_finding_nothing_stays_terminal_rather_than_retrying` asserts
    /// that a scan over `2020-01-01T10:00Z..11:00Z` finds NOTHING — the same
    /// hour. When the two overlapped, the second test's scan saw the first
    /// test's transcript through the override, `found` came back true, and it
    /// failed at `assert!(!r.found)`. Measured on Linux: 1 failure in 15 runs of
    /// the full lib binary, and 0 in 20 runs of either test on its own, which is
    /// exactly the profile of a race and exactly the profile of a flake nobody
    /// can reproduce from the failure message.
    ///
    /// The old code carried a `// SAFETY:` note claiming the concurrent readers
    /// "tolerate an extra root". They do tolerate it; that was never the
    /// problem. The problem is that the extra root CONTAINS an in-window sample
    /// for the window another test is asserting is empty, so tolerating it is
    /// precisely what makes the measurement wrong.
    ///
    /// Both tests take this lock, so the override is never live while another
    /// discovery scan runs. Poisoning is deliberately ignored: a panic in one
    /// test has already failed that test, and turning it into a cascade of
    /// unrelated failures hides the original.
    static DISCOVERY_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A `done` event payload carrying a transcript path, SERIALISED rather than
    /// formatted.
    ///
    /// These tests rewrite event payloads directly, because the engine stamps
    /// completions with wall-clock time and the windows under test are
    /// field-observed instants. The payload must be built by [`serde_json`]: a
    /// transcript path is an OS path, and on Windows it is `C:\Users\…`, whose
    /// backslashes are not valid JSON escapes. Interpolating one into a
    /// hand-written JSON string yields a document that is well-formed on Linux
    /// and malformed on Windows — and it does not fail loudly there, it simply
    /// stops parsing into an attributable completion, so the test measures an
    /// empty set and reads as a logic bug in the engine. That is what held
    /// `test (windows-latest)` red from 2026-07-26.
    fn done_payload(completed: &str, transcript_path: &str) -> String {
        serde_json::json!({
            "completed": completed,
            "client": "claude-code",
            "transcript_path": transcript_path,
        })
        .to_string()
    }

    fn sample(ts: &str, input: u64, output: u64) -> UsageSample {
        UsageSample {
            id: None,
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
    fn a_sample_inside_two_windows_is_banked_for_neither() {
        let samples = vec![sample("2026-07-25T09:47:00Z", 1000, 2000)];
        let foreign = vec![(
            "2026-07-25T09:46:53Z".to_string(),
            "2026-07-25T10:01:44Z".to_string(),
        )];
        let (totals, counted, contested) = totals_in_window_excluding(
            &samples,
            "2026-07-25T09:46:53Z",
            "2026-07-25T09:49:37Z",
            &foreign,
        );
        assert_eq!(counted, 0);
        assert_eq!(contested, 1);
        assert_eq!(totals.input, 0);
    }

    #[test]
    fn a_sample_outside_every_foreign_window_is_counted_normally() {
        let samples = vec![sample("2026-07-25T09:47:00Z", 1000, 2000)];
        let foreign = vec![(
            "2026-07-25T10:30:00Z".to_string(),
            "2026-07-25T10:45:00Z".to_string(),
        )];
        let (totals, counted, contested) = totals_in_window_excluding(
            &samples,
            "2026-07-25T09:46:53Z",
            "2026-07-25T09:49:37Z",
            &foreign,
        );
        assert_eq!((counted, contested), (1, 0));
        assert_eq!(totals.input, 1000);
    }

    #[test]
    fn an_unparseable_foreign_window_contests_nothing() {
        let samples = vec![sample("2026-07-25T09:47:00Z", 1000, 2000)];
        let foreign = vec![("not-a-time".to_string(), "2026-07-25T10:01:44Z".to_string())];
        let (_, counted, contested) = totals_in_window_excluding(
            &samples,
            "2026-07-25T09:46:53Z",
            "2026-07-25T09:49:37Z",
            &foreign,
        );
        assert_eq!((counted, contested), (1, 0));
    }

    #[test]
    fn no_foreign_windows_matches_totals_in_window_exactly() {
        let samples = vec![sample("2026-07-25T09:47:00Z", 1000, 2000)];
        let plain = totals_in_window(&samples, "2026-07-25T09:46:53Z", "2026-07-25T09:49:37Z");
        let (totals, counted, contested) = totals_in_window_excluding(
            &samples,
            "2026-07-25T09:46:53Z",
            "2026-07-25T09:49:37Z",
            &[],
        );
        assert_eq!((totals, counted), plain);
        assert_eq!(contested, 0);
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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
        };
        // Two days later the file is never coming: terminate with an empty marker
        // (found == false) rather than retrying — and forcing a rebuild — forever.
        let r = compute_attribution(&pa, ts("2026-07-26T11:05:00Z")).unwrap();
        assert!(!r.found);
        assert_eq!(r.samples, 0);
        assert_eq!(r.tool, "claude-code");
    }

    #[test]
    fn an_unreadable_transcript_retries_then_gives_up_on_the_same_deadline() {
        // A directory at the transcript path: `exists()` is true (so the absent
        // branch never fires) but every `std::fs::read` fails with EISDIR, exactly
        // like a root-owned session file or a `--transcript-path` typo pointing at
        // a folder. Before this test the unreadable case had NO cutoff at all: it
        // errored on every tick forever, and each error makes `attribution_tick`
        // return -1, which rebuilds the whole pending set twice a second for the
        // life of the daemon and never terminates the task.
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-unreadable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: Some(dir.to_string_lossy().into_owned()),
            session_id: None,
            otel_samples: Vec::new(),
            otel_tool: None,
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
        };

        // Minutes after completion: still transient. A file being written right
        // now can fail one read and succeed the next, so retry rather than burn
        // the task's only chance on a zero-sample marker.
        let err = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap_err();
        assert!(err.message.contains("failed to read"), "{}", err.message);

        // Two days later it is never becoming readable: terminate with an empty
        // marker, the same deadline the absent case already honoured.
        let r = compute_attribution(&pa, ts("2026-07-26T11:05:00Z")).unwrap();
        assert!(!r.found);
        assert_eq!(r.samples, 0);
        assert_eq!(r.tool, "claude-code");

        std::fs::remove_dir_all(&dir).ok();
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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
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

    /// #73: a transcript that parses fine but holds nothing in the window yet is
    /// TRANSIENT, on the same deadline as absent and unreadable.
    ///
    /// This is the case task #38 left open, and it is the common one rather than
    /// the exotic one. `tasqx done` runs *inside* the agent turn whose usage it
    /// wants to count, and that turn's usage line is not written until the turn
    /// ends — so for any task that lives entirely inside one turn, the transcript
    /// exists, parses, and is simply empty in the window at the only moment the
    /// old code looked. It then wrote `found: false`, and `has_attributed_event`
    /// made that marker terminal, so the real numbers could never replace it.
    /// Observed in the field: two of five tasks got `samples: 0` within one tick.
    #[test]
    fn a_transcript_with_nothing_in_the_window_yet_retries_then_gives_up() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-empty-window-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess-1.jsonl");
        // A readable, parseable transcript whose only usage line predates the
        // window — exactly the shape of a log the current turn has not flushed to
        // yet. Not empty and not malformed: the parser succeeds and returns a
        // sample, `totals_in_window` just excludes it.
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-07-24T09:00:00.000Z","message":{"id":"old","usage":{"input_tokens":10,"output_tokens":20}}}"#,
        )
        .unwrap();

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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
        };

        // Minutes after completion: the turn may still be writing. Retry rather
        // than burn the task's only chance on a zero that cannot be revised.
        let err = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap_err();
        assert!(
            err.message.contains("no usage in window"),
            "expected a transient empty-window error, got: {}",
            err.message
        );

        // Two days later nothing is coming: terminate with the empty marker, the
        // same deadline the other two unusable-transcript cases honour. Without
        // this the task never leaves the pending set and every tick rebuilds it.
        let r = compute_attribution(&pa, ts("2026-07-26T11:05:00Z")).unwrap();
        assert!(!r.found);
        assert_eq!(r.samples, 0);
        assert_eq!(r.tool, "claude-code");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D50: in-window samples exist, but every one is also claimed by another
    /// task's window — banked for no one, TRANSIENT on the same give-up
    /// deadline as the empty-window case. A distinct message ("contested", not
    /// "no usage") so daemon stderr tells the two apart.
    #[test]
    fn a_fully_contested_window_retries_then_gives_up_like_an_empty_one() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-contested-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess-1.jsonl");
        // One usage line inside pa's window — and inside the foreign window too.
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-07-24T10:10:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
        )
        .unwrap();

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
            self_reported: false,
            foreign_windows: vec![(
                "2026-07-24T09:50:00Z".to_string(),
                "2026-07-24T11:30:00Z".to_string(),
            )],
            consumed_sample_ids: HashSet::new(),
        };

        // Minutes after completion: transient — the contest may look different
        // once late-arriving lines land (mid-write timestamps are not monotonic).
        let err = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap_err();
        assert!(
            err.message.contains("contested"),
            "expected the distinct contested message, got: {}",
            err.message
        );

        // Two days later: give up exactly like the empty-window case — an empty
        // marker, never a measurement built from contested samples.
        let r = compute_attribution(&pa, ts("2026-07-26T11:05:00Z")).unwrap();
        assert!(!r.found);
        assert_eq!(r.samples, 0);
        assert_eq!(r.tool, "claude-code");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uncontested_samples_still_bank_when_a_neighbour_contests_others() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-partial-contest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sess-1.jsonl");
        // Two in-window lines: the 10:10 one is also inside the foreign window
        // (dropped), the 10:40 one is only in pa's window (banks).
        let content = [
            r#"{"timestamp":"2026-07-24T10:10:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
            r#"{"timestamp":"2026-07-24T10:40:00.000Z","message":{"id":"b","usage":{"input_tokens":100,"output_tokens":200}}}"#,
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
            self_reported: false,
            foreign_windows: vec![(
                "2026-07-24T10:05:00Z".to_string(),
                "2026-07-24T10:15:00Z".to_string(),
            )],
            consumed_sample_ids: HashSet::new(),
        };
        let r = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert!(r.found, "the uncontested remainder still banks");
        assert_eq!(r.samples, 1, "the contested line is never counted");
        assert_eq!(r.totals.input, 100);
        assert_eq!(r.totals.output, 200);
        assert_eq!(
            r.confidence, CONFIDENCE_HIGH,
            "an uncontested remainder keeps its earned confidence"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn otel_samples_claimed_by_a_foreign_window_are_refused_too() {
        // Refusal applies to the OTLP path with the same mechanism: OTLP samples
        // are keyed by session, so overlapping windows over one session would
        // double-count identically. The contested 10:15 sample drops; only the
        // uncontested 10:40 one banks.
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("claude-code".into()),
            transcript_path: None,
            session_id: Some("sess-1".into()),
            otel_samples: vec![
                sample("2026-07-24T10:15:00Z", 1000, 2000),
                sample("2026-07-24T10:40:00Z", 100, 200),
            ],
            otel_tool: Some("claude-code".into()),
            self_reported: false,
            foreign_windows: vec![(
                "2026-07-24T10:10:00Z".to_string(),
                "2026-07-24T10:20:00Z".to_string(),
            )],
            consumed_sample_ids: HashSet::new(),
        };
        let r = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert_eq!(r.source, SOURCE_OTEL);
        assert!(r.found);
        assert_eq!(r.samples, 1, "the contested telemetry sample is refused");
        assert_eq!(r.totals.input, 100);
        assert_eq!(r.totals.output, 200);
    }

    /// D50 symmetry: contested TELEMETRY stays transient like contested
    /// transcripts. The OTLP arm used to discard its contested count, so a
    /// fully-contested telemetry window fell through to the terminal empty
    /// paths — a permanent zero where the transcript path would have retried
    /// until the give-up deadline.
    #[test]
    fn a_fully_contested_otlp_window_retries_then_gives_up_like_a_transcript() {
        // Client with no parser: if the OTLP arm falls through, the result is
        // deterministically the terminal empty marker — which is exactly the
        // wrong outcome this test pins against.
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2026-07-24T10:00:00Z".into(),
            window_end: "2026-07-24T11:00:00Z".into(),
            client: Some("cursor".into()),
            transcript_path: None,
            session_id: Some("sess-1".into()),
            otel_samples: vec![sample("2026-07-24T10:15:00Z", 1000, 2000)],
            otel_tool: Some("cursor".into()),
            self_reported: false,
            foreign_windows: vec![(
                "2026-07-24T10:10:00Z".to_string(),
                "2026-07-24T10:20:00Z".to_string(),
            )],
            consumed_sample_ids: HashSet::new(),
        };

        // Minutes after completion: transient, with the distinct message.
        let err = compute_attribution(&pa, ts("2026-07-24T11:05:00Z")).unwrap_err();
        assert!(
            err.message.contains("contested"),
            "expected the contested-transient error, got: {}",
            err.message
        );

        // Two days later: give up and terminate with the empty marker.
        let r = compute_attribution(&pa, ts("2026-07-26T11:05:00Z")).unwrap();
        assert!(!r.found);
        assert_eq!(r.samples, 0);
    }

    /// The other half of #73's boundary: DISCOVERY finding nothing stays
    /// terminal. Only an explicit `transcript_path` earns the retry.
    ///
    /// The flush race is a property of a named file we were told to read: we know
    /// which log the tokens will land in, so "not yet" is a real answer. A
    /// discovery scan that came back empty has no such anchor — it means no
    /// candidate file in the tool's default roots carried anything for this
    /// window, and retrying that on every tick would keep every unattributable
    /// task in the pending set until the deadline for no gain.
    ///
    /// # The window is in 2020 on purpose
    ///
    /// This is the DISCOVERY branch, so it scans the tool's default roots —
    /// which on a developer's machine is their real `~/.claude`, full of live
    /// transcripts. The window used to be a recent date, and on any machine that
    /// had actually used Claude Code in that hour the scan found real samples
    /// and `found` was true: the test failed on the maintainer's box and passed
    /// on CI, where no such data exists.
    ///
    /// A test whose result depends on the developer's own history is not a test.
    /// 2020 is the fix the sibling `contested_discovery_*` already uses and for
    /// the same reason — no real transcript can carry samples there — and it is
    /// better than overriding `CLAUDE_CONFIG_DIR`, which needs `unsafe`, mutates
    /// process-global state, and races the other tests in this binary.
    #[test]
    fn discovery_finding_nothing_stays_terminal_rather_than_retrying() {
        // Keeps the sibling's `$CLAUDE_CONFIG_DIR` override — whose planted
        // transcript sits in this very window — out of this scan. See
        // [`DISCOVERY_ENV`].
        let _guard = DISCOVERY_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2020-01-01T10:00:00Z".into(),
            window_end: "2020-01-01T11:00:00Z".into(),
            client: Some("claude-code".into()),
            // No explicit path: this is the discovery branch.
            transcript_path: None,
            session_id: None,
            otel_samples: Vec::new(),
            otel_tool: None,
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
        };
        let r = compute_attribution(&pa, ts("2020-01-01T11:05:00Z"))
            .expect("discovery must terminate, not retry");
        assert!(!r.found);
        assert_eq!(r.samples, 0);
    }

    /// Discovery doctrine holds even under contest: a DISCOVERY scan whose
    /// only in-window samples are contested terminates on the first tick, like
    /// every other discovery outcome. The contested-transient retry belongs to
    /// the explicit-transcript path alone — without the gate, one contested
    /// discovery sample flipped discovery from terminal-on-first-tick to a 24h
    /// retry, keeping the task in the pending set for a day against the
    /// deliberate one-shot doctrine.
    #[test]
    fn contested_discovery_samples_stay_terminal_rather_than_retrying() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-contested-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // A transcript under a CLAUDE_CONFIG_DIR override, so discovery finds
        // it. The window is in 2020 — no real transcript can hold samples
        // there, so the planted (contested) sample is the only in-window one.
        let proj = dir.join("projects").join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess-1.jsonl"),
            r#"{"timestamp":"2020-01-01T10:10:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
        )
        .unwrap();

        let pa = PendingAttribution {
            task_id: "t".into(),
            short_id: 1,
            window_start: "2020-01-01T10:00:00Z".into(),
            window_end: "2020-01-01T11:00:00Z".into(),
            client: Some("claude-code".into()),
            // No explicit path: this is the discovery branch.
            transcript_path: None,
            session_id: None,
            otel_samples: Vec::new(),
            otel_tool: None,
            self_reported: false,
            foreign_windows: vec![(
                "2020-01-01T10:05:00Z".to_string(),
                "2020-01-01T10:15:00Z".to_string(),
            )],
            consumed_sample_ids: HashSet::new(),
        };

        // Held across the whole override, so no other discovery scan in this
        // binary can see the planted root. See [`DISCOVERY_ENV`].
        let _guard = DISCOVERY_ENV.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: restored below, and [`DISCOVERY_ENV`] keeps the only other
        // discovery test out for the duration.
        let prev = std::env::var_os("CLAUDE_CONFIG_DIR");
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &dir) };
        // Minutes after completion — where the explicit path would retry.
        let r = compute_attribution(&pa, ts("2020-01-01T11:05:00Z"));
        match prev {
            Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);

        let r = r.expect("contested discovery must terminate, not retry");
        assert!(!r.found, "the contested sample banks for no one");
        assert_eq!(r.samples, 0);
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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
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
            self_reported: false,
            foreign_windows: vec![],
            consumed_sample_ids: HashSet::new(),
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
                    id: None,
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

    #[test]
    fn a_self_reported_completion_is_not_re_attributed_and_never_double_counted() {
        // The documented happy path: an agent completes a task self-reporting its
        // spend AND supplying correlation (a session id + transcript). The
        // self-report writes one `token_usage` row in the done transaction. Async
        // attribution then sees a correlated `done` with no marker — it MUST NOT
        // reconstruct a second measurement for the same window, or every report
        // would show double the tokens actually spent.
        let engine = Engine::open_in_memory().unwrap();
        let sid = engine.task_add(&json!({ "title": "t" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        engine
            .task_done(&json!({
                "ref": sid,
                "client": "claude-code",
                "session_id": "sess-77",
                "input_tokens": 1000,
                "output_tokens": 500,
            }))
            .unwrap();
        assert_eq!(
            count_rows(&engine, "SELECT COUNT(*) FROM token_usage"),
            1,
            "the self-report is the single measurement"
        );

        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(pending.len(), 1, "correlated completion is pending");
        let pa = &pending[0];
        assert!(
            pa.self_reported,
            "the pending-set build flags the self-reported completion"
        );

        let r = compute_attribution(pa, ts("2026-07-24T11:05:00Z")).unwrap();
        assert!(
            !r.found,
            "no second measurement: the self-report is authoritative"
        );

        assert!(
            attribute_one(&engine, pa, &r).unwrap(),
            "the task is still terminated with a marker so it leaves the queue"
        );
        assert_eq!(
            count_rows(&engine, "SELECT COUNT(*) FROM token_usage"),
            1,
            "still exactly one measurement — no double count"
        );
        assert_eq!(
            count_rows(
                &engine,
                "SELECT COUNT(*) FROM events WHERE op = 'tokens.attributed'"
            ),
            1,
            "and one terminating marker, so it never re-enters the pending set"
        );
        // A second tick is a clean no-op: the marker now sits past the done.
        assert!(pending_attributions(&engine).unwrap().is_empty());
    }

    /// #13's gap, closed by D50's "one task never mixes channels": a
    /// self-report that arrives via `token.add` AFTER the completion (not in
    /// the done payload) must exclude log-parse exactly like a done-time
    /// self-report — otherwise async attribution reconstructs a second
    /// measurement for spend the caller already reported.
    #[test]
    fn a_token_add_self_report_after_done_also_excludes_log_parse() {
        let engine = Engine::open_in_memory().unwrap();
        let sid = engine.task_add(&json!({ "title": "t" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        // Completed with correlation but WITHOUT done-time token counts, so the
        // done payload carries no `tokens` key.
        engine
            .task_done(&json!({ "ref": sid, "client": "claude-code", "session_id": "sess-9" }))
            .unwrap();
        engine
            .token_add(&json!({
                "ref": sid,
                "source": SOURCE_SELF_REPORT,
                "tool": "claude-code",
                "confidence": "medium",
                "input_tokens": 500,
                "output_tokens": 100,
            }))
            .unwrap();

        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "still carried through the queue so it receives its marker"
        );
        assert!(
            pending[0].self_reported,
            "a stored self-report row excludes log-parse like a done-time one"
        );
        // And compute honours the flag: marker only, no second measurement.
        let r = compute_attribution(&pending[0], ts("2026-07-24T11:05:00Z")).unwrap();
        assert!(!r.found);
    }

    #[test]
    fn a_pending_task_carries_the_windows_of_attributed_neighbours_on_the_same_transcript() {
        // Task A: started 09:46:53, done 10:01:44, transcript /tmp/t.jsonl,
        // ALREADY attributed (its `tokens.attributed` marker landed on an
        // earlier tick, so it has left the queue). Task B: started 09:46:53,
        // done 09:49:37, same transcript, unattributed. B must still see A's
        // window as foreign — co-pending entries alone are race-dependent.
        let engine = Engine::open_in_memory().unwrap();
        let a = engine.task_add(&json!({ "title": "a" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        let b = engine.task_add(&json!({ "title": "b" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        engine.task_start(&json!({ "ref": a })).unwrap();
        engine
            .task_done(&json!({
                "ref": a, "client": "claude-code", "transcript_path": "/tmp/t.jsonl"
            }))
            .unwrap();
        engine.task_start(&json!({ "ref": b })).unwrap();
        engine
            .task_done(&json!({
                "ref": b, "client": "claude-code", "transcript_path": "/tmp/t.jsonl"
            }))
            .unwrap();
        engine
            .token_attribute(&json!({
                "ref": a, "source": SOURCE_LOG_PARSE, "tool": "claude-code", "confidence": "low"
            }))
            .unwrap();

        // Pin the windows to the field-observed instants so the assertion is
        // exact rather than clock-dependent.
        let a_id: String = engine
            .conn()
            .query_row("SELECT id FROM tasks WHERE short_id = ?1", [a], |r| {
                r.get(0)
            })
            .unwrap();
        let b_id: String = engine
            .conn()
            .query_row("SELECT id FROM tasks WHERE short_id = ?1", [b], |r| {
                r.get(0)
            })
            .unwrap();
        for (id, started, done_payload) in [
            (
                &a_id,
                "2026-07-25T09:46:53Z",
                r#"{"completed":"2026-07-25T10:01:44Z","client":"claude-code","transcript_path":"/tmp/t.jsonl"}"#,
            ),
            (
                &b_id,
                "2026-07-25T09:46:53Z",
                r#"{"completed":"2026-07-25T09:49:37Z","client":"claude-code","transcript_path":"/tmp/t.jsonl"}"#,
            ),
        ] {
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
                    (format!(r#"{{"interval_started":"{started}"}}"#), id),
                )
                .unwrap();
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
                    (done_payload, id),
                )
                .unwrap();
        }

        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(pending.len(), 1, "A is attributed; only B is pending");
        let pa = &pending[0];
        assert_eq!(pa.short_id, b);
        assert_eq!(
            pa.foreign_windows,
            vec![(
                "2026-07-25T09:46:53Z".to_string(),
                "2026-07-25T10:01:44Z".to_string()
            )],
            "B carries A's window even though A already left the queue"
        );
    }

    #[test]
    fn tasks_on_different_transcripts_and_sessions_do_not_contest_each_other() {
        let engine = Engine::open_in_memory().unwrap();
        let a = engine.task_add(&json!({ "title": "a" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        let b = engine.task_add(&json!({ "title": "b" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        engine
            .task_done(&json!({
                "ref": a,
                "client": "claude-code",
                "transcript_path": "/tmp/a.jsonl",
                "session_id": "sess-a",
            }))
            .unwrap();
        engine
            .task_done(&json!({
                "ref": b,
                "client": "claude-code",
                "transcript_path": "/tmp/b.jsonl",
                "session_id": "sess-b",
            }))
            .unwrap();
        engine
            .token_attribute(&json!({
                "ref": a, "source": SOURCE_LOG_PARSE, "tool": "claude-code", "confidence": "low"
            }))
            .unwrap();

        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].short_id, b);
        assert!(
            pending[0].foreign_windows.is_empty(),
            "no shared transcript or session: nothing contests"
        );
    }

    /// Two spellings of one file — `dir/t.jsonl` and `dir/./t.jsonl` — parse
    /// the same bytes, but a string comparison of `transcript_path` said they
    /// shared nothing, so fully overlapping windows banked the same spend
    /// twice in one tick. Paths must be compared by identity, not spelling.
    #[test]
    fn an_aliased_transcript_path_still_contests_the_same_file() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("sess-1.jsonl");
        let canonical = transcript.to_string_lossy().into_owned();
        let aliased = format!("{}/./sess-1.jsonl", dir.to_string_lossy());
        std::fs::write(
            &transcript,
            r#"{"timestamp":"2026-07-25T10:10:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
        )
        .unwrap();

        let engine = Engine::open_in_memory().unwrap();
        let a = engine.task_add(&json!({ "title": "A" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        let b = engine.task_add(&json!({ "title": "B" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        for (sid, p) in [(a, &canonical), (b, &aliased)] {
            engine.task_start(&json!({ "ref": sid })).unwrap();
            engine
                .task_done(&json!({ "ref": sid, "client": "claude-code", "transcript_path": p }))
                .unwrap();
        }
        let task_id = |sid: i64| -> String {
            engine
                .conn()
                .query_row("SELECT id FROM tasks WHERE short_id = ?1", [sid], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        // Fully overlapping windows [10:00, 10:30] on both tasks.
        for (id, p) in [(task_id(a), &canonical), (task_id(b), &aliased)] {
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
                    (r#"{"interval_started":"2026-07-25T10:00:00Z"}"#, &id),
                )
                .unwrap();
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
                    (done_payload("2026-07-25T10:30:00Z", p), &id),
                )
                .unwrap();
        }

        let now = ts("2026-07-25T10:35:00Z");
        for pa in &pending_attributions(&engine).unwrap() {
            assert_eq!(
                pa.foreign_windows.len(),
                1,
                "task #{} must see its alias-spelled neighbour as foreign",
                pa.short_id
            );
            if let Ok(r) = compute_attribution(pa, now) {
                attribute_one(&engine, pa, &r).unwrap();
            }
        }
        let tasks_billed = count_rows(
            &engine,
            "SELECT COUNT(DISTINCT task_id) FROM token_usage WHERE source='log-parse'",
        );
        assert!(
            tasks_billed < 2,
            "one spend was billed to {tasks_billed} tasks via path spelling"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The TOCTOU half of "one task never mixes channels": the pending set is
    /// built, the tick drops the lock to parse the transcript, and the agent's
    /// self-report lands via `token.add` in that gap — exactly the flow the
    /// `tokens_hint` nudge encourages. The stale `PendingAttribution` still
    /// says `self_reported: false`, so the write seam itself must re-check
    /// inside its transaction and suppress the log-parse row.
    #[test]
    fn a_self_report_landing_mid_tick_suppresses_the_log_parse_write() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-selfreport-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("sess-1.jsonl");
        let path = transcript.to_string_lossy().into_owned();

        let engine = Engine::open_in_memory().unwrap();
        let sid = engine.task_add(&json!({ "title": "t" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        engine.task_start(&json!({ "ref": sid })).unwrap();
        // Completed with correlation but WITHOUT done-time token counts.
        engine
            .task_done(&json!({ "ref": sid, "client": "claude-code", "transcript_path": path }))
            .unwrap();
        let id: String = engine
            .conn()
            .query_row("SELECT id FROM tasks WHERE short_id = ?1", [sid], |r| {
                r.get(0)
            })
            .unwrap();
        engine
            .conn()
            .execute(
                "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
                (r#"{"interval_started":"2026-07-25T10:00:00Z"}"#, &id),
            )
            .unwrap();
        engine
            .conn()
            .execute(
                "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
                (done_payload("2026-07-25T10:30:00Z", &path), &id),
            )
            .unwrap();
        std::fs::write(
            &transcript,
            r#"{"timestamp":"2026-07-25T10:10:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
        )
        .unwrap();

        // Tick: pending set and result built BEFORE the self-report exists.
        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].self_reported, "flag captured before the race");
        let r = compute_attribution(&pending[0], ts("2026-07-25T10:35:00Z")).unwrap();
        assert!(r.found, "log-parse found the spend");

        // The self-report lands while the tick is off parsing the file.
        engine
            .token_add(&json!({
                "ref": sid,
                "source": SOURCE_SELF_REPORT,
                "tool": "claude-code",
                "confidence": "medium",
                "input_tokens": 1000,
                "output_tokens": 2000,
            }))
            .unwrap();

        // The tick resumes with its stale result: the write seam must refuse
        // the second channel — marker written, measurement suppressed.
        assert!(attribute_one(&engine, &pending[0], &r).unwrap());
        assert_eq!(
            count_rows(&engine, "SELECT COUNT(*) FROM token_usage"),
            1,
            "exactly one measurement survives the race"
        );
        let source: String = engine
            .conn()
            .query_row("SELECT source FROM token_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, SOURCE_SELF_REPORT, "and it is the self-report");
        assert_eq!(
            count_rows(
                &engine,
                "SELECT COUNT(*) FROM events WHERE op = 'tokens.attributed'"
            ),
            1,
            "the task still terminates with its marker"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The adversary's cross-tick theft: banked decisions are final, but
    /// contestedness used to be re-judged each tick against re-parsed stamps —
    /// and the Claude Code parser keeps the LAST occurrence's timestamp for a
    /// deduped message id, so a streamed re-emission can move a sample out of
    /// the banked task's window and into a neighbour's. Identity must decide:
    /// once B banked message `m`, A may never bank it, whatever its stamp
    /// currently reads.
    #[test]
    fn a_sample_banked_on_an_earlier_tick_is_refused_even_when_its_stamp_moves() {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-attr-moving-stamp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("sess-1.jsonl");
        let path = transcript.to_string_lossy().into_owned();

        // Two tasks on one transcript: A [10:00, 10:05], B [10:04, 10:30].
        let engine = Engine::open_in_memory().unwrap();
        let a = engine.task_add(&json!({ "title": "A" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        let b = engine.task_add(&json!({ "title": "B" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        for sid in [a, b] {
            engine.task_start(&json!({ "ref": sid })).unwrap();
            engine
                .task_done(&json!({ "ref": sid, "client": "claude-code", "transcript_path": path }))
                .unwrap();
        }
        let task_id = |sid: i64| -> String {
            engine
                .conn()
                .query_row("SELECT id FROM tasks WHERE short_id = ?1", [sid], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        for (id, started, completed) in [
            (task_id(a), "2026-07-25T10:00:00Z", "2026-07-25T10:05:00Z"),
            (task_id(b), "2026-07-25T10:04:00Z", "2026-07-25T10:30:00Z"),
        ] {
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
                    (format!(r#"{{"interval_started":"{started}"}}"#), &id),
                )
                .unwrap();
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
                    (done_payload(completed, &path), &id),
                )
                .unwrap();
        }
        let now = ts("2026-07-25T10:35:00Z");

        // Tick N: message `m` reads stamp 10:06 — inside B only, uncontested,
        // so B banks it; A is transient ("no usage in window yet").
        std::fs::write(
            &transcript,
            r#"{"timestamp":"2026-07-25T10:06:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
        )
        .unwrap();
        let mut banked = 0;
        for pa in &pending_attributions(&engine).unwrap() {
            if let Ok(r) = compute_attribution(pa, now) {
                assert_eq!(pa.short_id, b, "only B can bank at tick N");
                assert!(r.found);
                banked += 1;
                attribute_one(&engine, pa, &r).unwrap();
            }
        }
        assert_eq!(banked, 1, "B banked m at tick N");

        // Between ticks: streaming re-emits `m` stamped EARLIER (10:03 — the
        // non-monotonic mid-write case). Dedupe keeps the last occurrence, so
        // the deduped stamp now reads inside A's window and outside B's.
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(f).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-25T10:03:00.000Z","message":{{"id":"m","usage":{{"input_tokens":1000,"output_tokens":2000}}}}}}"#
        )
        .unwrap();

        // Tick N+1: A must NOT bank m — B already consumed it, by identity.
        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(pending.len(), 1, "only A is still pending");
        assert_eq!(pending[0].short_id, a);
        if let Ok(r) = compute_attribution(&pending[0], now) {
            assert!(
                !r.found,
                "A banked the spend B already holds: input={}",
                r.totals.input
            );
            attribute_one(&engine, &pending[0], &r).unwrap();
        }
        let tasks_billed = count_rows(
            &engine,
            "SELECT COUNT(DISTINCT task_id) FROM token_usage WHERE source='log-parse'",
        );
        assert_eq!(tasks_billed, 1, "one spend is never billed to two tasks");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The symlink-vanish theft: B banks message `m` under a symlink spelling
    /// of the transcript; the symlink is deleted between ticks; streaming
    /// re-emits `m` with an earlier stamp inside A's window. If the consumed-id
    /// set is joined through source identity, B's claim dissolves with the
    /// symlink (canonicalize fails, the lexical fallback yields a different
    /// identity) and A re-banks the claimed spend. Identity claims must be
    /// GLOBAL: a claimed sample id is refused store-wide, whatever the current
    /// filesystem says about the path it was banked under.
    #[cfg(unix)]
    #[test]
    fn a_claimed_sample_id_is_refused_store_wide_even_when_its_source_path_dissolves() {
        let base = std::env::temp_dir().join(format!(
            "tasqx-attr-symlink-vanish-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let lnk = base.join("lnk");
        std::os::unix::fs::symlink(&real, &lnk).unwrap();

        let transcript = real.join("sess-1.jsonl");
        let a_path = transcript.to_string_lossy().into_owned();
        let b_path = lnk.join("sess-1.jsonl").to_string_lossy().into_owned();

        let engine = Engine::open_in_memory().unwrap();
        let a = engine.task_add(&json!({ "title": "A" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        let b = engine.task_add(&json!({ "title": "B" })).unwrap()["short_id"]
            .as_i64()
            .unwrap();
        for (sid, p) in [(a, &a_path), (b, &b_path)] {
            engine.task_start(&json!({ "ref": sid })).unwrap();
            engine
                .task_done(&json!({ "ref": sid, "client": "claude-code", "transcript_path": p }))
                .unwrap();
        }
        let task_id = |sid: i64| -> String {
            engine
                .conn()
                .query_row("SELECT id FROM tasks WHERE short_id = ?1", [sid], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        // A [10:00, 10:05] on the real spelling, B [10:04, 10:30] on the
        // symlink spelling.
        for (id, started, completed, p) in [
            (
                task_id(a),
                "2026-07-25T10:00:00Z",
                "2026-07-25T10:05:00Z",
                &a_path,
            ),
            (
                task_id(b),
                "2026-07-25T10:04:00Z",
                "2026-07-25T10:30:00Z",
                &b_path,
            ),
        ] {
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
                    (format!(r#"{{"interval_started":"{started}"}}"#), &id),
                )
                .unwrap();
            engine
                .conn()
                .execute(
                    "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
                    (done_payload(completed, p), &id),
                )
                .unwrap();
        }
        let now = ts("2026-07-25T10:35:00Z");

        // Tick N: `m` reads stamp 10:06 — inside B only, so B banks it (with
        // its sample id persisted in the marker); A is transient.
        std::fs::write(
            &transcript,
            r#"{"timestamp":"2026-07-25T10:06:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
        )
        .unwrap();
        let mut banked = 0;
        for pa in &pending_attributions(&engine).unwrap() {
            if let Ok(r) = compute_attribution(pa, now) {
                assert_eq!(pa.short_id, b, "only B can bank at tick N");
                assert!(r.found);
                banked += 1;
                attribute_one(&engine, pa, &r).unwrap();
            }
        }
        assert_eq!(banked, 1, "B banked m at tick N");

        // Between ticks: the symlink vanishes (B's banked spelling no longer
        // resolves) and streaming re-emits `m` stamped earlier, inside A's
        // window and outside B's.
        std::fs::remove_file(&lnk).unwrap();
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(f).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-07-25T10:03:00.000Z","message":{{"id":"m","usage":{{"input_tokens":1000,"output_tokens":2000}}}}}}"#
        )
        .unwrap();

        // Tick N+1: A must NOT bank m — B's identity claim is global and does
        // not dissolve with the dangling symlink.
        let pending = pending_attributions(&engine).unwrap();
        assert_eq!(pending.len(), 1, "only A is still pending");
        assert_eq!(pending[0].short_id, a);
        assert!(
            pending[0].consumed_sample_ids.contains("m"),
            "A must carry B's claim on m even though B's spelling stopped resolving"
        );
        if let Ok(r) = compute_attribution(&pending[0], now) {
            assert!(
                !r.found,
                "A banked the spend B already holds: input={}",
                r.totals.input
            );
            attribute_one(&engine, &pending[0], &r).unwrap();
        }
        let tasks_billed = count_rows(
            &engine,
            "SELECT COUNT(DISTINCT task_id) FROM token_usage WHERE source='log-parse'",
        );
        assert_eq!(
            tasks_billed, 1,
            "a dangling symlink dissolved the banked claim"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    fn count_rows(engine: &Engine, sql: &str) -> i64 {
        engine.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }
}
