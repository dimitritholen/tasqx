//! The dashboard screen (D58) — what a bare, interactive `tasqx` opens.
//!
//! Split the way `pick` and `settings` are: [`model`] holds the data and the
//! geometry, both pure, and everything here draws or reacts to a key. The
//! terminal stays owned by the parent [`crate::tui`] module — no second
//! lifecycle, which is the invariant that keeps the existing panic hook and
//! `Restore` guard sufficient.
//!
//! **Chrome is composited at screen level, not by the panels.** Each panel
//! writes body text into its own rectangle and nothing else; every rule,
//! corner, tee and vertical is drawn in one pass before any panel runs. That is
//! not an aesthetic preference — it is forced. [`model::layout`] partitions the
//! width with no cells left over, so a column boundary has to be *stolen* from
//! the column on its left, and the glyph at a boundary is a function of BOTH
//! neighbours: the same cell must be `┬` where two panels start, `┤` where only
//! the left column starts one, and `┼` where all three do. A panel holding only
//! its own rectangle cannot know that. Panels drawing their own borders is also
//! what the ladder cannot afford — `Borders::ALL` costs two rows each, where
//! this costs three for the whole screen.

pub mod json;
pub mod model;
pub mod panels;

use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui::style::Style as RtStyle;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::render;
use crate::theme::{Caps, Theme};
use crate::tui::rt_style;
use model::{Dashboard, PanelId, Placement, Screen};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The burndown windows `w` cycles through, in days.
///
/// Must stay equal to the `dashboard.window` vocabulary in `config::SETTINGS` —
/// a gate asserts it rather than trusting two lists to agree, which is the
/// idiom the `VERBS`/`METHODS` tables already run on.
pub const WINDOW_CHOICES: [(&str, usize); 3] = [("week", 7), ("14d", 14), ("30d", 30)];

/// What the event loop must do next. Everything else the screen handles itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Quit,
    /// Hand over to `pick` inside the SAME terminal session (D58).
    Pick,
    /// Leave the screen and print the working-set table into the scrollback.
    List,
    /// Re-read the four results and rebuild the model.
    Refresh,
    /// Fetch `task.get` for this short_id and hand it back through
    /// [`App::show_detail`] — the lazy read D58 specified for the focused row
    /// and never made.
    ///
    /// Served inside the event loop like [`Action::Refresh`], not on the way
    /// out like [`Action::Pick`]: the overlay is drawn on the screen it belongs
    /// to, so tearing the terminal down for it would flicker the whole
    /// dashboard to show one card.
    Detail(i64),
}

/// The screen's state. A pure state machine: keys in, intents out, no terminal.
pub struct App {
    dash: Dashboard,
    order: Vec<PanelId>,
    /// The focused panel — never [`PanelId::Slot`]. Focus is always a real
    /// panel, and the *screen* decides whether that means highlighting its own
    /// rectangle or filling the analytics slot with it. That is what answers
    /// D58's "on a narrow screen a digit places rather than focuses" without
    /// putting one dimension of geometry in here.
    focus: PanelId,
    /// Which slot member the analytics slot shows. Sticky across refreshes.
    slot: PanelId,
    /// Where the reader is in each panel, as a SELECTABLE-ROW index — never a
    /// body-line index and never a scroll offset.
    ///
    /// It was a scroll offset, and a scroll offset has nothing to point at: the
    /// content moved past the viewport with no row marked, so "where am I" had
    /// no answer on the screen. Rows and not lines because `due_body` puts its
    /// bucket names (`OVERDUE`, `TODAY`, …) and its `…N more` marker in the
    /// same `Vec<Line>` as its tasks, and a cursor that can land on either is a
    /// cursor `Enter` cannot use (D58).
    ///
    /// The offset that used to live here is now DERIVED, in `panels`, from this
    /// index and the viewport — the state machine is still never told the
    /// terminal's height.
    cursor: HashMap<PanelId, usize>,
    /// What the last draw actually placed, fed back by the event loop.
    ///
    /// Data in, exactly like a key press. Without it Tab has dead stops: on the
    /// XS rung there is no slot AND no Projects/Burndown/Tokens rectangle, so
    /// three of its eight stops would aim at something that cannot be drawn.
    placed: Vec<PanelId>,
    has_slot: bool,
    window: usize,
    auto_refresh: bool,
    help: bool,
    /// The open detail card, or nothing.
    ///
    /// State and not a flag, because the card is a fetch: the loop answers
    /// [`Action::Detail`] by calling `task.get` and handing the result to
    /// [`App::show_detail`]. Modal on the terms `?` established, and checked
    /// beside `help` so the two cannot both be open.
    detail: Option<model::TaskDetail>,
    /// One transient line, shown in the footer instead of the key hints.
    status: String,
}

impl App {
    pub fn new(
        dash: Dashboard,
        order: Vec<PanelId>,
        window_days: usize,
        auto_refresh: bool,
    ) -> Self {
        let focus = order.first().copied().unwrap_or(PanelId::Now);
        // The slot opens on the first slot member the reader CONFIGURED, in
        // their own ordering. It used to open on a hard-coded Burndown, so a
        // `dashboard.panels` naming only `projects` drew the excluded panel and
        // hid the requested one — and the slot is sticky, so it stayed wrong.
        // The fallback is unreachable in practice: with no slot member
        // configured `layout` builds no slot, so nothing ever draws from here.
        let slot = order
            .iter()
            .copied()
            .find(|id| PanelId::SLOT_MEMBERS.contains(id))
            .unwrap_or(PanelId::Burndown);
        App {
            dash,
            order,
            focus,
            slot,
            cursor: HashMap::new(),
            placed: Vec::new(),
            has_slot: false,
            window: WINDOW_CHOICES
                .iter()
                .position(|(_, d)| *d == window_days)
                .unwrap_or(0),
            auto_refresh,
            help: false,
            detail: None,
            status: String::new(),
        }
    }

    #[cfg(test)]
    pub fn dash(&self) -> &Dashboard {
        &self.dash
    }

    /// The screen this state would draw at this size.
    ///
    /// The one place `model::layout` is called from. Both the renderer and the
    /// event loop need the answer — the loop feeds `placed` back through
    /// [`App::observe`] — and a second call site is a second chance to pass
    /// different arguments, which would tell the state machine about a screen
    /// nobody drew.
    ///
    /// It also owns the demand closure, which needs the CONFIGURED slot
    /// members: the analytics slot is one rectangle for three panels, and it is
    /// sized for the tallest of them so that `6`/`7`/`8` swap the occupant
    /// without resizing the box under the reader.
    pub fn screen(&self, width: u16, height: u16) -> Option<Screen> {
        let members: Vec<PanelId> = self
            .order
            .iter()
            .copied()
            .filter(|id| PanelId::SLOT_MEMBERS.contains(id))
            .collect();
        model::layout(width, height, &self.order, &|id| {
            model::demand(&self.dash, &members, id)
        })
    }

    #[cfg(test)]
    pub fn focus(&self) -> PanelId {
        self.focus
    }

    #[cfg(test)]
    pub fn slot(&self) -> PanelId {
        self.slot
    }

    #[cfg(test)]
    pub fn help_open(&self) -> bool {
        self.help
    }

    /// The transient footer line, or empty. Read by the gate that checks every
    /// advertised key does something — for a digit naming an unreachable panel,
    /// this line IS the whole effect.
    #[cfg(test)]
    pub fn status_line(&self) -> &str {
        &self.status
    }

    pub fn auto_refresh(&self) -> bool {
        self.auto_refresh
    }

    pub fn window_days(&self) -> usize {
        WINDOW_CHOICES[self.window].1
    }

    /// Where the reader is in `id`, clamped to the data as it stands NOW.
    ///
    /// Clamped on the way out and not on the way in, because the data can shrink
    /// under a cursor that nobody touched: `replace` swaps the whole `Dashboard`
    /// on every auto-refresh, and a task completed elsewhere shortens a panel.
    /// A stored index is a claim about a list that no longer exists.
    pub fn cursor_of(&self, id: PanelId) -> usize {
        let stored = self.cursor.get(&id).copied().unwrap_or(0);
        stored.min(self.rows_in(id).saturating_sub(1))
    }

    /// Open the detail overlay on a card the loop has fetched.
    ///
    /// The state machine asked for it ([`Action::Detail`]) and the loop did the
    /// I/O — the same division `Refresh` runs on, and the reason this module
    /// still touches no backend.
    pub fn show_detail(&mut self, card: model::TaskDetail) {
        self.detail = Some(card);
    }

    /// Tell the reader why the card they asked for never opened.
    ///
    /// A row can be gone by the time `⏎` reaches the store — auto-refresh redraws
    /// every five seconds and another terminal can finish the task in between —
    /// so a stale row is expected, not exceptional, and it is a message rather
    /// than the end of the session.
    pub fn say(&mut self, message: impl Into<String>) {
        self.status = message.into();
    }

    #[cfg(test)]
    pub fn detail_open(&self) -> bool {
        self.detail.is_some()
    }

    /// Replace the data after a refresh, keeping focus, slot and cursor.
    ///
    /// A screen that jumped back to the top on every interval would be
    /// unreadable with auto-refresh on.
    pub fn replace(&mut self, dash: Dashboard) {
        self.dash = dash;
    }

    /// Tell the state machine what the last draw placed. Called by the loop
    /// straight after `draw` returns.
    pub fn observe(&mut self, placed: &[PanelId], has_slot: bool) {
        self.placed = placed.to_vec();
        self.has_slot = has_slot;
    }

    /// Whether `id` can be reached right now — either it has its own rectangle,
    /// or it can be summoned into the analytics slot.
    ///
    /// The slot half is guarded by `order` as well, and that is not belt and
    /// braces: the slot is one rectangle shared by three panels, so "a slot
    /// exists" says nothing about WHICH members were configured. Without the
    /// guard a digit summoned any of the three into it, overriding
    /// `dashboard.panels` from the keyboard.
    fn reachable(&self, id: PanelId) -> bool {
        self.placed.contains(&id)
            || (self.has_slot && PanelId::SLOT_MEMBERS.contains(&id) && self.order.contains(&id))
    }

    /// How many rows the cursor can reach in `id` — [`model::row_count`], the
    /// one count of selectable rows.
    ///
    /// It used to be [`model::demand`], the count of body LINES, because the
    /// thing being clamped was a scroll offset and lines are what scrolls. A
    /// cursor is not an offset: DUE's body is twelve lines over eight tasks,
    /// each non-empty bucket spending a row on its name, so a cursor clamped to
    /// twelve walks four rows past its last task onto headings.
    fn rows_in(&self, id: PanelId) -> usize {
        model::row_count(&self.dash, id)
    }

    /// Point focus at `id`, and if it lives in the analytics slot, put it there.
    ///
    /// One assignment covers both of D58's behaviours: where the panel has its
    /// own rectangle the renderer highlights it, and where it does not the
    /// renderer draws the slot from `slot` — so a digit genuinely *places*.
    fn aim(&mut self, id: PanelId) {
        self.focus = id;
        if PanelId::SLOT_MEMBERS.contains(&id) {
            self.slot = id;
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Windows crossterm sends a Release for every Press. Without this
        // filter every `j` scrolls two rows and every `?` toggles help back
        // shut — the bug `pick` and `settings` have each shipped once.
        if key.kind != KeyEventKind::Press {
            return None;
        }
        self.status.clear();

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            // Ctrl-C quits outright, help open or not: it is never "close the
            // overlay". Nothing else takes a modifier — unlike `pick` there is
            // no query line here, so plain printable keys are all commands.
            return match key.code {
                KeyCode::Char('c') => Some(Action::Quit),
                _ => None,
            };
        }

        // The overlay is MODAL, and it says so on its own last line: "any key
        // closes this". It used to be a lie in both directions — only `?`, `q`
        // and `esc` closed it, and every other key fell through and acted on
        // the screen behind it. `2` moved a focus nobody could see, `j` scrolled
        // a hidden panel, `p` opened the picker over the top of the help text,
        // and `l` left the dashboard from behind a modal the reader was still
        // looking at.
        //
        // Ctrl-C is the exception and is handled above: it is never "close the
        // overlay", it is always "close the program".
        //
        // The detail card is modal on exactly those terms, and is checked in
        // the same place so the two can never be open at once.
        if self.help {
            self.help = false;
            return None;
        }
        if self.detail.is_some() {
            self.detail = None;
            return None;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
            KeyCode::Char(c @ '1'..='8') => {
                let id = panel_of_digit(c as u8 - b'0')?;
                if self.reachable(id) {
                    self.aim(id);
                } else if self.order.contains(&id) {
                    self.status = format!("{} does not fit at this size", id.title());
                } else {
                    // A DIFFERENT refusal, and conflating the two sent readers
                    // off resizing a window that was never the problem: this
                    // panel is not in `dashboard.panels`, so no terminal is
                    // ever big enough to bring it back. Name the setting —
                    // it is the only thing that fixes this.
                    self.status = format!("{} is not in dashboard.panels", id.title());
                }
                None
            }
            KeyCode::Tab => {
                self.step(1);
                None
            }
            KeyCode::BackTab => {
                self.step(-1);
                None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let max = self.rows_in(self.focus).saturating_sub(1);
                let cur = self.cursor_of(self.focus);
                self.cursor.insert(self.focus, (cur + 1).min(max));
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                // Saturating, not `- 1`: an underflow here panics inside a raw
                // mode alt screen, where the message is wiped before it can be
                // read.
                let cur = self.cursor_of(self.focus);
                self.cursor.insert(self.focus, cur.saturating_sub(1));
                None
            }
            KeyCode::Char('g') => {
                self.cursor.insert(self.focus, 0);
                None
            }
            KeyCode::Char('G') => {
                let max = self.rows_in(self.focus).saturating_sub(1);
                self.cursor.insert(self.focus, max);
                None
            }
            KeyCode::Char('r') => Some(Action::Refresh),
            KeyCode::Char('R') => {
                self.auto_refresh = !self.auto_refresh;
                self.status = if self.auto_refresh {
                    "auto-refresh on".into()
                } else {
                    "auto-refresh off".into()
                };
                None
            }
            KeyCode::Enter => {
                match model::row_at(&self.dash, self.focus, self.cursor_of(self.focus)) {
                    Some(t) => Some(Action::Detail(t.short_id)),
                    // Named, not silent. A key that advertises itself in the footer
                    // and then does nothing is this repository's recurring
                    // silent-drop shape, and the digit refusals already answer the
                    // same question the same way.
                    None => {
                        self.status = format!("{} has no row to open", self.focus.title());
                        None
                    }
                }
            }
            KeyCode::Char('p') => Some(Action::Pick),
            KeyCode::Char('l') => Some(Action::List),
            KeyCode::Char('?') => {
                self.help = !self.help;
                None
            }
            // The window changes which events are fetched (D59's `from`), so it
            // must re-read. A `w` that only relabelled the axis would lie.
            KeyCode::Char('w') => {
                self.window = (self.window + 1) % WINDOW_CHOICES.len();
                Some(Action::Refresh)
            }
            _ => None,
        }
    }

    /// Walk focus to the next reachable panel, wrapping.
    fn step(&mut self, by: i32) {
        let reachable: Vec<PanelId> = self
            .order
            .iter()
            .copied()
            .filter(|id| self.reachable(*id))
            .collect();
        if reachable.is_empty() {
            return;
        }
        let at = reachable
            .iter()
            .position(|id| *id == self.focus)
            .unwrap_or(0);
        let n = reachable.len() as i32;
        let next = ((at as i32 + by).rem_euclid(n)) as usize;
        self.aim(reachable[next]);
    }
}

fn panel_of_digit(d: u8) -> Option<PanelId> {
    [
        PanelId::Now,
        PanelId::Next,
        PanelId::Due,
        PanelId::Blocked,
        PanelId::Recent,
        PanelId::Projects,
        PanelId::Burndown,
        PanelId::Tokens,
    ]
    .into_iter()
    .find(|p| p.digit() == Some(d))
}

// ============================================================================
// Chrome
// ============================================================================

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

/// The box-drawing character for a set of connections, or its ASCII stand-in.
///
/// The ASCII branch is not a nicety: box-drawing bytes on a legacy Windows
/// console are mojibake, and mojibake in a grid misaligns every column to its
/// right. Same signal source (`caps.unicode`) the rest of the CLI degrades on.
fn glyph(b: u8, unicode: bool) -> char {
    if !unicode {
        return match b {
            0 => ' ',
            b if b & (UP | DOWN) == 0 => '-',
            b if b & (LEFT | RIGHT) == 0 => '|',
            _ => '+',
        };
    }
    match b {
        0 => ' ',
        b if b == RIGHT | DOWN => '┌',
        b if b == LEFT | DOWN => '┐',
        b if b == RIGHT | UP => '└',
        b if b == LEFT | UP => '┘',
        b if b == UP | DOWN | RIGHT => '├',
        b if b == UP | DOWN | LEFT => '┤',
        b if b == LEFT | RIGHT | DOWN => '┬',
        b if b == LEFT | RIGHT | UP => '┴',
        b if b == LEFT | RIGHT | UP | DOWN => '┼',
        b if b & (UP | DOWN) == 0 => '─',
        _ => '│',
    }
}

/// The x of the frame's left edge plus the last cell of every column — the
/// cells the screen owns and no panel may write into.
///
/// Derived from `Screen::columns`, never from the placements: a configured
/// `dashboard.panels` can leave a whole column empty, and a seam derived from
/// placements would then vanish while the columns beside it still need
/// separating.
fn chrome_cols(width: u16, columns: u16) -> Vec<u16> {
    let mut out = vec![0u16];
    for (x, w) in model::column_extents(width, columns) {
        out.push(x + w - 1);
    }
    out
}

/// The greatest chrome column at or left of `x`.
fn left_chrome(cols: &[u16], x: u16) -> u16 {
    cols.iter().copied().rev().find(|c| *c <= x).unwrap_or(0)
}

/// Where a panel may actually write: its body rect, inset past the chrome the
/// screen owns on either side.
///
/// Column 0 loses two cells (the frame's left edge and its own right seam);
/// every other column loses one, because its left seam belongs to the column
/// before it.
pub(crate) fn interior(p: &Placement) -> Rect {
    let (x, y, w, h) = p.body();
    let left = u16::from(x == 0);
    Rect::new(x + left, y, w.saturating_sub(left + 1), h)
}

/// `──4─ BLOCKED ` — the label that sits in a panel's rule.
///
/// Focus is a glyph swap in a cell that exists either way, plus `accent` on the
/// label. Not a background: `theme::Style` has no `bg` field, so `rt_style` can
/// never produce one, and a focus ring drawn in colour alone would vanish under
/// `NO_COLOR`.
fn rule_label(id: PanelId, focused: bool, unicode: bool) -> String {
    let dash = if unicode { '─' } else { '-' };
    let mark = if focused {
        if unicode {
            '▸'
        } else {
            '>'
        }
    } else {
        ' '
    };
    let n = id
        .digit()
        .map(|d| d.to_string())
        .unwrap_or_else(|| "*".to_string());
    format!("{dash}{dash}{n}{dash}{mark}{} ", id.title())
}

/// Draw every rule, corner, tee and vertical in one pass.
fn draw_chrome(screen: &Screen, app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let area = frame.area();
    let (w, h) = (area.width, area.height);
    let cols = chrome_cols(w, screen.columns);
    let mut grid: HashMap<(u16, u16), u8> = HashMap::new();
    let mut mark = |x: u16, y: u16, bits: u8| {
        *grid.entry((x, y)).or_insert(0) |= bits;
    };

    // Horizontal: the top rule, every panel's title rule, the closing rule.
    let horiz = |y: u16, x0: u16, x1: u16, m: &mut dyn FnMut(u16, u16, u8)| {
        for x in x0..=x1 {
            let mut bits = 0;
            if x > x0 {
                bits |= LEFT;
            }
            if x < x1 {
                bits |= RIGHT;
            }
            m(x, y, bits);
        }
    };
    horiz(0, 0, w - 1, &mut mark);
    horiz(screen.rule_y, 0, w - 1, &mut mark);
    for p in &screen.panels {
        let x0 = left_chrome(&cols, p.x);
        horiz(p.y, x0, p.x + p.w - 1, &mut mark);
    }

    // Vertical: the frame's edges over the whole height, the seams from row 1
    // down. Starting the seams at row 1 rather than 0 is what makes the first
    // rule read `┬` rather than `┼`.
    for (i, cx) in cols.iter().enumerate() {
        let y0 = if i == 0 || i + 1 == cols.len() { 0 } else { 1 };
        for y in y0..=screen.rule_y {
            let mut bits = 0;
            if y > y0 {
                bits |= UP;
            }
            if y < screen.rule_y {
                bits |= DOWN;
            }
            mark(*cx, y, bits);
        }
    }

    let muted = rt_style(theme.role("muted"), caps);
    for ((x, y), bits) in grid {
        if x >= w || y >= h {
            continue;
        }
        let ch = glyph(bits, caps.unicode);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(ch.to_string(), muted))),
            Rect::new(x, y, 1, 1),
        );
    }

    // The labels go on top of the rules they interrupt.
    let accent = rt_style(theme.role("accent"), caps);
    for p in &screen.panels {
        let id = if p.id == PanelId::Slot {
            app.slot
        } else {
            p.id
        };
        let focused = id == app.focus;
        let label = rule_label(id, focused, caps.unicode);
        let x0 = left_chrome(&cols, p.x) + 1;
        let avail = (p.x + p.w).saturating_sub(x0 + 1);
        let cut = render::truncate(&label, avail as usize, caps.unicode);
        let style = if focused { accent } else { muted };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(cut, style))),
            Rect::new(x0, p.y, avail, 1),
        );
    }
}

// ============================================================================
// Render
// ============================================================================

/// Draw the whole screen.
pub fn render(app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let area = frame.area();
    let Some(screen) = app.screen(area.width, area.height) else {
        // Unreachable in practice — the caller refuses a terminal this small
        // before entering the alternate screen — but a return is the only
        // honest answer if it ever is reached.
        return;
    };
    draw_chrome(&screen, app, theme, caps, frame);
    for p in &screen.panels {
        draw_panel(p, app, theme, caps, frame);
    }
    draw_status(&screen, app, theme, caps, frame);
    draw_footer(&screen, app, theme, caps, frame);
    // `on_key` closes one before the other can open, so the order here settles
    // nothing at runtime — it is written down so a future edit that breaks that
    // invariant still draws one overlay rather than two overlapping boxes.
    if app.help {
        draw_help(area, theme, caps, frame);
    } else if let Some(card) = &app.detail {
        draw_detail(card, area, theme, caps, frame);
    }
}

/// Draw one panel's body, and nothing else.
///
/// `pub(crate)` and separate from the loop on purpose: the containment test
/// draws a single panel into a blank buffer and asserts nothing landed outside
/// its interior, which is impossible if this is inlined.
pub(crate) fn draw_panel(p: &Placement, app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let id = if p.id == PanelId::Slot {
        app.slot
    } else {
        p.id
    };
    let inner = interior(p);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = panels::body(
        id,
        p.detail,
        &app.dash,
        inner.width,
        inner.height,
        panels::Cursor {
            row: app.cursor_of(id),
            // The rule already says which panel the keys act on; the glyph says
            // which ROW. Drawn in eight panels at once it would say neither.
            shown: id == app.focus,
        },
        theme,
        caps,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_status(screen: &Screen, app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let width = screen.status.w;
    if width < 4 {
        return;
    }
    let spans = panels::status_line(&app.dash.status, width - 2, theme, caps);
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(1, 0, width - 2, 1),
    );
}

fn draw_footer(screen: &Screen, app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let width = screen.status.w;
    let muted = rt_style(theme.role("muted"), caps);
    let accent = rt_style(theme.role("accent"), caps);
    let text = if app.status.is_empty() {
        None
    } else {
        Some(app.status.as_str())
    };
    let line = match text {
        Some(t) => Line::from(Span::styled(
            render::truncate(t, width as usize, caps.unicode),
            accent,
        )),
        None => Line::from(footer_spans(KEYS, width, accent, muted)),
    };
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(1, screen.footer_y, width.saturating_sub(1), 1),
    );
}

/// The footer's spans: as many of `keys` as the width affords, lowest rank
/// first, drawn in table order.
///
/// The width decides, not the rung. A rung match here would be the second list
/// again in a different costume — and it made the wide footer a fixed eight
/// even on a 200-column terminal with room for every one of them. What the rank
/// buys is that the *narrow* end keeps `?`, `q` and `p`: the three a reader
/// cannot do without, chosen by the table rather than by a literal beside it.
///
/// A hint is taken whole or not at all. A footer cut mid-word reads as a broken
/// screen, which is why this measures before it pushes rather than truncating
/// afterwards — the idiom `build_bar` established for the status line.
fn footer_spans(keys: &[Key], width: u16, accent: RtStyle, muted: RtStyle) -> Vec<Span<'static>> {
    let mut ranked: Vec<(usize, &Hint)> = keys
        .iter()
        .enumerate()
        .filter_map(|(i, k)| k.footer.as_ref().map(|h| (i, h)))
        .collect();
    ranked.sort_by_key(|(_, h)| h.rank);

    let mut budget = width as usize;
    let mut taken: Vec<(usize, &Hint)> = Vec::new();
    for (i, h) in ranked {
        // The keys, a space, the word, and the three cells of gutter that
        // separate this hint from the next one.
        let need = render::width(h.keys) + 1 + render::width(h.word) + 3;
        if need > budget {
            continue;
        }
        budget -= need;
        taken.push((i, h));
    }
    taken.sort_by_key(|(i, _)| *i);

    let mut spans = Vec::new();
    for (_, h) in taken {
        spans.push(Span::styled(h.keys.to_string(), accent));
        spans.push(Span::styled(format!(" {}   ", h.word), muted));
    }
    spans
}

/// The help overlay: every binding, from the one table that also drives the
/// footer, so the two cannot drift.
fn draw_help(area: Rect, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let accent = rt_style(theme.role("accent"), caps);
    let muted = rt_style(theme.role("muted"), caps);
    let header = rt_style(theme.role("header"), caps);
    let mut lines = vec![
        Line::from(Span::styled("tasqx dashboard".to_string(), header)),
        Line::from(Span::styled(String::new(), muted)),
    ];
    for k in KEYS {
        lines.push(Line::from(vec![
            // Width from the table itself: a hard-coded 10 fused
            // `tab / S-tab` (11 cells) into the description beside it.
            Span::styled(
                format!("  {:<width$}", k.keys, width = key_column()),
                accent,
            ),
            Span::styled(k.help.to_string(), RtStyle::default()),
        ]));
    }
    lines.push(Line::from(Span::styled(String::new(), muted)));
    lines.push(Line::from(Span::styled(MODAL_FOOT.to_string(), muted)));
    draw_overlay(lines, area, theme, caps, frame);
}

/// The last line of every modal, and the sentence its behaviour is held to.
///
/// A constant because it is a promise `on_key` keeps: any key closes, `ctrl-c`
/// excepted, and nothing falls through to the screen behind. It was a lie once
/// in both directions — only `?`, `q` and `esc` closed the help, and every
/// other key acted on the dashboard underneath it.
const MODAL_FOOT: &str = "  any key closes this";

/// Draw `lines` as a centred, bordered box over `area`.
///
/// One frame for both overlays. It was written once for `?` and the detail card
/// would have been the second copy — the same second-literal drift the footer
/// was just rebuilt to end (D62), in the code that draws rather than in the code
/// that lists.
fn draw_overlay(lines: Vec<Line>, area: Rect, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let muted = rt_style(theme.role("muted"), caps);
    // Wide enough for the longest line it actually holds. A fixed width cut
    // `focus a panel (or place it in the analytics slot)` mid-word at EVERY
    // terminal size, so no window ever revealed the tail.
    let widest = lines
        .iter()
        .map(|l| render::width(&l.to_string()))
        .max()
        .unwrap_or(0) as u16;
    let w = (widest + 4).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let x = (area.width - w) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    frame.render_widget(ratatui::widgets::Clear, rect);
    // A BORDER, not just a cleared rectangle. Without one the overlay reads as
    // the dashboard having come apart: the panels around it keep their rules,
    // and the blank region in the middle looks like damage rather than like
    // something drawn on top.
    let rule = if caps.unicode { '─' } else { '-' };
    let side = if caps.unicode { '│' } else { '|' };
    let (tl, tr, bl, br) = if caps.unicode {
        ('┌', '┐', '└', '┘')
    } else {
        ('+', '+', '+', '+')
    };
    let inner = w.saturating_sub(2) as usize;
    let mut framed: Vec<Line> = Vec::with_capacity(lines.len() + 2);
    framed.push(Line::from(Span::styled(
        format!("{tl}{}{tr}", rule.to_string().repeat(inner)),
        muted,
    )));
    for l in lines {
        // Cut to the box, not clipped by it: a card holds an annotation body
        // nobody wrote to fit, and ratatui's own clip would lose the ellipsis
        // that says something was cut.
        let text = render::truncate(&l.to_string(), inner, caps.unicode);
        let pad = inner.saturating_sub(render::width(&text));
        framed.push(Line::from(vec![
            Span::styled(side.to_string(), muted),
            Span::raw(format!("{text}{}", " ".repeat(pad))),
            Span::styled(side.to_string(), muted),
        ]));
    }
    framed.push(Line::from(Span::styled(
        format!("{bl}{}{br}", rule.to_string().repeat(inner)),
        muted,
    )));
    frame.render_widget(Paragraph::new(framed), rect);
}

/// The detail overlay: one `task.get`, and the `depends_on` that is the only
/// answer BLOCKED has ever been able to give.
fn draw_detail(
    card: &model::TaskDetail,
    area: Rect,
    theme: &Theme,
    caps: &Caps,
    frame: &mut Frame,
) {
    let accent = rt_style(theme.role("accent"), caps);
    let muted = rt_style(theme.role("muted"), caps);
    let header = rt_style(theme.role("header"), caps);
    let danger = rt_style(theme.role("danger"), caps);
    let plain = RtStyle::default();
    let dash = "—";

    // The running marker is NOW's, reused rather than invented: one glyph means
    // "the timer is on this" everywhere on the screen.
    let running = if card.active_since.is_some() {
        if caps.unicode {
            "▶ "
        } else {
            "> "
        }
    } else {
        ""
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            running.to_string(),
            rt_style(theme.role("timer.active"), caps),
        ),
        Span::styled(format!("#{} ", card.short_id), accent),
        Span::styled(card.title().to_string(), header),
    ])];

    let mut facts = vec![
        card.project().unwrap_or(dash).to_string(),
        card.status.as_str().to_string(),
    ];
    if let Some(p) = card.priority {
        facts.push(p.as_str().to_string());
    }
    facts.push(format!("urgency {:.1}", card.urgency));
    lines.push(Line::from(Span::styled(
        format!("  {}", facts.join(" · ")),
        muted,
    )));

    // The reason this overlay exists. It is drawn before the dates, and in
    // `danger`, because a reader who pressed ⏎ on a BLOCKED row asked exactly
    // one question.
    if !card.depends_on().is_empty() {
        let ids: Vec<String> = card.depends_on().iter().map(|n| format!("#{n}")).collect();
        lines.push(Line::from(vec![
            Span::styled("  blocked by ".to_string(), danger),
            Span::styled(ids.join(" "), accent),
        ]));
    } else if card.blocked {
        // `blocked` and no edges is not a contradiction to hide: D11 computes
        // it, and a reader looking at a BLOCKED row deserves to be told the
        // panel and the task disagree rather than shown a card with a gap.
        lines.push(Line::from(Span::styled(
            "  blocked, with no dependency recorded".to_string(),
            danger,
        )));
    }

    let when = |t: Option<jiff::Timestamp>| {
        t.map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date().to_string())
            .unwrap_or_else(|| dash.to_string())
    };
    lines.push(Line::from(Span::styled(
        format!(
            "  due {}   scheduled {}   wait {}",
            when(card.due),
            when(card.scheduled),
            when(card.wait)
        ),
        muted,
    )));
    let est = card
        .estimate_secs
        .map(panels::dur_compact)
        .unwrap_or_else(|| dash.to_string());
    lines.push(Line::from(Span::styled(
        format!(
            "  est {est}   tracked {}",
            panels::dur_compact(card.tracked_secs)
        ),
        muted,
    )));
    // `completed` where there is one, `modified` where there is not: a closed
    // task's last edit is rarely the fact anybody wants, and an open task has no
    // completion date to show. Both are on the card because both are on
    // `task.get`, and a field fetched and never drawn is a field the reader was
    // told nothing about.
    let last = match card.completed {
        Some(t) => format!("completed {}", when(Some(t))),
        None => format!("modified {}", when(Some(card.modified))),
    };
    lines.push(Line::from(Span::styled(
        format!("  created {}   {last}", when(Some(card.created))),
        muted,
    )));
    if let Some(r) = card.recurrence() {
        lines.push(Line::from(Span::styled(format!("  repeats {r}"), muted)));
    }
    if !card.tags().is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  tags ".to_string(), muted),
            Span::styled(card.tags().join("  "), plain),
        ]));
    }

    // Newest last, which is the order they were written and the order every
    // other surface in this program prints them.
    if !card.annotations().is_empty() {
        lines.push(Line::from(Span::styled(String::new(), muted)));
        for n in card.annotations() {
            let stamp = n
                .created
                .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date().to_string())
                .unwrap_or_else(|| dash.to_string());
            lines.push(Line::from(vec![
                Span::styled(format!("  {stamp}  "), muted),
                Span::styled(n.body.as_str(), plain),
            ]));
        }
    }

    lines.push(Line::from(Span::styled(String::new(), muted)));
    lines.push(Line::from(Span::styled(MODAL_FOOT.to_string(), muted)));
    draw_overlay(lines, area, theme, caps, frame);
}

/// The width the help overlay's key column needs, derived rather than guessed.
fn key_column() -> usize {
    KEYS.iter().map(|k| k.keys.len()).max().unwrap_or(8) + 1
}

/// Every binding, once. The help overlay renders from this, and a gate asserts
/// the two agree — the docs-drift idiom, applied inside the TUI.
/// One binding, in the ONE table the help overlay and the footer both read.
///
/// They used to be two. The overlay iterated `KEYS`; the footer built from a
/// `hints` literal of its own, under a doc comment asserting the two "cannot
/// drift". They had: `w`, `R`, `g`/`G` and `S-tab` were advertised nowhere but
/// behind `?`, so the burndown window was a control the screen never mentioned
/// and a reader who did not open the help had no way to learn it was there
/// (D62). A second list of strings is a second path to the data, and this is
/// that rule from `CLAUDE.md` applied to the key vocabulary.
pub struct Key {
    /// The binding as a reader types it, e.g. `"j / k"`. Parsed by the tests
    /// that bind this table to `on_key`, so the shape is load-bearing: tokens
    /// split on `/`, and anything that is not a single character or one of the
    /// named non-`Char` keys makes them fail loudly rather than skip it.
    pub keys: &'static str,
    /// The overlay's line — a sentence, because the overlay has the room.
    pub help: &'static str,
    /// How the footer spells this binding, or `None` for one it deliberately
    /// withholds.
    ///
    /// `ctrl-c` is the only `None`, because `q` already answers "how do I
    /// leave" — an omission this table states rather than one the footer
    /// arrives at by drifting.
    pub footer: Option<Hint>,
}

/// A binding as the footer draws it: shorter than the overlay's spelling, and
/// ranked, because a 56-column footer cannot hold eleven of them.
pub struct Hint {
    /// The keys the footer prints, which is not always the overlay's spelling.
    /// `tab / S-tab` prints as `tab` and `q / esc` as `q` — the second half of
    /// each is a variant of the first. `j / k` prints as `j/k` and `g / G` as
    /// `g/G`, because there the second half is the other direction and a
    /// footer naming one of a pair is a footer that has hidden the other.
    pub keys: &'static str,
    /// The word beside the keys. One word: the footer is a reminder, and the
    /// sentence lives in `Key::help`.
    pub word: &'static str,
    /// Which hints survive a narrow footer — low numbers first. `?`, `q` and
    /// `p` are ranked ahead of everything because they are the three a reader
    /// cannot do without, which is the judgement the deleted `Rung::Xs` arm
    /// used to carry as a literal.
    pub rank: u8,
}

pub const KEYS: &[Key] = &[
    Key {
        keys: "1-8",
        help: "focus a panel (or place it in the analytics slot)",
        footer: Some(Hint {
            keys: "1-8",
            word: "panel",
            rank: 4,
        }),
    },
    Key {
        keys: "tab / S-tab",
        help: "next / previous panel",
        footer: Some(Hint {
            keys: "tab",
            word: "cycle",
            rank: 5,
        }),
    },
    Key {
        keys: "j / k",
        help: "move the cursor in the focused panel",
        footer: Some(Hint {
            keys: "j/k",
            word: "move",
            rank: 6,
        }),
    },
    Key {
        keys: "g / G",
        help: "cursor to the first / last row",
        footer: Some(Hint {
            keys: "g/G",
            word: "ends",
            rank: 10,
        }),
    },
    Key {
        keys: "r",
        help: "refresh now",
        footer: Some(Hint {
            keys: "r",
            word: "refresh",
            rank: 7,
        }),
    },
    Key {
        keys: "R",
        help: "toggle auto-refresh",
        footer: Some(Hint {
            keys: "R",
            word: "auto",
            rank: 11,
        }),
    },
    Key {
        keys: "w",
        help: "cycle the burndown window",
        footer: Some(Hint {
            keys: "w",
            word: "window",
            rank: 9,
        }),
    },
    Key {
        keys: "enter",
        help: "open the row under the cursor",
        footer: Some(Hint {
            keys: "enter",
            word: "open",
            rank: 3,
        }),
    },
    Key {
        keys: "p",
        help: "pick a task and start it",
        footer: Some(Hint {
            keys: "p",
            word: "pick",
            rank: 2,
        }),
    },
    Key {
        keys: "l",
        help: "leave and print the task list",
        footer: Some(Hint {
            keys: "l",
            word: "list",
            rank: 8,
        }),
    },
    Key {
        keys: "?",
        help: "this help",
        footer: Some(Hint {
            keys: "?",
            word: "help",
            rank: 0,
        }),
    },
    Key {
        keys: "q / esc",
        help: "close",
        footer: Some(Hint {
            keys: "q",
            word: "close",
            rank: 1,
        }),
    },
    Key {
        keys: "ctrl-c",
        help: "close, always",
        footer: None,
    },
];

#[cfg(test)]
mod tests;
