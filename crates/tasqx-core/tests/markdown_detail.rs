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

/// The case the test above does NOT reach, and the one that actually shipped
/// broken.
///
/// All four inputs there are ones `util::duration_secs` refuses, so they never
/// touch the arithmetic. A bare seconds count does: `PT9223372036854775807S`
/// parses to `Some(i64::MAX)` — the store's own validator accepts it — and then
/// `round_div`'s old `(secs + unit / 2) / unit` overflowed. Debug builds
/// panicked, so `tasqx_get_task` answered `{"error":{"code":-32603}}` instead of
/// the task; the release profile ships without `overflow-checks`, so it wrapped
/// silently and rendered `| estimate | -106751991167300d |`.
///
/// The silent wrap is the worse half: a panic at least announces itself, and a
/// view that degrades to an error envelope is exactly what D49 forbids by making
/// it worse than the plain JSON it replaced.
#[test]
fn a_duration_at_the_integer_ceiling_neither_panics_nor_wraps() {
    // Not a hypothetical: `tasqx add x --estimate PT9223372036854775807S` is
    // accepted today, and `store.import` reaches the same value through
    // `tracked_seconds`, which no user ever types.
    assert_eq!(
        tasqx_core::util::duration_secs("PT9223372036854775807S"),
        Some(i64::MAX),
        "this test is only meaningful while the shared reader ACCEPTS this input"
    );

    for time in [TimeFormat::Iso, TimeFormat::Relative, TimeFormat::Both] {
        let out = task_detail(&with_estimate("PT9223372036854775807S"), &opts(time));
        assert!(
            !out.contains("| estimate | -") && !out.contains(" (-"),
            "a wrapped negative total must never reach the view, got:\n{out}"
        );
        assert!(
            out.contains("| estimate |"),
            "the row must still render, got:\n{out}"
        );
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

/// How one key proves it reached the view.
///
/// There is no fallback arm and no generated needle: a key absent from
/// [`RENDERED_AS`] and from [`OMITTED`] fails the guard by name. A fallback that
/// guessed `| {key} |` would have been the substring search this mapping exists
/// to replace — it matches the row's LABEL, which `row()` writes from a literal,
/// so it stays true after the VALUE stops arriving.
enum Shows {
    /// The key renders as its own `| label | value |` row. The needle is built
    /// from the declared label and the task's OWN value, so a row that kept its
    /// label and lost its cell fails. The value is read per snapshot: a key that
    /// is null everywhere yields no needle at all and is reported unaccounted,
    /// which is the right answer — the fixture must populate it or declare it.
    Row(&'static str),
    /// The key does not render as one labelled row — it is folded into another
    /// cell, shares a heading, or lands in a section table. The literal needle
    /// carries the fixture's value wherever the shape allows.
    Cell(&'static str),
}

/// Every key `task.get` returns, and what proves it reached the view.
///
/// This is a declared key-to-row mapping, NOT a substring search for the key
/// name: `_rev` renders as `rev`, `urgency` sits inside the priority cell and
/// `status_unrecognized` turns into a suffix on the status cell, so a text
/// search would miss all three and match by accident on values.
///
/// The needles carry content, not just structure. `short_id` and `title` share
/// one heading, and a needle of `## #` would be satisfied by a heading that had
/// lost the title — a guard that passes on the fault it exists to catch. Every
/// entry here therefore points at a fixture value: literally for [`Shows::Cell`],
/// and through the task's own JSON for [`Shows::Row`].
const RENDERED_AS: &[(&str, Shows)] = &[
    ("short_id", Shows::Cell("## #2 ·")),
    ("title", Shows::Cell("· fully populated")),
    ("status", Shows::Row("status")),
    // The flag is a boolean, and the only trace it leaves is this suffix. A
    // needle of `| status |` would be satisfied by the always-present status
    // row, which is how rule 4 could be deleted outright with this guard still
    // green.
    (
        "status_unrecognized",
        Shows::Cell("| status | Done (unrecognized — "),
    ),
    ("priority", Shows::Cell("| priority | H (urgency 6) |")),
    ("urgency", Shows::Cell("(urgency 6)")),
    ("project", Shows::Row("project")),
    ("tags", Shows::Cell("| tags | a, b |")),
    ("estimate", Shows::Row("estimate")),
    ("tracked", Shows::Row("tracked")),
    ("due", Shows::Row("due")),
    ("scheduled", Shows::Row("scheduled")),
    ("wait", Shows::Row("wait")),
    ("remind", Shows::Row("remind")),
    ("recurrence", Shows::Row("recurrence")),
    ("active_since", Shows::Row("active since")),
    ("completed", Shows::Row("completed")),
    ("blocked", Shows::Cell("| blocked | yes |")),
    ("depends_on", Shows::Cell("| depends on | #1 |")),
    ("created", Shows::Row("created")),
    ("modified", Shows::Row("modified")),
    ("_rev", Shows::Row("rev")),
    // The count is what this view derives from the array. That the BODIES land
    // verbatim is pinned by
    // `annotation_bodies_survive_verbatim_including_their_own_markdown`, which
    // compares the whole output.
    ("annotations", Shows::Cell("### Annotations (1)")),
    (
        "tokens",
        Shows::Cell("| claude-code | 12 | 0 | 0 | 0 | self-report | medium |"),
    ),
];

/// The scalar a `Shows::Row` needle interpolates, or `None` when the snapshot
/// does not carry one. Strings and numbers only: those are the JSON shapes the
/// view writes into a cell unchanged.
fn cell_value(task: &Value, key: &str) -> Option<String> {
    match task.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[test]
fn every_field_task_get_returns_is_accounted_for_in_the_view() {
    // A task with EVERY field populated, so no key can be missing merely
    // because this fixture left it null.
    //
    // `active_since` and `completed` cannot both hold a value on one task —
    // starting sets the first, completing clears it and sets the second — so
    // the task is read MORE THAN ONCE, and a key counts as accounted for when
    // any snapshot shows it. Rendering one snapshot would otherwise leave
    // whichever field the snapshot cannot hold permanently unchecked.
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

    // `pending` and `backlog` are read too, and that is not padding: an
    // adversarial review found that snapshotting only active/done/anomalous let a
    // key emitted ONLY for a pending task slip past this guard green — and
    // `pending` is the status every task tasqx creates starts in. The probe that
    // proved it (`if status is pending { v["probe"] = ... }` in
    // `flag_unrecognized_status`) passed before this snapshot existed and fails
    // now. `backlog` needs its own task: it is what a future `wait` produces, and
    // a task in it cannot be started, so task 2 can never visit both.
    let pending = d("task.get", &json!({ "ref": 2 }));
    d(
        "task.add",
        &json!({ "title": "waiting", "wait": "2099-01-01T00:00:00Z" }),
    );
    let waiting = d("task.get", &json!({ "ref": 3 }));

    d("task.start", &json!({ "ref": 2 }));
    let running = d("task.get", &json!({ "ref": 2 }));
    d("task.done", &json!({ "ref": 2 }));
    let finished = d("task.get", &json!({ "ref": 2 }));
    // `status_unrecognized` is emitted for a status no writer of THIS build
    // could have produced (D28), so a fixture that only drives the state
    // machine can never carry it — and a mapping that can never fire is a
    // mapping nobody is checking. A store written by a newer build is what
    // produces it in the field; `crates/tasqx-core/tests/increment.rs` reaches
    // that state the same way, by writing the status through the connection
    // rather than through the API that refuses it.
    engine
        .conn()
        .execute("UPDATE tasks SET status = 'Done' WHERE short_id = 2", [])
        .expect("a store from a newer build");
    let anomalous = d("task.get", &json!({ "ref": 2 }));

    let snapshots: Vec<(Value, String)> = [pending, waiting, running, finished, anomalous]
        .into_iter()
        .map(|task| {
            let view = task_detail(&task, &iso_opts());
            (task, view)
        })
        .collect();

    let mut keys: Vec<String> = Vec::new();
    for (task, _) in &snapshots {
        for key in task.as_object().expect("task.get returns an object").keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
    }

    let mut unaccounted: Vec<(&str, String)> = Vec::new();
    for key in &keys {
        if OMITTED.iter().any(|(k, _)| k == key) {
            continue;
        }
        let Some((_, shows)) = RENDERED_AS.iter().find(|(k, _)| k == key) else {
            unaccounted.push((key, "not in RENDERED_AS or OMITTED".to_string()));
            continue;
        };
        // A needle per snapshot, checked against THAT snapshot's view: a `Row`
        // needle carries the value the very task being rendered holds, so it
        // cannot be satisfied by a different snapshot's cell.
        let mut needles: Vec<String> = Vec::new();
        let mut found = false;
        for (task, view) in &snapshots {
            let needle = match shows {
                Shows::Cell(literal) => (*literal).to_string(),
                Shows::Row(label) => match cell_value(task, key) {
                    Some(value) => format!("| {label} | {value} |"),
                    None => continue,
                },
            };
            found |= view.contains(&needle);
            if !needles.contains(&needle) {
                needles.push(needle);
            }
        }
        if !found {
            let tried = if needles.is_empty() {
                "no snapshot carried a value to look for".to_string()
            } else {
                format!("looked for {needles:?}")
            };
            unaccounted.push((key, tried));
        }
    }

    let views: Vec<&str> = snapshots.iter().map(|(_, v)| v.as_str()).collect();
    assert!(
        unaccounted.is_empty(),
        "fields task.get returns whose VALUE the detail view neither renders \
         nor declares as omitted: {unaccounted:#?}\n\nAdd each to the view, to \
         RENDERED_AS with the row it lands in, or to OMITTED with a reason.\n\n\
         views were:\n{}",
        views.join("\n----\n")
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
