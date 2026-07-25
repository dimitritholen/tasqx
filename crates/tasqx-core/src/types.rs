//! Domain types and the API envelope (DESIGN.md §3 data model, §4 envelope).
//!
//! These are the serde structs that *are* the contract: the same types flow
//! over stdio one-shot and (later) the daemon socket. Timestamps are kept as
//! RFC3339 strings — the simplest representation that is correct and preserves
//! whatever offset a client sends (see `crate::util::now` for generation).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::is_future_at;

/// The kinds of thing the event log records against (`events.entity`).
///
/// This exists so the set is **derived rather than retyped**. The entity column
/// was previously written only as the bare literals `"task"` and `"project"` at
/// nineteen `insert_event` call sites, which made the accepted set of
/// `event.list {entity}` a fact nobody owned: the reader passed the caller's
/// string straight into `WHERE entity = ?1`, so `entity: "tsak"` was an empty
/// list at `ok: true` — a closed vocabulary answering a typo with silence, the
/// same shape as `status:pendign`. With `insert_event` taking this type, the
/// writers *cannot* introduce a third spelling and the reader's accepted set is
/// [`Entity::ALL`] by construction, which is D30's "derive it, don't keep a list
/// in sync" applied one layer in from clap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Entity {
    /// A task row. Also the entity every annotation event is filed under.
    Task,
    /// A project row.
    Project,
    /// A memory document (D41). Annotation events stay under [`Entity::Task`]
    /// — an annotation belongs to its task — so `doc` covers only the
    /// standalone knowledge rows in `docs`.
    Doc,
}

impl Entity {
    /// Every variant. Hand-written like [`Status::ALL`], and pinned by the same
    /// exhaustiveness test, because Rust has no way to enumerate a plain enum.
    pub const ALL: [Entity; 3] = [Entity::Task, Entity::Project, Entity::Doc];

    /// The wire spelling written into `events.entity` — the exact lowercase word
    /// [`Entity::parse`] round-trips, and the only text any writer may store.
    pub fn as_str(self) -> &'static str {
        match self {
            Entity::Task => "task",
            Entity::Project => "project",
            Entity::Doc => "doc",
        }
    }

    /// The inverse of [`Entity::as_str`]. `None` for anything outside the closed
    /// set, which is what lets `event.list {entity}` refuse a typo instead of
    /// answering it with an empty list.
    pub fn parse(s: &str) -> Option<Entity> {
        match s {
            "task" => Some(Entity::Task),
            "project" => Some(Entity::Project),
            "doc" => Some(Entity::Doc),
            _ => None,
        }
    }

    /// The accepted set as a message fragment, e.g. `task, project`. Built from
    /// [`Entity::ALL`] so an error message can never fall behind the enum.
    pub fn accepted() -> String {
        Entity::ALL
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

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

/// The single definition of "is this task still being held back by a date?" —
/// DESIGN's `[*] --> backlog: task.add (waiting/scheduled)` and its promised
/// counterpart `backlog --> pending: wait/schedule reached` (§ state machine,
/// and the `ls status:backlog` example that says outright these "will surface
/// automatically when their date arrives").
///
/// A task is parked in `backlog` exactly while its `wait` or `scheduled` instant
/// is still ahead of `now`; once either has passed it is `pending`. Only that
/// one edge: any other `stored` status is returned untouched, because `pending`,
/// `active`, `done` and `cancelled` were all reached because the *user* said so,
/// and no clock may undo that. (DESIGN has no `pending --> backlog` edge, so
/// pushing a wait back into the future on an already-released task is a
/// different question, deliberately left alone here.)
///
/// Three callers, one rule. `task.add` and the recurrence spawn used to inline
/// `if is_future(wait) || is_future(scheduled)` separately — the same expression
/// typed twice, one edit away from disagreeing — and the reverse edge was
/// implemented nowhere at all, which is what made a task parked behind a future
/// `wait` invisible forever: pushing the wait into the past did not release it,
/// and no verb could set a status. Adding [`crate::storage::map_task_row`] as
/// the third caller is what closes that, because it is the choke point every
/// read of a task goes through.
///
/// `now` is injected rather than read here: this is the only transition with no
/// user action behind it, so a test that cannot name the instant could only
/// probe it by sleeping.
pub fn effective_status(
    stored: Status,
    wait: Option<&str>,
    scheduled: Option<&str>,
    now: Timestamp,
) -> Status {
    match stored {
        Status::Backlog if !is_future_at(wait, now) && !is_future_at(scheduled, now) => {
            Status::Pending
        }
        other => other,
    }
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

    /// True for work that is not finished — the complement of
    /// [`Status::is_terminal`].
    ///
    /// Written as a qualified path, not a bare ``[`is_terminal`]``: an inherent
    /// associated item is not in module scope for intra-doc resolution, so the
    /// short form resolves to nothing and renders as plain text.
    pub fn is_open(self) -> bool {
        !self.is_terminal()
    }

    /// True when this status counts toward report aggregations (DESIGN §12-D24):
    /// everything except `cancelled`. Abandoned work is not work; completed work
    /// is, which is why this is not the same question as [`Status::is_open`].
    pub fn counts_in_reports(self) -> bool {
        match self {
            Status::Cancelled => false,
            Status::Backlog | Status::Pending | Status::Active | Status::Done => true,
        }
    }

    /// The wire spelling stored in `tasks.status` and emitted by every read
    /// surface. A bare lowercase ASCII word by contract — [`Status::sql_in_list`]
    /// interpolates these straight into SQL and asserts that shape.
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

    /// The accepted set as a message fragment, e.g. `backlog, pending, …`.
    /// Built from [`Status::ALL`] so no error message can list four of five.
    pub fn accepted() -> String {
        Status::ALL
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The inverse of [`Status::as_str`], exact-match only. `None` is what makes
    /// a row carrying an unrecognized status visible rather than silently
    /// recoded — see [`Task::status_raw`].
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
    /// High. Contributes the largest priority term to [`crate::urgency`].
    H,
    /// Medium.
    M,
    /// Low. Still ranked above a task carrying no priority at all.
    L,
}

impl Priority {
    /// Every priority, highest first. The canonical list, following the
    /// [`Status::ALL`] precedent for the same reason: anything that needs to
    /// enumerate "all priorities" derives from here rather than retyping `H`,
    /// `M`, `L`.
    ///
    /// This exists because the letters were retyped in the MCP tool schema
    /// (`crate::mcp`), which an agent reads to decide what it is allowed to
    /// send. A schema listing a letter the engine rejects makes the agent's
    /// call fail; a schema *omitting* a letter the engine accepts makes the
    /// agent never use it — and neither shows up as an error anywhere, because
    /// nothing was comparing the two lists.
    pub const ALL: [Priority; 3] = [Priority::H, Priority::M, Priority::L];

    /// The wire spelling: a single UPPERCASE letter. [`Priority::parse`] is the
    /// forgiving side of the pair, so the two are not symmetric on purpose.
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
    /// Stable UUID. The identity that survives a rename; `name` does not.
    pub id: String,
    /// The dotted path (`work.api`). This IS the hierarchy — there is no parent
    /// column — so `work` and `work.api` are related only by this string.
    pub name: String,
    /// Free-text description. Omitted from the JSON entirely when absent, so a
    /// client cannot tell "unset" from "set to null" (there is no null case).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Archived projects stay in the store and keep their tasks; they are hidden
    /// from `project.list` unless the caller asks for them.
    pub archived: bool,
    /// RFC3339 instant the project row was written.
    pub created: String,
}

/// A tag. Many-to-many with tasks via the `task_tags` join.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    /// Stable UUID for the tag row.
    pub id: String,
    /// The tag text as the user typed it, without the `+`/`-` filter sigil —
    /// those are filter syntax ([`crate::filter`]), never part of the name.
    pub name: String,
}

/// The full task record as stored, mirroring the `tasks` table columns.
/// Output shapes for specific methods are built per-handler; this is the
/// canonical in-memory row used internally and for `task.list` rendering.
#[derive(Debug, Clone)]
pub struct Task {
    /// Stable UUID (v7, so it sorts by creation time). The identity every event
    /// row, dependency edge and tag link points at.
    pub id: String,
    /// The small integer a human types (`tasqx done 12`). Allocated from a
    /// monotonic floor, so it is stable for the life of the task and is NOT
    /// reused after deletion — a recycled number would silently redirect a
    /// command typed from memory at whatever now holds it.
    pub short_id: i64,
    /// The task's one-line summary.
    pub title: String,
    /// The lifecycle state as this reader understood it. Read through
    /// [`effective_status`] on the way out of storage, so a task parked behind a
    /// future `wait`/`scheduled` reads `backlog` and releases to `pending` on its
    /// own. When the stored text was not one of the five this holds a
    /// placeholder and [`Task::status_raw`] holds the truth.
    pub status: Status,
    /// The `status` column's text, kept **only** when it is not one of the five
    /// (`Status::parse` returned `None`). `None` on every well-formed row.
    ///
    /// A store can hold such a row — `store.import` accepted an arbitrary status
    /// string until this cluster closed that hole, so the stores most likely to
    /// contain one are precisely the stores an upgrade must not lock anyone out
    /// of. `status` then carries a placeholder so the row keeps flowing through
    /// filters and sorts, and this field carries the truth: it is what every
    /// read surface prints and what `store.export` emits, so the original bytes
    /// survive the one command that can get data out of a store like this.
    pub status_raw: Option<String>,
    /// Priority, or `None` for "unset" — which is a real third answer, not a
    /// synonym for [`Priority::L`]: it scores 0 where `L` scores 1.8.
    pub priority: Option<Priority>,
    /// The owning project's dotted NAME, not its id. Denormalized on purpose so
    /// a task row renders without a join; a renamed project therefore has to
    /// rewrite its tasks.
    pub project: Option<String>,
    /// RFC3339 deadline. The dominant term in [`crate::urgency`], and the anchor
    /// a relative `remind` offset is measured back from.
    pub due: Option<String>,
    /// RFC3339 instant before which the task is not meant to be started. Holds
    /// the task in `backlog` while it is in the future ([`effective_status`]).
    pub scheduled: Option<String>,
    /// RFC3339 instant before which the task should not even be SHOWN. Holds the
    /// task in `backlog` exactly as `scheduled` does; the two differ in intent,
    /// not in mechanism.
    pub wait: Option<String>,
    /// Estimated effort as an ISO-8601 duration (`PT4H`). Normalized on write by
    /// `datetime::parse_duration`, so the friendly `4h` a user types is never
    /// what lands here — one stored spelling, one reader.
    pub estimate: Option<String>,
    /// The recurrence rule in [`crate::recur`]'s canonical spelling, normalized
    /// on write for the same reason `estimate` is. `None` on a one-off task.
    pub recurrence: Option<String>,
    /// Reminder spec (§9), canonical form: a signed offset anchored to `due`
    /// (`-1h`) or an absolute RFC3339 instant. See [`crate::remind`].
    pub remind: Option<String>,
    /// The cached [`crate::urgency`] score. A DERIVED value that is also
    /// persisted, so it is only as fresh as the last write to this row — the
    /// due-proximity and age terms both move with the wall clock.
    pub urgency: f64,
    /// RFC3339 instant the current active interval began (None when not active).
    pub active_since: Option<String>,
    /// Accumulated tracked time across closed intervals, in seconds.
    pub tracked_seconds: i64,
    /// Per-task event counter (`_rev` in the API).
    pub rev: i64,
    /// RFC3339 instant the task was added. Feeds the small age term in
    /// [`crate::urgency`].
    pub created: String,
    /// RFC3339 instant of the last mutation. Moves with every event this task
    /// records, including ones that change no visible field.
    pub modified: String,
    /// RFC3339 instant the task reached `done`, `None` otherwise. This is the
    /// field `completed.before:`/`completed.after:` filter on, and the reason
    /// "what did I finish this week" is answerable at all.
    pub completed: Option<String>,
}

impl Task {
    /// What this task's status should be *shown* and *exported* as: the stored
    /// text when the reader could not recognize it, the canonical name
    /// otherwise. Every surface goes through here so no surface can accidentally
    /// print the placeholder as though it were the fact.
    pub fn status_text(&self) -> &str {
        self.status_raw
            .as_deref()
            .unwrap_or_else(|| self.status.as_str())
    }

    /// True when [`status_text`](Self::status_text) is a value no writer of this
    /// engine could have produced. Callers use it to flag the row rather than to
    /// hide it.
    pub fn status_is_unrecognized(&self) -> bool {
        self.status_raw.is_some()
    }
}

/// Request envelope (DESIGN.md §4). `params` and `id` are optional on stdio.
#[derive(Debug, Deserialize)]
pub struct ApiRequest {
    /// The client's API version string. Checked against [`crate::API_VERSION`];
    /// a major it does not serve is refused with `unsupported_version` rather
    /// than answered on a best guess.
    pub tasqx: String,
    /// Opaque correlation id, echoed verbatim in the response. Any JSON value,
    /// because it is the CLIENT's — nothing here interprets it. Absent on a
    /// notification, which is what makes it `Option` rather than defaulted.
    #[serde(default)]
    pub id: Option<Value>,
    /// The dotted method name (`task.add`), matched by [`crate::dispatch()`].
    pub method: String,
    /// The method's arguments. Optional because several methods take none;
    /// `check_params` is what turns an unknown or missing key into a
    /// `bad_request` instead of a silently ignored field.
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
            assert_eq!(
                Status::parse(s.as_str()),
                Some(s),
                "round trip failed for {s:?}"
            );
        }
        assert_eq!(
            Status::parse("canceled"),
            None,
            "a near-miss must not parse"
        );
        assert_eq!(Status::parse(""), None);
    }

    /// `Entity::ALL` carries the same silent-failure mode as `Status::ALL` and
    /// one extra consequence of its own: it is now the *published accepted set*
    /// for `event.list {entity}`, so a dropped or duplicated entry becomes an
    /// entity the log holds rows for and the reader refuses to list — turning
    /// the fix for a silent empty answer into a silent refusal of a real one.
    #[test]
    fn entity_all_lists_every_variant_exactly_once_and_round_trips() {
        let mut seen: Vec<&str> = Entity::ALL.iter().map(|e| e.as_str()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "Entity::ALL contains a duplicate");
        assert_eq!(before, 3, "Entity::ALL must list every variant");
        for e in Entity::ALL {
            assert_eq!(
                Entity::parse(e.as_str()),
                Some(e),
                "round trip failed for {e:?}"
            );
            assert!(
                Entity::accepted().contains(e.as_str()),
                "accepted() must name {e:?}"
            );
        }
        assert_eq!(Entity::parse("tasks"), None, "a near-miss must not parse");
        assert_eq!(Entity::parse(""), None);
    }

    /// `Priority::ALL` is hand-written, exactly like `Status::ALL`, and carries
    /// the same silent-failure mode: a duplicate entry satisfies the declared
    /// array length while dropping a real priority, and the MCP tool schema is
    /// now rendered from this list — so a dropped letter becomes a priority no
    /// agent is ever told it may send.
    #[test]
    fn priority_all_lists_every_variant_exactly_once() {
        let mut seen: Vec<&str> = Priority::ALL.iter().map(|p| p.as_str()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), before, "Priority::ALL contains a duplicate");
        assert_eq!(before, 3, "Priority::ALL must list every variant");
        for p in Priority::ALL {
            assert_eq!(
                Priority::parse(p.as_str()),
                Some(p),
                "round trip failed for {p:?}"
            );
        }
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
        assert_eq!(
            Status::sql_in_list(Status::is_open),
            "'backlog','pending','active'"
        );
        assert_eq!(
            Status::sql_in_list(Status::is_terminal),
            "'done','cancelled'"
        );
        assert_eq!(
            Status::sql_in_list(Status::counts_in_reports),
            "'backlog','pending','active','done'"
        );

        // Shape, independent of any one predicate: every element is a bare
        // lowercase word in single quotes, and the set matches the predicate.
        for pred in [
            Status::is_open,
            Status::is_terminal,
            Status::counts_in_reports,
        ] {
            let list = Status::sql_in_list(pred);
            let parts: Vec<&str> = list.split(',').collect();
            let want: Vec<&str> = Status::ALL
                .into_iter()
                .filter(|s| pred(*s))
                .map(Status::as_str)
                .collect();
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
            Status::ALL
                .into_iter()
                .filter(|s| f(*s))
                .map(Status::as_str)
                .collect()
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

#[cfg(test)]
mod release_tests {
    use super::*;

    const T: &str = "2026-07-19T12:00:00Z";

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// Both sides of the boundary, and the boundary instant itself. The held
    /// side and the released side are named as literals rather than derived from
    /// `T`, so this cannot pass by agreeing with itself.
    #[test]
    fn the_boundary_is_the_instant_itself() {
        // one second before the wait: still held.
        assert_eq!(
            effective_status(Status::Backlog, Some(T), None, at("2026-07-19T11:59:59Z")),
            Status::Backlog
        );
        // exactly at it: `wait` is not in the future any more, so it is reached.
        assert_eq!(
            effective_status(Status::Backlog, Some(T), None, at(T)),
            Status::Pending
        );
        // one second after: released.
        assert_eq!(
            effective_status(Status::Backlog, Some(T), None, at("2026-07-19T12:00:01Z")),
            Status::Pending
        );
    }

    /// `scheduled` holds on its own, and the later of the two decides: a passed
    /// `wait` must not release a task still scheduled for next week.
    #[test]
    fn either_field_can_hold_the_task() {
        let now = at("2026-07-19T12:00:00Z");
        assert_eq!(
            effective_status(Status::Backlog, None, Some("2026-07-26T00:00:00Z"), now),
            Status::Backlog
        );
        assert_eq!(
            effective_status(
                Status::Backlog,
                Some("2020-01-01T00:00:00Z"),
                Some("2026-07-26T00:00:00Z"),
                now
            ),
            Status::Backlog
        );
        // Neither field set at all: nothing is holding it.
        assert_eq!(
            effective_status(Status::Backlog, None, None, now),
            Status::Pending
        );
        // Unparseable dates cannot hold a task hostage (they never have).
        assert_eq!(
            effective_status(Status::Backlog, Some("whenever"), None, now),
            Status::Pending
        );
    }

    /// No clock may move a status the user chose. A task started, finished or
    /// abandoned stays where it is even with a wait far in the future — and a
    /// released task is not pushed back into the backlog either, since DESIGN
    /// has no such edge.
    #[test]
    fn only_backlog_is_subject_to_the_clock() {
        let now = at("2026-07-19T12:00:00Z");
        let future = Some("2999-01-01T00:00:00Z");
        for s in [
            Status::Pending,
            Status::Active,
            Status::Done,
            Status::Cancelled,
        ] {
            assert_eq!(
                effective_status(s, future, future, now),
                s,
                "{s:?} with a future wait"
            );
            assert_eq!(
                effective_status(s, None, None, now),
                s,
                "{s:?} with no dates"
            );
        }
    }
}
