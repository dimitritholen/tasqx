//! Projects domain methods for Engine.

use super::*;

impl Engine {
    // ---- project.create ------------------------------------------------------

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
                format!("no project named {name}"),
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
    /// Set by the first `project.create` and moved only by `project.use` (D21).
    pub fn default_project(&self) -> Result<Option<String>, ApiError> {
        get_config(&self.conn, DEFAULT_PROJECT_KEY)
    }

    // ---- project.list --------------------------------------------------------

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
        Ok(json!({ "count": out.len(), "projects": out }))
    }

    // ---- project.archive -----------------------------------------------------

    pub fn project_archive(&self, p: &Value) -> Result<Value, ApiError> {
        // A lookup, like `project.use`: retiring a legacy whitespace-named
        // project is precisely the escape hatch D36 must not weld shut (D28).
        let name = req_str_lookup(p, "name")?;
        let id: String = self
            .conn
            .query_row(
                "SELECT id FROM projects WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => ApiError::not_found(
                    format!("no project named {name}"),
                    Some(json!({ "name": name })),
                ),
                other => other.into(),
            })?;

        let tx = self.begin_mutation()?;
        tx.execute(
            "UPDATE projects SET archived = 1 WHERE id = ?1",
            params![id],
        )?;
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
            &json!({ "name": name, "default_cleared": default_cleared }),
        )?;
        tx.commit()?;

        // Always present, never omitted: a machine consumer must be able to tell
        // "did not clear" from "this build does not report it".
        Ok(json!({ "name": name, "archived": true, "default_cleared": default_cleared }))
    }
}
