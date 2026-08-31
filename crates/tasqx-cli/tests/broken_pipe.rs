//! Closing stdout early is a shell idiom, not a crash.
//!
//! `emit()`'s own doc states the rule: `print!` panics when stdout closes
//! mid-write, and a downstream reader closing the pipe first is normal, so
//! `BrokenPipe` is success. But `emit` guarded only the `Exit::Out` terminal —
//! `api`, `manual`, and both `watch` output paths wrote to stdout with bare
//! `println!`/`print!`, so `tasqx watch | head` died with a panic (exit 101 and
//! a `failed printing to stdout` backtrace) the moment `head` exited.
//!
//! `api` is the deterministic stage for the defect: it writes nothing until
//! stdin reaches EOF, so the test can close the read end of stdout *before*
//! the response write happens, with no timing involved. The same `emit_via`
//! seam now carries every other site; the watch loop's exit-on-closed-pipe
//! behaviour is unit-tested at that seam in `lib.rs`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A fresh, isolated config dir + store for this test.
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let mut cfg = std::env::temp_dir();
    cfg.push(format!("tasqx-pipe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cfg);
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let mut db = std::env::temp_dir();
    db.push(format!("tasqx-pipe-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    (cfg, db)
}

#[test]
fn api_survives_a_reader_that_closed_before_the_response() {
    let (cfg, db) = scratch("api");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &cfg)
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "api"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tasqx api");

    // The reader leaves BEFORE the child can write a byte: `api` blocks on
    // stdin-EOF first, so dropping stdout here and only then closing stdin
    // sequences the closed pipe ahead of the response write on every platform.
    drop(child.stdout.take());
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(br#"{"tasqx":"1","method":"event.list","params":{"limit":1}}"#)
        .expect("write request");
    // stdin handle dropped by `take()` scope end above: EOF delivered.

    let status = child.wait().expect("wait for tasqx api");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert!(
        status.success(),
        "a closed stdout must be tolerated, not a panic: exit={status:?}\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "no panic may reach stderr:\n{stderr}"
    );
}
