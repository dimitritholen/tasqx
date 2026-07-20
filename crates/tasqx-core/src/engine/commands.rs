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
