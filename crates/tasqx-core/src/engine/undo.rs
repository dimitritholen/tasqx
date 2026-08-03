//! `event.revert` — the undo safety net (DESIGN.md §5 example 12, §10).
//!
//! # The three rules this file exists to keep
//!
//! **1. Undo never rewrites or deletes an event.** It appends a *compensating*
//! mutation, so the log reads "X happened, then it was undone" rather than "X
//! never happened". D3 builds sync on this log and `event.list` is the audit
//! trail; a history that quietly loses rows is a history no consumer can trust,
//! and a peer that already replicated the removed row would never learn it was
//! meant to be gone.
//!
//! **2. Only a closed, explicit set of operations is undoable** —
//! [`UNDOABLE_OPS`], four of them. Everything else refuses BY NAME and says what
//! taking it back would actually have required, from [`NOT_UNDOABLE`]. Guessing
//! an inverse is how an undo silently corrupts a store: most of the ops here
//! record what the caller ASKED for, not what changed, and the two differ
//! exactly where it hurts (see `tag.add` and `dependency.add` below).
//!
//! **3. A compound effect is undone atomically or not at all.** Completing a
//! recurring task spawns the next occurrence and `task.done` can record a token
//! measurement in the same transaction; undoing the completion while leaving
//! either behind is a store nobody asked for. `done` is therefore refused
//! outright rather than supported for the simple case — see [`NOT_UNDOABLE`].
//!
//! # Two decisions this file makes, and why
//!
//! **How far back: exactly one step, the newest row in the whole log.** Not a
//! bounded walk, and not scoped to a task. This is not caution, it is what makes
//! the four inverses provably exact: *nothing has happened since*, so the state
//! each inverse writes back is the state that operation found. The moment undo
//! reaches past the newest event — whether by walking to the newest *undoable*
//! one, or by scoping to `#42` and skipping whatever happened elsewhere — later
//! events may have read or overwritten the very fields it is about to restore,
//! and the inverse becomes a guess dressed up as a guarantee. Dependency edges
//! make that concrete: an edge spans two tasks, so "the last event on #42" can
//! be undone by a change recorded against #7.
//!
//! The cost is stated rather than hidden: `undo` twice in a row is a refusal,
//! because the newest event is then the `undo` itself, and `undo` is not in the
//! closed set. There is no redo.
//!
//! **And the second cost, which is easier to miss.** "The newest event" is not
//! the same as "the last command you typed". A verb that changed nothing writes
//! no event — that is the rule every mutation here follows and the one the
//! whole-surface guard in tests/engine.rs enforces — so a command that answered
//! ok while doing nothing is invisible to undo, which then reverses the change
//! BEFORE it. Two verbs reach that state deliberately: `dependency.remove` on an
//! edge that is not there (a documented no-op, relationships.rs) and `task.start`
//! on a task already running (idempotent, task.rs). Type `undep 1 2` against an
//! edge that never existed and then `undo`, and undo takes back the annotation
//! you wrote before it.
//!
//! That is not fixed by teaching undo to guess which command the user meant —
//! it has no session and no way to know. It is fixed by the answer: undo names
//! the operation, the task and what it restored precisely so the reader can see
//! it hit something other than what they were aiming at, while the text it
//! removed is still in the answer and still in the log's payload. The property
//! is pinned by
//! `undo_reaches_past_a_command_that_answered_ok_without_recording_anything`,
//! so it cannot drift into being untrue in either direction unnoticed.
//!
//! **What it covers: task edits.** Projects and memory docs are named things a
//! user re-states in one word (`tasqx use work`, `tasqx memory rm <id>`), and
//! both carry effects the log does not fully record — archiving may also have
//! cleared the store's default project, and a `memory.add` written by
//! `memory.import` replaced a same-source doc whose text is already gone. Task
//! edits are the ones made in bulk and by muscle memory, and the ones where
//! "what exactly did that just change?" is hard to answer afterwards.

use super::*;

/// The operations `event.revert` will undo, and the only ones it ever will.
///
/// Membership is not a matter of taste. Each of these four is *exactly*
/// invertible from its own event payload plus the state the store is in when
/// undo runs, with nothing left to infer:
///
///  * **`stop`** — the payload carries `tracked`, the seconds the closed
///    interval contributed, so the interval can be reopened and those seconds
///    taken back off the total. (See `revert_stop` for the one thing this does
///    NOT reproduce to the byte, and why the number that matters is exact. A
///    code span and not a `[link]`, because that function is private: rustdoc
///    resolves such a link only under `--document-private-items` and 404s it for
///    everyone reading the published page, which `-D rustdoc::private_intra_doc_links`
///    rejects. `storage::open_read_only` records the same trade for the same
///    reason — the span, not an `#[allow]`.)
///  * **`tag.remove`** — D52 makes the removal all-or-nothing behind a
///    pre-check, so every tag the payload names was demonstrably attached before
///    the call and was demonstrably detached by it. Re-attaching exactly those
///    names restores exactly the previous set.
///  * **`dependency.remove`** — the handler writes its event only when a row was
///    really deleted (`if removed > 0`), so the payload's `depends_on` names an
///    edge that existed and is now gone.
///  * **`annotation.add`** — the payload carries the annotation's `id`, and the
///    row it names is the whole of what the call created.
///
/// A fifth entry needs the same proof, in writing, before it joins them: the
/// guard `every_event_op_the_engine_writes_is_either_undoable_or_refused_by_name`
/// (tests/engine.rs) forces every op the engine can write into this list or into
/// [`NOT_UNDOABLE`], so the choice is always made deliberately — but it cannot
/// check that a listed inverse is *correct*.
pub const UNDOABLE_OPS: [&str; 4] = ["stop", "tag.remove", "dependency.remove", "annotation.add"];

/// Every other op the engine writes, paired with the reason `undo` refuses it
/// and — the half that makes a refusal useful — what does take it back.
///
/// This is a closed vocabulary checked against the writers: the guard named in
/// [`UNDOABLE_OPS`] reads every `insert_event` call out of the engine sources
/// and fails if an op is in neither table, or if a table names an op nothing
/// writes any more. A new mutation therefore arrives here the day it is written,
/// instead of falling through to a message that says nothing.
pub const NOT_UNDOABLE: &[(&str, &str)] = &[
    (
        "add",
        "A task cannot be un-created: deleting the row would strand the `add` event that names \
         it and hand back a short_id D4 promises never to recycle. This is also what a completed \
         recurring task's next occurrence is — so if you have just completed one, the newest \
         event is that spawn, and reversing it would delete a task the log still points at. \
         `tasqx cancel <ref>` retires a task without pretending it never existed.",
    ),
    (
        "start",
        "Starting a task auto-stops whatever else was running (D6), and those stops are their \
         own events *behind* the start; reversing only the start would leave the other task \
         stopped with nothing in the log saying why. `tasqx stop <ref>` closes the interval you \
         just opened, and its `tracked` shows what the mistake cost.",
    ),
    (
        "done",
        "Completing a task can also spawn the next occurrence of a recurring rule and record a \
         token measurement, both in the same transaction, and undoing the completion while \
         leaving either behind is a store nobody asked for. `tasqx reopen <ref>` is the \
         sanctioned way back and writes its own event.",
    ),
    (
        "cancel",
        "Cancelling a running task folds its open interval into tracked time, and the event \
         records the status it came from but not where that interval started — so undo could \
         restore the status or the clock, never both. `tasqx reopen <ref>` brings it back as \
         pending.",
    ),
    (
        "reopen",
        "Reopening clears `completed`, and the event does not record the instant it cleared, so \
         putting the task back into `done` would have to invent a completion date. `tasqx done \
         <ref>` completes it again with a real one.",
    ),
    (
        "modify",
        "A `modify` event records the values that were SET, never the ones they replaced, so the \
         log holds nothing to restore. `tasqx show <ref>` and a second `modify` is the way back \
         — and `--expected-rev` makes that second edit refuse if anything moved meanwhile.",
    ),
    (
        "tag.add",
        "Attaching a tag is idempotent, so the event records the tags that were ASKED for, not \
         the ones that were actually attached — undoing it could strip a tag the task already \
         carried. `tasqx untag <ref> <tag>` removes exactly the one you name.",
    ),
    (
        "dependency.add",
        "The edge goes in with INSERT OR IGNORE and the event is written either way, so the log \
         cannot tell an edge this call created from one it found already there. `tasqx undep \
         <ref> <blocker>` removes the edge you name.",
    ),
    (
        "token.add",
        "A token measurement is the only record of what an agent turn cost, and nothing can \
         recompute a self-reported one. `tasqx tokens recompute` re-derives the measurements \
         attribution owns; a self-report is corrected by measuring again.",
    ),
    (
        "tokens.attributed",
        "Same as `token.add`: the measurement is evidence of spend, not an edit to the task. \
         `tasqx tokens recompute` is the one sanctioned way to redo attribution over the \
         windows already stored (D50).",
    ),
    (
        "reminded",
        "A `reminded` event is not a change to the store — it is the dedupe key that stops a \
         reminder firing twice, across restarts and across the daemon and one-shot paths. \
         Removing it would make the notification arrive again, which is the single thing the \
         row exists to prevent. `tasqx modify <ref> --clear remind` stops the reminder instead.",
    ),
    (
        "create",
        "A project cannot be un-created: its NAME is what every task in it stores, so removing \
         it would leave those tasks pointing at nothing. `tasqx archive <name>` takes it out of \
         rotation and leaves the tasks alone.",
    ),
    (
        "use",
        "The event records the project the default moved to and the one it moved from, but \
         putting the old one back is a `use` in its own right — it has to refuse an archived \
         project (D22), which the previous name may since have become. `tasqx use <name>` says \
         which project you mean instead of undo guessing.",
    ),
    (
        "archive",
        "Archiving may also have cleared the store's default project (D22), and there is no \
         `project.unarchive` method for undo to reach for — archiving is deliberately one-way. \
         `store.import` writes the `archived` flag from a document, so restoring a saved export \
         is the way back; that is a data restore, not an undo.",
    ),
    (
        "import",
        "An import writes a whole document in one transaction and records an event per row it \
         touched, so undoing one event would take back a fraction of it and leave the rest. \
         Import the document you meant into a fresh store instead.",
    ),
    (
        "memory.add",
        "A doc written by `memory.import` REPLACES the doc that shared its `source`, and the \
         replaced text is already gone by the time the event is written — so undo can delete \
         the new doc but can never bring the old one back. `tasqx memory rm <id>` removes the \
         doc you name, and says so.",
    ),
    (
        "memory.remove",
        "The event records the doc's id, not its title or its text, so there is nothing in the \
         log to put back. `tasqx memory add` re-files it, or `tasqx memory import` restores it \
         from the file it came from.",
    ),
    (
        "undo",
        "There is no redo. Undoing an undo would put the store back into the state you just \
         chose to leave, and since `undo` only ever reaches the newest event, the pair would \
         toggle one change back and forth forever. Whatever the undo restored can be changed \
         again with the verb that changes it.",
    ),
];

impl Engine {
    // ---- event.revert --------------------------------------------------------

    /// `event.revert` — undo the newest event in the log. Takes no params.
    ///
    /// No `ref`, no `limit`, no `dry_run`: see this module's header for why "the
    /// newest event, or nothing" is the only position from which the inverses
    /// are exact rather than plausible — and, in the same place, why the newest
    /// EVENT can be older than the last command the user typed.
    ///
    /// Answers with what it undid — the operation, the task's short_id and
    /// title, and a per-op `restored` object — because "ok" is precisely the
    /// answer a user cannot check. Refuses with `conflict` when the newest event
    /// is outside [`UNDOABLE_OPS`], and with `not_found` when there is no event
    /// at all.
    pub fn event_revert(&self) -> Result<Value, ApiError> {
        // Everything happens inside one IMMEDIATE transaction: the read of "the
        // newest event" is the whole basis for the inverses being exact, so it
        // has to be taken under the write lock. Reading it first and locking
        // afterwards would let a racing writer append between the two, and undo
        // would then reverse an operation that is no longer the last one.
        let tx = self.begin_mutation()?;
        // events.id is UUIDv7, so ORDER BY id DESC is newest-first — the same
        // ordering `event.list` publishes, and for the same reason.
        let newest: Option<(String, String, String, Option<String>, String)> = tx
            .query_row(
                "SELECT id, entity_id, op, payload, ts FROM events ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((event_id, entity_id, op, payload, event_ts)) = newest else {
            return Err(ApiError::not_found(
                "there is nothing to undo — this store's event log is empty",
                None,
            ));
        };

        if !UNDOABLE_OPS.contains(&op.as_str()) {
            return Err(not_undoable(&op, &event_ts));
        }

        // A payload that will not parse cannot be inverted from. `Value::Null`
        // rather than an error here so the per-op arms below each say what they
        // needed and did not find, naming the key — a bare "malformed payload"
        // tells the operator nothing about which undo failed or why.
        let payload: Value = payload
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(Value::Null);

        // Every op in the closed set is written against a task row (the
        // constants above are checked against the real writers, entity included,
        // by the guard in tests/engine.rs), so this resolve cannot be reached
        // with a project or doc id.
        let task = self.task_by_id_on(&tx, &entity_id)?;

        let restored = match op.as_str() {
            "stop" => revert_stop(&tx, &task, &payload, &event_ts)?,
            "tag.remove" => revert_tag_remove(&tx, &task, &payload)?,
            "dependency.remove" => revert_dependency_remove(&tx, &task, &payload)?,
            "annotation.add" => revert_annotation_add(&tx, &task, &payload)?,
            // Unreachable while this match covers UNDOABLE_OPS, and an error
            // rather than a fallthrough precisely so that if the two ever drift
            // the store is left alone instead of being told the undo happened.
            other => {
                return Err(ApiError::internal(format!(
                    "`{other}` is listed in UNDOABLE_OPS but has no inverse here — the list and \
                     the match have drifted, and nothing was undone"
                )))
            }
        };

        // Rule 1, in one call: the compensating mutation APPENDS. The original
        // event is untouched, so `tasqx history` reads "the tag came off, then
        // that was undone" — which is what happened — and any consumer that
        // already replicated the original row stays consistent with this one.
        //
        // `rev` moves with it because a client's `expected_rev` has to mean "the
        // task really changed", and this changed it.
        let ts = now();
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "undo",
            &json!({
                "reverted": event_id,
                "reverted_op": op,
                "restored": restored,
            }),
        )?;
        tx.commit()?;

        Ok(json!({
            // Named, not "ok": the operator has to be able to see that undo hit
            // the thing they meant. The title is here for the same reason — a
            // short_id alone is not something anyone recognizes at a glance.
            "reverted": { "event": event_id, "op": op, "ts": event_ts },
            "short_id": task.short_id,
            "title": task.title,
            "restored": restored,
            "_rev": task.rev + 1,
        }))
    }
}

/// The refusal for an op outside [`UNDOABLE_OPS`], carrying the op's own reason
/// and the set that IS undoable.
///
/// `conflict` (exit 5) and not `bad_request`: the caller's request was
/// well-formed — `tasqx undo` takes no arguments and there is nothing they could
/// have spelled differently. What refuses it is the state the store happens to
/// be in, which is exactly what exit 5 means here.
fn not_undoable(op: &str, event_ts: &str) -> ApiError {
    let reason = NOT_UNDOABLE
        .iter()
        .find(|(known, _)| *known == op)
        .map(|(_, why)| (*why).to_string())
        // Reachable on a store written by a NEWER tasqx (D12: a newer export
        // must stay readable), which can carry ops this build has never seen.
        // Inventing an inverse for one of those is the corruption this whole
        // closed set exists to prevent, so an unknown op refuses and says so.
        .unwrap_or_else(|| {
            format!(
                "`{op}` is not an operation this build knows how to reverse at all — a store \
                 written by a newer tasqx can carry ops this one has never seen, and guessing an \
                 inverse for one is how an undo corrupts a store."
            )
        });
    ApiError::conflict(format!(
        "the newest thing in this store's log is `{op}` (at {event_ts}), and `undo` will not \
         reverse it. {reason} `undo` reverses exactly these operations and nothing else: {}.",
        UNDOABLE_OPS.join(", ")
    ))
}

/// Reopen the interval a `task.stop` closed: back to `active`, with the seconds
/// that stop folded into `tracked_seconds` taken off again.
///
/// **What this restores exactly, and what it does not.** `tracked_seconds`
/// returns to its pre-stop value to the second — that is the number every report
/// and every `--tracked` metric reads, and it is exact because the payload
/// carries the very figure that was added. The reconstructed `active_since` is
/// derived as *event instant minus tracked*, which can sit up to a second later
/// than the original: `task.stop` computes its elapsed seconds against its own
/// `now()` and `insert_event` takes a second reading for the row's `ts`, and
/// `seconds_between` truncates to whole seconds either way. The log does not
/// record the interval's start, so this is the closest the store can honestly
/// get, and the sub-second drift lands on a field nothing sums.
///
/// Refuses when the task is not `pending`. Nothing can have happened since (this
/// is the newest event), so a task in any other state means the store was
/// changed underneath the log — by an external SQLite writer, a restore, a
/// half-migrated file — and reopening an interval on it would be inventing
/// tracked time on a row this event says nothing about.
fn revert_stop(
    tx: &Transaction,
    task: &Task,
    payload: &Value,
    event_ts: &str,
) -> Result<Value, ApiError> {
    let tracked = payload
        .get("tracked")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ApiError::conflict(
                "this `stop` event carries no `tracked` duration, so the log does not say how \
                 much time the interval contributed and undo cannot take it back off the total. \
                 Nothing was changed; `tasqx start <ref>` opens a fresh interval.",
            )
        })?;
    let elapsed = duration_secs(tracked).ok_or_else(|| {
        ApiError::conflict(format!(
            "this `stop` event records `tracked` as {tracked:?}, which is not a duration this \
             build can read, so undo cannot say how many seconds to take back. Nothing was \
             changed."
        ))
    })?;

    if task.status != Status::Pending {
        return Err(ApiError::conflict(format!(
            "#{} is {} — `undo` reopens the interval `stop` closed, which is only meaningful on \
             the pending task that stop left behind, and something has changed it since. Nothing \
             was undone.",
            task.short_id,
            task.status.as_str()
        )));
    }
    if task.tracked_seconds < elapsed {
        return Err(ApiError::conflict(format!(
            "#{} has {}s of tracked time but this `stop` contributed {elapsed}s, so taking the \
             interval back would leave a negative total — the row has been edited outside the \
             log. Nothing was undone.",
            task.short_id, task.tracked_seconds
        )));
    }

    let started = parse_ts(event_ts)
        .and_then(|t| Timestamp::from_second(t.as_second() - elapsed).ok())
        .ok_or_else(|| {
            ApiError::internal(format!(
                "cannot rebuild the interval start from event timestamp {event_ts:?} minus \
                 {elapsed}s"
            ))
        })?
        .to_string();

    tx.execute(
        "UPDATE tasks SET status='active', active_since=?1, tracked_seconds=?2 WHERE id=?3",
        params![started, task.tracked_seconds - elapsed, task.id],
    )?;
    Ok(json!({
        "status": "active",
        "tracked": tracked,
        "interval_started": started,
    }))
}

/// Re-attach the tags a `tag.remove` detached.
///
/// Exact because of D52's all-or-nothing pre-check: `tag.remove` verifies every
/// name is attached BEFORE it deletes any of them, so a `tag.remove` event in
/// the log is proof that each tag it lists was on the task and came off. There
/// is no "was it already there?" ambiguity to guess at — the ambiguity that
/// keeps `tag.add` out of the closed set.
///
/// Refuses if any of them is attached again. Nothing can have happened since, so
/// that means an external writer put it back, and re-attaching silently would
/// report a restoration that did not happen.
fn revert_tag_remove(tx: &Transaction, task: &Task, payload: &Value) -> Result<Value, ApiError> {
    let tags: Vec<String> = payload
        .get("tags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if tags.is_empty() {
        return Err(ApiError::conflict(
            "this `tag.remove` event names no tags, so there is nothing for undo to put back. \
             Nothing was changed; `tasqx tag <ref> <tag>` attaches one by name.",
        ));
    }

    let present = task_tags(tx, &task.id)?;
    let back: Vec<&String> = tags.iter().filter(|t| present.contains(t)).collect();
    if !back.is_empty() {
        return Err(ApiError::conflict(format!(
            "#{} already carries {} — the removal this event records has been put back by \
             something other than the log, so undo would report a restoration that did not \
             happen. Nothing was changed.",
            task.short_id,
            back.iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    for tag in &tags {
        ensure_tag_link(tx, &task.id, tag)?;
    }
    Ok(json!({ "tags": tags }))
}

/// Re-insert the dependency edge a `dependency.remove` deleted.
///
/// The payload names the blocker by UUID, which is what makes this replayable at
/// all — a short_id is a display handle an import may re-mint.
///
/// Refuses if the edge is back already, and refuses if putting it back would
/// close a cycle. The second check is `dependency.add`'s own, run again, and it
/// is here for the reason the other three inverses in this file already act on:
/// each of them re-verifies its effect is still undone, because an external
/// SQLite writer, a restore or a half-migrated file may have changed the store
/// since the event was written. This is the only inverse that writes a GRAPH
/// edge, so that same actor can have inserted the reverse edge while this one was
/// gone — and re-inserting without checking mints the mutual cycle
/// `dependency.add` refuses with `conflict`, leaving both tasks `blocked` with no
/// verb that unblocks them. D16 records exactly that corruption shipping once,
/// through `store.import` bypassing this same guard.
fn revert_dependency_remove(
    tx: &Transaction,
    task: &Task,
    payload: &Value,
) -> Result<Value, ApiError> {
    let dep_id = payload
        .get("depends_on")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ApiError::conflict(
                "this `dependency.remove` event names no `depends_on`, so the log does not say \
                 which edge came off. Nothing was changed; `tasqx dep <ref> <blocker>` adds the \
                 edge you name.",
            )
        })?;

    // The FOREIGN KEY would refuse a missing blocker anyway, but as a bare
    // constraint violation naming a column. Reading the row first means the
    // refusal can say which task is gone and what that implies.
    let blocker: Option<i64> = tx
        .query_row(
            "SELECT short_id FROM tasks WHERE id = ?1",
            params![dep_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(blocker_short) = blocker else {
        return Err(ApiError::not_found(
            format!(
                "the blocker this edge pointed at ({dep_id}) is no longer in this store, so the \
                 dependency cannot be put back. Nothing was changed."
            ),
            Some(json!({ "depends_on": dep_id })),
        ));
    };

    // The same read `dependency.add` makes before the same INSERT, under the
    // same held write lock: the edge task -> blocker closes a cycle exactly when
    // the blocker already reaches the task. Skipping it on the strength of
    // "nothing has happened since" would be trusting a premise the three
    // siblings above each refuse to trust, on the one inverse where being wrong
    // costs a graph no verb can repair.
    if reaches(tx, dep_id, &task.id)? {
        return Err(ApiError::conflict(format!(
            "putting this edge back would make #{} and #{blocker_short} block each other: \
             #{blocker_short} now depends on #{}, which it cannot have done when the edge came \
             off, so something other than the log has changed the graph. Restoring it would \
             leave both tasks blocked with no verb that unblocks them. Nothing was changed; \
             `tasqx undep {blocker_short} {}` breaks the other side first.",
            task.short_id, task.short_id, task.short_id
        )));
    }

    let inserted = tx.execute(
        "INSERT OR IGNORE INTO dependencies (task_id, depends_on_id) VALUES (?1, ?2)",
        params![task.id, dep_id],
    )?;
    if inserted == 0 {
        return Err(ApiError::conflict(format!(
            "#{} already depends on #{blocker_short} — the edge this event removed is back \
             already, put there by something other than the log. Nothing was changed.",
            task.short_id
        )));
    }
    Ok(json!({ "depends_on": blocker_short }))
}

/// Delete the annotation an `annotation.add` created.
///
/// This is the one inverse that removes user text, and it is still exact: the
/// row it deletes is the whole of what the undone call created, named by the
/// `id` in that call's own event, and the note's body is returned so the answer
/// shows what went. The FTS5 delete trigger keeps `annotations_fts` in step, so
/// `tasqx memory search` cannot go on finding a note that is no longer there.
fn revert_annotation_add(
    tx: &Transaction,
    task: &Task,
    payload: &Value,
) -> Result<Value, ApiError> {
    let id = payload.get("id").and_then(Value::as_str).ok_or_else(|| {
        ApiError::conflict(
            "this `annotation.add` event carries no annotation `id`, so undo cannot tell which \
             note it created. Nothing was changed.",
        )
    })?;

    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM annotations WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Err(ApiError::conflict(format!(
            "the note #{} recorded under id {id} is already gone, so undo has nothing to remove. \
             Nothing was changed.",
            task.short_id
        )));
    };

    tx.execute("DELETE FROM annotations WHERE id = ?1", params![id])?;
    Ok(json!({ "annotation": body }))
}
