//! The D41 memory subsystem: lexical retrieval over docs and annotations.
//!
//! `memory.search` promises ranked hits, not a ranking algorithm — the FTS5
//! backend is an implementation detail behind a retrieval-agnostic wire shape,
//! so a semantic backend can slot in later without an API change.

use super::*;

/// #229 item 4: validate a memory `id`'s SHAPE before the store is ever
/// asked about it — mirroring `resolve_ref_value_on`'s `bad_request` for a
/// task ref that cannot possibly be a short_id or a UUID. Every memory id
/// this engine ever mints is a UUID (`memory_add`'s `clock::uuid_v7()`), so a
/// string that fails to parse as one is a malformed request, not a store
/// miss — the two must not share one exit code (DESIGN.md's `2`
/// bad_request vs `4` not_found contract exists so a script can branch
/// without parsing JSON).
fn require_uuid_shape(id: &str) -> Result<(), ApiError> {
    if Uuid::parse_str(id).is_ok() {
        return Ok(());
    }
    Err(ApiError::bad_request(format!(
        "memory id is not a UUID: {id} — expected a UUID like the one memory.add returns"
    )))
}

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

/// The FTS5 columns `--raw` can name a `column:query` against, for the given
/// `scope`. Kept beside [`MEMORY_SCOPES`]'s table rather than derived from
/// `docs_fts`/`annotations_fts` at query time — sqlite's own column
/// introspection would need a second round trip for an error path that is
/// already reporting a failure.
fn fts5_columns_for(scope: &str) -> &'static str {
    match scope {
        // D135: `docs_fts.body` reads the derived `docs.search_body` through
        // the `docs_search` view, but keeps the column name D41 published.
        "docs" => "title, body",
        "annotations" => "body",
        _ => "title, body (docs) or body (annotations)",
    }
}

/// #228.5: wrap a raw-mode FTS5 failure so it teaches instead of just
/// refusing. `--raw`'s own help advertises "columns" as part of the syntax it
/// hands the caller, so `no such column: title` must say which ones this
/// scope actually has — the same standard every other tasqx error holds
/// (`unknown scope`, `unknown setting`, `invalid priority`, …) — rather than
/// stopping at sqlite's own bare message, which also has no obligation to
/// stay legible outside its own error taxonomy.
fn raw_fts5_error(e: &rusqlite::Error, scope: &str) -> String {
    let msg = e.to_string();
    if let Some(col) = msg
        .rsplit("no such column: ")
        .next()
        .filter(|_| msg.contains("no such column: "))
    {
        return format!(
            "invalid FTS5 query: no such column {col:?} (searchable columns for \
             scope {scope:?}: {})",
            fts5_columns_for(scope)
        );
    }
    format!("invalid FTS5 query: {msg}")
}

impl Engine {
    // ---- memory.add ----------------------------------------------------------

    /// `memory.add` — store one knowledge document. Params: `title`, `body`,
    /// optional `source`, optional `project` (#134). Returns its new id.
    ///
    /// A doc is standalone, not attached to a task: annotations already cover
    /// "a note about this task", and [`Entity::Doc`] exists so the two stay
    /// distinguishable in the event log.
    ///
    /// `project` is free-standing scoping, not a foreign key onto
    /// `projects`: a doc worth keeping can outlive the project it was
    /// written about, or apply to none at all. Omitting it leaves the doc
    /// global/unscoped rather than defaulting it onto whatever project is
    /// current — the same reasoning `task.add`'s `default_project` explicitly
    /// does NOT extend to memory.
    ///
    /// Task #12/D135: `body` is stored exactly as given — `memory.get`,
    /// `memory show` and `store.export` all see it byte for byte, and a
    /// `store.export`/`store.import` round-trip stays lossless. What a
    /// leading YAML frontmatter block gets is a SEPARATE, derived
    /// `search_body` ([`crate::frontmatter::flatten`]), read only by
    /// `docs_fts` (see `crate::storage::migrate_memory`) — so `memory.add`
    /// is the one write door every client shares (CLI, MCP's
    /// `tasqx_add_memory`, `memory.import`'s per-doc insert) is also the one
    /// place that keeps the index in step with the body, without the body
    /// itself ever losing a byte of what was written.
    pub fn memory_add(&self, p: &Value) -> Result<Value, ApiError> {
        let title = req_str(p, "title")?;
        let body = req_str(p, "body")?;
        let search_body = crate::frontmatter::flatten(&body).into_owned();
        let source = opt_str(p, "source")?;
        let project = opt_str_nonempty(p, "project")?;

        let id = crate::clock::uuid_v7().to_string();
        let ts = now();
        let tx = self.begin_mutation()?;
        tx.execute(
            "INSERT INTO docs (id, source, title, body, search_body, project, created, modified) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, source, title, body, search_body, project, ts],
        )?;
        insert_event(
            &tx,
            Entity::Doc,
            &id,
            "memory.add",
            &json!({ "title": title, "source": source, "project": project }),
        )?;
        tx.commit()?;

        Ok(json!({ "id": id, "title": title, "project": project, "created": ts }))
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
        let mut replaced = 0i64;
        for dv in docs {
            let dv = import_shape("", "doc", dv)?;
            import_keys("", "doc", dv, &["title", "body", "source"])?;
            let title = req_str(dv, "title")?;
            // The CLI's own importer already cuts frontmatter before it ever
            // reaches this call (#228.4's throwaway agent-memory metadata),
            // so `search_body` equals `body` there; it only matters for a
            // caller on the JSON API directly, which gets `memory.add`'s same
            // index guarantee without `body` itself being touched.
            let body = req_str(dv, "body")?;
            let search_body = crate::frontmatter::flatten(&body).into_owned();
            let source = opt_str_nonempty(dv, "source")?;
            // A doc whose `source` matches an existing row is a RE-IMPORT of
            // the same logical document (a directory re-run after an edit),
            // not a new one — so it UPDATES that row rather than deleting and
            // re-minting (#178/#198). The DELETE-then-INSERT this replaces
            // always struck a fresh UUIDv7, so `memory show <id>`, an
            // annotation citing the doc, and MCP's `tasqx_get_memory` all
            // 404'd the moment ANY re-run happened — announced nowhere, and
            // at scale on a real store one source had been silently
            // overwritten 34 times.
            let existing: Option<(String, i64)> = match &source {
                Some(src) => tx
                    .query_row(
                        "SELECT id, rev FROM docs WHERE source = ?1",
                        params![src],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?,
                None => None,
            };
            let is_replace = existing.is_some();
            let (id, rev) = match existing {
                Some((id, cur_rev)) => (id, cur_rev + 1),
                None => (crate::clock::uuid_v7().to_string(), 0),
            };
            // ON CONFLICT DO UPDATE, never DELETE+INSERT (D41's own rule,
            // learned the hard way for the annotation upsert): a DELETE does
            // not fire `docs_fts`'s delete trigger for free, and re-doing it
            // by hand here would be a second copy of the exact bug that rule
            // exists to prevent. The UPDATE path fires `docs_fts_au` and
            // keeps the index honest. `created` is deliberately absent from
            // the SET list, so a source-replace keeps the ORIGINAL creation
            // date rather than pretending the doc is new.
            //
            // `rev` is in the SET list because a source-replace is a revision
            // of the same document (D143): a `memory.update` still holding
            // the pre-import `expected_rev` must conflict, not clobber the
            // import. The value is the looked-up row's rev + 1, computed here
            // the way `memory_update` and `store.import` do it; the lookup
            // and the upsert sit in one IMMEDIATE transaction
            // (`begin_mutation`), so nothing can move the row between them.
            tx.execute(
                "INSERT INTO docs (id, source, title, body, search_body, rev, created, modified) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) \
                 ON CONFLICT(id) DO UPDATE SET \
                 source=excluded.source, title=excluded.title, body=excluded.body, \
                 search_body=excluded.search_body, modified=excluded.modified, rev=excluded.rev",
                params![id, source, title, body, search_body, rev, ts],
            )?;
            insert_event(
                &tx,
                Entity::Doc,
                &id,
                "memory.add",
                &json!({
                    "title": title,
                    "source": source,
                    "via": "memory.import",
                    "replaced": is_replace,
                    "rev": rev,
                }),
            )?;
            if is_replace {
                replaced += 1;
            }
            out.push(json!({
                "id": id,
                "title": title,
                "source": source,
                "replaced": is_replace,
                "_rev": rev,
            }));
        }
        tx.commit()?;

        Ok(json!({ "imported": out.len(), "replaced": replaced, "docs": out }))
    }

    // ---- memory.search -------------------------------------------------------

    /// `memory.search` — ranked lexical retrieval over docs and annotations.
    /// Params: `query`, `limit` (default 10), `scope` (one of [`MEMORY_SCOPES`],
    /// default `all`), `raw`, optional `project` (#134).
    ///
    /// `raw:false` (the default) escapes the query into FTS5 phrases, so
    /// ordinary text containing `-` or `:` is a search rather than a syntax
    /// error. `raw:true` hands the FTS5 operator grammar to the caller, who then
    /// owns its errors — which is why a refused raw query is `bad_request` and
    /// not `internal`.
    ///
    /// #132: `limit` truncates silently no longer. `total` is the count of
    /// every row the MATCH (+ `project`, if given) found, before the window —
    /// the same relation `task.list`'s `count`/`total` hold — and `has_more`
    /// is that comparison already done for a caller that only wants a
    /// boolean. Both cost one extra `COUNT(*)` query, run against the exact
    /// same WHERE clauses as the page itself, so the two numbers can never
    /// name a different match set than the hits do.
    pub fn memory_search(&self, p: &Value) -> Result<Value, ApiError> {
        let query = req_str(p, "query")?;
        let raw = opt_bool(p, "raw")?.unwrap_or(false);
        // Checked, not `as i64`: a value above i64::MAX wrapped negative, and
        // SQLite reads a negative LIMIT as UNLIMITED — the exact opposite of
        // the bound the caller asked for (review finding).
        let limit =
            i64::try_from(opt_u64(p, "limit")?.unwrap_or(crate::engine::MEMORY_SEARCH_LIMIT))
                .map_err(|_| {
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
        let project = opt_str_nonempty(p, "project")?;
        // D136: a document with no project is GLOBAL knowledge — `tasqx memory
        // import docs/` sets no project on anything it imports, so a strict
        // project scope hides every ADR a reader fed the store. Opt-in and
        // additive, because D115's strict scope stays the right answer when a
        // caller is asking about one project's own notes.
        let include_unscoped = opt_bool(p, "include_unscoped")?.unwrap_or(false);
        if include_unscoped && project.is_none() {
            // D33: a value that changes nothing is refused rather than
            // accepted and ignored. With no project there is nothing to widen
            // FROM — an unscoped search already returns every document — and a
            // caller who sent this believes a scope is being applied.
            return Err(ApiError::bad_request(
                "`include_unscoped` widens a `project` scope to documents that have no \
                 project, so it needs a `project` to widen from",
            ));
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
        // Named params (`:match`/`:project`/`:limit`), not positional: the
        // count query below reuses these same two arms without `:limit`, and
        // named binding is what lets the arm text stay identical between the
        // two statements instead of hand-renumbering `?1`/`?2` per query.
        const DOCS_ARM: &str = "SELECT d.id AS id, 'doc' AS kind, d.title AS title, \
             d.source AS source, snippet(docs_fts, 1, '', '', '…', 12) AS snip, \
             bm25(docs_fts) AS score \
             FROM docs_fts JOIN docs d ON d.rowid = docs_fts.rowid \
             WHERE docs_fts MATCH :match";
        const ANN_ARM: &str = "SELECT a.id AS id, 'annotation' AS kind, t.title AS title, \
             'task:#' || t.short_id AS source, \
             snippet(annotations_fts, 0, '', '', '…', 12) AS snip, \
             bm25(annotations_fts) AS score \
             FROM annotations_fts \
             JOIN annotations a ON a.rowid = annotations_fts.rowid \
             JOIN tasks t ON t.id = a.task_id \
             WHERE annotations_fts MATCH :match";
        // #134: a doc's own `project` column vs. its task's `project` for an
        // annotation — the same "docs carry it directly, annotations inherit
        // it from their task" split `memory_add`'s doc column and the
        // pre-existing `task:#` source already draw.
        let (docs_arm, ann_arm) = if project.is_some() {
            // `IS NULL`, not `IS NOT :project`: the widening admits documents
            // belonging to NO project, never documents belonging to another
            // one. An annotation inherits its task's project — the same split
            // `memory_add`'s doc column and the `task:#` source already draw.
            let (d, a) = if include_unscoped {
                (
                    "AND (d.project = :project OR d.project IS NULL)",
                    "AND (t.project = :project OR t.project IS NULL)",
                )
            } else {
                ("AND d.project = :project", "AND t.project = :project")
            };
            (format!("{DOCS_ARM} {d}"), format!("{ANN_ARM} {a}"))
        } else {
            (DOCS_ARM.to_string(), ANN_ARM.to_string())
        };
        let matched_sql = match scope.as_str() {
            "docs" => docs_arm.clone(),
            "annotations" => ann_arm.clone(),
            _ => format!("{docs_arm} UNION ALL {ann_arm}"),
        };
        let sql = format!("{matched_sql} ORDER BY score LIMIT :limit");
        let count_sql = format!("SELECT COUNT(*) FROM ({matched_sql})");

        let named: Vec<(&str, &dyn rusqlite::ToSql)> = match &project {
            Some(proj) => vec![(":match", &match_expr), (":project", proj)],
            None => vec![(":match", &match_expr)],
        };

        let run = || -> Result<Vec<Value>, rusqlite::Error> {
            let mut stmt = self.conn.prepare(&sql)?;
            let mut all_params = named.clone();
            all_params.push((":limit", &limit));
            let rows = stmt.query_map(all_params.as_slice(), |r| {
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
            // own message, never as ok-empty and never as `internal`. #228.5:
            // `--raw`'s own help advertises "columns", so a `col:query` typo
            // that names a column this scope does not have must say which
            // ones exist, the same way every other tasqx error names the
            // valid set (`unknown scope`, three lines up, does this already)
            // rather than stopping at SQLite's bare `no such column: X`.
            Err(e) if raw => {
                return Err(ApiError::bad_request(raw_fts5_error(&e, &scope)));
            }
            Err(e) => return Err(e.into()),
        };
        // Only counted once the page query above has already proved the MATCH
        // expression itself is valid — a raw syntax error is reported once,
        // by `run()`, not doubled by this second statement failing the same
        // way.
        let total: i64 = self
            .conn
            .query_row(&count_sql, named.as_slice(), |r| r.get(0))?;

        Ok(json!({
            "count": hits.len(),
            "total": total,
            "has_more": (hits.len() as i64) < total,
            "hits": hits,
            "matched": match_expr,
        }))
    }

    // ---- memory.get ----------------------------------------------------------

    /// `memory.get` — read one knowledge document whole, by the `id`
    /// `memory.search` printed.
    ///
    /// The subsystem could store prose and could not hand it back. A search hit
    /// carries a `snippet()` excerpt — measured at 60 to 88 characters on real
    /// documents — plus an id that no verb accepted, so a doc written over MCP
    /// in the morning was, by the afternoon, findable and unreadable. Its
    /// author had no route to it at all; a reader on the same machine had
    /// `store.export`, which dumps every task, project and document in the
    /// store to recover one of them, and which `UNEXPOSED_METHODS` withholds
    /// from MCP for exactly that size. That table also already concedes the
    /// client is elsewhere — it withholds `memory.import` because "the
    /// filesystem the CLI reads is not the one an MCP client is on".
    ///
    /// This is D64's asymmetry one noun over. That entry fixed writing without
    /// retracting; writing without reading was the other half, and a retrieval
    /// surface whose whole value is that what it returns can be trusted is
    /// worth nothing if what it returns is 8% of the document.
    ///
    /// **Docs only.** An annotation id is refused with the route that does
    /// work rather than served here: annotations already have a read path
    /// (`memory.search` names their task as `task:#<short_id>`, and `task.get`
    /// returns the bodies), and a second spelling of a reachable behaviour is
    /// the drift D30 warns about. The refusal names `task.get`, because an id
    /// that is real and rejected with a bare "not found" is the worst of both.
    pub fn memory_get(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        require_uuid_shape(&id)?;
        let found = self
            .conn
            .query_row(
                "SELECT id, source, title, body, created, modified, project, rev \
                 FROM docs WHERE id = ?1",
                params![id],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "source": r.get::<_, Option<String>>(1)?,
                        "title": r.get::<_, String>(2)?,
                        "body": r.get::<_, String>(3)?,
                        "created": r.get::<_, String>(4)?,
                        "modified": r.get::<_, String>(5)?,
                        "project": r.get::<_, Option<String>>(6)?,
                        "_rev": r.get::<_, i64>(7)?,
                    }))
                },
            )
            .optional()?;
        if let Some(doc) = found {
            return Ok(doc);
        }
        // The id might be a real annotation, which `memory.search` returns
        // alongside docs and which this method deliberately does not serve.
        let annotation_task: Option<i64> = self
            .conn
            .query_row(
                "SELECT t.short_id FROM annotations a JOIN tasks t ON t.id = a.task_id \
                 WHERE a.id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Err(match annotation_task {
            Some(short_id) => ApiError::not_found(
                format!(
                    "{id} is an annotation on task #{short_id}, not a memory doc — \
                     read it with task.get on #{short_id}"
                ),
                Some(json!({ "id": id, "task": short_id })),
            ),
            None => ApiError::not_found(format!("no memory doc with id {id}"), None),
        })
    }

    // ---- memory.remove -------------------------------------------------------

    /// `memory.remove` — delete one doc by `id`. An id that matches nothing is
    /// `not_found`, not a silent no-op: "I deleted it" and "there was nothing
    /// there" are different answers to the caller.
    pub fn memory_remove(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        require_uuid_shape(&id)?;
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

    // ---- memory.list -----------------------------------------------------

    /// `memory.list` — browse memory docs without already knowing a literal
    /// word inside one (#133). Params: `limit`, `offset`, optional `project`
    /// (#134). Newest-modified-first by default — the recency a caller
    /// browsing "what's in here" wants, and the same axis `memory.update`
    /// moves a doc along.
    ///
    /// Same `{count, total, next_offset}` shape as `task.list` (D70):
    /// `total` is matched rows before the window, `next_offset` is the value
    /// to pass back to keep walking and is `null` once nothing is left. This
    /// is the browse counterpart to `memory.search` — that one requires a
    /// query, this one requires none.
    pub fn memory_list(&self, p: &Value) -> Result<Value, ApiError> {
        let project = opt_str_nonempty(p, "project")?;
        let offset = opt_u64(p, "offset")?.unwrap_or(0);
        let offset_i64 = i64::try_from(offset).map_err(|_| {
            ApiError::bad_request(format!(
                "`offset` must be at most {}, or omitted for the default",
                i64::MAX
            ))
        })?;
        let limit = opt_u64(p, "limit")?;
        let limit_i64 = match limit {
            Some(n) => Some(i64::try_from(n).map_err(|_| {
                ApiError::bad_request(format!(
                    "`limit` must be at most {}, or omitted for the default",
                    i64::MAX
                ))
            })?),
            None => None,
        };

        let where_clause = if project.is_some() {
            "WHERE project = :project"
        } else {
            ""
        };
        let count_sql = format!("SELECT COUNT(*) FROM docs {where_clause}");
        // Newest-modified first (D115 #133), `id DESC` as the tiebreak two
        // docs written the same instant still need so they list in one
        // deterministic order (UUIDv7 ids sort chronologically, so it is
        // also a secondary recency signal, not an arbitrary one) — the same
        // reasoning `task.list`'s `compare_by` always ends on a tiebreak.
        // It settles tie order only: a doc updated between two OFFSET pages
        // still moves across the boundary, as on any offset-paged list.
        //
        // The key is `modified` with its `Z` stripped, not the column itself
        // (D144). `util::now` prints a variable-length fractional second —
        // trailing zeros trimmed, absent at a whole second — and under
        // BINARY collation `'Z'` sorts above `'.'` and every digit, so
        // `...10Z` > `...10.9Z` and `...10.12Z` > `...10.123Z`: an older doc
        // above a newer one. Without the terminator a trimmed decimal
        // fraction compares correctly as a plain prefix string (`10` <
        // `10.9`, `10.12` < `10.123`). `id` alone cannot replace it, unlike
        // D142's annotations: `modified` moves on `memory.update` and on an
        // import that replaces by source, and this list promises to show
        // that.
        let row_sql = format!(
            "SELECT id, title, source, project, created, modified, rev, \
                    substr(body, 1, 160) AS preview, length(body) AS body_len \
             FROM docs {where_clause} \
             ORDER BY rtrim(modified, 'Z') DESC, id DESC \
             LIMIT :limit OFFSET :offset"
        );

        let named: Vec<(&str, &dyn rusqlite::ToSql)> = match &project {
            Some(proj) => vec![(":project", proj)],
            None => vec![],
        };
        let total: i64 = self
            .conn
            .query_row(&count_sql, named.as_slice(), |r| r.get(0))?;

        let mut row_params = named.clone();
        // `LIMIT -1` is SQLite's own "unlimited" spelling, used because the
        // param binder needs a concrete value: `limit` omitted must return
        // everything from `offset` on, mirroring `task.list`'s `Option<u64>`
        // without a limit clause at all — sending 0 or a negative literal
        // instead would either answer nothing or (per `memory_search`'s own
        // comment above) answer everything for the wrong reason.
        let limit_bound: i64 = limit_i64.unwrap_or(-1);
        row_params.push((":limit", &limit_bound));
        row_params.push((":offset", &offset_i64));

        let mut stmt = self.conn.prepare(&row_sql)?;
        let rows = stmt.query_map(row_params.as_slice(), |r| {
            let body_len: i64 = r.get(8)?;
            let preview: String = r.get(7)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "source": r.get::<_, Option<String>>(2)?,
                "project": r.get::<_, Option<String>>(3)?,
                "created": r.get::<_, String>(4)?,
                "modified": r.get::<_, String>(5)?,
                "_rev": r.get::<_, i64>(6)?,
                "body_preview": preview,
                "body_truncated": body_len > 160,
            }))
        })?;
        let docs: Vec<Value> = rows.collect::<Result<_, _>>()?;

        let next_offset = if limit_i64 == Some(0) {
            Value::Null
        } else {
            match offset + docs.len() as u64 {
                reached if (reached as i64) < total => json!(reached),
                _ => Value::Null,
            }
        };

        Ok(json!({
            "count": docs.len(),
            "total": total,
            "next_offset": next_offset,
            "docs": docs,
        }))
    }

    // ---- memory.update -----------------------------------------------------

    /// `memory.update` — replace a doc's `title`/`body` in place (#135).
    /// Params: `id`, optional `title`, optional `body`, optional `source`,
    /// optional `project`, optional `expected_rev`. At least one of
    /// `title`/`body`/`source`/`project` must be given.
    ///
    /// `memory.remove` is genuinely permanent — no `undo`, nothing in the
    /// event log to reconstruct the body from — so a correction had only two
    /// routes: add a second doc (the stale one still pollutes every later
    /// search) or delete-then-recreate (loses the id and the revision
    /// history, and risks the permanent half landing with nothing to follow
    /// it). This is the missing third route: an in-place UPDATE, so the id
    /// stays stable for anything that already cited it.
    ///
    /// `expected_rev` is `task.modify`'s optimistic-concurrency guard,
    /// unchanged: supplied and mismatched is a `conflict` naming both
    /// revs, never a silent overwrite.
    pub fn memory_update(&self, p: &Value) -> Result<Value, ApiError> {
        let id = req_str(p, "id")?;
        require_uuid_shape(&id)?;
        let title = opt_str_nonempty(p, "title")?;
        // Task #12/D135: `body` is stored as given, same as `memory.add` —
        // only `search_body` (below) is derived from it, so a doc corrected
        // to open with a fresh frontmatter block gets the index kept in
        // step without the stored body ever being rewritten.
        let body = opt_str_nonempty(p, "body")?;
        let source = opt_str(p, "source")?;
        let project = opt_str_nonempty(p, "project")?;
        if title.is_none() && body.is_none() && source.is_none() && project.is_none() {
            return Err(ApiError::bad_request(
                "memory.update requires at least one of `title`, `body`, `source`, `project`",
            ));
        }
        let expected_rev = opt_i64(p, "expected_rev")?;

        let tx = self.begin_mutation()?;
        let current = tx
            .query_row(
                "SELECT title, body, source, project, rev FROM docs WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((cur_title, cur_body, cur_source, cur_project, cur_rev)) = current else {
            return Err(ApiError::not_found(
                format!("no memory doc with id {id}"),
                None,
            ));
        };

        if let Some(exp) = expected_rev {
            if exp != cur_rev {
                return Err(ApiError::new(
                    crate::ErrorCode::Conflict,
                    format!(
                        "expected_rev {exp} but doc is at rev {cur_rev}: re-read it with \
                         tasqx_get_memory and retry with expected_rev {cur_rev}"
                    ),
                    Some(json!({ "expected": exp, "current": cur_rev, "id": id })),
                ));
            }
        }

        let new_title = title.clone().unwrap_or(cur_title);
        let new_body = body.clone().unwrap_or(cur_body);
        let new_search_body = crate::frontmatter::flatten(&new_body).into_owned();
        let new_source = source.clone().or(cur_source);
        let new_project = project.clone().or(cur_project);
        let new_rev = cur_rev + 1;
        let ts = now();

        tx.execute(
            "UPDATE docs SET title = ?1, body = ?2, search_body = ?3, source = ?4, \
             project = ?5, rev = ?6, modified = ?7 WHERE id = ?8",
            params![
                new_title,
                new_body,
                new_search_body,
                new_source,
                new_project,
                new_rev,
                ts,
                id
            ],
        )?;
        insert_event(
            &tx,
            Entity::Doc,
            &id,
            "memory.update",
            &json!({
                "title": &new_title,
                "source": &new_source,
                "project": &new_project,
                "rev": new_rev,
            }),
        )?;
        tx.commit()?;

        Ok(json!({
            "id": id,
            "title": new_title,
            "source": new_source,
            "project": new_project,
            "_rev": new_rev,
            "modified": ts,
        }))
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;
    use serde_json::json;

    /// #229 item 4: a malformed identifier was `bad_request` for a task
    /// `ref` (`resolve_ref_value_on`) but `not_found` for a memory id — a
    /// script that retries on `not_found` (transient/wrong id) and gives up
    /// on `bad_request` (malformed input) got the opposite treatment
    /// depending only on which entity it asked about, contradicting
    /// DESIGN.md's stated exit-code-per-class contract. `notauuid` cannot be
    /// a UUID, so it is the same failure as a malformed task ref.
    #[test]
    fn a_malformed_memory_id_is_bad_request_not_not_found() {
        let e = crate::Engine::open_in_memory().unwrap();

        let get_err = e
            .memory_get(&json!({ "id": "notauuid" }))
            .expect_err("a non-UUID id must be refused");
        assert_eq!(
            get_err.code,
            crate::ErrorCode::BadRequest,
            "shape is wrong, not merely absent: {}",
            get_err.message
        );
        assert!(
            get_err.message.contains("UUID"),
            "must say what shape was expected: {}",
            get_err.message
        );

        let rm_err = e
            .memory_remove(&json!({ "id": "notauuid" }))
            .expect_err("a non-UUID id must be refused");
        assert_eq!(
            rm_err.code,
            crate::ErrorCode::BadRequest,
            "{}",
            rm_err.message
        );

        // A well-formed but nonexistent UUID is still genuinely not_found —
        // only the SHAPE check moved, not the "no such doc" answer.
        let real_uuid = crate::clock::uuid_v7().to_string();
        let missing = e.memory_get(&json!({ "id": real_uuid })).unwrap_err();
        assert_eq!(
            missing.code,
            crate::ErrorCode::NotFound,
            "{}",
            missing.message
        );
    }

    /// `n` docs titled `doc {i}` with body `body {i}`, returned as ids in
    /// write order — UUIDv7, so id order is write order.
    fn add_docs(e: &crate::Engine, n: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for i in 0..n {
            let out = e
                .memory_add(&json!({ "title": format!("doc {i}"), "body": format!("body {i}") }))
                .unwrap();
            ids.push(out["id"].as_str().unwrap().to_string());
        }
        ids
    }

    /// Stamp each `id` with its paired `modified` text directly — writing it
    /// this way is the only way to reproduce a specific spelling without
    /// racing a clock.
    fn stamp_docs(e: &crate::Engine, ids: &[String], stamps: &[&str]) {
        assert_eq!(ids.len(), stamps.len());
        for (i, (id, ts)) in ids.iter().zip(stamps.iter()).enumerate() {
            let n = e
                .conn
                .execute(
                    "UPDATE docs SET modified = ?1 WHERE id = ?2",
                    params![ts, id],
                )
                .unwrap();
            assert_eq!(n, 1, "doc {i} must exist exactly once to be stamped");
        }
    }

    /// The `docs` array's `id` strings, in order.
    fn listed_ids(out: &serde_json::Value) -> Vec<String> {
        out["docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// memory.list is newest-modified first, and the variable-length `modified`
    /// text may not decide it (D142's defect, on docs).
    ///
    /// `util::now` prints a jiff Timestamp whose fractional second is trimmed —
    /// entirely absent on a whole second — and under SQLite's BINARY collation
    /// 'Z' (0x5A) sorts above '.' and above every digit, so `...10Z` >
    /// `...10.1Z` > `...10.12Z` > `...10.123Z` while each is later than the
    /// last. Ordering by that text put a doc stamped on a whole second above
    /// every doc written after it in the same second; the paging test in
    /// tests/memory.rs failed once under load for exactly this reason.
    #[test]
    fn memory_list_orders_by_modified_instant_not_the_variable_length_text() {
        let e = crate::Engine::open_in_memory().unwrap();
        let ids = add_docs(&e, 4);

        // Chronological, and deliberately fully INVERTED in BINARY text
        // order: 'Z' beats '1', '2' and '3'.
        let stamps = [
            "2026-01-01T00:00:10Z",
            "2026-01-01T00:00:10.1Z",
            "2026-01-01T00:00:10.12Z",
            "2026-01-01T00:00:10.123Z",
        ];
        stamp_docs(&e, &ids, &stamps);

        let out = e.memory_list(&json!({})).unwrap();
        let listed = listed_ids(&out);
        let expected: Vec<String> = ids.iter().rev().cloned().collect();
        assert_eq!(
            listed, expected,
            "the newest-modified doc must come first regardless of how its stamp's text sorts"
        );

        let page = e.memory_list(&json!({ "limit": 1 })).unwrap();
        let page_ids = listed_ids(&page);
        assert_eq!(
            page_ids,
            vec![ids[3].clone()],
            "the first page must hold the doc modified last, not the whole-second one"
        );
    }

    /// memory.list is newest-MODIFIED first, not newest-created (D115 #133,
    /// D144): a `memory.update` moves a doc to the top even though its UUIDv7
    /// id stays where it was minted. Ordering by `id` alone would read as
    /// correct on every fresh store and only lie once a doc is edited.
    #[test]
    fn memory_list_puts_an_updated_doc_first_even_though_its_id_is_the_oldest() {
        let e = crate::Engine::open_in_memory().unwrap();
        let ids = add_docs(&e, 3);

        // Strictly increasing, and in write order, so the baseline (before
        // the update below) is unambiguous regardless of how fast the
        // machine that runs this test is.
        let stamps = [
            "2026-01-01T00:00:10Z",
            "2026-01-01T00:00:11Z",
            "2026-01-01T00:00:12Z",
        ];
        stamp_docs(&e, &ids, &stamps);

        // Update the OLDEST id (doc 0) — memory.update sets util::now(),
        // which is later than the crafted stamps, but this pins it
        // explicitly so the test does not depend on the wall clock.
        e.memory_update(&json!({ "id": ids[0], "body": "edited" }))
            .unwrap();
        stamp_docs(&e, &ids[..1], &["2026-01-01T00:00:13Z"]);

        let out = e.memory_list(&json!({})).unwrap();
        let listed = listed_ids(&out);
        assert_eq!(
            listed,
            vec![ids[0].clone(), ids[2].clone(), ids[1].clone()],
            "the edited doc surfaces first; the rest stay newest-modified first"
        );

        let page = e.memory_list(&json!({ "limit": 1 })).unwrap();
        let page_ids = listed_ids(&page);
        assert_eq!(page_ids, vec![ids[0].clone()]);
    }
}
