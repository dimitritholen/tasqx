//! D74 end to end: the store is a read surface on every branch.
//!
//! These tests run the real binary against a real in-thread daemon, because
//! every one of them is about which stderr/stdout line reaches an operator —
//! the surface the 2026-07-25 incident and the 2026-08-31 field test both
//! showed nothing was speaking on. The fixture here deliberately does NOT
//! force `--no-daemon` the way `regressions.rs`'s does: routing through the
//! daemon is the subject, not a hazard to be fenced off. Every daemon sits on
//! a unique per-test socket, so a developer's real daemon on the default
//! address is never touched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// A per-test scratch world: a db for the daemon, a second db for `$TASQX_DB`
/// (different on purpose — proving an answer came over the socket requires the
/// env var to name a file the daemon has never seen), a unique socket and a
/// fresh config dir.
struct World {
    daemon_db: PathBuf,
    env_db: PathBuf,
    sock: String,
    config_dir: PathBuf,
}

fn world(tag: &str) -> World {
    let stem = format!("tasqx-dvis-{tag}-{}", std::process::id());
    let daemon_db = std::env::temp_dir().join(format!("{stem}-daemon.db"));
    let env_db = std::env::temp_dir().join(format!("{stem}-env.db"));
    let _ = std::fs::remove_file(&daemon_db);
    let _ = std::fs::remove_file(&env_db);
    let sock = if cfg!(windows) {
        stem.clone()
    } else {
        std::env::temp_dir()
            .join(format!("{stem}.sock"))
            .to_string_lossy()
            .into_owned()
    };
    let config_dir = std::env::temp_dir().join(format!("{stem}-config"));
    let _ = std::fs::remove_dir_all(&config_dir);
    std::fs::create_dir_all(&config_dir).expect("create config dir");
    World {
        daemon_db,
        env_db,
        sock,
        config_dir,
    }
}

/// Serve the daemon db on the world's socket from a background thread; the
/// same in-thread pattern `tokens_recompute.rs` uses, with the same generous
/// readiness budget (an instrumented coverage build is slow, not broken).
fn start_daemon(w: &World) -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let sd = shutdown.clone();
    let db = w.daemon_db.to_string_lossy().into_owned();
    let sk = w.sock.clone();
    thread::spawn(move || {
        let engine = tasqx_core::Engine::open(&db).expect("open daemon store");
        tasqx_core::daemon::serve(engine, &sk, sd).expect("serve");
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(c) = tasqx_core::daemon::try_connect(&w.sock) {
            drop(c);
            return shutdown;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("daemon never became connectable at {}", w.sock);
}

/// The binary, in this world: `$TASQX_DB` names the env db (NOT the daemon's),
/// `$TASQX_SOCK` names the world's socket, and the config dir is scratch.
fn bin(w: &World) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", &w.config_dir)
        .env("TASQX_DB", &w.env_db)
        .env("TASQX_SOCK", &w.sock);
    c
}

fn canon(p: &str) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|e| panic!("canonicalize {p}: {e}"))
}

/// D74 / #254: `tasqx config store` on the daemon branch names the daemon's
/// own file, by asking it — `$TASQX_DB` names a different file here precisely
/// so a local guess cannot pass as the daemon's answer. D47 forbade printing
/// the client's inert local path; it never forbade the daemon telling the
/// truth about its own.
#[test]
fn config_store_names_the_daemons_file_by_asking_it() {
    let w = world("cfgstore");
    let shutdown = start_daemon(&w);

    let out = bin(&w).args(["config", "store"]).output().expect("run");
    shutdown.store(true, Ordering::Relaxed);
    assert!(
        out.status.success(),
        "config store: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let named = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("the daemon owns the store: "))
        .unwrap_or_else(|| panic!("the daemon branch must name the daemon's file: {stdout}"));
    assert_eq!(
        canon(named),
        canon(&w.daemon_db.to_string_lossy()),
        "the named store must be the daemon's file, not $TASQX_DB's"
    );
    assert!(
        !stdout.contains(&*w.env_db.to_string_lossy()),
        "the client's inert local path must still not be presented (D47): {stdout}"
    );
}

/// D74 / #246: a command that routes through a daemon while `$TASQX_DB` is set
/// says the variable is not in effect — on stderr, every time the condition
/// holds, including for the exact `add`+`list` pair that confirmed the
/// 2026-07-25 operator in the wrong belief. The control run without the
/// variable stays silent: quiet in the common case is the condition being
/// rare, not the note being throttled.
#[test]
fn a_remote_routed_command_says_tasqx_db_is_not_in_effect() {
    let w = world("inertenv");
    let shutdown = start_daemon(&w);

    let out = bin(&w).args(["list"]).output().expect("run list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "list through the daemon: {stderr}");
    assert!(
        stderr.contains("$TASQX_DB is not in effect"),
        "the path that ignores the variable must say the word: {stderr}"
    );
    assert!(
        stderr.contains("--no-daemon"),
        "the note must name the way out: {stderr}"
    );

    // Control: no $TASQX_DB, same daemon, same verb — silence.
    let mut quiet = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    quiet
        .env("TASQX_CONFIG_DIR", &w.config_dir)
        .env("TASQX_SOCK", &w.sock)
        .env_remove("TASQX_DB");
    let out = quiet.args(["list"]).output().expect("run control");
    shutdown.store(true, Ordering::Relaxed);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("TASQX_DB"),
        "with no variable set there is nothing to warn about: {stderr}"
    );
}

/// The marker filename `daemon_retired_marker` derives for a socket, repeated
/// here so the consumption test plants the file where the binary will look. If
/// the formula in `lib.rs` drifts, the note never prints and this file's
/// retirement test goes red — which is the correct failure.
fn planted_marker(config_dir: &Path, sock: &str) -> PathBuf {
    let sanitized: String = sock
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    config_dir.join(format!("daemon-retired-{sanitized}"))
}

/// D74 / #255: the first command whose daemon connect fails reads the
/// retirement note, reports the transition, and consumes the marker — said
/// once, then silence. The end-to-end write of this marker is covered on the
/// core suite (`an_idle_retirement_records_itself_where_the_options_said_to`);
/// here the file is planted in the daemon's own format and the client half is
/// driven through the real binary.
#[test]
fn the_first_command_after_a_retirement_reports_it_and_consumes_the_note() {
    let w = world("retired");
    let marker = planted_marker(&w.config_dir, &w.sock);
    let daemon_db = w.daemon_db.to_string_lossy().into_owned();
    std::fs::write(
        &marker,
        format!(
            "# planted by the test in the daemon's own format\n\
             socket {}\n\
             store {daemon_db}\n\
             retired 2026-08-31T09:00:00Z\n",
            w.sock
        ),
    )
    .expect("plant the marker");

    // No daemon on the socket: the connect fails, the fallback speaks.
    let out = bin(&w).args(["list"]).output().expect("run list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "list in-process: {stderr}");
    assert!(
        stderr.contains("left on its idle timeout"),
        "the transition must be reported: {stderr}"
    );
    assert!(
        stderr.contains(&daemon_db),
        "the store that is no longer being served must be named: {stderr}"
    );
    assert!(
        !marker.exists(),
        "the note is said once — the marker must be consumed"
    );

    // The second command is ordinary again.
    let out = bin(&w).args(["list"]).output().expect("run list again");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("idle timeout"),
        "a consumed note must not repeat: {stderr}"
    );
}

/// D74 / #254: `tasqx daemon` names its store on startup, beside the address,
/// so the one line an operator reads in a scrollback answers the question
/// every wrong-store incident starts with.
#[test]
fn the_daemon_names_its_store_on_startup() {
    let w = world("announce");
    let daemon_db = w.daemon_db.to_string_lossy().into_owned();
    let mut child = bin(&w)
        .args(["daemon", "--socket", &w.sock, "--db", &daemon_db])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn daemon");

    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut lines = Vec::new();
    let store_line = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => {
                lines.push(line);
                if let Some(l) = lines
                    .iter()
                    .find_map(|l| l.strip_prefix("tasqx daemon: store "))
                {
                    break Some(l.to_string());
                }
            }
            Err(_) => break None,
        }
    };
    let _ = child.kill();
    let _ = child.wait();

    let store_line =
        store_line.unwrap_or_else(|| panic!("no store line on startup; stderr was {lines:?}"));
    assert_eq!(
        canon(&store_line),
        canon(&daemon_db),
        "the announced store must be the file the daemon opened"
    );
}

/// A scratch `$HOME` for the two `#184` tests below: `directories::ProjectDirs`
/// resolves the platform-default store from `$HOME` (Linux and macOS; Windows
/// asks the OS profile API instead and does not read this variable, which is
/// why both callers are `#[cfg(unix)]`), and `api`/`mcp serve` with no
/// `$TASQX_DB` is exactly the branch under test — it must land in a directory
/// this test owns and deletes, never a real developer's data dir.
#[cfg(unix)]
fn scratch_home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("tasqx-dvis-{tag}-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create scratch HOME");
    home
}

/// #184: `api` never routes through a daemon (D73 keeps it an in-process
/// stdio host), and until now an ambient `$TASQX_SOCK` naming a *live*
/// daemon, with `$TASQX_DB` unset, was left completely unremarked — the
/// exact repro in #184's Observed section: a daemon serving 25 real tasks
/// sat one env var away while `api` silently opened and answered from a
/// brand-new, empty default store, stderr empty. The note added for this is
/// additive only — D73's ruling that the env var stays ambient (never
/// refused) on this verb is untouched, so the assertion below is explicit
/// that this is a note, not the D73 refusal.
#[cfg(unix)]
#[test]
fn api_notes_a_live_daemon_it_never_routed_through() {
    use std::io::Write;
    let w = world("apinote");
    let shutdown = start_daemon(&w);
    let home = scratch_home("apinote");

    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &w.config_dir)
        .env("TASQX_SOCK", &w.sock)
        .env("HOME", &home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("TASQX_DB")
        .arg("api")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx api");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"tasqx":"1","id":"n","method":"task.list","params":{}}"#)
        .expect("write envelope");
    let out = child.wait_with_output().expect("wait");
    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        out.status.success(),
        "api must still answer: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&w.sock),
        "the note must name the daemon it saw but did not route through: {stderr}"
    );
    assert!(
        stderr.contains("TASQX_DB"),
        "the note must point at the way to reach the daemon's own store: {stderr}"
    );
    assert!(
        !stderr.contains("error ["),
        "this is a note, not a refusal — D73 keeps $TASQX_SOCK ambient here: {stderr}"
    );
}

/// The same gap on `mcp serve` — the surface #184 calls out by name ("`mcp
/// serve` is how an agent is wired up"). Stdin is closed immediately (EOF),
/// which is enough to drive the server through its open-and-announce path and
/// back out without a real MCP client.
#[cfg(unix)]
#[test]
fn mcp_serve_notes_a_live_daemon_it_never_routed_through() {
    let w = world("mcpnote");
    let shutdown = start_daemon(&w);
    let home = scratch_home("mcpnote");

    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &w.config_dir)
        .env("TASQX_SOCK", &w.sock)
        .env("HOME", &home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("TASQX_DB")
        .args(["mcp", "serve"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx mcp serve");
    drop(child.stdin.take().expect("stdin")); // immediate EOF
    let out = child.wait_with_output().expect("wait");
    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&home);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&w.sock),
        "the note must name the daemon it saw but did not route through: {stderr}"
    );
    assert!(
        stderr.contains("TASQX_DB"),
        "the note must point at the way to reach the daemon's own store: {stderr}"
    );
    assert!(
        !stderr.contains("error ["),
        "this is a note, not a refusal — D73 keeps $TASQX_SOCK ambient here: {stderr}"
    );
}

/// #184 (reviewer follow-up): a first attempt at the note above fired even
/// when the daemon's store and the local default store named the exact same
/// file — reproduced directly against a daemon serving the very path
/// `api` resolves with no `$TASQX_DB` — which contradicted the fix's own
/// "fires only on a live divergent store" claim and the task's own
/// Verification annotation, which names "daemon serving a non-default store"
/// as a required precondition. Here there is no divergence: the daemon
/// happens to answer from the file `api` would open anyway, so opening it
/// in-process reads the right data and there is nothing to warn about.
#[cfg(unix)]
#[test]
fn api_says_nothing_when_the_daemons_store_is_the_local_default() {
    use std::io::Write;
    let home = scratch_home("samestore");
    let config_dir = std::env::temp_dir().join(format!(
        "tasqx-dvis-samestore-config-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&config_dir);
    std::fs::create_dir_all(&config_dir).expect("create config dir");

    // Ask the binary itself where the local default store lives under this
    // scratch $HOME, rather than re-deriving `directories::ProjectDirs`'s
    // algorithm here — the two must never drift apart for this test to mean
    // anything.
    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &config_dir)
        .env("HOME", &home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("TASQX_DB")
        .env_remove("TASQX_SOCK")
        .args(["config", "store"])
        .output()
        .expect("run config store");
    assert!(
        out.status.success(),
        "config store: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let default_store = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_else(|| panic!("config store printed nothing"))
        .trim()
        .to_string();

    let sock = if cfg!(windows) {
        format!("tasqx-dvis-samestore-{}", std::process::id())
    } else {
        std::env::temp_dir()
            .join(format!("tasqx-dvis-samestore-{}.sock", std::process::id()))
            .to_string_lossy()
            .into_owned()
    };
    let w = World {
        daemon_db: PathBuf::from(&default_store),
        env_db: std::env::temp_dir().join(format!(
            "tasqx-dvis-samestore-unused-{}.db",
            std::process::id()
        )),
        sock,
        config_dir: config_dir.clone(),
    };
    let shutdown = start_daemon(&w);

    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &w.config_dir)
        .env("TASQX_SOCK", &w.sock)
        .env("HOME", &home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("TASQX_DB")
        .arg("api")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx api");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"tasqx":"1","id":"n","method":"task.list","params":{}}"#)
        .expect("write envelope");
    let out = child.wait_with_output().expect("wait");
    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&config_dir);

    assert!(
        out.status.success(),
        "api must still answer: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("tasqx: note:"),
        "the daemon answers from the exact file this verb would open anyway \
         — there is no divergent store to warn about: {stderr}"
    );
}

/// #250: a second daemon on a held address used to print
/// `listening on <addr>` BEFORE attempting the bind, then contradict itself —
/// and in a log or a service unit the listening line is the one an operator
/// reads. During the field test that false line, plus a daemon leaked by a
/// crashed harness, sent the investigation chasing a daemon death that had
/// not happened. The banner now prints only after the bind succeeds: a
/// process that never listened must not say it did.
#[test]
fn a_daemon_that_cannot_bind_never_claims_to_be_listening() {
    let w = world("secondbind");
    let shutdown = start_daemon(&w);

    let second_db =
        std::env::temp_dir().join(format!("tasqx-dvis-secondbind-b-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&second_db);
    let out = bin(&w)
        .args([
            "daemon",
            "--socket",
            &w.sock,
            "--db",
            &second_db.to_string_lossy(),
        ])
        .output()
        .expect("run second daemon");
    shutdown.store(true, Ordering::Relaxed);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "the second daemon must refuse: {stderr}"
    );
    assert!(
        stderr.contains("bind/serve failed"),
        "the refusal must name what failed — the half that already worked: {stderr}"
    );
    assert!(
        !stderr.contains("listening on"),
        "a process that never listened must not announce that it did: {stderr}"
    );
    let _ = std::fs::remove_file(&second_db);
}

/// Finding #14 (audit-2026-09): an explicitly named `--socket` with nothing
/// listening used to fall back to in-process silently, exit 0 — but
/// `--socket`'s own help promises single-writer routing "when a daemon is
/// reachable", so a caller who passed the flag stated an intention the
/// fallback then quietly broke. No daemon is started in this world on
/// purpose: the socket path is real (a scratch temp dir) but nothing binds
/// it, which is the "stale/missing socket" case the flag can genuinely hit.
#[test]
fn an_explicit_unreachable_socket_warns_on_fallback() {
    let w = world("explicitsock");
    // No daemon started — `w.sock` names a path nothing is listening on.
    let out = bin(&w)
        .args(["--socket", &w.sock, "list"])
        .output()
        .expect("run list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the fallback must still succeed: {stderr}"
    );
    assert!(
        stderr.contains("no daemon at"),
        "an explicit --socket that cannot be reached must warn: {stderr}"
    );
    assert!(
        stderr.contains(&w.sock),
        "the warning must name the socket: {stderr}"
    );
}

/// The mirror of the test above: with NO `--socket` flag, the same missing
/// daemon (env/default discovery) must stay silent about it — only an
/// explicit flag is a stated intention worth warning about.
#[test]
fn an_unreachable_socket_from_env_discovery_stays_silent() {
    let w = world("envsock");
    // `$TASQX_SOCK` (set by `bin`) names a path nothing is listening on, and
    // no `--socket` flag is passed.
    let out = bin(&w).args(["list"]).output().expect("run list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the fallback must still succeed: {stderr}"
    );
    assert!(
        !stderr.contains("no daemon at"),
        "env/default discovery is silent on a missing daemon: {stderr}"
    );
}
