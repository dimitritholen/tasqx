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
    rev, created, modified, completed, remind, budget_tokens, delivered_annotation_id, \
    tracked_adjustment_seconds";

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
            remind          TEXT,
            -- D139: a size gauge over FRESH tokens (input + output + cache
            -- creation), never cache reads. NULL means no threshold, which is
            -- the default and deliberately not a number: a shipped default
            -- would be a guess about workloads tasqx has no data on, applied
            -- to every task in every store.
            budget_tokens   INTEGER,
            -- D165: the newest live annotation at the instant `task.done`
            -- completed the task — the card's Delivered row. NULL on an open
            -- task, and on one completed before the column existed.
            delivered_annotation_id TEXT,
            -- D166: the net of every `task.adjust_tracked` delta. Already
            -- folded into `tracked_seconds`; kept so a read can say how much
            -- of the total is correction rather than clock.
            tracked_adjustment_seconds INTEGER NOT NULL DEFAULT 0,
            -- D170: the task whose completion spawned this one (a recurrence),
            -- by uuid. NULL on everything else. Not in `TASK_COLS`: only
            -- `task.get`, `task.brief` and the export read it.
            spawned_from    TEXT
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

        -- D160: explicit links between any two nodes of the knowledge graph —
        -- tasks, memory docs, annotations and projects.
        --
        -- The endpoints are polymorphic (`<type>`, `<id>`) and so carry NO
        -- foreign key, unlike `dependencies` just above, where the FKs are what
        -- stops a dangling edge. The price is paid where the rows are written
        -- instead: `link.add` resolves both endpoints inside its own write
        -- transaction, and `memory.remove` — the one hard delete of a node this
        -- engine has — deletes the links naming that doc in the same
        -- transaction. An annotation removal is a tombstone (D113), so the node
        -- still exists and its links are left alone.
        --
        -- `relation` is a string, not a CHECK constraint, although D160 fixes
        -- the registry today: the validation lives in the engine, where a
        -- refusal can list the accepted set, and a sixth relation must not cost
        -- a schema migration on every store.
        --
        -- `metadata` is the caller's own JSON object, stored verbatim and never
        -- interpreted — `checks.evidence`'s rule one table up.
        --
        -- The UNIQUE key carries `relation`, so "A references B" and "A
        -- contradicts B" coexist while a repeat of either is idempotent.
        CREATE TABLE IF NOT EXISTS links (
            id         TEXT PRIMARY KEY,
            from_type  TEXT NOT NULL,
            from_id    TEXT NOT NULL,
            to_type    TEXT NOT NULL,
            to_id      TEXT NOT NULL,
            relation   TEXT NOT NULL,
            metadata   TEXT,
            created    TEXT NOT NULL,
            created_by TEXT NOT NULL DEFAULT 'user',
            UNIQUE (from_type, from_id, to_type, to_id, relation)
        );
        -- `link.list {ref}` and every graph expansion read a node's edges from
        -- both ends, and the UNIQUE index above is from-leading, so it cannot
        -- serve the `to` half.
        CREATE INDEX IF NOT EXISTS idx_links_from ON links(from_type, from_id);
        CREATE INDEX IF NOT EXISTS idx_links_to   ON links(to_type, to_id);

        -- D138: acceptance criteria with a state and a citation.
        --
        -- Its own table rather than a convention inside an annotation body: a
        -- check has a STATE MACHINE, and state in prose is unqueryable,
        -- unfilterable, silently broken by an edit, and impossible to carry
        -- across an export without re-parsing free text. D135 is the recent
        -- lesson on what it costs when an index has to read structure out of a
        -- body.
        --
        -- `evidence` is the caller's proof — a test name, a command's output,
        -- a commit sha — stored verbatim and NEVER interpreted. tasqx executes
        -- nothing: §1 holds the core to local-first and offline, and shelling
        -- out to run a caller-supplied string is a larger breach of that than a
        -- network call, since the string arrives over MCP from a model.
        --
        -- `position` and not `created`: criteria are written as a sequence, and
        -- two added in the same second would otherwise sort arbitrarily.
        CREATE TABLE IF NOT EXISTS checks (
            id       TEXT PRIMARY KEY,
            task_id  TEXT NOT NULL REFERENCES tasks(id),
            body     TEXT NOT NULL,
            state    TEXT NOT NULL,
            evidence TEXT,
            position INTEGER NOT NULL,
            created  TEXT NOT NULL,
            modified TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_checks_task ON checks(task_id, position);

        CREATE TABLE IF NOT EXISTS annotations (
            id      TEXT PRIMARY KEY,
            task_id TEXT NOT NULL,
            body    TEXT NOT NULL,
            created TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_annotations_task ON annotations(task_id);

        -- AI token measurements, many per task (DESIGN.md §10).
        -- Four separate counts by design — cache tokens cost a fraction of fresh
        -- ones, so a blended total destroys what a cost report needs. `tool` is
        -- free-form (new agents appear faster than releases); `source` and
        -- `confidence` are the closed vocabularies in `crate::tokens`, enforced
        -- at every write door. `extra` is reserved for per-tool oddities the
        -- later parser phases may need to carry (nothing writes it yet).
        -- `total_tokens` (D167) is a fifth, separate kind: one unsplit count
        -- from a reporter that cannot split it, never folded into the four.
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
            created               TEXT NOT NULL,
            total_tokens          INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_token_usage_task ON token_usage(task_id);

        -- Raw per-request OTLP samples buffered by the opt-in local receiver
        -- (#18, DESIGN.md §10). Deliberately NOT joined to a
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
    add_column_if_missing(conn, "tasks", "budget_tokens", "INTEGER")?;
    add_column_if_missing(conn, "tasks", "delivered_annotation_id", "TEXT")?;
    add_column_if_missing(
        conn,
        "tasks",
        "tracked_adjustment_seconds",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "tasks", "spawned_from", "TEXT")?;

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

    // D113: `annotation.remove` tombstones a row rather than deleting it — the
    // removal event, the id and the timestamp stay, only `body` is overwritten.
    // `NULL` means "never removed"; every reader that lists annotations filters
    // on it. Additive column, same upgrade path as `remind` above.
    add_column_if_missing(conn, "annotations", "removed", "TEXT")?;
    // D167: the unsplit count. 0 on every existing row, which is what those
    // rows mean — each was reported split.
    add_column_if_missing(
        conn,
        "token_usage",
        "total_tokens",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    normalize_stored_tags(conn)?;
    Ok(())
}

/// D41 memory subsystem: the `docs` table plus FTS5 indexes over `docs` and
/// `annotations`, kept in sync by triggers so no writer can forget the index.
///
/// The rebuild step matters for upgrades: a store that predates this migration
/// already holds annotation rows the brand-new index has never seen, and
/// external-content FTS5 reads its content table only when told to. Rebuilding
/// unconditionally would rescan every body on every open, so it runs only when
/// this call actually created the index — or, since #128, actually changed the
/// tokenizer.
///
/// Task #12/D135: `docs_fts`'s text column, still named `body`, reads
/// `docs.search_body` through the `docs_search` view rather than `docs.body`
/// itself — a derived, frontmatter-flattened copy ([`crate::frontmatter::flatten`])
/// that `memory.add`/`memory.update`/`memory.import`/`store.import` all keep
/// in step with `body` at write time. `body` stays exactly what was written
/// (`memory.get`, `memory show` and `store.export` all read it unchanged; a
/// `store.export`/`store.import` round-trip stays byte-for-byte), while the
/// INDEX reads text with a leading fence's `---` delimiters and raw
/// `key: value` lines turned into D121(e)'s `key  value` prose, so
/// `memory.search`'s FTS5 `snippet()` — which, being external-content, reads
/// its named column straight from the content table — never surfaces either.
/// A store written before this existed can hold a `docs.search_body` that is
/// stale or, on first open under this code, entirely absent; see the
/// backfill below.
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

    // #128: stemming, via SQLite FTS5's built-in porter tokenizer layered on
    // the default unicode61 one, so "review"/"reviewing"/"reviewed" match
    // each other without needing `raw:true`. A virtual table's module
    // arguments (its `tokenize=` clause) cannot be ALTERed — the only route
    // is dropping and recreating it, the same move
    // `add_dependency_foreign_keys_if_missing` makes for a constraint SQLite
    // has no ALTER for either. Checked against `sqlite_master.sql` rather
    // than assumed, so a store already on porter (including every fresh one
    // created by the CREATE below) is left alone.
    let docs_fts_sql: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='docs_fts'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let annotations_fts_sql: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='annotations_fts'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let docs_needs_porter = docs_fts_sql
        .as_deref()
        .is_some_and(|sql| !sql.contains("porter"));
    // D135: a store whose `docs_fts` still reads `docs` directly predates
    // the derived-column fix — it must be recreated onto the `docs_search`
    // view exactly as a tokenizer change forces a recreate, since a virtual
    // table's `content=` is no more ALTERable than its `tokenize=`. Keyed on
    // the content table, not a column name: the text column stays `body`
    // either way, because raw `body:` filters are part of the API (D41).
    let docs_needs_search_view = docs_fts_sql
        .as_deref()
        .is_some_and(|sql| sql.contains("content='docs'"));
    let docs_needs_recreate = docs_needs_porter || docs_needs_search_view;
    let annotations_needs_porter = annotations_fts_sql
        .as_deref()
        .is_some_and(|sql| !sql.contains("porter"));
    if docs_needs_recreate {
        // The TABLE and its TRIGGERs both name the old column: `DROP TABLE`
        // alone would leave `docs_fts_ai`/`ad`/`au` referencing a dropped
        // table (the next write to `docs` would fail with "no such table:
        // docs_fts"), and `CREATE TRIGGER IF NOT EXISTS` below would not
        // replace a same-named trigger that already exists, old body and
        // all — so the triggers must go too, unconditionally, not
        // conditionally on porter alone as before D135.
        tx.execute_batch(
            "DROP TABLE docs_fts; \
             DROP TRIGGER IF EXISTS docs_fts_ai; \
             DROP TRIGGER IF EXISTS docs_fts_ad; \
             DROP TRIGGER IF EXISTS docs_fts_au;",
        )?;
    }
    if annotations_needs_porter {
        tx.execute_batch("DROP TABLE annotations_fts;")?;
    }

    // The `docs` TABLE first, alone — deliberately split from the
    // `docs_fts`/trigger DDL below. `docs.search_body` must already hold
    // correct values (the backfill just after this) before ANY trigger that
    // reads `new.search_body`/`old.search_body` exists, or the very first
    // write through such a trigger fires a 'delete' for a rowid the
    // freshly-(re)created, still-empty `docs_fts` never held — confirmed as
    // an FTS5 "database disk image is malformed" error, not a graceful
    // no-op. Doing the column ALTERs and the backfill UPDATE here, before
    // `docs_fts`/its triggers exist at all in this transaction, means that
    // UPDATE fires no trigger and raises no such question.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS docs (
            id       TEXT PRIMARY KEY,
            source   TEXT,
            title    TEXT NOT NULL,
            body     TEXT NOT NULL,
            created  TEXT NOT NULL,
            modified TEXT NOT NULL
        );",
    )?;

    // #134: optional project scoping, additive and nullable — a doc with no
    // project stays global rather than being forced onto a default.
    // #135: `rev` for `memory.update`'s optimistic-concurrency guard, the
    // exact shape `task.modify`'s `expected_rev` already uses. Both are
    // additive columns, so existing rows read back as unscoped/rev-0 rather
    // than breaking.
    add_column_if_missing(&tx, "docs", "project", "TEXT")?;
    add_column_if_missing(&tx, "docs", "rev", "INTEGER NOT NULL DEFAULT 0")?;
    // D135: `search_body` itself — additive, default `''`, same shape as
    // `project`/`rev` above.
    add_column_if_missing(&tx, "docs", "search_body", "TEXT NOT NULL DEFAULT ''")?;
    // #101/D156: `standing` — a doc that belongs in every session of its
    // project (or of every project, when it has none) until it is retracted.
    // Additive and defaulted to 0, like the three above, so every doc on a
    // store written before this reads back as ordinary memory rather than
    // being promoted by a migration nobody asked for.
    add_column_if_missing(&tx, "docs", "standing", "INTEGER NOT NULL DEFAULT 0")?;

    // Any row still at that default needs backfilling: every row on a store
    // that just got the column for the first time (the common case, once
    // ever, per store), but ALSO a row written directly against `docs` by
    // something that never set `search_body` at all — an old binary's own
    // `INSERT`, run against a store a newer build had already upgraded —
    // which is a real mixed-version case, not a hypothetical one, so this
    // runs every open rather than gating on "did this open just add the
    // column". Cheap on a healthy store: an equality filter over a column
    // that is never legitimately `''` (a doc's `body` is never empty),
    // costing a scan of `search_body` alone, not of every `body`.
    let candidates: Vec<(String, String)> = tx
        .prepare("SELECT id, body FROM docs WHERE search_body = ''")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, body) in candidates {
        let search_body = crate::frontmatter::flatten(&body).into_owned();
        tx.execute(
            "UPDATE docs SET search_body = ?1 WHERE id = ?2",
            params![search_body, id],
        )?;
    }

    tx.execute_batch(
        r#"
        -- External-content FTS: the index stores no second copy of the text;
        -- rows are joined back by rowid. Triggers are the only writers.
        -- `tokenize='porter unicode61'` (#128) stems each term through the
        -- unicode61 tokenizer before matching, so a plain AND-of-terms query
        -- still requires every word — just its STEM, not its exact spelling.
        -- D135: the index's text column keeps the name `body` (raw `body:`
        -- column filters are part of the API, D41) but reads
        -- `docs.search_body` — the derived, frontmatter-flattened text —
        -- through the `docs_search` view. External-content FTS5 resolves a
        -- content column by NAME, so the view's `search_body AS body` is
        -- what makes `snippet()` read the flattened copy while `docs.body`
        -- itself stays exactly what was written. By the time this table
        -- (and the triggers below) exist, `docs.search_body` is already
        -- correct for every row (see the backfill above) — the ordering
        -- that keeps a trigger's own 'delete' honest.
        CREATE VIEW IF NOT EXISTS docs_search AS
            SELECT rowid, title, search_body AS body FROM docs;
        CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
            title, body, content='docs_search', content_rowid='rowid',
            tokenize='porter unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS docs_fts_ai AFTER INSERT ON docs BEGIN
            INSERT INTO docs_fts(rowid, title, body)
                VALUES (new.rowid, new.title, new.search_body);
        END;
        CREATE TRIGGER IF NOT EXISTS docs_fts_ad AFTER DELETE ON docs BEGIN
            INSERT INTO docs_fts(docs_fts, rowid, title, body)
                VALUES ('delete', old.rowid, old.title, old.search_body);
        END;
        CREATE TRIGGER IF NOT EXISTS docs_fts_au AFTER UPDATE ON docs BEGIN
            INSERT INTO docs_fts(docs_fts, rowid, title, body)
                VALUES ('delete', old.rowid, old.title, old.search_body);
            INSERT INTO docs_fts(rowid, title, body)
                VALUES (new.rowid, new.title, new.search_body);
        END;

        CREATE VIRTUAL TABLE IF NOT EXISTS annotations_fts USING fts5(
            body, content='annotations', content_rowid='rowid', tokenize='porter unicode61'
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

    if !fts_existed || docs_needs_recreate {
        tx.execute_batch("INSERT INTO docs_fts(docs_fts) VALUES('rebuild');")?;
    }
    if !fts_existed || annotations_needs_porter {
        tx.execute_batch("INSERT INTO annotations_fts(annotations_fts) VALUES('rebuild');")?;
    }

    // D174: `source` is a doc's identity, held by at most one row. A store
    // written before this may already have two on one source, so they are
    // resolved first — once, gated on the index being absent — without
    // deleting anything: the most recently modified row keeps the source and
    // the older ones keep their title and body with the source cleared. An
    // empty source is no identity, exactly like NULL: left out of both the
    // resolution and the index, and left stored as it is (#81 owns that).
    // `rtrim(modified, 'Z')` is `memory.list`'s own normalisation of a
    // trimmed-fraction stamp (D142's trap), `id` the tie-break. The UPDATE
    // runs after `docs_fts` and its triggers exist, so `docs_fts_au`
    // re-indexes each touched row with the same text it already had.
    let has_source_index: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master \
         WHERE type = 'index' AND name = 'idx_docs_source')",
        [],
        |r| r.get(0),
    )?;
    if !has_source_index {
        tx.execute_batch(
            "UPDATE docs SET source = NULL \
             WHERE source IS NOT NULL AND source <> '' AND id <> ( \
                 SELECT d.id FROM docs d WHERE d.source = docs.source \
                 ORDER BY rtrim(d.modified, 'Z') DESC, d.id DESC LIMIT 1); \
             CREATE UNIQUE INDEX idx_docs_source ON docs(source) \
                 WHERE source IS NOT NULL AND source <> '';",
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
/// `ADD COLUMN IF NOT EXISTS`, so the column list is checked first. Returns
/// whether it actually added the column, so a caller that needs to backfill
/// the new column exactly once (D135's `docs.search_body`) can gate on that
/// instead of re-deriving "is this a fresh store" some other way.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    col: &str,
    decl: &str,
) -> Result<bool, ApiError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let present = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|c| c == col);
    if !present {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"))?;
    }
    Ok(!present)
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
/// `entity` is the typed [`Entity`], not a `&str`, so the only spellings the
/// column may ever hold are the enum's four variants — `task`, `project`,
/// `doc` (D41) and `link` (D160) — rather than a literal hand-typed at each call site. That is what
/// lets `event.list` state its accepted set from [`Entity::ALL`] instead of
/// keeping a second list in sync with these writers.
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
            crate::clock::uuid_v7().to_string(),
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

/// The lowest `events.id` that can sort at or below any event written at or
/// after `ts` — the bound `event.list {from}` filters on (D59).
///
/// **The `ts` column cannot do this job, and that is why this exists.** `ts` is
/// `TEXT` with no `COLLATE`, written by [`crate::util::now`] as
/// `Timestamp::to_string()`, and jiff prints a *variable-length* fractional
/// second — omitted entirely when it is zero. Under SQLite's BINARY collation
/// `'.'` (0x2E) sorts below `'Z'` (0x5A), so:
///
/// ```text
/// sqlite> SELECT '2026-07-15T11:06:10.5Z' >= '2026-07-15T11:06:10Z';
/// 0
/// ```
///
/// A `WHERE ts >= ?` bound therefore *drops* every event in the boundary second
/// that carries a fraction — and the caller this parameter was built for passes
/// a midnight instant, so that is not a rare edge but every event on the first
/// day of the window. An index on `ts` would only make the wrong answer fast.
/// `id` is UUIDv7, time-ordered, and already the PRIMARY KEY, so the range is
/// index-served with no second index to write on every insert.
///
/// **The one-second margin is not slack, it is the contract.** [`insert_event`]
/// reads the clock *twice* — `crate::clock::uuid_v7()` for `id`, then `now()`
/// for `ts`, with a `payload.to_string()` between them — so a row whose write
/// straddles a millisecond tick has `ts` one millisecond ahead of the instant
/// inside its own `id`. Flooring exactly would then exclude a row whose `ts` is
/// inside the window: an under-inclusion, the direction that loses data. A
/// second of margin swamps that gap by six orders of magnitude, and
/// over-inclusion is free because every consumer buckets by `ts` anyway.
///
/// The margin is sized for that tick and nothing larger, which is why both
/// reads must come off the same clock. They did not, once: `id` was minted by
/// `Uuid::now_v7()` from the wall clock while `ts` followed `TASQX_NOW`, so a
/// pin ahead of today put every freshly written `id` a pin's distance BELOW a
/// floor derived from its own `ts`, and a bounded `event.list` dropped rows the
/// same command had just written (D149).
///
/// So `from` is a **lower bound, not a filter**: it promises no events older
/// than roughly that instant, not exactly the events at or after it. Callers
/// that need exactness do not exist, and the store cannot offer it from two
/// clock reads.
///
/// The low bytes are all zero deliberately — any non-zero counter or random
/// suffix would sort above part of the boundary millisecond and drop it.
pub fn event_id_floor(ts: Timestamp) -> String {
    let millis = ts
        .as_millisecond()
        .saturating_sub(1_000)
        .max(0)
        .unsigned_abs();
    uuid::Builder::from_unix_timestamp_millis(millis, &[0u8; 10])
        .into_uuid()
        .to_string()
}

/// Map a `SELECT {TASK_COLS}` row into a `Task`, at the caller's clock.
///
/// A SINGLE-row read may use the [`map_task_row`] wrapper; a bulk reader must
/// pass one `now` for the whole statement, or two rows straddling a
/// wait/schedule boundary within one `task.list` are classified against
/// different clocks — the exact skew filter.rs forbids for `tomorrow`
/// ("resolve now ONCE, when the filter is built").
pub fn map_task_row_at(row: &Row, now: Timestamp) -> rusqlite::Result<Task> {
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
    // the `status` text can be fooled by it, and all five such filters are immune
    // by construction rather than by luck: `task.start`'s auto-stop sweep selects
    // `active`, which this rule never produces, and the other four — the reminder
    // rebuild, `compute_unblocked`, the remaining-blocker count, and the blocked
    // predicate `unmet_blocker_source` that `is_blocked`, `unmet_blockers` and
    // the snapshot loader's blocked set all read — take a WHOLE open or terminal
    // set from `Status::sql_in_list`, and both sides of this edge are open while
    // neither is terminal, so no set can hold one and not the other. The query
    // that would break the bargain is a new one naming `backlog` or `pending`
    // on its own.
    let scheduled: Option<String> = row.get(7)?;
    let wait: Option<String> = row.get(8)?;
    let status = effective_status(status, wait.as_deref(), scheduled.as_deref(), now);

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
        budget_tokens: row.get(19)?,
        delivered_annotation_id: row.get(20)?,
        tracked_adjustment_seconds: row.get(21)?,
    })
}

/// [`map_task_row_at`] at the current clock — for SINGLE-row reads only,
/// where "one now per statement" is one call by construction.
pub fn map_task_row(row: &Row) -> rusqlite::Result<Task> {
    map_task_row_at(row, crate::clock::now())
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

/// D172: the one spelling a tag is stored under — lowercased, Unicode-aware, so
/// `Perf` and `perf` are one tag. Whitespace is refused rather than rewritten:
/// a tag is one word on the command line and in the filter DSL, and guessing
/// between `has-space` and `has_space` for the caller would be a silent rename.
pub fn normalize_tag(name: &str) -> Result<String, ApiError> {
    if name.chars().any(char::is_whitespace) {
        return Err(ApiError::bad_request(format!(
            "tag {name:?} contains whitespace — a tag is one word; write it as {:?}",
            hyphenated(name)
        )));
    }
    Ok(name.to_lowercase())
}

/// [`normalize_tag`] over a list, duplicates collapsed in the order given, so
/// `["Perf", "perf"]` is one tag in the stored set and in the event payload.
pub fn normalize_tags(names: Vec<String>) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let tag = normalize_tag(&name)?;
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    Ok(out)
}

/// The lowercased name with each run of whitespace turned into one hyphen —
/// the migration's rewrite of a legacy spaced tag, and the spelling a refusal
/// suggests.
fn hyphenated(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase()
}

/// D172: fold the tags a store written before normalisation can hold — `UPPER`
/// beside `upper`, `has space` — into the stored spelling: lowercased, each
/// whitespace run a hyphen. A name that lands on an existing tag is MERGED: its
/// links are re-pointed at the survivor (a task carrying both keeps one link)
/// and its row is deleted, so no task loses a tag.
///
/// Unlike the silent repairs beside it, this one changes what a user sees on a
/// task, so each task it touched gets one `tag.normalize` event naming every
/// `{from, to}` it applied — `event.list {ref}` is the record of what the
/// migration did. It runs on every open; a store with nothing to fold reads
/// the tag table once and writes nothing.
fn normalize_stored_tags(conn: &Connection) -> Result<(), ApiError> {
    if stale_tags(conn)?.is_empty() {
        return Ok(());
    }
    // IMMEDIATE, and the list re-read under the lock: a daemon and a one-shot
    // opening the same old store at once must not both apply (and log) it.
    let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let stale = stale_tags(&tx)?;
    let mut changed: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
        std::collections::BTreeMap::new();
    for (id, from, to) in &stale {
        let tasks: Vec<String> = tx
            .prepare("SELECT task_id FROM task_tags WHERE tag_id = ?1")?
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let survivor: Option<String> = tx
            .query_row("SELECT id FROM tags WHERE name = ?1", params![to], |r| {
                r.get(0)
            })
            .optional()?;
        match survivor {
            Some(keep) => {
                tx.execute(
                    "INSERT OR IGNORE INTO task_tags (task_id, tag_id) \
                     SELECT task_id, ?2 FROM task_tags WHERE tag_id = ?1",
                    params![id, keep],
                )?;
                tx.execute("DELETE FROM task_tags WHERE tag_id = ?1", params![id])?;
                tx.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
            }
            None => {
                tx.execute("UPDATE tags SET name = ?1 WHERE id = ?2", params![to, id])?;
            }
        }
        for task in tasks {
            changed
                .entry(task)
                .or_default()
                .push(serde_json::json!({ "from": from, "to": to }));
        }
    }
    for (task, tags) in changed {
        tx.execute(
            "UPDATE tasks SET rev = rev + 1 WHERE id = ?1",
            params![task],
        )?;
        insert_event(
            &tx,
            Entity::Task,
            &task,
            "tag.normalize",
            &serde_json::json!({ "tags": tags }),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Every stored tag whose name is not in the D172 form, as `(id, from, to)`.
///
/// `typeof` filters: an external writer can leave a NULL or INTEGER in either
/// column of this non-STRICT table, and a migration that errored on such a row
/// would refuse to open the store at all. A name that is nothing but
/// whitespace has no form to fold into and is left alone.
fn stale_tags(conn: &Connection) -> Result<Vec<(String, String, String)>, ApiError> {
    let rows = conn
        .prepare(
            "SELECT id, name FROM tags \
             WHERE typeof(id) = 'text' AND typeof(name) = 'text' ORDER BY name",
        )?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| {
            let to = hyphenated(&name);
            (id, name, to)
        })
        .filter(|(_, name, to)| name != to && !to.is_empty())
        .collect())
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
///
/// The name is run through [`normalize_tag`] first (D172), so every door that
/// attaches a tag stores the same spelling whether or not it normalised first.
pub fn ensure_tag_link(tx: &Transaction, task_id: &str, tag_name: &str) -> Result<(), ApiError> {
    let tag_name = &normalize_tag(tag_name)?;
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
            let id = crate::clock::uuid_v7().to_string();
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

    /// The reason `event_id_floor` exists at all, stated as an executable fact
    /// rather than a comment: the `ts` column's text order is **not** its time
    /// order, so the obvious `WHERE ts >= ?` bound is wrong.
    ///
    /// jiff omits the fractional second when it is zero, and `'.'` sorts below
    /// `'Z'`, so a bound written as a whole second excludes every event in that
    /// second that carries a fraction. The caller `from` was built for passes
    /// exactly such a bound (midnight), which makes this every event on the
    /// first day of the window rather than an unlucky few.
    #[test]
    fn a_ts_text_bound_would_drop_the_fractional_seconds_an_id_bound_keeps() {
        let conn = open_in_memory().unwrap();
        let bare = "2026-07-15T11:06:10Z";
        for frac in ["2026-07-15T11:06:10.5Z", "2026-07-15T11:06:10.123456Z"] {
            let ge: bool = conn
                .query_row("SELECT ?1 >= ?2", params![frac, bare], |r| r.get(0))
                .unwrap();
            assert!(
                !ge,
                "{frac:?} >= {bare:?} is expected to be FALSE under BINARY collation — \
                 if this ever becomes true the `ts` column grew a fixed width or a \
                 collation, and `event_id_floor`'s whole reason to exist should be \
                 re-read before it is kept"
            );
        }
    }

    /// The `from` bound must be *served by* the primary key, not merely correct
    /// against it — that is the whole reason D59 chose an id range over a second
    /// index. The unscoped arm is the one that matters: it is what the chart
    /// callers use, over a log that is append-only and never pruned.
    ///
    /// Only the unscoped plan is asserted. The scoped arms legitimately prefer
    /// their own scope index and take `id >=` as a residual, and demanding the
    /// PK there would pin a planner choice that is not this decision's business.
    #[test]
    fn the_event_from_bound_seeks_the_primary_key_rather_than_scanning() {
        let conn = open_in_memory().unwrap();
        // Literals rather than `?1`/`?2`: the `query_plan` helper binds nothing,
        // and the plan is a property of the statement's shape, which literals
        // preserve.
        let sql = "SELECT id, entity, entity_id, op, payload, ts, actor FROM events \
                   WHERE id >= '019f0000-0000-7000-8000-000000000000' \
                   ORDER BY id DESC LIMIT 50";
        let plan = query_plan(&conn, sql);
        assert!(
            !plan.contains("SCAN events"),
            "the bounded read must not walk the whole log, got: {plan}"
        );
        assert!(
            plan.contains("sqlite_autoindex_events_1"),
            "the bound must be served by the `id` primary key — if this ever fails \
             because someone reached for `WHERE ts >= ?` and an idx_events_ts, read \
             `event_id_floor` first: that column's text order is not its time order. \
             Got: {plan}"
        );
    }

    /// The floor sorts below every id that instant can mint, which is the
    /// property the bound depends on.
    ///
    /// A fixed instant and a constructed id, not `clock::now()` and a real
    /// mint: this is a property of the derivation, and a test that reads a
    /// clock at all can only be as stable as the environment it runs in — with
    /// `TASQX_NOW` set it would compare a pinned floor against a pinned mint
    /// and prove nothing it did not already assume. `from_unix_timestamp_millis`
    /// with zero bytes is the SMALLEST id that millisecond can produce, so
    /// beating it beats every real one. That a real write lands inside its own
    /// window is asserted end to end instead, in the CLI's
    /// `an_event_written_under_a_future_pin_is_inside_its_own_window`.
    #[test]
    fn the_event_id_floor_sorts_below_an_id_minted_at_that_instant() {
        let ms: u64 = 1_800_000_000_000;
        let floor = event_id_floor(Timestamp::from_millisecond(ms as i64).unwrap());
        let smallest = uuid::Builder::from_unix_timestamp_millis(ms, &[0u8; 10])
            .into_uuid()
            .to_string();
        assert!(
            floor < smallest,
            "floor {floor} must sort below the smallest id that instant can mint \
             ({smallest})"
        );
        assert!(
            floor.ends_with("-8000-000000000000"),
            "the low bytes must be zero or the boundary millisecond is partly \
             excluded, got {floor}"
        );
    }

    /// The margin is deliberate: `insert_event` reads the clock twice, so an
    /// id can trail its own row's `ts` across a millisecond tick. Flooring
    /// exactly would exclude that row.
    #[test]
    fn the_event_id_floor_leaves_a_margin_below_the_instant_it_is_given() {
        let ts = Timestamp::from_millisecond(1_800_000_000_000).unwrap();
        let floor = event_id_floor(ts);
        let exact = uuid::Builder::from_unix_timestamp_millis(1_800_000_000_000, &[0u8; 10])
            .into_uuid()
            .to_string();
        assert!(
            floor < exact,
            "the floor {floor} must sort strictly below an exact floor {exact} — \
             without the margin a row whose id trails its ts across a tick is dropped"
        );
    }

    /// The pre-epoch guard: a `from` before 1970 must not wrap the unsigned
    /// millisecond conversion into an enormous timestamp, which would floor
    /// *above* every real event and return an empty page at `ok: true`.
    #[test]
    fn an_instant_before_the_epoch_floors_to_the_bottom_rather_than_wrapping() {
        let ts = Timestamp::from_millisecond(-10_000).unwrap();
        let floor = event_id_floor(ts);
        // The lowest id any clock can mint: the epoch itself, with zero bytes.
        // Constructed rather than read from the clock, for the reason above.
        let lowest = uuid::Builder::from_unix_timestamp_millis(0, &[0u8; 10])
            .into_uuid()
            .to_string();
        assert!(
            floor <= lowest,
            "a pre-epoch bound must still sort below every real event, got {floor}"
        );
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

    /// #128: a store built before the porter tokenizer landed has `docs_fts`
    /// and `annotations_fts` without `tokenize='porter unicode61'` — opening
    /// it must drop + recreate both indexes onto the new tokenizer AND
    /// rebuild them from the content tables that are the only thing
    /// migrated, since a virtual table's module arguments cannot be ALTERed.
    /// Simulated by hand-building the pre-#128 shape (the exact DDL this
    /// migration replaced) and inserting a row through it directly — the
    /// migration path this exercises runs on a real upgrade, where the row
    /// already exists before the binary that knows about porter ever opens
    /// the file.
    #[test]
    fn migration_adds_the_porter_tokenizer_to_a_legacy_fts5_index_and_rebuilds_it() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        // The pre-#128 memory schema: no `tokenize=`, so unicode61's default
        // exact-token matching is all a query gets.
        conn.execute_batch(
            r#"
            CREATE TABLE docs (
                id       TEXT PRIMARY KEY,
                source   TEXT,
                title    TEXT NOT NULL,
                body     TEXT NOT NULL,
                created  TEXT NOT NULL,
                modified TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE docs_fts USING fts5(
                title, body, content='docs', content_rowid='rowid'
            );
            CREATE TRIGGER docs_fts_ai AFTER INSERT ON docs BEGIN
                INSERT INTO docs_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
            END;
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY, short_id INTEGER NOT NULL UNIQUE, title TEXT NOT NULL,
                status TEXT NOT NULL, priority TEXT, project TEXT, due TEXT, scheduled TEXT,
                wait TEXT, estimate TEXT, recurrence TEXT, urgency REAL NOT NULL DEFAULT 0,
                active_since TEXT, tracked_seconds INTEGER NOT NULL DEFAULT 0,
                rev INTEGER NOT NULL DEFAULT 0, created TEXT NOT NULL, modified TEXT NOT NULL,
                completed TEXT
            );
            CREATE TABLE annotations (
                id TEXT PRIMARY KEY, task_id TEXT NOT NULL, body TEXT NOT NULL, created TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE annotations_fts USING fts5(
                body, content='annotations', content_rowid='rowid'
            );
            CREATE TRIGGER annotations_fts_ai AFTER INSERT ON annotations BEGIN
                INSERT INTO annotations_fts(rowid, body) VALUES (new.rowid, new.body);
            END;
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO docs (id, source, title, body, created, modified) \
             VALUES ('d1', NULL, 'PR checklist', 'review the diff before merging', 't', 't')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='docs_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sql.contains("porter"),
            "docs_fts must be recreated onto the porter tokenizer: {sql}"
        );
        let ann_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='annotations_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            ann_sql.contains("porter"),
            "annotations_fts must be recreated onto the porter tokenizer: {ann_sql}"
        );

        // The pre-existing row must still be searchable at all (the rebuild
        // actually ran), and now by STEM, not just exact spelling.
        let stemmed_hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM docs_fts WHERE docs_fts MATCH 'reviewing'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stemmed_hit, 1,
            "a doc migrated from a pre-porter index must be found by a stemmed query"
        );

        // Re-running the migration on an already-porter store must be a no-op
        // that does not choke on its own idempotence guard.
        migrate(&conn).unwrap();
        let sql_again: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='docs_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sql, sql_again,
            "a second migrate() must not touch a store already on porter"
        );
    }

    /// Task #12/D135: a store written by code that predates `docs.search_body`
    /// entirely has none — `docs_fts`'s content column no longer even
    /// exists there. `migrate()` must add it and backfill it from `body` so
    /// `memory.search`'s FTS5 `snippet()` (external-content, so it reads
    /// `docs.search_body` straight from the row) stops leaking a leading
    /// fence's `---` and raw `key: value` lines — WITHOUT ever touching
    /// `docs.body` itself, unlike this fix's first cut (review finding):
    /// `memory.get`/`memory show`/`store.export` must all still see the doc
    /// exactly as written.
    ///
    /// Simulated the same way the porter-tokenizer migration above is: the
    /// pre-migration schema (no `search_body` column, `docs_fts` still
    /// declared over `body`) is hand-built and a row inserted directly,
    /// exactly as an old binary's write would have landed before this column
    /// existed.
    #[test]
    fn migration_backfills_search_body_and_leaves_the_stored_body_alone() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE docs (
                id       TEXT PRIMARY KEY,
                source   TEXT,
                title    TEXT NOT NULL,
                body     TEXT NOT NULL,
                created  TEXT NOT NULL,
                modified TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE docs_fts USING fts5(
                title, body, content='docs', content_rowid='rowid', tokenize='porter unicode61'
            );
            CREATE TRIGGER docs_fts_ai AFTER INSERT ON docs BEGIN
                INSERT INTO docs_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
            END;
            CREATE TRIGGER docs_fts_ad AFTER DELETE ON docs BEGIN
                INSERT INTO docs_fts(docs_fts, rowid, title, body)
                    VALUES ('delete', old.rowid, old.title, old.body);
            END;
            CREATE TRIGGER docs_fts_au AFTER UPDATE ON docs BEGIN
                INSERT INTO docs_fts(docs_fts, rowid, title, body)
                    VALUES ('delete', old.rowid, old.title, old.body);
                INSERT INTO docs_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
            END;
            CREATE TABLE annotations (
                id TEXT PRIMARY KEY, task_id TEXT NOT NULL, body TEXT NOT NULL, created TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE annotations_fts USING fts5(
                body, content='annotations', content_rowid='rowid', tokenize='porter unicode61'
            );
            "#,
        )
        .unwrap();
        let raw_body = "---\ndescription: How an SDK release is cut\n---\nCut the branch.";
        conn.execute(
            "INSERT INTO docs (id, source, title, body, created, modified) \
             VALUES ('d1', NULL, 'release-process', ?1, 't', 't')",
            params![raw_body],
        )
        .unwrap();

        // Re-running migrate() is exactly what the next `tasqx` open does on
        // this same file — the path an upgrade actually takes.
        migrate(&conn).unwrap();

        let body: String = conn
            .query_row("SELECT body FROM docs WHERE id = 'd1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            body, raw_body,
            "the stored body must stay exactly what was written"
        );

        let search_body: String = conn
            .query_row("SELECT search_body FROM docs WHERE id = 'd1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            !search_body.contains("---"),
            "the derived search text must not fence: {search_body:?}"
        );
        assert!(
            search_body.contains("description  How an SDK release is cut"),
            "the description survives as prose in the index text: {search_body:?}"
        );

        // The index must read the backfilled column, not just the row: a
        // query for a word that lived only in the frontmatter must still
        // resolve, and its snippet must be clean.
        let snip: String = conn
            .query_row(
                "SELECT snippet(docs_fts, 1, '', '', '…', 12) FROM docs_fts \
                 WHERE docs_fts MATCH 'SDK'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !snip.contains("---"),
            "the reindexed snippet still fences: {snip:?}"
        );

        // The index's text column keeps the name D41 gave it, so a raw
        // `body:` column filter still resolves after the upgrade.
        let by_column: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM docs_fts WHERE docs_fts MATCH 'body:SDK'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(by_column, 1, "`body:` must still name the text column");

        // A store already migrated must not be touched again.
        migrate(&conn).unwrap();
        let body_again: String = conn
            .query_row("SELECT body FROM docs WHERE id = 'd1'", [], |r| r.get(0))
            .unwrap();
        let search_body_again: String = conn
            .query_row("SELECT search_body FROM docs WHERE id = 'd1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(body, body_again, "a third migrate() must be a no-op here");
        assert_eq!(search_body, search_body_again);
    }

    /// #101/D156: a store written before `docs.standing` existed must gain the
    /// column on the way in, with every doc already in it reading back as NOT
    /// standing — a migration that promoted existing docs would put a store's
    /// whole memory into every session at once.
    ///
    /// Driven through `memory.get`/`memory.update` rather than through SQL,
    /// because the column landing and the engine being able to read and flip
    /// it on an upgraded row are two different claims, and only the second is
    /// what an agent on a real (upgraded) store experiences.
    ///
    /// File-backed, unlike the in-memory fixtures above: the upgrade path IS a
    /// close and a reopen — `Engine::open` is what runs `migrate` — and an
    /// in-memory store does not survive one.
    #[test]
    fn migration_adds_standing_and_leaves_an_existing_doc_not_standing() {
        use serde_json::json;

        let path = std::env::temp_dir()
            .join(format!(
                "tasqx-standing-migration-{}-{}.db",
                std::process::id(),
                crate::clock::uuid_v7()
            ))
            .to_string_lossy()
            .into_owned();
        // A UUID because `memory.get` refuses any other shape before it ever
        // asks the store (#229 item 4).
        let id = crate::clock::uuid_v7().to_string();
        {
            let conn = Connection::open(&path).unwrap();
            configure(&conn).unwrap();
            // The pre-#101 `docs` shape: every column this one arrived next
            // to, and no `standing`.
            conn.execute_batch(
                "CREATE TABLE docs (
                    id         TEXT PRIMARY KEY,
                    source     TEXT,
                    title      TEXT NOT NULL,
                    body       TEXT NOT NULL,
                    created    TEXT NOT NULL,
                    modified   TEXT NOT NULL,
                    project    TEXT,
                    rev        INTEGER NOT NULL DEFAULT 0,
                    search_body TEXT NOT NULL DEFAULT ''
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO docs (id, source, title, body, created, modified) \
                 VALUES (?1, NULL, 'the ruling', 'reinstall after merging', 't', 't')",
                params![id],
            )
            .unwrap();
        }

        let e = crate::Engine::open(&path).unwrap();
        let before = e.memory_get(&json!({ "id": id })).unwrap();
        assert_eq!(
            before["standing"],
            json!(false),
            "a doc that predates the column must read back as ordinary memory: {before}"
        );

        let flipped = e
            .memory_update(&json!({ "id": id, "standing": true }))
            .expect("the upgraded row takes the flag");
        assert_eq!(flipped["standing"], json!(true));
        let after = e.memory_get(&json!({ "id": id })).unwrap();
        assert_eq!(
            after["standing"],
            json!(true),
            "the flip must be what the store now holds, not only what the update echoed: {after}"
        );

        drop(e);
        let _ = std::fs::remove_file(&path);
    }

    /// D174 (task #85): `docs.source` is a doc's identity, backed by a partial
    /// UNIQUE index — but a store written before it can already hold two rows
    /// on one source. The migration must resolve them WITHOUT deleting
    /// anything: the most recently modified row keeps the source, the older
    /// ones keep their title and body with the source cleared, and only then
    /// does the index go on.
    #[test]
    fn migration_resolves_duplicate_doc_sources_and_adds_the_unique_index() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        migrate(&conn).unwrap();
        // The pre-D174 shape: every column, no index on `source`.
        conn.execute_batch("DROP INDEX IF EXISTS idx_docs_source;")
            .unwrap();
        // `newest`'s whole-second stamp against `mid`'s trimmed fraction is
        // the D142 trap: as raw text `...10Z` sorts ABOVE `...10.5Z`.
        for (id, source, modified) in [
            ("old", Some("deploy.md"), "2026-09-01T09:00:00Z"),
            ("newest", Some("deploy.md"), "2026-09-10T10:00:11Z"),
            ("mid", Some("deploy.md"), "2026-09-10T10:00:10.5Z"),
            ("alone", Some("other.md"), "2026-09-01T09:00:00Z"),
            ("loose", None, "2026-09-01T09:00:00Z"),
            // An empty source is no identity (D174): both keep their `""`.
            ("empty1", Some(""), "2026-09-01T09:00:00Z"),
            ("empty2", Some(""), "2026-09-02T09:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO docs (id, source, title, body, created, modified) \
                 VALUES (?1, ?2, ?1, 'body of ' || ?1, 't', ?3)",
                params![id, source, modified],
            )
            .unwrap();
        }

        migrate(&conn).unwrap();

        let rows: Vec<(String, Option<String>, String)> = conn
            .prepare("SELECT id, source, body FROM docs ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let src = |id: &str| rows.iter().find(|r| r.0 == id).unwrap().1.clone();
        assert_eq!(rows.len(), 7, "nothing may be deleted: {rows:?}");
        assert_eq!(src("empty1").as_deref(), Some(""), "{rows:?}");
        assert_eq!(src("empty2").as_deref(), Some(""), "{rows:?}");
        assert_eq!(src("newest").as_deref(), Some("deploy.md"), "{rows:?}");
        assert_eq!(src("mid"), None, "{rows:?}");
        assert_eq!(src("old"), None, "{rows:?}");
        assert_eq!(src("alone").as_deref(), Some("other.md"), "{rows:?}");
        assert!(
            rows.iter().all(|r| r.2 == format!("body of {}", r.0)),
            "every body must survive: {rows:?}"
        );

        let dup = conn.execute(
            "INSERT INTO docs (id, source, title, body, created, modified) \
             VALUES ('late', 'deploy.md', 't', 'b', 't', 't')",
            [],
        );
        assert!(dup.is_err(), "the index must refuse a second holder");
        conn.execute(
            "INSERT INTO docs (id, source, title, body, created, modified) \
             VALUES ('loose2', NULL, 't', 'b', 't', 't')",
            [],
        )
        .expect("the index is partial: many docs may have no source");
        conn.execute(
            "INSERT INTO docs (id, source, title, body, created, modified) \
             VALUES ('empty3', '', 't', 'b', 't', 't')",
            [],
        )
        .expect("nor is an empty source an identity");

        migrate(&conn).expect("the migration is idempotent");
    }

    /// A YAML list item or a folded block scalar's continuation line has no
    /// `:` — the exact case a review finding caught the first cut of this fix
    /// dropping entirely, silently un-finding a doc by any word that lived
    /// only in one. `migrate()`'s backfill must carry those words into
    /// `search_body` too.
    #[test]
    fn migration_backfill_keeps_a_frontmatter_list_items_words_searchable() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        migrate(&conn).unwrap();
        // A direct INSERT that omits `search_body`, landing it at its `''`
        // default — the same shape an old binary's write (or a hand-restored
        // row) takes against a store this code has already upgraded once.
        conn.execute(
            "INSERT INTO docs (id, source, title, body, created, modified) \
             VALUES ('d1', NULL, 'runbook', \
             '---\ntags:\n  - deploy\n  - kubernetes\n---\nRoll out the change.', 't', 't')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let body: String = conn
            .query_row("SELECT body FROM docs WHERE id = 'd1'", [], |r| r.get(0))
            .unwrap();
        assert!(body.contains("---"), "the stored body must stay untouched");

        let hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM docs_fts WHERE docs_fts MATCH 'kubernetes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            hit, 1,
            "a word that lived only inside a frontmatter list must still be found"
        );
    }

    /// A store opened by a build whose `docs_fts` declared its text column
    /// `search_body` straight over `docs` must be moved onto the `body`
    /// column that reads it through `docs_search`, or a raw `body:` filter
    /// stays broken on exactly the stores that ran that build.
    #[test]
    fn migration_moves_a_search_body_column_index_back_to_body() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch(
            r#"
            DROP TRIGGER docs_fts_ai; DROP TRIGGER docs_fts_ad; DROP TRIGGER docs_fts_au;
            DROP TABLE docs_fts;
            CREATE VIRTUAL TABLE docs_fts USING fts5(
                title, search_body, content='docs', content_rowid='rowid',
                tokenize='porter unicode61'
            );
            CREATE TRIGGER docs_fts_ai AFTER INSERT ON docs BEGIN
                INSERT INTO docs_fts(rowid, title, search_body)
                    VALUES (new.rowid, new.title, new.search_body);
            END;
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO docs (id, source, title, body, search_body, created, modified) \
             VALUES ('d1', NULL, 'runbook', 'Cut the release.', 'Cut the release.', 't', 't')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM docs_fts WHERE docs_fts MATCH 'body:release'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, 1);
        // And a write after the upgrade goes through the recreated triggers.
        conn.execute(
            "UPDATE docs SET search_body = 'Tag it.' WHERE id = 'd1'",
            [],
        )
        .unwrap();
        let tagged: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM docs_fts WHERE docs_fts MATCH 'body:tag'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tagged, 1);
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

    /// D172: a store written before tags were normalised can hold `UPPER` and
    /// `upper` as two tags, and a tag with a space in it. The migration folds
    /// them into the lowercased, hyphenated form, re-points every link at the
    /// surviving row, loses no task's tag, and logs what it changed on each
    /// task it touched. A second run is a no-op.
    #[test]
    fn migration_lowercases_hyphenates_and_merges_tags_without_losing_a_link() {
        let conn = open_in_memory().unwrap();
        for (id, name) in [
            ("t1", "UPPER"),
            ("t2", "upper"),
            ("t3", "has  space"),
            ("t4", "ünïcödé"),
            ("t5", "Ünïcödé"),
        ] {
            conn.execute(
                "INSERT INTO tags (id, name) VALUES (?1, ?2)",
                params![id, name],
            )
            .unwrap();
        }
        for (task, tag) in [
            ("task-1", "t1"),
            ("task-1", "t2"),
            ("task-1", "t3"),
            ("task-1", "t4"),
            ("task-2", "t5"),
            ("task-2", "t1"),
        ] {
            conn.execute(
                "INSERT INTO task_tags (task_id, tag_id) VALUES (?1, ?2)",
                params![task, tag],
            )
            .unwrap();
        }

        migrate(&conn).unwrap();

        let names: Vec<String> = conn
            .prepare("SELECT name FROM tags ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(names, ["has-space", "upper", "ünïcödé"]);
        assert_eq!(
            task_tags(&conn, "task-1").unwrap(),
            ["has-space", "upper", "ünïcödé"]
        );
        assert_eq!(task_tags(&conn, "task-2").unwrap(), ["upper", "ünïcödé"]);

        let logged = |task: &str| -> Vec<String> {
            conn.prepare("SELECT payload FROM events WHERE entity_id = ?1 AND op = 'tag.normalize'")
                .unwrap()
                .query_map(params![task], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let one = logged("task-1");
        assert_eq!(one.len(), 1, "one event per task touched: {one:?}");
        for fixture in ["UPPER", "has  space"] {
            assert!(one[0].contains(fixture), "{fixture} missing from {one:?}");
        }
        assert!(logged("task-2")[0].contains("Ünïcödé"));

        let events = |c: &Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                .unwrap()
        };
        let before = events(&conn);
        migrate(&conn).unwrap();
        assert_eq!(events(&conn), before, "a second run changes nothing");
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
