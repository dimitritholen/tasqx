//! D191: two machines that both complete a recurring task each spawn its next
//! occurrence, and an import folds the two copies into one. These histories
//! need hours between their events, so they run the real binary with the
//! clock pinned per call (`TASQX_NOW`), which a test inside the engine cannot
//! do without moving every other test in its binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// A scratch directory holding one store per machine.
fn scratch(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("tasqx-fold-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// One JSON API call against `machine`'s store at the pinned instant `now`.
fn api(dir: &Path, machine: &str, now: &str, method: &str, params: Value) -> Value {
    use std::io::Write;
    let req = json!({ "tasqx": "1", "id": "t", "method": method, "params": params }).to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", dir)
        .env("TASQX_DB", dir.join(format!("{machine}.db")))
        .env("TASQX_NOW", format!("2030-01-01T{now}:00Z"))
        .args(["--no-daemon", "api"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn api");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(req.as_bytes())
        .expect("write request");
    let out = child.wait_with_output().expect("api output");
    let env: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{method} did not answer JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(env["ok"], json!(true), "{method} on {machine}: {env}");
    env["result"].clone()
}

/// Merge `from`'s whole store into `into`, at `now`.
fn merge(dir: &Path, into: &str, from: &str, now: &str) {
    let mut doc = api(dir, from, now, "store.export", json!({}));
    doc["merge"] = json!(true);
    api(dir, into, now, "store.import", doc);
}

/// Every occurrence of the recurring task, as exported.
fn occurrences(dir: &Path, machine: &str, now: &str) -> Vec<Value> {
    api(dir, machine, now, "store.export", json!({}))["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .filter(|t| t.get("spawned_from").is_some())
        .cloned()
        .collect()
}

/// B holds R; A is a copy of B; A completes R at 01:00 and B at 02:00.
fn two_machines(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    api(
        &dir,
        "b",
        "00:00",
        "task.add",
        json!({ "title": "R", "recurrence": "every 3 days", "due": "2030-01-02T00:00:00Z" }),
    );
    let seed = api(&dir, "b", "00:00", "store.export", json!({}));
    api(&dir, "a", "00:00", "store.import", seed);
    api(&dir, "a", "01:00", "task.done", json!({ "ref": 1 }));
    api(&dir, "b", "02:00", "task.done", json!({ "ref": 1 }));
    dir
}

/// B starts its copy at 03:00; A folds it into its own at 03:30, which
/// makes A's copy active from 03:00; B stops its copy at 04:00. After both
/// directions, the stop is the latest word on the status: the timer is not
/// running again anywhere, and the hour counts once.
#[test]
fn a_stop_on_the_dropped_copy_after_the_fold_stops_the_survivor() {
    let dir = two_machines("stop");
    api(&dir, "b", "03:00", "task.start", json!({ "ref": 2 }));
    merge(&dir, "a", "b", "03:30");
    api(&dir, "b", "04:00", "task.stop", json!({ "ref": 2 }));
    merge(&dir, "b", "a", "05:00");
    merge(&dir, "a", "b", "05:00");
    for machine in ["a", "b"] {
        let occ = occurrences(&dir, machine, "06:00");
        assert_eq!(occ.len(), 1, "{machine}: {occ:?}");
        let t = &occ[0];
        assert_eq!(t["status"], json!("pending"), "{machine}: {t}");
        assert!(t["active_since"].is_null(), "{machine}: {t}");
        assert_eq!(t["tracked_seconds"], json!(3600), "{machine}: {t}");
    }
}

/// A completion on B's copy, taken in by A, must survive going back.
#[test]
fn a_completion_on_the_dropped_copy_survives_the_round_trip() {
    let dir = two_machines("done");
    api(&dir, "b", "03:00", "task.done", json!({ "ref": 2 }));
    merge(&dir, "a", "b", "04:00");
    merge(&dir, "b", "a", "05:00");
    merge(&dir, "a", "b", "06:00");
    for machine in ["a", "b"] {
        let pending: Vec<Value> = occurrences(&dir, machine, "07:00")
            .into_iter()
            .filter(|t| t["status"] == "pending")
            .collect();
        assert_eq!(pending.len(), 1, "{machine}: {pending:?}");
    }
}

/// B completes R at 00:30, A at 01:00; B then reopens R and completes it
/// again. That second completion lands on the slot B already spawned, so
/// both stores end with one next occurrence, the same one.
#[test]
fn a_reopen_and_done_on_one_machine_converges_with_the_other() {
    let dir = scratch("reopen");
    api(
        &dir,
        "b",
        "00:00",
        "task.add",
        json!({ "title": "R", "recurrence": "every 3 days", "due": "2030-01-02T00:00:00Z" }),
    );
    let seed = api(&dir, "b", "00:00", "store.export", json!({}));
    api(&dir, "a", "00:00", "store.import", seed);
    api(&dir, "a", "01:00", "task.done", json!({ "ref": 1 }));
    api(&dir, "b", "00:30", "task.done", json!({ "ref": 1 }));
    api(&dir, "b", "02:10", "task.reopen", json!({ "ref": 1 }));
    api(&dir, "b", "02:20", "task.done", json!({ "ref": 1 }));
    for (into, from) in [("a", "b"), ("b", "a"), ("a", "b"), ("b", "a")] {
        merge(&dir, into, from, "03:00");
    }
    let ids = |machine: &str| -> Vec<Value> {
        occurrences(&dir, machine, "04:00")
            .iter()
            .map(|t| t["id"].clone())
            .collect()
    };
    assert_eq!(ids("a").len(), 1, "{:?}", ids("a"));
    assert_eq!(ids("a"), ids("b"));
}
