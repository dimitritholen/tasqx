//! Recall@5 of memory search across English and Dutch (DESIGN.md D234).
//!
//! `fixtures/multilingual-eval.json` holds public tasqx text only: English
//! excerpts of DESIGN.md §12 and Dutch paraphrases of other §12 rulings,
//! written for the fixture. Each query expects exactly one doc. The docs are
//! imported into an in-memory store, each under a neutral title, and every
//! query runs through `memory.search` in its default mode — hybrid: bm25 and
//! meaning fused by reciprocal rank (D196) — at limit 5, which is what a
//! user gets. Recall@5 is reported per language pair, query language first.
//!
//! The floors are the targets stated before any model swap, against
//! potion-base-8M's hybrid baseline (en→en 0.85, nl→nl 0.80, en→nl 0.10,
//! nl→en 0.05): Dutch and both cross-lingual pairs at least 0.15 above it,
//! English at most 0.02 below it. No candidate meets them yet (task #2331):
//! potion-multilingual-128M pruned to English and Dutch, with the language
//! direction projected out, missed them even with the similarity floor and
//! the vocabulary gate retuned for it on a held-out half of this fixture and
//! of `memory-eval.json` (seed 2331, stratified). So the test is a
//! measurement: run it with
//! `cargo test -p tasqx-core --test multilingual_eval -- --ignored --nocapture`.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine};

const FIXTURE: &str = include_str!("fixtures/multilingual-eval.json");

const FLOORS: &[(&str, f64)] = &[
    ("en->en", 0.83),
    ("nl->nl", 0.95),
    ("en->nl", 0.25),
    ("nl->en", 0.20),
];

fn ok(e: &Engine, method: &str, params: Value) -> Value {
    dispatch(e, method, &params).unwrap_or_else(|err| panic!("{method} {params}: {err:?}"))
}

#[test]
#[ignore = "a measurement: no candidate model meets these floors yet (task #2331)"]
fn hybrid_recall_at_5_holds_in_dutch_english_and_across_them() {
    let fx: Value = serde_json::from_str(FIXTURE).unwrap();
    let e = Engine::open_in_memory().unwrap();
    let docs = fx["docs"].as_array().unwrap();
    let lang: HashMap<&str, &str> = docs
        .iter()
        .map(|d| (d["key"].as_str().unwrap(), d["lang"].as_str().unwrap()))
        .collect();
    let batch: Vec<Value> = docs
        .iter()
        .enumerate()
        .map(|(i, d)| json!({ "title": format!("m{i}"), "body": d["text"], "source": d["key"] }))
        .collect();
    let res = ok(&e, "memory.import", json!({ "docs": batch }));
    let id2key: HashMap<String, String> = res["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["source"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let queries = fx["queries"].as_array().unwrap();
    assert!(queries.len() >= 30, "the ruling asks for at least 30 pairs");

    let mut pairs: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut misses = String::new();
    for q in queries {
        let want = q["expect"].as_str().unwrap();
        let pair = format!("{}->{}", q["lang"].as_str().unwrap(), lang[want]);
        let res = ok(
            &e,
            "memory.search",
            json!({ "query": q["text"], "limit": 5 }),
        );
        let got: Vec<&str> = res["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| id2key[h["id"].as_str().unwrap()].as_str())
            .collect();
        let hit = got.contains(&want);
        if !hit {
            misses.push_str(&format!(
                "  {pair} {:?} wants {want}, got {got:?}\n",
                q["text"]
            ));
        }
        let e = pairs.entry(pair).or_default();
        e.0 += usize::from(hit);
        e.1 += 1;
    }

    let mut report = String::new();
    let mut failures = Vec::new();
    for (pair, floor) in FLOORS {
        let (hits, n) = pairs.get(*pair).copied().unwrap_or_default();
        assert!(n >= 10, "{pair} has {n} queries");
        let r = hits as f64 / n as f64;
        report.push_str(&format!(
            "{pair}  recall@5 {r:.2}  ({hits}/{n}, floor {floor})\n"
        ));
        if r < *floor {
            failures.push(format!("{pair} recall@5 {r:.2} is under {floor}"));
        }
    }
    eprintln!("{report}misses:\n{misses}");
    assert!(failures.is_empty(), "{}\n{report}", failures.join("\n"));
}
