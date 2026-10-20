//! D236's checks that need separate processes: lease expiry read against the
//! clock with no daemon running (`TASQX_NOW` moves the clock per process), and
//! two processes claiming one task at the same moment.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{json, Value};

struct Store {
    dir: PathBuf,
    db: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Store {
        let mut dir = std::env::temp_dir();
        dir.push(format!("tasqx-claims-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create config dir");
        let db = dir.join("tasks.db");
        Store { dir, db }
    }

    /// The binary on this store, `--no-daemon`, the clock pinned at `now`.
    fn bin(&self, now: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", &self.db)
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
            .env("TASQX_NOW", now)
            .arg("--no-daemon");
        c
    }

    fn api(&self, now: &str, method: &str, params: Value) -> Value {
        let mut child = self
            .bin(now)
            .arg("api")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn tasqx api");
        let envelope = json!({ "tasqx": "1", "id": "t", "method": method, "params": params });
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(envelope.to_string().as_bytes())
            .expect("write envelope");
        let out = child.wait_with_output().expect("wait");
        serde_json::from_slice(&out.stdout).expect("one JSON response")
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const T0: &str = "2026-10-20T09:00:00Z";
const T_LIVE: &str = "2026-10-20T09:29:00Z";
const T_EXPIRED: &str = "2026-10-20T09:31:00Z";

#[test]
fn an_expired_lease_is_claimable_by_anyone_with_no_daemon_running() {
    let s = Store::new("expiry");
    s.api(T0, "task.add", json!({ "title": "leased" }));
    let first = s.api(T0, "task.claim", json!({ "ref": 1, "actor": "a" }));
    assert_eq!(
        first["result"]["claim_expires"], "2026-10-20T09:30:00Z",
        "{first}"
    );

    let early = s.api(T_LIVE, "task.claim", json!({ "ref": 1, "actor": "b" }));
    assert_eq!(early["ok"], false, "{early}");
    assert_eq!(early["error"]["code"], "conflict", "{early}");

    // Nothing ran between the two calls: the lease lapsed because the clock
    // moved, and the read is what notices.
    let got = s.api(T_EXPIRED, "task.get", json!({ "ref": 1 }));
    assert_eq!(got["result"]["claimed_by"], Value::Null, "{got}");
    let late = s.api(T_EXPIRED, "task.claim", json!({ "ref": 1, "actor": "b" }));
    assert_eq!(late["ok"], true, "{late}");
    assert_eq!(late["result"]["op"], "takeover", "{late}");

    let events = s.api(T_EXPIRED, "event.list", json!({ "ref": 1 }));
    let takeover = events["result"]["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["op"] == "takeover")
        .cloned()
        .expect("a takeover event");
    assert_eq!(takeover["actor"], "b", "{takeover}");
    assert_eq!(takeover["payload"]["holder"], "b", "{takeover}");
    assert_eq!(takeover["payload"]["from"], "a", "{takeover}");
}

#[test]
fn the_cli_verbs_claim_and_release_under_a_holder() {
    let s = Store::new("verbs");
    s.api(T0, "task.add", json!({ "title": "by hand" }));
    let run = |args: &[&str]| -> Output { s.bin(T0).args(args).output().expect("run") };
    let ok = run(&["claim", "1", "--holder", "builder-2", "--lease", "2h"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("claimed by builder-2"));

    let refused = run(&["claim", "1"]);
    assert_eq!(refused.status.code(), Some(5), "a live lease is a conflict");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("builder-2"));
    let start = run(&["start", "1"]);
    assert_eq!(start.status.code(), Some(5), "the shell is not builder-2");

    let shown = run(&["show", "1"]);
    let text = String::from_utf8_lossy(&shown.stdout);
    assert!(
        text.contains("claimed") && text.contains("builder-2"),
        "{text}"
    );

    let released = run(&["release", "1", "--holder", "builder-2"]);
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
    assert!(run(&["claim", "1"]).status.success(), "free again");
}

/// Review of #2328: the shell's own claim stays in its own `next`, a named
/// holder can start what it claimed, and `--force` frees another's claim.
#[test]
fn a_shell_claim_stays_in_next_and_a_named_holder_can_start_it() {
    let s = Store::new("shellnext");
    s.api(T0, "task.add", json!({ "title": "five" }));
    s.api(T0, "task.add", json!({ "title": "named" }));
    let run = |args: &[&str]| -> Output { s.bin(T0).args(args).output().expect("run") };

    assert!(run(&["claim", "1"]).status.success());
    let next = run(&["--json", "next"]);
    let body: Value = serde_json::from_slice(&next.stdout).expect("next --json");
    assert!(body.to_string().contains("\"five\""), "{body}");

    assert!(run(&["claim", "2", "--holder", "builder-2"])
        .status
        .success());
    assert_eq!(
        run(&["start", "2"]).status.code(),
        Some(5),
        "the shell is not builder-2"
    );
    let started = run(&["start", "2", "--holder", "builder-2"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );

    assert_eq!(run(&["release", "2"]).status.code(), Some(5));
    let forced = run(&["release", "2", "--force-claim"]);
    assert!(
        forced.status.success(),
        "{}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(String::from_utf8_lossy(&forced.stdout).contains("forced"));
}

/// Two processes claim one task at the same moment, 100 times over: exactly
/// one wins each round. `task.claim` reads and writes the lease inside one
/// `BEGIN IMMEDIATE`, so the loser sees the winner's row.
#[test]
fn two_processes_racing_for_one_task_have_exactly_one_winner() {
    const ROUNDS: usize = 100;
    let s = Store::new("race");
    let tasks: Vec<Value> = (0..ROUNDS)
        .map(|i| json!({ "id": format!("019f0000-0000-7000-8000-{i:012}"), "short_id": i + 1, "title": format!("round {i}"), "status": "pending" }))
        .collect();
    let imported = s.api(T0, "store.import", json!({ "tasks": tasks }));
    assert_eq!(imported["ok"], true, "{imported}");

    for round in 1..=ROUNDS {
        let r = round.to_string();
        let spawn = |holder: &str| {
            s.bin(T0)
                .args(["claim", &r, "--holder", holder])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn claim")
        };
        let (a, b) = (spawn("racer-a"), spawn("racer-b"));
        let wins = [a, b]
            .into_iter()
            .map(|c| c.wait_with_output().expect("wait").status.code())
            .collect::<Vec<_>>();
        let won = wins.iter().filter(|c| **c == Some(0)).count();
        let lost = wins.iter().filter(|c| **c == Some(5)).count();
        assert_eq!((won, lost), (1, 1), "round {round}: exit codes {wins:?}");
    }
}

/// Review 2 #4: a CLI agent naming its holder sees its own claims in `next`
/// and `list`, and the shell, holding none of them, does not.
#[test]
fn next_and_list_take_the_holder_a_cli_agent_claims_under() {
    let s = Store::new("holdernext");
    s.api(T0, "task.add", json!({ "title": "agent work" }));
    let run = |args: &[&str]| -> Output { s.bin(T0).args(args).output().expect("run") };
    assert!(run(&["claim", "1", "--holder", "agent-1"]).status.success());

    let mine = run(&["--json", "next", "--holder", "agent-1"]);
    assert!(String::from_utf8_lossy(&mine.stdout).contains("agent work"));
    let listed = run(&["--json", "list", "--holder", "agent-1"]);
    assert!(String::from_utf8_lossy(&listed.stdout).contains("agent work"));
    let shell = run(&["--json", "next"]);
    assert!(!String::from_utf8_lossy(&shell.stdout).contains("agent work"));
    // A plain `list` is a view (review 3): it shows the claim and its holder.
    let view = run(&["--json", "list"]);
    let text = String::from_utf8_lossy(&view.stdout);
    assert!(
        text.contains("agent work") && text.contains("agent-1"),
        "{text}"
    );
}

/// Review 2 #10: `start --holder` names the claim holder only. The clock stays
/// anonymous, so a plain shell `start` elsewhere auto-stops it as D140 always
/// let a shell do, rather than being refused as another actor's clock.
#[test]
fn start_holder_leaves_the_clock_anonymous() {
    let s = Store::new("anonclock");
    for i in 1..=9 {
        s.api(T0, "task.add", json!({ "title": format!("task {i}") }));
    }
    let run = |args: &[&str]| -> Output { s.bin(T0).args(args).output().expect("run") };
    assert!(run(&["claim", "4", "--holder", "bot"]).status.success());
    let started = run(&["start", "4", "--holder", "bot"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );

    // Review 3 #1: the shell's start leaves bot's claimed task running.
    let plain = run(&["--json", "start", "9"]);
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let body: Value = serde_json::from_slice(&plain.stdout).expect("start --json");
    assert_eq!(body["kept_running"][0]["short_id"], 4, "{body}");
    let four = s.api(T0, "task.get", json!({ "ref": 4 }));
    assert_eq!(
        four["result"]["status"], "active",
        "#4 kept running: {four}"
    );

    // Had `--holder` named the clock's actor, a NAMED actor starting
    // elsewhere would be refused (D140). It is not: the clock is anonymous.
    let named = s.api(T0, "task.start", json!({ "ref": 8, "actor": "agent-2" }));
    assert_eq!(named["ok"], true, "{named}");
}
