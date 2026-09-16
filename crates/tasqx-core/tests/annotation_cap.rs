//! `max_body_bytes` on `task.get`/`task.brief`, and the tombstone list D113's
//! scrub leaves behind (D148).
//!
//! The engine half of the byte budget: the transport decides how many bytes a
//! response may cost, and the only lever it lacked was a bound on ONE body. A
//! task holding a single 240,000-byte annotation answered 240,000 bytes at
//! every page size, because the smallest page the bisection can reach is one
//! whole annotation.
//!
//! Absent the parameter nothing is cut and no key is added, which is D63's rule
//! one field over: `tasqx api` and the CLI keep reading the frozen v1 answer.

use serde_json::{json, Value};
use tasqx_core::Engine;

fn engine() -> Engine {
    Engine::open_in_memory().expect("in-memory store")
}

/// The page `task.get` answered, as rows.
fn rows(result: &Value) -> &Vec<Value> {
    result["annotations"].as_array().expect("annotations array")
}

/// A cut lands on a `char` boundary at or under the cap, never on the middle of
/// a codepoint.
///
/// Slicing a `String` by byte index panics inside a codepoint, so a naive
/// `&body[..cap]` turns a task with an accented word at the wrong offset into a
/// 500 on a READ — and the bodies this project stores are prose with em dashes
/// in them, so the offending byte is not exotic.
#[test]
fn a_capped_body_is_cut_on_a_char_boundary_and_says_what_it_cut() {
    let e = engine();
    e.task_add(&json!({ "title": "one enormous note" }))
        .expect("add");
    // Two bytes per character, so every ODD cap falls inside a codepoint.
    let body = "é".repeat(10);
    e.annotation_add(&json!({ "ref": 1, "body": body }))
        .expect("annotate");

    let got = e
        .task_get(&json!({ "ref": 1, "max_body_bytes": 7 }))
        .expect("task.get");
    let row = &rows(&got)[0];
    assert_eq!(
        row["body"], "ééé",
        "the longest prefix at or under the cap that is still valid UTF-8: {row}"
    );
    assert_eq!(
        row["body_bytes"],
        json!(20),
        "the marker needs the ORIGINAL size, which is the one the reader cannot \
         measure from what arrived: {row}"
    );
    assert_eq!(row["body_truncated"], json!(true));

    // A cap of zero is the degenerate end of the same rule, not a special case.
    let none = e
        .task_get(&json!({ "ref": 1, "max_body_bytes": 0 }))
        .expect("task.get");
    let row = &rows(&none)[0];
    assert_eq!(row["body"], "");
    assert_eq!(row["body_bytes"], json!(20));
    assert_eq!(row["body_truncated"], json!(true));
}

/// A body that fits is byte-identical to the one the same call answered before
/// this parameter existed: same `body`, and NEITHER new key.
///
/// The keys are additive and conditional on purpose. A `body_truncated: false`
/// on every row would put two new fields on every annotation of every response
/// — the thing the budget exists to stop — and would make "was anything cut?"
/// a value comparison rather than a presence one.
#[test]
fn a_body_under_the_cap_gains_no_keys() {
    let e = engine();
    e.task_add(&json!({ "title": "short notes" })).expect("add");
    e.annotation_add(&json!({ "ref": 1, "body": "well under it" }))
        .expect("annotate");

    let got = e
        .task_get(&json!({ "ref": 1, "max_body_bytes": 16_384 }))
        .expect("task.get");
    let row = &rows(&got)[0];
    assert_eq!(row["body"], "well under it");
    assert!(
        row.get("body_bytes").is_none() && row.get("body_truncated").is_none(),
        "a row that fits must read exactly as it always has: {row}"
    );
}

/// No parameter, no cap — the whole body, and no new keys. D63's reasoning
/// exactly: the bound belongs to the transport that has a payload limit, and
/// absent `annotations_limit` still means the whole history.
#[test]
fn without_the_parameter_the_body_is_whole() {
    let e = engine();
    e.task_add(&json!({ "title": "one enormous note" }))
        .expect("add");
    let body = "z".repeat(40_000);
    e.annotation_add(&json!({ "ref": 1, "body": body.clone() }))
        .expect("annotate");

    let got = e.task_get(&json!({ "ref": 1 })).expect("task.get");
    let row = &rows(&got)[0];
    assert_eq!(row["body"].as_str().expect("body").len(), 40_000);
    assert!(
        row.get("body_bytes").is_none() && row.get("body_truncated").is_none(),
        "the frozen v1 answer is untouched by a parameter nobody passed: {row}"
    );
}

/// D113's tombstone becomes visible. The row stays after `annotation.remove`
/// scrubs its text; until now the only trace in `task.get` was a hole in the
/// count, so a reader comparing two reads of the same task saw an annotation
/// vanish with nothing saying it had been removed on purpose.
///
/// It is its OWN array, never a row in `annotations`: `annotations` and
/// `annotations_total` keep excluding removed rows, so every client's page
/// arithmetic reads what it always read.
#[test]
fn a_removed_annotation_leaves_a_tombstone_beside_the_page() {
    let e = engine();
    e.task_add(&json!({ "title": "rotate the leaked key" }))
        .expect("add");
    let added = e
        .annotation_add(&json!({ "ref": 1, "body": "sk-super-secret-token" }))
        .expect("annotate");
    let id = added["annotation"]["id"].as_str().expect("id").to_string();
    e.annotation_remove(&json!({ "ref": 1, "annotation_id": id.clone() }))
        .expect("annotation.remove");

    let got = e.task_get(&json!({ "ref": 1 })).expect("task.get");
    assert_eq!(got["annotations"], json!([]), "the page still excludes it");
    assert_eq!(got["annotations_total"], json!(0), "and so does the total");

    let stones = got["annotations_removed"]
        .as_array()
        .expect("annotations_removed is always an array");
    assert_eq!(stones.len(), 1, "one removal, one tombstone: {got}");
    assert_eq!(stones[0]["id"], json!(id));
    assert!(
        stones[0]["removed"]
            .as_str()
            .is_some_and(|t| t.parse::<jiff::Timestamp>().is_ok()),
        "the tombstone carries WHEN, as an instant: {stones:?}"
    );
    assert!(
        stones[0].get("body").is_none(),
        "and never the text it scrubbed: {stones:?}"
    );
}

/// The array is present with nothing removed, for the reason every other
/// always-present key here is: a field that appears only in the unusual case
/// makes a client branch on presence to ask an ordinary question.
#[test]
fn the_tombstone_list_is_present_and_empty_when_nothing_was_removed() {
    let e = engine();
    e.task_add(&json!({ "title": "nothing removed" }))
        .expect("add");
    let got = e.task_get(&json!({ "ref": 1 })).expect("task.get");
    assert_eq!(got["annotations_removed"], json!([]));
}

/// `task.brief` takes the same cap and applies it to its task half.
///
/// The brief is `task.get`'s result plus two more sets of reads (D136), so a
/// brief on a task with one enormous annotation was the same oversized answer
/// with a neighbourhood stapled to it. The param has to REACH the shared
/// detail body, which builds its own params object.
#[test]
fn the_brief_caps_its_task_half_too() {
    let e = engine();
    e.task_add(&json!({ "title": "briefed" })).expect("add");
    e.annotation_add(&json!({ "ref": 1, "body": "y".repeat(5_000) }))
        .expect("annotate");

    let brief = e
        .task_brief(&json!({ "ref": 1, "max_body_bytes": 100 }))
        .expect("task.brief");
    let row = &brief["task"]["annotations"][0];
    assert_eq!(row["body"].as_str().expect("body").len(), 100);
    assert_eq!(row["body_bytes"], json!(5_000));
    assert_eq!(row["body_truncated"], json!(true));

    let whole = e.task_brief(&json!({ "ref": 1 })).expect("task.brief");
    assert_eq!(
        whole["task"]["annotations"][0]["body"]
            .as_str()
            .expect("body")
            .len(),
        5_000,
        "and without the param the brief is as whole as it ever was"
    );
}

/// A negative cap is refused at the edge rather than cast into a huge one —
/// the rule `annotations_limit` already follows.
#[test]
fn a_negative_cap_is_refused() {
    let e = engine();
    e.task_add(&json!({ "title": "x" })).expect("add");
    let err = e
        .task_get(&json!({ "ref": 1, "max_body_bytes": -1 }))
        .unwrap_err();
    assert_eq!(err.code, tasqx_core::ErrorCode::BadRequest);
}
