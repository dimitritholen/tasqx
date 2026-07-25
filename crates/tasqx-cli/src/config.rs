//! The settings registry and the one D9 precedence resolver.
//!
//! Before this module, `config.toml` was read by four hand-written functions
//! that each hard-coded one key, and D9's precedence chain (defaults →
//! config.toml → `TASQX_*` → CLI flags) existed in exactly one place —
//! `theme::resolve_name`. Every other setting re-invented a shorter chain at
//! its own call site, so `--socket` and `--theme` obeyed different rules and
//! nothing said so. One registry plus one resolver means a new setting is a
//! table row, not four edits in three files.

use std::path::PathBuf;

use tasqx_core::ApiError;

/// Which config store owns a key.
///
/// tasqx deliberately has two. D21 put `default_project` in the store's own
/// `config` table because it names a row in *this store's* `projects` table
/// and is meaningless against a different `TASQX_DB`; putting it in
/// `config.toml` too would buy a precedence rule and a class of bug where
/// config names a project the store has never heard of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Home {
    /// `config.toml` — per machine, best-effort, unvalidated.
    Toml,
    /// The store's `config` table — per store, validated, transactional, and
    /// recorded in the event log. Read-only through `tasqx config`.
    Store,
}

/// The value type a setting holds, and how a typed string is validated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Str,
    Bool,
}

/// Which layer supplied the value a user is looking at. Reported by
/// `config list` because the question behind a surprising setting is always
/// "which layer won", and a bare value cannot answer it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Default,
    File,
    Env,
    Flag,
}

impl Source {
    pub fn label(self, s: &Setting) -> String {
        match self {
            Source::Default => "default".to_string(),
            Source::File => "config.toml".to_string(),
            Source::Env => s
                .env
                .map(|e| format!("${e}"))
                .unwrap_or_else(|| "env".into()),
            Source::Flag => s
                .flag
                .map(|f| f.to_string())
                .unwrap_or_else(|| "flag".into()),
        }
    }
}

/// Whether a setting has a closed set of acceptable values that an interactive
/// picker can offer, and where that set comes from.
///
/// The registry names the *source* rather than the values because the theme
/// list is a filesystem question (built-ins plus `themes/*.toml`) and this
/// module must stay free of that lookup. Without this field the settings TUI
/// would have had to test `key == "theme.name"` to decide whether Enter opens a
/// picker — a hardcoded key in a second place, which is exactly the
/// parallel-list problem this registry exists to remove.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choices {
    /// Any string the validator accepts. An editor offers no list.
    Free,
    /// The installed themes: the built-ins plus the user's theme files.
    Themes,
}

/// One setting. The `[section] key` form in `config.toml` is derived by
/// splitting `key` on its single dot.
pub struct Setting {
    pub key: &'static str,
    pub home: Home,
    pub kind: Kind,
    pub default: &'static str,
    /// The `TASQX_*` variable that overrides the file, if any.
    pub env: Option<&'static str>,
    /// The CLI flag that overrides everything, if any.
    pub flag: Option<&'static str>,
    /// Where an interactive editor gets the list of acceptable values, if any.
    pub choices: Choices,
    pub summary: &'static str,
}

impl Setting {
    /// The `[section]` and key halves of a dotted name.
    pub fn parts(&self) -> (&'static str, &'static str) {
        self.key
            .split_once('.')
            .expect("every SETTINGS key is section.name")
    }
}

pub const SETTINGS: &[Setting] = &[
    Setting {
        key: "theme.name",
        home: Home::Toml,
        kind: Kind::Str,
        default: crate::theme::DEFAULT_THEME,
        env: Some("TASQX_THEME"),
        flag: Some("--theme"),
        choices: Choices::Themes,
        summary:
            "Terminal theme: a built-in (nord, gruvbox, dracula, solarized, mono) or a user file.",
    },
    Setting {
        key: "notify.enabled",
        home: Home::Toml,
        kind: Kind::Bool,
        default: "false",
        env: None,
        flag: None,
        choices: Choices::Free,
        summary: "Allow the daemon to raise native OS notifications for reminders.",
    },
    Setting {
        key: "default_project",
        home: Home::Store,
        kind: Kind::Str,
        default: "",
        env: None,
        flag: None,
        choices: Choices::Free,
        summary:
            "Project a bare `tasqx add` files into. Lives in the store; set it with `tasqx use`.",
    },
];

pub fn find(key: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.key == key)
}

/// A value is present only if it is non-empty after trimming. Lifted verbatim
/// from `theme::resolve_name`: without it `TASQX_THEME=" "` selects a theme
/// named " " and falls through to the "unknown theme" path instead of the
/// default.
fn pick(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// Resolve one setting across the D9 chain, reporting which layer won.
///
/// `flag` is the CLI value; `file` is the value read from `config.toml`. The
/// env layer is read here rather than passed in, so a caller cannot forget it.
pub fn resolve(s: &Setting, flag: Option<&str>, file: Option<&str>) -> (String, Source) {
    if let Some(v) = pick(flag) {
        return (v.to_string(), Source::Flag);
    }
    if let Some(name) = s.env {
        if let Ok(raw) = std::env::var(name) {
            if let Some(v) = pick(Some(&raw)) {
                return (v.to_string(), Source::Env);
            }
        }
    }
    if let Some(v) = pick(file) {
        return (v.to_string(), Source::File);
    }
    (s.default.to_string(), Source::Default)
}

/// The directory holding `config.toml`: `$TASQX_CONFIG_DIR` when set and
/// non-empty, else the platform config dir. An empty variable means "no config
/// dir at all", which is how tests isolate themselves.
pub fn config_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("TASQX_CONFIG_DIR") {
        if d.is_empty() {
            return None;
        }
        return Some(PathBuf::from(d));
    }
    Some(
        directories::ProjectDirs::from("dev", "tasqx", "tasqx")?
            .config_dir()
            .to_path_buf(),
    )
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Map a TOML value onto a setting's declared `Kind`, or `None` if the types
/// disagree.
///
/// One function because there are two readers — the silent one on every
/// command's path and the strict one behind `tasqx config` — and they must
/// agree on what counts as a value. They started as two copies of this match,
/// which is the parallel-list problem one layer down: a mutation test aimed at
/// the coercion rule hit one copy and passed, because the other still enforced
/// it.
fn coerce(kind: Kind, v: toml::Value) -> Option<String> {
    match (kind, v) {
        (Kind::Str, toml::Value::String(x)) => Some(x),
        (Kind::Bool, toml::Value::Boolean(b)) => Some(b.to_string()),
        // A value of the wrong type is not a value. It falls through to the
        // default, exactly as it did before the registry existed: the old
        // reader used `toml::Value::as_bool`, so `enabled = "true"` (a quoted
        // boolean, a common mistake) was a type mismatch that fell to `false`.
        _ => None,
    }
}

/// Parse `config.toml` under an explicit directory, or `None` if it is missing
/// or unreadable.
///
/// Deliberately silent: this is on the path of every command, and a malformed
/// config must never block a task capture. `tasqx config` does NOT use this —
/// it reports the parse error, because there the user is asking about the file.
fn read_table_in(dir: &std::path::Path) -> Option<toml::Table> {
    let text = std::fs::read_to_string(dir.join("config.toml")).ok()?;
    text.parse::<toml::Table>().ok()
}

/// The loud counterpart of `read_table_in`, for `tasqx config` only.
///
/// Silent degradation is right on the path of every command — a malformed
/// config must never block a task capture. It is indefensible for `config
/// list`/`get`, where the user is explicitly asking about the file: they would
/// be told they never set the value, which is the exact confusion that sends
/// someone looking at the wrong thing. `set` already reported the parse error;
/// the read side just did not use the same door.
///
/// `Ok(None)` means "no file", which is a legitimate fresh-install state.
pub fn read_table_strict(dir: &std::path::Path) -> Result<Option<toml::Table>, ApiError> {
    let path = dir.join("config.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => text.parse::<toml::Table>().map(Some).map_err(|e| {
            ApiError::bad_request(format!("{} is not valid TOML: {e}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::bad_request(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

impl Kind {
    /// The declared type, named the way a TOML author would name it. Used only
    /// in the mismatch warning, so it must match `toml::Value::type_str`'s
    /// vocabulary — a user comparing "expected boolean, found string" against
    /// their file should not have to translate Rust's spelling.
    fn type_str(self) -> &'static str {
        match self {
            Kind::Str => "string",
            Kind::Bool => "boolean",
        }
    }
}

/// The file names a key, but with a value of a type the setting does not
/// declare — `name = 42` where a string was expected.
///
/// Carried out of the reader instead of printed inside it so `config.rs` stays
/// free of I/O and a test can assert the detection without capturing stderr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub key: &'static str,
    pub declared: &'static str,
    pub found: &'static str,
    pub path: PathBuf,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately does NOT claim which value is used instead: `$TASQX_THEME`
        // and `--theme` both outrank the file, so "falling back to the default"
        // would be a lie in exactly the sessions where it matters most.
        //
        // No articles before the type names: "a integer" is what an `a {}`
        // template produces, and the set of found types is open enough
        // (integer, array, …) that hand-picking articles is a bug waiting to
        // happen for one word of polish.
        write!(
            f,
            "{} in {}: expected {}, found {} — the file's value is ignored",
            self.key,
            self.path.display(),
            self.declared,
            self.found
        )
    }
}

/// What `config.toml` says about one setting, read strictly.
///
/// `value` is `None` both when the key is absent and when it is present but
/// wrong-typed; `mismatch` is what distinguishes those two, and it is the whole
/// reason this type exists. Before it, `toml_value_strict` returned a bare
/// `Option` and the two cases were indistinguishable — so `config get
/// theme.name` answered `nord` for a file that plainly said `name = 42`, which
/// is the "you never set this" confusion D25 set out to kill, one layer down.
#[derive(Clone, Debug, Default)]
pub struct FileValue {
    pub value: Option<String>,
    pub mismatch: Option<Mismatch>,
}

/// One setting's value, reading the file strictly. Used by `tasqx config`.
///
/// Reports a wrong type rather than swallowing it. The silent counterpart
/// [`toml_value_in`] must keep swallowing it: it is on the path of every
/// command, and nothing about a bad config line may stand between a user and a
/// captured task.
pub fn toml_value_strict(s: &Setting) -> Result<FileValue, ApiError> {
    let Some(dir) = config_dir() else {
        return Ok(FileValue::default());
    };
    toml_value_strict_in(&dir, s)
}

/// [`toml_value_strict`] under an explicit directory.
///
/// The directory is a parameter for the same reason [`toml_value_in`] takes
/// one: tests must exercise a real file without mutating process-global env,
/// which cargo's parallel test threads make racy. Without this split the
/// wrong-type detection below could only be tested through the ambient
/// `$TASQX_CONFIG_DIR`, and two tests setting it at once would flake.
pub fn toml_value_strict_in(dir: &std::path::Path, s: &Setting) -> Result<FileValue, ApiError> {
    let Some(table) = read_table_strict(dir)? else {
        return Ok(FileValue::default());
    };
    let (section, name) = s.parts();
    let Some(v) = table.get(section).and_then(|t| t.get(name)).cloned() else {
        return Ok(FileValue::default());
    };
    let found = v.type_str();
    match coerce(s.kind, v) {
        Some(value) => Ok(FileValue {
            value: Some(value),
            mismatch: None,
        }),
        None => Ok(FileValue {
            value: None,
            mismatch: Some(Mismatch {
                key: s.key,
                declared: s.kind.type_str(),
                found,
                path: dir.join("config.toml"),
            }),
        }),
    }
}

/// One setting's raw value from a `config.toml` under an explicit directory.
///
/// The directory is a parameter rather than an ambient `$TASQX_CONFIG_DIR`
/// read so tests can exercise a real file without mutating process-global env,
/// which cargo's parallel test threads make racy. Same move `datetime.rs`
/// already makes by taking an explicit `now`.
/// Matching is by declared `Kind`, not "whatever converts". The first version
/// of this accepted any scalar and stringified it, which quietly changed
/// behaviour: the old reader used `toml::Value::as_bool`, so `enabled = "true"`
/// (a quoted boolean, a common mistake) was a type mismatch and fell to
/// `false`. Stringifying turned that exact input into `true`, so a user who had
/// been silent since install would start getting OS toasts after an upgrade —
/// on the one code path whose doc comment promises every failure mode lands on
/// "don't notify".
pub fn toml_value_in(dir: &std::path::Path, s: &Setting) -> Option<String> {
    let (section, name) = s.parts();
    let v = read_table_in(dir)?.get(section)?.get(name)?.clone();
    coerce(s.kind, v)
}

/// One setting's raw value from the user's real `config.toml`.
pub fn toml_value(s: &Setting) -> Option<String> {
    toml_value_in(&config_dir()?, s)
}

/// Load a `config.toml` as an editable document, preserving comments and
/// layout. An absent file is an empty document; an unparseable one is an error,
/// because the caller is about to write and must not clobber content it cannot
/// read.
fn read_document(path: &std::path::Path) -> Result<toml_edit::DocumentMut, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.parse::<toml_edit::DocumentMut>().map_err(|e| {
            ApiError::bad_request(format!(
                "{} is not valid TOML and was left untouched: {e}",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml_edit::DocumentMut::new()),
        Err(e) => Err(ApiError::bad_request(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Write the document back atomically: a temp file in the same directory, then
/// a rename. A crash mid-write would otherwise leave no config at all — and the
/// reader degrades silently, so the user would get no error, just their theme
/// quietly reverting.
/// A scratch path in the target's own directory, private to this writer.
///
/// The first version was `path.with_extension("toml.tmp")` — ONE fixed name for
/// every writer on the machine. Two `tasqx config set` processes racing (a
/// script, a shell and an editor, two terminals) would write the same file
/// interleaved and each then rename it over `config.toml`, so one could publish
/// the other's half-written document as the user's config. The rename stays the
/// atomic publish step; the pid and counter only make the SOURCE of that rename
/// nobody else's business.
///
/// Same directory as the target on purpose: a rename is only atomic within one
/// filesystem, and `$TMPDIR` is routinely on another volume.
fn scratch_path(path: &std::path::Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    // The pid separates processes; the counter separates writes within one
    // process, which matters for the test suite's parallel threads.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let stem = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = path.parent().map(PathBuf::from).unwrap_or_default();
    dir.join(format!(".{stem}.{}.{n}.tmp", std::process::id()))
}

fn write_document(
    path: &std::path::Path,
    doc: &toml_edit::DocumentMut,
) -> Result<PathBuf, ApiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::internal(format!("cannot create {}: {e}", parent.display())))?;
    }
    let tmp = scratch_path(path);
    // Both failure paths remove the scratch file before returning: it is an
    // implementation detail of a write that did not happen, and leaving it
    // beside config.toml means every failed `config set` adds one more file the
    // user has to recognise as debris and delete by hand.
    if let Err(e) = std::fs::write(&tmp, doc.to_string()) {
        let _ = std::fs::remove_file(&tmp); // A partial write still leaves a file.
        return Err(ApiError::internal(format!(
            "cannot write {}: {e}",
            tmp.display()
        )));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ApiError::internal(format!(
            "cannot replace {}: {e}",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

/// The one explanation of why a `Home::Store` setting cannot be written through
/// `config.toml`, and what to do instead.
///
/// Two places refuse this now — `write_value_in`, which is reached by `config
/// set`, and the settings screen, which never attempts the write and so cannot
/// borrow the writer's error. A second literal would drift, and the two answers
/// a user gets to the same question would disagree about which command works.
pub fn store_home_message(s: &Setting) -> String {
    format!(
        "{} lives in the store, not config.toml — set it with `tasqx use <project>`, \
         which validates the name against this store (D21)",
        s.key
    )
}

/// Set one setting in a `config.toml` under an explicit directory, creating the
/// section if needed.
pub fn write_value_in(
    dir: &std::path::Path,
    s: &Setting,
    value: &str,
) -> Result<PathBuf, ApiError> {
    if s.home != Home::Toml {
        return Err(ApiError::bad_request(store_home_message(s)));
    }
    // Built once as a bare `Value` so both branches below can use it: the
    // decor-preserving path needs a Value, the insert path wraps it in an Item.
    let parsed: toml_edit::Value = match s.kind {
        Kind::Bool => match value {
            "true" => true.into(),
            "false" => false.into(),
            _ => {
                return Err(ApiError::bad_request(format!(
                    "{} takes true or false, got {value:?}",
                    s.key
                )))
            }
        },
        Kind::Str => value.into(),
    };
    let path = dir.join("config.toml");
    let mut doc = read_document(&path)?;
    let (section, name) = s.parts();
    // Seed a real `[section]` table when the file does not have one yet.
    // Assigning straight into a missing key makes toml_edit emit an implicit
    // inline table (`theme = { name = "mono" }`), which is valid TOML but not
    // the hand-editable shape a config file the user opens should have.
    if !doc.contains_key(section) {
        doc.insert(section, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    // Replace the VALUE, not the Item. `doc[section][name] = parsed` swaps the
    // whole entry including its decor, which silently drops an inline comment:
    // `name = "gruvbox"  # for the projector` came back as `name = "mono"`.
    // That is the spec's own motivating example, and cmddoc tells the user this
    // command preserves their comments — so losing it made the help text a lie.
    match doc[section].get_mut(name).and_then(|i| i.as_value_mut()) {
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = parsed;
            *existing.decor_mut() = decor;
        }
        // The key is absent: a plain insert, with no decor to carry over.
        None => doc[section][name] = toml_edit::Item::Value(parsed),
    }
    write_document(&path, &doc)
}

/// Set one setting in the user's real `config.toml`.
pub fn write_value(s: &Setting, value: &str) -> Result<PathBuf, ApiError> {
    let dir = config_dir()
        .ok_or_else(|| ApiError::bad_request("no config directory on this platform"))?;
    write_value_in(&dir, s, value)
}

/// Remove one setting from a `config.toml` under an explicit directory, so it
/// falls back to its default. Returns whether the key was actually present.
pub fn clear_value_in(dir: &std::path::Path, s: &Setting) -> Result<bool, ApiError> {
    if s.home != Home::Toml {
        return Err(ApiError::bad_request(format!(
            "{} lives in the store, not config.toml",
            s.key
        )));
    }
    let path = dir.join("config.toml");
    let mut doc = read_document(&path)?;
    let (section, name) = s.parts();
    let existed = doc
        .get_mut(section)
        .and_then(|t| t.as_table_like_mut())
        .map(|t| t.remove(name).is_some())
        .unwrap_or(false);
    if existed {
        write_document(&path, &doc)?;
    }
    Ok(existed)
}

/// Remove one setting from the user's real `config.toml`.
pub fn clear_value(s: &Setting) -> Result<bool, ApiError> {
    let dir = config_dir()
        .ok_or_else(|| ApiError::bad_request("no config directory on this platform"))?;
    clear_value_in(&dir, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D9's chain is flag > env > config.toml > default. Before this registry it
    /// existed in exactly one place (`theme::resolve_name`) and every other
    /// setting re-invented a shorter version at its own call site, so "why did
    /// my flag not win" had a different answer per setting.
    #[test]
    fn resolve_follows_the_d9_chain_and_names_its_source() {
        let s = find("theme.name").expect("theme.name is a known setting");
        assert_eq!(
            resolve(s, Some("mono"), Some("gruvbox")),
            ("mono".into(), Source::Flag)
        );
        assert_eq!(
            resolve(s, None, Some("gruvbox")),
            ("gruvbox".into(), Source::File)
        );
        assert_eq!(resolve(s, None, None), ("nord".into(), Source::Default));
    }

    /// A whitespace-only value at any layer means "unset", not "the empty
    /// string". `theme::resolve_name` already did this; the generic resolver
    /// must keep it, or `TASQX_THEME=" "` starts selecting a theme named " ".
    #[test]
    fn a_blank_value_at_any_layer_is_treated_as_absent() {
        let s = find("theme.name").unwrap();
        assert_eq!(
            resolve(s, Some("   "), Some("gruvbox")),
            ("gruvbox".into(), Source::File)
        );
        assert_eq!(resolve(s, None, Some("")), ("nord".into(), Source::Default));
    }

    /// An unknown key must be rejected rather than silently ignored. Today an
    /// unknown key in config.toml is read by nothing and reported by nothing,
    /// so a typo'd `[theme] nmae` looks like it worked.
    #[test]
    fn unknown_keys_are_not_settings() {
        assert!(find("theme.nmae").is_none());
        assert!(find("").is_none());
    }

    /// Every registry entry must be reachable by its own key, and keys must be
    /// unique — a duplicate would shadow the earlier entry silently.
    #[test]
    fn every_setting_is_findable_and_keys_are_unique() {
        let mut keys: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), before, "duplicate key in SETTINGS");
        for s in SETTINGS {
            assert!(find(s.key).is_some(), "{} is not findable", s.key);
        }
    }

    /// A private directory per test. These tests used to set `$TASQX_CONFIG_DIR`,
    /// which is process-global and therefore racy under cargo's parallel test
    /// threads: one test's writer could observe another's directory. Passing the
    /// directory in removes the shared mutable state entirely.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tasqx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// `config.toml` reading used to be three functions that each hard-coded a
    /// key path and swallowed every failure. The registry-driven reader has to
    /// return the same values for the same file, including returning None for a
    /// key the file does not mention.
    #[test]
    fn toml_value_reads_a_setting_out_of_a_real_file() {
        let dir = temp_dir("cfg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[theme]
name = \"gruvbox\"

[notify]
enabled = true
",
        )
        .unwrap();

        assert_eq!(
            toml_value_in(&dir, find("theme.name").unwrap()).as_deref(),
            Some("gruvbox")
        );
        assert_eq!(
            toml_value_in(&dir, find("notify.enabled").unwrap()).as_deref(),
            Some("true")
        );

        std::fs::write(
            dir.join("config.toml"),
            "[theme]
name = \"mono\"
",
        )
        .unwrap();
        assert_eq!(
            toml_value_in(&dir, find("notify.enabled").unwrap()),
            None,
            "absent key is None"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reason this module writes through toml_edit rather than a
    /// toml::Table round trip. Verified by running it: the round trip drops
    /// every comment AND reorders sections alphabetically. A user who wrote
    /// "# gruvbox because the office projector washes out nord" loses it on the
    /// first `tasqx config set`, with no warning.
    #[test]
    fn writing_a_setting_preserves_comments_and_order() {
        let dir = temp_dir("w");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = "# chosen for the projector
[theme]
name = \"gruvbox\"

[notify]
enabled = true
";
        std::fs::write(&path, original).unwrap();

        write_value_in(&dir, find("theme.name").unwrap(), "mono").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();

        assert!(
            after.contains("# chosen for the projector"),
            "comment lost:
{after}"
        );
        assert!(
            after.contains("name = \"mono\""),
            "value not written:
{after}"
        );
        assert!(
            after.find("[theme]") < after.find("[notify]"),
            "sections reordered:
{after}"
        );
        assert!(
            after.contains("enabled = true"),
            "other section damaged:
{after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writing must work when there is no file and no directory yet, which is
    /// the state of every fresh install — and it must produce a real `[theme]`
    /// section. Assigning into a missing key makes toml_edit emit an implicit
    /// inline table (`theme = { name = "mono" }`): valid TOML, but not a shape
    /// a user opening the file would recognise or extend by hand.
    #[test]
    fn writing_creates_the_file_and_its_directory() {
        let dir = temp_dir("new");

        write_value_in(&dir, find("theme.name").unwrap(), "mono").unwrap();
        let after = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(after.contains("[theme]"), "{after}");
        assert!(after.contains("mono"), "{after}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file we cannot parse must NOT be overwritten. Replacing it with a
    /// valid file that has lost the user's content is worse than refusing, and
    /// the silent reader would never have told them either way.
    #[test]
    fn writing_refuses_to_clobber_an_unparseable_file() {
        let dir = temp_dir("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[theme
name = broken",
        )
        .unwrap();

        let err = write_value_in(&dir, find("theme.name").unwrap(), "mono").unwrap_err();
        assert!(err.message.contains("config.toml"), "{}", err.message);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[theme
name = broken",
            "the unparseable file must be left exactly as it was"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A key set then read back must round-trip through a real file, and
    /// unsetting it must fall back to the built-in default. This is the whole
    /// contract of `config set` / `config unset` in one assertion chain.
    #[test]
    fn a_toml_key_round_trips_and_unset_falls_back_to_the_default() {
        let dir = temp_dir("rt");
        let s = find("theme.name").unwrap();

        write_value_in(&dir, s, "gruvbox").unwrap();
        assert_eq!(toml_value_in(&dir, s).as_deref(), Some("gruvbox"));

        assert!(
            clear_value_in(&dir, s).unwrap(),
            "the key was present, so removal is reported"
        );
        assert_eq!(
            toml_value_in(&dir, s),
            None,
            "unset means the file no longer names it"
        );
        assert!(
            !clear_value_in(&dir, s).unwrap(),
            "a second unset removes nothing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The old reader used `toml::Value::as_bool`, so a quoted boolean was a
    /// type mismatch that fell to `false`. The first registry version
    /// stringified every scalar and compared `v == "true"`, which turned that
    /// exact input into `true` — a real user who wrote `enabled = "true"` and
    /// had been silent since install would start getting OS toasts after an
    /// upgrade. Found by a differential test against the pre-registry reader,
    /// not by review: no test in the suite covered a wrong-typed value.
    #[test]
    fn a_wrong_typed_value_falls_back_to_the_default_rather_than_being_coerced() {
        let dir = temp_dir("wrongtype");
        std::fs::create_dir_all(&dir).unwrap();
        let notify = find("notify.enabled").unwrap();
        let theme = find("theme.name").unwrap();

        for bad in ["\"true\"", "\" true \"", "1"] {
            std::fs::write(
                dir.join("config.toml"),
                format!(
                    "[notify]
enabled = {bad}
"
                ),
            )
            .unwrap();
            assert_eq!(
                toml_value_in(&dir, notify),
                None,
                "a quoted/numeric boolean must not be read as one: {bad}"
            );
        }
        // The right type still reads.
        std::fs::write(
            dir.join("config.toml"),
            "[notify]
enabled = true
",
        )
        .unwrap();
        assert_eq!(toml_value_in(&dir, notify).as_deref(), Some("true"));

        // Symmetric: a bare boolean where a string is declared is not a string.
        std::fs::write(
            dir.join("config.toml"),
            "[theme]
name = true
",
        )
        .unwrap();
        assert_eq!(toml_value_in(&dir, theme), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The silent fallback above is right, but `tasqx config` must not inherit
    /// it. D25 made `config` loud about a MALFORMED file; `name = 42` parses
    /// fine and only the declared kind disagrees, so it slipped through and
    /// `config get theme.name` answered `nord` for a file that plainly said
    /// otherwise — the same "you never set this" confusion D25 called
    /// indefensible, one layer down. The report has to carry the key, the
    /// declared type and what was actually found, because with any one of those
    /// missing the user still cannot tell which line to fix.
    #[test]
    fn the_strict_reader_reports_a_wrong_typed_value_where_the_silent_one_hides_it() {
        let dir = temp_dir("mismatch");
        std::fs::create_dir_all(&dir).unwrap();
        let theme = find("theme.name").unwrap();
        std::fs::write(dir.join("config.toml"), "[theme]\nname = 42\n").unwrap();

        assert_eq!(
            toml_value_in(&dir, theme),
            None,
            "the silent reader still degrades"
        );

        let read = toml_value_strict_in(&dir, theme).expect("a wrong type is not a parse error");
        assert_eq!(read.value, None, "the caller still gets the fallback");
        let m = read
            .mismatch
            .expect("the strict reader must report the mismatch");
        assert_eq!(m.key, "theme.name");
        assert_eq!(m.declared, "string");
        assert_eq!(m.found, "integer");
        let msg = m.to_string();
        assert!(msg.contains("theme.name"), "{msg}");
        assert!(
            msg.contains("config.toml"),
            "the message must locate the file: {msg}"
        );

        // Symmetric for the other Kind, so the report is not string-specific.
        std::fs::write(dir.join("config.toml"), "[notify]\nenabled = \"true\"\n").unwrap();
        let n = toml_value_strict_in(&dir, find("notify.enabled").unwrap()).unwrap();
        let m = n.mismatch.expect("a quoted boolean is a mismatch too");
        assert_eq!((m.declared, m.found), ("boolean", "string"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mismatch check that fired on a well-typed value, or on a key the file
    /// never mentions, would make `tasqx config` warn on every run — noise that
    /// trains the user to ignore the one warning that matters.
    #[test]
    fn the_strict_reader_is_silent_when_there_is_nothing_wrong() {
        let dir = temp_dir("nomismatch");
        std::fs::create_dir_all(&dir).unwrap();
        let theme = find("theme.name").unwrap();

        std::fs::write(dir.join("config.toml"), "[theme]\nname = \"gruvbox\"\n").unwrap();
        let ok = toml_value_strict_in(&dir, theme).unwrap();
        assert_eq!(ok.value.as_deref(), Some("gruvbox"));
        assert!(
            ok.mismatch.is_none(),
            "a value of the declared type is not a problem"
        );

        // An absent key is a fresh install, not a mistake.
        std::fs::write(dir.join("config.toml"), "[notify]\nenabled = true\n").unwrap();
        let absent = toml_value_strict_in(&dir, theme).unwrap();
        assert_eq!(absent.value, None);
        assert!(
            absent.mismatch.is_none(),
            "a key the file omits must not warn"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `tasqx config` must be LOUD about a file the user is explicitly asking
    /// about. `set` already reported the parse error while `list`/`get` read
    /// through the silent path and answered with the built-in default — so
    /// someone whose theme had mysteriously reverted asked `config get` and was
    /// told they never set it, which is the exact confusion the spec called
    /// indefensible.
    #[test]
    fn the_strict_reader_reports_a_malformed_file_where_the_silent_one_hides_it() {
        let dir = temp_dir("strict");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[theme
name = broken",
        )
        .unwrap();

        assert!(
            read_table_in(&dir).is_none(),
            "the silent reader still degrades"
        );
        let err = read_table_strict(&dir).expect_err("the strict reader must report it");
        assert!(err.message.contains("not valid TOML"), "{}", err.message);
        assert!(err.message.contains("config.toml"), "{}", err.message);

        // A missing file is not an error: that is a fresh install.
        let empty = temp_dir("strict-empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(read_table_strict(&empty)
            .expect("no file is fine")
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    /// The spec's own motivating example for choosing toml_edit carries an
    /// INLINE comment, and the first implementation dropped exactly that one:
    /// `doc[section][name] = parsed` replaces the whole Item including its
    /// decor. The block comment survived, so the original guard passed while
    /// the headline case failed — and cmddoc tells the user this command
    /// preserves their comments.
    #[test]
    fn writing_preserves_an_inline_comment_on_the_edited_line() {
        let dir = temp_dir("inline");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "# block note
[theme]
name = \"gruvbox\"  # inline note
",
        )
        .unwrap();

        write_value_in(&dir, find("theme.name").unwrap(), "mono").unwrap();
        let after = std::fs::read_to_string(dir.join("config.toml")).unwrap();

        assert!(
            after.contains("# block note"),
            "block comment lost:
{after}"
        );
        assert!(
            after.contains("# inline note"),
            "INLINE comment lost:
{after}"
        );
        assert!(
            after.contains("name = \"mono\""),
            "value not written:
{after}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The write lands via a temp file and a rename so a crash cannot leave the
    /// user with no config at all — and the reader degrades silently, so they
    /// would get no error, just their theme quietly reverting. Nothing checked
    /// it: replacing the two-step write with a direct `fs::write` left all 330
    /// tests green, so the doc comment sold a durability property CI could not
    /// see.
    #[test]
    fn writing_leaves_no_temp_file_behind_and_replaces_atomically() {
        let dir = temp_dir("atomic");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[theme]
name = \"nord\"
",
        )
        .unwrap();

        write_value_in(&dir, find("theme.name").unwrap(), "mono").unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "config.toml")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp file not cleaned up: {leftovers:?}"
        );
        // The rename target is the real file, not a sibling.
        assert!(std::fs::read_to_string(dir.join("config.toml"))
            .unwrap()
            .contains("mono"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Defaults are user-visible behaviour, and the registry makes flipping one
    /// a single-token edit. `notify.enabled` was unpinned: changing it from
    /// "false" to "true" left all 330 tests green while turning native OS
    /// notifications on for every install. Pinned by literal here rather than
    /// by reading `Setting::default`, because a test that derives both sides
    /// from the same constant moves with it and guards nothing.
    #[test]
    fn the_shipped_defaults_are_pinned_by_literal() {
        assert_eq!(find("theme.name").unwrap().default, "nord");
        assert_eq!(
            find("notify.enabled").unwrap().default,
            "false",
            "toasts are opt-in"
        );
        assert_eq!(find("default_project").unwrap().default, "");
    }

    /// `write_value_in` refuses a store-homed key with a sentence that tells the
    /// user where to go instead (`tasqx use`). The settings TUI must refuse with
    /// the SAME sentence, and it cannot call `write_value_in` to get it — it
    /// never attempts the write at all. Before this function the text existed
    /// only as a literal inside `write_value_in`, so the second refusal site had
    /// no way to reuse it and would have answered the same question differently.
    #[test]
    fn the_store_home_refusal_has_exactly_one_wording() {
        let dir = temp_dir("storemsg");
        let s = find("default_project").unwrap();

        let from_writer = write_value_in(&dir, s, "work").unwrap_err().message;
        assert_eq!(
            from_writer,
            store_home_message(s),
            "two wordings for one refusal"
        );
        assert!(
            from_writer.contains("tasqx use"),
            "must name the command that works: {from_writer}"
        );
        assert!(
            from_writer.contains("default_project"),
            "must name the key: {from_writer}"
        );
        // The refusal happens before any filesystem work: a rejected write must
        // not leave a config directory behind.
        assert!(!dir.exists(), "a refused write created {}", dir.display());
    }

    /// `Choices` is what lets an interactive editor decide "Enter opens a
    /// picker" without testing `key == "theme.name"` itself. Pinned by literal
    /// per key: deriving both sides from `Setting::choices` would move with any
    /// edit and guard nothing. A `theme.name` silently downgraded to `Free`
    /// leaves `config edit` with no theme picker — and the live theme preview is
    /// the whole reason that screen exists.
    #[test]
    fn only_the_theme_setting_declares_a_closed_value_set() {
        assert_eq!(find("theme.name").unwrap().choices, Choices::Themes);
        assert_eq!(find("notify.enabled").unwrap().choices, Choices::Free);
        assert_eq!(find("default_project").unwrap().choices, Choices::Free);
    }

    /// Two `config set` processes shared ONE scratch filename
    /// (`config.toml.tmp`), so the loser's rename could publish the winner's
    /// half-written document as the user's config — and a failed rename left
    /// that scratch file sitting beside `config.toml` forever.
    ///
    /// Both halves are guarded here: the name must be private to the writer,
    /// and a failure must leave the directory exactly as it found it.
    #[test]
    fn the_scratch_file_is_private_and_never_litters() {
        let dir = temp_dir("tmp-private");
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("config.toml");

        // Private: no two scratch paths collide, and the name carries the pid
        // so a CONCURRENT process cannot pick the same one either.
        let a = scratch_path(&target);
        let b = scratch_path(&target);
        assert_ne!(a, b, "two writers must not share one scratch file: {a:?}");
        let pid = std::process::id().to_string();
        for p in [&a, &b] {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(
                name.contains(&pid),
                "scratch name must carry the pid: {name}"
            );
            assert_eq!(
                p.parent(),
                target.parent(),
                "scratch must share the target's directory"
            );
        }

        // No litter: a rename that cannot succeed must clean up after itself.
        // A directory standing where config.toml goes makes the rename fail on
        // every platform without needing permissions games.
        std::fs::create_dir_all(&target).unwrap();
        let doc: toml_edit::DocumentMut = "[theme]
name = \"nord\"
"
        .parse()
        .unwrap();
        let err = write_document(&target, &doc).expect_err("rename onto a directory must fail");
        assert!(
            err.message.contains("config.toml"),
            "the error must name the file: {}",
            err.message
        );

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "config.toml")
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed write left scratch files behind: {leftovers:?}"
        );
    }
}
