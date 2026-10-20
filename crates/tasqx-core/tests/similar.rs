//! D237: `task.add` and `memory.add` name the open tasks and live docs that
//! look like what was just written, in `similar`, and the write succeeds
//! whatever they find.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine};

fn engine() -> Engine {
    let e = Engine::open_in_memory().expect("open in-memory store");
    ok(&e, "project.create", json!({ "name": "tasqx" }));
    ok(&e, "project.create", json!({ "name": "other" }));
    e
}

fn ok(e: &Engine, method: &str, params: Value) -> Value {
    dispatch(e, method, &params).unwrap_or_else(|err| panic!("{method} {params}: {err:?}"))
}

/// Have the D196 index store a vector for every doc, as any search does:
/// a doc without one is not a candidate (D237).
fn index(e: &Engine) {
    ok(e, "memory.search", json!({ "query": "anything" }));
}

fn add_task(e: &Engine, title: &str, project: &str) -> Value {
    ok(e, "task.add", json!({ "title": title, "project": project }))
}

fn similar_ids(out: &Value, key: &str) -> Vec<Value> {
    out["similar"]
        .as_array()
        .unwrap_or_else(|| panic!("`similar` is always an array: {out}"))
        .iter()
        .map(|s| s[key].clone())
        .collect()
}

const TITLE: &str = "Flaky daemon test: the watch reconnect test panics when its daemon dies";
const AGAIN: &str = "Flaky test: watch reconnect panics after the daemon dies";

#[test]
fn task_add_names_an_open_near_duplicate_in_the_same_project() {
    let e = engine();
    let first = add_task(&e, TITLE, "tasqx");
    let unrelated = add_task(&e, "Release 0.14.0 with a changelog section", "tasqx");
    assert_eq!(similar_ids(&first, "short_id"), Vec::<Value>::new());
    assert_eq!(similar_ids(&unrelated, "short_id"), Vec::<Value>::new());

    let again = add_task(&e, AGAIN, "tasqx");
    assert!(
        again["short_id"].is_i64(),
        "the write still succeeds: {again}"
    );
    assert_eq!(
        similar_ids(&again, "short_id"),
        vec![first["short_id"].clone()]
    );
    let hit = &again["similar"][0];
    assert_eq!(hit["id"], first["id"]);
    assert_eq!(hit["title"], TITLE);
    let s = hit["similarity"].as_f64().unwrap();
    assert!(s > 0.0 && s <= 1.0, "{hit}");

    // Another project's open task is not this project's duplicate.
    let elsewhere = add_task(&e, AGAIN, "other");
    assert_eq!(similar_ids(&elsewhere, "short_id"), Vec::<Value>::new());
}

#[test]
fn done_and_cancelled_tasks_are_never_offered() {
    let e = engine();
    let done = add_task(&e, TITLE, "tasqx");
    ok(&e, "task.done", json!({ "ref": done["short_id"] }));
    let cancelled = add_task(&e, TITLE, "tasqx");
    ok(&e, "task.cancel", json!({ "ref": cancelled["short_id"] }));
    let again = add_task(&e, AGAIN, "tasqx");
    assert_eq!(similar_ids(&again, "short_id"), Vec::<Value>::new());
}

#[test]
fn at_most_three_tasks_are_offered_best_first() {
    let e = engine();
    for _ in 0..5 {
        add_task(&e, TITLE, "tasqx");
    }
    let again = add_task(&e, TITLE, "tasqx");
    let sims: Vec<f64> = again["similar"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["similarity"].as_f64().unwrap())
        .collect();
    assert_eq!(sims.len(), 3, "{again}");
    assert!(sims.windows(2).all(|w| w[0] >= w[1]), "{sims:?}");
}

const RULING: &str = "Landing a task PR: gh pr merge --rebase without --delete-branch";
const RULING_BODY: &str = "Merge a task branch with gh pr merge --auto --rebase and never pass \
    --delete-branch, because the worktree still has the branch checked out.";
const RULING_AGAIN: &str = "Land a PR with a rebase merge and keep the branch";
const RULING_AGAIN_BODY: &str = "When landing a task branch, run gh pr merge --rebase --auto \
    without --delete-branch: the branch is still checked out in a worktree.";

#[test]
fn memory_add_names_a_live_near_duplicate_doc() {
    let e = engine();
    let first = ok(
        &e,
        "memory.add",
        json!({ "title": RULING, "body": RULING_BODY, "project": "tasqx" }),
    );
    assert_eq!(similar_ids(&first, "id"), Vec::<Value>::new());
    let other = ok(
        &e,
        "memory.add",
        json!({ "title": "Release checklist", "body": "Bump the version, write the changelog \
            section, tag and push.", "project": "tasqx" }),
    );
    assert_eq!(similar_ids(&other, "id"), Vec::<Value>::new());

    index(&e);
    let again = ok(
        &e,
        "memory.add",
        json!({ "title": RULING_AGAIN, "body": RULING_AGAIN_BODY, "project": "tasqx" }),
    );
    assert!(again["id"].is_string(), "the write still succeeds: {again}");
    assert_eq!(similar_ids(&again, "id"), vec![first["id"].clone()]);
    assert_eq!(again["similar"][0]["title"], RULING);

    // A removed doc is gone, and so is the warning.
    ok(&e, "memory.remove", json!({ "id": first["id"] }));
    index(&e);
    let third = ok(
        &e,
        "memory.add",
        json!({ "title": RULING, "body": RULING_BODY, "project": "tasqx" }),
    );
    assert_eq!(similar_ids(&third, "id"), vec![again["id"].clone()]);
}

#[test]
fn memory_add_compares_its_own_project_and_unscoped_docs_only() {
    let e = engine();
    let unscoped = ok(
        &e,
        "memory.add",
        json!({ "title": RULING, "body": RULING_BODY }),
    );
    ok(
        &e,
        "memory.add",
        json!({ "title": RULING, "body": RULING_BODY, "project": "other" }),
    );
    index(&e);
    let again = ok(
        &e,
        "memory.add",
        json!({ "title": RULING_AGAIN, "body": RULING_AGAIN_BODY, "project": "tasqx" }),
    );
    assert_eq!(similar_ids(&again, "id"), vec![unscoped["id"].clone()]);
}

/// D237's latency budget: `task.add`'s p95 on a store holding 5,000 open
/// tasks in the project, each with a description of the length this
/// project's own tasks have (the labelled pairs' descriptions, reused), stays
/// under 50 ms. A measurement, so it is ignored by default and meant for an
/// optimised build:
///
/// `cargo test --release -p tasqx-core --test similar -- --ignored --nocapture`
#[test]
#[ignore = "a timing; run in release, see the doc comment"]
fn task_add_on_five_thousand_open_tasks_stays_within_its_budget() {
    let pairs: Value =
        serde_json::from_str(include_str!("../src/engine/similar/pairs.json")).unwrap();
    let texts: Vec<(String, String)> = pairs["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["kind"] == "task")
        .map(|p| {
            let old = &p["existing"];
            (
                old["title"].as_str().unwrap().to_string(),
                old["description"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let dir = std::env::temp_dir().join(format!("tasqx-similar-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let e = Engine::open(&dir.join("tasks.db").to_string_lossy()).unwrap();
    ok(&e, "project.create", json!({ "name": "tasqx" }));
    // Parked as done while the store fills, so the fill itself compares
    // nothing, then all reopened at once.
    let park = |id: &Value| {
        e.conn()
            .execute(
                "UPDATE tasks SET status = 'done' WHERE id = ?1",
                [id.as_str().unwrap()],
            )
            .unwrap();
    };
    for i in 0..5_000 {
        let (title, description) = &texts[i % texts.len()];
        let t = add_task(&e, &format!("{title} ({i})"), "tasqx");
        if !description.is_empty() {
            ok(
                &e,
                "annotation.add",
                json!({ "ref": t["short_id"], "body": description }),
            );
        }
        park(&t["id"]);
    }
    e.conn()
        .execute("UPDATE tasks SET status = 'pending'", [])
        .unwrap();
    let db = dir.join("tasks.db").to_string_lossy().into_owned();
    let time_add = |e: &Engine, i: usize| {
        let t0 = std::time::Instant::now();
        let out = add_task(e, &format!("{AGAIN} {i}"), "tasqx");
        let d = t0.elapsed();
        assert!(out["similar"].is_array());
        e.conn()
            .execute(
                "UPDATE tasks SET status = 'done' WHERE id = ?1",
                [out["id"].as_str().unwrap()],
            )
            .unwrap();
        d
    };
    // Cold: a fresh engine per add, as a one-shot `tasqx add` runs.
    let cold: Vec<std::time::Duration> = (0..20)
        .map(|i| time_add(&Engine::open(&db).unwrap(), i))
        .collect();
    // Warm: one engine, as the daemon and `mcp serve` run, after its first add.
    time_add(&e, 100);
    let warm: Vec<std::time::Duration> = (0..40).map(|i| time_add(&e, 200 + i)).collect();
    let p95s: Vec<std::time::Duration> = [("cold", cold), ("warm", warm)]
        .into_iter()
        .map(|(kind, mut took)| {
            took.sort();
            let p50 = took[took.len() / 2];
            let p95 = took[took.len() * 95 / 100];
            println!(
                "task.add on 5,000 open tasks, {kind}: p50 {p50:?}, p95 {p95:?}, max {:?}",
                took[took.len() - 1]
            );
            p95
        })
        .collect();
    drop(e);
    let _ = std::fs::remove_dir_all(&dir);
    if !cfg!(debug_assertions) {
        assert!(
            p95s.iter()
                .all(|p| *p < std::time::Duration::from_millis(50)),
            "p95 {p95s:?}"
        );
    }
}
