//! `tasqx sync` through the real binary and the real `tasqx-remote-dir`
//! connector (D201): two scratch stores standing in for two machines, one
//! shared folder standing in for the remote.
//!
//! Every machine is its own directory holding its store and its
//! `config.toml`, and every run carries `TASQX_DB`, `TASQX_CONFIG_DIR` and
//! `--no-daemon`, so nothing here reaches the developer's own store, config or
//! daemon. Two directories rather than two files side by side because the
//! connector's state (D198, `remote::state_dir`) lives beside the store: two
//! machines never share it, and a test that let them would hide a leak.
//!
//! The connector is a sibling workspace binary, so this suite finds it next
//! to `tasqx` in cargo's target directory — `cargo test --workspace` builds
//! every package's binaries before it runs any test — and copies it into a
//! `bin` directory of the test's own that goes first on `PATH`. The retry,
//! conflict and mid-push-failure paths need a remote that misbehaves on cue;
//! those are unit tests over a fake remote in `src/sync.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const CONNECTOR: &str = "tasqx-remote-dir";

/// A fresh scratch dir of this test's own, named per tag and per process.
fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("tasqx-sync-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// The connector binary cargo built beside `tasqx`.
fn connector_binary() -> PathBuf {
    let tasqx = Path::new(env!("CARGO_BIN_EXE_tasqx"));
    let name = format!("{CONNECTOR}{}", std::env::consts::EXE_SUFFIX);
    // An integration test's CARGO_BIN_EXE_* points into target/<profile>/.
    let found = tasqx.with_file_name(&name);
    assert!(
        found.is_file(),
        "{} is missing: run the suite with `cargo test --workspace` (or \
         `cargo build -p {CONNECTOR}` first), so the connector is built beside tasqx",
        found.display()
    );
    found
}

/// One test's world: a `bin` dir holding the connector, a remote folder, and
/// the machines.
struct World {
    root: PathBuf,
    path_var: std::ffi::OsString,
}

impl World {
    fn new(tag: &str) -> World {
        let root = scratch(tag);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("create bin dir");
        let exe = format!("{CONNECTOR}{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(connector_binary(), bin.join(exe)).expect("copy the connector");
        std::fs::create_dir_all(root.join("remote")).expect("create the remote folder");
        let mut dirs = vec![bin];
        dirs.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path_var = std::env::join_paths(dirs).expect("join PATH");
        World { root, path_var }
    }

    fn remote(&self) -> PathBuf {
        self.root.join("remote")
    }

    fn machine(&self, name: &str) -> Machine<'_> {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).expect("create machine dir");
        Machine { world: self, dir }
    }
}

struct Machine<'w> {
    world: &'w World,
    dir: PathBuf,
}

impl Machine<'_> {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_DB", self.dir.join("tasks.db"))
            .env("TASQX_CONFIG_DIR", self.dir.join("cfg"))
            .env("PATH", &self.world.path_var)
            .env_remove("TASQX_SOCK")
            .env_remove("TASQX_NOW")
            .env("NO_COLOR", "1")
            .arg("--no-daemon")
            .args(args);
        c
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out: Output = self.cmd(args).output().expect("run tasqx");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Run and insist on exit 0, returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 0, "tasqx {args:?} failed:\n{out}\n{err}");
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_str(&self.ok(&all)).expect("--json prints JSON")
    }

    fn set_up(&self) {
        let path = format!("path={}", self.world.remote().display());
        self.ok(&["sync", "setup", "dir", "--set", &path]);
    }

    fn config_file(&self) -> PathBuf {
        self.dir.join("cfg").join("config.toml")
    }

    fn export(&self) -> Value {
        self.json(&["export"])
    }
}

/// The one task in an export with this title.
fn task<'a>(export: &'a Value, title: &str) -> &'a Value {
    export["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|t| t["title"] == title)
        .unwrap_or_else(|| panic!("no task {title:?} in {export}"))
}

/// An export minus what each store keeps for itself: a task's `_rev` (its
/// own concurrency counter, which a merge never takes from the payload) and
/// its `urgency` (scored at the instant of export).
fn without_local_fields(mut export: Value) -> Value {
    for t in export["tasks"].as_array_mut().expect("tasks array") {
        let t = t.as_object_mut().expect("task object");
        t.remove("_rev");
        t.remove("urgency");
    }
    export
}

#[test]
fn status_says_not_set_up_until_setup_and_then_names_the_connector_and_the_last_sync() {
    let world = World::new("status");
    let a = world.machine("a");

    let before = a.json(&["sync", "--status"]);
    assert_eq!(before["set_up"], false, "{before}");
    assert_eq!(before["connector"], Value::Null, "{before}");
    assert!(
        a.ok(&["sync", "--status"]).contains("not set up"),
        "the human form says so"
    );

    a.set_up();
    let configured = a.json(&["sync", "--status"]);
    assert_eq!(configured["set_up"], true, "{configured}");
    assert_eq!(configured["connector"], "dir", "{configured}");
    assert_eq!(configured["version"], Value::Null, "never synced yet");
    assert!(a.ok(&["sync", "--status"]).contains("never synced"));

    a.ok(&["init", "home"]);
    let synced = a.json(&["sync"]);
    let after = a.json(&["sync", "--status"]);
    assert_eq!(after["version"], synced["version"], "{after}");
    assert!(after["synced_at"].is_string(), "{after}");
    let text = a.ok(&["sync", "--status"]);
    assert!(
        text.contains("today"),
        "a relative day, not an instant: {text}"
    );
    let short: String = synced["version"]
        .as_str()
        .unwrap()
        .chars()
        .take(8)
        .collect();
    assert!(text.contains(&short), "the version, short: {text}");
}

#[test]
fn setup_with_set_writes_the_sync_table_and_a_refusal_leaves_config_untouched() {
    let world = World::new("setup");
    let a = world.machine("a");

    // Refused by the connector: a relative folder. Its words reach the user,
    // and config.toml is not written.
    let (code, out, err) = a.run(&["sync", "setup", "dir", "--set", "path=relative/dir"]);
    assert_ne!(code, 0, "a refusal is a failure:\n{out}{err}");
    assert!(
        err.contains("is not an absolute path"),
        "the connector's own error is shown: {err}"
    );
    assert!(!a.config_file().exists(), "nothing written on ok:false");

    a.set_up();
    let text = std::fs::read_to_string(a.config_file()).expect("config.toml written");
    let table: toml::Table = text.parse().expect("valid TOML");
    assert_eq!(
        table["sync"]["connector"].as_str(),
        Some("dir"),
        "the [sync] table names the connector:\n{text}"
    );
    assert!(
        !text.contains(&world.remote().display().to_string()),
        "no connector setting is kept in config.toml:\n{text}"
    );

    // A second refusal leaves the working setup exactly as it was.
    let (code, _, _) = a.run(&["sync", "setup", "dir", "--set", "path=relative/dir"]);
    assert_ne!(code, 0);
    assert_eq!(std::fs::read_to_string(a.config_file()).unwrap(), text);
}

#[test]
fn setup_off_a_terminal_without_a_required_value_names_the_field() {
    let world = World::new("missing");
    let a = world.machine("a");
    let (code, out, err) = a.run(&["sync", "setup", "dir"]);
    assert_eq!(code, 2, "bad_request:\n{out}{err}");
    assert!(err.contains("path"), "names the key: {err}");
    assert!(err.contains("--set"), "names the way out: {err}");
    assert!(!a.config_file().exists());

    let (code, _, err) = a.run(&["sync", "setup", "dir", "--set", "nope=1"]);
    assert_eq!(
        code, 2,
        "an unknown key is refused before the connector: {err}"
    );
    assert!(err.contains("nope"), "{err}");

    let (code, _, err) = a.run(&["sync", "setup", "no-such-connector"]);
    assert_eq!(code, 4, "not_found: {err}");
    assert!(err.contains("tasqx-remote-no-such-connector"), "{err}");
}

#[test]
fn sync_before_setup_is_refused_with_the_way_out() {
    let world = World::new("unset");
    let a = world.machine("a");
    let (code, _, err) = a.run(&["sync"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("tasqx sync setup"), "{err}");
}

#[test]
fn two_stores_converge_through_the_folder_remote() {
    let world = World::new("converge");
    let a = world.machine("a");
    let b = world.machine("b");
    a.set_up();
    b.set_up();

    a.ok(&["init", "home"]);
    a.ok(&["add", "Water the plants +garden +weekly"]);
    let first = a.json(&["sync"]);
    assert_eq!(first["pushed"], true, "{first}");

    // B starts empty and receives A's task.
    let got = b.json(&["sync"]);
    assert_eq!(got["tasks_new"], 1, "{got}");
    let on_b = b.export();
    assert_eq!(task(&on_b, "Water the plants")["project"], "home");

    // Both edit the same task apart: B renames and annotates it, A drops a tag.
    b.ok(&["modify", "1", "Water the plants twice", "!high"]);
    b.ok(&["annotate", "1", "the fern needs more"]);
    a.ok(&["untag", "1", "weekly"]);

    a.ok(&["sync"]);
    b.ok(&["sync"]);
    a.ok(&["sync"]);

    let ea = a.export();
    let eb = b.export();
    let t = task(&ea, "Water the plants twice");
    assert_eq!(t["priority"], "H", "{t}");
    assert_eq!(t["tags"], serde_json::json!(["garden"]), "{t}");
    assert_eq!(t["annotations"][0]["body"], "the fern needs more", "{t}");
    assert_eq!(
        without_local_fields(ea),
        without_local_fields(eb),
        "the two stores converge"
    );

    // Nothing left to exchange: a further sync on either side pushes nothing.
    let quiet = b.json(&["sync"]);
    assert_eq!(quiet["pushed"], false, "{quiet}");
    assert_eq!(quiet["tasks_new"], 0, "{quiet}");
}

#[test]
fn a_removed_annotation_is_gone_on_the_other_store_after_both_sync() {
    let world = World::new("unannotate");
    let a = world.machine("a");
    let b = world.machine("b");
    a.set_up();
    b.set_up();

    a.ok(&["init", "home"]);
    a.ok(&["add", "Renew the passport"]);
    a.ok(&["annotate", "1", "photo booth at the station"]);
    a.ok(&["sync"]);
    b.ok(&["sync"]);
    let id = task(&b.export(), "Renew the passport")["annotations"][0]["id"]
        .as_str()
        .expect("B holds the note")
        .to_string();

    a.ok(&["unannotate", "1", &id]);
    a.ok(&["sync"]);
    b.ok(&["sync"]);

    let eb = b.export();
    let t = task(&eb, "Renew the passport");
    assert_eq!(t["annotations"], serde_json::json!([]), "{t}");
    assert!(
        !eb.to_string().contains("photo booth"),
        "D113: the body is scrubbed, not only hidden"
    );
}

/// Through a daemon, the daemon is the only writer: the merge lands in ITS
/// store, `$TASQX_DB` is never opened, and both the connector's state and the
/// sync state sit beside the daemon's file (D5, D74).
#[test]
fn through_a_daemon_the_merge_lands_in_the_daemons_store_and_nothing_else() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let world = World::new("daemon");
    let a = world.machine("a");
    a.set_up();
    a.ok(&["init", "home"]);
    a.ok(&["add", "Book the ferry"]);
    a.ok(&["sync"]);

    let served = world.root.join("served");
    std::fs::create_dir_all(&served).unwrap();
    let daemon_db = served.join("tasks.db");
    let env_db = world.root.join("env").join("tasks.db");
    let sock = if cfg!(windows) {
        format!("tasqx-sync-daemon-{}", std::process::id())
    } else {
        world.root.join("d.sock").to_string_lossy().into_owned()
    };
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let (db, sk, sd) = (
            daemon_db.to_string_lossy().into_owned(),
            sock.clone(),
            shutdown.clone(),
        );
        std::thread::spawn(move || {
            let engine = tasqx_core::Engine::open(&db).expect("open daemon store");
            tasqx_core::daemon::serve(engine, &sk, sd).expect("serve");
        });
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while tasqx_core::daemon::try_connect(&sock).is_none() {
        assert!(Instant::now() < deadline, "daemon never came up at {sock}");
        std::thread::sleep(Duration::from_millis(10));
    }

    let via_daemon = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &env_db)
            .env("TASQX_SOCK", &sock)
            .env("TASQX_CONFIG_DIR", served.join("cfg"))
            .env("PATH", &world.path_var)
            .env_remove("TASQX_NOW")
            .env("NO_COLOR", "1")
            .args(args)
            .output()
            .expect("run tasqx");
        assert!(
            out.status.success(),
            "tasqx {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let path = format!("path={}", world.remote().display());
    via_daemon(&["sync", "setup", "dir", "--set", &path]);
    via_daemon(&["sync"]);
    shutdown.store(true, Ordering::SeqCst);

    let engine = tasqx_core::Engine::open(daemon_db.to_str().unwrap()).unwrap();
    let listed = tasqx_core::dispatch(&engine, "task.list", &serde_json::json!({})).unwrap();
    assert_eq!(listed["tasks"][0]["title"], "Book the ferry", "{listed}");
    assert!(!env_db.exists(), "$TASQX_DB was never opened");
    assert!(
        served.join("tasks.db.sync.json").is_file(),
        "sync state beside the daemon's store"
    );
    assert!(
        served.join("remotes").join("dir").is_dir(),
        "connector state beside it too"
    );
}
