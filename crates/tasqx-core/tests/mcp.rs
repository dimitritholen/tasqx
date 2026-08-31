//! Tests for the bundled MCP server (DESIGN.md §7, §12-D7).
//!
//! These drive the pure `McpServer::handle_message` function directly against a
//! real in-memory Engine — no stdio piping — exercising the initialize
//! handshake, tools/list, tools/call, scope enforcement, and ApiError
//! passthrough as `isError` results.

use serde_json::{json, Value};
use tasqx_core::{Engine, McpServer, Scope};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

/// Extract and parse the first text-content block of a tools/call result.
fn tool_text(result: &Value) -> Value {
    let text = result["result"]["content"][0]["text"]
        .as_str()
        .expect("tools/call result carries text content");
    serde_json::from_str(text).unwrap_or(Value::String(text.to_string()))
}

/// Parse the machine-readable JSON block of a tools/call result.
///
/// `tasqx_get_task` leads with the rendered markdown view and carries its JSON
/// behind it, so for that one tool the JSON is the LAST block, not the first.
/// Every other tool returns a single block, where first and last coincide —
/// which is why this is a separate helper and `tool_text` still pins "block
/// zero is the JSON" for all of them.
fn tool_json(result: &Value) -> Value {
    let text = result["result"]["content"]
        .as_array()
        .and_then(|c| c.last())
        .and_then(|b| b["text"].as_str())
        .expect("tools/call result carries text content");
    serde_json::from_str(text).unwrap_or(Value::String(text.to_string()))
}

fn is_error(result: &Value) -> bool {
    result["result"]["isError"].as_bool().unwrap_or(false)
}

fn call(server: &McpServer, id: i64, name: &str, arguments: Value) -> Value {
    server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
        .expect("tools/call is a request and yields a response")
}

// ---- full protocol sequence --------------------------------------------------

#[test]
fn full_protocol_sequence() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    // 1. initialize
    let init = server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test-harness", "version": "0.0.0" }
            }
        }))
        .expect("initialize is a request");
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "tasqx");
    assert!(init["result"]["serverInfo"]["version"].is_string());
    assert!(init["result"]["protocolVersion"].is_string());
    // capabilities.tools must be present (an object).
    assert!(init["result"]["capabilities"]["tools"].is_object());

    // 2. notifications/initialized — a notification yields NO response.
    let note = server.handle_message(&json!({
        "jsonrpc": "2.0", "method": "notifications/initialized"
    }));
    assert!(note.is_none(), "notifications must not produce a response");

    // 3. tools/list — all 20 tools present, each with an inputSchema.
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 20, "expected 20 tools");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "tasqx_list_tasks",
        "tasqx_get_task",
        "tasqx_summary",
        "tasqx_list_projects",
        "tasqx_search_memory",
        "tasqx_add_task",
        "tasqx_modify_task",
        "tasqx_complete_task",
        "tasqx_reopen_task",
        "tasqx_start_timer",
        "tasqx_stop_timer",
        "tasqx_tag_task",
        "tasqx_untag_task",
        "tasqx_annotate_task",
        "tasqx_add_dependency",
        "tasqx_remove_dependency",
        "tasqx_add_memory",
        "tasqx_remove_memory",
        "tasqx_create_project",
        "tasqx_get_memory",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    for t in tools {
        assert_eq!(
            t["inputSchema"]["type"], "object",
            "tool {} must have an object inputSchema",
            t["name"]
        );
    }
    // Reads are read-only; `destructiveHint` is a per-tool fact and NOT the
    // write flag restated (D68). Creating a task is additive.
    let get_add = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
    assert_eq!(
        get_add("tasqx_add_task")["annotations"]["destructiveHint"],
        false,
        "creating a task is additive: labelling it destructive is what made the hint          indistinguishable from `readOnlyHint` and cost the host its gate"
    );
    assert_eq!(
        get_add("tasqx_remove_memory")["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        get_add("tasqx_list_tasks")["annotations"]["readOnlyHint"],
        true
    );

    // 4. tools/call tasqx_add_task
    let added = call(
        &server,
        3,
        "tasqx_add_task",
        json!({ "title": "Ship the v1 JSON API freeze", "priority": "H" }),
    );
    assert!(!is_error(&added));
    let added_body = tool_text(&added);
    let short_id = added_body["short_id"]
        .as_i64()
        .expect("short_id in add result");

    // 5. tools/call tasqx_list_tasks — the added task appears.
    let listed_tasks = call(
        &server,
        4,
        "tasqx_list_tasks",
        json!({ "filter": "status:pending" }),
    );
    assert!(!is_error(&listed_tasks));
    let body = tool_text(&listed_tasks);
    let titles: Vec<&str> = body["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap_or(""))
        .collect();
    assert!(
        titles.contains(&"Ship the v1 JSON API freeze"),
        "added task should appear in the list"
    );

    // 6. tools/call tasqx_complete_task
    let done = call(
        &server,
        5,
        "tasqx_complete_task",
        json!({ "ref": short_id }),
    );
    assert!(!is_error(&done));
    let done_body = tool_text(&done);
    assert_eq!(done_body["status"], "done");
}

// ---- unknown method ----------------------------------------------------------

#[test]
fn unknown_method_is_jsonrpc_error() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let resp = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/list" }))
        .expect("a request yields a response");
    assert_eq!(resp["id"], 9);
    assert_eq!(resp["error"]["code"], -32601);
    assert!(resp.get("result").is_none());
}

// ---- scope enforcement -------------------------------------------------------

#[test]
fn read_scope_rejects_writes_and_does_not_mutate() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Read);

    // A write tool under read scope => isError, no mutation.
    let attempt = call(
        &server,
        1,
        "tasqx_add_task",
        json!({ "title": "should not exist" }),
    );
    assert!(is_error(&attempt), "write under read scope must be isError");
    let msg = attempt["result"]["content"][0]["text"].as_str().unwrap();
    assert!(msg.contains("read-only") || msg.contains("write scope"));

    // A read tool is still allowed, and shows nothing was created.
    let listed = call(
        &server,
        2,
        "tasqx_list_tasks",
        json!({ "filter": "status:pending" }),
    );
    assert!(!is_error(&listed));
    let body = tool_text(&listed);
    assert_eq!(
        body["count"].as_i64().unwrap(),
        0,
        "no task should have been created"
    );
}

// ---- ApiError passthrough ----------------------------------------------------

#[test]
fn bad_ref_get_task_is_not_found_iserror() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let resp = call(&server, 1, "tasqx_get_task", json!({ "ref": 999999 }));
    assert!(is_error(&resp), "bad ref must yield isError");
    let msg = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        msg.contains("not_found"),
        "message should carry the not_found code: {msg}"
    );
}

// ---- tools/list is scope-filtered --------------------------------------------

#[test]
fn read_scope_tools_list_hides_write_tools() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Read);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    // A read-only session advertises only the six read tools — including both
    // memory readers: a read-only agent may consult knowledge (D41), and D71
    // made "consult" mean the document rather than an excerpt of it.
    assert_eq!(tools.len(), 6, "read scope should list only the read tools");
    for t in tools {
        assert_eq!(
            t["annotations"]["readOnlyHint"], true,
            "read scope must not advertise a write tool: {}",
            t["name"]
        );
    }
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(
        !names.contains(&"tasqx_add_task"),
        "write tool leaked into read-scope list"
    );
}

// ---- optimistic concurrency by default ---------------------------------------

#[test]
fn modify_pins_expected_rev_by_default() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    // Create a task (starts at _rev 1).
    let added = call(
        &server,
        1,
        "tasqx_add_task",
        json!({ "title": "concurrency guard" }),
    );
    let short_id = tool_text(&added)["short_id"].as_i64().expect("short_id");

    // A modify with NO expected_rev supplied still succeeds — the server reads
    // the current _rev and pins it — advancing the task to _rev 2.
    let m1 = call(
        &server,
        2,
        "tasqx_modify_task",
        json!({ "ref": short_id, "set": { "priority": "M" } }),
    );
    assert!(!is_error(&m1), "default modify should succeed");
    assert_eq!(tool_text(&m1)["_rev"].as_i64(), Some(2));

    // A caller pinning a now-stale rev (simulating a human edit under it) gets a
    // conflict instead of a silent clobber — the §7 guarantee, model-visible.
    let stale = call(
        &server,
        3,
        "tasqx_modify_task",
        json!({ "ref": short_id, "set": { "priority": "L" }, "expected_rev": 1 }),
    );
    assert!(is_error(&stale), "stale expected_rev must conflict");
    let msg = stale["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        msg.contains("conflict"),
        "message should carry the conflict code: {msg}"
    );

    // D75: the surface an agent reads must say what this test just drove, and
    // it is guarded HERE, beside the behaviour, so wording and injection
    // cannot drift apart (D30's rule, applied to a description instead of a
    // table). The old description read "Pass expected_rev for optimistic
    // concurrency" — literally: omit it and the guard is off — the exact
    // inverse of the pinning asserted above, measured costing an agent 199
    // conflicts in 200 contended rounds it never opted into (field test
    // 2026-08-31, finding #7).
    let tools = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/list" }))
        .expect("tools/list answers");
    let modify = tools["result"]["tools"]
        .as_array()
        .expect("tools is an array")
        .iter()
        .find(|t| t["name"] == "tasqx_modify_task")
        .expect("tasqx_modify_task is listed");
    let description = modify["description"].as_str().expect("description");
    for load_bearing in [
        "omitted",
        "conflict",
        "re-read",
        "retry",
        "no way to opt out",
    ] {
        assert!(
            description.contains(load_bearing),
            "the tool description must state the injection and the retry it \
             expects — missing `{load_bearing}`: {description}"
        );
    }
    let guard = modify["inputSchema"]["properties"]["expected_rev"]["description"]
        .as_str()
        .expect("expected_rev has a description");
    assert!(
        guard.contains("Supplied by the server") && guard.contains("no last-writer-wins"),
        "`expected_rev` must not read as opt-in — the truth is the reverse: {guard}"
    );
}

// ---- protocol version negotiation --------------------------------------------

#[test]
fn initialize_negotiates_supported_protocol_version() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Read);

    // A supported requested version is echoed back.
    let older = server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-03-26", "capabilities": {} }
        }))
        .expect("initialize is a request");
    assert_eq!(older["result"]["protocolVersion"], "2025-03-26");

    // An unknown requested version falls back to the server's own default.
    let unknown = server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "initialize",
            "params": { "protocolVersion": "1999-01-01", "capabilities": {} }
        }))
        .expect("initialize is a request");
    assert_eq!(unknown["result"]["protocolVersion"], "2025-06-18");
}

// ---- annotation.add over MCP -------------------------------------------------

#[test]
fn annotate_tool_round_trips_multiline_markdown() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    let added = call(&server, 1, "tasqx_add_task", json!({ "title": "carrier" }));
    let short_id = tool_text(&added)["short_id"].as_i64().expect("short_id");

    // The body is exactly the feature-context shape the tool exists for:
    // multi-line markdown with headers, checkboxes, and a fenced code block.
    let body = "## Context\n\n- [ ] server-side recompute\n\n```rust\nfn f() {}\n```";
    let annotated = call(
        &server,
        2,
        "tasqx_annotate_task",
        json!({ "ref": short_id, "body": body }),
    );
    assert!(!is_error(&annotated), "annotate failed: {annotated}");
    let ann = &tool_text(&annotated)["annotation"];
    assert_eq!(ann["body"], body, "body must survive byte-for-byte");
    assert!(
        ann["created"].is_string(),
        "annotation carries its timestamp"
    );

    // The annotation is readable back through tasqx_get_task, unmangled.
    let got = call(&server, 3, "tasqx_get_task", json!({ "ref": short_id }));
    assert_eq!(tool_json(&got)["annotations"][0]["body"], body);
}

#[test]
fn annotate_tool_without_body_is_bad_request_not_panic() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let added = call(&server, 1, "tasqx_add_task", json!({ "title": "carrier" }));
    let short_id = tool_text(&added)["short_id"].as_i64().expect("short_id");

    let resp = call(
        &server,
        2,
        "tasqx_annotate_task",
        json!({ "ref": short_id }),
    );
    assert!(is_error(&resp), "missing body must be an isError result");
    let msg = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        msg.contains("body"),
        "error should name the missing field: {msg}"
    );
}

// ---- dependency.add over MCP ---------------------------------------------------

#[test]
fn add_dependency_tool_blocks_task_and_refuses_cycles() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    let a = tool_text(&call(
        &server,
        1,
        "tasqx_add_task",
        json!({ "title": "design" }),
    ))["short_id"]
        .as_i64()
        .unwrap();
    let b = tool_text(&call(
        &server,
        2,
        "tasqx_add_task",
        json!({ "title": "implement" }),
    ))["short_id"]
        .as_i64()
        .unwrap();

    // b depends on a: b is now blocked and reports the edge.
    let dep = call(
        &server,
        3,
        "tasqx_add_dependency",
        json!({ "ref": b, "depends_on": a }),
    );
    assert!(!is_error(&dep), "dependency add failed: {dep}");
    let dep_body = tool_text(&dep);
    assert_eq!(dep_body["blocked"], true);
    assert_eq!(dep_body["depends_on"][0].as_i64(), Some(a));

    // The reverse edge would be a cycle: refused as a conflict, not applied.
    let cycle = call(
        &server,
        4,
        "tasqx_add_dependency",
        json!({ "ref": a, "depends_on": b }),
    );
    assert!(is_error(&cycle), "a cycle must be an isError result");
    let msg = cycle["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        msg.contains("conflict"),
        "cycle should be a conflict: {msg}"
    );

    // Completing a unblocks b — the dependency is live, not decorative.
    let done = call(&server, 5, "tasqx_complete_task", json!({ "ref": a }));
    assert_eq!(tool_text(&done)["unblocked"][0].as_i64(), Some(b));
}

#[test]
fn new_relationship_tools_are_write_scoped() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Read);
    for (id, name, args) in [
        (1, "tasqx_annotate_task", json!({ "ref": 1, "body": "x" })),
        (
            2,
            "tasqx_add_dependency",
            json!({ "ref": 1, "depends_on": 2 }),
        ),
    ] {
        let resp = call(&server, id, name, args);
        assert!(is_error(&resp), "{name} must be refused under read scope");
        let msg = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            msg.contains("read-only") || msg.contains("write scope"),
            "{name} refusal should say why: {msg}"
        );
    }
}

// ---- clientInfo -> lifecycle correlation (#12) ---------------------------------

/// The newest event payload of the given op, straight from the store — events
/// are the durable correlation record, so this is the surface under test.
fn event_payload(engine: &Engine, op: &str) -> Value {
    let raw: String = engine
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE op = ?1 ORDER BY id DESC LIMIT 1",
            [op],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn client_info_from_initialize_is_stamped_onto_start_and_done_events() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "claude-code", "version": "2.1.0" }
            }
        }))
        .expect("initialize is a request");

    let added = call(&server, 2, "tasqx_add_task", json!({ "title": "carrier" }));
    let sid = tool_text(&added)["short_id"].as_i64().expect("short_id");

    let started = call(&server, 3, "tasqx_start_timer", json!({ "ref": sid }));
    assert!(!is_error(&started), "start failed: {started}");
    assert_eq!(
        event_payload(&engine, "start")["client"],
        "claude-code 2.1.0",
        "the start event must name the tool captured at initialize"
    );

    let done = call(&server, 4, "tasqx_complete_task", json!({ "ref": sid }));
    assert!(!is_error(&done), "done failed: {done}");
    assert_eq!(
        event_payload(&engine, "done")["client"],
        "claude-code 2.1.0",
        "the done event must name the tool captured at initialize"
    );
}

/// The expected_rev rule carried over: a caller that supplies its own
/// `client` is respected, and a session that never sent clientInfo injects
/// nothing rather than an empty string the engine would refuse.
#[test]
fn a_caller_supplied_client_wins_and_no_client_info_injects_nothing() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    // No initialize at all: nothing to inject.
    let added = call(&server, 1, "tasqx_add_task", json!({ "title": "carrier" }));
    let sid = tool_text(&added)["short_id"].as_i64().expect("short_id");
    let started = call(&server, 2, "tasqx_start_timer", json!({ "ref": sid }));
    assert!(!is_error(&started), "start failed: {started}");
    assert!(
        event_payload(&engine, "start").get("client").is_none(),
        "no clientInfo, no client key"
    );

    let done = call(
        &server,
        3,
        "tasqx_complete_task",
        json!({ "ref": sid, "client": "my-wrapper 0.1" }),
    );
    assert!(!is_error(&done), "done failed: {done}");
    assert_eq!(
        event_payload(&engine, "done")["client"],
        "my-wrapper 0.1",
        "an explicit client must not be overwritten"
    );
}

// ---- self-reported tokens on complete (#13) ------------------------------------

#[test]
fn complete_task_with_token_args_records_a_self_report() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "cursor", "version": "1.3" }
            }
        }))
        .expect("initialize is a request");

    let added = call(&server, 2, "tasqx_add_task", json!({ "title": "carrier" }));
    let sid = tool_text(&added)["short_id"].as_i64().expect("short_id");

    // No `tool` supplied: the injected client is the attribution fallback.
    let done = call(
        &server,
        3,
        "tasqx_complete_task",
        json!({
            "ref": sid,
            "model": "gpt-5.4",
            "input_tokens": 500,
            "output_tokens": 60,
            "cache_read_tokens": 2000
        }),
    );
    assert!(!is_error(&done), "complete failed: {done}");
    assert_eq!(tool_text(&done)["status"], "done");

    let got = tool_json(&call(&server, 4, "tasqx_get_task", json!({ "ref": sid })));
    let m = &got["tokens"][0];
    assert_eq!(m["tool"], "cursor 1.3");
    assert_eq!(m["source"], "self-report");
    assert_eq!(m["confidence"], "medium");
    assert_eq!(m["model"], "gpt-5.4");
    assert_eq!(m["input_tokens"], 500);
    assert_eq!(m["output_tokens"], 60);
    assert_eq!(m["cache_read_tokens"], 2000);
    assert_eq!(m["cache_creation_tokens"], 0);

    // The done event echoes the measurement and no token.add event exists —
    // one mutation, one event.
    assert_eq!(event_payload(&engine, "done")["tokens"], *m);
    let token_add_events: i64 = engine
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE op = 'token.add'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(token_add_events, 0);
}

/// D50: a completion with no self-report answers with a `tokens_hint` nudging
/// the machine caller toward the primary channel. Response key only — never an
/// event key, and it asserts nothing about ownership or spend.
#[test]
fn complete_task_without_token_args_carries_the_self_report_hint() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    let added = call(&server, 1, "tasqx_add_task", json!({ "title": "quiet" }));
    let sid = tool_text(&added)["short_id"].as_i64().expect("short_id");

    let done = call(&server, 2, "tasqx_complete_task", json!({ "ref": sid }));
    assert!(!is_error(&done), "complete failed: {done}");
    assert_eq!(
        tool_text(&done)["tokens_hint"],
        "no token counts were self-reported; log-parse attribution is a \
         best-effort fallback — pass input_tokens/output_tokens/\
         cache_read_tokens/cache_creation_tokens on completion for a \
         reliable measurement",
        "the unmeasured completion must nudge toward self-report"
    );
    // The hint is a response key, never an event: the done event payload
    // stays exactly the shape the one-event-per-mutation invariant pins.
    assert!(
        event_payload(&engine, "done").get("tokens_hint").is_none(),
        "tokens_hint leaked into the done event payload"
    );
}

/// The counterpart: a completion that DID self-report gets no hint — it would
/// recommend what already happened.
#[test]
fn complete_task_with_token_args_carries_no_hint() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    let added = call(&server, 1, "tasqx_add_task", json!({ "title": "measured" }));
    let sid = tool_text(&added)["short_id"].as_i64().expect("short_id");

    let done = call(
        &server,
        2,
        "tasqx_complete_task",
        json!({ "ref": sid, "tool": "claude-code", "input_tokens": 12, "output_tokens": 3 }),
    );
    assert!(!is_error(&done), "complete failed: {done}");
    assert!(
        tool_text(&done).get("tokens_hint").is_none(),
        "a self-reported completion must not carry the hint"
    );
}

// ---- memory over MCP (D41) ---------------------------------------------------

#[test]
fn memory_add_is_write_scoped_but_search_works_read_only() {
    let engine = engine();

    // Seed one doc through a write session.
    let writer = McpServer::new(&engine, Scope::Write);
    let added = call(
        &writer,
        1,
        "tasqx_add_memory",
        json!({ "title": "Deploy runbook", "body": "deploys go through the blue-green pipeline" }),
    );
    assert!(!is_error(&added), "add_memory failed: {added}");
    assert!(tool_text(&added)["id"].is_string());

    // A read-only session can consult knowledge but not write it.
    let reader = McpServer::new(&engine, Scope::Read);
    let found = call(
        &reader,
        2,
        "tasqx_search_memory",
        json!({ "query": "blue-green" }),
    );
    assert!(!is_error(&found), "search under read scope failed: {found}");
    assert_eq!(tool_text(&found)["count"], 1);

    let refused = call(
        &reader,
        3,
        "tasqx_add_memory",
        json!({ "title": "nope", "body": "nope" }),
    );
    assert!(is_error(&refused), "add_memory must be refused read-only");
}

#[test]
fn search_memory_schema_advertises_the_scope_enum() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Read);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().unwrap();
    let search = tools
        .iter()
        .find(|t| t["name"] == "tasqx_search_memory")
        .expect("search_memory is a read tool");
    // The schema enum renders from MEMORY_SCOPES — the agent must read the
    // same closed set the engine validates against.
    let scopes = search["inputSchema"]["properties"]["scope"]["enum"]
        .as_array()
        .expect("scope enum");
    let names: Vec<&str> = scopes.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(names, tasqx_core::engine::MEMORY_SCOPES);
}

// ---- operator-selected scope -------------------------------------------------

#[test]
fn scope_is_a_capability_choice_not_a_credential() {
    assert_eq!(Scope::Read.as_str(), "read");
    assert_eq!(Scope::Write.as_str(), "write");
    assert!(!Scope::Read.allows_write());
    assert!(Scope::Write.allows_write());
}

// ---- the annotation history is bounded on the way out ------------------------

/// A task whose history is long enough to have been the problem.
fn task_with_annotations(engine: &Engine, n: usize) {
    engine
        .task_add(&json!({ "title": "a task worth reading" }))
        .expect("add");
    for i in 0..n {
        engine
            .annotation_add(&json!({
                "ref": 1,
                "body": format!("### Step {i}\n\nWhat was decided, why, and what it cost — a body \
                    of roughly the size the annotations in this project actually reach when a \
                    task is worked over several days.\n"),
            }))
            .expect("annotate");
    }
}

/// `tasqx_get_task` must not hand back a payload no client can accept.
///
/// The reported failure: a real feature task with five days of annotations
/// returned ~58 KB and blew through an MCP client's tool-output limit, so the
/// task with the richest history was the one the tool could not return. The
/// core answers `task.get` whole on purpose — the JSON API has read that way
/// since v1 was frozen — so the bound belongs HERE, at the transport that has
/// the limit, alongside the two defaults `tools_call` already supplies
/// (`expected_rev` and `client`).
///
/// The budget is a row count, not a byte cap: bodies are unbounded text and no
/// page size can promise bytes. What this asserts is that a realistically sized
/// history of the length that caused the report now fits.
#[test]
fn get_task_bounds_a_long_history_when_the_caller_names_no_page_size() {
    let engine = engine();
    task_with_annotations(&engine, 200);
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    assert!(!is_error(&out));
    let json = tool_json(&out);
    assert_eq!(json["annotations_total"], json!(200));
    let returned = json["annotations"].as_array().expect("annotations").len();
    assert!(
        returned < 200,
        "an unbounded default is the defect: {returned} annotations came back"
    );
    assert!(
        json["annotations_next_offset"].as_u64().is_some(),
        "an elided history must name the offset that continues it, or the rest is unreachable"
    );

    let bytes: usize = out["result"]["content"]
        .as_array()
        .expect("content blocks")
        .iter()
        .map(|b| b["text"].as_str().unwrap_or("").len())
        .sum();
    assert!(
        bytes < 32_768,
        "the whole response is {bytes} bytes; the report's 58 KB is what this bound exists to \
         prevent"
    );
}

/// A caller that names its own page size keeps it, and can still ask for
/// everything — the same "explicit wins" rule `expected_rev` and `client` follow.
#[test]
fn an_explicit_annotations_limit_overrides_the_transport_default() {
    let engine = engine();
    task_with_annotations(&engine, 40);
    let server = McpServer::new(&engine, Scope::Read);

    let three = tool_json(&call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 3 }),
    ));
    assert_eq!(three["annotations"].as_array().unwrap().len(), 3);

    let all = tool_json(&call(
        &server,
        2,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 40 }),
    ));
    assert_eq!(all["annotations"].as_array().unwrap().len(), 40);
    assert!(all["annotations_next_offset"].is_null());
}

/// A short history is returned whole and says nothing about paging — the bound
/// must not turn every task into a paginated one.
#[test]
fn a_short_history_is_returned_whole_and_advertises_no_next_page() {
    let engine = engine();
    task_with_annotations(&engine, 2);
    let server = McpServer::new(&engine, Scope::Read);

    let json = tool_json(&call(&server, 1, "tasqx_get_task", json!({ "ref": 1 })));
    assert_eq!(json["annotations"].as_array().unwrap().len(), 2);
    assert_eq!(json["annotations_total"], json!(2));
    assert!(json["annotations_next_offset"].is_null());
}

/// The shape the field report actually hit: FEW annotations, each enormous.
///
/// A row count cannot bound this and the first version of the fix did not. The
/// task that blew the client's limit carried eleven bodies, not two hundred, so
/// a page size of twenty returned every one of them and changed nothing —
/// measured on the live store at 29 KB of JSON, doubled by the D49 two-block
/// response. The bound has to be measured in the unit the limit is expressed
/// in.
#[test]
fn get_task_shrinks_its_page_until_the_response_fits_the_budget() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "eleven very long notes" }))
        .expect("add");
    // ~6 KB each: eleven of them is the reported payload, and no row count
    // short of one gets under a budget on its own.
    let body = "detail ".repeat(900);
    for i in 0..11 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("## Note {i}\n\n{body}\n") }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    assert!(!is_error(&out));
    let bytes: usize = out["result"]["content"]
        .as_array()
        .expect("content blocks")
        .iter()
        .map(|b| b["text"].as_str().unwrap_or("").len())
        .sum();
    assert!(
        bytes < 24_576,
        "the response is {bytes} bytes: a page of twenty returns all eleven of these, so a row \
         count alone never bounded the case that was reported"
    );

    // Shrinking to fit spends the duplicate JSON block first, so the machine
    // block is gone by the time the page is cut — the counts are read from the
    // view, which is where the reader would find them too.
    let view = out["result"]["content"][0]["text"]
        .as_str()
        .expect("the view");
    assert!(
        view.contains("of 11"),
        "a response shrunk to fit must still say how much history it left out:\n{view}"
    );
    assert!(
        view.contains("annotations_offset"),
        "and how to reach it:\n{view}"
    );
    let returned = view.matches("## Note ").count();
    assert!(
        (1..11).contains(&returned),
        "expected a shrunk page, got {returned} of 11"
    );
}

/// Shrinking is for the caller who named no page size. One who did gets exactly
/// what they asked for, over budget or not — an explicit request second-guessed
/// is a caller who can never fetch a big page on purpose.
#[test]
fn an_explicit_limit_is_never_shrunk_to_fit() {
    let engine = engine();
    engine.task_add(&json!({ "title": "big" })).expect("add");
    let body = "detail ".repeat(900);
    for i in 0..11 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("## Note {i}\n\n{body}\n") }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Read);

    let json = tool_json(&call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 11 }),
    ));
    assert_eq!(json["annotations"].as_array().unwrap().len(), 11);
    assert!(json["annotations_next_offset"].is_null());
}

// ---- memory removal reaches the agent that wrote the memory ------------------

/// An agent that writes a wrong memory must be able to take it back.
///
/// The field report: a memory asserting three skills had been archived turned
/// out to be wrong for one of them twenty minutes later, and with no remove tool
/// the only available repair was to write a SECOND memory contradicting the
/// first. Both then sit in the store, `memory.search` returns both bm25-ranked
/// with no recency weighting and no supersession relation, and the next reader
/// gets a true document and a false one with no signal which is which. Writing
/// is not a correctness feature on its own; writing plus retracting is.
#[test]
fn remove_memory_retracts_a_doc_the_agent_wrote() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    let added = tool_text(&call(
        &server,
        1,
        "tasqx_add_memory",
        json!({ "title": "wrong claim", "body": "three skills were archived" }),
    ));
    let id = added["id"].as_str().expect("the new doc's id").to_string();

    let found = tool_text(&call(
        &server,
        2,
        "tasqx_search_memory",
        json!({ "query": "three skills were archived" }),
    ));
    assert_eq!(found["count"], json!(1), "the doc must be findable first");

    let removed = call(&server, 3, "tasqx_remove_memory", json!({ "id": id }));
    assert!(!is_error(&removed));
    assert_eq!(tool_text(&removed)["removed"], json!(true));

    let gone = tool_text(&call(
        &server,
        4,
        "tasqx_search_memory",
        json!({ "query": "three skills were archived" }),
    ));
    assert_eq!(
        gone["count"],
        json!(0),
        "a retracted memory that search still returns is the failure this closes"
    );
}

/// Removing an id that is not there is `not_found`, not a silent success: an
/// agent told "removed" about a doc still in the store would stop trying.
#[test]
fn removing_an_unknown_memory_id_is_not_found() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let out = call(
        &server,
        1,
        "tasqx_remove_memory",
        json!({ "id": "019f6a1f-0000-0000-0000-000000000000" }),
    );
    assert!(is_error(&out));
}

/// The removal is a write, and a read-only server refuses it before the engine
/// is touched — the same fence `tasqx_add_memory` sits behind.
#[test]
fn remove_memory_is_write_scoped() {
    let engine = engine();
    let added = engine
        .memory_add(&json!({ "title": "kept", "body": "not going anywhere" }))
        .expect("doc");
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(
        &server,
        1,
        "tasqx_remove_memory",
        json!({ "id": added["id"] }),
    );
    assert!(is_error(&out));
    let still_there = engine
        .memory_search(&json!({ "query": "not going anywhere" }))
        .expect("search");
    assert_eq!(still_there["count"], json!(1));
}

/// The one claim on this tool that a caller cannot discover by trying: the
/// removal is permanent.
///
/// `tasqx undo` (D54) covers task edits and deliberately not memory docs — the
/// event log records that a doc was removed and does not carry its body, so
/// there is nothing to put back. A human at the CLI re-states a note they wrote;
/// an agent handed a delete with no mention of that reads it as reversible,
/// because every other write it can reach through this server is.
#[test]
fn the_removal_tool_says_the_removal_cannot_be_undone() {
    let roster = tasqx_core::mcp::tool_roster();
    assert!(
        roster
            .iter()
            .any(|(n, write)| *n == "tasqx_remove_memory" && *write),
        "the removal tool must ship as a write tool"
    );
    let listed = McpServer::new(&engine(), Scope::Write)
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tool = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t["name"] == "tasqx_remove_memory")
        .expect("the removal tool is listed");
    let description = tool["description"].as_str().expect("a description");
    assert!(
        description.contains("permanent"),
        "the description must say the removal is permanent, got: {description}"
    );
    assert!(
        description.contains("undo"),
        "the description must name `undo` as the thing that does NOT cover it, got: {description}"
    );
    assert_eq!(
        tool["annotations"]["destructiveHint"],
        json!(true),
        "the host's confirmation gate is the safeguard here (DESIGN §7), so the hint that \
         triggers it may not be false"
    );
}

/// The reported failure, through the tool an agent actually calls.
///
/// Completing #207 with `model`, `tool` and `session_id` — all documented
/// optional — was refused, and the retry that succeeded dropped `model` and
/// `tool`, so the store recorded that completion with neither. An agent
/// generally cannot observe its own token spend: no harness hands the model a
/// running count. The old coupling therefore demanded a number the caller
/// cannot see in exchange for recording the two facts it can.
#[test]
fn complete_task_records_tool_and_model_without_token_counts() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    engine
        .task_add(&json!({ "title": "attributed work" }))
        .expect("add");

    let out = call(
        &server,
        1,
        "tasqx_complete_task",
        json!({ "ref": 1, "tool": "claude-code", "model": "claude-opus-5",
                "session_id": "sess-1" }),
    );
    assert!(
        !is_error(&out),
        "a completion naming its tool and model must not be refused: {:?}",
        tool_text(&out)
    );
    let hint = tool_text(&out)["tokens_hint"]
        .as_str()
        .expect("a hint")
        .to_string();
    assert!(
        hint.contains("recorded") && hint.contains("tool") && hint.contains("model"),
        "the response must name what it recorded: {hint}"
    );

    let events = engine
        .event_list(&json!({ "limit": 50 }))
        .expect("event.list");
    let done = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["op"] == "done")
        .expect("a done event");
    assert_eq!(done["payload"]["tool"], json!("claude-code"));
    assert_eq!(done["payload"]["model"], json!("claude-opus-5"));
    assert_eq!(done["payload"]["session_id"], json!("sess-1"));
}

/// The budget spends the JSON block before it spends annotations.
///
/// D49 ships two blocks: the rendered view and the same result as pretty JSON.
/// On a task whose bulk is annotation prose that is the *same text twice*, so
/// half of every oversized response is a duplicate — and under the byte budget
/// the duplicate is paid for in annotations the reader never sees. The view is
/// what leads and what a model reads (D49's own reason for the order), so when
/// something has to go, the redundant block goes first and the history gets the
/// room.
#[test]
fn an_oversized_response_drops_the_duplicate_json_before_it_drops_history() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "eleven long notes" }))
        .expect("add");
    let body = "detail ".repeat(900);
    for i in 0..11 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("## Note {i}\n\n{body}\n") }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Read);
    let out = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    assert!(!is_error(&out));

    let blocks = out["result"]["content"].as_array().expect("content");
    assert_eq!(
        blocks.len(),
        1,
        "the duplicate JSON block must be the first thing sacrificed, not the last"
    );
    let view = blocks[0]["text"].as_str().expect("the view");
    assert!(
        view.len() < 24_576,
        "the surviving block still has to fit: {} bytes",
        view.len()
    );
    // Silent omission of a whole block is the failure shape this repo keeps
    // paying for: the reader has to be told what is not there and how to get it.
    assert!(
        view.contains("annotations_limit"),
        "an omitted JSON block must name the call that brings it back:\n{view}"
    );
    let shown = view.matches("## Note ").count();
    assert!(
        shown >= 3,
        "dropping the duplicate should buy annotations, got {shown} of 11"
    );
}

/// A caller that named its own page size gets both blocks, however large. The
/// frozen machine-readable shape stays reachable for every task — it is only
/// off by default on the ones too big to carry it twice.
#[test]
fn an_explicit_limit_keeps_the_json_block_even_over_budget() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "eleven long notes" }))
        .expect("add");
    let body = "detail ".repeat(900);
    for i in 0..11 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("## Note {i}\n\n{body}\n") }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Read);
    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 11 }),
    );
    let blocks = out["result"]["content"].as_array().expect("content");
    assert_eq!(
        blocks.len(),
        2,
        "an explicit page size is not second-guessed"
    );
    assert_eq!(tool_json(&out)["annotations"].as_array().unwrap().len(), 11);
}

/// An ordinary task is untouched: two blocks, no note, nothing to notice.
#[test]
fn a_response_within_budget_still_carries_both_blocks() {
    let engine = engine();
    engine.task_add(&json!({ "title": "small" })).expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "a short note" }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);
    let out = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    let blocks = out["result"]["content"].as_array().expect("content");
    assert_eq!(blocks.len(), 2);
    assert!(!blocks[0]["text"].as_str().unwrap().contains("omitted"));
}

// ---- the corrective half of every exposed pair --------------------------------

/// An agent that can add a tag can take one off again.
///
/// The MCP surface was additive-only: `tag.add`, `dependency.add` and the
/// completion were reachable and their inverses were not, so an agent that
/// filed something wrong could describe the mistake and never undo it.
#[test]
fn untag_removes_what_tag_added() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    engine
        .task_add(&json!({ "title": "mislabelled" }))
        .expect("add");

    call(
        &server,
        1,
        "tasqx_tag_task",
        json!({ "ref": 1, "tags": ["api", "typo"] }),
    );
    let out = call(
        &server,
        2,
        "tasqx_untag_task",
        json!({ "ref": 1, "tags": ["typo"] }),
    );
    assert!(!is_error(&out));
    let tags = tool_text(&out)["tags"]
        .as_array()
        .expect("the resulting set")
        .clone();
    assert_eq!(tags, vec![json!("api")]);
}

/// A dependency added by mistake blocks the task forever unless it can be cut.
#[test]
fn remove_dependency_unblocks_what_add_dependency_blocked() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    engine
        .task_add(&json!({ "title": "blocker" }))
        .expect("add");
    engine
        .task_add(&json!({ "title": "dependent" }))
        .expect("add");

    let blocked = tool_text(&call(
        &server,
        1,
        "tasqx_add_dependency",
        json!({ "ref": 2, "depends_on": 1 }),
    ));
    assert_eq!(blocked["blocked"], json!(true));

    let out = call(
        &server,
        2,
        "tasqx_remove_dependency",
        json!({ "ref": 2, "depends_on": 1 }),
    );
    assert!(!is_error(&out));
    let after = tool_text(&out);
    assert_eq!(after["blocked"], json!(false));
    assert_eq!(after["depends_on"].as_array().unwrap().len(), 0);
}

/// Completion and cancellation both had reachable destructive halves and no
/// inverse: `task.modify status:cancelled` is exposed by design (§7) and
/// `task.reopen` was not, so an agent could close a task it should not have and
/// had no way back.
#[test]
fn reopen_undoes_a_completion_and_a_cancellation() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    engine
        .task_add(&json!({ "title": "closed too early" }))
        .expect("add");
    engine
        .task_add(&json!({ "title": "cancelled too early" }))
        .expect("add");

    call(&server, 1, "tasqx_complete_task", json!({ "ref": 1 }));
    let reopened = tool_text(&call(&server, 2, "tasqx_reopen_task", json!({ "ref": 1 })));
    assert_eq!(reopened["status"], json!("pending"));

    call(
        &server,
        3,
        "tasqx_modify_task",
        json!({ "ref": 2, "set": { "status": "cancelled" } }),
    );
    let back = tool_text(&call(&server, 4, "tasqx_reopen_task", json!({ "ref": 2 })));
    assert_eq!(back["status"], json!("pending"));
}

/// All three are writes, and a read-only server refuses them before the engine
/// is touched.
#[test]
fn the_corrective_tools_are_write_scoped() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Read);
    for (tool, args) in [
        ("tasqx_untag_task", json!({ "ref": 1, "tags": ["x"] })),
        (
            "tasqx_remove_dependency",
            json!({ "ref": 1, "depends_on": 1 }),
        ),
        ("tasqx_reopen_task", json!({ "ref": 1 })),
    ] {
        assert!(
            is_error(&call(&server, 1, tool, args)),
            "`{tool}` must not be reachable from a read-only server"
        );
    }
}

// ---- D68: the behaviour hints ------------------------------------------------

/// Every tool the running server advertises, name -> annotations.
fn listed_annotations() -> Vec<(String, Value)> {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| {
            (
                t["name"].as_str().expect("a name").to_string(),
                t["annotations"].clone(),
            )
        })
        .collect()
}

/// D68: `destructiveHint` and `idempotentHint` stop being `write` under two
/// other names.
///
/// The emission was `"destructiveHint": s.write` with `idempotentHint` hard
/// false, so all fourteen writes carried one pair and a host gating on
/// `destructiveHint` gated every write or none — which is not a gate, and it
/// is the gate D64 chose as `tasqx_remove_memory`'s only safeguard.
///
/// Asserted as *distinctions* rather than as a table of nineteen literals: a
/// second copy of the table would have to be edited in lockstep with the thing
/// it checks, which is the drift this repository keeps paying for.
#[test]
fn the_behaviour_hints_are_not_the_write_flag_under_another_name() {
    let tools = listed_annotations();
    let hint = |name: &str, key: &str| -> bool {
        tools
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("tool {name} is listed"))
            .1[key]
            .as_bool()
            .unwrap_or_else(|| panic!("{name}.{key} is a boolean"))
    };

    // A write that is additive, and a write that destroys. If these ever agree
    // the hint has collapsed back into the write flag.
    assert!(
        !hint("tasqx_add_task", "destructiveHint"),
        "creating a task is additive"
    );
    assert!(
        hint("tasqx_remove_memory", "destructiveHint"),
        "a permanent, un-undoable removal is the destructive case D64 named"
    );

    // Append-only writes are additive; the correctives are not.
    for additive in [
        "tasqx_add_task",
        "tasqx_annotate_task",
        "tasqx_add_memory",
        "tasqx_add_dependency",
        "tasqx_tag_task",
        "tasqx_start_timer",
        "tasqx_stop_timer",
        "tasqx_create_project",
    ] {
        assert!(
            !hint(additive, "destructiveHint"),
            "`{additive}` only adds to the store"
        );
        assert!(
            !hint(additive, "readOnlyHint"),
            "`{additive}` is still a write"
        );
    }
    for corrective in [
        "tasqx_remove_memory",
        "tasqx_untag_task",
        "tasqx_remove_dependency",
        "tasqx_reopen_task",
        "tasqx_modify_task",
        "tasqx_complete_task",
    ] {
        assert!(
            hint(corrective, "destructiveHint"),
            "`{corrective}` overwrites or removes what the store already held"
        );
    }

    // A read is never destructive, and repeating one changes nothing.
    for (name, ann) in &tools {
        if ann["readOnlyHint"].as_bool() == Some(true) {
            assert_eq!(
                ann["destructiveHint"],
                json!(false),
                "read tool `{name}` cannot be destructive"
            );
            assert_eq!(
                ann["idempotentHint"],
                json!(true),
                "read tool `{name}` has no effect to repeat"
            );
        }
    }

    // `idempotentHint` distinguishes too: set-shaped writes converge, appends
    // do not.
    assert!(
        hint("tasqx_tag_task", "idempotentHint"),
        "attaching a tag the task already carries changes nothing"
    );
    assert!(
        !hint("tasqx_annotate_task", "idempotentHint"),
        "every annotation is a new row"
    );
}

/// D70: an unbounded `tasqx_list_tasks` is bounded by the transport, and the
/// answer says what it withheld.
///
/// Measured on a real store of 223 tasks, `tasqx_list_tasks {}` — the first
/// call an agent makes, and the one this tool's own schema invites with "no
/// filter means no filtering" — returned 180,412 bytes in one block, past most
/// clients' tool-output limit, with no elision and nothing saying anything had
/// been large.
#[test]
fn an_unbounded_task_list_is_bounded_by_the_transport() {
    let engine = engine();
    // Bodies large enough that the whole store cannot fit the budget, in a
    // field every row carries.
    let long = "x".repeat(400);
    for i in 0..400 {
        engine
            .task_add(&json!({ "title": format!("{long} #{i}") }))
            .expect("add");
    }
    let server = McpServer::new(&engine, Scope::Write);
    let result = call(&server, 1, "tasqx_list_tasks", json!({}));
    let bytes = serde_json::to_string(&result).expect("serialize").len();
    assert!(
        bytes <= 32_768,
        "an unbounded list must not blow a client's limit: {bytes} bytes"
    );

    let body = tool_json(&result);
    assert_eq!(body["total"], json!(400), "the answer names what matched");
    let count = body["count"].as_u64().expect("count");
    assert!(count < 400, "the page is smaller than the store: {count}");
    assert_eq!(
        body["next_offset"],
        json!(count),
        "and it names the offset that reaches the rest"
    );
}

/// A caller that names its own `limit` is answered exactly, however large —
/// the rule `fit_to_budget` already keeps for `annotations_limit`.
#[test]
fn a_named_limit_on_task_list_is_answered_as_asked() {
    let engine = engine();
    let long = "y".repeat(400);
    for i in 0..300 {
        engine
            .task_add(&json!({ "title": format!("{long} #{i}") }))
            .expect("add");
    }
    let server = McpServer::new(&engine, Scope::Write);
    let body = tool_json(&call(
        &server,
        1,
        "tasqx_list_tasks",
        json!({ "limit": 300 }),
    ));
    assert_eq!(
        body["count"],
        json!(300),
        "a request second-guessed is a caller who can never page big on purpose"
    );
    assert_eq!(body["next_offset"], Value::Null);
}

/// A small store never notices the transport page exists.
#[test]
fn a_small_store_is_answered_whole_with_the_walk_already_closed() {
    let engine = engine();
    for i in 0..3 {
        engine
            .task_add(&json!({ "title": format!("t{i}") }))
            .expect("add");
    }
    let server = McpServer::new(&engine, Scope::Write);
    let body = tool_json(&call(&server, 1, "tasqx_list_tasks", json!({})));
    assert_eq!(body["count"], json!(3));
    assert_eq!(body["total"], json!(3));
    assert_eq!(body["next_offset"], Value::Null);
}

/// The re-cut page is the page the engine would have returned.
///
/// `fit_list_to_budget` shortens the array it already holds instead of asking
/// the engine again, on the claim that `limit` is a prefix of a fully
/// determined order. That claim is the whole basis for not re-dispatching —
/// and it is only true because `compare_by` ends on an unconditional
/// `short_id`, so this test is what stops the tiebreak being removed as
/// "cosmetic" later.
#[test]
fn the_transport_recut_page_equals_a_real_limited_call() {
    let engine = engine();
    let long = "z".repeat(400);
    for i in 0..400 {
        engine
            .task_add(&json!({ "title": format!("{long} #{i}") }))
            .expect("add");
    }
    let server = McpServer::new(&engine, Scope::Write);
    let recut = tool_json(&call(&server, 1, "tasqx_list_tasks", json!({})));
    let k = recut["count"].as_u64().expect("count");

    let asked = tool_json(&call(&server, 2, "tasqx_list_tasks", json!({ "limit": k })));
    assert_eq!(
        recut, asked,
        "the shortened answer must be the answer, not an approximation of it"
    );
}

// ---- D72: the bytes the caller already holds --------------------------------

/// `include_json: false` returns the rendered view alone, at any size.
///
/// D49 ships the result twice — once formatted, once as escaped JSON — and D66
/// spends the duplicate only once the budget is already blown. Below it every
/// ordinary read paid in full with no way to decline: measured on a live task
/// with ONE annotation, the JSON block was 54% of a 6,375-byte response, and
/// 66% of a 1,351-byte one read with `annotations_limit: 0`.
#[test]
fn include_json_false_returns_the_view_alone() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "parse the statements" }))
        .expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "a".repeat(600) }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Write);

    let both = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    let blocks = both["result"]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 2, "the default is unchanged");

    let view_only = call(
        &server,
        2,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": false }),
    );
    let one = view_only["result"]["content"].as_array().expect("blocks");
    assert_eq!(one.len(), 1, "the view, and nothing restating it");
    assert!(
        !is_error(&view_only),
        "`include_json` must never reach the params gate: {view_only}"
    );

    let big = serde_json::to_string(&both).expect("json").len();
    let small = serde_json::to_string(&view_only).expect("json").len();
    assert!(
        small * 2 <= big,
        "dropping the duplicate has to actually drop it: {small} vs {big}"
    );

    // And it is the bare view: the over-budget notice explains an omission the
    // caller did not choose, so appending it here would be a false sentence
    // charged at ~300 bytes — most of what declining the block was to save.
    let text = one[0]["text"].as_str().expect("text");
    assert!(
        !text.contains("response budget"),
        "a chosen omission is not an over-budget omission: {}",
        &text[text.len().saturating_sub(300)..]
    );
}

/// The argument is consumed by the transport, never forwarded.
///
/// `check_params` refuses any key the method does not accept, so a forwarded
/// `include_json` would be an instant `bad_request` — which is the failure
/// mode this test pins, alongside the one where a caller who names it also
/// names a page size.
#[test]
fn include_json_is_stripped_before_the_params_gate_on_every_path() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    for i in 0..3 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("note {i}") }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Write);
    for (id, args) in [
        (1, json!({ "ref": 1, "include_json": false })),
        (2, json!({ "ref": 1, "include_json": true })),
        (
            3,
            json!({ "ref": 1, "include_json": false, "annotations_limit": 2 }),
        ),
        (
            4,
            json!({ "ref": 1, "include_json": false, "annotations_offset": 1 }),
        ),
    ] {
        let result = call(&server, id, "tasqx_get_task", args.clone());
        assert!(
            !is_error(&result),
            "`{args}` was refused: {}",
            result["result"]["content"][0]["text"]
        );
        let want = if args["include_json"] == json!(true) {
            2
        } else {
            1
        };
        assert_eq!(
            result["result"]["content"]
                .as_array()
                .expect("blocks")
                .len(),
            want,
            "block count for {args}"
        );
    }
}

/// The omission notice says what naming a limit COSTS.
///
/// It read as a bounded retry — "Pass `annotations_limit` to get the JSON
/// block back" — and the obvious value to retry with is the page size printed
/// two lines above it. Measured on a live task: the budgeted answer was 22,932
/// bytes, the same call with the page size it had just been shown was 46,512,
/// and with `annotations_total` (which the tool's own description recommends
/// for a whole history) 173,032 — seven times the budget the server had just
/// refused to exceed.
#[test]
fn the_json_omission_notice_says_that_naming_a_limit_removes_the_budget() {
    let engine = engine();
    engine.task_add(&json!({ "title": "long" })).expect("add");
    for i in 0..12 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("{} #{i}", "z".repeat(3000)) }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Write);
    let result = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    let blocks = result["result"]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1, "the premise: this response is over budget");
    let text = blocks[0]["text"].as_str().expect("text");
    assert!(
        text.contains("unbounded"),
        "the notice must say the escape hatch is an opt-out, not a page: {}",
        &text[text.len().saturating_sub(400)..]
    );
    assert!(
        text.contains("include_json"),
        "and name the way to keep the budget: {}",
        &text[text.len().saturating_sub(400)..]
    );

    // And the claim the notice now makes is true.
    let unbounded = call(
        &server,
        2,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 12 }),
    );
    let big = serde_json::to_string(&unbounded).expect("json").len();
    let budgeted = serde_json::to_string(&result).expect("json").len();
    assert!(
        big > budgeted * 2,
        "naming a limit really is several times the budgeted answer: {big} vs {budgeted}"
    );
}
