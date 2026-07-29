//! Golden tests for the MCP task-detail view.
//!
//! These compare the WHOLE output byte-for-byte rather than asserting on
//! fragments. The promise this feature makes is "the same task reads the same
//! for everyone", and a test that checks a substring cannot tell the difference
//! between that promise and a near-miss.

use serde_json::{json, Value};
use tasqx_core::markdown::{task_detail, DetailOpts, TimeFormat};
use tasqx_core::{dispatch, Engine, McpServer, Scope};

/// A fixed instant so relative formatting is testable. Nothing here calls the
/// clock: the renderer takes `now` as a parameter precisely so this is possible.
fn at(iso: &str) -> jiff::Timestamp {
    iso.parse().expect("fixture timestamp parses")
}

fn iso_opts() -> DetailOpts {
    DetailOpts {
        time: TimeFormat::Iso,
        now: at("2026-07-29T11:00:00Z"),
    }
}

#[test]
fn a_minimal_task_renders_its_heading_and_core_rows() {
    let task = json!({
        "short_id": 76,
        "title": "Three field-test papercuts",
        "status": "pending",
        "priority": "L",
        "urgency": 1.8,
        "project": "tasqx-field-test-2026-07",
        "created": "2026-07-29T09:00:58Z",
        "modified": "2026-07-29T09:01:45Z",
        "_rev": 2
    });

    let expected = "\
## #76 · Three field-test papercuts

| | |
|---|---|
| status | pending |
| priority | L (urgency 1.8) |
| project | tasqx-field-test-2026-07 |
| created | 2026-07-29T09:00:58Z |
| modified | 2026-07-29T09:01:45Z |
| rev | 2 |
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}

#[test]
fn an_unrecognized_status_is_named_as_such_with_the_valid_set() {
    let task = json!({
        "short_id": 5,
        "title": "From a newer build",
        "status": "snoozed",
        "status_unrecognized": true,
        "priority": null,
        "urgency": 0.0,
        "project": null,
        "created": "2026-07-29T09:00:00Z",
        "modified": "2026-07-29T09:00:00Z",
        "_rev": 1
    });

    let out = task_detail(&task, &iso_opts());
    assert!(
        out.contains(
            "| status | snoozed (unrecognized — not one of backlog, pending, active, done, cancelled) |"
        ),
        "got:\n{out}"
    );
}

#[test]
fn optional_rows_appear_only_when_they_hold_a_value() {
    let task = json!({
        "short_id": 73,
        "title": "A present-but-incomplete transcript is stored as a terminal zero",
        "status": "pending",
        "priority": "H",
        "urgency": 6.0,
        "project": "tasqx-field-test-2026-07",
        "tags": ["attribution", "field-test", "tokens"],
        "estimate": "PT3H",
        "tracked": "PT0S",
        "due": null,
        "scheduled": null,
        "wait": null,
        "remind": null,
        "recurrence": null,
        "active_since": null,
        "completed": null,
        "blocked": false,
        "depends_on": [],
        "created": "2026-07-29T09:00:52Z",
        "modified": "2026-07-29T09:35:30Z",
        "_rev": 4
    });

    let expected = "\
## #73 · A present-but-incomplete transcript is stored as a terminal zero

| | |
|---|---|
| status | pending |
| priority | H (urgency 6) |
| project | tasqx-field-test-2026-07 |
| tags | attribution, field-test, tokens |
| estimate | PT3H |
| tracked | PT0S |
| created | 2026-07-29T09:00:52Z |
| modified | 2026-07-29T09:35:30Z |
| rev | 4 |
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}

#[test]
fn a_blocked_task_with_dates_and_dependencies_shows_them() {
    let task = json!({
        "short_id": 9,
        "title": "VH-STD-001 ratificatie",
        "status": "pending",
        "priority": "H",
        "urgency": 18.0,
        "project": "qore-architecture",
        "tags": ["pr-55"],
        "due": "2026-07-28T00:00:00Z",
        "blocked": true,
        "depends_on": [72, 74],
        "created": "2026-07-24T09:51:24Z",
        "modified": "2026-07-24T09:51:49Z",
        "_rev": 2
    });

    let expected = "\
## #9 · VH-STD-001 ratificatie

| | |
|---|---|
| status | pending |
| priority | H (urgency 18) |
| project | qore-architecture |
| tags | pr-55 |
| due | 2026-07-28T00:00:00Z |
| blocked | yes |
| depends on | #72, #74 |
| created | 2026-07-24T09:51:24Z |
| modified | 2026-07-24T09:51:49Z |
| rev | 2 |
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}

#[test]
fn annotation_bodies_survive_verbatim_including_their_own_markdown() {
    let task = json!({
        "short_id": 76,
        "title": "Papercuts",
        "status": "pending",
        "priority": "L",
        "urgency": 1.8,
        "project": "tasqx-field-test-2026-07",
        "created": "2026-07-29T09:00:58Z",
        "modified": "2026-07-29T09:01:45Z",
        "_rev": 2,
        "annotations": [
            { "id": "a1", "created": "2026-07-29T09:01:45Z",
              "body": "## Problem\n\nA list:\n\n- one\n- two\n" }
        ]
    });

    let expected = "\
## #76 · Papercuts

| | |
|---|---|
| status | pending |
| priority | L (urgency 1.8) |
| project | tasqx-field-test-2026-07 |
| created | 2026-07-29T09:00:58Z |
| modified | 2026-07-29T09:01:45Z |
| rev | 2 |

### Annotations (1)

---
**2026-07-29T09:01:45Z**

## Problem

A list:

- one
- two
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}

#[test]
fn measurements_render_as_their_own_table() {
    let task = json!({
        "short_id": 72,
        "title": "Correlation flags",
        "status": "done",
        "priority": "H",
        "urgency": 6.0,
        "project": "tasqx-field-test-2026-07",
        "created": "2026-07-29T09:00:45Z",
        "modified": "2026-07-29T09:35:30Z",
        "_rev": 6,
        "tokens": [
            { "input_tokens": 1200, "output_tokens": 340,
              "cache_read_tokens": 8000, "cache_creation_tokens": 150,
              "source": "log-parse", "confidence": "high",
              "tool": "claude-code 2.1", "model": null,
              "created": "2026-07-29T09:34:56Z" }
        ]
    });

    let out = task_detail(&task, &iso_opts());
    assert!(out.contains("### Tokens (1)"), "got:\n{out}");
    assert!(
        out.contains("| claude-code 2.1 | 1200 | 340 | 8000 | 150 | log-parse | high |"),
        "got:\n{out}"
    );
}

#[test]
fn a_task_without_annotations_or_tokens_emits_neither_section() {
    let task = json!({
        "short_id": 1, "title": "Bare", "status": "pending",
        "priority": null, "urgency": 0.0, "project": null,
        "created": "2026-07-29T09:00:00Z", "modified": "2026-07-29T09:00:00Z",
        "_rev": 1, "annotations": [], "tokens": []
    });
    let out = task_detail(&task, &iso_opts());
    assert!(!out.contains("Annotations"), "got:\n{out}");
    assert!(!out.contains("Tokens"), "got:\n{out}");
}

fn opts(time: TimeFormat) -> DetailOpts {
    DetailOpts {
        time,
        now: at("2026-07-29T11:00:00Z"),
    }
}

#[test]
fn relative_formatting_replaces_the_instant_and_the_duration() {
    let task = json!({
        "short_id": 76, "title": "Papercuts", "status": "pending",
        "priority": "L", "urgency": 1.8, "project": "p",
        "estimate": "PT2H",
        "created": "2026-07-29T09:00:58Z", "modified": "2026-07-29T09:00:58Z",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Relative));
    assert!(out.contains("| created | 2 hours ago |"), "got:\n{out}");
    assert!(out.contains("| estimate | 2h |"), "got:\n{out}");
    assert!(!out.contains("2026-07-29T09:00:58Z"), "got:\n{out}");
}

#[test]
fn both_shows_the_exact_value_with_the_readable_one_in_parentheses() {
    let task = json!({
        "short_id": 76, "title": "Papercuts", "status": "pending",
        "priority": "L", "urgency": 1.8, "project": "p",
        "estimate": "PT2H",
        "created": "2026-07-29T09:00:58Z", "modified": "2026-07-29T09:00:58Z",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Both));
    assert!(
        out.contains("| created | 2026-07-29T09:00:58Z (2 hours ago) |"),
        "got:\n{out}"
    );
    assert!(out.contains("| estimate | PT2H (2h) |"), "got:\n{out}");
}

#[test]
fn a_future_instant_reads_as_in_rather_than_ago() {
    let task = json!({
        "short_id": 9, "title": "Due soon", "status": "pending",
        "priority": "H", "urgency": 18.0, "project": "p",
        "due": "2026-07-30T11:00:00Z",
        "created": "2026-07-29T11:00:00Z", "modified": "2026-07-29T11:00:00Z",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Relative));
    assert!(out.contains("| due | in 1 day |"), "got:\n{out}");
}

/// Build a task whose only interesting field is `estimate`.
fn with_estimate(estimate: &str) -> Value {
    json!({
        "short_id": 1, "title": "Estimated", "status": "pending",
        "priority": null, "urgency": 0.0, "project": null,
        "estimate": estimate,
        "created": "2026-07-29T09:00:00Z", "modified": "2026-07-29T09:00:00Z",
        "_rev": 1
    })
}

/// The view must read every duration the STORE accepts, not a subset of it.
///
/// `datetime::parse_duration` validates an ISO estimate with
/// `util::duration_secs` and stores it verbatim, so `estimate:P2W` reaches the
/// renderer as the literal `P2W`. A second, narrower reader that knew only
/// D/H/M/S answered `None` for it, and `TimeFormat::Relative` — which promises
/// the readable form INSTEAD of the ISO one — silently handed back the raw ISO
/// string it was asked to replace. The two readers are now one.
#[test]
fn a_calendar_unit_estimate_humanizes_instead_of_leaking_its_iso_string() {
    for (estimate, human) in [
        ("P2W", "14d"),
        ("P1Y", "365d"),
        ("P1M", "30d"),
        ("P1WT12H", "8d"),
    ] {
        let out = task_detail(&with_estimate(estimate), &opts(TimeFormat::Relative));
        assert!(
            out.contains(&format!("| estimate | {human} |")),
            "{estimate} must render as {human}, got:\n{out}"
        );
        assert!(
            !out.contains(estimate),
            "{estimate} must not survive into a relative view, got:\n{out}"
        );
    }
}

/// Presentation may not fail (DESIGN.md), and `task_detail` is public API — so
/// "no store writes a duration this big" is not a defence. It reached the
/// renderer before this test existed, through a reader whose `* 86_400` and
/// `+=` were unchecked: this panicked in debug and printed a wrapped, negative
/// total in release. `util::duration_secs` is checked throughout and answers
/// `None`, which lands in the raw-string fallback.
#[test]
fn an_overflowing_duration_falls_back_rather_than_panicking_or_wrapping() {
    for absurd in [
        "PT999999999999999999H",
        "P999999999999999999D",
        "P9223372036854775807Y",
        "P9223372036854775807W",
    ] {
        for time in [TimeFormat::Iso, TimeFormat::Relative, TimeFormat::Both] {
            let out = task_detail(&with_estimate(absurd), &opts(time));
            assert!(
                out.contains(&format!("| estimate | {absurd} |")),
                "{absurd} must fall back to itself, got:\n{out}"
            );
            assert!(
                !out.contains("| estimate | -") && !out.contains(" (-"),
                "a wrapped negative total must never reach the view, got:\n{out}"
            );
        }
    }
}

#[test]
fn an_unparseable_instant_falls_back_to_the_raw_value_rather_than_panicking() {
    let task = json!({
        "short_id": 1, "title": "Broken", "status": "pending",
        "priority": null, "urgency": 0.0, "project": null,
        "created": "not-a-timestamp", "modified": "not-a-timestamp",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Relative));
    assert!(out.contains("| created | not-a-timestamp |"), "got:\n{out}");
}

// ---- the view on the wire ----------------------------------------------------

/// Drive one `tools/call` against a scratch engine and return its result.
///
/// The entrypoint is `handle_message`, which returns `Option<Value>` — `None`
/// for a notification, which a `tools/call` never is. `crates/tasqx-core/tests/mcp.rs`
/// drives the server the same way; this mirrors it rather than inventing a
/// second style.
fn call_tool(server: &McpServer, name: &str, args: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    server
        .handle_message(&req)
        .expect("tools/call always answers")
        .get("result")
        .cloned()
        .expect("a successful call carries result")
}

#[test]
fn get_task_returns_the_rendered_view_first_and_the_json_second() {
    let engine = Engine::open_in_memory().expect("engine");
    dispatch(&engine, "task.add", &json!({ "title": "measure me" })).expect("add");
    let server = McpServer::new(&engine, Scope::Read);

    let result = call_tool(&server, "tasqx_get_task", json!({ "ref": 1 }));
    let content = result["content"].as_array().expect("content array");

    assert_eq!(content.len(), 2, "got: {content:#?}");
    let view = content[0]["text"].as_str().expect("markdown block");
    assert!(view.starts_with("## #1 · measure me"), "got:\n{view}");
    let raw = content[1]["text"].as_str().expect("json block");
    assert!(raw.trim_start().starts_with('{'), "got:\n{raw}");
}

#[test]
fn every_other_tool_still_returns_exactly_one_block() {
    let engine = Engine::open_in_memory().expect("engine");
    let server = McpServer::new(&engine, Scope::Read);
    let result = call_tool(&server, "tasqx_list_tasks", json!({}));
    assert_eq!(result["content"].as_array().expect("content").len(), 1);
}

// ---- the drift guard ---------------------------------------------------------

/// Keys `task_detail` deliberately does not render, each with its reason.
/// Adding to this list is a decision; leaving a key off it and out of the view
/// is the accident this test exists to prevent.
const OMITTED: &[(&str, &str)] = &[("id", "the UUID; short_id is the handle users type")];

/// Keys rendered under a different label or folded into another row. This is a
/// declared key-to-row mapping, NOT a substring search for the key name: `_rev`
/// renders as `rev` and `urgency` sits inside the priority cell, so a text
/// search would both miss those and match by accident on values.
///
/// The needles carry content, not just structure. `short_id` and `title` share
/// one heading, and a needle of `## #` would be satisfied by a heading that had
/// lost the title — a guard that passes on the fault it exists to catch. Both
/// therefore point at the fixture's own values below.
const RENDERED_AS: &[(&str, &str)] = &[
    ("_rev", "| rev |"),
    ("urgency", "(urgency "),
    ("status_unrecognized", "| status |"),
    ("short_id", "## #2 ·"),
    ("title", "· fully populated"),
    ("depends_on", "| depends on |"),
    ("active_since", "| active since |"),
    ("annotations", "### Annotations"),
    ("tokens", "### Tokens"),
];

#[test]
fn every_field_task_get_returns_is_accounted_for_in_the_view() {
    // A task with EVERY field populated, so no key can be missing merely
    // because this fixture left it null.
    //
    // `active_since` and `completed` cannot both hold a value on one task —
    // starting sets the first, completing clears it and sets the second — so
    // the task is read TWICE, once running and once finished, and a key counts
    // as accounted for when either view shows it. Rendering one snapshot would
    // otherwise leave whichever field the snapshot cannot hold permanently
    // unchecked.
    let engine = Engine::open_in_memory().expect("engine");
    let d = |m: &str, p: &Value| dispatch(&engine, m, p).expect("dispatch");
    d("project.create", &json!({ "name": "p" }));
    d("task.add", &json!({ "title": "blocker" }));
    d(
        "task.add",
        &json!({
            "title": "fully populated",
            "project": "p", "priority": "H", "tags": ["a", "b"],
            "estimate": "PT3H", "due": "2030-01-01T00:00:00Z",
            // Both in the past: a future `wait`/`scheduled` would land the task
            // in `backlog`, which cannot be started, and this fixture needs an
            // `active_since`.
            "scheduled": "2020-01-02T00:00:00Z", "wait": "2020-01-01T00:00:00Z",
            "remind": "-1h", "recurrence": "weekly on mon"
        }),
    );
    d("dependency.add", &json!({ "ref": 2, "depends_on": 1 }));
    d("annotation.add", &json!({ "ref": 2, "body": "note" }));
    d(
        "token.add",
        &json!({
            "ref": 2, "tool": "claude-code", "source": "self-report",
            "confidence": "medium", "input_tokens": 12
        }),
    );

    d("task.start", &json!({ "ref": 2 }));
    let running = d("task.get", &json!({ "ref": 2 }));
    let running_view = task_detail(&running, &iso_opts());
    d("task.done", &json!({ "ref": 2 }));
    let finished = d("task.get", &json!({ "ref": 2 }));
    let finished_view = task_detail(&finished, &iso_opts());

    let mut keys: Vec<String> = Vec::new();
    for task in [&running, &finished] {
        for key in task.as_object().expect("task.get returns an object").keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
    }

    let mut unaccounted: Vec<&str> = Vec::new();
    for key in &keys {
        if OMITTED.iter().any(|(k, _)| k == key) {
            continue;
        }
        let needle = RENDERED_AS
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, n)| (*n).to_string())
            .unwrap_or_else(|| format!("| {} |", key.replace('_', " ")));
        if !running_view.contains(&needle) && !finished_view.contains(&needle) {
            unaccounted.push(key);
        }
    }

    assert!(
        unaccounted.is_empty(),
        "fields task.get returns that the detail view neither renders nor \
         declares as omitted: {unaccounted:?}\n\nAdd each to the view, to \
         RENDERED_AS with the row it lands in, or to OMITTED with a reason.\n\n\
         running view was:\n{running_view}\nfinished view was:\n{finished_view}"
    );
}

#[test]
fn a_malformed_value_yields_a_thin_view_rather_than_a_panic() {
    for bad in [
        json!({}),
        json!(null),
        json!([1, 2, 3]),
        json!({ "short_id": "not-a-number", "title": 42, "tags": "not-an-array" }),
    ] {
        let out = task_detail(&bad, &iso_opts());
        assert!(
            !out.is_empty(),
            "renderer must never return an empty string for {bad:?}"
        );
        assert!(out.starts_with("## #"), "got:\n{out}");
    }
}
