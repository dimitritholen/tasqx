//! D238: the `triage` status for proposed work — created by `task.add
//! {status: "triage"}`, kept out of `@working`, `next` and the urgency
//! ranking, and left by one call, `task.triage {action: accept|decline|duplicate}`.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode, McpServer, Scope};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn ok(e: &Engine, method: &str, params: Value) -> Value {
    call(e, method, params).unwrap_or_else(|err| panic!("{method}: {}", err.message))
}

fn add(e: &Engine, extra: Value) -> i64 {
    let mut p = json!({ "title": "a proposal" });
    p.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    ok(e, "task.add", p)["short_id"].as_i64().unwrap()
}

fn status(e: &Engine, id: i64) -> Value {
    ok(e, "task.get", json!({ "ref": id }))["status"].clone()
}

fn listed(e: &Engine, filter: &str) -> Vec<i64> {
    ok(e, "task.list", json!({ "filter": filter }))["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["short_id"].as_i64().unwrap())
        .collect()
}

#[test]
fn a_triage_task_is_out_of_working_next_and_backlog_and_answers_status_triage() {
    let e = engine();
    let planned = add(&e, json!({ "priority": "L" }));
    let proposed = add(&e, json!({ "status": "triage", "priority": "H" }));
    let parked = add(&e, json!({ "wait": "2999-01-01T00:00:00Z" }));

    let got = ok(&e, "task.get", json!({ "ref": proposed }));
    assert_eq!(got["status"], "triage");
    assert!(
        got.get("status_unrecognized").is_none(),
        "triage is a status this engine wrote: {got}"
    );
    assert_eq!(
        got["status"]
            .as_str()
            .and_then(tasqx_core::types::Status::parse),
        Some(tasqx_core::types::Status::Triage),
        "triage is a Status variant (D238)"
    );
    assert_eq!(listed(&e, "@working"), vec![planned]);
    assert_eq!(listed(&e, "status:triage"), vec![proposed]);
    assert_eq!(listed(&e, "status:backlog"), vec![parked]);
    assert_eq!(listed(&e, "status:triage or status:backlog").len(), 2);
    let next = ok(&e, "task.next", json!({}));
    assert_eq!(
        next["task"]["short_id"], planned,
        "a higher-priority proposal outranked planned work"
    );

    // A future date does not move a proposal: triage is not read against the clock.
    let dated = add(
        &e,
        json!({ "status": "triage", "wait": "2999-01-01T00:00:00Z" }),
    );
    assert_eq!(status(&e, dated), "triage");

    let err = call(&e, "task.add", json!({ "title": "x", "status": "done" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("triage"), "{}", err.message);
}

#[test]
fn a_proposal_cannot_be_started_or_completed_until_accepted() {
    let e = engine();
    let t = add(&e, json!({ "status": "triage" }));
    for m in ["task.start", "task.done"] {
        let err = call(&e, m, json!({ "ref": t })).unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict, "{m}");
        assert!(err.message.contains("tasqx accept"), "{m}: {}", err.message);
    }
}

#[test]
fn accept_moves_a_proposal_to_pending_and_undo_takes_it_back() {
    let e = engine();
    let t = add(&e, json!({ "status": "triage" }));
    let out = ok(&e, "task.triage", json!({ "ref": t, "action": "accept" }));
    assert_eq!(out["status"], "pending");
    assert_eq!(status(&e, t), "pending");
    assert_eq!(listed(&e, "@working"), vec![t]);

    let undone = ok(&e, "event.revert", json!({}));
    assert_eq!(undone["reverted"]["op"], "accept");
    assert_eq!(status(&e, t), "triage");

    // Accepting what is not in triage is a conflict, not a no-op.
    ok(&e, "task.triage", json!({ "ref": t, "action": "accept" }));
    let err = call(&e, "task.triage", json!({ "ref": t, "action": "accept" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);

    // Accept honours a future date: the computed backlog still applies.
    let later = add(
        &e,
        json!({ "status": "triage", "wait": "2999-01-01T00:00:00Z" }),
    );
    ok(
        &e,
        "task.triage",
        json!({ "ref": later, "action": "accept" }),
    );
    assert_eq!(status(&e, later), "backlog");
}

#[test]
fn decline_cancels_with_the_reason_as_a_note_and_undo_restores_triage() {
    let e = engine();
    let t = add(&e, json!({ "status": "triage" }));
    ok(
        &e,
        "task.triage",
        json!({ "ref": t, "action": "decline", "reason": "out of scope" }),
    );
    let got = ok(&e, "task.get", json!({ "ref": t }));
    assert_eq!(got["status"], "cancelled");
    let notes = got["annotations"].as_array().unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n["body"].as_str().unwrap().contains("out of scope")),
        "{got}"
    );
    let undone = ok(&e, "event.revert", json!({}));
    assert_eq!(undone["reverted"]["op"], "cancel");
    assert_eq!(status(&e, t), "triage");

    // A plain cancel of a proposal is undone back into triage as well.
    ok(&e, "task.cancel", json!({ "ref": t }));
    ok(&e, "event.revert", json!({}));
    assert_eq!(status(&e, t), "triage");
}

#[test]
fn duplicate_cancels_links_the_original_and_moves_the_notes() {
    let e = engine();
    let original = add(&e, json!({}));
    let dup = add(&e, json!({ "status": "triage" }));
    ok(
        &e,
        "annotation.add",
        json!({ "ref": dup, "body": "seen in the logs" }),
    );

    let err = call(
        &e,
        "task.triage",
        json!({ "ref": dup, "action": "duplicate" }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "duplicate needs `of`");
    let err = call(
        &e,
        "task.triage",
        json!({ "ref": dup, "action": "duplicate", "of": dup }),
    )
    .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::Conflict,
        "a task is no duplicate of itself"
    );

    let out = ok(
        &e,
        "task.triage",
        json!({ "ref": dup, "action": "duplicate", "of": original }),
    );
    assert_eq!(out["status"], "cancelled");
    assert_eq!(out["moved_annotations"], 1);
    assert_eq!(status(&e, dup), "cancelled");
    let orig = ok(&e, "task.get", json!({ "ref": original }));
    assert!(orig["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["body"] == "seen in the logs"));
    let links = ok(
        &e,
        "link.list",
        json!({ "ref": dup, "relation": "duplicates" }),
    );
    assert_eq!(links["links"].as_array().unwrap().len(), 1, "{links}");
}

#[test]
fn discovered_from_writes_a_derived_from_link() {
    let e = engine();
    let parent = add(&e, json!({}));
    let found = add(&e, json!({ "status": "triage", "discovered_from": parent }));
    let links = ok(
        &e,
        "link.list",
        json!({ "ref": found, "relation": "derived_from" }),
    );
    let links = links["links"].as_array().unwrap();
    assert_eq!(links.len(), 1);
    let err = call(
        &e,
        "task.add",
        json!({ "title": "x", "discovered_from": 999 }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn a_proposal_is_still_an_open_blocker() {
    let e = engine();
    let blocker = add(&e, json!({ "status": "triage" }));
    let dependent = add(&e, json!({ "depends_on": [blocker] }));
    assert_eq!(listed(&e, "@blocked"), vec![dependent]);
}

#[test]
fn export_and_import_round_trip_the_triage_status() {
    let a = engine();
    let t = add(&a, json!({ "status": "triage" }));
    let doc = ok(&a, "store.export", json!({}));
    assert_eq!(doc["tasks"][0]["status"], "triage");
    let b = engine();
    ok(&b, "store.import", doc);
    assert_eq!(status(&b, t), "triage");
}

#[test]
fn the_mcp_server_files_its_adds_into_triage_when_configured() {
    let e = engine();
    let server = McpServer::new(&e, Scope::Write).with_add_status("triage");
    server.handle_message(&json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": { "name": "agent", "version": "1" } }
    }));
    let add = |id: i64, args: Value| {
        server.handle_message(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "tasqx_add_task", "arguments": args }
        }))
    };
    add(1, json!({ "title": "found it" }));
    add(2, json!({ "title": "planned", "status": "pending" }));
    assert_eq!(status(&e, 1), "triage");
    assert_eq!(status(&e, 2), "pending");
    // The add event names the client that proposed it.
    let events = ok(&e, "event.list", json!({ "ref": 1 }));
    let add_ev = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ev| ev["op"] == "add")
        .unwrap()
        .clone();
    assert_eq!(add_ev["payload"]["client"], "agent 1", "{add_ev}");

    let triaged = server.handle_message(&json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": { "name": "tasqx_triage_task",
                    "arguments": { "ref": 1, "action": "accept" } }
    }));
    assert!(
        !triaged.unwrap()["result"]["isError"]
            .as_bool()
            .unwrap_or(false),
        "tasqx_triage_task accept failed"
    );
    assert_eq!(status(&e, 1), "pending");
}

#[test]
fn reopening_a_declined_or_duplicate_proposal_puts_it_back_in_triage() {
    let e = engine();
    let original = add(&e, json!({}));
    let declined = add(&e, json!({ "status": "triage" }));
    let dup = add(&e, json!({ "status": "triage" }));
    let cancelled = add(&e, json!({ "status": "triage" }));
    let modified = add(&e, json!({ "status": "triage" }));
    ok(
        &e,
        "task.triage",
        json!({ "ref": declined, "action": "decline" }),
    );
    ok(
        &e,
        "task.triage",
        json!({ "ref": dup, "action": "duplicate", "of": original }),
    );
    ok(&e, "task.cancel", json!({ "ref": cancelled }));
    ok(
        &e,
        "task.modify",
        json!({ "ref": modified, "set": { "status": "cancelled" } }),
    );
    for t in [declined, dup, cancelled, modified] {
        let out = ok(&e, "task.reopen", json!({ "ref": t }));
        assert_eq!(out["status"], "triage", "#{t}: {out}");
        assert_eq!(status(&e, t), "triage", "#{t}");
        // The event names where it went, for a replay (the desktop's CFD).
        let events = ok(&e, "event.list", json!({ "ref": t }));
        assert_eq!(events["events"][0]["op"], "reopen");
        assert_eq!(events["events"][0]["payload"]["to"], "triage");
    }
    // A task cancelled from pending still reopens to pending.
    ok(&e, "task.cancel", json!({ "ref": original }));
    assert_eq!(
        ok(&e, "task.reopen", json!({ "ref": original }))["status"],
        "pending"
    );
}

#[test]
fn refusals_name_the_triage_status() {
    let e = engine();
    let t = add(&e, json!({ "status": "triage" }));
    let err = call(&e, "task.stop", json!({ "ref": t })).unwrap_err();
    assert!(err.message.contains("triage"), "{}", err.message);
    let err = call(&e, "task.reopen", json!({ "ref": t })).unwrap_err();
    assert!(err.message.contains("a triage task"), "{}", err.message);
}

#[test]
fn a_proposal_is_never_overdue_nor_in_next_resume() {
    let e = engine();
    let t = add(
        &e,
        json!({ "status": "triage", "due": "2000-01-01T00:00:00Z" }),
    );
    ok(
        &e,
        "annotation.add",
        json!({ "ref": t, "body": "a fresh note" }),
    );
    let summary = ok(&e, "report.summary", json!({ "group_by": "status" }));
    let overdue: i64 = summary["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["overdue"].as_i64().unwrap_or(0))
        .sum();
    assert_eq!(overdue, 0, "{summary}");
    let next = ok(&e, "task.next", json!({ "resume": true }));
    assert!(next["recent"].as_array().unwrap().is_empty(), "{next}");
    assert!(next["task"].is_null());
}

#[test]
fn graph_query_filters_on_triage() {
    let e = engine();
    let parent = add(&e, json!({}));
    add(&e, json!({ "status": "triage", "discovered_from": parent }));
    let g = ok(
        &e,
        "graph.query",
        json!({ "root": parent, "status": "triage" }),
    );
    assert!(
        g["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["status"] == "triage"),
        "{g}"
    );
}

#[test]
fn the_inbox_is_one_read_oldest_first_with_client_origin_and_a_page() {
    let e = engine();
    let parent = add(&e, json!({}));
    let first = add(
        &e,
        json!({ "status": "triage", "discovered_from": parent, "client": "agent 1" }),
    );
    let second = add(&e, json!({ "status": "triage" }));
    add(&e, json!({}));
    let inbox = ok(&e, "task.triage_list", json!({ "limit": 1 }));
    assert_eq!(inbox["total"], 2, "{inbox}");
    assert_eq!(inbox["next_offset"], 1, "{inbox}");
    let row = &inbox["tasks"][0];
    assert_eq!(row["short_id"], first);
    assert_eq!(row["client"], "agent 1");
    assert_eq!(row["discovered_from"]["short_id"], parent);
    let rest = ok(&e, "task.triage_list", json!({ "offset": 1 }));
    assert_eq!(rest["tasks"][0]["short_id"], second);
    assert!(rest["tasks"][0]["client"].is_null());
    assert!(rest["tasks"][0]["discovered_from"].is_null());
    assert!(rest["next_offset"].is_null());
}

#[test]
fn decline_and_duplicate_respect_a_claim_and_undo_restores_it() {
    let e = engine();
    let original = add(&e, json!({}));
    let t = add(&e, json!({ "status": "triage" }));
    ok(
        &e,
        "task.claim",
        json!({ "ref": t, "actor": "agent-9", "lease": "24h" }),
    );
    for p in [
        json!({ "ref": t, "action": "decline", "actor": "someone" }),
        json!({ "ref": t, "action": "duplicate", "of": original, "actor": "someone" }),
    ] {
        let err = call(&e, "task.triage", p).unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert!(err.message.contains("agent-9"), "{}", err.message);
    }
    ok(
        &e,
        "task.triage",
        json!({ "ref": t, "action": "decline", "actor": "agent-9" }),
    );
    ok(&e, "event.revert", json!({}));
    let got = ok(&e, "task.get", json!({ "ref": t }));
    assert_eq!(got["status"], "triage");
    assert_eq!(
        got["claimed_by"], "agent-9",
        "undo restores the claim: {got}"
    );
    let out = ok(
        &e,
        "task.triage",
        json!({ "ref": t, "action": "decline", "actor": "x", "force_claim": true }),
    );
    assert_eq!(out["status"], "cancelled");
}

#[test]
fn a_repeated_duplicate_is_refused_by_undo_whole() {
    let e = engine();
    let original = add(&e, json!({}));
    let t = add(&e, json!({ "status": "triage" }));
    ok(
        &e,
        "task.triage",
        json!({ "ref": t, "action": "duplicate", "of": original }),
    );
    ok(&e, "task.reopen", json!({ "ref": t }));
    // The `duplicates` link is already there, so no `link.add` is written.
    ok(
        &e,
        "task.triage",
        json!({ "ref": t, "action": "duplicate", "of": original }),
    );
    let err = call(&e, "event.revert", json!({})).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("`duplicate`"), "{}", err.message);
    assert_eq!(status(&e, t), "cancelled", "nothing was half undone");
}
