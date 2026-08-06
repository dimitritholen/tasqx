//! The dashboard as data — what `tasqx --json dashboard` answers with.
//!
//! Here rather than in `lib.rs` because it needs the model's private accessors
//! (`Task::title`, `StatusBar::project`, `ProjectRow::name`, `TokenRow::name`),
//! which are private for D19's reason: the strings are sanitised at
//! construction and there is no way to build one that is not.
//!
//! Hand-written rather than `#[derive(Serialize)]`, and that is the point. A
//! derive would publish whatever the model happens to hold — including fields
//! added later for the screen's convenience — and would spell them however Rust
//! does. This document is a surface a script reads: it names the four token
//! buckets by their `report.summary` metric keys, keeps every bucket array an
//! array so nothing reads as the grand total D50 removed, and answers with the
//! store's own spelling of a priority (`"H"`, not `"high"`).
//!
//! It is deliberately NOT frozen by the conformance suite, which covers the
//! JSON API's methods (D56). This is a CLI view over four of them.

use serde_json::{json, Value};

use crate::tokens::BUCKETS;

use super::model::{Dashboard, PanelId, Task};

/// The panel a `PanelId` names, in the document's vocabulary.
///
/// One table, on the enum: this used to be a second copy, and a second copy of
/// a name list is how `dashboard.panels` and this document would come to
/// disagree about what a panel is called. `Slot` keeps its spelling here
/// because the document has shipped with it.
fn panel_name(id: PanelId) -> &'static str {
    id.slug().unwrap_or("slot")
}

/// One task row. The model guarantees there is exactly one row type, so every
/// list panel below spells a task the same way.
fn task(t: &Task) -> Value {
    json!({
        "short_id": t.short_id,
        "id": t.id,
        "title": t.title(),
        "project": t.project(),
        // The store's spelling, not a prettier one: a script that filters on
        // this must be able to compare it with what `task.list` returns.
        "priority": t.priority.map(|p| p.as_str()),
        "urgency": t.urgency,
        "status": t.status.as_str(),
        "blocked": t.blocked,
        "due": t.due.map(|d| d.to_string()),
        "completed": t.completed.map(|d| d.to_string()),
        "modified": t.modified.to_string(),
        "active_since": t.active_since.map(|d| d.to_string()),
        "estimate_secs": t.estimate_secs,
        "tracked_secs": t.tracked_secs,
    })
}

fn tasks(rows: &[Task]) -> Value {
    Value::Array(rows.iter().map(task).collect())
}

/// The whole screen, as data.
pub fn document(d: &Dashboard, days: usize, order: &[PanelId]) -> Value {
    json!({
        "dashboard": "panels",
        "today": d.today.to_string(),
        "window_days": days,
        "panels": order.iter().map(|p| panel_name(*p)).collect::<Vec<_>>(),
        "status": {
            "project": d.status.project(),
            "open": d.status.open,
            "active": d.status.active,
            "overdue": d.status.overdue,
            "blocked": d.status.blocked,
            "done_week": d.status.done_week,
        },
        "now": d.now.as_ref().map(|n| json!({
            "task": task(&n.task),
            "elapsed_secs": n.elapsed_secs,
            // Tracked PLUS the interval still running — the number the card
            // shows, because `tracked` alone reads as the final answer when it
            // is only the total so far.
            "total_secs": n.total_secs(),
        })),
        "next": {
            "max_urgency": d.next.max_urgency,
            "rows": tasks(&d.next.rows),
        },
        "due": {
            "overdue": tasks(&d.due.overdue),
            "today": tasks(&d.due.today),
            "tomorrow": tasks(&d.due.tomorrow),
            "week": tasks(&d.due.week),
        },
        "blocked": { "rows": tasks(&d.blocked.rows) },
        "recent": { "rows": tasks(&d.recent.rows) },
        "projects": {
            "rows": d.projects.rows.iter().map(|r| json!({
                "name": r.name(),
                "archived": r.archived,
                "default": r.is_default,
                "open": r.open,
                "overdue": r.overdue,
                "est_secs": r.est_secs,
                "tracked_secs": r.tracked_secs,
            })).collect::<Vec<_>>(),
        },
        "burndown": {
            "days": d.burndown.days,
            // Says when the window may be clipped rather than presenting a
            // partial series as complete.
            "truncated": d.burndown.truncated,
            "series": d.burndown.series.iter().map(|p| json!({
                "date": p.date.to_string(),
                "remaining": p.remaining,
            })).collect::<Vec<_>>(),
        },
        "tokens": {
            // The four buckets, named by their `report.summary` metric keys and
            // in the fixed D48 order, so a reader can line the arrays up with
            // the API they came from. Never summed: D50 removed `tokens_total`
            // because the buckets do not mean the same thing.
            "buckets": BUCKETS.map(|(key, _, _)| key),
            "totals": d.tokens.totals,
            "rows": d.tokens.rows.iter().map(|r| json!({
                "name": r.name(),
                "buckets": r.buckets,
                "total": r.total(),
            })).collect::<Vec<_>>(),
        },
    })
}
