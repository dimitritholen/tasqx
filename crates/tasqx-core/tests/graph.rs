//! Tests for the D160 graph layer: the shared node-reference parser, the
//! explicit `link.add` / `link.remove` / `link.list` family, and the bounded
//! `graph.query` projection built over both.
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

// ---- graph.query: the fixtures ----------------------------------------------

/// A chain of four tasks, a note, a doc and one explicit link — one of every
/// structural edge D160 names except `belongs_to_project`, which is left out on
/// purpose: nothing here carries a project, because a project node expands to
/// everything filed under it and that fan-out is noise in a test about hops.
fn chain(e: &Engine) -> String {
    for title in [
        "the root task",
        "the blocker",
        "the far blocker",
        "the distant one",
    ] {
        ok(e, "task.add", json!({ "title": title, "tags": ["alpha"] }));
    }
    for (dependent, blocker) in [(1, 2), (2, 3), (3, 4)] {
        ok(
            e,
            "dependency.add",
            json!({ "ref": dependent, "depends_on": blocker }),
        );
    }
    ok(
        e,
        "annotation.add",
        json!({ "ref": 1, "body": "the opening note" }),
    );
    let doc = ok(
        e,
        "memory.add",
        json!({ "title": "the ruling", "body": "links are D160" }),
    )["id"]
        .as_str()
        .expect("doc id")
        .to_string();
    ok(
        e,
        "link.add",
        json!({ "from": 1, "to": format!("memory:{doc}"), "relation": "references" }),
    );
    doc
}

/// Every node's `label`, in the order the projection returned them.
fn labels(g: &Value) -> Vec<String> {
    g["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|n| n["label"].as_str().expect("label").to_string())
        .collect()
}

/// Every edge as `(relation, source)`, deduplicated and sorted — what a test
/// about provenance asks, without pinning uuids.
fn provenance(g: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = g["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|e| {
            (
                e["relation"].as_str().expect("relation").to_string(),
                e["source"].as_str().expect("source").to_string(),
            )
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

// ---- graph.query: depth -----------------------------------------------------

/// Depth 0 is the root and nothing else — the degenerate projection a client
/// asks for when it wants the node's own fields in the graph's vocabulary.
#[test]
fn depth_zero_returns_the_root_and_nothing_else() {
    let e = engine();
    chain(&e);
    let g = ok(&e, "graph.query", json!({ "root": 1, "depth": 0 }));
    assert_eq!(g["depth"], json!(0));
    assert_eq!(g["node_count"], json!(1));
    assert_eq!(g["edge_count"], json!(0));
    assert_eq!(g["nodes"][0]["id"], g["root"]);
    assert_eq!(g["nodes"][0]["type"], json!("task"));
    assert_eq!(g["nodes"][0]["short_id"], json!(1));
    assert_eq!(g["nodes"][0]["label"], json!("the root task"));
    assert_eq!(g["truncated"], json!(false));
    assert_eq!(g["omitted_nodes"], json!(0));
    assert_eq!(g["omitted_edges"], json!(0));
    assert_eq!(g["include_inferred"], json!(false));
}

/// The default is two hops, and every structural edge is walked on the way —
/// each one naming the table it was read out of, so a reader can tell a stored
/// fact from a computed one without knowing how the engine is built.
#[test]
fn the_default_depth_is_two_hops_over_every_structural_edge() {
    let e = engine();
    chain(&e);
    let g = ok(&e, "graph.query", json!({ "root": 1 }));
    assert_eq!(g["depth"], json!(2));

    let seen = labels(&g);
    for reached in [
        "the root task",
        "the blocker",      // one hop, `dependencies`
        "the far blocker",  // two hops
        "the opening note", // `annotations`
        "the ruling",       // an explicit link
    ] {
        assert!(seen.contains(&reached.to_string()), "{reached} in {seen:?}");
    }
    assert!(
        !seen.contains(&"the distant one".to_string()),
        "three hops is past the default depth: {seen:?}"
    );

    assert_eq!(
        provenance(&g),
        [
            ("depends_on".to_string(), "dependencies".to_string()),
            ("has_annotation".to_string(), "annotations".to_string()),
            ("references".to_string(), "links".to_string()),
        ]
    );
    for edge in g["edges"].as_array().expect("edges") {
        assert_eq!(edge["kind"], json!("structural"));
        assert_eq!(
            edge["confidence"],
            Value::Null,
            "a stored edge carries no confidence"
        );
    }
}

/// An explicit link is a first-class structural edge: the relation as stored,
/// and `links` as the table it came from, so a curated edge reads the same way
/// as one the engine derives.
#[test]
fn an_explicit_link_is_a_structural_edge_sourced_from_links() {
    let e = engine();
    let doc = chain(&e);
    let g = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "relation_types": ["references"] }),
    );
    let edges = g["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["relation"], json!("references"));
    assert_eq!(edges[0]["source"], json!("links"));
    assert_eq!(edges[0]["kind"], json!("structural"));
    assert_eq!(edges[0]["to"], json!(format!("memory:{doc}")));
    assert!(
        edges[0]["id"]
            .as_str()
            .expect("edge id")
            .starts_with("link:"),
        "a link edge is named by the link it is: {}",
        edges[0]["id"]
    );
}

/// Project membership is walked in both directions: a task reaches the project
/// it is filed under, and a project root reaches everything filed under it —
/// which is the whole of what a project node is for.
#[test]
fn belongs_to_project_is_walked_from_both_ends() {
    let e = engine();
    ok(&e, "project.create", json!({ "name": "work" }));
    ok(
        &e,
        "task.add",
        json!({ "title": "the filed task", "project": "work" }),
    );
    ok(
        &e,
        "memory.add",
        json!({ "title": "the filed doc", "body": "notes", "project": "work" }),
    );

    let from_task = ok(&e, "graph.query", json!({ "root": 1, "depth": 1 }));
    assert!(labels(&from_task).contains(&"work".to_string()));
    assert_eq!(
        provenance(&from_task),
        [(
            "belongs_to_project".to_string(),
            "tasks.project".to_string()
        )]
    );

    let from_project = ok(
        &e,
        "graph.query",
        json!({ "root": "project:work", "depth": 1 }),
    );
    let seen = labels(&from_project);
    assert!(seen.contains(&"the filed task".to_string()), "{seen:?}");
    assert!(seen.contains(&"the filed doc".to_string()), "{seen:?}");
    assert_eq!(
        provenance(&from_project),
        [
            ("belongs_to_project".to_string(), "docs.project".to_string()),
            (
                "belongs_to_project".to_string(),
                "tasks.project".to_string()
            ),
        ]
    );
}

// ---- graph.query: filters ---------------------------------------------------

/// `node_types` and `relation_types` narrow what the walk keeps and what it
/// crosses. A node the filter drops is neither returned nor expanded, so the
/// filter bounds the cost as well as the answer.
#[test]
fn node_types_and_relation_types_narrow_the_walk() {
    let e = engine();
    chain(&e);

    let tasks_only = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "node_types": ["task"] }),
    );
    assert_eq!(
        labels(&tasks_only),
        ["the root task", "the blocker"],
        "the note and the doc are not tasks"
    );

    let deps_only = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "relation_types": ["depends_on"] }),
    );
    assert_eq!(labels(&deps_only), ["the root task", "the blocker"]);
    assert_eq!(
        provenance(&deps_only),
        [("depends_on".to_string(), "dependencies".to_string())]
    );
}

/// `status` and `tags` narrow the tasks and leave every other kind of node
/// alone: a note has no status to match and a project carries no tags, so
/// applying either to them would answer an empty graph for a live store.
#[test]
fn the_status_and_tag_filters_apply_to_tasks_only() {
    let e = engine();
    chain(&e);
    ok(&e, "tag.add", json!({ "ref": 2, "tags": ["beta"] }));
    ok(&e, "task.done", json!({ "ref": 2, "force": true }));

    let pending = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "status": "pending" }),
    );
    let seen = labels(&pending);
    assert!(!seen.contains(&"the blocker".to_string()), "{seen:?}");
    assert!(seen.contains(&"the opening note".to_string()), "{seen:?}");
    assert!(seen.contains(&"the ruling".to_string()), "{seen:?}");

    let tagged = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 2, "tags": ["beta"] }),
    );
    let seen = labels(&tagged);
    assert!(seen.contains(&"the blocker".to_string()), "{seen:?}");
    assert!(
        !seen.contains(&"the far blocker".to_string()),
        "a task the filter dropped is not expanded either: {seen:?}"
    );
}

/// `project` keeps the things that carry a project — tasks and docs — and lets
/// a project node and the notes on a kept task through, because neither is
/// filed anywhere of its own.
#[test]
fn the_project_filter_keeps_what_carries_a_project() {
    let e = engine();
    ok(&e, "project.create", json!({ "name": "work" }));
    ok(&e, "project.create", json!({ "name": "play" }));
    ok(
        &e,
        "task.add",
        json!({ "title": "the filed task", "project": "work" }),
    );
    // Filed elsewhere, explicitly: the first project a store creates becomes
    // its default, so a bare `task.add` would land in `work` too.
    ok(
        &e,
        "task.add",
        json!({ "title": "the other task", "project": "play" }),
    );
    ok(&e, "dependency.add", json!({ "ref": 1, "depends_on": 2 }));
    ok(
        &e,
        "annotation.add",
        json!({ "ref": 1, "body": "the opening note" }),
    );

    let g = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "project": "work" }),
    );
    let seen = labels(&g);
    assert!(!seen.contains(&"the other task".to_string()), "{seen:?}");
    assert!(seen.contains(&"work".to_string()), "{seen:?}");
    assert!(seen.contains(&"the opening note".to_string()), "{seen:?}");
}

/// The date window reads a task's and a doc's `modified` and a note's
/// `created`. A project has no date at all, so the window cannot exclude one —
/// a filter that silently dropped every project would make a project root
/// answer with itself and nothing else.
#[test]
fn the_date_window_bounds_what_carries_a_date() {
    let e = engine();
    ok(&e, "project.create", json!({ "name": "work" }));
    ok(
        &e,
        "task.add",
        json!({ "title": "the root task", "project": "work" }),
    );
    ok(&e, "task.add", json!({ "title": "the blocker" }));
    ok(&e, "dependency.add", json!({ "ref": 1, "depends_on": 2 }));

    let future = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "modified_after": "2099-01-01T00:00:00Z" }),
    );
    assert_eq!(
        labels(&future),
        ["the root task", "work"],
        "the root is the anchor and a project carries no date"
    );

    let past = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "modified_before": "2000-01-01T00:00:00Z" }),
    );
    assert_eq!(labels(&past), ["the root task", "work"]);

    let open = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "modified_after": "2000-01-01T00:00:00Z" }),
    );
    assert!(labels(&open).contains(&"the blocker".to_string()));
}

// ---- graph.query: caps ------------------------------------------------------

/// The caps bite deterministically: the same call twice is the same JSON, and
/// what was cut is COUNTED rather than silently absent — a client that cannot
/// tell a small graph from a truncated one will draw the wrong picture.
#[test]
fn the_caps_truncate_and_a_repeat_is_byte_identical() {
    let e = engine();
    ok(&e, "project.create", json!({ "name": "work" }));
    for n in 0..12 {
        ok(
            &e,
            "task.add",
            json!({ "title": format!("task {n:02}"), "project": "work" }),
        );
    }

    let g = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 2, "max_nodes": 5 }),
    );
    assert_eq!(g["node_count"], json!(5));
    assert_eq!(g["nodes"].as_array().expect("nodes").len(), 5);
    assert_eq!(g["truncated"], json!(true));
    assert!(g["omitted_nodes"].as_i64().expect("omitted") > 0);
    assert_eq!(
        g,
        ok(
            &e,
            "graph.query",
            json!({ "root": 1, "depth": 2, "max_nodes": 5 })
        )
    );

    let few = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 2, "max_edges": 3 }),
    );
    assert_eq!(few["edge_count"], json!(3));
    assert_eq!(few["truncated"], json!(true));
    assert!(few["omitted_edges"].as_i64().expect("omitted") > 0);
    assert_eq!(
        few,
        ok(
            &e,
            "graph.query",
            json!({ "root": 1, "depth": 2, "max_edges": 3 })
        )
    );
}

// ---- graph.query: inferred edges --------------------------------------------

/// The inferred half is opt-in and labelled: every edge it adds says how
/// confident it is and what computed it, so nothing derived can be mistaken for
/// something the store was told.
#[test]
fn inferred_edges_are_off_by_default_and_carry_a_confidence_when_on() {
    let e = engine();
    ok(&e, "task.add", json!({ "title": "rate limit ceiling" }));
    ok(
        &e,
        "task.add",
        json!({ "title": "the neighbour", "tags": ["api"] }),
    );
    ok(&e, "tag.add", json!({ "ref": 1, "tags": ["api"] }));
    ok(&e, "dependency.add", json!({ "ref": 1, "depends_on": 2 }));
    ok(
        &e,
        "memory.add",
        json!({ "title": "rate limit ceiling", "body": "60 requests a minute per key." }),
    );

    let off = ok(&e, "graph.query", json!({ "root": 1, "depth": 1 }));
    assert_eq!(off["include_inferred"], json!(false));
    for edge in off["edges"].as_array().expect("edges") {
        assert_eq!(edge["kind"], json!("structural"));
    }

    let on = ok(
        &e,
        "graph.query",
        json!({ "root": 1, "depth": 1, "include_inferred": true }),
    );
    assert_eq!(on["include_inferred"], json!(true));
    let inferred: Vec<&Value> = on["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .filter(|e| e["kind"] == json!("inferred"))
        .collect();
    assert!(!inferred.is_empty(), "nothing was inferred: {on}");
    for edge in &inferred {
        let c = edge["confidence"].as_f64().expect("confidence");
        assert!((0.0..=1.0).contains(&c), "confidence {c} is out of range");
        assert!(!edge["source"].as_str().expect("source").is_empty());
    }
    let relations: Vec<&str> = inferred
        .iter()
        .map(|e| e["relation"].as_str().expect("relation"))
        .collect();
    assert!(relations.contains(&"search_match"), "{relations:?}");
    assert!(relations.contains(&"shared_tag"), "{relations:?}");
    assert!(
        labels(&on).contains(&"rate limit ceiling".to_string()),
        "the matching doc joined the projection: {:?}",
        labels(&on)
    );
}

// ---- graph.query: refusals --------------------------------------------------

/// Every bound is refused BY NAME rather than clamped: a caller who asked for
/// depth 5 wants five, and quietly serving two is an answer they cannot tell
/// from the graph really ending there.
#[test]
fn an_out_of_range_bound_or_an_unknown_name_is_refused() {
    let e = engine();
    chain(&e);
    for (params, needle) in [
        (json!({ "root": 1, "depth": 5 }), "depth"),
        (json!({ "root": 1, "depth": -1 }), "depth"),
        (json!({ "root": 1, "max_nodes": 0 }), "max_nodes"),
        (json!({ "root": 1, "max_nodes": 1001 }), "max_nodes"),
        (json!({ "root": 1, "max_edges": 5001 }), "max_edges"),
        (json!({ "root": 1, "node_types": ["doc"] }), "doc"),
        (
            json!({ "root": 1, "relation_types": ["mentions"] }),
            "mentions",
        ),
        (json!({ "root": 1, "status": "finished" }), "finished"),
        (
            json!({ "root": 1, "modified_after": "yesterday" }),
            "modified_after",
        ),
    ] {
        let err = call(&e, "graph.query", params.clone())
            .err()
            .unwrap_or_else(|| panic!("{params} was accepted"));
        assert_eq!(err.code, ErrorCode::BadRequest, "{params}");
        assert!(
            err.message.contains(needle),
            "the refusal must name `{needle}`: {}",
            err.message
        );
    }
}

/// A root that names nothing is `not_found`, the same split every node
/// reference makes: a typo is exit 2 and an absent node is exit 4.
#[test]
fn a_missing_root_is_not_found() {
    let e = engine();
    chain(&e);
    let err = call(&e, "graph.query", json!({ "root": 9999 })).expect_err("no task #9999");
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("9999"), "{}", err.message);

    let missing = call(&e, "graph.query", json!({})).expect_err("`root` is required");
    assert_eq!(missing.code, ErrorCode::BadRequest);
    assert!(missing.message.contains("root"), "{}", missing.message);
}

// ---- graph.query: the bound holds at store scale ----------------------------

/// The default cap is what stands between a graph call and a whole store.
///
/// Twelve hundred tasks in one project, so the project node at depth 1 expands
/// to every one of them at depth 2: the answer must be exactly the 250 the
/// default allows, say that it was cut, and come back fast enough that a UI can
/// call it on a keystroke. The bound is generous on purpose — this runs in a
/// debug build, and the failure it is watching for is a per-node query, which
/// is two orders of magnitude away rather than a few percent.
#[test]
fn a_twelve_hundred_task_store_clamps_to_the_default_cap_quickly() {
    let e = engine();
    ok(&e, "project.create", json!({ "name": "bulk" }));
    for n in 0..1200 {
        ok(
            &e,
            "task.add",
            json!({ "title": format!("bulk task {n:04}"), "project": "bulk", "tags": ["shared"] }),
        );
    }
    // A chain over the first slice only: `dependency.add` walks the graph it
    // already has to refuse a cycle, so a 1,200-long chain is quadratic in the
    // FIXTURE and measures nothing about the query under test.
    for n in 1..60 {
        ok(
            &e,
            "dependency.add",
            json!({ "ref": n, "depends_on": n + 1 }),
        );
    }

    let started = std::time::Instant::now();
    let g = ok(&e, "graph.query", json!({ "root": 1 }));
    let elapsed = started.elapsed();

    assert_eq!(g["node_count"], json!(250), "the default max_nodes");
    assert_eq!(g["nodes"].as_array().expect("nodes").len(), 250);
    assert_eq!(g["truncated"], json!(true));
    assert!(g["omitted_nodes"].as_i64().expect("omitted") > 900);
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "graph.query over 1,200 tasks took {elapsed:?} — a batched lookup became a \
         per-node query"
    );
}
