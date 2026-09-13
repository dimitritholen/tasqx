//! Projects domain methods for Engine.

use super::*;

/// #229 item 3: `project.use` and `project.archive`'s not_found on an unknown
/// name used to say only "no project named X", while the same class of error
/// on `task.add --project`/`task.modify --project` (`require_live_project`,
/// `engine.rs`) names the fix inline. This is that hint, plus the pointer
/// `require_live_project` does not need but these two verbs do: the likeliest
/// reason `archive X` (and, less often, `use X`) comes back not_found is that
/// X is already archived — out of the default `projects` listing — and
/// `--all` is exactly what reveals that case.
fn unknown_project_message(name: &str) -> String {
    format!(
        "no project named {name} (`tasqx projects --all` lists archived ones; \
         create it with `tasqx init {name}`)"
    )
}

impl Engine {
    // ---- project.create ------------------------------------------------------

    /// `project.create` — mint a project. Params: `name` (the dotted path),
    /// optional `description`. A name that already exists is `conflict`.
    ///
    /// The first project created also becomes the default (D21), so this is the
    /// method that decides where a bare `tasqx add` lands.
    pub fn project_create(&self, p: &Value) -> Result<Value, ApiError> {
        // D23's rule at the edge where a project name is *born* — `init " "`
        // used to mint a project that claimed the default, printed as a blank
        // row in `tasqx projects`, and (since `use` rejects the same string)
        // could never be re-selected once the default moved. D36 moved the check
        // itself into `req_str`, so this door no longer carries a private copy:
        // one rule means a title and a name cannot drift apart again.
        let name = req_str(p, "name")?;
        // D35: the last nullable free-text column with no parser in front of it.
        // `""` used to be laundered into NULL, so `init x --description "$UNSET"`
        // gave "no description" two spellings and threw the stated intent away —
        // D18's finding at the one edge D18 did not reach.
        let description = opt_str_nonempty(p, "description")?;

        let id = Uuid::now_v7().to_string();
        let ts = now();
        let tx = self.begin_mutation()?;
        // Duplicate check runs inside the IMMEDIATE tx: the write lock is already
        // held, so a racing project.create serializes behind us and its check
        // observes our committed row, yielding a clean `conflict` (not the
        // `internal` a bare UNIQUE-violation on the INSERT would produce).
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM projects WHERE name = ?1",
                params![name],
                |_| Ok(()),
            )
            .is_ok();
        if exists {
            return Err(ApiError::conflict(format!(
                "project already exists: {name}"
            )));
        }
        tx.execute(
            "INSERT INTO projects (id, name, description, archived, created) \
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![id, name, description, ts],
        )?;
        // D21: creating a project claims the default ONLY when the store has none
        // — the first project you ever create becomes the one a bare `task.add`
        // inherits, and nothing after that silently steals it. `project.use` is
        // the one explicit way to move it. Read inside the tx, which already
        // holds the write lock, so the check and the claim see one snapshot.
        let existing = get_config(&tx, DEFAULT_PROJECT_KEY)?;
        let claimed = existing.is_none();
        if claimed {
            set_config(&tx, DEFAULT_PROJECT_KEY, &name)?;
        }
        // D23: `default` is in the payload because this create may have moved the
        // default, and the log is where "where were bare adds landing?" is
        // answered. Its siblings already record it (`use` → `previous`,
        // `archive` → `default_cleared`); without it the log cannot say which
        // create claimed the key, and "the first create ever" is the wrong guess
        // for a store whose default was cleared by an archive and re-claimed
        // later (a sequence D22 blesses). Computed above so the row states what
        // this transaction actually did, and written inside it, as ever.
        insert_event(
            &tx,
            Entity::Project,
            &id,
            "create",
            &json!({ "name": name, "description": description, "default": claimed }),
        )?;
        tx.commit()?;

        // `default` is the truth of what happened, not a constant: the CLI paints
        // "now your default project" off this field, so it must be able to lie
        // no more than the store can. `current_default` says what the default IS
        // either way, so a caller who did not claim it still learns where a bare
        // `task.add` will go instead of having to ask a second method.
        let current_default = if claimed {
            Some(name.clone())
        } else {
            existing
        };
        Ok(json!({
            "id": id,
            "name": name,
            "default": claimed,
            "current_default": current_default,
        }))
    }

    // ---- project.use ---------------------------------------------------------

    /// D21: point the default project at an existing, live project. This is the
    /// only method that moves the default once it is set.
    pub fn project_use(&self, p: &Value) -> Result<Value, ApiError> {
        // D23: emptiness is checked where names are born (`project.create`), not
        // here. `req_str_lookup` still rejects "" (`use "$UNSET"` → bad_request),
        // and a whitespace-only name simply names no project, so the lookup below
        // answers it truthfully with not_found. The previous special case made
        // `use` reject a name `init` would happily create — a one-way door of
        // the exact kind D21 exists to remove, at a narrower edge. D36 is why
        // this is `_lookup` and not `req_str`: a store written before D23 can
        // still HOLD such a project, and a write-door rule applied here would
        // make it unselectable forever (D28).
        let name = req_str_lookup(p, "name")?;

        let tx = self.begin_mutation()?;
        // Existence + archived state are read inside the IMMEDIATE tx: the write
        // lock is held, so a racing `project.archive` serializes against us and
        // we can never commit a default aimed at a project archived mid-flight.
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT id, archived FROM projects WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (id, archived) = row.ok_or_else(|| {
            ApiError::not_found(
                unknown_project_message(&name),
                Some(json!({ "name": name })),
            )
        })?;
        // D22: archived means out of rotation. Pointing the default at one would
        // route every bare `add` into a project the default project list does not
        // even show — the invisible-state bug this whole change exists to kill.
        if archived != 0 {
            return Err(ApiError::conflict(format!(
                "project is archived: {name} (archived projects cannot be the default)"
            )));
        }

        let previous = get_config(&tx, DEFAULT_PROJECT_KEY)?;
        set_config(&tx, DEFAULT_PROJECT_KEY, &name)?;
        // THE invariant: the event row lands in the same transaction as the
        // mutation. The default is state, so moving it is history.
        insert_event(
            &tx,
            Entity::Project,
            &id,
            "use",
            &json!({ "name": name, "previous": previous }),
        )?;
        tx.commit()?;

        Ok(json!({ "name": name, "default": true, "previous": previous }))
    }

    /// The current default project — the project a bare `task.add` inherits.
    ///
    /// Claimed by any `project.create` made while the store holds no default,
    /// not just the first create ever (D21); moved explicitly by `project.use`;
    /// cleared by `project.archive` when the project it archives is the one the
    /// key names (D22); and cleared again by the stale-default repair on open,
    /// for a store written by older code whose default points at a project that
    /// is archived or gone. "The first create wins forever" is therefore the
    /// wrong thing to reason from: a store whose default an archive cleared has
    /// its next create claim the key again.
    pub fn default_project(&self) -> Result<Option<String>, ApiError> {
        get_config(&self.conn, DEFAULT_PROJECT_KEY)
    }

    // ---- project.list --------------------------------------------------------

    /// `project.list` — every project by name. Param: `include_archived`
    /// (default `false`). Each row carries a `default` flag read from the same
    /// config key `core.capabilities` reports, so the two cannot disagree.
    pub fn project_list(&self, p: &Value) -> Result<Value, ApiError> {
        let include_archived = opt_bool(p, "include_archived")?.unwrap_or(false);
        let sql = if include_archived {
            "SELECT id, name, description, archived FROM projects ORDER BY name"
        } else {
            "SELECT id, name, description, archived FROM projects WHERE archived = 0 ORDER BY name"
        };
        // D21: the default drives where a bare `add` lands, so the surface that
        // lists projects must say which one it is. Read once, outside the row
        // loop — this is the same fact `core.capabilities.default_project`
        // reports, from the same key, so the two can never disagree.
        let default = self.default_project()?;
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            let name = r.get::<_, String>(1)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "default": default.as_deref() == Some(name.as_str()),
                "name": name,
                "description": r.get::<_, Option<String>>(2)?,
                "archived": r.get::<_, i64>(3)? != 0,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(json!({
            "count": out.len(),
            // See `task_list`'s field of the same name (#233): a fresh store
            // and an `include_archived:false` read that hid every project
            // both come back with an empty `projects` array, and only this
            // tells the reader which one happened.
            "store_empty": self.store_is_empty()?,
            "projects": out,
        }))
    }

    // ---- project.archive -----------------------------------------------------

    /// `project.archive` — hide a project from the default listing by `name`.
    /// Its tasks are untouched: archiving is a shelf, not a delete.
    ///
    /// `name` is read as a LOOKUP rather than validated (D28/D36): retiring a
    /// legacy whitespace-named project is exactly the escape hatch the name
    /// rules must not weld shut.
    ///
    /// A project that is ALREADY archived is a `conflict` (D22), not a second
    /// `ok`: see the refusal below for why.
    ///
    /// D89: the result also carries `open_tasks` and `open_overdue` — the work
    /// this archive leaves fully live and unmentioned everywhere else (`list`,
    /// `agenda`, `report`). Counted inside the same IMMEDIATE transaction as the
    /// archive itself, against the same clock, so the number printed is exactly
    /// what a `tasqx list project:<name>` run immediately after would show —
    /// never a race against a concurrent add.
    pub fn project_archive(&self, p: &Value) -> Result<Value, ApiError> {
        // A lookup, like `project.use`: retiring a legacy whitespace-named
        // project is precisely the escape hatch D36 must not weld shut (D28).
        let name = req_str_lookup(p, "name")?;

        let tx = self.begin_mutation()?;
        // Existence + archived state are read inside the IMMEDIATE tx, for the
        // same reason `project.use` reads them there: the write lock is already
        // held, so a second `project.archive` racing this one serializes behind
        // us and observes our committed `archived = 1` instead of passing the
        // check beside us and committing a second archive of the same project.
        // The read used to sit outside the transaction, which was harmless only
        // as long as nothing was decided by it.
        let row: Option<(String, i64)> = tx
            .query_row(
                "SELECT id, archived FROM projects WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (id, already) = row.ok_or_else(|| {
            ApiError::not_found(
                unknown_project_message(&name),
                Some(json!({ "name": name })),
            )
        })?;
        // D22 both ways, and D33/D34's rule about an unfalsifiable write. This
        // method used to run `UPDATE ... SET archived = 1` over a project that
        // already had it and answer `{"archived": true, "default_cleared":
        // false}` — byte-identical to the run that genuinely retired it. So
        // `tasqx archive old` printed "Project old archived · your default
        // project is unchanged" every time, and neither a human nor a script
        // could tell "I retired it" from "it was already retired", while each
        // repeat appended another `archive` row to the event log — the audit
        // surface D22 points at to answer "where did the default go", now
        // carrying three archives of a project archived once. Refusing also
        // makes true the sentence D22, `tasqx archive --help` and the user
        // guide all state — that no verb may name an archived project — which
        // this method was the single exception to.
        //
        // Nothing is welded shut by the refusal: the project is already in the
        // state the caller asked for, and there is no `unarchive` to reach past
        // (`store.import` writes the flag, which is the documented way back).
        if already != 0 {
            return Err(ApiError::conflict(format!(
                "project is already archived: {name} (`tasqx projects --all` lists it; \
                 archiving it again would change nothing)"
            )));
        }
        tx.execute(
            "UPDATE projects SET archived = 1 WHERE id = ?1",
            params![id],
        )?;
        // D89: the count that makes the archive line honest. Read inside the
        // same transaction as the `UPDATE` above — the write lock is already
        // held, so a concurrent `task.add` into this project either lands
        // before this SELECT (and is counted) or serializes behind our commit
        // (and is not), never a torn read of "some but not all". A raw scan
        // rather than the bulk snapshot loader (`SnapshotParts`) on purpose:
        // this needs two numbers, not a filterable `Task` per row, tags, or
        // token buckets, and it must run against `tx`, which the loader is not
        // wired to take.
        let (open_tasks, open_overdue) = {
            let now_ts = Timestamp::now();
            let mut stmt = tx.prepare("SELECT status, due FROM tasks WHERE project = ?1")?;
            let mut rows = stmt.query(params![name])?;
            let (mut open, mut overdue) = (0i64, 0i64);
            while let Some(row) = rows.next()? {
                let status: String = row.get(0)?;
                // Same placeholder rule as `map_task_row_at`: an unrecognized
                // status (only reachable on a store written before D23 closed
                // the last unvalidated writer) reads as `Pending` — open — so
                // it stays counted rather than silently vanishing from a total
                // that exists specifically to keep abandoned-looking work
                // visible.
                let status = Status::parse(&status).unwrap_or(Status::Pending);
                if !status.is_open() {
                    continue;
                }
                open += 1;
                let due: Option<String> = row.get(1)?;
                if due
                    .as_deref()
                    .and_then(parse_ts)
                    .is_some_and(|d| crate::filter::overdue_at(d, now_ts))
                {
                    overdue += 1;
                }
            }
            (open, overdue)
        };
        // D22: archiving the *current* default un-points it, in this same
        // transaction. The alternative — leaving the default aimed at a retired
        // project — routes every bare `add` into a project `tasqx projects` no
        // longer lists, which is exactly the invisible state this change kills.
        // Clearing returns the store to the state a fresh one is in (no default,
        // bare `add` is projectless), and `use` is the way back.
        let default_cleared = get_config(&tx, DEFAULT_PROJECT_KEY)?.as_deref()
            == Some(name.as_str())
            && clear_config(&tx, DEFAULT_PROJECT_KEY)?;
        insert_event(
            &tx,
            Entity::Project,
            &id,
            "archive",
            &json!({
                "name": name,
                "default_cleared": default_cleared,
                "open_tasks": open_tasks,
                "open_overdue": open_overdue,
            }),
        )?;
        tx.commit()?;

        // Always present, never omitted: a machine consumer must be able to tell
        // "did not clear" from "this build does not report it", and the same
        // rule (D22) now covers `open_tasks`/`open_overdue` — a script asking
        // "did this leave work behind" gets a number, not an absent field it
        // has to distinguish from a build that never counted.
        Ok(json!({
            "name": name,
            "archived": true,
            "default_cleared": default_cleared,
            "open_tasks": open_tasks,
            "open_overdue": open_overdue,
        }))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// #229 item 3: `task.add --project nosuch` names the fix
    /// ("create it with `tasqx init nosuch`"); `project.use`/`project.archive`
    /// on an unknown name did not, even though it is the identical "no project
    /// named X" not_found. Pins both verbs against the hint `require_live_project`
    /// already gives `task.add`/`task.modify`.
    #[test]
    fn use_and_archive_on_an_unknown_project_name_the_fix_like_add_does() {
        let e = crate::Engine::open_in_memory().unwrap();

        let use_err = e
            .project_use(&json!({ "name": "nosuch" }))
            .expect_err("an unknown project must be not_found");
        assert_eq!(use_err.code, crate::ErrorCode::NotFound);
        assert!(
            use_err.message.contains("tasqx init nosuch"),
            "project.use's not_found must name the fix, like task.add's does: {}",
            use_err.message
        );

        let archive_err = e
            .project_archive(&json!({ "name": "nosuch" }))
            .expect_err("an unknown project must be not_found");
        assert!(
            archive_err.message.contains("tasqx init nosuch"),
            "project.archive's not_found must name the fix, like task.add's does: {}",
            archive_err.message
        );
        assert!(
            archive_err.message.contains("--all"),
            "archive's likeliest not_found cause is 'already archived', so the \
             message must point at `tasqx projects --all` which reveals that case: {}",
            archive_err.message
        );
    }
}
