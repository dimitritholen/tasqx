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
    scroll: HashMap<PanelId, usize>,
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
        App {
            dash,
            order,
            focus,
            slot: PanelId::Burndown,
            scroll: HashMap::new(),
            placed: Vec::new(),
            has_slot: false,
            window: WINDOW_CHOICES
                .iter()
                .position(|(_, d)| *d == window_days)
                .unwrap_or(0),
            auto_refresh,
            help: false,
            status: String::new(),
        }
    }

    #[cfg(test)]
    pub fn dash(&self) -> &Dashboard {
        &self.dash
    }

    pub fn order(&self) -> &[PanelId] {
        &self.order
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

    pub fn auto_refresh(&self) -> bool {
        self.auto_refresh
    }

    pub fn window_days(&self) -> usize {
        WINDOW_CHOICES[self.window].1
    }

    pub fn scroll_of(&self, id: PanelId) -> usize {
        self.scroll.get(&id).copied().unwrap_or(0)
    }

    /// Replace the data after a refresh, keeping focus, slot and scroll.
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
    fn reachable(&self, id: PanelId) -> bool {
        self.placed.contains(&id) || (self.has_slot && PanelId::SLOT_MEMBERS.contains(&id))
    }

    /// How many rows the focused panel could scroll through.
    fn rows_in(&self, id: PanelId) -> usize {
        match id {
            PanelId::Next => self.dash.next.rows.len(),
            PanelId::Blocked => self.dash.blocked.rows.len(),
            PanelId::Recent => self.dash.recent.rows.len(),
            PanelId::Projects => self.dash.projects.rows.len(),
            PanelId::Tokens => self.dash.tokens.rows.len(),
            PanelId::Due => {
                let d = &self.dash.due;
                d.overdue.len() + d.today.len() + d.tomorrow.len() + d.week.len()
            }
            _ => 0,
        }
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

        match key.code {
            // Narrower-thing-first, the rule `pick`'s Esc already follows: a
            // reader who opened help and wants it gone must not lose the screen.
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.help {
                    self.help = false;
                    None
                } else {
                    Some(Action::Quit)
                }
            }
            KeyCode::Char(c @ '1'..='8') => {
                let id = panel_of_digit(c as u8 - b'0')?;
                if self.reachable(id) {
                    self.aim(id);
                } else {
                    self.status = format!("{} does not fit at this size", id.title());
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
                let cur = self.scroll_of(self.focus);
                self.scroll.insert(self.focus, (cur + 1).min(max));
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                // Saturating, not `- 1`: an underflow here panics inside a raw
                // mode alt screen, where the message is wiped before it can be
                // read.
                let cur = self.scroll_of(self.focus);
                self.scroll.insert(self.focus, cur.saturating_sub(1));
                None
            }
            KeyCode::Char('g') => {
                self.scroll.insert(self.focus, 0);
                None
            }
            KeyCode::Char('G') => {
                let max = self.rows_in(self.focus).saturating_sub(1);
                self.scroll.insert(self.focus, max);
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
    let Some(screen) = model::layout(area.width, area.height, &app.order) else {
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
    if app.help {
        draw_help(area, theme, caps, frame);
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
        app.scroll_of(id),
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
        Some(app.status.clone())
    };
    let line = match text {
        Some(t) => Line::from(Span::styled(
            render::truncate(&t, width as usize, caps.unicode),
            accent,
        )),
        None => {
            // Eight hints do not fit in 56 cells, and a footer cut mid-word
            // reads as a broken screen. The narrow rungs keep the three a
            // reader cannot do without.
            let hints: &[(&str, &str)] = match screen.rung {
                model::Rung::Xs | model::Rung::S => &[("?", "help"), ("q", "quit"), ("p", "pick")],
                _ => &[
                    ("1-8", "panel"),
                    ("tab", "cycle"),
                    ("j/k", "scroll"),
                    ("p", "pick"),
                    ("l", "list"),
                    ("r", "refresh"),
                    ("?", "help"),
                    ("q", "quit"),
                ],
            };
            let mut spans = Vec::new();
            for (k, what) in hints {
                spans.push(Span::styled((*k).to_string(), accent));
                spans.push(Span::styled(format!(" {what}   "), muted));
            }
            Line::from(spans)
        }
    };
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(1, screen.footer_y, width.saturating_sub(1), 1),
    );
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
    for (k, what) in KEYS {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<10}"), accent),
            Span::styled((*what).to_string(), RtStyle::default()),
        ]));
    }
    lines.push(Line::from(Span::styled(String::new(), muted)));
    lines.push(Line::from(Span::styled(
        "  any key closes this".to_string(),
        muted,
    )));

    let h = (lines.len() as u16 + 2).min(area.height);
    let w = 46u16.min(area.width);
    let x = (area.width - w) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(x + 1, y + 1, w.saturating_sub(2), h.saturating_sub(1)),
    );
}

/// Every binding, once. The help overlay renders from this, and a gate asserts
/// the two agree — the docs-drift idiom, applied inside the TUI.
pub const KEYS: &[(&str, &str)] = &[
    ("1-8", "focus a panel (or place it in the analytics slot)"),
    ("tab / S-tab", "next / previous panel"),
    ("j / k", "scroll the focused panel"),
    ("g / G", "jump to the top / bottom"),
    ("r", "refresh now"),
    ("R", "toggle auto-refresh"),
    ("w", "cycle the burndown window"),
    ("p", "pick a task and start it"),
    ("l", "leave and print the task list"),
    ("?", "this help"),
    ("q / esc", "close"),
    ("ctrl-c", "close, always"),
];

#[cfg(test)]
mod tests;
