//! The notes browser (D241, task #929): `tasqx notes` on a terminal, and `N`
//! from the dashboard.
//!
//! One line per note, newest first, with the memory browser's keys (D121):
//! `/` filters as you type, Enter opens a note whole. On top of those, the
//! writes a note takes: `a` archives or brings one back, `t` and `m` promote
//! it, `x` removes it after a y/n, `l` keeps the notes holding a link.
//!
//! Same shape as the memory browser: a pure [`App`] that folds keys into
//! state, and a [`render`] that decides nothing. A write is the one thing the
//! screen cannot do for itself, so [`App::on_key`] hands it back as an [`Op`]
//! carrying the `_rev` the screen read; the caller makes the call, reads the
//! list again into [`App::reload`] and says what happened through
//! [`App::said`].

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style as RtStyle};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use serde_json::Value;

use crate::columns::{self, Column};
use crate::render;
use crate::theme::{Caps, Theme};
use crate::tui::list::{ListState, Mode};
use crate::tui::{self, first_visible, rt_style, Hint, Key};

/// Which notes the list holds, as `--archived` and `--all` ask.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Show {
    #[default]
    Open,
    Archived,
    All,
}

/// What the screen opens on: `tasqx notes`' own flags.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub show: Show,
    pub links: bool,
    /// `--search`, which becomes the query `/` edits, so Esc clears it.
    pub query: String,
    /// `--project`: the caller reads only that project's notes, and the
    /// header names it.
    pub project: Option<String>,
}

/// One note as the screen shows it, sanitised at construction (D19): a body
/// is text somebody pasted, and none of its bytes reach the terminal as
/// anything but text.
pub struct Note {
    pub id: String,
    pub short_id: i64,
    pub rev: i64,
    body: String,
    first: String,
    lower: String,
    project: String,
    added: String,
    archived: bool,
    links: bool,
    /// Where a promotion sent it, spelled (`task #64`, `a memory doc`).
    promoted: Option<String>,
}

impl Note {
    /// From one `note.list` row. `added` arrives spelled (`2d ago`): the
    /// screen has no clock of its own.
    pub fn new(v: &Value, added: String) -> Self {
        let body = render::san_multiline(v["body"].as_str().unwrap_or(""));
        let promoted = v["promoted_to"].as_str().map(|to| {
            match (
                to.starts_with("task:"),
                v["promoted_task_short_id"].as_i64(),
            ) {
                (true, Some(n)) => format!("task #{n}"),
                (true, None) => "a task".to_string(),
                (false, _) => "a memory doc".to_string(),
            }
        });
        Note {
            id: render::san(v["id"].as_str().unwrap_or("")),
            short_id: v["short_id"].as_i64().unwrap_or(0),
            rev: v["_rev"].as_i64().unwrap_or(0),
            first: render::san(render::note_first_line(&body)),
            lower: body.to_lowercase(),
            body,
            project: render::san(v["project"].as_str().unwrap_or("")),
            added,
            archived: v["status"] == "archived",
            links: v["has_links"].as_bool().unwrap_or(false),
            promoted,
        }
    }
}

/// A write the screen asks for, with the rev it read, so a note changed in
/// between is the API's conflict rather than a lost write.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Op {
    /// `note.update {status}`: `archived`, or `open` to bring it back.
    Status {
        id: String,
        short_id: i64,
        rev: i64,
        to: &'static str,
    },
    /// `note.promote {to}`: `task` or `memory`.
    Promote {
        id: String,
        short_id: i64,
        rev: i64,
        to: &'static str,
    },
    /// `note.remove`, after the reader said y.
    Remove { id: String, short_id: i64, rev: i64 },
}

/// What the caller does after a key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Stay,
    Quit,
    Do(Op),
}

/// The state of the screen.
pub struct App {
    notes: Vec<Note>,
    filter: Filter,
    /// Cursor, query, mode and scroll; `matches` is newest first.
    ls: ListState,
    /// `x` was pressed on this note and the screen waits for y or n.
    confirm: Option<usize>,
    /// The line above the key bar: what the last write did, or why not.
    echo: Option<(String, bool)>,
    unicode: bool,
}

impl App {
    pub fn new(notes: Vec<Note>, filter: Filter, unicode: bool) -> Self {
        let mut ls = ListState::new(0);
        ls.query = filter.query.clone();
        let mut app = App {
            notes,
            filter,
            ls,
            confirm: None,
            echo: None,
            unicode,
        };
        app.refilter();
        app
    }

    pub fn observe(&mut self, width: u16, height: u16) {
        self.ls.size = (width, height);
        self.ls.scroll = self.ls.scroll.min(self.max_scroll());
    }

    pub fn selected(&self) -> Option<&Note> {
        self.ls.selected().map(|i| &self.notes[i])
    }

    /// The list read again after a write. The cursor stays on its note, or
    /// on the row that took its place when the write moved it out of view.
    pub fn reload(&mut self, notes: Vec<Note>) {
        let held = self.selected().map(|n| n.id.clone());
        let at = self.ls.cursor;
        self.notes = notes;
        self.refilter();
        match held.and_then(|id| self.ls.matches.iter().position(|&i| self.notes[i].id == id)) {
            Some(pos) => self.ls.cursor = pos,
            None => {
                self.ls.cursor = at.min(self.ls.matches.len().saturating_sub(1));
                // The note it was reading is gone from the list, so the
                // detail view has nothing to show.
                if self.ls.mode == Mode::Detail {
                    self.ls.mode = Mode::List;
                }
            }
        }
    }

    /// What a write did (`Ok`), or the refusal (`Err`), for the echo line.
    pub fn said(&mut self, outcome: Result<String, String>) {
        self.echo = Some(match outcome {
            Ok(s) => (s, false),
            Err(s) => (render::san(&s), true),
        });
    }

    fn shown(&self, n: &Note) -> bool {
        let status = match self.filter.show {
            Show::Open => !n.archived,
            Show::Archived => n.archived,
            Show::All => true,
        };
        status && (!self.filter.links || n.links)
    }

    /// The notes the filter and the query keep, newest first, the cursor on
    /// the note it was on. Every word of the query must occur in the body,
    /// ignoring case: `--search`'s plain match, not a ranking (D241).
    fn refilter(&mut self) {
        let anchor = self.ls.selected();
        let q = self.ls.query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.ls.matches = (0..self.notes.len())
            .filter(|&i| {
                let n = &self.notes[i];
                self.shown(n) && terms.iter().all(|t| n.lower.contains(t))
            })
            .collect();
        self.ls.cursor = anchor
            .and_then(|a| self.ls.matches.iter().position(|&i| i == a))
            .unwrap_or(0);
    }

    /// Fold one key into the state. Only presses count (the Windows release
    /// guard every screen carries).
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        if key.kind != KeyEventKind::Press {
            return Outcome::Stay;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Outcome::Quit;
        }
        self.echo = None;
        if let Some(i) = self.confirm.take() {
            let n = &self.notes[i];
            if key.code == KeyCode::Char('y') {
                return Outcome::Do(Op::Remove {
                    id: n.id.clone(),
                    short_id: n.short_id,
                    rev: n.rev,
                });
            }
            self.echo = Some((format!("Kept note #{}", n.short_id), false));
            return Outcome::Stay;
        }
        if matches!(self.ls.mode, Mode::List | Mode::Detail) {
            if let Some(out) = self.note_key(key.code) {
                return out;
            }
        }
        let len = self.notes.len();
        let notes = &self.notes;
        let score = |i: usize, terms: &[&str]| {
            terms
                .iter()
                .all(|t| notes[i].lower.contains(t))
                .then_some(0)
        };
        match self.ls.mode {
            Mode::List => {
                let page = self.list_rows();
                if !self.ls.list_key(key, ctrl, page, len, score) {
                    return match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => Outcome::Quit,
                        _ => Outcome::Stay,
                    };
                }
            }
            Mode::Search => self.ls.search_key(key, ctrl, len, score),
            Mode::Detail => {
                let (page, max) = (self.detail_rows(), self.max_scroll());
                self.ls.detail_key(key, page, max);
            }
        }
        // ListState ranks every note; the status and link filter is this
        // screen's, so it is applied again over whatever it kept.
        self.refilter();
        Outcome::Stay
    }

    /// The writes, and `l`. `None` leaves the key to the list.
    fn note_key(&mut self, code: KeyCode) -> Option<Outcome> {
        if code == KeyCode::Char('l') && self.ls.mode == Mode::List {
            self.filter.links = !self.filter.links;
            self.refilter();
            return Some(Outcome::Stay);
        }
        let to = match code {
            KeyCode::Char('t') => "task",
            KeyCode::Char('m') => "memory",
            KeyCode::Char('a' | 'x') => "",
            _ => return None,
        };
        let i = self.ls.selected()?;
        let n = &self.notes[i];
        let (id, short_id, rev) = (n.id.clone(), n.short_id, n.rev);
        Some(match code {
            KeyCode::Char('a') => Outcome::Do(Op::Status {
                id,
                short_id,
                rev,
                to: if n.archived { "open" } else { "archived" },
            }),
            KeyCode::Char('x') => {
                self.confirm = Some(i);
                Outcome::Stay
            }
            // The key bar does not offer these on a promoted note; pressed
            // anyway, the screen answers rather than asking the API.
            _ if n.promoted.is_some() => {
                let to = n.promoted.clone().unwrap_or_default();
                self.echo = Some((
                    format!("Note #{short_id} was already promoted to {to}"),
                    true,
                ));
                Outcome::Stay
            }
            _ => Outcome::Do(Op::Promote {
                id,
                short_id,
                rev,
                to,
            }),
        })
    }

    /// The frame less the header, the blank under it, the echo line and the
    /// key bar.
    fn list_rows(&self) -> usize {
        (self.ls.size.1 as usize).saturating_sub(4).max(1)
    }

    /// The frame less the title, the facts, a blank, the echo line and the
    /// key bar.
    fn detail_rows(&self) -> usize {
        (self.ls.size.1 as usize).saturating_sub(5).max(1)
    }

    fn detail_width(&self) -> usize {
        (self.ls.size.0 as usize).saturating_sub(2).max(8)
    }

    fn body_lines(&self) -> Vec<String> {
        self.selected()
            .map(|n| wrap_body(&n.body, self.detail_width()))
            .unwrap_or_default()
    }

    fn max_scroll(&self) -> usize {
        if self.ls.mode != Mode::Detail {
            return 0;
        }
        self.body_lines().len().saturating_sub(self.detail_rows())
    }

    /// The key table for what is on screen, less the promote keys on a
    /// promoted note: the bar never offers a write the note cannot take.
    fn keys(&self) -> Vec<&'static Key> {
        let table = match (self.confirm.is_some(), self.ls.mode) {
            (true, _) => CONFIRM_KEYS,
            (_, Mode::List) => LIST_KEYS,
            (_, Mode::Search) => tui::memory::SEARCH_KEYS,
            (_, Mode::Detail) => DETAIL_KEYS,
        };
        let promoted = self.selected().is_some_and(|n| n.promoted.is_some());
        table
            .iter()
            .filter(|k| !(promoted && matches!(k.keys, "t" | "m")))
            .collect()
    }
}

/// A body in lines of at most `width` cells, its own line breaks kept. A word
/// wider than the line, which is a URL more often than not, is broken across
/// as many lines as it needs and never cut: the detail view exists to show a
/// note whole.
pub fn wrap_body(body: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in body.lines() {
        let mut line = String::new();
        let mut used = 0;
        for word in raw.split_whitespace() {
            let w = render::width(word);
            if used > 0 && used + 1 + w > width {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            if w > width {
                if used > 0 {
                    out.push(std::mem::take(&mut line));
                    used = 0;
                }
                for c in word.chars() {
                    let cw = render::width(c.encode_utf8(&mut [0; 4]));
                    if used + cw > width {
                        out.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    line.push(c);
                    used += cw;
                }
                continue;
            }
            if used > 0 {
                line.push(' ');
                used += 1;
            }
            line.push_str(word);
            used += w;
        }
        out.push(line);
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

// ============================================================================
// The key tables
// ============================================================================

/// The list's keys. The footer is drawn from this, lowest rank first.
pub const LIST_KEYS: &[Key] = &[
    Key {
        keys: "j / k",
        help: "move down / up (the arrows too)",
        footer: Some(Hint {
            keys: "j/k",
            word: "move",
            rank: 5,
        }),
    },
    Key {
        keys: "/",
        help: "search the notes' text",
        footer: Some(Hint {
            keys: "/",
            word: "search",
            rank: 2,
        }),
    },
    Key {
        keys: "enter",
        help: "read the note under the cursor whole",
        footer: Some(Hint {
            keys: "enter",
            word: "open",
            rank: 1,
        }),
    },
    Key {
        keys: "a",
        help: "archive the note, or bring an archived one back",
        footer: Some(Hint {
            keys: "a",
            word: "archive",
            rank: 3,
        }),
    },
    Key {
        keys: "t",
        help: "promote the note to a task, and archive it",
        footer: Some(Hint {
            keys: "t",
            word: "task",
            rank: 4,
        }),
    },
    Key {
        keys: "m",
        help: "promote the note to a memory doc, and archive it",
        footer: Some(Hint {
            keys: "m",
            word: "memory",
            rank: 6,
        }),
    },
    Key {
        keys: "x",
        help: "remove the note for good, after a y/n",
        footer: Some(Hint {
            keys: "x",
            word: "remove",
            rank: 7,
        }),
    },
    Key {
        keys: "l",
        help: "only the notes holding a link, or all of them again",
        footer: Some(Hint {
            keys: "l",
            word: "links",
            rank: 8,
        }),
    },
    Key {
        keys: "g / G",
        help: "first / last note",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 10,
        }),
    },
    Key {
        keys: "esc",
        help: "clear the search; with none, leave",
        footer: Some(Hint {
            keys: "esc",
            word: "clear",
            rank: 9,
        }),
    },
    Key {
        keys: "q",
        help: "leave",
        footer: Some(Hint {
            keys: "q",
            word: "quit",
            rank: 0,
        }),
    },
];

/// The detail view's keys: scrolling, the writes, and the way back.
pub const DETAIL_KEYS: &[Key] = &[
    Key {
        keys: "j / k",
        help: "scroll down / up",
        footer: Some(Hint {
            keys: "j/k",
            word: "scroll",
            rank: 5,
        }),
    },
    Key {
        keys: "space / b",
        help: "a page down / up",
        footer: Some(Hint {
            keys: "space",
            word: "page",
            rank: 8,
        }),
    },
    Key {
        keys: "g / G",
        help: "top / bottom",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 9,
        }),
    },
    Key {
        keys: "a",
        help: "archive the note, or bring an archived one back",
        footer: Some(Hint {
            keys: "a",
            word: "archive",
            rank: 1,
        }),
    },
    Key {
        keys: "t",
        help: "promote the note to a task, and archive it",
        footer: Some(Hint {
            keys: "t",
            word: "task",
            rank: 2,
        }),
    },
    Key {
        keys: "m",
        help: "promote the note to a memory doc, and archive it",
        footer: Some(Hint {
            keys: "m",
            word: "memory",
            rank: 3,
        }),
    },
    Key {
        keys: "x",
        help: "remove the note for good, after a y/n",
        footer: Some(Hint {
            keys: "x",
            word: "remove",
            rank: 4,
        }),
    },
    Key {
        keys: "esc / q",
        help: "back to the list",
        footer: Some(Hint {
            keys: "esc",
            word: "back",
            rank: 0,
        }),
    },
];

/// While `x` waits for an answer.
pub const CONFIRM_KEYS: &[Key] = &[
    Key {
        keys: "y",
        help: "remove the note",
        footer: Some(Hint {
            keys: "y",
            word: "remove",
            rank: 0,
        }),
    },
    Key {
        keys: "n / esc",
        help: "keep it",
        footer: Some(Hint {
            keys: "n",
            word: "keep",
            rank: 1,
        }),
    },
];

// ============================================================================
// Rendering
// ============================================================================

/// Draw the screen. Decides nothing: `App` made every choice.
pub fn render(app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let sty = |role: &str| rt_style(theme.role(role), caps);
    let area = frame.area();
    if area.height < 2 || area.width == 0 {
        return;
    }
    match app.ls.mode {
        Mode::Detail => draw_detail(app, &sty, frame, area),
        Mode::List | Mode::Search => draw_list(app, &sty, frame, area),
    }

    // The echo: a question waiting on y/n, what a write did, or why not.
    let echo = match (app.confirm, &app.echo) {
        (Some(i), _) => Some((
            format!(
                "Remove note #{} for good? y removes it, n keeps it",
                app.notes[i].short_id
            ),
            sty("warn"),
        )),
        (None, Some((text, true))) => Some((text.clone(), sty("danger"))),
        (None, Some((text, false))) => Some((
            text.clone(),
            RtStyle::default().add_modifier(Modifier::BOLD),
        )),
        (None, None) => None,
    };
    if let Some((text, style)) = echo {
        let room = (area.width as usize).saturating_sub(2);
        line_at(
            frame,
            area,
            area.bottom() - 2,
            area.x,
            vec![
                Span::raw(" "),
                Span::styled(render::truncate(&text, room, app.unicode), style),
            ],
        );
    }

    let foot = Rect {
        y: area.bottom() - 1,
        height: 1,
        ..area
    };
    let position = (app.ls.mode == Mode::Detail && app.confirm.is_none())
        .then(|| (app.ls.scroll, app.detail_rows(), app.body_lines().len()));
    let keys: Vec<Key> = app
        .keys()
        .into_iter()
        .map(|k| Key {
            keys: k.keys,
            help: k.help,
            footer: k.footer.as_ref().map(|h| Hint { ..*h }),
        })
        .collect();
    let spans = tui::key_bar(
        &keys,
        area.width,
        position,
        sty("accent"),
        sty("muted"),
        app.unicode,
    );
    frame.render_widget(Paragraph::new(Line::from(spans)), foot);
}

fn line_at(frame: &mut Frame, area: Rect, y: u16, x: u16, spans: Vec<Span<'static>>) {
    if y >= area.bottom() || x >= area.right() {
        return;
    }
    let rect = Rect {
        x,
        y,
        width: area.right() - x,
        height: 1,
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// What the list holds, in words: the question asked (rule 8), and only the
/// parts of it that narrow anything.
fn question(app: &App) -> String {
    let mid = if app.unicode { " · " } else { " - " };
    let n = app.ls.matches.len();
    let what = match app.filter.show {
        Show::Open => "open ",
        Show::Archived => "archived ",
        Show::All => "",
    };
    let mut parts = vec![format!(
        "{n} {what}{}",
        if n == 1 { "note" } else { "notes" }
    )];
    if app.filter.links {
        parts.push("with a link".to_string());
    }
    if let Some(p) = &app.filter.project {
        parts.push(format!("project {}", render::san(p)));
    }
    parts.join(mid)
}

fn draw_list(app: &App, sty: &dyn Fn(&str) -> RtStyle, frame: &mut Frame, area: Rect) {
    let (w, x0, y0) = (area.width as usize, area.x, area.y);
    let mut head = vec![
        Span::raw(" "),
        Span::styled("notes".to_string(), sty("header")),
        Span::raw("   "),
    ];
    let asked = question(app);
    if app.ls.mode == Mode::Search || !app.ls.query.is_empty() {
        let total = app.notes.iter().filter(|n| app.shown(n)).count();
        head.extend(tui::search_spans(
            &tui::SearchLine {
                filter: "",
                query: &app.ls.query,
                searching: app.ls.mode == Mode::Search,
                kept: app.ls.matches.len(),
                total,
            },
            w.saturating_sub(1 + 5 + 3),
            sty("accent"),
            sty("muted"),
            app.unicode,
        ));
    } else {
        head.push(Span::styled(asked, sty("table.label")));
    }
    line_at(frame, area, y0, x0, head);

    let top = y0 + 2;
    if app.ls.matches.is_empty() {
        let msg = if !app.ls.query.is_empty() {
            format!("Nothing matches {:?}. Esc clears the search.", app.ls.query)
        } else if app.filter.links {
            "No notes here hold a link. l shows them all.".to_string()
        } else {
            "No notes here. `tasqx note <text…>` captures one.".to_string()
        };
        line_at(
            frame,
            area,
            top,
            x0,
            vec![Span::raw("   "), Span::styled(msg, sty("muted"))],
        );
        return;
    }

    let rows: Vec<&Note> = app.ls.matches.iter().map(|&i| &app.notes[i]).collect();
    let widest = |f: fn(&Note) -> usize| rows.iter().map(|n| f(n)).max().unwrap_or(0);
    let id_w = widest(|n| n.short_id.to_string().len());
    let link_w = widest(|n| usize::from(n.links));
    let project_w = widest(|n| render::width(&n.project));
    let added_w = widest(|n| render::width(&n.added));
    let cw = columns::fit(
        &[
            Column::fixed(id_w),
            Column::shrinks(widest(|n| render::width(&n.first)), 12),
            Column::fixed(link_w),
            Column::drops(project_w, 8),
            // A day cut to `yesterd…` is no day: whole or not at all.
            Column::drops(added_w, added_w),
        ],
        // A margin, the cursor and a space.
        w.saturating_sub(4),
    );

    let mark_link = if app.unicode { "↗" } else { "L" };
    let page = app.list_rows();
    let first = first_visible(app.ls.cursor, rows.len(), page);
    for (n, note) in rows.iter().enumerate().skip(first).take(page) {
        let on = n == app.ls.cursor;
        let mark = match (on, app.unicode) {
            (true, true) => "▸",
            (true, false) => ">",
            _ => " ",
        };
        // An archived note under `--all` recedes, as on the table.
        let text_style = match (on, note.archived) {
            (true, _) => sty("accent").add_modifier(Modifier::BOLD),
            (false, true) => sty("muted"),
            (false, false) => RtStyle::default(),
        };
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(mark.to_string(), sty("accent")),
            Span::raw(" "),
            Span::styled(render::pad(&note.short_id.to_string(), cw[0]), sty("muted")),
            Span::raw("  "),
            Span::styled(
                render::pad(&render::truncate(&note.first, cw[1], app.unicode), cw[1]),
                text_style,
            ),
        ];
        let mut cell = |text: &str, width: usize, style: RtStyle| {
            if width > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    render::pad(&render::truncate(text, width, app.unicode), width),
                    style,
                ));
            }
        };
        cell(
            if note.links { mark_link } else { "" },
            cw[2],
            sty("accent"),
        );
        cell(&note.project, cw[3], sty("project"));
        cell(&note.added, cw[4], sty("table.label"));
        line_at(frame, area, top + (n - first) as u16, x0, spans);
    }
}

fn draw_detail(app: &App, sty: &dyn Fn(&str) -> RtStyle, frame: &mut Frame, area: Rect) {
    let Some(note) = app.selected() else { return };
    let mid = if app.unicode { "  ·  " } else { "  -  " };
    let (x0, y0) = (area.x, area.y);
    line_at(
        frame,
        area,
        y0,
        x0,
        vec![
            Span::raw(" "),
            Span::styled(format!("note #{}", note.short_id), sty("header")),
        ],
    );
    let mut facts = vec![Span::raw(" ")];
    let mut fact = |text: String, role: &str| {
        if facts.len() > 1 {
            facts.push(Span::styled(mid.to_string(), sty("muted")));
        }
        facts.push(Span::styled(text, sty(role)));
    };
    if !note.project.is_empty() {
        fact(note.project.clone(), "project");
    }
    fact(format!("added {}", note.added), "table.label");
    match &note.promoted {
        Some(to) => fact(format!("promoted to {to}"), "table.label"),
        None if note.archived => fact("archived".to_string(), "table.label"),
        None => {}
    }
    line_at(frame, area, y0 + 1, x0, facts);

    let top = y0 + 3;
    for (r, line) in app
        .body_lines()
        .into_iter()
        .skip(app.ls.scroll)
        .take(app.detail_rows())
        .enumerate()
    {
        line_at(
            frame,
            area,
            top + r as u16,
            x0,
            vec![Span::raw(" "), Span::raw(line)],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use serde_json::json;

    fn note(short: i64, body: &str, status: &str, promoted: Option<&str>) -> Note {
        Note::new(
            &json!({
                "id": format!("uuid-{short}"),
                "short_id": short,
                "_rev": short * 10,
                "body": body,
                "project": if short % 2 == 0 { Value::Null } else { json!("api") },
                "status": status,
                "has_links": body.contains("https://"),
                "promoted_to": promoted,
                "promoted_task_short_id": promoted.map(|_| 64),
            }),
            "2d ago".to_string(),
        )
    }

    fn notes() -> Vec<Note> {
        vec![
            note(4, "idea: a dark-mode toggle", "open", None),
            note(
                3,
                "read https://www.rfc-editor.org/rfc/rfc9457 first",
                "open",
                None,
            ),
            note(2, "try the tracing exporter", "open", None),
            note(1, "filed already", "archived", Some("task:abc")),
        ]
    }

    fn app_with(filter: Filter) -> App {
        let mut a = App::new(notes(), filter, true);
        a.observe(100, 24);
        a
    }

    fn app() -> App {
        app_with(Filter::default())
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        press(KeyCode::Char(c))
    }

    fn ids(a: &App) -> Vec<i64> {
        a.ls.matches.iter().map(|&i| a.notes[i].short_id).collect()
    }

    fn draw(a: &App, w: u16, h: u16) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let th = theme::load("nord", None);
        let caps = Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        };
        term.draw(|f| render(a, &th, &caps, f)).unwrap();
        term.backend().buffer().clone()
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol())
            .collect()
    }

    fn all(buf: &Buffer) -> String {
        (0..buf.area().height)
            .map(|y| row(buf, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `tasqx notes`' flags are where the screen opens: open notes by
    /// default, `--archived`, `--all`, `--links`, and `--search` as the
    /// query Esc clears.
    #[test]
    fn the_flags_set_the_initial_filter() {
        assert_eq!(ids(&app()), [4, 3, 2]);
        let archived = app_with(Filter {
            show: Show::Archived,
            ..Filter::default()
        });
        assert_eq!(ids(&archived), [1]);
        let every = app_with(Filter {
            show: Show::All,
            ..Filter::default()
        });
        assert_eq!(ids(&every), [4, 3, 2, 1]);
        let links = app_with(Filter {
            links: true,
            ..Filter::default()
        });
        assert_eq!(ids(&links), [3]);
        let mut searched = app_with(Filter {
            query: "EXPORTER".to_string(),
            ..Filter::default()
        });
        assert_eq!(ids(&searched), [2], "the search ignores case");
        assert_eq!(searched.on_key(press(KeyCode::Esc)), Outcome::Stay);
        assert_eq!(ids(&searched), [4, 3, 2], "esc clears --search's query");
    }

    #[test]
    fn l_toggles_the_links_only_view() {
        let mut a = app();
        a.on_key(ch('l'));
        assert_eq!(ids(&a), [3]);
        a.on_key(ch('l'));
        assert_eq!(ids(&a), [4, 3, 2]);
    }

    /// A search keeps the status filter: typing over the open notes never
    /// brings an archived one in.
    #[test]
    fn a_search_never_widens_the_filter() {
        let mut a = app();
        a.on_key(ch('/'));
        for c in "file".chars() {
            a.on_key(ch(c));
        }
        assert!(ids(&a).is_empty(), "{:?}", ids(&a));
        for _ in 0..4 {
            a.on_key(press(KeyCode::Backspace));
        }
        assert_eq!(ids(&a), [4, 3, 2], "an empty query showed archived notes");
    }

    /// Every write carries the rev the screen read.
    #[test]
    fn the_writes_carry_the_rev_they_read() {
        let mut a = app();
        a.on_key(ch('j'));
        assert_eq!(
            a.on_key(ch('a')),
            Outcome::Do(Op::Status {
                id: "uuid-3".into(),
                short_id: 3,
                rev: 30,
                to: "archived"
            })
        );
        assert_eq!(
            a.on_key(ch('t')),
            Outcome::Do(Op::Promote {
                id: "uuid-3".into(),
                short_id: 3,
                rev: 30,
                to: "task"
            })
        );
        assert_eq!(
            a.on_key(ch('m')),
            Outcome::Do(Op::Promote {
                id: "uuid-3".into(),
                short_id: 3,
                rev: 30,
                to: "memory"
            })
        );
    }

    /// `a` on an archived note brings it back.
    #[test]
    fn a_on_an_archived_note_unarchives_it() {
        let mut a = app_with(Filter {
            show: Show::Archived,
            ..Filter::default()
        });
        assert!(matches!(
            a.on_key(ch('a')),
            Outcome::Do(Op::Status { to: "open", .. })
        ));
    }

    /// `x` asks first; only y removes, and anything else keeps the note.
    #[test]
    fn x_removes_only_after_a_y() {
        let mut a = app();
        assert_eq!(a.on_key(ch('x')), Outcome::Stay);
        assert!(all(&draw(&a, 100, 24)).contains("Remove note #4 for good?"));
        assert_eq!(
            a.on_key(ch('y')),
            Outcome::Do(Op::Remove {
                id: "uuid-4".into(),
                short_id: 4,
                rev: 40
            })
        );
        for no in [ch('n'), press(KeyCode::Esc), ch('q')] {
            a.on_key(ch('x'));
            assert_eq!(a.on_key(no), Outcome::Stay, "{no:?} removed the note");
            assert!(all(&draw(&a, 100, 24)).contains("Kept note #4"));
        }
    }

    /// A promoted note is offered no promote key, and a promote key pressed
    /// anyway is answered on the screen, not sent.
    #[test]
    fn a_promoted_note_offers_no_promote_keys() {
        let mut a = app_with(Filter {
            show: Show::Archived,
            ..Filter::default()
        });
        let bar = row(&draw(&a, 140, 24), 23);
        assert!(!bar.contains("task") && !bar.contains("memory"), "{bar}");
        assert_eq!(a.on_key(ch('t')), Outcome::Stay);
        let screen = all(&draw(&a, 140, 24));
        assert!(
            screen.contains("Note #1 was already promoted to task #64"),
            "{screen}"
        );
        let open = app();
        let bar = row(&draw(&open, 140, 24), 23);
        assert!(bar.contains("task") && bar.contains("memory"), "{bar}");
    }

    /// A refusal from the API is a line on the screen, never the end of it.
    #[test]
    fn a_refusal_is_shown_inline() {
        let mut a = app();
        a.said(Err("note #4 changed since it was read".into()));
        let screen = all(&draw(&a, 100, 24));
        assert!(
            row(&draw(&a, 100, 24), 22).contains("changed since it was read"),
            "{screen}"
        );
        a.on_key(ch('j'));
        assert!(!all(&draw(&a, 100, 24)).contains("changed since"));
    }

    /// After a write the list is read again: an archived note leaves the open
    /// view and the cursor takes the row that replaced it.
    #[test]
    fn a_reload_keeps_the_cursor_in_place() {
        let mut a = app();
        a.on_key(ch('j'));
        let mut fresh = notes();
        fresh[1] = note(3, "read https://example.com", "archived", None);
        a.reload(fresh);
        assert_eq!(ids(&a), [4, 2]);
        assert_eq!(a.selected().unwrap().short_id, 2);
        a.on_key(press(KeyCode::Enter));
        a.reload(vec![note(9, "new", "open", None)]);
        assert_eq!(a.ls.mode, Mode::List, "the detail outlived its note");
    }

    /// The detail view shows the body whole: a URL longer than the line is
    /// broken across lines, never cut with an ellipsis.
    #[test]
    fn the_detail_shows_a_long_url_whole() {
        let url = format!("https://example.com/{}", "a".repeat(150));
        let mut a = App::new(
            vec![note(5, &format!("see {url} soon"), "open", None)],
            Filter::default(),
            true,
        );
        a.observe(60, 24);
        a.on_key(press(KeyCode::Enter));
        let buf = draw(&a, 60, 24);
        let joined: String = (3..10).map(|y| row(&buf, y).trim().to_string()).collect();
        assert!(joined.contains(&url), "{}", all(&buf));
        assert!(!all(&buf).contains('…'), "{}", all(&buf));
    }

    #[test]
    fn wrap_body_keeps_breaks_and_splits_only_what_cannot_fit() {
        assert_eq!(
            wrap_body("one two three\n\nfour", 9),
            ["one two", "three", "", "four"]
        );
        assert_eq!(wrap_body("ab abcdefgh", 4), ["ab", "abcd", "efgh"]);
    }

    /// Every key each table advertises changes something or asks for a
    /// write, so the bar cannot promise a key the screen ignores (D121(c)).
    #[test]
    fn every_key_in_the_tables_does_something() {
        let long: String = (0..60).map(|n| format!("line {n}\n")).collect();
        let setup = |table: &str| {
            let mut v = notes();
            v[1] = note(3, &long, "open", None);
            let mut a = App::new(v, Filter::default(), true);
            a.observe(80, 20);
            a.on_key(ch('j'));
            match table {
                "search" => {
                    a.on_key(ch('/'));
                    a.on_key(ch('e'));
                }
                "detail" => {
                    a.on_key(press(KeyCode::Enter));
                    for _ in 0..10 {
                        a.on_key(ch('j'));
                    }
                }
                "confirm" => {
                    a.on_key(ch('x'));
                }
                _ => {}
            }
            a
        };
        let key_of = |tok: &str| -> KeyEvent {
            match tok {
                "enter" => press(KeyCode::Enter),
                "esc" => press(KeyCode::Esc),
                "up" => press(KeyCode::Up),
                "down" => press(KeyCode::Down),
                "space" => ch(' '),
                "ctrl-u" => KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                t if t.chars().count() == 1 => ch(t.chars().next().unwrap()),
                t => panic!("the key table names {t:?}, which this test cannot press"),
            }
        };
        let state = |a: &App| {
            (
                a.ls.mode,
                a.ls.cursor,
                a.ls.query.clone(),
                a.ls.scroll,
                a.filter.links,
                a.confirm,
                a.echo.clone(),
            )
        };
        for (name, table) in [
            ("list", LIST_KEYS),
            ("search", tui::memory::SEARCH_KEYS),
            ("detail", DETAIL_KEYS),
            ("confirm", CONFIRM_KEYS),
        ] {
            for k in table {
                for tok in k.keys.split(" / ") {
                    let mut a = setup(name);
                    let before = state(&a);
                    let out = a.on_key(key_of(tok));
                    assert!(
                        out != Outcome::Stay || state(&a) != before,
                        "{name}: `{tok}` ({}) did nothing",
                        k.help
                    );
                }
            }
        }
    }

    #[test]
    fn the_list_names_the_question_and_draws_the_rows() {
        let a = app();
        let buf = draw(&a, 100, 24);
        assert!(row(&buf, 0).contains("3 open notes"), "{}", row(&buf, 0));
        assert!(row(&buf, 2).contains("▸ 4"), "{}", all(&buf));
        assert!(row(&buf, 3).contains('↗'), "{}", all(&buf));
        assert!(row(&buf, 23).contains("quit"), "{}", row(&buf, 23));
    }

    /// A body is sanitised on the way in (D19), asserted on what the screen
    /// keeps, since ratatui's buffer drops control characters by itself.
    #[test]
    fn a_body_is_sanitised_on_the_way_in() {
        let n = note(7, "before\x1b]0;owned\x07after", "open", None);
        assert!(!n.body.contains('\x1b') && !n.body.contains('\x07'));
        assert!(n.body.contains("before") && n.body.contains("after"));
    }
}
