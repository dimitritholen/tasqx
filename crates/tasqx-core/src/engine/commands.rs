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
}

impl From<TaskReopened> for Value {
    fn from(result: TaskReopened) -> Self {
        json!({ "short_id": result.short_id, "status": "pending" })
    }
}
