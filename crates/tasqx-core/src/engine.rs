//! The engine: domain logic + the mutation/query methods behind every API
//! method (DESIGN.md §2, §4).
//!
//! One `Engine` owns one SQLite connection. Every mutating method opens an
//! immediate transaction via [`Engine::begin`] (so the public API can take
//! `&self` per DESIGN's `dispatch(&Engine, ...)` shape), performs its state change AND
//! writes the corresponding event row, then commits. If anything fails before
//! `commit`, the transaction drops and rolls back — leaving no state change and
//! no event. State and history therefore move together, always.

use std::collections::HashSet;
use std::sync::LazyLock;

use jiff::Timestamp;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::datetime;
use crate::error::ApiError;
use crate::filter::{Filter, MatchCtx};
use crate::storage::{
    self, alloc_short_id, bump_short_id_floor, clear_config, ensure_tag_link, get_config,
    insert_event, map_task_row, set_config, task_tags, TASK_COLS,
};
use crate::recur;
use crate::remind;
use crate::types::{effective_status, Entity, Priority, Status, Task};
use crate::urgency;
use crate::util::{
    duration_secs, iso_duration, now, opt_array, opt_bool, opt_i64, opt_str, opt_str_array,
    opt_str_nonempty,
    opt_u64, parse_ts, req_array, req_i64, req_object, req_str, req_str_lookup, req_str_value,
    seconds_between,
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
pub const SORT_KEYS: [&str; 7] =
    ["urgency", "short_id", "priority", "due", "created", "modified", "title"];

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
        id: String::new(), short_id: 0, title: String::new(), status: Status::Pending,
        status_raw: Some(String::new()), priority: None, project: None, due: None,
        scheduled: None, wait: None, estimate: None, recurrence: None, remind: None,
        urgency: 0.0, active_since: None, tracked_seconds: 0, rev: 0,
        created: String::new(), modified: String::new(), completed: None,
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

impl Engine {
    /// Open (creating if needed) a file-backed store.
    pub fn open(path: &str) -> Result<Engine, ApiError> {
        Ok(Engine { conn: storage::open(path)? })
    }

    /// Open an ephemeral in-memory store (tests).
    pub fn open_in_memory() -> Result<Engine, ApiError> {
        Ok(Engine { conn: storage::open_in_memory()? })
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
    fn begin(&self) -> Result<Transaction<'_>, ApiError> {
        Ok(Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?)
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
        let r = p
            .get("ref")
            .ok_or_else(|| ApiError::bad_request("missing required field: ref"))?;
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
            return Err(ApiError::bad_request(format!("ref is neither short_id nor UUID: {s}")));
        }
        Err(ApiError::bad_request("ref must be an integer or string"))
    }

    fn task_by_short_on(&self, conn: &Connection, short_id: i64) -> Result<Task, ApiError> {
        conn
            .query_row(
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
        conn
            .query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE id = ?1"),
                params![id],
                map_task_row,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ApiError::not_found(
                    format!("no task with id {id}"),
                    Some(json!({ "id": id })),
                ),
                other => other.into(),
            })
    }

    // ---- project.create ------------------------------------------------------

    pub fn project_create(&self, p: &Value) -> Result<Value, ApiError> {
        // D23's rule at the edge where a project name is *born* — `init " "`
        // used to mint a project that claimed the default, printed as a blank
        // row in `tasqx projects`, and (since `use` rejects the same string)
        // could never be re-selected once the default moved. D36 moved the check
        // itself into `req_str`, so this door no longer carries a private copy:
        // one rule means a title and a name cannot drift apart again.
        let name = req_str(p, "name")?;
        // D35: the last nullable free-text column with no parser in front of it.
        // `""` used to be laundered into NULL, so `init x --description "$UNSET"`
        // gave "no description" two spellings and threw the stated intent away —
        // D18's finding at the one edge D18 did not reach.
        let description = opt_str_nonempty(p, "description")?;

        let id = Uuid::now_v7().to_string();
        let ts = now();
        let tx = self.begin()?;
        // Duplicate check runs inside the IMMEDIATE tx: the write lock is already
        // held, so a racing project.create serializes behind us and its check
        // observes our committed row, yielding a clean `conflict` (not the
        // `internal` a bare UNIQUE-violation on the INSERT would produce).
        let exists: bool = tx
            .query_row("SELECT 1 FROM projects WHERE name = ?1", params![name], |_| Ok(()))
            .is_ok();
        if exists {
            return Err(ApiError::conflict(format!("project already exists: {name}")));
        }
        tx.execute(
            "INSERT INTO projects (id, name, description, archived, created) \
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![id, name, description, ts],
        )?;
        // D21: creating a project claims the default ONLY when the store has none
        // — the first project you ever create becomes the one a bare `task.add`
        // inherits, and nothing after that silently steals it. `project.use` is
        // the one explicit way to move it. Read inside the tx, which already
        // holds the write lock, so the check and the claim see one snapshot.
        let existing = get_config(&tx, DEFAULT_PROJECT_KEY);
        let claimed = existing.is_none();
        if claimed {
            set_config(&tx, DEFAULT_PROJECT_KEY, &name)?;
        }
        // D23: `default` is in the payload because this create may have moved the
        // default, and the log is where "where were bare adds landing?" is
        // answered. Its siblings already record it (`use` → `previous`,
        // `archive` → `default_cleared`); without it the log cannot say which
        // create claimed the key, and "the first create ever" is the wrong guess
        // for a store whose default was cleared by an archive and re-claimed
        // later (a sequence D22 blesses). Computed above so the row states what
        // this transaction actually did, and written inside it, as ever.
        insert_event(
            &tx,
            Entity::Project,
            &id,
            "create",
            &json!({ "name": name, "description": description, "default": claimed }),
        )?;
        tx.commit()?;

        // `default` is the truth of what happened, not a constant: the CLI paints
        // "now your default project" off this field, so it must be able to lie
        // no more than the store can. `current_default` says what the default IS
        // either way, so a caller who did not claim it still learns where a bare
        // `task.add` will go instead of having to ask a second method.
        let current_default = if claimed { Some(name.clone()) } else { existing };
        Ok(json!({
            "id": id,
            "name": name,
            "default": claimed,
            "current_default": current_default,
        }))
    }

    // ---- project.use ---------------------------------------------------------

    /// D21: point the default project at an existing, live project. This is the
    /// only method that moves the default once it is set.
    pub fn project_use(&self, p: &Value) -> Result<Value, ApiError> {
        // D23: emptiness is checked where names are born (`project.create`), not
        // here. `req_str_lookup` still rejects "" (`use "$UNSET"` → bad_request),
        // and a whitespace-only name simply names no project, so the lookup below
        // answers it truthfully with not_found. The previous special case made
        // `use` reject a name `init` would happily create — a one-way door of
        // the exact kind D21 exists to remove, at a narrower edge. D36 is why
        // this is `_lookup` and not `req_str`: a store written before D23 can
        // still HOLD such a project, and a write-door rule applied here would
        // make it unselectable forever (D28).
        let name = req_str_lookup(p, "name")?;

        let tx = self.begin()?;
        // Existence + archived state are read inside the IMMEDIATE tx: the write
        // lock is held, so a racing `project.archive` serializes against us and
        // we can never commit a default aimed at a project archived mid-flight.
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT id, archived FROM projects WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (id, archived) = row.ok_or_else(|| {
            ApiError::not_found(format!("no project named {name}"), Some(json!({ "name": name })))
        })?;
        // D22: archived means out of rotation. Pointing the default at one would
        // route every bare `add` into a project the default project list does not
        // even show — the invisible-state bug this whole change exists to kill.
        if archived != 0 {
            return Err(ApiError::conflict(format!(
                "project is archived: {name} (archived projects cannot be the default)"
            )));
        }

        let previous = get_config(&tx, DEFAULT_PROJECT_KEY);
        set_config(&tx, DEFAULT_PROJECT_KEY, &name)?;
        // THE invariant: the event row lands in the same transaction as the
        // mutation. The default is state, so moving it is history.
        insert_event(&tx, Entity::Project, &id, "use", &json!({ "name": name, "previous": previous }))?;
        tx.commit()?;

        Ok(json!({ "name": name, "default": true, "previous": previous }))
    }

    /// The current default project — the project a bare `task.add` inherits.
    /// Set by the first `project.create` and moved only by `project.use` (D21).
    pub fn default_project(&self) -> Option<String> {
        get_config(&self.conn, DEFAULT_PROJECT_KEY)
    }

    // ---- task.add ------------------------------------------------------------

    pub fn task_add(&self, p: &Value) -> Result<Value, ApiError> {
        let title = req_str(p, "title")?;
        // Gap fix A1: with no explicit project, inherit the default set by
        // `project.create` (init). An explicit project always wins.
        // D23: an *explicit* project is validated below, inside the transaction.
        // The inherited one needs no check — create/use/archive plus the D23
        // open-time repair keep the key aimed at a live project.
        // D35: `project: ""` used to read as "no project given" and INHERIT the
        // default — the caller who named a project got a different one. D18
        // refused the same string on `task.modify`; `add` never did.
        let explicit_project = opt_str_nonempty(p, "project")?;
        let project = explicit_project.clone().or_else(|| self.default_project());
        let priority = match opt_str_nonempty(p, "priority")? {
            Some(s) => Some(
                Priority::parse(&s)
                    .ok_or_else(|| ApiError::bad_request(format!("invalid priority: {s}")))?,
            ),
            None => None,
        };
        let now_ts = Timestamp::now();
        let due = opt_when(p, "due", now_ts)?;
        let scheduled = opt_when(p, "scheduled", now_ts)?;
        let wait = opt_when(p, "wait", now_ts)?;
        let estimate = match opt_str_nonempty(p, "estimate")? {
            Some(s) => Some(datetime::parse_duration(&s)?),
            None => None,
        };
        let tags = opt_str_array(p, "tags")?;
        // Recurrence rule (optional). Validate + normalize before storing so a
        // bad rule fails the add cleanly and the stored form is canonical.
        let recurrence = match opt_str_nonempty(p, "recurrence")? {
            Some(s) => Some(recur::rule_to_string(&recur::parse_rule(&s)?)),
            None => None,
        };
        // Reminder spec (§9). Validated + normalized here, exactly like
        // recurrence, so a bad spec fails the add cleanly and the stored form is
        // canonical. Accepts a `due`-anchored offset (`-1h`) or any NL date; the
        // absolute branch resolves against this add's `now` (see `crate::remind`).
        let remind = match opt_str_nonempty(p, "remind")? {
            Some(s) => Some(remind::spec_to_string(&remind::parse_remind(
                &s,
                Timestamp::now(),
            )?)),
            None => None,
        };

        // add -> pending, or backlog if wait/scheduled is in the future. Asking
        // the shared rule what a *backlog* task would be right now answers both
        // halves, and keeps this in step with the spawn path and every read.
        let status = effective_status(Status::Backlog, wait.as_deref(), scheduled.as_deref(), now_ts);

        let id = Uuid::now_v7().to_string();
        let ts = now();
        let urg = urgency::score(priority, due.as_deref(), &ts);

        let tx = self.begin()?;
        // D23: inside the IMMEDIATE tx, so a racing `project.archive` serializes
        // against us and we can never file a task into a project archived
        // mid-flight — the same reason `project.use` reads in its own tx.
        if let Some(name) = &explicit_project {
            require_live_project(&tx, name)?;
        }
        let short_id = alloc_short_id(&tx)?;
        tx.execute(
            &format!(
                "INSERT INTO tasks ({TASK_COLS}) VALUES \
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)"
            ),
            params![
                id,
                short_id,
                title,
                status.as_str(),
                priority.map(|x| x.as_str()),
                project,
                due,
                scheduled,
                wait,
                estimate,
                recurrence,
                urg,
                Option::<String>::None, // active_since
                0i64,                   // tracked_seconds
                1i64,                   // rev starts at 1 (this add is event #1)
                ts,
                ts,
                Option::<String>::None, // completed
                remind,
            ],
        )?;
        for tag in &tags {
            ensure_tag_link(&tx, &id, tag)?;
        }
        insert_event(
            &tx,
            Entity::Task,
            &id,
            "add",
            &json!({
                "title": title,
                "status": status.as_str(),
                "project": project,
                "priority": priority.map(|x| x.as_str()),
                "tags": tags,
                "recurrence": recurrence.clone(),
            }),
        )?;
        tx.commit()?;

        Ok(json!({
            "id": id,
            "short_id": short_id,
            "status": status.as_str(),
            // D21: the project this task actually landed in. When it was
            // inherited from the default rather than named on the command, this
            // is the ONLY place the caller learns where it went — "silently
            // lands in prive.klussen" was this field not existing.
            "project": project,
            "urgency": urg,
            "recurrence": recurrence,
        }))
    }

    // ---- task.start ----------------------------------------------------------

    pub fn task_start(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        let keep = opt_bool(p, "keep")?.unwrap_or(false);

        match task.status {
            Status::Active => {
                // Idempotent: already running.
                return Ok(json!({
                    "id": task.id,
                    "status": "active",
                    "interval_started": task.active_since,
                }));
            }
            Status::Pending => {}
            other => {
                return Err(ApiError::conflict(format!(
                    "cannot start a {} task (only pending -> active)",
                    other.as_str()
                )));
            }
        }

        let ts = now();
        let tx = self.begin()?;

        // D6: single active by default — auto-stop any currently active task.
        if !keep {
            let mut actives: Vec<(String, Option<String>, i64, i64)> = Vec::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT id, active_since, tracked_seconds, rev FROM tasks WHERE status = 'active'",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?;
                for row in rows {
                    actives.push(row?);
                }
            }
            for (aid, active_since, tracked, rev) in actives {
                let elapsed = seconds_between(&active_since, &ts);
                tx.execute(
                    "UPDATE tasks SET status='pending', active_since=NULL, \
                     tracked_seconds=?1, rev=?2, modified=?3 WHERE id=?4",
                    params![tracked + elapsed, rev + 1, ts, aid],
                )?;
                insert_event(
                    &tx,
                    Entity::Task,
                    &aid,
                    "stop",
                    &json!({ "reason": "auto_stop", "tracked": iso_duration(elapsed) }),
                )?;
            }
        }

        tx.execute(
            "UPDATE tasks SET status='active', active_since=?1, rev=?2, modified=?3 WHERE id=?4",
            params![ts, task.rev + 1, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "start", &json!({ "interval_started": ts }))?;
        tx.commit()?;

        Ok(json!({
            "id": task.id,
            "status": "active",
            "interval_started": ts,
        }))
    }

    // ---- task.stop -----------------------------------------------------------

    pub fn task_stop(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        if task.status != Status::Active {
            return Err(ApiError::conflict(format!(
                "cannot stop a {} task (only active -> pending)",
                task.status.as_str()
            )));
        }

        let ts = now();
        let elapsed = seconds_between(&task.active_since, &ts);
        let total = task.tracked_seconds + elapsed;

        let tx = self.begin()?;
        tx.execute(
            "UPDATE tasks SET status='pending', active_since=NULL, \
             tracked_seconds=?1, rev=?2, modified=?3 WHERE id=?4",
            params![total, task.rev + 1, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "stop", &json!({ "tracked": iso_duration(elapsed) }))?;
        tx.commit()?;

        Ok(json!({ "status": "pending", "tracked": iso_duration(elapsed) }))
    }

    // ---- task.done -----------------------------------------------------------

    pub fn task_done(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        match task.status {
            Status::Pending | Status::Active => {}
            other => {
                return Err(ApiError::conflict(format!(
                    "cannot complete a {} task (only pending|active -> done)",
                    other.as_str()
                )));
            }
        }

        let ts = now();
        // If it was active, close the open interval into tracked time.
        let elapsed = if task.status == Status::Active {
            seconds_between(&task.active_since, &ts)
        } else {
            0
        };
        let total = task.tracked_seconds + elapsed;

        // A recurring template spawns its next instance on completion (D2). Read
        // the template's tags before opening the write tx (they don't change).
        let template_tags = task_tags(&self.conn, &task.id)?;

        let tx = self.begin()?;
        tx.execute(
            "UPDATE tasks SET status='done', completed=?1, active_since=NULL, \
             tracked_seconds=?2, rev=?3, modified=?4 WHERE id=?5",
            params![ts, total, task.rev + 1, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "done", &json!({ "completed": ts }))?;

        // Spawn the next recurring instance in the SAME transaction: if this
        // fails, the whole completion rolls back — no orphan spawn, no event.
        let spawned = self.spawn_next(&tx, &task, &template_tags, &ts)?;

        // Which dependents just became fully unblocked?
        let unblocked = Self::compute_unblocked(&tx, &task.id)?;
        tx.commit()?;

        let mut out = json!({
            "status": "done",
            "completed": ts,
            "unblocked": unblocked,
        });
        if let Some(sp) = spawned {
            out["spawned"] = sp;
        }
        Ok(out)
    }

    /// If `template` carries a recurrence rule, create the next instance inside
    /// `tx` with its dates advanced per the rule (missed slots collapse to one,
    /// D2). Returns the spawned instance's summary, or `None` when there is no
    /// rule. `ts` is the completion instant (RFC3339); it is the reference `now`
    /// the collapse logic advances past.
    fn spawn_next(
        &self,
        tx: &Transaction,
        template: &Task,
        template_tags: &[String],
        ts: &str,
    ) -> Result<Option<Value>, ApiError> {
        let Some(rule_str) = template.recurrence.as_deref() else {
            return Ok(None);
        };
        let rule = recur::parse_rule(rule_str)?;
        let now_ts =
            parse_ts(ts).ok_or_else(|| ApiError::internal("completion timestamp unparseable"))?;

        // Anchor on the current due, else the scheduled, else `now`.
        let anchor = template
            .due
            .as_deref()
            .or(template.scheduled.as_deref())
            .and_then(parse_ts)
            .unwrap_or(now_ts);
        let next = recur::next_after(&rule, anchor, now_ts)?;
        let delta = next.as_second() - anchor.as_second();

        // Shift every present date field by the same delta so their relative
        // offsets are preserved; the anchor field lands exactly on `next`.
        let mut new_due = shift_ts(&template.due, delta);
        let new_scheduled = shift_ts(&template.scheduled, delta);
        let new_wait = shift_ts(&template.wait, delta);
        if new_due.is_none() && new_scheduled.is_none() {
            new_due = Some(next.to_string());
        }

        // Same rule as `task_add`, on the shifted timestamps, against this
        // completion's instant rather than a second reading of the clock.
        let status =
            effective_status(Status::Backlog, new_wait.as_deref(), new_scheduled.as_deref(), now_ts);
        let urg = urgency::score(template.priority, new_due.as_deref(), ts);

        // Carry the reminder onto the new instance (§9). A `due`-anchored offset
        // is symbolic, so it rides along unchanged and re-anchors on the new due
        // for free. An *absolute* remind is a date field like scheduled/wait, so
        // it shifts by the same delta — carrying it verbatim would hand the fresh
        // instance an already-past instant that fires the moment it spawns.
        let new_remind = match template.remind.as_deref().and_then(remind::parse_spec) {
            Some(remind::Remind::At(_)) => shift_ts(&template.remind, delta),
            _ => template.remind.clone(),
        };

        let new_id = Uuid::now_v7().to_string();
        let new_short = alloc_short_id(tx)?;
        tx.execute(
            &format!(
                "INSERT INTO tasks ({TASK_COLS}) VALUES \
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)"
            ),
            params![
                new_id,
                new_short,
                template.title,
                status.as_str(),
                template.priority.map(|x| x.as_str()),
                template.project,
                new_due,
                new_scheduled,
                new_wait,
                template.estimate,
                template.recurrence, // carry the rule forward
                urg,
                Option::<String>::None, // active_since
                0i64,                   // tracked_seconds
                1i64,                   // rev
                ts,
                ts,
                Option::<String>::None, // completed
                new_remind,
            ],
        )?;
        for tag in template_tags {
            ensure_tag_link(tx, &new_id, tag)?;
        }
        insert_event(
            tx,
            Entity::Task,
            &new_id,
            "add",
            &json!({
                "title": template.title,
                "status": status.as_str(),
                "recurrence": template.recurrence,
                "spawned_from": template.id,
            }),
        )?;

        Ok(Some(json!({
            "short_id": new_short,
            "id": new_id,
            "status": status.as_str(),
            "due": new_due,
            "scheduled": new_scheduled,
        })))
    }

    /// Return short_ids of tasks that depend on `done_id` and now have *all*
    /// their dependencies completed (i.e. this completion cleared their last
    /// blocker). With no dependencies in the MVP store this is trivially empty.
    fn compute_unblocked(
        tx: &rusqlite::Transaction,
        done_id: &str,
    ) -> Result<Vec<i64>, ApiError> {
        // Only *open* dependents can "become actionable" — a dependent that is
        // itself already done/cancelled must never be reported as unblocked.
        let dependents: Vec<String> = {
            // Enum-derived, never caller text — see `Status::sql_in_list`.
            let open = Status::sql_in_list(Status::is_open);
            let mut stmt = tx.prepare(&format!(
                "SELECT d.task_id FROM dependencies d \
                 JOIN tasks t ON t.id = d.task_id \
                 WHERE d.depends_on_id = ?1 \
                 AND t.status IN ({open})",
            ))?;
            let rows = stmt.query_map(params![done_id], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };

        let mut out = Vec::new();
        for dep_task in dependents {
            // Count this dependent's still-unresolved blockers. A dependency is
            // resolved when it is `done` OR `cancelled` (DESIGN §3, D11), so a
            // cancelled blocker no longer keeps the dependent blocked.
            // Enum-derived, never caller text — see `Status::sql_in_list`.
            let terminal = Status::sql_in_list(Status::is_terminal);
            let remaining: i64 = tx.query_row(
                &format!(
                    "SELECT COUNT(*) FROM dependencies d \
                     JOIN tasks t ON t.id = d.depends_on_id \
                     WHERE d.task_id = ?1 AND t.status NOT IN ({terminal})"
                ),
                params![dep_task],
                |r| r.get(0),
            )?;
            if remaining == 0 {
                if let Ok(sid) = tx.query_row(
                    "SELECT short_id FROM tasks WHERE id = ?1",
                    params![dep_task],
                    |r| r.get::<_, i64>(0),
                ) {
                    out.push(sid);
                }
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    // ---- task.modify ---------------------------------------------------------

    pub fn task_modify(&self, p: &Value) -> Result<Value, ApiError> {
        let set = req_object(p, "set")
            .map_err(|e| ApiError::bad_request(format!("{} (modify requires a `set` object)", e.message)))?;
        if set.is_empty() {
            return Err(ApiError::bad_request("`set` must contain at least one field"));
        }
        let expected_rev = opt_i64(p, "expected_rev")?;

        // Store-dependent validation starts only after BEGIN IMMEDIATE. Another
        // process may have changed the row after request parsing but before this
        // lock; the transaction's row is the only authoritative one.
        let tx = self.begin()?;
        let task = self.resolve_ref_on(&tx, p)?;

        // Optional optimistic concurrency.
        // D32: read through the typed layer. As `.and_then(Value::as_i64)` this
        // guard FAILED OPEN — `expected_rev: "1"` was indistinguishable from no
        // guard at all, so the stale write landed and the caller was told `ok`.
        if let Some(exp) = expected_rev {
            if exp != task.rev {
                return Err(ApiError::conflict(format!(
                    "expected_rev {} but task is at rev {}",
                    exp, task.rev
                )));
            }
        }

        // Whitelist of modifiable columns; recompute urgency if inputs change.
        let mut priority = task.priority;
        let mut due = task.due.clone();
        let mut assignments: Vec<(&str, Value)> = Vec::new();
        // Set when the only sanctioned lifecycle edit — cancellation — is requested.
        let mut cancelling = false;
        // D23: the project this modify moves the task into, if any (None for an
        // unchanged or cleared project). Validated inside the write tx below.
        let mut project_target: Option<String> = None;

        for (k, v) in set {
            match k.as_str() {
                "title" => {
                    // D36: the SAME rule `task.add` and `store.import` apply.
                    // This arm used to accept any string, so `set:{title:""}`
                    // wrote a store that exported fine and then failed its own
                    // import — D12's round trip breakable from the API.
                    // D13: `title` is deliberately NOT clearable — "a task with
                    // no title is not a task" — which the CLI enforces by
                    // leaving `title` out of `CLEARABLE`, so `--clear title`
                    // dies at parse time. `set:{title:null}` is this API's
                    // spelling of that same request, and it used to fall
                    // through to the generic wrong-type message ("send a string
                    // or omit `title`"), which describes a type mistake rather
                    // than the rule actually being enforced. A caller trying to
                    // erase a title was told nothing about erasing being the
                    // refused part. One request, one answer, on both surfaces.
                    if v.is_null() {
                        return Err(ApiError::bad_request(
                            "title cannot be cleared — a task with no title is not a task; send a new title, or cancel the task if it is not real work",
                        ));
                    }
                    let s = req_str_value("title", Some(v))?;
                    assignments.push(("title", Value::String(s)));
                }
                "priority" => {
                    if v.is_null() {
                        priority = None;
                        assignments.push(("priority", Value::Null));
                    } else {
                        let s = v.as_str().ok_or_else(|| ApiError::bad_request("priority must be a string"))?;
                        let pr = Priority::parse(s)
                            .ok_or_else(|| ApiError::bad_request(format!("invalid priority: {s}")))?;
                        priority = Some(pr);
                        assignments.push(("priority", Value::String(pr.as_str().to_string())));
                    }
                }
                "project" => {
                    // `project` is the only nullable field with no parser in
                    // front of it, so an empty string used to sail through and
                    // become a nameless bucket distinct from NULL. Reject it at
                    // the edge, exactly as due/scheduled/wait/estimate do, and
                    // keep `--clear project` the one way to empty the field.
                    if v.as_str().is_some_and(|s| s.trim().is_empty()) {
                        return Err(ApiError::bad_request(
                            "project cannot be empty — use `--clear project` to remove it",
                        ));
                    }
                    // D23: a move must land somewhere the project list shows,
                    // exactly as an add must. Validated below inside the write
                    // transaction, not here, so a racing `project.archive`
                    // serializes against it (`null` clears and needs no check).
                    project_target = v.as_str().map(str::to_string);
                    assignments.push(("project", nullable_string(v, "project")?));
                }
                "due" => {
                    let norm = nullable_when(v, "due", Timestamp::now())?;
                    due = norm.as_str().map(str::to_string);
                    assignments.push(("due", norm));
                }
                "scheduled" => {
                    assignments.push(("scheduled", nullable_when(v, "scheduled", Timestamp::now())?))
                }
                "wait" => assignments.push(("wait", nullable_when(v, "wait", Timestamp::now())?)),
                "estimate" => assignments.push(("estimate", nullable_duration(v, "estimate")?)),
                "recurrence" => {
                    // Set a rule (validated + normalized) or clear it with null
                    // — the sanctioned "stop recurring" path (DESIGN §10, D2).
                    if v.is_null() {
                        assignments.push(("recurrence", Value::Null));
                    } else {
                        let s = v
                            .as_str()
                            .ok_or_else(|| ApiError::bad_request("recurrence must be a string or null"))?;
                        let norm = recur::rule_to_string(&recur::parse_rule(s)?);
                        assignments.push(("recurrence", Value::String(norm)));
                    }
                }
                "remind" => {
                    // Set a reminder (validated + normalized, same parser as
                    // task.add) or clear it with null — the "stop reminding me"
                    // path (§9). A relative offset re-anchors automatically when
                    // `due` changes, so it is stored symbolically, not resolved.
                    if v.is_null() {
                        assignments.push(("remind", Value::Null));
                    } else {
                        let s = v.as_str().ok_or_else(|| {
                            ApiError::bad_request("remind must be a string or null")
                        })?;
                        let norm =
                            remind::spec_to_string(&remind::parse_remind(s, Timestamp::now())?);
                        assignments.push(("remind", Value::String(norm)));
                    }
                }
                "status" => {
                    // `status` in a modify is NOT a general lifecycle backdoor.
                    // The only transition it may drive is cancellation (DESIGN
                    // §7: "Cancellation goes through task.modify status:cancelled").
                    // Every other target (active/done/pending/backlog) must go
                    // through task.start/stop/done so their invariants
                    // (single-active D6, completed timestamp, interval closing)
                    // are enforced — otherwise this would produce
                    // invariant-violating rows.
                    let s = v.as_str().ok_or_else(|| ApiError::bad_request("status must be a string"))?;
                    let st = Status::parse(s)
                        .ok_or_else(|| ApiError::bad_request(format!("invalid status: {s}")))?;
                    if st != Status::Cancelled {
                        return Err(ApiError::bad_request(format!(
                            "status can only be set to 'cancelled' via modify; \
                             use task.start/stop/done for other transitions (got {s})"
                        )));
                    }
                    // Cancel is only valid from a non-terminal state.
                    match task.status {
                        Status::Backlog | Status::Pending | Status::Active => {}
                        other => {
                            return Err(ApiError::conflict(format!(
                                "cannot cancel a {} task",
                                other.as_str()
                            )));
                        }
                    }
                    cancelling = true;
                    assignments.push(("status", Value::String(st.as_str().to_string())));
                }
                other => {
                    return Err(ApiError::bad_request(format!("field not modifiable: {other}")));
                }
            }
        }

        let ts = now();
        let new_urg = urgency::score(priority, due.as_deref(), &task.created);
        let new_rev = task.rev + 1;

        if let Some(name) = &project_target {
            require_live_project(&tx, name)?;
        }
        for (col, val) in &assignments {
            update_column(&tx, &task.id, col, val)?;
        }
        // Cancelling a running task closes its open interval into tracked time
        // and clears active_since, exactly as task.stop/task.done would.
        if cancelling && task.status == Status::Active {
            let elapsed = seconds_between(&task.active_since, &ts);
            tx.execute(
                "UPDATE tasks SET active_since=NULL, tracked_seconds=?1 WHERE id=?2",
                params![task.tracked_seconds + elapsed, task.id],
            )?;
        }
        tx.execute(
            "UPDATE tasks SET urgency=?1, rev=?2, modified=?3 WHERE id=?4",
            params![new_urg, new_rev, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "modify", &Value::Object(set.clone()))?;
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "_rev": new_rev }))
    }

    // ---- tag.add -------------------------------------------------------------

    pub fn tag_add(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        let tags = opt_str_array(p, "tags")?;
        if tags.is_empty() {
            return Err(ApiError::bad_request("tag.add requires a non-empty `tags` array"));
        }

        let ts = now();
        let tx = self.begin()?;
        for tag in &tags {
            ensure_tag_link(&tx, &task.id, tag)?;
        }
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "tag.add", &json!({ "tags": tags }))?;

        // Re-read the full tag set inside the transaction for the response.
        let all = {
            let mut stmt = tx.prepare(
                "SELECT t.name FROM tags t JOIN task_tags tt ON tt.tag_id = t.id \
                 WHERE tt.task_id = ?1 ORDER BY t.name",
            )?;
            let rows = stmt.query_map(params![task.id], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "tags": all }))
    }

    // ---- task.list -----------------------------------------------------------

    pub fn task_list(&self, p: &Value) -> Result<Value, ApiError> {
        // D35's one recorded exception, and it is a decision, not an oversight:
        // D27 ruled the empty filter matches everything — no filter means no
        // filtering — so `""` here is a genuine empty value rather than an
        // absent one. The CLI sends exactly this on every unfiltered read, so a
        // blanket refusal would break the tool. Same at `report.*` and
        // `store.export`, which is why all four spell it identically.
        let filter_str = opt_str(p, "filter")?.unwrap_or_default();
        let filter = Filter::parse(&filter_str, Timestamp::now()).map_err(ApiError::bad_request)?;

        // Fetch all rows, then evaluate the filter in Rust: the §12-D8 grammar
        // (or/parens) and instant `due` comparison are evaluated on the loaded
        // task + its tags + its blocked flag (see filter.rs).
        let mut all: Vec<Task> = {
            let mut stmt = self.conn.prepare(&format!("SELECT {TASK_COLS} FROM tasks"))?;
            let rows = stmt.query_map([], map_task_row)?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };

        // Urgency has time-dependent terms (due proximity, age), so the value
        // persisted at write time goes stale. Recompute it for the fetched page
        // before sorting/rendering so "urgency-hot first" stays honest.
        // Carry each surviving task's tags (already fetched for the filter) so
        // the projection loop below reuses them instead of re-querying.
        // `blocked` is carried alongside the tags for the same reason: it is
        // already computed here for the filter, and throwing it away meant
        // `@blocked` could FILTER on a fact that `fields:["blocked"]` could not
        // RETURN. A caller wanting it per row had to issue one `task.get` each.
        let mut tasks: Vec<(Task, Vec<String>, bool)> = Vec::new();
        for mut t in all.drain(..) {
            t.urgency = urgency::score(t.priority, t.due.as_deref(), &t.created);
            let tags = task_tags(&self.conn, &t.id)?;
            let blocked = self.is_blocked(&t.id)?;
            let ctx = MatchCtx {
                status: t.status,
                project: t.project.as_deref(),
                tags: &tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked,
            };
            if filter.matches(&ctx) {
                tasks.push((t, tags, blocked));
            }
        }

        // Sort (default: hottest urgency first). Validated, so an unknown key
        // fails here rather than quietly producing some other order.
        let sort_keys = parse_sort(p)?;
        tasks.sort_by(|a, b| compare_by(&a.0, &b.0, &sort_keys));

        // Limit.
        if let Some(limit) = opt_u64(p, "limit")? {
            tasks.truncate(limit as usize);
        }

        // Field projection (whole row when `fields` absent). Validated, so an
        // unknown key fails here rather than quietly yielding a narrower row.
        let fields = parse_fields(p)?;

        let mut out = Vec::with_capacity(tasks.len());
        for (t, tags, blocked) in &tasks {
            let full = list_row_json(t, tags, *blocked);
            match &fields {
                Some(keys) => {
                    let mut obj = Map::new();
                    for k in keys {
                        if let Some(v) = full.get(k) {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                    out.push(Value::Object(obj));
                }
                None => out.push(full),
            }
        }

        Ok(json!({ "count": out.len(), "tasks": out }))
    }

    // ---- blocked / dependency helpers ---------------------------------------

    /// A task is *blocked* if it has any dependency that is not yet *resolved*.
    /// A dependency is resolved when it is `done` OR `cancelled` (DESIGN §3, D11):
    /// a cancelled blocker will never complete, so keeping the dependent blocked
    /// forever is a trap — cancellation releases dependents. Consistent with
    /// `compute_unblocked`. Read helper; no mutation.
    fn is_blocked(&self, task_id: &str) -> Result<bool, ApiError> {
        // Enum-derived, never caller text — see `Status::sql_in_list`.
        let terminal = Status::sql_in_list(Status::is_terminal);
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM dependencies d \
                 JOIN tasks t ON t.id = d.depends_on_id \
                 WHERE d.task_id = ?1 AND t.status NOT IN ({terminal})"
            ),
            params![task_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// short_ids of a task's dependencies, sorted (for get/dependency output).
    fn depends_on_short_ids(&self, task_id: &str) -> Result<Vec<i64>, ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.short_id FROM dependencies d \
             JOIN tasks t ON t.id = d.depends_on_id \
             WHERE d.task_id = ?1 ORDER BY t.short_id",
        )?;
        let rows = stmt.query_map(params![task_id], |r| r.get::<_, i64>(0))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }

    /// UUIDs of a task's dependencies, sorted (for the canonical export shape).
    ///
    /// Joins `tasks` for the same reason `is_blocked` and `depends_on_short_ids`
    /// do: an edge to a row that isn't there is not a dependency anyone can see
    /// or remove, so the export must not see it either. Without the join this
    /// reader disagreed with every other one and re-emitted edges the user could
    /// not observe. The FOREIGN KEY (§2 schema) makes that state unreachable
    /// now; the join keeps the two readers honest regardless.
    fn depends_on_ids(&self, task_id: &str) -> Result<Vec<String>, ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id FROM dependencies d \
             JOIN tasks t ON t.id = d.depends_on_id \
             WHERE d.task_id = ?1 ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![task_id], |r| r.get::<_, String>(0))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }

    /// Annotations of a task as `[{id, body, created}]`, oldest first.
    fn annotations_of(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, body, created FROM annotations WHERE task_id = ?1 ORDER BY created, id",
        )?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "body": r.get::<_, String>(1)?,
                "created": r.get::<_, String>(2)?,
            }))
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }

    // ---- task.get ------------------------------------------------------------

    pub fn task_get(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        let tags = task_tags(&self.conn, &task.id)?;
        let mut obj = task_to_json(&task, &tags);
        // Recompute urgency for a live read (list does the same).
        obj["urgency"] = json!(urgency::score(task.priority, task.due.as_deref(), &task.created));
        obj["depends_on"] = json!(self.depends_on_short_ids(&task.id)?);
        obj["annotations"] = json!(self.annotations_of(&task.id)?);
        obj["blocked"] = json!(self.is_blocked(&task.id)?);
        Ok(obj)
    }

    // ---- task.cancel ---------------------------------------------------------

    pub fn task_cancel(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        match task.status {
            Status::Backlog | Status::Pending | Status::Active => {}
            other => {
                return Err(ApiError::conflict(format!(
                    "cannot cancel a {} task (only backlog|pending|active -> cancelled)",
                    other.as_str()
                )));
            }
        }

        let ts = now();
        // Closing an open interval into tracked time, exactly as stop/done do.
        let elapsed = if task.status == Status::Active {
            seconds_between(&task.active_since, &ts)
        } else {
            0
        };
        let total = task.tracked_seconds + elapsed;

        let tx = self.begin()?;
        tx.execute(
            "UPDATE tasks SET status='cancelled', active_since=NULL, \
             tracked_seconds=?1, rev=?2, modified=?3 WHERE id=?4",
            params![total, task.rev + 1, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "cancel", &json!({ "from": task.status.as_str() }))?;
        // Cancelling a blocker resolves it (D11), so dependents may become
        // actionable — surface the same unblock cascade task.done reports.
        let unblocked = Self::compute_unblocked(&tx, &task.id)?;
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "status": "cancelled", "unblocked": unblocked }))
    }

    // ---- task.reopen ---------------------------------------------------------

    pub fn task_reopen(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        match task.status {
            Status::Done | Status::Cancelled => {}
            other => {
                return Err(ApiError::conflict(format!(
                    "cannot reopen a {} task (only done|cancelled -> pending)",
                    other.as_str()
                )));
            }
        }

        let ts = now();
        let tx = self.begin()?;
        tx.execute(
            "UPDATE tasks SET status='pending', completed=NULL, rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "reopen", &json!({ "from": task.status.as_str() }))?;
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "status": "pending" }))
    }

    // ---- project.list --------------------------------------------------------

    pub fn project_list(&self, p: &Value) -> Result<Value, ApiError> {
        let include_archived = opt_bool(p, "include_archived")?.unwrap_or(false);
        let sql = if include_archived {
            "SELECT id, name, description, archived FROM projects ORDER BY name"
        } else {
            "SELECT id, name, description, archived FROM projects WHERE archived = 0 ORDER BY name"
        };
        // D21: the default drives where a bare `add` lands, so the surface that
        // lists projects must say which one it is. Read once, outside the row
        // loop — this is the same fact `core.capabilities.default_project`
        // reports, from the same key, so the two can never disagree.
        let default = self.default_project();
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            let name = r.get::<_, String>(1)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "default": default.as_deref() == Some(name.as_str()),
                "name": name,
                "description": r.get::<_, Option<String>>(2)?,
                "archived": r.get::<_, i64>(3)? != 0,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(json!({ "count": out.len(), "projects": out }))
    }

    // ---- project.archive -----------------------------------------------------

    pub fn project_archive(&self, p: &Value) -> Result<Value, ApiError> {
        // A lookup, like `project.use`: retiring a legacy whitespace-named
        // project is precisely the escape hatch D36 must not weld shut (D28).
        let name = req_str_lookup(p, "name")?;
        let id: String = self
            .conn
            .query_row("SELECT id FROM projects WHERE name = ?1", params![name], |r| r.get(0))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ApiError::not_found(format!("no project named {name}"), Some(json!({ "name": name })))
                }
                other => other.into(),
            })?;

        let tx = self.begin()?;
        tx.execute("UPDATE projects SET archived = 1 WHERE id = ?1", params![id])?;
        // D22: archiving the *current* default un-points it, in this same
        // transaction. The alternative — leaving the default aimed at a retired
        // project — routes every bare `add` into a project `tasqx projects` no
        // longer lists, which is exactly the invisible state this change kills.
        // Clearing returns the store to the state a fresh one is in (no default,
        // bare `add` is projectless), and `use` is the way back.
        let default_cleared = get_config(&tx, DEFAULT_PROJECT_KEY).as_deref() == Some(name.as_str())
            && clear_config(&tx, DEFAULT_PROJECT_KEY)?;
        insert_event(
            &tx,
            Entity::Project,
            &id,
            "archive",
            &json!({ "name": name, "default_cleared": default_cleared }),
        )?;
        tx.commit()?;

        // Always present, never omitted: a machine consumer must be able to tell
        // "did not clear" from "this build does not report it".
        Ok(json!({ "name": name, "archived": true, "default_cleared": default_cleared }))
    }

    // ---- annotation.add ------------------------------------------------------

    pub fn annotation_add(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        let body = req_str(p, "body")?;

        let id = Uuid::now_v7().to_string();
        let ts = now();
        let tx = self.begin()?;
        tx.execute(
            "INSERT INTO annotations (id, task_id, body, created) VALUES (?1, ?2, ?3, ?4)",
            params![id, task.id, body, ts],
        )?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(&tx, Entity::Task, &task.id, "annotation.add", &json!({ "id": id, "body": body }))?;
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "annotation": { "id": id, "body": body, "created": ts },
        }))
    }

    // ---- dependency.add ------------------------------------------------------

    pub fn dependency_add(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        let dep = p
            .get("depends_on")
            .ok_or_else(|| ApiError::bad_request("missing required field: depends_on"))?;
        let target = self.resolve_ref_value(dep)?;

        if task.id == target.id {
            return Err(ApiError::conflict("a task cannot depend on itself"));
        }

        let ts = now();
        let tx = self.begin()?;
        // Cycle check runs inside the IMMEDIATE tx (write lock held) so the
        // acyclicity read and the INSERT observe one consistent snapshot: a
        // concurrent writer can't slip an edge in between the check and the
        // insert. Adding task -> target cycles iff `target` already
        // (transitively) depends on `task`.
        if reaches(&tx, &target.id, &task.id)? {
            return Err(ApiError::conflict(format!(
                "dependency would create a cycle: #{} already depends on #{}",
                target.short_id, task.short_id
            )));
        }
        tx.execute(
            "INSERT OR IGNORE INTO dependencies (task_id, depends_on_id) VALUES (?1, ?2)",
            params![task.id, target.id],
        )?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "dependency.add",
            &json!({ "depends_on": target.id }),
        )?;
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "depends_on": self.depends_on_short_ids(&task.id)?,
            "blocked": self.is_blocked(&task.id)?,
        }))
    }

    // ---- dependency.remove ---------------------------------------------------

    pub fn dependency_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        let dep = p
            .get("depends_on")
            .ok_or_else(|| ApiError::bad_request("missing required field: depends_on"))?;
        let target = self.resolve_ref_value(dep)?;

        let ts = now();
        let tx = self.begin()?;
        let removed = tx.execute(
            "DELETE FROM dependencies WHERE task_id = ?1 AND depends_on_id = ?2",
            params![task.id, target.id],
        )?;
        if removed > 0 {
            tx.execute(
                "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
                params![task.rev + 1, ts, task.id],
            )?;
            insert_event(
                &tx,
                Entity::Task,
                &task.id,
                "dependency.remove",
                &json!({ "depends_on": target.id }),
            )?;
        }
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "depends_on": self.depends_on_short_ids(&task.id)?,
            "blocked": self.is_blocked(&task.id)?,
        }))
    }

    // ---- report.summary ------------------------------------------------------

    pub fn report_summary(&self, p: &Value) -> Result<Value, ApiError> {
        // D35: `unwrap_or_else` fires only on a genuinely ABSENT value now, so
        // `group_by: ""` reaches the vocabulary check below instead of silently
        // becoming the default axis — the closed-set rule of D34, which
        // `group_by: "bogus"` already got and `""` did not.
        let group_by = opt_str(p, "group_by")?.unwrap_or_else(|| SUMMARY_GROUP_BY[0].to_string());
        if !SUMMARY_GROUP_BY.contains(&group_by.as_str()) {
            return Err(ApiError::bad_request(format!(
                "group_by must be {} (got {group_by:?})",
                SUMMARY_GROUP_BY.join("|")
            )));
        }
        // Validated against the same constant the MCP schema renders its `enum`
        // from. It used to `filter_map` unknown names away and answer `ok`, so
        // `metrics:["overdeu"]` produced a table with the column missing — the
        // `fields` and sort-key drop again, on the surface that had already
        // published its valid set and simply did not enforce it.
        let metrics: Vec<String> = match p.get("metrics") {
            None => vec![SUMMARY_METRICS[0].to_string()],
            Some(Value::Array(a)) => {
                let mut v = Vec::with_capacity(a.len());
                for m in a {
                    let name = m.as_str().filter(|s| SUMMARY_METRICS.contains(s));
                    let Some(name) = name else {
                        return Err(ApiError::bad_request(format!(
                            "unknown metric {m} (valid metrics: {})",
                            SUMMARY_METRICS.join(", ")
                        )));
                    };
                    v.push(name.to_string());
                }
                v
            }
            Some(_) => {
                return Err(ApiError::bad_request("`metrics` must be an array of metric names"))
            }
        };

        let filter = Filter::parse(&opt_str(p, "filter")?.unwrap_or_default(), Timestamp::now())
            .map_err(ApiError::bad_request)?;
        let now_ts = parse_ts(&now());

        // D24: a report is an *aggregation*, so abandoned work must not inflate
        // any total. tasqx has no hard delete (DESIGN §725) — cancelling is how
        // you get rid of a task — so without this every throwaway task counted
        // forever. `done` deliberately still counts: completed work is real work
        // and carries nearly all the tracked time.
        //
        // Resolution order (D24): `all` wins; otherwise a caller who already
        // named a status is taken literally, so `status:cancelled` returns
        // cancelled tasks rather than a baffling empty table; otherwise the
        // default applies. The rule lives here, in core, so the CLI, the HTML
        // report and MCP agents all inherit one answer.
        let all = opt_bool(p, "all")?.unwrap_or(false);
        let apply_default = !all && !filter.constrains_status();

        // Accumulator per group key (insertion via BTreeMap => sorted output).
        use std::collections::BTreeMap;
        struct Agg {
            count: i64,
            est_secs: i64,
            tracked_secs: i64,
            overdue: i64,
        }
        let mut groups: BTreeMap<String, Agg> = BTreeMap::new();

        let mut stmt = self.conn.prepare(&format!("SELECT {TASK_COLS} FROM tasks"))?;
        let rows = stmt.query_map([], map_task_row)?;
        for r in rows {
            let t = r?;
            if apply_default && !t.status.counts_in_reports() {
                continue;
            }
            let tags = task_tags(&self.conn, &t.id)?;
            let blocked = self.is_blocked(&t.id)?;
            let ctx = MatchCtx {
                status: t.status,
                project: t.project.as_deref(),
                tags: &tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked,
            };
            if !filter.matches(&ctx) {
                continue;
            }
            let key = match group_by.as_str() {
                "project" => t.project.clone().unwrap_or_else(|| "(none)".to_string()),
                "status" => t.status.as_str().to_string(),
                "priority" => t.priority.map(|x| x.as_str().to_string()).unwrap_or_else(|| "(none)".to_string()),
                _ => unreachable!(),
            };
            let agg = groups.entry(key).or_insert(Agg { count: 0, est_secs: 0, tracked_secs: 0, overdue: 0 });
            agg.count += 1;
            // Saturating: a single estimate is bounded by `duration_secs`, but a
            // roll-up sums arbitrarily many rows. A clamped total is wrong-but-
            // visible; a wrapped one is negative nonsense and a panic in debug.
            if let Some(e) = t.estimate.as_deref().and_then(duration_secs) {
                agg.est_secs = agg.est_secs.saturating_add(e);
            }
            agg.tracked_secs = agg.tracked_secs.saturating_add(t.tracked_seconds);
            if t.status.is_open() {
                if let (Some(due), Some(n)) = (t.due.as_deref().and_then(parse_ts), now_ts) {
                    if due < n {
                        agg.overdue += 1;
                    }
                }
            }
        }

        let mut out = Vec::new();
        for (key, agg) in groups {
            let mut obj = Map::new();
            obj.insert(group_by.clone(), Value::String(key));
            obj.insert("count".into(), json!(agg.count));
            for m in &metrics {
                match m.as_str() {
                    "count" => {}
                    "est_total" => {
                        obj.insert("est_total".into(), json!(iso_duration(agg.est_secs)));
                    }
                    "tracked_total" => {
                        obj.insert("tracked_total".into(), json!(iso_duration(agg.tracked_secs)));
                    }
                    "overdue" => {
                        obj.insert("overdue".into(), json!(agg.overdue));
                    }
                    _ => {}
                }
            }
            out.push(Value::Object(obj));
        }

        Ok(json!({ "groups": out, "generated": now() }))
    }

    // ---- store.export --------------------------------------------------------

    /// Export the tasks matching `filter` as `{tasks, dropped_dependencies}`.
    ///
    /// A filter selects a *subset*, but a dependency edge points outside the
    /// subset as happily as inside it. An export that names an id it does not
    /// carry is not a document — it is a dangling pointer, and `store.import`
    /// now (correctly) rejects it. So edges are trimmed to the exported set in a
    /// second pass, and the count of trimmed edges is reported: silently losing
    /// a dependency is exactly the kind of thing that must be visible.
    /// `dropped_dependencies` is always present and is 0 for an unfiltered
    /// export, which stays a byte-identical round trip.
    pub fn store_export(&self, p: &Value) -> Result<Value, ApiError> {
        let filter = Filter::parse(&opt_str(p, "filter")?.unwrap_or_default(), Timestamp::now())
            .map_err(ApiError::bad_request)?;
        let mut stmt = self.conn.prepare(&format!("SELECT {TASK_COLS} FROM tasks ORDER BY short_id"))?;
        let rows = stmt.query_map([], map_task_row)?;

        // Pass 1: which tasks survive the filter. Edges can only be resolved
        // against the *whole* selected set, so nothing is emitted yet.
        let mut selected: Vec<(Task, Vec<String>)> = Vec::new();
        for r in rows {
            let t = r?;
            let tags = task_tags(&self.conn, &t.id)?;
            let blocked = self.is_blocked(&t.id)?;
            let ctx = MatchCtx {
                status: t.status,
                project: t.project.as_deref(),
                tags: &tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked,
            };
            if !filter.matches(&ctx) {
                continue;
            }
            selected.push((t, tags));
        }
        let present: HashSet<&str> = selected.iter().map(|(t, _)| t.id.as_str()).collect();

        // Pass 2: emit, keeping only edges whose target is also being emitted.
        let mut dropped = 0i64;
        let mut out = Vec::with_capacity(selected.len());
        for (t, tags) in &selected {
            out.push(self.export_task(t, tags, &present, &mut dropped)?);
        }
        Ok(json!({
            "tasks": out,
            "dropped_dependencies": dropped,
            // D37: a project is a RECORD (D21/D22/D23), not a string that happens
            // to appear on tasks, and an export that carries only the string is
            // not the self-contained document D12 promises — restoring it lost
            // every description, every archived flag, and the default, leaving a
            // store whose tasks name projects `tasqx projects` does not list and
            // `task.add` refuses. Always ALL of them, archived included and
            // regardless of `filter`: a filter selects TASKS, and a project the
            // selected tasks do not mention is not a dangling pointer, so there
            // is nothing to trim and no second `dropped_` counter to explain.
            "projects": self.export_projects()?,
            // Store state, so the document carries it (D21: it lives in the
            // store's `config` table, never in config.toml). `null` when there
            // is none, which is a fact and not an omission.
            "default_project": self.default_project(),
        }))
    }

    /// Every project row, name-ordered, in the canonical §3 shape. D37.
    fn export_projects(&self) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description, archived, created FROM projects ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "description": r.get::<_, Option<String>>(2)?,
                "archived": r.get::<_, i64>(3)? != 0,
                "created": r.get::<_, String>(4)?,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Build the canonical §3 export object for one task. serde_json's default
    /// `Map` is a `BTreeMap`, so keys serialize sorted (canonical form).
    /// `present` is the id set being exported; edges leaving it are dropped and
    /// counted into `dropped`.
    fn export_task(
        &self,
        t: &Task,
        tags: &[String],
        present: &HashSet<&str>,
        dropped: &mut i64,
    ) -> Result<Value, ApiError> {
        let all = self.depends_on_ids(&t.id)?;
        let kept: Vec<String> =
            all.iter().filter(|d| present.contains(d.as_str())).cloned().collect();
        *dropped += (all.len() - kept.len()) as i64;
        Ok(flag_unrecognized_status(t, json!({
            "id": t.id,
            "short_id": t.short_id,
            "title": t.title,
            // Verbatim, not canonicalized: export is the escape hatch out of a
            // store the reader could not fully understand, so it must hand back
            // what is actually in the file. Re-importing it then fails naming
            // this value (the import gate), which is what tells the user which
            // line to edit — a rescue that quietly rewrote it would leave them
            // with no evidence and a store that still disagrees with itself.
            "status": t.status_text(),
            "priority": t.priority.map(|x| x.as_str()),
            "project": t.project,
            "tags": tags,
            "due": t.due,
            "scheduled": t.scheduled,
            "wait": t.wait,
            "estimate": t.estimate,
            "recurrence": t.recurrence,
            "remind": t.remind,
            "depends_on": kept,
            "annotations": self.annotations_of(&t.id)?,
            "urgency": urgency::score(t.priority, t.due.as_deref(), &t.created),
            "created": t.created,
            "modified": t.modified,
            "completed": t.completed,
            "_rev": t.rev,
        })))
    }

    // ---- store.import --------------------------------------------------------

    pub fn store_import(&self, p: &Value) -> Result<Value, ApiError> {
        let tasks = req_array(p, "tasks")
            .map_err(|e| ApiError::bad_request(format!("{} — store.import requires a `tasks` array", e.message)))?;

        // D37: the projects half of the document, optional so an export written
        // by an older tasqx — which had no such section — still imports. Its
        // PRESENCE is load-bearing, not just its contents: a payload that
        // declares its projects is claiming to be self-contained, and is held to
        // it below; one that does not is a legacy document whose projects can
        // only be inferred from the tasks. `opt_array` distinguishes the two,
        // which `unwrap_or_default` would have flattened.
        let declared = opt_array(p, "projects")?.cloned();
        // Read before the transaction so a malformed default is refused without
        // taking the write lock. Validated below either way — whether a document
        // is coherent is a property of the document, not of where it lands.
        let want_default = opt_str_nonempty(p, "default_project")?;

        let mut imported = 0i64;
        let mut projects_imported = 0i64;
        // Names minted by inference rather than sent as records. A write the
        // caller did not ask for must be visible, so these are reported.
        let mut projects_created: Vec<String> = Vec::new();
        // (task_id, its raw `depends_on` value) collected during pass 1.
        // (task_id, its validated `depends_on` ids) — typed at collection time so
        // pass 2 cannot inherit a silently-dropped edge from a wrong-typed value.
        let mut edges: Vec<(String, Vec<String>)> = Vec::new();
        let tx = self.begin()?;

        // Pass 0: projects, before any task, so a task's `project` can be checked
        // against the document's own records rather than against whatever the
        // destination happened to already hold.
        if let Some(rows) = &declared {
            for pv in rows {
                let pv = import_shape("", "project", pv)?;
                // D36/D28: a LOOKUP-strength read, not `req_str`. A store written
                // before D23 can hold a whitespace-named project, its export says
                // so, and refusing the name here would make that document — the
                // escape hatch out of exactly such a store — impossible to
                // restore. `""` is still refused.
                let name = req_str_lookup(pv, "name").map_err(|e| {
                    ApiError::bad_request(format!(
                        "{} — each imported project requires a string `name`",
                        e.message
                    ))
                })?;
                import_keys(&format!("project {name}, "), "project", pv, IMPORT_PROJECT_KEYS)?;
                let description = import_project_field(
                    &name,
                    "description",
                    opt_str_nonempty(pv, "description"),
                )?;
                let archived =
                    import_project_field(&name, "archived", opt_bool(pv, "archived"))?.unwrap_or(false);
                let created = import_project_field(&name, "created", opt_str_nonempty(pv, "created"))?
                    .unwrap_or_else(now);
                let payload_id =
                    import_project_field(&name, "id", opt_str_nonempty(pv, "id"))?;
                let row_id = upsert_project(&tx, &name, description.as_deref(), archived, &created, payload_id.as_deref())?;
                insert_event(
                    &tx,
                    Entity::Project,
                    &row_id,
                    "import",
                    &json!({ "name": name, "archived": archived }),
                )?;
                projects_imported += 1;
            }
        }
        for tv in tasks {
            // Shape first, fields second: a non-object entry used to be
            // diagnosed by `req_str` as "missing required field: id", sending
            // the reader to hunt for a key in a value that cannot hold one.
            let tv = import_shape("", "task", tv)?;
            let id = req_str(tv, "id").map_err(|e| {
                ApiError::bad_request(format!("{} — each imported task requires a string `id`", e.message))
            })?;
            let id = id.as_str();
            // After `id`, so the error can name the task the caller must edit —
            // one bad field in a thousand-line export is useless without it.
            import_keys(&format!("task {id}, "), "task", tv, IMPORT_TASK_KEYS)?;
            let short_id = import_field(id, "short_id", req_i64(tv, "short_id"))?;
            // D17's rule where the value ENTERS: `short_id` is untrusted i64 and
            // the mint floor below is `short_id + 1`, which panicked in debug and
            // wrapped in release at `i64::MAX` — leaving a floor of `i64::MIN`, so
            // the next `add` re-minted a live short_id and broke D4. The counter
            // starts at 1 and only advances, so anything outside 1..i64::MAX is a
            // value no minter could have produced.
            let short_id_floor = short_id
                .checked_add(1)
                .filter(|_| short_id >= 1)
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "store.import: task {id} has short_id {short_id} — expected an integer \
                         from 1 to {}",
                        i64::MAX - 1
                    ))
                })?;
            // D35 + D16: `task.add` refuses an empty title through `req_str`, so
            // import does too. `title: ""` used to store a titleless task that
            // `add` cannot create and every listing renders as a blank row.
            let title = import_field(id, "title", req_str(tv, "title"))?;
            // Validated, not carried verbatim: an unrecognized status used to be
            // written to the row as-is and then laundered back to `pending` by
            // `map_task_row`, so a `done` task with a mis-cased status resurfaced
            // as open work while still carrying `completed`. Reject like D12 does
            // for a bad reference, and store the canonical spelling so the reader
            // never has to guess.
            let raw_status = import_field(id, "status", opt_str_nonempty(tv, "status"))?
                .unwrap_or_else(|| "pending".to_string());
            let raw_status = raw_status.as_str();
            let status = Status::parse(raw_status)
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "store.import: task {id} has status {raw_status:?} — expected one of {}",
                        Status::ALL.map(Status::as_str).join(", ")
                    ))
                })?
                .as_str();
            let priority = match import_field(id, "priority", opt_str_nonempty(tv, "priority"))? {
                Some(raw) => Some(Priority::parse(&raw).map(Priority::as_str).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "store.import: task {id} has priority {raw:?} — expected one of {}",
                        Priority::ALL.map(Priority::as_str).join(", ")
                    ))
                })?),
                None => None,
            };
            // D35: D18's rule on the import path — `project: ""` used to become
            // NULL, the ghost-bucket state D18 exists to prevent.
            let project = import_field(id, "project", opt_str_nonempty(tv, "project"))?;
            // D37 / N3b: D23 closed this for `task.add` and `task.modify` — "an
            // unknown --project exits 4 naming it, because a typo lost the task
            // silently" — and left import open, so a payload could mint a task in
            // a bucket no project surface has ever heard of. What "closed" means
            // depends on what the document claimed:
            //
            //   * It declared its projects → it is authoritative, and naming a
            //     project it did not define is an incoherent document. Refuse,
            //     naming the task and the value, exactly as D23 does at the other
            //     two doors. This is the only way a typo in a hand-edited export
            //     is ever caught.
            //   * It declared none (an older tasqx) → there is nothing to be
            //     incoherent WITH, and refusing would make every legacy export
            //     unrestorable. Infer the row instead, so the store still ends up
            //     coherent — the name the import accepted is a name `add`
            //     accepts — and report the mint, because a record the caller did
            //     not send is not something to write silently.
            //
            // Existence only, never archived state: an export legitimately holds
            // done work in a retired project (D22 puts a project out of rotation
            // for NEW work), and refusing to restore history is not what that
            // rule says.
            if let Some(name) = &project {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE name = ?1)",
                    params![name],
                    |r| r.get(0),
                )?;
                if !exists {
                    if declared.is_some() {
                        return Err(ApiError::bad_request(format!(
                            "store.import: task {id} names project {name:?}, which the payload's \
                             `projects` section does not define and the store does not have — \
                             add it to `projects`, or create it first with `tasqx init {name}`"
                        )));
                    }
                    let row_id = upsert_project(&tx, name, None, false, &now(), None)?;
                    insert_event(
                        &tx,
                        Entity::Project,
                        &row_id,
                        "import",
                        &json!({ "name": name, "inferred": true }),
                    )?;
                    projects_created.push(name.clone());
                }
            }
            // The same gate the CLI, the JSON API and MCP pass, called on the
            // import payload itself rather than restated here: these four fields
            // used to be read raw into the INSERT, so `due whenever` entered a
            // store no reader downstream can compare, sort or render. Prefixing
            // the validator's own message is all the import layer adds — one
            // failing task out of a thousand-line export is useless without its
            // id and column.
            let now_ts = Timestamp::now();
            let due = import_field(id, "due", opt_when(tv, "due", now_ts))?;
            let scheduled = import_field(id, "scheduled", opt_when(tv, "scheduled", now_ts))?;
            let wait = import_field(id, "wait", opt_when(tv, "wait", now_ts))?;
            // The EXTRACTION is wrapped as well as the parse. These three read
            // their value outside `import_field`, so `estimate:""` was refused
            // by a message that never said which of a thousand exported tasks
            // held it, while `estimate:"3 fortnights"` — the same field, one
            // error path over — named it. Both doors now go through the wrapper
            // that exists for exactly this reason.
            let estimate = match import_field(id, "estimate", opt_str_nonempty(tv, "estimate"))? {
                Some(s) => Some(import_field(id, "estimate", datetime::parse_duration(&s))?),
                None => None,
            };
            // The last two fields D28 skipped, now through the SAME two parsers
            // `task.add` and `task.modify` already call — parse-then-normalize,
            // not a second spelling of the rule. Carried verbatim, these let a
            // payload write `remind:"sometime"` that `add` rejects, and then
            // re-export it to every downstream store (D16); unparseable, neither
            // field ever schedules, so the user is simply never reminded and
            // nothing says why. "Re-normalizing perturbs the round trip" was a
            // code comment, never a decision: normalization runs the same
            // functions that WROTE the stored form, so it is idempotent on
            // anything an export produced (D12 stays byte-identical).
            let recurrence = match import_field(id, "recurrence", opt_str_nonempty(tv, "recurrence"))? {
                Some(s) => Some(import_field(id, "recurrence", recur::parse_rule(&s))
                    .map(|r| recur::rule_to_string(&r))?),
                None => None,
            };
            // `parse_remind` validates the SHAPE without collapsing it: an
            // offset stays the symbolic `-1h` that re-anchors when `due` moves,
            // and only the absolute branch resolves — exactly as on `add`.
            let remind = match import_field(id, "remind", opt_str_nonempty(tv, "remind"))? {
                Some(s) => Some(import_field(id, "remind", remind::parse_remind(&s, now_ts))
                    .map(|r| remind::spec_to_string(&r))?),
                None => None,
            };
            // Through the SAME gate as due/scheduled/wait above, not a second
            // spelling of it. B2 closed those four and left these three, so
            // `"created":"not-a-date"` still imported with rc=0 and came back out
            // of the next export verbatim — and `created` feeds `urgency::score`
            // three lines down, so the garbage silently flattened the ranking
            // every list is sorted by. The store writes all six as RFC3339, which
            // `parse_when` short-circuits on, so D12's byte-identical round trip
            // is untouched.
            let created = import_field(id, "created", opt_when(tv, "created", now_ts))?.unwrap_or_else(now);
            let modified = import_field(id, "modified", opt_when(tv, "modified", now_ts))?.unwrap_or_else(now);
            let completed = import_field(id, "completed", opt_when(tv, "completed", now_ts))?;
            let rev = import_field(id, "_rev", opt_i64(tv, "_rev"))?.unwrap_or(1);
            let urgency = urgency::score(
                priority.and_then(Priority::parse),
                due.as_deref(),
                &created,
            );

            // Upsert by id.
            tx.execute(
                "INSERT INTO tasks (id, short_id, title, status, priority, project, due, \
                 scheduled, wait, estimate, recurrence, urgency, active_since, tracked_seconds, \
                 rev, created, modified, completed, remind) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,NULL,0,?13,?14,?15,?16,?17) \
                 ON CONFLICT(id) DO UPDATE SET \
                 short_id=?2, title=?3, status=?4, priority=?5, project=?6, due=?7, \
                 scheduled=?8, wait=?9, estimate=?10, recurrence=?11, urgency=?12, \
                 rev=?13, created=?14, modified=?15, completed=?16, remind=?17",
                params![
                    id, short_id, title, status, priority, project, due, scheduled, wait,
                    estimate, recurrence, urgency, rev, created, modified, completed, remind
                ],
            )?;

            // short_id must never later be re-minted (§12-D4).
            bump_short_id_floor(&tx, short_id_floor)?;

            // Replace tags.
            tx.execute("DELETE FROM task_tags WHERE task_id = ?1", params![id])?;
            for tg in import_field(id, "tags", opt_str_array(tv, "tags"))? {
                ensure_tag_link(&tx, id, &tg)?;
            }

            // Replace annotations.
            tx.execute("DELETE FROM annotations WHERE task_id = ?1", params![id])?;
            if let Some(anns) = import_field(id, "annotations", opt_array(tv, "annotations"))? {
                for a in anns {
                    // `Value::get` answers None on a non-object, so every field
                    // fell back to its default: `annotations:[42]` MINTED a
                    // blank annotation with a fresh uuid, and `{"text":"hi"}`
                    // stored an empty body. Fabricating a row is worse than
                    // dropping one — the caller's note is gone and something
                    // stands in its place.
                    import_keys(
                        &format!("task {id}, "),
                        "annotations[]",
                        a,
                        IMPORT_ANNOTATION_KEYS,
                    )?;
                    let aid = import_field(id, "annotations[].id", opt_str_nonempty(a, "id"))?
                        .unwrap_or_else(|| Uuid::now_v7().to_string());
                    let body = import_field(id, "annotations[].body", req_str(a, "body"))?;
                    let acreated = import_field(id, "annotations[].created", opt_str_nonempty(a, "created"))?
                        .unwrap_or_else(now);
                    tx.execute(
                        "INSERT OR REPLACE INTO annotations (id, task_id, body, created) VALUES (?1,?2,?3,?4)",
                        params![aid, id, body, acreated],
                    )?;
                }
            }

            // Edges are deferred to pass 2: a payload may list a target *after*
            // its dependent, and the FOREIGN KEY would reject it here.
            tx.execute("DELETE FROM dependencies WHERE task_id = ?1", params![id])?;
            edges.push((
                id.to_string(),
                import_field(id, "depends_on", opt_str_array(tv, "depends_on"))?,
            ));

            insert_event(&tx, Entity::Task, id, "import", &json!({ "short_id": short_id }))?;
            imported += 1;
        }

        // Pass 2: wire the edges now that every task in the payload exists.
        // A target may live in the payload *or* already in the store (importing
        // one filtered slice on top of another is a normal workflow); anything
        // else is a dangling pointer and the import fails naming it. Silently
        // inserting it — the old behaviour — produced an edge no reader could
        // see and no `undep` could remove, which detonated the moment the target
        // finally arrived. Same transaction, so a reject writes nothing at all.
        for (id, deps) in &edges {
            for d in deps {
                let d = d.as_str();
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
                    params![d],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(ApiError::bad_request(format!(
                        "store.import: task {id} depends on {d}, which is neither in the \
                         payload nor in the store (export the dependency too, or drop the edge)"
                    )));
                }
                // Import is not a back door around the graph invariants that
                // `dependency.add` enforces. Without these two guards a payload
                // could mint a task blocked by itself, or a mutual cycle that
                // silently empties the working set — states the API itself calls
                // a conflict, and which re-export verbatim so the corruption
                // outlives the store that created it. The FOREIGN KEY above
                // constrains existence, not acyclicity.
                if d == id {
                    return Err(ApiError::conflict(format!(
                        "store.import: task {id} depends on itself — a task cannot depend on itself"
                    )));
                }
                // Same DFS `dependency.add` uses, on the transaction's own
                // write-locked snapshot: if the target already reaches the
                // dependent, this edge closes a cycle.
                if reaches(&tx, d, id)? {
                    return Err(ApiError::conflict(format!(
                        "store.import: dependency would create a cycle: \
                         {d} already depends on {id}"
                    )));
                }
                tx.execute(
                    "INSERT OR IGNORE INTO dependencies (task_id, depends_on_id) VALUES (?1,?2)",
                    params![id, d],
                )?;
            }
        }
        // Pass 3: the default, last, because it must be checked against the
        // projects this very transaction wrote. D21's rule — nothing silently
        // steals the default — applies here more than anywhere else: import is
        // the only write that can carry SOMEONE ELSE'S default in its payload,
        // and redirecting where a bare `add` lands is the invisible-write bug
        // D21 exists to kill. So the document's default is honoured only by a
        // store that has none; otherwise the standing one wins. It is validated
        // either way, so the same document is not coherent in one store and
        // incoherent in another.
        let standing = get_config(&tx, DEFAULT_PROJECT_KEY);
        if let Some(name) = &want_default {
            let row: Option<i64> = tx
                .query_row("SELECT archived FROM projects WHERE name = ?1", params![name], |r| {
                    r.get(0)
                })
                .optional()?;
            match row {
                None => {
                    return Err(ApiError::bad_request(format!(
                        "store.import: `default_project` names {name:?}, which the payload's \
                         `projects` section does not define and the store does not have"
                    )))
                }
                // D22: archived is out of rotation, so a default aimed at one is
                // a state no live sequence of calls can reach and the D23
                // open-time repair would undo on the next open anyway.
                Some(a) if a != 0 => {
                    return Err(ApiError::bad_request(format!(
                        "store.import: `default_project` names {name:?}, which is archived \
                         (an archived project cannot be the default)"
                    )))
                }
                Some(_) => {}
            }
            if standing.is_none() {
                set_config(&tx, DEFAULT_PROJECT_KEY, name)?;
            }
        }
        let default_project = standing.or_else(|| want_default.clone());
        tx.commit()?;

        // All four always present: a machine consumer must be able to tell "no
        // projects in the document" from "this build does not report them", the
        // same reason `dropped_dependencies` and `default_cleared` are never
        // omitted.
        Ok(json!({
            "imported": imported,
            "projects_imported": projects_imported,
            "projects_created": projects_created,
            "default_project": default_project,
        }))
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
            ("WHERE entity = ?1".to_string(), Some(ent.as_str().to_string()))
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
        let task = self.resolve_ref(p)?;
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

        let tx = self.begin()?;
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
    pub fn capabilities(&self) -> Value {
        let mut v = crate::dispatch::capabilities();
        v["default_project"] = match self.default_project() {
            Some(name) => Value::String(name),
            None => Value::Null,
        };
        v
    }
}

// ---- free helpers -----------------------------------------------------------

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
        .query_row("SELECT archived FROM projects WHERE name = ?1", params![name], |r| r.get(0))
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
        let mut stmt =
            conn.prepare("SELECT depends_on_id FROM dependencies WHERE task_id = ?1")?;
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
        Err(ApiError::bad_request(format!("{field} must be a string or null")))
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
    "id", "short_id", "title", "status", "priority", "project", "tags", "due", "scheduled",
    "wait", "estimate", "recurrence", "remind", "depends_on", "annotations", "urgency",
    "created", "modified", "completed", "_rev", "status_unrecognized",
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
        ApiError::bad_request(format!("store.import: project {name}, {field}: {}", e.message))
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
        .query_row("SELECT id FROM projects WHERE name = ?1", params![name], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        tx.execute(
            "UPDATE projects SET description = ?2, archived = ?3 WHERE id = ?1",
            params![id, description, archived as i64],
        )?;
        return Ok(id);
    }
    let taken = |id: &str| -> Result<bool, ApiError> {
        Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)", params![id], |r| {
            r.get(0)
        })?)
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
    let obj = import_shape(ctx, label, v)?.as_object().expect("import_shape proved it");
    let unknown: Vec<&str> =
        obj.keys().filter(|k| !accepted.contains(&k.as_str())).map(String::as_str).collect();
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
        unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "),
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
    flag_unrecognized_status(t, json!({
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
    }))
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
        keys.push(SortKey { key: key.to_string(), desc });
    }
    if keys.is_empty() {
        // The documented default, spelled from the same list it validates.
        keys.push(SortKey { key: SORT_KEYS[0].to_string(), desc: true });
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
        e.conn.execute("DELETE FROM tasks WHERE id = ?1", params![bid]).unwrap();
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
        e.task_done(&json!({ "ref": done["short_id"].clone() })).unwrap();
        e.task_cancel(&json!({ "ref": gone["short_id"].clone() })).unwrap();
        for (row, secs) in [(&done, 3600i64), (&gone, 1800i64)] {
            e.conn
                .execute(
                    "UPDATE tasks SET tracked_seconds=?1 WHERE id=?2",
                    params![secs, row["id"].as_str().unwrap()],
                )
                .unwrap();
        }

        let g = |p: Value| -> Value {
            e.report_summary(&p).unwrap()["groups"][0].clone()
        };
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
            assert!(err.message.contains(bad), "{field}: {} must name the offending value", err.message);
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
            assert!(err.message.contains(bad), "{field}: {} must name the offending value", err.message);
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
        assert_eq!(get(&e)["due"], "2026-07-20T00:00:00Z", "a bare ISO date resolves to midnight UTC");
        assert_eq!(get(&e)["estimate"], "PT4H", "a human duration is stored ISO");

        // Absent: modifying only the title leaves due/estimate alone.
        e.task_modify(&json!({ "ref": r#ref, "set": { "title": "y" } })).unwrap();
        assert_eq!(get(&e)["due"], "2026-07-20T00:00:00Z");
        assert_eq!(get(&e)["estimate"], "PT4H");

        // Explicit null still clears.
        e.task_modify(&json!({ "ref": r#ref, "set": { "due": null, "estimate": null } })).unwrap();
        assert!(get(&e)["due"].is_null(), "null must still clear due");
        assert!(get(&e)["estimate"].is_null(), "null must still clear estimate");
    }
}
