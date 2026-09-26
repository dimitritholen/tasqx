//! D196: the semantic index — reconciling `memory_vectors` with the text it
//! describes, a per-process copy of it, and the retrieval primitive
//! `memory.search` builds its semantic list from.
//!
//! The triggers `storage::migrate_vectors` creates only ever delete, so the
//! table holds a vector for an entry exactly while the entry still holds the
//! text it was built from. What has no row is embedded here, just before a
//! search needs it, and written back with a guard: the source row must still
//! hold the text that was embedded, or nothing is inserted. The write never
//! waits on the lock; when it cannot be made (another writer holds the lock,
//! the file is read-only), the vectors serve this call from memory and the
//! next search tries again. The index never fails a search.
//!
//! The copy in memory is keyed on `PRAGMA data_version`, which moves when
//! ANOTHER connection commits, and on this connection's `total_changes`,
//! which moves when this one writes anything at all (a trigger's delete
//! included), because `data_version` does not see a connection's own commits.

// `memory.search` is this module's caller, and it arrives with #838; until
// then only the tests below reach the primitive. `expect`, so the attribute
// goes the moment it is no longer true.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "memory.search calls it from #838 (D196)")
)]

use std::cell::RefCell;
use std::collections::HashSet;
use std::time::Duration;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};

use super::Engine;
use crate::embed::{self, BLOB_LEN, DIMS, MODEL_ID};

/// What a vector belongs to. Ordered as the words sort, so a tie on
/// similarity breaks the same way D196's `(kind, id)` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    Annotation,
    Doc,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Annotation => "annotation",
            Kind::Doc => "doc",
        }
    }
}

/// Which entries the semantic list is drawn from, with the lexical arms'
/// meaning in `engine/memory.rs`: a doc's project is its own column, an
/// annotation's is its task's; `include_unscoped` widens a project to
/// entries with none; `exclude_task` drops that task's own annotations.
#[derive(Clone, Debug)]
pub(crate) struct SemanticFilter<'a> {
    /// One of [`super::MEMORY_SCOPES`].
    pub scope: &'a str,
    pub project: Option<&'a str>,
    pub include_unscoped: bool,
    pub exclude_task: Option<&'a str>,
    /// The floor, compared with the similarity after rounding.
    pub min_similarity: f64,
    /// How many hits to return, best first.
    pub depth: usize,
}

/// One entry's best chunk.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticHit {
    pub kind: Kind,
    pub owner_id: String,
    /// The chunk's index in what [`crate::embed::chunk`] cuts from the
    /// entry's text, which [`Engine::semantic_snippet`] cuts again.
    pub chunk: i64,
    /// Cosine to the query, rounded to three decimals.
    pub similarity: f64,
}

/// The semantic list: the first `depth` hits, and how many entries in the
/// filter were at or above the floor.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SemanticHits {
    pub hits: Vec<SemanticHit>,
    pub total: usize,
}

/// A stored vector as the cache holds it: the int8 values and their norm,
/// taken as [`embed::cosine_quantized`] takes it, so a similarity computed
/// here has the same bits as one computed there.
struct Stored {
    q: [i8; DIMS],
    norm: f32,
}

impl Stored {
    fn from_blob(blob: &[u8]) -> Option<Stored> {
        if blob.len() != BLOB_LEN {
            return None;
        }
        let mut q = [0i8; DIMS];
        let mut n = 0.0f32;
        for (i, slot) in q.iter_mut().enumerate() {
            *slot = blob[i] as i8;
            let f = f32::from(*slot);
            n += f * f;
        }
        Some(Stored { q, norm: n.sqrt() })
    }

    fn similarity(&self, query: &[f32; DIMS]) -> f32 {
        if self.norm == 0.0 {
            return 0.0;
        }
        let mut d = 0.0f32;
        for (qv, &s) in query.iter().zip(&self.q) {
            d += qv * f32::from(s);
        }
        d / self.norm
    }
}

/// An entry and every chunk vector it has, with what the filter needs.
struct Entry {
    kind: Kind,
    owner_id: String,
    project: Option<String>,
    /// An annotation's task; `None` for a doc.
    task_id: Option<String>,
    chunks: Vec<(i64, Stored)>,
}

/// The per-process copy of the current model's vectors.
#[derive(Default)]
pub(super) struct VectorCache {
    /// `(data_version, total_changes)` the entries were read at.
    key: Option<(i64, u64)>,
    /// Every entry had a row under the current model at `key`, so a search
    /// at the same key has nothing to embed and need not look.
    complete: bool,
    entries: Vec<Entry>,
}

pub(super) type VectorCell = RefCell<VectorCache>;

/// An entry with no row under the current model, as it was read: the text
/// the guard compares against, and what embedding it gave.
pub(super) struct Pending {
    kind: Kind,
    owner_id: String,
    project: Option<String>,
    task_id: Option<String>,
    /// A doc's `title`, or an annotation's `body`.
    text_a: String,
    /// A doc's `search_body`; unused for an annotation.
    text_b: String,
    /// `(chunk index, blob)` for every chunk with a known token. Empty means
    /// the entry is stored as one sentinel row.
    vectors: Vec<(i64, [u8; BLOB_LEN])>,
}

impl Pending {
    fn into_entry(self) -> Option<Entry> {
        let chunks: Vec<(i64, Stored)> = self
            .vectors
            .iter()
            .filter_map(|(ix, blob)| Stored::from_blob(blob).map(|s| (*ix, s)))
            .collect();
        (!chunks.is_empty()).then_some(Entry {
            kind: self.kind,
            owner_id: self.owner_id,
            project: self.project,
            task_id: self.task_id,
            chunks,
        })
    }
}

/// Whether the index failed for a reason the design expects — the lock is
/// held, the file or the connection cannot be written — rather than a bug.
/// Expected ones are absorbed silently; an unexpected one is too, outside
/// this crate's tests, which it fails so that a broken statement cannot
/// hide behind "the index never fails a search".
fn absorb(e: &rusqlite::Error) {
    let expected = matches!(
        e.sqlite_error_code(),
        Some(
            ErrorCode::DatabaseBusy
                | ErrorCode::DatabaseLocked
                | ErrorCode::ReadOnly
                | ErrorCode::CannotOpen
                | ErrorCode::PermissionDenied
                | ErrorCode::SystemIoFailure
                | ErrorCode::DiskFull
        )
    );
    if cfg!(test) && !expected {
        panic!("the vector index hit an unexpected error: {e}");
    }
}

fn data_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, "data_version", |r| r.get(0))
}

fn table_exists(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master \
         WHERE type = 'table' AND name = 'memory_vectors')",
        [],
        |r| r.get(0),
    )
}

/// Every current-model vector whose owner still exists, grouped by owner.
fn load(conn: &Connection) -> rusqlite::Result<Vec<Entry>> {
    if !table_exists(conn)? {
        return Ok(Vec::new());
    }
    const DOCS: &str = "SELECT v.owner_id, v.chunk, v.vec, d.project, NULL \
         FROM memory_vectors v JOIN docs d ON d.id = v.owner_id \
         WHERE v.kind = 'doc' AND v.model = ?1 AND v.vec IS NOT NULL \
         ORDER BY v.owner_id, v.chunk";
    const ANNOTATIONS: &str = "SELECT v.owner_id, v.chunk, v.vec, t.project, a.task_id \
         FROM memory_vectors v JOIN annotations a ON a.id = v.owner_id \
         JOIN tasks t ON t.id = a.task_id \
         WHERE v.kind = 'annotation' AND v.model = ?1 AND v.vec IS NOT NULL \
         AND a.removed IS NULL \
         ORDER BY v.owner_id, v.chunk";
    let mut out: Vec<Entry> = Vec::new();
    for (kind, sql) in [(Kind::Doc, DOCS), (Kind::Annotation, ANNOTATIONS)] {
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(params![MODEL_ID])?;
        while let Some(r) = rows.next()? {
            let owner: String = r.get(0)?;
            let chunk: i64 = r.get(1)?;
            let blob = r.get_ref(2)?.as_blob()?;
            let Some(stored) = Stored::from_blob(blob) else {
                continue;
            };
            match out.last_mut() {
                Some(e) if e.kind == kind && e.owner_id == owner => e.chunks.push((chunk, stored)),
                _ => out.push(Entry {
                    kind,
                    owner_id: owner,
                    project: r.get(3)?,
                    task_id: r.get(4)?,
                    chunks: vec![(chunk, stored)],
                }),
            }
        }
    }
    Ok(out)
}

/// Embed each chunk; a chunk with no known token has no vector.
fn embed_chunks(chunks: &[String]) -> Vec<(i64, [u8; BLOB_LEN])> {
    chunks
        .iter()
        .enumerate()
        .filter_map(|(ix, c)| embed::embed(c).map(|v| (ix as i64, embed::quantize(&v))))
        .collect()
}

/// Every entry that has no row under the current model, read and embedded.
/// Docs: all of them. Annotations: live and non-empty.
pub(super) fn scan_missing(conn: &Connection) -> rusqlite::Result<Vec<Pending>> {
    let has_table = table_exists(conn)?;
    let missing = |kind: &str, alias: &str| {
        if has_table {
            format!(
                "AND NOT EXISTS (SELECT 1 FROM memory_vectors v WHERE v.kind = '{kind}' \
                 AND v.owner_id = {alias}.id AND v.model = ?1)"
            )
        } else {
            // A read-only store an older binary wrote: nothing is stored,
            // everything is computed for the call. `?1` still has a home.
            "AND ?1 IS NOT NULL".to_string()
        }
    };
    let mut out = Vec::new();
    let docs = format!(
        "SELECT d.id, d.title, d.search_body, d.project FROM docs d WHERE 1 {}",
        missing("doc", "d")
    );
    let mut stmt = conn.prepare(&docs)?;
    let rows = stmt.query_map(params![MODEL_ID], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, title, search_body, project) = row?;
        let vectors = embed_chunks(&embed::chunk::chunk_doc(&title, &search_body));
        out.push(Pending {
            kind: Kind::Doc,
            owner_id: id,
            project,
            task_id: None,
            text_a: title,
            text_b: search_body,
            vectors,
        });
    }
    let annotations = format!(
        "SELECT a.id, a.body, a.task_id, t.project FROM annotations a \
         JOIN tasks t ON t.id = a.task_id \
         WHERE a.removed IS NULL AND a.body <> '' {}",
        missing("annotation", "a")
    );
    let mut stmt = conn.prepare(&annotations)?;
    let rows = stmt.query_map(params![MODEL_ID], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, body, task_id, project) = row?;
        let vectors = embed_chunks(&embed::chunk::chunk_annotation(&body));
        out.push(Pending {
            kind: Kind::Annotation,
            owner_id: id,
            project,
            task_id: Some(task_id),
            text_a: body,
            text_b: String::new(),
            vectors,
        });
    }
    Ok(out)
}

/// Why nothing was written.
#[derive(Debug)]
pub(super) enum NotPersisted {
    /// Read-only, or inside a transaction someone else owns: not attempted.
    Skipped,
    /// Attempted and refused; see [`absorb`].
    Failed(rusqlite::Error),
}

/// Write `pending` in one transaction taken without waiting, each entry
/// only while its source row still holds the text that was embedded. Returns
/// how many entries were written; one that lost a race with an edit is not.
pub(super) fn persist(conn: &Connection, pending: &[Pending]) -> Result<usize, NotPersisted> {
    if !conn.is_autocommit() || conn.is_readonly("main").unwrap_or(true) {
        return Err(NotPersisted::Skipped);
    }
    if !table_exists(conn).map_err(NotPersisted::Failed)? {
        return Err(NotPersisted::Skipped);
    }
    let previous: i64 = conn
        .pragma_query_value(None, "busy_timeout", |r| r.get(0))
        .map_err(NotPersisted::Failed)?;
    conn.busy_timeout(Duration::ZERO)
        .map_err(NotPersisted::Failed)?;
    let result = write_guarded(conn, pending);
    let restored = conn.busy_timeout(Duration::from_millis(previous.max(0).unsigned_abs()));
    let written = result.map_err(NotPersisted::Failed)?;
    restored.map_err(NotPersisted::Failed)?;
    Ok(written)
}

fn write_guarded(conn: &Connection, pending: &[Pending]) -> rusqlite::Result<usize> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut doc = tx.prepare(
        "INSERT OR IGNORE INTO memory_vectors (kind, owner_id, model, chunk, vec) \
         SELECT 'doc', ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM docs \
         WHERE id = ?1 AND title IS ?5 AND search_body IS ?6)",
    )?;
    let mut annotation = tx.prepare(
        "INSERT OR IGNORE INTO memory_vectors (kind, owner_id, model, chunk, vec) \
         SELECT 'annotation', ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM annotations \
         WHERE id = ?1 AND body = ?5 AND removed IS NULL)",
    )?;
    let mut doc_holds = tx.prepare(
        "SELECT EXISTS (SELECT 1 FROM docs WHERE id = ?1 AND title IS ?2 AND search_body IS ?3)",
    )?;
    let mut annotation_holds = tx.prepare(
        "SELECT EXISTS (SELECT 1 FROM annotations WHERE id = ?1 AND body = ?2 \
         AND removed IS NULL)",
    )?;
    let mut written = 0;
    for p in pending {
        let holds: bool = match p.kind {
            Kind::Doc => doc_holds.query_row(params![p.owner_id, p.text_a, p.text_b], |r| r.get(0)),
            Kind::Annotation => {
                annotation_holds.query_row(params![p.owner_id, p.text_a], |r| r.get(0))
            }
        }?;
        if !holds {
            continue;
        }
        let sentinel = [(-1i64, None)];
        let rows: Vec<(i64, Option<&[u8]>)> = if p.vectors.is_empty() {
            sentinel.to_vec()
        } else {
            p.vectors
                .iter()
                .map(|(ix, blob)| (*ix, Some(&blob[..])))
                .collect()
        };
        for (ix, blob) in rows {
            match p.kind {
                Kind::Doc => {
                    doc.execute(params![p.owner_id, MODEL_ID, ix, blob, p.text_a, p.text_b])?
                }
                Kind::Annotation => {
                    annotation.execute(params![p.owner_id, MODEL_ID, ix, blob, p.text_a])?
                }
            };
        }
        written += 1;
    }
    drop((doc, annotation, doc_holds, annotation_holds));
    tx.commit()?;
    Ok(written)
}

fn in_filter(e: &Entry, f: &SemanticFilter<'_>) -> bool {
    let scoped = match f.scope {
        "docs" => e.kind == Kind::Doc,
        "annotations" => e.kind == Kind::Annotation,
        _ => true,
    };
    let in_project = match f.project {
        None => true,
        Some(p) => match e.project.as_deref() {
            Some(q) => q == p,
            None => f.include_unscoped,
        },
    };
    let excluded = f.exclude_task.is_some() && e.task_id.as_deref() == f.exclude_task;
    scoped && in_project && !excluded
}

/// The best chunk of `e` for `query`, by rounded similarity, the lowest
/// chunk index on a tie.
fn best(e: &Entry, query: &[f32; DIMS]) -> (i64, f64) {
    let mut top = (i64::MAX, f64::NEG_INFINITY);
    for (ix, s) in &e.chunks {
        let sim = embed::round_similarity(s.similarity(query));
        if sim > top.1 || (sim == top.1 && *ix < top.0) {
            top = (*ix, sim);
        }
    }
    top
}

impl Engine {
    /// Bring the index up to date and return what it could not store: the
    /// entries embedded for this call only. Never fails; see [`absorb`].
    fn reconcile_vectors(&self) -> Vec<Entry> {
        match self.try_reconcile() {
            Ok(ephemeral) => ephemeral,
            Err(e) => {
                absorb(&e);
                self.vectors.borrow_mut().key = None;
                Vec::new()
            }
        }
    }

    fn try_reconcile(&self) -> rusqlite::Result<Vec<Entry>> {
        let conn = &self.conn;
        // Read before the rows, so a commit landing between the two makes
        // the key stale rather than the rows.
        let key = (data_version(conn)?, conn.total_changes());
        let mut cache = self.vectors.borrow_mut();
        if cache.key == Some(key) && cache.complete {
            return Ok(Vec::new());
        }
        if cache.key != Some(key) {
            cache.entries = load(conn)?;
            cache.key = Some(key);
            cache.complete = false;
        }
        let pending = scan_missing(conn)?;
        if pending.is_empty() {
            cache.complete = true;
            return Ok(Vec::new());
        }
        match persist(conn, &pending) {
            Ok(written) => {
                let after = (data_version(conn)?, conn.total_changes());
                if after.0 == key.0 && written == pending.len() {
                    // Nobody else committed since `key`: the store is what
                    // was loaded plus what was just written.
                    cache
                        .entries
                        .extend(pending.into_iter().filter_map(Pending::into_entry));
                    cache.key = Some(after);
                    cache.complete = true;
                    Ok(Vec::new())
                } else {
                    cache.key = None;
                    Ok(pending
                        .into_iter()
                        .filter_map(Pending::into_entry)
                        .collect())
                }
            }
            Err(not) => {
                if let NotPersisted::Failed(e) = &not {
                    absorb(e);
                }
                cache.complete = false;
                Ok(pending
                    .into_iter()
                    .filter_map(Pending::into_entry)
                    .collect())
            }
        }
    }

    /// D196's semantic list: every entry in `filter` whose best chunk's
    /// similarity to `query`, rounded to three decimals, is at or above
    /// `filter.min_similarity`, best first, then by `(kind, id)`; the first
    /// `filter.depth` of them, and how many there were. Reconciles the
    /// index first. Empty when the query has no known token.
    pub(crate) fn semantic_candidates(
        &self,
        query: &str,
        filter: &SemanticFilter<'_>,
    ) -> SemanticHits {
        let Some(q) = embed::embed(query) else {
            return SemanticHits::default();
        };
        let ephemeral = self.reconcile_vectors();
        let fresh: HashSet<(Kind, &str)> = ephemeral
            .iter()
            .map(|e| (e.kind, e.owner_id.as_str()))
            .collect();
        let cache = self.vectors.borrow();
        let mut hits: Vec<SemanticHit> = cache
            .entries
            .iter()
            .filter(|e| fresh.is_empty() || !fresh.contains(&(e.kind, e.owner_id.as_str())))
            .chain(ephemeral.iter())
            .filter(|e| in_filter(e, filter))
            .filter_map(|e| {
                let (chunk, similarity) = best(e, &q);
                (similarity >= filter.min_similarity).then(|| SemanticHit {
                    kind: e.kind,
                    owner_id: e.owner_id.clone(),
                    chunk,
                    similarity,
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.similarity
                .total_cmp(&a.similarity)
                .then(a.kind.cmp(&b.kind))
                .then_with(|| a.owner_id.cmp(&b.owner_id))
        });
        let total = hits.len();
        hits.truncate(filter.depth);
        SemanticHits { hits, total }
    }

    /// The text of chunk `chunk` of an entry, cut again from its source row
    /// the way it was cut to embed it: a semantic hit's snippet. `None` when
    /// the entry or the chunk is gone.
    pub(crate) fn semantic_snippet(
        &self,
        kind: Kind,
        owner_id: &str,
        chunk: i64,
    ) -> Option<String> {
        let chunks = match kind {
            Kind::Doc => {
                let (title, body): (String, String) = self
                    .conn
                    .query_row(
                        "SELECT title, search_body FROM docs WHERE id = ?1",
                        params![owner_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                    .ok()??;
                embed::chunk::chunk_doc(&title, &body)
            }
            Kind::Annotation => {
                let body: String = self
                    .conn
                    .query_row(
                        "SELECT body FROM annotations WHERE id = ?1 AND removed IS NULL",
                        params![owner_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .ok()??;
                embed::chunk::chunk_annotation(&body)
            }
        };
        usize::try_from(chunk)
            .ok()
            .and_then(|ix| chunks.into_iter().nth(ix))
    }
}

#[cfg(test)]
mod tests;
