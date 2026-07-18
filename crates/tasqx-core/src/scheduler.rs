//! The reminder scheduler (DESIGN.md §9, daemon path).
//!
//! An in-memory **min-heap of upcoming reminder timestamps**, rebuilt from the
//! store on daemon start and whenever an event says a task changed. The daemon
//! owns the thread and the clock; everything here is pure enough to test:
//!
//!  * [`ReminderScheduler::rebuild`] reads the store. It takes **no** `now` —
//!    it loads every *unfired* reminder regardless of when it ripens, so
//!    "what is scheduled" and "what is ripe" stay separate concerns.
//!  * [`ReminderScheduler::pop_ripe`] takes `now` **explicitly**, exactly like
//!    `datetime::parse_when` / `recur::next_after`. No hidden clock read sits
//!    anywhere in the ripeness decision, so tests drive time by argument and
//!    never sleep.
//!
//! **A reminder that ripened while the daemon was down still fires**, once, on
//! the next start (§9: "survives sleep by re-checking on wake"). That is what
//! makes dedupe load-bearing rather than decorative.
//!
//! **Dedupe / idempotency** (§9 "a fired reminder writes a `reminded` event so
//! it never double-fires"): the `reminded` event row *is* the dedupe record.
//! `rebuild` filters already-reminded (task, instant) pairs out of the heap in
//! one query, and [`fire_one`] re-checks inside its own transaction — so a
//! restart, a concurrent external writer, and a mid-tick rebuild all converge on
//! exactly one delivery per (task, instant). Keying on the *instant* and not just
//! the task is deliberate: moving `due` moves a relative reminder to a genuinely
//! new instant, which should fire again.

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

use jiff::Timestamp;
use serde_json::json;

use crate::engine::Engine;
use crate::error::ApiError;
use crate::notify::Notification;
use crate::remind;
use crate::storage::reminded_keys;

/// One scheduled reminder: the task it belongs to plus the instant it ripens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub task_id: String,
    pub short_id: i64,
    pub title: String,
    /// The task's `due` at rebuild time (carried for the notification body).
    pub due: Option<String>,
    /// The resolved instant this reminder fires at.
    pub at: Timestamp,
}

/// Ordered by fire instant, then `short_id`. The tie-break is not cosmetic: it
/// makes `pop_ripe` a *deterministic* sequence when several reminders share an
/// instant, which the tests rely on.
impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at.cmp(&other.at).then(self.short_id.cmp(&other.short_id))
    }
}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A min-heap of upcoming reminders. `BinaryHeap` is a max-heap, so entries are
/// wrapped in `Reverse` — the root is always the soonest reminder.
#[derive(Default)]
pub struct ReminderScheduler {
    heap: BinaryHeap<Reverse<Pending>>,
}

impl ReminderScheduler {
    /// An empty scheduler (nothing scheduled).
    pub fn new() -> Self {
        ReminderScheduler { heap: BinaryHeap::new() }
    }

    /// Rebuild the heap from the store: every live task carrying a `remind`,
    /// minus the (task, instant) pairs already reminded.
    ///
    /// Skipped, by design:
    ///  * `done` / `cancelled` tasks — a finished task has nothing to remind about;
    ///  * relative reminders on a task with no `due` — unanchored, so there is no
    ///    instant to schedule (see [`remind::resolve`]);
    ///  * unparseable specs — a bad row must never wedge the whole scheduler.
    ///
    /// Two queries total, regardless of task count.
    pub fn rebuild(engine: &Engine) -> Result<Self, ApiError> {
        let fired = reminded_keys(engine.conn())?;
        let conn = engine.conn();
        // "Live task" here is exactly `is_open()` — the doc comment above states
        // the rule as "skip done/cancelled", which is the same partition, so this
        // shares the predicate rather than keeping a fourth hand-typed copy.
        // Enum-derived, never caller text — see `Status::sql_in_list`.
        let open = crate::types::Status::sql_in_list(crate::types::Status::is_open);
        let mut stmt = conn.prepare(&format!(
            "SELECT id, short_id, title, due, remind FROM tasks \
             WHERE remind IS NOT NULL AND status IN ({open}) \
             ORDER BY short_id",
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;

        let mut heap = BinaryHeap::new();
        for row in rows {
            let (task_id, short_id, title, due, spec) = row?;
            // An unanchored or unparseable reminder is simply not scheduled.
            let Some(at) = remind::resolve(&spec, due.as_deref()) else {
                continue;
            };
            if fired.contains(&(task_id.clone(), at.to_string())) {
                continue; // already delivered — never fire twice (across restarts).
            }
            heap.push(Reverse(Pending { task_id, short_id, title, due, at }));
        }
        Ok(ReminderScheduler { heap })
    }

    /// How many reminders are scheduled.
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// The soonest scheduled instant, if any. The daemon uses this to decide how
    /// long it may sleep.
    pub fn peek_at(&self) -> Option<Timestamp> {
        self.heap.peek().map(|Reverse(p)| p.at)
    }

    /// Pop every reminder ripe at `now` (fire instant at or before `now`), in
    /// chronological order. `now` is explicit — this function never reads a clock.
    pub fn pop_ripe(&mut self, now: Timestamp) -> Vec<Pending> {
        let mut out = Vec::new();
        while let Some(Reverse(p)) = self.heap.peek() {
            if p.at > now {
                break; // heap is ordered: nothing behind this is ripe either.
            }
            let _ = p;
            let Reverse(p) = self.heap.pop().expect("peeked, so pop cannot fail");
            out.push(p);
        }
        out
    }
}

/// Fire one reminder: write its `reminded` event, transactionally and
/// idempotently. Returns whether it actually fired (`false` = already reminded,
/// so the caller must NOT notify).
///
/// Thin wrapper over the `reminder.fire` API method, so the daemon path and any
/// future one-shot path go through the identical seam.
pub fn fire_one(engine: &Engine, p: &Pending) -> Result<bool, ApiError> {
    let res = engine.reminder_fire(&json!({ "ref": p.short_id, "at": p.at.to_string() }))?;
    Ok(res.get("fired").and_then(serde_json::Value::as_bool).unwrap_or(false))
}

/// Build the user-facing notification for a ripe reminder.
pub fn notification_for(p: &Pending) -> Notification {
    Notification {
        short_id: p.short_id,
        title: p.title.clone(),
        body: match &p.due {
            Some(d) => format!("due {d}"),
            None => String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// A task with `due` + `remind`, added through the real API.
    fn add(engine: &Engine, title: &str, due: Option<&str>, remind: &str) -> i64 {
        let mut p = json!({ "title": title, "remind": remind });
        if let Some(d) = due {
            p["due"] = json!(d);
        }
        let r = engine.task_add(&p).unwrap();
        r.get("short_id").and_then(Value::as_i64).unwrap()
    }

    #[test]
    fn rebuild_resolves_relative_offsets_against_due() {
        let e = Engine::open_in_memory().unwrap();
        add(&e, "ship it", Some("2026-07-20T17:00:00Z"), "-1h");
        let s = ReminderScheduler::rebuild(&e).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.peek_at().unwrap(), ts("2026-07-20T16:00:00Z"));
    }

    #[test]
    fn heap_orders_soonest_first_regardless_of_insert_order() {
        let e = Engine::open_in_memory().unwrap();
        // Added latest-first; the heap must still surface the soonest.
        add(&e, "late", Some("2026-07-25T17:00:00Z"), "-1h");
        add(&e, "early", Some("2026-07-20T17:00:00Z"), "-1h");
        add(&e, "middle", Some("2026-07-22T17:00:00Z"), "-1h");
        let mut s = ReminderScheduler::rebuild(&e).unwrap();
        assert_eq!(s.peek_at().unwrap(), ts("2026-07-20T16:00:00Z"));
        let ripe = s.pop_ripe(ts("2026-09-01T00:00:00Z"));
        let titles: Vec<&str> = ripe.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, vec!["early", "middle", "late"], "chronological order");
    }

    #[test]
    fn pop_ripe_is_driven_entirely_by_the_injected_now() {
        let e = Engine::open_in_memory().unwrap();
        add(&e, "ship it", Some("2026-07-20T17:00:00Z"), "-1h"); // ripens 16:00Z
        let mut s = ReminderScheduler::rebuild(&e).unwrap();

        // A second early: not ripe.
        assert!(s.pop_ripe(ts("2026-07-20T15:59:59Z")).is_empty());
        assert_eq!(s.len(), 1, "a non-ripe reminder stays on the heap");

        // Exactly at the instant: ripe (boundary is inclusive).
        let ripe = s.pop_ripe(ts("2026-07-20T16:00:00Z"));
        assert_eq!(ripe.len(), 1);
        assert_eq!(ripe[0].title, "ship it");
        assert!(s.is_empty(), "a popped reminder leaves the heap");
    }

    #[test]
    fn unanchored_and_finished_tasks_are_not_scheduled() {
        let e = Engine::open_in_memory().unwrap();
        // Relative remind with no due -> nothing to anchor to.
        add(&e, "no due", None, "-1h");
        // Absolute remind, but the task gets completed -> nothing to remind about.
        let done = add(&e, "finished", Some("2026-07-20T17:00:00Z"), "2026-07-19T08:00:00Z");
        e.task_done(&json!({ "ref": done })).unwrap();
        assert_eq!(ReminderScheduler::rebuild(&e).unwrap().len(), 0);
    }

    /// The inclusion side of `rebuild`'s status filter. Every guard here was
    /// exclusion-only — `unanchored_and_finished_tasks_are_not_scheduled` seeds
    /// a `done` task, and the `add` helper always produces `pending` — so no
    /// test ever gave a `backlog` task a reminder. Narrowing the generated
    /// `IN (…)` list to drop `'backlog'` therefore left the whole suite green
    /// while every reminder on a waiting or scheduled task silently stopped
    /// firing, which is indistinguishable from never having set one.
    ///
    /// `backlog` is exactly the status where a reminder matters most: the task
    /// is parked until a future date, so the reminder is the only thing that
    /// will bring it back to your attention.
    #[test]
    fn a_backlog_task_still_gets_its_reminder_scheduled() {
        let e = Engine::open_in_memory().unwrap();
        // `wait` in the future parks the task in `backlog`.
        let r = e
            .task_add(&json!({
                "title": "parked but due later",
                "wait": "2999-01-01T00:00:00Z",
                "remind": "2026-07-19T08:00:00Z",
            }))
            .unwrap();
        assert_eq!(r["status"], "backlog", "fixture must actually be backlog");

        let s = ReminderScheduler::rebuild(&e).unwrap();
        assert_eq!(s.len(), 1, "a backlog task's reminder must still be scheduled");
        assert_eq!(s.peek_at().unwrap(), ts("2026-07-19T08:00:00Z"));
    }

    #[test]
    fn absolute_reminder_schedules_without_a_due() {
        let e = Engine::open_in_memory().unwrap();
        add(&e, "standalone", None, "2026-07-19T08:00:00Z");
        let s = ReminderScheduler::rebuild(&e).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.peek_at().unwrap(), ts("2026-07-19T08:00:00Z"));
    }

    #[test]
    fn fire_one_writes_a_reminded_event_and_is_idempotent() {
        let e = Engine::open_in_memory().unwrap();
        add(&e, "ship it", Some("2026-07-20T17:00:00Z"), "-1h");
        let mut s = ReminderScheduler::rebuild(&e).unwrap();
        let ripe = s.pop_ripe(ts("2026-07-20T16:00:00Z"));
        assert_eq!(ripe.len(), 1);

        assert!(fire_one(&e, &ripe[0]).unwrap(), "first fire delivers");
        // The event log is the dedupe record AND the headless verification surface.
        let evts = e.event_list(&json!({ "entity": "task" })).unwrap();
        let reminded: Vec<&Value> = evts["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v.get("op") == Some(&json!("reminded")))
            .collect();
        assert_eq!(reminded.len(), 1, "exactly one reminded event");

        // Firing the same (task, instant) again is a no-op — no second event.
        assert!(!fire_one(&e, &ripe[0]).unwrap(), "second fire is deduped");
        let evts = e.event_list(&json!({ "entity": "task" })).unwrap();
        let n = evts["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v.get("op") == Some(&json!("reminded")))
            .count();
        assert_eq!(n, 1, "dedupe must not append a second reminded event");
    }

    #[test]
    fn restart_does_not_refire_an_already_reminded_reminder() {
        let e = Engine::open_in_memory().unwrap();
        add(&e, "ship it", Some("2026-07-20T17:00:00Z"), "-1h");

        // First "run": ripe and fired.
        let mut s = ReminderScheduler::rebuild(&e).unwrap();
        let ripe = s.pop_ripe(ts("2026-07-20T16:00:00Z"));
        assert!(fire_one(&e, &ripe[0]).unwrap());

        // Restart: rebuild from the same store. The reminder is in the past and
        // still unfired-looking by timestamp alone — only the `reminded` event
        // keeps it off the heap.
        let s2 = ReminderScheduler::rebuild(&e).unwrap();
        assert!(s2.is_empty(), "a restart must not re-schedule a fired reminder");
    }

    #[test]
    fn a_reminder_missed_while_down_still_fires_once_on_restart() {
        let e = Engine::open_in_memory().unwrap();
        add(&e, "missed me", Some("2026-07-20T17:00:00Z"), "-1h");
        // Nothing fired it yet; the daemon starts long after the instant passed.
        let mut s = ReminderScheduler::rebuild(&e).unwrap();
        assert_eq!(s.len(), 1, "a past-due unfired reminder is still scheduled");
        let ripe = s.pop_ripe(ts("2026-08-01T00:00:00Z"));
        assert_eq!(ripe.len(), 1);
        assert!(fire_one(&e, &ripe[0]).unwrap(), "it fires once, late");
    }

    #[test]
    fn moving_due_moves_a_relative_reminder_to_a_new_instant_that_fires_again() {
        let e = Engine::open_in_memory().unwrap();
        let id = add(&e, "ship it", Some("2026-07-20T17:00:00Z"), "-1h");

        let mut s = ReminderScheduler::rebuild(&e).unwrap();
        let ripe = s.pop_ripe(ts("2026-07-20T16:00:00Z"));
        assert!(fire_one(&e, &ripe[0]).unwrap());

        // Push `due` out a day: the offset is symbolic, so the reminder follows
        // it to a genuinely new instant — which is a new reminder, not a re-fire.
        e.task_modify(&json!({ "ref": id, "set": { "due": "2026-07-21T17:00:00Z" } })).unwrap();
        let mut s2 = ReminderScheduler::rebuild(&e).unwrap();
        assert_eq!(s2.len(), 1, "the moved reminder is scheduled again");
        assert_eq!(s2.peek_at().unwrap(), ts("2026-07-21T16:00:00Z"));
        let ripe2 = s2.pop_ripe(ts("2026-07-21T16:00:00Z"));
        assert!(fire_one(&e, &ripe2[0]).unwrap(), "a new instant fires on its own merit");
    }

    #[test]
    fn notification_carries_short_id_title_and_due() {
        let e = Engine::open_in_memory().unwrap();
        let id = add(&e, "ship it", Some("2026-07-20T17:00:00Z"), "-1h");
        let mut s = ReminderScheduler::rebuild(&e).unwrap();
        let ripe = s.pop_ripe(ts("2026-07-20T16:00:00Z"));
        let n = notification_for(&ripe[0]);
        assert_eq!(n.short_id, id);
        assert_eq!(n.title, "ship it");
        assert_eq!(n.body, "due 2026-07-20T17:00:00Z");
    }
}
