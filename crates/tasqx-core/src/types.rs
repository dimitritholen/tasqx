//! Domain types and the API envelope (DESIGN.md §3 data model, §4 envelope).
//!
//! These are the serde structs that *are* the contract: the same types flow
//! over stdio one-shot and (later) the daemon socket. Timestamps are kept as
//! RFC3339 strings — the simplest representation that is correct and preserves
//! whatever offset a client sends (see `crate::util::now` for generation).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Task lifecycle states (DESIGN.md §3). Serialized as lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Exists but not yet actionable (`wait`/`scheduled` in the future).
    Backlog,
    /// Actionable, not started. The default working set.
    Pending,
    /// Currently being worked (has an open time interval).
    Active,
    /// Completed.
    Done,
    /// Abandoned; retained for history.
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Backlog => "backlog",
            Status::Pending => "pending",
            Status::Active => "active",
            Status::Done => "done",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "backlog" => Some(Status::Backlog),
            "pending" => Some(Status::Pending),
            "active" => Some(Status::Active),
            "done" => Some(Status::Done),
            "cancelled" => Some(Status::Cancelled),
            _ => None,
        }
    }
}

/// Task priority. Serialized as the single letters used across the CLI/API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    H,
    M,
    L,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::H => "H",
            Priority::M => "M",
            Priority::L => "L",
        }
    }

    /// Accepts `H`/`M`/`L` (any case) and the words high/medium/low.
    pub fn parse(s: &str) -> Option<Priority> {
        match s.trim().to_ascii_lowercase().as_str() {
            "h" | "high" => Some(Priority::H),
            "m" | "medium" | "med" => Some(Priority::M),
            "l" | "low" => Some(Priority::L),
            _ => None,
        }
    }
}

/// A project — hierarchy is expressed via dotted names (`work.api`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub archived: bool,
    pub created: String,
}

/// A tag. Many-to-many with tasks via the `task_tags` join.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub name: String,
}

/// The full task record as stored, mirroring the `tasks` table columns.
/// Output shapes for specific methods are built per-handler; this is the
/// canonical in-memory row used internally and for `task.list` rendering.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub short_id: i64,
    pub title: String,
    pub status: Status,
    pub priority: Option<Priority>,
    pub project: Option<String>,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub wait: Option<String>,
    pub estimate: Option<String>,
    pub recurrence: Option<String>,
    /// Reminder spec (§9), canonical form: a signed offset anchored to `due`
    /// (`-1h`) or an absolute RFC3339 instant. See [`crate::remind`].
    pub remind: Option<String>,
    pub urgency: f64,
    /// RFC3339 instant the current active interval began (None when not active).
    pub active_since: Option<String>,
    /// Accumulated tracked time across closed intervals, in seconds.
    pub tracked_seconds: i64,
    /// Per-task event counter (`_rev` in the API).
    pub rev: i64,
    pub created: String,
    pub modified: String,
    pub completed: Option<String>,
}

/// Request envelope (DESIGN.md §4). `params` and `id` are optional on stdio.
#[derive(Debug, Deserialize)]
pub struct ApiRequest {
    pub tasqx: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}
