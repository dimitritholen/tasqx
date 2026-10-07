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

/// Every machine of a world shares this sync passphrase unless a test gives
/// one another (D202).
const PASSPHRASE: &str = "correct horse battery staple";

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

    /// Another workspace connector binary, installed under its own name.
    fn install_binary(&self, crate_name: &str) {
        let exe = format!("{crate_name}{}", std::env::consts::EXE_SUFFIX);
        let built = Path::new(env!("CARGO_BIN_EXE_tasqx")).with_file_name(&exe);
        assert!(
            built.is_file(),
            "{} is missing: build the workspace",
            built.display()
        );
        std::fs::copy(built, self.root.join("bin").join(exe)).expect("copy the connector");
    }

    /// The same connector again under another name, as a second connector.
    fn install_as(&self, name: &str) {
        let exe = format!("tasqx-remote-{name}{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(connector_binary(), self.root.join("bin").join(exe))
            .expect("copy the connector");
    }

    fn remote(&self) -> PathBuf {
        self.root.join("remote")
    }

    fn machine(&self, name: &str) -> Machine<'_> {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).expect("create machine dir");
        let config_dir = dir.join("cfg");
        Machine {
            world: self,
            dir,
            config_dir,
            passphrase: PASSPHRASE,
        }
    }
}

struct Machine<'w> {
    world: &'w World,
    dir: PathBuf,
    config_dir: PathBuf,
    passphrase: &'static str,
}

impl Machine<'_> {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_DB", self.dir.join("tasks.db"))
            .env("TASQX_CONFIG_DIR", &self.config_dir)
            .env("PATH", &self.world.path_var)
            .env_remove("TASQX_SOCK")
            .env_remove("TASQX_NOW")
            .env("TASQX_SYNC_PASSPHRASE", self.passphrase)
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
        self.config_dir.join("config.toml")
    }

    /// A config.toml with a setting in it, so "untouched" is a real claim.
    fn seed_config(&self) -> String {
        let text = "[theme]\nname = \"mono\"\n".to_string();
        std::fs::create_dir_all(&self.config_dir).unwrap();
        std::fs::write(self.config_file(), &text).unwrap();
        text
    }

    fn state_file(&self) -> PathBuf {
        self.dir.join("tasks.db.sync.json")
    }

    fn key_file(&self) -> PathBuf {
        self.dir.join("tasks.db.sync.key")
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
fn setup_with_set_records_the_connector_beside_the_store_and_a_refusal_records_nothing() {
    let world = World::new("setup");
    let a = world.machine("a");
    let config = a.seed_config();

    // Refused by the connector: a relative folder. Its words reach the user,
    // and nothing is recorded.
    let (code, out, err) = a.run(&["sync", "setup", "dir", "--set", "path=relative/dir"]);
    assert_ne!(code, 0, "a refusal is a failure:\n{out}{err}");
    assert!(
        err.contains("is not an absolute path"),
        "the connector's own error is shown: {err}"
    );
    assert!(!a.state_file().exists(), "nothing written on ok:false");

    a.set_up();
    let state: Value =
        serde_json::from_slice(&std::fs::read(a.state_file()).expect("state written")).unwrap();
    assert_eq!(state["connector"], "dir", "{state}");
    assert_eq!(state["version"], Value::Null, "not synced yet: {state}");
    assert!(
        !state
            .to_string()
            .contains(&world.remote().display().to_string()),
        "no connector setting is kept by tasqx: {state}"
    );

    // A second refusal leaves the working setup exactly as it was.
    let before = std::fs::read(a.state_file()).unwrap();
    let (code, _, _) = a.run(&["sync", "setup", "dir", "--set", "path=relative/dir"]);
    assert_ne!(code, 0);
    assert_eq!(std::fs::read(a.state_file()).unwrap(), before);

    a.ok(&["sync"]);
    assert_eq!(
        std::fs::read_to_string(a.config_file()).unwrap(),
        config,
        "neither setup nor sync touches config.toml"
    );
}

/// One remote per STORE: two stores on one machine (one config dir) with
/// different connectors each see their own.
#[test]
fn two_stores_sharing_a_config_keep_their_own_connectors() {
    let world = World::new("perstore");
    world.install_as("folder");
    let a = world.machine("a");
    let mut b = world.machine("b");
    b.config_dir = a.config_dir.clone();
    let config = a.seed_config();

    a.set_up();
    let path = format!("path={}", world.root.join("other-remote").display());
    b.ok(&["sync", "setup", "folder", "--set", &path]);

    assert_eq!(a.json(&["sync", "--status"])["connector"], "dir");
    assert_eq!(b.json(&["sync", "--status"])["connector"], "folder");
    let mut c = world.machine("c");
    c.config_dir = a.config_dir.clone();
    assert_eq!(
        c.json(&["sync", "--status"])["set_up"],
        false,
        "a third store sharing the config is not set up"
    );
    assert_eq!(std::fs::read_to_string(a.config_file()).unwrap(), config);
}

#[test]
fn setup_off_a_terminal_without_a_required_value_names_the_field() {
    let world = World::new("missing");
    let a = world.machine("a");
    let (code, out, err) = a.run(&["sync", "setup", "dir"]);
    assert_eq!(code, 2, "bad_request:\n{out}{err}");
    assert!(err.contains("path"), "names the key: {err}");
    assert!(err.contains("--set"), "names the way out: {err}");
    assert!(!a.state_file().exists());

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

    // Nothing left to exchange: a further sync on either side pushes nothing,
    // though every seal of the same store is different ciphertext.
    let quiet = b.json(&["sync"]);
    assert_eq!(quiet["pushed"], false, "{quiet}");
    assert_eq!(quiet["tasks_new"], 0, "{quiet}");

    // The folder never held a byte of either store in clear (D202).
    let blobs = remote_blobs(&world.remote());
    assert!(!blobs.is_empty(), "the connector kept snapshots");
    for (path, bytes) in blobs {
        for marker in [&b"Water the plants"[..], b"the fern needs more", b"garden"] {
            assert!(
                !bytes.windows(marker.len()).any(|w| w == marker),
                "{} holds {:?} in clear",
                path.display(),
                String::from_utf8_lossy(marker)
            );
        }
    }
}

/// Every file `tasqx-remote-dir` keeps in the remote folder.
fn remote_blobs(remote: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut dirs = vec![remote.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("read the remote folder") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                let bytes = std::fs::read(&path).expect("read a remote file");
                out.push((path, bytes));
            }
        }
    }
    out
}

/// A machine that set up the same remote with another passphrase cannot read
/// it: the sync fails naming the passphrase, nothing is merged and nothing is
/// recorded. Set up again with the right one, it joins (D202).
#[test]
fn a_machine_with_another_passphrase_merges_nothing_until_set_up_with_the_right_one() {
    let world = World::new("wrongpass");
    let a = world.machine("a");
    let mut b = world.machine("b");
    b.passphrase = "a different passphrase";
    a.set_up();
    b.set_up();
    a.ok(&["init", "home"]);
    a.ok(&["add", "Renew the passport"]);
    a.ok(&["sync"]);

    b.ok(&["init", "work"]);
    let before = b.export();
    let (code, out, err) = b.run(&["sync"]);
    assert_eq!(code, 2, "refused:\n{out}{err}");
    assert!(err.contains("passphrase"), "names the likely cause: {err}");
    assert!(err.contains("nothing"), "says nothing was merged: {err}");
    assert_eq!(b.export()["tasks"], before["tasks"], "store unchanged");
    let state: Value = serde_json::from_slice(&std::fs::read(b.state_file()).unwrap()).unwrap();
    assert_eq!(state["version"], Value::Null, "no sync recorded: {state}");

    b.passphrase = PASSPHRASE;
    b.set_up();
    let got = b.json(&["sync"]);
    assert_eq!(got["tasks_new"], 1, "{got}");
}

/// The passphrase is kept beside the store, owner-only, and never in the
/// state file; `--status` says encryption is on without printing it.
#[test]
fn setup_keeps_the_passphrase_in_its_own_private_file_and_status_says_encrypted() {
    let world = World::new("keyfile");
    let a = world.machine("a");
    a.set_up();
    assert_eq!(std::fs::read_to_string(a.key_file()).unwrap(), PASSPHRASE);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(a.key_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }
    let state = std::fs::read_to_string(a.state_file()).unwrap();
    assert!(!state.contains(PASSPHRASE), "{state}");

    let status = a.json(&["sync", "--status"]);
    assert_eq!(status["encrypted"], true, "{status}");
    assert!(!status.to_string().contains(PASSPHRASE), "{status}");
    let text = a.ok(&["sync", "--status"]);
    assert!(text.contains("encrypted"), "{text}");
    assert!(!text.contains(PASSPHRASE), "{text}");
}

/// The passphrase never travels in argv; off a terminal it comes from
/// `TASQX_SYNC_PASSPHRASE`, and without it setup is refused before the
/// connector records anything. Empty is refused; short is warned about.
#[test]
fn the_passphrase_comes_from_the_environment_never_from_set() {
    let world = World::new("passarg");
    let mut a = world.machine("a");
    let path = format!("path={}", world.remote().display());

    let (code, out, err) = a.run(&[
        "sync",
        "setup",
        "dir",
        "--set",
        &path,
        "--set",
        "passphrase=hunter2",
    ]);
    assert_eq!(code, 2, "refused:\n{out}{err}");
    assert!(err.contains("TASQX_SYNC_PASSPHRASE"), "the way in: {err}");
    assert!(!err.contains("hunter2"), "{err}");
    assert!(!a.state_file().exists() && !a.key_file().exists());

    let out = a
        .cmd(&["sync", "setup", "dir", "--set", &path])
        .env_remove("TASQX_SYNC_PASSPHRASE")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("TASQX_SYNC_PASSPHRASE"));
    assert!(!a.state_file().exists() && !a.key_file().exists());

    a.passphrase = "";
    let (code, _, err) = a.run(&["sync", "setup", "dir", "--set", &path]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("empty"), "{err}");
    assert!(!a.state_file().exists() && !a.key_file().exists());

    a.passphrase = "short";
    let (code, _, err) = a.run(&["sync", "setup", "dir", "--set", &path]);
    assert_eq!(code, 0, "a short passphrase is taken: {err}");
    assert!(
        err.contains("under 12 characters"),
        "and warned about: {err}"
    );
    assert!(a.key_file().is_file());
}

/// A store set up before snapshots were encrypted has a state file and no
/// key: sync refuses and says to set up again (D202).
#[test]
fn a_store_set_up_before_encryption_is_told_to_set_up_again() {
    let world = World::new("prekey");
    let a = world.machine("a");
    a.set_up();
    std::fs::remove_file(a.key_file()).unwrap();
    a.ok(&["init", "home"]);
    let (code, _, err) = a.run(&["sync"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("tasqx sync setup dir"), "{err}");
    assert!(
        world.remote().read_dir().unwrap().next().is_none(),
        "nothing reached the remote"
    );
    assert_eq!(a.json(&["sync", "--status"])["encrypted"], false);
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

/// A daemon serving `<root>/<name>/tasks.db` on a socket of this test's own,
/// and a way to run the binary through it. `$TASQX_DB` names the daemon's own
/// file: one naming another file beside `$TASQX_SOCK` is refused (D204), and
/// leaving it unset would aim a failed route at the real default store.
struct Daemon<'w> {
    world: &'w World,
    dir: PathBuf,
    sock: String,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    server: std::thread::JoinHandle<()>,
}

impl<'w> Daemon<'w> {
    fn start(world: &'w World, name: &str) -> Daemon<'w> {
        use std::time::{Duration, Instant};
        let dir = world.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        // A pipe name is global on Windows, and the tests of this binary share
        // a pid and run in parallel, so the name carries the world's own
        // root (`tasqx-sync-<tag>-<pid>`) as a Unix socket's path does.
        let sock = if cfg!(windows) {
            let world_name = world.root.file_name().expect("world root has a name");
            format!("{}-{name}", world_name.to_string_lossy())
        } else {
            world
                .root
                .join(format!("{name}.sock"))
                .to_string_lossy()
                .into_owned()
        };
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server;
        {
            let (db, sk, sd) = (
                dir.join("tasks.db").to_string_lossy().into_owned(),
                sock.clone(),
                shutdown.clone(),
            );
            server = std::thread::spawn(move || {
                let engine = tasqx_core::Engine::open(&db).expect("open daemon store");
                tasqx_core::daemon::serve(engine, &sk, sd).expect("serve");
            });
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while tasqx_core::daemon::try_connect(&sock).is_none() {
            assert!(Instant::now() < deadline, "daemon never came up at {sock}");
            std::thread::sleep(Duration::from_millis(10));
        }
        Daemon {
            world,
            dir,
            sock,
            shutdown,
            server,
        }
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", self.dir.join("tasks.db"))
            .env("TASQX_SOCK", &self.sock)
            .env("TASQX_CONFIG_DIR", self.dir.join("cfg"))
            .env("PATH", &self.world.path_var)
            .env_remove("TASQX_NOW")
            .env("TASQX_SYNC_PASSPHRASE", PASSPHRASE)
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
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn set_up(&self) {
        let path = format!("path={}", self.world.remote().display());
        self.ok(&["sync", "setup", "dir", "--set", &path]);
    }

    /// The daemon's store, read directly once it is stopped and its thread
    /// has returned, so its last write is in and its endpoint is released.
    fn stop_and_export(self) -> Value {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.server.join().expect("daemon thread");
        let db = self.dir.join("tasks.db");
        let engine = tasqx_core::Engine::open(db.to_str().unwrap()).unwrap();
        tasqx_core::dispatch(&engine, "store.export", &serde_json::json!({})).unwrap()
    }
}

/// Through a daemon, the daemon is the only writer: the merge lands in ITS
/// store, and the connector's state and the sync state sit beside the
/// daemon's file (D5, D74).
#[test]
fn through_a_daemon_the_merge_lands_in_the_daemons_store_and_nothing_else() {
    let world = World::new("daemon");
    let a = world.machine("a");
    a.set_up();
    a.ok(&["init", "home"]);
    a.ok(&["add", "Book the ferry"]);
    a.ok(&["sync"]);

    let d = Daemon::start(&world, "served");
    d.set_up();
    d.ok(&["sync"]);
    let served = d.dir.clone();
    let export = d.stop_and_export();
    assert_eq!(task(&export, "Book the ferry")["project"], "home");
    assert!(
        served.join("tasks.db.sync.json").is_file(),
        "sync state beside the daemon's store"
    );
    assert!(
        served.join("remotes").join("dir").is_dir(),
        "connector state beside it too"
    );
}

/// A document of `n` tasks, each carrying a 4 KB note, in the import shape.
fn big_document(n: usize) -> Value {
    let tasks: Vec<Value> = (0..n)
        .map(|i| {
            serde_json::json!({
                "id": format!("01a0e2ec-0000-7000-8000-{i:012x}"),
                "short_id": i + 1,
                "title": format!("Bulk task {i}"),
                "status": "pending",
                "project": "home",
                "created": "2026-09-01T00:00:00Z",
                "modified": "2026-09-01T00:00:00Z",
                "annotations": [{
                    "id": format!("01a0e2ec-0001-7000-8000-{i:012x}"),
                    "body": "x".repeat(4000),
                    "created": "2026-09-01T00:00:00Z",
                }],
            })
        })
        .collect();
    serde_json::json!({ "tasks": tasks })
}

/// A store whose export is over the daemon's 1 MiB request frame syncs both
/// in-process and through a daemon, because `sync` hands `store.import` and
/// `store.export` a file path instead of the document (D201).
#[test]
fn a_store_over_one_mebibyte_syncs_in_process_and_through_a_daemon() {
    let world = World::new("big");
    let a = world.machine("a");
    let doc = world.root.join("big.json");
    std::fs::write(&doc, big_document(300).to_string()).unwrap();
    a.ok(&["import", doc.to_str().unwrap()]);
    assert!(
        a.ok(&["export"]).len() > 1 << 20,
        "the export must exceed the daemon's frame for this test to mean anything"
    );
    a.set_up();
    a.ok(&["sync"]);

    let d = Daemon::start(&world, "served");
    d.set_up();
    d.ok(&["sync"]);
    d.ok(&["add", "Written through the daemon", "project:home"]);
    d.ok(&["sync"]);
    let on_daemon = d.stop_and_export();

    let got = a.json(&["sync"]);
    assert_eq!(got["tasks_new"], 1, "{got}");
    let on_a = a.export();
    assert_eq!(on_a["tasks"].as_array().unwrap().len(), 301);
    assert_eq!(
        without_local_fields(on_a),
        without_local_fields(on_daemon),
        "the two stores converge"
    );
}

/// A secret never travels in argv, where shell history and `ps` would keep
/// it: `--set` of a field `describe` marks secret is refused before the
/// connector's `configure` runs, naming the two ways in (D201).
/// `tasqx-remote-r2` is the connector with a secret field; the refusal comes
/// before any request, so no bucket is needed.
#[test]
fn a_secret_field_in_set_is_refused_and_nothing_is_recorded() {
    let world = World::new("secret");
    world.install_binary("tasqx-remote-r2");
    let a = world.machine("a");
    let (code, out, err) = a.run(&["sync", "setup", "r2", "--set", "secret_access_key=hunter2"]);
    assert_eq!(code, 2, "bad_request:\n{out}{err}");
    assert!(err.contains("secret_access_key"), "names the field: {err}");
    assert!(
        err.contains("TASQX_SYNC_SECRET_ACCESS_KEY"),
        "names the env var: {err}"
    );
    assert!(err.contains("prompt"), "names the prompt: {err}");
    assert!(!err.contains("hunter2"), "never echoes the value: {err}");
    assert!(!a.state_file().exists());

    // Off a terminal, a missing secret names the env var as the way in.
    let (code, _, err) = a.run(&["sync", "setup", "r2"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("TASQX_SYNC_SECRET_ACCESS_KEY"), "{err}");
    assert!(!a.state_file().exists());
}
