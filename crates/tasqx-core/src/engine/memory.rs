//! The D41 memory subsystem: lexical retrieval over docs and annotations.
//!
//! `memory.search` promises ranked hits, not a ranking algorithm — the FTS5
//! backend is an implementation detail behind a retrieval-agnostic wire shape,
//! so a semantic backend can slot in later without an API change.

use super::*;

/// The closed `scope` vocabulary for `memory.search`. **First entry is the
/// default.** Source of truth in the [`SUMMARY_GROUP_BY`] sense: the engine
/// validates against it, builds its refusal from it, and the MCP tool schema
/// renders its JSON-Schema `enum` from it.
pub const MEMORY_SCOPES: [&str; 3] = ["all", "docs", "annotations"];

/// Escape a plain-text query into FTS5 phrase terms.
///
/// FTS5 treats `-`, `.`, `:` and quotes as query syntax, so the verified
/// failure this defuses is a user typing `server-side` and being answered
/// with `no such column: side`. Every whitespace-separated word becomes a
/// quoted phrase (embedded `"` doubled per FTS5's own escape rule), joined by
/// implicit AND. Callers who *want* the operator grammar pass `raw:true` and
/// own the syntax errors.
fn phrase_escape(query: &str) -> Result<String, ApiError> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return Err(ApiError::bad_request(
            "`query` must contain at least one word",
        ));
    }
    Ok(terms.join(" "))
}

impl Engine {
    // ---- memory.add ----------------------------------------------------------

    /// `memory.add` — store one knowledge document. Params: `title`, `body`,
    /// optional `source`. Returns its new id.
    ///
    /// A doc is standalone, not attached to a task: annotations already cover
    /// "a note about this task", and [`Entity::Doc`] exists so the two stay
    /// distinguishable in the event log.
    pub fn memory_add(&self, p: &Value) -> Result<Value, ApiError> {
        let title = req_str(p, "title")?;
        let body = req_str(p, "body")?;
        let source = opt_str(p, "source")?;

        let id = Uuid::now_v7().to_string();
        let ts = now();
        let tx = self.begin_mutation()?;
        tx.execute(
            "INSERT INTO docs (id, source, title, body, created, modified) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, source, title, body, ts],
        )?;
        insert_event(
            &tx,
            Entity::Doc,
            &id,
            "memory.add",
            &json!({ "title": title, "source": source }),
        )?;
        tx.commit()?;

        Ok(json!({ "id": id, "title": title, "created": ts }))
    }

    // ---- memory.import -------------------------------------------------------

    /// Bulk-load docs in ONE transaction with replace-by-source semantics.
    ///
    /// This exists because the CLI's first import looped over files calling
    /// `memory.add` per file (review finding): a mid-directory failure
    /// committed a silent partial import, and re-running the command
    /// duplicated every doc that had already landed. Here a bad entry rolls
    /// the whole batch back, and a doc whose `source` matches an existing one
    /// REPLACES it — re-importing an edited directory converges instead of
    /// accumulating.
    pub fn memory_import(&self, p: &Value) -> Result<Value, ApiError> {
        let docs = req_array(p, "docs").map_err(|e| {
            ApiError::bad_request(format!(
                "{} — memory.import requires a `docs` array",
                e.message
            ))
        })?;

        let ts = now();
        let tx = self.begin_mutation()?;
        let mut out = Vec::new();
        for dv in docs {
            let dv = import_shape("", "doc", dv)?;
            import_keys("", "doc", dv, &["title", "body", "source"])?;
            let title = req_str(dv, "title")?;
            let body = req_str(dv, "body")?;
            let source = opt_str_nonempty(dv, "source")?;
            if let Some(src) = &source {
                // Plain DELETE, so the delete trigger keeps the index honest.
                tx.execute("DELETE FROM docs WHERE source = ?1", params![src])?;
            }
            let id = Uuid::now_v7().to_string();
            tx.execute(
                "INSERT INTO docs (id, source, title, body, created, modified) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, source, title, body, ts],
            )?;
            insert_event(
                &tx,
                Entity::Doc,
                &id,
                "memory.add",
                &json!({ "title": title, "source": source, "via": "memory.import" }),
            )?;
            out.push(json!({ "id": id, "title": title, "source": source }));
        }
        tx.commit()?;

        Ok(json!({ "imported": out.len(), "docs": out }))
    }

    // ---- memory.search -------------------------------------------------------

    /// `memory.search` — ranked lexical retrieval over docs and annotations.
    /// Params: `query`, `limit` (default 10), `scope` (one of [`MEMORY_SCOPES`],
    /// default `all`), `raw`.
    ///
    /// `raw:false` (the default) escapes the query into FTS5 phrases, so
    /// ordinary text containing `-` or `:` is a search rather than a syntax
    /// error. `raw:true` hands the FTS5 operator grammar to the caller, who then
    /// owns its errors — which is why a refused raw query is `bad_request` and
    /// not `internal`.
    pub fn memory_search(&self, p: &Value) -> Result<Value, ApiError> {
        let query = req_str(p, "query")?;
        let raw = opt_bool(p, "raw")?.unwrap_or(false);
        // Checked, not `as i64`: a value above i64::MAX wrapped negative, and
        // SQLite reads a negative LIMIT as UNLIMITED — the exact opposite of
        // the bound the caller asked for (review finding).
        let limit = i64::try_from(opt_u64(p, "limit")?.unwrap_or(10)).map_err(|_| {
            ApiError::bad_request(format!(
                "`limit` must be at most {}, or omitted for the default",
                i64::MAX
            ))
        })?;
        let scope = opt_str(p, "scope")?.unwrap_or_else(|| MEMORY_SCOPES[0].to_string());
        if !MEMORY_SCOPES.contains(&scope.as_str()) {
            return Err(ApiError::bad_request(format!(
                "unknown scope `{scope}` — accepted: {}",
                MEMORY_SCOPES.join(", ")
            )));
        }
        // Echoed on the result (D69). Every word of a plain query becomes a
        // required quoted phrase, so a thirteen-word question is thirteen AND
        // terms and comes back `count: 0` — byte-identical to the answer for a
        // subject nobody ever wrote down. The caller could not tell those two
        // apart, and only one of them is worth retrying.
        let match_expr = if raw { query } else { phrase_escape(&query)? };

        // `bm25()` is aliased `score`, not `rank`: `rank` is a live column on
        // every FTS5 table and shadowing it inside a compound SELECT is asking
        // for a quiet resolution surprise. Lower bm25 = better, so ORDER BY ASC.
        const DOCS_ARM: &str = "SELECT d.id AS id, 'doc' AS kind, d.title AS title, \
             d.source AS source, snippet(docs_fts, 1, '', '', '…', 12) AS snip, \
             bm25(docs_fts) AS score \
             FROM docs_fts JOIN docs d ON d.rowid = docs_fts.rowid \
             WHERE docs_fts MATCH ?1";
        const ANN_ARM: &str = "SELECT a.id AS id, 'annotation' AS kind, t.title AS title, \
             'task:#' || t.short_id AS source, \
             snippet(annotations_fts, 0, '', '', '…', 12) AS snip, \
             bm25(annotations_fts) AS score \
             FROM annotations_fts \
             JOIN annotations a ON a.rowid = annotations_fts.rowid \
             JOIN tasks t ON t.id = a.task_id \
             WHERE annotations_fts MATCH ?1";
        let sql = match scope.as_str() {
            "docs" => format!("{DOCS_ARM} ORDER BY score LIMIT ?2"),
            "annotations" => format!("{ANN_ARM} ORDER BY score LIMIT ?2"),
            _ => format!("{DOCS_ARM} UNION ALL {ANN_ARM} ORDER BY score LIMIT ?2"),
        };

        let run = || -> Result<Vec<Value>, rusqlite::Error> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![match_expr, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "title": r.get::<_, String>(2)?,
                    "source": r.get::<_, Option<String>>(3)?,
                    "snippet": r.get::<_, String>(4)?,
                    "rank": r.get::<_, f64>(5)?,
                }))
            })?;
            rows.collect()
        };
        let hits = match run() {
            Ok(hits) => hits,
            // In raw mode the MATCH expression is caller input, so a query
            // SQLite refuses is the caller's error — surfaced with SQLite's
            // own message, never as ok-empty and never as `internal`.
            Err(e) if raw => return Err(ApiError::bad_request(format!("invalid FTS5 query: {e}"))),
            Err(e) => return Err(e.into()),
        };

        Ok(json!({ "count": hits.len(), "hits": hits, "matched": match_expr }))
    }

    // ---- memory.remove -------------------------------------------------------

    /// `memory.remove` — delete one doc by `id`. An id that matches nothing is
    /// `not_found`, not a silent no-op: "I deleted it" and "there was nothing
    /// there" are different answers to the caller.
    pub fn memory_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        let tx = self.begin_mutation()?;
        let n = tx.execute("DELETE FROM docs WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(ApiError::not_found(
                format!("no memory doc with id {id}"),
                None,
            ));
        }
        insert_event(&tx, Entity::Doc, &id, "memory.remove", &json!({}))?;
        tx.commit()?;
        Ok(json!({ "id": id, "removed": true }))
    }
}
