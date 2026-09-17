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
    // …and the handshake carries the workflow, not just the inventory.
    assert!(init["result"]["instructions"].is_string());

    // 2. notifications/initialized — a notification yields NO response.
    let note = server.handle_message(&json!({
        "jsonrpc": "2.0", "method": "notifications/initialized"
    }));
    assert!(note.is_none(), "notifications must not produce a response");

    // 3. tools/list — all 29 tools present, each with an inputSchema.
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 29, "expected 29 tools");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "tasqx_list_tasks",
        "tasqx_get_task",
        "tasqx_summary",
        "tasqx_outcomes",
        "tasqx_brief_task",
        "tasqx_list_projects",
        "tasqx_search_memory",
        "tasqx_add_task",
        "tasqx_modify_task",
        "tasqx_complete_task",
        "tasqx_reopen_task",
        "tasqx_cancel_task",
        "tasqx_start_timer",
        "tasqx_stop_timer",
        "tasqx_tag_task",
        "tasqx_untag_task",
        "tasqx_annotate_task",
        "tasqx_remove_annotation",
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
    // A read-only session advertises only the nine read tools — including all
    // three memory readers: a read-only agent may consult knowledge (D41), D71
    // made "consult" mean the document rather than an excerpt of it, and #133
    // added browsing to that same read-only set. D137's `tasqx_outcomes` joined
    // them for the same reason: an agent that cannot write should still be able
    // to see its own record, and D136's `tasqx_brief_task` for the reason
    // beside it — orienting is a read.
    assert_eq!(tools.len(), 9, "read scope should list only the read tools");
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

// ---- initialize instructions -------------------------------------------------

/// The `instructions` string as a live server answers it over the wire, so
/// every claim below is about the handshake a host actually reads and not
/// about a helper nobody is wired to.
fn instructions_of(scope: Scope) -> String {
    let engine = engine();
    let server = McpServer::new(&engine, scope);
    let init = server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {} }
        }))
        .expect("initialize is a request");
    init["result"]["instructions"]
        .as_str()
        .expect("initialize carries instructions as a string")
        .to_string()
}

/// Every `tasqx_…` run in a piece of prose, however it is punctuated around.
///
/// The same scan the guides' drift guard uses (`readme.rs`), kept regex-free
/// for the same reason: a tool name is an identifier run, and stopping at the
/// first character that cannot be in one is the whole parser.
fn tool_names_in(text: &str) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("tasqx_") {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(tail.len());
        let run = &tail[..end];
        if run.len() > "tasqx_".len() {
            named.push(run.to_string());
        }
        rest = &tail[end..];
    }
    named.sort();
    named.dedup();
    named
}

/// The MCP server tells a host what it CAN call and, until now, nothing told it
/// WHEN. `instructions` is the one in-band place a cross-tool workflow can live
/// — hosts such as Claude Code inject it into the agent's system prompt — so a
/// write-scoped handshake has to carry the whole loop: search, then track, then
/// write back.
#[test]
fn initialize_instructions_carry_the_write_scope_workflow() {
    let text = instructions_of(Scope::Write);
    assert!(!text.is_empty(), "instructions must not be an empty string");
    for phrase in [
        "tasqx_search_memory",
        "tasqx_annotate_task",
        "tasqx_complete_task",
        "tasqx_add_memory",
        "@working",
    ] {
        assert!(
            text.contains(phrase),
            "the write-scope instructions must name {phrase:?}:\n{text}"
        );
    }
}

/// Under a read-only scope the write tools are not in `tools/list`, so naming
/// one in the instructions is worse than saying nothing: it sends the agent at
/// a tool it will be refused, and the refusal is the first it hears of the
/// scope. The read variant says the scope out loud instead.
#[test]
fn initialize_instructions_under_read_scope_name_no_write_tool() {
    let text = instructions_of(Scope::Read);
    assert!(
        text.contains("tasqx_search_memory"),
        "the read-only instructions must still point at the one tool that works:\n{text}"
    );
    assert!(
        text.contains("read-only"),
        "the read-only instructions must say the server is read-only:\n{text}"
    );
    let roster = tasqx_core::mcp::tool_roster();
    let writes: Vec<&str> = roster
        .iter()
        .filter(|(_, write)| *write)
        .map(|(n, _)| *n)
        .collect();
    assert!(
        writes.len() >= 10,
        "the write half of the roster shrank to {writes:?} — an empty scan \
         must not pass as a clean one"
    );
    for name in writes {
        assert!(
            !text.contains(name),
            "the read-only instructions tell an agent to call {name:?}, which is \
             not listed under this scope:\n{text}"
        );
    }
}

/// The instructions are prose with no generator behind them, and a tool name
/// misspelled in them is a tool call that fails as `unknown tool` — in the
/// agent's system prompt, where nothing in the build would ever look. Both
/// scopes are scanned: the read variant drops two paragraphs, so a rename
/// could survive in exactly the half the other test does not read.
#[test]
fn initialize_instructions_name_only_tools_that_exist() {
    let roster = tasqx_core::mcp::tool_roster();
    for scope in [Scope::Read, Scope::Write] {
        let text = instructions_of(scope);
        let named = tool_names_in(&text);
        assert!(
            named.len() >= 3,
            "the scan of the {} instructions found only {named:?} — an empty \
             scan must not pass as a clean one",
            scope.as_str()
        );
        for name in &named {
            assert!(
                roster.iter().any(|(n, _)| n == name),
                "the {} instructions tell an agent to call {name:?}, which is not \
                 in the MCP roster: {:?}",
                scope.as_str(),
                roster.iter().map(|(n, _)| *n).collect::<Vec<_>>()
            );
        }
    }
}

// ---- initialize standing rulings (#96, D157) ----------------------------------

/// [`instructions_of`] for a server the test built itself, so it can carry a
/// store and a working directory.
fn instructions_from(server: &McpServer) -> String {
    let init = server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {} }
        }))
        .expect("initialize is a request");
    init["result"]["instructions"]
        .as_str()
        .expect("initialize carries instructions as a string")
        .to_string()
}

/// The rulings section alone: what follows the D141 text and its blank line.
fn rulings_of(server: &McpServer) -> String {
    let text = instructions_from(server);
    let base = format!("{}\n\n", tasqx_core::mcp::instructions(server.scope()));
    text.strip_prefix(&base)
        .unwrap_or_else(|| panic!("no rulings section after the D141 text:\n{text}"))
        .to_string()
}

/// A real, freshly created directory under the system temp dir.
fn temp_workdir(rel: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("tasqx-mcp-96-{}", std::process::id()))
        .join(rel);
    std::fs::create_dir_all(&dir).expect("create the workdir");
    dir
}

fn add_doc(engine: &Engine, title: &str, body: &str, project: Option<&str>, standing: bool) {
    engine
        .memory_add(&json!({
            "title": title, "body": body, "project": project, "standing": standing
        }))
        .expect("memory.add");
}

/// #96/D157: with nothing standing and nothing topical to show, the handshake
/// is exactly D141's text — a store full of unscoped imported docs must not
/// start charging every prompt for them.
#[test]
fn initialize_instructions_on_an_empty_store_are_byte_identical_to_the_workflow_text() {
    let dir = temp_workdir("empty/sub");
    for filled in [false, true] {
        let engine = engine();
        if filled {
            add_doc(&engine, "an imported adr", "not standing", None, false);
        }
        for scope in [Scope::Read, Scope::Write] {
            for workdir in [None, Some(dir.clone())] {
                let server = McpServer::new(&engine, scope).with_workdir(workdir);
                assert_eq!(
                    instructions_from(&server),
                    tasqx_core::mcp::instructions(scope),
                    "filled={filled}, scope={}",
                    scope.as_str()
                );
            }
        }
    }
}

/// #96/D157: the server runs in the session's repo, so the directory names the
/// project before the store-wide default does — through its ancestors, since
/// a task worktree's own basename is `<id>-<slug>`.
#[test]
fn standing_rulings_follow_the_working_directory_before_the_default_project() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    engine.project_create(&json!({ "name": "beta" })).unwrap();
    add_doc(&engine, "alpha rule", "alpha only", Some("alpha"), true);
    add_doc(&engine, "beta rule", "beta only", Some("beta"), true);
    add_doc(&engine, "global rule", "everywhere", None, true);

    for rel in ["beta/sub", "worktrees/beta/96-something"] {
        let server = McpServer::new(&engine, Scope::Write).with_workdir(Some(temp_workdir(rel)));
        let text = rulings_of(&server);
        assert!(
            text.starts_with("Standing rulings for project beta (working directory)."),
            "{rel}:\n{text}"
        );
        assert!(text.contains("- beta rule: beta only"), "{rel}:\n{text}");
        assert!(text.contains("- global rule: everywhere"), "{rel}:\n{text}");
        assert!(!text.contains("alpha rule"), "{rel}:\n{text}");
        assert!(
            text.find("beta rule") < text.find("global rule"),
            "project-scoped rulings come first:\n{text}"
        );
    }

    let server = McpServer::new(&engine, Scope::Write);
    let text = rulings_of(&server);
    assert!(
        text.starts_with("Standing rulings for project alpha (default project)."),
        "{text}"
    );
    assert!(
        text.contains("alpha rule") && !text.contains("beta rule"),
        "{text}"
    );
}

#[test]
fn with_no_project_inferred_only_unscoped_standing_docs_are_shown() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    engine.project_archive(&json!({ "name": "alpha" })).unwrap();
    assert_eq!(engine.default_project().unwrap(), None);
    add_doc(&engine, "alpha rule", "scoped", Some("alpha"), true);
    add_doc(&engine, "global rule", "unscoped", None, true);

    let text = rulings_of(&McpServer::new(&engine, Scope::Write));
    assert!(text.contains("no project inferred"), "{text}");
    assert!(text.contains("- global rule: unscoped"), "{text}");
    assert!(!text.contains("alpha rule"), "{text}");
}

/// #96/D157: topical fill is the project's own docs only — `memory import`
/// stores ADRs unscoped, so the newest unscoped docs are an arbitrary slice —
/// and the whole section stays inside the per-prompt budget.
#[test]
fn topical_fill_uses_only_the_projects_own_docs_and_stays_within_the_budget() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    add_doc(&engine, "the one rule", "short", Some("alpha"), true);
    let long = "word ".repeat(80);
    for i in 0..40 {
        add_doc(
            &engine,
            &format!("topical {i}"),
            &long,
            Some("alpha"),
            false,
        );
        add_doc(&engine, &format!("stray {i}"), "unscoped", None, false);
    }

    let text = rulings_of(&McpServer::new(&engine, Scope::Write));
    assert!(
        text.len() <= tasqx_core::mcp::STANDING_RULINGS_BUDGET,
        "{} bytes:\n{text}",
        text.len()
    );
    assert!(text.contains("- the one rule: short"), "{text}");
    assert!(!text.contains("stray"), "{text}");
    assert!(text.contains("- topical 39: word"), "newest first:\n{text}");
    let shown = text.matches("\n- topical ").count();
    assert!(shown > 0 && shown < 40, "{shown} shown:\n{text}");
    assert!(
        text.ends_with(&format!(
            "\n{} more docs for project alpha; tasqx_search_memory reaches them.",
            40 - shown
        )),
        "{text}"
    );
}

/// PR #48 review: topical fill reads as context, not as a ruling to follow
/// — it sits under its own heading, after every standing entry.
#[test]
fn topical_entries_sit_under_their_own_heading_after_standing_entries() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    add_doc(&engine, "alpha rule", "scoped", Some("alpha"), true);
    add_doc(
        &engine,
        "alpha note",
        "unscoped context",
        Some("alpha"),
        false,
    );

    let text = rulings_of(&McpServer::new(&engine, Scope::Write));
    let heading = text
        .find("Recent notes for project alpha (context, not rulings):")
        .unwrap_or_else(|| panic!("no topical heading:\n{text}"));
    let standing_at = text.find("alpha rule").expect("standing title present");
    let topical_at = text.find("alpha note").expect("topical title present");
    assert!(standing_at < heading, "{text}");
    assert!(topical_at > heading, "{text}");
}

/// PR #48 review: with nothing standing, the section must not announce
/// rulings it does not have — only the "Recent notes" heading appears.
#[test]
fn with_only_topical_docs_the_section_carries_no_standing_rulings_heading() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    add_doc(
        &engine,
        "alpha note",
        "unscoped context",
        Some("alpha"),
        false,
    );

    let text = rulings_of(&McpServer::new(&engine, Scope::Write));
    assert!(!text.contains("Standing rulings"), "{text}");
    assert!(
        text.contains("Recent notes for project alpha (context, not rulings):"),
        "{text}"
    );
    assert!(text.contains("alpha note"), "{text}");
}

/// PR #48 review: `session_rulings` bounds what one session reads with
/// `LIMIT 256`, so the footer's count must come from a separate `COUNT(*)`
/// (`topical_total`) rather than the length of what was actually fetched,
/// once a project holds more than that.
#[test]
fn the_footer_counts_every_topical_doc_not_only_the_ones_read() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    for i in 0..300 {
        add_doc(
            &engine,
            &format!("topical {i}"),
            "short body",
            Some("alpha"),
            false,
        );
    }

    let text = rulings_of(&McpServer::new(&engine, Scope::Write));
    let shown = text.matches("\n- topical ").count();
    assert!(shown > 0 && shown < 300, "{shown} shown:\n{text}");
    assert!(
        text.ends_with(&format!(
            "\n{} more docs for project alpha; tasqx_search_memory reaches them.",
            300 - shown
        )),
        "{text}"
    );
}

/// #96/D157: standing docs are never dropped. When their gists overflow the
/// budget every title is still listed, and the section says to consolidate.
#[test]
fn an_oversized_standing_set_lists_every_title_and_warns() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    let long = "gisttext ".repeat(40);
    for i in 0..20 {
        add_doc(&engine, &format!("ruling {i}"), &long, Some("alpha"), true);
    }
    add_doc(&engine, "topical doc", "fill", Some("alpha"), false);

    let text = rulings_of(&McpServer::new(&engine, Scope::Write));
    for i in 0..20 {
        assert!(
            text.contains(&format!("\n- ruling {i}\n")),
            "ruling {i}:\n{text}"
        );
    }
    assert!(!text.contains("gisttext"), "{text}");
    assert!(!text.contains("topical doc"), "{text}");
    assert!(
        text.ends_with(&format!(
            "\n20 standing rulings exceed the {}-byte session budget, so only titles are \
             shown; list them with `tasqx memory list --standing` and merge or clear some.",
            tasqx_core::mcp::STANDING_RULINGS_BUDGET
        )),
        "{text}"
    );
}

#[test]
fn the_gist_is_the_first_paragraph_without_frontmatter() {
    let engine = engine();
    add_doc(
        &engine,
        "fm rule",
        "---\nstatus: accepted\n---\n\nFirst   paragraph\nwraps here.\n\nSecond paragraph.",
        None,
        true,
    );
    // A body that is frontmatter alone has no gist: the entry is its title.
    add_doc(
        &engine,
        "bare rule",
        "---\nstatus: accepted\n---\n",
        None,
        true,
    );
    let text = rulings_of(&McpServer::new(&engine, Scope::Write));
    // Newest first: the bare rule was written last.
    assert!(
        text.ends_with("\n- fm rule: First paragraph wraps here."),
        "{text}"
    );
    assert!(text.contains("\n- bare rule\n"), "{text}");
    assert!(!text.contains("---") && !text.contains("Second"), "{text}");
}

/// Read-only sessions get the rulings too: they are what an earlier session
/// decided, and a read-only agent must follow them as much as any other. The
/// section's own prose (here with its footer) names no tool this scope lacks.
#[test]
fn initialize_instructions_under_read_scope_carry_standing_rulings() {
    let engine = engine();
    engine.project_create(&json!({ "name": "alpha" })).unwrap();
    add_doc(&engine, "global rule", "everywhere", None, true);
    for i in 0..40 {
        add_doc(
            &engine,
            &format!("topical {i}"),
            &"word ".repeat(80),
            Some("alpha"),
            false,
        );
    }
    let text = rulings_of(&McpServer::new(&engine, Scope::Read));
    assert!(text.contains("- global rule: everywhere"), "{text}");
    assert!(text.contains("more docs for project alpha"), "{text}");
    let roster = tasqx_core::mcp::tool_roster();
    for name in tool_names_in(&text) {
        assert!(
            roster.iter().any(|(n, w)| *n == name && !*w),
            "the read-scope rulings name {name:?}, not a read tool:\n{text}"
        );
    }
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

    // The annotation is readable back through tasqx_get_task, unmangled. Read
    // from the machine block, which D151 made opt-in.
    let got = call(
        &server,
        3,
        "tasqx_get_task",
        json!({ "ref": short_id, "include_json": true }),
    );
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

    let got = tool_json(&call(
        &server,
        4,
        "tasqx_get_task",
        json!({ "ref": sid, "include_json": true }),
    ));
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

/// D153: the closing card a person reads after a completion comes out of
/// `tasqx_complete_task` itself, so the `tasqx_get_task` re-read that used to
/// follow every completion disappears. The JSON block behind it is the frozen
/// `task.done` result, unchanged by the view.
#[test]
fn complete_task_with_view_card_leads_with_the_closing_card_and_keeps_its_json() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);

    let finish = |id: i64, title: &str, view: Option<&str>| {
        let added = call(&server, id, "tasqx_add_task", json!({ "title": title }));
        let sid = tool_text(&added)["short_id"].as_i64().expect("short_id");
        let annotated = call(
            &server,
            id + 1,
            "tasqx_annotate_task",
            json!({ "ref": sid, "body": "Shipped the widget.\n\nDetails." }),
        );
        assert!(!is_error(&annotated), "annotate failed: {annotated}");
        let checked = call(
            &server,
            id + 2,
            "tasqx_add_check",
            json!({ "ref": sid, "body": "the widget ships" }),
        );
        assert!(!is_error(&checked), "add_check failed: {checked}");
        let check_id = tool_text(&checked)["check"]["id"]
            .as_str()
            .expect("check id")
            .to_string();
        let mut args = json!({ "ref": sid, "checks_passed": [check_id] });
        if let Some(v) = view {
            args["view"] = json!(v);
        }
        let done = call(&server, id + 3, "tasqx_complete_task", args);
        assert!(!is_error(&done), "complete failed: {done}");
        (sid, done)
    };

    let (sid, done) = finish(1, "carded", Some("card"));
    let blocks = done["result"]["content"]
        .as_array()
        .expect("content is an array");
    assert_eq!(blocks.len(), 2, "card + JSON, nothing else: {done}");

    let card = blocks[0]["text"].as_str().expect("the card block");
    assert!(
        card.starts_with("```text\n\u{250c}") && card.ends_with("```\n"),
        "block 0 must be the fenced box card: {card:?}"
    );
    assert!(
        card.contains("Delivered") && card.contains("Shipped the widget."),
        "the card must be the task AS COMPLETED: {card}"
    );
    assert!(
        card.contains("[x]"),
        "the card must show the check it was completed with: {card}"
    );

    let result = tool_json(&done);
    assert_eq!(result["status"], "done");
    assert_eq!(result["short_id"], sid);

    // The same completion without `view`: one block, and the very same JSON
    // keys — the view changes the wrapping, never the frozen result (D56).
    let (_, plain) = finish(10, "plain", None);
    assert_eq!(
        plain["result"]["content"]
            .as_array()
            .expect("content is an array")
            .len(),
        1,
        "no view means the single JSON block it has always been: {plain}"
    );
    let keys = |v: &Value| {
        let mut k: Vec<String> = v
            .as_object()
            .expect("an object result")
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&result),
        keys(&tool_json(&plain)),
        "view: \"card\" must not add, drop or rename a `task.done` field"
    );
}

/// An unreadable `view` is refused BEFORE dispatch, so the refusal costs the
/// caller nothing: the task is still there to complete with a spelling the
/// transport knows.
#[test]
fn a_closing_card_over_the_budget_gives_way_to_a_notice_and_the_json_still_arrives() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let added = call(
        &server,
        1,
        "tasqx_add_task",
        json!({ "title": "Sixty long checks" }),
    );
    let sid = tool_text(&added)["short_id"].clone();
    // Each check wraps to ~9 card lines; sixty of them is a card well past
    // the budget, and there is no annotation page whose cutting could help.
    for i in 0..60 {
        let body = format!("check {i}: {}", "criterion ".repeat(50));
        let checked = call(
            &server,
            100 + i,
            "tasqx_add_check",
            json!({ "ref": sid, "body": body }),
        );
        assert!(!is_error(&checked), "add_check failed: {checked}");
    }
    let done = call(
        &server,
        2,
        "tasqx_complete_task",
        json!({ "ref": sid, "view": "card" }),
    );
    assert!(!is_error(&done), "complete failed: {done}");
    let blocks = done["result"]["content"]
        .as_array()
        .expect("content is an array");
    assert_eq!(blocks.len(), 2, "notice + JSON: {done}");
    let notice = blocks[0]["text"].as_str().expect("the notice block");
    assert!(
        notice.starts_with("Closing card omitted") && notice.contains("tasqx_get_task"),
        "an over-budget card is replaced by a notice naming the read that draws it: {notice}"
    );
    let total: usize = blocks
        .iter()
        .map(|b| b["text"].as_str().map(str::len).unwrap_or(0))
        .sum();
    assert!(total <= 24_576, "{total} bytes is over the budget");
    let result = tool_json(&done);
    assert_eq!(
        result["status"], "done",
        "the completion itself is untouched"
    );
}

#[test]
fn complete_task_with_an_unreadable_view_is_refused_before_it_completes() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let added = call(&server, 1, "tasqx_add_task", json!({ "title": "survivor" }));
    let sid = tool_text(&added)["short_id"].as_i64().expect("short_id");

    let refused = call(
        &server,
        2,
        "tasqx_complete_task",
        json!({ "ref": sid, "view": "table" }),
    );
    assert!(
        is_error(&refused),
        "an unknown view must be an isError result"
    );
    let msg = refused["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(msg.contains("view"), "the error must name `view`: {msg}");

    let got = tool_json(&call(
        &server,
        3,
        "tasqx_get_task",
        json!({ "ref": sid, "include_json": true }),
    ));
    assert_eq!(
        got["status"], "pending",
        "a refused view must not have completed the task: {got}"
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

    // `include_json: true`: the page this asserts on is read from the machine
    // block, and D151 made that block opt-in. The page size is still unnamed,
    // which is what this test is about.
    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": true }),
    );
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
        json!({ "ref": 1, "annotations_limit": 3, "include_json": true }),
    ));
    assert_eq!(three["annotations"].as_array().unwrap().len(), 3);

    let all = tool_json(&call(
        &server,
        2,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 40, "include_json": true }),
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

    let json = tool_json(&call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": true }),
    ));
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

/// The budget holds for an explicit page size too (D148, superseding D66's
/// exemption).
///
/// The field report: `tasqx_get_task {ref: 609, include_json: false,
/// annotations_limit: 5}` answered **244,633 bytes** against a 24,576-byte
/// budget, and the same call with NO limit answered 245,038 — naming a limit
/// removed the bound, and the client refused the tool result either way. An
/// exemption a caller can only discover by blowing their client's output limit
/// is not an escape hatch; `max_body_bytes` is, and it says what it costs.
#[test]
fn an_explicit_limit_over_budget_is_answered_view_only() {
    let engine = engine();
    engine.task_add(&json!({ "title": "big" })).expect("add");
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
    assert!(!is_error(&out));
    let sent = serde_json::to_string(&out).expect("json").len();
    assert!(
        sent < 24_576,
        "an explicit limit used to exempt the whole response; this one is {sent} bytes"
    );

    let blocks = out["result"]["content"].as_array().expect("content");
    assert_eq!(
        blocks.len(),
        1,
        "the rendered view alone is the answer (D151), and the bound is what is on trial here"
    );
    let view = blocks[0]["text"].as_str().expect("the view");
    assert!(
        view.contains("of 11"),
        "a cut page still says how much of the history it holds:\n{view}"
    );
    assert!(
        view.contains("annotations_offset"),
        "and how to reach the rest:\n{view}"
    );
    let shown = view.matches("## Note ").count();
    assert!(
        (1..11).contains(&shown),
        "expected the page cut to what fits, got {shown} of 11"
    );
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
///
/// Driven with `include_json: true`, because the spend ORDER is what this pins
/// and since D151 there is nothing to spend unless the caller asked for it.
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
    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": true }),
    );
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
        view.contains("include_json") && view.contains("annotations_limit"),
        "an omitted JSON block must name the argument that asked for it and the one that \
         pages the history:\n{view}"
    );
    let shown = view.matches("## Note ").count();
    assert!(
        shown >= 3,
        "dropping the duplicate should buy annotations, got {shown} of 11"
    );
}

/// A caller that named its own page size keeps BOTH blocks whenever the answer
/// fits — the budget is a bound, not a policy of dropping the machine block.
///
/// This is what D148 leaves of D66's exemption: the frozen machine-readable
/// shape stays reachable for every task whose answer fits, and the answers that
/// do not fit are the ones a client was refusing anyway. Reachable by asking —
/// `include_json: true` since D151.
#[test]
fn an_explicit_limit_whose_answer_fits_keeps_both_blocks() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "eleven short notes" }))
        .expect("add");
    for i in 0..11 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("## Note {i}\n\nshort.\n") }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Read);
    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 11, "include_json": true }),
    );
    let blocks = out["result"]["content"].as_array().expect("content");
    assert_eq!(
        blocks.len(),
        2,
        "a page that fits is answered exactly as asked, both blocks"
    );
    assert_eq!(tool_json(&out)["annotations"].as_array().unwrap().len(), 11);
}

/// The reported defect, end to end: ONE 240,000-byte annotation, read with an
/// explicit page size and with none, both under the budget (D148).
///
/// The page size was never the lever here. The bisection's floor is one whole
/// annotation, and this task's one annotation is ten times the budget on its
/// own, so every page from 20 down to 1 answered the same quarter of a
/// megabyte. `max_body_bytes` is the lever that was missing, and the transport
/// applies it by default.
#[test]
fn one_enormous_annotation_fits_the_budget_at_every_page_size() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "one enormous note" }))
        .expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "z".repeat(240_000) }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);

    for (id, args) in [
        (
            1,
            json!({ "ref": 1, "include_json": false, "annotations_limit": 5 }),
        ),
        (2, json!({ "ref": 1 })),
    ] {
        let out = call(&server, id, "tasqx_get_task", args.clone());
        assert!(!is_error(&out), "{args}: {out}");
        let sent = serde_json::to_string(&out).expect("json").len();
        assert!(
            sent < 24_576,
            "{args} answered {sent} bytes; the field report measured 244,633 and 245,038"
        );
        let view = out["result"]["content"][0]["text"]
            .as_str()
            .expect("the view");
        assert!(
            view.contains("truncated: 240000 bytes"),
            "the cut has to be MARKED, with the original size on it:\n{view}"
        );
        assert!(
            view.contains("`max_body_bytes: 240000`"),
            "and name the exact call that reads the note whole:\n{view}"
        );
    }
}

/// The cut is visible in the machine block too, on the rows it happened to,
/// and only on those rows.
#[test]
fn the_json_block_marks_which_bodies_were_cut() {
    let engine = engine();
    engine.task_add(&json!({ "title": "mixed" })).expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "small enough" }))
        .expect("annotate");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "y".repeat(30_000) }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "max_body_bytes": 1_000, "include_json": true }),
    );
    let blocks = out["result"]["content"].as_array().expect("content");
    assert_eq!(blocks.len(), 2, "capped, this answer fits both blocks");
    let rows = tool_json(&out)["annotations"]
        .as_array()
        .expect("annotations")
        .clone();
    assert!(
        rows[0].get("body_truncated").is_none() && rows[0].get("body_bytes").is_none(),
        "the small note is untouched: {}",
        rows[0]
    );
    assert_eq!(rows[1]["body_truncated"], json!(true));
    assert_eq!(rows[1]["body_bytes"], json!(30_000));
    assert_eq!(rows[1]["body"].as_str().expect("body").len(), 1_000);
}

/// Raising the cap is the deliberate escape, and it is answered as asked.
///
/// The floor stays what D63 set it at — one whole annotation — so a caller who
/// raises `max_body_bytes` past the budget on purpose, with
/// `annotations_limit: 1` so it is one note and not a history, gets every byte
/// of that note. What changed is that this now takes an argument that says what
/// it is doing, rather than being what any page size at all did by accident.
#[test]
fn a_raised_cap_returns_the_whole_body_on_purpose() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "one enormous note" }))
        .expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "z".repeat(240_000) }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({
            "ref": 1, "include_json": false,
            "annotations_limit": 1, "max_body_bytes": 300_000
        }),
    );
    let blocks = out["result"]["content"].as_array().expect("content");
    assert_eq!(blocks.len(), 1, "`include_json: false` was asked for");
    let view = blocks[0]["text"].as_str().expect("the view");
    assert!(
        view.contains(&"z".repeat(240_000)),
        "the whole body, uncut: {} bytes of view",
        view.len()
    );
    assert!(
        !view.contains("truncated:"),
        "and nothing claiming it was cut"
    );
}

/// The cap reaches the engine through `tasqx_brief_task` as well — the brief's
/// task half is `task.get`'s own result (D136), so an uncapped one is the same
/// oversized answer with a neighbourhood stapled to it.
#[test]
fn the_brief_takes_the_body_cap_too() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "briefed" }))
        .expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "y".repeat(30_000) }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(
        &server,
        1,
        "tasqx_brief_task",
        json!({ "ref": 1, "max_body_bytes": 500 }),
    );
    assert!(!is_error(&out), "the brief must accept the cap: {out}");
    let sent = serde_json::to_string(&out).expect("json").len();
    assert!(sent < 24_576, "the brief is bounded too: {sent} bytes");

    // And the default cap applies with nothing named at all.
    let defaulted = call(&server, 2, "tasqx_brief_task", json!({ "ref": 1 }));
    assert!(!is_error(&defaulted));
    let view = defaulted["result"]["content"][0]["text"]
        .as_str()
        .expect("the view");
    assert!(
        view.contains("truncated: 30000 bytes"),
        "the transport's own default cap has to reach the brief:\n{}",
        &view[..view.len().min(600)]
    );
}

/// An ordinary task asked for both blocks gets both, no note, nothing to
/// notice: the budget is a bound on oversized answers, not a policy.
#[test]
fn a_response_within_budget_carries_both_blocks_when_asked_for() {
    let engine = engine();
    engine.task_add(&json!({ "title": "small" })).expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "a short note" }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);
    let out = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": true }),
    );
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

/// The published `tasqx_untag_task` description must say what D52 actually
/// does — `not_found`, all-or-nothing — not its opposite (audit #170).
///
/// Before this fix the description read "Removing a tag the task does not
/// carry is not an error", which is the exact behaviour D52 refused: an
/// agent designing a tag-sync routine against that sentence would fire
/// `tag.remove` over a union of tags and read a miss as a harmless no-op,
/// when the real contract removes nothing and errors.
#[test]
fn untag_task_description_matches_d52_rather_than_contradicting_it() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let untag = tools
        .iter()
        .find(|t| t["name"] == "tasqx_untag_task")
        .expect("tasqx_untag_task is listed");
    let desc = untag["description"].as_str().expect("a description");
    assert!(
        !desc.contains("is not an error"),
        "the description still claims the opposite of D52: {desc}"
    );
    assert!(
        desc.contains("not_found") && desc.to_lowercase().contains("all"),
        "the description should name the real contract (not_found, all-or-nothing): {desc}"
    );
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

/// D114: `task.cancel` was on `UNEXPOSED_METHODS` (its reasoning: already
/// reachable via `tasqx_modify_task status:cancelled`) until the MCP tool's
/// discoverability gap — no status enum on `tasqx_modify_task`'s schema — was
/// judged to outweigh that. This is the same round trip the read-only
/// `task.modify status:cancelled` path already had, now via the purpose-built
/// tool, and it exercises the D11 unblock cascade `tasqx_complete_task`
/// already reports for the analogous close.
#[test]
fn cancel_tool_closes_a_task_and_reports_the_unblock_cascade() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    engine
        .task_add(&json!({ "title": "blocker" }))
        .expect("add");
    engine
        .task_add(&json!({ "title": "blocked" }))
        .expect("add");
    engine
        .dependency_add(&json!({ "ref": 2, "depends_on": 1 }))
        .expect("dependency_add");

    let cancelled = tool_text(&call(&server, 1, "tasqx_cancel_task", json!({ "ref": 1 })));
    assert_eq!(cancelled["status"], json!("cancelled"));
    let unblocked = cancelled["unblocked"].as_array().expect("unblocked array");
    assert_eq!(unblocked.len(), 1, "{cancelled:?}");

    // A task that is already terminal is a conflict, not a silent no-op.
    let out = call(&server, 2, "tasqx_cancel_task", json!({ "ref": 1 }));
    assert!(is_error(&out), "re-cancelling a cancelled task must error");
}

/// The tool used to be absent entirely (D67); this guards the other direction
/// of D114's narrowing — it must not still be excused in `UNEXPOSED_METHODS`
/// now that it ships a tool.
#[test]
fn cancel_is_no_longer_excused_as_unexposed() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let tools = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"tasqx_cancel_task"),
        "task.cancel must be exposed, not excused: {names:?}"
    );
}

/// All four are writes, and a read-only server refuses them before the engine
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
        ("tasqx_cancel_task", json!({ "ref": 1 })),
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

// ---- D152: the row the caller did not project -------------------------------

/// The nine fields a default `tasqx_list_tasks` row carries (D152).
///
/// A deliberate second copy of `mcp::LIST_DEFAULT_FIELDS`, which is private to
/// the crate: narrowing the default row changes what every agent reads, so it
/// should cost a line here rather than pass unnoticed.
const DEFAULT_ROW_FIELDS: [&str; 9] = [
    "short_id", "title", "status", "priority", "urgency", "blocked", "due", "project", "tags",
];

/// The key set of one row, sorted.
fn row_keys(row: &Value) -> Vec<String> {
    let mut keys: Vec<String> = row
        .as_object()
        .expect("a row is an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// D152: a caller that names no `fields` gets the nine an agent picking work
/// reads, with the unset ones left out entirely.
///
/// Measured over 36 hours of transcripts, `fields` was never passed once, and
/// a `@working` page was 41 rows of 637 bytes carrying 22 keys each — most of
/// them null. `scheduled`, `wait`, `remind`, `recurrence`, `completed` and
/// `budget_tokens` were spelled on every row to say nothing about any of them.
#[test]
fn a_default_task_list_row_is_the_nine_fields_an_agent_reads() {
    let engine = engine();
    // D23: an explicit project must name a live project row.
    engine
        .project_create(&json!({ "name": "p" }))
        .expect("init p");
    engine
        .task_add(&json!({
            "title": "t",
            "project": "p",
            "priority": "H",
            "tags": ["api"],
        }))
        .expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let body = tool_json(&call(&server, 1, "tasqx_list_tasks", json!({})));
    let row = &body["tasks"][0];

    let mut expected: Vec<String> = DEFAULT_ROW_FIELDS
        .iter()
        .filter(|f| **f != "due")
        .map(|f| (*f).to_string())
        .collect();
    expected.sort();
    assert_eq!(
        row_keys(row),
        expected,
        "a default row is the nine minus the ones that are null: {row}"
    );
    let bytes = serde_json::to_string(row).expect("serialize").len();
    assert!(
        bytes < 150,
        "the default row is what every unprojected call pays for: {bytes} bytes for {row}"
    );
}

/// The same rule at its extreme: a task with nothing but a title spells
/// nothing but what it has.
#[test]
fn a_default_row_omits_every_null_key() {
    let engine = engine();
    engine.task_add(&json!({ "title": "bare" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let body = tool_json(&call(&server, 1, "tasqx_list_tasks", json!({})));
    let row = &body["tasks"][0];
    for absent in ["priority", "due", "project"] {
        assert!(
            row.get(absent).is_none(),
            "an unset `{absent}` must not be spelled at all: {row}"
        );
    }
    assert_eq!(row["title"], json!("bare"), "what it does have is there");
    assert_eq!(row["tags"], json!([]), "an empty list is not a null");
}

/// An explicit `fields` reaches any column, default or not — and reaches it
/// exactly, with nothing added back.
#[test]
fn an_explicit_fields_list_reaches_a_non_default_column() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let body = tool_json(&call(
        &server,
        1,
        "tasqx_list_tasks",
        json!({ "fields": ["short_id", "modified", "_rev"] }),
    ));
    assert_eq!(
        row_keys(&body["tasks"][0]),
        vec![
            "_rev".to_string(),
            "modified".to_string(),
            "short_id".to_string()
        ],
        "the projection the caller asked for is the projection it gets"
    );
}

/// `fields: []` is how a caller asks THIS transport for the whole row.
///
/// The engine reads an empty projection as no restriction at all (#76.1), and
/// D152 forwards an explicit `fields` of any kind untouched — so what an MCP
/// client sees here is `task_list`'s own answer, nulls included. Without that,
/// narrowing the default row would have made the full row unreachable over
/// MCP, because omitting `fields` no longer means "everything".
#[test]
fn empty_fields_over_mcp_is_the_engines_own_full_row() {
    let engine = engine();
    engine
        .project_create(&json!({ "name": "p" }))
        .expect("init p");
    for i in 0..3 {
        engine
            .task_add(&json!({ "title": format!("t{i}"), "project": "p" }))
            .expect("add");
    }
    let server = McpServer::new(&engine, Scope::Write);
    let over_mcp = tool_json(&call(
        &server,
        1,
        "tasqx_list_tasks",
        json!({ "fields": [], "limit": 3 }),
    ));
    let direct = engine
        .task_list(&json!({ "fields": [], "limit": 3 }))
        .expect("task_list");
    assert_eq!(
        over_mcp, direct,
        "an explicit projection is forwarded, not rewritten"
    );
    assert!(
        over_mcp["tasks"][0].get("due").is_some_and(Value::is_null),
        "and its nulls survive: {}",
        over_mcp["tasks"][0]
    );
}

/// A `null` `fields` counts as absent, not as a projection — the D32 reading
/// this transport already applies to `view`, `client` and `max_body_bytes`,
/// for the client that serializes unset optionals as null.
#[test]
fn a_null_fields_gets_the_default_row() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let body = tool_json(&call(
        &server,
        1,
        "tasqx_list_tasks",
        json!({ "fields": Value::Null }),
    ));
    assert!(
        body["tasks"][0].get("created").is_none(),
        "a null projection must not be answered with the whole row: {}",
        body["tasks"][0]
    );
    assert_eq!(body["tasks"][0]["title"], json!("t"));
}

/// A whole `arguments: null` is "no arguments" too — the reading `check_params`
/// gives a null `params` — so it gets the same default row and the same
/// transport page as `{}`. Left unnormalized, the null skipped the whole
/// `task.list` seam: the engine answered it (null is no params to it), but
/// with the full row and no page, past every default this transport supplies
/// (PR #38 review).
#[test]
fn null_arguments_get_the_default_row_and_the_transport_page() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let body = tool_json(&call(&server, 1, "tasqx_list_tasks", Value::Null));
    assert!(
        body["tasks"][0].get("created").is_none(),
        "null arguments must not be answered with the whole row: {}",
        body["tasks"][0]
    );
    assert_eq!(body["tasks"][0]["short_id"], json!(1));
    let with_empty = tool_json(&call(&server, 2, "tasqx_list_tasks", json!({})));
    assert_eq!(body, with_empty, "null and {{}} must be the same call");
}

// ---- D72: the bytes the caller already holds --------------------------------

/// `include_json: false` returns the rendered view alone — spelled out, and as
/// the default D151 made it.
///
/// D49 ships the result twice — once formatted, once as escaped JSON. Measured
/// on a live task with ONE annotation, the JSON block was 54% of a 6,375-byte
/// response and 66% of a 1,351-byte one read with `annotations_limit: 0`, so
/// since D151 it is sent only when asked for. Not "at any size" any more: the
/// view-only answer goes through the same budget as the two-block one, which
/// `a_view_only_answer_is_still_bounded` pins.
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

    let both = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": true }),
    );
    let blocks = both["result"]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 2, "a caller that asks for both gets both");

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

// ---- D151: the view alone is what a caller who says nothing gets ------------

/// A `tasqx_get_task` call that names no `include_json` gets ONE block, and it
/// is the rendered view — the JSON is there for the asking and not before.
///
/// Measured over 36 hours of transcripts, `tasqx_get_task` and
/// `tasqx_brief_task` returned 150 KB of the 373 KB tasqx sent back, every
/// call made by a model, which reads the view. D72's opt-out was passed in
/// none of the 42 calls, so D151 moved the default to where the observed
/// caller already was.
#[test]
fn the_default_answer_is_the_view_alone() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "read me" }))
        .expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "one short note" }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    assert!(!is_error(&out));
    let blocks = out["result"]["content"].as_array().expect("blocks");
    assert_eq!(
        blocks.len(),
        1,
        "the default answer is one block: {blocks:?}"
    );
    let view = blocks[0]["text"].as_str().expect("the view");
    assert!(view.starts_with("## #"), "and it is the view:\n{view}");
    // Nothing was dropped that the caller asked for, so nothing is explained.
    assert!(
        !view.contains("Machine-readable JSON omitted"),
        "a chosen answer is not an omission:\n{view}"
    );

    let asked = call(
        &server,
        2,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": true }),
    );
    let blocks = asked["result"]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 2, "asked for, the machine block is sent");
    assert_eq!(
        tool_json(&asked)["short_id"],
        json!(1),
        "and it is the same result, parseable"
    );
}

/// The same rule one tool over: a brief answers the view alone, and its JSON
/// half — the larger of the two, since it restates the neighbourhood and every
/// memory snippet — is opt-in.
#[test]
fn the_default_brief_is_the_view_alone() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "brief me" }))
        .expect("add");
    engine
        .annotation_add(&json!({ "ref": 1, "body": "one short note" }))
        .expect("annotate");
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(&server, 1, "tasqx_brief_task", json!({ "ref": 1 }));
    assert!(!is_error(&out));
    let blocks = out["result"]["content"].as_array().expect("blocks");
    assert_eq!(
        blocks.len(),
        1,
        "the default brief is one block: {blocks:?}"
    );
    let view = blocks[0]["text"].as_str().expect("the view");
    assert!(view.starts_with("## #"), "and it is the view:\n{view}");
    assert!(
        !view.contains("Machine-readable JSON omitted"),
        "nothing asked for was dropped:\n{view}"
    );

    let asked = call(
        &server,
        2,
        "tasqx_brief_task",
        json!({ "ref": 1, "include_json": true }),
    );
    assert_eq!(
        asked["result"]["content"].as_array().expect("blocks").len(),
        2,
        "asked for, the machine block is sent"
    );
    assert_eq!(tool_json(&asked)["task"]["short_id"], json!(1));
}

/// The view-only answer is BOUNDED, which is the half of D72 that D151 had to
/// take back.
///
/// `include_json: false` used to return before the budget ran — "the view, at
/// any size" — bounded only by D148's per-body cap. As an opt-in that was
/// survivable; as the default it would hand a client a twenty-annotation page
/// of 16 KB bodies, which is 320 KB and the exact failure D148 exists to
/// remove. So the bisection runs over the view alone, and the view's own
/// heading is what says which page it holds.
#[test]
fn a_view_only_answer_is_still_bounded() {
    let engine = engine();
    engine
        .task_add(&json!({ "title": "eleven very long notes" }))
        .expect("add");
    // ~6 KB each, each one well under the body cap: nothing here is truncated,
    // so the only lever is the page.
    let body = "detail ".repeat(900);
    for i in 0..11 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("## Note {i}\n\n{body}\n") }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(&server, 1, "tasqx_get_task", json!({ "ref": 1 }));
    assert!(!is_error(&out));
    let blocks = out["result"]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1, "one block, as the default promises");
    let sent = serde_json::to_string(&out).expect("json").len();
    assert!(
        sent < 24_576,
        "the view-only answer is {sent} bytes: the default may not be the unbounded path"
    );
    let view = blocks[0]["text"].as_str().expect("the view");
    assert!(
        view.contains("of 11") && view.contains("annotations_offset"),
        "the view's own heading is the whole notice: what it holds, and how to read the \
         rest:\n{view}"
    );
    assert!(
        !view.contains("Machine-readable JSON omitted"),
        "and it explains no omission, because the caller chose this answer:\n{view}"
    );

    // Saying it out loud is the same answer as saying nothing.
    let spelled = call(
        &server,
        2,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": false }),
    );
    assert_eq!(
        out["result"]["content"], spelled["result"]["content"],
        "`include_json: false` is the default spelled out, not a second behaviour"
    );
}

/// The brief's view-only answer is bounded the same way, over its own lever:
/// the memory page (D136/D66), never the task half or the neighbourhood.
///
/// The fixture makes MEMORY the overflow on purpose — ten long-titled rulings
/// the derived query finds — because the brief's bisection has no other lever,
/// and before D151 a default call returned every byte of them without asking
/// the budget anything.
#[test]
fn a_view_only_brief_is_still_bounded() {
    let engine = engine();
    // Each ruling's own title is what the section spends its bytes on: the
    // snippet beside it is a dozen tokens, so a page of ten is the overflow.
    let padding = "retention ".repeat(300);
    for i in 0..12 {
        engine
            .memory_add(&json!({
                "title": format!("kafka retention ruling {i} — {padding}"),
                "body": "kafka retention is seven days, and the compaction runs nightly"
            }))
            .expect("doc");
    }
    engine
        .task_add(&json!({ "title": "kafka retention" }))
        .expect("add");
    let server = McpServer::new(&engine, Scope::Read);

    let out = call(&server, 1, "tasqx_brief_task", json!({ "ref": 1 }));
    assert!(!is_error(&out), "{out}");
    let blocks = out["result"]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1, "one block, as the default promises");
    let view = blocks[0]["text"].as_str().expect("the view");
    assert!(
        view.len() <= 24_576,
        "the view-only brief is {} bytes: the memory page is the lever and it was not pulled",
        view.len()
    );
    let shown = view.matches("kafka retention ruling").count();
    assert!(
        (1..10).contains(&shown),
        "expected the memory page cut to what fits, got {shown} rulings"
    );
    assert!(
        !view.contains("Machine-readable JSON omitted"),
        "and no notice: nothing the caller asked for was dropped:\n{}",
        &view[..view.len().min(400)]
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

/// The omission notice may not say a thing the server no longer does.
///
/// It used to read: naming `annotations_limit` "returns both blocks
/// **unbounded** — that is an opt-out of the budget, not a page within it".
/// D148 removed that exemption, so the sentence became an instruction to make
/// a call that no longer exists, printed on the response that had just been
/// cut to fit. What the notice owes the reader now is what IS true: they asked
/// for both blocks, the two together were too big, the view carries the same
/// annotations and its own heading says how much of the history it holds, and
/// an oversized body is cut with a marker naming `max_body_bytes`. Since D151
/// the notice is only ever produced on that path — the view alone is the
/// default and explains nothing, because nothing asked for was dropped — so
/// this call names `include_json: true`.
#[test]
fn the_json_omission_notice_states_the_rule_the_server_now_follows() {
    let engine = engine();
    engine.task_add(&json!({ "title": "long" })).expect("add");
    for i in 0..12 {
        engine
            .annotation_add(&json!({ "ref": 1, "body": format!("{} #{i}", "z".repeat(3000)) }))
            .expect("annotate");
    }
    let server = McpServer::new(&engine, Scope::Write);
    let result = call(
        &server,
        1,
        "tasqx_get_task",
        json!({ "ref": 1, "include_json": true }),
    );
    let blocks = result["result"]["content"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1, "the premise: this response is over budget");
    let text = blocks[0]["text"].as_str().expect("text");
    let tail = &text[text.len().saturating_sub(600)..];
    assert!(
        !text.contains("unbounded"),
        "no answer is unbounded any more: {tail}"
    );
    assert!(
        text.contains("include_json"),
        "the notice still names the way to spend the budget on history: {tail}"
    );
    assert!(
        text.contains("max_body_bytes"),
        "and the argument that governs one oversized body: {tail}"
    );

    // And the claim it no longer makes is false: naming the whole history is
    // answered inside the same budget, not outside it.
    let named = call(
        &server,
        2,
        "tasqx_get_task",
        json!({ "ref": 1, "annotations_limit": 12 }),
    );
    let big = serde_json::to_string(&named).expect("json").len();
    assert!(
        big < 24_576,
        "an explicit limit is bounded like every other answer: {big} bytes"
    );
}

// ---- structured errors carry code, message and data (audit #175) ------------

/// An MCP error result must carry the same `code`/`message`/`data` a
/// `tasqx api` caller gets, as `structuredContent`, not only as a substring of
/// the text block's `error [code]: message` prose.
///
/// Before this fix `data` (here, the project name a caller would need to
/// retry usefully) was dropped entirely on the way to an MCP client, and
/// `code` survived only as an undocumented regex target.
#[test]
fn an_mcp_error_carries_structured_code_message_and_data() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let resp = call(
        &server,
        1,
        "tasqx_add_task",
        json!({ "title": "x", "project": "no-such-project" }),
    );
    assert!(is_error(&resp), "an unknown project must refuse");
    let structured = &resp["result"]["structuredContent"];
    assert_eq!(
        structured["error"]["code"], "not_found",
        "structuredContent.error.code must carry the machine-readable code: {resp}"
    );
    assert_eq!(
        structured["error"]["data"]["name"], "no-such-project",
        "structuredContent.error.data must survive the trip over MCP: {resp}"
    );
    assert!(
        structured["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "structuredContent.error.message must not be empty: {resp}"
    );
    // The text block is unchanged — this is additive, not a replacement.
    let text = resp["result"]["content"][0]["text"].as_str().expect("text");
    assert!(text.starts_with("error [not_found]:"), "got: {text}");
}

// ---- MCP-surface error remedies name a tool an agent can call (audit #225.3) -

/// A `task.modify` refusal that tells the caller to use `task.start/stop/done`
/// is correct advice over `tasqx api` and wrong advice over MCP, where those
/// are not callable names. The MCP presentation must rewrite it to the tools
/// that actually exist on this surface.
#[test]
fn an_mcp_transition_refusal_names_mcp_tools_not_api_methods() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let resp = call(
        &server,
        1,
        "tasqx_modify_task",
        json!({ "ref": 1, "set": { "status": "pending" } }),
    );
    assert!(is_error(&resp));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("task.start/stop/done"),
        "the refusal still names JSON-API method names an MCP agent cannot call: {text}"
    );
    assert!(
        text.contains("tasqx_start_timer")
            && text.contains("tasqx_stop_timer")
            && text.contains("tasqx_complete_task"),
        "the refusal should name the real MCP tools: {text}"
    );
}

/// `require_live_project`'s refusal names `tasqx init NAME` — a CLI verb with
/// no MCP equivalent and no shell to run it in. Over MCP it must name the
/// tool that actually creates a project.
#[test]
fn an_mcp_missing_project_refusal_names_the_create_project_tool() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let resp = call(
        &server,
        1,
        "tasqx_add_task",
        json!({ "title": "x", "project": "no-such-project" }),
    );
    assert!(is_error(&resp));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("tasqx init"),
        "the refusal still names a CLI verb with no MCP equivalent: {text}"
    );
    assert!(
        text.contains("tasqx_create_project"),
        "the refusal should name the MCP tool that creates a project: {text}"
    );
}

// ---- the read-only refusal names the fix (audit #225.11) ---------------------

#[test]
fn the_read_only_refusal_names_the_flag_that_fixes_it() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Read);
    let resp = call(&server, 1, "tasqx_add_task", json!({ "title": "nope" }));
    assert!(is_error(&resp));
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("tasqx mcp serve --scope write"),
        "the refusal should name the exact fix, since a retry or a workaround \
         cannot succeed on this surface: {text}"
    );
}

// ---- the roster's own size is a per-prompt cost (D155) ----------------------

/// `tools/list` is paid on EVERY prompt by a client that does not defer tool
/// schemas — Codex, Gemini and most others inject the whole block — so the
/// roster's serialized size is a budget, not a detail.
///
/// Measured 2026-09-16 with a `tools/list` handshake against this build:
/// 29 tools, **30,897 bytes** for `result.tools` serialized compact, the
/// largest single entry 2,679 bytes (`tasqx_complete_task`) and the largest
/// `description` 498 bytes. Before D155 the same measurement read 42,737
/// bytes, with descriptions up to 1,034 bytes.
///
/// The bounds are deliberately close to the measurement: they exist to catch
/// the essay creeping back, which is how the 42 KB accumulated in the first
/// place. Re-measure with the pipeline in D155 before raising one, and raise
/// it because a tool was ADDED, not because a description grew.
///
/// The floor is not zero. With every `description` key removed from the roster
/// the same serialization is 11,597 bytes of schema skeleton — property names,
/// `type`, the closed `enum` lists D30 renders from the engine's own consts,
/// and the MCP `annotations` hints — none of which is prose that can be cut.
/// That is why the per-entry bound is thousands and not hundreds: a tool with
/// nine parameters costs ~1 KB before it says anything at all.
#[test]
fn the_whole_tool_roster_stays_inside_its_per_prompt_budget() {
    const MAX_DESCRIPTION: usize = 800;
    const MAX_ENTRY: usize = 3_072;
    const MAX_ROSTER: usize = 31_744;

    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");

    for tool in tools {
        let name = tool["name"].as_str().expect("every tool is named");
        let described = tool["description"]
            .as_str()
            .expect("every tool carries a description")
            .len();
        assert!(
            described <= MAX_DESCRIPTION,
            "`{name}`'s description is {described} bytes, over the {MAX_DESCRIPTION}-byte \
             cap (D155). A description states the contract and cites the D-number; the \
             reasoning behind the rule belongs in DESIGN.md §12."
        );
        let entry = serde_json::to_string(tool)
            .expect("a tool entry serializes")
            .len();
        assert!(
            entry <= MAX_ENTRY,
            "`{name}` serializes to {entry} bytes, over the {MAX_ENTRY}-byte per-tool cap \
             (D155). Its parameter descriptions are the half of that cost that is prose."
        );
    }

    let roster = serde_json::to_string(tools)
        .expect("the roster serializes")
        .len();
    assert!(
        roster <= MAX_ROSTER,
        "`result.tools` serializes to {roster} bytes, over the {MAX_ROSTER}-byte roster cap \
         (D155) — about {} tokens on every prompt of every client that does not defer tool \
         schemas.",
        roster / 4
    );
}

// ---- schema descriptions carry facts the audit found missing ----------------

/// `tasqx_summary`'s `metrics` param was the only property on the whole
/// surface with no `description` at all, and it is the one param that
/// controls whether a summary is a bare headcount or the full report (audit
/// #189).
#[test]
fn summary_metrics_schema_names_its_own_default() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let summary = tools
        .iter()
        .find(|t| t["name"] == "tasqx_summary")
        .expect("tasqx_summary is listed");
    let desc = summary["inputSchema"]["properties"]["metrics"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !desc.is_empty(),
        "`metrics` must document that it is opt-in, since omitting it silently drops every \
         metric but `count`"
    );
}

/// `memory.search`'s `rank` is FTS5's raw bm25 score, where LOWER is BETTER —
/// the opposite of most scoring conventions — and nothing said so (audit
/// #225.12).
#[test]
fn search_memory_description_names_the_rank_direction() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let search = tools
        .iter()
        .find(|t| t["name"] == "tasqx_search_memory")
        .expect("tasqx_search_memory is listed");
    let desc = search["description"].as_str().unwrap_or_default();
    assert!(
        desc.to_lowercase().contains("lower") || desc.to_lowercase().contains("negative"),
        "the description must say which direction of `rank` is better: {desc}"
    );
}

/// The three tools that read or write the `standing` flag (D156) must
/// advertise it with a real description, not a bare boolean.
#[test]
fn memory_tools_document_the_standing_flag() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");

    for name in [
        "tasqx_add_memory",
        "tasqx_update_memory",
        "tasqx_list_memory",
    ] {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        let standing = &tool["inputSchema"]["properties"]["standing"];
        assert_eq!(
            standing["type"],
            json!("boolean"),
            "`{name}`'s `standing` param must be a boolean: {standing}"
        );
        assert!(
            !standing["description"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "`{name}`'s `standing` param must carry a description: {standing}"
        );
    }
}

/// The two parameters that together decide how much history a page buys —
/// `annotations_limit` and `include_json` — must cross-reference each other,
/// since the one that leads a caller into the trap never used to mention the
/// one that governs the other half of the spend (audit #225.13).
///
/// The direction flipped with D151: `include_json: false` used to be the
/// escape from a duplicate the caller got by default, and now the duplicate is
/// what `include_json: true` buys at the history's expense. Either way the
/// page-size description has to name it, which is what this asserts.
#[test]
fn annotations_limit_description_names_include_json_as_the_other_spend() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let get_task = tools
        .iter()
        .find(|t| t["name"] == "tasqx_get_task")
        .expect("tasqx_get_task is listed");
    let desc = get_task["inputSchema"]["properties"]["annotations_limit"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        desc.contains("include_json"),
        "`annotations_limit`'s description should name `include_json` as the other claim on \
         the same budget: {desc}"
    );
}

/// `tasqx_create_project` gives the agent nothing to act on: the new project
/// is not the default and there is no MCP tool to change that. The
/// description must say so rather than leave the agent to discover it by a
/// failed `tasqx_add_task` (audit #225's item 2).
#[test]
fn create_project_description_says_it_is_never_the_default() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let create = tools
        .iter()
        .find(|t| t["name"] == "tasqx_create_project")
        .expect("tasqx_create_project is listed");
    let desc = create["description"].as_str().unwrap_or_default();
    assert!(
        desc.to_lowercase().contains("default") && desc.contains("project"),
        "the description should say the new project does not become the default: {desc}"
    );
}

// ---- annotation.add's echo is opt-out, not gone (audit #172; challenges D72) -

/// The default is unchanged: a caller that says nothing still gets the body
/// echoed back, verbatim-storage proof intact (D72/D75).
#[test]
fn annotate_still_echoes_the_body_by_default() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let resp = call(
        &server,
        1,
        "tasqx_annotate_task",
        json!({ "ref": 1, "body": "hello world" }),
    );
    assert!(!is_error(&resp));
    let json = tool_text(&resp);
    assert_eq!(json["annotation"]["body"], "hello world");
    assert!(json["annotation"].get("body_bytes").is_none());
}

/// `include_body: false` drops the echoed body and reports its length
/// instead, so a long note does not cost its own bytes twice with no way to
/// decline. The stored annotation is untouched — a follow-up read gets the
/// body back whole.
#[test]
fn annotate_include_body_false_reports_a_length_instead_of_the_bytes() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    let long_body = "y".repeat(5000);
    let resp = call(
        &server,
        1,
        "tasqx_annotate_task",
        json!({ "ref": 1, "body": long_body.clone(), "include_body": false }),
    );
    assert!(!is_error(&resp));
    let json = tool_text(&resp);
    assert!(
        json["annotation"].get("body").is_none(),
        "body must not be echoed when declined: {json}"
    );
    assert_eq!(json["annotation"]["body_bytes"], long_body.len());

    // The response is genuinely smaller — this is the point.
    let with_body = call(
        &server,
        2,
        "tasqx_annotate_task",
        json!({ "ref": 1, "body": long_body.clone() }),
    );
    let small = serde_json::to_string(&resp).expect("json").len();
    let big = serde_json::to_string(&with_body).expect("json").len();
    assert!(
        small < big,
        "declining the echo must actually shrink the response: {small} vs {big}"
    );

    // Nothing was lost in the store: the body is still there, whole.
    let got = engine
        .task_get(&json!({ "ref": 1, "annotations_limit": 2 }))
        .expect("get");
    let bodies: Vec<&str> = got["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["body"].as_str().unwrap())
        .collect();
    assert!(bodies.contains(&long_body.as_str()));
}

/// `include_body` must never reach the params gate: `annotation.add` does not
/// accept it as a method param, so a forwarded copy would be an instant
/// `bad_request` on every call that names it.
#[test]
fn include_body_is_stripped_before_the_params_gate() {
    let engine = engine();
    engine.task_add(&json!({ "title": "t" })).expect("add");
    let server = McpServer::new(&engine, Scope::Write);
    for args in [
        json!({ "ref": 1, "body": "a", "include_body": false }),
        json!({ "ref": 1, "body": "b", "include_body": true }),
    ] {
        let resp = call(&server, 1, "tasqx_annotate_task", args.clone());
        assert!(!is_error(&resp), "`{args}` was refused: {resp}");
    }
}

// ---- D154: the three memory reads, slimmed ----------------------------------

/// D154: a search hit carries no `rank` unless the caller asks for it.
///
/// `rank` is the raw FTS5 bm25 float, seventeen characters of
/// `-1.2345678901234567` per hit, and `hits` is already sorted best-first — so
/// the number answers a question nobody asked. The JSON API still freezes it
/// (D56, `MEMORY_HIT_ROW`); this is the transport declining to send it, the way
/// D152 narrows a `task.list` row.
#[test]
fn a_search_hit_carries_no_rank_unless_asked_for() {
    let engine = engine();
    engine
        .memory_add(&json!({ "title": "envelope rules", "body": "every envelope carries an id" }))
        .expect("doc");
    let server = McpServer::new(&engine, Scope::Read);

    let bare = tool_json(&call(
        &server,
        1,
        "tasqx_search_memory",
        json!({ "query": "envelope" }),
    ));
    let hit = &bare["hits"][0];
    assert_eq!(
        hit["title"],
        json!("envelope rules"),
        "a hit came back: {bare}"
    );
    assert!(
        hit.get("rank").is_none(),
        "the default hit spends nothing on bm25: {hit}"
    );

    let asked = tool_json(&call(
        &server,
        2,
        "tasqx_search_memory",
        json!({ "query": "envelope", "include_rank": true }),
    ));
    assert!(
        asked["hits"][0]["rank"].is_number(),
        "asked for, the score is the engine's own: {}",
        asked["hits"][0]
    );
}

/// The same rule on the brief's memory half, measured where it is actually
/// sent: inside the machine block `include_json: true` buys.
#[test]
fn a_brief_memory_hit_carries_no_rank_unless_asked_for() {
    let engine = engine();
    engine
        .memory_add(&json!({ "title": "envelope rules", "body": "every envelope carries an id" }))
        .expect("doc");
    engine
        .task_add(&json!({ "title": "envelope rules" }))
        .expect("task");
    let server = McpServer::new(&engine, Scope::Read);

    let bare = tool_json(&call(
        &server,
        1,
        "tasqx_brief_task",
        json!({ "ref": 1, "include_json": true }),
    ));
    let hit = &bare["memory"]["hits"][0];
    assert_eq!(
        hit["title"],
        json!("envelope rules"),
        "a hit came back: {bare}"
    );
    assert!(
        hit.get("rank").is_none(),
        "the brief's JSON block is the stripped one: {hit}"
    );

    let asked = tool_json(&call(
        &server,
        2,
        "tasqx_brief_task",
        json!({ "ref": 1, "include_json": true, "include_rank": true }),
    ));
    assert!(
        asked["memory"]["hits"][0]["rank"].is_number(),
        "asked for, the score is the engine's own: {}",
        asked["memory"]["hits"][0]
    );
}

/// Eight docs every one of which matches the task's own title, so the page size
/// is the only thing deciding how many come back.
fn task_with_eight_matching_docs(engine: &Engine) {
    for n in 0..8 {
        engine
            .memory_add(&json!({
                "title": format!("envelope rules {n}"),
                "body": "the envelope rules say every envelope carries an id",
            }))
            .expect("doc");
    }
    engine
        .task_add(&json!({ "title": "envelope rules" }))
        .expect("task");
}

/// D154: the brief's memory page defaults to five hits, and an explicit
/// `memory_limit` is still answered exactly as asked.
#[test]
fn the_default_brief_memory_page_is_five_hits() {
    let engine = engine();
    task_with_eight_matching_docs(&engine);
    let server = McpServer::new(&engine, Scope::Read);

    let memory = |id: i64, extra: Value| -> Value {
        let mut args = json!({ "ref": 1, "include_json": true });
        for (k, v) in extra.as_object().expect("an object") {
            args[k.clone()] = v.clone();
        }
        tool_json(&call(&server, id, "tasqx_brief_task", args))["memory"].clone()
    };

    let default = memory(1, json!({}));
    assert_eq!(default["count"], json!(5), "the default page: {default}");
    assert_eq!(default["hits"].as_array().expect("hits").len(), 5);

    let eight = memory(2, json!({ "memory_limit": 8 }));
    assert_eq!(
        eight["count"],
        json!(8),
        "an explicit page is honoured: {eight}"
    );

    let none = memory(3, json!({ "memory_limit": 0 }));
    assert_eq!(none["count"], json!(0), "including zero: {none}");
    assert_eq!(none["hits"].as_array().expect("hits").len(), 0);
}

fn twenty_five_docs(engine: &Engine) {
    for n in 0..25 {
        engine
            .memory_add(&json!({
                "title": format!("doc {n}"),
                "body": "a body long enough to have a preview cut out of it",
                "source": format!("docs/{n}.md"),
            }))
            .expect("doc");
    }
}

/// D154: `tasqx_list_memory` with no `limit` is twenty compact rows, not every
/// doc in the store with a 160-character preview on each.
#[test]
fn an_unpaged_list_memory_call_is_twenty_narrow_rows() {
    let engine = engine();
    twenty_five_docs(&engine);
    let server = McpServer::new(&engine, Scope::Read);

    let body = tool_json(&call(&server, 1, "tasqx_list_memory", json!({})));
    assert_eq!(body["count"], json!(20), "the transport's page: {body}");
    assert_eq!(body["total"], json!(25));
    assert_eq!(body["next_offset"], json!(20), "and the walk is still open");
    for row in body["docs"].as_array().expect("docs") {
        assert_eq!(
            row_keys(row),
            vec!["id", "modified", "source", "title"],
            "a browse row is what identifies a doc, not its body: {row}"
        );
    }
}

/// The narrowing is about the ROW and the page is about how many, so a caller
/// that names its own `limit` still gets the compact row.
#[test]
fn an_explicit_list_memory_limit_is_still_narrowed() {
    let engine = engine();
    twenty_five_docs(&engine);
    let server = McpServer::new(&engine, Scope::Read);

    let body = tool_json(&call(
        &server,
        1,
        "tasqx_list_memory",
        json!({ "limit": 3 }),
    ));
    assert_eq!(body["count"], json!(3), "asked for three: {body}");
    assert_eq!(
        row_keys(&body["docs"][0]),
        vec!["id", "modified", "source", "title"],
        "still the compact row: {body}"
    );
}

/// `include_preview: true` is the engine's own row back, preview and all.
#[test]
fn list_memory_include_preview_is_the_engines_own_row() {
    let engine = engine();
    twenty_five_docs(&engine);
    let server = McpServer::new(&engine, Scope::Read);

    let body = tool_json(&call(
        &server,
        1,
        "tasqx_list_memory",
        json!({ "limit": 1, "include_preview": true }),
    ));
    let row = &body["docs"][0];
    assert_eq!(
        row_keys(row),
        vec![
            "_rev",
            "body_preview",
            "body_truncated",
            "created",
            "id",
            "modified",
            "project",
            "source",
            "standing",
            "title"
        ],
        "the ten frozen keys, untouched: {row}"
    );
}

/// `limit: 0` is a real page of nothing, not an omitted limit in disguise, and
/// the narrowing must not trip over an empty page.
#[test]
fn list_memory_limit_zero_returns_no_rows() {
    let engine = engine();
    twenty_five_docs(&engine);
    let server = McpServer::new(&engine, Scope::Read);

    let body = tool_json(&call(
        &server,
        1,
        "tasqx_list_memory",
        json!({ "limit": 0 }),
    ));
    assert_eq!(body["count"], json!(0), "zero means zero: {body}");
    assert_eq!(body["docs"].as_array().expect("docs").len(), 0);
    assert_eq!(body["total"], json!(25), "and the store is still counted");
}

/// Neither transport-only argument may reach the params gate: `memory.search`,
/// `task.brief` and `memory.list` refuse an unknown key, so a forwarded copy is
/// a `bad_request` on every call that names one.
#[test]
fn the_memory_transport_arguments_are_stripped_before_the_params_gate() {
    let engine = engine();
    twenty_five_docs(&engine);
    engine.task_add(&json!({ "title": "doc 1" })).expect("task");
    let server = McpServer::new(&engine, Scope::Read);
    for (tool, args) in [
        (
            "tasqx_search_memory",
            json!({ "query": "doc", "include_rank": false }),
        ),
        (
            "tasqx_search_memory",
            json!({ "query": "doc", "include_rank": true }),
        ),
        (
            "tasqx_brief_task",
            json!({ "ref": 1, "include_rank": true }),
        ),
        ("tasqx_list_memory", json!({ "include_preview": false })),
        ("tasqx_list_memory", json!({ "include_preview": true })),
    ] {
        let resp = call(&server, 1, tool, args.clone());
        assert!(!is_error(&resp), "`{tool}` refused `{args}`: {resp}");
    }
}

/// The brief's `include_rank` is only observable in the machine-readable
/// block: the rendered view never prints a rank, and that block is what
/// `include_json: true` buys (D151). A caller who passes the one without the
/// other gets an answer identical to the default, so the argument's own
/// description has to name the flag that makes it visible (PR #42 review).
#[test]
fn brief_include_rank_description_names_include_json_as_where_it_shows() {
    let engine = engine();
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request");
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let brief = tools
        .iter()
        .find(|t| t["name"] == "tasqx_brief_task")
        .expect("tasqx_brief_task is listed");
    let desc = brief["inputSchema"]["properties"]["include_rank"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        desc.contains("include_json"),
        "`include_rank`'s description on the brief should name `include_json` as the block \
         the rank appears in: {desc}"
    );
}
