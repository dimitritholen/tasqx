//! Urgency scoring (DESIGN.md §12-D1: one fixed, well-chosen formula).
//!
//! MVP scope ships a single opinionated function: priority weight + due
//! proximity + a small age term. Weights are intentionally not configurable
//! yet — that's an additive, non-breaking Later change. The score is rounded to
//! one decimal so display and stored value agree.
//!
//! `now` is a parameter of the `*_at` pair, never the system clock — the rule
//! every other time-reading module here states (`scheduler`, `remind`,
//! `recur`), adopted late: this module used to read the wall clock through
//! `days_until` twice per breakdown, so scoring N tasks sampled 2N slightly
//! different instants and every exact test needed a drift epsilon. The
//! parameterless [`score`]/[`breakdown`] pair stays as the public API's stable
//! names and reads the clock ONCE, then delegates.

use jiff::Timestamp;

use crate::types::Priority;
use crate::util::parse_ts;

/// Days from `now` until `target`, or None if `target` is unparseable.
///
/// Seconds-to-f64 is exact for any instant this tool will ever see (f64
/// carries integers to 2^53; the i64 second range that survives `parse_ts` is
/// far inside it), so the lint's precision warning names a loss that cannot
/// occur here.
#[allow(clippy::cast_precision_loss)]
fn days_until(target: &str, now: Timestamp) -> Option<f64> {
    let t = parse_ts(target)?;
    Some((t.as_second() - now.as_second()) as f64 / 86_400.0)
}

/// The named contributions to a task's urgency (DESIGN §12-D1: `tasqx why`
/// always exposes the breakdown), measured at `now`. The total is their sum,
/// rounded to one decimal — identical to what [`score_at`] returns.
#[must_use]
pub fn breakdown_at(
    priority: Option<Priority>,
    due: Option<&str>,
    created: &str,
    now: Timestamp,
) -> Vec<(&'static str, f64)> {
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
        if let Some(d) = days_until(due, now) {
            due_term = if d <= 0.0 {
                12.0
            } else {
                (12.0 * (1.0 - d / 14.0)).max(0.0)
            };
        }
    }
    parts.push(("due_proximity", due_term));

    let mut age_term = 0.0;
    if let Some(age) = days_until(created, now) {
        let age_days = (-age).max(0.0); // created is in the past => negative
        age_term = (age_days * 0.01).min(1.0);
    }
    parts.push(("age", age_term));

    parts
}

/// Compute a task's urgency from the fields that feed it, measured at `now`.
#[must_use]
pub fn score_at(
    priority: Option<Priority>,
    due: Option<&str>,
    created: &str,
    now: Timestamp,
) -> f64 {
    let u: f64 = breakdown_at(priority, due, created, now)
        .iter()
        .map(|(_, v)| v)
        .sum();
    (u * 10.0).round() / 10.0
}

/// [`breakdown_at`] measured now — ONE clock read for all three terms.
#[must_use]
pub fn breakdown(
    priority: Option<Priority>,
    due: Option<&str>,
    created: &str,
) -> Vec<(&'static str, f64)> {
    breakdown_at(priority, due, created, Timestamp::now())
}

/// [`score_at`] measured now — ONE clock read for all three terms.
#[must_use]
pub fn score(priority: Option<Priority>, due: Option<&str>, created: &str) -> f64 {
    score_at(priority, due, created, Timestamp::now())
}

#[cfg(test)]
mod tests {
    // Exact f64 equality IS the property under test here: with `now` injected
    // the formula is pure, and the clamped/half-ramp fixtures below are chosen
    // to be bit-exact, so an epsilon would only hide a formula change.
    #![allow(clippy::float_cmp)]

    use super::*;

    /// An RFC3339 instant `days` away from the real clock.
    ///
    /// Only the wrapper pair ([`score`]/[`breakdown`]) still anchors to the
    /// wall clock, so only its tests need this; every formula property is
    /// pinned exactly against a fixed `now` below.
    fn days_from_now(days: i64) -> String {
        let secs = Timestamp::now().as_second() + days * 86_400;
        Timestamp::from_second(secs).unwrap().to_string()
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn term(parts: &[(&'static str, f64)], name: &str) -> f64 {
        parts
            .iter()
            .find(|(k, _)| *k == name)
            .unwrap_or_else(|| panic!("no {name} term"))
            .1
    }

    fn close(got: f64, want: f64, tol: f64) {
        assert!(
            (got - want).abs() <= tol,
            "expected {want} +/- {tol}, got {got}"
        );
    }

    /// With `now` injected the formula is a pure function, so the mid-ramp
    /// values that needed a 0.02 drift epsilon while the module read the wall
    /// clock are exact equalities here: due in 7 of 14 days is HALF the weight,
    /// bit-for-bit, and 500 days of age is the 1.0 cap. This is the test the
    /// injection exists to make writable.
    #[test]
    fn the_formula_is_exact_at_a_fixed_instant() {
        let now = at("2026-08-31T12:00:00Z");
        let parts = breakdown_at(
            Some(Priority::H),
            Some("2026-09-07T12:00:00Z"), // exactly 7 days out: 12 * (1 - 7/14)
            "2025-04-18T12:00:00Z",       // 500 days back: capped age
            now,
        );
        assert_eq!(term(&parts, "priority"), 6.0);
        assert_eq!(term(&parts, "due_proximity"), 6.0);
        assert_eq!(term(&parts, "age"), 1.0);
        assert_eq!(
            score_at(
                Some(Priority::H),
                Some("2026-09-07T12:00:00Z"),
                "2025-04-18T12:00:00Z",
                now,
            ),
            13.0
        );
    }

    /// `tasqx why` renders these three names and their values straight out of
    /// `breakdown`. Nothing asserted its shape, so returning an empty vec — or
    /// renaming a term — was a change the suite accepted in silence, and `why`
    /// would simply stop explaining the score printed next to it.
    #[test]
    fn breakdown_names_exactly_the_three_documented_terms() {
        let parts = breakdown(Some(Priority::H), None, &days_from_now(-1));
        let names: Vec<&str> = parts.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, ["priority", "due_proximity", "age"]);
    }

    /// The priority weights are the largest single input to the default ordering
    /// of `tasqx list`. They were bare literals with nothing behind them, so a
    /// slipped digit silently reorders every user's list — a wrong answer that
    /// still looks exactly like a right one.
    #[test]
    fn priority_weights_are_pinned() {
        let now = at("2026-08-31T12:00:00Z");
        let created = "2026-08-30T12:00:00Z";
        for (prio, want) in [
            (Some(Priority::H), 6.0),
            (Some(Priority::M), 3.9),
            (Some(Priority::L), 1.8),
            (None, 0.0),
        ] {
            assert_eq!(
                term(&breakdown_at(prio, None, created, now), "priority"),
                want
            );
        }
    }

    /// Overdue work must saturate the due term rather than ramping past it, and a
    /// due date beyond the 14-day horizon must contribute nothing. Both ends were
    /// unasserted: flipping the `<=` that separates them, or the sign inside the
    /// ramp, left the suite green while inverting which tasks sort to the top.
    #[test]
    fn due_proximity_saturates_when_overdue_and_vanishes_beyond_the_horizon() {
        let now = at("2026-08-31T12:00:00Z");
        let created = "2026-08-30T12:00:00Z";
        let due_term =
            |due: Option<&str>| term(&breakdown_at(None, due, created, now), "due_proximity");

        assert_eq!(due_term(Some("2026-08-28T12:00:00Z")), 12.0); // 3 days overdue
        assert_eq!(due_term(Some("2027-10-05T12:00:00Z")), 0.0); // 400 days out
                                                                 // No due at all is a different question from a distant due, but scores
                                                                 // the same — pinned so the two cannot drift apart.
        assert_eq!(due_term(None), 0.0);
    }

    /// The ramp between "overdue" and "past the horizon" is linear in days, and
    /// only a mid-horizon due exercises it: at both ends the expression is
    /// clamped, so turning the `/ 14.0` into `* 14.0` — or the `12.0 *` into
    /// `12.0 +` — still produced the clamped value and survived everything else
    /// asserted here. The visible effect of getting this wrong is that due-soon
    /// tasks stop out-ranking due-later ones.
    #[test]
    fn due_proximity_ramps_linearly_across_the_fourteen_day_horizon() {
        let now = at("2026-08-31T12:00:00Z");
        let created = "2026-08-30T12:00:00Z";
        let due_term = |due: &str| {
            term(
                &breakdown_at(None, Some(due), created, now),
                "due_proximity",
            )
        };

        assert_eq!(due_term("2026-09-07T12:00:00Z"), 6.0); // halfway => half the weight
        assert_eq!(due_term("2026-09-04T00:00:00Z"), 9.0); // 3.5 of 14 days
        assert!(
            due_term("2026-09-02T12:00:00Z") > due_term("2026-09-09T12:00:00Z"),
            "a sooner due must rank higher"
        );
    }

    /// Age accrues 0.01/day and caps at 1.0. `created` is in the past, so the
    /// code negates a negative difference — dropping that negation clamps every
    /// task's age term to zero, quietly deleting the tie-breaker that stops old
    /// untouched tasks from sinking forever.
    #[test]
    fn age_accrues_a_hundredth_per_day_and_caps_at_one() {
        let now = at("2026-08-31T12:00:00Z");
        let age_term = |created: &str| term(&breakdown_at(None, None, created, now), "age");

        close(age_term("2026-08-21T12:00:00Z"), 0.10, 1e-12); // 10 days
        close(age_term("2026-07-12T12:00:00Z"), 0.50, 1e-12); // 50 days
                                                              // Capped: 500 days would score 5.0 without the `.min(1.0)`.
        assert_eq!(age_term("2025-04-18T12:00:00Z"), 1.0);
        // A task created in the future must not earn negative age.
        assert_eq!(age_term("2026-09-05T12:00:00Z"), 0.0);
    }

    /// `score` is documented as the sum of `breakdown`, rounded to one decimal so
    /// the stored value and the rendered one agree. Nothing held the two
    /// together: `score` could return a constant and every test still passed,
    /// because callers only ever stored whatever came back and compared it to
    /// nothing. Expected totals are written out as literals rather than recomputed
    /// from `breakdown`, so this cannot degrade into restating the implementation.
    #[test]
    fn score_is_the_breakdown_summed_and_rounded_to_one_decimal() {
        let now = at("2026-08-31T12:00:00Z");
        let old = "2025-04-18T12:00:00Z"; // 500 days back: age exactly at its cap
        let overdue = "2026-08-28T12:00:00Z";

        assert_eq!(score_at(Some(Priority::H), None, old, now), 7.0); // 6.0 + 0.0 + 1.0
        assert_eq!(score_at(Some(Priority::M), Some(overdue), old, now), 16.9); // 3.9 + 12.0 + 1.0
        assert_eq!(score_at(Some(Priority::L), Some(overdue), old, now), 14.8); // 1.8 + 12.0 + 1.0
        assert_eq!(score_at(None, None, old, now), 1.0); // 0.0 + 0.0 + 1.0
    }

    /// The stable names are one clock read and a delegation — asserted loosely
    /// (the wall clock moved between this test's read and theirs), which is
    /// exactly the imprecision the `*_at` pair exists to confine to this one
    /// test.
    #[test]
    fn the_wrappers_agree_with_the_at_pair_measured_now() {
        let created = days_from_now(-500); // capped age: drift-proof
        let now = Timestamp::now();
        assert_eq!(
            score(Some(Priority::H), None, &created),
            score_at(Some(Priority::H), None, &created, now)
        );
        assert_eq!(
            breakdown(None, None, &created),
            breakdown_at(None, None, &created, now)
        );
    }
}
