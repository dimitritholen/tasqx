//! Relationships domain methods for Engine.

use super::*;

impl Engine {
    // ---- tag.add -------------------------------------------------------------

    /// `tag.add` — attach one or more tags to a task. Params: `ref`, `tags` (a
    /// non-empty array). Returns the task's FULL tag set, re-read inside the
    /// transaction, so the caller never has to guess what a partially-duplicate
    /// add left behind.
    pub fn tag_add(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let tags = opt_str_array(p, "tags")?;
        if tags.is_empty() {
            return Err(ApiError::bad_request(
                "tag.add requires a non-empty `tags` array",
            ));
        }

        let ts = now();
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        for tag in &tags {
            ensure_tag_link(&tx, &task.id, tag)?;
        }
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "tag.add",
            &json!({ "tags": tags }),
        )?;

        // Re-read the full tag set inside the transaction for the response.
        // `task_tags` and not a second copy of its SELECT: this method and
        // `tag_remove` both answer with "the task's tags afterwards", and two
        // copies of one query is how the pair would come to disagree about
        // ordering the day either is touched.
        let all = task_tags(&tx, &task.id)?;
        tx.commit()?;

        Ok(json!({ "short_id": task.short_id, "tags": all }))
    }

    // ---- tag.remove ----------------------------------------------------------

    /// `tag.remove` — detach one or more tags from a task. Params: `ref`, `tags`
    /// (a non-empty array). Mirrors [`Engine::tag_add`]: same params, same
    /// transaction, one event, and a response carrying the task's FULL tag set
    /// re-read inside that transaction.
    ///
    /// **Removing a tag the task does not carry is `not_found`, and nothing is
    /// written.** This is the one place `tag.remove` deliberately does NOT
    /// mirror its sibling `dependency.remove`, which treats an absent edge as a
    /// no-op answering ok, so the difference is worth stating rather than
    /// leaving to be discovered.
    ///
    /// The two cases are not alike. A dependency edge is named by two refs that
    /// both had to resolve, so "the edge is not there" is already visible in the
    /// response — `depends_on` comes back and the caller can see the target is
    /// absent from it. A tag is a bare string the caller typed. `tasqx untag 42
    /// blockign` has exactly one plausible cause, and answering ok with a tag
    /// set that still contains `blocking` is D33's unfalsifiable write: the
    /// caller stated an intent, nothing happened, and the answer was
    /// indistinguishable from success. So the refusal names the tags the task
    /// does not have AND the tags it does, which is the whole fix — the typo is
    /// one glance from the correction.
    ///
    /// **All-or-nothing.** `tags: ["api", "blockign"]` removes neither. The
    /// check runs inside the write transaction before the first DELETE, so a
    /// partly-applied removal is unreachable rather than merely unlikely, and
    /// the caller never has to ask which half landed.
    ///
    /// The `tags` ROW is left behind when the last task loses a tag. Nothing
    /// reads the table except through the `task_tags` join (the completion
    /// vocabulary is derived from `task.list` rows, D50 — there is no
    /// `tag.list`), so an unreferenced name is invisible, and deleting it would
    /// mean deciding what happens to a name two concurrent transactions are
    /// racing to reuse.
    pub fn tag_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let tags = opt_str_array(p, "tags")?;
        if tags.is_empty() {
            return Err(ApiError::bad_request(
                "tag.remove requires a non-empty `tags` array",
            ));
        }

        let ts = now();
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;

        // Read the current set inside the IMMEDIATE tx — the write lock is held,
        // so a concurrent `tag.add` serializes against us and this can neither
        // refuse a tag that has just arrived nor delete one that has just gone.
        let before = task_tags(&tx, &task.id)?;
        let missing: Vec<String> = tags
            .iter()
            .filter(|t| !before.contains(t))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let quoted = |names: &[String]| {
                names
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let has = match before.is_empty() {
                true => "the task has no tags".to_string(),
                false => format!("it is tagged {}", quoted(&before)),
            };
            return Err(ApiError::not_found(
                format!(
                    "#{} does not have the tag {} — {has}. Check the spelling in `tags`, \
                     or drop the entry; nothing was removed.",
                    task.short_id,
                    quoted(&missing),
                ),
                Some(json!({ "missing": missing, "tags": before })),
            ));
        }

        for tag in &tags {
            tx.execute(
                "DELETE FROM task_tags WHERE task_id = ?1 \
                 AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
                params![task.id, tag],
            )?;
        }
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "tag.remove",
            &json!({ "tags": tags }),
        )?;

        let all = task_tags(&tx, &task.id)?;
        tx.commit()?;

        // `removed` as well as `tags`: the remaining set alone cannot tell a
        // caller which of its entries this call took away, and D39 asks that a
        // computed fact be visible on a surface rather than inferred.
        Ok(json!({ "short_id": task.short_id, "tags": all, "removed": tags }))
    }

    // ---- annotation.add ------------------------------------------------------

    // ---- check.add / check.set / check.remove (D138) -------------------------

    /// `check.add` — append one acceptance criterion. Params: `ref`, `body`.
    ///
    /// The criterion is prose the caller wrote; what makes it a check rather
    /// than an annotation is that it carries a STATE, so something can ask at
    /// completion time whether it was met. It starts `open`.
    pub fn check_add(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let body = req_str(p, "body")?;
        let id = crate::clock::uuid_v7().to_string();
        let ts = now();
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        // Appended at the end, computed inside the write so two concurrent
        // adds cannot land on one position.
        let position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM checks WHERE task_id = ?1",
            params![task.id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO checks (id, task_id, body, state, evidence, position, created, modified) \
             VALUES (?1, ?2, ?3, 'open', NULL, ?4, ?5, ?5)",
            params![id, task.id, body, position, ts],
        )?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "check.add",
            &json!({ "id": id, "body": body }),
        )?;
        tx.commit()?;
        Ok(json!({
            "short_id": task.short_id,
            "check": {
                "id": id, "body": body, "state": "open",
                "evidence": Value::Null, "position": position,
                "created": ts, "modified": ts,
            },
        }))
    }

    /// `check.set` — mark one criterion. Params: `ref`, `check_id`, `state`,
    /// `evidence?`.
    ///
    /// `evidence` is the citation and tasqx stores it verbatim. It is optional
    /// because a criterion can be met by something nobody can quote — a human
    /// looked and it was right — and refusing that would push the caller to
    /// invent a proof, which is worse than an unproven `passed`.
    pub fn check_set(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let named = check_ref(p)?;
        let state = req_str(p, "state")?;
        if !CHECK_STATES.contains(&state.as_str()) {
            return Err(ApiError::bad_request(format!(
                "state must be {} (got {state:?})",
                CHECK_STATES.join("|")
            )));
        }
        let evidence = opt_str_nonempty(p, "evidence")?;
        let ts = now();
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        let check_id = check_id_on(&tx, &task, named)?;
        tx.execute(
            "UPDATE checks SET state = ?1, evidence = COALESCE(?2, evidence), modified = ?3 \
             WHERE id = ?4",
            params![state, evidence, ts, check_id],
        )?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "check.set",
            &json!({ "id": check_id, "state": state }),
        )?;
        tx.commit()?;
        Ok(json!({ "short_id": task.short_id, "check_id": check_id, "state": state }))
    }

    /// `check.remove` — drop one criterion. Params: `ref`, `check_id`.
    ///
    /// A criterion that turned out to be wrong is deleted rather than marked,
    /// because the states say whether the work met it and none of them says
    /// "this was never the right thing to ask". Unlike `annotation.remove`
    /// (D113) this scrubs no prose anybody relied on: a check's body is a
    /// criterion, not a record of what happened, and the event log keeps the
    /// text it was added with.
    pub fn check_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let named = check_ref(p)?;
        let ts = now();
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        let check_id = check_id_on(&tx, &task, named)?;
        tx.execute("DELETE FROM checks WHERE id = ?1", params![check_id])?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "check.remove",
            &json!({ "id": check_id }),
        )?;
        tx.commit()?;
        Ok(json!({ "short_id": task.short_id, "check_id": check_id, "removed": true }))
    }

    /// `annotation.add` — append a timestamped note to a task. Params: `ref`,
    /// `body`. Annotations are indexed alongside docs by `memory.search`, which
    /// is why the note is worth writing rather than editing into the title.
    pub fn annotation_add(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let body = req_str(p, "body")?;

        let id = crate::clock::uuid_v7().to_string();
        let ts = now();
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        tx.execute(
            "INSERT INTO annotations (id, task_id, body, created) VALUES (?1, ?2, ?3, ?4)",
            params![id, task.id, body, ts],
        )?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "annotation.add",
            &json!({ "id": id, "body": body }),
        )?;
        tx.commit()?;

        // The body comes BACK, and that is a ruling rather than an oversight
        // (D72). It reads as drift beside `memory.add`, which takes a longer
        // body and answers `{created, id, title}` — a field report measured
        // 594-680 bytes for a ~350-byte annotation against 292 for a ~700-byte
        // doc, and called the echo waste, which for the caller's own bytes it
        // is. Two things outrank that. The tool promises the body is stored
        // *verbatim*, newlines and markdown included, and the echo is the only
        // evidence of it a caller ever gets; and the annotation object here is
        // the same `ANNOTATION` shape `task.get` returns, frozen by D56, so
        // dropping the field would be a removal from a v1 result — the one
        // thing the freeze does not permit — while eliding it above some size
        // would make a frozen field's value depend on its length, which is
        // worse than the bytes.
        Ok(json!({
            "short_id": task.short_id,
            "annotation": { "id": id, "body": body, "created": ts },
        }))
    }

    // ---- annotation.remove ----------------------------------------------------

    /// `annotation.remove` — tombstone one annotation by id (D113). Params:
    /// `ref`, `annotation_id`.
    ///
    /// **This is a scrub, not a soft-delete-and-hide.** The row named by
    /// `annotation_id` stays — `id`, `task_id`, `created` and the new `removed`
    /// timestamp are the audit trail that a note existed and when it went — but
    /// `body` is overwritten with `""` in the same statement, and the FTS5
    /// trigger re-indexes the empty string so `memory.search` cannot go on
    /// finding text that is no longer in the file. The alternative — flip a
    /// `removed` flag and leave `body` alone — would satisfy every reader that
    /// filters on the flag while leaving a secret sitting in the `.db` file for
    /// anything that reads the table directly (`store.export`, a hex editor, a
    /// backup). D113's threat model is exactly a caller who pasted one, so the
    /// text has to actually leave, not merely become invisible to this API.
    ///
    /// **The removal event never carries the body.** Recording what was removed
    /// in the `annotation.remove` event's payload would recreate, in the
    /// append-only event log, the exact leak this method exists to close — so
    /// the payload is `{"id": …}` and nothing else, and the response echoes
    /// only the id and the timestamp, never the text.
    ///
    /// An unknown id, or one already removed, is `not_found`: there is nothing
    /// left to remove either way, and conflating "never existed" with "already
    /// scrubbed" would make a caller unable to tell a typo from a job already
    /// done — the same reasoning `tag.remove` applies to a tag the task never
    /// had.
    pub fn annotation_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let annotation_id = req_str(p, "annotation_id")?;

        let ts = now();
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;

        let existing: Option<Option<String>> = tx
            .query_row(
                "SELECT removed FROM annotations WHERE id = ?1 AND task_id = ?2",
                params![annotation_id, task.id],
                |r| r.get(0),
            )
            .optional()?;

        let Some(removed) = existing else {
            // #624: a typo on a hard delete is worth a list of what IS there,
            // live notes only (a scrubbed one has no text and nothing left to
            // remove), newest first and capped, since a task can carry many.
            let mut stmt = tx.prepare(
                "SELECT id, body FROM annotations WHERE task_id = ?1 AND removed IS NULL \
                 ORDER BY created DESC, id DESC LIMIT 11",
            )?;
            let live: Vec<(String, String)> = stmt
                .query_map(params![task.id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let listed = match live.len() {
                0 => "it has no annotations".to_string(),
                n => format!(
                    "its {}annotations are: {}",
                    if n > 10 { "10 newest " } else { "" },
                    live.iter()
                        .take(10)
                        .map(|(id, body)| format!("{id} \"{}\"", first_words(body)))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            return Err(ApiError::not_found(
                format!(
                    "#{} has no annotation with id {annotation_id}; {listed} — nothing was \
                     removed.",
                    task.short_id
                ),
                None,
            ));
        };
        if removed.is_some() {
            return Err(ApiError::not_found(
                format!(
                    "annotation {annotation_id} on #{} was already removed — its body is \
                     already gone from the store; nothing further to remove.",
                    task.short_id
                ),
                None,
            ));
        }

        tx.execute(
            "UPDATE annotations SET body = '', removed = ?1 WHERE id = ?2",
            params![ts, annotation_id],
        )?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;

        // D113's own text used to promise this in part (2) and contradict it in
        // part (1): the `annotations` row and its FTS index are scrubbed above,
        // but until now the ORIGINAL `annotation.add` event — append-only,
        // readable forever via `event.list` and `store.export` — still carried
        // the full plaintext body. That is the exact leak the finding named:
        // a secret pasted into a note, "removed", still sitting in the file.
        //
        // This is a narrow, deliberate exception to append-only, not a second
        // precedent: ONLY the `annotation.add` event's own `body` field is
        // redacted, ONLY here, in the SAME transaction as the tombstone, and
        // ONLY for the annotation `annotation_remove` has just established is
        // being removed — so a live annotation's `add` event is never touched
        // by this code path (it only runs once removal is already underway).
        // `id` is kept so `event.list` still shows which note this record was
        // for, and `undo`'s `revert_annotation_add` never reads this payload's
        // `body` at all (it re-reads `annotations.body` fresh, and by this
        // point `annotation.remove` is the newest event, which refuses `undo`
        // by name — see D54/D113(3) — so the redacted payload is never even a
        // candidate for restoration).
        // A task can carry more than one `annotation.add` event, so this scans
        // by task and matches on the payload's own `id`, tolerantly (a
        // malformed payload is skipped, never a hard failure — matching how
        // event payloads are read elsewhere, `commands.rs`).
        let mut stmt = tx.prepare(
            "SELECT id, payload FROM events \
             WHERE op = 'annotation.add' AND entity_id = ?1",
        )?;
        let rows = stmt.query_map(params![task.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut redact_event_id = None;
        for row in rows {
            let (event_id, payload) = row?;
            let Some(payload) = payload else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            if opt_str(&v, "id").ok().flatten().as_deref() == Some(annotation_id.as_str()) {
                redact_event_id = Some(event_id);
                break;
            }
        }
        drop(stmt);
        if let Some(event_id) = redact_event_id {
            tx.execute(
                "UPDATE events SET payload = ?1 WHERE id = ?2",
                params![
                    json!({ "id": annotation_id, "body": null, "redacted": true }).to_string(),
                    event_id,
                ],
            )?;
        }

        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "annotation.remove",
            &json!({ "id": annotation_id }),
        )?;
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "removed": { "id": annotation_id, "removed": ts },
        }))
    }

    // ---- dependency.add ------------------------------------------------------

    /// `dependency.add` — record that `ref` is blocked by `depends_on`. Both are
    /// refs (short_id or UUID). Self-dependency and any edge that would close a
    /// cycle are `conflict`.
    ///
    /// The acyclicity check runs INSIDE the write transaction, so no concurrent
    /// writer can slip the closing edge in between the check and the insert.
    pub fn dependency_add(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let dep = p
            .get("depends_on")
            .ok_or_else(|| ApiError::bad_request("missing required field: depends_on"))?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        let target = self.resolve_ref_value_on(&tx, dep)?;

        if task.id == target.id {
            return Err(ApiError::conflict("a task cannot depend on itself"));
        }

        let ts = now();
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
        // Finding #9 (audit-2026-09): `dep` on an already-existing edge
        // answered exactly the same line as `dep` on a new one, so a caller
        // re-running the command after losing scrollback could not tell
        // whether anything happened. `INSERT OR IGNORE`'s own row count is
        // free evidence of which happened — no second query needed.
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO dependencies (task_id, depends_on_id) VALUES (?1, ?2)",
            params![task.id, target.id],
        )? > 0;
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
        // Re-read the resulting state INSIDE the transaction (the `tag_add`
        // rule): the helpers run on this same connection, so before the
        // commit is what puts their statements in the transaction — after it,
        // a writer landing in the gap makes the response describe a store
        // this call did not produce.
        let depends_on = self.depends_on_short_ids(&task.id)?;
        let blocked = self.is_blocked(&task.id)?;
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "depends_on": depends_on,
            "blocked": blocked,
            // Finding #9 (audit-2026-09): false on an edge that already
            // existed, so the CLI can say "already depends on" instead of
            // "now depends on" for a request that changed nothing.
            "inserted": inserted,
        }))
    }

    // ---- dependency.remove ---------------------------------------------------

    /// `dependency.remove` — drop the `ref` → `depends_on` edge. Both refs must
    /// resolve; an edge that was not there is a no-op that bumps no `rev` and
    /// writes no event, since nothing changed.
    ///
    /// The response reports `depends_on` and `blocked` either way, so the caller
    /// reads the resulting state rather than inferring it from "removed".
    pub fn dependency_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let dep = p
            .get("depends_on")
            .ok_or_else(|| ApiError::bad_request("missing required field: depends_on"))?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_on(&tx, p)?;
        let target = self.resolve_ref_value_on(&tx, dep)?;
        let ts = now();
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
        // Inside the transaction, as in `dependency_add` — see the comment
        // there.
        let depends_on = self.depends_on_short_ids(&task.id)?;
        let blocked = self.is_blocked(&task.id)?;
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "depends_on": depends_on,
            "blocked": blocked,
        }))
    }
}

/// The states a check may be in (D138).
///
/// A closed vocabulary, refused by name on a typo (D34), and the source of
/// truth for the engine's validation, its rejection message and the MCP
/// schema's `enum` — the shape [`crate::engine::SUMMARY_GROUP_BY`] established
/// and for the same reason: a drifted schema either forbids a valid state
/// forever or produces calls the engine rejects, with nothing going red.
///
/// `failed` is a first-class state and not an error: recording that a criterion
/// was NOT met is a normal, useful write, and a vocabulary of `open`/`passed`
/// alone would force a caller to delete the check or lie.
pub const CHECK_STATES: [&str; 3] = ["open", "passed", "failed"];

/// Which check a `check.set` / `check.remove` call names (#624).
enum CheckRef {
    Id(String),
    /// 1-based, in the order `task.get` and the card list the checks — NOT the
    /// stored `position` column, which keeps its gaps after a removal.
    Position(i64),
}

/// Read `check_id` or `position` — exactly one — before the transaction opens.
fn check_ref(p: &Value) -> Result<CheckRef, ApiError> {
    match (opt_str_nonempty(p, "check_id")?, opt_i64(p, "position")?) {
        (Some(id), None) => Ok(CheckRef::Id(id)),
        (None, Some(n)) if n >= 1 => Ok(CheckRef::Position(n)),
        (None, Some(n)) => Err(ApiError::bad_request(format!(
            "`position` is 1-based, as task.get lists the checks (got {n})"
        ))),
        _ => Err(ApiError::bad_request(
            "name the check by exactly one of `check_id` or `position` (1-based, in the order \
             task.get lists them)",
        )),
    }
}

/// The id of the check `named` on `task`, or a `not_found` listing the checks
/// the task really has. The `task_id` scope is the D113 shape: naming another
/// task's check answers `not_found` rather than reaching across tasks.
fn check_id_on(conn: &Connection, task: &Task, named: CheckRef) -> Result<String, ApiError> {
    let (found, what) = match named {
        CheckRef::Id(id) => (
            conn.query_row(
                "SELECT id FROM checks WHERE id = ?1 AND task_id = ?2",
                params![id, task.id],
                |r| r.get(0),
            )
            .optional()?,
            id,
        ),
        CheckRef::Position(n) => (
            conn.query_row(
                "SELECT id FROM checks WHERE task_id = ?1 ORDER BY position LIMIT 1 OFFSET ?2",
                params![task.id, n - 1],
                |r| r.get(0),
            )
            .optional()?,
            format!("at position {n}"),
        ),
    };
    match found {
        Some(id) => Ok(id),
        None => Err(no_such_check(conn, task, &what)?),
    }
}

/// #624: `task #589 has no check 01a0…` cost a whole `task.get` to recover
/// from a one-character typo. A task carries a handful of checks, so the
/// refusal lists them all — position, id, first words — and the retry is one
/// call. Shared with `task.done`'s `checks_passed`, which refuses the same way.
pub(super) fn no_such_check(
    conn: &Connection,
    task: &Task,
    what: &str,
) -> Result<ApiError, ApiError> {
    let mut stmt =
        conn.prepare("SELECT id, body FROM checks WHERE task_id = ?1 ORDER BY position")?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![task.id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let listed = if rows.is_empty() {
        "it has no checks".to_string()
    } else {
        let rows: Vec<String> = rows
            .iter()
            .enumerate()
            .map(|(i, (id, body))| format!("{}. {id} \"{}\"", i + 1, first_words(body)))
            .collect();
        format!(
            "its checks are: {} — name one by `check_id` or by `position`",
            rows.join(", ")
        )
    };
    Ok(ApiError::not_found(
        format!("task #{} has no check {what}; {listed}", task.short_id),
        None,
    ))
}

/// The first few words of a body, for naming a row in a refusal.
fn first_words(body: &str) -> String {
    let words: Vec<&str> = body.split_whitespace().collect();
    match words.len() {
        0..=6 => words.join(" "),
        _ => format!("{} …", words[..6].join(" ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `dependency.add` naming a DONE task as `ref` answered `blocked: true`
    /// — a flag nothing can ever act on, since `done` cannot be re-blocked and
    /// the task will never be re-evaluated by any lifecycle verb. Reproduces
    /// tasqx audit #158's second repro ("the flag can be set on an
    /// already-closed task after the fact").
    #[test]
    fn dependency_add_onto_a_done_task_never_reports_it_blocked() {
        let e = crate::Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "finished" })).unwrap();
        e.task_add(&json!({ "title": "future-blocker" })).unwrap();
        e.task_done(&json!({ "ref": 1 })).unwrap();

        let resp = e
            .dependency_add(&json!({ "ref": 1, "depends_on": 2 }))
            .unwrap();
        assert_eq!(resp["blocked"], json!(false));
        assert_eq!(
            e.task_get(&json!({ "ref": 1 })).unwrap()["blocked"],
            json!(false)
        );
    }

    /// Finding #9 (audit-2026-09): `dep` on an edge that already existed
    /// answered exactly the same line as `dep` on a brand new one, so a
    /// caller re-running the command could not tell whether anything
    /// happened. `inserted` must say which.
    #[test]
    fn re_adding_an_existing_dependency_says_it_was_not_inserted() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "dependent" })).unwrap();
        e.task_add(&json!({ "title": "blocker" })).unwrap();

        let first = e
            .dependency_add(&json!({ "ref": 1, "depends_on": 2 }))
            .unwrap();
        assert_eq!(first["inserted"], json!(true));

        let second = e
            .dependency_add(&json!({ "ref": 1, "depends_on": 2 }))
            .unwrap();
        assert_eq!(second["inserted"], json!(false));
        assert_eq!(second["depends_on"], first["depends_on"]);
    }

    /// Both dependency handlers answer with "the resulting state", so their
    /// response reads must run INSIDE the mutation's transaction — the rule
    /// `tag_add` states and keeps for the tag pair. Read after `commit()`, a
    /// writer landing in the gap makes the response describe a store this
    /// call did not produce. The helpers take `&self` on the engine's one
    /// connection, so ordering them before the commit is exactly what puts
    /// their statements inside the transaction. Structural, as with
    /// `store_export`'s twin guard: the interleaving point is inside SQLite.
    #[test]
    fn dependency_responses_are_read_inside_the_transaction() {
        let source = include_str!("relationships.rs");
        for name in ["dependency_add", "dependency_remove"] {
            // Assembled, never written out literally: `dispatch`'s
            // accepted-key guard splits this same source at every `fn NAME(`.
            let marker = format!("pub fn {name}(");
            let start = source
                .find(&marker)
                .unwrap_or_else(|| panic!("{name} exists"));
            let rest = &source[start + marker.len()..];
            let end = rest.find("\n    pub fn ").unwrap_or(rest.len());
            let body: String = rest[..end]
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");

            let commit = body
                .find(".commit()")
                .unwrap_or_else(|| panic!("{name} commits its transaction"));
            for read in ["depends_on_short_ids", "is_blocked"] {
                let at = body
                    .find(read)
                    .unwrap_or_else(|| panic!("{name} answers with {read}"));
                assert!(
                    at < commit,
                    "{name}: `{read}` runs after commit, outside the transaction \
                     whose result the response claims to report"
                );
            }
        }
    }
}
