//! Completing a task whose dependencies are still open (D149).
//!
//! Finding #626: `task.done` used to complete a blocked task silently. The
//! store knew the blocker was open — `task.get` said `blocked: true` on the
//! same task a moment earlier — and said nothing at the one moment the answer
//! would have changed what the caller did next.
//!
//! So the completion is REFUSED, naming the open blockers, and `force: true`
//! completes it anyway with the override recorded on the completion event.
//! Refusal rather than a warning field on a success, because a warning on a
//! success is exactly the silent shape #626 complained about: the caller that
//! ignored the blocked flag would ignore the warning too, and nothing would
//! remain in the store to count afterwards.
//!
//! Driven through `dispatch` so the D33 params gate is exercised along with
//! the engine, like `tests/outcomes.rs`.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn add(e: &Engine, title: &str) -> i64 {
    call(e, "task.add", json!({ "title": title })).expect("task.add")["short_id"]
        .as_i64()
        .expect("short_id")
}

/// `depends_on` blocks `task`: the edge every test here is about.
fn depend(e: &Engine, task: i64, depends_on: i64) {
    call(
        e,
        "dependency.add",
        json!({ "ref": task, "depends_on": depends_on }),
    )
    .expect("dependency.add");
}

fn status_of(e: &Engine, r: i64) -> String {
    call(e, "task.get", json!({ "ref": r })).expect("task.get")["status"]
        .as_str()
        .expect("a status is a string")
        .to_string()
}

/// Every event on `r`, newest first — the audit record the override is written
/// into, read the way `event.list` publishes it.
fn events_of(e: &Engine, r: i64) -> Vec<Value> {
    call(e, "event.list", json!({ "ref": r, "limit": 100 })).expect("event.list")["events"]
        .as_array()
        .expect("events is an array")
        .clone()
}

fn done_payload(e: &Engine, r: i64) -> Value {
    events_of(e, r)
        .into_iter()
        .find(|ev| ev["op"] == "done")
        .expect("the completion wrote a done event")["payload"]
        .clone()
}

// ---- the refusal ----------------------------------------------------------

#[test]
fn completing_a_task_whose_blocker_is_still_open_is_refused_and_writes_nothing() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let blocker = add(&e, "the blocker");
    depend(&e, dependent, blocker);

    let err = call(&e, "task.done", json!({ "ref": dependent }))
        .expect_err("D149: an open blocker refuses the completion");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains(&format!(
            "cannot complete #{dependent}: blocked by #{blocker} (pending)"
        )),
        "the refusal has to NAME the blocker and its status — \"blocked\" alone sends the \
         caller back to `task.get` to find out by what: {}",
        err.message
    );
    assert!(
        err.message.contains("force"),
        "a refusal that does not say how to override it is a wall: {}",
        err.message
    );

    assert_eq!(
        status_of(&e, dependent),
        "pending",
        "the refusal is whole: nothing about the task moved"
    );
    assert!(
        !events_of(&e, dependent).iter().any(|ev| ev["op"] == "done"),
        "a refused completion may not leave a done event behind"
    );
}

#[test]
fn every_open_blocker_is_named_in_short_id_order_with_its_status() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let first = add(&e, "still pending");
    let second = add(&e, "being worked");
    // Added in reverse, so the ORDER in the message is the query's and not the
    // order the edges happened to land in.
    depend(&e, dependent, second);
    depend(&e, dependent, first);
    call(&e, "task.start", json!({ "ref": second })).expect("start");

    let err =
        call(&e, "task.done", json!({ "ref": dependent })).expect_err("two open blockers, still");
    assert!(
        err.message.contains(&format!(
            "cannot complete #{dependent}: blocked by #{first} (pending), #{second} (active)"
        )),
        "both blockers, in short_id order, each with the status that says how far off it is: {}",
        err.message
    );
}

/// The `status` column is a CACHE for a deferred task: a blocker whose `wait`
/// has passed still reads `backlog` in SQL until some verb rewrites the row,
/// and every read in the store resolves that through `effective_status`. A
/// refusal naming `(backlog)` for a blocker that is ready to be picked up
/// would be a message `task.get` contradicts on the same task.
#[test]
fn a_deferred_blocker_is_named_by_the_status_every_other_read_gives_it() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let blocker = call(
        &e,
        "task.add",
        json!({ "title": "parked", "wait": "2099-01-01T00:00:00Z" }),
    )
    .expect("task.add")["short_id"]
        .as_i64()
        .expect("short_id");
    depend(&e, dependent, blocker);

    let err = call(&e, "task.done", json!({ "ref": dependent })).expect_err("still blocked");
    assert!(
        err.message.contains(&format!("#{blocker} (backlog)")),
        "a blocker still parked behind a future wait says so: {}",
        err.message
    );

    // The wait moves into the past. No verb rewrites `status`, so SQL still
    // says `backlog` and every reader says `pending`.
    call(
        &e,
        "task.modify",
        json!({ "ref": blocker, "set": { "wait": "2020-01-01T00:00:00Z" } }),
    )
    .expect("task.modify");
    assert_eq!(status_of(&e, blocker), "pending", "the store's own answer");

    let err = call(&e, "task.done", json!({ "ref": dependent })).expect_err("still blocked");
    assert!(
        err.message.contains(&format!("#{blocker} (pending)")),
        "the refusal must not print the cached column: {}",
        err.message
    );
}

#[test]
fn a_blocker_that_closed_is_not_an_unmet_one() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let finished = add(&e, "finished");
    let dropped = add(&e, "dropped");
    depend(&e, dependent, finished);
    depend(&e, dependent, dropped);
    call(&e, "task.done", json!({ "ref": finished })).expect("done the blocker");
    call(&e, "task.cancel", json!({ "ref": dropped })).expect("cancel the blocker");

    call(&e, "task.done", json!({ "ref": dependent }))
        .expect("D145: resolved means done OR cancelled, and neither still blocks");
    assert_eq!(status_of(&e, dependent), "done");
}

#[test]
fn the_check_runs_on_every_completion_not_only_the_first() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let blocker = add(&e, "the blocker");
    depend(&e, dependent, blocker);
    call(&e, "task.done", json!({ "ref": dependent, "force": true })).expect("forced");
    call(&e, "task.reopen", json!({ "ref": dependent })).expect("reopen");

    let err = call(&e, "task.done", json!({ "ref": dependent }))
        .expect_err("the blocker is still open, so the second completion is refused too");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains(&format!("blocked by #{blocker}")),
        "one override does not buy the next one: {}",
        err.message
    );
}

#[test]
fn a_closed_task_still_gets_its_status_refusal_rather_than_the_blocker_one() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let blocker = add(&e, "the blocker");
    depend(&e, dependent, blocker);
    call(&e, "task.cancel", json!({ "ref": dependent })).expect("cancel");

    let err = call(&e, "task.done", json!({ "ref": dependent }))
        .expect_err("a cancelled task cannot be completed at all");
    assert!(
        err.message.contains("cancelled"),
        "the blocker check runs AFTER the status match, so the caller is told the thing that \
         actually stops them: {}",
        err.message
    );
}

#[test]
fn cancelling_blocked_work_is_what_cancelling_is_for_and_needs_no_force() {
    let e = engine();
    let blocker = add(&e, "the blocker");
    let by_cancel = add(&e, "dropped through task.cancel");
    let by_modify = add(&e, "dropped through task.modify");
    depend(&e, by_cancel, blocker);
    depend(&e, by_modify, blocker);

    call(&e, "task.cancel", json!({ "ref": by_cancel }))
        .expect("a cancel closes nothing that depended on the blocker");
    call(
        &e,
        "task.modify",
        json!({ "ref": by_modify, "set": { "status": "cancelled" } }),
    )
    .expect("§7's second path to cancellation answers the same way");

    assert_eq!(status_of(&e, by_cancel), "cancelled");
    assert_eq!(status_of(&e, by_modify), "cancelled");
}

// ---- the override ---------------------------------------------------------

#[test]
fn force_completes_the_blocked_task_and_names_what_it_overrode() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let blocker = add(&e, "the blocker");
    depend(&e, dependent, blocker);

    let out = call(&e, "task.done", json!({ "ref": dependent, "force": true }))
        .expect("force completes it anyway");
    assert_eq!(out["status"], "done");
    assert_eq!(out["forced"], true);
    assert_eq!(
        out["blocked_by"],
        json!([{ "short_id": blocker, "title": "the blocker" }]),
        "the response names what was overridden in the row shape `unmet_blockers` uses, so the \
         caller reads one vocabulary for blockers and not two"
    );
    assert_eq!(status_of(&e, dependent), "done");
}

#[test]
fn the_override_is_recorded_on_the_completion_event() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let blocker = add(&e, "the blocker");
    depend(&e, dependent, blocker);
    call(&e, "task.done", json!({ "ref": dependent, "force": true })).expect("forced");

    let payload = done_payload(&e, dependent);
    assert_eq!(
        payload["forced"], true,
        "the durable half: a response is gone after the turn, and `report.outcomes` counts \
         this from the log — {payload}"
    );
    assert_eq!(
        payload["blocked_by"],
        json!([blocker]),
        "short_ids, because the event is the record of WHICH blockers were open at that \
         instant and the titles can be re-read: {payload}"
    );
}

#[test]
fn force_on_an_unblocked_task_is_a_no_op_rather_than_an_error() {
    let e = engine();
    let unblocked = add(&e, "nothing blocks this");

    let out = call(&e, "task.done", json!({ "ref": unblocked, "force": true }))
        .expect("force is an override, and there is nothing to override");
    assert!(
        out.get("forced").is_none() && out.get("blocked_by").is_none(),
        "an override that overrode nothing must not claim one — the metric it feeds would \
         count completions that were never blocked: {out}"
    );

    let payload = done_payload(&e, unblocked);
    assert!(
        payload.get("forced").is_none() && payload.get("blocked_by").is_none(),
        "and nothing extra reaches the durable record either: {payload}"
    );
}

#[test]
fn a_completion_with_a_closed_blocker_answers_exactly_as_it_did_before_d149() {
    let e = engine();
    let dependent = add(&e, "the dependent");
    let blocker = add(&e, "the blocker");
    depend(&e, dependent, blocker);
    call(&e, "task.done", json!({ "ref": blocker })).expect("done the blocker");

    let out = call(&e, "task.done", json!({ "ref": dependent })).expect("done");
    assert!(
        out.get("forced").is_none() && out.get("blocked_by").is_none(),
        "the unblocked path is byte-identical to what every existing client already reads: \
         {out}"
    );
}

#[test]
fn force_is_refused_on_a_method_that_does_not_read_it() {
    let e = engine();
    let blocker = add(&e, "the blocker");
    let dependent = add(&e, "the dependent");
    depend(&e, dependent, blocker);

    let err = call(
        &e,
        "task.cancel",
        json!({ "ref": dependent, "force": true }),
    )
    .expect_err("D33: the params gate refuses a key the method does not accept");
    assert_eq!(
        err.code,
        ErrorCode::BadRequest,
        "cancelling a blocked task was never refused, so there is nothing here to force — a \
         tolerated `force` would teach the caller a flag that does nothing"
    );
}
