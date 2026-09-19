//! The Objects section: one page per thing the JSON API hands back (#648, D161).
//!
//! # Nothing on these pages is retyped
//!
//! | What | Where it comes from |
//! |---|---|
//! | every field row | the `tasqx_core::docs` groups [`OBJECTS`] names, the same rows the API page renders |
//! | the example object | a slice of one method's example on the API page — captured, or illustrative with its reason |
//! | the operations list | every method whose namespace is the object's, or whose response shape carries one of its groups or a group it is `linked` to ([`objects_of`]) |
//! | each operation's CLI verb and MCP tool | the API page's own `verbs_for` / `tool_for` |
//!
//! [`objects_of`] is also what the API page reads to link a method back to its
//! objects, so the two directions are one function and cannot disagree.
//!
//! # Which page owns a field
//!
//! A field belongs to the object it DESCRIBES, not to the object it is nested
//! in. `checks` sits on a task and is documented on Check; `depends_on`,
//! `blocks`, `blocked` and `unmet_blockers` on Dependency; `annotations` and
//! `annotations_removed` on Annotation; `tokens` on Measurement. A field is
//! identified by where it sits — a key at one path of one method's response —
//! so `id` on a task and `id` on a check are two fields on two pages.
//! [`PAGING_KEYS`] are the only keys inside an object that belong to no page:
//! they describe the response, not the thing. The envelope keys of a list
//! result (`count`, `total`, `next_offset`) never reach this question, because
//! a list result carries no object group and so is not an object position.

use serde_json::Value;

use super::api_ref::{
    describe, example, group_of, json_html, presence, response_json, tool_for, verbs_for,
};
use super::cli_ref::screen_cmd;
use super::{
    field_list, lead, note, p, page_close, page_open, pre_plain, ref_section, table_owned,
    term_screen,
};
use crate::html::esc;
use tasqx_core::docs::{self as d, FieldDoc};

/// One object page.
pub(super) struct Object {
    /// The page id, `obj-<slug>` — frozen like every other page id.
    pub(super) id: &'static str,
    /// The sidebar label and the page title.
    pub(super) name: &'static str,
    /// What it is and how it lives. Trusted markup, a literal in this file.
    lead: &'static str,
    /// The API page's group prefixes whose methods act on this object.
    namespaces: &'static [&'static str],
    /// Groups whose presence in a response links a method to this page
    /// without making their fields this object's — a search hit may be a
    /// document, but its row is the search's.
    linked: &'static [&'static [FieldDoc]],
    /// `(table caption, row-name prefix, group)`. An empty caption is the
    /// object's own table; a prefix names a nested object's rows
    /// (`urgency_breakdown.priority`).
    groups: &'static [(&'static str, &'static str, &'static [FieldDoc])],
    /// `(method, JSON pointer into its example's result, keys kept)`. No keys
    /// keeps the whole object.
    example: (&'static str, &'static str, &'static [&'static str]),
}

const ON_A_TASK: &str = "On a task";

/// The ten objects, in sidebar order.
pub(super) const OBJECTS: [Object; 10] = [
    Object {
        id: "obj-task",
        name: "Task",
        lead: "A task is one piece of work: a title, the dates and priority that score its \
               urgency, the time tracked against it, and everything hung off it — notes, \
               acceptance criteria, dependencies and token spend, each documented on its own page. \
               No method deletes a task, and <code>event.revert</code> refuses to undo its \
               <code>add</code>: a finished task is <code>done</code>, an abandoned one \
               <code>cancelled</code>, and <code>task.reopen</code> brings either back.",
        namespaces: &["task", "tag", "reminder"],
        linked: &[],
        groups: &[
            ("", "", d::TASK_CORE),
            ("", "", d::TASK_STATUS_FLAG),
            ("", "", d::TASK_LIVE_TIME),
            ("", "", d::TASK_EXPORT_TIME),
            ("", "", d::TASK_BUDGET_GAUGE),
            ("", "", d::TASK_SPAWNED_FROM),
            ("", "", d::TASK_EXPORT_SPAWNED_FROM),
            ("", "", d::TASK_URGENCY_BREAKDOWN),
            ("", "urgency_breakdown.", d::URGENCY_BREAKDOWN_ROW),
        ],
        example: ("task.get", "", &[]),
    },
    Object {
        id: "obj-project",
        name: "Project",
        lead: "A project is a name tasks and knowledge docs are filed under. One project can be \
               the store's default, where a task added without a project lands. Archiving hides a \
               project and removes nothing.",
        namespaces: &["project"],
        linked: &[],
        groups: &[
            ("", "", d::PROJECT_LIST_ROW),
            ("", "", d::PROJECT_EXPORT_ROW),
        ],
        example: ("project.list", "/projects/0", &[]),
    },
    Object {
        id: "obj-annotation",
        name: "Annotation",
        lead: "An annotation is a note on a task, stored verbatim. <code>annotation.update</code> \
               corrects one in place, keeping its id, timestamp and position (D165), and \
               <code>store.import</code> replaces an imported task's notes with the document's. <code>annotation.remove</code> scrubs a note's text and leaves a \
               tombstone — its id and when it went — under <code>annotations_removed</code>; \
               undoing the <code>annotation.add</code> that wrote a note deletes it outright, \
               with no tombstone.",
        namespaces: &["annotation"],
        linked: &[],
        groups: &[
            ("", "", d::ANNOTATION_ROW),
            ("", "", d::ANNOTATION_CAP),
            ("", "annotations_removed[].", d::TOMBSTONE_ROW),
            (ON_A_TASK, "", d::TASK_ANNOTATIONS),
            (ON_A_TASK, "", d::TASK_EXPORT_ANNOTATIONS),
            (ON_A_TASK, "", d::TASK_ANNOTATIONS_REMOVED),
            (ON_A_TASK, "", d::TASK_CARD_NOTES),
            (ON_A_TASK, "", d::TASK_EXPORT_PIN),
        ],
        example: ("task.get", "/annotations/0", &[]),
    },
    Object {
        id: "obj-check",
        name: "Check",
        lead: "A check is one acceptance criterion on a task (D138): a sentence tasqx stores and \
               does not execute. It starts <code>open</code>, and <code>check.set</code> moves it to \
               <code>passed</code>, <code>failed</code> or back to <code>open</code>, with the \
               evidence that decided it.",
        namespaces: &["check"],
        linked: &[],
        groups: &[("", "", d::TASK_CHECK), (ON_A_TASK, "", d::TASK_CHECKS)],
        example: ("task.get", "/checks/0", &[]),
    },
    Object {
        id: "obj-dependency",
        name: "Dependency",
        lead: "A dependency is an edge between two tasks: this task waits on that one. It has no \
               id of its own: the API reads it off the tasks at both ends — \
               <code>depends_on</code> on the task that waits, <code>blocks</code> on the one it \
               waits on. A \
               task is <code>blocked</code> while it is open and at least one task it waits on is \
               neither <code>done</code> nor <code>cancelled</code>.",
        namespaces: &["dependency"],
        linked: &[],
        groups: &[
            ("", "", d::TASK_DEPENDS_ON),
            ("", "", d::TASK_BLOCKS),
            ("", "", d::TASK_BLOCKED),
            ("", "", d::TASK_UNMET_BLOCKERS),
            ("", "unmet_blockers[].", d::BLOCKER_ROW),
        ],
        example: (
            "task.get",
            "",
            &["depends_on", "blocks", "blocked", "unmet_blockers"],
        ),
    },
    Object {
        id: "obj-memory",
        name: "Memory document",
        lead: "A memory document is a piece of knowledge the store keeps beside the tasks: a \
               title and a body, searchable together with the notes on tasks. It can be scoped to a \
               project and marked standing (D156). Importing a doc with a <code>source</code> \
               already stored replaces that doc in place; removing one is permanent.",
        namespaces: &["memory"],
        linked: &[d::MEMORY_HIT_ROW],
        groups: &[("", "", d::DOC_EXPORT_ROW), ("", "", d::MEMORY_LIST_ROW)],
        example: ("memory.get", "", &[]),
    },
    Object {
        id: "obj-link",
        name: "Link",
        lead: "A link is an explicit edge between two nodes of the knowledge graph (D160): a \
               task, a memory document, an annotation or a project at either end, and a relation \
               naming what it asserts. Links are written and removed, never edited, and a repeat \
               of one that already exists answers with the link that is there rather than a \
               second row. Unlike a dependency they may form cycles, and unlike an inferred \
               graph edge they are durable: nothing mints one but a caller who asked for it. \
               Removing a memory document removes its links with it; an annotation tombstone \
               leaves them alone, because the node is still there.",
        namespaces: &["link"],
        linked: &[],
        groups: &[("", "", d::LINK_ROW)],
        example: ("link.list", "/links/0", &[]),
    },
    Object {
        id: "obj-graph",
        name: "Graph",
        lead: "A graph projection is what <code>graph.query</code> hands back: the nodes around \
               one root and the edges between them, bounded and deterministically ordered so the \
               same call twice is the same picture. A node is a task, a memory document, an \
               annotation or a project, wearing the id a link's endpoints use. An edge is either \
               <em>structural</em> — read out of a table, and naming that table as its \
               <code>source</code> — or <em>inferred</em>, computed for that one call from an \
               FTS search on the root's title or from the tags two tasks share, carrying a \
               <code>confidence</code> and stored nowhere. Nothing here is a row in the store: a \
               projection is a view, and the durable edges it draws are <a \
               href=\"#obj-link\">links</a>.",
        namespaces: &["graph"],
        linked: &[],
        groups: &[("", "", d::GRAPH_NODE), ("Edges", "", d::GRAPH_EDGE)],
        example: ("graph.query", "/nodes/0", &[]),
    },
    Object {
        id: "obj-event",
        name: "Event",
        lead: "An event is one row of the audit log. Writes to tasks — their notes, checks, \
               dependencies and measurements included — to projects and to memory documents \
               append one. Events are not edited, with one narrow exception: \
               <code>annotation.remove</code> redacts the body out of the \
               <code>annotation.add</code> event that wrote the note, so a removed note's text \
               does not survive in the log. <code>event.revert</code> rewrites nothing either: it \
               undoes the newest event by appending its inverse, and refuses by name the ops it \
               cannot reverse.",
        namespaces: &["event"],
        linked: &[],
        groups: &[("", "", d::EVENT_ROW)],
        example: ("event.list", "/events/0", &[]),
    },
    Object {
        id: "obj-measurement",
        name: "Token measurement",
        lead: "A token measurement is AI token spend banked against a task: four buckets kept \
               apart (D48), the tool and model that spent them, and how checkable the \
               figure is. A task's measurements are read under <code>tokens</code>.",
        namespaces: &["token"],
        linked: &[],
        groups: &[
            ("", "", d::MEASUREMENT_ROW),
            (ON_A_TASK, "", d::TASK_TOKENS),
            (ON_A_TASK, "", d::TASK_EXPORT_TOKENS),
        ],
        example: ("token.add", "/measurement", &[]),
    },
];

/// Two groups are the same group when they carry the same rows. Compared by
/// content, because a `const` slice has no address it is guaranteed to keep.
fn same_group(a: &[FieldDoc], b: &[FieldDoc]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.key == y.key && x.desc == y.desc)
}

/// The objects a method acts on or answers with, in [`OBJECTS`] order: by
/// namespace, by a group the page owns, or by a group the page is only
/// `linked` to — `task.brief`'s memory hits link the Memory page without their
/// search rows becoming document fields.
///
/// The API page links a method section to exactly these pages, and each page
/// lists exactly the methods that name it here.
pub(super) fn objects_of(method: &str) -> Vec<&'static Object> {
    let shape = d::result_shape(method);
    OBJECTS
        .iter()
        .filter(|o| {
            o.namespaces.contains(&group_of(method))
                || shape.iter().any(|(_, g)| {
                    o.groups.iter().any(|(_, _, og)| same_group(g, og))
                        || o.linked.iter().any(|lg| same_group(g, lg))
                })
        })
        .collect()
}

/// One row of an object's field table.
pub(super) struct FieldRow {
    pub(super) caption: &'static str,
    /// The field as the page names it: the group's page prefix plus its key.
    /// Variants of one field share it.
    pub(super) base: String,
    /// The field as the responses in `contexts` spell it — `base`, unless a
    /// response carries the group under a container of its own.
    pub(super) name: String,
    pub(super) field: &'static FieldDoc,
    /// The methods whose responses carry this spelling and contract of the
    /// field, in `PARAMS` order. Empty when the field has one spelling and one
    /// contract everywhere, so only a field that really differs is labelled.
    pub(super) contexts: Vec<&'static str>,
}

/// The same field, the same contract: type, nullability, optionality and
/// description all equal.
fn same_contract(a: &FieldDoc, b: &FieldDoc) -> bool {
    a.ty == b.ty && a.null_ok == b.null_ok && a.optional == b.optional && a.desc == b.desc
}

/// Every `(method, path)` whose documented response carries `group`, in
/// `PARAMS` order.
fn occurrences(group: &[FieldDoc]) -> Vec<(&'static str, &'static str)> {
    tasqx_core::PARAMS
        .iter()
        .flat_map(|(m, ..)| {
            d::result_shape(m)
                .iter()
                .filter(|(_, g)| same_group(g, group))
                .map(move |(path, _)| (*m, *path))
        })
        .collect()
}

/// How a response at `path` spells a key of a group the page lists under
/// `prefix`.
///
/// An empty prefix is a field of the object itself, wherever the object sits.
/// A prefix names a container (`unmet_blockers[].`): a response carrying the
/// group inside that container spells it that way, and one carrying it
/// anywhere else spells it under its own last path segment —
/// `task.done`'s `result.blocked_by[]` gives `blocked_by[].short_id`.
pub(super) fn spelling(prefix: &str, path: &str, key: &str) -> String {
    let Some(container) = prefix.strip_suffix('.') else {
        return format!("{prefix}{key}");
    };
    if path == container || path.ends_with(&format!(".{container}")) {
        return format!("{prefix}{key}");
    }
    let last = path.rsplit('.').next().unwrap_or(path);
    format!("{last}.{key}")
}

/// A page's field rows: one per distinct spelling and contract of each field.
///
/// A later group repeating a key is the same field as another response carries
/// it. Where the spelling and the contract are identical the responses share
/// one row; where either differs — `store.export` omits `active_since` and
/// `tokens` where `task.get` sends them, `task.done` carries blocker rows under
/// `blocked_by[]` rather than `unmet_blockers[]` — each is its own row,
/// labelled with the responses that carry it, rather than the first one
/// silently standing for all. A field's variants are adjacent, in the order
/// the field first appears.
pub(super) fn field_rows(o: &Object) -> Vec<FieldRow> {
    let mut rows: Vec<FieldRow> = Vec::new();
    for (caption, prefix, group) in o.groups {
        for (method, path) in occurrences(group) {
            for f in *group {
                let base = format!("{prefix}{}", f.key);
                let name = spelling(prefix, path, f.key);
                let same = rows.iter_mut().find(|r| {
                    r.caption == *caption
                        && r.base == base
                        && r.name == name
                        && same_contract(r.field, f)
                });
                match same {
                    Some(r) => {
                        if !r.contexts.contains(&method) {
                            r.contexts.push(method);
                        }
                    }
                    None => rows.push(FieldRow {
                        caption,
                        base,
                        name,
                        field: f,
                        contexts: vec![method],
                    }),
                }
            }
        }
    }
    let rank = |m: &str| tasqx_core::PARAMS.iter().position(|(p, ..)| *p == m);
    let mut out: Vec<FieldRow> = Vec::new();
    while !rows.is_empty() {
        let (caption, base) = (rows[0].caption, rows[0].base.clone());
        let mut variants: Vec<FieldRow> = Vec::new();
        let mut i = 0;
        while i < rows.len() {
            if rows[i].caption == caption && rows[i].base == base {
                variants.push(rows.remove(i));
            } else {
                i += 1;
            }
        }
        if variants.len() == 1 {
            variants[0].contexts.clear();
        } else {
            for v in &mut variants {
                v.contexts.sort_by_key(|m| rank(m));
            }
            variants.sort_by_key(|v| v.contexts.first().and_then(|m| rank(m)));
        }
        out.extend(variants);
    }
    out
}

// ============================================================================
// The pages
// ============================================================================

pub(super) fn page(o: &Object) -> String {
    let mut s = page_open(o.id);
    s.push_str(&lead(o.lead));
    s.push_str(&ref_section(
        &format!("{}-fields", o.id),
        "Fields",
        &fields_html(o),
        &example_html(o),
    ));
    if o.id == "obj-task" {
        s.push_str(&task_lifecycle());
        s.push_str(&task_urgency());
    }
    s.push_str(&operations_html(o));
    s.push_str(&page_close(o.id));
    s
}

fn fields_html(o: &Object) -> String {
    let rows = field_rows(o);
    let mut captions: Vec<&str> = Vec::new();
    for r in &rows {
        if !captions.contains(&r.caption) {
            captions.push(r.caption);
        }
    }
    let mut out = String::new();
    for caption in captions {
        if !caption.is_empty() {
            out.push_str(&format!("<div class=\"shapepath\">{}</div>", esc(caption)));
        }
        let cells: Vec<(String, String)> = rows
            .iter()
            .filter(|r| r.caption == caption)
            .map(|r| {
                // A labelled row is one contract of a field that differs by
                // response; the label names those responses and links them.
                let label = if r.contexts.is_empty() {
                    String::new()
                } else {
                    let links: Vec<String> = r
                        .contexts
                        .iter()
                        .map(|m| format!("<a href=\"#api-{m}\"><code>{m}</code></a>"))
                        .collect();
                    format!("<span class=\"pdef\">in {}</span>", links.join(" · "))
                };
                (
                    format!(
                        "<code class=\"fname\">{}</code><span class=\"badge\">{}</span>{}{label}",
                        esc(&r.name),
                        esc(r.field.ty),
                        presence(r.field.null_ok, r.field.optional),
                    ),
                    describe(r.field.desc),
                )
            })
            .collect();
        out.push_str(&field_list(&cells));
    }
    out
}

/// The object as a real response carries it, sliced out of the API page's
/// example for `method`, and labelled the way that example is.
pub(super) fn example_value(o: &Object) -> Value {
    let (method, pointer, keys) = o.example;
    let whole = response_json(example(method));
    let mut v = whole["result"]
        .pointer(pointer)
        .cloned()
        .unwrap_or_else(|| panic!("`{method}`'s example has nothing at `result{pointer}`"));
    if !keys.is_empty() {
        if let Value::Object(map) = &mut v {
            map.retain(|k, _| keys.contains(&k.as_str()));
        }
    }
    v
}

fn example_html(o: &Object) -> String {
    let (method, pointer, _) = o.example;
    let mut out = format!(
        "{}<div class=\"snip\"><pre class=\"out json\"><code>{}</code></pre></div>",
        p(&format!(
            "From <a href=\"#api-{method}\"><code>{method}</code></a>{}.",
            if pointer.is_empty() {
                String::new()
            } else {
                format!(", at <code>result{}</code>", esc(pointer))
            }
        )),
        json_html(&example_value(o), 0),
    );
    let why = example(method).why;
    if !why.is_empty() {
        out.push_str(&note(&format!(
            "<strong>Illustrative.</strong> {}",
            describe(why)
        )));
    }
    out
}

fn operations_html(o: &Object) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (method, ..) in tasqx_core::PARAMS {
        if !objects_of(method).iter().any(|x| x.id == o.id) {
            continue;
        }
        let verbs: Vec<String> = verbs_for(method)
            .iter()
            .map(|v| format!("<a href=\"#cli-{v}\"><code>{v}</code></a>"))
            .collect();
        let tool = tool_for(method).map_or_else(
            || "—".to_string(),
            |t| format!("<a href=\"#mcp-{n}\"><code>{n}</code></a>", n = t.name),
        );
        rows.push(vec![
            format!("<a href=\"#api-{method}\"><code>{method}</code></a>"),
            if verbs.is_empty() {
                "—".to_string()
            } else {
                verbs.join(" · ")
            },
            tool,
        ]);
    }
    format!(
        "<h3 id=\"{id}-ops\">Operations</h3>{}{}",
        p(
            "Every method that acts on this object or answers with it, and the verb and tool that \
           reach it."
        ),
        table_owned(&["JSON API", "CLI", "MCP"], &rows),
        id = o.id,
    )
}

// ============================================================================
// Task: lifecycle and urgency
// ============================================================================

/// The status machine, as `engine/task.rs` enforces it. `backlog` and
/// `pending` are one box because every write that stores `pending` — stop,
/// the D6 auto-stop, reopen — is read back through `effective_status`, so no
/// arrow may end in `pending` alone (PR #56 review). A raw string, so
/// the first line keeps its indentation (a `"\` continuation strips it).
const LIFECYCLE: &str = r"
                          task.add
                              │
 ┌─ open ─────────────────────┼─────────────────────────────┐
 │                            v                             │
 │  ┌────────────────────────────────────────────────────┐  │
 │  │ backlog  while a future wait or scheduled date     │  │
 │  │          holds it                                  │  │
 │  │ pending  otherwise — the clock and task.modify     │  │
 │  │          move a task between the two               │  │
 │  └────────────────────────────────────────────────────┘  │
 │     │                  ^                          ^      │
 │     │ task.start       │ task.stop, or            │      │
 │     │ (pending only)   │ another task.start       │      │
 │     v                  │ without keep (D6)        │      │
 │  ┌──────────────────────────┐                     │      │
 │  │          active          │                     │      │
 │  └──────────────────────────┘                     │      │
 └────────────┬──────────────────────┬───────────────┼──────┘
              │ task.done from       │ task.cancel   │
              │ pending or active    │ from any      │
              │                      │ open status   │
              v                      v               │
         ┌─────────┐           ┌───────────┐         │
         │  done   │           │ cancelled │         │
         └─────────┘           └───────────┘         │
              │                      │               │
              └──────────────────────┴───────────────┘
                            task.reopen";

fn task_lifecycle() -> String {
    format!(
        "<h3 id=\"obj-task-lifecycle\">Lifecycle</h3>{}{}{}{}",
        p("A task has five statuses. Three are open — <code>backlog</code>, <code>pending</code> \
           and <code>active</code> — and two are closed: <code>done</code> and \
           <code>cancelled</code>. Each arrow is labelled with the method that makes the move."),
        pre_plain(LIFECYCLE.trim_start_matches('\n')),
        p("<code>backlog</code> and <code>pending</code> are one question asked of the clock \
           every time a task is read: a task is in <code>backlog</code> while its \
           <code>wait</code> or <code>scheduled</code> instant is still in the future, and in \
           <code>pending</code> otherwise. Only those two statuses are read against the clock: \
           <code>active</code>, <code>done</code> and <code>cancelled</code> keep their status \
           whatever the task's dates say. So every arrow into that box lands in whichever of the \
           two the task's dates say — a task added, stopped or reopened with a future date is in \
           <code>backlog</code>, and moves to <code>pending</code> only once neither its \
           <code>wait</code> nor its <code>scheduled</code> is still in the future — each has \
           passed or been cleared with <code>task.modify</code>. There is no separate waiting status. <code>task.start</code> \
           and <code>task.done</code> refuse a backlog task. Starting a task without <code>keep</code> stops \
           every task already running (D6), with one exception (D140): when the start names an \
           <code>actor</code> and the most recently started running clock was started under a \
           different named actor, the start is refused instead. A start that names no actor, \
           as <code>tasqx start</code> sends none, auto-stops as usual; the MCP server fills in \
           its connection's id when a call names none. When the newest event is a \
           <code>task.stop</code>, <code>event.revert</code> takes it back, putting the task \
           back to <code>active</code> if it is still <code>pending</code>. <code>task.modify</code> refuses a <code>status</code> \
           other than <code>cancelled</code>."),
        p("<strong>Blocked</strong> is not a status either: it is the \
           <a href=\"#obj-dependency\"><code>blocked</code></a> flag, true while an open task \
           waits on a task that is neither done nor cancelled. <code>@working</code> is pending \
           or active and not blocked, and <code>task.done</code> refuses a blocked task unless \
           <code>force</code> records the override (D150)."),
    )
}

/// The priority term per priority, as `urgency::breakdown_at` scores it. The
/// page prints these, and a test holds them to the engine.
const PRIORITY_TERMS: [(&str, Option<tasqx_core::Priority>, f64); 4] = [
    ("H", Some(tasqx_core::Priority::H), 6.0),
    ("M", Some(tasqx_core::Priority::M), 3.9),
    ("L", Some(tasqx_core::Priority::L), 1.8),
    ("none", None, 0.0),
];

/// Days of lead time the due term ramps over, and the age term's per-day rate
/// and ceiling. Printed on the page; a test holds them to the engine.
const DUE_RAMP_DAYS: f64 = 14.0;
const AGE_PER_DAY: f64 = 0.01;
const AGE_CAP: f64 = 1.0;

fn task_urgency() -> String {
    let prio: Vec<String> = PRIORITY_TERMS
        .iter()
        .map(|(name, _, w)| format!("<code>{name}</code> {w:.1}"))
        .collect();
    let due = tasqx_core::urgency::DUE_WEIGHT;
    let prose = format!(
        "{}{}{}",
        p("<code>urgency</code> is one fixed formula (D1), the sum of three terms rounded to one \
           decimal. The weights are not configurable."),
        table_owned(
            &["Term", "Score"],
            &[
                vec!["priority".into(), prio.join(" · ")],
                vec![
                    "due proximity".into(),
                    format!(
                        "{due:.0} once the due date is reached or past; before that \
                         {due:.0} × (1 − days left ÷ {DUE_RAMP_DAYS:.0}), never below 0 — a due \
                         date more than {DUE_RAMP_DAYS:.0} days away adds nothing. No due date, 0."
                    ),
                ],
                vec![
                    "age".into(),
                    format!(
                        "{AGE_PER_DAY} per day since <code>created</code>, capped at {AGE_CAP:.1}."
                    ),
                ],
            ],
        ),
        p("<code>tasqx why</code> prints the three terms for one task. The screen beside this is \
           a task in the demo store: the terms, then their sum, then what still blocks it. \
           <code>task.get</code> with <code>explain: true</code> answers the same terms as \
           <a href=\"#obj-task-fields\"><code>urgency_breakdown</code></a>."),
    );
    ref_section(
        "obj-task-urgency",
        "Urgency",
        &prose,
        &term_screen(&screen_cmd("why"), "why"),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    /// Keys that sit inside an object and describe the RESPONSE rather than the
    /// thing: which page of a task's notes came back. They belong to no object
    /// page, and every one of them must still be met by the walk below.
    const PAGING_KEYS: [&str; 3] = [
        "annotations_total",
        "annotations_offset",
        "annotations_next_offset",
    ];

    /// The methods that act on no single object and answer with none.
    const NO_OBJECT: [&str; 5] = [
        "report.summary",
        "report.outcomes",
        "store.import",
        "core.capabilities",
        "otlp.status",
    ];

    /// The rendered HTML of one page: from its `<section class="page">` to the
    /// next one.
    fn page_html<'a>(doc: &'a str, id: &str) -> &'a str {
        let open = format!("<section class=\"page\" id=\"{id}\">");
        let start = doc.find(&open).unwrap_or_else(|| panic!("no page `{id}`"));
        let rest = &doc[start + open.len()..];
        let end = rest.find("<section class=\"page\"").unwrap_or(rest.len());
        &rest[..end]
    }

    /// Every field of every object appears on exactly one object page.
    ///
    /// Walks every method's documented response shape — held key for key to
    /// the conformance freeze by `documented_response_shapes_match_the_freeze`
    /// — and treats a path as an object position when any group there is on
    /// an object page. Every key at such a position must then be rendered by
    /// exactly one page, as a row of a group that page lists; a paging key by
    /// none. A group added to an object position and placed on no page is red,
    /// and so is a group placed on two. A row the page stops rendering is red
    /// at the HTML check.
    ///
    /// What it cannot see: a NEW object position whose every group is on no
    /// page — a response gaining a brand-new kind of row. That is a new object,
    /// and deciding it is one is a ruling, not a test.
    #[test]
    fn every_object_field_appears_on_exactly_one_object_page() {
        let doc = super::super::generate();
        let mut positions = 0usize;
        let mut paging_seen: BTreeSet<&str> = BTreeSet::new();
        for (method, ..) in tasqx_core::PARAMS {
            let mut by_path: BTreeMap<&str, Vec<&[FieldDoc]>> = BTreeMap::new();
            for (path, group) in d::result_shape(method) {
                by_path.entry(path).or_default().push(group);
            }
            for (path, groups) in by_path {
                let owners_of = |g: &[FieldDoc]| -> Vec<(&'static Object, &'static str)> {
                    OBJECTS
                        .iter()
                        .flat_map(|o| {
                            o.groups
                                .iter()
                                .filter(|(_, _, og)| same_group(g, og))
                                .map(move |(_, prefix, _)| (o, *prefix))
                        })
                        .collect()
                };
                if groups.iter().all(|g| owners_of(g).is_empty()) {
                    continue;
                }
                positions += 1;
                for g in &groups {
                    for f in *g {
                        let owners: Vec<(&Object, String)> = groups
                            .iter()
                            .filter(|g2| g2.iter().any(|f2| f2.key == f.key))
                            .flat_map(|g2| owners_of(g2))
                            .map(|(o, prefix)| (o, spelling(prefix, path, f.key)))
                            .collect();
                        let pages: BTreeSet<&str> = owners.iter().map(|(o, _)| o.id).collect();
                        if PAGING_KEYS.contains(&f.key) {
                            assert!(
                                pages.is_empty(),
                                "`{method}` {path}.{}: a paging key is documented on {pages:?}",
                                f.key
                            );
                            paging_seen.insert(f.key);
                            continue;
                        }
                        assert_eq!(
                            pages.len(),
                            1,
                            "`{method}` {path}.{} is on {pages:?} object pages, not exactly one",
                            f.key
                        );
                        for (o, name) in &owners {
                            assert!(
                                field_rows(o).iter().any(|r| r.name == *name
                                    && same_contract(r.field, f)
                                    && (r.contexts.is_empty() || r.contexts.contains(method))),
                                "`{method}` {path}.{}: `{}` has no `{name}` row with this \
                                 response's contract, labelled with `{method}` if it varies",
                                f.key,
                                o.id
                            );
                            let row = format!("<code class=\"fname\">{}</code>", esc(name));
                            assert!(
                                page_html(&doc, o.id).contains(&row),
                                "`{}` does not render its `{name}` row",
                                o.id
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(
            paging_seen,
            PAGING_KEYS.into_iter().collect(),
            "a PAGING_KEYS entry no longer sits inside any object"
        );
        assert!(positions > 20, "only {positions} object positions walked");
    }

    /// The field rows of one page naming `field`, as HTML.
    ///
    /// A row is `<div class="param field">` up to its first `</dd></div>`:
    /// the description is a `<dd>`, so its close is immediately followed by
    /// the row's own closing `</div>`.
    fn rows_named<'a>(page: &'a str, field: &str) -> Vec<&'a str> {
        let needle = format!("<code class=\"fname\">{field}</code>");
        page.split(super::super::FIELD_ROW)
            .skip(1)
            .map(|r| r.split("</dd></div>").next().unwrap_or(r))
            .filter(|r| r.contains(&needle))
            .collect()
    }

    /// A field whose contract differs between responses shows every contract,
    /// each labelled with the responses that carry it — never the first one
    /// alone (PR #56 review: `store.export` omits `active_since` and `tokens`
    /// where `task.get` sends them, and the page said nullable/always).
    #[test]
    fn a_field_whose_contract_differs_by_response_shows_each_variant() {
        let doc = super::super::generate();
        for (page, field, live, live_pill) in [
            ("obj-task", "active_since", "task.get", "nullable"),
            ("obj-measurement", "tokens", "task.get", "always"),
            ("obj-annotation", "annotations", "task.get", "always"),
        ] {
            let rows = rows_named(page_html(&doc, page), field);
            let export: Vec<&&str> = rows
                .iter()
                .filter(|r| r.contains("href=\"#api-store.export\""))
                .collect();
            assert_eq!(
                export.len(),
                1,
                "`{page}` has no row for `{field}` as store.export sends it: {rows:?}"
            );
            let live_rows: Vec<&&str> = rows
                .iter()
                .filter(|r| r.contains(&format!("href=\"#api-{live}\"")))
                .collect();
            assert_eq!(
                live_rows.len(),
                1,
                "`{page}` has no `{live}` row for `{field}`"
            );
            assert!(
                live_rows[0].contains(&format!(">{live_pill}</span>")),
                "`{page}`'s `{live}` row for `{field}` is not {live_pill}"
            );
            if field != "annotations" {
                assert!(
                    export[0].contains(">optional</span>"),
                    "`{page}`'s store.export row for `{field}` does not say optional"
                );
            } else {
                assert!(
                    export[0].contains("unpaged") && !export[0].contains("annotations_offset"),
                    "`{page}`'s store.export row for `annotations` carries the paging description"
                );
            }
        }
        // A field with one contract everywhere stays one unlabelled row.
        let id_rows = rows_named(page_html(&doc, "obj-task"), "id");
        assert_eq!(id_rows.len(), 1);
        assert!(
            !id_rows[0].contains("href=\"#api-"),
            "a single contract is labelled"
        );
    }

    /// A group reused at another path is spelled at that path, with the method
    /// that carries it there — not under the page's own prefix (PR #58 review:
    /// `task.done` carries blocker rows under `blocked_by[]`, and
    /// `annotation.remove` a tombstone under `removed`).
    #[test]
    fn a_group_reused_at_another_path_is_spelled_where_it_sits() {
        let doc = super::super::generate();
        for (page, spelled, method, not_under) in [
            (
                "obj-dependency",
                "blocked_by[].short_id",
                "task.done",
                "unmet_blockers[].short_id",
            ),
            (
                "obj-annotation",
                "removed.id",
                "annotation.remove",
                "annotations_removed[].id",
            ),
        ] {
            let html = page_html(&doc, page);
            let link = format!("href=\"#api-{method}\"");
            let rows = rows_named(html, spelled);
            assert_eq!(
                rows.len(),
                1,
                "`{page}` has no `{spelled}` row for `{method}`"
            );
            assert!(
                rows[0].contains(&link),
                "`{page}`'s `{spelled}` row does not name `{method}`"
            );
            assert!(
                rows_named(html, not_under)
                    .iter()
                    .all(|r| !r.contains(&link)),
                "`{page}` still files `{method}` under `{not_under}`"
            );
        }
    }

    /// Every group an object page lists is really in some response, so a page
    /// cannot document a row no method emits.
    #[test]
    fn every_listed_group_is_in_a_response_shape() {
        for o in &OBJECTS {
            for (_, _, g) in o.groups {
                assert!(
                    tasqx_core::PARAMS
                        .iter()
                        .any(|(m, ..)| d::result_shape(m).iter().any(|(_, sg)| same_group(sg, g))),
                    "`{}` lists a group no response carries",
                    o.id
                );
            }
        }
    }

    /// Each object page links to the methods that name it, and each of those
    /// method sections links back — and nothing else links either way.
    #[test]
    fn object_pages_and_method_sections_cross_link_both_ways() {
        let doc = super::super::generate();
        let mut from_objects: BTreeSet<(String, String)> = BTreeSet::new();
        for o in &OBJECTS {
            let html = page_html(&doc, o.id);
            let ops = &html[html.find("-ops\">").expect("an operations block")..];
            for (i, _) in ops.match_indices("href=\"#api-") {
                let rest = &ops[i + 11..];
                let m = &rest[..rest.find('"').unwrap()];
                from_objects.insert((o.id.to_string(), m.to_string()));
            }
        }
        let mut from_methods: BTreeSet<(String, String)> = BTreeSet::new();
        for (method, ..) in tasqx_core::PARAMS {
            let open = format!("<section class=\"ref\" id=\"api-{method}\"");
            let start = doc.find(&open).expect("a method section");
            let section = &doc[start..start + doc[start..].find("</section>").unwrap()];
            for (i, _) in section.match_indices("href=\"#obj-") {
                let rest = &section[i + 7..];
                let id = &rest[..rest.find('"').unwrap()];
                if OBJECTS.iter().any(|o| o.id == id) {
                    from_methods.insert((id.to_string(), method.to_string()));
                }
            }
        }
        let one_way: Vec<_> = from_objects.symmetric_difference(&from_methods).collect();
        assert!(
            one_way.is_empty(),
            "(object page, method) links that run only one way: {one_way:?}"
        );
        assert!(from_objects.len() > 40, "only {} links", from_objects.len());
        for o in &OBJECTS {
            assert!(
                from_objects.iter().any(|(id, _)| id == o.id),
                "`{}` lists no operation",
                o.id
            );
        }
    }

    /// The namespace table is the one hand list: every prefix is a real group
    /// of the API page, and the methods it leaves objectless are named.
    #[test]
    fn only_the_expected_methods_act_on_no_object() {
        let groups: BTreeSet<&str> = tasqx_core::PARAMS
            .iter()
            .map(|(m, ..)| group_of(m))
            .collect();
        for o in &OBJECTS {
            for ns in o.namespaces {
                assert!(
                    groups.contains(ns),
                    "`{}` names namespace `{ns}`, which has no method",
                    o.id
                );
            }
        }
        let orphans: BTreeSet<&str> = tasqx_core::PARAMS
            .iter()
            .map(|(m, ..)| *m)
            .filter(|m| objects_of(m).is_empty())
            .collect();
        assert_eq!(orphans, NO_OBJECT.into_iter().collect());
    }

    /// Every page shows an example, and every key the example carries is a
    /// documented field of some object page or a paging key.
    #[test]
    fn every_example_carries_only_documented_fields() {
        let names: BTreeSet<String> = OBJECTS
            .iter()
            .flat_map(|o| field_rows(o).into_iter().map(|r| r.name))
            .collect();
        for o in &OBJECTS {
            let v = example_value(o);
            let map = v
                .as_object()
                .unwrap_or_else(|| panic!("`{}`'s example is not an object", o.id));
            assert!(!map.is_empty(), "`{}`'s example is empty", o.id);
            for k in map.keys() {
                assert!(
                    names.contains(k) || PAGING_KEYS.contains(&k.as_str()),
                    "`{}`'s example carries `{k}`, which no object page documents",
                    o.id
                );
            }
        }
    }

    /// The lifecycle diagram and prose name the engine's real transitions:
    /// each claimed move succeeds and each refusal the prose states refuses.
    #[test]
    fn the_lifecycle_the_task_page_draws_is_the_engines() {
        use serde_json::json;
        let e = tasqx_core::Engine::open_in_memory().expect("engine");
        let call = |m: &str, p: Value| tasqx_core::dispatch(&e, m, &p);
        let add = |extra: Value| {
            let mut p = json!({"title": "t"});
            p.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            call("task.add", p).expect("add")["short_id"]
                .as_i64()
                .unwrap()
        };
        let status = |id: i64| call("task.get", json!({"ref": id})).unwrap()["status"].clone();

        let parked = add(json!({"wait": "2999-01-01T00:00:00Z"}));
        assert_eq!(status(parked), "backlog");
        assert!(call("task.start", json!({"ref": parked})).is_err());
        assert!(call("task.done", json!({"ref": parked})).is_err());
        call("task.modify", json!({"ref": parked, "set": {"wait": null}})).unwrap();
        assert_eq!(status(parked), "pending", "clearing the date releases it");
        call(
            "task.modify",
            json!({"ref": parked, "set": {"wait": "2999-01-01T00:00:00Z"}}),
        )
        .unwrap();
        assert_eq!(status(parked), "backlog", "a future date parks it again");
        call("task.cancel", json!({"ref": parked})).unwrap();
        assert_eq!(status(parked), "cancelled");

        let a = add(json!({}));
        let b = add(json!({}));
        assert_eq!(status(a), "pending");
        call("task.start", json!({"ref": a})).unwrap();
        assert_eq!(status(a), "active");
        call("task.start", json!({"ref": b})).unwrap();
        assert_eq!(
            status(a),
            "pending",
            "starting another without keep stops it"
        );
        call("task.stop", json!({"ref": b})).unwrap();
        assert_eq!(status(b), "pending");
        assert!(
            call("task.modify", json!({"ref": b, "set": {"status": "done"}})).is_err(),
            "modify sets only cancelled"
        );

        call("dependency.add", json!({"ref": b, "depends_on": a})).unwrap();
        assert!(
            call("task.done", json!({"ref": b})).is_err(),
            "blocked refuses done"
        );
        call("task.done", json!({"ref": b, "force": true})).unwrap();
        assert_eq!(status(b), "done");
        assert!(call("task.stop", json!({"ref": b})).is_err());
        call("task.reopen", json!({"ref": b})).unwrap();
        assert_eq!(status(b), "pending");
        call("task.cancel", json!({"ref": b})).unwrap();
        call("task.reopen", json!({"ref": b})).unwrap();
        assert_eq!(status(b), "pending");

        // Every write that stores `pending` — reopen, stop, the D6 auto-stop —
        // is read back through the clock rule, so a task still holding a
        // future date lands in backlog (PR #56 review).
        call("task.reopen", json!({"ref": parked})).unwrap();
        assert_eq!(
            status(parked),
            "backlog",
            "a reopened task with a future wait"
        );
        let c = add(json!({}));
        let d = add(json!({}));
        let later = json!({"scheduled": "2999-01-01T00:00:00Z"});
        call("task.start", json!({"ref": c})).unwrap();
        call("task.modify", json!({"ref": c, "set": later})).unwrap();
        assert_eq!(status(c), "active", "a date does not interrupt a clock");
        call("task.stop", json!({"ref": c})).unwrap();
        assert_eq!(status(c), "backlog", "a stopped task with a future date");
        call("task.modify", json!({"ref": c, "set": {"scheduled": null}})).unwrap();
        call("task.start", json!({"ref": c})).unwrap();
        call("task.modify", json!({"ref": c, "set": later})).unwrap();
        call("task.start", json!({"ref": d})).unwrap();
        assert_eq!(
            status(c),
            "backlog",
            "an auto-stopped task with a future date"
        );

        // `event.revert` takes back the newest `task.stop`, on a task still pending.
        let e2 = add(json!({}));
        call("task.start", json!({"ref": e2})).unwrap();
        call("task.stop", json!({"ref": e2})).unwrap();
        call("event.revert", json!({})).unwrap();
        assert_eq!(status(e2), "active", "undoing a stop restarts the clock");

        // Either future date holds a task in backlog; clearing one of two
        // releases nothing (PR #58 review, types.rs effective_status).
        let far = "2999-01-01T00:00:00Z";
        let held = add(json!({"wait": far, "scheduled": far}));
        call("task.modify", json!({"ref": held, "set": {"wait": null}})).unwrap();
        assert_eq!(
            status(held),
            "backlog",
            "the future scheduled still holds it"
        );
        call(
            "task.modify",
            json!({"ref": held, "set": {"scheduled": null}}),
        )
        .unwrap();
        assert_eq!(
            status(held),
            "pending",
            "released once neither is in the future"
        );
        // The rule never touches a running clock: effective_status leaves
        // Active alone (PR #58 review).
        call("task.start", json!({"ref": held, "keep": true})).unwrap();
        call("task.modify", json!({"ref": held, "set": {"wait": far}})).unwrap();
        assert_eq!(
            status(held),
            "active",
            "a future date does not park a running task"
        );

        // D140 refuses only when BOTH the incoming start and the running
        // clock's start named an actor, and they differ (PR #58 review).
        let f = add(json!({}));
        let g = add(json!({}));
        call("task.start", json!({"ref": f, "actor": "a"})).unwrap();
        assert!(
            call("task.start", json!({"ref": g, "actor": "b"})).is_err(),
            "a start naming another actor is refused"
        );
        assert_eq!(status(f), "active");
        call("task.start", json!({"ref": g})).unwrap();
        assert_eq!(
            status(f),
            "pending",
            "an actor-less start auto-stops a named clock"
        );
        call("task.start", json!({"ref": f, "actor": "b"})).unwrap();
        assert_eq!(
            status(g),
            "pending",
            "a named start auto-stops an actor-less clock"
        );
        // The Task page's `_rev` and `completed` rows (PR #58 re-read): token
        // spend leaves the revision alone, and a cancelled task is never
        // stamped `completed`.
        let get = |id: i64| call("task.get", json!({"ref": id})).unwrap();
        let rev = get(g)["_rev"].clone();
        call(
            "token.add",
            json!({"ref": g, "tool": "t", "source": "self-report", "confidence": "low",
                   "input_tokens": 1}),
        )
        .unwrap();
        assert_eq!(get(g)["_rev"], rev, "token spend bumped the task's _rev");
        call("tag.add", json!({"ref": g, "tags": ["x"]})).unwrap();
        assert_ne!(get(g)["_rev"], rev, "a tag change left _rev alone");
        call("task.cancel", json!({"ref": g})).unwrap();
        assert!(
            get(g)["completed"].is_null(),
            "a cancelled task was stamped"
        );

        let prose = task_lifecycle();
        assert!(
            prose.contains("once neither") && prose.contains("names an <code>actor</code>"),
            "the prose does not state the two-date hold or the two-actor refusal"
        );

        // So the drawing may not send any arrow to `pending` alone, and the
        // prose may not say a move ends there.
        assert!(
            !LIFECYCLE.contains("> pending"),
            "an arrow ends in pending, which a future wait or scheduled overrides"
        );
        let prose = task_lifecycle();
        assert!(
            !prose.contains("goes back to <code>pending</code>"),
            "the prose sends a stopped task to pending unconditionally"
        );
    }

    /// A method answering with memory search hits links the Memory page, and
    /// the page lists it, without the hit rows becoming document fields
    /// (PR #59 review: `task.brief` linked neither way).
    #[test]
    fn a_method_answering_with_memory_hits_links_the_memory_page() {
        let doc = super::super::generate();
        let memory = page_html(&doc, "obj-memory");
        let ops = &memory[memory.find("-ops\">").unwrap()..];
        for method in ["task.brief", "memory.search"] {
            assert!(
                ops.contains(&format!("href=\"#api-{method}\"")),
                "the Memory page does not list `{method}`"
            );
            assert!(
                objects_of(method).iter().any(|o| o.id == "obj-memory"),
                "`{method}` does not link the Memory page"
            );
        }
        assert!(
            rows_named(memory, "snippet").is_empty() && rows_named(memory, "rank").is_empty(),
            "a search hit's fields are documented as memory-document fields"
        );
        // A linked-only group is linked, not owned: it is on no page's field
        // list, and it is really in a response.
        for o in &OBJECTS {
            for g in o.linked {
                assert!(
                    OBJECTS
                        .iter()
                        .all(|p| p.groups.iter().all(|(_, _, pg)| !same_group(pg, g))),
                    "`{}` links a group some page owns",
                    o.id
                );
                assert!(
                    !occurrences(g).is_empty(),
                    "`{}` links a group no response carries",
                    o.id
                );
            }
        }
    }

    /// The backticked `entity` names the pages document are the ones the engine
    /// writes and `event.list` accepts (PR #59 review: `memory` for `doc`).
    #[test]
    fn documented_event_entities_are_the_engines() {
        let names = |text: &str| -> BTreeSet<String> {
            text.split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect()
        };
        let engine: BTreeSet<String> = tasqx_core::types::Entity::ALL
            .iter()
            .map(|e| e.as_str().to_string())
            .collect();
        let row = d::EVENT_ROW.iter().find(|f| f.key == "entity").unwrap();
        assert_eq!(names(row.desc), engine, "EVENT_ROW.entity");
        let (_, _, param) = super::super::api_ref::param_doc("event.list", "entity");
        assert_eq!(
            names(&param.replace("<code>", "`").replace("</code>", "`")),
            engine,
            "event.list's `entity` parameter"
        );
    }

    /// Descriptions that bind to engine behaviour say what the engine does.
    #[test]
    fn row_descriptions_match_the_engine() {
        use serde_json::json;
        let desc = |g: &[FieldDoc], k: &str| g.iter().find(|f| f.key == k).unwrap().desc;
        // D148: the body is stored whole, and a capped read marks the cut.
        let body = desc(d::ANNOTATION_ROW, "body");
        for marker in ["max_body_bytes", "body_truncated", "body_bytes"] {
            assert!(
                body.contains(marker),
                "ANNOTATION_ROW.body does not name `{marker}`"
            );
        }
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        let call = |m: &str, p: Value| tasqx_core::dispatch(&e, m, &p).unwrap();
        let id = call("task.add", json!({"title": "t"}))["short_id"].clone();
        call(
            "annotation.add",
            json!({"ref": id, "body": "0123456789abcdef"}),
        );
        let got = call("task.get", json!({"ref": id, "max_body_bytes": 4}));
        let note = &got["annotations"][0];
        assert_eq!(note["body_truncated"], true);
        assert_eq!(note["body_bytes"], 16);

        // effective_status swaps only pending and backlog: a future date keeps
        // active, done and cancelled as they are.
        for key in ["scheduled", "wait"] {
            let text = desc(d::TASK_CORE, key);
            for kept in ["`active`", "`done`", "`cancelled`"] {
                assert!(
                    text.contains(kept),
                    "TASK_CORE.{key} does not say `{kept}` is kept"
                );
            }
        }
        let far = "2999-01-01T00:00:00Z";
        let done = call("task.add", json!({"title": "d"}))["short_id"].clone();
        call("task.done", json!({"ref": done}));
        call("task.modify", json!({"ref": done, "set": {"wait": far}}));
        assert_eq!(call("task.get", json!({"ref": done}))["status"], "done");

        // `completed` is scoped to the methods that set and clear it.
        let completed = desc(d::TASK_CORE, "completed");
        assert!(
            completed.contains("task.cancel") && !completed.contains("never"),
            "TASK_CORE.completed makes a store-wide promise"
        );
    }

    /// The urgency terms the page prints are the ones the engine scores.
    #[test]
    fn the_urgency_formula_on_the_page_is_the_engines() {
        use tasqx_core::urgency::breakdown_at;
        let now: jiff::Timestamp = "2026-09-16T09:00:00Z".parse().unwrap();
        let term = |parts: Vec<(&str, f64)>, name: &str| {
            parts.into_iter().find(|(k, _)| *k == name).unwrap().1
        };
        let at = |days: f64| {
            jiff::Timestamp::from_second(now.as_second() + (days * 86_400.0) as i64)
                .unwrap()
                .to_string()
        };
        let created = now.to_string();
        for (_, prio, w) in PRIORITY_TERMS {
            assert_eq!(term(breakdown_at(prio, None, &created, now), "priority"), w);
        }
        let due = tasqx_core::urgency::DUE_WEIGHT;
        let due_term = |days: f64| {
            term(
                breakdown_at(None, Some(&at(days)), &created, now),
                "due_proximity",
            )
        };
        assert_eq!(due_term(-1.0), due);
        assert_eq!(due_term(0.0), due);
        assert!((due_term(DUE_RAMP_DAYS / 2.0) - due / 2.0).abs() < 1e-9);
        assert_eq!(due_term(DUE_RAMP_DAYS + 5.0), 0.0);
        assert_eq!(
            term(breakdown_at(None, None, &created, now), "due_proximity"),
            0.0
        );
        let age_term = |days: f64| term(breakdown_at(None, None, &at(-days), now), "age");
        assert!((age_term(50.0) - 50.0 * AGE_PER_DAY).abs() < 1e-9);
        assert_eq!(age_term(1000.0), AGE_CAP);

        let page = page(&OBJECTS[0]);
        for (name, _, w) in PRIORITY_TERMS {
            assert!(page.contains(&format!("<code>{name}</code> {w:.1}")));
        }
    }

    /// An object's fields are stacked rows, not a four-column table: in the
    /// half-width reference column a table let a long name push the
    /// description off the edge at every width.
    #[test]
    fn object_fields_render_as_stacked_rows() {
        for o in &OBJECTS {
            let fields = fields_html(o);
            assert!(!fields.contains("<table"), "`{}` fields are a table", o.id);
            assert_eq!(
                fields.matches(super::super::FIELD_ROW).count(),
                field_rows(o).len(),
                "`{}` renders a row per field",
                o.id
            );
        }
    }
}
