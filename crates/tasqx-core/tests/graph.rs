//! Tests for the D160 graph layer: the shared node-reference parser and the
//! explicit `link.add` / `link.remove` / `link.list` family.
//!
//! Driven through `dispatch` so the D33 params gate is exercised with the
//! engine, the way `tests/memory.rs` drives the memory subsystem.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn ok(e: &Engine, method: &str, params: Value) -> Value {
    call(e, method, params).unwrap_or_else(|err| panic!("{method}: {}", err.message))
}

/// Two tasks and one memory doc — one endpoint of every kind the link tests
/// reach for, so each test below adds only what it is about.
fn fixture(e: &Engine) -> (String, String, String) {
    let a = ok(e, "task.add", json!({ "title": "the dependent" }))["id"]
        .as_str()
        .expect("task id")
        .to_string();
    let b = ok(e, "task.add", json!({ "title": "the blocker" }))["id"]
        .as_str()
        .expect("task id")
        .to_string();
    let doc = ok(
        e,
        "memory.add",
        json!({ "title": "the ruling", "body": "links are D160" }),
    )["id"]
        .as_str()
        .expect("doc id")
        .to_string();
    (a, b, doc)
}

// ---- the relation registry --------------------------------------------------

/// D160 fixes the registry, and a typo in it must be refused the way every
/// other closed vocabulary here refuses one (D34): by naming the whole set, so
/// the correction is one glance away rather than one round trip.
#[test]
fn an_unregistered_relation_is_refused_and_names_the_five() {
    let e = engine();
    fixture(&e);
    let err = call(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "refrences" }),
    )
    .expect_err("an unknown relation must not be stored");
    assert_eq!(err.code, ErrorCode::BadRequest);
    for known in [
        "references",
        "supersedes",
        "implements_decision",
        "derived_from",
        "contradicts",
    ] {
        assert!(
            err.message.contains(known),
            "the refusal must list `{known}`: {}",
            err.message
        );
    }
}

// ---- endpoints --------------------------------------------------------------

/// A node cannot reference itself: the edge carries no information and every
/// traversal would have to special-case it.
#[test]
fn a_self_link_is_refused() {
    let e = engine();
    let (a, _, doc) = fixture(&e);
    for (from, to) in [(json!(1), json!(1)), (json!(a), json!(format!("task:{a}")))] {
        let err = call(
            &e,
            "link.add",
            json!({ "from": from, "to": to, "relation": "references" }),
        )
        .expect_err("a self-link must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest);
    }
    // The same node reached by two spellings is still the same node.
    let err = call(
        &e,
        "link.add",
        json!({ "from": doc.clone(), "to": format!("memory:{doc}"), "relation": "references" }),
    )
    .expect_err("a self-link must be refused however it is spelled");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

/// A reference that is well-formed and names nothing is `not_found` carrying
/// the reference back, on both halves and for both spellings the parser takes.
#[test]
fn a_missing_endpoint_is_not_found_by_short_id_and_by_uuid() {
    let e = engine();
    fixture(&e);
    let missing_task = call(
        &e,
        "link.add",
        json!({ "from": 1, "to": 9999, "relation": "references" }),
    )
    .expect_err("no task #9999");
    assert_eq!(missing_task.code, ErrorCode::NotFound);
    assert!(
        missing_task.message.contains("9999"),
        "the refusal must name the reference: {}",
        missing_task.message
    );

    let missing_doc = call(
        &e,
        "link.add",
        json!({
            "from": 1,
            "to": "memory:019f7c0a-3d51-7c42-9a08-1f0c4e5b62d7",
            "relation": "references",
        }),
    )
    .expect_err("no such doc");
    assert_eq!(missing_doc.code, ErrorCode::NotFound);
    assert!(
        missing_doc.message.contains("019f7c0a"),
        "the refusal must name the reference: {}",
        missing_doc.message
    );
}

/// An unknown prefix is a malformed request, not a store miss — the split
/// `require_uuid_shape` already makes for memory ids, so a script can branch on
/// the exit code without parsing JSON.
#[test]
fn an_unknown_node_prefix_is_a_bad_request() {
    let e = engine();
    fixture(&e);
    let err = call(
        &e,
        "link.add",
        json!({ "from": 1, "to": "doc:12", "relation": "references" }),
    )
    .expect_err("`doc:` is not a node type");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("task") && err.message.contains("memory"),
        "the refusal must name the accepted prefixes: {}",
        err.message
    );
}

/// Every spelling D160 gives a node resolves to the same stable id, so a client
/// that holds a short id and one that holds a uuid describe one edge.
#[test]
fn every_node_spelling_resolves_to_the_same_stable_id() {
    let e = engine();
    let (a, _, doc) = fixture(&e);
    let project = ok(&e, "project.create", json!({ "name": "work" }))["id"]
        .as_str()
        .expect("project id")
        .to_string();
    let annotation = ok(
        &e,
        "annotation.add",
        json!({ "ref": 1, "body": "the opening note" }),
    )["annotation"]["id"]
        .as_str()
        .expect("annotation id")
        .to_string();

    let from = |v: Value| -> String {
        ok(
            &e,
            "link.add",
            json!({ "from": v, "to": 2, "relation": "references" }),
        )["from"]
            .as_str()
            .expect("from")
            .to_string()
    };
    assert_eq!(from(json!(1)), format!("task:{a}"));
    assert_eq!(from(json!("1")), format!("task:{a}"));
    assert_eq!(from(json!(a.clone())), format!("task:{a}"));
    assert_eq!(from(json!(format!("task:{a}"))), format!("task:{a}"));
    assert_eq!(from(json!("task:1")), format!("task:{a}"));
    assert_eq!(from(json!(doc.clone())), format!("memory:{doc}"));
    assert_eq!(
        from(json!(format!("memory:{doc}"))),
        format!("memory:{doc}")
    );
    assert_eq!(
        from(json!(format!("annotation:{annotation}"))),
        format!("annotation:{annotation}")
    );
    assert_eq!(
        from(json!(format!("project:{project}"))),
        format!("project:{project}")
    );
    assert_eq!(from(json!("project:work")), format!("project:{project}"));
}

// ---- duplicates and cycles --------------------------------------------------

/// A duplicate is idempotent and SAYS SO: `created:false` beside the id the
/// first call minted, so a caller re-running after losing scrollback can tell
/// the two apart — `dependency.add`'s `inserted` for the explicit edges.
#[test]
fn a_duplicate_link_is_idempotent_and_reports_created_false() {
    let e = engine();
    fixture(&e);
    let first = ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references" }),
    );
    assert_eq!(first["created"], json!(true));

    let second = ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references", "metadata": { "note": "again" } }),
    );
    assert_eq!(second["created"], json!(false));
    assert_eq!(second["id"], first["id"], "the existing link comes back");
    assert_eq!(
        second["metadata"], first["metadata"],
        "a duplicate must not overwrite the metadata the first call stored"
    );
    assert_eq!(
        ok(&e, "link.list", json!({ "ref": 1 }))["total"],
        json!(1),
        "the second call must not have inserted a row"
    );
}

/// A different relation between the same two nodes is a different link: the
/// UNIQUE key carries the relation, so `references` and `contradicts` coexist.
#[test]
fn the_same_pair_can_carry_two_relations() {
    let e = engine();
    fixture(&e);
    ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references" }),
    );
    ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "contradicts" }),
    );
    assert_eq!(ok(&e, "link.list", json!({ "ref": 1 }))["total"], json!(2));
}

/// Cycles are permitted, unlike dependency edges: "A references B" and "B
/// references A" are both true statements about knowledge, and nothing
/// schedules work off a link.
#[test]
fn a_cycle_is_permitted() {
    let e = engine();
    fixture(&e);
    ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references" }),
    );
    let back = ok(
        &e,
        "link.add",
        json!({ "from": 2, "to": 1, "relation": "references" }),
    );
    assert_eq!(back["created"], json!(true));
    assert_eq!(ok(&e, "link.list", json!({ "ref": 1 }))["total"], json!(2));
}

// ---- metadata ---------------------------------------------------------------

/// `metadata` is a JSON object or nothing. A string is a caller who meant to
/// send one and did not, and storing it would make the column's type depend on
/// who wrote the row.
#[test]
fn metadata_must_be_an_object_and_round_trips() {
    let e = engine();
    fixture(&e);
    let err = call(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references", "metadata": "why" }),
    )
    .expect_err("a non-object metadata must be refused");
    assert_eq!(err.code, ErrorCode::BadRequest);

    let added = ok(
        &e,
        "link.add",
        json!({
            "from": 1,
            "to": 2,
            "relation": "references",
            "metadata": { "why": "the design note" },
        }),
    );
    assert_eq!(added["metadata"], json!({ "why": "the design note" }));
    let listed = ok(&e, "link.list", json!({ "ref": 1 }));
    assert_eq!(
        listed["links"][0]["metadata"],
        json!({ "why": "the design note" })
    );

    let bare = ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "supersedes" }),
    );
    assert_eq!(
        bare["metadata"],
        Value::Null,
        "no metadata is null, not {{}}"
    );
}

// ---- expected_rev -----------------------------------------------------------

/// The optimistic-concurrency guard, on the two endpoint kinds that carry a
/// `rev`: a stale one is a `conflict` naming both revs, never a silent write.
#[test]
fn a_stale_expected_rev_is_a_conflict_on_a_task_and_on_a_doc() {
    let e = engine();
    let (_, _, doc) = fixture(&e);
    let rev = |r: Value| -> i64 {
        ok(&e, "task.get", json!({ "ref": r }))["_rev"]
            .as_i64()
            .expect("rev")
    };
    let current = rev(json!(1));

    let err = call(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references", "expected_rev": current + 7 }),
    )
    .expect_err("a stale rev must refuse");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains(&(current + 7).to_string())
            && err.message.contains(&current.to_string()),
        "the conflict must name both revs: {}",
        err.message
    );
    assert_eq!(
        ok(&e, "link.list", json!({ "ref": 1 }))["total"],
        json!(0),
        "nothing may be written behind a refused guard"
    );

    ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references", "expected_rev": current }),
    );

    let doc_rev = ok(&e, "memory.get", json!({ "id": doc.clone() }))["_rev"]
        .as_i64()
        .expect("doc rev");
    let stale_doc = call(
        &e,
        "link.add",
        json!({ "from": doc.clone(), "to": 2, "relation": "references", "expected_rev": doc_rev + 1 }),
    )
    .expect_err("a stale doc rev must refuse");
    assert_eq!(stale_doc.code, ErrorCode::Conflict);
    ok(
        &e,
        "link.add",
        json!({ "from": doc, "to": 2, "relation": "references", "expected_rev": doc_rev }),
    );
}

/// An annotation and a project carry no `rev` at all, so the guard has nothing
/// to compare and is ignored rather than refused — refusing would make
/// `expected_rev` unusable by a client that sends it uniformly.
#[test]
fn expected_rev_is_ignored_for_an_annotation_or_project_endpoint() {
    let e = engine();
    fixture(&e);
    let annotation = ok(&e, "annotation.add", json!({ "ref": 1, "body": "note" }))["annotation"]
        ["id"]
        .as_str()
        .expect("annotation id")
        .to_string();
    ok(&e, "project.create", json!({ "name": "work" }));

    for from in [
        format!("annotation:{annotation}"),
        "project:work".to_string(),
    ] {
        let added = ok(
            &e,
            "link.add",
            json!({ "from": from, "to": 2, "relation": "references", "expected_rev": 99 }),
        );
        assert_eq!(added["created"], json!(true));
    }
}

// ---- link.list --------------------------------------------------------------

/// A link is listed from either end: a node's neighbourhood is what points at
/// it as much as what it points at, and a caller holding one id should not have
/// to ask twice.
#[test]
fn list_answers_from_both_directions_and_filters_by_relation() {
    let e = engine();
    let (_, _, doc) = fixture(&e);
    ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references" }),
    );
    ok(
        &e,
        "link.add",
        json!({ "from": doc, "to": 1, "relation": "supersedes" }),
    );

    let both = ok(&e, "link.list", json!({ "ref": 1 }));
    assert_eq!(both["total"], json!(2), "#1 is an endpoint of both");
    assert_eq!(both["count"], json!(2));

    let filtered = ok(
        &e,
        "link.list",
        json!({ "ref": 1, "relation": "supersedes" }),
    );
    assert_eq!(filtered["total"], json!(1));
    assert_eq!(filtered["links"][0]["relation"], json!("supersedes"));

    let other_end = ok(&e, "link.list", json!({ "ref": 2 }));
    assert_eq!(other_end["total"], json!(1));

    let whole_store = ok(&e, "link.list", json!({}));
    assert_eq!(
        whole_store["total"],
        json!(2),
        "an omitted `ref` lists the store's links"
    );
}

/// Newest first and paged like every other list (D70): `total` is what matched
/// before the window, `next_offset` is null once nothing is left.
#[test]
fn list_pages_newest_first() {
    let e = engine();
    fixture(&e);
    for relation in ["references", "supersedes", "implements_decision"] {
        ok(
            &e,
            "link.add",
            json!({ "from": 1, "to": 2, "relation": relation }),
        );
    }

    let first = ok(&e, "link.list", json!({ "ref": 1, "limit": 2 }));
    assert_eq!(first["count"], json!(2));
    assert_eq!(first["total"], json!(3));
    assert_eq!(first["next_offset"], json!(2));
    assert_eq!(
        first["links"][0]["relation"],
        json!("implements_decision"),
        "ids are UUIDv7, so newest first is id DESC"
    );

    let second = ok(
        &e,
        "link.list",
        json!({ "ref": 1, "limit": 2, "offset": 2 }),
    );
    assert_eq!(second["count"], json!(1));
    assert_eq!(second["next_offset"], Value::Null);
    assert_eq!(second["links"][0]["relation"], json!("references"));
}

// ---- link.remove ------------------------------------------------------------

/// Removing twice is `not_found` the second time: there is nothing left to
/// remove, and answering ok would be D33's unfalsifiable write.
#[test]
fn remove_deletes_once_and_is_not_found_afterwards() {
    let e = engine();
    fixture(&e);
    let id = ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references" }),
    )["id"]
        .as_str()
        .expect("link id")
        .to_string();

    let removed = ok(&e, "link.remove", json!({ "id": id.clone() }));
    assert_eq!(removed, json!({ "id": id, "removed": true }));
    assert_eq!(ok(&e, "link.list", json!({ "ref": 1 }))["total"], json!(0));

    let again = call(&e, "link.remove", json!({ "id": id.clone() }))
        .expect_err("a second removal has nothing to remove");
    assert_eq!(again.code, ErrorCode::NotFound);
    assert!(again.message.contains(&id), "{}", again.message);
}

// ---- cascade ----------------------------------------------------------------

/// `memory.remove` is a hard delete, so a link naming that doc would otherwise
/// be an edge to nothing — invisible to every reader that resolves its
/// endpoints, yet still in the table (the dangling-dependency shape D12 fixed
/// with foreign keys, which a polymorphic endpoint cannot have).
#[test]
fn removing_a_doc_removes_its_links_and_removing_an_annotation_does_not() {
    let e = engine();
    let (_, _, doc) = fixture(&e);
    ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": doc.clone(), "relation": "references" }),
    );
    ok(
        &e,
        "link.add",
        json!({ "from": doc.clone(), "to": 2, "relation": "supersedes" }),
    );
    let annotation = ok(&e, "annotation.add", json!({ "ref": 1, "body": "note" }))["annotation"]
        ["id"]
        .as_str()
        .expect("annotation id")
        .to_string();
    ok(
        &e,
        "link.add",
        json!({ "from": format!("annotation:{annotation}"), "to": 2, "relation": "references" }),
    );

    ok(&e, "memory.remove", json!({ "id": doc }));
    assert_eq!(
        ok(&e, "link.list", json!({}))["total"],
        json!(1),
        "both links naming the removed doc must go, in the same transaction"
    );

    ok(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": annotation }),
    );
    assert_eq!(
        ok(&e, "link.list", json!({}))["total"],
        json!(1),
        "an annotation tombstone leaves the node in place, so its links stay"
    );
}

// ---- events -----------------------------------------------------------------

/// Both writes append an event under the new `link` entity, so the audit log
/// and `event.list {entity:"link"}` can answer what happened to an edge that
/// belongs to no single task.
#[test]
fn link_writes_append_events_under_the_link_entity() {
    let e = engine();
    fixture(&e);
    let id = ok(
        &e,
        "link.add",
        json!({ "from": 1, "to": 2, "relation": "references" }),
    )["id"]
        .as_str()
        .expect("link id")
        .to_string();
    ok(&e, "link.remove", json!({ "id": id.clone() }));

    let events = ok(&e, "event.list", json!({ "entity": "link" }));
    let ops: Vec<&str> = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|ev| ev["op"].as_str().expect("op"))
        .collect();
    assert_eq!(ops, ["link.remove", "link.add"]);
    for ev in events["events"].as_array().expect("events") {
        assert_eq!(ev["entity"], json!("link"));
        assert_eq!(ev["entity_id"], json!(id));
    }
    assert_eq!(
        events["events"][1]["payload"]["relation"],
        json!("references")
    );
}
