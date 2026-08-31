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
use serde_json::Value;

use crate::theme::Ctx;
use crate::AGENDA_MAX_DAYS;

/// Strip terminal-control bytes from untrusted text before it is painted, so an
/// imported or agent-authored task field can't smuggle ANSI/OSC escapes that
/// clear the screen, move the cursor, set the window title, or spoof CLI output.
/// This is the terminal-path analogue of `html::esc`. C0 controls (except tab),
/// DEL, and C1 controls are dropped; ordinary printable text is untouched.
pub fn san(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\t' || !c.is_control())
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
fn s(v: &Value, key: &str) -> String {
    san(v.get(key).and_then(Value::as_str).unwrap_or(""))
}

/// D21: the trailer is driven by the `default` field the core returns, not
/// printed unconditionally. `project.create` claims the default only when the
/// store has none, so this line was a lie on every `init` but the first.
pub fn project_created(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let became_default = result
        .get("default")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let trailer = if became_default {
        "  ·  now your default project".to_string()
    } else {
        // Name the verb that would do it: the user's complaint was being left
        // with no way to steer this and no hint that one existed.
        format!(
            "  ·  default is still {}  (tasqx use {name})",
            default_label(ctx, result)
        )
    };
    format!(
        "{} created{trailer}\n",
        ctx.paint("accent", &format!("Project {name}"))
    )
}

/// The name of the default at the time of a `project.create` that did not claim
/// it. The core reports it on the same result so the CLI never has to guess.
fn default_label(ctx: &Ctx, result: &Value) -> String {
    match result.get("current_default").and_then(Value::as_str) {
        Some(d) => ctx.paint("project", &san(d)),
        None => "unset".to_string(),
    }
}

/// D21: `use` moved the default. Both sides are printed — the new one because
/// it is the answer, the old one because a silent switch is the bug.
pub fn default_switched(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let previous = result
        .get("previous")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty());
    let trailer = match previous {
        Some(p) => format!("  ·  was {}", ctx.paint("project", &san(p))),
        None => String::new(),
    };
    format!(
        "{}  ·  a bare `tasqx add` lands here{trailer}\n",
        ctx.paint("accent", &format!("Default project is now {name}"))
    )
}

/// D22: `archive` retired a project, and may have un-pointed the default doing
/// it. Both facts go on the line.
///
/// The default-clearing branch is the whole reason this function is not a
/// one-liner. `project.archive` clears the `default_project` key when it
/// archives the project that key names, so a single `tasqx archive work` can
/// change where every future bare `tasqx add` lands — and D22 wrote, before any
/// terminal copy existed, that when a CLI verb landed it would render
/// `default_cleared`. Printing only "Project work archived" would leave the user
/// to discover the move by finding their next task in no project at all.
///
/// The other branch is stated rather than implied for the D39 reason: "Project
/// work archived" alone is also exactly what the cleared case would print, so
/// silence cannot be read as "the default is fine". Both outcomes name
/// themselves, and `default_cleared` — a field the core always sends, never
/// omits — decides which.
pub fn project_archived(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let cleared = result
        .get("default_cleared")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Name the verb that points the default somewhere again: a store with no
    // default is a valid state (D22), but it is one the user has to be able to
    // leave, and `use` is the only way out.
    let trailer = if cleared {
        "  ·  it was your default project, so a bare `tasqx add` has no home until `tasqx use <project>`"
    } else {
        "  ·  your default project is unchanged"
    };
    format!(
        "{}{trailer}\n",
        ctx.paint("accent", &format!("Project {name} archived"))
    )
}

pub fn task_added(ctx: &Ctx, result: &Value, title: &str) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    let status = s(result, "status");
    // D21: name where it landed. With no explicit `project:`, the task inherits
    // the default, and this is the only place the user finds out which project
    // that was — "silently lands in prive.klussen" is this text not existing.
    let proj = match result
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        Some(p) => format!("  ·  {}", ctx.paint("project", &san(p))),
        None => String::new(),
    };
    format!(
        "{}  ·  {status}  ·  urgency {urg:.1}{proj}\n  {}\n",
        ctx.paint("accent", &format!("Added #{sid}")),
        san(title)
    )
}

pub fn started(ctx: &Ctx, result: &Value) -> String {
    let started = s(result, "interval_started");
    format!(
        "{}  ·  timer running (since {started})\n",
        ctx.paint("timer.active", "Started task")
    )
}

pub fn stopped(ctx: &Ctx, result: &Value) -> String {
    let tracked = s(result, "tracked");
    format!(
        "{}  ·  tracked {tracked}\n",
        ctx.paint("timer.active", "Stopped")
    )
}

/// The dependents a closing verb just released, or nothing if it released none.
///
/// Shared by `done` and `status_line` because BOTH `task.done` and
/// `task.cancel` return this list from the same helper (`compute_unblocked`),
/// and only `done` used to render it. D11 makes cancelling a blocker release
/// its dependents precisely so the graph stays honest, so a `cancel` that
/// printed `#1 -> cancelled` and nothing else hid the very effect the decision
/// exists to produce — and hid it from a reader who had already learned from
/// `done` that a release gets announced, so silence read as "nothing changed".
///
/// One helper rather than a second copy: the two verbs answering differently is
/// the failure, so they cannot have two renderers to drift between. Empty in,
/// empty out — a verb that released nothing must not claim a heading either.
fn unblocked_line(ctx: &Ctx, result: &Value) -> String {
    let refs: Vec<String> = result
        .get("unblocked")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect()
        })
        .unwrap_or_default();
    if refs.is_empty() {
        return String::new();
    }
    format!(
        "  {} {}\n",
        ctx.paint("accent", "now actionable:"),
        refs.join(" ")
    )
}

/// The dependents a reopen just put back into `blocked`, or nothing.
///
/// The mirror of [`unblocked_line`], and it exists for the mirror reason: a
/// reopen changes which work is actionable and used to say nothing about it
/// (D69). Keyed on the `blocked` list, which only `task.reopen` returns, so the
/// shared `status_line` stays correct for `task.cancel` without a list of which
/// verbs re-block.
fn reblocked_line(ctx: &Ctx, result: &Value) -> String {
    let refs: Vec<String> = result
        .get("blocked")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect()
        })
        .unwrap_or_default();
    if refs.is_empty() {
        return String::new();
    }
    format!(
        "  {} {}
",
        ctx.paint("accent", "back to blocked:"),
        refs.join(" ")
    )
}

pub fn done(ctx: &Ctx, result: &Value) -> String {
    let completed = s(result, "completed");
    let mut out = format!(
        "{}  ·  completed {completed}\n",
        ctx.paint("timer.active", "Done")
    );
    out.push_str(&unblocked_line(ctx, result));
    // A recurring task spawns its next instance on completion (DESIGN §10, D2).
    if let Some(sp) = result.get("spawned") {
        let sid = sp.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        let when = sp
            .get("due")
            .and_then(Value::as_str)
            .or_else(|| sp.get("scheduled").and_then(Value::as_str))
            .unwrap_or("");
        let tail = if when.is_empty() {
            String::new()
        } else {
            format!(" due {when}")
        };
        out.push_str(&format!(
            "  {} #{sid}{tail}\n",
            ctx.paint(
                "accent",
                if ctx.caps.unicode {
                    "\u{21b3} next:"
                } else {
                    "-> next:"
                }
            )
        ));
    }
    out
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
struct TaskRow {
    sid: String,
    urg: String,
    ramp: f64,
    prio: String,
    title: String,
    project: String,
    due: String,
    overdue: bool,
    tags: String,
}

/// The width of every column of one table, in cells. A `0` means the column is
/// ABSENT — not empty-but-drawn — and neither its header nor its gap is emitted.
struct TaskCols {
    id: usize,
    urg: usize,
    title: usize,
    project: usize,
    due: usize,
    tags: usize,
}

/// Cells between two columns.
const GAP: usize = 2;

impl TaskCols {
    /// Floors: below these a column stops carrying information, so the table
    /// overflows the terminal rather than shrinking past them.
    const MIN_TITLE: usize = 20;
    const MIN_PROJECT: usize = 8;
    const MIN_TAGS: usize = 8;
    /// A cut date must still show the date: `2026-07-20…` is 11 cells.
    const MIN_DUE: usize = 11;
    /// Ceilings. A column wider than this stops earning its cells: the eye
    /// loses the row across a 90-cell title, and the tail of a tag list or a
    /// project name identifies far less than its head. Overflow goes to the
    /// ellipsis, not to the column — and the cells stay available to the
    /// neighbours that can still use them.
    const MAX_TITLE: usize = 72;
    const MAX_PROJECT: usize = 24;
    const MAX_TAGS: usize = 28;

    /// Everything left of `TASK`, plus the gap that follows it.
    fn head_width(&self) -> usize {
        self.id + GAP + self.urg + GAP + 1 + GAP
    }

    /// The whole row, gaps included, absent columns costing nothing.
    fn total(&self) -> usize {
        let opt = |w: usize| if w == 0 { 0 } else { GAP + w };
        self.head_width() + self.title + opt(self.project) + opt(self.due) + opt(self.tags)
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
    fn fit(rows: &[TaskRow], budget: usize, when_label: &str) -> TaskCols {
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

        let mut c = TaskCols {
            // The id column keeps a floor of 4 rather than sizing to its digits:
            // ids grow monotonically, and a table that shifted left by a cell
            // the day the store passed #999 would look like the bug this
            // function fixes.
            id: max_of(|r| &r.sid).max(4),
            urg: max_of(|r| &r.urg).max(width("URG")),
            title: sized(max_of(|r| &r.title), "TASK").max(width("TASK")),
            project: sized(max_of(|r| &r.project), "PROJECT"),
            due: sized(max_of(|r| &r.due), when_label),
            tags: sized(max_of(|r| &r.tags), "TAGS"),
        };
        c.title = c.title.min(Self::MAX_TITLE);
        c.project = c.project.min(Self::MAX_PROJECT);
        c.tags = c.tags.min(Self::MAX_TAGS);

        // Over budget: take each cell from whichever column is currently WIDEST,
        // down to its floor. Not "shrink the least important one first" — that
        // was tried, and on a real store it cut `PROJECT` and `TAGS` to their
        // floors while a 68-cell `TASK` column sat untouched, which is the same
        // failure as the old fixed widths (one column keeping room it does not
        // need while its neighbours are unreadable), just chosen dynamically.
        // Taking from the widest converges on columns of comparable size, so
        // what gets cut is whatever has the most left to lose.
        //
        // Ties go to the title: it is the one column whose first characters are
        // rarely enough to identify the row.
        let mut over = c.total().saturating_sub(budget);
        while over > 0 {
            // Order matters: `max_by_key` keeps the LAST of equal maxima, so
            // the title comes first and is the last to be picked on a tie.
            let mut cols: Vec<(&mut usize, usize)> = vec![
                (&mut c.title, Self::MIN_TITLE),
                (&mut c.due, Self::MIN_DUE),
                (&mut c.project, Self::MIN_PROJECT),
                (&mut c.tags, Self::MIN_TAGS),
            ];
            cols.retain(|(w, floor)| **w > *floor);
            let Some((widest, _)) = cols.into_iter().max_by_key(|(w, _)| **w) else {
                break; // every column is at its floor — see the drop pass below
            };
            *widest -= 1;
            over -= 1;
        }

        // Still over, with every column at its floor: a narrow terminal that
        // simply cannot hold this many columns. Drop them from the RIGHT until
        // the row fits — positional, so a reader can predict which column goes
        // without reading this function. A row that overflowed instead would
        // wrap, and a wrapped row destroys the alignment of every column at
        // once, which is worse than showing fewer of them.
        if c.total() > budget {
            c.tags = 0;
        }
        if c.total() > budget {
            c.due = 0;
        }
        if c.total() > budget {
            c.project = 0;
        }
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
fn cell(ctx: &Ctx, role: Option<&str>, text: &str, w: usize) -> String {
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
fn join_cells(cells: Vec<String>) -> String {
    cells.join(&" ".repeat(GAP)).trim_end().to_string()
}

/// The header line for a fitted table. `when_label` must be the same string the
/// widths were fitted with — see [`TaskCols::fit`].
fn header_line(c: &TaskCols, when_label: &str) -> String {
    let mut head = vec![
        rpad("ID", c.id),
        rpad("URG", c.urg),
        "P".to_string(),
        pad("TASK", c.title),
    ];
    for (w, label) in [
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
    let prio_role = match r.prio.as_str() {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    let urg_plain = rpad(&r.urg, c.urg);
    let mut line = vec![
        rpad(&r.sid, c.id),
        ctx.theme.ramp_style(r.ramp).paint(&urg_plain, &ctx.caps),
        cell(ctx, Some(prio_role), &r.prio, 1),
        cell(ctx, None, &r.title, c.title),
    ];
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

/// Measure one `task.list` row into the cells the layout will be computed from.
///
/// Shared with [`agenda_text`], which then overwrites `due`/`overdue` with what
/// its own `WHEN` column holds. Everything else — the urgency ramp, the
/// sanitizing, the `-` for an unset priority — is identical by construction
/// rather than by two functions agreeing, which is how the two views cannot come
/// to disagree about the same task.
fn task_row(t: &Value, max_urg: f64, now: Timestamp) -> TaskRow {
    let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    TaskRow {
        sid: format!("{}", t.get("short_id").and_then(Value::as_i64).unwrap_or(0)),
        urg: format!("{urg:.1}"),
        ramp: urg / max_urg,
        prio: t
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string(),
        title: s(t, "title"),
        project: s(t, "project"),
        due: s(t, "due"),
        overdue: field_ts(t, "due").map(|d| d < now).unwrap_or(false)
            && status_is_open(&s(t, "status")),
        tags: t
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                san(&a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "))
            })
            .unwrap_or_default(),
    }
}

/// The urgency denominator that normalizes the ramp across the visible rows.
/// Floored at 1.0 so a table of zero-urgency rows divides by something.
fn max_urgency(tasks: &[&Value]) -> f64 {
    tasks
        .iter()
        .filter_map(|t| t.get("urgency").and_then(Value::as_f64))
        .fold(0.0_f64, f64::max)
        .max(1.0)
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

/// Render a `task.list` result as an aligned, themed table.
///
/// `now` is a parameter and never the system clock — the rule this module
/// already states at [`agenda_select`], adopted here late: an internal read
/// made the overdue highlight untestable at the day boundary.
pub fn task_table(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if tasks.is_empty() {
        return "No tasks.\n".to_string();
    }

    let refs: Vec<&Value> = tasks.iter().collect();
    let max_urg = max_urgency(&refs);
    let rows: Vec<TaskRow> = tasks.iter().map(|t| task_row(t, max_urg, now)).collect();
    let c = TaskCols::fit(&rows, ctx.cols, "DUE");

    // The rule spans the TABLE, not the header text. Those differ by the last
    // column's padding, which `join_cells` trims — and a rule cut to the trimmed
    // header stops short of the rows that run under it, which reads as the rows
    // overflowing something.
    let rule_len = c.total().min(ctx.cols);

    let mut out = String::new();
    out.push_str(&ctx.paint("header", &header_line(&c, "DUE")));
    out.push('\n');
    out.push_str(&ctx.hrule(rule_len));
    out.push('\n');

    for r in &rows {
        out.push_str(&row_line(ctx, &c, r));
        out.push('\n');
    }

    let count = result
        .get("count")
        .and_then(Value::as_i64)
        .unwrap_or(tasks.len() as i64);
    out.push_str(&ctx.hrule(rule_len));
    out.push('\n');
    out.push_str(&ctx.paint("muted", &format!("{count} task(s)")));
    out.push('\n');

    for note in store_health_notes(tasks) {
        out.push_str(&ctx.paint("warn", &note));
        out.push('\n');
    }
    out
}

/// The notes a status-less task table owes its reader about rows the store
/// could not read back cleanly — one per defect, naming the offending ids and
/// the way out. Empty for a healthy store, so their presence always means
/// something.
///
/// Shared rather than written per view, and that is the whole point of it being
/// a function. Both [`task_table`] and [`agenda_text`] draw the same rows
/// WITHOUT a status column and WITH a title cell that can come out empty, so
/// each of them can hide exactly these two defects. `agenda` shipped as a second
/// table over the same rows and did not carry the notes: an unreadable status
/// sat under `Wed 2026-08-05` looking like ordinary open work, and a blank-title
/// row drew as an empty TASK cell with nothing under the table to say why —
/// the invisible-field failure rebuilt one view over, which is what a copied
/// layout does. A third view gets them by calling this; it cannot get them by
/// remembering to.
fn store_health_notes(tasks: &[Value]) -> Vec<String> {
    let mut notes = Vec::new();

    // Neither table has a status column, so a row the store could not read
    // would otherwise sit in the default view indistinguishable from ordinary
    // open work — the invisible-field failure this project keeps rebuilding.
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
struct Entry {
    task: Value,
    at: Timestamp,
    day: Date,
    kind: When,
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
pub struct Agenda {
    entries: Vec<Entry>,
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
}

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
pub fn agenda_select(result: &Value, days: usize, now: Timestamp) -> Agenda {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

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
    };

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
        a.entries.push(Entry {
            task: t.clone(),
            at,
            day,
            kind,
        });
    }

    // STABLE, and by the instant alone. The rows arrive in the engine's
    // `-urgency` order, so two tasks landing on the same instant keep the
    // ranking the rest of the tool would give them instead of an arbitrary one.
    a.entries.sort_by_key(|e| e.at);
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

fn group_of(day: Date, today: Date) -> Group {
    if day < today {
        Group::Overdue
    } else {
        Group::Day(day)
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

/// `Today · Mon 2026-08-03`. The date is spelled out on every heading, today's
/// included: "Today" alone is the one label that means something different
/// tomorrow, and terminal output gets pasted into tickets.
fn day_heading(day: Date, today: Date) -> String {
    let stamp = format!("{} {day}", weekday_abbrev(day));
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
/// cells carry the full date instead — `due 2026-07-29` — because "how late" is
/// the whole content of that group.
fn when_cell(kind: When, at: Timestamp, dated: bool) -> String {
    let z = at.to_zoned(TimeZone::UTC);
    let t = z.time();
    let clock = if t.hour() == 0 && t.minute() == 0 {
        String::new()
    } else {
        format!("{:02}:{:02}", t.hour(), t.minute())
    };
    let stamp = match (dated, clock.is_empty()) {
        (true, true) => z.date().to_string(),
        (true, false) => format!("{} {clock}", z.date()),
        (false, true) => String::new(),
        (false, false) => clock,
    };
    if stamp.is_empty() {
        kind.label().to_string()
    } else {
        format!("{} {stamp}", kind.label())
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

    let refs: Vec<&Value> = a.entries.iter().map(|e| &e.task).collect();
    if !refs.is_empty() {
        let max_urg = max_urgency(&refs);
        let rows: Vec<TaskRow> = a
            .entries
            .iter()
            .map(|e| {
                let mut r = task_row(&e.task, max_urg, a.at_start_of_today());
                let overdue = e.day < a.today;
                r.due = when_cell(e.kind, e.at, overdue);
                // Repainted from the AGENDA instant, not from `due` alone: a
                // task scheduled last week and due next month is late on the
                // thing this row is about, and `task_row`'s answer is about the
                // deadline only.
                r.overdue = overdue && status_is_open(&s(&e.task, "status"));
                r
            })
            .collect();
        let c = TaskCols::fit(&rows, ctx.cols, "WHEN");
        let rule_len = c.total().min(ctx.cols);

        out.push_str(&ctx.paint("header", &header_line(&c, "WHEN")));
        out.push('\n');
        out.push_str(&ctx.hrule(rule_len));
        out.push('\n');

        let mut current: Option<Group> = None;
        for (e, r) in a.entries.iter().zip(&rows) {
            let g = group_of(e.day, a.today);
            if current != Some(g) {
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
        out.push_str(&ctx.hrule(rule_len));
        out.push('\n');
    }

    // The horizon is stated on every run, not only when it cut something: "5
    // task(s)" alone cannot be read as "and that is all there is" unless the
    // window it is all there is WITHIN is on the same line. No weekday here,
    // unlike the day headings -- the count can be four digits and this line has
    // to survive a 40-cell terminal, and the headings already carry the days.
    out.push_str(&ctx.paint(
        "muted",
        &format!(
            "{} task(s) · through {} (+{}d)",
            a.entries.len(),
            a.through,
            a.days
        ),
    ));
    out.push('\n');
    for note in a.omissions() {
        out.push_str(&ctx.paint("muted", &note));
        out.push('\n');
    }
    // Last, and in `warn` rather than `muted`, because these are not this
    // view's own omissions: they are damage in the store that this layout —
    // like `list`'s, which it shares — cannot show in a cell. See
    // `store_health_notes` for why both views read one implementation.
    for note in &a.health {
        out.push_str(&ctx.paint("warn", note));
        out.push('\n');
    }
    out
}

impl Agenda {
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

pub fn task_detail(ctx: &Ctx, result: &Value) -> String {
    // The card is the interactive rendering (D76); everything below it is the
    // byte-stable plain layout every pipe, script and docs example reads.
    if ctx.caps.unicode {
        return task_detail_card(ctx, result);
    }
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let mut out = String::new();
    out.push_str(&ctx.paint("header", &format!("#{sid}  {}", s(result, "title"))));
    out.push('\n');
    out.push_str(&format!("  status     {}\n", status_cell(ctx, result)));
    let prio = result
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let prio_role = match prio {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    out.push_str(&format!("  priority   {}\n", ctx.paint(prio_role, prio)));
    if !s(result, "project").is_empty() {
        out.push_str(&format!(
            "  project    {}\n",
            ctx.paint("project", &s(result, "project"))
        ));
    }
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    out.push_str(&format!("  urgency    {urg:.1}\n"));
    if !s(result, "due").is_empty() {
        out.push_str(&format!("  due        {}\n", s(result, "due")));
    }
    if !s(result, "remind").is_empty() {
        out.push_str(&format!(
            "  remind     {}\n",
            ctx.paint("accent", &s(result, "remind"))
        ));
    }
    if !s(result, "scheduled").is_empty() {
        out.push_str(&format!("  scheduled  {}\n", s(result, "scheduled")));
    }
    if !s(result, "wait").is_empty() {
        out.push_str(&format!("  wait       {}\n", s(result, "wait")));
    }
    if !s(result, "recurrence").is_empty() {
        out.push_str(&format!(
            "  repeats    {}\n",
            ctx.paint("accent", &s(result, "recurrence"))
        ));
    }
    if !s(result, "estimate").is_empty() {
        out.push_str(&format!("  estimate   {}\n", s(result, "estimate")));
    }
    // Conditional for the reason `tracked` is: only a closed task HAS a
    // completion moment, and an empty `completed` row on every pending task is
    // noise. It was stored, returned by `task.get` and rendered by `done` — the
    // one surface that scrolls away — so the detail view, whose whole job is
    // showing a task's fields, was the only place the moment could be looked up
    // later and the only place it did not appear.
    if !s(result, "completed").is_empty() {
        out.push_str(&format!("  completed  {}\n", s(result, "completed")));
    }
    // Conditional, unlike `blocked`: every task has a blocked answer worth
    // reading, but "tracked PT0S" on the many tasks that were never timed is
    // noise on the detail of every one of them. Shown from the first tracked
    // second onward, which is when the number starts meaning something.
    let tracked = s(result, "tracked");
    if !tracked.is_empty() && tracked != "PT0S" {
        out.push_str(&format!("  tracked    {tracked}\n"));
    }
    // The open interval is NOT folded into `tracked` (see `task_to_json`), so
    // an active task must say the clock is still running or its tracked total
    // reads as the final answer when it is only the total so far.
    if !s(result, "active_since").is_empty() {
        out.push_str(&format!(
            "  running    {}\n",
            ctx.paint("accent", &format!("since {}", s(result, "active_since")))
        ));
    }
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    out.push_str(&format!("  blocked    {blocked}\n"));
    if let Some(tags) = result.get("tags").and_then(Value::as_array) {
        if !tags.is_empty() {
            let names: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
            out.push_str(&format!(
                "  tags       {}\n",
                ctx.paint("tag", &san(&names.join(" ")))
            ));
        }
    }
    if let Some(deps) = result.get("depends_on").and_then(Value::as_array) {
        if !deps.is_empty() {
            let refs: Vec<String> = deps
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            out.push_str(&format!("  depends_on {}\n", refs.join(" ")));
        }
    }
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
            out.push_str(&format!(
                "  tokens     in {} · out {} · cacheR {} · cacheW {}\n",
                sum("input_tokens"),
                sum("output_tokens"),
                sum("cache_read_tokens"),
                sum("cache_creation_tokens")
            ));
        }
    }
    if let Some(anns) = result.get("annotations").and_then(Value::as_array) {
        for a in anns {
            out.push_str(&format!(
                "  {} {}\n",
                ctx.paint("muted", "·"),
                san(a.get("body").and_then(Value::as_str).unwrap_or(""))
            ));
        }
    }
    out
}

// ============================================================================
// The task card (D76)
// ============================================================================
//
// The interactive echo of `add` and the interactive body of `show`. Gated on
// `caps.unicode` — true exactly when stdout is a VT-capable terminal — so
// every piped, dumb-terminal and legacy-console caller keeps the byte-stable
// plain rendering above, and a script diffing two runs sees what it always
// saw. Output-only formatting, so stdout alone decides; the dashboard demands
// stdin too because it reads keys, and that stricter gate must not be copied
// here. Tones come from the `card.*` roles — see `theme::builtin` for why
// they are deliberately achromatic in every built-in.

/// One run of card text: the role that paints it, or `None` for the
/// terminal's own foreground — the card's "values" tone. Default-fg rather
/// than a literal light gray, so the card reads on light and dark grounds
/// alike.
type Seg = (Option<&'static str>, String);

fn segs_width(segs: &[Seg]) -> usize {
    segs.iter().map(|(_, t)| width(t)).sum()
}

fn paint_segs(ctx: &Ctx, segs: &[Seg]) -> String {
    segs.iter()
        .map(|(role, t)| match role {
            Some(r) => ctx.paint(r, t),
            None => t.clone(),
        })
        .collect()
}

/// Truncate a seg run to at most `max` cells, keeping each run's role and
/// letting [`truncate`] put the ellipsis on the piece that was cut.
fn fit_segs(segs: Vec<Seg>, max: usize) -> Vec<Seg> {
    if segs_width(&segs) <= max {
        return segs;
    }
    let mut out: Vec<Seg> = Vec::new();
    let mut used = 0usize;
    for (role, t) in segs {
        let w = width(&t);
        if used + w <= max {
            used += w;
            out.push((role, t));
            continue;
        }
        if max > used {
            out.push((role, truncate(&t, max - used, true)));
        }
        break;
    }
    out
}

/// The rounded gray frame around body rows, with the header worked into the
/// top border — the quiet-frame shape the user chose in #259.
///
/// The interior is as wide as the widest row asks, capped at 80 total cells
/// (a card is a glance, not a table) and at the live terminal width. Glyph
/// honesty note, recorded in D76: `─`/`│`/`●`/`▲` are East-Asian-ambiguous
/// width, which [`width`] counts as one cell; a terminal configured to draw
/// ambiguous glyphs wide will bend the right edge. The dashboard's borders
/// accepted the same edge first.
fn card_box(ctx: &Ctx, header: Vec<Seg>, rows: Vec<Vec<Seg>>) -> String {
    let cap = ctx.cols.clamp(Ctx::MIN_COLS, 80).saturating_sub(4);
    let body_w = rows.iter().map(|r| segs_width(r)).max().unwrap_or(0);
    let head_w = segs_width(&header);
    let inner = body_w.max(head_w + 2).clamp(10, cap);
    let header = fit_segs(header, inner.saturating_sub(2));
    let head_w = segs_width(&header);
    let mut out = String::new();
    out.push_str(&ctx.paint("card.frame", "╭─ "));
    out.push_str(&paint_segs(ctx, &header));
    out.push(' ');
    out.push_str(&ctx.paint(
        "card.frame",
        &format!("{}╮", "─".repeat(inner - 1 - head_w)),
    ));
    out.push('\n');
    for row in rows {
        let row = fit_segs(row, inner);
        let fill = " ".repeat(inner - segs_width(&row));
        out.push_str(&format!(
            "{}{}{fill}{}\n",
            ctx.paint("card.frame", "│ "),
            paint_segs(ctx, &row),
            ctx.paint("card.frame", " │"),
        ));
    }
    out.push_str(&ctx.paint("card.frame", &format!("╰{}╯", "─".repeat(inner + 2))));
    out.push('\n');
    out
}

/// The `add` confirmation card (D76). Takes the FULL task — a `task.get`
/// result — because `task.add`'s own result is a frozen five-field summary
/// that carries no tags, due, priority or estimate (see `run_add`, which
/// reads the task back on the interactive path and falls back to the plain
/// line when that read fails).
pub fn task_added_card(ctx: &Ctx, task: &Value) -> String {
    let sid = task.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let urg = task.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    let gap = |row: &mut Vec<Seg>| {
        if !row.is_empty() {
            row.push((None, "   ".into()));
        }
    };

    let header: Vec<Seg> = vec![
        (Some("card.label"), format!("#{sid}")),
        (Some("card.frame"), " · ".into()),
        (Some("card.strong"), s(task, "title")),
    ];

    // Row 1: the state of the thing — status, priority when set, urgency.
    // Raw status, not `status_cell`: segs are measured for padding before they
    // are painted, so a pre-painted string would have its SGR bytes counted as
    // cells and bend the frame. The plain `add` line shows the raw status too.
    let mut state: Vec<Seg> = vec![(None, format!("● {}", s(task, "status")))];
    match task.get("priority").and_then(Value::as_str) {
        Some("H") => {
            gap(&mut state);
            state.push((Some("card.strong"), "! high".into()));
        }
        Some("M") => {
            gap(&mut state);
            state.push((None, "med".into()));
        }
        Some("L") => {
            gap(&mut state);
            state.push((Some("card.label"), "low".into()));
        }
        _ => {}
    }
    gap(&mut state);
    state.push((Some("card.strong"), format!("▲ {urg:.1}")));

    // Row 2: where it lives — project and tags, absent rows drawn as nothing.
    let mut place: Vec<Seg> = Vec::new();
    let proj = s(task, "project");
    if !proj.is_empty() {
        place.push((Some("card.label"), proj));
    }
    if let Some(tags) = task.get("tags").and_then(Value::as_array) {
        let names: Vec<String> = tags
            .iter()
            .filter_map(Value::as_str)
            .map(|t| format!("#{}", san(t)))
            .collect();
        if !names.is_empty() {
            if !place.is_empty() {
                place.push((Some("card.frame"), " · ".into()));
            }
            place.push((None, names.join(" ")));
        }
    }

    // Row 3: when and how much — due (the fact you act on), estimate, repeat.
    let mut when: Vec<Seg> = Vec::new();
    if !s(task, "due").is_empty() {
        when.push((Some("card.label"), "due ".into()));
        when.push((Some("card.strong"), s(task, "due")));
    }
    if !s(task, "estimate").is_empty() {
        gap(&mut when);
        when.push((Some("card.label"), "est ".into()));
        when.push((None, s(task, "estimate")));
    }
    if !s(task, "recurrence").is_empty() {
        gap(&mut when);
        when.push((Some("card.label"), "↻ ".into()));
        when.push((None, s(task, "recurrence")));
    }

    let rows: Vec<Vec<Seg>> = [state, place, when]
        .into_iter()
        .filter(|r| !r.is_empty())
        .collect();
    card_box(ctx, header, rows)
}

/// The `show` card (D76): the ledger the user chose — a right-aligned gray
/// label column, terminal-default values, emphasis only where the reader
/// acts (priority H, due, a running timer, blocked=true). The row set and
/// the label SPELLINGS are the plain view's, verbatim; the parity test is
/// what keeps the two layouts naming the same facts.
fn task_detail_card(ctx: &Ctx, result: &Value) -> String {
    // "depends_on" is the widest label either layout prints.
    const LW: usize = 10;
    let lab = |name: &str| -> String {
        format!(
            "{}{}",
            " ".repeat(LW.saturating_sub(width(name))),
            ctx.paint("card.label", name)
        )
    };
    let mut out = String::new();

    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let title = s(result, "title");
    out.push_str(&format!(
        "{}  {}\n",
        ctx.paint("card.label", &format!("#{sid}")),
        ctx.paint("card.strong", &title)
    ));
    let head_w = width(&format!("#{sid}  {title}"));
    out.push_str(&ctx.paint("card.frame", &"─".repeat(head_w.clamp(4, ctx.cols.min(80)))));
    out.push('\n');

    let mut row = |label: &str, value: String| {
        out.push_str(&format!("{}  {value}\n", lab(label)));
    };

    row("status", status_cell(ctx, result));
    let prio = result
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let prio_cell = match prio {
        "H" => ctx.paint("card.strong", prio),
        "L" => ctx.paint("card.label", prio),
        "M" => prio.to_string(),
        _ => ctx.paint("card.frame", prio),
    };
    row("priority", prio_cell);
    if !s(result, "project").is_empty() {
        row("project", s(result, "project"));
    }
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    row("urgency", format!("{urg:.1}"));
    if !s(result, "due").is_empty() {
        row("due", ctx.paint("card.strong", &s(result, "due")));
    }
    if !s(result, "remind").is_empty() {
        row("remind", s(result, "remind"));
    }
    if !s(result, "scheduled").is_empty() {
        row("scheduled", s(result, "scheduled"));
    }
    if !s(result, "wait").is_empty() {
        row("wait", s(result, "wait"));
    }
    if !s(result, "recurrence").is_empty() {
        row("repeats", s(result, "recurrence"));
    }
    if !s(result, "estimate").is_empty() {
        row("estimate", s(result, "estimate"));
    }
    if !s(result, "completed").is_empty() {
        row("completed", s(result, "completed"));
    }
    let tracked = s(result, "tracked");
    if !tracked.is_empty() && tracked != "PT0S" {
        row("tracked", tracked);
    }
    if !s(result, "active_since").is_empty() {
        row(
            "running",
            ctx.paint(
                "card.strong",
                &format!("since {}", s(result, "active_since")),
            ),
        );
    }
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    row(
        "blocked",
        if blocked {
            ctx.paint("card.strong", "true")
        } else {
            "false".to_string()
        },
    );
    if let Some(tags) = result.get("tags").and_then(Value::as_array) {
        if !tags.is_empty() {
            let names: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
            row("tags", san(&names.join(" ")));
        }
    }
    if let Some(deps) = result.get("depends_on").and_then(Value::as_array) {
        if !deps.is_empty() {
            let refs: Vec<String> = deps
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            row("depends_on", refs.join(" "));
        }
    }
    if let Some(tokens) = result.get("tokens").and_then(Value::as_array) {
        if !tokens.is_empty() {
            let sum = |key: &str| -> u64 {
                tokens
                    .iter()
                    .filter_map(|m| m.get(key).and_then(Value::as_u64))
                    .fold(0u64, u64::saturating_add)
            };
            row(
                "tokens",
                format!(
                    "in {} · out {} · cacheR {} · cacheW {}",
                    sum("input_tokens"),
                    sum("output_tokens"),
                    sum("cache_read_tokens"),
                    sum("cache_creation_tokens")
                ),
            );
        }
    }
    if let Some(anns) = result.get("annotations").and_then(Value::as_array) {
        for a in anns {
            out.push_str(&format!(
                "{}  {}\n",
                lab("·"),
                san(a.get("body").and_then(Value::as_str).unwrap_or(""))
            ));
        }
    }
    out
}

/// `tasqx modify` — echo back exactly what changed, and at what rev.
///
/// The echo is the point: `modify` is the one verb that can quietly do the wrong
/// thing (a misparsed date, a field cleared that you meant to set), and a bare
/// "ok" would hide it. Cleared fields print as `field ← (cleared)` so a removal
/// never reads like a set. Values shown are the RESOLVED ones the core stored —
/// `due:friday` echoes the timestamp it actually became, which is where a
/// natural-language misread becomes visible.
pub fn modified(
    ctx: &Ctx,
    result: &Value,
    set: &serde_json::Map<String, Value>,
    tags: &[String],
) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let rev = result.get("_rev").and_then(Value::as_i64);

    let mut out = match rev {
        Some(r) => format!(
            "{}  ·  rev {r}\n",
            ctx.paint("accent", &format!("Modified #{sid}"))
        ),
        None => format!("{}\n", ctx.paint("accent", &format!("Modified #{sid}"))),
    };

    // Stable order: whatever the user typed, the report reads the same every time.
    let mut keys: Vec<&String> = set.keys().collect();
    keys.sort();
    for k in keys {
        let v = &set[k];
        let shown = if v.is_null() {
            ctx.paint("muted", "(cleared)")
        } else {
            san(v.as_str().unwrap_or(""))
        };
        out.push_str(&format!("  {} <- {shown}\n", pad(k, 11)));
    }
    if !tags.is_empty() {
        let all: Vec<String> = result
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|t| format!("+{}", san(t)))
                    .collect()
            })
            .unwrap_or_else(|| tags.iter().map(|t| format!("+{}", san(t))).collect());
        out.push_str(&format!(
            "  {} <- {}\n",
            pad("tags", 11),
            ctx.paint("tag", &all.join(" "))
        ));
    }
    out
}

/// The one-line result of a verb that only changes status — and the cascade it
/// caused, when it caused one.
///
/// `unblocked` is appended rather than branched on by verb: the key is present
/// only when the method returns it (today `task.cancel`), so the shared
/// renderer stays correct for the verbs that release nothing without needing a
/// list of which ones those are.
pub fn status_line(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let mut out = format!(
        "{}  ->  {}\n",
        ctx.paint("accent", &format!("#{sid}")),
        s(result, "status")
    );
    out.push_str(&unblocked_line(ctx, result));
    out.push_str(&reblocked_line(ctx, result));
    out
}

pub fn annotated(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let body = san(result
        .get("annotation")
        .and_then(|a| a.get("body"))
        .and_then(Value::as_str)
        .unwrap_or(""));
    format!(
        "{}: {body}\n",
        ctx.paint("accent", &format!("Annotated #{sid}"))
    )
}

/// `tasqx undo`.
///
/// Names the operation AND the task AND what came back, because "undone" on its
/// own is the one answer nobody can check: undo takes no argument, so the user
/// never named the thing it acted on and has only this line to confirm it was
/// the thing they meant.
///
/// The detail line is driven by the reverted op rather than by sniffing which
/// keys `restored` happens to carry — a response shape that grows a key must not
/// be able to silently change the sentence. An op this build has no phrasing for
/// still prints its `restored` object rather than nothing: a new entry in the
/// core's closed set would otherwise reach the terminal as a blank second line,
/// which reads as "it restored nothing".
pub fn undone(ctx: &Ctx, result: &Value) -> String {
    let op = result
        .get("reverted")
        .and_then(|r| r.get("op"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let title = san(result.get("title").and_then(Value::as_str).unwrap_or(""));
    let restored = result.get("restored").cloned().unwrap_or(Value::Null);

    let tags = |key: &str| -> String {
        restored
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|t| format!("+{}", san(t)))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    };
    let detail = match op {
        "tag.remove" => format!("tags back: {}", ctx.paint("tag", &tags("tags"))),
        "dependency.remove" => format!(
            "depends on {} again",
            ctx.paint(
                "accent",
                &format!(
                    "#{}",
                    restored
                        .get("depends_on")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                )
            )
        ),
        "stop" => format!(
            "the timer is running again  ·  {} back on the clock",
            san(restored
                .get("tracked")
                .and_then(Value::as_str)
                .unwrap_or("PT0S"))
        ),
        "annotation.add" => format!(
            "note removed: {}",
            ctx.paint(
                "muted",
                &san(restored
                    .get("annotation")
                    .and_then(Value::as_str)
                    .unwrap_or(""))
            )
        ),
        _ => format!("restored: {}", san(&restored.to_string())),
    };

    let arrow = match ctx.caps.unicode {
        true => "↩",
        false => "<-",
    };
    format!(
        "{} {}  ·  {} {}\n  {detail}\n",
        ctx.paint("accent", arrow),
        ctx.paint("accent", &format!("undid {}", san(op))),
        ctx.paint("accent", &format!("#{sid}")),
        title,
    )
}

/// `tasqx tag` / `untag`.
///
/// Both halves name what CHANGED and what the task carries now, for the reason
/// [`dep_result`] does below: `tags` in the result is the set that REMAINS, so a
/// removal rendered from it alone reads `#42 tags: +api` with no mention of what
/// went, which is the same line a call that removed nothing would print. The
/// core answers `removed` on `tag.remove` precisely so this line does not have
/// to be reconstructed from the request, and D39 asks that a field the core
/// computes reach a human surface.
///
/// `added` selects the verb rather than sniffing for the `removed` key, so a
/// response shape that changes cannot silently flip the wording.
pub fn tag_result(ctx: &Ctx, result: &Value, added: bool, asked: &[String]) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let painted = |names: &[String]| -> String {
        match names.is_empty() {
            true => ctx.paint("muted", "(none)"),
            false => ctx.paint(
                "tag",
                &names
                    .iter()
                    .map(|t| format!("+{}", san(t)))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        }
    };
    let strings = |key: &str| -> Vec<String> {
        result
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    // The remaining set comes from the core; what changed comes from the core on
    // removal (`removed`) and from the request on addition, where `tag.add` has
    // no equivalent key and re-adding an existing tag is a legitimate no-change.
    let all = strings("tags");
    let changed = match added {
        true => asked.to_vec(),
        false => {
            let removed = strings("removed");
            match removed.is_empty() {
                true => asked.to_vec(),
                false => removed,
            }
        }
    };
    let verb = match added {
        true => "tagged",
        false => "untagged",
    };
    format!(
        "{} {verb} {}   ·   tags: {}\n",
        ctx.paint("accent", &format!("#{sid}")),
        painted(&changed),
        painted(&all),
    )
}

/// `tasqx dep` / `undep`.
///
/// `depends_on` in the result is the set that REMAINS, which made the removal
/// line read `#2 no longer depends on: (none)` — indistinguishable from "the
/// removal did nothing" at a glance. So the removed edge is named from the
/// request, and the remaining set is labelled as such rather than being silently
/// substituted for it.
pub fn dep_result(ctx: &Ctx, result: &Value, added: bool, target: &str) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let deps: Vec<String> = result
        .get("depends_on")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect()
        })
        .unwrap_or_default();
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let list = if deps.is_empty() {
        "(none)".to_string()
    } else {
        deps.join(" ")
    };
    let target = san(target.trim_start_matches('#'));

    if added {
        format!(
            "{} now depends on #{target}   ·   depends on: {list}   blocked={blocked}\n",
            ctx.paint("accent", &format!("#{sid}"))
        )
    } else {
        format!(
            "{} no longer depends on #{target}   ·   still depends on: {list}   blocked={blocked}\n",
            ctx.paint("accent", &format!("#{sid}"))
        )
    }
}

pub fn project_table(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let projects = result
        .get("projects")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if projects.is_empty() {
        return "No projects.\n".to_string();
    }
    let mut out = String::new();
    // D21: the leading column is the default marker. `projects` is THE read
    // surface for "where does a bare `tasqx add` land?" — a fact that drove
    // behavior while being shown nowhere.
    out.push_str(&ctx.paint(
        "header",
        &format!(
            "{:<7}  {:<24}  {:<9}  {}",
            "DEFAULT", "PROJECT", "ARCHIVED", "DESCRIPTION"
        ),
    ));
    out.push('\n');
    for p in projects {
        let name = s(p, "name");
        let archived = p.get("archived").and_then(Value::as_bool).unwrap_or(false);
        let is_default = p.get("default").and_then(Value::as_bool).unwrap_or(false);
        let desc = san(p.get("description").and_then(Value::as_str).unwrap_or(""));
        out.push_str(&format!(
            "{:<7}  {}  {:<9}  {desc}\n",
            if is_default { "*" } else { "" },
            ctx.paint("project", &pad(&name, 24)),
            if archived { "yes" } else { "no" }
        ));
    }
    out
}

pub fn report(ctx: &Ctx, result: &Value, group_by: &str) -> String {
    let empty = Vec::new();
    let groups = result
        .get("groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if groups.is_empty() {
        return "No matching tasks.\n".to_string();
    }
    let mut out = String::new();
    // D48a: the four buckets are never blended on any output surface, and this
    // column used to be the blend. `tokens_total` answered "how much did this
    // cost?" with a number that cannot mean that — cache read is 98% of this
    // project's own volume and 68% of its cost, so the blend is wrong in the
    // flattering direction.
    //
    // Four columns is what the HTML report gets; here they would take the row
    // from 74 characters to ~104, past any usable terminal. So the terminal
    // names the largest bucket and its own count instead: no blend, no derived
    // figure, same width. `tasqx report --json` and the HTML page carry all four
    // for anyone who needs the split.
    out.push_str(&ctx.paint(
        "header",
        &format!(
            "{:<20}  {:>5}  {:>10}  {:>7}  {:>10}  {:>12}",
            group_by.to_uppercase(),
            "COUNT",
            "EST",
            "OVERDUE",
            "TRACKED",
            "TOKENS"
        ),
    ));
    out.push('\n');
    for g in groups {
        let key = san(g.get(group_by).and_then(Value::as_str).unwrap_or(""));
        let count = g.get("count").and_then(Value::as_i64).unwrap_or(0);
        let est = g.get("est_total").and_then(Value::as_str).unwrap_or("-");
        let overdue = g.get("overdue").and_then(Value::as_i64).unwrap_or(0);
        let tracked = g
            .get("tracked_total")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let tokens = crate::tokens::dominant_cell(g);
        let overdue_cell = format!("{overdue:>7}");
        let overdue_p = if overdue > 0 {
            ctx.paint("warn", &overdue_cell)
        } else {
            ctx.paint("muted", &overdue_cell)
        };
        out.push_str(&format!(
            "{}  {count:>5}  {est:>10}  {overdue_p}  {tracked:>10}  {tokens:>12}\n",
            ctx.paint("project", &pad(&key, 20))
        ));
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
/// The totals line carries the engine's blended before/after — a migration
/// delta, deliberately not a report surface (the D48 no-blend rule is about
/// reporting spend, and labelling this pair as the delta is what keeps it from
/// reading as one).
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

    let before = result["totals"]["before"].as_i64().unwrap_or(0);
    let after = result["totals"]["after"].as_i64().unwrap_or(0);
    out.push_str(&format!(
        "totals  {before} -> {after} tokens (blended migration delta)  ·  {} task(s) in scope, {unchanged} unchanged\n",
        tasks.len()
    ));
    if dry_run {
        out.push_str("Dry-run: nothing was written. Run `tasqx tokens recompute --apply` to perform this repair.\n");
    }
    out
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

pub fn next_task(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    match tasks.first() {
        None => "Nothing actionable — you're clear.\n".to_string(),
        Some(t) => {
            let sid = t.get("short_id").and_then(Value::as_i64).unwrap_or(0);
            let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
            format!(
                "{}  (urgency {urg:.1})  {}\n",
                ctx.paint("accent", &format!("#{sid}")),
                s(t, "title")
            )
        }
    }
}

/// Urgency breakdown (`tasqx why`), computed from the task.get fields via the
/// same D1 formula the engine uses — so ranking is never a black box.
pub fn why(ctx: &Ctx, result: &Value) -> String {
    use tasqx_core::{urgency, Priority};
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let prio = result
        .get("priority")
        .and_then(Value::as_str)
        .and_then(Priority::parse);
    let due = result.get("due").and_then(Value::as_str);
    let created = result.get("created").and_then(Value::as_str).unwrap_or("");
    why_rows(ctx, sid, &urgency::breakdown(prio, due, created))
}

/// Render a breakdown the caller has already computed.
///
/// `parts` is a PARAMETER for the same reason `chart::render_throughput`'s
/// series is: [`why`] reaches `urgency::breakdown` through the wall clock, so
/// a test that could only reach this code through it would not get to choose
/// the value under test — and the value that broke this display (`-0.0`, from
/// `(-age).max(0.0)` when `created` lands in the very second the clock is
/// read) is one a test cannot schedule there, while `urgency::breakdown_at`
/// plus this seam can stage it exactly.
fn why_rows(ctx: &Ctx, sid: i64, parts: &[(&'static str, f64)]) -> String {
    let total: f64 = parts.iter().map(|(_, v)| v).sum();
    let total = (total * 10.0).round() / 10.0;

    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!("Why #{sid} has urgency {}", signed(total, 1)),
    ));
    out.push('\n');
    for (name, val) in parts {
        out.push_str(&format!("  {name:<14} {:>6}\n", signed(*val, 2)));
    }
    out.push_str(&format!("  {:<14} {:>6}\n", "= total", signed(total, 1)));
    out
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
/// matching the rest of the glyph gating (hrule/arrow/mid/chart bars).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{self, Caps, Ctx};
    use crate::AGENDA_DEFAULT_DAYS;
    use serde_json::json;

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

    #[test]
    fn san_strips_control_and_escape_bytes() {
        // A title carrying a screen-clear + OSC title-set + cursor move: every
        // control byte is removed, printable text survives.
        let malicious = "\x1b[2Jpwned\x1b]0;evil\x07\x08 ok\ttab";
        let clean = san(malicious);
        assert!(!clean.contains('\x1b'), "escape byte leaked: {clean:?}");
        assert!(!clean.contains('\x07') && !clean.contains('\x08'));
        assert_eq!(clean, "[2Jpwned]0;evil ok\ttab", "printable kept, tab kept");
    }

    #[test]
    fn task_detail_shows_remind_when_set() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let base = json!({
            "short_id": 1, "title": "probe", "status": "pending", "urgency": 8.2,
            "project": "work", "due": "2026-07-20T17:00:00Z", "remind": "-1h",
            "estimate": "PT4H"
        });
        let out = task_detail(&ctx, &base);
        assert!(out.contains("remind"), "remind row missing: {out:?}");
        assert!(out.contains("-1h"), "remind value missing: {out:?}");
        // `estimate` is settable via `est:` sugar and totalled by `report`, so the
        // detail view must show it too — an invisible field is how the dependency
        // bug stayed hidden.
        assert!(out.contains("estimate"), "estimate row missing: {out:?}");
        assert!(out.contains("PT4H"), "estimate value missing: {out:?}");

        // Absent remind must stay absent — the row is conditional, like `due`.
        let mut bare = base.clone();
        bare["remind"] = json!("");
        assert!(!task_detail(&ctx, &bare).contains("remind"));
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
            "annotations": [{"body": "called the plumber"}]
        })
    }

    /// D76's gate: the card belongs to VT terminals only. The plain path is
    /// what every pipe, script and executed docs example reads, so it must
    /// keep its exact old layout and never leak a frame glyph.
    #[test]
    fn the_cards_render_only_on_a_unicode_terminal() {
        let t = full_task();
        let plain = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &t);
        assert!(
            plain.contains("  status     "),
            "the plain detail layout changed: {plain:?}"
        );
        assert!(
            !plain.contains('╭') && !plain.contains('─'),
            "card glyphs leaked into the plain path: {plain:?}"
        );
        let card = task_detail(&Ctx::new(theme::default_theme(), card_caps()), &t);
        assert!(
            card.contains('─') && !card.contains("  status     "),
            "unicode caps should render the ledger card: {card:?}"
        );
        let added = task_added_card(&Ctx::new(theme::default_theme(), card_caps()), &t);
        assert!(
            added.contains('╭'),
            "the add card lost its frame: {added:?}"
        );
    }

    /// The frame is only worth drawing if it closes: every line the same cell
    /// width, empty rows not drawn at all, and a 300-cell title truncated into
    /// the border instead of bursting it.
    #[test]
    fn the_add_card_frame_stays_closed() {
        let ctx = Ctx::new(theme::default_theme(), card_caps());
        let out = task_added_card(&ctx, &full_task());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5, "header + 3 rows + bottom:\n{out}");
        let w = width(lines[0]);
        for line in &lines {
            assert_eq!(width(line), w, "ragged frame:\n{out}");
        }

        // No project, tags, due, estimate or recurrence: one state row only.
        let bare = json!({"short_id": 7, "title": "bare", "status": "pending",
                          "urgency": 0.0});
        let out = task_added_card(&ctx, &bare);
        assert_eq!(
            out.lines().count(),
            3,
            "empty rows must not be drawn:\n{out}"
        );

        // A title longer than any terminal still yields a closed, capped box.
        let mut long = full_task();
        long["title"] = json!("x".repeat(300));
        let out = task_added_card(&ctx, &long);
        let lines: Vec<&str> = out.lines().collect();
        let w = width(lines[0]);
        assert!(w <= 80, "the card must cap its width, got {w}");
        for line in &lines {
            assert_eq!(width(line), w, "long title burst the frame:\n{out}");
        }
    }

    /// The two detail layouts must name the same facts, in the same
    /// spellings. The plain view is the contract (its labels are what docs
    /// and muscle memory know); this walks its label column and demands each
    /// one appear in the card. Rename a row in one layout and not the other
    /// and this is what goes red.
    #[test]
    fn the_detail_card_names_every_field_the_plain_view_names() {
        let t = full_task();
        let plain = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &t);
        let card = task_detail(&Ctx::new(theme::default_theme(), card_caps()), &t);
        let mut labels = 0;
        for line in plain.lines().skip(1) {
            let Some(first) = line.split_whitespace().next() else {
                continue;
            };
            if first == "·" {
                continue; // an annotation marker, not a field label
            }
            assert!(
                card.contains(first),
                "the card lost the {first:?} row:\n{card}"
            );
            labels += 1;
        }
        assert!(
            labels >= 15,
            "the parity scan saw only {labels} labels — full_task stopped \
             exercising the conditional rows and this guard is checking little"
        );
    }

    /// D50: `tokens_hint` targets machine callers who see raw JSON. The CLI
    /// `done` verb has no token flags, so printing the hint would recommend
    /// the impossible. The fixture carries the key deliberately — present and
    /// deliberately unrendered, the same shape as the D48a tokens_total guard:
    /// a payload without it could not tell rendering from absence.
    #[test]
    fn done_never_renders_the_tokens_hint() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = done(
            &ctx,
            &json!({
                "status": "done",
                "completed": "2026-07-31T10:00:00Z",
                "unblocked": [],
                "tokens_hint": "no token counts were self-reported; log-parse \
                    attribution is a best-effort fallback"
            }),
        );
        assert!(
            out.contains("Done"),
            "the completion line itself went missing: {out:?}"
        );
        assert!(
            !out.contains("tokens_hint") && !out.contains("self-reported"),
            "the machine-only hint reached the terminal: {out:?}"
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
        assert!(!task_row(&t, 1.0, at("2026-08-31T11:59:59Z")).overdue);
        assert!(task_row(&t, 1.0, at("2026-08-31T12:00:01Z")).overdue);
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
        assert!(
            out.contains("DEFAULT"),
            "no default column on the projects table: {out:?}"
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

    /// A bare `add` inherits the default, so the confirmation has to say where
    /// the task actually went — otherwise the landing project stays invisible
    /// at the exact moment it matters.
    #[test]
    fn task_added_names_the_project_it_landed_in() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_added(
            &ctx,
            &json!({ "short_id": 3, "status": "pending", "urgency": 5.0, "project": "work" }),
            "a task",
        );
        assert!(
            out.contains("work"),
            "the landing project is invisible: {out:?}"
        );

        // Projectless stays quiet rather than printing an empty field.
        let none = task_added(
            &ctx,
            &json!({ "short_id": 4, "status": "pending", "urgency": 5.0, "project": null }),
            "homeless",
        );
        assert!(
            !none.contains("project"),
            "printed an empty project row: {none:?}"
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
        let rows: Vec<&str> = out.lines().skip(2).take(AWKWARD.len()).collect();
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
            let rows: Vec<&str> = out.lines().skip(2).take(AWKWARD.len()).collect();
            let want = cells(rows[0]);
            for (row, v) in rows.iter().zip(AWKWARD) {
                assert_eq!(cells(row), want, "{field}={v:?} broke alignment: {row:?}");
            }
        }
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

        let head = |t: &Value| {
            task_table(&ctx, t, Timestamp::now())
                .lines()
                .next()
                .unwrap()
                .to_string()
        };
        assert!(head(&with_due).contains("DUE"), "{}", head(&with_due));
        assert!(
            !head(&without_due).contains("DUE"),
            "an empty DUE column was still drawn: {:?}",
            head(&without_due)
        );
        // And the title survives whole once the dead column is gone.
        let row = task_table(&ctx, &without_due, Timestamp::now())
            .lines()
            .nth(2)
            .unwrap()
            .to_string();
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
        let out = report(&ctx, &json!({ "groups": groups }), "project");
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
        );
        let row = out.lines().nth(1).unwrap();
        assert!(row.trim_end().ends_with('-'), "expected a dash: {row:?}");
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
        let out = why_rows(
            &ctx,
            1,
            &[("priority", 0.0), ("due_proximity", 0.0), ("age", -0.0)],
        );
        assert!(
            !out.contains("-0"),
            "a component rendered as negative zero: {out:?}"
        );
        assert!(
            out.contains("0.00"),
            "the zero itself must still be shown: {out:?}"
        );

        // Every part negative-zero makes the SUM negative zero too, which is the
        // heading and the total row — the twin the component fix does not reach.
        let all_neg = why_rows(&ctx, 1, &[("priority", -0.0), ("age", -0.0)]);
        assert!(
            !all_neg.contains("-0"),
            "the total rendered as negative zero: {all_neg:?}"
        );

        // The rule is about a sign that survived ROUNDING, not about the value
        // being exactly zero: -0.004 is genuinely negative and still prints as a
        // row of zeros, so `v == 0.0` would not have caught it.
        let tiny = why_rows(&ctx, 1, &[("age", -0.004)]);
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
        let out = why_rows(&ctx, 1, &[("penalty", -1.5), ("priority", 6.0)]);
        assert!(
            out.contains("-1.50"),
            "a real negative lost its sign: {out:?}"
        );
        assert!(out.contains("4.5"), "the total must still net out: {out:?}");
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
        assert!(
            out.contains("7101 -> 1101"),
            "the totals line must carry the delta: {out:?}"
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
        let out = undone(&ctx, &result);
        assert!(
            out.contains("tag.remove"),
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
            undone(
                &ctx,
                &json!({
                    "reverted": { "event": "e1", "op": op, "ts": "t" },
                    "short_id": 7,
                    "title": "t",
                    "restored": restored,
                }),
            )
        };

        assert!(line("dependency.remove", json!({ "depends_on": 3 })).contains("#3"));
        let stopped = line("stop", json!({ "tracked": "PT30M", "status": "active" }));
        assert!(
            stopped.contains("PT30M") && stopped.contains("running"),
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
        let out = undone(
            &ctx,
            &json!({
                "reverted": { "event": "e1", "op": "annotation.add", "ts": "t" },
                "short_id": 1,
                "title": "\u{1b}[2Jclear",
                "restored": { "annotation": "\u{1b}]0;evil\u{7}note" },
            }),
        );
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
        let out = tag_result(&ctx, &result, false, &["api".to_string()]);
        assert!(out.contains("untagged"), "{out:?}");
        assert!(out.contains("+api"), "the removed tag must appear: {out:?}");
        assert!(
            out.contains("+release"),
            "the remaining set must appear: {out:?}"
        );
    }

    /// The addition half, and the empty case. `tag.add` returns no `removed`
    /// key, so the changed set comes from the request there; and a task left
    /// with no tags renders `(none)` rather than a blank where a list belongs.
    #[test]
    fn a_tag_line_names_the_added_tag_and_an_empty_set_says_so() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let added = json!({ "short_id": 7, "tags": ["api", "release"] });
        let out = tag_result(&ctx, &added, true, &["api".to_string()]);
        assert!(out.contains("#7") && out.contains("tagged"), "{out:?}");
        assert!(out.contains("+api") && out.contains("+release"), "{out:?}");
        assert!(!out.contains("untagged"), "the verb must not flip: {out:?}");

        let emptied = json!({ "short_id": 7, "tags": [], "removed": ["api"] });
        let out = tag_result(&ctx, &emptied, false, &["api".to_string()]);
        assert!(
            out.contains("(none)"),
            "a task with no tags left must say so, not print a blank: {out:?}"
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
        let out = tag_result(&ctx, &result, false, &[]);
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

    fn agenda_of(tasks: Vec<Value>, days: usize) -> Agenda {
        agenda_select(&json!({ "tasks": tasks }), days, anchor())
    }

    fn agenda_out(tasks: Vec<Value>, days: usize) -> String {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        agenda_text(&ctx, &agenda_of(tasks, days))
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
        let a = agenda_of(
            vec![
                dated(1, "deadline only", "2026-08-05T00:00:00Z", ""),
                dated(
                    2,
                    "starts tuesday, due much later",
                    "2026-08-24T00:00:00Z",
                    "2026-08-04T00:00:00Z",
                ),
                dated(3, "scheduled only", "", "2026-08-10T00:00:00Z"),
            ],
            30,
        );
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
        let a = agenda_of(
            vec![dated(
                1,
                "same instant",
                "2026-08-05T09:00:00Z",
                "2026-08-05T09:00:00Z",
            )],
            30,
        );
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
        assert!(out.contains("through 2026-08-17 (+14d)"), "{out}");

        // Raising it brings them in, which is the other half of the promise.
        let wide = agenda_out(tasks, 90);
        assert!(wide.contains("far outside"), "{wide}");
        assert!(
            !wide.contains("further out"),
            "nothing is beyond a horizon that reaches everything: {wide}"
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
        // The date, not a bare `due`: which day it was late on is the whole
        // content of this group.
        assert!(out.contains("due 2026-07-01"), "{out}");
        // A time-of-day is shown only where the store has one. `2026-07-01` was
        // typed without a time and resolves to midnight, so printing `00:00`
        // would be a time nobody entered.
        assert!(!out.contains("00:00"), "{out}");
        assert!(out.contains("due 17:00"), "a real time is shown: {out}");
    }

    /// Today and tomorrow get their words, and every heading carries the date
    /// anyway -- "Today" alone is the one label that means something else
    /// tomorrow, and terminal output gets pasted into tickets.
    #[test]
    fn day_headings_name_the_relative_day_and_the_date() {
        let out = agenda_out(
            vec![
                dated(1, "a", "2026-08-03T00:00:00Z", ""),
                dated(2, "b", "2026-08-04T00:00:00Z", ""),
                dated(3, "c", "2026-08-06T00:00:00Z", ""),
            ],
            14,
        );
        assert!(out.contains("Today"), "{out}");
        assert!(out.contains("Mon 2026-08-03"), "{out}");
        assert!(out.contains("Tomorrow"), "{out}");
        assert!(out.contains("Tue 2026-08-04"), "{out}");
        assert!(out.contains("\nThu 2026-08-06\n"), "{out}");
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
            // Up to and including the closing rule. What follows it is prose:
            // the count line and the omission notes are sentences carrying a
            // command to paste, and a terminal narrower than a sentence gets a
            // wrapped sentence rather than a truncated instruction -- the same
            // treatment `task_table` gives its store-health warnings.
            let mut rules = 0;
            for line in out.lines() {
                assert!(
                    cells(line) <= cols,
                    "a {}-cell line in a {cols}-cell terminal: {line:?}",
                    cells(line)
                );
                if !line.is_empty() && line.chars().all(|c| c == '-' || c == '\u{2500}') {
                    rules += 1;
                    if rules == 2 {
                        break;
                    }
                }
            }
            assert_eq!(rules, 2, "the table must have a rule above and below it");
        }
    }

    /// `--json` and the table answer the same question. The raw `task.list`
    /// result would have made `tasqx agenda --json | jq .tasks` count every
    /// matching task -- horizon and undated rows included -- while the table
    /// beside it showed one.
    #[test]
    fn agenda_json_holds_exactly_the_rows_the_table_drew() {
        let a = agenda_of(
            vec![
                dated(1, "shown", "2026-08-05T00:00:00Z", ""),
                dated(2, "beyond", "2026-11-01T00:00:00Z", ""),
                dated(3, "undated", "", ""),
            ],
            14,
        );
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
        for note in &notes {
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
        let a = agenda_of(vec![hot, cold], 14);
        assert_eq!(
            a.entries
                .iter()
                .map(|e| e.task["short_id"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}
