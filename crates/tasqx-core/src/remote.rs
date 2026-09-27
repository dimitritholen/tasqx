//! Remote connectors: the protocol tasqx speaks to a storage backend, and the
//! runner that speaks it (D198).
//!
//! A connector is an executable named `tasqx-remote-<name>` on `PATH`, in the
//! way git finds `git-remote-<scheme>`. tasqx runs it once per verb, writes one
//! JSON request to its stdin and reads one JSON reply from its stdout; stderr
//! is free-form, for a person, and is quoted (its end, truncated) when a call
//! fails. Every request and every reply carries `"protocol": 1`.
//!
//! | verb | request | reply |
//! |---|---|---|
//! | `describe` | — | `{name, version, protocol, fields: [{key, label, secret, help, help_url?}]}` |
//! | `configure` | `{values: {key: string}}` | `{ok: true}` or `{ok: false, error}` |
//! | `pull` | `{out_dir}` | `{snapshots: [{version, path}]}` |
//! | `push` | `{in_path, expected_version: string \| null}` | `{version}` or `{conflict: true}` |
//!
//! Exit 0 is an answer, exit 2 is a push conflict (and only that), anything
//! else is a failure. `configure` says no with `ok: false` and exit 0: a
//! rejected value is an answer, and its `error` is shown to the user as is.
//!
//! **Where secrets live.** `configure` is the only verb that sees field
//! values. The connector validates them (by connecting, typically) and keeps
//! what it needs itself; tasqx stores none of them. For every call the child's
//! environment carries [`STATE_DIR_ENV`], a directory of its own that the
//! runner creates first ([`state_dir`] says where), and `pull`/`push` read
//! their saved configuration from there.
//!
//! **The blob is opaque and travels by file.** `push` names a file to upload,
//! `pull` writes files into `out_dir` and names them; the runner refuses a
//! named path that resolves outside `out_dir`, and stdout is capped
//! ([`Limits::max_stdout`]) because it only ever carries the small reply.
//! `pull` answers with a LIST: none means the remote is empty, normally there
//! is one, and a folder connector may later hand back conflict copies beside
//! it.
//!
//! **`expected_version` is compare-and-swap.** `push` writes only if the
//! remote's current version equals it; `null` means "only if the remote is
//! empty". Otherwise it answers `{conflict: true}` with exit 2 and changes
//! nothing, and the caller pulls, merges and tries again.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::exec::Limits;
use crate::exec::{self, ExecError};

#[cfg(feature = "conformance")]
pub mod conformance;

/// The protocol version this tasqx speaks, carried by every request and reply.
pub const PROTOCOL: u32 = 1;

/// The exit code that means "push conflict" and nothing else.
pub const EXIT_CONFLICT: i32 = 2;

/// The kill deadline `tasqx sync` gives one `pull` or `push` (D201), instead of
/// [`Limits::default`]'s 120 s: a multi-megabyte upload on a slow link can
/// take longer than that. A connector's own per-request timeout must stay
/// below it — `tasqx-remote-r2`'s `CALL_TIMEOUT` (540 s, D199) does, and a
/// unit test there holds it to that.
pub const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The environment variable naming the connector's own state directory.
pub const STATE_DIR_ENV: &str = "TASQX_REMOTE_STATE_DIR";

/// The prefix of every connector executable's file name.
pub const BINARY_PREFIX: &str = "tasqx-remote-";

/// Configuration values, by field key.
pub type Values = BTreeMap<String, String>;

/// One request, minus the protocol number [`Envelope`] adds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    /// Say who you are and which settings you need.
    Describe,
    /// Validate these settings and keep them.
    Configure {
        /// The user's answers, by [`Field::key`].
        values: Values,
    },
    /// Write the remote's current snapshot(s) into `out_dir`.
    Pull {
        /// An existing, absolute directory for the snapshot files.
        out_dir: PathBuf,
    },
    /// Upload `in_path` if the remote is still at `expected_version`.
    Push {
        /// The absolute path of the blob to upload.
        in_path: PathBuf,
        /// The version the caller last pulled; `None` means "only if empty".
        expected_version: Option<String>,
    },
}

impl Request {
    /// The verb, as the wire spells it.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Describe => "describe",
            Self::Configure { .. } => "configure",
            Self::Pull { .. } => "pull",
            Self::Push { .. } => "push",
        }
    }
}

/// A request as written to the connector's stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Always [`PROTOCOL`].
    pub protocol: u32,
    /// The verb and its arguments.
    #[serde(flatten)]
    pub request: Request,
}

/// One setting a connector asks the user for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// The key its value is sent under in `configure`.
    pub key: String,
    /// What to call it when asking.
    pub label: String,
    /// Mask the input and never echo it: a password or a token.
    pub secret: bool,
    /// One line on what to enter.
    pub help: String,
    /// Where to read more, if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help_url: Option<String>,
}

/// The reply to `describe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    /// The connector's name.
    pub name: String,
    /// The connector's own version.
    pub version: String,
    /// The protocol it speaks; always [`PROTOCOL`] once the runner accepts it.
    pub protocol: u32,
    /// The settings `configure` expects.
    pub fields: Vec<Field>,
}

/// The reply to `configure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigureReply {
    /// Always [`PROTOCOL`].
    pub protocol: u32,
    /// Whether the values were accepted and saved.
    pub ok: bool,
    /// Why not, for the user, when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One snapshot a `pull` wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The remote's version of it; what a later push passes as expected.
    pub version: String,
    /// The file, inside `out_dir`. The runner hands it back canonicalized.
    pub path: PathBuf,
}

/// The reply to `pull`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullReply {
    /// Always [`PROTOCOL`].
    pub protocol: u32,
    /// None when the remote is empty.
    pub snapshots: Vec<Snapshot>,
}

/// The reply to `push`: exactly one of the two fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushReply {
    /// Always [`PROTOCOL`].
    pub protocol: u32,
    /// The remote's new version, on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// True when the remote was not at the expected version (exit 2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conflict: bool,
}

/// What `configure` concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Configured {
    /// Accepted and saved by the connector.
    Ok,
    /// Refused, with the connector's message for the user.
    Rejected(String),
}

/// What `push` concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// Written; the remote is now at `version`.
    Pushed {
        /// The remote's new version.
        version: String,
    },
    /// The remote was not at the expected version and is unchanged.
    Conflict,
}

/// Why a connector call gave no answer.
#[derive(Debug)]
pub enum Error {
    /// The name is not a connector name: lowercase ASCII letters, digits and
    /// inner dashes.
    BadName {
        /// What was asked for.
        name: String,
    },
    /// No `tasqx-remote-<name>` on `PATH`.
    NotFound {
        /// What was asked for.
        name: String,
    },
    /// Running it failed: could not start, timed out, flooded stdout.
    Exec(ExecError),
    /// It exited with a failure code.
    Failed {
        /// The executable.
        program: PathBuf,
        /// The verb it failed.
        verb: &'static str,
        /// The exit code, `None` when a signal ended it.
        code: Option<i32>,
        /// Its stderr.
        stderr: String,
    },
    /// Its reply was not JSON, not the verb's shape, or said something the
    /// protocol does not allow.
    Malformed {
        /// The executable.
        program: PathBuf,
        /// The verb it answered.
        verb: &'static str,
        /// What was wrong.
        detail: String,
    },
    /// Its reply is in another protocol version, or names none.
    Protocol {
        /// The executable.
        program: PathBuf,
        /// The `protocol` value it sent, if any.
        got: Option<Value>,
    },
    /// A `pull` named a file that resolves outside `out_dir`.
    PathOutside {
        /// The executable.
        program: PathBuf,
        /// The path it named.
        path: PathBuf,
        /// The directory it was confined to.
        out_dir: PathBuf,
    },
    /// Something on this side failed before or after the call.
    Local {
        /// What was being done.
        context: String,
        /// Why it failed.
        source: std::io::Error,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadName { name } => write!(
                f,
                "{name:?} is not a connector name: use lowercase letters, digits and dashes"
            ),
            Self::NotFound { name } => write!(
                f,
                "no connector {BINARY_PREFIX}{name} on PATH; install it or check its name"
            ),
            Self::Exec(e) => e.fmt(f),
            Self::Failed {
                program,
                verb,
                code,
                stderr,
            } => {
                write!(f, "{} failed on `{verb}`", program.display())?;
                match code {
                    Some(c) => write!(f, " (exit {c})")?,
                    None => write!(f, " (ended by a signal)")?,
                }
                exec::write_stderr(f, stderr)
            }
            Self::Malformed {
                program,
                verb,
                detail,
            } => write!(
                f,
                "{} gave a bad answer to `{verb}`: {detail}",
                program.display()
            ),
            Self::Protocol { program, got } => match got {
                Some(v) => write!(
                    f,
                    "{} speaks protocol {v}; this tasqx speaks protocol {PROTOCOL}",
                    program.display()
                ),
                None => write!(
                    f,
                    "{}'s reply has no \"protocol\" field; this tasqx speaks protocol {PROTOCOL}",
                    program.display()
                ),
            },
            Self::PathOutside {
                program,
                path,
                out_dir,
            } => write!(
                f,
                "{} named {} as a snapshot, which is outside {}; refused",
                program.display(),
                path.display(),
                out_dir.display()
            ),
            Self::Local { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Exec(e) => Some(e),
            Self::Local { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ExecError> for Error {
    fn from(e: ExecError) -> Self {
        Self::Exec(e)
    }
}

/// Whether `name` may name a connector (and a state directory): 1–64
/// characters of lowercase ASCII letters, digits and dashes, not starting or
/// ending with a dash. Narrow on purpose — it becomes part of a file name to
/// look up and a directory to create, so nothing in it may climb or separate.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn checked(name: &str) -> Result<(), Error> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(Error::BadName {
            name: name.to_string(),
        })
    }
}

/// The first `tasqx-remote-<name>` (plus the platform's executable suffix) in
/// the directories of `path_var`, a `PATH`-style list. `name` is taken as
/// already valid; [`Connector::find`] checks it.
pub fn find_in(name: &str, path_var: &OsStr) -> Option<PathBuf> {
    let file = format!("{BINARY_PREFIX}{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path_var)
        .map(|dir| dir.join(&file))
        .find(|p| is_runnable(p))
}

/// Every connector name found on `path_var`: the `tasqx-remote-*` executables
/// on `PATH`, named for the part after the prefix, deduped and sorted.
///
/// Written for `tasqx config edit`'s `c` (connect) key, which has to offer a
/// picker before it knows which name the user wants — [`find_in`] answers
/// "does this ONE name exist" and cannot be turned into a listing without
/// reading every directory on `PATH` twice. Same rule as `find_in`: an
/// execute bit on Unix, the platform's own suffix on Windows, and the first
/// match on `PATH` wins when a name appears in more than one directory.
pub fn list_on(path_var: &OsStr) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in std::env::split_paths(path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let stem = file
                .strip_suffix(std::env::consts::EXE_SUFFIX)
                .unwrap_or(file);
            let Some(name) = stem.strip_prefix(BINARY_PREFIX) else {
                continue;
            };
            if name.is_empty() || !is_runnable(&path) || names.iter().any(|n| n == name) {
                continue;
            }
            names.push(name.to_string());
        }
    }
    names.sort();
    names
}

/// A file this platform would run. On Unix that takes an execute bit: a stray
/// non-executable file of the right name earlier on `PATH` must not shadow the
/// real connector behind it, the way a shell skips it too. On Windows the
/// suffix [`find_in`] adds is what makes a file runnable.
fn is_runnable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Create `dir` (and any missing parent) readable by its owner alone, and
/// tighten it to that if it already exists open to group or world. A connector
/// keeps its credentials here. On Windows a directory under the user's profile
/// is already private to them, so this only creates it.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Where remote `remote`'s connector keeps its state: `remotes/<remote>/`
/// beside the store at `db_path`.
///
/// Derived from the store's path rather than resolved separately, so it
/// follows `$TASQX_DB` exactly as the store does: a scratch store gets scratch
/// connector state, and no dev build can reach the real one's credentials.
pub fn state_dir(db_path: &Path, remote: &str) -> Result<PathBuf, Error> {
    checked(remote)?;
    let base = db_path.parent().unwrap_or(Path::new(""));
    Ok(base.join("remotes").join(remote))
}

/// One connector executable and the state directory it runs with.
#[derive(Debug, Clone)]
pub struct Connector {
    program: PathBuf,
    state_dir: PathBuf,
    limits: Limits,
    env: Vec<(String, String)>,
}

impl Connector {
    /// The executable at `program`, keeping its state in `state_dir`.
    pub fn new(program: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            program,
            state_dir,
            limits: Limits::default(),
            env: Vec::new(),
        }
    }

    /// `tasqx-remote-<name>` from `PATH`.
    pub fn find(name: &str, state_dir: PathBuf) -> Result<Self, Error> {
        checked(name)?;
        let path = std::env::var_os("PATH").unwrap_or_default();
        find_in(name, &path)
            .map(|program| Self::new(program, state_dir))
            .ok_or_else(|| Error::NotFound {
                name: name.to_string(),
            })
    }

    /// Replace the default time and stdout bounds.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Set one more variable in the child's environment.
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// The executable this runs.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// `describe`: name, version and the settings to ask for.
    pub fn describe(&self) -> Result<Description, Error> {
        let (_, d): (_, Description) = self.call(Request::Describe)?;
        let bad = |detail: String| self.malformed("describe", detail);
        if d.name.trim().is_empty() {
            return Err(bad("its name is empty".into()));
        }
        let mut seen = BTreeSet::new();
        for field in &d.fields {
            if field.key.is_empty() {
                return Err(bad("a field has an empty key".into()));
            }
            if field.label.trim().is_empty() {
                return Err(bad(format!("field {:?} has no label", field.key)));
            }
            if !seen.insert(field.key.as_str()) {
                return Err(bad(format!("field key {:?} appears twice", field.key)));
            }
        }
        Ok(d)
    }

    /// `configure`: hand the user's values to the connector to check and keep.
    pub fn configure(&self, values: &Values) -> Result<Configured, Error> {
        let request = Request::Configure {
            values: values.clone(),
        };
        let (_, r): (_, ConfigureReply) = self.call(request)?;
        match (r.ok, r.error) {
            (true, _) => Ok(Configured::Ok),
            (false, Some(e)) if !e.trim().is_empty() => Ok(Configured::Rejected(e)),
            (false, _) => Err(self.malformed("configure", "it refused without an error".into())),
        }
    }

    /// `pull`: the remote's snapshot(s), written into `out_dir` (created if
    /// missing). Each returned path is canonical and inside `out_dir`.
    pub fn pull(&self, out_dir: &Path) -> Result<Vec<Snapshot>, Error> {
        std::fs::create_dir_all(out_dir).map_err(|source| Error::Local {
            context: format!("cannot create {}", out_dir.display()),
            source,
        })?;
        let root = std::fs::canonicalize(out_dir).map_err(|source| Error::Local {
            context: format!("cannot resolve {}", out_dir.display()),
            source,
        })?;
        let request = Request::Pull {
            out_dir: root.clone(),
        };
        let (_, r): (_, PullReply) = self.call(request)?;
        r.snapshots
            .into_iter()
            .map(|s| self.confine(s, &root))
            .collect()
    }

    /// Check one pulled snapshot names a real file inside `root`.
    fn confine(&self, s: Snapshot, root: &Path) -> Result<Snapshot, Error> {
        if s.version.is_empty() {
            return Err(self.malformed("pull", "a snapshot has an empty version".into()));
        }
        let outside = || Error::PathOutside {
            program: self.program.clone(),
            path: s.path.clone(),
            out_dir: root.to_path_buf(),
        };
        // Canonical, so neither `..` nor a symlink can walk out. A path that
        // does not resolve is refused as outside rather than trusted.
        let real = std::fs::canonicalize(root.join(&s.path)).map_err(|_| outside())?;
        if !real.starts_with(root) {
            return Err(outside());
        }
        if !real.is_file() {
            return Err(self.malformed(
                "pull",
                format!("snapshot {} is not a file", s.path.display()),
            ));
        }
        Ok(Snapshot {
            version: s.version,
            path: real,
        })
    }

    /// `push`: upload `in_path` if the remote is at `expected_version`
    /// (`None`: only if it is empty).
    pub fn push(
        &self,
        in_path: &Path,
        expected_version: Option<&str>,
    ) -> Result<PushOutcome, Error> {
        let in_path = std::path::absolute(in_path).map_err(|source| Error::Local {
            context: format!("cannot resolve {}", in_path.display()),
            source,
        })?;
        let request = Request::Push {
            in_path,
            expected_version: expected_version.map(str::to_string),
        };
        let (code, r): (_, PushReply) = self.call(request)?;
        let bad = |detail: &str| Err(self.malformed("push", detail.into()));
        match (code == EXIT_CONFLICT, r.conflict, r.version) {
            (true, true, None) => Ok(PushOutcome::Conflict),
            (true, _, _) => bad("exit 2 means conflict, but the reply is not {\"conflict\": true}"),
            (false, true, _) => bad("it reported a conflict but did not exit 2"),
            (false, false, Some(v)) if !v.is_empty() => Ok(PushOutcome::Pushed { version: v }),
            (false, false, _) => bad("the reply names no version"),
        }
    }

    fn malformed(&self, verb: &'static str, detail: String) -> Error {
        Error::Malformed {
            program: self.program.clone(),
            verb,
            detail,
        }
    }

    /// Run one verb: spawn, judge the exit code, check the protocol, decode.
    /// Returns the exit code (0, or 2 for `push`) beside the reply.
    fn call<T: DeserializeOwned>(&self, request: Request) -> Result<(i32, T), Error> {
        let verb = request.verb();
        private_dir(&self.state_dir).map_err(|source| Error::Local {
            context: format!("cannot create {}", self.state_dir.display()),
            source,
        })?;
        let envelope = Envelope {
            protocol: PROTOCOL,
            request,
        };
        let input = serde_json::to_vec(&envelope).map_err(|e| Error::Local {
            context: format!("cannot encode the `{verb}` request"),
            source: std::io::Error::other(e),
        })?;
        let mut cmd = Command::new(&self.program);
        for (name, _) in std::env::vars_os() {
            if is_sync_secret_var(&name) {
                cmd.env_remove(&name);
            }
        }
        cmd.env(STATE_DIR_ENV, &self.state_dir);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        let out = exec::run(&mut cmd, &input, &self.limits)?;
        let code = out.status.code();
        let answered = code == Some(0) || (verb == "push" && code == Some(EXIT_CONFLICT));
        if !answered {
            return Err(Error::Failed {
                program: self.program.clone(),
                verb,
                code,
                stderr: out.stderr,
            });
        }
        let value: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| self.malformed(verb, format!("not a JSON reply ({e})")))?;
        match value.get("protocol") {
            Some(p) if p.as_u64() == Some(u64::from(PROTOCOL)) => {}
            got => {
                return Err(Error::Protocol {
                    program: self.program.clone(),
                    got: got.cloned(),
                })
            }
        }
        let reply = serde_json::from_value(value)
            .map_err(|e| self.malformed(verb, format!("wrong shape ({e})")))?;
        Ok((code.unwrap_or_default(), reply))
    }
}

/// The prefix of the variables `tasqx sync` reads secrets from: the sync
/// passphrase (`TASQX_SYNC_PASSPHRASE`, D202) and a connector field's
/// `TASQX_SYNC_<KEY>` (D201).
const SYNC_SECRET_PREFIX: &str = "TASQX_SYNC_";

/// Whether a variable of this process's is one of `sync`'s secrets, which a
/// connector must not inherit (D198): it gets a secret only in the JSON
/// request `configure` sends, and never the passphrase that keeps its own
/// blobs unreadable to it. Everything else is inherited, since a connector
/// legitimately needs `HOME`, `PATH`, the keyring's session bus, proxies, CA
/// bundles and its own `TASQX_<NAME>_*` overrides. Windows compares variable
/// names without case, so there the prefix is matched the same way.
fn is_sync_secret_var(name: &std::ffi::OsStr) -> bool {
    // By bytes, so a name that is not UTF-8 after the prefix is still caught.
    let Some(head) = name.as_encoded_bytes().get(..SYNC_SECRET_PREFIX.len()) else {
        return false;
    };
    if cfg!(windows) {
        head.eq_ignore_ascii_case(SYNC_SECRET_PREFIX.as_bytes())
    } else {
        head == SYNC_SECRET_PREFIX.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_carry_the_protocol_and_the_verb_on_the_wire() {
        let push = Envelope {
            protocol: PROTOCOL,
            request: Request::Push {
                in_path: "/b".into(),
                expected_version: None,
            },
        };
        assert_eq!(
            serde_json::to_value(&push).unwrap(),
            json!({"protocol": 1, "verb": "push", "in_path": "/b", "expected_version": null})
        );
        let pull = Envelope {
            protocol: PROTOCOL,
            request: Request::Pull {
                out_dir: "/o".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&pull).unwrap(),
            json!({"protocol": 1, "verb": "pull", "out_dir": "/o"})
        );
        let configure = Envelope {
            protocol: PROTOCOL,
            request: Request::Configure {
                values: Values::from([("path".into(), "/r".into())]),
            },
        };
        assert_eq!(
            serde_json::to_value(&configure).unwrap(),
            json!({"protocol": 1, "verb": "configure", "values": {"path": "/r"}})
        );
    }

    #[test]
    fn a_push_reply_spells_only_the_field_it_means() {
        let ok = PushReply {
            protocol: 1,
            version: Some("v".into()),
            conflict: false,
        };
        assert_eq!(
            serde_json::to_value(ok).unwrap(),
            json!({"protocol": 1, "version": "v"})
        );
        let conflict = PushReply {
            protocol: 1,
            version: None,
            conflict: true,
        };
        assert_eq!(
            serde_json::to_value(conflict).unwrap(),
            json!({"protocol": 1, "conflict": true})
        );
    }

    #[test]
    fn names_are_slugs_and_nothing_else() {
        for good in ["dir", "s3", "web-dav", "a"] {
            assert!(is_valid_name(good), "{good}");
        }
        let long = "a".repeat(65);
        for bad in [
            "",
            "-a",
            "a-",
            "A",
            "a/b",
            "a\\b",
            "..",
            "a.b",
            "a b",
            long.as_str(),
        ] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }
}
