//! Tests for D239: `git.event`, the inbound door for a branch's PR events.
//!
//! tasqx never calls GitHub. A caller that already knows what happened (the
//! land skill reading `gh pr view --json`, a CI step, a git hook) says so, and
//! the store records the branch, the PR and the merge commit on the task and
//! moves its status the way the event means — never past a check that did not
//! pass, and never twice for the same event.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

const PR: &str = "https://github.com/o/r/pull/7";

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn add(e: &Engine, title: &str) -> i64 {
    call(e, "task.add", json!({ "title": title })).expect("task.add")["short_id"]
        .as_i64()
        .expect("short_id")
}

fn get(e: &Engine, r: i64) -> Value {
    call(e, "task.get", json!({ "ref": r })).expect("task.get")
}

fn event(e: &Engine, kind: &str, branch: &str, extra: Value) -> Value {
    let mut p = json!({ "event": kind, "branch": branch });
    for (k, v) in extra.as_object().expect("object") {
        p[k] = v.clone();
    }
    call(e, "git.event", p).expect("git.event")
}

fn events(e: &Engine, r: i64) -> Vec<Value> {
    call(e, "event.list", json!({ "ref": r })).expect("event.list")["events"]
        .as_array()
        .expect("events")
        .clone()
}

fn refs(e: &Engine, r: i64) -> Vec<(String, String, String)> {
    get(e, r)["git_refs"]
        .as_array()
        .expect("git_refs is an array on task.get")
        .iter()
        .map(|g| {
            (
                g["kind"].as_str().unwrap().to_string(),
                g["ref"].as_str().unwrap().to_string(),
                g["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn notes(e: &Engine, r: i64) -> Vec<String> {
    get(e, r)["annotations"]
        .as_array()
        .expect("annotations")
        .iter()
        .map(|a| a["body"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_task_branch_resolves_to_its_task_whatever_the_suffix() {
    let e = engine();
    let a = add(&e, "first");
    let b = add(&e, "second");
    let c = add(&e, "third");
    event(&e, "opened", &format!("task/{a}-link-prs"), json!({}));
    event(
        &e,
        "opened",
        &format!("task/{b}-link-prs-rebased"),
        json!({}),
    );
    event(&e, "opened", &format!("refs/heads/task/{c}-v3"), json!({}));
    for r in [a, b, c] {
        assert_eq!(get(&e, r)["status"], "active", "#{r}");
    }
}

#[test]
fn task_overrides_the_branch_and_neither_is_refused() {
    let e = engine();
    let a = add(&e, "named");
    let out = event(&e, "opened", "feature/whatever", json!({ "task": a }));
    assert_eq!(out["short_id"], a);
    assert_eq!(
        refs(&e, a),
        vec![("branch".into(), "feature/whatever".into(), "open".into())]
    );

    let err = call(
        &e,
        "git.event",
        json!({ "event": "opened", "branch": "main" }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
    assert!(err.message.contains("task/<id>-<slug>"), "{}", err.message);

    let err = call(
        &e,
        "git.event",
        json!({ "event": "reviewed", "branch": format!("task/{a}-x") }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
}

#[test]
fn opened_starts_a_pending_task_records_branch_and_pr_and_is_attributed_to_git() {
    let e = engine();
    let a = add(&e, "link prs");
    let out = event(&e, "opened", &format!("task/{a}-link"), json!({ "pr": PR }));
    assert_eq!(out["changed"], true);
    assert_eq!(get(&e, a)["status"], "active");
    let mut got = refs(&e, a);
    got.sort();
    assert_eq!(
        got,
        vec![
            ("branch".into(), format!("task/{a}-link"), "open".into()),
            ("pr".into(), PR.into(), "open".into()),
        ]
    );
    let log = events(&e, a);
    let start = log
        .iter()
        .find(|ev| ev["op"] == "start")
        .expect("a start event");
    assert_eq!(start["actor"], "git", "{start}");
    let rec = log
        .iter()
        .find(|ev| ev["op"] == "git.ref")
        .expect("a git.ref event");
    assert_eq!(rec["actor"], "git", "{rec}");
}

#[test]
fn merged_completes_with_the_pr_in_the_delivery_note() {
    let e = engine();
    let a = add(&e, "ship");
    let out = event(
        &e,
        "merged",
        &format!("task/{a}-ship"),
        json!({ "pr": PR, "sha": "abc1234" }),
    );
    assert_eq!(out["changed"], true);
    let t = get(&e, a);
    assert_eq!(t["status"], "done", "{t}");
    assert_eq!(
        t["delivered_annotation"]["body"],
        format!("Merged {PR} (abc1234)"),
        "{t}"
    );
    assert!(refs(&e, a).contains(&("commit".into(), "abc1234".into(), "merged".into())));
    assert!(refs(&e, a).contains(&("pr".into(), PR.into(), "merged".into())));
    let done = events(&e, a)
        .into_iter()
        .find(|ev| ev["op"] == "done")
        .expect("a done event");
    assert_eq!(done["actor"], "git");
}

#[test]
fn merged_completes_when_every_check_passed() {
    let e = engine();
    let a = add(&e, "proven");
    let id = call(&e, "check.add", json!({ "ref": a, "body": "tests pass" })).unwrap()["check"]
        ["id"]
        .clone();
    call(
        &e,
        "check.set",
        json!({ "ref": a, "check_id": id, "state": "passed" }),
    )
    .unwrap();
    event(
        &e,
        "merged",
        &format!("task/{a}-p"),
        json!({ "pr": PR, "sha": "f00" }),
    );
    assert_eq!(get(&e, a)["status"], "done");
}

#[test]
fn merged_never_completes_past_an_open_or_failed_check() {
    let e = engine();
    let a = add(&e, "unproven");
    call(&e, "check.add", json!({ "ref": a, "body": "docs updated" })).unwrap();
    let id = call(&e, "check.add", json!({ "ref": a, "body": "gates green" })).unwrap()["check"]
        ["id"]
        .clone();
    call(
        &e,
        "check.set",
        json!({ "ref": a, "check_id": id, "state": "failed" }),
    )
    .unwrap();
    let out = event(
        &e,
        "merged",
        &format!("task/{a}-u"),
        json!({ "pr": PR, "sha": "beef" }),
    );
    assert_eq!(out["changed"], true);
    let t = get(&e, a);
    assert_eq!(t["status"], "pending", "{t}");
    let last = notes(&e, a).pop().expect("an annotation");
    assert!(last.contains(PR), "{last}");
    assert!(
        last.contains("docs updated") && last.contains("open"),
        "{last}"
    );
    assert!(
        last.contains("gates green") && last.contains("failed"),
        "{last}"
    );
    assert!(refs(&e, a).contains(&("pr".into(), PR.into(), "merged".into())));
}

#[test]
fn closed_unmerged_annotates_and_leaves_status_alone() {
    let e = engine();
    let a = add(&e, "abandoned");
    let out = event(&e, "closed", &format!("task/{a}-x"), json!({ "pr": PR }));
    assert_eq!(out["changed"], true);
    assert_eq!(get(&e, a)["status"], "pending");
    let last = notes(&e, a).pop().expect("an annotation");
    assert!(last.contains(PR) && last.contains("closed"), "{last}");
    assert!(refs(&e, a).contains(&("pr".into(), PR.into(), "closed".into())));
}

#[test]
fn replaying_an_event_changes_nothing_the_second_time() {
    for kind in ["opened", "merged", "closed"] {
        let e = engine();
        let a = add(&e, "replayed");
        call(&e, "check.add", json!({ "ref": a, "body": "still open" })).unwrap();
        let branch = format!("task/{a}-r");
        let extra = json!({ "pr": PR, "sha": "c0ffee" });
        assert_eq!(event(&e, kind, &branch, extra.clone())["changed"], true);
        let before = (get(&e, a), events(&e, a).len());
        let again = event(&e, kind, &branch, extra);
        assert_eq!(again["changed"], false, "{kind}: {again}");
        assert_eq!((get(&e, a), events(&e, a).len()), before, "{kind}");
    }
}

#[test]
fn refs_travel_through_export_and_a_merge_import() {
    let e = engine();
    let a = add(&e, "carried");
    event(&e, "opened", &format!("task/{a}-c"), json!({ "pr": PR }));
    let doc = call(&e, "store.export", json!({})).unwrap();
    assert_eq!(
        doc["tasks"][0]["git_refs"].as_array().unwrap().len(),
        2,
        "{doc}"
    );

    // A plain restore into an empty store.
    let f = engine();
    call(&f, "store.import", doc.clone()).expect("import");
    assert_eq!(refs(&f, a).len(), 2);

    // The copy moves on (merged there), and merging it back takes the further state.
    event(
        &f,
        "merged",
        &format!("task/{a}-c"),
        json!({ "pr": PR, "sha": "9a9" }),
    );
    let mut later = call(&f, "store.export", json!({})).unwrap();
    later["merge"] = json!(true);
    call(&e, "store.import", later.clone()).expect("merge import");
    let got = refs(&e, a);
    assert!(
        got.contains(&("pr".into(), PR.into(), "merged".into())),
        "{got:?}"
    );
    assert!(
        got.contains(&("commit".into(), "9a9".into(), "merged".into())),
        "{got:?}"
    );
    // And merging it a second time is a no-op.
    let rows = got.len();
    call(&e, "store.import", later).expect("merge again");
    assert_eq!(refs(&e, a).len(), rows);
}

#[test]
fn brief_carries_the_refs() {
    let e = engine();
    let a = add(&e, "briefed");
    event(&e, "opened", &format!("task/{a}-b"), json!({ "pr": PR }));
    let b = call(&e, "task.brief", json!({ "ref": a })).unwrap();
    assert_eq!(b["task"]["git_refs"].as_array().unwrap().len(), 2, "{b}");
}

// ---- review round 1: idempotency is about the outcome ---------------------

#[test]
fn a_backward_event_changes_nothing_and_says_why() {
    let e = engine();
    let a = add(&e, "merged first");
    let branch = format!("task/{a}-b");
    event(&e, "merged", &branch, json!({ "pr": PR, "sha": "1a2b" }));
    let before = (get(&e, a), events(&e, a).len());
    for late in ["closed", "opened"] {
        let out = event(&e, late, &branch, json!({ "pr": PR }));
        assert_eq!(out["changed"], false, "{late}: {out}");
        assert!(
            out["reason"].as_str().is_some_and(|r| r.contains("merged")),
            "{out}"
        );
    }
    assert_eq!((get(&e, a), events(&e, a).len()), before);
    assert!(refs(&e, a).contains(&("pr".into(), PR.into(), "merged".into())));
}

#[test]
fn a_merged_replay_completes_once_the_checks_are_fixed() {
    let e = engine();
    let a = add(&e, "fixed later");
    let id =
        call(&e, "check.add", json!({ "ref": a, "body": "docs" })).unwrap()["check"]["id"].clone();
    let branch = format!("task/{a}-f");
    let extra = json!({ "pr": PR, "sha": "77aa" });
    event(&e, "merged", &branch, extra.clone());
    assert_eq!(get(&e, a)["status"], "pending");
    // The same state again restates nothing.
    let notes_before = notes(&e, a).len();
    assert_eq!(
        event(&e, "merged", &branch, extra.clone())["changed"],
        false
    );
    assert_eq!(notes(&e, a).len(), notes_before);

    call(
        &e,
        "check.set",
        json!({ "ref": a, "check_id": id, "state": "passed" }),
    )
    .unwrap();
    let out = event(&e, "merged", &branch, extra);
    assert_eq!(out["changed"], true, "{out}");
    assert_eq!(out["transition"], "completed");
    let t = get(&e, a);
    assert_eq!(t["status"], "done");
    assert_eq!(
        t["delivered_annotation"]["body"],
        format!("Merged {PR} (77aa)")
    );
}

#[test]
fn a_blocked_merge_restates_only_when_what_blocks_it_changes() {
    let e = engine();
    let blocker = add(&e, "blocker");
    let a = add(&e, "waits");
    call(
        &e,
        "dependency.add",
        json!({ "ref": a, "depends_on": blocker }),
    )
    .unwrap();
    call(&e, "check.add", json!({ "ref": a, "body": "c" })).unwrap();
    let branch = format!("task/{a}-w");
    event(&e, "merged", &branch, json!({ "pr": PR }));
    let first = notes(&e, a).len();
    assert_eq!(
        event(&e, "merged", &branch, json!({ "pr": PR }))["changed"],
        false
    );
    // The blocker closes: what blocks it now differs, so it is said again.
    call(&e, "task.done", json!({ "ref": blocker })).unwrap();
    let out = event(&e, "merged", &branch, json!({ "pr": PR }));
    assert_eq!(out["changed"], true, "{out}");
    assert_eq!(notes(&e, a).len(), first + 1);
    assert!(!notes(&e, a).last().unwrap().contains("blocked by"));
}

#[test]
fn opened_starts_a_task_at_most_once_per_pr() {
    let e = engine();
    let a = add(&e, "stopped since");
    let branch = format!("task/{a}-o");
    event(&e, "opened", &branch, json!({ "pr": PR }));
    call(&e, "task.stop", json!({ "ref": a })).unwrap();
    // A redelivery after the user stopped it: the user's stop stands.
    let out = event(&e, "opened", &branch, json!({ "pr": PR }));
    assert_eq!(out["changed"], false, "{out}");
    assert!(
        out["reason"].as_str().is_some_and(|r| r.contains("once")),
        "{out}"
    );
    assert_eq!(get(&e, a)["status"], "pending");
}

#[test]
fn opened_on_a_task_that_cannot_start_leaves_a_short_note() {
    let e = engine();
    let a = add(&e, "already done");
    call(&e, "task.done", json!({ "ref": a })).unwrap();
    let out = event(&e, "opened", &format!("task/{a}-d"), json!({ "pr": PR }));
    assert_eq!(out["changed"], true);
    assert_eq!(get(&e, a)["status"], "done");
    assert!(
        notes(&e, a).last().unwrap().contains(PR),
        "{:?}",
        notes(&e, a)
    );
}

#[test]
fn a_backlog_task_is_completed_by_its_merge() {
    let e = engine();
    let a = call(
        &e,
        "task.add",
        json!({ "title": "later", "wait": "2099-01-01T00:00:00Z" }),
    )
    .unwrap()["short_id"]
        .as_i64()
        .unwrap();
    assert_eq!(get(&e, a)["status"], "backlog");
    event(
        &e,
        "merged",
        &format!("task/{a}-l"),
        json!({ "pr": PR, "sha": "b1" }),
    );
    let t = get(&e, a);
    assert_eq!(t["status"], "done", "{t}");
    assert_eq!(
        t["delivered_annotation"]["body"],
        format!("Merged {PR} (b1)")
    );
}

#[test]
fn a_null_task_falls_back_to_the_branch() {
    let e = engine();
    let a = add(&e, "null task");
    let out = event(
        &e,
        "opened",
        &format!("task/{a}-n"),
        json!({ "task": null }),
    );
    assert_eq!(out["short_id"], a);
}

#[test]
fn the_delivery_note_is_pinned_by_id_in_the_completing_transaction() {
    let e = engine();
    let a = add(&e, "pinned");
    event(
        &e,
        "merged",
        &format!("task/{a}-p"),
        json!({ "pr": PR, "sha": "c3" }),
    );
    let t = get(&e, a);
    assert_eq!(
        t["delivered_annotation_id"],
        t["delivered_annotation"]["id"]
    );
    // The note and the completion are one commit: the done event is the
    // newest, the note's add right before it.
    let log: Vec<Value> = events(&e, a)
        .into_iter()
        .filter(|ev| ev["op"] != "git.ref")
        .collect();
    assert_eq!(log[0]["op"], "done");
    assert_eq!(log[1]["op"], "annotation.add");
    assert_eq!(log[1]["payload"]["id"], t["delivered_annotation_id"]);
}

#[test]
fn a_pr_ref_is_its_url_and_carries_no_second_copy() {
    let e = engine();
    let a = add(&e, "one copy");
    event(&e, "opened", &format!("task/{a}-u"), json!({ "pr": PR }));
    for g in get(&e, a)["git_refs"].as_array().unwrap() {
        assert!(g.get("url").is_none(), "{g}");
    }
}

// ---- review round 2 ---------------------------------------------------------

#[test]
fn the_clock_opened_starts_is_anonymous_and_says_git_started_it() {
    let e = engine();
    let a = add(&e, "opened by git");
    let b = add(&e, "an agent's own");
    event(&e, "opened", &format!("task/{a}-x"), json!({ "pr": PR }));
    let start = events(&e, a)
        .into_iter()
        .find(|ev| ev["op"] == "start")
        .expect("a start event");
    assert_eq!(start["payload"]["via"], "git", "{start}");
    assert!(start["payload"].get("actor").is_none(), "{start}");
    // D140 reads no holder off it, so an agent's plain start is not refused.
    call(&e, "task.start", json!({ "ref": b, "actor": "agent-1" })).expect("not refused");
}

#[test]
fn a_reopened_pr_moves_back_to_open_and_a_second_close_is_noted_again() {
    let e = engine();
    let a = add(&e, "reopened");
    let branch = format!("task/{a}-r");
    event(&e, "opened", &branch, json!({ "pr": PR }));
    event(&e, "closed", &branch, json!({ "pr": PR }));
    let out = event(&e, "opened", &branch, json!({ "pr": PR }));
    assert_eq!(out["changed"], true, "{out}");
    assert!(refs(&e, a).contains(&("pr".into(), PR.into(), "open".into())));
    event(&e, "closed", &branch, json!({ "pr": PR }));
    let closed = notes(&e, a)
        .iter()
        .filter(|n| n.contains("closed without merging"))
        .count();
    assert_eq!(closed, 2);
}

#[test]
fn a_new_pr_on_a_branch_is_recorded_beside_the_closed_one() {
    let e = engine();
    let a = add(&e, "second try");
    let branch = format!("task/{a}-t");
    event(&e, "closed", &branch, json!({ "pr": PR }));
    let second = "https://github.com/o/r/pull/8";
    let out = event(&e, "opened", &branch, json!({ "pr": second }));
    assert_eq!(out["transition"], "started", "{out}");
    let got = refs(&e, a);
    assert!(
        got.contains(&("pr".into(), PR.into(), "closed".into())),
        "{got:?}"
    );
    assert!(
        got.contains(&("pr".into(), second.into(), "open".into())),
        "{got:?}"
    );
}

#[test]
fn merged_on_a_cancelled_task_is_noted_once() {
    let e = engine();
    let a = add(&e, "dropped");
    call(&e, "task.cancel", json!({ "ref": a })).unwrap();
    let branch = format!("task/{a}-c");
    let out = event(&e, "merged", &branch, json!({ "pr": PR, "sha": "d4" }));
    assert_eq!(out["transition"], "annotated", "{out}");
    assert_eq!(
        notes(&e, a).last().unwrap(),
        &format!("Merged {PR} (d4) while the task was cancelled")
    );
    let n = notes(&e, a).len();
    assert_eq!(
        event(&e, "merged", &branch, json!({ "pr": PR, "sha": "d4" }))["changed"],
        false
    );
    assert_eq!(notes(&e, a).len(), n);
}

#[test]
fn the_pr_and_the_sha_have_one_spelling() {
    let e = engine();
    let a = add(&e, "normalised");
    event(
        &e,
        "merged",
        &format!("task/{a}-s"),
        json!({ "pr": format!(" {PR}/ "), "sha": " ABC123 " }),
    );
    let got = refs(&e, a);
    assert!(
        got.contains(&("pr".into(), PR.into(), "merged".into())),
        "{got:?}"
    );
    assert!(
        got.contains(&("commit".into(), "abc123".into(), "merged".into())),
        "{got:?}"
    );
}

#[test]
fn undo_steps_over_git_bookkeeping_to_the_last_change() {
    let e = engine();
    let a = add(&e, "undo me");
    call(&e, "task.start", json!({ "ref": a })).unwrap();
    call(&e, "annotation.add", json!({ "ref": a, "body": "mine" })).unwrap();
    // Active already: the event only records refs.
    event(&e, "opened", &format!("task/{a}-u"), json!({ "pr": PR }));
    let out = call(&e, "event.revert", json!({})).expect("undo reaches past git.ref");
    assert_eq!(out["reverted"]["op"], "annotation.add", "{out}");
    assert!(notes(&e, a).iter().all(|n| n != "mine"));
}

#[test]
fn undoing_a_merge_completion_is_one_step() {
    let e = engine();
    let a = add(&e, "completed by a merge");
    event(
        &e,
        "merged",
        &format!("task/{a}-m"),
        json!({ "pr": PR, "sha": "e5" }),
    );
    let out = call(&e, "event.revert", json!({})).expect("undo");
    assert_eq!(out["reverted"]["op"], "done", "{out}");
    assert_eq!(get(&e, a)["status"], "pending");
}

#[test]
fn a_retry_writes_the_note_a_failed_call_left_missing() {
    let e = engine();
    let a = add(&e, "note missing");
    // A ref recorded closed with no note behind it, as a call that failed
    // between the two commits leaves it.
    let mut doc = call(&e, "store.export", json!({})).unwrap();
    doc["tasks"][0]["git_refs"] = json!([
        {
            "id": "019f0000-0000-7000-8000-0000000000g1", "kind": "pr", "ref": PR,
            "state": "closed", "created": "2026-01-01T00:00:00Z"
        },
        {
            "id": "019f0000-0000-7000-8000-0000000000g2", "kind": "branch",
            "ref": format!("task/{a}-n"), "state": "closed",
            "created": "2026-01-01T00:00:00Z"
        }
    ]);
    call(&e, "store.import", doc).unwrap();
    let out = event(&e, "closed", &format!("task/{a}-n"), json!({ "pr": PR }));
    assert_eq!(out["transition"], "annotated", "{out}");
    assert!(notes(&e, a)
        .last()
        .unwrap()
        .contains("closed without merging"));
}

// ---- review round 3 ---------------------------------------------------------

#[test]
fn the_fact_lives_on_the_branch_so_with_or_without_pr_there_is_one_note() {
    let e = engine();
    let a = add(&e, "one note");
    call(&e, "check.add", json!({ "ref": a, "body": "open" })).unwrap();
    let branch = format!("task/{a}-b");
    event(&e, "merged", &branch, json!({ "pr": PR }));
    let n = notes(&e, a).len();
    let out = event(&e, "merged", &branch, json!({}));
    assert_eq!(out["changed"], false, "{out}");
    assert_eq!(notes(&e, a).len(), n);
}

#[test]
fn a_reason_is_kept_when_the_event_still_changed_something() {
    let e = engine();
    let a = add(&e, "merged, then closed from another branch");
    event(&e, "merged", &format!("task/{a}-x"), json!({ "pr": PR }));
    let out = event(&e, "closed", "other-branch", json!({ "pr": PR, "task": a }));
    assert_eq!(out["changed"], true, "{out}");
    assert!(
        out["reason"].as_str().is_some_and(|r| r.contains("merged")),
        "{out}"
    );
}

#[test]
fn a_merge_does_not_complete_again_a_task_the_user_reopened() {
    let e = engine();
    let a = add(&e, "reopened by hand");
    let branch = format!("task/{a}-r");
    let extra = json!({ "pr": PR, "sha": "f1" });
    event(&e, "merged", &branch, extra.clone());
    call(&e, "task.reopen", json!({ "ref": a })).unwrap();
    let out = event(&e, "merged", &branch, extra);
    assert_eq!(out["changed"], false, "{out}");
    assert!(
        out["reason"]
            .as_str()
            .is_some_and(|r| r.contains("reopened")),
        "{out}"
    );
    assert_ne!(get(&e, a)["status"], "done");
}

#[test]
fn a_git_event_acts_for_the_live_claim_holder() {
    let e = engine();
    let a = add(&e, "claimed");
    call(&e, "task.claim", json!({ "ref": a, "actor": "agent-1" })).unwrap();
    let branch = format!("task/{a}-c");
    let out = event(&e, "opened", &branch, json!({ "pr": PR }));
    assert_eq!(out["transition"], "started", "{out}");
    let start = events(&e, a)
        .into_iter()
        .find(|ev| ev["op"] == "start")
        .expect("a start event");
    assert_eq!(start["payload"]["holder"], "agent-1", "{start}");
    let out = event(&e, "merged", &branch, json!({ "pr": PR }));
    assert_eq!(out["transition"], "completed", "{out}");
    assert!(get(&e, a)["claimed_by"].is_null(), "done ends the claim");
}

#[test]
fn refs_are_ordered_by_instant_not_by_text() {
    let e = engine();
    let a = add(&e, "ordered");
    let mut doc = call(&e, "store.export", json!({})).unwrap();
    doc["tasks"][0]["git_refs"] = json!([
        {
            "id": "019f0000-0000-7000-8000-0000000000h1", "kind": "branch",
            "ref": "task/x", "state": "open", "created": "2026-01-01T00:00:10.5Z"
        },
        {
            "id": "019f0000-0000-7000-8000-0000000000h2", "kind": "pr",
            "ref": PR, "state": "open", "created": "2026-01-01T00:00:10Z"
        }
    ]);
    call(&e, "store.import", doc).unwrap();
    let kinds: Vec<String> = refs(&e, a).into_iter().map(|(k, _, _)| k).collect();
    assert_eq!(kinds, vec!["pr", "branch"], "10s comes before 10.5s");
}

// ---- final round --------------------------------------------------------------

#[test]
fn both_spellings_of_a_branch_share_one_row() {
    let e = engine();
    let a = add(&e, "one branch");
    event(&e, "opened", &format!("refs/heads/task/{a}-x"), json!({}));
    event(&e, "closed", &format!("  task/{a}-x "), json!({}));
    let branches: Vec<(String, String, String)> = refs(&e, a)
        .into_iter()
        .filter(|(k, _, _)| k == "branch")
        .collect();
    assert_eq!(
        branches,
        vec![("branch".into(), format!("task/{a}-x"), "closed".into())]
    );
}

#[test]
fn concurrent_merge_reports_from_two_processes_write_one_note() {
    let dir = std::env::temp_dir().join(format!(
        "tasqx-git-race-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tasks.db");
    let path = path.to_str().unwrap().to_string();
    let first = Engine::open(&path).unwrap();
    let a = add(&first, "raced");
    drop(first);
    for _ in 0..5 {
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    let e = Engine::open(&path).unwrap();
                    barrier.wait();
                    event(
                        &e,
                        "merged",
                        &format!("task/{a}-r"),
                        json!({ "pr": PR, "sha": "aa" }),
                    );
                });
            }
        });
    }
    let e = Engine::open(&path).unwrap();
    let delivery = notes(&e, a)
        .iter()
        .filter(|n| **n == format!("Merged {PR} (aa)"))
        .count();
    assert_eq!(delivery, 1, "{:?}", notes(&e, a));
    assert_eq!(get(&e, a)["status"], "done");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_different_pr_from_the_same_branch_may_complete_a_reopened_task() {
    let e = engine();
    let a = add(&e, "second PR");
    let branch = format!("task/{a}-p");
    event(&e, "merged", &branch, json!({ "pr": PR, "sha": "01" }));
    call(&e, "task.reopen", json!({ "ref": a })).unwrap();
    let second = "https://github.com/o/r/pull/8";
    let out = event(&e, "merged", &branch, json!({ "pr": second, "sha": "02" }));
    assert_eq!(out["transition"], "completed", "{out}");
    assert_eq!(get(&e, a)["status"], "done");
}

#[test]
fn a_held_then_fixed_merge_writes_one_delivery_note() {
    let e = engine();
    let a = add(&e, "held then fixed");
    let id =
        call(&e, "check.add", json!({ "ref": a, "body": "c" })).unwrap()["check"]["id"].clone();
    let branch = format!("task/{a}-h");
    event(&e, "merged", &branch, json!({ "pr": PR }));
    call(
        &e,
        "check.set",
        json!({ "ref": a, "check_id": id, "state": "passed" }),
    )
    .unwrap();
    event(&e, "merged", &branch, json!({ "pr": PR }));
    event(&e, "merged", &branch, json!({ "pr": PR }));
    let delivery = notes(&e, a)
        .iter()
        .filter(|n| **n == format!("Merged {PR}"))
        .count();
    assert_eq!(delivery, 1, "{:?}", notes(&e, a));
    assert_eq!(get(&e, a)["status"], "done");
}

#[test]
fn a_change_of_facts_moves_the_branch_row_so_a_merge_carries_it() {
    let e = engine();
    let a = add(&e, "facts move");
    let id =
        call(&e, "check.add", json!({ "ref": a, "body": "c" })).unwrap()["check"]["id"].clone();
    let branch = format!("task/{a}-m");
    event(&e, "merged", &branch, json!({ "pr": PR }));
    let at = |e: &Engine| {
        get(e, a)["git_refs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["kind"] == "branch")
            .unwrap()["modified"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let before = at(&e);
    std::thread::sleep(std::time::Duration::from_millis(5));
    call(
        &e,
        "check.set",
        json!({ "ref": a, "check_id": id, "state": "passed" }),
    )
    .unwrap();
    event(&e, "merged", &branch, json!({ "pr": PR }));
    assert_ne!(at(&e), before, "the facts moved, so modified did");
}

#[test]
fn an_import_keeps_the_earliest_created_by_instant() {
    let e = engine();
    let a = add(&e, "created");
    let row = |created: &str, modified: &str| {
        json!([{
            "id": "019f0000-0000-7000-8000-0000000000k1", "kind": "branch",
            "ref": "task/x", "state": "open", "created": created, "modified": modified
        }])
    };
    let mut doc = call(&e, "store.export", json!({})).unwrap();
    doc["tasks"][0]["git_refs"] = row("2026-01-01T00:00:10.5Z", "2026-01-01T00:00:10.5Z");
    call(&e, "store.import", doc.clone()).unwrap();
    let mut later = call(&e, "store.export", json!({})).unwrap();
    later["tasks"][0]["git_refs"] = row("2026-01-01T00:00:10Z", "2026-01-02T00:00:00Z");
    later["tasks"][0]["git_refs"][0]["state"] = json!("closed");
    later["merge"] = json!(true);
    call(&e, "store.import", later).unwrap();
    let g = get(&e, a)["git_refs"][0].clone();
    let created = g["created"].as_str().unwrap();
    assert!(
        created.starts_with("2026-01-01T00:00:10") && !created.contains(".5"),
        "{g}"
    );
}

#[test]
fn opened_on_an_active_task_records_the_start_for_that_pr() {
    let e = engine();
    let a = add(&e, "already running");
    call(&e, "task.start", json!({ "ref": a })).unwrap();
    let branch = format!("task/{a}-s");
    event(&e, "opened", &branch, json!({ "pr": PR }));
    call(&e, "task.stop", json!({ "ref": a })).unwrap();
    let out = event(&e, "opened", &branch, json!({ "pr": PR }));
    assert_eq!(out["changed"], false, "{out}");
    assert_eq!(get(&e, a)["status"], "pending");
}
