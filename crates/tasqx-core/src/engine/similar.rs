//! D237: what a new task or doc looks like that is already there.
//!
//! `task.add` and `memory.add` answer `similar`, always present: up to
//! [`MAX`] open tasks or live docs whose similarity to what is being written
//! is at or above [`TASK_THRESHOLD`] or [`DOC_THRESHOLD`], best first. A
//! warning, never a refusal, and computed before the add takes its write
//! lock; a panic while embedding answers an empty list.
//!
//! Tasks keep no stored vectors (D237): the open tasks of the project are
//! embedded on demand and kept in this process only ([`SimilarCache`]), so a
//! long-lived engine (the daemon, `mcp serve`) embeds only what changed.
//! Nothing of them is searchable. Docs are compared against the vectors the
//! D196 index has already stored; a doc it has not indexed yet is not a
//! candidate, and only the new doc is embedded.
//!
//! Both thresholds were chosen on the labelled pairs in `similar/pairs.json`
//! for the model [`TUNED_FOR`] names; a test fails the moment
//! [`crate::embed::MODEL_ID`] moves, so a model swap re-tunes them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};

use rusqlite::params;
use serde_json::{json, Value};

use super::task::opening_summary;
use super::Engine;
use crate::embed::{self, QueryVector, StoredVector, DIMS, MODEL_ID};
use crate::error::ApiError;
use crate::types::Status;

/// The floor a task's similarity, rounded to three decimals, must reach.
pub(crate) const TASK_THRESHOLD: f64 = 0.7;

/// The floor a doc's similarity, rounded to three decimals, must reach.
pub(crate) const DOC_THRESHOLD: f64 = 0.8;

/// At most this many candidates are named.
pub(crate) const MAX: usize = 3;

/// Below this many tasks to embed, threads cost more than they save.
const INLINE_BELOW: usize = 64;

/// The model the thresholds were tuned for.
#[cfg(test)]
const TUNED_FOR: &str = "potion-base-8M@bf8b056651a2c21b8d2565580b8569da283cab23/int8/tok2/chunk4";

type Vector = Option<[f32; DIMS]>;

/// An open task's vector, and the title and oldest note it was embedded
/// from: what an add compares to tell whether it must embed the task again.
struct TaskVector {
    title: String,
    note: String,
    vector: Vector,
}

/// This process's embedded open tasks, by task id.
#[derive(Default)]
pub(super) struct SimilarCache {
    tasks: HashMap<String, TaskVector>,
    /// Texts embedded, of either kind.
    embedded: usize,
    /// Makes the next embedding panic: the failure a test injects.
    #[cfg(test)]
    fail: bool,
}

pub(super) type SimilarCell = RefCell<SimilarCache>;

impl SimilarCache {
    /// Whether a test asked the next embedding to panic.
    fn failing(&self) -> bool {
        #[cfg(test)]
        return self.fail;
        #[cfg(not(test))]
        false
    }
}

/// The failure a test injects: a panic where an embedding would be.
fn injected(fail: bool) {
    if fail {
        panic!("injected embedding failure (D237 test)");
    }
}

/// An open task's vector: its title, then its description (the opening
/// summary of its oldest note).
fn embed_task(title: &str, note: &str) -> Vector {
    embed::embed_query(&format!("{title}\n{}", opening_summary(note)))
}

/// [`embed_task`] for each of `todo`, in order: inline for a few, spread
/// over the machine's cores for many. Each is independent, so the result is
/// the same bits either way.
fn embed_all(todo: &[(&str, &str)], fail: bool) -> Vec<Vector> {
    let one = |&(title, note): &(&str, &str)| {
        injected(fail);
        embed_task(title, note)
    };
    if todo.len() < INLINE_BELOW {
        return todo.iter().map(one).collect();
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    // Four parts a core, so a slow core (an efficiency core, a busy one) holds
    // up a quarter of its share rather than all of it.
    let chunk = todo.len().div_ceil(threads * 4).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = todo
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(one).collect::<Vec<_>>()))
            .collect();
        // A panicking part panics the scope, which `similar_tasks` catches.
        parts
            .into_iter()
            .flat_map(|p| p.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    })
}

fn sim(q: &[f32; DIMS], v: &Vector) -> f64 {
    v.as_ref().map_or(f64::NEG_INFINITY, |v| {
        embed::round_similarity(embed::cosine(q, v))
    })
}

impl Engine {
    /// The open tasks of `project` whose similarity to `title` is at or
    /// above `floor`, best first then by short id, at most [`MAX`]. A task
    /// is compared as its title then its description.
    pub(crate) fn similar_tasks(
        &self,
        title: &str,
        project: Option<&str>,
        floor: f64,
    ) -> Result<Value, ApiError> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT t.id, t.short_id, t.title, \
             (SELECT a.body FROM annotations a WHERE a.task_id = t.id \
              AND a.removed IS NULL ORDER BY a.id LIMIT 1) \
             FROM tasks t WHERE t.project IS ?1 AND t.status NOT IN ({})",
            Status::sql_in_list(Status::is_terminal)
        ))?;
        let rows: Vec<(String, i64, String, String)> = stmt
            .query_map(params![project], |r| {
                let note: Option<String> = r.get(3)?;
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, note.unwrap_or_default()))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut cache = self.similar.borrow_mut();
        let cache = &mut *cache;
        let scored = catch_unwind(AssertUnwindSafe(|| {
            let q = embed::embed_query(title);
            cache.embedded += 1;
            let todo: Vec<(&str, &str)> = rows
                .iter()
                .filter(|(id, _, t, s)| {
                    cache
                        .tasks
                        .get(id)
                        .is_none_or(|c| c.title != *t || c.note != *s)
                })
                .map(|(_, _, t, s)| (t.as_str(), s.as_str()))
                .collect();
            let mut fresh = embed_all(&todo, cache.failing()).into_iter();
            drop(todo);
            // The rows move into the cache, so a cold add copies no text.
            let mut open: Vec<(String, i64)> = Vec::with_capacity(rows.len());
            for (id, short_id, title, note) in rows {
                if cache
                    .tasks
                    .get(&id)
                    .is_none_or(|c| c.title != title || c.note != note)
                {
                    let vector = fresh.next().expect("one per task to embed");
                    cache.embedded += 1;
                    cache.tasks.insert(
                        id.clone(),
                        TaskVector {
                            title,
                            note,
                            vector,
                        },
                    );
                }
                open.push((id, short_id));
            }
            let keep: HashSet<&str> = open.iter().map(|(id, _)| id.as_str()).collect();
            cache.tasks.retain(|id, _| keep.contains(id.as_str()));
            let Some(q) = q else {
                return Vec::new();
            };
            open.into_iter()
                .filter_map(|(id, short_id)| {
                    let c = &cache.tasks[&id];
                    let s = sim(&q, &c.vector);
                    (s >= floor).then(|| (s, short_id, c.title.clone(), id))
                })
                .collect::<Vec<_>>()
        }));
        let mut hits = scored.unwrap_or_default();
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        hits.truncate(MAX);
        Ok(hits
            .into_iter()
            .map(|(s, short_id, t, id)| {
                json!({ "id": id, "short_id": short_id, "title": t, "similarity": s })
            })
            .collect())
    }

    /// The docs — every doc for an unscoped one, else those of `project` and
    /// the unscoped — whose stored best chunk is at or above `floor` to the
    /// doc `title` + `body`, best first then by id, at most [`MAX`]. A doc
    /// the index has not stored a vector for is not a candidate.
    pub(crate) fn similar_docs(
        &self,
        title: &str,
        body: &str,
        project: Option<&str>,
        floor: f64,
    ) -> Result<Value, ApiError> {
        let q = {
            let mut cache = self.similar.borrow_mut();
            cache.embedded += 1;
            let fail = cache.failing();
            catch_unwind(|| {
                injected(fail);
                embed::embed(&format!("{title}\n{body}"))
            })
        };
        let Ok(Some(q)) = q else {
            return Ok(json!([]));
        };
        let q = QueryVector::new(&q);
        let mut stmt = self.conn.prepare_cached(
            "SELECT d.id, d.title, v.vec FROM memory_vectors v JOIN docs d ON d.id = v.owner_id \
             WHERE v.kind = 'doc' AND v.model = ?1 AND v.vec IS NOT NULL \
             AND (?2 IS NULL OR d.project = ?2 OR d.project IS NULL) \
             ORDER BY d.id, v.chunk",
        )?;
        let mut rows = stmt.query(params![MODEL_ID, project])?;
        let mut best: Vec<(f64, String, String)> = Vec::new();
        while let Some(r) = rows.next()? {
            let Some(v) =
                StoredVector::from_blob(r.get_ref(2)?.as_blob().map_err(rusqlite::Error::from)?)
            else {
                continue;
            };
            let s = embed::round_similarity(v.similarity(&q));
            let id: String = r.get(0)?;
            match best.last_mut() {
                Some(b) if b.1 == id => b.0 = b.0.max(s),
                _ => best.push((s, id, r.get(1)?)),
            }
        }
        best.retain(|b| b.0 >= floor);
        best.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        best.truncate(MAX);
        Ok(best
            .into_iter()
            .map(|(s, id, t)| json!({ "id": id, "title": t, "similarity": s }))
            .collect())
    }
}

#[cfg(test)]
mod tests;
