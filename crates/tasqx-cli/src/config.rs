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

/// Parse `config.toml`, or `None` if it is missing or unreadable.
///
/// Deliberately silent: this is on the path of every command, and a malformed
/// config must never block a task capture. `tasqx config` does NOT use this —
/// it reports the parse error, because there the user is asking about the file.
fn read_table() -> Option<toml::Table> {
    let text = std::fs::read_to_string(config_path()?).ok()?;
    text.parse::<toml::Table>().ok()
}

/// One setting's raw value from `config.toml`, rendered as a string so every
/// `Kind` shares one path. `None` when the file, section or key is absent.
pub fn toml_value(s: &Setting) -> Option<String> {
    let (section, name) = s.parts();
    let v = read_table()?.get(section)?.get(name)?.clone();
    match v {
        toml::Value::String(x) => Some(x),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }
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

    /// `config.toml` reading used to be three functions that each hard-coded a
    /// key path and swallowed every failure. The registry-driven reader has to
    /// return the same values for the same file, including returning None for a
    /// key the file does not mention.
    #[test]
    fn toml_value_reads_a_setting_out_of_a_real_file() {
        let dir = std::env::temp_dir().join(format!("tasqx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[theme]\nname = \"gruvbox\"\n\n[notify]\nenabled = true\n",
        )
        .unwrap();
        let prev = std::env::var("TASQX_CONFIG_DIR").ok();
        std::env::set_var("TASQX_CONFIG_DIR", &dir);

        assert_eq!(toml_value(find("theme.name").unwrap()).as_deref(), Some("gruvbox"));
        assert_eq!(toml_value(find("notify.enabled").unwrap()).as_deref(), Some("true"));

        std::fs::write(dir.join("config.toml"), "[theme]\nname = \"mono\"\n").unwrap();
        assert_eq!(toml_value(find("notify.enabled").unwrap()), None, "absent key is None");

        match prev {
            Some(v) => std::env::set_var("TASQX_CONFIG_DIR", v),
            None => std::env::remove_var("TASQX_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
