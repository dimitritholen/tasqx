//! Tests for the additive increment (DESIGN §4 method catalogue, §12-D8 filter
//! grammar, real dependency/blocked logic, default project, instant `due`
//! comparison, report.summary, and store export/import round-trip).

use jiff::{SignedDuration, Timestamp};
use serde_json::{json, Value};
use tasqx_core::{Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

/// An RFC3339 (UTC) instant `h` hours from now.
fn plus_hours(h: i64) -> String {
    (Timestamp::now() + SignedDuration::from_hours(h)).to_string()
}

fn count(r: &serde_json::Value) -> i64 {
    r["count"].as_i64().unwrap()
}

// ---- A1: default-project inheritance ----------------------------------------

#[test]
fn init_sets_default_project_and_task_add_inherits() {
    let e = engine();
    let created = e.project_create(&json!({ "name": "work.x" })).unwrap();
    assert_eq!(created["default"], true);
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work.x"));

    // A bare add inherits the default.
    let added = e.task_add(&json!({ "title": "inherits" })).unwrap();
    let got = e
        .task_get(&json!({ "ref": added["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["project"], "work.x");

    // An explicit project always wins (and must exist - D23).
    e.project_create(&json!({ "name": "other" })).unwrap();
    let a2 = e
        .task_add(&json!({ "title": "explicit", "project": "other" }))
        .unwrap();
    let g2 = e
        .task_get(&json!({ "ref": a2["short_id"].clone() }))
        .unwrap();
    assert_eq!(g2["project"], "other");

    // Exposed via capabilities.
    let caps = e.capabilities().unwrap();
    assert_eq!(caps["default_project"], "work.x");
}

// ---- A2: due comparison is by INSTANT, not lexicographic ---------------------

#[test]
fn due_before_after_compare_as_instants() {
    let e = engine();
    e.task_add(&json!({ "title": "due tomorrow", "due": plus_hours(24) }))
        .unwrap();

    let next_week = plus_hours(24 * 7);
    let yesterday = plus_hours(-24);

    // Tomorrow is before next week => matches.
    let r = e
        .task_list(&json!({ "filter": format!("due.before:{next_week}") }))
        .unwrap();
    assert_eq!(count(&r), 1);

    // Tomorrow is NOT before yesterday => no match.
    let r = e
        .task_list(&json!({ "filter": format!("due.before:{yesterday}") }))
        .unwrap();
    assert_eq!(count(&r), 0);

    // due.after is the mirror.
    let r = e
        .task_list(&json!({ "filter": format!("due.after:{yesterday}") }))
        .unwrap();
    assert_eq!(count(&r), 1);
}

#[test]
fn due_comparison_respects_timezone_offset_not_string_order() {
    // The classic gotcha: a due instant written in +12:00 is lexicographically
    // *after* a Z bound but temporally *before* it. Instant comparison must
    // match due.before; a naive string `<` would not.
    let e = engine();
    // 2026-07-16T06:00:00+12:00 == 2026-07-15T18:00:00Z
    e.task_add(&json!({ "title": "offset", "due": "2026-07-16T06:00:00+12:00" }))
        .unwrap();

    let r = e
        .task_list(&json!({ "filter": "due.before:2026-07-15T20:00:00Z" }))
        .unwrap();
    assert_eq!(
        count(&r),
        1,
        "18:00Z is before 20:00Z regardless of string form"
    );

    let r = e
        .task_list(&json!({ "filter": "due.before:2026-07-15T12:00:00Z" }))
        .unwrap();
    assert_eq!(count(&r), 0, "18:00Z is not before 12:00Z");
}

// ---- C: real dependency / blocked logic -------------------------------------

#[test]
fn blocked_excluded_from_working_then_visible_and_unblocked() {
    let e = engine();
    let a = e.task_add(&json!({ "title": "A depends" })).unwrap(); // short 1
    let b = e.task_add(&json!({ "title": "B blocker" })).unwrap(); // short 2
    let (asid, bsid) = (a["short_id"].clone(), b["short_id"].clone());

    // A depends on B.
    let dep = e
        .dependency_add(&json!({ "ref": asid, "depends_on": bsid }))
        .unwrap();
    assert_eq!(dep["blocked"], true);

    // @working excludes the blocked task A; only B shows.
    let working = e.task_list(&json!({ "filter": "@working" })).unwrap();
    assert_eq!(count(&working), 1);
    assert_eq!(working["tasks"][0]["short_id"], bsid);

    // status:blocked (and +blocked) surface A.
    let blocked = e.task_list(&json!({ "filter": "status:blocked" })).unwrap();
    assert_eq!(count(&blocked), 1);
    assert_eq!(blocked["tasks"][0]["short_id"], asid);
    let blocked2 = e.task_list(&json!({ "filter": "+blocked" })).unwrap();
    assert_eq!(count(&blocked2), 1);

    // task.get reports the flag.
    assert_eq!(
        e.task_get(&json!({ "ref": asid })).unwrap()["blocked"],
        true
    );

    // Completing B unblocks A (reported by task.done).
    let done = e.task_done(&json!({ "ref": bsid })).unwrap();
    let unblocked = done["unblocked"].as_array().unwrap();
    assert!(unblocked.contains(&asid), "A's short_id in unblocked");

    // A is no longer blocked and now appears in @working.
    assert_eq!(
        e.task_get(&json!({ "ref": asid })).unwrap()["blocked"],
        false
    );
    let working2 = e.task_list(&json!({ "filter": "@working" })).unwrap();
    assert_eq!(count(&working2), 1);
    assert_eq!(working2["tasks"][0]["short_id"], asid);
}

/// `compute_unblocked` restricts its dependent scan to *open* tasks, and until
/// this test nothing exercised that restriction — widening the status set there
/// left the whole suite green. It matters because `unblocked` is what the CLI
/// prints as "now actionable" and what an MCP agent reads to decide what to pick
/// up next: naming a task that is already finished or abandoned sends a human or
/// an agent back to work that no longer exists. `done` and `cancelled` are
/// checked separately because they reach terminal state by different code paths.
#[test]
fn a_terminal_dependent_is_never_announced_as_unblocked() {
    use tasqx_core::types::Status;
    // Derived from `Status::ALL`, not a hand-written ["done", "cancelled"]: the
    // clause under test is itself derived from `is_terminal`, so a hardcoded
    // loop would keep checking two cases while production silently followed a
    // third variant — the parallel-list-one-layer-up mistake that let an earlier
    // guard in this repo ship a false promise.
    for finish in Status::ALL.into_iter().filter(|s| s.is_terminal()) {
        let e = engine();
        let dependent = e.task_add(&json!({ "title": "dependent" })).unwrap()["short_id"].clone();
        let blocker = e.task_add(&json!({ "title": "blocker" })).unwrap()["short_id"].clone();
        e.dependency_add(&json!({ "ref": dependent, "depends_on": blocker }))
            .unwrap();

        // Take the dependent itself out of play *before* its blocker resolves.
        // Exhaustive on purpose: a new terminal status fails to compile here
        // until someone wires up the transition that reaches it.
        match finish {
            Status::Done => e.task_done(&json!({ "ref": dependent })).unwrap(),
            Status::Cancelled => e.task_cancel(&json!({ "ref": dependent })).unwrap(),
            Status::Backlog | Status::Pending | Status::Active => {
                unreachable!("filtered to terminal statuses")
            }
        };

        let res = e.task_done(&json!({ "ref": blocker })).unwrap();
        assert!(
            !res["unblocked"].as_array().unwrap().contains(&dependent),
            "a {} dependent must not be reported as newly actionable",
            finish.as_str()
        );
    }
}

/// The inclusion side of the same clause, and the reason it needed its own test:
/// every guard around `compute_unblocked` was exclusion-only — they proved
/// `done`/`cancelled` dependents stay OUT, and only ever used pending dependents
/// (the default status). Nothing asserted a `backlog` dependent is let IN, so
/// dropping `'backlog'` from the generated `IN (…)` list left all 292 tests
/// green while a waiting task whose blocker finished would silently never be
/// announced as actionable.
#[test]
fn a_backlog_dependent_is_still_announced_as_unblocked() {
    let e = engine();
    // `wait` in the future is what parks a task in `backlog`.
    let dependent = e
        .task_add(&json!({ "title": "waiting", "wait": "2999-01-01T00:00:00Z" }))
        .unwrap();
    assert_eq!(
        dependent["status"], "backlog",
        "fixture must actually be backlog"
    );
    let dependent = dependent["short_id"].clone();
    let blocker = e.task_add(&json!({ "title": "blocker" })).unwrap()["short_id"].clone();
    e.dependency_add(&json!({ "ref": dependent, "depends_on": blocker }))
        .unwrap();

    let res = e.task_done(&json!({ "ref": blocker })).unwrap();
    assert!(
        res["unblocked"].as_array().unwrap().contains(&dependent),
        "a backlog dependent must still be announced when its blocker resolves"
    );
}

#[test]
fn dependency_self_and_cycle_are_conflicts() {
    let e = engine();
    let a = e.task_add(&json!({ "title": "A" })).unwrap();
    let b = e.task_add(&json!({ "title": "B" })).unwrap();
    let (asid, bsid) = (a["short_id"].clone(), b["short_id"].clone());

    // Self-dependency.
    let self_err = e
        .dependency_add(&json!({ "ref": asid, "depends_on": asid }))
        .unwrap_err();
    assert_eq!(self_err.code, ErrorCode::Conflict);

    // A depends on B is fine; B depends on A would cycle.
    e.dependency_add(&json!({ "ref": asid, "depends_on": bsid }))
        .unwrap();
    let cycle_err = e
        .dependency_add(&json!({ "ref": bsid, "depends_on": asid }))
        .unwrap_err();
    assert_eq!(cycle_err.code, ErrorCode::Conflict);
}

#[test]
fn cancelled_dependency_releases_dependent() {
    // DESIGN §3 + D11: a dependency is *resolved* when done OR cancelled. A
    // cancelled blocker will never complete, so it must release its dependents
    // rather than block them forever.
    let e = engine();
    let a = e.task_add(&json!({ "title": "A depends" })).unwrap()["short_id"].clone();
    let b = e.task_add(&json!({ "title": "B blocker" })).unwrap()["short_id"].clone();
    e.dependency_add(&json!({ "ref": a, "depends_on": b }))
        .unwrap();
    assert_eq!(e.task_get(&json!({ "ref": a })).unwrap()["blocked"], true);

    // Cancel the blocker: A becomes actionable, and cancel reports the cascade.
    let res = e.task_cancel(&json!({ "ref": b })).unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": a })).unwrap()["blocked"],
        false,
        "a cancelled blocker releases its dependent (D11)"
    );
    assert!(
        res["unblocked"].as_array().unwrap().contains(&a),
        "task.cancel surfaces the unblock cascade"
    );
    // And A now appears in @working.
    let working = e.task_list(&json!({ "filter": "@working" })).unwrap();
    assert!(working["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["short_id"] == a));

    // Completing the blocker instead also clears it.
    let e2 = engine();
    let a2 = e2.task_add(&json!({ "title": "A2" })).unwrap()["short_id"].clone();
    let b2 = e2.task_add(&json!({ "title": "B2" })).unwrap()["short_id"].clone();
    e2.dependency_add(&json!({ "ref": a2, "depends_on": b2 }))
        .unwrap();
    e2.task_done(&json!({ "ref": b2 })).unwrap();
    assert_eq!(
        e2.task_get(&json!({ "ref": a2 })).unwrap()["blocked"],
        false
    );
}

#[test]
fn unknown_filter_token_is_rejected_rather_than_widening_the_result() {
    // The inverse of what this test used to pin. Unknown tokens were the
    // always-true term, so `staus:pending` (missing 't') returned BOTH tasks —
    // a typo in a filter handed back more rows than the correct filter would,
    // and nothing said so. A filter exists to narrow; the one failure mode it
    // must not have is silently widening.
    let e = engine();
    e.task_add(&json!({ "title": "one" })).unwrap();
    e.task_add(&json!({ "title": "two" })).unwrap();

    for bogus in ["staus:pending", "totally_unknown_token"] {
        let err = e.task_list(&json!({ "filter": bogus })).expect_err(bogus);
        assert_eq!(
            err.code,
            ErrorCode::BadRequest,
            "{bogus} must be a bad_request"
        );
        assert!(
            err.message.contains(bogus),
            "the message must name the token: {}",
            err.message
        );
    }

    // The empty filter is NOT a malformed one: no filter means no filtering,
    // and breaking that would break every unfiltered list in the tool.
    let r = e.task_list(&json!({ "filter": "" })).unwrap();
    assert_eq!(count(&r), 2);
    let r = e.task_list(&json!({})).unwrap();
    assert_eq!(count(&r), 2);

    // A valid filter still narrows, so the guard cannot be satisfied by a
    // parser that simply rejects everything.
    let r = e.task_list(&json!({ "filter": "status:pending" })).unwrap();
    assert_eq!(count(&r), 2);
    let r = e.task_list(&json!({ "filter": "status:done" })).unwrap();
    assert_eq!(count(&r), 0);
}

#[test]
fn a_dangling_operator_or_unbalanced_paren_is_rejected() {
    // Same silent-widening shape, reached through the grammar rather than a
    // token: `+api or` used to parse as `+api OR <always-true>`, i.e. every
    // task, and `(+api or +infra` dropped the missing paren without a word.
    let e = engine();
    e.task_add(&json!({ "title": "one" })).unwrap();

    for bad in ["+api or", "(+api or +infra", "+api)", "()"] {
        let err = e.task_list(&json!({ "filter": bad })).expect_err(bad);
        assert_eq!(
            err.code,
            ErrorCode::BadRequest,
            "{bad} must be a bad_request"
        );
    }
}

// ---- B: new method happy paths ----------------------------------------------

#[test]
fn task_cancel_closes_active_interval() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_start(&json!({ "ref": sid })).unwrap();

    let res = e.task_cancel(&json!({ "ref": sid })).unwrap();
    assert_eq!(res["status"], "cancelled");

    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    assert_eq!(got["status"], "cancelled");
    // Interval closed: no active tasks remain.
    let active: i64 = e
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE status='active'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active, 0);

    // Cancelling a terminal task is a conflict.
    let err = e.task_cancel(&json!({ "ref": sid })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[test]
fn task_reopen_from_done_and_cancelled() {
    let e = engine();
    let s1 = e.task_add(&json!({ "title": "done one" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": s1 })).unwrap();
    let re = e.task_reopen(&json!({ "ref": s1 })).unwrap();
    assert_eq!(re["status"], "pending");
    assert!(e.task_get(&json!({ "ref": s1 })).unwrap()["completed"].is_null());

    // Reopening a pending task is a conflict.
    let err = e.task_reopen(&json!({ "ref": s1 })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[test]
fn project_list_and_archive() {
    let e = engine();
    e.project_create(&json!({ "name": "p.one", "description": "first" }))
        .unwrap();
    e.project_create(&json!({ "name": "p.two" })).unwrap();

    let listed = e.project_list(&json!({})).unwrap();
    assert_eq!(count(&listed), 2);

    e.project_archive(&json!({ "name": "p.one" })).unwrap();

    // Archived hidden by default.
    let active = e.project_list(&json!({})).unwrap();
    assert_eq!(count(&active), 1);
    assert_eq!(active["projects"][0]["name"], "p.two");

    // include_archived surfaces both.
    let all = e
        .project_list(&json!({ "include_archived": true }))
        .unwrap();
    assert_eq!(count(&all), 2);

    // Archiving a missing project is not_found.
    let err = e.project_archive(&json!({ "name": "nope" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn annotation_add_and_get() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let res = e
        .annotation_add(&json!({ "ref": sid, "body": "a note" }))
        .unwrap();
    assert_eq!(res["annotation"]["body"], "a note");

    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    let anns = got["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["body"], "a note");
}

#[test]
fn dependency_remove_clears_blocked() {
    let e = engine();
    let a = e.task_add(&json!({ "title": "A" })).unwrap()["short_id"].clone();
    let b = e.task_add(&json!({ "title": "B" })).unwrap()["short_id"].clone();
    e.dependency_add(&json!({ "ref": a, "depends_on": b }))
        .unwrap();
    assert_eq!(e.task_get(&json!({ "ref": a })).unwrap()["blocked"], true);

    let res = e
        .dependency_remove(&json!({ "ref": a, "depends_on": b }))
        .unwrap();
    assert_eq!(res["blocked"], false);
    assert_eq!(res["depends_on"].as_array().unwrap().len(), 0);
}

#[test]
fn event_list_scoped_by_ref() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_start(&json!({ "ref": sid })).unwrap();
    e.task_done(&json!({ "ref": sid })).unwrap();

    let events = e.event_list(&json!({ "ref": sid })).unwrap();
    // add + start + done => 3 events on this task.
    assert_eq!(count(&events), 3);
    // Newest first (UUIDv7 ordering).
    assert_eq!(events["events"][0]["op"], "done");

    // Entity-scoped listing works too.
    let proj_events = e
        .event_list(&json!({ "entity": "task", "limit": 10 }))
        .unwrap();
    assert_eq!(count(&proj_events), 3);
}

/// `entity` is a **closed, compile-time** vocabulary — `Entity::ALL` — so a
/// value outside it is a caller error, not a query that legitimately found
/// nothing. It used to go straight into `WHERE entity = ?1`, so `entity: "tsak"`
/// returned `{count: 0, events: []}` at `ok: true`: an empty audit log is
/// exactly what a caller checking "did anything happen?" would read as an
/// answer, and it is indistinguishable from a store where nothing happened.
///
/// Same rule as `status:` in the filter grammar, and the same reason it does
/// *not* extend to `ref` — a task id is an open runtime set, and `ref` already
/// resolves it and returns `not_found`.
#[test]
fn event_list_refuses_an_entity_outside_the_closed_set() {
    let e = engine();
    e.task_add(&json!({ "title": "t" })).unwrap();

    // `""` is not in this list because D35 gave it its own test — see
    // `event_list_refuses_an_explicitly_empty_entity_instead_of_listing_everything`.
    // It was the deferred half of this decision: `util::opt_str` collapsed an
    // empty string to `None` for every optional string param, so `entity: ""`
    // meant "not given" and listed the whole log.
    for bogus in ["tsak", "Task", "tasks", "annotation"] {
        let err = e
            .event_list(&json!({ "entity": bogus }))
            .expect_err("an unknown entity must be refused, not answered with an empty log");
        assert_eq!(err.code, ErrorCode::BadRequest, "for entity {bogus:?}");
        assert!(err.message.contains(bogus), "must name the value: {err:?}");
        // Derived from the enum the writers use, so a third entity kind joins
        // this message the day `insert_event` can write it.
        for ent in tasqx_core::Entity::ALL {
            assert!(
                err.message.contains(ent.as_str()),
                "must list {:?} as accepted; got {:?}",
                ent.as_str(),
                err.message
            );
        }
    }

    // Every entity the writers can actually produce still lists cleanly.
    for ent in tasqx_core::Entity::ALL {
        e.event_list(&json!({ "entity": ent.as_str() }))
            .unwrap_or_else(|err| panic!("entity {:?} must be accepted: {err:?}", ent.as_str()));
    }
}

// ---- report.summary aggregation ---------------------------------------------

#[test]
fn report_summary_aggregates_count_est_and_overdue() {
    let e = engine();
    e.project_create(&json!({ "name": "P" })).unwrap(); // D23
    e.project_create(&json!({ "name": "Q" })).unwrap();
    e.task_add(
        &json!({ "title": "p1a", "project": "P", "estimate": "PT2H", "due": plus_hours(-48) }),
    )
    .unwrap();
    e.task_add(
        &json!({ "title": "p1b", "project": "P", "estimate": "PT3H", "due": plus_hours(48) }),
    )
    .unwrap();
    e.task_add(&json!({ "title": "q1", "project": "Q" }))
        .unwrap();

    let rep = e
        .report_summary(&json!({
            "group_by": "project",
            "metrics": ["count", "est_total", "overdue"]
        }))
        .unwrap();
    let groups = rep["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);

    let p = groups.iter().find(|g| g["project"] == "P").unwrap();
    assert_eq!(p["count"], 2);
    assert_eq!(p["est_total"], "PT5H"); // 2H + 3H
    assert_eq!(p["overdue"], 1); // only the past-due one

    let q = groups.iter().find(|g| g["project"] == "Q").unwrap();
    assert_eq!(q["count"], 1);
    assert_eq!(q["est_total"], "PT0S");
    assert_eq!(q["overdue"], 0);
}

/// Attach one token measurement to a task via `token.add`. Source/confidence
/// come from the closed vocabularies; the four bucket counts are the payload.
fn add_tokens(e: &Engine, r: &str, input: i64, output: i64, cache_read: i64, cache_creation: i64) {
    e.token_add(&json!({
        "ref": r,
        "tool": "claude-code",
        "source": "self-report",
        "confidence": "high",
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_tokens": cache_read,
        "cache_creation_tokens": cache_creation,
    }))
    .unwrap();
}

/// #19: a group's token metrics are the sum of the four buckets across every
/// measurement of every task in the group — many measurements per task, many
/// tasks per group — four separate fields, never a blend (D48a, closed for the
/// API by D50). Emitted as JSON integers, never ISO durations (that type is
/// frozen from day one).
#[test]
fn report_summary_sums_token_measurements_per_group() {
    let e = engine();
    e.project_create(&json!({ "name": "P" })).unwrap(); // D23
    e.task_add(&json!({ "title": "a", "project": "P" }))
        .unwrap(); // ref 1
    e.task_add(&json!({ "title": "b", "project": "P" }))
        .unwrap(); // ref 2
                   // Task 1 has two measurements, task 2 has one: the roll-up must cross both
                   // the per-task and the per-group boundary.
    add_tokens(&e, "1", 100, 10, 5, 1);
    add_tokens(&e, "1", 200, 20, 0, 2);
    add_tokens(&e, "2", 300, 30, 7, 0);

    let g = report(
        &e,
        json!({
            "group_by": "project",
            "metrics": [
                "tokens_in", "tokens_out", "tokens_cache_read",
                "tokens_cache_creation"
            ]
        }),
    );
    assert_eq!(g["tokens_in"], 600); // 100 + 200 + 300
    assert_eq!(g["tokens_out"], 60); // 10 + 20 + 30
    assert_eq!(g["tokens_cache_read"], 12); // 5 + 0 + 7
    assert_eq!(g["tokens_cache_creation"], 3); // 1 + 2 + 0
                                               // The JSON type is an integer, not a string — the type contract, not a
                                               // value spot-check, is what breaks a client if it ever regresses to
                                               // iso_duration.
    for m in [
        "tokens_in",
        "tokens_out",
        "tokens_cache_read",
        "tokens_cache_creation",
    ] {
        assert!(g[m].is_i64(), "{m} must be a JSON integer, got {}", g[m]);
    }

    // D50: `tokens_total` left the metric vocabulary. Asking for it must be a
    // refusal, not a silently absent column — the fail-loud rule the G1 cluster
    // already pinned for every other unknown metric.
    let err = e
        .report_summary(&json!({ "group_by": "project", "metrics": ["tokens_total"] }))
        .expect_err("tokens_total is no longer a metric (D50); it must be refused");
    assert_eq!(err.code, ErrorCode::BadRequest);
}

/// #19 + D24: token measurements attributed to a cancelled task were still
/// genuinely spent, but a report is an aggregation, so they must not inflate the
/// default roll-up any more than the task's own count does. `all:true` is the
/// one way to see abandoned spend.
#[test]
fn report_summary_token_metrics_follow_the_d24_cancelled_scope() {
    let e = engine();
    e.project_create(&json!({ "name": "P" })).unwrap(); // D23
    e.task_add(&json!({ "title": "keep", "project": "P" }))
        .unwrap(); // ref 1
    e.task_add(&json!({ "title": "drop", "project": "P" }))
        .unwrap(); // ref 2
    add_tokens(&e, "1", 0, 100, 0, 0);
    add_tokens(&e, "2", 0, 500, 0, 0); // spent, then abandoned
    e.task_cancel(&json!({ "ref": "2" })).unwrap();

    let g = report(
        &e,
        json!({ "group_by": "project", "metrics": ["tokens_out"] }),
    );
    assert_eq!(
        g["tokens_out"], 100,
        "cancelled work's tokens must stay out of the default roll-up (D24)"
    );

    let g = report(
        &e,
        json!({ "group_by": "project", "all": true, "metrics": ["tokens_out"] }),
    );
    assert_eq!(
        g["tokens_out"], 600,
        "all:true reveals the cancelled task's spend"
    );
}

// ---- D24: report aggregations exclude cancelled by default -------------------

/// Seed one project `P` with one task per interesting status, each carrying a
/// 1-hour estimate so `est_total` reads back as a count of included rows.
fn engine_with_one_task_per_status() -> Engine {
    let e = engine();
    e.project_create(&json!({ "name": "P" })).unwrap(); // D23
    for title in ["pend", "act", "fin", "dead"] {
        e.task_add(&json!({ "title": title, "project": "P", "estimate": "PT1H" }))
            .unwrap();
    }
    // short ids 1..4 in insertion order.
    e.task_start(&json!({ "ref": "2" })).unwrap();
    e.task_done(&json!({ "ref": "3" })).unwrap();
    e.task_cancel(&json!({ "ref": "4" })).unwrap();
    e
}

fn report(e: &Engine, params: serde_json::Value) -> serde_json::Value {
    let rep = e.report_summary(&params).unwrap();
    rep["groups"]
        .as_array()
        .unwrap()
        .first()
        .cloned()
        .unwrap_or(json!({}))
}

/// D24: cancelling is tasqx's only way to get rid of a task (there is no hard
/// delete, DESIGN §7), so before this rule every throwaway task inflated the
/// headline `count`/`est_total`/`tracked_total` forever. An unfiltered report
/// must not count abandoned work.
#[test]
fn report_summary_excludes_cancelled_by_default() {
    let e = engine_with_one_task_per_status();
    // `tracked_total` is deliberately absent: elapsed time is 0s in a fresh
    // fixture, so it would assert nothing here. Its D24 behaviour is pinned by
    // engine.rs::report_summary_tracked_total_drops_cancelled_but_keeps_done,
    // which forges `tracked_seconds` directly.
    let g = report(&e, json!({ "metrics": ["count", "est_total"] }));
    assert_eq!(g["count"], 3, "pending+active+done, not the cancelled one");
    assert_eq!(g["est_total"], "PT3H");
}

/// The other half of D24: `done` is real work and keeps counting. Excluding it
/// alongside `cancelled` would make `tracked_total` useless — tracked time is
/// overwhelmingly logged against tasks that are now finished.
#[test]
fn report_summary_still_counts_done_tasks() {
    let e = engine_with_one_task_per_status();
    let rep = e
        .report_summary(&json!({ "group_by": "status", "metrics": ["count"] }))
        .unwrap();
    let groups = rep["groups"].as_array().unwrap();
    let done = groups.iter().find(|g| g["status"] == "done");
    assert!(done.is_some(), "done must survive the default: {groups:?}");
    assert_eq!(done.unwrap()["count"], 1);
    assert!(
        groups.iter().all(|g| g["status"] != "cancelled"),
        "cancelled must not appear: {groups:?}"
    );
}

/// D24 rule 1: `--all` turns the default off entirely, which is the only way to
/// see abandoned work in an aggregation.
#[test]
fn report_summary_all_true_includes_cancelled_again() {
    let e = engine_with_one_task_per_status();
    let g = report(
        &e,
        json!({ "all": true, "metrics": ["count", "est_total"] }),
    );
    assert_eq!(g["count"], 4);
    assert_eq!(g["est_total"], "PT4H");
}

/// D24 rule 2 beats rule 3: an explicitly-typed filter is used literally. Typing
/// `tasqx report status:cancelled` and getting an empty table back reads as a
/// bug however well the default is documented.
#[test]
fn report_summary_honours_an_explicit_status_filter() {
    let e = engine_with_one_task_per_status();
    let g = report(
        &e,
        json!({ "filter": "status:cancelled", "metrics": ["count"] }),
    );
    assert_eq!(
        g["count"], 1,
        "the default must step aside, not narrow to nothing"
    );
}

/// Rule 2 also covers `@working`, which names a status set without using the
/// word `status`. Note what this test does and does not prove: `@working`
/// already restricts to pending+active, so the skip-cancelled default would
/// change nothing on top of it — the count is 2 either way. It pins that
/// `@working` is honoured literally; the structural-vs-lexical distinction it
/// motivates is guarded by filter::tests::constrains_status_sees_only_real_status_predicates.
#[test]
fn report_summary_honours_working_literally() {
    let e = engine_with_one_task_per_status();
    let g = report(&e, json!({ "filter": "@working", "metrics": ["count"] }));
    assert_eq!(g["count"], 2, "pending + active");
}

// ---- store.export -> store.import round-trip --------------------------------

#[test]
fn export_import_round_trip_equality() {
    let a = engine();
    a.project_create(&json!({ "name": "work.rt" })).unwrap();
    let dep = a.task_add(&json!({ "title": "blocker" })).unwrap(); // short 1
    let main = a
        .task_add(&json!({
            "title": "main",
            "priority": "H",
            "due": "2099-01-01T00:00:00Z", // far future => 0 urgency contribution, stable
            "tags": ["api", "release"]
        }))
        .unwrap(); // short 2
    let (dsid, msid) = (dep["short_id"].clone(), main["short_id"].clone());
    a.annotation_add(&json!({ "ref": msid, "body": "first note" }))
        .unwrap();
    a.dependency_add(&json!({ "ref": msid, "depends_on": dsid }))
        .unwrap();

    let export_a = a.store_export(&json!({})).unwrap();
    let tasks_a = export_a["tasks"].clone();
    assert_eq!(tasks_a.as_array().unwrap().len(), 2);

    // Import into a fresh store and re-export.
    let b = engine();
    let imp = b
        .store_import(&json!({ "tasks": tasks_a.clone() }))
        .unwrap();
    assert_eq!(imp["imported"], 2);

    let export_b = b.store_export(&json!({})).unwrap();
    assert_eq!(
        export_b["tasks"], tasks_a,
        "export -> import -> export is identity"
    );
}

/// An unparseable `status` used to be written to the row verbatim and then
/// laundered back to `pending` by the row reader — so a `done` task with a
/// mis-cased status came back as open work while still carrying `completed`.
/// Import rejects instead, following the D12 precedent for a bad reference.
#[test]
fn import_rejects_an_unparseable_status_or_priority() {
    let a = engine();
    let t = a.task_add(&json!({ "title": "finished" })).unwrap();
    a.task_done(&json!({ "ref": t["short_id"].clone() }))
        .unwrap();
    let good = a.store_export(&json!({})).unwrap()["tasks"].clone();
    assert_eq!(good[0]["status"], "done", "precondition: exported as done");

    let mut bad = good.clone();
    bad[0]["status"] = json!("Done");
    let b = engine();
    let err = b.store_import(&json!({ "tasks": bad })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("Done"),
        "the message must name the offending value: {}",
        err.message
    );
    assert_eq!(
        count(&b.task_list(&json!({ "all": true })).unwrap()),
        0,
        "a rejected import writes nothing"
    );

    let mut bad = good.clone();
    bad[0]["priority"] = json!("urgent");
    let c = engine();
    let err = c.store_import(&json!({ "tasks": bad })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("urgent"),
        "the message must name the offending value: {}",
        err.message
    );

    // The valid payload still imports, and still comes back done.
    let d = engine();
    d.store_import(&json!({ "tasks": good.clone() })).unwrap();
    assert_eq!(d.store_export(&json!({})).unwrap()["tasks"], good);
}

/// D28's rule in the last two fields it skipped. `remind` and `recurrence` were
/// read straight into the INSERT, so a payload carrying `remind:"sometime"` or
/// `recurrence:"every blue moon"` imported with exit 0 — values `task.add` and
/// `task.modify` reject through these very parsers — and then came back out of
/// the next export verbatim, propagating to every downstream store (D16's
/// concern). Neither field ever schedules anything when unparseable, so the
/// damage is silent: the user asked to be reminded and simply never is.
#[test]
fn import_rejects_an_unparseable_remind_or_recurrence() {
    let seed = || {
        json!([{ "id": "0193bbbb-0000-7000-8000-00000000000b", "short_id": 1,
                 "title": "hostile", "status": "pending" }])
    };

    let mut bad = seed();
    bad[0]["remind"] = json!("sometime");
    let a = engine();
    let err = a.store_import(&json!({ "tasks": bad })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("sometime"),
        "must name the offending value: {}",
        err.message
    );
    assert!(
        err.message.contains("remind"),
        "must name the offending field: {}",
        err.message
    );
    assert_eq!(
        count(&a.task_list(&json!({ "all": true })).unwrap()),
        0,
        "a rejected import writes nothing"
    );

    let mut bad = seed();
    bad[0]["recurrence"] = json!("every blue moon");
    let b = engine();
    let err = b.store_import(&json!({ "tasks": bad })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("every blue moon"),
        "must name the offending value: {}",
        err.message
    );
    assert!(
        err.message.contains("recurrence"),
        "must name the offending field: {}",
        err.message
    );
    assert_eq!(
        count(&b.task_list(&json!({ "all": true })).unwrap()),
        0,
        "a rejected import writes nothing"
    );

    // D28(2), the other half of the same rule: stores written before this gate
    // existed DO hold these values, and export is the only rescue path. The
    // door refuses them; the window must still open. Seeded by raw SQL because
    // `store.import` is now a strict door and can no longer produce the row.
    let c = engine();
    c.task_add(&json!({ "title": "legacy" })).unwrap();
    c.conn()
        .execute(
            "UPDATE tasks SET remind = 'sometime', recurrence = 'every blue moon'",
            [],
        )
        .unwrap();
    let out = c
        .store_export(&json!({}))
        .expect("a reader never refuses stored data");
    assert_eq!(
        out["tasks"][0]["remind"],
        json!("sometime"),
        "carried verbatim to the reader"
    );
    assert_eq!(out["tasks"][0]["recurrence"], json!("every blue moon"));
    assert_eq!(
        count(&c.task_list(&json!({ "all": true })).unwrap()),
        1,
        "and it still lists"
    );
}

/// The anchoring rule, pinned. A relative `remind` is SYMBOLIC — `-1h` means
/// "an hour before whatever `due` currently is" — so validating it on import
/// must check its shape without resolving it to an absolute instant. Collapsing
/// it would look like a successful import and silently sever the reminder from
/// `due`, which is exactly the invisible-field failure this project keeps
/// paying for. So: import an offset, then MOVE `due`, and require the reminder
/// to have moved with it.
#[test]
fn an_imported_relative_remind_still_re_anchors_when_due_moves() {
    let e = engine();
    e.store_import(&json!({ "tasks": [{
        "id": "0193cccc-0000-7000-8000-00000000000c", "short_id": 1,
        "title": "anchored", "status": "pending",
        "due": "2099-01-01T12:00:00Z", "remind": "-1h",
    }] }))
    .unwrap();

    let stored = e.task_get(&json!({ "ref": 1 })).unwrap();
    assert_eq!(
        stored["remind"],
        json!("-1h"),
        "stored symbolically, not collapsed to an instant"
    );
    assert_eq!(
        tasqx_core::remind::resolve("-1h", Some("2099-01-01T12:00:00Z")).map(|t| t.to_string()),
        Some("2099-01-01T11:00:00Z".to_string()),
    );

    // Move `due` a day later; the reminder must follow it, not stay behind.
    e.task_modify(&json!({ "ref": 1, "set": { "due": "2099-01-02T12:00:00Z" } }))
        .unwrap();
    let moved = e.task_get(&json!({ "ref": 1 })).unwrap();
    assert_eq!(
        moved["remind"],
        json!("-1h"),
        "the offset survives a due move verbatim"
    );
    assert_eq!(
        tasqx_core::remind::resolve(moved["remind"].as_str().unwrap(), moved["due"].as_str(),)
            .map(|t| t.to_string()),
        Some("2099-01-02T11:00:00Z".to_string()),
        "the reminder re-anchored on the new due",
    );
}

/// D12's unfiltered round trip stays byte-identical with both newly-validated
/// fields populated: validation normalizes through the same functions that
/// WROTE the stored form, so it is idempotent on anything an export produced.
#[test]
fn export_import_round_trip_is_byte_identical_with_remind_and_recurrence() {
    let a = engine();
    a.project_create(&json!({ "name": "rt" })).unwrap();
    a.task_add(&json!({
        "title": "repeating", "due": "2099-01-01T12:00:00Z",
        "remind": "-1h", "recurrence": "every 3 days",
    }))
    .unwrap();
    a.task_add(&json!({
        "title": "absolute reminder", "remind": "2099-06-01T09:00:00Z",
        "recurrence": "weekly on mon,wed",
    }))
    .unwrap();

    let tasks_a = a.store_export(&json!({})).unwrap()["tasks"].clone();
    let b = engine();
    b.store_import(&json!({ "tasks": tasks_a.clone() }))
        .unwrap();
    assert_eq!(
        b.store_export(&json!({})).unwrap()["tasks"],
        tasks_a,
        "export -> import -> export is identity"
    );
}

/// D17's rule at a new edge: `short_id` arrives from untrusted JSON as an i64
/// and was fed straight into `short_id + 1` to raise the mint floor. At
/// `i64::MAX` that panicked in debug and — far worse — wrapped in release,
/// leaving a floor of `i64::MIN` so the next `add` re-minted a short_id the
/// store already holds, breaking D4. A negative short_id is the same edge from
/// the other side: no minter can ever produce one, so it can only corrupt.
#[test]
fn import_rejects_a_short_id_outside_the_mintable_range() {
    let task = |sid: Value| {
        json!({ "tasks": [
            { "id": "0193aaaa-0000-7000-8000-00000000000a", "short_id": sid,
              "title": "hostile", "status": "pending" },
        ] })
    };

    for sid in [json!(i64::MAX), json!(-1), json!(0)] {
        let e = engine();
        let err = e.store_import(&task(sid.clone())).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::BadRequest,
            "short_id {sid}: {}",
            err.message
        );
        assert!(
            err.message.contains(&sid.to_string()),
            "the message must name the offending value: {}",
            err.message
        );
        // Nothing written, and — the part that matters — the mint floor is
        // untouched, so the next task still gets a fresh short_id.
        assert_eq!(count(&e.task_list(&json!({ "all": true })).unwrap()), 0);
        let t = e.task_add(&json!({ "title": "after" })).unwrap();
        assert_eq!(
            t["short_id"],
            json!(1),
            "short_id {sid} corrupted the floor"
        );
    }

    // The largest short_id a minter could have produced still imports, and the
    // floor lands above it. The store is then genuinely full, so the next `add`
    // says so instead of wrapping the counter round to re-mint from `i64::MIN`.
    let e = engine();
    e.store_import(&task(json!(i64::MAX - 1)))
        .expect("the boundary value is legal");
    let err = e.task_add(&json!({ "title": "after" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("exhausted"), "got: {}", err.message);
}

// ---- D8: boolean or + parentheses grouping ----------------------------------

#[test]
fn filter_or_and_parentheses() {
    let e = engine();
    e.task_add(&json!({ "title": "t api", "tags": ["api"] }))
        .unwrap();
    e.task_add(&json!({ "title": "t infra", "tags": ["infra"] }))
        .unwrap();
    e.task_add(&json!({ "title": "t other", "tags": ["other"] }))
        .unwrap();

    // Explicit or.
    let r = e.task_list(&json!({ "filter": "+api or +infra" })).unwrap();
    assert_eq!(count(&r), 2);

    // Parenthesised or, AND-ed with a status predicate.
    let r = e
        .task_list(&json!({ "filter": "(+api or +infra) and status:pending" }))
        .unwrap();
    assert_eq!(count(&r), 2);

    // Implicit AND still means AND: no task has both tags.
    let r = e.task_list(&json!({ "filter": "+api +infra" })).unwrap();
    assert_eq!(count(&r), 0);

    // Precedence: and binds tighter than or.
    // "+api or +infra and +other" == api OR (infra AND other) => only the api task.
    let r = e
        .task_list(&json!({ "filter": "+api or +infra and +other" }))
        .unwrap();
    assert_eq!(count(&r), 1);
    assert_eq!(r["tasks"][0]["title"], "t api");
}

// ---- D2: recurrence — spawn-on-completion, collapse, transactional ----------

/// Seconds between two RFC3339 instants in a task JSON value's field.
fn secs(s: &str) -> i64 {
    s.parse::<Timestamp>().unwrap().as_second()
}

/// Total number of tasks currently stored (any status).
fn task_count(e: &Engine) -> i64 {
    count(&e.task_list(&json!({})).unwrap())
}

#[test]
fn completing_recurring_spawns_exactly_one_advanced_instance() {
    let e = engine();
    let due = plus_hours(48); // future, so completion is "on time" (now < due)
    let added = e
        .task_add(&json!({ "title": "water plants", "due": due, "recurrence": "every 3 days" }))
        .unwrap();
    assert_eq!(added["recurrence"], "every 3 days");

    let done = e
        .task_done(&json!({ "ref": added["short_id"].clone() }))
        .unwrap();
    let spawned = &done["spawned"];
    assert!(
        spawned.is_object(),
        "done result carries a spawned instance"
    );

    // Exactly one new instance: template (done) + spawn = 2 tasks total.
    assert_eq!(task_count(&e), 2);

    // The spawned due is advanced by exactly one period (3 days) from the anchor.
    let new_due = spawned["due"].as_str().unwrap();
    assert_eq!(secs(new_due) - secs(due.as_str()), 3 * 86_400);

    // Spawn carries the rule forward and is a fresh short_id.
    let got = e
        .task_get(&json!({ "ref": spawned["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["recurrence"], "every 3 days");
    assert_eq!(got["status"], "pending");
    assert_ne!(spawned["short_id"], added["short_id"]);
    assert_eq!(spawned["status"], "pending");
}

#[test]
fn non_recurring_done_spawns_nothing() {
    let e = engine();
    let a = e
        .task_add(&json!({ "title": "one off", "due": plus_hours(24) }))
        .unwrap();
    let done = e
        .task_done(&json!({ "ref": a["short_id"].clone() }))
        .unwrap();
    assert!(done.get("spawned").is_none());
    assert_eq!(task_count(&e), 1);
}

#[test]
fn long_gap_collapses_to_a_single_catch_up() {
    let e = engine();
    // Anchor far in the past: many periods have been "missed".
    let a = e
        .task_add(&json!({
            "title": "stale", "due": "2020-01-01T09:00:00Z", "recurrence": "every 3 days"
        }))
        .unwrap();
    let done = e
        .task_done(&json!({ "ref": a["short_id"].clone() }))
        .unwrap();
    let spawned = &done["spawned"];

    // ONE catch-up instance, not a backfill storm.
    assert_eq!(task_count(&e), 2, "collapse to a single instance");

    // The single instance's due is in the future (anchor advanced past now).
    let new_due = spawned["due"].as_str().unwrap();
    assert!(
        secs(new_due) > Timestamp::now().as_second(),
        "next slot is in the future"
    );
    // And it is on the 3-day lattice from the original anchor.
    let anchor = secs("2020-01-01T09:00:00Z");
    assert_eq!((secs(new_due) - anchor) % (3 * 86_400), 0);
}

#[test]
fn weekly_on_days_spawns_a_listed_weekday() {
    let e = engine();
    let a = e
        .task_add(&json!({
            "title": "standup", "due": "2020-01-06T08:00:00Z", // a Monday, long ago
            "recurrence": "weekly on mon,wed,fri"
        }))
        .unwrap();
    let done = e
        .task_done(&json!({ "ref": a["short_id"].clone() }))
        .unwrap();
    let new_due = done["spawned"]["due"].as_str().unwrap();
    // The spawned weekday must be one of the listed days.
    let wd = new_due
        .parse::<Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .weekday();
    use jiff::civil::Weekday::*;
    assert!(
        matches!(wd, Monday | Wednesday | Friday),
        "got weekday {wd:?}"
    );
}

#[test]
fn completing_monthly_5th_friday_spawns_a_real_5th_friday() {
    // Regression: "monthly on the 5th friday" must NOT error/roll back in months
    // without a 5th Friday — it skips to the next month that has one. Anchor is
    // far in the past so the collapse loop crosses several 4-Friday months.
    let e = engine();
    let a = e
        .task_add(&json!({
            "title": "payroll", "due": "2020-01-31T09:00:00Z", // 5th Fri of Jan 2020
            "recurrence": "monthly on the 5th friday"
        }))
        .unwrap();
    let done = e
        .task_done(&json!({ "ref": a["short_id"].clone() }))
        .unwrap();
    assert!(
        done["spawned"].is_object(),
        "completion succeeds and spawns"
    );
    assert_eq!(task_count(&e), 2, "exactly one catch-up instance");

    let new_due = done["spawned"]["due"].as_str().unwrap();
    let d = new_due
        .parse::<Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    // It is a Friday, in the future, and genuinely the 5th Friday of its month.
    assert_eq!(d.weekday(), jiff::civil::Weekday::Friday);
    assert!(
        secs(new_due) > Timestamp::now().as_second(),
        "next slot is in the future"
    );
    let fifth = d
        .nth_weekday_of_month(5, jiff::civil::Weekday::Friday)
        .unwrap();
    assert_eq!(d, fifth, "spawned date is the 5th Friday of its month");
}

#[test]
fn completing_monthly_day_31_clamps_to_month_end() {
    // "monthly on day 31" collapsed over a long gap must land on a valid month-end
    // (28/29/30/31) rather than erroring, exercising the clamp through the spawn.
    let e = engine();
    let a = e
        .task_add(&json!({
            "title": "invoice", "due": "2020-01-31T09:00:00Z",
            "recurrence": "monthly on day 31"
        }))
        .unwrap();
    let done = e
        .task_done(&json!({ "ref": a["short_id"].clone() }))
        .unwrap();
    assert_eq!(task_count(&e), 2);

    let new_due = done["spawned"]["due"].as_str().unwrap();
    let d = new_due
        .parse::<Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    assert!(
        secs(new_due) > Timestamp::now().as_second(),
        "next slot is in the future"
    );
    // Day is 31, or the clamped last day of a shorter month.
    let expected = 31.min(d.last_of_month().day());
    assert_eq!(d.day(), expected, "day-31 rule clamps to month end");
}

#[test]
fn modify_can_set_and_clear_recurrence() {
    let e = engine();
    let a = e
        .task_add(&json!({ "title": "rent", "due": plus_hours(48) }))
        .unwrap();
    let r#ref = a["short_id"].clone();

    // Set a rule via modify.
    e.task_modify(&json!({ "ref": r#ref, "set": { "recurrence": "monthly on day 1" } }))
        .unwrap();
    let got = e.task_get(&json!({ "ref": r#ref })).unwrap();
    assert_eq!(got["recurrence"], "monthly on day 1");

    // Clear it: completing then no longer spawns.
    e.task_modify(&json!({ "ref": r#ref, "set": { "recurrence": null } }))
        .unwrap();
    let got = e.task_get(&json!({ "ref": r#ref })).unwrap();
    assert!(got["recurrence"].is_null());

    // An invalid rule is rejected.
    let bad = e.task_modify(&json!({ "ref": r#ref, "set": { "recurrence": "every blue moon" } }));
    assert_eq!(bad.unwrap_err().code, ErrorCode::BadRequest);
}

#[test]
fn spawn_is_transactional_failed_completion_leaves_no_trace() {
    let e = engine();
    let a = e
        .task_add(&json!({ "title": "corruptible", "due": plus_hours(48) }))
        .unwrap();
    let sid = a["short_id"].as_i64().unwrap();

    // Corrupt the stored rule directly so spawn_next's parse fails mid-transaction.
    e.conn()
        .execute(
            &format!("UPDATE tasks SET recurrence='not a rule' WHERE short_id={sid}"),
            [],
        )
        .unwrap();

    let events_before = count(&e.event_list(&json!({ "limit": 10000 })).unwrap());

    // Completion must fail (bad rule) and roll the whole transaction back.
    let res = e.task_done(&json!({ "ref": a["short_id"].clone() }));
    assert!(res.is_err(), "completion with an unparseable rule errors");

    // No done: task is still pending. No spawn: still one task. No new event.
    let got = e
        .task_get(&json!({ "ref": a["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["status"], "pending", "completion rolled back");
    assert_eq!(task_count(&e), 1, "no spawn created");
    let events_after = count(&e.event_list(&json!({ "limit": 10000 })).unwrap());
    assert_eq!(events_before, events_after, "no event written on rollback");
}

// ---- §9: reminders — the API/schema surface ---------------------------------

/// The `remind` field as seen through `task.list`, for `short_id`.
fn remind_of(e: &Engine, short_id: i64) -> serde_json::Value {
    let list = e.task_list(&json!({ "filter": "" })).unwrap();
    list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["short_id"] == short_id)
        .expect("task present in task.list")["remind"]
        .clone()
}

#[test]
fn remind_is_stored_canonically_and_surfaced_in_task_list() {
    let e = engine();
    // A due-anchored offset stays symbolic (it must re-anchor when due moves).
    let a = e
        .task_add(&json!({ "title": "ship", "due": "2099-01-01T00:00:00Z", "remind": "-60m" }))
        .unwrap();
    // `-60m` normalizes to the canonical `-1h`.
    assert_eq!(remind_of(&e, a["short_id"].as_i64().unwrap()), json!("-1h"));

    // An absolute expression resolves ONCE, at set time, to RFC3339.
    let b = e
        .task_add(&json!({ "title": "call", "remind": "2099-01-01T09:00:00Z" }))
        .unwrap();
    assert_eq!(
        remind_of(&e, b["short_id"].as_i64().unwrap()),
        json!("2099-01-01T09:00:00Z")
    );

    // Quiet by default (§9): no remind key => no reminder, ever.
    let c = e
        .task_add(&json!({ "title": "no reminder", "due": "2099-01-01T00:00:00Z" }))
        .unwrap();
    assert_eq!(remind_of(&e, c["short_id"].as_i64().unwrap()), json!(null));
}

#[test]
fn task_add_rejects_an_invalid_remind_spec() {
    let e = engine();
    let err = e
        .task_add(&json!({ "title": "bad", "due": "2099-01-01T00:00:00Z", "remind": "-1x" }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    // The add is rejected as a whole — no half-created task.
    assert_eq!(count(&e.task_list(&json!({ "filter": "" })).unwrap()), 0);
}

#[test]
fn task_modify_sets_and_clears_remind() {
    let e = engine();
    let t = e
        .task_add(&json!({ "title": "ship", "due": "2099-01-01T00:00:00Z" }))
        .unwrap();
    let sid = t["short_id"].as_i64().unwrap();
    assert_eq!(remind_of(&e, sid), json!(null));

    e.task_modify(&json!({ "ref": sid, "set": { "remind": "-2h" } }))
        .unwrap();
    assert_eq!(remind_of(&e, sid), json!("-2h"));

    // null is the sanctioned "stop reminding me" path.
    e.task_modify(&json!({ "ref": sid, "set": { "remind": null } }))
        .unwrap();
    assert_eq!(remind_of(&e, sid), json!(null));

    // A bad spec is rejected and leaves the field untouched.
    e.task_modify(&json!({ "ref": sid, "set": { "remind": "-1h" } }))
        .unwrap();
    let err = e
        .task_modify(&json!({ "ref": sid, "set": { "remind": "nope" } }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert_eq!(
        remind_of(&e, sid),
        json!("-1h"),
        "a rejected modify changes nothing"
    );
}

#[test]
fn remind_survives_an_export_import_round_trip_byte_identically() {
    let a = engine();
    a.task_add(&json!({
        "title": "offset reminder",
        "due": "2099-01-01T00:00:00Z",
        "remind": "-1h"
    }))
    .unwrap();
    a.task_add(&json!({ "title": "absolute reminder", "remind": "2099-01-01T09:00:00Z" }))
        .unwrap();

    let export_a = a.store_export(&json!({})).unwrap();
    assert_eq!(
        export_a["tasks"][0]["remind"],
        json!("-1h"),
        "export carries the spec"
    );
    assert_eq!(
        export_a["tasks"][1]["remind"],
        json!("2099-01-01T09:00:00Z")
    );

    let b = engine();
    b.store_import(&json!({ "tasks": export_a["tasks"].clone() }))
        .unwrap();
    let export_b = b.store_export(&json!({})).unwrap();

    // The whole export must be byte-identical, not merely equivalent.
    assert_eq!(
        serde_json::to_string(&export_a).unwrap(),
        serde_json::to_string(&export_b).unwrap(),
        "remind must round-trip byte-identically"
    );
}

#[test]
fn reminder_fire_is_idempotent_through_the_public_api() {
    let e = engine();
    let t = e
        .task_add(&json!({ "title": "ship", "due": "2099-01-01T00:00:00Z", "remind": "-1h" }))
        .unwrap();
    let sid = t["short_id"].as_i64().unwrap();
    let at = "2098-12-31T23:00:00Z";

    let first = e.reminder_fire(&json!({ "ref": sid, "at": at })).unwrap();
    assert_eq!(first["fired"], json!(true));
    assert_eq!(first["short_id"], json!(sid));

    let second = e.reminder_fire(&json!({ "ref": sid, "at": at })).unwrap();
    assert_eq!(
        second["fired"],
        json!(false),
        "the same instant never fires twice"
    );

    // Exactly one `reminded` event — the dedupe record AND the push surface.
    let evts = e.event_list(&json!({ "ref": sid, "limit": 100 })).unwrap();
    let reminded: Vec<_> = evts["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["op"] == json!("reminded"))
        .collect();
    assert_eq!(reminded.len(), 1);

    // A DIFFERENT instant on the same task is a different reminder and fires.
    let other = e
        .reminder_fire(&json!({ "ref": sid, "at": "2098-12-30T23:00:00Z" }))
        .unwrap();
    assert_eq!(other["fired"], json!(true));
}

/// **`at` is the one date input in the tool that deliberately does NOT take the
/// `due:` grammar, and the message has to say so.**
///
/// D33 unified every date a human types onto `datetime::parse_when`. `at` is not
/// one: `scheduler::fire` supplies it as `p.at.to_string()`, the already-resolved
/// instant of the reminder, and `storage::already_reminded` compares it to the
/// stored payload by **exact string match**. That makes `at` an identifier for a
/// specific scheduled reminder, not a moment the caller picks — so a relative
/// spelling is not a friendlier way to say the same thing, it is a different
/// instant. `at: "tomorrow"` would resolve to something that matches no pending
/// reminder, write a `reminded` row that dedupes nothing, and leave the real
/// reminder free to fire again: a silent double-notify plus a junk audit row.
///
/// So the refusal stays, and this test pins the *reason* being in the text. The
/// old message ("`at` must be RFC3339") implied the caller should have typed the
/// date differently, which invites exactly the retry that cannot work.
#[test]
fn reminder_fire_refuses_a_relative_at_and_says_why_rather_than_blaming_the_spelling() {
    let e = engine();
    let sid = e
        .task_add(&json!({ "title": "ship", "due": "2099-01-01T00:00:00Z", "remind": "-1h" }))
        .unwrap()["short_id"]
        .clone();

    // Every one of these is a spelling `due:` accepts, which is the whole point:
    // the exception is deliberate and must not be quietly closed by a later
    // "unify the last date input" pass.
    for loose in ["tomorrow", "friday", "in 3 days", "eom", "2026-07-25"] {
        let err = e
            .reminder_fire(&json!({ "ref": sid, "at": loose }))
            .expect_err(&format!(
                "`at` must stay strict, but {loose:?} was accepted"
            ));
        assert_eq!(err.code, ErrorCode::BadRequest, "for at:{loose:?}");
        assert!(
            err.message.contains(loose),
            "must name the value; got {:?}",
            err.message
        );
        // The two facts that turn "you typed it wrong" into "this field is not
        // yours to type": it identifies a scheduled reminder, and it is the
        // dedupe key. Asserted as substrings so the sentence can be reworded
        // but not hollowed out.
        assert!(
            err.message.contains("dedupe"),
            "the message must say `at` is the dedupe key, not just the wrong format; got {:?}",
            err.message
        );
        assert!(
            err.message.contains("scheduler"),
            "the message must say the scheduler supplies it; got {:?}",
            err.message
        );
    }
}

#[test]
fn reminder_fire_normalizes_at_so_equivalent_instants_dedupe() {
    let e = engine();
    let t = e
        .task_add(&json!({ "title": "ship", "remind": "2099-01-01T09:00:00Z" }))
        .unwrap();
    let sid = t["short_id"].as_i64().unwrap();

    let first = e
        .reminder_fire(&json!({ "ref": sid, "at": "2099-01-01T09:00:00Z" }))
        .unwrap();
    assert_eq!(first["fired"], json!(true));
    // Same instant, different spelling: must NOT read as a new reminder.
    let same = e
        .reminder_fire(&json!({ "ref": sid, "at": "2099-01-01T11:00:00+02:00" }))
        .unwrap();
    assert_eq!(
        same["fired"],
        json!(false),
        "offset spelling must not defeat dedupe"
    );
}

#[test]
fn reminder_fire_does_not_bump_rev_or_modified() {
    let e = engine();
    let t = e
        .task_add(&json!({ "title": "ship", "due": "2099-01-01T00:00:00Z", "remind": "-1h" }))
        .unwrap();
    let sid = t["short_id"].as_i64().unwrap();
    let before = e.task_get(&json!({ "ref": sid })).unwrap();

    e.reminder_fire(&json!({ "ref": sid, "at": "2098-12-31T23:00:00Z" }))
        .unwrap();
    let after = e.task_get(&json!({ "ref": sid })).unwrap();

    // A reminder is a fact about time passing, not an edit: bumping `_rev` would
    // spuriously break a client holding `expected_rev`.
    assert_eq!(
        before["_rev"], after["_rev"],
        "reminder.fire must not bump _rev"
    );
    assert_eq!(
        before["modified"], after["modified"],
        "reminder.fire must not touch modified"
    );
}

#[test]
fn reminder_fire_rejects_a_non_rfc3339_instant() {
    let e = engine();
    let t = e
        .task_add(&json!({ "title": "ship", "remind": "2099-01-01T09:00:00Z" }))
        .unwrap();
    let err = e
        .reminder_fire(&json!({ "ref": t["short_id"], "at": "friday" }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
}

#[test]
fn a_recurring_instance_carries_its_reminder_forward() {
    let e = engine();
    // Offset reminder: symbolic, so it re-anchors onto the spawned instance's due.
    let t = e
        .task_add(&json!({
            "title": "water plants",
            "due": plus_hours(1),
            "remind": "-30m",
            "recurrence": "every 3 days"
        }))
        .unwrap();
    let done = e.task_done(&json!({ "ref": t["short_id"] })).unwrap();
    let spawned = done["spawned"]["short_id"].as_i64().unwrap();
    assert_eq!(
        remind_of(&e, spawned),
        json!("-30m"),
        "the offset rides along unchanged"
    );
}

#[test]
fn a_recurring_instance_shifts_an_absolute_reminder_instead_of_inheriting_the_past() {
    let e = engine();
    // Absolute reminder 30m before a due 2h out.
    let due = plus_hours(2);
    let remind_at = (due.parse::<Timestamp>().unwrap() - SignedDuration::from_mins(30)).to_string();
    let t = e
        .task_add(&json!({
            "title": "weekly report",
            "due": due,
            "remind": remind_at,
            "recurrence": "every 1 weeks"
        }))
        .unwrap();

    let done = e.task_done(&json!({ "ref": t["short_id"] })).unwrap();
    let spawned = done["spawned"]["short_id"].as_i64().unwrap();
    let got = remind_of(&e, spawned);
    let got_ts = got.as_str().unwrap().parse::<Timestamp>().unwrap();

    // It must move with the instance, not sit in the past firing immediately.
    assert!(
        got_ts > Timestamp::now(),
        "a spawned absolute reminder must be in the future"
    );
    let new_due = done["spawned"]["due"]
        .as_str()
        .unwrap()
        .parse::<Timestamp>()
        .unwrap();
    assert_eq!(
        new_due.as_second() - got_ts.as_second(),
        30 * 60,
        "the reminder keeps its offset from the new due"
    );
}

// ---- store.export / store.import dependency integrity ------------------------
//
// Two halves of one confirmed data-integrity bug:
//   1. a filtered export emitted edges whose target was filtered out, so the
//      payload referenced a task that was not in it;
//   2. import inserted those dangling edges silently — invisible to `show`,
//      `next` and `undep` (all inner-join `tasks`) yet still exported, and it
//      detonated later when the target finally arrived.
// Neither path had coverage: the round-trip test never applied a filter.

/// A store with `#1 Blocker task +infra` blocking `#2 Dependent task +api`.
fn blocker_and_dependent() -> (Engine, serde_json::Value, serde_json::Value) {
    let e = engine();
    let blocker = e
        .task_add(&json!({ "title": "Blocker task", "tags": ["infra"] }))
        .unwrap();
    let dependent = e
        .task_add(&json!({ "title": "Dependent task", "tags": ["api"] }))
        .unwrap();
    e.dependency_add(&json!({
        "ref": dependent["short_id"].clone(),
        "depends_on": blocker["short_id"].clone()
    }))
    .unwrap();
    (e, blocker, dependent)
}

/// Half 1: a filtered export must be self-consistent — never reference a task
/// it did not emit — and must report the trim rather than doing it silently.
#[test]
fn filtered_export_drops_edges_to_tasks_outside_the_filter() {
    let (e, blocker, _dependent) = blocker_and_dependent();

    let ex = e.store_export(&json!({ "filter": "+api" })).unwrap();
    let tasks = ex["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "+api selects only the dependent");
    assert_eq!(
        tasks[0]["depends_on"],
        json!([]),
        "an edge whose target is outside the export must not be emitted"
    );
    assert_eq!(
        ex["dropped_dependencies"],
        json!(1),
        "the trim must be visible"
    );

    // Self-consistency, stated generally: every referenced id is present.
    let ids: Vec<&str> = tasks.iter().map(|t| t["id"].as_str().unwrap()).collect();
    for t in tasks {
        for d in t["depends_on"].as_array().unwrap() {
            assert!(ids.contains(&d.as_str().unwrap()), "dangling id in export");
        }
    }

    // The unfiltered export is unaffected: the edge survives, nothing dropped.
    let full = e.store_export(&json!({})).unwrap();
    assert_eq!(full["dropped_dependencies"], json!(0));
    assert_eq!(
        full["tasks"][1]["depends_on"],
        json!([blocker["id"].clone()])
    );
}

/// Half 2a: import must reject a payload carrying an edge to a task that is
/// neither in the payload nor already in the store, naming the missing id.
#[test]
fn import_rejects_a_dangling_dependency_edge() {
    let (a, blocker, _dependent) = blocker_and_dependent();
    let blocker_id = blocker["id"].as_str().unwrap().to_string();

    // A payload as produced by the *old* buggy export (or hand-edited): the
    // dependent alone, still pointing at the absent blocker.
    let mut tv = a.store_export(&json!({ "filter": "+api" })).unwrap()["tasks"][0].clone();
    tv["depends_on"] = json!([blocker_id]);

    let b = engine();
    let err = b.store_import(&json!({ "tasks": [tv] })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains(&blocker_id),
        "the error must name the missing target, got: {}",
        err.message
    );
    // The whole import is one transaction: nothing was written.
    let after = b.store_export(&json!({})).unwrap();
    assert_eq!(after["tasks"].as_array().unwrap().len(), 0);
}

/// Half 2b: the fixed filtered export imports cleanly, and the dependent is
/// genuinely edge-free — importing the blocker later must NOT resurrect a
/// hidden edge and flip it to blocked.
#[test]
fn importing_a_filtered_export_leaves_no_hidden_edge() {
    let (a, _blocker, dependent) = blocker_and_dependent();
    let api = a.store_export(&json!({ "filter": "+api" })).unwrap();
    let infra = a.store_export(&json!({ "filter": "+infra" })).unwrap();

    let b = engine();
    b.store_import(&json!({ "tasks": api["tasks"].clone() }))
        .unwrap();
    let got = b
        .task_get(&json!({ "ref": dependent["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["blocked"], json!(false));
    assert_eq!(got["depends_on"], json!([]));

    // The blocker arrives later. The dependent must stay unblocked: the edge
    // was dropped at export, so it is really gone, not merely invisible.
    b.store_import(&json!({ "tasks": infra["tasks"].clone() }))
        .unwrap();
    let got = b
        .task_get(&json!({ "ref": dependent["short_id"].clone() }))
        .unwrap();
    assert_eq!(
        got["blocked"],
        json!(false),
        "a dropped edge must not resurrect"
    );
    assert_eq!(got["depends_on"], json!([]));
    assert_eq!(
        b.store_export(&json!({})).unwrap()["dropped_dependencies"],
        json!(0)
    );
}

/// Import is two-pass: an edge may point forward to a task later in the array,
/// and to a task that is already in the target store.
#[test]
fn import_wires_edges_regardless_of_payload_order() {
    let (a, _blocker, dependent) = blocker_and_dependent();
    let full = a.store_export(&json!({})).unwrap();
    let mut tasks = full["tasks"].as_array().unwrap().clone();
    tasks.reverse(); // dependent first, its target second

    let b = engine();
    let imp = b.store_import(&json!({ "tasks": tasks })).unwrap();
    assert_eq!(imp["imported"], 2);
    let got = b
        .task_get(&json!({ "ref": dependent["short_id"].clone() }))
        .unwrap();
    assert_eq!(
        got["blocked"],
        json!(true),
        "forward reference must still wire up"
    );

    // Target already in the store, payload carries only the dependent.
    let c = engine();
    c.store_import(&json!({ "tasks": full["tasks"].clone() }))
        .unwrap();
    let only_dep = json!([full["tasks"][1].clone()]);
    c.store_import(&json!({ "tasks": only_dep })).unwrap();
    let got = c
        .task_get(&json!({ "ref": dependent["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["blocked"], json!(true));
}

// ---- modify: set + clear round-trip (DESIGN §5, §12-D13) --------------------

/// Every field the CLI's `modify` steers must survive a set → read → clear →
/// read round-trip through the real API. The CLI's `--clear <field>` compiles to
/// exactly the `null` asserted here, so this pins the contract that syntax rides
/// on: a null CLEARS, it is not "no change".
#[test]
fn modify_sets_then_clears_every_steering_field() {
    let e = engine();
    // D23: every explicit project must name a live project row.
    e.project_create(&json!({ "name": "p" })).unwrap();
    e.project_create(&json!({ "name": "work.x" })).unwrap();
    let added = e
        .task_add(&json!({ "title": "Original", "project": "p", "priority": "L" }))
        .unwrap();
    let r = added["short_id"].clone();

    e.task_modify(&json!({
        "ref": r,
        "set": {
            "title": "Renamed",
            "project": "work.x",
            "priority": "H",
            "due": plus_hours(24),
            "scheduled": plus_hours(2),
            "wait": plus_hours(1),
            "estimate": "PT4H",
            "recurrence": "every 3 days",
            "remind": "-1h",
        }
    }))
    .unwrap();

    let got = e.task_get(&json!({ "ref": r })).unwrap();
    assert_eq!(got["title"], "Renamed");
    assert_eq!(got["project"], "work.x");
    assert_eq!(got["priority"], "H");
    assert!(got["due"].is_string());
    assert!(got["scheduled"].is_string());
    assert!(got["wait"].is_string());
    assert_eq!(got["estimate"], "PT4H");
    assert_eq!(got["recurrence"], "every 3 days");
    assert_eq!(got["remind"], "-1h");

    // Clearing: null on every nullable field, exactly as `--clear` emits.
    e.task_modify(&json!({
        "ref": r,
        "set": {
            "project": null, "priority": null, "due": null, "scheduled": null,
            "wait": null, "estimate": null, "recurrence": null, "remind": null,
        }
    }))
    .unwrap();

    let got = e.task_get(&json!({ "ref": r })).unwrap();
    for field in [
        "project",
        "priority",
        "due",
        "scheduled",
        "wait",
        "estimate",
        "recurrence",
        "remind",
    ] {
        assert_eq!(got[field], json!(null), "{field} must be cleared");
    }
    // The title is untouched by a clear-everything sweep — it has no null form.
    assert_eq!(got["title"], "Renamed");
}

/// `modify --expected-rev` must actually protect a concurrent edit. The CLI
/// passes the flag straight through, so this is the guarantee behind it.
#[test]
fn modify_expected_rev_rejects_a_stale_write() {
    let e = engine();
    let added = e.task_add(&json!({ "title": "Contended" })).unwrap();
    let r = added["short_id"].clone();

    let first = e
        .task_modify(&json!({ "ref": r, "set": { "priority": "H" } }))
        .unwrap();
    let rev = first["_rev"].as_i64().unwrap();

    // Someone else moves the task on.
    e.task_modify(&json!({ "ref": r, "set": { "priority": "L" } }))
        .unwrap();

    // Our stale rev must lose, and must not clobber.
    let err = e
        .task_modify(&json!({ "ref": r, "set": { "priority": "M" }, "expected_rev": rev }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let got = e.task_get(&json!({ "ref": r })).unwrap();
    assert_eq!(
        got["priority"], "L",
        "the losing write must not have applied"
    );

    // At the current rev it goes through.
    let cur = got["_rev"]
        .as_i64()
        .or_else(|| got["rev"].as_i64())
        .unwrap();
    e.task_modify(&json!({ "ref": r, "set": { "priority": "M" }, "expected_rev": cur }))
        .unwrap();
    assert_eq!(e.task_get(&json!({ "ref": r })).unwrap()["priority"], "M");
}

/// Clearing a recurrence is the sanctioned "stop it repeating" path (D2), and it
/// must stop the task from spawning a successor on completion.
#[test]
fn clearing_recurrence_stops_the_task_repeating() {
    let e = engine();
    let added = e
        .task_add(
            &json!({ "title": "Water plants", "recurrence": "every 3 days", "due": plus_hours(1) }),
        )
        .unwrap();
    let r = added["short_id"].clone();
    assert_eq!(
        e.task_get(&json!({ "ref": r })).unwrap()["recurrence"],
        "every 3 days"
    );

    e.task_modify(&json!({ "ref": r, "set": { "recurrence": null } }))
        .unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": r })).unwrap()["recurrence"],
        json!(null)
    );

    // Completing it now spawns nothing.
    let before = count(&e.task_list(&json!({ "filter": "" })).unwrap());
    e.task_done(&json!({ "ref": r })).unwrap();
    let after = count(&e.task_list(&json!({ "filter": "" })).unwrap());
    assert_eq!(
        after, before,
        "a cleared recurrence must not spawn a successor"
    );
}

// ---- review follow-ups: import must enforce the guards the API enforces ----

/// `dependency.add` refuses a self-edge; `store.import` must refuse it too.
/// It did not, so an import could mint a task blocked by itself forever — a
/// state the API itself calls a conflict and cannot otherwise reach. Worse, it
/// re-exported verbatim, so the corruption survived every future hop.
#[test]
fn import_rejects_a_self_dependency() {
    let e = engine();
    let id = "0193aaaa-0000-7000-8000-00000000000a";
    let err = e
        .store_import(&json!({ "tasks": [{
            "id": id, "short_id": 1, "title": "self", "status": "pending",
            "depends_on": [id],
        }] }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains("cannot depend on itself"),
        "got: {}",
        err.message
    );
    // One transaction: a rejected payload writes nothing at all.
    assert_eq!(
        e.store_export(&json!({})).unwrap()["tasks"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

/// The mutual case: A->B and B->A in one payload. Both tasks end up blocked,
/// which silently empties the working set (`tasqx list` printed "No tasks."
/// with no indication why). `dependency.add` rejects this as a cycle; import
/// must apply the same reachability check.
#[test]
fn import_rejects_a_dependency_cycle() {
    let e = engine();
    let (a, b) = (
        "0193aaaa-0000-7000-8000-00000000000a",
        "0193aaaa-0000-7000-8000-00000000000b",
    );
    let err = e
        .store_import(&json!({ "tasks": [
            { "id": a, "short_id": 1, "title": "A", "status": "pending", "depends_on": [b] },
            { "id": b, "short_id": 2, "title": "B", "status": "pending", "depends_on": [a] },
        ] }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(err.message.contains("cycle"), "got: {}", err.message);
    assert_eq!(
        e.store_export(&json!({})).unwrap()["tasks"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

/// The guard must not reject legitimate diamonds: A->B, A->C, B->D, C->D is a
/// DAG, not a cycle. Import has to stay usable for real dependency graphs.
#[test]
fn import_accepts_a_diamond_dependency_graph() {
    let e = engine();
    let id = |n: &str| format!("0193aaaa-0000-7000-8000-00000000000{n}");
    e.store_import(&json!({ "tasks": [
        { "id": id("a"), "short_id": 1, "title": "A", "status": "pending",
          "depends_on": [id("b"), id("c")] },
        { "id": id("b"), "short_id": 2, "title": "B", "status": "pending",
          "depends_on": [id("d")] },
        { "id": id("c"), "short_id": 3, "title": "C", "status": "pending",
          "depends_on": [id("d")] },
        { "id": id("d"), "short_id": 4, "title": "D", "status": "pending" },
    ] }))
    .expect("a diamond is a DAG and must import");
    let got = e.task_get(&json!({ "ref": 1 })).unwrap();
    assert_eq!(got["depends_on"].as_array().unwrap().len(), 2);
}

/// A store can still hold an unreadable estimate: stores predate the parser
/// guard, and `store.import` used to write these columns raw. `report` must
/// survive it — the reader returns None and the roll-up skips it, rather than
/// aborting the process.
///
/// Seeded by raw SQL rather than through `store.import`, which is now a strict
/// door (B2) and no longer able to produce this row. That is the point of the
/// pairing: writes refuse the value, reads still cope with one already stored.
#[test]
fn report_over_an_unreadable_estimate_does_not_panic() {
    let e = engine();
    e.project_create(&json!({ "name": "p" })).unwrap(); // D23
    e.task_add(&json!({ "title": "huge", "project": "p", "estimate": "PT4H" }))
        .unwrap();
    e.task_add(&json!({ "title": "real", "project": "p", "estimate": "PT4H" }))
        .unwrap();
    e.conn()
        .execute(
            "UPDATE tasks SET estimate = 'P7000000000000000000D' WHERE title = 'huge'",
            [],
        )
        .unwrap();
    let r = e
        .report_summary(&json!({ "metrics": ["count", "est_total"] }))
        .expect("report must not panic over an unreadable estimate");
    let row = &r["groups"][0];
    assert_eq!(row["count"], json!(2), "both rows are still counted");
    // The unreadable row contributes nothing; the real 4h survives rather than
    // being swallowed by a wrapped total.
    assert_eq!(row["est_total"], json!("PT4H"), "got {row:?}");
}

/// `project` is the one nullable field with no parser in front of it, so
/// `--project ""` wrote an empty string where every sibling field rejects one
/// (`--due/--scheduled/--wait ""` -> "empty date expression", `--estimate ""`
/// -> "empty duration"). That minted a nameless project bucket: `projects`
/// never lists it, `report` shows a blank-named row containing the task. Two
/// different states for "no project", one of them invisible. `--clear project`
/// stays the single sanctioned way to empty it.
#[test]
fn modify_rejects_an_empty_project_rather_than_minting_a_nameless_bucket() {
    let e = engine();
    e.project_create(&json!({ "name": "realwork" })).unwrap(); // D23
    e.task_add(&json!({ "title": "x", "project": "realwork" }))
        .unwrap();

    let err = e
        .task_modify(&json!({ "ref": 1, "set": { "project": "" } }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("clear"),
        "must point at --clear, got: {}",
        err.message
    );

    // Whitespace is the same mistake wearing a hat.
    assert!(e
        .task_modify(&json!({ "ref": 1, "set": { "project": "   " } }))
        .is_err());

    // The rejected write changed nothing.
    assert_eq!(
        e.task_get(&json!({ "ref": 1 })).unwrap()["project"],
        json!("realwork")
    );

    // And the sanctioned path still empties the field to a real NULL.
    e.task_modify(&json!({ "ref": 1, "set": { "project": null } }))
        .unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": 1 })).unwrap()["project"],
        json!(null)
    );
}

// ---- D21/D22: the default project is explicitly controlled -------------------

/// The bug the user hit by hand: `init work` then `init prive.klussen` silently
/// moved the default, so a bare `add` landed in the project they had just
/// finished creating rather than the one they were working in.
#[test]
fn project_create_claims_the_default_only_when_none_is_set() {
    let e = engine();

    // First project ever: claims the default (the helpful bit).
    let first = e.project_create(&json!({ "name": "work" })).unwrap();
    assert_eq!(
        first["default"], true,
        "the first project must claim the default"
    );
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work"));

    // Second project: does NOT steal it, and says so in its own result.
    assert_eq!(first["current_default"], "work");
    let second = e
        .project_create(&json!({ "name": "prive.klussen" }))
        .unwrap();
    assert_eq!(
        second["default"], false,
        "a later project must not steal the default"
    );
    assert_eq!(
        second["current_default"], "work",
        "must report what the default still is, not just that it did not move"
    );
    assert_eq!(
        e.default_project().unwrap().as_deref(),
        Some("work"),
        "default was stolen"
    );

    // The behavior that actually matters: a bare add still lands in `work`.
    let added = e.task_add(&json!({ "title": "a task" })).unwrap();
    assert_eq!(
        added["project"], "work",
        "bare add landed in the wrong project"
    );
}

/// `project.use` is the one explicit way to move the default.
#[test]
fn project_use_switches_the_default_and_reports_the_previous_one() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "prive.klussen" }))
        .unwrap();

    let r = e.project_use(&json!({ "name": "prive.klussen" })).unwrap();
    assert_eq!(r["name"], "prive.klussen");
    assert_eq!(r["default"], true);
    assert_eq!(
        r["previous"], "work",
        "the switch must name what it replaced"
    );
    assert_eq!(
        e.default_project().unwrap().as_deref(),
        Some("prive.klussen")
    );

    let added = e.task_add(&json!({ "title": "klus" })).unwrap();
    assert_eq!(added["project"], "prive.klussen");

    // And back again - `previous` tracks the real prior value, not a guess.
    let back = e.project_use(&json!({ "name": "work" })).unwrap();
    assert_eq!(back["previous"], "prive.klussen");
    assert_eq!(
        e.task_add(&json!({ "title": "werk" })).unwrap()["project"],
        "work"
    );
}

/// THE invariant: every mutation writes its event row in the same transaction.
/// `project.use` is a mutation, so it owes the log an event.
#[test]
fn project_use_records_an_event() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "other" })).unwrap();
    e.project_use(&json!({ "name": "other" })).unwrap();

    let events = e.event_list(&json!({ "limit": 50 })).unwrap();
    let uses: Vec<_> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|ev| ev["op"] == "use")
        .collect();
    assert_eq!(
        uses.len(),
        1,
        "expected exactly one `use` event, got {uses:?}"
    );
    assert_eq!(uses[0]["entity"], "project");
    assert_eq!(uses[0]["payload"]["name"], "other");
    assert_eq!(uses[0]["payload"]["previous"], "work");
}

/// Validate at the edge: naming a project that does not exist is an error that
/// names it, not a silent write of a ghost default.
#[test]
fn project_use_rejects_an_unknown_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();

    let err = e.project_use(&json!({ "name": "nope" })).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::NotFound,
        "same code project.archive gives"
    );
    assert!(
        err.message.contains("nope"),
        "the error must name it: {}",
        err.message
    );
    // The rejected write changed nothing.
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work"));

    // "" is still a bad_request: req_str rejects it before any lookup, so
    // `use "$UNSET_VAR"` can never write a ghost default (D13/D18).
    let empty = e.project_use(&json!({ "name": "" })).unwrap_err();
    assert_eq!(empty.code, ErrorCode::BadRequest);
    // D23: a whitespace-only name is no longer special-cased here. `create`
    // rejects such a name, so nothing can hold it, and the lookup answers
    // truthfully: it names no project. `use` refusing a name `init` would
    // accept was the one-way door D21 exists to remove, at a narrower edge.
    let blank = e.project_use(&json!({ "name": "   " })).unwrap_err();
    assert_eq!(blank.code, ErrorCode::NotFound);
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work"));
}

/// D22: archived means out of rotation, so it cannot be pointed at.
#[test]
fn project_use_rejects_an_archived_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "old" })).unwrap();
    e.project_archive(&json!({ "name": "old" })).unwrap();

    let err = e.project_use(&json!({ "name": "old" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains("archived"),
        "must explain why: {}",
        err.message
    );
    assert!(err.message.contains("old"));
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work"));
}

/// D22, the other half: archiving the *current* default un-points it, and says
/// so out loud rather than leaving a default aimed at a retired project.
#[test]
fn archiving_the_default_project_clears_the_default_and_reports_it() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "side" })).unwrap();
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work"));

    // Archiving a NON-default project leaves the default alone and says nothing.
    let quiet = e.project_archive(&json!({ "name": "side" })).unwrap();
    assert_eq!(quiet["default_cleared"], false);
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work"));

    // Archiving the default clears it, visibly.
    let loud = e.project_archive(&json!({ "name": "work" })).unwrap();
    assert_eq!(
        loud["default_cleared"], true,
        "silently keeping a retired default is the bug"
    );
    assert_eq!(e.default_project().unwrap(), None);

    // A bare add is now projectless - the same state a fresh store is in.
    let added = e.task_add(&json!({ "title": "homeless" })).unwrap();
    assert_eq!(added["project"], json!(null));

    // No default => the next create claims it, exactly like a fresh store.
    e.project_create(&json!({ "name": "work2" })).unwrap();
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work2"));
}

/// D22 in the direction the first cut of it missed: `project.archive` on a
/// project that is ALREADY archived is a `conflict`, not a second `ok`.
///
/// The defect this pins shipped and was found by review: the method ran
/// `UPDATE projects SET archived = 1` without reading the prior value and
/// returned `{"archived": true, "default_cleared": false}` regardless, so the
/// run that retired a project and the fourth run that did nothing at all were
/// byte-identical answers. That is D34's unfalsifiable write, and it landed on
/// the one surface D22 names as the place "where did the default go" is
/// answered: the event log grew an `archive` row per repeat.
///
/// It was also the single counterexample to D22's own sentence — repeated in
/// `tasqx archive --help` and in the user guide — that no verb may name an
/// archived project.
#[test]
fn archiving_an_already_archived_project_is_refused_and_writes_nothing() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "old" })).unwrap();
    e.project_archive(&json!({ "name": "old" })).unwrap();

    let err = e.project_archive(&json!({ "name": "old" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains("already archived"),
        "must say the state was already reached, not merely that something is wrong: {}",
        err.message
    );
    assert!(
        err.message.contains("old"),
        "must name the project the caller got wrong: {}",
        err.message
    );
    // The refusal must be distinguishable from the archive that did the work,
    // which is the whole point: `use` on the same project says something else.
    let use_err = e.project_use(&json!({ "name": "old" })).unwrap_err();
    assert_ne!(
        err.message, use_err.message,
        "the two archived-project refusals must say which one happened"
    );

    // Nothing was written: exactly one `archive` event for a project archived
    // once, against the one `create`. Counted rather than asserted on the top
    // row, because the defect appended a row and left the top one identical.
    let events = e
        .event_list(&json!({ "entity": "project", "limit": 100 }))
        .unwrap();
    let ops: Vec<&str> = events["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|ev| ev["payload"]["name"] == json!("old"))
        .map(|ev| ev["op"].as_str().expect("op"))
        .collect();
    assert_eq!(
        ops.iter().filter(|o| **o == "archive").count(),
        1,
        "a project archived once must have one archive event, got {ops:?}"
    );

    // And the store still reads the same: still archived, default untouched.
    let all = e
        .project_list(&json!({ "include_archived": true }))
        .unwrap();
    let old_row = all["projects"]
        .as_array()
        .expect("projects array")
        .iter()
        .find(|r| r["name"] == json!("old"))
        .expect("old is still listed by --all");
    assert_eq!(old_row["archived"], json!(true));
    assert_eq!(e.default_project().unwrap().as_deref(), Some("work"));
}

/// The invisible-field trap: the default drives where a bare add lands, so every
/// read surface that lists projects must show which one it is.
#[test]
fn project_list_marks_the_default_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "prive.klussen" }))
        .unwrap();

    let listed = e.project_list(&json!({})).unwrap();
    let rows = listed["projects"].as_array().unwrap();
    let default_rows: Vec<_> = rows
        .iter()
        .filter(|p| p["default"] == json!(true))
        .collect();
    assert_eq!(
        default_rows.len(),
        1,
        "exactly one row must be marked default: {rows:?}"
    );
    assert_eq!(default_rows[0]["name"], "work");
    // Every row must carry the field, not just the winner - an absent field and
    // a false one are different things to a machine consumer.
    for p in rows {
        assert!(
            p.get("default")
                .map(serde_json::Value::is_boolean)
                .unwrap_or(false),
            "row missing `default`: {p:?}"
        );
    }

    // It tracks `use`.
    e.project_use(&json!({ "name": "prive.klussen" })).unwrap();
    let listed = e.project_list(&json!({})).unwrap();
    let marked: Vec<_> = listed["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["default"] == json!(true))
        .map(|p| p["name"].clone())
        .collect();
    assert_eq!(marked, vec![json!("prive.klussen")]);
}

/// The other read surface for the same fact: `task.add` must report the project
/// it chose. "silently lands in prive.klussen" is precisely this field missing.
#[test]
fn task_add_reports_the_project_it_landed_in() {
    let e = engine();
    // No default yet: projectless, and it says so rather than omitting the key.
    let orphan = e.task_add(&json!({ "title": "no project" })).unwrap();
    assert!(
        orphan.get("project").is_some(),
        "the field must always be present"
    );
    assert_eq!(orphan["project"], json!(null));

    e.project_create(&json!({ "name": "work" })).unwrap();
    assert_eq!(
        e.task_add(&json!({ "title": "inherited" })).unwrap()["project"],
        "work"
    );
    // Explicit still wins, and is still reported (and must exist - D23).
    e.project_create(&json!({ "name": "other" })).unwrap();
    let explicit = e
        .task_add(&json!({ "title": "explicit", "project": "other" }))
        .unwrap();
    assert_eq!(explicit["project"], "other");
}

/// `project.use` is reachable through the one dispatch table every surface
/// shares, and is enumerated for feature detection.
#[test]
fn project_use_is_dispatchable_and_advertised() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "other" })).unwrap();

    let r = tasqx_core::dispatch(&e, "project.use", &json!({ "name": "other" })).unwrap();
    assert_eq!(r["name"], "other");
    assert_eq!(e.default_project().unwrap().as_deref(), Some("other"));

    let caps = e.capabilities().unwrap();
    let methods: Vec<&str> = caps["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert!(
        methods.contains(&"project.use"),
        "project.use must be advertised: {methods:?}"
    );
    assert_eq!(caps["default_project"], "other");
}

// ---- D23: the default and every explicit project name a project you can see --

/// A file-backed store path, unique per call (the legacy-repair test has to
/// close and reopen a store, which an in-memory one cannot survive).
fn temp_db_path(stem: &str) -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!("tasqx-{stem}-{pid}-{nanos}.db"))
        .to_string_lossy()
        .into_owned()
}

/// D23: a store written by OLD code can hold a default pointing at an archived
/// project — the old `create` let the newest project steal the key and the old
/// `archive` did not clear it. No sequence of *new* calls can reach that state,
/// so the legacy row is seeded directly: this must pin the repair, not the
/// writers that already uphold the invariant.
#[test]
fn a_default_left_pointing_at_an_archived_project_by_old_code_is_repaired_on_open() {
    let path = temp_db_path("legacy-default");
    {
        let e = Engine::open(&path).unwrap();
        e.project_create(&json!({ "name": "work" })).unwrap();
        e.project_create(&json!({ "name": "prive" })).unwrap();
        e.project_archive(&json!({ "name": "prive" })).unwrap();
        // Rewind to what the old binary would have left behind: `prive` stole
        // the default on create, and archiving it did not clear the key.
        e.conn()
            .execute(
                "INSERT OR REPLACE INTO config (key, value) VALUES ('default_project', 'prive')",
                [],
            )
            .unwrap();
    }

    // Reopening is the upgrade: the repair runs on the way in.
    let e = Engine::open(&path).unwrap();
    assert_eq!(
        e.default_project().unwrap(),
        None,
        "a default aimed at an archived project must not survive the open"
    );
    assert_eq!(
        e.capabilities().unwrap()["default_project"],
        json!(null),
        "capabilities must agree with the project list, not report a ghost"
    );
    let rows = e
        .project_list(&json!({ "include_archived": true }))
        .unwrap();
    let marked: Vec<_> = rows["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["default"] == json!(true))
        .collect();
    assert!(
        marked.is_empty(),
        "no row may claim a default the store does not have: {marked:?}"
    );

    // The behavior that actually bit: a bare add filed into the archived project.
    let added = e.task_add(&json!({ "title": "legacy orphan" })).unwrap();
    assert_eq!(
        added["project"],
        json!(null),
        "bare add landed in an archived project"
    );

    // And the store is not stranded: the next create claims the default again.
    e.project_create(&json!({ "name": "fresh" })).unwrap();
    assert_eq!(e.default_project().unwrap().as_deref(), Some("fresh"));
    let _ = std::fs::remove_file(&path);
}

/// The same repair for the other stale shape: a default naming a project row
/// that is not there at all.
#[test]
fn a_default_naming_a_missing_project_is_repaired_on_open() {
    let path = temp_db_path("ghost-default");
    {
        let e = Engine::open(&path).unwrap();
        e.project_create(&json!({ "name": "work" })).unwrap();
        e.conn()
            .execute(
                "INSERT OR REPLACE INTO config (key, value) VALUES ('default_project', 'vanished')",
                [],
            )
            .unwrap();
    }
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.default_project().unwrap(), None);
    assert_eq!(
        e.task_add(&json!({ "title": "x" })).unwrap()["project"],
        json!(null)
    );
    let _ = std::fs::remove_file(&path);
}

/// A healthy default is left alone by the repair — it must fix the broken shape,
/// not quietly wipe the setting on every open.
#[test]
fn a_live_default_survives_the_open_repair() {
    let path = temp_db_path("live-default");
    {
        let e = Engine::open(&path).unwrap();
        e.project_create(&json!({ "name": "work" })).unwrap();
        e.project_create(&json!({ "name": "prive" })).unwrap();
        e.project_use(&json!({ "name": "prive" })).unwrap();
    }
    let e = Engine::open(&path).unwrap();
    assert_eq!(e.default_project().unwrap().as_deref(), Some("prive"));
    assert_eq!(
        e.task_add(&json!({ "title": "x" })).unwrap()["project"],
        "prive"
    );
    let _ = std::fs::remove_file(&path);
}

/// D23: a whitespace-only project name is rejected where names are born. It used
/// to be accepted, claim the default, print as a blank row, and then be
/// unreachable by the one verb that selects projects.
#[test]
fn project_create_rejects_a_whitespace_only_name() {
    let e = engine();
    let err = e.project_create(&json!({ "name": "   " })).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::BadRequest,
        "D18's rule at the create edge"
    );
    // Nothing was written: no row, and no default claimed.
    assert_eq!(
        e.default_project().unwrap(),
        None,
        "a rejected create must not claim the default"
    );
    assert_eq!(
        count(
            &e.project_list(&json!({ "include_archived": true }))
                .unwrap()
        ),
        0
    );
    // "" is rejected by req_str, as it always was.
    assert_eq!(
        e.project_create(&json!({ "name": "" })).unwrap_err().code,
        ErrorCode::BadRequest
    );
}

/// D23: the create event says whether *this* create claimed the default. Its
/// siblings already record their effect on the key (`use` -> `previous`,
/// `archive` -> `default_cleared`); without this the log cannot answer "where
/// were bare adds landing?" for a store whose default was cleared and re-claimed.
#[test]
fn project_create_records_whether_it_claimed_the_default() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "side" })).unwrap();
    // Cleared by archiving the default, then re-claimed by a later create - the
    // sequence that makes "the first create ever" the wrong guess (D22).
    e.project_archive(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "third" })).unwrap();

    let events = e
        .event_list(&json!({ "limit": 50, "entity": "project" }))
        .unwrap();
    let creates: Vec<_> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|ev| ev["op"] == "create")
        .collect();
    assert_eq!(creates.len(), 3);
    let claimed_by = |name: &str| -> serde_json::Value {
        creates
            .iter()
            .find(|ev| ev["payload"]["name"] == name)
            .unwrap_or_else(|| panic!("no create event for {name}"))["payload"]["default"]
            .clone()
    };
    assert_eq!(
        claimed_by("work"),
        json!(true),
        "the first create claimed the default"
    );
    assert_eq!(
        claimed_by("side"),
        json!(false),
        "a create that did not claim it must say so"
    );
    assert_eq!(
        claimed_by("third"),
        json!(true),
        "re-claiming after a clear is a claim too"
    );
}

/// D23: an explicit `project` is validated exactly like `project.use`'s target.
/// A typo used to file the task into a bucket no project surface lists.
#[test]
fn task_add_rejects_an_unknown_explicit_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();

    let err = e
        .task_add(&json!({ "title": "ghost", "project": "totally-not-a-project" }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "same code project.use gives");
    assert!(
        err.message.contains("totally-not-a-project"),
        "must name it: {}",
        err.message
    );
    // The rejected add wrote nothing at all.
    assert_eq!(
        count(&e.task_list(&json!({})).unwrap()),
        0,
        "a rejected add must not write"
    );
}

/// D23 / D22's other half: an archived project cannot take new tasks, exactly as
/// it cannot be the default. `use <archived>` was a conflict while
/// `add --project <archived>` sailed through with exit 0.
#[test]
fn task_add_rejects_an_archived_explicit_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "prive.klussen" }))
        .unwrap();
    e.project_archive(&json!({ "name": "prive.klussen" }))
        .unwrap();

    let err = e
        .task_add(&json!({ "title": "into archived", "project": "prive.klussen" }))
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::Conflict,
        "same code project.use gives for an archived one"
    );
    assert!(
        err.message.contains("archived"),
        "must explain why: {}",
        err.message
    );
    assert!(err.message.contains("prive.klussen"));
    assert_eq!(count(&e.task_list(&json!({})).unwrap()), 0);

    // A live project still works, and the default path is untouched.
    assert_eq!(
        e.task_add(&json!({ "title": "ok", "project": "work" }))
            .unwrap()["project"],
        "work"
    );
}

/// The sibling path: `task.modify` moves a task between projects, so it owes the
/// same guard. Half-applying it is how the dependency-reader bug worked.
#[test]
fn task_modify_rejects_an_unknown_or_archived_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "old" })).unwrap();
    e.project_archive(&json!({ "name": "old" })).unwrap();
    let t = e
        .task_add(&json!({ "title": "x", "project": "work" }))
        .unwrap();
    let r = t["short_id"].clone();

    let ghost = e
        .task_modify(&json!({ "ref": r, "set": { "project": "nope" } }))
        .unwrap_err();
    assert_eq!(ghost.code, ErrorCode::NotFound);
    let archived = e
        .task_modify(&json!({ "ref": r, "set": { "project": "old" } }))
        .unwrap_err();
    assert_eq!(archived.code, ErrorCode::Conflict);

    // Neither rejected modify moved the task or bumped its rev.
    let got = e.task_get(&json!({ "ref": r })).unwrap();
    assert_eq!(got["project"], "work");
    assert_eq!(got["_rev"], 1);

    // Clearing the project is still allowed - null is not a project name.
    e.task_modify(&json!({ "ref": r, "set": { "project": null } }))
        .unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": r })).unwrap()["project"],
        json!(null)
    );
}

// ---- status write/read round trip -------------------------------------------

/// Every `Status` variant must be reachable through the API and read back as
/// itself. This closes a silent-corruption path that no test covered.
///
/// The engine WRITES status as bare SQL literals (`SET status='done'`, six of
/// them) while `storage.rs` READS with `Status::parse(..).unwrap_or(Pending)`.
/// Those are two independent hand-written spellings of the same five names,
/// joined by a fallback that cannot fail loudly. Rename a variant in `types.rs`
/// — exactly what `as_str_and_parse_round_trip` exists to protect against — and
/// the UPDATEs keep emitting the old string, `parse` returns `None`, and every
/// affected row silently becomes `pending`. A finished task would quietly
/// reappear as actionable work, with no error anywhere.
///
/// `as_str_and_parse_round_trip` cannot catch this: it checks the two matches in
/// `types.rs` against each other, never against what the writers actually emit.
/// This drives the real transitions through SQLite instead.
#[test]
fn every_status_survives_a_write_read_round_trip() {
    use tasqx_core::types::Status;

    // Exhaustive: a new variant fails to compile until someone names the
    // transition that reaches it, rather than silently going untested.
    for want in Status::ALL {
        let e = engine();
        let r = match want {
            Status::Pending => e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone(),
            Status::Backlog => e
                .task_add(&json!({ "title": "t", "wait": "2999-01-01T00:00:00Z" }))
                .unwrap()["short_id"]
                .clone(),
            Status::Active => {
                let r = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
                e.task_start(&json!({ "ref": r })).unwrap();
                r
            }
            Status::Done => {
                let r = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
                e.task_done(&json!({ "ref": r })).unwrap();
                r
            }
            Status::Cancelled => {
                let r = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
                e.task_cancel(&json!({ "ref": r })).unwrap();
                r
            }
        };

        let got = e.task_get(&json!({ "ref": r })).unwrap();
        assert_eq!(
            got["status"],
            want.as_str(),
            "a {} task did not read back as {} — the write literal and \
             Status::as_str have diverged, and the read fallback hid it",
            want.as_str(),
            want.as_str()
        );
    }
}

/// The transitions back out of a terminal state, for the same reason: `stop` and
/// `reopen` each carry their own `SET status='pending'` literal, and the silent
/// read fallback is *also* `Pending` — so a desync in exactly these two writes
/// would be invisible to the round trip above, which only ever asserts the
/// happy value they coincidentally share.
#[test]
fn stop_and_reopen_write_a_readable_pending() {
    use tasqx_core::types::Status;
    let e = engine();

    let started = e.task_add(&json!({ "title": "a" })).unwrap()["short_id"].clone();
    e.task_start(&json!({ "ref": started })).unwrap();
    e.task_stop(&json!({ "ref": started })).unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": started })).unwrap()["status"],
        Status::Pending.as_str()
    );

    let finished = e.task_add(&json!({ "title": "b" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": finished })).unwrap();
    e.task_reopen(&json!({ "ref": finished })).unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": finished })).unwrap()["status"],
        Status::Pending.as_str()
    );
}

// ---- B1: a status the reader does not recognize must not brick the store -----

/// Seed the one state no current writer can produce: a task row whose `status`
/// column holds text `Status::parse` rejects. `store.import` used to accept it,
/// so the stores most likely to hold one are exactly the stores an upgrade must
/// not lock the user out of.
fn store_with_an_unrecognized_status() -> (Engine, serde_json::Value) {
    let e = Engine::open_in_memory().unwrap();
    e.project_create(&json!({ "name": "work" })).unwrap();
    let sid = e.task_add(&json!({ "title": "important work" })).unwrap()["short_id"].clone();
    e.conn()
        .execute("UPDATE tasks SET status = 'Done'", [])
        .unwrap();
    (e, sid)
}

/// Every read path stays open. `export` matters most: it is the only way to get
/// data out of a store the reader dislikes, so making it fail leaves a user who
/// hit the *old* bug unable to read OR rescue their tasks.
#[test]
fn an_unrecognized_status_still_lists_shows_and_exports() {
    let (e, sid) = store_with_an_unrecognized_status();

    let list = e
        .task_list(&json!({}))
        .expect("list must survive an unreadable status");
    assert_eq!(
        list["tasks"].as_array().unwrap().len(),
        1,
        "the row must not vanish either"
    );
    let shown = e
        .task_get(&json!({ "ref": sid }))
        .expect("show must survive it");
    let exported = e
        .store_export(&json!({}))
        .expect("export is the escape hatch; it must survive");

    // The stored bytes reach the export verbatim — a rescue that silently
    // rewrote the value would lose the only evidence of what went wrong.
    assert_eq!(exported["tasks"][0]["status"], json!("Done"));
    assert_eq!(list["tasks"][0]["status"], json!("Done"));
    assert_eq!(shown["status"], json!("Done"));
}

/// The anomaly is *reported*, not laundered. Calling it `pending` invents open
/// work out of a row nobody could read, with `completed` still set and nothing
/// on any surface saying so — the exact bug this cluster set out to fix.
#[test]
fn an_unrecognized_status_is_reported_rather_than_passed_off_as_pending() {
    let (e, sid) = store_with_an_unrecognized_status();
    for (surface, task) in [
        ("task.get", e.task_get(&json!({ "ref": sid })).unwrap()),
        (
            "task.list",
            e.task_list(&json!({})).unwrap()["tasks"][0].clone(),
        ),
        (
            "store.export",
            e.store_export(&json!({})).unwrap()["tasks"][0].clone(),
        ),
    ] {
        assert_ne!(
            task["status"],
            json!("pending"),
            "{surface} laundered the anomaly"
        );
        assert_eq!(
            task["status_unrecognized"],
            json!(true),
            "{surface} must flag the anomaly for machine consumers: {task}"
        );
    }
}

/// The asymmetry is the point: refuse bad data at the door, never become unable
/// to read data already inside. Re-importing the rescue export therefore fails
/// loudly and names the value, which is what tells the user what to edit.
#[test]
fn the_rescue_export_is_still_refused_on_the_way_back_in() {
    let (e, _) = store_with_an_unrecognized_status();
    let exported = e.store_export(&json!({})).unwrap();
    let err = Engine::open_in_memory()
        .unwrap()
        .store_import(&exported)
        .expect_err("store.import must stay strict");
    assert!(
        err.message.contains("Done"),
        "the error must name the value: {}",
        err.message
    );
}

// ---- B2: store.import must pass the same date/duration gate as everyone else -

/// The one payload shape that used to walk straight past every validator: four
/// date-shaped fields read raw out of the JSON and handed to the INSERT. The
/// resulting store said `due whenever`, which no reader downstream can compare,
/// sort or render — the invisible-field failure this project keeps rebuilding.
fn payload_with(field: &str, value: &str) -> Value {
    json!({ "tasks": [{
        "id": "11111111-1111-4111-8111-111111111111",
        "short_id": 1,
        "title": "bad dates",
        "status": "pending",
        field: value,
    }]})
}

#[test]
fn store_import_refuses_dates_and_durations_it_cannot_parse() {
    for (field, value) in [
        ("due", "whenever"),
        ("scheduled", "nope"),
        ("wait", "nah"),
        ("estimate", "soonish"),
    ] {
        let e = engine();
        let err = e
            .store_import(&payload_with(field, value))
            .expect_err(&format!("store.import accepted {field} = {value:?}"));
        assert_eq!(err.code, ErrorCode::BadRequest);
        // Both halves matter: the field says which column, the value says which
        // line of a thousand-task export to edit.
        assert!(
            err.message.contains(field) && err.message.contains(value),
            "the error must name the field and the value: {}",
            err.message
        );
        // A rejected import writes nothing at all (one transaction).
        assert_eq!(
            count(&e.task_list(&json!({})).unwrap()),
            0,
            "a refused import left a row"
        );
    }
}

/// D12's unfiltered round trip must stay BYTE-identical, so routing these four
/// fields through a *normalizing* parser has to be a no-op on values an export
/// wrote. It is only a no-op because the store already holds the canonical form
/// — asserting it here is what stops a future "friendlier" normalizer (local
/// time, a trailing `+00:00`, lowercased ISO durations) from silently breaking
/// the contract while every other test still passes.
#[test]
fn the_date_gate_leaves_an_export_import_round_trip_byte_identical() {
    let a = engine();
    // Deliberately mixed input forms — bare date, naive datetime, full RFC3339,
    // human duration — so the stored values are the parser's own output rather
    // than something that happened to be canonical already.
    a.task_add(&json!({
        "title": "every date field",
        "due": "2099-01-01",
        "scheduled": "2099-01-02T09:30",
        "wait": "2099-01-03T00:00:00Z",
        "estimate": "1h30m",
    }))
    .unwrap();
    a.task_add(&json!({ "title": "no dates at all" })).unwrap();

    let export_a = a.store_export(&json!({})).unwrap();
    let b = engine();
    b.store_import(&json!({ "tasks": export_a["tasks"].clone() }))
        .unwrap();
    let export_b = b.store_export(&json!({})).unwrap();

    assert_eq!(
        serde_json::to_string(&export_a).unwrap(),
        serde_json::to_string(&export_b).unwrap(),
        "the date/duration gate perturbed an unfiltered round trip (D12)"
    );
}

/// The B1 asymmetry, applied to dates: a legacy store written before validation
/// existed must stay readable and exportable, and the export must carry the
/// offending bytes verbatim so the user can see what to fix. Import is the
/// strict door — it refuses and names the value. Same story, same shape.
#[test]
fn a_legacy_bad_date_still_exports_verbatim_and_is_refused_on_the_way_back_in() {
    let e = engine();
    e.task_add(&json!({ "title": "written before the gate existed" }))
        .unwrap();
    e.conn()
        .execute("UPDATE tasks SET due = 'whenever'", [])
        .unwrap();

    let exported = e
        .store_export(&json!({}))
        .expect("export is the escape hatch; it must survive");
    assert_eq!(
        exported["tasks"][0]["due"],
        json!("whenever"),
        "export must not launder it"
    );

    let err = engine()
        .store_import(&exported)
        .expect_err("store.import must stay strict");
    assert!(
        err.message.contains("whenever"),
        "the error must name the value: {}",
        err.message
    );
}

/// B2 closed `due`/`scheduled`/`wait`/`estimate` and stopped there, so
/// `created`, `modified` and `completed` — written by the same export, read by
/// the same INSERT — walked straight past the gate. `"created":"not-a-date"`
/// imported with rc=0 and came back out of the next export verbatim, and
/// `created` is worse than the rest: `urgency::score` reads it, so the garbage
/// also silently flattened the ranking every list is sorted by.
///
/// The field list is DERIVED from a real export rather than hand-written: any
/// future timestamp column an export starts emitting joins this guard the day
/// it appears, instead of the day someone remembers to add it here. That is the
/// difference between a guard and a list that rots (D30).
#[test]
fn store_import_gates_every_timestamp_field_an_export_emits() {
    let seed = engine();
    seed.task_add(
        &json!({ "title": "carries every timestamp", "due": "2099-01-01",
    // `wait`/`scheduled` in the PAST: they still export as RFC3339 instants,
    // but leave the task pending rather than backlog, so `done` below is a legal
    // transition and `completed` actually gets filled.
                           "scheduled": "2020-01-02", "wait": "2020-01-03" }),
    )
    .unwrap();
    seed.task_done(&json!({ "ref": "1" })).unwrap(); // fills `completed`
    let exported = seed.store_export(&json!({})).unwrap();
    let task = exported["tasks"][0].as_object().expect("one exported task");

    // "Timestamp-shaped" is decided by the exported VALUE, not by a name list:
    // anything the store wrote as an RFC3339 instant is a field a reader will
    // later parse, and therefore a field import must not accept garbage in.
    let stamped: Vec<String> = task
        .iter()
        .filter(|(_, v)| {
            v.as_str()
                .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                .is_some()
        })
        .map(|(k, _)| k.clone())
        .collect();
    for want in [
        "created",
        "modified",
        "completed",
        "due",
        "scheduled",
        "wait",
    ] {
        assert!(
            stamped.contains(&want.to_string()),
            "export stopped emitting {want}: {stamped:?}"
        );
    }

    for field in &stamped {
        let mut bad = task.clone();
        bad.insert(field.clone(), json!("not-a-date"));
        let e = engine();
        let err = e
            .store_import(&json!({ "tasks": [Value::Object(bad)] }))
            .expect_err(&format!("store.import accepted {field} = \"not-a-date\""));
        assert_eq!(err.code, ErrorCode::BadRequest, "{field}: {}", err.message);
        assert!(
            err.message.contains(field.as_str()) && err.message.contains("not-a-date"),
            "the error must name the field and the value: {}",
            err.message
        );
        // One transaction: a refused import leaves the store untouched, so the
        // garbage cannot survive as a half-written row either.
        assert_eq!(
            count(&e.task_list(&json!({})).unwrap()),
            0,
            "{field}: a refused import left a row"
        );
    }
}

// ---- E2: tracked time has a read surface ------------------------------------

/// `tracked_seconds` drove `report.summary`'s `tracked_total` and nothing else.
/// Every read surface a person actually uses — `task.get`, `task.list`, and the
/// `show` detail rendered from them — omitted it, so the one question a timer
/// exists to answer ("how long have I spent on this?") could only be answered
/// by grouping the task into a report of its own.
///
/// `tracked` is the STORED total and excludes any interval still running, which
/// is why `active_since` ships with it: the two together are the whole truth and
/// the running part stays derivable. Folding the open interval into `tracked`
/// here would make `show` disagree with `report` about the same task, trading a
/// missing number for two numbers that contradict each other.
#[test]
fn tracked_time_and_active_since_reach_the_read_surfaces() {
    let e = engine();
    let a = e.task_add(&json!({ "title": "timed" })).unwrap();
    let r = json!({ "ref": a["short_id"].clone() });

    // Nothing tracked yet: the field is present and zero, not absent. An absent
    // key would make "never started" indistinguishable from "this build has no
    // such field" for a machine reader.
    let got = e.task_get(&r).unwrap();
    assert_eq!(
        got["tracked"], "PT0S",
        "task.get must publish tracked from the start"
    );
    assert_eq!(
        got["active_since"],
        Value::Null,
        "an idle task has no open interval"
    );

    // While running: the stored total is still zero, and `active_since` is what
    // says the clock is moving.
    e.task_start(&r).unwrap();
    let running = e.task_get(&r).unwrap();
    assert!(
        running["active_since"].as_str().is_some(),
        "an active task must expose when its interval opened: {running}"
    );

    // Stopping closes the interval into the stored total.
    e.task_stop(&r).unwrap();
    let stopped = e.task_get(&r).unwrap();
    assert_eq!(
        stopped["active_since"],
        Value::Null,
        "stop must clear the open interval"
    );
    let tracked = stopped["tracked"]
        .as_str()
        .expect("tracked must be an ISO duration string");
    assert!(
        tracked.starts_with("PT"),
        "tracked must be ISO-8601 like `estimate`: {tracked}"
    );

    // The same fields must be projectable from task.list, which is the surface a
    // script reads. `fields` returns only what it can find, so a missing key is
    // a silent empty object rather than an error - hence an explicit check.
    let listed = e
        .task_list(&json!({ "fields": ["short_id", "tracked", "active_since"] }))
        .unwrap();
    let t0 = &listed["tasks"][0];
    assert_eq!(
        t0["tracked"], stopped["tracked"],
        "task.list must agree with task.get on tracked"
    );
    assert!(
        t0.get("active_since").is_some(),
        "task.list must be able to project active_since"
    );
}

// ---- E3: the computed `blocked` flag survives into the projection -----------

/// `task.list` computed `blocked` for every row (the filter grammar needs it),
/// used it, then threw it away — so `@blocked` could FILTER on a fact that
/// `fields:["blocked"]` could not RETURN. A caller could ask "which of these are
/// blocked" only by issuing one `task.get` per row.
///
/// The export shape is deliberately NOT part of this: `store.export` builds its
/// own §3 object, and D12 makes an unfiltered export a byte-identical round
/// trip. That is asserted here rather than assumed.
#[test]
fn blocked_is_projectable_from_task_list_without_disturbing_export() {
    let e = engine();
    let blocker = e.task_add(&json!({ "title": "blocker" })).unwrap();
    let dependent = e.task_add(&json!({ "title": "dependent" })).unwrap();
    e.dependency_add(&json!({
        "ref": dependent["short_id"].clone(),
        "depends_on": blocker["short_id"].clone(),
    }))
    .unwrap();

    let listed = e
        .task_list(&json!({ "fields": ["short_id", "blocked"], "sort": ["short_id"] }))
        .unwrap();
    let tasks = listed["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks[0]["blocked"], false,
        "the blocker itself is not blocked: {listed}"
    );
    assert_eq!(
        tasks[1]["blocked"], true,
        "the dependent IS blocked and must say so: {listed}"
    );

    // The filter and the projection must be reading the same fact, not two
    // implementations of it that can drift apart.
    let filtered = e
        .task_list(&json!({ "filter": "@blocked", "fields": ["short_id"] }))
        .unwrap();
    assert_eq!(count(&filtered), 1);
    assert_eq!(filtered["tasks"][0]["short_id"], tasks[1]["short_id"]);

    // D12: export -> import -> export is byte-identical, and `blocked` (a
    // derived fact, not stored state) never enters the document.
    let first = e.store_export(&json!({})).unwrap();
    for t in first["tasks"].as_array().unwrap() {
        assert!(
            t.get("blocked").is_none(),
            "export must not carry the derived blocked flag: {t}"
        );
    }
    let e2 = engine();
    e2.store_import(&json!({ "tasks": first["tasks"].clone() }))
        .unwrap();
    let second = e2.store_export(&json!({})).unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap(),
        "carrying blocked into the list projection changed the export round trip"
    );
}

// ---- E7: an unknown sort key is refused, not silently ignored ---------------

/// `compare_by` matched known keys and fell through to `Ordering::Equal` for
/// everything else, so `sort:["bogus"]` returned rows in whatever order the
/// remaining keys (or none) produced — a different question answered with exit
/// 0. Same family as D27's unknown filter token: on a READ path nothing is lost
/// by refusing, and the caller learns immediately instead of trusting an order
/// that was never applied.
#[test]
fn an_unknown_sort_key_is_rejected_and_every_published_one_works() {
    let e = engine();
    e.task_add(&json!({ "title": "a" })).unwrap();
    e.task_add(&json!({ "title": "b" })).unwrap();

    let err = e
        .task_list(&json!({ "sort": ["bogus"] }))
        .expect_err("an unknown sort key must not be silently ignored");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("bogus"),
        "the error must name the offending key: {}",
        err.message
    );
    for k in tasqx_core::engine::SORT_KEYS {
        assert!(
            err.message.contains(k),
            "the error must list the way out ({k}): {}",
            err.message
        );
    }

    // A descending prefix must not smuggle an unknown key past the check.
    let err = e
        .task_list(&json!({ "sort": ["-nope"] }))
        .expect_err("`-nope` must be refused too");
    assert!(
        err.message.contains("nope"),
        "the `-` prefix must be stripped before naming: {}",
        err.message
    );

    // Every published key is genuinely sortable — the constant must not grow a
    // name that `compare_by` does not implement, which would re-open the same
    // silent drop through the front door.
    for k in tasqx_core::engine::SORT_KEYS {
        for spelling in [k.to_string(), format!("-{k}")] {
            let r = e
                .task_list(&json!({ "sort": [spelling.clone()] }))
                .unwrap_or_else(|e| {
                    panic!("published sort key {spelling} was rejected: {}", e.message)
                });
            assert_eq!(count(&r), 2, "sort by {spelling} lost rows");
        }
    }
}

// ---- G1: an unknown `fields` key is refused, not silently dropped -----------

/// The projection loop did `if let Some(v) = full.get(k)` and dropped anything
/// that missed, so `fields:["short_id","titel"]` returned rows without the
/// field, `ok: true`, and no way to tell a typo from an empty column. A script
/// built on it renders blanks forever.
///
/// Fourth instance of one shape (D27's filter token, `!priority`, SORT_KEYS):
/// on a READ path nothing is lost by refusing — the caller retypes — while a
/// silent wrong answer is unfalsifiable.
#[test]
fn an_unknown_fields_key_is_rejected_and_every_published_one_projects() {
    let e = engine();
    e.task_add(&json!({ "title": "write the report" })).unwrap();

    let err = e
        .task_list(&json!({ "fields": ["short_id", "titel"] }))
        .expect_err("an unknown field must not be silently dropped");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("titel"),
        "the error must name the offending key: {}",
        err.message
    );
    assert!(
        err.message.contains("title"),
        "the error must list the way out: {}",
        err.message
    );

    // Every published name really is a key the projection emits, and asking for
    // it returns it. `status_unrecognized` is the one field absent from a
    // well-formed row by design (D28), so it is proven separately below.
    for f in tasqx_core::engine::TASK_FIELDS.iter() {
        let r = e
            .task_list(&json!({ "fields": [f] }))
            .unwrap_or_else(|e| panic!("published field {f} was rejected: {}", e.message));
        assert_eq!(count(&r), 1, "projecting {f} lost rows");
        if f != "status_unrecognized" {
            assert!(
                r["tasks"][0].get(f).is_some(),
                "published field {f} projected nothing"
            );
        }
    }

    // Cross-check between two independently maintained constants: every key you
    // may SORT by must also be a key you may ASK FOR. Neither side derives from
    // the other, so this fails if either drifts.
    for k in tasqx_core::engine::SORT_KEYS {
        assert!(
            tasqx_core::engine::TASK_FIELDS.iter().any(|f| f == k),
            "sort key {k} is not a projectable field"
        );
    }
}

/// The same silent drop, one method over: `report.summary`'s `metrics`.
///
/// `SUMMARY_METRICS` was already published and the MCP schema already renders
/// it as an `enum` — so an agent is told the set is closed while the engine
/// filtered unknown names out with `filter_map` and answered `ok`. A caller
/// asking for `overdeu` got a table with the column missing and no reason to
/// doubt it. Fixed against the constant that already existed.
#[test]
fn an_unknown_metric_is_rejected_rather_than_filtered_out() {
    let e = engine();
    e.task_add(&json!({ "title": "a" })).unwrap();

    let err = e
        .report_summary(&json!({ "group_by": "project", "metrics": ["count", "overdeu"] }))
        .expect_err("an unknown metric must not be silently dropped");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("overdeu"),
        "the error must name it: {}",
        err.message
    );
    for m in tasqx_core::engine::SUMMARY_METRICS {
        assert!(
            err.message.contains(m),
            "the error must list the way out ({m}): {}",
            err.message
        );
    }

    for m in tasqx_core::engine::SUMMARY_METRICS {
        e.report_summary(&json!({ "group_by": "project", "metrics": [m] }))
            .unwrap_or_else(|e| panic!("published metric {m} was rejected: {}", e.message));
    }
}

// ---- D34: the import document is strict one level down ----------------------

/// D33 closed the params object and stopped at the top level. A task object
/// inside `tasks` is the record itself, not an envelope, so a key it does not
/// read is data the import DROPS while answering `imported: 1`.
#[test]
fn an_unknown_key_on_an_imported_task_is_rejected_rather_than_ignored() {
    let e = engine();
    let err = e
        .store_import(&json!({ "tasks": [
            { "id": "019f6a0f-99df-7000-8000-000000000001", "short_id": 1, "title": "t",
              "tag": "red", "prioritee": "H" }
        ] }))
        .expect_err("a misspelled task field must not import as `ok`");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("019f6a0f-99df-7000-8000-000000000001"),
        "name the task: {}",
        err.message
    );
    assert!(
        err.message.contains("tag"),
        "name the field: {}",
        err.message
    );
    assert!(
        err.message.contains("prioritee"),
        "name every field: {}",
        err.message
    );
    assert!(
        err.message.contains("tags"),
        "name the way out: {}",
        err.message
    );
    // Nothing landed: the refusal is not cosmetic.
    assert_eq!(e.task_list(&json!({})).unwrap()["count"], 0);
}

/// `urgency` is emitted by every export and deliberately NOT read back (it is
/// derived), so it must be accepted-and-ignored rather than refused — the one
/// case where "the code does not read it" is correct behaviour, not a drop.
#[test]
fn a_derived_export_only_field_is_accepted_by_the_import_key_gate() {
    let e = engine();
    e.store_import(&json!({ "tasks": [
        { "id": "019f6a0f-99df-7000-8000-000000000002", "short_id": 1, "title": "t",
          "urgency": 9.5, "status_unrecognized": false }
    ] }))
    .expect("a field an export emits must stay importable");
}

/// Worse than a drop: a non-object annotation entry MINTED a blank annotation
/// with a fresh uuid, so `[42, "note"]` became two empty rows and the note
/// itself vanished. `Value::get` answers `None` on a non-object, so every
/// field fell back to its default and nothing was refused.
#[test]
fn a_non_object_annotation_entry_is_rejected_rather_than_minting_a_blank_one() {
    let e = engine();
    let err = e
        .store_import(&json!({ "tasks": [
            { "id": "019f6a0f-99df-7000-8000-000000000003", "short_id": 1, "title": "t",
              "annotations": [42] }
        ] }))
        .expect_err("a non-object annotation must not become a blank annotation");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("019f6a0f-99df-7000-8000-000000000003"),
        "name the task: {}",
        err.message
    );
    assert!(
        err.message.contains("annotations"),
        "name the field: {}",
        err.message
    );
    assert_eq!(e.task_list(&json!({})).unwrap()["count"], 0);
}

/// The same hole with the right container type: `{"text":"hello"}` imported as
/// one empty annotation, so the body the caller sent was lost at exit 0.
#[test]
fn an_unknown_key_on_an_imported_annotation_is_rejected_rather_than_ignored() {
    let e = engine();
    let err = e
        .store_import(&json!({ "tasks": [
            { "id": "019f6a0f-99df-7000-8000-000000000004", "short_id": 1, "title": "t",
              "annotations": [{ "text": "hello" }] }
        ] }))
        .expect_err("a misspelled annotation field must not import as `ok`");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("text"),
        "name the field: {}",
        err.message
    );
    assert!(
        err.message.contains("body"),
        "name the way out: {}",
        err.message
    );
    assert_eq!(e.task_list(&json!({})).unwrap()["count"], 0);
}

/// A non-object task entry was refused, but by the WRONG rule: `req_str` said
/// "missing or empty required field: id" for a payload whose problem is that it
/// is a string. The message sent the reader hunting for an `id` key in a value
/// that can never have one.
#[test]
fn a_non_object_task_entry_is_refused_naming_its_type() {
    let e = engine();
    let err = e.store_import(&json!({ "tasks": ["nope"] })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("object"),
        "name what was expected: {}",
        err.message
    );
    assert!(
        !err.message.contains("missing"),
        "do not misdiagnose it as absence: {}",
        err.message
    );
}

/// D34's drift guard, and the reason the key table is safe to exist at all: it
/// is checked against a REAL export rather than against a reading of the
/// import code, so a field added to `export_task` tomorrow either joins the
/// table or turns the suite red. A second copy nothing compares is the failure
/// this project keeps paying for (D30: derive it).
///
/// Both directions matter. A key an export emits and the table omits makes the
/// round trip D12 guarantees fail outright — the strict gate would refuse our
/// own output. A key the table declares and no export emits is a name nothing
/// will ever send, i.e. a typo that has quietly become part of the contract.
#[test]
fn the_import_key_table_matches_the_keys_an_export_actually_emits() {
    use std::collections::BTreeSet;

    let keys = |t: &serde_json::Value| -> BTreeSet<String> {
        t.as_object()
            .expect("an exported task is an object")
            .keys()
            .cloned()
            .collect()
    };

    // A task carrying every optional field, so the export emits every key it
    // can — an export of a bare task would silently under-report the set.
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    let t = e
        .task_add(&json!({
            "title": "everything", "project": "work", "priority": "H", "due": "2030-01-01",
            // Both dates PAST: a future `wait` OR `scheduled` makes the task
            // backlog (D29) and `done` is then a conflict, leaving `completed`
            // null — the one key this test most needs populated.
            "scheduled": "2020-01-02", "wait": "2020-01-01", "estimate": "4h",
            "tags": ["red"], "recurrence": "every 3 days", "remind": "-1h"
        }))
        .unwrap();
    let sid = t["short_id"].clone();
    e.annotation_add(&json!({ "ref": sid, "body": "note" }))
        .unwrap();
    // `tokens` is emitted only when a task has measurements (the
    // status_unrecognized rule), so the maximal fixture needs one.
    e.token_add(&json!({
        "ref": sid, "tool": "claude-code", "source": "log-parse",
        "model": "claude-fable-5", "input_tokens": 1, "confidence": "high"
    }))
    .unwrap();
    e.task_done(&json!({ "ref": sid })).unwrap();

    let exported = e.store_export(&json!({})).unwrap()["tasks"][0].clone();
    let mut emitted = keys(&exported);
    // `status_unrecognized` appears only on an anomalous row, so it needs the
    // one store that can produce it.
    let (bad, _) = store_with_an_unrecognized_status();
    emitted.extend(keys(&bad.store_export(&json!({})).unwrap()["tasks"][0]));

    // The two timing keys are conditional in the same way: `tracked_seconds`
    // appears only on a non-zero total and `active_since` only while a task is
    // running. Elapsed wall-clock is 0s in a test and there is no public way to
    // forge a total, so the store that emits both is seeded through `import` —
    // which also makes this guard prove the round trip accepts what it emits.
    let timed = engine();
    timed
        .store_import(&json!({ "tasks": [{
            "id": "019f0000-0000-7000-8000-0000000000t1",
            "short_id": 1,
            "title": "running",
            "status": "active",
            "tracked_seconds": 60,
            "active_since": "2020-01-01T00:00:00Z",
        }]}))
        .unwrap();
    emitted.extend(keys(&timed.store_export(&json!({})).unwrap()["tasks"][0]));

    let declared: BTreeSet<String> = tasqx_core::engine::IMPORT_TASK_KEYS
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        declared, emitted,
        "IMPORT_TASK_KEYS and store.export disagree — a key an export emits but the table omits \
         makes the gate refuse our own output (D12), and a key the table declares but no export \
         emits is a name nothing will ever send"
    );

    let ann_declared: BTreeSet<String> = tasqx_core::engine::IMPORT_ANNOTATION_KEYS
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        ann_declared,
        keys(&exported["annotations"][0]),
        "IMPORT_ANNOTATION_KEYS drifted"
    );

    // Same contract one child table over. `extra` is deliberately outside the
    // gate until something writes it (see IMPORT_TOKEN_KEYS), so this stays an
    // equality against what an export actually emits.
    let tok_declared: BTreeSet<String> = tasqx_core::engine::IMPORT_TOKEN_KEYS
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        tok_declared,
        keys(&exported["tokens"][0]),
        "IMPORT_TOKEN_KEYS drifted"
    );
}

/// D12's contract, re-checked against the strict gate: the export the gate now
/// polices is still exactly what the gate accepts, byte for byte.
#[test]
fn the_import_key_gate_leaves_an_unfiltered_round_trip_byte_identical() {
    let a = engine();
    a.project_create(&json!({ "name": "work" })).unwrap();
    let t = a
        .task_add(&json!({
            "title": "everything", "project": "work", "priority": "H", "due": "2030-01-01",
            "estimate": "4h", "tags": ["red", "blue"], "remind": "-1h"
        }))
        .unwrap();
    a.annotation_add(&json!({ "ref": t["short_id"].clone(), "body": "note" }))
        .unwrap();
    let export_a = a.store_export(&json!({})).unwrap();

    let b = engine();
    // D37: the WHOLE document, which is what an export now is. Handing back only
    // its `tasks` array is the legacy shape, and it round-trips its tasks but not
    // its projects — see the sibling below, where that difference is the point.
    b.store_import(&export_a).unwrap();
    assert_eq!(export_a, b.store_export(&json!({})).unwrap());
}

/// D37's compatibility half, pinned: a document with NO `projects` section — the
/// only shape any tasqx before this one ever wrote — must still import, and must
/// still leave a coherent store behind. Its tasks come back byte-identical; its
/// projects cannot, because the document never carried their identity, so the
/// row is inferred and says so through `projects_created`.
#[test]
fn a_document_with_no_projects_section_round_trips_its_tasks_and_infers_the_rest() {
    let a = engine();
    a.project_create(&json!({ "name": "work", "description": "day job" }))
        .unwrap();
    a.task_add(&json!({ "title": "everything", "project": "work" }))
        .unwrap();
    let export_a = a.store_export(&json!({})).unwrap();

    let b = engine();
    let r = b
        .store_import(&json!({ "tasks": export_a["tasks"].clone() }))
        .unwrap();
    assert_eq!(
        r["projects_created"],
        json!(["work"]),
        "the inferred row must be reported: {r}"
    );
    assert_eq!(r["projects_imported"], json!(0));

    let export_b = b.store_export(&json!({})).unwrap();
    assert_eq!(
        export_b["tasks"], export_a["tasks"],
        "the task half is still byte-identical"
    );
    // What a legacy document genuinely cannot carry, and therefore does not:
    // the description, the identity, and the default.
    assert_eq!(export_b["projects"][0]["name"], json!("work"));
    assert_eq!(export_b["projects"][0]["description"], Value::Null);
    assert_eq!(export_b["projects"][0]["archived"], json!(false));
    assert_eq!(export_b["default_project"], Value::Null);
    // But the store is usable: the name the import accepted is a name `add`
    // accepts, which is the whole reason the row is minted instead of refused.
    a.task_add(&json!({ "title": "x", "project": "work" }))
        .expect("source");
    b.task_add(&json!({ "title": "x", "project": "work" }))
        .expect("restored store must work too");
}

// ---- D35: an explicitly-supplied empty string is a PRESENT value ------------

/// The read half of D35, on a closed vocabulary. `entity: ""` used to reach
/// `opt_str`, which collapsed it to `None`, so the scoped query became the
/// UNSCOPED one and the caller who asked for a narrow slice got the whole log
/// at `ok: true` — while `entity: "tsak"` one character away was correctly
/// refused. The malformed value got feedback and the empty one did not.
#[test]
fn event_list_refuses_an_explicitly_empty_entity_instead_of_listing_everything() {
    let e = engine();
    e.task_add(&json!({ "title": "t" })).unwrap();
    let err = e
        .event_list(&json!({ "entity": "" }))
        .expect_err("`entity: \"\"` states a scope; it must not widen to the whole log");
    assert_eq!(err.code, ErrorCode::BadRequest);
    for ent in tasqx_core::Entity::ALL {
        assert!(
            err.message.contains(ent.as_str()),
            "must name the way out: {}",
            err.message
        );
    }
}

/// The read half again, on the other closed vocabulary. `group_by: ""` silently
/// grouped by `project` — the default — while `group_by: "bogus"` was refused.
#[test]
fn report_summary_refuses_an_explicitly_empty_group_by_instead_of_defaulting() {
    let e = engine();
    let err = e
        .report_summary(&json!({ "group_by": "" }))
        .expect_err("`group_by: \"\"` must not silently mean the default axis");
    assert_eq!(err.code, ErrorCode::BadRequest);
    for axis in tasqx_core::engine::SUMMARY_GROUP_BY {
        assert!(
            err.message.contains(axis),
            "must list the axes: {}",
            err.message
        );
    }
}

/// The write half, on `task.add`. Each of these silently WROTE A DEFAULT over a
/// value the caller stated: `project: ""` stored NULL (D18's rule, enforced on
/// `task.modify` since D18 and never on `add`), `due: ""` stored null, and the
/// closed vocabularies stored "none". This is D13 on the engine: a shell
/// variable that expands to nothing must not reach the column.
#[test]
fn task_add_refuses_an_explicitly_empty_optional_string() {
    let e = engine();
    for field in [
        "project",
        "priority",
        "due",
        "scheduled",
        "wait",
        "estimate",
        "recurrence",
        "remind",
    ] {
        let err = e
            .task_add(&json!({ "title": "t", field: "" }))
            .expect_err("an explicitly empty value must be refused, not replaced by a default");
        assert_eq!(err.code, ErrorCode::BadRequest, "for {field}");
    }
    assert_eq!(
        count(&e.task_list(&json!({})).unwrap()),
        0,
        "no task may have been written"
    );
}

/// The write half, on `store.import` — the surface D16/D28 hold to every
/// invariant the API enforces. Each pair here had the malformed value refused
/// and the empty one silently accepted as a default.
#[test]
fn store_import_refuses_an_explicitly_empty_field() {
    for (field, value) in [
        ("title", ""),
        ("status", ""),
        ("priority", ""),
        ("project", ""),
        ("due", ""),
        ("estimate", ""),
        ("recurrence", ""),
        ("remind", ""),
    ] {
        let e = engine();
        let mut task =
            json!({ "id": "019f6a0f-99df-7000-8000-0000000000a1", "short_id": 1, "title": "t" });
        task[field] = json!(value);
        let err = e
            .store_import(&json!({ "tasks": [task] }))
            .expect_err("an explicitly empty field must be refused, not written as a default");
        assert_eq!(err.code, ErrorCode::BadRequest, "for {field}");
        assert!(
            err.message.contains(field),
            "must name the field {field}: {}",
            err.message
        );
        assert_eq!(
            e.task_list(&json!({})).unwrap()["count"],
            0,
            "nothing written for {field}"
        );
    }
}

/// An annotation IS its body, so an empty one is a fabricated row — the same
/// thing `a_non_object_annotation_entry_is_rejected_rather_than_minting_a_blank_one`
/// refuses one shape over.
#[test]
fn store_import_refuses_an_empty_annotation_body_or_id() {
    for ann in [json!({ "body": "" }), json!({ "id": "", "body": "note" })] {
        let e = engine();
        let err = e
            .store_import(&json!({ "tasks": [
                { "id": "019f6a0f-99df-7000-8000-0000000000a2", "short_id": 1, "title": "t",
                  "annotations": [ann] }
            ] }))
            .expect_err("an empty annotation field must be refused, not defaulted");
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert_eq!(e.task_list(&json!({})).unwrap()["count"], 0);
    }
}

/// D18's rule at the one edge that still had no parser in front of it: a
/// nullable free-text column. `init x --description "$UNSET"` used to store
/// NULL, giving "no description" two spellings, one of them the caller's
/// stated intent thrown away.
#[test]
fn project_create_refuses_an_explicitly_empty_description() {
    let e = engine();
    let err = e
        .project_create(&json!({ "name": "work", "description": "" }))
        .expect_err("`description: \"\"` must not be laundered into NULL");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("description"),
        "name the field: {}",
        err.message
    );
}

/// The ONE recorded exception, pinned so it is not "fixed" later. D27 decided
/// that the empty filter matches everything — no filter means no filtering —
/// and the CLI sends exactly this on every unfiltered read, so `filter: ""` is
/// a genuine empty value, not an absent one that happens to agree.
#[test]
fn an_empty_filter_string_is_a_genuine_empty_filter_not_a_refusal() {
    let e = engine();
    e.task_add(&json!({ "title": "one" })).unwrap();
    e.task_add(&json!({ "title": "two" })).unwrap();
    assert_eq!(
        e.task_list(&json!({ "filter": "" })).unwrap(),
        e.task_list(&json!({})).unwrap(),
        "the empty filter must still mean no filtering (D27)"
    );
    assert_eq!(count(&e.task_list(&json!({ "filter": "" })).unwrap()), 2);
    assert_eq!(
        e.store_export(&json!({ "filter": "" })).unwrap()["tasks"],
        e.store_export(&json!({})).unwrap()["tasks"],
    );
    assert_eq!(
        e.report_summary(&json!({ "filter": "" })).unwrap()["groups"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

// ---- D36: one rule for a required string, at every door ----------------------

/// Every door that WRITES a required string must give the same answer to the
/// same blank input. The regression this pins: `task.modify` accepted a title
/// `task.add` and `store.import` refuse, so the API could mint a store that
/// exported cleanly and then failed its own import — D12's round trip broken
/// from inside. The whitespace half is the same bug one character over, and it
/// was already ruled on for a project name in D23.
///
/// The table is deliberately a cross product: the previous rounds shipped three
/// regressions whose single cause was a test that covered one spelling and not
/// its sibling, so neither the doors nor the spellings may be sampled.
#[test]
fn a_blank_required_string_is_refused_at_every_door() {
    // "" is the shape `--title "$UNSET"` produces; the rest are what a shell
    // hands over when the variable expands to padding rather than nothing.
    for blank in ["", " ", "   ", "\t", "\n", " \t \n "] {
        let e = engine();
        let seed = e.task_add(&json!({ "title": "seed" })).unwrap();
        let r#ref = seed["short_id"].clone();

        let add = e.task_add(&json!({ "title": blank })).unwrap_err();
        assert_eq!(
            add.code,
            ErrorCode::BadRequest,
            "task.add must refuse {blank:?}"
        );

        let modify = e
            .task_modify(&json!({ "ref": r#ref, "set": { "title": blank } }))
            .unwrap_err();
        assert_eq!(
            modify.code,
            ErrorCode::BadRequest,
            "task.modify must refuse {blank:?}"
        );

        let project = e.project_create(&json!({ "name": blank })).unwrap_err();
        assert_eq!(
            project.code,
            ErrorCode::BadRequest,
            "project.create must refuse {blank:?}"
        );

        let import = e
            .store_import(&json!({ "tasks": [{
                "id": "019f7eb6-0000-7000-8000-000000000001", "short_id": 9, "title": blank,
            }] }))
            .unwrap_err();
        assert_eq!(
            import.code,
            ErrorCode::BadRequest,
            "store.import must refuse {blank:?}"
        );

        // A refusal writes nothing: the seed keeps its title and its _rev.
        let got = e.task_get(&json!({ "ref": r#ref })).unwrap();
        assert_eq!(
            got["title"], "seed",
            "a refused modify must not have written"
        );
        assert_eq!(
            got["_rev"], 1,
            "a refused modify must not bump the revision"
        );
        assert_eq!(
            count(&e.task_list(&json!({})).unwrap()),
            1,
            "no blank task was created"
        );
    }
}

/// The N2a round trip, end to end through the core: anything the tool accepts
/// must survive export and re-import. Before the fix, `set: {title: ""}` was
/// `ok` and the resulting export was rejected by `store.import` naming the
/// task's uuid — a store you could write and could not restore.
#[test]
fn anything_modify_accepts_can_be_re_imported() {
    let e = engine();
    let seed = e.task_add(&json!({ "title": "alpha" })).unwrap();
    assert!(e
        .task_modify(&json!({ "ref": seed["short_id"].clone(), "set": { "title": "" } }))
        .is_err());

    // And the accepted spelling still round-trips, so the guard above is not
    // just "modify always fails".
    e.task_modify(&json!({ "ref": seed["short_id"].clone(), "set": { "title": "beta" } }))
        .unwrap();
    let exported = e.store_export(&json!({})).unwrap();
    let fresh = engine();
    fresh
        .store_import(&json!({ "tasks": exported["tasks"].clone() }))
        .unwrap();
    assert_eq!(
        fresh.store_export(&json!({})).unwrap()["tasks"],
        exported["tasks"],
        "D12: the unfiltered round trip is byte-identical"
    );
}

/// D28: the strictness belongs at the WRITE door. A store already holding a
/// blank or padded title — written by an older binary, or by another tool —
/// must still export, because refusing to read is how you lose the data the
/// rule was meant to protect. Seeded through the connection because no sequence
/// of current calls can reach that state any more.
#[test]
fn a_store_already_holding_a_blank_title_still_reads_and_exports() {
    for stored in ["", "   ", "  padded  "] {
        let e = engine();
        e.task_add(&json!({ "title": "seed" })).unwrap();
        e.conn()
            .execute("UPDATE tasks SET title = ?1", [stored])
            .unwrap();

        let listed = e.task_list(&json!({})).unwrap();
        assert_eq!(
            count(&listed),
            1,
            "a blank title must not vanish from a read"
        );
        assert_eq!(
            listed["tasks"][0]["title"], stored,
            "and it reads back verbatim"
        );

        let exported = e.store_export(&json!({})).unwrap();
        assert_eq!(
            exported["tasks"][0]["title"], stored,
            "export never refuses (D28)"
        );

        // The other half of D28's asymmetry, and the same shape the rescue
        // export already has for an unrecognized status: that export is refused
        // on the way back IN, loudly and naming the task, because import is a
        // write door. The data is never trapped — it exported, so the user can
        // see the blank title and fix it — but it is not laundered into a fresh
        // store either. A padded title is a legal value and re-imports cleanly.
        let fresh = engine();
        let back = fresh.store_import(&json!({ "tasks": exported["tasks"].clone() }));
        if stored.trim().is_empty() {
            let err = back.unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::BadRequest,
                "a blank title is refused at the door"
            );
            assert!(
                err.message.contains("title"),
                "and the message names what to edit: {}",
                err.message
            );
        } else {
            back.unwrap();
            assert_eq!(
                fresh.store_export(&json!({})).unwrap()["tasks"],
                exported["tasks"],
                "D12: a padded title survives the round trip byte-identical"
            );
        }
    }
}

/// The accepted-value half of the rule, stated so it is a decision and not a
/// side effect: a title with surrounding whitespace is STORED VERBATIM. Trim is
/// the emptiness test, never a normalisation — trimming on write would make
/// `store.import` rewrite the titles of a store that already holds padded rows,
/// and D12's byte-identical round trip would fail on exactly the legacy data
/// D28 says must survive.
#[test]
fn an_accepted_required_string_is_stored_verbatim_not_trimmed() {
    let e = engine();
    let added = e.task_add(&json!({ "title": "  padded  " })).unwrap();
    let r#ref = added["short_id"].clone();
    assert_eq!(
        e.task_get(&json!({ "ref": r#ref.clone() })).unwrap()["title"],
        "  padded  "
    );

    e.task_modify(&json!({ "ref": r#ref.clone(), "set": { "title": "\tmodified " } }))
        .unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": r#ref })).unwrap()["title"],
        "\tmodified "
    );

    let p = e.project_create(&json!({ "name": " spaced " })).unwrap();
    assert_eq!(
        p["name"], " spaced ",
        "a project name is not trimmed either"
    );

    // The whole point of not trimming: a padded row survives import unchanged.
    let exported = e.store_export(&json!({})).unwrap();
    let fresh = engine();
    fresh
        .store_import(&json!({ "tasks": exported["tasks"].clone() }))
        .unwrap();
    assert_eq!(
        fresh.store_export(&json!({})).unwrap()["tasks"],
        exported["tasks"]
    );
}

/// D36 stops at the write door, and this is where it stops. A store written
/// before D23 can hold a project whose name is whitespace; `project.use` and
/// `project.archive` are LOOKUPS, so they must keep taking the exact string
/// that names it. Applying the write rule there would make that project
/// impossible to select and impossible to retire — D21's one-way door rebuilt
/// by the check meant to prevent it, and D28's "a reader never refuses".
#[test]
fn a_lookup_door_still_accepts_a_name_no_write_door_would_mint() {
    let e = engine();
    // Seeded through the connection because `project.create` refuses it now —
    // which is the whole point: the state is legacy, and it must stay reachable.
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.conn()
        .execute(
            "INSERT INTO projects (id, name, archived, created) \
             VALUES ('p-legacy', '   ', 0, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

    // Selectable by the exact name it has.
    e.project_use(&json!({ "name": "   " })).unwrap();
    assert_eq!(
        e.default_project().unwrap().as_deref(),
        Some("   "),
        "the legacy project is reachable"
    );
    // And retirable, so there is a way out of it.
    e.project_archive(&json!({ "name": "   " })).unwrap();

    // "" is still refused at a lookup: it is `use "$UNSET"`, and it names nothing.
    assert_eq!(
        e.project_use(&json!({ "name": "" })).unwrap_err().code,
        ErrorCode::BadRequest
    );
    // A whitespace name matching no row is a truthful not_found, not a bad_request.
    assert_eq!(
        e.project_use(&json!({ "name": " \t " })).unwrap_err().code,
        ErrorCode::NotFound,
        "D23: emptiness is checked where names are born, not at the lookup"
    );
}

#[test]
fn config_read_failure_aborts_task_add() {
    let e = engine();
    e.conn()
        .execute_batch("DROP TABLE config")
        .expect("damage config schema");

    let err = e
        .task_add(&json!({ "title": "must not be written" }))
        .expect_err("a store read failure must not be treated as an absent default");
    assert_eq!(err.code, ErrorCode::Internal);

    let tasks: i64 = e
        .conn()
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .expect("task count");
    let events: i64 = e
        .conn()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("event count");
    assert_eq!(tasks, 0, "the failed read must abort the task write");
    assert_eq!(events, 0, "the failed read must abort its event too");
}
