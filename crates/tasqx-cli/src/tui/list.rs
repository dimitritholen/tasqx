//! The list screen the pick and memory screens share: three modes, a cursor
//! over the ranked matches, a query, a scroll offset and the terminal's size,
//! with the keys that move them. Each screen keeps what is its own (the rows,
//! what `s` or Esc does, what the detail view shows) and asks this for the
//! rest, so a key means the same thing on both.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::tui::fuzzy;

/// Which keys the screen is listening to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Moving through the list: j/k, Enter opens, `/` starts a search.
    List,
    /// Typing a query: every character is a letter, so j and q type j and q.
    Search,
    /// One item, full screen, scrolling.
    Detail,
}

/// Cursor, query and scroll state of a screen that filters a list as you type.
pub struct ListState {
    /// Indices into the screen's rows that match `query`, best first; every
    /// row, in the caller's own order, while there is no query.
    pub matches: Vec<usize>,
    /// Position within `matches`, NOT within the rows.
    pub cursor: usize,
    /// What the reader has typed after `/`.
    pub query: String,
    pub mode: Mode,
    /// The first line the detail view shows.
    pub scroll: usize,
    pub size: (u16, u16),
}

impl ListState {
    pub fn new(len: usize) -> Self {
        ListState {
            matches: (0..len).collect(),
            cursor: 0,
            query: String::new(),
            mode: Mode::List,
            scroll: 0,
            size: (80, 24),
        }
    }

    /// The row index under the cursor, or `None` when nothing matches.
    pub fn selected(&self) -> Option<usize> {
        self.matches.get(self.cursor).copied()
    }

    /// Move the cursor by `by`, clamped at both ends — never wrapped, and
    /// never a usize underflow inside a raw-mode alt screen.
    pub fn step(&mut self, by: isize) {
        let last = self.matches.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + by).clamp(0, last.max(0)) as usize;
    }

    /// Re-rank `0..len` against the query, keeping the highlight on the SAME
    /// item (see [`fuzzy::refilter`]).
    fn refilter(&mut self, len: usize, score: impl Fn(usize, &[&str]) -> Option<i64>) {
        fuzzy::refilter(len, &self.query, score, &mut self.matches, &mut self.cursor);
    }

    /// The list's keys that both screens share. `true` when the key was
    /// dealt with (a chord this screen has no use for is dealt with too, so
    /// it never types or quits); `false` leaves `s`, `q` and an Esc with
    /// nothing to undo to the screen.
    pub fn list_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        page: usize,
        len: usize,
        score: impl Fn(usize, &[&str]) -> Option<i64>,
    ) -> bool {
        let page = page as isize;
        match key.code {
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('d') if ctrl => self.step(page / 2),
            KeyCode::Char('u') if ctrl => self.step(-page / 2),
            _ if ctrl => {}
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::PageDown => self.step(page),
            KeyCode::PageUp => self.step(-page),
            KeyCode::Char('g') | KeyCode::Home => self.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.cursor = self.matches.len().saturating_sub(1);
            }
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Enter => {
                if self.selected().is_some() {
                    self.mode = Mode::Detail;
                    self.scroll = 0;
                }
            }
            // Esc undoes the search first, and only leaves once there is
            // nothing left to undo, so the key that ends a search is never the
            // key that quits the screen by surprise.
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.refilter(len, score);
            }
            _ => return false,
        }
        true
    }

    /// Every key while typing a query.
    pub fn search_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        len: usize,
        score: impl Fn(usize, &[&str]) -> Option<i64>,
    ) {
        match key.code {
            // Both keep the filter: the search is finished, not abandoned.
            // Esc in the list clears it afterwards, one press further on.
            KeyCode::Enter | KeyCode::Esc => self.mode = Mode::List,
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::Char('p') if ctrl => self.step(-1),
            // The readline pair that goes with ctrl-n/ctrl-p: clear the line,
            // and delete the word behind the cursor.
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter(len, score);
            }
            KeyCode::Char('w') if ctrl => {
                // Trailing whitespace, then the word behind it — a shell's
                // ctrl-w, so `"foo "` becomes `""` in one press.
                let cut = self
                    .query
                    .trim_end()
                    .rfind(char::is_whitespace)
                    .map_or(0, |i| i + 1);
                self.query.truncate(cut);
                self.refilter(len, score);
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter(len, score);
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.refilter(len, score);
            }
            _ => {}
        }
    }

    /// Every key on the detail view: scrolling, and the ways back. `true`
    /// when the key went back to the list.
    pub fn detail_key(&mut self, key: KeyEvent, page: usize, max: usize) -> bool {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll = (self.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll = (self.scroll + page).min(max),
            KeyCode::Char('b') | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = max,
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Left => {
                self.mode = Mode::List;
                return true;
            }
            _ => {}
        }
        false
    }
}
