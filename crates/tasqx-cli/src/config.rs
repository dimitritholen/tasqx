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

/// The loud counterpart of [`read_table_in`], for `tasqx config` only.
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
        Err(e) => Err(ApiError::bad_request(format!("cannot read {}: {e}", path.display()))),
    }
}

/// One setting's value, reading the file strictly. Used by `tasqx config`.
pub fn toml_value_strict(s: &Setting) -> Result<Option<String>, ApiError> {
    let Some(dir) = config_dir() else { return Ok(None) };
    let Some(table) = read_table_strict(&dir)? else { return Ok(None) };
    let (section, name) = s.parts();
    let Some(v) = table.get(section).and_then(|t| t.get(name)).cloned() else {
        return Ok(None);
    };
    Ok(coerce(s.kind, v))
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
            std::fs::write(dir.join("config.toml"), format!("[notify]
enabled = {bad}
")).unwrap();
            assert_eq!(
                toml_value_in(&dir, notify),
                None,
                "a quoted/numeric boolean must not be read as one: {bad}"
            );
        }
        // The right type still reads.
        std::fs::write(dir.join("config.toml"), "[notify]
enabled = true
").unwrap();
        assert_eq!(toml_value_in(&dir, notify).as_deref(), Some("true"));

        // Symmetric: a bare boolean where a string is declared is not a string.
        std::fs::write(dir.join("config.toml"), "[theme]
name = true
").unwrap();
        assert_eq!(toml_value_in(&dir, theme), None);
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
        std::fs::write(dir.join("config.toml"), "[theme
name = broken").unwrap();

        assert!(read_table_in(&dir).is_none(), "the silent reader still degrades");
        let err = read_table_strict(&dir).expect_err("the strict reader must report it");
        assert!(err.message.contains("not valid TOML"), "{}", err.message);
        assert!(err.message.contains("config.toml"), "{}", err.message);

        // A missing file is not an error: that is a fresh install.
        let empty = temp_dir("strict-empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(read_table_strict(&empty).expect("no file is fine").is_none());
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

        assert!(after.contains("# block note"), "block comment lost:
{after}");
        assert!(after.contains("# inline note"), "INLINE comment lost:
{after}");
        assert!(after.contains("name = \"mono\""), "value not written:
{after}");
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
        std::fs::write(dir.join("config.toml"), "[theme]
name = \"nord\"
").unwrap();

        write_value_in(&dir, find("theme.name").unwrap(), "mono").unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "config.toml")
            .collect();
        assert!(leftovers.is_empty(), "temp file not cleaned up: {leftovers:?}");
        // The rename target is the real file, not a sibling.
        assert!(std::fs::read_to_string(dir.join("config.toml")).unwrap().contains("mono"));
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
        assert_eq!(find("notify.enabled").unwrap().default, "false", "toasts are opt-in");
        assert_eq!(find("default_project").unwrap().default, "");
    }

}
