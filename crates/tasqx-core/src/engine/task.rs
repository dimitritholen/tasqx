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
    /// Each task's token measurements, as the exported `[{id, tool, …}]`.
    ///
    /// Gated for the same reason `annotations` is, and it is the same cost
    /// shape: `token_usage` grows with every attributed turn while the page a
    /// caller asked for does not, and `TASK_FIELDS` has no key sourced from it,
    /// so `task.list` built one `serde_json` object per measurement in the
    /// store and dropped the lot. Both readers that ask for the whole relation
    /// — `store.export` and `report.summary` — do consume it.
    tokens: bool,
}

impl SnapshotParts {
    /// Statements run regardless of the gates: tasks, tags, blocked.
    const BASE_STATEMENTS: usize = 3;

    /// Everything a filter needs and nothing else — `task.list`.
    pub(super) const FILTERS_ONLY: Self = Self {
        depends_on: false,
        annotations: false,
        tokens: false,
    };

    /// `task.list` when the caller PROJECTS `depends_on` — the filter inputs
    /// plus the edge list, and nothing else. One statement more than
    /// [`Self::FILTERS_ONLY`], paid only by the call that asked for it.
    pub(super) const FILTERS_AND_DEPENDENCIES: Self = Self {
        depends_on: true,
        annotations: false,
        tokens: false,
    };

    /// The whole task relation — `store.export` and `report.summary`, which
    /// between them emit every gated part.
    pub(super) const EVERYTHING: Self = Self {
        depends_on: true,
        annotations: true,
        tokens: true,
    };

    /// How many statements a load with these parts runs. Kept next to the
    /// gates themselves so adding a part cannot silently leave the O(1)
    /// statement contract unpinned.
    pub(super) const fn statement_count(self) -> usize {
        Self::BASE_STATEMENTS
            + self.depends_on as usize
            + self.annotations as usize
            + self.tokens as usize
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
        // #12: the start event is the durable half of the correlation record —
        // the attribution engine later pairs it with the done event to know
        // which session/transcript covered this interval.
        let mut start_payload = json!({ "interval_started": ts });
        command.correlation.apply(&mut start_payload);
        insert_event(&tx, Entity::Task, &task.id, "start", &start_payload)?;
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
        // Pure params first (#12/#13): correlation metadata and any
        // self-reported token usage are validated before the write lock,
        // exactly like every other parse-then-lock mutation.
        let correlation = commands::parse_correlation(p)?;
        let report = commands::parse_self_report(p)?;
        // D65: `tool` and `model` are facts about the completion, kept whether
        // or not the caller could also count tokens. Read off the report before
        // it is consumed, because `into_usage` folds them into a measurement
        // that only exists when a count was given.
        let (named_tool, named_model) = (report.tool.clone(), report.model.clone());
        let usage = report.into_usage(&correlation)?;
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
        // #12: the done event carries the correlation record for this
        // completion — see `commands::Correlation` for why it lives in the
        // event payload rather than on the task row.
        let mut done_payload = json!({ "completed": ts });
        correlation.apply(&mut done_payload);
        // One rule, not two: what the caller named is on the event whether or
        // not a measurement was written beside it. The measurement is the fact
        // about spend; the event is the audit of the call. They can differ —
        // the measurement's `tool` falls back to `client` and this does not —
        // and that is not drift to reconcile: this key records what was said,
        // the measurement records what was concluded.
        for (key, value) in [("tool", &named_tool), ("model", &named_model)] {
            if let Some(v) = value {
                done_payload[key] = json!(v);
            }
        }
        // #13: a self-report is one measurement row in the SAME transaction,
        // echoed in this done event's payload — NOT a second `token.add`
        // event, because one mutation writes exactly one event (the invariant
        // tests/engine.rs pins), and the completion is the occurrence the
        // measurement belongs to.
        if let Some(u) = &usage {
            done_payload["tokens"] = tokens::record_token_usage(&tx, &task.id, u)?;
        }
        insert_event(&tx, Entity::Task, &task.id, "done", &done_payload)?;

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
        // D50: a completion with no self-report nudges the machine caller
        // toward the primary channel. Response key only — built after the
        // commit, so it can never leak into the done event — and it asserts
        // nothing about ownership or spend: whether tokens were spent at all
        // is exactly what nobody but the caller knows.
        // D65: three states, and the response names which one it is. Saying
        // only what is missing is what made the old refusal feel arbitrary —
        // a caller who supplied everything it could observe was told, twice,
        // about the one thing it could not.
        if usage.is_none() {
            let recorded: Vec<&str> = [("tool", &named_tool), ("model", &named_model)]
                .into_iter()
                .filter_map(|(k, v)| v.as_ref().map(|_| k))
                .collect();
            out["tokens_hint"] = json!(if recorded.is_empty() {
                "no token counts were self-reported; log-parse attribution is \
                 a best-effort fallback — pass input_tokens/output_tokens/\
                 cache_read_tokens/cache_creation_tokens on completion for a \
                 reliable measurement"
                    .to_string()
            } else {
                format!(
                    "recorded {} on the completion event; no measurement was made because no \
                     token count was given — log-parse attribution is a best-effort fallback, \
                     and input_tokens/output_tokens/cache_read_tokens/cache_creation_tokens \
                     make it a measurement",
                    recorded.join(" and ")
                )
            });
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
    /// their dependencies resolved (i.e. closing this one cleared their last
    /// blocker). Both `task.done` and `task.cancel` call this — under D11 a
    /// cancelled blocker counts as resolved — so `done_id` is whichever task
    /// just closed, not necessarily a completed one. An empty result is a real
    /// answer, not a degenerate one: this list is what the CLI prints as "now
    /// actionable" and what an agent reads to decide what to pick up next.
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
                // `.optional()?`, not `.ok()`: the row provably exists (its id
                // came from the JOIN above, in this same transaction), so the
                // only thing a swallowed error could ever hide is a genuine
                // storage fault — shipped as a wrong list at `ok: true`.
                let sid = tx
                    .query_row(
                        "SELECT short_id FROM tasks WHERE id = ?1",
                        params![dep_task],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?;
                if let Some(sid) = sid {
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

        // Same gate, same reason as annotations above: `token_usage` is written
        // once per attributed turn and never pruned, so an ungated read here
        // put the growth of the telemetry log on the critical path of every
        // `tasqx list` — the exact cost this whole type exists to keep off it.
        let mut token_rows: HashMap<String, Vec<Value>> = HashMap::new();
        if parts.tokens {
            statements += 1;
            let mut stmt = self.conn.prepare(&format!(
                "SELECT task_id, {} FROM token_usage ORDER BY task_id, created, id",
                tokens::TOKEN_COLS
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    tokens::measurement_from_row(row, 1)?,
                ))
            })?;
            for row in rows {
                let (task_id, measurement) = row?;
                token_rows.entry(task_id).or_default().push(measurement);
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
                    tokens: token_rows.remove(id).unwrap_or_default(),
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
        // Read before the load: projecting `depends_on` is the one thing that
        // makes this reader need a side table, so the gate is decided here
        // rather than by loading everything and hoping.
        let fields = parse_fields(p)?;
        let want_deps = crate::engine::fields_want_depends_on(fields.as_ref());
        let parts = if want_deps {
            SnapshotParts::FILTERS_AND_DEPENDENCIES
        } else {
            SnapshotParts::FILTERS_ONLY
        };
        let mut all = self.load_task_snapshots_for(parts)?;

        // `TaskSnapshot::depends_on` carries store ids; every surface an agent
        // reads names dependencies by `short_id` (`task.get` does), so one map
        // over the rows already in hand translates them. Built before the
        // filter drains `all`, because an edge may point at a task the filter
        // is about to drop and the reader still has to be able to name it.
        let short_ids: std::collections::HashMap<String, i64> = if want_deps {
            all.iter()
                .map(|s| (s.task.id.clone(), s.task.short_id))
                .collect()
        } else {
            std::collections::HashMap::new()
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

        // How many rows MATCHED, counted before the window is applied (D70).
        // `count` has always been the number of rows returned, which is the
        // same number under no limit and a different one under any limit — so
        // a caller that did the right thing and bounded its request got a list
        // that looked complete, could not tell how much had been dropped, and
        // had no way to ask for the rest.
        let total = tasks.len();

        // Window: offset first, then limit. Both are optional and the pair is
        // meaningless without the stable tiebreak `compare_by` ends on — a
        // page walked over an order that varies between calls shows a row
        // twice or skips it, and nothing about the response would say so.
        let offset = opt_u64(p, "offset")?.unwrap_or(0) as usize;
        if offset > 0 {
            tasks.drain(..offset.min(tasks.len()));
        }
        if let Some(limit) = opt_u64(p, "limit")? {
            tasks.truncate(limit as usize);
        }
        // Nullable, never absent: a key that comes and goes makes every client
        // branch on presence, and this one would flip on the last page of
        // every walk (D63's rule for `annotations_next_offset`).
        let next_offset = match offset + tasks.len() {
            reached if reached < total => json!(reached),
            _ => Value::Null,
        };

        // Field projection (whole row when `fields` absent). Validated above,
        // so an unknown key fails before any of this rather than quietly
        // yielding a narrower row.
        let mut out = Vec::with_capacity(tasks.len());
        for snapshot in &tasks {
            let deps: Option<Vec<i64>> = want_deps.then(|| {
                let mut v: Vec<i64> = snapshot
                    .depends_on
                    .iter()
                    .filter_map(|id| short_ids.get(id).copied())
                    .collect();
                v.sort_unstable();
                v
            });
            let full = list_row_json(
                &snapshot.task,
                &snapshot.tags,
                snapshot.blocked,
                deps.as_deref(),
            );
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

        Ok(json!({
            "count": out.len(),
            "total": total,
            "next_offset": next_offset,
            "tasks": out,
        }))
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
    /// One page of a task's annotations, newest end first, plus how many there
    /// are in total.
    ///
    /// `limit` is `None` for "all of them" — the JSON API answered `task.get`
    /// whole from v1 onwards and may not start dropping rows on its own. The
    /// window is taken from the RECENT end and the page is then reversed back
    /// into chronological order, because the page is read as a history while
    /// the interesting end of a long one is the near end.
    ///
    /// The ordering carries `id` as a tiebreak in BOTH directions. `created`
    /// alone is not unique — several annotations can share a timestamp — and a
    /// window whose tie order differs between two queries pages inconsistently:
    /// a row is shown twice, or skipped, and the reader has no way to notice.
    fn annotations_page(
        &self,
        task_id: &str,
        limit: Option<u64>,
        offset: u64,
    ) -> Result<(Vec<Value>, u64), ApiError> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM annotations WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )?;
        let total = total.max(0) as u64;
        // SQLite reads a negative LIMIT as "no limit", which is how "all of
        // them" and "the newest N" stay one query rather than two that could
        // disagree about the ordering.
        let sql_limit = match limit {
            Some(n) => i64::try_from(n).unwrap_or(i64::MAX),
            None => -1,
        };
        let sql_offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, body, created FROM annotations WHERE task_id = ?1 \
             ORDER BY created DESC, id DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![task_id, sql_limit, sql_offset], |r| {
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
        v.reverse();
        Ok((v, total))
    }

    // ---- task.get ------------------------------------------------------------

    /// `task.get` — one task in full. Params: `ref` (short_id or UUID),
    /// `annotations_limit?`, `annotations_offset?`. Adds the four fields the row
    /// itself does not carry — `depends_on`, `annotations`, `tokens`,
    /// `blocked` — and recomputes `urgency` for the same reason `task.list`
    /// does.
    ///
    /// # The history is pageable, and an elided one says so
    ///
    /// A task's annotations are unbounded and the tasks worth reading are the
    /// ones that have most of them, so the richest history was the one a caller
    /// with a payload limit could not fetch — and there was no smaller answer to
    /// ask for. `annotations_limit` takes the newest N; `annotations_offset`
    /// walks back from there.
    ///
    /// Both defaults keep the frozen v1 answer intact: absent `annotations_limit`
    /// is every annotation, exactly what clients have read since the API was
    /// declared stable. The bound belongs to the transport that has a payload
    /// limit, and the MCP server supplies it there.
    ///
    /// `annotations_total` is present on every response, elided or not, because
    /// a count only a truncated caller sees is a count nobody compares against.
    /// `annotations_next_offset` is the offset that fetches the page behind this
    /// one, and `null` once there is none — a next page that keeps being
    /// advertised at the end of the history is a loop, not a hint.
    pub fn task_get(&self, p: &Value) -> Result<Value, ApiError> {
        // One point in time for the whole detail. This response is assembled
        // from six separate reads — the task row, its tags, its dependencies,
        // its annotations, the count of those annotations, and its
        // measurements — and without a snapshot a write landing between any two
        // of them ships an answer that never existed. The pagination made that
        // visible rather than theoretical: `annotations_total` and the page
        // itself are two statements, so a concurrent `annotation.add` produced a
        // total the rows disagreed with and an `annotations_next_offset`
        // computed from both.
        //
        // DEFERRED, exactly as `store_export` does it and for the same reason
        // (§2: concurrent readers never block) — the snapshot pins at the first
        // read and holds without taking the write lock. Bound to a NAME: `let _`
        // drops the guard on the spot and turns the whole thing into a no-op.
        let _snapshot = self.conn.unchecked_transaction()?;
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

        let limit = opt_u64(p, "annotations_limit")?;
        let offset = opt_u64(p, "annotations_offset")?.unwrap_or(0);
        let (annotations, total) = self.annotations_page(&task.id, limit, offset)?;
        let returned = annotations.len() as u64;
        obj["annotations"] = json!(annotations);
        obj["annotations_total"] = json!(total);
        // Echoed, not merely accepted. A page carries no evidence of where it
        // sits: the rendered view assumed every page started at the newest
        // annotation, so on the second page of ten it announced "newest first"
        // and called all six missing rows older, while four of them were newer
        // than anything on the page. A reader — human or model — cannot
        // reconstruct the offset from the rows, and neither could the renderer.
        obj["annotations_offset"] = json!(offset);
        // An empty page never advertises a successor: past the end of the
        // history `offset` would otherwise be handed straight back and the
        // caller would page in place forever.
        obj["annotations_next_offset"] = if returned > 0 && offset + returned < total {
            json!(offset + returned)
        } else {
            Value::Null
        };
        obj["tokens"] = json!(self.tokens_of(&task.id)?);
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
        // Read AFTER the UPDATE, so the reopened task is already back among
        // its dependents' unresolved blockers and the count below is the one
        // the store now holds.
        let blocked = Self::compute_reblocked(&tx, &task.id)?;
        tx.commit()?;

        Ok(commands::TaskReopened {
            short_id: task.short_id,
            blocked,
        }
        .into())
    }

    /// short_ids of the open dependents this reopen just put back into
    /// `blocked` — the inverse of [`Engine::compute_unblocked`].
    ///
    /// Run inside the reopening transaction and AFTER the status write, so a
    /// dependent flipped by this call is exactly one whose count of
    /// still-unresolved blockers is now **one**: the reopened task is back
    /// among them, and if it were not the only one the dependent was already
    /// blocked and nothing changed for it.
    ///
    /// It exists because `task.done` has always answered `unblocked` and its
    /// inverse answered nothing (D69). Reopening is what an agent does the
    /// moment it finds it closed a task too early, and it was removing work
    /// from its own actionable set with no signal: the next `@working` list
    /// came back shorter and no response said why.
    fn compute_reblocked(
        tx: &rusqlite::Transaction,
        reopened_id: &str,
    ) -> Result<Vec<i64>, ApiError> {
        // Enum-derived, never caller text — see `Status::sql_in_list`.
        let open = Status::sql_in_list(Status::is_open);
        let terminal = Status::sql_in_list(Status::is_terminal);
        let mut stmt = tx.prepare(&format!(
            "SELECT t.short_id FROM dependencies d              JOIN tasks t ON t.id = d.task_id              WHERE d.depends_on_id = ?1 AND t.status IN ({open})              AND ( SELECT COUNT(*) FROM dependencies d2                    JOIN tasks b ON b.id = d2.depends_on_id                    WHERE d2.task_id = t.id AND b.status NOT IN ({terminal}) ) = 1              ORDER BY t.short_id",
        ))?;
        let rows = stmt.query_map(params![reopened_id], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two tasks — #2 blocked by #1 — plus one annotation and one tag. It seeds
    /// no `token_usage` row: the third gated side table is covered by
    /// `report_summary_sums_token_measurements_per_group` (tests/increment.rs)
    /// and by the `EVERYTHING.statement_count() == SNAPSHOT_QUERY_COUNT` const
    /// assert above, not from here.
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

    /// One task carrying `n` annotations, bodies numbered so an assertion can
    /// name which ones came back.
    fn with_annotations(n: usize) -> Engine {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "long-running" })).unwrap();
        for i in 0..n {
            e.annotation_add(&json!({ "ref": 1, "body": format!("note {i}") }))
                .unwrap();
        }
        e
    }

    /// A caller who knows the history is long must be able to ask for less of
    /// it, and what they get must be the RECENT end.
    ///
    /// `task.get` had no bound of any kind: no limit, no offset, no
    /// newest-first. An MCP client hit its tool-output limit on a real task
    /// whose annotations had accumulated over five days and could not ask for a
    /// smaller answer — the task with the richest history was the one the tool
    /// could not return. Newest-first because in every use that hit this, the
    /// recent annotations were the wanted ones and the oldest were the reason
    /// the payload was large.
    ///
    /// The page itself stays in chronological order: it is read as a history,
    /// and reversing it would make the rendered view disagree with every other
    /// surface that prints annotations.
    #[test]
    fn task_get_returns_the_newest_annotations_when_a_limit_is_given() {
        let e = with_annotations(10);
        let out = e
            .task_get(&json!({ "ref": 1, "annotations_limit": 3 }))
            .unwrap();
        let bodies: Vec<&str> = out["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["body"].as_str().unwrap())
            .collect();
        assert_eq!(bodies, ["note 7", "note 8", "note 9"]);
        assert_eq!(out["annotations_total"], json!(10));
    }

    /// An offset walks BACKWARDS from the newest, so the caller pages into
    /// history without having to know how much of it there is first.
    #[test]
    fn annotations_offset_pages_back_through_the_history() {
        let e = with_annotations(10);
        let out = e
            .task_get(&json!({ "ref": 1, "annotations_limit": 3, "annotations_offset": 3 }))
            .unwrap();
        let bodies: Vec<&str> = out["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["body"].as_str().unwrap())
            .collect();
        assert_eq!(bodies, ["note 4", "note 5", "note 6"]);
    }

    /// Elision must be SAID, not merely done.
    ///
    /// Silent truncation is worse than the 58 KB payload it replaces: a reader
    /// who cannot tell a short history from a trimmed one draws conclusions
    /// from the half they were given. This project has paid for that shape
    /// repeatedly — a field that drives behaviour and appears on no read
    /// surface — so the response names the total and the offset that fetches
    /// the next page, and stops naming a next page once there is none.
    #[test]
    fn an_elided_history_names_its_total_and_the_offset_that_continues_it() {
        let e = with_annotations(10);
        let first = e
            .task_get(&json!({ "ref": 1, "annotations_limit": 4 }))
            .unwrap();
        assert_eq!(first["annotations_total"], json!(10));
        assert_eq!(first["annotations_next_offset"], json!(4));

        let last = e
            .task_get(&json!({ "ref": 1, "annotations_limit": 4, "annotations_offset": 8 }))
            .unwrap();
        assert_eq!(last["annotations"].as_array().unwrap().len(), 2);
        assert!(
            last["annotations_next_offset"].is_null(),
            "the oldest page must not advertise a page behind it"
        );
    }

    /// Absent means ALL, and it must keep meaning that.
    ///
    /// The bound is transport policy, applied by the MCP server where the
    /// payload limit lives (see `mcp::tools_call`); the JSON API itself may not
    /// start dropping rows from an answer clients have been reading whole since
    /// v1 was frozen. `0` therefore means zero rows, exactly as it does for
    /// `task.list`'s `limit` — one sentinel spelling across the API.
    #[test]
    fn an_absent_limit_returns_the_whole_history_and_zero_returns_none() {
        let e = with_annotations(10);
        let all = e.task_get(&json!({ "ref": 1 })).unwrap();
        assert_eq!(all["annotations"].as_array().unwrap().len(), 10);
        assert_eq!(all["annotations_total"], json!(10));
        assert!(all["annotations_next_offset"].is_null());

        let none = e
            .task_get(&json!({ "ref": 1, "annotations_limit": 0 }))
            .unwrap();
        assert_eq!(none["annotations"].as_array().unwrap().len(), 0);
        assert_eq!(none["annotations_total"], json!(10));
    }

    /// `task.get` must read every part of its answer from ONE snapshot.
    ///
    /// The response is assembled from six statements — the task row, tags,
    /// dependencies, the annotation page, the count behind it, and the
    /// measurements — and in WAL each takes its own snapshot, so a writer
    /// committing between two of them ships an answer that never existed. The
    /// pagination is what made it observable rather than theoretical:
    /// `annotations_total` and the page are separate reads, so a concurrent
    /// `annotation.add` yields a total the returned rows disagree with, and an
    /// `annotations_next_offset` computed from both.
    ///
    /// Structural, for the same reason `store_export_opens_its_snapshot_before_the_first_read`
    /// is: the interleaving point is inside SQLite and rusqlite's `hooks`
    /// feature — the only way to drive a write from between two of our reads —
    /// is not compiled in.
    #[test]
    fn task_get_opens_its_snapshot_before_the_first_read() {
        let source = include_str!("task.rs");
        // Assembled rather than written out: `dispatch`'s accepted-key guard
        // splits this same source at every `fn NAME(`, so a marker spelled in
        // full would register here as a second definition of the handler.
        let marker = format!("pub fn {}(", "task_get");
        let marker = marker.as_str();
        let start = source.find(marker).expect("task_get exists");
        let rest = &source[start..];
        // BOTH visibilities, unlike the `store_export` scan: the next item after
        // `task_get` is `pub fn task_cancel`, so a terminator of `\n    fn `
        // alone runs the slice on into every later handler — and one of those
        // opens a write transaction, which this guard then reports as a defect
        // in a function that never had one.
        let end = ["\n    pub fn ", "\n    fn "]
            .iter()
            .filter_map(|t| rest[marker.len()..].find(t))
            .min()
            .map(|offset| marker.len() + offset)
            .unwrap_or(rest.len());
        // Comments out: this function's prose names the constructor it does not
        // use, and a scanner that cannot tell code from a comment would read
        // that as the defect it warns about.
        let body: String = rest[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = body.as_str();

        let guard = body
            .find("unchecked_transaction()")
            .expect("task.get must open a transaction so its reads share one snapshot");
        let first_read = body
            .find("self.resolve_ref(p)")
            .expect("task.get resolves the ref before anything else");
        assert!(
            guard < first_read,
            "the snapshot pins at the first read, so the transaction must be opened before it"
        );
        assert!(
            !body.contains("let _ = self.conn.unchecked_transaction"),
            "a `_` binding drops the transaction on the spot, making the guard a no-op"
        );
        // DEFERRED, never IMMEDIATE: a reader that takes the write lock blocks
        // every writer for its duration, which §2's "concurrent readers never
        // block" forbids.
        for forbidden in ["begin_mutation", "Immediate"] {
            assert!(
                !body.contains(forbidden),
                "task.get is a read and must not take the write lock (`{forbidden}`)"
            );
        }
    }

    /// A negative page size is refused at the edge, not cast into a huge one.
    #[test]
    fn a_negative_annotations_limit_is_refused() {
        let e = with_annotations(3);
        let err = e
            .task_get(&json!({ "ref": 1, "annotations_limit": -1 }))
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::BadRequest);
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
    fn task_list_never_touches_the_token_usage_table() {
        let e = seeded();
        // The same proof as for annotations, for the gate added when token
        // accounting merged — and the stake is higher: `token_usage` gains a
        // row per attributed turn and is never pruned, so a `tasqx list` that
        // scanned it would get slower with every agent turn ever recorded.
        // `seeded()` deliberately writes no measurement, so counting rows here
        // would pass on a broken gate; dropping the table makes the read
        // impossible instead of merely cheap.
        e.conn().execute_batch("DROP TABLE token_usage").unwrap();

        let out = e.task_list(&json!({ "sort": ["short_id"] })).unwrap();
        assert_eq!(out["count"], 2);
    }

    /// A storage fault on the `short_id` read must SURFACE, not silently vanish
    /// a dependent from the `unblocked` list. The row provably exists — its id
    /// came from a JOIN in the same transaction — so the only thing an
    /// error-swallowing read can ever swallow is a genuine fault, shipped as a
    /// wrong list at `ok: true`. Same ruling as `ensure_tag_link`
    /// (storage.rs): `.optional()?`, not `.ok()` — only an absent row is
    /// absence.
    #[test]
    fn compute_unblocked_surfaces_a_storage_fault_instead_of_dropping_the_dependent() {
        let e = seeded();
        // Resolve the blocker through the front door; its response is the
        // healthy-store baseline the fault case is measured against.
        let done = e.task_done(&json!({ "ref": 1 })).unwrap();
        assert_eq!(done["unblocked"], json!([2]));
        let done_id: String = e
            .conn()
            .query_row("SELECT id FROM tasks WHERE short_id = 1", [], |r| r.get(0))
            .unwrap();

        // The injected fault: the one column the guarded read needs goes
        // missing while every other read in the function still works — the
        // dependents JOIN and the blocker COUNT touch t.id and t.status only.
        e.conn()
            .execute_batch("ALTER TABLE tasks RENAME COLUMN short_id TO short_id_gone")
            .unwrap();
        let tx = e.conn().unchecked_transaction().unwrap();
        assert!(
            Engine::compute_unblocked(&tx, &done_id).is_err(),
            "a fault must be an error, not an empty unblocked list"
        );
    }

    #[test]
    fn snapshot_parts_gate_the_optional_side_tables() {
        let e = seeded();

        // Filter-only: tasks + tags + blocked, three statements, and the gated
        // collections come back empty rather than half-populated. Three gates
        // exist (`depends_on`, `annotations`, `tokens`); the two seeded here
        // are the two asserted below.
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

        // Everything: all three gated statements run (hence
        // `SNAPSHOT_QUERY_COUNT`, not `3 + 2`) and the seeded rows arrive.
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
