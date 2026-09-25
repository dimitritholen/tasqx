//! D189: `store.import --merge` decides a known task's scalars field group by
//! field group, from the event log, and counts tracked time from the union of
//! both logs.
//!
//! D185 took every scalar from whichever copy had the later task-level
//! `modified`, so a machine that completed a task and then lost the race to a
//! later `adjust` elsewhere came back `pending`, and one side's tracked time
//! replaced the other's instead of adding to it (#766). D3 names per-field
//! last-writer-wins driven off the append-only event log; this module is that
//! rule, and nothing else: `store_import` hands it the two copies of the row
//! and the two sides' events, and writes what it answers.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;
use serde_json::Value;

use crate::datetime;
use crate::util::{duration_secs, parse_ts};

/// The scalar columns a merge decides, in groups that move as one unit.
///
/// A group is the set of columns one operation writes together, so taking
/// half of it from each side could build a row no sequence of calls reaches:
/// `status` with `completed`, `active_since` and the D165 delivery pin
/// (`done` sets all four, `reopen` clears two, `start`/`stop` move the
/// anchor); and the four dates `task.modify` validates against each other
/// (#141/#142: `due` after `scheduled` and `wait`, an offset `remind` needs a
/// `due`), so a merge cannot pair one side's `wait` with the other's earlier
/// `due`. Every other column is its own group. `created`, `short_id` and the
/// tracked pair are not here: the first two keep the store's value, and the
/// tracked pair is summed, not chosen ([`merged_tracked`]).
pub(super) const GROUPS: &[(&str, &[&str])] = &[
    ("title", &["title"]),
    (
        "status",
        &[
            "status",
            "completed",
            "active_since",
            "delivered_annotation_id",
        ],
    ),
    ("priority", &["priority"]),
    ("project", &["project"]),
    ("schedule", &["due", "scheduled", "wait", "remind"]),
    ("estimate", &["estimate"]),
    ("recurrence", &["recurrence"]),
    ("budget_tokens", &["budget_tokens"]),
    ("spawned_from", &["spawned_from"]),
];

/// One task event, as either side of a merge holds it.
pub(super) struct LoggedEvent {
    pub id: String,
    pub op: String,
    pub payload: Value,
    pub ts: String,
}

/// Which copy a field group, or the whole row, is taken from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Side {
    Store,
    Payload,
}

/// The scalar half of one task row, owned, keyed by column name.
#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Scalars(pub HashMap<&'static str, Value>);

impl Scalars {
    pub fn get(&self, field: &str) -> &Value {
        self.0.get(field).unwrap_or(&Value::Null)
    }

    pub fn text(&self, field: &str) -> Option<String> {
        self.get(field).as_str().map(str::to_string)
    }

    pub fn int(&self, field: &str) -> Option<i64> {
        self.get(field).as_i64()
    }
}

/// The group a `task.modify` key belongs to. `tracked` is none of them: an
/// absolute total is a reset in [`merged_tracked`], not a chosen value.
fn group_of(field: &str) -> Option<&'static str> {
    GROUPS
        .iter()
        .find(|(_, fields)| fields.contains(&field))
        .map(|(g, _)| *g)
}

/// Whether `ev` wrote a column of `group`. `add` writes the whole row; every
/// lifecycle op writes the status group (`undo` of a `stop` reopens the
/// interval); a `modify` writes exactly the keys its `set` named.
fn touches(ev: &LoggedEvent, group: &str) -> bool {
    match ev.op.as_str() {
        "add" => true,
        "start" | "stop" | "done" | "cancel" | "reopen" => group == "status",
        "undo" => group == "status" && ev.payload["reverted_op"] == "stop",
        "modify" => ev
            .payload
            .as_object()
            .is_some_and(|set| set.keys().any(|k| group_of(k) == Some(group))),
        _ => false,
    }
}

/// Where an event sits in a log: its effective instant, a rank, then its id.
///
/// The order is causal, not only chronological. An event that closes a
/// running interval (`stop`, `done`, `cancel`) names the instant that
/// interval opened (`interval_started`), and it cannot have happened before
/// it: a close stamped earlier than its own start was written by a machine
/// whose clock ran slow, so it takes the start's instant and rank 1, which
/// places it after every other event at that instant, the start included.
/// The id breaks what is left of a tie the same way on every machine, so two
/// stores merging each other agree on which came last. An unparseable `ts`
/// sorts first.
type Order<'a> = (Option<Timestamp>, u8, &'a str);

fn order(ev: &LoggedEvent) -> Order<'_> {
    let ts = parse_ts(&ev.ts);
    let opened = matches!(ev.op.as_str(), "stop" | "done" | "cancel")
        .then(|| ev.payload["interval_started"].as_str().and_then(parse_ts))
        .flatten();
    match (ts, opened) {
        (Some(t), Some(since)) if since >= t => (Some(since), 1, ev.id.as_str()),
        _ => (ts, 0, ev.id.as_str()),
    }
}

fn latest_touch<'a>(events: &'a [LoggedEvent], group: &str) -> Option<Order<'a>> {
    events.iter().filter(|e| touches(e, group)).map(order).max()
}

/// Whether both sides hold the task's `add` — the one event that proves a
/// log is the task's history rather than a fragment of it. A task that
/// reached a store through a document without events holds only an
/// `import`, which says nothing about which field anyone last wrote.
pub(super) fn both_hold_the_add(store: &[LoggedEvent], payload: &[LoggedEvent]) -> bool {
    let has_add = |log: &[LoggedEvent]| log.iter().any(|e| e.op == "add");
    has_add(store) && has_add(payload)
}

/// The side whose latest event touching `group` is later, by [`Order`].
/// Everything the logs cannot decide — the very same latest event on both
/// sides, a group only one log touches, or neither — answers `tie`, which the
/// caller computes the same way whichever side is the store ([`tie_side`]).
pub(super) fn group_winner(
    store: &[LoggedEvent],
    payload: &[LoggedEvent],
    group: &str,
    tie: Side,
) -> Side {
    match (latest_touch(store, group), latest_touch(payload, group)) {
        (Some(s), Some(p)) if p > s => Side::Payload,
        (Some(s), Some(p)) if s > p => Side::Store,
        _ => tie,
    }
}

/// The side a group goes to when the logs cannot decide it: the copy with
/// the later task-level `modified` (D185), and at equal stamps the copy whose
/// values sort higher as JSON text. Both are symmetric in the two copies, so
/// the two directions of a merge pick the same values; equal values make the
/// choice moot.
pub(super) fn tie_side(
    stored_modified: &str,
    payload_modified: &str,
    stored: &[&Value],
    payload: &[&Value],
) -> Side {
    let text = |v: &[&Value]| serde_json::to_string(v).unwrap_or_default();
    let by_stamp = parse_ts(payload_modified).cmp(&parse_ts(stored_modified));
    match by_stamp.then_with(|| text(payload).cmp(&text(stored))) {
        std::cmp::Ordering::Greater => Side::Payload,
        _ => Side::Store,
    }
}

/// What one log (or the union of two) says about the tracked pair.
struct Counted<'a> {
    tracked: i64,
    adjustment: i64,
    /// The latest absolute `task.modify tracked:` reset the count starts at.
    reset: Option<Order<'a>>,
}

/// Seconds in an ISO duration an event wrote.
fn iso_secs(v: &Value) -> Option<i64> {
    v.as_str().and_then(duration_secs)
}

fn secs_between(from: Timestamp, to: Option<Timestamp>) -> i64 {
    to.map_or(0, |b| (b.as_second() - from.as_second()).max(0))
}

/// Whether `ev` closes a running interval: `stop`, `done`, `cancel`, or a
/// cancelling `modify`.
fn closes(ev: &LoggedEvent) -> bool {
    match ev.op.as_str() {
        "stop" | "done" | "cancel" => true,
        "modify" => ev.payload.get("status").is_some(),
        _ => false,
    }
}

/// The intervals a count has closed: each one's length by the instant it
/// opened, every interval ever closed (a reset drops the lengths, not the
/// fact of the close), which event closed which, and the one closed last.
#[derive(Default)]
struct Tally<'a> {
    intervals: HashMap<Timestamp, i64>,
    closed: HashSet<Timestamp>,
    closed_by: HashMap<&'a str, Timestamp>,
    last: Option<Timestamp>,
}

impl<'a> Tally<'a> {
    /// Close the interval opened at `since` with `secs`, the event `by`
    /// closing it. One interval closed twice keeps the longer close.
    fn close(&mut self, by: &'a str, since: Timestamp, secs: i64) {
        let held = self.intervals.entry(since).or_insert(secs);
        *held = (*held).max(secs);
        self.closed.insert(since);
        self.closed_by.insert(by, since);
        self.last = Some(since);
    }
}

/// Count tracked time from a log.
///
/// Every timed interval is its own entity, keyed by the instant it opened,
/// and its length comes from the event that closed it — the one measure
/// taken on a single machine's clock: a `stop`'s recorded `tracked`, or a
/// `done`/`cancel`'s own instant minus the `interval_started` it names, which
/// is exactly what the closing machine folded in. No length is ever read off
/// the gap between two events two different machines stamped. An interval
/// closed on both machines is one entity and counts once, at the longer of
/// its two closes. A `stop` that names no interval (written before closes
/// carried one) closes the one the log last opened, with its own recorded
/// length, and one that finds nothing open re-closes the one closed last; a
/// `done` or `cancel` that names none closes nothing.
///
/// An interval that NO close ends is either the running timer (`running`,
/// the merged row's `active_since`, which is not counted) or was left
/// running on one machine while another moved the task on. It is banked up
/// to the next status event after it in the union, unless that event is a
/// `start`: a new start while an interval is open can only come from a
/// machine that never saw it open (on the machine that had it open, a start
/// writes nothing), so the gap between the two is two clocks apart and is
/// not counted. The banked span runs from the start to the FIRST event of
/// any kind after it, which is never later than the next event the start's
/// own machine wrote, so a later event on the other machine cannot stretch
/// it; only what lies after the latest reset counts. The bound is read from
/// the union alone, never from which log holds what, so several stores
/// merged in any order bank the same span.
///
/// `adjust_tracked` adds its delta to both columns, `undo` takes a stop or an
/// adjustment back, and `modify tracked:` resets: the count restarts at the
/// value it set, with no adjustment.
fn count<'a>(logs: &[&[&'a LoggedEvent]], running: Option<Timestamp>) -> Counted<'a> {
    let mut ordered: Vec<&LoggedEvent> = logs.concat();
    ordered.sort_by_key(|e| order(e));
    ordered.dedup_by_key(|e| e.id.as_str());
    let by_id: HashMap<&str, &LoggedEvent> = ordered.iter().map(|e| (e.id.as_str(), *e)).collect();

    let mut base = (0i64, 0i64);
    let mut reset: Option<Order> = None;
    let mut t = Tally::default();
    let mut others: HashMap<&str, (i64, i64)> = HashMap::new();
    let mut opened: Vec<(Timestamp, usize)> = Vec::new();
    let mut open: Option<Timestamp> = None;

    for (at, ev) in ordered.iter().enumerate() {
        let ts = parse_ts(&ev.ts);
        let named = ev.payload["interval_started"].as_str().and_then(parse_ts);
        match ev.op.as_str() {
            "start" => {
                if let Some(since) = named.or(ts) {
                    opened.push((since, at));
                    open = Some(since);
                }
            }
            "stop" => {
                let recorded = iso_secs(&ev.payload["tracked"]);
                match named.or(open).or(t.last) {
                    Some(since) => {
                        t.close(&ev.id, since, recorded.unwrap_or(secs_between(since, ts)))
                    }
                    None => {
                        others.insert(&ev.id, (recorded.unwrap_or(0), 0));
                    }
                }
                if open.is_some_and(|o| named.is_none_or(|n| n == o)) {
                    open = None;
                }
            }
            "adjust_tracked" => {
                let d = ev.payload["delta_seconds"].as_i64().unwrap_or(0);
                others.insert(&ev.id, (d, d));
            }
            "undo" => match ev.payload["reverted_op"].as_str() {
                Some("stop") => {
                    let reverted = ev.payload["reverted"].as_str();
                    match reverted.and_then(|id| t.closed_by.get(id)).copied() {
                        Some(since) => {
                            t.intervals.remove(&since);
                            t.closed.remove(&since);
                        }
                        None => {
                            let back = iso_secs(&ev.payload["restored"]["tracked"]).unwrap_or(0);
                            others.insert(&ev.id, (-back, 0));
                        }
                    }
                    let since = ev.payload["restored"]["interval_started"]
                        .as_str()
                        .and_then(parse_ts);
                    if let Some(since) = since {
                        opened.push((since, at));
                        open = Some(since);
                    }
                }
                Some("adjust_tracked") => {
                    let d = ev.payload["reverted"]
                        .as_str()
                        .and_then(|id| by_id.get(id))
                        .and_then(|r| r.payload["delta_seconds"].as_i64())
                        .unwrap_or(0);
                    others.insert(&ev.id, (-d, -d));
                }
                _ => {}
            },
            _ => {
                if ev.op == "modify" {
                    if let Some(v) = ev.payload.get("tracked") {
                        let set = v
                            .as_str()
                            .and_then(|s| datetime::parse_duration(s).ok())
                            .and_then(|iso| duration_secs(&iso))
                            .unwrap_or(0);
                        base = (set, 0);
                        reset = Some(order(ev));
                        t.intervals.clear();
                        others.clear();
                        t.last = None;
                    }
                }
                // A `done` or `cancel` closes only the interval it names: one
                // that names none was written on a machine with no timer
                // running (or before closes named theirs), and closing the
                // log's open interval with it would read a length off two
                // machines' clocks. That interval is banked instead, below.
                if let Some(since) = named.filter(|_| closes(ev)) {
                    t.close(&ev.id, since, secs_between(since, ts));
                    if open == Some(since) {
                        open = None;
                    }
                }
            }
        }
    }

    // Intervals no close ended, other than the running one: banked up to the
    // next status event after them, unless that is a start.
    let mut banked = 0i64;
    let mut seen: HashSet<Timestamp> = HashSet::new();
    for (since, at) in opened {
        if t.closed.contains(&since) || running == Some(since) || !seen.insert(since) {
            continue;
        }
        let after = &ordered[at + 1..];
        let next = after.iter().find(|e| touches(e, "status"));
        if next.is_none_or(|e| e.op == "start" || e.op == "undo") {
            continue;
        }
        // The span runs to the FIRST event of any kind after the start: a
        // bound no later than the next event the start's own machine wrote.
        let Some(end) = after.first().and_then(|e| parse_ts(&e.ts)) else {
            continue;
        };
        // Only what lies after the latest reset counts.
        let from = reset.and_then(|r| r.0).map_or(since, |r| r.max(since));
        banked = banked.saturating_add(secs_between(from, Some(end)));
    }

    let (tracked, adjustment) = t
        .intervals
        .values()
        .map(|s| (*s, 0))
        .chain(others.values().copied())
        .fold(base, |(t, a), (dt, da)| {
            (t.saturating_add(dt), a.saturating_add(da))
        });
    Counted {
        tracked: tracked.saturating_add(banked),
        adjustment,
        reset,
    }
}

/// The running timer each copy of the row carries, and the merged row's.
pub(super) struct Running {
    pub stored: Option<Timestamp>,
    pub payload: Option<Timestamp>,
    pub merged: Option<Timestamp>,
}

/// The merged `(tracked_seconds, tracked_adjustment_seconds)`: a symmetric
/// function of the two sides, so both directions of a merge agree.
///
/// The history-derived part is [`count`] over the UNION of both logs. What a
/// side's total holds beyond what its own log counts — a legacy row, a task
/// that arrived by a plain import, time from before the log began — is its
/// residual, and the larger of the two residuals is added, never their sum:
/// one side may have received the other's residual through an earlier
/// import, and a sum would count it twice. A residual whose side does not
/// hold the union's latest reset predates that reset and is dropped. The
/// adjustment follows the same rule, and is clamped with the total: when the
/// sum would take the total below zero, the total stops at zero and the
/// adjustment moves by the same amount, since it is part of the total.
pub(super) fn merged_tracked(
    stored: (i64, i64),
    payload: (i64, i64),
    store_events: &[LoggedEvent],
    payload_events: &[LoggedEvent],
    running: &Running,
) -> (i64, i64) {
    let s_log: Vec<&LoggedEvent> = store_events.iter().collect();
    let p_log: Vec<&LoggedEvent> = payload_events.iter().collect();
    let union = count(&[&s_log, &p_log], running.merged);
    let residual = |total: (i64, i64), log: &[&LoggedEvent], own_running| {
        let own = count(&[log], own_running);
        (own.reset == union.reset).then(|| (total.0 - own.tracked, total.1 - own.adjustment))
    };
    let kept: Vec<(i64, i64)> = [
        residual(stored, &s_log, running.stored),
        residual(payload, &p_log, running.payload),
    ]
    .into_iter()
    .flatten()
    .collect();
    // Never below zero: a residual is time a total holds that its history
    // does not explain, and a history can only over-explain a total when it
    // was misread.
    let rest_t = kept.iter().map(|r| r.0).max().unwrap_or(0).max(0);
    let rest_a = kept.iter().map(|r| r.1).max().unwrap_or(0);
    let tracked = union.tracked.saturating_add(rest_t);
    let adjustment = union.adjustment.saturating_add(rest_a);
    if tracked < 0 {
        (0, adjustment.saturating_sub(tracked))
    } else {
        (tracked, adjustment)
    }
}
