//! Transfer domain methods for Engine.

use super::*;

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
        let filter = Filter::parse(&opt_str(p, "filter")?.unwrap_or_default(), Timestamp::now())
            .map_err(ApiError::bad_request)?;
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
        let mut snapshots = self.load_task_snapshots()?;
        snapshots.sort_by_key(|snapshot| snapshot.task.short_id);

        // Pass 1: which tasks survive the filter. Edges can only be resolved
        // against the *whole* selected set, so nothing is emitted yet.
        let mut selected = Vec::new();
        for snapshot in snapshots {
            let t = &snapshot.task;
            let ctx = MatchCtx {
                status: t.status,
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
            out.push(Self::export_task(snapshot, &present, &mut dropped));
        }
        Ok(json!({
            "tasks": out,
            "dropped_dependencies": dropped,
            // D37: a project is a RECORD (D21/D22/D23), not a string that happens
            // to appear on tasks, and an export that carries only the string is
            // not the self-contained document D12 promises — restoring it lost
            // every description, every archived flag, and the default, leaving a
            // store whose tasks name projects `tasqx projects` does not list and
            // `task.add` refuses. Always ALL of them, archived included and
            // regardless of `filter`: a filter selects TASKS, and a project the
            // selected tasks do not mention is not a dangling pointer, so there
            // is nothing to trim and no second `dropped_` counter to explain.
            "projects": self.export_projects()?,
            // D41 memory docs, ALL of them regardless of `filter` — a filter
            // selects tasks, and knowledge is not attached to a task. Omitting
            // them was the D37 omission shape reintroduced: a backup that
            // answered ok:true and silently lost every doc on restore (review
            // finding).
            "docs": self.export_docs()?,
            // Store state, so the document carries it (D21: it lives in the
            // store's `config` table, never in config.toml). `null` when there
            // is none, which is a fact and not an omission.
            "default_project": self.default_project()?,
        }))
    }

    /// Every memory doc row, id-ordered (creation order, since UUIDv7). D41.
    fn export_docs(&self) -> Result<Vec<Value>, ApiError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, source, title, body, created, modified FROM docs ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "source": r.get::<_, Option<String>>(1)?,
                "title": r.get::<_, String>(2)?,
                "body": r.get::<_, String>(3)?,
                "created": r.get::<_, String>(4)?,
                "modified": r.get::<_, String>(5)?,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Every project row, name-ordered, in the canonical §3 shape. D37.
    fn export_projects(&self) -> Result<Vec<Value>, ApiError> {
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
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Build the canonical §3 export object for one task. serde_json's default
    /// `Map` is a `BTreeMap`, so keys serialize sorted (canonical form).
    /// `present` is the id set being exported; edges leaving it are dropped and
    /// counted into `dropped`.
    fn export_task(snapshot: &TaskSnapshot, present: &HashSet<&str>, dropped: &mut i64) -> Value {
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
                "depends_on": kept,
                "annotations": snapshot.annotations,
                "urgency": urgency::score(t.priority, t.due.as_deref(), &t.created),
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
        // Last, and exactly once. It only ever ADDS `status_unrecognized`, so
        // wrapping the literal or the finished object is the same document —
        // wrapping last is the spelling that stays correct as conditional keys
        // are added above it.
        flag_unrecognized_status(t, out)
    }

    // ---- store.import --------------------------------------------------------

    /// `store.import` — load an export document. Params: `tasks` (required),
    /// `projects`, `default_project`, `docs`.
    ///
    /// The one method whose params are a DOCUMENT, not a request (the `document`
    /// flag in [`crate::PARAMS`]): an export written by a newer tasqx must stay
    /// readable here, so an unrecognized top-level key is a future field rather
    /// than a typo. That tolerance is only safe because `tasks` is required — a
    /// misspelled `taskss` is still refused, by absence.
    pub fn store_import(&self, p: &Value) -> Result<Value, ApiError> {
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
        // (task_id, its raw `depends_on` value) collected during pass 1.
        // (task_id, its validated `depends_on` ids) — typed at collection time so
        // pass 2 cannot inherit a silently-dropped edge from a wrong-typed value.
        let mut edges: Vec<(String, Vec<String>)> = Vec::new();
        // Task ids this payload has already written, so a short_id collision can
        // say whether the number was taken by the DESTINATION or by an earlier
        // task in the same document — two faults with two different remedies.
        let mut written: HashSet<String> = HashSet::new();
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
        let mut docs_imported = 0i64;
        if let Some(rows) = opt_array(p, "docs")?.cloned() {
            for dv in &rows {
                let dv = import_shape("", "doc", dv)?;
                import_keys("", "doc", dv, IMPORT_DOC_KEYS)?;
                let did = opt_str_nonempty(dv, "id")?.unwrap_or_else(|| Uuid::now_v7().to_string());
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
                let created = opt_str_nonempty(dv, "created")?.unwrap_or_else(now);
                let modified = opt_str_nonempty(dv, "modified")?.unwrap_or_else(now);
                tx.execute(
                    "INSERT INTO docs (id, source, title, body, created, modified) \
                     VALUES (?1,?2,?3,?4,?5,?6) \
                     ON CONFLICT(id) DO UPDATE SET \
                     source=excluded.source, title=excluded.title, body=excluded.body, \
                     created=excluded.created, modified=excluded.modified",
                    params![did, source, title, body, created, modified],
                )?;
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
            let short_id = import_field(id, "short_id", req_i64(tv, "short_id"))?;
            // D17's rule where the value ENTERS: `short_id` is untrusted i64 and
            // the mint floor below is `short_id + 1`, which panicked in debug and
            // wrapped in release at `i64::MAX` — leaving a floor of `i64::MIN`, so
            // the next `add` re-minted a live short_id and broke D4. The counter
            // starts at 1 and only advances, so anything outside 1..i64::MAX is a
            // value no minter could have produced.
            let short_id_floor = short_id
                .checked_add(1)
                .filter(|_| short_id >= 1)
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "store.import: task {id} has short_id {short_id} — expected an integer \
                         from 1 to {}",
                        i64::MAX - 1
                    ))
                })?;
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
            let now_ts = Timestamp::now();
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
            // Through the same date gate as created/modified/completed, so a
            // malformed anchor is named rather than stored.
            let active_since =
                import_field(id, "active_since", opt_when(tv, "active_since", now_ts))?;
            let urgency =
                urgency::score(priority.and_then(Priority::parse), due.as_deref(), &created);

            // D4: `short_id` is the handle the whole CLI addresses a task by, and
            // the column is NOT NULL UNIQUE — so a payload carrying a number some
            // OTHER task in this store already holds cannot be written. The
            // upsert keys on `id`, which never noticed, and the raw UNIQUE
            // violation came back through `From<rusqlite::Error>` as `internal` /
            // exit 1: the code §4 reserves for "internal bug; safe to
            // retry-report", with a message naming neither task. Merging two
            // machines' stores, or restoring a filtered export on top of live
            // work, is a user error with an obvious remedy — it must read as one.
            //
            // `conflict` (exit 5), not `bad_request`: §4 files a duplicate under
            // conflict, and the document is not malformed — it is this
            // DESTINATION that already holds the number. The self-dependency and
            // cycle guards in pass 2 spell the same distinction the same way.
            // Hence also the message built by hand rather than through
            // `import_field`, which relabels everything it wraps as bad_request.
            //
            // BEFORE the upsert, so it also catches a payload that moves one
            // task's short_id onto another and two payload tasks claiming one
            // number (the second sees the first, already inserted in this
            // transaction). `.optional()`: no row is the normal case, and
            // `query_row` would otherwise raise `QueryReturnedNoRows`.
            let owner: Option<String> = tx
                .query_row(
                    "SELECT id FROM tasks WHERE short_id = ?1",
                    params![short_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(other) = owner.filter(|owner| owner != id) {
                // Two faults, two remedies, so they must not share one sentence:
                // a number this very payload already handed to another task is an
                // incoherent DOCUMENT, and "import into a fresh store" fixes
                // nothing — the second task would collide there too.
                if written.contains(&other) {
                    return Err(ApiError::conflict(format!(
                        "store.import: task {id} carries short_id {short_id}, which task {other} \
                         in the same payload already claims — one short_id addresses exactly one \
                         task, so this document cannot be restored anywhere"
                    )));
                }
                return Err(ApiError::conflict(format!(
                    "store.import: task {id} carries short_id {short_id}, which already belongs \
                     to task {other} in this store — import into a fresh store, or renumber"
                )));
            }
            // Set, not a scan of `edges`: an import of 10k tasks would otherwise
            // cost 10k² comparisons for a check that fires almost never.
            written.insert(id.to_string());

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
            tx.execute(
                "INSERT INTO tasks (id, short_id, title, status, priority, project, due, \
                 scheduled, wait, estimate, recurrence, urgency, active_since, tracked_seconds, \
                 rev, created, modified, completed, remind) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12, \
                 CASE WHEN ?4 = 'active' THEN COALESCE(?18,?19) ELSE NULL END, \
                 COALESCE(?20,0),?13,?14,?15,?16,?17) \
                 ON CONFLICT(id) DO UPDATE SET \
                 short_id=?2, title=?3, status=?4, priority=?5, project=?6, due=?7, \
                 scheduled=?8, wait=?9, estimate=?10, recurrence=?11, urgency=?12, \
                 active_since = CASE WHEN ?4 = 'active' \
                 THEN COALESCE(?18, active_since, ?19) ELSE NULL END, \
                 tracked_seconds = COALESCE(?20, tracked_seconds), \
                 rev=?13, created=?14, modified=?15, completed=?16, remind=?17",
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
                    rev,
                    created,
                    modified,
                    completed,
                    remind,
                    active_since,
                    now(),
                    tracked_seconds
                ],
            )?;

            // short_id must never later be re-minted (§12-D4).
            bump_short_id_floor(&tx, short_id_floor)?;

            // Replace tags.
            tx.execute("DELETE FROM task_tags WHERE task_id = ?1", params![id])?;
            for tg in import_field(id, "tags", opt_str_array(tv, "tags"))? {
                ensure_tag_link(&tx, id, &tg)?;
            }

            // Replace annotations.
            tx.execute("DELETE FROM annotations WHERE task_id = ?1", params![id])?;
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
                        .unwrap_or_else(|| Uuid::now_v7().to_string());
                    let body = import_field(id, "annotations[].body", req_str(a, "body"))?;
                    let acreated =
                        import_field(id, "annotations[].created", opt_str_nonempty(a, "created"))?
                            .unwrap_or_else(now);
                    // ON CONFLICT DO UPDATE, never INSERT OR REPLACE: REPLACE
                    // deletes the old row WITHOUT firing the delete trigger
                    // (recursive_triggers is off), so a payload that moves an
                    // annotation id from a task outside the payload left a
                    // dangling entry in annotations_fts — and once the freed
                    // rowid was reused, memory.search answered the OLD text
                    // with an UNRELATED annotation. The UPDATE path keeps the
                    // rowid and fires annotations_fts_au, which does the
                    // delete+insert pair the index needs (D41 review finding).
                    tx.execute(
                        "INSERT INTO annotations (id, task_id, body, created) \
                         VALUES (?1,?2,?3,?4) \
                         ON CONFLICT(id) DO UPDATE SET \
                         task_id=excluded.task_id, body=excluded.body, created=excluded.created",
                        params![aid, id, body, acreated],
                    )?;
                }
            }

            // Replace token measurements, wholesale like tags and annotations:
            // the payload's task object is authoritative about its own child
            // rows. Each row passes the same closed-vocabulary gates
            // `token.add` enforces, with `import_field` naming the task —
            // carrying an unknown source/confidence verbatim would let one bad
            // payload re-export the corruption to every downstream store (D16).
            tx.execute("DELETE FROM token_usage WHERE task_id = ?1", params![id])?;
            if let Some(measurements) = import_field(id, "tokens", opt_array(tv, "tokens"))? {
                for m in measurements {
                    import_keys(&format!("task {id}, "), "tokens[]", m, IMPORT_TOKEN_KEYS)?;
                    let mid = import_field(id, "tokens[].id", opt_str_nonempty(m, "id"))?
                        .unwrap_or_else(|| Uuid::now_v7().to_string());
                    let tool = import_field(id, "tokens[].tool", req_str(m, "tool"))?;
                    let source = import_field(id, "tokens[].source", req_str(m, "source"))?;
                    import_field(
                        id,
                        "tokens[].source",
                        crate::tokens::require_source(&source),
                    )?;
                    let confidence =
                        import_field(id, "tokens[].confidence", req_str(m, "confidence"))?;
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
                    let taken: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM token_usage WHERE id = ?1)",
                        params![mid],
                        |r| r.get(0),
                    )?;
                    import_field(
                        id,
                        "tokens[].id",
                        if taken {
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
                         cache_creation_tokens, confidence, created) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
                            mcreated
                        ],
                    )?;
                }
            }

            // Edges are deferred to pass 2: a payload may list a target *after*
            // its dependent, and the FOREIGN KEY would reject it here.
            tx.execute("DELETE FROM dependencies WHERE task_id = ?1", params![id])?;
            edges.push((
                id.to_string(),
                import_field(id, "depends_on", opt_str_array(tv, "depends_on"))?,
            ));

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
                tx.execute(
                    "INSERT OR IGNORE INTO dependencies (task_id, depends_on_id) VALUES (?1,?2)",
                    params![id, d],
                )?;
            }
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
        tx.commit()?;

        // All four always present: a machine consumer must be able to tell "no
        // projects in the document" from "this build does not report them", the
        // same reason `dropped_dependencies` and `default_cleared` are never
        // omitted.
        Ok(json!({
            "imported": imported,
            "projects_imported": projects_imported,
            "projects_created": projects_created,
            "docs_imported": docs_imported,
            "default_project": default_project,
        }))
    }
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

    /// A payload short_id that a DIFFERENT task in the destination already holds
    /// is a collision the caller can act on, not an engine bug. It used to hit
    /// the raw `tasks.short_id` UNIQUE constraint, which `From<rusqlite::Error>`
    /// turns into `internal` / exit 1 — the code §4 reserves for "internal bug;
    /// safe to retry-report" — with a message naming no task at all.
    #[test]
    fn store_import_refuses_a_short_id_another_task_already_holds() {
        let e = Engine::open_in_memory().expect("open");
        let mine = e.task_add(&json!({ "title": "already here" })).expect("add");
        let mine_id = mine["id"].as_str().expect("id").to_string();
        let taken = mine["short_id"].as_i64().expect("short_id");
        let taken_text = taken.to_string();

        const THEIRS: &str = "0193aaaa-0000-7000-8000-00000000beef";
        let err = e
            .store_import(&json!({ "tasks": [
                { "id": THEIRS, "short_id": taken, "title": "from the other store" },
            ] }))
            .expect_err("a collision must not be accepted");

        assert_eq!(err.code, ErrorCode::Conflict, "{}", err.message);
        for needle in [mine_id.as_str(), THEIRS, taken_text.as_str()] {
            assert!(
                err.message.contains(needle),
                "message must name {needle}: {}",
                err.message
            );
        }
        assert!(
            !err.message.contains("UNIQUE constraint"),
            "the raw SQLite string diagnoses nothing: {}",
            err.message
        );
        // The remedy, not just the diagnosis: the document is fine, it is this
        // destination that is already using the number.
        assert!(
            err.message.contains("in this store") && err.message.contains("fresh store"),
            "message must say where the number is taken and what to do: {}",
            err.message
        );
        // Same transaction, so the refusal writes nothing at all.
        assert_eq!(exported_task_count(&e), 1);
    }

    /// The same guard, one step earlier: two payload tasks claiming one short_id
    /// is an incoherent document, and the second one sees the first because both
    /// are written inside the import's own transaction. A DIFFERENT fault from
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

        let again = e.store_import(&document).expect("re-import must still work");
        assert_eq!(again["imported"], 2);
        assert_eq!(
            e.store_export(&json!({})).expect("export"),
            document,
            "export -> import -> export stays identity"
        );
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
            .find("self.load_task_snapshots()")
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

        e.task_add(&json!({ "title": "two" })).expect("add after export");
        assert_eq!(exported_task_count(&e), 2);
    }
}
