//! D241: quick-capture notes — a top-level entity beside tasks and memory
//! docs, driven through `dispatch` so the D33 params gate runs too.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn add(e: &Engine, body: &str) -> Value {
    call(e, "note.add", json!({ "body": body })).expect("note.add")
}

#[test]
fn a_note_is_added_with_its_own_short_id_counter_and_leaves_tasks_alone() {
    let e = engine();
    let first = add(&e, "read https://example.com later");
    let second = add(&e, "brainstorm: a pony");
    assert_eq!(first["short_id"], json!(1), "{first}");
    assert_eq!(second["short_id"], json!(2), "{second}");
    assert_eq!(first["status"], json!("open"));
    assert_eq!(first["promoted_to"], Value::Null);
    assert_eq!(first["has_links"], json!(true));
    assert_eq!(second["has_links"], json!(false));
    assert_eq!(first["_rev"], json!(0));
    // The task counter never moved: the first task is still #1.
    let task = call(&e, "task.add", json!({ "title": "t" })).unwrap();
    assert_eq!(task["short_id"], json!(1), "{task}");
    // And a note is no task: nothing lists it.
    let listed = call(&e, "task.list", json!({})).unwrap();
    assert_eq!(listed["total"], json!(1), "{listed}");
}

#[test]
fn note_add_refuses_a_blank_body_and_an_unknown_project() {
    let e = engine();
    let err = call(&e, "note.add", json!({ "body": "  " })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
    let err = call(&e, "note.add", json!({ "body": "x", "project": "nope" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "{}", err.message);
}

#[test]
fn note_list_pages_filters_and_searches_by_substring() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "home" })).unwrap();
    add(&e, "Read HTTPS://nowhere? no: plain");
    add(&e, "see http://a.example for the Pony");
    call(
        &e,
        "note.add",
        json!({ "body": "pony in home", "project": "home" }),
    )
    .unwrap();
    let archived = add(&e, "old pony idea");
    call(
        &e,
        "note.update",
        json!({ "id": archived["id"], "status": "archived" }),
    )
    .unwrap();

    let open = call(&e, "note.list", json!({})).unwrap();
    assert_eq!(open["total"], json!(3), "{open}");
    assert_eq!(open["count"], json!(3));
    assert_eq!(open["next_offset"], Value::Null);
    let all = call(&e, "note.list", json!({ "status": "all" })).unwrap();
    assert_eq!(all["total"], json!(4), "{all}");
    let arch = call(&e, "note.list", json!({ "status": "archived" })).unwrap();
    assert_eq!(arch["total"], json!(1), "{arch}");

    let pony = call(&e, "note.list", json!({ "query": "PONY" })).unwrap();
    assert_eq!(
        pony["total"],
        json!(2),
        "case-insensitive substring: {pony}"
    );
    let links = call(&e, "note.list", json!({ "links": true })).unwrap();
    assert_eq!(
        links["total"],
        json!(1),
        "only a real http(s):// link: {links}"
    );
    assert_eq!(links["notes"][0]["has_links"], json!(true));
    let home = call(&e, "note.list", json!({ "project": "home" })).unwrap();
    assert_eq!(home["total"], json!(1), "{home}");

    let page = call(&e, "note.list", json!({ "limit": 2 })).unwrap();
    assert_eq!(page["count"], json!(2));
    assert_eq!(page["next_offset"], json!(2), "{page}");
    let rest = call(&e, "note.list", json!({ "limit": 2, "offset": 2 })).unwrap();
    assert_eq!(rest["count"], json!(1));
    assert_eq!(rest["next_offset"], Value::Null);

    let err = call(&e, "note.list", json!({ "status": "gone" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
}

#[test]
fn a_like_wildcard_in_the_query_is_matched_literally() {
    let e = engine();
    add(&e, "100% sure");
    add(&e, "100 percent");
    let hit = call(&e, "note.list", json!({ "query": "0%" })).unwrap();
    assert_eq!(hit["total"], json!(1), "{hit}");
    let underscore = call(&e, "note.list", json!({ "query": "_" })).unwrap();
    assert_eq!(underscore["total"], json!(0), "{underscore}");
}

#[test]
fn note_get_takes_a_uuid_or_a_short_id() {
    let e = engine();
    let n = add(&e, "find me");
    let by_id = call(&e, "note.get", json!({ "id": n["id"] })).unwrap();
    let by_short = call(&e, "note.get", json!({ "id": 1 })).unwrap();
    let by_text = call(&e, "note.get", json!({ "id": "1" })).unwrap();
    assert_eq!(by_id, by_short);
    assert_eq!(by_id, by_text);
    assert_eq!(by_id["body"], json!("find me"));
    let err = call(&e, "note.get", json!({ "id": 9 })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = call(&e, "note.get", json!({ "id": "nonsense" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn note_update_guards_on_expected_rev_and_archives_through_status() {
    let e = engine();
    let n = add(&e, "draft");
    let up = call(
        &e,
        "note.update",
        json!({ "id": n["id"], "body": "better", "expected_rev": 0 }),
    )
    .unwrap();
    assert_eq!(up["body"], json!("better"));
    assert_eq!(up["_rev"], json!(1));
    let err = call(
        &e,
        "note.update",
        json!({ "id": n["id"], "body": "stale", "expected_rev": 0 }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    let arch = call(&e, "note.update", json!({ "id": 1, "status": "archived" })).unwrap();
    assert_eq!(arch["status"], json!("archived"));
    let back = call(&e, "note.update", json!({ "id": 1, "status": "open" })).unwrap();
    assert_eq!(back["status"], json!("open"));
    let err = call(&e, "note.update", json!({ "id": 1 })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
}

#[test]
fn note_remove_deletes_the_row() {
    let e = engine();
    let n = add(&e, "throwaway");
    let out = call(&e, "note.remove", json!({ "id": n["id"] })).unwrap();
    assert_eq!(out["removed"], json!(true));
    let err = call(&e, "note.get", json!({ "id": n["id"] })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn promoting_to_a_task_creates_it_archives_the_note_and_keeps_the_long_body() {
    let e = engine();
    let n = add(
        &e,
        "Ship the notes slice\nwith every touchpoint\nand a test",
    );
    let out = call(&e, "note.promote", json!({ "id": n["id"], "to": "task" })).unwrap();
    let short = out["task"]["short_id"].as_i64().expect("a task");
    assert_eq!(out["task"]["title"], json!("Ship the notes slice"));
    assert_eq!(out["note"]["status"], json!("archived"));
    let target = format!("task:{}", out["task"]["id"].as_str().unwrap());
    assert_eq!(out["note"]["promoted_to"], json!(target));
    assert_eq!(out["note"]["promoted_task_short_id"], json!(short));
    let task = call(&e, "task.get", json!({ "ref": short })).unwrap();
    let notes = task["annotations"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{task}");
    assert_eq!(
        notes[0]["body"],
        json!("Ship the notes slice\nwith every touchpoint\nand a test")
    );

    // A second promote is refused, naming where it went.
    let err = call(&e, "note.promote", json!({ "id": n["id"], "to": "memory" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains(&target), "{}", err.message);
}

#[test]
fn a_one_line_note_promotes_to_a_task_with_no_annotation_and_an_explicit_title_wins() {
    let e = engine();
    add(&e, "short one");
    let out = call(&e, "note.promote", json!({ "id": 1, "to": "task" })).unwrap();
    let task = call(&e, "task.get", json!({ "ref": out["task"]["short_id"] })).unwrap();
    assert_eq!(task["title"], json!("short one"));
    assert!(task["annotations"].as_array().unwrap().is_empty(), "{task}");

    add(&e, "body text that is not the title");
    let out = call(
        &e,
        "note.promote",
        json!({ "id": 2, "to": "task", "title": "The title" }),
    )
    .unwrap();
    let task = call(&e, "task.get", json!({ "ref": out["task"]["short_id"] })).unwrap();
    assert_eq!(task["title"], json!("The title"));
    assert_eq!(task["annotations"].as_array().unwrap().len(), 1);
}

#[test]
fn a_long_first_line_is_cut_on_a_char_boundary() {
    let e = engine();
    let long = "é".repeat(200);
    add(&e, &long);
    let out = call(&e, "note.promote", json!({ "id": 1, "to": "task" })).unwrap();
    let title = out["task"]["title"].as_str().unwrap();
    assert!(title.chars().count() <= 81, "{title}");
    assert!(title.ends_with('…'), "{title}");
}

#[test]
fn promoting_to_memory_files_a_doc_in_the_notes_project() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "home" })).unwrap();
    let n = call(
        &e,
        "note.add",
        json!({ "body": "Kettle descaling\nvinegar, an hour", "project": "home" }),
    )
    .unwrap();
    let out = call(&e, "note.promote", json!({ "id": n["id"], "to": "memory" })).unwrap();
    let doc_id = out["doc"]["id"].as_str().expect("a doc").to_string();
    assert_eq!(
        out["note"]["promoted_to"],
        json!(format!("memory:{doc_id}"))
    );
    let doc = call(&e, "memory.get", json!({ "id": doc_id })).unwrap();
    assert_eq!(doc["title"], json!("Kettle descaling"));
    assert_eq!(doc["body"], json!("Kettle descaling\nvinegar, an hour"));
    assert_eq!(doc["project"], json!("home"));
}

#[test]
fn a_promote_the_task_side_refuses_writes_nothing_at_all() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "old" })).unwrap();
    call(&e, "note.add", json!({ "body": "x", "project": "old" })).unwrap();
    call(&e, "project.archive", json!({ "name": "old" })).unwrap();
    let err = call(&e, "note.promote", json!({ "id": 1, "to": "task" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
    let note = call(&e, "note.get", json!({ "id": 1 })).unwrap();
    assert_eq!(
        note["status"],
        json!("open"),
        "the note must not move: {note}"
    );
    assert_eq!(note["promoted_to"], Value::Null);
    let tasks = call(&e, "task.list", json!({ "filter": "status:any" })).unwrap();
    assert_eq!(tasks["total"], json!(0), "{tasks}");
    // Not even a short_id was spent.
    let t = call(&e, "task.add", json!({ "title": "t" })).unwrap();
    assert_eq!(t["short_id"], json!(1));
}

#[test]
fn promote_honours_expected_rev_and_refuses_an_unknown_target() {
    let e = engine();
    add(&e, "x");
    let err = call(
        &e,
        "note.promote",
        json!({ "id": 1, "to": "task", "expected_rev": 5 }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let err = call(&e, "note.promote", json!({ "id": 1, "to": "board" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn notes_round_trip_through_export_and_import_and_a_merge_keeps_both_sides() {
    let a = engine();
    let kept = add(&a, "keep me https://x.example");
    add(&a, "archive me");
    call(&a, "note.update", json!({ "id": 2, "status": "archived" })).unwrap();
    let doc = call(&a, "store.export", json!({})).unwrap();
    assert_eq!(doc["notes"].as_array().unwrap().len(), 2, "{doc}");
    assert_eq!(doc["dropped_notes"], json!(0));
    assert!(
        doc["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ev| ev["entity"] == "note"),
        "note events must travel"
    );

    let b = engine();
    let out = call(&b, "store.import", doc.clone()).unwrap();
    assert_eq!(out["notes_imported"], json!(2), "{out}");
    let again = call(&b, "store.export", json!({})).unwrap();
    assert_eq!(again["notes"], doc["notes"]);
    // The counter follows the import, so the next note is #3.
    assert_eq!(add(&b, "fresh")["short_id"], json!(3));

    // Merge: each side writes its own note; both survive both ways.
    let c = engine();
    call(&c, "store.import", doc).unwrap();
    add(&a, "only on a");
    add(&c, "only on c");
    let from_a = call(&a, "store.export", json!({})).unwrap();
    let mut merge = from_a.clone();
    merge["merge"] = json!(true);
    call(&c, "store.import", merge).unwrap();
    let bodies: Vec<String> = call(&c, "note.list", json!({ "status": "all" })).unwrap()["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["body"].as_str().unwrap().to_string())
        .collect();
    assert!(bodies.contains(&"only on a".to_string()), "{bodies:?}");
    assert!(bodies.contains(&"only on c".to_string()), "{bodies:?}");
    assert_eq!(bodies.len(), 4, "{bodies:?}");
    let _ = kept;
}

#[test]
fn a_merge_carries_a_note_removal() {
    let a = engine();
    add(&a, "doomed");
    let doc = call(&a, "store.export", json!({})).unwrap();
    let b = engine();
    call(&b, "store.import", doc).unwrap();
    call(&a, "note.remove", json!({ "id": 1 })).unwrap();
    let mut gone = call(&a, "store.export", json!({})).unwrap();
    gone["merge"] = json!(true);
    call(&b, "store.import", gone).unwrap();
    let left = call(&b, "note.list", json!({ "status": "all" })).unwrap();
    assert_eq!(left["total"], json!(0), "{left}");
}

#[test]
fn a_merge_takes_the_later_edit_of_one_note() {
    let a = engine();
    add(&a, "v1");
    let doc = call(&a, "store.export", json!({})).unwrap();
    let b = engine();
    call(&b, "store.import", doc).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    call(&a, "note.update", json!({ "id": 1, "body": "v2" })).unwrap();
    let mut newer = call(&a, "store.export", json!({})).unwrap();
    newer["merge"] = json!(true);
    call(&b, "store.import", newer).unwrap();
    let got = call(&b, "note.get", json!({ "id": 1 })).unwrap();
    assert_eq!(got["body"], json!("v2"), "{got}");
}

#[test]
fn a_filtered_export_scopes_notes_like_docs() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "a" })).unwrap();
    call(&e, "project.create", json!({ "name": "b" })).unwrap();
    call(&e, "note.add", json!({ "body": "in a", "project": "a" })).unwrap();
    call(&e, "note.add", json!({ "body": "in b", "project": "b" })).unwrap();
    add(&e, "unscoped");
    let doc = call(&e, "store.export", json!({ "filter": "project:a" })).unwrap();
    let notes = doc["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{doc}");
    assert_eq!(notes[0]["body"], json!("in a"));
    assert_eq!(doc["dropped_notes"], json!(2));
    assert!(
        !doc["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ev| ev["entity"] == "note" && ev["entity_id"] != notes[0]["id"]),
        "an excluded note's events must not leak"
    );
}

/// Review round 1, item 1: promotion is terminal and survives a merge,
/// whichever side was edited later.
#[test]
fn a_promotion_survives_a_merge_with_a_later_edit_from_the_other_side() {
    let a = engine();
    add(&a, "v1");
    let b = engine();
    call(
        &b,
        "store.import",
        call(&a, "store.export", json!({})).unwrap(),
    )
    .unwrap();
    call(&a, "note.promote", json!({ "id": 1, "to": "task" })).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    call(&b, "note.update", json!({ "id": 1, "body": "v2 later" })).unwrap();

    // Both directions: the later edit's body lands, the promotion stays.
    for (into, from) in [(&a, &b), (&b, &a)] {
        let mut doc = call(from, "store.export", json!({})).unwrap();
        doc["merge"] = json!(true);
        call(into, "store.import", doc).unwrap();
        let got = call(into, "note.get", json!({ "id": 1 })).unwrap();
        assert_eq!(got["status"], json!("archived"), "{got}");
        assert!(
            got["promoted_to"]
                .as_str()
                .unwrap_or("")
                .starts_with("task:"),
            "{got}"
        );
        assert_eq!(got["body"], json!("v2 later"), "{got}");
        let again = call(into, "note.promote", json!({ "id": 1, "to": "task" })).unwrap_err();
        assert_eq!(again.code, ErrorCode::Conflict);
    }
}

/// Item 2: a read resolves the promoted task's short id in this store; the
/// export carries no derived key.
#[test]
fn reads_resolve_the_promoted_tasks_short_id_and_the_export_does_not() {
    let e = engine();
    add(&e, "ship it");
    let out = call(&e, "note.promote", json!({ "id": 1, "to": "task" })).unwrap();
    let listed = call(&e, "note.list", json!({ "status": "all" })).unwrap();
    assert_eq!(
        listed["notes"][0]["promoted_task_short_id"],
        out["task"]["short_id"]
    );
    add(&e, "a doc");
    let doc = call(&e, "note.promote", json!({ "id": 2, "to": "memory" })).unwrap();
    assert_eq!(doc["note"]["promoted_task_short_id"], Value::Null);
    let export = call(&e, "store.export", json!({})).unwrap();
    assert!(export["notes"][0].get("promoted_task_short_id").is_none());
}

/// Item 3: renumbering a colliding note never cascades onto a note that had
/// no conflict, and is reported.
#[test]
fn an_import_renumbers_a_colliding_note_above_the_whole_payload() {
    let a = engine();
    let first = add(&a, "a1");
    add(&a, "a2");
    let payload = call(&a, "store.export", json!({})).unwrap();
    let b = engine();
    add(&b, "held");
    let out = call(&b, "store.import", payload).unwrap();
    assert_eq!(
        out["notes_renumbered"],
        json!([{ "id": first["id"], "from": 1, "to": 3 }]),
        "{out}"
    );
    assert_eq!(
        call(&b, "note.get", json!({ "id": 2 })).unwrap()["body"],
        json!("a2")
    );
}

#[test]
fn an_import_refuses_two_notes_claiming_one_short_id() {
    let a = engine();
    add(&a, "a1");
    add(&a, "a2");
    let mut payload = call(&a, "store.export", json!({})).unwrap();
    payload["notes"][1]["short_id"] = json!(1);
    let err = call(&engine(), "store.import", payload).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
}

/// Item 4: a repeated note id is refused, and only rows written are counted.
#[test]
fn an_import_refuses_a_repeated_note_id_and_counts_only_rows_written() {
    let a = engine();
    add(&a, "a1");
    let mut payload = call(&a, "store.export", json!({})).unwrap();
    let row = payload["notes"][0].clone();
    payload["notes"].as_array_mut().unwrap().push(row);
    let err = call(&engine(), "store.import", payload).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);

    let mut same = call(&a, "store.export", json!({})).unwrap();
    same["merge"] = json!(true);
    let out = call(&a, "store.import", same).unwrap();
    assert_eq!(
        out["notes_imported"],
        json!(0),
        "the store's copy stood: {out}"
    );
}

/// Item 5: a promoted note cannot be reopened.
#[test]
fn a_promoted_note_cannot_be_reopened() {
    let e = engine();
    add(&e, "x");
    call(&e, "note.promote", json!({ "id": 1, "to": "task" })).unwrap();
    let err = call(&e, "note.update", json!({ "id": 1, "status": "open" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("task:"), "{}", err.message);
    // An archived record can still be corrected.
    let fixed = call(
        &e,
        "note.update",
        json!({ "id": 1, "body": "x, corrected" }),
    )
    .unwrap();
    assert_eq!(fixed["status"], json!("archived"));
}

/// Review round 2, item 1: a plain restore never drops a held promotion.
#[test]
fn a_restore_never_drops_a_held_promotion() {
    let a = engine();
    add(&a, "v1");
    let b = engine();
    call(
        &b,
        "store.import",
        call(&a, "store.export", json!({})).unwrap(),
    )
    .unwrap();
    call(&a, "note.promote", json!({ "id": 1, "to": "task" })).unwrap();
    call(&b, "note.update", json!({ "id": 1, "body": "v2" })).unwrap();
    call(
        &a,
        "store.import",
        call(&b, "store.export", json!({})).unwrap(),
    )
    .unwrap();
    let got = call(&a, "note.get", json!({ "id": 1 })).unwrap();
    assert_eq!(got["status"], json!("archived"), "{got}");
    assert!(got["promoted_to"].as_str().is_some(), "{got}");
    assert_eq!(got["body"], json!("v2"), "{got}");
}

/// Item 2: an incoherent promotion in a payload is refused at the door.
#[test]
fn an_import_refuses_a_malformed_or_open_promotion() {
    let a = engine();
    add(&a, "x");
    let base = call(&a, "store.export", json!({})).unwrap();
    let id = base["notes"][0]["id"].as_str().unwrap().to_string();
    for (status, target) in [
        ("archived", "task:nonexistent".to_string()),
        (
            "archived",
            "board:019f5a10-0000-7000-8000-00000000b001".to_string(),
        ),
        (
            "open",
            "task:019f5a10-0000-7000-8000-00000000b001".to_string(),
        ),
    ] {
        let mut doc = base.clone();
        doc["notes"][0]["status"] = json!(status);
        doc["notes"][0]["promoted_to"] = json!(target);
        let err = call(&engine(), "store.import", doc).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest, "{target}: {}", err.message);
        assert!(err.message.contains(&id), "{}", err.message);
    }
    // A target this store does not hold is still legal.
    let mut doc = base.clone();
    doc["notes"][0]["status"] = json!("archived");
    doc["notes"][0]["promoted_to"] = json!("task:019f5a10-0000-7000-8000-00000000b001");
    let e = engine();
    call(&e, "store.import", doc).unwrap();
    assert_eq!(
        call(&e, "note.get", json!({ "id": 1 })).unwrap()["promoted_task_short_id"],
        Value::Null
    );
}

/// Item 3: a merge that adopts a promotion logs what it wrote, and the rev
/// clears both sides' (`max(held, payload) + 1`, the task rule).
#[test]
fn a_merge_adopting_a_promotion_logs_the_row_written_and_lifts_the_rev() {
    let a = engine();
    add(&a, "v0");
    let b = engine();
    call(
        &b,
        "store.import",
        call(&a, "store.export", json!({})).unwrap(),
    )
    .unwrap();
    for body in ["v1", "v2", "v3"] {
        call(&a, "note.update", json!({ "id": 1, "body": body })).unwrap();
    }
    call(&a, "note.promote", json!({ "id": 1, "to": "task" })).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    call(&b, "note.update", json!({ "id": 1, "body": "b later" })).unwrap();
    let mut doc = call(&a, "store.export", json!({})).unwrap();
    doc["merge"] = json!(true);
    call(&b, "store.import", doc).unwrap();
    let got = call(&b, "note.get", json!({ "id": 1 })).unwrap();
    assert_eq!(got["body"], json!("b later"), "{got}");
    assert_eq!(got["status"], json!("archived"), "{got}");
    assert_eq!(got["_rev"], json!(5), "max(1, 4) + 1: {got}");
    let events = call(&b, "event.list", json!({ "entity": "note", "limit": 1 })).unwrap();
    let ev = &events["events"][0];
    assert_eq!(ev["op"], json!("import"), "{ev}");
    assert_eq!(ev["payload"]["status"], json!("archived"), "{ev}");
}
