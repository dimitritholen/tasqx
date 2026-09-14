//! Human-readable rendering of API results (DESIGN.md §5, §8).
//!
//! Every function takes a [`Ctx`] (active theme + detected terminal capability)
//! and paints via semantic *role* lookups — `header`, `project`, `overdue`,
//! `urgency.ramp` — never a literal color. The one render pipeline adapts to the
//! terminal: truecolor/256/16 color, `NO_COLOR` emphasis-only, or byte-plain
//! when piped (script-safe). Unicode rules degrade to ASCII on the same signal.

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Unit};
use serde_json::{json, Value};
use tasqx_core::filter::overdue_at;

use crate::theme::Ctx;
use crate::AGENDA_MAX_DAYS;

mod echo;
pub use echo::*;

/// Strip terminal-control bytes from untrusted text before it is painted, so an
/// imported or agent-authored task field can't smuggle ANSI/OSC escapes that
/// clear the screen, move the cursor, set the window title, or spoof CLI output.
/// This is the terminal-path analogue of `html::esc`. Every C0 control
/// (tab included), DEL, and every C1 control are dropped; ordinary printable
/// text is untouched.
///
/// Tab is dropped here (#234 item 10 / bundle #228's duplicate — same root
/// cause), where `html::esc` deliberately keeps it (D19): a raw `\t` expands
/// to the next 8-column stop in any real terminal, shifting every column to
/// the right of it on that row — the exact misalignment D51 exists to end —
/// while an HTML `<table>` cell has no fixed-width grid for a tab to break.
/// D19's "one sanitizer standard" is about the RULE (strip control bytes,
/// keep printable text) both surfaces share, not that every exception must be
/// identical when the two surfaces' hazards differ.
///
/// For a field that is expected to hold ONE line (a title, a source, a search
/// snippet), a stray newline is itself part of what this guards against — it
/// is how a hostile field would forge a second line of fake CLI output — so it
/// is neutralised. A field that is legitimately multiple lines wants
/// [`san_multiline`] instead.
///
/// TAB and newline are replaced with a single visible space rather than
/// dropped outright: deleting them welds the text on either side into one
/// word (`"a\tb"` -> `"ab"`, `"line1\nline2"` -> `"line1line2"`), which reads
/// as a single, different value rather than the two the field actually held —
/// actively misleading, not merely cosmetic (audit #229 item 11). Every other
/// control byte (escape, bell, backspace, C1) has no legitimate content
/// reading and is still dropped outright.
pub fn san(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' || c == '\n' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

/// Same guard as [`san`], for text that is legitimately more than one line — a
/// stored memory doc's body, not a title or a snippet. `\n` survives alongside
/// `\t`; every other control byte (escape, bell, backspace, carriage return,
/// C1) is still dropped, so paragraph breaks and list items reach the screen
/// without opening the escape-injection door `san` closes (#195: `memory show`
/// ran a whole markdown doc through `san` and every newline in it vanished,
/// even though `memory show --json` and D71's "stored verbatim" promise both
/// carried them).
pub fn san_multiline(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\t' || c == '\n' || !c.is_control())
        .collect()
}

/// Is this task still open, given the `status` string as it arrived in a JSON
/// payload? The one place the CLI turns a wire status back into a [`Status`].
///
/// The fallback is the whole point. The CLI reads these strings from JSON that
/// may come from a *different build of core* over the daemon socket, so a status
/// this binary has never heard of is a real possibility — and the previous
/// `!matches!(status, "done" | "cancelled")` treated it as open by accident,
/// simply because it was not one of the two names it knew. Guessing "open" is
/// the safe guess: an unknown status shows up in the open counts and on the
/// overdue list, where a human will notice it, instead of vanishing from every
/// total and leaving a report that is quietly short a task. That behaviour is
/// preserved here deliberately rather than inherited, so a future editor changes
/// it on purpose.
pub fn status_is_open(status: &str) -> bool {
    tasqx_core::types::Status::parse(status).is_none_or(tasqx_core::types::Status::is_open)
}

/// Extract a string field, sanitized — every field pulled here is display text
/// that may originate from `store.import` or an MCP write tool.
/// True when a `task.list`/`report.summary`/`project.list` result names a
/// store that has NEVER held a task, distinguished from a filter or window
/// that simply matched nothing (#233). Missing the key (an older core, or a
/// hand-built fixture) reads as `false` — the existing dead-end copy — so a
/// build skew never invents a hint about a store it cannot vouch for.
fn store_is_empty(result: &Value) -> bool {
    result
        .get("store_empty")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The two-line getting-started hint every read verb prints in place of its
/// ordinary "nothing here" copy when [`store_is_empty`] is true (#233.1): five
/// verbs (`list`, `next`, `projects`, `report`, `agenda`) used to give a
/// brand-new store five different dead ends, none naming a way forward, on
/// literally the first command a new user runs.
/// Wrapped to the terminal with its indent kept (#346): as one line it ran to
/// 100 cells under `next`, `agenda`, `list`, `projects` and `report`. The
/// commands are quoted the way every other note quotes one, which is also
/// what keeps each of them on one line.
fn onboarding_hint(ctx: &Ctx) -> String {
    format!(
        "No tasks yet.\n{}",
        prose(
            ctx,
            None,
            "`tasqx init <project>` creates one, `tasqx add \"…\"` captures your first \
             task, `tasqx manual` explains the rest.",
            "  ",
        )
    )
}

fn s(v: &Value, key: &str) -> String {
    san(v.get(key).and_then(Value::as_str).unwrap_or(""))
}

/// One row of the `task.list` table, as plain text — measured, not yet painted.
///
/// The cells are built ONCE and the layout is computed FROM them, because a
/// column can only be sized to content that already exists. The alternative the
/// table used to run — constants in a header format string, and every cell
/// cut to fit them — sized the columns to nothing at all: `DUE` held 22 cells
/// on a store with no due dates and `TASK` held 36 on a 150-cell terminal, so
/// the widest gap in the table sat where there was no data and the titles that
/// had some were the ones truncated.
pub(crate) struct TaskRow {
    sid: String,
    urg: String,
    ramp: f64,
    prio: String,
    title: String,
    project: String,
    due: String,
    overdue: bool,
    tags: String,
    /// The role/glyph pair for the left rail (`None` on an ordinary row —
    /// see [`rail_marker`]).
    rail: Option<(&'static str, &'static str)>,
    /// The role/text pair for the STATUS marker column (`None` when the row's
    /// status is ordinary open work — see [`status_marker`]).
    marker: Option<(&'static str, String)>,
}

/// The width of every column of one table, in cells. A `0` means the column is
/// ABSENT — not empty-but-drawn — and neither its header nor its gap is emitted.
pub(crate) struct TaskCols {
    /// The left rail: `0` when no visible row has a state to show, else
    /// [`TaskCols::RAIL`] — glyph plus the one space that keeps it off a
    /// four-digit id.
    rail: usize,
    id: usize,
    /// The whole urgency cell, priority letter included: `H 17.9`.
    urg: usize,
    title: usize,
    marker: usize,
    project: usize,
    due: usize,
    tags: usize,
}

use crate::columns::{self, Column, GAP};

impl TaskCols {
    /// Floors: below these a column stops carrying information, so the table
    /// overflows the terminal rather than shrinking past them.
    const MIN_TITLE: usize = 20;
    const MIN_PROJECT: usize = 8;
    const MIN_TAGS: usize = 8;
    /// A cut date must still read as a date. [`due_cell`] spells the longest
    /// one `tomorrow 23:59`; `tomorrow` alone is the shortest prefix of that
    /// which is still an answer.
    const MIN_DUE: usize = 8;
    /// The rail's own width: one glyph and one space. Not a column that
    /// shrinks — there is nothing between one cell and none.
    const RAIL: usize = 2;
    /// The priority letter and the space between it and the urgency number.
    /// `P` used to be a column of its own, which cost a cell of gap on either
    /// side to say something about the number two columns over.
    const PRIO: usize = 2;
    /// [`urgency_meter`]'s four cells plus the space after them. Zero without
    /// Unicode, where the gauge is not drawn at all.
    const METER: usize = 5;
    /// `cancelled` is the longest word this column ever holds; a cut STATUS
    /// still has to stay a word (or the header `STATUS` itself), not a single
    /// ambiguous letter.
    const MIN_MARKER: usize = 6;
    /// Ceilings. A column wider than this stops earning its cells: the eye
    /// loses the row across a 90-cell title, and the tail of a tag list or a
    /// project name identifies far less than its head. Overflow goes to the
    /// ellipsis, not to the column — and the cells stay available to the
    /// neighbours that can still use them.
    const MAX_TITLE: usize = 72;
    const MAX_PROJECT: usize = 24;
    const MAX_TAGS: usize = 28;
    /// `cancelled` (9) plus a little room; a marker never needs more than a
    /// status word or one glyph.
    const MAX_MARKER: usize = 11;

    /// Everything left of `TASK`, as one column the fitter never touches: the
    /// rail is glued to the id with no gap, and the urgency cell is a
    /// composite whose parts are sized by their own constants.
    fn head(&self) -> usize {
        self.rail + self.id + GAP + self.urg
    }

    /// Size the columns to the rows, then to the terminal.
    ///
    /// A column every row leaves empty is DROPPED. That is the visible half of
    /// this function: a store with no due dates was spending 24 cells on a
    /// `DUE` column that could never hold anything, and the reader saw it as the
    /// table falling apart between `PROJECT` and `TAGS`.
    ///
    /// `when_label` is the header the date column will be printed under — `DUE`
    /// for [`task_table`], `WHEN` for [`agenda_text`], whose column holds
    /// whichever of `due`/`scheduled` put the row on the calendar. It is a
    /// PARAMETER rather than a constant because a column is sized to its own
    /// header as well as to its content (the `sized` closure below), so a label
    /// chosen by the caller and a width computed from a different one is a
    /// header that can overhang its column by exactly the difference — the
    /// misalignment this whole function exists to end, rebuilt one caller over.
    pub(crate) fn fit(
        rows: &[TaskRow],
        budget: usize,
        when_label: &str,
        unicode: bool,
    ) -> TaskCols {
        let meter_w = if unicode { Self::METER } else { 0 };
        let max_of = |f: fn(&TaskRow) -> &str| rows.iter().map(|r| width(f(r))).max().unwrap_or(0);
        // A column is as wide as its widest cell OR its own header, whichever
        // asks for more — a label that does not fit its column is the same
        // misalignment one row up.
        let sized = |content: usize, label: &str| {
            if content == 0 {
                0
            } else {
                content.max(width(label))
            }
        };

        // The marker column's content isn't measured through `f: fn(&TaskRow)
        // -> &str` like the others (it lives behind an `Option`), so it gets
        // its own max rather than going through `max_of`.
        let marker_content = rows
            .iter()
            .filter_map(|r| r.marker.as_ref())
            .map(|(_, text)| width(text))
            .max()
            .unwrap_or(0);

        let mut c = TaskCols {
            // Droppable like `DUE` and for the same reason (D51): a store with
            // nothing blocked and no timer running would otherwise indent
            // every row by two cells to hold a column that can never say
            // anything.
            rail: if rows.iter().any(|r| r.rail.is_some()) {
                Self::RAIL
            } else {
                0
            },
            // The id column keeps a floor of 4 rather than sizing to its digits:
            // ids grow monotonically, and a table that shifted left by a cell
            // the day the store passed #999 would look like the bug this
            // function fixes.
            id: max_of(|r| &r.sid).max(4),
            // `r.urg` is the number alone; the cell it is measured for also
            // holds the priority letter and the gauge in front of it.
            urg: (max_of(|r| &r.urg) + Self::PRIO + meter_w).max(width("URG")),
            title: sized(max_of(|r| &r.title), "TASK").max(width("TASK")),
            marker: sized(marker_content, "STATUS"),
            project: sized(max_of(|r| &r.project), "PROJECT"),
            due: sized(max_of(|r| &r.due), when_label),
            tags: sized(max_of(|r| &r.tags), "TAGS"),
        };
        c.title = c.title.min(Self::MAX_TITLE);
        c.marker = c.marker.min(Self::MAX_MARKER);
        c.project = c.project.min(Self::MAX_PROJECT);
        c.tags = c.tags.min(Self::MAX_TAGS);

        // The shrink and drop passes are `columns::fit`'s, which is where they
        // live for every table (#352). They were found HERE, on a real store:
        // "shrink the least important column first" cut PROJECT and TAGS to
        // their floors while a 68-cell TASK column sat untouched, so cells come
        // off whichever column is widest, and ties go right, so TASK, the
        // leftmost of these, is cut last. Among the rest, a tie cuts STATUS,
        // then TAGS, then PROJECT, and DUE last: a date cut to `due 2026-0…`
        // says nothing, while a project or tag cut by the same cell still
        // identifies itself. At every floor and still over,
        // columns drop from the right: TAGS, DUE, PROJECT, then STATUS. STATUS
        // goes last because on a store where some row is not open work, that
        // fact is the reason to open the table at all. The rail is never
        // dropped. It costs two cells, it carries the two facts a reader most
        // needs off a narrow terminal (what is running, what is stuck), and
        // there is no smaller version of it to fall back to.
        let w = columns::fit(
            &[
                Column::fixed(c.head()),
                Column::shrinks(c.title, Self::MIN_TITLE),
                Column::drops(c.marker, Self::MIN_MARKER).tie_rank(4),
                Column::drops(c.project, Self::MIN_PROJECT).tie_rank(2),
                Column::drops(c.due, Self::MIN_DUE).tie_rank(1),
                Column::drops(c.tags, Self::MIN_TAGS).tie_rank(3),
            ],
            budget,
        );
        (c.title, c.marker, c.project, c.due, c.tags) = (w[1], w[2], w[3], w[4], w[5]);
        c
    }
}

/// Right-align `s` in `w` CELLS. The `{:>w$}` this replaces pads by char count.
fn rpad(s: &str, w: usize) -> String {
    format!("{}{}", " ".repeat(w.saturating_sub(width(s))), s)
}

/// Fit `text` into `w` cells, paint it in `role`, and pad the RESULT — so the
/// spaces sit OUTSIDE the escape sequence. Padding inside it is padding a
/// `trim_end` cannot reach, which is how a table grows invisible trailing cells.
///
/// `role: None` is a deliberately unpainted cell (the title, an ordinary due
/// date): the alternative is inventing a role name no theme file defines, which
/// would read as themed and paint nothing.
pub(crate) fn cell(ctx: &Ctx, role: Option<&str>, text: &str, w: usize) -> String {
    let t = truncate(text, w, ctx.caps.unicode);
    let padding = " ".repeat(w.saturating_sub(width(&t)));
    match role {
        Some(r) => format!("{}{padding}", ctx.paint(r, &t)),
        None => format!("{t}{padding}"),
    }
}

/// Cells between two columns, applied by the ONE joiner both the header and
/// every row go through — so a dropped column cannot survive in one of them and
/// not the other.
pub(crate) fn join_cells(cells: Vec<String>) -> String {
    cells.join(&" ".repeat(GAP)).trim_end().to_string()
}

/// The header line for a fitted table. `when_label` must be the same string the
/// widths were fitted with — see [`TaskCols::fit`].
pub(crate) fn header_line(c: &TaskCols, when_label: &str) -> String {
    // The rail has no label — a two-cell column cannot hold one, and the two
    // glyphs it draws are the kind a reader learns once. It is prefixed to the
    // id cell rather than joined as a column of its own, so `join_cells` does
    // not put a GAP between the glyph and the number it belongs to.
    let mut head = vec![
        format!("{}{}", " ".repeat(c.rail), rpad("ID", c.id)),
        rpad("URG", c.urg),
        pad("TASK", c.title),
    ];
    for (w, label) in [
        (c.marker, "STATUS"),
        (c.project, "PROJECT"),
        (c.due, when_label),
        (c.tags, "TAGS"),
    ] {
        if w > 0 {
            head.push(pad(label, w));
        }
    }
    join_cells(head)
}

/// One painted row of a fitted table.
fn row_line(ctx: &Ctx, c: &TaskCols, r: &TaskRow) -> String {
    row_line_at(ctx, c, r, false)
}

/// [`row_line`], for a screen with a cursor (`pick`, D124): the row the reader
/// is on carries its id and title in `accent`, the title bold as well, so the
/// row survives `NO_COLOR` and `mono` with more than the cursor glyph to mark
/// it. The memory browser marks its cursor row the same way. `list` never
/// passes `true`, so its bytes do not move.
pub(crate) fn row_line_at(ctx: &Ctx, c: &TaskCols, r: &TaskRow, on_cursor: bool) -> String {
    let prio_role = match r.prio.as_str() {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    // The number is padded to whatever the cell has left once the priority
    // letter, the gauge and their spaces are taken out, so `H ▄▄▄▄ 17.9` and
    // `L ▁▁▁▁  1.8` end on the same cell and the column reads as one number.
    let meter_w = if ctx.caps.unicode { TaskCols::METER } else { 0 };
    let gauge = if ctx.caps.unicode {
        let (bar, track) = urgency_meter(r.ramp);
        format!(
            "{}{} ",
            ctx.theme.ramp_style(r.ramp).paint(&bar, &ctx.caps),
            ctx.paint("muted", &track)
        )
    } else {
        String::new()
    };
    let urg_plain = rpad(&r.urg, c.urg.saturating_sub(TaskCols::PRIO + meter_w));
    let urg = format!(
        "{} {gauge}{}",
        cell(ctx, Some(prio_role), &r.prio, 1),
        ctx.theme.ramp_style(r.ramp).paint(&urg_plain, &ctx.caps)
    );
    let rail = match (c.rail, r.rail) {
        (0, _) => String::new(),
        (w, Some((role, glyph))) => cell(ctx, Some(role), glyph, w),
        (w, None) => " ".repeat(w),
    };
    let (sid, title) = if on_cursor {
        let at = ctx.theme.role("accent");
        let t = truncate(&r.title, c.title, ctx.caps.unicode);
        (
            at.paint(&rpad(&r.sid, c.id), &ctx.caps),
            format!(
                "{}{}",
                at.bold().paint(&t, &ctx.caps),
                " ".repeat(c.title.saturating_sub(width(&t)))
            ),
        )
    } else {
        (rpad(&r.sid, c.id), cell(ctx, None, &r.title, c.title))
    };
    let mut line = vec![format!("{rail}{sid}"), urg, title];
    if c.marker > 0 {
        let (role, text) = r
            .marker
            .as_ref()
            .map_or((None, ""), |(role, text)| (Some(*role), text.as_str()));
        line.push(cell(ctx, role, text, c.marker));
    }
    if c.project > 0 {
        line.push(cell(ctx, Some("project"), &r.project, c.project));
    }
    if c.due > 0 {
        // Painted or bare, the cell went through the SAME fit, so the
        // overdue branch cannot drift out of width from the ordinary one.
        let role = if r.overdue { Some("overdue") } else { None };
        line.push(cell(ctx, role, &r.due, c.due));
    }
    if c.tags > 0 {
        line.push(cell(ctx, Some("tag"), &r.tags, c.tags));
    }
    join_cells(line)
}

/// The LEFT-RAIL glyph for one row: the two states that stop the reader or
/// occupy them, put in the one column the eye crosses before any other.
///
/// Split out of [`status_marker`], which used to carry these two alongside the
/// word statuses in a `STATUS` column drawn to the RIGHT of the title. Audit
/// finding #147 is what that cost: the running task — the single piece of
/// state a work block depends on — sat past a 72-cell title where nothing
/// draws the eye, and on a narrow terminal [`TaskCols::fit`] would drop the
/// column holding it outright. A rail cannot be dropped and cannot be scrolled
/// past.
///
/// Blocked outranks `active`, the same priority [`rail_role`] gives the `show`
/// card's rail (D78): it is the fact that stops the reader working, and a task
/// can be blocked while its own timer runs.
///
/// The two glyphs differ from each other in SHAPE, not only in role. `NO_COLOR`
/// (§8's degradation table) keeps emphasis and drops every hue, so a rail that
/// said "red bar or green bar" would say nothing at all to the reader who most
/// needs the terminal to behave.
fn rail_marker(t: &Value, unicode: bool) -> Option<(&'static str, &'static str)> {
    if t.get("blocked").and_then(Value::as_bool).unwrap_or(false) {
        return Some(("danger", if unicode { "⊘" } else { "B" }));
    }
    if s(t, "status") == "active" {
        // `*`, not `>`. Without Unicode `>` is the CURSOR — `pick` and the
        // dashboard both reserve it for the row the reader is on — and a state
        // marker that draws as the cursor is two meanings on one glyph, on the
        // exact terminal that has no colour left to tell them apart.
        return Some(("timer.active", if unicode { "▶" } else { "*" }));
    }
    None
}

/// The STATUS marker for one row of the shared list/agenda table: the statuses
/// D51's fixed column set never gave a cell to — `done`, `cancelled`,
/// `backlog`, or text this build could not parse.
///
/// `tasqx list project:x` and `tasqx agenda` both send whatever the caller's
/// filter matched — literally, D27/D28's contract for a read — so a closed or
/// unrecognized-status row reaches this renderer indistinguishable from open
/// work unless it carries its own cell. What it holds is a WORD, which is why
/// `blocked` and `active` left for [`rail_marker`]: a column sized to
/// `cancelled` is the wrong shape for a one-cell state glyph, and the two
/// facts are read at different moments. `None` is the ordinary row and is what
/// makes the column droppable exactly like an empty `DUE` (D51):
/// [`TaskCols::fit`] sizes it to zero when every row answers `None`.
fn status_marker(t: &Value) -> Option<(&'static str, String)> {
    if status_is_unrecognized(t) {
        // `status` already carries the raw text here (`Task::status_text`),
        // so there is nothing to look up — just show what the store holds.
        return Some(("warn", s(t, "status")));
    }
    let status = s(t, "status");
    if !status_is_open(&status) {
        return Some(("muted", status));
    }
    None
}

/// The urgency gauge: `r.ramp` — this row's place on [`urgency_scale`] — as a
/// bar on a track. Returns the two halves separately, because they are
/// painted differently: the bar in the theme's ramp color, the track in
/// `muted`.
///
/// Four cells, three steps inside each. A whole-cell bar was mocked first and
/// could not tell 17.9 from 15.8 — four filled cells either way — which is
/// exactly the pair the reader is ranking. The remainder is drawn as a SHORTER
/// glyph (`▂`, `▃`) rather than a taller one: height above the bar's own `▄`
/// would make a nearly-empty gauge the loudest mark in the column, which is the
/// ranking inverted. So visual mass rises monotonically with urgency, which is
/// the only thing this mark is for.
///
/// The number beside it always prints and is the precise answer; the bar is for
/// the scan down the column, not for reading a value off. Without Unicode there
/// is no glyph set that degrades honestly here, so the gauge is not drawn at
/// all — the caller checks `caps.unicode` and the cell falls back to the number.
pub(crate) fn urgency_meter(ramp: f64) -> (String, String) {
    /// The remainder glyphs, all SHORTER than the `▄` a full cell draws.
    const PART: [char; 3] = ['▂', '▃', '▄'];
    const CELLS: usize = 4;
    // Floored, not rounded (D119). A full gauge claims "as urgent as an overdue
    // task", and rounding drew 11.5 full while the ramp, which bands by
    // flooring, still painted it `warn`. The bar and its colour then disagreed
    // about the one threshold both exist to show.
    let steps = (ramp.clamp(0.0, 1.0) * (CELLS * PART.len()) as f64).floor() as usize;
    let full = (steps / PART.len()).min(CELLS);
    let rest = steps % PART.len();

    let mut bar = "▄".repeat(full);
    if full < CELLS && rest > 0 {
        bar.push(PART[rest - 1]);
    }
    // `▁` rather than a space: an empty gauge reads as a track waiting to fill,
    // and a gap in the middle of a column of bars reads as a rendering fault.
    let track = "▁".repeat(CELLS - bar.chars().count());
    (bar, track)
}

/// `Sep`, `Jan` — the month as a `DUE` cell abbreviates it. Only ever fed
/// `Date::month`, whose range is 1..=12; anything else would be a jiff bug, and
/// falling back to the number keeps a date on screen either way.
fn month_abbrev(m: i8) -> String {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    NAMES
        .get((m - 1).max(0) as usize)
        .map_or_else(|| m.to_string(), ToString::to_string)
}

/// The `DUE` cell: the deadline as a reader dates it, not as the store spells
/// it.
///
/// [`task_row`] used to hand `s(t, "due")` — an RFC-3339 instant — straight to
/// the cell, so a dated row spent twenty cells on `2026-09-11T00:00:00Z` while
/// `TASK`, the column the row is actually read for, truncated at eighteen
/// characters on an 80-cell terminal. The `show` card has humanized the same
/// field since D78 (`markdown::fmt_instant`, "in 4 hours"), so the table was
/// the view disagreeing with the rest of the CLI rather than the one missing a
/// capability.
///
/// CALENDAR days, not elapsed hours, and that is the difference from
/// `fmt_instant`: a deadline at 09:00 tomorrow is "tomorrow" to the person
/// reading it, and "in 14 hours" hands them the arithmetic this cell exists to
/// do. A date is spelled by [`calendar_date`], which [`day_heading`] shares, so
/// `list` and `agenda` name the same day the same way (D133).
///
/// Inside the next week the weekday alone identifies the day — there is
/// exactly one Sunday in any six-day window — so the date is not spelled out
/// until it stops being unambiguous. The clock is printed for today and
/// tomorrow only, the two days on which "when today" is still a live question,
/// and only when the store holds one: a date typed without a time resolves to
/// 00:00 UTC (`datetime.rs`), so midnight is precisely the store's spelling of
/// "no time given", which is how [`when_cell`] already reads it.
pub(crate) fn due_cell(due: Timestamp, now: Timestamp) -> String {
    let z = due.to_zoned(TimeZone::UTC);
    let day = z.date();
    let today = now.to_zoned(TimeZone::UTC).date();
    let days = day
        .since((Unit::Day, today))
        .map_or(0, |s| s.get_days())
        .into();

    let t = z.time();
    let clock = if t.hour() == 0 && t.minute() == 0 {
        String::new()
    } else {
        format!(" {:02}:{:02}", t.hour(), t.minute())
    };

    match days {
        0i64 => format!("today{clock}"),
        1 => format!("tomorrow{clock}"),
        -1 => "yesterday".to_string(),
        d if d < -1 => format!("{}d ago", -d),
        d if d < 7 => weekday_abbrev(day).to_string(),
        _ => calendar_date(day, today),
    }
}

/// `13 Sep`, `4 Jan 27`: the one spelling of a calendar date on the terminal
/// screens, with the two-digit year only once the date leaves the current one.
///
/// ONE function because two spellings drifted: `list` said `2d ago` and
/// `22 Sep` while `agenda`, over the same rows, printed `due 2026-09-11`,
/// `Wed 2026-09-16` and `through 2026-09-27`. [`due_cell`], [`day_ago`],
/// [`day_heading`], the agenda horizon and its overdue footer all call this,
/// so the views cannot come to name one day two ways again (D133). The
/// `--json` objects stay ISO: those are a machine contract, not a screen.
pub(crate) fn calendar_date(day: Date, today: Date) -> String {
    if day.year() == today.year() {
        format!("{} {}", day.day(), month_abbrev(day.month()))
    } else {
        format!(
            "{} {} {:02}",
            day.day(),
            month_abbrev(day.month()),
            day.year().rem_euclid(100)
        )
    }
}

/// When something last happened, as a calendar day: `today`, `yesterday`,
/// `3d ago`, `17 Aug`, `4 Jan 25`. The past-facing half of [`due_cell`]'s
/// vocabulary, for a timestamp that records an event rather than a deadline.
/// It never prints a clock: "updated 14:02" answers a question nobody browsing
/// a list of notes is asking.
pub(crate) fn day_ago(at: Timestamp, now: Timestamp) -> String {
    let day = at.to_zoned(TimeZone::UTC).date();
    let today = now.to_zoned(TimeZone::UTC).date();
    let days: i64 = today
        .since((Unit::Day, day))
        .map_or(0, |s| s.get_days())
        .into();
    match days {
        d if d <= 0 => "today".to_string(),
        1 => "yesterday".to_string(),
        d if d < 7 => format!("{d}d ago"),
        _ => calendar_date(day, today),
    }
}

/// One line that says what a memory doc is about, out of the start of its body.
///
/// The frontmatter's `description`, when the doc has one: imported agent
/// memories open with a `---` block, and printing it verbatim is how
/// `name: vh-mcp-standaard description: "…` came to be every preview's first
/// words. Otherwise the first line of prose, without its markdown markers.
///
/// The fence itself is found by `tasqx_core::frontmatter::split` (task
/// #12/D135) — the same parser `tui::memory::doc_lines` and
/// `verbs::memory_docs_from_path`'s import cut both call — rather than a
/// second hand-rolled `---` scan reading the same bytes its own way.
pub(crate) fn doc_summary(body: &str) -> String {
    let (pairs, rest) = tasqx_core::frontmatter::split(body);
    if let Some(d) = pairs
        .iter()
        .find_map(|(k, v)| (*k == Some("description")).then_some(*v))
        .filter(|d| !d.is_empty())
    {
        return san(d);
    }
    rest.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '-', '*', '>', ' '])
                .trim()
        })
        .find(|l| !l.is_empty() && !l.starts_with("```") && *l != "---")
        .map(|l| san(&l.replace("**", "").replace('`', "")))
        .unwrap_or_default()
}

/// The floor of `memory list`'s title beside the id, which never goes
/// (D125(b)): as much of what the title asks for as the terminal can give it
/// beside the id, so the other columns go before the title gives way (rule 1,
/// D120(c)), and never below [`MIN_TITLE_CELLS`], where a title stops telling
/// one doc from another. Past that the row overflows, as every table's does
/// (D120). The first version had no lower bound and printed `...` for every
/// title at 40 columns. `memory search` does not need it: each of its records
/// fits its own head line ([`memory_hits`]).
fn lead_floor(asked: usize, cols: usize, fixed: usize) -> usize {
    asked.min(
        cols.saturating_sub(fixed + columns::GAP)
            .max(MIN_TITLE_CELLS),
    )
}

/// Twelve cells: `android-t...`, still a name. The floor `memory list` shipped
/// with (D121), kept as the lower bound of [`lead_floor`].
const MIN_TITLE_CELLS: usize = 12;

/// `tasqx memory list` off a terminal: one line per doc under a header (D121).
///
/// It used to print three lines per doc, the title with its source in
/// parentheses, a snippet with frontmatter leaking into it, and a whole line
/// for the id, and closed on `N doc(s) of M`. Every row weighed the same and
/// a grep for a title found a line without the id `memory show` needs. Now
/// the title carries the row, the id sits on it (dim, last, never dropped:
/// it is the handle), and the summary says what the reader cannot count.
pub fn memory_table(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let empty = Vec::new();
    let docs = result
        .get("docs")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let total = result.get("total").and_then(Value::as_u64).unwrap_or(0);
    if docs.is_empty() {
        return if total == 0 {
            "No memory docs yet. `tasqx memory add <title> <body>` stores one.\n".to_string()
        } else {
            format!("No docs on this page; the store holds {total}.\n")
        };
    }
    struct Row {
        title: String,
        project: String,
        updated: String,
        id: String,
    }
    let rows: Vec<Row> = docs
        .iter()
        .map(|d| Row {
            title: s(d, "title"),
            project: s(d, "project"),
            updated: field_ts(d, "modified").map_or_else(String::new, |t| day_ago(t, now)),
            id: s(d, "id"),
        })
        .collect();
    let widest = |f: fn(&Row) -> &str, label: &str| {
        let content = rows.iter().map(|r| width(f(r))).max().unwrap_or(0);
        if content == 0 {
            0
        } else {
            content.max(width(label))
        }
    };
    let (title_w, id_w) = (widest(|r| &r.title, "TITLE"), widest(|r| &r.id, "ID"));
    let w = columns::fit(
        &[
            Column::shrinks(title_w, lead_floor(title_w, ctx.cols, id_w)),
            Column::drops(widest(|r| &r.project, "PROJECT"), 8),
            Column::drops(widest(|r| &r.updated, "UPDATED"), 7),
            Column::fixed(id_w),
        ],
        ctx.cols,
    );
    let line = |cells: [(Option<&str>, &str); 4]| {
        join_cells(
            cells
                .iter()
                .zip(&w)
                .filter(|(_, w)| **w > 0)
                .map(|((role, text), w)| cell(ctx, *role, text, *w))
                .collect(),
        )
    };

    let shown = docs.len() as u64;
    let mut parts = vec![(
        "card.strong",
        format!("{total} {}", if total == 1 { "doc" } else { "docs" }),
    )];
    if shown < total {
        parts.push(("muted", format!("{shown} shown")));
    }
    let mut out = format!("{}\n\n", summary_line(ctx, None, parts));
    out.push_str(&ctx.paint(
        "table.label",
        &line([
            (None, "TITLE"),
            (None, "PROJECT"),
            (None, "UPDATED"),
            (None, "ID"),
        ]),
    ));
    out.push('\n');
    for r in &rows {
        out.push_str(&line([
            (None, &r.title),
            (Some("project"), &r.project),
            (Some("muted"), &r.updated),
            (Some("muted"), &r.id),
        ]));
        out.push('\n');
    }
    out
}

/// `tasqx memory search`: one record per hit (D125(a)).
///
/// The head line is the title, where it came from, and the handle that opens
/// it, fitted like a table row: the source goes first, the title gives way
/// last ([`lead_floor`]), and the handle never goes. Under it are the words
/// that matched, cut to the terminal, because they are the reason the hit is
/// on the screen. The handle is a doc's id, which `memory show` takes, or
/// `annotation on #N` for an annotation, whose own id `memory show` refuses
/// and whose task `tasqx show N` opens.
///
/// It printed three lines per hit (the title with `(doc · source)`, the
/// snippet, and `id <uuid>` on a line of its own), every one at the same
/// weight, and closed on `N hit(s)`. #346's first cut made it a table, and at
/// 60 and 80 columns the 36-cell id left room for the title alone.
pub fn memory_hits(ctx: &Ctx, result: &Value, query: &str, raw: bool) -> String {
    let empty = Vec::new();
    let hits = result
        .get("hits")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let count = hits.len() as u64;
    let total = result
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or(count)
        .max(count);

    let mut parts = vec![(
        "card.strong",
        format!("{total} {}", if total == 1 { "hit" } else { "hits" }),
    )];
    if count < total {
        parts.push(("muted", format!("{count} shown")));
    }
    // The expression that ran (D69), not the words as typed: a plain query
    // comes back with each term quoted, which sets it off from the count
    // where dim does not reach the screen, and it is what a miss has to name.
    let asked = result.get("matched").and_then(Value::as_str).map_or_else(
        || san(query),
        |m| {
            if raw {
                format!("\"{}\"", san(m))
            } else {
                san(m)
            }
        },
    );
    let mut out = summary_line(ctx, Some(&asked), parts);
    out.push('\n');

    if !hits.is_empty() {
        struct Hit {
            title: String,
            source: String,
            handle: String,
            snippet: String,
        }
        let rows: Vec<Hit> = hits
            .iter()
            .map(|h| {
                let source = s(h, "source");
                let task = source
                    .strip_prefix("task:#")
                    .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
                let (source, handle) = match (s(h, "kind").as_str(), task) {
                    // The source already names the task, and the handle says
                    // it, so the source column stays empty (rule 11).
                    ("annotation", Some(n)) => (String::new(), format!("annotation on #{n}")),
                    _ => (source, s(h, "id")),
                };
                Hit {
                    title: s(h, "title"),
                    source,
                    handle,
                    // The engine's snippet keeps the body's line breaks,
                    // which a one-line cell cannot.
                    snippet: s(h, "snippet")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                }
            })
            .collect();
        out.push('\n');
        for r in &rows {
            // Each record's head line is fitted to its OWN handle (round 2
            // review): fitted as one table, a 36-cell doc id set the title
            // width for an annotation whose handle is 17 cells, and cut its
            // title beside 19 empty ones. The title is data and is not cut
            // to make room for the handle: where the two cannot share a line,
            // the handle takes the next one, and the title is cut only where
            // it is wider than the terminal itself. A path cut in the middle
            // names no file, so SOURCE is whole or gone, and it goes first.
            let (title_w, handle_w) = (width(&r.title), width(&r.handle));
            if title_w + columns::GAP + handle_w <= ctx.cols {
                let source_w = width(&r.source);
                let w = columns::fit(
                    &[
                        Column::fixed(title_w),
                        Column::drops(source_w, source_w),
                        Column::fixed(handle_w),
                    ],
                    ctx.cols,
                );
                out.push_str(&join_cells(
                    [
                        (None, r.title.as_str()),
                        (Some("muted"), r.source.as_str()),
                        (Some("muted"), r.handle.as_str()),
                    ]
                    .iter()
                    .zip(&w)
                    .filter(|(_, w)| **w > 0)
                    .map(|((role, text), w)| cell(ctx, *role, text, *w))
                    .collect(),
                ));
                out.push('\n');
            } else {
                out.push_str(&truncate(&r.title, ctx.cols, ctx.caps.unicode));
                out.push('\n');
                out.push_str(&format!("  {}\n", ctx.paint("muted", &r.handle)));
            }
            if !r.snippet.is_empty() {
                // Four cells, under the handle's two: `muted` draws nothing
                // under NO_COLOR, so indent is what ranks these lines there.
                out.push_str(&format!(
                    "    {}\n",
                    ctx.paint(
                        "muted",
                        &truncate(&r.snippet, ctx.cols.saturating_sub(4), ctx.caps.unicode)
                    )
                ));
            }
        }
    }

    // Prose after the records stands off them by a blank line (rule 7), and
    // wraps rather than running past the terminal.
    let mut notes: Vec<(Option<&str>, String)> = Vec::new();
    if count < total {
        // At the terminal's own weight, like the miss hint below: both name
        // the command that shows what this screen could not.
        notes.push((None, format!("--limit {total} shows every hit")));
    }
    // On a miss, name the expression that ran and why it came back empty
    // (D69). The summary's label is cut to half the width, so a twelve-term
    // question showed only its first terms there; this note carries the
    // expression WHOLE, wrapped by `prose` rather than cut, because "use
    // fewer" cannot be acted on by a reader who cannot see which terms there
    // were. That is why it is not a repeat of the summary (rule 11): it is
    // the only complete copy. At the terminal's own weight, because it is the
    // only line that says what to do next. The advice fits the search that
    // ran: every word of a plain query is a required phrase, so dropping
    // terms widens it, while a raw expression is already the caller's own and
    // is widened with OR.
    if count == 0 {
        if let Some(matched) = result.get("matched").and_then(Value::as_str) {
            let expr = san(matched);
            notes.push((
                None,
                if raw {
                    // Quoted like the summary's label: the engine quotes a
                    // plain query's terms for us, a raw expression it does not.
                    format!("nothing matched \"{expr}\" — OR widens it")
                } else {
                    format!("every term was required: {expr} — use fewer, or --raw with OR")
                },
            ));
        }
    }
    if !notes.is_empty() {
        out.push('\n');
        for (role, note) in notes {
            out.push_str(&prose(ctx, role, &note, ""));
        }
    }
    out
}

/// A note printed under a screen, wrapped at words to the terminal and
/// painted in `role`, each line prefixed with `indent` (#346).
///
/// The prose under `agenda`, `next`, `report` and `memory search` used to be
/// printed at whatever length it was written, and the longest of them, the
/// onboarding hint, ran to 100 cells: a 60-column terminal broke it mid-word.
/// Every such note goes through here, so none of them can come to wrap on its
/// own again.
///
/// A command quoted in backticks is one word to the wrap: split across two
/// lines it is a command nobody can paste, and these notes exist to hand the
/// reader one.
pub(crate) fn prose(ctx: &Ctx, role: Option<&str>, text: &str, indent: &str) -> String {
    /// Stands in for a space inside a quoted span while the line is wrapped. A
    /// private-use character, so `split_whitespace` cannot see it and no note
    /// can contain it.
    const HELD: char = '\u{E000}';
    // Only PAIRED backticks open and close a span. A stray one (raw store
    // text, a query as typed) would otherwise hold the rest of the note as
    // one word, and the note would run past the terminal it exists to fit.
    let mut toggles = text.matches('`').count() / 2 * 2;
    let mut quoted = false;
    let held: String = text
        .chars()
        .map(|c| {
            if c == '`' && toggles > 0 {
                toggles -= 1;
                quoted = !quoted;
            }
            if quoted && c == ' ' {
                HELD
            } else {
                c
            }
        })
        .collect();
    let mut out = String::new();
    for line in wrap_words(&held, ctx.cols.saturating_sub(width(indent))) {
        let line = line.replace(HELD, " ");
        out.push_str(indent);
        match role {
            Some(r) => out.push_str(&ctx.paint(r, &line)),
            None => out.push_str(&line),
        }
        out.push('\n');
    }
    out
}

/// Measure one `task.list` row into the cells the layout will be computed from.
///
/// Shared with [`agenda_text`], which then overwrites `due`/`overdue` with what
/// its own `WHEN` column holds. Everything else — the urgency ramp, the
/// sanitizing, the `-` for an unset priority — is identical by construction
/// rather than by two functions agreeing, which is how the two views cannot come
/// to disagree about the same task.
pub(crate) fn task_row(t: &Value, now: Timestamp, unicode: bool) -> TaskRow {
    let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    TaskRow {
        sid: format!("{}", t.get("short_id").and_then(Value::as_i64).unwrap_or(0)),
        urg: format!("{urg:.1}"),
        ramp: urgency_scale(urg),
        prio: t
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string(),
        title: s(t, "title"),
        project: s(t, "project"),
        // Unparseable falls back to itself, the policy `field_ts` already
        // states and `markdown::fmt_instant` already follows: a stamp this
        // build cannot read is still a stamp the reader may recognize, and
        // printing nothing would hide a field the store plainly holds.
        due: match field_ts(t, "due") {
            Some(d) => due_cell(d, now),
            None => s(t, "due"),
        },
        overdue: field_ts(t, "due").is_some_and(|d| overdue_at(d, now))
            && status_is_open(&s(t, "status")),
        // #228.16: rendered bare (`cardtag`), the one filter spelling that
        // does not parse (`tasqx list cardtag` -> "unknown filter token") is
        // exactly the text this table just printed. `+tag` is what `modify`'s
        // echo already renders and the only spelling `list`'s own filter
        // grammar accepts, so the table matches it rather than adding a third
        // spelling of the same tag.
        tags: t
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                san(&a
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|t| format!("+{t}"))
                    .collect::<Vec<_>>()
                    .join(" "))
            })
            .unwrap_or_default(),
        rail: rail_marker(t, unicode),
        marker: status_marker(t),
    }
}

/// Where an urgency sits on the gauge and the ramp: 0 is nothing pressing, 1
/// is as urgent as an overdue task (`urgency::DUE_WEIGHT`), and anything
/// above that is clipped to 1 (D119).
///
/// Absolute, so it is a property of the task and not of the rows beside it.
/// The denominator used to be the hottest row on screen. On a real store one
/// overdue task at 18.5 drew every H row at a third of a gauge and in the
/// ramp's cool colour, and any filtered view gave its top row a full bar in
/// `danger`, done tasks included. `list`, `agenda` and the dashboard's TASKS
/// all read this one function, so the same task is the same mark on all three.
///
/// The price, taken knowingly: rows at or past the overdue landmark all draw a
/// full bar, and the figure beside it tells them apart.
pub(crate) fn urgency_scale(urgency: f64) -> f64 {
    (urgency / tasqx_core::urgency::DUE_WEIGHT).clamp(0.0, 1.0)
}

/// One RFC3339 instant field of a task, or `None` when it is absent, null, or
/// text this build cannot parse. Unparseable is treated as absent deliberately:
/// the CLI may be talking to a different build of core over the socket, and a
/// stamp it cannot read is a stamp it cannot place on a day either.
fn field_ts(t: &Value, key: &str) -> Option<Timestamp> {
    t.get(key)
        .and_then(Value::as_str)
        .and_then(|v| v.parse::<Timestamp>().ok())
}

/// `1 task` / `9 tasks`. The `N task(s)` this replaces was the reader being
/// handed a `match` the writer would not make.
fn plural_tasks(n: i64) -> String {
    if n == 1 {
        "1 task".to_string()
    } else {
        format!("{n} tasks")
    }
}

/// The line a table opens with: what was asked for, and what the answer holds
/// beyond its own size.
///
/// It replaces the `N task(s)` trailer, which was the least useful summary
/// available — a reader who has the rows in front of them can count them, and
/// what they cannot see at a glance is how much of the list is late, due
/// before the day is out, running, or stuck behind something else. Those four
/// are counted over the rows ON SCREEN, which is why a bounded result
/// (`serve::bound_to_viewport`, `--limit`) names both numbers: `count` is the
/// store's answer and stays true, and a fact counted over twenty rows must not
/// be read as a claim about forty-four.
///
/// Only non-zero facts are printed. A line that says `0 overdue · 0 blocked`
/// trains the reader to skip it, and then it is not there on the day it says
/// something.
pub(crate) fn table_summary(
    ctx: &Ctx,
    tasks: &[&Value],
    rows: &[TaskRow],
    count: i64,
    label: Option<&str>,
    now: Timestamp,
    day_grouped: bool,
) -> String {
    // Collected as (role, plain text) and painted at the END: the line has to
    // be MEASURED before it is emitted, and an SGR escape is not a cell.
    let mut parts: Vec<(&str, String)> = vec![("card.strong", plural_tasks(count))];
    if count > rows.len() as i64 {
        parts.push(("muted", format!("{} shown", rows.len())));
    }

    let overdue = rows.iter().filter(|r| r.overdue).count();
    if overdue > 0 {
        parts.push(("overdue", format!("{overdue} overdue")));
    }

    // "Due today" is the rest of THIS day, so a row already past its deadline
    // is late rather than upcoming and is counted once, above. `due_cell`
    // spells both of them `today …`, which is the point: the cell says which
    // day, and this line says which side of now.
    let today = now.to_zoned(TimeZone::UTC).date();
    let due_today = tasks
        .iter()
        .filter(|t| {
            field_ts(t, "due").is_some_and(|d| {
                !overdue_at(d, now)
                    && d.to_zoned(TimeZone::UTC).date() == today
                    && status_is_open(&s(t, "status"))
            })
        })
        .count();
    // An agenda prints its own `Today · Thu 10 Sep` heading with the rows
    // under it, so the count is already on the screen in a form that also says
    // WHICH rows. Printing it again beside the heading that made it redundant
    // is the kind of line a reader learns to skip.
    if due_today > 0 && !day_grouped {
        parts.push(("warn", format!("{due_today} due today")));
    }

    // Named by id, not counted. There is normally one timer, and "1 running"
    // makes the reader run a second command to learn which — the question the
    // line exists to answer.
    let running: Vec<String> = tasks
        .iter()
        .filter(|t| s(t, "status") == "active")
        .map(|t| {
            format!(
                "#{}",
                t.get("short_id").and_then(Value::as_i64).unwrap_or(0)
            )
        })
        .collect();
    if !running.is_empty() {
        parts.push(("timer.active", format!("{} running", running.join(" "))));
    }

    let blocked = tasks
        .iter()
        .filter(|t| t.get("blocked").and_then(Value::as_bool).unwrap_or(false))
        .count();
    if blocked > 0 {
        parts.push(("muted", format!("{blocked} blocked")));
    }
    summary_line(ctx, label, parts)
}

/// The summary line every table opens with: what was asked (`label`), then the
/// facts, each `(role, text)`, in falling order of what a reader loses by not
/// seeing them. `list`, `agenda`, `memory list` and `memory search` all go
/// through this one function, so a summary fits, drops and paints the same way
/// on each of them (#346).
///
/// The line has to FIT. It sits above the table, where a wrap would put a
/// second line between the header and the rows it labels — the old `N task(s)`
/// trailer could overflow harmlessly, and this cannot. Facts are dropped from
/// the RIGHT until it does, which is why callers push them in falling order:
/// for `list` the count, what is late, what is due today, what is running,
/// what is stuck. Dropping says less; truncating mid-word would say something
/// else.
fn summary_line(ctx: &Ctx, label: Option<&str>, parts: Vec<(&str, String)>) -> String {
    let head = label.map_or(0, |l| {
        width(&truncate(l, ctx.cols / 2, ctx.caps.unicode)) + 3
    });
    let widths: Vec<usize> = parts.iter().map(|(_, t)| width(t)).collect();
    let ranks: Vec<u8> = (0..parts.len()).map(|i| i as u8).collect();
    let keep = keep_ranked(
        &widths,
        &ranks,
        width(ctx.mid()) + 2,
        ctx.cols.saturating_sub(head),
    );
    let parts: Vec<(&str, String)> = parts
        .into_iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(p))
        .collect();

    let sep = format!(" {} ", ctx.paint("muted", ctx.mid()));
    let facts = parts
        .iter()
        .map(|(role, text)| ctx.paint(role, text))
        .collect::<Vec<_>>()
        .join(&sep);
    match label {
        // What was ASKED, echoed on every run rather than only on an empty
        // result. For `list` that is the filter — it defaults to `@working`
        // (`verbs::list`), and a reader who cannot see which question was
        // asked cannot tell a short answer from a narrow one. For `agenda` it
        // is the horizon, which its own trailer already stated on every run
        // and for the same reason: "N tasks" cannot be read as "and that is
        // all there is" unless the window it is all there is WITHIN is on the
        // same line.
        Some(l) => format!(
            "{}   {facts}",
            ctx.paint("muted", &truncate(l, ctx.cols / 2, ctx.caps.unicode))
        ),
        None => facts,
    }
}

/// Render a `task.list` result as an aligned, themed table.
///
/// `now` is a parameter and never the system clock — the rule this module
/// already states at [`agenda_select`], adopted here late: an internal read
/// made the overdue highlight untestable at the day boundary.
pub fn task_table(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    task_table_filtered(ctx, result, now, None)
}

/// [`task_table`], plus the filter DSL that produced `result` — echoed on an
/// empty result so "no pending tasks" and "this filter excludes everything"
/// (which look identical from outside) get different answers. Finding #4
/// (audit-2026-09): D55 wrote this argument for `pick`'s empty-set line
/// (DESIGN.md:1554) and it was never applied to the plain-list verb everyone
/// runs far more often. `None` (every other caller) keeps the original
/// unconditional "No tasks." — those callers have no single filter string to
/// name (the dashboard's `@working` repaint, the HTML report's grouped
/// tables).
pub fn task_table_filtered(
    ctx: &Ctx,
    result: &Value,
    now: Timestamp,
    filter: Option<&str>,
) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if tasks.is_empty() {
        // #229 item 1: matches `report`'s phrasing for the same situation —
        // an empty result set from a read verb — rather than the CLI naming
        // the same outcome two different ways depending which verb answered.
        // A genuinely empty store gets the onboarding hint regardless of
        // whether a filter was named; a filter that matched nothing on a
        // non-empty store gets to see the filter it excluded everything with.
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            match filter {
                Some(f) => format!("No tasks match `{f}`.\n"),
                None => "No matching tasks.\n".to_string(),
            }
        };
    }

    let refs: Vec<&Value> = tasks.iter().collect();
    let rows: Vec<TaskRow> = tasks
        .iter()
        .map(|t| task_row(t, now, ctx.caps.unicode))
        .collect();
    let c = TaskCols::fit(&rows, ctx.cols, "DUE", ctx.caps.unicode);

    let count = result
        .get("count")
        .and_then(Value::as_i64)
        .unwrap_or(tasks.len() as i64);

    // Summary, blank, header — three lines of chrome where the
    // header/rule/rule/trailer shape cost four, so `serve::watch_repaint`'s
    // row budget gains one.
    let mut out = String::new();
    out.push_str(&table_summary(ctx, &refs, &rows, count, filter, now, false));
    out.push('\n');
    out.push('\n');
    out.push_str(&ctx.paint("table.label", &header_line(&c, "DUE")));
    out.push('\n');

    for r in &rows {
        out.push_str(&row_line(ctx, &c, r));
        out.push('\n');
    }

    // Set off by a blank line. These are sentences carrying a command to
    // paste, not more rows, and the table no longer has a closing rule to end
    // it — without the gap the first note reads as a row whose columns broke.
    let notes = store_health_notes(tasks);
    if !notes.is_empty() {
        out.push('\n');
    }
    for note in notes {
        out.push_str(&prose(ctx, Some("warn"), &note, ""));
    }
    out
}

/// The notes a task table owes its reader about rows the store could not read
/// back cleanly — one per defect, naming the offending ids and the way out.
/// Empty for a healthy store, so their presence always means something.
///
/// Shared rather than written per view, and that is the whole point of it
/// being a function. Both [`task_table`] and [`agenda_text`] now draw a
/// STATUS marker for an unrecognized status ([`status_marker`], D86) — but
/// that column is droppable exactly like `DUE` under a narrow terminal or the
/// piped fixed width ([`TaskCols::fit`]), so the one row that most needs the
/// warning can be exactly the one the width squeeze takes it from. A title cell can
/// still come out empty with no column of its own at any width. `agenda`
/// shipped as a second table over the same rows and did not carry these notes
/// at all: an unreadable status sat under `Wed 2026-08-05` looking like
/// ordinary open work, and a blank-title row drew as an empty TASK cell with
/// nothing under the table to say why — the invisible-field failure rebuilt
/// one view over, which is what a copied layout does. A third view gets them
/// by calling this; it cannot get them by remembering to.
fn store_health_notes(tasks: &[Value]) -> Vec<String> {
    let mut notes = Vec::new();

    // The STATUS marker (D86) can be dropped by TaskCols::fit under a narrow
    // terminal or the piped fixed width, so a row the store could not read
    // back cleanly can still sit in the default view indistinguishable from
    // ordinary open work — the invisible-field failure this project keeps rebuilding.
    let broken: Vec<String> = tasks
        .iter()
        .filter(|t| status_is_unrecognized(t))
        .map(|t| {
            format!(
                "#{} ({})",
                t.get("short_id").and_then(Value::as_i64).unwrap_or(0),
                s(t, "status")
            )
        })
        .collect();
    if !broken.is_empty() {
        // Names the offending values and the way out. It is deliberately not
        // "run tasqx modify": status is not freely settable (§10 routes every
        // transition through start/stop/done), and the correct value is the
        // user's call, not ours — export, edit that one field, import.
        notes.push(format!(
            "unrecognized status in the store: {} — `tasqx export` still works; \
             fix the status there and `tasqx import` it back",
            broken.join(", ")
        ));
    }

    // The same failure shape one field over, and the worse one: a blank title
    // renders as an EMPTY cell, so the row is not merely unlabelled in the
    // default view — it is invisible in it. D36 closed every door that could
    // write one, but a store predating D36 can already hold one, and then its
    // `tasqx export` is a document that fails its own `tasqx import`, which
    // costs the user the escape hatch D28 relies on.
    //
    // Detection rather than repair, deliberately: D28 allows repair-on-open
    // only where the correct value is KNOWABLE (D23's stale `default_project`
    // could only be cleared), and nothing here knows what the title was meant
    // to say. Guessing would overwrite the user's bytes with no undo.
    //
    // `modify` is named as the way out, unlike the status note above, because a
    // title IS freely settable — there is no reason to route the user through
    // export/edit/import for a field one command can fix.
    let blank: Vec<String> = tasks
        .iter()
        .filter(|t| s(t, "title").trim().is_empty())
        .map(|t| {
            format!(
                "#{}",
                t.get("short_id").and_then(Value::as_i64).unwrap_or(0)
            )
        })
        .collect();
    if !blank.is_empty() {
        notes.push(format!(
            "blank title in the store: {} — written before this rule existed, and an export \
             holding one will not import; fix with `tasqx modify <ref> \"<a real title>\"`",
            blank.join(", ")
        ));
    }
    notes
}

/// True when the core flagged this task's `status` as text it could not
/// recognize. Reads the explicit boolean rather than re-parsing the string, so
/// one answer comes from the core and the CLI does not grow a second opinion.
fn status_is_unrecognized(t: &Value) -> bool {
    t.get("status_unrecognized")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

// ============================================================================
// Agenda — the same `task.list` answer, ordered by time instead of urgency
// ============================================================================

/// Which dated field put a task on the agenda.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum When {
    Due,
    Scheduled,
}

impl When {
    /// The word the `WHEN` cell opens with. Short, because it is repeated on
    /// every row and the date beside it is the information.
    fn label(self) -> &'static str {
        match self {
            When::Due => "due",
            When::Scheduled => "sched",
        }
    }
}

/// One task placed on the calendar.
///
/// Borrowed out of the `task.list` payload, not cloned into it: the clone
/// existed only to avoid a lifetime parameter, and the one caller holds the
/// payload for the agenda's whole life.
struct Entry<'a> {
    task: &'a Value,
    at: Timestamp,
    day: Date,
    kind: When,
    /// Late by [`overdue_at`] at the instant that placed the row (D131). A late
    /// row leads the table under `Overdue` even when its day is today.
    overdue: bool,
}

/// A `task.list` answer arranged as an agenda: the rows that have a place on the
/// calendar, in time order, plus an exact account of every row that does not.
///
/// The counts are not decoration. This view drops rows the filter DID match, for
/// two reasons of its own, and a time-ordered list that silently omits a task is
/// the failure this project keeps rebuilding (D33/D39): a deadline that is not on
/// the screen is indistinguishable from a deadline that does not exist. So each
/// reason carries its own count, and [`agenda_text`] prints the ones that are
/// non-zero together with the command or flag that reveals what they hold.
pub struct Agenda<'a> {
    entries: Vec<Entry<'a>>,
    /// Tasks with neither `due` nor `scheduled`. Nothing can put them on a day.
    undated: usize,
    /// Tasks dated past the horizon.
    beyond: usize,
    /// Days from `today` to the furthest thing `beyond` is holding. `None` when
    /// nothing is beyond the horizon.
    ///
    /// A distance, NOT necessarily a usable `--days`: it can exceed
    /// [`AGENDA_MAX_DAYS`], and then no window reaches that row and
    /// [`Agenda::omissions`] says so instead of quoting a flag value the parser
    /// refuses. `agenda_json` reports it beside `max_days` so a script can tell
    /// the two cases apart the same way the footer does.
    reach_days: Option<usize>,
    /// Store-health notes over every task the filter matched — see
    /// [`store_health_notes`]. Computed over the whole result and not over
    /// `entries`, because an unreadable status or a blank title is a fact about
    /// the store whether or not that row happened to land inside the horizon.
    health: Vec<String>,
    today: Date,
    through: Date,
    days: usize,
    /// Overdue rows the [`AGENDA_OVERDUE_CAP`] cut, over and above the
    /// [`AGENDA_OVERDUE_CAP`] kept in `entries`. The past side of rule 3 has
    /// no horizon to bound it (`--days` never applies to it), so on a store
    /// with years of history this is the count that keeps the view itself
    /// from being the thing burying the day headings it exists to show.
    overdue_cut: usize,
    /// The oldest day among the rows `overdue_cut` counts. `None` when
    /// nothing was cut.
    overdue_oldest: Option<Date>,
    /// Whether the underlying `task.list` result named a genuinely empty
    /// store (#233.1) — the signal [`agenda_text`] needs to tell "nothing
    /// scheduled yet" apart from "nothing scheduled" on a real backlog.
    store_empty: bool,
}

/// A screenful: past this many overdue rows, the Overdue group stops being a
/// list a person can act on and starts being the reason `Today` is 1900 lines
/// below the fold. Not user-configurable (unlike `--days`) because the choice
/// here is a rendering limit, not a question about what the caller wants
/// included — `tasqx list due.before:today` already shows every one of them,
/// uncapped, to whoever asks for that.
const AGENDA_OVERDUE_CAP: usize = 20;

/// Arrange a `task.list` result into an [`Agenda`].
///
/// `now` is a parameter and never the system clock, for the reason
/// `datetime::parse_when` gives: a view whose grouping depends on a hidden clock
/// cannot be tested at the boundaries that matter — the last second of today,
/// the first of the horizon.
///
/// # Which field orders it, and why both
///
/// `due` and `scheduled` are both calendar facts and they answer different
/// questions: `scheduled` is when you meant to START, `due` is when it must be
/// FINISHED. A view built on `due` alone loses every planned-but-undeadlined
/// task, which is most of what a week actually contains; one built on
/// `scheduled` alone loses every deadline. So a task is placed on the EARLIER of
/// the two — the first day it asks anything of you — and the row says which
/// field that was, because "Friday" means something different under each. When
/// the two coincide the label is `due`: a deadline is the more consequential
/// reading of the same instant.
///
/// A task carrying NEITHER is not on the agenda at all, because there is no
/// honest day to put it on — but it is counted, and the footer names `tasqx
/// list` as the view that does show it. Inventing a "Someday" bucket was
/// rejected: it would sort a hundred undated backlog rows into the same screen
/// as this week, which is precisely what `list`'s urgency order is for.
///
/// # Done and cancelled
///
/// Never seen here: which statuses reach this function is the caller's filter,
/// and `lib::run_agenda` composes an open-status default into it under D24's
/// resolution order. Filtering again here would be a second opinion about a
/// question the filter grammar already answers, and the two would drift.
pub fn agenda_select(result: &Value, days: usize, now: Timestamp) -> Agenda<'_> {
    // Borrowed with the payload's own lifetime — a local empty-vec fallback
    // would tie the borrow to this frame and forbid returning it.
    let tasks: &[Value] = result
        .get("tasks")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);

    // Everything the store holds is UTC (`datetime.rs`: a naive date resolves to
    // 00:00 UTC), so the day boundaries are UTC too. Grouping by the LOCAL day
    // was considered and rejected: `--due 2026-08-05` is stored as midnight UTC,
    // and west of Greenwich that instant is the 4th, so a local-day agenda would
    // file the task one day before the date the user typed. Matching the
    // parser's zone is the only arrangement in which a date round-trips.
    let today = now.to_zoned(TimeZone::UTC).date();
    // `--days` is bounded at parse time at `AGENDA_MAX_DAYS`, so this
    // addition cannot overflow. `Date::MAX` on failure anyway, because the
    // failure direction that matters is "show it" — a horizon that cannot be
    // computed must not silently swallow the rows past it.
    let through = today.checked_add((days as i64).days()).unwrap_or(Date::MAX);

    let mut a = Agenda {
        entries: Vec::new(),
        undated: 0,
        beyond: 0,
        reach_days: None,
        health: store_health_notes(tasks),
        today,
        through,
        days,
        overdue_cut: 0,
        overdue_oldest: None,
        store_empty: store_is_empty(result),
    };

    // Kept separate from the future side while the cap decision is made: the
    // two are re-merged below, once `overdue` has been trimmed to its cap.
    let mut overdue: Vec<Entry> = Vec::new();
    for t in tasks {
        let (at, kind) = match (field_ts(t, "due"), field_ts(t, "scheduled")) {
            (Some(d), Some(sc)) if sc < d => (sc, When::Scheduled),
            (Some(d), _) => (d, When::Due),
            (None, Some(sc)) => (sc, When::Scheduled),
            (None, None) => {
                a.undated += 1;
                continue;
            }
        };
        let day = at.to_zoned(TimeZone::UTC).date();
        if day > through {
            a.beyond += 1;
            // The reach is REPORTED, not guessed: the footer will name the
            // exact `--days` that brings the furthest of these into view, so
            // the reader does not have to widen the window by trial. A raw
            // distance, deliberately — `omissions()` decides what to do with
            // one that no window can reach, because that is a question about
            // advice and this loop only knows facts.
            let need = day
                .since((Unit::Day, today))
                .map(|s| s.get_days().max(0) as usize)
                .unwrap_or(days);
            a.reach_days = Some(a.reach_days.map_or(need, |cur: usize| cur.max(need)));
            continue;
        }
        let late = overdue_at(at, now);
        let entry = Entry {
            task: t,
            at,
            day,
            kind,
            overdue: late,
        };
        if late {
            overdue.push(entry);
        } else {
            a.entries.push(entry);
        }
    }

    // Rule 3 gives the past side no horizon at all, so on a store with years
    // of open work this is the one place left that can grow without bound.
    // Kept: the [`AGENDA_OVERDUE_CAP`] rows the caller's own sort already
    // ranked hottest — `overdue` was filled in the engine's `-urgency` order
    // (the same request `list` sends, `run_agenda` asks for it byte for
    // byte), so truncating it keeps exactly "the most urgent N" without a
    // second sort here that could disagree with the engine's.
    if overdue.len() > AGENDA_OVERDUE_CAP {
        a.overdue_oldest = overdue[AGENDA_OVERDUE_CAP..].iter().map(|e| e.day).min();
        a.overdue_cut = overdue.len() - AGENDA_OVERDUE_CAP;
        overdue.truncate(AGENDA_OVERDUE_CAP);
    }
    a.entries.extend(overdue);

    // STABLE, and by the instant alone. The rows arrive in the engine's
    // `-urgency` order, so two tasks landing on the same instant keep the
    // ranking the rest of the tool would give them instead of an arbitrary one.
    a.entries.sort_by_key(|e| (!e.overdue, e.at));
    a
}

/// The heading a row's day sits under. Every row before "now" collapses into ONE
/// group rather than getting a heading per past day: a store that has been
/// running for a year would otherwise open the agenda with a hundred headings
/// nobody can act on, and what the reader needs from the past is the list, not
/// the calendar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Overdue,
    Day(Date),
}

fn group_of(e: &Entry) -> Group {
    if e.overdue {
        Group::Overdue
    } else {
        Group::Day(e.day)
    }
}

/// Three letters, written out rather than taken from a locale: the rest of this
/// module renders one fixed English surface, and a heading whose width changes
/// with `$LANG` would move a column the table has already been fitted to.
fn weekday_abbrev(d: Date) -> &'static str {
    match d.weekday() {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

/// `Today · Mon 3 Aug`. The date is spelled out on every heading, today's
/// included: "Today" alone is the one label that means something different
/// tomorrow, and terminal output gets pasted into tickets. It is spelled by
/// [`calendar_date`], `list`'s spelling, not ISO (D133), so a heading more than
/// a year out keeps its two-digit year and still names exactly one day.
fn day_heading(day: Date, today: Date) -> String {
    let stamp = format!("{} {}", weekday_abbrev(day), calendar_date(day, today));
    let tomorrow = today.tomorrow().ok();
    if day == today {
        format!("Today · {stamp}")
    } else if Some(day) == tomorrow {
        format!("Tomorrow · {stamp}")
    } else {
        stamp
    }
}

/// The `WHEN` cell: which field placed this row, plus the part of the instant
/// its heading does not already carry.
///
/// Inside a day group the heading names the date, so the cell shows the time —
/// and only when there is one to show. A date typed without a time resolves to
/// 00:00 UTC (`datetime.rs`), so midnight is precisely the store's spelling of
/// "no time given"; printing `due 00:00` on every row would fill the column with
/// a time nobody typed. A caller who genuinely meant midnight loses nothing the
/// store itself distinguishes.
///
/// The overdue group spans many days and so has no date in its heading. Its
/// cells carry the day instead, in [`due_cell`]'s words — `due 2d ago`,
/// `due today 17:00` — because "how late" is the whole content of that group,
/// and `list` beside it spells the same row that way (D133). `overdue_now` is
/// the instant those words are relative to; `None` is a row under a heading.
fn when_cell(kind: When, at: Timestamp, overdue_now: Option<Timestamp>) -> String {
    if let Some(now) = overdue_now {
        return format!("{} {}", kind.label(), due_cell(at, now));
    }
    let t = at.to_zoned(TimeZone::UTC).time();
    let stamp = if t.hour() == 0 && t.minute() == 0 {
        String::new()
    } else {
        format!("{:02}:{:02}", t.hour(), t.minute())
    };
    match (stamp.is_empty(), kind) {
        // A deadline on the day its own heading names, with no time in the
        // store: the heading is the whole answer, and a column of rows reading
        // `due` `due` `due` beneath `Tomorrow · Fri 11 Sep` spends cells
        // repeating it. Blank here means "the heading said it" — and if every
        // row on the agenda is one of these, `TaskCols::fit` drops the column
        // outright, which is the correct answer to a view whose day headings
        // carry all of the time information there is (D51, D117).
        (true, When::Due) => String::new(),
        // `sched` is not the default reading, so it says so even bare: this row
        // is on this day because you meant to START it, not because anything is
        // owed. (The overdue group never reaches here: it returned above.)
        (true, When::Scheduled) => kind.label().to_string(),
        (false, _) => format!("{} {stamp}", kind.label()),
    }
}

/// Render an [`Agenda`] as day-grouped tables sharing ONE fitted layout.
///
/// The columns are fitted once, over every visible row, and every group prints
/// under that one layout — so `Today` and `Fri` line up with each other. Fitting
/// per group would size each day's `TASK` column to its own longest title, and
/// the result reads as a table that changes shape as you scroll down it.
pub fn agenda_text(ctx: &Ctx, a: &Agenda) -> String {
    let mut out = String::new();

    // The horizon is stated on every run, not only when it cut something: "5
    // tasks" alone cannot be read as "and that is all there is" unless the
    // window it is all there is WITHIN is on the same line. No weekday here,
    // unlike the day headings — the count can be four digits and this line has
    // to survive a 40-cell terminal, and the headings already carry the days.
    //
    // That claim stops being true when every visible row is overdue: `--days
    // 14` did not put any of them on screen, so "through <date> (+14d)"
    // attached to a set that is 100% before today would be advertising a
    // horizon none of the rows are anywhere near. `has_future` asks the rows
    // that actually made it past the cap, not the raw counts, so a store cut
    // down to nothing-but-overdue prints the same honest line as one that
    // never had a future row to begin with.
    //
    // D117 moved it from a trailer to the head of the table, where `list`'s
    // filter sits: both views open by naming the question they answered.
    let has_future = a.entries.iter().any(|e| !e.overdue);
    let horizon = if a.entries.is_empty() || has_future {
        format!(
            "through {} (+{}d)",
            calendar_date(a.through, a.today),
            a.days
        )
    } else {
        "all overdue".to_string()
    };

    let refs: Vec<&Value> = a.entries.iter().map(|e| e.task).collect();
    if !refs.is_empty() {
        let rows: Vec<TaskRow> = a
            .entries
            .iter()
            .map(|e| {
                let mut r = task_row(e.task, a.at_start_of_today(), ctx.caps.unicode);
                let overdue = e.overdue;
                r.due = when_cell(e.kind, e.at, overdue.then(|| a.at_start_of_today()));
                // Repainted from the AGENDA instant, not from `due` alone: a
                // task scheduled last week and due next month is late on the
                // thing this row is about, and `task_row`'s answer is about the
                // deadline only.
                r.overdue = overdue && status_is_open(&s(e.task, "status"));
                r
            })
            .collect();
        let c = TaskCols::fit(&rows, ctx.cols, "WHEN", ctx.caps.unicode);

        out.push_str(&table_summary(
            ctx,
            &refs,
            &rows,
            a.entries.len() as i64,
            Some(&horizon),
            a.at_start_of_today(),
            true,
        ));
        out.push('\n');
        out.push('\n');
        out.push_str(&ctx.paint("table.label", &header_line(&c, "WHEN")));
        out.push('\n');

        let mut current: Option<Group> = None;
        for (e, r) in a.entries.iter().zip(&rows) {
            let g = group_of(e);
            if current != Some(g) {
                // A blank line ahead of every heading but the first. Groups run
                // flush otherwise, and a heading with no air above it reads as
                // one more row of the group it is ending rather than the start
                // of the next.
                if current.is_some() {
                    out.push('\n');
                }
                let (role, text) = match g {
                    Group::Overdue => ("overdue", "Overdue".to_string()),
                    Group::Day(d) => ("accent", day_heading(d, a.today)),
                };
                out.push_str(&ctx.paint(role, &truncate(&text, ctx.cols, ctx.caps.unicode)));
                out.push('\n');
                current = Some(g);
            }
            out.push_str(&row_line(ctx, &c, r));
            out.push('\n');
        }
    }

    // An agenda with nothing on it draws no table at all, so the line that
    // names the horizon has to be printed here too — it is the only thing on
    // screen saying what was looked at and found empty.
    if a.entries.is_empty() {
        out.push_str(&ctx.paint("muted", &format!("{}   {}", horizon, plural_tasks(0))));
        out.push('\n');
        // #233.1: an empty agenda on a genuinely empty store is the same dead
        // end as `list`'s and `next`'s — append the getting-started hint rather
        // than leaving that one line alone on the screen.
        if a.store_empty {
            out.push_str(&onboarding_hint(ctx));
        }
    }
    // Same blank line as `task_table`'s, and for the same reason: prose that
    // starts flush against the last row reads as a row that came out wrong.
    let omissions = a.omissions();
    if !omissions.is_empty() || !a.health.is_empty() {
        out.push('\n');
    }
    for note in omissions {
        out.push_str(&prose(ctx, Some("muted"), &note, ""));
    }
    // Last, and in `warn` rather than `muted`, because these are not this
    // view's own omissions: they are damage in the store that this layout —
    // like `list`'s, which it shares — cannot show in a cell. See
    // `store_health_notes` for why both views read one implementation.
    for note in &a.health {
        out.push_str(&prose(ctx, Some("warn"), note, ""));
    }
    out
}

impl Agenda<'_> {
    /// Midnight of the agenda's `today`, used as the "now" [`task_row`] compares
    /// `due` against. The agenda has its own overdue answer (per row, from the
    /// agenda instant), so this only has to be a stable instant on the right day
    /// rather than the wall clock — and taking it from `today` keeps the whole
    /// render a pure function of the `now` that was passed in.
    fn at_start_of_today(&self) -> Timestamp {
        self.today
            .to_zoned(TimeZone::UTC)
            .map(|z| z.timestamp())
            .unwrap_or_else(|_| Timestamp::now())
    }

    /// One line per reason this view is holding something back, each naming the
    /// way to see it. Empty on an agenda that omitted nothing — the notes are
    /// conditional so that their presence always means something.
    fn omissions(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.undated > 0 {
            v.push(format!(
                "{} undated — no due or scheduled date, so nothing puts them on a day; \
                 `tasqx list` shows them",
                self.undated
            ));
        }
        if self.beyond > 0 {
            // The exact flag that reaches the furthest one, so widening the
            // window is one paste rather than a guess-and-retry — but only when
            // the CLI would accept it. `--days` is bounded at 3650
            // (`AGENDA_MAX_DAYS`) and the reach is a raw distance, so a task due
            // in 2060 used to print ``tasqx agenda --days 12204``, which exits 2
            // with `12204 is not in 1..=3650`. A footer that hands out a refused
            // command is worse than one that admits the row is out of range: the
            // reader spends the retry before learning anything.
            //
            // So past the ceiling the note says the widest window still does not
            // reach, and names `tasqx list` — the view with no horizon at all —
            // exactly as the undated note does. The count stays either way,
            // which is the promise that actually matters: nothing is dropped in
            // silence.
            //
            // One line either way, including the mixed case where some cut rows
            // ARE reachable and the furthest is not. This note has only ever
            // named one number — the furthest — so the mixed case loses nothing
            // it used to have, and a second line ("--days 3650 reaches all but
            // one") would need a second counter to be true, which is more
            // machinery than a footer is worth. `tasqx list` shows every one of
            // them regardless.
            let reach = self.reach_days.unwrap_or(self.days);
            v.push(if reach > AGENDA_MAX_DAYS {
                format!(
                    "{} further out — past `--days {AGENDA_MAX_DAYS}`, the widest window there \
                     is; `tasqx list` shows them",
                    self.beyond
                )
            } else {
                format!(
                    "{} further out — `tasqx agenda --days {reach}` reaches the furthest",
                    self.beyond
                )
            });
        }
        if self.overdue_cut > 0 {
            // Same style as `undated`/`beyond`: a count, and the command that
            // shows the rest — `list`'s own horizon-free filter, since
            // `--days` (rule 3) never reaches the past side anyway.
            let oldest = self
                .overdue_oldest
                .map(|d| calendar_date(d, self.today))
                .unwrap_or_default();
            v.push(format!(
                "{} more overdue, oldest {oldest} — `tasqx list due.before:today` shows them",
                self.overdue_cut
            ));
        }
        v
    }
}

/// The `--json` half of the agenda, and the reason it is not simply the
/// `task.list` result.
///
/// `--json` and the table must answer the same question (the rule
/// `report --html` already follows). Handing the raw result back would make
/// `tasqx agenda --json | jq '.tasks | length'` report every matching task,
/// horizon and undated rows included, while the table beside it showed five —
/// a number that means something different depending on which flag was passed.
/// So the array is the rows the table drew, in the order it drew them, and every
/// count the footer prints is a field a script can read.
pub fn agenda_json(a: &Agenda) -> Value {
    serde_json::json!({
        "tasks": a.entries.iter().map(|e| e.task.clone()).collect::<Vec<_>>(),
        "count": a.entries.len(),
        "agenda": {
            "days": a.days,
            "today": a.today.to_string(),
            "through": a.through.to_string(),
            "undated": a.undated,
            "beyond_horizon": a.beyond,
            "reach_days": a.reach_days,
            // The past-side counterpart of `beyond_horizon`/`reach_days`: how
            // many overdue rows the screenful cap held back, and the oldest
            // of them. Zero/null on any agenda the cap did not touch.
            "overdue_cut": a.overdue_cut,
            "overdue_oldest": a.overdue_oldest.map(|d| d.to_string()),
            // The ceiling, so a script can make the decision the footer makes.
            // `reach_days` is a distance and may exceed it, and without this
            // field the obvious `tasqx agenda --days $(jq .agenda.reach_days)`
            // is the same exit-2 the text note used to walk the reader into.
            "max_days": AGENDA_MAX_DAYS,
        }
    })
}

/// Full task detail (task.get): fields plus tags, deps, annotations, blocked.
/// The status cell both detail layouts print: the plain string, unless the
/// wire sent a status this build cannot parse — then say so, and say what the
/// real ones are. The list of real ones is derived, never retyped:
/// `Status::ALL` is the canonical list. One helper, because the card layout
/// (D76) renders the same fact and two copies of the unrecognized branch is
/// how they would drift apart.
///
/// `now` is a parameter rather than a `Timestamp::now()` call inside — the same
/// reason [`task_table`] takes one: relative rendering (`detail.time_format`)
/// must be reproducible and testable, not a hidden clock.
fn status_cell(ctx: &Ctx, result: &Value) -> String {
    if status_is_unrecognized(result) {
        ctx.paint(
            "warn",
            &format!(
                "{}  (unrecognized — not one of {})",
                s(result, "status"),
                tasqx_core::types::Status::ALL
                    .map(tasqx_core::types::Status::as_str)
                    .join(", ")
            ),
        )
    } else {
        s(result, "status")
    }
}

pub fn task_detail(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    // The card is the interactive rendering (D76); everything below it is the
    // byte-stable plain layout every pipe, script and docs example reads.
    if ctx.caps.unicode {
        return task_detail_card(ctx, result, now);
    }
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let mut out = String::new();
    out.push_str(&ctx.paint("header", &format!("#{sid}  {}", s(result, "title"))));
    out.push('\n');
    for row in detail_rows(ctx, result, now) {
        if matches!(row.field, DetailField::Annotation) {
            out.push_str(&format!("  {} {}\n", ctx.paint("muted", "·"), row.value));
            continue;
        }
        // D138: the marker carries the state, so it is painted and the
        // criterion is not — a failed check should catch the eye at the marker
        // rather than shouting its whole line.
        if matches!(row.field, DetailField::Check) {
            let role = match row.label {
                "[x]" => "ok",
                "[!]" => "danger",
                _ => "muted",
            };
            out.push_str(&format!("  {} {}\n", ctx.paint(role, row.label), row.value));
            continue;
        }
        // The plain layout's emphasis map over the SAME row set the card
        // renders — the facts and their conditions live in `detail_rows`.
        let cell = match row.field {
            DetailField::Project => ctx.paint("project", &row.value),
            DetailField::Remind | DetailField::Repeats => ctx.paint("accent", &row.value),
            DetailField::Blocked => ctx.paint("danger", &row.value),
            DetailField::Tags => ctx.paint("tag", &row.value),
            _ => row.value,
        };
        out.push_str(&format!("  {:<11}{cell}\n", row.label));
    }
    out
}

/// `tasqx brief` (D136): `show`'s card, then what the prerequisites concluded,
/// then what the store already knows about this subject.
///
/// The task half is [`task_detail`] unchanged, for D49's reason one surface
/// over: one task reads the same however it was asked for, and a second layout
/// for the same object is how two screens start disagreeing about one row.
/// Each section is omitted when empty rather than printed with "none" under it
/// — house style rule 12, and a brief is read before work starts, so a heading
/// that says nothing costs attention at the worst moment.
pub fn task_brief(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let mut out = task_detail(ctx, result.get("task").unwrap_or(result), now);

    let list = |key: &str| -> Vec<Value> {
        result
            .get("neighbourhood")
            .and_then(|v| v.get(key))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let heading = |out: &mut String, text: &str| {
        out.push('\n');
        out.push_str(&ctx.paint("table.label", text));
        out.push('\n');
    };
    let ref_line = |v: &Value| {
        let sid = v.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        format!(
            "  {}  {}  {}",
            ctx.paint("accent", &format!("#{sid}")),
            san(&s(v, "title")),
            ctx.paint("muted", &san(&s(v, "status")))
        )
    };

    let depends_on = list("depends_on");
    if !depends_on.is_empty() {
        heading(&mut out, "DEPENDS ON");
        for d in &depends_on {
            out.push_str(&ref_line(d));
            out.push('\n');
            // The prerequisite's last word, wrapped to the terminal under it.
            // This is the row the section exists for: what the upstream task
            // concluded is what a reader needs before starting, and reading it
            // used to cost a second `tasqx show`.
            if let Some(a) = d.get("annotation").filter(|a| !a.is_null()) {
                for line in wrap_words(&san(&s(a, "body")), ctx.cols.saturating_sub(6).max(20)) {
                    out.push_str(&format!("    {}\n", ctx.paint("muted", &line)));
                }
            }
        }
    }

    let blocks = list("blocks");
    if !blocks.is_empty() {
        heading(&mut out, "BLOCKS");
        for b in &blocks {
            out.push_str(&ref_line(b));
            out.push('\n');
        }
    }

    let hits = result
        .get("memory")
        .and_then(|m| m.get("hits"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !hits.is_empty() {
        heading(&mut out, "FROM MEMORY");
        for h in &hits {
            let source = s(h, "source");
            out.push_str(&format!(
                "  {}{}\n",
                san(&s(h, "title")),
                if source.is_empty() {
                    String::new()
                } else {
                    format!("  {}", ctx.paint("muted", &san(&source)))
                }
            ));
            let snippet = san(&s(h, "snippet"));
            if !snippet.is_empty() {
                for line in wrap_words(&snippet, ctx.cols.saturating_sub(6).max(20)) {
                    out.push_str(&format!("    {}\n", ctx.paint("muted", &line)));
                }
            }
        }
        // D70's rule one surface over: a bounded page that does not say it is
        // bounded is read as the whole answer.
        let total = result
            .get("memory")
            .and_then(|m| m.get("total"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if total > hits.len() as i64 {
            out.push('\n');
            out.push_str(&prose(
                ctx,
                Some("muted"),
                &format!(
                    "{} of {total} matches shown — tasqx memory search reaches the rest.",
                    hits.len()
                ),
                "",
            ));
        }
    }
    out
}

/// An instant as a detail view spells it (D122): the stored text under
/// `detail.time_format = iso`, and otherwise the calendar day `list` would
/// print (`Wed 16 Sep`, `today 17:00`, `3d ago`), followed by the elapsed
/// words where they add something (`Wed 16 Sep (in 5 days)`).
///
/// It used to be `fmt_instant`'s spelling: the raw instant, nanoseconds
/// included (`2026-09-11T09:36:45.809321632Z (just now)`), under the default
/// `both`. That is rule 3 broken on the one screen that exists to read a
/// task's dates, and the width it cost is what made `show` wrap at 60
/// columns. The MCP text view keeps core's spelling; this is the terminal's.
fn instant_value(ctx: &Ctx, v: &str, now: Timestamp) -> String {
    use tasqx_core::markdown::{fmt_instant, DetailOpts, TimeFormat};
    let Ok(at) = v.parse::<Timestamp>() else {
        return v.to_string();
    };
    if ctx.time_format == TimeFormat::Iso {
        return v.to_string();
    }
    let day = if at >= now {
        due_cell(at, now)
    } else {
        let today = now.to_zoned(TimeZone::UTC).date() == at.to_zoned(TimeZone::UTC).date();
        if today {
            due_cell(at, now)
        } else {
            day_ago(at, now)
        }
    };
    if ctx.time_format == TimeFormat::Relative {
        return day;
    }
    // `both`: the elapsed words, unless the day already is elapsed words
    // (`yesterday`, `3d ago`), where they would say it twice.
    let days = (at.as_second() - now.as_second()).abs() / 86_400;
    if at < now && days < 7 && !day.starts_with("today") {
        return day;
    }
    let rel = fmt_instant(
        v,
        &DetailOpts {
            time: TimeFormat::Relative,
            now,
        },
    );
    format!("{day} ({rel})")
}

/// A duration as a detail view spells it (D122): `PT5H` under `iso`, `5h`
/// otherwise. `both` used to print `PT5H (5h)`, the same duration twice.
fn duration_value(ctx: &Ctx, v: &str, now: Timestamp) -> String {
    use tasqx_core::markdown::{fmt_duration, DetailOpts, TimeFormat};
    let time = if ctx.time_format == TimeFormat::Iso {
        TimeFormat::Iso
    } else {
        TimeFormat::Relative
    };
    fmt_duration(v, &DetailOpts { time, now })
}

/// Which fact a detail row names, so each layout can map the same row set to
/// its own emphasis without restating the row conditions.
enum DetailField {
    Status,
    Project,
    Urgency,
    Due,
    Remind,
    Scheduled,
    Wait,
    Repeats,
    Estimate,
    Completed,
    Tracked,
    Blocked,
    Tags,
    DependsOn,
    Blocks,
    Created,
    Modified,
    Rev,
    Budget,
    Tokens,
    Check,
    Annotation,
}

/// One row of a task detail: the label spelling both layouts print, the
/// rendered value, and which fact it is.
struct DetailRow {
    label: &'static str,
    field: DetailField,
    value: String,
}

/// The rows a task detail names, in order, CONDITIONS INCLUDED — one walk for
/// both layouts. The plain and card views used to keep ~18 conditions in sync
/// by hand (`tracked != "PT0S"`, non-empty `completed`, …) with a parity test
/// that checked label presence only against an all-fields fixture, so a
/// condition drifting in one layout passed it. A row that renders in one
/// layout and not the other is unrepresentable now.
fn detail_rows(ctx: &Ctx, result: &Value, now: Timestamp) -> Vec<DetailRow> {
    // `detail.time_format` (D49's "on the one retreat", now reaching `show` —
    // see `Ctx::with_time_format`), read through the same pure formatters
    // `tasqx_get_task` uses, so `PT4H` becomes `4h` and an ISO instant becomes
    // `Thu 10 Sep` / `in 2 days` identically on both surfaces. Only the
    // *values* converge here; the layout stays each renderer's own (D78's rail
    // card is untouched by this).
    // D132: every clock on this screen is UTC, and the first one that shows a
    // clock says so — once, after the clock, not on every cell. `iso` prints
    // the stored `…Z`, which already says it, and a card with no clock on it
    // has nothing to mark.
    let marked = std::cell::Cell::new(false);
    let fmt_i = |v: &str| {
        let out = instant_value(ctx, v, now);
        if marked.get()
            || ctx.time_format == tasqx_core::markdown::TimeFormat::Iso
            || v.parse::<Timestamp>().is_err()
        {
            return out;
        }
        let (day, rest) = out.split_at(out.find(" (").unwrap_or(out.len()));
        if !day.contains(':') {
            return out;
        }
        marked.set(true);
        format!("{day} UTC{rest}")
    };
    let fmt_d = |v: &str| duration_value(ctx, v, now);

    let mut rows = Vec::new();
    let mut row = |label: &'static str, field: DetailField, value: String| {
        rows.push(DetailRow {
            label,
            field,
            value,
        });
    };

    // D122: a running task says so in its status, not on a `running` row of
    // its own beside `status active`, which said the same thing twice. The
    // open interval is still not folded into `tracked` (see `task_to_json`),
    // so the moment it started is what distinguishes "running" from "done
    // running": it rides on the status.
    let mut status = status_cell(ctx, result);
    if !s(result, "active_since").is_empty() && !status_is_unrecognized(result) {
        status = format!("{status} · since {}", fmt_i(&s(result, "active_since")));
    }
    row("status", DetailField::Status, status);
    // D122: blocked is a line only when it is true, and it names what blocks
    // the task. It used to be `blocked true` beside `depends_on #51`, two rows
    // for one fact, and `blocked false` on every other task, which is noise.
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if blocked {
        let empty = Vec::new();
        let blockers: Vec<String> = result
            .get("unmet_blockers")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
            .iter()
            .map(|b| {
                let sid = b.get("short_id").and_then(Value::as_i64).unwrap_or(0);
                let title = san(b.get("title").and_then(Value::as_str).unwrap_or(""));
                if title.is_empty() {
                    format!("#{sid}")
                } else {
                    format!("#{sid} · {title}")
                }
            })
            .collect();
        let named = if blockers.is_empty() {
            // An older core, or a caller that trimmed the field: the edge is
            // still in `depends_on`.
            result
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|d| {
                    d.iter()
                        .filter_map(Value::as_i64)
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        } else {
            blockers.join(", ")
        };
        row("blocked", DetailField::Blocked, format!("by {named}"));
    }
    // D122 (rule 5): priority sits in the urgency cell it modifies, as in
    // `list`, rather than on a row of its own describing a number two rows
    // away.
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    let prio = result
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("-");
    row("urgency", DetailField::Urgency, format!("{prio} {urg:.1}"));
    if !s(result, "project").is_empty() {
        row("project", DetailField::Project, s(result, "project"));
    }
    if !s(result, "due").is_empty() {
        row("due", DetailField::Due, fmt_i(&s(result, "due")));
    }
    if !s(result, "remind").is_empty() {
        row("remind", DetailField::Remind, fmt_i(&s(result, "remind")));
    }
    if !s(result, "scheduled").is_empty() {
        row(
            "scheduled",
            DetailField::Scheduled,
            fmt_i(&s(result, "scheduled")),
        );
    }
    if !s(result, "wait").is_empty() {
        row("wait", DetailField::Wait, fmt_i(&s(result, "wait")));
    }
    if !s(result, "recurrence").is_empty() {
        row("repeats", DetailField::Repeats, s(result, "recurrence"));
    }
    if !s(result, "estimate").is_empty() {
        row(
            "estimate",
            DetailField::Estimate,
            fmt_d(&s(result, "estimate")),
        );
    }
    // Conditional for the reason `tracked` is: only a closed task HAS a
    // completion moment, and an empty `completed` row on every pending task is
    // noise. It was stored, returned by `task.get` and rendered by `done` — the
    // one surface that scrolls away — so the detail view, whose whole job is
    // showing a task's fields, was the only place the moment could be looked up
    // later and the only place it did not appear.
    if !s(result, "completed").is_empty() {
        row(
            "completed",
            DetailField::Completed,
            fmt_i(&s(result, "completed")),
        );
    }
    // Conditional, unlike `blocked`: every task has a blocked answer worth
    // reading, but "tracked PT0S" on the many tasks that were never timed is
    // noise on the detail of every one of them. Shown from the first tracked
    // second onward, which is when the number starts meaning something.
    let tracked = s(result, "tracked");
    if !tracked.is_empty() && tracked != "PT0S" {
        row("tracked", DetailField::Tracked, fmt_d(&tracked));
    }
    if let Some(tags) = result.get("tags").and_then(Value::as_array) {
        if !tags.is_empty() {
            // `+tag`, matching `list`'s table and its own filter grammar — see
            // the `tags:` field comment on `TaskRow` (#228.16).
            let names: Vec<String> = tags
                .iter()
                .filter_map(Value::as_str)
                .map(|t| format!("+{}", san(t)))
                .collect();
            row("tags", DetailField::Tags, names.join(" "));
        }
    }
    // Only while the task is not blocked: a blocked task's `blocked` line
    // already names what it waits on, and the edges to finished tasks no
    // longer stop anything.
    if let Some(deps) = result.get("depends_on").and_then(Value::as_array) {
        if !deps.is_empty() && !blocked {
            let refs: Vec<String> = deps
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            row("depends_on", DetailField::DependsOn, refs.join(" "));
        }
    }
    // The reverse edge (tasqx audit #159): `depends_on` names what blocks
    // this task, `blocks` names what THIS task blocks. Conditional like its
    // sibling above — a leaf that blocks nothing is the common case and an
    // empty row on every one of them would be noise.
    if let Some(blocks) = result.get("blocks").and_then(Value::as_array) {
        if !blocks.is_empty() {
            let refs: Vec<String> = blocks
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            row("blocks", DetailField::Blocks, refs.join(" "));
        }
    }
    // D139: the gauge, and only when a threshold was set — `fresh_tokens`
    // alone is a number with nothing to read it against, and the TOKENS row
    // below already says what was spent. Sits above `created` for the reason
    // `tracked` sits where it does: it is a fact about the work, not about the
    // row's bookkeeping.
    if let Some(budget) = result.get("budget_tokens").and_then(Value::as_i64) {
        let fresh = result
            .get("fresh_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let over = result.get("over").and_then(Value::as_bool).unwrap_or(false);
        row(
            "budget",
            DetailField::Budget,
            format!(
                "{} / {} fresh{}",
                crate::tokens::compact(fresh),
                crate::tokens::compact(budget),
                if over { " — over" } else { "" }
            ),
        );
    }
    // Always rendered, unconditionally — the same rule D49 states for
    // `tasqx_get_task`: `status`, `priority`, `project`, `created`, `modified`
    // and `_rev` are never the noisy zero a reader would want hidden, and
    // `_rev` in particular is the value `--expected-rev` needs, which used to
    // force a `--json` round trip just to read it back (audit #188).
    row(
        "created",
        DetailField::Created,
        fmt_i(&s(result, "created")),
    );
    row(
        "modified",
        DetailField::Modified,
        fmt_i(&s(result, "modified")),
    );
    row(
        "rev",
        DetailField::Rev,
        result
            .get("_rev")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .to_string(),
    );
    // D39: AI token spend renders here or it is data nobody reported.
    // Conditional like `tracked`: most tasks never get a measurement, and four
    // zeroes on every one of them is noise. Totals, not per-measurement rows —
    // the detail view answers "what did this task cost", and `--json` carries
    // the individual measurements for anyone who needs them.
    if let Some(tokens) = result.get("tokens").and_then(Value::as_array) {
        if !tokens.is_empty() {
            let sum = |key: &str| -> u64 {
                tokens
                    .iter()
                    .filter_map(|m| m.get(key).and_then(Value::as_u64))
                    .fold(0u64, u64::saturating_add)
            };
            // #217: the sum above blends every measurement's counts together
            // with no way back to which row contributed what, so the WORST
            // confidence across them is appended — the one fact a reader
            // needs to know before budgeting against this total. Silent when
            // every measurement is `high`: a marker on the common case would
            // train the reader to stop noticing it.
            let worst_confidence = tokens
                .iter()
                .filter_map(|m| m.get("confidence").and_then(Value::as_str))
                .min_by_key(|c| tasqx_core::tokens::confidence_rank(c));
            let confidence_suffix = match worst_confidence {
                Some(c) if c != tasqx_core::tokens::CONFIDENCE_HIGH => format!(" [{c} confidence]"),
                _ => String::new(),
            };
            row(
                "tokens",
                DetailField::Tokens,
                format!(
                    "in {} · out {} · cacheR {} · cacheW {}{confidence_suffix}",
                    sum("input_tokens"),
                    sum("output_tokens"),
                    sum("cache_read_tokens"),
                    sum("cache_creation_tokens")
                ),
            );
        }
    }
    // D138: the criteria, above the annotations and below the facts — each on
    // its own line with the state as a marker, and its evidence indented under
    // it, because a citation is only meaningful beside the claim it supports.
    if let Some(checks) = result.get("checks").and_then(Value::as_array) {
        for c in checks {
            let mark = match s(c, "state").as_str() {
                "passed" => "[x]",
                "failed" => "[!]",
                _ => "[ ]",
            };
            row(
                mark,
                DetailField::Check,
                san_multiline(c.get("body").and_then(Value::as_str).unwrap_or("")),
            );
            let evidence = s(c, "evidence");
            if !evidence.is_empty() {
                row("   ", DetailField::Check, san_multiline(&evidence));
            }
        }
    }
    if let Some(anns) = result.get("annotations").and_then(Value::as_array) {
        for a in anns {
            // #76.2: `san` neutralises a stray `\n` into a space because most
            // fields it guards (title, source, a search snippet) are meant to
            // be ONE line, and a newline in them is itself part of the hazard
            // (see `san`'s doc). An annotation body is not one of those — it
            // round-trips `\n` through storage and `task.get` intact, and
            // holds markdown a human wrote on purpose (headers, lists,
            // tables) — so it wants `san_multiline`, exactly the swap #195
            // already made for `memory show`.
            row(
                "·",
                DetailField::Annotation,
                san_multiline(a.get("body").and_then(Value::as_str).unwrap_or("")),
            );
        }
    }
    rows
}

// ============================================================================
// The task card (D76)
// ============================================================================
//
// The interactive body of `show` (the write echoes, `add`'s among them, are
// built in `echo`, D126). Gated on
// `caps.unicode` — true exactly when stdout is a VT-capable terminal — so
// every piped, dumb-terminal and legacy-console caller keeps the byte-stable
// plain rendering above, and a script diffing two runs sees what it always
// saw. Output-only formatting, so stdout alone decides; the dashboard demands
// stdin too because it reads keys, and that stricter gate must not be copied
// here. Tones come from the `card.*` roles — see `theme::builtin` for why
// they are deliberately achromatic in every built-in.

/// Which parts of a line of facts survive the width, by the one rule every
/// such line follows (`docs/maintainers/terminal-style.md` rule 9): the parts are taken
/// in rank order, lowest rank first, and the first that does not fit ends the
/// line, so nothing less important survives a part that was dropped.
/// Dropping says less; truncating mid-word would say something else. The
/// lowest-ranked part is always kept, since a line with nothing on it says
/// less than one that runs over.
///
/// `summary_line` ranks its parts in the order they print, which makes this
/// "drop from the right". `fit_facts` ranks `next`'s and `add`'s facts by what
/// the reader loses without each, which is not where each prints: the first
/// version took facts left to right, and a long project name pushed the
/// deadline off `next`'s line. A second version skipped a fact that did not
/// fit and carried on, so `next` kept the tags after dropping the project
/// (round 2 review of #346). There is one rule now, and it lives here.
fn keep_ranked(widths: &[usize], ranks: &[u8], sep: usize, budget: usize) -> Vec<bool> {
    let mut order: Vec<usize> = (0..widths.len()).collect();
    order.sort_by_key(|&i| ranks[i]);
    let mut keep = vec![false; widths.len()];
    let mut used = 0usize;
    for (n, i) in order.into_iter().enumerate() {
        let need = widths[i] + if n == 0 { 0 } else { sep };
        if n > 0 && used + need > budget {
            break;
        }
        keep[i] = true;
        used += need;
    }
    keep
}

/// A line of facts, three cells apart, fitted to `avail` cells by
/// [`keep_ranked`]. Each fact is `(rank, plain text for measuring, painted
/// text)` and is taken whole or not at all. `add`'s echo and `next` both fit
/// their facts here and rank the facts they share the same way (urgency,
/// deadline, then project and tags), which is how they cannot drift apart.
fn fit_facts(facts: Vec<(u8, String, String)>, avail: usize) -> String {
    let widths: Vec<usize> = facts.iter().map(|(_, plain, _)| width(plain)).collect();
    let ranks: Vec<u8> = facts.iter().map(|(rank, _, _)| *rank).collect();
    let keep = keep_ranked(&widths, &ranks, 3, avail);
    facts
        .into_iter()
        .zip(keep)
        .filter_map(|((_, _, painted), k)| k.then_some(painted))
        .collect::<Vec<_>>()
        .join("   ")
}

// The `show` card: the status-colored left rail (the C variant that
// superseded the D76 ledger — D78). One loud signal per card — the rail
// down the left edge, keyed by [`rail_role`] — with everything inside it
// quiet.

/// The rail's one decision: which role colors the edge. Blocked outranks
/// every status (it is the fact that stops the reader working), an
/// unrecognized status is the warning it is everywhere else, `active` gets
/// the timer's green, `pending` the theme accent, and the parked or closed
/// states recede into muted.
fn rail_role(result: &Value) -> &'static str {
    if result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "danger";
    }
    if status_is_unrecognized(result) {
        return "warn";
    }
    match s(result, "status").as_str() {
        "active" => "timer.active",
        "pending" => "accent",
        _ => "muted",
    }
}

/// Greedy word-wrap into lines of at most `max` cells, measured with
/// [`width`]. Wraps, never truncates: this feeds the card's title and
/// annotation blocks, which are the user's own words. A single word wider
/// than `max` gets its own overlong line rather than being cut — the
/// enclosing layout budgets generously enough that only pathological input
/// reaches that arm.
pub(crate) fn wrap_words(text: &str, max: usize) -> Vec<String> {
    wrap_pieces(text.split_whitespace().map(|word| (" ", word)), max)
}

/// The greedy fill behind [`wrap_words`], over pieces that each carry what
/// joins them to the piece before: a space between words, ` · ` between the
/// names in a list, nothing before a usage line's `|alternative`. The joiner
/// is dropped where a piece starts a line. Like `wrap_words` it never cuts: a
/// piece wider than `max` gets an overlong line of its own.
pub(crate) fn wrap_pieces<'a>(
    pieces: impl IntoIterator<Item = (&'a str, &'a str)>,
    max: usize,
) -> Vec<String> {
    wrap_hanging(pieces, max, max)
}

/// [`wrap_pieces`] with a first line of its own width: a synopsis whose
/// continuation lines hang further in than its first.
pub(crate) fn wrap_hanging<'a>(
    pieces: impl IntoIterator<Item = (&'a str, &'a str)>,
    first: usize,
    rest: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (joiner, piece) in pieces {
        if line.is_empty() {
            line = piece.to_string();
            continue;
        }
        let max = if lines.is_empty() { first } else { rest }.max(1);
        if width(&line) + width(joiner) + width(piece) <= max {
            line.push_str(joiner);
            line.push_str(piece);
        } else {
            lines.push(std::mem::take(&mut line));
            line = piece.to_string();
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn task_detail_card(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    // The label column: "depends_on" (the widest label) plus a two-cell gap.
    const LW: usize = 12;
    // Where the second column of a paired line starts.
    const COL2: usize = 30;
    let rail = ctx.paint(rail_role(result), "▌");
    // Everything renders inside the rail's two-cell gutter.
    let avail = ctx.cols.saturating_sub(2).max(20);
    let push = |out: &mut String, content: String| {
        out.push_str(&rail);
        if !content.is_empty() {
            out.push(' ');
            out.push_str(&content);
        }
        out.push('\n');
    };
    let lab = |name: &str| {
        format!(
            "{}{}",
            ctx.paint("card.label", name),
            " ".repeat(LW.saturating_sub(width(name)))
        )
    };
    let mut out = String::new();

    // Title block: `#N` in gray, the title strong and WRAPPED — the ledger
    // let a long title run past the terminal edge.
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let id = format!("#{sid}");
    let ind = width(&id) + 2;
    let title_w = avail.saturating_sub(ind).max(10);
    for (i, line) in wrap_words(&s(result, "title"), title_w).iter().enumerate() {
        let painted = ctx.paint("card.strong", line);
        if i == 0 {
            push(
                &mut out,
                format!("{}  {painted}", ctx.paint("card.label", &id)),
            );
        } else {
            push(&mut out, format!("{}{painted}", " ".repeat(ind)));
        }
    }
    push(&mut out, String::new());

    // The same row set the plain layout renders — the facts and their
    // conditions live in `detail_rows`, once. The card only re-maps emphasis
    // and geometry: short facts pair two to a line, long values wrap with a
    // hanging indent, annotations become their own block below.
    struct MetaCell {
        label: &'static str,
        value: String,
        role: Option<&'static str>,
        // The one pre-painted value (an unrecognized status arrives from
        // `status_cell` with its warn SGR already applied), whose width
        // cannot be measured — it is emitted alone and never wrapped.
        prepainted: bool,
        // A value painted here whose width is known from its plain form (the
        // urgency cell), so it can still pair with a neighbour.
        width: Option<usize>,
    }
    let unrecognized = status_is_unrecognized(result);
    let mut cells: Vec<MetaCell> = Vec::new();
    let mut annotations: Vec<String> = Vec::new();
    for r in detail_rows(ctx, result, now) {
        let (role, prepainted): (Option<&'static str>, bool) = match r.field {
            DetailField::Annotation => {
                annotations.push(r.value);
                continue;
            }
            // D122: the one fact that stops the reader working opens the
            // card, under the title, in the rail's own colour: `⊘ blocked by
            // #51 · <title>`. A row among the others was the same fact at the
            // same weight as `created`.
            DetailField::Blocked => {
                let glyph = if ctx.caps.unicode { "⊘ " } else { "B " };
                push(
                    &mut out,
                    format!(
                        "{}{}",
                        ctx.paint("danger", &format!("{glyph}{}", r.label)),
                        ctx.paint("danger", &format!(" {}", r.value)),
                    ),
                );
                push(&mut out, String::new());
                continue;
            }
            // D122: `list`'s urgency cell, priority letter and gauge
            // included, so a task reads the same on both screens. Painted
            // here and measured from the plain value, whose width it shares
            // once the gauge's cells are added.
            DetailField::Urgency => {
                let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
                let prio = r.value.split(' ').next().unwrap_or("-").to_string();
                let prio_role = match prio.as_str() {
                    "H" => "priority.H",
                    "M" => "priority.M",
                    "L" => "priority.L",
                    _ => "muted",
                };
                let (bar, track) = urgency_meter(urgency_scale(urg));
                let ramp = ctx.theme.ramp_style(urgency_scale(urg));
                let painted = format!(
                    "{} {}{} {}",
                    ctx.paint(prio_role, &prio),
                    ramp.paint(&bar, &ctx.caps),
                    ctx.paint("muted", &track),
                    ramp.paint(&format!("{urg:.1}"), &ctx.caps),
                );
                let plain_w = width(&format!("{prio} {bar}{track} {urg:.1}"));
                cells.push(MetaCell {
                    label: r.label,
                    value: painted,
                    role: None,
                    prepainted: false,
                    width: Some(plain_w),
                });
                continue;
            }
            DetailField::Status => (None, unrecognized),
            // An open task past its deadline reads in `overdue`, as its row
            // does in `list`: the same fact in the same colour on both.
            DetailField::Due => (
                Some(
                    if field_ts(result, "due").is_some_and(|d| overdue_at(d, now))
                        && status_is_open(&s(result, "status"))
                    {
                        "overdue"
                    } else {
                        "card.strong"
                    },
                ),
                false,
            ),
            _ => (None, false),
        };
        cells.push(MetaCell {
            label: r.label,
            value: r.value,
            role,
            prepainted,
            width: None,
        });
    }

    let paint_val = |cell: &MetaCell, text: &str| -> String {
        match cell.role {
            Some(role) => ctx.paint(role, text),
            None => text.to_string(),
        }
    };
    let value_w = |c: &MetaCell| c.width.unwrap_or_else(|| width(&c.value));
    let mut i = 0;
    while i < cells.len() {
        let a = &cells[i];
        let aw = LW + value_w(a);
        // Pair two short facts on one line when both fit their columns.
        if !a.prepainted && aw + 3 <= COL2 && i + 1 < cells.len() {
            let b = &cells[i + 1];
            if !b.prepainted && COL2 + LW + value_w(b) <= avail {
                push(
                    &mut out,
                    format!(
                        "{}{}{}{}{}",
                        lab(a.label),
                        paint_val(a, &a.value),
                        " ".repeat(COL2 - aw),
                        lab(b.label),
                        paint_val(b, &b.value)
                    ),
                );
                i += 2;
                continue;
            }
        }
        if a.prepainted || a.width.is_some() || aw <= avail {
            push(
                &mut out,
                format!("{}{}", lab(a.label), paint_val(a, &a.value)),
            );
        } else {
            // A value wider than the line wraps under itself — wrapped, not
            // truncated, because the value is the user's own text.
            let vw = avail.saturating_sub(LW).max(10);
            for (j, line) in wrap_words(&a.value, vw).iter().enumerate() {
                if j == 0 {
                    push(&mut out, format!("{}{}", lab(a.label), paint_val(a, line)));
                } else {
                    push(
                        &mut out,
                        format!("{}{}", " ".repeat(LW), paint_val(a, line)),
                    );
                }
            }
        }
        i += 1;
    }

    // Annotations: the content-rich block the ledger rendered as one
    // unwrapped line each. A blank rail line above, `·` markers, a two-cell
    // hanging indent, wrapped at the terminal width.
    if !annotations.is_empty() {
        push(&mut out, String::new());
        let aw = avail.saturating_sub(2).max(10);
        for body in &annotations {
            // #76.2: `wrap_words` reflows on `str::split_whitespace`, which
            // reads a `\n` as just another space — so a markdown annotation
            // (headers, a table, a list) came out as one run-on paragraph,
            // its line breaks gone. Each of the AUTHOR'S OWN lines is now
            // wrapped on its own; only a line too wide for the terminal still
            // gets `wrap_words`'s reflow, and a blank line (a paragraph
            // break) still prints as one.
            let mut first = true;
            for src_line in body.split('\n') {
                if src_line.is_empty() {
                    push(&mut out, String::new());
                    first = false;
                    continue;
                }
                for line in wrap_words(src_line, aw) {
                    if first {
                        push(&mut out, format!("{} {line}", ctx.paint("card.label", "·")));
                        first = false;
                    } else {
                        push(&mut out, format!("  {line}"));
                    }
                }
            }
        }
    }
    out
}

pub fn project_table(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let projects = result
        .get("projects")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if projects.is_empty() {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            "No projects.\n".to_string()
        };
    }
    // D21: `projects` is THE read surface for "where does a bare `tasqx add`
    // land?" — a fact that drove behavior while being shown nowhere. #346
    // moved the mark from a seven-cell DEFAULT column into the rail.
    struct Row {
        default: bool,
        name: String,
        archived: bool,
        desc: String,
    }
    let flag = |p: &Value, key: &str| p.get(key).and_then(Value::as_bool).unwrap_or(false);
    let rows: Vec<Row> = projects
        .iter()
        .map(|p| Row {
            default: flag(p, "default"),
            name: s(p, "name"),
            archived: flag(p, "archived"),
            desc: san(p.get("description").and_then(Value::as_str).unwrap_or("")),
        })
        .collect();
    let rail = CurrentRail::over(rows.iter().map(|r| r.default));

    // Laid out on `columns::fit` like `list` (#352). It used to be
    // `format!("{:<7}  {:<24}  {:<9}  {}")`: a project name past 24 cells
    // pushed its row out of line, and DESCRIPTION had no end at all, so on an
    // 80-column terminal every described project wrapped and took the grid
    // with it. The name gives way before it would wrap, down to a floor that
    // still tells projects apart. DESCRIPTION gives first, being the widest,
    // and is the first to go. A store where no project has one gets no
    // column for it (D51), and the same goes for STATUS, which says
    // `archived` on the rows it is true of (#346). It was an ARCHIVED column
    // that, without `--all`, could only ever say `no`, on every row.
    const MIN_PROJECT: usize = 12;
    const MAX_PROJECT: usize = 32;
    const MIN_DESC: usize = 12;
    const ARCHIVED: &str = "archived";
    let desc_w = rows.iter().map(|r| width(&r.desc)).max().unwrap_or(0);
    let name_w = rows
        .iter()
        .map(|r| width(&r.name))
        .max()
        .unwrap_or(0)
        .max(width("PROJECT"))
        .min(MAX_PROJECT);
    let status_w = if rows.iter().any(|r| r.archived) {
        width(ARCHIVED).max(width("STATUS"))
    } else {
        0
    };
    let w = columns::fit(
        &[
            Column::shrinks(name_w, MIN_PROJECT.min(name_w)),
            Column::drops(status_w, status_w),
            Column::drops(
                if desc_w == 0 {
                    0
                } else {
                    desc_w.max(width("DESCRIPTION"))
                },
                MIN_DESC,
            ),
        ],
        ctx.cols.saturating_sub(rail.width()),
    );
    let row = |cells: [(Option<&str>, &str); 3]| {
        join_cells(
            cells
                .iter()
                .zip(&w)
                .filter(|(_, w)| **w > 0)
                .map(|((role, text), w)| cell(ctx, *role, text, *w))
                .collect(),
        )
    };

    let mut out = format!(
        "{}{}",
        rail.cell(ctx, false),
        ctx.paint(
            "table.label",
            &row([(None, "PROJECT"), (None, "STATUS"), (None, "DESCRIPTION")]),
        )
    );
    out.push('\n');
    for r in &rows {
        out.push_str(&rail.cell(ctx, r.default));
        out.push_str(&row([
            (Some("project"), &r.name),
            (Some("muted"), if r.archived { ARCHIVED } else { "" }),
            (None, &r.desc),
        ]));
        out.push('\n');
    }
    out
}

/// The rail that marks the one row in a table of choices that is in effect:
/// `projects`' default and `theme list`'s active theme (#346). `*`, the way
/// `git branch` marks the branch that is checked out.
///
/// It is `docs/maintainers/terminal-style.md` rule 4 applied to a table that is not a task
/// table: two cells at the far left, never dropped by the width fit, and not
/// drawn at all when no row carries it. `projects` spent a seven-cell DEFAULT
/// column on one `*`, and `theme list` nine cells of `← active` on one row.
///
/// `*` is also the running marker in `list` without Unicode, and the dashboard
/// draws both at once: `*` for a running task in TASKS, `*` for the default
/// project in PROJECTS. They are two panels and two columns, never the same
/// column of one row, which is the collision this rail exists to prevent. An
/// earlier cut of the ruling justified the glyph with "no screen draws both",
/// which is false; `DESIGN.md` D125(d) carries the true reason.
pub(crate) struct CurrentRail {
    drawn: bool,
}

impl CurrentRail {
    /// The glyph the rail draws on the row in effect.
    pub(crate) const MARK: &'static str = "*";

    /// A rail for these rows, drawn only when one of them is in effect.
    pub(crate) fn over(current: impl IntoIterator<Item = bool>) -> Self {
        Self {
            drawn: current.into_iter().any(|c| c),
        }
    }

    /// The cells the rail takes from the row: two, or none.
    pub(crate) fn width(&self) -> usize {
        if self.drawn {
            2
        } else {
            0
        }
    }

    /// The rail's cell on one row, padding included. The padding sits outside
    /// the paint, so a row still trims cleanly.
    pub(crate) fn cell(&self, ctx: &Ctx, current: bool) -> String {
        match (self.drawn, current) {
            (false, _) => String::new(),
            (true, true) => format!("{} ", ctx.paint("accent", Self::MARK)),
            (true, false) => "  ".to_string(),
        }
    }
}

/// `"—"` (the HTML/dashboard "nothing" glyph, via `html::humanize_iso`)
/// rewritten to the terminal's own dash — matching the TOKENS column's `-`
/// for an absent value (#234 item 11), rather than mixing two "nothing"
/// glyphs on one row.
fn human_or_dash(iso: &str) -> String {
    let h = crate::html::humanize_iso(iso);
    if h == "—" {
        "-".to_string()
    } else {
        h
    }
}

/// `tasqx report` — the terminal table for `report.summary`.
///
/// `token_metrics` is `--metrics`, filtered to the request's `Vec<String>` as
/// given (validated against `SUMMARY_METRICS` before this is called): when it
/// names any of the four `tokens_*` buckets, all four render as their own
/// columns — the same four the HTML report and `--json` already carry —
/// instead of the single dominant-bucket cell (#212, D48a). `--metrics`
/// otherwise leaves this table alone: COUNT/EST/OVERDUE/TRACKED are its fixed
/// axis, and the terminal's actual gap with the other two surfaces is
/// specifically the token ranking, not those four.
/// `tasqx report --outcomes` — D137's scorecard as one fitted table.
///
/// Every rate is printed as `count/n`, never as a percentage. That is the
/// ruling's own requirement made typographic: "3/12" carries its denominator
/// in the cell, and "25%" does not — and the difference between a rework rate
/// over twelve completions and one over three is the difference between a
/// figure a maintainer should act on and one they should ignore.
///
/// Same skeleton as [`report`] above (D117/D120): the key column is the one
/// that gives when the terminal is narrow, every other column is a number and
/// a number is never cut to fit.
pub fn outcomes(ctx: &Ctx, result: &Value, group_by: &str) -> String {
    let empty = Vec::new();
    let groups = result
        .get("groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if groups.is_empty() {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            // Not "no matching tasks": the scope test here is that a task
            // CLOSED, so an open backlog matching the filter perfectly still
            // lands here, and a reader told "no matching tasks" would go
            // looking for a filter bug that is not there.
            "No closed work in scope — outcomes are measured on tasks that finished.\n".to_string()
        };
    }

    // `a/b` — the count over the denominator it was computed against. A cell
    // with nothing to divide by prints `-`, not `0/0`.
    let over = |m: &Value, key: &str| -> String {
        let n = m.get("n").and_then(Value::as_i64).unwrap_or(0);
        if n <= 0 {
            return "-".to_string();
        }
        let c = m.get(key).and_then(Value::as_i64).unwrap_or(0);
        format!("{c}/{n}")
    };

    struct Row {
        key: String,
        rework: i64,
        cells: Vec<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut labels: Vec<String> = vec!["CLOSED".into(), "DONE".into()];
    let first = &groups[0];
    let has = |m: &str| first.get(m).is_some();
    if has("rework") {
        labels.push("REWORK".into());
    }
    if has("silent") {
        labels.push("SILENT".into());
    }
    if has("calibration") {
        labels.push("CALIB".into());
    }
    if has("abandonment") {
        labels.push("DROPPED".into());
    }
    if has("overrun") {
        labels.push("OVER".into());
    }
    if has("unproven") {
        labels.push("UNPROVEN".into());
    }
    if has("cost") {
        labels.push("TOKENS".into());
    }

    for g in groups {
        let key = san(g.get(group_by).and_then(Value::as_str).unwrap_or(""));
        let closed = g.get("closed").and_then(Value::as_i64).unwrap_or(0);
        let done = g.get("completions").and_then(Value::as_i64).unwrap_or(0);
        let mut cells = vec![closed.to_string(), done.to_string()];
        let mut rework_count = 0;
        if let Some(m) = g.get("rework") {
            rework_count = m.get("count").and_then(Value::as_i64).unwrap_or(0);
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("silent") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("calibration") {
            let n = m.get("n").and_then(Value::as_i64).unwrap_or(0);
            // `×1.8 n7`: the ratio and the sample size it came from, in the
            // cell, for the same reason the rates carry their denominator. A
            // median over one completion is not a calibration.
            cells.push(match m.get("median_ratio").and_then(Value::as_f64) {
                Some(r) if n > 0 => format!("×{r:.2} n{n}"),
                _ => "-".to_string(),
            });
        }
        if let Some(m) = g.get("abandonment") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("overrun") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("unproven") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("cost") {
            // The group's dominant bucket, the D92 cell — `cost` is keyed with
            // `report.summary`'s own bucket names precisely so this helper
            // reads it unchanged, and so the reader is never shown a blend.
            let cell = crate::tokens::dominant_cell(m);
            cells.push(match m.get("confidence").and_then(Value::as_str) {
                Some(c) if c != tasqx_core::tokens::CONFIDENCE_HIGH => format!("{cell} ~{c}"),
                _ => cell,
            });
        }
        rows.push(Row {
            key,
            rework: rework_count,
            cells,
        });
    }

    const MIN_KEY: usize = 8;
    const MAX_KEY: usize = 32;
    let header_label = group_by.to_uppercase();
    let key_w = rows
        .iter()
        .map(|r| width(&r.key))
        .chain([width(&header_label)])
        .max()
        .unwrap_or(0)
        .min(MAX_KEY);
    let mut cols = vec![Column::shrinks(key_w, MIN_KEY.min(key_w))];
    for (n, label) in labels.iter().enumerate() {
        let w = rows
            .iter()
            .map(|r| width(&r.cells[n]))
            .chain([width(label)])
            .max()
            .unwrap_or(0);
        cols.push(Column::fixed(w));
    }
    let w = columns::fit(&cols, ctx.cols);

    let line = |key: String, cells: Vec<(Option<&str>, String)>| {
        let mut parts = vec![key];
        for ((role, c), cw) in cells.into_iter().zip(&w[1..]) {
            if *cw == 0 {
                continue;
            }
            let pad = " ".repeat(cw.saturating_sub(width(&c)));
            let c = match role {
                Some(r) => ctx.paint(r, &c),
                None => c,
            };
            parts.push(format!("{pad}{c}"));
        }
        join_cells(parts)
    };

    let rework_col = labels.iter().position(|l| l == "REWORK");
    let mut out = ctx.paint(
        "table.label",
        &line(
            pad(&header_label, w[0]),
            labels.iter().map(|l| (None, l.clone())).collect(),
        ),
    );
    out.push('\n');
    for r in rows {
        // REWORK is the one figure here that is a problem when it is nonzero,
        // so it takes `warn` then and `muted` otherwise — the rule the OVERDUE
        // column already follows in `report`. Every other column is neutral:
        // a high SILENT count is a habit to fix, not an alarm, and DROPPED
        // work is often the right call.
        let cells = r
            .cells
            .into_iter()
            .enumerate()
            .map(|(n, c)| {
                let role =
                    (Some(n) == rework_col).then_some(if r.rework > 0 { "warn" } else { "muted" });
                (role, c)
            })
            .collect::<Vec<_>>();
        out.push_str(&line(cell(ctx, Some("project"), &r.key, w[0]), cells));
        out.push('\n');
    }

    // The legend earns its place for the same reason D92's does: `3/12` and
    // `×1.8 n7` are compact because they are dense, and a dense cell nobody
    // can read is not compact. Set off by a blank line (rule 7).
    out.push('\n');
    out.push_str(&prose(
        ctx,
        Some("muted"),
        "REWORK / SILENT / DROPPED / OVER / UNPROVEN read count over the completions or \
         closings they were counted against — a rate with no denominator beside it is not \
         a rate. OVER counts only the completions that had a budget and UNPROVEN only those \
         that had acceptance criteria. CALIB is the median tracked-over-estimate ratio and \
         the number of completions carrying both figures.",
        "",
    ));
    out
}

pub fn report(
    ctx: &Ctx,
    result: &Value,
    group_by: &str,
    token_metrics: Option<&[String]>,
) -> String {
    let empty = Vec::new();
    let groups = result
        .get("groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if groups.is_empty() {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            "No matching tasks.\n".to_string()
        };
    }

    let show_all_tokens = token_metrics.is_some_and(|m| {
        m.iter().any(|s| {
            matches!(
                s.as_str(),
                "tokens_in" | "tokens_out" | "tokens_cache_read" | "tokens_cache_creation"
            )
        })
    });

    // Totals (#234 item 5), accumulated alongside the rows — the report
    // already holds every group before rendering, so this cannot drift from
    // what the rows above it show the way a second, independent sum could.
    let mut total_count = 0i64;
    let mut total_est_secs = 0i64;
    let mut total_tracked_secs = 0i64;
    let mut total_overdue = 0i64;
    let mut total_bucket = std::collections::HashMap::<&str, i64>::new();
    let mut any_tokens = false;
    let mut total_confidence: Option<&str> = None;

    // #217: `report.summary` names the group's WORST confidence in
    // `tokens_confidence` (D50's trust hierarchy). Silent when it is
    // `high`, or absent — a marker on the common case teaches the reader
    // to stop noticing it.
    let mark_confidence = |cell: String, confidence: Option<&str>| match confidence {
        Some(c) if c != tasqx_core::tokens::CONFIDENCE_HIGH => format!("{cell} ~{c}"),
        _ => cell,
    };
    // The token cells of one row: all four buckets, or the one that dominates.
    let token_cells = |g: &Value, confidence: Option<&str>| -> Vec<String> {
        if show_all_tokens {
            crate::tokens::BUCKETS
                .iter()
                .map(|(bkey, _, _)| {
                    crate::tokens::compact(g.get(*bkey).and_then(Value::as_i64).unwrap_or(0))
                })
                .collect()
        } else {
            vec![mark_confidence(crate::tokens::dominant_cell(g), confidence)]
        }
    };

    // Every cell is measured before any is printed, so the columns can be
    // sized to what they hold (#352). The numeric columns were a fixed 10 or
    // 12 cells whatever they held, so on a 60-column terminal the key was
    // crushed to `code-...` and the row still ran two cells past the edge.
    struct Row {
        key: String,
        overdue: i64,
        cells: Vec<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    for g in groups {
        let key = san(g.get(group_by).and_then(Value::as_str).unwrap_or(""));
        let count = g.get("count").and_then(Value::as_i64).unwrap_or(0);
        let est_iso = g.get("est_total").and_then(Value::as_str).unwrap_or("PT0S");
        let tracked_iso = g
            .get("tracked_total")
            .and_then(Value::as_str)
            .unwrap_or("PT0S");
        let overdue = g.get("overdue").and_then(Value::as_i64).unwrap_or(0);
        let confidence = g.get("tokens_confidence").and_then(Value::as_str);

        total_count += count;
        total_overdue += overdue;
        if let Some(s) = tasqx_core::util::duration_secs(est_iso) {
            total_est_secs = total_est_secs.saturating_add(s);
        }
        if let Some(s) = tasqx_core::util::duration_secs(tracked_iso) {
            total_tracked_secs = total_tracked_secs.saturating_add(s);
        }
        for (bkey, _, _) in crate::tokens::BUCKETS {
            let n = g.get(bkey).and_then(Value::as_i64).unwrap_or(0);
            if n != 0 {
                any_tokens = true;
            }
            *total_bucket.entry(bkey).or_insert(0) += n;
        }
        if let Some(c) = confidence {
            let worse = total_confidence.is_none_or(|cur| {
                tasqx_core::tokens::confidence_rank(c) < tasqx_core::tokens::confidence_rank(cur)
            });
            if worse {
                total_confidence = Some(c);
            }
        }

        let mut cells = vec![
            count.to_string(),
            human_or_dash(est_iso),
            overdue.to_string(),
            human_or_dash(tracked_iso),
        ];
        cells.extend(token_cells(g, confidence));
        rows.push(Row {
            key,
            overdue,
            cells,
        });
    }

    // The totals row — same columns, same widths, "TOTAL" where a group name
    // would be.
    let total_row = json!({
        "tokens_cache_read": total_bucket.get("tokens_cache_read").copied().unwrap_or(0),
        "tokens_cache_creation": total_bucket.get("tokens_cache_creation").copied().unwrap_or(0),
        "tokens_in": total_bucket.get("tokens_in").copied().unwrap_or(0),
        "tokens_out": total_bucket.get("tokens_out").copied().unwrap_or(0),
    });
    let mut total_cells = vec![
        total_count.to_string(),
        human_or_dash(&tasqx_core::util::iso_duration(total_est_secs)),
        total_overdue.to_string(),
        human_or_dash(&tasqx_core::util::iso_duration(total_tracked_secs)),
    ];
    total_cells.extend(token_cells(&total_row, total_confidence));

    let header_label = group_by.to_uppercase();
    let mut labels = vec![
        "COUNT".to_string(),
        "EST".to_string(),
        "OVERDUE".to_string(),
        "TRACKED".to_string(),
    ];
    if show_all_tokens {
        labels.extend(
            crate::tokens::BUCKETS
                .iter()
                .map(|(_, short, _)| short.to_uppercase()),
        );
    } else {
        labels.push("TOKENS".to_string());
    }

    // #234 item 2: the group_by column was once a hardcoded 20 cells, and a
    // name past it pushed every column after it to the right. It is sized to
    // its names, capped, and it is the one column that gives when the
    // terminal is narrow. Every other column is a number, and a number cut to
    // fit is a different number, while one dropped to fit hides a bucket the
    // reader may have asked for by name (`--metrics tokens_in`). Past the
    // key's floor the row overflows.
    const MIN_KEY: usize = 8;
    const MAX_KEY: usize = 32;
    let key_w = rows
        .iter()
        .map(|r| width(&r.key))
        .chain([width(&header_label), width("TOTAL")])
        .max()
        .unwrap_or(0)
        .min(MAX_KEY);
    let mut cols = vec![Column::shrinks(key_w, MIN_KEY.min(key_w))];
    for (n, label) in labels.iter().enumerate() {
        let w = rows
            .iter()
            .map(|r| width(&r.cells[n]))
            .chain([width(label), width(&total_cells[n])])
            .max()
            .unwrap_or(0);
        cols.push(Column::fixed(w));
    }
    let w = columns::fit(&cols, ctx.cols);

    // One line: the key cell, then every other cell right-aligned. The padding
    // goes OUTSIDE any paint, so `join_cells` can trim the end and the
    // escapes never count as width.
    let line = |key: String, cells: Vec<(Option<&str>, String)>| {
        let mut parts = vec![key];
        for ((role, c), cw) in cells.into_iter().zip(&w[1..]) {
            if *cw == 0 {
                continue;
            }
            let pad = " ".repeat(cw.saturating_sub(width(&c)));
            let c = match role {
                Some(r) => ctx.paint(r, &c),
                None => c,
            };
            parts.push(format!("{pad}{c}"));
        }
        join_cells(parts)
    };
    let plain = |cells: Vec<String>| cells.into_iter().map(|c| (None, c)).collect::<Vec<_>>();

    let mut out = ctx.paint(
        "table.label",
        &line(pad(&header_label, w[0]), plain(labels)),
    );
    out.push('\n');
    // OVERDUE is `warn` when there is any and `muted` when there is none, on
    // the TOTAL row as on every other.
    let with_overdue = |overdue: i64, cells: Vec<String>| {
        let role = if overdue > 0 { "warn" } else { "muted" };
        cells
            .into_iter()
            .enumerate()
            .map(|(n, c)| ((n == 2).then_some(role), c))
            .collect::<Vec<_>>()
    };
    for r in rows {
        let cells = with_overdue(r.overdue, r.cells);
        out.push_str(&line(cell(ctx, Some("project"), &r.key, w[0]), cells));
        out.push('\n');
    }
    // Set off by a blank line (rule 7): in `mono` and under NO_COLOR the
    // dim label alone did not tell TOTAL from a group named in capitals.
    out.push('\n');
    // A row with a label, not a title (#346, `docs/maintainers/terminal-style.md`
    // rule 12). The whole line was painted `header`, the role for `TASQX
    // MANUAL` and a task's own name, so the sums shouted over the rows they
    // sum. `TOTAL` is structure and takes the column labels' role; the
    // figures print as every other row's do.
    out.push_str(&line(
        cell(ctx, Some("table.label"), "TOTAL", w[0]),
        with_overdue(total_overdue, total_cells),
    ));
    out.push('\n');

    // Footnotes — printed only when they have something to say, set off from
    // the table by a blank line (rule 7: a note flush against the last row
    // reads as a row whose columns broke), and wrapped at words to the
    // terminal: the legend is one 171-cell sentence, which a narrow terminal
    // otherwise broke mid-word on its own (#352).
    let mut first_note = true;
    let mut footnote = |out: &mut String, text: &str| {
        if std::mem::take(&mut first_note) {
            out.push('\n');
        }
        out.push_str(&prose(ctx, Some("muted"), text, ""));
    };
    if any_tokens && !show_all_tokens {
        // #212 (D48a, challenges-design — see the commit and the report for
        // the reasoning): the cell above is one bucket of four, ranked by
        // volume rather than a priced ratio (D48a's own wording), and until
        // now the terminal gave no way to see the other three at all.
        footnote(
            &mut out,
            "TOKENS shows the largest of four buckets (cacheR/cacheW/in/out) by volume; \
             --metrics tokens_in,tokens_out,tokens_cache_read,tokens_cache_creation or --html shows all four.",
        );
    }
    let excluded = result
        .get("tokens_excluded_cancelled_tasks")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if excluded > 0 {
        // #234 item 12: D24 excludes cancelled tasks from this report by
        // default (right for counting abandoned work; silent about the spend
        // already incurred on it, which this line stops being silent about).
        footnote(
            &mut out,
            &if excluded == 1 {
                "1 cancelled task excluded; --all includes its spend.".to_string()
            } else {
                format!("{excluded} cancelled tasks excluded; --all includes their spend.")
            },
        );
    }
    out
}

/// The four buckets as `tokens.recompute` spells them in `before`/`after`,
/// in the same reading order `task_detail`'s tokens line uses. NOT
/// `crate::tokens::BUCKETS`: those are `report.summary`'s aggregate keys
/// (`tokens_in`, …) ordered by price for picking a dominant bucket, and this
/// is a per-task delta over the measurement-row keys, where the plan's own
/// example reads input-first.
const RECOMPUTE_BUCKETS: [(&str, &str); 4] = [
    ("input_tokens", "in"),
    ("output_tokens", "out"),
    ("cache_read_tokens", "cacheR"),
    ("cache_creation_tokens", "cacheW"),
];

/// `tasqx tokens recompute` — the migration delta a user reads before granting
/// `--apply` (D50 Decision 3).
///
/// One line per CHANGED task; `unchanged` tasks are counted in the totals line
/// but not listed, because the list exists to answer "who loses what" and a
/// task losing nothing would bury the ones that do. Counts are printed raw,
/// never `tokens::compact`ed: this is the one surface whose numbers someone
/// must be able to audit against `--json` before approving a deletion.
///
/// The totals line sums every task's `before`/`after` bucket maps and renders
/// them through the same [`bucket_delta`] the per-task lines use (#219) —
/// deliberately NOT the engine's blended `totals.before`/`totals.after`
/// figure, which is exactly the aggregate this module's header exists to
/// forbid and cannot even detect the failure it is meant to catch: a repair
/// that drops 200k output tokens and adds 200k cache-read tokens nets to the
/// SAME blended number on both sides and reads as a no-op, having swapped the
/// cheapest bucket for one of the most expensive.
pub fn tokens_recompute(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let dry_run = result
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if tasks.is_empty() {
        return "No log-parse measurements to recompute.\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!(
            "Token recompute ({})",
            if dry_run { "dry-run" } else { "applied" }
        ),
    ));
    out.push('\n');

    let mut unchanged = 0usize;
    for t in tasks {
        let sid = t.get("task").and_then(Value::as_i64).unwrap_or(0);
        let action = t.get("action").and_then(Value::as_str).unwrap_or("");
        let cell = |side: &str| -> &Value { t.get(side).unwrap_or(&Value::Null) };
        let line = match action {
            "unchanged" => {
                unchanged += 1;
                continue;
            }
            "recomputed" => format!(
                "recomputed   {}",
                bucket_delta(cell("before"), cell("after"))
            ),
            "downgraded" => {
                // The result carries counts, not confidences; the engine only
                // ever downgrades TO low, so the destination is safe to name
                // and the origin is not restated.
                "downgraded   confidence -> low (transcript unreadable; counts kept)".to_string()
            }
            "channel_conflict" => {
                "conflict     log-parse rows removed; the self-report is the measurement"
                    .to_string()
            }
            // A verb action this build has not heard of: show it rather than
            // silently dropping a task from a report about deletions.
            other => format!("{other}   {}", bucket_delta(cell("before"), cell("after"))),
        };
        out.push_str(&format!(
            "  {:>5}  {line}\n",
            ctx.paint("accent", &format!("#{sid}"))
        ));
    }

    out.push_str(&format!(
        "totals  {}  ·  {} task(s) in scope, {unchanged} unchanged\n",
        bucket_delta(&sum_buckets(tasks, "before"), &sum_buckets(tasks, "after")),
        tasks.len()
    ));
    if dry_run {
        out.push_str("Dry-run: nothing was written. Run `tasqx tokens recompute --apply` to perform this repair.\n");
    }
    out
}

/// Sum every task's `side` bucket map (`"before"` or `"after"`) across the
/// whole report, so the totals line carries the same four buckets the
/// per-task lines do rather than blending them into the one number #219
/// flags — a `channel_conflict` task's `null` "after" reads as zero in every
/// bucket, same as [`bucket_delta`] already treats it per task.
fn sum_buckets(tasks: &[Value], side: &str) -> Value {
    let mut totals = serde_json::json!({});
    for (key, _) in RECOMPUTE_BUCKETS {
        let sum: i64 = tasks
            .iter()
            .map(|t| {
                t.get(side)
                    .and_then(|v| v.get(key))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            })
            .fold(0i64, i64::saturating_add);
        totals[key] = serde_json::json!(sum);
    }
    totals
}

/// `in 1500->500 · out 2600->600` — only the buckets that carry a number on
/// either side; a bucket at 0->0 is noise on every line it would join. `after`
/// may be the null a `channel_conflict` reports, which reads as 0.
fn bucket_delta(before: &Value, after: &Value) -> String {
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_i64).unwrap_or(0);
    let cells: Vec<String> = RECOMPUTE_BUCKETS
        .iter()
        .filter_map(|(key, label)| {
            let (b, a) = (n(before, key), n(after, key));
            (b != 0 || a != 0).then(|| format!("{label} {b}->{a}"))
        })
        .collect();
    if cells.is_empty() {
        // All-zero rows exist only in strange stores, but a blank cell would
        // read as a rendering bug rather than an empty measurement.
        return "empty".to_string();
    }
    cells.join(" · ")
}

/// `tasqx next` picks the highest-urgency `@working` row, and `@working`
/// includes `active` — a task already running can BE that row, with nothing
/// on screen saying so. Paired with D6's single-active default (`start`
/// auto-stops whatever was running), a caller who does not already know #9 is
/// the one they started an hour ago reads this as a fresh recommendation and
/// starts timing the wrong thing.
pub fn next_task(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let Some(t) = tasks.first() else {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            "Nothing actionable — you're clear.\n".to_string()
        };
    };
    // D122: the "what now" answer says why this one, and what to type next.
    // It printed `#48  (urgency 18.1)  <title>`: no deadline (the task was two
    // days overdue), no project, and a score in parentheses that explained
    // nothing. The title comes first because it is the answer; the reasons
    // follow on `list`'s own terms (the urgency cell, the project, the
    // deadline as a calendar day); the last line is the two commands a reader
    // reaches for next, so neither has to be remembered.
    let sid = t.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    // The lines under the title start where `#N` does, measured rather than
    // counted: a hand-counted indent drew the facts one column short of it.
    let label = "next  ";
    let pad = " ".repeat(width(label) + 2);
    // Fitted to the terminal (#346): the title is cut where the width ends,
    // as `add`'s echo cuts it, and the facts go through `fit_facts`.
    let id = format!("#{sid}");
    let title = truncate(
        &s(t, "title"),
        ctx.cols.saturating_sub(width(&pad) + width(&id) + 2),
        ctx.caps.unicode,
    );
    let mut out = format!(
        "{}  {}  {}\n",
        ctx.paint("table.label", label),
        ctx.paint("card.label", &id),
        ctx.paint("card.strong", &title)
    );
    let prio = t.get("priority").and_then(Value::as_str).unwrap_or("-");
    let prio_role = match prio {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    let ramp = ctx.theme.ramp_style(urgency_scale(urg));
    // Ranked by what the reader loses without each (`fit_facts`): the
    // urgency cell and the deadline say why this task, the running timer what
    // state it is in (the command line below says so too), the project and
    // the tags only where it lives.
    let mut facts: Vec<(u8, String, String)> = vec![if ctx.caps.unicode {
        let (bar, track) = urgency_meter(urgency_scale(urg));
        (
            0,
            format!("{prio} {bar}{track} {urg:.1}"),
            format!(
                "{} {}{} {}",
                ctx.paint(prio_role, prio),
                ramp.paint(&bar, &ctx.caps),
                ctx.paint("muted", &track),
                ramp.paint(&format!("{urg:.1}"), &ctx.caps)
            ),
        )
    } else {
        (
            0,
            format!("{prio} {urg:.1}"),
            format!(
                "{} {}",
                ctx.paint(prio_role, prio),
                ramp.paint(&format!("{urg:.1}"), &ctx.caps)
            ),
        )
    }];
    let proj = s(t, "project");
    if !proj.is_empty() {
        facts.push((3, proj.clone(), ctx.paint("project", &proj)));
    }
    if let Some(due) = field_ts(t, "due") {
        let cell = format!("due {}", due_cell(due, now));
        let painted = if overdue_at(due, now) {
            ctx.paint("overdue", &cell)
        } else {
            cell.clone()
        };
        facts.push((1, cell, painted));
    }
    if s(t, "status") == "active" {
        let since = field_ts(t, "active_since").map(|at| due_cell(at, now));
        let text = match since {
            Some(when) => format!("already running, since {when}"),
            None => "already running".to_string(),
        };
        facts.push((2, text.clone(), ctx.paint("timer.active", &text)));
    }
    if let Some(tags) = t.get("tags").and_then(Value::as_array) {
        let names: Vec<String> = tags
            .iter()
            .filter_map(Value::as_str)
            .map(|g| format!("+{}", san(g)))
            .collect();
        if !names.is_empty() {
            let joined = names.join(" ");
            facts.push((4, joined.clone(), ctx.paint("tag", &joined)));
        }
    }
    out.push_str(&format!(
        "{pad}{}\n",
        fit_facts(facts, ctx.cols.saturating_sub(width(&pad)))
    ));
    let start = if s(t, "status") == "active" {
        format!("tasqx done {sid}")
    } else {
        format!("tasqx start {sid}")
    };
    out.push_str(&format!(
        "{pad}{}\n",
        ctx.paint("muted", &format!("{start}  {}  tasqx why {sid}", ctx.mid()))
    ));
    out
}

/// Urgency breakdown (`tasqx why`).
///
/// #150: reads the `urgency_breakdown` the engine returns for a `task.get
/// {explain: true}` call — the same numbers `--json` carries, one clock read
/// for both surfaces — falling back to recomputing via the D1 formula only
/// when the field is absent (a caller that fetched the task without
/// `explain`, e.g. an older daemon on the wire).
///
/// The breakdown alone answers "why is the number 18.0" and says nothing
/// about whether `next` will ever hand this task out — `@working` excludes
/// every blocked row (D53's rule), so a task can score highest here and still
/// never be offered. `task.get` already carries `blocked`/`depends_on`
/// (`show` renders both), so the one line this appends costs no extra call.
pub fn why(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    use tasqx_core::{urgency, Priority};
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);

    let from_breakdown_field = result
        .get("urgency_breakdown")
        .and_then(Value::as_object)
        .map(|b| {
            [
                ("priority", "priority"),
                ("due_proximity", "due_proximity"),
                ("age", "age"),
            ]
            .iter()
            .filter_map(|(name, key)| b.get(*key).and_then(Value::as_f64).map(|v| (*name, v)))
            .collect::<Vec<(&'static str, f64)>>()
        })
        .filter(|parts| !parts.is_empty());

    let parts = match from_breakdown_field {
        Some(parts) => parts,
        None => {
            let prio = result
                .get("priority")
                .and_then(Value::as_str)
                .and_then(Priority::parse);
            let due = result.get("due").and_then(Value::as_str);
            let created = result.get("created").and_then(Value::as_str).unwrap_or("");
            urgency::breakdown(prio, due, created)
        }
    };

    // D122: the task by name, and each term by what drove it. `why` used to
    // say `Why #48 has urgency 18.1` over `due_proximity 12.00`, which named
    // neither the task nor the fact that it was two days overdue.
    let title = s(result, "title");
    let mut out = String::new();
    if title.is_empty() {
        out.push_str(&ctx.paint("card.label", &format!("#{sid}")));
    } else {
        out.push_str(&format!(
            "{}  {}",
            ctx.paint("card.label", &format!("#{sid}")),
            ctx.paint("card.strong", &title)
        ));
    }
    out.push_str("\n\n");
    let rows: Vec<(&str, String, f64)> = parts
        .iter()
        .map(|(name, v)| match *name {
            "priority" => (
                "priority",
                result
                    .get("priority")
                    .and_then(Value::as_str)
                    .unwrap_or("none")
                    .to_string(),
                *v,
            ),
            "due_proximity" => (
                "deadline",
                match field_ts(result, "due") {
                    None => "none".to_string(),
                    Some(due) if overdue_at(due, now) => format!("overdue, {}", day_ago(due, now)),
                    Some(due) => format!("due {}", due_cell(due, now)),
                },
                *v,
            ),
            "age" => (
                "age",
                field_ts(result, "created").map_or_else(String::new, |c| {
                    let days = (now.as_second() - c.as_second()).max(0) / 86_400;
                    match days {
                        0 => "created today".to_string(),
                        1 => "1 day".to_string(),
                        n => format!("{n} days"),
                    }
                }),
                *v,
            ),
            other => (other, String::new(), *v),
        })
        .collect();
    out.push_str(&why_table(ctx, &rows));
    out.push_str(&blocked_line_for_why(ctx, result));
    out
}

/// Finding #8 (audit-2026-09): `why` explained a blocked task's urgency
/// arithmetic and never mentioned that `next` will skip it anyway — the least
/// relevant half of the answer, with the more relevant half left unsaid. Empty
/// when the task is not blocked, or its blockers are not attached to `result`
/// (an older core, or a caller that trimmed fields).
fn blocked_line_for_why(ctx: &Ctx, result: &Value) -> String {
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !blocked {
        return String::new();
    }
    let empty = Vec::new();
    let names: Vec<String> = result
        .get("unmet_blockers")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .map(|b| {
            let sid = b.get("short_id").and_then(Value::as_i64).unwrap_or(0);
            format!("#{sid} {}", s(b, "title"))
        })
        .collect();
    if names.is_empty() {
        return String::new();
    }
    format!(
        "  {} {} — `next` skips this task\n",
        ctx.paint("overdue", "blocked by"),
        names.join(", ")
    )
}

/// The breakdown as a table: each term, what drove it, and its share, then
/// the urgency. One decimal throughout, so the total is the number `list`,
/// `show` and `next` print for the same task (D122).
///
/// The parts are rounded by largest remainder so the rounded shares add up to
/// the rounded total, which is what audit finding #6 asked of this table
/// (`3.90 + 11.52` must not read as `15.4`), kept at one decimal rather than
/// bought with two. Two decimals made the total `18.09` under a heading, and
/// beside a `list`, that said `18.1`.
///
/// `parts` is a PARAMETER for the reason `chart::render_throughput`'s series
/// is: [`why`] reaches `urgency::breakdown` through the wall clock, and the
/// value that once broke this display (`-0.0`, from `(-age).max(0.0)` when
/// `created` lands in the very second the clock is read) is one a test can
/// only stage through this seam.
fn why_table(ctx: &Ctx, parts: &[(&str, String, f64)]) -> String {
    let shares = apportion(&parts.iter().map(|(_, _, v)| *v).collect::<Vec<_>>());
    let total: f64 = parts.iter().map(|(_, _, v)| v).sum();
    let desc_w = parts.iter().map(|(_, d, _)| width(d)).max().unwrap_or(0);
    let mut out = String::new();
    for ((name, desc, _), share) in parts.iter().zip(&shares) {
        out.push_str(&format!(
            "  {} {}  {:>6}\n",
            ctx.paint("table.label", &pad(name, 10)),
            pad(desc, desc_w),
            signed(*share, 1)
        ));
    }
    out.push_str(&format!(
        "  {} {}  {:>6}\n",
        ctx.paint("table.label", &pad("urgency", 10)),
        " ".repeat(desc_w),
        ctx.paint("card.strong", &format!("{:>6}", signed(total, 1)))
    ));
    out
}

/// `values` rounded to one decimal so that the rounded parts add up to the
/// rounded total: round each down to a tenth, then hand the tenths still
/// missing to the parts that lost the most in rounding. Largest remainder, the
/// method percentages that must add to 100 use.
fn apportion(values: &[f64]) -> Vec<f64> {
    let target = (values.iter().sum::<f64>() * 10.0).round() as i64;
    let mut floors: Vec<i64> = values.iter().map(|v| (v * 10.0).floor() as i64).collect();
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        let ra = values[a] * 10.0 - floors[a] as f64;
        let rb = values[b] * 10.0 - floors[b] as f64;
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    let missing = target - floors.iter().sum::<i64>();
    for &i in order.iter().cycle().take(missing.max(0) as usize) {
        floors[i] += 1;
    }
    floors.into_iter().map(|t| t as f64 / 10.0).collect()
}

/// `v` to `places` decimals, without a minus sign the printed number does not
/// earn.
///
/// IEEE 754 has two zeros, and `-0.0` compares EQUAL to `0.0` while keeping its
/// sign bit, so `{:.2}` renders it `-0.00` — which tells the reader a term
/// subtracted urgency when it contributed none. The age term reaches that value
/// honestly: it is scaled from `(-age).max(0.0)`, and a task created inside the
/// second the clock is read has `age == 0.0`, so `max` may hand back the `-0.0`
/// the negation just made.
///
/// The sign is judged AFTER rounding rather than on the value, because the two
/// disagree: `-0.004` is a genuinely negative number that still prints as a row
/// of zeros, so a `v == 0.0` test (which `-0.0` passes) would keep leaking a
/// minus for it. And it is a sign STRIP rather than `.abs()`, because a term
/// that rounds to something non-zero must keep its sign — D1's formula is free
/// to grow a negative one, and a display that quietly dropped the minus would
/// report that change wrong.
fn signed(v: f64, places: usize) -> String {
    let out = format!("{v:.places$}");
    match out.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => out,
    }
}

/// How many terminal CELLS this text occupies — the only unit a column can be
/// measured in.
///
/// A column is a grid position, and a grid is made of cells, not of `char`s.
/// The two disagree in every direction: a CJK ideograph is one char in two
/// cells, a combining mark is a char in none, and an emoji ZWJ sequence is five
/// chars forming one two-cell cluster. Every padded column in this module goes
/// through here (or through [`pad`]/[`fit`], which do) so there is one answer to
/// "how wide is this" rather than one per call site.
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Pad `s` out to at least `max` cells. Never truncates.
///
/// This is the treatment for a column whose content is DATA the reader came for
/// — a project name, a config value. Overflowing such a cell pushes the columns
/// to its right, which is ugly; silently cutting the value would be worse, so
/// this half of the pair only ever adds spaces.
pub fn pad(s: &str, max: usize) -> String {
    let mut out = s.to_string();
    // `push_str` on a run of spaces rather than `format!("{s:<max$}")`, because
    // that macro pads by char count — the exact bug this function exists to end.
    out.push_str(&" ".repeat(max.saturating_sub(width(s))));
    out
}

/// Truncate `s` to at most `max` cells, with a trailing ellipsis when it had to
/// cut. The other half of the pair [`pad`] completes — see [`cell`], which
/// applies both to a column whose budget the table's layout depends on.
///
/// The ellipsis degrades to ASCII `...` when the terminal can't render Unicode
/// (piped/dumb/legacy), so the script-safe path never leaks a stray `…` —
/// matching the rest of the glyph gating (arrow/mid/chart bars).
///
/// The cut is made by `unicode_truncate`, which walks GRAPHEME CLUSTERS: half a
/// ZWJ sequence is not a shorter emoji but a different one — or a dangling
/// joiner the terminal draws as tofu — and it would still overflow the column,
/// so a cluster is never sliced. The `pad` that follows it is not redundant:
/// cutting a budget just before a double-width glyph leaves one cell short, and
/// the spaces make up the difference.
/// `pub(crate)` for the dashboard, which cannot lean on ratatui's own clipping:
/// a silent clip loses the ellipsis that says something was cut, and on a
/// multi-column screen an over-wide string erases the neighbouring column
/// rather than only the space to its right.
pub(crate) fn truncate(s: &str, max: usize, unicode: bool) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    // `…` is one cell, `...` is three; reserve the room either way so the
    // ellipsis lands INSIDE the budget instead of blowing it by its own width.
    let ellipsis = if unicode { "…" } else { "..." };
    let (head, _) = unicode_truncate::UnicodeTruncateStr::unicode_truncate(
        s,
        max.saturating_sub(width(ellipsis)),
    );
    format!("{head}{ellipsis}")
}

/// The END of `text` in at most `cells` cells, an ellipsis standing for what
/// was cut from the FRONT: the mirror of [`truncate`], for the strings whose
/// LAST characters identify them — the part of a query the reader is typing
/// at, and a path, whose leaf says which directory this is while every
/// sibling shares its root.
pub(crate) fn tail_fit(text: &str, cells: usize, unicode: bool) -> String {
    if width(text) <= cells {
        return text.to_string();
    }
    let dots = if unicode { "…" } else { "..." };
    let keep = cells.saturating_sub(width(dots));
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let cw = width(&c.to_string());
        if used + cw > keep {
            break;
        }
        used += cw;
        tail.push(c);
    }
    tail.reverse();
    format!("{dots}{}", tail.into_iter().collect::<String>())
}

/// Gauges in `text` whose glyphs disagree with what this renderer draws at the
/// figure printed beside them, as `"<drawn> beside <figure> (renderer draws
/// <expected>)"`. One scan defines what a stale gauge is, for the guard over
/// the HTML guide's samples and the one over the manual's alike: a sample is
/// a picture of this screen, and a picture that stopped matching is a screen
/// this build cannot produce (#562).
#[cfg(test)]
pub(crate) fn gauges_disagreeing(text: &str) -> Vec<String> {
    const CELLS: [char; 4] = ['▄', '▁', '▂', '▃'];
    let chars: Vec<char> = text.chars().collect();
    let mut wrong = Vec::new();
    let mut i = 0;
    while i + 4 < chars.len() {
        let gauge: String = chars[i..i + 4].iter().collect();
        if !gauge.chars().all(|c| CELLS.contains(&c)) {
            i += 1;
            continue;
        }
        let after: String = chars[i + 4..].iter().take(12).collect();
        let figure: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(urgency) = figure.parse::<f64>() {
            let (bar, track) = urgency_meter(urgency_scale(urgency));
            let expected = format!("{bar}{track}");
            if expected != gauge {
                wrong.push(format!(
                    "{gauge} beside {figure} (renderer draws {expected})"
                ));
            }
        }
        i += 4;
    }
    wrong
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{self, Caps, Ctx};
    use crate::AGENDA_DEFAULT_DAYS;
    use serde_json::json;
    use tasqx_core::markdown::TimeFormat;

    /// The CLI and core are separately deployable — `tasqx` talks to a daemon it
    /// did not necessarily ship with — so a status string this binary cannot
    /// parse can genuinely arrive on the wire. The old `!matches!(status, "done"
    /// | "cancelled")` called such a status open purely as a side effect of not
    /// recognising it; now it is a stated rule, and this test is what stops
    /// someone "tidying" the fallback to `false`. Flip the `map_or` default and
    /// an unknown status disappears from open counts, the overdue list and the
    /// tag roll-up at once — three wrong numbers, no error.
    #[test]
    fn an_unknown_wire_status_is_treated_as_open() {
        for unknown in ["", "snoozed", "DONE", "canceled", "archived"] {
            assert!(
                status_is_open(unknown),
                "{unknown:?} should fall back to open"
            );
        }
        // ...while the statuses core actually defines still answer for themselves.
        for s in tasqx_core::types::Status::ALL {
            assert_eq!(status_is_open(s.as_str()), s.is_open(), "{s:?}");
        }
    }

    /// Task #12/D135: `docs.body` is never rewritten, so `doc_summary` reads
    /// exactly the body `memory.get` would hand back (the demo store's own
    /// `release-process` doc, verbatim) — a labelled `description` value,
    /// not the whole `key: value` line and not the fence. Shares
    /// `tasqx_core::frontmatter::split` with `tui::memory::doc_lines`
    /// instead of its own `---` scan (review finding).
    #[test]
    fn doc_summary_reads_the_bare_description_from_a_body_memory_get_would_return() {
        let body = "---\ndescription: How an SDK release is cut, tagged and announced\n---\n\
                     Cut the release branch on Monday.";
        assert_eq!(
            doc_summary(body),
            "How an SDK release is cut, tagged and announced"
        );
    }

    /// No `description` key: the summary falls back to the first line of
    /// prose AFTER the fence, not the fence's other fields.
    #[test]
    fn doc_summary_falls_back_to_the_first_prose_line_when_there_is_no_description() {
        let body = "---\nname: deploy\nauthor: infra\n---\nRoll out the change on Monday.";
        assert_eq!(doc_summary(body), "Roll out the change on Monday.");
    }

    #[test]
    fn san_strips_control_and_escape_bytes() {
        // A title carrying a screen-clear + OSC title-set + cursor move: every
        // control byte is removed (tab replaced with a space — see
        // `san_replaces_tab_with_a_space_instead_of_deleting_it` — everything
        // else dropped outright), printable text survives.
        let malicious = "\x1b[2Jpwned\x1b]0;evil\x07\x08 ok\ttab";
        let clean = san(malicious);
        assert!(!clean.contains('\x1b'), "escape byte leaked: {clean:?}");
        assert!(!clean.contains('\x07') && !clean.contains('\x08'));
        assert!(!clean.contains('\t'), "a raw tab expands in any terminal and shifts every column to its right on that row — the misalignment D51 exists to end (D19/#234 item 10)");
        assert_eq!(
            clean, "[2Jpwned]0;evil ok tab",
            "printable kept, tab replaced with a visible space (#229 item 11)"
        );
    }

    /// #234 item 10 (= bundle #228's tab-alignment item, same root cause,
    /// D19): a raw TAB in a title survives `san` and expands to the next
    /// 8-column stop in any real terminal, shifting every column to the
    /// RIGHT of it on that one row — the exact misalignment D51 exists to
    /// end. `html::esc` keeps tab deliberately (D19: "legitimate document
    /// whitespace" — a `<table>` cell has no fixed-width grid to break), so
    /// this is a `render::san`-only fix, not a second D19 sanitizer standard.
    ///
    /// Dropping the tab outright (a prior fix's behaviour) is its own bug:
    /// `"a\tb"` became `"ab"`, silently welding two words together — audit
    /// #229 item 11 caught this as a regression on the rebased tree. The
    /// Direction was explicit: map TAB (and newline, see the sibling test
    /// below) to a single visible separator space, not delete it.
    #[test]
    fn san_replaces_tab_with_a_space_instead_of_deleting_it() {
        assert_eq!(
            san("tab\there"),
            "tab here",
            "a dropped tab welds two words together (#229 item 11), a table's column boundary must stay visible"
        );
        assert_eq!(
            san("bell\x07 and \x1b]0;PWNED\x07title"),
            "bell and ]0;PWNEDtitle",
            "other control bytes are still dropped outright, not spaced"
        );
    }

    /// #229 item 11: a newline in a single-line field (title, source,
    /// snippet) was deleted by `san` rather than replaced, so `"line1\nline2"`
    /// rendered as one welded word `"line1line2"` — actively misleading in
    /// both `list` and `show`. A newline becomes a visible space instead,
    /// same treatment as tab, while `san_multiline` (paragraph text) is left
    /// untouched — it keeps real newlines on purpose.
    #[test]
    fn san_replaces_newline_with_a_space_instead_of_deleting_it() {
        assert_eq!(
            san("line1\nline2"),
            "line1 line2",
            "a deleted newline welds two lines into one misleading word (#229 item 11)"
        );
    }

    /// `san_multiline` is `san` minus the newline drop: a stored doc's
    /// paragraph breaks and list items survive, while a bare escape byte
    /// smuggled inside the same body still does not (#195).
    #[test]
    fn san_multiline_keeps_newlines_and_still_strips_escape_bytes() {
        let doc = "# Runbook\n\nDeploys go\x1bthrough\n\n- step one\n- step two\ttabbed";
        let clean = san_multiline(doc);
        assert_eq!(
            clean, "# Runbook\n\nDeploys gothrough\n\n- step one\n- step two\ttabbed",
            "newlines and tabs kept, the lone escape byte dropped"
        );
        assert!(!clean.contains('\x1b'), "escape byte leaked: {clean:?}");
        assert_eq!(
            clean.matches('\n').count(),
            doc.matches('\n').count(),
            "every newline in the source must survive: {clean:?}"
        );
    }

    #[test]
    fn task_detail_shows_remind_when_set() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let base = json!({
            "short_id": 1, "title": "probe", "status": "pending", "urgency": 8.2,
            "project": "work", "due": "2026-07-20T17:00:00Z", "remind": "-1h",
            "estimate": "PT4H"
        });
        let out = task_detail(&ctx, &base, Timestamp::now());
        assert!(out.contains("remind"), "remind row missing: {out:?}");
        assert!(out.contains("-1h"), "remind value missing: {out:?}");
        // `estimate` is settable via `est:` sugar and totalled by `report`, so the
        // detail view must show it too — an invisible field is how the dependency
        // bug stayed hidden.
        assert!(out.contains("estimate"), "estimate row missing: {out:?}");
        assert!(out.contains("4h"), "estimate value missing: {out:?}");

        // Absent remind must stay absent — the row is conditional, like `due`.
        let mut bare = base.clone();
        bare["remind"] = json!("");
        assert!(!task_detail(&ctx, &bare, Timestamp::now()).contains("remind"));
    }

    /// audit #188: `tasqx show` was strictly poorer than the MCP
    /// `tasqx_get_task` detail view for the exact same `task.get` result —
    /// `created`/`modified`/`_rev` are in `--json show`'s payload and the human
    /// renderer simply had no row for any of them, so `--expected-rev` (whose
    /// own `--help` text points at `--json show` for the value) could not be
    /// read without dropping to JSON.
    #[test]
    fn task_detail_includes_created_modified_and_rev() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let t = json!({
            "short_id": 169, "title": "raw shape probe", "status": "pending",
            "urgency": 11.5, "project": "dates",
            "created": "2026-09-09T10:38:04Z",
            "modified": "2026-09-09T11:02:00Z",
            "_rev": 3
        });
        let out = task_detail(&ctx, &t, Timestamp::now());
        assert!(
            out.lines().any(|l| l.trim_start().starts_with("created")),
            "created row missing: {out:?}"
        );
        assert!(
            out.lines().any(|l| l.trim_start().starts_with("modified")),
            "modified row missing: {out:?}"
        );
        assert!(
            out.lines()
                .any(|l| l.trim_start().starts_with("rev") && l.contains('3')),
            "rev row missing or wrong value — this is the value `--expected-rev` \
             needs: {out:?}"
        );
    }

    /// audit #143: `detail.time_format` is wired to the MCP `tasqx_get_task`
    /// renderer only (`serve.rs`'s `McpServer::with_time_format`) — `show`
    /// rendered byte-identical output under `iso`/`relative`/`both`, always the
    /// raw ISO instant and the raw ISO-8601 duration.
    #[test]
    fn task_detail_honours_configured_time_format() {
        let now: Timestamp = "2026-09-09T12:00:00Z".parse().unwrap();
        let t = json!({
            "short_id": 2, "title": "timed task", "status": "pending",
            "urgency": 11.5, "project": "apollo",
            "due": "2026-09-10T00:00:00Z", "estimate": "PT4H"
        });

        let iso_ctx =
            Ctx::new(theme::default_theme(), Caps::PLAIN).with_time_format(TimeFormat::Iso);
        let out_iso = task_detail(&iso_ctx, &t, now);
        assert!(
            out_iso.contains("2026-09-10T00:00:00Z"),
            "iso mode must keep the exact instant: {out_iso:?}"
        );
        assert!(
            out_iso.contains("PT4H"),
            "iso mode must keep the exact duration: {out_iso:?}"
        );

        let rel_ctx =
            Ctx::new(theme::default_theme(), Caps::PLAIN).with_time_format(TimeFormat::Relative);
        let out_rel = task_detail(&rel_ctx, &t, now);
        assert!(
            !out_rel.contains("2026-09-10T00:00:00Z"),
            "relative mode leaked the raw ISO instant: {out_rel:?}"
        );
        assert!(
            out_rel.contains("4h"),
            "estimate must read as a humanized duration, not PT4H: {out_rel:?}"
        );
        // D122: in the terminal, the relative forms are calendar days, rule 3
        // of docs/maintainers/terminal-style.md, not elapsed prose.
        assert!(
            out_rel.contains("tomorrow"),
            "a future due date must read as a calendar day: {out_rel:?}"
        );

        // The bug, precisely: every mode rendered the identical bytes.
        assert_ne!(
            out_iso, out_rel,
            "detail.time_format had no effect on `show`'s output"
        );
    }

    /// D132: every clock `show` prints is UTC, and the screen says so once —
    /// on the first time value that carries a clock, not on every cell. Before
    /// it, `today 17:00` read as the wall clock to anyone not on UTC.
    #[test]
    fn show_says_utc_once_on_its_first_clock() {
        let now: Timestamp = "2026-09-13T12:00:00Z".parse().unwrap();
        let t = json!({
            "short_id": 55, "title": "C", "status": "pending",
            "urgency": 8.0, "project": "work",
            "due": "2026-09-13T17:00:00Z", "scheduled": "2026-09-14T09:00:00Z",
            "created": "2026-09-01T10:00:00Z", "modified": "2026-09-13T11:00:00Z",
            "_rev": 2
        });
        for (layout, ctx) in [
            ("plain", Ctx::new(theme::default_theme(), Caps::PLAIN)),
            ("card", Ctx::new(theme::default_theme(), card_caps())),
        ] {
            let out = task_detail(&ctx, &t, now);
            assert_eq!(
                out.matches("UTC").count(),
                1,
                "{layout}: UTC must be said exactly once:\n{out}"
            );
            assert!(
                out.contains("today 17:00 UTC"),
                "{layout}: the marker is not on the first clock:\n{out}"
            );
        }
        // No clock on the screen, nothing to mark: a date is a date.
        let mut dated = t.clone();
        dated["due"] = json!("2026-09-20T00:00:00Z");
        dated["scheduled"] = json!("");
        dated["modified"] = json!("2026-09-10T11:00:00Z");
        let out = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &dated, now);
        assert!(
            !out.contains("UTC"),
            "a clockless card grew a marker:\n{out}"
        );
        // `iso` prints the stored `…Z`, which already says it.
        let iso = Ctx::new(theme::default_theme(), Caps::PLAIN).with_time_format(TimeFormat::Iso);
        let out = task_detail(&iso, &t, now);
        assert!(!out.contains("UTC"), "iso mode doubled its Z:\n{out}");
    }

    /// The capability level the card renders at, measurable: `unicode` turns
    /// the card on, `ansi: false` keeps SGR bytes out of the output so lines
    /// can be measured with [`width`] directly. `detect_from` never produces
    /// this exact combination — it is a measuring instrument, not a terminal.
    fn card_caps() -> Caps {
        Caps {
            depth: theme::ColorDepth::None,
            ansi: false,
            unicode: true,
        }
    }

    /// A task exercising every conditional row both detail layouts can draw.
    fn full_task() -> serde_json::Value {
        json!({
            "short_id": 42, "title": "Ship the release notes",
            "status": "pending", "priority": "H", "project": "work",
            "urgency": 11.4, "due": "2026-09-04T17:00:00Z", "remind": "-1h",
            "scheduled": "2026-09-01T09:00:00Z", "wait": "2026-08-30T00:00:00Z",
            "recurrence": "weekly on mon", "estimate": "PT4H",
            "completed": "2026-09-05T10:00:00Z", "tracked": "PT2H",
            "active_since": "2026-08-31T12:00:00Z", "blocked": true,
            "tags": ["docs", "release"], "depends_on": [7, 9],
            "tokens": [{"input_tokens": 10, "output_tokens": 20,
                        "cache_read_tokens": 0, "cache_creation_tokens": 5}],
            "annotations": [{"body": "called the plumber"}],
            "created": "2026-08-20T09:00:00Z", "modified": "2026-09-03T14:00:00Z",
            "_rev": 6
        })
    }

    /// D76's gate: the card belongs to VT terminals only. The plain path is
    /// what every pipe, script and executed docs example reads, so it must
    /// keep its exact old layout and never leak a frame glyph.
    #[test]
    fn the_cards_render_only_on_a_unicode_terminal() {
        let t = full_task();
        let plain = task_detail(
            &Ctx::new(theme::default_theme(), Caps::PLAIN),
            &t,
            Timestamp::now(),
        );
        assert!(
            plain.contains("  status     "),
            "the plain detail layout changed: {plain:?}"
        );
        assert!(
            !plain.contains('╭') && !plain.contains('─') && !plain.contains('▌'),
            "card glyphs leaked into the plain path: {plain:?}"
        );
        let card = task_detail(
            &Ctx::new(theme::default_theme(), card_caps()),
            &t,
            Timestamp::now(),
        );
        assert!(
            card.contains('▌') && !card.contains("  status     "),
            "unicode caps should render the rail card: {card:?}"
        );
        let added = added(
            &Ctx::new(theme::default_theme(), card_caps()),
            &t,
            Timestamp::now(),
        );
        assert!(
            added.starts_with('▌'),
            "the add card lost its rail: {added:?}"
        );
    }

    /// D122: the add card is `show`'s rail in two lines, and it fits: every line
    /// starts on the rail, no line runs past the terminal, and what the reader
    /// typed comes back in `list`'s spelling rather than the store's. Since
    /// D126 the second line's rail cell carries the task's `⊘`/`▶` (this
    /// fixture is blocked).
    #[test]
    fn the_add_card_is_two_rail_lines_in_lists_spelling() {
        let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(80);
        let now: Timestamp = "2026-09-01T12:00:00Z".parse().unwrap();
        let out = added(&ctx, &full_task(), now);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "title line and facts line:\n{out}");
        for line in &lines {
            assert!(
                line.starts_with('▌') || line.starts_with('⊘'),
                "off the rail: {line:?}"
            );
            assert!(width(line) <= 80, "{} cells at 80: {line:?}", width(line));
        }
        assert!(
            lines[1].contains("due Fri 17:00") || lines[1].contains("due Fri"),
            "{out}"
        );
        assert!(lines[1].contains("est 4h"), "{out}");
        for iso in ["PT4H", "2026-09-04T17:00:00Z", "╭", "▲"] {
            assert!(!out.contains(iso), "{iso:?} in the add card:\n{out}");
        }

        let mut long = full_task();
        long["title"] = json!("x".repeat(300));
        let out = added(&ctx, &long, now);
        assert!(
            out.lines().all(|l| width(l) <= 80),
            "a long title ran past the terminal:\n{out}"
        );
    }

    /// The rail card (the C variant that superseded the D76 ledger): every
    /// line of the detail begins with the status rail, so the card reads as
    /// one object even where a value is blank.
    #[test]
    fn the_show_card_draws_the_rail_on_every_line() {
        let ctx = Ctx::new(theme::default_theme(), card_caps());
        let out = task_detail(&ctx, &full_task(), Timestamp::now());
        for line in out.lines() {
            assert!(
                line.starts_with('▌'),
                "a line escaped the rail: {line:?}\n{out}"
            );
        }
    }

    /// The ledger never wrapped: a long title ran past the terminal edge and
    /// the rule under it stopped at 80 while the title did not. The rail card
    /// wraps title, values and annotations at the terminal width — and wraps
    /// rather than truncates, so no word of the user's own text is lost.
    #[test]
    fn the_show_card_wraps_instead_of_overflowing_or_truncating() {
        let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(60);
        let mut t = full_task();
        t["title"] = json!("word ".repeat(40).trim().to_string());
        t["annotations"] = json!([{"body": "note ".repeat(50).trim().to_string()}]);
        let out = task_detail(&ctx, &t, Timestamp::now());
        for line in out.lines() {
            assert!(
                width(line) <= 60,
                "a line overflowed the terminal: {line:?}\n{out}"
            );
        }
        assert_eq!(
            out.matches("word").count(),
            40,
            "the wrap dropped or cut part of the title:\n{out}"
        );
        assert_eq!(
            out.matches("note").count(),
            50,
            "the wrap dropped or cut part of the annotation:\n{out}"
        );
    }

    /// #76.2: a multi-line annotation (markdown headers, a list, a table)
    /// used to come out as one unbroken run — `san` turned every `\n` into a
    /// space before the card ever saw it, and the card's own wrap reflowed on
    /// whitespace besides. Both had to change: the body must round-trip its
    /// line breaks, and the card must respect them rather than re-flowing
    /// them away.
    #[test]
    fn the_show_card_keeps_annotation_line_breaks() {
        let ctx = Ctx::new(theme::default_theme(), card_caps());
        let mut t = full_task();
        t["annotations"] = json!([{"body": "# Heading\n\n- one\n- two\n\n| a | b |\n|---|---|"}]);
        let out = task_detail(&ctx, &t, Timestamp::now());
        assert!(
            out.contains("# Heading"),
            "the heading line is gone:\n{out}"
        );
        assert!(out.contains("- one"), "the first list item is gone:\n{out}");
        assert!(
            out.contains("- two"),
            "the second list item is gone:\n{out}"
        );
        assert!(out.contains("| a | b |"), "the table row is gone:\n{out}");
        // The two list items must land on SEPARATE lines, not fused by a
        // whitespace-only reflow into "- one - two".
        assert!(
            !out.contains("- one - two"),
            "the line break between list items was collapsed:\n{out}"
        );
    }

    /// Same defect, the byte-stable plain path: a stray `\n` in an annotation
    /// body must survive to stdout rather than being neutralised to a space
    /// by `san` (that treatment is right for a one-line field like a title,
    /// wrong for a body that is legitimately more than one line — #195 made
    /// the same call for `memory show`).
    #[test]
    fn the_plain_detail_keeps_annotation_line_breaks() {
        let t = json!({
            "short_id": 1, "title": "multi-line note", "status": "pending",
            "annotations": [{"body": "line one\nline two"}],
        });
        let out = task_detail(
            &Ctx::new(theme::default_theme(), Caps::PLAIN),
            &t,
            Timestamp::now(),
        );
        assert!(
            out.contains("line one\nline two"),
            "the annotation's line break did not survive to the plain layout: {out:?}"
        );
    }

    /// Short facts pair up two to a line (status beside priority), so the
    /// card spends its height on content, not on a one-fact-per-line ledger.
    #[test]
    fn the_show_card_pairs_short_facts_on_one_line() {
        let ctx = Ctx::new(theme::default_theme(), card_caps());
        let out = task_detail(
            &ctx,
            &json!({
                "short_id": 7, "title": "pairing", "status": "pending",
                "priority": "M", "project": "work", "urgency": 4.3
            }),
            Timestamp::now(),
        );
        // D122 folded priority into the urgency cell, so the short pair is now
        // status and urgency.
        let paired = out
            .lines()
            .any(|l| l.contains("status") && l.contains("urgency"));
        assert!(paired, "status and urgency did not share a line:\n{out}");
    }

    /// A blocked task from the demo store, with a running timer's worth of
    /// fields around it, as `task.get` returns it.
    fn detail_fixture() -> serde_json::Value {
        json!({
            "short_id": 52, "title": "Write the migration guide for SDK 3.0",
            "status": "pending", "priority": "M", "project": "api",
            "urgency": 12.1, "due": "2026-09-16T00:00:00Z", "estimate": "PT5H",
            "blocked": true, "depends_on": [51],
            "unmet_blockers": [{ "short_id": 51, "title": "Rate-limit the /search endpoint" }],
            "tags": ["docs"], "created": "2026-09-01T10:00:00Z",
            "modified": "2026-09-11T09:36:55.218763722Z", "_rev": 2
        })
    }

    /// D122, rule 3: a detail view spells its instants as calendar days and
    /// its durations as `5h`. Under the default `both` it printed
    /// `2026-09-11T09:36:55.218763722Z (just now)` and `PT5H (5h)`.
    #[test]
    fn show_spells_dates_as_calendar_days_and_durations_once() {
        let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
        for caps in [Caps::PLAIN, card_caps()] {
            let ctx = Ctx::new(theme::default_theme(), caps).with_time_format(TimeFormat::Both);
            let out = task_detail(&ctx, &detail_fixture(), now);
            assert!(out.contains("Wed (in 5 days)"), "{out}");
            assert!(out.contains("1 Sep (10 days ago)"), "{out}");
            for raw in ["T00:00:00Z", "T09:36", "PT5H", ".218763722"] {
                assert!(!out.contains(raw), "{raw:?} leaked into show:\n{out}");
            }
        }
    }

    /// D122, rule 11: a blocked task says what blocks it, once, by name. It
    /// was `blocked true` beside `depends_on #51`; and every task that was
    /// not blocked carried `blocked false`.
    #[test]
    fn a_blocked_task_names_its_blocker_once_and_others_say_nothing() {
        let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
        let card = task_detail(
            &Ctx::new(theme::default_theme(), card_caps()),
            &detail_fixture(),
            now,
        );
        assert!(
            card.contains("⊘ blocked by #51 · Rate-limit the /search endpoint"),
            "{card}"
        );
        for gone in ["depends_on", "true", "false"] {
            assert!(!card.contains(gone), "{gone:?} still in the card:\n{card}");
        }
        let mut free = detail_fixture();
        free["blocked"] = json!(false);
        free["unmet_blockers"] = json!([]);
        let plain = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &free, now);
        assert!(!plain.contains("blocked"), "{plain}");
        assert!(
            plain.contains("depends_on"),
            "a met dependency still shows: {plain}"
        );
    }

    /// D122: a running task says so in its status line, and priority sits in
    /// the urgency cell it modifies (rule 5). Two rows each used to say half
    /// of one fact.
    #[test]
    fn running_joins_the_status_and_priority_joins_the_urgency() {
        let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
        let mut t = detail_fixture();
        t["status"] = json!("active");
        t["active_since"] = json!("2026-09-11T09:00:00Z");
        t["blocked"] = json!(false);
        let plain = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &t, now);
        let status = plain.lines().find(|l| l.contains("status")).unwrap();
        assert!(
            status.contains("active") && status.contains("since"),
            "{plain}"
        );
        assert!(
            !plain.contains("running"),
            "a running row beside the status: {plain}"
        );
        assert!(!plain.contains("priority"), "{plain}");
        assert!(plain.contains("urgency    M 12.1"), "{plain}");
    }

    /// An open task past its deadline reads in `overdue` on the card, as its
    /// row does in `list`. The card painted it in the plain emphasis every
    /// other date got.
    #[test]
    fn an_overdue_deadline_is_painted_overdue_on_the_card() {
        let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
        let ctx = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: theme::ColorDepth::Truecolor,
                ansi: true,
                unicode: true,
            },
        );
        let mut t = detail_fixture();
        t["due"] = json!("2026-09-09T00:00:00Z");
        let out = task_detail(&ctx, &t, now);
        assert!(out.contains(&ctx.paint("overdue", "2d ago")), "{out:?}");
    }

    /// D122: `next` says why this task, and what to type. It printed
    /// `#48  (urgency 18.1)  <title>` for a task two days overdue.
    #[test]
    fn next_says_why_this_task_and_what_to_type() {
        let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = next_task(
            &ctx,
            &json!({ "tasks": [{
                "short_id": 48, "title": "Renew the TLS certificate",
                "status": "pending", "priority": "H", "urgency": 18.1,
                "project": "infra", "due": "2026-09-09T00:00:00Z", "tags": ["ops"]
            }] }),
            now,
        );
        for want in [
            "Renew the TLS certificate",
            "H 18.1",
            "infra",
            "due 2d ago",
            "+ops",
            "tasqx start 48",
            "tasqx why 48",
        ] {
            assert!(out.contains(want), "{want:?} missing from next:\n{out}");
        }
        assert!(!out.contains("(urgency"), "{out}");
        let lines: Vec<&str> = out.lines().collect();
        let col = lines[0].find("#48").unwrap();
        assert_eq!(
            lines[1].len() - lines[1].trim_start().len(),
            col,
            "the facts do not start under #48:\n{out}"
        );
    }

    /// D122: `why` names the task, says what drove each term, and ends on the
    /// number every other screen prints. It said `Why #48 has urgency 18.1`
    /// over `due_proximity 12.00` and closed on `= total 18.09`.
    #[test]
    fn why_names_the_task_explains_each_term_and_totals_the_urgency() {
        let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = why(
            &ctx,
            &json!({
                "short_id": 48, "title": "Renew the TLS certificate", "priority": "H",
                "urgency": 18.1, "due": "2026-09-09T00:00:00Z",
                "created": "2026-09-02T10:00:00Z", "blocked": false,
                "urgency_breakdown": { "priority": 6.0, "due_proximity": 12.0, "age": 0.09 }
            }),
            now,
        );
        for want in [
            "Renew the TLS certificate",
            "deadline",
            "overdue, 2d ago",
            "9 days",
            "18.1",
        ] {
            assert!(out.contains(want), "{want:?} missing from why:\n{out}");
        }
        for gone in ["due_proximity", "18.09", "Why #"] {
            assert!(!out.contains(gone), "{gone:?} still in why:\n{out}");
        }
    }

    /// The rail is the card's one loud signal, keyed to what the reader most
    /// needs to know: blocked beats everything, an unreadable status is a
    /// warning, and the open/active/closed states each carry their own tone.
    #[test]
    fn the_rail_role_is_keyed_to_status_and_blocked() {
        let t = |v: serde_json::Value| rail_role(&v);
        assert_eq!(
            t(json!({"status": "pending", "blocked": true})),
            "danger",
            "blocked outranks every status"
        );
        assert_eq!(
            t(json!({"status": "Done", "status_unrecognized": true})),
            "warn"
        );
        assert_eq!(t(json!({"status": "active"})), "timer.active");
        assert_eq!(t(json!({"status": "pending"})), "accent");
        assert_eq!(t(json!({"status": "backlog"})), "muted");
        assert_eq!(t(json!({"status": "done"})), "muted");
        assert_eq!(t(json!({"status": "cancelled"})), "muted");
    }

    /// Every conditional row toggled off one at a time, plus the value edges
    /// (PT0S, empty collections, each priority, an unrecognized status) — the
    /// fixture matrix the parity guard walks, so a condition drifting between
    /// the layouts cannot hide behind the all-fields-set case.
    fn detail_matrix() -> Vec<(String, serde_json::Value)> {
        let mut m = vec![("full".to_string(), full_task())];
        for key in [
            "project",
            "due",
            "remind",
            "scheduled",
            "wait",
            "recurrence",
            "estimate",
            "completed",
            "tracked",
            "active_since",
            "tags",
            "depends_on",
            "tokens",
            "annotations",
            "priority",
        ] {
            let mut t = full_task();
            t.as_object_mut().unwrap().remove(key);
            m.push((format!("without-{key}"), t));
        }
        let mut t = full_task();
        t["tracked"] = json!("PT0S");
        m.push(("tracked-zero".to_string(), t));
        let mut t = full_task();
        t["blocked"] = json!(false);
        m.push(("unblocked".to_string(), t));
        for p in ["M", "L"] {
            let mut t = full_task();
            t["priority"] = json!(p);
            m.push((format!("prio-{p}"), t));
        }
        let mut t = full_task();
        t["status"] = json!("Done");
        t["status_unrecognized"] = json!(true);
        m.push(("status-unrecognized".to_string(), t));
        let mut t = full_task();
        t["tags"] = json!([]);
        t["depends_on"] = json!([]);
        t["tokens"] = json!([]);
        t["annotations"] = json!([]);
        m.push(("empty-collections".to_string(), t));
        m
    }

    /// The two detail layouts must name the same facts, in the same
    /// spellings. The plain view is the contract (its labels are what docs
    /// and muscle memory know); this walks its label column and demands each
    /// one appear in the card. Rename a row in one layout and not the other
    /// and this is what goes red.
    #[test]
    fn the_detail_card_names_every_field_the_plain_view_names() {
        // Over the whole fixture matrix, not one all-fields task: with every
        // conditional toggled off in turn, a row CONDITION drifting between
        // the layouts fails here too, not only a renamed label.
        for (name, t) in detail_matrix() {
            let plain = task_detail(
                &Ctx::new(theme::default_theme(), Caps::PLAIN),
                &t,
                Timestamp::now(),
            );
            let card = task_detail(
                &Ctx::new(theme::default_theme(), card_caps()),
                &t,
                Timestamp::now(),
            );
            for line in plain.lines().skip(1) {
                let Some(first) = line.split_whitespace().next() else {
                    continue;
                };
                if first == "·" {
                    continue; // an annotation marker, not a field label
                }
                assert!(
                    card.contains(first),
                    "[{name}] the card lost the {first:?} row:\n{card}"
                );
            }
        }
        // And the full fixture still exercises the conditional rows at all.
        let plain = task_detail(
            &Ctx::new(theme::default_theme(), Caps::PLAIN),
            &full_task(),
            Timestamp::now(),
        );
        let labels = plain
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().next())
            .filter(|w| *w != "·")
            .count();
        assert!(
            labels >= 15,
            "the parity scan saw only {labels} labels — full_task stopped \
             exercising the conditional rows and this guard is checking little"
        );
    }

    /// #217: `task.get`'s `tokens` array carries `confidence` per measurement
    /// (the field MCP's markdown table already renders), but `tasqx show`
    /// summed the four buckets and printed nothing else — a task whose only
    /// measurement was a low-confidence discovery-pass guess read exactly
    /// like one a verified `otel` correlation produced. The worst confidence
    /// across the task's measurements must show on this line.
    #[test]
    fn show_marks_a_low_confidence_token_measurement() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let t = json!({
            "short_id": 109, "title": "t", "status": "pending", "urgency": 1.0,
            "tokens": [{"input_tokens": 10, "output_tokens": 0,
                        "cache_read_tokens": 0, "cache_creation_tokens": 0,
                        "confidence": "low"}],
        });
        let out = task_detail(&ctx, &t, Timestamp::now());
        let line = out
            .lines()
            .find(|l| l.contains("tokens"))
            .expect("tokens row");
        assert!(
            line.contains("low"),
            "the low confidence never reached the tokens line: {line:?}"
        );
    }

    /// A high-confidence-only task must NOT get a confidence marker at all —
    /// the whole point is that the flag distinguishes the two.
    #[test]
    fn show_does_not_mark_a_fully_high_confidence_measurement() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let t = json!({
            "short_id": 1, "title": "t", "status": "pending", "urgency": 1.0,
            "tokens": [{"input_tokens": 10, "output_tokens": 0,
                        "cache_read_tokens": 0, "cache_creation_tokens": 0,
                        "confidence": "high"}],
        });
        let out = task_detail(&ctx, &t, Timestamp::now());
        let line = out
            .lines()
            .find(|l| l.contains("tokens"))
            .expect("tokens row");
        assert!(
            !line.contains("high") && !line.contains("low") && !line.contains("medium"),
            "an unwarranted confidence marker appeared: {line:?}"
        );
    }

    /// #214: `tokens_hint` used to target machine callers only, on the theory
    /// that the CLI `done` verb had no token flags of its own — so printing
    /// the hint would recommend the impossible. `done` now HAS those flags
    /// (`--input-tokens` etc., D50/D65), so the theory no longer holds: a
    /// terminal user who never passes them is exactly the reader the hint is
    /// for, and hiding it is how the feature stayed invisible from its
    /// primary surface. D126 moved it off the card: it is one line on stderr,
    /// after the card, and it still renders exactly once.
    #[test]
    fn done_renders_the_tokens_hint_once_muted() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let hint = "no token counts were self-reported; log-parse \
            attribution is a best-effort fallback";
        let result = json!({
            "short_id": 1, "title": "t",
            "status": "done",
            "completed": "2026-07-31T10:00:00Z",
            "unblocked": [],
            "tokens_hint": hint,
        });
        let out = done(&ctx, &result, &result, &Titles::new(), Timestamp::now());
        assert!(
            out.contains("done"),
            "the completion line itself went missing: {out:?}"
        );
        assert!(
            !out.contains("self-reported"),
            "the hint is stderr's: {out:?}"
        );
        let note = tokens_note(hint, 200, true).expect("a hint the reader can act on prints");
        assert_eq!(note.lines().count(), 1, "{note:?}");
        assert_eq!(
            note.matches("no token counts were self-reported").count(),
            1,
            "the hint should render exactly once: {note:?}"
        );
    }

    /// A completion that DID self-report (or a plain response with no hint
    /// key at all) must not grow a hint line from nothing, and the variant
    /// that asks nothing of the reader prints nothing on a terminal.
    #[test]
    fn done_omits_the_tokens_hint_line_when_the_response_has_none() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "short_id": 1, "title": "t",
            "status": "done",
            "completed": "2026-07-31T10:00:00Z",
            "unblocked": [],
        });
        let out = done(&ctx, &result, &result, &Titles::new(), Timestamp::now());
        assert!(
            !out.contains("tokens_hint") && !out.contains("self-reported"),
            "a hint appeared where the response carried none: {out:?}"
        );
        assert_eq!(
            tokens_note(
                "a self-report already covers this task; nothing further",
                200,
                true
            ),
            None
        );
    }

    /// B1: `tasqx list` prints no status column, so a row the store could not
    /// read would flow through the default view looking like ordinary open work.
    /// The core flags it; the table has to say so, and name the way out — the
    /// value cannot be corrected in place, only exported, edited and imported.
    /// The overdue flag is measured against the CALLER's instant — pinned on
    /// both sides of the boundary for one stored row, which the internal
    /// clock read this replaces made unschedulable.
    #[test]
    fn the_overdue_flag_flips_at_the_callers_instant_not_the_wall_clock() {
        let t = json!({
            "short_id": 1, "urgency": 1.0, "title": "x",
            "status": "pending", "due": "2026-08-31T12:00:00Z"
        });
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        assert!(!task_row(&t, at("2026-08-31T11:59:59Z"), true).overdue);
        assert!(task_row(&t, at("2026-08-31T12:00:01Z"), true).overdue);
    }

    /// #233.1: a genuinely fresh store (never held a task) must not answer
    /// `list` with the same bare "No tasks." a filter that matched nothing
    /// gets — the two are indistinguishable dead ends otherwise, and this is
    /// every user's very first command.
    #[test]
    fn task_table_on_a_genuinely_empty_store_names_the_onboarding_commands() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let empty_store = json!({ "tasks": [], "count": 0, "total": 0, "store_empty": true });
        let out = task_table(&ctx, &empty_store, Timestamp::now());
        assert!(out.contains("tasqx init"), "{out:?}");
        assert!(out.contains("tasqx add"), "{out:?}");
        assert!(out.contains("tasqx manual"), "{out:?}");

        // A filter that matched nothing on a non-empty store keeps the plain
        // empty-result phrasing — the onboarding hint would be misleading
        // there. #229 item 1 aligned this wording with `report`'s.
        let filtered_empty = json!({ "tasks": [], "count": 0, "total": 0, "store_empty": false });
        let out2 = task_table(&ctx, &filtered_empty, Timestamp::now());
        assert_eq!(out2, "No matching tasks.\n");
    }

    /// #233.1: `next` on a genuinely fresh store must not say "you're clear"
    /// — that phrase reads as "you finished your work", which is a lie about
    /// a store that has never held any.
    #[test]
    fn next_task_on_a_genuinely_empty_store_names_the_onboarding_commands() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let empty_store = json!({ "tasks": [], "store_empty": true });
        let out = next_task(&ctx, &empty_store, Timestamp::now());
        assert!(out.contains("tasqx add"), "{out:?}");

        // A working set that is genuinely clear (real tasks exist, none is
        // actionable right now) keeps the original, true statement.
        let clear = json!({ "tasks": [], "store_empty": false });
        assert_eq!(
            next_task(&ctx, &clear, Timestamp::now()),
            "Nothing actionable — you're clear.\n"
        );
    }

    /// #233.1: `projects` on a fresh store, same treatment as `list`.
    #[test]
    fn project_table_on_a_genuinely_empty_store_names_the_onboarding_commands() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let empty_store = json!({ "projects": [], "count": 0, "store_empty": true });
        assert!(project_table(&ctx, &empty_store).contains("tasqx init"));

        let none_archived = json!({ "projects": [], "count": 0, "store_empty": false });
        assert_eq!(project_table(&ctx, &none_archived), "No projects.\n");
    }

    /// #233.1: `report` on a fresh store, same treatment as `list`.
    #[test]
    fn report_on_a_genuinely_empty_store_names_the_onboarding_commands() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let empty_store = json!({ "groups": [], "store_empty": true });
        assert!(report(&ctx, &empty_store, "project", None).contains("tasqx add"));

        let filtered_empty = json!({ "groups": [], "store_empty": false });
        assert_eq!(
            report(&ctx, &filtered_empty, "project", None),
            "No matching tasks.\n"
        );
    }

    /// #233.1: an empty agenda on a fresh store gets the same hint, appended
    /// after the footer that already states the horizon on every run.
    #[test]
    fn agenda_on_a_genuinely_empty_store_names_the_onboarding_commands() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let empty_store = json!({ "tasks": [], "store_empty": true });
        let out = agenda_text(&ctx, &agenda_select(&empty_store, 14, anchor()));
        assert!(out.contains("tasqx add"), "{out:?}");

        let filtered_empty = json!({ "tasks": [], "store_empty": false });
        let out2 = agenda_text(&ctx, &agenda_select(&filtered_empty, 14, anchor()));
        assert!(!out2.contains("tasqx add"), "{out2:?}");
    }

    /// #229 item 1: `task_table` (`list`, `watch`) said "No tasks." while
    /// `report` said "No matching tasks." for the identical situation — an
    /// empty result set from a read verb, filtered or not. One phrasing
    /// across the read verbs, matching what `report` already used.
    #[test]
    fn an_empty_task_table_matches_reports_empty_phrasing() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let text = task_table(&ctx, &json!({ "tasks": [] }), Timestamp::now());
        assert_eq!(
            text, "No matching tasks.\n",
            "list/watch's empty phrasing must match report's: {text:?}"
        );
    }

    /// Finding #4 (audit-2026-09): `tasqx list +nosuchtag` answered only "No
    /// tasks." — indistinguishable from "nothing is pending" — while D55
    /// already drew this exact distinction for `pick` (DESIGN.md:1554): "the
    /// empty-set one quotes the filter back". `task_table` itself is unchanged
    /// for every caller with no single filter string to name.
    #[test]
    fn an_empty_list_with_a_filter_quotes_it_back() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let empty = json!({ "tasks": [], "count": 0 });

        let out = task_table_filtered(&ctx, &empty, Timestamp::now(), Some("+nosuchtag"));
        assert!(
            out.contains("+nosuchtag"),
            "the filter must be quoted back: {out:?}"
        );

        // The unfiltered caller (task_table itself) is untouched.
        assert_eq!(
            task_table(&ctx, &empty, Timestamp::now()),
            "No matching tasks.\n"
        );
    }

    #[test]
    fn task_table_reports_a_status_the_store_could_not_read() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "tasks": [{
                "short_id": 7, "urgency": 5.0, "priority": "M", "title": "important work",
                "project": "work", "due": "", "tags": [],
                "status": "Done", "status_unrecognized": true
            }],
            "count": 1
        });
        let out = task_table(&ctx, &result, Timestamp::now());
        assert!(
            out.contains("Done"),
            "the offending value must be named: {out:?}"
        );
        assert!(
            out.contains("#7"),
            "the affected task must be identified: {out:?}"
        );
        assert!(out.contains("export"), "the way out must be named: {out:?}");

        // A clean table stays clean — the note is conditional, not a banner.
        let mut ok = result.clone();
        ok["tasks"][0] = json!({
            "short_id": 7, "urgency": 5.0, "priority": "M", "title": "important work",
            "project": "work", "due": "", "tags": [], "status": "pending"
        });
        assert!(
            !task_table(&ctx, &ok, Timestamp::now()).contains("export"),
            "clean table grew a warning"
        );
    }

    /// The same anomaly on the detail view, which does print status: `Done`
    /// alone reads like a status the reader simply has not heard of yet.
    #[test]
    fn task_detail_marks_a_status_the_store_could_not_read() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_detail(
            &ctx,
            &json!({
                "short_id": 7, "title": "important work", "status": "Done",
                "status_unrecognized": true, "urgency": 1.0
            }),
            Timestamp::now(),
        );
        assert!(
            out.contains("Done"),
            "the stored value must survive to the screen: {out:?}"
        );
        assert!(
            out.contains("unrecognized"),
            "the anomaly must be labelled: {out:?}"
        );
        assert!(
            out.contains("pending"),
            "the five real statuses must be named: {out:?}"
        );
    }

    /// P4d: a store written before D36 can hold a BLANK title — every door
    /// refuses one now, but the old ones did not. Such a row renders as an empty
    /// TASK cell, so it is invisible in the one view the user is looking at,
    /// and its export is a document that fails its own import (D36 refuses the
    /// blank title on the way back in). That makes `export` useless as the
    /// escape hatch D28 leans on, and the user cannot even tell which row is at
    /// fault.
    ///
    /// Detection, not repair: D28 already ruled that repair-on-open needs the
    /// correct value to be KNOWABLE, and nothing here knows what the title was
    /// meant to say. So the table names the row and the one command that fixes
    /// it. Unlike the status case, `modify` really is the way out — a title is
    /// freely settable, so there is no need to send the user through export.
    #[test]
    fn task_table_reports_a_blank_title_the_store_should_not_hold() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        for blank in ["", "   ", "	"] {
            let result = json!({
                "tasks": [{
                    "short_id": 4, "urgency": 5.0, "priority": "M", "title": blank,
                    "project": "work", "due": "", "tags": [], "status": "pending"
                }],
                "count": 1
            });
            let out = task_table(&ctx, &result, Timestamp::now());
            assert!(
                out.contains("#4"),
                "the affected task must be identified for {blank:?}: {out:?}"
            );
            assert!(
                out.contains("blank title"),
                "the anomaly must be labelled for {blank:?}: {out:?}"
            );
            assert!(
                out.contains("modify"),
                "the way out must be named for {blank:?}: {out:?}"
            );
        }

        // Conditional, not a banner: an ordinary table stays clean.
        let ok = json!({
            "tasks": [{
                "short_id": 4, "urgency": 5.0, "priority": "M", "title": "real work",
                "project": "work", "due": "", "tags": [], "status": "pending"
            }],
            "count": 1
        });
        assert!(
            !task_table(&ctx, &ok, Timestamp::now()).contains("blank title"),
            "clean table grew a warning"
        );
    }

    #[test]
    fn task_table_neutralizes_escape_in_title() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "tasks": [{
                "short_id": 1, "urgency": 5.0, "priority": "M",
                "title": "hi\x1b[31mRED\x1b[0m", "project": "p",
                "due": "", "tags": ["\x1b[5mblink"], "status": "pending"
            }],
            "count": 1
        });
        let out = task_table(&ctx, &result, Timestamp::now());
        assert!(
            !out.contains('\x1b'),
            "raw escape reached the terminal: {out:?}"
        );
    }

    /// D21: the copy must be TRUE. This line printed unconditionally, so it was
    /// a lie on every `init` after the first — the user read "now your default
    /// project" while the default had not moved (or, before the core fix, while
    /// it had been silently stolen).
    #[test]
    fn project_created_only_claims_the_default_when_it_actually_became_it() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

        let claimed = project_created(&ctx, &json!({ "name": "work", "default": true }));
        assert!(claimed.contains("work"));
        assert!(
            claimed.contains("default project"),
            "the first project really is the default; say so: {claimed:?}"
        );

        let not_claimed =
            project_created(&ctx, &json!({ "name": "prive.klussen", "default": false }));
        assert!(not_claimed.contains("prive.klussen"));
        assert!(
            !not_claimed.contains("default project"),
            "claimed the default when it did not become it: {not_claimed:?}"
        );
        // And it must point at the verb that would do it, so the user is not
        // left guessing (this is the whole complaint).
        assert!(
            not_claimed.contains("use"),
            "must name the way to switch: {not_claimed:?}"
        );
    }

    /// #229 item 13: `init " padded "` mints a project whose own printed
    /// re-selection command cannot be typed — `tasqx use  padded ` reads as
    /// `use`, a bare argument `padded`, and two stray tokens the shell drops,
    /// which is not the name the store actually holds. `project.create`'s
    /// D36 rule (`req_str_value`) is that a name's padding survives verbatim
    /// — the fix belongs in the PRINTED hint, quoted the way `filter::quote`
    /// already quotes a project name inside a composed filter, not in a
    /// trim at the write door that would fight the store-import round trip
    /// D36 exists to keep byte-identical.
    #[test]
    fn a_padded_project_names_own_re_selection_hint_is_typeable() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = project_created(
            &ctx,
            &json!({ "name": " padded ", "default": false, "current_default": "work" }),
        );
        assert!(
            out.contains("\" padded \"") || out.contains("' padded '"),
            "the printed `tasqx use` hint must quote a name a bare shell word \
             cannot carry: {out:?}"
        );
    }

    /// The default is state; a switch must show both sides of it.
    #[test]
    fn default_switched_names_the_new_default_and_the_old_one() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = default_switched(
            &ctx,
            &json!({ "name": "work", "previous": "prive.klussen" }),
        );
        assert!(out.contains("work"), "missing the new default: {out:?}");
        assert!(
            out.contains("prive.klussen"),
            "missing the previous default: {out:?}"
        );

        // First-ever switch has no previous — no dangling "was" clause.
        let fresh = default_switched(&ctx, &json!({ "name": "work", "previous": null }));
        assert!(fresh.contains("work"));
        assert!(
            !fresh.contains("was"),
            "invented a previous default: {fresh:?}"
        );
    }

    /// The same rule one verb over: archiving the default MOVES where a bare
    /// `tasqx add` lands, so the line may not be the same either way.
    ///
    /// D22 reserved this copy in writing ("when one lands it renders
    /// `default_cleared`") while `project.archive` had no CLI verb at all. The
    /// failure this pins is the cheap one: render the name, drop the field, and
    /// the user reads "Project work archived" while their default silently
    /// became nothing.
    #[test]
    fn project_archived_says_when_it_cleared_the_default() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

        let cleared = project_archived(
            &ctx,
            &json!({ "name": "work", "archived": true, "default_cleared": true }),
        );
        assert!(cleared.contains("work"), "missing the project: {cleared:?}");
        assert!(
            cleared.contains("default project") && !cleared.contains("unchanged"),
            "the default moved and the line does not say so: {cleared:?}"
        );
        // And it must name the way back, exactly as `project_created` does when
        // it declines to claim the default: a store with no default is valid,
        // being unable to leave that state is not.
        assert!(
            cleared.contains("tasqx use"),
            "must name the verb that re-points the default: {cleared:?}"
        );

        let kept = project_archived(
            &ctx,
            &json!({ "name": "side", "archived": true, "default_cleared": false }),
        );
        assert!(kept.contains("side"));
        assert!(
            !kept.contains("no home"),
            "invented a default change that did not happen: {kept:?}"
        );
        // The two outcomes must be DISTINGUISHABLE, not merely different in
        // what they omit — "Project side archived" on its own is also the line
        // a cleared default would print if the field were dropped.
        assert_ne!(
            kept.replace("side", "work"),
            cleared,
            "the cleared and untouched cases print the same line"
        );
        assert!(
            kept.contains("unchanged"),
            "the untouched case must say the default is untouched: {kept:?}"
        );
    }

    /// D89/#161: archiving a project with open work said NOTHING about it —
    /// "Project acme archived  ·  your default project is unchanged" was the
    /// whole line for a project holding an overdue, high-priority task, and
    /// the two tasks stayed fully visible in `list`, `agenda` and `report`
    /// while `tasqx projects` (without `--all`) stopped mentioning acme at
    /// all. The one moment a user is thinking about this project is the one
    /// line that must say what got left behind.
    #[test]
    fn project_archived_says_what_open_work_it_leaves_behind() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

        let with_overdue = project_archived(
            &ctx,
            &json!({
                "name": "acme", "archived": true, "default_cleared": false,
                "open_tasks": 2, "open_overdue": 1,
            }),
        );
        assert!(
            with_overdue.contains("2 open tasks"),
            "must count the tasks left behind: {with_overdue:?}"
        );
        assert!(
            with_overdue.contains("1 overdue"),
            "must call out how many of them are overdue: {with_overdue:?}"
        );
        assert!(
            with_overdue.contains("tasqx list project:acme"),
            "must name how to find them: {with_overdue:?}"
        );
        // The default-project fact from D22 must survive alongside the new one.
        assert!(
            with_overdue.contains("unchanged"),
            "must not drop the pre-existing default-project fact: {with_overdue:?}"
        );

        // No overdue among the open tasks: no false "(0 overdue)".
        let no_overdue = project_archived(
            &ctx,
            &json!({
                "name": "acme", "archived": true, "default_cleared": false,
                "open_tasks": 1, "open_overdue": 0,
            }),
        );
        assert!(no_overdue.contains("1 open task"));
        assert!(!no_overdue.contains("open task,"));
        assert!(
            !no_overdue.contains("overdue"),
            "zero overdue must not be printed as a fact: {no_overdue:?}"
        );
        // The number must agree with its noun: "1 open tasks" is not English.
        // Assert the exact clause in both numbers, not a substring a mismatch
        // would still satisfy. (D126 spells it `left in it`, which has no verb
        // to disagree; it was `remain(s)`.)
        assert!(
            no_overdue.contains("1 open task left in it"),
            "singular: {no_overdue:?}"
        );
        assert!(
            with_overdue.contains("2 open tasks left in it, 1 overdue"),
            "plural: {with_overdue:?}"
        );

        // Nothing left behind: the line is exactly what it was before D89,
        // unchanged — no empty "0 open tasks" clause invented.
        let clean = project_archived(
            &ctx,
            &json!({
                "name": "acme", "archived": true, "default_cleared": false,
                "open_tasks": 0, "open_overdue": 0,
            }),
        );
        assert!(
            !clean.contains("open task"),
            "nothing was left behind, so nothing should be claimed: {clean:?}"
        );
        assert!(clean.contains("unchanged"));
    }

    /// The invisible-field trap: `projects` is the read surface for the default,
    /// so the table must mark it.
    #[test]
    fn project_table_marks_the_default_project() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = project_table(
            &ctx,
            &json!({
                "count": 2,
                "projects": [
                    { "name": "prive.klussen", "archived": false, "default": false, "description": "" },
                    { "name": "work", "archived": false, "default": true, "description": "" },
                ]
            }),
        );
        let work_line = out.lines().find(|l| l.contains("work")).expect("work row");
        let other_line = out
            .lines()
            .find(|l| l.contains("prive.klussen"))
            .expect("other row");
        assert!(
            work_line.contains('*'),
            "the default row is unmarked: {work_line:?}"
        );
        assert!(
            !other_line.contains('*'),
            "a non-default row is marked: {other_line:?}"
        );
    }

    /// #346: the default is marked in a two-cell rail, the way `git branch`
    /// marks the checked-out branch, and archived is a word on the rows it is
    /// true of (`docs/maintainers/terminal-style.md` rules 2 and 4).
    ///
    /// The table spent a seven-cell DEFAULT column on one `*`, and an
    /// eight-cell ARCHIVED column that, without `--all`, could only ever say
    /// `no` on every row.
    #[test]
    fn project_table_spends_a_rail_on_the_default_and_a_word_on_archived() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let row = |name: &str, default: bool, archived: bool| json!({ "name": name, "archived": archived, "default": default, "description": "" });
        let out = project_table(
            &ctx,
            &json!({ "projects": [row("home", false, false), row("work", true, false)] }),
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "  PROJECT", "{out}");
        assert_eq!(lines[1], "  home", "{out}");
        assert_eq!(lines[2], "* work", "{out}");

        let out = project_table(
            &ctx,
            &json!({ "projects": [row("old", false, true), row("work", true, false)] }),
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "  PROJECT  STATUS", "{out}");
        assert_eq!(lines[1], "  old      archived", "{out}");
        assert_eq!(lines[2], "* work", "{out}");
        assert!(!out.contains(" no"), "{out}");
    }

    /// #346: the TOTAL row is a row with a label, not a title. It was painted
    /// in `header`, the role for `TASQX MANUAL` and a task's own name, so the
    /// sums competed with the rows they sum (`docs/maintainers/terminal-style.md`
    /// rule 12). The label takes `table.label` like the column labels, and the
    /// figures print at the terminal's own foreground.
    #[test]
    fn report_total_row_is_labelled_not_titled() {
        let ctx = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: theme::ColorDepth::Truecolor,
                ansi: true,
                unicode: true,
            },
        );
        let groups = json!({ "groups": [
            { "project": "a", "count": 3, "est_total": "PT1H", "overdue": 1, "tracked_total": "PT0S" },
            { "project": "b", "count": 5, "est_total": "PT2H", "overdue": 0, "tracked_total": "PT0S" },
        ] });
        let out = report(&ctx, &groups, "project", None);
        let total = out.lines().find(|l| l.contains("TOTAL")).expect("TOTAL");
        let header_sgr = ctx.paint("header", "\u{0}");
        let header_sgr = header_sgr.split('\u{0}').next().expect("an SGR prefix");
        assert!(!header_sgr.is_empty(), "the test ctx paints nothing");
        assert!(
            !total.contains(header_sgr),
            "TOTAL is still painted as a title: {total:?}"
        );
        assert!(
            total.contains(&ctx.paint("table.label", "TOTAL")),
            "the TOTAL label is not a table label: {total:?}"
        );
        assert!(total.contains(" 8 "), "the count total: {total:?}");
        // Separated by whitespace, not by weight alone (rule 7): in `mono`
        // and under NO_COLOR a dim label is all that told TOTAL from a group
        // named in capitals.
        let lines: Vec<&str> = out.lines().collect();
        let at = lines.iter().position(|l| l.contains("TOTAL")).unwrap();
        assert_eq!(lines[at - 1], "", "TOTAL runs flush under the rows: {out}");
    }

    /// #346: prose after the table gets a blank line (rule 7), and counts are
    /// spelled `1 cancelled task` / `2 cancelled tasks`, never `task(s)`
    /// (rule 8).
    #[test]
    fn report_footnotes_stand_off_the_table_and_count_in_words() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let groups = |n: i64| {
            json!({ "tokens_excluded_cancelled_tasks": n, "groups": [
                { "project": "a", "count": 3, "est_total": "PT1H", "overdue": 0, "tracked_total": "PT0S" },
            ] })
        };
        let out = report(&ctx, &groups(1), "project", None);
        let lines: Vec<&str> = out.lines().collect();
        let total = lines.iter().position(|l| l.contains("TOTAL")).unwrap();
        assert_eq!(lines[total + 1], "", "no air between table and note: {out}");
        assert!(
            lines[total + 2].starts_with("1 cancelled task excluded"),
            "{out}"
        );
        let out = report(&ctx, &groups(2), "project", None);
        assert!(out.contains("2 cancelled tasks excluded"), "{out}");
        assert!(!out.contains("task(s)"), "{out}");
    }

    /// D138: the state is the marker, and the evidence sits under the claim it
    /// supports.
    #[test]
    fn a_check_renders_with_its_state_as_a_marker_and_its_evidence_under_it() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let t = json!({
            "short_id": 1, "title": "t", "status": "pending",
            "checks": [
                { "body": "passed one", "state": "passed", "evidence": "the proof" },
                { "body": "failed one", "state": "failed", "evidence": null },
                { "body": "open one", "state": "open", "evidence": null },
            ],
        });
        let out = task_detail(&ctx, &t, Timestamp::now());
        assert!(out.contains("[x] passed one"), "{out}");
        assert!(out.contains("[!] failed one"), "{out}");
        assert!(out.contains("[ ] open one"), "{out}");
        let lines: Vec<&str> = out.lines().collect();
        let at = lines.iter().position(|l| l.contains("passed one")).unwrap();
        assert!(
            lines[at + 1].contains("the proof"),
            "the citation sits under its claim: {out}"
        );
    }

    /// A task with no criteria gets no marker lines at all — the same rule the
    /// budget row follows.
    #[test]
    fn a_task_without_criteria_renders_no_check_lines() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let t = json!({ "short_id": 1, "title": "t", "status": "pending", "checks": [] });
        let out = task_detail(&ctx, &t, Timestamp::now());
        for marker in ["[x]", "[!]", "[ ]"] {
            assert!(
                !out.contains(marker),
                "{marker} printed over nothing: {out}"
            );
        }
    }

    /// D139: the pair renders as one row, and only when a threshold was set —
    /// `fresh_tokens` alone is a number with nothing to read it against, and
    /// the TOKENS row already reports the spend.
    #[test]
    fn a_budget_renders_as_one_row_and_only_when_it_exists() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let with = json!({
            "short_id": 1, "title": "t", "status": "pending",
            "budget_tokens": 1000, "fresh_tokens": 1300, "over": true,
        });
        let out = task_detail(&ctx, &with, Timestamp::now());
        assert!(out.contains("1.3K / 1.0K fresh"), "{out}");
        assert!(out.contains("over"), "the verdict is on the row: {out}");

        let without = json!({
            "short_id": 1, "title": "t", "status": "pending",
            "budget_tokens": null, "fresh_tokens": 1300, "over": null,
        });
        let out = task_detail(&ctx, &without, Timestamp::now());
        assert!(
            !out.contains("budget"),
            "no threshold, no row — 1300 against nothing says nothing: {out}"
        );
    }

    /// The gauge must not be readable as a bill: cache reads are excluded from
    /// it, so the row it prints and the TOKENS row are different numbers on
    /// purpose and both have to be legible at once.
    #[test]
    fn the_budget_row_and_the_token_row_coexist() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let t = json!({
            "short_id": 1, "title": "t", "status": "pending",
            "budget_tokens": 1000, "fresh_tokens": 1300, "over": true,
            "tokens": [{
                "input_tokens": 900, "output_tokens": 400,
                "cache_read_tokens": 500_000, "cache_creation_tokens": 0,
                "confidence": "medium", "tool": "claude-code", "source": "self-report"
            }],
        });
        let out = task_detail(&ctx, &t, Timestamp::now());
        assert!(out.contains("1.3K / 1.0K fresh"), "the gauge: {out}");
        assert!(
            out.contains("cacheR 500.0K") || out.contains("cacheR 500000"),
            "and the spend, undiminished by the gauge's definition: {out}"
        );
    }

    /// The brief's reason for existing, on the screen: the prerequisite's own
    /// last word, under the prerequisite.
    #[test]
    fn a_brief_prints_what_each_prerequisite_concluded() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "task": { "short_id": 2, "title": "Audit the retry path", "status": "pending" },
            "neighbourhood": {
                "depends_on": [{
                    "short_id": 1, "title": "Freeze the envelope", "status": "done",
                    "annotation": { "body": "the key is the request id, never the order id",
                                    "created": "2026-09-01T00:00:00Z" }
                }],
                "blocks": [{ "short_id": 3, "title": "Ship the notes", "status": "pending" }],
            },
            "memory": { "hits": [
                { "title": "Idempotency", "source": "docs/adr/014.md",
                  "snippet": "retries must not double-charge" }
            ], "total": 1 },
        });
        let out = task_brief(&ctx, &result, Timestamp::now());
        assert!(out.contains("DEPENDS ON"), "{out}");
        assert!(
            out.contains("the key is the request id, never the order id"),
            "the prerequisite's conclusion is the row this section exists for: {out}"
        );
        assert!(
            out.contains("BLOCKS") && out.contains("Ship the notes"),
            "{out}"
        );
        assert!(
            out.contains("FROM MEMORY") && out.contains("Idempotency"),
            "{out}"
        );
    }

    /// An empty section is omitted, not printed with nothing under it: a brief
    /// is read before work starts and a heading that says nothing costs
    /// attention at the worst moment (house style rule 12).
    #[test]
    fn a_brief_with_no_neighbours_prints_no_empty_headings() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "task": { "short_id": 1, "title": "alone", "status": "pending" },
            "neighbourhood": { "depends_on": [], "blocks": [] },
            "memory": { "hits": [], "total": 0 },
        });
        let out = task_brief(&ctx, &result, Timestamp::now());
        for heading in ["DEPENDS ON", "BLOCKS", "FROM MEMORY"] {
            assert!(
                !out.contains(heading),
                "{heading} printed over nothing: {out}"
            );
        }
    }

    /// D70's rule one surface over: a bounded page that does not say it is
    /// bounded is read as the whole answer.
    #[test]
    fn a_brief_says_when_it_withheld_memory_hits() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "task": { "short_id": 1, "title": "t", "status": "pending" },
            "neighbourhood": { "depends_on": [], "blocks": [] },
            "memory": { "hits": [{ "title": "one", "snippet": "s" }], "total": 9 },
        });
        let out = task_brief(&ctx, &result, Timestamp::now());
        assert!(out.contains("1 of 9 matches shown"), "{out}");
    }

    /// D137's denominator rule, made typographic. `3/12` and `3/3` are the same
    /// percentage and very different news, so the cell carries the `n` it was
    /// computed against and no cell anywhere prints a `%`.
    #[test]
    fn outcome_rates_print_over_their_denominator_and_never_as_a_percentage() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({ "groups": [{
            "project": "work",
            "closed": 4, "completions": 3,
            "rework": { "count": 1, "n": 3, "rate": 0.333, "refs": [2] },
            "silent": { "count": 2, "n": 3, "rate": 0.667, "refs": [2, 3] },
            "calibration": { "median_ratio": 1.5, "n": 2 },
            "abandonment": { "count": 1, "n": 4, "rate": 0.25, "tracked_total": "PT20M", "refs": [4] },
            "cost": { "tokens_in": 1, "tokens_out": 2, "tokens_cache_read": 900, "tokens_cache_creation": 3, "n": 1 },
        }] });
        let out = outcomes(&ctx, &result, "project");
        assert!(out.contains("1/3"), "rework reads count over n: {out}");
        assert!(out.contains("2/3"), "silent reads count over n: {out}");
        assert!(out.contains("1/4"), "abandonment reads count over n: {out}");
        assert!(
            out.contains("×1.50 n2"),
            "calibration carries its sample size: {out}"
        );
        assert!(
            !out.contains('%'),
            "a percentage drops the denominator D137 requires: {out}"
        );
        // D48/D50/D103: the cell names one bucket, never a blend.
        assert!(out.contains("cacheR"), "{out}");
    }

    /// A rate with nothing to divide by prints `-`, not `0/0`: "none of them"
    /// and "there were none" are different answers.
    #[test]
    fn an_outcome_rate_with_no_denominator_prints_a_dash() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({ "groups": [{
            "project": "work",
            "closed": 1, "completions": 0,
            "rework": { "count": 0, "n": 0, "rate": null, "refs": [] },
            "calibration": { "median_ratio": null, "n": 0 },
        }] });
        let out = outcomes(&ctx, &result, "project");
        assert!(!out.contains("0/0"), "{out}");
        assert!(out.contains('-'), "{out}");
    }

    /// An empty outcomes report may not say "no matching tasks": the scope test
    /// is that a task CLOSED, so a perfectly matching open backlog lands here
    /// and that wording would send the reader hunting a filter bug.
    #[test]
    fn an_empty_outcomes_report_says_the_scope_is_closed_work() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = outcomes(
            &ctx,
            &json!({ "groups": [], "store_empty": false }),
            "project",
        );
        assert!(out.contains("No closed work in scope"), "{out}");
        assert!(!out.contains("No matching tasks"), "{out}");
    }

    /// #346: the prose `next`, `agenda`, `projects` and `report` print wraps at
    /// words to the terminal. The onboarding hint was one 100-cell line and the
    /// agenda's undated note 95, so a 60-column terminal broke both mid-word.
    #[test]
    fn prose_under_a_screen_wraps_at_the_terminal_width() {
        use unicode_width::UnicodeWidthStr;
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
        let empty = json!({ "tasks": [], "projects": [], "groups": [], "store_empty": true });
        let payload = agenda_payload(vec![
            dated(1, "on a day", "2026-08-05T00:00:00Z", ""),
            dated(2, "no dates at all", "", ""),
            dated(3, "far out", "2026-12-24T00:00:00Z", ""),
        ]);
        let mut over = Vec::new();
        for (name, out) in [
            ("next", next_task(&ctx, &empty, anchor())),
            ("projects", project_table(&ctx, &empty)),
            ("report", report(&ctx, &empty, "project", None)),
            ("agenda", agenda_text(&ctx, &agenda_of(&payload, 14))),
        ] {
            assert!(!out.trim().is_empty(), "{name} printed nothing");
            for l in out.lines().filter(|l| l.width() > 60) {
                over.push(format!("{name} ({}): {l}", l.width()));
            }
        }
        assert!(over.is_empty(), "wider than 60:\n{}", over.join("\n"));
    }

    /// #346: a command quoted in a note is not broken across two lines. The
    /// first wrap of the agenda's undated note at 80 columns ended one line on
    /// `` `tasqx `` and began the next on `` list` shows them ``, which is a
    /// command a reader cannot paste.
    #[test]
    fn prose_never_breaks_a_quoted_command() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(80);
        let note = "7 undated — no due or scheduled date, so nothing puts them on a day; \
                    `tasqx list` shows them";
        let out = prose(&ctx, None, note, "");
        assert!(out.lines().count() > 1, "the fixture must wrap: {out:?}");
        for l in out.lines() {
            assert_eq!(
                l.matches('`').count() % 2,
                0,
                "a quoted command was split: {out:?}"
            );
        }
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>().join(" "),
            note,
            "the words changed: {out:?}"
        );
    }

    /// #346: `next` fits the terminal. Its title is cut to the width, and its
    /// facts are taken whole or not at all, by the same rule `add`'s echo
    /// follows. Which facts survive goes by what the reader loses without
    /// them, not by where they sit on the line: the urgency cell and the
    /// deadline say why this task, the project only where. The first cut took
    /// facts left to right and stopped at the first that did not fit, so a
    /// long project name pushed the deadline off the line (review of #346).
    #[test]
    fn next_fits_a_narrow_terminal_and_keeps_the_facts_that_say_why() {
        use unicode_width::UnicodeWidthStr;
        let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(60);
        let mut t = task_json(
            48,
            "Renew the TLS certificate for api.example.dev before it lapses on the weekend",
            "infrastructure-platform-team",
            "2026-08-01T00:00:00Z",
            &["ops", "security", "certificates"],
        );
        t["priority"] = json!("H");
        t["urgency"] = json!(18.1);
        t["status"] = json!("active");
        let out = next_task(&ctx, &json!({ "tasks": [t] }), anchor());
        for l in out.lines() {
            assert!(l.width() <= 60, "{} cells at 60: {l:?}\n{out}", l.width());
        }
        assert!(
            out.lines().next().is_some_and(|l| l.contains("#48  Renew")),
            "{out}"
        );
        let facts = out.lines().nth(1).expect("a facts line");
        assert!(facts.contains("18.1"), "{out}");
        assert!(facts.contains("due "), "the deadline gave way: {out}");
        assert!(
            !facts.contains("infrastructure"),
            "a fact was cut rather than dropped, or kept over the deadline: {facts:?}"
        );
        // The running state is still on screen when its fact is not: the
        // command line offers `done`, not `start`.
        assert!(out.contains("tasqx done 48"), "{out}");
    }

    /// #346 review: a memory title gives way only once the columns beside it
    /// have gone. Widest-first shrinking cut `Cut build time under five
    /// minutes` to 22 cells at 100 columns while a 20-cell match fragment and
    /// the source printed beside it, and `memory list` cut titles to 13 cells
    /// at 60 to keep PROJECT. The title's floor is now what the terminal can
    /// give it beside the id, in both tables.
    #[test]
    fn a_memory_title_gives_way_only_after_the_columns_beside_it() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(80);
        let hit = |title: &str, source: &str| {
            json!({ "id": "01a0903d-243b-7842-8f5c-184005a2d8f2", "kind": "doc",
                    "title": title, "source": source,
                    "snippet": "Blocked on the release of the new runner image; revisit after the infra release train" })
        };
        let out = memory_hits(
            &ctx,
            &json!({ "count": 2, "total": 2, "hits": [
                hit("Cut build time under five minutes", "docs/ci/build-time.md"),
                hit("release-process", "docs/release.md"),
            ] }),
            "release",
            false,
        );
        assert!(
            out.contains("Cut build time under five minutes"),
            "the title was cut while other columns kept room: {out}"
        );
        assert!(!out.contains("docs/ci/build-time.md"), "{out}");

        let ctx = ctx.with_cols(60);
        let doc = |title: &str| {
            json!({ "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "title": title,
                    "project": "website", "modified": "2026-08-01T00:00:00Z" })
        };
        let out = memory_table(
            &ctx,
            &json!({ "total": 2, "docs": [doc("pricing-page-decisions"), doc("dentist")] }),
            anchor(),
        );
        assert!(
            out.contains("pricing-page-decisions"),
            "the title was cut to keep PROJECT: {out}"
        );
        assert!(!out.contains("PROJECT"), "{out}");
    }

    /// Round 1 review of #346: a title never gives way below twelve cells,
    /// the floor at which it stops telling one doc from another; past it the
    /// row overflows, as every table's does (D120). The title floor had no
    /// lower bound, so at 40 columns `memory list` printed `...` on every row
    /// and `memory search` printed `r…`, and in ASCII the row still wrapped.
    #[test]
    fn a_memory_title_keeps_twelve_cells_on_a_narrow_terminal() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(40);
        let doc = |title: &str| {
            json!({ "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "title": title,
                    "project": "website", "modified": "2026-08-01T00:00:00Z" })
        };
        let list = memory_table(
            &ctx,
            &json!({ "total": 1, "docs": [doc("android-token-refresh")] }),
            anchor(),
        );
        assert!(list.contains("android-t"), "{list}");
        let hits = memory_hits(
            &ctx,
            &json!({ "count": 1, "total": 1, "hits": [
                { "id": "01a0903c-bff0-76a2-9bcb-5428786a56c4", "kind": "doc",
                  "title": "release-process", "source": "docs/release.md",
                  "snippet": "How an SDK release is cut" } ] }),
            "release",
            false,
        );
        assert!(hits.contains("release-p"), "{hits}");
        // And the handle, the one thing a search record never gives up
        // (D125(a)), is still on the line: the row overflows instead.
        assert!(
            hits.contains("01a0903c-bff0-76a2-9bcb-5428786a56c4"),
            "the handle was dropped to fit: {hits}"
        );
    }

    /// D125: the summary names the expression that ran, which a plain query
    /// quotes, so it stands apart from the count even where dim does not
    /// reach the screen (`mono`, NO_COLOR). It printed the query bare, and
    /// `the   5 hits · 2 shown` read as a sentence.
    #[test]
    fn a_search_summary_sets_the_expression_off() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = memory_hits(
            &ctx,
            &json!({ "count": 0, "total": 0, "hits": [], "matched": "\"the\"" }),
            "the",
            false,
        );
        assert!(out.starts_with("\"the\"   0 hits"), "{out}");
    }

    /// Round 1 review of #346: the `*` rail is not drawn when no row is in
    /// effect, so a table with no default spends no cells on it (rule 4).
    #[test]
    fn the_current_rail_is_not_drawn_when_nothing_is_in_effect() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = project_table(
            &ctx,
            &json!({ "projects": [
                { "name": "home", "archived": false, "default": false, "description": "" },
            ] }),
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines, ["PROJECT", "home"], "{out}");
    }

    /// #346 review: a stray backtick does not turn the rest of a note into one
    /// word. The unrecognized-status note embeds raw store text and the miss
    /// hint the query as typed, so either can carry one.
    #[test]
    fn prose_holds_only_paired_backticks() {
        use unicode_width::UnicodeWidthStr;
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
        let note = "every term was required: \"it`s\" and then enough further words \
                    that the note cannot fit on one sixty-column line";
        let out = prose(&ctx, None, note, "");
        for l in out.lines() {
            assert!(l.width() <= 60, "{} cells: {l:?}\n{out}", l.width());
        }
    }

    /// #346 review: on a miss the D69 hint is the only thing on the screen
    /// that says what to do next, so it is not painted in the dimmest role.
    #[test]
    fn a_search_miss_does_not_dim_its_hint() {
        let ctx = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: theme::ColorDepth::Truecolor,
                ansi: true,
                unicode: true,
            },
        );
        let out = memory_hits(
            &ctx,
            &json!({ "count": 0, "total": 0, "hits": [], "matched": "\"zebra\"" }),
            "zebra",
            false,
        );
        let hint = out
            .lines()
            .find(|l| l.contains("every term was required"))
            .unwrap_or_else(|| panic!("no hint: {out:?}"));
        let muted = ctx.paint("muted", "\u{0}");
        let muted = muted.split('\u{0}').next().expect("an SGR prefix");
        assert!(!hint.contains(muted), "the hint is muted: {hint:?}");

        // Round 2 review of #346: the bounded note does the same job (it
        // names the next command), so it is painted the same way.
        let hit = json!({ "id": "01a0903c-bff0-76a2-9bcb-5428786a56c4", "kind": "doc",
                          "title": "release-process", "source": "", "snippet": "cut" });
        let out = memory_hits(
            &ctx,
            &json!({ "count": 1, "total": 5, "hits": [hit], "matched": "\"cut\"" }),
            "cut",
            false,
        );
        let note = out
            .lines()
            .find(|l| l.contains("--limit 5"))
            .unwrap_or_else(|| panic!("no note: {out:?}"));
        assert!(!note.contains(muted), "the --limit note is muted: {note:?}");
    }

    /// D69, and the round-2 review of D125: a miss names the WHOLE expression
    /// it ran. The summary's label is cut to half the width, so a twelve-term
    /// question printed `"how" "do" ... "rel...   0 hits` and the advice to use
    /// fewer terms could not be acted on: the reader could not see which terms
    /// there were. The note carries it whole, wrapped, never cut.
    #[test]
    fn a_search_miss_names_the_whole_expression() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
        let expr = "\"how\" \"do\" \"i\" \"cut\" \"a\" \"release\" \"for\" \"the\" \"sdk\"";
        let out = memory_hits(
            &ctx,
            &json!({ "count": 0, "total": 0, "hits": [], "matched": expr }),
            "how do i cut a release for the sdk",
            false,
        );
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(expr),
            "the expression is not on the screen whole: {out}"
        );
    }

    /// D125(a) rests on the summary's label being CUT: that is why the note
    /// carrying the expression whole is its only complete copy rather than the
    /// summary said twice (rule 11). Nothing failed if `summary_line` stopped
    /// truncating, so this pins the half the other guard does not.
    #[test]
    fn a_search_summary_keeps_its_label_cut() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
        let expr = "\"how\" \"do\" \"i\" \"cut\" \"a\" \"release\" \"for\" \"the\" \"sdk\"";
        let out = memory_hits(
            &ctx,
            &json!({ "count": 0, "total": 0, "hits": [], "matched": expr }),
            "how do i cut a release for the sdk",
            false,
        );
        let summary = out.lines().next().expect("a summary line");
        assert!(
            !summary.contains(expr),
            "the label is not cut, so the note repeats it (rule 11): {summary:?}"
        );
        assert!(
            summary.contains("0 hits"),
            "the count left the summary: {summary:?}"
        );
        // ...and the whole of it is still on the screen, in the note.
        let rest = out
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(rest.contains(expr), "no complete copy anywhere: {out}");
    }

    /// Round 2 review of D125: the advice fits the search that was run. Telling
    /// someone who already passed `--raw` to use `--raw` is advice they cannot
    /// take, and a raw expression is not quoted by the engine, so the summary
    /// quotes it here to set it off.
    #[test]
    fn a_raw_search_miss_advises_something_the_reader_can_do() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
        let out = memory_hits(
            &ctx,
            &json!({ "count": 0, "total": 0, "hits": [],
                     "matched": "title:release OR body:ship" }),
            "title:release OR body:ship",
            true,
        );
        assert!(
            !out.contains("--raw"),
            "advises the flag the reader already used: {out}"
        );
        let summary = out.lines().next().expect("a summary line");
        assert!(
            summary.contains("\"title:release OR body:ship\""),
            "the summary does not set the raw expression off: {summary:?}"
        );
        // Flattened: `prose` wraps the note at words, and a raw expression has
        // spaces in it.
        let note = out
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            note.contains("nothing matched \"title:release OR body:ship\""),
            "the note does not name the expression it ran: {note:?}"
        );
    }

    /// Round 2 review of D125: a record's lines rank by indent, so the
    /// hierarchy survives a terminal that draws no colour. `muted` emits
    /// nothing under NO_COLOR, and the handle's own line and the matched words
    /// under it were both indented two cells and both unpainted.
    #[test]
    fn a_search_record_ranks_its_lines_by_indent() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(40);
        let out = memory_hits(
            &ctx,
            &json!({ "count": 1, "total": 1, "hits": [
                { "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "kind": "doc",
                  "title": "pricing-page-decisions", "source": "",
                  "snippet": "The release of v2 waits for legal" } ] }),
            "release",
            false,
        );
        let lines: Vec<&str> = out.lines().collect();
        let handle = lines
            .iter()
            .find(|l| l.contains("01a0903c-"))
            .unwrap_or_else(|| panic!("no handle line: {out}"));
        let words = lines
            .iter()
            .find(|l| l.contains("The release of v2"))
            .unwrap_or_else(|| panic!("no matched-words line: {out}"));
        assert!(
            handle.starts_with("  ") && !handle.starts_with("   "),
            "handle line: {handle:?}"
        );
        assert!(
            words.starts_with("    ") && !words.starts_with("     "),
            "the matched words sit at the handle's depth: {words:?}"
        );
    }

    /// Round 2 review of #346: each search record fits its head line to its
    /// OWN handle. The head lines were fitted as one table, so a 36-cell doc
    /// id set the title width for an annotation whose handle is 17 cells:
    /// `Cut build time unde...  annotation on #55` beside 19 empty cells at 60
    /// columns. And where a title and its handle cannot share a line, the
    /// handle takes the next one rather than the title being cut to make
    /// room: at 40 columns every title printed whole on main.
    #[test]
    fn a_search_record_fits_its_head_line_to_its_own_handle() {
        use unicode_width::UnicodeWidthStr;
        let hits = json!({ "count": 2, "total": 2, "hits": [
            { "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "kind": "doc",
              "title": "pricing-page-decisions", "source": "notes/pricing.md",
              "snippet": "The release of v2 waits for legal to sign off" },
            { "id": "01a0903d-243b-7842-8f5c-184005a2d8f2", "kind": "annotation",
              "title": "Cut build time under five minutes", "source": "task:#55",
              "snippet": "Blocked on the release of the new runner image" },
        ] });
        let at = |cols: usize| {
            let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols);
            memory_hits(&ctx, &hits, "release", false)
        };
        let out = at(60);
        assert!(
            out.lines()
                .any(|l| l.starts_with("Cut build time under five minutes")
                    && l.contains("annotation on #55")),
            "the annotation's title was cut beside room it did not need: {out}"
        );
        let out = at(40);
        for l in out.lines() {
            assert!(l.width() <= 40, "{} cells at 40: {l:?}\n{out}", l.width());
        }
        let lines: Vec<&str> = out.lines().collect();
        let doc = lines
            .iter()
            .position(|l| *l == "pricing-page-decisions")
            .unwrap_or_else(|| panic!("the doc title was cut or shares a line: {out}"));
        assert_eq!(
            lines[doc + 1].trim(),
            "01a0903c-c020-70e3-8c9a-7f625ab84c91",
            "the handle is not on the line under the title: {out}"
        );
        assert!(
            lines.contains(&"Cut build time under five minutes"),
            "{out}"
        );
    }

    /// Rule 9, through the one fitter since round 2 of #346 (`keep_ranked`):
    /// the summary drops facts from the right until it fits, and the first
    /// fact, the count, is kept whatever the width.
    #[test]
    fn the_summary_drops_facts_from_the_right() {
        // 40 is the narrowest width a Ctx takes (`Ctx::MIN_COLS`).
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(40);
        let parts = vec![
            ("card.strong", "44 tasks".to_string()),
            ("muted", "20 shown".to_string()),
            ("timer.active", "#49 #50 #51 running".to_string()),
            ("muted", "2 blocked".to_string()),
        ];
        // `2 blocked` would fit after the running fact is dropped; it goes
        // anyway, because a fact never outlives one ranked above it.
        let mid = ctx.mid().to_string();
        assert_eq!(
            summary_line(&ctx, None, parts),
            format!("44 tasks {mid} 20 shown")
        );
    }

    /// Round 2 review of #346: `next` and `add`'s echo fit their facts by the
    /// one rule `summary_line` follows: taken in rank order, and the first that
    /// does not fit ends the line, so nothing less important survives a fact
    /// that was dropped. `next` kept the tags after dropping the project, and
    /// `add` ranked the project above the deadline where `next` ranked it
    /// below.
    #[test]
    fn next_and_add_drop_facts_from_the_least_important_up() {
        let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(60);
        let mut t = task_json(
            48,
            "Renew the certificate",
            "infrastructure-platform-team-west",
            "2026-08-05T00:00:00Z",
            &["ops"],
        );
        t["priority"] = json!("H");
        t["urgency"] = json!(18.1);
        let next = next_task(&ctx, &json!({ "tasks": [t.clone()] }), anchor());
        let facts = next.lines().nth(1).expect("a facts line");
        assert!(facts.contains("due "), "{next}");
        assert!(!facts.contains("infrastructure"), "{next}");
        assert!(
            !facts.contains("+ops"),
            "a fact outlived a more important one that was dropped: {facts:?}"
        );

        let ctx = ctx.with_cols(40);
        let added = echo::added(&ctx, &t, anchor());
        let facts = added.lines().nth(1).expect("a facts line");
        assert!(
            facts.contains("due "),
            "add ranked the project above the deadline: {added}"
        );
        assert!(!facts.contains("infrastructure"), "{added}");
    }

    /// Finding #3 (audit-2026-09): `start`/`stop`/`done` confirmed an action
    /// without ever naming the task, so a wrong ref printed a success line
    /// identical to the right one — and `done` never compared tracked time
    /// against the estimate, the entire payoff of `est:2h` at capture time.
    #[test]
    fn start_stop_done_name_the_task_and_done_compares_tracked_to_estimate() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let now = Timestamp::now();
        let none = Titles::new();

        let started_json = json!({
            "interval_started": "2026-09-09T10:43:48Z",
            "short_id": 144,
            "title": "Prod: mailbox sync 500 op staging",
        });
        let out = started(&ctx, &started_json, &started_json, &none, now);
        assert!(out.contains("#144"), "{out:?}");
        assert!(out.contains("Prod: mailbox sync 500 op staging"), "{out:?}");

        let stopped_json = json!({
            "interval": "PT12S", "tracked": "PT12S", "short_id": 9, "title": "some task"
        });
        let out = stopped(&ctx, &stopped_json, &stopped_json, now);
        assert!(out.contains("#9"), "{out:?}");
        assert!(out.contains("some task"), "{out:?}");
        assert!(
            out.contains("12s"),
            "must humanize the ISO duration: {out:?}"
        );

        let done_json = json!({
            "completed": "2026-09-09T10:44:18Z",
            "unblocked": [],
            "short_id": 144,
            "title": "Prod: mailbox sync 500 op staging",
            "tracked": "PT30S",
            "estimate": "PT2H",
        });
        let out = done(&ctx, &done_json, &done_json, &none, now);
        assert!(out.contains("#144"), "{out:?}");
        assert!(out.contains("Prod: mailbox sync 500 op staging"), "{out:?}");
        assert!(
            out.contains("tracked 30s of 2h"),
            "the tracked-vs-estimate clause is missing: {out:?}"
        );
    }

    /// Finding #9 (audit-2026-09): `start` on an already-active task and `dep`
    /// on an already-existing edge both answered as if they had just done
    /// something — the same "Started"/"now depends on" a genuine change gets.
    #[test]
    fn start_and_dep_say_already_instead_of_claiming_a_fresh_action() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let now = Timestamp::now();
        let none = Titles::new();

        let fresh = json!({
            "interval_started": "2026-09-09T10:35:52Z", "short_id": 1, "title": "restart",
            "already_running": false,
        });
        assert!(started(&ctx, &fresh, &fresh, &none, now).contains("started"));

        let idempotent = json!({
            "interval_started": "2026-09-09T10:35:52Z", "short_id": 1, "title": "restart",
            "already_running": true,
        });
        let out = started(&ctx, &idempotent, &idempotent, &none, now);
        assert!(
            out.contains("already running"),
            "must not claim a fresh start: {out:?}"
        );
        assert!(!out.contains("started"), "{out:?}");

        let fresh_dep =
            json!({ "short_id": 250, "depends_on": [249], "blocked": true, "inserted": true });
        let out = dep_changed(&ctx, &fresh_dep, &fresh_dep, true, "249", now);
        assert!(
            out.contains("blocked by #249") && !out.contains("already"),
            "{out:?}"
        );

        let existing_dep =
            json!({ "short_id": 250, "depends_on": [249], "blocked": true, "inserted": false });
        let out = dep_changed(&ctx, &existing_dep, &existing_dep, true, "249", now);
        assert!(
            out.contains("already blocked by #249"),
            "must not claim a fresh edge: {out:?}"
        );
    }

    /// A bare `add` inherits the default, so the confirmation has to say where
    /// the task actually went — otherwise the landing project stays invisible
    /// at the exact moment it matters.
    #[test]
    fn task_added_names_the_project_it_landed_in() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let now = Timestamp::now();
        let out = added(
            &ctx,
            &json!({ "short_id": 3, "title": "a task", "status": "pending",
                     "urgency": 5.0, "project": "work" }),
            now,
        );
        assert!(
            out.contains("work"),
            "the landing project is invisible: {out:?}"
        );

        // Finding #10 (audit-2026-09): a task landing with no default project
        // must SAY so, naming the way out — not just omit the suffix, which
        // reads identically to every other successful `add`.
        let none = added(
            &ctx,
            &json!({ "short_id": 4, "title": "homeless", "status": "pending",
                     "urgency": 5.0, "project": null }),
            now,
        );
        assert!(
            none.contains("no project"),
            "did not say the task landed nowhere: {none:?}"
        );
        assert!(
            none.contains("tasqx use"),
            "did not name the way out: {none:?}"
        );
    }

    /// Text whose char count and terminal-cell count disagree, one entry per
    /// way they can disagree. Every table guard below runs the whole list, so a
    /// fix that measures CJK correctly but splits an emoji cluster still fails.
    ///
    /// `chars != cells` in four different directions:
    ///  * a CJK ideograph is 1 char, 2 cells;
    ///  * a combining mark is a char with 0 cells;
    ///  * an emoji ZWJ sequence is 5 chars and one 2-cell cluster;
    ///  * a skin-tone modifier is 2 chars and one 2-cell cluster.
    const AWKWARD: &[&str] = &[
        "plain ascii",
        "漢字テスト",
        "e\u{301}accent",                                  // e + COMBINING ACUTE
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466} fam", // family ZWJ sequence
        "\u{1f44d}\u{1f3fd} ok",                           // thumbs up + skin tone modifier
        "中文",
    ];

    fn cells(s: &str) -> usize {
        unicode_width::UnicodeWidthStr::width(s)
    }

    /// How many lines of chrome a `task_table` draws before its first row:
    /// summary, blank, header.
    ///
    /// A constant rather than a `skip(3)` written out at each call site. These
    /// tests are about a COLUMN — how wide it is, whether it is drawn at all —
    /// and every one of them that spelled its own row offset had to be edited
    /// when the summary line moved in front of the header, none of them for a
    /// reason to do with what they assert.
    const CHROME: usize = 3;

    /// The `ID … TAGS` label line of a rendered table.
    fn header_of(text: &str) -> &str {
        text.lines().nth(CHROME - 1).expect("header line")
    }

    /// The data rows of a rendered table. Not trailer-aware on purpose — the
    /// health notes print flush against the last row, so a caller that cares
    /// takes exactly as many rows as it put in.
    fn rows_of(text: &str) -> impl Iterator<Item = &str> {
        text.lines().skip(CHROME)
    }

    /// A terminal column is a grid of CELLS. `format!("{s:<36}")` pads by CHAR
    /// COUNT, so one CJK title or one emoji shifted every column to its right
    /// and the table stopped being a table.
    ///
    /// The assertion is on the whole ROW rather than on a helper: the rows here
    /// differ ONLY in the title, and every other cell is identical, so equal
    /// display width across rows is exactly "the title column holds its budget".
    /// That is true no matter how the padding is implemented, which is the point
    /// — it cannot be satisfied by a helper that is correct while a call site
    /// still formats with `{:<36}`.
    #[test]
    fn task_table_title_column_holds_its_width_in_cells() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let tasks: Vec<Value> = AWKWARD
            .iter()
            .enumerate()
            .map(|(i, title)| {
                json!({ "short_id": i + 1, "urgency": 5.0, "priority": "M", "title": title,
                        "project": "work", "due": "2026-07-20T17:00:00Z", "tags": ["t"],
                        "status": "pending" })
            })
            .collect();
        let out = task_table(
            &ctx,
            &json!({ "tasks": tasks, "count": tasks.len() }),
            Timestamp::now(),
        );
        let rows: Vec<&str> = rows_of(&out).take(AWKWARD.len()).collect();
        assert_eq!(
            rows.len(),
            AWKWARD.len(),
            "expected one row per title: {out:?}"
        );
        let want = cells(rows[0]);
        for (row, title) in rows.iter().zip(AWKWARD) {
            assert_eq!(
                cells(row),
                want,
                "row for {title:?} is {} cells, not {want}: {row:?}",
                cells(row)
            );
        }
    }

    /// The same rule on the OTHER columns of the same table: a fix that only
    /// sized `title` correctly would leave `project` and `due` shifting the
    /// columns to their right.
    #[test]
    fn task_table_project_and_due_columns_hold_their_width_in_cells() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        for field in ["project", "due"] {
            let tasks: Vec<Value> = AWKWARD
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let mut t = json!({ "short_id": i + 1, "urgency": 5.0, "priority": "M",
                                        "title": "same", "project": "work", "due": "",
                                        "tags": ["t"], "status": "pending" });
                    t[field] = json!(v);
                    t
                })
                .collect();
            let out = task_table(
                &ctx,
                &json!({ "tasks": tasks, "count": tasks.len() }),
                Timestamp::now(),
            );
            let rows: Vec<&str> = rows_of(&out).take(AWKWARD.len()).collect();
            let want = cells(rows[0]);
            for (row, v) in rows.iter().zip(AWKWARD) {
                assert_eq!(cells(row), want, "{field}={v:?} broke alignment: {row:?}");
            }
        }
    }

    /// #228.16: the table used to print a tag bare (`cardtag`), the one
    /// spelling `list`'s own filter grammar rejects (`unknown filter token
    /// "cardtag"`) — copying what the tool just printed into the tool's own
    /// query language was an error. `+tag` is what `tag.add`/`modify` already
    /// echo and the only spelling the filter parses.
    #[test]
    fn the_table_renders_tags_in_the_spelling_the_filter_accepts() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let tasks = vec![json!({ "short_id": 1, "urgency": 5.0, "priority": "M",
                                  "title": "t", "project": "", "due": "",
                                  "tags": ["cardtag"], "status": "pending" })];
        let out = task_table(
            &ctx,
            &json!({ "tasks": tasks, "count": 1 }),
            Timestamp::now(),
        );
        assert!(
            out.contains("+cardtag"),
            "the TAGS column must render `+cardtag`, not bare `cardtag`: {out:?}"
        );
    }

    // ---- what the list screen says without being read closely --------------

    /// `due_cell`'s whole vocabulary, dated from one fixed instant.
    ///
    /// The table is the assertion: every branch is one line, so a branch added
    /// without a spelling — or a spelling changed without a reason — is visible
    /// as a diff on this list rather than as a cell nobody looked at.
    #[test]
    fn the_due_cell_dates_a_deadline_the_way_a_reader_would() {
        // A Thursday, mid-afternoon: far enough into the day that "today" and
        // "already today" are both reachable from it.
        let now: Timestamp = "2026-09-10T15:00:00Z".parse().unwrap();
        for (iso, want) in [
            ("2026-09-10T23:59:00Z", "today 23:59"),
            ("2026-09-10T09:00:00Z", "today 09:00"),
            ("2026-09-10T00:00:00Z", "today"),
            ("2026-09-11T00:00:00Z", "tomorrow"),
            ("2026-09-11T09:00:00Z", "tomorrow 09:00"),
            ("2026-09-09T00:00:00Z", "yesterday"),
            ("2026-09-08T00:00:00Z", "2d ago"),
            ("2026-07-29T00:00:00Z", "43d ago"),
            // Inside the week the weekday is the whole answer: there is
            // exactly one Sunday between here and next Thursday.
            ("2026-09-13T00:00:00Z", "Sun"),
            ("2026-09-16T09:00:00Z", "Wed"),
            // Past it, the day needs naming; the year only when it changes.
            ("2026-09-17T00:00:00Z", "17 Sep"),
            ("2026-11-04T00:00:00Z", "4 Nov"),
            ("2027-01-04T00:00:00Z", "4 Jan 27"),
        ] {
            let at: Timestamp = iso.parse().unwrap();
            assert_eq!(due_cell(at, now), want, "{iso}");
        }
    }

    /// The `DUE` column holds a date a reader can act on, not the instant the
    /// store happens to keep.
    ///
    /// `task_row` used to pass `s(t, "due")` through untouched, so this column
    /// spent twenty cells per row on `2026-09-11T00:00:00Z` — while `TASK`, the
    /// column the row is read for, was the one `TaskCols::fit` cut to its floor
    /// on an 80-cell terminal. The assertion is on the SHAPE of the stamp
    /// rather than on the word, so it holds whatever [`due_cell`]'s vocabulary
    /// grows into.
    #[test]
    fn the_due_column_no_longer_prints_the_stored_instant() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let now: Timestamp = "2026-09-10T15:00:00Z".parse().unwrap();
        let out = task_table(
            &ctx,
            &json!({ "tasks": [
                task_json(1, "ship it", "work", "2026-09-11T00:00:00Z", &["t"]),
            ], "count": 1 }),
            now,
        );
        assert!(
            !out.contains("T00:00:00Z"),
            "the RFC-3339 instant is still in the table: {out:?}"
        );
        assert!(out.contains("tomorrow"), "{out:?}");
    }

    /// A `due` this build cannot parse is printed as it stands rather than
    /// dropped — the policy `field_ts` already states for the overdue test, and
    /// the one `markdown::fmt_instant` follows on the `show` card. A cell that
    /// went blank instead would hide a field the store plainly holds, which is
    /// the invisible-field failure D36 exists to prevent.
    #[test]
    fn an_unreadable_due_falls_back_to_itself() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_table(
            &ctx,
            &json!({ "tasks": [
                task_json(1, "ship it", "work", "next tuesday-ish", &["t"]),
            ], "count": 1 }),
            Timestamp::now(),
        );
        assert!(
            out.contains("next tuesday-ish"),
            "an unparseable due vanished from the table: {out:?}"
        );
    }

    /// The running task and the blocked one are marked in the left rail, and
    /// the rail survives the terminal that drops every other optional column.
    ///
    /// Audit finding #147: nothing in `tasqx list` marked the one running
    /// timer — the single piece of state a work block depends on. `STATUS` did
    /// carry it, to the RIGHT of a title that can run 72 cells, and
    /// `TaskCols::fit` drops that column outright on a narrow terminal. So the
    /// assertion is made at 40 cells: a rail that only shows up when there is
    /// room for it is the bug moving the glyph was meant to fix.
    #[test]
    fn the_rail_marks_the_running_and_the_blocked_row_at_any_width() {
        let mut ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        ctx.cols = 40;
        let result = json!({ "tasks": [
            { "short_id": 1, "urgency": 9.0, "priority": "H", "title": "running one",
              "project": "work", "due": "", "tags": [], "status": "active" },
            { "short_id": 2, "urgency": 8.0, "priority": "H", "title": "stuck one",
              "project": "work", "due": "", "tags": [], "status": "pending", "blocked": true },
            { "short_id": 3, "urgency": 7.0, "priority": "M", "title": "ordinary one",
              "project": "work", "due": "", "tags": [], "status": "pending" },
        ], "count": 3 });
        let out = task_table(&ctx, &result, Timestamp::now());
        let rows: Vec<&str> = rows_of(&out).take(3).collect();
        assert!(rows[0].starts_with('*'), "no timer glyph: {:?}", rows[0]);
        assert!(rows[1].starts_with('B'), "no blocked glyph: {:?}", rows[1]);
        assert!(
            rows[2].starts_with("  "),
            "an ordinary row grew a rail: {:?}",
            rows[2]
        );
    }

    /// The house style's Contract table is BUILT from the renderer, then
    /// looked for in the doc.
    ///
    /// `docs/maintainers/terminal-style.md` is what anyone touching a screen reads first,
    /// and a guide describing a screen the binary no longer prints is worse
    /// than no guide — it is a wrong answer carrying the repo's authority. So
    /// the rows are generated here and asserted present there, which catches
    /// the drift in BOTH directions: a glyph the code stopped drawing, and a
    /// glyph the doc stopped naming. Checking only "every glyph the code emits
    /// appears somewhere in the doc" was tried first and let the Contract
    /// table go stale while the prose above it still happened to quote the
    /// right character.
    ///
    /// `include_str!` reaches outside `src/` on purpose, the same way
    /// `doc_gate_tests` in `tasqx-core` embeds `.github/workflows/ci.yml`:
    /// moving or renaming the file is then a compile error rather than a
    /// silent orphaning.
    #[test]
    fn the_house_style_doc_still_describes_the_screens() {
        const DOC: &str = include_str!("../../../docs/maintainers/terminal-style.md");

        let task = |status: &str, blocked: bool| {
            json!({ "short_id": 1, "urgency": 1.0, "priority": "M", "title": "t",
                    "project": "p", "due": "", "tags": [], "status": status,
                    "blocked": blocked })
        };
        let rail = |status: &str, blocked: bool, unicode: bool| {
            rail_marker(&task(status, blocked), unicode)
                .expect("a rail glyph")
                .1
                .to_string()
        };

        // Every glyph the gauge can emit, swept across its range, split into
        // the three roles the doc names them by.
        let full = urgency_meter(1.0).0.chars().next().expect("a full cell");
        let track = urgency_meter(0.0).1.chars().next().expect("a track cell");
        let mut remainder: Vec<char> = Vec::new();
        for step in 0..=100 {
            let (bar, tail) = urgency_meter(f64::from(step) / 100.0);
            assert_eq!(
                bar.chars().count() + tail.chars().count(),
                4,
                "the gauge is not 4 cells wide at {step}%"
            );
            for c in bar.chars().chain(tail.chars()) {
                if c != full && c != track && !remainder.contains(&c) {
                    remainder.push(c);
                }
            }
        }
        remainder.sort_unstable();
        let remainder: String = remainder
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(" ");

        // Where each of the built-in ramp's bands begins, on the urgency scale.
        let anchors = theme::builtin("nord").unwrap().ramp().len();
        let bands = (0..anchors)
            .map(|i| {
                let from = i as f64 / (anchors - 1) as f64 * tasqx_core::urgency::DUE_WEIGHT;
                format!("from {from:.0}")
            })
            .collect::<Vec<_>>()
            .join(" · ");

        for row in [
            format!(
                "| rail, running | `{}` (`{}` without Unicode) |",
                rail("active", false, true),
                rail("active", false, false)
            ),
            format!(
                "| rail, blocked | `{}` (`{}` without Unicode) |",
                rail("pending", true, true),
                rail("pending", true, false)
            ),
            format!(
                "| rail, the one in effect | `{}` (`projects`, `theme list`) |",
                CurrentRail::MARK
            ),
            format!("| gauge, full cell | `{full}` |"),
            format!("| gauge, track | `{track}` |"),
            format!("| gauge, remainder | {remainder} |"),
            "| gauge width | 4 cells |".to_string(),
            format!(
                "| gauge full at | urgency {:.0} (`urgency::DUE_WEIGHT`) |",
                tasqx_core::urgency::DUE_WEIGHT
            ),
            format!("| ramp bands, built-ins | {bands} |"),
            "| column-header role | `table.label` |".to_string(),
        ] {
            assert!(
                DOC.contains(&row),
                "docs/maintainers/terminal-style.md's Contract table is missing the row the \
                 renderer produces:\n{row}"
            );
        }

        assert!(
            theme::default_theme()
                .role_names()
                .iter()
                .any(|r| r == "table.label"),
            "the doc names a theme role no built-in defines"
        );
        assert!(
            DOC.contains(&plural_tasks(1)) && DOC.contains("N tasks"),
            "the doc lost the count spelling"
        );
    }

    /// The gauge separates the scores a reader is actually ranking.
    ///
    /// The first mock drew whole cells only, and 17.9 and 15.8 against a top of
    /// 17.9 both came out `▄▄▄▄`, the two rows a reader compared hardest drawn
    /// identically. Three steps inside each cell is what fixed it. Under D119's
    /// absolute scale both of those now saturate, on purpose, so the pairs that
    /// matter are the landmarks below the top: no priority, L, M and H alone,
    /// an H task ten days from its deadline, and overdue. Each must draw its
    /// own mark. A step is one urgency point, so the age term's hundredths are
    /// not asked to show.
    #[test]
    fn the_urgency_gauge_separates_the_scores_the_reader_is_ranking() {
        let marks: Vec<(String, String)> = [0.0, 1.8, 3.9, 6.0, 9.0, 12.0]
            .iter()
            .map(|&u| urgency_meter(urgency_scale(u)))
            .collect();
        for (i, a) in marks.iter().enumerate() {
            for b in &marks[i + 1..] {
                assert_ne!(a, b, "two landmarks draw the same gauge: {marks:?}");
            }
        }
    }

    /// More urgency is never less ink.
    ///
    /// The first fix for the resolution problem drew the remainder as a TALLER
    /// glyph than the bar's own `▄`, which made a nearly-empty gauge (`▇▁▁▁` at
    /// 22% of the range) the heaviest mark in a column whose hottest row was a
    /// flat `▄▄▄▄`. That is the ranking inverted, and it is invisible to any
    /// assertion about the value — so this one weighs the glyphs.
    #[test]
    fn the_gauge_never_draws_more_ink_for_less_urgency() {
        let mass = |ramp: f64| -> usize {
            let (bar, track) = urgency_meter(ramp);
            format!("{bar}{track}")
                .chars()
                .map(|c| match c {
                    '▁' => 1,
                    '▂' => 2,
                    '▃' => 3,
                    '▄' => 4,
                    other => panic!("unexpected gauge glyph {other:?}"),
                })
                .sum()
        };
        let mut last = 0;
        for step in 0..=100 {
            let here = mass(f64::from(step) / 100.0);
            assert!(
                here >= last,
                "the gauge got lighter going up the scale at {step}%: {here} after {last}"
            );
            last = here;
        }
        assert_eq!(mass(0.0), 4, "an empty gauge is a bare track");
        assert_eq!(mass(1.0), 16, "a full gauge is four full cells");
    }

    /// On a tie between PROJECT and DUE, PROJECT gives the cell.
    ///
    /// `columns::fit` breaks ties to the right by default, which cut `agenda`'s
    /// overdue dates to `due 2026-0…` on an 80-column terminal while the
    /// project beside them kept its full width. The order the table had before
    /// the shared fitter, STATUS then TAGS then PROJECT then DUE, is carried
    /// as `tie_rank`s. A cut project name still identifies itself; a cut date
    /// says nothing.
    #[test]
    fn on_a_tie_the_project_gives_before_the_date() {
        let mut r = task_row(
            &json!({ "short_id": 1, "urgency": 1.0, "priority": "M",
                     "title": "t".repeat(14), "project": "p".repeat(14),
                     "status": "pending" }),
            Timestamp::now(),
            false,
        );
        r.due = "d".repeat(14);
        let natural = TaskCols::fit(std::slice::from_ref(&r), 200, "DUE", false);
        assert_eq!((natural.project, natural.due), (14, 14), "not a tie");
        let budget =
            columns::total(&[natural.head(), natural.title, natural.project, natural.due]) - 1;
        let c = TaskCols::fit(std::slice::from_ref(&r), budget, "DUE", false);
        assert_eq!((c.project, c.due), (13, 14));
    }

    /// The gauge glyphs on the row carrying `title`, and nothing else.
    fn gauge_of(out: &str, title: &str) -> String {
        out.lines()
            .find(|l| l.contains(title))
            .unwrap_or_else(|| panic!("no row for {title:?} in {out}"))
            .chars()
            .filter(|c| matches!(c, '▁' | '▂' | '▃' | '▄'))
            .collect()
    }

    /// D119: a task's gauge and colour are a property of the TASK, not of the
    /// other rows on screen.
    ///
    /// The denominator used to be the hottest visible row, so one overdue task
    /// at 18.5 flattened every other gauge on a real store, and a filtered view
    /// gave its top row a full bar in the danger colour whatever that row held.
    /// On a project with nothing pressing that meant done tasks were painted
    /// in `danger`. Compared painted, colour included, so a scale that moved
    /// only the colour would fail here too.
    #[test]
    fn the_same_task_draws_the_same_urgency_cell_whatever_else_is_on_screen() {
        let ctx = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: theme::ColorDepth::Truecolor,
                ansi: true,
                unicode: true,
            },
        );
        let row = |id: i64, title: &str, urgency: f64| {
            let mut t = task_json(id, title, "work", "", &[]);
            t["urgency"] = json!(urgency);
            t
        };
        let subject = "the task under test";
        let alone = json!({ "tasks": [row(1, subject, 6.0)], "count": 1 });
        let beside = json!({ "tasks": [
            row(2, "an outlier elsewhere", 9.9), row(1, subject, 6.0),
        ], "count": 2 });
        let cell = |r: &Value| {
            let out = task_table(&ctx, r, Timestamp::now());
            let line = out.lines().find(|l| l.contains(subject)).unwrap();
            line.split(subject).next().unwrap().to_string()
        };
        assert_eq!(
            cell(&alone),
            cell(&beside),
            "a hotter row elsewhere on screen changed this task's urgency cell"
        );
    }

    /// D119: a full gauge means "as urgent as an overdue task". The scale is
    /// the due term's saturation, read from core, so it cannot drift from the
    /// formula it describes.
    ///
    /// Scored through `urgency::score_at` rather than written as literals, so
    /// a change to the formula moves both sides of this test together. What H
    /// priority alone earns is half of that, and draws half a gauge. That half
    /// is also where the built-in ramp's middle band begins, so if the formula
    /// stops making H exactly half the due weight, this fails, and D119's
    /// "warn starts at H alone" needs revisiting rather than this assertion.
    #[test]
    fn a_full_gauge_means_as_urgent_as_an_overdue_task() {
        use tasqx_core::types::Priority;
        use tasqx_core::urgency::score_at;

        let mut ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        ctx.caps.unicode = true;
        let now: Timestamp = "2026-09-11T12:00:00Z".parse().unwrap();
        let created = "2026-09-11T12:00:00Z";
        let gauge = |urgency: f64| {
            let mut t = task_json(1, "subject", "work", "", &[]);
            t["urgency"] = json!(urgency);
            gauge_of(
                &task_table(&ctx, &json!({ "tasks": [t], "count": 1 }), now),
                "subject",
            )
        };

        let overdue = score_at(None, Some("2026-09-01T00:00:00Z"), created, now);
        let h_alone = score_at(Some(Priority::H), None, created, now);
        assert_eq!(gauge(overdue), "▄▄▄▄", "overdue at {overdue}");
        assert_eq!(gauge(h_alone), "▄▄▁▁", "H priority alone at {h_alone}");
        assert_ne!(
            gauge(overdue - 0.5),
            "▄▄▄▄",
            "the gauge fills before the task is as urgent as an overdue one"
        );
    }

    /// No glyph set degrades honestly here, so a terminal without Unicode gets
    /// the figure and no gauge — and the column narrows by exactly what the
    /// gauge would have cost, rather than leaving five cells of nothing.
    #[test]
    fn a_terminal_without_unicode_gets_no_gauge_and_the_cells_back() {
        let result = json!({ "tasks": [
            task_json(1, "a long enough title to notice", "work", "", &["t"]),
        ], "count": 1 });
        let plain = task_table(
            &Ctx::new(theme::default_theme(), Caps::PLAIN),
            &result,
            Timestamp::now(),
        );
        assert!(
            !plain.contains('▄') && !plain.contains('▁'),
            "block glyphs on a terminal that cannot draw them: {plain:?}"
        );

        let mut uni = Ctx::new(theme::default_theme(), Caps::PLAIN);
        uni.caps.unicode = true;
        let drawn = task_table(&uni, &result, Timestamp::now());
        assert!(drawn.contains('▄'), "no gauge with Unicode: {drawn:?}");
        assert_eq!(
            cells(header_of(&drawn)) - cells(header_of(&plain)),
            TaskCols::METER,
            "the gauge's cells were not handed back:\n{plain}\n{drawn}"
        );
    }

    /// A store with nothing running and nothing blocked pays nothing for the
    /// rail — D51's rule for `DUE`, applied to the column left of the ids.
    #[test]
    fn a_store_with_no_state_to_show_is_not_indented_by_the_rail() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_table(
            &ctx,
            &json!({ "tasks": [task_json(1, "ordinary", "work", "", &[])], "count": 1 }),
            Timestamp::now(),
        );
        assert!(
            header_of(&out).starts_with("  ID"),
            "an unused rail still indented the table: {:?}",
            header_of(&out)
        );
    }

    /// The summary line answers the questions a count cannot.
    ///
    /// `N task(s)` was the whole trailer: a reader with the rows in front of
    /// them can count them, and what they cannot see at a glance is how much of
    /// the list is late, due before the day is out, running, or stuck.
    #[test]
    fn the_summary_names_what_the_rows_do_not_show_at_a_glance() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let now: Timestamp = "2026-09-10T15:00:00Z".parse().unwrap();
        let result = json!({ "tasks": [
            { "short_id": 1, "urgency": 9.0, "priority": "H", "title": "late one",
              "project": "work", "due": "2026-09-08T00:00:00Z", "tags": [], "status": "pending" },
            { "short_id": 2, "urgency": 8.0, "priority": "H", "title": "today one",
              "project": "work", "due": "2026-09-10T23:00:00Z", "tags": [], "status": "active" },
            { "short_id": 3, "urgency": 7.0, "priority": "M", "title": "stuck one",
              "project": "work", "due": "", "tags": [], "status": "pending", "blocked": true },
        ], "count": 3 });
        let table = task_table(&ctx, &result, now);
        let summary = table.lines().next().unwrap();
        for want in [
            "3 tasks",
            "1 overdue",
            "1 due today",
            "#2 running",
            "1 blocked",
        ] {
            assert!(summary.contains(want), "{want:?} missing from {summary:?}");
        }
        assert!(
            !summary.contains("task(s)"),
            "the programmer's plural survived: {summary:?}"
        );
    }

    /// A fact that is zero is not printed. A line that always said `0 overdue`
    /// would train the reader to skip it, and then it is not there on the day
    /// it says something.
    #[test]
    fn the_summary_prints_only_the_facts_that_are_true() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_table(
            &ctx,
            &json!({ "tasks": [task_json(1, "ordinary", "work", "", &[])], "count": 1 }),
            Timestamp::now(),
        );
        let summary = out.lines().next().unwrap();
        assert_eq!(summary.trim(), "1 task", "{summary:?}");
    }

    /// `list` echoes the filter it answered, on every run rather than only on
    /// an empty result: `tasqx list` defaults to `@working` (`verbs::list`),
    /// and a reader who cannot see which question was asked cannot tell a short
    /// answer from a narrow one.
    #[test]
    fn the_summary_names_the_filter_that_produced_it() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_table_filtered(
            &ctx,
            &json!({ "tasks": [task_json(1, "ordinary", "work", "", &[])], "count": 1 }),
            Timestamp::now(),
            Some("project:raid +design"),
        );
        assert!(
            out.lines().next().unwrap().contains("project:raid +design"),
            "{out:?}"
        );
    }

    /// One row of table JSON, so a layout test can vary the one field it is about.
    fn task_json(id: i64, title: &str, project: &str, due: &str, tags: &[&str]) -> Value {
        json!({ "short_id": id, "urgency": 5.0, "priority": "M", "title": title,
                "project": project, "due": due, "tags": tags, "status": "pending" })
    }

    /// A column no row fills is not drawn, and its cells go to the columns that
    /// have something to show.
    ///
    /// This is what the reader was looking at when they said the table "doesn't
    /// look aligned": on a store with no due dates, `DUE` still held 22 cells
    /// plus its gaps, so the widest gap in the table sat where there was no
    /// data — a hole between `PROJECT` and `TAGS` that reads as a broken grid —
    /// while `TASK` was cut to 36 cells and ellipsised titles that had 40 cells
    /// of empty terminal to their right.
    #[test]
    fn a_column_no_task_fills_is_not_drawn_and_its_room_goes_to_the_titles() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let long = "Level-1 gameplay vision walkthrough, progression and escalation";
        let with_due = json!({ "tasks": [
            task_json(1, long, "raid.game", "2026-07-20T17:00:00Z", &["design"]),
        ], "count": 1 });
        let without_due = json!({ "tasks": [
            task_json(1, long, "raid.game", "", &["design"]),
        ], "count": 1 });

        let head = |t: &Value| header_of(&task_table(&ctx, t, Timestamp::now())).to_string();
        assert!(head(&with_due).contains("DUE"), "{}", head(&with_due));
        assert!(
            !head(&without_due).contains("DUE"),
            "an empty DUE column was still drawn: {:?}",
            head(&without_due)
        );
        // And the title survives whole once the dead column is gone.
        let table = task_table(&ctx, &without_due, Timestamp::now());
        let row = rows_of(&table).next().unwrap().to_string();
        assert!(
            row.contains(long),
            "the title was truncated with room to spare: {row:?}"
        );
    }

    /// The table lays out for the terminal it is printing into, and never
    /// overruns it — including the rule, which is drawn from the same widths.
    #[test]
    fn the_table_fits_the_width_it_was_given() {
        for cols in [Ctx::MIN_COLS, 60, 80, Ctx::DEFAULT_COLS, Ctx::MAX_COLS] {
            let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols);
            let tasks: Vec<Value> = (1..=4)
                .map(|i| {
                    task_json(
                        i,
                        "a title long enough that it cannot possibly fit any of these budgets \
                         without being cut somewhere",
                        "some.rather.long.project.name",
                        "2026-07-20T17:00:00Z",
                        &["one", "two", "three", "four", "five", "six"],
                    )
                })
                .collect();
            let out = task_table(
                &ctx,
                &json!({ "tasks": tasks, "count": tasks.len() }),
                Timestamp::now(),
            );
            for line in out.lines() {
                assert!(
                    cells(line) <= cols,
                    "a {}-cell line in a {cols}-cell terminal: {line:?}",
                    cells(line)
                );
            }
        }
    }

    /// Wider is not a licence to spread: past `MAX_COLS` the extra cells are
    /// left alone rather than poured into one enormous title column.
    #[test]
    fn an_ultrawide_terminal_does_not_get_an_ultrawide_table() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(400);
        assert_eq!(ctx.cols, Ctx::MAX_COLS, "the width must be clamped");
        let title = "x".repeat(300);
        let out = task_table(
            &ctx,
            &json!({ "tasks": [task_json(1, &title, "p", "", &["t"])], "count": 1 }),
            Timestamp::now(),
        );
        for line in out.lines() {
            assert!(cells(line) <= Ctx::MAX_COLS, "{line:?}");
        }
    }

    /// `projects` and `report` pad a user-authored string into a column too, so
    /// the rule is theirs as well — fixing only `list` would leave two tables
    /// with the old bug and no test able to see it.
    #[test]
    fn project_and_report_tables_hold_their_widths_in_cells() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

        let projects: Vec<Value> = AWKWARD
            .iter()
            .map(|n| json!({ "name": n, "archived": false, "default": false, "description": "d" }))
            .collect();
        let out = project_table(
            &ctx,
            &json!({ "count": projects.len(), "projects": projects }),
        );
        let rows: Vec<&str> = out.lines().skip(1).collect();
        let want = cells(rows[0]);
        for (row, n) in rows.iter().zip(AWKWARD) {
            assert_eq!(cells(row), want, "project {n:?} broke alignment: {row:?}");
        }

        let groups: Vec<Value> = AWKWARD
            .iter()
            .map(|k| {
                json!({ "project": k, "count": 1, "est_total": "PT1H", "overdue": 0,
                             "tracked_total": "PT2H", "tokens_total": 123456 })
            })
            .collect();
        let out = report(&ctx, &json!({ "groups": groups }), "project", None);
        let rows: Vec<&str> = out.lines().skip(1).collect();
        let want = cells(rows[0]);
        for (row, k) in rows.iter().zip(AWKWARD) {
            assert_eq!(
                cells(row),
                want,
                "report group {k:?} broke alignment: {row:?}"
            );
        }
    }

    /// D48a: the TOKENS column names the largest bucket, and the blend it used to
    /// print is gone from this surface.
    ///
    /// This test replaces `report_shows_a_tokens_total_column`, which asserted
    /// the opposite and was correct until D48. The fixture's `tokens_total` is
    /// deliberately present and deliberately unrendered: a store carrying the
    /// field is exactly the case where the old behaviour could creep back, and a
    /// fixture that omitted it could not tell the difference. Since D50 the
    /// engine no longer emits the field at all; the fixture stands in for a
    /// pre-D50 payload, so keep it.
    #[test]
    fn report_names_the_largest_bucket_instead_of_blending() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = report(
            &ctx,
            &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H",
                  "tokens_in": 136, "tokens_out": 83_479,
                  "tokens_cache_read": 13_630_240, "tokens_cache_creation": 186_965,
                  "tokens_total": 13_900_820 }
            ] }),
            "project",
            None,
        );
        assert!(out.contains("TOKENS"), "TOKENS header missing: {out:?}");
        let row = out.lines().nth(1).unwrap();
        assert!(
            row.contains("cacheR 13.6M"),
            "the dominant bucket is not named: {row:?}"
        );
        assert!(
            !row.contains("13900820") && !row.contains("13.9M"),
            "the blended total reached the terminal: {row:?}"
        );
    }

    /// #352: a narrow terminal never hides a bucket that was asked for.
    ///
    /// On the shared fitter the four token columns were first made droppable,
    /// and at 60 columns `report --metrics tokens_in` drew CACHER and CACHEW
    /// and silently lost IN and OUT: the very bucket the flag named, and half
    /// of D48(a)'s "four, never blended". A number is not a column to give way.
    /// The key does, down to its floor, and past that the row overflows.
    #[test]
    fn a_narrow_report_keeps_every_bucket_it_was_asked_for() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
        let metrics = vec!["tokens_in".to_string()];
        let out = report(
            &ctx,
            &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H",
                  "tokens_in": 136, "tokens_out": 83_479,
                  "tokens_cache_read": 13_630_240, "tokens_cache_creation": 186_965 }
            ] }),
            "project",
            Some(&metrics),
        );
        let header = out.lines().next().unwrap();
        for (_, short, _) in crate::tokens::BUCKETS {
            assert!(
                header.contains(&short.to_uppercase()),
                "{short} was dropped at 60 columns: {header:?}"
            );
        }
    }

    /// #352: the report's footnotes wrap at word boundaries to the terminal.
    ///
    /// The TOKENS legend is one 171-cell sentence, which a 60-column terminal
    /// broke mid-word on its own, three times.
    #[test]
    fn a_narrow_reports_footnotes_wrap_at_words() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
        let out = report(
            &ctx,
            &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H", "tokens_cache_read": 13_630_240 }
            ], "tokens_excluded_cancelled_tasks": 3 }),
            "project",
            None,
        );
        assert!(out.contains("largest of four"), "no legend: {out}");
        assert!(out.contains("--all includes"), "no exclusion note: {out}");
        for line in out.lines() {
            assert!(width(line) <= 60, "{} cells at 60: {line:?}", width(line));
        }
    }

    /// #217: `report.summary` carries a `tokens_confidence` field alongside
    /// the four buckets — the group's worst measurement — but the terminal
    /// table only ever named the dominant bucket, so a project whose entire
    /// spend came from a low-confidence discovery pass read identically to
    /// one built from verified `otel` correlations.
    #[test]
    fn report_marks_a_low_confidence_group() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = report(
            &ctx,
            &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H", "tokens_in": 100, "tokens_out": 0,
                  "tokens_cache_read": 0, "tokens_cache_creation": 0,
                  "tokens_confidence": "low" }
            ] }),
            "project",
            None,
        );
        let row = out.lines().nth(1).unwrap();
        assert!(
            row.contains("low"),
            "the group's low confidence never reached the terminal row: {row:?}"
        );
    }

    /// A group with no measurement must read as "nothing to report", not as a
    /// bucket that spent zero — the difference between an unmeasured project and
    /// a free one.
    #[test]
    fn report_shows_a_dash_for_a_group_that_spent_no_tokens() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = report(
            &ctx,
            &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H" }
            ] }),
            "project",
            None,
        );
        let row = out.lines().nth(1).unwrap();
        assert!(row.trim_end().ends_with('-'), "expected a dash: {row:?}");
    }

    // ========================================================================
    // Regression tests — tasqx audit 2026-09 (#212, #234)
    // ========================================================================

    /// #234 item 2: a project name past the old hardcoded 20-cell column must
    /// not shift every following column, and every row (long name or short)
    /// must hold the SAME width so a straight-down scan of COUNT/EST/OVERDUE/
    /// TRACKED is possible — the exact repro from the field
    /// (`code-review-2026-07` 19 cells, `eblinqx-claude-plugins` 22).
    #[test]
    fn report_column_does_not_shift_for_a_name_past_the_old_fixed_width() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let groups = json!({ "groups": [
            { "project": "code-review-2026-07", "count": 38, "est_total": "PT90H15M",
              "overdue": 0, "tracked_total": "PT0S" },
            { "project": "eblinqx-claude-plugins", "count": 3, "est_total": "PT7H",
              "overdue": 0, "tracked_total": "PT0S" },
            { "project": "fin-10034", "count": 1, "est_total": "PT0S",
              "overdue": 0, "tracked_total": "PT0S" },
        ] });
        let out = report(&ctx, &groups, "project", None);
        let rows: Vec<&str> = out.lines().skip(1).take(3).collect();
        // Every row's TOTAL display width must match: that is exactly "every
        // fixed-width column starts and ends in the same place", regardless
        // of how a right-justified number happens to sit inside its own
        // field (a 1-digit count naturally starts one cell later than a
        // 2-digit one in the SAME 5-cell field — that is correct alignment,
        // not a shift).
        let want = cells(rows[0]);
        for row in &rows {
            assert_eq!(
                cells(row),
                want,
                "a row's total width differs — the columns are not aligned: {row:?}\n{out}"
            );
        }
    }

    /// #234 item 11: the terminal used to print raw ISO-8601 (`PT5H53S`,
    /// which reads at a glance as 5h53m and is actually 5h and 53 SECONDS —
    /// an 87x error). It must use the same human duration the HTML report and
    /// the dashboard already use, and `-` (matching the TOKENS column) rather
    /// than `PT0S` for an absent value.
    #[test]
    fn report_prints_human_durations_not_iso8601() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let groups = json!({ "groups": [
            { "project": "p", "count": 1, "est_total": "PT1H30M", "overdue": 0,
              "tracked_total": "PT5H53S" },
        ] });
        let out = report(&ctx, &groups, "project", None);
        assert!(
            !out.contains("PT1H30M") && !out.contains("PT5H53S"),
            "raw ISO leaked: {out:?}"
        );
        assert!(out.contains("1h 30m"), "expected a human duration: {out:?}");
        assert!(
            out.contains("5h 53s") || out.contains("5h"),
            "expected a human duration for 5h53s: {out:?}"
        );

        let zero = json!({ "groups": [
            { "project": "p", "count": 1, "est_total": "PT0S", "overdue": 0, "tracked_total": "PT0S" },
        ] });
        let out = report(&ctx, &zero, "project", None);
        assert!(
            !out.contains("PT0S"),
            "PT0S leaked instead of a dash: {out:?}"
        );
    }

    /// #234 item 5: neither renderer had a totals row, forcing "how much
    /// estimated work is on the board" to be summed by hand across every
    /// group.
    #[test]
    fn report_carries_a_totals_row() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let groups = json!({ "groups": [
            { "project": "a", "count": 3, "est_total": "PT1H", "overdue": 1, "tracked_total": "PT30M" },
            { "project": "b", "count": 5, "est_total": "PT2H", "overdue": 0, "tracked_total": "PT1H" },
        ] });
        let out = report(&ctx, &groups, "project", None);
        assert!(out.contains("TOTAL"), "no totals row: {out:?}");
        let total_line = out.lines().find(|l| l.contains("TOTAL")).unwrap();
        assert!(
            total_line.contains('8'),
            "count total (3+5=8) missing: {total_line:?}"
        );
        assert!(
            total_line.contains('1'),
            "overdue total (1+0=1) missing: {total_line:?}"
        );
    }

    /// #212 (D48a, challenges-design): `--metrics` naming a token bucket must
    /// show all four as their own columns, and the terminal must otherwise
    /// carry a legend saying the single TOKENS cell is one bucket of four —
    /// today nothing on the page says that, and it is the number a lead reads
    /// weekly to ask "which project is burning the budget".
    #[test]
    fn report_metrics_flag_shows_all_four_buckets_and_dominant_mode_carries_a_legend() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let groups = json!({ "groups": [
            { "project": "budget-a", "count": 1, "est_total": "PT0S", "overdue": 0,
              "tracked_total": "PT0S", "tokens_in": 50_000, "tokens_out": 200_000,
              "tokens_cache_read": 300_000, "tokens_cache_creation": 40_000 },
        ] });

        let dominant = report(&ctx, &groups, "project", None);
        assert!(
            dominant.to_lowercase().contains("largest of four buckets"),
            "expected a legend explaining the TOKENS cell: {dominant:?}"
        );

        let metrics = vec!["tokens_in".to_string(), "tokens_out".to_string()];
        let all_four = report(&ctx, &groups, "project", Some(&metrics));
        for label in ["CACHER", "CACHEW", "IN", "OUT"] {
            assert!(
                all_four.contains(label),
                "expected a {label} column with --metrics: {all_four:?}"
            );
        }
        assert!(
            all_four.contains("200.0K") && all_four.contains("50.0K"),
            "expected the out/in counts as their own cells: {all_four:?}"
        );
    }

    /// #234 item 12: a cancelled task's spend must not vanish from the
    /// default report with nothing saying it was excluded.
    #[test]
    fn report_footnotes_cancelled_spend_excluded_by_default() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "groups": [
                { "project": "budget-a", "count": 1, "est_total": "PT0S", "overdue": 0,
                  "tracked_total": "PT0S" },
            ],
            "tokens_excluded_cancelled_tasks": 3,
        });
        let out = report(&ctx, &result, "project", None);
        assert!(
            out.contains('3') && out.to_lowercase().contains("cancelled"),
            "expected a footnote naming the 3 excluded cancelled tasks: {out:?}"
        );

        let clean = json!({
            "groups": [
                { "project": "budget-a", "count": 1, "est_total": "PT0S", "overdue": 0,
                  "tracked_total": "PT0S" },
            ],
            "tokens_excluded_cancelled_tasks": 0,
        });
        let out = report(&ctx, &clean, "project", None);
        assert!(
            !out.to_lowercase().contains("cancelled"),
            "no footnote should print when nothing was excluded: {out:?}"
        );
    }

    /// Truncation has to cut on a GRAPHEME boundary and budget in cells. Half a
    /// ZWJ sequence is not a shorter emoji — it is a different one, or a lone
    /// joiner the terminal draws as tofu, and it still overflows the column.
    ///
    /// Asserted through `cell` — the function the table actually builds its
    /// columns with — rather than through the truncation helper underneath it,
    /// so a correct helper called with the wrong budget still fails here.
    #[test]
    fn truncation_budgets_cells_and_never_splits_a_cluster() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466}";
        for unicode in [true, false] {
            let ctx = Ctx::new(
                theme::default_theme(),
                theme::Caps {
                    depth: theme::ColorDepth::None,
                    ansi: false,
                    unicode,
                },
            );
            // Long enough to force a cut, with the cut landing inside a cluster.
            let s = format!("ab{family}{family}cd");
            let got = cell(&ctx, None, &s, 7);
            assert_eq!(
                cells(&got),
                7,
                "cell budget blown (unicode={unicode}): {got:?}"
            );
            assert!(
                !got.ends_with('\u{200d}'),
                "cut left a dangling joiner (unicode={unicode}): {got:?}"
            );
            // A CJK string: 5 ideographs are 10 cells, so 6 cells must fit at
            // most 2 of them plus the ellipsis — a char-counting truncate keeps 5.
            let cjk = cell(&ctx, None, "漢字テスト", 6);
            assert_eq!(
                cells(&cjk),
                6,
                "CJK cell budget blown (unicode={unicode}): {cjk:?}"
            );
            // A string already inside its budget is padded, not cut.
            assert_eq!(
                cell(&ctx, None, "中文", 6),
                "中文  ",
                "short cell should be padded to 6 cells"
            );
        }
    }

    /// `tasqx why` printed `age             -0.00`.
    ///
    /// The age term is `(-age_days).max(0.0)`, and when `created` falls in the
    /// very second the clock is read, `age_days` is `0.0`, so the negation is
    /// `-0.0` — a value that compares EQUAL to zero while keeping its sign bit,
    /// which `{:.2}` then faithfully renders with a minus in front. A reader
    /// cannot act on "minus zero": it says a term subtracted urgency when it
    /// contributed none.
    ///
    /// Both spellings are covered because they are separate format calls with
    /// separate precisions — the component rows at 2 decimals and the total
    /// (which appears TWICE, in the heading and in the `= total` row) at 1. A
    /// fix applied to one of them leaves the other printing `-0`.
    #[test]
    fn why_never_renders_a_component_or_a_total_as_negative_zero() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let part = |name: &'static str, v: f64| (name, String::new(), v);
        let out = why_table(
            &ctx,
            &[
                part("priority", 0.0),
                part("deadline", 0.0),
                part("age", -0.0),
            ],
        );
        assert!(
            !out.contains("-0"),
            "a component rendered as negative zero: {out:?}"
        );
        assert!(
            out.contains("0.0"),
            "the zero itself must still be shown: {out:?}"
        );

        // Every part negative-zero makes the SUM negative zero too, which is the
        // total row — the twin the component fix does not reach.
        let all_neg = why_table(&ctx, &[part("priority", -0.0), part("age", -0.0)]);
        assert!(
            !all_neg.contains("-0"),
            "the total rendered as negative zero: {all_neg:?}"
        );

        // The rule is about a sign that survived ROUNDING, not about the value
        // being exactly zero: -0.004 is genuinely negative and still prints as a
        // row of zeros, so `v == 0.0` would not have caught it.
        let tiny = why_table(&ctx, &[part("age", -0.004)]);
        assert!(
            !tiny.contains("-0"),
            "a rounded-to-zero negative kept its sign: {tiny:?}"
        );
    }

    /// The twin of the above, and the reason it is not spelled `.abs()`: a term
    /// that really is negative must keep its minus. Nothing in today's formula
    /// produces one, but the formula is D1's to change and a display that
    /// silently drops signs would report the change wrong.
    #[test]
    fn why_keeps_the_sign_of_a_value_that_is_actually_negative() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = why_table(
            &ctx,
            &[
                ("penalty", String::new(), -1.5),
                ("priority", String::new(), 6.0),
            ],
        );
        assert!(
            out.contains("-1.5"),
            "a real negative lost its sign: {out:?}"
        );
        assert!(out.contains("4.5"), "the total must still net out: {out:?}");
    }

    /// Finding #6 (audit-2026-09), kept at one decimal (D122): the shares
    /// printed must add up to the total printed. Rounded independently,
    /// `3.95 + 11.45` reads `4.0 + 11.5` over a total of `15.4`; apportioned by
    /// largest remainder the shares are `3.9 + 11.5`, and the total is still
    /// the `15.4` every other screen prints for the task.
    #[test]
    fn why_shares_add_up_to_the_total_at_one_decimal() {
        for parts in [
            vec![3.95, 11.45],
            vec![6.0, 12.0, 0.09],
            vec![3.9, 11.52, 0.33],
            vec![1.8, 0.05, 0.05, 0.05],
        ] {
            let shares = apportion(&parts);
            let sum: f64 = shares.iter().sum();
            let total: f64 = parts.iter().sum();
            assert!(
                (sum - (total * 10.0).round() / 10.0).abs() < 1e-9,
                "{parts:?} -> {shares:?} sums to {sum}, not {total:.1}"
            );
        }
    }

    /// Finding #8 (audit-2026-09): `why` explained a blocked task's urgency
    /// and never mentioned that `next` will skip it — the least relevant half
    /// of the answer, with the fact that decides actionability left unsaid.
    #[test]
    fn why_names_the_unmet_blockers_and_that_next_skips_the_task() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = why(
            &ctx,
            &json!({
                "short_id": 279,
                "priority": "H",
                "due": null,
                "created": "2026-09-09T00:00:00Z",
                "blocked": true,
                "unmet_blockers": [{ "short_id": 280, "title": "the blocker" }],
            }),
            Timestamp::now(),
        );
        assert!(out.contains("#280"), "{out:?}");
        assert!(out.contains("the blocker"), "{out:?}");
        assert!(out.contains("next"), "must say `next` skips it: {out:?}");

        // Not blocked: no such line at all.
        let out = why(
            &ctx,
            &json!({
                "short_id": 1,
                "priority": "H",
                "due": null,
                "created": "2026-09-09T00:00:00Z",
                "blocked": false,
                "unmet_blockers": [],
            }),
            Timestamp::now(),
        );
        assert!(
            !out.contains("blocked by"),
            "an unblocked task must not claim a blocker: {out:?}"
        );
    }

    /// D50: the recompute delta lists every CHANGED task with auditable raw
    /// numbers, counts the unchanged ones instead of listing them, and names
    /// `--apply` only while nothing has been written yet.
    #[test]
    fn tokens_recompute_lists_changes_and_names_apply_only_on_dry_run() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let b4 = |i: i64, o: i64| {
            json!({ "input_tokens": i, "output_tokens": o,
                    "cache_read_tokens": 0, "cache_creation_tokens": 0 })
        };
        let mut result = json!({
            "dry_run": true,
            "tasks": [
                { "task": 3, "action": "recomputed", "before": b4(1500, 2600), "after": b4(500, 600) },
                { "task": 4, "action": "downgraded", "before": b4(800, 900), "after": b4(800, 900) },
                { "task": 5, "action": "channel_conflict", "before": b4(70, 0), "after": serde_json::Value::Null },
                { "task": 6, "action": "unchanged", "before": b4(1, 1), "after": b4(1, 1) },
            ],
            // The engine's blended figure is still ignored by the renderer
            // (#219) — present here to prove the totals line does not read it.
            "totals": { "before": 7101, "after": 1101 },
        });

        let out = tokens_recompute(&ctx, &result);
        assert!(out.contains("#3"), "{out:?}");
        assert!(
            out.contains("in 1500->500") && out.contains("out 2600->600"),
            "raw auditable numbers, no compaction: {out:?}"
        );
        assert!(
            out.contains("confidence -> low"),
            "the downgrade must say what happens to the label: {out:?}"
        );
        assert!(
            out.contains("self-report"),
            "a channel conflict must say which measurement stands: {out:?}"
        );
        assert!(
            !out.contains("#6"),
            "an unchanged task earns no line of its own: {out:?}"
        );
        assert!(out.contains("1 unchanged"), "{out:?}");
        // #219: the totals line sums the four buckets across every task
        // (1500+800+70+1 in, 2600+900+0+1 out; 500+800+0+1 / 600+900+0+1
        // after) — never the engine's blended 7101 -> 1101 figure above.
        assert!(
            !out.contains("7101") && !out.contains("blended"),
            "the totals line must not carry the engine's blended figure: {out:?}"
        );
        assert!(
            out.contains("in 2371->1301") && out.contains("out 3501->1501"),
            "the totals line must sum the four buckets, not blend them: {out:?}"
        );
        assert!(
            out.contains("--apply"),
            "a dry-run must name the flag that makes it real: {out:?}"
        );

        result["dry_run"] = json!(false);
        let out = tokens_recompute(&ctx, &result);
        assert!(
            !out.contains("--apply"),
            "an applied run advertising --apply invites running it twice: {out:?}"
        );
        assert!(out.contains("applied"), "{out:?}");
    }

    /// #219: the totals line used to blend all four buckets into the single
    /// aggregate `tokens.rs`'s own header exists to forbid, and the blend
    /// cannot even detect the failure it is there to catch — a repair that
    /// drops 200k OUTPUT tokens and adds 200k CACHE-READ tokens nets to the
    /// same blended figure on both sides and reads as a no-op, while having
    /// swapped the cheapest bucket for one of the most expensive.
    #[test]
    fn tokens_recompute_totals_never_blend_the_four_buckets() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let bucket = |i: i64, o: i64, cr: i64, cw: i64| {
            json!({ "input_tokens": i, "output_tokens": o,
                    "cache_read_tokens": cr, "cache_creation_tokens": cw })
        };
        let result = json!({
            "dry_run": true,
            "tasks": [
                {
                    "task": 20,
                    "action": "recomputed",
                    "before": bucket(0, 200_000, 0, 0),
                    "after": bucket(0, 0, 200_000, 0),
                },
            ],
            // The blended figure nets to the SAME number on both sides for
            // exactly this swap — the reading the totals line must not give.
            "totals": { "before": 200_000, "after": 200_000 },
        });

        let out = tokens_recompute(&ctx, &result);
        let totals_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("totals"))
            .unwrap_or_else(|| panic!("no totals line: {out:?}"));
        assert!(
            !totals_line.contains("blended"),
            "the totals line must not blend the four buckets: {totals_line:?}"
        );
        assert!(
            totals_line.contains("out 200000->0") && totals_line.contains("cacheR 0->200000"),
            "the totals line must show the real per-bucket change, not a blended no-op: \
             {totals_line:?}"
        );
    }

    /// Nothing in scope is an answer, not an empty table — and an action this
    /// build has never heard of still reaches the reader, because this report
    /// is the approval surface for a deletion.
    #[test]
    fn tokens_recompute_answers_when_empty_and_shows_unknown_actions() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = tokens_recompute(&ctx, &json!({ "dry_run": true, "tasks": [] }));
        assert!(out.contains("No log-parse measurements"), "{out:?}");

        let out = tokens_recompute(
            &ctx,
            &json!({
                "dry_run": true,
                "tasks": [ { "task": 9, "action": "quarantined",
                             "before": { "input_tokens": 5 }, "after": { "input_tokens": 5 } } ],
                "totals": { "before": 5, "after": 5 },
            }),
        );
        assert!(
            out.contains("quarantined") && out.contains("#9"),
            "an unknown action may not drop its task from the report: {out:?}"
        );
    }

    #[test]
    fn truncate_uses_ascii_ellipsis_without_unicode() {
        let long = "a".repeat(50);
        let uni = truncate(&long, 10, true);
        assert!(uni.ends_with('…') && !uni.contains("..."));
        let ascii = truncate(&long, 10, false);
        assert!(ascii.ends_with("...") && !ascii.contains('…'));
        assert!(ascii.is_ascii(), "no non-ASCII in plain path");
        assert_eq!(ascii.chars().count(), 10);
    }

    // ---- undone (the line that says what undo actually did) -----------------

    /// The whole point of the line: `undo` takes no argument, so unless it names
    /// the operation, the task and what came back, the user has no way to check
    /// that it reversed the thing they meant. A bare "undone" is the failure.
    #[test]
    fn the_undo_line_names_the_operation_the_task_and_what_came_back() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "reverted": { "event": "e1", "op": "tag.remove", "ts": "2026-08-03T10:00:00Z" },
            "short_id": 42,
            "title": "Ship v1",
            "restored": { "tags": ["api", "release"] },
        });
        let out = undone(&ctx, &result, &result, &Titles::new(), Timestamp::now());
        // D126 names the operation by the verb that did it: `untag`.
        assert!(
            out.contains("undid untag"),
            "the line must name the operation that was reversed: {out:?}"
        );
        assert!(
            out.contains("#42"),
            "the line must name the task it acted on: {out:?}"
        );
        assert!(
            out.contains("Ship v1"),
            "the line must carry the title — a short_id alone is not recognizable at a \
             glance, and undo took no argument to echo back: {out:?}"
        );
        assert!(
            out.contains("+api") && out.contains("+release"),
            "the line must name what came back, or it says nothing an undo that \
             restored nothing would not also say: {out:?}"
        );
    }

    /// One phrasing per op in the core's closed set, selected by the reverted op
    /// rather than by sniffing which keys `restored` happens to carry — and a
    /// fallback that still prints the payload, because an op this build has no
    /// phrasing for must not reach the terminal as a blank line reading "it
    /// restored nothing".
    #[test]
    fn every_undoable_op_gets_a_line_that_says_what_it_restored() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let line = |op: &str, restored: Value| -> String {
            let result = json!({
                "reverted": { "event": "e1", "op": op, "ts": "t" },
                "short_id": 7,
                "title": "t",
                "restored": restored,
            });
            // The task as read back after the revert: running again for a stop.
            let mut task = result.clone();
            if op == "stop" {
                task["status"] = json!("active");
            }
            undone(&ctx, &result, &task, &Titles::new(), Timestamp::now())
        };

        assert!(line("dependency.remove", json!({ "depends_on": 3 })).contains("#3"));
        // D126: `*` in the rail says it runs again, and the line says how long
        // it has been running — elapsed, not an instant, because `due_cell`'s
        // clock is UTC and read as the wall clock (D126 l); `restored.tracked`
        // is the interval put back, not a total.
        let stopped = line(
            "stop",
            json!({ "tracked": "PT30M", "status": "active",
                    "interval_started": "2026-09-09T10:00:00Z" }),
        );
        assert!(
            stopped.contains("* undid stop") && stopped.contains(" for "),
            "{stopped:?}"
        );
        let noted = line(
            "annotation.add",
            json!({ "annotation": "called the plumber" }),
        );
        assert!(noted.contains("called the plumber"), "{noted:?}");

        // The core's closed set can grow; this renderer must degrade to showing
        // the data rather than to showing nothing.
        let unknown = line("some.future.op", json!({ "whatever": 1 }));
        assert!(
            unknown.contains("whatever"),
            "an op with no phrasing must still print what it restored: {unknown:?}"
        );
    }

    /// An annotation body and a tag are untrusted text — argv, `store.import`,
    /// an MCP client — and this line goes straight to a terminal.
    #[test]
    fn undone_sanitizes_control_bytes_in_the_text_it_echoes() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "reverted": { "event": "e1", "op": "annotation.add", "ts": "t" },
            "short_id": 1,
            "title": "\u{1b}[2Jclear",
            "restored": { "annotation": "\u{1b}]0;evil\u{7}note" },
        });
        let out = undone(&ctx, &result, &result, &Titles::new(), Timestamp::now());
        assert!(
            !out.contains('\u{1b}'),
            "escape byte reached the terminal: {out:?}"
        );
        assert!(
            !out.contains('\u{7}'),
            "bell byte reached the terminal: {out:?}"
        );
    }

    // ---- tag_result (D39: what changed AND what remains) --------------------

    /// The line a removal prints must name the tag that went. Rendering only
    /// `tags` — the set that REMAINS — produces `#1 tags: +release` for a real
    /// removal and the same string for a call that removed nothing, which is
    /// the whole failure `dep_result` above was written to avoid, one noun over.
    #[test]
    fn an_untag_line_names_what_went_and_what_remains() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({ "short_id": 1, "tags": ["release"], "removed": ["api"] });
        let out = tag_changed(
            &ctx,
            &result,
            &result,
            false,
            &["api".to_string()],
            Timestamp::now(),
        );
        assert!(out.contains("untagged"), "{out:?}");
        assert!(out.contains("+api"), "the removed tag must appear: {out:?}");
        assert!(
            out.contains("+release"),
            "the remaining set must appear: {out:?}"
        );
    }

    /// The addition half, and the empty case. `tag.add` returns no `removed`
    /// key, so the changed set comes from the request there. A task left with
    /// no tags used to print `tags: (none)` so the label was not followed by a
    /// blank; D126 has no label to leave blank, and a zero is not a fact, so
    /// the line names what went and stops.
    #[test]
    fn a_tag_line_names_the_added_tag_and_an_empty_set_says_so() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let now = Timestamp::now();
        let added = json!({ "short_id": 7, "tags": ["api", "release"] });
        let out = tag_changed(&ctx, &added, &added, true, &["api".to_string()], now);
        assert!(out.contains("#7") && out.contains("tagged"), "{out:?}");
        assert!(out.contains("+api") && out.contains("+release"), "{out:?}");
        assert!(!out.contains("untagged"), "the verb must not flip: {out:?}");

        let emptied = json!({ "short_id": 7, "tags": [], "removed": ["api"] });
        let out = tag_changed(&ctx, &emptied, &emptied, false, &["api".to_string()], now);
        assert!(out.contains("untagged   +api"), "{out:?}");
        assert!(
            !out.contains("(none)") && !out.contains("tags:"),
            "no label left blank and no placeholder: {out:?}"
        );
    }

    /// A tag is untrusted text: it comes from argv, from `store.import` and from
    /// an MCP client, and this line goes straight to a terminal. Every other
    /// renderer in this file runs its values through `san` for that reason.
    #[test]
    fn tag_result_sanitizes_control_bytes_in_a_tag_name() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "short_id": 1,
            "tags": ["]0;evilsafe"],
            "removed": ["[2Jgone"],
        });
        let out = tag_changed(&ctx, &result, &result, false, &[], Timestamp::now());
        assert!(
            !out.contains(''),
            "escape byte reached the terminal: {out:?}"
        );
        assert!(
            !out.contains(''),
            "bell byte reached the terminal: {out:?}"
        );
    }

    // ---- agenda ------------------------------------------------------------

    /// 2026-08-03 is a Monday. Every agenda test anchors to it, so "Today" and
    /// "Tomorrow" are facts about the fixture rather than about the day the
    /// suite happens to run.
    const ANCHOR: &str = "2026-08-03T09:00:00Z";

    fn anchor() -> Timestamp {
        ANCHOR.parse().expect("the anchor is a real instant")
    }

    /// A task carrying whichever of the two dated fields the case is about.
    /// An empty `due`/`scheduled` means the field is absent, which is what the
    /// engine emits as `null` and what `field_ts` reads the same way.
    fn dated(id: i64, title: &str, due: &str, scheduled: &str) -> Value {
        json!({
            "short_id": id, "urgency": 5.0, "priority": "M", "title": title,
            "project": "p", "tags": [], "status": "pending",
            "due": if due.is_empty() { Value::Null } else { json!(due) },
            "scheduled": if scheduled.is_empty() { Value::Null } else { json!(scheduled) },
        })
    }

    /// The payload half of the old `agenda_of`: the agenda borrows its rows
    /// now, so the `task.list` answer must outlive it — callers bind this
    /// first, exactly as `run_agenda` does.
    fn agenda_payload(tasks: Vec<Value>) -> Value {
        json!({ "tasks": tasks })
    }

    fn agenda_of(payload: &Value, days: usize) -> Agenda<'_> {
        agenda_select(payload, days, anchor())
    }

    fn agenda_out(tasks: Vec<Value>, days: usize) -> String {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let payload = agenda_payload(tasks);
        agenda_text(&ctx, &agenda_of(&payload, days))
    }

    /// The ordering rule, and the reason the view reads both fields.
    ///
    /// A task scheduled for Tuesday with a deadline three weeks out comes
    /// BEFORE a task due on Wednesday, because Tuesday is the first day it asks
    /// anything of you. Reading `due` alone would sort it last and file it in
    /// the wrong week; reading `scheduled` alone would lose the Wednesday
    /// deadline entirely. Both halves are asserted here, plus the label, since
    /// "Tuesday" means something different under each field.
    #[test]
    fn agenda_places_a_task_on_the_earlier_of_due_and_scheduled() {
        let payload = agenda_payload(vec![
            dated(1, "deadline only", "2026-08-05T00:00:00Z", ""),
            dated(
                2,
                "starts tuesday, due much later",
                "2026-08-24T00:00:00Z",
                "2026-08-04T00:00:00Z",
            ),
            dated(3, "scheduled only", "", "2026-08-10T00:00:00Z"),
        ]);
        let a = agenda_of(&payload, 30);
        let order: Vec<i64> = a
            .entries
            .iter()
            .map(|e| e.task["short_id"].as_i64().unwrap())
            .collect();
        assert_eq!(
            order,
            vec![2, 1, 3],
            "the agenda instant is min(due, scheduled), so #2's Tuesday leads"
        );
        assert_eq!(
            a.entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![When::Scheduled, When::Due, When::Scheduled],
            "the row must name the field that placed it"
        );
    }

    /// When one instant carries both meanings the label is `due`: the deadline
    /// is the more consequential reading, and a row that said `sched` would
    /// send the reader looking for a deadline that is right there.
    #[test]
    fn a_task_due_and_scheduled_at_the_same_instant_is_labelled_due() {
        let payload = agenda_payload(vec![dated(
            1,
            "same instant",
            "2026-08-05T09:00:00Z",
            "2026-08-05T09:00:00Z",
        )]);
        let a = agenda_of(&payload, 30);
        assert_eq!(a.entries[0].kind, When::Due);
    }

    /// The rule that keeps this view from being a lie: a task the filter
    /// matched and the agenda cannot place is COUNTED, not dropped. A deadline
    /// that is not on the screen is indistinguishable from a deadline that does
    /// not exist, and the undated are the largest group this view omits.
    #[test]
    fn an_undated_task_is_counted_and_the_way_to_see_it_is_named() {
        let out = agenda_out(
            vec![
                dated(1, "on a day", "2026-08-05T00:00:00Z", ""),
                dated(2, "no dates at all", "", ""),
                dated(3, "no dates either", "", ""),
            ],
            14,
        );
        assert!(
            !out.contains("no dates at all"),
            "an undated task has no day to sit on: {out}"
        );
        assert!(
            out.contains("2 undated"),
            "the count of what was left out must be on the screen: {out}"
        );
        assert!(
            out.contains("tasqx list"),
            "the note must name the view that does show them: {out}"
        );
    }

    /// The horizon half of the same rule, plus the part that makes it
    /// actionable: the note names the exact `--days` that reaches the furthest
    /// thing it is holding, so widening the window is a paste and not a guess.
    #[test]
    fn a_task_past_the_horizon_is_counted_with_the_days_that_would_reach_it() {
        let tasks = vec![
            dated(1, "inside", "2026-08-05T00:00:00Z", ""),
            dated(2, "just outside", "2026-08-18T00:00:00Z", ""),
            dated(3, "far outside", "2026-11-01T00:00:00Z", ""),
        ];
        let out = agenda_out(tasks.clone(), AGENDA_DEFAULT_DAYS);
        assert!(!out.contains("just outside"), "{out}");
        assert!(out.contains("2 further out"), "{out}");
        // 2026-08-03 -> 2026-11-01 is 90 days. Anything short of the real
        // distance is a note that sends the reader back for another guess.
        assert!(
            out.contains("--days 90"),
            "the note must name the window that reaches the furthest row: {out}"
        );
        // ...and the horizon really is a fortnight by default, stated on screen.
        assert!(out.contains("through 17 Aug (+14d)"), "{out}");

        // Raising it brings them in, which is the other half of the promise.
        let wide = agenda_out(tasks, 90);
        assert!(wide.contains("far outside"), "{wide}");
        assert!(
            !wide.contains("further out"),
            "nothing is beyond a horizon that reaches everything: {wide}"
        );
    }

    /// An agenda row does not repeat the day its own heading just named.
    ///
    /// A deadline the store holds no time for, placed under `Tomorrow · Fri
    /// 2026-08-04`, printed the bare word `due` — and three rows of it in a
    /// column read as a column that failed to render rather than one saying
    /// anything. `sched` still prints bare: being on a day because you meant to
    /// START there is not the default reading of a row.
    #[test]
    fn an_agenda_row_does_not_repeat_the_day_its_heading_names() {
        let out = agenda_out(
            vec![
                dated(1, "no time on it", "2026-08-05T00:00:00Z", ""),
                dated(2, "planned for then", "", "2026-08-06T00:00:00Z"),
                dated(3, "at a real time", "2026-08-05T17:00:00Z", ""),
            ],
            14,
        );
        let row = |title: &str| {
            out.lines()
                .find(|l| l.contains(title))
                .unwrap_or_else(|| panic!("no row for {title:?}:\n{out}"))
                .to_string()
        };
        assert!(
            !row("no time on it").contains("due"),
            "the heading already said the day: {:?}",
            row("no time on it")
        );
        assert!(
            row("planned for then").contains("sched"),
            "`sched` is not the default reading and still says so: {:?}",
            row("planned for then")
        );
        assert!(
            row("at a real time").contains("due 17:00"),
            "a time the store holds is still shown: {:?}",
            row("at a real time")
        );
    }

    /// D131: `list` and `agenda` answer "how many are overdue" with one rule.
    /// Found by rendering a store: a task due at 17:00 read `today 17:00` in
    /// red with `2 overdue` in `list`, while `agenda` filed it under `Today`
    /// and said `1 overdue` — `list` compared instants and `agenda` compared
    /// days. A deadline with a time is late once it passes; a date-only one
    /// (midnight UTC) is late once its day has.
    #[test]
    fn list_and_agenda_agree_on_what_is_overdue_today() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let payload = json!({
            "tasks": [
                dated(1, "missed this morning", "2026-08-03T08:00:00Z", ""),
                dated(2, "due today no time", "2026-08-03T00:00:00Z", ""),
                dated(3, "due yesterday no time", "2026-08-02T00:00:00Z", ""),
                dated(4, "due this evening", "2026-08-03T17:00:00Z", ""),
            ],
            "count": 4,
            "total": 4,
        });
        let listed = task_table(&ctx, &payload, anchor());
        let agenda = agenda_text(&ctx, &agenda_of(&payload, 14));
        let head = |out: &str| out.lines().next().unwrap_or_default().to_string();

        assert!(head(&listed).contains("2 overdue"), "list:\n{listed}");
        assert!(head(&agenda).contains("2 overdue"), "agenda:\n{agenda}");
        assert!(
            head(&listed).contains("2 due today"),
            "the date-only deadline is still due today:\n{listed}"
        );

        let lines: Vec<&str> = agenda.lines().collect();
        let at = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} missing:\n{agenda}"))
        };
        let today = at("Today");
        assert!(
            at("missed this morning") < today,
            "a deadline that passed this morning is in the Overdue group:\n{agenda}"
        );
        assert!(
            at("due today no time") > today,
            "a date-only deadline today stays under Today:\n{agenda}"
        );
    }

    /// D131 on the two single-task screens: `why` and the `show` card said
    /// `overdue` for a date-only deadline from one second past midnight.
    #[test]
    fn why_and_the_card_do_not_call_a_date_only_deadline_overdue_on_its_own_day() {
        let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
        let plain = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let task = |due: &str| {
            json!({
                "short_id": 48, "title": "Renew the TLS certificate", "priority": "H",
                "urgency": 18.0, "due": due, "created": "2026-09-02T10:00:00Z",
                "blocked": false,
                "urgency_breakdown": { "priority": 6.0, "due_proximity": 12.0, "age": 0.0 }
            })
        };
        let date_only = why(&plain, &task("2026-09-11T00:00:00Z"), now);
        assert!(!date_only.contains("overdue"), "{date_only}");
        assert!(date_only.contains("due today"), "{date_only}");
        let missed = why(&plain, &task("2026-09-11T09:00:00Z"), now);
        assert!(missed.contains("overdue, today"), "{missed}");

        let color = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: theme::ColorDepth::Truecolor,
                ansi: true,
                unicode: true,
            },
        );
        let mut t = detail_fixture();
        t["due"] = json!("2026-09-11T00:00:00Z");
        let card = task_detail(&color, &t, now);
        assert!(
            !card.contains(&color.paint("overdue", "today")),
            "a date-only deadline today is not painted overdue: {card:?}"
        );
    }

    /// Overdue rows lead the table, under ONE heading, and the horizon does not
    /// apply to them: `--days 1` must not hide work that was due last month.
    /// The cells in that group carry the date, because the heading cannot.
    #[test]
    fn overdue_rows_lead_the_table_and_ignore_the_horizon() {
        let out = agenda_out(
            vec![
                dated(1, "today thing", "2026-08-03T17:00:00Z", ""),
                dated(2, "late by a week", "2026-07-27T00:00:00Z", ""),
                dated(3, "late by a month", "2026-07-01T00:00:00Z", ""),
            ],
            1,
        );
        let lines: Vec<&str> = out.lines().collect();
        let overdue = lines
            .iter()
            .position(|l| l.trim() == "Overdue")
            .unwrap_or_else(|| panic!("no Overdue heading:\n{out}"));
        let today = lines
            .iter()
            .position(|l| l.starts_with("Today"))
            .unwrap_or_else(|| panic!("no Today heading:\n{out}"));
        assert!(overdue < today, "overdue comes first:\n{out}");
        assert_eq!(
            lines.iter().filter(|l| l.trim() == "Overdue").count(),
            1,
            "past days collapse into one heading:\n{out}"
        );
        assert!(out.contains("late by a month"), "{out}");
        // The day, not a bare `due`: how late it is is the whole content of
        // this group — spelled as `list` spells it (D133).
        assert!(out.contains("due 33d ago"), "{out}");
        assert!(out.contains("due 7d ago"), "{out}");
        // A time-of-day is shown only where the store has one. `2026-07-01` was
        // typed without a time and resolves to midnight, so printing `00:00`
        // would be a time nobody entered.
        assert!(!out.contains("00:00"), "{out}");
        assert!(out.contains("due 17:00"), "a real time is shown: {out}");
    }

    /// Today and tomorrow get their words, and every heading carries the date
    /// anyway -- "Today" alone is the one label that means something else
    /// tomorrow, and terminal output gets pasted into tickets. The date is
    /// `list`'s calendar spelling, not ISO (D133), with the two-digit year
    /// only once it leaves the current one.
    #[test]
    fn day_headings_name_the_relative_day_and_the_date() {
        let out = agenda_out(
            vec![
                dated(1, "a", "2026-08-03T00:00:00Z", ""),
                dated(2, "b", "2026-08-04T00:00:00Z", ""),
                dated(3, "c", "2026-08-06T00:00:00Z", ""),
                dated(4, "d", "2027-01-04T00:00:00Z", ""),
            ],
            200,
        );
        assert!(out.contains("\nToday · Mon 3 Aug\n"), "{out}");
        assert!(out.contains("\nTomorrow · Tue 4 Aug\n"), "{out}");
        assert!(out.contains("\nThu 6 Aug\n"), "{out}");
        assert!(out.contains("\nMon 4 Jan 27\n"), "{out}");
        assert!(
            out.starts_with("through 19 Feb 27 (+200d)"),
            "the horizon is spelled the same way: {out}"
        );
        assert!(
            !out.contains("2026-"),
            "no ISO date on the text agenda: {out}"
        );
        assert!(
            !out.contains("2027-"),
            "no ISO date on the text agenda: {out}"
        );
    }

    /// The Overdue group has no heading to carry a day, so its cells do — in
    /// `list`'s words (D133). It used to print `due 2026-08-02` beside a
    /// `list` that said `yesterday` for the same row.
    #[test]
    fn overdue_when_cells_speak_lists_calendar_words() {
        let out = agenda_out(
            vec![
                dated(1, "missed this morning", "2026-08-03T08:00:00Z", ""),
                dated(2, "due yesterday", "2026-08-02T00:00:00Z", ""),
                dated(3, "meant to start", "", "2026-08-01T00:00:00Z"),
            ],
            14,
        );
        let row = |title: &str| {
            out.lines()
                .find(|l| l.contains(title))
                .unwrap_or_else(|| panic!("{title:?} missing:\n{out}"))
                .to_string()
        };
        assert!(
            row("missed this morning").contains("due today 08:00"),
            "{out}"
        );
        assert!(row("due yesterday").contains("due yesterday"), "{out}");
        assert!(row("meant to start").contains("sched 2d ago"), "{out}");
        assert!(
            !out.contains("2026-"),
            "no ISO date on the text agenda: {out}"
        );
    }

    /// D133's guard: `list` and `agenda` spell the same day identically for
    /// the same row, because both go through one spelling. Asserted as
    /// agreement between the two renders rather than as either's text, so a
    /// third spelling on either side reddens it.
    #[test]
    fn list_and_agenda_spell_the_same_day_the_same_way() {
        let ctx = || Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(200);
        let rows = [
            (1, "late by two days", "2026-08-01T00:00:00Z"),
            (2, "missed at eight", "2026-08-03T08:00:00Z"),
            (3, "on thursday", "2026-08-06T00:00:00Z"),
            (4, "in a fortnight", "2026-08-15T00:00:00Z"),
            (5, "next year", "2027-01-04T00:00:00Z"),
        ];
        let payload = json!({
            "tasks": rows.iter().map(|(id, t, d)| dated(*id, t, d, "")).collect::<Vec<_>>(),
            "count": rows.len(),
        });
        let listed = task_table(&ctx(), &payload, anchor());
        let agenda = agenda_text(&ctx(), &agenda_of(&payload, 200));
        let line_of = |out: &str, title: &str| {
            out.lines()
                .position(|l| l.contains(title))
                .unwrap_or_else(|| panic!("{title:?} missing:\n{out}"))
        };
        for (_, title, due) in rows {
            let spelled = due_cell(due.parse().unwrap(), anchor());
            let l = listed.lines().nth(line_of(&listed, title)).unwrap();
            assert!(
                l.contains(&spelled),
                "list spells {due} as {spelled:?}: {l:?}"
            );

            let agenda_lines: Vec<&str> = agenda.lines().collect();
            let at = line_of(&agenda, title);
            let row = agenda_lines[at];
            if row.contains("due ") {
                // An Overdue row carries the day in its own cell.
                assert!(
                    row.contains(&format!("due {spelled}")),
                    "agenda must spell {due} as list does ({spelled:?}):\n{agenda}"
                );
            } else {
                // A row under a day heading: the heading carries the day.
                let heading = agenda_lines[..at]
                    .iter()
                    .rev()
                    .find(|l| !l.starts_with(' ') && !l.is_empty())
                    .unwrap();
                assert!(
                    heading.contains(spelled.as_str()),
                    "agenda heading {heading:?} must carry list's {spelled:?}:\n{agenda}"
                );
            }
        }
    }

    /// One layout for every group, and it is the D51 one: the columns are
    /// fitted once across all the rows, and nothing -- heading, row, rule or
    /// count line -- overruns the terminal it was given. A second table layout
    /// written for this view is exactly what this asserts is absent.
    #[test]
    fn the_agenda_fits_the_width_it_was_given_across_every_group() {
        for cols in [Ctx::MIN_COLS, 60, 80, Ctx::DEFAULT_COLS] {
            let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols);
            let tasks: Vec<Value> = (1..=6)
                .map(|i| {
                    json!({
                        "short_id": i, "urgency": 5.0, "priority": "M",
                        "title": "a title far too long to fit any of these budgets without a cut",
                        "project": "some.rather.long.project.name",
                        "tags": ["one", "two", "three", "four"],
                        "status": "pending",
                        "due": format!("2026-08-{:02}T17:00:00Z", i + 2),
                        "scheduled": Value::Null,
                    })
                })
                .collect();
            // Nothing here is undated or beyond the horizon, so the whole
            // render is table: the omission notes are deliberately prose that
            // carries a command to paste, and cutting one would cut the way out.
            let out = agenda_text(
                &ctx,
                &agenda_select(&json!({ "tasks": tasks }), 14, anchor()),
            );
            // EVERY line, and no bookkeeping to say where the table stops:
            // D117 left it with no rules at all, and the summary line that
            // opens it is width-bounded like the rows are (facts are dropped
            // from the right until it fits, since a wrap there would put a line
            // between the header and the rows it labels). What used to be
            // exempt was the trailer, which is gone. The fixture has no
            // omission notes, so nothing here is prose.
            for line in out.lines() {
                assert!(
                    cells(line) <= cols,
                    "a {}-cell line in a {cols}-cell terminal: {line:?}",
                    cells(line)
                );
            }
            assert!(
                !out.lines()
                    .any(|l| { !l.is_empty() && l.chars().all(|c| c == '-' || c == '\u{2500}') }),
                "the table draws no rules any more: {out}"
            );
        }
    }

    /// `--json` and the table answer the same question. The raw `task.list`
    /// result would have made `tasqx agenda --json | jq .tasks` count every
    /// matching task -- horizon and undated rows included -- while the table
    /// beside it showed one.
    #[test]
    fn agenda_json_holds_exactly_the_rows_the_table_drew() {
        let payload = agenda_payload(vec![
            dated(1, "shown", "2026-08-05T00:00:00Z", ""),
            dated(2, "beyond", "2026-11-01T00:00:00Z", ""),
            dated(3, "undated", "", ""),
        ]);
        let a = agenda_of(&payload, 14);
        let v = agenda_json(&a);
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(v["tasks"][0]["short_id"], json!(1));
        // Every number the footer prints is also a field, so a script never has
        // to scrape the prose to learn what was left out.
        assert_eq!(v["agenda"]["undated"], json!(1));
        assert_eq!(v["agenda"]["beyond_horizon"], json!(1));
        assert_eq!(v["agenda"]["reach_days"], json!(90));
        assert_eq!(v["agenda"]["through"], json!("2026-08-17"));
        assert_eq!(v["agenda"]["days"], json!(14));
        // The ceiling too, read out of the constant rather than typed: without
        // it a script has no way to tell a `reach_days` it can pass to `--days`
        // from one the parser would refuse.
        assert_eq!(v["agenda"]["max_days"], json!(AGENDA_MAX_DAYS));
    }

    /// Both status-less tables print the SAME store-health notes, because they
    /// call the same function.
    ///
    /// The defect this pins: `agenda` shipped as a second table over `list`'s
    /// rows and carried neither note. An unreadable status has no column to
    /// appear in and a blank title draws as an empty cell, so the row sat under
    /// a day heading indistinguishable from ordinary open work and the run
    /// exited 0 — the invisible-field failure rebuilt one view over. Asserting
    /// the two views AGREE, rather than asserting each one's text, is what makes
    /// this a guard: a third view that forgets the notes fails it too, and
    /// rewording a note cannot pass it on one side only.
    #[test]
    fn every_status_less_table_carries_the_same_store_health_notes() {
        let mut unreadable = dated(7, "important work", "2026-08-05T00:00:00Z", "");
        unreadable["status"] = json!("Done");
        unreadable["status_unrecognized"] = json!(true);
        let untitled = dated(4, "", "2026-08-06T00:00:00Z", "");
        let tasks = vec![unreadable, untitled];

        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let list = task_table(
            &ctx,
            &json!({ "tasks": tasks, "count": 2 }),
            Timestamp::now(),
        );
        let agenda = agenda_out(tasks.clone(), AGENDA_DEFAULT_DAYS);

        let notes = store_health_notes(&tasks);
        assert_eq!(
            notes.len(),
            2,
            "the fixture must trip both notes: {notes:?}"
        );
        // Compared with the line breaks folded into spaces: both views wrap
        // their notes to the terminal (#346), so a note longer than the width
        // is the same words over two lines, and agreement is about the words.
        let fold = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let (list, agenda) = (fold(&list), fold(&agenda));
        for note in &notes {
            let note = fold(note);
            assert!(
                list.contains(note.as_str()),
                "`list` dropped a store-health note:\n{list}"
            );
            assert!(
                agenda.contains(note.as_str()),
                "`agenda` draws the same rows in the same layout, so it owes the \
                 reader the same note:\n{agenda}"
            );
        }
        // Named ids, not just a count: "1 unreadable row" leaves the reader with
        // a store to search by hand.
        assert!(agenda.contains("#7 (Done)"), "{agenda}");
        assert!(agenda.contains("#4"), "{agenda}");
    }

    /// A row the horizon cut but no `--days` can reach: the footer must not hand
    /// the reader a command the CLI refuses.
    ///
    /// `--days` is bounded at `AGENDA_MAX_DAYS`, and the reach is a raw distance
    /// to the furthest cut row, so a task due decades out produced ``tasqx
    /// agenda --days 12204`` — pasted, that exits 2 with `12204 is not in
    /// 1..=3650`, and the row was unreachable in this view by any window.
    /// D53 rule 2 promises the count is unconditional and the advice actionable;
    /// past the ceiling the actionable advice is a different view.
    #[test]
    fn a_row_no_window_can_reach_is_counted_without_quoting_a_refused_days() {
        // Comfortably past the ceiling from the 2026-08-03 anchor.
        let out = agenda_out(
            vec![dated(1, "retirement party", "2060-01-01T00:00:00Z", "")],
            AGENDA_DEFAULT_DAYS,
        );
        assert!(out.contains("1 further out"), "still counted: {out}");
        assert!(
            out.contains("tasqx list"),
            "the note must name a view that actually shows it: {out}"
        );

        // The real check, and it reads the ceiling rather than a literal: no
        // `--days N` in the footer may be one `window_parser` would refuse.
        for n in recommended_days(&out) {
            assert!(
                (1..=AGENDA_MAX_DAYS).contains(&n),
                "the footer recommended `--days {n}`, which the CLI refuses \
                 (1..={AGENDA_MAX_DAYS}): {out}"
            );
        }

        // ...and inside the ceiling the exact window is still quoted, so the
        // clamp did not buy the fix by making every note useless.
        let near = agenda_out(
            vec![dated(1, "far outside", "2026-11-01T00:00:00Z", "")],
            AGENDA_DEFAULT_DAYS,
        );
        assert_eq!(recommended_days(&near), vec![90], "{near}");
    }

    /// Every `--days N` the rendered footer tells the reader to run.
    fn recommended_days(out: &str) -> Vec<usize> {
        out.split("--days ")
            .skip(1)
            .filter_map(|tail| {
                let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .collect()
    }

    /// Two tasks on the same instant keep the engine's `-urgency` ranking: the
    /// sort is by the agenda instant alone and `sort_by_key` is stable, so the
    /// order `task.list` returned survives a tie.
    #[test]
    fn tasks_at_the_same_instant_keep_the_urgency_order_they_arrived_in() {
        let mut hot = dated(1, "hot", "2026-08-05T09:00:00Z", "");
        hot["urgency"] = json!(20.0);
        let mut cold = dated(2, "cold", "2026-08-05T09:00:00Z", "");
        cold["urgency"] = json!(1.0);
        // As `task.list {sort:["-urgency"]}` would return them.
        let payload = agenda_payload(vec![hot, cold]);
        let a = agenda_of(&payload, 14);
        assert_eq!(
            a.entries
                .iter()
                .map(|e| e.task["short_id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    /// #145: `tasqx list project:x` (a literal filter, no `@working`) returns
    /// whatever the project holds — done and cancelled included, D24's rule
    /// being about `report`, not `list`. Leaving that literal is defensible;
    /// what is not is a cancelled row printing identically to open work at
    /// the top of the table, ranked by urgency, with nothing to tell them
    /// apart short of a `show`.
    #[test]
    fn task_table_marks_a_row_whose_status_is_not_open() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let row = |status: &str| {
            json!({ "short_id": 1, "urgency": 5.0, "priority": "M", "title": "same work",
                    "project": "p", "due": "", "tags": [], "status": status })
        };
        let open = task_table(
            &ctx,
            &json!({ "tasks": [row("pending")], "count": 1 }),
            Timestamp::now(),
        );
        let closed = task_table(
            &ctx,
            &json!({ "tasks": [row("cancelled")], "count": 1 }),
            Timestamp::now(),
        );
        assert_ne!(
            open, closed,
            "a cancelled row must not render identically to a pending one: {closed:?}"
        );
        assert!(
            closed.contains("cancelled"),
            "the task's own status must be visible in the table, not only via \
             `tasqx show`: {closed:?}"
        );
    }

    /// #146: `blocked` sits on the JSON of every row `list`/`agenda` print and
    /// is rendered on neither. A blocked task can rank #1 by urgency and look
    /// exactly like ordinary open work, and `why` explains the score without
    /// ever mentioning the task cannot be started.
    #[test]
    fn task_table_and_why_surface_a_blocked_task() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let row = |blocked: bool| {
            json!({ "short_id": 1, "urgency": 18.0, "priority": "H",
                    "title": "BLOCKER-HIGH must not be next", "project": "p",
                    "due": "", "tags": [], "status": "pending", "blocked": blocked })
        };
        let open = task_table(
            &ctx,
            &json!({ "tasks": [row(false)], "count": 1 }),
            Timestamp::now(),
        );
        let blocked = task_table(
            &ctx,
            &json!({ "tasks": [row(true)], "count": 1 }),
            Timestamp::now(),
        );
        assert_ne!(
            open, blocked,
            "a blocked row must not render identically to an unblocked one: {blocked:?}"
        );

        let get_result = json!({
            "short_id": 1, "title": "BLOCKER-HIGH must not be next", "status": "pending",
            "urgency": 18.0, "blocked": true, "depends_on": [2],
            "unmet_blockers": [{ "short_id": 2, "title": "the blocker" }]
        });
        let why_out = why(&ctx, &get_result, Timestamp::now());
        assert!(
            why_out.contains("blocked") && why_out.contains("#2"),
            "`why` must say the task is blocked and name what it is blocked by, \
             since the breakdown alone explains a score `next` will never offer: \
             {why_out:?}"
        );
    }

    /// #147: the one piece of state a work block depends on — which task is
    /// already running — has no mark on `list`'s row for it, and `next`
    /// returning that same task says nothing to distinguish it from a fresh
    /// recommendation.
    #[test]
    fn task_table_and_next_surface_the_running_task() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let row = |status: &str| {
            json!({ "short_id": 2, "urgency": 5.0, "priority": "M", "title": "in progress",
                    "project": "p", "due": "", "tags": [], "status": status })
        };
        let pending = task_table(
            &ctx,
            &json!({ "tasks": [row("pending")], "count": 1 }),
            Timestamp::now(),
        );
        let active = task_table(
            &ctx,
            &json!({ "tasks": [row("active")], "count": 1 }),
            Timestamp::now(),
        );
        assert_ne!(
            pending, active,
            "an active row must not render identically to a pending one: {active:?}"
        );

        let mut running = row("active");
        running["active_since"] = json!("2026-08-31T12:00:00Z");
        let next_out = next_task(&ctx, &json!({ "tasks": [running] }), Timestamp::now());
        assert!(
            next_out.to_lowercase().contains("already running"),
            "`next` handing back the task that is already running must say so: \
             {next_out:?}"
        );
    }

    /// #182: `agenda`'s Overdue group has no horizon (rule 3), which on a
    /// store with years of history means no cap either — the field report's
    /// 10,000-task store printed 1903 rows before the first `Today` heading.
    /// Reproduced at a size a unit test can afford: enough all-overdue rows
    /// to exceed [`AGENDA_OVERDUE_CAP`], each ranked so the cut is checkable.
    #[test]
    fn agenda_caps_the_overdue_group_and_stops_naming_a_horizon_over_it() {
        let n = AGENDA_OVERDUE_CAP + 5;
        let tasks: Vec<Value> = (0..n)
            .map(|i| {
                let mut t = dated(
                    i as i64 + 1,
                    &format!("ancient #{i}"),
                    "2023-01-01T00:00:00Z",
                    "",
                );
                // Distinct urgency: the cap must keep the hottest N, and the
                // count below only proves a cap exists, not which end it cut.
                t["urgency"] = json!(100.0 - i as f64);
                t
            })
            .collect();
        let out = agenda_out(tasks, 14);
        let shown = out.matches("ancient #").count();
        assert_eq!(
            shown, AGENDA_OVERDUE_CAP,
            "the overdue group must stop at the cap: {out:?}"
        );
        assert!(
            out.contains("more overdue"),
            "the rows the cap held back must be counted and named, the same way \
             undated and beyond-horizon rows already are: {out:?}"
        );
        // D133: the oldest is spelled as `list` spells a date, the year kept
        // because it is not this one.
        assert!(
            out.contains("5 more overdue, oldest 1 Jan 23 "),
            "the footer spells its oldest day in list's words: {out:?}"
        );
        assert!(
            !out.contains("+14d"),
            "a set that is 100% overdue must not have a footer advertising a \
             horizon none of the visible rows are within: {out:?}"
        );
    }
}
