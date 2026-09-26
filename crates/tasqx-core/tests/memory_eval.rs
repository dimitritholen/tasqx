//! Retrieval-quality regression guard for D196's hybrid memory search.
//!
//! `fixtures/memory-eval.json` is a labelled corpus about an invented platform
//! team: docs (some scoped to a project, some not), tasks whose annotation
//! bodies are the retrievable units, and queries, each with the keys of the
//! entries that answer it and a category:
//!
//! | category | what it probes |
//! |---|---|
//! | `paraphrase`, `concept` | no content word shared with the target; only meaning can find it |
//! | `synonym` | one word swapped, the rest shared |
//! | `keyword`, `identifier` | the target's own words, ids, codes and paths |
//! | `span` | words that only co-occur across two entries |
//! | `typo` | misspelled keywords |
//! | `dutch`, `long` | Dutch against English notes, a whole sentence |
//! | `oov` | words the model does not know |
//! | `unrelated` | nothing relevant is stored (`relevant: []`) |
//!
//! Doc keys are `d<n>`, annotation keys `a<n>`; task titles are not indexed and
//! never count. The labels are strict: an entry is relevant when it answers the
//! query, not when it mentions the topic.
//!
//! The corpus is loaded in-process into an in-memory store, and every query is
//! run through `memory.search` at limit 10 twice: once in the default (hybrid)
//! mode and once with `mode: "lexical"`. The test then asserts three things:
//!
//! - hybrid recall@10 stays at or above a floor per category. Each floor sits a
//!   little under what the code scored when the guard was written, so a real
//!   loss fails and a single re-ranked query does not;
//! - hybrid returns no more meaning-side hits (`via` `semantic` or `both`) on
//!   the queries with nothing relevant than it did then;
//! - lexical mode returns exactly the ranked lists in
//!   `fixtures/memory-eval-lexical.json`: D196 promised that `mode: "lexical"`
//!   is the pre-D196 search, and a metric can stay equal while the order moves.
//!
//! On failure it prints the per-category table (R@5, R@10, MRR@10, hit@1). A
//! change that genuinely improves ranking may raise a floor; one that lowers a
//! floor needs a ruling in DESIGN.md §12 saying why. To re-derive the lexical
//! lists after a deliberate change to lexical search, run this test with
//! `TASQX_EVAL_BLESS=1` and review the diff of the fixture.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine};

const CORPUS: &str = include_str!("fixtures/memory-eval.json");
const LEXICAL: &str = include_str!("fixtures/memory-eval-lexical.json");

/// Tasks cannot be unscoped; the corpus's unscoped tasks land here.
const UNSCOPED_TASK_PROJECT: &str = "team";
const LIMIT: u64 = 10;

/// Hybrid recall@10 floors. `OVERALL` is the mean over every query that has a
/// relevant entry.
const FLOORS: &[(&str, f64)] = &[
    ("OVERALL", 0.68),
    ("paraphrase", 0.14),
    ("concept", 0.50),
    ("identifier", 1.0),
    ("keyword", 0.97),
    ("typo", 0.97),
    ("span", 0.93),
];

/// Meaning-side hits hybrid returned on the `unrelated` and `oov` queries that
/// have nothing relevant, when the guard was written. More is a regression.
const NEGATIVE_MEANING_HITS_CAP: usize = 16;

fn ok(e: &Engine, method: &str, params: Value) -> Value {
    dispatch(e, method, &params).unwrap_or_else(|err| panic!("{method} {params}: {err:?}"))
}

/// Loads the corpus; returns tasqx id -> corpus key.
fn load(e: &Engine, corpus: &Value) -> HashMap<String, String> {
    let mut id2key = HashMap::new();
    let mut projects: Vec<&str> = corpus["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    projects.push(UNSCOPED_TASK_PROJECT);
    for name in projects {
        ok(e, "project.create", json!({ "name": name }));
    }

    // One import per project, in order of first appearance: bm25 ties break
    // on id, ids follow creation order, and this is the order the lexical
    // fixture was recorded in.
    let mut by_project: Vec<(Option<&str>, Vec<Value>)> = Vec::new();
    for d in corpus["docs"].as_array().unwrap() {
        let project = d["project"].as_str();
        let doc = json!({ "title": d["title"], "body": d["body"], "source": d["key"] });
        match by_project.iter_mut().find(|(p, _)| *p == project) {
            Some((_, docs)) => docs.push(doc),
            None => by_project.push((project, vec![doc])),
        }
    }
    for (project, docs) in by_project {
        let n = docs.len();
        let mut params = json!({ "docs": docs });
        if let Some(p) = project {
            params["project"] = json!(p);
        }
        let res = ok(e, "memory.import", params);
        assert_eq!(res["imported"], json!(n), "{res}");
        for row in res["docs"].as_array().unwrap() {
            id2key.insert(
                row["id"].as_str().unwrap().to_string(),
                row["source"].as_str().unwrap().to_string(),
            );
        }
    }

    for t in corpus["tasks"].as_array().unwrap() {
        let project = t["project"].as_str().unwrap_or(UNSCOPED_TASK_PROJECT);
        let res = ok(
            e,
            "task.add",
            json!({ "title": t["title"], "project": project }),
        );
        let task_ref = res["short_id"].to_string();
        for a in t["annotations"].as_array().unwrap() {
            let r = ok(
                e,
                "annotation.add",
                json!({ "ref": task_ref, "body": a["body"] }),
            );
            id2key.insert(
                r["annotation"]["id"].as_str().unwrap().to_string(),
                a["key"].as_str().unwrap().to_string(),
            );
        }
    }
    id2key
}

struct Row {
    category: String,
    query: String,
    relevant: BTreeSet<String>,
    ranked: Vec<String>,
    vias: Vec<String>,
}

fn run(e: &Engine, corpus: &Value, id2key: &HashMap<String, String>, mode: Value) -> Vec<Row> {
    corpus["queries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            let mut params = json!({ "query": q["q"], "limit": LIMIT });
            if !mode.is_null() {
                params["mode"] = mode.clone();
            }
            let res = ok(e, "memory.search", params);
            let hits = res["hits"].as_array().unwrap();
            Row {
                category: q["category"].as_str().unwrap().to_string(),
                query: q["q"].as_str().unwrap().to_string(),
                relevant: q["relevant"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| k.as_str().unwrap().to_string())
                    .collect(),
                ranked: hits
                    .iter()
                    .map(|h| {
                        let id = h["id"].as_str().unwrap();
                        id2key.get(id).cloned().unwrap_or_else(|| format!("?{id}"))
                    })
                    .collect(),
                vias: hits
                    .iter()
                    .map(|h| h["via"].as_str().unwrap_or("").to_string())
                    .collect(),
            }
        })
        .collect()
}

#[derive(Default, Clone, Copy)]
struct Metrics {
    n: usize,
    r5: f64,
    r10: f64,
    mrr: f64,
    hit1: f64,
}

fn recall(row: &Row, k: usize) -> f64 {
    let found = row
        .ranked
        .iter()
        .take(k)
        .filter(|key| row.relevant.contains(*key))
        .count();
    found as f64 / row.relevant.len() as f64
}

fn metrics<'a>(rows: impl Iterator<Item = &'a Row>) -> Metrics {
    let mut m = Metrics::default();
    for row in rows.filter(|r| !r.relevant.is_empty()) {
        m.n += 1;
        m.r5 += recall(row, 5);
        m.r10 += recall(row, 10);
        if let Some(i) = row
            .ranked
            .iter()
            .take(10)
            .position(|k| row.relevant.contains(k))
        {
            m.mrr += 1.0 / (i + 1) as f64;
            if i == 0 {
                m.hit1 += 1.0;
            }
        }
    }
    if m.n > 0 {
        let n = m.n as f64;
        m.r5 /= n;
        m.r10 /= n;
        m.mrr /= n;
        m.hit1 /= n;
    }
    m
}

/// Per-category metrics, `OVERALL` last.
fn table(rows: &[Row]) -> Vec<(String, Metrics)> {
    let cats: BTreeSet<&str> = rows.iter().map(|r| r.category.as_str()).collect();
    let mut out: Vec<(String, Metrics)> = cats
        .into_iter()
        .map(|c| {
            (
                c.to_string(),
                metrics(rows.iter().filter(|r| r.category == c)),
            )
        })
        .collect();
    out.push(("OVERALL".to_string(), metrics(rows.iter())));
    out
}

fn negative_meaning_hits(rows: &[Row]) -> usize {
    rows.iter()
        .filter(|r| r.relevant.is_empty())
        .flat_map(|r| &r.vias)
        .filter(|v| *v == "semantic" || *v == "both")
        .count()
}

fn render(label: &str, rows: &[Row]) -> String {
    let mut s = format!(
        "{label}\n{:<11}{:>4}  {:>6} {:>6} {:>6} {:>6}\n",
        "category", "n", "R@5", "R@10", "MRR10", "hit@1"
    );
    for (c, m) in table(rows) {
        if m.n == 0 {
            let _ = writeln!(s, "{c:<11}{:>4}  (nothing relevant)", 0);
            continue;
        }
        let _ = writeln!(
            s,
            "{c:<11}{:>4}  {:6.3} {:6.3} {:6.3} {:6.3}",
            m.n, m.r5, m.r10, m.mrr, m.hit1
        );
    }
    let _ = writeln!(
        s,
        "meaning-side hits on queries with nothing relevant: {}",
        negative_meaning_hits(rows)
    );
    s
}

#[test]
fn memory_search_quality_holds_on_the_labelled_corpus() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("corpus parses");
    let e = Engine::open_in_memory().expect("open in-memory store");
    let id2key = load(&e, &corpus);

    let hybrid = run(&e, &corpus, &id2key, Value::Null);
    let lexical = run(&e, &corpus, &id2key, json!("lexical"));
    let report = format!(
        "{}\n{}",
        render("hybrid (default mode)", &hybrid),
        render("lexical mode", &lexical)
    );

    let mut failures = Vec::new();
    let measured: HashMap<String, Metrics> = table(&hybrid).into_iter().collect();
    for (cat, floor) in FLOORS {
        let r10 = measured.get(*cat).map_or(0.0, |m| m.r10);
        if r10 < *floor {
            failures.push(format!(
                "hybrid {cat} R@10 {r10:.3} is under its floor {floor}"
            ));
        }
    }
    let noise = negative_meaning_hits(&hybrid);
    if noise > NEGATIVE_MEANING_HITS_CAP {
        failures.push(format!(
            "hybrid returned {noise} meaning-side hits on queries with nothing relevant, cap {NEGATIVE_MEANING_HITS_CAP}"
        ));
    }

    let got: Vec<Value> = lexical
        .iter()
        .map(|r| json!({ "q": r.query, "ranked": r.ranked }))
        .collect();
    if std::env::var_os("TASQX_EVAL_BLESS").is_some() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/memory-eval-lexical.json"
        );
        let mut text = serde_json::to_string_pretty(&got).unwrap();
        text.push('\n');
        std::fs::write(path, text).expect("write the lexical fixture");
    } else {
        let want: Vec<Value> = serde_json::from_str(LEXICAL).expect("lexical fixture parses");
        if want.len() != got.len() {
            failures.push(format!(
                "lexical fixture has {} queries, the corpus {}",
                want.len(),
                got.len()
            ));
        }
        for (w, g) in want.iter().zip(&got) {
            if w != g {
                failures.push(format!("lexical mode moved: want {w}, got {g}"));
            }
        }
    }

    assert!(failures.is_empty(), "{}\n\n{report}", failures.join("\n"));
    eprintln!("{report}");
}
