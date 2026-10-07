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
//! **The snapshot boundary.** A connector only ever holds ciphertext (D202).
//! [`pull_snapshots`] and [`push_snapshot`] are the only two places bytes
//! cross between this store and the remote: the push seals the export with
//! the store's sync passphrase ([`seal`]) before the connector is handed the
//! file, and the pull opens every snapshot before any of them is merged —
//! one that is plaintext, altered, truncated or sealed under another
//! passphrase stops the sync with nothing from that pull merged. The opened
//! document is written to a file of its own in the attempt's private working
//! directory, since that file is what `store.import` reads. The passphrase is
//! `<store>.sync.key` beside the store, 0600, never in `<store>.sync.json`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tasqx_core::remote::{self, Connector, Limits, PushOutcome, Snapshot};
use tasqx_core::ApiError;

use crate::backend::Backend;
use crate::theme::Ctx;

mod seal;

use seal::SyncKey;

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

/// Pull into `work/pull`, and open every snapshot into `work` as export
/// bytes. The inbound half of the snapshot boundary: all of them are opened
/// before the caller merges any, so one that will not open leaves the store
/// untouched by this pull.
fn pull_snapshots(
    remote: &dyn Remote,
    work: &Path,
    key: &SyncKey,
) -> Result<Vec<Pulled>, ApiError> {
    remote
        .pull(&work.join("pull"))
        .map_err(remote_error)?
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let blob = std::fs::read(&s.path)
                .map_err(io_error(format!("cannot read snapshot {}", s.version)))?;
            let doc = key.open(&blob).map_err(|e| {
                ApiError::bad_request(format!(
                    "sync: snapshot {} was refused and nothing it holds was merged: {e}",
                    s.version
                ))
            })?;
            let path = work.join(format!("opened-{i}.json"));
            std::fs::write(&path, &doc)
                .map_err(io_error(format!("cannot write {}", path.display())))?;
            Ok(Pulled {
                version: s.version,
                doc,
                path,
            })
        })
        .collect()
}

/// Seal the export into `dir` and push that. The outbound half of the
/// snapshot boundary: the connector is never handed the export itself.
fn push_snapshot(
    remote: &dyn Remote,
    dir: &Path,
    doc: &[u8],
    expected: Option<&str>,
    key: &SyncKey,
) -> Result<PushOutcome, ApiError> {
    let blob = seal::seal(key, doc).map_err(|e| ApiError::internal(format!("sync: {e}")))?;
    let path = dir.join("snapshot.tasqxenc");
    std::fs::write(&path, blob).map_err(io_error(format!("cannot write {}", path.display())))?;
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
/// the per-store fields and `dropped_events` aside. Without that exception two converged machines
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
        // Export bookkeeping, not data: the count of withheld `memory.add`
        // events of removed docs (D197) differs between a store that removed
        // the doc and one that only merged the removal (#1127).
        if let Some(o) = v.as_object_mut() {
            o.remove("dropped_events");
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
/// The comparison that skips the push is over opened documents: ciphertext
/// differs on every seal.
fn run_loop(
    be: &mut Backend,
    remote: &dyn Remote,
    work_root: &Path,
    key: &SyncKey,
) -> Result<Synced, ApiError> {
    let mut tally = Tally::default();
    for attempt in 1..=MAX_ATTEMPTS {
        let work = WorkDir::new(work_root, attempt)?;
        let pulled = pull_snapshots(remote, &work.0, key)?;
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
        match push_snapshot(remote, &work.0, &doc, head.as_deref(), key)? {
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

/// Where this store's sync passphrase lives: beside the store and its
/// `.sync.json`, never inside it, so the state file can be read, printed or
/// copied without carrying the key.
fn key_path(store: &Path) -> PathBuf {
    let mut name = store.file_name().unwrap_or_default().to_os_string();
    name.push(".sync.key");
    store.with_file_name(name)
}

/// The passphrase, or `None` when there is no key file or it is empty.
fn read_key(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    (!text.is_empty()).then_some(text)
}

/// Write the passphrase whole, readable by this user alone. On Unix the file
/// is CREATED 0600 — a fresh temp file made with that mode and renamed over —
/// so there is no moment at which it exists with a wider one.
fn write_key(path: &Path, passphrase: &str) -> Result<(), ApiError> {
    use std::io::Write;
    let tmp = path.with_extension("key.tmp");
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let mut f = open.open(&tmp)?;
        f.write_all(passphrase.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ApiError::internal(format!("sync: cannot write {}: {e}", path.display()))
    })
}

/// The store the backend writes to. Through a daemon that is the daemon's
/// file, which only the daemon can name (`core.capabilities.store`, D74) —
/// `$TASQX_DB`, when set, names that same file (D204), and the connector's state and this
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
    let Some(passphrase) = read_key(&key_path(&store)) else {
        return Err(ApiError::bad_request(format!(
            "sync: this store has no sync passphrase, so its snapshots cannot be encrypted \
             (it was set up before sync encrypted them): run `tasqx sync setup {name}` again \
             to set one"
        )));
    };
    let key = SyncKey::new(passphrase);
    let connector = connector_at(&store, &name)?.with_limits(Limits {
        timeout: remote::TRANSFER_TIMEOUT,
        ..Limits::default()
    });
    let synced = run_loop(be, &connector, &std::env::temp_dir(), &key)?;
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
    let store = store_path(be)?;
    let state = read_state(&state_path(&store));
    let result = json!({
        "set_up": state.is_some(),
        "encrypted": state.is_some() && read_key(&key_path(&store)).is_some(),
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
    refuse_passphrase_in_set(&given)?;
    let given = collect_values(
        name,
        &description.fields,
        given,
        &env,
        interactive.then_some(&ask as &Ask),
    )?;
    let ask_twice = prompt::passphrase;
    let passphrase = collect_passphrase(&env, interactive.then_some(&ask_twice as &AskTwice))?;
    if passphrase.chars().count() < ADVISED_PASSPHRASE_CHARS {
        eprintln!(
            "tasqx: note: the sync passphrase is under {ADVISED_PASSPHRASE_CHARS} characters; \
             whoever holds a snapshot can try passphrases against it offline, so a longer \
             one (several words) is much harder to guess"
        );
    }
    match connector.configure(&given).map_err(remote_error)? {
        remote::Configured::Rejected(error) => Err(ApiError::bad_request(format!(
            "{name} refused these settings: {error}"
        ))),
        remote::Configured::Ok => {
            // The same connector set up again keeps its last sync; another one
            // starts with none, since that version names a different remote.
            write_key(&key_path(&store), &passphrase)?;
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
                "encrypted": true,
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

/// Where `sync setup` reads the sync passphrase off a terminal.
const PASSPHRASE_ENV: &str = "TASQX_SYNC_PASSPHRASE";

/// Under this many characters `sync setup` warns, and still takes it.
const ADVISED_PASSPHRASE_CHARS: usize = 12;

/// How the passphrase is asked for on the terminal: twice, neither echoed.
type AskTwice = dyn Fn() -> Result<(String, String), ApiError>;

/// The passphrase never comes from `--set`, for the reason a secret field
/// does not: argv lands in shell history and in `ps`.
fn refuse_passphrase_in_set(given: &remote::Values) -> Result<(), ApiError> {
    if given.contains_key("passphrase") {
        return Err(ApiError::bad_request(format!(
            "the sync passphrase is never taken from --set, which would leave it in your shell \
             history and in `ps`: run `tasqx sync setup` on a terminal to be asked for it with \
             the prompt, which does not echo, or put it in the environment variable \
             {PASSPHRASE_ENV}"
        )));
    }
    Ok(())
}

/// The sync passphrase: [`PASSPHRASE_ENV`] when set, else asked twice on a
/// terminal. Refused when empty, or when the two answers differ.
fn collect_passphrase(
    env: &dyn Fn(&str) -> Option<String>,
    ask: Option<&AskTwice>,
) -> Result<String, ApiError> {
    let passphrase = match (env(PASSPHRASE_ENV), ask) {
        (Some(p), _) => p,
        (None, Some(ask)) => {
            let (first, again) = ask()?;
            if first != again {
                return Err(ApiError::bad_request(
                    "the two passphrases differ; nothing was set up",
                ));
            }
            first
        }
        (None, None) => {
            return Err(ApiError::bad_request(format!(
                "sync needs a passphrase to encrypt this store's snapshots, and stdin is not a \
                 terminal to ask on: put it in the environment variable {PASSPHRASE_ENV}"
            )))
        }
    };
    if passphrase.is_empty() {
        return Err(ApiError::bad_request(
            "the sync passphrase is empty; every snapshot is encrypted with it, so it must \
             have at least one character (and should have many)",
        ));
    }
    Ok(passphrase)
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

    /// The sync passphrase, asked twice without echo.
    pub(super) fn passphrase() -> Result<(String, String), ApiError> {
        let mut err = std::io::stderr();
        let _ = writeln!(
            err,
            "Every snapshot is encrypted with this passphrase before it leaves this machine. \
             Every machine syncing with this remote needs the same one; without it the \
             snapshots cannot be read."
        );
        let _ = write!(err, "Sync passphrase: ");
        let _ = err.flush();
        let first = secret()?;
        let _ = write!(err, "Again, to confirm: ");
        let _ = err.flush();
        let again = secret()?;
        Ok((first, again))
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

    const PASSPHRASE: &str = "correct horse battery staple";

    /// Every machine in these tests shares one passphrase, at the cheapest
    /// Argon2 cost so a debug build does not spend seconds per seal.
    fn key() -> SyncKey {
        SyncKey::cheap(PASSPHRASE)
    }

    /// What a connector would hold for this document.
    fn sealed(doc: &[u8]) -> Vec<u8> {
        seal::seal(&key(), doc).unwrap()
    }

    /// The export document another machine would have pushed, holding these
    /// tasks, before it is sealed.
    fn document_of(titles: &[&str]) -> Vec<u8> {
        let mut other = Backend::Local(Engine::open_in_memory().unwrap());
        for t in titles {
            add(&mut other, t);
        }
        serde_json::to_vec(&other.call("store.export", &json!({})).unwrap()).unwrap()
    }

    /// The same, as the connector holds it.
    fn snapshot_of(titles: &[&str]) -> Vec<u8> {
        sealed(&document_of(titles))
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
        let done = run_loop(&mut be, &fake, &dir, &key()).unwrap();
        assert!(done.pushed);
        assert_eq!(done.attempts, 1);
        assert_eq!(*fake.pushes.borrow(), vec![None]);
        assert_eq!(done.version, "v1");
    }

    /// #1127: a doc added and removed before the first sync leaves the export
    /// withholding its `memory.add` (D197), so `dropped_events` is nonzero on
    /// the store that did it and zero on one that only merged the removal.
    /// Two machines holding the same data must still agree they are in sync
    /// rather than push at each other forever.
    #[test]
    fn a_removed_doc_s_dropped_events_do_not_keep_two_stores_pushing() {
        let dir = scratch("dropped");
        let mut a = store(&dir, "a");
        let mut b = store(&dir, "b");
        add(&mut a, "From a");
        let doc = a
            .call(
                "memory.add",
                &json!({"title": "Scratch", "body": "gone soon"}),
            )
            .unwrap();
        a.call("memory.remove", &json!({"id": doc["id"]})).unwrap();
        let dropped = |be: &mut Backend| {
            be.call("store.export", &json!({})).unwrap()["dropped_events"].clone()
        };
        assert!(
            dropped(&mut a).as_i64().unwrap() > 0,
            "the setup must drop an event"
        );

        let fake = Fake::new(Vec::new());
        run_loop(&mut a, &fake, &dir, &key()).unwrap();
        // B merges A's snapshot first, so both share one project row.
        run_loop(&mut b, &fake, &dir, &key()).unwrap();
        assert_eq!(dropped(&mut b), json!(0), "B never saw the doc's add");
        add(&mut b, "From b");
        let mut pushes = Vec::new();
        for _ in 0..3 {
            for be in [&mut a, &mut b] {
                pushes.push(run_loop(be, &fake, &dir, &key()).unwrap().pushed);
            }
        }
        // One round to converge; the last round must push nothing.
        assert_eq!(pushes[4..], [false, false], "never settled: {pushes:?}");
        assert_eq!(titles(&mut a), titles(&mut b));
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
        let done = run_loop(&mut be, &fake, &dir, &key()).unwrap();
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
        // Sealed afresh: another salt and nonce than any seal of this store
        // would draw, so only the OPENED documents can compare equal.
        let fake = Fake::new(vec![("head".into(), sealed(&mine))]);
        let done = run_loop(&mut be, &fake, &dir, &key()).unwrap();
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
        let done = run_loop(&mut be, &fake, &dir, &key()).unwrap();
        assert!(done.pushed);
        assert_eq!(done.attempts, 2);
        assert_eq!(
            *fake.pushes.borrow(),
            vec![Some("v0".to_string()), Some("raced".to_string())]
        );
        assert_eq!(titles(&mut be), ["Local", "Theirs", "Theirs, later"]);
        // What landed on the remote is this store, all three tasks of it.
        let pushed: Value =
            serde_json::from_slice(&key().open(&fake.snapshots.borrow()[0].1).unwrap()).unwrap();
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
        let err = run_loop(&mut be, &fake, &dir, &key()).unwrap_err();
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
        let err = run_loop(&mut be, &fake, &dir, &key()).unwrap_err();
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
        run_loop(&mut be, &fake, &work, &key()).unwrap();
        assert_eq!(std::fs::read_dir(&work).unwrap().count(), 0);
    }

    #[test]
    fn a_snapshot_that_is_not_an_export_is_refused_by_version() {
        let dir = scratch("junk");
        let mut be = store(&dir, "a");
        let fake = Fake::new(vec![("v9".into(), sealed(b"{\"nope\": 1}"))]);
        let err = run_loop(&mut be, &fake, &dir, &key()).unwrap_err();
        assert!(err.message.contains("v9"), "{err}");
        assert!(fake.pushes.borrow().is_empty());
    }

    #[test]
    fn what_is_pushed_is_sealed_and_holds_no_plaintext() {
        let dir = scratch("sealed");
        let mut be = store(&dir, "a");
        add(&mut be, "Renew the passport");
        let fake = Fake::new(Vec::new());
        run_loop(&mut be, &fake, &dir, &key()).unwrap();
        let blob = fake.snapshots.borrow()[0].1.clone();
        assert!(
            !blob.windows(8).any(|w| w == b"passport"),
            "no title in clear"
        );
        assert!(serde_json::from_slice::<Value>(&blob).is_err(), "not JSON");
        let opened: Value = serde_json::from_slice(&key().open(&blob).unwrap()).unwrap();
        assert_eq!(opened["tasks"][0]["title"], "Renew the passport");
    }

    /// A snapshot that will not open stops the sync before ANY snapshot of
    /// the pull is merged — even one that would have opened — and nothing is
    /// pushed.
    fn refused_before_any_merge(tag: &str, bad: Vec<u8>) -> ApiError {
        let dir = scratch(tag);
        let mut be = store(&dir, "a");
        add(&mut be, "Local");
        let fake = Fake::new(vec![
            ("good".into(), snapshot_of(&["Theirs"])),
            ("bad".into(), bad),
        ]);
        let err = run_loop(&mut be, &fake, &dir, &key()).unwrap_err();
        assert_eq!(err.code, tasqx_core::ErrorCode::BadRequest, "{err}");
        assert!(err.message.contains("bad"), "names the version: {err}");
        assert!(err.message.contains("nothing"), "{err}");
        assert_eq!(titles(&mut be), ["Local"], "nothing merged");
        assert!(fake.pushes.borrow().is_empty(), "nothing pushed");
        err
    }

    #[test]
    fn a_snapshot_under_another_passphrase_is_refused_before_any_merge() {
        let other = seal::seal(&SyncKey::cheap("another passphrase"), &document_of(&["X"]));
        let err = refused_before_any_merge("wrongpass", other.unwrap());
        assert!(
            err.message.contains("passphrase"),
            "the likely cause: {err}"
        );
    }

    #[test]
    fn a_snapshot_with_a_flipped_byte_is_refused_before_any_merge() {
        let mut blob = snapshot_of(&["X"]);
        let mid = blob.len() / 2;
        blob[mid] ^= 0x80;
        let err = refused_before_any_merge("flipped", blob);
        assert!(err.message.contains("altered"), "{err}");
    }

    #[test]
    fn a_truncated_snapshot_is_refused_before_any_merge() {
        let mut blob = snapshot_of(&["X"]);
        blob.truncate(blob.len() - 20);
        let err = refused_before_any_merge("truncated", blob);
        assert!(err.message.contains("truncated"), "{err}");
    }

    #[test]
    fn a_plaintext_snapshot_is_refused_before_any_merge() {
        let err = refused_before_any_merge("plain", document_of(&["X"]));
        assert!(err.message.contains("not encrypted"), "{err}");
    }

    #[test]
    fn the_key_sits_beside_the_store_owner_only_and_round_trips() {
        let dir = scratch("key");
        let path = key_path(&dir.join("tasks.db"));
        assert_eq!(path, dir.join("tasks.db.sync.key"));
        assert_eq!(read_key(&path), None);
        write_key(&path, "first").unwrap();
        write_key(&path, "second").unwrap();
        assert_eq!(read_key(&path).as_deref(), Some("second"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        }
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_key(&path), None, "an empty key file is no key");
    }

    #[test]
    fn the_passphrase_comes_from_its_env_var_or_the_prompt_never_set() {
        let env = |k: &str| (k == "TASQX_SYNC_PASSPHRASE").then(|| "from env".to_string());
        assert_eq!(collect_passphrase(&env, None).unwrap(), "from env");

        let err = refuse_passphrase_in_set(&given(&[("passphrase", "hunter2")])).unwrap_err();
        assert_eq!(err.code, tasqx_core::ErrorCode::BadRequest);
        assert!(err.message.contains("TASQX_SYNC_PASSPHRASE"), "{err}");
        assert!(!err.message.contains("hunter2"), "{err}");
        refuse_passphrase_in_set(&given(&[("path", "/x")])).unwrap();

        let err = collect_passphrase(&|_| None, None).unwrap_err();
        assert!(err.message.contains("TASQX_SYNC_PASSPHRASE"), "{err}");

        let twice = || Ok(("typed".to_string(), "typed".to_string()));
        assert_eq!(
            collect_passphrase(&|_| None, Some(&twice)).unwrap(),
            "typed"
        );
        let differ = || Ok(("typed".to_string(), "typo".to_string()));
        let err = collect_passphrase(&|_| None, Some(&differ)).unwrap_err();
        assert!(err.message.contains("differ"), "{err}");
    }

    #[test]
    fn an_empty_passphrase_is_refused() {
        let err = collect_passphrase(&|_| Some(String::new()), None).unwrap_err();
        assert!(err.message.contains("empty"), "{err}");
        let blank = || Ok((String::new(), String::new()));
        assert!(collect_passphrase(&|_| None, Some(&blank)).is_err());
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
        let mut doc: Value = serde_json::from_slice(&document_of(&["Theirs"])).unwrap();
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
