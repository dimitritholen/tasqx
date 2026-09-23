//! The `task.list` table: rows, columns and the DUE/URG cells shared with
//! the agenda (task #727 split of `render.rs`).

use super::*;

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Unit};
use serde_json::Value;
use tasqx_core::filter::overdue_at;

use crate::columns::{self, Column, GAP};
use crate::theme::Ctx;

/// Extract a string field, sanitized — every field pulled here is display text
/// that may originate from `store.import` or an MCP write tool.
/// True when a `task.list`/`report.summary`/`project.list` result names a
/// store that has NEVER held a task, distinguished from a filter or window
/// that simply matched nothing (#233). Missing the key (an older core, or a
/// hand-built fixture) reads as `false` — the existing dead-end copy — so a
/// build skew never invents a hint about a store it cannot vouch for.
pub(crate) fn store_is_empty(result: &Value) -> bool {
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
pub(crate) fn onboarding_hint(ctx: &Ctx) -> String {
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
    pub(crate) due: String,
    pub(crate) overdue: bool,
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
    pub(crate) fn head(&self) -> usize {
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
pub(crate) fn row_line(ctx: &Ctx, c: &TaskCols, r: &TaskRow) -> String {
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
pub(crate) fn rail_marker(t: &Value, unicode: bool) -> Option<(&'static str, &'static str)> {
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
pub(crate) fn status_marker(t: &Value) -> Option<(&'static str, String)> {
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
pub(crate) fn month_abbrev(m: i8) -> String {
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
pub(crate) fn field_ts(t: &Value, key: &str) -> Option<Timestamp> {
    t.get(key)
        .and_then(Value::as_str)
        .and_then(|v| v.parse::<Timestamp>().ok())
}

/// `1 task` / `9 tasks`. The `N task(s)` this replaces was the reader being
/// handed a `match` the writer would not make.
pub(crate) fn plural_tasks(n: i64) -> String {
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
pub(crate) fn store_health_notes(tasks: &[Value]) -> Vec<String> {
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

/// Three letters, written out rather than taken from a locale: the rest of this
/// module renders one fixed English surface, and a heading whose width changes
/// with `$LANG` would move a column the table has already been fitted to.
pub(crate) fn weekday_abbrev(d: Date) -> &'static str {
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
mod tests;
