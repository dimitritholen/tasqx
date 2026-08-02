//! Guards for the read-only store seam (`storage::open_read_only` /
//! `Engine::open_read_only`).
//!
//! The seam exists for the shell-completion callback path: a Tab press is not a
//! command, so it must never author a database, never run a migration, and
//! never take a write lock. `storage::open` does all three by design
//! (`storage.rs`: `Connection::open` creates, `configure` sets `journal_mode`,
//! `migrate` runs DDL), so completion cannot reuse it — and the three
//! properties that make the read-only twin safe are asserted here rather than
//! asserted in a comment.
//!
//! The never-create property in particular is checked against the FILESYSTEM
//! after the failed open, not against the returned `Err`. An `Err` proves the
//! caller was told; only `Path::exists` proves nothing was written.
//!
//! One warning about what "never creates" means, because this file used to imply
//! more than it tested. The only never-create assertion here was on the MISSING
//! store, where the open fails before SQLite reaches its WAL layer at all — so it
//! passed green while the interesting case, a read-only connection against a
//! store that EXISTS, went unguarded. It turned out to create the `-shm`/`-wal`
//! sidecars, which the doc comment had promised it could not.
//! [`a_read_only_query_against_a_live_store_leaves_the_store_itself_untouched`]
//! is the guard that was missing, and it pins what actually happens rather than
//! what was hoped.

use serde_json::json;
use tasqx_core::{storage, Engine};

/// A distinct temp path per test, in the shape the other core tests use
/// (`concurrency.rs`, `tokens.rs`): the process id keeps two concurrent
/// `cargo test` runs from colliding, and the label keeps the tests in this file
/// from colliding with each other.
struct Store {
    path: std::path::PathBuf,
}

impl Store {
    fn new(label: &str) -> Store {
        let dir = std::env::temp_dir().join(format!(
            "tasqx-readonly-{label}-{}-{}",
            std::process::id(),
            // Two tests may run in the same process at the same instant, and
            // `label` is the only thing separating them, so it is required to be
            // unique per call site; the counter guards a copy-pasted label.
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Store {
            path: dir.join("tasks.db"),
        }
    }

    fn as_str(&self) -> &str {
        self.path.to_str().expect("UTF-8 temp path")
    }
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Drop for Store {
    fn drop(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The property the whole seam exists for: a completion callback on a machine
/// that has never run tasqx must leave that machine exactly as it found it.
///
/// `storage::open` would return `Ok` here and leave a fully migrated database
/// behind, which is the failure this asserts against — so the assertion is on
/// the filesystem, not on the `Result`.
#[test]
fn opening_a_missing_store_read_only_fails_and_creates_nothing() {
    let store = Store::new("missing");
    assert!(!store.path.exists(), "precondition: nothing there yet");

    let err = storage::open_read_only(store.as_str())
        .expect_err("a read-only open of a nonexistent path must fail, not create it");
    // The message has to name the path: this error is the one a human sees when
    // they run a read-only tool against the wrong `$TASQX_DB`.
    assert!(
        err.message.contains(store.as_str()),
        "the error must name the path it could not open, got {:?}",
        err.message
    );

    assert!(
        !store.path.exists(),
        "a failed read-only open must not have created {}",
        store.path.display()
    );
    // The sidecars count too: a WAL pair left behind is a file the Tab press
    // created just as much as the database would have been.
    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from(format!("{}{suffix}", store.path.display()));
        assert!(
            !sidecar.exists(),
            "a failed read-only open must not have created {}",
            sidecar.display()
        );
    }

    // And the same one level up: `Engine::open_read_only` must not soften it.
    assert!(Engine::open_read_only(store.as_str()).is_err());
    assert!(!store.path.exists());
}

/// The seam is only useful if it can actually answer the questions completion
/// asks: what are the open tasks, and what are the projects.
#[test]
fn a_seeded_store_reads_back_through_the_read_only_engine() {
    let store = Store::new("seeded");
    {
        let e = Engine::open(store.as_str()).expect("seed the store read-write");
        e.project_create(&json!({ "name": "work" }))
            .expect("create project");
        e.task_add(&json!({ "title": "write the completer", "project": "work", "tags": ["cli"] }))
            .expect("add task");
    }

    let ro = Engine::open_read_only(store.as_str()).expect("open the seeded store read-only");

    let tasks = ro.task_list(&json!({ "filter": "" })).expect("list tasks");
    assert_eq!(tasks["count"], 1);
    assert_eq!(tasks["tasks"][0]["title"], "write the completer");
    assert_eq!(tasks["tasks"][0]["tags"][0], "cli");

    let projects = ro.project_list(&json!({})).expect("list projects");
    assert_eq!(projects["projects"][0]["name"], "work");
}

/// Read-only must mean read-only at the SQLite level, not by convention. If the
/// flag were ever dropped the two tests above would still pass, and a Tab press
/// would quietly acquire a write lock on the user's live store.
#[test]
fn a_write_through_the_read_only_connection_is_refused() {
    let store = Store::new("write");
    {
        let e = Engine::open(store.as_str()).expect("seed the store read-write");
        e.task_add(&json!({ "title": "immutable from here" }))
            .expect("add task");
    }

    let conn = storage::open_read_only(store.as_str()).expect("open read-only");
    let err = conn
        .execute("UPDATE tasks SET title = 'clobbered'", [])
        .expect_err("a read-only connection must refuse a write");
    assert!(
        err.to_string().to_ascii_lowercase().contains("readonly"),
        "the refusal must come from SQLite's read-only flag, got {err}"
    );

    // Belt and braces: the row is untouched when read back through a fresh
    // read-write connection, so the refusal was not merely reported.
    let e = Engine::open(store.as_str()).expect("reopen read-write");
    let tasks = e.task_list(&json!({ "filter": "" })).expect("list");
    assert_eq!(tasks["tasks"][0]["title"], "immutable from here");
}

/// Every file in `dir`, sorted, as plain names.
fn listing(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("read the fixture dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    v.sort();
    v
}

/// The whole `sqlite_master` table, as one string: the schema's fingerprint.
fn schema(conn: &rusqlite::Connection) -> String {
    conn.prepare("SELECT type, name, sql FROM sqlite_master ORDER BY name")
        .expect("prepare")
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default()
            ))
        })
        .expect("query")
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The guard the seam actually needed, and did not have: a read-only open of a
/// store that EXISTS, followed by a real query.
///
/// The only never-create assertion in this file was on the missing-store path,
/// where the open fails before SQLite reaches the WAL layer — so it could never
/// observe what a read-only connection does to a live store, and the doc comment
/// on `open_read_only` was free to claim something false for three commits. It
/// claimed a read-only connection "cannot create the `-shm`/`-wal` sidecars". It
/// creates both.
///
/// Two different things are asserted here and the difference is the point.
///
/// The STORE is untouched, and that is a requirement: the database file is
/// byte-identical, the schema is unchanged (no migration ran), and no second
/// database was authored. A Tab press must not alter the user's data, and these
/// are what "must not" means when written as a test.
///
/// The SIDECARS are pinned as OBSERVED, and that is not a requirement — it is a
/// tripwire. Their creation is SQLite's behaviour, not tasqx's: the read-only
/// flag governs the database file, while the shm layer opens `-shm` with
/// `RDWR|CREATE` first and only falls back if that fails, so a writable directory
/// is enough. Asserting the exact set means a future SQLite that stops (or
/// starts) creating them fails this test loudly instead of silently invalidating
/// the paragraph in `storage.rs` that documents it. If that happens, re-measure
/// and update BOTH — the pin exists to force that pairing.
#[test]
fn a_read_only_query_against_a_live_store_leaves_the_store_itself_untouched() {
    let store = Store::new("livestore");
    let dir = store.path.parent().expect("fixture dir").to_path_buf();
    {
        let e = Engine::open(store.as_str()).expect("seed the store read-write");
        e.project_create(&json!({ "name": "work" }))
            .expect("create project");
        e.task_add(&json!({ "title": "do not disturb", "project": "work" }))
            .expect("add task");
    }

    // Snapshot with the writer gone. A cleanly closed SQLite connection
    // checkpoints and removes its own sidecars, so this is the state a user's
    // machine is in between commands — the state a Tab press finds.
    let before_files = listing(&dir);
    assert_eq!(
        before_files,
        vec!["tasks.db".to_string()],
        "precondition: the writer cleaned up after itself"
    );
    let before_bytes = std::fs::read(&store.path).expect("read the store");
    let before_schema = {
        let conn = storage::open_read_only(store.as_str()).expect("open to fingerprint");
        schema(&conn)
    };

    // The thing under test: open read-only and actually QUERY. The sidecars are
    // created on first query rather than at open, so a test that only opened
    // would have gone on missing this.
    {
        let ro = Engine::open_read_only(store.as_str()).expect("open read-only");
        let tasks = ro.task_list(&json!({ "filter": "" })).expect("list tasks");
        assert_eq!(tasks["count"], 1, "the query must really have run");
    }

    // 1. The database file is byte-for-byte what it was.
    assert_eq!(
        std::fs::read(&store.path).expect("re-read the store"),
        before_bytes,
        "a read-only query rewrote bytes of {}",
        store.path.display()
    );

    // 2. No DDL ran: same schema, so nothing was migrated into the file.
    let after_schema = {
        let conn = storage::open_read_only(store.as_str()).expect("reopen to fingerprint");
        schema(&conn)
    };
    assert_eq!(
        after_schema, before_schema,
        "the read-only open migrated the schema"
    );

    // 3. No second database was authored anywhere beside it.
    let after_files = listing(&dir);
    let new_dbs: Vec<&String> = after_files
        .iter()
        .filter(|f| f.ends_with(".db") && !before_files.contains(f))
        .collect();
    assert!(
        new_dbs.is_empty(),
        "a read-only open authored a new database: {new_dbs:?}"
    );

    // 4. The sidecars: pinned as measured, deliberately, per the doc comment.
    assert_eq!(
        after_files,
        vec![
            "tasks.db".to_string(),
            "tasks.db-shm".to_string(),
            "tasks.db-wal".to_string()
        ],
        "the observed sidecar behaviour changed. This is a pin, not a \
         requirement: a read-only connection DOES create -shm/-wal (the flag \
         governs the database file; the shm layer opens with RDWR|CREATE first) \
         and cannot delete them on close, because deleting them is a write. \
         Re-measure and update this assertion together with the paragraph in \
         storage.rs that documents it."
    );
}

/// The read-only open must not run the migration either. A store from a future
/// tasqx, or a file that merely happens to be SQLite, must be read as it is —
/// and a store from an OLDER tasqx must not be silently upgraded by a keystroke,
/// which is a schema write dressed up as a read.
#[test]
fn the_read_only_open_runs_no_migration() {
    let store = Store::new("nomigrate");
    {
        let conn = rusqlite::Connection::open(store.as_str()).expect("create a bare SQLite file");
        conn.execute_batch("CREATE TABLE unrelated (x INTEGER);")
            .expect("write a schema tasqx knows nothing about");
    }

    let conn = storage::open_read_only(store.as_str()).expect("open the bare file read-only");
    // `migrate` would have created `tasks` here (and failed, being read-only).
    // Its absence is the proof that it was never called.
    let tasks_table: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='tasks'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert!(
        tasks_table.is_none(),
        "the read-only open must not have migrated a schema into the file"
    );
}
