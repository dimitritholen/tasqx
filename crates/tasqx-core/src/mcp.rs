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
    shell_quote, Engine, MEMORY_SCOPES, OUTCOME_METRICS, SORT_KEYS, SUMMARY_GROUP_BY,
    SUMMARY_METRICS, TASK_FIELDS,
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
/// The arguments as they will be dispatched, plus the transport decisions
/// taken while rewriting them — the facts `present` needs to fit the answer.
struct PreparedCall {
    args: Value,
    /// Whether the `task.get`/`task.brief` answer carries the machine block
    /// beside the rendered view. False unless the caller said otherwise
    /// (D151): the model reads the view, and the JSON is the same result
    /// again. It also decides what the budget spends first and whether an
    /// over-budget answer explains itself (D49/D66).
    include_json: bool,
    /// D146: which rendering the one rendered block is spelled in.
    view: View,
    /// Whether the `annotation.add` answer echoes the stored body back
    /// (D72/D75's default) or a `body_bytes` length in its place.
    include_body: bool,
    /// Whether THIS transport supplied the `task.list` page.
    paged_list_by_us: bool,
    /// Whether THIS transport supplied the `task.list` projection (D152).
    /// When it did — and only then — a null-valued key is dropped from every
    /// row on the way out, because a default row nobody asked for should not
    /// spend bytes saying a field is unset.
    fields_defaulted_by_us: bool,
}

/// Which of a task's two human renderings the rendered block carries (D146).
///
/// Transport-only, beside `include_json` and for the same reason: the engine
/// returns one result, and this decides only how it is spelled on the way out.
/// [`View::Markdown`] is the default because the reader of a tool result is the
/// model. [`View::Card`] is for the one moment it is not — an agent handing the
/// task to a PERSON, where a fixed-geometry box survives being pasted into a
/// chat reply or a pull request and a markdown table reflows into the prose
/// around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// `markdown::task_detail` / `markdown::task_brief` — the table views, and
    /// what a caller that says nothing has always got.
    Markdown,
    /// D146's 72-column box card, inside a `text` fence.
    Card,
}

/// What `dispatch` hands back, named so the prepare/dispatch/present seam
/// reads as the pipeline it is.
type DispatchOutcome = Result<Value, crate::error::ApiError>;

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
    \"friday\", \"2026-07-20\", \"in 3 days\", \"eom\", or \"2026-07-20T17:00\" (a clock is UTC unless it carries an offset).";

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

/// How many BYTES of one annotation body `tasqx_get_task`/`tasqx_brief_task`
/// carry when the caller names no `max_body_bytes` (D148).
///
/// Bytes, because [`ANNOTATION_PAGE`] bounds rows and rows were never the unit
/// the problem is expressed in — and this is the half of that sentence D63 left
/// unfinished. A page bounds how MANY notes come back and can say nothing about
/// how big ONE of them is, so [`Self::fit_to_budget`]'s floor of one whole
/// annotation was only a floor while an annotation was small. Field report:
/// task #609 holds a single 240,000-byte note, and `tasqx_get_task` answered
/// ~240 KB at every page size from 20 down to 1, ten times
/// [`RESPONSE_BUDGET_BYTES`].
///
/// 16,384 is two thirds of that budget: one capped body plus the task table,
/// the heading and the omission notice fits a view-only response, so the
/// bisection floor is now under budget by construction rather than by luck. It
/// is also comfortably above the annotations this project actually writes — the
/// ones that provoked D63 were ~6 KB — so an ordinary note is never cut.
///
/// **Writes are never capped.** `annotation.add` stores every byte it is given
/// and echoes them back (D72/D75); this is a bound on one RESPONSE, and the cut
/// is marked with the original size and the exact call that reads the note
/// whole. A write cap would silently shorten a note the caller believed was
/// stored, which is a different and much worse failure — and it would surprise
/// the CLI, which has no payload limit at all.
const ANNOTATION_BODY_CAP: u64 = 16_384;

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
///
/// Re-exported from [`crate::engine::task::DEFAULT_TASK_LIST_LIMIT`] rather
/// than kept as a second literal (D110): this transport's own default-insertion
/// below still exists — it is what drives [`Self::fit_list_to_budget`]'s
/// byte-shrink, which `task_list` itself has no notion of — but the NUMBER is
/// now decided once, at the engine, so the CLI and `tasqx api` share it
/// instead of falling back to "no limit" behind this transport's back.
const LIST_PAGE: u64 = crate::engine::task::DEFAULT_TASK_LIST_LIMIT;

/// The fields one `tasqx_list_tasks` row carries when the caller names none
/// (D152).
///
/// Measured, like [`LIST_PAGE`]: re-measured 2026-09-16 over 36 hours and 18
/// sessions of this repo, `tasqx_list_tasks` sent 54 KB over 19 calls and not
/// one of them passed `fields`. A single `project:tasqx @working` page was 41
/// rows of 637 bytes with 22 keys each, most of them null — every row spelled
/// `scheduled`, `wait`, `remind`, `recurrence`, `completed`, `active_since`
/// and `budget_tokens` to say nothing about any of them.
///
/// These nine are what an agent PICKING WORK reads: which task, what it is,
/// where it stands, how it ranks, whether it can be started, when it is due,
/// whose it is. Everything else is still one explicit `fields` away, and
/// `fields: []` — the engine's own "no restriction" (#76.1) — is the whole
/// row. The narrowing lives here and not in `task_list` for D63's reason: the
/// bound belongs to the transport that has a payload limit, so the CLI and
/// `tasqx api` keep answering whole rows.
const LIST_DEFAULT_FIELDS: &[&str] = &[
    "short_id", "title", "status", "priority", "urgency", "blocked", "due", "project", "tags",
];

/// The size a `tasqx_get_task` response is shrunk to fit, counting BOTH content
/// blocks — the rendered view and the JSON behind it, which D49 ships together.
///
/// Measured against the failure rather than chosen: the reported response was
/// ~58 KB and exceeded a client's tool-output limit, and its JSON half alone
/// measured ~29 KB on the live store. A budget under that half is what makes the
/// first, uninstructed call fit, which matters because a client that hard-fails
/// on an oversized result never gets to retry with a smaller page.
///
/// It is a budget, not a guarantee — but D148 closed the hole that made it
/// nearly one. The floor is still one whole annotation, and one annotation is
/// now bounded too: [`ANNOTATION_BODY_CAP`] cuts an oversized body with a
/// marker naming its real size and the call that reads it whole, so the floor
/// exceeds this number only when a caller raised `max_body_bytes` on purpose.
/// What D63 refused was an UNMARKED cut, and it was right to.
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

/// The `max_body_bytes` property, identical on `tasqx_get_task` and
/// `tasqx_brief_task` (D148).
///
/// One function for [`WHEN_GRAMMAR`]'s reason: the two tools take the same
/// argument with the same meaning and the same default, and two hand-copies of
/// that sentence are two chances to advertise two different rules for one
/// parameter — which is how `annotations_limit`'s description came to describe
/// a budget exemption the server had stopped honouring.
fn max_body_bytes_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": 0,
        "description": format!(
            "Cap each annotation body at this many BYTES **in the response**. Default \
             {ANNOTATION_BODY_CAP}. A longer body is cut on a character boundary and marked \
             with its real size and the exact call that reads it whole, so nothing is \
             silently altered; a body that fits is returned untouched and unmarked. This is \
             what keeps one enormous note inside the response budget, which `annotations_limit` \
             cannot do — a page bounds how MANY notes come back, never how big one of them is. \
             Raise it deliberately to read one long note in full, together with \
             `annotations_limit: 1`; that answer may exceed the budget, which is the point of \
             asking. Stored text is never capped: this bounds one response, not the store."
        )
    })
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
/// `tasqx_complete_task` must describe the same three params identically
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
        "otlp.status",
        "an operator diagnostic for a machine-local, opt-in receiver (#18) — is telemetry \
         reaching THIS daemon on THIS machine — not a fact about any task an agent is \
         working. `tasqx config store` / a CLI verb is where a human checks it (#222).",
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
        "token.add",
        "a measurement after the fact, and D50 makes the completion's self-report the primary \
         channel precisely so one task never mixes channels. An agent with a count to report \
         has `tasqx_complete_task`.",
    ),
    (
        "token.remove",
        "the corrective half of `token.add` (#210), and the same reasoning keeps it off: an \
         agent that reports a wrong count fixes it by reporting the right one, and a removal an \
         agent could reach unsupervised on the ledger a lead reads for budget decisions is a \
         bigger foot-gun than the gap it closes. A human runs `tasqx api token.remove` instead.",
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
const TRANSPORT_ONLY_ARGS: &[(&str, &str, &str)] = &[
    (
        "tasqx_get_task",
        "include_json",
        "whether the response carries the machine-readable block beside the rendered view.      The two blocks are the same result twice (D49), so on a task whose bulk is annotation      prose the second is that prose again — 54% of a 6.4 KB response for ONE annotation,      66% for a task read with `annotations_limit: 0`. Since D151 the default is FALSE:      the reader of a tool result is the model, which reads the view, and the duplicate is      opt-in for the script that parses it. `task.get` has no opinion on how many blocks      its answer is wrapped in.",
    ),
    (
        "tasqx_brief_task",
        "include_json",
        "whether the response carries the machine-readable block beside the rendered view.      Same argument, same reason and same default as `tasqx_get_task`'s — false since D151,      because the caller reading this is the model and the model reads the view. The two      blocks are one result twice, and a brief's second block is the larger of the pair      because it carries the neighbourhood and the memory snippets as well. `task.brief`      has no opinion on how many blocks its answer is wrapped in.",
    ),
    (
        "tasqx_get_task",
        "view",
        "which of the two human renderings the rendered block is spelled in (D146).      A card is a DOCUMENT — fixed 72-column geometry, no escape codes — because it is      pasted in front of a person, into a chat reply or a pull request, where a markdown      table reflows into the prose around it. Both views render the SAME `task.get`      result: nothing here reaches the store, and `check_params` would refuse the key.",
    ),
    (
        "tasqx_brief_task",
        "view",
        "which of the two human renderings the task half of the brief is spelled in.      Same argument and same default as `tasqx_get_task`'s (D146), and only the task half      moves: what the prerequisites decided and what the store remembers follow the card      as the markdown they already were, because the choice is about the TASK and not      about its neighbourhood. `task.brief` has no opinion on how its answer is spelled.",
    ),
    (
        "tasqx_complete_task",
        "view",
        "whether the response leads with the D146 box card of the task AS COMPLETED (D153).      The closing card a person reads after a completion used to cost a second      `tasqx_get_task` per task — 26 get_task calls against 14 completions over 36 hours      of transcripts — because `task.done`'s frozen result carries no task to render. So      the transport reads the task back itself and spells it ahead of the JSON. `task.done`      has no opinion on how its answer is wrapped.",
    ),
    (
        "tasqx_annotate_task",
        "include_body",
        "whether the response echoes the annotation body back beside its id and timestamp.      D72/D75 keep the echo ON by default — it is the caller's only evidence that a body      promised to be stored verbatim really was — so this is opt-OUT, not a reversal: a      caller who already holds every byte it sent (the common case for a long note) can      decline paying to receive them again, and one that wants the verbatim proof still      gets it by doing nothing. `annotation.add` has no opinion on how its own result is      echoed back over one particular transport.",
    ),
];

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
                `depends_on` with `fields` to see what a blocked row is waiting on. Two \
                CLI-only recipes worth composing here: the single highest-urgency \
                unblocked task (\"what now\") is `filter: \"@working\", sort: \
                [\"-urgency\"], limit: 1`; \"what was I doing\" is `filter: \
                \"status:active\"`. Rows are narrowed to a default set of fields \
                unless you name your own; `fields: []` returns every column.",
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
                             is null once nothing is left. A limit you name is honoured up to \
                             {max_limit}, the ceiling the core itself clamps to.",
                            max_limit = crate::engine::task::MAX_TASK_LIST_LIMIT
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
                        "description": format!(
                            "Restrict each row to these fields. An unknown name is rejected, not \
                             ignored. Omit it and each row carries {default} and nothing else, \
                             with any key whose value is null left out; send \"fields\": [] for \
                             the whole row, or name what you need (e.g. \"depends_on\", \
                             \"estimate\", \"tokens\").",
                            default = LIST_DEFAULT_FIELDS.join(", ")
                        )
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
                             `annotations_total` from a previous response to ask for every \
                             one. The response byte budget applies to EVERY answer, whatever \
                             you name here: a page too big for it is cut to the largest page \
                             that fits and the rendered view says how much it left out and \
                             which `annotations_offset` reads the rest. By default the whole \
                             budget goes on history; `include_json: true` spends it on the \
                             duplicate machine-readable block first, so a big page and that \
                             flag together buy less history. 0 returns none, which is how you \
                             read a task's fields without its history."
                        )
                    },
                    "max_body_bytes": max_body_bytes_schema(),
                    "include_json": {
                        "type": "boolean",
                        "description": "Send the machine-readable JSON block as well as the \
                             rendered view. Default FALSE: you get the rendered view alone, \
                             because the two blocks are the same result twice and on a task \
                             whose bulk is annotation prose the JSON is that prose again — \
                             measured at 54% of a 6.4 KB response for one annotation, and 66% \
                             for a task read with `annotations_limit: 0`. Send true when a \
                             SCRIPT is going to parse the result rather than read it."
                    },
                    "view": {
                        "type": "string",
                        "enum": enum_of(["markdown", "card"]),
                        "description": "How the rendered block is spelled. Default \"markdown\", \
                             the view YOU read. \"card\" is the D146 box card: a fixed \
                             72-column box-drawn summary inside a text code fence, for the \
                             moment you hand this task to a PERSON — pasted into a chat reply \
                             or a pull request it keeps its shape, where a markdown table \
                             reflows into the prose around it. It is a SUMMARY: the detail \
                             view carries the annotation bodies and the card quotes only the \
                             first one. The JSON block is unchanged under either view — \
                             `include_json: true` still decides whether it is sent, and the \
                             response budget still spends it first."
                    },
                    "annotations_offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How many annotations to skip, counted back from the \
                            newest. Pass the `annotations_next_offset` of the previous response \
                            to walk further into the history; that field is null once there is \
                            nothing older."
                    },
                    "explain": {
                        "type": "boolean",
                        "description": "Add `urgency_breakdown` (`priority`, `due_proximity`, \
                            `age`, `total`) — the terms `urgency` sums (D1). Default false: an \
                            agent deciding whether to trust or override the ranking otherwise \
                            gets the one number it already had and no way to see the arithmetic \
                            behind it (#150)."
                    }
                },
                "required": ["ref"]
            }),
        },
        // D136. Read-scoped like every other orientation tool, and deliberately
        // NOT a flag on `tasqx_get_task`: the two answer different questions
        // ("what is this task" vs "what do I need before starting it"), and a
        // flag that swings one result between two shapes is a method with two
        // shapes and one name.
        ToolSpec {
            name: "tasqx_brief_task",
            method: "task.brief",
            write: false,
            destructive: false,
            idempotent: true,
            description: "Everything you need before starting one task, in ONE call: the task \
                itself, the tasks it depends on with what each of THEM concluded, what it \
                blocks, and relevant memory — under a query tasqx derives from the task's own \
                title, tags and project, so you do not have to guess search terms. Prefer this \
                over get_task + search_memory when you are about to START work; use get_task \
                when you only need the task. Pure read, no side effects.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": { "type": ["integer", "string"], "description": "Task short_id or UUID." },
                    "memory_limit": {
                        "type": "integer",
                        "description": format!(
                            "How many memory hits to return. Optional; defaults to {}. Half \
                             the slots (rounded up) are reserved for knowledge docs — the \
                             rulings a sibling task's notes would otherwise outrank — and \
                             annotations fill the rest, either kind taking the other's unused \
                             slots. The response says what it did in `reserved_docs`, \
                             `docs_total` and `annotations_total`.",
                            crate::engine::MEMORY_SEARCH_LIMIT
                        )
                    },
                    "max_body_bytes": max_body_bytes_schema(),
                    "include_json": {
                        "type": "boolean",
                        "description": "Send the machine-readable JSON block as well as the \
                             rendered view. Default FALSE: you get the rendered view alone. \
                             The two blocks are the same result twice, and a brief's JSON half \
                             is the bigger one — it carries the neighbourhood and every memory \
                             snippet again. Pass true when a script needs the brief as JSON."
                    },
                    "view": {
                        "type": "string",
                        "enum": enum_of(["markdown", "card"]),
                        "description": "How the task half of the brief is spelled. Default \
                             \"markdown\", the view YOU read. \"card\" is the D146 box card: a \
                             fixed 72-column box-drawn summary inside a text code fence, for \
                             the moment you hand this task to a PERSON — pasted into a chat \
                             reply or a pull request it keeps its shape. Only the task half \
                             changes: what the prerequisites concluded and the memory hits \
                             follow the card as the same markdown either way. The JSON block \
                             is unchanged under either view — `include_json: true` still \
                             decides whether it is sent, and the response budget still spends \
                             it first."
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
                        },
                        "description": "Extra columns per group. Omit and each group carries \
                             only `count` — `tracked_total`, `overdue` and the token buckets are \
                             NOT included unless named here, unlike `tasqx report`, which shows \
                             every metric by default."
                    },
                    "since": {
                        "type": "string",
                        "description": "Window `tracked_total` and the token buckets to what happened at or after this instant — a task tracking time or logging a measurement — rather than the task's lifetime total. Independent of `filter`'s `completed.after:`/`completed.before:`, which selects tasks by completion date instead (D97). Same date grammar as `due`/`completed` (relative words, offsets, RFC3339)."
                    },
                    "until": {
                        "type": "string",
                        "description": "The other end of `since`'s window: excludes anything at or after this instant. `since`/`until` together window WHEN the spend happened, e.g. \"what did this project cost this week\" — the fix for a report that used to answer that question by task completion date and silently missed spend on tasks that hadn't closed."
                    }
                }
            }),
        },
        // Read-scoped on purpose (D137), for the reason `tasqx_search_memory`
        // is: an agent with no write access should still be able to see its own
        // record. A retrospective that can cite the project's rework rate is
        // answering from evidence rather than from its memory of the session,
        // which is the failure mode `docs/guides/self-improving-agent.md`
        // already names.
        ToolSpec {
            name: "tasqx_outcomes",
            method: "report.outcomes",
            write: false,
            destructive: false,
            idempotent: true,
            description: "How the work went, rather than what it cost: rework (completions that \
                were reopened), estimate calibration, token cost, completions with no annotation, \
                started-then-cancelled work, and `forced` (completions that overrode still-open \
                blockers). Every rate comes back beside the `n` it was \
                computed over — a rate over three completions and one over ninety are different \
                claims. Scope is tasks that CLOSED: a task still open is not an outcome yet, and \
                `tasqx_summary` is the read for work in flight. Pure read, no side effects.",
            schema: json!({
                "type": "object",
                "properties": {
                    "group_by": {
                        "type": "string",
                        "enum": enum_of(SUMMARY_GROUP_BY),
                        "description": format!("Grouping axis. Optional; defaults to {}.", SUMMARY_GROUP_BY[0])
                    },
                    "filter": { "type": "string", "description": "Optional filter DSL to scope the report — the same grammar `tasqx_list_tasks` takes." },
                    "metrics": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": enum_of(OUTCOME_METRICS)
                        },
                        "description": "Which metrics to emit. Omit for ALL of them, which is \
                             the intended reading: a rework rate with no cost beside it invites \
                             the wrong fix. Unlike `tasqx_summary`, naming none does not mean a \
                             bare count."
                    },
                    "since": {
                        "type": "string",
                        "description": "Only count tasks that closed at or after this instant. \
                             `since`/`until` window WHEN THE TASK CLOSED — its `done`, or its \
                             cancellation — which is a different axis from `tasqx_summary`'s \
                             window over when spend happened. Same date grammar as `due` \
                             (relative words, offsets, RFC3339; a bare clock time is UTC)."
                    },
                    "until": {
                        "type": "string",
                        "description": "The other end of `since`'s window: excludes tasks that closed at or after this instant."
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
                (prefix*, AND/OR, column filters). Every word of a query is matched \
                by its STEM, so \"reviewing\" finds a doc that only says \"review\" or \
                \"reviewed\". A hit carries a short excerpt: \
                read a doc whole with `tasqx_get_memory` on its `id`, and an \
                annotation whole with `tasqx_get_task` on the task its `source` \
                names. Every word of a plain query is REQUIRED, so `matched` on the \
                result is what explains a zero-hit answer. A hit's `rank` is the raw \
                FTS5 bm25 score: LOWER (more negative) is a BETTER match, the opposite \
                of most scoring conventions. `hits` is already sorted best-first, so \
                `rank` is for comparing hits against each other, not for a fixed \
                threshold. The response carries `total` (every row the query matched, \
                before `limit` truncates) and `has_more`, so a hit list that looks \
                complete is never mistaken for one that is.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Plain-text search words. Matched as quoted phrases, so hyphens and dots are safe."
                    },
                    // No `minimum` bound: `query` is required, so the one-key
                    // boundary probe in the minimum guard could never test it.
                    "limit": { "type": "integer", "description": "Max hits (0 or more); default 10. The response's `total`/`has_more` say what this left out." },
                    "scope": {
                        "type": "string",
                        "enum": enum_of(MEMORY_SCOPES),
                        "description": format!("What to search. Optional; defaults to {}.", MEMORY_SCOPES[0])
                    },
                    "raw": {
                        "type": "boolean",
                        "description": "Pass the query through as FTS5 syntax. Invalid syntax is refused as bad_request."
                    },
                    "project": {
                        "type": "string",
                        "description": "Scope to one project: docs stored with this `project`, and annotations whose task carries it. Omit to search across every project (and unscoped docs)."
                    },
                    "include_unscoped": {
                        "type": "boolean",
                        "description": "Widen a `project` scope to also return docs and \
                             annotations belonging to NO project — which is what \
                             `tasqx memory import` produces, so a strict scope hides every \
                             imported ADR. Never admits ANOTHER project's documents. Needs \
                             `project`; without one there is nothing to widen from and it is \
                             refused."
                    }
                },
                "required": ["query"]
            }),
        },
        ToolSpec {
            name: "tasqx_list_memory",
            method: "memory.list",
            write: false,
            destructive: false,
            idempotent: true,
            description: "Browse memory docs without already knowing a word inside one — the \
                enumeration `tasqx_search_memory` cannot do without a query. Newest-modified \
                first by default. Rows come back in pages: the response carries `count` \
                (returned), `total` (matched) and `next_offset`, null once nothing is left — the \
                same shape `tasqx_list_tasks` uses. Each row carries a short `body_preview`, not \
                the full body; read one whole with `tasqx_get_memory` on its `id`.",
            schema: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How many rows to return. Omit for every doc from `offset` on."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How many matching docs to skip. Pass the previous response's `next_offset` to keep walking."
                    },
                    "project": {
                        "type": "string",
                        "description": "Restrict to docs stored with this `project`. Omit to list every doc regardless of project."
                    }
                }
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
            description: "Create a new task. Returns its short_id, urgency, status — which is \
                `backlog`, not `pending`, when `scheduled` or `wait` is in the future, and a \
                backlog task is outside the `@working` set until that date passes — plus the \
                stored title, due, tags and resolved scheduled, so a title containing `due:`, \
                `project:`, `est:` or similar text can be checked for accidental inline-sugar \
                capture, and an ambiguous date like `in 3 days` or `friday` can be checked \
                against what was actually stored, rather than assumed unchanged.",
            schema: json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Stored verbatim — this server does not parse the \
                            CLI's inline sugar (`+tag`, `project:`, `due:`, `!prio`). A title \
                            like \"fix it +bug due:friday\" is saved with those characters in \
                            it; use the `tags`/`project`/`due`/`priority` fields below instead."
                    },
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
                    },
                    "budget_tokens": {
                        "type": "integer",
                        "description": "Token budget (D139): a size gauge over FRESH tokens \
                            (input + output + cache creation; cache reads are not counted, \
                            because a budget dominated by them measures re-reading rather than \
                            work). It STOPS NOTHING — tasqx is a store you call between turns \
                            and cannot preempt one — so read it as a signal: a task that blows \
                            its budget was usually too big to hand over whole. `task.get` and \
                            `task.brief` report `fresh_tokens` and `over` against it."
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
            description: "Change fields on a task via a `set` map. Returns `{short_id, _rev, \
                set}`, where `set` echoes the RESOLVED value actually stored for each field \
                this call named — an ambiguous `due:\"friday\"` or `estimate:\"90m\"` comes \
                back as the instant/duration it parsed to, so a write can be checked without \
                a follow-up `tasqx_get_task`. Optimistic concurrency \
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
                (session_id, transcript_path, client) land on that same event; \
                without a self-report, log-parse attribution is a fallback that refuses \
                samples claimed by more than one task's window. A task whose dependencies \
                are still open is refused with `conflict` naming the blockers; pass \
                `force: true` to complete it anyway — the override is recorded and \
                `tasqx_outcomes` counts it. Pass `view: \"card\"` when a person is about to \
                read what shipped: the closing card of the completed task leads the response \
                and no follow-up `tasqx_get_task` is needed.",
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
                    },
                    "checks_passed": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Ids of the acceptance criteria this work proved (D138). \
                            Completing with criteria still open is NOT refused — nothing is \
                            blocked — but it is counted as an unproven completion by \
                            `tasqx_outcomes`, so mark what you actually proved."
                    },
                    "evidence": {
                        "type": "string",
                        "description": "One citation covering the `checks_passed` above: a test \
                            name, an excerpt of output, a commit sha. Stored verbatim."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "Complete the task even though some of its dependencies \
                            are still open. Without it, a task with open blockers is refused \
                            with `conflict` naming them (D150). The override is recorded on the \
                            completion event and `tasqx_outcomes` counts it under `forced`."
                    },
                    "view": {
                        "type": "string",
                        "enum": enum_of(["markdown", "card"]),
                        "description": "Default \"markdown\": the plain JSON result. \"card\" \
                            answers the D146 box card of the task AS COMPLETED — Delivered row, \
                            checks ticked or left open — inside a text code fence, as the first \
                            block, with the JSON result unchanged behind it. For the moment you \
                            show a PERSON what shipped; it replaces the tasqx_get_task re-read \
                            that used to follow every completion (D153)."
                    }
                },
                "required": ["ref"]
            })),
        },
        ToolSpec {
            name: "tasqx_cancel_task",
            method: "task.cancel",
            write: true,
            destructive: true,
            idempotent: false,
            // D114 narrows D67's UNEXPOSED_METHODS placement of task.cancel:
            // task_modify status:cancelled already reached this, but its
            // schema names no status enum, so the reachable path was
            // reachable in principle and undiscoverable in practice.
            description: "Cancel a task: backlog, pending or active moves to cancelled. The row \
                is kept, not deleted, and stops counting in reports. Returns any tasks newly \
                unblocked by the cancellation (D11: cancelling a blocker resolves it, same as \
                completing one). A task already done or cancelled is a conflict, not a no-op.",
            schema: json!({
                "type": "object",
                "properties": { "ref": ref_schema() },
                "required": ["ref"]
            }),
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
                params (session_id, transcript_path, client) are recorded on \
                the start event for token attribution.",
            schema: with_correlation(json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "keep": {
                        "type": "boolean",
                        "description": "Keep other active tasks running (opt out of single-active)."
                    },
                    "actor": {
                        "type": "string",
                        "description": "Who is asking for the clock (D140). Filled in \
                            automatically with this connection's id when omitted, so you do not \
                            normally pass it — supply your own only if you have a stable agent \
                            identity worth recording. Starting a task while ANOTHER actor holds \
                            an active clock is refused with `conflict` rather than silently \
                            stopping their timer and leaving their work untracked; pass \
                            keep:true to run both deliberately."
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
            description: "Stop the timer on a task. Returns `interval` (the duration just \
                closed) and `tracked` (the task's running total, matching `tasqx_get_task`).",
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
                A tag the task does not carry is `not_found` and removes NONE of the tags \
                named — all or nothing, so a typo can never answer ok.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["ref", "tags"]
            }),
        },
        // D138. Write-scoped; `check_add` is append-only so it is not
        // destructive, while `set` overwrites a state and `remove` drops a row.
        ToolSpec {
            name: "tasqx_add_check",
            method: "check.add",
            write: true,
            destructive: false,
            idempotent: false,
            description: "Add an acceptance criterion to a task — one thing that must be true \
                for the task to count as done. tasqx NEVER RUNS a check: it is a claim you mark \
                later with evidence, not a command. Use these instead of burying criteria in an \
                annotation, where nothing can ask at completion time whether they were met.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "body": { "type": "string", "description": "The criterion, in your own words. Stored verbatim." }
                },
                "required": ["ref", "body"]
            }),
        },
        ToolSpec {
            name: "tasqx_set_check",
            method: "check.set",
            write: true,
            destructive: true,
            idempotent: true,
            description: "Mark one acceptance criterion passed or failed, with the evidence for \
                it. `failed` is a normal outcome, not an error — recording that a criterion was \
                NOT met is useful. Evidence is stored verbatim and never interpreted.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "check_id": { "type": "string", "description": "The check's id, from `tasqx_get_task` or the add result." },
                    "state": {
                        "type": "string",
                        "enum": enum_of(crate::engine::CHECK_STATES),
                        "description": "open | passed | failed."
                    },
                    "evidence": {
                        "type": "string",
                        "description": "Your proof: a test name, an excerpt of command output, a \
                            commit sha. Optional — some criteria are met by something nobody can \
                            quote, and inventing a citation is worse than an unproven `passed`."
                    }
                },
                "required": ["ref", "check_id", "state"]
            }),
        },
        ToolSpec {
            name: "tasqx_remove_check",
            method: "check.remove",
            write: true,
            destructive: true,
            idempotent: true,
            description: "Drop an acceptance criterion that turned out to be the wrong thing to \
                ask. Use `tasqx_set_check` with `failed` when the criterion was right and the \
                work did not meet it — deleting it there would hide the finding.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "check_id": { "type": "string", "description": "The check's id." }
                },
                "required": ["ref", "check_id"]
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
                implementation notes. The response echoes the stored body back \
                by default — proof the store kept it verbatim — but you \
                already hold every byte you sent, so pass `include_body: \
                false` to get `{id, created, body_bytes}` instead on a long \
                note. Wrote something you should not have — a token, a \
                customer name, a wrong root cause? `tasqx_remove_annotation` \
                takes the `id` this response returns and scrubs it; there is \
                otherwise no way to take an annotation back.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "body": { "type": "string", "description": "Note text, stored verbatim. Multi-line markdown is fine." },
                    "include_body": {
                        "type": "boolean",
                        "description": "Echo the stored body back in the response. Default \
                             true. A long note costs its own bytes twice — once in the \
                             request, once in this echo — so pass false to get `body_bytes` \
                             (a length) in place of `body` when you do not need the \
                             verbatim-storage proof."
                    }
                },
                "required": ["ref", "body"]
            }),
        },
        ToolSpec {
            name: "tasqx_remove_annotation",
            method: "annotation.remove",
            write: true,
            destructive: true,
            idempotent: true,
            // D113. Stated for the same reason `tasqx_remove_memory`'s
            // description states its own permanence: it is the one property of
            // this tool a caller cannot learn by trying.
            description: "Permanently scrub one annotation's text by id — the \
                id `tasqx_annotate_task` or `tasqx_get_task` reports for it. \
                Unlike every other write on this server, this is a HARD delete: \
                the body is overwritten in the store, not merely hidden, so a \
                secret pasted into a note by mistake is actually gone from the \
                file, not just gone from what tasqx shows you — this covers \
                `event.list` and `store.export` too, by redacting the original \
                `annotation.add` event's own body field in the same \
                transaction (D113), not only the `annotations` row. A \
                tombstone (the id and when it was removed) stays for audit, \
                with no text in it: `tasqx_get_task` lists it as \
                `annotations_removed` and prints one line per tombstone under \
                the annotations, so a note that went on purpose does not read \
                as one that quietly vanished. `tasqx undo` does NOT cover this — there \
                is nothing left in the log to restore — so double-check the id \
                before calling. An unknown or already-removed id is \
                `not_found`.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ref": ref_schema(),
                    "annotation_id": {
                        "type": "string",
                        "description": "The annotation's id, as tasqx_annotate_task or \
                            tasqx_get_task reports it."
                    }
                },
                "required": ["ref", "annotation_id"]
            }),
        },
        ToolSpec {
            name: "tasqx_add_dependency",
            method: "dependency.add",
            write: true,
            destructive: false,
            idempotent: true,
            description: "Make one task depend on another: `ref` is blocked \
                until `depends_on` is done or cancelled; a `ref` that is itself \
                already closed is never blocked. Returns the resulting \
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
                    "source": { "type": "string", "description": "Where this came from: a path, URL, or ticket." },
                    "project": { "type": "string", "description": "Optional project scope. Omit to leave the doc global/unscoped — it is not defaulted onto whatever project is current." }
                },
                "required": ["title", "body"]
            }),
        },
        ToolSpec {
            name: "tasqx_update_memory",
            method: "memory.update",
            write: true,
            destructive: true,
            idempotent: false,
            description: "Correct a memory doc in place: title, body, source and/or project, by \
                id. Use it instead of `tasqx_add_memory` for a correction — a fix written as a \
                second doc leaves the stale one polluting search rankings forever, and \
                `tasqx_remove_memory` is permanent, so delete-then-recreate risks losing the doc \
                with nothing to put it back. Optimistic concurrency is the same shape \
                `tasqx_modify_task` uses: when `expected_rev` is omitted, this server reads the \
                doc's current `rev` and pins it, so a concurrent edit yields a `conflict` naming \
                both revs instead of a silent overwrite. On `conflict`: re-read the doc \
                (`tasqx_get_memory`), re-apply the change, and retry with the fresh `rev`.",
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "The doc UUID, as printed by a search hit, `tasqx_add_memory`, or `tasqx_list_memory`."
                    },
                    "title": { "type": "string", "description": "New title. Omit to leave it unchanged." },
                    "body": { "type": "string", "description": "New body, stored verbatim. Omit to leave it unchanged." },
                    "source": { "type": "string", "description": "New source. Omit to leave it unchanged." },
                    "project": { "type": "string", "description": "New project scope. Omit to leave it unchanged." },
                    "expected_rev": { "type": "integer", "description": "Optimistic-concurrency guard. Supplied by the server from the doc's current `rev` when omitted; pass it only to pin a rev you read earlier." }
                },
                "required": ["id"]
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
            description: "Create a project. Returns its id and name. This does NOT become \
                the default project — MCP has no tool for `project.use`, so pass `project` \
                explicitly on every `tasqx_add_task` that should land here, including the \
                first one.",
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

/// The server-level workflow a host may inject into the agent's system prompt,
/// scope-aware (D141).
///
/// `tools/list` tells a host what an agent CAN call and nothing in the protocol
/// tells it WHEN: a tool description is read only once a caller has already
/// decided to call that tool, which is too late for "search before you decide".
/// `initialize`'s `instructions` is the one in-band place a cross-tool loop
/// fits, and until it carried one the loop had to be pasted into a per-client
/// instructions file that does not travel between clients.
///
/// The read variant is not the write text shortened. Under [`Scope::Read`] the
/// write tools are absent from `tools/list` entirely, so naming one here would
/// aim the agent at a tool it will be refused — and the refusal would be the
/// first it hears of the scope. It states the scope instead, and says what to
/// do with what it cannot store. Shared paragraphs are one literal each so the
/// two variants cannot drift into disagreeing about the same advice.
pub fn instructions(scope: Scope) -> String {
    const INTRO: &str = "tasqx is this workspace's backlog and long-term memory: one searchable \
        index over imported knowledge docs and the annotations written on tasks. Use it instead \
        of an in-conversation todo list or a memory file.";

    const SEARCH: &str = "Search first. Call tasqx_search_memory before resuming work, choosing \
        between designs, touching a convention-bearing file, or asserting how this project does \
        something. Query with two or three keywords, never a sentence: every word is required, \
        and `matched` shows what ran. Try a second wording before concluding nothing is there. A \
        hit is a snippet: read a doc whole with tasqx_get_memory, and an annotation (source \
        `task:#<id>`) with tasqx_get_task.";

    const TRACK: &str = "Track multi-step work as tasks. Anything with more than one step, or \
        that could outlive this session, goes in the backlog: tasqx_list_projects before \
        tasqx_create_project, tasqx_add_task per piece, tasqx_add_dependency to order them, \
        tasqx_add_check for acceptance criteria. Pick work with tasqx_list_tasks and the filter \
        `@working`; blocked and waiting tasks are hidden there by design. Per task: \
        tasqx_brief_task, then tasqx_start_timer, do the work, tasqx_annotate_task with what was \
        decided and delivered, and tasqx_complete_task naming in checks_passed what you proved. \
        Obsolete work is cancelled with tasqx_cancel_task, never left open.";

    const WRITE_BACK: &str = "Write as you go. Only annotation bodies are indexed, never titles, \
        so name files and symbols in them. Call tasqx_add_memory when a decision settles or you \
        learn a convention written down nowhere: the ruling in the first line, the why under it, \
        the path in source. A wrong entry is removed with tasqx_remove_memory (permanent), not \
        corrected beside.";

    const READ_ONLY: &str = "This server is read-only: no write tool is listed. Say so once, keep \
        searching, and put what you would have stored (decisions and their reasons, outcomes, \
        conventions) into your reply rather than dropping it. The operator grants writes by \
        relaunching with `tasqx mcp serve --scope write`.";

    const SEED: &str = "An empty store is a store nobody seeded. If searches keep returning \
        nothing, ask the user to run `tasqx memory import <docs-dir>` once per markdown folder; \
        no MCP tool imports. The CLI is always `tasqx <verb>`: a bare `tasqx` opens a full-screen \
        dashboard and hangs a tool call.";

    let mut parts = vec![INTRO, SEARCH];
    if scope.allows_write() {
        parts.push(TRACK);
        parts.push(WRITE_BACK);
    } else {
        parts.push(READ_ONLY);
    }
    parts.push(SEED);
    parts.join("\n\n")
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
    /// D140: this connection's identity, minted once per `tasqx mcp serve`
    /// process and injected on `task.start` the way `client` is.
    ///
    /// It answers "is this the same client that started the other timer", and
    /// nothing else — not who that client is. tasqx cannot authenticate a
    /// caller (§7 says so of `--scope` in as many words), but each serve is its
    /// own process with its own handshake, so telling two connections apart is
    /// a question the store CAN answer.
    ///
    /// Deliberately not minted by the CLI: a one-shot `tasqx start` is a fresh
    /// process every time, so a CLI-minted id would make a person at a shell a
    /// different actor on every command and refuse them their own auto-stop.
    /// Absent is the right value there, and absent means "behave as before".
    connection_id: String,
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
            connection_id: format!("mcp:{}", crate::clock::uuid_v7()),
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
            },
            // D141: what to do with the tools, not just which ones exist. Read
            // off the session's own scope, because the read variant may not
            // name a tool this server will refuse.
            "instructions": instructions(self.scope)
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
                    "tool `{name}` requires write scope, but this MCP server is running \
                     read-only. This cannot be changed from a tool call: the operator must \
                     relaunch the server as `tasqx mcp serve --scope write`."
                ),
            );
        }

        // A refusal here is a refusal BEFORE dispatch: an argument this
        // transport owns and cannot read is not a question the engine can be
        // asked, and answering it anyway would mean running the call to hand
        // back a response in a shape the caller did not ask for.
        let prepared = match self.prepare_args(spec, params) {
            Ok(prepared) => prepared,
            Err(refusal) => return refusal,
        };

        let outcome = dispatch(self.engine, spec.method, &prepared.args);
        self.present(spec, &prepared, outcome)
    }

    /// Everything this transport does to the arguments BEFORE dispatch, in one
    /// place. `tools_call` used to interleave these five rewrites with the
    /// lookup, the fence and the response fitting, and every new tool behavior
    /// landed as another inline `if spec.method == ...` in the middle of it.
    ///
    /// `Err` is a finished `tools/call` result (an `isError` text block), not a
    /// transport error: the only refusal raised here is a transport-only
    /// argument whose VALUE this server cannot read, which the engine will
    /// never see because the key never reaches it.
    fn prepare_args(&self, spec: &ToolSpec, params: &Value) -> Result<PreparedCall, Value> {
        // A JSON `null` is "no arguments", the same reading `check_params`
        // gives a null `params`; substituting `{}` here is what lets the
        // `task.list` defaults below apply to it. Left as `null`, the
        // engine still answered (null is no params to it) but with the full
        // row and no page, past every default this transport supplies.
        let mut args = params
            .get("arguments")
            .filter(|v| !v.is_null())
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

        // The same optimistic-concurrency default one entity over (#135):
        // `memory.update` gets the identical treatment `task.modify` gets
        // above, so `tasqx_update_memory`'s own description ("when omitted,
        // this server reads the doc's current rev and pins it") is true
        // rather than aspirational.
        if spec.method == "memory.update" {
            if let Some(obj) = args.as_object_mut() {
                if !obj.contains_key("expected_rev") {
                    if let Some(rev) = self.current_memory_rev(obj.get("id")) {
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
        // names its own page is paged exactly as it asked; what it does NOT
        // get is a response outside the byte budget (D148), because a page is
        // a request about rows and the budget is about bytes.
        // The same pattern for the collection reader, and for the same
        // reason one relation over: `task.list` had no default bound at all,
        // and the escape hatch it did have truncated silently — `count` was
        // the number of rows RETURNED, with no total and no offset anywhere in
        // the answer. Since D110 the core itself clamps an explicit `limit` to
        // `MAX_TASK_LIST_LIMIT` rather than answering whole when asked; the
        // page for an OMITTED limit is still supplied HERE, where the payload
        // limit lives, and `total` / `next_offset` make what was left out both
        // visible and reachable.
        //
        // D152 narrows the ROW the same way and in the same place the page is
        // narrowed, and for the same reason: measured over 36 hours of
        // transcripts, `fields` was never passed, so every list call paid for
        // 22 keys per row to read nine of them. An explicit `fields` of any
        // kind is forwarded untouched — including `fields: []`, which the
        // engine reads as no restriction at all (#76.1) and which is therefore
        // how a caller asks this transport for the whole row. A JSON `null`
        // counts as absent, the D32 reading this file already applies to
        // `view` and `client`.
        let mut paged_list_by_us = false;
        let mut fields_defaulted_by_us = false;
        if spec.method == "task.list" {
            if let Some(obj) = args.as_object_mut() {
                if !obj.contains_key("limit") {
                    obj.insert("limit".to_string(), json!(LIST_PAGE));
                    paged_list_by_us = true;
                }
                if obj.get("fields").is_none_or(Value::is_null) {
                    obj.insert("fields".to_string(), json!(LIST_DEFAULT_FIELDS));
                    fields_defaulted_by_us = true;
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
        // Default FALSE since D151: the reader of a tool result is the model,
        // and the model reads the view. The second block is that same result
        // restated — measured across 36 hours of transcripts, these two tools
        // sent 150 KB of 373 KB and not one of the 42 calls passed the D72
        // opt-out — so it is now what a script asks for, not what everyone
        // pays for. The view-only answer goes through the budget like any
        // other (see `fit_to_budget`).
        let include_json = consumed
            .get("include_json")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Default true, unlike the one above, and D151 does not touch it:
        // `annotation.add` has echoed its body since before this argument
        // existed (D72/D75) because that echo is the caller's only evidence
        // the body was stored verbatim — it is not a restatement of something
        // the response says elsewhere. `include_body: false` is the opt-out.
        let include_body = consumed
            .get("include_body")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        // D146, and the one transport-only argument that is not a boolean.
        //
        // An unreadable value is REFUSED rather than defaulted, which is the
        // opposite of what the two booleans above do with a non-boolean — and
        // deliberately so: `include_json: "yes"` defaulted to the block the
        // caller was already getting, while `view: "table"` defaulted would
        // answer a request for a document with the view meant for a model, and
        // the agent finds out when a person reads a reflowed table in a pull
        // request. A JSON `null` counts as absent, the same D32 reading this
        // file already applies to `client` and `actor`, so a client that
        // serializes unset optionals as null keeps the default.
        let view = match consumed.get("view").filter(|v| !v.is_null()) {
            None => View::Markdown,
            Some(v) => match v.as_str() {
                Some("markdown") => View::Markdown,
                Some("card") => View::Card,
                _ => {
                    return Err(tool_error(
                        "bad_request",
                        format!(
                            "`view` takes \"markdown\" or \"card\", not {v}. \"markdown\" is \
                             the default, the rendered view an agent reads; \"card\" is the \
                             fixed-width box card for pasting in front of a person. Both \
                             render the same result, so nothing is lost by retrying with \
                             either one."
                        ),
                    ));
                }
            },
        };

        if spec.method == "task.get" {
            if let Some(obj) = args.as_object_mut() {
                if !obj.contains_key("annotations_limit") {
                    obj.insert("annotations_limit".to_string(), json!(ANNOTATION_PAGE));
                }
            }
        }
        // D148: the per-body cap, on both reads that carry annotation prose.
        // Inserted here and not defaulted in the engine for D63's reason — the
        // bound belongs to the transport that has a payload limit, and
        // `tasqx api` and the CLI keep answering whole bodies.
        //
        // A JSON `null` counts as absent, the D32 reading this file already
        // applies to `view` and `client`: a client that serializes unset
        // optionals as null must get the default, not an uncapped answer.
        if spec.method == "task.get" || spec.method == "task.brief" {
            if let Some(obj) = args.as_object_mut() {
                if obj.get("max_body_bytes").is_none_or(Value::is_null) {
                    obj.insert("max_body_bytes".to_string(), json!(ANNOTATION_BODY_CAP));
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
        // D140, the same pattern one field over: a caller that names its own
        // `actor` is respected as-is (a fleet with stable agent ids has better
        // names than a connection), and one that names none is stamped with
        // this connection's. `task.done` is not stamped, because it takes no
        // `actor` — only starting a clock can collide with one.
        if spec.method == "task.start" {
            if let Some(obj) = args.as_object_mut() {
                if obj.get("actor").is_none_or(Value::is_null) {
                    obj.insert(
                        "actor".to_string(),
                        Value::String(self.connection_id.clone()),
                    );
                }
            }
        }

        Ok(PreparedCall {
            args,
            include_json,
            view,
            include_body,
            paged_list_by_us,
            fields_defaulted_by_us,
        })
    }

    /// The card options every card this transport draws is drawn with (D146).
    ///
    /// Always `Borders::Unicode`, never read off a capability: `Borders::Ascii`
    /// exists for a destination that mangles box-drawing glyphs — a legacy
    /// console, a proportional font — and there is no such destination here.
    /// An MCP response is read in a chat surface, and this server has no
    /// terminal to ask about anyway. `now` is the caller's, stamped once in
    /// `present`, so a card and the detail view beside it cannot disagree
    /// about what "2 hours ago" means.
    fn card_opts(&self, now: jiff::Timestamp) -> crate::markdown::CardOpts {
        crate::markdown::CardOpts {
            detail: crate::markdown::DetailOpts {
                time: self.time_format,
                now,
            },
            borders: crate::markdown::Borders::Unicode,
        }
    }

    /// Fit the dispatch outcome into a tool response — the post-dispatch half
    /// of the seam `prepare_args` is the pre-dispatch half of.
    fn present(&self, spec: &ToolSpec, prepared: &PreparedCall, outcome: DispatchOutcome) -> Value {
        match outcome {
            Ok(mut result) => {
                // D153: the closing card, drawn from the task as it is
                // AFTER completion, so it carries the Delivered row and the
                // check states. `task.done`'s frozen result has no task in it
                // to render, so the task is read back here with the same
                // annotation bounds the transport gives `task.get` — the card
                // is the one `tasqx_get_task view: "card"` would draw. A
                // failed re-read leaves the view empty, which
                // `tool_ok_with_view` degrades to the plain JSON: presentation
                // cannot make a completion that really happened look broken.
                //
                // No bisection: a card has no page to cut, its lines are fixed
                // 72-column geometry and only its row COUNT (checks, blockers,
                // dependents) can grow. So the budget is a single measure —
                // card plus JSON — and a card that would breach it is replaced
                // by a one-line notice naming the read that draws it. The JSON
                // is what must arrive, and a block the caller asked for is
                // never dropped silently (D72).
                if spec.method == "task.done" && prepared.view == View::Card {
                    let card_opts = self.card_opts(crate::clock::now());
                    let read = json!({
                        "ref": result["short_id"],
                        "annotations_limit": ANNOTATION_PAGE,
                        "max_body_bytes": ANNOTATION_BODY_CAP,
                    });
                    let mut card = dispatch(self.engine, "task.get", &read)
                        .map(|task| fence(&crate::markdown::task_card(&task, &card_opts)))
                        .unwrap_or_default();
                    let json_len = serde_json::to_string(&result).map(|s| s.len()).unwrap_or(0);
                    if card.len() + json_len > RESPONSE_BUDGET_BYTES {
                        card = format!(
                            "Closing card omitted: with the completion result it would exceed \
                             this tool's response budget. `tasqx_get_task` with `ref: {}` and \
                             `view: \"card\"` draws it.\n",
                            result["short_id"]
                        );
                    }
                    return tool_ok_with_view(card, &result);
                }
                // The one rendered surface. Keyed on the method rather than the
                // tool name to match the `task.modify`/`task.start` checks
                // above; exactly one tool maps to `task.get`, so this is the
                // same set either way.
                if spec.method == "task.brief" {
                    let now = crate::clock::now();
                    let opts = crate::markdown::DetailOpts {
                        time: self.time_format,
                        now,
                    };
                    let card_opts = self.card_opts(now);
                    // Only the TASK half is a card. The tail — what each
                    // prerequisite concluded, what this releases, what the
                    // store remembers — is the markdown it always was, and is
                    // appended OUTSIDE the fence, because a fence around prose
                    // is a code block around sentences.
                    let render = |r: &Value| match prepared.view {
                        View::Markdown => crate::markdown::task_brief(r, &opts),
                        View::Card => {
                            let card = fence(&crate::markdown::task_card(r, &card_opts));
                            card + &crate::markdown::brief_tail_text(r)
                        }
                    };
                    return self.fit_brief_to_budget(
                        result,
                        &prepared.args,
                        &render,
                        prepared.include_json,
                    );
                }
                if spec.method == "task.get" {
                    // Stamped HERE, never inside the renderer: that is what
                    // keeps `task_detail` and `task_card` pure and their golden
                    // tests stable.
                    let now = crate::clock::now();
                    let opts = crate::markdown::DetailOpts {
                        time: self.time_format,
                        now,
                    };
                    let card_opts = self.card_opts(now);
                    // D146: one renderer chosen here, then handed to the budget
                    // whole, so D66's three steps run over whichever view the
                    // caller asked for instead of being written twice.
                    let render = |r: &Value| match prepared.view {
                        View::Markdown => crate::markdown::task_detail(r, &opts),
                        View::Card => fence(&crate::markdown::task_card(r, &card_opts)),
                    };
                    // The view-only answer (D151's default) runs the SAME
                    // budget, and this is where it used to return early
                    // instead: unbounded but for D148's body cap, which as an
                    // opt-in was survivable and as the default would hand a
                    // client a twenty-annotation page of 16 KB bodies. What it
                    // does not get is the omission notice — nothing the caller
                    // asked for was dropped, and saying "both blocks together
                    // exceeded this tool's response budget" over a 400-byte
                    // answer is a false sentence AND a ~300-byte bill.
                    return self.fit_to_budget(
                        result,
                        &prepared.args,
                        &render,
                        prepared.include_json,
                    );
                }
                // The opt-out half of D72/D75's echo: the caller already holds
                // every byte of `body` (it is right there in the request this
                // is a response to), so a caller who says so gets a length
                // rather than the bytes again. The frozen `annotation.add`
                // result itself is untouched by this — `dispatch` still
                // returns the full ANNOTATION shape D56 froze, and a caller of
                // `tasqx api` always gets it whole; only this transport's OWN
                // presentation of it is rewritten, on request.
                if spec.method == "annotation.add" && !prepared.include_body {
                    if let Some(body_len) = result["annotation"]["body"].as_str().map(str::len) {
                        if let Some(obj) = result["annotation"].as_object_mut() {
                            obj.remove("body");
                            obj.insert("body_bytes".to_string(), json!(body_len));
                        }
                    }
                }
                // The other half of D152's default row: a key the caller
                // never named AND whose value is null says nothing, so it is
                // dropped. Before the budget below, not after, so the fit
                // counts the bytes actually sent. Rows under an explicit
                // `fields` are byte-identical to `dispatch`'s own, nulls
                // included — this transport narrows only what it widened.
                if prepared.fields_defaulted_by_us {
                    if let Some(rows) = result.get_mut("tasks").and_then(Value::as_array_mut) {
                        for row in rows {
                            if let Some(obj) = row.as_object_mut() {
                                obj.retain(|_, v| !v.is_null());
                            }
                        }
                    }
                }
                if prepared.paged_list_by_us {
                    return self.fit_list_to_budget(result, &prepared.args);
                }
                tool_ok(&result)
            }
            Err(e) => {
                let code = serde_json::to_value(e.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "internal".to_string());
                let message = mcp_surface_message(e.message, e.data.as_ref());
                tool_error_with_data(&code, message, e.data)
            }
        }
    }

    /// Fit a `task.get` response to [`RESPONSE_BUDGET_BYTES`], spending the
    /// duplicate JSON block before it spends any of the history.
    ///
    /// **Every answer, whatever page size it named (D148).** D66 exempted a
    /// caller who passed `annotations_limit`, on the argument that a request
    /// second-guessed is a caller who can never fetch a big page on purpose.
    /// Measured in the field, the exemption was not an escape hatch but the
    /// default failure: `{ref: 609, include_json: false, annotations_limit: 5}`
    /// answered 244,633 bytes against this 24,576-byte budget and the client
    /// refused the tool result, and the only way to discover the exemption was
    /// to trip it. The deliberate escape is now an argument that says what it
    /// does — `max_body_bytes`, named on the response that was cut — and a page
    /// the caller asked for is still answered in full whenever it fits, which
    /// is every ordinary read.
    ///
    /// # The order the budget spends in
    ///
    /// Under the default (D151) there is no JSON block to spend, so step 1 does
    /// not apply and the search starts at step 2 — the view alone at the page in
    /// hand, then a smaller page. `include_json` is the parameter that says
    /// which of the two runs; the steps themselves are the same either way.
    ///
    /// 1. both blocks at the page in hand — an ordinary task never notices this
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
    /// The floor is one whole annotation, and one annotation is now bounded
    /// too. D63 stopped here because the only lever below the floor was cutting
    /// a body, and prose that stops mid-sentence with nothing marking the cut is
    /// worse than an oversized answer — a judgement about an UNMARKED cut, and
    /// still the right one. [`ANNOTATION_BODY_CAP`] cuts with a marker carrying
    /// the body's real size and the exact call that returns every byte of it,
    /// so the floor sits under this budget by construction. It exceeds it only
    /// when the caller raised `max_body_bytes` themselves, which is the escape
    /// and is documented on that parameter.
    ///
    /// The bisection's upper bound is the page size in `args` — the caller's
    /// own, or the default this transport inserted — never the constant. Using
    /// [`ANNOTATION_PAGE`] would re-cut an explicit `annotations_limit: 40`
    /// down to 20 before measuring anything, turning the bound into a second
    /// page size the caller never asked for.
    ///
    /// `render` is a parameter rather than the fixed call to `task_detail` it
    /// once was, because D146 gave the answer a second spelling: the steps
    /// above are about how many BYTES a view costs and nothing about which view
    /// it is, so the card runs the identical budget instead of a second copy of
    /// it that would be the one nobody tests.
    fn fit_to_budget(
        &self,
        first: Value,
        args: &Value,
        render: &dyn Fn(&Value) -> String,
        include_json: bool,
    ) -> Value {
        let json_len = |result: &Value| serde_json::to_string(result).map(|s| s.len()).unwrap_or(0);

        // The finished text is what is measured and what is sent: the notice
        // only exists when a JSON block the caller ASKED FOR was dropped (D72,
        // D151). Measuring the bare view and appending the notice afterwards is
        // how a view landing just under the limit once produced a response just
        // over it — the payload bound defeated by the sentence explaining it.
        let finish = |view: &str| {
            if include_json {
                view_only_text(view)
            } else {
                view.to_string()
            }
        };
        let fits = |view: &str| finish(view).len() <= RESPONSE_BUDGET_BYTES;
        let close = |view: &str| tool_ok_text(&finish(view));

        let view = render(&first);
        if include_json && view.len() + json_len(&first) <= RESPONSE_BUDGET_BYTES {
            return tool_ok_with_view(view, &first);
        }
        // Step 2: the same page, without the duplicate.
        if fits(&view) {
            return close(&view);
        }

        // Step 3: the largest page that fits, found by BISECTION rather than by
        // halving until something works. Halving lands on a power-of-two
        // fraction of the starting page and stops there, which on the shape
        // that provoked all this — a handful of very long bodies — overshoots
        // by a factor of two: it would show two annotations where four fit.
        // Same number of dispatches, an answer that is actually the largest.
        //
        // `hi` is the page this answer was produced at — the caller's explicit
        // `annotations_limit` or the default inserted for them — because the
        // search is for the largest page that fits AT OR BELOW what was asked
        // for. A caller who named 0 asked for a task's fields without its
        // history, and there is no page left to cut: whatever the view costs is
        // the task itself, and step 2 has already answered it.
        let mut view = view;
        let mut lo = 1u64;
        let mut hi = args
            .get("annotations_limit")
            .and_then(Value::as_u64)
            .unwrap_or(ANNOTATION_PAGE);
        if hi == 0 {
            return close(&view);
        }
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
            if fits(&rendered) {
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
        close(&best.unwrap_or(view))
    }

    /// Fit a `task.brief` response to [`RESPONSE_BUDGET_BYTES`] (D136).
    ///
    /// D66's three steps, one method over: both blocks if they fit, then the
    /// rendered view alone, then the largest `memory_limit` that fits, found by
    /// bisection rather than by halving. Under D151's default there is no JSON
    /// block to spend, so the first step is skipped and the view alone is what
    /// is measured — `include_json` says which. The lever is the memory page and
    /// nothing else — the task half and the neighbourhood are what the caller
    /// asked for, and a brief that cut the prerequisite's outcome to make room
    /// for a search hit would have dropped the more valuable half.
    ///
    /// A caller that named its own `memory_limit` is answered exactly as asked,
    /// however large: a request second-guessed is a caller who can never ask
    /// for a big page on purpose. That exemption is this method's own and no
    /// longer `fit_to_budget`'s — D148 removed the `annotations_limit` one,
    /// because a task's annotations are the caller's own prose and can be
    /// arbitrarily large, while a memory page is `MEMORY_SEARCH_LIMIT` bounded
    /// snippets. The task half arrives with its bodies already capped, which is
    /// where a brief's unbounded bytes actually came from.
    ///
    /// `render` is the caller's chosen view (D146), for `fit_to_budget`'s
    /// reason: the lever and the measurement are the same whichever way the
    /// task half is spelled.
    fn fit_brief_to_budget(
        &self,
        first: Value,
        args: &Value,
        render: &dyn Fn(&Value) -> String,
        include_json: bool,
    ) -> Value {
        let json_len = |result: &Value| serde_json::to_string(result).map(|s| s.len()).unwrap_or(0);
        // [`McpServer::fit_to_budget`]'s trio, for its reasons: the notice is
        // part of what is measured, and it exists only where a JSON block the
        // caller asked for was dropped (D72, D151).
        let finish = |view: &str| {
            if include_json {
                view_only_text(view)
            } else {
                view.to_string()
            }
        };
        let fits = |view: &str| finish(view).len() <= RESPONSE_BUDGET_BYTES;
        let close = |view: &str| tool_ok_text(&finish(view));

        let named_own_limit = args.get("memory_limit").is_some_and(|v| !v.is_null());
        let view = render(&first);
        if named_own_limit || view.len() + json_len(&first) <= RESPONSE_BUDGET_BYTES {
            // The exemption and the fitting answer both come out here, and
            // both honour the caller's block count: two blocks when asked for,
            // the bare view otherwise — with no notice either way, since
            // nothing was dropped.
            return if include_json {
                tool_ok_with_view(view, &first)
            } else {
                tool_ok_text(&view)
            };
        }
        if fits(&view) {
            return close(&view);
        }

        let mut view = view;
        let mut lo = 0u64;
        let mut hi = crate::engine::MEMORY_SEARCH_LIMIT;
        let mut best: Option<String> = None;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            let mut retry = args.clone();
            match retry.as_object_mut() {
                Some(obj) => obj.insert("memory_limit".to_string(), json!(mid)),
                None => break,
            };
            let Ok(candidate) = dispatch(self.engine, "task.brief", &retry) else {
                break;
            };
            let rendered = render(&candidate);
            if fits(&rendered) {
                best = Some(rendered);
                lo = mid + 1;
            } else {
                // The floor is ZERO hits, not one: unlike an annotation page,
                // dropping memory entirely still leaves a useful brief — the
                // task and what its prerequisites decided. If even that does
                // not fit, the task itself is past the budget and there is no
                // lever here that would help.
                if mid == 0 {
                    view = rendered;
                    break;
                }
                hi = mid - 1;
            }
        }
        close(&best.unwrap_or(view))
    }

    /// Fit a `task.list` response to [`RESPONSE_BUDGET_BYTES`] by re-cutting
    /// the page this transport supplied.
    ///
    /// Only for a caller who named no `limit`. One that did is answered
    /// exactly as asked, however large: a request second-guessed is a caller
    /// who can never fetch a big page on purpose. The exemption is safe HERE in
    /// a way D148 found it was not for `annotations_limit` — a `task.list` row
    /// is bounded fields, and `MAX_TASK_LIST_LIMIT` bounds how many (D110),
    /// while one annotation is unbounded prose.
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
        let size = |result: &Value| serde_json::to_string(result).map(|s| s.len()).unwrap_or(0);
        if size(&first) <= RESPONSE_BUDGET_BYTES {
            return tool_ok(&first);
        }

        let Some(rows) = first.get("tasks").and_then(Value::as_array).cloned() else {
            return tool_ok(&first);
        };
        let total = first.get("total").and_then(Value::as_u64).unwrap_or(0);
        // Carried through unchanged (#233): it describes the STORE, not this
        // page, so re-cutting the array never changes it — dropping it here
        // would make the re-cut answer diverge from a real `limit: k` call,
        // which is exactly what this function exists to not do.
        let store_empty = first.get("store_empty").cloned().unwrap_or(json!(false));
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let cut = |k: usize| -> Value {
            let reached = offset + k as u64;
            json!({
                "count": k,
                "total": total,
                "next_offset": if reached < total { json!(reached) } else { Value::Null },
                "store_empty": store_empty,
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

    /// `memory.update`'s counterpart to [`Self::current_rev`]: read a doc's
    /// current `rev` by id, for the same server-side auto-pin.
    fn current_memory_rev(&self, id_val: Option<&Value>) -> Option<i64> {
        let id_val = id_val?;
        let got = dispatch(self.engine, "memory.get", &json!({ "id": id_val })).ok()?;
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
        .iter()
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
            { "type": "text", "text": serde_json::to_string(result).unwrap_or_default() }
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
            { "type": "text", "text": serde_json::to_string(result).unwrap_or_default() }
        ],
        "isError": false
    })
}

/// Wrap a block in a `text` code fence — D146's card, ready to be pasted.
///
/// The fence is not decoration. A 72-column box is a box only while its lines
/// are rendered in a monospaced run, and the surfaces this response is read in
/// render a text block as markdown: unfenced, the border glyphs join the
/// paragraph around them and the card becomes one long line of `│`. `text`
/// rather than a language name, because there is no language — a highlighter
/// handed one would colour the borders.
///
/// The card already ends in a newline and the closing fence must not be
/// preceded by a blank line, so the trailing one is taken off and put back:
/// exactly one `\n` inside the fence and exactly one after it, whatever the
/// block arrived with.
fn fence(block: &str) -> String {
    let body = block.strip_suffix('\n').unwrap_or(block);
    format!("```text\n{body}\n```\n")
}

/// One text block, verbatim. Since D151 this is the ordinary `task.get` and
/// `task.brief` answer — the rendered view, fitted to the budget, with no
/// omission to explain — and it also carries the view plus
/// [`view_only_text`]'s notice on the one path where a caller asked for both
/// blocks and the two did not fit.
fn tool_ok_text(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    })
}

/// The view plus the omission notice, as one block.
///
/// Only for the caller who passed `include_json: true` and was answered one
/// block anyway (D151): a response silently short of a block the caller ASKED
/// for is indistinguishable from a server that never sends JSON. The default
/// answer is the view alone and gets no notice, because nothing the caller
/// asked for was dropped and the view's own Annotations heading already says
/// what page it holds.
///
/// One function so the budget and the answer measure the same string. They did
/// not: `fit_to_budget` compared the bare view against
/// [`RESPONSE_BUDGET_BYTES`] and the notice was appended afterwards, so a view
/// landing just under the limit produced a response just over it — the payload
/// bound defeated by the sentence explaining the payload bound.
fn view_only_text(view: &str) -> String {
    format!(
        "{view}\n_Machine-readable JSON omitted: you asked for both blocks and together they \
         exceeded this tool's response budget, so the budget spent the JSON first. The \
         rendered view above carries the same annotations — its Annotations heading says how \
         much of the history it holds, and names the `annotations_offset` that reads the rest. \
         The budget applies to every answer, whatever `annotations_limit` you name; the \
         rendered view alone is the default, and `include_json: true` is what spends the \
         budget on this duplicate before it spends it on history. A body longer than \
         `max_body_bytes` (default 16384) is cut IN THIS RESPONSE ONLY, marked with its real \
         size and the call that reads it whole._\n"
    )
}

/// An error `tools/call` result (scope denial, unknown tool, or a core
/// `ApiError`): the code + message as text content, flagged `isError`, with
/// no structured detail. Most refusals raised inside this module (an unknown
/// tool name, a scope denial) have none to carry; a dispatch `ApiError` goes
/// through [`tool_error_with_data`] instead.
fn tool_error(code: &str, message: impl Into<String>) -> Value {
    tool_error_with_data(code, message, None)
}

/// The same `tools/call` error shape as [`tool_error`], plus the `data` a core
/// `ApiError` carries — both as a `structuredContent.error` object beside the
/// text block.
///
/// Before this, `code` and `data` reached an MCP caller only as a substring of
/// prose (`error [not_found]: ...`), an undocumented wire format `code` had to
/// be regex-matched out of and `data` could not be recovered from at all —
/// while the same failure over `tasqx api` answers a machine-readable
/// `{code, message, data}` envelope. `structuredContent` is additive: the text
/// block is unchanged, so nothing that already parses `content[0].text` is
/// affected.
fn tool_error_with_data(code: &str, message: impl Into<String>, data: Option<Value>) -> Value {
    let message = message.into();
    let mut error = json!({ "code": code, "message": message });
    if let Some(d) = data {
        error["data"] = d;
    }
    json!({
        "content": [
            { "type": "text", "text": format!("error [{code}]: {message}") }
        ],
        "isError": true,
        "structuredContent": { "error": error }
    })
}

/// Engine error phrases naming a CLI verb or JSON-API method that has no MCP
/// tool of the same name, rewritten to the tool an MCP caller can actually
/// call.
///
/// Each entry is author-written prose emitted by exactly one call site (never
/// user input echoed back), so a substring replace cannot misfire on a task
/// title or filter value that happens to contain the same words. The engine
/// message stays exactly as written for `tasqx api` and the CLI, where
/// `task.start/stop/done` and `task.get` ARE the right names to print; only
/// what an MCP session sees is rewritten, here, at the transport boundary the
/// rest of this file already narrows through (`TRANSPORT_ONLY_ARGS`,
/// `UNEXPOSED_METHODS`).
const MCP_REMEDY_REWRITES: &[(&str, &str)] = &[
    (
        "use task.start/stop/done for other transitions",
        "use tasqx_start_timer / tasqx_stop_timer / tasqx_complete_task for other transitions",
    ),
    (
        "read it with task.get on #",
        "read it with tasqx_get_task on #",
    ),
];

/// Apply [`MCP_REMEDY_REWRITES`], then the one rewrite that needs the error's
/// own `data` rather than a fixed phrase: `require_live_project`'s refusal
/// names `tasqx init NAME`, a CLI verb with no MCP equivalent and no shell to
/// run it in. `data.name` carries the same name the message embeds, so the
/// CLI-specific clause is replaced exactly rather than guessed at from prose.
fn mcp_surface_message(message: String, data: Option<&Value>) -> String {
    let mut out = message;
    for (from, to) in MCP_REMEDY_REWRITES {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    if let Some(name) = data.and_then(|d| d.get("name")).and_then(Value::as_str) {
        let cli_clause = format!("(create it with `tasqx init {}`)", shell_quote(name));
        if out.contains(&cli_clause) {
            out = out.replace(
                &cli_clause,
                "(create it first with the `tasqx_create_project` tool)",
            );
        }
    }
    out
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
        let exposed: Vec<&str> = tool_specs().iter().map(|s| s.method).collect();
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
    /// `fit_to_budget` measured the bare view and the JSON-omission sentence
    /// was appended afterwards, so a view landing within that sentence's
    /// length of the limit shipped over it — the payload bound defeated by the
    /// text explaining the payload bound. The window is narrower than any
    /// hand-written fixture would reliably hit, so the fixture is searched for.
    ///
    /// Driven with `include_json: true` since D151: the notice exists only on
    /// that path now, and this test is about the notice.
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
                "params": {
                    "name": "tasqx_get_task",
                    "arguments": { "ref": 1, "include_json": true }
                }
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

    /// The date grammar offers a clock with no offset, and D132 reads that
    /// clock as UTC. An agent in Amsterdam sending `2026-07-20T17:00` from a
    /// description that never names the zone means 17:00 local and gets 19:00
    /// local — the one reader of this sentence who cannot see a `show` card's
    /// UTC marker is the one it must tell.
    #[test]
    fn the_date_grammar_says_an_offsetless_clock_is_utc() {
        assert!(
            WHEN_GRAMMAR.contains("UTC"),
            "the date grammar offers \"2026-07-20T17:00\" and never says the clock is UTC \
             (D132): {WHEN_GRAMMAR}"
        );
        assert!(
            WHEN_GRAMMAR.contains("offset"),
            "the date grammar says UTC without saying an explicit offset keeps its meaning: \
             {WHEN_GRAMMAR}"
        );
    }
}
