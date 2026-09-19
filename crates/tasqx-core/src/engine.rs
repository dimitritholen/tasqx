//! The engine: domain logic + the mutation/query methods behind every API
//! method (DESIGN.md §2, §4).
//!
//! One `Engine` owns one SQLite connection. Every mutating method opens an
//! immediate transaction via `begin_mutation` (so the public API can take
//! `&self` per DESIGN's `dispatch(&Engine, ...)` shape), performs its state change AND
//! writes the corresponding event row, then commits. If anything fails before
//! `commit`, the transaction drops and rolls back — leaving no state change and
//! no event. State and history therefore move together, always.

mod commands;
mod memory;
mod projects;
mod relationships;

/// D138's check-state vocabulary, re-exported beside the other closed
/// vocabularies this module publishes ([`SUMMARY_GROUP_BY`],
/// [`OUTCOME_METRICS`], [`MEMORY_SCOPES`]) so the MCP schema renders its `enum`
/// from the list the engine validates against.
pub use relationships::CHECK_STATES;
mod reports;
pub mod task;
mod tokens;
mod transfer;
mod undo;

pub use memory::{SessionDoc, SessionRulings, MEMORY_SCOPES};
pub use undo::{NOT_UNDOABLE, UNDOABLE_OPS};

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use jiff::Timestamp;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::datetime;
use crate::error::ApiError;
use crate::filter::{is_overdue, Filter, MatchCtx};
use crate::recur;
use crate::remind;
use crate::storage::{
    self, alloc_short_id, bump_short_id_floor, clear_config, ensure_tag_link, get_config,
    insert_event, map_task_row, map_task_row_at, normalize_tags, set_config, task_tags, TASK_COLS,
};
use crate::types::{effective_status, Entity, Priority, Status, Task};
use crate::urgency;
use crate::util::{
    duration_secs, is_future_at, iso_duration, now, opt_array, opt_bool, opt_i64, opt_str,
    opt_str_array, opt_str_nonempty, opt_u64, parse_ts, req_array, req_i64, req_object, req_str,
    req_str_lookup, req_str_value, require_all, seconds_between, signed_iso_duration,
};

/// The config key holding the default project name (inherited by `task.add`).
pub const DEFAULT_PROJECT_KEY: &str = "default_project";

/// The axes `report.summary` can group by. **First entry is the default** when
/// the caller omits `group_by`.
///
/// This is the source of truth: [`Engine::report_summary`] validates against it
/// and builds its rejection message from it, and the MCP tool schema
/// (`crate::mcp`) renders its JSON-Schema `enum` from it. Those two lists used
/// to be typed out separately, and the MCP one is what an agent reads to decide
/// what it may send — so a drifted schema either forbids a valid axis forever
/// or produces calls the engine rejects, with no test anywhere going red.
pub const SUMMARY_GROUP_BY: [&str; 3] = ["project", "status", "priority"];

/// The metrics `report.summary` can emit per group. `count` is always present;
/// the rest are opt-in via the `metrics` param.
///
/// Source of truth for the same reason as [`SUMMARY_GROUP_BY`]: the MCP schema's
/// `enum` is built from this, and a test drives every entry through the engine
/// to prove the name still produces a field.
///
/// The `tokens_*` metrics roll up the per-task token measurements (#11) and are
/// emitted as JSON integers, never ISO durations: a token count is a cardinal
/// number, and the JSON type of a metric is frozen from its first release.
/// There are exactly four buckets: the blended `tokens_total` left the
/// vocabulary with D50, so a downstream sum is an explicit choice, never an
/// ambient default. `tokens_unsplit` (D167) is not a fifth bucket but the sum
/// of counts a reporter could not split at all, kept apart from the four.
pub const SUMMARY_METRICS: [&str; 9] = [
    "count",
    "est_total",
    "overdue",
    "tracked_total",
    "tokens_in",
    "tokens_out",
    "tokens_cache_read",
    "tokens_cache_creation",
    "tokens_unsplit",
];

/// The metrics `report.outcomes` can emit per group (D137). Unlike
/// [`SUMMARY_METRICS`] there is no always-present one and no default subset:
/// omitting `metrics` emits all of them, because the point of the report is the
/// set read together — a rework rate without the cost beside it invites the
/// wrong fix.
///
/// Source of truth for the same reason as [`SUMMARY_GROUP_BY`]: the engine
/// validates against it, its rejection message is built from it, and the MCP
/// tool schema renders its `enum` from it.
///
/// Each name is what the metric MEASURES, never a verdict about it: `silent` is
/// completions carrying no annotation, not "bad completions". D137 refuses a
/// composite score for the same reason these stay five names — the blend
/// destroys the split that makes a figure actionable.
pub const OUTCOME_METRICS: [&str; 8] = [
    "rework",
    "calibration",
    "cost",
    "silent",
    "abandonment",
    // D139. Its denominator is completions that HAD a budget, not every
    // completion: a rate over all of them would shrink as unbudgeted work
    // landed, which would read as improvement and be nothing of the kind.
    "overrun",
    // D138. Its denominator is completions that HAD criteria, for the same
    // reason `overrun`'s is completions that had a budget: a rate over every
    // completion would shrink as uncriteriaed work landed.
    "unproven",
    // D150. Completions that overrode open blockers with `force`. Its
    // denominator IS completions, like `rework`'s and unlike the two above:
    // every completion could have been a forced one, so there is no smaller
    // population to measure it against.
    "forced",
];

/// `memory.search`'s default page when the caller names no `limit`.
///
/// A named constant because three places need the same number and one of them
/// is a different crate's concern: the engine applies it, and the MCP
/// transport's D136 brief-fitting uses it as the ceiling it bisects down from.
/// A transport bisecting from a hardcoded 10 while the engine served 20 would
/// silently cap every brief at half the hits.
pub const MEMORY_SEARCH_LIMIT: u64 = 10;

/// How many #101 standing docs one scope holds before `memory.add` says so.
///
/// A standing doc is re-read every session by design (D156), so the cost of
/// one is paid on every turn forever — which makes an unbounded pile of them
/// the failure mode, not a missing doc. The cap is a HINT on the add that
/// crosses it, never a refusal: the engine cannot know which of sixteen
/// rulings is the redundant one, and losing a correction to a hard limit is
/// worse than reading one line too many. Fifteen because the rules a project
/// actually re-states fit in that many and a longer list is a document, not a
/// set of standing orders.
pub const STANDING_SOFT_CAP: usize = 15;

/// `task.brief`'s own default memory page, and deliberately smaller than
/// [`MEMORY_SEARCH_LIMIT`] (D154).
///
/// A caller's `memory.search` query is a QUESTION — someone typed those words
/// and wants what they turn up — while the brief's is a derived bag of words
/// taken from the task's title, tags and project. A derived query earns a
/// shorter page: measured 2026-09-16 on the real store, the brief's ten hits
/// were ~4 KB of a 6.8 KB response, and the agent reading it opened at most two
/// of them. Five keeps D147's reservation intact — `ceil(5 / 2)` is three doc
/// slots, so a ruling still cannot be buried under a sibling task's notes — and
/// an explicit `memory_limit` is answered exactly as asked, at any size.
pub const BRIEF_MEMORY_LIMIT: u64 = 5;

/// The keys `task.list` can sort by. A `-` prefix on any of them sorts
/// descending; the default when `sort` is omitted is `-urgency`.
///
/// Source of truth for the same reason as [`SUMMARY_GROUP_BY`], plus one this
/// list paid for directly: `compare_by` used to match these names inline and
/// fall through to "equal" for anything else, so an unknown key was accepted,
/// ignored, and answered with exit 0 — the caller sorted by a key that was
/// never applied and had no way to find out. The list was also published
/// nowhere, so there was no way to look up what a valid key even was. Both
/// halves are fixed by having one list: `parse_sort` validates against it, its
/// rejection message is built from it, and the MCP schema and the HTML guide
/// render their key lists from it.
///
/// `tokens` (#215) ranks by the saturating sum of a task's four measurement
/// buckets. That sum is never itself reported on any surface — D48/D50 forbid
/// a blended `tokens_total` output — it exists here only to produce a total
/// order for "most expensive first", the same way `priority` sorts by a rank
/// nobody reads back as a number.
///
/// `estimate` and `tracked` (#663/D173) rank by the DURATION an `estimate`
/// string or `tracked_seconds` carries, not by the string itself — an
/// `estimate` of `PT4H` must outrank `PT90M` even though `"PT4H" < "PT90M"`
/// under `str::cmp`. Neither ever has a `None` that reaches `compare_by`:
/// `estimate` is a genuine `Option`, and `tracked` reads `tracked_seconds ==
/// 0` (never started) as the same "no value" case. Both place that case LAST
/// on the sorted list REGARDLESS of `-`, which `due`'s `opt_cmp` documents but
/// does not deliver once its answer passes back through the `desc` reversal
/// below — D173 records the fix as its own comparator rather than reusing
/// `opt_cmp` and inheriting that bug.
pub const SORT_KEYS: [&str; 10] = [
    "urgency", "short_id", "priority", "due", "created", "modified", "title", "tokens", "estimate",
    "tracked",
];

/// The keys `task.list`'s `fields` param may name. Sorted, since it is read off
/// a `serde_json` object.
///
/// Source of truth for the same reasons as [`SORT_KEYS`], and it paid the same
/// price: the projection loop kept a key only `if let Some(v) = full.get(k)`,
/// so `fields:["short_id","titel"]` returned rows missing the field with
/// `ok: true` — a typo and an empty column look identical, forever.
///
/// **Derived, not typed out.** It is the key set of one real
/// `list_row_json` call, so a field added to the projection joins this list
/// the moment it exists, and a field removed leaves it. The alternative — a
/// hand-written array next to `task_to_json` — is exactly the parallel-copy
/// drift this codebase keeps paying for (D30's rule: derive it). The probe task
/// carries `status_raw`, because `status_unrecognized` is emitted only for an
/// unrecognized status (D28) and must still be a name a caller may ask for.
pub static TASK_FIELDS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let probe = Task {
        id: String::new(),
        short_id: 0,
        title: String::new(),
        status: Status::Pending,
        status_raw: Some(String::new()),
        priority: None,
        project: None,
        due: None,
        scheduled: None,
        wait: None,
        estimate: None,
        recurrence: None,
        remind: None,
        urgency: 0.0,
        active_since: None,
        tracked_seconds: 0,
        rev: 0,
        created: String::new(),
        modified: String::new(),
        completed: None,
        budget_tokens: None,
        delivered_annotation_id: None,
        tracked_adjustment_seconds: 0,
    };
    match list_row_json(
        &probe,
        &[],
        false,
        Some(&[]),
        Some(&Value::Null),
        Timestamp::UNIX_EPOCH,
    ) {
        Value::Object(m) => m.keys().cloned().collect(),
        // Unreachable: `task_to_json` builds an object literal.
        _ => Vec::new(),
    }
});

/// The core engine. Cheap to construct; holds one open store connection.
pub struct Engine {
    conn: Connection,
}

/// Owns one serialized mutation from `BEGIN IMMEDIATE` through commit. The
/// concrete transaction remains directly usable through `Deref`, while the
/// type makes lock ownership visible in every mutating handler signature.
struct MutationContext<'conn> {
    transaction: Transaction<'conn>,
}

impl<'conn> std::ops::Deref for MutationContext<'conn> {
    type Target = Transaction<'conn>;

    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

impl MutationContext<'_> {
    fn commit(self) -> Result<(), ApiError> {
        self.transaction.commit().map_err(ApiError::from)
    }
}

/// Statements one whole-relation snapshot runs, independent of task count.
///
/// Seven since D138: tasks, tags, blocked, dependencies, annotations, checks
/// and token measurements. The number is pinned rather than derived so that
/// adding a relation is a decision somebody takes on purpose — the contract
/// this guards is O(1) statements, not O(1) relations.
const SNAPSHOT_QUERY_COUNT: usize = 7;

struct TaskSnapshot {
    task: Task,
    tags: Vec<String>,
    blocked: bool,
    depends_on: Vec<String>,
    annotations: Vec<Value>,
    /// D138's acceptance criteria, in `position` order. Gated like
    /// `annotations`: only `store.export` reads the whole relation.
    checks: Vec<Value>,
    /// Token measurements in the canonical object shape, oldest first. Loaded
    /// set-based like every other side table — a per-task point query here is
    /// the N+1 the statement-count test exists to forbid.
    tokens: Vec<Value>,
    /// D170: the uuid of the task a recurrence spawned this one from.
    spawned_from: Option<String>,
}

impl Engine {
    /// Open (creating if needed) a file-backed store.
    pub fn open(path: &str) -> Result<Engine, ApiError> {
        Ok(Engine {
            conn: storage::open(path)?,
        })
    }

    /// Open an EXISTING file-backed store for reading only.
    ///
    /// Every read method on this type works against the result; every mutating
    /// one fails at `begin_mutation`, because SQLite refuses the write rather
    /// than because anything here checked. That is the point: the guarantee is
    /// enforced by the connection's open flags, so it cannot be lost by a new
    /// method forgetting to consult a boolean. See
    /// [`storage::open_read_only`] for the properties and the accepted WAL
    /// limitation.
    pub fn open_read_only(path: &str) -> Result<Engine, ApiError> {
        Ok(Engine {
            conn: storage::open_read_only(path)?,
        })
    }

    /// Open an ephemeral in-memory store (tests).
    pub fn open_in_memory() -> Result<Engine, ApiError> {
        Ok(Engine {
            conn: storage::open_in_memory()?,
        })
    }

    /// Direct read access to the connection (read-only helpers, tests).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Begin a mutating transaction with `BEGIN IMMEDIATE` so the write lock is
    /// taken up front. This lets `busy_timeout` actually serialize racing
    /// one-shot writers instead of one hitting `SQLITE_BUSY_SNAPSHOT` on a
    /// deferred read-then-write upgrade (DESIGN §2: writers wait, don't error).
    /// Keeps the `&self` shape via `new_unchecked`.
    fn begin_mutation(&self) -> Result<MutationContext<'_>, ApiError> {
        Ok(MutationContext {
            transaction: Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?,
        })
    }

    // ---- reference resolution ------------------------------------------------

    /// Resolve a `ref` param (short_id integer, or a string that is either a
    /// numeric short_id or a full UUID) to the loaded task.
    fn resolve_ref(&self, p: &Value) -> Result<Task, ApiError> {
        self.resolve_ref_on(&self.conn, p)
    }

    /// Resolve a `ref` against the caller's connection view. Mutations pass
    /// their IMMEDIATE transaction so validation and the eventual write share
    /// one serialized snapshot.
    fn resolve_ref_on(&self, conn: &Connection, p: &Value) -> Result<Task, ApiError> {
        let r = ref_param(p)?;
        self.resolve_ref_value_on(conn, r)
    }

    /// Resolve any JSON ref value (int short_id, numeric string, or UUID string)
    /// to a task. Shared by `resolve_ref` and the dependency handlers.
    fn resolve_ref_value(&self, r: &Value) -> Result<Task, ApiError> {
        self.resolve_ref_value_on(&self.conn, r)
    }

    fn resolve_ref_value_on(&self, conn: &Connection, r: &Value) -> Result<Task, ApiError> {
        // Numeric ref => short_id.
        if let Some(n) = r.as_i64() {
            return self.task_by_short_on(conn, n);
        }
        if let Some(s) = r.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                return self.task_by_short_on(conn, n);
            }
            if Uuid::parse_str(s).is_ok() {
                return self.task_by_id_on(conn, s);
            }
            return Err(ApiError::bad_request(format!(
                "ref is neither short_id nor UUID: {s} — expected a short_id \
                 like 42, or a UUID"
            )));
        }
        Err(ApiError::bad_request("ref must be an integer or string"))
    }

    fn task_by_short_on(&self, conn: &Connection, short_id: i64) -> Result<Task, ApiError> {
        conn.query_row(
            &format!("SELECT {TASK_COLS} FROM tasks WHERE short_id = ?1"),
            params![short_id],
            map_task_row,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => ApiError::not_found(
                format!("no task with short_id {short_id}"),
                Some(json!({ "short_id": short_id })),
            ),
            other => other.into(),
        })
    }

    fn task_by_id_on(&self, conn: &Connection, id: &str) -> Result<Task, ApiError> {
        conn.query_row(
            &format!("SELECT {TASK_COLS} FROM tasks WHERE id = ?1"),
            params![id],
            map_task_row,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                ApiError::not_found(format!("no task with id {id}"), Some(json!({ "id": id })))
            }
            other => other.into(),
        })
    }

    // ---- event.list ----------------------------------------------------------

    /// `event.list` — the audit log, newest first. Params: `limit` (default 50),
    /// at most one scope — either `ref` (one task) or `entity` (a whole
    /// [`Entity`] class) — and an optional `from` lower bound (D59).
    ///
    /// `entity` is validated against the closed vocabulary rather than passed
    /// into the `WHERE` clause; the comment below records why an empty list was
    /// the wrong answer to a typo.
    ///
    /// `from` takes the same date grammar every other caller-picked bound takes
    /// (D33 — `due.before:` and friends), so `-30d`, `yesterday` and
    /// `2026-07-20` all work. It is a **lower bound, not an exact filter**: it
    /// is applied against the time-ordered `id`, and promises no events older
    /// than roughly that instant. [`storage::event_id_floor`] carries the whole
    /// argument for why the `ts` column cannot serve this and why the bound
    /// leans a second the over-inclusive way.
    pub fn event_list(&self, p: &Value) -> Result<Value, ApiError> {
        // Checked, not `as i64`: a value above i64::MAX wrapped negative, and
        // SQLite reads a negative LIMIT as UNLIMITED — the exact opposite of
        // the bound the caller asked for, handed back at `ok: true`. Same
        // wording as `memory.search` (engine/memory.rs), which already gated
        // this; the two page-size parameters must not disagree.
        let limit = i64::try_from(opt_u64(p, "limit")?.unwrap_or(50)).map_err(|_| {
            ApiError::bad_request(format!(
                "`limit` must be at most {}, or omitted for the default",
                i64::MAX
            ))
        })?;

        // Every bound value in one list, and every placeholder index derived
        // from its position in that list.
        //
        // This replaces a hand-written `?1`/`?2` pair whose own comment said it
        // was readable because there were exactly two arms. `from` combines
        // independently with both scopes, so two arms became four, and a
        // hand-indexed placeholder across four arms is how the scope filter ends
        // up handed the limit and LIMIT handed an entity name — the failure
        // `event_list_applies_the_limit_in_every_scoping_arm` already exists to
        // catch once. Deriving the index removes the chance rather than testing
        // for it.
        let mut preds: Vec<String> = Vec::new();
        let mut binds: Vec<SqlValue> = Vec::new();

        // Optional scoping: `ref` (a task) or `entity` (a type name).
        if let Some(r) = p.get("ref") {
            let task = self.resolve_ref_value(r)?;
            preds.push(format!("entity_id = ?{}", binds.len() + 1));
            binds.push(SqlValue::Text(task.id));
        } else if let Some(ent) = opt_str(p, "entity")? {
            // `entity` is a CLOSED, compile-time vocabulary — the writers can
            // only ever spell `Entity::ALL` — so a value outside it is a caller
            // error, not a query that legitimately found nothing. Passed raw
            // into `WHERE entity = ?1`, `entity: "tsak"` was `{count: 0}` at
            // `ok: true`, and an empty audit log reads as an answer.
            //
            // `ref` above stays a lookup rather than a vocabulary check because
            // a task id is an OPEN runtime set — and `resolve_ref_value` already
            // returns `not_found` for one that names nothing.
            let ent = Entity::parse(&ent).ok_or_else(|| {
                ApiError::bad_request(format!(
                    "unknown entity {ent:?} (expected one of: {})",
                    Entity::accepted()
                ))
            })?;
            preds.push(format!("entity = ?{}", binds.len() + 1));
            binds.push(SqlValue::Text(ent.as_str().to_string()));
        }

        // `from`, filtered on the time-ordered `id` rather than on `ts` — see
        // `storage::event_id_floor` for why the obvious column is the wrong one.
        //
        // Read as literally `opt_when(p, "from", ...)`: the D33 drift guard in
        // `dispatch.rs` finds the keys an engine method reads by scanning this
        // source for `(p,"` and `p.get("`, so spelling it any other way makes
        // the key invisible to the guard and reddens it blaming PARAMS.
        if let Some(when) = opt_when(p, "from", crate::clock::now())? {
            // `parse_when` returns the canonical RFC3339 form, so this cannot
            // fail — but an `unwrap` here would turn a future grammar change
            // into a panic in a read path, and the store is the wrong place to
            // learn that.
            let ts = parse_ts(&when).ok_or_else(|| {
                ApiError::bad_request(format!("`from` resolved to an unreadable instant {when:?}"))
            })?;
            preds.push(format!("id >= ?{}", binds.len() + 1));
            binds.push(SqlValue::Text(storage::event_id_floor(ts)));
        }

        let where_sql = if preds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", preds.join(" AND "))
        };
        // The limit is BOUND, not interpolated, and takes the index after every
        // predicate's — the one placeholder whose position is not fixed.
        let limit_ph = format!("?{}", binds.len() + 1);
        binds.push(SqlValue::Integer(limit));
        // `ORDER BY id DESC` is newest-first for every event this engine minted,
        // because `id` is UUIDv7 and time-ordered. That is a fact about the
        // WRITER, not a guarantee about the table: `store.import` replays an
        // event under the id its document carried, so an imported row can sort
        // anywhere at all, and one above `f` sorts above everything (D129, #422).
        //
        // Kept by id deliberately. This is a publishing surface — an audit trail
        // read newest-first, which survives a foreign row landing out of place —
        // and its `from` bound is an id RANGE, which D59 ruled after `ts` proved
        // uncomparable in this store. Reordering here means replacing that bound
        // too, and that is D59's ruling to reopen rather than this comment's.
        // `event.revert` makes the opposite trade for the opposite reason: it
        // MUTATES on the row it picks, so one wrong answer changes the store,
        // and it orders by `rowid`, the store's own append order — see
        // `engine/undo.rs`, whose header sets the two side by side.
        let sql = format!(
            "SELECT id, entity, entity_id, op, payload, ts, actor FROM events \
             {where_sql} ORDER BY id DESC LIMIT {limit_ph}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
            let payload: Option<String> = r.get(4)?;
            let parsed = payload
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Null);
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "entity": r.get::<_, String>(1)?,
                "entity_id": r.get::<_, String>(2)?,
                "op": r.get::<_, String>(3)?,
                "payload": parsed,
                "ts": r.get::<_, String>(5)?,
                "actor": r.get::<_, Option<String>>(6)?,
            }))
        };
        let mut out = Vec::new();
        let rows = stmt.query_map(rusqlite::params_from_iter(binds), map)?;
        for r in rows {
            out.push(r?);
        }
        Ok(json!({ "count": out.len(), "events": out }))
    }

    // ---- reminder.fire -------------------------------------------------------

    /// Record that a reminder ripened: append the `reminded` event for
    /// `(task, at)`, once (DESIGN.md §9).
    ///
    /// This is the *whole* delivery record. The event row is simultaneously:
    ///  * the **dedupe key** — §9's "a fired reminder writes a `reminded` event
    ///    so it never double-fires", across the daemon path, a restart, and any
    ///    future one-shot scheduler path;
    ///  * the **push surface** — the daemon broadcasts every new event row, so a
    ///    `tasqx watch` subscriber observes the reminder headlessly, with no OS
    ///    notification involved.
    ///
    /// Idempotent by construction: the dedupe check runs *inside* the same
    /// IMMEDIATE transaction that writes the row, so two racing firers cannot
    /// both observe "not yet reminded" and both append.
    ///
    /// The task itself is untouched — no `rev` bump, no `modified` change. A
    /// reminder is a fact about time passing, not an edit to the task, and
    /// bumping `rev` would spuriously break a client's `expected_rev`.
    ///
    /// Params: `{ref, at}` where `at` is the RFC3339 instant that ripened.
    /// Returns `{fired, short_id, at}`; `fired: false` means it was already
    /// delivered and the caller must not notify.
    pub fn reminder_fire(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let at_raw = req_str(p, "at")?;
        // Normalize before storing/comparing: the dedupe key is an exact string
        // match, so `…T16:00:00+00:00` and `…T16:00:00Z` must not read as two
        // different reminders.
        // `at` is the ONE date input in the tool that deliberately does not take
        // `datetime::parse_when`'s grammar (D33 unified every other one), so the
        // message has to say WHY rather than imply a spelling mistake. `at` is
        // not a moment the caller picks: `scheduler::fire` supplies the instant
        // it already resolved, and `already_reminded` matches it as an exact
        // string. A relative spelling would resolve to some other instant, write
        // a `reminded` row that dedupes nothing, and leave the real reminder to
        // fire again — a silent double-notify. Telling the user to "type
        // RFC3339" invites exactly the retry that cannot work.
        let at = parse_ts(&at_raw)
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "`at` must be the exact RFC3339 instant the scheduler resolved for this \
                     reminder (e.g. 2026-07-16T00:00:00Z), got {at_raw:?}. It is the dedupe key \
                     that stops a reminder firing twice, not a date you choose, so relative \
                     spellings like `due:` accepts are refused here. The daemon passes this \
                     for you; a caller firing by hand derives it from the task's `due` and \
                     `remind` (task.get reports both)."
                ))
            })?
            .to_string();

        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        if storage::already_reminded(&tx, &task.id, &at)? {
            return Ok(json!({ "fired": false, "short_id": task.short_id, "at": at }));
        }
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "reminded",
            &json!({
                "at": at,
                "short_id": task.short_id,
                "title": task.title,
                "due": task.due,
                "remind": task.remind,
            }),
        )?;
        tx.commit()?;

        Ok(json!({ "fired": true, "short_id": task.short_id, "at": at }))
    }

    // ---- core.capabilities ---------------------------------------------------

    /// Capabilities, including the current default project (gap fix A1) and
    /// the store this engine is answering from (D74).
    ///
    /// `store` is read off the live connection rather than re-resolved from
    /// flags or environment, which is what makes it truthful on every branch:
    /// in-process it names the caller's own file, and through a daemon it
    /// names the daemon's file — the one answer D47 established no client
    /// could compute for itself. Null only for an in-memory store, which has
    /// no path to name.
    pub fn capabilities(&self) -> Result<Value, ApiError> {
        let mut v = crate::dispatch::capabilities();
        v["default_project"] = match self.default_project()? {
            Some(name) => Value::String(name),
            None => Value::Null,
        };
        v["store"] = match self.conn.path() {
            Some(p) if !p.is_empty() => Value::String(p.to_string()),
            _ => Value::Null,
        };
        Ok(v)
    }

    /// The path of the store this engine writes to, or `None` for an
    /// in-memory one. The same answer `capabilities` publishes as `store`,
    /// exposed for callers that need it before any dispatch — the daemon's
    /// startup line, and its idle-retirement marker (D74).
    pub fn store_path(&self) -> Option<String> {
        self.conn
            .path()
            .filter(|p| !p.is_empty())
            .map(str::to_string)
    }

    /// Whether this store has ever held a task, at all — every status, every
    /// project, no filter applied.
    ///
    /// This is the signal `task.list`, `report.summary` and `project.list`
    /// need to tell a genuinely empty store from a filter (or window) that
    /// matched nothing: those two look identical from an empty result alone,
    /// and only this count can tell them apart (#233). A store whose one task
    /// is done or cancelled is not "empty" in this sense — D24's scope rules
    /// belong to the report, not to onboarding — so this counts every row
    /// unconditionally rather than reusing any filtered read.
    pub(crate) fn store_is_empty(&self) -> Result<bool, ApiError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
        Ok(n == 0)
    }
}

// ---- free helpers -----------------------------------------------------------

fn ref_param(p: &Value) -> Result<&Value, ApiError> {
    p.get("ref")
        .ok_or_else(|| ApiError::bad_request("missing required field: ref"))
}

/// D23: assert `name` is a project this store can show — it exists and is not
/// archived. The one reader for every edge that files a task into an explicitly
/// named project (`task.add`, `task.modify`), so those two and `project.use`
/// cannot drift into disagreeing about what a usable project is.
///
/// Takes `&Connection` so callers can pass an open `Transaction` (which derefs
/// to it) and have the check share the write-locked snapshot of their INSERT —
/// the same trick `reaches` uses for cycle detection.
///
/// Unknown → `not_found` (the name is a typo, and a typo must not silently
/// swallow the task into a bucket no project surface lists). Archived →
/// `conflict`, the other half of D22: refusing an archived project as the
/// *default* while accepting it as an explicit target would leave the guard
/// half-applied, which is how the last three invisible-state bugs here worked.
/// Single-quote `value` for a POSIX shell, the way the pasted `tasqx init …`
/// hint in `require_live_project` needs: unconditionally, so the hint stays
/// one argv element even when the name holds spaces or embedded double
/// quotes. Any embedded single quote closes the quoting, escapes itself with
/// `\'`, then reopens it — the standard trick, since a single-quoted string
/// cannot contain a literal single quote.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn require_live_project(conn: &Connection, name: &str) -> Result<(), ApiError> {
    let archived: Option<i64> = conn
        .query_row(
            "SELECT archived FROM projects WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    match archived {
        None => Err(ApiError::not_found(
            format!(
                "no project named {name} (create it with `tasqx init {}`)",
                shell_quote(name)
            ),
            Some(json!({ "name": name })),
        )),
        Some(a) if a != 0 => Err(ApiError::conflict(format!(
            "project is archived: {name} (archived projects cannot take new tasks)"
        ))),
        Some(_) => Ok(()),
    }
}

/// D109: a `project:`/`proj:` value in a FILTER, validated against the live
/// projects table at the same exact-match strictness the write side already
/// has (D23's `require_live_project`, above) — but not its archived-vs-missing
/// split, because a filter is a read: an archived project's tasks are still
/// real rows, and `tasqx list project:old-thing` naming an archived project is
/// not the mistake `task.add --project old-thing` is.
///
/// `tasqx list project:FIN-9695` (wrong case) or `project:nope-does-not-exist`
/// used to print `No tasks.` at exit 0 — the same silence `status:pendign`
/// answered before D34, and D27's collapse (a typo widening or, here,
/// emptying a result set and reading as the right answer) one predicate over.
/// `status:`/`priority:` already refuse a typo because their vocabulary is
/// closed at compile time; `project:` is open at parse time (the module
/// comment's split) but not at the call sites that hold a live table — this is
/// where that gap closes. Deliberately narrow: case-sensitive exact match
/// only, no fuzzy or case-insensitive matching (out of scope).
fn validate_filter_projects(conn: &Connection, filter: &Filter) -> Result<(), ApiError> {
    for name in filter.project_names() {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM projects WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(ApiError::not_found(
                format!(
                    "no project named {name} in filter (`tasqx projects --all` lists archived \
                     ones; names are case-sensitive)"
                ),
                Some(json!({ "name": name })),
            ));
        }
    }
    Ok(())
}

/// Does `from` reach `goal` following dependency edges (from -> depends_on
/// target)? DFS over the DAG; used for cycle detection. Takes a `&Connection`
/// so it can run on either the engine connection or an open transaction
/// (`Transaction` derefs to `Connection`), letting the cycle check share the
/// write-locked snapshot of the INSERT.
fn reaches(conn: &Connection, from: &str, goal: &str) -> Result<bool, ApiError> {
    let mut stack = vec![from.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(cur) = stack.pop() {
        if cur == goal {
            return Ok(true);
        }
        if !seen.insert(cur.clone()) {
            continue;
        }
        let mut stmt = conn.prepare("SELECT depends_on_id FROM dependencies WHERE task_id = ?1")?;
        let rows = stmt.query_map(params![cur], |r| r.get::<_, String>(0))?;
        for r in rows {
            stack.push(r?);
        }
    }
    Ok(false)
}

/// Shift an optional RFC3339 instant by `delta` whole seconds, preserving
/// `None`. Used to advance a recurring instance's secondary date fields
/// (scheduled/wait) by the same amount the anchor moved.
fn shift_ts(opt: &Option<String>, delta: i64) -> Option<String> {
    opt.as_deref()
        .and_then(parse_ts)
        .and_then(|t| jiff::Timestamp::from_second(t.as_second() + delta).ok())
        .map(|t| t.to_string())
}

/// Coerce a JSON value to a SQL string-or-null, rejecting non-string/non-null.
fn nullable_string(v: &Value, field: &str) -> Result<Value, ApiError> {
    if v.is_null() {
        Ok(Value::Null)
    } else if let Some(s) = v.as_str() {
        Ok(Value::String(s.to_string()))
    } else {
        Err(ApiError::bad_request(format!(
            "{field} must be a string or null"
        )))
    }
}

/// Attach the offending task and column to a validator's own error.
///
/// `store.import` is the one caller that processes many tasks per request, so
/// `could not parse date: "whenever"` alone does not say which of them to edit.
/// This adds only that context — the rule itself stays in the validator, since a
/// second copy of the grammar here is exactly how import drifted out of step in
/// the first place.
fn import_field<T>(id: &str, field: &str, r: Result<T, ApiError>) -> Result<T, ApiError> {
    r.map_err(|e| ApiError::bad_request(format!("store.import: task {id}, {field}: {}", e.message)))
}

/// Every key a `store.export` task object can carry. D34.
///
/// Two of these are read by nobody and belong here anyway: `urgency` is
/// derived (recomputed on import from priority/due/created, so honouring a
/// supplied one would let a payload contradict the ranking rule), and
/// `status_unrecognized` is a D28 read-side annotation, not stored state.
/// "Accepted and deliberately ignored" is a different fact from "unknown", and
/// only this table can tell them apart — which is why the gate is a list of
/// what an EXPORT emits rather than a list of what the importer reads.
pub const IMPORT_TASK_KEYS: &[&str] = &[
    "id",
    "short_id",
    "title",
    "status",
    "priority",
    "project",
    "tags",
    "due",
    "scheduled",
    "wait",
    "estimate",
    "recurrence",
    "remind",
    "depends_on",
    "annotations",
    "tokens",
    "urgency",
    "created",
    "modified",
    "completed",
    "_rev",
    "status_unrecognized",
    // Absent on a legacy export and on any task that was never timed, so the
    // import reads it as `Option` and an absent value PRESERVES the stored
    // total rather than zeroing it (see the upsert's COALESCE).
    "tracked_seconds",
    // D166: the net correction inside `tracked_seconds`. Absent on a legacy
    // export and on any task never adjusted; absent preserves the stored net.
    "tracked_adjustment_seconds",
    // D139's size gauge. Absent on a legacy export, which is NULL — no
    // threshold, the same thing the store said before the column existed.
    "budget_tokens",
    // Present only while a task is `active`. Both timing columns are read
    // through the same gate as every other field: a payload that names one is
    // held to it, and one that does not keeps what the store already has.
    "active_since",
    // D138's acceptance criteria, carried whole like `annotations`.
    "checks",
    // D165's pinned delivery note. Absent on a legacy export and on any task
    // no completion pinned one on.
    "delivered_annotation_id",
    // D170: the predecessor's uuid, present only on a recurrence spawn.
    "spawned_from",
];

/// Every key an exported annotation object can carry. D34.
pub const IMPORT_ANNOTATION_KEYS: &[&str] = &["id", "body", "created"];

/// Every key an exported check object can carry (D138). D34, like the
/// annotation list above.
pub const IMPORT_CHECK_KEYS: &[&str] = &[
    "id", "body", "state", "evidence", "position", "created", "modified",
];

/// Every key an exported token measurement object can carry. D34.
///
/// Deliberately NOT `extra`: the column is reserved for later parser phases,
/// nothing writes it yet, so no export can emit it — and the day something
/// does, this gate makes forgetting the import half a loud failure instead of
/// a silently dropped field.
pub const IMPORT_TOKEN_KEYS: &[&str] = &[
    "id",
    "tool",
    "source",
    "model",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "confidence",
    "created",
    // D167: absent in every export written before the unsplit kind existed,
    // which imports as 0 — what those rows mean.
    "total_tokens",
];

/// Every key an exported memory doc object can carry. D41, held to D34's gate.
// #134/#135: `project` and `_rev` are additive (a pre-D134 export has
// neither), so a legacy document still imports; both read through
// `opt_str_nonempty`/`opt_i64` exactly as the rest of this list does.
// #101: `standing` joins them on the same terms — absent in a legacy export,
// which imports as an ordinary (non-standing) doc.
pub const IMPORT_DOC_KEYS: &[&str] = &[
    "id", "source", "title", "body", "created", "modified", "project", "_rev", "standing",
];

/// Every key an exported event object can carry (#176). `id`, `payload`,
/// `ts` and `actor` are optional so a hand-written or trimmed event still
/// imports — only `entity`, `entity_id` and `op` are the row's identity.
pub const IMPORT_EVENT_KEYS: &[&str] =
    &["id", "entity", "entity_id", "op", "payload", "ts", "actor"];

/// Every key an exported project object can carry. D34's gate, D37's record.
///
/// Deliberately NOT `default`: `project.list` marks the default row with one,
/// but the document states its default once, at the top level, where a second
/// spelling cannot disagree with the first.
pub const IMPORT_PROJECT_KEYS: &[&str] = &["id", "name", "description", "archived", "created"];

/// [`import_field`] for a project: the same "name the record to edit" context,
/// keyed by the one identifier a project row has that a human recognises.
fn import_project_field<T>(name: &str, field: &str, r: Result<T, ApiError>) -> Result<T, ApiError> {
    r.map_err(|e| {
        ApiError::bad_request(format!(
            "store.import: project {name}, {field}: {}",
            e.message
        ))
    })
}

/// [`import_field`] for a doc: same context, keyed by the doc's id.
fn import_doc_field<T>(id: &str, field: &str, r: Result<T, ApiError>) -> Result<T, ApiError> {
    r.map_err(|e| ApiError::bad_request(format!("store.import: doc {id}, {field}: {}", e.message)))
}

/// Write a project row, keyed by NAME, and answer the row's id. D37.
///
/// Name, not id, because `name` is what a task points at and what the UNIQUE
/// constraint protects: a destination that already knows `work` keeps its own
/// id and `created` (its history is real and the payload's is not more true),
/// and only the fields the document is authoritative about — description and
/// archived — are updated. The payload's id is honoured only when it is free,
/// so restoring into a FRESH store round-trips identity exactly while a
/// collision with an unrelated row yields a new id rather than the `internal`
/// error a bare PRIMARY KEY violation would surface as.
fn upsert_project(
    tx: &Transaction,
    name: &str,
    description: Option<&str>,
    archived: bool,
    created: &str,
    payload_id: Option<&str>,
) -> Result<String, ApiError> {
    if let Some(id) = tx
        .query_row(
            "SELECT id FROM projects WHERE name = ?1",
            params![name],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        tx.execute(
            "UPDATE projects SET description = ?2, archived = ?3 WHERE id = ?1",
            params![id, description, archived as i64],
        )?;
        return Ok(id);
    }
    let taken = |id: &str| -> Result<bool, ApiError> {
        Ok(tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            params![id],
            |r| r.get(0),
        )?)
    };
    let id = match payload_id {
        Some(id) if !taken(id)? => id.to_string(),
        _ => crate::clock::uuid_v7().to_string(),
    };
    tx.execute(
        "INSERT INTO projects (id, name, description, archived, created) VALUES (?1,?2,?3,?4,?5)",
        params![id, name, description, archived as i64, created],
    )?;
    Ok(id)
}

/// Require an object and refuse any key outside `accepted`. D34.
///
/// This is D33's gate one level down, and it stops here rather than at the top
/// level on purpose: the params object is an ENVELOPE, where an unknown key is
/// a future field a newer export added around the data, but a task object IS
/// the data. There is no required sibling to catch a typo the way `tasks`
/// catches `taskss`, so `{"tag":"red"}` had nothing to fail against and
/// imported as `ok:true` with the tags gone. Silently dropping a field on a
/// WRITE tells the caller their data arrived when it did not (D12, D16).
/// The shape half, split from the key half because a task's key error must name
/// the task — and the id can only be read once the value is known to be an
/// object, so the two checks cannot happen at the same moment.
fn import_shape<'a>(ctx: &str, label: &str, v: &'a Value) -> Result<&'a Value, ApiError> {
    if v.is_object() {
        return Ok(v);
    }
    Err(ApiError::bad_request(format!(
        "store.import: {ctx}{label} must be an object, but {} was given ({v})",
        crate::util::type_of(v)
    )))
}

fn import_keys(ctx: &str, label: &str, v: &Value, accepted: &[&str]) -> Result<(), ApiError> {
    let obj = import_shape(ctx, label, v)?
        .as_object()
        .expect("import_shape proved it");
    let unknown: Vec<&str> = obj
        .keys()
        .filter(|k| !accepted.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    // Naming the accepted set is the whole point: the caller mistyped a field,
    // and the fix is one glance away only if the right names are in the error.
    Err(ApiError::bad_request(format!(
        "store.import: {ctx}unknown {label} field{} {} (accepted: {}) — check the spelling or \
         drop it; it was silently ignored before, so the import reported success and the value \
         never arrived",
        if unknown.len() == 1 { "" } else { "s" },
        unknown
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", "),
        accepted.join(", ")
    )))
}

/// Read an optional date-shaped param, validated and normalized to RFC3339.
///
/// The CLI parses dates before it calls, so it can fail without a round-trip and
/// with its own nicer message — but it is not the only caller. The JSON API, MCP
/// and `store.import` land straight in the engine, and while validation lived
/// only in the CLI they stored strings the CLI rejects (`due whenever`), which
/// every reader downstream then read back as garbage. This is the gate they all
/// pass — `store.import` via [`import_field`], which only re-labels the error.
/// It delegates to [`datetime::parse_when`] rather than restating the grammar,
/// so the rule stays in exactly one place.
///
/// Normalizing on the import path is safe for D12's byte-identical round trip
/// only because an export writes the canonical form and [`datetime::parse_when`]
/// short-circuits on it; `the_date_gate_leaves_an_export_import_round_trip_byte_identical`
/// is what holds that true.
fn opt_when(p: &Value, field: &str, now: Timestamp) -> Result<Option<String>, ApiError> {
    // D35, and D13 on the engine surface: `due: ""` used to read as "no due date
    // given", so a shell variable that expanded to nothing wrote null over a
    // field it meant to set. `opt_str_nonempty` rather than letting
    // `parse_when` say "empty date expression", so the message names the field.
    match opt_str_nonempty(p, field)? {
        Some(s) => Ok(Some(datetime::parse_when(&s, now)?)),
        None => Ok(None),
    }
}

/// [`opt_when`] for a `set` entry: `null` still clears the column (`--clear`,
/// D13) and only a present string is parsed.
fn nullable_when(v: &Value, field: &str, now: Timestamp) -> Result<Value, ApiError> {
    match nullable_string(v, field)? {
        Value::String(s) => Ok(Value::String(datetime::parse_when(&s, now)?)),
        cleared => Ok(cleared),
    }
}

/// [`nullable_when`] for `estimate`, which is a duration rather than an instant.
fn nullable_duration(v: &Value, field: &str) -> Result<Value, ApiError> {
    match nullable_string(v, field)? {
        Value::String(s) => Ok(Value::String(datetime::parse_duration(&s)?)),
        cleared => Ok(cleared),
    }
}

/// Apply a single whitelisted column update inside a transaction.
fn update_column(
    tx: &rusqlite::Transaction,
    id: &str,
    col: &str,
    val: &Value,
) -> Result<(), ApiError> {
    let sql = format!("UPDATE tasks SET {col} = ?1 WHERE id = ?2");
    match val {
        Value::Null => tx.execute(&sql, params![Option::<String>::None, id])?,
        Value::String(s) => tx.execute(&sql, params![s, id])?,
        // D139's `budget_tokens` is the first modifiable column that is not
        // text. The arm is narrow on purpose — an integer, nothing else — so
        // the refusal below still catches an object or array reaching a column
        // update, which is what it was written for.
        Value::Number(n) if n.is_i64() => tx.execute(&sql, params![n.as_i64(), id])?,
        _ => return Err(ApiError::internal("non-scalar column update")),
    };
    Ok(())
}

/// Render a task as the canonical full JSON object used by `task.list`.
///
/// Not the export shape: `store_export` builds its own §3 object, so fields
/// added here for a reader's benefit cannot disturb the D12 round trip.
///
/// `tracked` includes the interval still running, up to `now` (D166) — read
/// through [`Task::tracked_at`], the one place that sum is made, which
/// `report.summary`'s `tracked_total` reads too, so the two cannot disagree
/// about the same task. It used to be the stored total alone, and every
/// surface printed `PT0S` for as long as a clock ran. `active_since` still
/// marks where the open interval began; `tracked_adjustment` is the net of
/// the `task.adjust_tracked` corrections already inside `tracked`.
pub fn task_to_json(t: &Task, tags: &[String], now: Timestamp) -> Value {
    flag_unrecognized_status(
        t,
        json!({
            "id": t.id,
            "short_id": t.short_id,
            "title": t.title,
            "status": t.status_text(),
            "priority": t.priority.map(|x| x.as_str()),
            "project": t.project,
            "due": t.due,
            "scheduled": t.scheduled,
            "wait": t.wait,
            "estimate": t.estimate,
            "tracked": iso_duration(t.tracked_at(now)),
            "tracked_adjustment": signed_iso_duration(t.tracked_adjustment_seconds),
            "active_since": t.active_since,
            "recurrence": t.recurrence,
            "remind": t.remind,
            "urgency": t.urgency,
            "tags": tags,
            "created": t.created,
            "modified": t.modified,
            "completed": t.completed,
            // D139: null when unset, which is a true answer rather than a
            // silent zero — zero is a real budget somebody might set.
            "budget_tokens": t.budget_tokens,
            "_rev": t.rev,
        }),
    )
}

/// One `task.list` row: the canonical task object plus the derived `blocked`
/// fact.
///
/// `blocked` is set here rather than inside `task_to_json` because it is a fact
/// about the store, not a column, and `task.get`/`store.export` must keep
/// saying it (or not) exactly once themselves. It is a named function rather
/// than two lines in the loop so [`TASK_FIELDS`] can be read off the very
/// object the loop projects — one call site's keys, not a second list that has
/// to be remembered.
fn list_row_json(
    t: &Task,
    tags: &[String],
    blocked: bool,
    depends_on: Option<&[i64]>,
    tokens: Option<&Value>,
    now: Timestamp,
) -> Value {
    let mut v = task_to_json(t, tags, now);
    v["blocked"] = json!(blocked);
    // Emitted only when the caller projected it (D70). It is a name
    // `TASK_FIELDS` publishes and `parse_fields` accepts, and it is absent from
    // the default row on purpose: the edge list costs a statement and a
    // per-row array that every unfiltered `tasqx list` would pay for, on a
    // surface whose default response was already the largest thing this server
    // sends. Conditional emission has precedent one function down in
    // `status_unrecognized`.
    if let Some(d) = depends_on {
        v["depends_on"] = json!(d);
    }
    // Same gate, same reason, for the roll-up #215 added: a task's own
    // measurements are already read only when `parts.tokens` is set, and this
    // is that read's one consumer besides `report.summary`. The value is the
    // four-bucket object `engine::tokens::bucket_json` renders — never a
    // blended total (D48) — so a caller sees exactly what a measurement row
    // shows, summed.
    if let Some(tk) = tokens {
        v["tokens"] = tk.clone();
    }
    v
}

/// Add `status_unrecognized: true` when — and only when — the row's status is a
/// value no writer of this engine could have produced.
///
/// The key is absent on every well-formed task on purpose: a boolean that is
/// almost always `false` is noise on every surface that renders it, and its
/// absence keeps the §3 export shape byte-identical for stores that never hit
/// the bug. It exists at all because `status` alone carries the anomaly as a
/// *string*, which a machine consumer would have to recognize by not matching
/// anything — the same "squint at every reader" the D23 note rejected.
fn flag_unrecognized_status(t: &Task, mut v: Value) -> Value {
    if t.status_is_unrecognized() {
        v["status_unrecognized"] = Value::Bool(true);
    }
    v
}

/// A single sort directive: column key + descending flag.
struct SortKey {
    key: String,
    desc: bool,
}

/// Parse the `sort` param into directives, REFUSING any key `compare_by` does
/// not implement.
///
/// It used to accept anything and let `compare_by` fall through to "equal", so
/// `sort:["bogus"]` returned rows in an order the caller never asked for, with
/// exit 0 and no way to notice. Same family as D27's unknown filter token and
/// the invalid `!priority`: on a READ path nothing is lost by refusing, and
/// refusing is the only thing that turns a wrong answer into a fixable one.
fn parse_sort(p: &Value) -> Result<Vec<SortKey>, ApiError> {
    let mut keys: Vec<SortKey> = Vec::new();
    // D32: `sort: "urgency"` (a bare string, the obvious thing to type) used to
    // vanish here, and the rows came back in an order nobody asked for.
    for s in opt_str_array(p, "sort")? {
        // Strip the direction prefix BEFORE validating, or `-bogus` would
        // slip past a check that only ever saw the raw token.
        let (key, desc) = match s.strip_prefix('-') {
            Some(rest) => (rest, true),
            None => (s.as_str(), false),
        };
        if !SORT_KEYS.contains(&key) {
            return Err(ApiError::bad_request(format!(
                "unknown sort key \"{key}\" (valid keys: {}; prefix any with `-` for descending)",
                SORT_KEYS.join(", ")
            )));
        }
        keys.push(SortKey {
            key: key.to_string(),
            desc,
        });
    }
    if keys.is_empty() {
        // The documented default, spelled from the same list it validates.
        keys.push(SortKey {
            key: SORT_KEYS[0].to_string(),
            desc: true,
        });
    }
    Ok(keys)
}

/// Parse the `fields` param into a projection list, REFUSING any name the
/// projection does not emit.
///
/// The loop used to keep a key only when `full.get(k)` hit, so a typo'd name
/// was dropped without a word: `fields:["short_id","titel"]` answered `ok` with
/// rows that simply lacked the column, and a script built on it renders blanks
/// forever with nothing to notice. Same family as D27's unknown filter token,
/// the invalid `!priority` and the unknown sort key one function up: on a READ
/// path nothing is lost by refusing — the caller retypes — while a silent wrong
/// answer is unfalsifiable.
///
/// A non-string entry is refused for the same reason rather than skipped: it is
/// a caller asking for something that cannot be a field name.
/// Whether a parsed `fields` projection asks for the dependency edge list.
///
/// Named rather than inlined because two things read it: the snapshot loader
/// (which must fetch the edges) and the projection loop (which must emit them),
/// and the two disagreeing is a row that promises a field it does not carry.
pub(super) fn fields_want_depends_on(fields: Option<&Vec<String>>) -> bool {
    fields.is_some_and(|keys| keys.iter().any(|k| k == "depends_on"))
}

/// Whether a parsed `fields` projection asks for the `tokens` roll-up (#215).
///
/// Same shape and same reason as [`fields_want_depends_on`]: the snapshot
/// loader must fetch the measurements to compute it, and the projection loop
/// must emit them, with `SnapshotParts::tokens` as the one gate both consult
/// so they cannot disagree about whether the row promises the field.
pub(super) fn fields_want_tokens(fields: Option<&Vec<String>>) -> bool {
    fields.is_some_and(|keys| keys.iter().any(|k| k == "tokens"))
}

fn parse_fields(p: &Value) -> Result<Option<Vec<String>>, ApiError> {
    // D32: the array-ness and the string-ness of every entry are the typed
    // layer's job now; only the *name* check is specific to this param.
    if opt_array(p, "fields")?.is_none() {
        return Ok(None);
    }
    let keys = opt_str_array(p, "fields")?;
    for k in &keys {
        if !TASK_FIELDS.iter().any(|f| f == k) {
            return Err(ApiError::bad_request(format!(
                "unknown field \"{k}\" (valid fields: {})",
                TASK_FIELDS.join(", ")
            )));
        }
    }
    // #76.1: `fields: []` used to mean "restrict every row to the empty set",
    // i.e. `{}` per row — the one reading nobody wants and the schema does not
    // document. The schema's own words are "restrict to THESE fields"; naming
    // none is naming no restriction, the same as omitting the param entirely,
    // not a request for content-free rows.
    if keys.is_empty() {
        return Ok(None);
    }
    Ok(Some(keys))
}

fn priority_rank(p: Option<Priority>) -> i32 {
    match p {
        Some(Priority::H) => 0,
        Some(Priority::M) => 1,
        Some(Priority::L) => 2,
        None => 3,
    }
}

fn compare_by(
    a: &Task,
    b: &Task,
    a_tokens: u64,
    b_tokens: u64,
    keys: &[SortKey],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for k in keys {
        let ord = match k.key.as_str() {
            "urgency" => a.urgency.partial_cmp(&b.urgency).unwrap_or(Ordering::Equal),
            "short_id" => a.short_id.cmp(&b.short_id),
            "priority" => priority_rank(a.priority).cmp(&priority_rank(b.priority)),
            "due" => opt_cmp(&a.due, &b.due),
            // D144: `util::now` trims the fractional second at a whole
            // second, so under BINARY/`str::cmp` 'Z' outranks every digit and
            // '.' — `...10Z` would sort above `...10.9Z`. Strip the
            // terminator before comparing, same normalisation `memory.list`
            // applies via `rtrim(modified, 'Z')` (#87).
            "created" => a
                .created
                .trim_end_matches('Z')
                .cmp(b.created.trim_end_matches('Z')),
            "modified" => a
                .modified
                .trim_end_matches('Z')
                .cmp(b.modified.trim_end_matches('Z')),
            "title" => a.title.cmp(&b.title),
            // #215: a total order for "most expensive first" over the
            // saturating sum of a task's four measurement buckets. The sum
            // itself is never reported (D48/D50) — only used to rank here.
            "tokens" => a_tokens.cmp(&b_tokens),
            // #663/D173: duration order, not string order — see SORT_KEYS's
            // doc for why `PT4H` must outrank `PT90M`. `k.desc` is threaded
            // into the match arm itself (see `opt_magnitude_cmp_last`) rather
            // than left to the blanket reversal below, which is exactly what
            // leaves it OUT of the value comparison: the two magnitudes stay
            // direction-agnostic here and let that reversal flip them, while
            // the "no value" placement is pre-negated so the SAME reversal
            // cancels back out to "last" either way.
            "estimate" => opt_magnitude_cmp_last(
                a.estimate.as_deref().and_then(duration_secs),
                b.estimate.as_deref().and_then(duration_secs),
                k.desc,
            ),
            "tracked" => opt_magnitude_cmp_last(
                Some(a.tracked_seconds).filter(|&s| s != 0),
                Some(b.tracked_seconds).filter(|&s| s != 0),
                k.desc,
            ),
            // Unreachable via the API: `parse_sort` rejects anything not in
            // SORT_KEYS. It stays as a total match rather than a panic because
            // this is a read path, and a test drives every published key
            // through here so a name added to the constant without an arm
            // added here goes red instead of silently sorting by nothing.
            _ => Ordering::Equal,
        };
        let ord = if k.desc { ord.reverse() } else { ord };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    // The final tiebreak, unconditional and never reversed by `desc` (D70).
    // Every published sort key has ties — the live store holds six tasks at
    // urgency 6.2 — and a tie broken by whatever order the rows arrived in is
    // an order that can differ between two calls. That is invisible on one
    // whole answer and corrupting the moment `offset` walks pages over it: a
    // row shows up twice, or never, with nothing for the reader to notice.
    // Same correction D63 made for the annotation window, one relation over.
    a.short_id.cmp(&b.short_id)
}

/// Compare two optional strings, ordering `None` last regardless of direction.
fn opt_cmp(a: &Option<String>, b: &Option<String>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Compare two optional magnitudes (D173), placing the missing one LAST
/// whichever way `desc` points — unlike [`opt_cmp`] above, whose "`None`
/// last" is true only for the ascending caller and is silently undone the
/// moment `compare_by`'s blanket `if k.desc { ord.reverse() }` runs on its
/// answer too. `desc` therefore has to reach INTO this function rather than
/// stay outside it, and only half the match needs it:
///
///  * `(Some, Some)` stays a plain, direction-agnostic `cmp` — the caller's
///    reversal is exactly what turns that into "descending", so pre-flipping
///    it here would cancel out.
///  * `(Some, None)`/`(None, Some)` is pre-negated for `desc`, so that the
///    SAME reversal lands back on "the value sorts before the missing one"
///    either way, instead of flipping the "last" placement along with the
///    order it belongs to.
fn opt_magnitude_cmp_last(a: Option<i64>, b: Option<i64>, desc: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(&y),
        // The missing side sorts after; pre-flipped so compare_by's desc reversal restores it.
        _ => {
            let o = b.is_some().cmp(&a.is_some());
            if desc {
                o.reverse()
            } else {
                o
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `compare_by` is a TOTAL order: it never calls two distinct tasks equal,
    /// on any published sort key, in either direction.
    ///
    /// Without that, `offset` paging is unsound. Two tasks tying on the sort
    /// key are ordered by whatever the underlying sort does with equals, which
    /// is not a contract — and a page boundary landing inside a tie shows a row
    /// twice or skips it, with nothing in either response for a reader to
    /// notice. Same correction D63 made for the annotation window, which
    /// carries `id` as a tiebreak in both directions for exactly this reason.
    #[test]
    fn compare_by_never_calls_two_distinct_tasks_equal() {
        use std::cmp::Ordering;

        let a = Task {
            id: "a".to_string(),
            short_id: 1,
            title: "same".to_string(),
            status: Status::Pending,
            status_raw: None,
            priority: Some(Priority::M),
            project: None,
            due: None,
            scheduled: None,
            wait: None,
            estimate: None,
            recurrence: None,
            remind: None,
            urgency: 6.2,
            active_since: None,
            tracked_seconds: 0,
            rev: 1,
            created: "2026-08-30T12:00:00Z".to_string(),
            modified: "2026-08-30T12:00:00Z".to_string(),
            completed: None,
            budget_tokens: None,
            delivered_annotation_id: None,
            tracked_adjustment_seconds: 0,
        };
        let mut b = a.clone();
        b.id = "b".to_string();
        b.short_id = 2;

        for key in SORT_KEYS.iter() {
            for desc in [false, true] {
                let keys = vec![SortKey {
                    key: (*key).to_string(),
                    desc,
                }];
                let ord = compare_by(&a, &b, 0, 0, &keys);
                assert_ne!(
                    ord,
                    Ordering::Equal,
                    "sort key `{key}` (desc={desc}) leaves #1 and #2 unordered"
                );
                assert_eq!(
                    compare_by(&b, &a, 0, 0, &keys),
                    ord.reverse(),
                    "sort key `{key}` (desc={desc}) is not antisymmetric"
                );
            }
        }

        // And the tiebreak is ascending short_id whichever way the named key
        // points: it is a stabiliser, not a second sort the caller asked for.
        for desc in [false, true] {
            let keys = vec![SortKey {
                key: "urgency".to_string(),
                desc,
            }];
            assert_eq!(compare_by(&a, &b, 0, 0, &keys), Ordering::Less);
        }
    }

    /// #87: `created`/`modified` are `util::now` stamps whose fractional
    /// second is trimmed at a whole second, so under plain text comparison
    /// `...10Z` (BINARY 'Z' > any digit or '.') outranks `...10.9Z`, and
    /// `...10.12Z` outranks `...10.123Z` — a task can sort as newer than one
    /// that is chronologically later. D144 ruled the fix for `memory.list`:
    /// strip the trailing 'Z' terminator before comparing. `compare_by` must
    /// do the same for `sort:created`/`sort:modified`, in true time order.
    #[test]
    fn compare_by_created_and_modified_strip_terminator_before_comparing() {
        use std::cmp::Ordering;

        fn task_at(short_id: i64, stamp: &str) -> Task {
            Task {
                id: short_id.to_string(),
                short_id,
                title: "same".to_string(),
                status: Status::Pending,
                status_raw: None,
                priority: None,
                project: None,
                due: None,
                scheduled: None,
                wait: None,
                estimate: None,
                recurrence: None,
                remind: None,
                urgency: 0.0,
                active_since: None,
                tracked_seconds: 0,
                rev: 1,
                created: stamp.to_string(),
                modified: stamp.to_string(),
                completed: None,
                budget_tokens: None,
                delivered_annotation_id: None,
                tracked_adjustment_seconds: 0,
            }
        }

        // Chronological order, oldest first — but the text is inverted: a
        // longer fraction sorts as text-smaller because 'Z' beats every
        // digit and '.'.
        let oldest = task_at(1, "2026-08-30T12:00:10Z");
        let a_tenth = task_at(2, "2026-08-30T12:00:10.1Z");
        let a_hundredth = task_at(3, "2026-08-30T12:00:10.12Z");
        let newest = task_at(4, "2026-08-30T12:00:10.123Z");

        for field in ["created", "modified"] {
            let asc = vec![SortKey {
                key: field.to_string(),
                desc: false,
            }];
            assert_eq!(
                compare_by(&oldest, &a_tenth, 0, 0, &asc),
                Ordering::Less,
                "`{field}` ascending: whole-second stamp must sort before a \
                 later fractional one"
            );
            assert_eq!(
                compare_by(&a_hundredth, &newest, 0, 0, &asc),
                Ordering::Less,
                "`{field}` ascending: a shorter fraction must sort before a \
                 longer, later one"
            );

            let desc = vec![SortKey {
                key: field.to_string(),
                desc: true,
            }];
            assert_eq!(
                compare_by(&newest, &oldest, 0, 0, &desc),
                Ordering::Less,
                "`{field}` descending: the newest stamp must sort first"
            );
        }
    }

    /// tasqx audit 2026-09 #155: the `bad_request` a bad ref gets back named
    /// what the ref WAS, never what a ref is ALLOWED to be — "ref is neither
    /// short_id nor UUID: @active" gives no reader still holding DESIGN.md §5
    /// (which advertises ranges, comma lists and `@active`/`@last`) anywhere to
    /// go, because the message that should say "and here is what actually
    /// works" instead just restates the input back at them.
    #[test]
    fn ref_error_names_what_a_ref_may_actually_be() {
        let e = Engine::open_in_memory().unwrap();
        let err = e
            .resolve_ref_value(&json!("@active"))
            .expect_err("@active is not a resolvable ref");
        assert!(
            err.message.contains("short_id"),
            "must still name short_id: {}",
            err.message
        );
        assert!(
            err.message.contains("UUID"),
            "must still name UUID: {}",
            err.message
        );
        assert!(
            err.message.to_lowercase().contains("expected"),
            "must say what a ref may actually be, not only what this one \
             was not: {}",
            err.message
        );
    }

    /// #215: `tokens` ranks by the sum of the four buckets, independent of
    /// every other field — the `-tokens` sort key itself, isolated from
    /// `task_list`'s snapshot plumbing.
    #[test]
    fn compare_by_tokens_ranks_by_the_saturating_sum() {
        use std::cmp::Ordering;

        let a = Task {
            id: "a".to_string(),
            short_id: 1,
            title: "cheap".to_string(),
            status: Status::Pending,
            status_raw: None,
            priority: None,
            project: None,
            due: None,
            scheduled: None,
            wait: None,
            estimate: None,
            recurrence: None,
            remind: None,
            urgency: 0.0,
            active_since: None,
            tracked_seconds: 0,
            rev: 1,
            created: "2026-08-30T12:00:00Z".to_string(),
            modified: "2026-08-30T12:00:00Z".to_string(),
            completed: None,
            budget_tokens: None,
            delivered_annotation_id: None,
            tracked_adjustment_seconds: 0,
        };
        let mut b = a.clone();
        b.id = "b".to_string();
        b.short_id = 2;

        let keys = vec![SortKey {
            key: "tokens".to_string(),
            desc: true,
        }];
        // b spent more: descending `-tokens` must put b first.
        assert_eq!(compare_by(&a, &b, 15, 3_300, &keys), Ordering::Greater);
        assert_eq!(compare_by(&b, &a, 3_300, 15, &keys), Ordering::Less);
    }

    use crate::error::ErrorCode;

    #[test]
    fn lifecycle_commands_parse_once_and_preserve_wire_results() {
        let start = commands::parse_start_task(&json!({ "ref": "42", "keep": true })).unwrap();
        assert_eq!(start.target.value, json!("42"));
        assert!(start.keep);

        let started: Value = commands::TaskStarted {
            id: "task-id".to_string(),
            interval_started: Some("2026-07-20T12:00:00Z".to_string()),
            short_id: 42,
            title: "the title".to_string(),
            already_running: false,
            auto_stopped: Vec::new(),
        }
        .into();
        assert_eq!(
            started,
            json!({
                "id": "task-id",
                "status": "active",
                "interval_started": "2026-07-20T12:00:00Z",
                "short_id": 42,
                "title": "the title",
                "already_running": false,
                "auto_stopped": [],
            })
        );

        let cancelled: Value = commands::TaskCancelled {
            short_id: 7,
            unblocked: vec![8, 9],
        }
        .into();
        assert_eq!(
            cancelled,
            json!({ "short_id": 7, "status": "cancelled", "unblocked": [8, 9] })
        );
    }

    #[test]
    fn every_mutation_locks_before_authoritative_reads() {
        let source = [
            include_str!("engine.rs"),
            include_str!("engine/commands.rs"),
            include_str!("engine/memory.rs"),
            include_str!("engine/projects.rs"),
            include_str!("engine/relationships.rs"),
            include_str!("engine/reports.rs"),
            include_str!("engine/task.rs"),
            include_str!("engine/tokens.rs"),
            include_str!("engine/transfer.rs"),
        ]
        .join("\n");
        let handlers = [
            "project_create",
            "project_use",
            "project_archive",
            "task_add",
            "task_start",
            "task_stop",
            "task_done",
            "task_modify",
            "task_cancel",
            "task_reopen",
            "tag_add",
            "annotation_add",
            "dependency_add",
            "dependency_remove",
            "memory_add",
            "memory_remove",
            "memory_import",
            "store_import",
            "reminder_fire",
            "token_add",
            "token_attribute",
        ];

        for handler in handlers {
            let marker = format!("pub fn {handler}(");
            let start = source
                .find(&marker)
                .unwrap_or_else(|| panic!("missing mutation {handler}"));
            let rest = &source[start..];
            let end = rest[marker.len()..]
                .find("\n    pub fn ")
                .map(|offset| marker.len() + offset)
                .unwrap_or(rest.len());
            let body = &rest[..end];
            let lock = body
                .find("self.begin_mutation()")
                .unwrap_or_else(|| panic!("{handler} must acquire a MutationContext"));
            let before_lock = &body[..lock];
            for forbidden in [
                "self.resolve_ref(",
                "self.resolve_ref_on(",
                "self.task_by_",
                "self.default_project(",
                "self.conn.",
            ] {
                assert!(
                    !before_lock.contains(forbidden),
                    "{handler} performs authoritative read {forbidden} before begin_mutation"
                );
            }
        }
    }

    #[test]
    fn task_snapshot_statement_count_is_independent_of_task_count() {
        let statement_count = |task_count: usize| {
            let e = Engine::open_in_memory().unwrap();
            for n in 0..task_count {
                e.task_add(&json!({ "title": format!("task {n}"), "tags": ["shared"] }))
                    .unwrap();
            }
            let (snapshots, statements) = e.load_task_snapshots_counted().unwrap();
            assert_eq!(snapshots.len(), task_count);
            statements
        };

        let empty = statement_count(0);
        assert_eq!(empty, SNAPSHOT_QUERY_COUNT);
        assert_eq!(statement_count(1), empty);
        assert_eq!(statement_count(32), empty);
    }

    /// Manual fixture, deliberately ignored in CI: it gives maintainers a
    /// repeatable before/after measurement without turning noisy wall-clock
    /// timing into a correctness gate.
    #[test]
    #[ignore = "manual 1k/10k task snapshot benchmark"]
    fn benchmark_task_snapshot_bulk_readers() {
        use std::time::Instant;

        for task_count in [1_000usize, 10_000] {
            let e = Engine::open_in_memory().unwrap();
            let ids: Vec<String> = (1..=task_count)
                .map(|n| format!("019f7eb6-0000-7000-8000-{n:012x}"))
                .collect();
            let tasks: Vec<Value> = ids
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    let depends_on = if index > 0 && index % 2 == 1 {
                        vec![ids[index - 1].clone()]
                    } else {
                        Vec::new()
                    };
                    json!({
                        "id": id,
                        "short_id": index + 1,
                        "title": format!("benchmark task {index}"),
                        "tags": ["shared", format!("bucket-{}", index % 10)],
                        "depends_on": depends_on,
                    })
                })
                .collect();
            e.store_import(&json!({ "tasks": tasks })).unwrap();

            let started = Instant::now();
            let listed = e.task_list(&json!({})).unwrap();
            let list_elapsed = started.elapsed();
            let started = Instant::now();
            let report = e.report_summary(&json!({})).unwrap();
            let report_elapsed = started.elapsed();
            let started = Instant::now();
            let exported = e.store_export(&json!({})).unwrap();
            let export_elapsed = started.elapsed();

            assert_eq!(listed["count"], task_count);
            assert_eq!(report["groups"][0]["count"], task_count);
            assert_eq!(exported["tasks"].as_array().unwrap().len(), task_count);
            eprintln!(
                "{task_count} tasks: list={list_elapsed:?}, report={report_elapsed:?}, export={export_elapsed:?}"
            );
        }
    }

    /// `depends_on_ids` (the export reader) must agree with `is_blocked` /
    /// `depends_on_short_ids` (the display readers, which INNER JOIN `tasks`).
    /// A dangling edge contributes zero rows there, so it was invisible to the
    /// user yet still exported — an export that referenced a task nobody could
    /// see. The FOREIGN KEY makes this state unreachable going forward; legacy
    /// stores are cleaned by the migration. This pins the reader itself.
    #[test]
    fn depends_on_ids_ignores_a_dangling_edge() {
        let e = Engine::open_in_memory().unwrap();
        let blocker = e.task_add(&json!({ "title": "blocker" })).unwrap();
        let dependent = e.task_add(&json!({ "title": "dependent" })).unwrap();
        e.dependency_add(&json!({
            "ref": dependent["short_id"].clone(),
            "depends_on": blocker["short_id"].clone()
        }))
        .unwrap();
        let (bid, did) = (
            blocker["id"].as_str().unwrap().to_string(),
            dependent["id"].as_str().unwrap().to_string(),
        );
        assert_eq!(e.depends_on_ids(&did).unwrap(), vec![bid.clone()]);

        // Forge the pre-FOREIGN-KEY state: orphan the edge behind SQLite's back.
        e.conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        e.conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![bid])
            .unwrap();
        e.conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        assert!(e.depends_on_short_ids(&did).unwrap().is_empty());
        assert!(!e.is_blocked(&did).unwrap());
        assert!(
            e.depends_on_ids(&did).unwrap().is_empty(),
            "the export reader must not see what no other reader can see"
        );
    }

    /// D24 applies to every metric, not just `count`. `tracked_total` is the one
    /// that matters most in both directions: time logged against a *cancelled*
    /// task is time spent on work that was abandoned and must not inflate the
    /// total, while time logged against a *done* task is the bulk of all tracked
    /// time and must survive. Excluding done alongside cancelled would leave
    /// `tracked_total` reading ~PT0S on a mature store.
    ///
    /// Elapsed wall-clock through `task_start`/`task_stop` is 0s in a test, so
    /// the tracked totals are written directly.
    #[test]
    fn report_summary_tracked_total_drops_cancelled_but_keeps_done() {
        let e = Engine::open_in_memory().unwrap();
        let done = e.task_add(&json!({ "title": "done" })).unwrap();
        let gone = e.task_add(&json!({ "title": "cancelled" })).unwrap();
        e.task_done(&json!({ "ref": done["short_id"].clone() }))
            .unwrap();
        e.task_cancel(&json!({ "ref": gone["short_id"].clone() }))
            .unwrap();
        for (row, secs) in [(&done, 3600i64), (&gone, 1800i64)] {
            e.conn
                .execute(
                    "UPDATE tasks SET tracked_seconds=?1 WHERE id=?2",
                    params![secs, row["id"].as_str().unwrap()],
                )
                .unwrap();
        }

        let g = |p: Value| -> Value { e.report_summary(&p).unwrap()["groups"][0].clone() };
        assert_eq!(
            g(json!({ "metrics": ["tracked_total"] }))["tracked_total"],
            "PT1H",
            "the done task's hour survives; the cancelled task's half hour does not"
        );
        assert_eq!(
            g(json!({ "all": true, "metrics": ["tracked_total"] }))["tracked_total"],
            "PT1H30M",
            "`all` restores the cancelled task's tracked time"
        );
    }

    /// Forge a task's stored tracked total. Elapsed wall-clock through
    /// `task_start`/`task_stop` is 0s in a test, so every case below that needs
    /// a non-zero total writes it directly — the same device as
    /// `report_summary_tracked_total_drops_cancelled_but_keeps_done` above.
    fn forge_tracked(e: &Engine, task: &Value, secs: i64) {
        e.conn
            .execute(
                "UPDATE tasks SET tracked_seconds=?1 WHERE id=?2",
                params![secs, task["id"].as_str().unwrap()],
            )
            .unwrap();
    }

    /// `export_task` emitted every §3 field except the two timing columns, and
    /// the import upsert hardcoded `tracked_seconds=0`. So a full
    /// `store.export` -> `store.import` — the only backup/restore path tasqx has
    /// (D12: "an export is self-contained") — reported `ok` and the right
    /// `imported` count while silently zeroing every task's tracked time.
    #[test]
    fn export_import_round_trip_preserves_tracked_time() {
        let a = Engine::open_in_memory().unwrap();
        let t = a.task_add(&json!({ "title": "timed" })).unwrap();
        forge_tracked(&a, &t, 5445); // 1h30m45s

        let doc = a.store_export(&json!({})).unwrap();
        assert_eq!(
            doc["tasks"][0]["tracked_seconds"], 5445,
            "the export must carry the stored total verbatim"
        );

        let b = Engine::open_in_memory().unwrap();
        b.store_import(&doc).unwrap();
        let got = b.task_get(&json!({ "ref": 1 })).unwrap();
        assert_eq!(
            got["tracked"], "PT1H30M45S",
            "tracked time must survive a restore into a fresh store"
        );
    }

    /// Emitted only when non-zero: a new export stays importable by an older
    /// tasqx (whose `IMPORT_TASK_KEYS` gate is closed) for every task that was
    /// never timed, which is most of them.
    #[test]
    fn export_omits_tracked_seconds_only_when_no_time_was_tracked() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "never timed" })).unwrap();
        let timed = e.task_add(&json!({ "title": "timed" })).unwrap();
        forge_tracked(&e, &timed, 60);

        let doc = e.store_export(&json!({})).unwrap();
        // Both halves, so the test cannot pass by the key being absent
        // everywhere — which is exactly the bug it guards.
        assert!(
            doc["tasks"][0].get("tracked_seconds").is_none(),
            "an untimed task must not carry the key: {}",
            doc["tasks"][0]
        );
        assert_eq!(
            doc["tasks"][1]["tracked_seconds"], 60,
            "a timed task must carry it"
        );
    }

    /// A legacy export has no `tracked_seconds` key at all. Merge-importing one
    /// on top of a live store (DESIGN.md:364 calls that a normal workflow) must
    /// not read "absent" as "zero" and wipe the tracked time already stored.
    #[test]
    fn import_without_tracked_seconds_preserves_the_stored_total() {
        let e = Engine::open_in_memory().unwrap();
        let t = e.task_add(&json!({ "title": "timed" })).unwrap();
        forge_tracked(&e, &t, 3600);

        let mut doc = e.store_export(&json!({})).unwrap();
        doc["tasks"][0]
            .as_object_mut()
            .unwrap()
            .remove("tracked_seconds")
            .expect("precondition: the export carries the key");

        e.store_import(&doc).unwrap();
        let got = e.task_get(&json!({ "ref": 1 })).unwrap();
        assert_eq!(
            got["tracked"], "PT1H",
            "an absent key must preserve the stored total, not zero it"
        );
    }

    /// The upsert's SET list omitted `active_since`, so importing a payload
    /// whose status is terminal over a task that is currently running left the
    /// live anchor in place: a `done` task with an open timing interval, which
    /// no sequence of API calls can reach and nothing sweeps back out (the
    /// active sweep selects `WHERE status='active'`).
    #[test]
    fn import_of_a_terminal_status_over_a_running_task_clears_the_anchor() {
        let e = Engine::open_in_memory().unwrap();
        let t = e.task_add(&json!({ "title": "running" })).unwrap();
        e.task_start(&json!({ "ref": t["short_id"].clone() }))
            .unwrap();

        let mut doc = e.store_export(&json!({})).unwrap();
        assert_eq!(doc["tasks"][0]["status"], "active", "precondition");
        doc["tasks"][0]["status"] = json!("done");

        e.store_import(&doc).unwrap();
        let got = e.task_get(&json!({ "ref": 1 })).unwrap();
        assert_eq!(got["status"], "done");
        assert!(
            got["active_since"].is_null(),
            "a terminal status must leave no open interval: {}",
            got["active_since"]
        );
    }

    /// The mirror hole on the INSERT branch: `active_since` was hardcoded NULL,
    /// so a payload claiming `status:"active"` landed in a fresh store with no
    /// anchor. `seconds_between` reads a missing anchor as zero elapsed, so the
    /// next `stop` answered `PT0S` and the interval was lost.
    #[test]
    fn import_of_an_active_status_into_a_fresh_store_sets_an_anchor() {
        let a = Engine::open_in_memory().unwrap();
        let t = a.task_add(&json!({ "title": "running" })).unwrap();
        a.task_start(&json!({ "ref": t["short_id"].clone() }))
            .unwrap();
        let doc = a.store_export(&json!({})).unwrap();

        let b = Engine::open_in_memory().unwrap();
        b.store_import(&doc).unwrap();
        let got = b.task_get(&json!({ "ref": 1 })).unwrap();
        assert_eq!(got["status"], "active");
        assert!(
            got["active_since"].is_string(),
            "an active task must import with a usable anchor: {}",
            got["active_since"]
        );
    }

    /// Date/duration validation used to live only in the CLI, so the JSON API
    /// (and MCP, and `store.import`, which all land here) stored whatever string
    /// they were handed: `tasqx add "x" due:whenever` errored while the same
    /// value through `task.add` succeeded and `tasqx show` printed `due
    /// whenever`. The gate belongs in the core, where every surface passes.
    #[test]
    fn task_add_rejects_unparseable_dates_and_durations() {
        let e = Engine::open_in_memory().unwrap();
        for (field, bad) in [
            ("due", "whenever"),
            ("scheduled", "whenever"),
            ("wait", "whenever"),
            ("estimate", "soonish"),
        ] {
            let err = e
                .task_add(&json!({ "title": "x", field: bad }))
                .expect_err("the core must reject what the CLI rejects");
            assert_eq!(err.code, ErrorCode::BadRequest, "{field}");
            assert!(
                err.message.contains(bad),
                "{field}: {} must name the offending value",
                err.message
            );
        }
    }

    /// The same hole on the modify side: a task that was added clean could still
    /// be poisoned by a later `task.modify`.
    #[test]
    fn task_modify_rejects_unparseable_dates_and_durations() {
        let e = Engine::open_in_memory().unwrap();
        let t = e.task_add(&json!({ "title": "x" })).unwrap();
        for (field, bad) in [
            ("due", "whenever"),
            ("scheduled", "whenever"),
            ("wait", "whenever"),
            ("estimate", "soonish"),
            ("tracked", "soonish"),
        ] {
            let err = e
                .task_modify(&json!({ "ref": t["short_id"].clone(), "set": { field: bad } }))
                .expect_err("the core must reject what the CLI rejects");
            assert_eq!(err.code, ErrorCode::BadRequest, "{field}");
            assert!(
                err.message.contains(bad),
                "{field}: {} must name the offending value",
                err.message
            );
        }
    }

    /// #186 (audit-2026-09, D98): tracked time could not be corrected on any
    /// surface. `task.modify {"set":{"tracked":"PT2H30M"}}` answered
    /// `bad_request: field not modifiable: tracked`, `modify --help` carried
    /// no `--tracked` flag, and `tracked` was absent from `--clear`'s closed
    /// set — so a timer left running overnight permanently poisoned the
    /// total the moment anything else was logged, `undo` reaching back only
    /// one event. `tracked` now takes the same duration grammar `estimate`
    /// does, resolved to whole seconds because `tracked_seconds` is the
    /// column (not an opaque string like `estimate`'s), and `null` clears it
    /// to `PT0S` — there is no "never tracked" state distinct from zero.
    #[test]
    fn task_modify_can_correct_the_tracked_total() {
        let e = Engine::open_in_memory().unwrap();
        let t = e.task_add(&json!({ "title": "billable A" })).unwrap();
        let r#ref = t["short_id"].clone();
        e.task_start(&json!({ "ref": r#ref.clone() })).unwrap();
        e.task_stop(&json!({ "ref": r#ref.clone() })).unwrap();
        let before = e.task_get(&json!({ "ref": r#ref.clone() })).unwrap();
        assert_ne!(
            before["tracked"], "PT2H30M",
            "the fixture must not already read the target value"
        );

        e.task_modify(&json!({ "ref": r#ref.clone(), "set": { "tracked": "PT2H30M" } }))
            .expect("an overnight timer must be correctable");
        assert_eq!(
            e.task_get(&json!({ "ref": r#ref.clone() })).unwrap()["tracked"],
            "PT2H30M",
            "an explicit correction must overwrite the stored total, not add to it"
        );

        e.task_modify(&json!({ "ref": r#ref.clone(), "set": { "tracked": null } }))
            .expect("tracked must be clearable, same as estimate");
        assert_eq!(
            e.task_get(&json!({ "ref": r#ref })).unwrap()["tracked"],
            "PT0S",
            "clearing tracked resets it to the same total a never-timed task reports"
        );
    }

    /// The gate must not eat the two shapes D13 depends on: an absent field is
    /// untouched, an explicit null clears (`--clear`). Also pins that a good
    /// value still lands normalized, so the gate is a parser and not a filter.
    #[test]
    fn date_gate_preserves_absent_and_null_and_normalizes() {
        let e = Engine::open_in_memory().unwrap();
        let t = e
            .task_add(&json!({ "title": "x", "due": "2026-07-20", "estimate": "4h" }))
            .unwrap();
        let r#ref = t["short_id"].clone();
        let get = |e: &Engine| e.task_get(&json!({ "ref": r#ref })).unwrap();
        assert_eq!(
            get(&e)["due"],
            "2026-07-20T00:00:00Z",
            "a bare ISO date resolves to midnight UTC"
        );
        assert_eq!(
            get(&e)["estimate"],
            "PT4H",
            "a human duration is stored ISO"
        );

        // Absent: modifying only the title leaves due/estimate alone.
        e.task_modify(&json!({ "ref": r#ref, "set": { "title": "y" } }))
            .unwrap();
        assert_eq!(get(&e)["due"], "2026-07-20T00:00:00Z");
        assert_eq!(get(&e)["estimate"], "PT4H");

        // Explicit null still clears.
        e.task_modify(&json!({ "ref": r#ref, "set": { "due": null, "estimate": null } }))
            .unwrap();
        assert!(get(&e)["due"].is_null(), "null must still clear due");
        assert!(
            get(&e)["estimate"].is_null(),
            "null must still clear estimate"
        );
    }

    /// `event.list {limit}` used to be `opt_u64(...) as i64`: anything at or
    /// above 2^63 wrapped negative, and SQLite reads a negative LIMIT as
    /// UNLIMITED — so the one parameter whose whole job is to bound a page
    /// silently returned the entire audit log at `ok: true`. `memory.search`
    /// already rejects the same input (engine/memory.rs), so the two surfaces
    /// must agree on what an out-of-range page size means.
    #[test]
    fn event_list_rejects_a_limit_past_i64_max_instead_of_unbounding_the_page() {
        let e = Engine::open_in_memory().unwrap();
        for _ in 0..6 {
            e.task_add(&json!({ "title": "x" })).unwrap();
        }
        // Sanity: the parameter does bound the page for an honest value.
        assert_eq!(e.event_list(&json!({ "limit": 2 })).unwrap()["count"], 2);

        for over in [9_223_372_036_854_775_808_u64, u64::MAX] {
            let err = e
                .event_list(&json!({ "limit": over }))
                .expect_err("a limit past i64::MAX must be a bad_request, not the whole log");
            assert_eq!(err.code, ErrorCode::BadRequest, "limit {over}");
            assert!(
                err.message.contains(&i64::MAX.to_string()),
                "limit {over}: {} must name the accepted maximum",
                err.message
            );
        }
    }

    /// The limit moved out of `format!` into a bound parameter, and the two
    /// arms of that `format!` no longer share one placeholder index — the
    /// scoped arm binds the scope as `?1` and the limit as `?2`, the unscoped
    /// arm binds the limit as `?1`. Get those indices wrong and the scope
    /// filter is handed the number while LIMIT is handed the entity name.
    /// Also pins the documented default of 50, which the checked conversion
    /// must not move.
    #[test]
    fn event_list_applies_the_limit_in_every_scoping_arm() {
        let e = Engine::open_in_memory().unwrap();
        let mut first = Value::Null;
        for i in 0..60 {
            let t = e.task_add(&json!({ "title": format!("t{i}") })).unwrap();
            if i == 0 {
                first = t["short_id"].clone();
            }
        }
        // Give the first task extra events so a `ref`-scoped page can overflow.
        for i in 0..4 {
            e.annotation_add(&json!({ "ref": first.clone(), "body": format!("n{i}") }))
                .unwrap();
        }

        assert_eq!(
            e.event_list(&json!({})).unwrap()["count"],
            50,
            "the default page size is 50 and the conversion must not move it"
        );
        let scoped = e
            .event_list(&json!({ "entity": "task", "limit": 3 }))
            .unwrap();
        assert_eq!(scoped["count"], 3, "entity-scoped page must honour limit");
        assert_eq!(
            scoped["events"][0]["entity"], "task",
            "entity-scoped page must still filter on entity"
        );
        let by_ref = e.event_list(&json!({ "ref": first, "limit": 2 })).unwrap();
        assert_eq!(by_ref["count"], 2, "ref-scoped page must honour limit");
    }

    /// `from` combines independently with both scopes, so the two placeholder
    /// arms this method used to have became four. Every combination must bind
    /// the right value to the right placeholder — the failure being guarded is
    /// the scope filter receiving the id floor, or LIMIT receiving an entity
    /// name, which SQLite accepts silently and answers wrongly.
    ///
    /// Doubles the coverage of the test above rather than replacing it: that one
    /// pins the limit in every arm, this one pins the same arms with a bound.
    #[test]
    fn event_list_from_bounds_the_page_in_every_scoping_arm() {
        let e = Engine::open_in_memory().unwrap();
        let t = e.task_add(&json!({ "title": "seeded" })).unwrap();
        let short = t["short_id"].clone();
        e.annotation_add(&json!({ "ref": short.clone(), "body": "n" }))
            .unwrap();

        // A bound comfortably in the past keeps everything; the assertions are
        // about which rows come back through which arm, not about the clock.
        let past = "-1d";
        let unscoped = e.event_list(&json!({ "from": past })).unwrap();
        assert!(
            unscoped["count"].as_u64().unwrap() >= 2,
            "an unscoped page bounded in the past must still return the seeded events"
        );

        let by_entity = e
            .event_list(&json!({ "from": past, "entity": "task" }))
            .unwrap();
        assert!(
            by_entity["count"].as_u64().unwrap() >= 2,
            "the entity-scoped arm must not lose rows to the bound"
        );
        for ev in by_entity["events"].as_array().unwrap() {
            assert_eq!(
                ev["entity"], "task",
                "the entity scope must still filter when `from` is present — \
                 a swapped placeholder shows up exactly here"
            );
        }

        let by_ref = e
            .event_list(&json!({ "from": past, "ref": short.clone() }))
            .unwrap();
        assert!(
            by_ref["count"].as_u64().unwrap() >= 2,
            "the ref-scoped arm must not lose rows to the bound"
        );

        // And the limit still lands on LIMIT rather than on a predicate.
        let capped = e
            .event_list(&json!({ "from": past, "entity": "task", "limit": 1 }))
            .unwrap();
        assert_eq!(
            capped["count"], 1,
            "`limit` must still reach LIMIT when a bound occupies an earlier placeholder"
        );

        // A bound in the future excludes everything — proof the predicate is
        // wired at all, rather than being silently dropped from the SQL.
        let future = e.event_list(&json!({ "from": "+1d" })).unwrap();
        assert_eq!(
            future["count"], 0,
            "a bound in the future must exclude every existing event"
        );
    }

    /// The boundary row must survive, which is what the margin in
    /// `storage::event_id_floor` buys: `insert_event` reads the clock twice, so
    /// a row's `id` can trail its own `ts` across a millisecond tick.
    #[test]
    fn event_list_from_does_not_lose_the_event_at_the_bound() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "boundary" })).unwrap();

        let all = e.event_list(&json!({})).unwrap();
        let ts = all["events"][0]["ts"].as_str().unwrap().to_string();
        let id = all["events"][0]["id"].as_str().unwrap().to_string();

        let bounded = e.event_list(&json!({ "from": ts })).unwrap();
        let ids: Vec<&str> = bounded["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|ev| ev["id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&id.as_str()),
            "the event whose own `ts` is the bound must come back, got {ids:?}"
        );
    }

    /// `from` takes the same date grammar every other caller-picked bound takes
    /// (D33). The failure this prevents is the one D33 records: five of the six
    /// formats tasqx prints in its own error message silently matching nothing.
    #[test]
    fn event_list_from_takes_the_relative_date_grammar_d33_unified() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "grammar" })).unwrap();

        for spelling in ["-1d", "yesterday", "-7d"] {
            let got = e.event_list(&json!({ "from": spelling })).unwrap();
            assert!(
                got["count"].as_u64().unwrap() >= 1,
                "`from: {spelling:?}` must reach the same grammar `due.before:` does"
            );
        }

        let err = e
            .event_list(&json!({ "from": "yesterdya" }))
            .expect_err("an unreadable date must be refused, not ignored");
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(
            err.message.contains("could not parse date"),
            "the refusal must be the shared date message, got {}",
            err.message
        );
    }

    /// D35: an empty string is a caller mistake, not "no bound given". A shell
    /// variable that expanded to nothing must not silently widen the page to
    /// the whole log.
    #[test]
    fn event_list_refuses_an_empty_from_instead_of_reading_it_as_absent() {
        let e = Engine::open_in_memory().unwrap();
        let err = e
            .event_list(&json!({ "from": "" }))
            .expect_err("an empty `from` must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
        // The SHARED D35 wording, not merely a message that happens to contain
        // the field name. Asserting the looser thing was not enough: swapping
        // `opt_when` for a hand-rolled `opt_str` + parse still produces an error
        // mentioning `from` (an unreadable-instant one), so the weaker
        // assertion stayed green through exactly the regression it exists to
        // catch. Found by running that swap as a bite-check.
        assert_eq!(
            err.message, "`from` was given as an empty string — send a value or omit `from`",
            "the refusal must be D35's shared wording"
        );
    }
}
