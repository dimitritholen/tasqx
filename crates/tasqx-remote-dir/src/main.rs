//! `tasqx-remote-dir`: a tasqx remote connector (DESIGN.md D198) that keeps
//! snapshots in a folder — a network share, or one that Dropbox, Syncthing or
//! iCloud keeps in step between machines.
//!
//! tasqx runs this once per verb with one JSON request on stdin; it answers
//! with one JSON reply on stdout and exits 0, or 2 for a push conflict, or 1
//! with the reason on stderr. Its one setting, `path`, is saved as
//! `config.json` in `$TASQX_REMOTE_STATE_DIR`.
//!
//! Layout of the remote folder:
//!
//! ```text
//! <path>/
//!   snapshots/<sha256>   each blob, named by the SHA-256 of its bytes
//!   HEAD                 the hash of the current blob: the remote's version
//!   history              the hashes HEAD has held, newest last; pruning keeps
//!                        the blobs of the last KEEP of them
//!   lock                 present only while a push is swapping HEAD
//! ```
//!
//! **Push** writes the blob first (temp file, then an atomic rename), then
//! takes the lock, compares HEAD with `expected_version`, and swaps HEAD the
//! same temp-and-rename way. The version IS the content hash, so a remote that
//! went A → B → A really is at A again and a push expecting A is right to win.
//!
//! **Pull** reads HEAD and hands back a copy of the blob it names. A blob that
//! is missing, or whose bytes do not hash to its name, is a folder the sync
//! client has not finished delivering; that is reported as retryable rather
//! than read as an empty remote or a corrupt one.
//!
//! **The lock** is a file created with `create_new`, so exactly one process on
//! a filesystem that honours exclusive creation gets it. A crashed push leaves
//! it behind; one older than [`LOCK_STALE`] is taken to belong to a dead
//! process and removed. That outlives tasqx's default 120-second connector
//! timeout, after which the push that took it has been killed anyway. Across
//! machines behind a sync client no lock is exclusive — the client, not this
//! program, decides what happens when two machines write HEAD at once — and
//! detecting the conflict copies that produces is a later addition; `pull`
//! answers with a LIST so it has room to hand them back.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const PROTOCOL: u64 = 1;
const EXIT_FAILED: i32 = 1;
const EXIT_CONFLICT: i32 = 2;
const STATE_DIR_ENV: &str = "TASQX_REMOTE_STATE_DIR";

/// How many snapshots survive a push: HEAD and the four before it.
const KEEP: usize = 5;

/// A lock older than this belongs to a push that died holding it.
const LOCK_STALE: Duration = Duration::from_secs(150);

/// How long a push waits for another one's lock before giving up.
const LOCK_WAIT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
enum Request {
    Describe,
    Configure {
        values: BTreeMap<String, String>,
    },
    Pull {
        out_dir: PathBuf,
    },
    Push {
        in_path: PathBuf,
        expected_version: Option<String>,
    },
}

/// Why a verb has no success reply.
enum Stop {
    /// Push only: the remote is not at the expected version.
    Conflict,
    /// Anything else, with the reason for stderr.
    Failed(String),
}

impl From<String> for Stop {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}

fn main() {
    let (reply, code) = match run() {
        Ok(reply) => (Some(reply), 0),
        Err(Stop::Conflict) => (Some(json!({ "conflict": true })), EXIT_CONFLICT),
        Err(Stop::Failed(msg)) => {
            eprintln!("tasqx-remote-dir: {msg}");
            (None, EXIT_FAILED)
        }
    };
    if let Some(mut reply) = reply {
        reply["protocol"] = json!(PROTOCOL);
        println!("{reply}");
    }
    std::process::exit(code);
}

fn run() -> Result<Value, Stop> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("cannot read the request: {e}"))?;
    let value: Value =
        serde_json::from_str(&input).map_err(|e| format!("the request is not JSON: {e}"))?;
    if value.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL) {
        return Err(Stop::Failed(format!(
            "this connector speaks protocol {PROTOCOL}; the request says {}",
            value.get("protocol").unwrap_or(&Value::Null)
        )));
    }
    let request: Request =
        serde_json::from_value(value).map_err(|e| format!("bad request: {e}"))?;
    match request {
        Request::Describe => Ok(describe()),
        Request::Configure { values } => configure(&values),
        Request::Pull { out_dir } => pull(&load_root()?, &out_dir),
        Request::Push {
            in_path,
            expected_version,
        } => push(&load_root()?, &in_path, expected_version.as_deref()),
    }
}

fn describe() -> Value {
    json!({
        "name": "dir",
        "version": env!("CARGO_PKG_VERSION"),
        "fields": [{
            "key": "path",
            "label": "Folder",
            "secret": false,
            "help": "An absolute path to a folder every machine can reach: a network share, \
                     or a folder Dropbox, Syncthing or iCloud keeps in sync.",
        }],
    })
}

fn state_dir() -> Result<PathBuf, String> {
    match std::env::var_os(STATE_DIR_ENV) {
        Some(d) if !d.is_empty() => Ok(PathBuf::from(d)),
        _ => Err(format!(
            "{STATE_DIR_ENV} is not set; tasqx runs this connector, not a person"
        )),
    }
}

/// `configure`: a rejection is an answer (`ok: false`, exit 0), not a failure.
fn configure(values: &BTreeMap<String, String>) -> Result<Value, Stop> {
    let state = state_dir()?;
    match check_folder(values) {
        Err(error) => Ok(json!({ "ok": false, "error": error })),
        Ok(root) => {
            private_dir(&state).map_err(|e| format!("cannot create {}: {e}", state.display()))?;
            let config = json!({ "path": root });
            write_atomic(
                &state.join("config.json"),
                config.to_string().as_bytes(),
                true,
            )?;
            Ok(json!({ "ok": true }))
        }
    }
}

/// Create the state dir readable by its owner alone (0700 on Unix), and
/// tighten one that is open to group or world. tasqx's runner does the same
/// before it runs us; doing it here too keeps the promise when we are run by
/// anything else. Other connectors keep credentials in theirs.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if std::fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// The folder `values` names, once it is known to exist and take writes.
fn check_folder(values: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    if let Some(key) = values.keys().find(|k| *k != "path") {
        return Err(format!(
            "unknown setting {key:?}; this connector takes only `path`"
        ));
    }
    let raw = values.get("path").map(|s| s.trim()).unwrap_or_default();
    if raw.is_empty() {
        return Err("a folder path is required".into());
    }
    let root = PathBuf::from(raw);
    if !root.is_absolute() {
        return Err(format!("{raw} is not an absolute path"));
    }
    if root.exists() && !root.is_dir() {
        return Err(format!("{raw} exists and is not a folder"));
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {raw}: {e}"))?;
    let probe = root.join(format!(".tasqx-write-probe-{}", std::process::id()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|e| format!("cannot write to {raw}: {e}"))?;
    let _ = std::fs::remove_file(&probe);
    Ok(root)
}

fn load_root() -> Result<PathBuf, String> {
    let file = state_dir()?.join("config.json");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("not configured: run `configure` first".into())
        }
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let config: Value =
        serde_json::from_str(&text).map_err(|e| format!("{} is damaged: {e}", file.display()))?;
    config["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{} names no path", file.display()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn blob_path(root: &Path, hash: &str) -> PathBuf {
    root.join("snapshots").join(hash)
}

/// The version HEAD names, `None` for a remote nobody has pushed to.
fn read_head(root: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(root.join("HEAD")) {
        Ok(text) => {
            let hash = text.trim();
            if is_hash(hash) {
                Ok(Some(hash.to_string()))
            } else {
                Err(format!(
                    "{} does not hold a snapshot hash; is this folder a tasqx remote?",
                    root.join("HEAD").display()
                ))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read HEAD in {}: {e}", root.display())),
    }
}

fn not_synced(hash: &str, what: &str) -> String {
    format!(
        "remote not fully synced yet: HEAD names snapshot {hash}, which {what}; \
         try again once the folder has finished syncing"
    )
}

fn pull(root: &Path, out_dir: &Path) -> Result<Value, Stop> {
    let Some(head) = read_head(root)? else {
        return Ok(json!({ "snapshots": [] }));
    };
    let bytes = match std::fs::read(blob_path(root, &head)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(not_synced(&head, "is not there").into())
        }
        Err(e) => return Err(format!("cannot read snapshot {head}: {e}").into()),
    };
    if sha256_hex(&bytes) != head {
        return Err(not_synced(&head, "does not match its hash").into());
    }
    let out = out_dir.join(&head);
    write_atomic(&out, &bytes, false)?;
    Ok(json!({ "snapshots": [{ "version": head, "path": out }] }))
}

fn push(root: &Path, in_path: &Path, expected: Option<&str>) -> Result<Value, Stop> {
    let bytes =
        std::fs::read(in_path).map_err(|e| format!("cannot read {}: {e}", in_path.display()))?;
    let hash = sha256_hex(&bytes);
    let blob = blob_path(root, &hash);
    std::fs::create_dir_all(blob.parent().expect("a blob has a parent"))
        .map_err(|e| format!("cannot create the snapshots folder: {e}"))?;
    // The blob goes up BEFORE the lock, so a slow upload holds nobody up.
    let wrote = store_blob(&blob, &bytes, &hash)?;

    let _lock = Lock::acquire(root)?;
    let head = read_head(root)?;
    if head.as_deref() != expected {
        // Our own upload is garbage now, unless it is the very blob HEAD
        // names; a same-content push racing us rewrites it under the lock.
        if wrote && head.as_deref() != Some(hash.as_str()) {
            let _ = std::fs::remove_file(&blob);
        }
        return Err(Stop::Conflict);
    }
    // Another push's prune may have removed a blob we found already present.
    store_blob(&blob, &bytes, &hash)?;
    write_atomic(&root.join("HEAD"), format!("{hash}\n").as_bytes(), false)?;
    if let Err(e) = prune(root, &hash) {
        // The push landed; a failed cleanup only costs disk.
        eprintln!("tasqx-remote-dir: pushed, but pruning old snapshots failed: {e}");
    }
    Ok(json!({ "version": hash }))
}

/// Make sure `blob` holds `bytes`. True when it had to be written.
fn store_blob(blob: &Path, bytes: &[u8], hash: &str) -> Result<bool, String> {
    if let Ok(existing) = std::fs::read(blob) {
        if sha256_hex(&existing) == hash {
            return Ok(false);
        }
    }
    write_atomic(blob, bytes, false)?;
    Ok(true)
}

/// Record `head` in `history` and delete every blob not among its last
/// [`KEEP`] entries. Runs under the lock.
fn prune(root: &Path, head: &str) -> Result<(), String> {
    let file = root.join("history");
    let mut history: Vec<String> = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .filter(|l| is_hash(l))
        .map(str::to_string)
        .collect();
    history.retain(|h| h != head);
    history.push(head.to_string());
    let keep = history.split_off(history.len().saturating_sub(KEEP));
    write_atomic(&file, format!("{}\n", keep.join("\n")).as_bytes(), false)?;

    let dir = root.join("snapshots");
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Temp files are another push's upload in flight; leave them be.
        if is_hash(name) && !keep.iter().any(|h| h == name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// Write `bytes` to `path` so a reader sees the old file or the whole new one.
/// `private` makes it readable by its owner alone (0600 on Unix) from the
/// moment it exists, not after a chmod that leaves a window.
fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(
        ".tmp-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let written = (|| {
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut f = open.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {}: {e}", path.display())
    })
}

/// The push lock; removed when dropped.
struct Lock(PathBuf);

impl Lock {
    fn acquire(root: &Path) -> Result<Self, String> {
        let path = root.join("lock");
        let started = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Self(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() >= LOCK_WAIT {
                        return Err(format!(
                            "another push holds {}; try again shortly",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(format!("cannot create {}: {e}", path.display())),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Whether the lock at `path` is older than [`LOCK_STALE`]. A lock that
/// vanished while we looked is not stale: the next attempt will just take it.
fn is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > LOCK_STALE)
}
