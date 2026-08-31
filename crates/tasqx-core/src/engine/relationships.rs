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

    /// `annotation.add` — append a timestamped note to a task. Params: `ref`,
    /// `body`. Annotations are indexed alongside docs by `memory.search`, which
    /// is why the note is worth writing rather than editing into the title.
    pub fn annotation_add(&self, p: &Value) -> Result<Value, ApiError> {
        let _ = ref_param(p)?;
        let body = req_str(p, "body")?;

        let id = Uuid::now_v7().to_string();
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

#[cfg(test)]
mod tests {
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
