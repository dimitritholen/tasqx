//! Shared types and vocabularies for per-task AI token accounting
//! (docs/research/token-accounting.md).
//!
//! This module is deliberately pure: no SQL, no parsing of any tool's log
//! format. The engine-side writer/readers live in `engine::tokens`; the
//! per-tool transcript parsers of the later phases will produce
//! [`UsageSample`]s and depend on nothing but this file, so a parser can be
//! unit-tested without a store.
//!
//! Token counts are ALWAYS four separate fields — input, output, cache-read,
//! cache-creation — never a blended total: cache tokens cost a fraction of
//! fresh ones and every tool defines them differently (research rule #5), so
//! a single number destroys exactly the information a cost report needs.

// Per-tool transcript parsers. Keep this list alphabetical to minimize merge
// conflicts as sibling parsers land in parallel.
pub mod claude_code;
pub mod codex;
pub mod copilot;
pub mod gemini;

use crate::error::ApiError;

/// Read out of the tool's own on-disk transcript by the daemon's asynchronous
/// attribution, after the task was completed ([`crate::attribution`]). Nobody
/// had to be trusted for the numbers, but they reached this task by falling
/// inside its time window, so the edges are only as sharp as that window.
pub const SOURCE_LOG_PARSE: &str = "log-parse";

/// Arrived over the opt-in local OTLP receiver ([`crate::otlp`], `[otlp]
/// enabled`, off by default) and was matched to the task by session id.
/// Outranks [`SOURCE_LOG_PARSE`] when telemetry for the task's session is
/// buffered: the same per-request numbers, with no transcript-file hunting in
/// between.
pub const SOURCE_OTEL: &str = "otel";

/// The agent supplied the counts itself on `task.done`. The last resort for
/// tools that write neither a local transcript nor telemetry (Cursor), and the
/// only source nothing can check — hence always stored at
/// [`CONFIDENCE_MEDIUM`].
pub const SOURCE_SELF_REPORT: &str = "self-report";

/// Where a stored measurement came from. Closed vocabulary (D34): each source
/// implies a different trust story, and reporting needs to tell them apart
/// forever. `tool` stays free-form on purpose — new coding agents appear faster
/// than tasqx releases — but a *source* is a tasqx mechanism, and there are
/// exactly as many as tasqx implements. [`require_source`] gates every write
/// door against this array.
pub const TOKEN_SOURCES: [&str; 3] = [SOURCE_LOG_PARSE, SOURCE_OTEL, SOURCE_SELF_REPORT];

/// The samples provably belong to this task: an explicit `transcript_path` was
/// parsed *and* the completion's session id was confirmed against that file, or
/// buffered OTLP samples matched the session id outright. See
/// `attribution::confidence_for` for the exact rule.
pub const CONFIDENCE_HIGH: &str = "high";

/// Plausible but unproven — either an agent's self-report, or an explicit
/// transcript that parsed without its session id being confirmed (Gemini and
/// Copilot files carry no per-session anchor, so they can never do better than
/// this even with a path in hand).
pub const CONFIDENCE_MEDIUM: &str = "medium";

/// No anchor at all: no `transcript_path` was supplied, so the transcript was
/// *discovered* by scanning the tool's default roots and the tokens reached
/// this task on time-window overlap alone.
pub const CONFIDENCE_LOW: &str = "low";

/// How much to trust a measurement. Closed vocabulary (D34), same reasoning as
/// [`TOKEN_SOURCES`]. What is being graded is the *correlation* — how firmly
/// the tokens are tied to this particular task — not the precision of the
/// counts, which are per-request readings at every grade.
pub const TOKEN_CONFIDENCE: [&str; 3] = [CONFIDENCE_HIGH, CONFIDENCE_MEDIUM, CONFIDENCE_LOW];

/// Refuse a `source` outside [`TOKEN_SOURCES`], naming the value and the
/// accepted set (the house rule for every closed-vocabulary refusal).
pub fn require_source(value: &str) -> Result<(), ApiError> {
    if TOKEN_SOURCES.contains(&value) {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "unknown token source {value:?} (accepted: {})",
        TOKEN_SOURCES.join(", ")
    )))
}

/// Refuse a `confidence` outside [`TOKEN_CONFIDENCE`], same contract as
/// [`require_source`].
pub fn require_confidence(value: &str) -> Result<(), ApiError> {
    if TOKEN_CONFIDENCE.contains(&value) {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "unknown token confidence {value:?} (accepted: {})",
        TOKEN_CONFIDENCE.join(", ")
    )))
}

/// One timestamped per-request usage reading, as a transcript parser or OTLP
/// receiver produces it — *before* any task attribution. `ts` is RFC3339, so
/// samples can be bucketed into a task's time window later.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSample {
    /// The tool's own stable identity for the request this sample measures —
    /// Claude Code's assistant `message.id` — when the transcript carries one;
    /// `None` for parsers and receivers with no such anchor. Identity, not
    /// time: a streamed re-emission can move a deduped sample's *timestamp*
    /// across a window edge between daemon reads, but its id never changes, so
    /// attribution records the ids it consumed and refuses a sample another
    /// task already banked no matter what its current stamp says.
    pub id: Option<String>,
    /// When the measured request happened, RFC3339. Whatever a tool writes,
    /// every producer here re-emits it through jiff, so timestamps from
    /// different tools are comparable byte-for-byte. `attribution` re-parses
    /// this to decide whether the sample falls inside a task's window; a value
    /// it cannot parse is skipped rather than attributed.
    pub ts: String,
    /// The model that served the request, when the transcript names one.
    /// `None` when the tool does not record it, or has not yet at this point in
    /// the file (Codex learns the model from a preceding `turn_context` line).
    /// Persisted on the way through the OTLP buffer (`otlp_samples.model`), but
    /// never onto a measurement: attribution folds samples into
    /// [`TokenTotals`], which has no model, so a `token_usage` row names a
    /// model only when a self-report supplied one.
    pub model: Option<String>,
    /// Fresh, uncached input for this one request. Each parser translates its
    /// tool's numbers into that single meaning: where a tool reports cache
    /// reads as a *subset* of input, the cached portion is subtracted out here
    /// (see [`codex`] and [`copilot`]) so the bucket means the same thing
    /// whatever produced it.
    pub input_tokens: u64,
    /// Tokens generated, reasoning/"thinking" tokens included: every tool that
    /// breaks them out still bills them as output, so they are folded in here
    /// rather than dropped.
    pub output_tokens: u64,
    /// Input served from cache. Kept out of `input_tokens` because it costs a
    /// fraction of fresh input — collapsing the two is precisely the loss
    /// research rule #5 forbids.
    pub cache_read_tokens: u64,
    /// Input written *into* the cache. Always 0 for tools with no cache-write
    /// concept (Codex) and for tools that simply expose no counter for it
    /// (Gemini) — the field cannot tell either of those apart from a genuine
    /// zero.
    pub cache_creation_tokens: u64,
}

/// A four-way token roll-up. Saturating on purpose: totals are sums over
/// client-supplied numbers, and a roll-up must clamp rather than wrap
/// (the same rule `report.summary`'s duration aggregation follows).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    /// Fresh input, summed. Stored as `token_usage.input_tokens`, reported as
    /// the `tokens_in` summary metric.
    pub input: u64,
    /// Generated tokens, summed. Stored as `token_usage.output_tokens`,
    /// reported as `tokens_out`.
    pub output: u64,
    /// Cache reads, summed. Stored as `token_usage.cache_read_tokens`, reported
    /// as `tokens_cache_read`.
    pub cache_read: u64,
    /// Cache writes, summed. Stored as `token_usage.cache_creation_tokens`,
    /// reported as `tokens_cache_creation`.
    pub cache_creation: u64,
}

impl TokenTotals {
    /// Fold one sample's counts in.
    pub fn add_sample(&mut self, s: &UsageSample) {
        self.input = self.input.saturating_add(s.input_tokens);
        self.output = self.output.saturating_add(s.output_tokens);
        self.cache_read = self.cache_read.saturating_add(s.cache_read_tokens);
        self.cache_creation = self.cache_creation.saturating_add(s.cache_creation_tokens);
    }

    /// The blended grand total across all four buckets, saturating. Used only to
    /// answer "did we find anything?" — a measurement row is still stored as the
    /// four separate fields, never this number (research rule #5).
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both refusals must name the offending value AND the accepted set — the
    /// message is the fix, so it has to contain the right spellings.
    #[test]
    fn vocabulary_refusals_name_the_value_and_the_accepted_set() {
        let e = require_source("carrier-pigeon").unwrap_err();
        assert!(e.message.contains("carrier-pigeon"), "{}", e.message);
        for s in TOKEN_SOURCES {
            assert!(e.message.contains(s), "{} must list {s}", e.message);
            require_source(s).expect("every published source is accepted");
        }

        let e = require_confidence("absolute").unwrap_err();
        assert!(e.message.contains("absolute"), "{}", e.message);
        for c in TOKEN_CONFIDENCE {
            assert!(e.message.contains(c), "{} must list {c}", e.message);
            require_confidence(c).expect("every published confidence is accepted");
        }
    }

    #[test]
    fn totals_saturate_rather_than_wrap() {
        let mut t = TokenTotals {
            input: u64::MAX - 1,
            ..TokenTotals::default()
        };
        t.add_sample(&UsageSample {
            input_tokens: 10,
            ..UsageSample::default()
        });
        assert_eq!(t.input, u64::MAX);
    }
}
