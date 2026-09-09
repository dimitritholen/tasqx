//! The interactive chooser over the working set: `tasqx pick` (DESIGN.md §10,
//! D55).
//!
//! Same split as [`crate::tui::settings`], and for the same reason: [`App`] is a
//! pure state machine that folds one key press into a query, a cursor and a
//! match list and answers with an [`Action`] the caller performs, while
//! [`render`] draws `&App` into a `Frame` and decides nothing. Neither half
//! touches the terminal, the store or the filesystem, which is what earns a
//! full-screen surface real tests in a repo that fails the build on a warning.
//!
//! # What Enter does, and why it is exactly one thing
//!
//! Enter STARTS the highlighted task (`task.start`) and leaves. The spec sketch
//! in DESIGN.md §10 drew four dispatch keys on this screen — `⏎` printing a ref,
//! `^s` start, `^d` done, `^e` edit — and one is shipped, because the ref-print
//! form does not survive contact with the TTY gate this screen has to pass:
//! [`crate::tui::is_interactive`] refuses when stdout is redirected, so
//! `tasqx pick | tasqx done` and `$(tasqx pick)` — the only two things a printed
//! ref is FOR — are precisely the invocations that never reach the screen. A
//! chooser whose answer can only be read and retyped by hand is a slower
//! `tasqx list`. Starting the task is the one outcome that is complete on the
//! surface where the screen can actually run: `pick` answers "which of these am
//! I doing now", and beginning it is that answer.
//!
//! The other three keys are absent rather than deferred-and-hinted, because a
//! footer advertising `^d done` on a screen that ignores it is worse than a
//! footer that does not mention it.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::render;
use crate::theme::{Caps, Theme};
use crate::tui::{first_visible, rt_style};

/// One candidate task, flattened out of a `task.list` row.
///
/// Constructed only through [`Row::new`], which is not ceremony: the constructor
/// is where every display string goes through [`render::san`] and where the
/// searchable `fields` are derived FROM those sanitised strings. Both properties
/// are ones a second construction site would silently drop — a public struct
/// literal would let a caller build a row whose search text disagrees with its
/// title (so the screen shows a row the query says does not match), or whose
/// title still carries the C1 escapes an imported or agent-authored task can
/// contain. A `Paragraph` cell is written to the terminal verbatim, so an
/// unsanitised title here is the same hole `render::san` exists to close on the
/// printed path (D19).
pub struct Row {
    pub short_id: i64,
    pub title: String,
    pub project: String,
    /// `H`/`M`/`L`, or `-` when the task carries none.
    pub priority: String,
    /// Already formatted (`11.8`), because the ranking that produced this order
    /// happened in the store and this screen must not re-derive it.
    pub urgency: String,
    pub tags: String,
    /// The searchable fields, lowercased and kept SEPARATE: the id, the title,
    /// the project and the tag list. A term matches this row when it is a
    /// subsequence of any ONE of them.
    ///
    /// One concatenated haystack was the first version and it was wrong in a
    /// way that only shows up on real data: a subsequence may take each letter
    /// from a different field, so with every task in `work.tasqx` the query
    /// `wac` matched "Publish API docs" — `w` from the project, `a` from
    /// `tasqx`, `c` from `docs` — and the user, who typed the initials of a
    /// title, got back rows sharing no word with what they typed. Per field,
    /// the letters have to come from one thing the user can see.
    ///
    /// Priority is deliberately absent: `!H` is one letter that also appears in
    /// half the titles in any store, so folding it in would make typing `h`
    /// mean two unrelated things at once.
    fields: [String; 4],
}

impl Row {
    pub fn new(
        short_id: i64,
        title: &str,
        project: &str,
        priority: &str,
        urgency: &str,
        tags: &str,
    ) -> Self {
        let title = render::san(title);
        let project = render::san(project);
        let tags = render::san(tags);
        // Derived here, from the sanitised text, so the query cannot match on
        // bytes the screen does not draw.
        let fields = [
            short_id.to_string(),
            title.to_lowercase(),
            project.to_lowercase(),
            tags.to_lowercase(),
        ];
        Row {
            short_id,
            title,
            project,
            priority: render::san(priority),
            urgency: render::san(urgency),
            tags,
            fields,
        }
    }

    /// How well does this row match every term of an already-lowercased
    /// query? `None` when some term matches no field at all (the row is not a
    /// candidate); `Some(score)` otherwise, higher is better (D55 amendment,
    /// #203).
    ///
    /// Each term picks its OWN best-scoring field independently — the AND is
    /// still "every term matches something", exactly as the old boolean
    /// `matches` read it, just with "something" now graded rather than
    /// binary. A row where "api" hits the title and "test" only hits the tags
    /// is exactly as valid a candidate as before; only the ORDER among
    /// candidates is new information.
    fn score(&self, terms: &[&str]) -> Option<i64> {
        let mut total = 0i64;
        for t in terms {
            let best = self
                .fields
                .iter()
                .enumerate()
                .filter_map(|(idx, f)| score_subsequence(f, t).map(|s| s + FIELD_WEIGHT[idx]))
                .max()?;
            total += best;
        }
        Some(total)
    }
}

/// Per-field bonus added on top of a term's match score, in `fields`' own
/// order (id, title, project, tags). The suggested fix this ships (audit
/// #203) is explicit that a query should favour "the thing with a name" —
/// title or id — over incidental metadata, so a query that happens to also
/// scan as a subsequence of some unrelated task's tags does not outrank the
/// task actually named by what was typed.
const FIELD_WEIGHT: [i64; 4] = [250, 250, 50, 50];

/// An intent for the caller to carry out. `App` performs nothing itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Start this task and leave the screen.
    Choose { short_id: i64 },
    /// Leave having chosen nothing. The caller must not treat this as success —
    /// `pick` produced no task, and a command that produced nothing may not
    /// exit 0.
    Cancel,
}

pub struct App {
    rows: Vec<Row>,
    /// What the user has typed. Public because the caller's loop has nothing to
    /// do with it and `render` is the only reader — but tests assert on it, and
    /// a private field with a getter would be the same thing with more words.
    pub query: String,
    /// Indices into `rows` that match `query`, in the store's order.
    matches: Vec<usize>,
    /// Position within `matches`, NOT within `rows`.
    cursor: usize,
}

impl App {
    pub fn new(rows: Vec<Row>) -> Self {
        let matches = (0..rows.len()).collect();
        App {
            rows,
            query: String::new(),
            matches,
            cursor: 0,
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The indices of `rows` currently on screen, in display order.
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The highlighted row, or `None` when nothing matches the query.
    ///
    /// `None` is a real state, not a defensive `Option`: an empty store and a
    /// query that narrows to nothing both land here, and every caller has to
    /// answer for it rather than reach for row 0.
    pub fn selected(&self) -> Option<&Row> {
        self.matches.get(self.cursor).map(|i| &self.rows[*i])
    }

    /// Fold one key press into the state, returning what the caller must do.
    ///
    /// Pure: no terminal, no store, no environment.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Windows crossterm sends a Release for every Press. Without this the
        // query gains two characters per keystroke and Down skips a row — the
        // same filter, for the same reason, as `settings::App::on_key`.
        if key.kind != KeyEventKind::Press {
            return None;
        }
        // Modified keys are checked BEFORE the plain ones, because on this
        // screen every unmodified printable character is text. That is also why
        // `j`/`k` navigate the settings screen and not this one: there they are
        // the only thing a letter can mean, here they are a letter the user is
        // typing into a query. Ctrl-N/Ctrl-P are the readline spellings, which
        // is what a picker with a query line conventionally offers instead.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Some(Action::Cancel),
                KeyCode::Char('n') => {
                    self.step(1);
                    None
                }
                KeyCode::Char('p') => {
                    self.step(-1);
                    None
                }
                _ => None,
            };
        }
        match key.code {
            KeyCode::Up => {
                self.step(-1);
                None
            }
            KeyCode::Down => {
                self.step(1);
                None
            }
            // Esc clears a query before it closes the screen, the same
            // narrower-thing-first rule Esc follows in the settings picker. A
            // mistyped query is the most common reason to press Esc here, and
            // making that cost the whole screen means retyping the filter on
            // the command line to get back to where you were.
            KeyCode::Esc => {
                if self.query.is_empty() {
                    return Some(Action::Cancel);
                }
                self.query.clear();
                self.refilter();
                None
            }
            KeyCode::Enter => self.selected().map(|r| Action::Choose {
                short_id: r.short_id,
            }),
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
                None
            }
            KeyCode::Char(c) => {
                self.query.push(c);
                self.refilter();
                None
            }
            _ => None,
        }
    }

    /// Move the cursor by `delta`, clamped at both ends.
    ///
    /// Clamped and never wrapped: `cursor - 1` at the top is a usize underflow,
    /// which panics in debug and indexes out of bounds in release — inside a
    /// raw-mode alt screen, where the panic message is the last thing the user
    /// can read.
    fn step(&mut self, delta: isize) {
        let last = self.matches.len().saturating_sub(1);
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            (self.cursor + delta as usize).min(last)
        };
    }

    /// Recompute the match list, keeping the highlight on the SAME TASK when it
    /// survives the narrower query.
    ///
    /// Not cosmetic. The cursor indexes `matches`, so leaving it alone across a
    /// refilter silently re-aims it at whatever task now sits at that position —
    /// the user types one more character and Enter starts a task they never
    /// highlighted. Re-finding the task and only then clamping is the fix;
    /// resetting to 0 would also be safe but throws away a deliberate
    /// navigation on every keystroke.
    fn refilter(&mut self) {
        let anchor = self.matches.get(self.cursor).copied();
        let needle = self.query.to_lowercase();
        let terms: Vec<&str> = needle.split_whitespace().collect();
        if terms.is_empty() {
            // No query: every row is a candidate, in the order `task.list`
            // handed to `App::new` — there is nothing here for a score to
            // improve on, and re-sorting an unfiltered list by an arbitrary
            // match score against an empty needle would replace one
            // meaningless order with another.
            self.matches = (0..self.rows.len()).collect();
        } else {
            let mut scored: Vec<(usize, i64)> = (0..self.rows.len())
                .filter_map(|i| self.rows[i].score(&terms).map(|s| (i, s)))
                .collect();
            // Stable sort, descending score: ties keep their ORIGINAL
            // relative order, which is `task.list`'s `-urgency` sort — so
            // urgency is the tiebreak for free, exactly as #203 asks, with no
            // second key to compute or keep in sync with the caller's sort.
            scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
            self.matches = scored.into_iter().map(|(i, _)| i).collect();
        }
        self.cursor = anchor
            .and_then(|row| self.matches.iter().position(|i| *i == row))
            .unwrap_or(0)
            .min(self.matches.len().saturating_sub(1));
    }
}

/// How well does `needle`'s characters fit inside `haystack`, in order but
/// not necessarily adjacent? Both must already be lowercased. `None` when
/// `needle` is not a subsequence of `haystack` at all; `Some(score)`
/// otherwise, higher for a tighter, earlier-starting run (#203).
///
/// A subsequence rather than a substring, which is what makes the query worth
/// having: `wac` finds "Write API conformance tests" without the user
/// remembering where the word boundaries were. Whitespace in the query splits
/// it into independent terms (see `refilter`), so `api test` is an AND of two
/// subsequence matches and not one match against a literal space — a space is
/// in no field, so a literal reading would make the query unusable the moment
/// the user typed one.
///
/// The match itself is found greedily (earliest possible character each
/// step), which is not always the tightest span a subsequence could occupy —
/// only good enough to RANK matches against each other, which is all a
/// typeahead needs. Existence (does it match at all) is unaffected by the
/// greedy choice; only the score of an already-matching row could, in a rare
/// case, be a little pessimistic.
fn score_subsequence(haystack: &str, needle: &str) -> Option<i64> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let mut cursor = 0usize;
    let mut first = None;
    let mut last = 0usize;
    for c in needle.chars() {
        while cursor < hay.len() && hay[cursor] != c {
            cursor += 1;
        }
        if cursor >= hay.len() {
            return None;
        }
        first.get_or_insert(cursor);
        last = cursor;
        cursor += 1;
    }
    let first = first.unwrap_or(0);
    let needle_len = needle.chars().count() as i64;
    let span = (last - first + 1) as i64;
    // Characters inside the match that the needle did NOT ask for: zero for
    // "api" hitting a literal "api" substring, large for the same three
    // letters scattered across an 80-character title.
    let gaps = span - needle_len;
    Some(1_000 - gaps * 10 - first as i64)
}

// ============================================================================
// Rendering
// ============================================================================

/// Draw the whole screen. Decides nothing: `App` made every choice, and every
/// colour comes from `theme` at `caps`' real depth.
pub fn render(app: &App, theme: &Theme, caps: &Caps, frame: &mut Frame) {
    let sty = |role: &str| rt_style(theme.role(role), caps);
    let area = frame.area();
    let [head, query_area, rule_area, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let marker = if caps.unicode { "▸" } else { ">" };
    let rule = if caps.unicode { "─" } else { "-" };
    // A block, not a real cursor: `with_terminal` hides the terminal cursor on
    // the way in, so the query line has to draw its own or the user cannot see
    // where the characters they type are landing.
    let caret = if caps.unicode { "▊" } else { "_" };

    // --- header ---------------------------------------------------------------
    // The counter is `matches/total`, and it is the only thing on screen that
    // distinguishes "your query matches nothing" from "this store has nothing":
    // both draw an empty body, and they need completely different responses.
    // It is also the only thing that says the list is longer than the window —
    // `first_visible` draws at most one screenful, so a match count larger than
    // the rows on screen is how the user learns there is more below.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("pick a task", sty("header")),
            Span::raw("   "),
            Span::styled(
                format!("{}/{}", app.matches().len(), app.rows().len()),
                sty("muted"),
            ),
        ])),
        head,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", sty("muted")),
            Span::styled(app.query.as_str(), sty("accent")),
            Span::styled(caret, sty("accent")),
        ])),
        query_area,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(rule.repeat(area.width as usize), sty("muted"))),
        rule_area,
    );

    // --- rows -----------------------------------------------------------------
    let mut lines: Vec<Line> = Vec::new();
    if app.matches().is_empty() {
        // Named separately from the counter above because the counter is a
        // number in a corner and this is where the user is looking. An empty
        // body with no sentence in it reads as a hung screen.
        let message = if app.rows().is_empty() {
            "nothing to pick"
        } else {
            "no task matches this query — backspace, or esc to clear it"
        };
        lines.push(Line::styled(message, sty("warn")));
    }
    // Widths from every MATCH, not from the screenful about to be drawn, so a
    // three-digit id or a two-digit urgency does not shove every later column
    // one cell right on that row alone (D51's rule, and the bug
    // `render_config_table` names). Deliberately not per-viewport: the columns
    // would then re-align on every Down press once the list scrolls, which is
    // the same text jittering under a cursor that is not moving.
    // `pad` never truncates; ratatui clips the line at the frame edge, so an
    // over-wide title costs the columns to its right rather than the layout.
    let ids: Vec<String> = app
        .matches()
        .iter()
        .map(|i| format!("#{}", app.rows()[*i].short_id))
        .collect();
    let id_w = ids.iter().map(|s| render::width(s)).max().unwrap_or(0);
    let urg_w = app
        .matches()
        .iter()
        .map(|i| render::width(&app.rows()[*i].urgency))
        .max()
        .unwrap_or(0);
    // The title column is capped rather than fitted to the widest title: one
    // 200-character task would otherwise push PROJECT and TAGS off the right
    // edge of every OTHER row, which is the invisible-field failure again —
    // the fields are there, and nobody can see them.
    let title_w = app
        .matches()
        .iter()
        .map(|i| render::width(&app.rows()[*i].title))
        .max()
        .unwrap_or(0)
        .min(44);
    // PROJECT has no per-frame max like `title_w` — its budget is this fixed
    // cell count, cut two cells short so `pad` always has a two-cell gap to
    // add before TAGS rather than sometimes having none (#202's other half:
    // `pad` never truncates, so a project name at or past this width used to
    // run straight into the tag list with no separator at all).
    const PROJECT_W: usize = 14;

    // The window. `n` stays the index into the FULL match list — it is what the
    // cursor is compared against and what indexes `ids` — so the skip/take pair
    // goes after the `enumerate`, never before it.
    let height = body.height as usize;
    let start = first_visible(app.cursor(), app.matches().len(), height);
    for (n, i) in app.matches().iter().enumerate().skip(start).take(height) {
        let row = &app.rows()[*i];
        let at = n == app.cursor();
        let prio_role = match row.priority.as_str() {
            "H" | "M" | "L" => format!("priority.{}", row.priority),
            _ => "muted".to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                if at {
                    format!("{marker} ")
                } else {
                    "  ".to_string()
                },
                sty("accent"),
            ),
            Span::styled(
                render::pad(&ids[n], id_w + 2),
                if at { sty("accent") } else { sty("muted") },
            ),
            Span::styled(render::pad(&row.urgency, urg_w + 2), sty("muted")),
            Span::styled(render::pad(&row.priority, 3), sty(&prio_role)),
            Span::styled(
                render::pad(
                    &render::truncate(&row.title, title_w, caps.unicode),
                    title_w + 2,
                ),
                if at {
                    sty("accent")
                } else {
                    ratatui::style::Style::default()
                },
            ),
            Span::styled(
                render::pad(
                    &render::truncate(&row.project, PROJECT_W.saturating_sub(2), caps.unicode),
                    PROJECT_W,
                ),
                sty("project"),
            ),
            Span::styled(row.tags.as_str(), sty("tag")),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), body);

    // --- footer ---------------------------------------------------------------
    // `enter start` and not `enter select`: this screen's Enter has a side
    // effect on the store, and a hint that hid that would be the one place a
    // user could learn otherwise.
    frame.render_widget(
        Paragraph::new(Line::styled(
            "type to narrow   up/down move   enter start   esc clear/quit",
            sty("muted"),
        )),
        foot,
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

    /// Four tasks with deliberately overlapping words, so a query that narrows
    /// has something to narrow away from.
    fn app() -> App {
        App::new(vec![
            Row::new(
                42,
                "Ship the v1 JSON API freeze",
                "work.tasqx",
                "H",
                "11.8",
                "release api",
            ),
            Row::new(43, "Publish API docs", "work.tasqx", "M", "6.0", "docs"),
            Row::new(
                47,
                "Write API conformance tests",
                "work.tasqx",
                "M",
                "9.4",
                "api test",
            ),
            Row::new(55, "Draft README quickstart", "home", "L", "4.2", "docs"),
        ])
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
    }

    fn ids(app: &App) -> Vec<i64> {
        app.matches()
            .iter()
            .map(|i| app.rows()[*i].short_id)
            .collect()
    }

    fn draw(app: &App, w: u16, h: u16) -> Buffer {
        let th = theme::load("nord", None);
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

    // ---- state machine ------------------------------------------------------

    /// Up/Down must clamp, not wrap and not underflow. `cursor - 1` at the top
    /// row is a usize underflow: a panic in debug, an out-of-bounds index in
    /// release, both inside a raw-mode alt screen.
    #[test]
    fn the_cursor_clamps_at_both_ends() {
        let mut a = app();
        assert_eq!(a.cursor(), 0);
        assert!(a.on_key(press(KeyCode::Up)).is_none());
        assert_eq!(a.cursor(), 0, "up at the top row must stay put");

        for _ in 0..10 {
            a.on_key(press(KeyCode::Down));
        }
        assert_eq!(
            a.cursor(),
            a.matches().len() - 1,
            "down past the end must stay on the last row"
        );
        assert_eq!(a.selected().map(|r| r.short_id), Some(55));
    }

    /// An empty candidate set must survive every key without panicking, and
    /// Enter on it must produce NOTHING. Reaching for row 0 here is the
    /// index-out-of-bounds this screen is most likely to ship: the caller
    /// refuses an empty store before opening the screen, so the only way in is
    /// a store that empties under a query — which is the next test.
    #[test]
    fn an_empty_working_set_navigates_and_refuses_to_choose() {
        let mut a = App::new(Vec::new());
        assert!(a.selected().is_none());
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Backspace] {
            assert!(a.on_key(press(code)).is_none(), "{code:?}");
        }
        assert_eq!(a.cursor(), 0);
        assert!(
            a.on_key(press(KeyCode::Enter)).is_none(),
            "enter with nothing to pick must not choose a task"
        );
        // And it must still be leavable, or the user has to kill the process.
        assert_eq!(a.on_key(press(KeyCode::Esc)), Some(Action::Cancel));
    }

    /// Enter on a query that matches nothing must not start anything. The
    /// failure it guards is this project's named one: a screen that answers a
    /// keystroke by doing less than it looks like it did — here, silently
    /// starting whatever task happens to sit at index 0 of the unfiltered list.
    #[test]
    fn enter_on_a_query_that_matches_nothing_starts_nothing() {
        let mut a = app();
        typed(&mut a, "zzzz");
        assert!(ids(&a).is_empty(), "the fixture query must match nothing");
        assert!(
            a.on_key(press(KeyCode::Enter)).is_none(),
            "enter must not fall back to an unmatched row"
        );
        assert!(a.selected().is_none());
    }

    /// The query narrows by SUBSEQUENCE, not substring, and whitespace splits
    /// it into independent terms. Both are the feature: `wac` finds a title
    /// whose words the user cannot remember the order of, and `test api`
    /// intersects two terms rather than matching one literal string.
    #[test]
    fn the_query_narrows_by_subsequence_and_ands_its_terms() {
        let mut a = app();
        assert_eq!(ids(&a), vec![42, 43, 47, 55], "an empty query matches all");

        typed(&mut a, "wac");
        assert_eq!(
            ids(&a),
            vec![47],
            "a subsequence of `Write API conformance` must match it"
        );

        // The terms are given in the OPPOSITE order to the text, which is what
        // makes this falsifiable: `test api` read as one literal subsequence
        // matches nothing, because no field has a `t…e…s…t…␣…a…p…i` run. Only an
        // intersection of two independent terms finds #47. `api test`, the
        // natural spelling, passes either way — a subsequence is free to
        // consume the space in "write api conformance tests" — so it would have
        // asserted nothing.
        let mut b = app();
        typed(&mut b, "test api");
        assert_eq!(
            ids(&b),
            vec![47],
            "two terms must AND, not match one literal string with a space in it"
        );

        let mut c = app();
        typed(&mut c, "API");
        let mut got_c = ids(&c);
        got_c.sort_unstable();
        assert_eq!(
            got_c,
            vec![42, 43, 47],
            "matching is case-insensitive (ranking decides the ORDER, tested separately)"
        );

        // The haystack is more than the title: id, project and tags are all
        // things a user reaches for.
        let mut d = app();
        typed(&mut d, "55");
        assert_eq!(ids(&d), vec![55], "the short_id must be searchable");
        let mut e = app();
        typed(&mut e, "home");
        assert_eq!(ids(&e), vec![55], "the project must be searchable");
    }

    /// #203: no ranking meant `matches` kept the ORIGINAL list order (the
    /// order `task.list` handed to `App::new`) for every query, so a three- or
    /// four-character query narrowed the set without ever promoting the task
    /// it actually found. A picker that ships under the alias `fzf` has to
    /// put the best match on the cursor row regardless of where the candidate
    /// sat before the query, or the "fast path" is abandoning the picker.
    ///
    /// #10 sits FIRST in the row order (as if `task.list` returned it at the
    /// top by urgency) and only matches "mem" as a scattered subsequence
    /// spread across the whole title; #90 sits SECOND and matches it as one
    /// contiguous run at a word boundary. The unranked code drew #10 above
    /// #90 because that was the input order; ranking must reverse it.
    #[test]
    fn ranking_promotes_the_contiguous_match_over_a_scattered_one_regardless_of_input_order() {
        // `ids()` reads `App::matches()`, which `refilter` rebuilds from
        // scratch against the CURRENT query on every keystroke — unlike
        // `selected()`, it carries no memory of which task the cursor was
        // anchored to a keystroke ago, so it is the direct way to assert on
        // the ranking itself without the identity-preserving anchor (D55,
        // "the cursor follows the task, not the index") deciding the answer
        // instead.
        let mut a = App::new(vec![
            Row::new(
                10,
                "Random unrelated meeting notes for email marketing",
                "work",
                "H",
                "9.9",
                "misc",
            ),
            Row::new(
                90,
                "Fix the memory leak in the parser",
                "work",
                "L",
                "2.0",
                "bugfix",
            ),
        ]);
        typed(&mut a, "mem");
        assert_eq!(
            ids(&a),
            vec![90, 10],
            "the tight, contiguous match on #90 must rank above the scattered \
             one on #10 that only happens to sit first in the input order"
        );
    }

    /// Narrowing must keep the highlight on the task it was already on. The
    /// cursor indexes the MATCH list, so leaving it where it was re-aims it at
    /// a different task on every keystroke — and Enter then starts a task the
    /// user never highlighted, which is the worst outcome this screen has.
    #[test]
    fn narrowing_keeps_the_highlight_on_the_same_task() {
        let mut a = app();
        a.on_key(press(KeyCode::Down));
        a.on_key(press(KeyCode::Down));
        assert_eq!(a.selected().map(|r| r.short_id), Some(47));

        // A query that changes the highlighted task's RANKED position, not
        // just its presence: `test` ranks #55 above #47 (#203 — #55's title
        // is a tighter, earlier-starting match), so #47 moves from rank 0 in
        // the unfiltered order to rank 1 here. The identity lookup has to
        // re-find it at its NEW position; a version that forgot to re-rank,
        // or that clamped the old cursor instead of re-anchoring it, is what
        // the second half of this test (below) catches outright.
        typed(&mut a, "test");
        assert_eq!(ids(&a), vec![55, 47]);
        assert_eq!(
            a.selected().map(|r| r.short_id),
            Some(47),
            "the highlight followed the index instead of the task"
        );
        assert_eq!(
            a.cursor(),
            1,
            "the anchored task must sit at its ranked position"
        );

        // And when the highlighted task falls out of the set, the cursor must
        // land inside the new one rather than past its end.
        typed(&mut a, " home");
        assert_eq!(ids(&a), vec![55]);
        assert!(
            a.cursor() < a.matches().len(),
            "cursor {} is outside a {}-row match list",
            a.cursor(),
            a.matches().len()
        );
        assert_eq!(a.selected().map(|r| r.short_id), Some(55));
    }

    /// Enter answers with the task under the cursor, by `short_id`, out of the
    /// FILTERED list. Returning the unfiltered index is the same class of bug
    /// as the one above and would start a completely unrelated task.
    #[test]
    fn enter_chooses_the_task_under_the_cursor() {
        let mut a = app();
        // `test` keeps #47 and #55, ranked (#203) with #55 first. Cursor 1 is
        // #47 in this ranked list and #43 in the unfiltered row order — the
        // two readings disagree, which is the only kind of fixture that can
        // catch a caller that read the wrong index.
        typed(&mut a, "test");
        assert_eq!(ids(&a), vec![55, 47]);
        a.on_key(press(KeyCode::Down));
        assert_eq!(
            a.on_key(press(KeyCode::Enter)),
            Some(Action::Choose { short_id: 47 }),
            "enter must name the highlighted row of the narrowed list"
        );
    }

    /// Every unmodified printable key is TEXT here. `j`, `k` and `q` navigate
    /// or quit the settings screen; on a screen with a query line they must
    /// land in the query, or a task called "jam jars" is unsearchable and `q`
    /// throws the session away mid-word.
    #[test]
    fn letters_that_are_commands_elsewhere_are_typed_here() {
        let mut a = app();
        for c in ['j', 'k', 'q'] {
            let mut b = app();
            assert!(
                b.on_key(press(KeyCode::Char(c))).is_none(),
                "`{c}` must not be a command on this screen"
            );
            assert_eq!(b.query, c.to_string(), "`{c}` was swallowed, not typed");
        }
        typed(&mut a, "quick");
        assert_eq!(a.query, "quick");
        assert_eq!(ids(&a), vec![55], "the typed word still narrowed the list");
    }

    /// Ctrl-N/Ctrl-P move, because the arrow keys are not the only spelling and
    /// the letters cannot be. Ctrl-C leaves from any state, including mid-query
    /// — a modal screen that traps the conventional interrupt is how a TUI ends
    /// up killed from another window with the terminal still in raw mode.
    #[test]
    fn the_control_keys_navigate_and_interrupt() {
        let mut a = app();
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);

        assert!(a.on_key(ctrl('n')).is_none());
        assert_eq!(
            a.selected().map(|r| r.short_id),
            Some(43),
            "ctrl-n must move down the list"
        );
        assert!(a.on_key(ctrl('p')).is_none());
        assert_eq!(
            a.selected().map(|r| r.short_id),
            Some(42),
            "ctrl-p must move back up"
        );
        assert!(
            a.query.is_empty(),
            "a control chord must not type its letter: {:?}",
            a.query
        );

        typed(&mut a, "api");
        assert_eq!(
            a.on_key(ctrl('c')),
            Some(Action::Cancel),
            "ctrl-c must leave even with a query in progress"
        );
    }

    /// Esc clears a query before it closes the screen. Retyping a filter on the
    /// command line to undo one mistyped character is the cost of the other
    /// rule, and the settings picker already reads Esc as "the narrower thing
    /// first".
    #[test]
    fn esc_clears_the_query_first_and_only_then_quits() {
        let mut a = app();
        typed(&mut a, "api");
        // Ranked (#203), not the input order — the exact ranking is what
        // `ranking_promotes_…` pins; this test only needs a query that keeps
        // some rows narrowed and then restores all four on a clear.
        assert_eq!(ids(&a), vec![47, 43, 42]);

        assert!(
            a.on_key(press(KeyCode::Esc)).is_none(),
            "esc with a query must not leave the screen"
        );
        assert!(a.query.is_empty());
        assert_eq!(ids(&a), vec![42, 43, 47, 55], "clearing must restore all");

        assert_eq!(
            a.on_key(press(KeyCode::Esc)),
            Some(Action::Cancel),
            "a second esc, on an empty query, leaves"
        );
    }

    /// Backspace edits the query one character at a time and the match list
    /// follows it back out. A backspace that only shortened the string without
    /// re-filtering would leave the screen showing a narrower list than the
    /// query it displays.
    #[test]
    fn backspace_widens_the_match_list_again() {
        let mut a = app();
        typed(&mut a, "apix");
        assert!(ids(&a).is_empty());
        a.on_key(press(KeyCode::Backspace));
        assert_eq!(a.query, "api");
        // Ranked (#203); the point of this assertion is that all three come
        // back, not the order they come back in.
        assert_eq!(ids(&a), vec![47, 43, 42], "the list did not widen back");
    }

    /// Windows crossterm emits Press AND Release for every keystroke. Without
    /// the kind filter every typed character is doubled — `api` becomes `aappii`
    /// and matches nothing — and every Down skips a row. None of that is
    /// visible on Linux CI, which is exactly why it is pinned.
    #[test]
    fn a_key_release_is_not_a_second_key_press() {
        let mut a = app();
        let mut release = press(KeyCode::Char('a'));
        release.kind = KeyEventKind::Release;
        assert!(a.on_key(release).is_none());
        assert!(a.query.is_empty(), "a Release event typed a character");

        let mut release = press(KeyCode::Down);
        release.kind = KeyEventKind::Release;
        a.on_key(release);
        assert_eq!(a.cursor(), 0, "a Release event moved the cursor");
    }

    /// Task text is untrusted: it arrives from `store.import` and from MCP write
    /// tools, and a ratatui cell is written to the terminal verbatim. A title
    /// carrying `\x1b]0;` would retitle the reader's window from inside the alt
    /// screen — the exact hole `render::san` closes on the printed path, which
    /// a second construction site would reopen.
    #[test]
    fn a_row_sanitises_the_untrusted_text_it_is_built_from() {
        let row = Row::new(
            1,
            "quiet\x1b]0;pwned\x07 title",
            "proj\x1b[2J",
            "H",
            "1.0",
            "tag\u{9b}",
        );
        for field in [&row.title, &row.project, &row.tags] {
            assert!(
                !field.chars().any(char::is_control),
                "control bytes survived into {field:?}"
            );
        }
        assert!(row.title.contains("title"), "{:?}", row.title);

        // And the haystack is derived from the sanitised fields, so the query
        // cannot match on bytes the screen never shows.
        let mut a = App::new(vec![row]);
        typed(&mut a, "\u{1b}");
        assert!(
            a.matches().is_empty(),
            "an escape byte must not be searchable"
        );
    }

    // ---- rendering ----------------------------------------------------------

    /// Every matching task must be drawn, with the fields that make one row
    /// distinguishable from another: id, urgency, priority, title, project and
    /// tags. A column silently missing from this screen is the invisible-field
    /// failure this project keeps rebuilding — and here it decides which task
    /// the user starts.
    #[test]
    fn every_candidate_is_drawn_with_the_fields_that_tell_them_apart() {
        let buf = draw(&app(), 100, 12);
        let text = all_text(&buf);
        for needle in [
            "#42",
            "Ship the v1 JSON API freeze",
            "work.tasqx",
            "release api",
            "11.8",
            "#55",
            "Draft README quickstart",
            "home",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
        }
        assert!(
            text.contains("enter start"),
            "the footer must say what enter does:\n{text}"
        );
    }

    /// The marker is the only thing telling the user which task Enter will
    /// start, and it has to track the cursor through a narrowing query — not
    /// just through arrow keys.
    #[test]
    fn the_marker_sits_on_the_highlighted_task() {
        let mut a = app();
        let before = all_text(&draw(&a, 100, 12));
        assert!(
            before
                .lines()
                .any(|l| l.starts_with("▸") && l.contains("#42")),
            "{before}"
        );

        a.on_key(press(KeyCode::Down));
        typed(&mut a, "api");
        let after = all_text(&draw(&a, 100, 12));
        assert!(
            after
                .lines()
                .any(|l| l.starts_with("▸") && l.contains("#43")),
            "the marker did not follow the cursor:\n{after}"
        );
        assert!(
            !after
                .lines()
                .any(|l| l.starts_with("▸") && l.contains("#42")),
            "two rows are marked:\n{after}"
        );
    }

    /// The query the user typed has to be ON SCREEN. A picker that filters
    /// invisibly looks broken the moment a keystroke is dropped or doubled,
    /// and the counter is what separates "no match" from "empty store".
    #[test]
    fn the_query_and_the_match_counter_are_drawn() {
        let mut a = app();
        typed(&mut a, "api");
        let text = all_text(&draw(&a, 100, 12));
        assert!(text.contains("> api"), "the query line is missing:\n{text}");
        assert!(text.contains("3/4"), "the counter is missing:\n{text}");

        typed(&mut a, "zz");
        let text = all_text(&draw(&a, 100, 12));
        assert!(text.contains("0/4"), "{text}");
        assert!(
            text.contains("no task matches"),
            "an empty result must say so in words, not just in the counter:\n{text}"
        );
    }

    /// The two empty screens must not read the same. `0/0` with a query the
    /// user typed means "narrow it differently"; `0/0` with no rows at all
    /// means "this filter has no tasks" — opposite responses, and the caller
    /// refuses the second case before the screen opens, so the sentence here is
    /// what a store emptied by a concurrent write would show.
    #[test]
    fn an_empty_candidate_list_says_so_rather_than_drawing_a_blank_screen() {
        let text = all_text(&draw(&App::new(Vec::new()), 100, 12));
        assert!(text.contains("0/0"), "{text}");
        assert!(text.contains("nothing to pick"), "{text}");
    }

    /// On a terminal with no Unicode the marker, the rule and the caret must
    /// degrade to ASCII, exactly as `Ctx::hrule`/`Ctx::arrow` and the settings
    /// screen already do. Box-drawing bytes on a legacy Windows console render
    /// as mojibake and misalign every column.
    #[test]
    fn a_non_unicode_terminal_gets_ascii_glyphs() {
        let a = app();
        let ascii = Caps {
            depth: ColorDepth::Ansi16,
            ansi: true,
            unicode: false,
        };
        let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
        term.draw(|f| render(&a, &theme::load("nord", None), &ascii, f))
            .unwrap();
        let text = all_text(term.backend().buffer());

        assert!(
            !text.contains('▸') && !text.contains('─') && !text.contains('▊'),
            "Unicode leaked into ASCII mode:\n{text}"
        );
        assert!(
            text.lines()
                .any(|l| l.starts_with("> ") && l.ends_with('_')),
            "no ASCII caret on the query line:\n{text}"
        );
        assert!(
            text.lines().any(|l| l.starts_with("> #42")),
            "no ASCII marker on the highlighted row:\n{text}"
        );
    }

    /// #202: an over-wide title must be cut with an ellipsis, not glued to
    /// PROJECT. `title_w` was capped at 44 so ALL rows keep their alignment,
    /// but the cap only bounded the column WIDTH used for padding — the title
    /// text itself still went through `render::pad`, which never truncates —
    /// so a title past the cap ran straight into PROJECT with no separator at
    /// all, on the very row the cap exists to protect.
    #[test]
    fn an_over_wide_title_is_truncated_with_a_visible_gap_before_project() {
        let long_title =
            "VH-STD-001 ratificatie: Owner invullen, D39 STS-vraag, effective date (RC 28-7)";
        let a = App::new(vec![Row::new(
            9,
            long_title,
            "qore-architecture",
            "H",
            "18.5",
            "pr-55 vh-std",
        )]);
        let text = all_text(&draw(&a, 200, 12));
        let row = text.lines().find(|l| l.contains('9')).expect("row drawn");
        assert!(
            row.contains('…') || row.contains("..."),
            "an over-wide title must be truncated with an ellipsis: {row:?}"
        );
        assert!(
            !row.contains("(RC 28-7)qore-architecture"),
            "the title ran straight into PROJECT with no gap: {row:?}"
        );
    }

    /// #202's second half: PROJECT has the same `pad`-never-truncates hole —
    /// its budget is a hardcoded 14 cells with no truncation either, so a
    /// project name past that width (real ones routinely are) runs straight
    /// into TAGS with no separator, the same invisible-field failure one
    /// column over.
    #[test]
    fn an_over_wide_project_is_truncated_with_a_visible_gap_before_tags() {
        let a = App::new(vec![Row::new(
            9,
            "short title",
            "qore-architecture", // 18 cells, over any budget under ~16
            "H",
            "18.5",
            "pr-55 vh-std",
        )]);
        let text = all_text(&draw(&a, 200, 12));
        let row = text.lines().find(|l| l.contains('9')).expect("row drawn");
        assert!(
            !row.contains("qore-architecturepr-55"),
            "PROJECT ran straight into TAGS with no gap: {row:?}"
        );
    }

    /// #202 at a narrow terminal: the observed failure was not just a missing
    /// ellipsis but PROJECT vanishing entirely, because the untruncated title
    /// pushed it past the frame edge where ratatui silently clips the whole
    /// `Line`. Bounding the title to its cap must keep PROJECT on screen.
    #[test]
    fn a_narrow_terminal_still_shows_project_after_an_over_wide_title() {
        let long_title =
            "VH-STD-001 ratificatie: Owner invullen, D39 STS-vraag, effective date (RC 28-7)";
        let a = App::new(vec![Row::new(
            9,
            long_title,
            "qore-arch",
            "H",
            "18.5",
            "vh-std",
        )]);
        let text = all_text(&draw(&a, 80, 12));
        let row = text.lines().find(|l| l.contains('9')).expect("row drawn");
        assert!(
            row.contains("qore-arch"),
            "PROJECT was pushed off the 80-col frame by an untruncated title: {row:?}"
        );
    }

    /// The same property through the REAL render, on a terminal too short for
    /// the candidate list. The pure function above can be right while `render`
    /// ignores it — which is exactly what shipped: every match was pushed into
    /// one `Paragraph` from index 0, so past the last visible row nothing on
    /// screen was marked at all and `⏎` started a task the user never saw.
    #[test]
    fn a_list_taller_than_the_terminal_scrolls_the_marker_into_view() {
        let mut a = app();
        // 6 rows total: header, query, rule, footer and TWO body rows left over,
        // so two candidates of the fixture's four fit and the last two are only
        // reachable by scrolling. This is the size the review's probe used.
        for (presses, expected) in [(0, "#42"), (1, "#43"), (2, "#47"), (3, "#55")] {
            let mut b = app();
            for _ in 0..presses {
                b.on_key(press(KeyCode::Down));
            }
            let text = all_text(&draw(&b, 100, 6));
            assert!(
                text.lines()
                    .any(|l| l.starts_with('▸') && l.contains(expected)),
                "after {presses} Down the marker on {expected} was not drawn:\n{text}"
            );
        }

        // The task the marker sits on must be the task Enter names: a viewport
        // that scrolled the display without agreeing with the cursor would draw
        // a marked row and start a different one.
        for _ in 0..3 {
            a.on_key(press(KeyCode::Down));
        }
        let text = all_text(&draw(&a, 100, 6));
        assert!(
            text.contains("#55") && !text.contains("#42"),
            "the window did not scroll off the top:\n{text}"
        );
        assert_eq!(
            a.on_key(press(KeyCode::Enter)),
            Some(Action::Choose { short_id: 55 })
        );
    }

    /// The screen is painted through the theme at the terminal's real depth,
    /// like every other tasqx surface — not in colours of its own. A render
    /// path that ignored the theme it is handed would make `--theme` and
    /// `theme.name` mean nothing here while meaning something everywhere else.
    #[test]
    fn the_screen_is_painted_in_the_theme_it_is_handed() {
        let a = app();
        let mut nord = Terminal::new(TestBackend::new(100, 12)).unwrap();
        nord.draw(|f| render(&a, &theme::load("nord", None), &caps(), f))
            .unwrap();
        let mut gruvbox = Terminal::new(TestBackend::new(100, 12)).unwrap();
        gruvbox
            .draw(|f| render(&a, &theme::load("gruvbox", None), &caps(), f))
            .unwrap();

        let nord_fg = nord.backend().buffer()[(0, 0)].fg;
        let gruvbox_fg = gruvbox.backend().buffer()[(0, 0)].fg;
        assert_eq!(
            nord_fg,
            rt_style(theme::load("nord", None).role("header"), &caps())
                .fg
                .unwrap()
        );
        assert_ne!(
            nord_fg, gruvbox_fg,
            "the screen ignored the theme it was given"
        );
    }
}
