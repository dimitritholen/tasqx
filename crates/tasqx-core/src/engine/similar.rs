//! D237: what a new task or doc looks like that is already there.
//!
//! `task.add` and `memory.add` answer `similar`, always present: up to
//! [`MAX`] open tasks or live docs whose similarity to what was just written
//! is at or above [`TASK_THRESHOLD`] or [`DOC_THRESHOLD`], best first. A
//! warning, never a refusal: the write has committed before anything is
//! compared, and a comparison that fails answers an empty list.
//!
//! Tasks keep no vectors (D237): the open tasks of the new task's project are
//! embedded on every add, title and description, and nothing of them is
//! stored or searchable. Docs are compared through the semantic index D196
//! already keeps ([`super::vectors`]).
//!
//! Both thresholds were chosen on the labelled pairs in `similar/pairs.json`
//! for the model [`TUNED_FOR`] names; a test fails the moment
//! [`crate::embed::MODEL_ID`] moves, so a model swap re-tunes them.

use rusqlite::params;
use serde_json::{json, Value};

use super::vectors::{Kind, SemanticFilter};
use super::Engine;
use crate::embed::{self, QueryVector};

/// The floor a task's similarity, rounded to three decimals, must reach.
pub(crate) const TASK_THRESHOLD: f64 = 0.7;

/// The floor a doc's similarity, rounded to three decimals, must reach.
pub(crate) const DOC_THRESHOLD: f64 = 0.8;

/// At most this many candidates are named.
pub(crate) const MAX: usize = 3;

/// The model the thresholds were tuned for.
#[cfg(test)]
const TUNED_FOR: &str = "potion-base-8M@bf8b056651a2c21b8d2565580b8569da283cab23/int8/tok2/chunk4";

/// How much of a description is compared: as much as `task.next`'s summary.
/// More text compared catches more duplicates and costs more time, and this
/// is what fits the latency budget D237 sets on a 5,000-open-task store.
const DESCRIPTION_CHARS: usize = 400;

/// What an open task is compared as: its title, then its description — the
/// first paragraph of its oldest note, cut at [`DESCRIPTION_CHARS`].
fn task_text(title: &str, note: Option<&str>) -> String {
    let Some(note) = note else {
        return title.to_string();
    };
    let para = note.trim_start().split("\n\n").next().unwrap_or("");
    let cut = para
        .char_indices()
        .nth(DESCRIPTION_CHARS)
        .map_or(para, |(i, _)| &para[..i]);
    format!("{title}\n{cut}")
}

/// An open task as read for comparison: id, short id, title, oldest note.
type Row = (String, i64, String, Option<String>);

/// Each row's rounded similarity to `q`, in row order. Spread over the
/// machine's cores: embedding every open task is the whole cost of a task
/// add's comparison, and each row is independent, so the result is the same
/// bits on one thread or many.
fn scores(q: &[f32; embed::DIMS], rows: &[Row]) -> Vec<f64> {
    let score = |(_, _, title, note): &Row| {
        embed::embed_query(&task_text(title, note.as_deref())).map_or(f64::NEG_INFINITY, |v| {
            embed::round_similarity(embed::cosine(q, &v))
        })
    };
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let chunk = rows.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = rows
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(score).collect::<Vec<f64>>()))
            .collect();
        parts
            .into_iter()
            .flat_map(|p| p.join().expect("embedding does not panic"))
            .collect()
    })
}

impl Engine {
    /// The open tasks of `project` (other than `id`) whose similarity to
    /// `title` is at or above `floor`, best first then by short id, at most
    /// [`MAX`].
    pub(crate) fn similar_tasks(
        &self,
        id: &str,
        title: &str,
        project: Option<&str>,
        floor: f64,
    ) -> Value {
        let Some(q) = embed::embed_query(title) else {
            return json!([]);
        };
        let rows = (|| -> rusqlite::Result<Vec<Row>> {
            let mut stmt = self.conn.prepare_cached(
                "SELECT t.id, t.short_id, t.title, \
                 (SELECT a.body FROM annotations a WHERE a.task_id = t.id \
                  AND a.removed IS NULL ORDER BY a.id LIMIT 1) \
                 FROM tasks t WHERE t.project IS ?1 AND t.id <> ?2 \
                 AND t.status NOT IN ('done', 'cancelled')",
            )?;
            let rows = stmt.query_map(params![project, id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            rows.collect()
        })()
        .unwrap_or_default();
        let mut hits: Vec<(f64, Row)> = scores(&q, &rows)
            .into_iter()
            .zip(rows)
            .filter(|(s, _)| *s >= floor)
            .collect();
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1 .1.cmp(&b.1 .1)));
        hits.truncate(MAX);
        hits.into_iter()
            .map(|(s, (tid, short_id, t, _))| {
                json!({ "id": tid, "short_id": short_id, "title": t, "similarity": s })
            })
            .collect()
    }

    /// The docs other than `id` — every doc for an unscoped one, else those
    /// of `project` and the unscoped — whose best chunk is at or above
    /// `floor` to the doc `title` + `body`, best first, at most [`MAX`].
    pub(crate) fn similar_docs(
        &self,
        id: &str,
        title: &str,
        body: &str,
        project: Option<&str>,
        floor: f64,
    ) -> Value {
        let Some(q) = embed::embed(&format!("{title}\n{body}")) else {
            return json!([]);
        };
        let filter = SemanticFilter {
            scope: "docs",
            project,
            include_unscoped: true,
            exclude_task: None,
            min_similarity: floor,
            depth: MAX + 1,
        };
        self.semantic_pass(&QueryVector::new(&q), &filter)
            .hits
            .into_iter()
            .filter(|h| h.kind == Kind::Doc && h.owner_id != id)
            .take(MAX)
            .filter_map(|h| {
                let title: String = self
                    .conn
                    .query_row(
                        "SELECT title FROM docs WHERE id = ?1",
                        params![h.owner_id],
                        |r| r.get(0),
                    )
                    .ok()?;
                Some(json!({ "id": h.owner_id, "title": title, "similarity": h.similarity }))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
