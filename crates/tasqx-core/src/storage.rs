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

use jiff::Timestamp;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use uuid::Uuid;

use crate::error::ApiError;
use crate::types::{effective_status, Entity, Priority, Status, Task};
use crate::util::now;

/// Busy timeout for contended one-shot writers (DESIGN.md §2: ~3s).
const BUSY_TIMEOUT_MS: u32 = 3000;

/// Busy timeout for [`open_read_only`], two orders of magnitude below the
/// writer's. A reader on this path is inside somebody's keystroke and has a
/// wall-clock budget above it, so waiting out a contended write would spend the
/// whole budget on a lock and still produce nothing. Failing fast produces
/// nothing too, but instantly.
const READ_ONLY_BUSY_TIMEOUT_MS: u64 = 30;

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

/// Open an EXISTING store at `path` for reading only: no create, no migration,
/// no write lock, and a busy timeout measured in milliseconds rather than
/// seconds.
///
/// This is not a convenience twin of [`open`]; every difference from it is a
/// property some caller depends on, so none of them may be quietly restored:
///
///  * **No create.** [`open`] uses `Connection::open`, whose flags include
///    `SQLITE_OPEN_CREATE`, so it authors a database file for any path handed
///    to it. The caller this seam exists for is the shell-completion callback
///    (`tasqx-cli/src/complete.rs`), which runs on a Tab press. A keystroke that
///    did not run a command must not leave a database and a schema behind on a
///    machine that has never run tasqx, so an absent path is an `Err` here.
///  * **No migration.** `migrate` is DDL. Running it from a read path would
///    fail on the read-only connection anyway, but the deeper reason is that
///    upgrading a store's schema is a decision a command makes, never something
///    a completion lookup does on the user's behalf.
///
///    A code span and not a `[link]`, unlike [`open`] above, because `migrate`
///    is private: rustdoc resolves such a link only under
///    `--document-private-items` and 404s it for everyone who reads the
///    published page, so `-D rustdoc::private_intra_doc_links` rejects it. Naming
///    a private item in public prose is fine; linking to one is not. The repo
///    fixes this with the span rather than an `#[allow]` — see `ci.yml`'s rustdoc
///    step, which says so for the three earlier instances.
///  * **No `journal_mode = WAL`.** That pragma is a write to the database
///    header. Issuing it on a read-only connection fails, and "fixing" that by
///    dropping the read-only flag would undo the first two properties. WAL is
///    already persistent in the file the writer created, so a reader inherits
///    it without asking.
///  * **A short busy timeout.** [`open`]'s three seconds is right for a
///    one-shot writer that would otherwise report a spurious conflict. Here the
///    caller has its own wall-clock budget for the whole lookup and a long busy
///    wait inside a keystroke is precisely the stall this seam exists to avoid,
///    so contention degrades to "no candidates" almost immediately instead.
///
/// # What this does NOT promise: the `-shm`/`-wal` sidecars
///
/// **A read-only connection does create them.** This is the opposite of what
/// the obvious reading of `SQLITE_OPEN_READ_ONLY` suggests, and an earlier
/// version of this comment claimed the opposite. Measured, on a cleanly-closed
/// WAL store with no other connection open:
///
/// ```text
///   writer closed          ["tasks.db"]
///   opened read-only       ["tasks.db"]                  <- nothing yet
///   after one SELECT       ["tasks.db", "-shm", "-wal"]  <- both appear
///   reader dropped         ["tasks.db", "-shm", "-wal"]  <- both remain
/// ```
///
/// The flag governs the DATABASE FILE, not the WAL index. SQLite's shm layer
/// opens the `-shm` with `RDWR|CREATE` first and only falls back to a read-only
/// attempt if that fails, so a writable DIRECTORY is enough for both sidecars to
/// appear, whatever the connection flags say. They appear on first query rather
/// than at open, because that is when the WAL index is first needed.
///
/// They also stay. Deleting the pair on last-connection close is a write, and
/// this connection cannot perform one — so where an ordinary read-write reader
/// tidies up after itself, this one leaves them behind. An ordinary `tasqx list`
/// creates exactly the same two files during its run and then removes them
/// (measured: the directory holds only `tasks.db` after it exits).
///
/// So the honest promise is narrower than "nothing appears", and it is the one
/// the caller actually needs: **no database and no schema are created, and no
/// user data is written.** An absent path is still an `Err` with nothing left
/// behind, because the open fails before SQLite reaches the WAL layer at all.
/// What a completion callback can leave behind is two derived index files
/// belonging to a store that already existed, which the next writer reuses or
/// removes. It is not doing anything to the store that reading it normally does
/// not already do.
///
/// `immutable=1` would suppress the sidecars outright and is still rejected, but
/// on its own ground rather than on the false premise above: it *tells* SQLite
/// the file cannot change. Against a live writer that is a lie, and SQLite acts
/// on it — a concurrent write yields stale pages, garbage rows, or a spurious
/// "database disk image is malformed". Trading two harmless index files for
/// silently wrong query results, on a path whose entire job is to be harmless,
/// is not a trade worth making.
pub fn open_read_only(path: &str) -> Result<Connection, ApiError> {
    // Spelled out rather than `OpenFlags::default() - CREATE` because the flag
    // set is the whole security property of this function and subtraction from
    // a default hides what is actually being passed. `NO_MUTEX` and `URI` match
    // rusqlite's default so path handling is identical to [`open`]'s; `URI` is
    // safe to keep because SQLite refuses a `mode=` query parameter that is
    // LESS restrictive than the flags, so a crafted path cannot re-enable
    // writing or creation through it.
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(path, flags)
        .map_err(|e| ApiError::internal(format!("cannot open store {path}: {e}")))?;
    conn.busy_timeout(std::time::Duration::from_millis(READ_ONLY_BUSY_TIMEOUT_MS))?;
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

        -- AI token measurements, many per task (docs/research/token-accounting.md).
        -- Four separate counts by design — cache tokens cost a fraction of fresh
        -- ones, so a blended total destroys what a cost report needs. `tool` is
        -- free-form (new agents appear faster than releases); `source` and
        -- `confidence` are the closed vocabularies in `crate::tokens`, enforced
        -- at every write door. `extra` is reserved for per-tool oddities the
        -- later parser phases may need to carry (nothing writes it yet).
        CREATE TABLE IF NOT EXISTS token_usage (
            id                    TEXT PRIMARY KEY,
            task_id               TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            tool                  TEXT NOT NULL,
            source                TEXT NOT NULL,
            model                 TEXT,
            input_tokens          INTEGER NOT NULL DEFAULT 0,
            output_tokens         INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            extra                 TEXT,
            confidence            TEXT NOT NULL,
            created               TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_token_usage_task ON token_usage(task_id);

        -- Raw per-request OTLP samples buffered by the opt-in local receiver
        -- (#18, docs/research/token-accounting.md). Deliberately NOT joined to a
        -- task: these arrive over telemetry *before* any attribution and are
        -- matched to a task later by `session_id` + time window, so there is no
        -- `task_id` and no foreign key. `session_id` is nullable because a tool
        -- may emit a record without one. Bounded retention is enforced by the
        -- writer (`Engine::otlp_ingest`), not the schema.
        CREATE TABLE IF NOT EXISTS otlp_samples (
            id                    TEXT PRIMARY KEY,
            session_id            TEXT,
            tool                  TEXT NOT NULL,
            ts                    TEXT NOT NULL,
            model                 TEXT,
            input_tokens          INTEGER NOT NULL DEFAULT 0,
            output_tokens         INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            created               TEXT NOT NULL
        );
        -- Attribution looks samples up by the completing task's session id.
        CREATE INDEX IF NOT EXISTS idx_otlp_samples_session ON otlp_samples(session_id);

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
        -- `events` is append-only and never pruned, so an op-keyed filter over it
        -- is a scan that grows forever — against DESIGN.md "Keeping startup
        -- instant", which asks a query to touch an index, not the whole table.
        -- `reminded_keys` is the one that hurts: every scheduler rebuild runs it,
        -- up to 5x/s while anything is writing, *holding the engine mutex*, so
        -- its cost is every client's cost. Measured at 10^5 events: 4.1ms
        -- -> 0.08ms.
        --
        -- `entity_id` trails `op` rather than this being a bare `events(op)`,
        -- and that second column is not decoration. `already_reminded` filters
        -- `entity_id AND op`, and a single-column op index makes the planner
        -- drop `idx_events_entity_id` for it — swapping a seek over one task's
        -- handful of events for a seek over every `reminded` row ever written,
        -- i.e. paying for the rebuild by slowing the per-task check. With `op`
        -- leading and `entity_id` behind it, the rebuild seeks on the prefix and
        -- the dedupe check seeks on the whole key. `idx_events_entity_id` still
        -- serves `event.list {ref}`, which has no `op` to lead with.
        CREATE INDEX IF NOT EXISTS idx_events_op ON events(op, entity_id);
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
    migrate_memory(conn)?;
    Ok(())
}

/// D41 memory subsystem: the `docs` table plus FTS5 indexes over `docs` and
/// `annotations`, kept in sync by triggers so no writer can forget the index.
///
/// The rebuild step matters for upgrades: a store that predates this migration
/// already holds annotation rows the brand-new index has never seen, and
/// external-content FTS5 reads its content table only when told to. Rebuilding
/// unconditionally would rescan every body on every open, so it runs only when
/// this call actually created the index.
fn migrate_memory(conn: &Connection) -> Result<(), ApiError> {
    // One transaction around gate + DDL + rebuild (review finding): the gate
    // below is "annotations_fts exists", and the CREATE that makes it exist
    // used to commit separately from the rebuild it vouches for — a crash (or
    // disk-full) between the two left every pre-upgrade annotation silently
    // unsearchable forever, because every later open saw the table and skipped
    // the backfill. SQLite DDL is transactional, so rolling back the CREATEs
    // makes the next open retry from scratch.
    let tx = conn.unchecked_transaction()?;
    let fts_existed: bool = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='annotations_fts'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;

    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS docs (
            id       TEXT PRIMARY KEY,
            source   TEXT,
            title    TEXT NOT NULL,
            body     TEXT NOT NULL,
            created  TEXT NOT NULL,
            modified TEXT NOT NULL
        );

        -- External-content FTS: the index stores no second copy of the text;
        -- rows are joined back by rowid. Triggers are the only writers.
        CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            title, body, content='docs', content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS docs_fts_ai AFTER INSERT ON docs BEGIN
            INSERT INTO docs_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
        END;
        CREATE TRIGGER IF NOT EXISTS docs_fts_ad AFTER DELETE ON docs BEGIN
            INSERT INTO docs_fts(docs_fts, rowid, title, body)
                VALUES ('delete', old.rowid, old.title, old.body);
        END;
        CREATE TRIGGER IF NOT EXISTS docs_fts_au AFTER UPDATE ON docs BEGIN
            INSERT INTO docs_fts(docs_fts, rowid, title, body)
                VALUES ('delete', old.rowid, old.title, old.body);
            INSERT INTO docs_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
        END;

        CREATE VIRTUAL TABLE IF NOT EXISTS annotations_fts USING fts5(
            body, content='annotations', content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS annotations_fts_ai AFTER INSERT ON annotations BEGIN
            INSERT INTO annotations_fts(rowid, body) VALUES (new.rowid, new.body);
        END;
        CREATE TRIGGER IF NOT EXISTS annotations_fts_ad AFTER DELETE ON annotations BEGIN
            INSERT INTO annotations_fts(annotations_fts, rowid, body)
                VALUES ('delete', old.rowid, old.body);
        END;
        CREATE TRIGGER IF NOT EXISTS annotations_fts_au AFTER UPDATE ON annotations BEGIN
            INSERT INTO annotations_fts(annotations_fts, rowid, body)
                VALUES ('delete', old.rowid, old.body);
            INSERT INTO annotations_fts(rowid, body) VALUES (new.rowid, new.body);
        END;
        "#,
    )?;

    if !fts_existed {
        tx.execute_batch(
            "INSERT INTO annotations_fts(annotations_fts) VALUES('rebuild');
             INSERT INTO docs_fts(docs_fts) VALUES('rebuild');",
        )?;
    }
    tx.commit()?;
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
    let cur: i64 = tx.query_row(
        "SELECT value FROM meta WHERE key = 'next_short_id'",
        [],
        |r| r.get(0),
    )?;
    // Checked, not `cur + 1`: an import can legally carry a short_id up to
    // `i64::MAX - 1` (engine::store_import), which leaves the counter one mint
    // from the end. Wrapping there would hand the *next* task `i64::MIN` and
    // then re-mint every id from there — the D4 violation this counter exists
    // to prevent — so exhaustion has to be an error, not a silent restart.
    let next = cur.checked_add(1).ok_or_else(|| {
        ApiError::conflict(format!(
            "the short_id space is exhausted at {cur}; export, then import into a fresh store"
        ))
    })?;
    tx.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'next_short_id'",
        params![next],
    )?;
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
///
/// Only an absent row is absence. A damaged schema, I/O failure, or unexpected
/// value is a store error and must stop the caller rather than silently steering
/// a write as though the setting had never existed.
pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>, ApiError> {
    Ok(conn
        .query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
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
///
/// `entity` is the typed [`Entity`], not a `&str`, so the two spellings the
/// column may ever hold are the enum's variants rather than nineteen hand-typed
/// literals. That is what lets `event.list` state its accepted set from
/// [`Entity::ALL`] instead of keeping a second list in sync with these writers.
pub fn insert_event(
    tx: &Transaction,
    entity: Entity,
    entity_id: &str,
    op: &str,
    payload: &serde_json::Value,
) -> Result<(), ApiError> {
    tx.execute(
        "INSERT INTO events (id, entity, entity_id, op, payload, ts, actor) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            Uuid::now_v7().to_string(),
            entity.as_str(),
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
    // Tolerant, and loud about it. Every *writer* now validates (`store.import`
    // was the last hole), so an unrecognized status can only come from a store
    // written before that — which is exactly the store this must not brick.
    // Failing the read here made `list`, `show` AND `export` exit 1 on a store
    // whose only fault was having hit the earlier bug, and `export` is the sole
    // way to get the data back out, so the "safe" error was a one-way door.
    //
    // The two rejected alternatives, for the record. Repair-on-open (D23) works
    // when the correct value is *knowable* — a `default_project` naming no live
    // row can only be deleted — but nothing here knows whether `"Done"` meant
    // `done`; guessing would overwrite the user's bytes with no undo. Silently
    // coercing to `pending` is the original bug: open work invented from a row
    // we could not read, `completed` still set, nothing saying so.
    //
    // So: `status` gets a placeholder purely so the row keeps moving through
    // filters and sorts, and `status_raw` carries the fact. `Pending` is the
    // placeholder because it is the one value that keeps the row inside the
    // default `@working` view — an anomaly the user cannot see is the failure
    // shape this project keeps repeating, so the row must land where they are
    // already looking, wearing a label.
    let (status, status_raw) = match Status::parse(&status) {
        Some(s) => (s, None),
        None => (Status::Pending, Some(status)),
    };

    // `backlog --> pending: wait/schedule reached` is applied here, on the way
    // out of the store, and nowhere else. It is the one transition with no user
    // action behind it — a clock trips it — so there is no verb to hang it on,
    // and tasqx must work with no daemon running, which rules out a sweep being
    // the only mechanism. Deriving it at load makes the release unconditional
    // and instant: *every* task read in this codebase comes through here, so
    // `task.list`, `task.get`, the filters, reports, export, the scheduler and
    // the lifecycle guards all see one answer, and none of them can drift.
    //
    // The rejected alternative was writing the new status back during a read.
    // That buys a `status` column that is always current, at the price of a read
    // path that mutates: `list` would need a write transaction, would fail on a
    // read-only store or a read-only filesystem, and would contend with any
    // concurrent reader — a steep bill for a value we can recompute for free.
    // It also has to happen on read *anyway* to be correct between writes.
    //
    // The cost, stated plainly: for a released task the stored `status` still
    // reads `backlog` until some verb next writes the row. That column is
    // therefore a cache, not the truth, for backlog rows specifically — the same
    // bargain `urgency` already makes (persisted at write, recomputed on every
    // read because its inputs move on their own). Only raw SQL that filters on
    // the `status` text can be fooled by it, and both such queries are immune by
    // construction: `task.start`'s auto-stop sweep selects `active`, which this
    // rule never produces, and the reminder rebuild selects every open status,
    // which contains both sides of the edge.
    let scheduled: Option<String> = row.get(7)?;
    let wait: Option<String> = row.get(8)?;
    let status = effective_status(
        status,
        wait.as_deref(),
        scheduled.as_deref(),
        Timestamp::now(),
    );

    Ok(Task {
        id: row.get(0)?,
        short_id: row.get(1)?,
        title: row.get(2)?,
        status,
        status_raw,
        priority: priority.as_deref().and_then(Priority::parse),
        project: row.get(5)?,
        due: row.get(6)?,
        scheduled,
        wait,
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
    let mut stmt = conn.prepare("SELECT entity_id, payload FROM events WHERE op = 'reminded'")?;
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
///
/// `.optional()?`, not `.ok()`, for the same reason as [`get_config`]: only an
/// absent row is absence. `.ok()` folded every read fault — most reachably a
/// `tags.id` written as NULL or INTEGER by an external writer, which this
/// non-STRICT schema accepts — into the same `None` that means "first use of
/// this tag", so the function minted a fresh UUID and INSERTed. The operator
/// then saw `UNIQUE constraint failed: tags.name` on a column they never
/// touched, and the actual fault was never named.
pub fn ensure_tag_link(tx: &Transaction, task_id: &str, tag_name: &str) -> Result<(), ApiError> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM tags WHERE name = ?1",
            params![tag_name],
            |r| r.get(0),
        )
        .optional()?;
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
        let err = conn.execute("INSERT INTO dependencies VALUES ('b','nope')", []);
        assert!(
            err.is_err(),
            "a dangling edge must now be rejected by SQLite"
        );

        // Idempotent: a second migrate is a no-op, not another rebuild.
        migrate(&conn).unwrap();
        assert_eq!(fk_count(&conn), 2);
    }

    fn query_plan(conn: &Connection, sql: &str) -> String {
        conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// `events` is append-only and never pruned, so a query that filters it by
    /// `op` alone must reach an index rather than walk the whole log. The one
    /// that exists today is [`reminded_keys`], on the scheduler's hot path under
    /// the engine mutex; the schema used to index only `entity`/`entity_id`, so
    /// it scanned. The literal `reminded_keys` SQL is asserted, not a paraphrase,
    /// because the plan is a property of that exact statement.
    #[test]
    fn events_op_filter_uses_an_index() {
        let conn = open_in_memory().unwrap();
        let sql = "SELECT entity_id, payload FROM events WHERE op = 'reminded'";
        let plan = query_plan(&conn, sql);
        assert!(
            plan.contains("USING INDEX idx_events_op"),
            "the {sql:?} filter must reach idx_events_op, got: {plan}"
        );
    }

    /// Adding an `op` index must not *cost* the single-task lookup beside it.
    /// [`already_reminded`] filters `entity_id AND op`, and a bare `events(op)`
    /// index makes the planner abandon `idx_events_entity_id` for it — trading a
    /// seek on the handful of events for one task for a seek on every `reminded`
    /// event ever written, which is the unbounded set. Leading `op` with
    /// `entity_id` serves both: the rebuild seeks on the prefix, this one seeks
    /// on the whole key.
    #[test]
    fn the_op_index_does_not_widen_the_single_task_dedupe_check() {
        let conn = open_in_memory().unwrap();
        let plan = query_plan(
            &conn,
            "SELECT payload FROM events WHERE entity_id = 'x' AND op = 'reminded'",
        );
        assert!(
            plan.contains("entity_id=?"),
            "the dedupe check must still narrow by entity_id, got: {plan}"
        );
    }

    /// The index has to appear on a store created before it existed too — the
    /// CREATE lives in the batch that reruns on every open, so an upgrade is
    /// covered, but only as long as nobody moves it behind a version gate.
    #[test]
    fn migration_adds_events_op_index_to_a_legacy_store() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        // Legacy shape: the events table with only the two entity indices.
        conn.execute_batch(
            "CREATE TABLE events (id TEXT PRIMARY KEY, entity TEXT NOT NULL, \
               entity_id TEXT NOT NULL, op TEXT NOT NULL, payload TEXT, \
               ts TEXT NOT NULL, actor TEXT);
             CREATE INDEX idx_events_entity ON events(entity, entity_id);
             CREATE INDEX idx_events_entity_id ON events(entity_id);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='index' AND name='idx_events_op'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n, 1,
            "upgrading an existing store must create idx_events_op"
        );
    }

    /// A `tags` row this function cannot *read* is a store fault, not "no such
    /// tag". Folding the read error into `None` made the function mint a fresh
    /// UUID and INSERT, so the operator was handed a UNIQUE-constraint failure on
    /// a column they never touched while the actual fault went unmentioned.
    ///
    /// The store is not STRICT and `tags.id` is a non-INTEGER PRIMARY KEY, so
    /// SQLite accepts a NULL there — this is reachable on a healthy file that any
    /// external writer has touched, not only on a corrupt one.
    #[test]
    fn ensure_tag_link_reports_a_read_fault_instead_of_a_duplicate_tag() {
        let mut conn = open_in_memory().unwrap();
        conn.execute("INSERT INTO tags (id, name) VALUES (NULL, 'work')", [])
            .unwrap();

        let tx = conn.transaction().unwrap();
        let err = ensure_tag_link(&tx, "task-1", "work")
            .expect_err("an unreadable tag row must surface as an error");

        assert!(
            !err.message.contains("UNIQUE constraint"),
            "the fault is the unreadable id column, not a duplicate name: {}",
            err.message
        );
        assert!(
            err.message.contains("Invalid column type"),
            "the underlying rusqlite fault must reach the operator: {}",
            err.message
        );
    }

    /// The absent-tag path must keep working: `.optional()` maps only
    /// `QueryReturnedNoRows` to `None`, so a first-use tag is still created.
    #[test]
    fn ensure_tag_link_still_creates_a_missing_tag() {
        let mut conn = open_in_memory().unwrap();
        let tx = conn.transaction().unwrap();
        ensure_tag_link(&tx, "task-1", "work").unwrap();
        ensure_tag_link(&tx, "task-2", "work").unwrap();
        tx.commit().unwrap();

        let tags: Vec<String> = conn
            .prepare("SELECT name FROM tags")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(tags, vec!["work".to_string()], "the tag row is reused");
        assert_eq!(
            task_tags(&conn, "task-2").unwrap(),
            vec!["work".to_string()]
        );
    }

    /// A store created before token accounting has no `token_usage` table, and
    /// `CREATE TABLE IF NOT EXISTS` is the whole migration for a brand-new
    /// table. This seeds the legacy shape directly (the same recipe as the
    /// dependency-FK test above), proves the upgrade creates the table with a
    /// LIVE foreign key and its task index, and proves a second migrate is a
    /// no-op that keeps existing measurement rows.
    #[test]
    fn migration_creates_the_token_usage_table_on_a_legacy_store() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        // Hand-build a pre-token-accounting store: tasks exist, token_usage
        // does not.
        conn.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, short_id INTEGER NOT NULL UNIQUE, \
               title TEXT NOT NULL, status TEXT NOT NULL, priority TEXT, project TEXT, \
               due TEXT, scheduled TEXT, wait TEXT, estimate TEXT, recurrence TEXT, \
               urgency REAL NOT NULL DEFAULT 0, active_since TEXT, \
               tracked_seconds INTEGER NOT NULL DEFAULT 0, rev INTEGER NOT NULL DEFAULT 0, \
               created TEXT NOT NULL, modified TEXT NOT NULL, completed TEXT);
             INSERT INTO tasks (id, short_id, title, status, created, modified)
                VALUES ('a', 1, 'carrier', 'pending', 't', 't');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let table_count = |name: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(table_count("token_usage"), 1, "the table must exist");
        assert_eq!(table_count("idx_token_usage_task"), 1, "and its index");

        // The FOREIGN KEY is live, not merely declared: a measurement against
        // a task that is not there must be refused by SQLite itself.
        conn.execute(
            "INSERT INTO token_usage (id, task_id, tool, source, confidence, created) \
             VALUES ('m1', 'a', 'claude-code', 'self-report', 'medium', 't')",
            [],
        )
        .unwrap();
        let err = conn.execute(
            "INSERT INTO token_usage (id, task_id, tool, source, confidence, created) \
             VALUES ('m2', 'gone', 'claude-code', 'self-report', 'medium', 't')",
            [],
        );
        assert!(err.is_err(), "a dangling task_id must be rejected");

        // Idempotent, and it does not eat data: a second migrate keeps the row.
        migrate(&conn).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "a re-run migration must not touch measurements");
    }

    /// A store created before the OTLP receiver (#18) has no `otlp_samples`
    /// table; `CREATE TABLE IF NOT EXISTS` is the whole migration for it. Model a
    /// pre-#18 store as an otherwise-current one with exactly that table dropped,
    /// prove the upgrade re-creates the table + its session index, and prove a
    /// second migrate is a no-op that keeps buffered rows.
    #[test]
    fn migration_creates_the_otlp_samples_table_on_a_legacy_store() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        migrate(&conn).unwrap();

        let object_count = |name: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .unwrap()
        };

        // Simulate a pre-#18 store: drop exactly the table this migration adds
        // (dropping the table drops its index with it).
        conn.execute_batch("DROP TABLE otlp_samples;").unwrap();
        assert_eq!(
            object_count("otlp_samples"),
            0,
            "precondition: table absent"
        );

        // The upgrade re-creates the table and its session index.
        migrate(&conn).unwrap();
        assert_eq!(object_count("otlp_samples"), 1, "the table must exist");
        assert_eq!(
            object_count("idx_otlp_samples_session"),
            1,
            "and its session index"
        );

        // A buffered row (no task foreign key: raw telemetry, not attributed).
        conn.execute(
            "INSERT INTO otlp_samples (id, session_id, tool, ts, created) \
             VALUES ('s1', 'sess-x', 'claude_code', 't', 't')",
            [],
        )
        .unwrap();

        // Idempotent, and it does not eat data: a second migrate keeps the row.
        migrate(&conn).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM otlp_samples", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            rows, 1,
            "a re-run migration must not touch buffered samples"
        );
    }
}
