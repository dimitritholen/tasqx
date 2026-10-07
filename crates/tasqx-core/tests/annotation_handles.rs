//! Annotation handles and `annotation.move` (D211, task #1118).
//!
//! `annotation.update` and `annotation.remove` used to demand the 36-character
//! id; they now also take a unique prefix of at least eight characters or the
//! note's 1-based position on the task, and a note can change task.

use serde_json::{json, Value};
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

fn code(e: &Engine, method: &str, params: Value) -> (String, String) {
    let err = dispatch(e, method, &params).unwrap_err();
    (err.code.as_str().to_string(), err.message.clone())
}

fn bodies(e: &Engine, r: i64) -> Vec<String> {
    call(e, "task.get", json!({ "ref": r }))["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["body"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_unique_prefix_of_eight_or_more_characters_names_the_note() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    let id = note(&e, 1, "original");
    let out = call(
        &e,
        "annotation.update",
        json!({ "ref": 1, "annotation_id": &id[..12], "body": "edited" }),
    );
    assert_eq!(out["annotation"]["id"], json!(id));
    assert_eq!(bodies(&e, 1), ["edited"]);
}

#[test]
fn a_prefix_shorter_than_eight_characters_is_refused_by_name() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    let id = note(&e, 1, "original");
    let (c, msg) = code(
        &e,
        "annotation.update",
        json!({ "ref": 1, "annotation_id": &id[..7], "body": "x" }),
    );
    assert_eq!(c, "bad_request", "{msg}");
    assert!(msg.contains("8"), "{msg}");
    assert_eq!(bodies(&e, 1), ["original"]);
}

#[test]
fn an_ambiguous_prefix_is_a_conflict_listing_the_candidates() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    // UUIDv7 ids written in the same minute share their first eight characters.
    let a = note(&e, 1, "first alpha");
    let b = note(&e, 1, "second beta");
    assert_eq!(a[..8], b[..8], "the premise: same-minute ids collide");
    let (c, msg) = code(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": &a[..8] }),
    );
    assert_eq!(c, "conflict", "{msg}");
    assert!(msg.contains(&a) && msg.contains(&b), "{msg}");
    assert!(msg.contains("first alpha"), "{msg}");
    assert_eq!(bodies(&e, 1).len(), 2, "nothing was removed");
}

#[test]
fn a_position_names_the_note_oldest_first() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    note(&e, 1, "one");
    note(&e, 1, "two");
    note(&e, 1, "three");
    // As a string (the CLI) and as an integer (a JSON client).
    call(
        &e,
        "annotation.update",
        json!({ "ref": 1, "annotation_id": "2", "body": "TWO" }),
    );
    call(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": 3 }),
    );
    assert_eq!(bodies(&e, 1), ["one", "TWO"]);
    let (c, msg) = code(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": "5" }),
    );
    assert_eq!(c, "not_found", "{msg}");
    let (c, _) = code(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": "0" }),
    );
    assert_eq!(c, "bad_request");
}

#[test]
fn a_move_keeps_id_body_and_created_and_changes_the_task() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "wrong" }));
    call(&e, "task.add", json!({ "title": "right" }));
    let keep = note(&e, 1, "stays");
    let id = note(&e, 1, "belongs on the other task");
    let created = call(&e, "task.get", json!({ "ref": 1 }))["annotations"][1]["created"].clone();

    let out = call(
        &e,
        "annotation.move",
        json!({ "ref": 1, "annotation_id": "2", "to": 2 }),
    );
    assert_eq!(out["annotation"]["id"], json!(id));
    assert_eq!(out["short_id"], json!(1));
    assert_eq!(out["to"]["short_id"], json!(2), "{out}");

    let from = call(&e, "task.get", json!({ "ref": 1 }));
    assert_eq!(from["annotations"].as_array().unwrap().len(), 1);
    assert_eq!(from["annotations"][0]["id"], json!(keep));
    let to = call(&e, "task.get", json!({ "ref": 2 }));
    assert_eq!(to["annotations"][0]["id"], json!(id));
    assert_eq!(to["annotations"][0]["body"], "belongs on the other task");
    assert_eq!(to["annotations"][0]["created"], created);

    let hits = call(&e, "memory.search", json!({ "query": "belongs other" }));
    assert_eq!(hits["hits"][0]["source"], "task:#2", "{hits}");
}

#[test]
fn a_move_to_the_same_or_a_missing_task_is_refused() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "t" }));
    note(&e, 1, "n");
    let (c, msg) = code(
        &e,
        "annotation.move",
        json!({ "ref": 1, "annotation_id": "1", "to": 1 }),
    );
    assert_eq!(c, "conflict", "{msg}");
    let (c, _) = code(
        &e,
        "annotation.move",
        json!({ "ref": 1, "annotation_id": "1", "to": 99 }),
    );
    assert_eq!(c, "not_found");
    assert_eq!(bodies(&e, 1), ["n"]);
}

#[test]
fn undo_moves_the_note_back() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "a" }));
    call(&e, "task.add", json!({ "title": "b" }));
    let id = note(&e, 1, "wandering");
    call(
        &e,
        "annotation.move",
        json!({ "ref": 1, "annotation_id": &id, "to": 2 }),
    );
    let undone = e.event_revert().expect("undo covers annotation.move");
    assert_eq!(undone["reverted"]["op"], "annotation.move");
    assert_eq!(bodies(&e, 1), ["wandering"]);
    assert!(bodies(&e, 2).is_empty());
    let hits = call(&e, "memory.search", json!({ "query": "wandering" }));
    assert_eq!(hits["hits"][0]["source"], "task:#1", "{hits}");
}

#[test]
fn removing_a_moved_note_still_redacts_its_add_and_edit_events() {
    let e = engine();
    call(&e, "task.add", json!({ "title": "a" }));
    call(&e, "task.add", json!({ "title": "b" }));
    let id = note(&e, 1, "secretone");
    call(
        &e,
        "annotation.update",
        json!({ "ref": 1, "annotation_id": &id, "body": "secrettwo" }),
    );
    call(
        &e,
        "annotation.move",
        json!({ "ref": 1, "annotation_id": &id, "to": 2 }),
    );
    call(
        &e,
        "annotation.remove",
        json!({ "ref": 2, "annotation_id": &id }),
    );
    let text = call(&e, "store.export", json!({})).to_string();
    assert!(!text.contains("secretone"), "{text}");
    assert!(!text.contains("secrettwo"), "{text}");
}
