//! `undo` past the small edits: `done`, `cancel`, `tag.add` and `modify` (D215,
//! task #1129), `project.unarchive`, and an archived project's tasks leaving
//! `@working`.

use rusqlite::params;
use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Value {
    dispatch(e, method, &params).unwrap_or_else(|err| panic!("{method}: {err:?}"))
}

fn fail(e: &Engine, method: &str, params: Value) -> tasqx_core::ApiError {
    dispatch(e, method, &params).expect_err(method)
}

fn undo(e: &Engine) -> Value {
    e.event_revert().expect("undo")
}

fn undo_err(e: &Engine) -> String {
    let err = e.event_revert().expect_err("undo must refuse");
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    err.message
}

fn get(e: &Engine, r: i64) -> Value {
    call(e, "task.get", json!({ "ref": r }))
}

fn count(e: &Engine, sql: &str) -> i64 {
    e.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn backdate(e: &Engine, r: i64) {
    e.conn()
        .execute(
            "UPDATE tasks SET active_since = '2020-01-01T00:00:00Z' WHERE short_id = ?1",
            params![r],
        )
        .unwrap();
}

fn tracked(e: &Engine, r: i64) -> i64 {
    e.conn()
        .query_row(
            "SELECT tracked_seconds FROM tasks WHERE short_id = ?1",
            params![r],
            |x| x.get(0),
        )
        .unwrap()
}

// ---- task.done ---------------------------------------------------------------

#[test]
fn undo_done_puts_a_pending_task_back_and_says_so() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "ship it" }));
    call(&e, "task.done", json!({ "ref": 1 }));

    let out = undo(&e);

    assert_eq!(out["reverted"]["op"], "done");
    assert_eq!(out["short_id"], 1);
    assert_eq!(out["restored"]["status"], "pending");
    let t = get(&e, 1);
    assert_eq!(t["status"], "pending");
    assert_eq!(t["completed"], Value::Null);
}

#[test]
fn undo_done_of_a_running_task_reopens_the_interval_and_takes_the_time_back_off() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "ship it" }));
    call(&e, "task.start", json!({ "ref": 1 }));
    backdate(&e, 1);
    call(&e, "task.done", json!({ "ref": 1 }));
    assert!(tracked(&e, 1) > 0, "precondition: done banked the interval");

    let out = undo(&e);

    assert_eq!(out["restored"]["status"], "active");
    assert_eq!(get(&e, 1)["status"], "active");
    assert_eq!(tracked(&e, 1), 0, "the banked seconds come back off");
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM tasks WHERE active_since IS NOT NULL"
        ),
        1
    );
}

#[test]
fn undo_done_of_a_recurring_task_removes_the_untouched_next_instance() {
    let e = engine();
    call(
        &e,
        "task.add",
        json!({ "title": "water", "recurrence": "daily", "due": "2030-01-01" }),
    );
    call(&e, "task.done", json!({ "ref": 1 }));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), 2, "precondition");

    let out = undo(&e);

    assert_eq!(out["reverted"]["op"], "done");
    assert_eq!(out["restored"]["removed_spawn"], 2, "{out}");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), 1);
    assert_eq!(get(&e, 1)["status"], "pending");
    // Completing again spawns again (the D191 slot is free), under a new id.
    call(&e, "task.done", json!({ "ref": 1 }));
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), 2);
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM tasks WHERE short_id = 2"),
        0,
        "short ids are never recycled"
    );
}

#[test]
fn undo_done_refuses_when_the_next_instance_was_touched_and_names_it() {
    let e = engine();
    call(
        &e,
        "task.add",
        json!({ "title": "water", "recurrence": "daily", "due": "2030-01-01" }),
    );
    call(&e, "task.done", json!({ "ref": 1 }));
    // Touched behind the log's back: its rev moved with no event after the add.
    e.conn()
        .execute("UPDATE tasks SET rev = rev + 1 WHERE short_id = 2", [])
        .unwrap();
    let events = count(&e, "SELECT COUNT(*) FROM events");

    let msg = undo_err(&e);

    assert!(msg.contains("#2"), "names the touched instance: {msg}");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), events);
    assert_eq!(get(&e, 1)["status"], "done", "nothing was undone");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), 2);
}

#[test]
fn undo_done_puts_back_the_checks_the_completion_proved() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "ship it" }));
    let c = call(&e, "check.add", json!({ "ref": 1, "body": "tests pass" }));
    let id = c["check"]["id"].as_str().expect("check id").to_string();
    call(
        &e,
        "task.done",
        json!({ "ref": 1, "checks_passed": [id], "evidence": "ci run 9" }),
    );
    let state = |e: &Engine| -> (String, Option<String>) {
        e.conn()
            .query_row("SELECT state, evidence FROM checks", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    };
    assert_eq!(state(&e).0, "passed", "precondition");

    undo(&e);

    assert_eq!(state(&e), ("open".to_string(), None));
}

#[test]
fn undo_done_refuses_when_the_completion_recorded_a_token_measurement() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "ship it" }));
    call(
        &e,
        "task.done",
        json!({ "ref": 1, "tool": "claude-code", "input_tokens": 10, "output_tokens": 5 }),
    );

    let msg = undo_err(&e);

    assert!(msg.contains("token"), "{msg}");
    assert_eq!(get(&e, 1)["status"], "done");
}

// ---- task.cancel -------------------------------------------------------------

#[test]
fn undo_cancel_restores_the_previous_status() {
    let e = engine();
    call(
        &e,
        "task.add",
        json!({ "title": "later", "wait": "2030-01-01" }),
    );
    let before = get(&e, 1)["status"].clone();
    call(&e, "task.cancel", json!({ "ref": 1 }));

    let out = undo(&e);

    assert_eq!(out["reverted"]["op"], "cancel");
    assert_eq!(get(&e, 1)["status"], before);
    assert_eq!(out["restored"]["status"], before);
}

#[test]
fn undo_cancel_of_a_running_task_reopens_the_interval() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "x" }));
    call(&e, "task.start", json!({ "ref": 1 }));
    backdate(&e, 1);
    call(&e, "task.cancel", json!({ "ref": 1 }));
    assert!(tracked(&e, 1) > 0, "precondition");

    undo(&e);

    assert_eq!(get(&e, 1)["status"], "active");
    assert_eq!(tracked(&e, 1), 0);
}

// ---- tag.add / tag.remove ----------------------------------------------------

#[test]
fn undo_tag_add_takes_off_only_the_tags_that_call_attached() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "x", "tags": ["api"] }));
    call(
        &e,
        "tag.add",
        json!({ "ref": 1, "tags": ["api", "release"] }),
    );

    let out = undo(&e);

    assert_eq!(out["reverted"]["op"], "tag.add");
    assert_eq!(out["restored"]["removed"], json!(["release"]));
    assert_eq!(get(&e, 1)["tags"], json!(["api"]), "the old tag stays");
}

#[test]
fn undo_tag_add_refuses_an_event_that_does_not_say_which_tags_were_new() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "x" }));
    call(&e, "tag.add", json!({ "ref": 1, "tags": ["api"] }));
    e.conn()
        .execute(
            "UPDATE events SET payload = json_remove(payload, '$.added') WHERE op = 'tag.add'",
            [],
        )
        .unwrap();

    let msg = undo_err(&e);

    assert!(msg.contains("tag.add"), "{msg}");
    assert_eq!(get(&e, 1)["tags"], json!(["api"]));
}

// ---- task.modify -------------------------------------------------------------

#[test]
fn undo_modify_puts_every_changed_field_back() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "work" }));
    call(
        &e,
        "task.add",
        json!({ "title": "old", "priority": "low", "project": "work", "estimate": "1h" }),
    );
    call(
        &e,
        "task.modify",
        json!({ "ref": 1, "set": {
            "title": "new", "priority": "high", "project": null,
            "estimate": null, "due": "2030-05-01", "remind": null
        } }),
    );

    let out = undo(&e);

    assert_eq!(out["reverted"]["op"], "modify");
    let t = get(&e, 1);
    assert_eq!(t["title"], "old");
    assert_eq!(t["priority"], "L");
    assert_eq!(t["project"], "work");
    assert_eq!(t["estimate"], "PT1H");
    assert_eq!(t["due"], Value::Null);
    assert_eq!(
        out["restored"]["fields"]["title"], "old",
        "the answer names what came back: {out}"
    );
}

#[test]
fn undo_modify_of_a_cancellation_brings_back_status_and_running_time() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "x" }));
    call(&e, "task.start", json!({ "ref": 1 }));
    backdate(&e, 1);
    call(
        &e,
        "task.modify",
        json!({ "ref": 1, "set": { "status": "cancelled" } }),
    );
    assert!(tracked(&e, 1) > 0, "precondition");

    undo(&e);

    assert_eq!(get(&e, 1)["status"], "active");
    assert_eq!(tracked(&e, 1), 0);
}

#[test]
fn undo_modify_of_a_tracked_correction_restores_the_total() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "x" }));
    call(
        &e,
        "task.adjust_tracked",
        json!({ "ref": 1, "delta": "+30m", "reason": "forgot" }),
    );
    call(
        &e,
        "task.modify",
        json!({ "ref": 1, "set": { "tracked": "2h" } }),
    );
    assert_eq!(tracked(&e, 1), 7200, "precondition");

    undo(&e);

    assert_eq!(tracked(&e, 1), 1800);
    assert_eq!(
        count(&e, "SELECT tracked_adjustment_seconds FROM tasks"),
        1800
    );
}

#[test]
fn undo_modify_refuses_an_event_written_before_it_recorded_before_values() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "old" }));
    call(
        &e,
        "task.modify",
        json!({ "ref": 1, "set": { "title": "new" } }),
    );
    e.conn()
        .execute(
            "UPDATE events SET payload = json_remove(payload, '$.before') WHERE op = 'modify'",
            [],
        )
        .unwrap();
    let events = count(&e, "SELECT COUNT(*) FROM events");

    let msg = undo_err(&e);

    assert!(msg.contains("modify"), "{msg}");
    assert!(msg.contains("before"), "says what the event lacks: {msg}");
    assert_eq!(get(&e, 1)["title"], "new");
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), events);
}

// ---- project.unarchive -------------------------------------------------------

#[test]
fn unarchive_round_trips_and_refuses_what_is_not_archived() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "work" }));
    call(&e, "project.archive", json!({ "name": "work" }));
    assert_eq!(
        fail(&e, "task.add", json!({ "title": "x", "project": "work" })).code,
        ErrorCode::Conflict
    );

    let out = call(&e, "project.unarchive", json!({ "name": "work" }));

    assert_eq!(out["name"], "work");
    assert_eq!(out["archived"], false);
    call(&e, "task.add", json!({ "title": "x", "project": "work" }));
    let listed = call(&e, "project.list", json!({}));
    assert_eq!(listed["projects"][0]["archived"], false);

    let again = fail(&e, "project.unarchive", json!({ "name": "work" }));
    assert_eq!(again.code, ErrorCode::Conflict, "{}", again.message);
    let unknown = fail(&e, "project.unarchive", json!({ "name": "nosuch" }));
    assert_eq!(unknown.code, ErrorCode::NotFound);
}

#[test]
fn undo_does_not_reverse_an_unarchive() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "work" }));
    call(&e, "project.archive", json!({ "name": "work" }));
    call(&e, "project.unarchive", json!({ "name": "work" }));

    let msg = undo_err(&e);

    assert!(msg.contains("`unarchive`"), "{msg}");
    assert!(msg.contains("tasqx archive"), "names the way back: {msg}");
}

// ---- archived projects and @working ------------------------------------------

#[test]
fn an_archived_projects_tasks_leave_the_working_set_unless_the_filter_names_it() {
    let e = engine();
    // The first project created is the default, so `loose` lands in `live`.
    call(&e, "project.create", json!({ "name": "live" }));
    call(&e, "project.create", json!({ "name": "old" }));
    call(
        &e,
        "task.add",
        json!({ "title": "in old", "project": "old" }),
    );
    call(
        &e,
        "task.add",
        json!({ "title": "in live", "project": "live" }),
    );
    call(&e, "task.add", json!({ "title": "loose" }));
    call(&e, "project.archive", json!({ "name": "old" }));
    let titles = |filter: &str| -> Vec<String> {
        let r = call(
            &e,
            "task.list",
            json!({ "filter": filter, "sort": ["short_id"] }),
        );
        r["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap().to_string())
            .collect()
    };

    assert_eq!(titles("@working"), ["in live", "loose"]);
    assert_eq!(titles("@working and project:live"), ["in live", "loose"]);
    assert_eq!(titles("@working and project:old"), ["in old"]);
    // A plain list is not the working set: archiving is a shelf, not a delete.
    assert_eq!(titles("project:old"), ["in old"]);
    assert_eq!(titles("status:pending"), ["in old", "in live", "loose"]);
}
