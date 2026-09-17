//! The `tasqx setup` checklist (D158).
//!
//! Same split as every screen on the `tui` foundation (D26): [`App`] folds a
//! key into state and answers with an [`Action`]; [`render`] draws it and
//! decides nothing. Installing is the caller's, after the screen has closed,
//! so its result lines land on the normal screen where they can be read.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::render;
use crate::setup::{Item, Status};
use crate::theme::{Caps, Theme};
use crate::tui::{key_bar, rt_style, Hint, Key};

pub const KEYS: &[Key] = &[
    Key {
        keys: "j / k",
        help: "move down / up (the arrows too)",
        footer: Some(Hint {
            keys: "j/k",
            word: "move",
            rank: 2,
        }),
    },
    Key {
        keys: "space",
        help: "tick or untick the item under the cursor",
        footer: Some(Hint {
            keys: "space",
            word: "toggle",
            rank: 0,
        }),
    },
    Key {
        keys: "enter",
        help: "install every ticked item; a ticked item that differs is replaced",
        footer: Some(Hint {
            keys: "enter",
            word: "install",
            rank: 0,
        }),
    },
    Key {
        keys: "q / esc",
        help: "leave without installing anything",
        footer: Some(Hint {
            keys: "q",
            word: "quit",
            rank: 0,
        }),
    },
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Install,
    Quit,
}

pub struct Row {
    pub item: &'static Item,
    pub status: Status,
    pub ticked: bool,
}

pub struct App {
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// Where the items go, as the header names it (`~/.claude`).
    pub at: String,
}

impl App {
    /// Ticked by default: whatever is not installed yet. A `differs` skill
    /// starts unticked, because ticking it is what replaces the user's file.
    pub fn new(found: Vec<(&'static Item, Status)>, at: String) -> Self {
        let rows = found
            .into_iter()
            .map(|(item, status)| Row {
                item,
                status,
                ticked: status == Status::NotInstalled,
            })
            .collect();
        App {
            rows,
            cursor: 0,
            at,
        }
    }

    pub fn ticked(&self) -> impl Iterator<Item = &'static Item> + '_ {
        self.rows.iter().filter(|r| r.ticked).map(|r| r.item)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Windows reports a release for every press.
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        let last = self.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            KeyCode::Enter => return Some(Action::Install),
            KeyCode::Char(' ') => {
                if let Some(r) = self.rows.get_mut(self.cursor) {
                    r.ticked = !r.ticked;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            _ => {}
        }
        None
    }
}

pub fn render(app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let sty = |role: &str| rt_style(theme.role(role), caps);
    let area = frame.area();
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let title = "tasqx setup";
    let at = format!("Claude Code at {}", app.at);
    let gap = (area.width as usize).saturating_sub(render::width(title) + render::width(&at));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(title, sty("header")),
            Span::raw(" ".repeat(gap.max(3))),
            Span::styled(at, sty("muted")),
        ])),
        head,
    );

    let marker = if caps.unicode { "▸" } else { ">" };
    let name_w = app
        .rows
        .iter()
        .map(|r| render::width(r.item.name))
        .max()
        .unwrap_or(0)
        + 2;
    let what_w = app
        .rows
        .iter()
        .map(|r| render::width(r.item.what))
        .max()
        .unwrap_or(0)
        + 3;
    let mut lines: Vec<Line> = Vec::new();
    let mut group = "";
    for (i, r) in app.rows.iter().enumerate() {
        if r.item.group != group {
            // A blank line ahead of every heading but the first (house rule 7).
            if !group.is_empty() {
                lines.push(Line::raw(""));
            }
            group = r.item.group;
            lines.push(Line::styled(format!("  {group}"), sty("table.label")));
        }
        let here = i == app.cursor;
        lines.push(Line::from(vec![
            Span::styled(
                if here {
                    format!("{marker} ")
                } else {
                    "  ".into()
                },
                sty("accent"),
            ),
            Span::raw(if r.ticked { "[x] " } else { "[ ] " }),
            Span::raw(render::pad(r.item.name, name_w)),
            Span::styled(render::pad(r.item.what, what_w), sty("muted")),
            Span::styled(
                r.status.label(),
                if r.status.wants_attention() {
                    sty("warn")
                } else {
                    sty("muted")
                },
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), body);
    frame.render_widget(
        Paragraph::new(Line::from(key_bar(
            KEYS,
            area.width,
            None,
            sty("accent"),
            sty("muted"),
            caps.unicode,
        ))),
        foot,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::ITEMS;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app() -> App {
        App::new(
            vec![
                (&ITEMS[0], Status::NotInstalled),
                (&ITEMS[1], Status::Differs),
                (&ITEMS[2], Status::Current),
            ],
            "~/.claude".into(),
        )
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn names(a: &App) -> Vec<&'static str> {
        a.ticked().map(|i| i.name).collect()
    }

    #[test]
    fn only_what_is_not_installed_starts_ticked() {
        assert_eq!(names(&app()), ["mcp"]);
    }

    #[test]
    fn space_toggles_the_row_under_the_cursor_and_the_cursor_clamps() {
        let mut a = app();
        assert_eq!(a.on_key(press(KeyCode::Char(' '))), None);
        assert!(names(&a).is_empty());
        a.on_key(press(KeyCode::Up));
        assert_eq!(a.cursor, 0);
        for _ in 0..5 {
            a.on_key(press(KeyCode::Char('j')));
        }
        assert_eq!(a.cursor, 2);
        a.on_key(press(KeyCode::Char(' ')));
        assert_eq!(names(&a), ["retro"]);
        let mut release = press(KeyCode::Char(' '));
        release.kind = KeyEventKind::Release;
        a.on_key(release);
        assert_eq!(names(&a), ["retro"], "a release is not a second press");
    }

    #[test]
    fn enter_installs_and_q_esc_ctrl_c_quit() {
        let mut a = app();
        assert_eq!(a.on_key(press(KeyCode::Enter)), Some(Action::Install));
        assert_eq!(a.on_key(press(KeyCode::Char('q'))), Some(Action::Quit));
        assert_eq!(a.on_key(press(KeyCode::Esc)), Some(Action::Quit));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.on_key(ctrl_c), Some(Action::Quit));
    }

    #[test]
    fn the_screen_draws_groups_ticks_and_statuses() {
        let caps = Caps {
            depth: crate::theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        };
        let th = crate::theme::load("nord", None);
        let mut term = Terminal::new(TestBackend::new(72, 12)).unwrap();
        term.draw(|f| render(&app(), &th, &caps, f)).unwrap();
        let buf = term.backend().buffer();
        let text: Vec<String> = (0..buf.area().height)
            .map(|y| {
                (0..buf.area().width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        let all = text.join("\n");
        assert!(
            text[0].starts_with("tasqx setup") && text[0].contains("~/.claude"),
            "{all}"
        );
        let line = |needle: &str| text.iter().find(|l| l.contains(needle)).unwrap();
        assert!(line("Connection").trim() == "Connection", "{all}");
        assert!(line("mcp").starts_with("▸ [x] mcp") && line("mcp").contains("not installed"));
        assert!(
            line("tasqx-workflow").contains("[ ]") && line("tasqx-workflow").contains("differs")
        );
        assert!(line("retro").contains("current"), "{all}");
        assert!(
            all.contains("enter install") && all.contains("q quit"),
            "{all}"
        );
    }
}
