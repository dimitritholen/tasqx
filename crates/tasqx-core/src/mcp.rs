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

use std::sync::LazyLock;

use serde_json::{json, Map, Value};

use crate::dispatch::dispatch;
use crate::engine::{
    Engine, MEMORY_SCOPES, SORT_KEYS, SUMMARY_GROUP_BY, SUMMARY_METRICS, TASK_FIELDS,
};
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

/// The capability scope an operator selects for one local stdio server process.
/// This is process configuration, not authentication: `Read` rejects write
/// tools and `Write` permits them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Read-only: the five read tools (`tasqx_list_*`, `get_task`, `summary`,
    /// `search_memory`). Write tools are refused with an `isError` result.
    Read,
    /// Full access: every tool.
    Write,
}

impl Scope {
    /// The scope name as an operator writes it on the command line and as
    /// `initialize` reports it back.
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
}

/// One exposed MCP tool: its name, the core method it maps onto 1:1, whether it
/// is a write (destructive) tool, a model-facing description, and its
/// JSON-Schema `inputSchema`.
struct ToolSpec {
    name: &'static str,
    method: &'static str,
    write: bool,
    /// Whether this call can destroy or overwrite information the store
    /// already holds — the MCP `destructiveHint`, and the thing a host's
    /// confirmation policy keys off (§7, D64).
    ///
    /// It is a per-tool fact and not `write` restated. Derived from the write
    /// flag it said the same thing twice and therefore said nothing: creating
    /// a task, appending an annotation and opening a timer carried the label
    /// reserved for permanently deleting a memory doc, so an operator gating
    /// on it gated all fourteen writes or none, and turned the gate off.
    destructive: bool,
    /// Whether repeating the call with identical arguments leaves the store in
    /// the state one call left it in — the MCP `idempotentHint`.
    ///
    /// A refusal is not an effect: `tasqx_stop_timer` on an already-stopped
    /// task conflicts and writes nothing, which is idempotent by this
    /// definition. Reads are trivially true.
    idempotent: bool,
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

/// The one sentence describing what a date field takes, for every date field.
///
/// `due`, `scheduled` and `wait` all resolve through `datetime::parse_when`
/// (D33), so three separate descriptions would be three chances to advertise
/// three different grammars for one parser — and the first version did exactly
/// that, claiming RFC3339 only, which is the narrowest of the spellings the tool
/// prints in its own parse error.
const WHEN_GRAMMAR: &str = "Date/time in the tool's date grammar: \"tomorrow\", \
    \"friday\", \"2026-07-20\", \"in 3 days\", \"eom\", or \"2026-07-20T17:00\".";

/// How many annotations `tasqx_get_task` returns when the caller names no page
/// size.
///
/// Sized against the failure it exists to prevent rather than by taste: a task
/// whose annotations had accumulated over five days of real work returned tens
/// of kilobytes and exceeded an MCP client's tool-output limit. The regression
/// test in `tests/mcp.rs` builds a history of that shape and asserts the whole
/// response against a byte budget, so this number is answerable rather than
/// merely chosen — re-run it before changing the value.
///
/// It bounds ROWS, and rows are not the unit the problem is expressed in, which
/// is why [`RESPONSE_BUDGET_BYTES`] sits beside it: the task that produced the
/// field report carried eleven enormous annotations rather than two hundred
/// small ones, so this number alone returned every one of them and bounded
/// nothing.
const ANNOTATION_PAGE: u64 = 20;

/// How many tasks `tasqx_list_tasks` returns when the caller names no `limit`.
///
/// A STARTING page, not the answer: like [`ANNOTATION_PAGE`] it bounds rows,
/// and rows are not the unit a client's limit is expressed in, so the response
/// is then shrunk to fit [`RESPONSE_BUDGET_BYTES`] by bisection. Sized to be
/// generous enough that an ordinary store never notices, because the byte fit
/// is what actually holds.
///
/// The failure it exists to prevent, measured on a real store of 223 tasks:
/// `tasqx_list_tasks {}` — the first call an agent makes, and the one the
/// tool's own schema invites with "no filter means no filtering" — answered
/// **180,412 bytes** in one block, past most clients' tool-output limit, with
/// no elision and nothing saying anything had been large. This is the shape
/// D63 fixed for `task.get`; `task.list`'s worst case is bigger and grows with
/// the store rather than with one task's history.
const LIST_PAGE: u64 = 100;

/// The size a `tasqx_get_task` response is shrunk to fit, counting BOTH content
/// blocks — the rendered view and the JSON behind it, which D49 ships together.
///
/// Measured against the failure rather than chosen: the reported response was
/// ~58 KB and exceeded a client's tool-output limit, and its JSON half alone
/// measured ~29 KB on the live store. A budget under that half is what makes the
/// first, uninstructed call fit, which matters because a client that hard-fails
/// on an oversized result never gets to retry with a smaller page.
///
/// It is a budget, not a guarantee. A single annotation larger than this still
/// exceeds it: the floor is one whole annotation, because truncating a body
/// would hand the reader prose that stops mid-sentence with no marker, and a
/// silently altered body is worse than a large one.
const RESPONSE_BUDGET_BYTES: usize = 24_576;

/// A date field's schema: what *this* field does, then the grammar every date
/// field shares.
///
/// The grammar stays in one place for D33's reason, and the effect clause is
/// per field for the opposite one. [`WHEN_GRAMMAR`] alone fits `due`,
/// `scheduled` and `wait` equally well, so three identical descriptions left an
/// agent no way to choose between them: an MCP client set `scheduled` to a
/// four-week review date meaning "check back then" and parked the work in
/// `backlog`, invisible to `@working` until that date, and found out only by
/// reading `status` back out of the response.
fn when_schema(effect: &str) -> Value {
    json!({ "type": "string", "description": format!("{effect} {WHEN_GRAMMAR}") })
}

/// Schema fragment for a `ref` argument (short_id int OR full UUID string).
fn ref_schema() -> Value {
    json!({
        "type": ["integer", "string"],
        "description": "Task reference: short_id (integer) or full UUID (string)."
    })
}

/// Add the #12 correlation properties to a lifecycle tool's schema.
///
/// One function, not two hand-typed copies, because `tasqx_start_timer` and
/// `tasqx_complete_task` must describe the same four params identically
/// (D30). They are deliberately agent-visible: the schema-equality test
/// requires schema properties == PARAMS, and that is intended — an agent that
/// knows its own session id or transcript path SHOULD pass them, and `client`
/// is filled in server-side from the MCP handshake when omitted.
fn with_correlation(mut schema: Value) -> Value {
    let props = schema["properties"]
        .as_object_mut()
        .expect("tool schemas declare properties");
    props.insert(
        "session_id".to_string(),
        json!({
            "type": "string",
            "description": "Correlation: your agent-session id, recorded on this task's \
                event for later token attribution. Pass it if your runtime exposes one."
        }),
    );
    props.insert(
        "prompt_id".to_string(),
        json!({
            "type": "string",
            "description": "Correlation: the id of the prompt/turn driving this call, \
                recorded on this task's event for later token attribution."
        }),
    );
    props.insert(
        "transcript_path".to_string(),
        json!({
            "type": "string",
            "description": "Correlation: absolute path to your session transcript/log \
                file, recorded on this task's event so token usage can be attributed \
                from it later."
        }),
    );
    props.insert(
        "client".to_string(),
        json!({
            "type": "string",
            "description": "The calling tool as \"<name> <version>\". Filled in \
                automatically from the MCP clientInfo handshake when omitted — only \
                pass it to override that."
        }),
    );
    schema
}

/// Every dispatch method that deliberately has **no** MCP tool, and why.
///
/// The MCP surface drifted into additive-only without anybody deciding it: of
/// the methods `dispatch::PARAMS` carries, the ones that had quietly gone
/// unexposed were, with the exception of the internal ones, the corrective or
/// destructive half of a pair whose other half was reachable. An agent could
/// tag and not untag, block and not unblock, close and not reopen, write a
/// memory and not retract it. Nobody chose that; it accumulated, because
/// exposing a tool was a decision and NOT exposing one was silence.
///
/// This table is what turns the silence into a decision. `every_dispatch_method_is_exposed_or_listed_here`
/// asserts it against [`tool_specs`] in both directions, so a new method must
/// either ship a tool or land here with a reason, and an entry that stops being
/// true fails the build rather than sitting as a stale note.
///
/// A reason is not a formality. "Nobody asked for it" is a fine reason and is
/// written as such; what is not allowed is an omission with nothing beside it.
const UNEXPOSED_METHODS: &[(&str, &str)] = &[
    (
        "core.capabilities",
        "the MCP handshake already answers this question: `initialize` reports the protocol          revision and scope, and `tools/list` reports the surface. A second, differently          shaped capability document is a second thing to keep in sync.",
    ),
    (
        "event.list",
        "the audit log is unbounded and has no paging, so exposing it would repeat the          `task.get` mistake D63 fixed. It needs the same limit/offset treatment before it          can be a tool; no client has asked for it yet.",
    ),
    (
        "event.revert",
        "tasqx has an undo (D54) and an agent cannot reach it, which is the sharpest single          omission on this list. It stays off until the tool can show what it is about to          undo: `event.revert` acts on the last matching event, and its blast radius depends          on store state the calling agent has not read. A destructive one-shot whose effect          the caller cannot see is not a tool, it is a coin flip.",
    ),
    (
        "memory.import",
        "it takes a batch of documents read off a filesystem, and the filesystem the CLI          reads is not the one an MCP client is on. `memory.add` is the per-document tool          that does reach across the wire.",
    ),
    (
        "project.archive",
        "retiring a project is a decision about the human's workspace, not about the work.          An agent asked to tidy the project list is being asked to make that decision on          their behalf, and the CLI is where it belongs.",
    ),
    (
        "project.use",
        "ruled out by D22: an agent has `project` on `task.add` and should name it, rather          than silently re-aiming the human's default for every later call, including the          human's own.",
    ),
    (
        "reminder.fire",
        "daemon-internal. Its `reminded` event is a dedupe key and a push surface, not a          thing a client asks for.",
    ),
    (
        "store.export",
        "an agent cannot snapshot what it just wrote, and that is a real gap — but the          payload is the whole store, which is exactly the size problem D63 and D66 spent          two rounds on. It needs a filter and a budget before it is a tool rather than a          way to blow a client's limit in one call.",
    ),
    (
        "store.import",
        "it overwrites, in bulk, from a document nobody has reviewed. The confirmation model          (§7) defers to the host's gate, and a host gate on a call whose diff nobody can see          is not a safeguard.",
    ),
    (
        "task.cancel",
        "already reachable: §7 routes cancellation through `task.modify status:cancelled`,          and the engine accepts exactly that one transition. A second spelling of a reachable          behaviour is the drift D30 warns about, not a missing capability.",
    ),
    (
        "token.add",
        "a measurement after the fact, and D50 makes the completion's self-report the primary          channel precisely so one task never mixes channels. An agent with a count to report          has `tasqx_complete_task`.",
    ),
    (
        "tokens.recompute",
        "a maintenance pass over the whole store's attribution. It is an operator action with          a runtime proportional to history, not a step in anybody's task.",
    ),
];

/// Tool arguments this server READS AND DOES NOT FORWARD, each with the reason
/// it belongs to the transport rather than to the method.
///
/// §7's 1:1 mapping — the arguments object *is* the method's params — is what
/// makes every tool answerable from `dispatch::PARAMS`, and D64 leaned on it
/// when it declined to add a second identifying field to `tasqx_remove_memory`.
/// This narrows it rather than abandoning it: an entry here names a property of
/// the RESPONSE ENVELOPE, which is the transport's own subject and nothing the
/// engine could answer, and `check_params` would refuse it as an unknown key if
/// it were forwarded. Everything else still passes straight through.
///
/// The table exists so the narrowing cannot spread by accident. A guard asserts
/// it against the schemas in both directions, so an argument added to a schema
/// and not forwarded either lands here with an argument or reddens the build —
/// the `UNEXPOSED_METHODS` move, applied to the other end of the same seam.
const TRANSPORT_ONLY_ARGS: &[(&str, &str, &str)] = &[(
    "tasqx_get_task",
    "include_json",
    "whether the response carries the machine-readable block beside the rendered view.      The two blocks are the same result twice (D49), so on a task whose bulk is annotation      prose the second is that prose again — 54% of a 6.4 KB response for ONE annotation,      66% for a task read with `annotations_limit: 0`. D66 spends that duplicate only when      the budget is already blown, which left every ordinary read paying it in full and no      way to decline. `task.get` has no opinion on how many blocks its answer is wrapped in.",
)];

/// Built once per process. The table is a pure function of compile-time
/// constants — every runtime `format!` in it renders a `const` list — and it
/// was being rebuilt, nineteen `json!` schemas and their strings, on every
/// tools/call and tools/list, then linear-searched and dropped.
static TOOL_SPECS: LazyLock<Vec<ToolSpec>> = LazyLock::new(build_tool_specs);

/// The full §7 tool surface. Each entry maps 1:1 onto a core dispatch method;
/// the tool `arguments` object is passed straight through as the method params
/// (argument names are identical to the core param names by design), except for
/// the arguments listed in [`TRANSPORT_ONLY_ARGS`], which this server reads and
/// consumes.
fn tool_specs() -> &'static [ToolSpec] {
    &TOOL_SPECS
}

fn build_tool_specs() -> Vec<ToolSpec> {
    vec![
        // ---- reads ----------------------------------------------------------
        ToolSpec {
            name: "tasqx_list_tasks",
            method: "task.list",
            write: false,
            destructive: false,
            idempotent: true,
            description: "List tasks matching a filter-DSL query. The filter is \
                the same grammar the CLI takes, e.g. \
                \"project:work.tasqx status:pending +api due.before:tomorrow\". \
                Rows come back in pages: the response carries `count` (returned), `total` \
                (matched) and `next_offset`, null once nothing is left. Project \
                `depends_on` with `fields` to see what a blocked row is waiting on.",
            schema: json!({
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "string",
                        "description": "Filter DSL query, e.g. \"status:pending +api\". Use \"@working\" for the active working set. Omit it (or send \"\") for every task: no filter means no filtering."
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
                    // `minimum: 0`, not 1: `opt_u64` accepts 0 and the engine's
                    // own refusal for a negative limit says "send 0 or more".
                    // A schema that contradicts the sentence the engine prints
                    // denies an agent a call that works.
                    "limit": {
                        "type": "integer",
                        "minimum": 0,
                        "description": format!(
                            "How many rows to return. Omit and this tool applies its own page \
                             ({LIST_PAGE}), shrunk further if the response would exceed its byte \
                             budget; the answer always carries `total` and a `next_offset` that \
                             is null once nothing is left. A limit you name is answered exactly, \
                             however large."
                        )
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How many matching rows to skip. Pass the `next_offset` of \
                             the previous response to walk the rest; ordering is stable across \
                             pages, so a row is never shown twice or missed."
                    },
                    "fields": {
                        "type": "array",
                        "items": { "type": "string", "enum": enum_of(TASK_FIELDS.iter().map(String::as_str)) },
                        "description": "Restrict each row to these fields. An unknown name is rejected, not ignored."
                    }
                }
            }),
        },
        ToolSpec {
            name: "tasqx_get_task",
            method: "task.get",
            write: false,
            destructive: false,
            idempotent: true,
            description: "Get one task's full detail: fields, tags, annotations, and \
                dependencies. A long annotation history is returned newest-first in pages — \
                the response always carries `annotations_total`, and `annotations_next_offset` \
                whenever older annotations were left out.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "annotations_limit": {
                        "type": "integer",
                        "minimum": 0,
                        "description": format!(
                            "How many of the MOST RECENT annotations to return. Omit and this \
                             tool applies its own page size ({ANNOTATION_PAGE}), because an \
                             unbounded history can exceed a client's tool-output limit; pass \
                             `annotations_total` from a previous response to get every one. \
                             0 returns none, which is how you read a task's fields without its \
                             history."
                        )
                    },
                    "include_json": {
                        "type": "boolean",
                        "description": "Send the machine-readable JSON block as well as the \
                             rendered view. Default true. The two blocks are the same result \
                             twice, so on a task whose bulk is annotation prose the JSON is \
                             that prose again — measured at 54% of a 6.4 KB response for one \
                             annotation, and 66% for a task read with `annotations_limit: 0`. \
                             Send false when you are going to read the view."
                    },
                    "annotations_offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How many annotations to skip, counted back from the \
                            newest. Pass the `annotations_next_offset` of the previous response \
                            to walk further into the history; that field is null once there is \
                            nothing older."
                    }
                },
                "required": ["ref"]
            }),
        },
        ToolSpec {
            name: "tasqx_summary",
            method: "report.summary",
            write: false,
            destructive: false,
            idempotent: true,
            description: "Aggregate report grouped by project, status, or priority. Pure read, no side effects.",
            schema: json!({
                "type": "object",
                "properties": {
                    "group_by": {
                        "type": "string",
                        "enum": enum_of(SUMMARY_GROUP_BY),
                        "description": format!("Grouping axis. Optional; defaults to {}.", SUMMARY_GROUP_BY[0])
                    },
                    "filter": { "type": "string", "description": "Optional filter DSL to scope the report." },
                    "all": {
                        "type": "boolean",
                        "description": "Count cancelled tasks too. By default a report with no status term in its filter skips only cancelled tasks — done work always counts (D24)."
                    },
                    "metrics": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": enum_of(SUMMARY_METRICS)
                        }
                    }
                }
            }),
        },
        ToolSpec {
            name: "tasqx_list_projects",
            method: "project.list",
            write: false,
            destructive: false,
            idempotent: true,
            description: "List projects. By default excludes archived projects.",
            schema: json!({
                "type": "object",
                "properties": {
                    "include_archived": { "type": "boolean" }
                }
            }),
        },
        // Read, deliberately (D41): consulting knowledge mutates nothing, so a
        // read-only agent gets it too.
        ToolSpec {
            name: "tasqx_search_memory",
            method: "memory.search",
            write: false,
            destructive: false,
            idempotent: true,
            description: "Search the memory store: imported docs/patterns and \
                task annotations, bm25-ranked with snippets. Plain text queries \
                are matched as phrases; set raw=true for FTS5 operator syntax \
                (prefix*, AND/OR, column filters). A hit carries a short excerpt: \
                read a doc whole with `tasqx_get_memory` on its `id`, and an \
                annotation whole with `tasqx_get_task` on the task its `source` \
                names. Every word of a plain query is REQUIRED, so `matched` on the \
                result is what explains a zero-hit answer.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Plain-text search words. Matched as quoted phrases, so hyphens and dots are safe."
                    },
                    // No `minimum` bound: `query` is required, so the one-key
                    // boundary probe in the minimum guard could never test it.
                    "limit": { "type": "integer", "description": "Max hits (0 or more); default 10." },
                    "scope": {
                        "type": "string",
                        "enum": enum_of(MEMORY_SCOPES),
                        "description": format!("What to search. Optional; defaults to {}.", MEMORY_SCOPES[0])
                    },
                    "raw": {
                        "type": "boolean",
                        "description": "Pass the query through as FTS5 syntax. Invalid syntax is refused as bad_request."
                    }
                },
                "required": ["query"]
            }),
        },
        // ---- writes ---------------------------------------------------------
        ToolSpec {
            name: "tasqx_get_memory",
            method: "memory.get",
            write: false,
            destructive: false,
            idempotent: true,
            description: "Read one knowledge doc whole, by the `id` a search hit carries. \
                `tasqx_search_memory` returns a short excerpt and this is how you get the rest; \
                an annotation id is refused, naming the task to read it from instead.",
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The doc UUID, as printed by a search hit or by `tasqx_add_memory`."
                    }
                },
                "required": ["id"]
            }),
        },
        ToolSpec {
            name: "tasqx_add_task",
            method: "task.add",
            write: true,
            destructive: false,
            idempotent: false,
            description: "Create a new task. Returns its short_id, urgency and status — which \
                is `backlog`, not `pending`, when `scheduled` or `wait` is in the future, and a \
                backlog task is outside the `@working` set until that date passes.",
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
                    // Every date field names the SAME grammar, because they all
                    // run through `datetime::parse_when` (D33). Advertising
                    // RFC3339 alone was a schema narrower than the engine: an
                    // agent would never send `tomorrow`, which works. What the
                    // grammar cannot say is which field to reach for, so each
                    // one now leads with its own effect — see `when_schema`.
                    "due": when_schema(
                        "The deadline. Drives the urgency score and anchors relative reminders; \
                         it does not hide the task, so an overdue one stays in the working set."
                    ),
                    "scheduled": when_schema(
                        "When you intend to start. A future value holds the task in `backlog`, \
                         out of the `@working` set, until it arrives; `agenda` then places the \
                         task on the earlier of `due` and `scheduled`."
                    ),
                    "wait": when_schema(
                        "Hide the task until then. A future value holds it in `backlog` exactly \
                         as `scheduled` does; the difference is intent — `wait` is \"not my \
                         problem yet\", `scheduled` is \"I plan to start then\" and is the one \
                         `agenda` places on."
                    ),
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "estimate": { "type": "string", "description": "Duration: \"4h\", \"90m\", \"1h30m\", \"2d\", \"1w\", or ISO-8601 \"PT4H\"." },
                    "recurrence": {
                        "type": "string",
                        "description": "Recurrence rule (D2 subset): \"daily\", \"every 3 days\", \"weekly on mon,wed\", \"monthly on day 15\", \"monthly on the last friday\"."
                    },
                    "remind": {
                        "type": "string",
                        "description": "Reminder: a signed offset from `due` (\"-1h\", \"-30m\", \"-2d\", \"+15m\") or an absolute date in the `due` grammar."
                    }
                },
                "required": ["title"]
            }),
        },
        ToolSpec {
            name: "tasqx_modify_task",
            method: "task.modify",
            write: true,
            destructive: true,
            idempotent: false,
            description: "Change fields on a task via a `set` map. Optimistic concurrency \
                is ON by default: when `expected_rev` is omitted, this server reads the \
                task's current `_rev` and pins it, so a concurrent edit yields a `conflict` \
                naming both revs instead of a silent overwrite — there is no way to opt \
                out. On `conflict`: re-read the task (`tasqx_get_task`), re-apply the \
                change to the fresh state, and retry. Under contention that loop is the \
                protocol working, not a failure.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "set": {
                        "type": "object",
                        "description": "Field → new value, e.g. {\"priority\":\"M\",\"due\":\"2026-07-22T17:00:00+02:00\"}."
                    },
                    "expected_rev": { "type": "integer", "description": "Optimistic-concurrency guard. Supplied by the server from the task's current `_rev` when omitted; pass it only to pin a rev you read earlier. There is no last-writer-wins mode." }
                },
                "required": ["ref", "set"]
            }),
        },
        ToolSpec {
            name: "tasqx_complete_task",
            method: "task.done",
            write: true,
            destructive: true,
            idempotent: false,
            description: "Mark a task done. Returns any tasks newly unblocked by its \
                completion. Report the tokens this task cost via the *_tokens params — \
                the caller is the only party that knows which task a turn's spend \
                served, so self-report is the primary measurement channel; any present \
                count records a measurement. If you cannot observe your token spend, still \
                send `tool` and `model` — they are recorded on the completion event without \
                any count, and the response says what was recorded. Correlation params \
                (session_id, prompt_id, transcript_path, client) land on that same event; \
                without a self-report, log-parse attribution is a fallback that refuses \
                samples claimed by more than one task's window.",
            // The token-count fields carry no `minimum`: the numeric-minimum
            // drift guard cannot probe a bound on a tool with required args,
            // so the floor lives in the engine (opt_u64 refuses negatives)
            // and the description says "0 or more".
            schema: with_correlation(json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "tool": {
                        "type": "string",
                        "description": "The AI tool doing the work, free-form (e.g. \
                            \"claude-code\"). Recorded on the completion event on its own; \
                            when token counts are present it also names the measurement, \
                            defaulting to `client` if you omit it. You do NOT need a token \
                            count to send it."
                    },
                    "model": {
                        "type": "string",
                        "description": "The model doing the work, e.g. \"claude-opus-5\". \
                            Recorded on the completion event on its own, and carried on the \
                            measurement when token counts are present. You do NOT need a \
                            token count to send it."
                    },
                    "input_tokens": {
                        "type": "integer",
                        "description": "Self-reported input tokens this task cost (0 or more)."
                    },
                    "output_tokens": {
                        "type": "integer",
                        "description": "Self-reported output tokens this task cost (0 or more)."
                    },
                    "cache_read_tokens": {
                        "type": "integer",
                        "description": "Self-reported cache-read tokens this task cost (0 or more)."
                    },
                    "cache_creation_tokens": {
                        "type": "integer",
                        "description": "Self-reported cache-creation tokens this task cost (0 or more)."
                    }
                },
                "required": ["ref"]
            })),
        },
        ToolSpec {
            name: "tasqx_reopen_task",
            method: "task.reopen",
            write: true,
            destructive: true,
            idempotent: false,
            // The inverse of the two closes an agent can reach: `task.done` has
            // its own tool and cancellation goes through `task.modify
            // status:cancelled` (§7). Both were reachable and neither could be
            // taken back, which is the additive-only shape D67 removes.
            description: "Reopen a closed task: done or cancelled goes back to pending, and the \
                completion timestamp is cleared so the task stops answering questions about a \
                week it is no longer finished in. Use it when a task was closed or cancelled in \
                error. A task that is not closed is a conflict, not a no-op.",
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
            destructive: false,
            idempotent: false,
            description: "Start the timer on a task (moves it to active). Correlation \
                params (session_id, prompt_id, transcript_path, client) are recorded on \
                the start event for token attribution.",
            schema: with_correlation(json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "keep": {
                        "type": "boolean",
                        "description": "Keep other active tasks running (opt out of single-active)."
                    }
                },
                "required": ["ref"]
            })),
        },
        ToolSpec {
            name: "tasqx_stop_timer",
            method: "task.stop",
            write: true,
            destructive: false,
            idempotent: true,
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
            destructive: false,
            idempotent: true,
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
            name: "tasqx_untag_task",
            method: "tag.remove",
            write: true,
            destructive: true,
            idempotent: true,
            description: "Remove one or more tags from a task. Returns the resulting tag set. \
                Removing a tag the task does not carry is not an error.",
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
            name: "tasqx_annotate_task",
            method: "annotation.add",
            write: true,
            destructive: false,
            idempotent: false,
            description: "Attach a timestamped note to a task. The body is \
                stored verbatim (newlines and markdown included), so this is \
                where long-form context lives: acceptance criteria, links, \
                implementation notes.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "body": { "type": "string", "description": "Note text, stored verbatim. Multi-line markdown is fine." }
                },
                "required": ["ref", "body"]
            }),
        },
        ToolSpec {
            name: "tasqx_add_dependency",
            method: "dependency.add",
            write: true,
            destructive: false,
            idempotent: true,
            description: "Make one task depend on another: `ref` is blocked \
                until `depends_on` is done or cancelled. Returns the resulting \
                dependency list and blocked state. A cycle is refused as a \
                conflict.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "depends_on": {
                        "type": ["integer", "string"],
                        "description": "The task `ref` must wait for: short_id (integer) or full UUID (string)."
                    }
                },
                "required": ["ref", "depends_on"]
            }),
        },
        ToolSpec {
            name: "tasqx_remove_dependency",
            method: "dependency.remove",
            write: true,
            destructive: true,
            idempotent: true,
            description: "Cut a dependency edge: `ref` stops waiting on `depends_on`. Returns \
                the remaining dependency list and blocked state, so the answer says whether the \
                task is actually actionable now or still waiting on something else.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "depends_on": {
                        "type": ["integer", "string"],
                        "description": "The blocker to stop waiting for: short_id (integer) or full UUID (string)."
                    }
                },
                "required": ["ref", "depends_on"]
            }),
        },
        ToolSpec {
            name: "tasqx_add_memory",
            method: "memory.add",
            write: true,
            destructive: false,
            idempotent: false,
            description: "Store a knowledge document in memory: company \
                patterns, documentation, decisions worth finding again. Body is \
                stored verbatim (markdown fine) and becomes searchable via \
                tasqx_search_memory.",
            schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "body": { "type": "string", "description": "Stored verbatim; multi-line markdown is fine." },
                    "source": { "type": "string", "description": "Where this came from: a path, URL, or ticket." }
                },
                "required": ["title", "body"]
            }),
        },
        ToolSpec {
            name: "tasqx_remove_memory",
            method: "memory.remove",
            write: true,
            destructive: true,
            idempotent: true,
            // The permanence is stated because it is the one property of this
            // tool a caller cannot learn by trying: every other write reachable
            // through this server is either revertible or restatable, so an
            // agent handed a delete with nothing said reads it as reversible.
            // D54's `undo` covers task edits and deliberately not memory docs —
            // the event log records that a doc went and does not carry its body,
            // so there is nothing to put back.
            description: "Remove one knowledge document from memory by id, the id \
                tasqx_search_memory returns. Use it to retract something you wrote that turned \
                out to be wrong — a correction written as a second document leaves both in the \
                store, and search ranks them together with nothing to say which is true. The \
                removal is permanent: `tasqx undo` does not cover memory documents, and the \
                body is not recoverable from the event log.",
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The document's id, as tasqx_search_memory reports it \
                            on a `doc` hit."
                    }
                },
                "required": ["id"]
            }),
        },
        ToolSpec {
            name: "tasqx_create_project",
            method: "project.create",
            write: true,
            destructive: false,
            idempotent: true,
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

/// The tool roster as `(name, is_write)` pairs, in `tools/list` order.
///
/// Public for the doc-drift guards, not for callers: the HTML guide and the
/// README both restate this roster, and each binds itself to this list — a
/// tool added to `tool_specs` without reaching those surfaces (or a tool
/// they name that no longer exists) fails their tests instead of shipping as
/// a quiet disagreement between the server and its documentation.
pub fn tool_roster() -> Vec<(&'static str, bool)> {
    tool_specs().iter().map(|s| (s.name, s.write)).collect()
}

/// The methods deliberately left off the tool surface, as `(method, why)`.
///
/// Public for the same reason [`tool_roster`] is: the guards that hold this
/// decision still live outside the module that makes it. It is also the honest
/// answer to "why can't the agent do X" — the reasons are written for a reader,
/// not for a compiler.
pub fn unexposed_methods() -> &'static [(&'static str, &'static str)] {
    UNEXPOSED_METHODS
}

/// A long-lived MCP session over one [`Engine`], fenced to one [`Scope`]. It is
/// a pure message mapper — all state of record lives in the engine's store.
pub struct McpServer<'e> {
    engine: &'e Engine,
    scope: Scope,
    /// `clientInfo` from the `initialize` handshake, kept so lifecycle calls
    /// can be stamped with the calling tool (#12). `RefCell` rather than
    /// `&mut self`: the stdio loop is single-threaded, and a signature change
    /// would ripple through the CLI loop and every test call site for what is
    /// one late-bound field of per-process session state — not state of
    /// record, which stays in the store.
    client_info: std::cell::RefCell<Option<Value>>,
    /// How the rendered detail view writes time. Session state, fixed at
    /// construction: the CLI resolves the setting once per process, and a value
    /// that could change mid-session would mean two `get_task` calls in one
    /// conversation disagreeing about the same task.
    time_format: crate::markdown::TimeFormat,
}

impl<'e> McpServer<'e> {
    /// Bind a session to one engine and one scope. The scope is fixed for the
    /// life of the server — there is no per-message elevation, which is what
    /// makes "a read-only process" a property of the process rather than of
    /// every individual handler remembering to check.
    pub fn new(engine: &'e Engine, scope: Scope) -> Self {
        McpServer {
            engine,
            scope,
            client_info: std::cell::RefCell::new(None),
            time_format: crate::markdown::TimeFormat::Both,
        }
    }

    /// Choose how the detail view writes time. A builder rather than a third
    /// parameter on [`McpServer::new`]: only `run_mcp_serve` has a setting to
    /// supply, and widening `new` would edit every call site in the test suite
    /// to pass the default back in.
    pub fn with_time_format(mut self, time: crate::markdown::TimeFormat) -> Self {
        self.time_format = time;
        self
    }

    /// The scope this session was created with.
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
        // #12: remember who is talking. clientInfo arrives exactly once, here,
        // and each MCP client spawns its own `tasqx mcp serve` process, so the
        // field is per-session by construction. It is injected into
        // task.start/task.done calls below, never persisted on its own —
        // per-task attribution belongs in those events, not in memory.
        if let Some(info) = params.get("clientInfo") {
            *self.client_info.borrow_mut() = Some(info.clone());
        }
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

        let mut args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

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

        // The expected_rev pattern a third time, for the one field with no
        // bound: a task's annotations are unbounded text, and the tasks worth
        // reading are the ones that have the most of them, so `task.get`
        // answered whole is how the richest history became the one this
        // transport could not carry. The core keeps answering whole — clients
        // have read it that way since v1 was frozen — and the page size is
        // supplied HERE, where the payload limit actually lives. A caller that
        // names its own is respected as-is, including one asking for the lot.
        // The same pattern for the collection reader, and for the same
        // reason one relation over: `task.list` had no default bound at all,
        // and the escape hatch it did have truncated silently — `count` was
        // the number of rows RETURNED, with no total and no offset anywhere in
        // the answer. The core still answers whole when asked; the page is
        // supplied HERE, where the payload limit lives, and `total` /
        // `next_offset` make what was left out both visible and reachable.
        let mut paged_list_by_us = false;
        if spec.method == "task.list" {
            if let Some(obj) = args.as_object_mut() {
                if !obj.contains_key("limit") {
                    obj.insert("limit".to_string(), json!(LIST_PAGE));
                    paged_list_by_us = true;
                }
            }
        }

        // Arguments this server READS AND DOES NOT FORWARD. The removal is
        // driven by [`TRANSPORT_ONLY_ARGS`] rather than written out per key,
        // so the table is load-bearing instead of a note beside the code: a
        // listed argument is stripped whether or not anything below reads it,
        // and `check_params` — which refuses any key the method does not
        // accept — can never see one. Only the *meaning* is per-argument.
        let mut consumed: Map<String, Value> = Map::new();
        if let Some(obj) = args.as_object_mut() {
            for (_, arg, _) in TRANSPORT_ONLY_ARGS
                .iter()
                .filter(|(tool, _, _)| *tool == spec.name)
            {
                if let Some(v) = obj.remove(*arg) {
                    consumed.insert((*arg).to_string(), v);
                }
            }
        }
        // Default true: the second block has been there since D49 and a caller
        // that says nothing gets what it has always got.
        let include_json = consumed
            .get("include_json")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mut paged_by_us = false;
        if spec.method == "task.get" {
            if let Some(obj) = args.as_object_mut() {
                if !obj.contains_key("annotations_limit") {
                    obj.insert("annotations_limit".to_string(), json!(ANNOTATION_PAGE));
                    paged_by_us = true;
                }
            }
        }

        // #12, the expected_rev pattern again: lifecycle calls are stamped
        // with the tool captured at initialize, so the start/done events name
        // who did the work even when the agent passes nothing. A caller that
        // supplies its own `client` is respected as-is — but an explicit
        // `client: null` counts as absent, matching the engine's D32 read
        // (clients that serialize unset optionals as null must not lose
        // attribution).
        if spec.method == "task.start" || spec.method == "task.done" {
            if let Some(obj) = args.as_object_mut() {
                if obj.get("client").is_none_or(Value::is_null) {
                    if let Some(label) = self.client_label() {
                        obj.insert("client".to_string(), Value::String(label));
                    }
                }
            }
        }

        match dispatch(self.engine, spec.method, &args) {
            Ok(result) => {
                // The one rendered surface. Keyed on the method rather than the
                // tool name to match the `task.modify`/`task.start` checks
                // above; exactly one tool maps to `task.get`, so this is the
                // same set either way.
                if spec.method == "task.get" {
                    let opts = crate::markdown::DetailOpts {
                        time: self.time_format,
                        // Stamped HERE, never inside the renderer: that is what
                        // keeps `task_detail` pure and its golden tests stable.
                        now: jiff::Timestamp::now(),
                    };
                    // No notice: `tool_ok_view_only` explains an omission the
                    // caller did not choose, and here the caller chose it.
                    // Saying "both blocks together exceeded this tool's
                    // response budget" over a 400-byte answer is a false
                    // sentence AND a bill — the notice is ~300 bytes, which on
                    // a small task is most of what declining the duplicate was
                    // meant to save.
                    if !include_json {
                        return tool_ok_text(&crate::markdown::task_detail(&result, &opts));
                    }
                    return self.fit_to_budget(result, &args, &opts, paged_by_us);
                }
                if paged_list_by_us {
                    return self.fit_list_to_budget(result, &args);
                }
                tool_ok(&result)
            }
            Err(e) => {
                let code = serde_json::to_value(e.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "internal".to_string());
                tool_error(&code, e.message)
            }
        }
    }

    /// Fit a `task.get` response to [`RESPONSE_BUDGET_BYTES`], spending the
    /// duplicate JSON block before it spends any of the history.
    ///
    /// Only for a caller who named no page size. An explicit `annotations_limit`
    /// is answered exactly as asked, both blocks included, however large: a
    /// request second-guessed is a caller who can never fetch a big page on
    /// purpose, and it is what keeps the frozen machine-readable shape reachable
    /// for every task rather than only the small ones.
    ///
    /// # The order the budget spends in
    ///
    /// 1. both blocks at the default page — an ordinary task never notices this
    ///    function exists;
    /// 2. the view alone at that same page, because on a task whose bulk is
    ///    annotation prose the second block is that prose *again* (D49 renders
    ///    the same result twice, once formatted and once as escaped JSON), and
    ///    paying for a duplicate in history the reader never sees is the worse
    ///    trade;
    /// 3. the view alone, halving to a floor of one whole annotation.
    ///
    /// Dropping the JSON is a one-way door inside a single response: once gone
    /// it stays gone while the page shrinks, so the answer cannot flip shape
    /// halfway through its own search. D49's ordering is what makes this safe —
    /// the view leads *because* it is the block a model reads, so the block that
    /// survives is the one that was already doing the work.
    ///
    /// Bisection rather than extrapolation: bodies vary by orders of magnitude,
    /// so a size-per-row taken from the newest annotations is wrong in exactly
    /// the case that matters, while a measured yes/no per candidate is never
    /// wrong. Five dispatches is the worst case, each a read of one task from a
    /// local store, and only ever on a task already large enough to have failed
    /// outright.
    ///
    /// The floor is one whole annotation: below that the only lever left is
    /// cutting a body, and prose that stops mid-sentence with nothing marking
    /// the cut is worse than an oversized answer. A task whose newest single
    /// annotation exceeds the budget therefore still exceeds it — `0` is the
    /// caller's own escape, and it is documented on the parameter.
    fn fit_to_budget(
        &self,
        first: Value,
        args: &Value,
        opts: &crate::markdown::DetailOpts,
        paged_by_us: bool,
    ) -> Value {
        let render = |result: &Value| crate::markdown::task_detail(result, opts);
        let json_len = |result: &Value| {
            serde_json::to_string_pretty(result)
                .map(|s| s.len())
                .unwrap_or(0)
        };

        // Measured on the FINISHED block, never on the bare view: dropping the
        // JSON adds a sentence saying so, and a view that fits by less than that
        // sentence produced a response over the budget — the payload bound
        // defeated by the notice explaining the payload bound.
        let view_only_fits = |view: &str| view_only_text(view).len() <= RESPONSE_BUDGET_BYTES;

        let view = render(&first);
        if !paged_by_us || view.len() + json_len(&first) <= RESPONSE_BUDGET_BYTES {
            return tool_ok_with_view(view, &first);
        }
        // Step 2: the same page, without the duplicate.
        if view_only_fits(&view) {
            return tool_ok_view_only(&view);
        }

        // Step 3: the largest page that fits, found by BISECTION rather than by
        // halving until something works. Halving lands on a power-of-two
        // fraction of the starting page and stops there, which on the shape
        // that provoked all this — a handful of very long bodies — overshoots
        // by a factor of two: it would show two annotations where four fit.
        // Same number of dispatches, an answer that is actually the largest.
        let mut view = view;
        let mut lo = 1u64;
        let mut hi = ANNOTATION_PAGE;
        let mut best: Option<String> = None;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            let mut retry = args.clone();
            match retry.as_object_mut() {
                Some(obj) => obj.insert("annotations_limit".to_string(), json!(mid)),
                // Unreachable for a real call — `args` is the tool's arguments
                // object — and a fall-through beats a panic in a presentation
                // path that must never make a working call look broken.
                None => break,
            };
            let Ok(candidate) = dispatch(self.engine, "task.get", &retry) else {
                break;
            };
            let rendered = render(&candidate);
            if view_only_fits(&rendered) {
                best = Some(rendered);
                lo = mid + 1;
            } else {
                // `mid` is the floor and it still does not fit: one whole
                // annotation is larger than the budget, and cutting into a body
                // is the one thing this will not do.
                if mid == 1 {
                    view = rendered;
                    break;
                }
                hi = mid - 1;
            }
        }
        tool_ok_view_only(&best.unwrap_or(view))
    }

    /// Fit a `task.list` response to [`RESPONSE_BUDGET_BYTES`] by re-cutting
    /// the page this transport supplied.
    ///
    /// Only for a caller who named no `limit`. One that did is answered
    /// exactly as asked, however large — the same rule `fit_to_budget` keeps
    /// for `annotations_limit`, and for the same reason: a request
    /// second-guessed is a caller who can never fetch a big page on purpose.
    ///
    /// # Why this re-cuts instead of re-dispatching
    ///
    /// `limit` is a *prefix* of a fully determined order — `compare_by` ends on
    /// an unconditional `short_id`, so there are no ties left for a second
    /// query to resolve differently. The `k`-row answer is therefore
    /// byte-identical to what the engine would return for `limit: k`, and can
    /// be produced by truncating the array already in hand. D66's bisection
    /// re-dispatches because a `task.get` page is taken from the *newest* end
    /// and a shorter page is not a prefix of a longer one; here it is. The
    /// difference is worth the paragraph: the first version of this function
    /// did re-dispatch, which is up to seven whole-store scans per call to
    /// answer a question the first scan had already answered — invisible at
    /// 233 tasks (re-measured warm, both versions land at the same 38 ms for
    /// nine reads) and linear in the store from there. Re-cutting is not a
    /// speed trick either way; it is the version whose answer is exact by
    /// construction rather than by a second query agreeing with the first.
    ///
    /// `count` and `next_offset` are recomputed with the array, because a
    /// shortened page whose own count still describes the long one is the
    /// silent-drop shape this whole entry exists to remove. `total` is a
    /// property of the filter and does not move.
    ///
    /// There is no notice block and there does not need to be one:
    /// `task.list` answers `total` and `next_offset`, so a shortened response
    /// states the elision in its own machine-readable shape and names the
    /// offset that reaches the rest — where `task.get` had to say it in prose
    /// because a rendered view has nowhere else to put it. That also keeps
    /// this tool's single content block parseable as the frozen result, which
    /// the conformance guard reads.
    ///
    /// The floor is one whole row: below it the only lever left is cutting a
    /// task in half, and a row that stops mid-field is worse than an oversized
    /// answer. `fields` is the caller's lever for a store whose single row
    /// exceeds the budget, and the schema says so.
    fn fit_list_to_budget(&self, first: Value, args: &Value) -> Value {
        let size = |result: &Value| {
            serde_json::to_string_pretty(result)
                .map(|s| s.len())
                .unwrap_or(0)
        };
        if size(&first) <= RESPONSE_BUDGET_BYTES {
            return tool_ok(&first);
        }

        let Some(rows) = first.get("tasks").and_then(Value::as_array).cloned() else {
            return tool_ok(&first);
        };
        let total = first.get("total").and_then(Value::as_u64).unwrap_or(0);
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let cut = |k: usize| -> Value {
            let reached = offset + k as u64;
            json!({
                "count": k,
                "total": total,
                "next_offset": if reached < total { json!(reached) } else { Value::Null },
                "tasks": rows[..k].to_vec(),
            })
        };

        // Bisection rather than halving, for the reason D66 records: halving
        // lands on a power-of-two fraction of the page and stops there, which
        // on a store of few-and-enormous rows returns a fraction of what fits.
        // Each candidate here is a serialization, not a query.
        let (mut lo, mut hi) = (1usize, rows.len());
        let mut best: Option<Value> = None;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            let candidate = cut(mid);
            if size(&candidate) <= RESPONSE_BUDGET_BYTES {
                best = Some(candidate);
                lo = mid + 1;
            } else {
                if mid == 1 {
                    return tool_ok(&candidate);
                }
                hi = mid - 1;
            }
        }
        tool_ok(&best.unwrap_or(first))
    }

    /// The captured clientInfo as one display string, `"<name> <version>"`
    /// (or just the name when the version is absent/empty). `None` until a
    /// client introduces itself with a non-empty name — injecting an empty
    /// string would trip the engine's D35 empty-string refusal.
    fn client_label(&self) -> Option<String> {
        let info = self.client_info.borrow();
        let name = info.as_ref()?.get("name")?.as_str()?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        match info.as_ref()?.get("version").and_then(Value::as_str) {
            Some(v) if !v.trim().is_empty() => Some(format!("{name} {v}")),
            _ => Some(name),
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
                    "destructiveHint": s.destructive,
                    "idempotentHint": s.idempotent,
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

/// A successful `tools/call` result carrying a rendered human view ahead of the
/// machine-readable JSON.
///
/// Order is deliberate. Clients that surface only the first block prominently
/// then surface the readable one, and a model reading in order takes its cue
/// from what leads. An empty view degrades to [`tool_ok`]: presentation must
/// never be able to make a working call look broken.
fn tool_ok_with_view(view: String, result: &Value) -> Value {
    if view.is_empty() {
        return tool_ok(result);
    }
    json!({
        "content": [
            { "type": "text", "text": view },
            { "type": "text", "text": serde_json::to_string_pretty(result).unwrap_or_default() }
        ],
        "isError": false
    })
}

/// A `tools/call` result carrying the rendered view ALONE, with a line saying
/// the machine-readable block is missing and how to ask for it.
///
/// Emitted only when both blocks together exceed the response budget on a call
/// that named no page size (see [`McpServer::fit_to_budget`]). The note is not
/// optional politeness: a response silently one block short is indistinguishable
/// from a server that never sends JSON, and a reader who cannot tell those apart
/// stops looking for the field they need. It is appended HERE rather than in
/// `markdown::task_detail`, which is pure and golden-tested and knows nothing
/// about transports or budgets.
fn tool_ok_view_only(view: &str) -> Value {
    if view.is_empty() {
        return tool_ok(&Value::Null);
    }
    json!({
        "content": [ { "type": "text", "text": view_only_text(view) } ],
        "isError": false
    })
}

/// One text block, verbatim. The `task.get` view when the caller declined the
/// JSON block, where there is no omission to explain.
fn tool_ok_text(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    })
}

/// The view plus the omission notice, as one block — the exact bytes
/// [`tool_ok_view_only`] sends.
///
/// One function so the budget and the answer measure the same string. They did
/// not: `fit_to_budget` compared the bare view against
/// [`RESPONSE_BUDGET_BYTES`] and the notice was appended afterwards, so a view
/// landing just under the limit produced a response just over it — the payload
/// bound defeated by the sentence explaining the payload bound.
fn view_only_text(view: &str) -> String {
    format!(
        "{view}\n_Machine-readable JSON omitted: both blocks together exceeded this tool's \
         response budget, and the rendered view above carries the same annotations. Naming \
         `annotations_limit` returns both blocks **unbounded** — that is an opt-out of the \
         budget, not a page within it, and on a long history it is several times this \
         response. `include_json: false` keeps the budget and asks for this view on purpose._\n"
    )
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
            assert!(
                out.is_ok(),
                "schema advertises group_by `{axis}`, engine rejects it: {out:?}"
            );
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
            let out = dispatch(
                &e,
                "report.summary",
                &json!({ "group_by": "status", "metrics": [metric] }),
            )
            .expect("report.summary");
            let group = &out["groups"][0];
            assert!(
                group.get(metric).is_some(),
                "schema advertises metric `{metric}`, but the report has no such field: {group}"
            );
        }
        // Pin that a name NOT in the schema really is unknown, so the check
        // above cannot be satisfied by an always-present field. This used to
        // assert the *drop* — the engine accepted `est_hours` and answered a
        // report without it — which is the bug the G1 cluster closed here too:
        // an unknown metric is now refused, so the assertion is that the engine
        // says so rather than that the column is quietly absent.
        let err = dispatch(
            &e,
            "report.summary",
            &json!({ "group_by": "status", "metrics": ["est_hours"] }),
        )
        .expect_err("`est_hours` is not a real metric; if it became one, the schema must list it");
        assert!(
            err.message.contains("est_hours"),
            "the refusal must name it: {}",
            err.message
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
        assert_eq!(
            advertised.len(),
            Priority::ALL.len(),
            "schema lost a priority"
        );

        for p in &advertised {
            let added = dispatch(
                &e,
                "task.add",
                &json!({ "title": format!("p{p}"), "priority": p }),
            )
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

    /// The `required` list an agent reads must be the set the method really
    /// requires — in BOTH directions.
    ///
    /// The failure this guards, found by probing rather than by reading:
    /// `tasqx_list_tasks` declared `required: ["filter"]` and `tasqx_summary`
    /// declared `required: ["group_by"]`, while `task.list` and
    /// `report.summary` both answer a call with no arguments at all. So the
    /// schema forbade a call the engine honours, and an agent that wanted "every
    /// task" was forced to invent a filter — the two-surfaces-disagree shape
    /// pointing the other way from D33's, and just as invisible, because a call
    /// a client never makes cannot fail.
    ///
    /// Derived, not restated (D30): the arbiter is the engine's own answer to an
    /// empty params object, so a param that becomes required tomorrow turns this
    /// red the day it does. The message check is what stops a schema requiring
    /// the wrong key on a method that requires some other one.
    #[test]
    fn every_schema_required_list_matches_what_the_method_actually_requires() {
        let e = engine();
        for spec in tool_specs() {
            let required: Vec<String> = spec.schema["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|v| v.as_str().expect("a name").to_string())
                        .collect()
                })
                .unwrap_or_default();
            let empty = dispatch(&e, spec.method, &json!({}));
            match (required.is_empty(), empty) {
                (true, Ok(_)) => {}
                (false, Err(err)) => assert!(
                    required.iter().any(|k| err.message.contains(k)),
                    "tool `{}` declares required {required:?}, but {} refuses an empty call over \
                     something else entirely: {}",
                    spec.name,
                    spec.method,
                    err.message
                ),
                (true, Err(err)) => panic!(
                    "tool `{}` declares nothing required, but {} refuses a call with no arguments \
                     ({}) — an agent following the schema is refused for obeying it",
                    spec.name, spec.method, err.message
                ),
                (false, Ok(_)) => panic!(
                    "tool `{}` declares required {required:?}, but {} accepts a call with no \
                     arguments at all — the schema forbids a call the engine honours, so an agent \
                     invents a value the engine never needed",
                    spec.name, spec.method
                ),
            }
        }
    }

    /// A numeric bound in a schema must be the engine's own floor, probed at the
    /// boundary from both sides.
    ///
    /// The failure this guards: `tasqx_list_tasks.limit` advertised
    /// `minimum: 1` while `opt_u64` accepts 0 — and the engine's own refusal
    /// message for a negative limit says "send 0 or more", so the schema
    /// contradicted the sentence the engine prints. `limit: 0` is a legitimate
    /// "just the count" call an agent could never make.
    ///
    /// A `minimum` on a tool that also has required arguments cannot be probed
    /// by this one-key call, so it FAILS rather than being skipped: a silent
    /// skip is how a guard goes vacuous, and the floor below would not catch it
    /// while any other bound remained probeable.
    #[test]
    fn every_numeric_minimum_in_a_schema_is_the_engine_s_own_floor() {
        let e = engine();
        // One real task, so a bound sitting behind a required `ref` is probed
        // against a live row rather than against a `not_found` that would pass
        // the "below the floor is refused" half for the wrong reason.
        e.task_add(&json!({ "title": "probe" }))
            .expect("seed a task");
        let mut probed = 0;
        for spec in tool_specs() {
            let required: Vec<&str> = spec.schema["required"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for (name, node) in spec.schema["properties"]
                .as_object()
                .expect("an object schema")
            {
                let Some(min) = node.get("minimum").and_then(Value::as_i64) else {
                    continue;
                };
                // A bound behind a required argument is probed WITH that
                // argument rather than skipped: a skip is how a guard goes
                // vacuous with nothing to show for it. A required key this
                // fixture has no value for still fails, loudly — that is a
                // request to extend the fixture, not to drop the bound.
                let mut call = serde_json::Map::new();
                for key in &required {
                    match *key {
                        "ref" => {
                            call.insert("ref".to_string(), json!(1));
                        }
                        other => panic!(
                            "tool `{}` bounds `{name}` at {min} behind a required `{other}` this \
                             guard has no fixture value for — give it one, do not skip the bound",
                            spec.name
                        ),
                    }
                }
                let with = |v: Value| {
                    let mut p = call.clone();
                    p.insert(name.clone(), v);
                    Value::Object(p)
                };
                probed += 1;
                assert!(
                    dispatch(&e, spec.method, &with(json!(min))).is_ok(),
                    "schema says `{}`.{name} accepts {min}; the engine refuses it",
                    spec.name
                );
                let below = min - 1;
                assert!(
                    dispatch(&e, spec.method, &with(json!(below))).is_err(),
                    "schema forbids `{}`.{name} below {min}, but the engine accepts {below} — an \
                     agent is denied a call that works",
                    spec.name
                );
            }
        }
        assert!(
            probed > 0,
            "no numeric bound was probed; this guard has gone vacuous"
        );
    }

    /// Every method `dispatch::PARAMS` carries either has a tool or is listed
    /// in [`UNEXPOSED_METHODS`] with a reason. Both directions.
    ///
    /// The failure this guards is not a bug in any one call — it is a surface
    /// that drifts by omission. Ten of the methods on this table had gone
    /// unexposed without anybody ruling on it, and with the internal ones set
    /// aside they were the corrective or destructive half of a pair that WAS
    /// exposed: tag and no untag, block and no unblock, close and no reopen,
    /// write a memory and no way to retract it. Nothing failed while that was
    /// true, because adding a tool is an edit and leaving one out is silence.
    ///
    /// So silence stops being an option. A method added tomorrow must ship a
    /// tool or say why not, and a reason that stops being true — the method is
    /// exposed after all, or deleted — fails here rather than sitting on as a
    /// note nobody re-reads. The repo already runs this shape over the docs
    /// pages (`docs.rs`'s `VERBS`/`METHODS` guards); this is the same guard
    /// pointed at the tool table.
    #[test]
    fn every_dispatch_method_is_exposed_or_listed_as_deliberately_unexposed() {
        let exposed: Vec<&str> = tool_specs().into_iter().map(|s| s.method).collect();
        let methods: Vec<&str> = crate::dispatch::PARAMS.iter().map(|(m, _, _)| *m).collect();
        assert_eq!(
            exposure_faults(&methods, &exposed, UNEXPOSED_METHODS),
            Vec::<String>::new()
        );
    }

    /// The invariant itself, as a function of its three tables, so it can be
    /// run against inconsistent ones.
    ///
    /// Split out for exactly one reason: a guard asserted only over the real
    /// tables is a guard nobody has seen fail. Delete its assertions and the
    /// suite stays green, which is the failure mode this project already has a
    /// rule about — a test watched red is the only test whose behaviour is
    /// known. `every_dispatch_method_is_exposed_or_listed_as_deliberately_unexposed`
    /// drives it with the shipping tables; the fixtures below drive it with
    /// broken ones and assert it complains.
    fn exposure_faults(
        methods: &[&str],
        exposed: &[&str],
        unexposed: &[(&str, &str)],
    ) -> Vec<String> {
        let mut faults = Vec::new();
        for method in methods {
            let has_tool = exposed.contains(method);
            let listed = unexposed.iter().any(|(m, _)| m == method);
            if !has_tool && !listed {
                faults.push(format!(
                    "`{method}` is in PARAMS, has no MCP tool, and is not in \
                     UNEXPOSED_METHODS. Expose it, or add it there with the reason — an \
                     omission nobody wrote down is how this surface became additive-only \
                     in the first place"
                ));
            }
            if has_tool && listed {
                faults.push(format!(
                    "`{method}` is BOTH exposed and listed as deliberately unexposed; the \
                     reason beside it is now false and the next reader will believe it"
                ));
            }
        }
        for (method, reason) in unexposed {
            if !methods.contains(method) {
                faults.push(format!(
                    "UNEXPOSED_METHODS names `{method}`, which is not a dispatch method — a \
                     renamed or deleted method leaves a reason behind that reads as current"
                ));
            }
            if reason.len() <= 40 {
                faults.push(format!(
                    "`{method}` is excused in {} characters. A reason short enough to be a \
                     label is a label, and the point of this table is the argument",
                    reason.len()
                ));
            }
        }
        faults
    }

    /// Each way the exposure table can be wrong, proved to be caught.
    ///
    /// Without this, the guard above is a test of the current tables rather
    /// than a test of the rule: it passes today, it passes with its assertions
    /// deleted, and nobody finds out until a method ships unreachable again.
    #[test]
    fn the_exposure_guard_catches_every_way_the_tables_can_disagree() {
        let long = "a reason long enough to clear the floor this guard sets on excuses";

        // A method with neither a tool nor a written reason: the original
        // defect, and the one that arrives by doing nothing at all.
        let faults = exposure_faults(&["task.add", "task.reopen"], &["task.add"], &[]);
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert!(faults[0].contains("task.reopen"), "{faults:?}");

        // A reason that has stopped being true because the method was exposed.
        let faults = exposure_faults(&["task.add"], &["task.add"], &[("task.add", long)]);
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert!(faults[0].contains("BOTH exposed and listed"), "{faults:?}");

        // A reason left behind by a method that no longer exists.
        let faults = exposure_faults(&["task.add"], &["task.add"], &[("task.ghost", long)]);
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert!(faults[0].contains("not a dispatch method"), "{faults:?}");

        // An excuse short enough to be a label.
        let faults = exposure_faults(&["task.add"], &[], &[("task.add", "internal")]);
        assert_eq!(faults.len(), 1, "{faults:?}");
        assert!(
            faults[0].contains("short enough to be a label"),
            "{faults:?}"
        );

        // And consistent tables produce nothing, so the fixtures above are
        // failing for their own reason rather than because anything complains.
        assert_eq!(
            exposure_faults(
                &["task.add", "task.cancel"],
                &["task.add"],
                &[("task.cancel", long)]
            ),
            Vec::<String>::new()
        );
    }

    /// The response budget counts the notice the response adds.
    ///
    /// `fit_to_budget` measured the bare view and `tool_ok_view_only` appended
    /// the JSON-omission sentence afterwards, so a view landing within that
    /// sentence's length of the limit shipped over it — the payload bound
    /// defeated by the text explaining the payload bound. The window is
    /// narrower than any hand-written fixture would reliably hit, so the
    /// fixture is searched for.
    #[test]
    fn a_view_that_only_fits_without_its_own_notice_is_still_shrunk() {
        let notice = view_only_text("").len();
        assert!(
            notice > 0,
            "the notice must cost something to be worth guarding"
        );

        // Two annotations: an old fat one and a new small one, with the fat
        // body sized so the two-annotation view lands in the danger window —
        // under the budget on its own, over it once the notice is added.
        let target = RESPONSE_BUDGET_BYTES - notice / 2;
        let (mut lo, mut hi) = (1usize, RESPONSE_BUDGET_BYTES);
        let mut fixture = None;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            let e = engine();
            e.task_add(&json!({ "title": "boundary" })).expect("task");
            e.annotation_add(&json!({ "ref": 1, "body": "x".repeat(mid) }))
                .expect("the old fat one");
            e.annotation_add(&json!({ "ref": 1, "body": "the newest, and small" }))
                .expect("the new small one");
            let full = dispatch(&e, "task.get", &json!({ "ref": 1 })).expect("get");
            let len = crate::markdown::task_detail(&full, &iso_opts()).len();
            if len <= target {
                fixture = Some((mid, len));
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        let (body, view_len) = fixture.expect("a body size landing under the budget exists");
        assert!(
            view_len > RESPONSE_BUDGET_BYTES - notice,
            "the search did not reach the window this guards: view {view_len}, budget \
             {RESPONSE_BUDGET_BYTES}, notice {notice}"
        );

        let e = engine();
        e.task_add(&json!({ "title": "boundary" })).expect("task");
        e.annotation_add(&json!({ "ref": 1, "body": "x".repeat(body) }))
            .expect("the old fat one");
        e.annotation_add(&json!({ "ref": 1, "body": "the newest, and small" }))
            .expect("the new small one");
        let server = McpServer::new(&e, Scope::Read);
        let out = server
            .handle_message(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "tasqx_get_task", "arguments": { "ref": 1 } }
            }))
            .expect("tools/call is a request");
        let bytes: usize = out["result"]["result"]["content"]
            .as_array()
            .or_else(|| out["result"]["content"].as_array())
            .expect("content blocks")
            .iter()
            .map(|b| b["text"].as_str().unwrap_or("").len())
            .sum();
        assert!(
            bytes <= RESPONSE_BUDGET_BYTES,
            "the finished response is {bytes} bytes against a {RESPONSE_BUDGET_BYTES} budget: \
             the notice was added after the fit was decided"
        );
    }

    fn iso_opts() -> crate::markdown::DetailOpts {
        crate::markdown::DetailOpts {
            time: crate::markdown::TimeFormat::Iso,
            now: jiff::Timestamp::UNIX_EPOCH,
        }
    }

    /// A tool's properties must be EXACTLY the params its method accepts.
    ///
    /// A property the engine refuses is the loud half: the agent reads the
    /// schema, sends the argument, and gets a `bad_request` for doing what it
    /// was told. The silent half is the one this used to allow — a param the
    /// engine accepts and the schema omits is an option no agent ever tries, so
    /// nothing fails and the capability simply does not exist for AI callers.
    /// `tasqx_add_task` hid `scheduled`, `wait`, `recurrence` and `remind`
    /// that way, `tasqx_list_tasks` hid `fields`, `tasqx_summary` hid `all` and
    /// `tasqx_start_timer` hid `keep`; the subset-only version of this test was
    /// green throughout.
    ///
    /// Equality, therefore, and against `PARAMS` — the same table the dispatch
    /// gate enforces and that its own drift guard pins to the code that reads
    /// the keys. Exposing a method now means exposing all of it, or recording
    /// why not (D30: derive it, do not keep two lists in sync).
    #[test]
    fn every_tool_advertises_exactly_the_params_its_method_accepts() {
        for spec in tool_specs() {
            let (_, accepted, _) = crate::dispatch::PARAMS
                .iter()
                .find(|(m, _, _)| *m == spec.method)
                .unwrap_or_else(|| panic!("tool `{}` names an unlisted method", spec.name));
            let mut advertised: Vec<&str> = spec.schema["properties"]
                .as_object()
                .expect("an object schema")
                .keys()
                .map(String::as_str)
                .collect();
            advertised.sort_unstable();
            let mut expected: Vec<&str> = accepted.to_vec();
            expected.extend(
                TRANSPORT_ONLY_ARGS
                    .iter()
                    .filter(|(tool, _, _)| *tool == spec.name)
                    .map(|(_, arg, _)| *arg),
            );
            expected.sort_unstable();
            assert_eq!(
                advertised, expected,
                "tool `{}` and {} disagree about the argument set: a property the method refuses \
                 fails every call that uses it, and a param the schema omits is a capability no \
                 agent will ever discover. An argument this server consumes instead of \
                 forwarding belongs in TRANSPORT_ONLY_ARGS, with the reason it is not the \
                 method's business",
                spec.name, spec.method
            );
        }
    }

    /// `TRANSPORT_ONLY_ARGS` is read in both directions, so neither half can
    /// drift: an entry naming a tool or a property that no longer exists fails
    /// here, exactly as `UNEXPOSED_METHODS` fails when a method it excuses gets
    /// exposed after all. A reason under forty characters is refused for the
    /// same reason it is there: a reason short enough to be a label is a label.
    #[test]
    fn every_transport_only_argument_is_real_and_argued_for() {
        let specs = tool_specs();
        for (tool, arg, why) in TRANSPORT_ONLY_ARGS {
            let spec = specs
                .iter()
                .find(|s| s.name == *tool)
                .unwrap_or_else(|| panic!("TRANSPORT_ONLY_ARGS names an unlisted tool `{tool}`"));
            assert!(
                spec.schema["properties"]
                    .as_object()
                    .expect("an object schema")
                    .contains_key(*arg),
                "`{tool}` no longer advertises `{arg}`, so this excuse is stale"
            );
            let (_, accepted, _) = crate::dispatch::PARAMS
                .iter()
                .find(|(m, _, _)| *m == spec.method)
                .expect("a listed method");
            assert!(
                !accepted.contains(arg),
                "`{arg}` IS a param of {} now — forward it and drop this entry",
                spec.method
            );
            assert!(
                why.len() >= 40,
                "`{tool}.{arg}` needs a reason, not a label: {why:?}"
            );
        }
    }

    /// Each date field must say what it *does*, not only what it takes.
    ///
    /// `due`, `scheduled` and `wait` shared one description — the grammar
    /// sentence and nothing else — so all three read identically to an agent
    /// choosing between them. An MCP client picked `scheduled` for "check back
    /// in four weeks" and put the work in `backlog`, invisible to `@working`
    /// until late September; it noticed only by reading `status` back out of
    /// the response. A field whose effect is discoverable only by inspecting
    /// what it did is this repo's recurring defect, one layer earlier.
    ///
    /// The shared grammar stays (D33: three sentences were three chances to
    /// advertise three grammars for one parser). What is asserted here is that
    /// the effect clause is *added* to it and differs per field.
    #[test]
    fn every_date_field_names_its_own_effect_beside_the_shared_grammar() {
        let specs = tool_specs();
        let add = specs
            .iter()
            .find(|s| s.name == "tasqx_add_task")
            .expect("tasqx_add_task");
        let describe = |field: &str| {
            add.schema["properties"][field]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("`{field}` has no description"))
                .to_string()
        };
        let (due, scheduled, wait) = (describe("due"), describe("scheduled"), describe("wait"));

        for (field, text) in [("due", &due), ("scheduled", &scheduled), ("wait", &wait)] {
            assert!(
                text.contains(WHEN_GRAMMAR),
                "`{field}` dropped the shared grammar sentence: D33 exists because three \
                 hand-written grammars drifted into three different claims about one parser"
            );
            assert!(
                text.len() > WHEN_GRAMMAR.len(),
                "`{field}` says what it takes and not what it does — the grammar alone fits \
                 all three fields, which is exactly why a caller cannot choose between them"
            );
        }
        assert_ne!(due, scheduled, "`due` and `scheduled` read identically");
        assert_ne!(due, wait, "`due` and `wait` read identically");
        assert_ne!(scheduled, wait, "`scheduled` and `wait` read identically");

        // The two that move a task out of the working set must say so by name,
        // because `backlog` is the observable the client had to reverse-engineer.
        for (field, text) in [("scheduled", &scheduled), ("wait", &wait)] {
            assert!(
                text.contains("backlog"),
                "`{field}` holds a task in backlog until it passes \
                 (`types::effective_status`) and never says so"
            );
        }
        assert!(
            due.contains("urgency"),
            "`due` drives the urgency score and anchors relative reminders; the description \
             names neither"
        );
        assert!(
            add.description.contains("backlog"),
            "`tasqx_add_task` returns `status: \"backlog\"` for a future `scheduled` or `wait` \
             and its description never warns that a date can park the task"
        );
    }
}
