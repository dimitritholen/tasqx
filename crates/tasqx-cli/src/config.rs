//! The settings registry and the one D9 precedence resolver.
//!
//! Before this module, `config.toml` was read by four hand-written functions
//! that each hard-coded one key, and D9's precedence chain (defaults →
//! config.toml → `TASQX_*` → CLI flags) existed in exactly one place —
//! `theme::resolve_name`. Every other setting re-invented a shorter chain at
//! its own call site, so `--socket` and `--theme` obeyed different rules and
//! nothing said so. One registry plus one resolver means a new setting is a
//! table row, not four edits in three files.

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
}
