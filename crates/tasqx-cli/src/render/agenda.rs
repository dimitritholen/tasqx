//! `task.list` re-ordered by time instead of urgency: the `Agenda` model and
//! its day-grouped table/JSON renderers (task #727 split of `render.rs`).

use super::*;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Unit};
use serde_json::Value;
use tasqx_core::filter::overdue_at;

use crate::theme::Ctx;
use crate::AGENDA_MAX_DAYS;

// ============================================================================
// Agenda — the same `task.list` answer, ordered by time instead of urgency
// ============================================================================

/// Which dated field put a task on the agenda.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum When {
    Due,
    Scheduled,
}

impl When {
    /// The word the `WHEN` cell opens with. Short, because it is repeated on
    /// every row and the date beside it is the information.
    pub(crate) fn label(self) -> &'static str {
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
pub(crate) struct Entry<'a> {
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
pub(crate) const AGENDA_OVERDUE_CAP: usize = 20;

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
pub(crate) enum Group {
    Overdue,
    Day(Date),
}

pub(crate) fn group_of(e: &Entry) -> Group {
    if e.overdue {
        Group::Overdue
    } else {
        Group::Day(e.day)
    }
}

/// `Today · Mon 3 Aug`. The date is spelled out on every heading, today's
/// included: "Today" alone is the one label that means something different
/// tomorrow, and terminal output gets pasted into tickets. It is spelled by
/// [`calendar_date`], `list`'s spelling, not ISO (D133), so a heading more than
/// a year out keeps its two-digit year and still names exactly one day.
pub(crate) fn day_heading(day: Date, today: Date) -> String {
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
pub(crate) fn when_cell(kind: When, at: Timestamp, overdue_now: Option<Timestamp>) -> String {
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
    pub(crate) fn at_start_of_today(&self) -> Timestamp {
        self.today
            .to_zoned(TimeZone::UTC)
            .map(|z| z.timestamp())
            .unwrap_or_else(|_| crate::clock::now())
    }

    /// One line per reason this view is holding something back, each naming the
    /// way to see it. Empty on an agenda that omitted nothing — the notes are
    /// conditional so that their presence always means something.
    pub(crate) fn omissions(&self) -> Vec<String> {
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

#[cfg(test)]
mod tests;
