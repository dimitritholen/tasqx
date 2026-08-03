//! Core behavioral tests (the ones the brief requires):
//!  * storage round-trip
//!  * lifecycle transitions + an invalid transition => conflict
//!  * the same-transaction event-log invariant (commit => exactly one event;
//!    rollback => no event), per-op for the dependency edges and as a
//!    whole-surface floor for every handler that opens a mutation
//!  * an end-to-end envelope integration test

use std::collections::BTreeSet;

use rusqlite::params;
use serde_json::{json, Value};
use tasqx_core::engine::{NOT_UNDOABLE, UNDOABLE_OPS};
use tasqx_core::{dispatch, handle_envelope, storage, Engine, ErrorCode};

/// Every file carrying `impl Engine` methods. `include_str!` makes each one a
/// rebuild dependency, so no scan below can read a stale copy.
///
/// Module-level rather than inside the one test that used to own it, because
/// two guards now read it — the mutation/event coupling scan and the undo
/// vocabulary scan — and two copies of "which files hold the engine" is exactly
/// the drift that once left `engine/tokens.rs` and `engine/reports.rs` unscanned
/// for as long as they had existed.
const SOURCES: [(&str, &str); 9] = [
    ("engine.rs", include_str!("../src/engine.rs")),
    ("engine/memory.rs", include_str!("../src/engine/memory.rs")),
    (
        "engine/projects.rs",
        include_str!("../src/engine/projects.rs"),
    ),
    (
        "engine/relationships.rs",
        include_str!("../src/engine/relationships.rs"),
    ),
    (
        "engine/reports.rs",
        include_str!("../src/engine/reports.rs"),
    ),
    ("engine/task.rs", include_str!("../src/engine/task.rs")),
    ("engine/tokens.rs", include_str!("../src/engine/tokens.rs")),
    (
        "engine/transfer.rs",
        include_str!("../src/engine/transfer.rs"),
    ),
    ("engine/undo.rs", include_str!("../src/engine/undo.rs")),
];

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
    e.project_create(&json!({ "name": "work" }))
        .expect("init work");
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
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM tasks WHERE status='active'"),
        1
    );
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

/// Two handlers out of the twenty-three Engine methods that open a mutation
/// transaction. The name used to promise "every mutation", which is what let
/// `dependency.add` and `dependency.remove` ship with no event assertion
/// anywhere in the workspace — a test that claims a surface it never walks
/// stops anyone looking for the gap. The whole-surface floor lives in
/// `every_handler_that_opens_a_mutation_also_appends_an_event` below, which now
/// reaches all twenty-three; it read six of the eight engine files until a doc
/// audit found the comment claiming otherwise. The per-op payload assertions
/// live beside the behaviour they belong to.
#[test]
fn task_add_and_task_start_each_write_exactly_one_event() {
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
        storage::insert_event(
            &tx,
            tasqx_core::Entity::Task,
            "rollback-id",
            "add",
            &json!({}),
        )
        .unwrap();
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
        .query_row(
            "SELECT value FROM meta WHERE key = 'next_short_id'",
            [],
            |r| r.get(0),
        )
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

    e.task_modify(&json!({ "ref": sid, "set": { "status": "cancelled" } }))
        .unwrap();

    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM tasks WHERE status='active'"),
        0
    );
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

    e.task_modify(&json!({ "ref": sid, "set": { "wait": "2020-01-01T00:00:00Z" } }))
        .unwrap();

    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    assert_eq!(
        got["status"], "pending",
        "task.get must report the released status"
    );

    let listed = e.task_list(&json!({ "filter": "" })).unwrap();
    assert_eq!(
        listed["tasks"][0]["status"], "pending",
        "task.list must agree with task.get"
    );

    // And it must be startable — the guard reads the same status the user sees.
    assert_eq!(
        e.task_start(&json!({ "ref": sid })).unwrap()["status"],
        "active"
    );
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
    assert_eq!(
        e.task_get(&json!({ "ref": sid })).unwrap()["status"],
        "backlog"
    );

    e.task_modify(&json!({ "ref": sid, "set": { "scheduled": "2020-01-01T00:00:00Z" } }))
        .unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": sid })).unwrap()["status"],
        "pending"
    );
}

/// The recurrence spawn computes the same rule on the shifted timestamps, so it
/// must reach the same answer: an instance whose shifted `wait` has already
/// passed is actionable, not parked.
#[test]
fn a_spawned_instance_is_parked_or_released_by_its_shifted_wait() {
    // `next_after` collapses missed slots, so the spawned `due` always lands
    // within one day *after* now. The wait rides along at the same offset, so
    // the offset alone decides the answer — no wall clock in the assertion.
    for (wait, expect) in [
        ("2020-01-08T12:00:00Z", "pending"),
        ("2020-01-15T12:00:00Z", "backlog"),
    ] {
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

        let listed = e
            .task_list(&json!({ "filter": format!("status:{expect}") }))
            .unwrap();
        assert_eq!(listed["count"], 1, "wait {wait} => {expect}, got {listed}");
    }
}

// ---- H1: a present param of the wrong JSON type is refused, not ignored -----

/// THE BLOCKER, and the reason this whole family is one decision rather than
/// ten fixes: `expected_rev` was read with `.and_then(Value::as_i64)`, so a
/// stringified number — exactly what a JavaScript client sends — made the
/// optimistic-concurrency guard evaporate and the write proceed. A guard that
/// fails open is worse than no guard, because the caller believes it holds.
#[test]
fn a_stringified_expected_rev_conflicts_rather_than_overwriting() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "original" })).unwrap()["short_id"].clone();
    e.task_modify(&json!({ "ref": sid, "set": { "title": "bump" } }))
        .unwrap();

    let err = e
        .task_modify(&json!({ "ref": sid, "set": { "title": "LOST UPDATE" }, "expected_rev": "1" }))
        .expect_err("a stringified expected_rev must not silently skip the guard");
    assert_eq!(err.code, ErrorCode::BadRequest, "got {err:?}");
    assert!(
        err.message.contains("expected_rev"),
        "message must name the param: {}",
        err.message
    );

    // The write must not have landed.
    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    assert_eq!(got["title"], "bump", "the stale write overwrote the title");
    assert_eq!(got["_rev"], 1_i64 + 1);
}

/// An integer `expected_rev` keeps working on both sides — the guard must not
/// become stricter about the type it was always documented to take.
#[test]
fn an_integer_expected_rev_still_guards_and_still_passes() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_modify(&json!({ "ref": sid, "set": { "title": "two" } }))
        .unwrap();

    let stale = e
        .task_modify(&json!({ "ref": sid, "set": { "title": "x" }, "expected_rev": 1 }))
        .expect_err("stale rev must conflict");
    assert_eq!(stale.code, ErrorCode::Conflict);

    e.task_modify(&json!({ "ref": sid, "set": { "title": "three" }, "expected_rev": 2 }))
        .expect("a matching expected_rev must still succeed");
}

/// Every other instance of the same shape, driven through the real handlers.
/// Each pair is (method, params) where the params carry ONE wrong-typed value;
/// each must be a `bad_request` naming that param rather than a silent default.
#[test]
fn a_wrong_typed_param_is_a_bad_request_naming_it() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    let cases: Vec<(&str, serde_json::Value, &str)> = vec![
        // A non-string filter used to become the EMPTY filter, matching everything.
        ("task.list", json!({ "filter": 5 }), "filter"),
        ("report.summary", json!({ "filter": true }), "filter"),
        // A limit of the wrong type used to be dropped, returning every row.
        ("task.list", json!({ "limit": "2" }), "limit"),
        ("task.list", json!({ "limit": -1 }), "limit"),
        ("task.list", json!({ "limit": 2.7 }), "limit"),
        ("event.list", json!({ "limit": "5" }), "limit"),
        // `sort:"urgency"` (a bare string) used to leave the rows unsorted.
        ("task.list", json!({ "sort": "urgency" }), "sort"),
        // A non-string group_by used to fall back to the first summary key.
        ("report.summary", json!({ "group_by": 3 }), "group_by"),
        // "true" is not true: these three all silently meant `false`.
        (
            "project.list",
            json!({ "include_archived": "true" }),
            "include_archived",
        ),
        ("task.start", json!({ "ref": sid, "keep": "yes" }), "keep"),
        ("report.summary", json!({ "all": 1 }), "all"),
        // opt_str_array dropped a non-array, a non-string entry and an empty
        // string — and its two callers disagreed about which of those mattered.
        ("task.add", json!({ "title": "x", "tags": "home" }), "tags"),
        (
            "task.add",
            json!({ "title": "x", "tags": ["ok", 7] }),
            "tags",
        ),
        (
            "task.add",
            json!({ "title": "x", "tags": ["ok", ""] }),
            "tags",
        ),
        ("tag.add", json!({ "ref": sid, "tags": "home" }), "tags"),
        ("tag.add", json!({ "ref": sid, "tags": [7] }), "tags"),
        // A non-string ref, and a non-object `set`.
        ("task.modify", json!({ "ref": sid, "set": [] }), "set"),
        ("store.import", json!({ "tasks": {} }), "tasks"),
    ];

    for (method, params, key) in cases {
        let err = dispatch(&e, method, &params)
            .err()
            .unwrap_or_else(|| panic!("{method} {params} must be refused, not silently defaulted"));
        assert_eq!(
            err.code,
            ErrorCode::BadRequest,
            "{method} {params} => {err:?}"
        );
        assert!(
            err.message.contains(key),
            "{method} {params}: message must name `{key}`, got {}",
            err.message
        );
    }
}

/// Absent must stay absent. A stricter gate that turned optional params into
/// required ones would break every existing caller, so the defaults are pinned.
#[test]
fn an_absent_param_keeps_its_default() {
    let e = engine();
    e.task_add(&json!({ "title": "a" })).unwrap();
    let sid = e.task_add(&json!({ "title": "b" })).unwrap()["short_id"].clone();

    assert_eq!(e.task_list(&json!({})).unwrap()["count"], 2);
    e.task_start(&json!({ "ref": sid })).expect("keep absent");
    e.project_list(&json!({})).expect("include_archived absent");
    e.report_summary(&json!({})).expect("group_by/all absent");
    e.event_list(&json!({})).expect("limit absent");
    e.task_modify(&json!({ "ref": sid, "set": { "title": "b2" } }))
        .expect("expected_rev absent");
    // An explicit null is how a JS client spells "no value" — still absent.
    e.task_modify(&json!({ "ref": sid, "set": { "title": "b3" }, "expected_rev": null }))
        .expect("an explicit null expected_rev stays absent");
    assert_eq!(e.task_list(&json!({ "limit": null })).unwrap()["count"], 2);
}

/// D33. A misspelled params key was accepted and silently ignored, which on a
/// WRITE discards intent unfalsifiably: `task.add {"prioritee":"H"}` answered
/// `ok:true` and created a task with `priority: null`, and nothing anywhere
/// recorded that a priority had been asked for. Reads were the same story one
/// step milder (`limitt` returned an unlimited page that looks exactly like a
/// limited one).
///
/// Enforced at `dispatch`, the one seam every surface shares, so no method can
/// forget to check — the D31 "structurally unrepresentable" move applied to the
/// key set.
#[test]
fn an_unknown_params_key_is_refused_and_names_itself() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    let sid = e.task_add(&json!({ "title": "a" })).unwrap()["short_id"].clone();

    let cases = [
        (
            "task.add",
            json!({ "title": "b", "prioritee": "H" }),
            "prioritee",
            "priority",
        ),
        ("task.list", json!({ "limitt": 2 }), "limitt", "limit"),
        ("event.list", json!({ "bogus": 1 }), "bogus", "limit"),
        (
            "task.modify",
            json!({ "ref": sid, "set": { "title": "x" }, "expect_rev": 1 }),
            "expect_rev",
            "expected_rev",
        ),
        (
            "report.summary",
            json!({ "groupby": "status" }),
            "groupby",
            "group_by",
        ),
        (
            "project.create",
            json!({ "name": "x", "desc": "y" }),
            "desc",
            "description",
        ),
        ("store.export", json!({ "filtr": "" }), "filtr", "filter"),
        (
            "tag.add",
            json!({ "ref": sid, "tag": ["x"] }),
            "tag",
            "tags",
        ),
        (
            "task.start",
            json!({ "ref": sid, "kep": true }),
            "kep",
            "keep",
        ),
        (
            "core.capabilities",
            json!({ "anything": 1 }),
            "anything",
            "no params",
        ),
    ];

    for (method, params, bad, hint) in cases {
        let err = dispatch(&e, method, &params).err().unwrap_or_else(|| {
            panic!("{method} {params}: an unknown key must be refused, not ignored")
        });
        assert_eq!(
            err.code,
            ErrorCode::BadRequest,
            "{method} {params} => {err:?}"
        );
        assert!(
            err.message.contains(bad),
            "message must name `{bad}`: {}",
            err.message
        );
        assert!(
            err.message.contains(hint),
            "message must list the accepted set (expected `{hint}` in it): {}",
            err.message
        );
    }

    // The write really did not happen: the refusal is not cosmetic.
    assert_eq!(e.task_list(&json!({})).unwrap()["count"], 1);
}

/// D12 requires an export from a NEWER tasqx to stay readable by an older
/// binary, so the gate stops at the request surface: `store.import`'s params
/// object IS a data document, and a top-level key this build has never heard of
/// is a future field, not a typo. The required `tasks` key is what keeps that
/// tolerance safe — a misspelled `taskss` is still refused, by absence.
#[test]
fn the_import_document_tolerates_a_future_key_but_still_needs_tasks() {
    let e = engine();
    let payload = json!({
        "tasks": [{ "id": "019f6a0f-99df-7000-8000-000000000001", "short_id": 1, "title": "t" }],
        "dropped_dependencies": 0,
        "exported_by": "tasqx 2.0",
    });
    let r = dispatch(&e, "store.import", &payload).expect("a newer export must stay importable");
    assert_eq!(r["imported"], 1);

    let err = dispatch(&e, "store.import", &json!({ "taskss": [] })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("tasks"), "{}", err.message);
}

/// P1c — every per-task import error must name the task it came from.
///
/// `import_field`'s doc comment states why it exists: import processes many
/// tasks per request, so an error that does not name one does not say which of
/// a thousand exported rows to edit. Three fields — `estimate`, `recurrence`
/// and `remind` — called their extractor OUTSIDE the wrapper, so the PARSE
/// error for those fields kept the prefix while the EMPTY-STRING error for the
/// same field lost it. One field, two error paths, two different answers about
/// which task is broken.
///
/// The field list is DERIVED from `IMPORT_TASK_KEYS` (D30) rather than typed
/// here. A hand-written list is the shape that let three fields skip the
/// wrapper unnoticed in the first place, and it would let the fourth do the
/// same: a new key added to the export gate arrives in this test automatically.
///
/// The assertion is conditional by design — a key whose empty string is
/// legitimately ACCEPTED (or ignored, like the derived ones the gate lists so
/// it can tell "ignored" from "unknown") simply produces no error to inspect,
/// and no exemption list is needed to say so. What is guarded is the
/// implication: IF the field refuses, it names the task. The floor below stops
/// the guard from passing vacuously if a refactor made every field silent.
#[test]
fn every_import_field_error_names_its_task() {
    // `id` is the one recorded exception, and structurally so rather than by
    // taste: it is the value the prefix is BUILT from, so an error about `id`
    // itself has no task name to carry. It says so in its own words instead.
    const NAMES_NO_TASK: &str = "id";

    let mut refused = 0;
    for key in tasqx_core::engine::IMPORT_TASK_KEYS {
        if *key == NAMES_NO_TASK {
            continue;
        }
        let e = engine();
        // A minimal VALID task, with the field under test overwritten by the
        // empty string — the input D35 ruled is a value the caller sent, and
        // the one whose refusal was skipping the wrapper.
        let mut task = json!({ "id": "t1", "short_id": 1, "title": "A" });
        task[*key] = json!("");
        let r = dispatch(&e, "store.import", &json!({ "tasks": [task] }));
        let Err(err) = r else { continue };
        refused += 1;
        assert!(
            err.message.contains("task t1"),
            "`{key}` refused an empty string without naming the task to edit: {}",
            err.message
        );
    }
    // Every text-scanning or table-driven guard's failure mode is matching
    // nothing. At the time of writing thirteen keys refuse; the floor is set
    // below that so adding a permissive key is not a test failure, while
    // gutting the refusals is.
    assert!(
        refused >= 10,
        "expected most import fields to refuse an empty string, got {refused}"
    );
}

/// P1c, the half that pins the fix rather than the symptom: for the three
/// fields that had two error paths, BOTH must carry the prefix.
///
/// The empty-string path was the broken one and the parse path was fine, so a
/// test of either alone agrees with the bug. This is the twin rule: one field,
/// two ways to be wrong, both asserted.
#[test]
fn a_bad_value_and_an_empty_value_name_the_task_alike() {
    for (key, unparseable) in [
        ("estimate", "3 fortnights"),
        ("recurrence", "every blue moon"),
        ("remind", "sometime"),
    ] {
        for value in ["", unparseable] {
            let e = engine();
            let mut task = json!({ "id": "t1", "short_id": 1, "title": "A" });
            task[key] = json!(value);
            let err = dispatch(&e, "store.import", &json!({ "tasks": [task] }))
                .expect_err(&format!("`{key}` = {value:?} must be refused"));
            assert!(
                err.message.contains("task t1"),
                "`{key}` = {value:?} must name the task: {}",
                err.message
            );
            assert!(
                err.message.contains(key),
                "`{key}` = {value:?} must name the field: {}",
                err.message
            );
        }
    }
}

// ---- tag.remove (D52) -------------------------------------------------------

/// A task carrying `api` and `release`, and the `{ref: …}` value every test
/// below re-reads it with.
fn tagged_task(e: &Engine) -> (Value, Value) {
    let task = e.task_add(&json!({ "title": "tagged" })).expect("add");
    let by_ref = json!({ "ref": task["short_id"].clone() });
    e.tag_add(&json!({ "ref": task["short_id"].clone(), "tags": ["api", "release"] }))
        .expect("tag.add");
    (task, by_ref)
}

/// The `tag.*` events on one task, newest first.
fn tag_events(e: &Engine, short_id: &Value) -> Vec<Value> {
    let listed = e
        .event_list(&json!({ "ref": short_id.clone() }))
        .expect("event.list");
    listed["events"]
        .as_array()
        .expect("event.list returns an events array")
        .iter()
        .filter(|ev| ev["op"].as_str().is_some_and(|op| op.starts_with("tag.")))
        .cloned()
        .collect()
}

/// The happy path, and the two facts the response has to carry: what is LEFT
/// (so the caller does not re-read) and what WENT (so `#42 tags: +release` is
/// distinguishable from the same line printed by a call that removed nothing).
///
/// The event is asserted here for the same reason `dependency_add` has its own
/// test above the whole-surface scan: the scan only sees that the source
/// contains `insert_event(`, so it cannot notice the op landing on the wrong row
/// or under the wrong name, and `tag.remove` reaching the log as `tag.add` would
/// make the audit trail claim the opposite of what happened.
#[test]
fn tag_remove_drops_the_tag_reports_both_sides_and_logs_one_event() {
    let e = engine();
    let (task, by_ref) = tagged_task(&e);
    let rev_before = e.task_get(&by_ref).unwrap()["_rev"].as_i64().unwrap();

    let out = e
        .tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": ["api"] }))
        .expect("tag.remove");

    assert_eq!(out["tags"], json!(["release"]), "the set that remains");
    assert_eq!(out["removed"], json!(["api"]), "what this call took away");
    assert_eq!(out["short_id"], task["short_id"]);
    assert_eq!(
        e.task_get(&by_ref).unwrap()["tags"],
        json!(["release"]),
        "the removal must be in the store, not only in the response"
    );
    assert_eq!(
        e.task_get(&by_ref).unwrap()["_rev"],
        json!(rev_before + 1),
        "a real edit must bump `rev`, or a client's expected_rev is stale-safe by accident"
    );

    let logged = tag_events(&e, &task["short_id"]);
    assert_eq!(
        logged.len(),
        2,
        "add then remove => two edits, got {logged:#?}"
    );
    assert_eq!(logged[0]["op"], "tag.remove");
    assert_eq!(logged[0]["entity"], "task");
    assert_eq!(logged[0]["entity_id"], task["id"]);
    assert_eq!(logged[0]["payload"]["tags"], json!(["api"]));
}

/// D52(a). The refusal, and the whole reason `tag.remove` does not copy
/// `dependency.remove`'s no-op-answers-ok shape: `untag 42 blockign` states an
/// intent, and an `ok` carrying a tag set that still holds `blocking` is
/// indistinguishable from success. The message must therefore name the tag the
/// task does NOT have and the ones it does — a refusal that only says "no" sends
/// the caller to `tasqx show` to find their own typo.
///
/// The `not_found` code is asserted, not just the error-ness: it is what the CLI
/// turns into exit 4, which is the only thing a script can branch on.
#[test]
fn removing_a_tag_the_task_lacks_is_not_found_and_names_both_sides() {
    let e = engine();
    let (task, by_ref) = tagged_task(&e);
    let rev_before = e.task_get(&by_ref).unwrap()["_rev"].clone();
    let events_before = count(&e, "SELECT COUNT(*) FROM events");

    let err = e
        .tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": ["blockign"] }))
        .expect_err("a tag the task does not carry may not answer ok");

    assert_eq!(err.code, ErrorCode::NotFound, "{}", err.message);
    assert!(
        err.message.contains("blockign"),
        "the refusal must name the tag that was asked for: {}",
        err.message
    );
    assert!(
        err.message.contains("api") && err.message.contains("release"),
        "the refusal must name the tags the task DOES have, or the typo stays hidden: {}",
        err.message
    );
    assert_eq!(
        e.task_get(&by_ref).unwrap()["tags"],
        json!(["api", "release"]),
        "a refused removal must leave every tag in place"
    );
    assert_eq!(
        e.task_get(&by_ref).unwrap()["_rev"],
        rev_before,
        "a refused removal bumped `rev`, invalidating a client's expected_rev for nothing"
    );
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events"),
        events_before,
        "a refused removal appended an event the daemon would push as a change"
    );
}

/// D52(b). All-or-nothing: one unknown tag in the list takes the whole call
/// down, and the tags that WERE there are still there afterwards. Without the
/// pre-check inside the transaction the loop would delete `api`, hit nothing for
/// `blockign`, and commit a half-applied removal at `ok` — leaving the caller to
/// work out which half landed.
#[test]
fn one_unknown_tag_removes_none_of_them() {
    let e = engine();
    let (task, by_ref) = tagged_task(&e);

    let err = e
        .tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": ["api", "blockign"] }))
        .expect_err("a partly-unknown list may not partly apply");
    assert_eq!(err.code, ErrorCode::NotFound, "{}", err.message);

    assert_eq!(
        e.task_get(&by_ref).unwrap()["tags"],
        json!(["api", "release"]),
        "`api` was removable and must NOT have been removed"
    );
}

/// The empty-list refusal, mirroring `tag.add`'s. `tags: []` is a caller who
/// named no tag at all: removing nothing and answering ok would be the same
/// silent-success shape D52 exists to close, one level up.
#[test]
fn tag_remove_refuses_an_empty_tag_list() {
    let e = engine();
    let (task, _) = tagged_task(&e);

    let err = e
        .tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": [] }))
        .expect_err("an empty `tags` array names no tag");
    assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
    assert!(err.message.contains("tags"), "{}", err.message);
}

/// Reachability through the one seam every surface shares. The engine method
/// existing is not the same as the method being callable: `tag.remove` needs a
/// `PARAMS` row (or the D33 gate refuses `ref`/`tags` as unknown keys) *and* a
/// match arm (or it is `unknown method`), and each half fails differently.
#[test]
fn tag_remove_is_reachable_through_dispatch_and_published_by_capabilities() {
    let e = engine();
    let (task, _) = tagged_task(&e);

    let out = dispatch(
        &e,
        "tag.remove",
        &json!({ "ref": task["short_id"].clone(), "tags": ["api"] }),
    )
    .expect("tag.remove must be dispatchable, not just implemented");
    assert_eq!(out["tags"], json!(["release"]));

    let methods = tasqx_core::capabilities()["methods"].clone();
    assert!(
        methods.as_array().unwrap().contains(&json!("tag.remove")),
        "a method a client cannot feature-detect is a method it will not call: {methods}"
    );
}

// ---- dependency edges leave the same audit trail as every other edit --------

/// The `dependency.*` events recorded against one task, newest first (the
/// `task.add` noise every fixture generates is filtered out here so the counts
/// below read as "how many dependency edits were logged").
fn dependency_events(e: &Engine, short_id: &Value) -> Vec<Value> {
    let listed = e
        .event_list(&json!({ "ref": short_id.clone() }))
        .expect("event.list");
    listed["events"]
        .as_array()
        .expect("event.list returns an events array")
        .iter()
        .filter(|ev| {
            ev["op"]
                .as_str()
                .is_some_and(|op| op.starts_with("dependency."))
        })
        .cloned()
        .collect()
}

/// A task plus the blocker it will depend on, and the `{ref, depends_on}` params
/// naming that edge — the three values every test below starts from.
fn dependent_blocker_edge(e: &Engine) -> (Value, Value, Value) {
    let blocker = e.task_add(&json!({ "title": "blocker" })).expect("add");
    let dependent = e.task_add(&json!({ "title": "dependent" })).expect("add");
    let edge = json!({
        "ref": dependent["short_id"].clone(),
        "depends_on": blocker["short_id"].clone(),
    });
    (dependent, blocker, edge)
}

/// The events table is two things at once: the audit log, and the daemon's
/// change feed — the poller derives every `task.changed` push from new event
/// rows. So deleting `dependency_add`'s `insert_event` call breaks watchers
/// without breaking storage, and the whole workspace stayed green when it was
/// deleted: the edge still lands in `dependencies`, but nothing is announced,
/// so an open TUI or `tasqx watch` keeps reporting `blocked: false` and `why`
/// over a daemon answers from stale state. Nothing crashes, nothing turns red.
///
/// The payload names the blocker by UUID rather than by short_id (the short id
/// is a display handle a later import can re-mint), so the log stays meaningful
/// replayed against another store.
#[test]
fn dependency_add_logs_one_event_naming_the_blocker_by_uuid() {
    let e = engine();
    let (dependent, blocker, edge) = dependent_blocker_edge(&e);

    e.dependency_add(&edge).expect("dependency.add");

    let logged = dependency_events(&e, &dependent["short_id"]);
    assert_eq!(
        logged.len(),
        1,
        "one edge added => one logged edit, got {logged:#?}"
    );
    assert_eq!(logged[0]["op"], "dependency.add");
    assert_eq!(logged[0]["entity"], "task");
    // Recorded against the DEPENDENT: it is the task whose `blocked` flipped,
    // and the task a subscriber is watching.
    assert_eq!(logged[0]["entity_id"], dependent["id"]);
    assert_eq!(
        logged[0]["payload"]["depends_on"], blocker["id"],
        "the payload must carry the blocker's uuid, not its short_id"
    );

    // The blocker itself was not edited, so its own log stays empty.
    assert!(
        dependency_events(&e, &blocker["short_id"]).is_empty(),
        "the edge belongs to the dependent's history only"
    );
}

/// The removal half, and the case the `if removed > 0` guard exists for: taking
/// away an edge that was never there must append nothing and bump nothing.
/// Without the guard a no-op call would push a spurious `task.changed` to every
/// subscriber and invalidate a concurrent client's `expected_rev` for an edit
/// that did not happen — the optimistic-concurrency token has to mean "the task
/// really changed".
#[test]
fn dependency_remove_logs_one_event_and_a_no_op_removal_logs_none() {
    let e = engine();
    let (dependent, blocker, edge) = dependent_blocker_edge(&e);
    let dependent_ref = json!({ "ref": dependent["short_id"].clone() });

    e.dependency_add(&edge).expect("dependency.add");
    e.dependency_remove(&edge).expect("dependency.remove");

    let logged = dependency_events(&e, &dependent["short_id"]);
    assert_eq!(
        logged.len(),
        2,
        "add then remove => two logged edits, got {logged:#?}"
    );
    // event.list is newest-first (UUIDv7 ordering).
    assert_eq!(logged[0]["op"], "dependency.remove");
    assert_eq!(logged[0]["entity_id"], dependent["id"]);
    assert_eq!(
        logged[0]["payload"]["depends_on"], blocker["id"],
        "the removal must name the same uuid the addition did"
    );

    let rev_before = e.task_get(&dependent_ref).unwrap()["_rev"].clone();
    let events_before = count(&e, "SELECT COUNT(*) FROM events");

    // Removing the already-removed edge is not an error — and not an edit.
    e.dependency_remove(&edge)
        .expect("removing an absent edge is a no-op, not a failure");

    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events"),
        events_before,
        "a no-op removal appended an event the daemon would push as a change"
    );
    assert_eq!(
        e.task_get(&dependent_ref).unwrap()["_rev"],
        rev_before,
        "a no-op removal bumped `rev`, invalidating a client's expected_rev"
    );
}

/// The asymmetry between the two handlers, pinned deliberately rather than left
/// to be discovered: `dependency_add` inserts the edge with `INSERT OR IGNORE`,
/// so re-adding an existing dependency touches no row — yet it bumps `rev` and
/// appends a second `dependency.add` unconditionally, while `dependency_remove`
/// guards exactly that case (the test above).
///
/// Pinned as current behaviour because it is defensible on the log side — the
/// events table is append-only history and "the caller asked again" is a fact
/// that happened — but the `rev` bump is visible to clients, so if this is ever
/// made symmetric with removal, this test is the single place that has to
/// change, and it states what the old contract was.
#[test]
fn a_repeated_dependency_add_logs_a_second_event_and_bumps_rev() {
    let e = engine();
    let (dependent, _blocker, edge) = dependent_blocker_edge(&e);
    let dependent_ref = json!({ "ref": dependent["short_id"].clone() });

    e.dependency_add(&edge).expect("dependency.add");
    let rev_after_first = e.task_get(&dependent_ref).unwrap()["_rev"]
        .as_i64()
        .expect("_rev is an integer");

    let again = e
        .dependency_add(&edge)
        .expect("a duplicate add is accepted");

    // The graph did not change: one edge, still exactly one blocker.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM dependencies"), 1);
    assert_eq!(again["depends_on"].as_array().unwrap().len(), 1);
    assert_eq!(again["blocked"], true);

    // The history and the rev did move, though.
    let logged = dependency_events(&e, &dependent["short_id"]);
    assert_eq!(logged.len(), 2, "got {logged:#?}");
    assert!(logged.iter().all(|ev| ev["op"] == "dependency.add"));
    assert_eq!(
        e.task_get(&dependent_ref).unwrap()["_rev"],
        json!(rev_after_first + 1)
    );
}

/// The whole-surface floor for the class of regression the tests above cover
/// one handler at a time: a handler that opens a mutation transaction must also
/// append an event inside it, because DESIGN's coupling is commit => exactly one
/// event, and the daemon has no other way to learn that anything changed.
///
/// The mutating set is DERIVED from the source, not listed here. A hand-written
/// list's failure mode is the handler nobody remembers to add to it — which is
/// how `dependency.add` and `dependency.remove` came to have no event assertion
/// at all — so the scan asks the code which handlers mutate: every method whose
/// body reaches for `begin_mutation`. A new handler is therefore covered the
/// day it is written.
///
/// What a text scan cannot see is that the right op reaches the right row with
/// the right payload; that is what the per-handler tests are for. What it does
/// catch, for all twenty-four at once and at zero runtime cost, is the write
/// disappearing entirely — the mutation that stayed green.
#[test]
fn every_handler_that_opens_a_mutation_also_appends_an_event() {
    // One handler opens a mutation and deliberately appends no event, and it is
    // named here rather than skipped by a rule, so adding a second one is a
    // decision somebody has to write down. `otlp_ingest` grows the raw OTLP
    // staging buffer; those rows are attributed to a task only later, by
    // session id and time window, so at ingest there is no entity whose
    // watchers could be told anything. Every other mutation here changes a task.
    const EVENTLESS_BY_DESIGN: [&str; 1] = ["otlp_ingest"];

    let mut mutating = 0;
    for (file, source) in SOURCES {
        // engine.rs's own unit tests run a sibling scan and mention
        // `begin_mutation` inside a string literal; cut there so this scan sees
        // production code only.
        let production = source.split("\n#[cfg(test)]").next().unwrap_or(source);
        for chunk in production.split("\n    pub fn ").skip(1) {
            // A chunk runs to the next method; truncate at the `}` in column
            // zero too, so the last method of an impl block does not absorb the
            // free functions that follow it.
            let body = chunk.split("\n}").next().unwrap_or(chunk);
            if !body.contains("self.begin_mutation()") {
                continue;
            }
            let name = chunk.split('(').next().unwrap_or(chunk).trim();
            mutating += 1;
            if EVENTLESS_BY_DESIGN.contains(&name) {
                continue;
            }
            assert!(
                body.contains("insert_event("),
                "{file}: `{name}` opens a mutation transaction but appends no event — \
                 the write would land while every watcher (daemon push, `tasqx watch`, \
                 the audit log) is told nothing"
            );
        }
    }

    // A source-scanning guard's real failure mode is matching nothing at all
    // (a renamed helper, a re-indented impl). Twenty-four handlers mutate at
    // the time of writing; the floor keeps a refactor that hides them from
    // being silently "all clear", while still letting the set grow.
    //
    // This number is also what catches a file dropping out of SOURCES, and that
    // is not hypothetical: engine/tokens.rs and engine/reports.rs were both
    // absent for as long as they have existed, above a comment claiming the
    // list held every file with `impl Engine` methods. Four mutating handlers
    // went unscanned the whole time, and the floor of 19 sat comfortably below
    // the 19 the remaining six files happened to yield. Raise it whenever the
    // real count moves — a floor that drifts below the truth is a guard that
    // has stopped guarding while still reporting green.
    assert!(
        mutating >= 25,
        "the scan found only {mutating} mutating handlers — it has stopped matching"
    );
}

// ---- event.revert (undo) ----------------------------------------------------

/// Every `op` string literal the engine hands to `insert_event`, read out of the
/// engine sources rather than listed here (D30).
///
/// The extraction is deliberately strict: `insert_event(tx, Entity::X, id, "op",
/// payload)` has its op at argument index three, and anything that is not a
/// plain string literal there panics naming the file. A scanner that quietly
/// skipped a call site it could not parse would under-report the vocabulary,
/// which is the one failure mode that makes the guard below pass vacuously.
fn event_ops_the_engine_writes() -> BTreeSet<String> {
    const CALL: &str = "insert_event(";
    let mut ops = BTreeSet::new();
    for (file, source) in SOURCES {
        // The engine's own `#[cfg(test)]` modules are not writers, and the prose
        // above a call site quotes the helper's name constantly.
        let production = source.split("\n#[cfg(test)]").next().unwrap_or(source);
        let code: String = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for (i, _) in code.match_indices(CALL) {
            let args = &code[i + CALL.len()..];
            let op = args.split(',').nth(3).unwrap_or("").trim();
            let literal = op
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or_else(|| {
                    panic!(
                        "{file}: an insert_event call passes {op:?} as its op, which this scan \
                         cannot read as a string literal — the undo vocabulary guard would \
                         silently stop covering it"
                    )
                });
            ops.insert(literal.to_string());
        }
    }
    ops
}

/// Rule 2, enforced structurally: every operation the engine can write is either
/// undoable or refused BY NAME with a reason, and neither table names an op
/// nothing writes.
///
/// Without this, a mutation added tomorrow falls through to `undo`'s generic
/// "this build has never seen that op" message — which is the right answer for a
/// store written by a NEWER tasqx and the wrong one for an op this very binary
/// writes, because it tells the operator nothing about what would take it back.
/// The reverse direction matters just as much: an entry left behind after its
/// writer is deleted is a refusal message describing an operation that can no
/// longer happen.
#[test]
fn every_event_op_the_engine_writes_is_either_undoable_or_refused_by_name() {
    let written = event_ops_the_engine_writes();

    for op in &written {
        let undoable = UNDOABLE_OPS.contains(&op.as_str());
        let refused = NOT_UNDOABLE.iter().any(|(known, _)| known == op);
        assert!(
            undoable ^ refused,
            "`{op}` is written by the engine but is {} — it must be in exactly one of \
             UNDOABLE_OPS (with an exact inverse) or NOT_UNDOABLE (with the reason and what \
             does take it back)",
            match (undoable, refused) {
                (false, false) => "in neither undo table",
                _ => "in both undo tables",
            }
        );
    }

    for op in UNDOABLE_OPS {
        assert!(
            written.contains(op),
            "UNDOABLE_OPS names `{op}`, which no engine handler writes any more — undo claims to \
             reverse an operation that cannot happen"
        );
    }
    for (op, why) in NOT_UNDOABLE {
        assert!(
            written.contains(*op),
            "NOT_UNDOABLE names `{op}`, which no engine handler writes any more — a refusal \
             message for an operation that cannot happen"
        );
        assert!(
            why.len() > 60,
            "`{op}`'s refusal is {} characters, which cannot both name the reason and say what \
             does take it back",
            why.len()
        );
    }

    // A source-scanning guard's real failure mode is matching nothing. Twenty-two
    // distinct ops exist at the time of writing; re-derive from the count this
    // guard reports rather than adding the number of ops you just wrote.
    assert!(
        written.len() >= 22,
        "the scan found only {} event ops — it has stopped matching: {written:?}",
        written.len()
    );
}

/// A task carrying two tags, a note, a blocker it depends on, and a closed
/// interval — one fixture per undoable op, so each test below can put the store
/// into the shape it needs with one more call.
fn undo_fixture(e: &Engine) -> (Value, Value) {
    let blocker = e.task_add(&json!({ "title": "blocker" })).expect("add");
    let task = e.task_add(&json!({ "title": "Ship v1" })).expect("add");
    e.tag_add(&json!({ "ref": task["short_id"].clone(), "tags": ["api", "release"] }))
        .expect("tag.add");
    (task, blocker)
}

/// The whole shape of an undo, on the op whose inverse is exact by construction
/// (D52's all-or-nothing pre-check): the tag comes back, the answer NAMES what
/// it undid, and — rule 1 — the original event is still in the log with a fresh
/// `undo` event behind it rather than in place of it.
#[test]
fn undo_puts_a_removed_tag_back_and_says_what_it_undid() {
    let e = engine();
    let (task, _) = undo_fixture(&e);
    let by_ref = json!({ "ref": task["short_id"].clone() });
    e.tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": ["api"] }))
        .expect("tag.remove");
    let rev_before = e.task_get(&by_ref).unwrap()["_rev"].as_i64().unwrap();
    let events_before = count(&e, "SELECT COUNT(*) FROM events");

    let out = e.event_revert().expect("undo");

    assert_eq!(
        e.task_get(&by_ref).unwrap()["tags"],
        json!(["api", "release"]),
        "the tag must be back in the store, not only in the answer"
    );
    // Rule 4: it says what it undid, naming the task and the operation.
    assert_eq!(out["reverted"]["op"], "tag.remove");
    assert_eq!(out["short_id"], task["short_id"]);
    assert_eq!(
        out["title"], "Ship v1",
        "a short_id alone is not something anyone recognizes at a glance"
    );
    assert_eq!(out["restored"]["tags"], json!(["api"]));
    assert_eq!(
        e.task_get(&by_ref).unwrap()["_rev"],
        json!(rev_before + 1),
        "undo really changed the task, so a client's expected_rev must move with it"
    );

    // Rule 1: APPEND, never rewrite. The `tag.remove` row is untouched and a new
    // `undo` row sits behind it naming the event it reversed.
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events"),
        events_before + 1,
        "undo must add one event, not remove the one it reversed"
    );
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events WHERE op='tag.remove'"),
        1,
        "the reversed event was deleted — every consumer of the log (D3 sync, \
         `event.list`, the daemon feed) has just been lied to about what happened"
    );
    let logged = e
        .event_list(&json!({ "ref": task["short_id"].clone(), "limit": 1 }))
        .unwrap();
    assert_eq!(logged["events"][0]["op"], "undo");
    assert_eq!(
        logged["events"][0]["payload"]["reverted"], out["reverted"]["event"],
        "the compensating event must name the event it reversed, or the log cannot \
         be read as `X happened, then it was undone`"
    );
    assert_eq!(logged["events"][0]["payload"]["reverted_op"], "tag.remove");
}

/// The other three inverses, each end to end. One test rather than three
/// because the property is identical — the effect is gone from the store and the
/// answer names it — and because a per-op test that only ever ran for `tag.remove`
/// would leave three quarters of the closed set unproven.
#[test]
fn undo_reverses_every_operation_the_closed_set_claims() {
    // stop: the interval reopens and the seconds it folded in come back off.
    let e = engine();
    let (task, _blocker) = undo_fixture(&e);
    let by_ref = json!({ "ref": task["short_id"].clone() });
    e.task_start(&by_ref).expect("start");
    e.conn()
        .execute(
            "UPDATE tasks SET active_since = '2020-01-01T00:00:00Z' WHERE short_id = ?1",
            params![task["short_id"].as_i64().unwrap()],
        )
        .expect("backdate the interval so it has measurable length");
    let stopped = e.task_stop(&by_ref).expect("stop");
    let tracked_after_stop: i64 = e
        .conn()
        .query_row(
            "SELECT tracked_seconds FROM tasks WHERE short_id = ?1",
            params![task["short_id"].as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        tracked_after_stop > 0,
        "precondition: the stop tracked time"
    );

    let out = e.event_revert().expect("undo the stop");
    assert_eq!(out["reverted"]["op"], "stop");
    assert_eq!(out["restored"]["tracked"], stopped["tracked"]);
    let got = e.task_get(&by_ref).unwrap();
    assert_eq!(got["status"], "active", "the interval must be open again");
    // Read straight out of the column rather than off the rendered `tracked`
    // string: the whole claim is that the stored seconds return to their
    // pre-stop value exactly, and an ISO duration would round that claim.
    assert_eq!(
        count(
            &e,
            "SELECT tracked_seconds FROM tasks WHERE title = 'Ship v1'"
        ),
        0,
        "the seconds `stop` folded into the total must come back off, exactly — \
         that is the number every report reads"
    );

    // dependency.remove: the edge goes back, named by the blocker's short_id.
    let e = engine();
    let (task, blocker) = undo_fixture(&e);
    let by_ref = json!({ "ref": task["short_id"].clone() });
    let edge = json!({
        "ref": task["short_id"].clone(),
        "depends_on": blocker["short_id"].clone(),
    });
    e.dependency_add(&edge).expect("dependency.add");
    e.dependency_remove(&edge).expect("dependency.remove");
    assert_eq!(e.task_get(&by_ref).unwrap()["depends_on"], json!([]));

    let out = e.event_revert().expect("undo the dependency removal");
    assert_eq!(out["reverted"]["op"], "dependency.remove");
    assert_eq!(out["restored"]["depends_on"], blocker["short_id"]);
    let got = e.task_get(&by_ref).unwrap();
    assert_eq!(got["depends_on"], json!([blocker["short_id"].clone()]));
    assert_eq!(got["blocked"], true, "the edge is load-bearing again");

    // annotation.add: the note is gone, and the answer shows the text that went.
    let e = engine();
    let (task, _blocker) = undo_fixture(&e);
    let by_ref = json!({ "ref": task["short_id"].clone() });
    e.annotation_add(&json!({ "ref": task["short_id"].clone(), "body": "wrong task" }))
        .expect("annotation.add");

    let out = e.event_revert().expect("undo the note");
    assert_eq!(out["reverted"]["op"], "annotation.add");
    assert_eq!(
        out["restored"]["annotation"], "wrong task",
        "the answer must show the text it removed — it is the only copy left"
    );
    assert_eq!(
        e.task_get(&by_ref).unwrap()["annotations"],
        json!([]),
        "the note must be gone from the store"
    );
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM annotations_fts"),
        0,
        "the FTS row must go with it, or `memory search` keeps finding a note that is not there"
    );
}

/// Rule 2's refusal path, and the reason it names the op rather than saying
/// "cannot undo that": every one of these has a different way back, and a
/// refusal that does not say which one leaves the user guessing at a store they
/// have just changed by accident.
///
/// The three chosen are the three whole classes of refusal: an operation whose
/// event records the NEW values only (`modify`), one whose effect is compound
/// (`done` — recurrence spawn plus token measurement), and one that cannot be
/// reversed without deleting a row the log still names (`add`).
#[test]
fn undo_refuses_an_operation_outside_the_closed_set_and_names_the_way_back() {
    for (setup, op, way_back) in [
        ("modify", "modify", "tasqx show"),
        ("done", "done", "tasqx reopen"),
        ("add", "add", "tasqx cancel"),
    ] {
        let e = engine();
        let (task, _) = undo_fixture(&e);
        let by_ref = json!({ "ref": task["short_id"].clone() });
        match setup {
            "modify" => {
                e.task_modify(
                    &json!({ "ref": task["short_id"].clone(), "set": { "title": "typo" } }),
                )
                .expect("modify");
            }
            "done" => {
                e.task_done(&by_ref).expect("done");
            }
            // `add` needs no setup: the fixture's own last event is the tag.add,
            // so add one more task and its `add` is the newest event.
            _ => {
                e.task_add(&json!({ "title": "oops" })).expect("add");
            }
        }
        let events_before = count(&e, "SELECT COUNT(*) FROM events");

        let err = e
            .event_revert()
            .expect_err("an op outside the closed set must refuse, not guess an inverse");

        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert!(
            err.message.contains(&format!("`{op}`")),
            "the refusal must name the operation it will not reverse: {}",
            err.message
        );
        assert!(
            err.message.contains(way_back),
            "the refusal must say what DOES take it back (expected `{way_back}`): {}",
            err.message
        );
        assert!(
            err.message.contains("tag.remove"),
            "the refusal must publish the closed set, or there is no way to learn it: {}",
            err.message
        );
        assert_eq!(
            count(&e, "SELECT COUNT(*) FROM events"),
            events_before,
            "a refused undo wrote an event — the daemon would push it as a change"
        );
    }
}

/// There is no redo, and the reason is not squeamishness: `undo` reaches only
/// the newest event, so undoing an undo would toggle one change back and forth
/// forever. The refusal has to say that, because a second `tasqx undo` is
/// exactly what a user reaches for when the first one was not enough.
#[test]
fn undoing_an_undo_refuses_rather_than_toggling_forever() {
    let e = engine();
    let (task, _) = undo_fixture(&e);
    e.tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": ["api"] }))
        .expect("tag.remove");
    e.event_revert().expect("the first undo");

    let err = e.event_revert().expect_err("undo of an undo must refuse");
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    assert!(
        err.message.contains("`undo`") && err.message.contains("no redo"),
        "the refusal must name the op and say there is no redo: {}",
        err.message
    );
    assert_eq!(
        e.task_get(&json!({ "ref": task["short_id"].clone() }))
            .unwrap()["tags"],
        json!(["api", "release"]),
        "the refused second undo must have changed nothing"
    );
}

/// An empty log is `not_found`, not a silent success. "There was nothing to
/// undo" and "I undid something" are different answers, and exit 4 is the only
/// thing a script can branch on.
#[test]
fn undo_on_an_empty_log_is_not_found() {
    let e = engine();
    let err = e
        .event_revert()
        .expect_err("an empty log has nothing to undo");
    assert_eq!(err.code, ErrorCode::NotFound, "{}", err.message);
    assert!(err.message.contains("nothing to undo"), "{}", err.message);
}

/// The inverses are exact because nothing can have happened since the newest
/// event — so when the store says otherwise, undo has to stop rather than write
/// a "restoration" that restores nothing. This drives the store out of step the
/// only way that is possible (an external SQLite writer) and checks the refusal
/// names the tag that is already back.
#[test]
fn undo_refuses_when_the_effect_it_would_reverse_is_already_gone() {
    let e = engine();
    let (task, _) = undo_fixture(&e);
    e.tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": ["api"] }))
        .expect("tag.remove");
    // An external writer puts the tag back without touching the log — exactly
    // what another process editing the SQLite file does.
    e.tag_add(&json!({ "ref": task["short_id"].clone(), "tags": ["api"] }))
        .expect("tag.add");
    e.conn()
        .execute(
            "DELETE FROM events WHERE op = 'tag.add' AND id = (SELECT MAX(id) FROM events)",
            [],
        )
        .expect("drop the event so `tag.remove` is newest again");
    let events_before = count(&e, "SELECT COUNT(*) FROM events");

    let err = e
        .event_revert()
        .expect_err("undo must not report restoring a tag that is already attached");
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    assert!(
        err.message.contains("`api`"),
        "the refusal must name the tag that is already back: {}",
        err.message
    );
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events"),
        events_before,
        "a refused undo wrote an event"
    );
}

/// Reachability through the one seam every surface shares. The engine method
/// existing is not the same as the method being callable: `event.revert` needs a
/// `PARAMS` row (or the D33 gate refuses it) *and* a match arm (or it is
/// `unknown method`), and each half fails differently.
///
/// The empty accepted set is asserted too, because it is the scoping decision in
/// machine-readable form: a `ref` here would silently reach past whatever
/// happened elsewhere, so the published contract has to say there is none.
#[test]
fn event_revert_is_reachable_through_dispatch_and_takes_no_params() {
    let e = engine();
    let (task, _) = undo_fixture(&e);
    e.tag_remove(&json!({ "ref": task["short_id"].clone(), "tags": ["api"] }))
        .expect("tag.remove");

    let out = dispatch(&e, "event.revert", &json!({}))
        .expect("event.revert must be dispatchable, not just implemented");
    assert_eq!(out["reverted"]["op"], "tag.remove");

    let caps = tasqx_core::capabilities();
    assert!(
        caps["methods"]
            .as_array()
            .unwrap()
            .contains(&json!("event.revert")),
        "a method a client cannot feature-detect is a method it will not call: {}",
        caps["methods"]
    );
    assert_eq!(caps["params"]["event.revert"], json!([]));

    let err = dispatch(&e, "event.revert", &json!({ "ref": 1 })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("ref"), "{}", err.message);
}
