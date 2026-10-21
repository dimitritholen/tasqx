//! Tests for D140 — D6's auto-stop narrowed to the caller's own clock.
//!
//! The defect is quiet: two agents on one store, neither passing `keep`, and
//! agent B's `task.start` stops every active row. Nothing is lost — the elapsed
//! seconds are banked — but agent A's task leaves `active` while A is still
//! working on it, every second after that is untracked, and `task.done` later
//! accepts the `pending` task without comment. The auto-stop is reported in B's
//! response; A is never told.
//!
//! That matters beyond the timer: `report.outcomes` computes calibration as
//! median `tracked / estimate`, so under parallel agents the figure reads low
//! by an amount nobody can see.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine};

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

fn status(e: &Engine, r: i64) -> String {
    call(e, "task.get", json!({ "ref": r })).expect("task.get")["status"]
        .as_str()
        .expect("status")
        .to_string()
}

#[test]
fn a_shell_keeps_the_single_clock_it_has_always_had() {
    let e = engine();
    let (a, b) = (add(&e, "first"), add(&e, "second"));
    call(&e, "task.start", json!({ "ref": a })).expect("start a");
    // Neither call names an actor, which is the shell: no party can be shown to
    // be different from any other, so D6's auto-stop applies unchanged.
    let out = call(&e, "task.start", json!({ "ref": b })).expect("start b");
    assert_eq!(status(&e, a), "pending", "the first clock stopped");
    assert_eq!(status(&e, b), "active");
    assert_eq!(
        out["auto_stopped"][0]["short_id"], a,
        "and the caller is told what it stopped"
    );
}

#[test]
fn one_actor_starting_twice_still_auto_stops_its_own_clock() {
    let e = engine();
    let (a, b) = (add(&e, "first"), add(&e, "second"));
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).expect("start a");
    call(&e, "task.start", json!({ "ref": b, "actor": "agent-1" })).expect("start b");
    assert_eq!(
        status(&e, a),
        "pending",
        "moving between tasks is what single-active is FOR"
    );
    assert_eq!(status(&e, b), "active");
}

/// D236 amends D6: single-active is per holder. A second actor's start does
/// not touch the first actor's clock at all — no stop, and no refusal either.
#[test]
fn a_second_actor_starts_beside_the_first_ones_clock() {
    let e = engine();
    let (a, b) = (add(&e, "agent one's work"), add(&e, "agent two's work"));
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).expect("start a");
    let out = call(&e, "task.start", json!({ "ref": b, "actor": "agent-2" })).expect("start b");
    assert_eq!(out["auto_stopped"], json!([]), "{out}");
    assert_eq!(
        status(&e, a),
        "active",
        "agent one's clock is not agent two's to stop"
    );
    assert_eq!(status(&e, b), "active");
}

#[test]
fn keep_still_runs_two_clocks_across_actors() {
    let e = engine();
    let (a, b) = (add(&e, "first"), add(&e, "second"));
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).expect("start a");
    // D6 already ruled deliberate concurrency opt-in, so the escape hatch is
    // not new and the refusal above must not close it.
    call(
        &e,
        "task.start",
        json!({ "ref": b, "actor": "agent-2", "keep": true }),
    )
    .expect("keep is the documented way to say `both, deliberately`");
    assert_eq!(status(&e, a), "active");
    assert_eq!(status(&e, b), "active");
}

/// The shell's clock (no actor) is the shell's (`user`, D236), so an agent's
/// start leaves it running.
#[test]
fn an_agent_does_not_stop_the_shells_clock() {
    let e = engine();
    let (a, b) = (add(&e, "started from a shell"), add(&e, "an agent's work"));
    call(&e, "task.start", json!({ "ref": a })).expect("start a");
    call(&e, "task.start", json!({ "ref": b, "actor": "agent-2" })).expect("start b");
    assert_eq!(status(&e, a), "active");
    assert_eq!(status(&e, b), "active");
}

/// The mirror: the shell's start leaves an agent's clock running.
#[test]
fn the_shell_does_not_stop_an_agents_clock() {
    let e = engine();
    let (a, b) = (add(&e, "an agent's work"), add(&e, "started from a shell"));
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).expect("start a");
    call(&e, "task.start", json!({ "ref": b })).expect("start b");
    assert_eq!(status(&e, a), "active");
    assert_eq!(status(&e, b), "active");
}

#[test]
fn the_actor_is_recorded_on_the_start_event() {
    let e = engine();
    let a = add(&e, "attributed");
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).expect("start");
    let events = call(&e, "event.list", json!({ "ref": a })).expect("events");
    let start = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ev| ev["op"] == "start")
        .expect("a start event");
    assert_eq!(
        start["payload"]["actor"], "agent-1",
        "the durable record is what the next start compares against"
    );
}

#[test]
fn restarting_the_task_that_already_holds_the_clock_is_still_idempotent() {
    let e = engine();
    let a = add(&e, "already running");
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).expect("start");
    // Even from a DIFFERENT actor: this is not a second clock, it is the same
    // one, and D105 already ruled a re-start says so rather than changing
    // anything. Refusing here would turn an observation into an error.
    let out = call(&e, "task.start", json!({ "ref": a, "actor": "agent-2" }))
        .expect("re-starting the running task is an observation, not a claim");
    assert_eq!(out["already_running"], true);
    assert_eq!(status(&e, a), "active");
}

/// The clock belongs to whoever made the LATEST start of the active task: a
/// task started by agent-1, stopped, then restarted by agent-2 is agent-2's,
/// so agent-1's next start leaves it running and agent-2's stops it.
#[test]
fn the_clock_belongs_to_the_most_recent_start_not_the_first() {
    let e = engine();
    let (a, b, c) = (
        add(&e, "handed over"),
        add(&e, "agent one's next"),
        add(&e, "two's next"),
    );
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).expect("start a");
    call(&e, "task.stop", json!({ "ref": a })).expect("stop a");
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-2" })).expect("restart a");

    call(&e, "task.start", json!({ "ref": b, "actor": "agent-1" })).expect("start b");
    assert_eq!(
        status(&e, a),
        "active",
        "agent-2's clock, not agent-1's to stop"
    );
    call(&e, "task.start", json!({ "ref": c, "actor": "agent-2" })).expect("start c");
    assert_eq!(
        status(&e, a),
        "pending",
        "agent-2 moving on stops its own clock"
    );
}
