//! The engine: domain logic + the mutation/query methods behind every API
//! method (DESIGN.md §2, §4).
//!
//! One `Engine` owns one SQLite connection. Every mutating method opens an
//! immediate transaction via [`Engine::begin`] (so the public API can take
//! `&self` per DESIGN's `dispatch(&Engine, ...)` shape), performs its state change AND
//! writes the corresponding event row, then commits. If anything fails before
//! `commit`, the transaction drops and rolls back — leaving no state change and
//! no event. State and history therefore move together, always.

mod commands;
mod projects;
mod relationships;
mod reports;
mod task;
mod transfer;

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use jiff::Timestamp;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::datetime;
use crate::error::ApiError;
use crate::filter::{Filter, MatchCtx};
use crate::recur;
use crate::remind;
use crate::storage::{
    self, alloc_short_id, bump_short_id_floor, clear_config, ensure_tag_link, get_config,
    insert_event, map_task_row, set_config, task_tags, TASK_COLS,
};
use crate::types::{effective_status, Entity, Priority, Status, Task};
use crate::urgency;
use crate::util::{
    duration_secs, iso_duration, now, opt_array, opt_bool, opt_i64, opt_str, opt_str_array,
    opt_str_nonempty, opt_u64, parse_ts, req_array, req_i64, req_object, req_str, req_str_lookup,
    req_str_value, seconds_between,
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
pub const SUMMARY_METRICS: [&str; 4] = ["count", "est_total", "overdue", "tracked_total"];

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
pub const SORT_KEYS: [&str; 7] = [
    "urgency", "short_id", "priority", "due", "created", "modified", "title",
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
/// [`list_row_json`] call, so a field added to the projection joins this list
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
    };
    match list_row_json(&probe, &[], false) {
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

const SNAPSHOT_QUERY_COUNT: usize = 5;

struct TaskSnapshot {
    task: Task,
    tags: Vec<String>,
    blocked: bool,
    depends_on: Vec<String>,
    annotations: Vec<Value>,
}

impl Engine {
    /// Open (creating if needed) a file-backed store.
    pub fn open(path: &str) -> Result<Engine, ApiError> {
        Ok(Engine {
            conn: storage::open(path)?,
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
                "ref is neither short_id nor UUID: {s}"
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

    pub fn event_list(&self, p: &Value) -> Result<Value, ApiError> {
        let limit = opt_u64(p, "limit")?.unwrap_or(50) as i64;

        // Optional scoping: `ref` (a task) or `entity` (a type name).
        let (where_sql, arg): (String, Option<String>) = if let Some(r) = p.get("ref") {
            let task = self.resolve_ref_value(r)?;
            ("WHERE entity_id = ?1".to_string(), Some(task.id))
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
            (
                "WHERE entity = ?1".to_string(),
                Some(ent.as_str().to_string()),
            )
        } else {
            ("".to_string(), None)
        };

        // events.id is UUIDv7 (time-ordered), so ORDER BY id DESC = newest first.
        let sql = format!(
            "SELECT id, entity, entity_id, op, payload, ts, actor FROM events \
             {where_sql} ORDER BY id DESC LIMIT {limit}"
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
        match arg {
            Some(a) => {
                let rows = stmt.query_map(params![a], map)?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let rows = stmt.query_map([], map)?;
                for r in rows {
                    out.push(r?);
                }
            }
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

    /// Capabilities, including the current default project (gap fix A1).
    pub fn capabilities(&self) -> Result<Value, ApiError> {
        let mut v = crate::dispatch::capabilities();
        v["default_project"] = match self.default_project()? {
            Some(name) => Value::String(name),
            None => Value::Null,
        };
        Ok(v)
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
            format!("no project named {name} (create it with `tasqx init {name}`)"),
            Some(json!({ "name": name })),
        )),
        Some(a) if a != 0 => Err(ApiError::conflict(format!(
            "project is archived: {name} (archived projects cannot take new tasks)"
        ))),
        Some(_) => Ok(()),
    }
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
/// Three of these are read by nobody and belong here anyway: `urgency` is
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
    "urgency",
    "created",
    "modified",
    "completed",
    "_rev",
    "status_unrecognized",
];

/// Every key an exported annotation object can carry. D34.
pub const IMPORT_ANNOTATION_KEYS: &[&str] = &["id", "body", "created"];

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
        _ => Uuid::now_v7().to_string(),
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
        _ => return Err(ApiError::internal("non-scalar column update")),
    };
    Ok(())
}

/// Render a task as the canonical full JSON object used by `task.list`.
///
/// Not the export shape: `store_export` builds its own §3 object, so fields
/// added here for a reader's benefit cannot disturb the D12 round trip.
///
/// `tracked` is the STORED total and excludes an interval that is still
/// running, which is why `active_since` sits beside it: together they are the
/// whole truth and the running part stays derivable by anyone who wants it.
/// Folding the open interval in here would make `task.get` disagree with
/// `report.summary`'s `tracked_total` about the same task — trading a missing
/// number for two numbers that contradict each other.
pub fn task_to_json(t: &Task, tags: &[String]) -> Value {
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
            "tracked": iso_duration(t.tracked_seconds),
            "active_since": t.active_since,
            "recurrence": t.recurrence,
            "remind": t.remind,
            "urgency": t.urgency,
            "tags": tags,
            "created": t.created,
            "modified": t.modified,
            "completed": t.completed,
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
fn list_row_json(t: &Task, tags: &[String], blocked: bool) -> Value {
    let mut v = task_to_json(t, tags);
    v["blocked"] = json!(blocked);
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

fn compare_by(a: &Task, b: &Task, keys: &[SortKey]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for k in keys {
        let ord = match k.key.as_str() {
            "urgency" => a.urgency.partial_cmp(&b.urgency).unwrap_or(Ordering::Equal),
            "short_id" => a.short_id.cmp(&b.short_id),
            "priority" => priority_rank(a.priority).cmp(&priority_rank(b.priority)),
            "due" => opt_cmp(&a.due, &b.due),
            "created" => a.created.cmp(&b.created),
            "modified" => a.modified.cmp(&b.modified),
            "title" => a.title.cmp(&b.title),
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
    Ordering::Equal
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn lifecycle_commands_parse_once_and_preserve_wire_results() {
        let start = commands::parse_start_task(&json!({ "ref": "42", "keep": true })).unwrap();
        assert_eq!(start.target.value, json!("42"));
        assert!(start.keep);

        let started: Value = commands::TaskStarted {
            id: "task-id".to_string(),
            interval_started: Some("2026-07-20T12:00:00Z".to_string()),
        }
        .into();
        assert_eq!(
            started,
            json!({
                "id": "task-id",
                "status": "active",
                "interval_started": "2026-07-20T12:00:00Z",
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
            include_str!("engine/projects.rs"),
            include_str!("engine/relationships.rs"),
            include_str!("engine/reports.rs"),
            include_str!("engine/task.rs"),
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
            "store_import",
            "reminder_fire",
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
}
