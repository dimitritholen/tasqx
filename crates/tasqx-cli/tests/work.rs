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
    let argv: Vec<String> = s.read("claude.argv").lines().map(str::to_string).collect();
    assert_eq!(
        argv,
        [
            "--session-id",
            session.as_str(),
            "--append-system-prompt-file",
            context.as_str()
        ],
    );

    // The start context: the brief, its checks and the project's ruling.
    let ctx = s.read("claude.context");
    assert!(ctx.contains("Fix the login redirect"), "{ctx}");
    assert!(ctx.contains("the redirect lands on /home"), "{ctx}");
    assert!(ctx.contains("Never log tokens"), "{ctx}");
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
    assert_eq!(s.env_of("claude", "TASQX_DB"), s.db.display().to_string());

    // The session id is on the start event, for later token attribution.
    let log = s.event_list(r#"{"ref":"1"}"#);
    assert!(log.contains(&session), "session id not recorded: {log}");
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
    let work = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &s.dir)
        .env("TASQX_DB", &s.db)
        .env_remove("TASQX_SOCK")
        .env_remove("TASQX_NOW")
        .env("PATH", format!("{}:/usr/bin:/bin", s.bin.display()))
        .args(["--socket", &sock, "work", "1", "--agent", "claude"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx work");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while s.read("claude.during").is_empty() {
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
