//! D196's semantic index: reconcile, the guard, the lock, the cache, and
//! every write path leaving the index right by doing only its own SQL.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::*;
use crate::Engine;

const GIRAFFE: &str = "Giraffes graze on acacia leaves across the open savanna.";
const VOLCANO: &str = "Volcanic eruptions pour molten lava over the island's slopes.";
const ORCHARD: &str = "Apple orchards bloom in spring and are harvested in autumn.";

/// A directory under the system temp dir, removed on drop, holding one
/// store file: the tests that need two connections on one database.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tasqx-vectors-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn db(&self) -> String {
        self.0.join("tasks.db").to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Everything, with a floor only the text itself (or a near copy) clears.
fn everything() -> SemanticFilter<'static> {
    SemanticFilter {
        scope: "all",
        project: None,
        include_unscoped: false,
        exclude_task: None,
        min_similarity: 0.95,
        depth: 1000,
    }
}

/// The entries `query` finds at [`everything`]'s floor.
fn finds(e: &Engine, query: &str) -> Vec<(Kind, String)> {
    e.semantic_candidates(query, &everything())
        .hits
        .into_iter()
        .map(|h| (h.kind, h.owner_id))
        .collect()
}

fn found(e: &Engine, query: &str, kind: Kind, id: &str) -> bool {
    finds(e, query).contains(&(kind, id.to_string()))
}

fn rows(conn: &Connection, owner: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM memory_vectors WHERE owner_id = ?1 AND model = ?2",
        params![owner, MODEL_ID],
        |r| r.get(0),
    )
    .unwrap()
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_string()
}

fn add_doc(e: &Engine, title: &str, body: &str) -> String {
    id(&e
        .memory_add(&json!({ "title": title, "body": body }))
        .expect("memory.add"))
}

fn add_task(e: &Engine, title: &str) -> String {
    id(&e.task_add(&json!({ "title": title })).expect("task.add"))
}

fn add_note(e: &Engine, task: &str, body: &str) -> String {
    e.annotation_add(&json!({ "ref": task, "body": body }))
        .expect("annotation.add");
    note_id(
        &e.task_get(&json!({ "ref": task })).expect("task.get"),
        body,
    )
}

/// The id of the live note with `body` on the task `out` describes.
fn note_id(task: &Value, body: &str) -> String {
    task["annotations"]
        .as_array()
        .expect("annotations")
        .iter()
        .find(|a| a["body"] == body)
        .map(id)
        .unwrap_or_else(|| panic!("no note {body:?} in {task}"))
}

/// Asserts that `new` finds the entry and `old` no longer does.
fn moved(e: &Engine, kind: Kind, owner: &str, old: &str, new: &str) {
    assert!(found(e, new, kind, owner), "the new text must find {owner}");
    assert!(
        !found(e, old, kind, owner),
        "the old text must no longer find {owner}"
    );
}

// ---- reconcile ----------------------------------------------------------------

#[test]
fn a_search_embeds_what_has_no_row_and_a_second_search_writes_nothing() {
    let e = Engine::open_in_memory().unwrap();
    let doc = add_doc(&e, "Wildlife", GIRAFFE);
    let task = add_task(&e, "field trip");
    let note = add_note(&e, &task, VOLCANO);
    assert_eq!(rows(&e.conn, &doc), 0, "an insert stores no vector");

    assert_eq!(finds(&e, GIRAFFE), [(Kind::Doc, doc.clone())]);
    assert!(rows(&e.conn, &doc) >= 1, "the doc's chunks are stored");
    assert_eq!(rows(&e.conn, &note), 1, "a short note is one chunk");

    let used: i64 = e
        .conn
        .query_row(
            "SELECT last_used FROM memory_vector_models WHERE model = ?1",
            params![MODEL_ID],
            |r| r.get(0),
        )
        .expect("the search recorded its model");
    assert!(crate::clock::now().as_second() - used < 60);

    let before = e.conn.total_changes();
    assert_eq!(finds(&e, VOLCANO), [(Kind::Annotation, note)]);
    assert_eq!(
        e.conn.total_changes(),
        before,
        "nothing is embedded or written twice"
    );
}

#[test]
fn text_with_no_known_token_is_stored_once_as_a_sentinel() {
    assert!(
        embed::embed("🙂🙂").is_none(),
        "precondition: no known token"
    );
    let e = Engine::open_in_memory().unwrap();
    let doc = add_doc(&e, "🙂", "🙂🙂");
    let task = add_task(&e, "t");
    let note = add_note(&e, &task, "🙂🙂");
    assert!(finds(&e, GIRAFFE).is_empty());
    for owner in [&doc, &note] {
        let (chunk, vec): (i64, Option<Vec<u8>>) = e
            .conn
            .query_row(
                "SELECT chunk, vec FROM memory_vectors WHERE owner_id = ?1",
                params![owner],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((chunk, vec), (-1, None), "one sentinel row for {owner}");
    }
    let before = e.conn.total_changes();
    // A different engine on the same store would also skip them: the scan
    // itself no longer lists them.
    assert!(scan_missing(&e.conn, true).unwrap().is_empty());
    finds(&e, VOLCANO);
    assert_eq!(e.conn.total_changes(), before, "never re-embedded");
}

#[test]
fn a_removed_or_empty_note_is_not_embedded() {
    let e = Engine::open_in_memory().unwrap();
    let task = add_task(&e, "t");
    let note = add_note(&e, &task, GIRAFFE);
    e.annotation_remove(&json!({ "ref": task, "annotation_id": note }))
        .unwrap();
    assert!(finds(&e, GIRAFFE).is_empty());
    assert_eq!(rows(&e.conn, &note), 0, "no row, not even a sentinel");
}

/// Another model's rows are never read, and never deleted by a search or
/// by an open while that model is in use: a CLI and an MCP server built
/// with different models share a store without deleting each other's work.
/// Once the other model has not searched for over seven days, open sweeps
/// it.
#[test]
fn another_model_s_rows_are_ignored_and_swept_only_once_unused() {
    let s = Scratch::new("model");
    let e = Engine::open(&s.db()).unwrap();
    let doc = add_doc(&e, "Geology", VOLCANO);
    // A row under another model that would match GIRAFFE exactly.
    let giraffe = embed::quantize(&embed::embed(GIRAFFE).unwrap());
    let now = crate::clock::now().as_second();
    e.conn
        .execute(
            "INSERT INTO memory_vectors (kind, owner_id, model, chunk, vec) \
             VALUES ('doc', ?1, 'another-model', 0, ?2)",
            params![doc, &giraffe[..]],
        )
        .unwrap();
    e.conn
        .execute(
            "INSERT INTO memory_vector_models (model, last_used) VALUES ('another-model', ?1)",
            params![now],
        )
        .unwrap();
    assert!(finds(&e, GIRAFFE).is_empty(), "another model is never read");
    assert!(
        found(&e, VOLCANO, Kind::Doc, &doc),
        "and does not count as ours"
    );
    let others = |c: &Connection| -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE model = 'another-model'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(others(&e.conn), 1, "a search deletes nobody's rows");
    drop(e);
    for _ in 0..2 {
        let reopened = Engine::open(&s.db()).unwrap();
        assert_eq!(
            others(&reopened.conn),
            1,
            "nor does an open, while it is in use"
        );
    }
    let e = Engine::open(&s.db()).unwrap();
    e.conn
        .execute(
            "UPDATE memory_vector_models SET last_used = ?1 WHERE model = 'another-model'",
            params![now - 8 * 24 * 3600],
        )
        .unwrap();
    drop(e);
    let reopened = Engine::open(&s.db()).unwrap();
    assert_eq!(others(&reopened.conn), 0, "unused for eight days: swept");
    assert!(rows(&reopened.conn, &doc) >= 1, "and ours kept");
}

// ---- the guard, the lock, a read-only file ----------------------------------------

/// The race D196 closes: this engine reads v1 and embeds it, another
/// connection commits v2, and only then does this one write. The guard
/// finds the text changed and inserts nothing, so no vector describes text
/// the row no longer holds, and the next search embeds v2.
#[test]
fn a_write_that_lost_a_race_with_an_edit_stores_nothing() {
    let s = Scratch::new("race");
    let a = Engine::open(&s.db()).unwrap();
    let b = Engine::open(&s.db()).unwrap();
    let doc = add_doc(&a, "Notes", GIRAFFE);
    let task = add_task(&a, "t");
    let note = add_note(&a, &task, GIRAFFE);

    let pending = scan_missing(&a.conn, true).unwrap();
    assert_eq!(pending.len(), 2, "both read at v1");
    b.memory_update(&json!({ "id": doc, "body": VOLCANO }))
        .unwrap();
    b.annotation_update(&json!({ "ref": task, "annotation_id": note, "body": VOLCANO }))
        .unwrap();

    assert_eq!(
        persist(&a.conn, &pending, None).unwrap(),
        0,
        "the guard refused both"
    );
    assert_eq!(rows(&a.conn, &doc), 0, "no v1 vector for the doc");
    assert_eq!(rows(&a.conn, &note), 0, "no v1 vector for the note");

    moved(&a, Kind::Doc, &doc, GIRAFFE, VOLCANO);
    moved(&a, Kind::Annotation, &note, GIRAFFE, VOLCANO);
    assert!(
        rows(&a.conn, &doc) >= 1 && rows(&a.conn, &note) == 1,
        "v2 stored"
    );
}

/// The same race for a note removed in between: the guard's
/// `removed IS NULL` refuses it.
#[test]
fn a_note_removed_between_read_and_write_stores_nothing() {
    let s = Scratch::new("race-rm");
    let a = Engine::open(&s.db()).unwrap();
    let b = Engine::open(&s.db()).unwrap();
    let task = add_task(&a, "t");
    let note = add_note(&a, &task, GIRAFFE);
    let pending = scan_missing(&a.conn, true).unwrap();
    b.annotation_remove(&json!({ "ref": task, "annotation_id": note }))
        .unwrap();
    assert_eq!(persist(&a.conn, &pending, None).unwrap(), 0);
    assert_eq!(rows(&a.conn, &note), 0);
    assert!(!found(&a, GIRAFFE, Kind::Annotation, &note));
}

/// Another writer holds the lock: the search does not wait out the busy
/// timeout, still answers from vectors it computed for the call, stores
/// nothing, and leaves the connection's timeout as it found it.
#[test]
fn a_held_write_lock_neither_blocks_nor_breaks_a_search() {
    let s = Scratch::new("busy");
    let a = Engine::open(&s.db()).unwrap();
    let doc = add_doc(&a, "Wildlife", GIRAFFE);
    let holder = Connection::open(s.db()).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = Instant::now();
    assert!(found(&a, GIRAFFE, Kind::Doc, &doc), "answered from memory");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "did not wait on the lock: {:?}",
        started.elapsed()
    );
    let timeout: i64 = a
        .conn
        .pragma_query_value(None, "busy_timeout", |r| r.get(0))
        .unwrap();
    assert_eq!(timeout, 3000, "the busy timeout is restored");
    assert_eq!(rows(&holder, &doc), 0, "nothing stored under the lock");

    let embedded = a.vectors.borrow().embedded;
    assert!(found(&a, GIRAFFE, Kind::Doc, &doc), "still answered");
    assert_eq!(
        a.vectors.borrow().embedded,
        embedded,
        "a second search under the lock embeds nothing again"
    );

    holder.execute_batch("ROLLBACK").unwrap();
    assert!(found(&a, GIRAFFE, Kind::Doc, &doc));
    assert!(rows(&a.conn, &doc) >= 1, "stored once the lock is free");
    assert_eq!(a.vectors.borrow().embedded, embedded, "from what it kept");
}

#[test]
fn a_read_only_store_still_answers() {
    let s = Scratch::new("ro");
    let (doc, note) = {
        let w = Engine::open(&s.db()).unwrap();
        let doc = add_doc(&w, "Wildlife", GIRAFFE);
        let task = add_task(&w, "t");
        (doc, add_note(&w, &task, VOLCANO))
    };
    let r = Engine::open_read_only(&s.db()).unwrap();
    assert!(found(&r, GIRAFFE, Kind::Doc, &doc));
    assert!(found(&r, VOLCANO, Kind::Annotation, &note));
    assert_eq!(rows(&r.conn, &doc), 0, "nothing written");
    // Twice: the cache is not marked complete by what it could not store.
    assert!(found(&r, GIRAFFE, Kind::Doc, &doc));
}

/// A read-only open runs no migration, so a store an older binary wrote
/// has no `memory_vectors` at all. Search still works, from memory.
#[test]
fn a_read_only_store_without_the_table_still_answers() {
    let s = Scratch::new("ro-legacy");
    let doc = {
        let w = Engine::open(&s.db()).unwrap();
        let doc = add_doc(&w, "Wildlife", GIRAFFE);
        w.conn
            .execute_batch(
                "DROP TRIGGER memory_vectors_docs_au; DROP TRIGGER memory_vectors_docs_ad; \
                 DROP TRIGGER memory_vectors_annotations_au; \
                 DROP TRIGGER memory_vectors_annotations_ad; DROP TABLE memory_vectors;",
            )
            .unwrap();
        doc
    };
    let r = Engine::open_read_only(&s.db()).unwrap();
    assert!(found(&r, GIRAFFE, Kind::Doc, &doc));
}

/// A search from inside a transaction the caller owns stores nothing and
/// does not disturb the transaction.
#[test]
fn a_search_inside_an_open_transaction_does_not_write() {
    let e = Engine::open_in_memory().unwrap();
    let doc = add_doc(&e, "Wildlife", GIRAFFE);
    e.conn.execute_batch("BEGIN").unwrap();
    assert!(found(&e, GIRAFFE, Kind::Doc, &doc));
    assert!(!e.conn.is_autocommit(), "still the caller's transaction");
    assert_eq!(rows(&e.conn, &doc), 0);
    e.conn.execute_batch("COMMIT").unwrap();
}

// ---- the per-process copy ------------------------------------------------------------

/// Two engines on one file: each sees the other's commits (`data_version`)
/// and its own (`total_changes`) on the next search after a warm one.
#[test]
fn the_cache_follows_other_connections_and_its_own_writes() {
    let s = Scratch::new("cache");
    let a = Engine::open(&s.db()).unwrap();
    let b = Engine::open(&s.db()).unwrap();
    let first = add_doc(&a, "Wildlife", GIRAFFE);
    assert!(found(&a, GIRAFFE, Kind::Doc, &first), "warm");

    let second = add_doc(&b, "Geology", VOLCANO);
    assert!(
        found(&a, VOLCANO, Kind::Doc, &second),
        "another connection's add"
    );

    // B embeds the edit itself this time, so A only reloads.
    b.memory_update(&json!({ "id": first, "body": ORCHARD }))
        .unwrap();
    assert!(found(&b, ORCHARD, Kind::Doc, &first));
    moved(&a, Kind::Doc, &first, GIRAFFE, ORCHARD);

    // A's own write, which A's data_version does not see.
    a.memory_remove(&json!({ "id": second })).unwrap();
    assert!(!found(&a, VOLCANO, Kind::Doc, &second), "own remove");
    let third = add_doc(&a, "Wildlife", GIRAFFE);
    assert!(found(&a, GIRAFFE, Kind::Doc, &third), "own add");

    // And B, warm since before those, sees A's.
    assert!(!found(&b, VOLCANO, Kind::Doc, &second));
    assert!(found(&b, GIRAFFE, Kind::Doc, &third));
}

/// The enum orders as its words do, which is the `(kind, id)` tie-break.
#[test]
fn kinds_order_as_their_names() {
    assert!(Kind::Annotation < Kind::Doc);
    assert!(Kind::Annotation.as_str() < Kind::Doc.as_str());
}

/// The copy is keyed on memory's generation, so writes a semantic list
/// cannot see — a timer, a tag, a task's title, a token count — leave it
/// warm, and a task's project, which scopes its notes, does not.
#[test]
fn the_cache_survives_writes_memory_cannot_see() {
    let e = Engine::open_in_memory().unwrap();
    e.project_create(&json!({ "name": "zoo" })).unwrap();
    e.project_create(&json!({ "name": "farm" })).unwrap();
    let task = id(&e
        .task_add(&json!({ "title": "t", "project": "zoo" }))
        .unwrap());
    let note = add_note(&e, &task, GIRAFFE);
    let zoo = SemanticFilter {
        project: Some("zoo"),
        ..everything()
    };
    let in_zoo = |e: &Engine| {
        e.semantic_candidates(GIRAFFE, &zoo)
            .hits
            .iter()
            .any(|h| h.owner_id == note)
    };
    assert!(in_zoo(&e));
    let loads = e.vectors.borrow().loads;

    e.task_add(&json!({ "title": "another" })).unwrap();
    e.task_start(&json!({ "ref": task })).unwrap();
    e.task_stop(&json!({ "ref": task })).unwrap();
    e.tag_add(&json!({ "ref": task, "tags": ["x"] })).unwrap();
    e.task_modify(&json!({ "ref": task, "set": { "title": "renamed" } }))
        .unwrap();
    e.token_add(&json!({
        "ref": task, "tool": "claude_code", "source": "self-report", "input_tokens": 5,
        "confidence": "medium"
    }))
    .unwrap();
    assert!(in_zoo(&e));
    assert_eq!(e.vectors.borrow().loads, loads, "no reload");

    e.task_modify(&json!({ "ref": task, "set": { "project": "farm" } }))
        .unwrap();
    assert!(!in_zoo(&e), "the note follows its task's project");
    assert_eq!(e.vectors.borrow().loads, loads + 1);
}

/// Opening one store from many processes at once: the vector migration
/// takes the write lock for its checks and its DDL together, so no open
/// fails on another's half-done work.
#[test]
fn concurrent_opens_of_one_store_all_succeed() {
    for round in 0..3 {
        let s = Scratch::new(&format!("open-{round}"));
        if round > 0 {
            // A store that predates the index, upgraded by every opener.
            let e = Engine::open(&s.db()).unwrap();
            e.conn
                .execute_batch(
                    "DROP TRIGGER memory_vectors_docs_au; DROP TABLE memory_vectors; \
                     DROP TABLE memory_vector_models;",
                )
                .unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (db, barrier) = (s.db(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    Engine::open(&db).map(|_| ())
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("every concurrent open succeeds");
        }
    }
}

// ---- the retrieval primitive -----------------------------------------------------------

#[test]
fn the_filter_matches_the_lexical_arms() {
    let e = Engine::open_in_memory().unwrap();
    // Before any project exists, so it inherits no default project.
    let loose_task = add_task(&e, "lt");
    e.project_create(&json!({ "name": "zoo" })).unwrap();
    e.project_create(&json!({ "name": "farm" })).unwrap();
    let zoo_doc = id(&e
        .memory_add(&json!({ "title": "Z", "body": GIRAFFE, "project": "zoo" }))
        .unwrap());
    let farm_doc = id(&e
        .memory_add(&json!({ "title": "F", "body": GIRAFFE, "project": "farm" }))
        .unwrap());
    let global_doc = add_doc(&e, "G", GIRAFFE);
    let zoo_task = id(&e
        .task_add(&json!({ "title": "zt", "project": "zoo" }))
        .unwrap());
    let zoo_note = add_note(&e, &zoo_task, GIRAFFE);
    let loose_note = add_note(&e, &loose_task, GIRAFFE);

    let ids = |f: SemanticFilter<'_>| -> Vec<String> {
        let mut v: Vec<String> = e
            .semantic_candidates(GIRAFFE, &f)
            .hits
            .into_iter()
            .map(|h| h.owner_id)
            .collect();
        v.sort();
        v
    };
    let sorted = |mut v: Vec<&String>| -> Vec<String> {
        v.sort();
        v.into_iter().cloned().collect()
    };
    let base = everything();
    assert_eq!(
        ids(base.clone()),
        sorted(vec![
            &zoo_doc,
            &farm_doc,
            &global_doc,
            &zoo_note,
            &loose_note
        ])
    );
    assert_eq!(
        ids(SemanticFilter {
            scope: "docs",
            ..base.clone()
        }),
        sorted(vec![&zoo_doc, &farm_doc, &global_doc])
    );
    assert_eq!(
        ids(SemanticFilter {
            scope: "annotations",
            ..base.clone()
        }),
        sorted(vec![&zoo_note, &loose_note])
    );
    assert_eq!(
        ids(SemanticFilter {
            project: Some("zoo"),
            ..base.clone()
        }),
        sorted(vec![&zoo_doc, &zoo_note]),
        "a note's project is its task's"
    );
    assert_eq!(
        ids(SemanticFilter {
            project: Some("zoo"),
            include_unscoped: true,
            ..base.clone()
        }),
        sorted(vec![&zoo_doc, &global_doc, &zoo_note, &loose_note]),
        "widened to entries with no project, never to another project's"
    );
    assert_eq!(
        ids(SemanticFilter {
            exclude_task: Some(&zoo_task),
            ..base.clone()
        }),
        sorted(vec![&zoo_doc, &farm_doc, &global_doc, &loose_note]),
        "the task's own notes go"
    );
}

#[test]
fn hits_are_ordered_floored_rounded_and_cut_to_depth() {
    let e = Engine::open_in_memory().unwrap();
    let a = add_doc(&e, "A", GIRAFFE);
    let b = add_doc(&e, "B", GIRAFFE);
    let task = add_task(&e, "t");
    let n = add_note(&e, &task, GIRAFFE);
    let far = add_doc(&e, "Geology", VOLCANO);
    let mut f = SemanticFilter {
        min_similarity: 0.0,
        ..everything()
    };
    let all = e.semantic_candidates(GIRAFFE, &f);
    for w in all.hits.windows(2) {
        assert!(
            w[0].similarity > w[1].similarity
                || (w[0].similarity == w[1].similarity
                    && (w[0].kind, &w[0].owner_id) < (w[1].kind, &w[1].owner_id)),
            "best first, then (kind, id): {:?}",
            all.hits
        );
    }
    for h in &all.hits {
        assert_eq!(h.similarity, embed::round_similarity(h.similarity as f32));
        assert_eq!((h.similarity * 1000.0).round() / 1000.0, h.similarity);
    }
    assert_eq!(all.total, all.hits.len());
    let far_sim = all
        .hits
        .iter()
        .find(|h| h.owner_id == far)
        .expect("a zero floor admits everything")
        .similarity;

    f.min_similarity = far_sim + 0.001;
    let floored = e.semantic_candidates(GIRAFFE, &f);
    assert!(
        floored.hits.iter().all(|h| h.owner_id != far),
        "below the floor"
    );
    f.min_similarity = far_sim;
    let at = e.semantic_candidates(GIRAFFE, &f);
    assert!(
        at.hits.iter().any(|h| h.owner_id == far),
        "at the floor is in"
    );

    f.depth = 2;
    f.min_similarity = 0.9;
    let cut = e.semantic_candidates(GIRAFFE, &f);
    assert_eq!(cut.total, 3, "total counts past the depth");
    assert_eq!(cut.hits.len(), 2);
    // The three copies tie; the note sorts first by kind, then docs by id.
    let mut docs = [a, b];
    docs.sort();
    assert_eq!(
        cut.hits
            .iter()
            .map(|h| h.owner_id.clone())
            .collect::<Vec<_>>(),
        [n, docs[0].clone()]
    );
}

#[test]
fn a_query_with_no_known_token_finds_nothing() {
    let e = Engine::open_in_memory().unwrap();
    add_doc(&e, "Wildlife", GIRAFFE);
    let f = SemanticFilter {
        min_similarity: 0.0,
        ..everything()
    };
    assert_eq!(e.semantic_candidates("🙂", &f), SemanticHits::default());
}

#[test]
fn the_snippet_is_the_best_chunk_cut_again_from_the_row() {
    let e = Engine::open_in_memory().unwrap();
    let body = format!("# Animals\n\n{GIRAFFE}\n\n# Rocks\n\n{VOLCANO}");
    let doc = add_doc(&e, "Field guide", &body);
    let hit = e
        .semantic_candidates(
            VOLCANO,
            &SemanticFilter {
                min_similarity: 0.0,
                ..everything()
            },
        )
        .hits
        .into_iter()
        .find(|h| h.owner_id == doc)
        .unwrap();
    let chunks = embed::chunk::chunk_doc("Field guide", &body);
    assert!(!chunks.is_empty());
    let snippet = e.semantic_snippet(Kind::Doc, &doc, hit.chunk).unwrap();
    assert_eq!(Some(&snippet), chunks.get(hit.chunk as usize));
    assert!(snippet.contains("Volcanic"), "{snippet}");

    let task = add_task(&e, "t");
    let note = add_note(&e, &task, GIRAFFE);
    assert_eq!(
        e.semantic_snippet(Kind::Annotation, &note, 0).as_deref(),
        Some(GIRAFFE)
    );
    assert_eq!(e.semantic_snippet(Kind::Annotation, &note, 1), None);
    assert_eq!(e.semantic_snippet(Kind::Doc, &note, 0), None);
}

// ---- every write path --------------------------------------------------------------

#[test]
fn memory_add_update_and_remove_leave_the_index_right() {
    let e = Engine::open_in_memory().unwrap();
    let doc = add_doc(&e, "Notes", GIRAFFE);
    assert!(found(&e, GIRAFFE, Kind::Doc, &doc));
    e.memory_update(&json!({ "id": doc, "body": VOLCANO }))
        .unwrap();
    moved(&e, Kind::Doc, &doc, GIRAFFE, VOLCANO);
    // A title-only edit re-embeds too: every chunk carries the title.
    e.memory_update(&json!({ "id": doc, "title": ORCHARD }))
        .unwrap();
    assert_eq!(
        rows(&e.conn, &doc),
        0,
        "the title is embedded text, so its change drops the vectors"
    );
    finds(&e, VOLCANO);
    assert!(
        rows(&e.conn, &doc) >= 1,
        "and the next search embeds it again"
    );
    e.memory_remove(&json!({ "id": doc })).unwrap();
    assert!(finds(&e, VOLCANO).is_empty());
    assert_eq!(rows(&e.conn, &doc), 0);
}

#[test]
fn an_update_that_changes_nothing_embedded_keeps_the_vectors() {
    let e = Engine::open_in_memory().unwrap();
    e.project_create(&json!({ "name": "zoo" })).unwrap();
    let doc = add_doc(&e, "Notes", GIRAFFE);
    finds(&e, GIRAFFE);
    let before = rows(&e.conn, &doc);
    e.memory_update(&json!({ "id": doc, "project": "zoo", "standing": true }))
        .unwrap();
    assert_eq!(
        rows(&e.conn, &doc),
        before,
        "project and standing are not text"
    );

    let import = || {
        id(&e
            .memory_import(&json!({ "docs": [{
                "title": "n.md", "body": VOLCANO, "source": "docs/n.md"
            }]}))
            .unwrap()["docs"][0])
    };
    let imported = import();
    finds(&e, VOLCANO);
    let before = rows(&e.conn, &imported);
    assert!(before >= 1);
    assert_eq!(import(), imported);
    assert_eq!(
        rows(&e.conn, &imported),
        before,
        "an identical re-import keeps them"
    );
}

#[test]
fn memory_import_and_refresh_leave_the_index_right() {
    let s = Scratch::new("import");
    let e = Engine::open_in_memory().unwrap();
    let path = s.0.join("a.md");
    let import = |text: &str| -> String {
        std::fs::write(&path, text).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        id(&e
            .memory_import(&json!({ "docs": [{
                "title": "a.md",
                "body": text,
                "source": "docs/a.md",
                "origin_path": path.to_string_lossy(),
                "origin_mtime": crate::memory_doc::unix_seconds(&meta).unwrap(),
                "origin_size": meta.len(),
            }]}))
            .unwrap()["docs"][0])
    };
    let doc = import(GIRAFFE);
    assert!(found(&e, GIRAFFE, Kind::Doc, &doc));
    assert_eq!(import(VOLCANO), doc, "a re-import is the same doc");
    moved(&e, Kind::Doc, &doc, GIRAFFE, VOLCANO);

    // The file changes under the store; refresh re-reads it. A longer text
    // so the size differs whatever the mtime's resolution.
    let longer = format!("{ORCHARD} {ORCHARD}");
    std::fs::write(&path, &longer).unwrap();
    let out = e.memory_refresh(&json!({})).unwrap();
    assert_eq!(out["refreshed"].as_array().map(Vec::len), Some(1), "{out}");
    moved(&e, Kind::Doc, &doc, VOLCANO, &longer);
}

#[test]
fn annotation_add_update_and_remove_leave_the_index_right() {
    let e = Engine::open_in_memory().unwrap();
    let task = add_task(&e, "t");
    let note = add_note(&e, &task, GIRAFFE);
    assert!(found(&e, GIRAFFE, Kind::Annotation, &note));
    e.annotation_update(&json!({ "ref": task, "annotation_id": note, "body": VOLCANO }))
        .unwrap();
    moved(&e, Kind::Annotation, &note, GIRAFFE, VOLCANO);
    e.annotation_remove(&json!({ "ref": task, "annotation_id": note }))
        .unwrap();
    assert!(!found(&e, VOLCANO, Kind::Annotation, &note));
    assert_eq!(rows(&e.conn, &note), 0);
}

#[test]
fn undoing_a_note_operation_leaves_the_index_right() {
    let e = Engine::open_in_memory().unwrap();
    let task = add_task(&e, "t");
    let note = add_note(&e, &task, GIRAFFE);

    e.annotation_update(&json!({ "ref": task, "annotation_id": note, "body": VOLCANO }))
        .unwrap();
    assert!(found(&e, VOLCANO, Kind::Annotation, &note));
    e.event_revert().expect("undo the update");
    moved(&e, Kind::Annotation, &note, VOLCANO, GIRAFFE);

    // `annotation.remove` is not undoable (D113): nothing to restore.
    let second = add_note(&e, &task, ORCHARD);
    assert!(found(&e, ORCHARD, Kind::Annotation, &second));
    e.event_revert().expect("undo the add");
    assert!(!found(&e, ORCHARD, Kind::Annotation, &second), "gone again");
}

/// `store.import` replaces a task's notes by deleting and re-inserting them
/// and updates a doc in place; both modes leave the index right.
#[test]
fn store_import_leaves_the_index_right_in_both_modes() {
    for merge in [false, true] {
        let from = Engine::open_in_memory().unwrap();
        let task = add_task(&from, "t");
        let note = add_note(&from, &task, GIRAFFE);
        let doc = add_doc(&from, "Notes", GIRAFFE);
        let into = Engine::open_in_memory().unwrap();
        into.store_import(&from.store_export(&json!({})).unwrap())
            .unwrap();
        assert!(
            found(&into, GIRAFFE, Kind::Annotation, &note),
            "merge={merge}"
        );
        assert!(found(&into, GIRAFFE, Kind::Doc, &doc), "merge={merge}");

        from.annotation_update(&json!({ "ref": task, "annotation_id": note, "body": VOLCANO }))
            .unwrap();
        from.memory_update(&json!({ "id": doc, "body": VOLCANO }))
            .unwrap();
        let fresh_task = add_task(&from, "u");
        let fresh_note = add_note(&from, &fresh_task, ORCHARD);
        let mut export = from.store_export(&json!({})).unwrap();
        export["merge"] = json!(merge);
        into.store_import(&export).unwrap();

        moved(&into, Kind::Annotation, &note, GIRAFFE, VOLCANO);
        moved(&into, Kind::Doc, &doc, GIRAFFE, VOLCANO);
        assert!(
            found(&into, ORCHARD, Kind::Annotation, &fresh_note),
            "merge={merge}"
        );
    }
}

/// D191's fold rekeys the dropped copy's notes onto the survivor: the old
/// id's vectors go with the old id, and the note is found under its new one.
#[test]
fn the_recurrence_fold_s_rekey_leaves_the_index_right() {
    let b = Engine::open_in_memory().unwrap();
    let r = id(&b
        .task_add(&json!({
            "title": "R", "recurrence": "every 3 days", "due": "2026-09-25T00:00:00Z"
        }))
        .unwrap());
    let a = Engine::open_in_memory().unwrap();
    a.store_import(&b.store_export(&json!({})).unwrap())
        .unwrap();
    // A's spawn first, so A's copy is the older id and survives.
    a.task_done(&json!({ "ref": r })).unwrap();
    b.task_done(&json!({ "ref": r })).unwrap();
    let spawned = |e: &Engine| -> String {
        e.store_export(&json!({})).unwrap()["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["spawned_from"] == json!(r))
            .map(id)
            .unwrap()
    };
    let dropped = spawned(&b);
    let old_note = add_note(&b, &dropped, GIRAFFE);
    assert!(found(&b, GIRAFFE, Kind::Annotation, &old_note));
    let kept = spawned(&a);
    assert!(kept < dropped, "precondition: A's copy survives");
    let mut export = b.store_export(&json!({})).unwrap();
    export["merge"] = json!(true);
    let out = a.store_import(&export).unwrap();
    assert_eq!(
        out["deduplicated"].as_array().map(Vec::len),
        Some(1),
        "{out}"
    );

    let survivor = a.task_get(&json!({ "ref": kept })).unwrap();
    let new_note = note_id(&survivor, GIRAFFE);
    assert_ne!(new_note, old_note, "the fold rekeyed it");
    assert_eq!(finds(&a, GIRAFFE), [(Kind::Annotation, new_note)]);
    assert_eq!(rows(&a.conn, &old_note), 0);
}

/// The rekey itself, on rows the index already holds: an `UPDATE` of the
/// id drops the old id's vectors, and the next search embeds the new id.
#[test]
fn a_rekeyed_note_is_found_under_its_new_id_only() {
    let e = Engine::open_in_memory().unwrap();
    let task = add_task(&e, "t");
    let old = add_note(&e, &task, GIRAFFE);
    assert!(found(&e, GIRAFFE, Kind::Annotation, &old));
    let new = crate::clock::uuid_v7().to_string();
    e.conn
        .execute(
            "UPDATE annotations SET id = ?1 WHERE id = ?2",
            params![new, old],
        )
        .unwrap();
    assert_eq!(finds(&e, GIRAFFE), [(Kind::Annotation, new)]);
    assert_eq!(rows(&e.conn, &old), 0);
}

/// A recurrence spawn copies the oldest live note onto the next occurrence
/// as a new row: the copy is found under its own id.
#[test]
fn a_recurrence_spawn_s_copied_note_is_found() {
    let e = Engine::open_in_memory().unwrap();
    let r = id(&e
        .task_add(&json!({
            "title": "R", "recurrence": "every 3 days", "due": "2026-09-25T00:00:00Z"
        }))
        .unwrap());
    let original = add_note(&e, &r, GIRAFFE);
    assert_eq!(finds(&e, GIRAFFE), [(Kind::Annotation, original.clone())]);
    let done = e.task_done(&json!({ "ref": r })).unwrap();
    let next = done["spawned"]["id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            e.store_export(&json!({})).unwrap()["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["spawned_from"] == json!(r))
                .map(id)
                .unwrap()
        });
    let copy = note_id(&e.task_get(&json!({ "ref": next })).unwrap(), GIRAFFE);
    let hits = finds(&e, GIRAFFE);
    assert!(hits.contains(&(Kind::Annotation, original)), "{hits:?}");
    assert!(hits.contains(&(Kind::Annotation, copy)), "{hits:?}");
}

#[test]
fn store_export_carries_no_vectors() {
    let e = Engine::open_in_memory().unwrap();
    add_doc(&e, "Wildlife", GIRAFFE);
    let task = add_task(&e, "t");
    add_note(&e, &task, VOLCANO);
    finds(&e, GIRAFFE);
    let export = e.store_export(&json!({})).unwrap();
    let text = export.to_string();
    assert!(
        !text.contains("memory_vectors") && !text.contains(MODEL_ID),
        "{text}"
    );
    for (key, _) in export.as_object().unwrap() {
        assert!(!key.contains("vector"), "{key}");
    }
    // And an import of it builds the vectors afresh.
    let into = Engine::open_in_memory().unwrap();
    into.store_import(&export).unwrap();
    assert!(!finds(&into, GIRAFFE).is_empty());
}

// ---- cost ------------------------------------------------------------------------

/// D196's budget: warm under 50 ms and cold under 1 s at 50,000 entries, in
/// a release build, measured twice: 50,000 single-chunk entries (half docs,
/// half notes), and 50,000 docs of three sections each. Ignored by default
/// (it embeds every text); run with
/// `cargo test --release -p tasqx-core --lib vectors::tests::cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn cost_at_fifty_thousand_entries() {
    let words = [
        "release",
        "deploy",
        "sign-in",
        "token",
        "cache",
        "schema",
        "migration",
        "review",
        "budget",
        "memory",
        "search",
        "index",
        "vector",
        "daemon",
        "socket",
        "timer",
    ];
    let prose = |i: usize, n: usize| -> String {
        let text: Vec<&str> = (0..n)
            .map(|k| words[(i * 7 + k * 13) % words.len()])
            .collect();
        format!("Entry {i}: {}.", text.join(" "))
    };
    let single = |i: usize| (i % 2 == 1, prose(i, 40));
    let sections = |i: usize| {
        (
            false,
            format!(
                "# One\n\n{}\n\n# Two\n\n{}\n\n# Three\n\n{}",
                prose(i, 80),
                prose(i + 1, 80),
                prose(i + 2, 80)
            ),
        )
    };
    measure("50,000 single-chunk entries", &single);
    measure("50,000 docs of three sections", &sections);
}

/// Builds 50,000 entries from `make` (`(is_note, text)`) and prints the
/// search timings.
fn measure(label: &str, make: &dyn Fn(usize) -> (bool, String)) {
    let s = Scratch::new("cost");
    let e = Engine::open(&s.db()).unwrap();
    let tx = e.conn.unchecked_transaction().unwrap();
    tx.execute(
        "INSERT INTO tasks (id, short_id, title, status, created, modified) \
         VALUES ('t', 1000000, 'T', 'pending', 't', 't')",
        [],
    )
    .unwrap();
    for i in 0..50_000usize {
        let (note, text) = make(i);
        if note {
            tx.execute(
                "INSERT INTO annotations (id, task_id, body, created) VALUES (?1, 't', ?2, 't')",
                params![format!("a{i}"), text],
            )
            .unwrap();
        } else {
            tx.execute(
                "INSERT INTO docs (id, title, body, search_body, created, modified) \
                 VALUES (?1, ?2, ?3, ?3, 't', 't')",
                params![format!("d{i}"), format!("Doc {i}"), text],
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
    let chunks: i64 = {
        let q = "release deploy review budget";
        let f = SemanticFilter {
            min_similarity: 0.3,
            depth: 50,
            ..everything()
        };
        let t = Instant::now();
        let first = e.semantic_candidates(q, &f);
        println!(
            "{label}: first search, embedding everything: {:?}",
            t.elapsed()
        );
        assert!(first.total > 0);
        let warm: Vec<Duration> = (0..5)
            .map(|_| {
                let t = Instant::now();
                e.semantic_candidates(q, &f);
                t.elapsed()
            })
            .collect();
        println!("{label}: warm, five runs: {warm:?}");
        let t = Instant::now();
        e.task_add(&json!({ "title": "x" })).unwrap();
        e.semantic_candidates(q, &f);
        println!("{label}: after a task.add (no reload): {:?}", t.elapsed());
        e.conn
            .query_row("SELECT COUNT(*) FROM memory_vectors", [], |r| r.get(0))
            .unwrap()
    };
    drop(e);
    let e = Engine::open(&s.db()).unwrap();
    let f = SemanticFilter {
        min_similarity: 0.3,
        depth: 50,
        ..everything()
    };
    let t = Instant::now();
    e.semantic_candidates("release deploy review budget", &f);
    println!(
        "{label}: cold (new engine, {chunks} vectors stored): {:?}",
        t.elapsed()
    );
}
