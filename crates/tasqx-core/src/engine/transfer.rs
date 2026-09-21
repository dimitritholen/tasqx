//! Transfer domain methods for Engine.

use super::graph::{link_row, node_exists, node_id, LINK_COLS};
use super::*;
use crate::engine::CHECK_STATES;

impl Engine {
    // ---- store.export --------------------------------------------------------

    /// Export the tasks matching `filter` as `{tasks, dropped_dependencies}`.
    ///
    /// A filter selects a *subset*, but a dependency edge points outside the
    /// subset as happily as inside it. An export that names an id it does not
    /// carry is not a document — it is a dangling pointer, and `store.import`
    /// now (correctly) rejects it. So edges are trimmed to the exported set in a
    /// second pass, and the count of trimmed edges is reported: silently losing
    /// a dependency is exactly the kind of thing that must be visible.
    /// `dropped_dependencies` is always present and is 0 for an unfiltered
    /// export, which stays a byte-identical round trip.
    pub fn store_export(&self, p: &Value) -> Result<Value, ApiError> {
        // One instant for the whole export: the filter's relative dates, every
        // row's wait/schedule release and every recomputed urgency agree about
        // what time it is.
        let now_ts = crate::clock::now();
        let filter = Filter::parse(&opt_str(p, "filter")?.unwrap_or_default(), now_ts)
            .map_err(ApiError::bad_request)?;
        validate_filter_projects(self.conn(), &filter)?;
        // D171: widen a scoped export back to docs/projects with no project at
        // all. Refused, not silently ignored, on an UNFILTERED export — the
        // same "a value that changes nothing is refused" rule `memory.search`
        // already applies to its own `include_unscoped`: there is no scope
        // there to widen FROM, the whole store comes back either way, and a
        // caller who sent this believes a scope is being applied.
        let include_unscoped = opt_bool(p, "include_unscoped")?.unwrap_or(false);
        if include_unscoped && filter.is_unfiltered() {
            return Err(ApiError::bad_request(
                "`include_unscoped` widens a filtered export's docs/projects to ones that \
                 carry no project — an unfiltered export already carries all of them, so \
                 it needs a filter to widen from",
            ));
        }
        // ONE snapshot for the whole document. This function issues
        // SNAPSHOT_QUERY_COUNT statements (`load_task_snapshots`) plus three
        // more — projects, docs and the default. The total is deliberately not
        // spelled as a literal here: it read "eight" until the token side table
        // became a sixth snapshot query, and a count that drifts silently in
        // prose is the failure this comment exists to warn about.
        // In WAL each statement otherwise takes
        // its own snapshot — so a writer committing between two of them tears
        // the export: tasks read before a `done`, its annotations read after.
        // Every mutation in this engine keeps its state and its event row in one
        // transaction; the read that produces the BACKUP had no such boundary at
        // all, which is the one read where it matters most. DESIGN §2 makes racing
        // one-shot processes a supported configuration ("two `tasqx add` racing
        // from two shells are safe"), and a backup that mixes two points in time
        // is precisely the thing a backup must not be.
        //
        // DEFERRED (`unchecked_transaction`'s default), never `begin_mutation`'s
        // IMMEDIATE: the read snapshot pins at the first read and holds, without
        // taking the write lock — §2's "concurrent readers never block" means a
        // long export must not stall every writer for its duration.
        //
        // Bound to a NAME, not `_`: `let _ = ...` drops the guard on the spot
        // and the whole thing silently becomes a no-op. Dropping it at the end
        // rolls back, which is the correct no-op for a pure read.
        //
        // Held by the helpers too, without threading `&tx` through them: a
        // `Transaction` borrows this very `Connection`, so every `self.conn`
        // read below already runs inside it.
        let _snapshot = self.conn.unchecked_transaction()?;
        let mut snapshots = self.load_task_snapshots(now_ts)?;
        snapshots.sort_by_key(|snapshot| snapshot.task.short_id);

        // Pass 1: which tasks survive the filter. Edges can only be resolved
        // against the *whole* selected set, so nothing is emitted yet.
        let mut selected = Vec::new();
        for snapshot in snapshots {
            let t = &snapshot.task;
            let ctx = MatchCtx {
                status: t.status,
                priority: t.priority,
                project: t.project.as_deref(),
                tags: &snapshot.tags,
                due: t.due.as_deref(),
                completed: t.completed.as_deref(),
                blocked: snapshot.blocked,
            };
            if !filter.matches(&ctx) {
                continue;
            }
            selected.push(snapshot);
        }
        let present: HashSet<&str> = selected
            .iter()
            .map(|snapshot| snapshot.task.id.as_str())
            .collect();

        // Pass 2: emit, keeping only edges whose target is also being emitted.
        let mut dropped = 0i64;
        let mut out = Vec::with_capacity(selected.len());
        for snapshot in &selected {
            out.push(Self::export_task(snapshot, &present, &mut dropped, now_ts));
        }

        // D171 (finding #627): an export that narrows `tasks` used to still
        // ship every memory doc, every project row and the whole event log —
        // a leak when the file is shared, and a surprise on import elsewhere.
        // `needed_projects` is the scope those three now share: `None` means
        // "unfiltered, carry everything" (D12/D37's byte-identical round
        // trip, which must not lose an archived project no task references),
        // and `Some(set)` is the union of every `project:`/`proj:` value the
        // filter itself named (so an explicitly named but currently empty
        // project still comes back) and every project the SELECTED tasks
        // actually reference (so a filter naming no project at all, `+bug`
        // spanning two projects, still carries only what those tasks need —
        // the same "only what it needs" rule the named case follows).
        let needed_projects: Option<HashSet<&str>> = if filter.is_unfiltered() {
            None
        } else {
            let mut set: HashSet<&str> = filter.project_names().into_iter().collect();
            for snapshot in &selected {
                if let Some(name) = snapshot.task.project.as_deref() {
                    set.insert(name);
                }
            }
            Some(set)
        };

        let (docs, dropped_docs) = self.export_docs(needed_projects.as_ref(), include_unscoped)?;
        let doc_ids: HashSet<&str> = docs.iter().filter_map(|d| d["id"].as_str()).collect();
        let (projects, dropped_projects) = self.export_projects(needed_projects.as_ref())?;
        let project_ids: HashSet<&str> = projects.iter().filter_map(|p| p["id"].as_str()).collect();

        // D181: the ids, per node kind, that this document actually carries —
        // what BOTH ends of a link are checked against. An annotation is a node
        // too (D160) and travels inside its task, so the ones this document
        // carries are exactly the selected tasks'.
        //
        // Built on EVERY export, filtered or not (CodeRabbit on #117). On an
        // unfiltered one the task, doc and project sets are every row in the
        // store anyway, so the scope costs nothing there — but the annotations
        // are only the LIVE ones, because `annotation.remove` tombstones the
        // row (D113) and the export deliberately does not carry a tombstone.
        // An unscoped export that waved every link through therefore shipped a
        // link to a removed annotation, and the import refused that dangling
        // end and rolled the whole restore back: a store could not back itself
        // up at all once one annotation with a link to it had been removed.
        let mut carried: HashSet<(NodeType, &str)> = HashSet::new();
        carried.extend(present.iter().map(|id| (NodeType::Task, *id)));
        carried.extend(doc_ids.iter().map(|id| (NodeType::Memory, *id)));
        carried.extend(project_ids.iter().map(|id| (NodeType::Project, *id)));
        for snapshot in &selected {
            carried.extend(
                snapshot
                    .annotations
                    .iter()
                    .filter_map(|a| a["id"].as_str())
                    .map(|id| (NodeType::Annotation, id)),
            );
        }
        let (links, dropped_links) = self.export_links(&carried)?;
        let link_ids: HashSet<&str> = links.iter().filter_map(|l| l["id"].as_str()).collect();
        let (events, dropped_events) =
            self.export_events(&present, &doc_ids, &project_ids, &link_ids)?;

        // D171 review finding: `default_project` names the STORE's default
        // regardless of `filter`, so a filtered export whose scope drops that
        // very project (the default is B, `filter` is `project:A`) used to
        // hand back a document naming a project its own `projects` section
        // did not carry — exactly the shape `store.import` refuses ("names
        // ..., which the payload's `projects` section does not define and
        // the store does not have"), so the export could not round-trip into
        // a fresh store at all. Named only when it is among the rows this
        // document actually carries; `None` (unfiltered) always is, by the
        // same live-project invariant `default_project()` itself keeps.
        let default_project = self.default_project()?;
        let default_project = match &needed_projects {
            None => default_project,
            Some(_) => {
                let project_names: HashSet<&str> =
                    projects.iter().filter_map(|p| p["name"].as_str()).collect();
                default_project.filter(|name| project_names.contains(name.as_str()))
            }
        };

        Ok(json!({
            "tasks": out,
            "dropped_dependencies": dropped,
            // D37/D171: every project row this document needs. Always ALL of
            // them on an unfiltered export (`needed_projects: None`) — a
            // filter selects TASKS, and on that path a project the selected
            // tasks do not mention is not a dangling pointer. A filtered
            // export narrows to `needed_projects` and reports the trim.
            "projects": projects,
            "dropped_projects": dropped_projects,
            // D41/D171: memory docs, scoped the same way. An unscoped doc
            // ships only with `include_unscoped` — the leak concern is that a
            // filtered export should carry only what its tasks need, and a
            // doc with no project is nobody's in particular.
            "docs": docs,
            "dropped_docs": dropped_docs,
            // D160/D181: the graph's own edges. A backup used to restore with
            // none of them, beside an event log describing every edge that was
            // supposed to be there. Scoped on EVERY export — a link travels
            // only when BOTH its ends are nodes this document carries, because
            // an end it does not hold is the dangling pointer `store.import`
            // refuses outright, and an unfiltered export carries no tombstoned
            // annotation either.
            "links": links,
            "dropped_links": dropped_links,
            // The audit trail for the tasks, docs and projects THIS document
            // carries (#176, D171): a restored store used to answer `chart
            // heatmap`/`chart throughput` with zero `done`s while `list
            // status:done` still counted 81, because the document carried
            // every task's CURRENT fields and none of the events that explain
            // how they got there. Filtered to `present`/`doc_ids`/
            // `project_ids` the same way `depends_on` is trimmed above — an
            // excluded task, doc or project is excluded whole, and an `add`
            // or `memory.add` event naming its title verbatim would leak it
            // back into the document by the same side door `dropped_
            // dependencies` was invented to close for edges.
            "events": events,
            "dropped_events": dropped_events,
            // Store state, so the document carries it (D21: it lives in the
            // store's `config` table, never in config.toml). `null` when there
            // is none, or when a filtered export did not carry it (above).
            "default_project": default_project,
        }))
    }

    /// How many child rows one task holds across the four tables `merge`
    /// unions (D185).
    ///
    /// It exists because an `ON CONFLICT DO UPDATE` reports a row affected
    /// even when it rewrote the same bytes, so the statement's own count
    /// cannot answer "did this merge bring anything in" — and that question
    /// decides whether `rev` moves. Counting rows can only say "more than
    /// before", so the two changes it cannot see are reported separately: the
    /// scalars moving (`take_payload`), and a check both stores hold taking
    /// the payload's state on its own `modified` (`import_checks`' return).
    fn child_row_count(tx: &rusqlite::Transaction, id: &str) -> Result<i64, ApiError> {
        Ok(tx.query_row(
            "SELECT (SELECT COUNT(*) FROM annotations WHERE task_id = ?1) \
                  + (SELECT COUNT(*) FROM checks WHERE task_id = ?1) \
                  + (SELECT COUNT(*) FROM task_tags WHERE task_id = ?1) \
                  + (SELECT COUNT(*) FROM token_usage WHERE task_id = ?1)",
            params![id],
            |r| r.get(0),
        )?)
    }

    /// Replace one task's annotations from its import object — the
    /// annotations half of `store_import`'s per-task child-table work,
    /// self-contained: it reads `tv` (the task document), never the request
    /// params, which is what keeps it invisible to dispatch's key-scan on
    /// purpose.
    ///
    /// `merge` (D185) keeps every row this store already holds and upserts the
    /// payload's beside them, keyed on each annotation's own id, so a note
    /// written on either machine since the export survives. Without it the
    /// payload's task object stays authoritative about its own child rows
    /// (D138) and they are replaced wholesale.
    ///
    /// `take_payload` decides a note both stores hold under ONE id (task
    /// #802). An annotation carries only `created` — there is no per-row stamp
    /// to compare, the way a check has its own `modified` — so it follows the
    /// task-level winner: the stored body stands when the store's `modified`
    /// is the later one, and an older replica cannot overwrite an edit written
    /// since. An id this task does not hold is inserted either way.
    fn import_annotations(
        tx: &rusqlite::Transaction,
        id: &str,
        tv: &Value,
        merge: bool,
        take_payload: bool,
    ) -> Result<(), ApiError> {
        if !merge {
            tx.execute("DELETE FROM annotations WHERE task_id = ?1", params![id])?;
        }
        if let Some(anns) = import_field(id, "annotations", opt_array(tv, "annotations"))? {
            for a in anns {
                // `Value::get` answers None on a non-object, so every field
                // fell back to its default: `annotations:[42]` MINTED a
                // blank annotation with a fresh uuid, and `{"text":"hi"}`
                // stored an empty body. Fabricating a row is worse than
                // dropping one — the caller's note is gone and something
                // stands in its place.
                import_keys(
                    &format!("task {id}, "),
                    "annotations[]",
                    a,
                    IMPORT_ANNOTATION_KEYS,
                )?;
                let aid = import_field(id, "annotations[].id", opt_str_nonempty(a, "id"))?
                    .unwrap_or_else(|| crate::clock::uuid_v7().to_string());
                let body = import_field(id, "annotations[].body", req_str(a, "body"))?;
                let acreated =
                    import_field(id, "annotations[].created", opt_str_nonempty(a, "created"))?
                        .unwrap_or_else(now);
                let holder = child_owner(
                    tx,
                    "annotations",
                    "annotations[].id",
                    "annotation",
                    id,
                    &aid,
                )?;
                // D185 (task #802): the task-level winner decides a note both
                // stores hold, because an annotation has no stamp of its own.
                // Keeping the stored row is the whole of it — the store won,
                // so its body stands and nothing is written.
                if merge && !take_payload && holder.is_some() {
                    continue;
                }
                // ON CONFLICT DO UPDATE, never INSERT OR REPLACE: REPLACE
                // deletes the old row WITHOUT firing the delete trigger
                // (recursive_triggers is off), so a payload that rewrote an
                // annotation's body left a dangling entry in annotations_fts
                // — and once the freed rowid was reused, memory.search
                // answered the OLD text with an UNRELATED annotation. The
                // UPDATE path keeps the rowid and fires annotations_fts_au,
                // which does the delete+insert pair the index needs (D41
                // review finding).
                tx.execute(
                    "INSERT INTO annotations (id, task_id, body, created) \
                     VALUES (?1,?2,?3,?4) \
                     ON CONFLICT(id) DO UPDATE SET \
                     task_id=excluded.task_id, body=excluded.body, created=excluded.created",
                    params![aid, id, body, acreated],
                )?;
            }
        }
        Ok(())
    }

    /// Replace one task's acceptance criteria, wholesale like annotations
    /// (D138): the payload's task object is authoritative about its own child
    /// rows.
    ///
    /// `merge` (D185) unions them instead, like annotations — and a check is
    /// the one child row that resolves a same-id conflict on a stamp of its
    /// OWN. A check carries STATE, so a criterion both stores hold has two
    /// answers, and the one to keep is the later write's: the payload's body,
    /// state, evidence and position land only when the payload check's
    /// `modified` is strictly past the stored one (task #802). A stored
    /// `passed` is not rolled back to `open` by a copy that never saw it pass,
    /// and a criterion edited here is not thrown away because the payload's
    /// TASK happened to be the later write. A check the store does not hold at
    /// all is inserted either way.
    ///
    /// Answers whether a merge updated a check the store already held — the
    /// caller's [`Self::child_row_count`] sees inserts and nothing else, and
    /// that update is a change `rev` has to move for.
    ///
    /// `state` passes the same closed-vocabulary gate `check.set` enforces,
    /// with `import_field` naming the task — carrying an unknown state
    /// verbatim would let one bad payload re-export the corruption to every
    /// downstream store (D16).
    fn import_checks(
        tx: &rusqlite::Transaction,
        id: &str,
        tv: &Value,
        merge: bool,
    ) -> Result<bool, ApiError> {
        if !merge {
            tx.execute("DELETE FROM checks WHERE task_id = ?1", params![id])?;
        }
        let Some(rows) = import_field(id, "checks", opt_array(tv, "checks"))? else {
            return Ok(false);
        };
        let sql = "INSERT INTO checks (id, task_id, body, state, evidence, position, created, \
             modified) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO UPDATE SET \
             task_id=excluded.task_id, body=excluded.body, state=excluded.state, \
             evidence=excluded.evidence, position=excluded.position, \
             created=excluded.created, modified=excluded.modified";
        let mut updated = false;
        for (n, c) in rows.iter().enumerate() {
            import_keys(&format!("task {id}, "), "checks[]", c, IMPORT_CHECK_KEYS)?;
            let cid = import_field(id, "checks[].id", opt_str_nonempty(c, "id"))?
                .unwrap_or_else(|| crate::clock::uuid_v7().to_string());
            let body = import_field(id, "checks[].body", req_str(c, "body"))?;
            let state = import_field(id, "checks[].state", opt_str_nonempty(c, "state"))?
                .unwrap_or_else(|| CHECK_STATES[0].to_string());
            if !CHECK_STATES.contains(&state.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "task {id}, checks[]: state must be {} (got {state:?})",
                    CHECK_STATES.join("|")
                )));
            }
            let evidence = import_field(id, "checks[].evidence", opt_str_nonempty(c, "evidence"))?;
            // Absent falls back to the array's own order, which is what an
            // export emits anyway — a payload hand-written without positions
            // still round-trips in the order it was written.
            let position =
                import_field(id, "checks[].position", opt_i64(c, "position"))?.unwrap_or(n as i64);
            let created = import_field(id, "checks[].created", opt_str_nonempty(c, "created"))?
                .unwrap_or_else(now);
            let modified = import_field(id, "checks[].modified", opt_str_nonempty(c, "modified"))?
                .unwrap_or_else(|| created.clone());
            // The same last-writer-wins the scalars follow, read off the row
            // itself: a check this task already holds stands unless the
            // payload's copy was written strictly later. A tie keeps the
            // store's, which is what makes merging one document twice a no-op.
            let owner = child_owner(tx, "checks", "checks[].id", "check", id, &cid)?;
            if merge && owner.is_some() {
                let held: String = tx.query_row(
                    "SELECT modified FROM checks WHERE id = ?1",
                    params![cid],
                    |r| r.get(0),
                )?;
                if parse_ts(&modified) <= parse_ts(&held) {
                    continue;
                }
                updated = true;
            }
            tx.execute(
                sql,
                params![cid, id, body, state, evidence, position, created, modified],
            )?;
        }
        Ok(updated)
    }

    /// Replace one task's token measurements, wholesale like tags and
    /// annotations: the payload's task object is authoritative about its own
    /// child rows. Each row passes the same closed-vocabulary gates
    /// `token.add` enforces, with `import_field` naming the task — carrying
    /// an unknown source/confidence verbatim would let one bad payload
    /// re-export the corruption to every downstream store (D16).
    ///
    /// `merge` (D185) unions the ledger rather than replacing it, for the
    /// reason annotations and checks are unioned: a measurement recorded on
    /// the destination since the export is spend that really happened, and a
    /// wholesale replace would subtract it from every budget reading.
    fn import_token_measurements(
        tx: &rusqlite::Transaction,
        id: &str,
        tv: &Value,
        merge: bool,
    ) -> Result<(), ApiError> {
        if !merge {
            tx.execute("DELETE FROM token_usage WHERE task_id = ?1", params![id])?;
        }
        // Measurement ids this task's own payload has already carried (task
        // #802). A merge skips an id the task already holds, and the skip
        // cannot tell a re-import from a payload that lists one id TWICE —
        // the store's row is there either way — so the payload's own repeats
        // are counted here instead of read off the table.
        let mut seen: HashSet<String> = HashSet::new();
        if let Some(measurements) = import_field(id, "tokens", opt_array(tv, "tokens"))? {
            for m in measurements {
                import_keys(&format!("task {id}, "), "tokens[]", m, IMPORT_TOKEN_KEYS)?;
                let mid = import_field(id, "tokens[].id", opt_str_nonempty(m, "id"))?
                    .unwrap_or_else(|| crate::clock::uuid_v7().to_string());
                let tool = import_field(id, "tokens[].tool", req_str(m, "tool"))?;
                let source = import_field(id, "tokens[].source", req_str(m, "source"))?;
                import_field(
                    id,
                    "tokens[].source",
                    crate::tokens::require_source(&source),
                )?;
                let confidence = import_field(id, "tokens[].confidence", req_str(m, "confidence"))?;
                import_field(
                    id,
                    "tokens[].confidence",
                    crate::tokens::require_confidence(&confidence),
                )?;
                let model = import_field(id, "tokens[].model", opt_str_nonempty(m, "model"))?;
                let count = |key: &str| -> Result<i64, ApiError> {
                    Ok(import_field(
                        id,
                        &format!("tokens[].{key}"),
                        super::tokens::opt_token_count(m, key),
                    )?
                    .unwrap_or(0))
                };
                let mcreated =
                    import_field(id, "tokens[].created", opt_str_nonempty(m, "created"))?
                        .unwrap_or_else(now);
                // Plain INSERT, unlike the annotations upsert above: this
                // task's rows were just deleted, so a surviving row with
                // the same id means the payload reuses one measurement id
                // across tasks (or steals it from a task outside the
                // payload). An upsert would silently move the row to the
                // last claimant and the earlier task's spend would vanish
                // — refuse and name the id instead. No FTS index hangs off
                // token_usage, so the annotations' trigger reasoning does
                // not apply here.
                //
                // D185: on a merge nothing was deleted, so a row this very
                // task already holds under that id is the SAME measurement
                // arriving a second time, not a stolen one — the idempotent
                // answer is to leave it alone, ONCE. A second copy inside the
                // same payload (task #802) is not a re-import: it is one
                // measurement id standing for two rows of spend, and taking
                // the first silently drops the second.
                let repeat = !seen.insert(mid.clone());
                let holder: Option<String> = tx
                    .query_row(
                        "SELECT task_id FROM token_usage WHERE id = ?1",
                        params![mid],
                        |r| r.get(0),
                    )
                    .optional()?;
                if merge && !repeat && holder.as_deref() == Some(id) {
                    continue;
                }
                import_field(
                    id,
                    "tokens[].id",
                    if repeat || holder.is_some() {
                        Err(ApiError::bad_request(format!(
                            "measurement id {mid:?} appears more than once in the \
                             import (or belongs to a task outside it) — every \
                             tokens[].id must be unique"
                        )))
                    } else {
                        Ok(())
                    },
                )?;
                tx.execute(
                    "INSERT INTO token_usage (id, task_id, tool, source, model, \
                     input_tokens, output_tokens, cache_read_tokens, \
                     cache_creation_tokens, confidence, created, total_tokens) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        mid,
                        id,
                        tool,
                        source,
                        model,
                        count("input_tokens")?,
                        count("output_tokens")?,
                        count("cache_read_tokens")?,
                        count("cache_creation_tokens")?,
                        confidence,
                        mcreated,
                        count("total_tokens")?
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// Every memory doc row, id-ordered (creation order, since UUIDv7). D41.
    ///
    /// `needed`, D171: `None` on an unfiltered export keeps every doc,
    /// matching D12's byte-identical round trip. `Some(set)` keeps a doc whose
    /// `project` is in `set`, plus an unscoped doc only when `include_unscoped`
    /// asks for it — the same widening `memory.search`'s own flag of that name
    /// already does. Returns the kept rows and how many were dropped.
    fn export_docs(
        &self,
        needed: Option<&HashSet<&str>>,
        include_unscoped: bool,
    ) -> Result<(Vec<Value>, i64), ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, title, body, created, modified, project, rev, standing, \
             origin_path, origin_mtime, origin_size \
             FROM docs ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "source": r.get::<_, Option<String>>(1)?,
                "title": r.get::<_, String>(2)?,
                "body": r.get::<_, String>(3)?,
                "created": r.get::<_, String>(4)?,
                "modified": r.get::<_, String>(5)?,
                "project": r.get::<_, Option<String>>(6)?,
                "_rev": r.get::<_, i64>(7)?,
                // #101: the export is the backup (D12/D37), so a standing doc
                // must come back standing — the flag travels with the row.
                "standing": r.get::<_, i64>(8)? != 0,
                // #788/D180: on the same terms — a restored store must know
                // which file each doc came from, or the first freshness check
                // after a restore would report every imported doc as unknown.
                "origin_path": r.get::<_, Option<String>>(9)?,
                "origin_mtime": r.get::<_, Option<i64>>(10)?,
                "origin_size": r.get::<_, Option<i64>>(11)?,
            }))
        })?;
        let mut out = Vec::new();
        let mut dropped = 0i64;
        for r in rows {
            let v = r?;
            let keep = match needed {
                None => true,
                Some(set) => match v["project"].as_str() {
                    Some(name) => set.contains(name),
                    None => include_unscoped,
                },
            };
            if keep {
                out.push(v);
            } else {
                dropped += 1;
            }
        }
        Ok((out, dropped))
    }

    /// Every link row, id-ordered (creation order, since UUIDv7). D160/D181.
    ///
    /// The row is `link.list`'s own, read through the same `link_row`, so the
    /// archive and the reader cannot come to disagree about what a link is.
    /// `created_by` is not in it for the reason `LINK_COLS` names: nothing
    /// reads the column and `'user'` is the only value anything writes.
    ///
    /// `carried` is the scope — the nodes this document holds, per kind — and a
    /// link travels only when BOTH its ends are in it. Applied to an unfiltered
    /// export too: there the set is every task, doc and project in the store,
    /// but only the LIVE annotations, so a link to a removed one is trimmed
    /// here rather than exported as the dangling end `store.import` refuses.
    /// A `from_type`/`to_type` outside [`NodeType`] cannot be written by this
    /// engine, and is treated as a node no document carries rather than waved
    /// through — the only end an export may emit is one it can prove it holds.
    /// Returns the kept rows and how many were dropped.
    fn export_links(
        &self,
        carried: &HashSet<(NodeType, &str)>,
    ) -> Result<(Vec<Value>, i64), ApiError> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {LINK_COLS} FROM links ORDER BY id"))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                (r.get::<_, String>(3)?, r.get::<_, String>(4)?),
                link_row(r)?,
            ))
        })?;
        let carries = |(ty, id): &(String, String)| {
            NodeType::parse(ty).is_some_and(|ty| carried.contains(&(ty, id.as_str())))
        };
        let mut out = Vec::new();
        let mut dropped = 0i64;
        for r in rows {
            let (from, to, link) = r?;
            if carries(&from) && carries(&to) {
                out.push(link);
            } else {
                dropped += 1;
            }
        }
        Ok((out, dropped))
    }

    /// Every event this store has recorded, id-ordered (UUIDv7, so
    /// chronological), for the tasks in `present`, the docs in `doc_ids`, the
    /// projects in `project_ids` and the links in `link_ids` — MINUS the
    /// bookkeeping rows
    /// `store.import` itself writes on every call it makes (`import` on a
    /// task or project, and a doc's `memory.add` carrying
    /// `via: "store.import"`).
    ///
    /// `present` is the SAME set `export_task` trims `depends_on` against, and
    /// `doc_ids`/`project_ids` (D171) and `link_ids` (D181) are the ids of the
    /// rows `export_docs`/`export_projects`/`export_links` actually kept: an
    /// event is emitted only when its `entity_id` names something this document
    /// carries, so a filtered export cannot leak an excluded task's title, an
    /// excluded project's name, an excluded doc's own title or an excluded
    /// link's endpoints through the event log after `tasks`/`docs`/`projects`/
    /// `links` correctly left it out. On an
    /// unfiltered export every id set already names everything, so nothing
    /// here is trimmed — the same "no restriction" shape `needed_projects`
    /// itself follows. Returns the kept rows and how many were dropped by
    /// this scoping (the `import`/`via` exclusion below is separate
    /// bookkeeping noise, never counted as a drop).
    ///
    /// The `import`/`via` exclusion is what keeps D12's round trip byte-
    /// identical now that events ARE carried: `store.import` mints a fresh
    /// marker per row it touches on every single call (see the `insert_event`
    /// calls in `store_import` below), so replaying those markers on the next
    /// export would mean two successive `export -> import -> export` cycles
    /// produce two DIFFERENT `events` arrays — each restore permanently
    /// growing the next one. Nothing downstream reads an `import` event:
    /// `chart throughput` keys on `add`/`done`, `chart heatmap` on `done`
    /// (see `tasqx-cli/src/chart.rs`) — so leaving the markers out costs
    /// nothing real history depends on. A genuine `memory.add`, including one
    /// written by the UNRELATED `memory.import` CLI command, is real history
    /// and stays.
    fn export_events(
        &self,
        present: &HashSet<&str>,
        doc_ids: &HashSet<&str>,
        project_ids: &HashSet<&str>,
        link_ids: &HashSet<&str>,
    ) -> Result<(Vec<Value>, i64), ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, entity, entity_id, op, payload, ts, actor FROM events ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        let mut dropped = 0i64;
        for r in rows {
            let (id, entity, entity_id, op, payload, ts, actor) = r?;
            let keep = match Entity::parse(&entity) {
                Some(Entity::Task) => present.contains(entity_id.as_str()),
                Some(Entity::Doc) => doc_ids.contains(entity_id.as_str()),
                Some(Entity::Project) => project_ids.contains(entity_id.as_str()),
                // D181: the `links` table IS part of the archive now, so a link
                // event is scoped to the link this document carries, exactly
                // like the three kinds above. It used to travel whole, which on
                // a filtered export left the log describing edges the document
                // had no row for.
                Some(Entity::Link) => link_ids.contains(entity_id.as_str()),
                // A future entity kind this build does not know: pass it
                // through rather than silently dropping history it cannot
                // scope — the same "refuse or widen, never guess" stance as
                // `store.import`'s own unrecognized-top-level-key tolerance.
                None => true,
            };
            if !keep {
                dropped += 1;
                continue;
            }
            let payload: Value = payload
                .as_deref()
                .and_then(|p| serde_json::from_str(p).ok())
                .unwrap_or(Value::Null);
            let via_store_import =
                matches!(payload.get("via"), Some(Value::String(s)) if s == "store.import");
            if op == "import" || (op == "memory.add" && via_store_import) {
                continue;
            }
            out.push(json!({
                "id": id,
                "entity": entity,
                "entity_id": entity_id,
                "op": op,
                "payload": payload,
                "ts": ts,
                "actor": actor,
            }));
        }
        Ok((out, dropped))
    }

    /// Every project row, name-ordered, in the canonical §3 shape. D37.
    ///
    /// `needed`, D171: `None` on an unfiltered export keeps every project,
    /// archived included — an export selects TASKS, and a project no task
    /// mentions is not a dangling pointer there. `Some(set)` keeps a project
    /// whose `name` is in `set`. Returns the kept rows and how many were
    /// dropped.
    fn export_projects(
        &self,
        needed: Option<&HashSet<&str>>,
    ) -> Result<(Vec<Value>, i64), ApiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, archived, created FROM projects ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "description": r.get::<_, Option<String>>(2)?,
                "archived": r.get::<_, i64>(3)? != 0,
                "created": r.get::<_, String>(4)?,
            }))
        })?;
        let mut out = Vec::new();
        let mut dropped = 0i64;
        for r in rows {
            let v = r?;
            let keep = match needed {
                None => true,
                Some(set) => v["name"].as_str().is_some_and(|name| set.contains(name)),
            };
            if keep {
                out.push(v);
            } else {
                dropped += 1;
            }
        }
        Ok((out, dropped))
    }

    /// Build the canonical §3 export object for one task. serde_json's default
    /// `Map` is a `BTreeMap`, so keys serialize sorted (canonical form).
    /// `present` is the id set being exported; edges leaving it are dropped and
    /// counted into `dropped`.
    fn export_task(
        snapshot: &TaskSnapshot,
        present: &HashSet<&str>,
        dropped: &mut i64,
        now_ts: Timestamp,
    ) -> Value {
        let t = &snapshot.task;
        let all = &snapshot.depends_on;
        let kept: Vec<String> = all
            .iter()
            .filter(|d| present.contains(d.as_str()))
            .cloned()
            .collect();
        *dropped += (all.len() - kept.len()) as i64;
        let mut out = json!({
                "id": t.id,
                "short_id": t.short_id,
                "title": t.title,
                // Verbatim, not canonicalized: export is the escape hatch out of a
                // store the reader could not fully understand, so it must hand back
                // what is actually in the file. Re-importing it then fails naming
                // this value (the import gate), which is what tells the user which
                // line to edit — a rescue that quietly rewrote it would leave them
                // with no evidence and a store that still disagrees with itself.
                "status": t.status_text(),
                "priority": t.priority.map(|x| x.as_str()),
                "project": t.project,
                "tags": snapshot.tags,
                "due": t.due,
                "scheduled": t.scheduled,
                "wait": t.wait,
                "estimate": t.estimate,
                "recurrence": t.recurrence,
                "remind": t.remind,
                // D139: an export is self-contained (D12, D37), so the gauge
                // travels with the task it belongs to.
                "budget_tokens": t.budget_tokens,
                "depends_on": kept,
                // D138: an export is self-contained (D12, D37), so the criteria
                // travel with the task like its annotations do.
                "checks": snapshot.checks,
                "annotations": snapshot.annotations,
                "urgency": urgency::score_at(t.priority, t.due.as_deref(), &t.created, now_ts),
                "created": t.created,
                "modified": t.modified,
                "completed": t.completed,
                "_rev": t.rev,
        });
        // Three OPTIONAL keys follow, all conditional for one reason:
        // `IMPORT_TASK_KEYS` is a closed gate, so an always-present key would
        // make every new export a `bad_request` in an older tasqx — and the D12
        // byte-identical round trip holds only while the §3 shape of a task
        // that carries none of this is exactly what it always was. Each is
        // emitted only for the tasks the fact is actually true of, which is the
        // same rule `status_unrecognized` follows below.

        // D42: emitted only when non-zero. The stored form is emitted verbatim
        // (an i64 of seconds, like the column), not the ISO spelling `task.get`
        // publishes: this is the restore path, and it must not gain a way to
        // fail parsing.
        if t.tracked_seconds != 0 {
            out["tracked_seconds"] = json!(t.tracked_seconds);
        }
        // D166: the net of the `task.adjust_tracked` corrections inside
        // `tracked_seconds`, on the same conditional rule — absent when none.
        if t.tracked_adjustment_seconds != 0 {
            out["tracked_adjustment_seconds"] = json!(t.tracked_adjustment_seconds);
        }
        // D42: the open interval's anchor, present only while the task is
        // `active`, and emitted for the same reason D12 exists: an export that
        // drops it is not self-contained. The alternative — reconstructing an
        // anchor at import from `created` — silently bills every second since
        // the task was created to the next `stop`, which is the same class of
        // fabricated total this key was added to prevent.
        if let Some(anchor) = &t.active_since {
            out["active_since"] = json!(anchor);
        }
        // Absent, not `[]`, when a task has no measurements: an always-empty
        // key on every task would change the §3 export shape for stores that
        // never recorded a token.
        if !snapshot.tokens.is_empty() {
            out["tokens"] = json!(snapshot.tokens);
        }
        // D165: the pinned delivery note, present only on a task a completion
        // pinned one on — conditional for the closed-gate reason above.
        if let Some(pin) = &t.delivered_annotation_id {
            out["delivered_annotation_id"] = json!(pin);
        }
        // D170: by uuid, like `depends_on` here — only on a recurrence spawn.
        // Not trimmed to `present`: it is provenance, not an edge, and
        // `task.get` reads a predecessor the store does not hold as null.
        if let Some(from) = &snapshot.spawned_from {
            out["spawned_from"] = json!(from);
        }
        // Last, and exactly once. It only ever ADDS `status_unrecognized`, so
        // wrapping the literal or the finished object is the same document —
        // wrapping last is the spelling that stays correct as conditional keys
        // are added above it.
        flag_unrecognized_status(t, out)
    }

    // ---- store.import --------------------------------------------------------

    /// `store.import` — load an export document. Params: `tasks` (required),
    /// `projects`, `default_project`, `docs`, `dry_run`.
    ///
    /// The one method whose params are a DOCUMENT, not a request (the `document`
    /// flag in [`crate::PARAMS`]): an export written by a newer tasqx must stay
    /// readable here, so an unrecognized top-level key is a future field rather
    /// than a typo. That tolerance is only safe because `tasks` is required — a
    /// misspelled `taskss` is still refused, by absence.
    ///
    /// D184: `dry_run` (default false) runs the import exactly as below and
    /// rolls the transaction back instead of committing it. There is no
    /// second, predictive code path — every rule, every refusal and every
    /// renumbering decision runs for real, against this store, inside the
    /// same `BEGIN IMMEDIATE`; only the very last step differs.
    pub fn store_import(&self, p: &Value) -> Result<Value, ApiError> {
        let dry_run = opt_bool(p, "dry_run")?.unwrap_or(false);
        // D185: default false, so a restore keeps D138's wholesale replace.
        // True turns a KNOWN task's child tables into unions and its scalars
        // into D3's per-field last-writer-wins, which is what makes two live
        // stores mergeable in either direction.
        let merge = opt_bool(p, "merge")?.unwrap_or(false);
        let tasks = req_array(p, "tasks").map_err(|e| {
            ApiError::bad_request(format!(
                "{} — store.import requires a `tasks` array",
                e.message
            ))
        })?;

        // D37: the projects half of the document, optional so an export written
        // by an older tasqx — which had no such section — still imports. Its
        // PRESENCE is load-bearing, not just its contents: a payload that
        // declares its projects is claiming to be self-contained, and is held to
        // it below; one that does not is a legacy document whose projects can
        // only be inferred from the tasks. `opt_array` distinguishes the two,
        // which `unwrap_or_default` would have flattened.
        let declared = opt_array(p, "projects")?.cloned();
        // Read before the transaction so a malformed default is refused without
        // taking the write lock. Validated below either way — whether a document
        // is coherent is a property of the document, not of where it lands.
        let want_default = opt_str_nonempty(p, "default_project")?;

        let mut imported = 0i64;
        let mut projects_imported = 0i64;
        // Names minted by inference rather than sent as records. A write the
        // caller did not ask for must be visible, so these are reported.
        let mut projects_created: Vec<String> = Vec::new();
        // (task_id, its validated `depends_on` ids) — typed at collection time so
        // pass 2 cannot inherit a silently-dropped edge from a wrong-typed value.
        let mut edges: Vec<(String, Vec<String>)> = Vec::new();
        // Task ids this payload has already written, so an id repeated in one
        // document (#180) is refused rather than applied twice and counted
        // twice.
        let mut written: HashSet<String> = HashSet::new();
        // D177: every task whose number this store was already using, and the
        // number it took instead. Always reported, empty when nothing moved.
        let mut renumbered: Vec<Value> = Vec::new();
        // D185: one entry per task this store ALREADY held when `merge` was
        // asked for — `(id, took, rev_target, changed)`. `took` names which
        // side's scalars stand, and the last two carry the deferred `rev`
        // bump: the edges only land in pass 2, so a merge that added nothing
        // but an edge cannot know it changed anything until after that pass.
        // Always reported (as `{id, took}`), empty when `merge` is false or
        // the document held nothing this store had seen — `renumbered`'s rule.
        let mut merged: Vec<(String, &'static str, i64, bool)> = Vec::new();
        // D181: payload id -> the id that row actually landed under, per node
        // kind, filled by the passes that write those rows and read by the
        // links pass. It holds the kinds whose stored id CAN differ from the
        // document's — a project, because `upsert_project` keys on NAME and
        // keeps the destination's own row when it already knows that name, and
        // a doc, which is where #782's source-merge will land one. A kind (or
        // an id) the table does not name resolves as itself, which is what
        // every task and annotation does: D177 moves a task's `short_id` and
        // never its id.
        let mut remap: HashMap<(NodeType, String), String> = HashMap::new();
        let tx = self.begin_mutation()?;

        // Pass 0: projects, before any task, so a task's `project` can be checked
        // against the document's own records rather than against whatever the
        // destination happened to already hold.
        if let Some(rows) = &declared {
            for pv in rows {
                let pv = import_shape("", "project", pv)?;
                // D36/D28: a LOOKUP-strength read, not `req_str`. A store written
                // before D23 can hold a whitespace-named project, its export says
                // so, and refusing the name here would make that document — the
                // escape hatch out of exactly such a store — impossible to
                // restore. `""` is still refused.
                let name = req_str_lookup(pv, "name").map_err(|e| {
                    ApiError::bad_request(format!(
                        "{} — each imported project requires a string `name`",
                        e.message
                    ))
                })?;
                import_keys(
                    &format!("project {name}, "),
                    "project",
                    pv,
                    IMPORT_PROJECT_KEYS,
                )?;
                let description = import_project_field(
                    &name,
                    "description",
                    opt_str_nonempty(pv, "description"),
                )?;
                let archived = import_project_field(&name, "archived", opt_bool(pv, "archived"))?
                    .unwrap_or(false);
                let created =
                    import_project_field(&name, "created", opt_str_nonempty(pv, "created"))?
                        .unwrap_or_else(now);
                let payload_id = import_project_field(&name, "id", opt_str_nonempty(pv, "id"))?;
                let row_id = upsert_project(
                    &tx,
                    &name,
                    description.as_deref(),
                    archived,
                    &created,
                    payload_id.as_deref(),
                )?;
                // D181: the one remap entry that is routinely NOT the identity
                // — a destination that already knows this name kept its own row
                // and its own id, so a link naming the payload's project id has
                // to follow the row here.
                if let Some(payload_id) = payload_id {
                    remap.insert((NodeType::Project, payload_id), row_id.clone());
                }
                insert_event(
                    &tx,
                    Entity::Project,
                    &row_id,
                    "import",
                    &json!({ "name": name, "archived": archived }),
                )?;
                projects_imported += 1;
            }
        }

        // D41 memory docs: optional, so a pre-D41 document still imports.
        // Upsert by id via ON CONFLICT DO UPDATE — the UPDATE path fires
        // docs_fts_au, so the search index follows (the same trigger rule the
        // annotation upsert below learned from the review).
        // #179: PRESENCE, not just contents — a document that declares an
        // empty `docs` array is a legitimate "no memory docs" answer, while
        // one that omits the key entirely never claimed to be self-contained
        // on this axis. `opt_array` (not `unwrap_or_default`) is what lets the
        // two be told apart, the same distinction `declared` draws for
        // `projects` above.
        let docs_param = opt_array(p, "docs")?.cloned();
        let docs_declared = docs_param.is_some();
        // #800(a): a payload `docs` array naming one non-empty source twice
        // would insert the first entry and merge the second onto it (D183),
        // so the document imports order-dependently and `docs_imported`
        // counts both — the same fault `memory.import` already refuses for
        // its own `docs` array, in the same words (D174). Checked over the
        // whole array before any doc is written.
        if let Some(rows) = &docs_param {
            let mut seen = HashSet::new();
            let mut twice: Vec<&str> = Vec::new();
            let sources = rows.iter().filter_map(|d| d.get("source")?.as_str());
            for src in sources.filter(|s| !s.is_empty()) {
                if !seen.insert(src) && !twice.contains(&src) {
                    twice.push(src);
                }
            }
            if !twice.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "store.import names the same source more than once: {} — a source \
                     names one doc (D174), so send each file once",
                    twice.join(", ")
                )));
            }
        }
        let mut docs_imported = 0i64;
        // D183: every doc that landed on a row this store already held under
        // the same `source`. Always reported, empty when nothing merged — the
        // rule `renumbered` and `projects_created` already follow, and for
        // their reason: an id the import dropped is a write the caller did not
        // ask for.
        let mut docs_merged: Vec<Value> = Vec::new();
        if let Some(rows) = docs_param {
            for dv in &rows {
                let dv = import_shape("", "doc", dv)?;
                import_keys("", "doc", dv, IMPORT_DOC_KEYS)?;
                let did = opt_str_nonempty(dv, "id")?
                    .unwrap_or_else(|| crate::clock::uuid_v7().to_string());
                let title = req_str(dv, "title").map_err(|e| {
                    ApiError::bad_request(format!(
                        "{} — each imported doc requires a `title`",
                        e.message
                    ))
                })?;
                let body = req_str(dv, "body").map_err(|e| {
                    ApiError::bad_request(format!(
                        "{} — each imported doc requires a `body`",
                        e.message
                    ))
                })?;
                let source = opt_str_nonempty(dv, "source")?;
                // #88: the task branch's date gate, so a foreign stamp lands in
                // canonical form for D144's sort; RFC3339 short-circuits (D12).
                let now_ts = crate::clock::now();
                let created = import_doc_field(&did, "created", opt_when(dv, "created", now_ts))?
                    .unwrap_or_else(now);
                let modified =
                    import_doc_field(&did, "modified", opt_when(dv, "modified", now_ts))?
                        .unwrap_or_else(now);
                // #134/#135: additive, so a legacy export (or hand-written
                // import) carrying neither key still imports — an unscoped
                // doc at rev 0, exactly what a fresh `memory.add` would mint.
                let project = opt_str_nonempty(dv, "project")?;
                let rev = opt_i64(dv, "_rev")?.unwrap_or(0);
                // #101: additive on the same terms — an export written before
                // the flag existed carries no `standing` key, and its docs
                // restore as ordinary memory rather than as standing orders
                // nobody issued.
                let standing = opt_bool(dv, "standing")?.unwrap_or(false);
                // #788/D180: additive again — a legacy export carries no
                // origin, and its docs restore with none rather than with one
                // invented for them.
                let origin_path = opt_str_nonempty(dv, "origin_path")?;
                let origin_mtime = opt_i64(dv, "origin_mtime")?;
                let origin_size = opt_i64(dv, "origin_size")?;
                // D135: `search_body` is derived, never carried by the export
                // itself (it is not part of the exported doc shape) — always
                // recomputed here from the imported `body`, the same way
                // `memory.add`/`memory.import` compute it at their own write
                // doors, so a restored store's index matches its content.
                let search_body = crate::frontmatter::flatten(&body).into_owned();
                // D183, before the `_rev` guard and before D174's refusal,
                // because neither one is about this case. Two machines that
                // each ran `tasqx memory import docs/` hold one file under one
                // `source` (D179) and two ids, because each store minted its
                // own — and D174 already says the source IS the doc's
                // identity, so the arriving row is that doc, not a second
                // holder of its source. It merges onto the stored row the way
                // D177's task takes a free number: the id this store addresses
                // the doc by is kept, the payload's is dropped, and the remap
                // carries every link and event that named it onto the survivor.
                //
                // The `_rev` guard cannot decide which copy is newer here: the
                // two counters were minted independently, on two stores that
                // never saw each other's writes, so a payload at `_rev` 99 says
                // nothing about a stored row at 3. `modified` is the one field
                // that means the same thing on both machines, so the later edit
                // carries the text — and a tie keeps the store's copy, which is
                // what makes importing the same document twice a no-op.
                let held: Option<(String, String)> =
                    match source.as_deref().filter(|s| !s.is_empty()) {
                        Some(s) => tx
                            .query_row(
                                // `source <> ''` restates the D174 index's
                                // predicate so the planner can use it.
                                "SELECT id, modified FROM docs WHERE source = ?1 AND source <> ''",
                                params![s],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )
                            .optional()?,
                        None => None,
                    };
                if let Some((kept_id, held_modified)) = held.filter(|(id, _)| *id != did) {
                    // #800(b): the lookup above matched on `source`, not on
                    // `did` — so `did` can itself already be a LIVE doc here,
                    // under some other source, the destination's own second
                    // identity for a different file. Merging it onto
                    // `kept_id` would fold two files into one row and drop
                    // whichever text lost the race, so this is refused
                    // before anything moves.
                    let did_is_live: bool = tx
                        .query_row("SELECT 1 FROM docs WHERE id = ?1", params![did], |_| Ok(()))
                        .optional()?
                        .is_some();
                    if did_is_live {
                        let s = source.as_deref().unwrap_or_default();
                        return Err(ApiError::conflict(format!(
                            "store.import: doc {did} carries source {s:?}, which doc \
                             {kept_id} already holds, and {did} is itself a different doc \
                             here — two identities for one file cannot be merged (update \
                             one of them first)"
                        )));
                    }
                    let took = if parse_ts(&modified) > parse_ts(&held_modified) {
                        // D143's rule for a text-changing write: the KEPT row's
                        // own counter, one past where it stands, never the
                        // payload's. `origin_*` is left alone — D180 says it
                        // describes one machine's read of one file, and this
                        // row was read on the other machine.
                        tx.execute(
                            "UPDATE docs SET title = ?2, body = ?3, search_body = ?4, \
                             project = ?5, standing = ?6, modified = ?7, rev = rev + 1 \
                             WHERE id = ?1",
                            params![
                                kept_id,
                                title,
                                body,
                                search_body,
                                project,
                                standing,
                                modified
                            ],
                        )?;
                        "payload"
                    } else {
                        "store"
                    };
                    docs_merged.push(json!({
                        "source": source,
                        "kept_id": kept_id,
                        "dropped_id": did,
                        "took": took,
                    }));
                    // D181's seam, used for the first time: the payload's id is
                    // not a row here, so a link or an event naming it has to be
                    // re-pointed at the doc that kept the source.
                    remap.insert((NodeType::Memory, did.clone()), kept_id.clone());
                    insert_event(
                        &tx,
                        Entity::Doc,
                        &kept_id,
                        "memory.add",
                        &json!({ "title": title, "source": source, "via": "store.import" }),
                    )?;
                    docs_imported += 1;
                    continue;
                }
                // #84: the doc counterpart of the #177 guard above. An older
                // payload would rewind the doc's body and rev, reopening D143's
                // stale-`expected_rev` clobber; same or higher rev still passes (D12).
                let stored_rev: Option<i64> = tx
                    .query_row("SELECT rev FROM docs WHERE id = ?1", params![did], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if let Some(stored) = stored_rev.filter(|s| *s > rev) {
                    return Err(ApiError::conflict(format!(
                        "store.import: doc {did} carries _rev {rev}, but the store already \
                             holds it at _rev {stored} — this payload is older than what is \
                             already here, and importing it would roll its title and body back \
                             to a stale copy (run `tasqx export` first for a merge target, or \
                             drop this doc from the payload)"
                    )));
                }
                super::memory::refuse_source_held_elsewhere(&tx, source.as_deref(), &did)?;
                tx.execute(
                    "INSERT INTO docs \
                     (id, source, title, body, search_body, project, rev, standing, \
                      created, modified, origin_path, origin_mtime, origin_size) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) \
                     ON CONFLICT(id) DO UPDATE SET \
                     source=excluded.source, title=excluded.title, body=excluded.body, \
                     search_body=excluded.search_body, project=excluded.project, \
                     rev=excluded.rev, standing=excluded.standing, \
                     created=excluded.created, modified=excluded.modified, \
                     origin_path=excluded.origin_path, origin_mtime=excluded.origin_mtime, \
                     origin_size=excluded.origin_size",
                    params![
                        did,
                        source,
                        title,
                        body,
                        search_body,
                        project,
                        rev,
                        standing,
                        created,
                        modified,
                        origin_path,
                        origin_mtime,
                        origin_size
                    ],
                )?;
                // D181: the identity today — the upsert keys on the doc's own
                // id — and stated through the table anyway, so #782's
                // source-merge has one place to say otherwise.
                remap.insert((NodeType::Memory, did.clone()), did.clone());
                insert_event(
                    &tx,
                    Entity::Doc,
                    &did,
                    "memory.add",
                    &json!({ "title": title, "source": source, "via": "store.import" }),
                )?;
                docs_imported += 1;
            }
        }

        // #176: the event log half of a restore, optional so a document
        // written before it existed still imports. Faithfully replays the
        // ORIGINAL rows — their own id, op and timestamp — which is what lets
        // a restored store answer `chart heatmap`/`chart throughput` the way
        // the original did instead of starting its history from today.
        // `INSERT OR IGNORE` keyed on the original `id`: re-importing a
        // document already replayed here is a no-op, the same rule every
        // other section in this method follows, and it is also what keeps
        // two stores that both replay one shared history from duplicating it.
        let mut events_imported = 0i64;
        // D181: the link events, held back until pass 2b below has written the
        // links and knows which stored row each payload link landed on — an
        // edge this store already holds under another id keeps ITS row, and an
        // event filed under the payload's id would then name a link nobody
        // here has. Staged rather than run after that pass outright, because a
        // link's ends are the tasks, docs and projects this method has not
        // written yet, so pass 2b cannot move up here. `(id, entity_id, op,
        // payload, ts, actor)`, the row's own columns minus the entity.
        type StagedEvent = (String, String, String, String, String, Option<String>);
        let mut link_events: Vec<StagedEvent> = Vec::new();
        if let Some(rows) = opt_array(p, "events")?.cloned() {
            for ev in &rows {
                let ev = import_shape("", "event", ev)?;
                import_keys("", "event", ev, IMPORT_EVENT_KEYS)?;
                let eid = opt_str_nonempty(ev, "id")?
                    .unwrap_or_else(|| crate::clock::uuid_v7().to_string());
                let entity_raw = req_str(ev, "entity").map_err(|e| {
                    ApiError::bad_request(format!(
                        "{} — each imported event requires an `entity`",
                        e.message
                    ))
                })?;
                let entity = Entity::parse(&entity_raw).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "store.import: event {eid} has entity {entity_raw:?} — expected one of {}",
                        Entity::accepted()
                    ))
                })?;
                let entity_id = req_str(ev, "entity_id").map_err(|e| {
                    ApiError::bad_request(format!(
                        "{} — each imported event requires an `entity_id`",
                        e.message
                    ))
                })?;
                let op = req_str(ev, "op").map_err(|e| {
                    ApiError::bad_request(format!(
                        "{} — each imported event requires an `op`",
                        e.message
                    ))
                })?;
                let payload = ev.get("payload").cloned().unwrap_or(Value::Null);
                let ts = opt_str_nonempty(ev, "ts")?.unwrap_or_else(now);
                let actor = opt_str_nonempty(ev, "actor")?;
                if entity == Entity::Link {
                    link_events.push((eid, entity_id, op, payload.to_string(), ts, actor));
                    continue;
                }
                // D183: a doc event naming an id this import merged away lands
                // under the doc that kept the source, the way a link event
                // passes through `link_remap` below. Nothing is staged for it:
                // the docs pass runs ABOVE this one, so the table already knows
                // where every doc in this document landed.
                let entity_id = match entity {
                    Entity::Doc => remap
                        .get(&(NodeType::Memory, entity_id.clone()))
                        .cloned()
                        .unwrap_or(entity_id),
                    _ => entity_id,
                };
                let n = tx.execute(
                    "INSERT OR IGNORE INTO events (id, entity, entity_id, op, payload, ts, actor) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        eid,
                        entity.as_str(),
                        entity_id,
                        op,
                        payload.to_string(),
                        ts,
                        actor
                    ],
                )?;
                events_imported += n as i64;
            }
        }

        // D177: the mint floor is raised above the WHOLE payload before a single
        // task is written, because a renumbered task mints from the same counter
        // and a number a LATER row in this document still claims would only move
        // the collision one row down — and renumber a task that had no conflict
        // at all. `bump_short_id_floor` never lowers the counter, so a payload
        // below it changes nothing. A number outside the mintable range is
        // skipped here and refused by name, with the task that carries it, in
        // the loop below; `checked_add` for the reason `alloc_short_id` has one.
        //
        // #796: the same pass answers the other whole-document question, for
        // the same reason it has to be asked before the first row lands. Two
        // payload tasks claiming one number is an incoherent DOCUMENT, not a
        // collision to renumber, and it is a property of the payload alone —
        // deciding it inside the loop, from the ids already written, made the
        // answer depend on the row order and on what the destination happened
        // to hold: a new task carrying the number a KNOWN task was renumbered
        // to here was refused as a same-payload duplicate one way round and
        // renumbered the other, while two new tasks both claiming a number the
        // destination holds were both quietly renumbered and accepted.
        let mut claims: HashMap<i64, String> = HashMap::new();
        let mut floor: Option<i64> = None;
        for tv in tasks {
            // Unreadable values are skipped, not diagnosed: `import_shape` and
            // the typed reads in the loop below refuse them by name, with the
            // task that carries them.
            let Ok(Some(short_id)) = opt_i64(tv, "short_id") else {
                continue;
            };
            floor = floor.max(Some(short_id));
            let Ok(Some(id)) = opt_str(tv, "id") else {
                continue;
            };
            // Keyed on the id, so a payload that repeats one task verbatim is
            // left to the `written` check below, which names that fault.
            let claimed = claims.entry(short_id).or_insert_with(|| id.clone());
            if *claimed != id {
                return Err(ApiError::conflict(format!(
                    "store.import: task {id} carries short_id {short_id}, which task {claimed} \
                     in the same payload already claims — one short_id addresses exactly one \
                     task, so this document cannot be restored anywhere"
                )));
            }
        }
        if let Some(above) = floor.and_then(|n| n.checked_add(1)) {
            bump_short_id_floor(&tx, above)?;
        }

        for tv in tasks {
            // Shape first, fields second: a non-object entry used to be
            // diagnosed by `req_str` as "missing required field: id", sending
            // the reader to hunt for a key in a value that cannot hold one.
            let tv = import_shape("", "task", tv)?;
            let id = req_str(tv, "id").map_err(|e| {
                ApiError::bad_request(format!(
                    "{} — each imported task requires a string `id`",
                    e.message
                ))
            })?;
            let id = id.as_str();
            // After `id`, so the error can name the task the caller must edit —
            // one bad field in a thousand-line export is useless without it.
            import_keys(&format!("task {id}, "), "task", tv, IMPORT_TASK_KEYS)?;
            // #180: a repeated primary `id` in one payload is the same fault
            // the short_id check above refuses one field over — two entries
            // claim to be the same task, and the upsert's `ON CONFLICT(id) DO
            // UPDATE` would otherwise apply both and silently keep only the
            // LAST, while `imported` still counted every entry it read rather
            // than every row it actually wrote. Checked here, before any of
            // this task's other fields are parsed, so a document with this
            // fault is refused without the wasted work of validating an entry
            // that could never be written anyway. `written` is a set, not a
            // scan of `edges`, so a 10k-task import pays one hash insert per
            // task rather than 10k² comparisons for a check that almost never
            // fires.
            if !written.insert(id.to_string()) {
                return Err(ApiError::conflict(format!(
                    "store.import: task {id} appears more than once in this document's `tasks` \
                     array — one id addresses exactly one task, so this document cannot be \
                     restored anywhere"
                )));
            }
            let short_id = import_field(id, "short_id", req_i64(tv, "short_id"))?;
            // D17's rule where the value ENTERS: `short_id` is untrusted i64 and
            // the mint floor is `short_id + 1`, which panicked in debug and
            // wrapped in release at `i64::MAX` — leaving a floor of `i64::MIN`, so
            // the next `add` re-minted a live short_id and broke D4. The counter
            // starts at 1 and only advances, so anything outside 1..i64::MAX is a
            // value no minter could have produced. The floor itself is raised
            // once, above the whole payload, before the loop (D177); this is the
            // range check that makes that addition safe, kept HERE because only
            // here can the error name the task the caller must edit.
            if short_id < 1 || short_id.checked_add(1).is_none() {
                return Err(ApiError::bad_request(format!(
                    "store.import: task {id} has short_id {short_id} — expected an integer \
                     from 1 to {}",
                    i64::MAX - 1
                )));
            }
            // D35 + D16: `task.add` refuses an empty title through `req_str`, so
            // import does too. `title: ""` used to store a titleless task that
            // `add` cannot create and every listing renders as a blank row.
            let title = import_field(id, "title", req_str(tv, "title"))?;
            // Validated, not carried verbatim: an unrecognized status used to be
            // written to the row as-is and then laundered back to `pending` by
            // `map_task_row`, so a `done` task with a mis-cased status resurfaced
            // as open work while still carrying `completed`. Reject like D12 does
            // for a bad reference, and store the canonical spelling so the reader
            // never has to guess.
            let raw_status = import_field(id, "status", opt_str_nonempty(tv, "status"))?
                .unwrap_or_else(|| "pending".to_string());
            let raw_status = raw_status.as_str();
            let status = Status::parse(raw_status)
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "store.import: task {id} has status {raw_status:?} — expected one of {}",
                        Status::ALL.map(Status::as_str).join(", ")
                    ))
                })?
                .as_str();
            let priority = match import_field(id, "priority", opt_str_nonempty(tv, "priority"))? {
                Some(raw) => {
                    Some(Priority::parse(&raw).map(Priority::as_str).ok_or_else(|| {
                        ApiError::bad_request(format!(
                            "store.import: task {id} has priority {raw:?} — expected one of {}",
                            Priority::ALL.map(Priority::as_str).join(", ")
                        ))
                    })?)
                }
                None => None,
            };
            // D35: D18's rule on the import path — `project: ""` used to become
            // NULL, the ghost-bucket state D18 exists to prevent.
            let project = import_field(id, "project", opt_str_nonempty(tv, "project"))?;
            // D37 / N3b: D23 closed this for `task.add` and `task.modify` — "an
            // unknown --project exits 4 naming it, because a typo lost the task
            // silently" — and left import open, so a payload could mint a task in
            // a bucket no project surface has ever heard of. What "closed" means
            // depends on what the document claimed:
            //
            //   * It declared its projects → it is authoritative, and naming a
            //     project it did not define is an incoherent document. Refuse,
            //     naming the task and the value, exactly as D23 does at the other
            //     two doors. This is the only way a typo in a hand-edited export
            //     is ever caught.
            //   * It declared none (an older tasqx) → there is nothing to be
            //     incoherent WITH, and refusing would make every legacy export
            //     unrestorable. Infer the row instead, so the store still ends up
            //     coherent — the name the import accepted is a name `add`
            //     accepts — and report the mint, because a record the caller did
            //     not send is not something to write silently.
            //
            // Existence only, never archived state: an export legitimately holds
            // done work in a retired project (D22 puts a project out of rotation
            // for NEW work), and refusing to restore history is not what that
            // rule says.
            if let Some(name) = &project {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE name = ?1)",
                    params![name],
                    |r| r.get(0),
                )?;
                if !exists {
                    if declared.is_some() {
                        return Err(ApiError::bad_request(format!(
                            "store.import: task {id} names project {name:?}, which the payload's \
                             `projects` section does not define and the store does not have — \
                             add it to `projects`, or create it first with `tasqx init {name}`"
                        )));
                    }
                    let row_id = upsert_project(&tx, name, None, false, &now(), None)?;
                    insert_event(
                        &tx,
                        Entity::Project,
                        &row_id,
                        "import",
                        &json!({ "name": name, "inferred": true }),
                    )?;
                    projects_created.push(name.clone());
                }
            }
            // The same gate the CLI, the JSON API and MCP pass, called on the
            // import payload itself rather than restated here: these four fields
            // used to be read raw into the INSERT, so `due whenever` entered a
            // store no reader downstream can compare, sort or render. Prefixing
            // the validator's own message is all the import layer adds — one
            // failing task out of a thousand-line export is useless without its
            // id and column.
            let now_ts = crate::clock::now();
            let due = import_field(id, "due", opt_when(tv, "due", now_ts))?;
            let scheduled = import_field(id, "scheduled", opt_when(tv, "scheduled", now_ts))?;
            let wait = import_field(id, "wait", opt_when(tv, "wait", now_ts))?;
            // The EXTRACTION is wrapped as well as the parse. These three read
            // their value outside `import_field`, so `estimate:""` was refused
            // by a message that never said which of a thousand exported tasks
            // held it, while `estimate:"3 fortnights"` — the same field, one
            // error path over — named it. Both doors now go through the wrapper
            // that exists for exactly this reason.
            let estimate = match import_field(id, "estimate", opt_str_nonempty(tv, "estimate"))? {
                Some(s) => Some(import_field(id, "estimate", datetime::parse_duration(&s))?),
                None => None,
            };
            // The last two fields D28 skipped, now through the SAME two parsers
            // `task.add` and `task.modify` already call — parse-then-normalize,
            // not a second spelling of the rule. Carried verbatim, these let a
            // payload write `remind:"sometime"` that `add` rejects, and then
            // re-export it to every downstream store (D16); unparseable, neither
            // field ever schedules, so the user is simply never reminded and
            // nothing says why. "Re-normalizing perturbs the round trip" was a
            // code comment, never a decision: normalization runs the same
            // functions that WROTE the stored form, so it is idempotent on
            // anything an export produced (D12 stays byte-identical).
            let recurrence =
                match import_field(id, "recurrence", opt_str_nonempty(tv, "recurrence"))? {
                    Some(s) => Some(
                        import_field(id, "recurrence", recur::parse_rule(&s))
                            .map(|r| recur::rule_to_string(&r))?,
                    ),
                    None => None,
                };
            // `parse_remind` validates the SHAPE without collapsing it: an
            // offset stays the symbolic `-1h` that re-anchors when `due` moves,
            // and only the absolute branch resolves — exactly as on `add`.
            // D139. Absent on a legacy export, which is NULL — no threshold,
            // which is the same thing the store said before the column existed.
            let budget_tokens =
                match import_field(id, "budget_tokens", opt_i64(tv, "budget_tokens"))? {
                    Some(n) if n < 0 => {
                        return Err(ApiError::bad_request(format!(
                            "task {id}: budget_tokens must be a non-negative integer"
                        )))
                    }
                    other => other,
                };
            // D165. Absent is NULL: no pin, and the card falls back to the
            // newest note, exactly as on a task completed before the pin.
            let delivered_annotation_id = import_field(
                id,
                "delivered_annotation_id",
                opt_str_nonempty(tv, "delivered_annotation_id"),
            )?;
            let spawned_from =
                import_field(id, "spawned_from", opt_str_nonempty(tv, "spawned_from"))?;
            let remind = match import_field(id, "remind", opt_str_nonempty(tv, "remind"))? {
                Some(s) => Some(
                    import_field(id, "remind", remind::parse_remind(&s, now_ts))
                        .map(|r| remind::spec_to_string(&r))?,
                ),
                None => None,
            };
            // Through the SAME gate as due/scheduled/wait above, not a second
            // spelling of it. B2 closed those four and left these three, so
            // `"created":"not-a-date"` still imported with rc=0 and came back out
            // of the next export verbatim — and `created` feeds `urgency::score`
            // three lines down, so the garbage silently flattened the ranking
            // every list is sorted by. The store writes all six as RFC3339, which
            // `parse_when` short-circuits on, so D12's byte-identical round trip
            // is untouched.
            let created =
                import_field(id, "created", opt_when(tv, "created", now_ts))?.unwrap_or_else(now);
            let modified =
                import_field(id, "modified", opt_when(tv, "modified", now_ts))?.unwrap_or_else(now);
            let completed = import_field(id, "completed", opt_when(tv, "completed", now_ts))?;
            let rev = import_field(id, "_rev", opt_i64(tv, "_rev"))?.unwrap_or(1);
            // Stays `Option` all the way to the bind: absent is not zero. A
            // negative total is refused rather than stored, because it would
            // make `report.summary`'s `tracked_total` subtract time.
            let tracked_seconds = import_field(
                id,
                "tracked_seconds",
                opt_i64(tv, "tracked_seconds").and_then(|v| match v {
                    Some(n) if n < 0 => Err(ApiError::bad_request(format!(
                        "tracked_seconds must not be negative, got {n}"
                    ))),
                    other => Ok(other),
                }),
            )?;
            // D166: signed, since a correction goes either way.
            let tracked_adjustment_seconds = import_field(
                id,
                "tracked_adjustment_seconds",
                opt_i64(tv, "tracked_adjustment_seconds"),
            )?;
            // Through the same date gate as created/modified/completed, so a
            // malformed anchor is named rather than stored.
            let active_since =
                import_field(id, "active_since", opt_when(tv, "active_since", now_ts))?;
            let urgency = urgency::score_at(
                priority.and_then(Priority::parse),
                due.as_deref(),
                &created,
                now_ts,
            );

            // D4/D177: `short_id` is the handle the whole CLI addresses a task
            // by, and the column is NOT NULL UNIQUE — so a payload carrying a
            // number some OTHER task in this store already holds cannot be
            // written under it. The upsert keys on `id`, which never noticed, and
            // the raw UNIQUE violation came back through `From<rusqlite::Error>`
            // as `internal` / exit 1: the code §4 reserves for "internal bug;
            // safe to retry-report", with a message naming neither task. It was
            // then a `conflict` whose only remedy was a store nobody had —
            // merging two machines' stores, or restoring a filtered export on
            // top of live work, is the normal case, not a user error.
            //
            // So the arriving task keeps its id and takes a fresh number
            // instead, and the move is reported. Nothing keys on `short_id`:
            // dependencies, annotations, checks, links and events all key on the
            // id, so the only thing a move costs is what a human wrote down.
            //
            // A task this store ALREADY holds does not go through any of it: it
            // keeps the number it is addressed by HERE, whatever the document
            // says. That is the rule that makes importing one document twice a
            // no-op rather than a walk up the counter, and it is why the stored
            // number is read FIRST — with the row already present, the owner
            // query below would only ever find the payload's number held by some
            // third task, which is no collision at all.
            let stored_short_id: Option<i64> = tx
                .query_row(
                    "SELECT short_id FROM tasks WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?;
            let short_id = match stored_short_id {
                Some(stored) => stored,
                // `.optional()`: no row is the normal case, and `query_row`
                // would otherwise raise `QueryReturnedNoRows`. The id is new
                // here, so any owner of the number is a DIFFERENT task.
                None => {
                    let owner: Option<String> = tx
                        .query_row(
                            "SELECT id FROM tasks WHERE short_id = ?1",
                            params![short_id],
                            |r| r.get(0),
                        )
                        .optional()?;
                    match owner {
                        None => short_id,
                        // Whoever holds the number here — a task that was
                        // already in this store, or one an earlier row of this
                        // payload just wrote — the arriving task is new and the
                        // number is taken, so it moves. The OTHER fault, a
                        // document that hands one number to two tasks, was
                        // settled over the whole payload before this loop
                        // started (#796); by here there is nothing left to tell
                        // apart.
                        Some(_) => {
                            let minted = alloc_short_id(&tx)?;
                            renumbered.push(json!({
                                "id": id,
                                "from": short_id,
                                "to": minted,
                            }));
                            minted
                        }
                    }
                }
            };

            // #177: a task ALREADY in this store, at a HIGHER `_rev` than the
            // payload's, means the payload is a stale copy of this very task —
            // annotations, tags and dependency edges added since the export
            // was taken are about to be replaced wholesale by the
            // DELETE+reinsert every child table below uses, and the caller
            // finds out only by noticing they are gone afterwards. `_rev` is
            // exported on every task for exactly this comparison (D13's
            // `expected_rev` guard already trusts it as one) and nothing here
            // ever consulted it. Refused by name, the same shape the short_id
            // collision just above already gets; a payload at or ahead of the
            // stored rev still passes, which is what keeps re-importing a
            // store's own export (D12's round trip) a no-op rather than a
            // refusal.
            //
            // D185: `merge` skips it outright for this task, and `modified`
            // decides instead. The two counters were minted independently, on
            // stores that never saw each other's writes, so a payload at a
            // lower `_rev` is not a stale copy of this task but a different
            // count of a different history — D183's reasoning for a doc's
            // `rev`, one table over — and nothing is discarded either way,
            // because every child table below is a union on a merge.
            let stored: Option<(i64, String)> = tx
                .query_row(
                    "SELECT rev, modified FROM tasks WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let merging = merge && stored.is_some();
            // D3's per-field last-writer-wins at the import door: `modified` is
            // the one stamp both machines mean the same thing by. A tie keeps
            // the store's copy, which is what makes importing one document
            // twice leave the scalars alone.
            let take_payload = match &stored {
                Some((_, held)) => parse_ts(&modified) > parse_ts(held),
                None => true,
            };
            let stored_rev = stored.as_ref().map(|(r, _)| *r);
            if let Some(stored) = stored_rev.filter(|_| !merging) {
                if stored > rev {
                    return Err(ApiError::conflict(format!(
                        "store.import: task {id} carries _rev {rev}, but the store already holds \
                         it at _rev {stored} — this payload is older than what is already here, \
                         and importing it would discard every annotation, tag and dependency \
                         edge added since (run `tasqx export` first for a merge target, or drop \
                         this task from the payload)"
                    )));
                }
            }

            // Upsert by id.
            // Both timing columns are driven by the payload's STATUS, not
            // written blindly:
            //
            // `active_since` — an open interval belongs to an `active` task and
            // to no other. Importing a terminal status over a running task used
            // to leave the live anchor in place (the SET list omitted the
            // column), producing a `done` task with an open interval: a state no
            // sequence of API calls can reach, that `task.reopen` does not
            // clear, that `task.stop` refuses to touch, and that the active
            // sweep never sees because it selects `WHERE status='active'`.
            // COALESCE prefers the payload's anchor, falls back to the one
            // already stored (so re-importing a store's own export while a timer
            // runs is a no-op), and only then to now.
            //
            // `tracked_seconds` — COALESCE, never a plain bind: a legacy export
            // has no such key, and reading absent as zero would wipe the live
            // total on every merge-import.
            //
            // `tracked_adjustment_seconds` travels with the total it is part
            // of: absent beside a present `tracked_seconds` means the payload's
            // total carries no correction (0); absent beside an absent total
            // keeps what the store holds, as the total itself does.
            //
            // D185: on a merge the row already exists and the scalars follow
            // the later `modified`, so this statement runs only when the
            // PAYLOAD is that side; when the store is, nothing here is
            // touched at all and only the child unions below happen. The
            // `rev` it writes is `max(stored, payload) + 1` — one past
            // whichever counter was higher, so neither store's own
            // `expected_rev` guard (D13) reads the merge as a rewind.
            let rev_target = match stored_rev.filter(|_| merging) {
                Some(held) => held.max(rev).saturating_add(1),
                None => rev,
            };
            let children_before = if merging {
                Self::child_row_count(&tx, id)?
            } else {
                0
            };
            if !merging || take_payload {
                tx.execute(
                    "INSERT INTO tasks (id, short_id, title, status, priority, project, due, \
                 scheduled, wait, estimate, recurrence, urgency, active_since, tracked_seconds, \
                 rev, created, modified, completed, remind, budget_tokens, \
                 delivered_annotation_id, tracked_adjustment_seconds, spawned_from) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12, \
                 CASE WHEN ?4 = 'active' THEN COALESCE(?18,?19) ELSE NULL END, \
                 COALESCE(?20,0),?13,?14,?15,?16,?17,?21,?22,COALESCE(?23,0),?24) \
                 ON CONFLICT(id) DO UPDATE SET \
                 title=?3, status=?4, priority=?5, project=?6, due=?7, \
                 scheduled=?8, wait=?9, estimate=?10, recurrence=?11, urgency=?12, \
                 active_since = CASE WHEN ?4 = 'active' \
                 THEN COALESCE(?18, active_since, ?19) ELSE NULL END, \
                 tracked_seconds = COALESCE(?20, tracked_seconds), \
                 rev=?13, created=?14, modified=?15, completed=?16, remind=?17, \
                 budget_tokens=?21, delivered_annotation_id=?22, \
                 tracked_adjustment_seconds = COALESCE(?23, \
                 CASE WHEN ?20 IS NULL THEN tracked_adjustment_seconds ELSE 0 END), \
                 spawned_from=?24",
                    params![
                        id,
                        short_id,
                        title,
                        status,
                        priority,
                        project,
                        due,
                        scheduled,
                        wait,
                        estimate,
                        recurrence,
                        urgency,
                        rev_target,
                        created,
                        modified,
                        completed,
                        remind,
                        active_since,
                        now(),
                        tracked_seconds,
                        budget_tokens,
                        delivered_annotation_id,
                        tracked_adjustment_seconds,
                        spawned_from
                    ],
                )?;
            }

            // Replace tags — or union them, on a merge: a tag set is D3's
            // "already commutes" case, so both sides' labels stand.
            if !merging {
                tx.execute("DELETE FROM task_tags WHERE task_id = ?1", params![id])?;
            }
            for tg in import_field(
                id,
                "tags",
                opt_str_array(tv, "tags").and_then(normalize_tags),
            )? {
                ensure_tag_link(&tx, id, &tg)?;
            }

            Self::import_annotations(&tx, id, tv, merging, take_payload)?;
            let checks_updated = Self::import_checks(&tx, id, tv, merging)?;
            Self::import_token_measurements(&tx, id, tv, merging)?;

            // Edges are deferred to pass 2: a payload may list a target *after*
            // its dependent, and the FOREIGN KEY would reject it here. A merge
            // keeps the destination's own edges and lets the payload's join
            // them, the way its tags do — pass 2's `INSERT OR IGNORE` already
            // makes the union idempotent, and the cycle and self-dependency
            // guards there run on the union rather than on the payload alone.
            if !merging {
                tx.execute("DELETE FROM dependencies WHERE task_id = ?1", params![id])?;
            }
            edges.push((
                id.to_string(),
                import_field(id, "depends_on", opt_str_array(tv, "depends_on"))?,
            ));
            if merging {
                // An upsert reports a row affected even when it rewrote the
                // same bytes, so "did this merge add anything" is counted from
                // the child rows themselves — plus the one change a count
                // cannot see, a check both stores held taking the payload's
                // state on its own `modified`. Pass 2 finishes the answer with
                // the edges it inserts.
                let changed = take_payload
                    || checks_updated
                    || Self::child_row_count(&tx, id)? != children_before;
                merged.push((
                    id.to_string(),
                    if take_payload { "payload" } else { "store" },
                    rev_target,
                    changed,
                ));
            }

            insert_event(
                &tx,
                Entity::Task,
                id,
                "import",
                &json!({ "short_id": short_id }),
            )?;
            imported += 1;
        }

        // Pass 2: wire the edges now that every task in the payload exists.
        // A target may live in the payload *or* already in the store (importing
        // one filtered slice on top of another is a normal workflow); anything
        // else is a dangling pointer and the import fails naming it. Silently
        // inserting it — the old behaviour — produced an edge no reader could
        // see and no `undep` could remove, which detonated the moment the target
        // finally arrived. Same transaction, so a reject writes nothing at all.
        for (id, deps) in &edges {
            for d in deps {
                let d = d.as_str();
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
                    params![d],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(ApiError::bad_request(format!(
                        "store.import: task {id} depends on {d}, which is neither in the \
                         payload nor in the store (export the dependency too, or drop the edge)"
                    )));
                }
                // Import is not a back door around the graph invariants that
                // `dependency.add` enforces. Without these two guards a payload
                // could mint a task blocked by itself, or a mutual cycle that
                // silently empties the working set — states the API itself calls
                // a conflict, and which re-export verbatim so the corruption
                // outlives the store that created it. The FOREIGN KEY above
                // constrains existence, not acyclicity.
                if d == id {
                    return Err(ApiError::conflict(format!(
                        "store.import: task {id} depends on itself — a task cannot depend on itself"
                    )));
                }
                // Same DFS `dependency.add` uses, on the transaction's own
                // write-locked snapshot: if the target already reaches the
                // dependent, this edge closes a cycle.
                if reaches(&tx, d, id)? {
                    return Err(ApiError::conflict(format!(
                        "store.import: dependency would create a cycle: \
                         {d} already depends on {id}"
                    )));
                }
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO dependencies (task_id, depends_on_id) VALUES (?1,?2)",
                    params![id, d],
                )?;
                // D185: an edge the destination did not have is the one change
                // a merge can make after the task loop already asked itself
                // whether anything changed.
                if inserted > 0 {
                    if let Some(e) = merged.iter_mut().find(|(m, ..)| m == id) {
                        e.3 = true;
                    }
                }
            }
        }
        // D185: the deferred half of the `rev` bump. A merge whose SCALARS came
        // from the payload already wrote `max(stored, payload) + 1` in the
        // upsert above; one that kept the store's row did not write to `tasks`
        // at all, so the counter moves here — and only when the union actually
        // brought something in, which is what keeps merging one document twice
        // a no-op rather than a walk up the counter.
        for (id, took, rev_target, changed) in &merged {
            if *took == "store" && *changed {
                tx.execute(
                    "UPDATE tasks SET rev = ?2 WHERE id = ?1",
                    params![id, rev_target],
                )?;
            }
        }
        // Pass 2b, D181: the graph's edges, after every task, annotation, doc
        // and project the document carries has been written — a link spans all
        // four kinds, and there is no FOREIGN KEY to lean on, because the
        // endpoint columns are polymorphic. Optional, so a document written
        // before this section existed still imports with nothing to restore.
        let mut links_imported = 0i64;
        // D181: payload link id -> the stored row that edge actually landed on.
        // Its own table rather than an entry in `remap`, because a link is not
        // a [`NodeType`] — nothing links TO a link — and only the staged link
        // events below read it.
        let mut link_remap: HashMap<String, String> = HashMap::new();
        // Link ids this payload has already written, so an id repeated in one
        // document (#180's rule for `tasks`, at this door too) is refused
        // rather than applied twice by the upsert below — which keeps only the
        // LAST while `links_imported` counts both. Only the PAYLOAD's own
        // repeats: an id already in the STORE is a re-import, which the upsert
        // answers as the no-op every other section here answers it as.
        let mut seen_links: HashSet<String> = HashSet::new();
        if let Some(rows) = opt_array(p, "links")?.cloned() {
            // #88's date gate for `created_at` below, on one instant for the
            // whole pass, as the task and doc passes read theirs.
            let now_ts = crate::clock::now();
            for lv in &rows {
                let lv = import_shape("", "link", lv)?;
                import_keys("", "link", lv, IMPORT_LINK_KEYS)?;
                let lid = opt_str_nonempty(lv, "id")?
                    .unwrap_or_else(|| crate::clock::uuid_v7().to_string());
                // Checked before this link's other fields are parsed, the order
                // the `tasks` pass checks its own duplicate in, and for its
                // reason: a document with this fault can never be restored
                // anywhere, so validating the rest of the entry is wasted work.
                if !seen_links.insert(lid.clone()) {
                    return Err(ApiError::conflict(format!(
                        "store.import: link {lid} appears more than once in this document's \
                         `links` array — one id addresses exactly one link, and the same \
                         payload cannot state two, so this document cannot be restored anywhere"
                    )));
                }
                let from = import_link_end(&tx, &lid, "from", &req_str(lv, "from")?, &remap)?;
                let to = import_link_end(&tx, &lid, "to", &req_str(lv, "to")?, &remap)?;
                // `link_add`'s invariant, restated here because import is not a
                // back door around it: an edge from a node to itself carries no
                // information and every traversal would have to special-case
                // it. Checked on the RESOLVED ends, so a payload naming one
                // node by two ids the remap folds together is caught too.
                if from == to {
                    return Err(ApiError::bad_request(format!(
                        "store.import: link {lid}: a node cannot link to itself ({})",
                        node_id(&from)
                    )));
                }
                // The closed vocabulary `link.add` enforces, on the same terms
                // every other import gate applies one (D16): carrying an
                // unknown relation verbatim would let one bad payload
                // re-export the corruption to every downstream store.
                let relation = req_str(lv, "relation")?;
                if !LINK_RELATIONS.contains(&relation.as_str()) {
                    return Err(ApiError::bad_request(format!(
                        "store.import: link {lid} has relation {relation:?} — expected one of {}",
                        LINK_RELATIONS.join(", ")
                    )));
                }
                // `link.add`'s rule, restated at this door and for its reason:
                // the caller's own JSON, stored verbatim, and a non-object
                // refused rather than wrapped.
                let metadata = match lv.get("metadata") {
                    None | Some(Value::Null) => None,
                    Some(v) if v.is_object() => Some(v.to_string()),
                    Some(other) => {
                        return Err(ApiError::bad_request(format!(
                            "store.import: link {lid}, metadata must be an object, but {} was \
                             given ({other})",
                            crate::util::type_of(other)
                        )))
                    }
                };
                // The date gate every other imported stamp passes (#88): a
                // `created_at` no reader can parse used to be written to the
                // column verbatim, so `link.list` handed it straight back as
                // garbage. `now()` only when the key is absent, which is what
                // lets a hand-written link omit it.
                let created =
                    import_link_field(&lid, "created_at", opt_when(lv, "created_at", now_ts))?
                        .unwrap_or_else(now);

                // The table's OTHER uniqueness — (from, to, relation) — which
                // an `ON CONFLICT(id)` upsert cannot see: a payload stating an
                // edge this store already holds under a different id is
                // stating something already true, so the stored row stands and
                // the import counts it rather than raising the raw constraint
                // violation as `internal`. Idempotence for the same reason
                // `link.add` answers `created: false` instead of erroring.
                let twin: Option<String> = tx
                    .query_row(
                        "SELECT id FROM links WHERE from_type = ?1 AND from_id = ?2 \
                           AND to_type = ?3 AND to_id = ?4 AND relation = ?5",
                        params![from.0.as_str(), from.1, to.0.as_str(), to.1, relation],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(other) = twin.filter(|other| *other != lid) {
                    // The one link remap that is not the identity, and the
                    // reason the table exists: the payload's link EVENTS name
                    // the payload's id, and the row this import kept is the
                    // store's. Filed under the payload's id they would describe
                    // a link nobody here holds, so `event.list` on the kept row
                    // answered nothing about what the import brought.
                    link_remap.insert(lid, other);
                    links_imported += 1;
                    continue;
                }
                // ON CONFLICT DO UPDATE, never INSERT OR REPLACE: the rule the
                // annotations upsert above states in full. Nothing writes
                // `created_by` here, so re-importing a row leaves whatever the
                // store holds and a new one takes the column's `'user'`
                // default — the value `link.add` is the only writer of.
                tx.execute(
                    "INSERT INTO links \
                     (id, from_type, from_id, to_type, to_id, relation, metadata, created) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
                     ON CONFLICT(id) DO UPDATE SET \
                     from_type=excluded.from_type, from_id=excluded.from_id, \
                     to_type=excluded.to_type, to_id=excluded.to_id, \
                     relation=excluded.relation, metadata=excluded.metadata, \
                     created=excluded.created",
                    params![
                        lid,
                        from.0.as_str(),
                        from.1,
                        to.0.as_str(),
                        to.1,
                        relation,
                        metadata,
                        created
                    ],
                )?;
                links_imported += 1;
            }
        }
        // Pass 2c, D181: the link events the events pass held back, now that
        // `link_remap` knows which row each payload link landed on. Same
        // `INSERT OR IGNORE` on the event's own id, and counted the same way —
        // only the entity_id passes through the table.
        for (eid, entity_id, op, payload, ts, actor) in link_events {
            let entity_id = link_remap.get(&entity_id).unwrap_or(&entity_id);
            // #800(c): `entity_id` above is the LINK's own remap; the event's
            // `payload` names its ends as free-standing `from`/`to` strings
            // that remap never touches. A doc end D183's merge folded away
            // still spells the dropped id there unless it is rewritten too.
            let payload = remap_memory_ends(&payload, &remap);
            let n = tx.execute(
                "INSERT OR IGNORE INTO events (id, entity, entity_id, op, payload, ts, actor) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    eid,
                    Entity::Link.as_str(),
                    entity_id,
                    op,
                    payload,
                    ts,
                    actor
                ],
            )?;
            events_imported += n as i64;
        }

        // Pass 3: the default, last, because it must be checked against the
        // projects this very transaction wrote. D21's rule — nothing silently
        // steals the default — applies here more than anywhere else: import is
        // the only write that can carry SOMEONE ELSE'S default in its payload,
        // and redirecting where a bare `add` lands is the invisible-write bug
        // D21 exists to kill. So the document's default is honoured only by a
        // store that has none; otherwise the standing one wins. It is validated
        // either way, so the same document is not coherent in one store and
        // incoherent in another.
        let standing = get_config(&tx, DEFAULT_PROJECT_KEY)?;
        if let Some(name) = &want_default {
            let row: Option<i64> = tx
                .query_row(
                    "SELECT archived FROM projects WHERE name = ?1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?;
            match row {
                None => {
                    return Err(ApiError::bad_request(format!(
                        "store.import: `default_project` names {name:?}, which the payload's \
                         `projects` section does not define and the store does not have"
                    )))
                }
                // D22: archived is out of rotation, so a default aimed at one is
                // a state no live sequence of calls can reach and the D23
                // open-time repair would undo on the next open anyway.
                Some(a) if a != 0 => {
                    return Err(ApiError::bad_request(format!(
                        "store.import: `default_project` names {name:?}, which is archived \
                         (an archived project cannot be the default)"
                    )))
                }
                Some(_) => {}
            }
            if standing.is_none() {
                set_config(&tx, DEFAULT_PROJECT_KEY, name)?;
            }
        }
        let default_project = standing.or_else(|| want_default.clone());
        // D184: the whole import above ran for real, inside this one
        // transaction; `dry_run` only decides whether it is kept. A rollback
        // undoes every row this call wrote AND every event it inserted, so
        // nothing survives to be listed by `event.list` or seen by another
        // connection.
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()?;
        }

        // All ten always present: a machine consumer must be able to tell "no
        // projects in the document" from "this build does not report them", the
        // same reason `dropped_dependencies` and `default_cleared` are never
        // omitted.
        //
        // `docs_declared` is #179: `docs_imported: 0` alone cannot tell "the
        // document had an empty `docs` section" from "the document had none at
        // all", and a human surface rendering "0" only in the first case made
        // the two indistinguishable on the one command whose entire job is
        // telling the caller what came back. This is the same PRESENCE-vs-
        // absence distinction `declared` already draws for `projects` above.
        Ok(json!({
            "imported": imported,
            "projects_imported": projects_imported,
            "projects_created": projects_created,
            "docs_imported": docs_imported,
            "docs_declared": docs_declared,
            "events_imported": events_imported,
            // D181: how many of the document's links this store now holds —
            // including one it already held under another id, which is the
            // same edge stated twice, not a second one.
            "links_imported": links_imported,
            "default_project": default_project,
            "renumbered": renumbered,
            // D183: every doc that merged onto a row this store already held
            // under the same `source`, and which copy's text won.
            "docs_merged": docs_merged,
            // D185: every task this store already held that `merge` unioned
            // rather than replaced, and which side's scalars stand. Always
            // present, empty when `merge` was not asked for or the document
            // carried nothing this store had seen.
            "merged": merged
                .iter()
                .map(|(id, took, ..)| json!({ "id": id, "took": took }))
                .collect::<Vec<Value>>(),
            // D184: always present, false on a real run — the same rule every
            // other additive result field in this answer already follows.
            "dry_run": dry_run,
        }))
    }
}

/// The task already holding child row `child_id` in `table` — `None` when
/// nothing does, and a refusal when it is a task other than `task` (#802).
///
/// Annotation and check ids are global primary keys, so a payload can name one
/// another task owns, and the upserts above would MOVE the row: the owner
/// loses a note or a criterion it still lists, and its search index entry or
/// its evidence follows the thief. One annotation, and one check, belongs to
/// exactly one task — `import_token_measurements` has refused the same theft
/// at its own door since it was written, and this is that refusal at the other
/// two. `table`, `field` and `noun` are literals from the two call sites,
/// never anything the payload supplies.
fn child_owner(
    tx: &rusqlite::Transaction,
    table: &str,
    field: &str,
    noun: &str,
    task: &str,
    child_id: &str,
) -> Result<Option<String>, ApiError> {
    let holder: Option<String> = tx
        .query_row(
            &format!("SELECT task_id FROM {table} WHERE id = ?1"),
            params![child_id],
            |r| r.get(0),
        )
        .optional()?;
    match &holder {
        Some(other) if other != task => Err(ApiError::bad_request(format!(
            "store.import: task {task}, {field} {child_id} already belongs to task {other} in \
             this store — one {noun} belongs to exactly one task (drop it from the payload, or \
             give it a fresh id)"
        ))),
        _ => Ok(holder),
    }
}

/// Resolve one end of an imported link to the row it names HERE (D181).
///
/// Three steps, in order: the `<type>:<uuid>` grammar `link.list` emits, then
/// the remap table (`store_import`'s `remap` — payload id to the id that row
/// actually landed under), then existence. A node the store does not hold after
/// all three is refused by name, the shape a dangling `depends_on` already
/// gets: an edge to an unknown id means the operator exported the wrong slice,
/// and inventing the node or dropping the edge would hide that (D12).
///
/// The reference is NOT resolved through `parse_node_ref_on`: that grammar
/// accepts a short id and a project NAME, which would silently re-point an
/// imported edge at whatever this store happens to hold under the same number
/// or name. A document states ids.
fn import_link_end(
    tx: &rusqlite::Transaction,
    link_id: &str,
    side: &str,
    reference: &str,
    remap: &HashMap<(NodeType, String), String>,
) -> Result<(NodeType, String), ApiError> {
    let malformed = || {
        ApiError::bad_request(format!(
            "store.import: link {link_id}, {side}: {reference:?} is not a node reference — \
             expected `<type>:<uuid>`, where type is {}",
            NodeType::accepted()
        ))
    };
    let (ty, payload_id) = reference.split_once(':').ok_or_else(malformed)?;
    let ty = NodeType::parse(ty).ok_or_else(malformed)?;
    let id = remap
        .get(&(ty, payload_id.to_string()))
        .map(String::as_str)
        .unwrap_or(payload_id);
    if !node_exists(tx, ty, id)? {
        return Err(ApiError::bad_request(format!(
            "store.import: link {link_id} points at {}:{id}, which is neither in the payload \
             nor in the store (export the {} too, or drop the link)",
            ty.as_str(),
            ty.as_str()
        )));
    }
    Ok((ty, id.to_string()))
}

/// #800(c): rewrite a staged link event's `payload` so a `memory` end D183's
/// docs pass merged away names the row it landed on, not the id the merge
/// dropped.
///
/// `entity_id` above is `store_import`'s pass 2c re-pointing the EVENT ROW
/// itself through `link_remap` — a different table, for a different reason
/// (the twin-link branch). This is one field over: `from`/`to` are
/// free-standing `<type>:<uuid>` strings INSIDE the payload's own JSON,
/// which no remap touches on its way to the `events` table. Only a `memory`
/// end with an id `remap` actually names is rewritten; any other shape —
/// unparsable JSON, no `from`/`to`, a task or project end, an id `remap`
/// does not carry — is passed through byte for byte, because this only ever
/// tightens a doc end onto the row the docs pass already merged it onto.
fn remap_memory_ends(payload: &str, remap: &HashMap<(NodeType, String), String>) -> String {
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(payload) else {
        return payload.to_string();
    };
    for key in ["from", "to"] {
        let Some((ty, id)) = obj
            .get(key)
            .and_then(Value::as_str)
            .and_then(|s| s.split_once(':'))
        else {
            continue;
        };
        if ty != NodeType::Memory.as_str() {
            continue;
        }
        if let Some(kept) = remap.get(&(NodeType::Memory, id.to_string())) {
            obj.insert(key.to_string(), json!(format!("memory:{kept}")));
        }
    }
    Value::Object(obj).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    /// A file-backed store, so a second connection can hold the write lock
    /// against it. `:memory:` is private to one connection, so the snapshot
    /// guarantees below are simply not observable there.
    struct TempStore {
        path: std::path::PathBuf,
    }

    impl TempStore {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            TempStore {
                path: std::env::temp_dir().join(format!(
                    "tasqx-transfer-{label}-{}-{n}.db",
                    std::process::id()
                )),
            }
        }

        fn open_engine(&self) -> Engine {
            Engine::open(self.path.to_str().expect("UTF-8 temp path")).expect("open test store")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            // -wal/-shm too: WAL leaves both beside the database file.
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }

    fn exported_task_count(e: &Engine) -> usize {
        e.store_export(&json!({})).expect("export")["tasks"]
            .as_array()
            .expect("tasks array")
            .len()
    }

    /// D177: a payload short_id that a DIFFERENT task in the destination already
    /// holds is not a fault to refuse — the id is the key, the number is a
    /// display handle, so the arriving task keeps its id, takes a fresh number
    /// and the move is reported. It used to hit the raw `tasks.short_id` UNIQUE
    /// constraint (`internal` / exit 1, naming no task), then a `conflict` whose
    /// only remedy was a store nobody had.
    #[test]
    fn store_import_renumbers_a_short_id_another_task_already_holds() {
        let e = Engine::open_in_memory().expect("open");
        let mine = e
            .task_add(&json!({ "title": "already here" }))
            .expect("add");
        let mine_id = mine["id"].as_str().expect("id").to_string();
        let taken = mine["short_id"].as_i64().expect("short_id");

        const THEIRS: &str = "0193aaaa-0000-7000-8000-00000000beef";
        let r = e
            .store_import(&json!({ "tasks": [
                { "id": THEIRS, "short_id": taken, "title": "from the other store" },
            ] }))
            .expect("a number this store already uses must move, not refuse");

        assert_eq!(r["imported"], json!(1), "{r}");
        let moved = r["renumbered"]
            .as_array()
            .expect("renumbered is always present");
        assert_eq!(moved.len(), 1, "exactly the one task that moved: {r}");
        assert_eq!(moved[0]["id"], json!(THEIRS), "{r}");
        assert_eq!(moved[0]["from"], json!(taken), "{r}");
        let to = moved[0]["to"].as_i64().expect("the number it took");
        assert_ne!(to, taken, "the whole point is that it moved: {r}");

        // The arriving task keeps its ID and is addressable under the new
        // number; the task already here keeps the number it was addressed by.
        let theirs = e
            .task_get(&json!({ "ref": THEIRS }))
            .expect("the imported task exists under its own id");
        assert_eq!(theirs["short_id"], json!(to), "{theirs}");
        let ours = e.task_get(&json!({ "ref": mine_id })).expect("get ours");
        assert_eq!(
            ours["short_id"],
            json!(taken),
            "an import must never move a number the destination was using: {ours}"
        );
    }

    /// The minted number must clear the WHOLE payload, not just what the store
    /// holds: minting into a number a LATER task in the same document still
    /// claims would only move the collision one row down, and the cascade would
    /// renumber tasks that had no conflict at all.
    #[test]
    fn store_import_mints_above_every_payload_short_id() {
        let e = Engine::open_in_memory().expect("open");
        e.task_add(&json!({ "title": "one" })).expect("add");
        e.task_add(&json!({ "title": "two" })).expect("add");

        const A: &str = "0193aaaa-0000-7000-8000-0000000000aa";
        const B: &str = "0193aaaa-0000-7000-8000-0000000000bb";
        let r = e
            .store_import(&json!({ "tasks": [
                { "id": A, "short_id": 1, "title": "collides" },
                { "id": B, "short_id": 50, "title": "free" },
            ] }))
            .expect("import");

        let a_now = e.task_get(&json!({ "ref": A })).expect("get a")["short_id"]
            .as_i64()
            .expect("short_id");
        assert!(
            a_now > 50,
            "a mint at or below 50 would collide with B: {a_now}"
        );
        assert_eq!(
            e.task_get(&json!({ "ref": B })).expect("get b")["short_id"],
            json!(50),
            "a number nothing holds is kept — only the collision moves"
        );
        assert_eq!(
            r["renumbered"],
            json!([{ "id": A, "from": 1, "to": a_now }]),
            "only the task that moved is reported: {r}"
        );

        // And the counter is left above the document, so the next `add` cannot
        // re-mint a number this import just wrote (D4).
        let next = e.task_add(&json!({ "title": "after" })).expect("add")["short_id"]
            .as_i64()
            .expect("short_id");
        assert!(next > a_now, "the next mint must clear the import: {next}");
    }

    /// A task this store already holds keeps the number it is addressed by here,
    /// whatever the document says — which is what makes importing one document
    /// twice a no-op instead of a second renumbering that walks the task's
    /// number up on every run.
    #[test]
    fn store_import_keeps_the_stored_short_id_for_a_known_id() {
        let e = Engine::open_in_memory().expect("open");
        e.task_add(&json!({ "title": "already here" }))
            .expect("add");

        const X: &str = "0193aaaa-0000-7000-8000-0000000000cc";
        // Stamps and rev spelled out, as a real export carries them: without
        // them the second import would write a fresh `modified` and the
        // comparison below would be measuring the clock, not the rule.
        let payload = json!({ "tasks": [{
            "id": X,
            "short_id": 1,
            "title": "from the other store",
            "created": "2026-09-10T08:00:00Z",
            "modified": "2026-09-10T08:00:00Z",
            "_rev": 1,
        }] });

        let first = e.store_import(&payload).expect("import");
        let n = first["renumbered"][0]["to"]
            .as_i64()
            .expect("the number it took");
        let before = e.task_get(&json!({ "ref": X })).expect("get");

        let again = e.store_import(&payload).expect("second import");
        assert_eq!(
            again["renumbered"],
            json!([]),
            "a task this store already holds has nothing to move: {again}"
        );
        let after = e.task_get(&json!({ "ref": X })).expect("get");
        assert_eq!(after["short_id"], json!(n), "the stored number stands");
        assert_eq!(
            after, before,
            "re-importing the same document must change nothing at all, `_rev` included"
        );
        assert_eq!(exported_task_count(&e), 2, "and mint no second row");
    }

    /// Nothing keys on `short_id`: a dependency edge is stored by id, so both
    /// ends of it can move in one import and the graph still reads.
    #[test]
    fn store_import_dependencies_survive_renumbering() {
        let e = Engine::open_in_memory().expect("open");
        e.task_add(&json!({ "title": "one" })).expect("add");
        e.task_add(&json!({ "title": "two" })).expect("add");

        const A: &str = "0193aaaa-0000-7000-8000-0000000000a1";
        const B: &str = "0193aaaa-0000-7000-8000-0000000000b2";
        let r = e
            .store_import(&json!({ "tasks": [
                { "id": A, "short_id": 1, "title": "dependent", "depends_on": [B] },
                { "id": B, "short_id": 2, "title": "blocker" },
            ] }))
            .expect("import");
        assert_eq!(
            r["renumbered"].as_array().map(Vec::len),
            Some(2),
            "both numbers were taken here: {r}"
        );

        let b_now = e.task_get(&json!({ "ref": B })).expect("get b")["short_id"]
            .as_i64()
            .expect("short_id");
        let a = e.task_get(&json!({ "ref": A })).expect("get a");
        assert_eq!(
            a["depends_on"],
            json!([b_now]),
            "the edge must follow the id and be read back under the NEW number: {a}"
        );
        assert_eq!(a["blocked"], json!(true), "{a}");
    }

    /// #796: the second round trip between two machines. X was renumbered HERE
    /// by an earlier import, so the payload's number for it is stale and the
    /// stored one stands (D177) — and a LATER new task carrying the number X
    /// now holds is a plain destination collision, not a payload that claims
    /// one number twice. Deciding that from the ids this import had already
    /// written made the answer depend on row order: refused one way round,
    /// renumbered the other.
    #[test]
    fn store_import_renumbers_a_new_task_whose_number_a_known_task_holds_in_either_row_order() {
        const X: &str = "0193aaaa-0000-7000-8000-0000000000f1";
        const Z: &str = "0193aaaa-0000-7000-8000-0000000000f2";

        // Two stores set up identically, so the only difference between them is
        // the order of the second payload's rows.
        let prepare = || {
            let e = Engine::open_in_memory().expect("open");
            e.task_add(&json!({ "title": "already here" }))
                .expect("add");
            let first = e
                .store_import(&json!({ "tasks": [
                    { "id": X, "short_id": 1, "title": "from the other store" },
                ] }))
                .expect("import");
            let n = first["renumbered"][0]["to"]
                .as_i64()
                .expect("X took a fresh number here");
            (e, n)
        };
        let (forward, n) = prepare();
        let (reversed, n_again) = prepare();
        assert_eq!(n, n_again, "the two stores must start identical");
        assert_ne!(n, 3, "the stale number below must not be X's own");

        // X carries the number its OWN store still uses; Z, new here, carries
        // the number X was given on this machine.
        let x_row = json!({ "id": X, "short_id": 3, "title": "from the other store" });
        let z_row = json!({ "id": Z, "short_id": n, "title": "new over there" });
        let mut seen = Vec::new();
        for (e, tasks) in [
            (&forward, json!([x_row, z_row])),
            (&reversed, json!([z_row, x_row])),
        ] {
            let r = e
                .store_import(&json!({ "tasks": tasks }))
                .expect("a number a KNOWN task holds is a collision to renumber, not a refusal");
            assert_eq!(r["imported"], json!(2), "{r}");
            let x_now = e.task_get(&json!({ "ref": X })).expect("get x")["short_id"]
                .as_i64()
                .expect("short_id");
            assert_eq!(x_now, n, "a known task keeps its stored number (D177): {r}");
            let z_now = e.task_get(&json!({ "ref": Z })).expect("get z")["short_id"]
                .as_i64()
                .expect("short_id");
            assert!(
                z_now > n,
                "Z must take a fresh number above the payload: {r}"
            );
            assert_eq!(
                r["renumbered"],
                json!([{ "id": Z, "from": n, "to": z_now }]),
                "only the new task moved: {r}"
            );
            seen.push((x_now, z_now));
        }
        assert_eq!(
            seen[0], seen[1],
            "the same document in either row order must land the same numbers"
        );
    }

    /// Two payload tasks claiming one number is an incoherent document whatever
    /// the DESTINATION holds: when the number is also taken here, both used to
    /// be quietly renumbered and accepted, because each looked like an ordinary
    /// collision with the store. Checked over the whole payload before any row
    /// is written, so neither lands.
    #[test]
    fn store_import_refuses_two_new_tasks_claiming_a_number_the_destination_holds() {
        let e = Engine::open_in_memory().expect("open");
        let taken = e
            .task_add(&json!({ "title": "already here" }))
            .expect("add")["short_id"]
            .as_i64()
            .expect("short_id");

        const A: &str = "0193aaaa-0000-7000-8000-0000000000e1";
        const B: &str = "0193aaaa-0000-7000-8000-0000000000e2";
        let err = e
            .store_import(&json!({ "tasks": [
                { "id": A, "short_id": taken, "title": "first claimant" },
                { "id": B, "short_id": taken, "title": "second claimant" },
            ] }))
            .expect_err("one short_id cannot address two tasks");

        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert!(err.message.contains(A), "{}", err.message);
        assert!(err.message.contains(B), "{}", err.message);
        assert_eq!(
            e.task_get(&json!({ "ref": A })).unwrap_err().code,
            ErrorCode::NotFound,
            "a refused document writes nothing"
        );
        assert_eq!(
            e.task_get(&json!({ "ref": B })).unwrap_err().code,
            ErrorCode::NotFound,
            "a refused document writes nothing"
        );
        assert_eq!(exported_task_count(&e), 1);
    }

    /// The same guard, one step earlier: two payload tasks claiming one short_id
    /// is an incoherent document, caught over the whole `tasks` array before any
    /// row is written. A DIFFERENT fault from
    /// the one above, so it must not be diagnosed with the same sentence:
    /// "import into a fresh store" fixes nothing when both claimants arrived in
    /// the same payload.
    #[test]
    fn store_import_refuses_two_payload_tasks_sharing_one_short_id() {
        let e = Engine::open_in_memory().expect("open");
        const FIRST: &str = "0193aaaa-0000-7000-8000-000000000001";
        const SECOND: &str = "0193aaaa-0000-7000-8000-000000000002";
        let err = e
            .store_import(&json!({ "tasks": [
                { "id": FIRST, "short_id": 7, "title": "first" },
                { "id": SECOND, "short_id": 7, "title": "second" },
            ] }))
            .expect_err("one short_id cannot address two tasks");

        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert!(err.message.contains(FIRST), "{}", err.message);
        assert!(err.message.contains(SECOND), "{}", err.message);
        assert!(
            err.message.contains("same payload") && !err.message.contains("fresh store"),
            "a payload that contradicts itself is not fixed by importing it elsewhere: {}",
            err.message
        );
        assert_eq!(exported_task_count(&e), 0);
    }

    /// The guard is keyed on the OWNER, not on the number: re-importing a
    /// document over the store it came from updates each task in place, which is
    /// D12's round trip and the "restore on top of itself" workflow.
    #[test]
    fn store_import_still_accepts_a_task_reclaiming_its_own_short_id() {
        let e = Engine::open_in_memory().expect("open");
        e.task_add(&json!({ "title": "one" })).expect("add");
        e.task_add(&json!({ "title": "two" })).expect("add");
        let document = e.store_export(&json!({})).expect("export");

        let again = e
            .store_import(&document)
            .expect("re-import must still work");
        assert_eq!(again["imported"], 2);
        assert_eq!(
            e.store_export(&json!({})).expect("export"),
            document,
            "export -> import -> export stays identity"
        );
    }

    /// D109: `store.export`'s `filter` is the third of the three call sites
    /// sharing `validate_filter_projects` (`task.list`, `report.summary` are
    /// the other two). An unknown/wrong-case `project:` must refuse here too.
    #[test]
    fn store_export_refuses_a_project_filter_naming_no_live_project() {
        let e = Engine::open_in_memory().expect("open");
        e.project_create(&json!({ "name": "work" }))
            .expect("create project");
        e.task_add(&json!({ "title": "t", "project": "work" }))
            .expect("add");

        let err = e
            .store_export(&json!({ "filter": "project:WORK" }))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.message.contains("WORK"), "{}", err.message);

        // The exact, correctly-cased name still exports, as always.
        let out = e
            .store_export(&json!({ "filter": "project:work" }))
            .expect("export");
        assert_eq!(out["tasks"].as_array().unwrap().len(), 1);
    }

    /// `store_export` must read all of its statements from ONE snapshot: in WAL
    /// each statement otherwise takes its own, so a writer committing between
    /// them tears the document (tasks read before a `done`, annotations after).
    /// Structural, because the interleaving point is inside SQLite and rusqlite's
    /// `hooks` feature — the only way to drive a write from between two of our
    /// reads — is not compiled in. The companion test below pins the two things
    /// about the guard that ARE observable.
    #[test]
    fn store_export_opens_its_snapshot_before_the_first_read() {
        let source = include_str!("transfer.rs");
        // Assembled, never written out literally: `dispatch`'s accepted-key
        // guard splits this same source at every `fn NAME(`, so a marker spelled
        // in full would register HERE as a second definition of the handler and
        // overwrite the real one — with a body that reads no params at all.
        let marker = format!("pub fn {}(", "store_export");
        let marker = marker.as_str();
        let start = source.find(marker).expect("store_export exists");
        let rest = &source[start..];
        let end = rest[marker.len()..]
            .find("\n    fn ")
            .map(|offset| marker.len() + offset)
            .unwrap_or(rest.len());
        // Comments out: this function's own prose names the two constructors it
        // deliberately does NOT use, and a scanner that cannot tell code from a
        // comment would read that as the defect it is warning about.
        let body: String = rest[..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = body.as_str();

        let guard = body
            .find("unchecked_transaction()")
            .expect("store_export must open a transaction so its reads share one snapshot");
        let first_read = body
            .find("self.load_task_snapshots(")
            .expect("store_export loads the task relation");
        assert!(
            guard < first_read,
            "the snapshot pins at the first read, so the transaction must be opened before it"
        );
        assert!(
            !body.contains("let _ = self.conn.unchecked_transaction"),
            "a `_` binding drops the transaction on the spot, making the guard a no-op"
        );
        // DEFERRED, never IMMEDIATE: an exporting reader that takes the write
        // lock blocks every writer for the length of the export, which is
        // exactly what §2's "concurrent readers never block" forbids.
        for forbidden in ["begin_mutation", "Immediate"] {
            assert!(
                !body.contains(forbidden),
                "store_export must not take the write lock ({forbidden})"
            );
        }
    }

    /// The two observable halves of the same guard: it must not take the write
    /// lock (or this export would wait out `busy_timeout` and fail while another
    /// process holds it), and it must be released when the export returns (a
    /// leaked transaction makes the next `BEGIN IMMEDIATE` fail outright).
    #[test]
    fn store_export_neither_takes_the_write_lock_nor_leaks_its_transaction() {
        let store = TempStore::new("export-snapshot");
        let e = store.open_engine();
        e.task_add(&json!({ "title": "one" })).expect("add");

        let blocker = Connection::open(&store.path).expect("second connection");
        blocker
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold the write lock");

        let exported = e
            .store_export(&json!({}))
            .expect("a read must not wait on a writer");
        assert_eq!(exported["tasks"].as_array().expect("tasks").len(), 1);

        blocker.execute_batch("ROLLBACK").expect("release");
        drop(blocker);

        e.task_add(&json!({ "title": "two" }))
            .expect("add after export");
        assert_eq!(exported_task_count(&e), 2);
    }

    /// #180: a payload with three task entries but only two distinct ids (the
    /// third repeats the second's id under a different title) used to be
    /// accepted last-write-wins — "task 2" silently discarded, the store left
    /// with 2 rows while the result claimed 3 imported. A repeated `id` is the
    /// same fault the short_id collision below already refuses, one field
    /// over.
    #[test]
    fn store_import_refuses_a_duplicate_task_id_in_one_payload() {
        let e = Engine::open_in_memory().expect("open");
        const FIRST: &str = "0193aaaa-0000-7000-8000-0000000000f1";
        const SECOND: &str = "0193aaaa-0000-7000-8000-0000000000f2";
        let err = e
            .store_import(&json!({ "tasks": [
                { "id": FIRST, "short_id": 1, "title": "task 1" },
                { "id": SECOND, "short_id": 2, "title": "task 2" },
                { "id": SECOND, "short_id": 3, "title": "dup second" },
            ] }))
            .expect_err("a repeated id must not be accepted");

        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert!(err.message.contains(SECOND), "{}", err.message);
        assert!(
            err.message.contains("more than once"),
            "the message must say the id repeats: {}",
            err.message
        );
        // Same transaction, so the refusal writes nothing — not even the
        // first, non-conflicting task.
        assert_eq!(exported_task_count(&e), 0);
    }

    /// #177: restoring an OLDER export over a store that has since gathered
    /// more annotations and a tag used to silently regress the task to the
    /// stale snapshot — the newer child rows are DELETEd and reinserted from
    /// the payload's own (older) list, with no refusal and no report of what
    /// was lost. `_rev` is exported on every task and bumped by every one of
    /// those writes (D13's `expected_rev` already trusts it as an optimistic-
    /// concurrency token); this pins that a stale payload is refused by name
    /// instead, and that the refusal writes NOTHING — the live annotations and
    /// tag survive exactly as they were.
    #[test]
    fn store_import_refuses_a_stale_rev_that_would_discard_newer_annotations_and_tags() {
        let e = Engine::open_in_memory().expect("open");
        let added = e.task_add(&json!({ "title": "the task" })).expect("add");
        let sid = added["short_id"].as_i64().expect("short_id");
        e.annotation_add(&json!({ "ref": sid, "body": "monday: original context" }))
            .expect("annotate");

        // The "monday" backup: _rev 2 (add, then one annotation).
        let monday = e.store_export(&json!({})).expect("export");
        assert_eq!(monday["tasks"][0]["_rev"], json!(2), "{monday}");

        // A week of work happens: two more annotations and a tag, each
        // bumping `_rev` past what the backup carries.
        e.annotation_add(&json!({ "ref": sid, "body": "tuesday: found the root cause" }))
            .expect("annotate");
        e.annotation_add(&json!({ "ref": sid, "body": "wednesday: fix landed" }))
            .expect("annotate");
        e.tag_add(&json!({ "ref": sid, "tags": ["shipped"] }))
            .expect("tag");
        let live = e.store_export(&json!({})).expect("export");
        assert_eq!(live["tasks"][0]["_rev"], json!(5), "{live}");
        assert_eq!(live["tasks"][0]["annotations"].as_array().unwrap().len(), 3);

        // Restoring the older backup must be refused, not silently applied.
        let err = e
            .store_import(&monday)
            .expect_err("an older _rev must not overwrite a newer task");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        for needle in ["_rev 2", "_rev 5"] {
            assert!(err.message.contains(needle), "{}: {}", needle, err.message);
        }

        // The refusal wrote NOTHING: the annotations and tag from "the week
        // of work" are exactly as they were.
        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(after, live, "a refused import must not touch the store");
    }

    /// D185: the destination's own notes and checks survive a merge, and the
    /// payload's arrive beside them. Without `merge` the child tables are
    /// DELETEd and reinserted from the payload (D138), so whichever side's
    /// export lost the `_rev` race lost every note written since — the defect
    /// that made two live stores unmergeable in either direction.
    #[test]
    fn store_import_merge_keeps_notes_and_checks_written_on_both_sides() {
        let a = Engine::open_in_memory().expect("open a");
        let added = a.task_add(&json!({ "title": "shared work" })).expect("add");
        let sid = added["short_id"].as_i64().expect("short_id");
        a.annotation_add(&json!({ "ref": sid, "body": "N0 the original context" }))
            .expect("N0");
        a.check_add(&json!({ "ref": sid, "body": "C0 the original criterion" }))
            .expect("C0");

        // One export seeds the second machine, so both hold the same task
        // under the same id, with the same annotation and check.
        let seed = a.store_export(&json!({})).expect("export the seed");
        let b = Engine::open_in_memory().expect("open b");
        b.store_import(&seed).expect("seed b");

        a.annotation_add(&json!({ "ref": sid, "body": "NA written on machine A" }))
            .expect("NA");
        a.check_add(&json!({ "ref": sid, "body": "CA claimed on machine A" }))
            .expect("CA");
        b.annotation_add(&json!({ "ref": sid, "body": "NB written on machine B" }))
            .expect("NB");
        b.check_add(&json!({ "ref": sid, "body": "CB claimed on machine B" }))
            .expect("CB");

        let mut payload = a.store_export(&json!({})).expect("export a");
        payload["merge"] = json!(true);
        b.store_import(&payload).expect("a merge must not refuse");

        let after = b.store_export(&json!({})).expect("export b");
        let bodies: Vec<&str> = after["tasks"][0]["annotations"]
            .as_array()
            .expect("annotations")
            .iter()
            .filter_map(|a| a["body"].as_str())
            .collect();
        for note in [
            "N0 the original context",
            "NA written on machine A",
            "NB written on machine B",
        ] {
            assert_eq!(
                bodies.iter().filter(|b| **b == note).count(),
                1,
                "{note} must survive exactly once: {bodies:?}"
            );
        }
        assert_eq!(bodies.len(), 3, "and nothing else: {bodies:?}");

        let checks: Vec<&str> = after["tasks"][0]["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .filter_map(|c| c["body"].as_str())
            .collect();
        for claim in [
            "C0 the original criterion",
            "CA claimed on machine A",
            "CB claimed on machine B",
        ] {
            assert_eq!(
                checks.iter().filter(|c| **c == claim).count(),
                1,
                "{claim} must survive exactly once: {checks:?}"
            );
        }
        assert_eq!(checks.len(), 3, "and nothing else: {checks:?}");
    }

    /// Two stores holding one task, with one annotation under one id and two
    /// different bodies. The store is seeded from the payload, so the ids
    /// match; `payload_modified` then decides which side wrote last, and it
    /// is the TASK's stamp because an annotation carries only `created`.
    fn one_note_under_two_bodies(payload_modified: &str) -> (Engine, Value) {
        let a = Engine::open_in_memory().expect("open a");
        let added = a.task_add(&json!({ "title": "shared work" })).expect("add");
        a.annotation_add(&json!({
            "ref": added["short_id"].clone(),
            "body": "the body the store holds",
        }))
        .expect("note");
        let mut payload = a.store_export(&json!({})).expect("export");
        let b = Engine::open_in_memory().expect("open b");
        b.store_import(&payload).expect("seed b");
        payload["tasks"][0]["annotations"][0]["body"] = json!("the body the payload carries");
        payload["tasks"][0]["modified"] = json!(payload_modified);
        payload["merge"] = json!(true);
        (b, payload)
    }

    /// The annotations of the merged task, body-only, in export order.
    fn merged_note_bodies(b: &Engine) -> Vec<String> {
        b.store_export(&json!({})).expect("export b")["tasks"][0]["annotations"]
            .as_array()
            .expect("annotations")
            .iter()
            .map(|a| a["body"].as_str().expect("body").to_string())
            .collect()
    }

    /// D185 (task #802): an annotation carries `created` and nothing else, so
    /// there is no per-row stamp to resolve a note both stores hold under one
    /// id — it follows the task-level winner. The store won here, so an older
    /// replica's copy of the note does not overwrite the edit written since.
    #[test]
    fn store_import_merge_keeps_the_stored_note_body_when_the_store_won() {
        let (b, payload) = one_note_under_two_bodies("2000-01-01T00:00:00Z");
        b.store_import(&payload).expect("a merge must not refuse");
        assert_eq!(
            merged_note_bodies(&b),
            ["the body the store holds"],
            "one id is one note, and the store's copy is the later write"
        );
    }

    /// The other side of the same rule: the payload's task is the later write,
    /// so its copy of the note stands.
    #[test]
    fn store_import_merge_takes_the_payload_note_body_when_the_payload_won() {
        let (b, payload) = one_note_under_two_bodies("2099-01-01T00:00:00Z");
        b.store_import(&payload).expect("a merge must not refuse");
        assert_eq!(
            merged_note_bodies(&b),
            ["the body the payload carries"],
            "one id is one note, and the payload's copy is the later write"
        );
    }

    /// D185 (task #802): a check carries its OWN `modified`, so it resolves on
    /// that stamp rather than on the task-level winner. The payload's task is
    /// the later write here — its title lands — and its copy of the check is
    /// still the older one, so a `passed` this store recorded is not rolled
    /// back by a machine that never saw it pass. The reverse import proves the
    /// same stamp lets a newer check through while the store holds the task.
    #[test]
    fn store_import_merge_updates_a_check_only_when_its_own_modified_is_newer() {
        let a = Engine::open_in_memory().expect("open a");
        let added = a
            .task_add(&json!({ "title": "the title both stores start with" }))
            .expect("add");
        let sid = added["short_id"].clone();
        a.check_add(&json!({ "ref": sid, "body": "the criterion" }))
            .expect("check");
        let mut payload = a.store_export(&json!({})).expect("export");
        let b = Engine::open_in_memory().expect("open b");
        b.store_import(&payload).expect("seed b");
        let cid = payload["tasks"][0]["checks"][0]["id"].clone();
        b.check_set(&json!({
            "ref": sid,
            "check_id": cid,
            "state": "passed",
            "evidence": "the proof only b saw",
        }))
        .expect("passed on b");

        payload["tasks"][0]["title"] = json!("the title the payload wrote later");
        payload["tasks"][0]["modified"] = json!("2099-01-01T00:00:00Z");
        payload["tasks"][0]["checks"][0]["evidence"] = json!("nothing was proven here");
        payload["tasks"][0]["checks"][0]["modified"] = json!("2000-01-01T00:00:00Z");
        payload["merge"] = json!(true);
        b.store_import(&payload).expect("a merge must not refuse");

        let after = b.store_export(&json!({})).expect("export b");
        assert_eq!(
            after["tasks"][0]["title"],
            json!("the title the payload wrote later"),
            "precondition: the payload is the task-level winner: {after}"
        );
        assert_eq!(
            after["tasks"][0]["checks"][0]["state"],
            json!("passed"),
            "an older check must not roll the stored state back: {after}"
        );
        assert_eq!(
            after["tasks"][0]["checks"][0]["evidence"],
            json!("the proof only b saw"),
            "nor take its evidence: {after}"
        );

        // The reverse. The task's stamp is now a tie, so the STORE holds the
        // scalars, and the check still lands because its own stamp is later.
        payload["tasks"][0]["checks"][0]["modified"] = json!("2100-01-01T00:00:00Z");
        b.store_import(&payload).expect("a merge must not refuse");
        let after = b.store_export(&json!({})).expect("export b");
        assert_eq!(
            after["tasks"][0]["checks"][0]["state"],
            json!("open"),
            "a newer check is the later write, whoever holds the task: {after}"
        );
        assert_eq!(
            after["tasks"][0]["checks"][0]["evidence"],
            json!("nothing was proven here"),
            "and it carries its own evidence with it: {after}"
        );
    }

    /// D185 (task #802): annotation ids are global primary keys, and one
    /// annotation belongs to exactly one task. A payload handing another
    /// task's note id to a task of its own is refused rather than silently
    /// moving the row — the owner would lose a note it still lists, and the
    /// search index would follow the thief. Same rule with `merge` and
    /// without; this is the plain restore path, and the check twin below is
    /// the merge one.
    #[test]
    fn store_import_refuses_an_annotation_id_that_belongs_to_another_task() {
        let e = Engine::open_in_memory().expect("open");
        let owner = e
            .task_add(&json!({ "title": "owns the note" }))
            .expect("add");
        let note = e
            .annotation_add(
                &json!({ "ref": owner["short_id"].clone(), "body": "the owner's note" }),
            )
            .expect("note")["annotation"]["id"]
            .as_str()
            .expect("annotation id")
            .to_string();
        let thief = e.task_add(&json!({ "title": "claims it" })).expect("add");

        let document = json!({ "tasks": [{
            "id": thief["id"],
            "short_id": thief["short_id"],
            "title": "claims it",
            "created": "2026-09-16T09:00:00Z",
            "modified": "2026-09-16T09:00:00Z",
            "_rev": 99,
            "annotations": [{ "id": note, "body": "the note, stolen" }],
        }] });
        let err = e
            .store_import(&document)
            .expect_err("one annotation belongs to exactly one task");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        for named in [note.as_str(), owner["id"].as_str().expect("id")] {
            assert!(
                err.message.contains(named),
                "the refusal must name the note and the task holding it ({named}): {}",
                err.message
            );
        }
        let after = e.store_export(&json!({})).expect("export");
        let held = after["tasks"]
            .as_array()
            .expect("tasks")
            .iter()
            .find(|t| t["id"] == owner["id"])
            .expect("the owner is still here")
            .clone();
        assert_eq!(
            held["annotations"][0]["body"],
            json!("the owner's note"),
            "a refused import moves nothing: {after}"
        );
    }

    /// The check twin of the annotation refusal above, through the `merge`
    /// door: a check id is a global primary key too, and a payload cannot hand
    /// one task's criterion — with its state and its evidence — to another.
    #[test]
    fn store_import_merge_refuses_a_check_id_that_belongs_to_another_task() {
        let e = Engine::open_in_memory().expect("open");
        let owner = e
            .task_add(&json!({ "title": "owns the criterion" }))
            .expect("add");
        let check = e
            .check_add(
                &json!({ "ref": owner["short_id"].clone(), "body": "the owner's criterion" }),
            )
            .expect("check")["check"]["id"]
            .as_str()
            .expect("check id")
            .to_string();
        let thief = e.task_add(&json!({ "title": "claims it" })).expect("add");

        let document = json!({ "merge": true, "tasks": [{
            "id": thief["id"],
            "short_id": thief["short_id"],
            "title": "claims it",
            "created": "2026-09-16T09:00:00Z",
            "modified": "2026-09-16T09:00:00Z",
            "_rev": 99,
            "checks": [{ "id": check, "body": "the criterion, stolen", "state": "passed" }],
        }] });
        let err = e
            .store_import(&document)
            .expect_err("one check belongs to exactly one task");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        for named in [check.as_str(), owner["id"].as_str().expect("id")] {
            assert!(
                err.message.contains(named),
                "the refusal must name the check and the task holding it ({named}): {}",
                err.message
            );
        }
        let after = e.store_export(&json!({})).expect("export");
        let held = after["tasks"]
            .as_array()
            .expect("tasks")
            .iter()
            .find(|t| t["id"] == owner["id"])
            .expect("the owner is still here")
            .clone();
        assert_eq!(
            held["checks"][0]["body"],
            json!("the owner's criterion"),
            "a refused import moves nothing: {after}"
        );
        assert_eq!(held["checks"][0]["state"], json!("open"), "{after}");
    }

    /// D185 (task #802): a merge skips a measurement this task already holds,
    /// which is what makes importing one document twice a no-op — but the skip
    /// must not swallow a payload that lists one `tokens[].id` TWICE. That
    /// document cannot be applied: one measurement id is one row of spend, and
    /// taking the first silently drops the second.
    #[test]
    fn store_import_merge_refuses_a_token_id_repeated_in_one_payload() {
        let a = Engine::open_in_memory().expect("open a");
        let added = a
            .task_add(&json!({ "title": "spends tokens" }))
            .expect("add");
        a.token_add(&json!({
            "ref": added["short_id"].clone(),
            "tool": "claude-code",
            "source": "self-report",
            "confidence": "medium",
            "input_tokens": 100,
        }))
        .expect("token.add");
        let mut payload = a.store_export(&json!({})).expect("export");
        let b = Engine::open_in_memory().expect("open b");
        b.store_import(&payload).expect("seed b");
        payload["merge"] = json!(true);

        let measurements = |e: &Engine| -> usize {
            e.store_export(&json!({})).expect("export")["tasks"][0]["tokens"]
                .as_array()
                .map_or(0, Vec::len)
        };
        b.store_import(&payload)
            .expect("the same measurement arriving again is a skip, not an error");
        assert_eq!(measurements(&b), 1, "and it is skipped exactly once");

        let m = payload["tasks"][0]["tokens"][0].clone();
        let mid = m["id"].as_str().expect("measurement id").to_string();
        payload["tasks"][0]["tokens"] = json!([m.clone(), m]);
        let err = b
            .store_import(&payload)
            .expect_err("one measurement id, one row");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        assert!(
            err.message.contains(&mid),
            "the refusal must name the repeated id: {}",
            err.message
        );
        assert_eq!(measurements(&b), 1, "a refused import writes nothing");
    }

    /// D185: tags and dependency edges are sets, so a merge unions them — the
    /// destination keeps what it tagged and wired, and the payload's arrive
    /// beside it. The cycle and self-dependency guards still run on the union.
    #[test]
    fn store_import_merge_unions_tags_and_edges() {
        let a = Engine::open_in_memory().expect("open a");
        let t = a
            .task_add(&json!({ "title": "the dependent" }))
            .expect("add");
        let u = a.task_add(&json!({ "title": "the blocker" })).expect("add");
        let t_sid = t["short_id"].as_i64().expect("short_id");
        let u_sid = u["short_id"].as_i64().expect("short_id");
        let t_id = t["id"].as_str().expect("id").to_string();

        let seed = a.store_export(&json!({})).expect("export the seed");
        let b = Engine::open_in_memory().expect("open b");
        b.store_import(&seed).expect("seed b");

        a.tag_add(&json!({ "ref": t_sid, "tags": ["x"] }))
            .expect("x");
        a.dependency_add(&json!({ "ref": t_sid, "depends_on": u_sid }))
            .expect("edge");
        b.tag_add(&json!({ "ref": t_sid, "tags": ["y"] }))
            .expect("y");

        let mut payload = a.store_export(&json!({})).expect("export a");
        payload["merge"] = json!(true);
        b.store_import(&payload).expect("a merge must not refuse");

        let after = b.task_get(&json!({ "ref": t_id })).expect("get t");
        let mut tags: Vec<&str> = after["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        tags.sort_unstable();
        assert_eq!(tags, ["x", "y"], "both sides' tags must survive: {after}");
        assert_eq!(
            after["depends_on"],
            json!([u_sid]),
            "the payload's edge must be wired: {after}"
        );
    }

    /// D185 applies D3's per-field last-writer-wins at the import door: the
    /// scalars follow the later `modified`, the one stamp two independently
    /// minted stores mean the same thing by, and `merged` names which side won.
    #[test]
    fn store_import_merge_takes_scalars_from_the_later_modified_side() {
        let newer = Engine::open_in_memory().expect("open");
        let added = newer
            .task_add(&json!({ "title": "the stored title" }))
            .expect("add");
        let id = added["id"].as_str().expect("id").to_string();
        let sid = added["short_id"].as_i64().expect("short_id");

        let r = newer
            .store_import(&json!({
                "merge": true,
                "tasks": [{
                    "id": id,
                    "short_id": sid,
                    "title": "the payload title",
                    "modified": "2099-01-01T00:00:00Z",
                }],
            }))
            .expect("merge");
        assert_eq!(
            r["merged"],
            json!([{ "id": id, "took": "payload" }]),
            "a later payload wins the scalars, and says so: {r}"
        );
        let after = newer.task_get(&json!({ "ref": &id })).expect("get");
        assert_eq!(after["title"], json!("the payload title"), "{after}");

        // The same call with the stamps the other way round.
        let older = Engine::open_in_memory().expect("open");
        let added = older
            .task_add(&json!({ "title": "the stored title" }))
            .expect("add");
        let id = added["id"].as_str().expect("id").to_string();
        let sid = added["short_id"].as_i64().expect("short_id");
        let r = older
            .store_import(&json!({
                "merge": true,
                "tasks": [{
                    "id": id,
                    "short_id": sid,
                    "title": "the payload title",
                    "modified": "2000-01-01T00:00:00Z",
                }],
            }))
            .expect("merge");
        assert_eq!(
            r["merged"],
            json!([{ "id": id, "took": "store" }]),
            "an older payload leaves the scalars alone, and says so: {r}"
        );
        let after = older.task_get(&json!({ "ref": &id })).expect("get");
        assert_eq!(after["title"], json!("the stored title"), "{after}");
    }

    /// D185: the #177 `_rev` guard is skipped for a merged task. The two
    /// counters were minted on stores that never saw each other's writes, so a
    /// payload at a lower `_rev` is not a stale copy of this task — it is a
    /// different count of a different history, which is D183's reasoning for
    /// docs. The kept row takes `max(stored, payload) + 1`.
    #[test]
    fn store_import_merge_skips_the_rev_guard() {
        let e = Engine::open_in_memory().expect("open");
        let added = e
            .task_add(&json!({ "title": "the stored title" }))
            .expect("add");
        let id = added["id"].as_str().expect("id").to_string();
        let sid = added["short_id"].as_i64().expect("short_id");
        e.annotation_add(&json!({ "ref": sid, "body": "written here since" }))
            .expect("annotate");
        e.annotation_add(&json!({ "ref": sid, "body": "and again" }))
            .expect("annotate");
        let stored_rev = e.store_export(&json!({})).expect("export")["tasks"][0]["_rev"]
            .as_i64()
            .expect("_rev");
        assert!(stored_rev > 1, "the store must be ahead: {stored_rev}");

        let payload = json!({
            "tasks": [{
                "id": id,
                "short_id": sid,
                "title": "from the other machine",
                "modified": "2099-01-01T00:00:00Z",
                "_rev": 1,
                "annotations": [{ "body": "written over there" }],
            }],
        });
        let err = e
            .store_import(&payload)
            .expect_err("without merge the guard still bites");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);

        let mut merged = payload.clone();
        merged["merge"] = json!(true);
        let r = e.store_import(&merged).expect("a merge must not refuse");
        assert_eq!(r["merged"], json!([{ "id": id, "took": "payload" }]), "{r}");

        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(
            after["tasks"][0]["_rev"],
            json!(stored_rev + 1),
            "max(stored, payload) + 1: {after}"
        );
        assert_eq!(
            after["tasks"][0]["annotations"]
                .as_array()
                .expect("annotations")
                .len(),
            3,
            "both sides' notes: {after}"
        );
    }

    /// D138's contract, pinned: without `merge` the payload stays authoritative
    /// about a known task's child rows — they are replaced wholesale — and a
    /// payload behind the stored `_rev` is still refused.
    #[test]
    fn store_import_without_merge_still_replaces_wholesale_and_guards_rev() {
        let e = Engine::open_in_memory().expect("open");
        let added = e.task_add(&json!({ "title": "the task" })).expect("add");
        let id = added["id"].as_str().expect("id").to_string();
        let sid = added["short_id"].as_i64().expect("short_id");
        e.annotation_add(&json!({ "ref": sid, "body": "written here" }))
            .expect("annotate");
        e.check_add(&json!({ "ref": sid, "body": "claimed here" }))
            .expect("check");
        e.tag_add(&json!({ "ref": sid, "tags": ["here"] }))
            .expect("tag");

        let stale = json!({ "tasks": [{
            "id": id, "short_id": sid, "title": "the task", "_rev": 1,
        }]});
        let err = e
            .store_import(&stale)
            .expect_err("a payload behind the stored _rev is still refused");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);

        let r = e
            .store_import(&json!({ "tasks": [{
                "id": id,
                "short_id": sid,
                "title": "the task",
                "_rev": 99,
                "annotations": [{ "body": "written over there" }],
                "checks": [{ "body": "claimed over there" }],
            }]}))
            .expect("import");
        assert_eq!(
            r["merged"],
            json!([]),
            "`merged` is always present, empty without the flag: {r}"
        );

        let after = e.store_export(&json!({})).expect("export");
        let t = &after["tasks"][0];
        assert_eq!(
            t["annotations"].as_array().expect("annotations").len(),
            1,
            "the payload replaces the notes wholesale: {t}"
        );
        assert_eq!(
            t["annotations"][0]["body"],
            json!("written over there"),
            "{t}"
        );
        assert_eq!(
            t["checks"].as_array().expect("checks").len(),
            1,
            "and the checks: {t}"
        );
        assert_eq!(t["checks"][0]["body"], json!("claimed over there"), "{t}");
        assert_eq!(t["tags"], json!([]), "and the tags: {t}");
    }

    /// D184 composes with D185: a merge can be rehearsed. The report names
    /// the side that would have won and the store is left byte-identical.
    #[test]
    fn store_import_merge_with_dry_run_writes_nothing() {
        let e = Engine::open_in_memory().expect("open");
        let added = e
            .task_add(&json!({ "title": "the stored title" }))
            .expect("add");
        let id = added["id"].as_str().expect("id").to_string();
        let sid = added["short_id"].as_i64().expect("short_id");
        let before = e.store_export(&json!({})).expect("export");

        let r = e
            .store_import(&json!({
                "merge": true,
                "dry_run": true,
                "tasks": [{
                    "id": id,
                    "short_id": sid,
                    "title": "the payload title",
                    "modified": "2099-01-01T00:00:00Z",
                    "annotations": [{ "body": "written over there" }],
                }],
            }))
            .expect("a dry merge");
        assert_eq!(r["dry_run"], json!(true), "{r}");
        assert_eq!(r["merged"], json!([{ "id": id, "took": "payload" }]), "{r}");

        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(after, before, "a dry run must write nothing");
    }

    /// #84: the doc-branch counterpart to the test above. Restoring an OLDER
    /// export over a store whose doc has since been edited used to silently
    /// roll the doc's title, body and `rev` back to the stale snapshot — the
    /// upsert wrote `rev=excluded.rev` with no rewind guard, reopening the
    /// stale-`expected_rev` clobber D143 closed for `memory.import`. This
    /// pins that a stale doc payload is refused by name instead, and that
    /// the refusal writes NOTHING — the doc survives exactly as it was.
    #[test]
    fn store_import_refuses_a_stale_doc_rev_that_would_roll_back_its_body() {
        let e = Engine::open_in_memory().expect("open");
        let added = e
            .memory_add(&json!({ "title": "note", "body": "original text" }))
            .expect("add");
        let id = added["id"].as_str().expect("id").to_string();

        // The "monday" backup: rev 0 (just added).
        let monday = e.store_export(&json!({})).expect("export");
        assert_eq!(monday["docs"][0]["_rev"], json!(0), "{monday}");

        // Two edits happen, each bumping `rev` past what the backup carries.
        e.memory_update(&json!({ "id": id, "body": "edit one" }))
            .expect("update");
        e.memory_update(&json!({ "id": id, "body": "edit two" }))
            .expect("update");
        let live = e.store_export(&json!({})).expect("export");
        assert_eq!(live["docs"][0]["_rev"], json!(2), "{live}");
        assert_eq!(live["docs"][0]["body"], json!("edit two"), "{live}");

        // Restoring the older backup must be refused, not silently applied.
        let err = e
            .store_import(&monday)
            .expect_err("an older doc _rev must not overwrite a newer one");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        for needle in ["_rev 0", "_rev 2"] {
            assert!(err.message.contains(needle), "{}: {}", needle, err.message);
        }

        // The refusal wrote NOTHING: the doc's body and rev are exactly as
        // they were.
        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(after, live, "a refused import must not touch the store");
    }

    /// #84: an import at the SAME rev (re-importing a store's own export,
    /// D12) or a HIGHER one (a merge-target export carrying more edits than
    /// this store has seen) must still apply — the guard above only refuses
    /// a rev that would go BACKWARDS.
    #[test]
    fn store_import_still_accepts_a_doc_at_the_same_or_a_higher_rev() {
        let e = Engine::open_in_memory().expect("open");
        let added = e
            .memory_add(&json!({ "title": "note", "body": "v1" }))
            .expect("add");
        let id = added["id"].as_str().expect("id").to_string();

        // Same rev: re-importing a store's own export is a no-op, D12's
        // round trip.
        let doc = e.store_export(&json!({})).expect("export");
        e.store_import(&doc)
            .expect("an import at the same rev must still apply");
        assert_eq!(
            e.store_export(&json!({})).expect("export"),
            doc,
            "export -> import -> export stays identity"
        );

        // Higher rev: a payload naming the SAME doc at a rev ahead of what
        // is stored (the merge-target case: it came from a branch of this
        // same store that has since moved on) must apply too, updating the
        // row in place.
        let ahead = json!({
            "tasks": [],
            "docs": [{ "id": id, "title": "note", "body": "v2", "_rev": 5 }],
        });
        e.store_import(&ahead)
            .expect("an import at a higher rev must still apply");
        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(after["docs"][0]["_rev"], json!(5), "{after}");
        assert_eq!(after["docs"][0]["body"], json!("v2"), "{after}");
    }

    /// #800(a): a source names one doc (D174) — a payload `docs` array
    /// naming it twice used to insert the first entry and merge the second
    /// onto it (D183), so the document imported order-dependently. Refused
    /// whole, the same rule `memory.import` already enforces for its own
    /// `docs` array.
    #[test]
    fn store_import_refuses_a_duplicate_source_within_one_payload() {
        let e = Engine::open_in_memory().expect("open");
        let before = e.store_export(&json!({})).expect("export")["docs"]
            .as_array()
            .expect("docs")
            .len();

        let err = e
            .store_import(&json!({
                "tasks": [],
                "docs": [
                    {
                        "id": "0193aaaa-0000-7000-8000-0000000000f1",
                        "source": "docs/a.md",
                        "title": "first",
                        "body": "v1",
                    },
                    {
                        "id": "0193aaaa-0000-7000-8000-0000000000f2",
                        "source": "docs/a.md",
                        "title": "second",
                        "body": "v2",
                    },
                ],
            }))
            .expect_err("one source names one doc, so a payload cannot state it twice");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        assert!(
            err.message.contains("docs/a.md"),
            "the refusal must name the duplicated source: {}",
            err.message
        );
        assert_eq!(
            e.store_export(&json!({})).expect("export")["docs"]
                .as_array()
                .expect("docs")
                .len(),
            before,
            "a refused import writes nothing at all"
        );
    }

    /// #800(b): the D183 lookup above matches a payload doc by `source`, not
    /// by its own `id` — so that `id` can already be a DIFFERENT live doc
    /// here, under some other source. Folding it onto `kept_id` would merge
    /// two files into one row, so this is a `conflict`, not a merge.
    #[test]
    fn store_import_refuses_a_payload_id_that_names_a_different_live_doc() {
        const X: &str = "0193aaaa-0000-7000-8000-0000000000f3";
        const Y: &str = "0193aaaa-0000-7000-8000-0000000000f4";
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [
                {
                    "id": X,
                    "source": "docs/a.md",
                    "title": "x",
                    "body": "x's text",
                },
                {
                    "id": Y,
                    "source": "docs/b.md",
                    "title": "y",
                    "body": "y's text",
                },
            ],
        }))
        .expect("seed the destination");
        let before = e.store_export(&json!({})).expect("export");

        let err = e
            .store_import(&json!({
                "tasks": [],
                "docs": [{
                    "id": X,
                    "source": "docs/b.md",
                    "title": "x, relabelled as y's file",
                    "body": "must not land anywhere",
                    "modified": "2026-09-03T00:00:00Z",
                }],
            }))
            .expect_err("X is itself a live doc here, not a second holder of Y's source");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        for named in [X, Y, "docs/b.md"] {
            assert!(
                err.message.contains(named),
                "the refusal must name both docs and the source ({named}): {}",
                err.message
            );
        }
        assert_eq!(
            e.store_export(&json!({})).expect("export"),
            before,
            "a refused import writes nothing at all"
        );
    }

    /// D183: two machines that each ran `tasqx memory import docs/` hold one
    /// file under one `source` (D179) and two ids, because each store minted
    /// its own. D174 already says the source IS the doc's identity, so the
    /// arriving row is the same doc, not a second holder to refuse — and the
    /// later `modified` carries the text, the way the later edit does
    /// everywhere else.
    #[test]
    fn store_import_merges_a_doc_onto_the_one_holding_its_source_when_the_payload_is_newer() {
        const OURS: &str = "0193aaaa-0000-7000-8000-0000000000a1";
        const THEIRS: &str = "0193aaaa-0000-7000-8000-0000000000b1";
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [{
                "id": OURS,
                "source": "docs/a.md",
                "title": "this store's spelling",
                "body": "the copy imported here",
                "modified": "2026-09-01T00:00:00Z",
                "_rev": 3,
            }],
        }))
        .expect("seed the destination");

        let r = e
            .store_import(&json!({
                "tasks": [],
                "docs": [{
                    "id": THEIRS,
                    "source": "docs/a.md",
                    "title": "the other machine's spelling",
                    "body": "the copy imported there, later",
                    "modified": "2026-09-02T00:00:00Z",
                    "_rev": 0,
                }],
            }))
            .expect("a doc whose source another id holds must merge, not conflict");

        assert_eq!(r["docs_imported"], json!(1), "{r}");
        assert_eq!(
            r["docs_merged"],
            json!([{
                "source": "docs/a.md",
                "kept_id": OURS,
                "dropped_id": THEIRS,
                "took": "payload",
            }]),
            "every merge is reported, with both ids and which copy won: {r}"
        );

        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(
            after["docs"].as_array().expect("docs").len(),
            1,
            "one source is one doc (D174): {after}"
        );
        assert_eq!(after["docs"][0]["id"], json!(OURS), "{after}");
        assert_eq!(
            after["docs"][0]["body"],
            json!("the copy imported there, later"),
            "the later `modified` carries the text: {after}"
        );
        assert_eq!(
            after["docs"][0]["title"],
            json!("the other machine's spelling"),
            "{after}"
        );
        assert_eq!(
            after["docs"][0]["_rev"],
            json!(4),
            "a text-changing write bumps the kept row's own counter (D143): {after}"
        );

        let err = e
            .memory_get(&json!({ "id": THEIRS }))
            .expect_err("the payload's id is dropped, not stored beside the one it merged onto");
        assert_eq!(err.code, ErrorCode::NotFound, "{}", err.message);
    }

    /// The other half of D183's tiebreak: a payload no newer than the row
    /// already here changes nothing — the merge is still reported, because the
    /// caller's id was dropped either way.
    #[test]
    fn store_import_keeps_the_stored_doc_when_the_payload_is_older_or_equal() {
        const OURS: &str = "0193aaaa-0000-7000-8000-0000000000a2";
        const THEIRS: &str = "0193aaaa-0000-7000-8000-0000000000b2";
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [{
                "id": OURS,
                "source": "docs/a.md",
                "title": "kept",
                "body": "the copy this store already has",
                "modified": "2026-09-02T00:00:00Z",
                "_rev": 3,
            }],
        }))
        .expect("seed the destination");
        let before = e.store_export(&json!({})).expect("export");

        let r = e
            .store_import(&json!({
                "tasks": [],
                "docs": [{
                    "id": THEIRS,
                    "source": "docs/a.md",
                    "title": "older",
                    "body": "the copy imported there, earlier",
                    "modified": "2026-09-01T00:00:00Z",
                    "_rev": 99,
                }],
            }))
            .expect("import");

        assert_eq!(
            r["docs_merged"],
            json!([{
                "source": "docs/a.md",
                "kept_id": OURS,
                "dropped_id": THEIRS,
                "took": "store",
            }]),
            "{r}"
        );
        assert_eq!(
            e.store_export(&json!({})).expect("export"),
            before,
            "an older payload must not touch the row, not even its rev — a high \
             `_rev` on an independently minted row is not a later edit"
        );
    }

    /// Idempotence, the rule every other section of this method follows: the
    /// same document imported twice leaves the store exactly as the first run
    /// left it. The second run still REPORTS the merge — the report is about
    /// the payload id that was dropped, not about bytes that changed.
    #[test]
    fn store_import_of_the_same_merged_document_twice_is_a_no_op() {
        const OURS: &str = "0193aaaa-0000-7000-8000-0000000000a3";
        const THEIRS: &str = "0193aaaa-0000-7000-8000-0000000000b3";
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [{
                "id": OURS,
                "source": "docs/a.md",
                "title": "ours",
                "body": "v1",
                "modified": "2026-09-01T00:00:00Z",
            }],
        }))
        .expect("seed the destination");
        let payload = json!({
            "tasks": [],
            "docs": [{
                "id": THEIRS,
                "source": "docs/a.md",
                "title": "theirs",
                "body": "v2",
                "modified": "2026-09-02T00:00:00Z",
            }],
        });

        e.store_import(&payload).expect("first import");
        let once = e.store_export(&json!({})).expect("export");

        let r = e.store_import(&payload).expect("second import");
        assert_eq!(
            r["docs_merged"][0]["took"],
            json!("store"),
            "the second run finds its own text already here, at the same instant: {r}"
        );
        assert_eq!(
            e.store_export(&json!({})).expect("export"),
            once,
            "importing the same document twice must change nothing the second time"
        );
    }

    /// A payload doc the store ALREADY holds under the SAME id is untouched by
    /// D183 — it is not a second holder of anything, so #84's rewind guard is
    /// still the rule that decides it.
    #[test]
    fn store_import_same_id_doc_still_goes_through_the_rev_guard() {
        const ID: &str = "0193aaaa-0000-7000-8000-0000000000a4";
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [{
                "id": ID,
                "source": "docs/a.md",
                "title": "note",
                "body": "v2",
                "modified": "2026-09-01T00:00:00Z",
                "_rev": 2,
            }],
        }))
        .expect("seed");

        let err = e
            .store_import(&json!({
                "tasks": [],
                "docs": [{
                    "id": ID,
                    "source": "docs/a.md",
                    "title": "note",
                    "body": "v1",
                    "modified": "2026-09-09T00:00:00Z",
                    "_rev": 1,
                }],
            }))
            .expect_err("a stale _rev on the doc's OWN id is still a conflict");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert_eq!(
            e.store_export(&json!({})).expect("export")["docs"][0]["body"],
            json!("v2"),
            "the refusal wrote nothing"
        );
    }

    /// D181 built the remap seam for exactly this: a link naming the payload's
    /// doc id has to land on the row this store kept, or the restored graph
    /// points at a doc nobody here holds. Its events follow the same table.
    #[test]
    fn a_link_to_a_merged_doc_follows_the_kept_id() {
        const OURS: &str = "0193aaaa-0000-7000-8000-0000000000a5";
        const THEIRS: &str = "0193aaaa-0000-7000-8000-0000000000b5";
        const TASK: &str = "0193aaaa-0000-7000-8000-0000000000c5";
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000d5";
        const EVENT: &str = "0193aaaa-0000-7000-8000-0000000000e5";
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [{
                "id": OURS,
                "source": "docs/a.md",
                "title": "ours",
                "body": "v1",
                "modified": "2026-09-01T00:00:00Z",
            }],
        }))
        .expect("seed the destination");

        e.store_import(&json!({
            "tasks": [{ "id": TASK, "short_id": 40, "title": "cites the doc" }],
            "docs": [{
                "id": THEIRS,
                "source": "docs/a.md",
                "title": "theirs",
                "body": "v2",
                "modified": "2026-09-02T00:00:00Z",
            }],
            "links": [{
                "id": LINK,
                "from": format!("task:{TASK}"),
                "to": format!("memory:{THEIRS}"),
                "relation": "references",
            }],
            "events": [{
                "id": EVENT,
                "entity": "doc",
                "entity_id": THEIRS,
                "op": "memory.update",
                "ts": "2026-09-02T00:00:00Z",
            }],
        }))
        .expect("import");

        let links = e.link_list(&json!({})).expect("link.list");
        assert_eq!(
            links["links"][0]["to"],
            json!(format!("memory:{OURS}")),
            "the end must follow the merge onto the row this store kept: {links}"
        );
        let events = e
            .event_list(&json!({ "entity": "doc" }))
            .expect("event.list");
        assert!(
            events["events"]
                .as_array()
                .expect("events")
                .iter()
                .any(|ev| ev["id"] == json!(EVENT) && ev["entity_id"] == json!(OURS)),
            "the merged doc's history must land under the id that kept the source: {events}"
        );
    }

    /// #800(c): a staged link EVENT's `entity_id` follows `link_remap` (pass
    /// 2c, the twin-link table), but its own `payload` also spells the edge's
    /// ends as free-standing `from`/`to` strings, which that remap never
    /// touches. A doc end D183's docs pass merged away must be rewritten
    /// there too, or the replayed history still names an id nobody here
    /// holds.
    #[test]
    fn a_staged_link_event_names_the_kept_doc_in_its_payload() {
        const KEPT: &str = "0193aaaa-0000-7000-8000-0000000000f5";
        const DROPPED: &str = "0193aaaa-0000-7000-8000-0000000000f6";
        const TASK: &str = "0193aaaa-0000-7000-8000-0000000000f7";
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000f8";
        const EVENT: &str = "0193aaaa-0000-7000-8000-0000000000f9";
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [{
                "id": KEPT,
                "source": "docs/a.md",
                "title": "kept",
                "body": "v1",
                "modified": "2026-09-01T00:00:00Z",
            }],
        }))
        .expect("seed the destination");

        let r = e
            .store_import(&json!({
                "tasks": [{ "id": TASK, "short_id": 41, "title": "cites the doc" }],
                "docs": [{
                    "id": DROPPED,
                    "source": "docs/a.md",
                    "title": "dropped",
                    "body": "v2",
                    "modified": "2026-09-02T00:00:00Z",
                }],
                "links": [{
                    "id": LINK,
                    "from": format!("task:{TASK}"),
                    "to": format!("memory:{DROPPED}"),
                    "relation": "references",
                }],
                "events": [{
                    "id": EVENT,
                    "entity": "link",
                    "entity_id": LINK,
                    "op": "link.add",
                    "payload": {
                        "from": format!("task:{TASK}"),
                        "to": format!("memory:{DROPPED}"),
                        "relation": "references",
                    },
                    "ts": "2026-09-02T00:00:00Z",
                }],
            }))
            .expect("import");
        assert_eq!(r["docs_merged"][0]["kept_id"], json!(KEPT), "{r}");

        let events = e
            .event_list(&json!({ "entity": "link" }))
            .expect("event.list");
        let events = events["events"].as_array().expect("events array");
        let stored = events
            .iter()
            .find(|ev| ev["id"] == json!(EVENT))
            .expect("the staged event lands under its own id");
        assert_eq!(
            stored["payload"]["to"],
            json!(format!("memory:{KEPT}")),
            "the payload must name the doc that kept the source, not the one the merge \
             dropped: {stored}"
        );
        assert_eq!(
            stored["payload"]["from"],
            json!(format!("task:{TASK}")),
            "a non-memory end is left exactly as given: {stored}"
        );
    }

    /// #88: a lowercase-`z` stamp must sort by instant, not by bytes.
    #[test]
    fn store_import_normalises_a_foreign_doc_stamp_so_memory_list_orders_by_instant() {
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [
                {
                    "id": "0193aaaa-0000-7000-8000-00000000000a",
                    "title": "earlier, lowercase z",
                    "body": "a",
                    "modified": "2026-01-01T00:00:10z",
                },
                {
                    "id": "0193aaaa-0000-7000-8000-00000000000b",
                    "title": "later, canonical",
                    "body": "b",
                    "modified": "2026-01-01T00:00:10.1Z",
                },
            ],
        }))
        .expect("import");

        let listed = e.memory_list(&json!({})).expect("list");
        let ids: Vec<&str> = listed["docs"]
            .as_array()
            .expect("docs")
            .iter()
            .map(|d| d["id"].as_str().expect("id"))
            .collect();
        assert_eq!(
            ids,
            vec![
                "0193aaaa-0000-7000-8000-00000000000b",
                "0193aaaa-0000-7000-8000-00000000000a",
            ],
            "the doc modified later must list first, regardless of the imported stamp's case: \
             {listed}"
        );
    }

    /// #88: an offset stamp (`+00:00` rather than `Z`) is unambiguous —
    /// `parse_when`'s RFC3339 branch reads it and re-serializes to the
    /// canonical form, the same as a task's `created`/`modified` already do.
    #[test]
    fn store_import_normalises_a_doc_stamp_carrying_an_explicit_offset() {
        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [],
            "docs": [{
                "id": "0193aaaa-0000-7000-8000-00000000000c",
                "title": "offset stamp",
                "body": "c",
                "created": "2026-01-01T02:00:10+02:00",
                "modified": "2026-01-01T02:00:10+02:00",
            }],
        }))
        .expect("import");

        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(
            after["docs"][0]["created"],
            json!("2026-01-01T00:00:10Z"),
            "an offset stamp must normalise to the UTC instant it names: {after}"
        );
        assert_eq!(after["docs"][0]["modified"], json!("2026-01-01T00:00:10Z"));
    }

    /// #88: unparsable is refused the same way a task's `created`/`modified`
    /// already are (`import_field`) — not silently stored and not silently
    /// replaced with `now`.
    #[test]
    fn store_import_refuses_a_doc_with_an_unparsable_modified_stamp() {
        let e = Engine::open_in_memory().expect("open");
        let err = e
            .store_import(&json!({
                "tasks": [],
                "docs": [{ "title": "bad stamp", "body": "d", "modified": "not-a-date" }],
            }))
            .expect_err("an unparsable modified stamp must be refused");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        assert!(
            err.message.contains("modified"),
            "must name the field: {}",
            err.message
        );
        let after = e.store_export(&json!({})).expect("export");
        assert_eq!(
            after["docs"].as_array().map(Vec::len),
            Some(0),
            "a refused import must write nothing: {after}"
        );
    }

    /// #176: `store.export` carried no event log at all, so a store restored
    /// from a backup answered `chart heatmap`/`chart throughput` (both of
    /// which read `event.list`, see `tasqx-cli/src/chart.rs`) as if no task
    /// had ever been completed — while `task.list status:done` still counted
    /// every one of them, because task rows round-tripped fine. This pins
    /// that the `done` event survives an export/import cycle into a FRESH
    /// store, which is the whole of what a restore promises.
    #[test]
    fn store_export_carries_the_event_log_so_a_restore_keeps_its_done_history() {
        let e = Engine::open_in_memory().expect("open");
        let added = e.task_add(&json!({ "title": "ship it" })).expect("add");
        let sid = added["short_id"].as_i64().expect("short_id");
        e.task_start(&json!({ "ref": sid })).expect("start");
        e.task_stop(&json!({ "ref": sid })).expect("stop");
        e.task_done(&json!({ "ref": sid })).expect("done");

        let doc = e.store_export(&json!({})).expect("export");
        let events = doc["events"].as_array().expect("events array");
        let ops: Vec<&str> = events
            .iter()
            .map(|ev| ev["op"].as_str().expect("op is a string"))
            .collect();
        assert!(
            ops.contains(&"done"),
            "the export must carry the `done` event: {ops:?}"
        );

        let fresh = Engine::open_in_memory().expect("open fresh");
        let imported = fresh.store_import(&doc).expect("import");
        assert!(imported["events_imported"].as_i64().unwrap() >= events.len() as i64);

        let restored_ops: Vec<Value> = fresh
            .event_list(&json!({ "entity": "task", "limit": 100 }))
            .expect("event.list")["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|ev| ev["op"].clone())
            .collect();
        assert!(
            restored_ops.contains(&json!("done")),
            "a restored store must still be able to answer `chart heatmap`'s question: {restored_ops:?}"
        );
    }

    /// #179: a document with no `docs` section at all (a pre-D41 export, or a
    /// typo that moved the array under the wrong key) used to import
    /// indistinguishably from one that declared an EMPTY `docs` section — both
    /// answered `docs_imported: 0`, silently. `docs_declared` is the signal a
    /// human surface (verbs.rs::run_import) needs to tell the two apart.
    #[test]
    fn store_import_reports_whether_the_document_declared_a_docs_section() {
        let no_section = Engine::open_in_memory().expect("open");
        let r = no_section
            .store_import(&json!({ "tasks": [] }))
            .expect("import with no docs key");
        assert_eq!(r["docs_imported"], json!(0), "{r}");
        assert_eq!(
            r["docs_declared"],
            json!(false),
            "no `docs` key at all must be reported as undeclared: {r}"
        );
        // D181: a document written before the `links` section existed carries
        // no such key either, and imports exactly as it did — the counter is
        // present and zero, never absent.
        assert_eq!(
            r["links_imported"],
            json!(0),
            "a legacy document restores no links, and says so: {r}"
        );

        let empty_section = Engine::open_in_memory().expect("open");
        let r = empty_section
            .store_import(&json!({ "tasks": [], "docs": [] }))
            .expect("import with an empty docs array");
        assert_eq!(r["docs_imported"], json!(0), "{r}");
        assert_eq!(
            r["docs_declared"],
            json!(true),
            "an explicit empty `docs` array must be reported as declared: {r}"
        );
    }

    /// D184: `dry_run` runs the whole import — renumbering (D177), a doc merge
    /// (D183), a link (D181) — and rolls the transaction back. The answer
    /// must name every one of those outcomes exactly as a real import would,
    /// while the store itself, its event log and the payload's own ids are
    /// left exactly as they were.
    #[test]
    fn store_import_dry_run_reports_everything_and_writes_nothing() {
        const DEST_TASK: &str = "0193aaaa-0000-7000-8000-0000000000d1";
        const DEST_DOC: &str = "0193aaaa-0000-7000-8000-0000000000d2";
        const PAYLOAD_TASK: &str = "0193aaaa-0000-7000-8000-0000000000d3";
        const PAYLOAD_DOC: &str = "0193aaaa-0000-7000-8000-0000000000d4";
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000d5";

        let e = Engine::open_in_memory().expect("open");
        e.store_import(&json!({
            "tasks": [{ "id": DEST_TASK, "short_id": 1, "title": "destination task" }],
            "docs": [{
                "id": DEST_DOC,
                "source": "docs/a.md",
                "title": "this store's spelling",
                "body": "the copy already here",
                "modified": "2026-09-01T00:00:00Z",
                "_rev": 1,
            }],
        }))
        .expect("seed the destination");

        let payload = json!({
            // Collides with DEST_TASK's short_id, so it must renumber.
            "tasks": [{ "id": PAYLOAD_TASK, "short_id": 1, "title": "payload task" }],
            // Collides with DEST_DOC's source, newer `modified`, so it must merge.
            "docs": [{
                "id": PAYLOAD_DOC,
                "source": "docs/a.md",
                "title": "the other machine's spelling",
                "body": "the newer copy",
                "modified": "2026-09-02T00:00:00Z",
                "_rev": 0,
            }],
            "links": [{
                "id": LINK,
                "from": format!("task:{PAYLOAD_TASK}"),
                "to": format!("memory:{PAYLOAD_DOC}"),
                "relation": "references",
            }],
        });

        let before = e.store_export(&json!({})).expect("export before dry run");
        let events_before =
            e.event_list(&json!({ "limit": 1000 })).expect("event.list")["count"].clone();

        let mut dry = payload.clone();
        dry["dry_run"] = json!(true);
        let r = e.store_import(&dry).expect("dry run");

        assert_eq!(r["dry_run"], json!(true), "{r}");
        assert_eq!(
            r["renumbered"].as_array().expect("renumbered array").len(),
            1,
            "{r}"
        );
        assert_eq!(
            r["docs_merged"]
                .as_array()
                .expect("docs_merged array")
                .len(),
            1,
            "{r}"
        );
        assert_eq!(r["links_imported"], json!(1), "{r}");

        let after = e.store_export(&json!({})).expect("export after dry run");
        assert_eq!(
            after, before,
            "a dry run must leave the store byte-identical"
        );
        let events_after =
            e.event_list(&json!({ "limit": 1000 })).expect("event.list")["count"].clone();
        assert_eq!(
            events_after, events_before,
            "no event survives a rolled-back transaction"
        );
        assert_eq!(
            e.task_get(&json!({ "ref": PAYLOAD_TASK }))
                .unwrap_err()
                .code,
            ErrorCode::NotFound,
            "the payload's task was never kept"
        );

        // The same document, for real, must report the same outcomes.
        let real = e.store_import(&payload).expect("real run");
        assert_eq!(real["renumbered"], r["renumbered"], "{real}");
        assert_eq!(real["docs_merged"], r["docs_merged"], "{real}");
        assert_eq!(real["links_imported"], r["links_imported"], "{real}");
        assert_eq!(real["dry_run"], json!(false), "{real}");
    }

    /// D184: a refusal surfaces on a dry run exactly as it does on a real one
    /// — the transaction never reaches its rollback-or-commit line at all.
    #[test]
    fn store_import_dry_run_still_refuses_what_the_real_run_refuses() {
        let e = Engine::open_in_memory().expect("open");
        const FIRST: &str = "0193aaaa-0000-7000-8000-0000000000e1";
        const SECOND: &str = "0193aaaa-0000-7000-8000-0000000000e2";
        let err = e
            .store_import(&json!({
                "dry_run": true,
                "tasks": [
                    { "id": FIRST, "short_id": 9, "title": "first" },
                    { "id": SECOND, "short_id": 9, "title": "second" },
                ],
            }))
            .expect_err("one short_id cannot address two tasks, dry run or not");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert_eq!(
            exported_task_count(&e),
            0,
            "a refused document writes nothing"
        );
    }

    /// `dry_run` guards a real write, so a value that is not a boolean is
    /// refused rather than coerced — the same rule `opt_bool` enforces
    /// everywhere else.
    #[test]
    fn store_import_refuses_a_non_boolean_dry_run() {
        let e = Engine::open_in_memory().expect("open");
        let err = e
            .store_import(&json!({ "tasks": [], "dry_run": "true" }))
            .expect_err("a string is not a boolean");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        assert!(err.message.contains("dry_run"), "{}", err.message);
    }

    /// D181: the `links` table used to be left out of the archive entirely, so
    /// a full backup restored with no graph edges at all — beside an event log
    /// that described every edge that was supposed to be there. The document
    /// carries the rows now, and a restore reproduces them one for one.
    #[test]
    fn store_export_then_import_reproduces_every_link_row() {
        let a = Engine::open_in_memory().expect("open");
        let one = a.task_add(&json!({ "title": "one" })).expect("add");
        let two = a.task_add(&json!({ "title": "two" })).expect("add");
        let doc = a
            .memory_add(&json!({ "title": "the ruling", "body": "why it is so" }))
            .expect("doc");
        let doc_ref = format!("memory:{}", doc["id"].as_str().expect("doc id"));
        a.link_add(&json!({
            "from": one["short_id"].clone(),
            "to": two["short_id"].clone(),
            "relation": "references",
        }))
        .expect("task -> task");
        a.link_add(&json!({
            "from": two["short_id"].clone(),
            "to": doc_ref,
            "relation": "derived_from",
            "metadata": { "why": "the note" },
        }))
        .expect("task -> doc");

        let document = a.store_export(&json!({})).expect("export");
        assert_eq!(
            document["links"].as_array().expect("links array").len(),
            2,
            "an unfiltered export carries every row: {document}"
        );
        assert_eq!(document["dropped_links"], json!(0), "{document}");

        let b = Engine::open_in_memory().expect("open");
        let r = b.store_import(&document).expect("import");
        assert_eq!(r["links_imported"], json!(2), "{r}");

        let links = |e: &Engine| e.link_list(&json!({})).expect("link.list")["links"].clone();
        assert_eq!(
            links(&b),
            links(&a),
            "every link row comes back whole — id, both ends, relation and metadata"
        );
        // And the document the restored store writes is the one it was handed:
        // D12's round trip now covers the graph's edges too.
        assert_eq!(
            b.store_export(&json!({})).expect("re-export")["links"],
            document["links"],
            "export -> import -> export is identity for links"
        );
    }

    /// A filtered export carries a link only when BOTH its ends are nodes the
    /// document itself carries — the same trim `depends_on` has had since D12,
    /// for the same reason: an edge naming a node this document does not hold
    /// is a dangling pointer the import would refuse outright.
    #[test]
    fn filtered_export_keeps_only_links_with_both_ends_and_reports_dropped_links() {
        let e = Engine::open_in_memory().expect("open");
        e.project_create(&json!({ "name": "kept" })).expect("kept");
        e.project_create(&json!({ "name": "other" }))
            .expect("other");
        let inside = e
            .task_add(&json!({ "title": "inside", "project": "kept" }))
            .expect("add");
        let also = e
            .task_add(&json!({ "title": "also inside", "project": "kept" }))
            .expect("add");
        let outside = e
            .task_add(&json!({ "title": "outside", "project": "other" }))
            .expect("add");
        let within = e
            .link_add(&json!({
                "from": inside["short_id"].clone(),
                "to": also["short_id"].clone(),
                "relation": "references",
            }))
            .expect("both ends inside");
        e.link_add(&json!({
            "from": inside["short_id"].clone(),
            "to": outside["short_id"].clone(),
            "relation": "references",
        }))
        .expect("one end outside");

        let document = e
            .store_export(&json!({ "filter": "project:kept" }))
            .expect("export");
        let links = document["links"].as_array().expect("links array");
        assert_eq!(links.len(), 1, "only the edge inside the slice: {document}");
        assert_eq!(links[0]["id"], within["id"], "{document}");
        assert_eq!(
            document["dropped_links"],
            json!(1),
            "losing an edge silently is what this counter exists to prevent: {document}"
        );

        // The count reports the FILTER, not the store: unfiltered, both rows
        // travel and nothing is dropped.
        let whole = e.store_export(&json!({})).expect("export");
        assert_eq!(whole["links"].as_array().expect("links").len(), 2);
        assert_eq!(whole["dropped_links"], json!(0), "{whole}");
    }

    /// The import's own half of the same rule: a link naming an end that is
    /// neither in the payload nor already in the store is refused by name,
    /// exactly as a dangling `depends_on` is — and the whole import writes
    /// nothing, because it is one transaction.
    #[test]
    fn store_import_refuses_a_link_whose_end_is_absent() {
        let e = Engine::open_in_memory().expect("open");
        let t = e.task_add(&json!({ "title": "here" })).expect("add");
        let mut document = e.store_export(&json!({})).expect("export");
        const GONE: &str = "0193aaaa-0000-7000-8000-0000000000ee";
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000dd";
        document["links"] = json!([{
            "id": LINK,
            "from": format!("task:{}", t["id"].as_str().expect("id")),
            "to": format!("task:{GONE}"),
            "relation": "references",
            "metadata": null,
            "created_at": "2026-09-16T09:00:00Z",
        }]);

        let b = Engine::open_in_memory().expect("open");
        let err = b
            .store_import(&document)
            .expect_err("an end nobody holds is a dangling pointer");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        for named in [LINK, GONE, "task"] {
            assert!(
                err.message.contains(named),
                "the refusal must name the link and the missing end ({named}): {}",
                err.message
            );
        }
        assert_eq!(
            exported_task_count(&b),
            0,
            "a refused import writes nothing at all"
        );
    }

    /// `annotation.remove` keeps the row and sets `removed` (D113). A payload
    /// link naming that id is the same dangling end as one naming nothing at
    /// all (task #799): the destination holds a corpse, not a node, and a
    /// `node_exists` that disagreed would let the edge in only for D181's own
    /// export to drop it on the very next round trip.
    ///
    /// The owning task is deliberately NOT in the payload's `tasks` array:
    /// `import_annotations` replaces a task's annotations wholesale on every
    /// import that touches it, which would wipe the tombstone as a side
    /// effect and test the wrong thing — the dangling-end refusal this test
    /// is after, not "the row is gone entirely" the GONE test above already
    /// covers.
    #[test]
    fn store_import_refuses_a_link_to_an_annotation_the_destination_holds_as_a_tombstone() {
        let e = Engine::open_in_memory().expect("open");
        let owner = e
            .task_add(&json!({ "title": "owns the annotation" }))
            .expect("add");
        let note = e
            .annotation_add(&json!({ "ref": owner["short_id"].clone(), "body": "a note" }))
            .expect("annotate");
        let note = note["annotation"]["id"]
            .as_str()
            .expect("annotation id")
            .to_string();
        e.annotation_remove(&json!({ "ref": owner["short_id"].clone(), "annotation_id": note }))
            .expect("remove");

        const LINKER: &str = "0193aaaa-0000-7000-8000-0000000000aa";
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000ab";
        let document = json!({ "tasks": [{
            "id": LINKER,
            "short_id": 99,
            "title": "from another store",
            "created": "2026-09-16T09:00:00Z",
            "modified": "2026-09-16T09:00:00Z",
            "_rev": 1,
        }], "links": [{
            "id": LINK,
            "from": format!("task:{LINKER}"),
            "to": format!("annotation:{note}"),
            "relation": "references",
            "metadata": null,
            "created_at": "2026-09-16T09:00:00Z",
        }] });

        let before = exported_task_count(&e);
        let err = e
            .store_import(&document)
            .expect_err("a tombstoned annotation is not a node a link can name");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        for named in [LINK, note.as_str(), "annotation"] {
            assert!(
                err.message.contains(named),
                "the refusal must name the link and the tombstoned end ({named}): {}",
                err.message
            );
        }
        assert_eq!(
            exported_task_count(&e),
            before,
            "a refused import writes nothing at all"
        );
        assert_eq!(
            e.link_list(&json!({})).expect("link.list")["links"]
                .as_array()
                .expect("links array")
                .len(),
            0,
            "no link row for a refused import"
        );
    }

    /// Both ends are resolved through the remap table the task, doc and project
    /// passes build, never through the payload's own id: `upsert_project` keeps
    /// the DESTINATION's row when a project of that name is already here, so a
    /// `project:<payload id>` end has to land on the id this store holds.
    #[test]
    fn store_import_resolves_link_ends_through_the_remap_table() {
        let a = Engine::open_in_memory().expect("open");
        let theirs = a
            .project_create(&json!({ "name": "shared" }))
            .expect("project")["id"]
            .as_str()
            .expect("id")
            .to_string();
        let t = a
            .task_add(&json!({ "title": "cites the project", "project": "shared" }))
            .expect("add");
        a.link_add(&json!({
            "from": t["short_id"].clone(),
            "to": format!("project:{theirs}"),
            "relation": "references",
        }))
        .expect("task -> project");
        let document = a.store_export(&json!({})).expect("export");

        let b = Engine::open_in_memory().expect("open");
        let ours = b
            .project_create(&json!({ "name": "shared" }))
            .expect("the same name, this store's own row")["id"]
            .as_str()
            .expect("id")
            .to_string();
        assert_ne!(theirs, ours, "precondition: two stores mint two ids");

        let r = b.store_import(&document).expect("import");
        assert_eq!(r["links_imported"], json!(1), "{r}");
        let links = b.link_list(&json!({})).expect("link.list");
        assert_eq!(
            links["links"][0]["to"],
            json!(format!("project:{ours}")),
            "the end must follow the remap onto the row this store kept: {links}"
        );
    }
    /// `link.add` refuses an edge from a node to itself, and import is not a
    /// back door around it: without this guard a payload could mint through
    /// `store.import` the very row the API declines to write, and re-export it
    /// to every downstream store.
    #[test]
    fn store_import_refuses_a_self_link_and_writes_nothing() {
        let a = Engine::open_in_memory().expect("open");
        a.task_add(&json!({ "title": "here" })).expect("add");
        let mut document = a.store_export(&json!({})).expect("export");
        let id = document["tasks"][0]["id"].as_str().expect("id").to_string();
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000aa";
        document["links"] = json!([{
            "id": LINK,
            "from": format!("task:{id}"),
            "to": format!("task:{id}"),
            "relation": "references",
            "metadata": null,
            "created_at": "2026-09-16T09:00:00Z",
        }]);

        let b = Engine::open_in_memory().expect("open");
        let err = b
            .store_import(&document)
            .expect_err("a node cannot link to itself");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        assert!(
            err.message.contains(LINK) && err.message.contains("itself"),
            "the refusal must name the link and the rule: {}",
            err.message
        );
        assert_eq!(
            exported_task_count(&b),
            0,
            "a refused import writes nothing at all"
        );
    }

    /// #180's rule for `tasks`, applied to `links`: one id addresses exactly
    /// one link, so a document stating two under the same id is refused rather
    /// than applied twice by the upsert — which would keep only the LAST while
    /// `links_imported` counted both.
    #[test]
    fn store_import_refuses_a_link_id_repeated_in_one_document() {
        let a = Engine::open_in_memory().expect("open");
        let one = a.task_add(&json!({ "title": "one" })).expect("add");
        let two = a.task_add(&json!({ "title": "two" })).expect("add");
        let mut document = a.store_export(&json!({})).expect("export");
        let one = format!("task:{}", one["id"].as_str().expect("id"));
        let two = format!("task:{}", two["id"].as_str().expect("id"));
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000bb";
        document["links"] = json!([
            { "id": LINK, "from": one, "to": two, "relation": "references" },
            { "id": LINK, "from": two, "to": one, "relation": "references" },
        ]);

        let b = Engine::open_in_memory().expect("open");
        let err = b
            .store_import(&document)
            .expect_err("one id addresses exactly one link");
        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        assert!(
            err.message.contains(LINK) && err.message.contains("same payload"),
            "the refusal must name the link and say it is the same payload: {}",
            err.message
        );
        assert_eq!(
            exported_task_count(&b),
            0,
            "a refused import writes nothing at all"
        );
    }

    /// `created_at` passes the same date gate `created`/`modified` do on a task
    /// and a doc: a stamp no reader can parse used to be written to the column
    /// verbatim, so every reader downstream read the column back as garbage.
    #[test]
    fn store_import_refuses_a_link_with_an_unparseable_created_at() {
        let a = Engine::open_in_memory().expect("open");
        let one = a.task_add(&json!({ "title": "one" })).expect("add");
        let two = a.task_add(&json!({ "title": "two" })).expect("add");
        let mut document = a.store_export(&json!({})).expect("export");
        const LINK: &str = "0193aaaa-0000-7000-8000-0000000000cc";
        document["links"] = json!([{
            "id": LINK,
            "from": format!("task:{}", one["id"].as_str().expect("id")),
            "to": format!("task:{}", two["id"].as_str().expect("id")),
            "relation": "references",
            "created_at": "whenever",
        }]);

        let b = Engine::open_in_memory().expect("open");
        let err = b
            .store_import(&document)
            .expect_err("a date no reader can parse");
        assert_eq!(err.code, ErrorCode::BadRequest, "{}", err.message);
        for named in [LINK, "created_at"] {
            assert!(
                err.message.contains(named),
                "the refusal must name the link and the field ({named}): {}",
                err.message
            );
        }
    }

    /// The twin branch keeps the STORE's row when the payload states an edge
    /// this store already holds under another id — so the payload's link
    /// EVENTS, which name the payload's id, used to be filed under a link row
    /// that does not exist here, leaving `event.list` on the kept row silent
    /// about everything the import brought.
    #[test]
    fn imported_link_events_follow_the_row_a_twin_import_kept() {
        let a = Engine::open_in_memory().expect("open");
        let one = a.task_add(&json!({ "title": "one" })).expect("add");
        let two = a.task_add(&json!({ "title": "two" })).expect("add");

        // The destination first, so both stores hold the SAME two task ids and
        // the edge below is the same edge stated twice.
        let b = Engine::open_in_memory().expect("open");
        b.store_import(&a.store_export(&json!({})).expect("export"))
            .expect("tasks only");
        let ours = b
            .link_add(&json!({
                "from": one["short_id"].clone(),
                "to": two["short_id"].clone(),
                "relation": "references",
            }))
            .expect("this store's own row")["id"]
            .as_str()
            .expect("id")
            .to_string();
        let theirs = a
            .link_add(&json!({
                "from": one["short_id"].clone(),
                "to": two["short_id"].clone(),
                "relation": "references",
            }))
            .expect("the same edge, the other store's id")["id"]
            .as_str()
            .expect("id")
            .to_string();
        assert_ne!(theirs, ours, "precondition: two stores mint two ids");

        let r = b
            .store_import(&a.store_export(&json!({})).expect("export"))
            .expect("import");
        assert_eq!(
            r["links_imported"],
            json!(1),
            "the same edge, not a second one: {r}"
        );

        let events = b
            .event_list(&json!({ "entity": "link" }))
            .expect("event.list");
        let events = events["events"].as_array().expect("events array");
        assert!(
            !events
                .iter()
                .any(|e| e["entity_id"].as_str() == Some(theirs.as_str())),
            "no event may name a link row this store does not hold: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| e["entity_id"].as_str() == Some(ours.as_str()))
                .count(),
            2,
            "this store's own `link.add` plus the payload's, both on the kept row: {events:?}"
        );
    }

    /// `annotation.remove` keeps the row and sets `removed` (D113), and an
    /// export carries only the LIVE annotations — so an unfiltered export used
    /// to ship a link to a tombstone it did not carry, and the import refused
    /// that dangling end and rolled the whole restore back. The carried-endpoint
    /// filter applies to every export now, counted like any other trim.
    #[test]
    fn an_unfiltered_export_drops_a_link_to_a_removed_annotation() {
        let e = Engine::open_in_memory().expect("open");
        let t = e.task_add(&json!({ "title": "the task" })).expect("add");
        let note = e
            .annotation_add(&json!({ "ref": t["short_id"].clone(), "body": "a note" }))
            .expect("annotate");
        let note = note["annotation"]["id"]
            .as_str()
            .expect("annotation id")
            .to_string();
        e.link_add(&json!({
            "from": t["short_id"].clone(),
            "to": format!("annotation:{note}"),
            "relation": "references",
        }))
        .expect("task -> annotation");
        e.annotation_remove(&json!({ "ref": t["short_id"].clone(), "annotation_id": note }))
            .expect("remove");

        let document = e.store_export(&json!({})).expect("export");
        assert_eq!(
            document["links"].as_array().expect("links array").len(),
            0,
            "a link to a tombstone the document does not carry must not travel: {document}"
        );
        assert_eq!(
            document["dropped_links"],
            json!(1),
            "losing an edge silently is what this counter exists to prevent: {document}"
        );

        let b = Engine::open_in_memory().expect("open");
        let r = b
            .store_import(&document)
            .expect("the document must restore, not refuse a dangling end");
        assert_eq!(r["links_imported"], json!(0), "{r}");
    }
}
