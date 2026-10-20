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
    call(&e, "task.done", json!({ "ref": a })).unwrap();
    call(&e, "task.cancel", json!({ "ref": b })).unwrap();
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
    // Not @working: a plain list still shows it.
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
