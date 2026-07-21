//! Tests for the D41 memory subsystem: `memory.add` / `memory.search` /
//! `memory.remove` over FTS5, driven through `dispatch` so the D33 params gate
//! is exercised along with the engine.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

#[test]
fn add_search_remove_round_trip() {
    let e = engine();
    let added = call(
        &e,
        "memory.add",
        json!({
            "title": "Order concurrency decision",
            "body": "Orders are recomputed server-side. We chose SELECT FOR UPDATE over optimistic locking, see ADR-012.",
            "source": "docs/adr/012.md"
        }),
    )
    .expect("memory.add");
    let id = added["id"]
        .as_str()
        .expect("add returns the doc id")
        .to_string();

    let found = call(
        &e,
        "memory.search",
        json!({ "query": "optimistic locking" }),
    )
    .expect("memory.search");
    assert_eq!(found["count"], 1);
    let hit = &found["hits"][0];
    assert_eq!(hit["id"], id.as_str());
    assert_eq!(hit["kind"], "doc");
    assert_eq!(hit["title"], "Order concurrency decision");
    assert_eq!(hit["source"], "docs/adr/012.md");
    assert!(hit["rank"].is_number(), "rank is the bm25 score");
    let snippet = hit["snippet"].as_str().unwrap();
    assert!(
        snippet.contains("optimistic locking"),
        "snippet shows the match: {snippet}"
    );

    let removed = call(&e, "memory.remove", json!({ "id": id })).expect("memory.remove");
    assert_eq!(removed["removed"], true);
    let gone = call(
        &e,
        "memory.search",
        json!({ "query": "optimistic locking" }),
    )
    .unwrap();
    assert_eq!(gone["count"], 0, "a removed doc must leave the index too");
}

/// The verified FTS5 sharp edge: `-` and `.` are operators in its query
/// grammar, so a raw `server-side` is a SYNTAX ERROR against the index. The
/// default path phrase-escapes, so ordinary text just works.
#[test]
fn hyphenated_and_dotted_queries_are_phrases_not_syntax_errors() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "Pricing", "body": "Prices are recomputed server-side, per ADR-012." }),
    )
    .unwrap();

    for query in ["server-side", "ADR-012.", "recomputed server-side"] {
        let found = call(&e, "memory.search", json!({ "query": query }))
            .unwrap_or_else(|e| panic!("query {query:?} must not be a syntax error: {e:?}"));
        assert_eq!(found["count"], 1, "query {query:?} should hit");
    }
}

#[test]
fn raw_mode_passes_operators_through_and_refuses_bad_syntax_cleanly() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "Deploys", "body": "deploys go through the blue-green pipeline" }),
    )
    .unwrap();

    // Prefix search is an FTS5 operator: only reachable via raw mode.
    let found = call(
        &e,
        "memory.search",
        json!({ "query": "pipel*", "raw": true }),
    )
    .expect("valid raw syntax works");
    assert_eq!(found["count"], 1);

    // Broken raw syntax is the caller's error: bad_request, never a panic and
    // never an ok-empty answer.
    let err = call(
        &e,
        "memory.search",
        json!({ "query": "AND (", "raw": true }),
    )
    .expect_err("broken raw syntax must be refused");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn annotations_are_searchable_and_hits_name_their_task() {
    let e = engine();
    let t = call(&e, "task.add", json!({ "title": "Ship the checkout" })).unwrap();
    let short_id = t["short_id"].as_i64().unwrap();
    call(
        &e,
        "annotation.add",
        json!({ "ref": short_id, "body": "Blocked on the idempotency-key review" }),
    )
    .unwrap();

    let found = call(
        &e,
        "memory.search",
        json!({ "query": "idempotency-key", "scope": "annotations" }),
    )
    .unwrap();
    assert_eq!(found["count"], 1);
    let hit = &found["hits"][0];
    assert_eq!(hit["kind"], "annotation");
    assert_eq!(
        hit["title"], "Ship the checkout",
        "hit carries the task title"
    );
    assert_eq!(
        hit["source"],
        format!("task:#{short_id}"),
        "an annotation hit names its task"
    );
}

#[test]
fn scope_filters_and_an_unknown_scope_is_refused() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "Doc", "body": "shared needle" }),
    )
    .unwrap();
    let t = call(&e, "task.add", json!({ "title": "Carrier" })).unwrap();
    call(
        &e,
        "annotation.add",
        json!({ "ref": t["short_id"], "body": "shared needle" }),
    )
    .unwrap();

    let all = call(&e, "memory.search", json!({ "query": "needle" })).unwrap();
    assert_eq!(all["count"], 2, "default scope covers docs and annotations");
    let docs = call(
        &e,
        "memory.search",
        json!({ "query": "needle", "scope": "docs" }),
    )
    .unwrap();
    assert_eq!(docs["count"], 1);
    assert_eq!(docs["hits"][0]["kind"], "doc");
    let ann = call(
        &e,
        "memory.search",
        json!({ "query": "needle", "scope": "annotations" }),
    )
    .unwrap();
    assert_eq!(ann["count"], 1);
    assert_eq!(ann["hits"][0]["kind"], "annotation");

    let err = call(
        &e,
        "memory.search",
        json!({ "query": "needle", "scope": "everything" }),
    )
    .expect_err("a scope outside the closed set is a caller error, not an empty answer");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        format!("{err:?}").contains("docs"),
        "the refusal should list the accepted scopes"
    );
}

#[test]
fn limit_caps_the_hits_and_best_rank_comes_first() {
    let e = engine();
    for n in 0..5 {
        call(
            &e,
            "memory.add",
            json!({ "title": format!("doc {n}"), "body": "needle ".repeat(n + 1) }),
        )
        .unwrap();
    }
    let found = call(
        &e,
        "memory.search",
        json!({ "query": "needle", "limit": 3 }),
    )
    .unwrap();
    assert_eq!(found["count"], 3);
    let ranks: Vec<f64> = found["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["rank"].as_f64().unwrap())
        .collect();
    let mut sorted = ranks.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(ranks, sorted, "hits are ordered best (lowest bm25) first");
}

#[test]
fn memory_writes_land_in_the_event_log() {
    let e = engine();
    let added = call(&e, "memory.add", json!({ "title": "T", "body": "B" })).unwrap();
    let id = added["id"].as_str().unwrap().to_string();
    call(&e, "memory.remove", json!({ "id": id })).unwrap();

    let events = call(&e, "event.list", json!({ "entity": "doc" })).unwrap();
    let ops: Vec<&str> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ev| ev["op"].as_str().unwrap())
        .collect();
    assert!(ops.contains(&"memory.add"), "add is audited: {ops:?}");
    assert!(ops.contains(&"memory.remove"), "remove is audited: {ops:?}");
}

#[test]
fn removing_an_unknown_id_is_not_found_and_unknown_params_are_refused() {
    let e = engine();
    let err = call(
        &e,
        "memory.remove",
        json!({ "id": "018f0000-0000-7000-8000-000000000000" }),
    )
    .expect_err("removing a doc that does not exist is not_found");
    assert_eq!(err.code, ErrorCode::NotFound);

    // The D33 gate applies to the new methods like every other.
    let err = call(
        &e,
        "memory.add",
        json!({ "title": "T", "body": "B", "bogus": 1 }),
    )
    .expect_err("an unknown param is refused, not ignored");
    assert_eq!(err.code, ErrorCode::BadRequest);

    let err = call(&e, "memory.add", json!({ "title": "T" })).expect_err("body is required");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

// ---- review findings (adversarial pass on D41) --------------------------------

/// Confirmed by three independent review lenses: `store.import` wrote
/// annotations with `INSERT OR REPLACE`, and SQLite's REPLACE deletes the old
/// row WITHOUT firing the delete trigger (recursive_triggers is off), leaving
/// the external-content FTS index holding a dangling entry. Moving an
/// annotation id between tasks through import then answered searches with a
/// stale or unrelated hit — silently, forever.
#[test]
fn import_moving_an_annotation_between_tasks_keeps_the_index_in_sync() {
    let e = engine();
    let a = call(&e, "task.add", json!({ "title": "task alpha" })).unwrap();
    call(&e, "task.add", json!({ "title": "task beta" })).unwrap();
    call(
        &e,
        "annotation.add",
        json!({ "ref": a["short_id"], "body": "the original searchable needle" }),
    )
    .unwrap();

    // Build a PARTIAL import payload: only task beta, now claiming annotation
    // id X. Task alpha — where X currently lives — is absent, so the per-task
    // annotation DELETE never touches the old row and the upsert must handle
    // the PK collision itself. With `INSERT OR REPLACE`, SQLite deletes the
    // old row WITHOUT firing the delete trigger, leaving a dangling FTS entry
    // on X's freed rowid.
    let mut doc = call(&e, "store.export", json!({})).unwrap();
    let tasks = doc["tasks"].as_array_mut().unwrap();
    let ann = tasks
        .iter_mut()
        .find(|t| t["title"] == "task alpha")
        .and_then(|t| t["annotations"].as_array_mut())
        .and_then(Vec::pop)
        .expect("alpha carries the annotation");
    tasks.retain(|t| t["title"] == "task beta");
    tasks[0]["annotations"] = json!([{
        "id": ann["id"],
        "body": "the relocated searchable needle",
        "created": ann["created"],
    }]);
    let doc_for_clear = {
        let mut d2 = doc.clone();
        d2["tasks"][0]["annotations"] = json!([]);
        d2
    };
    call(&e, "store.import", doc).expect("import the partial document");

    let new = call(&e, "memory.search", json!({ "query": "relocated" })).unwrap();
    assert_eq!(new["count"], 1, "the moved body must be findable");
    assert_eq!(
        new["hits"][0]["title"], "task beta",
        "hit names the NEW task"
    );

    // Surface a dangling index entry through the public API: clear beta's
    // annotations (freeing the current max rowid) so the next insert reuses
    // the slot a dangling entry would still point at. With the broken
    // REPLACE, the bystander below answered a search for the ORIGINAL body.
    call(&e, "store.import", doc_for_clear).expect("clear beta's annotations");
    call(
        &e,
        "annotation.add",
        json!({ "ref": a["short_id"], "body": "innocent bystander note" }),
    )
    .unwrap();

    let old = call(&e, "memory.search", json!({ "query": "original" })).unwrap();
    assert_eq!(
        old["count"], 0,
        "the old body must be OUT of the index — a hit here is a dangling \
         entry resolving to an unrelated annotation: {old}"
    );
}

/// Review finding: the export document promised to be the self-contained
/// backup (D12/D37) while silently omitting every memory doc — the exact
/// omission shape D37 fixed for projects, reintroduced for docs.
#[test]
fn export_import_round_trip_carries_memory_docs() {
    let e1 = engine();
    call(
        &e1,
        "memory.add",
        json!({ "title": "Runbook", "body": "the backup needle", "source": "rb.md" }),
    )
    .unwrap();
    let doc = call(&e1, "store.export", json!({})).unwrap();
    assert_eq!(
        doc["docs"].as_array().map(Vec::len),
        Some(1),
        "export carries docs"
    );

    let e2 = engine();
    call(&e2, "store.import", doc).expect("import into a fresh store");
    let found = call(&e2, "memory.search", json!({ "query": "backup" })).unwrap();
    assert_eq!(found["count"], 1, "restored docs are searchable");
    assert_eq!(found["hits"][0]["title"], "Runbook");
}

/// Review finding: the CLI import looped over files calling memory.add per
/// file — a mid-directory failure committed a partial import, and re-running
/// duplicated every already-imported doc. memory.import is one transactional
/// call with replace-by-source semantics.
#[test]
fn memory_import_replaces_by_source_and_is_all_or_nothing() {
    let e = engine();
    let first = call(
        &e,
        "memory.import",
        json!({ "docs": [{ "title": "A", "body": "the import needle", "source": "s.md" }] }),
    )
    .unwrap();
    assert_eq!(first["imported"], 1);

    // Re-import from the same source: replaced, not duplicated.
    call(
        &e,
        "memory.import",
        json!({ "docs": [{ "title": "A2", "body": "the import needle again", "source": "s.md" }] }),
    )
    .unwrap();
    let found = call(&e, "memory.search", json!({ "query": "import needle" })).unwrap();
    assert_eq!(found["count"], 1, "same source must replace, not duplicate");
    assert_eq!(found["hits"][0]["title"], "A2");

    // One bad doc rejects the whole batch: nothing from it lands.
    let err = call(
        &e,
        "memory.import",
        json!({ "docs": [
            { "title": "B", "body": "fresh batch needle", "source": "b.md" },
            { "title": "C", "body": "" }
        ] }),
    )
    .expect_err("an invalid doc refuses the whole batch");
    assert_eq!(err.code, ErrorCode::BadRequest);
    let after = call(&e, "memory.search", json!({ "query": "fresh batch" })).unwrap();
    assert_eq!(after["count"], 0, "a refused batch must write nothing");
}

/// Review finding: `limit as i64` wrapped a u64 above i64::MAX negative, and
/// SQLite treats a negative LIMIT as unlimited — the opposite of what the
/// caller bounded.
#[test]
fn a_limit_beyond_i64_is_refused_not_unlimited() {
    let e = engine();
    let err = call(
        &e,
        "memory.search",
        json!({ "query": "x", "limit": u64::MAX }),
    )
    .expect_err("an unrepresentable limit is a caller error");
    assert_eq!(err.code, ErrorCode::BadRequest);
}
