//! D191: folding duplicate recurrence occurrences into one.
//!
//! Completing a recurring task spawns its next occurrence in the completing
//! store (D2, D170). Two machines that both complete the SAME occurrence each
//! spawn a next one under their own id, so one store imported into the other,
//! plain or `merge`, holds two copies of one piece of work (#776).
//!
//! `store.import` calls [`fold_duplicate_occurrences`] after its tasks,
//! edges, links and events are written, inside its own transaction, so a
//! `dry_run` rolls the fold back with everything else.
//!
//! # What an occurrence is
//!
//! `(spawned_from, due, scheduled)`, NULLs equal: one slot of one rule. Two
//! rows on one slot are one occurrence twice, whoever minted them, so every
//! such group in the store is folded, not only one this import brought a
//! member to; that is what lets every store reach the same state, and each
//! fold is reported, so none of it is silent. `task.done` spawns nothing
//! into a slot that is already filled, so a reopen and a second completion
//! no longer make a pair in one store. Copies on different dates are left
//! alone rather than guessed at.
//!
//! # What the fold keeps
//!
//! The survivor is the lowest id, so every store picks the same one. The
//! status group (`status`, `completed`, `active_since`, the delivery pin)
//! goes to the copy whose log holds the latest status event, D189's rule
//! over the two copies' logs ([`field_merge::group_winner`]), so a stop or a
//! completion on either copy is the last word wherever it lands. Everything
//! else is the survivor's. Tracked time is D189's
//! [`field_merge::merged_tracked`] over the same two logs: what their union
//! counts, each event once by id, plus the larger unexplained residual. The
//! dropped copy's events move onto the survivor, so the next merge reads the
//! same history, a round trip does not count work twice, and work logged on
//! a copy after another store folded it still arrives.
//!
//! # Why moved child rows change id
//!
//! A note, check or token id held by another task refuses the import (#802,
//! #803), and the other store still holds the dropped copy's rows under their
//! old ids. So a moved row takes a UUIDv8 derived from its ORIGINAL id and the
//! survivor: the original's 48-bit time prefix, the rest XORed with a hash of
//! the survivor. The XOR is its own inverse, so a row derived onto one copy
//! and folded again is traced back to its original first, and every fold path
//! ends on the same id. A live note, or a check with the same body, state and
//! evidence, that the survivor already holds is the D170 spawn copy and is
//! dropped rather than doubled.

use super::*;

/// Fold every recurrence group in the store into its lowest id, one report
/// row per task removed, until no group is left. Re-pointing a dropped
/// copy's successors onto the survivor can form a new group one generation
/// down, so the loop reads the groups afresh after every fold.
///
/// `payload_events` is the document's task events by task id, as the event
/// pass read them.
pub(super) fn fold_duplicate_occurrences(
    tx: &Transaction,
    payload_events: &HashMap<String, Vec<field_merge::LoggedEvent>>,
) -> Result<Vec<Value>, ApiError> {
    let mut report = Vec::new();
    while let Some((kept, dropped)) = next_pair(tx)? {
        report.push(fold(tx, &kept, &dropped, payload_events)?);
    }
    Ok(report)
}

/// The lowest two ids of the first group holding more than one task.
fn next_pair(tx: &Transaction) -> Result<Option<(String, String)>, ApiError> {
    let group: Option<String> = tx
        .query_row(
            "SELECT group_concat(id, ' ' ORDER BY id) FROM tasks WHERE spawned_from IS NOT NULL \
             GROUP BY spawned_from, due, scheduled HAVING count(*) > 1 \
             ORDER BY spawned_from, due, scheduled LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(group.map(|g| {
        let mut ids = g.split(' ');
        let kept = ids.next().unwrap_or_default().to_string();
        let dropped = ids.next().unwrap_or_default().to_string();
        (kept, dropped)
    }))
}

/// The fields of one copy the fold reads.
struct Copy {
    short_id: i64,
    status: String,
    completed: Option<String>,
    active_since: Option<String>,
    tracked: i64,
    adjustment: i64,
    delivered: Option<String>,
    parent: Option<String>,
    modified: String,
}

fn read_copy(tx: &Transaction, id: &str) -> Result<Copy, ApiError> {
    Ok(tx.query_row(
        "SELECT short_id, status, completed, active_since, tracked_seconds, \
         tracked_adjustment_seconds, delivered_annotation_id, spawned_from, modified \
         FROM tasks WHERE id = ?1",
        params![id],
        |r| {
            Ok(Copy {
                short_id: r.get(0)?,
                status: r.get(1)?,
                completed: r.get(2)?,
                active_since: r.get(3)?,
                tracked: r.get(4)?,
                adjustment: r.get(5)?,
                delivered: r.get(6)?,
                parent: r.get(7)?,
                modified: r.get(8)?,
            })
        },
    )?)
}

/// Move everything `dropped` owns onto `kept`, merge the scalars, delete
/// `dropped`, and report it.
fn fold(
    tx: &Transaction,
    kept: &str,
    dropped: &str,
    payload_events: &HashMap<String, Vec<field_merge::LoggedEvent>>,
) -> Result<Value, ApiError> {
    let k = read_copy(tx, kept)?;
    let d = read_copy(tx, dropped)?;
    let parent_short: Option<i64> = tx
        .query_row(
            "SELECT short_id FROM tasks WHERE id = ?1",
            params![k.parent],
            |r| r.get(0),
        )
        .optional()?;
    let mut changed = false;

    let notes = move_annotations(tx, kept, dropped, &mut changed)?;
    move_checks(tx, kept, dropped, &mut changed)?;
    for old in ids_owned_by(tx, "token_usage", dropped)? {
        rekey_row(tx, "token_usage", &old, dropped, kept, &mut changed)?;
    }
    changed |= tx.execute(
        "INSERT OR IGNORE INTO task_tags (task_id, tag_id) \
         SELECT ?1, tag_id FROM task_tags WHERE task_id = ?2",
        params![kept, dropped],
    )? > 0;
    tx.execute("DELETE FROM task_tags WHERE task_id = ?1", params![dropped])?;

    for col in ["task_id", "depends_on_id"] {
        changed |= tx.execute(
            &format!("UPDATE OR IGNORE dependencies SET {col} = ?1 WHERE {col} = ?2"),
            params![kept, dropped],
        )? > 0;
        tx.execute(
            &format!("DELETE FROM dependencies WHERE {col} = ?1"),
            params![dropped],
        )?;
    }
    tx.execute(
        "DELETE FROM dependencies WHERE task_id = ?1 AND depends_on_id = ?1",
        params![kept],
    )?;
    let dropped_edges = break_cycles(tx, kept)?;
    changed |= !dropped_edges.is_empty();

    changed |= repoint_link_ends(tx, NodeType::Task, dropped, kept)?;
    tx.execute(
        "DELETE FROM links WHERE from_type = ?1 AND to_type = ?1 AND from_id = ?2 AND to_id = ?2",
        params![NodeType::Task.as_str(), kept],
    )?;

    // Both copies' logs, read before the dropped copy's events move.
    let kept_log = copy_log(tx, kept, payload_events)?;
    let dropped_log = copy_log(tx, dropped, payload_events)?;

    // The status group follows the latest status event in either log, D189's
    // rule, with its tie-break; a fixed rank would let a copy that was
    // started once stay running after a later stop on the other.
    let group = |c: &Copy| {
        [
            json!(c.status),
            json!(c.completed),
            json!(c.active_since),
            json!(c.delivered),
        ]
    };
    let (kv, dv) = (group(&k), group(&d));
    let tie = field_merge::tie_side(
        &k.modified,
        &d.modified,
        &kv.iter().collect::<Vec<_>>(),
        &dv.iter().collect::<Vec<_>>(),
    );
    let dropped_wins = field_merge::group_winner(&kept_log, &dropped_log, "status", tie)
        == field_merge::Side::Payload;
    let (status, completed, active_since, delivered) = if dropped_wins {
        let pin = d.delivered.as_ref().and_then(|p| notes.get(p)).cloned();
        (
            d.status.clone(),
            d.completed.clone(),
            d.active_since.clone(),
            pin,
        )
    } else {
        (
            k.status.clone(),
            k.completed.clone(),
            k.active_since.clone(),
            k.delivered.clone(),
        )
    };
    changed |= (&status, &completed, &active_since, &delivered)
        != (&k.status, &k.completed, &k.active_since, &k.delivered);

    // Time, D189's way: both copies are one task, so their two logs are its
    // history. The total is what the union of the logs counts plus the
    // larger unexplained residual, a symmetric function of the two copies,
    // and an event arriving twice (the other store folded first) is one
    // event, so nothing is counted twice and later work on either copy is.
    let running = |c: &Copy| {
        (c.status == "active")
            .then(|| c.active_since.as_deref().and_then(parse_ts))
            .flatten()
    };
    let (tracked, adjustment) = field_merge::merged_tracked(
        (k.tracked, k.adjustment),
        (d.tracked, d.adjustment),
        &kept_log,
        &dropped_log,
        &field_merge::Running {
            stored: running(&k),
            payload: running(&d),
            merged: (status == "active")
                .then(|| active_since.as_deref().and_then(parse_ts))
                .flatten(),
        },
    );
    changed |= (tracked, adjustment) != (k.tracked, k.adjustment);

    tx.execute(
        "UPDATE events SET entity_id = ?1 WHERE entity = ?2 AND entity_id = ?3",
        params![kept, Entity::Task.as_str(), dropped],
    )?;
    tx.execute(
        "UPDATE tasks SET spawned_from = ?1 WHERE spawned_from = ?2",
        params![kept, dropped],
    )?;
    tx.execute("DELETE FROM tasks WHERE id = ?1", params![dropped])?;
    // `rev` moves when the survivor did, so an export taken before the fold
    // is stale to the #177 guard; `modified` stays, since nobody edited it.
    tx.execute(
        "UPDATE tasks SET status = ?1, completed = ?2, active_since = ?3, \
         delivered_annotation_id = ?4, tracked_seconds = ?5, \
         tracked_adjustment_seconds = ?6, \
         rev = rev + ?7 WHERE id = ?8",
        params![
            status,
            completed,
            active_since,
            delivered,
            tracked,
            adjustment,
            i64::from(changed),
            kept
        ],
    )?;

    Ok(json!({
        "kept": k.short_id,
        "dropped": d.short_id,
        "spawned_from": parent_short,
        "kept_id": kept,
        "dropped_id": dropped,
        "dropped_status": d.status,
        "status": status,
        "dropped_dependencies": dropped_edges,
    }))
}

/// One copy's log: its task events here (bar `import`, which no export
/// carries) and every event the document carried for it. An event the
/// document names that this store already held under the other copy was
/// skipped by the event pass, so the document's list is what says this copy
/// saw it.
fn copy_log(
    tx: &Transaction,
    task: &str,
    payload_events: &HashMap<String, Vec<field_merge::LoggedEvent>>,
) -> Result<Vec<field_merge::LoggedEvent>, ApiError> {
    let mut stmt = tx.prepare(
        "SELECT id, op, payload, ts FROM events \
         WHERE entity = ?1 AND entity_id = ?2 AND op <> 'import'",
    )?;
    let rows = stmt.query_map(params![Entity::Task.as_str(), task], |r| {
        Ok(field_merge::LoggedEvent {
            id: r.get(0)?,
            op: r.get(1)?,
            payload: r
                .get::<_, Option<String>>(2)?
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or(Value::Null),
            ts: r.get(3)?,
        })
    })?;
    let mut log: Vec<field_merge::LoggedEvent> = rows.collect::<Result<_, _>>()?;
    let held: HashSet<String> = log.iter().map(|e| e.id.clone()).collect();
    for ev in payload_events.get(task).into_iter().flatten() {
        if !held.contains(&ev.id) {
            log.push(field_merge::LoggedEvent {
                id: ev.id.clone(),
                op: ev.op.clone(),
                payload: ev.payload.clone(),
                ts: ev.ts.clone(),
            });
        }
    }
    Ok(log)
}

/// Two copies wired into opposite ends of one chain close a cycle once they
/// are one task. Each of the survivor's own edges that now closes one is
/// dropped, lowest id first, and its target's short id returned, so the
/// import still lands and the two stores can go on syncing.
fn break_cycles(tx: &Transaction, kept: &str) -> Result<Vec<i64>, ApiError> {
    let blockers: Vec<(String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT d.depends_on_id, t.short_id FROM dependencies d \
             JOIN tasks t ON t.id = d.depends_on_id WHERE d.task_id = ?1 ORDER BY d.depends_on_id",
        )?;
        let rows = stmt.query_map(params![kept], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut dropped = Vec::new();
    for (b, short) in blockers {
        if reaches(tx, &b, kept)? {
            tx.execute(
                "DELETE FROM dependencies WHERE task_id = ?1 AND depends_on_id = ?2",
                params![kept, b],
            )?;
            dropped.push(short);
        }
    }
    Ok(dropped)
}

/// The ids of the rows in `table` that `task` owns, oldest first. `table` is
/// always a literal from this file.
fn ids_owned_by(tx: &Transaction, table: &str, task: &str) -> Result<Vec<String>, ApiError> {
    let mut stmt = tx.prepare(&format!(
        "SELECT id FROM {table} WHERE task_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map(params![task], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Move the dropped copy's notes, returning old id to the id each now lives
/// under. A live note the survivor already holds word for word is the D170
/// copy of the description: it goes, and a link to it follows to the
/// survivor's.
fn move_annotations(
    tx: &Transaction,
    kept: &str,
    dropped: &str,
    changed: &mut bool,
) -> Result<HashMap<String, String>, ApiError> {
    let mut moved = HashMap::new();
    for old in ids_owned_by(tx, "annotations", dropped)? {
        let twin: Option<String> = tx
            .query_row(
                "SELECT k.id FROM annotations k JOIN annotations d ON d.body = k.body \
                 WHERE d.id = ?1 AND d.removed IS NULL AND k.task_id = ?2 \
                 AND k.removed IS NULL ORDER BY k.id LIMIT 1",
                params![old, kept],
                |r| r.get(0),
            )
            .optional()?;
        let to = match twin {
            Some(twin) => {
                tx.execute("DELETE FROM annotations WHERE id = ?1", params![old])?;
                twin
            }
            None => rekey_row(tx, "annotations", &old, dropped, kept, changed)?,
        };
        *changed |= repoint_link_ends(tx, NodeType::Annotation, &old, &to)?;
        moved.insert(old, to);
    }
    Ok(moved)
}

/// A check the survivor already holds with the same body, state and evidence
/// is the D170 copy of the criterion and goes; every other check moves.
fn move_checks(
    tx: &Transaction,
    kept: &str,
    dropped: &str,
    changed: &mut bool,
) -> Result<(), ApiError> {
    for old in ids_owned_by(tx, "checks", dropped)? {
        let twin: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM checks k JOIN checks d ON d.body = k.body \
             AND d.state = k.state AND d.evidence IS k.evidence \
             WHERE d.id = ?1 AND k.task_id = ?2)",
            params![old, kept],
            |r| r.get(0),
        )?;
        if twin {
            tx.execute("DELETE FROM checks WHERE id = ?1", params![old])?;
        } else {
            rekey_row(tx, "checks", &old, dropped, kept, changed)?;
        }
    }
    Ok(())
}

/// Hand row `old` of `table`, owned by `owner`, to `kept` under its folded
/// id. When that id is already there, this fold ran before, here or where the
/// row came from, and the arriving copy is deleted instead. Returns the id the
/// row now lives under.
fn rekey_row(
    tx: &Transaction,
    table: &str,
    old: &str,
    owner: &str,
    kept: &str,
    changed: &mut bool,
) -> Result<String, ApiError> {
    let new = folded_id(&original_id(old, owner), kept);
    let held: bool = tx.query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?1)"),
        params![new],
        |r| r.get(0),
    )?;
    if held {
        tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), params![old])?;
    } else {
        tx.execute(
            &format!("UPDATE {table} SET id = ?1, task_id = ?2 WHERE id = ?3"),
            params![new, kept, old],
        )?;
        *changed = true;
    }
    Ok(new)
}

/// The XOR mask a fold onto task `kept` applies: everything after the time
/// prefix, minus the version nibble and the variant bits.
fn fold_mask(kept: &str) -> [u8; 16] {
    let mut m = *Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        format!("tasqx-fold:{kept}").as_bytes(),
    )
    .as_bytes();
    m[..6].fill(0);
    m[6] &= 0x0F;
    m[8] &= 0x3F;
    m
}

fn with_version(mut bytes: [u8; 16], version: u8) -> String {
    bytes[6] = (bytes[6] & 0x0F) | (version << 4);
    Uuid::from_bytes(bytes).to_string()
}

/// The id a row minted under a UUIDv7 `original` takes on task `kept`: the
/// same time prefix, the rest XORed with [`fold_mask`], stamped version 8. An
/// id that is not a UUIDv7 gets a v5 hash instead, which cannot be traced
/// back but is still the same on every store.
fn folded_id(original: &str, kept: &str) -> String {
    match Uuid::parse_str(original) {
        Ok(u) if u.get_version_num() == 7 => {
            let mut bytes = *u.as_bytes();
            for (b, m) in bytes.iter_mut().zip(fold_mask(kept)) {
                *b ^= m;
            }
            with_version(bytes, 8)
        }
        _ => Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("tasqx-fold:{kept}:{original}").as_bytes(),
        )
        .to_string(),
    }
}

/// The inverse of [`folded_id`] for a row `owner` holds: a version-8 id was
/// folded onto `owner`, so XORing its mask again gives the original back.
fn original_id(id: &str, owner: &str) -> String {
    match Uuid::parse_str(id) {
        Ok(u) if u.get_version_num() == 8 => {
            let mut bytes = *u.as_bytes();
            for (b, m) in bytes.iter_mut().zip(fold_mask(owner)) {
                *b ^= m;
            }
            with_version(bytes, 7)
        }
        _ => id.to_string(),
    }
}

/// Re-point every link end naming `(kind, from)` at `(kind, to)`, reporting
/// whether any moved. A link that would duplicate one already there is
/// dropped, the UNIQUE key's rule.
fn repoint_link_ends(
    tx: &Transaction,
    kind: NodeType,
    from: &str,
    to: &str,
) -> Result<bool, ApiError> {
    let mut moved = false;
    for end in ["from", "to"] {
        moved |= tx.execute(
            &format!(
                "UPDATE OR IGNORE links SET {end}_id = ?1 WHERE {end}_type = ?2 AND {end}_id = ?3"
            ),
            params![to, kind.as_str(), from],
        )? > 0;
        tx.execute(
            &format!("DELETE FROM links WHERE {end}_type = ?1 AND {end}_id = ?2"),
            params![kind.as_str(), from],
        )?;
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUE: &str = "2026-09-25T00:00:00Z";

    /// The #776 repro: B holds a recurring task R, A is seeded from B's
    /// export, and both complete R. `a_first` decides which store's spawn is
    /// the older (lower UUIDv7) id and so which copy survives.
    struct TwoMachines {
        a: Engine,
        b: Engine,
        r_id: String,
    }

    fn two_machines(a_first: bool) -> TwoMachines {
        let b = Engine::open_in_memory().expect("open b");
        let r = b
            .task_add(&json!({ "title": "R", "recurrence": "every 3 days", "due": DUE }))
            .expect("add R");
        let r_id = r["id"].as_str().expect("id").to_string();
        b.annotation_add(&json!({ "ref": r_id, "body": "What R is." }))
            .expect("note");
        b.check_add(&json!({ "ref": r_id, "body": "R verified" }))
            .expect("check");
        let a = Engine::open_in_memory().expect("open a");
        a.store_import(&b.store_export(&json!({})).expect("export b"))
            .expect("seed a");
        let (first, second) = if a_first { (&a, &b) } else { (&b, &a) };
        first
            .task_done(&json!({ "ref": r_id }))
            .expect("done R first");
        second
            .task_done(&json!({ "ref": r_id }))
            .expect("done R second");
        TwoMachines { a, b, r_id }
    }

    fn export(e: &Engine) -> Value {
        e.store_export(&json!({})).expect("export")
    }

    /// Every task in `e` spawned from `parent`, as exported.
    fn occurrences(e: &Engine, parent: &str) -> Vec<Value> {
        export(e)["tasks"]
            .as_array()
            .expect("tasks")
            .iter()
            .filter(|t| t["spawned_from"].as_str() == Some(parent))
            .cloned()
            .collect()
    }

    fn only_occurrence(e: &Engine, parent: &str) -> Value {
        let occ = occurrences(e, parent);
        assert_eq!(occ.len(), 1, "exactly one next occurrence: {occ:?}");
        occ.into_iter().next().expect("one")
    }

    fn id_of(t: &Value) -> String {
        t["id"].as_str().expect("id").to_string()
    }

    fn import(into: &Engine, from: &Engine, merge: bool) -> Value {
        let mut doc = export(from);
        doc["merge"] = json!(merge);
        into.store_import(&doc).expect("import")
    }

    /// The export's tasks with the one field computed from the wall clock at
    /// export time taken out, so two exports of one state compare equal.
    fn stable_tasks(e: &Engine) -> Value {
        let mut doc = export(e);
        for t in doc["tasks"].as_array_mut().expect("tasks") {
            t.as_object_mut().expect("task").remove("urgency");
        }
        doc["tasks"].take()
    }

    #[test]
    fn a_merge_of_two_completions_keeps_one_next_occurrence() {
        let m = two_machines(true);
        let c_a = only_occurrence(&m.a, &m.r_id);
        let c_b = only_occurrence(&m.b, &m.r_id);
        assert_eq!(c_a["due"], c_b["due"], "the same next slot on both");

        let r = import(&m.a, &m.b, true);
        let kept = only_occurrence(&m.a, &m.r_id);
        assert_eq!(id_of(&kept), id_of(&c_a), "the older spawn survives");
        let dedup = r["deduplicated"].as_array().expect("deduplicated array");
        assert_eq!(dedup.len(), 1, "{r}");
        assert_eq!(dedup[0]["kept_id"], c_a["id"], "{r}");
        assert_eq!(dedup[0]["dropped_id"], c_b["id"], "{r}");
        assert_eq!(dedup[0]["kept"], c_a["short_id"], "{r}");
        let r_short = m.a.task_get(&json!({ "ref": m.r_id })).expect("R")["short_id"].clone();
        assert_eq!(dedup[0]["spawned_from"], r_short, "{r}");
        assert_eq!(dedup[0]["dropped_status"], json!("pending"), "{r}");
    }

    #[test]
    fn a_plain_import_of_two_completions_keeps_one_next_occurrence() {
        let m = two_machines(false);
        let c_b = only_occurrence(&m.b, &m.r_id);
        let r = import(&m.a, &m.b, false);
        let kept = only_occurrence(&m.a, &m.r_id);
        assert_eq!(id_of(&kept), id_of(&c_b), "the older spawn survives");
        assert_eq!(r["deduplicated"].as_array().map(Vec::len), Some(1), "{r}");
    }

    /// The survivor is chosen by id alone, so both stores keep the same one
    /// whichever direction runs first, for either survivor, plain or merged.
    #[test]
    fn both_directions_converge_on_the_same_occurrence() {
        for a_first in [true, false] {
            for merge in [false, true] {
                let m = two_machines(a_first);
                // A note of each machine's own on its copy: the dropped one's
                // moves, and the reverse import must not refuse it as a note
                // the other store holds under the copy it is about to drop.
                for (e, body) in [(&m.a, "written on A"), (&m.b, "written on B")] {
                    let c = id_of(&only_occurrence(e, &m.r_id));
                    e.annotation_add(&json!({ "ref": c, "body": body }))
                        .expect("note");
                }
                let oldest = std::cmp::min(
                    id_of(&only_occurrence(&m.a, &m.r_id)),
                    id_of(&only_occurrence(&m.b, &m.r_id)),
                );
                import(&m.a, &m.b, merge);
                let b_kept_its_own = id_of(&only_occurrence(&m.b, &m.r_id)) == oldest;
                let r = import(&m.b, &m.a, merge);
                // B folds only when A's survivor is not B's own copy.
                assert_eq!(
                    r["deduplicated"].as_array().map(Vec::len),
                    Some(usize::from(!b_kept_its_own)),
                    "{r}"
                );
                let in_a = only_occurrence(&m.a, &m.r_id);
                let in_b = only_occurrence(&m.b, &m.r_id);
                assert_eq!(id_of(&in_a), oldest, "a_first={a_first} merge={merge}");
                assert_eq!(id_of(&in_b), oldest, "a_first={a_first} merge={merge}");
                assert_eq!(
                    in_a["annotations"], in_b["annotations"],
                    "the folded notes agree: a_first={a_first} merge={merge}"
                );
                assert_eq!(
                    in_a["checks"], in_b["checks"],
                    "the folded checks agree: a_first={a_first} merge={merge}"
                );
            }
        }
    }

    /// Importing the other store's unchanged copy again brings the dropped
    /// occurrence back and folds it away by the same rule: no tombstone.
    #[test]
    fn re_importing_the_same_document_changes_nothing() {
        for merge in [false, true] {
            let m = two_machines(true);
            import(&m.a, &m.b, merge);
            let once = stable_tasks(&m.a);
            let r = import(&m.a, &m.b, merge);
            assert_eq!(r["deduplicated"].as_array().map(Vec::len), Some(1), "{r}");
            assert_eq!(stable_tasks(&m.a), once, "merge={merge}");
        }
    }

    #[test]
    fn the_dropped_copy_s_notes_tags_and_edges_move_to_the_survivor() {
        let m = two_machines(true);
        let c_a = id_of(&only_occurrence(&m.a, &m.r_id));
        let c_b = id_of(&only_occurrence(&m.b, &m.r_id));
        let blocker = m.b.task_add(&json!({ "title": "blocker" })).expect("add");
        let dependent = m.b.task_add(&json!({ "title": "dependent" })).expect("add");
        m.b.annotation_add(&json!({ "ref": c_b, "body": "written on B" }))
            .expect("note");
        m.b.tag_add(&json!({ "ref": c_b, "tags": ["from-b"] }))
            .expect("tag");
        m.b.dependency_add(&json!({ "ref": c_b, "depends_on": blocker["id"] }))
            .expect("edge out");
        m.b.dependency_add(&json!({ "ref": dependent["id"], "depends_on": c_b }))
            .expect("edge in");

        import(&m.a, &m.b, true);
        let kept = m.a.task_get(&json!({ "ref": c_a })).expect("survivor");
        let bodies: Vec<&str> = kept["annotations"]
            .as_array()
            .expect("notes")
            .iter()
            .filter_map(|n| n["body"].as_str())
            .collect();
        assert_eq!(
            bodies,
            ["What R is.", "written on B"],
            "the spawn's copied description is not doubled: {kept}"
        );
        assert_eq!(
            kept["checks"].as_array().map(Vec::len),
            Some(1),
            "the spawn's copied check is not doubled: {kept}"
        );
        assert_eq!(kept["tags"], json!(["from-b"]), "{kept}");
        assert_eq!(kept["depends_on"], json!([blocker["short_id"]]), "{kept}");
        let dep =
            m.a.task_get(&json!({ "ref": dependent["id"] }))
                .expect("dependent");
        assert_eq!(dep["depends_on"], json!([kept["short_id"]]), "{dep}");
        assert!(
            m.a.task_get(&json!({ "ref": c_b })).is_err(),
            "the duplicate row is gone"
        );
    }

    /// A copy the other machine already finished carries its progress onto
    /// the survivor: the status, the completion instant and the tracked time,
    /// and the next occurrence that completion spawned is re-pointed at the
    /// survivor. A round trip back must not undo any of it.
    #[test]
    fn a_finished_duplicate_s_progress_survives_the_round_trip() {
        for merge in [false, true] {
            let m = two_machines(true);
            let c_a = id_of(&only_occurrence(&m.a, &m.r_id));
            let c_b = id_of(&only_occurrence(&m.b, &m.r_id));
            m.a.task_adjust_tracked(&json!({ "ref": c_a, "delta": "+10m", "reason": "a" }))
                .expect("time on A");
            m.b.task_adjust_tracked(&json!({ "ref": c_b, "delta": "+20m", "reason": "b" }))
                .expect("time on B");
            m.b.task_done(&json!({ "ref": c_b })).expect("done C on B");
            let g_b = id_of(&only_occurrence(&m.b, &c_b));

            let r = import(&m.a, &m.b, merge);
            assert_eq!(r["deduplicated"][0]["dropped_status"], json!("done"), "{r}");
            assert_eq!(r["deduplicated"][0]["status"], json!("done"), "{r}");
            import(&m.b, &m.a, merge);

            for (name, e) in [("a", &m.a), ("b", &m.b)] {
                let kept = only_occurrence(e, &m.r_id);
                assert_eq!(id_of(&kept), c_a, "{name}");
                assert_eq!(kept["status"], json!("done"), "{name}: {kept}");
                assert!(kept["completed"].is_string(), "{name}: {kept}");
                assert_eq!(kept["tracked_seconds"], json!(1800), "{name}: {kept}");
                assert_eq!(
                    id_of(&only_occurrence(e, &c_a)),
                    g_b,
                    "{name}: the grandchild follows the survivor"
                );
                let pending = export(e)["tasks"]
                    .as_array()
                    .expect("tasks")
                    .iter()
                    .filter(|t| t["title"] == "R" && t["status"] == "pending")
                    .count();
                assert_eq!(pending, 1, "{name} merge={merge}");
            }
        }
    }

    /// Both copies completed: each spawned a grandchild on the same date,
    /// and those fold too, down the chain until nothing is left to fold.
    #[test]
    fn grandchildren_of_two_finished_copies_fold_as_well() {
        let m = two_machines(true);
        for e in [&m.a, &m.b] {
            let c = id_of(&only_occurrence(e, &m.r_id));
            e.task_done(&json!({ "ref": c })).expect("done C");
        }
        let r = import(&m.a, &m.b, true);
        assert_eq!(r["deduplicated"].as_array().map(Vec::len), Some(2), "{r}");
        let c = id_of(&only_occurrence(&m.a, &m.r_id));
        only_occurrence(&m.a, &c);
    }

    /// The fold moved a note onto the survivor and bumped its `rev`, so the
    /// store's own export from before the fold is stale and the #177 guard
    /// refuses it instead of wiping the moved note.
    #[test]
    fn a_pre_fold_export_is_refused_as_stale() {
        let m = two_machines(true);
        let c_b = id_of(&only_occurrence(&m.b, &m.r_id));
        m.b.annotation_add(&json!({ "ref": c_b, "body": "written on B" }))
            .expect("note");
        // Only the survivor: R's own `rev` moves with the merge anyway.
        let c_a = only_occurrence(&m.a, &m.r_id);
        let before = json!({ "tasks": [c_a] });
        import(&m.a, &m.b, true);
        let err = m.a.store_import(&before).expect_err("stale");
        assert_eq!(
            err.code,
            crate::error::ErrorCode::Conflict,
            "{}",
            err.message
        );
        assert!(err.message.contains(&id_of(&c_a)), "{}", err.message);
    }

    /// A copy of `e` under the same ids, events and all.
    fn clone_store(e: &Engine) -> Engine {
        let c = Engine::open_in_memory().expect("open clone");
        c.store_import(&export(e)).expect("clone");
        c
    }

    /// Three stores fold C into B's copy and then into A's, or C straight
    /// into A's, and must end with the same rows under the same ids.
    #[test]
    fn every_fold_path_through_three_stores_ends_the_same() {
        for order in [[0usize, 1, 2], [2, 1, 0], [1, 2, 0]] {
            let origin = Engine::open_in_memory().expect("open origin");
            let r = origin
                .task_add(&json!({ "title": "R", "recurrence": "every 3 days", "due": DUE }))
                .expect("add R");
            let r_id = id_of(&r);
            let stores = [
                clone_store(&origin),
                clone_store(&origin),
                clone_store(&origin),
            ];
            for i in order {
                stores[i]
                    .task_done(&json!({ "ref": r_id }))
                    .expect("done R");
            }
            for (i, e) in stores.iter().enumerate() {
                let c = id_of(&only_occurrence(e, &r_id));
                e.annotation_add(&json!({ "ref": c, "body": format!("note {i}") }))
                    .expect("note");
                e.token_add(&json!({
                    "ref": c, "tool": "t", "source": "self-report", "confidence": "low",
                    "input_tokens": 100 * (i + 1),
                }))
                .expect("token");
                e.task_adjust_tracked(&json!({
                    "ref": c, "delta": format!("+{}m", 10 * (i + 1)), "reason": "work",
                }))
                .expect("time");
            }
            let paths: [&[(usize, usize)]; 4] = [
                &[(0, 1), (0, 2)],
                &[(1, 2), (0, 1)],
                &[(2, 1), (0, 2)],
                &[(1, 2), (0, 1), (0, 2)],
            ];
            let mut ends = Vec::new();
            for path in paths {
                let copies: Vec<Engine> = stores.iter().map(clone_store).collect();
                for &(into, from) in path {
                    import(&copies[into], &copies[from], true);
                }
                let mut kept = only_occurrence(&copies[0], &r_id);
                let obj = kept.as_object_mut().expect("task");
                // Per-store: the clock, the revision counter and the display
                // number (D177 renumbers freely); the id and rows must agree.
                for volatile in ["urgency", "_rev", "modified", "short_id"] {
                    obj.remove(volatile);
                }
                assert_eq!(
                    kept["annotations"].as_array().map(Vec::len),
                    Some(3),
                    "{kept}"
                );
                assert_eq!(kept["tokens"].as_array().map(Vec::len), Some(3), "{kept}");
                assert_eq!(kept["tracked_seconds"], json!(3600), "{kept}");
                ends.push(kept);
            }
            for end in &ends[1..] {
                assert_eq!(end, &ends[0], "order={order:?}");
            }
        }
    }

    /// Completing R again after a reopen lands on the slot the first
    /// completion already spawned, so it spawns nothing: one store never
    /// makes a same-date pair of its own, and its own export folds nothing.
    #[test]
    fn reopen_and_done_again_spawns_no_second_occurrence() {
        for merge in [false, true] {
            let a = Engine::open_in_memory().expect("open a");
            let r = a
                .task_add(&json!({ "title": "R", "recurrence": "every 3 days", "due": DUE }))
                .expect("add R");
            let r_id = id_of(&r);
            a.task_done(&json!({ "ref": r_id })).expect("done");
            a.task_reopen(&json!({ "ref": r_id })).expect("reopen");
            let again = a.task_done(&json!({ "ref": r_id })).expect("done again");
            assert!(again.get("spawned").is_none(), "{again}");
            assert_eq!(occurrences(&a, &r_id).len(), 1, "merge={merge}");
            let res = import(&a, &a, merge);
            assert_eq!(res["deduplicated"], json!([]), "{res}");
        }
    }

    /// Two copies wired into opposite ends of one chain close a cycle once
    /// they are one task. The survivor's closing edge is dropped and named,
    /// rather than the import refused, so the two stores can still sync.
    #[test]
    fn a_fold_that_would_close_a_cycle_drops_the_edge() {
        let b = Engine::open_in_memory().expect("open b");
        let r = b
            .task_add(&json!({ "title": "R", "recurrence": "every 3 days", "due": DUE }))
            .expect("add R");
        let r_id = id_of(&r);
        let x = b.task_add(&json!({ "title": "X" })).expect("add X");
        let x_id = id_of(&x);
        let a = clone_store(&b);
        a.task_done(&json!({ "ref": r_id })).expect("done on A");
        b.task_done(&json!({ "ref": r_id })).expect("done on B");
        let c_a = id_of(&only_occurrence(&a, &r_id));
        let c_b = id_of(&only_occurrence(&b, &r_id));
        a.dependency_add(&json!({ "ref": x_id, "depends_on": c_a }))
            .expect("X after C on A");
        b.dependency_add(&json!({ "ref": c_b, "depends_on": x_id }))
            .expect("C after X on B");

        let res = import(&a, &b, true);
        let fold = &res["deduplicated"][0];
        assert_eq!(
            fold["dropped_dependencies"],
            json!([x["short_id"]]),
            "{res}"
        );
        import(&b, &a, true);
        for e in [&a, &b] {
            let kept = e.task_get(&json!({ "ref": c_a })).expect("survivor");
            assert_eq!(kept["depends_on"], json!([]), "{kept}");
            let x_now = e.task_get(&json!({ "ref": x_id })).expect("X");
            assert_eq!(x_now["depends_on"], json!([kept["short_id"]]), "{x_now}");
        }
    }

    /// A second row on `task`'s slot, as a store written before spawning
    /// checked for one could hold.
    fn legacy_twin(e: &Engine, task: &str) -> String {
        let twin = crate::clock::uuid_v7().to_string();
        e.conn
            .execute_batch(&format!(
                "CREATE TEMP TABLE twin AS SELECT * FROM tasks WHERE id = '{task}'; \
                 UPDATE twin SET id = '{twin}', short_id = 1000; \
                 INSERT INTO tasks SELECT * FROM twin; DROP TABLE twin;"
            ))
            .expect("insert the legacy twin");
        twin
    }

    /// The key is what an occurrence IS, so a same-slot pair already in a
    /// store is a duplicate like any other and the next import folds it,
    /// whatever that import brings, and says so. Every store therefore
    /// reaches the same one, where one that held a legacy pair used to keep
    /// it while the other refolded on every import.
    #[test]
    fn a_legacy_local_pair_is_folded_by_any_import() {
        let a = Engine::open_in_memory().expect("open a");
        let r = a
            .task_add(&json!({ "title": "R", "recurrence": "every 3 days", "due": DUE }))
            .expect("add R");
        let r_id = id_of(&r);
        a.task_done(&json!({ "ref": r_id })).expect("done");
        let c = id_of(&only_occurrence(&a, &r_id));
        legacy_twin(&a, &c);
        assert_eq!(occurrences(&a, &r_id).len(), 2, "the legacy pair");
        let other = Engine::open_in_memory().expect("open other");
        other
            .task_add(&json!({ "title": "unrelated" }))
            .expect("add");
        let res = import(&a, &other, true);
        assert_eq!(
            res["deduplicated"].as_array().map(Vec::len),
            Some(1),
            "{res}"
        );
        assert_eq!(id_of(&only_occurrence(&a, &r_id)), c);

        // Two machines, B holding a legacy pair: both converge on one.
        for merge in [false, true] {
            let m = two_machines(false);
            let c_b = id_of(&only_occurrence(&m.b, &m.r_id));
            legacy_twin(&m.b, &c_b);
            import(&m.a, &m.b, merge);
            import(&m.b, &m.a, merge);
            import(&m.a, &m.b, merge);
            let in_a = id_of(&only_occurrence(&m.a, &m.r_id));
            let in_b = id_of(&only_occurrence(&m.b, &m.r_id));
            assert_eq!(in_a, in_b, "merge={merge}");
        }
    }

    #[test]
    fn a_dry_run_reports_the_fold_and_writes_nothing() {
        let m = two_machines(true);
        let before = stable_tasks(&m.a);
        let mut doc = export(&m.b);
        doc["merge"] = json!(true);
        doc["dry_run"] = json!(true);
        let r = m.a.store_import(&doc).expect("dry run");
        assert_eq!(r["deduplicated"].as_array().map(Vec::len), Some(1), "{r}");
        assert_eq!(stable_tasks(&m.a), before);
    }

    fn adjust(e: &Engine, task: &str, delta: &str) {
        e.task_adjust_tracked(&json!({ "ref": task, "delta": delta, "reason": "work" }))
            .expect("adjust");
    }

    fn tracked(e: &Engine, parent: &str) -> Value {
        only_occurrence(e, parent)["tracked_seconds"].clone()
    }

    /// Two copies with 10m and 20m, merged back and forth: each store counts
    /// the 30 minutes once, however many times the fold runs again.
    #[test]
    fn repeated_round_trips_do_not_count_time_twice() {
        for merge in [false, true] {
            let m = two_machines(true);
            adjust(&m.a, &id_of(&only_occurrence(&m.a, &m.r_id)), "+10m");
            adjust(&m.b, &id_of(&only_occurrence(&m.b, &m.r_id)), "+20m");
            import(&m.a, &m.b, merge);
            import(&m.b, &m.a, merge);
            import(&m.a, &m.b, merge);
            import(&m.b, &m.a, merge);
            for e in [&m.a, &m.b] {
                assert_eq!(tracked(e, &m.r_id), json!(1800), "merge={merge}");
            }
        }
    }

    /// B logs more time on its copy after A folded it. The next import of B
    /// into A brings that copy back, and its new time must land on the
    /// survivor instead of being taken as already counted.
    #[test]
    fn work_on_a_copy_after_it_was_folded_elsewhere_is_counted() {
        for merge in [false, true] {
            let m = two_machines(true);
            adjust(&m.a, &id_of(&only_occurrence(&m.a, &m.r_id)), "+10m");
            let c_b = id_of(&only_occurrence(&m.b, &m.r_id));
            adjust(&m.b, &c_b, "+20m");
            import(&m.a, &m.b, merge);
            adjust(&m.b, &c_b, "+5m");
            import(&m.a, &m.b, merge);
            assert_eq!(tracked(&m.a, &m.r_id), json!(2100), "a, merge={merge}");
            import(&m.b, &m.a, merge);
            assert_eq!(tracked(&m.b, &m.r_id), json!(2100), "b, merge={merge}");
            import(&m.a, &m.b, merge);
            assert_eq!(
                tracked(&m.a, &m.r_id),
                json!(2100),
                "a again, merge={merge}"
            );
        }
    }
}
