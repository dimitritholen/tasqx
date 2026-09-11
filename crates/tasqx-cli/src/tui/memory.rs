//! The memory browser (D121): `tasqx memory list` on a terminal.
//!
//! A list of docs on the left and the body of the one under the cursor on the
//! right; `/` filters as you type, with the same ranking `pick` uses; Enter
//! opens the doc full-screen. The key bar on the bottom row is generated from
//! the table for the mode the screen is in, so it cannot advertise a key the
//! screen does not answer, or hide one it does.
//!
//! Same shape as every screen on the `tui` foundation (D26): a pure [`App`]
//! that folds keys into state and returns an [`Action`] for the caller to
//! carry out, and a [`render`] that decides nothing. The one thing the screen
//! cannot do for itself is read a body from the store, so it asks:
//! [`App::wanted`] names the doc it needs and the caller answers with
//! [`App::set_body`].

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style as RtStyle};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::columns::{self, Column};
use crate::render;
use crate::theme::{Caps, Theme};
use crate::tui::{first_visible, footer_spans, fuzzy, rt_style, Hint, Key};

/// Below this the screen is not drawn and `memory list` prints its table.
pub const MIN_WIDTH: u16 = 48;
pub const MIN_HEIGHT: u16 = 10;
/// Below this width there is no preview pane: the list needs about forty
/// cells to tell docs apart, and a preview narrower than that wraps every
/// sentence into a column of fragments.
pub const PREVIEW_MIN_WIDTH: u16 = 96;

/// The bonus a query term earns in each field: the title is what a doc is
/// found by, the project and source say where it belongs, and the start of
/// the body counts at all because memory titles are often slugs.
const TITLE_WEIGHT: i64 = 250;
const PROJECT_WEIGHT: i64 = 50;
const SOURCE_WEIGHT: i64 = 50;
/// A contiguous hit in the source or body scores from this, not from the 1000
/// a subsequence starts at, so any reasonable title hit outranks it.
const PROSE_BASE: i64 = 600;
/// Added when a term occurs in a title or project as a whole run rather than
/// as letters spread through it: the word the reader typed beats the same
/// letters inside another word (`tui` in "tasqx-tui-restyle" over `tui` in
/// "punctuation").
const WHOLE_BONUS: i64 = 400;

/// One memory doc as the list shows it, sanitised at construction (D19): a
/// title or a body is text an agent wrote, and none of its bytes may reach the
/// terminal as anything but text.
pub struct Doc {
    pub id: String,
    title: String,
    project: String,
    source: String,
    updated: String,
    created: String,
    summary: String,
    /// Lowercased copies of title, project, source and preview, derived from
    /// the sanitised text so a query cannot match bytes the screen never draws.
    fields: [String; 4],
}

impl Doc {
    /// How well this doc matches every one of `terms` (lowercased), or `None`.
    ///
    /// Title and project match as subsequences, `pick`'s rule (D100), so `wac`
    /// still finds "Write API conformance", and a run found whole scores
    /// above the same letters scattered. Source and body match only as a
    /// contiguous run: across 160 characters of prose almost any three
    /// letters occur in order, and on a real store `tui` matched 88 of 90
    /// docs that way.
    fn score(&self, terms: &[&str]) -> Option<i64> {
        let prose = |field: &str, t: &str, weight: i64| {
            field
                .find(t)
                .map(|at| PROSE_BASE - (at as i64).min(PROSE_BASE / 2) + weight)
        };
        let name = |field: &str, t: &str, weight: i64| {
            let whole = if field.contains(t) { WHOLE_BONUS } else { 0 };
            fuzzy::score_subsequence(field, t).map(|s| s + weight + whole)
        };
        let mut total = 0;
        for t in terms {
            let best = [
                name(&self.fields[0], t, TITLE_WEIGHT),
                name(&self.fields[1], t, PROJECT_WEIGHT),
                prose(&self.fields[2], t, SOURCE_WEIGHT),
                prose(&self.fields[3], t, 0),
            ]
            .into_iter()
            .flatten()
            .max()?;
            total += best;
        }
        Some(total)
    }

    /// `updated` and `created` arrive already spelled (`2d ago`): the screen
    /// has no clock of its own, so a redraw cannot disagree with the list the
    /// caller measured.
    pub fn new(
        id: &str,
        title: &str,
        project: Option<&str>,
        source: Option<&str>,
        updated: String,
        created: String,
        preview: &str,
    ) -> Self {
        let title = render::san(title);
        let project = render::san(project.unwrap_or(""));
        let source = render::san(source.unwrap_or(""));
        let preview = render::san(preview);
        let summary = render::doc_summary(&render::san_multiline(preview.as_str()));
        let fields = [
            title.to_lowercase(),
            project.to_lowercase(),
            source.to_lowercase(),
            preview.to_lowercase(),
        ];
        Doc {
            id: render::san(id),
            title,
            project,
            source,
            updated,
            created,
            summary,
            fields,
        }
    }
}

/// Which keys the screen is listening to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Moving through the list: j/k, Enter opens, `/` starts a search.
    List,
    /// Typing a query: every character is a letter, so j and q type j and q.
    Search,
    /// One doc, full screen, scrolling.
    Detail,
}

/// An intent for the caller. `App` performs nothing itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Quit,
}

/// The state of the screen.
pub struct App {
    docs: Vec<Doc>,
    /// Indices into `docs`, best match first; every doc, in the store's
    /// newest-first order, while there is no query.
    matches: Vec<usize>,
    cursor: usize,
    query: String,
    mode: Mode,
    /// The first body line shown in the detail view.
    scroll: usize,
    bodies: HashMap<String, String>,
    size: (u16, u16),
    unicode: bool,
}

impl App {
    pub fn new(docs: Vec<Doc>, unicode: bool) -> Self {
        let matches = (0..docs.len()).collect();
        App {
            docs,
            matches,
            cursor: 0,
            query: String::new(),
            mode: Mode::List,
            scroll: 0,
            bodies: HashMap::new(),
            size: (80, 24),
            unicode,
        }
    }

    /// The terminal's size, re-read before every frame so paging and the
    /// detail view's scroll limit follow a resize.
    pub fn observe(&mut self, width: u16, height: u16) {
        self.size = (width, height);
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn selected(&self) -> Option<&Doc> {
        self.matches.get(self.cursor).map(|&i| &self.docs[i])
    }

    /// The doc whose body the screen needs and does not have: the one under
    /// the cursor, whether the preview or the detail view is showing it.
    pub fn wanted(&self) -> Option<&str> {
        let d = self.selected()?;
        (!self.bodies.contains_key(&d.id)).then_some(d.id.as_str())
    }

    /// The body of `id`, sanitised on the way in and kept for the rest of the
    /// session, so moving back over a doc does not read it again.
    pub fn set_body(&mut self, id: &str, body: &str) {
        self.bodies
            .insert(id.to_string(), render::san_multiline(body));
    }

    /// Fold one key into the state.
    ///
    /// Only presses count. Windows reports a release for every key, and
    /// folding both in moved the cursor twice per press (the same guard
    /// `pick` and the dashboard carry).
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        match self.mode {
            Mode::List => return self.list_key(key, ctrl),
            Mode::Search => self.search_key(key, ctrl),
            Mode::Detail => self.detail_key(key),
        }
        None
    }

    fn list_key(&mut self, key: KeyEvent, ctrl: bool) -> Option<Action> {
        let page = self.list_rows() as isize;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::Char('d') if ctrl => self.step(page / 2),
            KeyCode::Char('u') if ctrl => self.step(-page / 2),
            KeyCode::PageDown => self.step(page),
            KeyCode::PageUp => self.step(-page),
            KeyCode::Char('g') | KeyCode::Home => self.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.cursor = self.matches.len().saturating_sub(1);
            }
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Enter if self.selected().is_some() => {
                self.mode = Mode::Detail;
                self.scroll = 0;
            }
            // Esc undoes the search first, and only leaves once there is
            // nothing left to undo, so the key that ends a search is never
            // the key that quits the screen by surprise.
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Esc | KeyCode::Char('q') => return Some(Action::Quit),
            _ => {}
        }
        None
    }

    fn search_key(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            // Both keep the filter: the search is finished, not abandoned.
            // Esc in the list clears it afterwards, one press further on.
            KeyCode::Enter | KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char('w') if ctrl => {
                let kept = self.query.trim_end().rfind(' ').map_or(0, |i| i + 1);
                self.query.truncate(kept);
                self.refilter();
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
    }

    fn detail_key(&mut self, key: KeyEvent) {
        let page = self.detail_rows();
        let max = self.max_scroll();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll = (self.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll = (self.scroll + page).min(max),
            KeyCode::Char('b') | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = max,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Left => {
                self.mode = Mode::List;
            }
            _ => {}
        }
    }

    fn step(&mut self, by: isize) {
        let last = self.matches.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + by).clamp(0, last.max(0)) as usize;
    }

    /// Re-rank against the query, keeping the cursor on the doc it was on.
    ///
    /// `pick` learned this the hard way: re-filtering under a cursor index
    /// leaves the index pointing at whatever doc now sits there, and Enter
    /// opens a doc the reader never highlighted. The doc is found again, and
    /// only when it has left the matches does the cursor fall back to the top.
    fn refilter(&mut self) {
        let anchor = self.matches.get(self.cursor).copied();
        let needle = self.query.to_lowercase();
        let terms: Vec<&str> = needle.split_whitespace().collect();
        self.matches = if terms.is_empty() {
            (0..self.docs.len()).collect()
        } else {
            fuzzy::rank(self.docs.len(), |i| self.docs[i].score(&terms))
        };
        self.cursor = anchor
            .and_then(|a| self.matches.iter().position(|&i| i == a))
            .unwrap_or(0)
            .min(self.matches.len().saturating_sub(1));
    }

    /// Rows the list gets: the frame less the header (which carries the
    /// query), the blank line under it, the blank line above the key bar, and
    /// the key bar.
    fn list_rows(&self) -> usize {
        (self.size.1 as usize).saturating_sub(4).max(1)
    }

    /// Rows the detail view's body gets: the frame less the title, the two
    /// fact lines, the id line, a blank line, and the blank line and key bar
    /// at the bottom.
    fn detail_rows(&self) -> usize {
        (self.size.1 as usize).saturating_sub(7).max(1)
    }

    fn detail_width(&self) -> usize {
        (self.size.0 as usize).saturating_sub(2).max(8)
    }

    fn body_lines(&self, width: usize) -> Vec<DocLine> {
        self.selected()
            .and_then(|d| self.bodies.get(&d.id))
            .map(|b| doc_lines(b, width, self.unicode))
            .unwrap_or_default()
    }

    fn max_scroll(&self) -> usize {
        if self.mode != Mode::Detail {
            return 0;
        }
        self.body_lines(self.detail_width())
            .len()
            .saturating_sub(self.detail_rows())
    }
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
            rank: 1,
        }),
    },
    Key {
        keys: "/",
        help: "search titles, projects, sources and the start of each body",
        footer: Some(Hint {
            keys: "/",
            word: "search",
            rank: 0,
        }),
    },
    Key {
        keys: "enter",
        help: "open the doc under the cursor",
        footer: Some(Hint {
            keys: "enter",
            word: "open",
            rank: 0,
        }),
    },
    Key {
        keys: "g / G",
        help: "first / last doc",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 3,
        }),
    },
    Key {
        keys: "esc",
        help: "clear the search; with none, leave",
        footer: Some(Hint {
            keys: "esc",
            word: "clear",
            rank: 2,
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

/// The search line's keys: every other character is part of the query.
pub const SEARCH_KEYS: &[Key] = &[
    Key {
        keys: "enter / esc",
        help: "keep the filter and go back to moving",
        footer: Some(Hint {
            keys: "enter",
            word: "done",
            rank: 0,
        }),
    },
    Key {
        keys: "up / down",
        help: "move while typing",
        footer: Some(Hint {
            keys: "↑↓",
            word: "move",
            rank: 1,
        }),
    },
    Key {
        keys: "ctrl-u",
        help: "clear the query",
        footer: Some(Hint {
            keys: "ctrl-u",
            word: "clear",
            rank: 2,
        }),
    },
];

/// The detail view's keys.
pub const DETAIL_KEYS: &[Key] = &[
    Key {
        keys: "j / k",
        help: "scroll down / up",
        footer: Some(Hint {
            keys: "j/k",
            word: "scroll",
            rank: 1,
        }),
    },
    Key {
        keys: "space / b",
        help: "a page down / up",
        footer: Some(Hint {
            keys: "space",
            word: "page",
            rank: 2,
        }),
    },
    Key {
        keys: "g / G",
        help: "top / bottom",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 3,
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

fn keys_for(mode: Mode) -> &'static [Key] {
    match mode {
        Mode::List => LIST_KEYS,
        Mode::Search => SEARCH_KEYS,
        Mode::Detail => DETAIL_KEYS,
    }
}

// ============================================================================
// A doc body, as lines
// ============================================================================

/// What a run of body text is, so `render` can give it a role.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ink {
    Text,
    Heading,
    /// A list bullet, or a frontmatter key.
    Label,
    Code,
    Strong,
}

pub type DocLine = Vec<(Ink, String)>;

/// A body laid out in lines of at most `width` cells: markdown read lightly,
/// never printed as its own syntax.
///
/// Frontmatter becomes `key  value` lines, which is how imported agent
/// memories read instead of opening on `---` and `name:`. Headings are bold,
/// bullets hang, fenced code keeps its lines, `code` and `**strong**` lose
/// their markers and keep their emphasis. Anything else wraps as prose. It is
/// deliberately not a markdown parser: a memory doc is notes, and the goal is
/// that its syntax stops being noise, not that every construct renders.
pub fn doc_lines(body: &str, width: usize, unicode: bool) -> Vec<DocLine> {
    let width = width.max(8);
    let mut out: Vec<DocLine> = Vec::new();
    let mut lines = body.lines().peekable();

    if lines.peek().is_some_and(|l| l.trim() == "---") {
        let rest: Vec<&str> = lines.clone().skip(1).collect();
        if let Some(end) = rest.iter().position(|l| l.trim() == "---") {
            let key_w = rest[..end]
                .iter()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, _)| render::width(k.trim()))
                .max()
                .unwrap_or(0);
            for l in &rest[..end] {
                if let Some((k, v)) = l.split_once(':') {
                    let key = render::pad(k.trim(), key_w + 2);
                    let value = v.trim().trim_matches('"');
                    let words = words_of(&[(Ink::Text, value.to_string())]);
                    wrap(
                        &mut out,
                        words,
                        width,
                        (Ink::Label, key.clone()),
                        " ".repeat(key_w + 2),
                        unicode,
                    );
                }
            }
            out.push(Vec::new());
            for _ in 0..end + 2 {
                lines.next();
            }
        }
    }

    // Consecutive lines are one block, as in markdown: a note wrapped at 80
    // columns in its source is still one paragraph, and a bullet's second
    // source line continues the bullet. Rendered line by line, the second
    // line of every bullet sat flush against the margin.
    enum Block {
        Para(String),
        Bullet(String),
    }
    let marker = if unicode { "• " } else { "- " };
    let flush = |out: &mut Vec<DocLine>, block: &mut Option<Block>| match block.take() {
        Some(Block::Para(t)) => wrap(
            out,
            words_of(&inline(&t)),
            width,
            (Ink::Text, String::new()),
            String::new(),
            unicode,
        ),
        Some(Block::Bullet(t)) => wrap(
            out,
            words_of(&inline(&t)),
            width,
            (Ink::Label, marker.to_string()),
            "  ".to_string(),
            unicode,
        ),
        None => {}
    };
    let mut block: Option<Block> = None;
    let mut fence = false;
    let mut blank = true;
    for raw in lines {
        let trimmed = raw.trim();
        if trimmed.starts_with("```") {
            flush(&mut out, &mut block);
            fence = !fence;
            continue;
        }
        if fence {
            out.push(vec![(
                Ink::Code,
                render::truncate(raw.trim_end(), width, unicode),
            )]);
            blank = false;
            continue;
        }
        if trimmed.is_empty() || trimmed == "---" || trimmed == "***" {
            flush(&mut out, &mut block);
            if !blank {
                out.push(Vec::new());
                blank = true;
            }
            continue;
        }
        blank = false;
        if trimmed.starts_with('#') {
            flush(&mut out, &mut block);
            let text = trimmed.trim_start_matches('#').trim();
            let words = words_of(&[(Ink::Heading, text.to_string())]);
            wrap(
                &mut out,
                words,
                width,
                (Ink::Heading, String::new()),
                String::new(),
                unicode,
            );
        } else if let Some(rest) = bullet(trimmed) {
            flush(&mut out, &mut block);
            block = Some(Block::Bullet(rest.to_string()));
        } else {
            // A bullet continues only on an indented line; a line back at
            // the margin ends it and opens a paragraph.
            let indented = raw.starts_with(char::is_whitespace);
            match &mut block {
                Some(Block::Bullet(t)) if indented => {
                    t.push(' ');
                    t.push_str(trimmed);
                }
                Some(Block::Para(t)) => {
                    t.push(' ');
                    t.push_str(trimmed);
                }
                _ => {
                    flush(&mut out, &mut block);
                    block = Some(Block::Para(trimmed.to_string()));
                }
            }
        }
    }
    flush(&mut out, &mut block);
    while out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

/// The text after a list marker (`- `, `* `, `+ `, `1. `), or `None`.
fn bullet(line: &str) -> Option<&str> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(m) {
            return Some(rest);
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        return line[digits..].strip_prefix(". ");
    }
    None
}

/// `code` and `**strong**` spans, without their markers.
fn inline(text: &str) -> Vec<(Ink, String)> {
    let mut out = Vec::new();
    for (i, part) in text.split('`').enumerate() {
        if i % 2 == 1 {
            out.push((Ink::Code, part.to_string()));
            continue;
        }
        for (j, piece) in part.split("**").enumerate() {
            let ink = if j % 2 == 1 { Ink::Strong } else { Ink::Text };
            out.push((ink, piece.to_string()));
        }
    }
    out
}

/// A word is one or more styled pieces with no whitespace between them, so
/// `` `d9a534c`. `` stays `d9a534c.` rather than gaining a space before the
/// full stop where the code span ends.
type Word = Vec<(Ink, String)>;

fn words_of(spans: &[(Ink, String)]) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    // Whether the previous span ended mid-word, so this one continues it.
    let mut glued = false;
    for (ink, text) in spans {
        let opens_mid_word = glued && !text.starts_with(char::is_whitespace);
        for (n, piece) in text.split_whitespace().enumerate() {
            match words.last_mut() {
                Some(last) if n == 0 && opens_mid_word => last.push((*ink, piece.to_string())),
                _ => words.push(vec![(*ink, piece.to_string())]),
            }
        }
        if !text.trim().is_empty() {
            glued = !text.ends_with(char::is_whitespace);
        } else if !text.is_empty() {
            glued = false;
        }
    }
    words
}

/// Greedy word wrap of styled words, the first line opened by `lead` and the
/// rest by `indent`, so a bullet's continuation lines hang under its text.
fn wrap(
    out: &mut Vec<DocLine>,
    words: Vec<Word>,
    width: usize,
    lead: (Ink, String),
    indent: String,
    unicode: bool,
) {
    let mut line: DocLine = vec![lead.clone()];
    let mut used = render::width(&lead.1);
    let mut empty = true;
    for word in words {
        let w: usize = word.iter().map(|(_, t)| render::width(t)).sum();
        if !empty && used + 1 + w > width {
            out.push(std::mem::take(&mut line));
            line.push((Ink::Text, indent.clone()));
            used = render::width(&indent);
            empty = true;
        }
        if !empty {
            line.push((Ink::Text, " ".to_string()));
            used += 1;
        }
        if w > width.saturating_sub(used) {
            // Longer than a whole line: cut, in the ink it opens with.
            let text: String = word.iter().map(|(_, t)| t.as_str()).collect();
            let cut = render::truncate(&text, width.saturating_sub(used), unicode);
            used += render::width(&cut);
            line.push((word[0].0, cut));
        } else {
            used += w;
            line.extend(word);
        }
        empty = false;
    }
    out.push(line);
}

// ============================================================================
// Rendering
// ============================================================================

/// Draw the screen. Decides nothing: `App` made every choice.
pub fn render(app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let sty = |role: &str| rt_style(theme.role(role), caps);
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return;
    }
    match app.mode {
        Mode::Detail => draw_detail(app, &sty, frame, area),
        Mode::List | Mode::Search => draw_list(app, &sty, frame, area),
    }

    // The key bar: always the bottom row, always the table for the mode on
    // screen. A bar that said `j/k move` while j was typing into the query
    // would be describing a different screen.
    let foot = Rect {
        y: area.bottom() - 1,
        height: 1,
        ..area
    };
    let mut spans = vec![Span::raw(" ")];
    spans.extend(footer_spans(
        keys_for(app.mode),
        area.width.saturating_sub(1),
        sty("accent"),
        sty("muted"),
    ));
    if app.mode == Mode::Detail {
        let total = app.body_lines(app.detail_width()).len();
        let rows = app.detail_rows();
        if total > rows {
            let used: usize = spans.iter().map(|s| render::width(&s.content)).sum();
            let from = app.scroll + 1;
            let to = (app.scroll + rows).min(total);
            let dash = if app.unicode { "–" } else { "-" };
            let pos = format!("{from}{dash}{to} of {total} ");
            let gap = (area.width as usize).saturating_sub(used + render::width(&pos));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(pos, sty("muted")));
        }
    }
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

fn ink_style(ink: Ink, sty: &dyn Fn(&str) -> RtStyle) -> RtStyle {
    match ink {
        Ink::Text => RtStyle::default(),
        Ink::Heading | Ink::Strong => RtStyle::default().add_modifier(Modifier::BOLD),
        Ink::Label => sty("table.label"),
        Ink::Code => sty("accent"),
    }
}

fn doc_spans(line: &DocLine, sty: &dyn Fn(&str) -> RtStyle) -> Vec<Span<'static>> {
    line.iter()
        .map(|(ink, text)| Span::styled(text.clone(), ink_style(*ink, sty)))
        .collect()
}

/// The facts line under a title: project, then when, printing only what the
/// doc has.
fn facts(doc: &Doc, created: bool, mid: &str, sty: &dyn Fn(&str) -> RtStyle) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if !doc.project.is_empty() {
        spans.push(Span::styled(doc.project.clone(), sty("project")));
        spans.push(Span::styled(format!("  {mid}  "), sty("muted")));
    }
    spans.push(Span::styled(
        format!("updated {}", doc.updated),
        sty("table.label"),
    ));
    if created && doc.created != doc.updated {
        spans.push(Span::styled(
            format!("  {mid}  created {}", doc.created),
            sty("table.label"),
        ));
    }
    spans
}

fn draw_list(app: &App, sty: &dyn Fn(&str) -> RtStyle, frame: &mut Frame, area: Rect) {
    let mid = if app.unicode { "·" } else { "-" };
    let (w, x0, y0) = (area.width as usize, area.x, area.y);

    // One line: the name of the screen, then either what the list holds or
    // the query and how much of the list it kept (rule 8: say what the reader
    // cannot count, and only the facts that are there). The query lives here
    // rather than on a line of its own, so the list does not move when a
    // search opens and does not sit under empty rows when none is open.
    let total = app.docs.len();
    let mut head = vec![
        Span::raw(" "),
        Span::styled("memory".to_string(), sty("header")),
        Span::raw("   "),
    ];
    if app.mode == Mode::Search || !app.query.is_empty() {
        let searching = app.mode == Mode::Search;
        head.push(Span::styled(
            "/ ".to_string(),
            sty(if searching { "accent" } else { "muted" }),
        ));
        head.push(Span::raw(app.query.clone()));
        if searching {
            head.push(Span::styled(
                if app.unicode { "▏" } else { "_" }.to_string(),
                sty("accent"),
            ));
        }
        head.push(Span::styled(
            format!("   {} of {total} match", app.matches.len()),
            sty("table.label"),
        ));
    } else {
        let mut projects: Vec<&str> = app
            .docs
            .iter()
            .map(|d| d.project.as_str())
            .filter(|p| !p.is_empty())
            .collect();
        projects.sort_unstable();
        projects.dedup();
        let mut facts = vec![format!(
            "{total} {}",
            if total == 1 { "doc" } else { "docs" }
        )];
        if !projects.is_empty() {
            let n = projects.len();
            facts.push(format!(
                "{n} {}",
                if n == 1 { "project" } else { "projects" }
            ));
        }
        if let Some(first) = app.docs.first() {
            facts.push(format!("newest {}", first.updated));
        }
        head.push(Span::styled(
            facts.join(&format!(" {mid} ")),
            sty("table.label"),
        ));
    }
    line_at(frame, area, y0, x0, head);

    let rows = app.list_rows();
    let top = y0 + 2;
    let preview = area.width >= PREVIEW_MIN_WIDTH;
    let left = if preview {
        (w * 45 / 100).clamp(40, 72)
    } else {
        w
    };

    if app.docs.is_empty() || app.matches.is_empty() {
        let msg = if app.docs.is_empty() {
            "No memory docs yet. `tasqx memory add <title> <body>` stores one.".to_string()
        } else {
            format!("Nothing matches {:?}. Esc clears the search.", app.query)
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

    // Three cells of lead: a margin, the cursor, a space.
    let budget = left.saturating_sub(4);
    let updated_w = app
        .docs
        .iter()
        .map(|d| render::width(&d.updated))
        .max()
        .unwrap_or(0);
    let project_w = if preview {
        0
    } else {
        app.docs
            .iter()
            .map(|d| render::width(&d.project))
            .max()
            .unwrap_or(0)
    };
    let title_w = app
        .docs
        .iter()
        .map(|d| render::width(&d.title))
        .max()
        .unwrap_or(0);
    let cw = columns::fit(
        &[
            Column::shrinks(title_w, 12),
            Column::drops(project_w, 8),
            Column::fixed(updated_w),
        ],
        budget,
    );

    let first = first_visible(app.cursor, app.matches.len(), rows);
    for (n, &i) in app.matches.iter().enumerate().skip(first).take(rows) {
        let d = &app.docs[i];
        let on = n == app.cursor;
        let mark = match (on, app.unicode) {
            (true, true) => "▸",
            (true, false) => ">",
            _ => " ",
        };
        let title = render::pad(&render::truncate(&d.title, cw[0], app.unicode), cw[0]);
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(mark.to_string(), sty("accent")),
            Span::raw(" "),
            Span::styled(
                title,
                if on {
                    sty("accent").add_modifier(Modifier::BOLD)
                } else {
                    RtStyle::default()
                },
            ),
        ];
        if cw[1] > 0 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                render::pad(&render::truncate(&d.project, cw[1], app.unicode), cw[1]),
                sty("project"),
            ));
        }
        spans.push(Span::raw("  "));
        spans.push(Span::styled(d.updated.clone(), sty("table.label")));
        line_at(frame, area, top + (n - first) as u16, x0, spans);
    }

    if !preview {
        return;
    }
    let Some(doc) = app.selected() else { return };
    let px = x0 + left as u16 + 1;
    let pw = w.saturating_sub(left + 4);
    let rule = if app.unicode { "│" } else { "|" };
    for r in 0..rows as u16 {
        line_at(
            frame,
            area,
            top + r,
            px,
            vec![Span::styled(rule.to_string(), sty("muted"))],
        );
    }
    let cx = px + 3;
    let mut lines: Vec<Vec<Span<'static>>> = vec![
        vec![Span::styled(
            render::truncate(&doc.title, pw, app.unicode),
            RtStyle::default().add_modifier(Modifier::BOLD),
        )],
        facts(doc, false, mid, sty),
        Vec::new(),
    ];
    match app.bodies.get(&doc.id) {
        Some(body) => {
            lines.extend(
                doc_lines(body, pw, app.unicode)
                    .iter()
                    .map(|l| doc_spans(l, sty)),
            );
        }
        None => lines.push(vec![Span::styled(doc.summary.clone(), sty("muted"))]),
    }
    for (r, spans) in lines.into_iter().take(rows).enumerate() {
        line_at(frame, area, top + r as u16, cx, spans);
    }
}

fn draw_detail(app: &App, sty: &dyn Fn(&str) -> RtStyle, frame: &mut Frame, area: Rect) {
    let Some(doc) = app.selected() else { return };
    let mid = if app.unicode { "·" } else { "-" };
    let (x0, y0) = (area.x, area.y);
    let width = app.detail_width();
    line_at(
        frame,
        area,
        y0,
        x0,
        vec![
            Span::raw(" "),
            Span::styled(
                render::truncate(&doc.title, width, app.unicode),
                sty("header"),
            ),
        ],
    );
    let mut fact_line = vec![Span::raw(" ")];
    fact_line.extend(facts(doc, true, mid, sty));
    line_at(frame, area, y0 + 1, x0, fact_line);
    let label = |text: &str| Span::styled(render::pad(text, 8), sty("table.label"));
    if !doc.source.is_empty() {
        line_at(
            frame,
            area,
            y0 + 2,
            x0,
            vec![
                Span::raw(" "),
                label("source"),
                Span::styled(
                    render::truncate(&doc.source, width.saturating_sub(8), app.unicode),
                    sty("muted"),
                ),
            ],
        );
    }
    line_at(
        frame,
        area,
        y0 + 3,
        x0,
        vec![
            Span::raw(" "),
            label("id"),
            Span::styled(doc.id.clone(), sty("muted")),
        ],
    );

    let rows = app.detail_rows();
    let top = y0 + 5;
    let lines = app.body_lines(width);
    for (r, line) in lines.iter().skip(app.scroll).take(rows).enumerate() {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(doc_spans(line, sty));
        line_at(frame, area, top + r as u16, x0, spans);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn doc(id: &str, title: &str, project: Option<&str>, preview: &str) -> Doc {
        Doc::new(
            id,
            title,
            project,
            Some("docs/notes.md"),
            "2d ago".to_string(),
            "9 Sep".to_string(),
            preview,
        )
    }

    fn app() -> App {
        let mut a = App::new(
            vec![
                doc(
                    "a",
                    "Deploy checklist",
                    Some("infra"),
                    "Run the smoke tests first.",
                ),
                doc(
                    "b",
                    "Release notes style",
                    Some("website"),
                    "Write them for users.",
                ),
                doc(
                    "c",
                    "qore-subscription-create",
                    None,
                    "How to create a webhook deploy.",
                ),
                doc(
                    "d",
                    "Oncall handbook",
                    Some("infra"),
                    "Who to page, and when.",
                ),
            ],
            true,
        );
        a.observe(120, 30);
        a
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(a: &mut App, text: &str) {
        for c in text.chars() {
            a.on_key(press(KeyCode::Char(c)));
        }
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

    #[test]
    fn j_k_and_the_arrows_move_and_stop_at_the_ends() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('k')));
        assert_eq!(a.selected().unwrap().id, "a", "k at the top stays put");
        a.on_key(press(KeyCode::Char('j')));
        a.on_key(press(KeyCode::Down));
        assert_eq!(a.selected().unwrap().id, "c");
        a.on_key(press(KeyCode::Char('G')));
        a.on_key(press(KeyCode::Char('j')));
        assert_eq!(a.selected().unwrap().id, "d", "j at the bottom stays put");
        a.on_key(press(KeyCode::Up));
        assert_eq!(a.selected().unwrap().id, "c");
    }

    /// `/` is the only way into the query, and once there every letter is a
    /// letter: `j` types j and `q` types q rather than moving or quitting.
    #[test]
    fn slash_opens_the_search_where_j_and_q_are_letters() {
        let mut a = app();
        assert_eq!(a.on_key(press(KeyCode::Char('/'))), None);
        assert_eq!(a.mode, Mode::Search);
        typed(&mut a, "jq");
        assert_eq!(a.query, "jq");
        assert_eq!(a.mode, Mode::Search, "q must not leave the search");
    }

    #[test]
    fn enter_or_esc_leaves_the_search_with_the_filter_kept_and_esc_again_clears_it() {
        for leave in [KeyCode::Enter, KeyCode::Esc] {
            let mut a = app();
            a.on_key(press(KeyCode::Char('/')));
            typed(&mut a, "deploy");
            a.on_key(press(leave));
            assert_eq!(a.mode, Mode::List);
            assert_eq!(a.query, "deploy", "{leave:?} dropped the filter");
            assert_eq!(a.matches.len(), 2);
            assert_eq!(a.on_key(press(KeyCode::Esc)), None, "the first esc clears");
            assert_eq!(a.query, "");
            assert_eq!(a.matches.len(), 4);
            assert_eq!(
                a.on_key(press(KeyCode::Esc)),
                Some(Action::Quit),
                "the next leaves"
            );
        }
    }

    /// The same failure `pick` shipped once: a re-filter under a cursor index
    /// leaves the index on whatever doc now sits there.
    ///
    /// The doc under the cursor must land somewhere other than the top of the
    /// new ranking, or a cursor that simply resets to 0 passes too. The first
    /// fixture had that flaw: once a whole-word hit earned a bonus, the doc
    /// under the cursor also ranked first, and the guard went silent.
    #[test]
    fn the_cursor_keeps_its_doc_through_a_refilter() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('G')));
        a.on_key(press(KeyCode::Char('k')));
        assert_eq!(a.selected().unwrap().id, "c");
        a.on_key(press(KeyCode::Char('/')));
        typed(&mut a, "s");
        assert_ne!(
            a.matches[0], 2,
            "the fixture no longer moves the doc off the top"
        );
        assert_eq!(
            a.selected().unwrap().id,
            "c",
            "the cursor jumped to another doc"
        );
    }

    /// A doc named by the query outranks one that merely mentions it: the
    /// title's weight is what `pick` gives the thing with a name.
    #[test]
    fn a_title_hit_outranks_a_preview_hit() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('/')));
        typed(&mut a, "deploy");
        let ids: Vec<&str> = a.matches.iter().map(|&i| a.docs[i].id.as_str()).collect();
        assert_eq!(ids, ["a", "c"], "the title hit must come first");
    }

    #[test]
    fn enter_opens_the_doc_under_the_cursor_and_esc_returns_to_it() {
        let mut a = app();
        a.on_key(press(KeyCode::Char('j')));
        a.on_key(press(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        assert_eq!(a.selected().unwrap().id, "b");
        a.on_key(press(KeyCode::Char('q')));
        assert_eq!(a.mode, Mode::List, "q in the detail goes back, not out");
        assert_eq!(a.selected().unwrap().id, "b");
    }

    #[test]
    fn the_body_under_the_cursor_is_asked_for_once() {
        let mut a = app();
        assert_eq!(a.wanted(), Some("a"));
        a.set_body("a", "Run the smoke tests first.");
        assert_eq!(a.wanted(), None);
        a.on_key(press(KeyCode::Char('j')));
        assert_eq!(a.wanted(), Some("b"));
        a.on_key(press(KeyCode::Char('k')));
        assert_eq!(a.wanted(), None, "a body already read is not read again");
    }

    /// Every key each table advertises changes something, so the bar cannot
    /// promise a key the screen ignores. The docs-drift idiom again, applied
    /// to a key table.
    #[test]
    fn every_key_in_the_tables_does_something() {
        let long: String = (0..80).map(|n| format!("paragraph {n}\n\n")).collect();
        let setup = |mode: Mode| {
            let mut a = app();
            a.set_body("b", &long);
            a.on_key(press(KeyCode::Char('j')));
            a.on_key(press(KeyCode::Char('j')));
            a.on_key(press(KeyCode::Char('k')));
            match mode {
                Mode::List => {}
                Mode::Search => {
                    a.on_key(press(KeyCode::Char('/')));
                    typed(&mut a, "e");
                }
                Mode::Detail => {
                    a.on_key(press(KeyCode::Enter));
                    a.observe(80, 20);
                    for _ in 0..10 {
                        a.on_key(press(KeyCode::Char('j')));
                    }
                }
            }
            a
        };
        let key_of = |tok: &str| -> KeyEvent {
            match tok {
                "enter" => press(KeyCode::Enter),
                "esc" => press(KeyCode::Esc),
                "up" => press(KeyCode::Up),
                "down" => press(KeyCode::Down),
                "space" => press(KeyCode::Char(' ')),
                "ctrl-u" => KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                t if t.chars().count() == 1 => press(KeyCode::Char(t.chars().next().unwrap())),
                t => panic!("the key table names {t:?}, which this test cannot press"),
            }
        };
        let state = |a: &App| (a.mode, a.cursor, a.query.clone(), a.scroll);
        for (mode, table) in [
            (Mode::List, LIST_KEYS),
            (Mode::Search, SEARCH_KEYS),
            (Mode::Detail, DETAIL_KEYS),
        ] {
            for k in table {
                for tok in k.keys.split(" / ") {
                    let mut a = setup(mode);
                    let before = state(&a);
                    let action = a.on_key(key_of(tok));
                    assert!(
                        action.is_some() || state(&a) != before,
                        "{mode:?}: `{tok}` ({}) did nothing",
                        k.help
                    );
                }
            }
        }
    }

    #[test]
    fn the_key_bar_is_the_bottom_row_in_every_mode() {
        let mut a = app();
        let last = |a: &App| row(&draw(a, 120, 30), 29);
        assert!(last(&a).contains("quit"), "{}", last(&a));
        a.on_key(press(KeyCode::Char('/')));
        assert!(last(&a).contains("done"), "{}", last(&a));
        assert!(
            !last(&a).contains("quit"),
            "the search bar advertised the list's q"
        );
        a.on_key(press(KeyCode::Enter));
        a.on_key(press(KeyCode::Enter));
        assert!(last(&a).contains("back"), "{}", last(&a));
    }

    #[test]
    fn the_preview_shows_the_body_under_the_cursor_and_follows_it() {
        let mut a = app();
        a.set_body("a", "Run the smoke tests first.");
        a.set_body("b", "Write them for users.");
        assert!(all(&draw(&a, 120, 30)).contains("Run the smoke tests"));
        a.on_key(press(KeyCode::Char('j')));
        let screen = all(&draw(&a, 120, 30));
        assert!(screen.contains("Write them for users"), "{screen}");
        assert!(!screen.contains("Run the smoke tests"), "{screen}");
    }

    /// Under the preview's width the list keeps the terminal, with the
    /// project column it gave up to the preview back.
    #[test]
    fn a_narrow_terminal_keeps_the_list_and_drops_the_preview() {
        let mut a = app();
        a.set_body("a", "Run the smoke tests first.");
        a.observe(80, 24);
        let screen = all(&draw(&a, 80, 24));
        assert!(
            !screen.contains('│'),
            "a preview rule at 80 columns:\n{screen}"
        );
        assert!(!screen.contains("Run the smoke tests"), "{screen}");
        assert!(
            screen.contains("infra"),
            "no project column without a preview:\n{screen}"
        );
    }

    #[test]
    fn frontmatter_is_read_as_fields_not_printed_as_dashes() {
        let lines = doc_lines(
            "---\nname: vh-mcp\ndescription: \"The MCP server standard\"\n---\n# Why\nBecause.",
            60,
            true,
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.iter().map(|(_, t)| t.as_str()).collect())
            .collect();
        assert!(!text.iter().any(|l| l.trim() == "---"), "{text:?}");
        assert!(
            text[0].starts_with("name") && text[0].contains("vh-mcp"),
            "{text:?}"
        );
        assert!(
            text[1].contains("The MCP server standard") && !text[1].contains('"'),
            "{text:?}"
        );
        assert!(lines
            .iter()
            .any(|l| l.iter().any(|(ink, t)| *ink == Ink::Heading && t == "Why")));
    }

    #[test]
    fn a_bullet_hangs_and_its_markers_are_read_not_printed() {
        let lines = doc_lines(
            "- a **strong** point with `code` that runs long enough to wrap onto a second line",
            32,
            true,
        );
        assert!(lines.len() >= 2, "{lines:?}");
        assert_eq!(lines[0][0], (Ink::Label, "• ".to_string()));
        assert_eq!(
            lines[1][0],
            (Ink::Text, "  ".to_string()),
            "the second line does not hang"
        );
        let flat: String = lines.iter().flatten().map(|(_, t)| t.as_str()).collect();
        assert!(!flat.contains("**") && !flat.contains('`'), "{flat}");
        assert!(lines
            .iter()
            .flatten()
            .any(|(ink, t)| *ink == Ink::Strong && t == "strong"));
        assert!(lines
            .iter()
            .flatten()
            .any(|(ink, t)| *ink == Ink::Code && t == "code"));
    }

    /// Letters scattered through a body are not a match. Subsequence matching
    /// is right for a title, where `wac` should find "Write API conformance",
    /// and wrong for 160 characters of prose, where almost any three letters
    /// appear in order somewhere: rendered on a real store, `tui` matched 88
    /// of 90 docs and put unrelated slugs first.
    #[test]
    fn letters_scattered_through_a_body_are_not_a_match() {
        let mut a = App::new(
            vec![
                doc("x", "Quarterly numbers", None, "the quick uncle is right"),
                doc("y", "Oncall", None, "tui screens, mostly"),
                doc("z", "tasqx-tui-restyle", None, "nothing here"),
            ],
            true,
        );
        a.on_key(press(KeyCode::Char('/')));
        typed(&mut a, "tui");
        let ids: Vec<&str> = a.matches.iter().map(|&i| a.docs[i].id.as_str()).collect();
        assert_eq!(
            ids,
            ["z", "y"],
            "a scattered body hit matched, or outranked a title"
        );
    }

    /// The query as a word beats the query as scattered letters. Rendered on a
    /// real store, `tui` put "feedback-punctuation-variety" (t…u…i inside
    /// "punctuation") above the doc filed under `tasqx-tui-restyle`, where
    /// the three letters are the word the reader typed.
    #[test]
    fn a_query_found_whole_outranks_its_letters_scattered() {
        let mut a = App::new(
            vec![
                doc(
                    "x",
                    "feedback-punctuation-variety",
                    None,
                    "Vary the punctuation.",
                ),
                doc(
                    "y",
                    "House style",
                    Some("tasqx-tui-restyle"),
                    "Rules for screens.",
                ),
            ],
            true,
        );
        a.on_key(press(KeyCode::Char('/')));
        typed(&mut a, "tui");
        let ids: Vec<&str> = a.matches.iter().map(|&i| a.docs[i].id.as_str()).collect();
        assert_eq!(
            ids.first(),
            Some(&"y"),
            "scattered letters outranked the word: {ids:?}"
        );
    }

    /// Markdown wrapped at 80 columns in its source is still one bullet, one
    /// paragraph. Rendered on a real handoff doc, every source line started a
    /// new paragraph, so a bullet's second line sat flush against the margin.
    #[test]
    fn a_soft_wrapped_bullet_is_one_bullet_and_hangs() {
        let lines = doc_lines(
            "- first half of a point
  and the rest of it
Next para.",
            80,
            true,
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.iter().map(|(_, t)| t.as_str()).collect())
            .collect();
        assert_eq!(
            text[0], "• first half of a point and the rest of it",
            "{text:?}"
        );
        assert!(text.len() >= 2 && text[1].contains("Next para"), "{text:?}");
    }

    /// Code next to punctuation keeps no space: `` `d9a534c`. `` is
    /// `d9a534c.`, not `d9a534c .`.
    #[test]
    fn code_next_to_punctuation_gains_no_space() {
        let lines = doc_lines("the binary is `d9a534c`. Run `list`: done", 80, true);
        let flat: String = lines[0].iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(flat, "the binary is d9a534c. Run list: done");
    }

    /// The query sits on the header line, so the list starts one blank row
    /// under it whether or not a search is open. With its own line the list
    /// sat under three blank rows whenever there was no search.
    #[test]
    fn the_list_starts_under_the_header_and_the_query_sits_on_it() {
        let mut a = app();
        let buf = draw(&a, 120, 30);
        assert!(row(&buf, 2).contains("Deploy checklist"), "{}", all(&buf));
        a.on_key(press(KeyCode::Char('/')));
        typed(&mut a, "dep");
        let buf = draw(&a, 120, 30);
        assert!(row(&buf, 0).contains("/ dep"), "{}", row(&buf, 0));
        assert!(row(&buf, 2).contains("Deploy checklist"), "{}", all(&buf));
    }

    /// A body is text an agent wrote, and D19's rule holds on this screen as
    /// on every other: it is sanitised on the way in.
    ///
    /// Asserted on the stored body, not on the frame. The first version
    /// checked the drawn buffer for ESC and stayed green with the sanitiser
    /// removed: ratatui's buffer drops control characters itself, so the frame
    /// could not tell the two apart. The guard has to read what this code
    /// keeps.
    #[test]
    fn a_body_is_sanitised_on_the_way_in() {
        let mut a = app();
        a.set_body("a", "before\x1b]0;owned\x07after\x1b[2J");
        let kept = &a.bodies["a"];
        assert!(!kept.contains('\x1b') && !kept.contains('\x07'), "{kept:?}");
        assert!(
            kept.contains("before") && kept.contains("after"),
            "{kept:?}"
        );
    }
}
