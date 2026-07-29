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
