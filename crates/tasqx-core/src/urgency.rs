//! Urgency scoring (DESIGN.md §12-D1: one fixed, well-chosen formula).
//!
//! MVP scope ships a single opinionated function: priority weight + due
//! proximity + a small age term. Weights are intentionally not configurable
//! yet — that's an additive, non-breaking Later change. The score is rounded to
//! one decimal so display and stored value agree.

use crate::types::Priority;
use crate::util::{now, parse_ts};

/// Days between two instants, or None if either is unparseable.
fn days_until(target: &str) -> Option<f64> {
    let (t, n) = (parse_ts(target)?, parse_ts(&now())?);
    Some((t.as_second() - n.as_second()) as f64 / 86_400.0)
}

/// The named contributions to a task's urgency (DESIGN §12-D1: `tasqx why`
/// always exposes the breakdown). The total is their sum, rounded to one
/// decimal — identical to what [`score`] returns.
pub fn breakdown(priority: Option<Priority>, due: Option<&str>, created: &str) -> Vec<(&'static str, f64)> {
    let mut parts: Vec<(&'static str, f64)> = Vec::new();

    let prio = match priority {
        Some(Priority::H) => 6.0,
        Some(Priority::M) => 3.9,
        Some(Priority::L) => 1.8,
        None => 0.0,
    };
    parts.push(("priority", prio));

    let mut due_term = 0.0;
    if let Some(due) = due {
        if let Some(d) = days_until(due) {
            due_term = if d <= 0.0 { 12.0 } else { (12.0 * (1.0 - d / 14.0)).max(0.0) };
        }
    }
    parts.push(("due_proximity", due_term));

    let mut age_term = 0.0;
    if let Some(age) = days_until(created) {
        let age_days = (-age).max(0.0); // created is in the past => negative
        age_term = (age_days * 0.01).min(1.0);
    }
    parts.push(("age", age_term));

    parts
}

/// Compute a task's urgency from the fields that feed it.
pub fn score(priority: Option<Priority>, due: Option<&str>, created: &str) -> f64 {
    let u: f64 = breakdown(priority, due, created).iter().map(|(_, v)| v).sum();
    (u * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    /// An RFC3339 instant `days` away from the real clock.
    ///
    /// This module reads `util::now()` internally and takes no reference instant,
    /// so tests must anchor to the same wall clock it does. Every assertion below
    /// is written to tolerate the sub-second drift between the two clock reads —
    /// which is also why the exact-value cases all use inputs that the formula
    /// clamps, making them drift-proof by construction.
    fn days_from_now(days: f64) -> String {
        let secs = Timestamp::now().as_second() + (days * 86_400.0) as i64;
        Timestamp::from_second(secs).unwrap().to_string()
    }

    fn term(parts: &[(&'static str, f64)], name: &str) -> f64 {
        parts.iter().find(|(k, _)| *k == name).unwrap_or_else(|| panic!("no {name} term")).1
    }

    fn close(got: f64, want: f64, tol: f64) {
        assert!((got - want).abs() <= tol, "expected {want} +/- {tol}, got {got}");
    }

    /// `tasqx why` renders these three names and their values straight out of
    /// `breakdown`. Nothing asserted its shape, so returning an empty vec — or
    /// renaming a term — was a change the suite accepted in silence, and `why`
    /// would simply stop explaining the score printed next to it.
    #[test]
    fn breakdown_names_exactly_the_three_documented_terms() {
        let parts = breakdown(Some(Priority::H), None, &days_from_now(-1.0));
        let names: Vec<&str> = parts.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, ["priority", "due_proximity", "age"]);
    }

    /// The priority weights are the largest single input to the default ordering
    /// of `tasqx list`. They were bare literals with nothing behind them, so a
    /// slipped digit silently reorders every user's list — a wrong answer that
    /// still looks exactly like a right one.
    #[test]
    fn priority_weights_are_pinned() {
        let created = days_from_now(-1.0);
        for (prio, want) in [
            (Some(Priority::H), 6.0),
            (Some(Priority::M), 3.9),
            (Some(Priority::L), 1.8),
            (None, 0.0),
        ] {
            close(term(&breakdown(prio, None, &created), "priority"), want, 1e-9);
        }
    }

    /// Overdue work must saturate the due term rather than ramping past it, and a
    /// due date beyond the 14-day horizon must contribute nothing. Both ends were
    /// unasserted: flipping the `<=` that separates them, or the sign inside the
    /// ramp, left the suite green while inverting which tasks sort to the top.
    #[test]
    fn due_proximity_saturates_when_overdue_and_vanishes_beyond_the_horizon() {
        let created = days_from_now(-1.0);
        let due_term = |due: Option<&str>| term(&breakdown(None, due, &created), "due_proximity");

        close(due_term(Some(&days_from_now(-3.0))), 12.0, 1e-9);
        close(due_term(Some(&days_from_now(400.0))), 0.0, 1e-9);
        // No due at all is a different question from a distant due, but scores
        // the same — pinned so the two cannot drift apart.
        close(due_term(None), 0.0, 1e-9);
    }

    /// The ramp between "overdue" and "past the horizon" is linear in days, and
    /// only a mid-horizon due exercises it: at both ends the expression is
    /// clamped, so turning the `/ 14.0` into `* 14.0` — or the `12.0 *` into
    /// `12.0 +` — still produced the clamped value and survived everything else
    /// asserted here. The visible effect of getting this wrong is that due-soon
    /// tasks stop out-ranking due-later ones.
    #[test]
    fn due_proximity_ramps_linearly_across_the_fourteen_day_horizon() {
        let created = days_from_now(-1.0);
        let due_term = |days: f64| term(&breakdown(None, Some(&days_from_now(days)), &created), "due_proximity");

        close(due_term(7.0), 6.0, 0.02); // halfway across => half the weight
        close(due_term(3.5), 9.0, 0.02);
        assert!(due_term(2.0) > due_term(9.0), "a sooner due must rank higher");
    }

    /// Age accrues 0.01/day and caps at 1.0. `created` is in the past, so the
    /// code negates a negative difference — dropping that negation clamps every
    /// task's age term to zero, quietly deleting the tie-breaker that stops old
    /// untouched tasks from sinking forever.
    #[test]
    fn age_accrues_a_hundredth_per_day_and_caps_at_one() {
        let age_term = |days: f64| term(&breakdown(None, None, &days_from_now(days)), "age");

        close(age_term(-10.0), 0.10, 5e-3);
        close(age_term(-50.0), 0.50, 5e-3);
        // Capped: 500 days would score 5.0 without the `.min(1.0)`.
        close(age_term(-500.0), 1.0, 1e-9);
        // A task created in the future must not earn negative age.
        close(age_term(5.0), 0.0, 1e-9);
    }

    /// `score` is documented as the sum of `breakdown`, rounded to one decimal so
    /// the stored value and the rendered one agree. Nothing held the two
    /// together: `score` could return a constant and every test still passed,
    /// because callers only ever stored whatever came back and compared it to
    /// nothing. Expected totals are written out as literals rather than recomputed
    /// from `breakdown`, so this cannot degrade into restating the implementation.
    #[test]
    fn score_is_the_breakdown_summed_and_rounded_to_one_decimal() {
        // `created` 500 days back pins the age term at exactly its 1.0 cap, which
        // makes these totals independent of when the test runs.
        let old = days_from_now(-500.0);
        let overdue = days_from_now(-3.0);

        close(score(Some(Priority::H), None, &old), 7.0, 1e-9); // 6.0 + 0.0 + 1.0
        close(score(Some(Priority::M), Some(&overdue), &old), 16.9, 1e-9); // 3.9 + 12.0 + 1.0
        close(score(Some(Priority::L), Some(&overdue), &old), 14.8, 1e-9); // 1.8 + 12.0 + 1.0
        close(score(None, None, &old), 1.0, 1e-9); // 0.0 + 0.0 + 1.0
    }
}
