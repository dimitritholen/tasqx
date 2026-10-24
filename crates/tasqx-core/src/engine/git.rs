//! D239: `git.event`, the inbound door for a task branch's PR events.
//!
//! tasqx never calls GitHub or any network (§1). A caller that already knows
//! what happened — the land skill reading `gh pr view --json`, a CI step, a
//! git hook — reports it here, and the store records the branch, the PR and
//! the merge commit in `task_git_refs` and moves the task the way the event
//! means. Every write is attributed to the actor `git`.
//!
//! Idempotency is about the outcome. Every event re-evaluates what it means
//! for the task, and every note it writes is keyed on a FACT stored on the
//! ref (`noted`), never on matching annotation text: a retry writes a note
//! that is missing and never one that is already there.

use super::*;

/// The events a caller can report.
pub const GIT_EVENTS: [&str; 3] = ["opened", "merged", "closed"];

/// Every key an exported git ref object can carry. D34, like the check list.
pub const IMPORT_GIT_REF_KEYS: &[&str] =
    &["id", "kind", "ref", "state", "created", "modified", "noted"];

const KINDS: [&str; 3] = ["branch", "pr", "commit"];
const STATES: [&str; 3] = ["open", "closed", "merged"];

/// The words a merge that could not complete its task writes after `Merged …`.
const NOT_COMPLETED: &str = ", but not completed: ";

/// `task/<id>-<slug>` (any suffix: `-rebased`, `-v3`) or a bare `task/<id>`,
/// with or without `refs/heads/`, to the short id it names.
fn short_id_of_branch(branch: &str) -> Option<i64> {
    let rest = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let rest = rest.strip_prefix("task/")?;
    let digits = rest.split('-').next()?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

const REF_COLS: &str = "id, kind, ref, state, created, modified, noted";

fn ref_row(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(at)?,
        "kind": r.get::<_, String>(at + 1)?,
        "ref": r.get::<_, String>(at + 2)?,
        "state": r.get::<_, String>(at + 3)?,
        "created": r.get::<_, String>(at + 4)?,
        "modified": r.get::<_, String>(at + 5)?,
        "noted": r.get::<_, Option<String>>(at + 6)?,
    }))
}

/// One task's refs, oldest first.
pub(super) fn refs_of(conn: &Connection, task_id: &str) -> Result<Vec<Value>, ApiError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REF_COLS} FROM task_git_refs WHERE task_id = ?1 \
         ORDER BY created, instr('branch pr commit', kind), ref"
    ))?;
    let rows = stmt.query_map(params![task_id], |r| ref_row(r, 0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The refs of the given tasks, keyed by task uuid — the export reads only
/// the tasks it carries, a bounded batch of them per statement.
pub(super) fn refs_for(
    conn: &Connection,
    ids: &[&str],
) -> Result<HashMap<String, Vec<Value>>, ApiError> {
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();
    for chunk in ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut stmt = conn.prepare(&format!(
            "SELECT task_id, {REF_COLS} FROM task_git_refs WHERE task_id IN ({marks}) \
             ORDER BY task_id, created, instr('branch pr commit', kind), ref"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((r.get::<_, String>(0)?, ref_row(r, 1)?))
        })?;
        for row in rows {
            let (task, v) = row?;
            out.entry(task).or_default().push(v);
        }
    }
    Ok(out)
}

/// What [`upsert_ref`] did with one row.
#[derive(Debug, PartialEq)]
enum Upsert {
    Wrote,
    Same,
    /// The recorded state stood: it is `merged`, or a later change.
    Kept(String),
}

/// The one writer of a ref row, for `git.event` and `store.import` alike.
///
/// `merged` is terminal: nothing moves a merged ref, and a merge moves any
/// other. Between `open` and `closed` (a PR reopened, closed again) the later
/// recorded change (`at` against the row's `modified`) wins. `noted` is `None`
/// to leave the stored fact alone (an event) and `Some` to take the payload's
/// with its state (an import). `ties_win` lets a change stamped the same
/// instant as the recorded one through (an event, not an import).
#[allow(clippy::too_many_arguments)]
fn upsert_ref(
    conn: &Connection,
    task_id: &str,
    kind: &str,
    r: &str,
    state: &str,
    at: &str,
    id: Option<&str>,
    noted: Option<Option<&str>>,
    ties_win: bool,
) -> Result<Upsert, ApiError> {
    let held: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT state, modified, noted FROM task_git_refs \
             WHERE task_id = ?1 AND kind = ?2 AND ref = ?3",
            params![task_id, kind, r],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((held, held_at, held_noted)) = held else {
        let id = id.map_or_else(|| crate::clock::uuid_v7().to_string(), str::to_string);
        conn.execute(
            "INSERT INTO task_git_refs (id, task_id, kind, ref, state, created, modified, noted) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![id, task_id, kind, r, state, at, noted.flatten()],
        )?;
        return Ok(Upsert::Wrote);
    };
    // An event reported now wins a tie with the stamp it follows; an import
    // of the same row twice does not.
    let later = match ties_win {
        true => parse_ts(at) >= parse_ts(&held_at),
        false => parse_ts(at) > parse_ts(&held_at),
    };
    let wins = match (held.as_str(), state) {
        ("merged", "merged") => later,
        ("merged", _) => false,
        (_, "merged") => true,
        _ => later,
    };
    if !wins {
        return Ok(match held == state {
            true => Upsert::Same,
            false => Upsert::Kept(held),
        });
    }
    let noted_moves = noted.is_some_and(|n| n != held_noted.as_deref());
    if held == state && !noted_moves {
        return Ok(Upsert::Same);
    }
    conn.execute(
        "UPDATE task_git_refs SET state = ?1, modified = ?2, noted = CASE WHEN ?3 THEN ?4 \
         ELSE noted END WHERE task_id = ?5 AND kind = ?6 AND ref = ?7",
        params![
            state,
            at,
            noted.is_some(),
            noted.flatten(),
            task_id,
            kind,
            r
        ],
    )?;
    Ok(Upsert::Wrote)
}

/// `store.import`'s half: replace one task's refs from its payload, or on a
/// merge (D185) union them by `(kind, ref)` through [`upsert_ref`]: `merged`
/// wins, and between `open` and `closed` the later recorded change does.
/// Answers whether a merge added or changed a row.
pub(super) fn import_refs(
    tx: &rusqlite::Transaction,
    id: &str,
    tv: &Value,
    merge: bool,
    now_ts: Timestamp,
) -> Result<bool, ApiError> {
    if !merge {
        tx.execute("DELETE FROM task_git_refs WHERE task_id = ?1", params![id])?;
    }
    let Some(rows) = import_field(id, "git_refs", opt_array(tv, "git_refs"))? else {
        return Ok(false);
    };
    let mut changed = false;
    for g in rows {
        import_keys(
            &format!("task {id}, "),
            "git_refs[]",
            g,
            IMPORT_GIT_REF_KEYS,
        )?;
        let kind = import_field(id, "git_refs[].kind", req_str(g, "kind"))?;
        let state = import_field(id, "git_refs[].state", req_str(g, "state"))?;
        if !KINDS.contains(&kind.as_str()) || !STATES.contains(&state.as_str()) {
            return Err(ApiError::bad_request(format!(
                "task {id}, git_refs[]: kind must be {} and state {} (got {kind:?}, {state:?})",
                KINDS.join("|"),
                STATES.join("|")
            )));
        }
        let r = import_field(id, "git_refs[].ref", req_str(g, "ref"))?;
        let created = import_field(id, "git_refs[].created", opt_when(g, "created", now_ts))?
            .unwrap_or_else(now);
        let modified = import_field(id, "git_refs[].modified", opt_when(g, "modified", now_ts))?
            .unwrap_or_else(|| created.clone());
        let noted = import_field(id, "git_refs[].noted", opt_str_nonempty(g, "noted"))?;
        let rid = import_field(id, "git_refs[].id", opt_str_nonempty(g, "id"))?;
        let wrote = upsert_ref(
            tx,
            id,
            &kind,
            &r,
            &state,
            &modified,
            rid.as_deref(),
            Some(noted.as_deref()),
            false,
        )? == Upsert::Wrote;
        if wrote {
            // The row's first sighting keeps its own `created`.
            tx.execute(
                "UPDATE task_git_refs SET created = ?1 WHERE task_id = ?2 AND kind = ?3 \
                 AND ref = ?4 AND created > ?1",
                params![created, id, kind, r],
            )?;
        }
        changed |= wrote;
    }
    Ok(changed && merge)
}

impl Engine {
    // ---- git.event -------------------------------------------------------------

    /// `git.event` — record a branch's PR event on its task and move the task
    /// (D239). Params: `event` (`opened`|`merged`|`closed`), `branch`, `pr?`
    /// (the PR URL), `sha?` (the merge commit), `task?` (a ref overriding the
    /// branch's `task/<id>-<slug>`; null is absent).
    ///
    /// Every event re-evaluates its outcome, replay or not: `merged` completes
    /// a task once nothing holds it back, `opened` starts one still pending.
    /// A merged ref never moves back; an event that loses to it, or to a later
    /// change, answers `changed: false` with a `reason`.
    pub fn git_event(&self, p: &Value) -> Result<Value, ApiError> {
        crate::storage::as_actor(Some("git".to_string()), || self.git_event_as_git(p))
    }

    fn git_event_as_git(&self, p: &Value) -> Result<Value, ApiError> {
        let kind = req_str(p, "event")?;
        if !GIT_EVENTS.contains(&kind.as_str()) {
            return Err(ApiError::bad_request(format!(
                "`event` must be {} (got {kind:?})",
                GIT_EVENTS.join("|")
            )));
        }
        let branch = req_str(p, "branch")?.trim().to_string();
        if branch.is_empty() {
            return Err(crate::util::empty_string("branch"));
        }
        // One spelling per PR and per commit, however it was pasted.
        let pr = opt_str_nonempty(p, "pr")?
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        let sha = opt_str_nonempty(p, "sha")?
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty());
        let target = match p.get("task").filter(|t| !t.is_null()) {
            Some(t) => t.clone(),
            None => json!(short_id_of_branch(&branch).ok_or_else(|| {
                ApiError::bad_request(format!(
                    "branch {branch:?} names no task: use task/<id>-<slug>, or pass `task`"
                ))
            })?),
        };
        let state = match kind.as_str() {
            "opened" => "open",
            "merged" => "merged",
            _ => "closed",
        };
        let mut rows: Vec<(&str, String)> = vec![("branch", branch.clone())];
        if let Some(url) = &pr {
            rows.push(("pr", url.clone()));
        }
        if let (Some(s), "merged") = (&sha, kind.as_str()) {
            rows.push(("commit", s.clone()));
        }
        // The ref the event is about, and the one its notes are keyed on.
        let anchor = match &pr {
            Some(url) => ("pr", url.clone()),
            None => ("branch", branch.clone()),
        };

        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &target)?;
        let ts = now();
        let mut wrote = false;
        let mut anchor_kept: Option<String> = None;
        for (k, r) in &rows {
            match upsert_ref(&tx, &task.id, k, r, state, &ts, None, None, true)? {
                Upsert::Wrote => wrote = true,
                Upsert::Same => {}
                Upsert::Kept(held) if *k == anchor.0 && *r == anchor.1 => anchor_kept = Some(held),
                Upsert::Kept(_) => {}
            }
        }
        if wrote {
            tx.execute(
                "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
                params![task.rev + 1, ts, task.id],
            )?;
            insert_event(
                &tx,
                Entity::Task,
                &task.id,
                "git.ref",
                &json!({ "event": kind, "branch": branch, "pr": pr, "sha": sha }),
            )?;
            tx.commit()?;
        } else {
            drop(tx);
        }

        let answer = |changed: bool,
                      status: &str,
                      transition: Option<&str>,
                      note: Option<String>,
                      reason: Option<String>| {
            json!({
                "changed": changed,
                "event": kind,
                "short_id": task.short_id,
                "title": task.title,
                "status": status,
                "transition": transition,
                "note": note,
                "reason": match changed {
                    true => None,
                    false => Some(reason.unwrap_or_else(|| "already recorded".to_string())),
                },
            })
        };
        let status = task.status;
        if let Some(held) = anchor_kept {
            let reason = format!("{} {} is already {held}", anchor.0, anchor.1);
            return Ok(answer(wrote, status.as_str(), None, None, Some(reason)));
        }

        let (anchor_at, noted): (String, Option<String>) = self.conn.query_row(
            "SELECT modified, noted FROM task_git_refs WHERE task_id = ?1 AND kind = ?2 \
             AND ref = ?3",
            params![task.id, anchor.0, anchor.1],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let noted = noted.unwrap_or_default();
        let r = json!(task.id);
        let label = pr.clone().unwrap_or_else(|| branch.clone());
        let merged = match &sha {
            Some(s) => format!("Merged {label} ({s})"),
            None => format!("Merged {label}"),
        };
        // A note is written when the fact it states is not the one last
        // noted on the ref; the note and the fact are one commit.
        let note = |fact: String, body: String| -> Result<Value, ApiError> {
            if noted == fact {
                return Ok(answer(wrote, status.as_str(), None, None, None));
            }
            let id = crate::clock::uuid_v7().to_string();
            self.git_note(&task.id, &anchor, &id, &body, &fact)?;
            Ok(answer(
                true,
                status.as_str(),
                Some("annotated"),
                Some(body),
                None,
            ))
        };
        match (kind.as_str(), status) {
            ("opened", Status::Pending) => {
                self.task_start_via(&json!({ "ref": r, "keep": true }), Some("git"))?;
                Ok(answer(true, "active", Some("started"), None, None))
            }
            ("opened", Status::Active) => Ok(answer(wrote, status.as_str(), None, None, None)),
            ("opened", _) => note(
                format!("opened:{anchor_at}"),
                format!(
                    "{label} opened; #{} is {}, so it was not started",
                    task.short_id,
                    status.as_str()
                ),
            ),
            ("merged", Status::Pending | Status::Active | Status::Backlog) => {
                let unmet: Vec<String> = self
                    .checks_of(&task.id)?
                    .iter()
                    .filter(|c| c["state"] != "passed")
                    .map(|c| {
                        format!(
                            "{} \"{}\"",
                            c["state"].as_str().unwrap_or("open"),
                            c["body"].as_str().unwrap_or("")
                        )
                    })
                    .collect();
                let blockers: Vec<String> = self
                    .open_blockers(&task.id)?
                    .iter()
                    .map(|b| format!("#{}", b.short_id))
                    .collect();
                if !unmet.is_empty() || !blockers.is_empty() {
                    let mut why = Vec::new();
                    if !unmet.is_empty() {
                        let noun = if unmet.len() == 1 { "check" } else { "checks" };
                        why.push(format!(
                            "{} {noun} not passed: {}",
                            unmet.len(),
                            unmet.join("; ")
                        ));
                    }
                    if !blockers.is_empty() {
                        why.push(format!("blocked by {}", blockers.join(", ")));
                    }
                    let why = why.join("; ");
                    return note(
                        format!("held:{why}"),
                        format!("{merged}{NOT_COMPLETED}{why}"),
                    );
                }
                // The delivery note: the one this ref already wrote, if it is
                // still there, else a new one in its own commit.
                let mut changed = wrote;
                let pin = match noted.strip_prefix("merged:") {
                    Some(id) if self.live_note(&task.id, id)? => id.to_string(),
                    _ => {
                        let id = crate::clock::uuid_v7().to_string();
                        self.git_note(&task.id, &anchor, &id, &merged, &format!("merged:{id}"))?;
                        changed = true;
                        id
                    }
                };
                // The decision is re-read inside the completing transaction.
                match self.task_done_pinned(&json!({ "ref": r }), Some(&pin))? {
                    Some(done) => Ok(answer(
                        true,
                        done["status"].as_str().unwrap_or("done"),
                        Some("completed"),
                        Some(merged),
                        None,
                    )),
                    None => Ok(answer(
                        changed,
                        status.as_str(),
                        None,
                        None,
                        Some("the task changed while the merge was being recorded".to_string()),
                    )),
                }
            }
            // A task this ref already completed: nothing left to do.
            ("merged", _) if noted.starts_with("merged:") => {
                Ok(answer(wrote, status.as_str(), None, None, None))
            }
            ("merged", _) => note(
                format!("merged-while:{}", status.as_str()),
                format!("{merged} while the task was {}", status.as_str()),
            ),
            _ => note(
                format!("closed:{anchor_at}"),
                format!("{label} closed without merging"),
            ),
        }
    }

    /// Write one `git.event` note and the fact it states on the ref, in one
    /// commit with one `annotation.add` event.
    fn git_note(
        &self,
        task_id: &str,
        anchor: &(&str, String),
        note_id: &str,
        body: &str,
        fact: &str,
    ) -> Result<(), ApiError> {
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &json!(task_id))?;
        let ts = now();
        tx.execute(
            "INSERT INTO annotations (id, task_id, body, created) VALUES (?1, ?2, ?3, ?4)",
            params![note_id, task.id, body, ts],
        )?;
        tx.execute(
            "UPDATE tasks SET rev=?1, modified=?2 WHERE id=?3",
            params![task.rev + 1, ts, task.id],
        )?;
        tx.execute(
            "UPDATE task_git_refs SET noted = ?1 WHERE task_id = ?2 AND kind = ?3 AND ref = ?4",
            params![fact, task.id, anchor.0, anchor.1],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "annotation.add",
            &json!({ "id": note_id, "body": body }),
        )?;
        tx.commit()
    }

    /// Whether `id` is a live note on the task.
    fn live_note(&self, task_id: &str, id: &str) -> Result<bool, ApiError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM annotations WHERE id = ?1 AND task_id = ?2 AND removed IS NULL",
            params![id, task_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::short_id_of_branch;
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn branch_names_resolve_to_their_short_id() {
        assert_eq!(short_id_of_branch("task/2329-git-pr-links"), Some(2329));
        assert_eq!(short_id_of_branch("task/12-x-rebased"), Some(12));
        assert_eq!(short_id_of_branch("refs/heads/task/7-v3"), Some(7));
        assert_eq!(short_id_of_branch("task/7"), Some(7));
        assert_eq!(short_id_of_branch("task/x7-y"), None);
        assert_eq!(short_id_of_branch("main"), None);
        assert_eq!(short_id_of_branch("feature/task/7-x"), None);
    }

    /// A concurrent duplicate `merged` decided "complete it" before the first
    /// one committed; inside its own transaction it finds the task done and
    /// writes nothing.
    #[test]
    fn a_pinned_completion_of_a_task_already_done_is_none() {
        let e = Engine::open_in_memory().unwrap();
        e.task_add(&json!({ "title": "raced" })).unwrap();
        let out = e
            .git_event(
                &json!({ "event": "merged", "branch": "task/1-x", "pr": "https://x/pull/1" }),
            )
            .unwrap();
        assert_eq!(out["transition"], "completed");
        let pin = e.task_get(&json!({ "ref": 1 })).unwrap()["delivered_annotation_id"]
            .as_str()
            .unwrap()
            .to_string();
        let events = e.event_list(&json!({})).unwrap()["count"].clone();
        assert_eq!(
            e.task_done_pinned(&json!({ "ref": 1 }), Some(&pin))
                .unwrap(),
            None
        );
        assert_eq!(e.event_list(&json!({})).unwrap()["count"], events);
    }
}
