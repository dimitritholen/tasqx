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
    [("task", TASK_THRESHOLD, 0.15), ("doc", DOC_THRESHOLD, 0.77)];

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
            e.similar_tasks("new", new["title"].as_str().unwrap(), Some("tasqx"), -1.0)
        }
        "doc" => {
            ok(
                &e,
                "memory.add",
                json!({ "title": old["title"], "body": old["body"], "project": "tasqx" }),
            );
            e.similar_docs(
                "new",
                new["title"].as_str().unwrap(),
                new["body"].as_str().unwrap(),
                Some("tasqx"),
                -1.0,
            )
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
