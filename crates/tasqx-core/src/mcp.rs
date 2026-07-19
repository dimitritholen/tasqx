//! Bundled MCP (Model Context Protocol) server — a thin client of the Engine
//! (DESIGN.md §7, §12-D7).
//!
//! This layer holds **no** task logic. It (a) parses one JSON-RPC 2.0 message,
//! (b) maps a tool name + arguments onto an existing core dispatch method,
//! (c) calls the in-process [`dispatch`] table, and (d) wraps the response as an
//! MCP `tools/call` result. The AI surface is `tasqx api` with a schema — the
//! same [`Engine`] and the same envelopes a human `tasqx` invocation runs.
//!
//! The heart is [`McpServer::handle_message`]: a pure function taking a JSON-RPC
//! message [`Value`] and returning an optional response [`Value`] (`None` for
//! notifications). A tiny stdio loop in the CLI wraps it for real transport;
//! tests drive it directly, no piping required.
//!
//! Transport (per the MCP stdio spec): newline-delimited JSON, one JSON object
//! per line, on stdin/stdout. Logs go to stderr only.

use serde_json::{json, Value};

use crate::dispatch::dispatch;
use crate::engine::{Engine, SORT_KEYS, SUMMARY_GROUP_BY, SUMMARY_METRICS};
use crate::types::Priority;

/// MCP protocol revision this server implements by default (the `initialize`
/// handshake reports it when it cannot honor the client's request). Kept
/// current with the stable spec.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Protocol revisions this server can speak. On `initialize` the server echoes
/// the client's requested `protocolVersion` when it appears here (negotiation),
/// otherwise it falls back to [`PROTOCOL_VERSION`]. Newest first.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Server identity reported in `initialize` → `serverInfo.name`.
pub const SERVER_NAME: &str = "tasqx";

/// The capability scope a server instance runs under. This is the entire
/// auth model (D7): a token just selects a scope, and `Read` rejects the write
/// tools — no AI-specific enforcement path, just the plugin capability fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Read-only: the four `tasqx_list_*`/`get`/`summary` tools. Write tools
    /// are refused with an `isError` result.
    Read,
    /// Full access: every tool.
    Write,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
        }
    }

    /// Whether this scope may invoke a write (destructive) tool.
    pub fn allows_write(self) -> bool {
        matches!(self, Scope::Write)
    }

    /// Mint an opaque scoped token. Format: `tasqx_mcp_<scope>_<uuid>`. The
    /// scope is the only thing it encodes (D7 keeps this deliberately simple).
    pub fn mint_token(self) -> String {
        format!("tasqx_mcp_{}_{}", self.as_str(), uuid::Uuid::now_v7())
    }

    /// Recover the scope from a token minted by [`mint_token`]. Returns `None`
    /// for anything that is not a well-formed tasqx MCP token.
    pub fn from_token(token: &str) -> Option<Scope> {
        let rest = token.strip_prefix("tasqx_mcp_")?;
        if rest.starts_with("read_") {
            Some(Scope::Read)
        } else if rest.starts_with("write_") {
            Some(Scope::Write)
        } else {
            None
        }
    }
}

/// One exposed MCP tool: its name, the core method it maps onto 1:1, whether it
/// is a write (destructive) tool, a model-facing description, and its
/// JSON-Schema `inputSchema`.
struct ToolSpec {
    name: &'static str,
    method: &'static str,
    write: bool,
    description: &'static str,
    schema: Value,
}

/// Render a Rust list of accepted string values as a JSON-Schema `enum` array.
///
/// Every closed value set in these schemas goes through here rather than being
/// retyped as a JSON literal. The schemas are the *only* thing an agent sees
/// before choosing an argument: a value the engine accepts but the schema omits
/// is an option the agent will never try, and a value the schema advertises but
/// the engine rejects is a call that always fails. Both used to be possible with
/// nothing going red, because the JSON enums were hand-copies of Rust lists that
/// nothing compared them against.
fn enum_of(values: impl IntoIterator<Item = &'static str>) -> Value {
    Value::Array(values.into_iter().map(|v| json!(v)).collect())
}

/// Schema fragment for a `ref` argument (short_id int OR full UUID string).
fn ref_schema() -> Value {
    json!({
        "type": ["integer", "string"],
        "description": "Task reference: short_id (integer) or full UUID (string)."
    })
}

/// The full §7 tool surface. Each entry maps 1:1 onto a core dispatch method;
/// the tool `arguments` object is passed straight through as the method params
/// (argument names are identical to the core param names by design).
fn tool_specs() -> Vec<ToolSpec> {
    vec![
        // ---- reads ----------------------------------------------------------
        ToolSpec {
            name: "tasqx_list_tasks",
            method: "task.list",
            write: false,
            description: "List tasks matching a filter-DSL query. The filter is \
                the same grammar the CLI takes, e.g. \
                \"project:work.tasqx status:pending +api due.before:tomorrow\".",
            schema: json!({
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "string",
                        "description": "Filter DSL query, e.g. \"status:pending +api\". Use \"@working\" for the active working set."
                    },
                    // No `enum` here: a key may carry a `-` prefix, which a
                    // plain enum of the bare names would forbid. The valid set
                    // is still stated once, rendered from SORT_KEYS, so an
                    // agent reads the same list the engine validates against
                    // instead of guessing and being refused.
                    "sort": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": format!(
                            "Sort keys, e.g. [\"-urgency\", \"due\"]. Prefix \"-\" for descending. \
                             Valid keys: {}. An unknown key is rejected, not ignored.",
                            SORT_KEYS.join(", ")
                        )
                    },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["filter"]
            }),
        },
        ToolSpec {
            name: "tasqx_get_task",
            method: "task.get",
            write: false,
            description: "Get one task's full detail: fields, tags, annotations, and dependencies.",
            schema: json!({
                "type": "object",
                "properties": { "ref": ref_schema() },
                "required": ["ref"]
            }),
        },
        ToolSpec {
            name: "tasqx_summary",
            method: "report.summary",
            write: false,
            description: "Aggregate report grouped by project, status, or priority. Pure read, no side effects.",
            schema: json!({
                "type": "object",
                "properties": {
                    "group_by": {
                        "type": "string",
                        "enum": enum_of(SUMMARY_GROUP_BY)
                    },
                    "filter": { "type": "string", "description": "Optional filter DSL to scope the report." },
                    "metrics": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": enum_of(SUMMARY_METRICS)
                        }
                    }
                },
                "required": ["group_by"]
            }),
        },
        ToolSpec {
            name: "tasqx_list_projects",
            method: "project.list",
            write: false,
            description: "List projects. By default excludes archived projects.",
            schema: json!({
                "type": "object",
                "properties": {
                    "include_archived": { "type": "boolean" }
                }
            }),
        },
        // ---- writes ---------------------------------------------------------
        ToolSpec {
            name: "tasqx_add_task",
            method: "task.add",
            write: true,
            description: "Create a new task. Returns its short_id and urgency.",
            schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "project": { "type": "string" },
                    "priority": {
                        "type": "string",
                        "enum": enum_of(Priority::ALL.map(Priority::as_str)),
                        "description": "Priority: H (high), M (medium), or L (low)."
                    },
                    "due": { "type": "string", "description": "Due date/time, RFC3339, e.g. \"2026-07-20T17:00:00+02:00\"." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "estimate": { "type": "string", "description": "ISO-8601 duration, e.g. \"PT4H\"." }
                },
                "required": ["title"]
            }),
        },
        ToolSpec {
            name: "tasqx_modify_task",
            method: "task.modify",
            write: true,
            description: "Change fields on a task via a `set` map. Pass expected_rev \
                for optimistic concurrency (a stale rev yields a conflict instead of clobbering).",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "set": {
                        "type": "object",
                        "description": "Field → new value, e.g. {\"priority\":\"M\",\"due\":\"2026-07-22T17:00:00+02:00\"}."
                    },
                    "expected_rev": { "type": "integer", "description": "Optional optimistic-concurrency guard." }
                },
                "required": ["ref", "set"]
            }),
        },
        ToolSpec {
            name: "tasqx_complete_task",
            method: "task.done",
            write: true,
            description: "Mark a task done. Returns any tasks newly unblocked by its completion.",
            schema: json!({
                "type": "object",
                "properties": { "ref": ref_schema() },
                "required": ["ref"]
            }),
        },
        ToolSpec {
            name: "tasqx_start_timer",
            method: "task.start",
            write: true,
            description: "Start the timer on a task (moves it to active).",
            schema: json!({
                "type": "object",
                "properties": { "ref": ref_schema() },
                "required": ["ref"]
            }),
        },
        ToolSpec {
            name: "tasqx_stop_timer",
            method: "task.stop",
            write: true,
            description: "Stop the timer on a task. Returns the tracked duration.",
            schema: json!({
                "type": "object",
                "properties": { "ref": ref_schema() },
                "required": ["ref"]
            }),
        },
        ToolSpec {
            name: "tasqx_tag_task",
            method: "tag.add",
            write: true,
            description: "Add one or more tags to a task. Returns the resulting tag set.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["ref", "tags"]
            }),
        },
        ToolSpec {
            name: "tasqx_create_project",
            method: "project.create",
            write: true,
            description: "Create a project. Returns its id and name.",
            schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["name"]
            }),
        },
    ]
}

/// A long-lived MCP session over one [`Engine`], fenced to one [`Scope`]. It is
/// a pure message mapper — all state of record lives in the engine's store.
pub struct McpServer<'e> {
    engine: &'e Engine,
    scope: Scope,
}

impl<'e> McpServer<'e> {
    pub fn new(engine: &'e Engine, scope: Scope) -> Self {
        McpServer { engine, scope }
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Handle one JSON-RPC 2.0 message.
    ///
    /// Returns `Some(response)` for a request (a message with an `id`) and
    /// `None` for a notification (no `id`, e.g. `notifications/initialized`).
    /// Never panics: protocol problems become JSON-RPC error objects and
    /// tool-level problems become `tools/call` results with `isError: true`.
    pub fn handle_message(&self, msg: &Value) -> Option<Value> {
        let has_id = msg.get("id").is_some();
        let method = msg.get("method").and_then(Value::as_str);

        // A message with no `id` member is a notification: we act on it (or
        // ignore it) and emit nothing, per JSON-RPC 2.0.
        if !has_id {
            return None;
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);

        let method = match method {
            Some(m) => m,
            None => return Some(rpc_error(id, -32600, "Invalid Request: missing method")),
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

        match method {
            "initialize" => Some(rpc_result(id, self.initialize_result(&params))),
            "ping" => Some(rpc_result(id, json!({}))),
            "tools/list" => Some(rpc_result(id, json!({ "tools": tools_list(self.scope) }))),
            "tools/call" => Some(rpc_result(id, self.tools_call(&params))),
            other => Some(rpc_error(id, -32601, format!("Method not found: {other}"))),
        }
    }

    fn initialize_result(&self, params: &Value) -> Value {
        // Negotiate: if the client asked for a revision we speak, echo it back;
        // otherwise report our own supported default (spec-compliant either way).
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = match requested {
            Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
            _ => PROTOCOL_VERSION,
        };
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": env!("CARGO_PKG_VERSION")
            }
        })
    }

    /// Execute a `tools/call`. Always returns a CallToolResult value (never a
    /// transport error): unknown tools, scope denials, and core `ApiError`s all
    /// surface as `isError: true` text results the model can read and recover from.
    fn tools_call(&self, params: &Value) -> Value {
        let name = match params.get("name").and_then(Value::as_str) {
            Some(n) => n,
            None => return tool_error("bad_request", "tools/call is missing the tool `name`"),
        };

        let specs = tool_specs();
        let spec = match specs.iter().find(|s| s.name == name) {
            Some(s) => s,
            None => return tool_error("not_found", format!("unknown tool: {name}")),
        };

        // Scope fence: a write tool under a read-only scope is refused here,
        // before the engine is ever touched (no mutation happens).
        if spec.write && !self.scope.allows_write() {
            return tool_error(
                "bad_request",
                format!(
                    "tool `{name}` requires write scope, but this MCP server is running read-only"
                ),
            );
        }

        let mut args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

        // Optimistic concurrency by default (DESIGN §7): for a modify the server
        // reads `_rev` first and pins it as `expected_rev`, so a task a human
        // edited in another shell yields a `conflict` instead of a silent
        // last-writer-wins clobber. A caller that pins its own `expected_rev`
        // (e.g. re-reading after a conflict) is respected as-is.
        if spec.method == "task.modify" {
            if let Some(obj) = args.as_object_mut() {
                if !obj.contains_key("expected_rev") {
                    if let Some(rev) = self.current_rev(obj.get("ref")) {
                        obj.insert("expected_rev".to_string(), json!(rev));
                    }
                }
            }
        }

        match dispatch(self.engine, spec.method, &args) {
            Ok(result) => tool_ok(&result),
            Err(e) => {
                let code = serde_json::to_value(e.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "internal".to_string());
                tool_error(&code, e.message)
            }
        }
    }

    /// Read a task's current `_rev` via `task.get` for the optimistic-concurrency
    /// guard. Returns `None` if the ref is missing or does not resolve — the
    /// subsequent `task.modify` then surfaces the real error (e.g. `not_found`).
    fn current_rev(&self, ref_val: Option<&Value>) -> Option<i64> {
        let ref_val = ref_val?;
        let got = dispatch(self.engine, "task.get", &json!({ "ref": ref_val })).ok()?;
        got.get("_rev").and_then(Value::as_i64)
    }
}

/// Serialize the tool registry into the `tools/list` array shape, including
/// read/write behavior hints so the host can apply its confirmation policy.
///
/// Scope-filtered: a read-only session advertises only the read tools, so a
/// read-only agent is never shown a write tool it would always be refused.
fn tools_list(scope: Scope) -> Vec<Value> {
    tool_specs()
        .into_iter()
        .filter(|s| !s.write || scope.allows_write())
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "inputSchema": s.schema,
                "annotations": {
                    "title": s.name,
                    "readOnlyHint": !s.write,
                    "destructiveHint": s.write,
                    "idempotentHint": false,
                    "openWorldHint": false
                }
            })
        })
        .collect()
}

/// A successful `tools/call` result: the core method's JSON result carried as
/// text content.
fn tool_ok(result: &Value) -> Value {
    json!({
        "content": [
            { "type": "text", "text": serde_json::to_string_pretty(result).unwrap_or_default() }
        ],
        "isError": false
    })
}

/// An error `tools/call` result (scope denial, unknown tool, or a core
/// `ApiError`): the code + message as text content, flagged `isError`.
fn tool_error(code: &str, message: impl Into<String>) -> Value {
    json!({
        "content": [
            { "type": "text", "text": format!("error [{code}]: {}", message.into()) }
        ],
        "isError": true
    })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the JSON-Schema `enum` array at `properties.<prop>[.items]` of a
    /// tool's `inputSchema`, as the strings an agent would actually see.
    ///
    /// The tests below deliberately go through the *published schema* rather
    /// than through the Rust constants it is built from. Asserting the schema
    /// equals the constant it is rendered from would be a tautology; asserting
    /// that every value the schema advertises is one the engine really honors
    /// is the property that was unguarded.
    fn schema_enum(tool: &str, prop: &str, in_items: bool) -> Vec<String> {
        let specs = tool_specs();
        let spec = specs.iter().find(|s| s.name == tool).expect("no such tool");
        let mut node = &spec.schema["properties"][prop];
        if in_items {
            node = &node["items"];
        }
        node["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool}.{prop} has no enum in its schema"))
            .iter()
            .map(|v| v.as_str().expect("enum values are strings").to_string())
            .collect()
    }

    fn engine() -> Engine {
        Engine::open_in_memory().expect("in-memory store")
    }

    /// Every `group_by` the `tasqx_summary` schema advertises must be an axis
    /// `report.summary` actually accepts, and the schema must not omit one it
    /// accepts.
    ///
    /// The failure this guards: the schema's enum was a hand-typed JSON copy of
    /// the engine's `matches!` arm. Drop `priority` from the JSON and an agent
    /// never groups by priority again — every existing test still passes,
    /// because a report that simply never gets asked for looks like no bug at
    /// all. Add a fourth value and every call using it fails at runtime only.
    #[test]
    fn summary_group_by_schema_matches_what_the_engine_accepts() {
        let e = engine();
        let advertised = schema_enum("tasqx_summary", "group_by", false);

        // Direction 1 — nothing advertised is rejected. This walks the values
        // an agent would actually pick out of the published schema.
        for axis in &advertised {
            let out = dispatch(&e, "report.summary", &json!({ "group_by": axis }));
            assert!(out.is_ok(), "schema advertises group_by `{axis}`, engine rejects it: {out:?}");
        }

        // Direction 2 — nothing accepted is *hidden*. This is the direction a
        // behavioural loop cannot cover: an axis the schema omits is simply
        // never exercised, so every test still passes while the agent loses the
        // option. `SUMMARY_GROUP_BY` is the engine's own validation list, and
        // direction 3 below proves it is not itself a shrunken copy.
        assert_eq!(
            advertised,
            SUMMARY_GROUP_BY.map(String::from).to_vec(),
            "the MCP schema no longer advertises exactly the axes the engine validates against"
        );

        // Direction 3 — the list really is closed, so direction 2 is not
        // satisfied by an engine that accepts anything. `""` is deliberately
        // not probed: an empty `group_by` reads as "omitted" and takes the
        // default, which is a separate contract.
        for bogus in ["tag", "urgency", "Project", "due"] {
            assert!(
                dispatch(&e, "report.summary", &json!({ "group_by": bogus })).is_err(),
                "engine accepts group_by `{bogus}`, which the schema never advertises"
            );
        }
    }

    /// Every `metrics` value the schema advertises must produce a real field in
    /// the report, and a value outside the set must not.
    ///
    /// The failure this guards: the metric names live in a `match` inside
    /// `report_summary` whose `_ => {}` arm *silently ignores* anything it does
    /// not know. So a schema that advertises `est_hours` after the engine
    /// renamed it to `est_total` produces a successful, empty-looking report —
    /// no error, no failing test, just an agent that concludes tasqx cannot
    /// total estimates.
    #[test]
    fn summary_metrics_schema_matches_the_fields_the_engine_emits() {
        let e = engine();
        dispatch(&e, "task.add", &json!({ "title": "t", "estimate": "PT1H" })).unwrap();

        let advertised = schema_enum("tasqx_summary", "metrics", true);

        // A metric the schema *omits* is never exercised by the loop below, so
        // it would drop out of the agent's vocabulary in total silence. Pin the
        // published set against the engine's own list first.
        assert_eq!(
            advertised,
            SUMMARY_METRICS.map(String::from).to_vec(),
            "the MCP schema no longer advertises exactly the metrics the engine emits"
        );

        for metric in &advertised {
            let out =
                dispatch(&e, "report.summary", &json!({ "group_by": "status", "metrics": [metric] }))
                    .expect("report.summary");
            let group = &out["groups"][0];
            assert!(
                group.get(metric).is_some(),
                "schema advertises metric `{metric}`, but the report has no such field: {group}"
            );
        }
        // The `_ => {}` arm is the trap: an unknown metric is accepted and
        // dropped. Pin that a name NOT in the schema really is unknown, so the
        // check above cannot be satisfied by an always-present field.
        let out = dispatch(
            &e,
            "report.summary",
            &json!({ "group_by": "status", "metrics": ["est_hours"] }),
        )
        .expect("report.summary");
        assert!(
            out["groups"][0].get("est_hours").is_none(),
            "`est_hours` is not a real metric; if it became one, the schema must list it"
        );
    }

    /// Every priority letter the `tasqx_add_task` schema advertises must be one
    /// `task.add` stores and reads back unchanged.
    ///
    /// The failure this guards: the schema's `["H","M","L"]` was a hand-copy of
    /// the `Priority` enum. It is now rendered from `Priority::ALL`, and this
    /// walks the rendered list through a real add/get round trip — so adding a
    /// variant without teaching the engine to persist it fails here rather than
    /// at an agent's call site.
    #[test]
    fn priority_schema_values_round_trip_through_the_engine() {
        let e = engine();
        let advertised = schema_enum("tasqx_add_task", "priority", false);
        assert_eq!(advertised.len(), Priority::ALL.len(), "schema lost a priority");

        for p in &advertised {
            let added = dispatch(&e, "task.add", &json!({ "title": format!("p{p}"), "priority": p }))
                .unwrap_or_else(|err| panic!("schema advertises priority `{p}`, add failed: {err:?}"));
            let got = dispatch(&e, "task.get", &json!({ "ref": added["short_id"] })).expect("get");
            assert_eq!(
                got["priority"].as_str(),
                Some(p.as_str()),
                "priority `{p}` did not survive a round trip"
            );
        }
    }

    /// The tool registry is what an agent enumerates before it can call
    /// anything, and every entry names a core dispatch method by string. A
    /// renamed method leaves the tool listed and permanently broken — the
    /// scope fence and the schema both still look fine, and nothing calls
    /// `dispatch` with that name until an agent does.
    #[test]
    fn every_tool_names_a_method_dispatch_actually_routes() {
        let e = engine();
        for spec in tool_specs() {
            let err = dispatch(&e, spec.method, &json!({}))
                .err()
                .map(|err| err.message)
                .unwrap_or_default();
            assert!(
                !err.starts_with("unknown method"),
                "tool `{}` maps onto `{}`, which dispatch does not route",
                spec.name,
                spec.method
            );
        }
    }
}
