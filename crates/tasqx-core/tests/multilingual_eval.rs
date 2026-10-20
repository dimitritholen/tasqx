//! Recall@5 of the embedding model across English and Dutch (DESIGN.md D234).
//!
//! `fixtures/multilingual-eval.json` holds public tasqx text only: English
//! excerpts of DESIGN.md §12 and Dutch paraphrases of other §12 rulings,
//! written for the fixture. Each query expects exactly one doc. A doc is
//! embedded as memory is (`embed`), a query as search embeds one
//! (`embed_query`), and every doc is ranked against every query by cosine.
//! Recall@5 is reported per language pair, query language first.
//!
//! The floors are the ruling's targets, stated before the model swap against
//! potion-base-8M's baseline (en→en 1.00, nl→nl 0.60, en→nl 0.00,
//! nl→en 0.00): Dutch and both cross-lingual pairs at least 0.15 above it,
//! English at most 0.02 below it. Run with `--nocapture` to see the table.

use serde_json::Value;
use tasqx_core::embed::{cosine, embed, embed_query};

const FIXTURE: &str = include_str!("fixtures/multilingual-eval.json");

const FLOORS: &[(&str, f64)] = &[
    ("en->en", 0.98),
    ("nl->nl", 0.75),
    ("en->nl", 0.15),
    ("nl->en", 0.15),
];

#[test]
#[ignore = "a measurement: no candidate model meets these floors yet (task #2331); run with --ignored --nocapture"]
fn recall_at_5_holds_in_dutch_english_and_across_them() {
    let fx: Value = serde_json::from_str(FIXTURE).unwrap();
    let docs: Vec<(&str, &str, [f32; 256])> = fx["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let text = d["text"].as_str().unwrap();
            (
                d["key"].as_str().unwrap(),
                d["lang"].as_str().unwrap(),
                embed(text).unwrap_or_else(|| panic!("{text:?} has no vector")),
            )
        })
        .collect();
    let queries = fx["queries"].as_array().unwrap();
    assert!(queries.len() >= 30, "the ruling asks for at least 30 pairs");

    let mut pairs: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for q in queries {
        let text = q["text"].as_str().unwrap();
        let want = q["expect"].as_str().unwrap();
        let (_, doc_lang, _) = docs
            .iter()
            .find(|(k, _, _)| *k == want)
            .unwrap_or_else(|| panic!("{want} is not a doc"));
        let pair = format!("{}->{doc_lang}", q["lang"].as_str().unwrap());
        let v = embed_query(text).unwrap_or_else(|| panic!("{text:?} has no vector"));
        let mut ranked: Vec<(f32, &str)> =
            docs.iter().map(|(k, _, d)| (cosine(&v, d), *k)).collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(b.1)));
        let hit = ranked.iter().take(5).any(|(_, k)| *k == want);
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
    eprintln!("{report}");
    assert!(failures.is_empty(), "{}\n{report}", failures.join("\n"));
}
