//! Tracked time as every read reports it (task #655, D166): the running
//! interval counts on every surface that says `tracked`, and
//! `task.adjust_tracked` is the auditable, undoable correction beside it.
//!
//! A running interval is made measurable by backdating `active_since` (and the
//! `start` event, for the windowed summary) rather than by pinning the clock:
//! `TASQX_NOW` is process-global, so a pin here would move every other test in
//! the binary. The bounds below are therefore "at least the backdated span,
//! and not a minute more", which no real run can straddle.

use rusqlite::params;
use serde_json::{json, Value};
use tasqx_core::util::duration_secs;
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn add(e: &Engine, title: &str, extra: Value) -> i64 {
    let mut params = json!({ "title": title });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            params[k] = v.clone();
        }
    }
    call(e, "task.add", params).expect("task.add")["short_id"]
        .as_i64()
        .expect("add returns short_id")
}

/// Start `sid` and move its open interval `secs` into the past — the row's
/// anchor and the `start` event's stamp together, so the column and the log
/// tell the same story.
fn run_for(e: &Engine, sid: i64, secs: i64) {
    call(e, "task.start", json!({ "ref": sid })).expect("start");
    let at = ago(secs);
    e.conn()
        .execute(
            "UPDATE tasks SET active_since = ?1 WHERE short_id = ?2",
            params![at, sid],
        )
        .unwrap();
    e.conn()
        .execute(
            "UPDATE events SET ts = ?1 WHERE op = 'start' AND entity_id = \
             (SELECT id FROM tasks WHERE short_id = ?2)",
            params![at, sid],
        )
        .unwrap();
}

/// The instant `secs` seconds ago, spelled as `since`/`until` accept it.
fn ago(secs: i64) -> String {
    jiff::Timestamp::from_second(tasqx_core::clock::now().as_second() - secs)
        .unwrap()
        .to_string()
}

fn secs(v: &Value) -> i64 {
    duration_secs(
        v.as_str()
            .unwrap_or_else(|| panic!("not a duration: {v:?}")),
    )
    .unwrap_or_else(|| panic!("unreadable duration: {v:?}"))
}

fn assert_about(got: i64, want: i64, what: &str) {
    assert!(
        (want..want + 60).contains(&got),
        "{what}: expected about {want}s including the running interval, got {got}s"
    );
}

#[test]
fn get_and_list_report_the_running_interval_of_an_active_task() {
    let e = engine();
    let sid = add(&e, "on the clock", json!({}));
    run_for(&e, sid, 2 * 3600);

    let got = call(&e, "task.get", json!({ "ref": sid })).unwrap();
    assert_about(secs(&got["tracked"]), 7200, "task.get tracked");

    let listed = call(&e, "task.list", json!({ "filter": "status:active" })).unwrap();
    let row = &listed["tasks"][0];
    assert_eq!(row["short_id"], sid);
    assert_about(secs(&row["tracked"]), 7200, "task.list tracked");

    // The stored column is untouched: only closed intervals live there.
    let stored: i64 = e
        .conn()
        .query_row(
            "SELECT tracked_seconds FROM tasks WHERE short_id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, 0, "the running interval is reported, never stored");
}

#[test]
fn summary_counts_the_running_interval_and_clips_it_to_a_window() {
    let e = engine();
    let sid = add(&e, "on the clock", json!({}));
    run_for(&e, sid, 3 * 3600);
    let metrics = json!(["tracked_total"]);

    let lifetime = call(
        &e,
        "report.summary",
        json!({ "group_by": "project", "metrics": metrics }),
    )
    .unwrap();
    assert_about(
        secs(&lifetime["groups"][0]["tracked_total"]),
        3 * 3600,
        "unwindowed tracked_total",
    );

    let last_hour = call(
        &e,
        "report.summary",
        json!({ "group_by": "project", "metrics": metrics, "since": ago(3600) }),
    )
    .unwrap();
    assert_about(
        secs(&last_hour["groups"][0]["tracked_total"]),
        3600,
        "a window from an hour ago clips the running interval to that hour",
    );

    let before = call(
        &e,
        "report.summary",
        json!({ "group_by": "project", "metrics": metrics, "until": ago(4 * 3600) }),
    )
    .unwrap();
    assert_eq!(
        before["groups"][0]["tracked_total"], "PT0S",
        "a window ending before the interval began holds none of it"
    );
}

#[test]
fn adjust_on_a_done_task_moves_calibration_and_undo_takes_it_back() {
    let e = engine();
    let sid = add(&e, "stalled harness", json!({ "estimate": "1h" }));
    call(
        &e,
        "task.modify",
        json!({ "ref": sid, "set": { "tracked": "4h" } }),
    )
    .unwrap();
    call(&e, "task.done", json!({ "ref": sid })).unwrap();
    let calibration = |e: &Engine| {
        call(e, "report.outcomes", json!({ "metrics": ["calibration"] })).unwrap()["groups"][0]
            ["calibration"]["median_ratio"]
            .as_f64()
            .unwrap()
    };
    assert!((calibration(&e) - 4.0).abs() < 1e-9);

    let out = call(
        &e,
        "task.adjust_tracked",
        json!({ "ref": sid, "delta": "-2h30m", "reason": "idle gap" }),
    )
    .expect("adjust on a done task");
    assert_eq!(out["tracked"], "PT1H30M");
    assert_eq!(out["tracked_adjustment"], "-PT2H30M");

    let got = call(&e, "task.get", json!({ "ref": sid })).unwrap();
    assert_eq!(got["tracked"], "PT1H30M");
    assert_eq!(got["tracked_adjustment"], "-PT2H30M");
    assert!(
        (calibration(&e) - 1.5).abs() < 1e-9,
        "calibration reads the adjusted total"
    );

    let undone = e.event_revert().expect("undo the adjustment");
    assert_eq!(undone["reverted"]["op"], "adjust_tracked");
    let got = call(&e, "task.get", json!({ "ref": sid })).unwrap();
    assert_eq!(got["tracked"], "PT4H");
    assert_eq!(got["tracked_adjustment"], "PT0S");
    assert!((calibration(&e) - 4.0).abs() < 1e-9);
}

#[test]
fn adjust_refuses_a_total_below_zero_and_a_missing_reason() {
    let e = engine();
    let sid = add(&e, "timed", json!({}));
    call(
        &e,
        "task.modify",
        json!({ "ref": sid, "set": { "tracked": "1h" } }),
    )
    .unwrap();

    let err = call(
        &e,
        "task.adjust_tracked",
        json!({ "ref": sid, "delta": "-2h", "reason": "too much" }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{err:?}");

    for reason in [json!(""), json!("   "), Value::Null] {
        let mut p = json!({ "ref": sid, "delta": "+30m" });
        if !reason.is_null() {
            p["reason"] = reason;
        }
        let err = call(&e, "task.adjust_tracked", p).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest, "{err:?}");
    }

    let got = call(&e, "task.get", json!({ "ref": sid })).unwrap();
    assert_eq!(got["tracked"], "PT1H", "a refusal changes nothing");

    call(
        &e,
        "task.adjust_tracked",
        json!({ "ref": sid, "delta": "+30m", "reason": "forgot to start" }),
    )
    .expect("a positive delta");
    let got = call(&e, "task.get", json!({ "ref": sid })).unwrap();
    assert_eq!(got["tracked"], "PT1H30M");
    assert_eq!(got["tracked_adjustment"], "PT30M");
}

#[test]
fn an_adjustment_survives_export_and_import() {
    let e = engine();
    let sid = add(&e, "timed", json!({}));
    call(
        &e,
        "task.modify",
        json!({ "ref": sid, "set": { "tracked": "3h" } }),
    )
    .unwrap();
    call(
        &e,
        "task.adjust_tracked",
        json!({ "ref": sid, "delta": "-1h", "reason": "idle" }),
    )
    .unwrap();
    let doc = call(&e, "store.export", json!({})).unwrap();

    let fresh = engine();
    call(&fresh, "store.import", doc).expect("import");
    let got = call(&fresh, "task.get", json!({ "ref": sid })).unwrap();
    assert_eq!(got["tracked"], "PT2H");
    assert_eq!(got["tracked_adjustment"], "-PT1H");
}
