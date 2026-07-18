//! Tests for the additive increment (DESIGN §4 method catalogue, §12-D8 filter
//! grammar, real dependency/blocked logic, default project, instant `due`
//! comparison, report.summary, and store export/import round-trip).

use jiff::{SignedDuration, Timestamp};
use serde_json::json;
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
    assert_eq!(e.default_project().as_deref(), Some("work.x"));

    // A bare add inherits the default.
    let added = e.task_add(&json!({ "title": "inherits" })).unwrap();
    let got = e.task_get(&json!({ "ref": added["short_id"].clone() })).unwrap();
    assert_eq!(got["project"], "work.x");

    // An explicit project always wins (and must exist - D23).
    e.project_create(&json!({ "name": "other" })).unwrap();
    let a2 = e.task_add(&json!({ "title": "explicit", "project": "other" })).unwrap();
    let g2 = e.task_get(&json!({ "ref": a2["short_id"].clone() })).unwrap();
    assert_eq!(g2["project"], "other");

    // Exposed via capabilities.
    let caps = e.capabilities();
    assert_eq!(caps["default_project"], "work.x");
}

// ---- A2: due comparison is by INSTANT, not lexicographic ---------------------

#[test]
fn due_before_after_compare_as_instants() {
    let e = engine();
    e.task_add(&json!({ "title": "due tomorrow", "due": plus_hours(24) })).unwrap();

    let next_week = plus_hours(24 * 7);
    let yesterday = plus_hours(-24);

    // Tomorrow is before next week => matches.
    let r = e.task_list(&json!({ "filter": format!("due.before:{next_week}") })).unwrap();
    assert_eq!(count(&r), 1);

    // Tomorrow is NOT before yesterday => no match.
    let r = e.task_list(&json!({ "filter": format!("due.before:{yesterday}") })).unwrap();
    assert_eq!(count(&r), 0);

    // due.after is the mirror.
    let r = e.task_list(&json!({ "filter": format!("due.after:{yesterday}") })).unwrap();
    assert_eq!(count(&r), 1);
}

#[test]
fn due_comparison_respects_timezone_offset_not_string_order() {
    // The classic gotcha: a due instant written in +12:00 is lexicographically
    // *after* a Z bound but temporally *before* it. Instant comparison must
    // match due.before; a naive string `<` would not.
    let e = engine();
    // 2026-07-16T06:00:00+12:00 == 2026-07-15T18:00:00Z
    e.task_add(&json!({ "title": "offset", "due": "2026-07-16T06:00:00+12:00" })).unwrap();

    let r = e.task_list(&json!({ "filter": "due.before:2026-07-15T20:00:00Z" })).unwrap();
    assert_eq!(count(&r), 1, "18:00Z is before 20:00Z regardless of string form");

    let r = e.task_list(&json!({ "filter": "due.before:2026-07-15T12:00:00Z" })).unwrap();
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
    let dep = e.dependency_add(&json!({ "ref": asid, "depends_on": bsid })).unwrap();
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
    assert_eq!(e.task_get(&json!({ "ref": asid })).unwrap()["blocked"], true);

    // Completing B unblocks A (reported by task.done).
    let done = e.task_done(&json!({ "ref": bsid })).unwrap();
    let unblocked = done["unblocked"].as_array().unwrap();
    assert!(unblocked.iter().any(|v| *v == asid), "A's short_id in unblocked");

    // A is no longer blocked and now appears in @working.
    assert_eq!(e.task_get(&json!({ "ref": asid })).unwrap()["blocked"], false);
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
        e.dependency_add(&json!({ "ref": dependent, "depends_on": blocker })).unwrap();

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
    assert_eq!(dependent["status"], "backlog", "fixture must actually be backlog");
    let dependent = dependent["short_id"].clone();
    let blocker = e.task_add(&json!({ "title": "blocker" })).unwrap()["short_id"].clone();
    e.dependency_add(&json!({ "ref": dependent, "depends_on": blocker })).unwrap();

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
    e.dependency_add(&json!({ "ref": asid, "depends_on": bsid })).unwrap();
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
    e.dependency_add(&json!({ "ref": a, "depends_on": b })).unwrap();
    assert_eq!(e.task_get(&json!({ "ref": a })).unwrap()["blocked"], true);

    // Cancel the blocker: A becomes actionable, and cancel reports the cascade.
    let res = e.task_cancel(&json!({ "ref": b })).unwrap();
    assert_eq!(
        e.task_get(&json!({ "ref": a })).unwrap()["blocked"], false,
        "a cancelled blocker releases its dependent (D11)"
    );
    assert!(
        res["unblocked"].as_array().unwrap().contains(&a),
        "task.cancel surfaces the unblock cascade"
    );
    // And A now appears in @working.
    let working = e.task_list(&json!({ "filter": "@working" })).unwrap();
    assert!(working["tasks"].as_array().unwrap().iter().any(|t| t["short_id"] == a));

    // Completing the blocker instead also clears it.
    let e2 = engine();
    let a2 = e2.task_add(&json!({ "title": "A2" })).unwrap()["short_id"].clone();
    let b2 = e2.task_add(&json!({ "title": "B2" })).unwrap()["short_id"].clone();
    e2.dependency_add(&json!({ "ref": a2, "depends_on": b2 })).unwrap();
    e2.task_done(&json!({ "ref": b2 })).unwrap();
    assert_eq!(e2.task_get(&json!({ "ref": a2 })).unwrap()["blocked"], false);
}

#[test]
fn unknown_filter_token_is_forgiving_and_matches_all() {
    // §12-D8 / filter.rs: unknown or malformed tokens are deliberately ignored
    // (treated as the always-true term). Pin the documented contract.
    let e = engine();
    e.task_add(&json!({ "title": "one" })).unwrap();
    e.task_add(&json!({ "title": "two" })).unwrap();

    // Typo (missing dot) and a nonsense token both match everything.
    let r = e.task_list(&json!({ "filter": "staus:pending" })).unwrap();
    assert_eq!(count(&r), 2);
    let r = e.task_list(&json!({ "filter": "totally_unknown_token" })).unwrap();
    assert_eq!(count(&r), 2);
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
        .query_row("SELECT COUNT(*) FROM tasks WHERE status='active'", [], |r| r.get(0))
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
    e.project_create(&json!({ "name": "p.one", "description": "first" })).unwrap();
    e.project_create(&json!({ "name": "p.two" })).unwrap();

    let listed = e.project_list(&json!({})).unwrap();
    assert_eq!(count(&listed), 2);

    e.project_archive(&json!({ "name": "p.one" })).unwrap();

    // Archived hidden by default.
    let active = e.project_list(&json!({})).unwrap();
    assert_eq!(count(&active), 1);
    assert_eq!(active["projects"][0]["name"], "p.two");

    // include_archived surfaces both.
    let all = e.project_list(&json!({ "include_archived": true })).unwrap();
    assert_eq!(count(&all), 2);

    // Archiving a missing project is not_found.
    let err = e.project_archive(&json!({ "name": "nope" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn annotation_add_and_get() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let res = e.annotation_add(&json!({ "ref": sid, "body": "a note" })).unwrap();
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
    e.dependency_add(&json!({ "ref": a, "depends_on": b })).unwrap();
    assert_eq!(e.task_get(&json!({ "ref": a })).unwrap()["blocked"], true);

    let res = e.dependency_remove(&json!({ "ref": a, "depends_on": b })).unwrap();
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
    let proj_events = e.event_list(&json!({ "entity": "task", "limit": 10 })).unwrap();
    assert_eq!(count(&proj_events), 3);
}

// ---- report.summary aggregation ---------------------------------------------

#[test]
fn report_summary_aggregates_count_est_and_overdue() {
    let e = engine();
    e.project_create(&json!({ "name": "P" })).unwrap(); // D23
    e.project_create(&json!({ "name": "Q" })).unwrap();
    e.task_add(&json!({ "title": "p1a", "project": "P", "estimate": "PT2H", "due": plus_hours(-48) })).unwrap();
    e.task_add(&json!({ "title": "p1b", "project": "P", "estimate": "PT3H", "due": plus_hours(48) })).unwrap();
    e.task_add(&json!({ "title": "q1", "project": "Q" })).unwrap();

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

// ---- D24: report aggregations exclude cancelled by default -------------------

/// Seed one project `P` with one task per interesting status, each carrying a
/// 1-hour estimate so `est_total` reads back as a count of included rows.
fn engine_with_one_task_per_status() -> Engine {
    let e = engine();
    e.project_create(&json!({ "name": "P" })).unwrap(); // D23
    for title in ["pend", "act", "fin", "dead"] {
        e.task_add(&json!({ "title": title, "project": "P", "estimate": "PT1H" })).unwrap();
    }
    // short ids 1..4 in insertion order.
    e.task_start(&json!({ "ref": "2" })).unwrap();
    e.task_done(&json!({ "ref": "3" })).unwrap();
    e.task_cancel(&json!({ "ref": "4" })).unwrap();
    e
}

fn report(e: &Engine, params: serde_json::Value) -> serde_json::Value {
    let rep = e.report_summary(&params).unwrap();
    rep["groups"].as_array().unwrap().first().cloned().unwrap_or(json!({}))
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
    let rep = e.report_summary(&json!({ "group_by": "status", "metrics": ["count"] })).unwrap();
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
    let g = report(&e, json!({ "all": true, "metrics": ["count", "est_total"] }));
    assert_eq!(g["count"], 4);
    assert_eq!(g["est_total"], "PT4H");
}

/// D24 rule 2 beats rule 3: an explicitly-typed filter is used literally. Typing
/// `tasqx report status:cancelled` and getting an empty table back reads as a
/// bug however well the default is documented.
#[test]
fn report_summary_honours_an_explicit_status_filter() {
    let e = engine_with_one_task_per_status();
    let g = report(&e, json!({ "filter": "status:cancelled", "metrics": ["count"] }));
    assert_eq!(g["count"], 1, "the default must step aside, not narrow to nothing");
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
    a.annotation_add(&json!({ "ref": msid, "body": "first note" })).unwrap();
    a.dependency_add(&json!({ "ref": msid, "depends_on": dsid })).unwrap();

    let export_a = a.store_export(&json!({})).unwrap();
    let tasks_a = export_a["tasks"].clone();
    assert_eq!(tasks_a.as_array().unwrap().len(), 2);

    // Import into a fresh store and re-export.
    let b = engine();
    let imp = b.store_import(&json!({ "tasks": tasks_a.clone() })).unwrap();
    assert_eq!(imp["imported"], 2);

    let export_b = b.store_export(&json!({})).unwrap();
    assert_eq!(export_b["tasks"], tasks_a, "export -> import -> export is identity");
}

// ---- D8: boolean or + parentheses grouping ----------------------------------

#[test]
fn filter_or_and_parentheses() {
    let e = engine();
    e.task_add(&json!({ "title": "t api", "tags": ["api"] })).unwrap();
    e.task_add(&json!({ "title": "t infra", "tags": ["infra"] })).unwrap();
    e.task_add(&json!({ "title": "t other", "tags": ["other"] })).unwrap();

    // Explicit or.
    let r = e.task_list(&json!({ "filter": "+api or +infra" })).unwrap();
    assert_eq!(count(&r), 2);

    // Parenthesised or, AND-ed with a status predicate.
    let r = e.task_list(&json!({ "filter": "(+api or +infra) and status:pending" })).unwrap();
    assert_eq!(count(&r), 2);

    // Implicit AND still means AND: no task has both tags.
    let r = e.task_list(&json!({ "filter": "+api +infra" })).unwrap();
    assert_eq!(count(&r), 0);

    // Precedence: and binds tighter than or.
    // "+api or +infra and +other" == api OR (infra AND other) => only the api task.
    let r = e.task_list(&json!({ "filter": "+api or +infra and +other" })).unwrap();
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

    let done = e.task_done(&json!({ "ref": added["short_id"].clone() })).unwrap();
    let spawned = &done["spawned"];
    assert!(spawned.is_object(), "done result carries a spawned instance");

    // Exactly one new instance: template (done) + spawn = 2 tasks total.
    assert_eq!(task_count(&e), 2);

    // The spawned due is advanced by exactly one period (3 days) from the anchor.
    let new_due = spawned["due"].as_str().unwrap();
    assert_eq!(secs(new_due) - secs(due.as_str()), 3 * 86_400);

    // Spawn carries the rule forward and is a fresh short_id.
    let got = e.task_get(&json!({ "ref": spawned["short_id"].clone() })).unwrap();
    assert_eq!(got["recurrence"], "every 3 days");
    assert_eq!(got["status"], "pending");
    assert_ne!(spawned["short_id"], added["short_id"]);
    assert_eq!(spawned["status"], "pending");
}

#[test]
fn non_recurring_done_spawns_nothing() {
    let e = engine();
    let a = e.task_add(&json!({ "title": "one off", "due": plus_hours(24) })).unwrap();
    let done = e.task_done(&json!({ "ref": a["short_id"].clone() })).unwrap();
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
    let done = e.task_done(&json!({ "ref": a["short_id"].clone() })).unwrap();
    let spawned = &done["spawned"];

    // ONE catch-up instance, not a backfill storm.
    assert_eq!(task_count(&e), 2, "collapse to a single instance");

    // The single instance's due is in the future (anchor advanced past now).
    let new_due = spawned["due"].as_str().unwrap();
    assert!(secs(new_due) > Timestamp::now().as_second(), "next slot is in the future");
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
    let done = e.task_done(&json!({ "ref": a["short_id"].clone() })).unwrap();
    let new_due = done["spawned"]["due"].as_str().unwrap();
    // The spawned weekday must be one of the listed days.
    let wd = new_due
        .parse::<Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .weekday();
    use jiff::civil::Weekday::*;
    assert!(matches!(wd, Monday | Wednesday | Friday), "got weekday {wd:?}");
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
    let done = e.task_done(&json!({ "ref": a["short_id"].clone() })).unwrap();
    assert!(done["spawned"].is_object(), "completion succeeds and spawns");
    assert_eq!(task_count(&e), 2, "exactly one catch-up instance");

    let new_due = done["spawned"]["due"].as_str().unwrap();
    let d = new_due.parse::<Timestamp>().unwrap().to_zoned(jiff::tz::TimeZone::UTC).date();
    // It is a Friday, in the future, and genuinely the 5th Friday of its month.
    assert_eq!(d.weekday(), jiff::civil::Weekday::Friday);
    assert!(secs(new_due) > Timestamp::now().as_second(), "next slot is in the future");
    let fifth = d.nth_weekday_of_month(5, jiff::civil::Weekday::Friday).unwrap();
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
    let done = e.task_done(&json!({ "ref": a["short_id"].clone() })).unwrap();
    assert_eq!(task_count(&e), 2);

    let new_due = done["spawned"]["due"].as_str().unwrap();
    let d = new_due.parse::<Timestamp>().unwrap().to_zoned(jiff::tz::TimeZone::UTC).date();
    assert!(secs(new_due) > Timestamp::now().as_second(), "next slot is in the future");
    // Day is 31, or the clamped last day of a shorter month.
    let expected = 31.min(d.last_of_month().day());
    assert_eq!(d.day(), expected, "day-31 rule clamps to month end");
}

#[test]
fn modify_can_set_and_clear_recurrence() {
    let e = engine();
    let a = e.task_add(&json!({ "title": "rent", "due": plus_hours(48) })).unwrap();
    let r#ref = a["short_id"].clone();

    // Set a rule via modify.
    e.task_modify(&json!({ "ref": r#ref, "set": { "recurrence": "monthly on day 1" } })).unwrap();
    let got = e.task_get(&json!({ "ref": r#ref })).unwrap();
    assert_eq!(got["recurrence"], "monthly on day 1");

    // Clear it: completing then no longer spawns.
    e.task_modify(&json!({ "ref": r#ref, "set": { "recurrence": null } })).unwrap();
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
    let got = e.task_get(&json!({ "ref": a["short_id"].clone() })).unwrap();
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
    let b = e.task_add(&json!({ "title": "call", "remind": "2099-01-01T09:00:00Z" })).unwrap();
    assert_eq!(remind_of(&e, b["short_id"].as_i64().unwrap()), json!("2099-01-01T09:00:00Z"));

    // Quiet by default (§9): no remind key => no reminder, ever.
    let c = e.task_add(&json!({ "title": "no reminder", "due": "2099-01-01T00:00:00Z" })).unwrap();
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
    let t = e.task_add(&json!({ "title": "ship", "due": "2099-01-01T00:00:00Z" })).unwrap();
    let sid = t["short_id"].as_i64().unwrap();
    assert_eq!(remind_of(&e, sid), json!(null));

    e.task_modify(&json!({ "ref": sid, "set": { "remind": "-2h" } })).unwrap();
    assert_eq!(remind_of(&e, sid), json!("-2h"));

    // null is the sanctioned "stop reminding me" path.
    e.task_modify(&json!({ "ref": sid, "set": { "remind": null } })).unwrap();
    assert_eq!(remind_of(&e, sid), json!(null));

    // A bad spec is rejected and leaves the field untouched.
    e.task_modify(&json!({ "ref": sid, "set": { "remind": "-1h" } })).unwrap();
    let err = e.task_modify(&json!({ "ref": sid, "set": { "remind": "nope" } })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert_eq!(remind_of(&e, sid), json!("-1h"), "a rejected modify changes nothing");
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
    assert_eq!(export_a["tasks"][0]["remind"], json!("-1h"), "export carries the spec");
    assert_eq!(export_a["tasks"][1]["remind"], json!("2099-01-01T09:00:00Z"));

    let b = engine();
    b.store_import(&json!({ "tasks": export_a["tasks"].clone() })).unwrap();
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
    assert_eq!(second["fired"], json!(false), "the same instant never fires twice");

    // Exactly one `reminded` event — the dedupe record AND the push surface.
    let evts = e.event_list(&json!({ "ref": sid, "limit": 100 })).unwrap();
    let reminded: Vec<_> =
        evts["events"].as_array().unwrap().iter().filter(|v| v["op"] == json!("reminded")).collect();
    assert_eq!(reminded.len(), 1);

    // A DIFFERENT instant on the same task is a different reminder and fires.
    let other = e.reminder_fire(&json!({ "ref": sid, "at": "2098-12-30T23:00:00Z" })).unwrap();
    assert_eq!(other["fired"], json!(true));
}

#[test]
fn reminder_fire_normalizes_at_so_equivalent_instants_dedupe() {
    let e = engine();
    let t = e.task_add(&json!({ "title": "ship", "remind": "2099-01-01T09:00:00Z" })).unwrap();
    let sid = t["short_id"].as_i64().unwrap();

    let first = e.reminder_fire(&json!({ "ref": sid, "at": "2099-01-01T09:00:00Z" })).unwrap();
    assert_eq!(first["fired"], json!(true));
    // Same instant, different spelling: must NOT read as a new reminder.
    let same = e.reminder_fire(&json!({ "ref": sid, "at": "2099-01-01T11:00:00+02:00" })).unwrap();
    assert_eq!(same["fired"], json!(false), "offset spelling must not defeat dedupe");
}

#[test]
fn reminder_fire_does_not_bump_rev_or_modified() {
    let e = engine();
    let t = e
        .task_add(&json!({ "title": "ship", "due": "2099-01-01T00:00:00Z", "remind": "-1h" }))
        .unwrap();
    let sid = t["short_id"].as_i64().unwrap();
    let before = e.task_get(&json!({ "ref": sid })).unwrap();

    e.reminder_fire(&json!({ "ref": sid, "at": "2098-12-31T23:00:00Z" })).unwrap();
    let after = e.task_get(&json!({ "ref": sid })).unwrap();

    // A reminder is a fact about time passing, not an edit: bumping `_rev` would
    // spuriously break a client holding `expected_rev`.
    assert_eq!(before["_rev"], after["_rev"], "reminder.fire must not bump _rev");
    assert_eq!(before["modified"], after["modified"], "reminder.fire must not touch modified");
}

#[test]
fn reminder_fire_rejects_a_non_rfc3339_instant() {
    let e = engine();
    let t = e.task_add(&json!({ "title": "ship", "remind": "2099-01-01T09:00:00Z" })).unwrap();
    let err = e.reminder_fire(&json!({ "ref": t["short_id"], "at": "friday" })).unwrap_err();
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
    assert_eq!(remind_of(&e, spawned), json!("-30m"), "the offset rides along unchanged");
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
    assert!(got_ts > Timestamp::now(), "a spawned absolute reminder must be in the future");
    let new_due = done["spawned"]["due"].as_str().unwrap().parse::<Timestamp>().unwrap();
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
    assert_eq!(ex["dropped_dependencies"], json!(1), "the trim must be visible");

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
    assert_eq!(full["tasks"][1]["depends_on"], json!([blocker["id"].clone()]));
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
    b.store_import(&json!({ "tasks": api["tasks"].clone() })).unwrap();
    let got = b.task_get(&json!({ "ref": dependent["short_id"].clone() })).unwrap();
    assert_eq!(got["blocked"], json!(false));
    assert_eq!(got["depends_on"], json!([]));

    // The blocker arrives later. The dependent must stay unblocked: the edge
    // was dropped at export, so it is really gone, not merely invisible.
    b.store_import(&json!({ "tasks": infra["tasks"].clone() })).unwrap();
    let got = b.task_get(&json!({ "ref": dependent["short_id"].clone() })).unwrap();
    assert_eq!(got["blocked"], json!(false), "a dropped edge must not resurrect");
    assert_eq!(got["depends_on"], json!([]));
    assert_eq!(b.store_export(&json!({})).unwrap()["dropped_dependencies"], json!(0));
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
    let got = b.task_get(&json!({ "ref": dependent["short_id"].clone() })).unwrap();
    assert_eq!(got["blocked"], json!(true), "forward reference must still wire up");

    // Target already in the store, payload carries only the dependent.
    let c = engine();
    c.store_import(&json!({ "tasks": full["tasks"].clone() })).unwrap();
    let only_dep = json!([full["tasks"][1].clone()]);
    c.store_import(&json!({ "tasks": only_dep })).unwrap();
    let got = c.task_get(&json!({ "ref": dependent["short_id"].clone() })).unwrap();
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
    for field in ["project", "priority", "due", "scheduled", "wait", "estimate", "recurrence", "remind"] {
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

    let first = e.task_modify(&json!({ "ref": r, "set": { "priority": "H" } })).unwrap();
    let rev = first["_rev"].as_i64().unwrap();

    // Someone else moves the task on.
    e.task_modify(&json!({ "ref": r, "set": { "priority": "L" } })).unwrap();

    // Our stale rev must lose, and must not clobber.
    let err = e
        .task_modify(&json!({ "ref": r, "set": { "priority": "M" }, "expected_rev": rev }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let got = e.task_get(&json!({ "ref": r })).unwrap();
    assert_eq!(got["priority"], "L", "the losing write must not have applied");

    // At the current rev it goes through.
    let cur = got["_rev"].as_i64().or_else(|| got["rev"].as_i64()).unwrap();
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
        .task_add(&json!({ "title": "Water plants", "recurrence": "every 3 days", "due": plus_hours(1) }))
        .unwrap();
    let r = added["short_id"].clone();
    assert_eq!(e.task_get(&json!({ "ref": r })).unwrap()["recurrence"], "every 3 days");

    e.task_modify(&json!({ "ref": r, "set": { "recurrence": null } })).unwrap();
    assert_eq!(e.task_get(&json!({ "ref": r })).unwrap()["recurrence"], json!(null));

    // Completing it now spawns nothing.
    let before = count(&e.task_list(&json!({ "filter": "" })).unwrap());
    e.task_done(&json!({ "ref": r })).unwrap();
    let after = count(&e.task_list(&json!({ "filter": "" })).unwrap());
    assert_eq!(after, before, "a cleared recurrence must not spawn a successor");
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
    assert_eq!(e.store_export(&json!({})).unwrap()["tasks"].as_array().unwrap().len(), 0);
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
    assert_eq!(e.store_export(&json!({})).unwrap()["tasks"].as_array().unwrap().len(), 0);
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

/// A store can still hold an unreadable estimate (import writes columns raw,
/// and stores predate the parser guard). `report` must survive it: the reader
/// returns None and the roll-up skips it, rather than aborting the process.
#[test]
fn report_over_an_unreadable_estimate_does_not_panic() {
    let e = engine();
    e.store_import(&json!({ "tasks": [
        { "id": "0193aaaa-0000-7000-8000-00000000000a", "short_id": 1, "title": "huge",
          "status": "pending", "project": "p", "estimate": "P7000000000000000000D" },
        { "id": "0193aaaa-0000-7000-8000-00000000000b", "short_id": 2, "title": "real",
          "status": "pending", "project": "p", "estimate": "PT4H" },
    ] }))
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
    e.task_add(&json!({ "title": "x", "project": "realwork" })).unwrap();

    let err = e
        .task_modify(&json!({ "ref": 1, "set": { "project": "" } }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("clear"), "must point at --clear, got: {}", err.message);

    // Whitespace is the same mistake wearing a hat.
    assert!(e.task_modify(&json!({ "ref": 1, "set": { "project": "   " } })).is_err());

    // The rejected write changed nothing.
    assert_eq!(e.task_get(&json!({ "ref": 1 })).unwrap()["project"], json!("realwork"));

    // And the sanctioned path still empties the field to a real NULL.
    e.task_modify(&json!({ "ref": 1, "set": { "project": null } })).unwrap();
    assert_eq!(e.task_get(&json!({ "ref": 1 })).unwrap()["project"], json!(null));
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
    assert_eq!(first["default"], true, "the first project must claim the default");
    assert_eq!(e.default_project().as_deref(), Some("work"));

    // Second project: does NOT steal it, and says so in its own result.
    assert_eq!(first["current_default"], "work");
    let second = e.project_create(&json!({ "name": "prive.klussen" })).unwrap();
    assert_eq!(second["default"], false, "a later project must not steal the default");
    assert_eq!(
        second["current_default"], "work",
        "must report what the default still is, not just that it did not move"
    );
    assert_eq!(e.default_project().as_deref(), Some("work"), "default was stolen");

    // The behavior that actually matters: a bare add still lands in `work`.
    let added = e.task_add(&json!({ "title": "a task" })).unwrap();
    assert_eq!(added["project"], "work", "bare add landed in the wrong project");
}

/// `project.use` is the one explicit way to move the default.
#[test]
fn project_use_switches_the_default_and_reports_the_previous_one() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "prive.klussen" })).unwrap();

    let r = e.project_use(&json!({ "name": "prive.klussen" })).unwrap();
    assert_eq!(r["name"], "prive.klussen");
    assert_eq!(r["default"], true);
    assert_eq!(r["previous"], "work", "the switch must name what it replaced");
    assert_eq!(e.default_project().as_deref(), Some("prive.klussen"));

    let added = e.task_add(&json!({ "title": "klus" })).unwrap();
    assert_eq!(added["project"], "prive.klussen");

    // And back again - `previous` tracks the real prior value, not a guess.
    let back = e.project_use(&json!({ "name": "work" })).unwrap();
    assert_eq!(back["previous"], "prive.klussen");
    assert_eq!(e.task_add(&json!({ "title": "werk" })).unwrap()["project"], "work");
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
    assert_eq!(uses.len(), 1, "expected exactly one `use` event, got {uses:?}");
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
    assert_eq!(err.code, ErrorCode::NotFound, "same code project.archive gives");
    assert!(err.message.contains("nope"), "the error must name it: {}", err.message);
    // The rejected write changed nothing.
    assert_eq!(e.default_project().as_deref(), Some("work"));

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
    assert_eq!(e.default_project().as_deref(), Some("work"));
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
    assert!(err.message.contains("archived"), "must explain why: {}", err.message);
    assert!(err.message.contains("old"));
    assert_eq!(e.default_project().as_deref(), Some("work"));
}

/// D22, the other half: archiving the *current* default un-points it, and says
/// so out loud rather than leaving a default aimed at a retired project.
#[test]
fn archiving_the_default_project_clears_the_default_and_reports_it() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "side" })).unwrap();
    assert_eq!(e.default_project().as_deref(), Some("work"));

    // Archiving a NON-default project leaves the default alone and says nothing.
    let quiet = e.project_archive(&json!({ "name": "side" })).unwrap();
    assert_eq!(quiet["default_cleared"], false);
    assert_eq!(e.default_project().as_deref(), Some("work"));

    // Archiving the default clears it, visibly.
    let loud = e.project_archive(&json!({ "name": "work" })).unwrap();
    assert_eq!(loud["default_cleared"], true, "silently keeping a retired default is the bug");
    assert_eq!(e.default_project(), None);

    // A bare add is now projectless - the same state a fresh store is in.
    let added = e.task_add(&json!({ "title": "homeless" })).unwrap();
    assert_eq!(added["project"], json!(null));

    // No default => the next create claims it, exactly like a fresh store.
    e.project_create(&json!({ "name": "work2" })).unwrap();
    assert_eq!(e.default_project().as_deref(), Some("work2"));
}

/// The invisible-field trap: the default drives where a bare add lands, so every
/// read surface that lists projects must show which one it is.
#[test]
fn project_list_marks_the_default_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "prive.klussen" })).unwrap();

    let listed = e.project_list(&json!({})).unwrap();
    let rows = listed["projects"].as_array().unwrap();
    let default_rows: Vec<_> = rows.iter().filter(|p| p["default"] == json!(true)).collect();
    assert_eq!(default_rows.len(), 1, "exactly one row must be marked default: {rows:?}");
    assert_eq!(default_rows[0]["name"], "work");
    // Every row must carry the field, not just the winner - an absent field and
    // a false one are different things to a machine consumer.
    for p in rows {
        assert!(
            p.get("default").map(serde_json::Value::is_boolean).unwrap_or(false),
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
    assert!(orphan.get("project").is_some(), "the field must always be present");
    assert_eq!(orphan["project"], json!(null));

    e.project_create(&json!({ "name": "work" })).unwrap();
    assert_eq!(e.task_add(&json!({ "title": "inherited" })).unwrap()["project"], "work");
    // Explicit still wins, and is still reported (and must exist - D23).
    e.project_create(&json!({ "name": "other" })).unwrap();
    let explicit = e.task_add(&json!({ "title": "explicit", "project": "other" })).unwrap();
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
    assert_eq!(e.default_project().as_deref(), Some("other"));

    let caps = e.capabilities();
    let methods: Vec<&str> =
        caps["methods"].as_array().unwrap().iter().map(|m| m.as_str().unwrap()).collect();
    assert!(methods.contains(&"project.use"), "project.use must be advertised: {methods:?}");
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
        e.default_project(),
        None,
        "a default aimed at an archived project must not survive the open"
    );
    assert_eq!(
        e.capabilities()["default_project"],
        json!(null),
        "capabilities must agree with the project list, not report a ghost"
    );
    let rows = e.project_list(&json!({ "include_archived": true })).unwrap();
    let marked: Vec<_> = rows["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["default"] == json!(true))
        .collect();
    assert!(marked.is_empty(), "no row may claim a default the store does not have: {marked:?}");

    // The behavior that actually bit: a bare add filed into the archived project.
    let added = e.task_add(&json!({ "title": "legacy orphan" })).unwrap();
    assert_eq!(added["project"], json!(null), "bare add landed in an archived project");

    // And the store is not stranded: the next create claims the default again.
    e.project_create(&json!({ "name": "fresh" })).unwrap();
    assert_eq!(e.default_project().as_deref(), Some("fresh"));
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
    assert_eq!(e.default_project(), None);
    assert_eq!(e.task_add(&json!({ "title": "x" })).unwrap()["project"], json!(null));
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
    assert_eq!(e.default_project().as_deref(), Some("prive"));
    assert_eq!(e.task_add(&json!({ "title": "x" })).unwrap()["project"], "prive");
    let _ = std::fs::remove_file(&path);
}

/// D23: a whitespace-only project name is rejected where names are born. It used
/// to be accepted, claim the default, print as a blank row, and then be
/// unreachable by the one verb that selects projects.
#[test]
fn project_create_rejects_a_whitespace_only_name() {
    let e = engine();
    let err = e.project_create(&json!({ "name": "   " })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "D18's rule at the create edge");
    // Nothing was written: no row, and no default claimed.
    assert_eq!(e.default_project(), None, "a rejected create must not claim the default");
    assert_eq!(count(&e.project_list(&json!({ "include_archived": true })).unwrap()), 0);
    // "" is rejected by req_str, as it always was.
    assert_eq!(e.project_create(&json!({ "name": "" })).unwrap_err().code, ErrorCode::BadRequest);
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

    let events = e.event_list(&json!({ "limit": 50, "entity": "project" })).unwrap();
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
    assert_eq!(claimed_by("work"), json!(true), "the first create claimed the default");
    assert_eq!(claimed_by("side"), json!(false), "a create that did not claim it must say so");
    assert_eq!(claimed_by("third"), json!(true), "re-claiming after a clear is a claim too");
}

/// D23: an explicit `project` is validated exactly like `project.use`'s target.
/// A typo used to file the task into a bucket no project surface lists.
#[test]
fn task_add_rejects_an_unknown_explicit_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();

    let err =
        e.task_add(&json!({ "title": "ghost", "project": "totally-not-a-project" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "same code project.use gives");
    assert!(err.message.contains("totally-not-a-project"), "must name it: {}", err.message);
    // The rejected add wrote nothing at all.
    assert_eq!(count(&e.task_list(&json!({})).unwrap()), 0, "a rejected add must not write");
}

/// D23 / D22's other half: an archived project cannot take new tasks, exactly as
/// it cannot be the default. `use <archived>` was a conflict while
/// `add --project <archived>` sailed through with exit 0.
#[test]
fn task_add_rejects_an_archived_explicit_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "prive.klussen" })).unwrap();
    e.project_archive(&json!({ "name": "prive.klussen" })).unwrap();

    let err =
        e.task_add(&json!({ "title": "into archived", "project": "prive.klussen" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict, "same code project.use gives for an archived one");
    assert!(err.message.contains("archived"), "must explain why: {}", err.message);
    assert!(err.message.contains("prive.klussen"));
    assert_eq!(count(&e.task_list(&json!({})).unwrap()), 0);

    // A live project still works, and the default path is untouched.
    assert_eq!(e.task_add(&json!({ "title": "ok", "project": "work" })).unwrap()["project"], "work");
}

/// The sibling path: `task.modify` moves a task between projects, so it owes the
/// same guard. Half-applying it is how the dependency-reader bug worked.
#[test]
fn task_modify_rejects_an_unknown_or_archived_project() {
    let e = engine();
    e.project_create(&json!({ "name": "work" })).unwrap();
    e.project_create(&json!({ "name": "old" })).unwrap();
    e.project_archive(&json!({ "name": "old" })).unwrap();
    let t = e.task_add(&json!({ "title": "x", "project": "work" })).unwrap();
    let r = t["short_id"].clone();

    let ghost = e.task_modify(&json!({ "ref": r, "set": { "project": "nope" } })).unwrap_err();
    assert_eq!(ghost.code, ErrorCode::NotFound);
    let archived = e.task_modify(&json!({ "ref": r, "set": { "project": "old" } })).unwrap_err();
    assert_eq!(archived.code, ErrorCode::Conflict);

    // Neither rejected modify moved the task or bumped its rev.
    let got = e.task_get(&json!({ "ref": r })).unwrap();
    assert_eq!(got["project"], "work");
    assert_eq!(got["_rev"], 1);

    // Clearing the project is still allowed - null is not a project name.
    e.task_modify(&json!({ "ref": r, "set": { "project": null } })).unwrap();
    assert_eq!(e.task_get(&json!({ "ref": r })).unwrap()["project"], json!(null));
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
            got["status"], want.as_str(),
            "a {} task did not read back as {} — the write literal and \
             Status::as_str have diverged, and the read fallback hid it",
            want.as_str(), want.as_str()
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
    assert_eq!(e.task_get(&json!({ "ref": started })).unwrap()["status"], Status::Pending.as_str());

    let finished = e.task_add(&json!({ "title": "b" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": finished })).unwrap();
    e.task_reopen(&json!({ "ref": finished })).unwrap();
    assert_eq!(e.task_get(&json!({ "ref": finished })).unwrap()["status"], Status::Pending.as_str());
}
