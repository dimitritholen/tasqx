//! One body-builder per panel: model in, styled lines out.
//!
//! Nothing here draws chrome, and nothing here knows where on the screen it is
//! — a builder is handed a width, a height and a cursor, and returns at
//! most that many lines. The screen composites; these fill.
//!
//! **Every string is cut in Rust before it becomes a `Span`.** ratatui does clip
//! a `Paragraph` to its rect, but a silent clip loses the ellipsis that tells
//! the reader something was cut, and a double-width grapheme straddling the last
//! cell is exactly the artefact `unicode-truncate` exists to prevent. Widths are
//! measured in CELLS via `render::width`, never in chars.

use ratatui::style::Style as RtStyle;
use ratatui::text::{Line, Span};

use crate::chart;
use crate::render;
use crate::theme::{Caps, Theme};
use crate::tokens;
use crate::tui::{first_visible, rt_style};

use jiff::civil::Date;
use jiff::tz::TimeZone;

use super::model::{Dashboard, Detail, PanelId, Prio, Sort, StatusBar, Task, TaskGroup};

/// Where the reader is in this panel, and whether this panel is the one they
/// are reading.
///
/// `row` places the viewport in every list panel — a panel keeps its position
/// while focus is elsewhere, so Tab comes back to where you left off. `shown`
/// only decides whether the glyph is drawn: eight panels each drawing a cursor
/// would say nothing about which one the keys act on, and the rule already
/// carries panel focus.
///
/// The two-cell column is reserved on EVERY row of a list panel whether or not
/// the glyph lands on it — the `id_width` lesson, one column over. A column that
/// appears only under the cursor reflows the panel on every `j`, and a column
/// that appears only on focus makes Tab shift the text sideways.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cursor {
    pub row: usize,
    pub shown: bool,
}

/// The reserved cursor cell for one row: the glyph, or the space it would take.
///
/// A glyph and not a colour. `theme::Style` has no `bg` field so `rt_style` can
/// never produce one, and a cursor drawn in colour alone is invisible under
/// `NO_COLOR` — the reasoning `rule_label` already records for panel focus.
fn cursor_cell(at: bool, s: &Styles, unicode: bool) -> Span<'static> {
    let glyph = if !at {
        "  "
    } else if unicode {
        "▸ "
    } else {
        "> "
    };
    Span::styled(glyph, if at { s.accent } else { s.muted })
}

/// `4h`, `6h12`, `45m`, `30s` — a duration at a glance.
///
/// Deliberately not `humanize_secs` from core's markdown module: that one
/// rounds (`90m` reads as `2h`), which is right for prose and wrong for a
/// column where `6h12` beside `est 4h` is the whole point.
pub fn dur_compact(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3600 {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m:02}")
        }
    } else if s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// Right-align `right` against `left` inside `cells`, cutting `left` if needed.
fn split_row(left: &str, right: &str, cells: usize, unicode: bool) -> String {
    let rw = render::width(right);
    if rw + 1 >= cells {
        return render::truncate(right, cells, unicode);
    }
    let room = cells - rw - 1;
    let l = render::truncate(left, room, unicode);
    format!("{}{}{}", l, " ".repeat(room - render::width(&l) + 1), right)
}

struct Styles {
    muted: RtStyle,
    accent: RtStyle,
    warn: RtStyle,
    danger: RtStyle,
    overdue: RtStyle,
    active: RtStyle,
    project: RtStyle,
    /// The burndown's reference line. Its own role (D80) rather than `muted`,
    /// so a theme can pull the ideal apart from the data without also
    /// repainting every dim label on the screen.
    chart_ideal: RtStyle,
    plain: RtStyle,
}

fn styles(theme: &Theme, caps: &Caps) -> Styles {
    Styles {
        muted: rt_style(theme.role("muted"), caps),
        accent: rt_style(theme.role("accent"), caps),
        warn: rt_style(theme.role("warn"), caps),
        danger: rt_style(theme.role("danger"), caps),
        overdue: rt_style(theme.role("overdue"), caps),
        active: rt_style(theme.role("timer.active"), caps),
        project: rt_style(theme.role("project"), caps),
        chart_ideal: rt_style(theme.role("chart.ideal"), caps),
        plain: RtStyle::default(),
    }
}

/// A sentence for a panel with nothing to show.
///
/// An empty body reads as a hung screen — the rule `pick` states in its own
/// tests. Every panel here answers, even when the answer is "nothing".
fn empty(text: &str, s: &Styles, cells: u16, unicode: bool) -> Vec<Line<'static>> {
    vec![Line::from(Span::styled(
        render::truncate(text, cells as usize, unicode),
        s.muted,
    ))]
}

/// Everything a panel body needs to know about the frame it draws into, as
/// one value. `next_body` used to carry eight loose parameters under a
/// too_many_arguments allow, three of them derivable from the others (`s`
/// from theme+caps, `unicode` from caps) and rebuilt its ramp styles from a
/// second theme+caps pair — this struct is that named pain, closed.
struct PanelCtx<'a> {
    s: Styles,
    w: usize,
    height: u16,
    theme: &'a Theme,
    caps: &'a Caps,
}

impl PanelCtx<'_> {
    fn unicode(&self) -> bool {
        self.caps.unicode
    }
}

/// Build one panel's body.
#[allow(clippy::too_many_arguments)]
pub fn body(
    id: PanelId,
    // Unused since D80: every body is handed a height and fills it, and the
    // one builder that branched on the level (`now_body`) folded into TASKS.
    // Kept in the signature because `layout` still computes a level and the
    // caller still has one to pass; dropping it there is a separate change.
    _detail: Detail,
    dash: &Dashboard,
    width: u16,
    height: u16,
    cursor: Cursor,
    theme: &Theme,
    caps: &Caps,
) -> Vec<Line<'static>> {
    let ctx = PanelCtx {
        s: styles(theme, caps),
        w: width as usize,
        height,
        theme,
        caps,
    };
    let mut lines = match id {
        PanelId::Tasks => tasks_body(dash, &ctx, cursor),
        PanelId::Projects => projects_body(dash, &ctx, cursor),
        PanelId::Burndown => burndown_body(dash, &ctx),
        PanelId::Tokens => tokens_body(dash, &ctx),
        PanelId::Slot => Vec::new(),
    };
    lines.truncate(height as usize);
    lines
}

/// `(first line, lines to draw, lines still hidden below)` for a list panel,
/// placed so the cursor's line is on screen.
///
/// The viewport is DERIVED here and stored nowhere. `App` keeps a row index and
/// is deliberately never told the terminal's height — the same division
/// `pick::first_visible` makes, and this is that function, reused rather than
/// re-derived.
///
/// Two shipped defects are encoded in the arithmetic and must survive:
///
/// * The marker line is only reserved when there is something to mark.
///   Reserving it unconditionally left a blank row at the bottom of every panel
///   scrolled to its end — the marker had nothing to say, and the row it had
///   taken stayed empty.
/// * A single-row panel spends its row on DATA, never on the count of the data
///   it is not showing. With `visible == 1` the marker took the only line there
///   was, so BLOCKED and RECENT at 80x24 read `…3 more` and `…31 more` and
///   showed no task at any position — panels that had become counters for their
///   own emptiness.
///
/// The marker line is reserved BEFORE the viewport is placed, not after. Sized
/// against the unreduced height, the last screenful puts the cursor on the
/// `…N more` row.
fn window(cursor: usize, len: usize, visible: usize) -> (usize, usize, usize) {
    let visible = visible.max(1);
    let start = first_visible(cursor, len, visible);
    if len - start <= visible || visible == 1 {
        // The last screenful, or a viewport with no line to spare: nothing is
        // hidden below, so no line is reserved and the panel fills.
        return (start, (len - start).min(visible), 0);
    }
    // Something IS hidden below, so the marker takes a line and the viewport is
    // placed AGAIN against the room that is left. Placed once, against the
    // unreduced height, the last row of a scrolled panel is the `…N more` line
    // and the cursor sits on it.
    let room = visible - 1;
    let start = first_visible(cursor, len, room);
    let drawn = if len - start > room {
        room
    } else {
        (len - start).min(visible)
    };
    (start, drawn, len - start - drawn)
}

fn more_line(hidden: usize, s: &Styles, w: usize, unicode: bool) -> Option<Line<'static>> {
    (hidden > 0).then(|| {
        Line::from(Span::styled(
            render::truncate(&format!("…{hidden} more"), w, unicode),
            s.muted,
        ))
    })
}

/// The TASKS panel: one list, grouped by project, in the same idiom `tasqx
/// list` draws (D117).
///
/// Five panels folded in here. The running timer and anything blocked are the
/// LEFT RAIL — `▶` and `⊘`, the two states a reader needs before any other, in
/// the column the eye crosses first. The deadline is a right-hand column
/// carrying the same calendar words `due_cell` writes. What was RECENT is a
/// sort, cycled with `s`.
///
/// The row is `list`'s: rail, id, priority and the urgency gauge, title, tags.
/// Two screens drawing the same rows two different ways is what the house style
/// exists to stop, and the dashboard was the screen still doing it.
fn tasks_body(dash: &Dashboard, ctx: &PanelCtx, cursor: Cursor) -> Vec<Line<'static>> {
    let (s, w, height, unicode) = (&ctx.s, ctx.w, ctx.height, ctx.unicode());
    let t = &dash.tasks;
    if t.total == 0 {
        return empty(
            "nothing open — everything is done or waiting",
            s,
            w as u16,
            unicode,
        );
    }

    // The drawn body is headings and rows interleaved, but the CURSOR walks
    // rows only, so the window is computed over rows and the headings are laid
    // in around whatever it chose.
    //
    // Headings are dropped entirely when they would cost more than they say. A
    // panel with one row of space spent it on `▍ work — 5 open` and drew no
    // task at all, which is a heading for nothing: the group's whole meaning is
    // the rows under it. Half the panel is the line — below that the rows win,
    // and the project is still on every row's own group when the panel grows.
    let headings = t.sort != Sort::Touched && height as usize > t.groups.len() * 2;
    let reserved = if headings { t.groups.len() } else { 0 };
    let visible_rows = (height as usize).saturating_sub(reserved).max(1);
    let (start, room, hidden) = window(cursor.row, t.total, visible_rows);

    let id_w = t
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .map(|task| render::width(&format!("#{}", task.short_id)))
        .max()
        .unwrap_or(3);
    let gauge_w = if unicode { 5 } else { 0 };
    // cursor(2) + rail(2) + id + gap + priority(1) + space + gauge
    // + urgency(4) + gap
    let head_w = 2 + 2 + id_w + 2 + 2 + gauge_w + 4 + 2;

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut idx = 0usize; // the row index the cursor is expressed in
    for g in &t.groups {
        let first = idx;
        let last = idx + g.rows.len();
        idx = last;
        // A group entirely above or below the window contributes nothing —
        // not even its heading, which would otherwise announce a project whose
        // rows are all off screen.
        if last <= start || first >= start + room {
            continue;
        }
        if headings {
            out.push(group_heading(g, s, w, unicode));
        }
        for (n, task) in g.rows.iter().enumerate() {
            let at = first + n;
            if at < start || at >= start + room {
                continue;
            }
            out.push(task_line(
                task,
                at == cursor.row && cursor.shown,
                t.max_urgency,
                dash.today,
                ctx,
                id_w,
                head_w,
            ));
        }
    }
    if let Some(line) = more_line(hidden, s, w, unicode) {
        out.push(line);
    }
    out
}

/// `▍ fin-9695 — 5 open · 1 overdue`, the heading over one project's rows.
///
/// The same shape `agenda` gives a day heading, and for the same reason: a
/// group that is not announced is a change of subject the reader has to infer
/// from the rows themselves.
fn group_heading(g: &TaskGroup, s: &Styles, w: usize, unicode: bool) -> Line<'static> {
    let bar = if unicode { "▍" } else { "|" };
    let name = g.project().unwrap_or("(no project)").to_string();
    let mut tail = format!(" — {} open", g.rows.len());
    if g.overdue > 0 {
        tail.push_str(&format!(" · {} overdue", g.overdue));
    }
    Line::from(vec![
        // Two cells of nothing, so the heading starts where the cursor column
        // does and the bar sits above the rail rather than beside it.
        Span::styled("  ".to_string(), s.muted),
        Span::styled(format!("{bar} "), s.muted),
        Span::styled(
            render::truncate(&name, w.saturating_sub(render::width(&tail) + 2), unicode),
            s.project,
        ),
        Span::styled(tail, if g.overdue > 0 { s.overdue } else { s.muted }),
    ])
}

/// One task row, in `tasqx list`'s layout.
#[allow(clippy::too_many_arguments)]
fn task_line(
    t: &Task,
    on_cursor: bool,
    max_urgency: f64,
    today: Date,
    ctx: &PanelCtx,
    id_w: usize,
    head_w: usize,
) -> Line<'static> {
    let (s, w, unicode) = (&ctx.s, ctx.w, ctx.unicode());

    // The rail: what is running and what is stuck, before anything else.
    // Blocked outranks the timer — a task can be blocked while its own timer
    // runs, and the thing that stops you working is the one to say first.
    let (rail, rail_style) = if t.blocked {
        (if unicode { "⊘" } else { "B" }, s.danger)
    } else if t.active_since.is_some() {
        // `*` rather than `>`: the cursor column two cells to the left draws
        // `>` without Unicode, and the same glyph twice on one row is the
        // ambiguity the rail exists to remove.
        (if unicode { "▶" } else { "*" }, s.active)
    } else {
        (" ", s.plain)
    };

    let prio = t
        .priority
        .map_or("-".to_string(), |p| p.as_str().to_string());
    let prio_style = match t.priority {
        Some(Prio::H) => s.danger,
        Some(Prio::M) => s.warn,
        _ => s.muted,
    };
    let gauge = if unicode {
        let (bar, track) = render::urgency_meter(t.urgency / max_urgency);
        Some((bar, track))
    } else {
        None
    };

    // Midnight of the dashboard's own `today`, not the wall clock: the screen
    // has one instant every relative cell is measured against, and a renderer
    // that read the clock itself would disagree with the row above it whenever
    // a redraw straddled midnight.
    let midnight = today
        .to_zoned(TimeZone::UTC)
        .map(|z| z.timestamp())
        .unwrap_or_else(|_| jiff::Timestamp::UNIX_EPOCH);
    let due = t
        .due
        .map(|d| render::due_cell(d, midnight))
        .unwrap_or_default();
    let overdue = t.due_date().is_some_and(|d| d < today);
    // Capped the way `TaskCols::MAX_TAGS` caps it in `list`: the tail of a tag
    // list identifies far less than its head, and every cell it takes comes off
    // the title, which is the column the row is read for.
    let tags = render::truncate(
        &t.tags()
            .iter()
            .map(|g| format!("+{g}"))
            .collect::<Vec<_>>()
            .join(" "),
        28,
        unicode,
    );

    // What is left for the title once the fixed columns are taken out. The
    // trailing ones are dropped rather than squeezed: a two-cell tag column
    // says nothing, and the row it truncates is the one being read.
    let tail_w = render::width(&due)
        + if tags.is_empty() {
            0
        } else {
            render::width(&tags) + 2
        };
    let title_w = w.saturating_sub(head_w + tail_w + 2).max(10);

    let mut spans = vec![
        // The cursor keeps its own column, reserved on every row whether it is
        // drawn or not — a marker that only occupies space when it is showing
        // shifts every row beside it as the reader moves.
        cursor_cell(on_cursor, s, unicode),
        Span::styled(format!("{rail} "), rail_style),
        Span::styled(
            format!("{:>id_w$}  ", format!("#{}", t.short_id)),
            if on_cursor { s.accent } else { s.muted },
        ),
        Span::styled(format!("{prio} "), prio_style),
    ];
    if let Some((bar, track)) = gauge {
        let ramp = rt_style(ctx.theme.ramp_style(t.urgency / max_urgency), ctx.caps);
        spans.push(Span::styled(bar, ramp));
        spans.push(Span::styled(track, s.muted));
        spans.push(Span::styled(" ".to_string(), s.plain));
    }
    // The figure takes the ramp, exactly as `list` paints it — the two screens
    // shading the same task differently is what the shared denominator exists
    // to prevent.
    spans.push(Span::styled(
        format!("{:>4}  ", format!("{:.1}", t.urgency)),
        rt_style(ctx.theme.ramp_style(t.urgency / max_urgency), ctx.caps),
    ));
    spans.push(Span::styled(
        render::pad(&render::truncate(t.title(), title_w, unicode), title_w),
        if on_cursor { s.accent } else { s.plain },
    ));
    if !tags.is_empty() {
        spans.push(Span::styled(format!("  {tags}"), s.project));
    }
    // The running task shows how long it has been running, where every other
    // row shows its deadline. That is the NOW card's number, kept when the card
    // went: `▶` alone says which task and not for how long.
    if let Some(secs) = t.running_secs {
        spans.push(Span::styled(format!(" {}", dur_compact(secs)), s.active));
    } else if !due.is_empty() {
        spans.push(Span::styled(
            format!(" {due}"),
            if overdue { s.overdue } else { s.muted },
        ));
    }
    Line::from(spans)
}

fn projects_body(dash: &Dashboard, ctx: &PanelCtx, cursor: Cursor) -> Vec<Line<'static>> {
    let (s, w, height, unicode) = (&ctx.s, ctx.w, ctx.height, ctx.unicode());
    let rows = &dash.projects.rows;
    if rows.is_empty() {
        return empty("no projects yet — tasqx init <name>", s, w as u16, unicode);
    }
    let visible = height as usize;
    let (start, room, hidden) = window(cursor.row, rows.len(), visible);
    let mut out: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(room)
        .map(|(n, r)| {
            let star = if r.is_default { "*" } else { " " };
            let name = r.name().unwrap_or("(none)");
            let mut tail = if r.overdue > 0 {
                format!("{} open · {} overdue", r.open, r.overdue)
            } else {
                format!("{} open", r.open)
            };
            // Estimate and tracked come from `report.summary` and stay there:
            // deriving them locally would fork D24's scope rule, which is the
            // thing that decides whether cancelled work counts.
            if r.tracked_secs > 0 || r.est_secs > 0 {
                let extra = format!(
                    " · {}/{}",
                    dur_compact(r.tracked_secs),
                    dur_compact(r.est_secs)
                );
                if render::width(&tail) + render::width(&extra) + 6 <= w {
                    tail.push_str(&extra);
                }
            }
            Line::from(vec![
                cursor_cell(cursor.shown && n == cursor.row, s, unicode),
                Span::styled(star.to_string(), s.accent),
                Span::styled(
                    render::truncate(name, w.saturating_sub(render::width(&tail) + 4), unicode),
                    if r.archived { s.muted } else { s.project },
                ),
                Span::styled(
                    format!(" {tail}"),
                    if r.overdue > 0 { s.overdue } else { s.muted },
                ),
            ])
        })
        .collect();

    // What is off the bottom, and how much of it is worth scrolling to. The
    // panel is sized to the projects with work in them (`model::demand`), so on
    // a real store the rows below the fold are mostly the ones saying `0 open`
    // — and "…14 more" would send a reader after fourteen rows of nothing. It
    // says which kind of nothing instead. They stay reachable with `j`: a row
    // the cursor can land on and the panel will not name is the invisible-field
    // failure, and counting them is not the same as hiding them.
    if hidden > 0 {
        let idle = rows
            .iter()
            .skip(start + room)
            .filter(|r| r.open == 0)
            .count();
        let text = if idle == hidden {
            format!("…{hidden} more, nothing open")
        } else {
            format!("…{hidden} more")
        };
        out.push(Line::from(Span::styled(
            render::truncate(&text, w, unicode),
            s.muted,
        )));
    }
    out
}

/// The BURNDOWN panel: the same step line the standalone `tasqx chart
/// burndown` draws, over whatever rows the panel was given.
///
/// It used to carry its own sparkline — a third implementation of the idea,
/// with its own glyph table and its own ASCII fallback, next to the one in
/// `chart.rs` and the one in `render_burndown`. Three copies of a bad chart is
/// how a panel and the command it mirrors end up disagreeing about the same
/// week. The GEOMETRY is shared now (`chart::plot_step_line`) and the painting
/// is not, which is the right seam: where a line goes is universal, and
/// ratatui spans and ANSI escapes are not.
fn burndown_body(dash: &Dashboard, ctx: &PanelCtx) -> Vec<Line<'static>> {
    let (s, w, height, unicode) = (&ctx.s, ctx.w, ctx.height, ctx.unicode());
    let series = &dash.burndown.series;
    if series.is_empty() {
        return empty("no history in this window", s, w as u16, unicode);
    }
    let max = series.iter().map(|p| p.remaining).max().unwrap_or(1).max(1);
    let last = series.last().map(|p| p.remaining).unwrap_or(0);
    let first = series.first().map(|p| p.remaining).unwrap_or(0);
    let net = i64::from(last) - i64::from(first);

    // One row goes to the footer, and one more to the clipped warning when
    // there is one. A plot floored at two rows still shows a direction; at one
    // it is the sparkline again.
    let reserved = 1 + usize::from(dash.burndown.truncated && height >= 3);
    let plot_rows = (height as usize).saturating_sub(reserved).clamp(2, 12);
    let gutter = format!("{max}").len();
    let plot_cols = (w.saturating_sub(gutter + 2)).max(4);

    let g = chart::LineGlyphs::new(unicode);
    let values: Vec<u32> = series.iter().map(|p| p.remaining).collect();
    let mut grid = chart::plot_step_line(&values, plot_rows, plot_cols, max, &g);
    chart::lay_ideal(&mut grid, first, plot_rows, plot_cols, max, &g);

    // The axis glyphs go through `unicode` like every other one. Hard-coding
    // them here is the exact trap the ASCII test exists to catch: box-drawing
    // bytes on a legacy console are mojibake, and mojibake in a grid misaligns
    // every column to its right.
    let (tick, spine, foot) = if unicode {
        ('┤', '│', '┴')
    } else {
        ('|', '|', '+')
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    for (y, row) in grid.iter().enumerate() {
        let (label, mark) = if y == 0 {
            (format!("{max:>gutter$}"), tick)
        } else if y == plot_rows - 1 {
            (format!("{:>gutter$}", 0), tick)
        } else {
            (" ".repeat(gutter), spine)
        };
        let mut spans = vec![Span::styled(format!("{label}{mark}"), s.muted)];
        for (is_ideal, part) in split_line(row, g.ideal) {
            spans.push(Span::styled(
                part,
                if is_ideal { s.chart_ideal } else { s.accent },
            ));
        }
        out.push(Line::from(spans));
    }
    out.push(Line::from(Span::styled(
        render::truncate(
            &format!(
                "{}{foot} {} days · now {last} · net {net:+}",
                " ".repeat(gutter),
                dash.burndown.days
            ),
            w,
            unicode,
        ),
        s.muted,
    )));
    if dash.burndown.truncated && height >= 3 {
        out.push(Line::from(Span::styled(
            render::truncate("window clipped — history may be incomplete", w, unicode),
            s.warn,
        )));
    }
    out
}

/// Split a plotted row into runs of ideal-line cells and runs of everything
/// else, so the reference and the data can be painted apart. The mirror of
/// `chart::split_ideal`, which does the same for the ANSI path; they are two
/// because a `Span` and an escape sequence are not the same object, not
/// because the rule differs.
fn split_line(row: &[char], ideal: char) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    for &c in row {
        let is_ideal = c == ideal;
        match runs.last_mut() {
            Some((flag, text)) if *flag == is_ideal => text.push(c),
            _ => runs.push((is_ideal, c.to_string())),
        }
    }
    runs
}

fn tokens_body(dash: &Dashboard, ctx: &PanelCtx) -> Vec<Line<'static>> {
    let (s, w, height, unicode) = (&ctx.s, ctx.w, ctx.height, ctx.unicode());
    let rows = &dash.tokens.rows;
    if rows.is_empty() {
        return empty("no token spend attributed yet", s, w as u16, unicode);
    }
    let visible = (height as usize).saturating_sub(1).max(1);
    let mut out: Vec<Line<'static>> = rows
        .iter()
        .take(visible)
        .map(|r| {
            let name = r.name().unwrap_or("(none)");
            let total = tokens::compact(r.total());
            Line::from(Span::styled(split_row(name, &total, w, unicode), s.project))
        })
        .collect();
    // The four buckets, never summed into one number and never priced (D48).
    let legend = tokens::BUCKETS
        .iter()
        .enumerate()
        .map(|(i, (_, short, _))| format!("{short} {}", tokens::compact(dash.tokens.totals[i])))
        .collect::<Vec<_>>()
        .join(" · ");
    out.push(Line::from(Span::styled(
        render::truncate(&legend, w, unicode),
        s.muted,
    )));
    out
}

/// The header line's spans.
///
/// Names no project (#200/D62). Every count here is STORE-WIDE, and the bar
/// used to set the default project's name beside them — `tasqx work · 5 open`
/// on a store with more than one project, while the PROJECTS panel eight
/// lines below, built from the same snapshot, gave `work` a different `open`.
/// Nothing was miscounted; the line simply put a name next to numbers that
/// were not its, with no way for a reader to tell which half to believe. The
/// default project is still marked — the `*` `project.list` already spends on
/// it — in the PROJECTS panel, which is the one place a project name and a
/// project's own counts are the same row. A header scope strip that makes the
/// counts follow a chosen project is D62's fuller fix and is not this one.
pub fn status_line(bar: &StatusBar, width: u16, theme: &Theme, caps: &Caps) -> Vec<Span<'static>> {
    let s = styles(theme, caps);
    let header = rt_style(theme.role("header"), caps);
    let mut spans = vec![Span::styled("tasqx".to_string(), header)];
    let sep = || Span::styled(" · ".to_string(), s.muted);

    // Zero counts stay legible rather than turning invisible: colour is
    // emphasis here, never the only channel.
    spans.push(sep());
    spans.push(Span::styled(format!("{} open", bar.open), s.plain));
    let mut budget = width as usize;
    for sp in &spans {
        budget = budget.saturating_sub(render::width(&sp.content));
    }
    // Narrow terminals drop the tail fields rather than truncating mid-word.
    let extras: [(String, RtStyle); 4] = [
        (
            format!("{} active", bar.active),
            if bar.active > 0 { s.active } else { s.muted },
        ),
        (
            format!("{} overdue", bar.overdue),
            if bar.overdue > 0 { s.overdue } else { s.muted },
        ),
        (
            format!("{} blocked", bar.blocked),
            if bar.blocked > 0 { s.warn } else { s.muted },
        ),
        (format!("{} done/week", bar.done_week), s.muted),
    ];
    for (text, style) in extras {
        let need = render::width(&text) + 3;
        if need > budget {
            break;
        }
        budget -= need;
        spans.push(sep());
        spans.push(Span::styled(text, style));
    }
    spans
}
