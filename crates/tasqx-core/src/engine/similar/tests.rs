//! D237's threshold, held to the labelled pairs it was chosen on.
//!
//! `pairs.json` holds pairs from this project's own store and repository:
//! an `existing` open task or doc, and a `new` one written later, labelled
//! `duplicate` when the new one restates the existing one's work or ruling
//! and not when it is different work in the same area. Each pair is scored
//! the way `task.add` and `memory.add` score it — the existing side stored
//! alone in a fresh store, the new side compared against it with no floor —
//! and precision and recall at [`TASK_THRESHOLD`] and [`DOC_THRESHOLD`] are
//! checked against what they were when the thresholds were chosen.
//!
//! Run with `TASQX_SIMILAR_SWEEP=1` and `--nocapture` to print every pair's
//! score and the precision and recall a range of thresholds would give: what
//! re-tuning after a model change starts from.

use serde_json::{json, Value};

use super::*;
use crate::dispatch;

const PAIRS: &str = include_str!("pairs.json");

/// Precision at each kind's threshold may not fall below this (D237).
const PRECISION_FLOOR: f64 = 0.9;

/// Recall at each kind's threshold when it was chosen; less is a regression.
const RECALL_FLOORS: [(&str, f64, f64); 2] =
    [("task", TASK_THRESHOLD, 0.15), ("doc", DOC_THRESHOLD, 0.66)];

fn ok(e: &Engine, method: &str, params: Value) -> Value {
    dispatch(e, method, &params).unwrap_or_else(|err| panic!("{method} {params}: {err:?}"))
}

/// The pair's similarity, as the add of its `new` side would compute it
/// against its `existing` side.
fn score(pair: &Value) -> f64 {
    let e = Engine::open_in_memory().unwrap();
    ok(&e, "project.create", json!({ "name": "tasqx" }));
    let (old, new) = (&pair["existing"], &pair["new"]);
    let hits = match pair["kind"].as_str().unwrap() {
        "task" => {
            let t = ok(
                &e,
                "task.add",
                json!({ "title": old["title"], "project": "tasqx" }),
            );
            if let Some(d) = old["description"].as_str() {
                ok(
                    &e,
                    "annotation.add",
                    json!({ "ref": t["short_id"], "body": d }),
                );
            }
            e.similar_tasks(new["title"].as_str().unwrap(), Some("tasqx"), -1.0)
                .0
        }
        "doc" => {
            ok(
                &e,
                "memory.add",
                json!({ "title": old["title"], "body": old["body"], "project": "tasqx" }),
            );
            let chunks = super::super::vectors::doc_vectors(
                new["title"].as_str().unwrap(),
                new["body"].as_str().unwrap(),
            );
            e.similar_docs(chunks.as_deref(), Some("tasqx"), -1.0).0
        }
        k => panic!("unknown kind {k}"),
    };
    hits[0]["similarity"].as_f64().unwrap_or(-1.0)
}

/// `(precision, recall)` of calling every pair scoring at or above
/// `threshold` a duplicate.
fn rates(scored: &[(f64, bool)], threshold: f64) -> (f64, f64) {
    let tp = scored.iter().filter(|(s, d)| *d && *s >= threshold).count() as f64;
    let fp = scored
        .iter()
        .filter(|(s, d)| !*d && *s >= threshold)
        .count() as f64;
    let positives = scored.iter().filter(|(_, d)| *d).count() as f64;
    let precision = if tp + fp == 0.0 { 1.0 } else { tp / (tp + fp) };
    (precision, tp / positives)
}

#[test]
fn the_threshold_was_tuned_for_this_model() {
    let fixture: Value = serde_json::from_str(PAIRS).unwrap();
    assert_eq!(
        (TUNED_FOR, fixture["model"].as_str().unwrap()),
        (embed::MODEL_ID, embed::MODEL_ID),
        "the embedding model changed: re-tune THRESHOLD on similar/pairs.json \
         (TASQX_SIMILAR_SWEEP=1), then record the new model here and in the fixture (D237)"
    );
}

#[test]
fn the_threshold_holds_its_precision_and_recall_on_the_labelled_pairs() {
    let fixture: Value = serde_json::from_str(PAIRS).unwrap();
    let pairs = fixture["pairs"].as_array().unwrap();
    assert!(
        pairs.len() >= 40,
        "D237 asks for at least 40 labelled pairs"
    );
    let scored: Vec<(&str, f64, bool)> = pairs
        .iter()
        .map(|p| {
            (
                p["kind"].as_str().unwrap(),
                score(p),
                p["duplicate"].as_bool().unwrap(),
            )
        })
        .collect();
    let sweep = std::env::var_os("TASQX_SIMILAR_SWEEP").is_some();
    if sweep {
        for (p, (k, s, d)) in pairs.iter().zip(&scored) {
            let dup = if *d { "DUP" } else { "   " };
            println!("{s:.3} {dup} {k:4} {}", p["new"]["title"].as_str().unwrap());
        }
    }
    let mut failures = Vec::new();
    for (kind, threshold, recall_floor) in RECALL_FLOORS {
        let of_kind: Vec<(f64, bool)> = scored
            .iter()
            .filter(|(k, _, _)| *k == kind)
            .map(|&(_, s, d)| (s, d))
            .collect();
        let dups = of_kind.iter().filter(|(_, d)| *d).count();
        assert!(
            dups > 0 && dups < of_kind.len(),
            "{kind} pairs need both labels"
        );
        if sweep {
            for t in (50..=95).map(|t| f64::from(t) / 100.0) {
                let (p, r) = rates(&of_kind, t);
                println!("{kind} at {t:.2}: precision {p:.3} recall {r:.3}");
            }
        }
        let (precision, recall) = rates(&of_kind, threshold);
        println!(
            "{kind}: {} pairs, {dups} duplicates; at {threshold}: precision {precision:.3}, \
             recall {recall:.3}",
            of_kind.len()
        );
        if precision < PRECISION_FLOOR || recall < recall_floor {
            failures.push(format!(
                "{kind} at {threshold}: precision {precision:.3} (floor {PRECISION_FLOOR}), \
                 recall {recall:.3} (floor {recall_floor})"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// An add embeds the new doc and nothing else: no doc the index lacks is
/// embedded on its behalf, so N adds embed N texts however many there are.
#[test]
fn n_doc_adds_embed_exactly_n_texts() {
    let e = Engine::open_in_memory().unwrap();
    let before = e.similar.borrow().embedded;
    for i in 0..5 {
        ok(
            &e,
            "memory.add",
            json!({ "title": format!("doc {i}"), "body": "the same body each time" }),
        );
    }
    assert_eq!(e.similar.borrow().embedded - before, 5);
}

/// A doc added a moment ago is a candidate for the next add: its vectors
/// are stored with it. A doc whose vector is not stored is not one.
#[test]
fn a_doc_added_a_moment_ago_is_a_candidate_and_an_unindexed_one_is_not() {
    let e = Engine::open_in_memory().unwrap();
    let doc = json!({ "title": "release checklist", "body": "bump, changelog, tag, push" });
    let first = ok(&e, "memory.add", doc.clone());
    let second = ok(&e, "memory.add", doc.clone());
    assert_eq!(second["similar"][0]["id"], first["id"], "{second}");
    e.conn
        .execute(
            "DELETE FROM memory_vectors WHERE owner_id = ?1",
            [first["id"].as_str().unwrap()],
        )
        .unwrap();
    let third = ok(&e, "memory.add", doc);
    let named: Vec<&Value> = third["similar"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["id"])
        .collect();
    assert_eq!(named, [&second["id"]], "{third}");
}

/// A long-lived engine embeds an open task once: an add embeds its own
/// title and whatever changed since the last add, not every open task.
#[test]
fn an_add_embeds_only_the_open_tasks_that_changed() {
    let e = Engine::open_in_memory().unwrap();
    for i in 0..10 {
        ok(&e, "task.add", json!({ "title": format!("task {i}") }));
    }
    let before = e.similar.borrow().embedded;
    ok(&e, "task.add", json!({ "title": "one more" }));
    // Its title, and task 9's (task 9's own add ran before it existed).
    assert_eq!(e.similar.borrow().embedded - before, 2);
    let before = e.similar.borrow().embedded;
    ok(
        &e,
        "task.modify",
        json!({ "ref": 3, "set": { "title": "renamed" } }),
    );
    ok(&e, "task.add", json!({ "title": "and another" }));
    // Its title, "one more", and the renamed task 3.
    assert_eq!(e.similar.borrow().embedded - before, 3);
}

/// A comparison that panics or errs leaves the add standing, with nothing
/// in `similar`.
#[test]
fn a_failing_comparison_answers_an_empty_similar() {
    for failure in [Failure::Panic, Failure::Error] {
        let e = Engine::open_in_memory().unwrap();
        ok(&e, "task.add", json!({ "title": "the same task" }));
        ok(&e, "memory.add", json!({ "title": "same", "body": "same" }));
        e.similar.borrow_mut().fail = Some(failure);
        let t = ok(&e, "task.add", json!({ "title": "the same task" }));
        assert!(t["short_id"].is_i64());
        assert_eq!(t["similar"], json!([]));
        assert_eq!(t["similar_skipped"], true);
        let d = ok(&e, "memory.add", json!({ "title": "same", "body": "same" }));
        assert!(d["id"].is_string());
        assert_eq!(d["similar"], json!([]));
        assert_eq!(d["similar_skipped"], true);
        e.similar.borrow_mut().fail = None;
        let t = ok(&e, "task.add", json!({ "title": "the same task" }));
        assert_eq!(t["similar"].as_array().unwrap().len(), 2, "{t}");
        assert_eq!(t["similar_skipped"], false);
    }
}

/// A store whose tables cannot be read for the comparison still adds.
#[test]
fn an_sql_error_in_the_comparison_does_not_fail_the_add() {
    let e = Engine::open_in_memory().unwrap();
    e.conn
        .execute_batch("ALTER TABLE annotations RENAME TO annotations_gone")
        .unwrap();
    let t = ok(&e, "task.add", json!({ "title": "still added" }));
    assert!(t["short_id"].is_i64());
    assert_eq!(t["similar"], json!([]));
    assert_eq!(t["similar_skipped"], true);
}

/// Past the bound the least recently used entries of other projects leave
/// first; the current project's are never evicted, even when it alone is
/// over the bound, and every one of them is compared.
#[test]
fn the_cache_evicts_other_projects_least_recently_used_first() {
    let e = Engine::open_in_memory().unwrap();
    for p in ["a", "b", "c"] {
        ok(&e, "project.create", json!({ "name": p }));
    }
    e.similar.borrow_mut().bound = Some(3);
    let short = |out: Value| out["short_id"].as_i64().unwrap();
    let a1 = short(ok(
        &e,
        "task.add",
        json!({ "title": "alpha", "project": "a" }),
    ));
    let b1 = short(ok(
        &e,
        "task.add",
        json!({ "title": "beta", "project": "b" }),
    ));
    // Embeds a1, then b1; a1 is used again after b1, so b1 is now the oldest.
    ok(&e, "task.add", json!({ "title": "x", "project": "a" }));
    ok(&e, "task.add", json!({ "title": "x", "project": "b" }));
    ok(&e, "task.add", json!({ "title": "x", "project": "a" }));
    let held = |e: &Engine| -> Vec<i64> {
        let ids: Vec<String> = e.similar.borrow().tasks.keys().cloned().collect();
        let mut v: Vec<i64> = ids
            .iter()
            .map(|id| short(ok(e, "task.get", json!({ "ref": id }))))
            .collect();
        v.sort();
        v
    };
    assert!(held(&e).contains(&a1) && held(&e).contains(&b1));
    // Four open tasks in c, over the bound of three: c is kept and compared
    // whole, and the others go.
    for _ in 0..4 {
        ok(&e, "task.add", json!({ "title": "gamma", "project": "c" }));
    }
    let again = ok(&e, "task.add", json!({ "title": "gamma", "project": "c" }));
    assert_eq!(again["similar"].as_array().unwrap().len(), MAX, "{again}");
    let cache = e.similar.borrow();
    assert_eq!(cache.tasks.len(), 4, "only c's four are left");
    assert!(cache
        .tasks
        .values()
        .all(|c| c.project.as_deref() == Some("c")));
}

/// A hit refreshes an entry's place, so it is used, not merely embedded.
#[test]
fn a_hit_refreshes_an_entry() {
    let e = Engine::open_in_memory().unwrap();
    ok(&e, "task.add", json!({ "title": "one" }));
    ok(&e, "task.add", json!({ "title": "two" }));
    let stamp = |e: &Engine| e.similar.borrow().tasks.values().map(|c| c.stamp).max();
    let before = stamp(&e);
    ok(&e, "task.add", json!({ "title": "three" }));
    assert!(stamp(&e) > before);
}

/// Another project's open tasks stay embedded while this one is compared,
/// and a task that closes leaves.
#[test]
fn the_cache_keeps_other_projects_and_drops_closed_tasks() {
    let e = Engine::open_in_memory().unwrap();
    for p in ["a", "b"] {
        ok(&e, "project.create", json!({ "name": p }));
    }
    ok(&e, "task.add", json!({ "title": "in a", "project": "a" }));
    ok(&e, "task.add", json!({ "title": "in b", "project": "b" }));
    ok(&e, "task.add", json!({ "title": "x", "project": "a" }));
    ok(&e, "task.add", json!({ "title": "y", "project": "b" }));
    assert_eq!(
        e.similar.borrow().tasks.len(),
        2,
        "one per project, both kept"
    );
    ok(&e, "task.done", json!({ "ref": 1 }));
    ok(&e, "task.add", json!({ "title": "z", "project": "a" }));
    let ids: Vec<i64> = {
        let cache = e.similar.borrow();
        let mut v: Vec<i64> = cache
            .tasks
            .keys()
            .map(|id| {
                ok(&e, "task.get", json!({ "ref": id }))["short_id"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(ids, [2, 3], "the done task left, project b's stayed");
}

/// `task.next`'s summary skips a removed note.
#[test]
fn the_summary_skips_a_removed_note() {
    let e = Engine::open_in_memory().unwrap();
    ok(&e, "task.add", json!({ "title": "t" }));
    let first = ok(&e, "annotation.add", json!({ "ref": 1, "body": "first" }));
    ok(&e, "annotation.add", json!({ "ref": 1, "body": "second" }));
    ok(
        &e,
        "annotation.remove",
        json!({ "ref": 1, "annotation_id": first["annotation"]["id"] }),
    );
    assert_eq!(ok(&e, "task.next", json!({}))["summary"], "second");
}

/// The description is the note's first paragraph, `\r\n` or `\n`.
#[test]
fn the_description_is_the_first_paragraph_with_either_line_ending() {
    assert_eq!(opening_summary("one\r\ntwo\r\n\r\nthree"), "one two");
    assert_eq!(opening_summary("\n\none\ntwo\n\nthree"), "one two");
    assert_eq!(opening_summary("one\n \nthree"), "one");
    let long = "word ".repeat(200);
    let cut = opening_summary(&long);
    assert_eq!(cut.chars().count(), 401);
    assert!(cut.ends_with('…'));
    assert_eq!(opening_summary(&"x".repeat(400)), "x".repeat(400));
}
