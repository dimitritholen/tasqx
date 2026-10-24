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

use super::task::MergeDone;
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

/// Oldest first, by the instant each was recorded — parsed, never compared
/// as text: `…:10Z` and `…:10.5Z` sort the wrong way round as strings —
/// then branch, PR, commit, then the ref itself.
fn sort_refs(refs: &mut [Value]) {
    let kind = |v: &Value| match v["kind"].as_str() {
        Some("branch") => 0,
        Some("pr") => 1,
        _ => 2,
    };
    refs.sort_by(|a, b| {
        let at = |v: &Value| v["created"].as_str().and_then(parse_ts);
        at(a)
            .cmp(&at(b))
            .then(kind(a).cmp(&kind(b)))
            .then(a["ref"].as_str().cmp(&b["ref"].as_str()))
    });
}

/// One task's refs, oldest first ([`sort_refs`]).
pub(super) fn refs_of(conn: &Connection, task_id: &str) -> Result<Vec<Value>, ApiError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REF_COLS} FROM task_git_refs WHERE task_id = ?1"
    ))?;
    let rows = stmt.query_map(params![task_id], |r| ref_row(r, 0))?;
    let mut refs: Vec<Value> = rows.collect::<Result<_, _>>()?;
    sort_refs(&mut refs);
    Ok(refs)
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
            "SELECT task_id, {REF_COLS} FROM task_git_refs WHERE task_id IN ({marks})"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            Ok((r.get::<_, String>(0)?, ref_row(r, 1)?))
        })?;
        for row in rows {
            let (task, v) = row?;
            out.entry(task).or_default().push(v);
        }
    }
    out.values_mut().for_each(|refs| sort_refs(refs));
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

/// What `git.event` has already done for a branch, kept on its branch row
/// (`noted`) as JSON: the PR (or branch) it started the task for, the fact
/// its last note stated, the delivery note it wrote, and the note it
/// completed the task with. A retry reads these, never annotation text.
#[derive(Default, serde::Serialize, serde::Deserialize, PartialEq, Clone)]
struct Facts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merged: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed: Option<String>,
}

impl Engine {
    // ---- git.event -------------------------------------------------------------

    /// `git.event` — record a branch's PR event on its task and move the task
    /// (D239). Params: `event` (`opened`|`merged`|`closed`), `branch`, `pr?`
    /// (the PR URL), `sha?` (the merge commit), `task?` (a ref overriding the
    /// branch's `task/<id>-<slug>`; null is absent).
    ///
    /// Every event re-evaluates its outcome, replay or not, against what this
    /// branch has already done (its stored facts): `opened` starts a pending task at
    /// most once per PR, and `merged` completes it once nothing holds it back
    /// — and never again after the user reopened it. It acts as the task's
    /// live claim holder when there is one (D236), else as the shell.
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
        // The ref whose state the event is about; its facts live on the branch.
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
                // Whenever there is one, changed or not.
                "reason": reason.or_else(|| (!changed).then(|| "already recorded".to_string())),
            })
        };
        let status = task.status;
        if let Some(held) = anchor_kept {
            let reason = format!("{} {} is already {held}", anchor.0, anchor.1);
            return Ok(answer(wrote, status.as_str(), None, None, Some(reason)));
        }

        let (anchor_at,): (String,) = self.conn.query_row(
            "SELECT modified FROM task_git_refs WHERE task_id = ?1 AND kind = ?2 AND ref = ?3",
            params![task.id, anchor.0, anchor.1],
            |r| Ok((r.get(0)?,)),
        )?;
        let stored: Option<String> = self.conn.query_row(
            "SELECT noted FROM task_git_refs WHERE task_id = ?1 AND kind = 'branch' AND ref = ?2",
            params![task.id, branch],
            |r| r.get(0),
        )?;
        let facts: Facts = stored
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let r = json!(task.id);
        let label = pr.clone().unwrap_or_else(|| branch.clone());
        let merged = match &sha {
            Some(s) => format!("Merged {label} ({s})"),
            None => format!("Merged {label}"),
        };
        // D236: a git event acts for the task's live claim holder, else as
        // the shell, so the claim's own guards pass and `done` ends it.
        let holder = self.live_claim_holder(&task.id)?;
        // A note is written when the fact it states is not the one last
        // noted for this branch; the note and the fact are one commit.
        let note = |fact: String, body: String| -> Result<Value, ApiError> {
            if facts.note.as_deref() == Some(fact.as_str()) {
                return Ok(answer(wrote, status.as_str(), None, None, None));
            }
            let next = Facts {
                note: Some(fact),
                ..facts.clone()
            };
            let id = crate::clock::uuid_v7().to_string();
            self.git_facts(&task.id, &branch, &next, Some((&id, &body)))?;
            Ok(answer(
                true,
                status.as_str(),
                Some("annotated"),
                Some(body),
                None,
            ))
        };
        match kind.as_str() {
            "opened" => {
                if facts.started.as_deref() == Some(label.as_str()) {
                    let reason = format!("{label} already started this task once");
                    return Ok(answer(wrote, status.as_str(), None, None, Some(reason)));
                }
                match status {
                    Status::Pending => {
                        let mut start = json!({ "ref": r, "keep": true });
                        if let Some(h) = &holder {
                            start["holder"] = json!(h);
                        }
                        self.task_start_via(&start, Some("git"))?;
                        let next = Facts {
                            started: Some(label.clone()),
                            ..facts.clone()
                        };
                        self.git_facts(&task.id, &branch, &next, None)?;
                        Ok(answer(true, "active", Some("started"), None, None))
                    }
                    Status::Active => Ok(answer(wrote, status.as_str(), None, None, None)),
                    _ => note(
                        format!("opened:{anchor_at}"),
                        format!(
                            "{label} opened; #{} is {}, so it was not started",
                            task.short_id,
                            status.as_str()
                        ),
                    ),
                }
            }
            "merged" => {
                let open = matches!(status, Status::Pending | Status::Active | Status::Backlog);
                if open && facts.completed.is_some() {
                    let reason = "this merge completed the task once and it was reopened since; \
                                  it stays open"
                        .to_string();
                    return Ok(answer(wrote, status.as_str(), None, None, Some(reason)));
                }
                let mut done_params = json!({ "ref": r });
                if let Some(h) = &holder {
                    done_params["actor"] = json!(h);
                }
                let mut facts = facts.clone();
                let mut changed = wrote;
                // At most twice: once to learn a note is needed, once to pin it.
                for _ in 0..2 {
                    match self.task_done_merged(&done_params, facts.merged.as_deref())? {
                        MergeDone::Completed(done) => {
                            let next = Facts {
                                completed: facts.merged.clone(),
                                ..facts.clone()
                            };
                            self.git_facts(&task.id, &branch, &next, None)?;
                            return Ok(answer(
                                true,
                                done["status"].as_str().unwrap_or("done"),
                                Some("completed"),
                                Some(merged),
                                None,
                            ));
                        }
                        MergeDone::Held(why) => {
                            return note(
                                format!("held:{why}"),
                                format!("{merged}{NOT_COMPLETED}{why}"),
                            )
                        }
                        MergeDone::NotOpen(_) if facts.merged.is_some() => {
                            return Ok(answer(changed, status.as_str(), None, None, None));
                        }
                        MergeDone::NotOpen(st) => {
                            return note(
                                format!("merged-while:{}", st.as_str()),
                                format!("{merged} while the task was {}", st.as_str()),
                            )
                        }
                        MergeDone::NeedsNote => {
                            let id = crate::clock::uuid_v7().to_string();
                            facts.merged = Some(id.clone());
                            self.git_facts(&task.id, &branch, &facts, Some((&id, &merged)))?;
                            changed = true;
                        }
                    }
                }
                Ok(answer(
                    changed,
                    status.as_str(),
                    None,
                    None,
                    Some("the delivery note went missing while the merge was recorded".to_string()),
                ))
            }
            _ => note(
                format!("closed:{anchor_at}"),
                format!("{label} closed without merging"),
            ),
        }
    }

    /// Store a branch's [`Facts`], with the note they record when there is
    /// one, in one commit: the note's `annotation.add` is its event, else a
    /// `git.ref` bookkeeping event that `undo` steps over.
    fn git_facts(
        &self,
        task_id: &str,
        branch: &str,
        facts: &Facts,
        note: Option<(&str, &str)>,
    ) -> Result<(), ApiError> {
        let noted = serde_json::to_string(facts)
            .map_err(|e| ApiError::internal(format!("git facts: {e}")))?;
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &json!(task_id))?;
        tx.execute(
            "UPDATE task_git_refs SET noted = ?1 WHERE task_id = ?2 AND kind = 'branch' \
             AND ref = ?3",
            params![noted, task.id, branch],
        )?;
        match note {
            Some((id, body)) => super::write_note(&tx, &task, id, body, &now())?,
            None => insert_event(
                &tx,
                Entity::Task,
                &task.id,
                "git.ref",
                &json!({ "branch": branch, "noted": noted }),
            )?,
        }
        tx.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::short_id_of_branch;
    use crate::engine::task::MergeDone;
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
    fn a_merged_completion_of_a_task_already_done_writes_nothing() {
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
        let again = e
            .task_done_merged(&json!({ "ref": 1 }), Some(&pin))
            .unwrap();
        assert!(matches!(again, MergeDone::NotOpen(_)));
        assert_eq!(e.event_list(&json!({})).unwrap()["count"], events);
    }
}
