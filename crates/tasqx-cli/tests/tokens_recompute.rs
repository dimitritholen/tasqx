//! `tasqx tokens recompute` end to end (D50 Decision 3, plan task 3.2).
//!
//! The engine verb is covered in `tasqx-core/tests/tokens.rs`; these drive the
//! REAL binary because the CLI adds the two things the engine cannot assert:
//! the destructive-by-default trap (`--apply` must be the explicit opt-in, and
//! a bare `tasqx tokens recompute` must write NOTHING no matter how often it is
//! repeated) and the human rendering a user reads before granting that opt-in.

use std::path::PathBuf;
use std::process::Command;

use serde_json::json;
use tasqx_core::Engine;

/// A `done` event payload carrying a transcript path, SERIALISED rather than
/// formatted.
///
/// The payload is rewritten directly because the engine stamps completions with
/// wall-clock time and these windows are field-observed. It must be built by
/// [`serde_json`]: a transcript path is an OS path, and on Windows it is
/// `C:\Users\…`, whose backslashes are not valid JSON escapes. A hand-written
/// JSON string carrying one is well-formed on Linux and malformed on Windows,
/// where it does not error — it just stops parsing into an attributable
/// completion, so the test measures nothing and reads as an engine bug.
fn done_payload(completed: &str, transcript_path: &str) -> String {
    json!({
        "completed": completed,
        "client": "claude-code",
        "transcript_path": transcript_path,
    })
    .to_string()
}

/// A fresh, isolated scratch dir (config + store + transcript) for one test.
fn scratch(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("tasqx-tokrec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// The binary against this test's own store. `--no-daemon` and `TASQX_DB` are
/// BOTH load-bearing (see regressions.rs `bin`): without them the recompute
/// under test — the one verb built to delete measurement rows — would run
/// against the developer's real store through a reachable daemon.
fn bin(dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir)
        .env("TASQX_DB", dir.join("store.db"))
        .arg("--no-daemon");
    c
}

fn run(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    let out = bin(dir).args(args).output().expect("run tasqx");
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout_json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn task_uuid(e: &Engine, sid: &serde_json::Value) -> String {
    e.conn()
        .query_row(
            "SELECT id FROM tasks WHERE short_id = ?1",
            [sid.as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap()
}

fn count(e: &Engine, sql: &str) -> i64 {
    e.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Seed the store file the binary will open with the live store's `019f98a4`
/// shape (the same fixture the engine tests use): task Y's window is a strict
/// subset of task X's over one transcript, and pre-D50 ticks banked the same
/// 1000/2000 line on both. X banked first and also caught an uncontested
/// 500/600 line.
fn seed_overlap(dir: &std::path::Path) {
    let transcript = dir.join("sess-1.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
            "\n",
            r#"{"timestamp":"2026-07-25T09:55:00.000Z","message":{"id":"b","usage":{"input_tokens":500,"output_tokens":600}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();

    let e = Engine::open(dir.join("store.db").to_str().unwrap()).expect("open the scratch store");
    let x = e.task_add(&json!({ "title": "X" })).unwrap()["short_id"].clone();
    let y = e.task_add(&json!({ "title": "Y" })).unwrap()["short_id"].clone();
    e.task_start(&json!({ "ref": y })).unwrap();
    e.task_done(&json!({ "ref": y, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    e.task_done(&json!({ "ref": x, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    let (x_id, y_id) = (task_uuid(&e, &x), task_uuid(&e, &y));
    // Pin the field-observed windows in afterwards: engine timestamps are
    // wall-clock, and this history has to predate D50.
    e.conn()
        .execute(
            "UPDATE tasks SET created = '2026-07-25T09:40:00Z' WHERE id = ?1",
            [&x_id],
        )
        .unwrap();
    e.conn()
        .execute(
            "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
            (r#"{"interval_started":"2026-07-25T09:46:53Z"}"#, &y_id),
        )
        .unwrap();
    for (id, completed) in [
        (&x_id, "2026-07-25T10:01:44Z"),
        (&y_id, "2026-07-25T09:49:37Z"),
    ] {
        e.conn()
            .execute(
                "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
                (done_payload(completed, &path), id),
            )
            .unwrap();
    }
    // The pre-D50 double-count, X banked first, no sample_ids in the markers.
    e.token_attribute(&json!({
        "ref": x, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 2, "input_tokens": 1500, "output_tokens": 2600,
    }))
    .unwrap();
    e.token_attribute(&json!({
        "ref": y, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();
}

/// The whole contract in one pass over one store: dry-run is the default and
/// writes nothing however often it runs; `--apply` performs exactly the delta
/// the dry-run printed; `--json` is the engine result verbatim.
#[test]
fn dry_run_is_the_default_and_apply_is_the_only_way_to_write() {
    let dir = scratch("contract");
    seed_overlap(&dir);
    let rows_before = {
        let e = Engine::open(dir.join("store.db").to_str().unwrap()).unwrap();
        count(&e, "SELECT COUNT(*) FROM token_usage")
    };

    // Bare invocation: dry-run, and the engine result reaches --json verbatim.
    let first = stdout_json(&run(&dir, &["--json", "tokens", "recompute"]));
    assert_eq!(first["dry_run"], true, "{first}");
    let tasks = first["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 2, "{first}");
    assert!(tasks.iter().all(|t| t["action"] == "recomputed"), "{first}");
    assert_eq!(first["totals"], json!({ "before": 7100, "after": 1100 }));

    // A second dry-run reports the SAME delta — the store cannot have moved.
    let second = stdout_json(&run(&dir, &["--json", "tokens", "recompute"]));
    assert_eq!(second, first, "a dry-run wrote something");
    {
        let e = Engine::open(dir.join("store.db").to_str().unwrap()).unwrap();
        assert_eq!(
            count(&e, "SELECT COUNT(*) FROM token_usage"),
            rows_before,
            "dry-run changed the row count"
        );
    }

    // --apply performs the printed plan, once.
    let applied = stdout_json(&run(&dir, &["--json", "tokens", "recompute", "--apply"]));
    assert_eq!(applied["dry_run"], false, "{applied}");
    assert_eq!(
        applied["tasks"], first["tasks"],
        "apply must do what the dry-run said"
    );
    {
        let e = Engine::open(dir.join("store.db").to_str().unwrap()).unwrap();
        // The subset window (Y, short_id 2) lost its rows; the superset (X)
        // keeps only the uncontested remainder.
        let (input, output): (i64, i64) = e
            .conn()
            .query_row(
                "SELECT input_tokens, output_tokens FROM token_usage WHERE source='log-parse'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("exactly one log-parse row should survive");
        assert_eq!((input, output), (500, 600));
        assert_eq!(
            count(
                &e,
                "SELECT COUNT(*) FROM token_usage WHERE source='log-parse'"
            ),
            1
        );
    }

    // The repaired store reports no further changes.
    let after = stdout_json(&run(&dir, &["--json", "tokens", "recompute"]));
    assert_eq!(after["totals"], json!({ "before": 1100, "after": 1100 }));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The human rendering: one line per changed task, a totals line, and — only on
/// dry-run — a closer naming the flag that would make it real.
#[test]
fn the_dry_run_rendering_names_apply_and_the_applied_one_does_not() {
    let dir = scratch("render");
    seed_overlap(&dir);

    let dry = run(&dir, &["tokens", "recompute"]);
    let text = String::from_utf8_lossy(&dry.stdout);
    assert!(
        text.contains("--apply"),
        "the dry-run must say how to make it real: {text}"
    );
    assert!(text.contains("#1"), "one line per changed task: {text}");
    assert!(text.contains("#2"), "one line per changed task: {text}");
    assert!(text.contains("recomputed"), "{text}");
    assert!(
        text.contains("7100") && text.contains("1100"),
        "the totals line must carry the delta: {text}"
    );

    let applied = run(&dir, &["tokens", "recompute", "--apply"]);
    let text = String::from_utf8_lossy(&applied.stdout);
    assert!(
        !text.contains("--apply"),
        "an applied run must not advertise --apply: {text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A store with nothing in scope answers politely on both surfaces rather than
/// printing an empty table — the first thing every fresh install will see.
#[test]
fn an_empty_store_reports_nothing_to_recompute() {
    let dir = scratch("empty");
    let out = run(&dir, &["tokens", "recompute"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.to_lowercase().contains("no log-parse"),
        "expected a nothing-in-scope line: {text}"
    );
    let v = stdout_json(&run(&dir, &["--json", "tokens", "recompute"]));
    assert_eq!(v["tasks"], json!([]));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The help surfaces must sell the safety contract: the subcommand help names
/// dry-run as the default and `--apply` as the opt-in.
#[test]
fn help_text_pins_the_dry_run_default() {
    let dir = scratch("help");
    let out = bin(&dir)
        .args(["tokens", "recompute", "-h"])
        .output()
        .expect("run -h");
    let h = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(h.contains("dry-run"), "{h}");
    assert!(h.contains("--apply"), "{h}");

    let out = bin(&dir).args(["tokens", "-h"]).output().expect("run -h");
    let h = String::from_utf8_lossy(&out.stdout);
    assert!(
        h.contains("EXAMPLES"),
        "the tokens group must carry cmddoc examples: {h}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A daemon-routed `tasqx tokens recompute` is REFUSED by the daemon (the verb
/// parses transcripts and must never run under the daemon's engine lock), and
/// the CLI surfaces the daemon's refusal message verbatim — naming the
/// `--no-daemon` invocation — instead of swallowing it. Unix-only for the
/// same reason as the stub-daemon regression: a portable path socket.
#[cfg(unix)]
#[test]
fn a_daemon_routed_recompute_surfaces_the_in_process_refusal_verbatim() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    let dir = scratch("refused");
    let sock = dir.join("scratch.sock").to_string_lossy().into_owned();
    let db = dir.join("daemon-store.db").to_string_lossy().into_owned();

    // A REAL daemon on a scratch socket + scratch store: the routed command
    // below never sees `TASQX_DB`, so the daemon must own an isolated store.
    let shutdown = Arc::new(AtomicBool::new(false));
    let sd = shutdown.clone();
    let sk = sock.clone();
    let server = std::thread::spawn(move || {
        let engine = tasqx_core::Engine::open(&db).expect("open scratch daemon store");
        tasqx_core::daemon::serve(engine, &sk, sd).expect("serve scratch daemon");
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(c) = tasqx_core::daemon::try_connect(&sock) {
            drop(c);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "scratch daemon never became connectable"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Deliberately WITHOUT `--no-daemon`: `TASQX_SOCK` routes the command to
    // the scratch daemon, which must refuse it before dispatch.
    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &dir)
        .env("TASQX_SOCK", &sock)
        .args(["tokens", "recompute"])
        .output()
        .expect("run tasqx");
    assert!(
        !out.status.success(),
        "a daemon-routed recompute must fail, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(out.status.code(), Some(2), "bad_request maps to exit 2");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "tokens.recompute parses transcripts and must run in-process: \
             stop the daemon and run `tasqx --no-daemon tokens recompute`"
        ),
        "the daemon's message must surface verbatim: {stderr}"
    );

    shutdown.store(true, Ordering::Relaxed);
    let _ = server.join();
    let _ = std::fs::remove_dir_all(&dir);
}
