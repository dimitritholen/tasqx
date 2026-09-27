//! `tasqx sync` (D201): one store, one remote, and a loop of pull, merge, push.
//!
//! The local SQLite file stays the store. The remote is a connector
//! (`tasqx-remote-<name>` on PATH, D198) that holds snapshots — whole
//! `store.export` documents — and `sync` is a client of the same dispatch as
//! every other verb: it merges through `store.import` with `merge: true`
//! (D185, D189, D197) and reads the result back through `store.export`, both
//! via [`Backend`], so a running daemon stays the single writer and nothing
//! here opens the store beside it. Both are handed a FILE path (`path`,
//! `out_path`), never the document inline: a real store's export is several
//! megabytes and a daemon refuses a request frame over 1 MiB.
//!
//! The store's remote is its own: `sync setup` records the connector's name
//! in `<store>.sync.json` beside the store, where each sync then records the
//! version and time it left the remote at. Nothing about sync is in
//! `config.toml`, so two stores on one machine can sync to two remotes.
//!
//! One attempt:
//!
//! 1. pull into a fresh working directory (a connector may write a fixed file
//!    name, so no attempt reuses another's);
//! 2. merge EVERY snapshot the pull returned, in order — a push supersedes all
//!    of them (D198), so one left unmerged would be lost;
//! 3. export, and push with `expected_version` = the first snapshot's version
//!    (`None` when the remote was empty). When the pull returned exactly one
//!    snapshot and the export equals it apart from each task's `_rev` and
//!    `urgency`, nothing is pushed: the remote already holds this store.
//!
//! A conflict means another machine pushed between 1 and 3; the loop starts
//! over at 1, at most [`MAX_ATTEMPTS`] times. Merging is idempotent, so what
//! an abandoned attempt merged stays merged and costs nothing when pulled
//! again. The last synced version and time are recorded only after an
//! attempt ends with the remote holding this store.
//!
//! **The snapshot boundary.** Snapshot bytes are opaque to the connector.
//! [`pull_snapshots`] and [`push_snapshot`] are the only two places bytes
//! cross between this store and the remote — task #883's encryption goes
//! there and nowhere else. Decrypting means writing the plaintext to a file
//! of its own and pointing [`Pulled`]'s `path` at it, since that file is what
//! `store.import` reads.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tasqx_core::remote::{self, Connector, Limits, PushOutcome, Snapshot};
use tasqx_core::ApiError;

use crate::backend::Backend;
use crate::theme::Ctx;

/// Attempts before a sync that keeps losing the race gives up.
pub(crate) const MAX_ATTEMPTS: u32 = 3;

/// What a sync needs from a remote. [`Connector`] is the real one; the tests
/// below drive the loop through a fake that misbehaves on cue.
pub(crate) trait Remote {
    fn pull(&self, out_dir: &Path) -> Result<Vec<Snapshot>, remote::Error>;
    fn push(&self, in_path: &Path, expected: Option<&str>) -> Result<PushOutcome, remote::Error>;
}

impl Remote for Connector {
    fn pull(&self, out_dir: &Path) -> Result<Vec<Snapshot>, remote::Error> {
        Connector::pull(self, out_dir)
    }

    fn push(&self, in_path: &Path, expected: Option<&str>) -> Result<PushOutcome, remote::Error> {
        Connector::push(self, in_path, expected)
    }
}

/// A connector error in the API's vocabulary: an unknown connector is
/// `not_found`, a name that cannot be one is `bad_request`, and everything a
/// running connector can go wrong with — a failed exit, a bad reply, a
/// timeout — is exit 1, a failure this command could not complete.
fn remote_error(e: remote::Error) -> ApiError {
    match e {
        remote::Error::NotFound { .. } => ApiError::not_found(e.to_string(), None),
        remote::Error::BadName { .. } => ApiError::bad_request(e.to_string()),
        other => ApiError::internal(format!("sync: {other}")),
    }
}

fn io_error(what: String) -> impl FnOnce(std::io::Error) -> ApiError {
    move |e| ApiError::internal(format!("sync: {what}: {e}"))
}

/// One pulled snapshot, opened: the remote's version, the export document,
/// and the file holding it, which is what `store.import` is handed.
struct Pulled {
    version: String,
    doc: Vec<u8>,
    path: PathBuf,
}

/// Pull, and read every snapshot back as export bytes. The inbound half of
/// the snapshot boundary (#883 decrypts here).
fn pull_snapshots(remote: &dyn Remote, out_dir: &Path) -> Result<Vec<Pulled>, ApiError> {
    remote
        .pull(out_dir)
        .map_err(remote_error)?
        .into_iter()
        .map(|s| {
            let doc = std::fs::read(&s.path)
                .map_err(io_error(format!("cannot read snapshot {}", s.version)))?;
            Ok(Pulled {
                version: s.version,
                doc,
                path: s.path,
            })
        })
        .collect()
}

/// Write the export into `dir` and push it. The outbound half of the snapshot
/// boundary (#883 encrypts here).
fn push_snapshot(
    remote: &dyn Remote,
    dir: &Path,
    doc: &[u8],
    expected: Option<&str>,
) -> Result<PushOutcome, ApiError> {
    let path = dir.join("snapshot.json");
    std::fs::write(&path, doc).map_err(io_error(format!("cannot write {}", path.display())))?;
    remote.push(&path, expected).map_err(remote_error)
}

/// A directory for one attempt, private to this user, gone when dropped.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(root: &Path, attempt: u32) -> Result<WorkDir, ApiError> {
        let dir = root.join(format!("tasqx-sync-{}-{attempt}", std::process::id()));
        // Never into a directory somebody else made: a leftover of our own
        // (a crashed run under a recycled pid) is cleared first, and then the
        // create is non-recursive, so it fails rather than adopt one.
        let _ = std::fs::remove_dir_all(&dir);
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(false);
        // On Windows a directory under the user's temp dir is already
        // private to them, so only Unix narrows the mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(io_error(format!("cannot create {}", dir.display())))?;
        Ok(WorkDir(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the merges of one sync reported, summed over every attempt.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally {
    snapshots: u64,
    tasks_new: u64,
    tasks_updated: u64,
}

impl Tally {
    /// Count one `store.import` result: a task the store did not hold is new;
    /// one it held is updated when the merge took a field or tracked time from
    /// the snapshot (D189's `from_payload`, `tracked_delta_seconds`).
    fn add(&mut self, result: &Value) {
        let merged = result
            .get("merged")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let imported = result.get("imported").and_then(Value::as_u64).unwrap_or(0);
        self.snapshots += 1;
        self.tasks_new += imported.saturating_sub(merged.len() as u64);
        self.tasks_updated += merged
            .iter()
            .filter(|m| {
                m.get("from_payload")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
                    || m.get("tracked_delta_seconds")
                        .and_then(Value::as_i64)
                        .is_some_and(|d| d != 0)
            })
            .count() as u64;
    }
}

/// Merge one snapshot into the store through `store.import`, `merge: true`.
///
/// By file path, never inline: a real store's export is several megabytes and
/// a daemon refuses a request frame over 1 MiB. The daemon runs as this user
/// on a socket private to this user, so it can read the file we can (D201).
fn merge(be: &mut Backend, snapshot: &Pulled) -> Result<Value, ApiError> {
    let path = std::path::absolute(&snapshot.path).map_err(io_error(format!(
        "cannot resolve snapshot {}",
        snapshot.version
    )))?;
    let params = json!({ "path": path.to_string_lossy(), "merge": true });
    be.call("store.import", &params).map_err(|e| {
        ApiError::new(
            e.code,
            format!("sync: snapshot {}: {}", snapshot.version, e.message),
            e.data,
        )
    })
}

/// Export the store into `dir` by file, for `merge`'s reason, and read it back.
fn export(be: &mut Backend, dir: &Path) -> Result<(Vec<u8>, Value), ApiError> {
    let out = dir.join("export.json");
    be.call(
        "store.export",
        &json!({ "out_path": out.to_string_lossy() }),
    )?;
    let bytes = std::fs::read(&out).map_err(io_error(format!("cannot read {}", out.display())))?;
    let doc = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::internal(format!("sync: the export is not JSON: {e}")))?;
    Ok((bytes, doc))
}

/// Per-store fields of a task that an export carries but a merge never takes
/// from the payload: `_rev` is this store's own concurrency counter (a merge
/// skips its guard, D185) and `urgency` is scored at the instant of export.
/// Two machines holding the same tasks export different values for both.
const LOCAL_TASK_FIELDS: [&str; 2] = ["_rev", "urgency"];

/// Whether a pulled snapshot already holds exactly what this store exports,
/// the per-store fields aside. Without that exception two converged machines
/// would each find the other's snapshot different and push on every sync.
fn same_store(snapshot: &[u8], export: &Value) -> bool {
    let strip = |mut v: Value| {
        if let Some(tasks) = v.get_mut("tasks").and_then(Value::as_array_mut) {
            for t in tasks.iter_mut().filter_map(Value::as_object_mut) {
                for k in LOCAL_TASK_FIELDS {
                    t.remove(k);
                }
            }
        }
        v
    };
    serde_json::from_slice::<Value>(snapshot).is_ok_and(|s| strip(s) == strip(export.clone()))
}

/// How one sync ended.
#[derive(Debug)]
struct Synced {
    version: String,
    pushed: bool,
    attempts: u32,
    tally: Tally,
}

/// The loop: pull, merge every snapshot, export, push; again on conflict.
fn run_loop(be: &mut Backend, remote: &dyn Remote, work_root: &Path) -> Result<Synced, ApiError> {
    let mut tally = Tally::default();
    for attempt in 1..=MAX_ATTEMPTS {
        let work = WorkDir::new(work_root, attempt)?;
        let pull_dir = work.0.join("pull");
        let pulled = pull_snapshots(remote, &pull_dir)?;
        for snapshot in &pulled {
            let result = merge(be, snapshot)?;
            tally.add(&result);
        }
        let (doc, export) = export(be, &work.0)?;
        let head = pulled.first().map(|p| p.version.clone());
        if let [only] = pulled.as_slice() {
            if same_store(&only.doc, &export) {
                return Ok(Synced {
                    version: only.version.clone(),
                    pushed: false,
                    attempts: attempt,
                    tally,
                });
            }
        }
        match push_snapshot(remote, &work.0, &doc, head.as_deref())? {
            PushOutcome::Pushed { version } => {
                return Ok(Synced {
                    version,
                    pushed: true,
                    attempts: attempt,
                    tally,
                })
            }
            PushOutcome::Conflict => continue,
        }
    }
    Err(ApiError::conflict(format!(
        "sync: another machine pushed during each of {MAX_ATTEMPTS} attempts, so nothing was \
         pushed. Everything pulled is merged into this store and kept; run `tasqx sync` again"
    )))
}

// ------------------------------------------------------------------- state

/// Where this store's sync state lives: a file beside the store, named after
/// it, so it follows `$TASQX_DB` exactly as the store does and two stores in
/// one directory keep two states.
fn state_path(store: &Path) -> PathBuf {
    let mut name = store.file_name().unwrap_or_default().to_os_string();
    name.push(".sync.json");
    store.with_file_name(name)
}

/// This store's remote and its last successful sync. `sync setup` writes the
/// connector with no version; a sync fills the other two in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    connector: String,
    version: Option<String>,
    synced_at: Option<String>,
}

fn read_state(path: &Path) -> Option<State> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(State {
        connector: field("connector").filter(|c| remote::is_valid_name(c))?,
        version: field("version"),
        synced_at: field("synced_at"),
    })
}

/// Write the state whole or not at all: a temp file beside it, renamed over.
fn write_state(path: &Path, state: &State) -> Result<(), ApiError> {
    let body = json!({
        "connector": state.connector,
        "version": state.version,
        "synced_at": state.synced_at,
    });
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{body:#}\n"))
        .map_err(io_error(format!("cannot write {}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(io_error(format!("cannot write {}", path.display())))
}

/// The store the backend writes to. Through a daemon that is the daemon's
/// file, which only the daemon can name (`core.capabilities.store`, D74) —
/// `$TASQX_DB` is not in effect there, and the connector's state and this
/// store's sync state must sit beside the store that is actually synced.
fn store_path(be: &mut Backend) -> Result<PathBuf, ApiError> {
    let caps = be.call("core.capabilities", &json!({}))?;
    caps.get("store")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            ApiError::bad_request(
                "sync: cannot tell which file this store is (an in-memory store, or a daemon \
                 older than D74), so there is nowhere to keep its sync state",
            )
        })
}

fn connector_at(store: &Path, name: &str) -> Result<Connector, ApiError> {
    let state = remote::state_dir(store, name).map_err(remote_error)?;
    Connector::find(name, state).map_err(remote_error)
}

/// The first eight characters of a version, for a person to compare.
pub(crate) fn short_version(v: &str) -> String {
    v.trim_matches('"').chars().take(8).collect()
}

// ---------------------------------------------------------------- commands

/// `tasqx sync`.
pub(crate) fn run(be: &mut Backend, ctx: &Ctx) -> crate::CmdOutcome {
    let store = store_path(be)?;
    let Some(name) = read_state(&state_path(&store)).map(|s| s.connector) else {
        return Err(not_set_up());
    };
    let connector = connector_at(&store, &name)?.with_limits(Limits {
        timeout: remote::TRANSFER_TIMEOUT,
        ..Limits::default()
    });
    let synced = run_loop(be, &connector, &std::env::temp_dir())?;
    let state = State {
        connector: name,
        version: Some(synced.version),
        synced_at: Some(crate::clock::now().to_string()),
    };
    write_state(&state_path(&store), &state)?;
    let result = json!({
        "connector": state.connector,
        "version": state.version,
        "pushed": synced.pushed,
        "attempts": synced.attempts,
        "snapshots_merged": synced.tally.snapshots,
        "tasks_new": synced.tally.tasks_new,
        "tasks_updated": synced.tally.tasks_updated,
        "synced_at": state.synced_at,
    });
    let text = crate::render::synced(ctx, &result);
    Ok((result, text))
}

fn not_set_up() -> ApiError {
    ApiError::bad_request(
        "sync is not set up: run `tasqx sync setup <connector>` to choose a remote \
         (`tasqx sync setup dir` keeps it in a shared folder)",
    )
}

/// `tasqx sync --status`.
pub(crate) fn status(be: &mut Backend, ctx: &Ctx) -> crate::CmdOutcome {
    let state = read_state(&state_path(&store_path(be)?));
    let result = json!({
        "set_up": state.is_some(),
        "connector": state.as_ref().map(|s| s.connector.clone()),
        "version": state.as_ref().and_then(|s| s.version.clone()),
        "synced_at": state.as_ref().and_then(|s| s.synced_at.clone()),
    });
    let text = crate::render::sync_status(ctx, &result, crate::clock::now());
    Ok((result, text))
}

/// `tasqx sync setup <connector> [--set key=value]…`.
pub(crate) fn setup(be: &mut Backend, ctx: &Ctx, name: &str, set: &[String]) -> crate::CmdOutcome {
    let mut given = remote::Values::new();
    for pair in set {
        let Some((key, value)) = pair.split_once('=').filter(|(k, _)| !k.trim().is_empty()) else {
            return Err(ApiError::bad_request(format!(
                "--set {pair:?} is not KEY=VALUE"
            )));
        };
        given.insert(key.trim().to_string(), value.to_string());
    }
    let store = store_path(be)?;
    let connector = connector_at(&store, name)?;
    let description = connector.describe().map_err(remote_error)?;
    let interactive = {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal()
    };
    let ask = |f: &remote::Field| prompt::ask(f);
    let env = |k: &str| std::env::var(k).ok();
    let given = collect_values(
        name,
        &description.fields,
        given,
        &env,
        interactive.then_some(&ask as &Ask),
    )?;
    match connector.configure(&given).map_err(remote_error)? {
        remote::Configured::Rejected(error) => Err(ApiError::bad_request(format!(
            "{name} refused these settings: {error}"
        ))),
        remote::Configured::Ok => {
            // The same connector set up again keeps its last sync; another one
            // starts with none, since that version names a different remote.
            let path = state_path(&store);
            let (version, synced_at) = match read_state(&path) {
                Some(old) if old.connector == name => (old.version, old.synced_at),
                _ => (None, None),
            };
            write_state(
                &path,
                &State {
                    connector: name.to_string(),
                    version,
                    synced_at,
                },
            )?;
            let result = json!({
                "connector": name,
                "connector_version": description.version,
                "state": path.to_string_lossy(),
            });
            let text = crate::render::sync_set_up(ctx, &result);
            Ok((result, text))
        }
    }
}

/// The environment variable a secret field can be given in, off a terminal:
/// `TASQX_SYNC_<KEY>`, the key upper-cased with anything but a letter or a
/// digit made `_`. Read once by `sync setup`, handed to `configure`, never
/// stored by tasqx.
fn secret_env(key: &str) -> String {
    let key: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("TASQX_SYNC_{key}")
}

/// How a field is asked for on the terminal.
type Ask = dyn Fn(&remote::Field) -> Result<String, ApiError>;

/// The answers to hand `configure`, from `--set`, a secret's env var, and —
/// when `ask` is given, on a terminal — the prompt.
///
/// A secret field is refused in `--set`: argv lands in shell history and in
/// `ps`, where a token must never be. Its two ways in are the no-echo prompt
/// and [`secret_env`].
fn collect_values(
    name: &str,
    fields: &[remote::Field],
    mut given: remote::Values,
    env: &dyn Fn(&str) -> Option<String>,
    ask: Option<&Ask>,
) -> Result<remote::Values, ApiError> {
    let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
    if let Some(unknown) = given.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(ApiError::bad_request(format!(
            "{name} has no setting {unknown:?}; it asks for: {}",
            keys.join(", ")
        )));
    }
    if let Some(f) = fields
        .iter()
        .find(|f| f.secret && given.contains_key(&f.key))
    {
        return Err(ApiError::bad_request(format!(
            "{} ({}) is secret, and --set would leave it in your shell history and in `ps`: \
             run `tasqx sync setup {name}` on a terminal to be asked for it with the prompt, \
             which does not echo, or put it in the environment variable {}",
            f.key,
            f.label,
            secret_env(&f.key)
        )));
    }
    for f in fields.iter().filter(|f| f.secret) {
        if let Some(v) = env(&secret_env(&f.key)) {
            given.insert(f.key.clone(), v);
        }
    }
    let missing: Vec<&remote::Field> = fields
        .iter()
        .filter(|f| !given.contains_key(&f.key))
        .collect();
    if missing.is_empty() {
        return Ok(given);
    }
    let Some(ask) = ask else {
        let named: Vec<String> = missing
            .iter()
            .map(|f| format!("{} ({})", f.key, f.label))
            .collect();
        let ways: Vec<String> = missing
            .iter()
            .map(|f| {
                if f.secret {
                    format!("{}=<value> in the environment", secret_env(&f.key))
                } else {
                    format!("--set {}=<value>", f.key)
                }
            })
            .collect();
        return Err(ApiError::bad_request(format!(
            "{name} needs {} and stdin is not a terminal to ask on: pass {}",
            named.join(", "),
            ways.join(", ")
        )));
    };
    for field in missing {
        let value = ask(field)?;
        given.insert(field.key.clone(), value);
    }
    Ok(given)
}

/// Asking for a field on the terminal: plain text echoes, a secret does not.
mod prompt {
    use std::io::{BufRead, Write};

    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use ratatui::crossterm::terminal;
    use tasqx_core::remote::Field;
    use tasqx_core::ApiError;

    fn failed(e: std::io::Error) -> ApiError {
        ApiError::internal(format!("cannot read from the terminal: {e}"))
    }

    pub(super) fn ask(field: &Field) -> Result<String, ApiError> {
        let mut err = std::io::stderr();
        let _ = writeln!(err, "{}", field.help);
        if let Some(url) = &field.help_url {
            let _ = writeln!(err, "  {url}");
        }
        let _ = write!(err, "{}: ", field.label);
        let _ = err.flush();
        if field.secret {
            return secret();
        }
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(failed)?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }

    /// Raw mode leaves the terminal on every way out of here.
    struct Raw;

    impl Drop for Raw {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
            eprintln!();
        }
    }

    fn secret() -> Result<String, ApiError> {
        terminal::enable_raw_mode().map_err(failed)?;
        let _raw = Raw;
        let mut value = String::new();
        loop {
            let Event::Key(key) = event::read().map_err(failed)? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Enter => return Ok(value),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(ApiError::bad_request("setup cancelled"));
                }
                KeyCode::Esc => return Err(ApiError::bad_request("setup cancelled")),
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tasqx_core::Engine;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tasqx-sync-unit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A store on disk, as the command would have it.
    fn store(dir: &Path, name: &str) -> Backend {
        let path = dir.join(format!("{name}.db"));
        Backend::Local(Engine::open(path.to_str().unwrap()).unwrap())
    }

    fn add(be: &mut Backend, title: &str) {
        if be.call("project.list", &json!({})).unwrap()["projects"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            be.call("project.create", &json!({"name": "home"})).unwrap();
        }
        be.call("task.add", &json!({"title": title})).unwrap();
    }

    fn titles(be: &mut Backend) -> Vec<String> {
        let mut t: Vec<String> = be.call("store.export", &json!({})).unwrap()["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap().to_string())
            .collect();
        t.sort();
        t
    }

    /// A document another machine would have pushed, holding these tasks.
    fn snapshot_of(titles: &[&str]) -> Vec<u8> {
        let mut other = Backend::Local(Engine::open_in_memory().unwrap());
        for t in titles {
            add(&mut other, t);
        }
        serde_json::to_vec(&other.call("store.export", &json!({})).unwrap()).unwrap()
    }

    type Hook = Box<dyn FnMut(&mut Vec<(String, Vec<u8>)>)>;

    /// An in-memory remote. `snapshots` is what a pull returns, first = HEAD.
    /// `before_push` runs inside every push, before the version check — the
    /// seat of another machine racing this one.
    struct Fake {
        snapshots: RefCell<Vec<(String, Vec<u8>)>>,
        pushes: RefCell<Vec<Option<String>>>,
        next: RefCell<u32>,
        before_push: RefCell<Option<Hook>>,
        fail_push: bool,
    }

    impl Fake {
        fn new(snapshots: Vec<(String, Vec<u8>)>) -> Fake {
            Fake {
                snapshots: RefCell::new(snapshots),
                pushes: RefCell::new(Vec::new()),
                next: RefCell::new(0),
                before_push: RefCell::new(None),
                fail_push: false,
            }
        }
    }

    impl Remote for Fake {
        fn pull(&self, out_dir: &Path) -> Result<Vec<Snapshot>, remote::Error> {
            std::fs::create_dir_all(out_dir).unwrap();
            // A fixed file name per slot, as tasqx-remote-r2 writes one: a
            // reused directory would hand back the previous attempt's bytes.
            let mut out = Vec::new();
            for (i, (version, bytes)) in self.snapshots.borrow().iter().enumerate() {
                let path = out_dir.join(format!("snapshot-{i}"));
                assert!(!path.exists(), "every attempt pulls into a fresh directory");
                std::fs::write(&path, bytes).unwrap();
                out.push(Snapshot {
                    version: version.clone(),
                    path,
                });
            }
            Ok(out)
        }

        fn push(
            &self,
            in_path: &Path,
            expected: Option<&str>,
        ) -> Result<PushOutcome, remote::Error> {
            self.pushes.borrow_mut().push(expected.map(str::to_string));
            if self.fail_push {
                return Err(remote::Error::Failed {
                    program: "tasqx-remote-fake".into(),
                    verb: "push",
                    code: Some(1),
                    stderr: "the link dropped".into(),
                });
            }
            if let Some(hook) = self.before_push.borrow_mut().as_mut() {
                hook(&mut self.snapshots.borrow_mut());
            }
            let head = self.snapshots.borrow().first().map(|s| s.0.clone());
            if head.as_deref() != expected {
                return Ok(PushOutcome::Conflict);
            }
            let mut n = self.next.borrow_mut();
            *n += 1;
            let version = format!("v{n}");
            let bytes = std::fs::read(in_path).unwrap();
            *self.snapshots.borrow_mut() = vec![(version.clone(), bytes)];
            Ok(PushOutcome::Pushed { version })
        }
    }

    #[test]
    fn an_empty_remote_is_pushed_to_with_no_expected_version() {
        let dir = scratch("empty");
        let mut be = store(&dir, "a");
        add(&mut be, "Water the plants");
        let fake = Fake::new(Vec::new());
        let done = run_loop(&mut be, &fake, &dir).unwrap();
        assert!(done.pushed);
        assert_eq!(done.attempts, 1);
        assert_eq!(*fake.pushes.borrow(), vec![None]);
        assert_eq!(done.version, "v1");
    }

    #[test]
    fn every_snapshot_is_merged_and_the_push_expects_the_first() {
        let dir = scratch("every");
        let mut be = store(&dir, "a");
        add(&mut be, "Local");
        let fake = Fake::new(vec![
            ("head".into(), snapshot_of(&["From head"])),
            ("copy".into(), snapshot_of(&["From a conflict copy"])),
        ]);
        let done = run_loop(&mut be, &fake, &dir).unwrap();
        assert_eq!(
            titles(&mut be),
            ["From a conflict copy", "From head", "Local"]
        );
        assert_eq!(*fake.pushes.borrow(), vec![Some("head".to_string())]);
        assert_eq!(done.tally.snapshots, 2);
        assert_eq!(done.tally.tasks_new, 2);
    }

    #[test]
    fn a_remote_that_already_holds_this_store_is_not_pushed_to() {
        let dir = scratch("same");
        let mut be = store(&dir, "a");
        add(&mut be, "Local");
        let mine = serde_json::to_vec(&be.call("store.export", &json!({})).unwrap()).unwrap();
        let fake = Fake::new(vec![("head".into(), mine)]);
        let done = run_loop(&mut be, &fake, &dir).unwrap();
        assert!(!done.pushed);
        assert_eq!(done.version, "head");
        assert!(fake.pushes.borrow().is_empty());
    }

    #[test]
    fn a_conflict_starts_over_from_the_pull_and_merges_what_raced_in() {
        let dir = scratch("retry");
        let mut be = store(&dir, "a");
        add(&mut be, "Local");
        let fake = Fake::new(vec![("v0".into(), snapshot_of(&["Theirs"]))]);
        let raced = snapshot_of(&["Theirs, later"]);
        let mut once = Some(raced);
        *fake.before_push.borrow_mut() = Some(Box::new(move |snaps| {
            if let Some(bytes) = once.take() {
                *snaps = vec![("raced".into(), bytes)];
            }
        }));
        let done = run_loop(&mut be, &fake, &dir).unwrap();
        assert!(done.pushed);
        assert_eq!(done.attempts, 2);
        assert_eq!(
            *fake.pushes.borrow(),
            vec![Some("v0".to_string()), Some("raced".to_string())]
        );
        assert_eq!(titles(&mut be), ["Local", "Theirs", "Theirs, later"]);
        // What landed on the remote is this store, all three tasks of it.
        let pushed: Value = serde_json::from_slice(&fake.snapshots.borrow()[0].1).unwrap();
        assert_eq!(pushed["tasks"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_remote_that_keeps_moving_fails_after_three_attempts_and_keeps_the_merges() {
        let dir = scratch("persistent");
        let mut be = store(&dir, "a");
        add(&mut be, "Local");
        let fake = Fake::new(vec![("v0".into(), snapshot_of(&["Theirs"]))]);
        let mut n = 0;
        *fake.before_push.borrow_mut() = Some(Box::new(move |snaps| {
            n += 1;
            let bytes = snaps[0].1.clone();
            *snaps = vec![(format!("moved{n}"), bytes)];
        }));
        let err = run_loop(&mut be, &fake, &dir).unwrap_err();
        assert_eq!(err.code, tasqx_core::ErrorCode::Conflict, "{err}");
        assert!(err.message.contains("run `tasqx sync` again"), "{err}");
        assert_eq!(fake.pushes.borrow().len(), MAX_ATTEMPTS as usize);
        assert_eq!(titles(&mut be), ["Local", "Theirs"], "the merge is kept");
    }

    #[test]
    fn a_push_that_fails_records_nothing_and_keeps_only_the_merge() {
        let dir = scratch("failed");
        let db = dir.join("a.db");
        let mut be = Backend::Local(Engine::open(db.to_str().unwrap()).unwrap());
        add(&mut be, "Local");
        let mut fake = Fake::new(vec![("v0".into(), snapshot_of(&["Theirs"]))]);
        fake.fail_push = true;
        let err = run_loop(&mut be, &fake, &dir).unwrap_err();
        assert_eq!(err.code, tasqx_core::ErrorCode::Internal, "{err}");
        assert!(err.message.contains("the link dropped"), "{err}");
        assert_eq!(titles(&mut be), ["Local", "Theirs"]);
        assert!(!state_path(&db).exists(), "no sync recorded");
        assert_eq!(
            fake.snapshots.borrow()[0].0,
            "v0",
            "the remote is untouched"
        );
    }

    #[test]
    fn working_directories_are_gone_after_a_sync() {
        let dir = scratch("clean");
        let mut be = store(&dir, "a");
        add(&mut be, "Local");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let fake = Fake::new(vec![("v0".into(), snapshot_of(&["Theirs"]))]);
        run_loop(&mut be, &fake, &work).unwrap();
        assert_eq!(std::fs::read_dir(&work).unwrap().count(), 0);
    }

    #[test]
    fn a_snapshot_that_is_not_an_export_is_refused_by_version() {
        let dir = scratch("junk");
        let mut be = store(&dir, "a");
        let fake = Fake::new(vec![("v9".into(), b"{\"nope\": 1}".to_vec())]);
        let err = run_loop(&mut be, &fake, &dir).unwrap_err();
        assert!(err.message.contains("v9"), "{err}");
        assert!(fake.pushes.borrow().is_empty());
    }

    #[test]
    fn state_sits_beside_the_store_and_round_trips() {
        let dir = scratch("state");
        let path = state_path(&dir.join("tasks.db"));
        assert_eq!(path, dir.join("tasks.db.sync.json"));
        let s = State {
            connector: "dir".into(),
            version: Some("abc".into()),
            synced_at: Some("2026-09-27T10:00:00Z".into()),
        };
        write_state(&path, &s).unwrap();
        assert_eq!(read_state(&path), Some(s));
        let set_up_only = State {
            connector: "r2".into(),
            version: None,
            synced_at: None,
        };
        write_state(&path, &set_up_only).unwrap();
        assert_eq!(read_state(&path), Some(set_up_only));
    }

    #[test]
    fn merge_ignores_a_dry_run_the_document_asks_for() {
        let dir = scratch("dryrun");
        let mut be = store(&dir, "a");
        let mut doc: Value = serde_json::from_slice(&snapshot_of(&["Theirs"])).unwrap();
        doc["dry_run"] = json!(true);
        let path = dir.join("snap.json");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let pulled = Pulled {
            version: "v".into(),
            doc: Vec::new(),
            path,
        };
        merge(&mut be, &pulled).unwrap();
        assert_eq!(titles(&mut be), ["Theirs"]);
    }

    #[test]
    fn a_snapshot_differing_only_in_rev_and_urgency_is_the_same_store() {
        let doc = |rev: i64, urgency: f64, title: &str| json!({"tasks": [{"id": "t", "_rev": rev, "urgency": urgency, "title": title}]});
        let snap = serde_json::to_vec(&doc(5, 6.0, "a")).unwrap();
        assert!(same_store(&snap, &doc(4, 7.5, "a")));
        assert!(!same_store(&snap, &doc(5, 6.0, "b")));
        assert!(!same_store(b"not json", &doc(5, 6.0, "a")));
    }

    fn field(key: &str, secret: bool) -> remote::Field {
        remote::Field {
            key: key.into(),
            label: key.to_uppercase(),
            secret,
            help: String::new(),
            help_url: None,
        }
    }

    fn given(pairs: &[(&str, &str)]) -> remote::Values {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_secret_comes_from_its_env_var_and_is_refused_in_set() {
        let fields = [field("account", false), field("api-token", true)];
        let env = |k: &str| (k == "TASQX_SYNC_API_TOKEN").then(|| "s3cret".to_string());
        let values = collect_values("x", &fields, given(&[("account", "me")]), &env, None).unwrap();
        assert_eq!(values, given(&[("account", "me"), ("api-token", "s3cret")]));

        let err = collect_values(
            "x",
            &fields,
            given(&[("account", "me"), ("api-token", "s3cret")]),
            &env,
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, tasqx_core::ErrorCode::BadRequest);
        assert!(err.message.contains("api-token"), "{}", err.message);
        assert!(
            err.message.contains("TASQX_SYNC_API_TOKEN"),
            "{}",
            err.message
        );
        assert!(!err.message.contains("s3cret"), "{}", err.message);
    }

    #[test]
    fn a_missing_secret_off_a_terminal_names_its_env_var_and_a_plain_field_names_set() {
        let fields = [field("account", false), field("token", true)];
        let err = collect_values("x", &fields, given(&[]), &|_| None, None).unwrap_err();
        assert!(
            err.message.contains("--set account=<value>"),
            "{}",
            err.message
        );
        assert!(err.message.contains("TASQX_SYNC_TOKEN"), "{}", err.message);
        assert!(!err.message.contains("--set token"), "{}", err.message);
    }

    #[test]
    fn on_a_terminal_every_missing_field_is_asked_for() {
        let fields = [field("account", false), field("token", true)];
        let ask = |f: &remote::Field| Ok(format!("typed {}", f.key));
        let values = collect_values("x", &fields, given(&[]), &|_| None, Some(&ask)).unwrap();
        assert_eq!(
            values,
            given(&[("account", "typed account"), ("token", "typed token")])
        );
    }

    #[test]
    fn short_version_is_eight_characters_without_quotes() {
        assert_eq!(short_version("\"0123456789abcdef\""), "01234567");
        assert_eq!(short_version("abc"), "abc");
    }
}
