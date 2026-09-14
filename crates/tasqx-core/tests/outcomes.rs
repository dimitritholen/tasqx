//! Tests for `report.outcomes` (D137) — the read that measures whether the
//! work worked rather than what it cost.
//!
//! Driven through `dispatch` so the D33 params gate is exercised along with
//! the engine, like `tests/memory.rs`. Every figure here is derived from data
//! the store already held before D137, which is the ruling's own claim: no
//! metric below is allowed to need a write that did not already exist.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

/// Add a task and return its `short_id`.
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

/// The one group of an ungrouped-by-project report, for a store whose tasks
/// all sit in one project.
fn only_group(out: &Value) -> &Value {
    let groups = out["groups"].as_array().expect("groups is an array");
    assert_eq!(groups.len(), 1, "expected one group, got {groups:?}");
    &groups[0]
}

// ---- rework ---------------------------------------------------------------

#[test]
fn rework_counts_a_completion_that_was_reopened_and_names_its_refs() {
    let e = engine();
    let a = add(&e, "reopened later", json!({}));
    let b = add(&e, "stayed done", json!({}));
    call(&e, "task.done", json!({ "ref": a })).expect("done a");
    call(&e, "task.done", json!({ "ref": b })).expect("done b");
    call(&e, "task.reopen", json!({ "ref": a })).expect("reopen a");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["rework"] })).expect("outcomes");
    let g = only_group(&out);
    let rework = &g["rework"];
    assert_eq!(rework["count"], 1, "one of the two completions came back");
    assert_eq!(
        rework["n"], 2,
        "the denominator is completions in scope, not tasks"
    );
    assert_eq!(rework["refs"], json!([a]));
    // The rate travels with its own denominator; a bare 0.5 is the thing D137
    // refuses to print.
    assert!((rework["rate"].as_f64().unwrap() - 0.5).abs() < 1e-9);
}

#[test]
fn a_reopen_from_cancelled_is_not_rework() {
    let e = engine();
    let a = add(&e, "cancelled then resurrected", json!({}));
    call(&e, "task.cancel", json!({ "ref": a })).expect("cancel");
    call(&e, "task.reopen", json!({ "ref": a })).expect("reopen");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["rework"] })).expect("outcomes");
    let g = only_group(&out);
    assert_eq!(
        g["rework"]["count"], 0,
        "resurrecting abandoned work is not a completion coming back"
    );
}

// ---- calibration ----------------------------------------------------------

#[test]
fn calibration_is_the_median_ratio_over_completions_that_had_both_numbers() {
    let e = engine();
    // Estimated an hour, took two: ratio 2.
    let a = add(&e, "underestimated", json!({ "estimate": "1h" }));
    call(
        &e,
        "task.modify",
        json!({ "ref": a, "set": { "tracked": "2h" } }),
    )
    .expect("tracked a");
    call(&e, "task.done", json!({ "ref": a })).expect("done a");
    // Estimated two hours, took one: ratio 0.5.
    let b = add(&e, "overestimated", json!({ "estimate": "2h" }));
    call(
        &e,
        "task.modify",
        json!({ "ref": b, "set": { "tracked": "1h" } }),
    )
    .expect("tracked b");
    call(&e, "task.done", json!({ "ref": b })).expect("done b");
    // No estimate: contributes nothing and must not be counted in `n`.
    let c = add(&e, "unestimated", json!({}));
    call(
        &e,
        "task.modify",
        json!({ "ref": c, "set": { "tracked": "9h" } }),
    )
    .expect("tracked c");
    call(&e, "task.done", json!({ "ref": c })).expect("done c");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["calibration"] })).expect("outcomes");
    let cal = &only_group(&out)["calibration"];
    assert_eq!(cal["n"], 2, "only completions carrying both numbers count");
    let median = cal["median_ratio"].as_f64().expect("median_ratio");
    assert!(
        (median - 1.25).abs() < 1e-9,
        "median of 0.5 and 2 is their mean at even n: {median}"
    );
}

#[test]
fn calibration_reports_no_median_rather_than_zero_when_nothing_qualifies() {
    let e = engine();
    let a = add(&e, "no estimate", json!({}));
    call(&e, "task.done", json!({ "ref": a })).expect("done");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["calibration"] })).expect("outcomes");
    let cal = &only_group(&out)["calibration"];
    assert_eq!(cal["n"], 0);
    assert!(
        cal["median_ratio"].is_null(),
        "a median over nothing is null, never 0 — 0 is a real calibration figure"
    );
}

// ---- silent completions ---------------------------------------------------

#[test]
fn a_completion_with_no_annotation_is_silent() {
    let e = engine();
    let a = add(&e, "documented", json!({}));
    call(
        &e,
        "annotation.add",
        json!({ "ref": a, "body": "did the thing, here is why" }),
    )
    .expect("annotate");
    call(&e, "task.done", json!({ "ref": a })).expect("done a");
    let b = add(&e, "undocumented", json!({}));
    call(&e, "task.done", json!({ "ref": b })).expect("done b");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["silent"] })).expect("outcomes");
    let silent = &only_group(&out)["silent"];
    assert_eq!(silent["count"], 1);
    assert_eq!(silent["n"], 2);
    assert_eq!(silent["refs"], json!([b]));
}

// ---- abandonment ----------------------------------------------------------

#[test]
fn abandonment_is_work_that_was_started_and_then_cancelled() {
    let e = engine();
    let worked = add(&e, "started then dropped", json!({}));
    call(&e, "task.start", json!({ "ref": worked })).expect("start");
    call(&e, "task.cancel", json!({ "ref": worked })).expect("cancel");
    // Cancelled without ever being started: not abandoned effort, just a task
    // that went away. Nothing was spent on it.
    let never = add(&e, "cancelled untouched", json!({}));
    call(&e, "task.cancel", json!({ "ref": never })).expect("cancel");
    let done = add(&e, "finished", json!({}));
    call(&e, "task.done", json!({ "ref": done })).expect("done");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["abandonment"] })).expect("outcomes");
    let ab = &only_group(&out)["abandonment"];
    assert_eq!(ab["count"], 1, "only the one that had work put into it");
    assert_eq!(ab["refs"], json!([worked]));
    assert_eq!(
        ab["n"], 3,
        "the denominator is every task that reached a terminal state"
    );
    assert!(
        ab["tracked_total"].is_string(),
        "the time inside abandoned work is the point of counting it"
    );
}

#[test]
fn a_cancellation_driven_through_modify_is_counted_like_task_cancel() {
    let e = engine();
    let a = add(&e, "modify-cancelled", json!({}));
    call(&e, "task.start", json!({ "ref": a })).expect("start");
    call(
        &e,
        "task.modify",
        json!({ "ref": a, "set": { "status": "cancelled" } }),
    )
    .expect("modify-cancel");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["abandonment"] })).expect("outcomes");
    let ab = &only_group(&out)["abandonment"];
    assert_eq!(
        ab["count"], 1,
        "cancelling writes a `modify` event on this path and a `cancel` on the other; \
         the metric reads the task's terminal status, so both land"
    );
}

// ---- cost -----------------------------------------------------------------

#[test]
fn cost_keeps_the_four_buckets_apart_and_never_emits_a_blend() {
    let e = engine();
    let a = add(&e, "measured", json!({}));
    call(
        &e,
        "task.done",
        json!({
            "ref": a,
            "tool": "claude-code",
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_tokens": 5000,
            "cache_creation_tokens": 300
        }),
    )
    .expect("done with a self-report");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["cost"] })).expect("outcomes");
    let cost = &only_group(&out)["cost"];
    // The `report.summary` spelling, not `task.done`'s `input_tokens`: `cost`
    // is a report aggregate and lives in that vocabulary, which is also what
    // lets the shared token render helpers read it unchanged.
    assert_eq!(cost["tokens_in"], 100);
    assert_eq!(cost["tokens_out"], 20);
    assert_eq!(cost["tokens_cache_read"], 5000);
    assert_eq!(cost["tokens_cache_creation"], 300);
    assert_eq!(cost["n"], 1, "one completion contributed a measurement");
    // D48/D50/D103: a blended total is never a reported field.
    for blended in ["total", "tokens_total", "total_tokens"] {
        assert!(
            cost.get(blended).is_none(),
            "{blended} is a blend and D137 refuses to print one"
        );
    }
    assert_eq!(
        cost["confidence"], "medium",
        "a self-report is plausible-but-unproven (D50), and the grading travels with the sum"
    );
}

// ---- scope, grouping and the envelope -------------------------------------

#[test]
fn group_by_project_splits_every_metric_and_the_default_is_one_group() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "alpha" })).expect("alpha");
    call(&e, "project.create", json!({ "name": "beta" })).expect("beta");
    let a = add(&e, "in alpha", json!({ "project": "alpha" }));
    let b = add(&e, "in beta", json!({ "project": "beta" }));
    call(&e, "task.done", json!({ "ref": a })).expect("done a");
    call(&e, "task.done", json!({ "ref": b })).expect("done b");
    call(&e, "task.reopen", json!({ "ref": a })).expect("reopen a");

    let out = call(
        &e,
        "report.outcomes",
        json!({ "group_by": "project", "metrics": ["rework"] }),
    )
    .expect("outcomes");
    let groups = out["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 2);
    let alpha = groups.iter().find(|g| g["project"] == "alpha").unwrap();
    let beta = groups.iter().find(|g| g["project"] == "beta").unwrap();
    assert_eq!(alpha["rework"]["count"], 1);
    assert_eq!(alpha["rework"]["n"], 1);
    assert_eq!(beta["rework"]["count"], 0);
    assert_eq!(beta["rework"]["n"], 1);
}

#[test]
fn the_filter_decides_which_tasks_the_report_is_about() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "alpha" })).expect("alpha");
    call(&e, "project.create", json!({ "name": "beta" })).expect("beta");
    let a = add(&e, "in alpha", json!({ "project": "alpha" }));
    let b = add(&e, "in beta", json!({ "project": "beta" }));
    call(&e, "task.done", json!({ "ref": a })).expect("done a");
    call(&e, "task.done", json!({ "ref": b })).expect("done b");
    call(&e, "task.reopen", json!({ "ref": b })).expect("reopen b");

    let out = call(
        &e,
        "report.outcomes",
        json!({ "filter": "project:alpha", "metrics": ["rework"] }),
    )
    .expect("outcomes");
    let g = only_group(&out);
    assert_eq!(g["rework"]["n"], 1, "beta's completion is out of scope");
    assert_eq!(g["rework"]["count"], 0, "and so is beta's reopen");
    assert_eq!(
        out["filter"], "project:alpha",
        "the scope is echoed, because a rate read against the wrong scope is worse \
         than no rate (D69)"
    );
}

#[test]
fn the_window_bounds_when_the_work_closed_not_when_the_task_was_made() {
    let e = engine();
    let a = add(&e, "closed today", json!({}));
    call(&e, "task.done", json!({ "ref": a })).expect("done");

    // A window that ends before anything closed sees no completions at all.
    let out = call(
        &e,
        "report.outcomes",
        json!({ "since": "2020-01-01", "until": "2020-01-02", "metrics": ["rework"] }),
    )
    .expect("outcomes");
    assert_eq!(
        out["groups"].as_array().unwrap().len(),
        0,
        "nothing closed inside the window"
    );
    assert!(
        out["since"].is_string(),
        "the window is echoed like `filter`"
    );
    assert!(out["until"].is_string());
}

#[test]
fn every_metric_is_reported_when_none_is_named() {
    let e = engine();
    let a = add(&e, "one completion", json!({}));
    call(&e, "task.done", json!({ "ref": a })).expect("done");

    let out = call(&e, "report.outcomes", json!({})).expect("outcomes");
    let g = only_group(&out);
    for m in ["rework", "calibration", "cost", "silent", "abandonment"] {
        assert!(g.get(m).is_some(), "{m} missing from a default report: {g}");
    }
}

#[test]
fn an_unknown_metric_is_refused_rather_than_dropped() {
    let e = engine();
    let err = call(&e, "report.outcomes", json!({ "metrics": ["rewrok"] }))
        .expect_err("a typo must not answer ok with the column missing");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("rework"),
        "the refusal names the valid set: {}",
        err.message
    );
}

#[test]
fn an_unknown_group_by_is_refused() {
    let e = engine();
    let err = call(&e, "report.outcomes", json!({ "group_by": "assignee" }))
        .expect_err("a closed vocabulary refuses a word it does not have (D34)");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn a_param_the_method_does_not_read_is_refused_at_dispatch() {
    let e = engine();
    let err = call(&e, "report.outcomes", json!({ "all": true }))
        .expect_err("D31: the params gate refuses a key the method does not accept");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn an_empty_store_says_so_rather_than_looking_like_an_empty_filter() {
    let e = engine();
    let out = call(&e, "report.outcomes", json!({})).expect("outcomes");
    assert_eq!(out["groups"], json!([]));
    assert_eq!(
        out["store_empty"], true,
        "an empty result on an empty store reads identically to a filter that \
         matched nothing, and only this tells them apart"
    );
}

#[test]
fn the_report_writes_nothing() {
    let e = engine();
    let a = add(&e, "a task", json!({}));
    call(&e, "task.done", json!({ "ref": a })).expect("done");
    let before = call(&e, "event.list", json!({})).expect("events")["events"]
        .as_array()
        .expect("events array")
        .len();
    call(&e, "report.outcomes", json!({})).expect("outcomes");
    let after = call(&e, "event.list", json!({})).expect("events")["events"]
        .as_array()
        .expect("events array")
        .len();
    assert_eq!(before, after, "a read appends nothing to the log");
}
