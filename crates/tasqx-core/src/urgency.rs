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
