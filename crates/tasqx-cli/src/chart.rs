//! Native terminal charts (DESIGN.md §8) — pure clients of the core API.
//!
//! The core returns numbers (`event.list`, `task.list`); this module buckets
//! them and draws with Unicode block/braille glyphs, degrading to ASCII bars
//! under dumb/piped/legacy-console (the same `caps.unicode` signal the tables
//! use). Under `NO_COLOR` the glyphs are *kept* — NO_COLOR governs color only,
//! and the stream is still a Unicode-capable TTY — but color is dropped, so the
//! bars read in monochrome. Color comes from the active theme's
//! `urgency.ramp` / `accent` roles.
//!
//! The *data* functions (`throughput`, `heatmap`, `burndown`) are separated from
//! the glyph rendering so they can be unit-tested against a seeded event set.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use serde_json::Value;

use crate::theme::Ctx;

// ============================================================================
// Shared helpers
// ============================================================================

/// The UTC civil date of an RFC3339 timestamp string (events store UTC).
fn ev_date(ts: &str) -> Option<Date> {
    let t: Timestamp = ts.parse().ok()?;
    Some(t.to_zoned(TimeZone::UTC).date())
}

/// Today's UTC date — the anchor for every "last N" window.
pub fn today() -> Date {
    Timestamp::now().to_zoned(TimeZone::UTC).date()
}

/// Pull the events array out of an `event.list` result.
fn events_of(result: &Value) -> Vec<&Value> {
    result
        .get("events")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn op_of(ev: &Value) -> &str {
    ev.get("op").and_then(Value::as_str).unwrap_or("")
}
fn ts_of(ev: &Value) -> Option<&str> {
    ev.get("ts").and_then(Value::as_str)
}
fn entity_id_of(ev: &Value) -> Option<&str> {
    ev.get("entity_id").and_then(Value::as_str)
}

// ============================================================================
// Throughput — added vs done per ISO week
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeekBucket {
    pub iso_year: i16,
    pub iso_week: i8,
    pub added: u32,
    pub done: u32,
}

impl WeekBucket {
    pub fn label(&self) -> String {
        format!("W{:02}", self.iso_week)
    }
    pub fn net(&self) -> i64 {
        self.added as i64 - self.done as i64
    }
}

/// Bucket `add` vs `done` events into the last `weeks` ISO weeks (oldest→newest),
/// including empty weeks so the series is contiguous.
pub fn throughput(result: &Value, weeks: usize, anchor: Date) -> Vec<WeekBucket> {
    let weeks = weeks.max(1);
    // Build the ordered list of (iso_year, iso_week) keys for the window.
    let mut keys: Vec<(i16, i8)> = Vec::with_capacity(weeks);
    let mut d = anchor;
    for _ in 0..weeks {
        let iso = d.iso_week_date();
        keys.push((iso.year(), iso.week()));
        d = d.saturating_sub(7i64.days());
    }
    keys.reverse(); // oldest first
    let mut buckets: Vec<WeekBucket> = keys
        .iter()
        .map(|(y, w)| WeekBucket {
            iso_year: *y,
            iso_week: *w,
            added: 0,
            done: 0,
        })
        .collect();

    for ev in events_of(result) {
        let (Some(ts), op) = (ts_of(ev), op_of(ev)) else {
            continue;
        };
        if op != "add" && op != "done" {
            continue;
        }
        let Some(date) = ev_date(ts) else { continue };
        let iso = date.iso_week_date();
        if let Some(b) = buckets
            .iter_mut()
            .find(|b| b.iso_year == iso.year() && b.iso_week == iso.week())
        {
            if op == "add" {
                b.added += 1;
            } else {
                b.done += 1;
            }
        }
    }
    buckets
}

/// 4-week average done/week (velocity).
fn velocity(buckets: &[WeekBucket]) -> f64 {
    let tail: Vec<&WeekBucket> = buckets.iter().rev().take(4).collect();
    if tail.is_empty() {
        return 0.0;
    }
    let sum: u32 = tail.iter().map(|b| b.done).sum();
    sum as f64 / tail.len() as f64
}

/// Render a series the caller has already computed.
///
/// The series is a PARAMETER, not something this function derives for itself,
/// so `--json` and the sparkline are two views of one computation rather than
/// two computations that happen to agree today. (Same rule as `report`'s two
/// modes sharing one request object.)
pub fn render_throughput(ctx: &Ctx, buckets: &[WeekBucket]) -> String {
    let max = buckets
        .iter()
        .map(|b| b.added.max(b.done))
        .max()
        .unwrap_or(0)
        .max(1);
    let width = 10usize;

    let legend = if ctx.caps.unicode {
        "added ▁▂▃  done ▁▂▃"
    } else {
        "added [#]  done [#]"
    };
    let mut out = String::new();
    out.push_str(&ctx.paint("header", "Weekly throughput"));
    out.push_str(&format!("   {}\n", ctx.paint("muted", legend)));

    for b in buckets {
        let added_bar = bar(b.added, max, width, ctx);
        let done_bar = bar(b.done, max, width, ctx);
        let added_s = ctx.paint("accent", &added_bar);
        let done_s = ctx.paint("timer.active", &done_bar);
        let net = b.net();
        let net_s = if net > 0 {
            format!("+{net}")
        } else {
            net.to_string()
        };
        let note = if net < 0 { "  burning down" } else { "" };
        out.push_str(&format!(
            "  {}  added {added_s} {:>2}   done {done_s} {:>2}   net {:>3}{}\n",
            ctx.paint("muted", &b.label()),
            b.added,
            b.done,
            net_s,
            ctx.paint("muted", note),
        ));
    }

    let vel = velocity(buckets);
    let recent_net: i64 = buckets.iter().rev().take(4).map(|b| b.net()).sum();
    let trend = if recent_net < 0 {
        "WIP trending down"
    } else if recent_net > 0 {
        "WIP trending up"
    } else {
        "WIP steady"
    };
    out.push_str(&format!(
        "  {}\n",
        ctx.paint(
            "muted",
            &format!(
                "{a} 4-wk velocity {vel:.1} done/wk {m} {trend}",
                a = ctx.arrow(),
                m = ctx.mid()
            )
        )
    ));
    out
}

/// A block-glyph bar of `n/max` over `width` cells, ASCII `#` when no Unicode.
fn bar(n: u32, max: u32, width: usize, ctx: &Ctx) -> String {
    if max == 0 {
        return String::new();
    }
    let filled = ((n as f64 / max as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    if ctx.caps.unicode {
        "█".repeat(filled)
    } else {
        "#".repeat(filled)
    }
}

// ============================================================================
// Heatmap — completions per day (GitHub-style density)
// ============================================================================

/// One cell of the completion heatmap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DayCount {
    pub date: Date,
    pub count: u32,
}

/// Completions per day across the last `weeks` weeks, ending on `anchor`.
/// Returns a contiguous day series (oldest→newest), aligned so the last column
/// is the week containing `anchor`.
pub fn heatmap(result: &Value, weeks: usize, anchor: Date) -> Vec<DayCount> {
    let weeks = weeks.max(1);
    // Tally done events by day.
    use std::collections::HashMap;
    let mut tally: HashMap<Date, u32> = HashMap::new();
    for ev in events_of(result) {
        if op_of(ev) != "done" {
            continue;
        }
        if let Some(d) = ts_of(ev).and_then(ev_date) {
            *tally.entry(d).or_insert(0) += 1;
        }
    }

    // Window: from the Monday `weeks-1` weeks before the anchor's Monday,
    // through the anchor's Sunday — a whole-week grid.
    let days_from_mon = anchor.weekday().to_monday_zero_offset() as i64;
    let this_monday = anchor.saturating_sub(days_from_mon.days());
    let start = this_monday.saturating_sub((((weeks - 1) * 7) as i64).days());
    let total_days = weeks * 7;

    let mut out = Vec::with_capacity(total_days);
    let mut d = start;
    for _ in 0..total_days {
        out.push(DayCount {
            date: d,
            count: *tally.get(&d).unwrap_or(&0),
        });
        d = d.saturating_add(1i64.days());
    }
    out
}

/// Longest run of consecutive days (up to & including `anchor`) with ≥1 done.
pub fn current_streak(days: &[DayCount], anchor: Date) -> u32 {
    let mut streak = 0u32;
    let mut d = anchor;
    let map: std::collections::HashMap<Date, u32> =
        days.iter().map(|dc| (dc.date, dc.count)).collect();
    loop {
        match map.get(&d) {
            Some(c) if *c > 0 => {
                streak += 1;
                d = d.saturating_sub(1i64.days());
            }
            _ => break,
        }
    }
    streak
}

/// Longest run of consecutive done-days anywhere in the series.
pub fn best_streak(days: &[DayCount]) -> u32 {
    let mut best = 0u32;
    let mut cur = 0u32;
    for dc in days {
        if dc.count > 0 {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

/// Render a day series the caller has already computed. See `render_throughput`.
pub fn render_heatmap(ctx: &Ctx, days: &[DayCount], anchor: Date) -> String {
    let weeks_n = days.len() / 7;

    let legend = if ctx.caps.unicode {
        "░ 0  ▒ 1–2  ▓ 3–4  █ 5+"
    } else {
        ". 0  : 1-2  + 3-4  # 5+"
    };
    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!("Completions {} last {weeks_n} weeks", ctx.mid()),
    ));
    out.push_str(&format!("   {}\n", ctx.paint("muted", legend)));

    // 7 weekday rows (Mon..Sun) × weeks columns.
    const ROW: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut total = 0u32;
    for dc in days {
        total += dc.count;
    }
    for (wd, label) in ROW.iter().enumerate() {
        // Show alternate weekday labels only (Mon/Wed/Fri/Sun) to stay compact.
        let shown = if wd % 2 == 0 { *label } else { "   " };
        let mut line = format!("  {} ", ctx.paint("muted", shown));
        for w in 0..weeks_n {
            let idx = w * 7 + wd;
            if let Some(dc) = days.get(idx) {
                line.push_str(&cell(dc.count, ctx));
                line.push(' ');
            }
        }
        out.push_str(&line);
        out.push('\n');
    }

    let cur = current_streak(days, anchor);
    let best = best_streak(days);
    out.push_str(&format!(
        "  {}\n",
        ctx.paint(
            "muted",
            &format!(
                "{a} {total} done {m} current streak {cur} days {m} best {best}",
                a = ctx.arrow(),
                m = ctx.mid()
            )
        )
    ));
    out
}

/// A density cell colored by the urgency ramp bucket for its count.
fn cell(count: u32, ctx: &Ctx) -> String {
    let (glyph, t) = match count {
        0 => (if ctx.caps.unicode { '░' } else { '.' }, 0.0),
        1..=2 => (if ctx.caps.unicode { '▒' } else { ':' }, 0.4),
        3..=4 => (if ctx.caps.unicode { '▓' } else { '+' }, 0.7),
        _ => (if ctx.caps.unicode { '█' } else { '#' }, 1.0),
    };
    let g = glyph.to_string();
    if count == 0 {
        ctx.paint("muted", &g)
    } else {
        ctx.theme.ramp_style(t).paint(&g, &ctx.caps)
    }
}

// ============================================================================
// Burndown — remaining open tasks over the last N days
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemainingPoint {
    pub date: Date,
    pub remaining: u32,
}

/// A task's open/close lifecycle reconstructed from its events.
#[derive(Clone, Copy, Debug)]
struct Life {
    open: Option<Date>,  // date of `add` (or None => opened before recorded time)
    close: Option<Date>, // date of first `done`/`cancel` after open
}

/// Historically-correct remaining-open series over the last `days_n` days.
///
/// `member_ids` scopes to a project (the caller resolves membership via
/// `task.list project:P`); pass all task ids for a global burndown. A task
/// counts open on day D when its `add` is on/before D and it has no
/// `done`/`cancel` on/before D. Reopen events are ignored (first close wins) —
/// a small, flagged simplification.
pub fn burndown(
    result: &Value,
    member_ids: &std::collections::HashSet<String>,
    days_n: usize,
    anchor: Date,
) -> Vec<RemainingPoint> {
    use std::collections::HashMap;
    let days_n = days_n.max(1);

    // Reduce events → per-task lifecycle. Events arrive newest-first; we keep
    // the earliest add and the earliest close.
    let mut lives: HashMap<String, Life> = HashMap::new();
    for ev in events_of(result) {
        let Some(id) = entity_id_of(ev) else { continue };
        if !member_ids.contains(id) {
            continue;
        }
        let Some(date) = ts_of(ev).and_then(ev_date) else {
            continue;
        };
        let entry = lives.entry(id.to_string()).or_insert(Life {
            open: None,
            close: None,
        });
        match op_of(ev) {
            "add" | "import" => {
                entry.open = Some(match entry.open {
                    Some(cur) if cur < date => cur,
                    _ => date,
                });
            }
            "done" | "cancel" => {
                entry.close = Some(match entry.close {
                    Some(cur) if cur < date => cur,
                    _ => date,
                });
            }
            _ => {}
        }
    }

    // Ensure every member with no add event still counts (opened pre-window).
    for id in member_ids {
        lives.entry(id.clone()).or_insert(Life {
            open: None,
            close: None,
        });
    }

    let start = anchor.saturating_sub(((days_n - 1) as i64).days());
    let mut out = Vec::with_capacity(days_n);
    let mut d = start;
    for _ in 0..days_n {
        let mut remaining = 0u32;
        for life in lives.values() {
            let opened = match life.open {
                Some(o) => o <= d,
                None => true, // no recorded add => treat as opened before window
            };
            let closed = matches!(life.close, Some(c) if c <= d);
            if opened && !closed {
                remaining += 1;
            }
        }
        out.push(RemainingPoint { date: d, remaining });
        d = d.saturating_add(1i64.days());
    }
    out
}

/// Render the burndown series as a labeled sparkline column chart. The data is
/// the historically-correct remaining-open count per day; the drawing is a
/// compact column chart (not the §8 dual ideal-vs-actual line — see report).
/// Render a remaining-open series the caller has already computed.
/// See `render_throughput`.
pub fn render_burndown(ctx: &Ctx, series: &[RemainingPoint], scope_label: &str) -> String {
    let max = series.iter().map(|p| p.remaining).max().unwrap_or(0).max(1);

    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!("Remaining open {} {scope_label}", ctx.mid()),
    ));
    out.push('\n');

    // Sparkline row of vertical blocks, colored hot→cold by fill.
    let spark: String = series
        .iter()
        .map(|p| {
            let t = p.remaining as f64 / max as f64;
            let g = spark_glyph(t, ctx.caps.unicode);
            ctx.theme
                .ramp_style(1.0 - t)
                .paint(&g.to_string(), &ctx.caps)
        })
        .collect();
    out.push_str(&format!(
        "  {}  {}\n",
        ctx.paint("muted", &format!("{max:>3}")),
        spark
    ));
    out.push_str(&format!(
        "  {}  {}\n",
        ctx.paint("muted", "  0"),
        ctx.paint("muted", &axis_labels(series, ctx.caps.unicode)),
    ));

    let first = series.first().map(|p| p.remaining).unwrap_or(0);
    let last = series.last().map(|p| p.remaining).unwrap_or(0);
    let delta = last as i64 - first as i64;
    let trend = if delta < 0 {
        format!("down {} over {} days", -delta, series.len())
    } else if delta > 0 {
        format!("up {} over {} days", delta, series.len())
    } else {
        format!("flat over {} days", series.len())
    };
    // Simple projection: at the recent net burn rate, days to zero.
    let proj = project_finish(series, ctx.mid());
    out.push_str(&format!(
        "  {}\n",
        ctx.paint(
            "muted",
            &format!(
                "{a} {last} left {m} {trend}{proj}",
                a = ctx.arrow(),
                m = ctx.mid()
            )
        )
    ));
    out
}

fn spark_glyph(t: f64, unicode: bool) -> char {
    if unicode {
        const G: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let i = ((t.clamp(0.0, 1.0)) * (G.len() - 1) as f64).round() as usize;
        G[i]
    } else if t <= 0.01 {
        '_'
    } else if t < 0.5 {
        '.'
    } else {
        '#'
    }
}

fn axis_labels(series: &[RemainingPoint], unicode: bool) -> String {
    // First and last date, spaced to the series width.
    if series.is_empty() {
        return String::new();
    }
    let first = series.first().unwrap().date;
    let last = series.last().unwrap().date;
    let fs = format!(
        "{:04}-{:02}-{:02}",
        first.year(),
        first.month(),
        first.day()
    );
    let ls = format!("{:04}-{:02}-{:02}", last.year(), last.month(), last.day());
    let arrow = if unicode { "→" } else { "->" };
    let width = series.len();
    if width <= fs.len() + ls.len() + 1 {
        format!("{fs} {arrow} {ls}")
    } else {
        let pad = width - fs.len() - ls.len();
        format!("{fs}{}{ls}", " ".repeat(pad))
    }
}

fn project_finish(series: &[RemainingPoint], mid: &str) -> String {
    if series.len() < 2 {
        return String::new();
    }
    let last = series.last().unwrap().remaining as i64;
    if last == 0 {
        return format!(" {mid} cleared");
    }
    // Recent burn rate over the last min(7, n) days.
    let n = series.len();
    let look = n.clamp(2, 7);
    let a = series[n - look].remaining as i64;
    let b = last;
    let per_day = (a - b) as f64 / (look - 1) as f64;
    if per_day <= 0.0 {
        return format!(" {mid} not burning down");
    }
    let days = (last as f64 / per_day).ceil() as i64;
    format!(" {mid} ~{days}d to clear at current rate")
}

/// Round a duration to whole days for the default windows.
pub fn default_weeks(is_year: bool, weeks: Option<usize>) -> usize {
    if let Some(w) = weeks {
        return w;
    }
    if is_year {
        52
    } else {
        12
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(op: &str, ts: &str, id: &str) -> Value {
        json!({ "op": op, "ts": ts, "entity": "task", "entity_id": id })
    }

    fn result(evs: Vec<Value>) -> Value {
        json!({ "count": evs.len(), "events": evs })
    }

    // Anchor all tests on a fixed Monday so ISO weeks are deterministic.
    // 2026-07-13 is a Monday (ISO week 29 of 2026).
    fn anchor() -> Date {
        Date::constant(2026, 7, 13)
    }

    #[test]
    fn throughput_buckets_add_and_done_by_iso_week() {
        // Week 29 (Mon 2026-07-13): 2 adds, 1 done.
        // Week 28 (2026-07-06..12): 1 add, 2 done.
        let evs = vec![
            ev("add", "2026-07-13T09:00:00Z", "a"),
            ev("add", "2026-07-14T09:00:00Z", "b"),
            ev("done", "2026-07-15T09:00:00Z", "a"),
            ev("add", "2026-07-07T09:00:00Z", "c"),
            ev("done", "2026-07-08T09:00:00Z", "c"),
            ev("done", "2026-07-09T09:00:00Z", "d"),
            // an ignored op
            ev("modify", "2026-07-13T10:00:00Z", "a"),
        ];
        let buckets = throughput(&result(evs), 3, anchor());
        assert_eq!(buckets.len(), 3);
        // newest last
        let w29 = buckets.last().unwrap();
        assert_eq!(w29.iso_week, 29);
        assert_eq!(w29.added, 2);
        assert_eq!(w29.done, 1);
        assert_eq!(w29.net(), 1);
        let w28 = &buckets[buckets.len() - 2];
        assert_eq!(w28.iso_week, 28);
        assert_eq!(w28.added, 1);
        assert_eq!(w28.done, 2);
        assert_eq!(w28.net(), -1);
        // oldest (week 27) empty
        assert_eq!(buckets[0].added, 0);
        assert_eq!(buckets[0].done, 0);
    }

    #[test]
    fn heatmap_counts_done_per_day() {
        let evs = vec![
            ev("done", "2026-07-13T09:00:00Z", "a"),
            ev("done", "2026-07-13T18:00:00Z", "b"), // same day => 2
            ev("done", "2026-07-10T09:00:00Z", "c"),
            ev("add", "2026-07-13T09:00:00Z", "z"), // not a completion
        ];
        let days = heatmap(&result(evs), 2, anchor());
        assert_eq!(days.len(), 14);
        let by: std::collections::HashMap<Date, u32> =
            days.iter().map(|d| (d.date, d.count)).collect();
        assert_eq!(by.get(&Date::constant(2026, 7, 13)).copied(), Some(2));
        assert_eq!(by.get(&Date::constant(2026, 7, 10)).copied(), Some(1));
        assert_eq!(by.get(&Date::constant(2026, 7, 12)).copied(), Some(0));
    }

    #[test]
    fn streaks_computed() {
        // Completions on 11,12,13 (a 3-day run ending at anchor).
        let evs = vec![
            ev("done", "2026-07-11T09:00:00Z", "a"),
            ev("done", "2026-07-12T09:00:00Z", "b"),
            ev("done", "2026-07-13T09:00:00Z", "c"),
            ev("done", "2026-07-08T09:00:00Z", "d"), // isolated
        ];
        let days = heatmap(&result(evs), 2, anchor());
        assert_eq!(current_streak(&days, anchor()), 3);
        assert_eq!(best_streak(&days), 3);
    }

    #[test]
    fn burndown_reconstructs_remaining() {
        // a: added 07-10, done 07-12  → open on 10,11 ; closed from 12
        // b: added 07-11, never closed → open from 11 onward
        let evs = vec![
            ev("add", "2026-07-10T09:00:00Z", "a"),
            ev("done", "2026-07-12T09:00:00Z", "a"),
            ev("add", "2026-07-11T09:00:00Z", "b"),
        ];
        let members: std::collections::HashSet<String> =
            ["a".to_string(), "b".to_string()].into_iter().collect();
        let series = burndown(&result(evs), &members, 5, anchor()); // 07-09..07-13
        let by: std::collections::HashMap<Date, u32> =
            series.iter().map(|p| (p.date, p.remaining)).collect();
        assert_eq!(by.get(&Date::constant(2026, 7, 9)).copied(), Some(0)); // nothing yet
        assert_eq!(by.get(&Date::constant(2026, 7, 10)).copied(), Some(1)); // a
        assert_eq!(by.get(&Date::constant(2026, 7, 11)).copied(), Some(2)); // a,b
        assert_eq!(by.get(&Date::constant(2026, 7, 12)).copied(), Some(1)); // a closed, b open
        assert_eq!(by.get(&Date::constant(2026, 7, 13)).copied(), Some(1)); // b open
    }

    #[test]
    fn glyphs_kept_under_no_color_but_ascii_when_plain() {
        use crate::theme::{self, Caps, ColorDepth};
        // NO_COLOR: still a Unicode TTY, color off — block glyphs survive, but no
        // color escape is emitted (documented contract; NO_COLOR governs color).
        let no_color = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: ColorDepth::None,
                ansi: true,
                unicode: true,
            },
        );
        assert_eq!(bar(3, 6, 6, &no_color), "███", "glyphs kept under NO_COLOR");
        let c = cell(6, &no_color);
        assert!(c.contains('█'), "heatmap glyph kept: {c:?}");
        assert!(
            !c.contains("38;2") && !c.contains("38;5"),
            "color dropped: {c:?}"
        );

        // Piped/dumb/legacy (unicode off): ASCII bars, zero escapes.
        let plain = Ctx::new(theme::default_theme(), Caps::PLAIN);
        assert_eq!(
            bar(3, 6, 6, &plain),
            "###",
            "ASCII bars when Unicode unavailable"
        );
        assert_eq!(cell(6, &plain), "#");
        assert!(!bar(3, 6, 6, &plain).contains('\x1b'));
        assert_eq!(spark_glyph(1.0, true), '█');
        assert_eq!(spark_glyph(1.0, false), '#');
        assert_eq!(spark_glyph(0.0, false), '_');
    }

    #[test]
    fn burndown_scopes_to_members() {
        let evs = vec![
            ev("add", "2026-07-11T09:00:00Z", "a"),
            ev("add", "2026-07-11T09:00:00Z", "x"), // not a member
        ];
        let members: std::collections::HashSet<String> = ["a".to_string()].into_iter().collect();
        let series = burndown(&result(evs), &members, 3, anchor());
        assert_eq!(series.last().unwrap().remaining, 1);
    }
}
