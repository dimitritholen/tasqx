//! D236: task claims with leases, so two agents never work the same task.
//!
//! The holder is D140's actor key (`actor`, else `user`); the lease is two
//! nullable columns whose liveness is decided on read. Expiry needs the clock
//! to move, so the expiry and race checks run the real binary under
//! `TASQX_NOW` in `tasqx-cli/tests/claims.rs`; these run in process.

use serde_json::{json, Value};
use tasqx_core::{dispatch, ApiError, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, ApiError> {
    dispatch(e, method, &params)
}

fn add(e: &Engine, title: &str) -> i64 {
    call(e, "task.add", json!({ "title": title })).expect("task.add")["short_id"]
        .as_i64()
        .expect("short_id")
}

fn claim(e: &Engine, r: i64, actor: &str) -> Result<Value, ApiError> {
    call(e, "task.claim", json!({ "ref": r, "actor": actor }))
}

fn events(e: &Engine, r: i64) -> Vec<Value> {
    call(e, "event.list", json!({ "ref": r })).expect("event.list")["events"]
        .as_array()
        .expect("events")
        .clone()
}

fn ids(list: &Value) -> Vec<i64> {
    list["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|t| t["short_id"].as_i64().expect("short_id"))
        .collect()
}

#[test]
fn a_claim_names_its_holder_and_a_default_thirty_minute_lease() {
    let e = engine();
    let t = add(&e, "work");
    let out = claim(&e, t, "agent-1").expect("claim");
    assert_eq!(out["claimed_by"], "agent-1", "{out}");
    assert_eq!(out["op"], "claim", "{out}");
    let expires: jiff::Timestamp = out["claim_expires"].as_str().unwrap().parse().unwrap();
    let left = expires.duration_since(jiff::Timestamp::now()).as_secs();
    assert!((29 * 60..=30 * 60).contains(&left), "{left}s left");

    let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(got["claimed_by"], "agent-1");
    assert_eq!(got["claim_expires"], out["claim_expires"]);
    let brief = call(&e, "task.brief", json!({ "ref": t })).unwrap();
    assert_eq!(brief["task"]["claimed_by"], "agent-1", "{brief}");
}

#[test]
fn a_second_holder_on_a_live_lease_is_refused_naming_holder_and_expiry() {
    let e = engine();
    let t = add(&e, "contested");
    let first = claim(&e, t, "agent-1").unwrap();
    let err = claim(&e, t, "agent-2").expect_err("live lease");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("agent-1"), "{}", err.message);
    assert!(
        err.message
            .contains(first["claim_expires"].as_str().unwrap()),
        "{}",
        err.message
    );
}

#[test]
fn the_holder_renews_and_releases_and_nobody_else_can_release() {
    let e = engine();
    let t = add(&e, "mine");
    claim(&e, t, "agent-1").unwrap();
    let renewed = call(
        &e,
        "task.claim",
        json!({ "ref": t, "actor": "agent-1", "lease": "2h" }),
    )
    .expect("renew");
    assert_eq!(renewed["op"], "renew");
    let expires: jiff::Timestamp = renewed["claim_expires"].as_str().unwrap().parse().unwrap();
    assert!(expires.duration_since(jiff::Timestamp::now()).as_secs() > 60 * 60);

    let err = call(&e, "task.release", json!({ "ref": t, "actor": "agent-2" }))
        .expect_err("not the holder");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("agent-1"), "{}", err.message);

    call(&e, "task.release", json!({ "ref": t, "actor": "agent-1" })).expect("release");
    let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(got["claimed_by"], Value::Null);
    claim(&e, t, "agent-2").expect("free again");
}

#[test]
fn a_lease_is_bounded_by_a_day_and_must_be_positive() {
    let e = engine();
    let t = add(&e, "bounded");
    let err = call(&e, "task.claim", json!({ "ref": t, "lease": "25h" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
    call(&e, "task.claim", json!({ "ref": t, "lease": "24h" })).expect("a day is the cap");
    let err = call(&e, "task.claim", json!({ "ref": t, "lease": "soon" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn a_shell_with_no_actor_holds_as_user() {
    let e = engine();
    let t = add(&e, "shell");
    let out = call(&e, "task.claim", json!({ "ref": t })).unwrap();
    assert_eq!(out["claimed_by"], "user");
}

#[test]
fn starting_under_another_holders_lease_is_refused_and_the_holders_start_renews() {
    let e = engine();
    let t = add(&e, "leased");
    call(
        &e,
        "task.claim",
        json!({ "ref": t, "actor": "agent-1", "lease": "1m" }),
    )
    .unwrap();

    let err = call(&e, "task.start", json!({ "ref": t, "actor": "agent-2" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("agent-1"), "{}", err.message);
    // A shell names no actor, which is holder `user`: not agent-1 either.
    let err = call(&e, "task.start", json!({ "ref": t })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);

    call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).expect("holder starts");
    let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
    let expires: jiff::Timestamp = got["claim_expires"].as_str().unwrap().parse().unwrap();
    assert!(
        expires.duration_since(jiff::Timestamp::now()).as_secs() > 25 * 60,
        "the start renewed the one-minute lease: {got}"
    );
    let ops: Vec<String> = events(&e, t)
        .iter()
        .map(|ev| ev["op"].as_str().unwrap().to_string())
        .collect();
    assert!(ops.contains(&"renew".to_string()), "{ops:?}");
}

#[test]
fn done_and_cancel_end_the_claim() {
    let e = engine();
    let (a, b) = (add(&e, "a"), add(&e, "b"));
    claim(&e, a, "agent-1").unwrap();
    claim(&e, b, "agent-1").unwrap();
    call(&e, "task.done", json!({ "ref": a, "actor": "agent-1" })).unwrap();
    call(&e, "task.cancel", json!({ "ref": b, "actor": "agent-1" })).unwrap();
    for r in [a, b] {
        let got = call(&e, "task.get", json!({ "ref": r })).unwrap();
        assert_eq!(got["claimed_by"], Value::Null, "{got}");
        assert_eq!(got["claim_expires"], Value::Null, "{got}");
    }
}

#[test]
fn next_and_working_skip_another_holders_live_claim_and_claimed_lists_it() {
    let e = engine();
    let hot = call(&e, "task.add", json!({ "title": "hot", "priority": "H" })).unwrap()["short_id"]
        .as_i64()
        .unwrap();
    let cold = add(&e, "cold");
    claim(&e, hot, "agent-1").unwrap();

    let next = |actor: Option<&str>| {
        let mut p = json!({});
        if let Some(a) = actor {
            p["actor"] = json!(a);
        }
        call(&e, "task.next", p).unwrap()["task"]["short_id"].clone()
    };
    assert_eq!(next(Some("agent-1")), hot, "the holder still sees its own");
    assert_eq!(next(Some("agent-2")), cold, "another holder skips it");
    assert_eq!(
        next(None),
        cold,
        "no holder: every live claim is someone else's"
    );

    let working = |actor: &str| {
        ids(&call(
            &e,
            "task.list",
            json!({ "filter": "@working", "actor": actor }),
        )
        .unwrap())
    };
    assert_eq!(working("agent-1"), vec![hot, cold]);
    assert_eq!(working("agent-2"), vec![cold]);

    let claimed = call(&e, "task.list", json!({ "filter": "@claimed" })).unwrap();
    assert_eq!(ids(&claimed), vec![hot]);
    // Only `@working` with an actor chooses; any other list is a view and
    // shows the claim, an actor named or not (review 8).
    assert_eq!(ids(&call(&e, "task.list", json!({})).unwrap()).len(), 2);
    assert_eq!(
        ids(&call(&e, "task.list", json!({ "actor": "agent-2" })).unwrap()).len(),
        2
    );
}

#[test]
fn claim_renew_release_are_events_carrying_the_holder() {
    let e = engine();
    let t = add(&e, "logged");
    claim(&e, t, "agent-1").unwrap();
    claim(&e, t, "agent-1").unwrap();
    call(&e, "task.release", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    let evs = events(&e, t);
    for op in ["claim", "renew", "release"] {
        let ev = evs
            .iter()
            .find(|ev| ev["op"] == op)
            .unwrap_or_else(|| panic!("no {op} event in {evs:?}"));
        assert_eq!(ev["actor"], "agent-1", "{ev}");
        assert_eq!(ev["payload"]["holder"], "agent-1", "{ev}");
    }
}

#[test]
fn a_claim_survives_an_export_and_import_round_trip() {
    let a = engine();
    let t = add(&a, "travels");
    let claimed = claim(&a, t, "agent-1").unwrap();
    let doc = call(&a, "store.export", json!({})).unwrap();
    let row = &doc["tasks"][0];
    assert_eq!(row["claimed_by"], "agent-1", "{row}");
    assert_eq!(row["claim_expires"], claimed["claim_expires"]);

    let b = engine();
    call(&b, "store.import", doc.clone()).expect("import");
    let got = call(&b, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(got["claimed_by"], "agent-1");
    assert_eq!(got["claim_expires"], claimed["claim_expires"]);

    // An unclaimed task's export carries neither key (the closed import gate).
    let c = engine();
    add(&c, "free");
    let doc = call(&c, "store.export", json!({})).unwrap();
    assert!(doc["tasks"][0].get("claimed_by").is_none(), "{doc}");
}

#[test]
fn a_closed_task_cannot_be_claimed() {
    let e = engine();
    let t = add(&e, "finished");
    call(&e, "task.done", json!({ "ref": t })).unwrap();
    let err = claim(&e, t, "agent-1").unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

/// Over MCP the holder is the connection (D140's minted id), so two servers on
/// one store are two holders without either naming itself.
#[test]
fn two_mcp_connections_are_two_holders() {
    use tasqx_core::{McpServer, Scope};
    let e = engine();
    let t = add(&e, "zebrawork");
    let tool = |server: &McpServer, name: &str, arguments: Value| {
        server
            .handle_message(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .expect("a response")
    };
    let (a, b) = (
        McpServer::new(&e, Scope::Write),
        McpServer::new(&e, Scope::Write),
    );
    let won = tool(&a, "tasqx_claim_task", json!({ "ref": t }));
    assert_ne!(won["result"]["isError"], true, "{won}");
    let lost = tool(&b, "tasqx_claim_task", json!({ "ref": t }));
    assert_eq!(lost["result"]["isError"], true, "{lost}");

    let holder = call(&e, "task.get", json!({ "ref": t })).unwrap()["claimed_by"].clone();
    assert!(holder.as_str().unwrap().starts_with("mcp:"), "{holder}");

    let working = |server: &McpServer| {
        let out = tool(server, "tasqx_list_tasks", json!({ "filter": "@working" }));
        out.to_string().contains("zebrawork")
    };
    assert!(working(&a), "the holder keeps it in its working set");
    assert!(!working(&b), "the other connection does not see it");
}

/// Review #1: the already-running answer is no way past a claim.
#[test]
fn another_holders_start_on_a_claimed_running_task_is_refused() {
    let e = engine();
    let t = add(&e, "running");
    claim(&e, t, "agent-1").unwrap();
    call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    let err = call(&e, "task.start", json!({ "ref": t, "actor": "agent-2" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    assert!(err.message.contains("agent-1"), "{}", err.message);
    // The holder's own re-start stays idempotent.
    let again = call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    assert_eq!(again["already_running"], true, "{again}");
}

/// Review #3: no actor is `user` on reads too, so a shell keeps its own claim.
#[test]
fn a_shell_claim_stays_in_the_shells_own_next() {
    let e = engine();
    let t = add(&e, "mine from a shell");
    call(&e, "task.claim", json!({ "ref": t })).unwrap();
    let next = call(&e, "task.next", json!({})).unwrap();
    assert_eq!(next["task"]["short_id"], t, "{next}");
    let other = call(&e, "task.next", json!({ "actor": "agent-2" })).unwrap();
    assert_eq!(other["task"], Value::Null, "{other}");
}

/// Review #5: `force` takes a claim from a holder that is gone, and says so.
#[test]
fn force_releases_another_holders_claim_and_logs_it() {
    let e = engine();
    let t = add(&e, "orphaned");
    claim(&e, t, "mcp:dead").unwrap();
    let err = call(&e, "task.release", json!({ "ref": t, "actor": "agent-2" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let out = call(
        &e,
        "task.release",
        json!({ "ref": t, "actor": "agent-2", "force_claim": true }),
    )
    .unwrap();
    assert_eq!(out["forced"], true, "{out}");
    let ev = events(&e, t)
        .into_iter()
        .find(|ev| ev["op"] == "release")
        .expect("a release event");
    assert_eq!(ev["actor"], "agent-2", "{ev}");
    assert_eq!(ev["payload"]["claim_forced"], "mcp:dead", "{ev}");
    assert_eq!(ev["payload"]["holder"], "mcp:dead", "{ev}");
    claim(&e, t, "agent-2").expect("free again");
}

/// Review #6: the holder's renewing start is one rev bump, not two writes at
/// one rev, so `expected_rev` read before the start is the rev it consumes.
#[test]
fn a_renewing_start_bumps_the_rev_once() {
    let e = engine();
    let t = add(&e, "guarded");
    let rev = claim(&e, t, "agent-1").unwrap()["_rev"].as_i64().unwrap();
    call(
        &e,
        "task.start",
        json!({ "ref": t, "actor": "agent-1", "expected_rev": rev }),
    )
    .expect("the rev the caller read");
    let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(got["_rev"], rev + 1, "{got}");
    let stale = call(&e, "task.stop", json!({ "ref": t, "expected_rev": rev })).unwrap_err();
    assert_eq!(stale.code, ErrorCode::Conflict);
}

/// Review #7: a lease refusal speaks of a lease.
#[test]
fn a_zero_lease_is_refused_as_a_lease() {
    let e = engine();
    let t = add(&e, "zero");
    let err = call(&e, "task.claim", json!({ "ref": t, "lease": "0m" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert_eq!(err.message, "a lease must be greater than zero");
    let err = call(&e, "task.claim", json!({ "ref": t, "lease": "soon" })).unwrap_err();
    assert!(err.message.contains("lease"), "{}", err.message);
    assert!(!err.message.contains("estimate"), "{}", err.message);
}

/// Review 8 #2: single-active is per holder. A start leaves another holder's
/// running task alone — no stop, no report, no refusal.
#[test]
fn a_start_never_touches_another_holders_running_task() {
    let e = engine();
    let (a, b) = (add(&e, "agent's"), add(&e, "mine"));
    claim(&e, a, "agent-1").unwrap();
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).unwrap();
    let out = call(&e, "task.start", json!({ "ref": b })).unwrap();
    assert_eq!(out["auto_stopped"], json!([]), "{out}");
    assert!(out.get("kept_running").is_none(), "{out}");
    for t in [a, b] {
        let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
        assert_eq!(got["status"], "active", "#{t}: {got}");
    }
}
/// Review 3 #2: a task running on another actor's clock cannot be claimed; a
/// shell's anonymous clock names nobody to defer to.
#[test]
fn claiming_a_task_on_another_actors_clock_is_refused() {
    let e = engine();
    let (a, b) = (add(&e, "agent clock"), add(&e, "shell clock"));
    call(
        &e,
        "task.start",
        json!({ "ref": a, "actor": "agent-1", "keep": true }),
    )
    .unwrap();
    let err = claim(&e, a, "agent-2").unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("agent-1"), "{}", err.message);
    claim(&e, a, "agent-1").expect("the clock's own actor may claim");
    call(&e, "task.start", json!({ "ref": b, "keep": true })).unwrap();
    // Review 5 #1: an anonymous clock is the shell's, so only `user` claims it.
    let err = claim(&e, b, "agent-2").unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    call(&e, "task.claim", json!({ "ref": b })).expect("the shell claims its own");
}

/// Review 3 #3: closing a task under another holder's live claim needs force,
/// and the event says whose claim it went past.
#[test]
fn closing_another_holders_claimed_task_needs_force() {
    let e = engine();
    let (a, b, c) = (add(&e, "done"), add(&e, "cancel"), add(&e, "modify"));
    for t in [a, b, c] {
        claim(&e, t, "agent-1").unwrap();
    }
    let closes = [
        ("task.done", json!({ "ref": a })),
        ("task.cancel", json!({ "ref": b })),
        (
            "task.modify",
            json!({ "ref": c, "set": { "status": "cancelled" } }),
        ),
    ];
    for (method, params) in &closes {
        let err = call(&e, method, params.clone()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict, "{method}: {}", err.message);
        assert!(err.message.contains("agent-1"), "{method}: {}", err.message);
    }
    for (method, params) in closes {
        let mut forced = params;
        forced["force_claim"] = json!(true);
        call(&e, method, forced).unwrap_or_else(|e| panic!("{method}: {}", e.message));
    }
    for (t, op) in [(a, "done"), (b, "cancel"), (c, "modify")] {
        let ev = events(&e, t)
            .into_iter()
            .find(|ev| ev["op"] == op)
            .expect(op);
        assert_eq!(ev["payload"]["claim_forced"], "agent-1", "{ev}");
    }
}

/// Review 3 #6: a month has no fixed length, so it is no lease.
#[test]
fn a_calendar_lease_is_refused() {
    let e = engine();
    let t = add(&e, "monthly");
    let err = call(&e, "task.claim", json!({ "ref": t, "lease": "P1M" })).unwrap_err();
    assert_eq!(err.message, "a lease must be a fixed duration");
}

/// Review 2 #5: the ISO spelling of nothing is no lease either.
#[test]
fn an_iso_zero_lease_is_refused() {
    let e = engine();
    let t = add(&e, "iso");
    let err = call(&e, "task.claim", json!({ "ref": t, "lease": "PT0S" })).unwrap_err();
    assert_eq!(err.message, "a lease must be greater than zero");
}

/// Review 2 #6: a row imported closed carries no claim, and `@claimed` never
/// lists a closed task.
#[test]
fn a_done_row_imported_with_a_claim_holds_none() {
    let e = engine();
    call(
        &e,
        "store.import",
        json!({ "tasks": [{
            "id": "019f0000-0000-7000-8000-000000000001", "short_id": 1,
            "title": "closed", "status": "done",
            "completed": "2026-01-01T00:00:00Z",
            "claimed_by": "agent-1", "claim_expires": "2099-01-01T00:00:00Z",
        }]}),
    )
    .expect("import");
    let got = call(&e, "task.get", json!({ "ref": 1 })).unwrap();
    assert_eq!(got["claimed_by"], Value::Null, "{got}");
    let claimed = call(&e, "task.list", json!({ "filter": "@claimed" })).unwrap();
    assert_eq!(claimed["total"], 0, "{claimed}");
}

/// Review 2 #8: an expired claim is anyone's to clear, without `force`.
#[test]
fn anyone_releases_an_expired_claim_and_it_is_logged_expired() {
    let e = engine();
    let t = add(&e, "lapsed");
    claim(&e, t, "agent-1").unwrap();
    e.conn()
        .execute(
            "UPDATE tasks SET claim_expires = '2020-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    let out = call(&e, "task.release", json!({ "ref": t, "actor": "agent-2" })).unwrap();
    assert_eq!(out["forced"], false, "{out}");
    let ev = events(&e, t)
        .into_iter()
        .find(|ev| ev["op"] == "release")
        .expect("a release event");
    assert_eq!(ev["payload"]["expired"], true, "{ev}");
    assert!(ev["payload"].get("claim_forced").is_none(), "{ev}");
    assert_eq!(ev["payload"]["holder"], "agent-1", "{ev}");
}

/// Review 4 #2/#3: stopping another holder's running claimed task needs
/// `force_claim`, and D150's `force` is not a way past a claim.
#[test]
fn stop_needs_force_claim_and_force_is_only_for_dependencies() {
    let e = engine();
    let (a, b) = (add(&e, "running"), add(&e, "done"));
    claim(&e, a, "agent-1").unwrap();
    claim(&e, b, "agent-1").unwrap();
    call(&e, "task.start", json!({ "ref": a, "actor": "agent-1" })).unwrap();

    let err = call(&e, "task.stop", json!({ "ref": a })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    assert!(err.message.contains("--force-claim"), "{}", err.message);
    call(&e, "task.stop", json!({ "ref": a, "actor": "agent-1" })).expect("the holder stops");

    let err = call(&e, "task.done", json!({ "ref": b, "force": true })).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::Conflict,
        "force is D150's: {}",
        err.message
    );
    call(&e, "task.done", json!({ "ref": b, "force_claim": true })).expect("force_claim");
    let ev = events(&e, b)
        .into_iter()
        .find(|ev| ev["op"] == "done")
        .expect("done");
    assert_eq!(ev["payload"]["claim_forced"], "agent-1", "{ev}");
    assert!(
        ev["payload"].get("forced").is_none(),
        "no dependency was forced: {ev}"
    );
}

/// Review 4 #5: re-starting a running claimed task in quick succession writes
/// nothing; only a lease past half-spent is renewed by it.
#[test]
fn quick_restarts_of_a_running_claimed_task_leave_the_rev_alone() {
    let e = engine();
    let t = add(&e, "steady");
    claim(&e, t, "agent-1").unwrap();
    call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    let rev = |e: &Engine| call(e, "task.get", json!({ "ref": t })).unwrap()["_rev"].clone();
    let before = rev(&e);
    call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    assert_eq!(rev(&e), before);
}

/// Review 5 #2: undoing a completion puts back the claim it ended.
#[test]
fn undoing_a_done_keeps_the_claim() {
    let e = engine();
    let t = add(&e, "undone");
    let claimed = claim(&e, t, "agent-1").unwrap();
    call(&e, "task.done", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    assert_eq!(
        call(&e, "task.get", json!({ "ref": t })).unwrap()["claimed_by"],
        Value::Null
    );
    call(&e, "event.revert", json!({})).expect("undo");
    let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(got["claimed_by"], "agent-1", "{got}");
    assert_eq!(got["claim_expires"], claimed["claim_expires"], "{got}");

    // And a cancellation, the other close the trigger clears a claim on.
    call(&e, "task.cancel", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    call(&e, "event.revert", json!({})).expect("undo the cancel");
    let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(got["claimed_by"], "agent-1", "{got}");
}

/// Review 5 #3: the first refusal names the real cause — a pending task
/// cannot be stopped, whoever holds it.
#[test]
fn stopping_a_pending_claimed_task_names_the_status() {
    let e = engine();
    let t = add(&e, "not running");
    claim(&e, t, "agent-1").unwrap();
    let err = call(&e, "task.stop", json!({ "ref": t })).unwrap_err();
    assert!(
        err.message.contains("cannot stop a pending task"),
        "{}",
        err.message
    );
}

/// Review 5 #5: two MCP actors. A's running task is under A's own claim, so
/// B's start is not D140's conflict: it proceeds and keeps A's clock running.
#[test]
fn a_second_actor_starts_beside_a_claimed_running_task() {
    use tasqx_core::{McpServer, Scope};
    let e = engine();
    let (t1, t2) = (add(&e, "a's"), add(&e, "b's"));
    let tool = |server: &McpServer, name: &str, arguments: Value| {
        server
            .handle_message(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .expect("a response")
    };
    let (a, b) = (
        McpServer::new(&e, Scope::Write),
        McpServer::new(&e, Scope::Write),
    );
    for (name, args) in [
        ("tasqx_claim_task", json!({ "ref": t1 })),
        (
            "tasqx_start_timer",
            json!({ "ref": t1, "include_memory": false }),
        ),
    ] {
        let out = tool(&a, name, args);
        assert_ne!(out["result"]["isError"], true, "{out}");
    }
    let out = tool(
        &b,
        "tasqx_start_timer",
        json!({ "ref": t2, "include_memory": false }),
    );
    assert_ne!(out["result"]["isError"], true, "{out}");
    for t in [t1, t2] {
        let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
        assert_eq!(got["status"], "active", "#{t}: {got}");
    }
}

/// Final review #1: a reconnected agent is a new identity. Its old clock,
/// unclaimed, is D140's — the new connection's start is refused; under the
/// old identity's live claim it is left running (the documented limit: two
/// clocks until the agent passes a stable actor).
#[test]
fn a_reconnect_meets_its_old_clock_as_d140_or_as_a_claim() {
    let e = engine();
    let (t1, t2) = (add(&e, "before the restart"), add(&e, "after it"));
    call(&e, "task.start", json!({ "ref": t1, "actor": "mcp:old" })).unwrap();
    let err = call(&e, "task.start", json!({ "ref": t2, "actor": "mcp:new" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    assert!(err.message.contains("mcp:old"), "{}", err.message);

    call(&e, "task.claim", json!({ "ref": t1, "actor": "mcp:old" })).unwrap();
    call(&e, "task.start", json!({ "ref": t2, "actor": "mcp:new" })).expect("left running");
    for t in [t1, t2] {
        let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
        assert_eq!(got["status"], "active", "#{t}: {got}");
    }
}

/// Final review #6: half a claim pair is a malformed import, named.
#[test]
fn an_import_with_half_a_claim_is_refused() {
    let e = engine();
    let err = call(
        &e,
        "store.import",
        json!({ "tasks": [{
            "id": "019f0000-0000-7000-8000-000000000001", "short_id": 1,
            "title": "half", "status": "pending", "claimed_by": "agent-1",
        }]}),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("019f0000-0000-7000-8000-000000000001"),
        "{}",
        err.message
    );
    assert!(err.message.contains("pair"), "{}", err.message);
}

/// Review 8 #2: undoing a stop puts a clock back beside another holder's.
#[test]
fn undo_of_a_stop_is_refused_only_by_the_same_holders_clock() {
    let e = engine();
    let (t1, t2) = (add(&e, "agent's"), add(&e, "mine"));
    claim(&e, t1, "agent-1").unwrap();
    call(&e, "task.start", json!({ "ref": t1, "actor": "agent-1" })).unwrap();
    call(&e, "task.start", json!({ "ref": t2 })).unwrap();
    call(&e, "task.stop", json!({ "ref": t2 })).unwrap();
    call(&e, "event.revert", json!({})).expect("agent-1's clock is not the shell's");
    for t in [t1, t2] {
        let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
        assert_eq!(got["status"], "active", "#{t}: {got}");
    }
}
/// Review 6 #3: a modify to cancelled records the claim it ends, and undo
/// puts it back.
#[test]
fn undoing_a_modify_to_cancelled_keeps_the_claim() {
    let e = engine();
    let t = add(&e, "cancelled by modify");
    claim(&e, t, "agent-1").unwrap();
    call(
        &e,
        "task.modify",
        json!({ "ref": t, "set": { "status": "cancelled" }, "actor": "agent-1" }),
    )
    .unwrap();
    call(&e, "event.revert", json!({})).expect("undo");
    let got = call(&e, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(got["claimed_by"], "agent-1", "{got}");
}

/// Review 8 #1: over MCP every list is stamped with the connection's actor,
/// so the rule is what decides: `@working` hides another agent's claim,
/// `@claimed` and `status:active` show it.
#[test]
fn an_mcp_list_hides_claims_only_under_working() {
    use tasqx_core::{McpServer, Scope};
    let e = engine();
    let t = add(&e, "zebraclaimed");
    claim(&e, t, "agent-9").unwrap();
    call(&e, "task.start", json!({ "ref": t, "actor": "agent-9" })).unwrap();
    let server = McpServer::new(&e, Scope::Write);
    let list = |filter: &str| {
        server
            .handle_message(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "tasqx_list_tasks", "arguments": { "filter": filter } }
            }))
            .expect("a response")
            .to_string()
    };
    assert!(!list("@working").contains("zebraclaimed"));
    assert!(list("@claimed").contains("zebraclaimed"));
    assert!(list("status:active").contains("zebraclaimed"));
}

/// Review 8 #4: claim and release take the optimistic-concurrency guard.
#[test]
fn claim_and_release_honour_expected_rev() {
    let e = engine();
    let t = add(&e, "guarded");
    let rev = call(&e, "task.get", json!({ "ref": t })).unwrap()["_rev"]
        .as_i64()
        .unwrap();
    let err = call(
        &e,
        "task.claim",
        json!({ "ref": t, "expected_rev": rev - 1 }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    let out = call(&e, "task.claim", json!({ "ref": t, "expected_rev": rev })).unwrap();
    let rev = out["_rev"].as_i64().unwrap();
    let err = call(
        &e,
        "task.release",
        json!({ "ref": t, "expected_rev": rev - 1 }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    call(&e, "task.release", json!({ "ref": t, "expected_rev": rev })).expect("current rev");
}

/// Review 8 #6: the restoring `renew` is the undoer's act; the claim's holder
/// rides in the payload.
#[test]
fn a_restored_claim_is_attributed_to_whoever_undid_it() {
    let e = engine();
    let t = add(&e, "undone");
    claim(&e, t, "agent-1").unwrap();
    call(&e, "task.done", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    call(&e, "event.revert", json!({})).unwrap();
    let ev = events(&e, t)
        .into_iter()
        .find(|ev| ev["op"] == "renew" && ev["payload"]["restored"] == true)
        .expect("the restoring renew");
    assert_eq!(ev["actor"], "user", "{ev}");
    assert_eq!(ev["payload"]["holder"], "agent-1", "{ev}");
}
/// Review 7 #4: a start of a task already running never writes, the holder's
/// included; renewing there is an explicit claim.
#[test]
fn an_idempotent_start_writes_nothing() {
    let e = engine();
    let t = add(&e, "running");
    claim(&e, t, "agent-1").unwrap();
    call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    call(
        &e,
        "task.claim",
        json!({ "ref": t, "actor": "agent-1", "lease": "1m" }),
    )
    .unwrap();
    let before = call(&e, "task.get", json!({ "ref": t })).unwrap();
    let again = call(&e, "task.start", json!({ "ref": t, "actor": "agent-1" })).unwrap();
    assert_eq!(again["already_running"], true, "{again}");
    let after = call(&e, "task.get", json!({ "ref": t })).unwrap();
    assert_eq!(after["_rev"], before["_rev"]);
    assert_eq!(after["claim_expires"], before["claim_expires"]);
}

/// Review 8's rule: a list hides another holder's live claim only when it
/// names an actor AND asks for `@working`; reports, exports and every other
/// list show it with its holder and expiry.
#[test]
fn views_show_every_claim_and_choosers_skip_others() {
    let e = engine();
    let t = add(&e, "agent work");
    claim(&e, t, "agent-1").unwrap();
    let summary = call(&e, "report.summary", json!({ "filter": "@working" })).unwrap();
    assert!(summary.to_string().contains("\"count\":1"), "{summary}");
    let doc = call(&e, "store.export", json!({ "filter": "@working" })).unwrap();
    assert_eq!(doc["tasks"].as_array().unwrap().len(), 1, "{doc}");
    for filter in ["@working", "status:pending", "@claimed", ""] {
        let view = call(&e, "task.list", json!({ "filter": filter })).unwrap();
        assert_eq!(ids(&view), vec![t], "{filter}: {view}");
        assert_eq!(view["tasks"][0]["claimed_by"], "agent-1", "{view}");
        assert!(view["tasks"][0]["claim_expires"].is_string(), "{view}");
    }
    for (filter, shown) in [("@working", 0), ("status:pending", 1), ("@claimed", 1)] {
        let chosen = call(
            &e,
            "task.list",
            json!({ "filter": filter, "actor": "agent-2" }),
        )
        .unwrap();
        assert_eq!(chosen["total"], shown, "{filter}: {chosen}");
    }
    let next = call(&e, "task.next", json!({})).unwrap();
    assert_eq!(
        next["task"],
        Value::Null,
        "the shell is not agent-1: {next}"
    );
}
