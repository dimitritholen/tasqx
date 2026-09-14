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

/// #228.5: `--raw`'s own help advertises "columns" as part of the grammar it
/// hands the caller, but a `col:query` naming one this scope does not have
/// used to answer with sqlite's bare `no such column: title` — the raw
/// storage-engine text, never a `tasqx` message elsewhere refuses without
/// naming the valid set (`unknown scope`, `unknown setting "bogus.key"`, …).
#[test]
fn a_raw_query_against_an_unknown_column_names_the_real_ones() {
    let e = engine();
    // `annotations_fts` has no `title` column (only `docs_fts` does) — the
    // scope-`all` union hits that arm and it is FTS5, not this engine, that
    // refuses, so the annotations arm needs a row for FTS5 to reach it.
    let t = call(&e, "task.add", json!({ "title": "Ship" })).unwrap();
    call(
        &e,
        "annotation.add",
        json!({ "ref": t["short_id"], "body": "release notes" }),
    )
    .unwrap();

    let err = call(
        &e,
        "memory.search",
        json!({ "query": "title:release", "raw": true }),
    )
    .expect_err("`title` is not a column on the annotations_fts table");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("title") && err.message.contains("body"),
        "the refusal must name the columns that DO exist, not just refuse: {}",
        err.message
    );
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

/// #178/#198: a re-import that replaces a doc sharing its `source` used to
/// DELETE the old row and mint a fresh UUIDv7 for the new one, every single
/// time — so `memory show <id>` (and any annotation or MCP `tasqx_get_memory`
/// call citing that id) 404'd after ANY re-run, and the result line
/// ("Imported 1 doc(s) into memory") never said a replacement had even
/// happened. This pins both halves of the fix: the id and creation date
/// survive a source-replace, and the engine reports how many rows it
/// replaced.
#[test]
fn memory_import_keeps_the_doc_id_stable_across_a_source_replace_and_reports_it() {
    let e = engine();
    let first = call(
        &e,
        "memory.import",
        json!({ "docs": [{ "title": "Runbook", "body": "ORIGINAL: hard won", "source": "runbook.md" }] }),
    )
    .unwrap();
    assert_eq!(first["imported"], 1);
    assert_eq!(
        first["replaced"], 0,
        "a brand-new doc replaces nothing: {first}"
    );
    let id = first["docs"][0]["id"].as_str().unwrap().to_string();
    let created = call(&e, "memory.get", json!({ "id": id.clone() })).expect("memory.get")
        ["created"]
        .as_str()
        .unwrap()
        .to_string();

    // The file is regenerated (e.g. truncated) and the import re-run.
    let second = call(
        &e,
        "memory.import",
        json!({ "docs": [{ "title": "Runbook", "body": "TODO", "source": "runbook.md" }] }),
    )
    .unwrap();
    assert_eq!(second["imported"], 1);
    assert_eq!(
        second["replaced"], 1,
        "the re-run must say it replaced a doc: {second}"
    );
    assert_eq!(
        second["docs"][0]["replaced"], true,
        "and name which entry: {second}"
    );
    let id_after = second["docs"][0]["id"].as_str().unwrap();
    assert_eq!(
        id_after, id,
        "the id must survive a source-replace, so a citation to it does not 404"
    );

    // The OLD id still resolves — to the NEW content, not a 404.
    let doc =
        call(&e, "memory.get", json!({ "id": id.clone() })).expect("memory.get by the same id");
    assert_eq!(doc["body"], "TODO");
    assert_eq!(
        doc["created"], created,
        "a source-replace updates the doc, it does not recreate it"
    );

    // And it still does not duplicate: one row for that source, not two.
    let found = call(&e, "memory.search", json!({ "query": "TODO" })).unwrap();
    assert_eq!(found["count"], 1, "{found}");
}

/// D143 (task #67): a re-import that replaces a doc by `source` used to leave
/// `rev` untouched, so a `memory.update` carrying the PRE-import `expected_rev`
/// passed the optimistic-concurrency guard and silently overwrote the freshly
/// imported text — the exact clobber that guard exists to refuse.
/// `store.import` already carried `rev`; `memory.import` now bumps it too.
#[test]
fn memory_import_bumps_rev_on_a_source_replace_so_a_stale_expected_rev_conflicts() {
    let e = engine();
    let added = call(
        &e,
        "memory.add",
        json!({ "title": "Deploy", "body": "v1 of the deploy steps", "source": "deploy.md" }),
    )
    .unwrap();
    let id = added["id"].as_str().unwrap().to_string();
    let before = call(&e, "memory.get", json!({ "id": id.clone() })).unwrap();
    assert_eq!(before["_rev"], 0, "{before}");

    // The file is edited on disk and the directory import re-run.
    let imported = call(
        &e,
        "memory.import",
        json!({ "docs": [{ "title": "Deploy", "body": "v2 from the re-import", "source": "deploy.md" }] }),
    )
    .unwrap();
    assert_eq!(imported["replaced"], 1, "{imported}");
    assert_eq!(imported["docs"][0]["id"], id, "{imported}");
    assert_eq!(
        imported["docs"][0]["_rev"], 1,
        "the per-doc result must carry the new revision: {imported}"
    );

    let after = call(&e, "memory.get", json!({ "id": id.clone() })).unwrap();
    assert_eq!(after["body"], "v2 from the re-import");
    assert_eq!(after["_rev"], 1, "a source-replace is a revision: {after}");

    // A writer still holding the pre-import rev must be refused, not let
    // through to clobber the import.
    let err = call(
        &e,
        "memory.update",
        json!({ "id": id.clone(), "body": "stale text from before the import", "expected_rev": 0 }),
    )
    .expect_err("a stale expected_rev must conflict after an import");
    assert_eq!(err.code, ErrorCode::Conflict, "{err:?}");
    assert_eq!(err.data.as_ref().unwrap()["expected"], 0, "{err:?}");
    assert_eq!(err.data.as_ref().unwrap()["current"], 1, "{err:?}");
    let untouched = call(&e, "memory.get", json!({ "id": id })).unwrap();
    assert_eq!(
        untouched["body"], "v2 from the re-import",
        "the refused update must write nothing"
    );
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

/// `memory.search` echoes the FTS5 expression it actually ran.
///
/// Every word of a plain query becomes a required quoted phrase, so a question
/// asked the way an agent would ask it is thirteen AND terms and comes back
/// `count: 0` — byte-identical to the answer for a subject nobody wrote down.
/// The echo is what tells those two apart.
#[test]
fn search_echoes_the_expression_that_produced_the_result() {
    let e = Engine::open_in_memory().expect("open");
    e.memory_add(&json!({
        "title": "the daemon transport",
        "body": "the dashboard talks to the tasqx daemon over the local socket / named pipe",
    }))
    .expect("doc");

    let hit = e
        .memory_search(&json!({ "query": "named pipe" }))
        .expect("search");
    assert_eq!(hit["count"], json!(1));
    assert_eq!(hit["matched"], json!("\"named\" \"pipe\""));

    // The same store, the same subject, asked as a sentence.
    let miss = e
        .memory_search(&json!({ "query": "why did we choose a named pipe instead of TCP" }))
        .expect("search");
    assert_eq!(miss["count"], json!(0));
    let matched = miss["matched"].as_str().expect("the expression");
    assert!(
        matched.contains("\"why\"") && matched.contains("\"TCP\""),
        "the required terms are what explain the zero: {matched}"
    );

    // In raw mode the caller owns the syntax, so the echo is their expression.
    let raw = e
        .memory_search(&json!({ "query": "named OR nothingmatchesthis", "raw": true }))
        .expect("search");
    assert_eq!(raw["matched"], json!("named OR nothingmatchesthis"));
}

// ---- D71: the document a search finds can be read ---------------------------

/// The body a hit only excerpts comes back whole.
///
/// The failure: `memory.search` returns a `snippet()` excerpt — 60 to 88
/// characters on real documents — plus an id no verb accepted. A doc written
/// in the morning was, by the afternoon, findable and unreadable, and the
/// nearest thing to a recovery was `store.export`, which dumps every task,
/// project and doc in the store to get one of them back.
#[test]
fn a_doc_a_search_finds_can_be_read_whole() {
    let e = engine();
    let body = "ING exports CAMT.053 under camt.053.001.02 and Rabobank under \
                camt.053.001.08. Matching on the qualified name works against whichever \
                bank you tested with and fails silently against the other: zero entries \
                parsed, no error, a statement that foots to zero.";
    let added = call(
        &e,
        "memory.add",
        json!({ "title": "namespace versions differ per bank", "body": body, "source": "acme #1" }),
    )
    .expect("add");
    let id = added["id"].as_str().expect("an id");

    let hit = &call(&e, "memory.search", json!({ "query": "camt" })).expect("search")["hits"][0];
    let snippet = hit["snippet"].as_str().expect("a snippet");
    assert!(
        snippet.len() < body.len() / 2,
        "the premise: a hit is an excerpt, not the document ({} of {} chars)",
        snippet.len(),
        body.len()
    );

    let doc = call(&e, "memory.get", json!({ "id": hit["id"] })).expect("get");
    assert_eq!(doc["id"], json!(id));
    assert_eq!(doc["body"], json!(body), "the whole body, verbatim");
    assert_eq!(doc["title"], json!("namespace versions differ per bank"));
    assert_eq!(doc["source"], json!("acme #1"));
    assert!(doc["created"].is_string() && doc["modified"].is_string());
}

/// An annotation id is refused with the route that works, not a bare miss.
///
/// `memory.search` returns docs AND annotations, so half the ids it hands back
/// are not docs. Annotations already have a read path — the hit names the task
/// as `task:#<short_id>` and `task.get` returns the bodies — and serving them
/// here too would be a second spelling of a reachable behaviour (D30).
#[test]
fn an_annotation_id_is_refused_by_naming_the_task_it_belongs_to() {
    let e = engine();
    e.task_add(&json!({ "title": "parse the statements" }))
        .expect("add");
    let annotated = e
        .annotation_add(&json!({ "ref": 1, "body": "matching on the qualified name fails" }))
        .expect("annotate");
    let ann_id = annotated["annotation"]["id"].clone();

    let err = call(&e, "memory.get", json!({ "id": ann_id })).expect_err("not a doc");
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(
        err.message.contains("annotation") && err.message.contains("task.get"),
        "the refusal has to name the route that does work: {}",
        err.message
    );
    assert!(
        err.message.contains("#1"),
        "and which task to use it on: {}",
        err.message
    );
}

/// An id that is nothing at all is a plain miss.
#[test]
fn an_unknown_memory_id_is_a_plain_not_found() {
    let e = engine();
    let err = call(
        &e,
        "memory.get",
        json!({ "id": "01a05499-db0a-7d52-81b1-b8633edaf598" }),
    )
    .expect_err("nothing there");
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("no memory doc"), "{}", err.message);
}

/// The reader is read scope: a read-only agent may consult knowledge (D41),
/// and after D71 "consult" means the document rather than an excerpt of it.
#[test]
fn get_memory_is_reachable_from_a_read_only_server() {
    use tasqx_core::{McpServer, Scope};
    let e = engine();
    let added = call(
        &e,
        "memory.add",
        json!({ "title": "the runbook", "body": "deploys go through blue-green" }),
    )
    .expect("add");
    let server = McpServer::new(&e, Scope::Read);
    let result = server
        .handle_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "tasqx_get_memory", "arguments": { "id": added["id"] } }
        }))
        .expect("a response");
    assert_eq!(
        result["result"]["isError"].as_bool(),
        Some(false),
        "a read tool must not be fenced out of a read scope: {result}"
    );
    let text = result["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    let doc: Value = serde_json::from_str(text).expect("json");
    assert_eq!(doc["body"], json!("deploys go through blue-green"));
}

// ---- #128: stemming --------------------------------------------------------

/// The porter tokenizer stems each term before matching, so "reviewing" finds
/// a doc that only ever says "review" — the exact miss the review finding
/// reported (default AND-of-literal-tokens had zero tolerance for a single
/// absent inflection).
#[test]
fn a_stemmed_query_matches_an_unrelated_inflection_of_the_same_word() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "PR checklist", "body": "review the diff before merging" }),
    )
    .expect("add");

    let hits = call(&e, "memory.search", json!({ "query": "reviewing" })).expect("search");
    assert_eq!(
        hits["count"], 1,
        "the porter tokenizer should stem \"reviewing\" to match \"review\": {hits}"
    );

    // And the reverse direction, so the guard is not a coincidence of one
    // particular pair of inflections.
    let hits2 = call(&e, "memory.search", json!({ "query": "reviewed" })).expect("search");
    assert_eq!(
        hits2["count"], 1,
        "\"reviewed\" should stem-match too: {hits2}"
    );
}

// ---- #132: total / has_more -------------------------------------------------

/// `total` counts every match before `limit` truncates, and `has_more` is
/// that comparison already done — a query matching more docs than `limit`
/// must say so instead of coming back looking complete.
#[test]
fn search_total_and_has_more_survive_a_limit_narrower_than_the_match_set() {
    let e = engine();
    for i in 0..5 {
        call(
            &e,
            "memory.add",
            json!({ "title": format!("doc {i}"), "body": "shared keyword everywhere" }),
        )
        .expect("add");
    }

    let narrow = call(
        &e,
        "memory.search",
        json!({ "query": "keyword", "limit": 2 }),
    )
    .expect("search");
    assert_eq!(narrow["count"], 2, "{narrow}");
    assert_eq!(narrow["total"], 5, "{narrow}");
    assert_eq!(narrow["has_more"], true, "{narrow}");

    let wide = call(
        &e,
        "memory.search",
        json!({ "query": "keyword", "limit": 10 }),
    )
    .expect("search");
    assert_eq!(wide["count"], 5, "{wide}");
    assert_eq!(wide["total"], 5, "{wide}");
    assert_eq!(wide["has_more"], false, "{wide}");
}

// ---- #133: memory.list ------------------------------------------------------

/// `memory.list` browses without a query, newest-modified first, and pages
/// the same way `task.list` does: `total`, and a `next_offset` that is null
/// once nothing is left and otherwise walks the rest.
#[test]
fn list_pages_newest_first_and_next_offset_walks_to_the_end() {
    let e = engine();
    let mut ids = Vec::new();
    for i in 0..3 {
        let added = call(
            &e,
            "memory.add",
            json!({ "title": format!("doc {i}"), "body": "body" }),
        )
        .expect("add");
        ids.push(added["id"].as_str().unwrap().to_string());
    }
    // UUIDv7 ids (and `modified`) are monotonic with insertion order, so the
    // newest-first default should hand them back reversed.
    ids.reverse();

    let page1 = call(&e, "memory.list", json!({ "limit": 2 })).expect("list");
    assert_eq!(page1["count"], 2, "{page1}");
    assert_eq!(page1["total"], 3, "{page1}");
    let page1_ids: Vec<&str> = page1["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(page1_ids, vec![ids[0].as_str(), ids[1].as_str()]);
    let next = page1["next_offset"].as_u64().expect("more to walk");

    let page2 = call(&e, "memory.list", json!({ "limit": 2, "offset": next })).expect("list");
    assert_eq!(page2["count"], 1, "{page2}");
    assert!(
        page2["next_offset"].is_null(),
        "the walk must end once nothing is left: {page2}"
    );
    let page2_ids: Vec<&str> = page2["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(page2_ids, vec![ids[2].as_str()]);
}

// ---- #134: project scoping --------------------------------------------------

/// A doc's `project` is optional and free-standing: `memory.search` and
/// `memory.list` scope to it when asked, and an unscoped doc is invisible to
/// a project-scoped query — not defaulted into it.
#[test]
fn project_scopes_search_and_list_without_leaking_unscoped_docs() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "alpha doc", "body": "shared term", "project": "alpha" }),
    )
    .expect("add");
    call(
        &e,
        "memory.add",
        json!({ "title": "unscoped doc", "body": "shared term" }),
    )
    .expect("add");

    let scoped = call(
        &e,
        "memory.search",
        json!({ "query": "shared", "project": "alpha" }),
    )
    .expect("search");
    assert_eq!(scoped["count"], 1, "{scoped}");
    assert_eq!(scoped["hits"][0]["title"], json!("alpha doc"), "{scoped}");

    let unscoped_search = call(&e, "memory.search", json!({ "query": "shared" })).expect("search");
    assert_eq!(
        unscoped_search["count"], 2,
        "no project filter must see both docs: {unscoped_search}"
    );

    let list_scoped = call(&e, "memory.list", json!({ "project": "alpha" })).expect("list");
    assert_eq!(list_scoped["count"], 1, "{list_scoped}");
    assert_eq!(list_scoped["total"], 1, "{list_scoped}");

    let list_all = call(&e, "memory.list", json!({})).expect("list");
    assert_eq!(list_all["total"], 2, "{list_all}");
}

/// The doc-side arm above filters on `d.project`, a column on `docs` itself.
/// The annotation arm filters on `t.project` — the project of the TASK the
/// annotation belongs to, since annotations carry no `project` column of
/// their own. A guard that only ever exercised the doc arm would miss a
/// broken `AND t.project = :project` clause entirely.
#[test]
fn project_scopes_search_over_annotations_by_their_tasks_project() {
    let e = engine();
    e.project_create(&json!({ "name": "alpha" }))
        .expect("create alpha project");
    e.project_create(&json!({ "name": "beta" }))
        .expect("create beta project");

    let alpha_task = e
        .task_add(&json!({ "title": "alpha task", "project": "alpha" }))
        .expect("add alpha task");
    e.annotation_add(&json!({
        "ref": alpha_task["short_id"],
        "body": "shared keyword in alpha"
    }))
    .expect("annotate alpha task");

    let beta_task = e
        .task_add(&json!({ "title": "beta task", "project": "beta" }))
        .expect("add beta task");
    e.annotation_add(&json!({
        "ref": beta_task["short_id"],
        "body": "shared keyword in beta"
    }))
    .expect("annotate beta task");

    let scoped = call(
        &e,
        "memory.search",
        json!({ "query": "shared", "project": "alpha" }),
    )
    .expect("search");
    assert_eq!(
        scoped["count"], 1,
        "only the alpha task's annotation should match: {scoped}"
    );
    assert_eq!(scoped["hits"][0]["kind"], json!("annotation"), "{scoped}");
    assert_eq!(scoped["hits"][0]["title"], json!("alpha task"), "{scoped}");

    let unscoped = call(&e, "memory.search", json!({ "query": "shared" })).expect("search");
    assert_eq!(
        unscoped["count"], 2,
        "no project filter must see both annotations: {unscoped}"
    );
}

// ---- #135: memory.update ----------------------------------------------------

/// `memory.update` replaces fields in place, bumps `_rev`, and — the same
/// optimistic-concurrency shape `task.modify` uses — a stale `expected_rev`
/// is a `conflict` naming both revs rather than a silent overwrite.
#[test]
fn update_replaces_fields_in_place_and_a_stale_expected_rev_conflicts() {
    let e = engine();
    let added = call(
        &e,
        "memory.add",
        json!({ "title": "runbook", "body": "v1 of the deploy steps" }),
    )
    .expect("add");
    let id = added["id"].as_str().unwrap().to_string();

    let updated = call(
        &e,
        "memory.update",
        json!({ "id": id, "body": "v2 of the deploy steps", "expected_rev": 0 }),
    )
    .expect("update");
    assert_eq!(updated["_rev"], 1, "{updated}");
    assert_eq!(
        updated["title"],
        json!("runbook"),
        "title untouched: {updated}"
    );

    let fetched = call(&e, "memory.get", json!({ "id": id })).expect("get");
    assert_eq!(fetched["body"], json!("v2 of the deploy steps"));
    assert_eq!(fetched["_rev"], 1);

    // A stale expected_rev (still 0, but the doc is now at 1) conflicts and
    // writes nothing.
    let conflict = call(
        &e,
        "memory.update",
        json!({ "id": id, "body": "v3, a lost race", "expected_rev": 0 }),
    )
    .expect_err("stale expected_rev must conflict");
    assert_eq!(conflict.code, ErrorCode::Conflict, "{}", conflict.message);
    assert_eq!(conflict.data.as_ref().unwrap()["current"], json!(1));

    let still_v2 = call(&e, "memory.get", json!({ "id": id })).expect("get");
    assert_eq!(
        still_v2["body"],
        json!("v2 of the deploy steps"),
        "a refused conflict must not have written anything"
    );

    // Retrying with the current rev succeeds.
    let retried = call(
        &e,
        "memory.update",
        json!({ "id": id, "body": "v3, retried correctly", "expected_rev": 1 }),
    )
    .expect("update with the fresh rev");
    assert_eq!(retried["_rev"], 2, "{retried}");
}

// ---- task #12 / D135: frontmatter never reaches a search snippet ----------

/// The reported defect, byte for byte: a doc whose body opens with a
/// `---\n...\n---\n` block (D121(e)'s "frontmatter becomes `key  value`
/// lines", never brought to the write path) put `---` and the raw
/// `description:` line straight into `memory.search`'s FTS5 `snippet()`,
/// because `docs_fts` is external-content and reads its indexed text
/// verbatim. D135's second cut points that index at a DERIVED
/// `docs.search_body` (flattened) rather than rewriting `docs.body` itself,
/// so `memory.get`/`memory show`/`tasqx_get_memory` see the fence exactly as
/// written (a `store.export`/`store.import` round-trip stays byte-for-byte)
/// while the snippet — read by the CLI *and* `tasqx_search_memory` over MCP,
/// since both are clients of this one JSON result — never carries it.
#[test]
fn memory_search_snippet_never_shows_frontmatter_but_the_stored_body_is_untouched() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({
            "title": "release-process",
            "body": "---\ndescription: How an SDK release is cut, tagged and announced\n---\n\
                      Cut the release branch on Monday, tag after the canary has run for a day, \
                      and announce in the changelog feed once the packages are live.",
            "source": "docs/release.md",
        }),
    )
    .expect("memory.add");

    let found = call(&e, "memory.search", json!({ "query": "release" })).expect("memory.search");
    assert_eq!(found["count"], 1, "{found}");
    let snippet = found["hits"][0]["snippet"].as_str().unwrap();
    assert!(
        !snippet.contains("---"),
        "a frontmatter delimiter leaked into the snippet: {snippet:?}"
    );
    assert!(
        !snippet.contains("description:"),
        "raw `key: value` frontmatter leaked into the snippet as body text: {snippet:?}"
    );

    // `memory.get` (and so `memory show`'s `--json`, `tasqx_get_memory`) must
    // see the doc exactly as written — the fence and all. Rewriting `body`
    // itself was this fix's own first cut, and a review finding caught it:
    // it broke `store.export`/`store.import`'s byte-for-byte round-trip.
    let id = found["hits"][0]["id"].as_str().unwrap();
    let whole = call(&e, "memory.get", json!({ "id": id })).expect("memory.get");
    let body = whole["body"].as_str().unwrap();
    assert!(
        body.contains("---") && body.contains("description:"),
        "the stored body must stay exactly what was written: {body:?}"
    );
}

/// An empty fence (`---\n---\n`) has no lines, but it is still a fence, and
/// its delimiters must not reach the snippet any more than a full one's do.
#[test]
fn an_empty_frontmatter_fence_never_reaches_the_snippet() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "empty-fence", "body": "---\n---\nZebrafish husbandry notes." }),
    )
    .expect("memory.add");

    let found = call(&e, "memory.search", json!({ "query": "zebrafish" })).expect("search");
    assert_eq!(found["count"], 1, "{found}");
    let snippet = found["hits"][0]["snippet"].as_str().unwrap();
    assert!(
        !snippet.contains("---"),
        "an empty fence's delimiters leaked into the snippet: {snippet:?}"
    );
}

/// D41 advertises FTS5 column filters under `raw`, and `docs_fts`'s text
/// column has always been called `body`. Pointing the index at the derived
/// frontmatter-flattened text (D135) must not rename that column out from
/// under a caller: `body:release` found this doc before D135 and must still.
#[test]
fn a_raw_body_column_filter_still_finds_a_doc() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({
            "title": "release-process",
            "body": "---\ndescription: How an SDK is shipped\n---\nCut the release branch on Monday.",
        }),
    )
    .expect("memory.add");

    let found = call(
        &e,
        "memory.search",
        json!({ "query": "body:release", "raw": true, "scope": "docs" }),
    )
    .expect("`body` is the docs index's text column");
    assert_eq!(found["count"], 1, "{found}");

    // And the column reads the flattened text, not the raw body: a word that
    // lives only in the frontmatter is found through it, fence-free.
    let fm = call(
        &e,
        "memory.search",
        json!({ "query": "body:SDK", "raw": true, "scope": "docs" }),
    )
    .expect("search");
    assert_eq!(fm["count"], 1, "{fm}");
    let snippet = fm["hits"][0]["snippet"].as_str().unwrap();
    assert!(!snippet.contains("---"), "{snippet:?}");
}

/// A term that only ever appears inside the frontmatter block (the
/// `description`'s own words, never repeated in the prose body) must still
/// find the doc — flattening turns the fence into prose, it does not cut it,
/// unlike `memory.import`'s unrelated agent-memory frontmatter cut (#228.4).
#[test]
fn a_query_matching_only_inside_frontmatter_still_finds_the_doc() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({
            "title": "release-process",
            "body": "---\ndescription: How an SDK release is cut, tagged and announced\n---\n\
                      Cut the release branch on Monday.",
        }),
    )
    .expect("memory.add");

    // "SDK" appears nowhere but the frontmatter description.
    let found = call(&e, "memory.search", json!({ "query": "SDK" })).expect("memory.search");
    assert_eq!(
        found["count"], 1,
        "a word that lived only in frontmatter must still be findable: {found}"
    );
}

/// The review finding's own repro: a YAML list (`tags:\n  - deploy\n  -
/// kubernetes`) and a folded block scalar (`summary: >\n  folded block
/// text`) both have lines with no `:` — `flatten`'s first cut silently
/// dropped every one of them, so `kubernetes` (a word that lives only inside
/// the list) stopped finding the doc entirely (`count: 0`) where it used to
/// before this fix existed. `memory.get` must also return `body` completely
/// unchanged — the stored copy is never flattened, only the search index is.
#[test]
fn a_frontmatter_list_items_word_still_finds_the_doc_and_the_body_is_unchanged() {
    let e = engine();
    let raw_body = "---\ntags:\n  - deploy\n  - kubernetes\nsummary: >\n  folded block text\n\
                     ---\nBody.";
    let added = call(
        &e,
        "memory.add",
        json!({ "title": "deploy-notes", "body": raw_body }),
    )
    .expect("memory.add");
    let id = added["id"].as_str().unwrap();

    let found = call(&e, "memory.search", json!({ "query": "kubernetes" })).expect("search");
    assert_eq!(
        found["count"], 1,
        "a word that lives only in a frontmatter list item must still find the doc: {found}"
    );

    let whole = call(&e, "memory.get", json!({ "id": id })).expect("memory.get");
    assert_eq!(
        whole["body"].as_str().unwrap(),
        raw_body,
        "memory.get must return the body exactly as written"
    );
}

/// A body that merely opens with a horizontal rule (`---` with no matching
/// close) is not frontmatter — flattening must leave it alone exactly as
/// `frontmatter::block`'s own doc promises, rather than eating the whole
/// body hunting a fence that never comes.
#[test]
fn a_bare_leading_horizontal_rule_is_not_frontmatter() {
    let e = engine();
    let added = call(
        &e,
        "memory.add",
        json!({ "title": "notes", "body": "---\nThis is just a rule up top, no closing fence." }),
    )
    .expect("memory.add");
    let id = added["id"].as_str().unwrap();
    let whole = call(&e, "memory.get", json!({ "id": id })).expect("memory.get");
    assert_eq!(
        whole["body"],
        json!("---\nThis is just a rule up top, no closing fence."),
        "a body with no closing fence must be stored unchanged"
    );
}

/// `memory.update`'s index gets the same treatment as `memory.add`'s: a doc
/// corrected to open with a fresh frontmatter block must not reintroduce the
/// snippet leak — but `memory.get` must still see the correction exactly as
/// given, fence and all.
#[test]
fn memory_update_reindexes_a_newly_added_frontmatter_block_without_rewriting_it() {
    let e = engine();
    let added = call(
        &e,
        "memory.add",
        json!({ "title": "runbook", "body": "v1 of the deploy steps" }),
    )
    .expect("add");
    let id = added["id"].as_str().unwrap().to_string();

    call(
        &e,
        "memory.update",
        json!({
            "id": id,
            "body": "---\nauthor: infra\n---\nv2 of the deploy steps",
        }),
    )
    .expect("update");

    let fetched = call(&e, "memory.get", json!({ "id": id })).expect("get");
    let body = fetched["body"].as_str().unwrap();
    assert!(
        body.contains("---") && body.contains("author: infra"),
        "the stored body must stay exactly what memory.update was given: {body:?}"
    );

    let found = call(&e, "memory.search", json!({ "query": "infra" })).expect("search");
    assert_eq!(
        found["count"], 1,
        "a word that lives only in the new frontmatter must still find the doc: {found}"
    );
    let snippet = found["hits"][0]["snippet"].as_str().unwrap();
    assert!(
        !snippet.contains("---"),
        "a frontmatter delimiter leaked into the reindexed snippet: {snippet:?}"
    );
}

/// `include_unscoped` widens a project scope to documents belonging to NO
/// project — never to documents belonging to ANOTHER one (D136).
#[test]
fn include_unscoped_admits_global_docs_and_still_excludes_other_projects() {
    let e = engine();
    for (title, project) in [
        ("alpha note", Some("alpha")),
        ("beta note", Some("beta")),
        ("global note", None),
    ] {
        let mut params = json!({ "title": title, "body": "retries are bounded and logged" });
        if let Some(p) = project {
            params["project"] = json!(p);
        }
        call(&e, "memory.add", params).expect("doc");
    }

    let titles = |v: &Value| -> Vec<String> {
        let mut t: Vec<String> = v["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["title"].as_str().unwrap().to_string())
            .collect();
        t.sort();
        t
    };

    let strict = call(
        &e,
        "memory.search",
        json!({ "query": "retries", "project": "alpha" }),
    )
    .expect("strict");
    assert_eq!(titles(&strict), ["alpha note"], "D115's scope is unchanged");

    let widened = call(
        &e,
        "memory.search",
        json!({ "query": "retries", "project": "alpha", "include_unscoped": true }),
    )
    .expect("widened");
    assert_eq!(
        titles(&widened),
        ["alpha note", "global note"],
        "the global doc joins; beta's does not"
    );
}

/// With no `project` there is nothing to widen from, and a caller who sent
/// this believes a scope is being applied (D33).
#[test]
fn include_unscoped_without_a_project_is_refused() {
    let e = engine();
    let err = call(
        &e,
        "memory.search",
        json!({ "query": "retries", "include_unscoped": true }),
    )
    .expect_err("a value that changes nothing is refused, not ignored");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("project"),
        "the refusal names what is missing: {}",
        err.message
    );
}
