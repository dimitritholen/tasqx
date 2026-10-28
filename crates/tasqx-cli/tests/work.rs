//! `tasqx work` (D245) against a FAKE agent: a shell script on a temp `PATH`
//! named `claude` or `codex` that records its argv, its `TASQX_WORK_*` env, a
//! copy of the start context and the task as the store holds it mid-session,
//! then exits with a chosen code. The real binaries are never run.
//!
//! Unix only: the fake agent is a `#!/bin/sh` script, and the spawn path
//! under test is the same code on every platform.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Store {
    dir: PathBuf,
    db: PathBuf,
    bin: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("tasqx-work-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("create fake PATH dir");
        let db = dir.join("tasks.db");
        Store { dir, db, bin }
    }

    /// The binary on this store, `--no-daemon`, with only the fake agents and
    /// the system's `sh` tools on `PATH`.
    fn tasqx(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", &self.db)
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("HOME", dir_home(&self.dir))
            .env_remove("TASQX_NOW")
            .arg("--no-daemon");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.tasqx().args(args).output().expect("run tasqx")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn task(&self, r: &str) -> Value {
        serde_json::from_str(&self.ok(&["--json", "show", r])).unwrap()
    }

    /// `event.list` through `tasqx api`, the one JSON API.
    fn event_list(&self, params: &str) -> String {
        use std::io::Write;
        let mut child = self
            .tasqx()
            .arg("api")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn tasqx api");
        let env = format!(r#"{{"tasqx":"1","id":"t","method":"event.list","params":{params}}}"#);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(env.as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
    }

    fn events(&self) -> usize {
        let v: Value = serde_json::from_str(&self.event_list(r#"{"limit":500}"#)).unwrap();
        v["result"]["events"].as_array().map_or(0, Vec::len)
    }

    /// A fake agent named `name` that records into `<dir>/<name>.*` and exits
    /// with `code`.
    fn fake_agent(&self, name: &str, code: i32) {
        self.fake_agent_then(name, code, "");
    }

    /// [`Store::fake_agent`], running `then` (shell) just before it exits.
    fn fake_agent_then(&self, name: &str, code: i32, then: &str) {
        let rec = self.dir.join(name);
        let rec = rec.display();
        let tasqx = env!("CARGO_BIN_EXE_tasqx");
        let script = format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\"; done > {rec}.argv\n\
             env | grep '^TASQX_' | sort > {rec}.env\n\
             [ -n \"$TASQX_WORK_CONTEXT\" ] && cat \"$TASQX_WORK_CONTEXT\" > {rec}.context\n\
             [ -n \"$TASQX_WORK_CONTEXT\" ] && ls -l \"$TASQX_WORK_CONTEXT\" | cut -c1-10 > {rec}.mode\n\
             {tasqx} --no-daemon --json show \"$TASQX_WORK_TASK\" > {rec}.during\n\
             {then}\n\
             exit {code}\n"
        );
        let path = self.bin.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn env_of(&self, agent: &str, key: &str) -> String {
        self.read(&format!("{agent}.env"))
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")).map(str::to_string))
            .unwrap_or_default()
    }

    /// The task seeded every test works on: one check, one standing ruling in
    /// its project.
    fn seed(&self) {
        self.ok(&["init", "proj"]);
        self.ok(&["add", "Fix the login redirect", "project:proj"]);
        self.ok(&["check", "add", "1", "the redirect lands on /home"]);
        self.ok(&[
            "memory",
            "add",
            "Never log tokens",
            "Tokens stay out of every log line.",
            "--project",
            "proj",
            "--standing",
        ]);
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A HOME of the test's own, so nothing reaches the real `~/.claude`.
fn dir_home(dir: &Path) -> PathBuf {
    dir.join("home")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn work_claims_times_spawns_claude_with_its_context_and_cleans_up_after() {
    let s = Store::new("claude");
    s.seed();
    s.fake_agent("claude", 3);

    let out = s.run(&["work", "1", "--agent", "claude"]);
    // The child's exit code is the command's.
    assert_eq!(out.status.code(), Some(3), "{out:?}");

    let session = s.env_of("claude", "TASQX_WORK_SESSION");
    let context = s.env_of("claude", "TASQX_WORK_CONTEXT");
    assert_eq!(s.env_of("claude", "TASQX_WORK_TASK"), "1");
    assert_eq!(session.len(), 36, "a UUID: {session:?}");
    // The argv is one line per argument; the --settings JSON is one line.
    let argv: Vec<String> = s.read("claude.argv").lines().map(str::to_string).collect();
    assert_eq!(
        argv[..5],
        [
            "--session-id",
            session.as_str(),
            "--append-system-prompt-file",
            context.as_str(),
            "--settings",
        ],
    );
    assert_eq!(argv.len(), 6, "{argv:?}");
    // D248: per-session hooks, every one the same constant command.
    let settings: Value = serde_json::from_str(&argv[5]).expect("--settings is JSON");
    let hooks = settings["hooks"].as_object().expect("hooks");
    let events: Vec<&str> = hooks.keys().map(String::as_str).collect();
    assert_eq!(events, ["SessionEnd", "SessionStart"], "{settings}");
    for (event, groups) in hooks {
        assert_eq!(
            groups,
            &serde_json::json!([{ "hooks": [{ "type": "command", "command": "tasqx hook claude" }] }]),
            "{event}"
        );
    }
    // The user's own Claude settings are not touched.
    assert!(!s.dir.join("home").join(".claude").exists());

    // The start context: the brief, its checks and the project's ruling.
    let ctx = s.read("claude.context");
    assert!(ctx.contains("Fix the login redirect"), "{ctx}");
    assert!(ctx.contains("the redirect lands on /home"), "{ctx}");
    assert!(ctx.contains("Never log tokens"), "{ctx}");
    // D248: the D246 section, gist included, not a list of titles and ids.
    assert!(ctx.contains("Tokens stay out of every log line."), "{ctx}");
    assert!(!ctx.contains("tasqx_add_check"), "{ctx}");

    // While the agent ran: claimed and timed.
    let during: Value = serde_json::from_str(&s.read("claude.during")).unwrap();
    assert_eq!(during["status"], "active", "{during}");
    assert!(
        during["claimed_by"]
            .as_str()
            .unwrap_or("")
            .starts_with("work:"),
        "{during}"
    );

    // After: the timer stopped, the claim released, the context file gone.
    let after = s.task("1");
    assert_eq!(after["status"], "pending", "{after}");
    assert!(after["claimed_by"].is_null(), "{after}");
    assert!(!Path::new(&context).exists(), "context file left behind");
    // Owner-only: the brief and the rulings are nobody else's to read.
    assert_eq!(s.read("claude.mode").trim(), "-rw-------");
    // In-process, the agent's own tasqx is pointed at this very store.
    assert_eq!(
        std::fs::canonicalize(s.env_of("claude", "TASQX_DB")).ok(),
        std::fs::canonicalize(&s.db).ok()
    );
    // ...and not at a socket this process did not route through, which may
    // serve another store.
    assert_eq!(
        s.env_of("claude", "TASQX_SOCK"),
        "",
        "an inherited socket leaked"
    );
    // Claude's id is the one `work` chose: no file for codex's (D249).
    assert_eq!(s.env_of("claude", "TASQX_WORK_SESSIONS"), "");

    // The session id and the client are on the start event: completion-time
    // attribution finds the session's transcript through them (D248).
    let log: Value = serde_json::from_str(&s.event_list(r#"{"ref":"1"}"#)).unwrap();
    let start = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"] == "start")
        .unwrap_or_else(|| panic!("no start event: {log}"));
    assert_eq!(start["payload"]["session_id"], session.as_str(), "{start}");
    assert_eq!(start["payload"]["client"], "claude", "{start}");
}

#[test]
fn work_hands_codex_its_context_as_a_toml_string() {
    let s = Store::new("codex");
    s.seed();
    s.fake_agent("codex", 0);

    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");

    let argv = s.read("codex.argv");
    let mut lines = argv.lines();
    assert_eq!(lines.next(), Some("-c"), "{argv}");
    let rest: String = argv.split_once('\n').unwrap().1.to_string();
    // The hook overrides (D249) follow the context.
    let rest = rest
        .split_once("\n-c\nhooks.")
        .map_or(rest.as_str(), |(v, _)| v);
    let value = rest
        .strip_prefix("developer_instructions=")
        .unwrap_or_else(|| panic!("{argv}"));
    // Codex parses the value as TOML: it must come back as the very context.
    let doc: toml_edit::DocumentMut = format!("v = {}", value.trim_end_matches('\n'))
        .parse()
        .expect("a TOML value");
    let text = doc["v"].as_str().expect("a TOML string");
    assert!(text.contains("Fix the login redirect"), "{text}");
    assert!(text.contains("Never log tokens"), "{text}");
    assert_eq!(s.env_of("codex", "TASQX_WORK_TASK"), "1");
    assert!(s.task("1")["claimed_by"].is_null());
}

#[test]
fn a_task_without_checks_tells_the_agent_to_ask_what_done_means() {
    let s = Store::new("nochecks");
    s.ok(&["add", "Something vague"]);
    s.fake_agent("claude", 0);
    let out = s.run(&["work", "1", "--agent", "claude"]);
    assert!(out.status.success(), "{out:?}");
    let ctx = s.read("claude.context");
    assert!(ctx.contains("tasqx_add_check"), "{ctx}");
}

#[test]
fn one_installed_agent_launches_without_a_prompt_even_off_a_terminal() {
    let s = Store::new("one");
    s.seed();
    s.fake_agent("codex", 0);
    let out = s.run(&["work", "1"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!s.read("codex.argv").is_empty(), "codex never ran");
}

#[test]
fn the_remembered_agent_is_used_when_both_are_installed() {
    let s = Store::new("remembered");
    s.seed();
    s.fake_agent("claude", 0);
    s.fake_agent("codex", 0);
    s.ok(&["config", "set", "work.agent", "codex"]);
    let out = s.run(&["work", "1"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!s.read("codex.argv").is_empty(), "codex never ran");
    assert!(s.read("claude.argv").is_empty(), "claude ran");
}

#[test]
fn off_a_terminal_a_needed_prompt_is_refused_naming_the_flag_and_writes_nothing() {
    let s = Store::new("refuse");
    s.seed();
    s.fake_agent("claude", 0);
    s.fake_agent("codex", 0);
    let before = s.events();

    let out = s.run(&["work", "1"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(stderr(&out).contains("--agent claude"), "{}", stderr(&out));

    let out = s.run(&["work", "--agent", "claude"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        stderr(&out).contains("tasqx work <ref>"),
        "{}",
        stderr(&out)
    );

    assert_eq!(s.events(), before, "a refusal wrote to the store");
    let t = s.task("1");
    assert_eq!(t["status"], "pending");
    assert!(t["claimed_by"].is_null());
    assert!(s.read("claude.argv").is_empty() && s.read("codex.argv").is_empty());
}

#[test]
fn a_refusal_before_any_store_exists_creates_none() {
    let s = Store::new("nostore");
    s.fake_agent("claude", 0);
    s.fake_agent("codex", 0);
    let out = s.run(&["work", "1"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(!s.db.exists(), "the refusal created a store");
}

#[test]
fn free_text_is_refused_rather_than_searched() {
    let s = Store::new("freetext");
    s.seed();
    s.fake_agent("claude", 0);
    let out = s.run(&["work", "login bug on mobile", "--agent", "claude"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(stderr(&out).contains("tasqx add"), "{}", stderr(&out));
    assert!(s.read("claude.argv").is_empty());
}

#[test]
fn no_agent_installed_names_both() {
    let s = Store::new("none");
    s.seed();
    let out = s.run(&["work", "1"]);
    assert!(!out.status.success(), "{out:?}");
    let err = stderr(&out);
    assert!(err.contains("claude") && err.contains("codex"), "{err}");
}

#[test]
fn a_spawn_that_fails_still_stops_the_timer_and_releases_the_claim() {
    let s = Store::new("spawnfail");
    s.seed();
    // On PATH, but not executable: found, then refused by the OS.
    std::fs::write(s.bin.join("claude"), "not a program").unwrap();
    let out = s.run(&["work", "1", "--agent", "claude"]);
    assert!(!out.status.success(), "{out:?}");
    let t = s.task("1");
    assert_eq!(t["status"], "pending", "{t}");
    assert!(t["claimed_by"].is_null(), "{t}");
}

/// A `tasqx daemon` process serving `db` on `sock`, killed on drop.
struct Daemon(std::process::Child);

impl Daemon {
    fn start(s: &Store, sock: &str) -> Daemon {
        let child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_CONFIG_DIR", &s.dir)
            .env("TASQX_DB", &s.db)
            .env_remove("TASQX_NOW")
            .args(["--socket", sock, "daemon"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn tasqx daemon");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while tasqx_core::daemon::try_connect(sock).is_none() {
            assert!(std::time::Instant::now() < deadline, "daemon never came up");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Daemon(child)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `tasqx --socket <sock> work 1 --agent claude` through a daemon that is
/// killed while the agent runs, and restarted on the same store when
/// `restart` is set, before the agent exits.
fn work_through_a_lost_daemon(tag: &str, restart: bool) -> (Store, Output, String) {
    let s = Store::new(tag);
    s.seed();
    let sock = s.dir.join("d.sock").display().to_string();
    let marker = s.dir.join("go");
    s.fake_agent_then(
        "claude",
        0,
        &format!(
            "n=0; while [ ! -f {m} ] && [ $n -lt 300 ]; do sleep 0.1; n=$((n+1)); done",
            m = marker.display()
        ),
    );
    let first = Daemon::start(&s, &sock);
    // No TASQX_DB here: the store is the daemon's, and only it can say which.
    let work = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &s.dir)
        .env_remove("TASQX_DB")
        .env_remove("TASQX_SOCK")
        .env_remove("TASQX_NOW")
        .env("PATH", format!("{}:/usr/bin:/bin", s.bin.display()))
        .args(["--socket", &sock, "work", "1", "--agent", "claude"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx work");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while s.read("claude.argv").is_empty() {
        assert!(std::time::Instant::now() < deadline, "the agent never ran");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(first);
    let second = restart.then(|| Daemon::start(&s, &sock));
    std::fs::write(&marker, "").unwrap();
    let out = work.wait_with_output().unwrap();
    drop(second);
    (s, out, sock)
}

/// Routed through a daemon: the agent is told the same socket, and when the
/// connection `work` opened is gone by the time the agent exits (the daemon
/// drops idle clients after 15 minutes; here it is restarted mid-session),
/// cleanup reconnects: the timer is stopped and the claim released.
#[test]
fn through_a_daemon_the_agent_gets_the_socket_and_cleanup_survives_a_lost_connection() {
    let (s, out, sock) = work_through_a_lost_daemon("daemon", true);
    assert!(out.status.success(), "{out:?}");
    let t = s.task("1");
    assert_eq!(t["status"], "pending", "timer left running: {t}");
    assert!(t["claimed_by"].is_null(), "claim left held: {t}");
    assert!(!stderr(&out).contains("tasqx stop"), "{}", stderr(&out));
    assert_eq!(s.env_of("claude", "TASQX_SOCK"), sock);
    // `tasqx mcp serve` opens TASQX_DB and never the socket, so the agent is
    // told the daemon's store file too.
    let told = std::fs::canonicalize(s.env_of("claude", "TASQX_DB")).ok();
    assert_eq!(told, std::fs::canonicalize(&s.db).ok(), "TASQX_DB");
}

/// A cleanup that cannot reach the store says so, naming the commands that
/// finish the job, instead of leaving the timer running in silence.
#[test]
fn a_cleanup_that_cannot_reach_the_store_names_stop_and_release() {
    let (_s, out, _) = work_through_a_lost_daemon("daemon-gone", false);
    let err = stderr(&out);
    assert!(err.contains("tasqx stop 1"), "{err}");
    assert!(err.contains("tasqx release 1"), "{err}");
}

/// An agent that closes the task itself leaves nothing to stop or release:
/// the refusals cleanup then meets are no warning.
#[test]
fn an_agent_that_finishes_the_task_itself_leaves_cleanup_silent() {
    let s = Store::new("selfdone");
    s.seed();
    let tasqx = env!("CARGO_BIN_EXE_tasqx");
    s.fake_agent_then(
        "claude",
        0,
        &format!("{tasqx} --no-daemon done \"$TASQX_WORK_TASK\" --force-claim >/dev/null"),
    );
    let out = s.run(&["work", "1", "--agent", "claude"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.task("1")["status"], "done");
    assert!(!stderr(&out).contains("warning"), "{}", stderr(&out));
}

/// `--json` is a declared carve-out: the agent owns stdout, so nothing is
/// appended after what it printed, and stderr says why.
#[test]
fn json_leaves_stdout_to_the_agent() {
    let s = Store::new("json");
    s.seed();
    s.fake_agent_then("claude", 0, "echo agent-output");
    let out = s.run(&["--json", "work", "1", "--agent", "claude"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "agent-output\n");
    assert!(
        stderr(&out).contains("`work` does not honour --json"),
        "{}",
        stderr(&out)
    );
}

/// A relative TASQX_DB reaches the agent made absolute: an agent that runs
/// `cd subdir && tasqx …` must still open this store, not a new one there.
#[test]
fn a_relative_store_path_reaches_the_agent_absolute() {
    let s = Store::new("relative");
    s.seed();
    s.fake_agent("claude", 0);
    let out = s
        .tasqx()
        .current_dir(&s.dir)
        .env("TASQX_DB", "tasks.db")
        .args(["work", "1", "--agent", "claude"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let told = s.env_of("claude", "TASQX_DB");
    assert!(Path::new(&told).is_absolute(), "{told:?}");
    assert_eq!(
        std::fs::canonicalize(&told).ok(),
        std::fs::canonicalize(&s.db).ok()
    );
}

/// `tasqx hook claude` as Claude Code runs it: the hook JSON on stdin, the
/// work session's env (or none) around it.
fn hook(s: &Store, env: &[(&str, &str)], stdin: &str) -> Output {
    hook_as(s, "claude", env, stdin)
}

fn hook_as(s: &Store, agent: &str, env: &[(&str, &str)], stdin: &str) -> Output {
    use std::io::Write;
    let mut c = s.tasqx();
    c.env_remove("TASQX_WORK_TASK")
        .env_remove("TASQX_WORK_SESSION")
        .env_remove("TASQX_WORK_SESSIONS")
        .envs(env.iter().copied())
        .args(["hook", agent])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().expect("spawn tasqx hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

const SESSION: &str = "0199f0e2-0000-7000-8000-000000000001";

fn hook_json(session: &str, event: &str) -> String {
    format!(
        r#"{{"session_id":"{session}","transcript_path":"/tmp/x/{session}.jsonl","hook_event_name":"{event}","cwd":"/tmp"}}"#
    )
}

/// Silent, exit 0, nothing written: what every hook outcome but a fault is.
fn assert_silent(s: &Store, out: &Output, before: usize) {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(stderr(out), "", "{out:?}");
    assert_eq!(s.events(), before, "a hook wrote to the store");
}

/// D248: in the session it belongs to, SessionStart and SessionEnd are
/// entry points that record nothing yet; tokens come from completion-time
/// attribution, not from the hook.
#[test]
fn the_hook_in_its_own_session_accepts_start_and_end_and_writes_nothing() {
    let s = Store::new("hook-match");
    s.seed();
    let before = s.events();
    let env = [("TASQX_WORK_TASK", "1"), ("TASQX_WORK_SESSION", SESSION)];
    for event in ["SessionStart", "SessionEnd"] {
        let out = hook(&s, &env, &hook_json(SESSION, event));
        assert_silent(&s, &out, before);
    }
}

/// Another session's hook (a nested `claude`, a stale env) is not this
/// session's business: silent, even for an event it would complain about.
#[test]
fn the_hook_for_another_session_is_silent() {
    let s = Store::new("hook-other");
    s.seed();
    let before = s.events();
    let env = [("TASQX_WORK_TASK", "1"), ("TASQX_WORK_SESSION", SESSION)];
    let other = "0199f0e2-0000-7000-8000-000000000002";
    let out = hook(&s, &env, &hook_json(other, "NoSuchEvent"));
    assert_silent(&s, &out, before);
    // No session in the JSON at all is not this session either.
    let out = hook(&s, &env, r#"{"hook_event_name":"NoSuchEvent"}"#);
    assert_silent(&s, &out, before);
    // Nor is a session-less JSON when no session was started.
    let out = hook(
        &s,
        &[("TASQX_WORK_TASK", "1")],
        r#"{"hook_event_name":"NoSuchEvent"}"#,
    );
    assert_silent(&s, &out, before);
}

/// Outside a `tasqx work` session the hook does nothing, not even read its
/// input: a hook left in someone's settings never complains.
#[test]
fn the_hook_outside_a_work_session_is_silent_whatever_it_is_fed() {
    let s = Store::new("hook-outside");
    s.seed();
    let before = s.events();
    let out = hook(&s, &[], "not json at all");
    assert_silent(&s, &out, before);
}

/// A fault never breaks the agent: unreadable input or an event this build
/// does not know is said on stderr, and the exit is still 0.
#[test]
fn a_hook_fault_goes_to_stderr_and_still_exits_zero() {
    let s = Store::new("hook-fault");
    s.seed();
    let before = s.events();
    let env = [("TASQX_WORK_TASK", "1"), ("TASQX_WORK_SESSION", SESSION)];
    for input in [
        "not json at all".to_string(),
        hook_json(SESSION, "NoSuchEvent"),
    ] {
        let out = hook(&s, &env, &input);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert!(stderr(&out).contains("tasqx hook claude"), "{out:?}");
        assert_eq!(s.events(), before);
    }
    assert!(stderr(&hook(&s, &env, &hook_json(SESSION, "NoSuchEvent"))).contains("NoSuchEvent"));
}

/// The `-c` overrides after `developer_instructions`, one per line of argv.
fn codex_overrides(s: &Store) -> Vec<String> {
    let argv = s.read("codex.argv");
    let lines: Vec<&str> = argv.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("hooks."))
        .unwrap_or_else(|| panic!("no hooks override: {argv}"));
    assert_eq!(lines[at - 1], "-c", "{argv}");
    lines[at..]
        .iter()
        .filter(|l| **l != "-c")
        .map(|l| l.to_string())
        .collect()
}

/// D249: codex gets one constant hook command on SessionStart and
/// SessionEnd, byte-identical from run to run since codex trusts it by hash,
/// and never the trust bypass; `~/.codex` is not written.
#[test]
fn codex_gets_one_constant_hook_command_and_its_home_is_untouched() {
    let s = Store::new("codex-hooks");
    s.seed();
    s.fake_agent("codex", 0);
    let out = s.run(&["work", "1", "--agent", "codex"]);
    // Codex started no session here, so there is no hook to hint about.
    assert!(!stderr(&out).contains("/hooks"), "{out:?}");
    let first = codex_overrides(&s);
    s.ok(&["work", "1", "--agent", "codex"]);
    assert_eq!(codex_overrides(&s), first, "the hook definition varied");
    assert_eq!(first.len(), 2, "{first:?}");
    for (event, line) in ["SessionStart", "SessionEnd"].iter().zip(&first) {
        // Codex parses `-c key=value` as TOML.
        let doc: toml_edit::DocumentMut = line.parse().expect("a TOML override");
        let groups = doc["hooks"][*event]
            .as_array()
            .unwrap_or_else(|| panic!("{line}"));
        let handler = groups.get(0).unwrap().as_inline_table().unwrap()["hooks"]
            .as_array()
            .unwrap()
            .get(0)
            .unwrap()
            .as_inline_table()
            .unwrap()
            .clone();
        assert_eq!(handler["type"].as_str(), Some("command"), "{line}");
        assert_eq!(
            handler["command"].as_str(),
            Some("tasqx hook codex"),
            "{line}"
        );
    }
    assert!(!s.read("codex.argv").contains("dangerously"));
    assert!(!dir_home(&s.dir).join(".codex").exists());
}

/// A shell line that writes a rollout as codex would: `session_meta` with
/// the id, this directory and the time now, under `$HOME/.codex/sessions`.
fn rollout_sh(id: &str) -> String {
    format!(
        "d=\"$HOME/.codex/sessions/2026/10/27\"; mkdir -p \"$d\"; \
         ts=$(perl -MPOSIX -MTime::HiRes=time -e '$t=time; printf \"%s.%03dZ\", strftime(\"%Y-%m-%dT%H:%M:%S\", gmtime $t), ($t-int $t)*1000'); \
         printf '{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"%s\",\"timestamp\":\"%s\"}}}}\\n' \"$(pwd -P)\" \"$ts\" > \"$d/rollout-{id}.jsonl\""
    )
}

/// The session ids on the task's start events, in order, with their client.
fn start_sessions(s: &Store) -> Vec<(String, String)> {
    let log: Value = serde_json::from_str(&s.event_list(r#"{"ref":"1","limit":500}"#)).unwrap();
    let mut events: Vec<&Value> = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["op"] == "start")
        .collect();
    events.sort_by_key(|e| e["id"].as_str().map(str::to_string));
    events
        .iter()
        .map(|e| {
            let p = &e["payload"];
            let field = |k: &str| p[k].as_str().unwrap_or("").to_string();
            (field("session_id"), field("client"))
        })
        .collect()
}

const CODEX_ID: &str = "01a17f55-9a67-7162-b594-48bd262d962b";

/// D249: a trusted hook tells tasqx codex's own session id on SessionStart,
/// and that id is recorded on a start of the task, where D247's fallback
/// finds the rollout by `session_meta.id`. No hint: the hook ran.
#[test]
fn a_trusted_codex_hook_records_codexs_own_session_id() {
    let s = Store::new("codex-trusted");
    s.seed();
    let tasqx = env!("CARGO_BIN_EXE_tasqx");
    let json = format!(
        r#"{{"session_id":"{CODEX_ID}","hook_event_name":"SessionStart","cwd":"/x","transcript_path":null}}"#
    );
    // A rollout of another id beside it: the hook's word wins over a match.
    let then = format!(
        "printf '%s' '{json}' | {tasqx} hook codex; {}",
        rollout_sh("not-this-one")
    );
    s.fake_agent_then("codex", 0, &then);
    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!stderr(&out).contains("/hooks"), "{out:?}");
    let starts = start_sessions(&s);
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1], (CODEX_ID.to_string(), "codex".to_string()));
    let after = s.task("1");
    assert_eq!(after["status"], "pending", "{after}");
    assert!(after["claimed_by"].is_null(), "{after}");
    // The record file is gone with the context file.
    let sessions = s.env_of("codex", "TASQX_WORK_SESSIONS");
    assert!(
        !sessions.is_empty() && !Path::new(&sessions).exists(),
        "{sessions:?}"
    );
}

/// D249: an untrusted hook is skipped silently by codex. After it exits,
/// tasqx finds the rollout its launch wrote (this directory, started since
/// the launch) and records that id; and says once how to trust the hook.
#[test]
fn an_untrusted_codex_hook_falls_back_to_the_rollout_and_hints_once() {
    let s = Store::new("codex-untrusted");
    s.seed();
    // A clock the user started in their shell meanwhile keeps running: the
    // record after exit stops no other clock.
    s.ok(&["add", "Something else"]);
    let tasqx = env!("CARGO_BIN_EXE_tasqx");
    let then = format!(
        "{tasqx} --no-daemon start 2 >/dev/null; {}",
        rollout_sh(CODEX_ID)
    );
    s.fake_agent_then("codex", 0, &then);
    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.task("2")["status"], "active");
    let err = stderr(&out);
    assert!(
        err.contains("/hooks") && err.contains("tasqx hook codex"),
        "{err}"
    );
    let starts = start_sessions(&s);
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1], (CODEX_ID.to_string(), "codex".to_string()));
    let after = s.task("1");
    assert_eq!(after["status"], "pending", "{after}");
    assert!(after["claimed_by"].is_null(), "{after}");

    // Next run: the earlier rollout started before this launch and is not
    // its session; the new one is. The hint is not repeated.
    let second = "01a17f60-0000-7000-8000-000000000002";
    s.fake_agent_then("codex", 0, &rollout_sh(second));
    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!stderr(&out).contains("/hooks"), "{out:?}");
    let starts = start_sessions(&s);
    assert_eq!(starts.len(), 4, "{starts:?}");
    assert_eq!(starts[3].0, second);
}

/// Two codex sessions in this directory since the launch (another codex
/// running beside it): which one was the launch's is unknown, so neither is
/// recorded, and stderr says so.
#[test]
fn two_candidate_rollouts_record_neither() {
    let s = Store::new("codex-ambiguous");
    s.seed();
    let then = format!("{}; {}", rollout_sh(CODEX_ID), rollout_sh("beside-it"));
    s.fake_agent_then("codex", 0, &then);
    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(start_sessions(&s).len(), 1);
    assert!(stderr(&out).contains("2 codex sessions"), "{out:?}");
}

/// `tasqx hook codex`: on SessionStart, codex's id is appended to the file
/// `tasqx work` named; SessionEnd adds nothing; outside a work session or
/// without that file it is silent; a fault is a stderr line and exit 0.
#[test]
fn the_codex_hook_appends_its_session_and_never_breaks_the_agent() {
    let s = Store::new("hook-codex");
    s.seed();
    let before = s.events();
    let file = s.dir.join("sessions");
    std::fs::write(&file, "").unwrap();
    let f = file.to_str().unwrap();
    let env = [("TASQX_WORK_TASK", "1"), ("TASQX_WORK_SESSIONS", f)];
    let codex = |s: &Store, env: &[(&str, &str)], input: &str| hook_as(s, "codex", env, input);

    assert_silent(
        &s,
        &codex(&s, &env, &hook_json(CODEX_ID, "SessionStart")),
        before,
    );
    assert_silent(
        &s,
        &codex(&s, &env, &hook_json("ended", "SessionEnd")),
        before,
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{CODEX_ID}\n")
    );
    // Outside a work session, or with no file named, nothing is read.
    assert_silent(&s, &codex(&s, &[], "not json"), before);
    assert_silent(
        &s,
        &codex(
            &s,
            &[("TASQX_WORK_TASK", "1")],
            &hook_json("x", "SessionStart"),
        ),
        before,
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{CODEX_ID}\n")
    );

    // An empty id is a fault, and nothing is appended.
    let out = codex(
        &s,
        &env,
        r#"{"hook_event_name":"SessionStart","session_id":""}"#,
    );
    assert!(stderr(&out).contains("session_id"), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        format!("{CODEX_ID}\n")
    );

    // Faults: unreadable input, no session id, an unknown event, a file
    // that is gone (never created by the hook).
    std::fs::remove_file(&file).unwrap();
    for input in [
        "not json".to_string(),
        r#"{"hook_event_name":"SessionStart"}"#.to_string(),
        hook_json(CODEX_ID, "NoSuchEvent"),
        hook_json(CODEX_ID, "SessionStart"),
    ] {
        let out = codex(&s, &env, &input);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(
            stderr(&out).contains("tasqx hook codex"),
            "{input}: {out:?}"
        );
    }
    assert!(!file.exists(), "the hook created the file");
    assert_eq!(s.events(), before);
}

/// The payload of the task's `done` event.
fn done_payload(s: &Store, r: &str) -> Value {
    let log: Value =
        serde_json::from_str(&s.event_list(&format!(r#"{{"ref":"{r}","limit":500}}"#))).unwrap();
    log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"] == "done")
        .unwrap_or_else(|| panic!("no done event: {log}"))["payload"]
        .clone()
}

/// D249: the agent completes the task through its shell before codex exits.
/// The post-exit record would come too late for a done task, so the `done`
/// itself names codex and the id its trusted hook reported.
#[test]
fn a_done_inside_a_codex_session_carries_codex_and_its_hooked_id() {
    let s = Store::new("codex-done");
    s.seed();
    let tasqx = env!("CARGO_BIN_EXE_tasqx");
    let json = format!(r#"{{"session_id":"{CODEX_ID}","hook_event_name":"SessionStart"}}"#);
    let then = format!("printf '%s' '{json}' | {tasqx} hook codex; {}", done_sh(&s));
    s.fake_agent_then("codex", 0, &then);
    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.task("1")["status"], "done", "{}", s.read("done.out"));
    let p = done_payload(&s, "1");
    assert_eq!(p["client"], "codex", "{p}");
    assert_eq!(p["session_id"], CODEX_ID, "{p}");
}

/// `tasqx done` as the agent's shell runs it, inside a codex work session on
/// task 1 whose sessions file holds `hooked`.
fn done_in_session(s: &Store, hooked: &str, args: &[&str]) -> Output {
    let file = s.dir.join("sessions");
    std::fs::write(&file, hooked).unwrap();
    s.tasqx()
        .env("TASQX_WORK_TASK", "1")
        .env("TASQX_WORK_SESSIONS", &file)
        .arg("done")
        .args(args)
        .output()
        .unwrap()
}

/// Untrusted hook and no rollout to match: the launch's session is unknown,
/// so nothing is added. A client alone would read every codex rollout in the
/// window, other projects' too: under-counting beats over-counting.
#[test]
fn a_done_inside_a_codex_session_without_a_known_session_adds_nothing() {
    let s = Store::new("codex-done-nohook");
    s.seed();
    let out = done_in_session(&s, "", &["1"]);
    assert!(out.status.success(), "{out:?}");
    let p = done_payload(&s, "1");
    assert!(p["client"].is_null() && p["session_id"].is_null(), "{p}");
}

/// Correlation the caller passed is never overridden, and a done of another
/// task inside the session gets nothing added.
#[test]
fn a_done_inside_a_codex_session_keeps_explicit_correlation_and_other_tasks_alone() {
    let s = Store::new("codex-done-explicit");
    s.seed();
    s.ok(&["add", "Something else"]);
    let hooked = format!("{CODEX_ID}\n");
    let out = done_in_session(
        &s,
        &hooked,
        &["1", "--client", "mine", "--session-id", "s1"],
    );
    assert!(out.status.success(), "{out:?}");
    let p = done_payload(&s, "1");
    assert_eq!(
        (&p["client"], &p["session_id"]),
        (&"mine".into(), &"s1".into()),
        "{p}"
    );

    let out = done_in_session(&s, &hooked, &["2"]);
    assert!(out.status.success(), "{out:?}");
    let p = done_payload(&s, "2");
    assert!(p["client"].is_null() && p["session_id"].is_null(), "{p}");
}

/// A shell line that completes the work task as the agent would, as the
/// claim's holder, output into `<dir>/done.out`.
fn done_sh(s: &Store) -> String {
    let tasqx = env!("CARGO_BIN_EXE_tasqx");
    format!(
        "actor=$({tasqx} --no-daemon --json show 1 | sed -n 's/.*\"claimed_by\": *\"\\([^\"]*\\)\".*/\\1/p'); \
         {tasqx} --no-daemon done \"$TASQX_WORK_TASK\" --holder \"$actor\" > {}/done.out 2>&1",
        s.dir.display()
    )
}

/// D249: correlation on the done would turn off D247's measure of every
/// session the task's starts named since its previous done. So a task that
/// an earlier Claude session worked on gets none added: the fallback still
/// sees that session, and the in-session codex part goes unmeasured.
#[test]
fn a_done_inside_codex_adds_nothing_when_an_earlier_session_worked_the_task() {
    let s = Store::new("codex-done-mixed");
    s.seed();
    s.ok(&["start", "1", "--client", "claude", "--session-id", "c1"]);
    s.ok(&["stop", "1"]);
    let tasqx = env!("CARGO_BIN_EXE_tasqx");
    let json = format!(r#"{{"session_id":"{CODEX_ID}","hook_event_name":"SessionStart"}}"#);
    let then = format!("printf '%s' '{json}' | {tasqx} hook codex; {}", done_sh(&s));
    s.fake_agent_then("codex", 0, &then);
    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.task("1")["status"], "done", "{}", s.read("done.out"));
    let p = done_payload(&s, "1");
    assert!(p["client"].is_null() && p["session_id"].is_null(), "{p}");
    // The fallback's input: `c1` on a start since the previous done.
    assert!(start_sessions(&s)
        .iter()
        .any(|(sid, client)| sid == "c1" && client == "claude"));
}

/// A `tasqx work` run inside a codex work session does not hand the outer
/// session's file on: a claude child's `tasqx done` must not read as codex.
#[test]
fn a_nested_launch_does_not_inherit_the_codex_sessions_file() {
    let s = Store::new("nested");
    s.seed();
    s.fake_agent("claude", 0);
    let out = s
        .tasqx()
        .env("TASQX_WORK_SESSIONS", s.dir.join("outer.sessions"))
        .args(["work", "1", "--agent", "claude"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.env_of("claude", "TASQX_WORK_SESSIONS"), "");
}

/// A session before the task's previous done was banked by that done: it is
/// not "earlier" for this one, and the codex correlation is added.
#[test]
fn a_session_before_the_previous_done_does_not_hold_the_correlation_back() {
    let s = Store::new("codex-done-reopened");
    s.seed();
    s.ok(&["start", "1", "--client", "claude", "--session-id", "c1"]);
    s.ok(&["done", "1"]);
    s.ok(&["reopen", "1"]);
    let out = done_in_session(&s, &format!("{CODEX_ID}\n"), &["1"]);
    assert!(out.status.success(), "{out:?}");
    let log: Value = serde_json::from_str(&s.event_list(r#"{"ref":"1","limit":500}"#)).unwrap();
    let last_done = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"] == "done")
        .unwrap()["payload"]
        .clone();
    assert_eq!(last_done["client"], "codex", "{last_done}");
    assert_eq!(last_done["session_id"], CODEX_ID, "{last_done}");
}

/// Two distinct sessions hooked in one launch (a `/new` inside codex): a
/// done naming one would leave the other out, and naming codex alone would
/// read other projects' rollouts, so nothing is added. The same id hooked
/// twice (a resume) is still one session.
#[test]
fn a_done_after_two_hooked_sessions_adds_nothing() {
    let s = Store::new("codex-done-two");
    s.seed();
    s.ok(&["add", "Something else"]);
    let out = done_in_session(&s, &format!("{CODEX_ID}\nsecond\n"), &["1"]);
    assert!(out.status.success(), "{out:?}");
    let p = done_payload(&s, "1");
    assert!(p["client"].is_null() && p["session_id"].is_null(), "{p}");

    let file = s.dir.join("sessions");
    std::fs::write(&file, format!("{CODEX_ID}\n{CODEX_ID}\n")).unwrap();
    let out = s
        .tasqx()
        .env("TASQX_WORK_TASK", "2")
        .env("TASQX_WORK_SESSIONS", &file)
        .args(["done", "2"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(done_payload(&s, "2")["session_id"], CODEX_ID);
}

/// A rollout as codex writes it, under the store's `$HOME/.codex`, started
/// in `cwd` at `ts`.
fn write_rollout(s: &Store, id: &str, cwd: &Path, ts: &str) {
    let day = dir_home(&s.dir).join(".codex/sessions/2026/10/28");
    std::fs::create_dir_all(&day).unwrap();
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": id, "cwd": cwd, "timestamp": ts },
    });
    std::fs::write(day.join(format!("rollout-{id}.jsonl")), format!("{meta}\n")).unwrap();
}

/// `tasqx done 1` inside a codex launch on task 1 whose hook wrote nothing,
/// launched in `launch_cwd` at `since`, run from another directory (the
/// agent may have changed it).
fn done_unhooked(s: &Store, launch_cwd: &Path, since: &str) -> Value {
    let file = s.dir.join("sessions");
    std::fs::write(&file, "").unwrap();
    let elsewhere = s.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = s
        .tasqx()
        .current_dir(&elsewhere)
        .env("TASQX_WORK_TASK", "1")
        .env("TASQX_WORK_SESSIONS", &file)
        .env("TASQX_WORK_CWD", launch_cwd)
        .env("TASQX_WORK_SINCE", since)
        .args(["done", "1"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    done_payload(s, "1")
}

const SINCE: &str = "2020-01-01T10:00:00Z";

/// D249: an untrusted hook leaves the id to the rollout match the post-exit
/// step uses, run at done time with the launch's directory and start: one
/// match is the launch's session.
#[test]
fn an_unhooked_done_names_the_one_rollout_of_the_launch() {
    let s = Store::new("codex-done-rollout");
    s.seed();
    let launch = s.dir.join("project");
    std::fs::create_dir_all(&launch).unwrap();
    write_rollout(&s, CODEX_ID, &launch, "2020-01-01T10:00:01Z");
    // Another project's codex, and one started before the launch here.
    write_rollout(
        &s,
        "elsewhere",
        &s.dir.join("elsewhere"),
        "2020-01-01T10:00:02Z",
    );
    write_rollout(&s, "before", &launch, "2020-01-01T09:59:59Z");
    let p = done_unhooked(&s, &launch, SINCE);
    assert_eq!(p["client"], "codex", "{p}");
    assert_eq!(p["session_id"], CODEX_ID, "{p}");
}

/// Two rollouts of this launch's directory since it started: which one is
/// the launch's is unknown, so nothing is added.
#[test]
fn an_unhooked_done_with_two_matching_rollouts_adds_nothing() {
    let s = Store::new("codex-done-rollout-two");
    s.seed();
    let launch = s.dir.join("project");
    std::fs::create_dir_all(&launch).unwrap();
    write_rollout(&s, CODEX_ID, &launch, "2020-01-01T10:00:01Z");
    write_rollout(&s, "beside-it", &launch, "2020-01-01T10:00:02Z");
    let p = done_unhooked(&s, &launch, SINCE);
    assert!(p["client"].is_null() && p["session_id"].is_null(), "{p}");
}

/// A nested launch clears the launch directory and start too.
#[test]
fn a_nested_launch_does_not_inherit_the_launch_directory_or_start() {
    let s = Store::new("nested-cwd");
    s.seed();
    s.fake_agent("claude", 0);
    let out = s
        .tasqx()
        .env("TASQX_WORK_CWD", "/outer")
        .env("TASQX_WORK_SINCE", SINCE)
        .args(["work", "1", "--agent", "claude"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.env_of("claude", "TASQX_WORK_CWD"), "");
    assert_eq!(s.env_of("claude", "TASQX_WORK_SINCE"), "");
}

/// End to end: an untrusted hook, the agent finishes the task before codex
/// exits, and `work` handed the done its launch directory and start, so the
/// done names the rollout codex wrote for this launch.
#[test]
fn an_unhooked_codex_session_finishing_its_task_names_its_rollout() {
    let s = Store::new("codex-done-unhooked-e2e");
    s.seed();
    let then = format!("{}; {}", rollout_sh(CODEX_ID), done_sh(&s));
    s.fake_agent_then("codex", 0, &then);
    let out = s.run(&["work", "1", "--agent", "codex"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(s.task("1")["status"], "done", "{}", s.read("done.out"));
    let p = done_payload(&s, "1");
    assert_eq!(p["client"], "codex", "{p}");
    assert_eq!(p["session_id"], CODEX_ID, "{p}");
}

/// D249: the cycle boundary follows the store's append order. A previous
/// done synced in from a machine whose clock runs ahead sorts above this
/// cycle's Claude start by id; walked in id order, the boundary came first
/// and `c1` was missed, so the done named codex and turned off D247's
/// measure of `c1`.
#[test]
fn a_done_inside_codex_finds_an_earlier_session_past_a_done_whose_id_sorts_ahead() {
    let s = Store::new("codex-done-synced");
    s.seed();
    s.ok(&["done", "1"]);
    s.ok(&["reopen", "1"]);
    s.ok(&["start", "1", "--client", "claude", "--session-id", "c1"]);
    s.ok(&["stop", "1"]);
    let engine = tasqx_core::engine::Engine::open(s.db.to_str().unwrap()).unwrap();
    engine
        .conn()
        .execute(
            "UPDATE events SET id = 'ffffffff-ffff-7fff-bfff-ffffffffffff' WHERE op = 'done'",
            [],
        )
        .unwrap();
    drop(engine);

    let out = done_in_session(&s, &format!("{CODEX_ID}\n"), &["1"]);
    assert!(out.status.success(), "{out:?}");
    let log: Value =
        serde_json::from_str(&s.event_list(r#"{"ref":"1","limit":500,"order":"append"}"#)).unwrap();
    let last_done = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"] == "done")
        .unwrap()["payload"]
        .clone();
    assert!(
        last_done["client"].is_null() && last_done["session_id"].is_null(),
        "{last_done}"
    );
}

/// D249 with #4203: a task reopened and completed again in the same codex
/// session would name that session again, and a correlated done measures
/// from the task's earliest start, so the first cycle would count twice.
/// The second done adds nothing: it under-counts until #4203.
#[test]
fn a_second_done_in_the_same_codex_session_adds_nothing() {
    let s = Store::new("codex-done-again");
    s.seed();
    let hooked = format!("{CODEX_ID}\n");
    let out = done_in_session(&s, &hooked, &["1"]);
    assert!(out.status.success(), "{out:?}");
    s.ok(&["reopen", "1"]);
    let out = done_in_session(&s, &hooked, &["1"]);
    assert!(out.status.success(), "{out:?}");
    let log: Value =
        serde_json::from_str(&s.event_list(r#"{"ref":"1","limit":500,"order":"append"}"#)).unwrap();
    let dones: Vec<&Value> = log["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["op"] == "done")
        .map(|e| &e["payload"])
        .collect();
    assert_eq!(
        dones[1]["session_id"], CODEX_ID,
        "the first done: {dones:?}"
    );
    assert!(
        dones[0]["client"].is_null() && dones[0]["session_id"].is_null(),
        "the second done: {dones:?}"
    );
}
