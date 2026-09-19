//! The JSON API reference page: one section per method (#647).
//!
//! # Nothing on this page is retyped
//!
//! Every column comes from the surface it describes, and each has a guard at
//! the bottom of this file:
//!
//! | What | Where it comes from |
//! |---|---|
//! | the method list | `tasqx_core::PARAMS` |
//! | each method's parameter NAMES | `tasqx_core::PARAMS` |
//! | which of them are required | [`super::METHODS`], itself bound to the engine's own "missing required field" complaint |
//! | each parameter's description and type | the MCP tool's `inputSchema`, when a tool maps to the method; [`PARAM_DOCS`] for the methods that have none |
//! | the response shape | `tasqx_core::docs::result_shape`, held equal to the conformance freeze |
//! | the CLI verb that reaches it | [`super::VERBS`] + `cmddoc`'s own summary |
//! | the MCP tool that reaches it, or why none does | `tasqx_core::mcp::tool_docs` / `unexposed_methods` |
//!
//! The one hand-written thing left is the EXAMPLE — and even there, most
//! methods carry a response captured from the real binary on the pinned day
//! (`docs-fixtures`, D149) rather than a plausible-looking paste. The ones
//! that cannot are marked "illustrative" on the page WITH the reason, because a
//! response nobody can reproduce is exactly what a reader must not mistake for
//! one they can (see [`EXAMPLES`]).
//!
//! # Why the prose at the top stayed
//!
//! The envelope, the error table and the feature-detection handshake are the
//! page's opening, unchanged: they describe the transport, not a method, and
//! every method section below assumes them. They moved here verbatim when the
//! per-method reference took the rest of the page.

use serde_json::Value;

use super::{
    capabilities_snippet, field_list, h3, lead, note, p, page_close, page_open, param_table,
    pre_plain, ref_section, snippet, table, table_owned, tabs, term_block, term_screen,
    verb_summary, Param, METHODS, VERBS,
};
use crate::html::esc;

// ============================================================================
// The page
// ============================================================================

pub(super) fn page() -> String {
    let mut s = page_open("api");

    s.push_str(&lead(
        "The load-bearing artifact. The CLI is a client; so is the MCP server; so could yours be. \
         One envelope in, one envelope out.",
    ));

    // Two-column: the sentence about the transport and the invocation of it,
    // read side by side. The CLI tab is the invocation alone. It used to carry
    // the `tasqx list` table screen under it — another client of `task.list`,
    // which says nothing about envelopes on stdin; the real `tasqx api`
    // response is the captured one the Response section just below shows.
    let cli_transport = term_block(&esc(
        "echo '{\"tasqx\":\"1\",\"method\":\"task.list\"}' | tasqx api",
    ));
    s.push_str(&ref_section(
        "api-transport",
        "The transport",
        &p(
            "<code>tasqx api</code> reads ONE request envelope on stdin and writes ONE response on \
             stdout. No framing, no handshake, no daemon needed. For many calls on one connection, \
             talk to the <a href=\"#daemon\">daemon</a> socket instead — same envelopes, newline-delimited.",
        ),
        &tabs(&[
            ("CLI", &cli_transport),
            (
                "JSON API",
                &snippet(
                    "echo '{\"tasqx\":\"1\",\"id\":\"e1\",\"method\":\"task.list\",\"params\":{\"filter\":\"+docs\"}}' | tasqx api",
                    "{\"id\":\"e1\",\"ok\":true,\"result\":{ … },\"tasqx\":\"1\"}",
                ),
            ),
        ]),
    ));

    s.push_str(&h3("Request"));
    s.push_str(&pre_plain(
        "{\n\
         \x20 \"tasqx\":  \"1\",            // API major. Required.\n\
         \x20 \"id\":     \"e1\",           // Optional. Echoed back if present.\n\
         \x20 \"method\": \"task.list\",    // Required.\n\
         \x20 \"params\": { }             // Optional; defaults to {}.\n\
         }",
    ));

    s.push_str(&h3("Response"));
    s.push_str(&p("Success carries <code>result</code>; failure carries <code>error</code>. <code>ok</code> tells you which without inspecting further."));
    // Captured, not typed: this block was a hand-pasted row from before the
    // list grew `next_offset`, `total` and `store_empty` (D152). The fixture is
    // the same one `#api-task-list` shows below.
    s.push_str(&term_screen(
        &format!("echo '{}' | tasqx api", example("task.list").request),
        "api-task-list",
    ));

    // The second two-column block: the codes and what they mean on the left,
    // the two captured envelopes on the right, where a reader comparing them
    // does not have to scroll between the table and the example.
    let error_prose = format!(
        "{}{}{}",
        p("An error is a value, not a crash. It carries a stable <code>code</code>, a human \
           <code>message</code>, and machine-readable <code>data</code> — and the CLI's exit codes \
           are these same codes."),
        table(
            &["Code", "Exit", "Means"],
            &[
            &[
                "<code>bad_request</code>",
                "2",
                "Malformed params, an unparseable date, contradictory input.",
            ],
            &[
                "<code>not_found</code>",
                "4",
                "No such task/project/reference.",
            ],
            &[
                "<code>conflict</code>",
                "5",
                "A lost <code>expected_rev</code> race, or a lifecycle rule.",
            ],
            &[
                "<code>unsupported_version</code>",
                "6",
                "The <code>tasqx</code> major you sent is not this build's.",
            ],
            &[
                "<code>internal</code>",
                "1",
                "A bug or an I/O failure. Should not happen.",
            ],
            ],
        ),
        p("Version mismatches are caught before dispatch, and the error's <code>data</code> names \
           the major that <em>is</em> supported."),
    );
    let error_envelopes = format!(
        "{}{}",
        snippet(
            "echo '{\"tasqx\":\"1\",\"id\":\"e2\",\"method\":\"task.get\",\"params\":{\"ref\":\"999\"}}' | tasqx api",
            "{\"error\":{\"code\":\"not_found\",\"data\":{\"short_id\":999},\"message\":\"no task with short_id 999\"},\"id\":\"e2\",\"ok\":false,\"tasqx\":\"1\"}",
        ),
        snippet(
            "echo '{\"tasqx\":\"2\",\"id\":\"v1\",\"method\":\"task.list\"}' | tasqx api",
            "{\"error\":{\"code\":\"unsupported_version\",\"data\":{\"supported\":\"1\"},\"message\":\"unsupported api major version: 2\"},\"id\":\"v1\",\"ok\":false,\"tasqx\":\"1\"}",
        ),
    );
    s.push_str(&ref_section(
        "api-errors",
        "Errors",
        &error_prose,
        &tabs(&[
            ("JSON API", &error_envelopes),
            (
                "CLI",
                &super::soon(
                    "Coming in the CLI reference: the exit code every verb returns, verb by verb.",
                ),
            ),
        ]),
    ));

    s.push_str(&h3("Feature detection"));
    s.push_str(&p(
        "Do not guess what a build supports — ask it. <code>core.capabilities</code> is the \
         handshake, and it also reports the current default project.",
    ));
    s.push_str(&snippet(
        "echo '{\"tasqx\":\"1\",\"id\":\"c1\",\"method\":\"core.capabilities\"}' | tasqx api",
        &capabilities_snippet(),
    ));

    s.push_str(&h3("Reading tasks without the API"));
    s.push_str(&p(
        "Any CLI command with <code>--json</code> prints the raw API result — the same bytes the \
         envelope's <code>result</code> would carry. That is usually the shortest path from a shell \
         script to structured data:",
    ));
    s.push_str(&term_screen("tasqx show 53 --json", "api-show-json"));

    s.push_str(&h3("Every method"));
    s.push_str(&p(&format!(
        "All {}, grouped by the object they act on. Every section below carries the method's \
         parameters, the shape of what it answers with, and a real request and response — and \
         every one of those columns is generated from the engine rather than described beside it.",
        METHODS.len()
    )));
    s.push_str(&index_table());

    for (object, title) in GROUPS {
        s.push_str(&h3(title));
        for method in methods_of(object) {
            s.push_str(&method_section(method));
        }
    }

    s.push_str(&page_close("api"));
    s
}

/// The objects the reference is grouped by, in reading order: `(prefix, heading)`.
///
/// `tokens.recompute` files under `token` with `token.add`/`token.remove` — the
/// two spellings are one subject, and a reader looking for "what tasqx knows
/// about token spend" should not have to know that one of the three is plural.
const GROUPS: [(&str, &str); 16] = [
    ("project", "Project methods"),
    ("task", "Task methods"),
    ("tag", "Tag methods"),
    ("annotation", "Annotation methods"),
    ("check", "Check methods"),
    ("dependency", "Dependency methods"),
    ("link", "Link methods"),
    ("graph", "Graph methods"),
    ("memory", "Memory methods"),
    ("token", "Token methods"),
    ("report", "Report methods"),
    ("store", "Store methods"),
    ("event", "Event methods"),
    ("reminder", "Reminder methods"),
    ("core", "Core methods"),
    ("otlp", "OTLP methods"),
];

/// The methods filed under one [`GROUPS`] prefix, in `PARAMS` order.
fn methods_of(object: &str) -> Vec<&'static str> {
    tasqx_core::PARAMS
        .iter()
        .map(|(m, _, _)| *m)
        .filter(|m| group_of(m) == object)
        .collect()
}

/// Which [`GROUPS`] prefix a method belongs to.
pub(super) fn group_of(method: &str) -> &str {
    let head = method.split('.').next().unwrap_or(method);
    if head == "tokens" {
        "token"
    } else {
        head
    }
}

/// A jump table: every method, linked to its section.
fn index_table() -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (object, title) in GROUPS {
        let links: Vec<String> = methods_of(object)
            .iter()
            .map(|m| format!("<a href=\"#api-{m}\"><code>{m}</code></a>"))
            .collect();
        rows.push(vec![esc(title), links.join(" · ")]);
    }
    table_owned(&["Object", "Methods"], &rows)
}

// ============================================================================
// One method
// ============================================================================

fn method_section(method: &str) -> String {
    let id = format!("api-{method}");
    let (_, returns) = method_row(method);

    let mut left = p(returns);
    // The objects this method acts on or answers with — the same function the
    // object pages list their operations from, so the links run both ways.
    let objects: Vec<String> = super::obj_ref::objects_of(method)
        .iter()
        .map(|o| format!("<a href=\"#{}\">{}</a>", o.id, esc(o.name)))
        .collect();
    if !objects.is_empty() {
        left.push_str(&p(&format!("Objects: {}.", objects.join(" · "))));
    }
    left.push_str("<h4>Parameters</h4>");
    let rows = params_of(method);
    if rows.is_empty() {
        left.push_str(&p(
            "None. Send <code>params</code> as <code>{}</code>, or leave it out.",
        ));
    } else {
        left.push_str(&param_table(&id, &rows));
    }
    left.push_str("<h4>Response</h4>");
    left.push_str(&response_tables(method));

    ref_section(
        &id,
        method,
        &left,
        &tabs(&[
            ("JSON API", &json_tab(method)),
            ("MCP", &mcp_tab(method)),
            ("CLI", &cli_tab(method)),
        ]),
    )
}

/// The [`super::METHODS`] row for a method: `(params cell, returns cell)`.
///
/// Panics for a method with no row, which cannot happen past
/// `documented_methods_match_core_capabilities` — that guard holds the table
/// equal to `core.capabilities`, so a miss here is that test being deleted.
fn method_row(method: &str) -> (&'static str, &'static str) {
    METHODS
        .iter()
        .find(|(m, _, _)| *m == method)
        .map(|(_, params, returns)| (*params, *returns))
        .unwrap_or_else(|| panic!("no METHODS row for `{method}`"))
}

/// The parameters a method takes, in `PARAMS` order, each with its type,
/// required-ness, default and description.
fn params_of(method: &str) -> Vec<Param<'static>> {
    let accepted = accepted_params(method);
    let required = required_params(method);
    accepted
        .iter()
        .map(|name| {
            let (ty, default, desc) = param_doc(method, name);
            Param {
                name,
                ty,
                required: required.contains(name),
                default,
                html_desc: desc,
            }
        })
        .collect()
}

/// The accepted key set for a method, from the engine's own table.
fn accepted_params(method: &str) -> &'static [&'static str] {
    tasqx_core::PARAMS
        .iter()
        .find(|(m, _, _)| *m == method)
        .map(|(_, accepted, _)| *accepted)
        .unwrap_or_else(|| panic!("`{method}` is not in tasqx_core::PARAMS"))
}

/// The ones with no trailing `?` in [`super::METHODS`]' Params column — the
/// spelling the engine's "missing required field" complaint is checked against.
fn required_params(method: &str) -> Vec<&'static str> {
    let (cell, _) = method_row(method);
    cell.split("<code>")
        .skip(1)
        .filter_map(|chunk| chunk.split("</code>").next())
        .filter(|name| !name.ends_with('?'))
        .collect()
}

/// A parameter's type, default and description.
///
/// [`PARAM_DOCS`] first, because it is the override as well as the fallback,
/// then the MCP tool's own `inputSchema` — the description an agent is already
/// choosing this argument from, so the page and the tool cannot teach two
/// different things about one key. A parameter with neither is a panic at
/// generation time and a red test before that: an undescribed parameter is the
/// hole this page exists to close.
pub(super) fn param_doc(
    method: &str,
    name: &str,
) -> (&'static str, Option<&'static str>, &'static str) {
    if let Some((_, _, ty, default, desc)) = PARAM_DOCS
        .iter()
        .find(|(m, param, ..)| *m == method && *param == name)
    {
        let default = if default.is_empty() {
            None
        } else {
            Some(*default)
        };
        return (ty, default, desc);
    }
    if let Some(prop) = described_property(method, name) {
        return (
            schema_badge(prop),
            None,
            describe_cached(prop["description"].as_str().unwrap_or_default()),
        );
    }
    panic!(
        "`{method}.{name}` has no description: it is in tasqx_core::PARAMS, no MCP schema \
         describes it, and PARAM_DOCS in docs/api_ref.rs does not either"
    );
}

/// The `inputSchema` property for one param of the tool that maps to `method`,
/// when that property carries a description.
///
/// A property without one is not a source: several exist (`tags`,
/// `include_archived`, a project's `name`), each obvious enough to an agent
/// reading the tool's own prose and to nobody arriving at a reference page.
/// Those fall through to [`PARAM_DOCS`], which is why the fallback table's
/// guard asks whether a schema DESCRIBES a key rather than whether it lists
/// one.
pub(super) fn described_property(method: &str, name: &str) -> Option<&'static Value> {
    let tool = tool_for(method)?;
    let prop = tool.schema.get("properties")?.get(name)?;
    let desc = prop.get("description")?.as_str()?;
    (!desc.trim().is_empty()).then_some(prop)
}

/// The JSON-Schema `type` of a property, as a badge: `string`, `integer`,
/// `array of string`, or `any` when the schema states none.
pub(super) fn schema_badge(prop: &Value) -> &'static str {
    // `ref` is `["integer", "string"]` — a short id or a uuid — and a schema
    // that states two types is answering the question the badge asks, so it is
    // spelled rather than flattened to "any".
    if let Some(list) = prop["type"].as_array() {
        let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
        return match names.as_slice() {
            ["integer", "string"] | ["string", "integer"] => "integer or string",
            _ => "any",
        };
    }
    let ty = prop["type"].as_str().unwrap_or("any");
    let item = prop["items"]["type"].as_str();
    match (ty, item) {
        ("array", Some("string")) => "array of string",
        ("array", Some("object")) => "array of object",
        ("array", _) => "array",
        ("string", _) => "string",
        ("integer", _) => "integer",
        ("number", _) => "number",
        ("boolean", _) => "boolean",
        ("object", _) => "object",
        _ => "any",
    }
}

/// The tool that routes to `method`, or `None` for a method that has none.
pub(super) fn tool_for(method: &str) -> Option<&'static tasqx_core::mcp::ToolDoc> {
    TOOLS.iter().find(|t| t.method == method)
}

/// Built once: `tool_docs()` allocates a `Vec`, and every parameter row of
/// every section would otherwise rebuild it.
static TOOLS: std::sync::LazyLock<Vec<tasqx_core::mcp::ToolDoc>> =
    std::sync::LazyLock::new(tasqx_core::mcp::tool_docs);

// ============================================================================
// The response shape
// ============================================================================

/// One table per object the response carries — `result`, then each nested row
/// shape under the path that reaches it.
fn response_tables(method: &str) -> String {
    let shape = tasqx_core::docs::result_shape(method);
    assert!(
        !shape.is_empty(),
        "`{method}` has no documented response shape; add it to tasqx_core::docs::result_shape"
    );
    let mut out = String::new();
    let mut path = "";
    let mut rows: Vec<(String, String)> = Vec::new();
    for (p, group) in shape {
        // Consecutive groups under one path are ONE object, composed — the way
        // the freeze composes it — so they render as one table.
        if *p != path {
            if !rows.is_empty() {
                out.push_str(&shape_table(path, &rows));
                rows.clear();
            }
            path = p;
        }
        for f in *group {
            rows.push((
                format!(
                    "<code class=\"fname\">{}</code><span class=\"badge\">{}</span>{}",
                    esc(f.key),
                    esc(f.ty),
                    presence(f.null_ok, f.optional),
                ),
                describe(f.desc),
            ));
        }
    }
    if !rows.is_empty() {
        out.push_str(&shape_table(path, &rows));
    }
    out
}

fn shape_table(path: &str, rows: &[(String, String)]) -> String {
    format!(
        "<div class=\"shapepath\"><code>{}</code></div>{}",
        esc(path),
        field_list(rows),
    )
}

/// The three answers to "can I read this key without checking first?", spelled
/// as the contract spells them: always there, always there and sometimes
/// `null`, or sometimes not there at all.
pub(super) fn presence(null_ok: bool, optional: bool) -> &'static str {
    match (null_ok, optional) {
        (_, true) => "<span class=\"pill opt\">optional</span>",
        (true, _) => "<span class=\"pill opt\">nullable</span>",
        _ => "<span class=\"pill req\">always</span>",
    }
}

// ============================================================================
// The three tabs
// ============================================================================

/// The request envelope and the response it really produced.
fn json_tab(method: &str) -> String {
    let ex = example(method);
    let response = response_json(ex);
    let mut out = format!(
        "<div class=\"snip\"><div class=\"snip-h\"><span class=\"dollar\">$</span>\
           <button class=\"copy\" type=\"button\">Copy</button></div>\
         <pre class=\"cmd\"><code>{}</code></pre>\
         <pre class=\"out json\"><code>{}</code></pre></div>",
        esc(&format!("echo '{}' | tasqx api", ex.request)),
        json_html(&response, 0),
    );
    if !ex.why.is_empty() {
        out.push_str(&note(&format!(
            "<strong>Illustrative.</strong> {}",
            describe(ex.why)
        )));
    }
    out
}

/// The tool that reaches this method, or the reason none does.
fn mcp_tab(method: &str) -> String {
    let Some(tool) = tool_for(method) else {
        let why = tasqx_core::mcp::unexposed_methods()
            .iter()
            .find(|(m, _)| *m == method)
            .map(|(_, why)| *why)
            .unwrap_or_else(|| {
                panic!(
                    "`{method}` has no MCP tool and no reason in mcp::unexposed_methods — one of \
                     the two tables is wrong"
                )
            });
        return format!(
            "{}{}",
            p(
                "<strong>No MCP tool.</strong> An agent reaches this method only through a human \
               running <code>tasqx api</code>."
            ),
            note(&describe(why)),
        );
    };
    let args = example_params(method);
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool.name, "arguments": args },
    });
    format!(
        "{}{}{}",
        p(&format!(
            "<a href=\"#mcp-{name}\"><code>{name}</code></a> — {scope} scope.",
            name = tool.name,
            scope = if tool.write { "write" } else { "read" },
        )),
        term_block(&esc(&serde_json::to_string(&call).unwrap_or_default())),
        p(
            "The tool hands back this method's result in a text block — or, where its own \
           description says so, less of it by default: a rendered view, a compact row, no \
           <code>rank</code>. What it sends and how to ask for the rest is \
           <a href=\"#mcp\">documented with the tool</a>."
        ),
    )
}

/// The verbs that reach this method, each linked to its own section in the CLI
/// reference (#646) and described by the summary `tasqx <verb> -h` prints.
fn cli_tab(method: &str) -> String {
    let verbs = verbs_for(method);
    if verbs.is_empty() {
        return p(
            "No verb reaches this method on its own — <code>tasqx api</code> is the way, and the \
             envelope beside this is the whole call.",
        );
    }
    let mut out = String::new();
    for verb in verbs {
        out.push_str(&term_block(&esc(&format!("tasqx {verb}"))));
        out.push_str(&p(&format!(
            "<a href=\"#cli-{verb}\"><code>{verb}</code></a> — {}",
            describe(verb_summary(verb)),
        )));
    }
    out
}

/// Every verb whose [`super::VERBS`] row names this method.
///
/// The Method column is prose-ish by design — a verb that calls three methods
/// says so — so it is read the way it is written: split on ` + `, and a part
/// spelled as a suffix list (`memory.search + get/add/remove/list/update`)
/// expands against the object of the part before it. A cell that frames its own
/// transport (`—`, `(any)`) names no method and is skipped, exactly as
/// `verb_table_only_names_real_methods` skips it.
pub(super) fn verbs_for(method: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for (verb, _, cell) in VERBS {
        if cell.starts_with('—') || cell.starts_with('(') {
            continue;
        }
        let mut object = "";
        for part in cell.split(" + ") {
            if let Some((head, _)) = part.split_once('.') {
                object = head;
                if part == method {
                    out.push(verb);
                }
            } else if part.contains('/')
                && part
                    .split('/')
                    .any(|suffix| format!("{object}.{suffix}") == method)
            {
                out.push(verb);
            }
        }
    }
    out
}

// ============================================================================
// Examples
// ============================================================================

/// One worked call per method.
pub(super) struct Example {
    method: &'static str,
    /// The request envelope, verbatim. For a captured row this is byte-for-byte
    /// the manifest's `stdin` column, and a test holds the two equal.
    request: &'static str,
    /// The `docs-fixtures` row that captured the answer, or `""`.
    fixture: &'static str,
    /// The hand-written answer, for a method whose real one cannot be
    /// captured. Empty whenever `fixture` is not.
    response: &'static str,
    /// Why this method's answer cannot be a fixture. Empty for a captured one;
    /// rendered on the page beside the example, because a reader must never
    /// mistake an invented response for a reproducible one.
    pub(super) why: &'static str,
}

/// The `params` object of a method's example request, for the MCP page: the two
/// references show ONE call per method rather than two plausible ones.
pub(super) fn example_params(method: &str) -> Value {
    serde_json::from_str::<Value>(example(method).request)
        .ok()
        .and_then(|v| v.get("params").cloned())
        .unwrap_or_else(|| serde_json::json!({}))
}

pub(super) fn example(method: &str) -> &'static Example {
    EXAMPLES
        .iter()
        .find(|e| e.method == method)
        .unwrap_or_else(|| panic!("no example for `{method}` in docs/api_ref.rs"))
}

/// The response an example shows, parsed.
///
/// One example has neither a fixture nor a literal: `core.capabilities` answers
/// with this machine's own store path, so the page renders the handshake this
/// build really emits with a plausible store substituted (D74), derived here
/// exactly as the section at the top of the page derives it.
pub(super) fn response_json(ex: &Example) -> Value {
    let text = if ex.fixture.is_empty() && ex.response.is_empty() {
        capabilities_snippet()
    } else if ex.fixture.is_empty() {
        ex.response.to_string()
    } else {
        crate::fixtures::screen(ex.fixture)
            .unwrap_or_else(|| {
                panic!(
                    "`{}` names fixture `{}`, which is not captured; add a row to \
                     crates/tasqx-cli/docs-fixtures/manifest.tsv and re-run \
                     scripts/docs-capture.sh",
                    ex.method, ex.fixture
                )
            })
            .to_string()
    };
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("`{}`'s example response is not JSON: {e}", ex.method))
}

// ============================================================================
// JSON, pretty-printed and coloured
// ============================================================================

/// The same value as HTML: two-space indent, one span class per token kind.
///
/// Written out rather than pulled in, because a highlighter is a dependency
/// (and usually a download) for four token kinds on a page that refuses both.
/// Every string goes through [`esc`]; nothing here can emit markup a value
/// carried.
/// A string key or value as a JSON token, quotes included, made safe for HTML.
///
/// serde_json does the JSON escaping, so every control character becomes an
/// escape sequence before [`esc`] runs — `esc` drops raw control bytes, and a
/// hand-rolled escaper that missed one showed a value the API never sent.
fn json_string(s: &str) -> String {
    esc(&serde_json::to_string(s).expect("a str always serializes"))
}

pub(super) fn json_html(v: &Value, depth: usize) -> String {
    let pad = "  ".repeat(depth + 1);
    let closing = "  ".repeat(depth);
    match v {
        Value::Null => "<span class=\"j-b\">null</span>".to_string(),
        Value::Bool(b) => format!("<span class=\"j-b\">{b}</span>"),
        Value::Number(n) => format!("<span class=\"j-n\">{n}</span>"),
        Value::String(s) => format!("<span class=\"j-s\">{}</span>", json_string(s)),
        Value::Array(items) if items.is_empty() => "[]".to_string(),
        Value::Array(items) => {
            let body: Vec<String> = items
                .iter()
                .map(|i| format!("{pad}{}", json_html(i, depth + 1)))
                .collect();
            format!("[\n{}\n{closing}]", body.join(",\n"))
        }
        Value::Object(map) if map.is_empty() => "{}".to_string(),
        Value::Object(map) => {
            let body: Vec<String> = map
                .iter()
                .map(|(k, val)| {
                    format!(
                        "{pad}<span class=\"j-k\">{}</span>: {}",
                        json_string(k),
                        json_html(val, depth + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{closing}}}", body.join(",\n"))
        }
    }
}

/// Inline `code` spans out of a plain sentence, escaped.
///
/// Shared with the MCP reference, which renders the same kind of string: a
/// sentence written for another reader, carrying identifiers in backticks.
///
/// The descriptions this page renders are written for two other readers — an
/// agent (the MCP schemas) and a Rust reader (`tasqx_core::docs`) — and both
/// spell an identifier in backticks. Turning them into `<code>` here is the one
/// piece of markup this page adds to a runtime string, and it runs AFTER
/// escaping, so nothing a description carries can become a tag.
pub(super) fn describe(text: &str) -> String {
    let escaped = esc(text);
    let mut out = String::with_capacity(escaped.len());
    let mut open = false;
    for ch in escaped.chars() {
        if ch == '`' {
            out.push_str(if open { "</code>" } else { "<code>" });
            open = !open;
        } else {
            out.push(ch);
        }
    }
    if open {
        out.push_str("</code>");
    }
    out
}

/// [`describe`] for a `&'static str` that must stay `&'static` — the parameter table
/// takes borrowed descriptions, and the MCP schemas are built once and live for
/// the process, so the rendered form can be leaked into the same lifetime
/// rather than threading owned strings through every row.
///
/// The leak is bounded by the schemas: one allocation per parameter per
/// generated page, in a process that generates one page and exits.
fn describe_cached(text: &str) -> &'static str {
    Box::leak(describe(text).into_boxed_str())
}

// ============================================================================
// The two hand-written tables
// ============================================================================

/// Parameters no MCP schema describes, because no tool reaches their method.
///
/// Two kinds of gap, and nothing else. Some methods are deliberately off
/// the tool surface (`mcp::unexposed_methods`), so their parameters have no
/// agent-facing description to render; and a few properties that ARE in a
/// schema carry no `description` of their own. Every other parameter comes
/// from the schema an agent already reads, and a row added here for one of
/// those would be a second description of a key that has one — which
/// `the_fallback_table_covers_only_what_no_schema_does` refuses.
///
/// `(method, param, type, default, description)`. An empty default means there
/// is none to state.
const PARAM_DOCS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "project.create",
        "name",
        "string",
        "",
        "The project's name. Dotted names (`work.tasqx`) are free-form: nothing nests, and a filter on `work` does not match `work.tasqx`.",
    ),
    (
        "project.create",
        "description",
        "string",
        "",
        "What the project is for. Shown by `project.list` and nowhere load-bearing.",
    ),
    (
        "project.list",
        "include_archived",
        "boolean",
        "false",
        "Include archived projects in the list. Omitted, an archive is out of sight.",
    ),
    (
        "task.add",
        "project",
        "string",
        "",
        "The project to file it under. Omitted, the task lands in the store's default project — and the response says which, because that is the only place a caller learns it.",
    ),
    (
        "task.add",
        "tags",
        "array of string",
        "",
        "Tags to put on the task. Lowercased and deduplicated as they are stored.",
    ),
    (
        "tag.add",
        "tags",
        "array of string",
        "",
        "The tags to add. Already-present tags are a no-op rather than an error.",
    ),
    (
        "tag.remove",
        "tags",
        "array of string",
        "",
        "The tags to take off. All or nothing: a tag the task does not have is `not_found` and removes none of them, so a typo cannot answer ok.",
    ),
    (
        "memory.add",
        "title",
        "string",
        "",
        "The doc's title. Indexed with the body, so the words a future search will use belong in it.",
    ),
    (
        "project.use",
        "name",
        "string",
        "",
        "The project that becomes the default — where a `task.add` with no `project` lands.",
    ),
    (
        "project.archive",
        "name",
        "string",
        "",
        "The project to archive. Archiving hides it from the list; it removes nothing.",
    ),
    (
        "token.add",
        "source",
        "string",
        "",
        "How the figure was obtained: `self-report`, `log-parse` or `otel`. It is what `confidence` is graded against.",
    ),
    (
        "token.add",
        "model",
        "string",
        "",
        "The model that did the work. Recorded, never interpreted.",
    ),
    (
        "token.add",
        "input_tokens",
        "integer",
        "0",
        "Fresh input tokens.",
    ),
    (
        "token.add",
        "output_tokens",
        "integer",
        "0",
        "Output tokens.",
    ),
    (
        "token.add",
        "cache_read_tokens",
        "integer",
        "0",
        "Tokens read from cache. Kept in its own bucket and never blended with the others (D48).",
    ),
    (
        "token.add",
        "cache_creation_tokens",
        "integer",
        "0",
        "Tokens spent writing the cache.",
    ),
    (
        "token.add",
        "confidence",
        "string",
        "",
        "How checkable the figure is: `medium` or `low` for a self-report — `high` is refused, because confidence describes verifiability and not preference (D50).",
    ),
    (
        "token.remove",
        "measurement_id",
        "string",
        "",
        "The measurement to delete, by the id `token.add` handed back. There is no `ref`: a measurement id is globally unique.",
    ),
    (
        "tokens.recompute",
        "dry_run",
        "boolean",
        "true",
        "Report the per-task delta and write nothing. Send `false` to apply it — this is the one method that deletes measurement rows.",
    ),
    (
        "store.export",
        "filter",
        "string",
        "",
        "A filter-DSL query narrowing which tasks are exported. Omitted, the whole store comes back.",
    ),
    (
        "store.import",
        "tasks",
        "array of object",
        "",
        "The task documents to import, in the shape `store.export` emits. Required, so a misspelled key at the top level is still refused by absence.",
    ),
    (
        "store.import",
        "projects",
        "array of object",
        "",
        "The project records. A task naming a project this list does not carry mints one, reported in `projects_created`.",
    ),
    (
        "store.import",
        "default_project",
        "string",
        "",
        "The default project to set after the import.",
    ),
    (
        "store.import",
        "docs",
        "array of object",
        "",
        "The knowledge docs to import.",
    ),
    (
        "store.import",
        "events",
        "array of object",
        "",
        "The audit rows to import, so history survives a round trip.",
    ),
    (
        "event.list",
        "limit",
        "integer",
        "50",
        "How many events to return, newest first.",
    ),
    (
        "event.list",
        "ref",
        "integer or string",
        "",
        "Only events about this task.",
    ),
    (
        "event.list",
        "entity",
        "string",
        "",
        "Only events about this kind of thing: `task`, `project`, `doc` or `link`. An unknown name is refused, not read as a filter matching nothing.",
    ),
    (
        "event.list",
        "from",
        "string",
        "",
        "A lower bound in the usual date grammar (`yesterday`, `2026-09-01`). It promises no events older than roughly that instant, not an exact cut.",
    ),
    (
        "link.add",
        "from",
        "integer or string",
        "",
        "The node the link starts at. A task short id (`42`), a bare uuid (tried against tasks, memory docs, annotations and projects in that order), or `task:`/`memory:`/`annotation:`/`project:` plus an id — a project also by name (`project:work`). An unknown prefix is refused; a well-formed reference naming nothing is `not_found`.",
    ),
    (
        "link.add",
        "to",
        "integer or string",
        "",
        "The node the link points at, in the same grammar as `from`. Linking a node to itself is refused; a cycle between two nodes is not — nothing schedules work off a link.",
    ),
    (
        "link.add",
        "relation",
        "string",
        "",
        "What the link asserts: `references`, `supersedes`, `implements_decision`, `derived_from` or `contradicts`. Anything else is refused with the list, not stored.",
    ),
    (
        "link.add",
        "metadata",
        "object",
        "",
        "Your own object, stored verbatim and never interpreted. A repeat of an existing link never overwrites it.",
    ),
    (
        "link.add",
        "expected_rev",
        "integer",
        "",
        "Optimistic-concurrency guard on the `from` endpoint, mismatched a `conflict` naming both revs. Ignored when that endpoint is an annotation or a project, which carry no rev.",
    ),
    (
        "link.remove",
        "id",
        "string",
        "",
        "The link's own uuid, as `link.add` and `link.list` report it.",
    ),
    (
        "link.list",
        "ref",
        "integer or string",
        "",
        "Only links with this node at EITHER end, in `link.add`'s reference grammar. Omitted, the whole store's links come back.",
    ),
    (
        "link.list",
        "relation",
        "string",
        "",
        "Only links asserting this relation. An unknown one is refused, not read as a filter matching nothing.",
    ),
    (
        "link.list",
        "limit",
        "integer",
        "100",
        "How many links to return, newest first. Clamped to 1,000.",
    ),
    (
        "link.list",
        "offset",
        "integer",
        "0",
        "Where the page starts. `next_offset` from the previous page keeps the walk going.",
    ),
    (
        "graph.query",
        "root",
        "integer or string",
        "",
        "The node the projection is anchored at, in `link.add`'s reference grammar: a task short id (`42`), a bare uuid, or `task:`/`memory:`/`annotation:`/`project:` plus an id. It is returned resolved as `<type>:<uuid>`, and it is kept whatever the filters below say — a projection with no anchor in it has nothing to project from.",
    ),
    (
        "graph.query",
        "depth",
        "integer",
        "2",
        "How many hops to walk, 0 to 4. Out of range is refused, never clamped: a caller who asked for 5 cannot tell a clamped answer from the graph really ending there.",
    ),
    (
        "graph.query",
        "node_types",
        "array of string",
        "",
        "Keep only these kinds of node: `task`, `memory`, `annotation`, `project`. A dropped node is not expanded either, so this bounds the cost as well as the answer. An empty array filters nothing.",
    ),
    (
        "graph.query",
        "relation_types",
        "array of string",
        "",
        "Only cross these relations: `depends_on`, `has_annotation`, `belongs_to_project`, the five link relations, and `search_match`/`shared_tag` for the inferred ones. An unknown name is refused with the list. An empty array crosses every relation.",
    ),
    (
        "graph.query",
        "project",
        "string",
        "",
        "Keep only the nodes filed under this project — tasks and memory documents. A project node and the notes on a kept task pass through, because neither is filed anywhere of its own.",
    ),
    (
        "graph.query",
        "status",
        "string",
        "",
        "Keep only tasks in this status, read the way every other surface reads it (a future `wait` shows `backlog`). Nodes that are not tasks are unaffected.",
    ),
    (
        "graph.query",
        "tags",
        "array of string",
        "",
        "Keep only tasks carrying at least one of these tags. Nodes that are not tasks are unaffected.",
    ),
    (
        "graph.query",
        "modified_after",
        "string",
        "",
        "An RFC 3339 lower bound on a task's or document's `modified` and a note's `created`. A project carries no such date and is never excluded by the window.",
    ),
    (
        "graph.query",
        "modified_before",
        "string",
        "",
        "The upper bound of the same window, also RFC 3339.",
    ),
    (
        "graph.query",
        "include_inferred",
        "boolean",
        "false",
        "Add the computed edges: up to ten `search_match` hits for the root's own title, and a `shared_tag` edge between every pair of returned tasks sharing a tag. Each carries a `confidence` and the `source` that produced it, and none of them is ever stored.",
    ),
    (
        "graph.query",
        "max_nodes",
        "integer",
        "250",
        "How many nodes the answer may carry, 1 to 1,000. Applied to the deterministic order, so the cut is the same every time; `omitted_nodes` says how many went.",
    ),
    (
        "graph.query",
        "max_edges",
        "integer",
        "750",
        "How many edges the answer may carry, 1 to 5,000. Applied after the edges whose endpoints were cut have already gone; `omitted_edges` says how many were left out.",
    ),
    (
        "memory.import",
        "docs",
        "array of object",
        "",
        "The documents to store, each `{title, body, source?}`. One transaction: same `source` replaces in place, keeping the doc's id and creation date (D143); a batch naming one `source` twice is refused whole (D174).",
    ),
    (
        "memory.import",
        "project",
        "string",
        "",
        "Scopes every doc in the batch (#657) — one value for the whole import, not per-doc. Omitted, a new doc lands global and an existing one (a re-import) keeps whatever scope it already had, like `standing`; named, it MOVES an existing doc's scope on re-import.",
    ),
    (
        "reminder.fire",
        "ref",
        "integer or string",
        "",
        "The task whose reminder is being fired. Daemon-internal — the `reminded` event is a dedupe key, not a notification anyone reads here.",
    ),
    (
        "reminder.fire",
        "at",
        "string",
        "",
        "The instant the reminder is recorded against. Firing the same instant twice is a no-op.",
    ),
];

/// One request/response pair per method.
///
/// Most of them name a `docs-fixtures` row: the request in `request` is
/// byte-for-byte the manifest's `stdin` column (a test holds the two equal),
/// and the response is what the binary printed for it on the pinned day.
///
/// The rest cannot be captured, each for a reason the manifest's own header
/// argues: a freshly minted v7 id whose low bits are random (`task.add`,
/// `annotation.add`, `check.add`, `memory.add`, `memory.import`, `token.add`,
/// `project.create`), an FTS bm25 `rank` whose last digits are the platform's
/// `log()` (`memory.search`, `task.brief`), a response that is the whole store
/// (`store.export`), this machine's own paths (`core.capabilities`), or an
/// answer that depends on store state no fixture can pin (`event.revert`).
/// Those carry `why`, which the page prints beside the example.
const EXAMPLES: &[Example] = &[
    Example {
        method: "project.create",
        request: r#"{"tasqx":"1","id":"pc1","method":"project.create","params":{"name":"research","description":"Prototypes and spikes"}}"#,
        fixture: "",
        response: r#"{"id":"pc1","ok":true,"result":{"current_default":"website","default":false,"id":"019f7c0a-3d51-7c42-9a08-1f0c4e5b62d7","name":"research"},"tasqx":"1"}"#,
        why: "a create mints a fresh v7 id, whose low bits are random — the pinned clock fixes the timestamp half and nothing fixes the rest, so a captured answer would differ on every run.",
    },
    Example {
        method: "project.list",
        request: r#"{"tasqx":"1","id":"p1","method":"project.list"}"#,
        fixture: "api-project-list",
        response: "",
        why: "",
    },
    Example {
        method: "project.use",
        request: r#"{"tasqx":"1","id":"pu1","method":"project.use","params":{"name":"api"}}"#,
        fixture: "api-project-use",
        response: "",
        why: "",
    },
    Example {
        method: "project.archive",
        request: r#"{"tasqx":"1","id":"pa1","method":"project.archive","params":{"name":"mobile"}}"#,
        fixture: "api-project-archive",
        response: "",
        why: "",
    },
    Example {
        method: "task.add",
        request: r#"{"tasqx":"1","id":"a1","method":"task.add","params":{"title":"Draft the Q4 roadmap","project":"website","priority":"H","due":"friday","tags":["planning"]}}"#,
        fixture: "",
        response: r#"{"id":"a1","ok":true,"result":{"due":"2026-09-18T00:00:00Z","id":"019f7c0a-3d51-7c42-9a08-1f0c4e5b62d7","project":"website","recurrence":null,"scheduled":null,"short_id":62,"status":"pending","tags":["planning"],"title":"Draft the Q4 roadmap","urgency":16.6},"tasqx":"1"}"#,
        why: "a new task's v7 id is half clock and half entropy, so no pin can reproduce it. The CLI's own `add` echo, which prints the short id, is captured instead.",
    },
    Example {
        method: "task.list",
        request: r#"{"tasqx":"1","id":"e1","method":"task.list","params":{"filter":"+docs","sort":["-urgency"]}}"#,
        fixture: "api-task-list",
        response: "",
        why: "",
    },
    Example {
        method: "task.get",
        request: r#"{"tasqx":"1","id":"g1","method":"task.get","params":{"ref":"51"}}"#,
        fixture: "api-task-get",
        response: "",
        why: "",
    },
    Example {
        method: "task.brief",
        request: r#"{"tasqx":"1","id":"b1","method":"task.brief","params":{"ref":"51","memory_limit":3}}"#,
        fixture: "",
        response: r#"{"id":"b1","ok":true,"result":{"memory":{"annotations_total":3,"count":1,"docs_total":1,"has_more":true,"hits":[{"id":"eb864f1e-e68a-4d96-af89-597bd0d2d52e","kind":"doc","project":"api","rank":-0.9096037228757,"snippet":"Cut the release branch on Monday, tag after the canary has run…","source":"docs/release.md","standing":false,"title":"release-process"}],"matched":"guide OR migration OR sdk OR docs OR api","project":"api","reserved_docs":2,"total":4},"neighbourhood":{"blocks":[],"depends_on":[{"annotation":null,"short_id":50,"status":"pending","title":"Rate-limit the /search endpoint"}]},"task":{"…":"task.get's own result, verbatim"}},"tasqx":"1"}"#,
        why: "every hit carries an FTS bm25 `rank`, computed through the platform's `log()` — captured on macOS the last digits differ on Linux, and the drift job goes red for everyone but whoever captured last. The `tasqx brief` SCREEN is captured instead; it prints snippets and never the number.",
    },
    Example {
        method: "task.start",
        request: r#"{"tasqx":"1","id":"s1","method":"task.start","params":{"ref":"50"}}"#,
        fixture: "api-task-start",
        response: "",
        why: "",
    },
    Example {
        method: "task.stop",
        request: r#"{"tasqx":"1","id":"st1","method":"task.stop","params":{"ref":"48"}}"#,
        fixture: "api-task-stop",
        response: "",
        why: "",
    },
    Example {
        method: "task.done",
        request: r#"{"tasqx":"1","id":"d1","method":"task.done","params":{"ref":"47"}}"#,
        fixture: "api-task-done",
        response: "",
        why: "",
    },
    Example {
        method: "task.modify",
        request: r#"{"tasqx":"1","id":"m1","method":"task.modify","params":{"ref":"52","set":{"priority":"H","due":"friday"}}}"#,
        fixture: "api-task-modify",
        response: "",
        why: "",
    },
    Example {
        method: "task.cancel",
        request: r#"{"tasqx":"1","id":"c1","method":"task.cancel","params":{"ref":"60"}}"#,
        fixture: "api-task-cancel",
        response: "",
        why: "",
    },
    Example {
        method: "task.reopen",
        request: r#"{"tasqx":"1","id":"ro1","method":"task.reopen","params":{"ref":"12"}}"#,
        fixture: "api-task-reopen",
        response: "",
        why: "",
    },
    Example {
        method: "tag.add",
        request: r#"{"tasqx":"1","id":"t1","method":"tag.add","params":{"ref":"53","tags":["q4"]}}"#,
        fixture: "api-tag-add",
        response: "",
        why: "",
    },
    Example {
        method: "tag.remove",
        request: r#"{"tasqx":"1","id":"t2","method":"tag.remove","params":{"ref":"53","tags":["ops"]}}"#,
        fixture: "api-tag-remove",
        response: "",
        why: "",
    },
    Example {
        method: "annotation.add",
        request: r#"{"tasqx":"1","id":"an1","method":"annotation.add","params":{"ref":"51","body":"Ruling: the guide ships WITH the 3.0 release."}}"#,
        fixture: "",
        response: r#"{"id":"an1","ok":true,"result":{"annotation":{"body":"Ruling: the guide ships WITH the 3.0 release.","created":"2026-09-16T09:00:00Z","id":"019f7c0a-3d51-7401-a1e6-c040f1ddbe51"},"short_id":51},"tasqx":"1"}"#,
        why: "the note's id is minted fresh, v7, half of it random. The `tasqx annotate` screen is captured instead.",
    },
    Example {
        method: "annotation.remove",
        request: r#"{"tasqx":"1","id":"ar1","method":"annotation.remove","params":{"ref":"51","annotation_id":"17dd6621-7db4-43b5-9f36-ddf89018081e"}}"#,
        fixture: "api-annotation-remove",
        response: "",
        why: "",
    },
    Example {
        method: "annotation.update",
        request: r#"{"tasqx":"1","id":"au1","method":"annotation.update","params":{"ref":"51","annotation_id":"019f7c0a-3d51-7401-a1e6-c040f1ddbe51","body":"Ruling: the guide ships AFTER the 3.0 release."}}"#,
        fixture: "",
        response: r#"{"id":"au1","ok":true,"result":{"_rev":7,"annotation":{"body":"Ruling: the guide ships AFTER the 3.0 release.","created":"2026-09-16T09:00:00Z","id":"019f7c0a-3d51-7401-a1e6-c040f1ddbe51"},"short_id":51},"tasqx":"1"}"#,
        why: "the note it edits has an id minted fresh, v7, half of it random, so no fixed request can name it.",
    },
    Example {
        method: "check.add",
        request: r#"{"tasqx":"1","id":"ca1","method":"check.add","params":{"ref":"51","body":"Every renamed symbol has a row in the table"}}"#,
        fixture: "",
        response: r#"{"id":"ca1","ok":true,"result":{"check":{"body":"Every renamed symbol has a row in the table","created":"2026-09-16T09:00:00Z","evidence":null,"id":"019f7c0a-3d51-7a90-b0c1-52f0a7d3c611","modified":"2026-09-16T09:00:00Z","position":4,"state":"open"},"short_id":51},"tasqx":"1"}"#,
        why: "the criterion's id is minted fresh, v7, half of it random.",
    },
    Example {
        method: "check.set",
        request: r#"{"tasqx":"1","id":"cs1","method":"check.set","params":{"ref":"51","check_id":"7e37a508-7921-4cb2-bf0c-0a2944eb31e4","state":"passed","evidence":"docs/guides/sdk-3.0.md#rate-limits"}}"#,
        fixture: "api-check-set",
        response: "",
        why: "",
    },
    Example {
        method: "check.remove",
        request: r#"{"tasqx":"1","id":"cr1","method":"check.remove","params":{"ref":"51","check_id":"e6a1096b-6f05-4e95-96f5-52452080f2ac"}}"#,
        fixture: "api-check-remove",
        response: "",
        why: "",
    },
    Example {
        method: "token.add",
        request: r#"{"tasqx":"1","id":"tk1","method":"token.add","params":{"ref":"51","tool":"claude-code","source":"self-report","model":"opus","input_tokens":18400,"output_tokens":2600,"cache_read_tokens":91000,"cache_creation_tokens":12000,"confidence":"medium"}}"#,
        fixture: "",
        response: r#"{"id":"tk1","ok":true,"result":{"measurement":{"cache_creation_tokens":12000,"cache_read_tokens":91000,"confidence":"medium","created":"2026-09-16T09:00:00Z","id":"019f7c0a-3d51-7d22-b73c-9353de0a3360","input_tokens":18400,"model":"opus","output_tokens":2600,"source":"self-report","tool":"claude-code","total_tokens":0},"short_id":51},"tasqx":"1"}"#,
        why: "the measurement's id is minted fresh, v7, half of it random.",
    },
    Example {
        method: "token.remove",
        request: r#"{"tasqx":"1","id":"tr1","method":"token.remove","params":{"measurement_id":"019f7c0a-3d51-7d22-b73c-9353de0a3360"}}"#,
        fixture: "",
        response: r#"{"id":"tr1","ok":true,"result":{"removed":{"cache_creation_tokens":12000,"cache_read_tokens":91000,"confidence":"medium","created":"2026-09-16T09:00:00Z","id":"019f7c0a-3d51-7d22-b73c-9353de0a3360","input_tokens":18400,"model":"opus","output_tokens":2600,"source":"self-report","tool":"claude-code","total_tokens":0},"short_id":51},"tasqx":"1"}"#,
        why: "it deletes a measurement by id, and the only measurement to delete is one a previous call just minted — so the request itself quotes an id no fixture can pin.",
    },
    Example {
        method: "tokens.recompute",
        request: r#"{"tasqx":"1","id":"rc1","method":"tokens.recompute"}"#,
        fixture: "api-tokens-recompute",
        response: "",
        why: "",
    },
    Example {
        method: "dependency.add",
        request: r#"{"tasqx":"1","id":"da1","method":"dependency.add","params":{"ref":"55","depends_on":"50"}}"#,
        fixture: "api-dependency-add",
        response: "",
        why: "",
    },
    Example {
        method: "dependency.remove",
        request: r#"{"tasqx":"1","id":"dr1","method":"dependency.remove","params":{"ref":"51","depends_on":"50"}}"#,
        fixture: "api-dependency-remove",
        response: "",
        why: "",
    },
    Example {
        method: "link.add",
        request: r#"{"tasqx":"1","id":"la1","method":"link.add","params":{"from":"51","to":"memory:eb864f1e-e68a-4d96-af89-597bd0d2d52e","relation":"implements_decision"}}"#,
        fixture: "",
        response: r#"{"id":"la1","ok":true,"result":{"created":true,"created_at":"2026-09-17T09:12:04Z","from":"task:019f7c0a-3d51-7c42-9a08-1f0c4e5b62d7","id":"019f8b31-77a4-7f10-8c55-2d7e9a13b004","metadata":null,"relation":"implements_decision","to":"memory:eb864f1e-e68a-4d96-af89-597bd0d2d52e"},"tasqx":"1"}"#,
        why: "a new link mints a fresh v7 id whose low bits are random, and the response also carries the task's own uuid — the pinned clock fixes the timestamp half of each and nothing fixes the rest, so a captured answer would differ on every run.",
    },
    Example {
        method: "link.remove",
        request: r#"{"tasqx":"1","id":"lr1","method":"link.remove","params":{"id":"019f8b31-77a4-7f10-8c55-2d7e9a13b004"}}"#,
        fixture: "",
        response: r#"{"id":"lr1","ok":true,"result":{"id":"019f8b31-77a4-7f10-8c55-2d7e9a13b004","removed":true},"tasqx":"1"}"#,
        why: "it echoes the link id it was given, and that id came from a `link.add` whose own answer cannot be captured — so a fixture here would pin a uuid no other example on this page can produce.",
    },
    Example {
        method: "link.list",
        request: r#"{"tasqx":"1","id":"ll1","method":"link.list","params":{"ref":"51"}}"#,
        fixture: "",
        response: r#"{"id":"ll1","ok":true,"result":{"count":1,"links":[{"created_at":"2026-09-17T09:12:04Z","from":"task:019f7c0a-3d51-7c42-9a08-1f0c4e5b62d7","id":"019f8b31-77a4-7f10-8c55-2d7e9a13b004","metadata":null,"relation":"implements_decision","to":"memory:eb864f1e-e68a-4d96-af89-597bd0d2d52e"}],"next_offset":null,"total":1},"tasqx":"1"}"#,
        why: "every row carries the link's own v7 id and the uuid of each endpoint, all minted when the fixture store was built — half clock, half entropy, so no pin reproduces the row.",
    },
    Example {
        method: "graph.query",
        request: r#"{"tasqx":"1","id":"gq1","method":"graph.query","params":{"root":"51","depth":1}}"#,
        fixture: "api-graph-query",
        response: "",
        why: "",
    },
    Example {
        method: "memory.add",
        request: r#"{"tasqx":"1","id":"ma1","method":"memory.add","params":{"title":"rate-limit-ceiling","body":"60 requests a minute per key, burst 120.","source":"notes/limits.md","project":"api"}}"#,
        fixture: "",
        response: r#"{"id":"ma1","ok":true,"result":{"created":"2026-09-16T09:00:00Z","id":"019f7c0a-3d51-7b18-8d44-6e1a92c07f35","project":"api","standing":false,"title":"rate-limit-ceiling"},"tasqx":"1"}"#,
        why: "the doc's id is minted fresh, v7, half of it random.",
    },
    Example {
        method: "memory.search",
        request: r#"{"tasqx":"1","id":"ms1","method":"memory.search","params":{"query":"release canary","limit":2}}"#,
        fixture: "",
        response: r#"{"id":"ms1","ok":true,"result":{"count":1,"has_more":false,"hits":[{"id":"eb864f1e-e68a-4d96-af89-597bd0d2d52e","kind":"doc","project":"api","rank":-1.2419537228757,"snippet":"Cut the release branch on Monday, tag after the canary has run for a day…","source":"docs/release.md","standing":false,"title":"release-process"}],"matched":"release AND canary","total":1},"tasqx":"1"}"#,
        why: "every hit carries an FTS bm25 `rank` whose last digits are the platform's `log()`, so a capture is reproducible only on the machine that took it. The `tasqx memory search` screen is captured instead — it prints the snippet, never the number.",
    },
    Example {
        method: "memory.get",
        request: r#"{"tasqx":"1","id":"mg1","method":"memory.get","params":{"id":"eb864f1e-e68a-4d96-af89-597bd0d2d52e"}}"#,
        fixture: "api-memory-get",
        response: "",
        why: "",
    },
    Example {
        method: "memory.remove",
        request: r#"{"tasqx":"1","id":"mr1","method":"memory.remove","params":{"id":"091eb5ff-05d5-4cb2-ba2f-0afdc77f7935"}}"#,
        fixture: "api-memory-remove",
        response: "",
        why: "",
    },
    Example {
        method: "memory.import",
        request: r#"{"tasqx":"1","id":"mi1","method":"memory.import","params":{"docs":[{"title":"release-process","body":"Cut the release branch on Monday.","source":"docs/release.md"}]}}"#,
        fixture: "api-memory-import",
        response: "",
        why: "",
    },
    Example {
        method: "memory.list",
        request: r#"{"tasqx":"1","id":"m1","method":"memory.list"}"#,
        fixture: "api-memory-list",
        response: "",
        why: "",
    },
    Example {
        method: "memory.update",
        request: r#"{"tasqx":"1","id":"mu1","method":"memory.update","params":{"id":"1bce1a9b-5134-4ab7-8665-34cd79fe0c5f","body":"The handover happens Friday at 16:00, in the on-call channel."}}"#,
        fixture: "api-memory-update",
        response: "",
        why: "",
    },
    Example {
        method: "report.summary",
        request: r#"{"tasqx":"1","id":"r1","method":"report.summary","params":{"group_by":"project"}}"#,
        fixture: "api-report-summary",
        response: "",
        why: "",
    },
    Example {
        method: "report.outcomes",
        request: r#"{"tasqx":"1","id":"ro1","method":"report.outcomes","params":{"group_by":"project","metrics":["rework","calibration"]}}"#,
        fixture: "api-report-outcomes",
        response: "",
        why: "",
    },
    Example {
        method: "store.export",
        request: r#"{"tasqx":"1","id":"x1","method":"store.export","params":{"filter":"+docs"}}"#,
        fixture: "",
        response: r##"{"id":"x1","ok":true,"result":{"default_project":"website","docs":[{"_rev":0,"body":"# Pricing page…","created":"2026-09-09T09:15:00Z","id":"0787b26d-9e2e-4be5-ab66-ec953102fad3","modified":"2026-09-11T09:15:00Z","project":"website","source":"notes/pricing.md","standing":false,"title":"pricing-page-decisions"}],"dropped_dependencies":0,"events":[{"actor":"user","entity":"task","entity_id":"46773aad-c4aa-435a-abe1-fcde8ce09658","id":"37e2265e-0745-46cf-ab75-44127cc95bc2","op":"add","payload":{"title":"Write the migration guide for SDK 3.0"},"ts":"2026-08-26T10:00:00Z"}],"projects":[{"archived":false,"created":"2026-06-18T09:00:00Z","description":"Public REST API and its SDKs","id":"309d6b79-965e-4a32-9ae4-45508201e2bd","name":"api"}],"tasks":[{"…":"one whole task per row"}]},"tasqx":"1"}"##,
        why: "the real answer is the entire store — every task, project, doc and audit row, in one payload. What matters here is its shape, so this example is a single row of each with the bodies elided.",
    },
    Example {
        method: "store.import",
        request: r#"{"tasqx":"1","id":"si1","method":"store.import","params":{"tasks":[{"id":"9a1f3c52-0b44-4a2e-9d6f-7c5b1e08a4d1","short_id":900,"title":"Restore the incident timeline","status":"pending","priority":"M","project":"infra","created":"2026-09-10T08:00:00Z","modified":"2026-09-10T08:00:00Z","_rev":1,"tags":["ops"],"depends_on":[],"annotations":[]}]}}"#,
        fixture: "api-store-import",
        response: "",
        why: "",
    },
    Example {
        method: "event.list",
        request: r#"{"tasqx":"1","id":"ev1","method":"event.list","params":{"entity":"project","limit":2}}"#,
        fixture: "api-event-list",
        response: "",
        why: "",
    },
    Example {
        method: "event.revert",
        request: r#"{"tasqx":"1","id":"u1","method":"event.revert"}"#,
        fixture: "",
        response: r#"{"id":"u1","ok":true,"result":{"_rev":5,"restored":{"tags":["ops"]},"reverted":{"event":"9c2b4d61-1f77-4a0c-9c1e-2b8e5d3a7f40","op":"tag.remove","ts":"2026-09-16T09:00:00Z"},"short_id":53,"title":"Move nightly backups to object storage"},"tasqx":"1"}"#,
        why: "it undoes whatever is NEWEST in the log, so its answer depends on store state a fixture cannot pin — and on the demo store the newest event is a `start`, which undo refuses by name rather than reversing.",
    },
    Example {
        method: "reminder.fire",
        request: r#"{"tasqx":"1","id":"rf1","method":"reminder.fire","params":{"ref":"49","at":"2026-09-17T16:00:00Z"}}"#,
        fixture: "api-reminder-fire",
        response: "",
        why: "",
    },
    Example {
        method: "core.capabilities",
        request: r#"{"tasqx":"1","id":"c1","method":"core.capabilities"}"#,
        fixture: "",
        response: "",
        why: "its `store` is the path of whatever store answered, so a capture would quote the machine that took it. The handshake at the top of this page is rendered live from this build's own `capabilities()` with a plausible store instead (D74).",
    },
    Example {
        method: "otlp.status",
        request: r#"{"tasqx":"1","id":"o1","method":"otlp.status"}"#,
        fixture: "api-otlp-status",
        response: "",
        why: "",
    },
];

// ============================================================================
// The colours the JSON blocks use
// ============================================================================

/// Appended to the guide's stylesheet by `super::css`.
///
/// Fixed colours, not theme variables, and for the reason `term_screen`'s
/// container already carries: a `.snip` is dark in BOTH themes, so a palette
/// that followed `prefers-color-scheme` would put light-mode ink on a dark
/// panel. `pre.json` gets a height cap because a response is as long as the
/// store makes it and the code column is `position: sticky` — an uncapped
/// `store.export` would be taller than the viewport it is meant to sit beside.
pub(super) const CSS: &str = r#"
/* ---- JSON, coloured (#647) ---- */
pre.json { max-height: 26rem; overflow: auto; white-space: pre; }
.j-k { color: #88c0d0; }
.j-s { color: #a3be8c; }
.j-n { color: #d08770; }
.j-b { color: #b48ead; }
.pills { display: flex; flex-wrap: wrap; gap: 0.35rem; margin: 0 0 0.55rem; }
.shapepath { font-family: ui-monospace, "SF Mono", Consolas, monospace; font-size: 0.72rem;
  color: var(--muted); margin: 0.9rem 0 0.35rem; }
.shapepath code { background: none; border: 0; padding: 0; font-size: 1em; }
"#;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The manifest, read at compile time like every other cross-file guard
    /// here: a moved fixtures directory has to be a compile error, not a test
    /// that reports "file missing".
    const MANIFEST: &str = include_str!("../../docs-fixtures/manifest.tsv");

    fn manifest_rows() -> Vec<Vec<&'static str>> {
        MANIFEST
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| l.split('\t').collect())
            .collect()
    }

    /// Every method this build serves has exactly one example, and every
    /// example names a method it serves.
    #[test]
    fn every_method_has_exactly_one_example() {
        let served: BTreeSet<&str> = tasqx_core::PARAMS.iter().map(|(m, _, _)| *m).collect();
        let documented: BTreeSet<&str> = EXAMPLES.iter().map(|e| e.method).collect();
        assert_eq!(
            served, documented,
            "the API reference's examples and the served method set disagree"
        );
        assert_eq!(
            EXAMPLES.len(),
            served.len(),
            "a method has two examples; the page renders the first and the other is invisible"
        );
    }

    /// An example is EITHER captured or illustrative, and an illustrative one
    /// says why.
    ///
    /// The failure this closes is the one the whole fixture pipeline exists
    /// for: a hand-typed response that looks exactly like a captured one, and
    /// is wrong the moment the shape moves. A reader cannot tell them apart by
    /// looking, so the page has to say which it is, and that cannot be
    /// optional.
    #[test]
    fn an_example_is_either_captured_or_labelled_illustrative() {
        for e in EXAMPLES {
            let captured = !e.fixture.is_empty();
            let invented = !e.response.is_empty() || !e.why.is_empty();
            assert!(
                captured != invented,
                "`{}`: an example is a fixture or an illustration, not both and not neither",
                e.method
            );
            if invented {
                assert!(
                    !e.why.is_empty(),
                    "`{}` shows an invented response with no reason beside it",
                    e.method
                );
                assert!(
                    e.why.len() > 30,
                    "`{}`'s reason is too short to be one: {:?}",
                    e.method,
                    e.why
                );
            }
        }
        // Floor: most of the surface must be real output, or this reference is
        // a mock-up with a guard on it.
        let captured = EXAMPLES.iter().filter(|e| !e.fixture.is_empty()).count();
        assert!(
            captured * 3 >= EXAMPLES.len() * 2,
            "only {captured} of {} methods show a captured response",
            EXAMPLES.len()
        );
    }

    /// A captured example's request is the manifest row's `stdin`, byte for
    /// byte — so the envelope printed above the response is the one that
    /// produced it.
    ///
    /// Without this the page could show request A beside response B with every
    /// other gate green, which is the most convincing way to be wrong.
    #[test]
    fn a_captured_example_shows_the_request_that_produced_it() {
        let rows = manifest_rows();
        for e in EXAMPLES.iter().filter(|e| !e.fixture.is_empty()) {
            let row = rows
                .iter()
                .find(|r| r[0] == e.fixture)
                .unwrap_or_else(|| panic!("`{}` names no manifest row", e.fixture));
            assert_eq!(
                row[4], "api",
                "the `{}` row does not run `tasqx api`",
                e.fixture
            );
            assert_eq!(
                row[6], e.request,
                "the `{}` row was captured from a different envelope than the page shows",
                e.fixture
            );
        }
    }

    /// Every example request is a well-formed envelope for the method it
    /// illustrates, naming only keys the engine accepts.
    #[test]
    fn every_example_request_is_one_the_engine_would_accept() {
        for e in EXAMPLES {
            let v: Value = serde_json::from_str(e.request)
                .unwrap_or_else(|err| panic!("`{}`'s request is not JSON: {err}", e.method));
            assert_eq!(v["tasqx"], "1", "`{}`'s request has no version", e.method);
            assert_eq!(
                v["method"], e.method,
                "`{}`'s example calls another method",
                e.method
            );
            let accepted: BTreeSet<&str> = accepted_params(e.method).iter().copied().collect();
            if let Some(params) = v["params"].as_object() {
                for key in params.keys() {
                    assert!(
                        accepted.contains(key.as_str()),
                        "`{}`'s example sends `{key}`, which the params gate refuses",
                        e.method
                    );
                }
            }
        }
    }

    /// Every example response carries exactly the keys the documented shape
    /// says it does — at `result` and at every nested object the shape names.
    ///
    /// This is what keeps an ILLUSTRATIVE example honest. The captured ones are
    /// the binary's own bytes; the hand-written ones are a person's idea of the
    /// answer, and the whole reason they are allowed is that the shape beside
    /// them is generated. Checking them against that same shape means an
    /// invented response cannot quietly show a key the method stopped emitting,
    /// or leave out one it started emitting (the `standing` flag D156 added to
    /// every doc row was that case) — the exact rot that made every other
    /// hand-typed output on this site a liability. Nested rows are walked too,
    /// because that is where an added key hides: a hit, a doc, an export row.
    ///
    /// An object carrying an elision key (`"…"`) is exempt: it is how an
    /// oversized example says "a row of the shape above goes here" without
    /// pasting a store. `core.capabilities` is the one example with no response
    /// of its own: the page renders the live handshake for it, which
    /// `the_capabilities_snippet_is_the_envelope_the_api_really_emits` already
    /// checks against `tasqx api` itself.
    #[test]
    fn every_example_response_matches_the_documented_shape() {
        /// Every object in `v`, under the path vocabulary `result_shape` uses:
        /// `result.hits[]` for each element of an array of objects.
        fn objects<'a>(
            path: String,
            v: &'a Value,
            out: &mut Vec<(String, &'a serde_json::Map<String, Value>)>,
        ) {
            if let Value::Object(map) = v {
                for (k, child) in map {
                    match child {
                        Value::Array(items) => {
                            for item in items {
                                objects(format!("{path}.{k}[]"), item, out);
                            }
                        }
                        _ => objects(format!("{path}.{k}"), child, out),
                    }
                }
                out.push((path, map));
            }
        }

        // Rows whose columns the REQUEST picks: `metrics` names which ones a
        // report group carries, so a real answer legitimately lacks the rest.
        // Only the other direction — no key the shape does not document — is
        // checked there. The freeze makes the same call: its report cases ask
        // for every metric they hold as required.
        const CALLER_SELECTED_ROWS: [(&str, &str); 2] = [
            ("report.summary", "result.groups[]"),
            ("report.outcomes", "result.groups[]"),
        ];

        let mut checked = 0;
        for e in EXAMPLES {
            if e.fixture.is_empty() && e.response.is_empty() {
                continue;
            }
            let value = response_json(e);
            assert_eq!(value["ok"], true, "`{}`'s example is not ok", e.method);
            assert!(
                value["result"].is_object(),
                "`{}`'s example result is not an object",
                e.method
            );
            let shape = tasqx_core::docs::result_shape(e.method);
            let mut found = Vec::new();
            objects("result".to_string(), &value["result"], &mut found);
            for (path, obj) in found {
                if obj.contains_key("…") {
                    continue;
                }
                let fields: Vec<&tasqx_core::docs::FieldDoc> = shape
                    .iter()
                    .filter(|(p, _)| *p == path)
                    .flat_map(|(_, group)| group.iter())
                    .collect();
                // A path the shape does not name is one the freeze leaves
                // open (`payload`, `set`, `restored`), not a gap.
                if fields.is_empty() {
                    continue;
                }
                for key in obj.keys() {
                    assert!(
                        fields.iter().any(|f| f.key == key),
                        "`{}`'s example answers with `{path}.{key}`, which the documented shape \
                         does not carry",
                        e.method
                    );
                }
                if CALLER_SELECTED_ROWS.contains(&(e.method, path.as_str())) {
                    continue;
                }
                for f in fields.iter().filter(|f| !f.optional) {
                    assert!(
                        obj.contains_key(f.key),
                        "`{}`'s example is missing `{path}.{}`, which is always present",
                        e.method,
                        f.key
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 150, "only {checked} keys compared");
    }

    /// Every parameter of every method reaches the page with a type and a
    /// description, from one of the two sources and not from nowhere.
    #[test]
    fn every_parameter_is_documented_exactly_once() {
        for (method, accepted, _) in tasqx_core::PARAMS {
            let rows = params_of(method);
            let names: Vec<&str> = rows.iter().map(|r| r.name).collect();
            assert_eq!(
                names,
                accepted.to_vec(),
                "`{method}`: the rendered parameter list is not the accepted key set"
            );
            for row in &rows {
                assert!(
                    !row.ty.is_empty() && row.ty != "any",
                    "`{method}.{}` has no type",
                    row.name
                );
                assert!(
                    row.html_desc.len() > 8,
                    "`{method}.{}`'s description is a placeholder: {:?}",
                    row.name,
                    row.html_desc
                );
            }
        }
    }

    /// [`PARAM_DOCS`] describes only parameters that exist, and only ones no
    /// MCP schema already describes.
    ///
    /// Both halves matter. A row for a key the engine does not accept is a
    /// parameter the page invents; a row beside a schema is a second
    /// description of one key, which is how the page and the tool start
    /// teaching different things.
    #[test]
    fn the_fallback_table_covers_only_what_no_schema_does() {
        for (method, param, ..) in PARAM_DOCS {
            let accepted = accepted_params(method);
            assert!(
                accepted.contains(param),
                "PARAM_DOCS describes `{method}.{param}`, which the engine does not accept"
            );
            assert!(
                described_property(method, param).is_none(),
                "PARAM_DOCS describes `{method}.{param}`, which the MCP schema already describes \
                 — one key, two descriptions, and nothing holding them together"
            );
        }
    }

    /// The methods no CLI verb reaches are the nine we know about.
    ///
    /// Read in both directions from [`super::VERBS`]' Method column, because
    /// that column is prose a human maintains: a mapping lost to a typo would
    /// otherwise print "no verb reaches this method" on a page under a verb
    /// that plainly does.
    #[test]
    fn only_the_expected_methods_have_no_cli_verb() {
        let orphans: BTreeSet<&str> = tasqx_core::PARAMS
            .iter()
            .map(|(m, _, _)| *m)
            .filter(|m| verbs_for(m).is_empty())
            .collect();
        let expected: BTreeSet<&str> = [
            // The handshake: `tasqx config` reads it, and the VERBS row says so
            // in prose rather than naming it, so this is not a gap.
            "core.capabilities",
            // An operator diagnostic with no verb yet (#222).
            "otlp.status",
            // Daemon-internal.
            "reminder.fire",
            // The token ledger's corrective half: `tasqx api token.remove`.
            "token.remove",
            // D160's explicit links have no verb yet: `tasqx api link.add` is
            // the way, and the graph UI the family was built for is a desktop
            // client rather than a terminal screen.
            "link.add",
            "link.remove",
            "link.list",
            // D160's projection is drawn, not printed: its consumer is the
            // desktop client, and `tasqx api graph.query` is the way from a
            // terminal.
            "graph.query",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            orphans, expected,
            "the set of methods with no CLI verb changed — either a verb mapping was lost, or \
             one was added and this list has not caught up"
        );
    }

    /// The page carries one addressable section per method, at the anchor the
    /// CLI reference links to.
    #[test]
    fn every_method_has_its_own_section() {
        let page = page();
        for (method, ..) in tasqx_core::PARAMS {
            assert!(
                page.contains(&format!("<section class=\"ref\" id=\"api-{method}\"")),
                "no `#api-{method}` section on the JSON API page"
            );
            assert!(
                page.contains(&format!("href=\"#api-{method}\"")),
                "`{method}` is in no index on the page, so nothing links to its section"
            );
        }
    }

    /// Every section renders all three columns: parameters, a response shape,
    /// and an example.
    #[test]
    fn every_section_carries_parameters_a_shape_and_an_example() {
        let page = page();
        assert_eq!(
            page.matches("<h4>Response</h4>").count(),
            tasqx_core::PARAMS.len(),
            "a method section has no response shape"
        );
        assert_eq!(
            page.matches("<h4>Parameters</h4>").count(),
            tasqx_core::PARAMS.len(),
            "a method section has no parameter block"
        );
        assert_eq!(
            page.matches("<pre class=\"out json\">").count(),
            tasqx_core::PARAMS.len(),
            "a method section has no example response"
        );
        // Three tabs per section, plus the two the transport and error blocks
        // carry.
        assert!(
            page.matches("data-tab=\"MCP\"").count() >= tasqx_core::PARAMS.len(),
            "a method section has no MCP tab"
        );
    }

    /// Nothing a description carries can become markup.
    #[test]
    fn a_description_is_escaped_and_its_backticks_become_code() {
        let out = describe("a `key` and <b>not bold</b> & co");
        assert!(out.contains("<code>key</code>"));
        assert!(out.contains("&lt;b&gt;"), "raw markup survived: {out}");
        assert!(out.contains("&amp; co"));
        assert!(!out.contains("<b>"));
    }

    /// The JSON renderer colours the four kinds and escapes the values.
    #[test]
    fn json_is_coloured_and_escaped() {
        let v: Value = serde_json::from_str(
            r#"{"title":"a <b> & \"quoted\"","n":3,"ok":true,"nil":null,"empty":[]}"#,
        )
        .expect("valid JSON");
        let html = json_html(&v, 0);
        assert!(html.contains("<span class=\"j-k\">&quot;title&quot;</span>"));
        assert!(html.contains("<span class=\"j-n\">3</span>"));
        assert!(html.contains("<span class=\"j-b\">true</span>"));
        assert!(html.contains("<span class=\"j-b\">null</span>"));
        assert!(html.contains("[]"), "an empty array is one line");
        assert!(
            html.contains("&lt;b&gt;"),
            "a value's markup was not escaped: {html}"
        );
        assert!(
            !html.contains("<b>"),
            "a value's markup reached the page: {html}"
        );
    }

    /// The stylesheet the coloured blocks need is really in the document.
    #[test]
    fn the_json_colours_reach_the_stylesheet() {
        let doc = super::super::generate();
        for class in [".j-k", ".j-s", ".j-n", ".j-b", "pre.json"] {
            assert!(
                doc.contains(class),
                "`{class}` is used by the API page and styled nowhere"
            );
        }
    }

    /// The highlighted JSON is still JSON once the markup is peeled off.
    ///
    /// A response body is the store's text, and a note can carry a tab, a
    /// carriage return or an escape byte. Hand-escaping only `\`, `"` and `\n`
    /// left the rest raw, where [`esc`] then dropped them — so the page showed
    /// a value the API never sent, and a copied example did not parse back.
    #[test]
    fn highlighted_json_parses_back_to_the_value_it_rendered() {
        let nasty = "tab\t cr\r bs\u{8} ff\u{c} esc\u{1b} quote\" slash\\ nl\n <b>&";
        let value = serde_json::json!({ nasty: [nasty, { "k": nasty }], "plain": 1 });

        let html = json_html(&value, 0);
        let mut text = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => text.push(c),
                _ => {}
            }
        }
        let text = text
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&");

        let back: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("rendered JSON does not parse ({e}):\n{text}"));
        assert_eq!(back, value);
    }

    /// A reference block's prose column never ends on a colon: what a colon
    /// promises sits in the OTHER column, which on a wide screen is beside the
    /// sentence, not under it — so the sentence dangles.
    #[test]
    fn no_prose_column_ends_promising_what_follows() {
        let doc = super::super::generate();
        for section in doc.split("<section class=\"ref\" id=\"").skip(1) {
            let id = section.split('"').next().unwrap_or_default();
            let prose = section
                .split("<div class=\"ref-code\">")
                .next()
                .unwrap_or_default()
                .trim_end()
                .trim_end_matches("</div>");
            assert!(
                !prose.ends_with(":</p>"),
                "`{id}`'s prose ends with a colon and nothing under it"
            );
        }
    }

    /// The request envelope's comments start in one column.
    #[test]
    fn the_request_envelope_comments_line_up() {
        let page = page();
        let block = page
            .split("id=\"h-request\"")
            .nth(1)
            .and_then(|s| s.split("</pre>").next())
            .expect("a Request block");
        let columns: BTreeSet<usize> = block
            .lines()
            .map(|l| l.replace("&quot;", "\""))
            .filter_map(|l| l.find("//").map(|i| l[..i].chars().count()))
            .collect();
        assert_eq!(columns.len(), 1, "comments start at columns {columns:?}");
    }

    /// The transport section shows the transport, not a table screen of some
    /// other client of the same method.
    #[test]
    fn the_transport_section_shows_only_the_transport() {
        let page = page();
        let section = page
            .split("<section class=\"ref\" id=\"api-transport\"")
            .nth(1)
            .and_then(|s| s.split("</section>").next())
            .expect("a transport section");
        assert!(
            !section.contains("<code>tasqx list</code>"),
            "the transport section shows the `tasqx list` screen"
        );
        assert!(section.contains("| tasqx api"));
    }

    /// Response shapes are stacked field rows, not a four-column table, which
    /// in a half-width reference column crushed the description to one word a
    /// line.
    #[test]
    fn response_shapes_render_as_field_rows() {
        let out = response_tables("task.done");
        assert!(!out.contains("<table"), "{out}");
        assert!(
            out.contains("<code class=\"fname\">blocked_by</code>"),
            "{out}"
        );
    }
}
