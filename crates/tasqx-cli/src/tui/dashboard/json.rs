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

/// The panels this document writes a payload for — four of the screen's six.
///
/// `pulse` and `effort` have no `want(…)` block in [`document`]: both are
/// derived in the screen's own model for drawing, and neither has ever been
/// serialised. So `--panels pulse,effort` is a valid request that answers with
/// the envelope and nothing else.
///
/// **What this guarantees**: asked for a set of panels, [`document`] writes a
/// key for each requested panel named here and for no other panel — and it
/// writes no non-envelope key that is not a panel at all. That is asserted
/// over more than one window, because the guarantee is unconditional and a
/// single-fixture test cannot see a key emitted only for some `days`.
///
/// Public because prose counts these. The guide's `--json` sentence and the
/// wiki's dashboard page both state how many panels the document carries, and
/// that number is NOT `PANEL_NAMES.len()` — a sentence pinned to the screen's
/// roster was wrong in a way that looked derived.
pub const PAYLOAD_PANELS: [&str; 4] = ["tasks", "projects", "burndown", "tokens"];

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
    if want(PanelId::Tasks) {
        // One key where there were five (D80). `now`, `next`, `due`, `blocked`
        // and `recent` described five panels over the same rows; a consumer
        // that wanted "the open work" had to union four of them and dedupe.
        // The facts they carried are on the row instead — `active_since` is
        // what NOW selected, `blocked` is what BLOCKED selected, `due` is what
        // DUE bucketed by, `modified` is what RECENT sorted on — so nothing
        // moved out of reach, and the grouping and order the screen is showing
        // are stated rather than inferred.
        let t = &d.tasks;
        let groups: Vec<Value> = t
            .groups
            .iter()
            .map(|g| {
                let (rows, total, truncated) = capped(&g.rows);
                json!({
                    "project": g.project(),
                    "open": g.rows.len(),
                    "overdue": g.overdue,
                    "rows": rows,
                    "total": total,
                    "truncated": truncated,
                })
            })
            .collect();
        obj.insert(
            "tasks".to_string(),
            json!({
                "sort": t.sort.label(),
                "total": t.total,
                "groups": groups,
            }),
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
                    // #217 (D50): the group's WORST measurement confidence, or
                    // absent when `report.summary` sent none. Every bucket
                    // above is already a blend across measurements, so this
                    // is the one field that answers "how sure are we" at all.
                    "confidence": r.confidence(),
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

    /// Every panel, derived from the roster rather than retyped.
    ///
    /// A hand-written vec here was invisible to the very drift these tests
    /// exist to catch: a `want(…)` block for a panel this list happened to
    /// omit would never be exercised, so its payload key could never be seen.
    fn all_panels() -> Vec<PanelId> {
        crate::tui::dashboard::model::PANEL_NAMES
            .iter()
            .map(|n| {
                PanelId::from_slug(n)
                    .unwrap_or_else(|| panic!("`{n}` is in PANEL_NAMES but resolves to no PanelId"))
            })
            .collect()
    }

    /// Windows worth exercising: the default, and one that is not.
    ///
    /// Both payload tests ran only at `days = 7`, so a key written under
    /// `if days != 7` shipped with all 888 lib tests green — every non-default
    /// window carrying an undeclared payload nobody could see.
    const WINDOWS: [usize; 3] = [7, 1, 30];

    /// The keys [`document`] writes whatever was asked for.
    ///
    /// Hoisted so the two tests below share ONE definition of "a panel
    /// payload": anything that is not envelope. Spelled per-test, the narrower
    /// one asked only whether the four DECLARED names were absent, and a drift
    /// that wrote an undeclared `pulse` key — the exact drift the pair exists
    /// to catch — went straight through it.
    const ENVELOPE: [&str; 5] = ["dashboard", "today", "window_days", "panels", "status"];

    /// Every non-envelope key the document carries, sorted.
    fn payload_keys(doc: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = doc
            .as_object()
            .expect("the document is an object")
            .keys()
            .map(String::as_str)
            .filter(|k| !ENVELOPE.contains(k))
            .collect();
        keys.sort_unstable();
        keys
    }

    /// [`PAYLOAD_PANELS`] must be exactly the panel keys [`document`] writes.
    ///
    /// Asked for all six panels, the document carries four. The guide and the
    /// wiki both state that count, and both used to state the screen's
    /// instead — a number that read as derived because it was, from the wrong
    /// table. This binds the declared list to the emitted one, so adding a
    /// `want(…)` block without naming it here reddens.
    #[test]
    fn the_payload_panels_are_the_keys_the_document_writes() {
        let mut declared = PAYLOAD_PANELS.to_vec();
        declared.sort_unstable();

        for days in WINDOWS {
            let doc = document(&dashboard_with(3), days, &all_panels());
            let written = payload_keys(&doc);
            assert_eq!(
                written, declared,
                "at days={days} `document` writes {written:?} but \
                 PAYLOAD_PANELS declares {declared:?} — the prose that counts \
                 these is generated from the const, so the two may not disagree"
            );
        }
    }

    /// A panel with no payload block is still a legal request, and answers
    /// with the envelope alone.
    ///
    /// Verified against the binary: `tasqx --json dashboard --panels
    /// pulse,effort` exits 0 and carries no panel key. The wiki and the guide
    /// both say so now, so it is worth a test rather than a memory.
    #[test]
    fn asking_only_for_screen_only_panels_answers_no_payload() {
        for days in WINDOWS {
            let doc = document(&dashboard_with(3), days, &[PanelId::Pulse, PanelId::Effort]);
            let written = payload_keys(&doc);
            assert!(
                written.is_empty(),
                "at days={days} `--panels pulse,effort` wrote {written:?}; \
                 neither panel has a payload block, so the answer is the \
                 envelope and nothing else"
            );
            assert_eq!(
                doc["panels"],
                json!(["pulse", "effort"]),
                "the panels array must still name what was asked for"
            );
        }
    }

    /// #152: `recent` used to return every task the store had, with no count
    /// and no way to tell a short list from a cut one. The list still scales
    /// with the store, so the cap is still checked on a store bigger than
    /// [`ROW_CAP`] — per GROUP now, since that is where the rows live.
    #[test]
    fn the_task_rows_are_capped_with_total_and_truncated() {
        let over_cap = ROW_CAP as i64 + 10;
        let dash = dashboard_with(over_cap);
        let doc = document(&dash, 7, &all_panels());

        let groups = doc["tasks"]["groups"]
            .as_array()
            .expect("tasks.groups is an array");
        let biggest = groups
            .iter()
            .max_by_key(|g| g["total"].as_i64().unwrap_or(0))
            .expect("a group");
        assert_eq!(
            biggest["rows"].as_array().expect("rows is an array").len(),
            ROW_CAP,
            "a group's rows must stop at ROW_CAP, not grow with the store"
        );
        assert_eq!(biggest["total"], json!(over_cap));
        assert_eq!(biggest["truncated"], json!(true));
        // …and the whole-list count is not the capped one.
        assert_eq!(doc["tasks"]["total"], json!(over_cap));
    }

    /// The flip side: a store under the cap is not miscalled truncated, and
    /// nothing is silently dropped from a short list.
    #[test]
    fn the_task_rows_are_whole_and_untruncated_under_the_cap() {
        let under_cap = ROW_CAP as i64 - 5;
        let dash = dashboard_with(under_cap);
        let doc = document(&dash, 7, &all_panels());
        let g = &doc["tasks"]["groups"][0];
        assert_eq!(g["rows"].as_array().unwrap().len(), under_cap as usize);
        assert_eq!(g["total"], json!(under_cap));
        assert_eq!(g["truncated"], json!(false));
    }

    /// #152's other half: `--panels`/`dashboard.panels` used to only rename
    /// the `panels` array — every panel's body was still built and emitted no
    /// matter what `order` said. A panel left out of `order` must not appear
    /// in the document at all, and the ones named must still be there.
    #[test]
    fn only_the_named_panels_appear_in_the_document() {
        let dash = dashboard_with(3);
        let order = vec![PanelId::Tasks];
        let doc = document(&dash, 7, &order);
        let obj = doc.as_object().expect("document is an object");

        for missing in ["projects", "burndown", "tokens"] {
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
            "tasks",
        ] {
            assert!(obj.contains_key(present), "`{present}` must be present");
        }
    }

    /// #217: `report.summary`'s `tokens_confidence` (D50's trust hierarchy —
    /// the group's worst measurement) reached the `TokenRow` model but this
    /// document, the thing `tasqx --json dashboard` actually prints, dropped
    /// it on the floor — `grep -c confidence` over that output found zero
    /// hits even against a store the daemon had flagged low-confidence.
    #[test]
    fn tokens_rows_carry_their_confidence() {
        let tasks = json!({ "count": 0, "tasks": [] });
        let summary = json!({ "groups": [
            { "count": 1, "project": "work", "est_total": "PT0S",
              "tracked_total": "PT0S", "tokens_cache_read": 900,
              "tokens_cache_creation": 80, "tokens_in": 7, "tokens_out": 5,
              "tokens_confidence": "low" }
        ] });
        let projects = json!({ "count": 1, "projects": [
            { "archived": false, "default": true, "description": null,
              "id": "019fd213-0001-7000-8000-0004", "name": "work" }
        ] });
        let events = json!({ "count": 0, "events": [] });
        let d = build(
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
        );
        let doc = document(&d, 7, &[PanelId::Tokens]);
        assert_eq!(
            doc["tokens"]["rows"][0]["confidence"],
            json!("low"),
            "confidence never reached the JSON document: {doc}"
        );
    }
}
