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
