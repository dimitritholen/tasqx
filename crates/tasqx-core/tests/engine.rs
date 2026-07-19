//! Core behavioral tests (the ones the brief requires):
//!  * storage round-trip
//!  * lifecycle transitions + an invalid transition => conflict
//!  * the same-transaction event-log invariant (commit => exactly one event;
//!    rollback => no event)
//!  * an end-to-end envelope integration test

use rusqlite::params;
use serde_json::json;
use tasqx_core::{dispatch, handle_envelope, storage, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn count(engine: &Engine, sql: &str) -> i64 {
    engine.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

// ---- storage round-trip -----------------------------------------------------

#[test]
fn storage_round_trip() {
    let e = engine();
    // D23: an explicit project must name a live project row.
    e.project_create(&json!({ "name": "work" })).expect("init work");
    let added = e
        .task_add(&json!({ "title": "hello world", "priority": "H", "project": "work" }))
        .expect("add");
    let short_id = added["short_id"].as_i64().unwrap();

    let listed = e.task_list(&json!({ "filter": "" })).expect("list");
    assert_eq!(listed["count"], 1);
    let t = &listed["tasks"][0];
    assert_eq!(t["short_id"], short_id);
    assert_eq!(t["title"], "hello world");
    assert_eq!(t["status"], "pending");
    assert_eq!(t["priority"], "H");
    assert_eq!(t["project"], "work");
}

// ---- lifecycle --------------------------------------------------------------

#[test]
fn lifecycle_start_stop_done() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    let started = e.task_start(&json!({ "ref": sid })).unwrap();
    assert_eq!(started["status"], "active");

    let stopped = e.task_stop(&json!({ "ref": sid })).unwrap();
    assert_eq!(stopped["status"], "pending");

    let done = e.task_done(&json!({ "ref": sid })).unwrap();
    assert_eq!(done["status"], "done");
    assert!(done["completed"].is_string());
}

#[test]
fn invalid_transition_returns_conflict() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    // Complete it, then try to stop a done task => conflict.
    e.task_done(&json!({ "ref": sid })).unwrap();
    let err = e.task_stop(&json!({ "ref": sid })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);

    // And starting a done task is also a conflict.
    let err2 = e.task_start(&json!({ "ref": sid })).unwrap_err();
    assert_eq!(err2.code, ErrorCode::Conflict);
}

#[test]
fn start_autostops_previous_active() {
    // D6: single active by default.
    let e = engine();
    let a = e.task_add(&json!({ "title": "a" })).unwrap()["short_id"].clone();
    let b = e.task_add(&json!({ "title": "b" })).unwrap()["short_id"].clone();

    e.task_start(&json!({ "ref": a })).unwrap();
    e.task_start(&json!({ "ref": b })).unwrap();

    // Exactly one active task, and it is b.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks WHERE status='active'"), 1);
    let a_status: String = e
        .conn()
        .query_row(
            "SELECT status FROM tasks WHERE short_id = ?1",
            params![a.as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(a_status, "pending");
}

#[test]
fn wait_in_future_starts_in_backlog() {
    let e = engine();
    let r = e
        .task_add(&json!({ "title": "later", "wait": "2999-01-01T00:00:00Z" }))
        .unwrap();
    assert_eq!(r["status"], "backlog");
}

// ---- the same-transaction event-log invariant -------------------------------

#[test]
fn every_mutation_writes_exactly_one_event() {
    let e = engine();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), 0);

    e.task_add(&json!({ "title": "t" })).unwrap();
    // task.add => exactly one event, of op 'add' on entity 'task'.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), 1);
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM events WHERE entity='task' AND op='add'"
        ),
        1
    );

    // A single start (no other active task) => exactly one more event.
    let sid = 1i64;
    e.task_start(&json!({ "ref": sid })).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), 2);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events WHERE op='start'"), 1);
}

#[test]
fn failed_mutation_leaves_no_event() {
    let e = engine();
    let before = count(&e, "SELECT COUNT(*) FROM events");
    let tasks_before = count(&e, "SELECT COUNT(*) FROM tasks");

    // A mutation that fails validation (nonexistent ref) writes nothing.
    let err = e.task_start(&json!({ "ref": 999 })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), before);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), tasks_before);
}

#[test]
fn rolled_back_transaction_couples_state_and_event() {
    // Directly demonstrate the coupling: within one transaction we insert a
    // task AND its event, then roll back (drop without commit). Both vanish.
    let e = engine();
    {
        let tx = e.conn().unchecked_transaction().unwrap();
        let short_id = storage::alloc_short_id(&tx).unwrap();
        tx.execute(
            "INSERT INTO tasks (id, short_id, title, status, urgency, tracked_seconds, rev, created, modified) \
             VALUES ('rollback-id', ?1, 'doomed', 'pending', 0, 0, 1, 'now', 'now')",
            params![short_id],
        )
        .unwrap();
        storage::insert_event(&tx, "task", "rollback-id", "add", &json!({})).unwrap();
        // Both rows exist inside the tx...
        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // ...tx dropped here without commit => rollback.
    }
    // Nothing survived: state and history rolled back together.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), 0);

    // And a committed transaction keeps exactly one of each.
    e.task_add(&json!({ "title": "kept" })).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), 1);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), 1);
}

// ---- end-to-end envelope ----------------------------------------------------

#[test]
fn envelope_end_to_end() {
    let e = engine();

    // project.create
    let resp = handle_envelope(
        &e,
        r#"{"tasqx":"1","id":"p1","method":"project.create","params":{"name":"work.tasqx"}}"#,
    );
    assert_eq!(resp["tasqx"], "1");
    assert_eq!(resp["id"], "p1");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["name"], "work.tasqx");

    // task.add
    let resp = handle_envelope(
        &e,
        r#"{"tasqx":"1","id":"t1","method":"task.add","params":{"title":"Ship v1","project":"work.tasqx","priority":"H","tags":["release"]}}"#,
    );
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["status"], "pending");
    let sid = resp["result"]["short_id"].as_i64().unwrap();

    // a second task, so we can prove auto-stop
    let resp2 = handle_envelope(
        &e,
        r#"{"tasqx":"1","id":"t2","method":"task.add","params":{"title":"Second"}}"#,
    );
    let sid2 = resp2["result"]["short_id"].as_i64().unwrap();

    // task.list (filter subset: project + tag)
    let resp = handle_envelope(
        &e,
        r#"{"tasqx":"1","id":"q1","method":"task.list","params":{"filter":"project:work.tasqx +release status:pending"}}"#,
    );
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["count"], 1);
    assert_eq!(resp["result"]["tasks"][0]["short_id"], sid);

    // task.start on the first, then start the second => first auto-stops.
    let _ = handle_envelope(
        &e,
        &format!(r#"{{"tasqx":"1","id":"s1","method":"task.start","params":{{"ref":{sid}}}}}"#),
    );
    let start2 = handle_envelope(
        &e,
        &format!(r#"{{"tasqx":"1","id":"s2","method":"task.start","params":{{"ref":{sid2}}}}}"#),
    );
    assert_eq!(start2["result"]["status"], "active");

    // The first task is back to pending (auto-stopped).
    let list_active = handle_envelope(
        &e,
        r#"{"tasqx":"1","id":"q2","method":"task.list","params":{"filter":"status:active"}}"#,
    );
    assert_eq!(list_active["result"]["count"], 1);
    assert_eq!(list_active["result"]["tasks"][0]["short_id"], sid2);

    // task.done on the first => done, unblocked present (trivially empty).
    let done = handle_envelope(
        &e,
        &format!(r#"{{"tasqx":"1","id":"d1","method":"task.done","params":{{"ref":{sid}}}}}"#),
    );
    assert_eq!(done["result"]["status"], "done");
    assert!(done["result"]["unblocked"].is_array());
    assert_eq!(done["result"]["unblocked"].as_array().unwrap().len(), 0);
}

#[test]
fn unsupported_version_envelope() {
    let e = engine();
    let resp = handle_envelope(
        &e,
        r#"{"tasqx":"2","id":"x","method":"task.list","params":{}}"#,
    );
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "unsupported_version");
}

#[test]
fn unknown_method_is_bad_request() {
    let e = engine();
    let err = dispatch(&e, "task.teleport", &json!({})).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn engine_mutation_rolls_back_state_and_event_together() {
    // Force a fault AFTER the first write *inside* an engine mutation: task_add
    // advances the meta counter (alloc_short_id) and then INSERTs the task. By
    // rewinding the counter to a short_id that already exists, the task INSERT
    // trips UNIQUE(short_id) *after* the counter write — proving the whole
    // mutation (counter + state + event) rolls back as one unit, against real
    // engine code rather than hand-written SQL.
    let e = engine();
    e.task_add(&json!({ "title": "first" })).unwrap(); // short_id 1, counter -> 2

    let tasks_before = count(&e, "SELECT COUNT(*) FROM tasks");
    let events_before = count(&e, "SELECT COUNT(*) FROM events");

    // Rewind so the next add re-mints short_id 1 => UNIQUE clash mid-transaction.
    e.conn()
        .execute("UPDATE meta SET value = 1 WHERE key = 'next_short_id'", [])
        .unwrap();

    let err = e.task_add(&json!({ "title": "doomed" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal); // UNIQUE violation => storage error

    // Nothing partial survived: no new task, no new event, and the counter
    // itself rolled back to 1 (not the incremented 2).
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), tasks_before);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), events_before);
    let counter: i64 = e
        .conn()
        .query_row("SELECT value FROM meta WHERE key = 'next_short_id'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(counter, 1);
}

// ---- task.modify status is not a lifecycle backdoor -------------------------

#[test]
fn modify_status_only_permits_cancel() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    // Arbitrary lifecycle jumps via modify are rejected — no backdoor around
    // the start/stop/done state machine.
    let bad = e
        .task_modify(&json!({ "ref": sid, "set": { "status": "active" } }))
        .unwrap_err();
    assert_eq!(bad.code, ErrorCode::BadRequest);
    let bad2 = e
        .task_modify(&json!({ "ref": sid, "set": { "status": "done" } }))
        .unwrap_err();
    assert_eq!(bad2.code, ErrorCode::BadRequest);

    // Cancellation is the one sanctioned status edit (DESIGN §7).
    let ok = e
        .task_modify(&json!({ "ref": sid, "set": { "status": "cancelled" } }))
        .unwrap();
    assert!(ok["_rev"].is_i64());
    let st: String = e
        .conn()
        .query_row(
            "SELECT status FROM tasks WHERE short_id = ?1",
            params![sid.as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(st, "cancelled");

    // Cancelling an already-terminal task is a conflict.
    let conflict = e
        .task_modify(&json!({ "ref": sid, "set": { "status": "cancelled" } }))
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::Conflict);
}

#[test]
fn modify_cancel_closes_active_interval() {
    // Cancelling a running task must close its open interval and clear
    // active_since, exactly as stop/done do — leaving no orphan `active` state.
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_start(&json!({ "ref": sid })).unwrap();

    e.task_modify(&json!({ "ref": sid, "set": { "status": "cancelled" } })).unwrap();

    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks WHERE status='active'"), 0);
    let active_since: Option<String> = e
        .conn()
        .query_row(
            "SELECT active_since FROM tasks WHERE short_id = ?1",
            params![sid.as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(active_since.is_none());
}

// ---- B1: backlog -> pending when the wait/scheduled instant passes ----------

/// DESIGN's `backlog --> pending: wait/schedule reached` transition, from the
/// only angle a user has: what the read surfaces say. A task parked behind a
/// future `wait` was invisible forever — moving the wait into the past left it
/// in `backlog`, out of the default view, with no verb able to release it.
#[test]
fn a_passed_wait_releases_the_task_to_pending() {
    let e = engine();
    let sid = e
        .task_add(&json!({ "title": "waiter", "wait": "2999-01-01T00:00:00Z" }))
        .unwrap()["short_id"]
        .clone();

    e.task_modify(&json!({ "ref": sid, "set": { "wait": "2020-01-01T00:00:00Z" } })).unwrap();

    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    assert_eq!(got["status"], "pending", "task.get must report the released status");

    let listed = e.task_list(&json!({ "filter": "" })).unwrap();
    assert_eq!(listed["tasks"][0]["status"], "pending", "task.list must agree with task.get");

    // And it must be startable — the guard reads the same status the user sees.
    assert_eq!(e.task_start(&json!({ "ref": sid })).unwrap()["status"], "active");
}

/// A future `scheduled` holds the task in the backlog just like `wait` does,
/// and neither release happens while the instant is still ahead.
#[test]
fn a_future_scheduled_still_holds_the_task_in_backlog() {
    let e = engine();
    let sid = e
        .task_add(&json!({ "title": "soon", "scheduled": "2999-01-01T00:00:00Z" }))
        .unwrap()["short_id"]
        .clone();
    assert_eq!(e.task_get(&json!({ "ref": sid })).unwrap()["status"], "backlog");

    e.task_modify(&json!({ "ref": sid, "set": { "scheduled": "2020-01-01T00:00:00Z" } })).unwrap();
    assert_eq!(e.task_get(&json!({ "ref": sid })).unwrap()["status"], "pending");
}

/// The recurrence spawn computes the same rule on the shifted timestamps, so it
/// must reach the same answer: an instance whose shifted `wait` has already
/// passed is actionable, not parked.
#[test]
fn a_spawned_instance_is_parked_or_released_by_its_shifted_wait() {
    // `next_after` collapses missed slots, so the spawned `due` always lands
    // within one day *after* now. The wait rides along at the same offset, so
    // the offset alone decides the answer — no wall clock in the assertion.
    for (wait, expect) in
        [("2020-01-08T12:00:00Z", "pending"), ("2020-01-15T12:00:00Z", "backlog")]
    {
        let e = engine();
        let sid = e
            .task_add(&json!({
                "title": "daily",
                "due": "2020-01-10T12:00:00Z",   // wait is due-2d, then due+5d
                "wait": wait,
                "recurrence": "every 1 days",
            }))
            .unwrap()["short_id"]
            .clone();
        e.task_done(&json!({ "ref": sid })).unwrap();

        let listed = e.task_list(&json!({ "filter": format!("status:{expect}") })).unwrap();
        assert_eq!(listed["count"], 1, "wait {wait} => {expect}, got {listed}");
    }
}
