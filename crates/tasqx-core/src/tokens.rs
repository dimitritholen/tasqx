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
pub mod gemini;

use crate::error::ApiError;

/// Where a stored measurement came from. Closed vocabulary (D34): each source
/// implies a different trust story (`log-parse` = read from the tool's own
/// transcript, `otel` = received over the opt-in telemetry channel,
/// `self-report` = the agent said so on `task.done`), and reporting needs to
/// tell them apart forever. `tool` stays free-form on purpose — new coding
/// agents appear faster than tasqx releases — but a *source* is a tasqx
/// mechanism, and there are exactly as many as tasqx implements.
pub const SOURCE_LOG_PARSE: &str = "log-parse";
pub const SOURCE_OTEL: &str = "otel";
pub const SOURCE_SELF_REPORT: &str = "self-report";
pub const TOKEN_SOURCES: [&str; 3] = [SOURCE_LOG_PARSE, SOURCE_OTEL, SOURCE_SELF_REPORT];

/// How much to trust a measurement. Closed vocabulary (D34), same reasoning
/// as [`TOKEN_SOURCES`]: `high` = per-request samples bucketed into an exact
/// window, `medium` = plausible but unverifiable (an agent's self-report),
/// `low` = a whole-session number attributed by fuzzy time-window overlap.
pub const CONFIDENCE_HIGH: &str = "high";
pub const CONFIDENCE_MEDIUM: &str = "medium";
pub const CONFIDENCE_LOW: &str = "low";
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
    pub ts: String,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// A four-way token roll-up. Saturating on purpose: totals are sums over
/// client-supplied numbers, and a roll-up must clamp rather than wrap
/// (the same rule `report.summary`'s duration aggregation follows).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
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
