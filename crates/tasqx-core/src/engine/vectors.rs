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
//! the file is read-only), the vectors stay in this process's copy and the
//! write is tried again on a later search, without embedding them again. The
//! index never fails a search, and never answers from vectors it has reason
//! to distrust: on an error it answers with none.
//!
//! The copy in memory is keyed on `memory_generation`, a counter pure-SQL
//! triggers bump on every change a semantic list can see, whichever process
//! or binary made it. A store with no such table (read-only, written by an
//! older binary) is keyed on `PRAGMA data_version` instead; that connection
//! cannot write, so another connection's commit is the only change there is.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};

use super::Engine;
use crate::embed::{self, QueryVector, StoredVector, BLOB_LEN, MODEL_ID};

/// What a vector belongs to. Ordered as the words sort, so a tie on
/// similarity breaks the same way D196's `(kind, id)` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    Annotation,
    Doc,
}

impl Kind {
    #[cfg(test)]
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
#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SemanticHits {
    pub hits: Vec<SemanticHit>,
    pub total: usize,
}

/// One semantic pass: the hits at or above the floor (the first `depth`),
/// and how many entries were compared at all.
pub(crate) struct SemanticPass {
    pub hits: Vec<SemanticHit>,
    pub compared: usize,
    /// The entries a pass inside a caller's transaction computed for itself,
    /// which [`Engine::semantic_similarities`] reads in place of the copy.
    this_call: Option<Vec<Entry>>,
}

/// Which entries one call answers from.
pub(super) enum Reconciled {
    /// The kept copy.
    Copy,
    /// Entries computed for this call alone, inside a caller's transaction.
    ThisCall(Vec<Entry>),
    /// None: the reconcile failed (see [`absorb`]).
    Failed,
}

/// An entry and every chunk vector it has, with what the filter needs.
pub(super) struct Entry {
    kind: Kind,
    owner_id: String,
    project: Option<String>,
    /// An annotation's task; `None` for a doc.
    task_id: Option<String>,
    chunks: Vec<(i64, StoredVector)>,
}

/// What the copy was read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    /// `memory_generation.gen`.
    Generation(i64),
    /// `PRAGMA data_version`, on a store without the index's tables.
    Unindexed(i64),
}

/// The per-process copy of the current model's vectors.
#[derive(Default)]
pub(super) struct VectorCache {
    key: Option<Key>,
    /// Every entry at `key` is in `entries`, stored or waiting in
    /// `unpersisted`, so a search at the same key embeds nothing.
    complete: bool,
    entries: Vec<Entry>,
    /// Embedded at `key` but not yet written: retried on a later search.
    unpersisted: Vec<Pending>,
    /// When this process last saw the model's `last_used` recorded, in unix
    /// seconds; it is written at most once a day.
    touched: Option<i64>,
    /// A busy timeout [`NoWait`] could not put back. The next reconcile puts
    /// it back before anything else, and writes nothing until it has.
    lost_timeout: Cell<Option<Duration>>,
    /// Entries embedded by this copy.
    #[cfg(test)]
    embedded: usize,
    /// Times the copy was read from the store.
    #[cfg(test)]
    loads: usize,
    /// Runs between a reconcile's scan and the end of its snapshot, so a
    /// test can land another writer's change exactly there.
    #[cfg(test)]
    after_scan: Option<Box<dyn FnMut() + Send>>,
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
    fn entry(&self) -> Option<Entry> {
        let chunks: Vec<(i64, StoredVector)> = self
            .vectors
            .iter()
            .filter_map(|(ix, blob)| StoredVector::from_blob(blob).map(|s| (*ix, s)))
            .collect();
        (!chunks.is_empty()).then(|| Entry {
            kind: self.kind,
            owner_id: self.owner_id.clone(),
            project: self.project.clone(),
            task_id: self.task_id.clone(),
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

/// Whether the store has the index's tables. A read-only open runs no
/// migration, so a store an older binary wrote has none.
fn indexed(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) = 3 FROM sqlite_master WHERE type = 'table' \
         AND name IN ('memory_vectors', 'memory_generation', 'memory_vector_models')",
        [],
        |r| r.get(0),
    )
}

fn read_key(conn: &Connection, indexed: bool) -> rusqlite::Result<Key> {
    if indexed {
        conn.query_row("SELECT gen FROM memory_generation WHERE id = 1", [], |r| {
            r.get(0)
        })
        .map(Key::Generation)
    } else {
        conn.pragma_query_value(None, "data_version", |r| r.get(0))
            .map(Key::Unindexed)
    }
}

/// Every current-model vector whose owner still exists, grouped by owner.
fn load(conn: &Connection, indexed: bool) -> rusqlite::Result<Vec<Entry>> {
    if !indexed {
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
            let Some(stored) = StoredVector::from_blob(r.get_ref(2)?.as_blob()?) else {
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
/// Docs: all of them. Annotations: live and non-empty. Without the index's
/// tables, every entry.
pub(super) fn scan_missing(
    conn: &Connection,
    indexed: bool,
    reuse: &mut HashMap<(Kind, String), Pending>,
) -> rusqlite::Result<(Vec<Pending>, usize)> {
    let mut embedded = 0;
    // An entry embedded earlier and not yet stored is taken as it is while
    // its text is unchanged (#838): a writer holding the lock across a
    // moving generation costs the changed entries alone.
    let mut take = |kind: Kind, id: &str, a: &str, b: &str| {
        reuse
            .remove(&(kind, id.to_string()))
            .filter(|p| p.text_a == a && p.text_b == b)
    };
    let missing = |kind: &str, alias: &str| {
        if indexed {
            format!(
                "AND NOT EXISTS (SELECT 1 FROM memory_vectors v WHERE v.kind = '{kind}' \
                 AND v.owner_id = {alias}.id AND v.model = ?1)"
            )
        } else {
            // `?1` still needs a home.
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
        if let Some(mut p) = take(Kind::Doc, &id, &title, &search_body) {
            p.project = project;
            out.push(p);
            continue;
        }
        embedded += 1;
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
        if let Some(mut p) = take(Kind::Annotation, &id, &body, "") {
            p.project = project;
            p.task_id = Some(task_id);
            out.push(p);
            continue;
        }
        embedded += 1;
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
    Ok((out, embedded))
}

/// Why nothing was written.
#[derive(Debug)]
pub(super) enum NotPersisted {
    /// Read-only, unindexed, or inside a transaction someone else owns: not
    /// attempted.
    Skipped,
    /// Attempted and refused; see [`absorb`].
    Failed(rusqlite::Error),
}

/// Sets a connection's busy timeout to zero and puts the old one back when
/// dropped, whichever way the write in between ends. A restore that fails
/// is recorded in `lost`, never dropped silently.
struct NoWait<'c> {
    conn: &'c Connection,
    previous: Duration,
    lost: &'c Cell<Option<Duration>>,
}

impl<'c> NoWait<'c> {
    fn new(conn: &'c Connection, lost: &'c Cell<Option<Duration>>) -> rusqlite::Result<NoWait<'c>> {
        let ms: i64 = conn.pragma_query_value(None, "busy_timeout", |r| r.get(0))?;
        let previous = Duration::from_millis(ms.max(0).unsigned_abs());
        conn.busy_timeout(Duration::ZERO)?;
        Ok(NoWait {
            conn,
            previous,
            lost,
        })
    }
}

impl Drop for NoWait<'_> {
    fn drop(&mut self) {
        if self.conn.busy_timeout(self.previous).is_err() {
            self.lost.set(Some(self.previous));
        }
    }
}

/// Write `pending` in one transaction taken without waiting, each entry
/// only while its source row still holds the text that was embedded, and,
/// with `touch` (unix seconds), record that the current model searched this
/// store then. Returns the rows inserted; an entry that lost a race with an
/// edit inserts none. `lost` is where a busy timeout that could not be put
/// back is kept; while it holds one, nothing is written.
pub(super) fn persist(
    conn: &Connection,
    pending: &[Pending],
    touch: Option<i64>,
    lost: &Cell<Option<Duration>>,
) -> Result<usize, NotPersisted> {
    if !conn.is_autocommit() || conn.is_readonly("main").unwrap_or(true) || lost.get().is_some() {
        return Err(NotPersisted::Skipped);
    }
    let _no_wait = NoWait::new(conn, lost).map_err(NotPersisted::Failed)?;
    write_guarded(conn, pending, touch).map_err(NotPersisted::Failed)
}

fn write_guarded(
    conn: &Connection,
    pending: &[Pending],
    touch: Option<i64>,
) -> rusqlite::Result<usize> {
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
    let mut inserted = 0;
    for p in pending {
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
            inserted += match p.kind {
                Kind::Doc => {
                    doc.execute(params![p.owner_id, MODEL_ID, ix, blob, p.text_a, p.text_b])?
                }
                Kind::Annotation => {
                    annotation.execute(params![p.owner_id, MODEL_ID, ix, blob, p.text_a])?
                }
            };
        }
    }
    drop((doc, annotation));
    if let Some(now) = touch {
        tx.execute(
            "INSERT INTO memory_vector_models (model, last_used) VALUES (?1, ?2) \
             ON CONFLICT (model) DO UPDATE SET last_used = MAX(last_used, excluded.last_used)",
            params![MODEL_ID, now],
        )?;
    }
    tx.commit()?;
    Ok(inserted)
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
fn best(e: &Entry, query: &QueryVector) -> (i64, f64) {
    let mut top = (i64::MAX, f64::NEG_INFINITY);
    for (ix, s) in &e.chunks {
        let sim = embed::round_similarity(s.similarity(query));
        if sim > top.1 || (sim == top.1 && *ix < top.0) {
            top = (*ix, sim);
        }
    }
    top
}

/// Whether the current model's `last_used` is due to be written: `(Some(now),
/// _)` when neither this process (`touched`) nor the store has it within a
/// day; otherwise `(None, Some(when the store has it))` when the store had
/// to be asked.
fn due_touch(
    conn: &Connection,
    touched: Option<i64>,
) -> rusqlite::Result<(Option<i64>, Option<i64>)> {
    let now = crate::clock::now().as_second();
    let fresh = |t: i64| now.saturating_sub(t) < TOUCH_EVERY_SECS;
    if touched.is_some_and(fresh) {
        return Ok((None, None));
    }
    let recorded: Option<i64> = conn
        .query_row(
            "SELECT last_used FROM memory_vector_models WHERE model = ?1",
            params![MODEL_ID],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match recorded {
        Some(t) if fresh(t) => (None, Some(t)),
        _ => (Some(now), None),
    })
}

/// How often a process records that its model searched a store.
const TOUCH_EVERY_SECS: i64 = 24 * 3600;

impl VectorCache {
    fn reset(&mut self) {
        self.reset_copy();
        self.unpersisted.clear();
    }

    /// Forget the copy, keeping what was embedded and not yet stored.
    fn reset_copy(&mut self) {
        self.key = None;
        self.complete = false;
        self.entries.clear();
    }

    /// Put `pending`'s vectors in the copy, replacing anything it held for
    /// the same owners.
    fn absorb_pending(&mut self, pending: &[Pending]) {
        absorb_into(&mut self.entries, pending);
    }
}

/// Put `pending`'s vectors in `entries`, replacing any for the same owners.
fn absorb_into(entries: &mut Vec<Entry>, pending: &[Pending]) {
    let owners: HashSet<(Kind, &str)> = pending
        .iter()
        .map(|p| (p.kind, p.owner_id.as_str()))
        .collect();
    entries.retain(|e| !owners.contains(&(e.kind, e.owner_id.as_str())));
    entries.extend(pending.iter().filter_map(Pending::entry));
}

impl Engine {
    /// Bring the copy (and, when it can, the stored index) up to date, and
    /// say which entries this call answers from. Never fails: on an error
    /// the call answers with none, and outside a caller's transaction the
    /// copy is emptied too, so no later search answers from it; see
    /// [`absorb`].
    pub(super) fn reconcile_vectors(&self) -> Reconciled {
        let in_transaction = !self.conn.is_autocommit();
        match self.try_reconcile() {
            Ok(r) => r,
            Err(e) => {
                absorb(&e);
                // A caller's transaction may fail for reasons of its own;
                // the warm copy was read committed, and stays.
                if !in_transaction {
                    self.vectors.borrow_mut().reset();
                }
                Reconciled::Failed
            }
        }
    }

    fn try_reconcile(&self) -> rusqlite::Result<Reconciled> {
        let conn = &self.conn;
        let mut cache = self.vectors.borrow_mut();
        if let Some(previous) = cache.lost_timeout.get() {
            if conn.busy_timeout(previous).is_ok() {
                cache.lost_timeout.set(None);
            }
        }
        let indexed = indexed(conn)?;
        if !conn.is_autocommit() {
            // Inside a caller's transaction. The copy answers when its key is
            // the generation visible here: every key it holds was read
            // committed, and a transaction that changed memory sees a
            // generation no committed read has reached yet, so an equal key
            // means this snapshot's memory is the copy's. Otherwise the
            // vectors are computed for this call and handed back, never kept
            // — the transaction may roll back, and another commit then reach
            // the same generation with other memory (ABA) — and the warm copy
            // is left as it was for the next search.
            let key = read_key(conn, indexed)?;
            if cache.key == Some(key) && cache.complete {
                return Ok(Reconciled::Copy);
            }
            let mut entries = load(conn, indexed)?;
            let (pending, _) = scan_missing(conn, indexed, &mut HashMap::new())?;
            absorb_into(&mut entries, &pending);
            return Ok(Reconciled::ThisCall(entries));
        }
        // Read-only, or a store without the index: vectors are kept in the
        // copy only, with no texts waiting for a write that cannot happen
        // and no last-used bookkeeping.
        let writable = indexed && !conn.is_readonly("main").unwrap_or(true);
        // The generation, the stored vectors and the missing entries are read
        // in ONE read transaction: a consistent snapshot, so they agree by
        // construction however many writers commit meanwhile. The copy is
        // keyed on the generation read inside it. A writer that commits after
        // the snapshot moves the generation, so the next search reloads.
        {
            let snapshot = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Deferred,
            )?;
            let key = read_key(&snapshot, indexed)?;
            if cache.key != Some(key) {
                // What was embedded and not yet stored is kept: the scan
                // below reuses each one whose text is unchanged.
                cache.reset_copy();
                cache.entries = load(&snapshot, indexed)?;
                #[cfg(test)]
                {
                    cache.loads += 1;
                }
                cache.key = Some(key);
            }
            if !cache.complete {
                let mut reuse: HashMap<(Kind, String), Pending> =
                    std::mem::take(&mut cache.unpersisted)
                        .into_iter()
                        .map(|p| ((p.kind, p.owner_id.clone()), p))
                        .collect();
                let (pending, _embedded) = scan_missing(&snapshot, indexed, &mut reuse)?;
                #[cfg(test)]
                {
                    cache.embedded += _embedded;
                    if let Some(hook) = cache.after_scan.as_mut() {
                        hook();
                    }
                }
                cache.absorb_pending(&pending);
                if writable {
                    cache.unpersisted = pending;
                }
                cache.complete = true;
            }
            snapshot.commit()?;
        }
        if !writable {
            return Ok(Reconciled::Copy);
        }
        let (touch, seen) = due_touch(conn, cache.touched)?;
        if seen.is_some() {
            cache.touched = seen;
        }
        if cache.unpersisted.is_empty() && touch.is_none() {
            return Ok(Reconciled::Copy);
        }
        // Written after the snapshot, each entry only while its row still
        // holds the text that was embedded. One the guard refuses changed
        // after the snapshot, which moved the generation: the next search
        // reloads and embeds its new text.
        match persist(conn, &cache.unpersisted, touch, &cache.lost_timeout) {
            Ok(_) => {
                cache.unpersisted.clear();
                if touch.is_some() {
                    cache.touched = touch;
                }
            }
            // Kept: the next search retries the write.
            Err(NotPersisted::Skipped) => {}
            Err(NotPersisted::Failed(e)) => absorb(&e),
        }
        Ok(Reconciled::Copy)
    }

    /// D196's semantic list: every entry in `filter` whose best chunk's
    /// similarity to `query`, rounded to three decimals, is at or above
    /// `filter.min_similarity`, best first, then by `(kind, id)`; the first
    /// `filter.depth` of them, and how many there were. Reconciles the
    /// index first. Empty when the query has no known token.
    #[cfg(test)]
    pub(crate) fn semantic_candidates(
        &self,
        query: &str,
        filter: &SemanticFilter<'_>,
    ) -> SemanticHits {
        let Some(q) = embed::embed(query) else {
            return SemanticHits::default();
        };
        let all = SemanticFilter {
            depth: usize::MAX,
            ..filter.clone()
        };
        let mut hits = self.semantic_pass(&QueryVector::new(&q), &all).hits;
        let total = hits.len();
        hits.truncate(filter.depth);
        SemanticHits { hits, total }
    }

    /// [`Self::semantic_candidates`] for a query already embedded, which
    /// also says how many entries in the filter had a vector to compare:
    /// none means the semantic side had nothing to answer from (nothing
    /// indexed, or a reconcile that failed), which is not the
    /// same as comparing and finding nothing close.
    pub(crate) fn semantic_pass(
        &self,
        q: &QueryVector,
        filter: &SemanticFilter<'_>,
    ) -> SemanticPass {
        let reconciled = self.reconcile_vectors();
        let cache = self.vectors.borrow();
        let entries: &[Entry] = match &reconciled {
            Reconciled::Copy => &cache.entries,
            Reconciled::ThisCall(e) => e,
            Reconciled::Failed => &[],
        };
        let mut compared = 0;
        let mut hits: Vec<SemanticHit> = entries
            .iter()
            .filter(|e| in_filter(e, filter))
            .filter_map(|e| {
                compared += 1;
                let (chunk, similarity) = best(e, q);
                (similarity >= filter.min_similarity).then(|| SemanticHit {
                    kind: e.kind,
                    owner_id: e.owner_id.clone(),
                    chunk,
                    similarity,
                })
            })
            .collect();
        drop(cache);
        hits.sort_by(|a, b| {
            b.similarity
                .total_cmp(&a.similarity)
                .then(a.kind.cmp(&b.kind))
                .then_with(|| a.owner_id.cmp(&b.owner_id))
        });
        hits.truncate(filter.depth);
        SemanticPass {
            hits,
            compared,
            this_call: match reconciled {
                Reconciled::ThisCall(e) => Some(e),
                _ => None,
            },
        }
    }

    /// The rounded similarity to `q` of each of `keys` that has a vector in
    /// the copy, below the floor or not: what a hit the words found says
    /// about its meaning. Reads the copy as the last pass left it, without
    /// reconciling again.
    pub(crate) fn semantic_similarities<'k>(
        &self,
        q: &QueryVector,
        keys: &HashSet<(Kind, &'k str)>,
        pass: &SemanticPass,
    ) -> HashMap<(Kind, &'k str), f64> {
        // Keyed by position so a lookup with the copy's own `&str` finds a
        // caller's key without cloning either.
        let wanted: Vec<(Kind, &'k str)> = keys.iter().copied().collect();
        let index: HashMap<(Kind, &str), usize> =
            wanted.iter().enumerate().map(|(i, k)| (*k, i)).collect();
        let cache = self.vectors.borrow();
        let entries: &[Entry] = pass.this_call.as_deref().unwrap_or(&cache.entries);
        entries
            .iter()
            .filter_map(|e| {
                let i = *index.get(&(e.kind, e.owner_id.as_str()))?;
                Some((wanted[i], best(e, q).1))
            })
            .collect()
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
