//! The task browser: `tasqx pick` (DESIGN.md §10, D55, D124).
//!
//! The memory browser's shape (D121) over the working set. `j`/`k` move, `/`
//! opens a fuzzy search on the header line, Enter opens the task's `show` card
//! (D122), and `s` starts the task — the one key here with an effect on the
//! store. The key bar on the bottom row is generated from the table for the
//! mode the screen is in, so it cannot advertise a key the screen ignores.
//!
//! Every row is `list`'s row, drawn by `list`'s own renderer
//! (`render::TaskCols` on `columns::fit`, the rail, the urgency gauge), and the
//! card is `show`'s: the printed renderers paint, and [`tui::painted_line`]
//! carries what they painted into the frame. There is no second layout of a
//! task here to drift from the first.
//!
//! Same split as every screen on the `tui` foundation (D26): [`App`] folds one
//! key into state and answers with an [`Action`] the caller performs, and
//! [`render`] draws `&App` into a `Frame` and decides nothing. The one thing
//! the screen cannot do for itself is read a task from the store, so it asks:
//! [`App::wanted`] names the task whose card it needs and the caller answers
//! with [`App::set_detail`].
//!
//! # Why `s` starts, and why that ends the session
//!
//! D55's reasoning for starting rather than printing a ref stands: the screen
//! refuses a pipe, so a printed ref could only be retyped by hand. What moved
//! (D124) is the key. Enter used to start; it now reads, because on a browser
//! Enter is the key a reader presses to look, and a look that started a timer
//! is the most expensive mis-aimed keystroke this screen could have. `s` is
//! `tasqx start`'s word, and the one D80 plans for the dashboard's row actions
//! (the shipped dashboard still spends `s` on its sort order, D124 "Left
//! standing"). Starting still ends the session:
//! `pick` answers "which of these am I doing now", and beginning it is that
//! answer.

use jiff::Timestamp;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use serde_json::Value;
use tasqx_core::markdown::TimeFormat;

use crate::render::{self, TaskCols, TaskRow};
use crate::theme::{Caps, Ctx, Theme};
use crate::tui::{self, first_visible, fuzzy, rt_style, Hint, Key};

/// A margin, the cursor glyph and a space: the memory browser's lead.
const LEAD: usize = 3;

/// One candidate task: the `task.list` row as the store sent it, for
/// `render::task_row` to lay out, plus what the search reads.
///
/// Constructed only through [`Row::new`], which is where the searchable
/// `fields` are derived FROM the sanitised text (D19), so a query cannot match
/// on bytes the screen never draws. The drawn text is sanitised where it is
/// drawn, by the same `render::task_row` and `render::task_detail` the printed
/// commands go through; [`painted_line`](tui::painted_line) then honours only
/// the painter's own escapes.
pub struct Row {
    pub short_id: i64,
    /// Sanitised: the scrollback line after `s` names the task by it.
    pub title: String,
    task: Value,
    /// The id, the title, the project and the tag list, lowercased and kept
    /// SEPARATE: a term matches this row when it is a subsequence of any ONE
    /// of them.
    ///
    /// One concatenated haystack was the first version and it was wrong in a
    /// way that only shows up on real data: a subsequence may take each letter
    /// from a different field, so with every task in `work.tasqx` the query
    /// `wac` matched "Publish API docs" — `w` from the project, `a` from
    /// `tasqx`, `c` from `docs`. Per field, the letters have to come from one
    /// thing the reader can see.
    ///
    /// Priority is deliberately absent: `H` is one letter that also appears in
    /// half the titles in any store.
    fields: [String; 4],
}

impl Row {
    pub fn new(task: Value) -> Self {
        let text = |key: &str| render::san(task.get(key).and_then(Value::as_str).unwrap_or(""));
        let short_id = task.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        let title = text("title");
        let project = text("project");
        let tags = render::san(
            &task
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default(),
        );
        let fields = [
            short_id.to_string(),
            title.to_lowercase(),
            project.to_lowercase(),
            tags.to_lowercase(),
        ];
        Row {
            short_id,
            title,
            task,
            fields,
        }
    }

    /// How well this row matches every term of an already-lowercased query,
    /// or `None` when some term matches no field (D100, with D121's
    /// whole-term bonus, D124).
    fn score(&self, terms: &[&str]) -> Option<i64> {
        fuzzy::score_terms(&self.fields, &FIELD_WEIGHT, terms)
    }
}

/// Per-field bonus on top of a term's match score, in `fields`' order (id,
/// title, project, tags): the thing with a name outranks incidental metadata
/// (audit #203).
const FIELD_WEIGHT: [i64; 4] = [250, 250, 50, 50];

/// Which keys the screen is listening to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Moving through the list: j/k, Enter reads, `s` starts, `/` searches.
    List,
    /// Typing a query: every character is a letter, so j and q type j and q.
    Search,
    /// One task's `show` card, full screen, scrolling.
    Detail,
}

/// An intent for the caller to carry out. `App` performs nothing itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Start this task and leave the screen.
    Start { short_id: i64 },
    /// Leave having started nothing. An ordinary end to a session since D128:
    /// this screen is a browser, and closing one is not a failed run — the
    /// caller exits 0 and writes nothing to the scrollback. (Under D55, when
    /// `pick` was a chooser whose whole output was the start, this was exit
    /// 4.) A request that could not be SERVED is still non-zero, but that is
    /// decided before the screen opens.
    Cancel,
}

/// The state of the screen.
pub struct App {
    rows: Vec<Row>,
    /// `render::task_row` of each row, measured once: `list`'s cells.
    table: Vec<TaskRow>,
    /// Indices into `rows` that match `query`, best first; every row, in the
    /// store's `-urgency` order, while there is no query.
    matches: Vec<usize>,
    /// Position within `matches`, NOT within `rows`.
    cursor: usize,
    /// What the reader has typed after `/`.
    pub query: String,
    mode: Mode,
    /// The first card line the detail view shows.
    scroll: usize,
    /// The `task.get` answer for the card, or why it could not be read.
    detail: Option<(i64, Result<Value, String>)>,
    /// A sentence for the bottom row in place of the key bar, until the next
    /// key: why a key did nothing (`s` on a task that cannot start).
    status: Option<String>,
    size: (u16, u16),
    theme: Theme,
    caps: Caps,
    time_format: TimeFormat,
    /// The filter the candidates were read with, echoed on the header (rule 8).
    filter: String,
    /// The one instant every date on this screen is measured against: the
    /// screen has no clock of its own, so a redraw cannot disagree with the
    /// rows the caller read.
    now: Timestamp,
}

impl App {
    pub fn new(rows: Vec<Row>, ctx: &Ctx, filter: &str, now: Timestamp) -> Self {
        let table = rows
            .iter()
            .map(|r| render::task_row(&r.task, now, ctx.caps.unicode))
            .collect();
        let matches = (0..rows.len()).collect();
        App {
            rows,
            table,
            matches,
            cursor: 0,
            query: String::new(),
            mode: Mode::List,
            scroll: 0,
            detail: None,
            status: None,
            size: (80, 24),
            theme: ctx.theme.clone(),
            caps: ctx.caps,
            time_format: ctx.time_format,
            filter: render::san(filter),
            now,
        }
    }

    /// The terminal's size, re-read before every frame so paging and the
    /// card's width and scroll limit follow a resize.
    pub fn observe(&mut self, width: u16, height: u16) {
        self.size = (width, height);
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    #[cfg(test)]
    /// The indices of `rows` currently listed, in display order.
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The highlighted row, or `None` when nothing matches the query.
    pub fn selected(&self) -> Option<&Row> {
        self.matches.get(self.cursor).map(|i| &self.rows[*i])
    }

    /// The task whose card the screen needs and does not have: the one under
    /// the cursor, once the card is open.
    pub fn wanted(&self) -> Option<i64> {
        if self.mode != Mode::Detail {
            return None;
        }
        let id = self.selected()?.short_id;
        match &self.detail {
            Some((have, _)) if *have == id => None,
            _ => Some(id),
        }
    }

    /// The `task.get` answer for `id`, or the sentence saying why there is
    /// none (the task can be gone by the time the key arrives).
    pub fn set_detail(&mut self, id: i64, task: Result<Value, String>) {
        // The error sentence is built from a store message, and it is painted
        // through the same seam as everything else, so it is sanitised too.
        self.detail = Some((id, task.map_err(|why| render::san(&why))));
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Fold one key into the state.
    ///
    /// Only presses count: Windows reports a release for every key, and
    /// folding both in typed every letter twice and moved twice per press.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.status = None;
        // Ctrl-C leaves from any state, mid-query included: a modal screen that
        // traps the conventional interrupt is how a TUI ends up killed from
        // another window with the terminal still in raw mode. From the
        // dashboard it leaves the picker, not the dashboard (#229 item 10).
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(Action::Cancel);
        }
        match self.mode {
            Mode::List => self.list_key(key, ctrl),
            Mode::Search => {
                self.search_key(key, ctrl);
                None
            }
            Mode::Detail => self.detail_key(key),
        }
    }

    /// Start the task under the cursor — when `task.start` would.
    ///
    /// A filter can list closed and parked work (`pick project:web`, and the
    /// dashboard's `p` from PROJECTS), and `s` on a done row used to leave
    /// the screen to be refused by the engine: exit 5 from `pick`, and the
    /// whole dashboard gone when `pick` was opened from it. The row's status
    /// is in hand, so the refusal happens here, in words, and the reader
    /// stays where they were.
    fn start(&mut self) -> Option<Action> {
        let row = self.selected()?;
        let status = row
            .task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if matches!(status, "pending" | "active") {
            return Some(Action::Start {
                short_id: row.short_id,
            });
        }
        let dash = if self.caps.unicode { "—" } else { "-" };
        self.status = Some(format!(
            "#{} is {} {dash} only a pending or running task can start",
            row.short_id,
            render::san(status)
        ));
        None
    }

    fn list_key(&mut self, key: KeyEvent, ctrl: bool) -> Option<Action> {
        let page = self.list_rows() as isize;
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
            KeyCode::Enter if self.selected().is_some() => {
                self.mode = Mode::Detail;
                self.scroll = 0;
            }
            KeyCode::Char('s') => return self.start(),
            // Esc undoes the search first, and only leaves once there is
            // nothing left to undo, so the key that ends a search is never the
            // key that quits the screen by surprise.
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Esc | KeyCode::Char('q') => return Some(Action::Cancel),
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
            // The readline pair that goes with ctrl-n/ctrl-p: clear the line,
            // and delete the word behind the cursor.
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char('w') if ctrl => {
                // Trailing whitespace, then the word behind it — a shell's
                // ctrl-w, so `"foo "` becomes `""` in one press.
                let trimmed = self.query.trim_end();
                let cut = trimmed
                    .rfind(char::is_whitespace)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                self.query.truncate(cut);
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

    fn detail_key(&mut self, key: KeyEvent) -> Option<Action> {
        let page = self.detail_rows();
        let max = self.max_scroll();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll = (self.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll = (self.scroll + page).min(max),
            KeyCode::Char('b') | KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = max,
            // Read, then start: the card is where the decision gets made.
            KeyCode::Char('s') => return self.start(),
            // The card is read again the next time it opens, so a failed read
            // is retried and a card does not outlive the list around it.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Left => {
                self.mode = Mode::List;
                self.detail = None;
            }
            _ => {}
        }
        None
    }

    /// Move the cursor by `by`, clamped at both ends — never wrapped, and
    /// never a usize underflow inside a raw-mode alt screen.
    fn step(&mut self, by: isize) {
        let last = self.matches.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + by).clamp(0, last.max(0)) as usize;
    }

    /// Re-rank against the query, keeping the highlight on the SAME TASK.
    ///
    /// Not cosmetic (D55). The cursor indexes `matches`, so leaving it alone
    /// across a refilter silently re-aims it at whatever task now sits there,
    /// and `s` then starts a task the reader never highlighted. The task is
    /// found again, and only when it has left the matches does the cursor
    /// fall back to the top.
    fn refilter(&mut self) {
        let anchor = self.matches.get(self.cursor).copied();
        let needle = self.query.to_lowercase();
        let terms: Vec<&str> = needle.split_whitespace().collect();
        self.matches = if terms.is_empty() {
            // No query: the store's own `-urgency` order.
            (0..self.rows.len()).collect()
        } else {
            // Ties keep their original order, which is `-urgency`, so urgency
            // is the tiebreak for free (#203).
            fuzzy::rank(self.rows.len(), |i| self.rows[i].score(&terms))
        };
        self.cursor = anchor
            .and_then(|a| self.matches.iter().position(|&i| i == a))
            .unwrap_or(0)
            .min(self.matches.len().saturating_sub(1));
    }

    /// Rows the list gets: the frame less the header, the blank line under
    /// it, the column labels, the blank line above the key bar, and the bar.
    fn list_rows(&self) -> usize {
        (self.size.1 as usize).saturating_sub(5).max(1)
    }

    /// Rows the card gets: the frame less the blank line and the key bar.
    fn detail_rows(&self) -> usize {
        (self.size.1 as usize).saturating_sub(2).max(1)
    }

    /// A render context at `cols` cells, for the printed renderers.
    fn ctx_at(&self, cols: usize) -> Ctx {
        Ctx {
            theme: self.theme.clone(),
            caps: self.caps,
            cols,
            time_format: self.time_format,
        }
    }

    /// The card for the task under the cursor, painted, one entry per line:
    /// `show`'s own rendering (D122) at this screen's width.
    fn card(&self) -> Vec<String> {
        let Some(sel) = self.selected() else {
            return Vec::new();
        };
        let cols = (self.size.0 as usize).saturating_sub(2);
        match &self.detail {
            Some((id, Ok(task))) if *id == sel.short_id => {
                render::task_detail(&self.ctx_at(cols), task, self.now)
                    .lines()
                    .map(str::to_string)
                    .collect()
            }
            Some((id, Err(why))) if *id == sel.short_id => {
                vec![self.theme.paint("warn", why, &self.caps)]
            }
            _ => Vec::new(),
        }
    }

    fn max_scroll(&self) -> usize {
        if self.mode != Mode::Detail {
            return 0;
        }
        self.card().len().saturating_sub(self.detail_rows())
    }
}

// ============================================================================
// The key tables
// ============================================================================

/// The list's keys with no search kept. The footer is drawn from this, lowest
/// rank first, so the way out ranks first of all: at any width the bar says
/// how to leave. Esc leaves here, so it rides on `q`'s row rather than being
/// called `clear` with nothing to clear.
pub const LIST_KEYS: &[Key] = &[
    Key {
        keys: "j / k",
        help: "move down / up (the arrows too)",
        footer: Some(Hint {
            keys: "j/k",
            word: "move",
            rank: 3,
        }),
    },
    Key {
        keys: "/",
        help: "search ids, titles, projects and tags",
        footer: Some(Hint {
            keys: "/",
            word: "search",
            rank: 2,
        }),
    },
    Key {
        keys: "enter",
        help: "read the task under the cursor",
        footer: Some(Hint {
            keys: "enter",
            word: "open",
            rank: 2,
        }),
    },
    Key {
        keys: "s",
        help: "start the task under the cursor, and leave",
        footer: Some(Hint {
            keys: "s",
            word: "start",
            rank: 1,
        }),
    },
    Key {
        keys: "g / G",
        help: "first / last task",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 5,
        }),
    },
    Key {
        keys: "q / esc",
        // "leave", not "quit": opened from the dashboard, `q` goes back to it.
        help: "leave, starting nothing (back to the dashboard, when opened from it)",
        footer: Some(Hint {
            keys: "q",
            word: "leave",
            rank: 0,
        }),
    },
];

/// The list's keys with a search kept: Esc clears it first.
pub const LIST_FILTERED_KEYS: &[Key] = &[
    Key {
        keys: "j / k",
        help: "move down / up (the arrows too)",
        footer: Some(Hint {
            keys: "j/k",
            word: "move",
            rank: 3,
        }),
    },
    Key {
        keys: "/",
        help: "change the search",
        footer: Some(Hint {
            keys: "/",
            word: "search",
            rank: 2,
        }),
    },
    Key {
        keys: "enter",
        help: "read the task under the cursor",
        footer: Some(Hint {
            keys: "enter",
            word: "open",
            rank: 2,
        }),
    },
    Key {
        keys: "s",
        help: "start the task under the cursor, and leave",
        footer: Some(Hint {
            keys: "s",
            word: "start",
            rank: 1,
        }),
    },
    Key {
        keys: "g / G",
        help: "first / last task",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 5,
        }),
    },
    Key {
        keys: "esc",
        help: "clear the search",
        footer: Some(Hint {
            keys: "esc",
            word: "clear",
            rank: 4,
        }),
    },
    Key {
        keys: "q",
        help: "leave, starting nothing (back to the dashboard, when opened from it)",
        footer: Some(Hint {
            keys: "q",
            word: "leave",
            rank: 0,
        }),
    },
];

/// The list's keys when nothing is listed (a search that matched nothing):
/// only the ones that still do something, D62's rule for a bar.
pub const LIST_EMPTY_KEYS: &[Key] = &[
    Key {
        keys: "/",
        help: "change the search",
        footer: Some(Hint {
            keys: "/",
            word: "search",
            rank: 2,
        }),
    },
    Key {
        keys: "esc",
        help: "clear the search",
        footer: Some(Hint {
            keys: "esc",
            word: "clear",
            rank: 4,
        }),
    },
    Key {
        keys: "q",
        help: "leave, starting nothing",
        footer: Some(Hint {
            keys: "q",
            word: "leave",
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
        help: "move while typing (ctrl-p / ctrl-n too)",
        footer: Some(Hint {
            keys: "↑↓",
            word: "move",
            rank: 1,
        }),
    },
    Key {
        keys: "ctrl-u",
        help: "clear the query (ctrl-w deletes a word)",
        footer: Some(Hint {
            keys: "ctrl-u",
            word: "clear",
            rank: 2,
        }),
    },
];

/// The search line's keys when the query matches nothing: there is nothing
/// to move through.
pub const SEARCH_EMPTY_KEYS: &[Key] = &[
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
        keys: "ctrl-u",
        help: "clear the query (ctrl-w deletes a word)",
        footer: Some(Hint {
            keys: "ctrl-u",
            word: "clear",
            rank: 1,
        }),
    },
];

/// The card's keys.
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
            rank: 3,
        }),
    },
    Key {
        keys: "g / G",
        help: "top / bottom",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 4,
        }),
    },
    Key {
        keys: "s",
        help: "start this task, and leave",
        footer: Some(Hint {
            keys: "s",
            word: "start",
            rank: 2,
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

/// The card's keys when it fits the screen: nothing to scroll.
pub const DETAIL_FIT_KEYS: &[Key] = &[
    Key {
        keys: "s",
        help: "start this task, and leave",
        footer: Some(Hint {
            keys: "s",
            word: "start",
            rank: 1,
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

/// The table for the state the screen is in. Each state that has keys with
/// nothing to act on — nothing listed, no search to clear, a card with
/// nothing to scroll — has a table of its own, so the bar only names live
/// keys (D62).
fn keys_for(app: &App) -> &'static [Key] {
    let empty = app.matches.is_empty();
    match app.mode {
        Mode::List if empty => LIST_EMPTY_KEYS,
        Mode::List if !app.query.is_empty() => LIST_FILTERED_KEYS,
        Mode::List => LIST_KEYS,
        Mode::Search if empty => SEARCH_EMPTY_KEYS,
        Mode::Search => SEARCH_KEYS,
        Mode::Detail if app.card().len() > app.detail_rows() => DETAIL_KEYS,
        Mode::Detail => DETAIL_FIT_KEYS,
    }
}

// ============================================================================
// Rendering
// ============================================================================

/// Draw the screen. Decides nothing: `App` made every choice, and every colour
/// comes from the theme at the terminal's real depth, through the printed
/// renderers wherever a task is drawn.
pub fn render(app: &App, frame: &mut Frame) {
    let area = frame.area();
    if area.height == 0 || area.width == 0 {
        return;
    }
    let sty = |role: &str| rt_style(app.theme.role(role), &app.caps);
    match app.mode {
        Mode::Detail => draw_detail(app, frame, area),
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
    if let Some(why) = &app.status {
        // Why the last key did nothing, in place of the bar until the next key.
        spans.push(Span::styled(
            render::truncate(
                why,
                (area.width as usize).saturating_sub(1),
                app.caps.unicode,
            ),
            sty("warn"),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), foot);
        return;
    }
    // Where the card is, when it is longer than the screen: `tui::key_bar`
    // measures that number first, so the hints give way to it.
    let position = (app.mode == Mode::Detail).then(|| {
        let card = app.card().len();
        (app.scroll, app.detail_rows(), card)
    });
    let spans = tui::key_bar(
        keys_for(app),
        area.width,
        position,
        sty("accent"),
        sty("muted"),
        app.caps.unicode,
    );
    frame.render_widget(Paragraph::new(Line::from(spans)), foot);
}

fn line_at(frame: &mut Frame, area: Rect, y: u16, spans: Vec<Span<'static>>) {
    if y >= area.bottom() {
        return;
    }
    let rect = Rect {
        x: area.x,
        y,
        width: area.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

fn draw_list(
    app: &App,
    sty: &dyn Fn(&str) -> ratatui::style::Style,
    frame: &mut Frame,
    area: Rect,
) {
    let w = area.width as usize;
    let unicode = app.caps.unicode;

    // One line: the name of the screen, the question that was asked (the
    // filter), and then either what the answer holds — `list`'s own summary,
    // non-zero facts only, fitted by dropping from the right (rules 8 and 9)
    // — or the query and how much of the list it kept. The query lives here
    // rather than on a line of its own, so the list does not move when a
    // search opens (the memory browser's lesson).
    let mut head = vec![
        Span::raw(" "),
        Span::styled("pick", sty("header")),
        Span::raw("   "),
    ];
    let lead_w = 1 + 4 + 3;
    if app.mode == Mode::Search || !app.query.is_empty() {
        // The memory browser's search line too: one function fits both.
        head.extend(tui::search_spans(
            &tui::SearchLine {
                filter: &app.filter,
                query: &app.query,
                searching: app.mode == Mode::Search,
                kept: app.matches.len(),
                total: app.rows.len(),
            },
            w.saturating_sub(lead_w),
            sty("accent"),
            sty("muted"),
            unicode,
        ));
    } else {
        let tasks: Vec<&Value> = app.rows.iter().map(|r| &r.task).collect();
        let summary = render::table_summary(
            &app.ctx_at(w.saturating_sub(lead_w + 1)),
            &tasks,
            &app.table,
            app.rows.len() as i64,
            Some(&app.filter),
            app.now,
            false,
        );
        head.extend(tui::painted_line(&summary).spans);
    }
    line_at(frame, area, area.y, head);

    if app.matches.is_empty() {
        // Each mode names the key that does it there: in the search Esc
        // keeps the filter, so it may not be the key this sentence offers.
        let msg = if app.rows.is_empty() {
            "Nothing to pick.".to_string()
        } else if app.mode == Mode::Search {
            format!(
                "Nothing matches {:?}. Backspace or ctrl-u edits the search.",
                app.query
            )
        } else {
            format!("Nothing matches {:?}. Esc clears the search.", app.query)
        };
        // Cut to the width with an ellipsis rather than by the frame, which
        // would stop it mid-word (rule 2: a line under a record is cut).
        let msg = render::truncate(&msg, w.saturating_sub(3), unicode);
        line_at(
            frame,
            area,
            area.y + 2,
            vec![Span::raw("   "), Span::styled(msg, sty("muted"))],
        );
        return;
    }

    // `list`'s columns, fitted over EVERY candidate rather than the matches or
    // the screenful: a layout sized to what is showing would reflow on every
    // keystroke of a search and every scroll, the same text jumping under a
    // cursor that has not moved.
    let ctx = app.ctx_at(w);
    // The whole width less the lead: no right margin, because at 60 columns
    // one more cell of nothing is what made the fit drop DUE (rule 2).
    let cols = TaskCols::fit(&app.table, w.saturating_sub(LEAD), "DUE", unicode);
    let mut labels = vec![Span::raw(" ".repeat(LEAD))];
    labels.push(Span::styled(
        render::header_line(&cols, "DUE"),
        sty("table.label"),
    ));
    line_at(frame, area, area.y + 2, tui::fit_spans(labels, w, unicode));

    let rows = app.list_rows();
    let top = area.y + 3;
    let first = first_visible(app.cursor, app.matches.len(), rows);
    for (n, &i) in app.matches.iter().enumerate().skip(first).take(rows) {
        let on = n == app.cursor;
        let mark = match (on, unicode) {
            (true, true) => "▸",
            (true, false) => ">",
            _ => " ",
        };
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(mark, sty("accent")),
            Span::raw(" "),
        ];
        spans.extend(tui::painted_line(&render::row_line_at(&ctx, &cols, &app.table[i], on)).spans);
        // Past `list`'s floors a row overflows; cut it with an ellipsis rather
        // than let the frame stop it mid-word (rule 2).
        let spans = tui::fit_spans(spans, w, unicode);
        line_at(frame, area, top + (n - first) as u16, spans);
    }
}

fn draw_detail(app: &App, frame: &mut Frame, area: Rect) {
    let rows = app.detail_rows();
    for (r, line) in app.card().iter().skip(app.scroll).take(rows).enumerate() {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(tui::painted_line(line).spans);
        line_at(frame, area, area.y + r as u16, spans);
    }
}

#[cfg(test)]
mod tests;
