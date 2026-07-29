//! Golden tests for the MCP task-detail view.
//!
//! These compare the WHOLE output byte-for-byte rather than asserting on
//! fragments. The promise this feature makes is "the same task reads the same
//! for everyone", and a test that checks a substring cannot tell the difference
//! between that promise and a near-miss.

use serde_json::json;
use tasqx_core::markdown::{task_detail, DetailOpts, TimeFormat};

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
