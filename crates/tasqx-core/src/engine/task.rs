//! Task domain methods for Engine.

use super::*;

/// Which optional side tables a bulk snapshot load should read.
///
/// The bulk loader exists so no reader drifts back to point queries, but
/// "everything, always" made the cheapest reader pay for the most expensive
/// one: `task.list` — the hottest read in the tool, behind every `tasqx list`,
/// the TUI and the HTML report — used the tasks/tags/blocked triple and threw
/// the dependency edge list and every annotation away, after materialising a
/// `serde_json` object per annotation row. That cost scales with a
/// monotonically growing log, not with the page the caller asked for.
///
/// `tasks`, `tags` and the `blocked` set are NOT gateable: all three bulk
/// readers build a [`MatchCtx`] from them to evaluate the filter, so a variant
/// without them cannot answer the question it was asked. `blocked` and the
/// edge list are separate gates on purpose — they are separate statements, and
/// `task.list` needs only the yes/no.
///
/// CAVEAT a follow-up should close: a gated-away part arrives as an EMPTY
/// collection, indistinguishable from a task that genuinely has none. Making
/// that unrepresentable means changing `TaskSnapshot` itself (it lives in
/// `engine.rs`), so for now the rule is enforced by call site: only pass a
/// narrow variant from a reader that never touches the gated fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SnapshotParts {
    /// Each task's full dependency edge list (`store.export` trims edges
    /// leaving the exported set, so the yes/no `blocked` flag is not enough).
    depends_on: bool,
    /// Each task's annotations, as the exported `[{id, body, created}]`.
    annotations: bool,
}

impl SnapshotParts {
    /// Statements run regardless of the gates: tasks, tags, blocked.
    const BASE_STATEMENTS: usize = 3;

    /// Everything a filter needs and nothing else — `task.list`.
    pub(super) const FILTERS_ONLY: Self = Self {
        depends_on: false,
        annotations: false,
    };

    /// The whole task relation — `store.export`, which emits both gated parts.
    pub(super) const EVERYTHING: Self = Self {
        depends_on: true,
        annotations: true,
    };

    /// How many statements a load with these parts runs. Kept next to the
    /// gates themselves so adding a part cannot silently leave the O(1)
    /// statement contract unpinned.
    pub(super) const fn statement_count(self) -> usize {
        Self::BASE_STATEMENTS + self.depends_on as usize + self.annotations as usize
    }
}

/// The pre-existing whole-relation count stays the authority for the widest
/// variant, so `task_snapshot_statement_count_is_independent_of_task_count`
/// keeps testing the same contract it always did.
const _: () = assert!(SnapshotParts::EVERYTHING.statement_count() == SNAPSHOT_QUERY_COUNT);

impl Engine {
    // ---- task.add ------------------------------------------------------------

    /// `task.add` — create a task. Params: `title` (required), `project`,
    /// `priority`, `due`, `scheduled`, `wait`, `estimate`, `tags`, `recurrence`,
    /// `remind`.
    ///
    /// Every free-form spec (`estimate`, `recurrence`, `remind`, the three
    /// dates) is parsed and NORMALIZED before the insert, so a bad spec fails
    /// the add cleanly rather than landing in the store to be discovered later
    /// by whatever reads it. A future `wait`/`scheduled` starts the task in
    /// `backlog`; [`effective_status`] releases it without any further command.
    pub fn task_add(&self, p: &Value) -> Result<Value, ApiError> {
        let title = req_str(p, "title")?;
        // Gap fix A1: with no explicit project, inherit the default set by
        // `project.create` (init). An explicit project always wins.
        // D23: the selected project is validated below, inside the transaction.
        // This covers explicit input and the inherited default alike.
        // D35: `project: ""` used to read as "no project given" and INHERIT the
        // default — the caller who named a project got a different one. D18
        // refused the same string on `task.modify`; `add` never did.
        let explicit_project = opt_str_nonempty(p, "project")?;
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
        let status = effective_status(
            Status::Backlog,
            wait.as_deref(),
            scheduled.as_deref(),
            now_ts,
        );

        let id = Uuid::now_v7().to_string();
        let ts = now();
        let urg = urgency::score(priority, due.as_deref(), &ts);

        let tx = self.begin_mutation()?;
        // Resolve both explicit and inherited routing inside the IMMEDIATE
        // transaction. Otherwise an archive can clear/retire the default after
        // this command reads it but before this write obtains its lock.
        let project = match &explicit_project {
            Some(name) => Some(name.clone()),
            None => get_config(&tx, DEFAULT_PROJECT_KEY)?,
        };
        if let Some(name) = &project {
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

    /// `task.start` — open a time interval. Params: `ref`, `keep`.
    ///
    /// `pending -> active` only; any other status is `conflict`, except an
    /// already-`active` task, which is idempotent and returns the interval it is
    /// already in. Without `keep`, D6's single-active rule auto-stops whatever
    /// else was running — the alternative is two clocks and no way to tell which
    /// one was the truth.
    pub fn task_start(&self, p: &Value) -> Result<Value, ApiError> {
        let command = commands::parse_start_task(p)?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &command.target.value)?;

        match task.status {
            Status::Active => {
                // Idempotent: already running.
                return Ok(commands::TaskStarted {
                    id: task.id,
                    interval_started: task.active_since,
                }
                .into());
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

        // D6: single active by default — auto-stop any currently active task.
        if !command.keep {
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
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "start",
            &json!({ "interval_started": ts }),
        )?;
        tx.commit()?;

        Ok(commands::TaskStarted {
            id: task.id,
            interval_started: Some(ts),
        }
        .into())
    }

    // ---- task.stop -----------------------------------------------------------

    /// `task.stop` — close the open interval and fold it into
    /// [`Task::tracked_seconds`]. Param: `ref`. `active -> pending` only;
    /// stopping anything else is `conflict` rather than a no-op, because there
    /// is no interval to close and reporting success would say there was.
    pub fn task_stop(&self, p: &Value) -> Result<Value, ApiError> {
        let command = commands::parse_task_target(p)?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &command.value)?;
        if task.status != Status::Active {
            return Err(ApiError::conflict(format!(
                "cannot stop a {} task (only active -> pending)",
                task.status.as_str()
            )));
        }

        let ts = now();
        let elapsed = seconds_between(&task.active_since, &ts);
        let total = task.tracked_seconds + elapsed;

        tx.execute(
            "UPDATE tasks SET status='pending', active_since=NULL, \
             tracked_seconds=?1, rev=?2, modified=?3 WHERE id=?4",
            params![total, task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "stop",
            &json!({ "tracked": iso_duration(elapsed) }),
        )?;
        tx.commit()?;

        Ok(commands::TaskStopped {
            tracked: iso_duration(elapsed),
        }
        .into())
    }

    // ---- task.done -----------------------------------------------------------

    /// `task.done` — complete a task. Param: `ref`. `pending|active -> done`;
    /// an active task's open interval is closed on the way, so finishing
    /// directly never silently loses the time already tracked.
    ///
    /// Also the point recurrence spawns the next instance and dependents may
    /// become unblocked — both consequences of the same commit, so history and
    /// state cannot disagree about them.
    pub fn task_done(&self, p: &Value) -> Result<Value, ApiError> {
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
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

        // A recurring template spawns its next instance on completion (D2).
        // Tags belong to the same locked snapshot as the template row.
        let template_tags = task_tags(&tx, &task.id)?;
        tx.execute(
            "UPDATE tasks SET status='done', completed=?1, active_since=NULL, \
             tracked_seconds=?2, rev=?3, modified=?4 WHERE id=?5",
            params![ts, total, task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "done",
            &json!({ "completed": ts }),
        )?;

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
        let status = effective_status(
            Status::Backlog,
            new_wait.as_deref(),
            new_scheduled.as_deref(),
            now_ts,
        );
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
    fn compute_unblocked(tx: &rusqlite::Transaction, done_id: &str) -> Result<Vec<i64>, ApiError> {
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

    /// `task.modify` — set fields on a task. Params: `ref`, `set` (a non-empty
    /// object), `expected_rev`.
    ///
    /// `expected_rev` is the optimistic-concurrency guard: when supplied and it
    /// does not match the row's current `rev`, the write is refused with
    /// `conflict` instead of overwriting whatever landed in between. The check
    /// runs after `BEGIN IMMEDIATE`, because the only authoritative row is the
    /// one inside the write lock.
    pub fn task_modify(&self, p: &Value) -> Result<Value, ApiError> {
        // Preserve the public validation order without loading store state:
        // callers have always seen a missing `ref` before errors in `set`.
        let _ = ref_param(p)?;
        let set = req_object(p, "set").map_err(|e| {
            ApiError::bad_request(format!("{} (modify requires a `set` object)", e.message))
        })?;
        if set.is_empty() {
            return Err(ApiError::bad_request(
                "`set` must contain at least one field",
            ));
        }
        let expected_rev = opt_i64(p, "expected_rev")?;

        // Store-dependent validation starts only after BEGIN IMMEDIATE. Another
        // process may have changed the row after request parsing but before this
        // lock; the transaction's row is the only authoritative one.
        let tx = self.begin_mutation()?;
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
                        let s = v
                            .as_str()
                            .ok_or_else(|| ApiError::bad_request("priority must be a string"))?;
                        let pr = Priority::parse(s).ok_or_else(|| {
                            ApiError::bad_request(format!("invalid priority: {s}"))
                        })?;
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
                "scheduled" => assignments.push((
                    "scheduled",
                    nullable_when(v, "scheduled", Timestamp::now())?,
                )),
                "wait" => assignments.push(("wait", nullable_when(v, "wait", Timestamp::now())?)),
                "estimate" => assignments.push(("estimate", nullable_duration(v, "estimate")?)),
                "recurrence" => {
                    // Set a rule (validated + normalized) or clear it with null
                    // — the sanctioned "stop recurring" path (DESIGN §10, D2).
                    if v.is_null() {
                        assignments.push(("recurrence", Value::Null));
                    } else {
                        let s = v.as_str().ok_or_else(|| {
                            ApiError::bad_request("recurrence must be a string or null")
                        })?;
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
                    let s = v
                        .as_str()
                        .ok_or_else(|| ApiError::bad_request("status must be a string"))?;
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
                    return Err(ApiError::bad_request(format!(
                        "field not modifiable: {other}"
                    )));
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
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "modify",
            &Value::Object(set.clone()),
        )?;
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "_rev": new_rev }))
    }

    // ---- task.list -----------------------------------------------------------

    /// Load the complete task relation in a fixed number of statements. Bulk
    /// readers need the same relationship data to evaluate filters; grouping
    /// it here prevents each reader from drifting back to point queries.
    pub(super) fn load_task_snapshots(&self) -> Result<Vec<TaskSnapshot>, ApiError> {
        self.load_task_snapshots_for(SnapshotParts::EVERYTHING)
    }

    /// As [`Engine::load_task_snapshots`], but reading only the side tables
    /// `parts` asks for — see [`SnapshotParts`] for why that is not a
    /// micro-optimization.
    pub(super) fn load_task_snapshots_for(
        &self,
        parts: SnapshotParts,
    ) -> Result<Vec<TaskSnapshot>, ApiError> {
        let (snapshots, statements) = self.load_task_snapshots_counted_for(parts)?;
        // Per-variant, not the one global `SNAPSHOT_QUERY_COUNT`: a narrow
        // load legitimately runs fewer statements, so keeping the old constant
        // here would abort every debug-build `tasqx list` on this assert.
        debug_assert_eq!(statements, parts.statement_count());
        Ok(snapshots)
    }

    /// Count statements as they execute so the performance contract is
    /// directly regression-tested rather than inferred from the SQL text.
    /// Test-only since the widest variant now has no production caller that
    /// needs the count — `load_task_snapshots_for` asserts it internally.
    #[cfg(test)]
    pub(super) fn load_task_snapshots_counted(
        &self,
    ) -> Result<(Vec<TaskSnapshot>, usize), ApiError> {
        self.load_task_snapshots_counted_for(SnapshotParts::EVERYTHING)
    }

    /// Counting variant of [`Engine::load_task_snapshots_for`].
    pub(super) fn load_task_snapshots_counted_for(
        &self,
        parts: SnapshotParts,
    ) -> Result<(Vec<TaskSnapshot>, usize), ApiError> {
        let mut statements = 0usize;

        statements += 1;
        let tasks: Vec<Task> = {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT {TASK_COLS} FROM tasks"))?;
            let rows = stmt.query_map([], map_task_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };

        statements += 1;
        let mut tags: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT tt.task_id, t.name FROM task_tags tt \
                 JOIN tags t ON t.id = tt.tag_id ORDER BY tt.task_id, t.name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (task_id, tag) = row?;
                tags.entry(task_id).or_default().push(tag);
            }
        }

        statements += 1;
        let mut blocked = HashSet::new();
        {
            let terminal = Status::sql_in_list(Status::is_terminal);
            let mut stmt = self.conn.prepare(&format!(
                "SELECT DISTINCT d.task_id FROM dependencies d \
                 JOIN tasks t ON t.id = d.depends_on_id \
                 WHERE t.status NOT IN ({terminal})"
            ))?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                blocked.insert(row?);
            }
        }

        // The edge LIST is a different read from the blocked SET above: export
        // needs every edge (to trim the ones leaving the exported set), while
        // `task.list` only needs the yes/no. Gated separately for that reason.
        let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();
        if parts.depends_on {
            statements += 1;
            let mut stmt = self.conn.prepare(
                "SELECT d.task_id, t.id FROM dependencies d \
                 JOIN tasks t ON t.id = d.depends_on_id ORDER BY d.task_id, t.id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (task_id, depends_on_id) = row?;
                dependencies.entry(task_id).or_default().push(depends_on_id);
            }
        }

        // Only `store.export` emits annotations. Building a `serde_json` object
        // per annotation for every task in the store — then dropping the lot
        // before rendering — was the bulk of what a `tasqx list` spent its time
        // on, and it grows with the log rather than with the requested page.
        let mut annotations: HashMap<String, Vec<Value>> = HashMap::new();
        if parts.annotations {
            statements += 1;
            let mut stmt = self.conn.prepare(
                "SELECT task_id, id, body, created FROM annotations \
                 ORDER BY task_id, created, id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    json!({
                        "id": row.get::<_, String>(1)?,
                        "body": row.get::<_, String>(2)?,
                        "created": row.get::<_, String>(3)?,
                    }),
                ))
            })?;
            for row in rows {
                let (task_id, annotation) = row?;
                annotations.entry(task_id).or_default().push(annotation);
            }
        }

        let snapshots = tasks
            .into_iter()
            .map(|task| {
                let id = &task.id;
                TaskSnapshot {
                    tags: tags.remove(id).unwrap_or_default(),
                    blocked: blocked.contains(id),
                    depends_on: dependencies.remove(id).unwrap_or_default(),
                    annotations: annotations.remove(id).unwrap_or_default(),
                    task,
                }
            })
            .collect();
        Ok((snapshots, statements))
    }

    /// `task.list` — the main read. Params: `filter` (the [`crate::filter`]
    /// grammar), `sort` (a key from [`SORT_KEYS`], `-` for descending, default
    /// `-urgency`), `limit`, `fields` (a subset of [`TASK_FIELDS`]).
    ///
    /// Urgency is recomputed per row rather than read from the stored column:
    /// the due-proximity and age terms both move with the clock, so the
    /// persisted value is only as fresh as the last write.
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
        // Filter inputs only: the projection below emits the task columns, its
        // tags and `blocked`, and `TASK_FIELDS` has no key sourced from the
        // dependency or annotation tables. Loading those here meant every
        // `tasqx list` scanned both end to end and discarded the result.
        let mut all = self.load_task_snapshots_for(SnapshotParts::FILTERS_ONLY)?;

        // Urgency has time-dependent terms (due proximity, age), so the value
        // persisted at write time goes stale. Recompute it for the fetched page
        // before sorting/rendering so "urgency-hot first" stays honest.
        // Carry each surviving task's tags (already fetched for the filter) so
        // the projection loop below reuses them instead of re-querying.
        // `blocked` is carried alongside the tags for the same reason: it is
        // already computed here for the filter, and throwing it away meant
        // `@blocked` could FILTER on a fact that `fields:["blocked"]` could not
        // RETURN. A caller wanting it per row had to issue one `task.get` each.
        let mut tasks = Vec::new();
        for mut snapshot in all.drain(..) {
            let t = &mut snapshot.task;
            t.urgency = urgency::score(t.priority, t.due.as_deref(), &t.created);
            let ctx = MatchCtx {
                status: t.status,
                project: t.project.as_deref(),
                tags: &snapshot.tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked: snapshot.blocked,
            };
            if filter.matches(&ctx) {
                tasks.push(snapshot);
            }
        }

        // Sort (default: hottest urgency first). Validated, so an unknown key
        // fails here rather than quietly producing some other order.
        let sort_keys = parse_sort(p)?;
        tasks.sort_by(|a, b| compare_by(&a.task, &b.task, &sort_keys));

        // Limit.
        if let Some(limit) = opt_u64(p, "limit")? {
            tasks.truncate(limit as usize);
        }

        // Field projection (whole row when `fields` absent). Validated, so an
        // unknown key fails here rather than quietly yielding a narrower row.
        let fields = parse_fields(p)?;

        let mut out = Vec::with_capacity(tasks.len());
        for snapshot in &tasks {
            let full = list_row_json(&snapshot.task, &snapshot.tags, snapshot.blocked);
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
    pub(super) fn is_blocked(&self, task_id: &str) -> Result<bool, ApiError> {
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
    pub(super) fn depends_on_short_ids(&self, task_id: &str) -> Result<Vec<i64>, ApiError> {
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
    #[cfg(test)]
    pub(super) fn depends_on_ids(&self, task_id: &str) -> Result<Vec<String>, ApiError> {
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

    /// `task.get` — one task in full. Param: `ref` (short_id or UUID). Adds the
    /// three fields the row itself does not carry — `depends_on`, `annotations`,
    /// `blocked` — and recomputes `urgency` for the same reason `task.list` does.
    pub fn task_get(&self, p: &Value) -> Result<Value, ApiError> {
        let task = self.resolve_ref(p)?;
        let tags = task_tags(&self.conn, &task.id)?;
        let mut obj = task_to_json(&task, &tags);
        // Recompute urgency for a live read (list does the same).
        obj["urgency"] = json!(urgency::score(
            task.priority,
            task.due.as_deref(),
            &task.created
        ));
        obj["depends_on"] = json!(self.depends_on_short_ids(&task.id)?);
        obj["annotations"] = json!(self.annotations_of(&task.id)?);
        obj["blocked"] = json!(self.is_blocked(&task.id)?);
        Ok(obj)
    }

    // ---- task.cancel ---------------------------------------------------------

    /// `task.cancel` — abandon a task. Param: `ref`. `backlog|pending|active ->
    /// cancelled`. The row is retained, not deleted, which is what keeps the
    /// event log and the short_id sequence honest; it simply stops counting in
    /// reports ([`Status::counts_in_reports`]).
    pub fn task_cancel(&self, p: &Value) -> Result<Value, ApiError> {
        let command = commands::parse_task_target(p)?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &command.value)?;
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

        tx.execute(
            "UPDATE tasks SET status='cancelled', active_since=NULL, \
             tracked_seconds=?1, rev=?2, modified=?3 WHERE id=?4",
            params![total, task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "cancel",
            &json!({ "from": task.status.as_str() }),
        )?;
        // Cancelling a blocker resolves it (D11), so dependents may become
        // actionable — surface the same unblock cascade task.done reports.
        let unblocked = Self::compute_unblocked(&tx, &task.id)?;
        tx.commit()?;

        Ok(commands::TaskCancelled {
            short_id: task.short_id,
            unblocked,
        }
        .into())
    }

    // ---- task.reopen ---------------------------------------------------------

    /// `task.reopen` — bring a closed task back. Param: `ref`. `done|cancelled
    /// -> pending`, clearing [`Task::completed`] so the reopened task cannot
    /// still answer a `completed.after:` query about a week it is no longer
    /// finished in.
    pub fn task_reopen(&self, p: &Value) -> Result<Value, ApiError> {
        let command = commands::parse_task_target(p)?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &command.value)?;
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
        tx.execute(
            "UPDATE tasks SET status='pending', completed=NULL, rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "reopen",
            &json!({ "from": task.status.as_str() }),
        )?;
        tx.commit()?;

        Ok(commands::TaskReopened {
            short_id: task.short_id,
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two tasks — #2 blocked by #1 — plus one annotation and one tag, so every
    /// side table the bulk loader can read has at least one row to find.
    fn seeded() -> Engine {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "blocker", "tags": ["shared"] }))
            .unwrap();
        e.task_add(&json!({ "title": "dependent", "tags": ["shared"] }))
            .unwrap();
        e.dependency_add(&json!({ "ref": 2, "depends_on": 1 }))
            .unwrap();
        e.annotation_add(&json!({ "ref": 1, "body": "a note" }))
            .unwrap();
        e
    }

    #[test]
    fn task_list_never_touches_the_annotations_table() {
        let e = seeded();
        // Dropping the table is what PROVES the read is gone: a statement
        // count can be met while still scanning a table end to end, and it is
        // the scan — not the statement — that costs a `tasqx list` its time on
        // a store with thousands of annotations.
        e.conn().execute_batch("DROP TABLE annotations").unwrap();

        let out = e.task_list(&json!({ "sort": ["short_id"] })).unwrap();
        assert_eq!(out["count"], 2);
        // `blocked` is read from `dependencies` and must SURVIVE the trim:
        // gating the wrong statement away would silently break `@blocked`
        // filtering and the `blocked` field of every listed row.
        assert_eq!(out["tasks"][0]["blocked"], json!(false));
        assert_eq!(out["tasks"][1]["blocked"], json!(true));
    }

    #[test]
    fn snapshot_parts_gate_the_optional_side_tables() {
        let e = seeded();

        // Filter-only: tasks + tags + blocked, three statements, and the two
        // gated collections come back empty rather than half-populated.
        let (narrow, statements) = e
            .load_task_snapshots_counted_for(SnapshotParts::FILTERS_ONLY)
            .unwrap();
        assert_eq!(statements, SnapshotParts::FILTERS_ONLY.statement_count());
        assert_eq!(statements, 3);
        assert_eq!(narrow.len(), 2);
        assert!(narrow.iter().all(|s| s.depends_on.is_empty()));
        assert!(narrow.iter().all(|s| s.annotations.is_empty()));
        assert!(narrow.iter().all(|s| s.tags == ["shared"]));
        assert_eq!(narrow.iter().filter(|s| s.blocked).count(), 1);

        // Everything: the two extra statements run and their rows arrive.
        let (full, statements) = e.load_task_snapshots_counted().unwrap();
        assert_eq!(statements, SNAPSHOT_QUERY_COUNT);
        assert_eq!(full.iter().filter(|s| !s.depends_on.is_empty()).count(), 1);
        assert_eq!(full.iter().filter(|s| !s.annotations.is_empty()).count(), 1);
        // Same `blocked` answer either way — the narrow load is a smaller read,
        // not a different one.
        assert_eq!(full.iter().filter(|s| s.blocked).count(), 1);
    }

    /// Manual fixture, ignored in CI for the same reason as
    /// `benchmark_task_snapshot_bulk_readers`: wall-clock timing is a lousy
    /// correctness gate. It exists separately because that one seeds NO
    /// annotations, and so is structurally blind to the cost this variant
    /// removes — the side tables, not the task table, are what a big store
    /// grows. Release profile, 2000 tasks × 2 annotations, best of 5:
    /// 10.6 ms loading every part, 3.5 ms with `FILTERS_ONLY`.
    #[test]
    #[ignore = "manual annotated-store task.list benchmark"]
    fn benchmark_task_list_on_an_annotated_store() {
        use std::time::Instant;

        let task_count = 2_000usize;
        let e = Engine::open_in_memory().unwrap();
        let tasks: Vec<Value> = (0..task_count)
            .map(|index| {
                json!({
                    "id": format!("019f7eb6-0000-7000-8000-{:012x}", index + 1),
                    "short_id": index + 1,
                    "title": format!("benchmark task {index}"),
                    "tags": ["shared", format!("bucket-{}", index % 10)],
                    "annotations": [
                        { "body": "first note on this task", "created": "2026-01-01T00:00:00Z" },
                        { "body": "second note on this task", "created": "2026-01-02T00:00:00Z" },
                    ],
                })
            })
            .collect();
        e.store_import(&json!({ "tasks": tasks })).unwrap();

        let mut best = None;
        for _ in 0..5 {
            let started = Instant::now();
            let listed = e.task_list(&json!({ "limit": 12 })).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(listed["count"], 12);
            best = Some(best.map_or(elapsed, |b: std::time::Duration| b.min(elapsed)));
        }
        println!("task.list over {task_count} annotated tasks: {best:?} (best of 5)");
    }
}
