//! One body-builder per panel: model in, styled lines out.
//!
//! Nothing here draws chrome, and nothing here knows where on the screen it is
//! — a builder is handed a width, a height and a scroll offset, and returns at
//! most that many lines. The screen composites; these fill.
//!
//! **Every string is cut in Rust before it becomes a `Span`.** ratatui does clip
//! a `Paragraph` to its rect, but a silent clip loses the ellipsis that tells
//! the reader something was cut, and a double-width grapheme straddling the last
//! cell is exactly the artefact `unicode-truncate` exists to prevent. Widths are
//! measured in CELLS via `render::width`, never in chars.

use ratatui::style::Style as RtStyle;
use ratatui::text::{Line, Span};

use crate::render;
use crate::theme::{Caps, Theme};
use crate::tokens;
use crate::tui::rt_style;

use super::model::{Dashboard, Detail, PanelId, StatusBar, Task};

/// `01:23:07` — a running clock.
pub fn hms(secs: i64) -> String {
    let s = secs.max(0);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
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

/// `-2d`, `6h`, `17:00` — how a deadline reads relative to now.
pub fn when_cell(task: &Task, today: jiff::civil::Date) -> String {
    let Some(d) = task.due_date() else {
        return String::new();
    };
    let days = (d - today).get_days();
    match days {
        0 => {
            // Midnight is the store's spelling of "no time given", so a
            // date-only due must not claim to be due at 00:00.
            let t = task.due.map(|t| t.to_zoned(jiff::tz::TimeZone::UTC));
            match t {
                Some(z) if z.hour() != 0 || z.minute() != 0 => {
                    format!("{:02}:{:02}", z.hour(), z.minute())
                }
                _ => "today".to_string(),
            }
        }
        d if d < 0 => format!("{}d", d),
        d => format!("+{d}d"),
    }
}

/// Pad `s` to `cells`, measuring in cells.
fn pad(s: &str, cells: usize) -> String {
    let w = render::width(s);
    if w >= cells {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(cells - w))
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

/// Build one panel's body.
#[allow(clippy::too_many_arguments)]
pub fn body(
    id: PanelId,
    detail: Detail,
    dash: &Dashboard,
    width: u16,
    height: u16,
    scroll: usize,
    theme: &Theme,
    caps: &Caps,
) -> Vec<Line<'static>> {
    let s = styles(theme, caps);
    let w = width as usize;
    let mut lines = match id {
        PanelId::Now => now_body(dash, detail, &s, w, caps.unicode),
        PanelId::Next => next_body(dash, &s, w, height, scroll, caps.unicode, theme, caps),
        PanelId::Due => due_body(dash, &s, w, height, scroll, caps.unicode),
        PanelId::Blocked => blocked_body(dash, &s, w, height, scroll, caps.unicode),
        PanelId::Recent => recent_body(dash, &s, w, height, scroll, caps.unicode),
        PanelId::Projects => projects_body(dash, &s, w, height, scroll, caps.unicode),
        PanelId::Burndown => burndown_body(dash, &s, w, height, caps.unicode),
        PanelId::Tokens => tokens_body(dash, &s, w, height, caps.unicode),
        PanelId::Slot => Vec::new(),
    };
    lines.truncate(height as usize);
    lines
}

fn now_body(
    dash: &Dashboard,
    detail: Detail,
    s: &Styles,
    w: usize,
    unicode: bool,
) -> Vec<Line<'static>> {
    let Some(card) = &dash.now else {
        return empty("no timer running · p to pick one", s, w as u16, unicode);
    };
    let marker = if unicode { "▶" } else { ">" };
    let head = Line::from(vec![
        Span::styled(format!("{marker} "), s.active),
        Span::styled(format!("#{} ", card.task.short_id), s.accent),
        Span::styled(
            render::truncate(card.task.title(), w.saturating_sub(6), unicode),
            s.plain,
        ),
    ]);
    if detail == Detail::OneLine {
        return vec![head];
    }
    let proj = card.task.project().unwrap_or("—").to_string();
    let clock = hms(card.elapsed_secs);
    let second = Line::from(Span::styled(
        split_row(&format!("  {proj}"), &clock, w, unicode),
        s.project,
    ));
    if detail == Detail::Compact {
        return vec![head, second];
    }
    let est = card
        .task
        .estimate_secs
        .map(|e| format!("est {}", dur_compact(e)))
        .unwrap_or_else(|| "no estimate".to_string());
    let third = Line::from(Span::styled(
        render::truncate(
            &format!("  {est} · tracked {}", dur_compact(card.total_secs())),
            w,
            unicode,
        ),
        s.muted,
    ));
    vec![head, second, third]
}

/// The id column is sized from EVERY row the panel can scroll to, not from the
/// screenful being drawn — otherwise the columns re-align on every `j` press.
fn id_width(rows: &[Task]) -> usize {
    rows.iter()
        .map(|t| format!("#{}", t.short_id).len())
        .max()
        .unwrap_or(3)
}

fn more_line(
    shown: usize,
    total: usize,
    s: &Styles,
    w: usize,
    unicode: bool,
) -> Option<Line<'static>> {
    (total > shown).then(|| {
        Line::from(Span::styled(
            render::truncate(&format!("…{} more", total - shown), w, unicode),
            s.muted,
        ))
    })
}

#[allow(clippy::too_many_arguments)]
fn next_body(
    dash: &Dashboard,
    s: &Styles,
    w: usize,
    height: u16,
    scroll: usize,
    unicode: bool,
    theme: &Theme,
    caps: &Caps,
) -> Vec<Line<'static>> {
    let rows = &dash.next.rows;
    if rows.is_empty() {
        return empty(
            "nothing actionable — all done, blocked or waiting",
            s,
            w as u16,
            unicode,
        );
    }
    let idw = id_width(rows);
    let visible = height as usize;
    let start = scroll.min(rows.len().saturating_sub(1));
    let mut out = Vec::new();
    let room = if rows.len() > visible {
        visible - 1
    } else {
        visible
    };
    for t in rows.iter().skip(start).take(room) {
        let urg = format!("{:>4.1}", t.urgency);
        let prio = t.priority.map(|p| p.as_str()).unwrap_or("-");
        let head = format!("{} {urg} {prio} ", pad(&format!("#{}", t.short_id), idw));
        let rest = w.saturating_sub(render::width(&head));
        // The ramp, over the same denominator `render` uses, so the dashboard
        // and `tasqx list` shade the same task the same colour. `ramp_style`
        // and NOT `role("urgency.ramp")`: the ramp is a sibling field of the
        // role map, so that lookup compiles, runs, and silently returns an
        // unstyled Style on every theme — a typo with no symptom.
        let ramp = rt_style(theme.ramp_style(t.urgency / dash.next.max_urgency), caps);
        // Priority has its own role per level, and `Prio::parse` admits nothing
        // but H/M/L, so the formatted name can never miss.
        let prio_style = t
            .priority
            .map(|p| rt_style(theme.role(&format!("priority.{}", p.as_str())), caps))
            .unwrap_or(s.muted);
        out.push(Line::from(vec![
            Span::styled(pad(&format!("#{}", t.short_id), idw), s.accent),
            Span::styled(format!(" {urg} "), ramp),
            Span::styled(format!("{prio} "), prio_style),
            Span::styled(render::truncate(t.title(), rest, unicode), s.plain),
        ]));
    }
    if let Some(l) = more_line(start + room, rows.len(), s, w, unicode) {
        out.push(l);
    }
    out
}

fn due_body(
    dash: &Dashboard,
    s: &Styles,
    w: usize,
    height: u16,
    scroll: usize,
    unicode: bool,
) -> Vec<Line<'static>> {
    let d = &dash.due;
    if d.is_empty() {
        return empty("no deadlines this week", s, w as u16, unicode);
    }
    let today = dash.today;
    let mut out = Vec::new();
    let buckets: [(&str, &Vec<Task>, RtStyle); 4] = [
        ("OVERDUE", &d.overdue, s.overdue),
        ("TODAY", &d.today, s.warn),
        ("TOMORROW", &d.tomorrow, s.muted),
        ("THIS WEEK", &d.week, s.muted),
    ];
    for (name, rows, style) in buckets {
        if rows.is_empty() {
            continue;
        }
        out.push(Line::from(Span::styled(
            render::truncate(&format!("{name}  {}", rows.len()), w, unicode),
            style,
        )));
        for t in rows {
            let when = when_cell(t, today);
            out.push(Line::from(Span::styled(
                split_row(
                    &format!(" #{} {}", t.short_id, t.title()),
                    &when,
                    w,
                    unicode,
                ),
                s.plain,
            )));
        }
    }
    let visible = height as usize;
    if out.len() > visible {
        let start = scroll.min(out.len() - 1);
        out = out.into_iter().skip(start).collect();
    }
    out
}

fn blocked_body(
    dash: &Dashboard,
    s: &Styles,
    w: usize,
    height: u16,
    scroll: usize,
    unicode: bool,
) -> Vec<Line<'static>> {
    let rows = &dash.blocked.rows;
    if rows.is_empty() {
        return empty("nothing is blocked", s, w as u16, unicode);
    }
    let visible = height as usize;
    let room = if rows.len() > visible {
        visible - 1
    } else {
        visible
    };
    let start = scroll.min(rows.len().saturating_sub(1));
    let mut out: Vec<Line<'static>> = rows
        .iter()
        .skip(start)
        .take(room)
        .map(|t| {
            Line::from(vec![
                Span::styled(format!("#{} ", t.short_id), s.danger),
                Span::styled(
                    render::truncate(t.title(), w.saturating_sub(6), unicode),
                    s.plain,
                ),
            ])
        })
        .collect();
    if let Some(l) = more_line(start + room, rows.len(), s, w, unicode) {
        out.push(l);
    }
    out
}

fn recent_body(
    dash: &Dashboard,
    s: &Styles,
    w: usize,
    height: u16,
    scroll: usize,
    unicode: bool,
) -> Vec<Line<'static>> {
    let rows = &dash.recent.rows;
    if rows.is_empty() {
        return empty("nothing has changed yet", s, w as u16, unicode);
    }
    let visible = height as usize;
    let room = if rows.len() > visible {
        visible - 1
    } else {
        visible
    };
    let start = scroll.min(rows.len().saturating_sub(1));
    let mut out: Vec<Line<'static>> = rows
        .iter()
        .skip(start)
        .take(room)
        .map(|t| {
            let status = t.status.as_str().to_string();
            let head = format!("#{} ", t.short_id);
            let rest = w.saturating_sub(render::width(&head) + status.len() + 1);
            Line::from(vec![
                Span::styled(head, s.accent),
                Span::styled(
                    pad(&render::truncate(t.title(), rest, unicode), rest),
                    s.plain,
                ),
                Span::styled(status, if t.status.is_open() { s.plain } else { s.muted }),
            ])
        })
        .collect();
    if let Some(l) = more_line(start + room, rows.len(), s, w, unicode) {
        out.push(l);
    }
    out
}

fn projects_body(
    dash: &Dashboard,
    s: &Styles,
    w: usize,
    height: u16,
    scroll: usize,
    unicode: bool,
) -> Vec<Line<'static>> {
    let rows = &dash.projects.rows;
    if rows.is_empty() {
        return empty("no projects yet — tasqx init <name>", s, w as u16, unicode);
    }
    let visible = height as usize;
    let start = scroll.min(rows.len().saturating_sub(1));
    rows.iter()
        .skip(start)
        .take(visible)
        .map(|r| {
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
                if render::width(&tail) + render::width(&extra) + 4 <= w {
                    tail.push_str(&extra);
                }
            }
            Line::from(vec![
                Span::styled(star.to_string(), s.accent),
                Span::styled(
                    render::truncate(name, w.saturating_sub(tail.len() + 2), unicode),
                    if r.archived { s.muted } else { s.project },
                ),
                Span::styled(
                    format!(" {tail}"),
                    if r.overdue > 0 { s.overdue } else { s.muted },
                ),
            ])
        })
        .collect()
}

fn burndown_body(
    dash: &Dashboard,
    s: &Styles,
    w: usize,
    height: u16,
    unicode: bool,
) -> Vec<Line<'static>> {
    let series = &dash.burndown.series;
    if series.is_empty() {
        return empty("no history in this window", s, w as u16, unicode);
    }
    let max = series.iter().map(|p| p.remaining).max().unwrap_or(1).max(1);
    let bars = if unicode {
        ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
    } else {
        ['.', '.', '-', '-', '=', '=', '#', '#']
    };
    let spark: String = series
        .iter()
        .map(|p| {
            let t = (p.remaining as f64 / max as f64 * 7.0).round() as usize;
            bars[t.min(7)]
        })
        .collect();
    let last = series.last().map(|p| p.remaining).unwrap_or(0);
    let first = series.first().map(|p| p.remaining).unwrap_or(0);
    let net = last as i64 - first as i64;
    // The axis glyphs go through `unicode` like every other one. Hard-coding
    // them here is the exact trap the ASCII test exists to catch: box-drawing
    // bytes on a legacy console are mojibake, and mojibake in a grid misaligns
    // every column to its right.
    let (tick, foot) = if unicode { ('┤', '┴') } else { ('|', '+') };
    let mut out = vec![
        Line::from(Span::styled(
            render::truncate(&format!("{max:>3} {tick} {spark}"), w, unicode),
            s.plain,
        )),
        Line::from(Span::styled(
            render::truncate(
                &format!(
                    "  0 {foot} {} days · now {last} · net {net:+}",
                    dash.burndown.days
                ),
                w,
                unicode,
            ),
            s.muted,
        )),
    ];
    if dash.burndown.truncated && height >= 3 {
        out.push(Line::from(Span::styled(
            render::truncate("window clipped — history may be incomplete", w, unicode),
            s.warn,
        )));
    }
    out
}

fn tokens_body(
    dash: &Dashboard,
    s: &Styles,
    w: usize,
    height: u16,
    unicode: bool,
) -> Vec<Line<'static>> {
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
pub fn status_line(bar: &StatusBar, width: u16, theme: &Theme, caps: &Caps) -> Vec<Span<'static>> {
    let s = styles(theme, caps);
    let header = rt_style(theme.role("header"), caps);
    let mut spans = vec![Span::styled("tasqx ".to_string(), header)];
    let sep = || Span::styled(" · ".to_string(), s.muted);

    match bar.project() {
        Some(p) => spans.push(Span::styled(p.to_string(), s.project)),
        None => spans.push(Span::styled("(no default project)".to_string(), s.muted)),
    }
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
