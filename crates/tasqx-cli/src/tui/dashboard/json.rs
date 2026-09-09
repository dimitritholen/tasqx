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
//!
//! `order` decides which panel keys are present at all — `dashboard.panels`
//! narrowed by `--panels` — and RECENT/NEXT are additionally row-capped
//! (`ROW_CAP`) with `total`/`truncated` alongside them, because both scale
//! with the store rather than with anything a screen would show. Before this,
//! every panel was always present and RECENT returned literally every task
//! the store held (#152: 61 KB of a 116 KB document, on a store with no
//! unusual amount of data).

use serde_json::{json, Map, Value};

use crate::tokens::BUCKETS;

use super::model::{Dashboard, PanelId, Task};

/// Row ceiling for `recent` and `next` on the JSON path (#152).
///
/// These are the two panels whose row count tracks the STORE rather than the
/// screen: RECENT holds every task the store has (unfiltered by status on
/// purpose), and NEXT holds the whole open, unblocked working set. Both grow
/// with the store's age, and RECENT alone was measured at 52% of a 116 KB
/// document on a 142-task store with no way to ask for less. There is no
/// terminal here to size a "real" ceiling against — `layout` grows the
/// on-screen versions to fit whatever height it is given, with no upper bound
/// of its own (D62) — so `--json` gets a ceiling of its own instead: enough
/// rows to answer "what changed" or "what's next" without echoing the whole
/// store. `total` and `truncated` say when a row was left off, so a caller
/// that wants more knows to ask a narrower question (`--panels`, a filter, or
/// `tasqx list`) rather than assuming the list it got was complete.
const ROW_CAP: usize = 20;

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

/// A row list bounded at [`ROW_CAP`], paired with the count a client needs to
/// tell a short list from a cut one: how many rows there really were, and
/// whether any were left out.
fn capped(rows: &[Task]) -> (Value, usize, bool) {
    let shown = rows.len().min(ROW_CAP);
    (tasks(&rows[..shown]), rows.len(), rows.len() > ROW_CAP)
}

/// The whole screen, as data.
///
/// Only the panels named in `order` are populated — `dashboard.panels`, or a
/// narrower `--panels` on the CLI, both funnel through the same list here, and
/// it is a payload-size knob now as much as a layout one: a caller who asks
/// for `now,next,due` no longer pays for BLOCKED, RECENT, PROJECTS, BURNDOWN
/// and TOKENS it never named. `status` is the one exception — the header the
/// screen always draws above whichever panels fit under it, and cheap beside
/// any one of them.
pub fn document(d: &Dashboard, days: usize, order: &[PanelId]) -> Value {
    let want = |id: PanelId| order.contains(&id);
    let mut obj = Map::new();
    obj.insert("dashboard".to_string(), json!("panels"));
    obj.insert("today".to_string(), json!(d.today.to_string()));
    obj.insert("window_days".to_string(), json!(days));
    obj.insert(
        "panels".to_string(),
        json!(order.iter().map(|p| panel_name(*p)).collect::<Vec<_>>()),
    );
    obj.insert(
        "status".to_string(),
        json!({
            "project": d.status.project(),
            "open": d.status.open,
            "active": d.status.active,
            "overdue": d.status.overdue,
            "blocked": d.status.blocked,
            "done_week": d.status.done_week,
        }),
    );
    if want(PanelId::Now) {
        obj.insert(
            "now".to_string(),
            json!(d.now.as_ref().map(|n| json!({
                "task": task(&n.task),
                "elapsed_secs": n.elapsed_secs,
                // Tracked PLUS the interval still running — the number the
                // card shows, because `tracked` alone reads as the final
                // answer when it is only the total so far.
                "total_secs": n.total_secs(),
            }))),
        );
    }
    if want(PanelId::Next) {
        let (rows, total, truncated) = capped(&d.next.rows);
        obj.insert(
            "next".to_string(),
            json!({
                "max_urgency": d.next.max_urgency,
                "rows": rows,
                "total": total,
                "truncated": truncated,
            }),
        );
    }
    if want(PanelId::Due) {
        obj.insert(
            "due".to_string(),
            json!({
                "overdue": tasks(&d.due.overdue),
                "today": tasks(&d.due.today),
                "tomorrow": tasks(&d.due.tomorrow),
                "week": tasks(&d.due.week),
            }),
        );
    }
    if want(PanelId::Blocked) {
        obj.insert(
            "blocked".to_string(),
            json!({ "rows": tasks(&d.blocked.rows) }),
        );
    }
    if want(PanelId::Recent) {
        let (rows, total, truncated) = capped(&d.recent.rows);
        obj.insert(
            "recent".to_string(),
            json!({ "rows": rows, "total": total, "truncated": truncated }),
        );
    }
    if want(PanelId::Projects) {
        obj.insert(
            "projects".to_string(),
            json!({
                "rows": d.projects.rows.iter().map(|r| json!({
                    "name": r.name(),
                    "archived": r.archived,
                    "default": r.is_default,
                    "open": r.open,
                    "overdue": r.overdue,
                    "est_secs": r.est_secs,
                    "tracked_secs": r.tracked_secs,
                })).collect::<Vec<_>>(),
            }),
        );
    }
    if want(PanelId::Burndown) {
        obj.insert(
            "burndown".to_string(),
            json!({
                "days": d.burndown.days,
                // Says when the window may be clipped rather than presenting
                // a partial series as complete.
                "truncated": d.burndown.truncated,
                "series": d.burndown.series.iter().map(|p| json!({
                    "date": p.date.to_string(),
                    "remaining": p.remaining,
                })).collect::<Vec<_>>(),
            }),
        );
    }
    if want(PanelId::Tokens) {
        obj.insert(
            "tokens".to_string(),
            json!({
                // The four buckets, named by their `report.summary` metric
                // keys and in the fixed D48 order, so a reader can line the
                // arrays up with the API they came from. Never summed: D50
                // removed `tokens_total` because the buckets do not mean the
                // same thing.
                "buckets": BUCKETS.map(|(key, _, _)| key),
                "totals": d.tokens.totals,
                "rows": d.tokens.rows.iter().map(|r| json!({
                    "name": r.name(),
                    "buckets": r.buckets,
                    "total": r.total(),
                })).collect::<Vec<_>>(),
            }),
        );
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::super::model::{build, Sources};
    use super::*;

    fn now() -> jiff::Timestamp {
        "2026-08-05T12:00:00Z".parse().unwrap()
    }

    fn today() -> jiff::civil::Date {
        jiff::civil::date(2026, 8, 5)
    }

    /// The minimal task row `Task::from_json` needs: `id` and `modified` are
    /// the only fields it refuses to default, and `pending` keeps every row
    /// open, unblocked and off the backlog so it lands in both RECENT and
    /// NEXT UP.
    fn task_row(n: i64) -> Value {
        json!({
            "id": format!("019fd213-0000-7000-8000-{n:012}"),
            "short_id": n,
            "title": format!("task {n}"),
            "status": "pending",
            "modified": "2026-08-01T09:00:00Z",
            "urgency": 1.0,
        })
    }

    /// A `Dashboard` fixture over `n` open tasks and nothing else — enough to
    /// drive RECENT and NEXT UP, which is all these tests examine.
    fn dashboard_with(n: i64) -> Dashboard {
        let rows: Vec<Value> = (1..=n).map(task_row).collect();
        let tasks = json!({ "count": rows.len(), "tasks": rows });
        let summary = json!({ "generated": "2026-08-05T12:00:00Z", "groups": [] });
        let projects = json!({ "count": 0, "projects": [] });
        let events = json!({ "count": 0, "events": [] });
        build(
            Sources {
                tasks: &tasks,
                summary: &summary,
                projects: &projects,
                events: &events,
                event_limit: 100,
                days: 7,
            },
            now(),
            today(),
        )
    }

    fn all_panels() -> Vec<PanelId> {
        vec![
            PanelId::Now,
            PanelId::Next,
            PanelId::Due,
            PanelId::Blocked,
            PanelId::Recent,
            PanelId::Projects,
            PanelId::Burndown,
            PanelId::Tokens,
        ]
    }

    /// #152: `recent` used to return every task the store had, with no count
    /// and no way to tell a short list from a cut one. RECENT and NEXT UP both
    /// scale with the store, so both are checked here on a store bigger than
    /// [`ROW_CAP`].
    #[test]
    fn recent_and_next_are_capped_with_total_and_truncated() {
        let over_cap = ROW_CAP as i64 + 10;
        let dash = dashboard_with(over_cap);
        let doc = document(&dash, 7, &all_panels());

        let recent = &doc["recent"];
        assert_eq!(
            recent["rows"]
                .as_array()
                .expect("recent.rows is an array")
                .len(),
            ROW_CAP,
            "recent.rows must stop at ROW_CAP, not grow with the store"
        );
        assert_eq!(recent["total"], json!(over_cap));
        assert_eq!(recent["truncated"], json!(true));

        let next = &doc["next"];
        assert_eq!(
            next["rows"]
                .as_array()
                .expect("next.rows is an array")
                .len(),
            ROW_CAP
        );
        assert_eq!(next["total"], json!(over_cap));
        assert_eq!(next["truncated"], json!(true));
    }

    /// The flip side: a store under the cap is not miscalled truncated, and
    /// nothing is silently dropped from a short list.
    #[test]
    fn recent_and_next_are_whole_and_untruncated_under_the_cap() {
        let under_cap = ROW_CAP as i64 - 5;
        let dash = dashboard_with(under_cap);
        let doc = document(&dash, 7, &all_panels());

        assert_eq!(
            doc["recent"]["rows"].as_array().unwrap().len(),
            under_cap as usize
        );
        assert_eq!(doc["recent"]["total"], json!(under_cap));
        assert_eq!(doc["recent"]["truncated"], json!(false));
    }

    /// #152's other half: `--panels`/`dashboard.panels` used to only rename
    /// the `panels` array — every panel's body was still built and emitted no
    /// matter what `order` said. A panel left out of `order` must not appear
    /// in the document at all, and the ones named must still be there.
    #[test]
    fn only_the_named_panels_appear_in_the_document() {
        let dash = dashboard_with(3);
        let order = vec![PanelId::Now, PanelId::Next, PanelId::Due];
        let doc = document(&dash, 7, &order);
        let obj = doc.as_object().expect("document is an object");

        for missing in ["blocked", "recent", "projects", "burndown", "tokens"] {
            assert!(
                !obj.contains_key(missing),
                "`{missing}` was not requested and must be absent, not merely empty"
            );
        }
        for present in [
            "dashboard",
            "today",
            "window_days",
            "panels",
            "status",
            "now",
            "next",
            "due",
        ] {
            assert!(obj.contains_key(present), "`{present}` must be present");
        }
    }
}
