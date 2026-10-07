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
/// own file, by asking it. `$TASQX_DB` is unset here: with it naming another
/// file and `$TASQX_SOCK` set, D204 refuses (see
/// `tasqx_db_disagreeing_with_an_explicit_socket_is_refused`). D47 forbade
/// printing the client's inert local path; it never forbade the daemon telling
/// the truth about its own.
#[test]
fn config_store_names_the_daemons_file_by_asking_it() {
    let w = world("cfgstore");
    let shutdown = start_daemon(&w);

    let out = via_daemon(&w)
        .args(["config", "store"])
        .output()
        .expect("run");
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
        "the named store must be the daemon's file"
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

/// #236.4: a daemon killed outright (no idle retirement, so no marker) must
/// still be told apart from one that is alive and serving. D74 covers the
/// idle-retirement case above with a note the first failed-connect command
/// consumes; a `kill -9`'d daemon leaves no such note, so before this fix
/// nothing was said at all — "my daemon is serving these writes" and "my
/// daemon died an hour ago" looked identical on stderr. The fallback fires
/// only when the socket address was explicitly given (flag or
/// `$TASQX_SOCK`) and nothing answers there; `bin(&w)` always sets
/// `$TASQX_SOCK`, matching the audit's repro.
#[test]
fn a_dead_daemon_with_no_retirement_note_still_says_it_fell_back() {
    let w = world("deadnomark");
    // No daemon ever started on `w.sock`, and no retirement marker planted:
    // the socket simply has nobody listening, as `kill -9` leaves it.

    let out = bin(&w).args(["list"]).output().expect("run list");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "list in-process fallback: {stderr}");
    assert!(
        stderr.contains(&format!("no daemon at {}", w.sock)),
        "the silent fallback must say so, naming the socket: {stderr}"
    );
    assert!(
        stderr.contains("running in-process against $TASQX_DB"),
        "the note must name what happens instead: {stderr}"
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

/// The mirror of the test above, narrowed by D106: with NO `--socket` flag,
/// the same missing daemon (env/default discovery via `$TASQX_SOCK`) must
/// stay silent about #14's specific WARNING wording ("this command ran
/// in-process instead") — that one stays scoped to an explicit `--socket`
/// flag, a stated intention a bare env var is not. But D106 gave `$TASQX_SOCK`
/// its own, different NOTE ("running in-process against $TASQX_DB") once a
/// killed daemon otherwise looked identical to no daemon configured at all —
/// so this path is no longer silent outright, just silent about the flag-only
/// warning.
#[test]
fn an_unreachable_socket_from_env_discovery_gets_the_note_not_the_warning() {
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
        !stderr.contains("this command ran in-process instead"),
        "env/default discovery must not get #14's explicit-flag-only warning: {stderr}"
    );
    assert!(
        stderr.contains("no daemon at") && stderr.contains("running in-process against $TASQX_DB"),
        "env discovery gets D106's note instead, so a killed daemon does not look silent: {stderr}"
    );
}

/// Does a `list` against this command's target show `title`? `--json` keeps
/// the check off the rendered table's truncation.
fn lists(mut c: Command, title: &str) -> bool {
    let out = c.args(["--json", "list"]).output().expect("run list");
    assert!(
        out.status.success(),
        "list: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).contains(title)
}

/// The daemon's own store, read through it with no `$TASQX_DB` in the way.
fn via_daemon(w: &World) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", &w.config_dir)
        .env("TASQX_SOCK", &w.sock)
        .env_remove("TASQX_DB");
    c
}

/// D204 case 1: `$TASQX_DB` naming the daemon's own store — spelled
/// differently, so the canonical compare is what decides — routes through the
/// daemon exactly as before, and says nothing: the variable IS in effect.
#[test]
fn tasqx_db_naming_the_daemons_own_store_routes_through_it_silently() {
    let w = world("samedb");
    let shutdown = start_daemon(&w);
    let spelled = w
        .daemon_db
        .parent()
        .expect("parent")
        .join(".")
        .join(w.daemon_db.file_name().expect("file name"));

    let out = bin(&w)
        .env("TASQX_DB", &spelled)
        .args(["add", "d204 same store"])
        .output()
        .expect("run add");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "add through the daemon: {stderr}");
    assert!(
        !stderr.contains("tasqx: note"),
        "$TASQX_DB names the daemon's store, so there is nothing to note: {stderr}"
    );
    assert!(
        lists(via_daemon(&w), "d204 same store"),
        "the write must land in the daemon's store"
    );

    let out = bin(&w)
        .env("TASQX_DB", &spelled)
        .args(["--json", "config", "store"])
        .output()
        .expect("run config store");
    shutdown.store(true, Ordering::Relaxed);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("config store --json");
    assert_eq!(json["backend"], "daemon", "{json}");
    assert_eq!(json["routing"], "daemon_same_store", "{json}");
}

/// D204 case 3: `$TASQX_DB` and an explicit socket (`$TASQX_SOCK`, then
/// `--socket`) that disagree are refused with exit 2, naming both files —
/// and neither store is written.
#[test]
fn tasqx_db_disagreeing_with_an_explicit_socket_is_refused() {
    let w = world("refused");
    let shutdown = start_daemon(&w);
    let daemon_name = w
        .daemon_db
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned();

    let by_env = bin(&w)
        .args(["add", "d204 refused"])
        .output()
        .expect("run add");
    let by_flag = bin(&w)
        .env_remove("TASQX_SOCK")
        .args(["--socket", &w.sock, "add", "d204 refused"])
        .output()
        .expect("run add --socket");
    for (how, out) in [("$TASQX_SOCK", &by_env), ("--socket", &by_flag)] {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{how}: {stderr}");
        assert!(stderr.contains("error [bad_request]"), "{how}: {stderr}");
        assert!(
            stderr.contains(&*w.env_db.to_string_lossy()) && stderr.contains(&daemon_name),
            "{how}: the refusal must name both stores: {stderr}"
        );
        assert!(
            stderr.contains("--no-daemon"),
            "{how}: and the way out: {stderr}"
        );
    }
    assert!(
        !lists(via_daemon(&w), "d204 refused"),
        "the daemon's store must not be written"
    );
    shutdown.store(true, Ordering::Relaxed);
    assert!(
        !w.env_db.exists(),
        "$TASQX_DB's store must not be created either"
    );
}

/// D204 cases 2 and 4: a daemon found only on the DEFAULT socket that serves a
/// different store than `$TASQX_DB` is not routed through — the command runs
/// in-process against `$TASQX_DB`, silently, and `config store` says why. The
/// default socket is moved under a scratch `$HOME` (short, under `/tmp`, for
/// the Unix socket path limit); Windows' default is a fixed pipe name, so
/// this is `#[cfg(unix)]` like the `#184` tests.
#[cfg(unix)]
#[test]
fn a_default_socket_daemon_on_another_store_is_bypassed_for_tasqx_db() {
    use std::io::BufRead;
    let w = world("defsock");
    let home = PathBuf::from(format!("/tmp/tq204-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create scratch HOME");
    let at_home = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_CONFIG_DIR", &w.config_dir)
            .env("HOME", &home)
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("XDG_DATA_HOME")
            .env_remove("TASQX_SOCK")
            .env_remove("TASQX_DB");
        c
    };

    let mut child = at_home()
        .args(["daemon", "--db", &w.daemon_db.to_string_lossy()])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn daemon");
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(rest) = line.strip_prefix("tasqx daemon: listening on ") {
                let _ = tx.send(rest.trim_end_matches(" (Ctrl-C to stop)").to_string());
            }
        }
    });
    let sock = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the daemon must announce its default socket");

    let mut add = at_home();
    add.env("TASQX_DB", &w.env_db)
        .args(["add", "d204 bypassed"]);
    let out = add.output().expect("run add");
    let add_stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    let mut local = at_home();
    local.env("TASQX_DB", &w.env_db).arg("--no-daemon");
    let in_env_db = lists(local, "d204 bypassed");
    let in_daemon = lists(at_home(), "d204 bypassed");

    let mut store = at_home();
    store
        .env("TASQX_DB", &w.env_db)
        .args(["--json", "config", "store"]);
    let store_out = store.output().expect("run config store");

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&home);

    assert!(out.status.success(), "add: {add_stderr}");
    assert!(
        add_stderr.is_empty(),
        "the bypass is silent (D204): {add_stderr}"
    );
    assert!(in_env_db, "the write must land in $TASQX_DB's store");
    assert!(!in_daemon, "the daemon's store must not be written");
    let json: serde_json::Value =
        serde_json::from_slice(&store_out.stdout).expect("config store --json");
    assert_eq!(json["backend"], "local", "{json}");
    assert_eq!(json["routing"], "local_tasqx_db_differs", "{json}");
    assert_eq!(json["bypassed_daemon"]["socket"], sock.as_str(), "{json}");
}

/// #1122/D208: `mcp serve` with `$TASQX_SOCK` set says once, on stderr, that
/// the variable is ignored; stdout stays the JSON-RPC channel.
#[test]
fn mcp_serve_says_an_ambient_socket_is_ignored() {
    use std::io::Write;
    let w = world("mcpsock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_CONFIG_DIR", &w.config_dir)
        .env("TASQX_SOCK", &w.sock)
        .env("TASQX_DB", &w.env_db)
        .args(["--no-daemon", "mcp", "serve"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx mcp serve");
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#
    )
    .expect("write initialize");
    drop(stdin);
    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let notes: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("TASQX_SOCK"))
        .collect();
    assert_eq!(notes.len(), 1, "exactly one note, got: {stderr}");
    assert!(
        notes[0].contains("ignored") && notes[0].contains("D73") && notes[0].contains("D208"),
        "{}",
        notes[0]
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout.lines().next().expect("a response on stdout");
    let v: serde_json::Value = serde_json::from_str(first).expect("stdout is JSON-RPC");
    assert_eq!(v["id"], 1);
    assert!(v["result"].is_object(), "{v}");
    assert!(
        stdout
            .lines()
            .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()),
        "stdout must carry only JSON-RPC: {stdout}"
    );
}
