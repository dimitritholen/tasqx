//! Typed internal commands and results for simple task lifecycle mutations.
//!
//! JSON is still the frozen public wire contract. These types begin after that
//! boundary so lifecycle policy cannot confuse a `ref` with an unrelated value
//! or reconstruct response keys ad hoc in every branch.

use super::*;

pub(super) struct TaskTarget {
    pub(super) value: Value,
}

pub(super) struct StartTask {
    pub(super) target: TaskTarget,
    pub(super) keep: bool,
    pub(super) correlation: Correlation,
}

/// Correlation metadata captured at the moment work starts or completes
/// (docs/research/token-accounting.md, #12): which agent session did it, in
/// which transcript the tokens will be found. Stored ONLY in the start/done
/// event payloads — the events table is already the durable per-occurrence
/// record with its own timestamp, and a task column could hold one value
/// where a task can start and finish many times over its life. All optional:
/// a human's `tasqx done 4` has none of these, and that must stay a one-word
/// command.
pub(super) struct Correlation {
    pub(super) session_id: Option<String>,
    pub(super) prompt_id: Option<String>,
    pub(super) transcript_path: Option<String>,
    pub(super) client: Option<String>,
}

impl Correlation {
    /// Widen an event payload with whichever correlation facts were supplied.
    /// Present keys only — event payloads are read tolerantly, and a `null`
    /// on every human-issued start/done would be noise the attribution
    /// engine then has to skip anyway.
    pub(super) fn apply(&self, payload: &mut Value) {
        for (key, value) in [
            ("session_id", &self.session_id),
            ("prompt_id", &self.prompt_id),
            ("transcript_path", &self.transcript_path),
            ("client", &self.client),
        ] {
            if let Some(v) = value {
                payload[key] = json!(v);
            }
        }
    }
}

pub(super) fn parse_task_target(p: &Value) -> Result<TaskTarget, ApiError> {
    Ok(TaskTarget {
        value: ref_param(p)?.clone(),
    })
}

pub(super) fn parse_start_task(p: &Value) -> Result<StartTask, ApiError> {
    Ok(StartTask {
        target: parse_task_target(p)?,
        keep: opt_bool(p, "keep")?.unwrap_or(false),
        correlation: parse_correlation(p)?,
    })
}

pub(super) fn parse_correlation(p: &Value) -> Result<Correlation, ApiError> {
    Ok(Correlation {
        session_id: opt_str_nonempty(p, "session_id")?,
        prompt_id: opt_str_nonempty(p, "prompt_id")?,
        transcript_path: opt_str_nonempty(p, "transcript_path")?,
        client: opt_str_nonempty(p, "client")?,
    })
}

/// Self-reported token usage on `task.done` (#13) — since D50 the primary
/// channel: the caller is the only party that knows which task a turn's spend
/// served, so log-parse and telemetry are fallbacks. Everything optional: a
/// completion without counts is still accepted (it earns a `tokens_hint`).
pub(super) struct SelfReport {
    pub(super) tool: Option<String>,
    pub(super) model: Option<String>,
    pub(super) input_tokens: Option<i64>,
    pub(super) output_tokens: Option<i64>,
    pub(super) cache_read_tokens: Option<i64>,
    pub(super) cache_creation_tokens: Option<i64>,
}

pub(super) fn parse_self_report(p: &Value) -> Result<SelfReport, ApiError> {
    Ok(SelfReport {
        tool: opt_str_nonempty(p, "tool")?,
        model: opt_str_nonempty(p, "model")?,
        input_tokens: tokens::opt_token_count(p, "input_tokens")?,
        output_tokens: tokens::opt_token_count(p, "output_tokens")?,
        cache_read_tokens: tokens::opt_token_count(p, "cache_read_tokens")?,
        cache_creation_tokens: tokens::opt_token_count(p, "cache_creation_tokens")?,
    })
}

impl SelfReport {
    /// The measurement this report asks for, or `None` when no token count
    /// was given. Policy decided here, once:
    ///
    ///  * ANY present count makes a measurement, absent counts default to 0 —
    ///    a tool that only knows its output tokens still reports honestly;
    ///  * `tool`/`model` WITHOUT any count is refused rather than silently
    ///    dropped (the D33 rule: a value that changes nothing must not answer
    ///    `ok`);
    ///  * the attributed tool falls back to the correlation `client` (which
    ///    the MCP server injects from clientInfo), and with neither present
    ///    the report is refused — a measurement attributed to nobody cannot
    ///    be reported per tool later, which is the whole point of recording it.
    pub(super) fn into_usage(
        self,
        correlation: &Correlation,
    ) -> Result<Option<tokens::NewTokenUsage>, ApiError> {
        let any_count = self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_creation_tokens.is_some();
        if !any_count {
            // No counts, no measurement — a zero-count `token_usage` row is a
            // phantom every later sum treats as real, which is worse than none.
            //
            // `tool`/`model` without a count used to be REFUSED here, on the
            // D33 rule that a value which changes nothing must not answer `ok`.
            // The rule was right and applied to the wrong half: an agent does
            // not know its own token spend, so the refusal read "supply a
            // number you cannot observe, or forfeit the tool and model you
            // can", and callers took the second option. `task.done` now writes
            // both onto the completion event (D65), so the value changes
            // something and the D33 objection is answered rather than waived.
            return Ok(None);
        }
        let tool = self
            .tool
            .or_else(|| correlation.client.clone())
            .ok_or_else(|| {
                ApiError::bad_request(
                    "self-reported token counts need a `tool` (or `client`) naming the AI tool \
                     that spent them — over MCP the server fills `client` in from clientInfo",
                )
            })?;
        Ok(Some(tokens::NewTokenUsage {
            tool,
            source: crate::tokens::SOURCE_SELF_REPORT.to_string(),
            model: self.model,
            input_tokens: self.input_tokens.unwrap_or(0),
            output_tokens: self.output_tokens.unwrap_or(0),
            cache_read_tokens: self.cache_read_tokens.unwrap_or(0),
            cache_creation_tokens: self.cache_creation_tokens.unwrap_or(0),
            confidence: crate::tokens::CONFIDENCE_MEDIUM.to_string(),
        }))
    }
}

pub(super) struct TaskStarted {
    pub(super) id: String,
    pub(super) interval_started: Option<String>,
}

impl From<TaskStarted> for Value {
    fn from(result: TaskStarted) -> Self {
        json!({
            "id": result.id,
            "status": "active",
            "interval_started": result.interval_started,
        })
    }
}

pub(super) struct TaskStopped {
    pub(super) tracked: String,
}

impl From<TaskStopped> for Value {
    fn from(result: TaskStopped) -> Self {
        json!({ "status": "pending", "tracked": result.tracked })
    }
}

pub(super) struct TaskCancelled {
    pub(super) short_id: i64,
    pub(super) unblocked: Vec<i64>,
}

impl From<TaskCancelled> for Value {
    fn from(result: TaskCancelled) -> Self {
        json!({
            "short_id": result.short_id,
            "status": "cancelled",
            "unblocked": result.unblocked,
        })
    }
}

pub(super) struct TaskReopened {
    pub(super) short_id: i64,
    /// short_ids of the open dependents this reopen put back into `blocked`.
    ///
    /// The inverse of [`TaskDone::unblocked`], and it exists for the same
    /// reason: a caller that reopens a task has just changed which work is
    /// actionable, and used to be told nothing about it (D69).
    pub(super) blocked: Vec<i64>,
}

impl From<TaskReopened> for Value {
    fn from(result: TaskReopened) -> Self {
        json!({
            "short_id": result.short_id,
            "status": "pending",
            "blocked": result.blocked,
        })
    }
}
