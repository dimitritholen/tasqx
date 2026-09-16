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
    crate::clock::now().to_zoned(TimeZone::UTC).date()
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
    /// The Monday this ISO week starts on — an axis label a reader can place
    /// on a calendar, where `label()`'s `W37` is the bucket key.
    pub fn start(&self) -> Option<Date> {
        jiff::civil::ISOWeekDate::new(self.iso_year, self.iso_week, jiff::civil::Weekday::Monday)
            .ok()
            .map(|w| w.date())
    }
    pub fn net(&self) -> i64 {
        self.added as i64 - self.done as i64
    }
}

/// Bucket `add` vs `done` events into the last `weeks` ISO weeks (oldest→newest),
/// including empty weeks so the series is contiguous.
///
/// `members` (#164, #162) is what makes this agree with `report`, on both the
/// membership axis and the double-counting axis: a raw event tally
/// double-counts `done` for a task closed, reopened and closed again (one task,
/// two events), counts `add` for a task that was later cancelled (which
/// `report` excludes by default, D24), and — with no membership scoping at all
/// — draws for the whole store while every other stat on a filtered `report
/// --html <filter>` page answered the filter (#162), because `report --html`
/// hands this function the store-wide `event.list` result (D59 bounds it by
/// time, not by scope). Neither bucket can be answered from the event log
/// alone — it has no idea what a task's status is NOW, or whether it is even
/// in scope.
///
///  * only events for an id present in `members` are counted at all.
///  * `added` skips a task whose CURRENT status is cancelled, matching D24.
///  * `done` counts each CURRENTLY-done task once, at its LATEST `done` event
///    — the event log's other `done` rows for that id (superseded by a
///    `reopen`) are not a second completion.
///
/// Every caller passes the membership it actually has — the terminal `chart
/// throughput` command's own unfiltered run computes the full-store list via
/// `burndown_members(engine, &None)`, same as a filtered report computes its
/// scoped one — so this takes `&[Member]`, not an `Option`, and is never asked
/// to count anonymously.
pub fn throughput(
    result: &Value,
    members: &[Member],
    weeks: usize,
    anchor: Date,
) -> Vec<WeekBucket> {
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

    use std::collections::HashMap;
    // Doubles as the membership scope (#162): an id absent from `members` has
    // no entry here at all, so `status_of.get(id)` returning `None` excludes
    // it from both `added` and `done` just as surely as a separate id-set
    // would — one lookup answers "in scope?" and "what status?" together.
    let status_of: HashMap<&str, &str> = members
        .iter()
        .map(|m| (m.id.as_str(), m.status.as_str()))
        .collect();

    let mut bump_added = |date: Date| {
        let iso = date.iso_week_date();
        if let Some(b) = buckets
            .iter_mut()
            .find(|b| b.iso_year == iso.year() && b.iso_week == iso.week())
        {
            b.added += 1;
        }
    };

    // Each currently-done member's LATEST `done` event date — a task closed,
    // reopened and closed again must count once, on the completion that
    // actually stands today, not once per historical event.
    let mut latest_done: HashMap<&str, Date> = HashMap::new();

    for ev in events_of(result) {
        let (Some(ts), op) = (ts_of(ev), op_of(ev)) else {
            continue;
        };
        if op != "add" && op != "done" {
            continue;
        }
        let Some(id) = entity_id_of(ev) else { continue };
        let Some(&status) = status_of.get(id) else {
            continue; // not in scope
        };
        let Some(date) = ev_date(ts) else { continue };
        match op {
            "add" => {
                if status != "cancelled" {
                    bump_added(date);
                }
            }
            "done" => {
                if status == "done" {
                    latest_done
                        .entry(id)
                        .and_modify(|d| {
                            if date > *d {
                                *d = date;
                            }
                        })
                        .or_insert(date);
                }
            }
            _ => unreachable!(),
        }
    }
    for date in latest_done.into_values() {
        let iso = date.iso_week_date();
        if let Some(b) = buckets
            .iter_mut()
            .find(|b| b.iso_year == iso.year() && b.iso_week == iso.week())
        {
            b.done += 1;
        }
    }
    buckets
}

/// 4-week average done/week (velocity) over the last four ISO weeks that have
/// FULLY elapsed as of `anchor` — independent of whatever window `--weeks`
/// asked to display (#234 item 4).
///
/// Averaging over whatever `buckets` happen to be on screen made `--weeks 1`
/// report a single partial week's rate under the label "4-wk velocity", which
/// doubled the true figure the moment the window narrowed to include today's
/// (necessarily incomplete) ISO week. Anchoring on the Sunday before the
/// current week's Monday always lands on four COMPLETE weeks, however wide the
/// caller's own display window is.
pub fn velocity_4wk(result: &Value, members: &[Member], anchor: Date) -> f64 {
    let days_from_mon = anchor.weekday().to_monday_zero_offset() as i64;
    let this_monday = anchor.saturating_sub(days_from_mon.days());
    let last_complete_sunday = this_monday.saturating_sub(1i64.days());
    let buckets = throughput(result, members, 4, last_complete_sunday);
    if buckets.is_empty() {
        return 0.0;
    }
    let sum: u32 = buckets.iter().map(|b| b.done).sum();
    sum as f64 / buckets.len() as f64
}

/// Render a series the caller has already computed.
///
/// The series is a PARAMETER, not something this function derives for itself,
/// so `--json` and the sparkline are two views of one computation rather than
/// two computations that happen to agree today. (Same rule as `report`'s two
/// modes sharing one request object.)
///
/// `velocity` is likewise a parameter rather than derived from `buckets` here
/// (#234 item 4): the "4-wk" figure must come from the last four COMPLETE ISO
/// weeks regardless of how many weeks `--weeks` put on screen, and deriving it
/// from whatever `buckets` happened to hold made a 1-week window report a
/// single partial week's rate under a "4-wk" label. `buckets.last()` is always
/// the ISO week containing the anchor (`today` — see `throughput`'s window
/// construction), so it is marked "(partial)" unconditionally: "today" has, by
/// definition, not finished its week.
///
/// `store_empty` is a PARAMETER for the same reason: whether the store has
/// ever held a task is a fact about the whole store, and a real user whose
/// current window simply has no activity yet (a quiet week on an otherwise
/// long-lived store) produces the identical all-zero `buckets` a genuinely
/// fresh store does — only the caller, which already lists every task to
/// build `buckets`, can tell the two apart (#233.2).
pub fn render_throughput(
    ctx: &Ctx,
    buckets: &[WeekBucket],
    velocity: f64,
    store_empty: bool,
) -> String {
    // An entirely empty store is a fact, not a grid of zeros to draw —
    // twelve identical "added 0 done 0 net 0" rows plus "WIP steady" reads
    // as a broken tool, and `report` already has the right one-line
    // treatment for this.
    if store_empty {
        return "No events recorded yet — add a task to start the history.\n".to_string();
    }
    let max = buckets
        .iter()
        .map(|b| b.added.max(b.done))
        .max()
        .unwrap_or(0)
        .max(1);
    let width = 10usize;

    // The facts open the chart, where `list` and `agenda` put theirs (D117
    // rule 8), so the rows underneath are only rows.
    let recent_net: i64 = buckets.iter().rev().take(4).map(|b| b.net()).sum();
    let trend = if recent_net < 0 {
        "WIP trending down"
    } else if recent_net > 0 {
        "WIP trending up"
    } else {
        "WIP steady"
    };
    let mut out = format!(
        "{}   {}\n\n",
        ctx.paint("header", "weekly throughput"),
        ctx.paint(
            "muted",
            &format!(
                "4-wk velocity {velocity:.1} done/wk {m} {trend}",
                m = ctx.mid()
            )
        )
    );

    // A header row, once, instead of the words `added`, `done` and `net`
    // printed on every line — thirty-six of them on a twelve-week chart, each
    // saying what the column above it already said. D117 rule 11, and rule 12
    // for the role it is painted in: a column label is not a title.
    out.push_str(&ctx.paint(
        "table.label",
        &format!(
            "  {:<4}  {:>3} {:<width$}  {:>4} {:<width$}   {:>4}",
            "WEEK", "ADD", "", "DONE", "", "NET"
        ),
    ));
    out.push('\n');

    let last_idx = buckets.len().saturating_sub(1);
    for (i, b) in buckets.iter().enumerate() {
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
        // The sign already says which way the week went, so "burning down"
        // beside a negative number is the same fact twice. `partial` stays:
        // nothing else on the row says the week is still running.
        let note = if i == last_idx { "  partial" } else { "" };
        // Padded first, painted second. `format!("{:<4}", painted)` counts the
        // escape bytes as width, so the cell comes out unpadded and every
        // column to its right drifts by however long the SGR happened to be —
        // the failure `render::cell` exists to prevent, rebuilt here.
        out.push_str(&format!(
            "  {}  {:>3} {added_s}  {:>4} {done_s}   {}{}\n",
            ctx.paint("muted", &format!("{:<4}", b.label())),
            b.added,
            b.done,
            ctx.paint("muted", &format!("{net_s:>4}")),
            ctx.paint("muted", note),
        ));
    }
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
    // A week with one task in it rounds to nothing against a peak of
    // forty-four, and a bar of nothing reads exactly like a bar for zero. Any
    // non-zero count draws at least one cell: the number beside it carries the
    // magnitude, and what the bar has to carry is "this week was not empty".
    let filled = filled.max(usize::from(n > 0)).min(width);
    if ctx.caps.unicode {
        // `▄`, not `█`. A full block fills its cell top to bottom, so bars on
        // consecutive rows touch and a column of them reads as one L-shaped
        // mass rather than as a bar per week. The half block leaves a gap above
        // each bar, which is the only thing separating one row from the next in
        // a chart with no rules on it.
        "▄".repeat(filled)
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
///
/// `members` (#164) makes this agree with `report`/`throughput`: a raw `done`
/// EVENT tally double-counts a task closed, reopened and closed again (one
/// task, two events, one of them superseded) — so only members whose CURRENT
/// status is "done" are tallied, at their LATEST `done` event's date.
pub fn heatmap(result: &Value, members: &[Member], weeks: usize, anchor: Date) -> Vec<DayCount> {
    let weeks = weeks.max(1);
    use std::collections::HashMap;
    let done_now: std::collections::HashSet<&str> = members
        .iter()
        .filter(|m| m.status == "done")
        .map(|m| m.id.as_str())
        .collect();
    // Each currently-done member's LATEST `done` event date.
    let mut latest_done: HashMap<&str, Date> = HashMap::new();
    for ev in events_of(result) {
        if op_of(ev) != "done" {
            continue;
        }
        let Some(id) = entity_id_of(ev) else { continue };
        if !done_now.contains(id) {
            continue;
        }
        if let Some(d) = ts_of(ev).and_then(ev_date) {
            latest_done
                .entry(id)
                .and_modify(|cur| {
                    if d > *cur {
                        *cur = d;
                    }
                })
                .or_insert(d);
        }
    }
    let mut tally: HashMap<Date, u32> = HashMap::new();
    for d in latest_done.into_values() {
        *tally.entry(d).or_insert(0) += 1;
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
///
/// `store_empty` is a PARAMETER for the same reason `velocity` is on
/// `render_throughput`: whether the store has ever held a task is a fact
/// about the whole store, not something a zero-filled `days` window can
/// stand in for. A real user whose current window simply has no completions
/// yet — the first week of an otherwise long-lived store — produces the
/// identical all-zero `days` a genuinely fresh store does; only the caller,
/// which already lists every task to build `days` in the first place, can
/// tell the two apart (#233.2, and the collision it had with #234 item 8's
/// deliberately all-zero, all-future single-week fixture).
/// `Sep`, `Jan` — the month as a chart axis abbreviates it. Only ever fed
/// `Date::month`, whose range is 1..=12.
fn month_abbrev(m: i8) -> &'static str {
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    NAMES.get((m - 1).max(0) as usize).copied().unwrap_or("???")
}

pub fn render_heatmap(ctx: &Ctx, days: &[DayCount], anchor: Date, store_empty: bool) -> String {
    if store_empty {
        return "No events recorded yet — add a task to start the history.\n".to_string();
    }
    let weeks_n = days.len() / 7;

    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!("Completions {} last {weeks_n} weeks", ctx.mid()),
    ));
    out.push_str(&format!("   {}\n", heatmap_legend(ctx)));

    // A month strip over the columns. Twelve weeks of grid with no date on it
    // anywhere is a shape a reader cannot place: "when was that gap" has no
    // answer. A label is written once, at the first column whose week opens a
    // new month, and only where it fits without colliding with the last one.
    const ROW: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut months = String::new();
    let mut last_month: Option<i8> = None;
    let mut written = 0usize; // columns of `months` already committed
    for w in 0..weeks_n {
        let Some(dc) = days.get(w * 7) else { continue };
        let m = dc.date.month();
        let at = w * 2; // each column is a glyph plus its trailing space
        if last_month != Some(m) && at >= written {
            let name = month_abbrev(m);
            months.push_str(&" ".repeat(at - written));
            months.push_str(name);
            written = at + name.len();
        }
        last_month = Some(m);
    }
    out.push_str(&format!("      {}\n", ctx.paint("muted", &months)));
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
                // #234 item 8: the grid is padded to whole weeks, so the
                // current (incomplete) week always carries a few days that
                // have not happened yet. Those must not draw as "zero
                // completions" — a day that has not occurred is not an idle
                // one — so they get their own blank glyph instead of `cell`'s
                // 0-count glyph.
                // Two glyphs per day, with no gap between days.
                //
                // A terminal cell is about twice as tall as it is wide, so one
                // glyph per day drew a grid of tall thin bars — the shape read
                // as twelve vertical stripes rather than as a calendar of days.
                // Two glyphs is a square, which is what a day should look like
                // beside the day next to it, and it costs the same two columns
                // the glyph-plus-separator already spent.
                if dc.date > anchor {
                    line.push_str(&ctx.paint("muted", "  "));
                } else {
                    line.push_str(&cell(dc.count, ctx));
                }
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
                "{a} {total} done {m} current streak {cur} {day} {m} best {best}",
                a = ctx.arrow(),
                m = ctx.mid(),
                day = day_word(cur),
            )
        )
    ));
    out
}

/// The heatmap's legend: one real grid swatch per level, not a description of
/// one.
///
/// It used to be a single string painted as ONE span in ONE role (`muted`),
/// so every bucket — the empty day and the busiest one alike — came out the
/// same dim slate colour and the same weight; only the glyph inside told them
/// apart, and in the `mono` theme even the swatch for "5+" was not bold like
/// the grid's own `5+` cells are. A legend is a key to the grid, so each
/// swatch is built by calling `cell` with a representative count for that
/// bucket — the exact function, and therefore the exact painted bytes, the
/// grid itself uses for a day at that level. Only the number labels beside
/// the swatches stay `muted`; they are captions, not data.
fn heatmap_legend(ctx: &Ctx) -> String {
    let dash = if ctx.caps.unicode { "–" } else { "-" };
    let entries: [(u32, String); 4] = [
        (0, "0".to_string()),
        (1, format!("1{dash}2")),
        (3, format!("3{dash}4")),
        (5, "5+".to_string()),
    ];
    let mut out = String::new();
    for (i, (level, label)) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        out.push_str(&cell(*level, ctx));
        out.push(' ');
        out.push_str(&ctx.paint("muted", label));
    }
    out
}

/// A density cell colored by the urgency ramp bucket for its count.
/// One day of the completion grid.
///
/// `░▒▓█` IS the scale — those four glyphs differ by ink density and nothing
/// else, which is what a magnitude wants. So the colour does not encode the
/// count a second time.
///
/// It used to: every non-zero cell went through `ramp_style`, which runs cold
/// to HOT, so five completions in a day were painted the same red this UI uses
/// for overdue and for danger. The best thing that can appear on this chart was
/// drawn in the colour reserved for the worst thing on every other one. Fixing
/// the direction alone would have left two channels carrying one number; taking
/// the colour off the variable entirely fixes both at once.
fn cell(count: u32, ctx: &Ctx) -> String {
    let glyph = match count {
        0 => {
            if ctx.caps.unicode {
                '░'
            } else {
                '.'
            }
        }
        1..=2 => {
            if ctx.caps.unicode {
                '▒'
            } else {
                ':'
            }
        }
        3..=4 => {
            if ctx.caps.unicode {
                '▓'
            } else {
                '+'
            }
        }
        _ => {
            if ctx.caps.unicode {
                '█'
            } else {
                '#'
            }
        }
    };
    let g = glyph.to_string().repeat(2);
    if count == 0 {
        ctx.paint("muted", &g)
    } else {
        ctx.paint("accent", &g)
    }
}

/// "day" for 1, "days" otherwise (#234 item 9) — shared by the heatmap streak
/// line and the burndown trend line, the two summary sentences people paste
/// into a standup note and the one place an ungrammatical "1 days" is public.
fn day_word(n: impl Into<i64>) -> &'static str {
    if n.into() == 1 {
        "day"
    } else {
        "days"
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

/// One task the burndown counts, as the caller already knows it.
///
/// The three facts the reconstruction needs, and all three come from the same
/// `task.list` snapshot the rest of the screen is built from — which is the
/// point: the burndown and the header can no longer disagree about a task,
/// because they are reading one answer.
#[derive(Clone, Debug)]
pub struct Member {
    pub id: String,
    /// When the task was created. Existence comes from here, not from an `add`
    /// event, so a window that clipped the birth off costs nothing.
    pub created: Date,
    /// Whether it is open NOW — `Status::is_open()`, the same predicate the
    /// status bar counts.
    pub open_now: bool,
    /// The current status text (`"done"`, `"cancelled"`, `"pending"`, …).
    ///
    /// `open_now` alone cannot tell `heatmap`/`throughput` what they need
    /// (#164): both "done" and "cancelled" are `!open_now`, but only "done"
    /// belongs on a completions chart, and only "cancelled" is the D24
    /// exclusion `report` already applies to `added`. Compared by literal
    /// string, the same choice `burndown`'s `modify` arm already makes for
    /// the same reason — tasqx has no closed enum for a status a *different*
    /// build of core might have written.
    pub status: String,
}

/// Project `task.list` rows into burndown members.
///
/// One reader, because three surfaces need it — the dashboard, `tasqx chart
/// burndown` and the HTML report — and three copies of "which tasks, and were
/// they open" is three chances to answer one question differently. A row
/// missing `id` or `created` is skipped rather than guessed at.
pub fn members_of(tasks: &Value) -> Vec<Member> {
    tasks
        .get("tasks")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|t| {
                    let status = t.get("status").and_then(Value::as_str).unwrap_or("");
                    Some(Member {
                        id: t.get("id").and_then(Value::as_str)?.to_string(),
                        created: t
                            .get("created")
                            .and_then(Value::as_str)?
                            .parse::<Timestamp>()
                            .ok()?
                            .to_zoned(TimeZone::UTC)
                            .date(),
                        open_now: crate::render::status_is_open(status),
                        status: status.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Historically-correct remaining-open series over the last `days_n` days.
///
/// **Reconstructed BACKWARDS from today's status, not forwards from a default.**
/// That inversion is the whole design, and it is what makes the function total:
/// forwards, a task with no closing event in the window had to be *guessed* at,
/// and the guess was "open". Three reachable paths made that guess wrong —
///
/// * `store.import` writes one `import` event per task, for done tasks too
///   (`engine/transfer.rs`), and the export carries no history, so an imported
///   done task's only lifecycle row is a birth that nothing ever closes;
/// * a task added *and* finished before the window has no events in it at all,
///   and `member_ids` came from an unbounded `task.list` while the events came
///   from a bounded `event.list {from}` — "no events in the window" is not "no
///   events";
/// * `task.modify {status: "cancelled"}` — the JSON API's and MCP's only
///   cancellation path — writes `op: "modify"`, not `"cancel"`.
///
/// each of which drew a closed task as open on every day of the chart, and put
/// the last point above the number the status bar printed beside it.
///
/// Backwards there is nothing to guess. The state on day D is: it did not exist
/// if `created > D`; otherwise the first close-or-reopen strictly after D says
/// what it was just before that event (a task that closed was open; a task that
/// reopened was closed); and if nothing has happened since D, it is whatever it
/// is now. The inversion is total because the engine's preconditions are:
/// `done` only from pending/active, `cancel` only from backlog/pending/active,
/// `reopen` only from done/cancelled.
///
/// Births are no longer lifecycle events. `created` answers existence, so
/// `add`/`import` carry no information the snapshot does not already have —
/// which is exactly why the import case stops mattering.
///
/// Replay is ordered by parsed `Timestamp`. It cannot be ordered by the `ts`
/// string (jiff's fractional second is variable-length, so text order is not
/// time order — see `storage::event_id_floor`) and cannot be ordered by event
/// id, which the callers do not carry into this function.
///
/// **The intra-day rule is unchanged from D59**: the state at the end of day D
/// is the state before the first event of the next day that has one. What is
/// kept per member is the raw ascending event list rather than a per-day
/// collapse, because the inverse of "the last event of this day left it open"
/// is not well defined, while the inverse of one event always is.
pub fn burndown(
    result: &Value,
    members: &[Member],
    days_n: usize,
    anchor: Date,
) -> Vec<RemainingPoint> {
    use std::collections::HashMap;
    let days_n = days_n.max(1);

    let ids: std::collections::HashSet<&str> = members.iter().map(|m| m.id.as_str()).collect();

    // Each member's close/reopen events, ascending. Births are deliberately
    // absent — `Member::created` carries existence.
    let mut moves: HashMap<&str, Vec<(Timestamp, Lifecycle)>> = HashMap::new();
    for ev in events_of(result) {
        let Some(id) = entity_id_of(ev) else { continue };
        let Some(&id) = ids.get(id) else { continue };
        let Some(at) = ts_of(ev).and_then(|s| s.parse::<Timestamp>().ok()) else {
            continue;
        };
        let state = match op_of(ev) {
            "done" | "cancel" => Lifecycle::Closed,
            "reopen" => Lifecycle::Open,
            // `task.modify {status: "cancelled"}` is a cancellation wearing a
            // `modify`, and it is the only cancel the JSON API and MCP can
            // write. Read the payload rather than the op name, or every
            // agent-cancelled task hangs open on the chart forever.
            "modify" => match ev
                .get("payload")
                .and_then(|p| p.get("status"))
                .and_then(Value::as_str)
            {
                Some("done") | Some("cancelled") => Lifecycle::Closed,
                Some("pending") | Some("active") | Some("backlog") => Lifecycle::Open,
                _ => continue,
            },
            _ => continue,
        };
        moves.entry(id).or_default().push((at, state));
    }
    for v in moves.values_mut() {
        v.sort_by_key(|(at, _)| *at);
    }

    let start = anchor.saturating_sub(((days_n - 1) as i64).days());
    let mut out = Vec::with_capacity(days_n);
    let mut d = start;
    for _ in 0..days_n {
        let remaining = members
            .iter()
            .filter(|m| open_on(m, d, moves.get(m.id.as_str()).map(Vec::as_slice)))
            .count() as u32;
        out.push(RemainingPoint { date: d, remaining });
        d = d.saturating_add(1i64.days());
    }
    out
}

/// Whether `m` was open at the end of day `d`.
fn open_on(m: &Member, d: Date, moves: Option<&[(Timestamp, Lifecycle)]>) -> bool {
    if m.created > d {
        return false;
    }
    // The first change strictly AFTER day d tells us what it was just before:
    // something that closed had been open, something that reopened had been
    // closed.
    let next = moves.and_then(|v| {
        v.iter()
            .find(|(at, _)| at.to_zoned(TimeZone::UTC).date() > d)
            .map(|(_, state)| *state)
    });
    match next {
        Some(Lifecycle::Closed) => true,
        Some(Lifecycle::Open) => false,
        None => m.open_now,
    }
}

// ============================================================================
// The step-line plotter
// ============================================================================

/// The glyphs a step line is drawn from, in whichever alphabet the terminal
/// can hold.
///
/// A corner carries which two edges of its cell the line leaves by, and that
/// is information ASCII has no way to encode — `+` is every corner at once.
/// Which is fine: the shape survives, and a reader who can only see `+` still
/// sees where the line turned.
pub(crate) struct LineGlyphs {
    horizontal: char,
    vertical: char,
    /// right + down, and left + down
    down_right: char,
    down_left: char,
    /// right + up, and left + up
    up_right: char,
    up_left: char,
    /// The ideal line, which is a reference rather than data. `pub(crate)`
    /// because the ratatui path has to tell the two apart to paint them apart,
    /// and comparing against this field beats a second literal that can drift.
    pub(crate) ideal: char,
}

impl LineGlyphs {
    pub(crate) fn new(unicode: bool) -> Self {
        if unicode {
            Self {
                horizontal: '─',
                vertical: '│',
                down_right: '╭',
                down_left: '╮',
                up_right: '╰',
                up_left: '╯',
                ideal: '·',
            }
        } else {
            Self {
                horizontal: '-',
                vertical: '|',
                down_right: '+',
                down_left: '+',
                up_right: '+',
                up_left: '+',
                ideal: '.',
            }
        }
    }
}

/// Plot `values` as a step line on a `rows` x `cols` grid of characters, top
/// row first. A cell no line passes through is a space.
///
/// A step rather than a slope, because the data is a step: `remaining` is a
/// count that holds until something changes it, and drawing a diagonal between
/// two days invents a series of intermediate values nobody recorded. The riser
/// sits in the column of the NEW value, so a drop is drawn at the moment it
/// happened rather than a column early.
///
/// `top` is the value the first row represents; 0 is the last. Passing it in
/// rather than taking the maximum means a caller can hold the scale steady
/// across a redraw — a chart whose axis moves under a changing series is one
/// that shows change where there is none.
///
/// This replaces a one-row sparkline. Eight glyph levels over one row gave a
/// thirteen-task swing about three of them to move through, and the row spent
/// its colour channel re-encoding the same number the height already carried —
/// two channels for one variable, and the louder of them, colour, ran hot for
/// "nearly finished" and cold for "barely started", which is backwards.
pub(crate) fn plot_step_line(
    values: &[u32],
    rows: usize,
    cols: usize,
    top: u32,
    g: &LineGlyphs,
) -> Vec<Vec<char>> {
    let mut grid = vec![vec![' '; cols]; rows];
    if rows == 0 || cols == 0 || values.is_empty() {
        return grid;
    }
    let top = top.max(1);
    let last_row = rows.saturating_sub(1);

    // Value -> row, with 0 on the bottom row and `top` on the first.
    let row_of = |v: u32| -> usize {
        let t = f64::from(v.min(top)) / f64::from(top);
        let r = ((1.0 - t) * last_row as f64).round() as usize;
        r.min(last_row)
    };

    let ys: Vec<usize> = (0..cols)
        .map(|x| {
            // Columns are sampled from the series rather than the series
            // stretched over the columns: a 30-day window in 90 cells should
            // hold each day three times, not smear thirty values across ninety
            // interpolated ones.
            let n = values.len();
            let i = if cols == 1 || n == 1 {
                n - 1
            } else if n <= cols {
                // Fewer values than columns: each holds for its share, which is
                // what makes a step a step.
                (x * (n - 1)) / (cols - 1)
            } else {
                // More values than columns: each column takes its bucket's LAST
                // value, `downsample`'s rule and for its reason — a burndown
                // reads "as of this column", so the newest state in a bucket is
                // the one that should draw, not an average that blurs a sharp
                // drop into the plateau before it.
                (((x + 1) * n) / cols).clamp(x + 1, n) - 1
            };
            row_of(values[i])
        })
        .collect();

    for (x, &y) in ys.iter().enumerate() {
        grid[y][x] = g.horizontal;
    }

    for x in 0..cols.saturating_sub(1) {
        let (a, b) = (ys[x], ys[x + 1]);
        if a == b {
            continue;
        }
        // The riser lives in column x+1, between the level it left and the one
        // it arrived at. Corners name the two edges the line uses, so the one
        // at the old level opens LEFT (where it came from) and the one at the
        // new level opens RIGHT (where it goes on).
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        for row in grid.iter_mut().take(hi).skip(lo + 1) {
            row[x + 1] = g.vertical;
        }
        if b < a {
            // Rising on screen: up and to the right.
            grid[a][x + 1] = g.up_left;
            grid[b][x + 1] = g.down_right;
        } else {
            grid[a][x + 1] = g.down_left;
            grid[b][x + 1] = g.up_right;
        }
    }
    grid
}

/// Lay the ideal burn — a straight run from the opening value to zero at the
/// close — into the cells the real line does not already occupy.
///
/// Only into the free cells: where the two coincide, the one that says what
/// HAPPENED wins. A reference line that overwrites the data it is a reference
/// for has the priority backwards.
pub(crate) fn lay_ideal(
    grid: &mut [Vec<char>],
    start: u32,
    rows: usize,
    cols: usize,
    top: u32,
    g: &LineGlyphs,
) {
    if rows == 0 || cols < 2 || start == 0 {
        return;
    }
    let top = top.max(1);
    let last_row = rows.saturating_sub(1);
    let rows_for: Vec<usize> = (0..cols)
        .map(|x| {
            let frac = x as f64 / (cols - 1) as f64;
            let v = f64::from(start) * (1.0 - frac);
            let t = (v / f64::from(top)).clamp(0.0, 1.0);
            (((1.0 - t) * last_row as f64).round() as usize).min(last_row)
        })
        .collect();
    for (x, &y) in rows_for.iter().enumerate() {
        if grid[y][x] == ' ' {
            grid[y][x] = g.ideal;
        }
    }
}

/// How many rows a standalone burndown plots over.
///
/// Eight is what it takes for a step to read as a step: a thirteen-task swing
/// on a twenty-five-task chart moves four rows, which is a shape, where on the
/// single row this replaced it moved three of eight glyph levels, which is a
/// texture. The dashboard's panel passes its own smaller number.
pub const BURNDOWN_ROWS: usize = 8;

/// Draw the remaining-open series as a step line with the ideal beside it.
///
/// The chart answers one question — is this burning down — and the shape of
/// the line is the answer. What it replaces could not give one: a one-row
/// sparkline, coloured by the same value its height already carried, with the
/// colour running hot at "almost done" and cold at "barely started". Every
/// reading of it had to be done from the sentence underneath.
pub fn render_burndown(
    ctx: &Ctx,
    series: &[RemainingPoint],
    scope_label: &str,
    had_any_task: bool,
) -> String {
    render_burndown_sized(ctx, series, scope_label, had_any_task, BURNDOWN_ROWS)
}

/// [`render_burndown`] over a caller's own row budget, for a panel that has
/// fewer than a screen.
///
/// The series is the historically-correct remaining-open count per day the
/// CALLER computed — see `render_throughput` for why that is a parameter.
///
/// `had_any_task` (#234 item 6) is whether the scope this burndown covers has
/// EVER held a task — a fact the series alone cannot carry, because "the whole
/// series is zero" means two different things: a real backlog that got cleared
/// (worth an axis and a "cleared" verdict) and a store that never had a task to
/// begin with (worth neither — there is no axis to draw and nothing was
/// cleared).
///
/// This used to document itself as "compact columns, not §8's dual
/// ideal-vs-actual line — that one belongs to the HTML report". That split is
/// retired: §8 sketched the line for the terminal all along, and D80 ruled it
/// outright ("connected rounded step-lines over a dotted ideal"). The cumulative
/// scope-added series D80 names beside it is NOT drawn here — `burndown` does
/// not compute it — and stays open under that ruling.
pub fn render_burndown_sized(
    ctx: &Ctx,
    series: &[RemainingPoint],
    scope_label: &str,
    had_any_task: bool,
    rows: usize,
) -> String {
    let mut out = String::new();

    if !had_any_task {
        out.push_str(&ctx.paint("header", "remaining open"));
        out.push('\n');
        out.push_str(&format!("  {}\n", ctx.paint("muted", "no open tasks yet")));
        return out;
    }

    let first = series.first().map(|p| p.remaining).unwrap_or(0);
    let last = series.last().map(|p| p.remaining).unwrap_or(0);
    let max = series.iter().map(|p| p.remaining).max().unwrap_or(0).max(1);
    let delta = i64::from(last) - i64::from(first);
    let day = day_word(series.len() as i64);

    // The facts go on the line the chart opens with, where `list` and `agenda`
    // put theirs (D117 rule 8): the shape says whether it is burning down, and
    // this line says by how much and from what.
    let trend = if delta < 0 {
        format!("down {} over {} {day}", -delta, series.len())
    } else if delta > 0 {
        format!("up {delta} over {} {day}", series.len())
    } else {
        format!("flat over {} {day}", series.len())
    };
    let facts = format!(
        "{last} left {m} {trend}{proj}",
        m = ctx.mid(),
        proj = project_finish(series.len(), i64::from(last), delta, ctx.mid())
    );
    out.push_str(&format!(
        "{}   {}\n\n",
        ctx.paint(
            "header",
            &format!("remaining open {} {scope_label}", ctx.mid())
        ),
        ctx.paint("muted", &facts)
    ));

    // The gutter holds the two labels the axis needs and nothing else. A row
    // per value would be a table with a picture in it.
    let gutter = format!("{max}").len().max(1);
    let plot_cols = ctx
        .cols
        .saturating_sub(gutter + 4)
        .clamp(10, series.len().max(10) * 3);

    let g = LineGlyphs::new(ctx.caps.unicode);
    let values: Vec<u32> = series.iter().map(|p| p.remaining).collect();
    let mut grid = plot_step_line(&values, rows, plot_cols, max, &g);
    lay_ideal(&mut grid, first, rows, plot_cols, max, &g);

    let (tick, spine, corner, rule) = if ctx.caps.unicode {
        ('┤', '│', '└', '─')
    } else {
        ('+', '|', '+', '-')
    };

    // A tick means "this row is labelled". Drawing one on every row put five
    // unlabelled `┤` down the axis, which reads as a scale whose numbers went
    // missing rather than as a scale with two.
    let last_row = rows.saturating_sub(1);
    for (y, row) in grid.iter().enumerate() {
        let (label, mark) = if y == 0 {
            (format!("{max:>gutter$}"), tick)
        } else if y == last_row {
            (format!("{:>gutter$}", 0), tick)
        } else {
            (" ".repeat(gutter), spine)
        };
        let line: String = row.iter().collect();
        // The ideal is a reference and recedes; the line is the data and does
        // not. Painting them apart is what lets one row carry both without the
        // reader having to work out which is which.
        let painted: String = split_ideal(&line, g.ideal)
            .into_iter()
            .map(|(is_ideal, part)| {
                if is_ideal {
                    ctx.paint("chart.ideal", &part)
                } else {
                    ctx.paint("accent", &part)
                }
            })
            .collect();
        out.push_str(&format!(
            "  {} {} {painted}\n",
            ctx.paint("muted", &label),
            ctx.paint("muted", &mark.to_string())
        ));
    }
    out.push_str(&format!(
        "  {} {}\n",
        " ".repeat(gutter),
        ctx.paint(
            "muted",
            &format!("{corner}{}", rule.to_string().repeat(plot_cols + 1))
        )
    ));
    out.push_str(&format!(
        "  {} {}\n",
        " ".repeat(gutter),
        ctx.paint(
            "muted",
            &axis_labels(
                series.first().unwrap().date,
                series.last().unwrap().date,
                plot_cols + 1,
                ctx.caps.unicode
            ),
        ),
    ));
    out
}

/// Split a plotted row into runs of ideal-line cells and runs of everything
/// else, so the two can be painted in different roles without measuring
/// anything twice.
fn split_ideal(line: &str, ideal: char) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    for c in line.chars() {
        let is_ideal = c == ideal;
        match runs.last_mut() {
            Some((flag, text)) if *flag == is_ideal => text.push(c),
            _ => runs.push((is_ideal, c.to_string())),
        }
    }
    runs
}

/// The two ends of the time axis, in the calendar words `list` and `agenda`
/// use (D133). Measured from the last day, so a window reaching back past New
/// Year gives its first day a year and not its last.
fn axis_labels(first: Date, last: Date, width: usize, unicode: bool) -> String {
    let fs = crate::render::calendar_date(first, last);
    let ls = crate::render::calendar_date(last, last);
    let arrow = if unicode { "→" } else { "->" };
    if width <= fs.len() + ls.len() + 1 {
        format!("{fs} {arrow} {ls}")
    } else {
        let pad = width - fs.len() - ls.len();
        format!("{fs}{}{ls}", " ".repeat(pad))
    }
}

/// The clearing estimate beside the trend clause — derived from the SAME
/// rate `render_burndown_sized` already computed for that clause (`delta`
/// over the full `len`-day window), not a second rate of its own.
///
/// It used to average a *different* window: the trend clause read "flat over
/// 30 days" (or even "up") from the whole span, while this recomputed its own
/// rate over the last `min(7, n)` days and could disagree with the sentence
/// right next to it — "flat over 30 days · ~30d to clear at current rate" is
/// two windows contradicting each other under one summary. One rate over the
/// stated window means: a window that is flat or rising has no rate a
/// clearing date can come from, so the estimate is omitted rather than
/// printed from a rate the trend clause never mentioned.
fn project_finish(len: usize, last: i64, delta: i64, mid: &str) -> String {
    if len < 2 {
        return String::new();
    }
    if last == 0 {
        return format!(" {mid} cleared");
    }
    if delta >= 0 {
        // Flat (delta == 0) or rising (delta > 0): the window's own rate is
        // not falling, so "days to clear" has no window to project from.
        return String::new();
    }
    let per_day = (-delta) as f64 / (len - 1) as f64;
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
        let members = [
            member_status("a", (2026, 7, 13), "done"),
            member_status("b", (2026, 7, 14), "pending"),
            member_status("c", (2026, 7, 7), "done"),
            member_status("d", (2026, 6, 1), "done"),
        ];
        let buckets = throughput(&result(evs), &members, 3, anchor());
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

    /// #162: a filtered `report --html` handed its throughput chart the
    /// UNSCOPED `event.list` result, so a report scoped to one project drew
    /// bars for the whole store — a zero-match filter still showed a
    /// full-height chart. `members` (mirroring `burndown_scopes_to_members`)
    /// is the fix: an event for a task outside the scoped export must not
    /// move a bar. An id absent from `members` has no entry in `status_of` at
    /// all, which is what actually excludes it (see `throughput`'s doc
    /// comment) — there is no separate "unfiltered" mode any more; the
    /// terminal `chart throughput` command gets the same exclusion by simply
    /// passing the full store's own membership.
    #[test]
    fn throughput_scopes_to_members() {
        let evs = vec![
            ev("add", "2026-07-13T09:00:00Z", "a"),
            ev("add", "2026-07-13T09:00:00Z", "x"), // not a member
            ev("done", "2026-07-14T09:00:00Z", "a"),
            ev("done", "2026-07-14T09:00:00Z", "x"), // not a member
        ];
        let members = [member_status("a", (2026, 7, 1), "done")];
        let scoped = throughput(&result(evs.clone()), &members, 3, anchor());
        let w29 = scoped.last().unwrap();
        assert_eq!(
            w29.added, 1,
            "the non-member's add must not count: {scoped:?}"
        );
        assert_eq!(
            w29.done, 1,
            "the non-member's done must not count: {scoped:?}"
        );

        // Both in scope: both count.
        let both_members = [
            member_status("a", (2026, 7, 1), "done"),
            member_status("x", (2026, 7, 1), "done"),
        ];
        let unfiltered = throughput(&result(evs), &both_members, 3, anchor());
        let w29u = unfiltered.last().unwrap();
        assert_eq!(w29u.added, 2, "every in-scope member's event must count");
        assert_eq!(w29u.done, 2, "every in-scope member's event must count");
    }

    #[test]
    fn heatmap_counts_done_per_day() {
        let evs = vec![
            ev("done", "2026-07-13T09:00:00Z", "a"),
            ev("done", "2026-07-13T18:00:00Z", "b"), // same day => 2
            ev("done", "2026-07-10T09:00:00Z", "c"),
            ev("add", "2026-07-13T09:00:00Z", "z"), // not a completion
        ];
        let members = [
            member_status("a", (2026, 7, 1), "done"),
            member_status("b", (2026, 7, 1), "done"),
            member_status("c", (2026, 7, 1), "done"),
            member_status("z", (2026, 7, 13), "pending"),
        ];
        let days = heatmap(&result(evs), &members, 2, anchor());
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
        let members = [
            member_status("a", (2026, 7, 1), "done"),
            member_status("b", (2026, 7, 1), "done"),
            member_status("c", (2026, 7, 1), "done"),
            member_status("d", (2026, 7, 1), "done"),
        ];
        let days = heatmap(&result(evs), &members, 2, anchor());
        assert_eq!(current_streak(&days, anchor()), 3);
        assert_eq!(best_streak(&days), 3);
    }

    /// #54: the axis ends were the one date on the chart screens still spelled
    /// in ISO, beside `list` and `agenda` saying `15 Aug` (D133).
    #[test]
    fn burndown_axis_ends_use_calendar_words() {
        let day = |y: i16, m: i8, d: i8| jiff::civil::date(y, m, d);
        let same = axis_labels(day(2026, 8, 15), day(2026, 9, 13), 60, true);
        assert!(
            same.starts_with("15 Aug") && same.ends_with("13 Sep"),
            "{same:?}"
        );
        let across = axis_labels(day(2025, 12, 20), day(2026, 1, 10), 60, true);
        assert!(
            across.starts_with("20 Dec 25") && across.ends_with("10 Jan"),
            "a first day outside the last day's year carries its year: {across:?}"
        );
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
        let members = [
            member("a", (2026, 7, 10), false), // done on the 12th
            member("b", (2026, 7, 11), true),  // still open
        ];
        let series = burndown(&result(evs), &members, 5, anchor()); // 07-09..07-13
        let by: std::collections::HashMap<Date, u32> =
            series.iter().map(|p| (p.date, p.remaining)).collect();
        assert_eq!(by.get(&Date::constant(2026, 7, 9)).copied(), Some(0)); // nothing yet
        assert_eq!(by.get(&Date::constant(2026, 7, 10)).copied(), Some(1)); // a
        assert_eq!(by.get(&Date::constant(2026, 7, 11)).copied(), Some(2)); // a,b
        assert_eq!(by.get(&Date::constant(2026, 7, 12)).copied(), Some(1)); // a closed, b open
        assert_eq!(by.get(&Date::constant(2026, 7, 13)).copied(), Some(1)); // b open
    }

    /// A member born on `created`, open now or not.
    ///
    /// Both facts are the caller's, not the event log's — which is the whole
    /// point of the backwards reconstruction. A test that only listed ids could
    /// not express "this task is done now", and that is exactly the fact the
    /// forwards version had to guess at and got wrong.
    fn member(id: &str, created: (i16, i8, i8), open_now: bool) -> Member {
        // Every existing burndown fixture only ever cared about open/closed,
        // never about "done" vs "cancelled" specifically, so a closed member
        // defaults to "done" here — the common case — and the heatmap/
        // throughput tests that DO care use `member_status` instead.
        member_status(id, created, if open_now { "pending" } else { "done" })
    }

    /// A member with an explicit status string, for the tests that need to
    /// tell "done" apart from "cancelled" (#164) rather than merely open vs
    /// closed.
    fn member_status(id: &str, created: (i16, i8, i8), status: &str) -> Member {
        Member {
            id: id.to_string(),
            created: Date::constant(created.0, created.1, created.2),
            open_now: crate::render::status_is_open(status),
            status: status.to_string(),
        }
    }

    /// Collect a burndown into a date→remaining map, so a case can assert the
    /// days it cares about by name.
    fn series_by_date(
        evs: Vec<Value>,
        members: &[Member],
        days: usize,
    ) -> std::collections::HashMap<Date, u32> {
        burndown(&result(evs), members, days, anchor())
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
            &[member("a", (2026, 7, 10), true)],
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
            &[member("a", (2026, 7, 9), true)],
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
            &[member("a", (2026, 7, 10), true)],
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
            &[member("a", (2026, 6, 1), false)],
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
            &[member("a", (2026, 7, 11), true)],
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
            &[member("a", (2026, 7, 9), false)],
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
            series_by_date(lifecycle, &[member("a", (2026, 7, 10), false)], 5),
            series_by_date(noisy, &[member("a", (2026, 7, 10), false)], 5),
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
        assert_eq!(bar(3, 6, 6, &no_color), "▄▄▄", "glyphs kept under NO_COLOR");
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
        assert_eq!(cell(6, &plain), "##", "a day is two glyphs wide");
        assert!(!bar(3, 6, 6, &plain).contains('\x1b'));
    }

    /// Every figure in a chart row must sit in the same column as the one above
    /// it. The bars used to be drawn BEFORE their numbers and were only as long
    /// as their own magnitude, so each row's `done`, its counts and its `net`
    /// landed wherever that row's bar happened to end — a chart whose four
    /// columns disagreed about where they were, on the same screen as the table
    /// this alignment work started from.
    #[test]
    fn throughput_rows_line_their_columns_up_whatever_the_bars_do() {
        use crate::theme::{self, Caps, ColorDepth};
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
        let out = render_throughput(&ctx, &buckets, 0.0, false);
        // The rows are the lines that open with a week label. Found rather than
        // counted from the top: this test is about columns, and it should not
        // fail the day a heading is added above them.
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                // `W28`, not the `WEEK` header, which also opens with a W.
                t.starts_with('W') && t.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
            })
            .collect();
        assert_eq!(rows.len(), buckets.len(), "one row per week: {out}");

        // Equal display width, every row. The labels this used to search for
        // (`added`, `done`, `net`) were printed on every line and are now a
        // header printed once, so the assertion is on the GRID instead — which
        // is what it was always about, and which also catches the failure mode
        // the words could not: a painted cell padded INSIDE its escape counts
        // the SGR bytes as width and silently shortens itself.
        let width = |r: &str| unicode_width::UnicodeWidthStr::width(r);
        // The last row carries the `partial` note, so it is measured up to it.
        fn bare(r: &str) -> &str {
            r.split("  partial").next().unwrap_or(r)
        }
        /// Everything a terminal would not draw: `ESC [ … m`.
        fn strip_sgr(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
        for row in &rows {
            assert_eq!(
                width(bare(row)),
                width(bare(rows[0])),
                "a row is a different width than the first:\n{out}"
            );
        }

        // …and the numbers inside it sit in the same place, whatever the bars
        // beside them did.
        let first_digit = |r: &str| r.find(|c: char| c.is_ascii_digit());
        for row in &rows {
            assert_eq!(
                first_digit(row),
                first_digit(rows[0]),
                "the count column moved:\n{out}"
            );
        }

        // Again WITH colour, measuring visible cells.
        //
        // `Caps::PLAIN` emits no escapes at all, so a cell padded inside its
        // SGR — `format!("{:<4}", painted)`, which counts the escape bytes as
        // width and pads to nothing — comes out identical under it. That bug
        // was written into this renderer and this test could not see it. A grid
        // assertion that never runs against a painted grid is not one.
        let colour = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: ColorDepth::Truecolor,
                ansi: true,
                unicode: true,
            },
        );
        let painted = render_throughput(&colour, &buckets, 0.0, false);
        let visible: Vec<usize> = painted
            .lines()
            .filter(|l| {
                let t = strip_sgr(l);
                let t = t.trim_start();
                t.starts_with('W') && t.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
            })
            .map(|l| unicode_width::UnicodeWidthStr::width(bare(&strip_sgr(l))))
            .collect();
        assert_eq!(visible.len(), buckets.len(), "one painted row per week");

        // Colour must not change the geometry. That is the assertion that
        // catches the class, and comparing painted rows to EACH OTHER does not:
        // `label()` is always three characters, so a cell padded inside its own
        // escape loses exactly one column on every row alike, and a row-against-
        // row check sees a grid that is merely one narrower than it should be.
        // Against the unpainted render it has nowhere to hide.
        let plain_widths: Vec<usize> = rows
            .iter()
            .map(|r| unicode_width::UnicodeWidthStr::width(bare(r)))
            .collect();
        assert_eq!(
            visible, plain_widths,
            "painting the grid changed its width:\n{painted}"
        );
    }

    /// The last point equals the number of members open now, by construction.
    ///
    /// This is the invariant that was violated in the field: the dashboard's
    /// header said 258 open while the burndown beside it ended at 298. No event
    /// can be dated after today, so today's state is exactly `open_now` — and
    /// the three paths that used to break it (an imported done task, a task
    /// closed before the window, a cancel written as a `modify`) are each a
    /// case below.
    #[test]
    fn the_last_point_equals_the_tasks_open_now() {
        let members = [
            member("a", (2026, 7, 1), true),
            member("b", (2026, 7, 1), false),
            member("c", (2026, 7, 1), false),
        ];
        // No events at all — the import case: a store restored from an export
        // has one birth per task and no history.
        let series = burndown(&result(vec![]), &members, 5, anchor());
        assert_eq!(
            series.last().unwrap().remaining,
            1,
            "with no events, today's count is the tasks open now — not the member count"
        );
        // And on every earlier day too: nothing has happened since, so nothing
        // was different.
        assert!(series.iter().all(|p| p.remaining == 1));
    }

    /// A task finished BEFORE the window still draws as closed.
    ///
    /// `member_ids` comes from an unbounded `task.list` and the events from a
    /// bounded `event.list {from}`, so "no events in the window" is not "no
    /// events". Forwards this drew a task finished a month ago as open on every
    /// day of the chart.
    #[test]
    fn a_task_closed_before_the_window_is_not_drawn_open() {
        let members = [
            member("old", (2026, 1, 1), false), // done long ago, event not in window
            member("live", (2026, 1, 1), true),
        ];
        let by = series_by_date(vec![], &members, 5);
        for day in 9..=13 {
            assert_eq!(
                by.get(&Date::constant(2026, 7, day)).copied(),
                Some(1),
                "07-{day:02}: only the still-open task counts"
            );
        }
    }

    /// A cancellation written as `task.modify {status: "cancelled"}` closes the
    /// task — that is the JSON API's and MCP's only cancel path, and reading the
    /// op name alone missed every one of them.
    #[test]
    fn a_cancel_written_as_a_modify_still_closes_the_task() {
        let mut modify = ev("modify", "2026-07-11T09:00:00Z", "a");
        modify["payload"] = serde_json::json!({ "status": "cancelled" });
        let by = series_by_date(vec![modify], &[member("a", (2026, 7, 9), false)], 5);
        assert_eq!(
            by.get(&Date::constant(2026, 7, 10)).copied(),
            Some(1),
            "open before"
        );
        assert_eq!(
            by.get(&Date::constant(2026, 7, 11)).copied(),
            Some(0),
            "closed on the day"
        );
        assert_eq!(
            by.get(&Date::constant(2026, 7, 12)).copied(),
            Some(0),
            "and after"
        );
    }

    /// A task created after a day was not open on it, whatever its events say.
    #[test]
    fn a_task_is_not_open_before_it_was_created() {
        let by = series_by_date(vec![], &[member("a", (2026, 7, 12), true)], 5);
        assert_eq!(by.get(&Date::constant(2026, 7, 11)).copied(), Some(0));
        assert_eq!(by.get(&Date::constant(2026, 7, 12)).copied(), Some(1));
        assert_eq!(by.get(&Date::constant(2026, 7, 13)).copied(), Some(1));
    }

    #[test]
    fn burndown_scopes_to_members() {
        let evs = vec![
            ev("add", "2026-07-11T09:00:00Z", "a"),
            ev("add", "2026-07-11T09:00:00Z", "x"), // not a member
        ];
        let members = [member("a", (2026, 7, 11), true)];
        let series = burndown(&result(evs), &members, 3, anchor());
        assert_eq!(series.last().unwrap().remaining, 1);
    }

    // ========================================================================
    // Regression tests — tasqx audit 2026-09 (#164, #167, #233, #234)
    // ========================================================================

    /// #164: `chart heatmap` must count a task once per its CURRENT completion,
    /// not once per `done` EVENT. A task done, reopened and done again wrote
    /// two `done` events for one task that is done exactly once right now —
    /// the exact repro from the field (`report status` said 1 completed task,
    /// `chart heatmap`/`chart throughput` said 2).
    ///
    /// Written against the CURRENT (pre-fix) `heatmap` signature — it takes
    /// only the event log, with no way to learn that "a" was reopened, so it
    /// necessarily over-counts. Left in place (updated to the new signature)
    /// once the fix lands, so a regression here is caught the same way.
    #[test]
    fn heatmap_counts_a_reopened_and_redone_task_once_not_twice() {
        let evs = vec![
            ev("done", "2026-07-11T09:00:00Z", "a"),
            ev("reopen", "2026-07-12T09:00:00Z", "a"),
            ev("done", "2026-07-13T09:00:00Z", "a"),
        ];
        let members = [member_status("a", (2026, 7, 1), "done")];
        let days = heatmap(&result(evs), &members, 2, anchor());
        let total: u32 = days.iter().map(|d| d.count).sum();
        assert_eq!(
            total, 1,
            "one task, done exactly once right now, must count once on the \
             heatmap — not once per historical `done` event"
        );
        let by: std::collections::HashMap<Date, u32> =
            days.iter().map(|d| (d.date, d.count)).collect();
        assert_eq!(
            by.get(&Date::constant(2026, 7, 13)).copied(),
            Some(1),
            "counted on the LATEST completion, not the superseded first one"
        );
    }

    /// #164 mirror: `chart throughput`'s `added` column must exclude a task
    /// whose CURRENT status is cancelled, matching D24's exclusion (`report`
    /// already does this) — otherwise `throughput --weeks 520`'s added sum
    /// counts every task ever created, cancelled included, while `report`
    /// excludes them and the two numbers can never agree.
    #[test]
    fn throughput_added_excludes_a_task_that_is_currently_cancelled() {
        let evs = vec![
            ev("add", "2026-07-13T09:00:00Z", "a"),
            ev("add", "2026-07-13T09:00:00Z", "b"),
        ];
        let members = [
            member_status("a", (2026, 7, 13), "pending"),
            member_status("b", (2026, 7, 13), "cancelled"),
        ];
        let buckets = throughput(&result(evs), &members, 1, anchor());
        let added: u32 = buckets.iter().map(|b| b.added).sum();
        assert_eq!(
            added, 1,
            "a cancelled task must not inflate `added`, matching report's D24 default"
        );
    }

    /// Finishing things is not a danger state.
    ///
    /// Every non-zero heatmap cell went through the urgency ramp, which runs
    /// cold to HOT — so five completions in a day were painted the red this UI
    /// reserves for overdue and for danger, and the best thing that can appear
    /// on the chart wore the colour of the worst thing on every other one.
    ///
    /// The assertion is that the count does not reach the colour AT ALL. `░▒▓█`
    /// already differ by ink density, which is what a magnitude wants; a second
    /// channel carrying the same number is what let the first one point the
    /// wrong way without anybody noticing.
    #[test]
    fn the_completion_grid_does_not_colour_by_count() {
        use crate::theme::{self, Caps, ColorDepth};
        let ctx = Ctx::new(
            theme::default_theme(),
            Caps {
                depth: ColorDepth::Truecolor,
                ansi: true,
                unicode: true,
            },
        );
        let sgr = |s: &str| -> String {
            s.chars()
                .take_while(|c| !"░▒▓█".contains(*c))
                .collect::<String>()
        };
        let one = sgr(&cell(1, &ctx));
        for n in [2u32, 3, 4, 5, 9, 40] {
            assert_eq!(
                sgr(&cell(n, &ctx)),
                one,
                "a day with {n} completions is painted differently from one with 1"
            );
        }
        // …and that colour is not the one danger and overdue are written in.
        let danger = ctx.paint("danger", "x");
        assert!(
            !one.is_empty() && !danger.starts_with(&one),
            "completions are painted in the danger colour: {one:?}"
        );
    }

    /// A day is as wide as it is tall.
    ///
    /// One glyph per day made the grid twelve tall thin stripes rather than a
    /// calendar: a terminal cell is about twice as tall as it is wide, so a
    /// square day needs two of them.
    #[test]
    fn a_heatmap_day_is_two_cells_wide() {
        use crate::theme::{self, Caps};
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        for n in [0u32, 1, 3, 7] {
            assert_eq!(
                crate::render::width(&cell(n, &ctx)),
                2,
                "a day with {n} completions is not square"
            );
        }
    }

    /// A week with work in it never draws as a week without.
    ///
    /// One task against a peak of forty-four rounds to zero cells, and a bar of
    /// nothing is indistinguishable from the bar for nothing. The number beside
    /// it carries the magnitude; what the bar has to carry is that the week was
    /// not empty.
    #[test]
    fn a_non_zero_week_always_draws_something() {
        use crate::theme::{self, Caps};
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        assert_eq!(bar(0, 44, 10, &ctx), "", "an empty week draws nothing");
        for n in [1u32, 2, 3] {
            assert!(
                !bar(n, 44, 10, &ctx).is_empty(),
                "{n} of 44 drew an empty bar, which reads as zero"
            );
        }
    }

    /// The step line's geometry, pinned as a picture.
    ///
    /// Corners carry which two edges the line leaves a cell by, and getting one
    /// backwards draws a shape that is still a shape — it just describes a
    /// different series. That is invisible to any assertion about values, so
    /// this one asserts the drawing.
    #[test]
    fn the_step_line_turns_the_way_the_series_does() {
        let g = LineGlyphs::new(true);
        let grid = plot_step_line(&[0, 0, 2, 2, 1, 1], 3, 6, 2, &g);
        let picture: Vec<String> = grid.iter().map(|r| r.iter().collect()).collect();
        assert_eq!(
            picture,
            vec![
                // Low for two columns, up to the peak at column 2, back down
                // to the middle at column 4. The riser sits in the column of
                // the NEW value, so a change is drawn where it happened.
                "  ╭─╮ ".to_string(),
                "  │ ╰─".to_string(),
                "──╯   ".to_string(),
            ],
            "{picture:#?}"
        );
    }

    /// Zero sits on the bottom row and the peak on the top one, so a reader can
    /// take the two labels on the axis at their word.
    #[test]
    fn the_step_line_puts_zero_on_the_floor_and_the_peak_on_the_ceiling() {
        let g = LineGlyphs::new(true);
        let grid = plot_step_line(&[0, 5], 4, 2, 5, &g);
        assert_eq!(grid[3][0], '─', "0 is not on the bottom row: {grid:?}");
        assert_eq!(grid[0][1], '╭', "the peak is not on the top row: {grid:?}");
    }

    /// The ideal is a reference; the line is what happened. Where they want the
    /// same cell, the data keeps it — a reference that overwrites the thing it
    /// is a reference for has the priority backwards, and the reader cannot
    /// tell which of the two they are looking at.
    #[test]
    fn the_ideal_never_paints_over_the_line() {
        let g = LineGlyphs::new(true);
        let values = [10, 8, 6, 4, 2, 0];
        let mut grid = plot_step_line(&values, 6, 12, 10, &g);
        let before = grid.clone();
        lay_ideal(&mut grid, 10, 6, 12, 10, &g);
        for (y, row) in before.iter().enumerate() {
            for (x, &c) in row.iter().enumerate() {
                if c != ' ' {
                    assert_eq!(grid[y][x], c, "the ideal overwrote the line at {y},{x}");
                }
            }
        }
        assert!(
            grid.iter().flatten().any(|&c| c == g.ideal),
            "the ideal was not drawn at all"
        );
    }

    /// More days than columns: each column takes its bucket's LAST value.
    ///
    /// A burndown reads "as of this column". Averaging a bucket blurs the drop
    /// that ended it into the plateau before it, which is the one event in the
    /// window a reader is looking for.
    #[test]
    fn a_window_longer_than_the_terminal_shows_each_bucket_as_it_ended() {
        let g = LineGlyphs::new(true);
        // Eight days over four columns: each bucket is one 0 followed by one
        // 9, so taking the LAST value draws a line along the ceiling and
        // taking the first draws one along the floor. The two answers could
        // not look more different, which is what a guard about this needs —
        // an earlier version of this test used a series where both rules
        // happened to produce the same picture, and it passed against both.
        let values = [0, 9, 0, 9, 0, 9, 0, 9];
        let grid = plot_step_line(&values, 2, 4, 9, &g);
        let on_ceiling: Vec<bool> = (0..4).map(|x| grid[0][x] != ' ').collect();
        assert_eq!(
            on_ceiling,
            vec![true, true, true, true],
            "columns did not take their bucket's last value: {grid:?}"
        );
    }

    /// #167, restated for the shape that replaced the ramp: a rising burndown
    /// must not flatten on a terminal without Unicode.
    ///
    /// The original defect was an ASCII ramp of three glyphs, which collapsed
    /// an 84% rise into two visually distinct bars. A step line cannot have
    /// that defect by construction — its resolution is its ROW COUNT, and rows
    /// are the same in both alphabets — so the guard now asserts the property
    /// the ramp was supposed to have rather than the glyph count it was fixed
    /// to. If the two alphabets ever disagree about a shape, that is the bug.
    #[test]
    fn a_rising_burndown_has_the_same_shape_without_unicode() {
        let values: Vec<u32> = (0..=10).map(|i| i * 3).collect();
        let rows_of = |unicode: bool| -> Vec<usize> {
            let g = LineGlyphs::new(unicode);
            let grid = plot_step_line(&values, 8, 22, 30, &g);
            // The row each column's line sits on, read back off the picture.
            (0..22)
                .map(|x| {
                    (0..8)
                        .find(|&y| grid[y][x] != ' ')
                        .expect("every column carries the line")
                })
                .collect()
        };
        let uni = rows_of(true);
        assert_eq!(
            uni,
            rows_of(false),
            "the alphabets disagree about the shape"
        );

        // Where the line GOES is glyph-independent, so the assertion above
        // cannot fail on an alphabet alone — the thing an alphabet can lose is
        // the distinction a cell draws. A corner may collapse to `+` (ASCII has
        // no way to say which two edges it uses, and the shape survives), but a
        // run along the line and a run across it must stay tellable apart, or
        // the picture stops being readable while every row index still agrees.
        for unicode in [true, false] {
            let g = LineGlyphs::new(unicode);
            assert_ne!(
                g.horizontal, g.vertical,
                "horizontal and vertical collapsed to one glyph (unicode={unicode})"
            );
            assert_ne!(
                g.horizontal, g.ideal,
                "the data and the reference collapsed to one glyph (unicode={unicode})"
            );
        }
        let levels: std::collections::BTreeSet<usize> = uni.iter().copied().collect();
        assert!(
            levels.len() >= 6,
            "a rising series flattened into {} levels: {uni:?}",
            levels.len()
        );
        assert!(
            uni.windows(2).all(|w| w[0] >= w[1]),
            "a monotonically rising series must never dip on screen: {uni:?}"
        );
    }

    /// #234 item 9: "current streak 1 days" does not singularise.
    #[test]
    fn heatmap_streak_line_singularises_one_day() {
        let evs = vec![ev("done", "2026-07-13T09:00:00Z", "a")];
        let members = [member_status("a", (2026, 7, 1), "done")];
        let days = heatmap(&result(evs), &members, 1, anchor());
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        let out = render_heatmap(&ctx, &days, anchor(), false);
        assert!(
            out.contains("streak 1 day ")
                || out.contains("streak 1 day\n")
                || out.contains("streak 1 day·"),
            "expected a singular \"1 day\", got: {out:?}"
        );
        assert!(
            !out.contains("streak 1 days"),
            "\"1 days\" is ungrammatical: {out:?}"
        );
    }

    /// #234 item 9 mirror: burndown's "flat over 1 days" / "up N over 1 days".
    #[test]
    fn burndown_trend_line_singularises_one_day() {
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        let series = [RemainingPoint {
            date: anchor(),
            remaining: 3,
        }];
        let out = render_burndown(&ctx, &series, "test", true);
        assert!(
            !out.contains("over 1 days"),
            "\"over 1 days\" is ungrammatical: {out:?}"
        );
        assert!(out.contains("over 1 day"), "expected singular: {out:?}");
    }

    /// #234 item 6: on a store that has never held a task at all, the
    /// burndown must not invent a y-axis maximum of "1" (nothing was ever
    /// plotted at that height) and must not congratulate the user with
    /// "cleared" (there was never a backlog to clear).
    #[test]
    fn burndown_on_a_store_with_no_tasks_says_so_instead_of_inventing_an_axis() {
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        let series: Vec<RemainingPoint> = (0..30)
            .map(|i| RemainingPoint {
                date: anchor().saturating_sub((i as i64).days()),
                remaining: 0,
            })
            .collect();
        let out = render_burndown(&ctx, &series, "all tasks", false);
        assert!(
            !out.contains("cleared"),
            "a store that never had a task was not \"cleared\": {out:?}"
        );
        assert!(
            out.contains("no open tasks yet") || out.contains("no tasks"),
            "expected a plain no-data message, got: {out:?}"
        );
    }

    /// #234 item 4: `chart throughput --weeks 1` must not label a single
    /// partial week's rate "4-wk velocity" — the 4-week figure must come from
    /// the last four COMPLETE ISO weeks, independent of the display window.
    /// Reproduced here at the `render_throughput` level against the CURRENT
    /// signature, which derives velocity from whatever `buckets` it was
    /// handed — so a 1-bucket window makes "4-wk" a lie.
    #[test]
    fn throughput_four_week_velocity_is_independent_of_the_display_window() {
        // Four complete weeks of history (2 done/week), then a 5th (current,
        // partial) week with a burst of 10 done — done/wk over the real last
        // four complete weeks is 2.0, not whatever a 1-week window would say.
        // `anchor()` is a Monday, so `anchor - 7*k` lands on the Monday of the
        // ISO week `k` weeks earlier — arithmetic, not a formatted day-of-month
        // that could run negative.
        let mut evs = Vec::new();
        let mut members = Vec::new();
        for w in 0..4u32 {
            let week_monday = anchor().saturating_sub(((7 * (4 - w)) as i64).days());
            for i in 0..2u32 {
                let id = format!("w{w}-{i}");
                let d = week_monday.saturating_add((i as i64).days());
                evs.push(ev("done", &format!("{d}T09:00:00Z"), &id));
                members.push(member_status(&id, (2026, 6, 1), "done"));
            }
        }
        for i in 0..10 {
            let id = format!("current-{i}");
            evs.push(ev("done", "2026-07-13T09:00:00Z", &id));
            members.push(member_status(&id, (2026, 7, 13), "done"));
        }
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        let one_week = throughput(&result(evs.clone()), &members, 1, anchor());
        let velocity = velocity_4wk(&result(evs), &members, anchor());
        let out = render_throughput(&ctx, &one_week, velocity, false);
        assert!(
            out.contains("4-wk velocity 2.0 done/wk"),
            "the 4-wk figure must reflect the last 4 COMPLETE weeks (2.0/wk), \
             not the single partial week on screen: {out:?}"
        );
    }

    /// #234 item 8: a heatmap window padded out to whole weeks always carries
    /// a few always-zero days after today (the rest of the current ISO week).
    /// Those must not render with the SAME glyph as a day that already
    /// happened and simply had no completions — that reads as an idle day,
    /// when it has not occurred yet at all.
    ///
    /// `anchor()` is a Monday, so within its own (single) week only Monday
    /// itself is "today or earlier" and Tue–Sun are all still ahead of it —
    /// exactly the padded tail the real bug hits at any anchor.
    #[test]
    fn heatmap_does_not_draw_future_days_as_idle_ones() {
        let days: Vec<DayCount> = (0..7)
            .map(|i| DayCount {
                date: anchor().saturating_add((i as i64).days()),
                count: 0,
            })
            .collect();
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        // Not an empty store — a store with real history whose display
        // window happens to be mostly future padding. `store_empty` is the
        // caller's fact to assert, not something this all-zero `days` window
        // could stand in for (see `render_heatmap`'s doc comment).
        let out = render_heatmap(&ctx, &days, anchor(), false);
        let mon = out.lines().find(|l| l.contains("Mon")).unwrap();
        assert!(
            mon.contains('.'),
            "Monday (the anchor itself) already happened and had zero \
             completions — it must draw the ordinary idle glyph: {mon:?}"
        );
        // Only every other weekday label is printed, so the remaining rows are
        // found RELATIVE to Monday's rather than counted from the top of the
        // output — every row after it in this single-week fixture is a future
        // day and must not carry the idle glyph. Counting from the top made
        // this fail the day the grid gained a month strip above it, which is a
        // change it has no opinion about.
        let mon_at = out
            .lines()
            .position(|l| l.contains("Mon"))
            .expect("a Monday row");
        let future_rows: Vec<&str> = out.lines().skip(mon_at + 1).take(6).collect();
        assert_eq!(
            future_rows.len(),
            6,
            "expected the 6 remaining weekday rows: {out:?}"
        );
        for row in future_rows {
            assert!(
                !row.contains('.'),
                "a day that has not happened yet must not draw as idle: {row:?}\n{out}"
            );
        }
    }

    /// #233.2: a genuinely empty store must print the same one-line
    /// empty-state treatment `report` already gives, not twelve rows of
    /// `added 0 done 0 net 0`, which reads as a broken tool rather than an
    /// empty one. Driven by `store_empty` rather than an all-zero `buckets`:
    /// a real user whose current window simply has no activity yet produces
    /// the identical all-zero series a fresh store does, so only the fact
    /// the caller already has (member list is empty) can tell them apart.
    #[test]
    fn render_throughput_short_circuits_an_entirely_empty_series() {
        use crate::theme::{self, Caps};
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let buckets: Vec<WeekBucket> = (0..12)
            .map(|i| WeekBucket {
                iso_year: 2026,
                iso_week: 26 + i,
                added: 0,
                done: 0,
            })
            .collect();
        let out = render_throughput(&ctx, &buckets, 0.0, true);
        assert!(
            !out.contains("added   0") && !out.contains("W26"),
            "still drawing the zero grid: {out}"
        );
        assert!(
            out.to_lowercase().contains("no") && (out.contains("event") || out.contains("task")),
            "must name the empty state, not just omit the grid: {out:?}"
        );
        assert_eq!(out.lines().count(), 1, "one line, like `report`'s: {out:?}");

        // A non-empty store still draws the grid, even over an all-zero window.
        assert!(render_throughput(&ctx, &buckets, 0.0, false).contains("W26"));
    }

    /// #233.2: same short-circuit for the heatmap, on the same `store_empty`
    /// signal — see `render_throughput_short_circuits_an_entirely_empty_series`.
    #[test]
    fn render_heatmap_short_circuits_an_entirely_empty_series() {
        use crate::theme::{self, Caps};
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let days: Vec<DayCount> = (0..84)
            .map(|i| DayCount {
                date: anchor().saturating_add((i as i64).days()),
                count: 0,
            })
            .collect();
        let out = render_heatmap(&ctx, &days, anchor(), true);
        assert!(!out.contains("Mon"), "still drawing the empty grid: {out}");
        assert_eq!(out.lines().count(), 1, "one line, like `report`'s: {out:?}");

        // A non-empty store still draws the grid, even over an all-zero window.
        assert!(render_heatmap(&ctx, &days, anchor(), false).contains("Mon"));
    }

    /// #13: the legend must be a KEY to the grid — each swatch painted with
    /// exactly the style (glyph + role) `cell` uses for a day at that level —
    /// not one flat span in one muted colour with only the glyph changing.
    ///
    /// Checked under truecolor, under the `mono` theme (where a grid cell's
    /// non-zero levels are bold and the zero level is dim — no colour is
    /// involved at all, so a legend that merely recolours itself uniformly
    /// cannot pass here even by accident) and under plain/NO_COLOR (where the
    /// glyph alone still has to carry the distinction).
    #[test]
    fn heatmap_legend_swatches_match_grid_cells() {
        use crate::theme::{self, Caps, ColorDepth};
        let truecolor = Caps {
            depth: ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        };
        let cases = [
            (
                "nord/truecolor",
                Ctx::new(theme::default_theme(), truecolor),
            ),
            (
                "mono/truecolor",
                Ctx::new(theme::builtin("mono").expect("mono is built in"), truecolor),
            ),
            (
                "plain/no-color",
                Ctx::new(theme::default_theme(), Caps::PLAIN),
            ),
        ];
        // 84 empty days (12 weeks) — the legend sits in the header line
        // regardless of what the grid underneath draws.
        let days: Vec<DayCount> = (0..84)
            .map(|i| DayCount {
                date: anchor().saturating_sub(((83 - i) as i64).days()),
                count: 0,
            })
            .collect();
        for (name, ctx) in &cases {
            let out = render_heatmap(ctx, &days, anchor(), false);
            let legend_line = out.lines().next().expect("a header line");
            for level in [0u32, 1, 3, 5] {
                let swatch = cell(level, ctx);
                assert!(
                    legend_line.contains(&swatch),
                    "[{name}] legend is missing the level-{level} grid swatch \
                     {swatch:?} (painted the same way a grid cell would be): \
                     legend line = {legend_line:?}"
                );
            }
        }
    }

    /// #14: the trend clause and the clearing estimate must come from ONE
    /// rate over the stated window, not two different windows that can
    /// disagree.
    ///
    /// This series falls 10 over its full 30-day span but is flat over its
    /// last 7 days (the window the old, buggy projection used on its own) —
    /// so the old code's "recent burn rate" was zero right when the headline
    /// trend was falling, and it printed "not burning down" beside a falling
    /// trend. Deriving both from the one 30-day rate keeps them agreeing.
    #[test]
    fn burndown_facts_falling_window_uses_the_trend_clauses_own_rate() {
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        let vals: [u32; 31] = [
            30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, // 10/10 days, 1/day
            20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20, 20,
            20, // flat
        ];
        let series: Vec<RemainingPoint> = vals
            .iter()
            .enumerate()
            .map(|(i, &remaining)| RemainingPoint {
                date: anchor().saturating_sub(((vals.len() - 1 - i) as i64).days()),
                remaining,
            })
            .collect();
        let out = render_burndown(&ctx, &series, "all tasks", true);
        assert!(
            out.contains("down 10 over 31 days"),
            "trend clause missing or wrong: {out:?}"
        );
        assert!(
            !out.contains("not burning down"),
            "a falling trend must not also claim it is not burning down: {out:?}"
        );
        assert!(
            out.contains("~60d to clear at current rate"),
            "the clearing estimate must use the SAME 30-day rate as the trend \
             clause (10 over 30 days => 1/3 per day => 60d for the 20 left), \
             not a separate last-7-days rate: {out:?}"
        );
    }

    /// #14: the exact shape of the reported bug — "remaining open · all
    /// tasks   15 left · flat over 30 days · ~Nd to clear at current rate".
    /// The series is flat end-to-end (15 -> 15 over 30 days) but dips down
    /// and back up in its last week, which is what let the old
    /// last-7-days-only projection compute a positive rate and print a
    /// clearing estimate beside a trend clause that says nothing is moving.
    #[test]
    fn burndown_facts_flat_window_omits_the_clearing_estimate() {
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        let mut vals = vec![15u32; 23]; // days 0..=22, unchanged
        vals.extend([20, 19, 18, 17, 16, 15, 15]); // days 23..=29: down then flat
        assert_eq!(vals.len(), 30);
        let series: Vec<RemainingPoint> = vals
            .iter()
            .enumerate()
            .map(|(i, &remaining)| RemainingPoint {
                date: anchor().saturating_sub(((vals.len() - 1 - i) as i64).days()),
                remaining,
            })
            .collect();
        let out = render_burndown(&ctx, &series, "all tasks", true);
        assert!(
            out.contains("15 left"),
            "expected the last value in the facts line: {out:?}"
        );
        assert!(
            out.contains("flat over 30 days"),
            "trend clause missing or wrong: {out:?}"
        );
        assert!(
            !out.contains("to clear"),
            "a flat window's net rate is zero, so a clearing estimate has no \
             meaning and must be omitted, not computed from a shorter \
             recent-days window: {out:?}"
        );
        assert!(
            !out.contains("not burning down"),
            "omitted means absent, not replaced with another clause: {out:?}"
        );
    }

    /// #14: a rising window likewise has no rate a clearing date can come
    /// from, so the estimate must be OMITTED — not printed as "not burning
    /// down", which still reads as a clause about clearing.
    #[test]
    fn burndown_facts_rising_window_omits_the_clearing_estimate() {
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN);
        let series: Vec<RemainingPoint> = (0..21)
            .map(|i| RemainingPoint {
                date: anchor().saturating_sub(((20 - i) as i64).days()),
                remaining: 10 + i as u32, // 10 -> 30, straight rise
            })
            .collect();
        let out = render_burndown(&ctx, &series, "all tasks", true);
        assert!(
            out.contains("up 20 over 21 days"),
            "trend clause missing or wrong: {out:?}"
        );
        assert!(
            !out.contains("to clear") && !out.contains("not burning down"),
            "a rising window must carry no clearing clause at all: {out:?}"
        );
    }
}
