//! The interactive settings screen: `tasqx config edit` (DESIGN.md D26).
//!
//! [`App`] is a pure state machine — it owns the selection, the edit mode and
//! the pending value, and `on_key` touches neither the terminal nor the
//! filesystem. It answers a key press with an [`Action`] the caller performs.
//! [`render`] takes `&App` and a `Frame` and decides nothing.
//!
//! That split is what earns this screen its tests. It also buys the feature the
//! screen exists for: because `preview_theme` reports the theme the *current
//! state* implies — the candidate under the picker cursor, not the saved value —
//! the caller can reload the theme every frame and the user sees a theme before
//! committing to it. A config file can never do that.

use std::collections::BTreeMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::config::{self, Choices, Home, Kind, Setting};
use crate::render;
use crate::theme::{Caps, Theme};
use crate::tui::{first_visible, rt_style};
use tasqx_core::remote;

/// One editable line: a registry entry plus what it currently resolves to.
///
/// The `Setting` is borrowed from `config::SETTINGS` rather than copied field by
/// field, so the screen cannot drift from the registry: it has no name, default
/// or summary of its own to get wrong.
pub struct Row {
    pub setting: &'static Setting,
    pub value: String,
    /// The layer that supplied `value`, already labelled by `Source::label`.
    pub source: String,
    /// The acceptable values, when the registry declared a closed set. Supplied
    /// by the caller because the theme list is a filesystem question and `App`
    /// does no I/O.
    pub choices: Vec<String>,
}

/// What the screen is doing right now.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    Browse,
    /// An inline picker is open over the selected row's `choices`.
    Pick {
        cursor: usize,
    },
    /// `c` (connect): choosing among the `tasqx-remote-*` connectors found on
    /// PATH.
    SyncPick {
        names: Vec<String>,
        cursor: usize,
    },
    /// The connector's own fields, followed by the sync passphrase twice —
    /// the same values `tasqx sync setup` collects off argv/env/prompt
    /// (D201), collected here off this form instead (D233).
    SyncForm(SyncForm),
    /// `d` (disconnect): confirm before removing `<store>.sync.json` and
    /// `<store>.sync.key`. The remote itself is untouched, which the render
    /// side says so the confirm text does not have to be taken on faith.
    SyncConfirmDisconnect,
}

/// One connector field, plus what the user has typed into it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SyncField {
    pub key: String,
    pub label: String,
    pub secret: bool,
    pub help: String,
    pub help_url: Option<String>,
    pub value: String,
}

/// The connect form's state: the connector's own fields (from `describe`),
/// then the sync passphrase, asked twice exactly as the terminal prompt
/// asks it (D202, D233) — so a typo is caught here before it is ever sent anywhere.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SyncForm {
    pub connector: String,
    pub fields: Vec<SyncField>,
    pub cursor: usize,
    pub passphrase: String,
    pub passphrase_again: String,
    /// The connector's own `ok: false` error, or a local check (empty/
    /// mismatched passphrase) — shown inline, with the form kept open, never
    /// dropping what was already typed.
    pub error: Option<String>,
}

impl SyncForm {
    fn new(connector: String, fields: Vec<remote::Field>) -> Self {
        SyncForm {
            connector,
            fields: fields
                .into_iter()
                .map(|f| SyncField {
                    key: f.key,
                    label: f.label,
                    secret: f.secret,
                    help: f.help,
                    help_url: f.help_url,
                    value: String::new(),
                })
                .collect(),
            cursor: 0,
            passphrase: String::new(),
            passphrase_again: String::new(),
            error: None,
        }
    }

    /// The connector's own fields, plus the passphrase asked for twice.
    fn len(&self) -> usize {
        self.fields.len() + 2
    }

    fn is_secret(&self, i: usize) -> bool {
        self.fields.get(i).is_none_or(|f| f.secret)
    }

    fn label(&self, i: usize) -> &str {
        match self.fields.get(i) {
            Some(f) => &f.label,
            None if i == self.fields.len() => "Sync passphrase",
            None => "Confirm passphrase",
        }
    }

    fn value(&self, i: usize) -> &str {
        let n = self.fields.len();
        if let Some(f) = self.fields.get(i) {
            &f.value
        } else if i == n {
            &self.passphrase
        } else {
            &self.passphrase_again
        }
    }

    fn value_mut(&mut self, i: usize) -> &mut String {
        let n = self.fields.len();
        if i < n {
            &mut self.fields[i].value
        } else if i == n {
            &mut self.passphrase
        } else {
            &mut self.passphrase_again
        }
    }
}

/// This store's sync setup, as the Sync section shows it (#884). `App` does
/// no I/O, so every field here is supplied by the caller: it comes from
/// `<store>.sync.json` (and whether `<store>.sync.key` exists), the same
/// files `tasqx sync --status` reads.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct SyncInfo {
    pub connector: Option<String>,
    /// [`crate::sync::short_version`]'s eight characters, never the whole
    /// thing — this is a screen a person reads, not a script.
    pub version: Option<String>,
    /// A day, relative to now (`crate::render::day_ago`), not a timestamp —
    /// the same rule `sync --status` follows.
    pub synced_relative: Option<String>,
    pub encrypted: bool,
}

/// An intent for the caller to carry out. `App` never writes anything itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Persist `value` for `key` through `config::write_value`, then report the
    /// re-resolved result back via [`App::refresh`].
    Save {
        key: &'static str,
        value: String,
    },
    Quit,
    /// `c`: list the `tasqx-remote-*` connectors on PATH.
    SyncListConnectors,
    /// A name chosen from that list: ask it to `describe` itself.
    SyncDescribe {
        name: String,
    },
    /// The form's last field, submitted: the SAME write `tasqx sync setup`
    /// performs (`sync::apply_setup`), never a second implementation of it.
    SyncSubmit {
        name: String,
        values: BTreeMap<String, String>,
        passphrase: String,
    },
    /// `s`: run the same `tasqx sync` the command runs.
    SyncNow,
    /// The disconnect confirm's `y`: remove `<store>.sync.json` and
    /// `<store>.sync.key`. The remote is untouched.
    SyncDisconnect,
}

pub struct App {
    pub rows: Vec<Row>,
    pub selected: usize,
    pub mode: Mode,
    /// The last thing that happened, shown in the footer. Empty at startup.
    pub status: String,
    /// Index of the row whose values are themes, if any. Resolved once from
    /// `Choices::Themes` so neither `on_key` nor `preview_theme` has to test a
    /// setting key by name.
    theme_row: Option<usize>,
    /// The Sync section (#884). Separate from `rows`: it is not a registered
    /// setting, has no `config.toml` home, and its own three keys (`c`/`s`/
    /// `d`) act outside the row picker's Enter/toggle vocabulary.
    pub sync: SyncInfo,
}

impl App {
    pub fn new(rows: Vec<Row>, sync: SyncInfo) -> Self {
        // The module's own invariant, owned HERE instead of asserted in
        // comments at three call sites: `row()` and the renderer index
        // `rows[selected]` unconditionally, so an empty screen must fail loud
        // at construction — before raw mode — not as an index panic mid-frame
        // inside the alt screen. Unreachable in practice: the rows come from
        // the SETTINGS registry, which is non-empty and gate-tested.
        assert!(
            !rows.is_empty(),
            "the settings screen needs at least one row to select"
        );
        let theme_row = rows
            .iter()
            .position(|r| r.setting.choices == Choices::Themes);
        App {
            rows,
            selected: 0,
            mode: Mode::Browse,
            status: String::new(),
            theme_row,
            sync,
        }
    }

    fn row(&self) -> &Row {
        &self.rows[self.selected]
    }

    /// The theme the screen should be drawn in right now.
    ///
    /// While the picker is open over the theme row this is the candidate under
    /// the cursor, NOT the saved value — that is the live preview, and it is the
    /// reason this screen exists rather than a line in the manual telling people
    /// to edit `config.toml`.
    pub fn preview_theme(&self) -> Option<&str> {
        let i = self.theme_row?;
        if self.selected == i {
            if let Mode::Pick { cursor } = self.mode {
                return self.rows[i].choices.get(cursor).map(String::as_str);
            }
        }
        Some(self.rows[i].value.as_str())
    }

    /// The candidate list of the selected row (empty when it has none).
    fn candidates(&self) -> &[String] {
        &self.row().choices
    }

    /// Fold one key press into the state, returning what the caller must do.
    ///
    /// Pure: no terminal, no filesystem, no environment. Everything that needs
    /// the outside world comes back as an [`Action`].
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Windows sends a Release event for every Press. Without this filter
        // every keystroke is applied twice — Down skips a row, Enter opens the
        // picker and immediately commits. The filter lives here rather than in
        // the event loop so it is covered by a test instead of by a person on
        // Windows noticing the cursor jumping.
        if key.kind != KeyEventKind::Press {
            return None;
        }
        // Ctrl-C is an unconditional exit at every level, including mid-edit.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        match &self.mode {
            Mode::Browse => self.on_key_browse(key.code),
            Mode::Pick { cursor } => {
                let cursor = *cursor;
                self.on_key_pick(key.code, cursor)
            }
            Mode::SyncPick { .. } => self.on_key_sync_pick(key.code),
            Mode::SyncForm(_) => self.on_key_sync_form(key),
            Mode::SyncConfirmDisconnect => self.on_key_sync_confirm(key.code),
        }
    }

    fn on_key_browse(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            // #228.8: a save/cancel status ("saved tokens.enabled = false")
            // used to sit in the description's row for the rest of the
            // session, so moving to a different setting kept showing the OLD
            // one's status against the NEW one's key — actively misleading,
            // not just stale. The description is what makes this screen
            // better than `config list`; it must track the cursor.
            // #228.8: a save/cancel status ("saved tokens.enabled = false")
            // used to sit in the description's row for the rest of the
            // session, so moving to a different setting kept showing the OLD
            // one's status against the NEW one's key — actively misleading,
            // not just stale. The description is what makes this screen
            // better than `config list`; it must track the cursor.
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.status.clear();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                // saturating_sub on an empty rows vec would underflow; rows is
                // never empty in practice (SETTINGS is non-empty) but the
                // arithmetic must not depend on that.
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
                self.status.clear();
                None
            }
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Enter => self.begin_edit(),
            KeyCode::Char('c') => {
                self.status.clear();
                Some(Action::SyncListConnectors)
            }
            KeyCode::Char('s') => {
                if self.sync.connector.is_none() {
                    self.status = "sync is not set up; press c to connect".to_string();
                    None
                } else {
                    self.status = "syncing…".to_string();
                    Some(Action::SyncNow)
                }
            }
            KeyCode::Char('d') => {
                if self.sync.connector.is_none() {
                    self.status = "sync is not set up".to_string();
                    None
                } else {
                    self.mode = Mode::SyncConfirmDisconnect;
                    None
                }
            }
            _ => None,
        }
    }

    fn on_key_sync_pick(&mut self, code: KeyCode) -> Option<Action> {
        let Mode::SyncPick { names, cursor } = &mut self.mode else {
            return None;
        };
        let last = names.len().saturating_sub(1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                *cursor = cursor.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *cursor = (*cursor + 1).min(last);
                None
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
                self.status = "cancelled".to_string();
                None
            }
            KeyCode::Enter => {
                let name = names.get(*cursor)?.clone();
                Some(Action::SyncDescribe { name })
            }
            _ => None,
        }
    }

    /// Esc is handled before the form is even borrowed, so cancelling from
    /// any field never fights the borrow checker over `self.mode` — every
    /// other key needs the form itself.
    fn on_key_sync_form(&mut self, key: KeyEvent) -> Option<Action> {
        if key.code == KeyCode::Esc {
            self.mode = Mode::Browse;
            self.status = "cancelled".to_string();
            return None;
        }
        let Mode::SyncForm(form) = &mut self.mode else {
            return None;
        };
        let last = form.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                form.cursor = form.cursor.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Tab => {
                form.cursor = (form.cursor + 1).min(last);
                None
            }
            KeyCode::Backspace => {
                let i = form.cursor;
                form.value_mut(i).pop();
                None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let i = form.cursor;
                form.value_mut(i).push(c);
                None
            }
            KeyCode::Enter => {
                if form.cursor < last {
                    form.cursor += 1;
                    return None;
                }
                // The last field: submitting the whole form. Checked here,
                // before a single byte reaches a connector, exactly like the
                // terminal prompt's own two checks (`collect_passphrase`).
                if form.passphrase.is_empty() {
                    form.error =
                        Some("the sync passphrase must have at least one character".to_string());
                    return None;
                }
                if form.passphrase != form.passphrase_again {
                    form.error = Some("the two passphrases differ".to_string());
                    return None;
                }
                form.error = None;
                let values: BTreeMap<String, String> = form
                    .fields
                    .iter()
                    .map(|f| (f.key.clone(), f.value.clone()))
                    .collect();
                Some(Action::SyncSubmit {
                    name: form.connector.clone(),
                    values,
                    passphrase: form.passphrase.clone(),
                })
            }
            _ => None,
        }
    }

    fn on_key_sync_confirm(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(Action::SyncDisconnect),
            _ => {
                self.mode = Mode::Browse;
                self.status = "cancelled".to_string();
                None
            }
        }
    }

    /// The connector names found on PATH, or the explanation why there are
    /// none — said explicitly rather than opening an empty picker a user
    /// could not act on.
    pub fn sync_names_found(&mut self, names: Vec<String>) {
        if names.is_empty() {
            self.status =
                "no tasqx-remote-* connector found on PATH (install one first)".to_string();
            return;
        }
        self.mode = Mode::SyncPick { names, cursor: 0 };
    }

    /// The chosen connector's own fields, ready for the form.
    pub fn sync_form_ready(&mut self, name: String, fields: Vec<remote::Field>) {
        self.mode = Mode::SyncForm(SyncForm::new(name, fields));
    }

    /// `describe` itself failed (not found, refused, timed out): back to
    /// Browse, the same as any other failed action on this screen.
    pub fn sync_describe_failed(&mut self, message: String) {
        self.mode = Mode::Browse;
        self.status = format!("not connected: {message}");
    }

    /// The connector accepted the form and the write landed: the new Sync
    /// section replaces the old one and the form closes.
    pub fn sync_setup_ok(&mut self, info: SyncInfo) {
        let name = info.connector.clone().unwrap_or_default();
        self.sync = info;
        self.mode = Mode::Browse;
        self.status = format!("connected to {name}");
    }

    /// The connector's own `ok: false`, or a transport failure reaching it:
    /// shown INLINE, with the form kept open and everything already typed
    /// still in it — the one thing this screen must not do is throw a
    /// half-filled form away over a rejection the user can probably fix.
    pub fn sync_setup_failed(&mut self, message: String) {
        if let Mode::SyncForm(form) = &mut self.mode {
            form.error = Some(message);
        }
    }

    /// `tasqx sync` ran and something happened either way: the Sync section
    /// is refreshed from what actually landed, and the one-line result sits
    /// in the same footer status every other action on this screen reports
    /// through.
    pub fn sync_now_done(&mut self, info: SyncInfo, one_line: String) {
        self.sync = info;
        self.status = one_line;
    }

    /// `tasqx sync` itself failed: the Sync section is untouched (nothing
    /// landed), and the failure is the status.
    pub fn sync_now_failed(&mut self, message: String) {
        self.status = format!("sync failed: {message}");
    }

    /// Both files are gone (or never existed) and the Sync section reports
    /// "not set up" again, the same as a store that was never connected.
    pub fn sync_disconnected(&mut self) {
        self.sync = SyncInfo::default();
        self.mode = Mode::Browse;
        self.status = "disconnected (the remote itself is untouched)".to_string();
    }

    /// Enter on the selected row: toggle, open a picker, or explain why not.
    fn begin_edit(&mut self) -> Option<Action> {
        let s = self.row().setting;
        // A store-homed setting is shown because leaving it out would make the
        // screen disagree with `config list` about how many settings exist. It
        // is not editable here, and the explanation is the registry's one
        // wording, so it matches what `config set` says.
        if s.home == Home::Store {
            self.status = config::store_home_message(s);
            return None;
        }
        if s.kind == Kind::Bool {
            let next = if self.row().value == "true" {
                "false"
            } else {
                "true"
            };
            self.rows[self.selected].value = next.to_string();
            return Some(Action::Save {
                key: s.key,
                value: next.to_string(),
            });
        }
        if !self.candidates().is_empty() {
            // Open on the current value so the first thing the user sees is
            // where they already are, not the top of an unrelated list.
            let cursor = self
                .candidates()
                .iter()
                .position(|c| *c == self.row().value)
                .unwrap_or(0);
            self.mode = Mode::Pick { cursor };
            self.status.clear();
            return None;
        }
        // A row with no closed value set and nothing to toggle. `otlp.port`
        // (Toml + Uint + Free) and `daemon.idle_timeout` (Toml + Minutes + Free)
        // land here: reachability turns on an empty `choices` list, not on the
        // kind, which is what the earlier "`default_project` is the only
        // Str + Free entry" reasoning missed.
        // Say where to set it instead of doing nothing, because a silent no-op
        // on Enter reads as a broken screen.
        self.status = format!("no inline editor for {} — use `tasqx config set`", s.key);
        None
    }

    fn on_key_pick(&mut self, code: KeyCode, cursor: usize) -> Option<Action> {
        let last = self.candidates().len().saturating_sub(1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Pick {
                    cursor: cursor.saturating_sub(1),
                };
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::Pick {
                    cursor: (cursor + 1).min(last),
                };
                None
            }
            // Esc and q close the PICKER, not the app. Quitting outright from a
            // picker would throw away a deliberate navigation and, worse, leave
            // the user unsure whether the theme they were previewing got saved.
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
                self.status = "cancelled".to_string();
                None
            }
            KeyCode::Enter => {
                let value = self.candidates().get(cursor)?.clone();
                self.mode = Mode::Browse;
                self.rows[self.selected].value = value.clone();
                Some(Action::Save {
                    key: self.row().setting.key,
                    value,
                })
            }
            _ => None,
        }
    }

    /// Report the re-resolved value after the caller performed a `Save`.
    ///
    /// The caller re-runs `config::resolve`, so a `TASQX_THEME` that still
    /// outranks `config.toml` is reported honestly instead of the screen
    /// claiming a write took effect that the user will not see on their next
    /// command.
    pub fn refresh(&mut self, key: &str, value: String, source: String) {
        let Some(row) = self.rows.iter_mut().find(|r| r.setting.key == key) else {
            return;
        };
        row.value = value.clone();
        row.source = source.clone();
        self.status = if source == "config.toml" {
            format!("saved {key} = {value}")
        } else {
            format!("saved {key} = {value}, but {source} still wins")
        };
    }

    /// Report a failed `Save`. The write is the one part that can fail for
    /// reasons the state machine cannot see (an unparseable `config.toml`, a
    /// read-only directory), and swallowing it would leave the screen showing a
    /// value that is not on disk.
    pub fn report_error(&mut self, message: String) {
        self.status = format!("not saved: {message}");
    }
}

// ============================================================================
// Rendering
// ============================================================================

/// Draw the whole screen. Decides nothing: every choice was already made in
/// `App`, and every colour comes from `theme` at `caps`' real depth.
pub fn render(app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let sty = |role: &str| rt_style(theme.role(role), caps);
    let area = frame.area();
    // The footer gets five lines, not two: `store_home_message` is ~130
    // characters and the summaries are not much shorter, so a single-line
    // detail area truncated the one sentence that tells the user what to do
    // instead — "set it with `tas" is worse than no message at all. The fifth
    // is the Sync section's own one-line status (#884): a fixed line rather
    // than a row in the registry list, because it is not a registered
    // setting and its three keys (`c`/`s`/`d`) act outside the row picker's
    // Enter/toggle vocabulary.
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(5),
    ])
    .areas(area);
    let [detail_area, sync_area, help_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(foot);

    let marker = if caps.unicode { "▸" } else { ">" };
    let rule = if caps.unicode { "─" } else { "-" };

    // --- header --------------------------------------------------------------
    let title = Line::from(vec![
        Span::styled("tasqx settings", sty("header")),
        Span::raw("   "),
        Span::styled(
            format!("theme: {}", app.preview_theme().unwrap_or("-")),
            sty("accent"),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(vec![
            title,
            Line::styled(rule.repeat(area.width as usize), sty("muted")),
        ]),
        head,
    );

    // --- rows ----------------------------------------------------------------
    // `anchor` is the index, among the lines below, of the one carrying the
    // marker — the row in Browse, the candidate under the picker cursor in
    // Pick. It is tracked in LINE space rather than row space because the rows
    // are not uniform height: an open picker inserts one line per candidate
    // under a single row, so a window computed over `app.selected` would put
    // the marker back off screen the moment the picker opened.
    // Measured from the rows about to be drawn, plus two cells of separator.
    // Both columns used to be `format!("{:<18}")` / `{:<22}`, which is the exact
    // pair of mistakes `render::pad` exists to end: a MINIMUM width with no gap
    // after it, padded by CHAR COUNT rather than display width. An 18-character
    // key therefore butted straight against its value — `detail.time_formatboth`
    // — and a value holding any wide or combining character mis-aligned every
    // column to its right.
    //
    // The value column keeps a cap. `dashboard.panels` resolves to a
    // 50-character list, and fitting the column to it would push `source` off
    // the right edge of EVERY row rather than just its own: pick's
    // invisible-field failure, reached through the widest value instead of the
    // widest title. `pad` never truncates, so that one row still overflows its
    // own line and ratatui clips it there.
    match &app.mode {
        Mode::SyncPick { names, cursor } => {
            render_sync_pick(names, *cursor, &sty, caps, body, frame)
        }
        Mode::SyncForm(form) => render_sync_form(form, &sty, caps, body, frame),
        Mode::SyncConfirmDisconnect => render_sync_confirm(&app.sync, &sty, body, frame),
        Mode::Browse | Mode::Pick { .. } => {
            let key_w = app
                .rows
                .iter()
                .map(|r| render::width(r.setting.key))
                .max()
                .unwrap_or(0)
                + 2;
            let mut lines: Vec<Line> = Vec::new();
            let mut anchor = 0usize;
            for (i, row) in app.rows.iter().enumerate() {
                let selected = i == app.selected;
                if selected {
                    anchor = lines.len();
                }
                let shown = if row.value.is_empty() {
                    "(unset)"
                } else {
                    row.value.as_str()
                };
                // A store-homed row, or one with nothing `begin_edit` can do with
                // Enter (no bool to toggle, no closed choice set to pick from — the
                // free-form Toml scalars like `otlp.port`), is dimmed so "shown but
                // not editable here" reads before the user presses Enter on it, not
                // only after (the muted `Line` returned by `begin_edit` on that press).
                let has_inline_editor = row.setting.home != Home::Store
                    && (row.setting.kind == Kind::Bool || !row.choices.is_empty());
                let value_style = if !has_inline_editor {
                    sty("muted")
                } else if selected {
                    sty("accent")
                } else {
                    ratatui::style::Style::default()
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        if selected {
                            format!("{marker} ")
                        } else {
                            "  ".to_string()
                        },
                        sty("accent"),
                    ),
                    Span::styled(render::pad(row.setting.key, key_w), sty("project")),
                    Span::styled(render::pad(shown, 22), value_style),
                    Span::styled(row.source.clone(), sty("muted")),
                ]));

                // The inline picker sits directly under its own row, so the value being
                // previewed and the list it came from are never separated on screen.
                if selected {
                    if let Mode::Pick { cursor } = &app.mode {
                        for (j, cand) in row.choices.iter().enumerate() {
                            let at = j == *cursor;
                            if at {
                                anchor = lines.len();
                            }
                            lines.push(Line::from(vec![
                                Span::raw("      "),
                                Span::styled(
                                    if at {
                                        format!("{marker} ")
                                    } else {
                                        "  ".to_string()
                                    },
                                    sty("warn"),
                                ),
                                Span::styled(
                                    cand.clone(),
                                    if at { sty("warn") } else { sty("muted") },
                                ),
                            ]));
                        }
                    }
                }
            }
            let height = body.height as usize;
            let start = first_visible(anchor, lines.len(), height);
            let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
            frame.render_widget(Paragraph::new(visible), body);
        }
    }

    // --- footer --------------------------------------------------------------
    let help = match &app.mode {
        Mode::Browse => "up/down move   enter edit   c/s/d sync   esc quit",
        Mode::Pick { .. } => "up/down preview   enter save   esc cancel",
        Mode::SyncPick { .. } => "up/down move   enter choose   esc cancel",
        Mode::SyncForm(_) => "tab/down next field   up previous   enter next / submit   esc cancel",
        Mode::SyncConfirmDisconnect => "y disconnect   any other key cancels",
    };
    let detail = match &app.mode {
        Mode::SyncForm(form) if app.status.is_empty() => Line::styled(
            form.error.clone().unwrap_or_else(|| sync_field_help(form)),
            sty(if form.error.is_some() {
                "warn"
            } else {
                "muted"
            }),
        ),
        _ if app.status.is_empty() => {
            Line::styled(app.rows[app.selected].setting.summary, sty("muted"))
        }
        _ => Line::styled(app.status.clone(), sty("warn")),
    };
    frame.render_widget(
        Paragraph::new(detail).wrap(ratatui::widgets::Wrap { trim: true }),
        detail_area,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(sync_status_line(&app.sync), sty("muted"))),
        sync_area,
    );
    frame.render_widget(Paragraph::new(Line::styled(help, sty("muted"))), help_area);
}

/// The Sync section's one line: the connector, its short version, when this
/// store last synced through it, and whether it is encrypted — the same
/// facts `tasqx sync --status` reports, kept to one line here because it sits
/// permanently in the footer rather than filling the screen.
fn sync_status_line(sync: &SyncInfo) -> String {
    let Some(name) = &sync.connector else {
        return "sync: not set up — c to connect".to_string();
    };
    let mut line = format!("sync: {name}");
    if let Some(v) = &sync.version {
        line.push_str(&format!(", v{v}"));
    }
    match &sync.synced_relative {
        Some(t) => line.push_str(&format!(", synced {t}")),
        None => line.push_str(", never synced"),
    }
    if !sync.encrypted {
        line.push_str(" — no passphrase set, run c again");
    }
    line
}

/// The focused field's help text, and where to read more, as one line —
/// exactly what the terminal prompt (`sync setup`'s own `ask`) prints ahead
/// of the label, just drawn in the footer instead of above the input.
fn sync_field_help(form: &SyncForm) -> String {
    let Some(f) = form.fields.get(form.cursor) else {
        return "the passphrase encrypts every snapshot before it leaves this machine".to_string();
    };
    match &f.help_url {
        Some(url) => format!("{}  ({url})", f.help),
        None => f.help.clone(),
    }
}

fn render_sync_pick(
    names: &[String],
    cursor: usize,
    sty: &dyn Fn(&str) -> Style,
    caps: &Caps,
    area: Rect,
    frame: &mut Frame,
) {
    let marker = if caps.unicode { "▸" } else { ">" };
    let mut lines = vec![
        Line::styled("Connect: choose a connector found on PATH", sty("header")),
        Line::raw(""),
    ];
    for (i, name) in names.iter().enumerate() {
        let here = i == cursor;
        lines.push(Line::from(vec![
            Span::styled(
                if here {
                    format!("{marker} ")
                } else {
                    "  ".to_string()
                },
                sty("accent"),
            ),
            Span::styled(
                name.clone(),
                if here {
                    sty("accent")
                } else {
                    Style::default()
                },
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_sync_form(
    form: &SyncForm,
    sty: &dyn Fn(&str) -> Style,
    caps: &Caps,
    area: Rect,
    frame: &mut Frame,
) {
    let marker = if caps.unicode { "▸" } else { ">" };
    let mask = if caps.unicode { "•" } else { "*" };
    let mut lines = vec![Line::styled(
        format!("Connect: {}", form.connector),
        sty("header"),
    )];
    lines.push(Line::raw(""));
    for i in 0..form.len() {
        let here = i == form.cursor;
        let shown = if form.is_secret(i) {
            mask.repeat(form.value(i).chars().count())
        } else {
            form.value(i).to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(
                if here {
                    format!("{marker} ")
                } else {
                    "  ".to_string()
                },
                sty("accent"),
            ),
            Span::styled(format!("{}: ", form.label(i)), sty("project")),
            Span::styled(
                shown,
                if here {
                    sty("accent")
                } else {
                    Style::default()
                },
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

fn render_sync_confirm(
    sync: &SyncInfo,
    sty: &dyn Fn(&str) -> Style,
    area: Rect,
    frame: &mut Frame,
) {
    let name = sync.connector.as_deref().unwrap_or("");
    let lines = vec![
        Line::styled(format!("Disconnect from {name}?"), sty("header")),
        Line::raw(""),
        Line::styled(
            "This removes this store's sync state and passphrase. The remote itself is \
             untouched.",
            sty("muted"),
        ),
        Line::raw(""),
        Line::styled("y disconnect   any other key cancels", sty("accent")),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    use crate::theme::{self, ColorDepth};

    fn caps() -> Caps {
        Caps {
            depth: ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        }
    }

    /// Rows built the way the real command builds them: straight out of the
    /// registry, with the theme candidates supplied from outside.
    fn app() -> App {
        let rows = config::SETTINGS
            .iter()
            .map(|s| Row {
                setting: s,
                // Every row starts at its DEFAULT, which is what `resolve`
                // returns for a store with no config file — the state this
                // screen is most often opened in. Naming two settings and
                // leaving the rest empty made the fixture disagree with the
                // real screen for every setting added afterwards, and left a
                // Bool row showing `""`, which the screen cannot toggle.
                value: s.default.to_string(),
                source: match s.home {
                    Home::Store => "store".to_string(),
                    Home::Toml => "default".to_string(),
                },
                choices: match s.choices {
                    Choices::Themes => theme::BUILTINS.iter().map(|t| t.to_string()).collect(),
                    Choices::Free | Choices::ManyOf(_) => Vec::new(),
                    Choices::OneOf(values) => values.iter().map(|v| (*v).to_string()).collect(),
                },
            })
            .collect();
        App::new(rows, SyncInfo::default())
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn draw(app: &App) -> Buffer {
        let name = app.preview_theme().unwrap_or("nord");
        // `None` for the themes dir: the built-ins need no files, and reading
        // the user's real theme directory would make this test depend on the
        // machine it runs on.
        let th = theme::load(name, None);
        // Tall enough for the whole registry plus the chrome (2 header, 4
        // footer). A fixed 16 made this helper fail the day a tenth setting was
        // added — which says nothing about the screen, and hides what the tests
        // using it actually assert. Every test using this helper is therefore
        // asserting about an UNSCROLLED screen; the two that pin the window
        // itself call `draw_at` with a deliberately short terminal instead.
        let rows = config::SETTINGS.len() as u16 + 8;
        let mut term = Terminal::new(TestBackend::new(70, rows)).unwrap();
        term.draw(|f| render(app, &th, &caps(), f)).unwrap();
        term.backend().buffer().clone()
    }

    /// The same screen on a terminal of a stated size — the sizes `draw` was
    /// built to avoid.
    fn draw_at(app: &App, w: u16, h: u16) -> Buffer {
        let th = theme::load(app.preview_theme().unwrap_or("nord"), None);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(app, &th, &caps(), f)).unwrap();
        term.backend().buffer().clone()
    }

    fn line_at(buf: &Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn all_text(buf: &Buffer) -> String {
        (0..buf.area().height)
            .map(|y| line_at(buf, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row_of(buf: &Buffer, needle: &str) -> u16 {
        (0..buf.area().height)
            .find(|y| line_at(buf, *y).contains(needle))
            .unwrap_or_else(|| panic!("no line contains {needle:?} in:\n{}", all_text(buf)))
    }

    // ---- state machine ------------------------------------------------------

    /// Up/Down must clamp, not wrap and not underflow. `selected - 1` at the top
    /// row is an integer underflow on a usize, which panics in debug and indexes
    /// out of bounds in release — inside a raw-mode alt screen, where the panic
    /// message is the last thing the user can read.
    #[test]
    fn the_selection_clamps_at_both_ends() {
        let mut a = app();
        assert_eq!(a.selected, 0);
        assert!(a.on_key(press(KeyCode::Up)).is_none());
        assert_eq!(a.selected, 0, "up at the top row must stay put");
        for _ in 0..a.rows.len() + 4 {
            a.on_key(press(KeyCode::Down));
        }
        assert_eq!(
            a.selected,
            a.rows.len() - 1,
            "down past the end must stay on the last row"
        );
    }

    /// Windows crossterm emits Press AND Release for every keystroke. Without
    /// the kind filter each press is applied twice: Down skips a row and Enter
    /// opens the picker and instantly commits whatever it landed on. Nothing
    /// about that is visible on Linux CI, which is exactly why it is pinned.
    #[test]
    fn a_key_release_is_not_a_second_key_press() {
        let mut a = app();
        let mut release = press(KeyCode::Down);
        release.kind = KeyEventKind::Release;
        assert!(a.on_key(release).is_none());
        assert_eq!(a.selected, 0, "a Release event moved the selection");

        a.on_key(press(KeyCode::Down));
        assert_eq!(a.selected, 1, "the Press that follows still counts");
    }

    /// A Bool toggles in place and asks the caller to persist it. The value the
    /// screen shows and the value handed to `write_value` must be the same one —
    /// showing `true` while saving `false` is the worst failure this screen has.
    #[test]
    fn enter_on_a_bool_toggles_and_asks_for_exactly_that_value_to_be_saved() {
        let mut a = app();
        a.selected = a
            .rows
            .iter()
            .position(|r| r.setting.kind == Kind::Bool)
            .unwrap();
        // Read the starting value off the row rather than assuming which Bool
        // sorts first or what its default is. The property under test is that
        // the shown value and the saved value agree; pinning a literal here made
        // this test fail the day a Bool was added above it, which taught nobody
        // anything about the screen.
        let before = a.rows[a.selected].value.clone();
        assert!(
            before == "true" || before == "false",
            "a Bool row must start at a real boolean, got {before:?}"
        );
        let expected = if before == "true" { "false" } else { "true" };

        let key = a.rows[a.selected].setting.key;

        let act = a
            .on_key(press(KeyCode::Enter))
            .expect("enter must produce a save");
        assert_eq!(
            act,
            Action::Save {
                key,
                value: expected.into()
            }
        );
        assert_eq!(
            a.rows[a.selected].value, expected,
            "the screen must show what it saved"
        );

        let act = a
            .on_key(press(KeyCode::Enter))
            .expect("a second enter toggles back");
        assert_eq!(
            act,
            Action::Save {
                key,
                value: before.clone()
            }
        );
    }

    /// The store-homed row is visible but not editable, and pressing Enter must
    /// say where to go instead. It reuses the registry's wording, so a user who
    /// tried `config set default_project` and then tried the screen gets one
    /// answer rather than two. Nothing is saved.
    #[test]
    fn enter_on_the_store_homed_row_explains_instead_of_writing() {
        let mut a = app();
        a.selected = a
            .rows
            .iter()
            .position(|r| r.setting.home == Home::Store)
            .unwrap();

        let act = a.on_key(press(KeyCode::Enter));
        assert!(
            act.is_none(),
            "a store-homed setting must never produce a Save: {act:?}"
        );
        let s = config::find("default_project").unwrap();
        assert_eq!(a.status, config::store_home_message(s));
        assert!(a.status.contains("tasqx use"), "{}", a.status);
    }

    /// #228.8: a save's status message used to sit in the description row
    /// for the rest of the session — move to a different setting and the OLD
    /// status ("saved tokens.enabled = false") stayed put next to the NEW
    /// key, which is actively misleading, not just stale. It must clear the
    /// moment the cursor moves.
    #[test]
    fn moving_the_cursor_after_a_save_clears_the_stale_status() {
        let mut a = app();
        let bool_row = a
            .rows
            .iter()
            .position(|r| r.setting.kind == Kind::Bool)
            .expect("at least one bool setting");
        a.selected = bool_row;
        let key = a.row().setting.key;
        let Some(Action::Save { value, .. }) = a.on_key(press(KeyCode::Enter)) else {
            panic!("a bool row's Enter must produce a Save");
        };
        // The screen's own driving loop calls this once the caller's write
        // to `config.toml` has actually happened; simulated here the same way.
        a.refresh(key, value.clone(), "config.toml".to_string());
        assert!(
            !a.status.is_empty(),
            "the toggle must leave a status to begin with"
        );

        a.on_key(press(KeyCode::Down));
        assert!(
            a.status.is_empty(),
            "the stale save status must not survive a cursor move: {}",
            a.status
        );

        // And the same for the other direction.
        a.selected = bool_row;
        a.on_key(press(KeyCode::Enter));
        a.refresh(key, value, "config.toml".to_string());
        assert!(!a.status.is_empty());
        a.on_key(press(KeyCode::Up));
        assert!(
            a.status.is_empty(),
            "Up must clear it too, not only Down: {}",
            a.status
        );
    }

    /// `otlp.port` and `daemon.idle_timeout` are `Toml`-homed with an empty
    /// candidate list — no bool to toggle, no picker to open — so pressing
    /// Enter on either only reports "no inline editor for … — use `tasqx
    /// config set`" AFTER the keystroke. Before that, they were drawn exactly
    /// like an editable numeric row, so the only way to learn which quarter of
    /// the registry declines Enter was to press it on every row.
    ///
    /// The store-homed row already gets this treatment (previous test); a row
    /// with no closed choice set and nothing to toggle must get it too.
    #[test]
    fn a_row_with_no_inline_editor_is_dimmed_before_enter_is_pressed() {
        let a = app();
        let i = a
            .rows
            .iter()
            .position(|r| r.setting.key == "otlp.port")
            .expect("otlp.port is in the registry");
        assert!(
            a.rows[i].choices.is_empty() && a.rows[i].setting.kind != Kind::Bool,
            "otlp.port must be the no-picker, no-toggle case this test means to cover"
        );

        let th = theme::load("nord", None);
        let buf = draw(&a);
        let y = row_of(&buf, "otlp.port");
        let line = line_at(&buf, y);
        let value_x =
            line.find(a.rows[i].value.as_str())
                .unwrap_or_else(|| panic!("value not on its own row: {line:?}")) as u16;

        let muted_fg = rt_style(th.role("muted"), &caps()).fg.unwrap();
        assert_eq!(
            buf[(value_x, y)].fg,
            muted_fg,
            "a setting with no inline editor must be dimmed before Enter is ever pressed: {line:?}"
        );
    }

    /// The picker opens on the value already in force, moves within its bounds,
    /// and commits the candidate under the cursor. Opening at index 0 instead
    /// would show a user on `mono` a cursor sitting on `nord`, one Enter away
    /// from silently changing their theme.
    #[test]
    fn the_picker_opens_on_the_current_value_and_commits_the_one_under_the_cursor() {
        let mut a = app();
        a.rows[0].value = "dracula".to_string();
        let expected = theme::BUILTINS
            .iter()
            .position(|t| *t == "dracula")
            .unwrap();

        assert!(
            a.on_key(press(KeyCode::Enter)).is_none(),
            "opening a picker saves nothing"
        );
        assert_eq!(a.mode, Mode::Pick { cursor: expected });

        a.on_key(press(KeyCode::Down));
        let at = match a.mode {
            Mode::Pick { cursor } => cursor,
            m => panic!("left the picker: {m:?}"),
        };
        let act = a.on_key(press(KeyCode::Enter)).expect("enter commits");
        assert_eq!(
            act,
            Action::Save {
                key: "theme.name",
                value: theme::BUILTINS[at].to_string()
            }
        );
        assert_eq!(a.mode, Mode::Browse, "committing closes the picker");
    }

    /// THE point of this screen. While the picker moves, `preview_theme` must
    /// report the candidate under the cursor rather than the saved value — that
    /// is what lets the caller reload the theme and repaint before anything is
    /// written. Cancelling must put the preview back, or a user who escaped out
    /// is left looking at a theme they rejected.
    #[test]
    fn moving_the_picker_previews_the_candidate_without_saving_it() {
        let mut a = app();
        assert_eq!(a.preview_theme(), Some("nord"));

        a.on_key(press(KeyCode::Enter));
        a.on_key(press(KeyCode::Down));
        assert_eq!(
            a.preview_theme(),
            Some(theme::BUILTINS[1]),
            "preview must follow the cursor"
        );
        assert_eq!(
            a.rows[0].value, "nord",
            "previewing must not change the stored value"
        );

        assert!(
            a.on_key(press(KeyCode::Esc)).is_none(),
            "esc in a picker must not quit the app"
        );
        assert_eq!(a.mode, Mode::Browse);
        assert_eq!(
            a.preview_theme(),
            Some("nord"),
            "cancelling must restore the preview"
        );
    }

    /// Esc/q quit from Browse but only close the picker from Pick. A `q` that
    /// quit mid-pick would drop the user back to their shell with no idea
    /// whether the theme they were looking at had been written.
    #[test]
    fn quit_keys_mean_different_things_inside_and_outside_the_picker() {
        for code in [KeyCode::Esc, KeyCode::Char('q')] {
            let mut a = app();
            assert_eq!(
                a.on_key(press(code)),
                Some(Action::Quit),
                "{code:?} must quit from browse"
            );

            let mut b = app();
            b.on_key(press(KeyCode::Enter));
            assert!(
                b.on_key(press(code)).is_none(),
                "{code:?} must not quit from a picker"
            );
            assert_eq!(b.mode, Mode::Browse);
        }
    }

    /// Ctrl-C has to work from inside the picker too. A modal state that traps
    /// the conventional interrupt is how a TUI ends up being killed from another
    /// window — with the terminal still in raw mode.
    #[test]
    fn ctrl_c_quits_from_every_mode() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut a = app();
        assert_eq!(a.on_key(ctrl_c), Some(Action::Quit));

        let mut b = app();
        b.on_key(press(KeyCode::Enter));
        assert_eq!(b.mode, Mode::Pick { cursor: 0 });
        assert_eq!(
            b.on_key(ctrl_c),
            Some(Action::Quit),
            "ctrl-c must escape the picker too"
        );
    }

    /// A write that lands in `config.toml` while `$TASQX_THEME` still outranks
    /// it changes nothing the user will see on their next command. Reporting a
    /// bare "saved" there is a lie the screen is uniquely placed to catch,
    /// because it is the only surface that re-resolves right after writing.
    #[test]
    fn a_save_shadowed_by_a_higher_layer_is_reported_as_shadowed() {
        let mut a = app();
        a.refresh("theme.name", "mono".into(), "config.toml".into());
        assert_eq!(a.status, "saved theme.name = mono");
        assert_eq!(a.rows[0].value, "mono");

        a.refresh("theme.name", "gruvbox".into(), "$TASQX_THEME".into());
        assert!(
            a.status.contains("still wins"),
            "shadowing not reported: {}",
            a.status
        );
        assert!(a.status.contains("$TASQX_THEME"), "{}", a.status);
    }

    /// A failed write must be visible. The screen has already optimistically
    /// flipped the row, so silence would leave it showing a value that is not on
    /// disk and will vanish on the next run.
    #[test]
    fn a_failed_write_is_shown_rather_than_swallowed() {
        let mut a = app();
        a.report_error("config.toml is not valid TOML".into());
        assert!(a.status.starts_with("not saved:"), "{}", a.status);
        assert!(a.status.contains("not valid TOML"), "{}", a.status);
    }

    // ---- rendering ----------------------------------------------------------

    /// Every registered setting must appear. The screen is driven by
    /// `config::SETTINGS`, so a fourth setting shows up here for free — but a
    /// layout that clipped it, or a filter that skipped the store-homed row,
    /// would make the screen quietly disagree with `config list` about what
    /// tasqx can be configured to do.
    #[test]
    fn every_registered_setting_is_drawn_with_its_value_and_source() {
        let buf = draw(&app());
        let text = all_text(&buf);
        for s in config::SETTINGS {
            assert!(
                text.contains(s.key),
                "{} missing from the screen:\n{text}",
                s.key
            );
        }
        assert!(text.contains("nord"), "{text}");
        assert!(text.contains("false"), "{text}");
        assert!(
            text.contains("(unset)"),
            "an empty value must read as unset:\n{text}"
        );
        assert!(
            text.contains("store"),
            "the store-homed row must name its home:\n{text}"
        );
        assert!(
            text.contains("esc quit"),
            "the key hints must be on screen:\n{text}"
        );
    }

    /// Every key is separated from its value by whitespace.
    ///
    /// The key column was a literal `{:<18}`, which is a MINIMUM width with no
    /// separator after it, so an 18-character key butted straight against its
    /// value: `detail.time_formatboth` and `daemon.idle_timeout0` were both on
    /// screen at every terminal size. The test the screen already had passed
    /// throughout, because `contains(s.key)` is satisfied by a key with the
    /// value glued to it — the value is what got lost, not the key.
    #[test]
    fn every_key_is_separated_from_its_value() {
        let buf = draw(&app());
        for s in config::SETTINGS {
            let line = line_at(&buf, row_of(&buf, s.key));
            let after = line
                .split_once(s.key)
                .expect("row_of found the key on this line")
                .1;
            assert!(
                after.starts_with(' '),
                "{} runs straight into its value:\n{line}",
                s.key
            );
        }
    }

    /// And the column is derived from the registry rather than assumed.
    ///
    /// A wider fixed number would fix today's two keys and break again on the
    /// next long one, with nothing to warn anybody: the fixed 18 was wide
    /// enough for every key that existed when it was written, too. The row here
    /// carries a key longer than anything in the registry, so it can only pass
    /// if the width is measured.
    #[test]
    fn the_key_column_is_measured_not_assumed() {
        static LONG: Setting = Setting {
            key: "a.deliberately.very.long.setting.key",
            home: Home::Toml,
            kind: Kind::Bool,
            default: "false",
            env: None,
            flag: None,
            choices: Choices::Free,
            summary: "Only ever drawn by this test.",
        };
        let mut a = app();
        a.rows.push(Row {
            setting: &LONG,
            value: "false".to_string(),
            source: "default".to_string(),
            choices: Vec::new(),
        });

        let buf = draw_at(&a, 100, config::SETTINGS.len() as u16 + 9);
        for key in config::SETTINGS
            .iter()
            .map(|s| s.key)
            .chain(std::iter::once(LONG.key))
        {
            let line = line_at(&buf, row_of(&buf, key));
            let after = line.split_once(key).expect("found on this line").1;
            assert!(
                after.starts_with(' '),
                "{key} runs into its value once a longer key exists:\n{line}"
            );
        }
    }

    /// Columns are measured in CELLS, not in `char`s.
    ///
    /// `format!("{v:<22}")` counts chars, so a value of six CJK characters —
    /// twelve cells — was padded as though it were six, and every column to its
    /// right on that row alone sat six cells further across than on its
    /// neighbours. `render::pad` exists precisely to end this, and its own doc
    /// names the macro as the bug; this screen was still using it.
    #[test]
    fn a_double_width_value_does_not_shift_the_columns_after_it() {
        static WIDE: Setting = Setting {
            key: "wide.value",
            home: Home::Toml,
            kind: Kind::Bool,
            default: "false",
            env: None,
            flag: None,
            choices: Choices::Free,
            summary: "Only ever drawn by this test.",
        };
        let mut a = app();
        a.rows.push(Row {
            setting: &WIDE,
            // Six characters, twelve cells.
            value: "日本語テスト".to_string(),
            source: "default".to_string(),
            choices: Vec::new(),
        });

        let buf = draw_at(&a, 100, config::SETTINGS.len() as u16 + 9);
        // The source column is last, so its start is where every earlier column
        // has finished. `rfind`, because a VALUE may also read "default".
        let source_x = |key: &str| {
            let line = line_at(&buf, row_of(&buf, key));
            // `line_at` returns ONE char per screen CELL — a double-width glyph
            // is its own char plus the continuation cell's space — so the column
            // is a char count here. `render::width` would count that glyph as
            // two and report a shift the screen does not have, which is how
            // this assertion first failed against a render that was correct.
            line.rfind("default")
                .map(|b| line[..b].chars().count())
                .unwrap_or_else(|| panic!("no source column on the {key} row:\n{line}"))
        };
        let plain = source_x("theme.name");
        assert_eq!(
            source_x(WIDE.key),
            plain,
            "a double-width value moved the source column:\n{}",
            all_text(&buf)
        );
    }

    /// The selection marker is the only thing telling the user which row Enter
    /// will act on. A marker that did not move — or moved on the wrong row —
    /// makes every subsequent keystroke a guess.
    #[test]
    fn the_selection_marker_sits_on_the_selected_row() {
        let mut a = app();
        // The first two rows by name, not by guess: naming a specific setting
        // as "the second row" made this test fail the day one was added above
        // it, which says nothing about the marker.
        let first = a.rows[0].setting.key;
        let second = a.rows[1].setting.key;

        let before = draw(&a);
        assert!(line_at(&before, row_of(&before, first)).starts_with('▸'));
        assert!(!line_at(&before, row_of(&before, second)).starts_with('▸'));

        a.on_key(press(KeyCode::Down));
        let after = draw(&a);
        assert!(line_at(&after, row_of(&after, second)).starts_with('▸'));
        assert!(!line_at(&after, row_of(&after, first)).starts_with('▸'));
    }

    /// Every row is reachable on a terminal too short to hold the registry.
    ///
    /// This screen pushed all its rows into one `Paragraph` starting at index 0
    /// while [`App::step`] clamped the selection to the number of SETTINGS, not
    /// to what fits. Past the last visible row nothing on screen was marked at
    /// all, and Enter then edited a setting the user had never seen — the same
    /// failure `pick` shipped once and D55 fixed there with `first_visible`.
    ///
    /// Driven through the REAL render at every selection, because the pure
    /// window function can be right while `render` ignores it. That is exactly
    /// the shape the bug took in both screens.
    #[test]
    fn a_registry_taller_than_the_terminal_scrolls_the_marker_into_view() {
        // 6 rows of chrome (2 header, 4 footer) leave 6 body rows here, so half
        // the registry is below the fold at this size.
        let (w, h) = (70, 12);
        for n in 0..config::SETTINGS.len() {
            let mut a = app();
            for _ in 0..n {
                a.on_key(press(KeyCode::Down));
            }
            let key = a.rows[a.selected].setting.key;
            let text = all_text(&draw_at(&a, w, h));
            assert!(
                text.lines().any(|l| l.starts_with('▸') && l.contains(key)),
                "after {n} Down the marker on {key} was not drawn at {w}x{h}:\n{text}"
            );
        }
    }

    /// And the candidate under the picker's own cursor, which is a second list
    /// nested inside the first.
    ///
    /// The rows are not uniform height: an open picker inserts a line per
    /// candidate under one row, so a window computed over row INDICES would put
    /// the marker back off screen the moment the picker opened. The window is
    /// therefore computed over drawn LINES, and this is what says so.
    #[test]
    fn an_open_picker_keeps_its_own_cursor_on_screen_too() {
        let (w, h) = (70, 12);
        let mut a = app();
        // The LAST row that has candidates, found rather than named: a picker
        // opening at the top of the registry fits on any terminal and proves
        // nothing. This one opens below the fold and then extends further down.
        let deep = a
            .rows
            .iter()
            .rposition(|r| !r.choices.is_empty())
            .expect("the registry has at least one row with candidates");
        for _ in 0..deep {
            a.on_key(press(KeyCode::Down));
        }
        a.on_key(press(KeyCode::Enter));
        assert!(
            matches!(a.mode, Mode::Pick { .. }),
            "the picker must be open for this test to mean anything"
        );

        let names: Vec<String> = a.rows[deep].choices.clone();
        // The picker opens on the CURRENT value, not on index 0 — walk to the
        // top first so the loop below starts where it thinks it does.
        for _ in 0..names.len() {
            a.on_key(press(KeyCode::Up));
        }
        for (n, name) in names.iter().enumerate() {
            if n > 0 {
                a.on_key(press(KeyCode::Down));
            }
            // The indented CANDIDATE line, not any marked line containing the
            // name: the setting row above shows its current value in the same
            // column vocabulary, so `▸ detail.time_format  both` satisfies a
            // looser needle while the candidate `both` is off screen — which
            // is exactly how this assertion first passed against a broken
            // window.
            let want = format!("      ▸ {name}");
            let text = all_text(&draw_at(&a, w, h));
            assert!(
                text.lines().any(|l| l.starts_with(&want)),
                "candidate {n} ({name}) is not on screen at {w}x{h}:\n{text}"
            );
        }
    }

    /// The picker has to render its candidates, and render them under the row
    /// they belong to. A picker drawn elsewhere on screen separates the value
    /// being previewed from the list it came from.
    #[test]
    fn the_open_picker_lists_the_candidates_under_its_own_row() {
        let mut a = app();
        a.on_key(press(KeyCode::Enter));
        let buf = draw(&a);
        let text = all_text(&buf);

        let owner = row_of(&buf, "theme.name");
        for name in theme::BUILTINS {
            assert!(text.contains(name), "candidate {name} missing:\n{text}");
            assert!(
                row_of(&buf, name) > owner || name == "nord",
                "{name} drawn above its row"
            );
        }
        assert!(
            text.contains("esc cancel"),
            "the picker must offer its own hints:\n{text}"
        );
    }

    /// The live preview, asserted on real cells rather than on `preview_theme`
    /// alone: repainting with the previewed theme must actually change the
    /// colours on screen. A render path that ignored the passed-in theme — or a
    /// caller that reloaded it only after saving — would leave `preview_theme`
    /// correct and the feature entirely absent.
    #[test]
    fn moving_the_picker_repaints_the_screen_in_the_previewed_theme() {
        let mut a = app();
        a.on_key(press(KeyCode::Enter)); // picker opens on nord
        let on_nord = draw(&a);
        a.on_key(press(KeyCode::Down)); // -> gruvbox
        let on_gruvbox = draw(&a);

        assert_eq!(a.preview_theme(), Some("gruvbox"));
        // The title carries the `header` role, which differs between the two.
        let y = row_of(&on_nord, "tasqx settings");
        let nord_fg = on_nord[(0, y)].fg;
        let gruvbox_fg = on_gruvbox[(0, y)].fg;
        assert_eq!(
            nord_fg,
            rt_style(theme::load("nord", None).role("header"), &caps())
                .fg
                .unwrap(),
            "the screen is not painted in the previewed theme"
        );
        assert_eq!(
            gruvbox_fg,
            rt_style(theme::load("gruvbox", None).role("header"), &caps())
                .fg
                .unwrap(),
        );
        assert_ne!(
            nord_fg, gruvbox_fg,
            "moving the picker changed nothing on screen"
        );
        // The header also names the theme being previewed, in words.
        assert!(
            all_text(&on_gruvbox).contains("theme: gruvbox"),
            "{}",
            all_text(&on_gruvbox)
        );
    }

    /// The footer messages are longer than a terminal is wide. `store_home_message`
    /// is ~130 characters, so an unwrapped footer showed the user "set it with
    /// `tas" and cut the answer off exactly where it became useful — the whole
    /// point of that message is naming `tasqx use`.
    #[test]
    fn a_footer_message_wider_than_the_screen_is_wrapped_not_truncated() {
        let mut a = app();
        a.selected = a
            .rows
            .iter()
            .position(|r| r.setting.home == Home::Store)
            .unwrap();
        a.on_key(press(KeyCode::Enter));
        assert!(
            a.status.len() > 80,
            "this guard assumes an over-wide message: {}",
            a.status
        );

        let th = theme::load("nord", None);
        // 60 columns: narrow enough that no part of the message fits by luck.
        // At 80 the words "tasqx use" happened to land before the cut, so an
        // unwrapped footer passed this guard while still losing the tail.
        let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
        term.draw(|f| render(&a, &th, &caps(), f)).unwrap();
        // Newlines are where the wrap happened; every word must survive.
        let flat = all_text(term.backend().buffer()).replace('\n', " ");
        for word in a.status.split_whitespace() {
            assert!(
                flat.contains(word),
                "{word:?} was cut off the footer:\n{flat}"
            );
        }
        assert!(
            flat.contains("esc quit"),
            "wrapping pushed the key hints off screen:\n{flat}"
        );
    }

    /// On a terminal with no Unicode the marker and the rule must degrade to
    /// ASCII, exactly as `Ctx::arrow` already does for printed
    /// output. Box-drawing bytes on a legacy Windows console render as mojibake
    /// and misalign every column.
    #[test]
    fn a_non_unicode_terminal_gets_ascii_markers() {
        let a = app();
        let ascii = Caps {
            depth: ColorDepth::Ansi16,
            ansi: true,
            unicode: false,
        };
        let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();
        term.draw(|f| render(&a, &theme::load("nord", None), &ascii, f))
            .unwrap();
        let text = all_text(term.backend().buffer());

        assert!(
            !text.contains('▸') && !text.contains('─'),
            "Unicode leaked into ASCII mode:\n{text}"
        );
        assert!(
            text.contains("> theme.name"),
            "no ASCII marker on the selected row:\n{text}"
        );
    }

    // ---- Sync section (#884) -------------------------------------------------

    fn field(key: &str, secret: bool) -> remote::Field {
        remote::Field {
            key: key.to_string(),
            label: key.to_uppercase(),
            secret,
            help: format!("enter {key}"),
            help_url: None,
        }
    }

    /// `c` lists what `list_on` found; with none, the screen says so and never
    /// opens an empty picker nobody could act on.
    #[test]
    fn c_with_no_connector_on_path_reports_it_instead_of_an_empty_picker() {
        let mut a = app();
        assert_eq!(
            a.on_key(press(KeyCode::Char('c'))),
            Some(Action::SyncListConnectors)
        );
        a.sync_names_found(Vec::new());
        assert_eq!(a.mode, Mode::Browse);
        assert!(a.status.contains("no tasqx-remote-"), "{}", a.status);
    }

    /// `c`, a name chosen, asks the caller to `describe` it; the reply
    /// becomes the form, ready for input at its first field.
    #[test]
    fn choosing_a_connector_opens_its_form_on_the_first_field() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('c')));
        a.sync_names_found(vec!["dir".to_string(), "r2".to_string()]);
        assert_eq!(
            a.mode,
            Mode::SyncPick {
                names: vec!["dir".into(), "r2".into()],
                cursor: 0
            }
        );
        a.on_key(press(KeyCode::Down));
        let act = a.on_key(press(KeyCode::Enter));
        assert_eq!(
            act,
            Some(Action::SyncDescribe {
                name: "r2".to_string()
            })
        );

        a.sync_form_ready(
            "r2".to_string(),
            vec![field("path", false), field("token", true)],
        );
        let Mode::SyncForm(form) = &a.mode else {
            panic!("expected a form: {:?}", a.mode);
        };
        assert_eq!(form.connector, "r2");
        assert_eq!(form.cursor, 0);
        assert_eq!(form.len(), 4, "two fields plus the passphrase twice");
    }

    /// Typing fills the field under the cursor; a secret field's own value is
    /// tracked the same way (masking is a render concern, tested below), and
    /// Tab/Down move forward one field at a time.
    #[test]
    fn typing_fills_the_focused_field_and_tab_moves_on() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('c')));
        a.sync_names_found(vec!["dir".to_string()]);
        a.on_key(press(KeyCode::Enter));
        a.sync_form_ready("dir".to_string(), vec![field("path", false)]);

        for c in "/srv/sync".chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
        a.on_key(press(KeyCode::Tab));
        for c in "hunter2".chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
        let Mode::SyncForm(form) = &a.mode else {
            panic!("left the form");
        };
        assert_eq!(form.fields[0].value, "/srv/sync");
        assert_eq!(form.passphrase, "hunter2");
        assert_eq!(form.cursor, 1);

        a.on_key(press(KeyCode::Backspace));
        let Mode::SyncForm(form) = &a.mode else {
            panic!("left the form");
        };
        assert_eq!(form.passphrase, "hunter");
    }

    /// Enter on the last field submits only once the two passphrases agree
    /// and neither is empty — the same two checks `sync setup`'s own prompt
    /// makes, just made here before a byte reaches anything.
    #[test]
    fn submitting_checks_the_passphrase_locally_before_asking_the_caller() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('c')));
        a.sync_names_found(vec!["dir".to_string()]);
        a.on_key(press(KeyCode::Enter));
        a.sync_form_ready("dir".to_string(), vec![field("path", false)]);

        // path (cursor 0) -> passphrase (cursor 1) -> passphrase again
        // (cursor 2, the last field). Only an Enter pressed AT the last field
        // checks anything; the two before it just move the cursor on, one
        // field at a time, same as Tab.
        for c in "/srv".chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
        a.on_key(press(KeyCode::Enter));
        a.on_key(press(KeyCode::Enter));
        // Both passphrase fields are still empty here: refused locally, the
        // form stays open.
        assert_eq!(a.on_key(press(KeyCode::Enter)), None);
        let Mode::SyncForm(form) = &a.mode else {
            panic!("left the form");
        };
        assert!(form.error.as_deref().unwrap().contains("character"));

        a.on_key(press(KeyCode::Up)); // back to the passphrase field
        for c in "correct horse".chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
        a.on_key(press(KeyCode::Down)); // on to confirm it
        for c in "correct horsE".chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(a.on_key(press(KeyCode::Enter)), None);
        let Mode::SyncForm(form) = &a.mode else {
            panic!("left the form");
        };
        assert!(form.error.as_deref().unwrap().contains("differ"));

        // Fix the second one and submit for real.
        for _ in 0.."correct horsE".chars().count() {
            a.on_key(press(KeyCode::Backspace));
        }
        for c in "correct horse".chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
        let act = a.on_key(press(KeyCode::Enter));
        assert_eq!(
            act,
            Some(Action::SyncSubmit {
                name: "dir".to_string(),
                values: BTreeMap::from([("path".to_string(), "/srv".to_string())]),
                passphrase: "correct horse".to_string(),
            })
        );
    }

    /// A connector's own `ok: false` is shown INLINE and the form is kept
    /// open with everything already typed still in it.
    #[test]
    fn a_rejected_setup_keeps_the_form_open_with_the_error_inline() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('c')));
        a.sync_names_found(vec!["dir".to_string()]);
        a.on_key(press(KeyCode::Enter));
        a.sync_form_ready("dir".to_string(), vec![field("path", false)]);
        for c in "relative/dir".chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
        a.sync_setup_failed("dir refused these settings: is not an absolute path".to_string());
        let Mode::SyncForm(form) = &a.mode else {
            panic!("the form must stay open on a rejection");
        };
        assert_eq!(
            form.fields[0].value, "relative/dir",
            "nothing typed is lost"
        );
        assert!(form.error.as_ref().unwrap().contains("absolute"));
    }

    /// A successful setup replaces the Sync section and closes the form.
    #[test]
    fn a_successful_setup_replaces_the_sync_section() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('c')));
        a.sync_names_found(vec!["dir".to_string()]);
        a.on_key(press(KeyCode::Enter));
        a.sync_form_ready("dir".to_string(), Vec::new());
        a.sync_setup_ok(SyncInfo {
            connector: Some("dir".to_string()),
            version: None,
            synced_relative: None,
            encrypted: true,
        });
        assert_eq!(a.mode, Mode::Browse);
        assert_eq!(a.sync.connector.as_deref(), Some("dir"));
        assert!(a.status.contains("connected to dir"));
    }

    /// `s` with nothing set up yet is refused locally, with a hint; set up,
    /// it asks the caller to run the sync and shows a one-line result.
    #[test]
    fn s_asks_for_a_sync_only_once_set_up() {
        let mut a = app();
        assert_eq!(a.on_key(press(KeyCode::Char('s'))), None);
        assert!(a.status.contains("press c to connect"), "{}", a.status);

        a.sync.connector = Some("dir".to_string());
        assert_eq!(a.on_key(press(KeyCode::Char('s'))), Some(Action::SyncNow));
        assert_eq!(a.status, "syncing…");

        a.sync_now_done(
            SyncInfo {
                connector: Some("dir".to_string()),
                version: Some("abcd1234".to_string()),
                synced_relative: Some("today".to_string()),
                encrypted: true,
            },
            "synced, pushed abcd1234".to_string(),
        );
        assert_eq!(a.status, "synced, pushed abcd1234");
        assert_eq!(a.sync.version.as_deref(), Some("abcd1234"));

        a.sync_now_failed("the link dropped".to_string());
        assert!(a.status.contains("the link dropped"));
    }

    /// `d` confirms before disconnecting, and any key but `y` cancels without
    /// touching the Sync section.
    #[test]
    fn d_confirms_before_disconnecting_and_anything_but_y_cancels() {
        let mut a = app();
        a.sync.connector = Some("dir".to_string());
        assert_eq!(a.on_key(press(KeyCode::Char('d'))), None);
        assert_eq!(a.mode, Mode::SyncConfirmDisconnect);

        assert_eq!(a.on_key(press(KeyCode::Esc)), None);
        assert_eq!(a.mode, Mode::Browse);
        assert_eq!(a.sync.connector.as_deref(), Some("dir"), "nothing changed");

        a.mode = Mode::SyncConfirmDisconnect;
        assert_eq!(
            a.on_key(press(KeyCode::Char('y'))),
            Some(Action::SyncDisconnect)
        );
        a.sync_disconnected();
        assert_eq!(a.mode, Mode::Browse);
        assert_eq!(a.sync, SyncInfo::default());
        assert!(a.status.contains("remote itself is untouched"));
    }

    /// The Sync section's status line, the confirm dialog and the form all
    /// draw, and a secret field's value is masked on screen — never its
    /// plain text, even mid-typing.
    #[test]
    fn the_sync_section_draws_status_form_and_confirm() {
        let mut a = app();
        a.sync.connector = Some("dir".to_string());
        a.sync.version = Some("abcd1234".to_string());
        a.sync.synced_relative = Some("today".to_string());
        a.sync.encrypted = true;
        let buf = draw(&a);
        let text = all_text(&buf);
        assert!(text.contains("sync: dir"), "{text}");
        assert!(text.contains("abcd1234"), "{text}");
        assert!(text.contains("today"), "{text}");

        a.mode = Mode::SyncForm(SyncForm::new(
            "dir".to_string(),
            vec![field("path", false), field("token", true)],
        ));
        if let Mode::SyncForm(form) = &mut a.mode {
            form.fields[1].value = "s3cret".to_string();
        }
        let buf = draw(&a);
        let text = all_text(&buf);
        assert!(text.contains("Connect: dir"), "{text}");
        assert!(
            !text.contains("s3cret"),
            "a secret value must never be drawn: {text}"
        );
        assert!(text.contains("••••••") || text.contains("******"), "{text}");

        a.mode = Mode::SyncConfirmDisconnect;
        let buf = draw(&a);
        let text = all_text(&buf);
        assert!(text.contains("Disconnect from dir?"), "{text}");
        assert!(text.contains("untouched"), "{text}");
    }
}
