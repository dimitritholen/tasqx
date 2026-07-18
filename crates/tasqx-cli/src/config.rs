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
            Source::Env => s.env.map(|e| format!("${e}")).unwrap_or_else(|| "env".into()),
            Source::Flag => s.flag.map(|f| f.to_string()).unwrap_or_else(|| "flag".into()),
        }
    }
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
    pub summary: &'static str,
}

impl Setting {
    /// The `[section]` and key halves of a dotted name.
    pub fn parts(&self) -> (&'static str, &'static str) {
        self.key.split_once('.').expect("every SETTINGS key is section.name")
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
        summary: "Terminal theme: a built-in (nord, gruvbox, dracula, solarized, mono) or a user file.",
    },
    Setting {
        key: "notify.enabled",
        home: Home::Toml,
        kind: Kind::Bool,
        default: "false",
        env: None,
        flag: None,
        summary: "Allow the daemon to raise native OS notifications for reminders.",
    },
    Setting {
        key: "default_project",
        home: Home::Store,
        kind: Kind::Str,
        default: "",
        env: None,
        flag: None,
        summary: "Project a bare `tasqx add` files into. Lives in the store; set it with `tasqx use`.",
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

/// One setting's raw value from a `config.toml` under an explicit directory.
///
/// The directory is a parameter rather than an ambient `$TASQX_CONFIG_DIR`
/// read so tests can exercise a real file without mutating process-global env,
/// which cargo's parallel test threads make racy. Same move `datetime.rs`
/// already makes by taking an explicit `now`.
pub fn toml_value_in(dir: &std::path::Path, s: &Setting) -> Option<String> {
    let (section, name) = s.parts();
    let v = read_table_in(dir)?.get(section)?.get(name)?.clone();
    match v {
        toml::Value::String(x) => Some(x),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }
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
        Err(e) => Err(ApiError::bad_request(format!("cannot read {}: {e}", path.display()))),
    }
}

/// Write the document back atomically: a temp file in the same directory, then
/// a rename. A crash mid-write would otherwise leave no config at all — and the
/// reader degrades silently, so the user would get no error, just their theme
/// quietly reverting.
fn write_document(path: &std::path::Path, doc: &toml_edit::DocumentMut) -> Result<PathBuf, ApiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::internal(format!("cannot create {}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())
        .map_err(|e| ApiError::internal(format!("cannot write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| ApiError::internal(format!("cannot replace {}: {e}", path.display())))?;
    Ok(path.to_path_buf())
}

/// Set one setting in a `config.toml` under an explicit directory, creating the
/// section if needed.
pub fn write_value_in(dir: &std::path::Path, s: &Setting, value: &str) -> Result<PathBuf, ApiError> {
    if s.home != Home::Toml {
        return Err(ApiError::bad_request(format!(
            "{} lives in the store, not config.toml — set it with `tasqx use <project>`, \
             which validates the name against this store (D21)",
            s.key
        )));
    }
    let parsed = match s.kind {
        Kind::Bool => match value {
            "true" => toml_edit::value(true),
            "false" => toml_edit::value(false),
            _ => {
                return Err(ApiError::bad_request(format!(
                    "{} takes true or false, got {value:?}",
                    s.key
                )))
            }
        },
        Kind::Str => toml_edit::value(value),
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
    doc[section][name] = parsed;
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
        assert_eq!(resolve(s, Some("mono"), Some("gruvbox")), ("mono".into(), Source::Flag));
        assert_eq!(resolve(s, None, Some("gruvbox")), ("gruvbox".into(), Source::File));
        assert_eq!(resolve(s, None, None), ("nord".into(), Source::Default));
    }

    /// A whitespace-only value at any layer means "unset", not "the empty
    /// string". `theme::resolve_name` already did this; the generic resolver
    /// must keep it, or `TASQX_THEME=" "` starts selecting a theme named " ".
    #[test]
    fn a_blank_value_at_any_layer_is_treated_as_absent() {
        let s = find("theme.name").unwrap();
        assert_eq!(resolve(s, Some("   "), Some("gruvbox")), ("gruvbox".into(), Source::File));
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

        assert_eq!(toml_value_in(&dir, find("theme.name").unwrap()).as_deref(), Some("gruvbox"));
        assert_eq!(toml_value_in(&dir, find("notify.enabled").unwrap()).as_deref(), Some("true"));

        std::fs::write(dir.join("config.toml"), "[theme]
name = \"mono\"
").unwrap();
        assert_eq!(toml_value_in(&dir, find("notify.enabled").unwrap()), None, "absent key is None");

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

        assert!(after.contains("# chosen for the projector"), "comment lost:
{after}");
        assert!(after.contains("name = \"mono\""), "value not written:
{after}");
        assert!(after.find("[theme]") < after.find("[notify]"), "sections reordered:
{after}");
        assert!(after.contains("enabled = true"), "other section damaged:
{after}");

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
        std::fs::write(&path, "[theme
name = broken").unwrap();

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

        assert!(clear_value_in(&dir, s).unwrap(), "the key was present, so removal is reported");
        assert_eq!(toml_value_in(&dir, s), None, "unset means the file no longer names it");
        assert!(!clear_value_in(&dir, s).unwrap(), "a second unset removes nothing");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
