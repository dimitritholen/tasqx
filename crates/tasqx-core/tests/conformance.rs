//! The v1 API conformance suite — the contract of record DESIGN.md §11 names.
//!
//! # What this is, and what it deliberately is not
//!
//! Every other test file in this directory tests **behaviour**: a cycle is
//! refused, a cancelled blocker releases its dependents, an import that fails
//! writes nothing. Those are the rules. This file tests the **shape** — the
//! envelope, the `"tasqx":"1"` version string, the error codes, which keys each
//! method's `result` carries and which of them are optional.
//!
//! The distinction is the whole reason this file exists. Rename `unblocked` to
//! `unblocks` in `task.done` and every behaviour test in this crate still
//! passes: the cascade is still computed, still correct, still in the response.
//! What breaks is every client written against v1, silently, at run time. §11
//! declares the API stable and points at "the conformance test suite" as the
//! thing that makes that declaration mean something. Before this file there was
//! no such suite, so the declaration was a sentence in a document.
//!
//! So: an added key is additive and allowed by the freeze — but it must JOIN a
//! shape here, because a shape that quietly tolerates unknown keys cannot tell
//! an addition from a rename (a rename shows up as one key missing *and* one
//! key added, and only the closed check sees the second half). A removed or
//! renamed key is a **major-version** change: restore the name, or bump
//! `dispatch::API_VERSION` to `"2"` and rewrite this file against it.
//!
//! # Derived, not restated
//!
//! The method list is not typed out here. [`cases`] is checked for **set
//! equality** against `tasqx_core::PARAMS`, the same runtime table
//! `core.capabilities` publishes and the params gate enforces. A method added
//! to the API without a case turns this suite red on the floor test, not on a
//! number someone forgot to bump — guards in this repo have shipped green for
//! months because their list was hand-maintained and had quietly shrunk.
//!
//! The error-code set is read out of `src/error.rs`'s own `as_str` arms for the
//! same reason, and the MCP tool→method map is read out of `src/mcp.rs` and
//! cross-checked against the live `tools/list`.
//!
//! # Scope: the JSON API only, and why that is a decision and not an oversight
//!
//! **This suite freezes the core JSON API (DESIGN.md §4). It does not freeze
//! the MCP tool layer (§7).** D7 separates them on purpose: MCP is a *host
//! integration*, versioned by the MCP protocol revision (`PROTOCOL_VERSION`,
//! currently `2025-06-18`), scope-filtered per process, and free to rename a
//! tool or reshape an `inputSchema` when the protocol or the host ecosystem
//! moves. The JSON API is versioned by `tasqx: "1"` and may not. Freezing both
//! in one file would hand MCP the API's immutability guarantee, which nobody
//! promised and which the protocol's own release cadence would break for us.
//!
//! The exclusion is made explicit rather than silent, by two tests that cover
//! its two halves. [`every_mcp_tool_routes_through_a_frozen_json_api_method`]
//! checks the routing table: every tool names a method this file freezes.
//! [`every_mcp_tool_hands_back_the_frozen_result_of_its_method`] checks what
//! actually comes back: each tool is driven through the real `tools/call` path
//! and its JSON block checked against the frozen shape. The second exists
//! because the first was once the whole argument, and a rename inserted between
//! `dispatch` and the wire sailed through it — a name map cannot see
//! post-processing, and `tools_call` does post-process (the `task.get` view).
//! So the *data* an MCP host receives is covered here even though the tool
//! wrapper is not; the day that stops being true, both halves say so. MCP's own
//! behaviour lives in `tests/mcp.rs`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};
use tasqx_core::{dispatch, handle_envelope, Engine, McpServer, Scope, API_VERSION, PARAMS};

// ---- the shape vocabulary ---------------------------------------------------

/// The JSON type a frozen key is pinned to.
///
/// Coarse on purpose: this is a wire contract, and what a client breaks on is a
/// string that became a number or an object that became an array, not the
/// difference between `3` and `3.0`. `Object` and `Array` carry an optional
/// nested shape (see [`Field::inner`]) for the cases where the row *is* the
/// contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    Str,
    /// An integer. `u64` and `i64` both count — serde_json picks by sign, and a
    /// count that crosses zero must not read as a type change.
    Int,
    /// Any number. Used where the value is a computed float (`urgency`, the
    /// bm25 `rank`) and an exact-integer sample would serialize as `Int`.
    Num,
    Bool,
    Array,
    Object,
}

impl Ty {
    fn matches(self, v: &Value) -> bool {
        match self {
            Ty::Str => v.is_string(),
            Ty::Int => v.is_i64() || v.is_u64(),
            Ty::Num => v.is_number(),
            Ty::Bool => v.is_boolean(),
            Ty::Array => v.is_array(),
            Ty::Object => v.is_object(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Ty::Str => "string",
            Ty::Int => "integer",
            Ty::Num => "number",
            Ty::Bool => "boolean",
            Ty::Array => "array",
            Ty::Object => "object",
        }
    }
}

/// One frozen key of a response object.
#[derive(Clone, Copy)]
struct Field {
    key: &'static str,
    ty: Ty,
    /// `null` is a legal value for this key. Part of the contract, not laxity:
    /// `previous`, `current_default` and `completed` are documented as always
    /// present and sometimes null, which is exactly what a client must be able
    /// to rely on — "absent" and "null" are different answers and §4 picks one
    /// per key.
    null_ok: bool,
    /// The key may be absent entirely. Reserved for keys the engine emits
    /// conditionally (`spawned`, `tokens_hint`, `tracked_seconds`), each of
    /// which has a documented reason to be conditional.
    optional: bool,
    /// For `Object`: the nested shape. For `Array`: the shape of every element.
    ///
    /// A list of groups rather than one slice, so a shape can be *composed* the
    /// way the engine composes it — `list_row_json` is `task_to_json` plus
    /// `blocked`, and this file says that instead of repeating twenty key names.
    ///
    /// Empty means "not descended into". That is a deliberate hole wherever it
    /// appears, and each one is argued at the declaration site.
    inner: Shape,
}

/// A composed object shape: every field of every group, as one flat key set.
type Shape = &'static [&'static [Field]];

const NO_INNER: Shape = &[];

const fn req(key: &'static str, ty: Ty) -> Field {
    Field {
        key,
        ty,
        null_ok: false,
        optional: false,
        inner: NO_INNER,
    }
}

/// Always present, may be `null`.
const fn nul(key: &'static str, ty: Ty) -> Field {
    Field {
        null_ok: true,
        ..req(key, ty)
    }
}

/// May be absent; when present it is not `null`.
const fn opt(key: &'static str, ty: Ty) -> Field {
    Field {
        optional: true,
        ..req(key, ty)
    }
}

const fn req_of(key: &'static str, ty: Ty, inner: Shape) -> Field {
    Field {
        inner,
        ..req(key, ty)
    }
}

/// May be absent; when present it is not `null` and its rows carry `inner`.
///
/// The combination is not decoration: `store.export`'s `tokens` is conditional
/// (omitted when a task has no measurement) *and* a row shape, and it was
/// declared nowhere at all until a review caught it — which left a rename of
/// that key green through this whole suite.
const fn opt_of(key: &'static str, ty: Ty, inner: Shape) -> Field {
    Field {
        inner,
        ..opt(key, ty)
    }
}

/// Walk `shape` (all groups, as one flat key set) against `value`.
///
/// Three failures, each named with the JSON path so a red run points at the key
/// rather than at the method:
///  * a required key is missing — the rename/removal half of a breaking change;
///  * a key's type moved;
///  * a key is present that no group declares — the *addition* half, which is
///    how a rename is told apart from a deletion, and how a genuinely additive
///    field is forced to be written down here rather than shipping unrecorded.
fn check_shape(value: &Value, shape: Shape, path: &str) {
    let obj = value
        .as_object()
        .unwrap_or_else(|| panic!("{path}: the v1 contract makes this a JSON object, got {value}"));

    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for group in shape {
        for f in *group {
            assert!(
                declared.insert(f.key),
                "{path}.{}: declared twice in this shape — a duplicate satisfies every count \
                 while covering one key less than it looks like it does",
                f.key
            );
            check_field(obj, f, path);
        }
    }

    let extra: Vec<&str> = obj
        .keys()
        .filter(|k| !declared.contains(k.as_str()))
        .map(String::as_str)
        .collect();
    assert!(
        extra.is_empty(),
        "{path}: the response carries {extra:?}, which the frozen v1 shape does not declare. \
         Adding a field is allowed by the freeze — but it has to JOIN the shape in \
         tests/conformance.rs, because a shape that tolerates unknown keys cannot tell an \
         addition from a rename: a rename shows up as one key missing AND one key added, and \
         only this half sees the second one."
    );
}

fn check_field(obj: &Map<String, Value>, f: &Field, path: &str) {
    let key = f.key;
    match obj.get(key) {
        None if f.optional => {}
        None => panic!(
            "{path}.{key}: the frozen v1 shape requires this key and the response does not carry \
             it. Present keys: {:?}. Removing or renaming a response field is a BREAKING change \
             to API v1 — restore the name, or bump dispatch::API_VERSION to \"2\" and rewrite \
             this suite against it.",
            obj.keys().collect::<Vec<_>>()
        ),
        Some(Value::Null) if f.null_ok => {}
        Some(Value::Null) => panic!(
            "{path}.{key}: null is not a value this key may take — v1 pins it as {}, always \
             present and never null. A key that starts answering null is a silent behaviour \
             change for every client that does not check.",
            f.ty.name()
        ),
        Some(v) => {
            assert!(
                f.ty.matches(v),
                "{path}.{key}: v1 pins this key as {}, and the response carries {v}. A type \
                 change on a live key breaks clients without breaking any behaviour test.",
                f.ty.name()
            );
            if f.inner.is_empty() {
                return;
            }
            match f.ty {
                Ty::Object => check_shape(v, f.inner, &format!("{path}.{key}")),
                Ty::Array => {
                    let arr = v.as_array().expect("type-checked as an array above");
                    // A fixture that stopped producing rows would leave every
                    // nested key below unchecked while the suite stayed green —
                    // the guard-that-guards-nothing failure this project has
                    // paid for. So the fixture, not the shape, is what fails.
                    assert!(
                        !arr.is_empty(),
                        "{path}.{key}: this array's ROW shape is frozen, and the fixture produced \
                         no rows — so nothing under it was checked. Fix the case's setup so it \
                         emits at least one element."
                    );
                    for (i, el) in arr.iter().enumerate() {
                        check_shape(el, f.inner, &format!("{path}.{key}[{i}]"));
                    }
                }
                other => panic!(
                    "{path}.{key}: a nested shape was declared under a {} — only object and \
                     array can carry one",
                    other.name()
                ),
            }
        }
    }
}

// ---- shared row shapes ------------------------------------------------------

/// The canonical task object — `engine::task_to_json`, the shape §3 describes
/// and every task-shaped surface starts from.
const TASK_CORE: &[Field] = &[
    req("id", Ty::Str),
    req("short_id", Ty::Int),
    req("title", Ty::Str),
    req("status", Ty::Str),
    nul("priority", Ty::Str),
    nul("project", Ty::Str),
    nul("due", Ty::Str),
    nul("scheduled", Ty::Str),
    nul("wait", Ty::Str),
    nul("estimate", Ty::Str),
    nul("recurrence", Ty::Str),
    nul("remind", Ty::Str),
    req("urgency", Ty::Num),
    req("tags", Ty::Array),
    req("created", Ty::Str),
    req("modified", Ty::Str),
    nul("completed", Ty::Str),
    req("_rev", Ty::Int),
];

/// Emitted only for a row whose `status` no writer of this engine could have
/// produced (`flag_unrecognized_status`). Optional by design: an always-present
/// flag would make every new export a `bad_request` in an older tasqx.
const TASK_STATUS_FLAG: &[Field] = &[opt("status_unrecognized", Ty::Bool)];

/// The live-read spelling of tracked time: an ISO duration, plus the open
/// interval's anchor — always present, `null` when there is no open interval.
const TASK_LIVE_TIME: &[Field] = &[req("tracked", Ty::Str), nul("active_since", Ty::Str)];

/// The restore spelling (D42): raw seconds, and both keys omitted when they
/// would be zero/absent — `IMPORT_TASK_KEYS` is a closed gate, so an
/// always-present key would make every new export unreadable to an older tasqx.
const TASK_EXPORT_TIME: &[Field] = &[
    opt("tracked_seconds", Ty::Int),
    opt("active_since", Ty::Str),
];

const ANNOTATION_ROW: &[Field] = &[
    req("id", Ty::Str),
    req("body", Ty::Str),
    req("created", Ty::Str),
];
const ANNOTATION: Shape = &[ANNOTATION_ROW];

/// One `token_usage` measurement — `engine::tokens::measurement_from_row`.
const MEASUREMENT_ROW: &[Field] = &[
    req("id", Ty::Str),
    req("tool", Ty::Str),
    req("source", Ty::Str),
    nul("model", Ty::Str),
    req("input_tokens", Ty::Int),
    req("output_tokens", Ty::Int),
    req("cache_read_tokens", Ty::Int),
    req("cache_creation_tokens", Ty::Int),
    req("confidence", Ty::Str),
    req("created", Ty::Str),
];
const MEASUREMENT: Shape = &[MEASUREMENT_ROW];

/// `depends_on` is an array of short_ids (integers), so there is no row shape to
/// descend into — `Ty::Array` is the whole contract for it.
const TASK_RELATIONS: &[Field] = &[
    req("depends_on", Ty::Array),
    req_of("annotations", Ty::Array, ANNOTATION),
];

/// What `task.get` says about the history it did NOT return.
///
/// `annotations_total` is required, not optional, and present whether the page
/// was elided or not: a count only a truncated caller sees is a count nobody can
/// compare `annotations.len()` against, so "did I get all of it?" would stay
/// unanswerable for exactly the client that needs to ask.
///
/// `annotations_next_offset` is nullable rather than absent for the same reason
/// every other nullable key here is: a key that appears and disappears makes a
/// client branch on presence, and this one flips on every read of the last page.
const TASK_ANNOTATION_PAGE: &[Field] = &[
    req("annotations_total", Ty::Int),
    req("annotations_offset", Ty::Int),
    nul("annotations_next_offset", Ty::Int),
];

const TASK_BLOCKED: &[Field] = &[req("blocked", Ty::Bool)];

const TASK_TOKENS: &[Field] = &[req_of("tokens", Ty::Array, MEASUREMENT)];

/// The same measurements on the export row — but OPTIONAL, because
/// `export_task` omits the key entirely for a task with no measurement rather
/// than emitting `[]` (an always-present key would change the §3 export shape
/// for every store that never recorded a token, and `IMPORT_TASK_KEYS` is a
/// closed gate). Split from [`TASK_TOKENS`] rather than reusing it: the two
/// differ in exactly the way a client breaks on, and `task.get`'s key is not
/// allowed to go missing.
const TASK_EXPORT_TOKENS: &[Field] = &[opt_of("tokens", Ty::Array, MEASUREMENT)];

const PROJECT_LIST_ROW: &[Field] = &[
    req("id", Ty::Str),
    req("name", Ty::Str),
    nul("description", Ty::Str),
    req("archived", Ty::Bool),
    req("default", Ty::Bool),
];

/// The exported project record (D37) — the list row minus `default` (the
/// document carries the default once, at the top level) plus `created`.
const PROJECT_EXPORT_ROW: &[Field] = &[
    req("id", Ty::Str),
    req("name", Ty::Str),
    nul("description", Ty::Str),
    req("archived", Ty::Bool),
    req("created", Ty::Str),
];

const DOC_EXPORT_ROW: &[Field] = &[
    req("id", Ty::Str),
    nul("source", Ty::Str),
    req("title", Ty::Str),
    req("body", Ty::Str),
    req("created", Ty::Str),
    req("modified", Ty::Str),
];

const EVENT_ROW: &[Field] = &[
    req("id", Ty::Str),
    req("entity", Ty::Str),
    req("entity_id", Ty::Str),
    req("op", Ty::Str),
    // Every writer in this engine passes an object; `event.list` hands back
    // `null` for a row whose payload will not parse, which is a corrupt store
    // rather than a shape. Not descended into: the payload keys are per-op and
    // belong to the event-vocabulary guard in tests/engine.rs, not to the
    // envelope freeze.
    nul("payload", Ty::Object),
    req("ts", Ty::Str),
    nul("actor", Ty::Str),
];

const MEMORY_HIT_ROW: &[Field] = &[
    req("id", Ty::Str),
    req("kind", Ty::Str),
    req("title", Ty::Str),
    nul("source", Ty::Str),
    req("snippet", Ty::Str),
    req("rank", Ty::Num),
];

const TOKEN_BUCKETS_ROW: &[Field] = &[
    req("input_tokens", Ty::Int),
    req("output_tokens", Ty::Int),
    req("cache_read_tokens", Ty::Int),
    req("cache_creation_tokens", Ty::Int),
];
const TOKEN_BUCKETS: Shape = &[TOKEN_BUCKETS_ROW];

// ---- the frozen result shape, per method ------------------------------------
//
// One `const` per case. They are `const` rather than inline literals because a
// nested shape has to outlive the call, and rather than `static` because a
// `const` may be read from another `const`'s initializer — which is what lets
// the composed shapes below name their parts instead of repeating them.

const R_PROJECT_CREATE: Shape = &[&[
    req("id", Ty::Str),
    req("name", Ty::Str),
    req("default", Ty::Bool),
    // Always present, null only in a store with no default at all. §4 makes
    // this the key by which a caller who did NOT claim the default still learns
    // where a bare `task.add` will go.
    nul("current_default", Ty::Str),
]];

const R_PROJECT_LIST: Shape = &[&[
    req("count", Ty::Int),
    req_of("projects", Ty::Array, &[PROJECT_LIST_ROW]),
]];

const R_PROJECT_USE: Shape = &[&[
    req("name", Ty::Str),
    req("default", Ty::Bool),
    nul("previous", Ty::Str),
]];

const R_PROJECT_ARCHIVE: Shape = &[&[
    req("name", Ty::Str),
    req("archived", Ty::Bool),
    req("default_cleared", Ty::Bool),
]];

const R_TASK_ADD: Shape = &[&[
    req("id", Ty::Str),
    req("short_id", Ty::Int),
    req("status", Ty::Str),
    // §4/D21: the project the task actually landed in — for an inherited
    // default this is the only place the caller learns it.
    nul("project", Ty::Str),
    req("urgency", Ty::Num),
    nul("recurrence", Ty::Str),
]];

const R_TASK_LIST: Shape = &[&[
    req("count", Ty::Int),
    req_of(
        "tasks",
        Ty::Array,
        &[TASK_CORE, TASK_LIVE_TIME, TASK_BLOCKED, TASK_STATUS_FLAG],
    ),
]];

const R_TASK_GET: Shape = &[
    TASK_CORE,
    TASK_LIVE_TIME,
    TASK_RELATIONS,
    TASK_ANNOTATION_PAGE,
    TASK_TOKENS,
    TASK_BLOCKED,
    TASK_STATUS_FLAG,
];

const R_TASK_START: Shape = &[&[
    req("id", Ty::Str),
    req("status", Ty::Str),
    // Null only on the idempotent re-start of a task whose `active_since` is
    // missing; §4 documents the key itself as always present.
    nul("interval_started", Ty::Str),
]];

const R_TASK_STOP: Shape = &[&[req("status", Ty::Str), req("tracked", Ty::Str)]];

const R_TASK_DONE: Shape = &[&[
    req("status", Ty::Str),
    req("completed", Ty::Str),
    req("unblocked", Ty::Array),
    // Present only when the completed task carried a recurrence rule (D2). Not
    // descended into: it is the spawned instance's summary, and `task.add`'s
    // shape already freezes that vocabulary.
    opt("spawned", Ty::Object),
    // D50: present only when the completion self-reported no token counts.
    opt("tokens_hint", Ty::Str),
]];

const R_TASK_MODIFY: Shape = &[&[req("short_id", Ty::Int), req("_rev", Ty::Int)]];

const R_TASK_CANCEL: Shape = &[&[
    req("short_id", Ty::Int),
    req("status", Ty::Str),
    req("unblocked", Ty::Array),
]];

const R_TASK_REOPEN: Shape = &[&[req("short_id", Ty::Int), req("status", Ty::Str)]];

const R_TAG_ADD: Shape = &[&[req("short_id", Ty::Int), req("tags", Ty::Array)]];

const R_TAG_REMOVE: Shape = &[&[
    req("short_id", Ty::Int),
    req("tags", Ty::Array),
    req("removed", Ty::Array),
]];

const R_ANNOTATION_ADD: Shape = &[&[
    req("short_id", Ty::Int),
    req_of("annotation", Ty::Object, ANNOTATION),
]];

const R_TOKEN_ADD: Shape = &[&[
    req("short_id", Ty::Int),
    req_of("measurement", Ty::Object, MEASUREMENT),
]];

const RECOMPUTE_ROW: &[Field] = &[
    req("task", Ty::Int),
    req("action", Ty::Str),
    req_of("before", Ty::Object, TOKEN_BUCKETS),
    req_of("after", Ty::Object, TOKEN_BUCKETS),
];

const R_TOKENS_RECOMPUTE: Shape = &[&[
    req("dry_run", Ty::Bool),
    req_of("tasks", Ty::Array, &[RECOMPUTE_ROW]),
    req_of(
        "totals",
        Ty::Object,
        &[&[req("before", Ty::Int), req("after", Ty::Int)]],
    ),
]];

const R_DEPENDENCY: Shape = &[&[
    req("short_id", Ty::Int),
    req("depends_on", Ty::Array),
    req("blocked", Ty::Bool),
]];

const R_MEMORY_ADD: Shape = &[&[
    req("id", Ty::Str),
    req("title", Ty::Str),
    req("created", Ty::Str),
]];

const R_MEMORY_SEARCH: Shape = &[&[
    req("count", Ty::Int),
    req_of("hits", Ty::Array, &[MEMORY_HIT_ROW]),
]];

const R_MEMORY_REMOVE: Shape = &[&[req("id", Ty::Str), req("removed", Ty::Bool)]];

const IMPORTED_DOC_ROW: &[Field] = &[
    req("id", Ty::Str),
    req("title", Ty::Str),
    nul("source", Ty::Str),
];

const R_MEMORY_IMPORT: Shape = &[&[
    req("imported", Ty::Int),
    req_of("docs", Ty::Array, &[IMPORTED_DOC_ROW]),
]];

/// The group key is *named by* `group_by` and the metric columns are exactly
/// the ones the caller selected, so this row shape belongs to the case that
/// asks for it (`group_by: "project"`, three metrics) rather than to the method.
const SUMMARY_GROUP_ROW: &[Field] = &[
    req("project", Ty::Str),
    req("count", Ty::Int),
    req("est_total", Ty::Str),
    req("overdue", Ty::Int),
];

const R_REPORT_SUMMARY: Shape = &[&[
    req_of("groups", Ty::Array, &[SUMMARY_GROUP_ROW]),
    req("generated", Ty::Str),
]];

const R_STORE_EXPORT: Shape = &[&[
    req_of(
        "tasks",
        Ty::Array,
        &[
            TASK_CORE,
            TASK_EXPORT_TIME,
            TASK_EXPORT_TOKENS,
            TASK_RELATIONS,
            TASK_STATUS_FLAG,
        ],
    ),
    req("dropped_dependencies", Ty::Int),
    req_of("projects", Ty::Array, &[PROJECT_EXPORT_ROW]),
    req_of("docs", Ty::Array, &[DOC_EXPORT_ROW]),
    nul("default_project", Ty::Str),
]];

const R_STORE_IMPORT: Shape = &[&[
    req("imported", Ty::Int),
    req("projects_imported", Ty::Int),
    // The NAMES of the projects the import had to mint because a task named one
    // the document did not carry — a list, not a count, because the operator
    // has to be able to see which ones appeared out of nowhere. No row shape to
    // descend into: they are plain strings.
    req("projects_created", Ty::Array),
    req("docs_imported", Ty::Int),
    nul("default_project", Ty::Str),
]];

const R_EVENT_LIST: Shape = &[&[
    req("count", Ty::Int),
    req_of("events", Ty::Array, &[EVENT_ROW]),
]];

const R_EVENT_REVERT: Shape = &[&[
    req_of(
        "reverted",
        Ty::Object,
        &[&[
            req("event", Ty::Str),
            req("op", Ty::Str),
            req("ts", Ty::Str),
        ]],
    ),
    req("short_id", Ty::Int),
    req("title", Ty::Str),
    // Per-op: what each inverse put back is that inverse's own vocabulary
    // (engine/undo.rs), not part of the envelope freeze.
    req("restored", Ty::Object),
    req("_rev", Ty::Int),
]];

const R_REMINDER_FIRE: Shape = &[&[
    req("fired", Ty::Bool),
    req("short_id", Ty::Int),
    req("at", Ty::Str),
]];

const R_CORE_CAPABILITIES: Shape = &[&[
    req("api", Ty::Str),
    req("methods", Ty::Array),
    req("params", Ty::Object),
    req("features", Ty::Array),
    nul("default_project", Ty::Str),
]];

// ---- envelope shapes --------------------------------------------------------

const E_SUCCESS: Shape = &[&[
    req("tasqx", Ty::Str),
    req("id", Ty::Str),
    req("ok", Ty::Bool),
    req("result", Ty::Object),
]];

/// The stdio call that sent no `id`: §4 makes `id` optional there, and the
/// answer is a response with **no `id` key** — not `"id": null`. A client that
/// keys a pending-request map on the presence of `id` breaks on the difference.
const E_SUCCESS_NO_ID: Shape = &[&[
    req("tasqx", Ty::Str),
    req("ok", Ty::Bool),
    req("result", Ty::Object),
]];

const E_ERROR: Shape = &[&[
    req("tasqx", Ty::Str),
    req("id", Ty::Str),
    req("ok", Ty::Bool),
    req("error", Ty::Object),
]];

/// A request too malformed to read carries no id to echo, so the key is absent
/// rather than null — the same rule the success envelope follows.
const E_ERROR_NO_ID: Shape = &[&[
    req("tasqx", Ty::Str),
    req("ok", Ty::Bool),
    req("error", Ty::Object),
]];

const E_ERROR_BODY: Shape = &[&[
    req("code", Ty::Str),
    req("message", Ty::Str),
    // §4: omitted entirely when there is none, so a client sees no key rather
    // than a null it would have to tell apart from a payload.
    opt("data", Ty::Object),
]];

/// The version refusal, with the one `data` key a client needs to negotiate
/// down: without it, a client that guessed wrong has nothing to guess from.
const E_ERROR_VERSION: Shape = &[&[
    req("code", Ty::Str),
    req("message", Ty::Str),
    req_of("data", Ty::Object, &[&[req("supported", Ty::Str)]]),
]];

// ---- cases ------------------------------------------------------------------

/// One conformance case: seed a store, call a method, freeze the answer.
struct Case {
    method: &'static str,
    /// What this case exercises. Printed on failure, because two cases for one
    /// method ("with a recurrence" / "without one") are otherwise
    /// indistinguishable in a panic message.
    note: &'static str,
    /// Seeds the engine and returns the params for the call.
    setup: Box<dyn Fn(&Engine) -> Value>,
    /// The frozen `result` shape.
    shape: Shape,
}

fn case(
    method: &'static str,
    note: &'static str,
    setup: impl Fn(&Engine) -> Value + 'static,
    shape: Shape,
) -> Case {
    Case {
        method,
        note,
        setup: Box::new(setup),
        shape,
    }
}

/// A task carrying a value in every optional column, so the nullable keys are
/// pinned on their non-null branch and not only as `null`. The dates are
/// absolute and in the past, so the row lands `pending` (a future `wait` would
/// make it `backlog`) and nothing here depends on the wall clock.
fn rich_task(e: &Engine) -> Value {
    e.project_create(&json!({ "name": "work", "description": "the day job" }))
        .expect("create project");
    e.task_add(&json!({
        "title": "ship the freeze",
        "project": "work",
        "priority": "H",
        "due": "2026-07-20T17:00:00Z",
        "scheduled": "2026-07-19T09:00:00Z",
        "wait": "2026-07-18T09:00:00Z",
        "estimate": "PT4H",
        "tags": ["release", "api"],
        "recurrence": "every 3 days",
        "remind": "-1h",
    }))
    .expect("add rich task")
}

fn plain_task(e: &Engine) -> Value {
    e.task_add(&json!({ "title": "a plain task" }))
        .expect("add task")
}

/// A self-report measurement — the one `token.add` vocabulary the engine
/// accepts from a caller with a straight face (D50).
fn self_report(r: i64) -> Value {
    json!({
        "ref": r,
        "tool": "claude-code",
        "source": "self-report",
        "model": "opus",
        "input_tokens": 120,
        "output_tokens": 40,
        "cache_read_tokens": 8,
        "cache_creation_tokens": 4,
        "confidence": "medium",
    })
}

/// Every case. Set-compared against `PARAMS` by [`every_frozen_method_is_covered`],
/// so this list cannot fall behind the API without the suite saying so.
fn cases() -> Vec<Case> {
    vec![
        case(
            "project.create",
            "the first create claims the default",
            |_| json!({ "name": "work", "description": "the day job" }),
            R_PROJECT_CREATE,
        ),
        case(
            "project.list",
            "one live project",
            |e| {
                e.project_create(&json!({ "name": "work", "description": "the day job" }))
                    .expect("create");
                json!({ "include_archived": false })
            },
            R_PROJECT_LIST,
        ),
        case(
            "project.use",
            "moving the default off the project that claimed it",
            |e| {
                e.project_create(&json!({ "name": "work" }))
                    .expect("create");
                e.project_create(&json!({ "name": "home" }))
                    .expect("create");
                json!({ "name": "home" })
            },
            R_PROJECT_USE,
        ),
        case(
            "project.archive",
            "archiving the default clears it (D22)",
            |e| {
                e.project_create(&json!({ "name": "work" }))
                    .expect("create");
                json!({ "name": "work" })
            },
            R_PROJECT_ARCHIVE,
        ),
        case(
            "task.add",
            "every optional column filled",
            |e| {
                e.project_create(&json!({ "name": "work" }))
                    .expect("create");
                json!({
                    "title": "ship the freeze",
                    "project": "work",
                    "priority": "H",
                    "due": "2026-07-20T17:00:00Z",
                    "scheduled": "2026-07-19T09:00:00Z",
                    "wait": "2026-07-18T09:00:00Z",
                    "estimate": "PT4H",
                    "tags": ["release", "api"],
                    "recurrence": "every 3 days",
                    "remind": "-1h",
                })
            },
            R_TASK_ADD,
        ),
        case(
            "task.list",
            "the default projection (no `fields`), with a blocked row",
            |e| {
                rich_task(e);
                plain_task(e);
                e.dependency_add(&json!({ "ref": 2, "depends_on": 1 }))
                    .expect("dep");
                json!({ "filter": "", "sort": ["-urgency"], "limit": 20 })
            },
            R_TASK_LIST,
        ),
        case(
            "task.get",
            "a pending task with tags, a dependency, an annotation and a measurement",
            |e| {
                rich_task(e);
                plain_task(e);
                e.dependency_add(&json!({ "ref": 1, "depends_on": 2 }))
                    .expect("dep");
                e.annotation_add(&json!({ "ref": 1, "body": "a note" }))
                    .expect("annotate");
                e.token_add(&self_report(1)).expect("token");
                json!({ "ref": 1 })
            },
            R_TASK_GET,
        ),
        case(
            "task.get",
            "an active task, so `active_since` is pinned on its non-null branch",
            |e| {
                plain_task(e);
                e.task_start(&json!({ "ref": 1 })).expect("start");
                e.annotation_add(&json!({ "ref": 1, "body": "still going" }))
                    .expect("annotate");
                e.token_add(&self_report(1)).expect("token");
                json!({ "ref": 1 })
            },
            R_TASK_GET,
        ),
        case(
            "task.start",
            "pending -> active",
            |e| {
                plain_task(e);
                json!({ "ref": 1, "keep": false, "client": "conformance" })
            },
            R_TASK_START,
        ),
        case(
            "task.stop",
            "active -> pending, the interval folded into tracked time",
            |e| {
                plain_task(e);
                e.task_start(&json!({ "ref": 1 })).expect("start");
                json!({ "ref": 1 })
            },
            R_TASK_STOP,
        ),
        case(
            "task.done",
            "a recurring completion: `spawned` present, `tokens_hint` present (D50)",
            |e| {
                e.task_add(&json!({
                    "title": "water the plants",
                    "due": "2026-07-20T17:00:00Z",
                    "recurrence": "every 3 days",
                }))
                .expect("add");
                json!({ "ref": 1 })
            },
            R_TASK_DONE,
        ),
        case(
            "task.done",
            "a self-reported completion: neither optional key appears",
            |e| {
                plain_task(e);
                json!({
                    "ref": 1,
                    "tool": "claude-code",
                    "model": "opus",
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "cache_read_tokens": 0,
                    "cache_creation_tokens": 0,
                    "session_id": "s-1",
                    "client": "conformance",
                })
            },
            R_TASK_DONE,
        ),
        case(
            "task.modify",
            "a `set` under an expected_rev that matches",
            |e| {
                plain_task(e);
                json!({ "ref": 1, "set": { "priority": "M" }, "expected_rev": 1 })
            },
            R_TASK_MODIFY,
        ),
        case(
            "task.cancel",
            "cancelling a blocker releases its dependents (D11)",
            |e| {
                plain_task(e);
                plain_task(e);
                e.dependency_add(&json!({ "ref": 2, "depends_on": 1 }))
                    .expect("dep");
                json!({ "ref": 1 })
            },
            R_TASK_CANCEL,
        ),
        case(
            "task.reopen",
            "done -> pending",
            |e| {
                plain_task(e);
                e.task_done(&json!({ "ref": 1 })).expect("done");
                json!({ "ref": 1 })
            },
            R_TASK_REOPEN,
        ),
        case(
            "tag.add",
            "the response carries the task's FULL tag set",
            |e| {
                plain_task(e);
                json!({ "ref": 1, "tags": ["blocking"] })
            },
            R_TAG_ADD,
        ),
        case(
            "tag.remove",
            "the full set plus what came off (D52)",
            |e| {
                e.task_add(&json!({ "title": "tagged", "tags": ["a", "b"] }))
                    .expect("add");
                json!({ "ref": 1, "tags": ["a"] })
            },
            R_TAG_REMOVE,
        ),
        case(
            "annotation.add",
            "the note comes back with its id and timestamp",
            |e| {
                plain_task(e);
                json!({ "ref": 1, "body": "a note worth keeping" })
            },
            R_ANNOTATION_ADD,
        ),
        case(
            "token.add",
            "one self-report measurement",
            |e| {
                plain_task(e);
                self_report(1)
            },
            R_TOKEN_ADD,
        ),
        case(
            "tokens.recompute",
            "the safe default (dry_run) over one stored log-parse row",
            |e| {
                plain_task(e);
                e.task_done(&json!({ "ref": 1 })).expect("done");
                e.token_add(&json!({
                    "ref": 1,
                    "tool": "claude-code",
                    "source": "log-parse",
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "cache_read_tokens": 0,
                    "cache_creation_tokens": 0,
                    "confidence": "low",
                }))
                .expect("log-parse row");
                json!({})
            },
            R_TOKENS_RECOMPUTE,
        ),
        case(
            "dependency.add",
            "the edge, the full dependency list, and the derived blocked flag",
            |e| {
                plain_task(e);
                plain_task(e);
                json!({ "ref": 2, "depends_on": 1 })
            },
            R_DEPENDENCY,
        ),
        case(
            "dependency.remove",
            "the same shape as add, so a client reads one answer",
            |e| {
                plain_task(e);
                plain_task(e);
                e.dependency_add(&json!({ "ref": 2, "depends_on": 1 }))
                    .expect("dep");
                json!({ "ref": 2, "depends_on": 1 })
            },
            R_DEPENDENCY,
        ),
        case(
            "memory.add",
            "one doc",
            |_| json!({ "title": "the freeze", "body": "v1 is stable", "source": "DESIGN.md" }),
            R_MEMORY_ADD,
        ),
        case(
            "memory.search",
            "a hit across docs and annotations",
            |e| {
                e.memory_add(&json!({
                    "title": "the freeze",
                    "body": "conformance is the contract of record",
                    "source": "DESIGN.md",
                }))
                .expect("doc");
                json!({ "query": "conformance", "limit": 10, "scope": "all", "raw": false })
            },
            R_MEMORY_SEARCH,
        ),
        case(
            "memory.remove",
            "an existing doc, by id",
            |e| {
                let added = e
                    .memory_add(&json!({ "title": "throwaway", "body": "gone soon" }))
                    .expect("doc");
                json!({ "id": added["id"] })
            },
            R_MEMORY_REMOVE,
        ),
        case(
            "memory.import",
            "a two-doc batch, one with a source and one without",
            |_| {
                json!({ "docs": [
                    { "title": "one", "body": "first", "source": "a.md" },
                    { "title": "two", "body": "second" },
                ]})
            },
            R_MEMORY_IMPORT,
        ),
        case(
            "report.summary",
            "grouped by project, with three metrics selected",
            |e| {
                rich_task(e);
                json!({
                    "group_by": "project",
                    "filter": "",
                    "metrics": ["count", "est_total", "overdue"],
                    "all": false,
                })
            },
            R_REPORT_SUMMARY,
        ),
        case(
            "store.export",
            "a document carrying a done task, an active task, a project and a doc",
            |e| {
                rich_task(e);
                plain_task(e);
                e.task_start(&json!({ "ref": 2 })).expect("start");
                e.task_stop(&json!({ "ref": 2 })).expect("stop");
                e.task_done(&json!({ "ref": 2 })).expect("done");
                e.task_start(&json!({ "ref": 1 })).expect("start");
                // Both tasks are annotated, not just one: the row shape below
                // `annotations` is checked per row, so a task without one would
                // leave that row's nested keys unexamined.
                e.annotation_add(&json!({ "ref": 1, "body": "a note" }))
                    .expect("annotate");
                e.annotation_add(&json!({ "ref": 2, "body": "and another" }))
                    .expect("annotate");
                e.memory_add(&json!({ "title": "kept", "body": "knowledge", "source": "x.md" }))
                    .expect("doc");
                // A start/stop inside one test run banks zero seconds — both
                // timestamps land in the same second — and `tracked_seconds` is
                // omitted when it is zero, so the export above would carry the
                // key on no task at all. It is set here the way a restore sets
                // it: through `store.import`, which is the only door that
                // writes banked time directly and is exactly the path D42's key
                // exists for.
                let mut doc = e.store_export(&json!({})).expect("export to edit");
                let task = doc["tasks"]
                    .as_array_mut()
                    .expect("the export carries tasks")
                    .iter_mut()
                    .find(|t| t["short_id"] == 2)
                    .expect("the completed task is in the export");
                task["tracked_seconds"] = json!(3600);
                e.store_import(&doc).expect("re-import with banked time");
                // A measurement, because `tokens` is a THIRD conditional export
                // key beside `tracked_seconds` and `active_since`, and this
                // fixture seeded no measurement at all — so the key appeared on
                // no exported row, and its absence from the frozen shape above
                // was invisible: renaming `out["tokens"]` in transfer.rs left
                // this entire suite green (review finding).
                //
                // AFTER the re-import above, not before, and the order is the
                // point. Seeded earlier, the measurement rides along in the
                // document that gets edited and fed back through `store.import`
                // — and `IMPORT_TASK_KEYS` is a closed gate, so a renamed export
                // key would blow up in that helper with a message about an
                // unknown import field, several steps before the shape check
                // ever ran. Red either way, but a red run that names the wrong
                // thing sends the next reader to the wrong file.
                e.token_add(&self_report(1)).expect("measure");
                json!({})
            },
            R_STORE_EXPORT,
        ),
        case(
            "store.import",
            "a real export document, round-tripped into a fresh store",
            |_| {
                // Built from an export rather than by hand: the import gate is
                // closed (`IMPORT_TASK_KEYS`), so a hand-written payload would
                // freeze this file's idea of a task document instead of the
                // engine's.
                let source = Engine::open_in_memory().expect("source store");
                rich_task(&source);
                source
                    .memory_add(&json!({ "title": "kept", "body": "knowledge" }))
                    .expect("doc");
                source.store_export(&json!({})).expect("export")
            },
            R_STORE_IMPORT,
        ),
        case(
            "event.list",
            "the audit log, newest first",
            |e| {
                plain_task(e);
                json!({ "limit": 50 })
            },
            R_EVENT_LIST,
        ),
        case(
            "event.revert",
            "undoing the newest event, a `tag.remove` (D54)",
            |e| {
                e.task_add(&json!({ "title": "tagged", "tags": ["a"] }))
                    .expect("add");
                e.tag_remove(&json!({ "ref": 1, "tags": ["a"] }))
                    .expect("untag");
                json!({})
            },
            R_EVENT_REVERT,
        ),
        case(
            "reminder.fire",
            "a first fire, so `fired` is true",
            |e| {
                e.task_add(&json!({
                    "title": "ripe",
                    "due": "2026-07-20T17:00:00Z",
                    "remind": "-1h",
                }))
                .expect("add");
                json!({ "ref": 1, "at": "2026-07-20T16:00:00Z" })
            },
            R_REMINDER_FIRE,
        ),
        case(
            "core.capabilities",
            "the feature-detection read every client starts with",
            |e| {
                e.project_create(&json!({ "name": "work" }))
                    .expect("create");
                json!({})
            },
            R_CORE_CAPABILITIES,
        ),
    ]
}

// ---- the floor --------------------------------------------------------------

/// The floor, derived from the dispatch table rather than counted by hand.
///
/// `PARAMS` is the runtime method table `core.capabilities` publishes and the
/// params gate enforces, and dispatch.rs's own guard already proves it matches
/// the `dispatch` match arm-for-arm. So set equality here means: every method
/// this build serves is shape-frozen, and every shape here freezes a method
/// that exists.
///
/// Set equality, not a count: a count is satisfied by a duplicate, which is
/// exactly how a list stops guarding while still reporting green.
#[test]
fn every_frozen_method_is_covered() {
    let declared: BTreeSet<&str> = PARAMS.iter().map(|(m, _, _)| *m).collect();
    let covered: BTreeSet<&str> = cases().iter().map(|c| c.method).collect();

    let missing: Vec<&&str> = declared.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "these methods are served by this build and frozen by no conformance case: {missing:?}. \
         A method that ships without a case ships without a contract — add one to `cases()` in \
         tests/conformance.rs pinning the keys its result carries."
    );

    let stale: Vec<&&str> = covered.difference(&declared).collect();
    assert!(
        stale.is_empty(),
        "these conformance cases name methods `PARAMS` does not declare: {stale:?}. Either the \
         method was removed (which is a v1 break — check dispatch::API_VERSION) or the case has \
         a typo and has been freezing nothing."
    );
}

// ---- the success envelope + the per-method result shape ---------------------

/// The point of the file: for every case the request goes through
/// `handle_envelope` — the real transport seam, not the handler — and both
/// halves of the answer are pinned. First the envelope, then the result.
#[test]
fn every_method_answers_the_frozen_envelope_and_result_shape() {
    for c in cases() {
        let engine = Engine::open_in_memory().expect("in-memory store");
        let params = (c.setup)(&engine);
        let request = json!({
            "tasqx": API_VERSION,
            "id": "conf-1",
            "method": c.method,
            "params": params,
        });
        let response = handle_envelope(&engine, &request.to_string());
        let at = format!("{} ({})", c.method, c.note);

        assert_eq!(
            response["ok"],
            Value::Bool(true),
            "{at}: this case must succeed, and it answered {}",
            serde_json::to_string(&response).unwrap_or_default()
        );
        check_shape(&response, E_SUCCESS, &format!("{at} response"));
        assert_eq!(
            response["tasqx"], API_VERSION,
            "{at}: the response carries the API major version it was served under"
        );
        assert_eq!(
            response["id"], "conf-1",
            "{at}: the correlation id is echoed verbatim — a socket client multiplexes on it"
        );

        check_shape(&response["result"], c.shape, &format!("{at} result"));
    }
}

/// `id` is optional on stdio (§4). See [`E_SUCCESS_NO_ID`] for why its absence
/// is a shape fact rather than a formatting detail.
#[test]
fn a_request_without_an_id_gets_a_response_without_an_id() {
    let engine = Engine::open_in_memory().expect("in-memory store");
    let response = handle_envelope(
        &engine,
        &json!({ "tasqx": API_VERSION, "method": "core.capabilities" }).to_string(),
    );
    check_shape(&response, E_SUCCESS_NO_ID, "id-less response");
}

/// `dispatch` and `handle_envelope` are two entry points onto one handler, and
/// §4 says so in as many words ("the same envelope flows over stdio and over
/// the daemon socket"; the CLI calls `dispatch` directly). A result that
/// differed between them would mean the frozen shape is frozen on one surface
/// only.
#[test]
fn the_envelope_wraps_the_same_result_the_direct_call_returns() {
    for c in cases() {
        let via_envelope = {
            let engine = Engine::open_in_memory().expect("in-memory store");
            let params = (c.setup)(&engine);
            let request =
                json!({ "tasqx": API_VERSION, "method": c.method, "params": params }).to_string();
            handle_envelope(&engine, &request)["result"].clone()
        };
        let direct = {
            let engine = Engine::open_in_memory().expect("in-memory store");
            let params = (c.setup)(&engine);
            dispatch(&engine, c.method, &params).unwrap_or_else(|e| {
                panic!("{} ({}) failed on the direct call: {e}", c.method, c.note)
            })
        };
        // Compared by key set, not by value: ids are UUIDv7 and timestamps come
        // from `now()`, so two runs of one case differ in content by design.
        // The contract under test is the shape, and the shape is the keys.
        assert_eq!(
            key_set(&via_envelope),
            key_set(&direct),
            "{} ({}): `handle_envelope` and `dispatch` answered different key sets, so one of \
             the two surfaces is not serving the frozen shape",
            c.method,
            c.note
        );
    }
}

fn key_set(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// Walk a shape beside a response, recording every `opt(...)` key it declares
/// and every one the response actually carried. Keyed by the path with array
/// indices collapsed to `[]`, so a key that appears on any row counts as seen.
fn collect_optionals(
    value: &Value,
    shape: Shape,
    path: &str,
    declared: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
) {
    let Some(obj) = value.as_object() else { return };
    for group in shape {
        for f in *group {
            let here = format!("{path}.{}", f.key);
            if f.optional {
                declared.insert(here.clone());
                if obj.contains_key(f.key) {
                    seen.insert(here.clone());
                }
            }
            if f.inner.is_empty() {
                continue;
            }
            match obj.get(f.key) {
                Some(Value::Object(_)) => {
                    collect_optionals(&obj[f.key], f.inner, &here, declared, seen)
                }
                Some(Value::Array(rows)) => {
                    let row_path = format!("{here}[]");
                    for row in rows {
                        collect_optionals(row, f.inner, &row_path, declared, seen);
                    }
                    // An array the fixture left empty declares its rows'
                    // optional keys and can never observe them, so the shape is
                    // still walked with a null stand-in to register them.
                    if rows.is_empty() {
                        collect_optionals(&Value::Null, f.inner, &row_path, declared, seen);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Optional keys that no call through the JSON API can make appear, each with
/// the reason. Same discipline as
/// [`UNREACHABLE_WITHOUT_FAULT_INJECTION`]: an unexplained entry is a gap
/// wearing a costume.
const OPTIONAL_KEYS_NO_FIXTURE_CAN_PRODUCE: &[(&str, &str)] = &[
    (
        "result.status_unrecognized",
        "set only for a task row whose `status` column holds text `Status::parse` rejects — a \
         store shape no writer of this engine can produce, reachable only by writing the row \
         with raw SQL",
    ),
    (
        "result.tasks[].status_unrecognized",
        "the same flag on the collection surfaces (`task.list`, `store.export`), and unreachable \
         for the same reason",
    ),
];

/// An `opt(...)` that no case ever observes present is a declaration nothing
/// checks: its type is never compared, and a rename of a conditional key would
/// slide straight through. So every optional key in every frozen shape must be
/// produced by at least one case, or be argued unreachable above.
#[test]
fn every_optional_key_is_observed_present_by_some_case() {
    let mut declared = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for c in cases() {
        let engine = Engine::open_in_memory().expect("in-memory store");
        let params = (c.setup)(&engine);
        let result = dispatch(&engine, c.method, &params)
            .unwrap_or_else(|e| panic!("{} ({}): {e}", c.method, c.note));
        collect_optionals(&result, c.shape, "result", &mut declared, &mut seen);
    }

    let excused: BTreeSet<&str> = OPTIONAL_KEYS_NO_FIXTURE_CAN_PRODUCE
        .iter()
        .map(|(k, _)| *k)
        .collect();

    let stale: Vec<&&str> = excused.iter().filter(|k| !declared.contains(**k)).collect();
    assert!(
        stale.is_empty(),
        "these keys are excused as unproducible but no shape declares them any more: {stale:?}"
    );

    let never_seen: Vec<&String> = declared
        .difference(&seen)
        .filter(|k| !excused.contains(k.as_str()))
        .collect();
    assert!(
        never_seen.is_empty(),
        "these keys are declared optional and no case ever produced one: {never_seen:?}. An \
         optional key nothing observes is a line of documentation, not a frozen shape — its \
         type is never checked and a rename of it would pass this suite. Give it a fixture that \
         emits it, or record why none can in OPTIONAL_KEYS_NO_FIXTURE_CAN_PRODUCE."
    );
}

// ---- the error contract -----------------------------------------------------

/// Every distinct failure the transport can produce, as (label, request text,
/// expected code). Written against raw request text because the version and
/// parse failures are reachable only through the envelope.
fn error_cases() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "an unsupported major version",
            json!({ "tasqx": "2", "id": "e", "method": "core.capabilities" }).to_string(),
            "unsupported_version",
        ),
        (
            "a method this build does not serve",
            json!({ "tasqx": "1", "id": "e", "method": "task.teleport" }).to_string(),
            "bad_request",
        ),
        (
            "a params key the method does not read (D33)",
            json!({ "tasqx": "1", "id": "e", "method": "task.add",
                    "params": { "title": "x", "prioritee": "H" } })
            .to_string(),
            "bad_request",
        ),
        (
            "a ref that names nothing",
            json!({ "tasqx": "1", "id": "e", "method": "task.get", "params": { "ref": 999 } })
                .to_string(),
            "not_found",
        ),
        (
            "an illegal lifecycle transition",
            json!({ "tasqx": "1", "id": "e", "method": "task.stop", "params": { "ref": 1 } })
                .to_string(),
            "conflict",
        ),
        (
            "a request that is not JSON at all",
            "{not json".to_string(),
            "bad_request",
        ),
    ]
}

/// The error envelope, per case: the same four keys as a success with `error`
/// in `result`'s place, `ok:false`, and a code from the frozen set.
#[test]
fn every_failure_answers_the_frozen_error_envelope() {
    for (label, request, expected_code) in error_cases() {
        let engine = Engine::open_in_memory().expect("in-memory store");
        // One pending task, so the `task.stop` case fails on the transition
        // rather than on the ref — a case has to fail for the reason it claims.
        plain_task(&engine);
        let response = handle_envelope(&engine, &request);

        let envelope = if response.get("id").is_some() {
            E_ERROR
        } else {
            E_ERROR_NO_ID
        };
        check_shape(&response, envelope, &format!("{label} response"));

        assert_eq!(
            response["tasqx"], API_VERSION,
            "{label}: a failure is still served under the API version — a client parsing the \
             envelope must not have to special-case errors"
        );
        assert_eq!(
            response["ok"],
            Value::Bool(false),
            "{label}: a refusal that answers ok:true is this project's named recurring defect"
        );
        check_shape(&response["error"], E_ERROR_BODY, &format!("{label} error"));
        assert_eq!(
            response["error"]["code"], expected_code,
            "{label}: the code is the machine-readable half of the contract and callers branch \
             on it; the message is free text and they must not"
        );
        assert!(
            !response["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{label}: an empty message leaves the caller with a code and no way to act on it"
        );
    }
}

/// `unsupported_version` carries the version this build DOES serve, in `data`.
#[test]
fn an_unsupported_version_names_the_version_that_is_served() {
    let engine = Engine::open_in_memory().expect("in-memory store");
    let response = handle_envelope(
        &engine,
        &json!({ "tasqx": "2", "id": "e", "method": "core.capabilities" }).to_string(),
    );
    check_shape(&response["error"], E_ERROR_VERSION, "version error");
    assert_eq!(response["error"]["data"]["supported"], API_VERSION);
}

/// A refusal must be structurally distinguishable from a success by a client
/// that reads nothing but the envelope: `ok` flips, and the two payload keys
/// are mutually exclusive. A response carrying both — or neither — would leave
/// a caller guessing, and "it looked fine and was not" is the failure this
/// whole file exists to make impossible.
#[test]
fn a_refusal_is_structurally_distinguishable_from_a_success() {
    let engine = Engine::open_in_memory().expect("in-memory store");
    let ok = handle_envelope(
        &engine,
        &json!({ "tasqx": API_VERSION, "id": "a", "method": "core.capabilities" }).to_string(),
    );
    let err = handle_envelope(
        &engine,
        &json!({ "tasqx": API_VERSION, "id": "a", "method": "nope.nope" }).to_string(),
    );

    assert_eq!(ok["ok"], Value::Bool(true));
    assert_eq!(err["ok"], Value::Bool(false));
    assert!(
        ok.get("result").is_some() && ok.get("error").is_none(),
        "a success carries `result` and only `result`"
    );
    assert!(
        err.get("error").is_some() && err.get("result").is_none(),
        "a refusal carries `error` and only `error` — a partial result beside an error is how a \
         caller ends up acting on half a write"
    );
}

/// The frozen code set, read out of `src/error.rs`'s own `as_str` arms.
///
/// Not a list typed here: a sixth `ErrorCode` is already a compile error inside
/// that exhaustive match, so scanning it means the sixth code arrives in this
/// test the day it is written — landing in neither the produced set nor the
/// excused set below, which is what fails the assertion.
fn declared_error_codes() -> BTreeSet<String> {
    let flat: String = include_str!("../src/error.rs")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let mut out = BTreeSet::new();
    for (i, _) in flat.match_indices("ErrorCode::") {
        let rest = &flat[i + "ErrorCode::".len()..];
        let name_len = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .count();
        let Some(lit) = rest[name_len..].strip_prefix("=>\"") else {
            continue;
        };
        if let Some(end) = lit.find('"') {
            out.insert(lit[..end].to_string());
        }
    }
    assert!(
        !out.is_empty(),
        "the scan of src/error.rs found no `ErrorCode::X => \"y\"` arms — the file was \
         restructured and this guard is now reading nothing"
    );
    out
}

/// Codes that exist in the domain but that a well-formed call cannot reach
/// through the JSON API, each with the reason it is excluded. Every exclusion is
/// argued; a code that lands here without an argument is a coverage gap wearing
/// a costume.
const UNREACHABLE_WITHOUT_FAULT_INJECTION: &[(&str, &str)] = &[(
    "internal",
    "raised only by a storage-layer failure (`From<rusqlite::Error>`) or by an engine \
     self-check a consistent build cannot trip. Reaching it from the API would mean corrupting \
     the SQLite file underneath a live call — a fault-injection test, not a contract this \
     suite can state.",
)];

/// Every code the domain declares is either produced by [`error_cases`] or
/// explicitly excused above. A code in neither set fails here — the mechanism
/// that drags the author of a sixth error code into writing its case.
#[test]
fn every_declared_error_code_is_produced_or_explicitly_excluded() {
    let declared = declared_error_codes();

    let produced: BTreeSet<String> = error_cases()
        .into_iter()
        .map(|(label, request, _)| {
            let engine = Engine::open_in_memory().expect("in-memory store");
            plain_task(&engine);
            handle_envelope(&engine, &request)["error"]["code"]
                .as_str()
                .unwrap_or_else(|| panic!("{label}: the failure carried no string `code`"))
                .to_string()
        })
        .collect();

    let excused: BTreeSet<String> = UNREACHABLE_WITHOUT_FAULT_INJECTION
        .iter()
        .map(|(c, _)| (*c).to_string())
        .collect();

    let unknown: Vec<&String> = excused.difference(&declared).collect();
    assert!(
        unknown.is_empty(),
        "these codes are excused as unreachable but the domain no longer declares them: \
         {unknown:?}"
    );

    let uncovered: Vec<&String> = declared
        .difference(&produced)
        .filter(|c| !excused.contains(*c))
        .collect();
    assert!(
        uncovered.is_empty(),
        "these error codes are part of the frozen v1 contract and no conformance case produces \
         one: {uncovered:?}. Add a case to `error_cases()` that provokes it, or — if it truly \
         cannot be reached through the API — add it to UNREACHABLE_WITHOUT_FAULT_INJECTION with \
         the reason."
    );

    let both: Vec<&String> = produced.intersection(&excused).collect();
    assert!(
        both.is_empty(),
        "{both:?} is both produced by a case and excused as unreachable — the excuse is stale, \
         and the reason recorded beside it is now false"
    );
}

/// The stable CLI exit-code mapping (§4) is the same contract seen from a shell:
/// scripts branch on the process status without parsing JSON, so the number a
/// code maps to may not move any more freely than the code's spelling may.
#[test]
fn the_error_codes_keep_their_stable_exit_numbers() {
    use tasqx_core::ErrorCode;
    let mapping = [
        (ErrorCode::BadRequest, 2),
        (ErrorCode::NotFound, 4),
        (ErrorCode::Conflict, 5),
        (ErrorCode::UnsupportedVersion, 6),
        (ErrorCode::Internal, 1),
    ];
    for (code, expected) in mapping {
        assert_eq!(
            code.exit_code(),
            expected,
            "{}: §4 publishes this exit code and shell scripts branch on it",
            code.as_str()
        );
    }

    // Every declared code is in the mapping above — otherwise a new code could
    // ship with an exit number nothing pins.
    let mapped: BTreeSet<&str> = mapping.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(
        mapped,
        declared_error_codes().iter().map(String::as_str).collect(),
        "the exit-code mapping and the declared code set have drifted"
    );

    // Distinct, or two different failures become one to a caller that has only
    // `$?` to go on.
    let numbers: BTreeSet<i32> = mapping.iter().map(|(c, _)| c.exit_code()).collect();
    assert_eq!(numbers.len(), mapping.len(), "two codes share an exit code");
    assert!(
        !numbers.contains(&0),
        "0 is success; an error code that exits 0 is the do-less-than-asked defect at the \
         process boundary"
    );
}

// ---- what `core.capabilities` publishes must be what is frozen --------------

/// `core.capabilities` is how a client feature-detects, so what it publishes is
/// itself part of the contract: a client that checks before calling must not be
/// promised a method this suite does not freeze, nor left ignorant of one it
/// does — and the accepted-key sets it publishes must be the ones the gate
/// enforces, or checking a call before making it proves nothing.
#[test]
fn the_published_capability_list_is_exactly_the_frozen_method_set() {
    let engine = Engine::open_in_memory().expect("in-memory store");
    let published = dispatch(&engine, "core.capabilities", &json!({})).expect("capabilities");

    assert_eq!(
        published["api"], API_VERSION,
        "`core.capabilities` publishes the major version the envelope demands"
    );

    let advertised: BTreeSet<&str> = published["methods"]
        .as_array()
        .expect("methods is an array")
        .iter()
        .map(|m| m.as_str().expect("method names are strings"))
        .collect();
    let frozen: BTreeSet<&str> = cases().iter().map(|c| c.method).collect();
    assert_eq!(
        advertised, frozen,
        "`core.capabilities` advertises a different method set than this suite freezes — a \
         client that feature-detects would be promised a shape nobody pinned"
    );

    let params = published["params"]
        .as_object()
        .expect("params is an object");
    for (method, accepted, _) in PARAMS {
        let published_keys: BTreeSet<&str> = params[*method]
            .as_array()
            .unwrap_or_else(|| panic!("`{method}` has no published params array"))
            .iter()
            .map(|k| k.as_str().expect("param names are strings"))
            .collect();
        let enforced: BTreeSet<&str> = accepted.iter().copied().collect();
        assert_eq!(
            published_keys, enforced,
            "`{method}`: the published accepted-key set differs from the enforced one, so a \
             client that checked before calling would still be refused"
        );
    }
}

// ---- the deliberate exclusion ----------------------------------------------

/// Every MCP tool, as `(tool name, core method)`, read out of `src/mcp.rs`'s own
/// `ToolSpec` literals.
///
/// `tool_specs()` is private to the `mcp` module, so this scans the source — the
/// same technique the dispatch-table drift guard uses, and for the same reason:
/// a second hand-kept copy of the registry is exactly the drift this repo keeps
/// paying for. The scan is proved complete against the live `tools/list` output
/// below, so it cannot quietly miss a spec.
fn mcp_tool_methods() -> BTreeMap<String, String> {
    let flat: String = include_str!("../src/mcp.rs")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let mut out = BTreeMap::new();
    for (i, _) in flat.match_indices("name:\"") {
        let rest = &flat[i + "name:\"".len()..];
        let Some(end) = rest.find('"') else { continue };
        let name = &rest[..end];
        let Some(after) = rest[end + 1..].strip_prefix(",method:\"") else {
            continue;
        };
        let Some(mend) = after.find('"') else {
            continue;
        };
        out.insert(name.to_string(), after[..mend].to_string());
    }
    out
}

/// The tools a live server actually advertises.
///
/// `Scope::Write` advertises the full registry (a read-only scope filters the
/// write tools out), so `tools/list` under it is the real set — asking under
/// `Read` would silently shrink every conclusion drawn from it to the five read
/// tools.
fn live_tool_names() -> BTreeSet<String> {
    let engine = Engine::open_in_memory().expect("in-memory store");
    let server = McpServer::new(&engine, Scope::Write);
    let listed = server
        .handle_message(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("tools/list is a request and yields a response");
    listed["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .expect("every tool has a name")
                .to_string()
        })
        .collect()
}

/// Half the exclusion: every tool *names* a method this file freezes.
///
/// This suite freezes the JSON API (§4) and not the MCP tool layer (§7). That is
/// sound only while every MCP tool bottoms out in a method this file freezes:
/// the tool wrapper may be renamed by a protocol revision, but the *data* an MCP
/// host receives is `dispatch`'s result, and `dispatch`'s results are pinned
/// above.
///
/// This test checks the routing table and nothing else. It was, for one commit,
/// the *whole* justification — and a review showed what that bought: inserting
/// a `count` → `total` rename into `tools_call`'s success arm left it green,
/// because a name map cannot see what happens to a result after `dispatch`
/// returns it. The half it does not cover is
/// [`every_mcp_tool_hands_back_the_frozen_result_of_its_method`]. Kept separate
/// rather than folded into it: this one fails in one line without running a
/// tool, and it is what proves [`mcp_tool_methods`]'s scan still agrees with the
/// live registry — which the other test *reads*, to find each tool's method, and
/// would otherwise trust unchecked.
#[test]
fn every_mcp_tool_routes_through_a_frozen_json_api_method() {
    let scanned = mcp_tool_methods();

    let live = live_tool_names();
    let scanned_names: BTreeSet<String> = scanned.keys().cloned().collect();
    assert_eq!(
        scanned_names, live,
        "the source scan of src/mcp.rs and the live tools/list disagree about which tools exist \
         — the scan has stopped reading the registry, and every conclusion below it is void"
    );

    let frozen: BTreeSet<&str> = PARAMS.iter().map(|(m, _, _)| *m).collect();
    let strays: Vec<(&String, &String)> = scanned
        .iter()
        .filter(|(_, method)| !frozen.contains(method.as_str()))
        .collect();
    assert!(
        strays.is_empty(),
        "these MCP tools do not route through a frozen JSON-API method: {strays:?}. This suite \
         excludes the MCP layer (D7: a host integration versioned by the MCP protocol revision, \
         not by `tasqx: \"1\"`) on the grounds that the data an MCP host sees IS a frozen \
         method's result. A tool with a shape of its own makes that false — either route it \
         through `dispatch`, or extend this suite to freeze the MCP surface too and say so in \
         DESIGN.md §11."
    );
}

/// The machine-readable block of a `tools/call` result, parsed.
///
/// The LAST content block, not the first. `tool_ok` emits one text block;
/// `tool_ok_with_view` — the `task.get` tool, the one rendered surface — puts a
/// human markdown view first and the JSON second. Reading `[0]` would work
/// today for seventeen tools and quietly start checking a markdown table
/// against a JSON shape the day an eighteenth grows a view.
fn tool_call_json(call: &Value, label: &str) -> Value {
    assert_eq!(
        call["isError"],
        json!(false),
        "{label}: the tool answered isError. The content was {:?} — a fixture that stopped \
         producing a successful call checks no shape at all, so fix the case's setup rather than \
         this assertion.",
        call["content"]
    );
    let content = call["content"].as_array().unwrap_or_else(|| {
        panic!("{label}: a CallToolResult carries a `content` array, got {call}")
    });
    // The JSON block is the LAST one, and `tasqx_get_task` is now allowed to
    // omit it: over the response budget, the transport spends the duplicate
    // block before it spends annotations (D66). Every case in this file is
    // small enough to stay under that budget and therefore still carries both
    // blocks — which is what keeps this guard live rather than quietly
    // checking a view. D56 pre-committed to saying so if that ever changed:
    // a fixture that grows past the budget will land here as "does not parse",
    // and the answer is to shrink the fixture, never to read a different block.
    let text = content
        .last()
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{label}: the last content block carries no text: {call}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("{label}: the tool's JSON block does not parse ({e}): {text}"))
}

/// The other half of the exclusion: what a tool hands back IS the frozen result.
///
/// [`every_mcp_tool_routes_through_a_frozen_json_api_method`] proves each tool
/// *names* a frozen method. That is not the same claim as "an MCP host receives
/// a frozen shape", and the gap between them is a live seam:
/// `McpServer::tools_call` already post-processes one method (`task.get`, which
/// gains a rendered view block), so nothing structural stops a second one from
/// renaming a key on the way out. A review proved the point by inserting a
/// `count` → `total` rename into the success arm — every conformance test stayed
/// green while the doc above went on asserting that the data a host sees is
/// `dispatch`'s result.
///
/// So: drive each tool through the real `tools/call` path with a case's own
/// fixture and params, and check what comes back against that case's frozen
/// shape. Params carry over verbatim because §7 maps tool arguments 1:1 onto the
/// method's params — the same property the tool-schema guard in `src/mcp.rs`
/// enforces — so this needs no second table of arguments to drift.
///
/// Driven from the LIVE `tools/list`, not from the source scan: a scan that
/// broke and returned nothing would make a scan-driven loop pass over zero
/// tools, which is the vacuous-green failure this project keeps paying for.
#[test]
fn every_mcp_tool_hands_back_the_frozen_result_of_its_method() {
    let methods = mcp_tool_methods();
    let all_cases = cases();
    let live = live_tool_names();
    assert!(
        !live.is_empty(),
        "tools/list advertised no tools, so this test checked nothing"
    );

    for tool in &live {
        let method = methods.get(tool).unwrap_or_else(|| {
            panic!("the source scan of src/mcp.rs has no method for the live tool `{tool}`")
        });
        // EVERY case for the method, not the first: `task.get` and `task.done`
        // each have two, and the second one is where the conditional keys live
        // (`spawned`, `tokens_hint`). Picking one would leave those unchecked on
        // the MCP path while reporting a tool covered.
        let matching: Vec<&Case> = all_cases.iter().filter(|c| c.method == method).collect();
        assert!(
            !matching.is_empty(),
            "the MCP tool `{tool}` routes to `{method}`, which no case in this file covers — \
             every_frozen_method_is_covered should already be red, and if it is not then \
             `{method}` is not in PARAMS and this tool reaches something unfrozen"
        );

        for c in matching {
            let engine = Engine::open_in_memory().expect("in-memory store");
            let server = McpServer::new(&engine, Scope::Write);
            let params = (c.setup)(&engine);
            let response = server
                .handle_message(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": { "name": tool, "arguments": params },
                }))
                .expect("tools/call is a request and yields a response");
            let label = format!("MCP tool {tool} -> {} ({})", c.method, c.note);
            let value = tool_call_json(&response["result"], &label);
            check_shape(&value, c.shape, &format!("{label}: result"));
        }
    }
}
