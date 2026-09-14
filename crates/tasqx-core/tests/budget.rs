//! Tests for `budget_tokens` (D139) — a size gauge over fresh tokens that
//! stops nothing.
//!
//! The gauge counts `input + output + cache_creation` and excludes cache
//! reads. That is not a blend D48/D50 forbid: those rulings govern a COST
//! report, where blending destroys the split a reader needs. This is a size
//! gauge — one number to compare against a threshold — and the same fact that
//! forbids blending in a bill decides the definition here, because a budget
//! dominated by cache reads measures how often the agent re-read its own
//! context rather than how much work the task was.

use serde_json::{json, Value};
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
        .expect("short_id")
}

fn get(e: &Engine, r: i64) -> Value {
    call(e, "task.get", json!({ "ref": r })).expect("task.get")
}

/// Bank a measurement on a task without completing it.
fn spend(e: &Engine, r: i64, input: i64, output: i64, cache_read: i64, cache_creation: i64) {
    call(
        e,
        "token.add",
        json!({
            "ref": r, "tool": "claude-code", "source": "self-report", "confidence": "medium",
            "input_tokens": input, "output_tokens": output,
            "cache_read_tokens": cache_read, "cache_creation_tokens": cache_creation
        }),
    )
    .expect("token.add");
}

// ---- the field ------------------------------------------------------------

#[test]
fn a_task_with_no_budget_reports_null_and_no_verdict() {
    let e = engine();
    let a = add(&e, "unbudgeted", json!({}));
    let t = get(&e, a);
    assert!(
        t["budget_tokens"].is_null(),
        "null, never a silent zero — a zero budget is a real thing to set"
    );
    assert!(
        t["over"].is_null(),
        "there is no verdict to give without a threshold: {t}"
    );
    // The spend itself is always reported: it is a fact about the task whether
    // or not anybody set a budget to read it against.
    assert_eq!(t["fresh_tokens"], 0);
}

#[test]
fn a_budget_is_set_on_add_and_changed_on_modify() {
    let e = engine();
    let a = add(&e, "budgeted", json!({ "budget_tokens": 50_000 }));
    assert_eq!(get(&e, a)["budget_tokens"], 50_000);

    call(
        &e,
        "task.modify",
        json!({ "ref": a, "set": { "budget_tokens": 80_000 } }),
    )
    .expect("modify");
    assert_eq!(get(&e, a)["budget_tokens"], 80_000);
}

#[test]
fn a_budget_is_cleared_by_setting_it_null() {
    let e = engine();
    let a = add(&e, "budgeted", json!({ "budget_tokens": 50_000 }));
    call(
        &e,
        "task.modify",
        json!({ "ref": a, "set": { "budget_tokens": null } }),
    )
    .expect("D13: --clear is the only way to unset");
    assert!(get(&e, a)["budget_tokens"].is_null());
}

#[test]
fn a_negative_budget_is_refused_at_the_parse_boundary() {
    let e = engine();
    let err = call(&e, "task.add", json!({ "title": "t", "budget_tokens": -1 }))
        .expect_err("D45: a value the caller supplies is refused where it is parsed");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

// ---- the gauge ------------------------------------------------------------

#[test]
fn fresh_tokens_excludes_cache_reads() {
    let e = engine();
    let a = add(&e, "measured", json!({}));
    spend(&e, a, 100, 20, 9_000, 300);
    let t = get(&e, a);
    assert_eq!(
        t["fresh_tokens"], 420,
        "input + output + cache_creation; the 9000 replayed tokens are not work"
    );
    // The four buckets keep their own reporting — the gauge is a second
    // reading of the same rows, never a replacement for them.
    let m = &t["tokens"][0];
    assert_eq!(m["cache_read_tokens"], 9_000);
}

#[test]
fn over_is_true_only_once_the_gauge_passes_the_threshold() {
    let e = engine();
    let a = add(&e, "budgeted", json!({ "budget_tokens": 1_000 }));
    spend(&e, a, 400, 100, 50_000, 200);
    let t = get(&e, a);
    assert_eq!(t["fresh_tokens"], 700);
    assert_eq!(
        t["over"], false,
        "50k cache reads must not blow a budget about how big the WORK was: {t}"
    );

    spend(&e, a, 400, 0, 0, 0);
    let t = get(&e, a);
    assert_eq!(t["fresh_tokens"], 1_100);
    assert_eq!(t["over"], true);
}

#[test]
fn the_brief_carries_the_gauge_too() {
    let e = engine();
    let a = add(&e, "budgeted", json!({ "budget_tokens": 100 }));
    spend(&e, a, 200, 0, 0, 0);
    // The brief is what an agent reads BEFORE starting, which is the one moment
    // knowing the budget can still change what it does.
    let b = call(&e, "task.brief", json!({ "ref": a })).expect("brief");
    assert_eq!(b["task"]["budget_tokens"], 100);
    assert_eq!(b["task"]["fresh_tokens"], 200);
    assert_eq!(b["task"]["over"], true);
}

// ---- completion, and the report ------------------------------------------

#[test]
fn completing_over_budget_says_so_and_does_not_refuse() {
    let e = engine();
    let a = add(&e, "budgeted", json!({ "budget_tokens": 100 }));
    let out = call(
        &e,
        "task.done",
        json!({ "ref": a, "tool": "claude-code", "input_tokens": 900, "output_tokens": 50 }),
    )
    .expect("a budget stops nothing — tasqx is not in the loop and cannot be");
    assert_eq!(out["status"], "done");
    // Its OWN field, not folded into `tokens_hint`: that hint is about
    // measurement channels and is only emitted when NO counts were supplied,
    // so an overrun folded into it would be hidden on exactly the completions
    // where the spend is known.
    let hint = out["budget_hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("over budget"),
        "the overrun is named where the caller will see it: {out}"
    );
}

#[test]
fn outcomes_counts_overruns_beside_its_denominator() {
    let e = engine();
    let over = add(&e, "blew it", json!({ "budget_tokens": 100 }));
    call(
        &e,
        "task.done",
        json!({ "ref": over, "tool": "claude-code", "input_tokens": 900 }),
    )
    .expect("done over");
    let under = add(&e, "fine", json!({ "budget_tokens": 10_000 }));
    call(
        &e,
        "task.done",
        json!({ "ref": under, "tool": "claude-code", "input_tokens": 90 }),
    )
    .expect("done under");
    // No budget: cannot be an overrun, and must not pad the denominator either
    // — a rate over "every completion" would shrink as unbudgeted work landed.
    let unbudgeted = add(&e, "no threshold", json!({}));
    call(&e, "task.done", json!({ "ref": unbudgeted })).expect("done unbudgeted");

    let out = call(&e, "report.outcomes", json!({ "metrics": ["overrun"] })).expect("outcomes");
    let groups = out["groups"].as_array().expect("groups");
    let g = &groups[0];
    assert_eq!(g["overrun"]["count"], 1);
    assert_eq!(
        g["overrun"]["n"], 2,
        "the denominator is completions that HAD a budget"
    );
    assert_eq!(g["overrun"]["refs"], json!([over]));
}

#[test]
fn an_export_round_trip_keeps_the_budget() {
    let e = engine();
    add(&e, "budgeted", json!({ "budget_tokens": 4_242 }));
    let doc = call(&e, "store.export", json!({})).expect("export");
    assert_eq!(doc["tasks"][0]["budget_tokens"], 4_242);

    let fresh = engine();
    call(
        &fresh,
        "store.import",
        json!({ "tasks": doc["tasks"], "projects": doc["projects"] }),
    )
    .expect("import");
    assert_eq!(get(&fresh, 1)["budget_tokens"], 4_242);
}
