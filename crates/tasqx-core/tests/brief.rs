//! Tests for `task.brief` (D136) — the one read that answers "what do I need
//! before starting this".
//!
//! Driven through `dispatch`, like `tests/memory.rs` and `tests/outcomes.rs`,
//! so the D33 params gate is exercised with the engine.
//!
//! The load-bearing case here is `a_task_word_that_matches_one_document_finds_it`:
//! `memory.search` turns every word of a plain query into a required quoted
//! phrase, so a query derived from a task title and handed to it unchanged
//! would be five ANDed terms and would answer `count: 0` on a store that holds
//! exactly the document the agent needed. A derived query is a bag of the
//! task's own words, not a statement of what the caller wants, so it has to be
//! a disjunction — and that difference is the whole reason this is a method
//! rather than something a caller composes.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn add(e: &Engine, title: &str, extra: Value) -> i64 {
    let mut params = json!({ "title": title });
    if let Some(obj) = extra.as_object() {
        for (k, v) in obj {
            params[k] = v.clone();
        }
    }
    call(e, "task.add", params).expect("task.add")["short_id"]
        .as_i64()
        .expect("add returns short_id")
}

fn brief(e: &Engine, r: i64) -> Value {
    call(e, "task.brief", json!({ "ref": r })).expect("task.brief")
}

fn hit_titles(out: &Value) -> Vec<String> {
    out["memory"]["hits"]
        .as_array()
        .expect("hits is an array")
        .iter()
        .map(|h| h["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

// ---- the task half --------------------------------------------------------

#[test]
fn the_brief_carries_the_task_in_the_shape_task_get_returns() {
    let e = engine();
    let a = add(&e, "Ship the release notes", json!({ "priority": "H" }));
    call(
        &e,
        "annotation.add",
        json!({ "ref": a, "body": "acceptance: the notes name every breaking change" }),
    )
    .expect("annotate");

    let out = brief(&e, a);
    let got = call(&e, "task.get", json!({ "ref": a })).expect("task.get");
    // Not a subset and not a reshaping: the same object, so a caller that
    // already knows how to read `task.get` needs to learn nothing new, and the
    // D49 renderer can be pointed straight at it.
    assert_eq!(out["task"], got, "the task half is `task.get`, verbatim");
    assert_eq!(out["task"]["short_id"], a);
    assert_eq!(out["task"]["annotations"][0]["body"].as_str().unwrap(), {
        "acceptance: the notes name every breaking change"
    });
}

// ---- the neighbourhood ----------------------------------------------------

#[test]
fn a_prerequisite_arrives_with_its_newest_annotation() {
    let e = engine();
    let upstream = add(&e, "Freeze the JSON envelope", json!({}));
    let downstream = add(&e, "Write the conformance suite", json!({}));
    call(
        &e,
        "dependency.add",
        json!({ "ref": downstream, "depends_on": upstream }),
    )
    .expect("dep");
    call(
        &e,
        "annotation.add",
        json!({ "ref": upstream, "body": "first pass, superseded" }),
    )
    .expect("note 1");
    call(
        &e,
        "annotation.add",
        json!({ "ref": upstream, "body": "froze it: the floor derives from PARAMS, not a list" }),
    )
    .expect("note 2");
    call(&e, "task.done", json!({ "ref": upstream })).expect("done");

    let n = &brief(&e, downstream)["neighbourhood"];
    let dep = &n["depends_on"][0];
    assert_eq!(dep["short_id"], upstream);
    assert_eq!(dep["title"], "Freeze the JSON envelope");
    assert_eq!(dep["status"], "done");
    // The prerequisite's outcome is the one field in the neighbourhood worth
    // the bytes: it is what the upstream task decided, and reading it used to
    // cost a second `task.get` the agent usually did not make.
    assert_eq!(
        dep["annotation"]["body"], "froze it: the floor derives from PARAMS, not a list",
        "the NEWEST note, not the first"
    );
}

#[test]
fn a_prerequisite_with_nothing_written_on_it_says_so_rather_than_being_dropped() {
    let e = engine();
    let upstream = add(&e, "silent prerequisite", json!({}));
    let downstream = add(&e, "the dependent", json!({}));
    call(
        &e,
        "dependency.add",
        json!({ "ref": downstream, "depends_on": upstream }),
    )
    .expect("dep");

    let dep = &brief(&e, downstream)["neighbourhood"]["depends_on"][0];
    assert_eq!(dep["short_id"], upstream);
    assert!(
        dep["annotation"].is_null(),
        "an unannotated prerequisite is still a prerequisite"
    );
}

#[test]
fn what_the_task_blocks_carries_no_annotation() {
    let e = engine();
    let upstream = add(&e, "the blocker", json!({}));
    let downstream = add(&e, "the dependent", json!({}));
    call(
        &e,
        "dependency.add",
        json!({ "ref": downstream, "depends_on": upstream }),
    )
    .expect("dep");
    call(
        &e,
        "annotation.add",
        json!({ "ref": downstream, "body": "context nobody starting the blocker needs" }),
    )
    .expect("note");

    let blocks = &brief(&e, upstream)["neighbourhood"]["blocks"][0];
    assert_eq!(blocks["short_id"], downstream);
    assert_eq!(blocks["title"], "the dependent");
    // The stored lifecycle status. "Blocked" is a derived property of having an
    // unresolved dependency, not one of the five states, and `task.get` already
    // carries it as its own boolean.
    assert_eq!(blocks["status"], "pending");
    // Deliberately absent: an agent starting work needs what was decided
    // UPSTREAM, not what it is about to unblock — and `task.done` already
    // reports the downstream direction at the moment that direction matters.
    assert!(
        blocks.get("annotation").is_none(),
        "the forward direction is title and status only: {blocks}"
    );
}

/// The neighbourhood and the detail name one row the same way.
///
/// This is a consistency assertion, not a proof of the derivation: D29's
/// `backlog`/`pending` swap only diverges from the stored column once TIME has
/// passed without a write (a task stored `backlog` whose `wait` is now behind
/// it), and at `task.add` the two already agree — so the divergence is not
/// constructible here without controlling the clock. The neighbourhood reads
/// through `map_task_row_at` anyway, because status is a read surface (D28) and
/// the time-dependent case is real even though this test cannot reach it.
#[test]
fn a_neighbour_names_its_status_the_way_the_detail_does() {
    let e = engine();
    let upstream = add(&e, "not yet", json!({ "wait": "in 30 days" }));
    let downstream = add(&e, "the dependent", json!({}));
    call(
        &e,
        "dependency.add",
        json!({ "ref": downstream, "depends_on": upstream }),
    )
    .expect("dep");

    let dep = &brief(&e, downstream)["neighbourhood"]["depends_on"][0];
    assert_eq!(dep["status"], "backlog");
    let got = call(&e, "task.get", json!({ "ref": upstream })).expect("task.get");
    assert_eq!(
        dep["status"], got["status"],
        "the brief and the detail name one row the same way"
    );
}

// ---- the derived memory query ---------------------------------------------

#[test]
fn a_task_word_that_matches_one_document_finds_it() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({
            "title": "Idempotency keys on payments",
            "body": "Every payment write carries an idempotency key; retries must not double-charge.",
            "source": "docs/adr/014.md"
        }),
    )
    .expect("doc");
    // Five words, only one of which the document contains. Handed to
    // `memory.search` as a plain query this is five ANDed phrases and comes
    // back empty — which is the failure D136 exists to remove, and it is
    // silent: an empty result is byte-identical to a store with nothing in it.
    let t = add(&e, "Audit the payments retry path", json!({}));

    let out = brief(&e, t);
    assert_eq!(
        out["memory"]["count"], 1,
        "one shared word is a hit: {}",
        out["memory"]
    );
    assert_eq!(hit_titles(&out), ["Idempotency keys on payments"]);
}

#[test]
fn the_expression_that_ran_is_named() {
    let e = engine();
    let t = add(&e, "Audit the payments retry path", json!({}));
    let out = brief(&e, t);
    let matched = out["memory"]["matched"]
        .as_str()
        .expect("the derived expression is reported, never left to be guessed");
    assert!(
        matched.contains("payments"),
        "the task's own words are in it: {matched}"
    );
    assert!(
        matched.contains(" OR "),
        "a derived query is a disjunction — ANDing a title's words answers \
         `count: 0` on a store that holds the document: {matched}"
    );
}

#[test]
fn tags_and_the_project_join_the_title_in_the_query() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "checkout" })).expect("project");
    call(
        &e,
        "memory.add",
        json!({
            "title": "Webhook replay",
            "body": "Stripe replays webhooks; the handler must be idempotent.",
            "project": "checkout"
        }),
    )
    .expect("doc");
    // The title shares nothing with the document. The tag does.
    let t = add(
        &e,
        "Fix the thing",
        json!({ "project": "checkout", "tags": ["webhook"] }),
    );

    let out = brief(&e, t);
    assert_eq!(
        out["memory"]["count"], 1,
        "a tag is part of what the task is about: {}",
        out["memory"]
    );
}

#[test]
fn memory_is_scoped_to_the_tasks_project_and_says_which() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "alpha" })).expect("alpha");
    call(&e, "project.create", json!({ "name": "beta" })).expect("beta");
    for p in ["alpha", "beta"] {
        call(
            &e,
            "memory.add",
            json!({
                "title": format!("{p} retries"),
                "body": "retries are bounded and logged",
                "project": p
            }),
        )
        .expect("doc");
    }
    let t = add(&e, "Bound the retries", json!({ "project": "alpha" }));

    let out = brief(&e, t);
    assert_eq!(hit_titles(&out), ["alpha retries"], "beta is out of scope");
    assert_eq!(
        out["memory"]["project"], "alpha",
        "the scope it applied is reported, like the expression is"
    );
}

/// D168: a task with NO project runs no project filter at all — store-wide,
/// the same answer `memory.search` itself gives when no `project` is named.
/// There is no scope here to widen FROM, so narrowing to "unscoped only"
/// would silently answer a different, narrower question than the one asked.
#[test]
fn a_task_with_no_project_searches_store_wide_like_memory_search_does() {
    let e = engine();
    // No `project.create` here on purpose: the first one created becomes the
    // store's DEFAULT project (gap fix A1) and `add`'s `t` below would
    // silently inherit it, testing "task in the default project" instead of
    // "task with no project at all". `memory.add` does not itself require a
    // project to be live, so the docs can name one without it existing.
    for p in ["alpha", "beta"] {
        call(
            &e,
            "memory.add",
            json!({
                "title": format!("{p} retries"),
                "body": "retries are bounded and logged",
                "project": p
            }),
        )
        .expect("doc");
    }
    let t = add(&e, "Bound the retries", json!({}));

    let out = brief(&e, t);
    let mut titles = hit_titles(&out);
    titles.sort();
    assert_eq!(
        titles,
        ["alpha retries", "beta retries"],
        "no project means no filter, the same as an unscoped `memory.search`: {}",
        out["memory"]
    );
    assert_eq!(out["memory"]["project"], Value::Null);
}

/// A document with no project is GLOBAL knowledge, and a project scope that
/// hides it hides exactly the conventions the reader imported.
///
/// `tasqx memory import docs/` — the documented way to feed a store your ADRs —
/// sets no project on anything it imports. Scoped strictly to the task's own
/// project, a brief for any task in a project would therefore never surface a
/// single imported document, which is most of what the memory half exists to
/// find. This is not the widening D136 refuses: that one is a FALLBACK, a
/// second search whose existence depends on the first being empty and which the
/// caller cannot see. This is one scope, applied always, that means "this
/// project's knowledge and the knowledge belonging to no project".
#[test]
fn a_document_belonging_to_no_project_is_global_and_reaches_every_brief() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "checkout" })).expect("project");
    call(
        &e,
        "memory.add",
        json!({
            "title": "Idempotency on payment retries",
            "body": "Every payment write carries an idempotency key; the key is the request id.",
            "source": "docs/adr/014.md"
        }),
    )
    .expect("an imported ADR carries no project");
    let t = add(
        &e,
        "Audit the payments retry path",
        json!({ "project": "checkout" }),
    );

    let out = brief(&e, t);
    assert_eq!(
        hit_titles(&out),
        ["Idempotency on payment retries"],
        "an unprojected doc is global, not invisible: {}",
        out["memory"]
    );
}

#[test]
fn an_empty_scoped_result_does_not_silently_widen() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "alpha" })).expect("alpha");
    call(&e, "project.create", json!({ "name": "beta" })).expect("beta");
    call(
        &e,
        "memory.add",
        json!({ "title": "beta retries", "body": "retries are bounded", "project": "beta" }),
    )
    .expect("doc");
    let t = add(&e, "Bound the retries", json!({ "project": "alpha" }));

    let out = brief(&e, t);
    assert_eq!(
        out["memory"]["count"], 0,
        "falling back to an unscoped search would make the result depend on a \
         branch the caller cannot see: {}",
        out["memory"]
    );
    assert_eq!(out["memory"]["project"], "alpha");
}

/// #607: a brief on `tasqx brief 51` (the demo store) once returned three of
/// five hits as the task's own annotations, verbatim, echoed back beside a
/// doc that belonged to a DIFFERENT project the derived query happened to
/// share words with. This pins the repro from that finding: a cross-project
/// doc is out of scope, the task's own note is not knowledge FOUND for it,
/// and the one hit that survives both cuts names its project (D168).
#[test]
fn a_cross_project_doc_and_the_tasks_own_note_are_both_excluded_and_a_surviving_hit_names_its_project(
) {
    let e = engine();
    call(&e, "project.create", json!({ "name": "ledger" })).expect("ledger");
    call(&e, "project.create", json!({ "name": "checkout" })).expect("checkout");
    // A different project, sharing the derived query's words — must stay out
    // of scope exactly as `memory_is_scoped_to_the_tasks_project_and_says_which`
    // already pins, pinned again here beside the other two cuts.
    call(
        &e,
        "memory.add",
        json!({
            "title": "Checkout reconciliation playbook",
            "body": "Reconcile the ledger balances against the checkout provider.",
            "project": "checkout",
        }),
    )
    .expect("cross-project doc");
    // Unscoped — global knowledge, must reach the brief (D136's
    // `include_unscoped` shape) and must say its project is null.
    call(
        &e,
        "memory.add",
        json!({
            "title": "Ledger reconciliation runbook",
            "body": "Reconcile the ledger balances at close of business.",
        }),
    )
    .expect("global doc");

    let t = add(
        &e,
        "Reconcile the ledger balances",
        json!({ "project": "ledger" }),
    );
    call(
        &e,
        "annotation.add",
        json!({ "ref": t, "body": "Reconciled the ledger balances by hand, this once." }),
    )
    .expect("own annotation");

    let out = brief(&e, t);
    assert_eq!(
        hit_titles(&out),
        ["Ledger reconciliation runbook"],
        "the cross-project doc and the task's own note must both be gone: {}",
        out["memory"]
    );
    assert_eq!(
        out["memory"]["hits"][0]["project"],
        Value::Null,
        "the surviving hit is global knowledge, and says so: {}",
        out["memory"]
    );
}

/// #607: `memory.search` has no notion of "the caller's own task" to
/// exclude, so the filter lives in the brief — the one caller that has a
/// task to exclude — and must not blanket-drop every annotation: a sibling's
/// note is still knowledge FOUND for this task.
#[test]
fn a_tasks_own_annotation_is_excluded_but_a_siblings_still_shows() {
    let e = engine();
    let past = add(&e, "the earlier task", json!({}));
    call(
        &e,
        "annotation.add",
        json!({ "ref": past, "body": "the migration needs the backfill run before the ALTER" }),
    )
    .expect("sibling's note");
    let t = add(&e, "Write the migration", json!({}));
    call(
        &e,
        "annotation.add",
        json!({ "ref": t, "body": "the migration plan: backfill, then ALTER, then verify" }),
    )
    .expect("own note");

    let out = brief(&e, t);
    let hit_sources: Vec<String> = out["memory"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["source"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        hit_sources.contains(&format!("task:#{past}")),
        "a sibling's annotation still counts as knowledge found: {:?}",
        out["memory"]
    );
    assert!(
        !hit_sources.contains(&format!("task:#{t}")),
        "the task's OWN annotation must not be echoed back as a hit: {:?}",
        out["memory"]
    );
}

/// Review finding on #657: excluding the task's own notes AFTER the page's
/// own `LIMIT` already ran undercounts the page itself, not only `total` — a
/// task whose own notes outrank a sibling's can fill the SQL-level page with
/// notes the post-hoc filter then discards, leaving a page with fewer hits
/// than `memory_limit` and the sibling's ruling nowhere on it, even though it
/// matched. The own notes here are packed with the derived query's words,
/// repeated, so bm25 ranks them ahead of the sibling's one-word mention —
/// exactly the shape that let the old post-filter cut the sibling at the
/// query's own `LIMIT` before it ever got a chance to drop the (better
/// ranked) notes that do not belong on the page at all.
#[test]
fn a_sibling_note_the_own_notes_outrank_still_fills_the_page_once_they_are_excluded_at_the_query() {
    let e = engine();
    let t = add(&e, "Reconcile the ledger balances", json!({}));
    call(
        &e,
        "annotation.add",
        json!({
            "ref": t,
            "body": "reconcile reconcile reconcile ledger ledger ledger balances balances \
                     balances every single time without fail",
        }),
    )
    .expect("own note, packed to outrank the sibling");
    let sibling = add(&e, "the earlier task", json!({}));
    call(
        &e,
        "annotation.add",
        json!({ "ref": sibling, "body": "a passing mention of the ledger" }),
    )
    .expect("sibling's note, ranked to lose on bm25 alone");

    let out = call(&e, "task.brief", json!({ "ref": t, "memory_limit": 1 })).expect("task.brief");
    let hit_sources: Vec<String> = out["memory"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["source"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        hit_sources,
        vec![format!("task:#{sibling}")],
        "the one slot must go to the sibling's ruling — cutting the task's \
         own (better-ranked) note has to happen in the query the LIMIT reads, \
         not after: {}",
        out["memory"]
    );
    assert_eq!(
        out["memory"]["annotations_total"], 1,
        "the own note must not count toward what a wider page would show \
         either: {}",
        out["memory"]
    );
}

/// The task's own notes are dropped before D147's counters run, so
/// `annotations_total` still answers "how many of this kind a wider page of
/// this query would show" — over the rows actually worth a slot, not the raw
/// MATCH count the self-notes would otherwise inflate.
#[test]
fn annotations_total_drops_by_the_self_notes_the_filter_removed() {
    let e = engine();
    let t = add(&e, "Reconcile the ledger balances", json!({}));
    for body in [
        "Reconcile the ledger balances, pass one",
        "Reconcile the ledger balances, pass two",
    ] {
        call(&e, "annotation.add", json!({ "ref": t, "body": body })).expect("own note");
    }
    let sibling = add(&e, "the earlier task", json!({}));
    call(
        &e,
        "annotation.add",
        json!({ "ref": sibling, "body": "reconcile the ledger balances too" }),
    )
    .expect("sibling's note");

    let out = brief(&e, t);
    assert_eq!(
        out["memory"]["annotations_total"], 1,
        "both self-notes are dropped before the count, leaving only the sibling's: {}",
        out["memory"]
    );
}

#[test]
fn annotations_are_searched_beside_documents() {
    let e = engine();
    let past = add(&e, "the earlier task", json!({}));
    call(
        &e,
        "annotation.add",
        json!({ "ref": past, "body": "the migration needs the backfill run before the ALTER" }),
    )
    .expect("note");
    let t = add(&e, "Write the migration", json!({}));

    let out = brief(&e, t);
    let kinds: Vec<&str> = out["memory"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"annotation"),
        "completing tasks well is half the index (D41): {}",
        out["memory"]
    );
}

/// D154: an omitted `memory_limit` is FIVE hits, not `memory.search`'s ten.
///
/// The number lives in the engine, so the CLI's `tasqx brief` and the MCP tool
/// answer the same page — and it is smaller than a search's because this query
/// was derived from the task's own words rather than asked by anybody. An
/// explicit limit is untouched by the change, which is what the test below
/// this one pins.
#[test]
fn the_default_memory_page_is_five_hits() {
    let e = engine();
    for n in 0..8 {
        call(
            &e,
            "memory.add",
            json!({ "title": format!("retries {n}"), "body": "retries are bounded and logged" }),
        )
        .expect("doc");
    }
    let t = add(&e, "Bound the retries", json!({}));

    let m = &brief(&e, t)["memory"];
    assert_eq!(m["count"], 5, "the default page: {m}");
    assert_eq!(m["hits"].as_array().expect("hits").len(), 5);
    assert_eq!(m["total"], 8, "and it says what it withheld: {m}");
    assert_eq!(m["has_more"], true);
}

#[test]
fn memory_limit_bounds_the_hits() {
    let e = engine();
    for n in 0..5 {
        call(
            &e,
            "memory.add",
            json!({ "title": format!("retries {n}"), "body": "retries are bounded and logged" }),
        )
        .expect("doc");
    }
    let t = add(&e, "Bound the retries", json!({}));

    let out = call(&e, "task.brief", json!({ "ref": t, "memory_limit": 2 })).expect("brief");
    assert_eq!(out["memory"]["count"], 2);
    assert_eq!(
        out["memory"]["total"], 5,
        "how much it withheld is reported"
    );
    assert_eq!(out["memory"]["has_more"], true);
}

#[test]
fn a_title_with_nothing_searchable_in_it_answers_empty_rather_than_failing() {
    let e = engine();
    // Function words only, no tags, no project: there is no query to derive.
    // An empty FTS expression is a syntax error, so this must be recognised
    // before the search rather than reported as one.
    let t = add(&e, "the of and to", json!({}));
    let out = brief(&e, t);
    assert_eq!(out["memory"]["count"], 0);
    assert!(
        out["memory"]["matched"].is_null(),
        "no expression ran, and saying so is not the same as saying none matched"
    );
    // The reservation fields are on THIS answer too (D147). A field that is
    // present on one branch and absent on another is a field every reader has
    // to test for, and the early return is the branch a reader reaches least
    // often and debugs hardest.
    for key in ["reserved_docs", "docs_total", "annotations_total"] {
        assert_eq!(
            out["memory"][key].as_i64(),
            Some(0),
            "`{key}` is an integer zero here, not absent: {}",
            out["memory"]
        );
    }
}

#[test]
fn a_quote_in_a_title_does_not_break_the_derived_expression() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "Quoting", "body": "the scanner has one quoting rule" }),
    )
    .expect("doc");
    let t = add(&e, r#"Fix the "quoting" rule"#, json!({}));
    // Built by tasqx, so it is well-formed by construction — but the words are
    // the caller's and a bare `"` would end a phrase early and leave the
    // expression unparseable.
    let out = brief(&e, t);
    assert_eq!(out["memory"]["count"], 1, "{}", out["memory"]);
}

// ---- the envelope ---------------------------------------------------------

#[test]
fn an_unknown_ref_is_not_found() {
    let e = engine();
    let err = call(&e, "task.brief", json!({ "ref": 404 })).expect_err("no such task");
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn a_param_the_method_does_not_read_is_refused_at_dispatch() {
    let e = engine();
    let t = add(&e, "a task", json!({}));
    let err = call(&e, "task.brief", json!({ "ref": t, "explain": true }))
        .expect_err("D31: the params gate refuses a key the method does not accept");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn the_brief_writes_nothing() {
    let e = engine();
    let t = add(&e, "a task", json!({}));
    let count = |e: &Engine| {
        call(e, "event.list", json!({})).expect("events")["events"]
            .as_array()
            .expect("events array")
            .len()
    };
    let before = count(&e);
    brief(&e, t);
    assert_eq!(before, count(&e), "a read appends nothing to the log");
}

// ---- the reserved half (D147) ---------------------------------------------

/// The case the reservation exists for: a ruling written once, in its own
/// words, against a project's worth of sibling notes written in the task's.
///
/// bm25 alone answers this wrong and answers it silently. The derived query is
/// the task's own title words, tags and project leaf; every sibling task in one
/// project shares exactly that vocabulary, and their annotations are many and
/// long, so the page fills with them and the one document that RULED on the
/// subject never reaches the reader. A store whose rulings cannot be retrieved
/// is a store that is write-only.
#[test]
fn a_ruling_survives_a_hundred_sibling_notes_that_share_the_tasks_words() {
    let e = engine();
    call(&e, "project.create", json!({ "name": "tasqx" })).expect("project");
    // Written once, and only two of its words are the task's.
    call(
        &e,
        "memory.add",
        json!({
            "title": "Retry audit ruling",
            "body": "every retry is bounded",
            "project": "tasqx",
            "source": "DESIGN.md"
        }),
    )
    .expect("the ruling");
    // Twenty siblings, five notes each: a hundred annotations repeating the
    // task's vocabulary, which is what an ordinary project looks like after a
    // month of work.
    for i in 0..20 {
        let sibling = add(
            &e,
            &format!("audit the retry path tests {i}"),
            json!({ "project": "tasqx" }),
        );
        for j in 0..5 {
            call(
                &e,
                "annotation.add",
                json!({
                    "ref": sibling,
                    "body": format!(
                        "audit {j}: the retry path tests audit the retry path again, \
                         retry path audit, tests audit retry path"
                    )
                }),
            )
            .expect("sibling note");
        }
    }
    let t = add(
        &e,
        "Audit the retry path tests",
        json!({ "project": "tasqx" }),
    );

    let out = brief(&e, t);
    let m = &out["memory"];
    assert!(
        hit_titles(&out).contains(&"Retry audit ruling".to_string()),
        "the ruling is on the page, not buried under its siblings' notes: {m}"
    );
    let kinds: Vec<&str> = m["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["kind"].as_str())
        .collect();
    assert_eq!(kinds[0], "doc", "docs are listed first: {m}");
    assert_eq!(
        m["reserved_docs"], 3,
        "half of the default limit, rounded up (D154 moved it to five): {m}"
    );
    assert_eq!(m["docs_total"], 1);
    assert!(
        m["annotations_total"].as_i64().expect("annotations_total") >= 100,
        "the notes really are the crowd this test claims: {m}"
    );
    assert_eq!(m["count"], 5);
    assert_eq!(m["has_more"], true);
    assert_eq!(
        m["total"].as_i64().expect("total"),
        m["docs_total"].as_i64().expect("docs_total")
            + m["annotations_total"].as_i64().expect("annotations_total"),
        "one total over both kinds, so `has_more` means what it says: {m}"
    );
}

/// The order and the fill: docs take the front of the page in their own bm25
/// order, and annotations take what is left — including the doc slots no doc
/// claimed.
#[test]
fn docs_come_first_and_annotations_fill_what_docs_leave() {
    let e = engine();
    for n in 0..2 {
        call(
            &e,
            "memory.add",
            json!({ "title": format!("retry ruling {n}"), "body": "retries are bounded and logged" }),
        )
        .expect("doc");
    }
    for n in 0..20 {
        let sibling = add(&e, &format!("earlier retry work {n}"), json!({}));
        call(
            &e,
            "annotation.add",
            json!({ "ref": sibling, "body": "the retries were bounded here too" }),
        )
        .expect("note");
    }
    let t = add(&e, "Bound the retries", json!({}));

    let m = &brief(&e, t)["memory"];
    let kinds: Vec<&str> = m["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        ["doc", "doc"]
            .into_iter()
            .chain(std::iter::repeat_n("annotation", 3))
            .collect::<Vec<_>>(),
        "two docs first, then annotations for the rest of the page — including the \
         third doc slot no doc claimed: {m}"
    );
    assert_eq!(m["reserved_docs"], 3, "reserved, not spent: {m}");
    assert_eq!(m["docs_total"], 2);
    assert_eq!(m["annotations_total"], 20);
}

/// The mirror: a reservation that HELD five slots for docs when eight docs
/// matched would be a cap, not a reservation. Either kind takes the other's
/// unused slots.
#[test]
fn annotations_take_the_doc_slots_docs_do_not_use() {
    let e = engine();
    for n in 0..8 {
        call(
            &e,
            "memory.add",
            json!({ "title": format!("retry ruling {n}"), "body": "retries are bounded and logged" }),
        )
        .expect("doc");
    }
    for n in 0..2 {
        let sibling = add(&e, &format!("earlier retry work {n}"), json!({}));
        call(
            &e,
            "annotation.add",
            json!({ "ref": sibling, "body": "the retries were bounded here too" }),
        )
        .expect("note");
    }
    let t = add(&e, "Bound the retries", json!({}));

    let out = call(&e, "task.brief", json!({ "ref": t, "memory_limit": 10 })).expect("brief");
    let m = &out["memory"];
    let kinds: Vec<&str> = m["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|h| h["kind"].as_str())
        .collect();
    assert_eq!(kinds.iter().filter(|k| **k == "doc").count(), 8, "{m}");
    assert_eq!(
        kinds.iter().filter(|k| **k == "annotation").count(),
        2,
        "{m}"
    );
    assert_eq!(m["count"], 10);
    assert_eq!(m["reserved_docs"], 5);
}

/// `memory_limit: 0` is the floor D66's bisection actually reaches, so it is
/// arithmetic on `0` in two places and must answer rather than panic.
#[test]
fn memory_limit_zero_answers_no_hits_and_no_panic() {
    let e = engine();
    call(
        &e,
        "memory.add",
        json!({ "title": "retry ruling", "body": "retries are bounded and logged" }),
    )
    .expect("doc");
    let sibling = add(&e, "earlier retry work", json!({}));
    call(
        &e,
        "annotation.add",
        json!({ "ref": sibling, "body": "the retries were bounded here too" }),
    )
    .expect("note");
    let t = add(&e, "Bound the retries", json!({}));

    let out = call(&e, "task.brief", json!({ "ref": t, "memory_limit": 0 })).expect("brief");
    let m = &out["memory"];
    assert_eq!(m["count"], 0);
    assert_eq!(m["hits"].as_array().expect("hits").len(), 0);
    assert_eq!(m["reserved_docs"], 0, "half of nothing is nothing: {m}");
    assert_eq!(m["docs_total"], 1);
    assert_eq!(m["annotations_total"], 1);
    assert_eq!(m["total"], 2);
    assert_eq!(
        m["has_more"], true,
        "a page of zero over a store of two withheld both: {m}"
    );
}
