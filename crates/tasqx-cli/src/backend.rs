//! How a command reaches a store: the daemon-or-local `Backend`, the socket
//! and store-path resolution behind it, and the direct `Engine` openers the
//! local branch and the pure-read verbs share. Split out of lib.rs (which had
//! grown to ~6,000 lines) along the seam its own section comments drew; the
//! observable behavior — routing, D74's $TASQX_DB note, the retirement
//! marker dance — is pinned by the daemon integration suites.

use super::*;

/// Where a one-shot command's dispatch runs: in-process against a local
/// [`Engine`] (default / no daemon), or over the socket against a running
/// daemon (single writer). Both return the identical `result` value, so every
/// `run_*` renders the same regardless of transport.
pub(crate) enum Backend {
    Local(Engine),
    /// The socket is kept alongside the connection because it is the only
    /// honest answer to "where did this write go?" — `config store` reports it,
    /// and recomputing it later would re-resolve the flag/env and could name a
    /// different target than the one actually connected to.
    Remote {
        conn: daemon::Conn,
        socket: String,
    },
}

impl Backend {
    /// The socket this process is routing through, or `None` when it holds the
    /// store itself.
    pub(crate) fn remote_socket(&self) -> Option<&str> {
        match self {
            Backend::Local(_) => None,
            Backend::Remote { socket, .. } => Some(socket.as_str()),
        }
    }
}

impl Backend {
    /// Route one method+params to the core dispatch, locally or via the daemon.
    pub(crate) fn call(&mut self, method: &str, params: &Value) -> Result<Value, ApiError> {
        match self {
            Backend::Local(engine) => dispatch(engine, method, params),
            Backend::Remote { conn, .. } => {
                let mut env = conn
                    .request(method, params)
                    .map_err(|e| ApiError::internal(format!("daemon transport error: {e}")))?;
                if env.get("ok") == Some(&Value::Bool(true)) {
                    // Taken, not cloned: the envelope is owned and about to be
                    // dropped, and for `store.export` through a daemon the
                    // clone was a full copy of the entire store document.
                    Ok(env
                        .as_object_mut()
                        .and_then(|o| o.remove("result"))
                        .unwrap_or(Value::Null))
                } else {
                    Err(api_error_from_env(&env))
                }
            }
        }
    }
}

/// Reconstruct a typed [`ApiError`] from a daemon error-response envelope, so a
/// routed command yields the same exit code + message as the in-process path.
pub(crate) fn api_error_from_env(env: &Value) -> ApiError {
    let body = env.get("error");
    let code = match body.and_then(|b| b.get("code")).and_then(Value::as_str) {
        Some("not_found") => ErrorCode::NotFound,
        Some("conflict") => ErrorCode::Conflict,
        Some("unsupported_version") => ErrorCode::UnsupportedVersion,
        Some("internal") => ErrorCode::Internal,
        _ => ErrorCode::BadRequest,
    };
    let message = body
        .and_then(|b| b.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("daemon error")
        .to_string();
    let data = body.and_then(|b| b.get("data")).cloned();
    ApiError::new(code, message, data)
}

/// Resolve the socket address: `--socket` > `$TASQX_SOCK` > platform default.
pub(crate) fn resolve_socket(flag: Option<&str>) -> String {
    flag.map(str::to_string)
        .or_else(|| std::env::var("TASQX_SOCK").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(default_socket)
}

/// The stable default socket address (DESIGN.md §2). Documented targets:
///  * Windows: the named pipe `\\.\pipe\tasqx-default`.
///  * Linux:   `$XDG_RUNTIME_DIR/tasqx/tasqx.sock` (falls back to the data dir).
///  * macOS:   `<data dir>/tasqx.sock` (no runtime dir on macOS).
pub(crate) fn default_socket() -> String {
    #[cfg(windows)]
    {
        "tasqx-default".to_string()
    }
    #[cfg(unix)]
    {
        if let Some(dirs) = directories::ProjectDirs::from("dev", "tasqx", "tasqx") {
            if let Some(rt) = dirs.runtime_dir() {
                return rt.join("tasqx.sock").to_string_lossy().into_owned();
            }
            return dirs
                .data_dir()
                .join("tasqx.sock")
                .to_string_lossy()
                .into_owned();
        }
        "/tmp/tasqx.sock".to_string()
    }
}

/// Build the backend: prefer a reachable daemon (single writer), else fall back
/// to a direct in-process Engine — the pre-daemon behaviour, unchanged. A
/// missing/stale socket falls back immediately (no hang).
pub(crate) fn open_backend(socket_flag: Option<&str>, no_daemon: bool) -> Result<Backend, String> {
    if !no_daemon {
        let target = resolve_socket(socket_flag);
        if let Some(conn) = daemon::try_connect(&target) {
            return Ok(Backend::Remote {
                conn,
                socket: target,
            });
        }
        // The connect failed and this command is about to address a different
        // store than the last one did, if a daemon recently retired here. Say
        // so once (D74). Deliberately not on the `--no-daemon` path: there the
        // operator chose the in-process store themselves.
        report_daemon_retirement(&target);
    }
    Ok(Backend::Local(open_engine()?))
}

/// Where an idle-retired daemon leaves its note (D74): beside `config.toml`,
/// keyed by the socket — the D57 marker precedent, because state lives in a
/// sibling file, never inside the user's config. The socket is sanitized for a
/// filename (a Unix socket address is a path) and recorded verbatim inside the
/// file; the content compare in [`report_daemon_retirement`] is what is
/// trusted, the filename is only a rendezvous.
pub(crate) fn daemon_retired_marker(socket: &str) -> Option<PathBuf> {
    let sanitized: String = socket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    config::config_dir().map(|d| d.join(format!("daemon-retired-{sanitized}")))
}

/// D74's client half: the first command whose daemon connect fails reads the
/// retirement note, reports the transition on stderr, and consumes the marker
/// so the note is said once — D57's marker-then-print discipline, inverted
/// (print-then-delete, because a note that cannot be delivered should survive
/// for the next command to try).
///
/// The recorded socket must equal the one this command resolved: two addresses
/// can sanitize to one filename, and a note about a different daemon is not
/// this command's transition.
pub(crate) fn report_daemon_retirement(target: &str) {
    let Some(marker) = daemon_retired_marker(target) else {
        return;
    };
    let Ok(body) = std::fs::read_to_string(&marker) else {
        return;
    };
    let field = |key: &str| {
        body.lines()
            .find_map(|l| l.strip_prefix(key).map(str::trim))
            .filter(|v| !v.is_empty())
    };
    if field("socket ") != Some(target) {
        return;
    }
    let owned = field("store ").filter(|s| *s != "-");
    let when = field("retired ");
    eprintln!(
        "tasqx: note: the daemon at {target} left on its idle timeout{}{}; this and later \
         commands open the local store in-process — `tasqx config store` names it",
        when.map(|w| format!(" at {w}")).unwrap_or_default(),
        owned
            .map(|s| format!(" and its store {s} is no longer being served"))
            .unwrap_or_default(),
    );
    let _ = std::fs::remove_file(&marker);
}

/// Resolve the store path and open the engine.
pub(crate) fn open_engine() -> Result<Engine, String> {
    let path = db_path()?;
    Engine::open(&path.to_string_lossy()).map_err(|e| e.message)
}

/// Open an Engine at an explicit `--db` path (the daemon), else the default
/// store. Creates parent directories so a fresh `--db path/to/tasks.db` works.
pub(crate) fn open_engine_at(db: Option<&str>) -> Result<Engine, String> {
    match db {
        Some(p) => {
            if let Some(parent) = PathBuf::from(p).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            Engine::open(p).map_err(|e| e.message)
        }
        None => open_engine(),
    }
}

/// Answer "which store does this process actually write to?".
///
/// All inputs are passed rather than read here, so both daemon branches are
/// reachable from a test without a listening socket.
///
/// The two cases are NOT variations on one sentence. In-process, the local file
/// IS the store. Through a daemon, it is not: `open_backend` prefers a reachable
/// daemon and the remote path never consults `TASQX_DB`, so the local path is
/// inert and reporting it would restate the exact falsehood this surface exists
/// to kill. The local `path` is therefore still absent on the daemon branch —
/// but the daemon's own file is not guessed, it is `daemon_store`: the caller
/// asked the daemon (`core.capabilities.store`, D74), which retires D47's "a
/// client cannot know the daemon's file" without touching what D47 actually
/// forbade. `None` here means an older daemon that predates the field, and the
/// answer degrades to naming the socket alone rather than inventing a path.
pub(crate) fn store_location(
    remote_socket: Option<&str>,
    daemon_store: Option<&str>,
    path: Result<PathBuf, String>,
) -> (Value, String) {
    if let Some(socket) = remote_socket {
        let owns = match daemon_store {
            Some(store) => format!("the daemon owns the store: {store}"),
            None => "the daemon owns the store; it predates D74 and cannot name it".to_string(),
        };
        return (
            json!({
                "backend": "daemon",
                "socket": socket,
                "store": daemon_store.map(str::to_string),
            }),
            format!(
                "daemon at {socket}\n  {owns}\n  $TASQX_DB is NOT in effect here. \
                 Pass --no-daemon to work on your own store instead.\n"
            ),
        );
    }
    match path {
        Ok(p) => {
            let p = p.to_string_lossy().into_owned();
            (
                json!({ "backend": "local", "path": p }),
                format!("{p}\n  in-process; this file is the store.\n"),
            )
        }
        // A path this process could not resolve is a fact, not an omission: it
        // is exactly the state in which every later command fails, and naming it
        // here is cheaper than reading that failure off a write.
        Err(e) => (
            json!({ "backend": "local", "path": Value::Null, "error": e }),
            format!("(no store path: {e})\n"),
        ),
    }
}

pub(crate) fn db_path() -> Result<PathBuf, String> {
    db_path_resolved(true)
}

/// The store path a command WOULD open, resolved without touching the disk.
///
/// The completion callback (`complete::lookup`) needs the same answer
/// [`db_path`] gives and none of its side effects. That is not a preference:
/// the whole promise of the `$TASQX_COMPLETE` path is that pressing Tab creates
/// nothing, and [`db_path`] creates the parent directory of `$TASQX_DB` and the
/// platform data directory before it returns. A Tab press on a machine that has
/// never run tasqx would therefore author `%APPDATA%\tasqx\tasqx\data\`, and
/// `tests/completion.rs` asserts against the filesystem that it does not.
///
/// Split off [`db_path`] rather than written beside it, because the two must
/// agree about WHERE the store is or completion offers ids from a store no
/// command reads. A second copy of the `$TASQX_DB`-then-`ProjectDirs` rule is
/// exactly the parallel-copy drift this repository keeps paying for; the
/// difference between the two callers is one boolean and nothing else.
pub(crate) fn db_path_read_only() -> Result<PathBuf, String> {
    db_path_resolved(false)
}

/// `$TASQX_DB` if set and non-empty, else the platform data dir. `create_dirs`
/// decides whether the containing directory is brought into existence on the
/// way — see [`db_path_read_only`] for why that is a caller's choice rather
/// than a fixed behaviour.
pub(crate) fn db_path_resolved(create_dirs: bool) -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("TASQX_DB") {
        if !p.is_empty() {
            if create_dirs {
                if let Some(parent) = PathBuf::from(&p).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            return Ok(PathBuf::from(p));
        }
    }
    let dirs = directories::ProjectDirs::from("dev", "tasqx", "tasqx")
        .ok_or_else(|| "cannot determine a data directory".to_string())?;
    let dir = dirs.data_dir();
    if create_dirs {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create data dir: {e}"))?;
    }
    Ok(dir.join("tasks.db"))
}
