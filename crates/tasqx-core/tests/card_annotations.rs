//! The card's Description and Delivered rows, and `annotation.update` (D165).
//!
//! Findings #619 and #630: the card read Description from the oldest note ON
//! THE PAGE and Delivered from the newest, so `annotations_limit: 0` lost both
//! rows and a note written after completion replaced the delivery paragraph —
//! and a wrong description could only be corrected by deleting it, which made
//! the next-oldest note the Description.

use serde_json::{json, Value};
use tasqx_core::markdown::{task_card, Borders, CardOpts, DetailOpts, TimeFormat};
use tasqx_core::{dispatch, Engine};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Value {
    dispatch(e, method, &params).unwrap_or_else(|err| panic!("{method}: {err:?}"))
}

fn note(e: &Engine, r: i64, body: &str) -> String {
    call(e, "annotation.add", json!({ "ref": r, "body": body }))["annotation"]["id"]
        .as_str()
        .expect("annotation id")
        .to_string()
}

fn card(e: &Engine, get: Value) -> String {
    let opts = CardOpts {
        detail: DetailOpts {
            time: TimeFormat::Iso,
            now: "2026-09-19T12:00:00Z".parse().unwrap(),
        },
        borders: Borders::Unicode,
    };
    task_card(&call(e, "task.get", get), &opts)
}

/// A done task with a description note, a progress note and a delivery note.
fn delivered_task(e: &Engine) -> i64 {
    call(e, "task.add", json!({ "title": "ship the card" }));
    note(e, 1, "Descriptionpara what the work is.");
    note(e, 1, "Progress note in the middle.");
    note(e, 1, "Deliverypara what shipped.");
    call(e, "task.done", json!({ "ref": 1 }));
    1
}

#[test]
fn a_card_read_with_no_annotation_page_still_shows_description_and_delivered() {
    let e = engine();
    let r = delivered_task(&e);
    let c = card(&e, json!({ "ref": r, "annotations_limit": 0 }));
    assert!(c.contains("Descriptionpara"), "Description row lost:\n{c}");
    assert!(c.contains("Deliverypara"), "Delivered row lost:\n{c}");
    assert!(!c.contains("not on this page"), "{c}");

    // The middle page holds neither end, and both rows still come through.
    let c = card(
        &e,
        json!({ "ref": r, "annotations_limit": 1, "annotations_offset": 1 }),
    );
    assert!(c.contains("Descriptionpara"), "{c}");
    assert!(c.contains("Deliverypara"), "{c}");
}

#[test]
fn a_note_added_after_completion_leaves_delivered_where_it_was() {
    let e = engine();
    let r = delivered_task(&e);
    let pinned = call(&e, "task.get", json!({ "ref": r }))["delivered_annotation_id"].clone();
    assert!(pinned.is_string(), "completion pins the delivery note");
    note(&e, r, "Bookkeeping remark written later.");
    let c = card(&e, json!({ "ref": r }));
    assert!(c.contains("Deliverypara"), "{c}");
    assert!(
        !c.contains("Bookkeeping"),
        "a later note displaced Delivered:\n{c}"
    );
}

#[test]
fn reopening_clears_the_pin_and_completing_again_pins_afresh() {
    let e = engine();
    let r = delivered_task(&e);
    call(&e, "task.reopen", json!({ "ref": r }));
    let got = call(&e, "task.get", json!({ "ref": r }));
    assert!(got["delivered_annotation_id"].is_null());
    assert!(got["delivered_annotation"].is_null());
    let second = note(&e, r, "Second delivery.");
    call(&e, "task.done", json!({ "ref": r }));
    let got = call(&e, "task.get", json!({ "ref": r }));
    assert_eq!(got["delivered_annotation_id"], json!(second));
}

#[test]
fn a_task_completed_before_the_pin_existed_falls_back_to_the_newest_note() {
    let e = engine();
    let r = delivered_task(&e);
    e.conn()
        .execute("UPDATE tasks SET delivered_annotation_id = NULL", [])
        .unwrap();
    note(&e, r, "Newest after all.");
    let c = card(&e, json!({ "ref": r, "annotations_limit": 0 }));
    assert!(c.contains("Newest after all"), "{c}");
}

#[test]
fn export_and_import_carry_the_pin() {
    let e = engine();
    let r = delivered_task(&e);
    let pinned = call(&e, "task.get", json!({ "ref": r }))["delivered_annotation_id"].clone();
    assert!(pinned.is_string());
    let doc = call(&e, "store.export", json!({}));
    assert_eq!(doc["tasks"][0]["delivered_annotation_id"], pinned);
    let fresh = engine();
    call(&fresh, "store.import", doc);
    let got = call(&fresh, "task.get", json!({ "ref": r }));
    assert_eq!(got["delivered_annotation_id"], pinned);
}

#[test]
fn annotation_update_edits_in_place_and_reindexes_search() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "budget table" }));
    let first = note(&e, 1, "budgets keyed by monthcolumn and category.");
    let second = note(&e, 1, "A later note.");
    let before = call(&e, "task.get", json!({ "ref": 1 }));
    let created = before["annotations"][0]["created"].clone();
    let rev = before["_rev"].as_i64().unwrap();

    let out = call(
        &e,
        "annotation.update",
        json!({ "ref": 1, "annotation_id": first, "body": "budgets keyed by category alone.", "expected_rev": rev }),
    );
    assert_eq!(out["annotation"]["id"], json!(first));
    assert_eq!(out["annotation"]["created"], created);

    let after = call(&e, "task.get", json!({ "ref": 1 }));
    assert_eq!(after["annotations"][0]["id"], json!(first), "position kept");
    assert_eq!(
        after["annotations"][0]["created"], created,
        "timestamp kept"
    );
    assert_eq!(
        after["annotations"][0]["body"],
        "budgets keyed by category alone."
    );
    assert_eq!(after["annotations"][1]["id"], json!(second));
    assert_eq!(after["_rev"].as_i64().unwrap(), rev + 1);

    let stale = call(&e, "memory.search", json!({ "query": "monthcolumn" }));
    assert_eq!(stale["hits"].as_array().unwrap().len(), 0, "{stale}");
    let fresh = call(&e, "memory.search", json!({ "query": "category alone" }));
    assert_eq!(fresh["hits"].as_array().unwrap().len(), 1, "{fresh}");
}

#[test]
fn annotation_update_refuses_a_stale_rev_and_a_removed_note() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    let id = note(&e, 1, "original");
    let err = dispatch(
        &e,
        "annotation.update",
        &json!({ "ref": 1, "annotation_id": id, "body": "x", "expected_rev": 0 }),
    )
    .unwrap_err();
    assert_eq!(err.code.as_str(), "conflict", "{err:?}");

    call(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": id }),
    );
    let err = dispatch(
        &e,
        "annotation.update",
        &json!({ "ref": 1, "annotation_id": id, "body": "x" }),
    )
    .unwrap_err();
    assert_eq!(err.code.as_str(), "not_found", "{err:?}");
}

#[test]
fn undo_puts_the_previous_body_back() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    let id = note(&e, 1, "the original words");
    call(
        &e,
        "annotation.update",
        json!({ "ref": 1, "annotation_id": id, "body": "the edited words" }),
    );
    let undone = e.event_revert().expect("undo covers annotation.update");
    assert_eq!(undone["reverted"]["op"], "annotation.update");
    let got = call(&e, "task.get", json!({ "ref": 1 }));
    assert_eq!(got["annotations"][0]["body"], "the original words");
    let hits = call(&e, "memory.search", json!({ "query": "edited" }));
    assert_eq!(hits["hits"].as_array().unwrap().len(), 0);
}

#[test]
fn removing_an_edited_note_redacts_the_edit_events_too() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    let id = note(&e, 1, "secretone");
    call(
        &e,
        "annotation.update",
        json!({ "ref": 1, "annotation_id": id, "body": "secrettwo" }),
    );
    call(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": id }),
    );
    for dump in [
        call(&e, "event.list", json!({ "ref": 1 })),
        call(&e, "store.export", json!({})),
    ] {
        let text = dump.to_string();
        assert!(!text.contains("secretone"), "{text}");
        assert!(!text.contains("secrettwo"), "{text}");
    }
    // The update event survives as a record, its two bodies nulled.
    let events = call(&e, "event.list", json!({ "ref": 1 }));
    let update = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ev| ev["op"] == "annotation.update")
        .expect("the edit is still in the log");
    assert!(update["payload"]["body"].is_null(), "{update}");
    assert!(update["payload"]["previous"].is_null(), "{update}");
    assert_eq!(update["payload"]["redacted"], true, "{update}");
}
