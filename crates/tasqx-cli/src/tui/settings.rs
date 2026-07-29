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

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::config::{self, Choices, Home, Kind, Setting};
use crate::theme::{Caps, Theme};
use crate::tui::rt_style;

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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Browse,
    /// An inline picker is open over the selected row's `choices`.
    Pick {
        cursor: usize,
    },
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
}

impl App {
    pub fn new(rows: Vec<Row>) -> Self {
        let theme_row = rows
            .iter()
            .position(|r| r.setting.choices == Choices::Themes);
        App {
            rows,
            selected: 0,
            mode: Mode::Browse,
            status: String::new(),
            theme_row,
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
        match self.mode {
            Mode::Browse => self.on_key_browse(key.code),
            Mode::Pick { cursor } => self.on_key_pick(key.code, cursor),
        }
    }

    fn on_key_browse(&mut self, code: KeyCode) -> Option<Action> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                // saturating_sub on an empty rows vec would underflow; rows is
                // never empty in practice (SETTINGS is non-empty) but the
                // arithmetic must not depend on that.
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
                None
            }
            KeyCode::Esc | KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Enter => self.begin_edit(),
            _ => None,
        }
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
        // A free-form string with no closed value set. No setting reaches this
        // today (`default_project` is the only Str + Free entry and it is
        // store-homed), but a registry row is a one-line edit away from it, and
        // a silent no-op on Enter reads as a broken screen.
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
    // The footer gets four lines, not two: `store_home_message` is ~130
    // characters and the summaries are not much shorter, so a single-line
    // detail area truncated the one sentence that tells the user what to do
    // instead — "set it with `tas" is worse than no message at all.
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(4),
    ])
    .areas(area);
    let [detail_area, help_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(foot);

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
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in app.rows.iter().enumerate() {
        let selected = i == app.selected;
        let shown = if row.value.is_empty() {
            "(unset)"
        } else {
            row.value.as_str()
        };
        // A store-homed row is dimmed so "shown but not editable here" reads
        // before the user presses Enter on it, not only after.
        let value_style = if row.setting.home == Home::Store {
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
            Span::styled(format!("{:<18}", row.setting.key), sty("project")),
            Span::styled(format!("{shown:<22}"), value_style),
            Span::styled(row.source.clone(), sty("muted")),
        ]));

        // The inline picker sits directly under its own row, so the value being
        // previewed and the list it came from are never separated on screen.
        if selected {
            if let Mode::Pick { cursor } = app.mode {
                for (j, cand) in row.choices.iter().enumerate() {
                    let at = j == cursor;
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
                        Span::styled(cand.clone(), if at { sty("warn") } else { sty("muted") }),
                    ]));
                }
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), body);

    // --- footer --------------------------------------------------------------
    let help = match app.mode {
        Mode::Browse => "up/down move   enter edit   esc quit",
        Mode::Pick { .. } => "up/down preview   enter save   esc cancel",
    };
    let detail = if app.status.is_empty() {
        Line::styled(app.rows[app.selected].setting.summary, sty("muted"))
    } else {
        Line::styled(app.status.clone(), sty("warn"))
    };
    frame.render_widget(
        Paragraph::new(detail).wrap(ratatui::widgets::Wrap { trim: true }),
        detail_area,
    );
    frame.render_widget(Paragraph::new(Line::styled(help, sty("muted"))), help_area);
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
                value: match s.key {
                    "theme.name" => "nord".to_string(),
                    "notify.enabled" => "false".to_string(),
                    _ => String::new(),
                },
                source: match s.home {
                    Home::Store => "store".to_string(),
                    Home::Toml => "default".to_string(),
                },
                choices: match s.choices {
                    Choices::Themes => theme::BUILTINS.iter().map(|t| t.to_string()).collect(),
                    Choices::Free => Vec::new(),
                    Choices::OneOf(values) => values.iter().map(|v| (*v).to_string()).collect(),
                },
            })
            .collect();
        App::new(rows)
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
        let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();
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
        for _ in 0..10 {
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
        assert_eq!(a.rows[a.selected].value, "false");

        let act = a
            .on_key(press(KeyCode::Enter))
            .expect("enter must produce a save");
        assert_eq!(
            act,
            Action::Save {
                key: "notify.enabled",
                value: "true".into()
            }
        );
        assert_eq!(
            a.rows[a.selected].value, "true",
            "the screen must show what it saved"
        );

        let act = a
            .on_key(press(KeyCode::Enter))
            .expect("a second enter toggles back");
        assert_eq!(
            act,
            Action::Save {
                key: "notify.enabled",
                value: "false".into()
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

    /// The selection marker is the only thing telling the user which row Enter
    /// will act on. A marker that did not move — or moved on the wrong row —
    /// makes every subsequent keystroke a guess.
    #[test]
    fn the_selection_marker_sits_on_the_selected_row() {
        let mut a = app();
        let before = draw(&a);
        assert!(line_at(&before, row_of(&before, "theme.name")).starts_with('▸'));
        assert!(!line_at(&before, row_of(&before, "notify.enabled")).starts_with('▸'));

        a.on_key(press(KeyCode::Down));
        let after = draw(&a);
        assert!(line_at(&after, row_of(&after, "notify.enabled")).starts_with('▸'));
        assert!(!line_at(&after, row_of(&after, "theme.name")).starts_with('▸'));
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
    /// ASCII, exactly as `Ctx::hrule`/`Ctx::arrow` already do for printed
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
}
