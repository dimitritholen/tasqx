//! `tasqx-remote-r2`: a tasqx remote connector (DESIGN.md D198, D199) that
//! keeps the snapshot as one object in a Cloudflare R2 bucket, through R2's
//! S3-compatible API.
//!
//! tasqx runs this once per verb with one JSON request on stdin; it answers
//! with one JSON reply on stdout and exits 0, or 2 for a push conflict, or 1
//! with the reason on stderr.
//!
//! **Settings.** `configure` takes the account ID, the bucket, an R2 API
//! token's Access Key ID and Secret Access Key, and optionally the object key
//! (default [`DEFAULT_KEY`]). It checks them with a signed ranged GET of the
//! object — a missing object is fine, a missing bucket or a refused token is
//! not — then saves everything but the secret as `config.json` (0600) in
//! `$TASQX_REMOTE_STATE_DIR`, and the secret in the OS keyring under the
//! service `tasqx-remote-r2` and the user `<account_id>/<bucket>/<access_key_id>`.
//! When the keyring cannot be used, `configure` says so and saves nothing; it
//! never falls back to writing the secret to a file.
//!
//! **Environment.**
//!
//! - `TASQX_R2_SECRET_ACCESS_KEY`: the secret, for CI and headless machines
//!   with no Secret Service. When set, every verb reads the secret from it
//!   and the keyring is not touched; it is never persisted. `configure` then
//!   needs no `secret_access_key` value, and refuses one that differs.
//! - `TASQX_R2_ENDPOINT`: `https://host[:port]` in place of
//!   `https://<account_id>.r2.cloudflarestorage.com` — a jurisdiction
//!   endpoint such as `https://<account_id>.eu.r2.cloudflarestorage.com`, or
//!   a test server. Plain `http://` is refused unless the host is loopback
//!   (`localhost`, 127.0.0.0/8, `[::1]`). Read on every call, never saved.
//!
//! **Versions are ETags.** `pull` is one GET: 404 is an empty remote, 200 is
//! the snapshot, streamed to a file in `out_dir`, and its ETag (quotes
//! stripped) is the version. `push` is one PUT, conditional on the server
//! side: `If-Match: "<version>"`, or `If-None-Match: *` for a push onto an
//! empty remote. R2 evaluates that atomically, so 412 Precondition Failed —
//! or 409 ConditionalRequestConflict, when another conditional write on the
//! same key is in flight — is a conflict that changed nothing.

mod s3;
mod secret;
mod sigv4;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use s3::{Client, Endpoint, Failure, Upload};
use secret::{Store, SECRET_ENV};

const PROTOCOL: u64 = 1;
const EXIT_FAILED: i32 = 1;
const EXIT_CONFLICT: i32 = 2;
const STATE_DIR_ENV: &str = "TASQX_REMOTE_STATE_DIR";

/// Where the snapshot lives in the bucket unless `object_key` says otherwise.
/// `.tqx`: the object is tasqx's own sealed snapshot (D202), not an `age` file.
const DEFAULT_KEY: &str = "tasqx/snapshot.tqx";

/// Cloudflare's page on creating R2 API tokens.
const TOKEN_HELP_URL: &str = "https://developers.cloudflare.com/r2/api/tokens/";

/// Every key `configure` accepts.
const KEYS: [&str; 5] = [
    "account_id",
    "bucket",
    "access_key_id",
    "secret_access_key",
    "object_key",
];

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

/// What `config.json` holds: every setting but the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Config {
    account_id: String,
    bucket: String,
    access_key_id: String,
    object_key: String,
}

impl Config {
    /// The keyring user the secret is filed under.
    fn keyring_user(&self) -> String {
        format!("{}/{}/{}", self.account_id, self.bucket, self.access_key_id)
    }

    /// The object's path, path-style: `/<bucket>/<key>`, not yet encoded.
    fn path(&self) -> String {
        format!("/{}/{}", self.bucket, self.object_key)
    }
}

fn main() {
    let (reply, code) = match run(&secret::Keyring) {
        Ok(reply) => (Some(reply), 0),
        Err(Stop::Conflict) => (Some(json!({ "conflict": true })), EXIT_CONFLICT),
        Err(Stop::Failed(msg)) => {
            eprintln!("tasqx-remote-r2: {msg}");
            (None, EXIT_FAILED)
        }
    };
    if let Some(mut reply) = reply {
        reply["protocol"] = json!(PROTOCOL);
        println!("{reply}");
    }
    std::process::exit(code);
}

fn run(store: &dyn Store) -> Result<Value, Stop> {
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
        Request::Configure { values } => configure(&values, store),
        Request::Pull { out_dir } => pull(store, &out_dir),
        Request::Push {
            in_path,
            expected_version,
        } => push(store, &in_path, expected_version.as_deref()),
    }
}

fn describe() -> Value {
    let field = |key: &str, label: &str, secret: bool, help: &str| json!({ "key": key, "label": label, "secret": secret, "help": help });
    let mut access_key_id = field(
        "access_key_id",
        "Access Key ID",
        false,
        "In the Cloudflare dashboard, under R2 > Manage API tokens, create an R2 API \
         token with Object Read & Write permission scoped to this one bucket, and copy \
         its Access Key ID and Secret Access Key.",
    );
    access_key_id["help_url"] = json!(TOKEN_HELP_URL);
    let mut secret_access_key = field(
        "secret_access_key",
        "Secret Access Key",
        true,
        "The token's Secret Access Key, shown once when the token is created. Kept in \
         the OS keyring, never in a file.",
    );
    secret_access_key["help_url"] = json!(TOKEN_HELP_URL);
    json!({
        "name": "r2",
        "version": env!("CARGO_PKG_VERSION"),
        "fields": [
            field(
                "account_id",
                "Account ID",
                false,
                "Your Cloudflare account ID: 32 hexadecimal characters, shown on the R2 \
                 overview page.",
            ),
            field(
                "bucket",
                "Bucket",
                false,
                "The R2 bucket to keep the snapshot in. Create it first; this connector \
                 does not.",
            ),
            access_key_id,
            secret_access_key,
            field(
                "object_key",
                "Object key",
                false,
                &format!(
                    "Where in the bucket the snapshot is kept. Leave empty for {DEFAULT_KEY}."
                ),
            ),
        ],
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

/// The secret from the environment, when the override is in use.
fn secret_override() -> Option<String> {
    std::env::var(SECRET_ENV).ok().filter(|s| !s.is_empty())
}

/// `configure`: a rejection is an answer (`ok: false`, exit 0), not a failure.
fn configure(values: &BTreeMap<String, String>, store: &dyn Store) -> Result<Value, Stop> {
    let state = state_dir()?;
    let overridden = secret_override();
    let accepted = check_values(values, overridden.as_deref()).and_then(|(config, secret)| {
        let client = Client::new(
            Endpoint::resolve(&config.account_id)?,
            &config.access_key_id,
            &secret,
        );
        probe(&client, &config)?;
        secret::save(store, overridden.is_some(), &config.keyring_user(), &secret)?;
        Ok(config)
    });
    match accepted {
        Err(error) => Ok(json!({ "ok": false, "error": error })),
        Ok(config) => {
            private_dir(&state).map_err(|e| format!("cannot create {}: {e}", state.display()))?;
            let text = serde_json::to_string(&config).expect("a config serialises");
            write_atomic(&state.join("config.json"), text.as_bytes(), true)?;
            Ok(json!({ "ok": true }))
        }
    }
}

/// The settings `values` names, and the secret to use, once each is well
/// formed. Nothing here touches the network.
fn check_values(
    values: &BTreeMap<String, String>,
    overridden: Option<&str>,
) -> Result<(Config, String), String> {
    if let Some(key) = values.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!(
            "unknown setting {key:?}; this connector takes {}",
            KEYS.join(", ")
        ));
    }
    let get = |key: &str| values.get(key).map(|s| s.trim()).unwrap_or_default();
    let required = |key: &str| match get(key) {
        "" => Err(format!("{key} is required")),
        v => Ok(v),
    };

    let account_id = required("account_id")?.to_ascii_lowercase();
    if account_id.len() != 32 || !account_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "{:?} is not a Cloudflare account ID: that is 32 hexadecimal characters, \
             shown on the R2 overview page",
            get("account_id")
        ));
    }
    let bucket = required("bucket")?;
    let bucket_ok = (3..=63).contains(&bucket.len())
        && bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !bucket.starts_with('-')
        && !bucket.ends_with('-');
    if !bucket_ok {
        return Err(format!(
            "{bucket:?} is not an R2 bucket name: 3 to 63 lowercase letters, digits and \
             dashes, not starting or ending with a dash"
        ));
    }
    let access_key_id = required("access_key_id")?;
    if !access_key_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!(
            "{access_key_id:?} is not an R2 Access Key ID: letters and digits only"
        ));
    }
    let object_key = match get("object_key") {
        "" => DEFAULT_KEY,
        k => k,
    };
    if object_key.starts_with('/')
        || object_key.len() > 1024
        || object_key.chars().any(char::is_control)
    {
        return Err(format!(
            "{object_key:?} is not a usable object key: no leading slash, no control \
             characters, at most 1024 bytes"
        ));
    }
    let given = get("secret_access_key");
    let secret = match overridden {
        Some(env) if given.is_empty() || given == env => env.to_string(),
        Some(_) => {
            return Err(format!(
                "the secret_access_key given differs from {SECRET_ENV}, which is set and \
                 would be used instead; unset it, or leave the field empty"
            ))
        }
        None if given.is_empty() => return Err("secret_access_key is required".into()),
        None => given.to_string(),
    };
    let config = Config {
        account_id,
        bucket: bucket.to_string(),
        access_key_id: access_key_id.to_string(),
        object_key: object_key.to_string(),
    };
    Ok((config, secret))
}

/// Check the credentials against the bucket with a one-byte GET of the object.
/// A missing object is fine: the first push creates it.
fn probe(client: &Client, config: &Config) -> Result<(), String> {
    let resp = client.send("GET", &config.path(), &[("range", "bytes=0-0")], None)?;
    match resp.status().as_u16() {
        // 416: an object with no bytes to range over.
        200 | 206 | 416 => Ok(()),
        _ => {
            let failure = Failure::read(resp);
            match failure.status {
                404 if failure.is("NoSuchBucket") => Err(no_bucket(config)),
                404 => Ok(()),
                401 | 403 => Err(format!(
                    "R2 refused these credentials ({failure}). Check the Access Key ID \
                     and Secret Access Key, and that the API token has Object Read & \
                     Write permission on bucket {}",
                    config.bucket
                )),
                _ => Err(format!("R2 at {} answered {failure}", client.base())),
            }
        }
    }
}

fn no_bucket(config: &Config) -> String {
    format!(
        "there is no bucket {:?} in account {}; create it in the Cloudflare dashboard first",
        config.bucket, config.account_id
    )
}

/// The saved settings and a client over them, for `pull` and `push`.
fn connect(store: &dyn Store) -> Result<(Config, Client), String> {
    let file = state_dir()?.join("config.json");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("not configured: run `configure` first".into())
        }
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let config: Config =
        serde_json::from_str(&text).map_err(|e| format!("{} is damaged: {e}", file.display()))?;
    let secret = secret::load(store, secret_override().as_deref(), &config.keyring_user())?;
    let client = Client::new(
        Endpoint::resolve(&config.account_id)?,
        &config.access_key_id,
        &secret,
    );
    Ok((config, client))
}

fn pull(store: &dyn Store, out_dir: &Path) -> Result<Value, Stop> {
    let (config, client) = connect(store)?;
    let resp = client.send("GET", &config.path(), &[], None)?;
    if resp.status().as_u16() != 200 {
        let failure = Failure::read(resp);
        return match failure.status {
            404 if failure.is("NoSuchBucket") => Err(no_bucket(&config).into()),
            404 => Ok(json!({ "snapshots": [] })),
            _ => Err(format!("pull failed: R2 answered {failure}").into()),
        };
    }
    let version = s3::etag(&resp)
        .ok_or_else(|| "pull failed: R2 sent the object without an ETag".to_string())?;
    let out = out_dir.join("snapshot");
    let mut body = resp.into_body().into_reader();
    // Streamed to disk, never held whole in memory.
    write_atomic_from(&out, &mut body, false)?;
    Ok(json!({ "snapshots": [{ "version": version, "path": out }] }))
}

fn push(store: &dyn Store, in_path: &Path, expected: Option<&str>) -> Result<Value, Stop> {
    if let Some(v) = expected {
        if v.is_empty() || v.contains('"') || !v.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(format!("{v:?} is not a version this connector hands out").into());
        }
    }
    let (config, client) = connect(store)?;
    let mut file =
        File::open(in_path).map_err(|e| format!("cannot open {}: {e}", in_path.display()))?;
    let sha256 =
        sha256_file(&mut file).map_err(|e| format!("cannot read {}: {e}", in_path.display()))?;
    let quoted;
    let condition = match expected {
        Some(v) => {
            quoted = format!("\"{v}\"");
            ("if-match", quoted.as_str())
        }
        None => ("if-none-match", "*"),
    };
    let resp = client.send(
        "PUT",
        &config.path(),
        &[condition],
        Some(Upload { file, sha256 }),
    )?;
    match resp.status().as_u16() {
        200 => {
            let version = s3::etag(&resp)
                .ok_or_else(|| "push failed: R2 accepted it but sent no ETag".to_string())?;
            Ok(json!({ "version": version }))
        }
        412 => Err(Stop::Conflict),
        _ => {
            let failure = Failure::read(resp);
            match failure.status {
                409 if failure.is("ConditionalRequestConflict") => Err(Stop::Conflict),
                404 if failure.is("NoSuchBucket") => Err(no_bucket(&config).into()),
                _ => Err(format!("push failed: R2 answered {failure}").into()),
            }
        }
    }
}

/// The SHA-256 of the file's bytes, read in chunks; leaves it rewound.
fn sha256_file(file: &mut File) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(sigv4::hex(&hasher.finalize()))
}

/// Create the state dir readable by its owner alone (0700 on Unix), and
/// tighten one that is open to group or world, as `tasqx-remote-dir` does.
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

fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    write_atomic_from(path, &mut &bytes[..], private)
}

/// Write what `source` yields to `path` so a reader sees the old file or the
/// whole new one. `private` makes it readable by its owner alone (0600 on
/// Unix) from the moment it exists.
fn write_atomic_from(path: &Path, source: &mut dyn Read, private: bool) -> Result<(), String> {
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
        std::io::copy(source, &mut f)?;
        f.flush()?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {}: {e}", path.display())
    })
}
