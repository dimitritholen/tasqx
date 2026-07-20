//! Relationships domain methods for Engine.

use super::*;

impl Engine {
    // ---- tag.add -------------------------------------------------------------

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

    // ---- annotation.add ------------------------------------------------------

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

        Ok(json!({
            "short_id": task.short_id,
            "annotation": { "id": id, "body": body, "created": ts },
        }))
    }

    // ---- dependency.add ------------------------------------------------------

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
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "depends_on": self.depends_on_short_ids(&task.id)?,
            "blocked": self.is_blocked(&task.id)?,
        }))
    }

    // ---- dependency.remove ---------------------------------------------------

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
        tx.commit()?;

        Ok(json!({
            "short_id": task.short_id,
            "depends_on": self.depends_on_short_ids(&task.id)?,
            "blocked": self.is_blocked(&task.id)?,
        }))
    }
}
