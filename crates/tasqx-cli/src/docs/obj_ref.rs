//! The Objects section: one page per thing the JSON API hands back (#648, D159).
//!
//! # Nothing on these pages is retyped
//!
//! | What | Where it comes from |
//! |---|---|
//! | every field row | the `tasqx_core::docs` groups [`OBJECTS`] names, the same rows the API page renders |
//! | the example object | a slice of one method's example on the API page — captured, or illustrative with its reason |
//! | the operations list | every method whose namespace is the object's, or whose response shape carries one of its groups ([`objects_of`]) |
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
    lead, note, p, page_close, page_open, pre_plain, ref_section, table_owned, term_screen,
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
    /// `(table caption, row-name prefix, group)`. An empty caption is the
    /// object's own table; a prefix names a nested object's rows
    /// (`urgency_breakdown.priority`).
    groups: &'static [(&'static str, &'static str, &'static [FieldDoc])],
    /// `(method, JSON pointer into its example's result, keys kept)`. No keys
    /// keeps the whole object.
    example: (&'static str, &'static str, &'static [&'static str]),
}

const ON_A_TASK: &str = "On a task";

/// The eight objects, in sidebar order.
pub(super) const OBJECTS: [Object; 8] = [
    Object {
        id: "obj-task",
        name: "Task",
        lead: "A task is one piece of work: a title, the dates and priority that score its \
               urgency, the time tracked against it, and everything hung off it — notes, \
               acceptance criteria, dependencies and token spend, each documented on its own page. \
               It is never deleted: a finished task is <code>done</code>, an abandoned one \
               <code>cancelled</code>, and both can be reopened.",
        namespaces: &["task", "tag", "reminder"],
        groups: &[
            ("", "", d::TASK_CORE),
            ("", "", d::TASK_STATUS_FLAG),
            ("", "", d::TASK_LIVE_TIME),
            ("", "", d::TASK_EXPORT_TIME),
            ("", "", d::TASK_BUDGET_GAUGE),
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
        groups: &[
            ("", "", d::PROJECT_LIST_ROW),
            ("", "", d::PROJECT_EXPORT_ROW),
        ],
        example: ("project.list", "/projects/0", &[]),
    },
    Object {
        id: "obj-annotation",
        name: "Annotation",
        lead: "An annotation is a note on a task, stored verbatim. Notes are added and removed, \
               never edited. Removing one scrubs its text and leaves a tombstone — the note's id \
               and when it went — under <code>annotations_removed</code>.",
        namespaces: &["annotation"],
        groups: &[
            ("", "", d::ANNOTATION_ROW),
            ("", "", d::ANNOTATION_CAP),
            ("", "annotations_removed[].", d::TOMBSTONE_ROW),
            (ON_A_TASK, "", d::TASK_ANNOTATIONS),
            (ON_A_TASK, "", d::TASK_EXPORT_ANNOTATIONS),
            (ON_A_TASK, "", d::TASK_ANNOTATIONS_REMOVED),
        ],
        example: ("task.get", "/annotations/0", &[]),
    },
    Object {
        id: "obj-check",
        name: "Check",
        lead: "A check is one acceptance criterion on a task (D138): a sentence tasqx stores and \
               never runs. It starts <code>open</code>, and <code>check.set</code> moves it to \
               <code>passed</code>, <code>failed</code> or back to <code>open</code>, with the \
               evidence that decided it.",
        namespaces: &["check"],
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
               title and a body, searchable together with every annotation. It can be scoped to a \
               project and marked standing (D156). Importing a doc with a <code>source</code> \
               already stored replaces that doc in place; removing one is permanent.",
        namespaces: &["memory"],
        groups: &[("", "", d::DOC_EXPORT_ROW), ("", "", d::MEMORY_LIST_ROW)],
        example: ("memory.get", "", &[]),
    },
    Object {
        id: "obj-event",
        name: "Event",
        lead: "An event is one row of the append-only audit log. Writes to tasks — their notes, \
               checks, dependencies and measurements included — to projects and to memory \
               documents append one. Events are never edited: <code>event.revert</code> undoes \
               the newest by appending its inverse, and refuses by name the ops it cannot \
               reverse.",
        namespaces: &["event"],
        groups: &[("", "", d::EVENT_ROW)],
        example: ("event.list", "/events/0", &[]),
    },
    Object {
        id: "obj-measurement",
        name: "Token measurement",
        lead: "A token measurement is AI token spend banked against a task: four buckets that are \
               never blended (D48), the tool and model that spent them, and how checkable the \
               figure is. A task's measurements are read under <code>tokens</code>.",
        namespaces: &["token"],
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

/// The objects a method acts on or answers with, in [`OBJECTS`] order.
///
/// The API page links a method section to exactly these pages, and each page
/// lists exactly the methods that name it here.
pub(super) fn objects_of(method: &str) -> Vec<&'static Object> {
    let shape = d::result_shape(method);
    OBJECTS
        .iter()
        .filter(|o| {
            o.namespaces.contains(&group_of(method))
                || shape
                    .iter()
                    .any(|(_, g)| o.groups.iter().any(|(_, _, og)| same_group(g, og)))
        })
        .collect()
}

/// A page's field rows: `(caption, row name, field)`, the first row of a
/// name winning.
///
/// A later group repeating a key describes the same field as another response
/// spells it — `store.export`'s `active_since` beside `task.get`'s — and the
/// API page carries that variant exactly; here the object gets one row.
pub(super) fn field_rows(o: &Object) -> Vec<(&'static str, String, &'static FieldDoc)> {
    let mut out: Vec<(&'static str, String, &'static FieldDoc)> = Vec::new();
    for (caption, prefix, group) in o.groups {
        for f in *group {
            let name = format!("{prefix}{}", f.key);
            if !out.iter().any(|(c, n, _)| c == caption && *n == name) {
                out.push((caption, name, f));
            }
        }
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
    for (c, ..) in &rows {
        if !captions.contains(c) {
            captions.push(c);
        }
    }
    let mut out = String::new();
    for caption in captions {
        if !caption.is_empty() {
            out.push_str(&format!("<div class=\"shapepath\">{}</div>", esc(caption)));
        }
        let cells: Vec<Vec<String>> = rows
            .iter()
            .filter(|(c, ..)| *c == caption)
            .map(|(_, name, f)| {
                vec![
                    format!("<code class=\"fname\">{}</code>", esc(name)),
                    format!("<span class=\"badge\">{}</span>", esc(f.ty)),
                    presence(f.null_ok, f.optional).to_string(),
                    describe(f.desc),
                ]
            })
            .collect();
        out.push_str(&table_owned(
            &["Field", "Type", "Presence", "Description"],
            &cells,
        ));
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

/// The status machine, as `engine/task.rs` enforces it. A raw string, so
/// the first line keeps its indentation (a `"\` continuation strips it).
const LIFECYCLE: &str = r"
                           task.add
                              │
    future wait or            │  otherwise
    scheduled    ┌────────────┴────────────┐
                 v                         v
            ┌─────────┐  dates pass   ┌─────────┐
            │ backlog │ ────────────> │ pending │
            │         │ <──────────── │         │
            └─────────┘ modify sets a └─────────┘
                 │       future date    │    ^
                 │           task.start │    │ task.stop
                 │                      v    │
                 │                    ┌─────────┐
                 │                    │ active  │
                 │                    └─────────┘
                 │                         │
 task.cancel     │ from backlog,           │ task.done from
                 │ pending, active         │ pending, active
                 v                         v
           ┌───────────┐              ┌─────────┐
           │ cancelled │              │  done   │
           └───────────┘              └─────────┘
                 │                         │
                 └─────────────────────────┴────> pending
                         task.reopen";

fn task_lifecycle() -> String {
    format!(
        "<h3 id=\"obj-task-lifecycle\">Lifecycle</h3>{}{}{}",
        p("A task has five statuses. Three are open — <code>backlog</code>, <code>pending</code> \
           and <code>active</code> — and two are closed: <code>done</code> and \
           <code>cancelled</code>. Each arrow is labelled with the method that makes the move; \
           the one between <code>backlog</code> and <code>pending</code> is also the clock."),
        pre_plain(LIFECYCLE.trim_start_matches('\n')),
        p("<code>backlog</code> and <code>pending</code> are the same question asked of the \
           clock every time a task is read: a task is in <code>backlog</code> while its \
           <code>wait</code> or <code>scheduled</code> instant is still in the future, and in \
           <code>pending</code> otherwise. There is no separate waiting status — a future \
           <code>wait</code> is what puts a task in <code>backlog</code>. A backlog task cannot \
           be started or completed until its date passes or is cleared. Starting a task without \
           <code>keep</code> stops the task already running, which goes back to \
           <code>pending</code> (D6) — or refuses, when that clock belongs to another named \
           session (D140). <code>task.modify</code> can set <code>status</code> only to \
           <code>cancelled</code>. <strong>Blocked</strong> is not a status either: it is the \
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
                            .map(|(o, prefix)| (o, format!("{prefix}{}", f.key)))
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
                                field_rows(o).iter().any(|(_, n, _)| n == name),
                                "`{method}` {path}.{}: `{}` lists its group but has no `{name}` row",
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
            .flat_map(|o| field_rows(o).into_iter().map(|(_, n, _)| n))
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
}
