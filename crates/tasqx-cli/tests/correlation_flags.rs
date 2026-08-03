//! End-to-end guards for the correlation flags on `start` and `done`.
//!
//! The defect these cover was invisible to every existing test because it lived
//! in the one layer they skip. `task.start` and `task.done` have accepted
//! `client` / `session_id` / `prompt_id` / `transcript_path` since #12, the MCP
//! server sends them, and the API-level tests pass — but the CLI never offered a
//! way to type them, so every CLI-completed task fell out of the attribution
//! engine's candidate set — its scan over the `done` events skips every one
//! whose payload carries none of `client`, `transcript_path` or `session_id`,
//! which is exactly what a hand-typed `tasqx done 4` looks like — and measured
//! zero. A test that calls the API directly cannot see that gap; only one that
//! drives the real binary's argv can.
//!
//! So these run the binary, and they read back through `tasqx api` rather than
//! through a formatted view: the event payload is the durable artifact the
//! attribution engine actually reads, and a renderer that dropped a key would
//! otherwise be indistinguishable from a CLI that never sent it.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A fresh, isolated config dir + store for one test. Per-tag so cargo's
/// parallel threads cannot share a store and see each other's events.
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let mut cfg = std::env::temp_dir();
    cfg.push(format!("tasqx-corr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cfg);
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let mut db = std::env::temp_dir();
    db.push(format!("tasqx-corr-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    (cfg, db)
}

/// The binary, pointed at this test's own store. `--no-daemon` is not optional:
/// `open_backend` prefers a reachable daemon and the remote path ignores
/// `TASQX_DB`, so on a developer machine running `tasqx daemon` these tests
/// would drive the real store while asserting against an untouched scratch file.
fn bin(cfg: &std::path::Path, db: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", cfg)
        .env("TASQX_DB", db)
        .arg("--no-daemon");
    c
}

/// Run one JSON envelope through `tasqx api` and return the parsed response.
fn api(cfg: &std::path::Path, db: &std::path::Path, envelope: &str) -> serde_json::Value {
    let mut child = bin(cfg, db)
        .arg("api")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn api");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(envelope.as_bytes())
        .expect("write envelope");
    let out = child.wait_with_output().expect("api output");
    serde_json::from_slice(&out.stdout).expect("api response is JSON")
}

/// The payload of the newest event with this op, for this test's store.
fn latest_payload(cfg: &std::path::Path, db: &std::path::Path, op: &str) -> serde_json::Value {
    let resp = api(
        cfg,
        db,
        r#"{"tasqx":"1","method":"event.list","params":{"limit":50}}"#,
    );
    let events = resp["result"]["events"]
        .as_array()
        .expect("event.list returns events");
    events
        .iter()
        .find(|e| e["op"] == op)
        .unwrap_or_else(|| panic!("no {op} event in {events:#?}"))["payload"]
        .clone()
}

#[test]
fn start_and_done_record_the_correlation_they_were_given() {
    let (cfg, db) = scratch("records");
    assert!(bin(&cfg, &db)
        .args(["add", "measure something"])
        .status()
        .expect("add")
        .success());

    let ok = bin(&cfg, &db)
        .args([
            "start",
            "1",
            "--client",
            "claude-code 2.1",
            "--session-id",
            "sess-abc",
            "--prompt-id",
            "turn-7",
            "--transcript-path",
            "/tmp/does-not-need-to-exist.jsonl",
        ])
        .status()
        .expect("start");
    assert!(ok.success(), "start with correlation flags must succeed");

    let start = latest_payload(&cfg, &db, "start");
    assert_eq!(start["client"], "claude-code 2.1");
    assert_eq!(start["session_id"], "sess-abc");
    assert_eq!(start["prompt_id"], "turn-7");
    assert_eq!(
        start["transcript_path"],
        "/tmp/does-not-need-to-exist.jsonl"
    );

    // `done` carries its own copy: a task can start and finish many times, and
    // the attribution engine pairs the two events per occurrence.
    assert!(bin(&cfg, &db)
        .args([
            "done",
            "1",
            "--client",
            "claude-code 2.1",
            "--session-id",
            "sess-abc",
        ])
        .status()
        .expect("done")
        .success());
    let done = latest_payload(&cfg, &db, "done");
    assert_eq!(done["client"], "claude-code 2.1");
    assert_eq!(done["session_id"], "sess-abc");
}

#[test]
fn a_flagless_completion_sends_no_correlation_keys_at_all() {
    let (cfg, db) = scratch("flagless");
    assert!(bin(&cfg, &db)
        .args(["add", "a human task"])
        .status()
        .expect("add")
        .success());
    assert!(bin(&cfg, &db)
        .args(["done", "1"])
        .status()
        .expect("done")
        .success());

    // Absent, not null. The engine reads payloads tolerantly, but a `null` on
    // every human-issued done would be noise the attribution engine then has to
    // skip — and it would make this the one command whose wire shape changed for
    // users who never asked for token accounting.
    let done = latest_payload(&cfg, &db, "done");
    for key in ["client", "session_id", "prompt_id", "transcript_path"] {
        assert!(
            done.get(key).is_none(),
            "flagless done must not send {key}, got {done:#?}"
        );
    }
}

#[test]
fn correlation_without_a_client_is_refused_before_it_reaches_the_store() {
    let (cfg, db) = scratch("requires");
    assert!(bin(&cfg, &db)
        .args(["add", "unmeasurable"])
        .status()
        .expect("add")
        .success());

    // Without `client` the engine cannot select a parser — `parser_for` is only
    // ever asked about a client string, so with none the `let ... else` around
    // it takes its else branch — and it terminates with a zero-sample marker that
    // `has_attributed_event` makes permanent. Accepting the command would look
    // like a measurement and poison the task against a later correct one, so
    // clap refuses the combination instead (D33: a value that changes nothing
    // must not answer ok).
    for flag in ["--session-id", "--transcript-path"] {
        let out = bin(&cfg, &db)
            .args(["start", "1", flag, "whatever"])
            .output()
            .expect("start");
        assert!(
            !out.status.success(),
            "{flag} without --client must be refused"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--client"),
            "the error must name the missing flag, got: {stderr}"
        );
    }

    // The task is untouched by the refusal: still pending, never started.
    let out = bin(&cfg, &db)
        .args(["--json", "show", "1"])
        .output()
        .expect("show");
    let shown: serde_json::Value = serde_json::from_slice(&out.stdout).expect("show JSON");
    assert_eq!(shown["status"], "pending");

    // `--prompt-id` is exempt: it selects no parser, so it cannot cause the
    // silent-zero outcome the other two do.
    assert!(
        bin(&cfg, &db)
            .args(["start", "1", "--prompt-id", "turn-1"])
            .status()
            .expect("start")
            .success(),
        "--prompt-id alone is legitimate correlation metadata"
    );
}
