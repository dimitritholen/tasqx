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
        // The counts sit LEFT of their bars and the bars are padded out to a
        // fixed cell budget, so every number in the column starts at the same
        // place. Drawn the other way round — a ragged-length bar and then its
        // number — each row's figures landed wherever that row's bar happened to
        // end, and the chart read as four columns that could not agree on where
        // they were. A bar is a magnitude; a magnitude belongs on a grid.
        let added_s = bar_cell(b.added, max, width, ctx, "accent");
        let done_s = bar_cell(b.done, max, width, ctx, "timer.active");
        let net = b.net();
        let net_s = if net > 0 {
            format!("+{net}")
        } else {
            net.to_string()
        };
        let note = if net < 0 { "  burning down" } else { "" };
        out.push_str(&format!(
            "  {}  added {:>3} {added_s}   done {:>3} {done_s}   net {:>4}{}\n",
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

/// A painted bar padded out to its full `width` in cells — the fixed-size box
/// the row after it is aligned against.
///
/// The padding is added AFTER painting so the trailing spaces carry no SGR
/// state (a themed background would otherwise draw an empty bar as a filled
/// one, which is the opposite of what it means).
fn bar_cell(n: u32, max: u32, width: usize, ctx: &Ctx, role: &str) -> String {
    let b = bar(n, max, width, ctx);
    let filled = b.chars().count();
    format!(
        "{}{}",
        ctx.paint(role, &b),
        " ".repeat(width.saturating_sub(filled))
    )
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

/// Whether a lifecycle event leaves the task open or closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    Open,
    Closed,
}

/// A task's status timeline: one entry per day it changed, in date order.
///
/// This replaces a `{ open, close }` pair, which could not represent a cycle at
/// all — there was one slot for a close, so the second one had nowhere to go and
/// a reopen had nothing to undo.
#[derive(Clone, Debug, Default)]
struct Life {
    /// `(date, state after the last event of that date)`, ascending, deduped.
    days: Vec<(Date, Lifecycle)>,
    /// Whether the earliest event seen for this task is its birth (`add` /
    /// `import`). False when the window clipped the birth off — see
    /// [`Life::open_on`].
    born_in_window: bool,
}

impl Life {
    /// Whether the task counts open at the end of day `d`.
    fn open_on(&self, d: Date) -> bool {
        // The last change on or before D decides D.
        if let Some((_, state)) = self.days.iter().rev().find(|(date, _)| *date <= d) {
            return *state == Lifecycle::Open;
        }
        // Nothing on or before D. Three cases, and only the middle one is new.
        if self.days.is_empty() {
            // A member with no lifecycle events at all: it exists (the caller
            // put it in `member_ids`) and nothing ever closed it.
            return true;
        }
        if self.born_in_window {
            // Its first event is its `add`, and that is after D — genuinely not
            // yet created.
            return false;
        }
        // Its first event is a close or a reopen, so the `add` happened before
        // the events we were given. That is what `event.list {from}` does to
        // every long-lived task (D59), and reading it as "not yet born" would
        // draw a task materialising from nothing already completed. It existed,
        // and it was open.
        true
    }
}

/// Historically-correct remaining-open series over the last `days_n` days.
///
/// `member_ids` scopes to a project (the caller resolves membership via
/// `task.list project:P`); pass all task ids for a global burndown.
///
/// The reconstruction replays each task's lifecycle — `add`/`import` open it,
/// `done`/`cancel` close it, `reopen` opens it again — and every other op is
/// status-neutral and ignored. **The last event of a calendar day decides that
/// day**; the series has one point per day, so a task closed and reopened
/// between breakfast and dinner has to resolve to one of them, and the day's
/// final state is the one a reader means by "how many were left".
///
/// Replay is ordered by parsed `Timestamp`. It cannot be ordered by the `ts`
/// string (jiff's fractional second is variable-length, so text order is not
/// time order — see `storage::event_id_floor`) and cannot be ordered by event
/// id, which the callers do not carry into this function.
///
/// D59 withdrew the previous simplification, which took the earliest close and
/// ignored `reopen` entirely: a task closed, reopened and still open read as
/// permanently done, on the panel a user checks to find out whether the pile is
/// emptying.
pub fn burndown(
    result: &Value,
    member_ids: &std::collections::HashSet<String>,
    days_n: usize,
    anchor: Date,
) -> Vec<RemainingPoint> {
    use std::collections::HashMap;
    let days_n = days_n.max(1);

    // Collect each member's lifecycle events, with the instant they happened.
    let mut raw: HashMap<&str, Vec<(Timestamp, Lifecycle, bool)>> = HashMap::new();
    for ev in events_of(result) {
        let Some(id) = entity_id_of(ev) else { continue };
        if !member_ids.contains(id) {
            continue;
        }
        let Some(at) = ts_of(ev).and_then(|s| s.parse::<Timestamp>().ok()) else {
            continue;
        };
        let (state, is_birth) = match op_of(ev) {
            "add" | "import" => (Lifecycle::Open, true),
            "done" | "cancel" => (Lifecycle::Closed, false),
            "reopen" => (Lifecycle::Open, false),
            _ => continue,
        };
        raw.entry(id).or_default().push((at, state, is_birth));
    }

    let mut lives: HashMap<&str, Life> = HashMap::new();
    for (id, mut evs) in raw {
        // Events arrive newest-first from `event.list`, and the fixtures are in
        // arbitrary order, so ordering is established here rather than assumed.
        evs.sort_by_key(|(at, _, _)| *at);
        let born_in_window = evs.first().is_some_and(|(_, _, birth)| *birth);
        let mut days: Vec<(Date, Lifecycle)> = Vec::new();
        for (at, state, _) in evs {
            let date = at.to_zoned(TimeZone::UTC).date();
            match days.last_mut() {
                // Same day: the later event overwrites — last of the day wins.
                Some((d, s)) if *d == date => *s = state,
                _ => days.push((date, state)),
            }
        }
        lives.insert(
            id,
            Life {
                days,
                born_in_window,
            },
        );
    }

    // A member with no lifecycle events at all still counts as open.
    for id in member_ids {
        lives.entry(id.as_str()).or_default();
    }

    let start = anchor.saturating_sub(((days_n - 1) as i64).days());
    let mut out = Vec::with_capacity(days_n);
    let mut d = start;
    for _ in 0..days_n {
        let remaining = lives.values().filter(|life| life.open_on(d)).count() as u32;
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

/// The chart window length in WEEKS: an explicit `--weeks` wins, otherwise 52
/// for the year-shaped heatmap and 12 for everything else.
///
/// One function rather than a default per call site, because `chart` and
/// `heatmap` echo this number straight back as `"weeks"` in their JSON: a
/// second copy of the default is how the window that was drawn and the window
/// that was reported drift apart with nothing to catch it. It does not clamp —
/// see `MAX_CHART_WEEKS` in `command.rs` for why an out-of-range `--weeks` is
/// refused at parse time instead of quietly rewritten here.
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

    /// Collect a burndown into a date→remaining map, so a case can assert the
    /// days it cares about by name.
    fn series_by_date(
        evs: Vec<Value>,
        members: &[&str],
        days: usize,
    ) -> std::collections::HashMap<Date, u32> {
        let set: std::collections::HashSet<String> =
            members.iter().map(|s| s.to_string()).collect();
        burndown(&result(evs), &set, days, anchor())
            .iter()
            .map(|p| (p.date, p.remaining))
            .collect()
    }

    /// D59's headline: a reopened task is open again.
    ///
    /// The old reducer kept the EARLIEST close and had no arm for `reopen` at
    /// all, so a task closed on the 11th and reopened on the 12th read as done
    /// forever — on a screen a user checks precisely to find out whether the
    /// pile is emptying.
    #[test]
    fn burndown_counts_a_reopened_task_as_open_again() {
        let by = series_by_date(
            vec![
                ev("add", "2026-07-10T09:00:00Z", "a"),
                ev("done", "2026-07-11T09:00:00Z", "a"),
                ev("reopen", "2026-07-12T09:00:00Z", "a"),
            ],
            &["a"],
            5, // 07-09..07-13
        );
        assert_eq!(by.get(&Date::constant(2026, 7, 9)).copied(), Some(0));
        assert_eq!(by.get(&Date::constant(2026, 7, 10)).copied(), Some(1));
        assert_eq!(by.get(&Date::constant(2026, 7, 11)).copied(), Some(0));
        assert_eq!(
            by.get(&Date::constant(2026, 7, 12)).copied(),
            Some(1),
            "the reopen must put the task back in the count"
        );
        assert_eq!(by.get(&Date::constant(2026, 7, 13)).copied(), Some(1));
    }

    /// Several cycles, because "first close wins" is not merely lossy once — it
    /// collapses the whole tail of the series.
    #[test]
    fn burndown_survives_several_lifecycle_cycles() {
        let by = series_by_date(
            vec![
                ev("add", "2026-07-09T09:00:00Z", "a"),
                ev("done", "2026-07-10T09:00:00Z", "a"),
                ev("reopen", "2026-07-11T09:00:00Z", "a"),
                ev("cancel", "2026-07-12T09:00:00Z", "a"),
                ev("reopen", "2026-07-13T09:00:00Z", "a"),
            ],
            &["a"],
            6, // 07-08..07-13
        );
        let want = [(8, 0), (9, 1), (10, 0), (11, 1), (12, 0), (13, 1)];
        for (day, remaining) in want {
            assert_eq!(
                by.get(&Date::constant(2026, 7, day)).copied(),
                Some(remaining),
                "07-{day:02} must be {remaining}"
            );
        }
    }

    /// The intra-day rule, stated rather than left to iteration order.
    ///
    /// The old reducer bucketed to a `Date` and then took a min over a HashMap,
    /// so a task closed and reopened on the same day resolved arbitrarily. The
    /// last event of the calendar day decides that day.
    #[test]
    fn burndown_resolves_intra_day_events_by_the_last_event_of_the_day() {
        let by = series_by_date(
            vec![
                ev("add", "2026-07-10T09:00:00Z", "a"),
                ev("done", "2026-07-11T09:00:00Z", "a"),
                ev("reopen", "2026-07-11T17:00:00Z", "a"),
            ],
            &["a"],
            4, // 07-10..07-13
        );
        assert_eq!(
            by.get(&Date::constant(2026, 7, 11)).copied(),
            Some(1),
            "the day's LAST event decides the day, so 07-11 ends open"
        );
        assert_eq!(by.get(&Date::constant(2026, 7, 12)).copied(), Some(1));
    }

    /// The clause that makes bounding the read by `from` safe (D59).
    ///
    /// Once `event.list {from}` clips the window, a long-lived task's `add`
    /// falls outside it and only the `done` survives. Without this rule the
    /// series shows a task materialising from nothing already completed —
    /// every day before its close reads "not yet born". It existed, and it was
    /// open.
    #[test]
    fn burndown_counts_a_task_whose_add_fell_outside_the_window() {
        let by = series_by_date(
            vec![ev("done", "2026-07-12T09:00:00Z", "a")],
            &["a"],
            5, // 07-09..07-13
        );
        for day in [9, 10, 11] {
            assert_eq!(
                by.get(&Date::constant(2026, 7, day)).copied(),
                Some(1),
                "07-{day:02}: a task with no `add` in the window existed and was open"
            );
        }
        assert_eq!(by.get(&Date::constant(2026, 7, 12)).copied(), Some(0));
        assert_eq!(by.get(&Date::constant(2026, 7, 13)).copied(), Some(0));
    }

    /// `import` opens a task exactly as `add` does, or a restored store reads
    /// as empty.
    #[test]
    fn burndown_counts_import_as_an_opening_event() {
        let by = series_by_date(
            vec![ev("import", "2026-07-11T09:00:00Z", "a")],
            &["a"],
            5, // 07-09..07-13
        );
        assert_eq!(by.get(&Date::constant(2026, 7, 10)).copied(), Some(0));
        assert_eq!(by.get(&Date::constant(2026, 7, 11)).copied(), Some(1));
        assert_eq!(by.get(&Date::constant(2026, 7, 13)).copied(), Some(1));
    }

    /// Replay order must come from the parsed instant, not from the `ts` string.
    ///
    /// jiff prints a variable-length fractional second and omits it entirely
    /// when zero, so `'…09:00:00.5Z'` sorts BELOW `'…09:00:00Z'` as text —
    /// `'.'` is 0x2E, `'Z'` is 0x5A. Two events half a second apart therefore
    /// replay backwards under a string sort, and on a day with both a close and
    /// an open that inverts the day's final state.
    ///
    /// Every other fixture in this file writes whole seconds, where text order
    /// and time order agree — which is exactly why this case is needed: a string
    /// sort passes all of them.
    #[test]
    fn burndown_orders_replay_by_instant_not_by_the_ts_string() {
        let by = series_by_date(
            vec![
                ev("add", "2026-07-09T09:00:00Z", "a"),
                // Same day, half a second apart, in the order they happened.
                ev("reopen", "2026-07-11T09:00:00Z", "a"),
                ev("done", "2026-07-11T09:00:00.5Z", "a"),
            ],
            &["a"],
            5, // 07-09..07-13
        );
        assert_eq!(
            by.get(&Date::constant(2026, 7, 11)).copied(),
            Some(0),
            "the `done` is half a second AFTER the `reopen`, so the day ends closed — \
             a `ts`-string sort replays them backwards and leaves it open"
        );
        assert_eq!(by.get(&Date::constant(2026, 7, 12)).copied(), Some(0));
    }

    /// Status-neutral ops must not move the series. Most events in a real log
    /// are these, so a reducer that mistook one for a lifecycle change would be
    /// wrong almost everywhere.
    #[test]
    fn burndown_ignores_status_neutral_ops() {
        let lifecycle = vec![
            ev("add", "2026-07-10T09:00:00Z", "a"),
            ev("done", "2026-07-12T09:00:00Z", "a"),
        ];
        let mut noisy = lifecycle.clone();
        for (op, ts) in [
            ("start", "2026-07-10T10:00:00Z"),
            ("stop", "2026-07-10T11:00:00Z"),
            ("modify", "2026-07-11T09:00:00Z"),
            ("annotation.add", "2026-07-11T10:00:00Z"),
            ("token.add", "2026-07-11T11:00:00Z"),
            ("reminded", "2026-07-13T09:00:00Z"),
        ] {
            noisy.push(ev(op, ts, "a"));
        }
        assert_eq!(
            series_by_date(lifecycle, &["a"], 5),
            series_by_date(noisy, &["a"], 5),
            "a status-neutral op must leave the series identical"
        );
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

    /// Every figure in a chart row must sit in the same column as the one above
    /// it. The bars used to be drawn BEFORE their numbers and were only as long
    /// as their own magnitude, so each row's `done`, its counts and its `net`
    /// landed wherever that row's bar happened to end — a chart whose four
    /// columns disagreed about where they were, on the same screen as the table
    /// this alignment work started from.
    #[test]
    fn throughput_rows_line_their_columns_up_whatever_the_bars_do() {
        use crate::theme::{self, Caps};
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        // Magnitudes chosen to give every row a different bar length, including
        // the empty bar and the full one.
        let buckets: Vec<WeekBucket> = [(0u32, 0u32), (6, 0), (23, 7), (16, 0), (3, 7)]
            .iter()
            .enumerate()
            .map(|(i, (added, done))| WeekBucket {
                iso_year: 2026,
                iso_week: 28 + i as i8,
                added: *added,
                done: *done,
            })
            .collect();
        let out = render_throughput(&ctx, &buckets);
        let rows: Vec<&str> = out.lines().skip(1).take(buckets.len()).collect();
        assert_eq!(rows.len(), buckets.len(), "one row per week: {out}");
        for label in ["added", "done", "net"] {
            let want = rows[0].find(label);
            assert!(want.is_some(), "no {label:?} in {:?}", rows[0]);
            for row in &rows {
                assert_eq!(row.find(label), want, "the {label:?} column moved:\n{out}");
            }
        }
        // …and the counts themselves, which sit at a fixed offset from `added`.
        let cut = |row: &str| row.split("done").next().unwrap().len();
        for row in &rows {
            assert_eq!(
                cut(row),
                cut(rows[0]),
                "the added block changed width:\n{out}"
            );
        }
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
