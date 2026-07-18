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
    /// Every status, in lifecycle order. The canonical list: anything that needs
    /// to reason about "all statuses" — a status set, a SQL `IN` clause, a
    /// filter expression, a test fixture — derives from here rather than
    /// retyping the names.
    ///
    /// This exists because the names were previously spelled out by hand in ten
    /// places across four syntaxes (Rust `matches!` on `&str`, `matches!` on the
    /// enum, SQL `IN`/`NOT IN`, and the filter DSL), so adding a variant meant
    /// finding all ten and nothing failed if you missed one — a task in the new
    /// status would simply stop appearing, and a chart missing tasks still looks
    /// like a valid chart.
    pub const ALL: [Status; 5] = [
        Status::Backlog,
        Status::Pending,
        Status::Active,
        Status::Done,
        Status::Cancelled,
    ];

    /// True for a status no further work will happen in: `done` or `cancelled`.
    /// The exhaustive `match` is load-bearing — a new variant fails to compile
    /// here, which forces the author to decide which side it falls on instead of
    /// silently inheriting "open".
    pub fn is_terminal(self) -> bool {
        match self {
            Status::Done | Status::Cancelled => true,
            Status::Backlog | Status::Pending | Status::Active => false,
        }
    }

    /// True for work that is not finished — the complement of [`is_terminal`].
    pub fn is_open(self) -> bool {
        !self.is_terminal()
    }

    /// True when this status counts toward report aggregations (DESIGN §12-D24):
    /// everything except `cancelled`. Abandoned work is not work; completed work
    /// is, which is why this is not the same question as [`is_open`].
    pub fn counts_in_reports(self) -> bool {
        match self {
            Status::Cancelled => false,
            Status::Backlog | Status::Pending | Status::Active | Status::Done => true,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Backlog => "backlog",
            Status::Pending => "pending",
            Status::Active => "active",
            Status::Done => "done",
            Status::Cancelled => "cancelled",
        }
    }

    /// Render the statuses satisfying `pred` as a SQL literal list ready to drop
    /// between the parentheses of an `IN (...)`, e.g. `'backlog','pending'`.
    ///
    /// This is string-built SQL, so the injection question has to be answered
    /// out loud: **no caller-supplied text can reach it**. The only values ever
    /// emitted are [`Status::as_str`] on members of [`Status::ALL`] — five
    /// compile-time `&'static str` literals of lowercase ASCII letters. `pred`
    /// chooses *which* of those five appear; it cannot introduce a sixth. The
    /// `debug_assert` below pins that invariant so a future variant spelled with
    /// a quote or a space fails loudly in tests rather than silently producing
    /// malformed SQL.
    ///
    /// It exists because the status sets were previously typed by hand into four
    /// separate `IN`/`NOT IN` clauses, so a new variant meant remembering all
    /// four — and forgetting one does not error, it just quietly drops rows.
    pub fn sql_in_list(pred: fn(Status) -> bool) -> String {
        Status::ALL
            .into_iter()
            .filter(|s| pred(*s))
            .map(|s| {
                let name = s.as_str();
                debug_assert!(
                    name.chars().all(|c| c.is_ascii_lowercase()),
                    "Status::as_str must stay a bare lowercase word to be SQL-literal-safe: {name:?}"
                );
                format!("'{name}'")
            })
            .collect::<Vec<_>>()
            .join(",")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is hand-written — the one place the variant list is retyped — so it
    /// is also the one place that can silently fall out of date. The array
    /// length is declared, so a *missing* entry is a compile error only if the
    /// author also bumps the count; a duplicate entry would satisfy the length
    /// while dropping a status. Asserting distinctness plus a round trip through
    /// `parse`/`as_str` makes both failures loud.
    #[test]
    fn all_lists_every_status_exactly_once() {
        let mut seen: Vec<&str> = Status::ALL.iter().map(|s| s.as_str()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "Status::ALL contains a duplicate");
        assert_eq!(before, 5, "Status::ALL must list every variant");
    }

    /// `as_str` and `parse` are two independent hand-written matches. Both are
    /// exhaustive, so the compiler catches a *missing* arm — but not a typo: an
    /// arm reading `"canceled"` compiles, passes every type check, and silently
    /// breaks every stored row and every filter naming that status.
    #[test]
    fn as_str_and_parse_round_trip() {
        for s in Status::ALL {
            assert_eq!(Status::parse(s.as_str()), Some(s), "round trip failed for {s:?}");
        }
        assert_eq!(Status::parse("canceled"), None, "a near-miss must not parse");
        assert_eq!(Status::parse(""), None);
    }

    /// `sql_in_list` builds SQL by string concatenation, and four live queries
    /// now depend on it. Two distinct ways it could go wrong silently: emitting
    /// the wrong *set* (a filter predicate applied inversely would swap which
    /// tasks a burndown or a blocked-check sees, with no error anywhere), or
    /// emitting the wrong *shape* — a missing quote or a stray separator turns
    /// the clause into a SQLite parse error at runtime, in code paths a unit test
    /// of the predicate alone never executes. Both are checked here against the
    /// exact literals the queries used before this was derived.
    #[test]
    fn sql_in_list_emits_quoted_names_for_exactly_the_matching_statuses() {
        assert_eq!(Status::sql_in_list(Status::is_open), "'backlog','pending','active'");
        assert_eq!(Status::sql_in_list(Status::is_terminal), "'done','cancelled'");
        assert_eq!(
            Status::sql_in_list(Status::counts_in_reports),
            "'backlog','pending','active','done'"
        );

        // Shape, independent of any one predicate: every element is a bare
        // lowercase word in single quotes, and the set matches the predicate.
        for pred in [Status::is_open, Status::is_terminal, Status::counts_in_reports] {
            let list = Status::sql_in_list(pred);
            let parts: Vec<&str> = list.split(',').collect();
            let want: Vec<&str> =
                Status::ALL.into_iter().filter(|s| pred(*s)).map(Status::as_str).collect();
            assert_eq!(parts.len(), want.len(), "wrong element count in {list:?}");
            for (part, name) in parts.iter().zip(&want) {
                assert_eq!(*part, format!("'{name}'"), "malformed element in {list:?}");
            }
        }
    }

    /// The three status sets the codebase actually reasons about, pinned by
    /// enumeration rather than by restating the predicate. `done` is the
    /// interesting case: terminal, yet it still counts in reports (D24).
    #[test]
    fn the_status_sets_partition_as_documented() {
        let names = |f: fn(Status) -> bool| -> Vec<&'static str> {
            Status::ALL.into_iter().filter(|s| f(*s)).map(Status::as_str).collect()
        };
        assert_eq!(names(Status::is_open), ["backlog", "pending", "active"]);
        assert_eq!(names(Status::is_terminal), ["done", "cancelled"]);
        assert_eq!(
            names(Status::counts_in_reports),
            ["backlog", "pending", "active", "done"],
            "D24: everything but cancelled — done is terminal but is still real work"
        );
    }
}
