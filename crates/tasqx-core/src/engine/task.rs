//! Task domain methods for Engine.

use super::*;

/// How many tasks [`Engine::task_list`] returns when the caller names no
/// `limit` at all (D110).
///
/// Matches [`crate::mcp`]'s own `LIST_PAGE` — same number, same reasoning —
/// but this is now the ONE place the default is decided. `mcp.rs`'s transport
/// used to be the only surface bounding an unbounded `task.list`, which meant
/// the CLI and `tasqx api` — a shell one-liner and the ambient socket, not an
/// exotic path — answered the entire store on every unfiltered read: measured
/// at 10,000 tasks, 4.55 MB and roughly 1.1M tokens for one response, with no
/// elision and nothing saying anything had been left out. That is D63's
/// `task.get` failure again, one relation over, on the two surfaces the fix
/// never reached.
pub const DEFAULT_TASK_LIST_LIMIT: u64 = 100;

/// The ceiling an explicitly-named `limit` is clamped to (D110), on D43's
/// precedent ("a user-supplied count is bounded where it is parsed"). Without
/// it `limit: 999999999` asked for the default page's protection to be turned
/// off by naming a number instead of omitting the field — the same escape a
/// clamp closes elsewhere in this file's neighbourhood.
pub const MAX_TASK_LIST_LIMIT: u64 = 10_000;

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

    /// `task.list` when the caller projects `tokens` or sorts by `-tokens`
    /// (#215) — the filter inputs plus one task's own measurements, no edge
    /// list. Same shape as [`Self::REPORT_SUMMARY`], named separately because
    /// the two readers ask for it for different reasons and each constant
    /// here documents its own call site.
    pub(super) const FILTERS_AND_TOKENS: Self = Self {
        depends_on: false,
        annotations: false,
        tokens: true,
    };

    /// `task.list` when the caller wants BOTH `depends_on` and `tokens`
    /// projected (or `-tokens` sort together with a `depends_on` projection)
    /// — the edge list and the measurements, still no annotations.
    pub(super) const FILTERS_DEPENDENCIES_AND_TOKENS: Self = Self {
        depends_on: true,
        annotations: false,
        tokens: true,
    };

    /// The whole task relation — `store.export`, the one reader that emits
    /// every gated part.
    pub(super) const EVERYTHING: Self = Self {
        depends_on: true,
        annotations: true,
        tokens: true,
    };

    /// The aggregation inputs and nothing else — `report.summary`, which sums
    /// the token buckets but never reads an annotation or the edge list. It
    /// loaded [`Self::EVERYTHING`] anyway, so every `tasqx report` scanned the
    /// annotations table end to end and materialised an object per note —
    /// exactly the log-shaped cost this struct was built to keep off
    /// `task.list`, paid by the other reader that also never asked for it.
    pub(super) const REPORT_SUMMARY: Self = Self {
        depends_on: false,
        annotations: false,
        tokens: true,
    };

    /// How many statements a load with these parts runs. Kept next to the
    /// gates themselves so adding a part cannot silently leave the O(1)
    /// statement contract unpinned.
    pub(super) const fn statement_count(self) -> usize {
        Self::BASE_STATEMENTS
            + self.depends_on as usize
            // D138: the `annotations` gate opens TWO statements now — the notes
            // and the checks — because both are the whole-relation reads only
            // `store.export` wants, and splitting the gate would let a future
            // caller ask for one and silently pay for the other.
            + (self.annotations as usize) * 2
            + self.tokens as usize
    }
}

/// The pre-existing whole-relation count stays the authority for the widest
/// variant, so `task_snapshot_statement_count_is_independent_of_task_count`
/// keeps testing the same contract it always did.
const _: () = assert!(SnapshotParts::EVERYTHING.statement_count() == SNAPSHOT_QUERY_COUNT);

/// #141: `wait`/`scheduled` set later than `due` hides a task past its own
/// deadline — every default surface (`@working`, `next`, `pick`) excludes
/// `backlog`, so a task that only leaves `backlog` after its own `due` has
/// passed is invisible until it is already overdue, with no signal anywhere.
/// Called with the EFFECTIVE (post-merge) values, so `task.add` and
/// `task.modify` share one rule regardless of which fields a given call
/// actually named.
///
/// Strict `>` only: `wait`/`scheduled` equal to `due` still releases the task
/// before it is overdue, not after, so that boundary is left alone.
fn check_dates_not_inverted(
    due: Option<&str>,
    scheduled: Option<&str>,
    wait: Option<&str>,
) -> Result<(), ApiError> {
    let Some(due_ts) = due.and_then(parse_ts) else {
        return Ok(());
    };
    for (field, val) in [("wait", wait), ("scheduled", scheduled)] {
        if let Some(v) = val {
            if v.parse::<Timestamp>().is_ok_and(|ts| ts > due_ts) {
                return Err(ApiError::bad_request(format!(
                    "{field} ({v}) is after due ({}) — the task would be hidden \
                     until after its own deadline",
                    due.expect("due_ts came from this same Option")
                )));
            }
        }
    }
    Ok(())
}

/// #142: a `due`-anchored offset `remind` (`-1h`, `+15m`, …) on a task with no
/// `due` has nothing to anchor to and can never fire — worse than not setting
/// one at all, because the caller believes they are covered. An ABSOLUTE
/// remind (a resolved instant) needs no anchor and is unaffected. Called with
/// the EFFECTIVE (post-merge) `remind`/`due`, so `task.modify` catches this
/// whichever field the caller changed — setting an offset remind with no
/// `due`, or clearing `due` out from under an existing offset remind.
fn check_remind_has_anchor(remind: Option<&str>, due: Option<&str>) -> Result<(), ApiError> {
    if due.is_some() {
        return Ok(());
    }
    let Some(r) = remind else { return Ok(()) };
    if matches!(remind::parse_spec(r), Some(remind::Remind::Offset(_))) {
        return Err(ApiError::bad_request(format!(
            "remind:{r} is measured from `due`, and this task has none — \
             set due:, or give remind: an absolute date"
        )));
    }
    Ok(())
}

/// "a" or "an" for a status name in a transition-conflict message
/// (`"cannot {verb} a {status} task"`). #229 item 15: `active` is the one
/// status in the set that starts with a vowel, so every one of these
/// machine-assembled messages read "a active task" the moment the task
/// actually reached this branch in that status — reachable in practice from
/// `task_reopen` (only `done`/`cancelled -> pending`, so any of
/// backlog/pending/active can land here), and defended against everywhere
/// else it could ever become reachable too, since "does the vowel rule hold"
/// is not a property any one call site should have to re-derive.
fn article_for_status(status: Status) -> &'static str {
    if status.as_str().starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

/// D148: write one annotation row's `body`, cut to `cap` BYTES and marked when
/// it did not fit. `None` is no cap.
///
/// It takes the body rather than reading it back off the row for a mechanical
/// reason worth knowing: `util`'s source scan bans `.get("…").and_then(as_…)`
/// anywhere under `engine/`, because that shape cannot tell an absent param
/// from a wrong-typed one. This is a row this function is BUILDING and not a
/// param at all, but a source scan cannot see the difference — and a guard with
/// a per-site exception is a guard nobody trusts. Passing the value in is also
/// simply the honest signature.
///
/// A row that already fits is left exactly as it was — same `body`, and neither
/// new key — so the frozen v1 answer is unchanged for every ordinary note and a
/// reader asks "was this cut?" by presence rather than by comparing a boolean
/// on every row of every response.
///
/// **The cut walks back to a `char` boundary.** `&body[..cap]` panics inside a
/// codepoint, and the bodies this project stores are prose full of em dashes
/// and accents, so an arbitrary byte cap lands mid-character routinely: the
/// naive version turns a READ of a perfectly good task into a 500. Walking down
/// takes at most three steps (UTF-8 is at most four bytes per character) and
/// yields the LONGEST prefix that is both within the cap and valid UTF-8 — a
/// cap of 0, or one shorter than the first character, yields an empty body,
/// which is the degenerate end of the same rule rather than a special case.
///
/// `body_bytes` is the ORIGINAL length, because it is the one fact the reader
/// cannot recover from what arrived, and the renderer needs it to name the
/// exact call that reads the note whole.
fn put_body(row: &mut Map<String, Value>, body: String, cap: Option<u64>) {
    let original = body.len();
    let cap = match cap {
        Some(c) => usize::try_from(c).unwrap_or(usize::MAX),
        None => usize::MAX,
    };
    if original <= cap {
        row.insert("body".to_string(), json!(body));
        return;
    }
    let mut end = cap;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    row.insert("body".to_string(), json!(&body[..end]));
    row.insert("body_bytes".to_string(), json!(original));
    row.insert("body_truncated".to_string(), json!(true));
}

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
            Some(s) => Some(Priority::parse(&s).ok_or_else(|| {
                ApiError::bad_request(format!(
                    "invalid priority: {s} (expected one of: {})",
                    Priority::accepted()
                ))
            })?),
            None => None,
        };
        let now_ts = crate::clock::now();
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
            Some(s) => Some(remind::spec_to_string(&remind::parse_remind(&s, now_ts)?)),
            None => None,
        };
        // D139: a size gauge over fresh tokens. Refused negative at the parse
        // boundary (D45) rather than stored and puzzled over later.
        let budget_tokens = match opt_i64(p, "budget_tokens")? {
            Some(n) if n < 0 => {
                return Err(ApiError::bad_request(
                    "budget_tokens must be a non-negative integer",
                ))
            }
            other => other,
        };

        // #141/#142: validated together, once every date field has resolved,
        // so the message can name the actual instants rather than the raw
        // strings the caller typed.
        check_dates_not_inverted(due.as_deref(), scheduled.as_deref(), wait.as_deref())?;
        check_remind_has_anchor(remind.as_deref(), due.as_deref())?;

        // add -> pending, or backlog if wait/scheduled is in the future. Asking
        // the shared rule what a *backlog* task would be right now answers both
        // halves, and keeps this in step with the spawn path and every read.
        let status = effective_status(
            Status::Backlog,
            wait.as_deref(),
            scheduled.as_deref(),
            now_ts,
        );

        let id = crate::clock::uuid_v7().to_string();
        // The instant the dates above resolved against, as the stored string —
        // `util::now` is `Timestamp::now().to_string()`, so this is the same
        // bytes minus the second clock read the pair used to be.
        let ts = now_ts.to_string();
        let urg = urgency::score_at(priority, due.as_deref(), &ts, now_ts);

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
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)"
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
                budget_tokens,
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
            // #191: the CLI's inline sugar scanner can silently rewrite `title`
            // (eating a `key:value`-shaped interior word) and fabricate `due`
            // out of nothing the caller stated as a date — and `--json` echoed
            // none of it, so the one caller most likely to hit this (an agent
            // writing an English title) could not detect it. Additive per D56.
            "title": title,
            "due": due,
            "tags": tags,
            // tasqx audit #174 (D69 gap): `status` alone tells a caller its
            // task landed in `backlog`, not WHY — an ambiguous `scheduled`
            // value like `in 3 days` is exactly the thing the caller cannot
            // predict the parse of, and it is precisely what flips this bit.
            // Additive per D56, the same move D85 already made for `due`.
            "scheduled": scheduled,
        }))
    }

    // ---- backlog refusal hint -------------------------------------------------

    /// Names the field(s) still holding a future date on a `backlog` task,
    /// and the exact `--clear` command that releases it. Appended to
    /// `task.done`'s and `task.start`'s refusal so "I scheduled it for
    /// Monday and finished it Friday" has a next step in the response
    /// instead of stopping at the transition table with no way out — every
    /// other refusal in this tool names the verb that gets a caller unstuck
    /// (`undo`'s messages, `no project named X (create it with
    /// \`tasqx init X\`)`) and this one was the outlier (tasqx audit #160).
    ///
    /// Only `task.status == Status::Backlog` ever calls this, and a task
    /// resolves to `Backlog` (via `effective_status`) only when at least one
    /// of `wait`/`scheduled` is still ahead of `now` — so at least one field
    /// is always found; an empty `fields` here would mean `effective_status`
    /// and this walk disagree about what "future" means; nothing was found
    /// wrong, so nothing renders rather than a hint that reads as a full
    /// sentence with no field named.
    fn backlog_escape_hint(task: &Task, now: Timestamp) -> String {
        let fields: Vec<(&'static str, &str)> = [
            ("wait", task.wait.as_deref()),
            ("scheduled", task.scheduled.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, v)| v.filter(|d| is_future_at(Some(d), now)).map(|d| (name, d)))
        .collect();
        if fields.is_empty() {
            return String::new();
        }
        let named = fields
            .iter()
            .map(|(name, date)| format!("`{name}` ({date})"))
            .collect::<Vec<_>>()
            .join(" and ");
        let clears = fields
            .iter()
            .map(|(name, _)| format!("--clear {name}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            " — it is deferred by {named}; clear it first with `tasqx modify {} {clears}`",
            task.short_id
        )
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
                    short_id: task.short_id,
                    title: task.title,
                    already_running: true,
                    auto_stopped: Vec::new(),
                }
                .into());
            }
            Status::Pending => {}
            Status::Backlog => {
                return Err(ApiError::conflict(format!(
                    "cannot start a backlog task (only pending -> active){}",
                    Self::backlog_escape_hint(&task, crate::clock::now())
                )));
            }
            other => {
                return Err(ApiError::conflict(format!(
                    "cannot start {} {} task (only pending -> active)",
                    article_for_status(other),
                    other.as_str()
                )));
            }
        }

        let ts = now();

        // D140: the auto-stop below is scoped to the caller's own clock. D6
        // wrote "the currently active one" for one actor and it reads as one
        // for as long as there is one; with two, it reaches across callers and
        // the OTHER party's task leaves `active` mid-work with its remaining
        // time untracked, reported only to the caller that caused it.
        //
        // Two unknowns are not evidence of two parties, so the refusal needs
        // BOTH sides named: a person at a shell (who names no actor) keeps the
        // auto-stop against an agent's clock and against another shell, which
        // is every single-actor store and therefore almost every store.
        if !command.keep {
            if let Some(mine) = command.actor.as_deref() {
                if let Some((held_by, short_id, title)) = self.active_clock_holder()? {
                    if held_by != mine {
                        return Err(ApiError::conflict(format!(
                            "#{short_id} \"{title}\" is active and its clock is held by another \
                             session ({held_by}); stopping it here would leave that session's \
                             work untracked. Pass keep:true to run both clocks deliberately \
                             (D6), or wait for it to stop."
                        )));
                    }
                }
            }
        }

        // D6: single active by default — auto-stop any currently active task.
        let mut auto_stopped: Vec<commands::AutoStopped> = Vec::new();
        if !command.keep {
            let mut actives: Vec<(String, i64, Option<String>, i64, i64)> = Vec::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT id, short_id, active_since, tracked_seconds, rev FROM tasks WHERE status = 'active'",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?;
                for row in rows {
                    actives.push(row?);
                }
            }
            for (aid, a_short_id, active_since, tracked, rev) in actives {
                let elapsed = seconds_between(&active_since, &ts);
                let elapsed_iso = iso_duration(elapsed);
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
                    &json!({ "reason": "auto_stop", "tracked": elapsed_iso }),
                )?;
                // #75: report what the stop loop already knows instead of
                // throwing it away — `task.start`'s own response used to say
                // nothing about the timer it just closed on the caller's
                // behalf.
                auto_stopped.push(commands::AutoStopped {
                    id: aid,
                    short_id: a_short_id,
                    tracked: elapsed_iso,
                });
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
        // D140: the durable half of the actor check. The next `task.start`
        // compares against THIS, so the record has to outlive the call —
        // `tasks` carries no column for it because a task holds a clock only
        // while it is active, and the event log is already the per-occurrence
        // record with its own timestamp (the same argument `Correlation` makes
        // for living here).
        if let Some(actor) = &command.actor {
            start_payload["actor"] = json!(actor);
        }
        insert_event(&tx, Entity::Task, &task.id, "start", &start_payload)?;
        tx.commit()?;

        Ok(commands::TaskStarted {
            id: task.id,
            interval_started: Some(ts),
            short_id: task.short_id,
            title: task.title,
            already_running: false,
            auto_stopped,
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
                "cannot stop {} {} task (only active -> pending)",
                article_for_status(task.status),
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
            interval: iso_duration(elapsed),
            tracked: iso_duration(total),
            short_id: task.short_id,
            title: task.title,
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
        // D138: which criteria this completion proved, and the one citation
        // covering them. Parsed here with the rest, before the lock.
        let checks_passed = opt_str_array(p, "checks_passed")?;
        let evidence = opt_str_nonempty(p, "evidence")?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        match task.status {
            Status::Pending | Status::Active => {}
            Status::Backlog => {
                return Err(ApiError::conflict(format!(
                    "cannot complete a backlog task (only pending|active -> done){}",
                    Self::backlog_escape_hint(&task, crate::clock::now())
                )));
            }
            other => {
                return Err(ApiError::conflict(format!(
                    "cannot complete {} {} task (only pending|active -> done)",
                    article_for_status(other),
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
        // D138. Refused WHOLE rather than half-applied: a completion that
        // marked what it could and reported an error about the rest would
        // leave the caller unable to tell which happened, and this is inside
        // the same transaction as the completion, so the refusal takes the
        // completion with it.
        for id in &checks_passed {
            let changed = tx.execute(
                "UPDATE checks SET state = 'passed', evidence = COALESCE(?1, evidence), \
                 modified = ?2 WHERE id = ?3 AND task_id = ?4",
                params![evidence, ts, id, task.id],
            )?;
            if changed == 0 {
                return Err(ApiError::not_found(
                    format!("task #{} has no check {id}", task.short_id),
                    None,
                ));
            }
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
            // Finding #3 (audit-2026-09): `done` echoed no name of the task it
            // acted on, and the tracked-vs-estimate comparison — the entire
            // payoff of typing `est:2h` at capture time — was left for a
            // separate `show`. Both are already in hand from the row read
            // above, at no extra query.
            "short_id": task.short_id,
            "title": task.title,
            "tracked": iso_duration(total),
            "estimate": task.estimate,
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
        //
        // #211: the hint is a claim about the TASK's measurement state, not
        // about whether THIS call carried params — `usage.is_none()` alone
        // cannot tell "nobody has self-reported yet" from "a self-report
        // already covers this task via an earlier `token.add`, and this
        // particular completion just did not repeat it". The engine already
        // knows the difference (`attribution::compute_attribution` reads the
        // same `token_usage` rows to skip log-parse for exactly this task),
        // so the hint reads it too rather than contradicting a fact the
        // engine has in hand.
        // D138: an unproven completion is COUNTED, not blocked. Refusing here
        // breaks every caller that exists, makes `done` unreachable for
        // criteria nobody can mechanically prove (which is most of them), and
        // is authority a store called BETWEEN turns does not have — a refusal
        // is something the caller routes around, not something that stops the
        // work. D65 settled the shape: make the value change something rather
        // than refuse it. What gives this teeth is `report.outcomes`' own
        // `unproven` metric, where a maintainer sees the pattern instead of an
        // agent hitting one wall at a time.
        //
        // Silent when nothing is open, including when there are no criteria at
        // all: a hint on the good path teaches the reader to stop reading
        // hints, and every completion that exists today must keep answering
        // exactly as it did.
        let still_open: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM checks WHERE task_id = ?1 AND state = 'open'",
            params![task.id],
            |r| r.get(0),
        )?;
        if still_open > 0 {
            out["checks_hint"] = json!(format!(
                "completed with {still_open} acceptance {} still open; nothing was blocked, \
                 and `report.outcomes` counts this as an unproven completion. Pass \
                 checks_passed (with evidence) on completion, or check.set them first.",
                if still_open == 1 { "check" } else { "checks" }
            ));
        }
        // D139: an overrun is named where the caller will see it, and named
        // FIRST — it is the one thing on this response that might change what
        // the reader does next. It stops nothing: the completion above already
        // happened, and a store an agent calls between turns could not have
        // stopped it anyway.
        if let Some(budget) = task.budget_tokens {
            let fresh = self.fresh_tokens(&task.id)?;
            if fresh > budget {
                out["budget_hint"] = json!(format!(
                    "over budget: {fresh} fresh tokens against a budget of {budget} \
                     (fresh = input + output + cache creation; cache reads are not counted). \
                     Nothing was blocked — this is a size signal, and a task that blew its \
                     budget is usually one that was too big to hand to an agent whole."
                ));
            }
        }
        if usage.is_none() {
            let already_self_reported: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM token_usage WHERE task_id = ?1 AND source = ?2)",
                params![task.id, crate::tokens::SOURCE_SELF_REPORT],
                |r| r.get(0),
            )?;
            let recorded: Vec<&str> = [("tool", &named_tool), ("model", &named_model)]
                .into_iter()
                .filter_map(|(k, v)| v.as_ref().map(|_| k))
                .collect();
            out["tokens_hint"] = json!(if already_self_reported {
                "a self-report already covers this task; log-parse attribution is \
                 skipped for it, and a second report on the same measurement would \
                 double-count it — nothing further is needed here"
                    .to_string()
            } else if recorded.is_empty() {
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

        // Anchor on the current due, else the scheduled, else midnight UTC of
        // the completion day. NOT the raw `now_ts`: every other date this tool
        // stores is a clean midnight or clock minute, and anchoring on the
        // unrounded completion instant carried its nanoseconds into the spawned
        // due forever, drifting a little further each cycle (audit #231.2).
        let anchor = template
            .due
            .as_deref()
            .or(template.scheduled.as_deref())
            .and_then(parse_ts)
            .unwrap_or_else(|| datetime::day_start_utc(now_ts));
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

        let new_id = crate::clock::uuid_v7().to_string();
        let new_short = alloc_short_id(tx)?;
        tx.execute(
            &format!(
                "INSERT INTO tasks ({TASK_COLS}) VALUES \
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)"
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
                // A recurring template's budget carries to each occurrence, as
                // `estimate` and `recurrence` do: the next instance is the same
                // work again and is the same size.
                template.budget_tokens,
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

    /// `task.modify`'s `set` map is a bare `object` in the MCP schema — the
    /// one place among the closed vocabularies D34 covers (`status:`, sort
    /// keys, `task.get`'s params) with no schema to consult, so a caller who
    /// misspells a key had to go look. This is the accepted set, named in the
    /// same order the `match` below tests the keys, so `field not modifiable`
    /// can print it instead of leaving the caller to guess a second time.
    /// `modifiable_fields_are_all_accepted` below fails if a name is listed
    /// here but the match arm for it is renamed or removed — it cannot see
    /// the reverse (a new arm added without adding its name here), because
    /// nothing short of re-typing the match can enumerate its arms.
    const MODIFIABLE_FIELDS: &[&str] = &[
        "title",
        "priority",
        "project",
        "due",
        "scheduled",
        "wait",
        "estimate",
        "recurrence",
        "remind",
        "status",
        "budget_tokens",
    ];

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
                // Finding #7 (audit-2026-09): every other refusal in this tool
                // names the mechanism and the next step (`undo`'s conflict is
                // four sentences of it); this one named two numbers. `data`
                // carries the current row so a retry costs one round trip —
                // `set` again with `expected_rev` bumped — not a `show` first.
                return Err(ApiError::new(
                    crate::ErrorCode::Conflict,
                    format!(
                        "expected_rev {exp} but task is at rev {}: re-read with \
                         `tasqx show {} --json` and retry with --expected-rev {}",
                        task.rev, task.short_id, task.rev
                    ),
                    Some(json!({
                        "expected": exp,
                        "current": task.rev,
                        "task": { "short_id": task.short_id, "title": task.title },
                    })),
                ));
            }
        }

        // ONE instant for the whole modify (as `task_add` does): every date
        // field in `set` resolves `today`/`tomorrow` against the same clock —
        // per-field reads could resolve `due:"today"` and `wait:"today"` in
        // one call across midnight — and the stored `modified`/urgency pair
        // below derives from it too.
        let now_ts = crate::clock::now();

        // Whitelist of modifiable columns; recompute urgency if inputs change.
        let mut priority = task.priority;
        let mut due = task.due.clone();
        // #141/#142: the EFFECTIVE post-merge values, so the cross-field
        // checks below see the same task `task_add` would have seen, whether
        // this call named the field or is only affecting it by leaving it
        // alone (a `due` change against an untouched `wait`, or vice versa).
        let mut scheduled = task.scheduled.clone();
        let mut wait = task.wait.clone();
        let mut remind_effective = task.remind.clone();
        let mut assignments: Vec<(&str, Value)> = Vec::new();
        // Set when the only sanctioned lifecycle edit — cancellation — is requested.
        let mut cancelling = false;
        // D23: the project this modify moves the task into, if any (None for an
        // unchanged or cleared project). Validated inside the write tx below.
        let mut project_target: Option<String> = None;
        // D98: a `tracked` correction, resolved to whole seconds. Kept apart
        // from `assignments` because `tracked_seconds` is an INTEGER column —
        // `update_column` below only knows TEXT and NULL, the shape every
        // other whitelisted field stores its value as.
        let mut new_tracked_seconds: Option<i64> = None;

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
                            ApiError::bad_request(format!(
                                "invalid priority: {s} (expected one of: {})",
                                Priority::accepted()
                            ))
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
                    let norm = nullable_when(v, "due", now_ts)?;
                    due = norm.as_str().map(str::to_string);
                    assignments.push(("due", norm));
                }
                "scheduled" => {
                    let norm = nullable_when(v, "scheduled", now_ts)?;
                    scheduled = norm.as_str().map(str::to_string);
                    assignments.push(("scheduled", norm));
                }
                "wait" => {
                    let norm = nullable_when(v, "wait", now_ts)?;
                    wait = norm.as_str().map(str::to_string);
                    assignments.push(("wait", norm));
                }
                "estimate" => assignments.push(("estimate", nullable_duration(v, "estimate")?)),
                "tracked" => {
                    // D98: the audit's "tracked time can never be corrected"
                    // gap — a timer left running overnight banks hours onto a
                    // task with no way back once anything else is logged
                    // (`undo` reaches only the immediately preceding event).
                    // Same duration grammar `estimate` takes, but resolved to
                    // seconds rather than stored as the ISO string itself: the
                    // column is `tracked_seconds`, read back as arithmetic
                    // everywhere (`report.summary`'s `tracked_total`,
                    // `task.stop`'s running total), not as text. `null`
                    // clears it to zero — there is no "never tracked" state
                    // distinct from `PT0S`, so a cleared task reads exactly
                    // like one that was never timed.
                    new_tracked_seconds = Some(if v.is_null() {
                        0
                    } else {
                        let s = v.as_str().ok_or_else(|| {
                            ApiError::bad_request("tracked must be a string or null")
                        })?;
                        let iso = datetime::parse_duration(s)?;
                        duration_secs(&iso).ok_or_else(|| {
                            ApiError::bad_request(format!("tracked duration out of range: {s}"))
                        })?
                    });
                }
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
                        remind_effective = None;
                        assignments.push(("remind", Value::Null));
                    } else {
                        let s = v.as_str().ok_or_else(|| {
                            ApiError::bad_request("remind must be a string or null")
                        })?;
                        let norm = remind::spec_to_string(&remind::parse_remind(s, now_ts)?);
                        remind_effective = Some(norm.clone());
                        assignments.push(("remind", Value::String(norm)));
                    }
                }
                "budget_tokens" => {
                    // D139. Null clears it (D13's rule: `--clear` is the only
                    // way to unset), and a negative one is refused where it is
                    // parsed rather than stored and puzzled over later (D45) —
                    // a negative threshold would make `over` true on a task
                    // nobody has spent anything on.
                    if v.is_null() {
                        assignments.push(("budget_tokens", Value::Null));
                    } else {
                        let n = v.as_i64().filter(|n| *n >= 0).ok_or_else(|| {
                            ApiError::bad_request(
                                "budget_tokens must be a non-negative integer, or null to clear",
                            )
                        })?;
                        assignments.push(("budget_tokens", json!(n)));
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
                                "cannot cancel {} {} task",
                                article_for_status(other),
                                other.as_str()
                            )));
                        }
                    }
                    cancelling = true;
                    assignments.push(("status", Value::String(st.as_str().to_string())));
                }
                other => {
                    return Err(ApiError::bad_request(format!(
                        "field not modifiable: {other} (modifiable: {})",
                        Self::MODIFIABLE_FIELDS.join(", ")
                    )));
                }
            }
        }

        // #141/#142: the same cross-field checks `task_add` runs, over the
        // EFFECTIVE post-merge values — so a modify is caught whichever side
        // of either combination it moves: a `due` set past an untouched
        // `wait`, a `wait` set past an untouched `due`, an offset `remind`
        // added with no `due`, or `due` cleared out from under one.
        check_dates_not_inverted(due.as_deref(), scheduled.as_deref(), wait.as_deref())?;
        check_remind_has_anchor(remind_effective.as_deref(), due.as_deref())?;

        // The operation instant, as the stored string — see `task_add`.
        let ts = now_ts.to_string();
        let new_urg = urgency::score_at(priority, due.as_deref(), &task.created, now_ts);
        let new_rev = task.rev + 1;

        if let Some(name) = &project_target {
            require_live_project(&tx, name)?;
        }
        for (col, val) in &assignments {
            update_column(&tx, &task.id, col, val)?;
        }
        // D98: an explicit correction, applied before the cancel branch below
        // reads a base to add the closing interval onto — so `tracked` and
        // `status:cancelled` in the same call compose (correct, then close)
        // rather than one silently overwriting the other.
        if let Some(secs) = new_tracked_seconds {
            tx.execute(
                "UPDATE tasks SET tracked_seconds=?1 WHERE id=?2",
                params![secs, task.id],
            )?;
        }
        // tasqx audit #174 (D69 gap): `assignments` already holds the RESOLVED
        // form of every field this call named — `due:"friday"` as its ISO
        // instant, `estimate:"90m"` as `PT90M` — because that is what
        // `update_column` just wrote. The caller sent ambiguous NL text and
        // has had no way to learn what it parsed into short of a second
        // `task.get` round trip. Echoing it back is additive to the frozen
        // result (D56) and mirrors `task.add`'s D85 echo of the same class of
        // caller-can't-predict-the-parse value.
        let resolved_set: Value = Value::Object(
            assignments
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        );
        // Cancelling a running task closes its open interval into tracked time
        // and clears active_since, exactly as task.stop/task.done would.
        if cancelling && task.status == Status::Active {
            let elapsed = seconds_between(&task.active_since, &ts);
            let base = new_tracked_seconds.unwrap_or(task.tracked_seconds);
            tx.execute(
                "UPDATE tasks SET active_since=NULL, tracked_seconds=?1 WHERE id=?2",
                params![base + elapsed, task.id],
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

        Ok(json!({ "short_id": task.short_id, "_rev": new_rev, "set": resolved_set }))
    }

    // ---- task.list -----------------------------------------------------------

    /// Load the complete task relation in a fixed number of statements. Bulk
    /// readers need the same relationship data to evaluate filters; grouping
    /// it here prevents each reader from drifting back to point queries.
    pub(super) fn load_task_snapshots(
        &self,
        now: Timestamp,
    ) -> Result<Vec<TaskSnapshot>, ApiError> {
        self.load_task_snapshots_for(SnapshotParts::EVERYTHING, now)
    }

    /// As [`Engine::load_task_snapshots`], but reading only the side tables
    /// `parts` asks for — see [`SnapshotParts`] for why that is not a
    /// micro-optimization.
    pub(super) fn load_task_snapshots_for(
        &self,
        parts: SnapshotParts,
        now: Timestamp,
    ) -> Result<Vec<TaskSnapshot>, ApiError> {
        let (snapshots, statements) = self.load_task_snapshots_counted_for(parts, now)?;
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
        self.load_task_snapshots_counted_for(SnapshotParts::EVERYTHING, crate::clock::now())
    }

    /// Counting variant of [`Engine::load_task_snapshots_for`].
    /// `now` is one instant for the WHOLE load: every row's wait/schedule
    /// release is classified against the same clock, so two rows straddling a
    /// boundary within one list cannot disagree about what time it is.
    pub(super) fn load_task_snapshots_counted_for(
        &self,
        parts: SnapshotParts,
        now: Timestamp,
    ) -> Result<(Vec<TaskSnapshot>, usize), ApiError> {
        let mut statements = 0usize;

        statements += 1;
        let tasks: Vec<Task> = {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT {TASK_COLS} FROM tasks"))?;
            let rows = stmt.query_map([], |r| map_task_row_at(r, now))?;
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
            // The same predicate the per-task readers take — literally, from
            // `unmet_blocker_source` (D145) — so a dependent already
            // `done`/`cancelled` is never blocked here either, no matter what
            // its blocker's status is. What differs is the SELECT: one
            // statement classifying every task at once, because a query per
            // row is the D70 cost this snapshot exists to avoid, which is why
            // this reader appends its own `SELECT DISTINCT` rather than
            // calling `unmet_blockers` per task.
            let mut stmt = self.conn.prepare(&format!(
                "SELECT DISTINCT d.task_id {}",
                Self::unmet_blocker_source()
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
            // Removed (D113) annotations are tombstones, not history a backup
            // should carry forward — the same reason a filtered export trims
            // dependency edges rather than exporting a dangling one.
            // By `id`, not `created`: creation order, since UUIDv7 (D142, and
            // the same reason `export_docs` orders that way).
            let mut stmt = self.conn.prepare(
                "SELECT task_id, id, body, created FROM annotations WHERE removed IS NULL \
                 ORDER BY task_id, id",
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

        // D138, on the annotations gate and for the same reason: only
        // `store.export` emits the whole relation, and a task's criteria are
        // small but grow with the store rather than with the requested page.
        let mut checks: HashMap<String, Vec<Value>> = HashMap::new();
        if parts.annotations {
            statements += 1;
            let mut stmt = self.conn.prepare(
                "SELECT task_id, id, body, state, evidence, position, created, modified \
                 FROM checks ORDER BY task_id, position",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    json!({
                        "id": row.get::<_, String>(1)?,
                        "body": row.get::<_, String>(2)?,
                        "state": row.get::<_, String>(3)?,
                        "evidence": row.get::<_, Option<String>>(4)?,
                        "position": row.get::<_, i64>(5)?,
                        "created": row.get::<_, String>(6)?,
                        "modified": row.get::<_, String>(7)?,
                    }),
                ))
            })?;
            for row in rows {
                let (task_id, check) = row?;
                checks.entry(task_id).or_default().push(check);
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
                // By `id` for the reason the annotations query above gives
                // (D142): `token_usage.id` is UUIDv7 too, so it is creation
                // order without the variable-length-fraction hazard.
                "SELECT task_id, {} FROM token_usage ORDER BY task_id, id",
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
                    checks: checks.remove(id).unwrap_or_default(),
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
        // THE operation's clock: the filter's relative dates, every row's
        // wait/schedule release, and the recomputed urgencies below all
        // resolve against this one instant.
        let now_ts = crate::clock::now();
        let filter = Filter::parse(&filter_str, now_ts).map_err(ApiError::bad_request)?;
        validate_filter_projects(self.conn(), &filter)?;

        // Fetch all rows, then evaluate the filter in Rust: the §12-D8 grammar
        // (or/parens) and instant `due` comparison are evaluated on the loaded
        // task + its tags + its blocked flag (see filter.rs).
        // Filter inputs only: the projection below emits the task columns, its
        // tags and `blocked`, and `TASK_FIELDS` has no key sourced from the
        // dependency or annotation tables. Loading those here meant every
        // `tasqx list` scanned both end to end and discarded the result.
        // Read before the load: projecting `depends_on` (or `tokens`, #215)
        // is the one thing that makes this reader need a side table, so the
        // gate is decided here rather than by loading everything and hoping.
        // `sort` is parsed up front for the same reason: `-tokens` needs the
        // same side table as the `tokens` field, and the gate has to see both
        // asks before the snapshot load, not just the one `fields` names.
        let fields = parse_fields(p)?;
        let sort_keys = parse_sort(p)?;
        let want_deps = crate::engine::fields_want_depends_on(fields.as_ref());
        // Field projection AND sort both need the same measurements, but only
        // the field projection means the row promises to CARRY them — sorting
        // by spend is silent about the numbers themselves, same as sorting by
        // `priority` never puts a priority rank in the row.
        let want_tokens_field = crate::engine::fields_want_tokens(fields.as_ref());
        let want_tokens = want_tokens_field || sort_keys.iter().any(|k| k.key == "tokens");
        let parts = match (want_deps, want_tokens) {
            (false, false) => SnapshotParts::FILTERS_ONLY,
            (true, false) => SnapshotParts::FILTERS_AND_DEPENDENCIES,
            (false, true) => SnapshotParts::FILTERS_AND_TOKENS,
            (true, true) => SnapshotParts::FILTERS_DEPENDENCIES_AND_TOKENS,
        };
        let mut all = self.load_task_snapshots_for(parts, now_ts)?;

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
        // Paired with each surviving snapshot: the saturating sum of its own
        // measurements (#215), computed only when `want_tokens` gated the read
        // above — otherwise the default zero totals, never read back as a
        // number, only ever used to compare below and to render the `tokens`
        // field when projected.
        let mut tasks: Vec<(TaskSnapshot, crate::tokens::TokenTotals)> = Vec::new();
        for mut snapshot in all.drain(..) {
            let t = &mut snapshot.task;
            t.urgency = urgency::score_at(t.priority, t.due.as_deref(), &t.created, now_ts);
            let ctx = MatchCtx {
                status: t.status,
                priority: t.priority,
                project: t.project.as_deref(),
                tags: &snapshot.tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked: snapshot.blocked,
            };
            if filter.matches(&ctx) {
                let totals = if want_tokens {
                    tokens::measurement_totals(&snapshot.tokens)
                } else {
                    crate::tokens::TokenTotals::default()
                };
                tasks.push((snapshot, totals));
            }
        }

        // Sort (default: hottest urgency first). Validated, so an unknown key
        // fails here rather than quietly producing some other order.
        tasks
            .sort_by(|a, b| compare_by(&a.0.task, &b.0.task, a.1.total(), b.1.total(), &sort_keys));

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
        // D110: a caller naming no `limit` gets [`DEFAULT_TASK_LIST_LIMIT`],
        // not the whole store — the same protection `mcp.rs`'s transport gave
        // only its own callers, now uniform across the CLI, `tasqx api` and
        // MCP (whose own default-insertion still runs first and so never
        // observes this fallback, but shares the same number by construction).
        // An explicit `limit` is clamped to [`MAX_TASK_LIST_LIMIT`] rather than
        // honoured as named, so a caller cannot opt back into an unbounded
        // response just by spelling a large number instead of omitting the
        // field.
        let limit = opt_u64(p, "limit")?
            .unwrap_or(DEFAULT_TASK_LIST_LIMIT)
            .min(MAX_TASK_LIST_LIMIT);
        tasks.truncate(limit as usize);
        // Nullable, never absent: a key that comes and goes makes every client
        // branch on presence, and this one would flip on the last page of
        // every walk (D63's rule for `annotations_next_offset`).
        //
        // `limit: 0` is the one width `offset + tasks.len()` cannot describe:
        // it returns to the exact offset the caller just sent, so a pager
        // walking `while next_offset != null: offset = next_offset` would spin
        // forever with `total` still outstanding. Zero rows requested can
        // never advance the walk, so it answers `null` — "nothing more will
        // ever come from this call shape" — the same as reaching the end.
        let next_offset = if limit == 0 {
            Value::Null
        } else {
            match offset + tasks.len() {
                reached if reached < total => json!(reached),
                _ => Value::Null,
            }
        };

        // Field projection (whole row when `fields` absent). Validated above,
        // so an unknown key fails before any of this rather than quietly
        // yielding a narrower row.
        let mut out = Vec::with_capacity(tasks.len());
        for (snapshot, totals) in &tasks {
            let deps: Option<Vec<i64>> = want_deps.then(|| {
                let mut v: Vec<i64> = snapshot
                    .depends_on
                    .iter()
                    .filter_map(|id| short_ids.get(id).copied())
                    .collect();
                v.sort_unstable();
                v
            });
            let token_field: Option<Value> = want_tokens_field.then(|| tokens::bucket_json(totals));
            let full = list_row_json(
                &snapshot.task,
                &snapshot.tags,
                snapshot.blocked,
                deps.as_deref(),
                token_field.as_ref(),
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
            // Whether the STORE (not this filter) has ever held a task — the
            // fact `list`/`next`/`agenda` need to tell "nothing yet" from
            // "nothing matched" apart (#233), which an empty `tasks` array
            // alone cannot say.
            "store_empty": self.store_is_empty()?,
            "tasks": out,
        }))
    }

    // ---- blocked / dependency helpers ---------------------------------------

    /// The one place the "still blocked" rule is spelled (D145): the
    /// FROM/JOIN/WHERE that every blocked reader appends its own SELECT — and
    /// its own `d.task_id = ?1` and ORDER BY, where it wants one task — to.
    /// Three hand-written copies of this clause is what let one of them drift,
    /// so there is now one; a rule that exists in a single place cannot
    /// disagree with itself.
    ///
    /// BOTH sides gate on `status NOT IN (terminal)`. The blocker's side is
    /// D11 (resolved means `done` **or** `cancelled`). The dependent's own
    /// side is D145: a closed task has no unmet blockers regardless of what it
    /// once depended on, so a blocker still being open no longer counts for
    /// it. Missing the `self` gate is the defect itself — a task completed
    /// while its blocker was open kept reporting `blocked: true` forever, and
    /// a fresh `dependency.add` could set the flag on a closed task after the
    /// fact. `self` is the dependent; `t` is the blocker, joined the way it
    /// already was.
    ///
    /// Enum-derived, never caller text — see `Status::sql_in_list`.
    fn unmet_blocker_source() -> String {
        let terminal = Status::sql_in_list(Status::is_terminal);
        format!(
            "FROM dependencies d \
             JOIN tasks t ON t.id = d.depends_on_id \
             JOIN tasks self ON self.id = d.task_id \
             WHERE t.status NOT IN ({terminal}) AND self.status NOT IN ({terminal})"
        )
    }

    /// Whether `task_id` is still blocked — the same predicate
    /// [`Self::unmet_blockers`] lists ([`Self::unmet_blocker_source`]),
    /// answered as a bool for the callers that only need the flag:
    /// `dependency.add` and `dependency.remove`, which ask inside their write
    /// transaction. `EXISTS` stops at the first matching edge instead of
    /// building a JSON object per blocker to answer a yes/no.
    ///
    /// `task.get` does NOT call this: it takes the flag from the single
    /// `unmet_blockers` call it already makes, so its two fields cannot
    /// disagree and the read costs one statement rather than two (D145).
    pub(super) fn is_blocked(&self, task_id: &str) -> Result<bool, ApiError> {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 {} AND d.task_id = ?1)",
            Self::unmet_blocker_source()
        );
        Ok(self
            .conn
            .query_row(&sql, params![task_id], |r| r.get::<_, bool>(0))?)
    }

    /// The dependencies still keeping `task_id` blocked — short_id and title,
    /// sorted — or an empty vec when there are none. THE answer to "what still
    /// blocks this task": [`Self::is_blocked`] is the bool form of the same
    /// predicate, and `task.get` reads `blocked` and `unmet_blockers` off this
    /// one call so the two can never disagree (finding #8, audit-2026-09;
    /// D145).
    pub(super) fn unmet_blockers(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT t.short_id, t.title {} AND d.task_id = ?1 ORDER BY t.short_id",
            Self::unmet_blocker_source()
        ))?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok(json!({ "short_id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)? }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
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

    /// short_ids of the tasks THIS task blocks — the reverse of
    /// `depends_on_short_ids`. Mirrors it exactly (same join, columns
    /// swapped, no status filter): `depends_on` names every edge regardless
    /// of the blocker's status, and the caller who can already see "what
    /// blocks me" is entitled to the same answer for "what do I block",
    /// without the entry disappearing the moment either side closes.
    ///
    /// Exists because nothing on any surface answered "if I finish this,
    /// what starts moving?" before the task actually finished — the only
    /// place the fact appeared was `task.done`'s `unblocked`, after the fact
    /// (tasqx audit #159).
    pub(super) fn blocks_short_ids(&self, task_id: &str) -> Result<Vec<i64>, ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.short_id FROM dependencies d \
             JOIN tasks t ON t.id = d.task_id \
             WHERE d.depends_on_id = ?1 ORDER BY t.short_id",
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
    /// **The ordering is `id` and only `id` (D142).** `created` is TEXT written
    /// by [`crate::util::now`], whose fractional second is variable-length, and
    /// under BINARY collation an older stamp can sort above a newer one — the
    /// defect [`crate::storage::event_id_floor`] documents at length for
    /// `events.ts`. Sorting on it paged a newer annotation out and an older one
    /// into its place. `id` is UUIDv7, minted in creation order, unique, and
    /// already the PRIMARY KEY, so one column settles both the order and the
    /// tie that `created` could never settle on its own.
    ///
    /// `max_body` is D148's per-row cap in BYTES: a body over it is replaced by
    /// its longest prefix that fits and still ends on a `char` boundary, and the
    /// row gains `body_bytes` (the ORIGINAL length) and `body_truncated: true`.
    /// `None` is no cap and no new keys, so the answer a caller who passed
    /// nothing has read since v1 is byte-identical.
    fn annotations_page(
        &self,
        task_id: &str,
        limit: Option<u64>,
        offset: u64,
        max_body: Option<u64>,
    ) -> Result<(Vec<Value>, u64), ApiError> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM annotations WHERE task_id = ?1 AND removed IS NULL",
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
            "SELECT id, body, created FROM annotations WHERE task_id = ?1 AND removed IS NULL \
             ORDER BY id DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![task_id, sql_limit, sql_offset], |r| {
            let mut row = Map::new();
            row.insert("id".to_string(), json!(r.get::<_, String>(0)?));
            put_body(&mut row, r.get::<_, String>(1)?, max_body);
            row.insert("created".to_string(), json!(r.get::<_, String>(2)?));
            Ok(Value::Object(row))
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        v.reverse();
        Ok((v, total))
    }

    /// The tombstones `annotation.remove` left on this task: `{id, removed}`
    /// per scrubbed row, oldest first, and never the text — D113's whole point
    /// is that the text is gone from storage, not merely hidden.
    ///
    /// Its own array rather than rows inside `annotations` (D148).
    /// `annotations`, `annotations_total` and `annotations_next_offset` are the
    /// page arithmetic every client already reads, and a tombstone folded into
    /// them would change what "the third annotation" means on every task that
    /// ever had one removed. Beside them it is additive: a client that never
    /// looks is unaffected.
    fn removed_annotations(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, removed FROM annotations WHERE task_id = ?1 AND removed IS NOT NULL \
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "removed": r.get::<_, String>(1)?,
            }))
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }

    // ---- task.get ------------------------------------------------------------

    /// `task.get` — one task in full. Params: `ref` (short_id or UUID),
    /// `annotations_limit?`, `annotations_offset?`. Adds the five fields the row
    /// itself does not carry — `depends_on`, `blocks`, `annotations`, `tokens`,
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
        self.task_detail_within_snapshot(p)
    }

    /// [`Engine::task_get`]'s body, with the caller owning the snapshot.
    ///
    /// Split out for `task.brief` (D136), which assembles this answer plus two
    /// more sets of reads and so has to hold ONE snapshot across all of them —
    /// `unchecked_transaction` is not reentrant, and the alternative was a
    /// second copy of this shape, which is precisely the drift the one-API rule
    /// exists to stop.
    fn task_detail_within_snapshot(&self, p: &Value) -> Result<Value, ApiError> {
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
        // The reverse edge (D-159): additive per D56/D85, so a v1 client
        // reading this shape unchanged never notices it arrived.
        obj["blocks"] = json!(self.blocks_short_ids(&task.id)?);

        let limit = opt_u64(p, "annotations_limit")?;
        let offset = opt_u64(p, "annotations_offset")?.unwrap_or(0);
        // D148. `None` is no cap: a caller that named none reads the bodies
        // whole, the same way one that names no `annotations_limit` reads the
        // whole history.
        let max_body = opt_u64(p, "max_body_bytes")?;
        let (annotations, total) = self.annotations_page(&task.id, limit, offset, max_body)?;
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
        // D148/D113: what was removed, beside the page rather than in it. A
        // scrubbed note leaves only a hole in the count otherwise, so a reader
        // comparing two reads of the same task watches an annotation vanish
        // with nothing saying it went on purpose. Always present, empty array
        // included — the rule every other always-present key here follows.
        obj["annotations_removed"] = json!(self.removed_annotations(&task.id)?);
        // D138: the criteria, in the order they were written. Always present,
        // empty array included — a caller that has to tell "no criteria" from
        // "this shape does not carry them" cannot, and the difference matters
        // to anything deciding whether a completion was proven.
        obj["checks"] = json!(self.checks_of(&task.id)?);
        obj["tokens"] = json!(self.tokens_of(&task.id)?);
        // D139: the gauge, beside the four buckets rather than instead of
        // them. `fresh_tokens` is always reported — it is a fact about the
        // task whether or not anybody set a threshold to read it against —
        // and `over` is null without one, because there is no verdict to give.
        let fresh = self.fresh_tokens(&task.id)?;
        obj["fresh_tokens"] = json!(fresh);
        obj["over"] = match task.budget_tokens {
            Some(budget) => json!(fresh > budget),
            None => Value::Null,
        };
        // Finding #8 (audit-2026-09): `blocked` said THAT the task cannot be
        // worked, never WHY — `why` computed the urgency arithmetic and never
        // mentioned the one fact that decides whether the number is
        // actionable. `depends_on` already carries every dependency; this
        // narrows to the ones still open, with the title `why` needs to name.
        //
        // One query feeding both fields (D145): `blocked` is exactly "this
        // list is non-empty", so asking a second time would be a second
        // statement and a second chance for the two to answer differently.
        let unmet = self.unmet_blockers(&task.id)?;
        obj["blocked"] = json!(!unmet.is_empty());
        obj["unmet_blockers"] = json!(unmet);

        // #150 / D1: `tasqx why` renders the urgency breakdown, but `--json`
        // was a bare `task.get` result and `task.get` never carried the terms
        // that sum to `urgency` — only the total. `explain` is additive and
        // opt-in (D56: the key is absent unless asked for, so the default
        // `task.get` shape every existing caller reads is unchanged) rather
        // than a standing `task.why` method, since this IS `task.get` plus
        // the D1 formula, not a second read path to the same row.
        if opt_bool(p, "explain")?.unwrap_or(false) {
            let parts = urgency::breakdown(task.priority, task.due.as_deref(), &task.created);
            let total: f64 = parts.iter().map(|(_, v)| v).sum();
            let total = (total * 10.0).round() / 10.0;
            let mut breakdown = Map::new();
            for (name, value) in &parts {
                breakdown.insert((*name).to_string(), json!(value));
            }
            breakdown.insert("total".to_string(), json!(total));
            obj["urgency_breakdown"] = Value::Object(breakdown);
        }
        Ok(obj)
    }

    /// A task's acceptance criteria, in `position` order (D138).
    pub(super) fn checks_of(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, body, state, evidence, position, created, modified \
             FROM checks WHERE task_id = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map(params![task_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "body": r.get::<_, String>(1)?,
                "state": r.get::<_, String>(2)?,
                "evidence": r.get::<_, Option<String>>(3)?,
                "position": r.get::<_, i64>(4)?,
                "created": r.get::<_, String>(5)?,
                "modified": r.get::<_, String>(6)?,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// A task's spend as D139's gauge counts it: `input + output +
    /// cache_creation`, with cache reads excluded.
    ///
    /// Not a blend in the sense D48/D50 forbid. Those rulings govern a COST
    /// report, where one number destroys the split a reader needs because a
    /// cache read costs a fraction of a fresh token. This is a SIZE gauge, and
    /// the same fact decides the definition: a budget dominated by cache reads
    /// measures how often the agent re-read its own context, not how much work
    /// the task was. D103 is the precedent for the boundary — an internal sum
    /// may exist to compare, and what gets reported as cost stays split.
    ///
    /// Saturating, for `report_summary`'s reason: one measurement is bounded,
    /// a sum over arbitrarily many rows is not, and a clamped total is
    /// wrong-but-visible where a wrapped one is negative nonsense.
    fn fresh_tokens(&self, task_id: &str) -> Result<i64, ApiError> {
        let sum: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(input_tokens), 0) + COALESCE(SUM(output_tokens), 0) \
                  + COALESCE(SUM(cache_creation_tokens), 0) \
             FROM token_usage WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )?;
        Ok(sum)
    }

    /// The actor holding the one active clock, with the task it is on — or
    /// `None` when nothing is active or the running timer names no actor.
    ///
    /// Read from the most recent `start` event of the active task rather than
    /// from a column, because that event is where D140 wrote it and because a
    /// column would have to be cleared on every stop, done and cancel — three
    /// more places to forget.
    ///
    /// A task can be active with no `actor` on its start (a shell, or a client
    /// predating D140), and that answers `None`: two unknowns are not evidence
    /// of two parties, and refusing on a guess would break the ordinary case of
    /// a person who started a task and then pointed an agent at the store.
    fn active_clock_holder(&self) -> Result<Option<(String, i64, String)>, ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT e.payload, t.short_id, t.title FROM tasks t \
             JOIN events e ON e.entity_id = t.id AND e.op = 'start' \
             WHERE t.status = 'active' \
             ORDER BY e.rowid DESC LIMIT 1",
        )?;
        let row = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .next();
        let Some(row) = row else { return Ok(None) };
        let (payload, short_id, title) = row?;
        let actor = payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<Value>(p).ok())
            .and_then(|v| payload_field(&v, "actor"));
        Ok(actor.map(|a| (a, short_id, title)))
    }

    // ---- task.brief ----------------------------------------------------------

    /// `task.brief` — D136. Everything needed before starting one task, in one
    /// read. Params: `ref`, `memory_limit?`.
    ///
    /// Three parts and no new data: the task exactly as [`Engine::task_get`]
    /// returns it, the dependency neighbourhood with each prerequisite's
    /// newest annotation, and memory hits under a query DERIVED FROM THE TASK
    /// rather than supplied by the caller.
    ///
    /// The derived query is the part that cannot be composed client-side. The
    /// other two reads can — that is the four-round-trip status quo D136
    /// removes — but a client composing the query is a client guessing, which
    /// is what `.claude/skills/tasqx-workflow/SKILL.md` step 1 asks an agent to
    /// do today ("search memory on the task's key terms") and what fails
    /// silently when the guess is wrong: an empty result is byte-identical to
    /// a store that holds nothing.
    pub fn task_brief(&self, p: &Value) -> Result<Value, ApiError> {
        // One snapshot over all three parts, for `task_get`'s own reason: this
        // answer is assembled from more separate reads than that one, and a
        // write landing between any two of them ships a brief that never
        // existed. DEFERRED and bound to a name, exactly as there.
        let _snapshot = self.conn.unchecked_transaction()?;
        let task = self.resolve_ref(p)?;
        let tags = task_tags(&self.conn, &task.id)?;

        // The task half is `task.get`'s own result, not a reshaping of it: a
        // caller that can read one can read the other, and D49's renderer can
        // be pointed straight at it. `annotations_limit` is deliberately not
        // forwarded — the brief is what you read BEFORE starting, so the
        // task's own history is the part least worth truncating, and the
        // transport's byte budget (D66) is where a too-large answer is cut.
        //
        // `max_body_bytes` IS forwarded (D148), and the difference is the
        // reason: a page limit drops whole notes, while the cap keeps every
        // note and cuts inside the longest one, with a marker naming the call
        // that reads it whole. Threaded by hand because this params object is
        // built here rather than being the caller's — a brief's `ref` may be a
        // UUID and is resolved to a short_id above.
        let mut detail_params = json!({ "ref": task.short_id });
        if let Some(cap) = opt_u64(p, "max_body_bytes")? {
            detail_params["max_body_bytes"] = json!(cap);
        }
        let detail = self.task_detail_within_snapshot(&detail_params)?;

        let neighbourhood = json!({
            "depends_on": self.prerequisites_with_outcome(&task.id)?,
            "blocks": self.dependents_brief(&task.id)?,
        });

        let memory = self.derived_memory(&task, &tags, opt_u64(p, "memory_limit")?)?;

        Ok(json!({
            "task": detail,
            "neighbourhood": neighbourhood,
            "memory": memory,
        }))
    }

    /// Each task this one depends on, with its newest annotation.
    ///
    /// Of everything the neighbourhood could carry, the prerequisite's last
    /// note earns its bytes on evidence: the task that unblocked this one was
    /// finished by somebody who wrote down what they did (the skill's step 4,
    /// and the whole mechanism of `docs/guides/self-improving-agent.md`), and
    /// reading it cost a second `task.get` that the agent usually did not make.
    ///
    /// One statement, not one per dependency: a point query per row here is
    /// the N+1 `SnapshotParts` exists to forbid. The correlated subquery picks
    /// the newest note by `id`, the same key `annotations_page` orders by
    /// (D142), so "newest" means one thing in the store.
    fn prerequisites_with_outcome(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let now = crate::clock::now();
        // `TASK_COLS` and `map_task_row_at`, not a hand-picked `t.status`:
        // status is a read surface and goes through the one derivation every
        // other reader uses (D28, D29). Read raw, a task parked behind a future
        // `wait` reports `pending` here while `list` and `show` call the same
        // row `backlog`, and a row whose stored status is not one of the five
        // prints the placeholder rather than its own text.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {}, \
                    (SELECT a.body FROM annotations a WHERE a.task_id = t.id \
                     ORDER BY a.id DESC LIMIT 1), \
                    (SELECT a.created FROM annotations a WHERE a.task_id = t.id \
                     ORDER BY a.id DESC LIMIT 1) \
             FROM dependencies d JOIN tasks t ON t.id = d.depends_on_id \
             WHERE d.task_id = ?1 ORDER BY t.short_id",
            TASK_COLS
                .split(", ")
                .map(|c| format!("t.{}", c.trim()))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let ncols = TASK_COLS.split(',').count();
        let rows = stmt.query_map(params![task_id], |r| {
            let task = map_task_row_at(r, now)?;
            let body: Option<String> = r.get(ncols)?;
            let created: Option<String> = r.get(ncols + 1)?;
            Ok(json!({
                "short_id": task.short_id,
                "title": task.title,
                "status": task.status_text(),
                // Null rather than omitted: a prerequisite nobody wrote on is
                // still a prerequisite, and a reader must be able to tell
                // "nothing was written" from "this row is shaped differently".
                "annotation": match (body, created) {
                    (Some(body), Some(created)) => json!({ "body": body, "created": created }),
                    _ => Value::Null,
                },
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Each task this one blocks — title and status, and deliberately no
    /// annotation.
    ///
    /// An agent starting work needs what was decided upstream, not the context
    /// of what it is about to release; `task.done`'s `unblocked` already
    /// reports the forward direction at the moment that direction matters.
    fn dependents_brief(&self, task_id: &str) -> Result<Vec<Value>, ApiError> {
        let now = crate::clock::now();
        // Through the same derivation as the prerequisites above, and for the
        // same reason (D28, D29): status is a read surface.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM dependencies d JOIN tasks t ON t.id = d.task_id \
             WHERE d.depends_on_id = ?1 ORDER BY t.short_id",
            TASK_COLS
                .split(", ")
                .map(|c| format!("t.{}", c.trim()))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![task_id], |r| {
            let task = map_task_row_at(r, now)?;
            Ok(json!({
                "short_id": task.short_id,
                "title": task.title,
                "status": task.status_text(),
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// The memory half: `memory.search` run under an expression derived from
    /// the task, scoped to the task's project.
    ///
    /// **Why this goes through `memory.search` in raw mode rather than beside
    /// it.** There is one FTS path and one ranking in this store, and D136 says
    /// so; `raw` is the existing, documented way for a caller to supply the
    /// MATCH expression itself, which is exactly what is happening. So the
    /// hits, their `rank`, the `snippet`, `total`/`has_more` and the `matched`
    /// echo all come back the way every other search answers, and a change to
    /// retrieval reaches the brief without anyone remembering to update it.
    ///
    /// **Why the expression is a disjunction.** A caller's query is a statement
    /// of what they want, so `phrase_escape`'s implicit AND is right for it. A
    /// DERIVED query is a bag of the task's own words, and ANDing five words of
    /// a title answers `count: 0` on a store holding exactly the document the
    /// agent needed — silently, because empty reads the same as a store with
    /// nothing in it. The two are different questions and take different
    /// operators.
    ///
    /// **Scope.** The task's project (D115), reported back so it is never
    /// inferred. There is no fallback to an unscoped search when the scoped one
    /// is empty: that would make the result depend on a branch the caller
    /// cannot see, and D69's rule is that a result says what it answered about.
    /// A caller who wants wider still has `memory.search`, unchanged.
    ///
    /// **Why half the page is held for docs (D147).** bm25 alone decided this
    /// wrong, and decided it silently. The derived expression is the task's own
    /// title words, tags and project leaf; every sibling task in one project
    /// shares exactly that vocabulary, and their annotations are many and long,
    /// so a ruling written ONCE, in its own words, loses every slot to them.
    /// Briefing two tasks in this repo returned ten hits out of a hundred and
    /// twenty-nine, every one a sibling's test-audit note, and none of the
    /// three rulings that actually applied. A ruling that cannot be retrieved
    /// makes the store write-only, so `ceil(limit/2)` slots are RESERVED for
    /// `kind: doc` — reserved, not capped: annotations take the doc slots no
    /// doc claims, and docs take the rest when there are more docs than that.
    /// Docs are listed first, and `reserved_docs`/`docs_total`/
    /// `annotations_total` say what was done (D69) rather than leaving a reader
    /// to infer it from the mix.
    ///
    /// The two searches are one expression run under two `scope`s, not a second
    /// ranking: D136's one-FTS-path rule holds, because each kind is still
    /// ordered by the same `memory.search` under the same `matched`. The
    /// alternative — a kind-aware boost inside one query — is an opaque
    /// reweighting that the `matched` echo cannot explain, and it would change
    /// `memory.search` for every caller to fix a problem only the derived query
    /// has.
    ///
    /// The composition is monotone in `limit`: a smaller limit never yields
    /// more hits of either kind. D66's byte-budget bisection over
    /// `memory_limit` (`fit_brief_to_budget`) depends on that.
    fn derived_memory(
        &self,
        task: &Task,
        tags: &[String],
        limit: Option<u64>,
    ) -> Result<Value, ApiError> {
        let project = task.project.clone();
        let Some(expr) = derive_match_expr(&task.title, tags, project.as_deref()) else {
            // Nothing searchable: a title of function words, no tags, no
            // project. An empty MATCH expression is an FTS5 syntax error, so
            // this is recognised here rather than reported as one — and
            // `matched` is null, which says no expression ran. That is a
            // different answer from "an expression ran and matched nothing",
            // and the two must not print the same.
            //
            // The D147 counters are present and zero here, not omitted: a field
            // that exists on one branch and not on another is a field every
            // reader has to test for, on the branch reached least often.
            return Ok(json!({
                "count": 0,
                "total": 0,
                "has_more": false,
                "hits": [],
                "matched": Value::Null,
                "project": project,
                "reserved_docs": 0,
                "docs_total": 0,
                "annotations_total": 0,
            }));
        };
        let limit = usize::try_from(limit.unwrap_or(crate::engine::MEMORY_SEARCH_LIMIT))
            .unwrap_or(usize::MAX);
        let mut params = json!({ "query": expr.clone(), "raw": true, "limit": limit });
        if let Some(p) = &project {
            params["project"] = json!(p);
            // A doc with no project is knowledge belonging to no ONE project —
            // which is what `memory import` produces — so a brief that hid it
            // would hide the ADRs its reader fed the store. This is not the
            // fallback D136 refuses: that one is a second search whose
            // existence depends on the first being empty and which the caller
            // cannot see. This is ONE scope, applied always, meaning "this
            // project's knowledge, and the knowledge belonging to no project".
            params["include_unscoped"] = json!(true);
        }
        // Each kind asked for a WHOLE page of its own, so either can fill the
        // other's unused slots without a second query to widen it.
        let scoped = |scope: &str| -> Result<(Vec<Value>, i64), ApiError> {
            let mut p = params.clone();
            p["scope"] = json!(scope);
            let out = self.memory_search(&p)?;
            // Through util's typed layer, like every other JSON read in the
            // engine: a raw accessor here would read a `hits` that came back
            // the wrong shape as an empty page, which is the silent-drop this
            // whole method is about.
            let hits = opt_array(&out, "hits")?.cloned().unwrap_or_default();
            let total = opt_i64(&out, "total")?.unwrap_or(0);
            Ok((hits, total))
        };
        let (docs, docs_total) = scoped("docs")?;
        let (annotations, annotations_total) = scoped("annotations")?;

        // Saturating throughout: `limit` is caller input and may be zero or
        // enormous, and a page that underflowed to `usize::MAX` would hand back
        // the whole store.
        let reserved = limit.div_ceil(2);
        let docs_take = docs
            .len()
            .min(reserved.max(limit.saturating_sub(annotations.len())));
        let ann_take = annotations.len().min(limit.saturating_sub(docs_take));
        let hits: Vec<Value> = docs
            .into_iter()
            .take(docs_take)
            .chain(annotations.into_iter().take(ann_take))
            .collect();

        let total = docs_total.saturating_add(annotations_total);
        Ok(json!({
            "count": hits.len(),
            "total": total,
            "has_more": (hits.len() as i64) < total,
            "hits": hits,
            "matched": expr,
            // Additive to `memory.search`'s own shape: which project the hits
            // were scoped to. The search echoes the expression it ran; the
            // brief chose the scope as well, so it echoes that too.
            "project": project,
            "reserved_docs": reserved,
            "docs_total": docs_total,
            "annotations_total": annotations_total,
        }))
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
                    "cannot cancel {} {} task (only backlog|pending|active -> cancelled)",
                    article_for_status(other),
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
                    "cannot reopen {} {} task (only done|cancelled -> pending)",
                    article_for_status(other),
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

/// One string field out of an event payload.
///
/// A function taking the key as a parameter rather than a literal
/// `.get("actor")` chain: D32's guard bans that shape across the engine because
/// it cannot tell an absent value from a wrong-typed one.
fn payload_field(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_string)
}

/// English function words dropped from a derived query (D136).
///
/// **This list cannot hide a document, and that is what makes it safe to be a
/// list.** The derived expression is a disjunction, so a term dropped here
/// still leaves every other term matching — the only documents it removes are
/// ones whose sole connection to the task is a function word, which were never
/// relevant. It is English-only, and the cost of that on a title in another
/// language is noise in the ranking, never a missed hit: the same cost as
/// having no list at all.
///
/// Kept deliberately short. A long stopword list starts making judgements
/// about which content words matter, and bm25 already does that better — a
/// term present in most documents contributes almost nothing to the score.
/// This list exists only so that a title made entirely of them produces no
/// query rather than one matching the whole store.
const QUERY_STOPWORDS: [&str; 24] = [
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "in", "is", "it", "of",
    "on", "or", "that", "the", "this", "to", "with", "we", "our",
];

/// The FTS5 MATCH expression for one task's brief, or `None` when the task
/// carries no searchable word at all.
///
/// Terms are the title's words, its tags, and the project's last dotted
/// segment — `work.tasqx` contributes `tasqx`, because the parent is an
/// organising prefix shared by every sibling project and matches accordingly.
/// Each is lowercased, stripped of surrounding punctuation, deduplicated, and
/// quoted as its own phrase; the phrases are joined with `OR`.
///
/// Quoting is `phrase_escape`'s rule (a `"` inside a term is doubled), because
/// the words are the caller's even though the expression is tasqx's: an
/// unescaped quote ends a phrase early and leaves the whole expression
/// unparseable, which would turn a brief into a `bad_request` on nothing worse
/// than a quoted word in a title.
pub(super) fn derive_match_expr(
    title: &str,
    tags: &[String],
    project: Option<&str>,
) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |word: &str| {
        let w: String = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        // One character is never a useful disjunct and is often punctuation
        // that survived the trim; a stopword is dropped for the reason the
        // list above gives.
        if w.chars().count() < 2 || QUERY_STOPWORDS.contains(&w.as_str()) {
            return;
        }
        if seen.insert(w.clone()) {
            terms.push(w);
        }
    };
    for word in title.split_whitespace() {
        push(word);
    }
    for tag in tags {
        push(tag);
    }
    if let Some(p) = project {
        // The leaf, not the whole dotted path: `work.tasqx` and `work.notes`
        // share `work`, so the prefix is an organising word rather than a
        // subject and matching on it would pull in every sibling.
        push(p.rsplit('.').next().unwrap_or(p));
    }
    if terms.is_empty() {
        return None;
    }
    let quoted: Vec<String> = terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    Some(quoted.join(" OR "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

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
    /// Finding #7 (audit-2026-09): an `expected_rev` conflict named the two
    /// numbers and nothing else — no next step, unlike every other refusal in
    /// this tool. The message must name the retry, and `data` must carry
    /// enough of the current row that a caller can retry in one round trip
    /// rather than two (a re-read then a retry).
    #[test]
    fn expected_rev_conflict_names_the_retry_and_carries_the_current_state() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "first edit" })).unwrap();
        e.task_modify(&json!({ "ref": 1, "set": { "title": "second edit" } }))
            .unwrap(); // bumps rev to 2

        let err = e
            .task_modify(&json!({
                "ref": 1,
                "set": { "title": "third edit" },
                "expected_rev": 1,
            }))
            .unwrap_err();
        assert!(
            err.message.contains("tasqx show") || err.message.contains("--expected-rev"),
            "the message must name the retry step: {}",
            err.message
        );
        let data = err
            .data
            .expect("a conflict on expected_rev must carry data");
        assert_eq!(data["expected"], json!(1));
        assert_eq!(data["current"], json!(2));
        assert_eq!(data["task"]["short_id"], json!(1));
        assert_eq!(data["task"]["title"], json!("second edit"));
    }

    /// Finding #9 (audit-2026-09): a re-`start` on an already-active task
    /// answered exactly the same "Started" line as a genuine start, so a human
    /// re-running a lost command could not tell whether the timer had just
    /// been reset. The idempotent path must SAY it changed nothing.
    #[test]
    fn restarting_an_active_task_says_it_was_already_running() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "restart" })).unwrap();
        let first = e.task_start(&json!({ "ref": 1 })).unwrap();
        assert_eq!(first["already_running"], json!(false));

        let second = e.task_start(&json!({ "ref": 1 })).unwrap();
        assert_eq!(second["already_running"], json!(true));
        assert_eq!(
            second["interval_started"], first["interval_started"],
            "the timer must not have reset"
        );
    }

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

    /// `tasqx why --json` (#150): the human form of `why` prints the urgency
    /// breakdown, but `--json` is a bare `task.get` result, and `task.get`
    /// never carried the terms that add up to `urgency` — only the total.
    /// `explain: true` is the additive, D56-safe opt-in: the key is absent by
    /// default (every existing `task.get` caller sees no change) and present,
    /// summing to the stored `urgency`, only when asked for.
    #[test]
    fn task_get_with_explain_carries_the_urgency_breakdown() {
        let e = seeded();
        let out = e.task_get(&json!({ "ref": 1 })).unwrap();
        assert!(
            out.get("urgency_breakdown").is_none(),
            "urgency_breakdown must stay absent unless explain:true is asked for"
        );

        let out = e.task_get(&json!({ "ref": 1, "explain": true })).unwrap();
        let b = &out["urgency_breakdown"];
        assert!(b["priority"].is_f64() || b["priority"].is_i64());
        assert!(b["due_proximity"].is_f64() || b["due_proximity"].is_i64());
        assert!(b["age"].is_f64() || b["age"].is_i64());
        let sum = b["priority"].as_f64().unwrap()
            + b["due_proximity"].as_f64().unwrap()
            + b["age"].as_f64().unwrap();
        let rounded = (sum * 10.0).round() / 10.0;
        assert_eq!(rounded, out["urgency"].as_f64().unwrap());
        assert_eq!(
            b["total"].as_f64().unwrap(),
            out["urgency"].as_f64().unwrap()
        );
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

    /// Stamp each `note {i}` with a hand-picked `created`, so a test can put
    /// the store in the state a fast machine reaches by itself.
    ///
    /// `crate::util::now` prints a jiff `Timestamp`, whose fractional second is
    /// VARIABLE-LENGTH — trailing zeros trimmed, the fraction gone entirely at
    /// a whole second. The stamps below are what ten annotations a few hundred
    /// microseconds apart genuinely look like; writing them directly is the
    /// only way to reproduce it without racing a clock.
    fn stamp_annotations(e: &Engine, stamps: &[&str]) {
        for (i, ts) in stamps.iter().enumerate() {
            let n = e
                .conn
                .execute(
                    "UPDATE annotations SET created = ?1 WHERE body = ?2",
                    params![ts, format!("note {i}")],
                )
                .unwrap();
            assert_eq!(n, 1, "note {i} must exist exactly once to be stamped");
        }
    }

    /// The page is ordered by `id`, and `created` may not get a vote (D142).
    ///
    /// Ordering by the `created` TEXT put an OLDER annotation above a newer
    /// one: under SQLite's BINARY collation `'Z'` (0x5A) sorts above every
    /// digit and above `'.'`, so `...10.1Z` > `...10.12Z` > `...10.123Z` even
    /// though each is later than the last. A release run caught it — the
    /// aarch64 runner is fast enough that ten annotations land inside one
    /// millisecond, and any one whose last digit is a zero gets trimmed and
    /// jumps the queue. The newest page then returns the wrong rows, and a
    /// reader has no way to notice.
    #[test]
    fn annotations_page_orders_by_id_not_the_variable_length_created_text() {
        let e = with_annotations(4);
        // Chronological, and deliberately DESCENDING in BINARY text order for
        // the first three: 'Z' beats '2', which beats nothing at all.
        stamp_annotations(
            &e,
            &[
                "2026-01-01T00:00:10.1Z",
                "2026-01-01T00:00:10.12Z",
                "2026-01-01T00:00:10.123Z",
                "2026-01-01T00:00:11Z",
            ],
        );

        let page = |limit: u64, offset: u64| -> Vec<String> {
            let out = e
                .task_get(&json!({
                    "ref": 1,
                    "annotations_limit": limit,
                    "annotations_offset": offset,
                }))
                .unwrap();
            out["annotations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["body"].as_str().unwrap().to_string())
                .collect()
        };

        assert_eq!(
            page(2, 0),
            ["note 2", "note 3"],
            "the newest page is the last two WRITTEN, whatever their text stamps sort like"
        );
        assert_eq!(
            page(2, 2),
            ["note 0", "note 1"],
            "and the offset walks back over the other two, with no row shown twice or skipped"
        );
        assert_eq!(
            page(4, 0),
            ["note 0", "note 1", "note 2", "note 3"],
            "the whole history stays in the order it was written"
        );
    }

    /// `task.brief` picks the prerequisite's newest note by the same key
    /// (D142): the two correlated subqueries order by `id` as well.
    ///
    /// This is the surface where the wrong row is most expensive — the
    /// prerequisite's LAST note is "what it concluded", and an older one in its
    /// place reads as a conclusion that was since revised.
    #[test]
    fn brief_takes_the_prerequisites_newest_annotation_by_id() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "blocker" })).unwrap();
        e.task_add(&json!({ "title": "dependent" })).unwrap();
        e.dependency_add(&json!({ "ref": 2, "depends_on": 1 }))
            .unwrap();
        for i in 0..3 {
            e.annotation_add(&json!({ "ref": 1, "body": format!("note {i}") }))
                .unwrap();
        }
        stamp_annotations(
            &e,
            &[
                "2026-01-01T00:00:10.1Z",
                "2026-01-01T00:00:10.12Z",
                "2026-01-01T00:00:10.123Z",
            ],
        );

        let out = e.task_brief(&json!({ "ref": 2 })).unwrap();
        let annotation = &out["neighbourhood"]["depends_on"][0]["annotation"];
        assert_eq!(
            annotation["body"],
            json!("note 2"),
            "the conclusion is the LAST note written, not the one whose text stamp sorts highest"
        );
        assert_eq!(
            annotation["created"],
            json!("2026-01-01T00:00:10.123Z"),
            "and its own stamp comes back with it — body and created must be the same row"
        );
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
    fn every_detail_read_opens_its_snapshot_before_the_first_read() {
        let source = include_str!("task.rs");
        // Assembled rather than written out: `dispatch`'s accepted-key guard
        // splits this same source at every `fn NAME(`, so a marker spelled in
        // full would register here as a second definition of the handler.
        let body_of = |name: &str| -> String {
            let marker = format!("fn {name}(");
            let start = source
                .find(&marker)
                .unwrap_or_else(|| panic!("{name} exists"));
            let rest = &source[start..];
            // BOTH visibilities: the next item after one of these is often
            // `pub fn`, so a terminator of `\n    fn ` alone runs the slice on
            // into every later handler — and one of those opens a write
            // transaction, which this guard would then report as a defect in a
            // function that never had one.
            let end = ["\n    pub fn ", "\n    fn "]
                .iter()
                .filter_map(|t| rest[marker.len()..].find(t))
                .min()
                .map(|offset| marker.len() + offset)
                .unwrap_or(rest.len());
            // Comments out: this function's prose names the constructor it does
            // not use, and a scanner that cannot tell code from a comment would
            // read that as the defect it warns about.
            rest[..end]
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // D136 split the detail read in two: `task_get` opens the snapshot and
        // delegates, `task_brief` opens one and then assembles this answer plus
        // two more sets of reads under it. Both are entry points and both carry
        // the obligation, so both are scanned — a guard that still named only
        // `task_get` would have gone quiet the moment the second one appeared.
        for (entry, first_read) in [
            ("task_get", "self.task_detail_within_snapshot"),
            ("task_brief", "self.resolve_ref(p)"),
        ] {
            let body = body_of(entry);
            let guard = body.find("unchecked_transaction()").unwrap_or_else(|| {
                panic!("{entry} must open a transaction so its reads share one snapshot")
            });
            let read = body
                .find(first_read)
                .unwrap_or_else(|| panic!("{entry} reaches the store via {first_read}"));
            assert!(
                guard < read,
                "{entry}: the snapshot pins at the first read, so the transaction \
                 must be opened before it"
            );
            assert!(
                !body.contains("let _ = self.conn.unchecked_transaction"),
                "{entry}: a `_` binding drops the transaction on the spot, making \
                 the guard a no-op"
            );
        }

        // The shared body relies on ITS CALLER's snapshot and must not open a
        // second: `unchecked_transaction` is not reentrant, so one here is an
        // `internal` error on every `task.brief` rather than a subtle one.
        assert!(
            !body_of("task_detail_within_snapshot").contains("unchecked_transaction"),
            "the shared detail body takes the caller's snapshot, never its own"
        );

        // DEFERRED, never IMMEDIATE: a reader that takes the write lock blocks
        // every writer for its duration, which §2's "concurrent readers never
        // block" forbids.
        for name in ["task_get", "task_brief", "task_detail_within_snapshot"] {
            let body = body_of(name);
            for forbidden in ["begin_mutation", "Immediate"] {
                assert!(
                    !body.contains(forbidden),
                    "{name} is a read and must not take the write lock (`{forbidden}`)"
                );
            }
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

    /// `limit: 0` must not answer `next_offset` equal to the offset just
    /// requested — a naive pager that loops `while next_offset != null` on
    /// that shape never terminates, because the same offset comes back
    /// forever while `total` stays outstanding.
    #[test]
    fn task_list_limit_zero_does_not_loop_a_pager() {
        let e = seeded();
        let out = e.task_list(&json!({ "limit": 0 })).unwrap();
        assert_eq!(out["count"], 0);
        assert!(out["total"].as_u64().unwrap() > 0);
        assert_eq!(
            out["next_offset"],
            Value::Null,
            "limit:0 returned an empty page but pointed the caller right back \
             at the offset they just sent, so the documented pager loop \
             `while next_offset != null: offset = next_offset` never ends"
        );
    }

    /// D110: `task.list` over a caller who names no `limit` at all no longer
    /// answers the whole store — measured at 10,000 tasks, 4.55 MB and
    /// roughly 1.1M tokens for one response, with nothing saying anything was
    /// left out. The default now lives in the engine (D63/D70's placement,
    /// transport-only, is superseded), so `task.list` directly — the path
    /// `api`/the CLI drive — is bounded exactly like the MCP transport
    /// already bounded its own callers.
    #[test]
    fn task_list_with_no_limit_named_gets_the_engine_default_page() {
        let e = Engine::open_in_memory().unwrap();
        for i in 0..(DEFAULT_TASK_LIST_LIMIT + 20) {
            e.task_add(&json!({ "title": format!("task {i}") }))
                .unwrap();
        }
        let out = e.task_list(&json!({})).unwrap();
        assert_eq!(
            out["count"].as_u64().unwrap(),
            DEFAULT_TASK_LIST_LIMIT,
            "an absent `limit` must page at the engine default, not answer every row"
        );
        assert_eq!(
            out["total"].as_u64().unwrap(),
            DEFAULT_TASK_LIST_LIMIT + 20,
            "`total` still counts every matching row, so the caller can tell a page from the store"
        );
        assert_eq!(
            out["next_offset"],
            json!(DEFAULT_TASK_LIST_LIMIT),
            "next_offset must name where the rest of the store starts"
        );
    }

    /// D110's other half: a caller who NAMES a `limit` past
    /// [`MAX_TASK_LIST_LIMIT`] is clamped rather than honoured, on D43's
    /// precedent — otherwise `limit: 999999999` is the escape hatch that
    /// turns the default page's protection back off just by spelling a
    /// number instead of omitting the field.
    #[test]
    fn task_list_clamps_an_absurdly_large_named_limit() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "one task" })).unwrap();
        let out = e.task_list(&json!({ "limit": 999_999_999_u64 })).unwrap();
        assert_eq!(
            out["count"],
            json!(1),
            "one task in the store is still one row back"
        );

        // The clamp itself: a limit above the ceiling never truncates BELOW
        // what the store actually holds when the store is small, so prove it
        // against a page bigger than the ceiling instead.
        for i in 0..(MAX_TASK_LIST_LIMIT + 5) {
            e.task_add(&json!({ "title": format!("t{i}") })).unwrap();
        }
        let out = e.task_list(&json!({ "limit": 999_999_999_u64 })).unwrap();
        assert_eq!(
            out["count"].as_u64().unwrap(),
            MAX_TASK_LIST_LIMIT,
            "a named limit past the ceiling must be clamped to it, not honoured whole"
        );
    }

    /// An explicit `limit` UNDER the default page is still honoured exactly —
    /// D110 only changes the ABSENT case, never second-guesses a caller who
    /// named a smaller number on purpose.
    #[test]
    fn task_list_honours_an_explicit_limit_under_the_default() {
        let e = seeded();
        let out = e.task_list(&json!({ "limit": 1 })).unwrap();
        assert_eq!(out["count"], json!(1));
    }

    /// D109: `project:` in a filter used to answer `No tasks.` at exit 0 for
    /// BOTH an unknown name and a right name typed in the wrong case, the
    /// same silence `status:pendign` answered before D34. A `project:` value
    /// naming nothing in the live projects table is now `not_found`, on the
    /// write side's own exact-match strictness (D23).
    #[test]
    fn task_list_refuses_a_project_filter_naming_no_live_project() {
        let e = Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "FIN-9695" })).unwrap();
        e.task_add(&json!({ "title": "t", "project": "FIN-9695" }))
            .unwrap();

        // Wrong case: the project exists, but not spelled this way.
        let err = e
            .task_list(&json!({ "filter": "project:fin-9695" }))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.message.contains("fin-9695"), "{}", err.message);

        // Genuinely unknown name.
        let err = e
            .task_list(&json!({ "filter": "project:nope-does-not-exist" }))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(
            err.message.contains("nope-does-not-exist"),
            "{}",
            err.message
        );

        // The exact, correctly-cased name still matches, as always.
        let out = e
            .task_list(&json!({ "filter": "project:FIN-9695" }))
            .unwrap();
        assert_eq!(out["count"], json!(1));
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

    /// #215: the only way to reach a task's token spend was `task.get` one
    /// ref at a time — `task.list` had no `tokens` field and no `-tokens`
    /// sort key, so "which tickets cost the most" cost one round-trip per
    /// task on a store where the SQL underneath answers it in one GROUP BY.
    #[test]
    fn task_list_sorts_and_projects_by_token_spend() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "cheap" })).unwrap();
        e.task_add(&json!({ "title": "expensive" })).unwrap();
        // #1 (cheap) gets a small measurement, #2 (expensive) a large one, so
        // `-tokens` must put #2 first.
        e.token_add(&json!({
            "ref": 1, "source": "self-report", "tool": "claude-code",
            "confidence": "medium", "input_tokens": 10, "output_tokens": 5,
        }))
        .unwrap();
        e.token_add(&json!({
            "ref": 2, "source": "self-report", "tool": "claude-code",
            "confidence": "medium", "input_tokens": 1000, "output_tokens": 500,
            "cache_read_tokens": 2000, "cache_creation_tokens": 300,
        }))
        .unwrap();

        let out = e
            .task_list(&json!({ "sort": ["-tokens"], "fields": ["short_id", "tokens"] }))
            .unwrap();
        let tasks = out["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            tasks[0]["short_id"],
            json!(2),
            "the pricier task sorts first"
        );
        assert_eq!(
            tasks[0]["tokens"],
            json!({
                "input_tokens": 1000,
                "output_tokens": 500,
                "cache_read_tokens": 2000,
                "cache_creation_tokens": 300,
            }),
            "the four buckets, never a blended total (D48)"
        );
        assert_eq!(
            tasks[1]["tokens"],
            json!({
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_tokens": 0,
                "cache_creation_tokens": 0,
            })
        );

        // Default listing (no `fields`) still omits `tokens`, exactly like
        // `depends_on` — a projection nobody asked for costs nothing.
        let default_out = e.task_list(&json!({})).unwrap();
        assert!(
            default_out["tasks"][0].get("tokens").is_none(),
            "tokens must stay opt-in on the default row"
        );
    }

    /// #76.1: `fields: []` used to mean "restrict every row to nothing",
    /// returning `{}` per row, while OMITTING `fields` entirely meant no
    /// restriction — an empty list and no list are different requests in
    /// every other params object this engine reads, but here they answered
    /// oppositely. The schema says `fields` "restrict[s] each row to these
    /// fields"; an empty list names none, so it must behave exactly like
    /// omitting the param, not like a maximal restriction.
    #[test]
    fn task_list_empty_fields_is_no_restriction() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "unrestricted" })).unwrap();

        let omitted = e.task_list(&json!({})).unwrap();
        let empty = e.task_list(&json!({ "fields": [] })).unwrap();
        assert_eq!(
            empty["tasks"], omitted["tasks"],
            "`fields: []` must return full rows, identically to omitting `fields`, \
             not `{{}}` per row"
        );
        assert!(
            empty["tasks"][0].get("title").is_some(),
            "an empty `fields` list must not strip every field: {empty}"
        );
    }

    /// A recurring task with neither `due` nor `scheduled` used to anchor its
    /// spawn on the raw completion `Timestamp` — nanosecond precision, drifting
    /// a little further every cycle, and truncated unreadably in the DUE
    /// column (audit #231.2: `2026-09-12T10:43:05.798165338Z`). Every other
    /// date the store produces is a clean midnight or clock minute; the spawn
    /// must land on the same kind of boundary — midnight UTC of the completion
    /// day, advanced by the rule — not on whatever instant `task.done` happened
    /// to run at.
    #[test]
    fn recurrence_with_no_due_spawns_on_a_clean_date_boundary() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "rec", "recurrence": "every 3 days" }))
            .unwrap();
        let done = e.task_done(&json!({ "ref": 1 })).unwrap();
        let due = done["spawned"]["due"]
            .as_str()
            .expect("the spawn must carry a due");
        assert!(
            due.ends_with("T00:00:00Z"),
            "spawned due is not a clean midnight boundary: {due}"
        );
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

    /// The loader classifies EVERY row at the caller's one instant. Unwritable
    /// while `map_task_row` read the wall clock per row: no test could stand
    /// on either side of a wait boundary, let alone both sides of one row.
    #[test]
    fn the_snapshot_loader_classifies_rows_at_the_callers_instant() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "later", "wait": "2999-01-01" }))
            .unwrap();
        let at = |s: &str| s.parse::<Timestamp>().unwrap();

        let held = e
            .load_task_snapshots_for(SnapshotParts::FILTERS_ONLY, at("2998-12-01T00:00:00Z"))
            .unwrap();
        assert_eq!(held[0].task.status, Status::Backlog);

        let released = e
            .load_task_snapshots_for(SnapshotParts::FILTERS_ONLY, at("2999-06-01T00:00:00Z"))
            .unwrap();
        assert_eq!(released[0].task.status, Status::Pending);
    }

    /// `blocked` was derived from the BLOCKER's status alone, so a dependent
    /// that closes while its blocker is still open kept reporting
    /// `blocked: true` forever — on `task.get` and on every `task.list` row.
    /// Reproduces tasqx audit #158.
    #[test]
    fn completing_a_task_whose_blocker_is_still_open_clears_the_blocked_flag() {
        let e = seeded(); // #1 blocker (pending), #2 depends on #1
        let done = e.task_done(&json!({ "ref": 2 })).unwrap();
        assert_eq!(done["status"], json!("done"));

        let got = e.task_get(&json!({ "ref": 2 })).unwrap();
        assert_eq!(
            got["blocked"],
            json!(false),
            "a closed task must never report blocked, no matter its blocker's status"
        );

        // The same fact must hold on the bulk read every `list`/`@blocked`
        // filter is built on, not only on the single-task read.
        let listed = e.task_list(&json!({ "sort": ["short_id"] })).unwrap();
        let row2 = listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["short_id"] == json!(2))
            .unwrap();
        assert_eq!(row2["blocked"], json!(false));
    }

    /// `task.get`'s `depends_on` names what blocks a task; nothing named the
    /// reverse edge — what a task itself blocks — so the question "if I
    /// finish this, what starts moving?" had no answer before completion.
    /// Reproduces tasqx audit #159.
    #[test]
    fn task_get_carries_the_reverse_dependency_edge() {
        let e = seeded(); // #2 depends_on #1
        let blocker = e.task_get(&json!({ "ref": 1 })).unwrap();
        assert_eq!(blocker["blocks"], json!([2]));
        let dependent = e.task_get(&json!({ "ref": 2 })).unwrap();
        assert_eq!(dependent["blocks"], json!([]), "a leaf blocks nothing");
    }

    /// A backlog task refuses `done`/`start` with a message that stops at the
    /// transition table and names no way out — every other refusal in this
    /// tool names the verb that gets a caller unstuck (`undo`'s messages,
    /// `tasqx init {name}` for an unknown project). Reproduces tasqx audit
    /// #160: the message must name the field still holding a future date and
    /// the exact `--clear` command that releases the task.
    #[test]
    fn a_backlog_refusal_names_the_field_and_the_escape() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "call the bank", "scheduled": "2999-12-01" }))
            .unwrap();

        let err = e.task_done(&json!({ "ref": 1 })).unwrap_err();
        assert!(
            err.message.contains("scheduled") && err.message.contains("2999-12-01"),
            "refusal must name the field and its date, got: {}",
            err.message
        );
        assert!(
            err.message.contains("--clear scheduled"),
            "refusal must name the exact escape, got: {}",
            err.message
        );

        let err = e.task_start(&json!({ "ref": 1 })).unwrap_err();
        assert!(
            err.message.contains("--clear scheduled"),
            "task.start's refusal must name the same escape, got: {}",
            err.message
        );
    }

    /// Both deferring fields, both named — clearing only one leaves the task
    /// in `backlog` and a message naming only `wait` would send the caller
    /// back into the same refusal.
    #[test]
    fn a_backlog_refusal_names_both_fields_when_both_defer_it() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({
            "title": "call the bank",
            "wait": "2999-06-01",
            "scheduled": "2999-12-01",
        }))
        .unwrap();

        let err = e.task_done(&json!({ "ref": 1 })).unwrap_err();
        assert!(
            err.message.contains("--clear wait") && err.message.contains("--clear scheduled"),
            "refusal must name both escapes, got: {}",
            err.message
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
            .load_task_snapshots_counted_for(SnapshotParts::FILTERS_ONLY, crate::clock::now())
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

    // ---- #141: wait/scheduled later than due -----------------------------

    /// #141: `wait`/`scheduled` set later than `due` hides a task past its own
    /// deadline — no default surface (`@working`, `next`, `pick`) will ever
    /// show a `backlog` task, so the deadline passes unseen. `task.add` must
    /// refuse the combination, not file it silently.
    #[test]
    fn add_refuses_wait_after_due() {
        let e = Engine::open_in_memory().unwrap();
        let err = e
            .task_add(&json!({
                "title": "conflict1",
                "due": "2026-07-16T00:00:00Z",
                "wait": "2026-12-01T00:00:00Z",
            }))
            .expect_err("wait after due must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(
            err.message.contains("wait") && err.message.contains("due"),
            "the message must name both fields: {}",
            err.message
        );
    }

    /// D34's rule ("a closed vocabulary names its accepted set") already held
    /// for `status:`, sort keys and `task.get`'s params. `task.add`'s
    /// `priority` was one of the three that did not: it named nothing an
    /// agent could retry against, unlike the CLI's own `--priority`, which
    /// already prints `[possible values: H, M, L]`.
    #[test]
    fn task_add_invalid_priority_names_the_accepted_set() {
        let e = Engine::open_in_memory().unwrap();
        let err = e
            .task_add(&json!({ "title": "x", "priority": "URGENT" }))
            .unwrap_err();
        assert!(
            err.message.contains('H') && err.message.contains('M') && err.message.contains('L'),
            "expected the accepted set (H, M, L) in the refusal, got {:?}",
            err.message
        );
    }

    /// The `scheduled` sibling of the same rule.
    #[test]
    fn add_refuses_scheduled_after_due() {
        let e = Engine::open_in_memory().unwrap();
        let err = e
            .task_add(&json!({
                "title": "conflict2",
                "due": "2026-07-16T00:00:00Z",
                "scheduled": "2026-12-01T00:00:00Z",
            }))
            .expect_err("scheduled after due must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
    }

    /// `task.modify` must catch the same inversion when it is reached one
    /// field at a time: a `due` already in the past relative to an existing
    /// `wait` is exactly as hidden-past-deadline as setting both in one call.
    #[test]
    fn modify_refuses_a_due_that_lands_before_an_existing_wait() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "x", "wait": "2026-12-01T00:00:00Z" }))
            .unwrap();
        let err = e
            .task_modify(&json!({ "ref": 1, "set": { "due": "2026-07-16T00:00:00Z" } }))
            .expect_err("a due before the existing wait must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
    }

    /// Equal instants are not an inversion — `wait` releasing exactly at
    /// `due` still shows the task before it is overdue, not after.
    #[test]
    fn wait_equal_to_due_is_allowed() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({
            "title": "boundary",
            "due": "2026-07-16T00:00:00Z",
            "wait": "2026-07-16T00:00:00Z",
        }))
        .expect("wait == due must be allowed");
    }

    // ---- #142: an offset remind with no due ------------------------------

    /// #142: a `due`-anchored offset `remind` on a task with no `due` can
    /// never fire — `add -h` documents the offset as due-anchored, so the
    /// caller believes they are covered and are not. `task.add` must refuse
    /// rather than store a reminder pointing at nothing.
    #[test]
    fn add_refuses_an_offset_remind_with_no_due() {
        let e = Engine::open_in_memory().unwrap();
        let err = e
            .task_add(&json!({ "title": "case1", "remind": "-1h" }))
            .expect_err("an offset remind with no due must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(
            err.message.contains("due"),
            "the message must point at `due`: {}",
            err.message
        );
    }

    /// Same gap, the `task.modify` arm.
    #[test]
    fn task_modify_invalid_priority_names_the_accepted_set() {
        let e = seeded();
        let err = e
            .task_modify(&json!({ "ref": 1, "set": { "priority": "URGENT" } }))
            .unwrap_err();
        assert!(
            err.message.contains('H') && err.message.contains('M') && err.message.contains('L'),
            "expected the accepted set (H, M, L) in the refusal, got {:?}",
            err.message
        );
    }

    /// The documented working case must keep working: an offset remind WITH
    /// a due in the same call is exactly what the offset is for.
    #[test]
    fn add_allows_an_offset_remind_with_a_due() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({
            "title": "remind+due",
            "due": "2026-07-16T00:00:00Z",
            "remind": "-1h",
        }))
        .expect("an offset remind anchored to a same-call due must be allowed");
    }

    /// An ABSOLUTE remind needs no anchor at all — only the offset form is
    /// due-anchored, so a resolved instant must not be swept into the same
    /// refusal.
    #[test]
    fn add_allows_an_absolute_remind_with_no_due() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "case2", "remind": "2026-07-20T09:00:00Z" }))
            .expect("an absolute remind needs no due");
    }

    /// `task.modify` must catch the same hazard reached the other way: adding
    /// an offset remind to a task that already has no `due`.
    #[test]
    fn modify_refuses_setting_an_offset_remind_when_due_is_absent() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "no due" })).unwrap();
        let err = e
            .task_modify(&json!({ "ref": 1, "set": { "remind": "-30m" } }))
            .expect_err("setting an offset remind with no due must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
    }

    /// And the mirror: clearing `due` out from under an existing offset
    /// remind leaves the same dangling symbolic offset.
    #[test]
    fn modify_refuses_clearing_due_while_an_offset_remind_remains() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({
            "title": "remind+due",
            "due": "2026-07-16T00:00:00Z",
            "remind": "-1h",
        }))
        .unwrap();
        let err = e
            .task_modify(&json!({ "ref": 1, "set": { "due": null } }))
            .expect_err("clearing due under a live offset remind must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
    }

    /// Clearing `due` while ALSO clearing (or never having) `remind` is fine
    /// — the refusal is about the combination, not about clearing `due` at
    /// all.
    #[test]
    fn modify_allows_clearing_due_when_remind_is_cleared_too() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({
            "title": "remind+due",
            "due": "2026-07-16T00:00:00Z",
            "remind": "-1h",
        }))
        .unwrap();
        e.task_modify(&json!({ "ref": 1, "set": { "due": null, "remind": null } }))
            .expect("clearing both together must be allowed");
    }

    /// `task.modify`'s `set` map is a bare `object` in the MCP schema — the
    /// one closed vocabulary in this tool with no schema an agent can read
    /// ahead of the call — so the refusal is the caller's only source for the
    /// accepted keys.
    #[test]
    fn task_modify_unmodifiable_field_names_the_modifiable_set() {
        let e = seeded();
        let err = e
            .task_modify(&json!({ "ref": 1, "set": { "nosuchfield": "x" } }))
            .unwrap_err();
        assert!(
            err.message.contains("title") && err.message.contains("priority"),
            "expected the modifiable field list in the refusal, got {:?}",
            err.message
        );
    }

    /// `MODIFIABLE_FIELDS`' doc comment used to claim a test asserted it
    /// against the `match` arms below; no such test existed, so a name could
    /// be dropped from the list (or from the arms) with nothing to catch it.
    /// This is that test, for the direction it CAN check: every name in
    /// `MODIFIABLE_FIELDS` must actually be wired to an arm, i.e. sending it
    /// through `set` must never come back as `field not modifiable`. Removing
    /// a name from `MODIFIABLE_FIELDS` only shrinks what this loop iterates,
    /// so that mutation stays green — it does not exercise the guard. Watched
    /// red by renaming the `"estimate"` match arm below to `"estimate_typo"`
    /// while leaving `"estimate"` in `MODIFIABLE_FIELDS`: the arm no longer
    /// matched, `task_modify` fell through to the wildcard's `field not
    /// modifiable` error, and the assertion failed on it. Reverted and it
    /// passes.
    #[test]
    fn modifiable_fields_are_all_accepted() {
        let sample = |field: &str| -> Value {
            match field {
                "title" => json!("a new title"),
                "priority" => json!("M"),
                "project" => json!("some-project"),
                "due" => json!("today"),
                "scheduled" => json!("today"),
                "wait" => json!("today"),
                "estimate" => json!("1h"),
                "recurrence" => json!("every 1 days"),
                "remind" => json!("-1h"),
                "budget_tokens" => json!(1_000),
                "status" => json!("cancelled"),
                other => panic!("no sample value wired for {other:?} — add one"),
            }
        };
        for field in Engine::MODIFIABLE_FIELDS {
            let e = seeded();
            let err = e
                .task_modify(&json!({ "ref": 1, "set": { (*field): sample(field) } }))
                .err();
            if let Some(err) = err {
                assert!(
                    !err.message.starts_with("field not modifiable"),
                    "field {field:?} is listed in MODIFIABLE_FIELDS but has no match arm: {}",
                    err.message
                );
            }
        }
    }
}
