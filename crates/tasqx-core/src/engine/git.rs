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
use std::collections::BTreeMap;

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
            // The row's first sighting keeps its own `created`, compared as
            // instants: `…:10Z` and `…:10.5Z` order wrongly as text.
            let held: String = tx.query_row(
                "SELECT created FROM task_git_refs WHERE task_id = ?1 AND kind = ?2 AND ref = ?3",
                params![id, kind, r],
                |row| row.get(0),
            )?;
            if parse_ts(&created) < parse_ts(&held) {
                tx.execute(
                    "UPDATE task_git_refs SET created = ?1 WHERE task_id = ?2 AND kind = ?3 \
                     AND ref = ?4",
                    params![created, id, kind, r],
                )?;
            }
        }
        changed |= wrote;
    }
    Ok(changed && merge)
}

/// What `git.event` has already done for a branch, kept on its branch row
/// (`noted`) as JSON, keyed per PR (its URL, else the merge sha, else the
/// branch): the PRs that started the task, the fact the last note stated,
/// the delivery note each merge wrote and the note each completed the task
/// with. A report reads these under the write lock, never annotation text.
#[derive(Default, serde::Serialize, serde::Deserialize, PartialEq, Clone)]
struct Facts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    started: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    merged: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    completed: BTreeMap<String, String>,
}

/// What a report leaves to do once its decision is committed.
enum Then {
    Nothing,
    Start,
    Complete(String),
}

impl Engine {
    // ---- git.event -------------------------------------------------------------

    /// `git.event` — record a branch's PR event on its task and move the task
    /// (D239). Params: `event` (`opened`|`merged`|`closed`), `branch`, `pr?`
    /// (the PR URL), `sha?` (the merge commit), `task?` (a ref overriding the
    /// branch's `task/<id>-<slug>`; null is absent).
    ///
    /// Every event re-evaluates its outcome, replay or not, against what this
    /// branch has already done (its stored facts), read and written under the
    /// write lock so concurrent reports serialise: `opened` starts a pending
    /// task at most once per PR, and `merged` completes it once nothing holds
    /// it back — and never again for that PR after the user reopened it. It
    /// acts as the task's live claim holder when there is one (D236), else as
    /// the shell.
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
        // One spelling per branch, PR and commit, however it was pasted.
        let raw = req_str(p, "branch")?;
        let branch = raw.trim();
        let branch = branch
            .strip_prefix("refs/heads/")
            .unwrap_or(branch)
            .to_string();
        if branch.is_empty() {
            return Err(crate::util::empty_string("branch"));
        }
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
        // The ref whose state the event is about; the facts live on the branch.
        let anchor = match &pr {
            Some(url) => ("pr", url.clone()),
            None => ("branch", branch.clone()),
        };
        // Which PR the facts are about: its URL, else the merge, else the branch.
        let key = pr
            .clone()
            .or_else(|| sha.clone())
            .unwrap_or_else(|| branch.clone());
        let label = pr.clone().unwrap_or_else(|| branch.clone());
        let merged = match &sha {
            Some(s) => format!("Merged {label} ({s})"),
            None => format!("Merged {label}"),
        };

        // One IMMEDIATE transaction: the refs, the facts read under the write
        // lock, the decision and the note it writes. A second report for the
        // branch waits here and sees the first one's facts.
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &target)?;
        let status = task.status;
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
        let (anchor_at, stored): (String, Option<String>) = tx.query_row(
            "SELECT a.modified, b.noted FROM task_git_refs a, task_git_refs b \
             WHERE a.task_id = ?1 AND a.kind = ?2 AND a.ref = ?3 \
             AND b.task_id = ?1 AND b.kind = 'branch' AND b.ref = ?4",
            params![task.id, anchor.0, anchor.1, branch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let held_facts: Facts = stored
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let mut facts = held_facts.clone();
        let mut reason: Option<String> = None;
        let mut note: Option<(String, String)> = None;
        let mut then = Then::Nothing;
        let mut transition: Option<&str> = None;
        // A note is due when the fact it states is not the one last noted;
        // it reads and moves the CURRENT facts, never a copy taken earlier.
        fn due(facts: &mut Facts, fact: String, body: String) -> Option<(String, String)> {
            if facts.note.as_deref() == Some(fact.as_str()) {
                return None;
            }
            facts.note = Some(fact);
            Some((crate::clock::uuid_v7().to_string(), body))
        }
        let open = matches!(status, Status::Pending | Status::Active | Status::Backlog);
        if let Some(held) = &anchor_kept {
            reason = Some(format!("{} {} is already {held}", anchor.0, anchor.1));
        } else {
            match kind.as_str() {
                "opened" if facts.started.contains(&label) => {
                    reason = Some(format!("{label} already started this task once"));
                }
                "opened" if matches!(status, Status::Pending | Status::Active) => {
                    // Recorded for an active task too: the user's later stop stands.
                    facts.started.push(label.clone());
                    if status == Status::Pending {
                        then = Then::Start;
                    }
                }
                "opened" => {
                    note = due(
                        &mut facts,
                        format!("opened:{anchor_at}"),
                        format!(
                            "{label} opened; #{} is {}, so it was not started",
                            task.short_id,
                            status.as_str()
                        ),
                    )
                }
                "merged" if open && facts.completed.contains_key(&key) => {
                    reason = Some(
                        "this merge completed the task once and it was reopened since; it \
                         stays open"
                            .to_string(),
                    );
                }
                "merged" if open => {
                    let why = self.merge_hold_back(&tx, &task.id)?;
                    if !why.is_empty() {
                        note = due(
                            &mut facts,
                            format!("held:{why}"),
                            format!("{merged}{NOT_COMPLETED}{why}"),
                        );
                    } else {
                        let live = match facts.merged.get(&key) {
                            Some(id) => tx.query_row(
                                "SELECT COUNT(*) FROM annotations WHERE id = ?1 AND task_id = ?2 \
                                 AND removed IS NULL",
                                params![id, task.id],
                                |r| r.get::<_, i64>(0),
                            )? > 0,
                            None => false,
                        };
                        if !live {
                            let id = crate::clock::uuid_v7().to_string();
                            facts.merged.insert(key.clone(), id.clone());
                            note = Some((id, merged.clone()));
                        }
                        then = Then::Complete(facts.merged[&key].clone());
                    }
                }
                "merged" if facts.merged.contains_key(&key) => {}
                "merged" => {
                    note = due(
                        &mut facts,
                        format!("merged-while:{}", status.as_str()),
                        format!("{merged} while the task was {}", status.as_str()),
                    )
                }
                _ => {
                    note = due(
                        &mut facts,
                        format!("closed:{anchor_at}"),
                        format!("{label} closed without merging"),
                    )
                }
            }
        }
        let facts_moved = facts != held_facts;
        if facts_moved {
            self.store_facts(&tx, &task.id, &branch, &facts, &ts)?;
        }
        if wrote || facts_moved || note.is_some() {
            // One rev bump and one event per commit: the note's own (which
            // bumps the rev), else the bookkeeping one.
            match &note {
                Some((id, body)) => super::write_note(&tx, &task, id, body, &ts)?,
                None => {
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
                }
            }
            tx.commit()?;
        } else {
            drop(tx);
        }
        let mut changed = wrote || facts_moved || note.is_some();
        let mut status_now = status.as_str().to_string();
        let mut note_text = note
            .as_ref()
            .filter(|(_, body)| *body != merged || matches!(then, Then::Nothing))
            .map(|(_, body)| body.clone());
        if note_text.is_some() {
            transition = Some("annotated");
        }

        // D236: a git event acts for the task's live claim holder, else as
        // the shell, so the claim's own guards pass and `done` ends it.
        let holder = self.live_claim_holder(&task.id)?;
        let r = json!(task.id);
        match then {
            Then::Nothing => {}
            Then::Start => {
                let mut start = json!({ "ref": r, "keep": true });
                if let Some(h) = &holder {
                    start["holder"] = json!(h);
                }
                self.task_start_via(&start, Some("git"))?;
                changed = true;
                status_now = "active".to_string();
                transition = Some("started");
            }
            Then::Complete(pin) => {
                let mut done = json!({ "ref": r });
                if let Some(h) = &holder {
                    done["actor"] = json!(h);
                }
                match self.task_done_merged(&done, Some(&pin))? {
                    MergeDone::Completed(out) => {
                        let mut facts = facts.clone();
                        facts.completed.insert(key.clone(), pin);
                        self.git_record_facts(&task.id, &branch, &facts)?;
                        changed = true;
                        status_now = out["status"].as_str().unwrap_or("done").to_string();
                        transition = Some("completed");
                        note_text = Some(merged.clone());
                    }
                    // Changed between the decision and the completion.
                    MergeDone::NotOpen(st) => {
                        reason = Some(format!("the task is {} now", st.as_str()));
                    }
                    MergeDone::Held(why) => {
                        reason = Some(format!("not completed: {why}; report it again"));
                    }
                    MergeDone::NeedsNote => {
                        reason = Some(
                            "the delivery note went missing; report the merge again".to_string(),
                        );
                    }
                }
            }
        }
        Ok(json!({
            "changed": changed,
            "event": kind,
            "short_id": task.short_id,
            "title": task.title,
            "status": status_now,
            "transition": transition,
            "note": note_text,
            // Whenever there is one, changed or not.
            "reason": reason.or_else(|| (!changed).then(|| "already recorded".to_string())),
        }))
    }

    /// Write a branch's [`Facts`] in the caller's transaction, bumping the
    /// row's `modified` so a merge import carries them.
    fn store_facts(
        &self,
        tx: &Connection,
        task_id: &str,
        branch: &str,
        facts: &Facts,
        ts: &str,
    ) -> Result<(), ApiError> {
        let noted = serde_json::to_string(facts)
            .map_err(|e| ApiError::internal(format!("git facts: {e}")))?;
        tx.execute(
            "UPDATE task_git_refs SET noted = ?1, modified = ?2 WHERE task_id = ?3 \
             AND kind = 'branch' AND ref = ?4",
            params![noted, ts, task_id, branch],
        )?;
        Ok(())
    }

    /// Record a branch's facts after the completion, in a commit of their own
    /// with a `git.ref` bookkeeping event that `undo` steps over.
    fn git_record_facts(&self, task_id: &str, branch: &str, facts: &Facts) -> Result<(), ApiError> {
        let tx = self.begin_mutation()?;
        let task = self.resolve_ref_value_on(&tx, &json!(task_id))?;
        let ts = now();
        self.store_facts(&tx, &task.id, branch, facts, &ts)?;
        insert_event(
            &tx,
            Entity::Task,
            &task.id,
            "git.ref",
            &json!({ "branch": branch, "completed": true }),
        )?;
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
