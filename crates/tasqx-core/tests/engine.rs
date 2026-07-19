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
        storage::insert_event(&tx, tasqx_core::Entity::Task, "rollback-id", "add", &json!({})).unwrap();
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
    e.task_modify(&json!({ "ref": sid, "set": { "title": "bump" } })).unwrap();

    let err = e
        .task_modify(&json!({ "ref": sid, "set": { "title": "LOST UPDATE" }, "expected_rev": "1" }))
        .expect_err("a stringified expected_rev must not silently skip the guard");
    assert_eq!(err.code, ErrorCode::BadRequest, "got {err:?}");
    assert!(err.message.contains("expected_rev"), "message must name the param: {}", err.message);

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
    e.task_modify(&json!({ "ref": sid, "set": { "title": "two" } })).unwrap();

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
        ("project.list", json!({ "include_archived": "true" }), "include_archived"),
        ("task.start", json!({ "ref": sid, "keep": "yes" }), "keep"),
        ("report.summary", json!({ "all": 1 }), "all"),
        // opt_str_array dropped a non-array, a non-string entry and an empty
        // string — and its two callers disagreed about which of those mattered.
        ("task.add", json!({ "title": "x", "tags": "home" }), "tags"),
        ("task.add", json!({ "title": "x", "tags": ["ok", 7] }), "tags"),
        ("task.add", json!({ "title": "x", "tags": ["ok", ""] }), "tags"),
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
        assert_eq!(err.code, ErrorCode::BadRequest, "{method} {params} => {err:?}");
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
        ("task.add", json!({ "title": "b", "prioritee": "H" }), "prioritee", "priority"),
        ("task.list", json!({ "limitt": 2 }), "limitt", "limit"),
        ("event.list", json!({ "bogus": 1 }), "bogus", "limit"),
        ("task.modify", json!({ "ref": sid, "set": { "title": "x" }, "expect_rev": 1 }), "expect_rev", "expected_rev"),
        ("report.summary", json!({ "groupby": "status" }), "groupby", "group_by"),
        ("project.create", json!({ "name": "x", "desc": "y" }), "desc", "description"),
        ("store.export", json!({ "filtr": "" }), "filtr", "filter"),
        ("tag.add", json!({ "ref": sid, "tag": ["x"] }), "tag", "tags"),
        ("task.start", json!({ "ref": sid, "kep": true }), "kep", "keep"),
        ("core.capabilities", json!({ "anything": 1 }), "anything", "no params"),
    ];

    for (method, params, bad, hint) in cases {
        let err = dispatch(&e, method, &params).err().unwrap_or_else(|| {
            panic!("{method} {params}: an unknown key must be refused, not ignored")
        });
        assert_eq!(err.code, ErrorCode::BadRequest, "{method} {params} => {err:?}");
        assert!(err.message.contains(bad), "message must name `{bad}`: {}", err.message);
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
