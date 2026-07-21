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

    // 3. tools/list — all ~11 tools present, each with an inputSchema.
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 13, "expected 13 tools");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "tasqx_list_tasks",
        "tasqx_get_task",
        "tasqx_summary",
        "tasqx_list_projects",
        "tasqx_add_task",
        "tasqx_modify_task",
        "tasqx_complete_task",
        "tasqx_start_timer",
        "tasqx_stop_timer",
        "tasqx_tag_task",
        "tasqx_annotate_task",
        "tasqx_add_dependency",
        "tasqx_create_project",
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
    // Write tools are annotated destructive; reads are read-only.
    let get_add = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
    assert_eq!(
        get_add("tasqx_add_task")["annotations"]["destructiveHint"],
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
    // A read-only session advertises only the four read tools.
    assert_eq!(tools.len(), 4, "read scope should list only the read tools");
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
    assert_eq!(tool_text(&got)["annotations"][0]["body"], body);
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

// ---- operator-selected scope -------------------------------------------------

#[test]
fn scope_is_a_capability_choice_not_a_credential() {
    assert_eq!(Scope::Read.as_str(), "read");
    assert_eq!(Scope::Write.as_str(), "write");
    assert!(!Scope::Read.allows_write());
    assert!(Scope::Write.allows_write());
}
