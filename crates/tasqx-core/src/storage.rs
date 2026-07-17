//! Storage layer: connection setup, embedded schema migration, and the small
//! row-level primitives the engine builds mutations from (DESIGN.md §2, §3).
//!
//! Design invariants enforced here:
//!  * SQLite opened in **WAL** mode with a `busy_timeout` so racing one-shot
//!    invocations wait briefly instead of erroring.
//!  * `short_id` is minted from a monotonic counter in the `meta` table — it is
//!    stable forever and never recycled (§12-D4).
//!  * The `events` table is append-only; every mutation writes exactly one row
//!    per changed entity *in the same transaction* as the state change. That
//!    coupling is the whole point of the layer, so the insert helper lives here.

use std::collections::HashSet;

use rusqlite::{params, Connection, Row, Transaction};
use uuid::Uuid;

use crate::error::ApiError;
use crate::types::{Priority, Status, Task};
use crate::util::now;

/// Busy timeout for contended one-shot writers (DESIGN.md §2: ~3s).
const BUSY_TIMEOUT_MS: u32 = 3000;

/// Column list shared by every task SELECT, kept in sync with `map_task_row`.
/// New columns are **appended**, never inserted: the order here is the positional
/// contract `map_task_row` reads by index, so appending leaves every existing
/// index untouched.
pub const TASK_COLS: &str = "id, short_id, title, status, priority, project, due, \
    scheduled, wait, estimate, recurrence, urgency, active_since, tracked_seconds, \
    rev, created, modified, completed, remind";

/// Open (creating if needed) the store at `path`, apply pragmas + migration.
pub fn open(path: &str) -> Result<Connection, ApiError> {
    let conn = Connection::open(path)
        .map_err(|e| ApiError::internal(format!("cannot open store {path}: {e}")))?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Open an in-memory store (used by tests).
pub fn open_in_memory() -> Result<Connection, ApiError> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<(), ApiError> {
    // WAL: concurrent readers never block; writers serialized by SQLite.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))?;
    Ok(())
}

/// Idempotent schema migration. Creates all MVP entity tables plus the
/// append-only event log and the `meta` counter table, with indices sized for
/// the §5 list filters (status, project, due, tag).
fn migrate(conn: &Connection) -> Result<(), ApiError> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value INTEGER NOT NULL
        );
        INSERT OR IGNORE INTO meta(key, value) VALUES ('next_short_id', 1);

        -- String-valued settings (e.g. default_project). Separate from `meta`
        -- because that table is integer-only (the short_id counter).
        CREATE TABLE IF NOT EXISTS config (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS projects (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL UNIQUE,
            description TEXT,
            archived    INTEGER NOT NULL DEFAULT 0,
            created     TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tasks (
            id              TEXT PRIMARY KEY,
            short_id        INTEGER NOT NULL UNIQUE,
            title           TEXT NOT NULL,
            status          TEXT NOT NULL,
            priority        TEXT,
            project         TEXT,
            due             TEXT,
            scheduled       TEXT,
            wait            TEXT,
            estimate        TEXT,
            recurrence      TEXT,
            urgency         REAL NOT NULL DEFAULT 0,
            active_since    TEXT,
            tracked_seconds INTEGER NOT NULL DEFAULT 0,
            rev             INTEGER NOT NULL DEFAULT 0,
            created         TEXT NOT NULL,
            modified        TEXT NOT NULL,
            completed       TEXT,
            -- Reminder spec (§9): a signed offset anchored to `due` (`-1h`) or an
            -- absolute RFC3339 instant. See `crate::remind` for the canonical form.
            remind          TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_status  ON tasks(status);
        CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project);
        CREATE INDEX IF NOT EXISTS idx_tasks_due     ON tasks(due);

        CREATE TABLE IF NOT EXISTS tags (
            id   TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS task_tags (
            task_id TEXT NOT NULL,
            tag_id  TEXT NOT NULL,
            PRIMARY KEY (task_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_task_tags_tag ON task_tags(tag_id);

        -- The FOREIGN KEYs are load-bearing, not decoration: a dangling edge is
        -- invisible to every reader that joins `tasks` (blocked/depends_on) yet
        -- was still exported, so it could never be seen or removed but did
        -- resurface later. `foreign_keys=ON` (see `configure`) enforces these.
        CREATE TABLE IF NOT EXISTS dependencies (
            task_id       TEXT NOT NULL,
            depends_on_id TEXT NOT NULL,
            PRIMARY KEY (task_id, depends_on_id),
            FOREIGN KEY (task_id)       REFERENCES tasks(id),
            FOREIGN KEY (depends_on_id) REFERENCES tasks(id)
        );
        CREATE INDEX IF NOT EXISTS idx_deps_dependson ON dependencies(depends_on_id);

        CREATE TABLE IF NOT EXISTS annotations (
            id      TEXT PRIMARY KEY,
            task_id TEXT NOT NULL,
            body    TEXT NOT NULL,
            created TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_annotations_task ON annotations(task_id);

        CREATE TABLE IF NOT EXISTS events (
            id        TEXT PRIMARY KEY,
            entity    TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            op        TEXT NOT NULL,
            payload   TEXT,
            ts        TEXT NOT NULL,
            actor     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_events_entity ON events(entity, entity_id);
        -- event.list {ref} scopes by entity_id alone; the composite index above
        -- is entity-leading and can't serve it, so index entity_id directly.
        CREATE INDEX IF NOT EXISTS idx_events_entity_id ON events(entity_id);
        "#,
    )?;

    // `CREATE TABLE IF NOT EXISTS` above is a no-op on a store that predates a
    // column, so additive columns need an explicit ALTER for existing files.
    // Fresh stores get `remind` from the CREATE and skip this.
    add_column_if_missing(conn, "tasks", "remind", "TEXT")?;

    // Must follow the ALTER: on an upgraded store the column does not exist
    // until the statement above runs. Partial, because the scheduler only ever
    // asks for the (typically tiny) set of tasks that carry a reminder.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_tasks_remind ON tasks(remind) \
         WHERE remind IS NOT NULL;",
    )?;

    add_dependency_foreign_keys_if_missing(conn)?;
    repair_stale_default_project(conn)?;
    Ok(())
}

/// D23: drop a `default_project` key that names a project the store cannot show
/// — one that is archived or gone entirely.
///
/// Every *new* write already upholds "the default names a live project"
/// (`project.create`/`project.use` validate it, `project.archive` clears it per
/// D22), but a store written by older code could hold a default pointing at an
/// archived project: the old `create` let the newest project steal the key and
/// the old `archive` did not clear it. The invariant is therefore not enforced
/// by the writers alone — the file has to be repaired on the way in, once, or
/// the store keeps routing bare adds into a project no read surface lists while
/// `tasqx projects` shows no default at all.
///
/// Deliberately silent (no event row): this is a schema/consistency migration
/// like the `remind` ALTER beside it, not a user mutation. It runs on every
/// open and is a no-op on a healthy store.
fn repair_stale_default_project(conn: &Connection) -> Result<(), ApiError> {
    conn.execute(
        "DELETE FROM config WHERE key = 'default_project' \
         AND value NOT IN (SELECT name FROM projects WHERE archived = 0)",
        [],
    )?;
    Ok(())
}

/// Rebuild `dependencies` with the two FOREIGN KEYs on a store created before
/// they existed. `CREATE TABLE IF NOT EXISTS` is a no-op there and SQLite has no
/// `ADD CONSTRAINT`, so the table is recreated per the official 12-step ALTER
/// recipe. Any edge that is already dangling is *dropped* rather than carried:
/// it could not be seen or deleted through the API anyway, and keeping it would
/// fail the rebuild's FK check.
fn add_dependency_foreign_keys_if_missing(conn: &Connection) -> Result<(), ApiError> {
    let has_fk = conn
        .prepare("PRAGMA foreign_key_list(dependencies)")?
        .query_map([], |_| Ok(()))?
        .next()
        .is_some();
    if has_fk {
        return Ok(());
    }
    // `foreign_keys` is a no-op inside a transaction, so it must toggle outside
    // the batch below; the batch itself is atomic.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let res = conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        CREATE TABLE dependencies_new (
            task_id       TEXT NOT NULL,
            depends_on_id TEXT NOT NULL,
            PRIMARY KEY (task_id, depends_on_id),
            FOREIGN KEY (task_id)       REFERENCES tasks(id),
            FOREIGN KEY (depends_on_id) REFERENCES tasks(id)
        );
        INSERT INTO dependencies_new (task_id, depends_on_id)
            SELECT d.task_id, d.depends_on_id FROM dependencies d
            JOIN tasks a ON a.id = d.task_id
            JOIN tasks b ON b.id = d.depends_on_id;
        DROP TABLE dependencies;
        ALTER TABLE dependencies_new RENAME TO dependencies;
        CREATE INDEX IF NOT EXISTS idx_deps_dependson ON dependencies(depends_on_id);
        COMMIT;
        "#,
    );
    conn.pragma_update(None, "foreign_keys", "ON")?;
    res?;
    Ok(())
}

/// Add `col` to `table` when it isn't there yet — the additive-migration
/// primitive for stores created by an older build. SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, so the column list is checked first.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    col: &str,
    decl: &str,
) -> Result<(), ApiError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let present = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|c| c == col);
    if !present {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"))?;
    }
    Ok(())
}

/// Allocate the next monotonic `short_id` inside `tx`. Never recycles: the
/// counter only advances, even if a task is later removed (§12-D4).
pub fn alloc_short_id(tx: &Transaction) -> Result<i64, ApiError> {
    let cur: i64 =
        tx.query_row("SELECT value FROM meta WHERE key = 'next_short_id'", [], |r| r.get(0))?;
    tx.execute("UPDATE meta SET value = ?1 WHERE key = 'next_short_id'", params![cur + 1])?;
    Ok(cur)
}

/// Ensure the monotonic `short_id` counter is at least `n` (used by
/// `store.import`, which carries its own short_ids in and must never later
/// re-mint one). Never lowers the counter.
pub fn bump_short_id_floor(tx: &Transaction, n: i64) -> Result<(), ApiError> {
    tx.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'next_short_id' AND value < ?1",
        params![n],
    )?;
    Ok(())
}

/// Read a string setting from the `config` table (None if unset).
pub fn get_config(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM config WHERE key = ?1", params![key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

/// Upsert a string setting inside `tx`.
pub fn set_config(tx: &Transaction, key: &str, value: &str) -> Result<(), ApiError> {
    tx.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}

/// Delete a setting inside `tx`, reporting whether a row was actually removed.
/// Used by `project.archive` to un-point a default aimed at the project it is
/// retiring (D22) — the caller reports that to the user rather than leaving the
/// store with a default the project list no longer shows.
pub fn clear_config(tx: &Transaction, key: &str) -> Result<bool, ApiError> {
    let n = tx.execute("DELETE FROM config WHERE key = ?1", params![key])?;
    Ok(n > 0)
}

/// Append one event row. THE invariant: this runs inside the same `tx` as the
/// state change it records, so state and history can never diverge.
pub fn insert_event(
    tx: &Transaction,
    entity: &str,
    entity_id: &str,
    op: &str,
    payload: &serde_json::Value,
) -> Result<(), ApiError> {
    tx.execute(
        "INSERT INTO events (id, entity, entity_id, op, payload, ts, actor) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            Uuid::now_v7().to_string(),
            entity,
            entity_id,
            op,
            payload.to_string(),
            now(),
            "user",
        ],
    )?;
    Ok(())
}

/// Map a `SELECT {TASK_COLS}` row into a `Task`.
pub fn map_task_row(row: &Row) -> rusqlite::Result<Task> {
    let status: String = row.get(3)?;
    let priority: Option<String> = row.get(4)?;
    Ok(Task {
        id: row.get(0)?,
        short_id: row.get(1)?,
        title: row.get(2)?,
        status: Status::parse(&status).unwrap_or(Status::Pending),
        priority: priority.as_deref().and_then(Priority::parse),
        project: row.get(5)?,
        due: row.get(6)?,
        scheduled: row.get(7)?,
        wait: row.get(8)?,
        estimate: row.get(9)?,
        recurrence: row.get(10)?,
        urgency: row.get(11)?,
        active_since: row.get(12)?,
        tracked_seconds: row.get(13)?,
        rev: row.get(14)?,
        created: row.get(15)?,
        modified: row.get(16)?,
        completed: row.get(17)?,
        remind: row.get(18)?,
    })
}

/// True when a `reminded` event already exists for this exact (task, instant).
///
/// The dedupe check (§9). Keyed on the *instant* as well as the task so that
/// moving `due` — which moves a relative reminder to a new instant — is a new
/// reminder rather than a suppressed one. Takes `&Connection` so it runs on
/// either the engine connection or an open `Transaction` (which derefs to
/// `Connection`), letting `reminder.fire` re-check inside its own write lock.
pub fn already_reminded(conn: &Connection, task_id: &str, at: &str) -> Result<bool, ApiError> {
    let mut stmt =
        conn.prepare("SELECT payload FROM events WHERE entity_id = ?1 AND op = 'reminded'")?;
    let rows = stmt.query_map(params![task_id], |r| r.get::<_, Option<String>>(0))?;
    for r in rows {
        let Some(payload) = r? else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else {
            continue; // a malformed payload must never wedge the check
        };
        if v.get("at").and_then(serde_json::Value::as_str) == Some(at) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Every already-reminded `(task_id, instant)` pair, in one query. The bulk form
/// of [`already_reminded`], so a scheduler rebuild stays O(1) queries instead of
/// one per task.
pub fn reminded_keys(conn: &Connection) -> Result<HashSet<(String, String)>, ApiError> {
    let mut stmt =
        conn.prepare("SELECT entity_id, payload FROM events WHERE op = 'reminded'")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = HashSet::new();
    for r in rows {
        let (task_id, payload) = r?;
        let Some(payload) = payload else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else {
            continue;
        };
        if let Some(at) = v.get("at").and_then(serde_json::Value::as_str) {
            out.insert((task_id, at.to_string()));
        }
    }
    Ok(out)
}

/// Load all tag names attached to a task, sorted for stable output.
pub fn task_tags(conn: &Connection, task_id: &str) -> Result<Vec<String>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT t.name FROM tags t \
         JOIN task_tags tt ON tt.tag_id = t.id \
         WHERE tt.task_id = ?1 ORDER BY t.name",
    )?;
    let rows = stmt.query_map(params![task_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Ensure a tag row exists (by name) and link it to a task inside `tx`.
/// Returns silently if the link already exists.
pub fn ensure_tag_link(tx: &Transaction, task_id: &str, tag_name: &str) -> Result<(), ApiError> {
    let existing: Option<String> = tx
        .query_row("SELECT id FROM tags WHERE name = ?1", params![tag_name], |r| r.get(0))
        .ok();
    let tag_id = match existing {
        Some(id) => id,
        None => {
            let id = Uuid::now_v7().to_string();
            tx.execute(
                "INSERT INTO tags (id, name) VALUES (?1, ?2)",
                params![id, tag_name],
            )?;
            id
        }
    };
    tx.execute(
        "INSERT OR IGNORE INTO task_tags (task_id, tag_id) VALUES (?1, ?2)",
        params![task_id, tag_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fk_count(conn: &Connection) -> usize {
        conn.prepare("PRAGMA foreign_key_list(dependencies)")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .count()
    }

    /// A store created before D12 has a `dependencies` table with no FOREIGN
    /// KEYs (and `CREATE TABLE IF NOT EXISTS` will not add them), possibly
    /// holding edges that are already dangling. Migration must rebuild the table
    /// with both constraints, keep every valid edge, and drop the dangling ones.
    #[test]
    fn migration_adds_dependency_foreign_keys_and_drops_dangling_edges() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        // Hand-build the legacy shape (pre-D12): no FKs.
        conn.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, short_id INTEGER NOT NULL UNIQUE, \
               title TEXT NOT NULL, status TEXT NOT NULL, priority TEXT, project TEXT, \
               due TEXT, scheduled TEXT, wait TEXT, estimate TEXT, recurrence TEXT, \
               urgency REAL NOT NULL DEFAULT 0, active_since TEXT, \
               tracked_seconds INTEGER NOT NULL DEFAULT 0, rev INTEGER NOT NULL DEFAULT 0, \
               created TEXT NOT NULL, modified TEXT NOT NULL, completed TEXT);
             CREATE TABLE dependencies (
                task_id TEXT NOT NULL, depends_on_id TEXT NOT NULL,
                PRIMARY KEY (task_id, depends_on_id));
             INSERT INTO tasks (id, short_id, title, status, created, modified)
                VALUES ('a',1,'blocker','pending','t','t'), ('b',2,'dependent','pending','t','t');
             INSERT INTO dependencies VALUES ('b','a');   -- valid
             INSERT INTO dependencies VALUES ('b','gone'); -- dangling, invisible, unremovable",
        )
        .unwrap();
        assert_eq!(fk_count(&conn), 0, "legacy store has no constraints");

        migrate(&conn).unwrap();

        assert_eq!(fk_count(&conn), 2, "both FOREIGN KEYs are now declared");
        let edges: Vec<(String, String)> = conn
            .prepare("SELECT task_id, depends_on_id FROM dependencies ORDER BY depends_on_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(edges, vec![("b".to_string(), "a".to_string())]);

        // The constraint is live, not merely declared.
        let err = conn.execute(
            "INSERT INTO dependencies VALUES ('b','nope')",
            [],
        );
        assert!(err.is_err(), "a dangling edge must now be rejected by SQLite");

        // Idempotent: a second migrate is a no-op, not another rebuild.
        migrate(&conn).unwrap();
        assert_eq!(fk_count(&conn), 2);
    }
}
