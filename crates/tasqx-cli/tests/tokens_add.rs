//! `tasqx tokens add` end to end (D167): a count that arrives after the task
//! was completed no longer needs a hand-built `tasqx api` envelope.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh, isolated scratch dir (config + store) for one test.
fn scratch(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("tasqx-tokadd-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// `--no-daemon` and `TASQX_DB` are both load-bearing: without them the write
/// under test would land in the developer's real store.
fn bin(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir)
        .env("TASQX_DB", dir.join("store.db"))
        .arg("--no-daemon");
    c
}

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = bin(dir).args(args).output().expect("run tasqx");
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn tokens_add_total_lands_after_completion_and_show_says_unsplit() {
    let dir = scratch("total");
    run(&dir, &["init", "demo"]);
    run(&dir, &["add", "measured late"]);
    run(&dir, &["done", "1"]);
    run(&dir, &["tokens", "add", "1", "--total", "37898"]);

    let out = run(&dir, &["--json", "show", "1"]);
    let t: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let m = &t["tokens"][0];
    assert_eq!(m["total_tokens"], 37898, "{t}");
    assert_eq!(m["source"], "self-report");
    assert_eq!(m["confidence"], "medium");

    let shown = String::from_utf8_lossy(&run(&dir, &["show", "1"]).stdout).to_string();
    assert!(shown.contains("total (unsplit) 37898"), "{shown}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tokens_add_split_counts_and_refuses_neither() {
    let dir = scratch("split");
    run(&dir, &["init", "demo"]);
    run(&dir, &["add", "split"]);
    run(
        &dir,
        &[
            "tokens",
            "add",
            "1",
            "--in",
            "10",
            "--out",
            "5",
            "--cache-read",
            "7",
            "--tool",
            "codex",
            "--model",
            "gpt",
        ],
    );
    let out = run(&dir, &["--json", "show", "1"]);
    let t: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let m = &t["tokens"][0];
    assert_eq!(m["input_tokens"], 10, "{t}");
    assert_eq!(m["output_tokens"], 5);
    assert_eq!(m["cache_read_tokens"], 7);
    assert_eq!(m["tool"], "codex");
    assert_eq!(m["model"], "gpt");

    // Neither a total nor a split is a usage error, not a zero measurement.
    let bad = bin(&dir).args(["tokens", "add", "1"]).output().unwrap();
    assert!(!bad.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}
