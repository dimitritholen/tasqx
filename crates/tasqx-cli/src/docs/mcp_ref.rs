//! The MCP reference page: how to run the server, then one section per tool
//! (#647).
//!
//! # Generated from the server, not beside it
//!
//! Every tool section is built from `tasqx_core::mcp::tool_docs()` — the same
//! table `tools/list` serves — so a tool's name, its scope, its hints, the
//! description a model reads and its `inputSchema` reach this page from the
//! only place they exist. The table that used to sit in `docs.rs` restating
//! them is gone; what is left of that guard now reads the roster directly, in
//! both directions, so a tool can neither ship undocumented nor survive on the
//! page after it is removed.
//!
//! # What a tool section deliberately does NOT repeat
//!
//! The result. §7 maps a tool 1:1 onto a JSON API method and the conformance
//! suite freezes what comes back through `tools/call` as that method's own
//! result — so the response shape is documented once, in the
//! [JSON API reference](super::api_ref), and each tool links to it. Rendering
//! it twice would be two descriptions of one contract, which is the shape of
//! drift this site keeps deleting.

use serde_json::Value;

use super::{
    h3, lead, note, p, page_close, page_open, param_table, pre_plain, ref_section, snippet, tabs,
    term_block, Param,
};
use crate::html::esc;

pub(super) fn page() -> String {
    let mut s = page_open("mcp");

    s.push_str(&lead(
        "tasqx bundles an MCP server, so an AI agent reads and mutates your tasks with zero glue. \
         It is the same core API underneath — the agent and your shell are peers.",
    ));

    let scope_runs = format!(
        "{}{}",
        snippet(
            "tasqx mcp serve\ntasqx mcp serve --scope write",
            "tasqx mcp: serving over stdio (scope=read)\ntasqx mcp: serving over stdio (scope=write)",
        ),
        snippet(
            "tasqx mcp serve   # scope omitted",
            "tasqx mcp: serving over stdio (scope=read)",
        ),
    );
    s.push_str(&ref_section(
        "mcp-scope",
        "It fails closed",
        &p(
            "The least-privilege default is <strong>read-only</strong>. Write access is an explicit \
             operator choice for this local stdio process. Scope is configuration, not an \
             authentication credential.",
        ),
        &tabs(&[
            ("CLI", &scope_runs),
            (
                "MCP",
                &p(&format!(
                    "Under read scope the {} write tools are absent from <code>tools/list</code> \
                     entirely, so an agent is never aimed at a call it will be refused. Each tool \
                     below states the scope it needs.",
                    tools().iter().filter(|t| t.write).count(),
                )),
            ),
        ]),
    ));

    // The second two-column block on this page: the transport described on the
    // left, the two captured exchanges beside it.
    let talking_prose = format!(
        "{}{}",
        p("Newline-delimited JSON-RPC 2.0 on stdin/stdout. Diagnostics go to stderr <em>only</em> — \
           stdout carries nothing but responses, so the transport is never corrupted by a log line."),
        p("The <code>initialize</code> result carries <code>instructions</code>: a scope-aware \
           workflow the host may inject into the agent's system prompt, so the agent is told \
           <em>when</em> to reach for tasqx and not only what it can call. It is elided beside \
           this, because it is a few paragraphs long."),
    );
    let exchanges = format!(
        "{}{}",
        snippet(
        "echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"demo\",\"version\":\"1\"}}}' | tasqx mcp serve 2>/dev/null | sed 's/\"instructions\":\"[^\"]*\"/\"instructions\":\"…\"/'",
        // The version comes from the crate, not from a copy of it. This snippet
        // shipped `"version":"0.1.0"` for the whole of 0.2.x: a captured output
        // is a claim about what the binary answers, and a hand-typed one stops
        // being true at the next release with nothing to notice. `Mcp::initialize`
        // fills the field from `CARGO_PKG_VERSION`, so read it from the same
        // place and the page cannot drift again.
        &format!(
            "{{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{{\"capabilities\":{{\"tools\":{{}}}},\
             \"instructions\":\"…\",\"protocolVersion\":\"2024-11-05\",\
             \"serverInfo\":{{\"name\":\"tasqx\",\"version\":\"{}\"}}}}}}",
            env!("CARGO_PKG_VERSION")
        ),
        ),
        snippet(
        "echo '{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"tasqx_list_tasks\",\"arguments\":{\"filter\":\"+api\"}}}' | tasqx mcp serve 2>/dev/null",
        "{\"id\":3,\"jsonrpc\":\"2.0\",\"result\":{\"content\":[{\"text\":\"{\\n  \\\"count\\\": 1,\\n  \\\"next_offset\\\": null,\\n  \\\"store_empty\\\": false,\\n  \\\"tasks\\\": [\\n    {\\n      \\\"blocked\\\": false,\\n      \\\"due\\\": \\\"2026-07-17T00:00:00Z\\\",\\n      \\\"priority\\\": \\\"H\\\",\\n      \\\"project\\\": \\\"work.tasqx\\\",\\n      \\\"short_id\\\": 1,\\n      \\\"status\\\": \\\"pending\\\",\\n      \\\"tags\\\": [\\n        \\\"api\\\",\\n        \\\"release\\\"\\n      ],\\n      \\\"title\\\": \\\"Ship the v1 JSON API freeze\\\",\\n      \\\"urgency\\\": 17.5\\n    }\\n  ],\\n  \\\"total\\\": 1\\n}\",\"type\":\"text\"}],\"isError\":false}}",
        ),
    );
    s.push_str(&ref_section(
        "mcp-transport",
        "Talking to it",
        &talking_prose,
        &tabs(&[
            ("MCP", &exchanges),
            (
                "JSON API",
                &p(
                    "The same calls without the JSON-RPC wrapper are <a href=\"#api\">the JSON \
                     API</a>: one envelope in, one out, and each tool below links to the method \
                     it routes to.",
                ),
            ),
        ]),
    ));

    s.push_str(&h3("Refusing a write"));
    s.push_str(&p(
        "Ask a read-only server to write, and it refuses by name:",
    ));
    s.push_str(&snippet(
        "echo '{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"tasqx_add_task\",\"arguments\":{\"title\":\"nope\"}}}' | tasqx mcp serve 2>/dev/null",
        "{\"id\":4,\"jsonrpc\":\"2.0\",\"result\":{\"content\":[{\"text\":\"error [bad_request]: tool `tasqx_add_task` requires write scope, but this MCP server is running read-only. This cannot be changed from a tool call: the operator must relaunch the server as `tasqx mcp serve --scope write`.\",\"type\":\"text\"}],\"isError\":true}}",
    ));

    s.push_str(&h3("Wiring it into a client"));
    s.push_str(&p(
        "Most MCP clients take a command and arguments. Pass the scope you want the child \
         process to have:",
    ));
    s.push_str(&pre_plain(
        "{\n\
         \x20 \"mcpServers\": {\n\
         \x20   \"tasqx\": {\n\
         \x20     \"command\": \"tasqx\",\n\
         \x20     \"args\": [\"mcp\", \"serve\", \"--scope\", \"write\"]\n\
         \x20   }\n\
         \x20 }\n\
         }",
    ));
    s.push_str(&note(
        "Start an agent with the default read scope. Give it write only once you have watched what \
         it does with read. A future network transport needs real authentication; this scope flag \
         is not a credential.",
    ));

    s.push_str(&h3("Every tool"));
    let reads = tools().iter().filter(|t| !t.write).count();
    s.push_str(&p(&format!(
        "{reads} read tools always; {writes} write tools only with write scope. Each carries MCP \
         annotations (<code>readOnlyHint</code>, <code>destructiveHint</code>) so a client can reason \
         about them before calling.",
        writes = tools().len() - reads,
    )));
    s.push_str(&index_table());

    for tool in tools() {
        s.push_str(&tool_section(tool));
    }

    s.push_str(&h3("What an agent cannot reach"));
    s.push_str(&p(&format!(
        "An agent cannot reach every method: {} of them are deliberately off the tool surface. \
         Not exposing a tool is a decision here rather than silence — each one names its reason, \
         and a method that ships with neither a tool nor a line in this table fails the build:",
        super::count_word(tasqx_core::mcp::unexposed_methods().len()),
    )));
    let rows: Vec<Vec<String>> = tasqx_core::mcp::unexposed_methods()
        .iter()
        .map(|(method, why)| {
            vec![
                format!("<a href=\"#api-{method}\"><code>{method}</code></a>"),
                super::api_ref::describe(why),
            ]
        })
        .collect();
    s.push_str(&super::table_owned(&["Method", "Why not"], &rows));

    s.push_str(&page_close("mcp"));
    s
}

/// Built once per process: `tool_docs()` allocates, and every section, every
/// count and every guard below would otherwise rebuild the roster.
fn tools() -> &'static [tasqx_core::mcp::ToolDoc] {
    static TOOLS: std::sync::LazyLock<Vec<tasqx_core::mcp::ToolDoc>> =
        std::sync::LazyLock::new(tasqx_core::mcp::tool_docs);
    &TOOLS
}

/// A jump table: every tool, its scope, the method it routes to.
fn index_table() -> String {
    let rows: Vec<Vec<String>> = tools()
        .iter()
        .map(|t| {
            vec![
                format!(
                    "<a href=\"#mcp-{name}\"><code>{name}</code></a>",
                    name = t.name
                ),
                if t.write { "write" } else { "read" }.to_string(),
                format!(
                    "<a href=\"#api-{method}\"><code>{method}</code></a>",
                    method = t.method
                ),
                first_sentence(t.description),
            ]
        })
        .collect();
    super::table_owned(&["Tool", "Scope", "Method", "Does"], &rows)
}

/// One tool: the description a model reads, its arguments, and the call.
fn tool_section(tool: &'static tasqx_core::mcp::ToolDoc) -> String {
    let id = format!("mcp-{}", tool.name);

    let mut left = String::new();
    left.push_str(&pills(tool));
    left.push_str(&p(&super::api_ref::describe(tool.description)));
    left.push_str(&p(&format!(
        "Routes to <a href=\"#api-{method}\"><code>{method}</code></a>, and hands back that \
         method's result — the shape is documented there, once.",
        method = tool.method,
    )));

    left.push_str("<h4>Arguments</h4>");
    let rows = arguments(tool);
    if rows.is_empty() {
        left.push_str(&p("None."));
    } else {
        left.push_str(&param_table(&id, &rows));
    }

    ref_section(
        &id,
        tool.name,
        &left,
        &tabs(&[
            ("MCP", &call_example(tool)),
            (
                "JSON API",
                &p(&format!(
                    "The same call as an envelope — arguments are the method's params, one for \
                     one — is under <a href=\"#api-{method}\"><code>{method}</code></a>, with a \
                     real request and the response it produced.",
                    method = tool.method,
                )),
            ),
        ]),
    )
}

/// The three facts a host's confirmation policy keys off, as pills.
fn pills(tool: &tasqx_core::mcp::ToolDoc) -> String {
    let scope = if tool.write {
        "<span class=\"pill req\">write scope</span>"
    } else {
        "<span class=\"pill opt\">read scope</span>"
    };
    let destructive = if tool.destructive {
        "<span class=\"pill req\">destructive</span>"
    } else {
        ""
    };
    let idempotent = if tool.idempotent {
        "<span class=\"pill opt\">idempotent</span>"
    } else {
        ""
    };
    format!("<div class=\"pills\">{scope}{destructive}{idempotent}</div>")
}

/// A tool's arguments, from its own `inputSchema`.
///
/// An argument the method does not accept is one this server reads and consumes
/// rather than forwarding (`include_json`, `view`, `annotations_limit` on the
/// tools that page), and it is labelled so: it exists on this transport and
/// nowhere else, which is precisely what a reader moving between the two
/// surfaces needs to know. Derived from the accepted key set rather than a
/// second list of transport arguments — the params gate would refuse them, so
/// the engine's own table already says which they are.
fn arguments(tool: &'static tasqx_core::mcp::ToolDoc) -> Vec<Param<'static>> {
    let accepted: Vec<&str> = tasqx_core::PARAMS
        .iter()
        .find(|(m, _, _)| *m == tool.method)
        .map(|(_, keys, _)| keys.to_vec())
        .unwrap_or_default();
    let required: Vec<&str> = tool.schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(props) = tool.schema["properties"].as_object() else {
        return Vec::new();
    };
    props
        .iter()
        .map(|(name, prop)| {
            let forwarded = accepted.contains(&name.as_str());
            // A forwarded argument IS a method param, so it is described by the
            // JSON API reference's own lookup — schema first, fallback table
            // for the keys no schema describes. Reading the schema directly
            // here would leave those arguments blank on this page while
            // the other page describes them.
            let (ty, desc) = if forwarded {
                let (ty, _, desc) = super::api_ref::param_doc(tool.method, name);
                (ty, desc.to_string())
            } else {
                (
                    super::api_ref::schema_badge(prop),
                    format!(
                        "<span class=\"pill opt\">MCP only</span> {} This argument is read by the \
                         server and never forwarded: it describes the response envelope, which is \
                         the transport's own business and not something <code>{}</code> has an \
                         opinion about.",
                        super::api_ref::describe(prop["description"].as_str().unwrap_or_default()),
                        tool.method,
                    ),
                )
            };
            Param {
                name: Box::leak(name.clone().into_boxed_str()),
                ty,
                required: required.contains(&name.as_str()),
                default: None,
                html_desc: Box::leak(desc.into_boxed_str()),
            }
        })
        .collect()
}

/// A `tools/call` for this tool, with the arguments the JSON API reference's
/// example for the same method sends — so the two pages show one call.
fn call_example(tool: &tasqx_core::mcp::ToolDoc) -> String {
    let args = super::api_ref::example_params(tool.method);
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool.name, "arguments": args },
    });
    format!(
        "{}{}",
        term_block(&esc(&serde_json::to_string(&call).unwrap_or_default())),
        p(
            "The answer is <code>content</code> text: the method's result as JSON, unless the \
           description above says this tool sends less of it by default — a rendered view, a \
           compact row, no <code>rank</code> — in which case an argument marked \
           <em>MCP only</em>, or <code>fields</code>, asks for the rest. <code>isError</code> is \
           <code>true</code> for a refusal, which is a value and not a transport failure."
        ),
    )
}

/// The first sentence of a tool description, for the index table.
///
/// The descriptions are written for a model deciding whether to call, so they
/// are paragraphs; a jump table wants a line. Cut at the first `. ` rather than
/// at a character budget, so the cell is always a whole sentence.
fn first_sentence(text: &str) -> String {
    let cut = text.find(". ").map(|i| i + 1).unwrap_or(text.len());
    super::api_ref::describe(text[..cut].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page documents exactly the roster the server serves.
    ///
    /// This is what the hand-written `MCP_TOOLS` table used to be checked
    /// against, now with nothing in between: the sections ARE the roster, so
    /// the only way to be wrong is for `tools/list` to serve something
    /// `tool_docs` does not — which the conformance suite's own live-roster
    /// tests already refuse.
    #[test]
    fn every_tool_has_a_section_and_every_section_is_a_tool() {
        let page = page();
        let roster = tasqx_core::mcp::tool_roster();
        assert!(
            roster.len() >= 15,
            "the MCP roster shrank below the shipped tool set: {}",
            roster.len()
        );
        for (name, write) in &roster {
            assert!(
                page.contains(&format!("<section class=\"ref\" id=\"mcp-{name}\"")),
                "MCP tool `{name}` has no section on the page"
            );
            let doc = tools()
                .iter()
                .find(|t| t.name == *name)
                .unwrap_or_else(|| panic!("`{name}` is in the roster and not in tool_docs"));
            assert_eq!(
                doc.write, *write,
                "read/write drift on `{name}` between the roster and the docs view"
            );
        }
        let sections = page
            .matches("<section class=\"ref\" id=\"mcp-tasqx_")
            .count();
        assert_eq!(
            sections,
            roster.len(),
            "the page renders a tool section the roster does not have"
        );
    }

    /// Every tool names the method it routes to, and that method has a section
    /// on the JSON API page to land on.
    #[test]
    fn every_tool_links_to_a_real_method() {
        let served: Vec<&str> = tasqx_core::PARAMS.iter().map(|(m, _, _)| *m).collect();
        for tool in tools() {
            assert!(
                served.contains(&tool.method),
                "`{}` routes to `{}`, which this build does not serve",
                tool.name,
                tool.method
            );
        }
        let page = page();
        for tool in tools() {
            assert!(
                page.contains(&format!("href=\"#api-{}\"", tool.method)),
                "`{}`'s section does not link to its method",
                tool.name
            );
        }
    }

    /// Every argument of every tool reaches the page with a description.
    #[test]
    fn every_argument_is_described() {
        for tool in tools() {
            for row in arguments(tool) {
                assert!(
                    row.html_desc.len() > 8,
                    "`{}.{}` has no description",
                    tool.name,
                    row.name
                );
                assert!(
                    !row.ty.is_empty(),
                    "`{}.{}` has no type",
                    tool.name,
                    row.name
                );
            }
        }
    }

    /// An argument the params gate would refuse is labelled as transport-only,
    /// and an argument the method accepts is not.
    ///
    /// Both halves bite. A forwarded argument marked "MCP only" sends a caller
    /// looking for a second way to do what `params` already does; a
    /// transport-only one left unmarked sends them to a `bad_request` for
    /// copying what the page showed.
    #[test]
    fn transport_only_arguments_are_labelled_and_no_others_are() {
        for tool in tools() {
            let accepted: Vec<&str> = tasqx_core::PARAMS
                .iter()
                .find(|(m, _, _)| *m == tool.method)
                .map(|(_, keys, _)| keys.to_vec())
                .unwrap_or_default();
            for row in arguments(tool) {
                let labelled = row.html_desc.contains("MCP only");
                assert_eq!(
                    labelled,
                    !accepted.contains(&row.name),
                    "`{}.{}`: the transport-only label and the accepted key set disagree",
                    tool.name,
                    row.name
                );
            }
        }
    }

    /// The methods with no tool are listed with their reasons, linked to the
    /// API sections a human would use instead.
    #[test]
    fn the_unexposed_methods_are_named_with_their_reasons() {
        let page = page();
        let unexposed = tasqx_core::mcp::unexposed_methods();
        assert!(
            unexposed.len() >= 10,
            "the unexposed table shrank to {} — a tool was added, or the table was gutted",
            unexposed.len()
        );
        for (method, _) in unexposed {
            assert!(
                page.contains(&format!(
                    "<a href=\"#api-{method}\"><code>{method}</code></a>"
                )),
                "`{method}` has no tool and the page does not say so"
            );
        }
        // And the count in the prose is the one the table has.
        assert!(
            page.contains(&format!(
                "{} of them are deliberately",
                super::super::count_word(unexposed.len())
            )),
            "the sentence over the table does not count it"
        );
    }

    /// The first sentence of a description is a sentence.
    #[test]
    fn the_index_cell_is_one_whole_sentence() {
        assert_eq!(first_sentence("One. Two. Three."), "One.");
        assert_eq!(first_sentence("No full stop"), "No full stop");
        assert_eq!(
            first_sentence("A `tag` here. And more."),
            "A <code>tag</code> here."
        );
    }
}
