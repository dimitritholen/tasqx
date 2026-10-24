//! D241: quick-capture notes — an idea, a URL to read later, a line of
//! brainstorm — kept apart from the work and the knowledge.
//!
//! Not a task, so nothing here scores urgency or shows in `@working` or a
//! report; not a D238 proposal, which is a task awaiting acceptance; not a
//! memory doc, so `memory.search` never hands an agent a throwaway line as
//! knowledge. `note.promote` is the one door from here into either of those.

use super::memory::{insert_new_doc, NewDoc};
use super::*;
use crate::storage::alloc_note_short_id;

/// The statuses a note can hold.
pub const NOTE_STATUSES: [&str; 2] = ["open", "archived"];

/// `note.list`'s `status` vocabulary. **First entry is the default.**
pub const NOTE_LIST_STATUSES: [&str; 3] = ["open", "archived", "all"];

/// What `note.promote` can turn a note into.
pub const NOTE_PROMOTE_TARGETS: [&str; 2] = ["task", "memory"];

/// The longest title `note.promote` derives from a body, in characters,
/// before the `…` that says it was cut.
const NOTE_TITLE_CHARS: usize = 80;

const NOTE_COLS: &str = "id, short_id, body, project, status, promoted_to, created, modified, rev";

/// Whether a body carries a web link. Derived on every read and never
/// stored; `note.list {links: true}` asks SQL the same question.
fn note_has_links(body: &str) -> bool {
    body.contains("http://") || body.contains("https://")
}

/// One note row as every method answers it.
fn note_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let body: String = r.get(2)?;
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "short_id": r.get::<_, i64>(1)?,
        "has_links": note_has_links(&body),
        "body": body,
        "project": r.get::<_, Option<String>>(3)?,
        "status": r.get::<_, String>(4)?,
        "promoted_to": r.get::<_, Option<String>>(5)?,
        "created": r.get::<_, String>(6)?,
        "modified": r.get::<_, String>(7)?,
        "_rev": r.get::<_, i64>(8)?,
    }))
}

/// The `id` param: a note's uuid or its short id, as a number or a string.
fn note_ref(p: &Value) -> Result<&Value, ApiError> {
    p.get("id")
        .filter(|v| !v.is_null())
        .ok_or_else(|| ApiError::bad_request("missing required field: id"))
}

/// The note `r` names, read through `conn` — a mutation passes its own
/// transaction, so the check and the write see one snapshot.
fn note_lookup(conn: &Connection, r: &Value) -> Result<Value, ApiError> {
    let by_short = |n: i64| {
        conn.query_row(
            &format!("SELECT {NOTE_COLS} FROM notes WHERE short_id = ?1"),
            params![n],
            note_row,
        )
        .optional()
    };
    let found = match r {
        Value::Number(_) => match r.as_i64() {
            Some(n) => by_short(n)?,
            None => return Err(ApiError::bad_request("note id must be an integer short id")),
        },
        Value::String(s) => match s.parse::<i64>() {
            Ok(n) => by_short(n)?,
            Err(_) if Uuid::parse_str(s).is_ok() => conn
                .query_row(
                    &format!("SELECT {NOTE_COLS} FROM notes WHERE id = ?1"),
                    params![s],
                    note_row,
                )
                .optional()?,
            Err(_) => {
                return Err(ApiError::bad_request(format!(
                    "note id is neither a short id nor a UUID: {s}"
                )))
            }
        },
        _ => {
            return Err(ApiError::bad_request(
                "note id must be an integer or a string",
            ))
        }
    };
    found.ok_or_else(|| ApiError::not_found(format!("no note {r}"), Some(json!({ "id": r }))))
}

/// The title `note.promote` derives: the body's first non-blank line,
/// trimmed, cut at the last word break inside [`NOTE_TITLE_CHARS`] (or at
/// the limit when there is none in its second half) and marked with `…`.
fn note_title(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= NOTE_TITLE_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(NOTE_TITLE_CHARS).collect();
    let at = match cut.rfind(char::is_whitespace) {
        Some(i) if cut[..i].chars().count() >= NOTE_TITLE_CHARS / 2 => i,
        _ => cut.len(),
    };
    format!("{}…", cut[..at].trim_end())
}

/// A `conflict` when `expected_rev` names a revision the note is not at.
fn note_rev_guard(note: &Value, expected: Option<i64>) -> Result<(), ApiError> {
    let current = note["_rev"].as_i64().unwrap_or_default();
    match expected {
        Some(exp) if exp != current => Err(ApiError::new(
            crate::ErrorCode::Conflict,
            format!(
                "expected_rev {exp} but note #{} is at rev {current}: re-read it and retry \
                 with expected_rev {current}",
                note["short_id"]
            ),
            Some(json!({ "expected": exp, "current": current, "id": note["id"] })),
        )),
        _ => Ok(()),
    }
}

impl Engine {
    /// `note.add` — capture one note. Params: `body` (required), `project`
    /// (optional; a live project, checked like `task.add`'s).
    pub fn note_add(&self, p: &Value) -> Result<Value, ApiError> {
        let body = req_str(p, "body")?;
        let project = opt_str_nonempty(p, "project")?;
        let tx = self.begin_mutation()?;
        if let Some(name) = &project {
            require_live_project(&tx, name)?;
        }
        let id = crate::clock::uuid_v7().to_string();
        let short_id = alloc_note_short_id(&tx)?;
        let ts = now();
        tx.execute(
            "INSERT INTO notes (id, short_id, body, project, status, created, modified, rev) \
             VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?5, 0)",
            params![id, short_id, body, project, ts],
        )?;
        // No body in the event: the log says a note was taken, not what it
        // said, the way `memory.add`'s names a title and not a body.
        insert_event(
            &tx,
            Entity::Note,
            &id,
            "note.add",
            &json!({ "short_id": short_id, "project": project }),
        )?;
        let note = note_lookup(&tx, &json!(id))?;
        tx.commit()?;
        Ok(note)
    }

    /// `note.list` — a D70 page of notes, newest first. Params: `status`
    /// (`open` default, `archived`, `all`), `links` (only bodies holding an
    /// `http://` or `https://` link), `query` (a case-insensitive substring
    /// of the body, never a ranked search), `project`, `limit`, `offset`.
    pub fn note_list(&self, p: &Value) -> Result<Value, ApiError> {
        let status = opt_str(p, "status")?.unwrap_or_else(|| NOTE_LIST_STATUSES[0].to_string());
        if !NOTE_LIST_STATUSES.contains(&status.as_str()) {
            return Err(ApiError::bad_request(format!(
                "unknown note status `{status}` — accepted: {}",
                NOTE_LIST_STATUSES.join(", ")
            )));
        }
        let links = opt_bool(p, "links")?.unwrap_or(false);
        let query = opt_str_nonempty(p, "query")?;
        let project = opt_str_nonempty(p, "project")?;
        let offset = opt_u64(p, "offset")?.unwrap_or(0);
        let offset_i64 = i64::try_from(offset)
            .map_err(|_| ApiError::bad_request(format!("`offset` must be at most {}", i64::MAX)))?;
        let limit_i64 = match opt_u64(p, "limit")? {
            Some(n) => Some(i64::try_from(n).map_err(|_| {
                ApiError::bad_request(format!("`limit` must be at most {}", i64::MAX))
            })?),
            None => None,
        };

        let mut conds: Vec<&str> = Vec::new();
        let mut named: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        if status != "all" {
            conds.push("status = :status");
            named.push((":status", &status));
        }
        if links {
            conds.push("(instr(body, 'http://') > 0 OR instr(body, 'https://') > 0)");
        }
        // `instr`, not LIKE, so a `%` or `_` in the query is a character.
        // shortcut: SQLite's lower() folds ASCII only, so a non-ASCII letter
        // matches its own case alone; fold in Rust if anyone needs more.
        if let Some(q) = &query {
            conds.push("instr(lower(body), lower(:query)) > 0");
            named.push((":query", q));
        }
        if let Some(proj) = &project {
            conds.push("project = :project");
            named.push((":project", proj));
        }
        let where_clause = if conds.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conds.join(" AND "))
        };
        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM notes {where_clause}"),
            named.as_slice(),
            |r| r.get(0),
        )?;
        // `LIMIT -1` is SQLite's unlimited, `memory.list`'s same spelling.
        let limit_bound = limit_i64.unwrap_or(-1);
        let mut row_params = named.clone();
        row_params.push((":limit", &limit_bound));
        row_params.push((":offset", &offset_i64));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {NOTE_COLS} FROM notes {where_clause} \
             ORDER BY short_id DESC LIMIT :limit OFFSET :offset"
        ))?;
        let notes: Vec<Value> = stmt
            .query_map(row_params.as_slice(), note_row)?
            .collect::<Result<_, _>>()?;
        let reached = offset + notes.len() as u64;
        let next_offset = if limit_i64 == Some(0) || (reached as i64) >= total {
            Value::Null
        } else {
            json!(reached)
        };
        Ok(json!({
            "count": notes.len(),
            "total": total,
            "next_offset": next_offset,
            "notes": notes,
        }))
    }

    /// `note.get` — one note, by uuid or short id.
    pub fn note_get(&self, p: &Value) -> Result<Value, ApiError> {
        note_lookup(&self.conn, note_ref(p)?)
    }

    /// `note.update` — change a note's `body`, `project` or `status`
    /// (`archived` archives, `open` brings it back), guarded by
    /// `expected_rev` when given.
    pub fn note_update(&self, p: &Value) -> Result<Value, ApiError> {
        let r = note_ref(p)?;
        let body = opt_str_nonempty(p, "body")?;
        if body.as_deref().is_some_and(|b| b.trim().is_empty()) {
            return Err(ApiError::bad_request("a note's `body` cannot be blank"));
        }
        let project = opt_str_nonempty(p, "project")?;
        let status = opt_str_nonempty(p, "status")?;
        if let Some(s) = &status {
            if !NOTE_STATUSES.contains(&s.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "unknown note status `{s}` — accepted: {}",
                    NOTE_STATUSES.join(", ")
                )));
            }
        }
        if body.is_none() && project.is_none() && status.is_none() {
            return Err(ApiError::bad_request(
                "note.update requires at least one of `body`, `project`, `status`",
            ));
        }
        let expected_rev = opt_i64(p, "expected_rev")?;
        let tx = self.begin_mutation()?;
        let note = note_lookup(&tx, r)?;
        note_rev_guard(&note, expected_rev)?;
        if let Some(name) = &project {
            require_live_project(&tx, name)?;
        }
        let id = note["id"].as_str().unwrap_or_default().to_string();
        let rev = note["_rev"].as_i64().unwrap_or_default() + 1;
        let new_project = project.or_else(|| note["project"].as_str().map(str::to_string));
        let new_status = status.unwrap_or_else(|| note["status"].as_str().unwrap_or("open").into());
        tx.execute(
            "UPDATE notes SET body = COALESCE(?1, body), project = ?2, status = ?3, rev = ?4, \
             modified = ?5 WHERE id = ?6",
            params![body, new_project, new_status, rev, now(), id],
        )?;
        insert_event(
            &tx,
            Entity::Note,
            &id,
            "note.update",
            &json!({
                "short_id": note["short_id"],
                "project": new_project,
                "status": new_status,
                "rev": rev,
            }),
        )?;
        let out = note_lookup(&tx, &json!(id))?;
        tx.commit()?;
        Ok(out)
    }

    /// `note.remove` — delete a note for good. Outside `undo`: the event
    /// records that it went, not what it said.
    pub fn note_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let r = note_ref(p)?;
        let tx = self.begin_mutation()?;
        let note = note_lookup(&tx, r)?;
        let id = note["id"].as_str().unwrap_or_default().to_string();
        tx.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        insert_event(
            &tx,
            Entity::Note,
            &id,
            "note.remove",
            &json!({ "id": id, "short_id": note["short_id"], "project": note["project"] }),
        )?;
        tx.commit()?;
        Ok(json!({ "id": id, "short_id": note["short_id"], "removed": true }))
    }

    /// `note.promote` — file a note as a task or a memory doc, and archive it
    /// naming where it went, in ONE transaction: the task (through
    /// `task.add`'s own write, so its refusals are the same) or the doc
    /// (through `memory.add`'s), the full body as the task's first
    /// annotation when the title does not already say all of it, and the
    /// note's `promoted_to`. A refusal anywhere leaves all three untouched.
    pub fn note_promote(&self, p: &Value) -> Result<Value, ApiError> {
        let r = note_ref(p)?;
        let to = req_str(p, "to")?;
        if !NOTE_PROMOTE_TARGETS.contains(&to.as_str()) {
            return Err(ApiError::bad_request(format!(
                "note.promote cannot make a `{to}` — accepted: {}",
                NOTE_PROMOTE_TARGETS.join(", ")
            )));
        }
        let title = opt_str_nonempty(p, "title")?;
        let expected_rev = opt_i64(p, "expected_rev")?;
        let tx = self.begin_mutation()?;
        let note = note_lookup(&tx, r)?;
        note_rev_guard(&note, expected_rev)?;
        if let Some(already) = note["promoted_to"].as_str() {
            return Err(ApiError::new(
                crate::ErrorCode::Conflict,
                format!(
                    "note #{} was already promoted to {already}",
                    note["short_id"]
                ),
                Some(json!({ "id": note["id"], "promoted_to": already })),
            ));
        }
        let id = note["id"].as_str().unwrap_or_default().to_string();
        let body = note["body"].as_str().unwrap_or_default().to_string();
        let project = note["project"].as_str().map(str::to_string);
        let title = title.unwrap_or_else(|| note_title(&body));
        let ts = now();
        let mut out = json!({});
        let promoted_to = if to == "task" {
            let mut add = json!({ "title": title });
            if let Some(proj) = &project {
                add["project"] = json!(proj);
            }
            let task = self.task_add_within(&add, Some(&tx))?;
            let short = task["short_id"].as_i64().unwrap_or_default();
            if body.trim() != title {
                let held = self.task_by_short_on(&tx, short)?;
                let aid = crate::clock::uuid_v7().to_string();
                write_note(&tx, &held, &aid, &body, &ts)?;
            }
            // Who the task is, not `task.add`'s whole answer: `task.get`
            // reads the rest.
            out["task"] = json!({
                "id": task["id"],
                "short_id": short,
                "title": task["title"],
                "status": task["status"],
                "project": task["project"],
            });
            format!("task:#{short}")
        } else {
            let doc_id = crate::clock::uuid_v7().to_string();
            insert_new_doc(
                &tx,
                &NewDoc {
                    id: &doc_id,
                    title: &title,
                    body: &body,
                    source: None,
                    project: project.as_deref(),
                    standing: false,
                    ts: &ts,
                },
            )?;
            out["doc"] = json!({
                "id": doc_id,
                "title": title,
                "project": project,
                "standing": false,
                "created": ts,
            });
            format!("memory:{doc_id}")
        };
        let rev = note["_rev"].as_i64().unwrap_or_default() + 1;
        tx.execute(
            "UPDATE notes SET status = 'archived', promoted_to = ?1, rev = ?2, modified = ?3 \
             WHERE id = ?4",
            params![promoted_to, rev, ts, id],
        )?;
        insert_event(
            &tx,
            Entity::Note,
            &id,
            "note.promote",
            &json!({
                "short_id": note["short_id"],
                "project": project,
                "promoted_to": promoted_to,
                "rev": rev,
            }),
        )?;
        out["note"] = note_lookup(&tx, &json!(id))?;
        tx.commit()?;
        Ok(out)
    }
}

/// Every key an exported note carries (D241): the stored row. `has_links`
/// is derived on read and never travels.
pub const IMPORT_NOTE_KEYS: &[&str] = &[
    "id",
    "short_id",
    "body",
    "project",
    "status",
    "promoted_to",
    "created",
    "modified",
    "_rev",
];

/// D241: every note row for `store.export`, short-id ordered, kept when
/// `in_scope` admits its project — the D171 rule docs follow. Returns the
/// kept rows and how many were dropped.
pub(super) fn export_notes(
    conn: &Connection,
    in_scope: &dyn Fn(Option<&str>) -> bool,
) -> Result<(Vec<Value>, i64), ApiError> {
    let mut stmt = conn.prepare(&format!("SELECT {NOTE_COLS} FROM notes ORDER BY short_id"))?;
    let rows = stmt.query_map([], note_row)?;
    let (mut out, mut dropped) = (Vec::new(), 0i64);
    for r in rows {
        let mut v = r?;
        if in_scope(v["project"].as_str()) {
            if let Some(o) = v.as_object_mut() {
                o.remove("has_links");
            }
            out.push(v);
        } else {
            dropped += 1;
        }
    }
    Ok((out, dropped))
}

/// D241: the payload's `notes`, in `store.import`'s transaction. A note this
/// store does not hold lands whole, keeping its short id unless another note
/// here holds that number, when it takes the next free one. One it holds is
/// replaced on a restore, refused when the payload's `_rev` is older; on a
/// `merge` the later `modified` wins, a tie keeping the store's copy, and a
/// note whose removal in either log is the latest word on it (D197) is not
/// written back. Answers how many notes the payload carried in.
pub(super) fn import_notes(
    tx: &Transaction,
    rows: &[Value],
    merge: bool,
    removals: &super::merge_removals::Ledger,
) -> Result<i64, ApiError> {
    let mut imported = 0i64;
    for nv in rows {
        let nv = import_shape("", "note", nv)?;
        import_keys("", "note", nv, IMPORT_NOTE_KEYS)?;
        let nid = req_str(nv, "id").map_err(|e| {
            ApiError::bad_request(format!(
                "{} — each imported note requires an `id`",
                e.message
            ))
        })?;
        let field = |name: &str, e: ApiError| {
            ApiError::bad_request(format!("store.import: note {nid}, {name}: {}", e.message))
        };
        let body = req_str(nv, "body").map_err(|e| field("body", e))?;
        let project = opt_str_nonempty(nv, "project")?;
        let status = opt_str_nonempty(nv, "status")?.unwrap_or_else(|| "open".into());
        if !NOTE_STATUSES.contains(&status.as_str()) {
            return Err(field(
                "status",
                ApiError::bad_request(format!(
                    "`{status}` — accepted: {}",
                    NOTE_STATUSES.join(", ")
                )),
            ));
        }
        let promoted_to = opt_str_nonempty(nv, "promoted_to")?;
        let now_ts = crate::clock::now();
        let created = opt_when(nv, "created", now_ts)
            .map_err(|e| field("created", e))?
            .unwrap_or_else(now);
        let modified = opt_when(nv, "modified", now_ts)
            .map_err(|e| field("modified", e))?
            .unwrap_or_else(now);
        let rev = opt_i64(nv, "_rev")?.unwrap_or(0);
        let short_id = opt_i64(nv, "short_id")?;
        if merge
            && removals
                .removal(&[super::merge_removals::note_key(&nid)])
                .is_some()
        {
            continue;
        }
        let stored: Option<(i64, String)> = tx
            .query_row(
                "SELECT rev, modified FROM notes WHERE id = ?1",
                params![nid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match stored {
            Some((held_rev, held_modified)) if merge => {
                if parse_ts(&modified) > parse_ts(&held_modified) {
                    tx.execute(
                        "UPDATE notes SET body = ?2, project = ?3, status = ?4, \
                         promoted_to = ?5, modified = ?6, rev = ?7 WHERE id = ?1",
                        params![
                            nid,
                            body,
                            project,
                            status,
                            promoted_to,
                            modified,
                            held_rev.max(rev) + 1
                        ],
                    )?;
                }
            }
            Some((held_rev, _)) if held_rev > rev => {
                return Err(ApiError::conflict(format!(
                    "store.import: note {nid} carries _rev {rev}, but the store already holds \
                     it at _rev {held_rev} — importing it would roll the note back"
                )));
            }
            Some(_) => {
                tx.execute(
                    "UPDATE notes SET body = ?2, project = ?3, status = ?4, promoted_to = ?5, \
                     created = ?6, modified = ?7, rev = ?8 WHERE id = ?1",
                    params![
                        nid,
                        body,
                        project,
                        status,
                        promoted_to,
                        created,
                        modified,
                        rev
                    ],
                )?;
            }
            None => {
                let held = match short_id {
                    Some(n) => tx
                        .query_row(
                            "SELECT 1 FROM notes WHERE short_id = ?1",
                            params![n],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some(),
                    None => true,
                };
                let number = match short_id.filter(|_| !held) {
                    Some(n) => {
                        let floor = n.checked_add(1).ok_or_else(|| {
                            field("short_id", ApiError::bad_request("out of range"))
                        })?;
                        crate::storage::bump_note_short_id_floor(tx, floor)?;
                        n
                    }
                    None => alloc_note_short_id(tx)?,
                };
                tx.execute(
                    "INSERT INTO notes (id, short_id, body, project, status, promoted_to, \
                     created, modified, rev) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        nid,
                        number,
                        body,
                        project,
                        status,
                        promoted_to,
                        created,
                        modified,
                        rev
                    ],
                )?;
            }
        }
        // Bookkeeping, like a task's: `store.export` never carries an `import`.
        insert_event(
            tx,
            Entity::Note,
            &nid,
            "import",
            &json!({ "status": status }),
        )?;
        imported += 1;
    }
    Ok(imported)
}

/// D197 for notes: delete every stored note whose removal is the latest word
/// on it in `ledger`.
pub(super) fn delete_removed_notes(
    tx: &Transaction,
    ledger: &super::merge_removals::Ledger,
) -> Result<(), ApiError> {
    let ids: Vec<String> = ledger.removed_under("note:").map(str::to_string).collect();
    for id in ids {
        if ledger
            .removal(&[super::merge_removals::note_key(&id)])
            .is_some()
        {
            tx.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::note_title;

    #[test]
    fn a_title_is_the_first_non_blank_line_cut_at_a_word() {
        assert_eq!(note_title("\n  hello  \nworld"), "hello");
        let long = format!("{} tail", "word ".repeat(20));
        let t = note_title(&long);
        assert!(t.ends_with("word…"), "{t}");
        assert!(t.chars().count() <= 81, "{t}");
        let unbroken = "x".repeat(100);
        assert_eq!(note_title(&unbroken), format!("{}…", "x".repeat(80)));
    }
}
