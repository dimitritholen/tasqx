//! D237: what a new task or doc looks like that is already there.
//!
//! `task.add` and `memory.add` answer `similar`, always present: up to
//! [`MAX`] open tasks or live docs whose similarity to what is being written
//! is at or above [`TASK_THRESHOLD`] or [`DOC_THRESHOLD`], best first. Advice,
//! never a refusal: it is computed before the add takes its write lock, and
//! anything that goes wrong in it — an error, a panic — is noted on stderr
//! and answers an empty list while the add goes ahead.
//!
//! Tasks keep no stored vectors (D237): the open tasks of the project are
//! embedded on demand and kept in this process only ([`SimilarCache`]), so a
//! long-lived engine (the daemon, `mcp serve`) embeds only what changed.
//! Nothing of them is searchable. Docs are compared through the D196 index,
//! against the vectors it has stored, without reconciling it; `memory.add`
//! stores the new doc's vectors with the doc, so the next add sees it.
//!
//! Both thresholds were chosen on the labelled pairs in `similar/pairs.json`
//! for the model [`TUNED_FOR`] names; a test fails the moment
//! [`crate::embed::MODEL_ID`] moves, so a model swap re-tunes them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::params;
use serde_json::{json, Value};

use super::task::opening_summary;
use super::vectors::{Kind, SemanticFilter};
use super::Engine;
use crate::embed::{self, QueryVector, DIMS};
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

/// The most open-task vectors one engine keeps, across all projects.
// ponytail: drop-oldest past the bound, not LRU; a store with more open
// tasks than this re-embeds the overflow on each add. Raise it, or make it
// LRU, if a real store gets there.
const CACHE_BOUND: usize = 20_000;

/// The model the thresholds were tuned for.
#[cfg(test)]
const TUNED_FOR: &str = "potion-base-8M@bf8b056651a2c21b8d2565580b8569da283cab23/int8/tok2/chunk4";

type Vector = Option<[f32; DIMS]>;

/// An open task's vector, and what it was embedded from: the title and
/// description an add compares to tell whether it must embed again.
struct TaskVector {
    project: Option<String>,
    title: String,
    summary: String,
    vector: Vector,
    /// When it was embedded, in this cache's own count: the oldest go first
    /// past [`CACHE_BOUND`].
    stamp: u64,
}

/// What a test makes the comparison do.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum Failure {
    Panic,
    Error,
}

/// This process's embedded open tasks, by task id.
#[derive(Default)]
pub(super) struct SimilarCache {
    tasks: HashMap<String, TaskVector>,
    stamp: u64,
    /// Texts embedded, of either kind: one per task, one per new doc.
    pub(super) embedded: usize,
    #[cfg(test)]
    pub(super) fail: Option<Failure>,
}

pub(super) type SimilarCell = RefCell<SimilarCache>;

impl SimilarCache {
    /// The failure a test injected, if any.
    fn injected(&self) -> Result<(), ApiError> {
        #[cfg(test)]
        match self.fail {
            Some(Failure::Panic) => panic!("injected failure (D237 test)"),
            Some(Failure::Error) => return Err(ApiError::internal("injected failure (D237 test)")),
            None => {}
        }
        Ok(())
    }
}

/// An open task's vector: its title, then its description.
fn embed_task(title: &str, summary: &str) -> Vector {
    embed::embed_query(&format!("{title}\n{summary}"))
}

/// `f` of each of `items`, in order: inline for a few; for many, one worker
/// a core, each taking the next item until none is left, so a slow core
/// does less rather than holding the rest up. Each item is independent, so
/// the result is the same bits either way.
fn par_map<T: Sync, R: Send + Clone + Default>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.len() < INLINE_BELOW {
        return items.iter().map(f).collect();
    }
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len());
    let next = AtomicUsize::new(0);
    let mut out: Vec<R> = vec![R::default(); items.len()];
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else {
                            return mine;
                        };
                        mine.push((i, f(item)));
                    }
                })
            })
            .collect();
        for h in handles {
            // A worker's panic is re-raised here, which the caller catches.
            for (i, v) in h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)) {
                out[i] = v;
            }
        }
    });
    out
}

/// `f`'s answer, or `[]` when it errs or panics, with a note on stderr: the
/// comparison is advice, and the add it advises goes ahead regardless.
fn advisory(what: &str, f: impl FnOnce() -> Result<Value, ApiError>) -> Value {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            eprintln!(
                "tasqx: note: the {what} duplicate check was skipped: {}",
                e.message
            );
            json!([])
        }
        Err(_) => {
            eprintln!("tasqx: note: the {what} duplicate check was skipped: it panicked");
            json!([])
        }
    }
}

impl Engine {
    /// The open tasks of `project` whose similarity to `title` is at or
    /// above `floor`, best first then by short id, at most [`MAX`]. A task
    /// is compared as its title then its description, the opening summary
    /// of its oldest live note.
    pub(crate) fn similar_tasks(&self, title: &str, project: Option<&str>, floor: f64) -> Value {
        advisory("task", || self.try_similar_tasks(title, project, floor))
    }

    fn try_similar_tasks(
        &self,
        title: &str,
        project: Option<&str>,
        floor: f64,
    ) -> Result<Value, ApiError> {
        let mut cache = self.similar.borrow_mut();
        cache.injected()?;
        let Some(q) = embed::embed_query(title) else {
            return Ok(json!([]));
        };
        cache.embedded += 1;
        // The note is cut in SQL to more than its summary can use, so a long
        // note costs no more than a short one; `opening_summary` cuts the rest.
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT t.id, t.short_id, t.title, \
             (SELECT substr(a.body, 1, 2000) FROM annotations a WHERE a.task_id = t.id \
              AND a.removed IS NULL ORDER BY a.id LIMIT 1) \
             FROM tasks t WHERE t.project IS ?1 AND t.status NOT IN ({})",
            Status::sql_in_list(Status::is_terminal)
        ))?;
        let read: Vec<(String, i64, String, Option<String>)> = stmt
            .query_map(params![project], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let summaries = par_map(&read, |(_, _, _, note)| {
            note.as_deref().map(opening_summary).unwrap_or_default()
        });
        let rows: Vec<(String, i64, String, String)> = read
            .into_iter()
            .zip(summaries)
            .map(|((id, short_id, title, _), summary)| (id, short_id, title, summary))
            .collect();
        let cache = &mut *cache;
        let stale = |c: Option<&TaskVector>, t: &str, s: &str| {
            c.is_none_or(|c| c.title != t || c.summary != s)
        };
        let todo: Vec<(&str, &str)> = rows
            .iter()
            .filter(|(id, _, t, s)| stale(cache.tasks.get(id), t, s))
            .map(|(_, _, t, s)| (t.as_str(), s.as_str()))
            .collect();
        let mut fresh = par_map(&todo, |(t, s)| embed_task(t, s)).into_iter();
        drop(todo);
        // The rows move into the cache, so a cold add copies no text.
        let mut open: Vec<(String, i64)> = Vec::with_capacity(rows.len());
        for (id, short_id, title, summary) in rows {
            match cache.tasks.get_mut(&id) {
                Some(c) if !stale(Some(c), &title, &summary) => {
                    c.project = project.map(str::to_string);
                }
                _ => {
                    cache.stamp += 1;
                    cache.embedded += 1;
                    let vector = fresh.next().expect("one vector per task to embed");
                    cache.tasks.insert(
                        id.clone(),
                        TaskVector {
                            project: project.map(str::to_string),
                            title,
                            summary,
                            vector,
                            stamp: cache.stamp,
                        },
                    );
                }
            }
            open.push((id, short_id));
        }
        // This project's tasks that are no longer open leave; every other
        // project's stay, until the bound pushes the oldest out.
        let keep: HashSet<&str> = open.iter().map(|(id, _)| id.as_str()).collect();
        cache
            .tasks
            .retain(|id, c| c.project.as_deref() != project || keep.contains(id.as_str()));
        if cache.tasks.len() > CACHE_BOUND {
            let mut stamps: Vec<u64> = cache.tasks.values().map(|c| c.stamp).collect();
            stamps.sort_unstable();
            let cut = stamps[cache.tasks.len() - CACHE_BOUND];
            cache.tasks.retain(|_, c| c.stamp >= cut);
        }
        let mut hits: Vec<(f64, i64, String, String)> = open
            .into_iter()
            .filter_map(|(id, short_id)| {
                let c = cache.tasks.get(&id)?;
                let s = c.vector.as_ref().map_or(f64::NEG_INFINITY, |v| {
                    embed::round_similarity(embed::cosine(&q, v))
                });
                (s >= floor).then(|| (s, short_id, c.title.clone(), id))
            })
            .collect();
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
    /// the index has no stored vector for is not a candidate.
    pub(crate) fn similar_docs(
        &self,
        title: &str,
        body: &str,
        project: Option<&str>,
        floor: f64,
    ) -> Value {
        advisory("doc", || self.try_similar_docs(title, body, project, floor))
    }

    fn try_similar_docs(
        &self,
        title: &str,
        body: &str,
        project: Option<&str>,
        floor: f64,
    ) -> Result<Value, ApiError> {
        {
            let mut cache = self.similar.borrow_mut();
            cache.injected()?;
            cache.embedded += 1;
        }
        let Some(q) = embed::embed(&format!("{title}\n{body}")) else {
            return Ok(json!([]));
        };
        let filter = SemanticFilter {
            scope: "docs",
            project,
            include_unscoped: true,
            exclude_task: None,
            min_similarity: floor,
            depth: MAX,
        };
        let hits = self.stored_pass(&QueryVector::new(&q), &filter)?;
        let mut stmt = self
            .conn
            .prepare_cached("SELECT title FROM docs WHERE id = ?1")?;
        let mut out = Vec::with_capacity(hits.len());
        for h in hits.into_iter().filter(|h| h.kind == Kind::Doc) {
            let title: String = stmt.query_row(params![h.owner_id], |r| r.get(0))?;
            out.push(json!({ "id": h.owner_id, "title": title, "similarity": h.similarity }));
        }
        Ok(Value::Array(out))
    }
}

#[cfg(test)]
mod tests;
