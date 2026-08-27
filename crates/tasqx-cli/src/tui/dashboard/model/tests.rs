//! Tests for the dashboard's data layer and geometry.
//!
//! Both halves are pure, so all of this is ordinary unit testing with no
//! terminal anywhere — which is the whole reason the split exists. The JSON
//! fixtures below are copied from real `--json` output against a seeded store,
//! not invented: a mapper tested against a shape the engine does not produce is
//! a mapper that passes and then reads nothing.

use super::*;

/// Unbounded demand: these tests assert about GEOMETRY, so every panel is
/// treated as having more content than any terminal could hold. The
/// content-aware ceiling has its own tests — mixing the two here would make a
/// pure-arithmetic assertion depend on a fixture's task count.
const ANY: &dyn Fn(PanelId) -> u16 = &|_| u16::MAX;
use jiff::civil::date;
use jiff::Timestamp;
use serde_json::{json, Value};

// ============================================================================
// Fixtures
// ============================================================================

fn now() -> Timestamp {
    "2026-08-05T12:00:00Z".parse().unwrap()
}

fn today() -> jiff::civil::Date {
    date(2026, 8, 5)
}

/// One task row with every key `task.list {}` really emits, defaults matching a
/// minimal task. Nulls are emitted rather than omitted, which is why every
/// nullable key is present and set to `null` here.
fn task_row(short_id: i64, title: &str) -> Value {
    json!({
        "_rev": 1,
        "active_since": null,
        "blocked": false,
        "completed": null,
        "created": "2026-08-01T09:00:00Z",
        "due": null,
        "estimate": null,
        "id": format!("019fd213-0000-7000-8000-{short_id:012}"),
        "modified": "2026-08-01T09:00:00Z",
        "priority": null,
        "project": "work",
        "recurrence": null,
        "remind": null,
        "scheduled": null,
        "short_id": short_id,
        "status": "pending",
        "tags": [],
        "title": title,
        "tracked": "PT0S",
        "urgency": 0.0,
        "wait": null
    })
}

fn with(mut row: Value, key: &str, v: Value) -> Value {
    row[key] = v;
    row
}

fn task_list(rows: Vec<Value>) -> Value {
    json!({ "count": rows.len(), "tasks": rows })
}

/// A `report.summary {group_by:"project"}` result in the observed shape: the
/// group key is a field named after the grouping, metrics sit flat beside it,
/// and `count` arrives whether asked for or not.
fn summary(groups: Vec<Value>) -> Value {
    json!({ "generated": "2026-08-05T12:00:00Z", "groups": groups })
}

fn group(project: &str, est: &str, tracked: &str, buckets: [i64; 4]) -> Value {
    json!({
        "count": 1,
        "project": project,
        "est_total": est,
        "tracked_total": tracked,
        "tokens_cache_read": buckets[0],
        "tokens_cache_creation": buckets[1],
        "tokens_in": buckets[2],
        "tokens_out": buckets[3],
    })
}

fn project_list(rows: Vec<Value>) -> Value {
    json!({ "count": rows.len(), "projects": rows })
}

fn project(name: &str, default: bool, archived: bool) -> Value {
    json!({
        "archived": archived,
        "default": default,
        "description": null,
        "id": format!("019fd213-0001-7000-8000-{}", name.len()),
        "name": name
    })
}

fn no_events() -> Value {
    json!({ "count": 0, "events": [] })
}

fn build_with(tasks: Value, summary: Value, projects: Value) -> Dashboard {
    build(
        Sources {
            tasks: &tasks,
            summary: &summary,
            projects: &projects,
            events: &no_events(),
            event_limit: 100,
            days: 7,
        },
        now(),
        today(),
    )
}

/// Every panel this ladder can place, in display order.
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

// ============================================================================
// Geometry
// ============================================================================

/// The invariants that make a layout drawable at all, asserted over the whole
/// range of terminal sizes rather than at a handful of sampled points.
///
/// A violation of any of these is a panel drawn on top of another, a panel
/// hanging off the screen, or a bordered box with nothing inside it — the three
/// things that read as a broken program rather than a small screen.
#[test]
fn every_placement_is_inside_the_frame_and_overlaps_nothing() {
    for w in [56u16, 60, 72, 80, 96, 100, 120, 150, 200] {
        for h in [14u16, 18, 22, 24, 28, 32, 40, 50] {
            let Some(screen) = layout(w, h, &all_panels(), ANY) else {
                panic!("{w}x{h} is at or above the floor and must produce a layout");
            };
            for p in &screen.panels {
                assert!(
                    p.x + p.w <= w && p.y + p.h <= h,
                    "{w}x{h}: {:?} at ({},{}) {}x{} escapes the frame",
                    p.id,
                    p.x,
                    p.y,
                    p.w,
                    p.h
                );
                // Row 0 is the status bar; the last two rows are the closing
                // rule and the footer. No panel may be drawn over them.
                assert!(p.y >= 1, "{w}x{h}: {:?} overwrites the status bar", p.id);
                assert!(
                    p.y + p.h <= screen.rule_y,
                    "{w}x{h}: {:?} runs into the footer chrome",
                    p.id
                );
                let (_, _, _, body_h) = p.body();
                assert!(
                    body_h >= 1,
                    "{w}x{h}: {:?} was placed with no room for content — it should \
                     have been omitted instead",
                    p.id
                );
            }
            for (i, a) in screen.panels.iter().enumerate() {
                for b in &screen.panels[i + 1..] {
                    let disjoint = a.x + a.w <= b.x
                        || b.x + b.w <= a.x
                        || a.y + a.h <= b.y
                        || b.y + b.h <= a.y;
                    assert!(disjoint, "{w}x{h}: {:?} and {:?} overlap", a.id, b.id);
                }
            }
        }
    }
}

/// Below the floor the alternate screen is never entered (D58), and the boundary
/// is exact rather than approximately right.
#[test]
fn the_layout_refuses_below_the_floor_and_accepts_exactly_at_it() {
    assert!(
        layout(55, 14, &all_panels(), ANY).is_none(),
        "55 wide is too narrow"
    );
    assert!(
        layout(56, 13, &all_panels(), ANY).is_none(),
        "13 high is too short"
    );
    assert!(
        layout(56, 14, &all_panels(), ANY).is_some(),
        "56x14 is the floor itself and must draw"
    );
}

/// The most common terminal there is, counted row by row. This size was missing
/// from both original plans and is the one a reader is most likely to see.
#[test]
fn eighty_by_twentyfour_fills_the_frame_exactly() {
    let screen = layout(80, 24, &all_panels(), ANY).expect("80x24 draws");
    assert_eq!(screen.columns, 1, "80 cells is a single column");
    assert_eq!(rung_for(80, 24), Rung::S);

    // Panels occupy every row between the status bar and the closing rule, with
    // no gap and no overlap — asserted by walking them rather than by trusting
    // the sum.
    let mut expected_y = 1u16;
    for p in &screen.panels {
        assert_eq!(
            p.y, expected_y,
            "{:?} starts at {} but the previous panel ended at {expected_y}",
            p.id, p.y
        );
        expected_y += p.h;
    }
    assert_eq!(
        expected_y, screen.rule_y,
        "the panels must reach the closing rule exactly, leaving no dead rows"
    );
    assert_eq!(screen.rule_y, 22);
    assert_eq!(screen.footer_y, 23);
}

/// The slot exists when the three analysis panels cannot each have their own
/// rectangle, and does not when they can.
#[test]
fn the_analytics_slot_appears_only_where_the_three_panels_cannot_fit_separately() {
    let narrow = layout(80, 24, &all_panels(), ANY).unwrap();
    assert!(
        narrow.has_slot(),
        "on one column the three analysis panels share a slot"
    );
    for id in PanelId::SLOT_MEMBERS {
        assert!(
            narrow.placement(id).is_none(),
            "{id:?} must not also have its own rectangle beside the slot"
        );
    }

    let wide = layout(160, 44, &all_panels(), ANY).unwrap();
    assert!(
        !wide.has_slot(),
        "a wide terminal places all three separately"
    );
    for id in PanelId::SLOT_MEMBERS {
        assert!(
            wide.placement(id).is_some(),
            "{id:?} must have its own rectangle when there is room"
        );
    }
}

/// The slot is all-or-nothing: its floor or it is not drawn at all. D58 forbids
/// drawing a panel clipped, and a slot one row short of its occupant is exactly
/// that.
///
/// Asserted against the SPEC rather than a literal. This test read `body >= 6`
/// under a comment claiming "a three-line burndown is not a burndown" — but
/// `burndown_body` emits two lines, three when the window is clipped, and never
/// more however tall the box. The literal outlived the number it was copied
/// from, and pinned three blank rows in place as if they were a requirement.
#[test]
fn the_slot_is_never_drawn_below_its_floor() {
    let floor = spec_body(PanelId::Slot, Detail::Full);
    for h in 14u16..=40 {
        let screen = layout(80, h, &all_panels(), ANY).unwrap();
        if let Some(slot) = screen.placement(PanelId::Slot) {
            let (_, _, _, body) = slot.body();
            assert!(
                body >= floor,
                "80x{h}: the slot was placed with {body} body lines, under a \
                 floor of {floor} — below it the slot must be omitted, not shrunk"
            );
        }
    }
}

/// A column too narrow to hold a task title is not a column.
///
/// This is asserted against the BREAKPOINT TABLE, not against sampled sizes,
/// because that is where the property actually lives: each rung's column count
/// divided into the narrowest width that can reach it must still leave a
/// readable column. Written this way after a bite-check showed the runtime guard
/// it replaces could never fire — the breakpoints already satisfied it, so the
/// branch was unreachable and the test that "covered" it proved nothing. Lower a
/// breakpoint or add a column and this goes red at the table.
#[test]
fn every_rung_gives_its_columns_room_to_read() {
    for (rung, min_w) in RUNG_MIN_WIDTH {
        let cols = columns_for(rung);
        let narrowest = min_w / cols;
        assert!(
            narrowest >= MIN_COLUMN,
            "{rung:?} can be reached at {min_w} cells with {cols} columns, giving \
             {narrowest}-cell columns — below the {MIN_COLUMN} a title needs"
        );
    }
    // And the property holds in the built layouts too, at every real width.
    for w in MIN_WIDTH..=200 {
        for p in &layout(w, 44, &all_panels(), ANY).unwrap().panels {
            assert!(
                p.w >= MIN_COLUMN,
                "{w} wide: {:?} got a {}-cell column",
                p.id,
                p.w
            );
        }
    }
}

/// The fit's first phase, driven directly at a budget no real terminal reaches.
///
/// With today's specs and column assignments the drop never fires through
/// `layout` — the floors always fit, which a bite-check proved by disabling the
/// loop and watching nothing go red. It is kept and tested here anyway, because
/// it is the phase that holds "a panel that does not fit is omitted, never drawn
/// clipped": add a panel to a column or raise a floor and it becomes reachable,
/// and an untested fit phase would fail by drawing off-screen.
#[test]
fn the_fit_drops_panels_from_the_bottom_rather_than_overflowing() {
    let members = [
        PanelId::Now,
        PanelId::Next,
        PanelId::Due,
        PanelId::Blocked,
        PanelId::Recent,
    ];
    // Five panels need 10 rows at their floors. Give them 5.
    let placed = fit(&members, 5, 0, 80, ANY);
    let total: u16 = placed.iter().map(|p| p.h).sum();
    assert!(
        total <= 5,
        "the fit returned {total} rows for a 5-row budget: {placed:?}"
    );
    assert!(
        !placed.is_empty() && placed.len() < members.len(),
        "some panels must be dropped and some kept, got {} of {}",
        placed.len(),
        members.len()
    );
    // Dropping is from the BOTTOM: the first panels in column order survive.
    assert_eq!(placed[0].id, PanelId::Now, "the top panel is kept");
    assert!(
        !placed.iter().any(|p| p.id == PanelId::Recent),
        "the bottom panel is the first to go"
    );

    // And a budget that fits nothing places nothing rather than something broken.
    assert!(fit(&members, 1, 0, 80, ANY).is_empty());
    assert!(fit(&members, 0, 0, 80, ANY).is_empty());
}

/// Configuration decides what exists. A panel absent from `dashboard.panels` is
/// never placed, and removing one must not strand the others.
#[test]
fn a_panel_left_out_of_the_configured_order_is_never_placed() {
    let order = vec![PanelId::Now, PanelId::Next];
    let screen = layout(120, 40, &order, ANY).unwrap();
    for p in &screen.panels {
        assert!(
            p.id == PanelId::Now || p.id == PanelId::Next,
            "{:?} was placed but is not in the configured order",
            p.id
        );
    }
    assert!(screen.placement(PanelId::Now).is_some());
    assert!(screen.placement(PanelId::Next).is_some());

    // Dropping all three slot members drops the slot with them.
    let no_analytics = vec![PanelId::Now, PanelId::Next, PanelId::Due];
    let screen = layout(80, 24, &no_analytics, ANY).unwrap();
    assert!(
        !screen.has_slot(),
        "with no analytics panels configured the slot must not appear"
    );
}

// ============================================================================
// Mappers
// ============================================================================

#[test]
fn the_status_bar_counts_agree_with_the_panels_below_it() {
    let tasks = task_list(vec![
        with(
            with(task_row(1, "active one"), "status", json!("active")),
            "active_since",
            json!("2026-08-05T11:00:00Z"),
        ),
        with(task_row(2, "blocked one"), "blocked", json!(true)),
        with(
            task_row(3, "overdue one"),
            "due",
            json!("2026-07-01T00:00:00Z"),
        ),
        with(
            with(task_row(4, "done recently"), "status", json!("done")),
            "completed",
            json!("2026-08-04T10:00:00Z"),
        ),
        with(
            with(task_row(5, "done long ago"), "status", json!("done")),
            "completed",
            json!("2026-01-01T10:00:00Z"),
        ),
    ]);
    let d = build_with(tasks, summary(vec![]), project_list(vec![]));

    assert_eq!(d.status.open, 3, "active + blocked + overdue are open");
    assert_eq!(d.status.active, 1);
    assert_eq!(
        d.status.overdue,
        d.due.overdue.len(),
        "the header may never disagree with the DUE panel"
    );
    assert_eq!(
        d.status.blocked,
        d.blocked.rows.len(),
        "the header may never disagree with the BLOCKED panel"
    );
    assert_eq!(d.status.done_week, 1, "only the recent completion counts");
}

/// The bug this bucketing exists to avoid: a date-only `due` normalises to
/// midnight, so an instant comparison calls a task due TODAY overdue from one
/// second past midnight. `report.summary`'s own `overdue` metric does exactly
/// that, which is why the dashboard derives its own.
#[test]
fn a_task_due_today_at_midnight_is_today_and_not_overdue() {
    let tasks = task_list(vec![
        with(
            task_row(1, "due today"),
            "due",
            json!("2026-08-05T00:00:00Z"),
        ),
        with(
            task_row(2, "due yesterday"),
            "due",
            json!("2026-08-04T00:00:00Z"),
        ),
        with(
            task_row(3, "due tomorrow"),
            "due",
            json!("2026-08-06T00:00:00Z"),
        ),
        with(
            task_row(4, "due in a week"),
            "due",
            json!("2026-08-12T00:00:00Z"),
        ),
        with(
            task_row(5, "due next month"),
            "due",
            json!("2026-09-12T00:00:00Z"),
        ),
    ]);
    let d = build_with(tasks, summary(vec![]), project_list(vec![]));

    assert_eq!(d.due.today.len(), 1, "midnight today is TODAY, not overdue");
    assert_eq!(d.due.today[0].short_id, 1);
    assert_eq!(d.due.overdue.len(), 1);
    assert_eq!(d.due.overdue[0].short_id, 2);
    assert_eq!(d.due.tomorrow.len(), 1);
    assert_eq!(d.due.week.len(), 1, "12 Aug is inside the week horizon");
    assert!(
        d.due.week.iter().all(|t| t.short_id != 5),
        "next month is beyond the horizon and must be dropped, not bucketed"
    );
}

/// `@working` excludes blocked work, and the dashboard splits the two apart
/// rather than inheriting the blind spot (D58).
#[test]
fn a_blocked_task_is_in_blocked_and_not_in_next_up() {
    let tasks = task_list(vec![
        with(task_row(1, "ready"), "urgency", json!(5.0)),
        with(
            with(task_row(2, "waiting"), "blocked", json!(true)),
            "urgency",
            json!(9.0),
        ),
        with(task_row(3, "backlog item"), "status", json!("backlog")),
    ]);
    let d = build_with(tasks, summary(vec![]), project_list(vec![]));

    let next_ids: Vec<i64> = d.next.rows.iter().map(|t| t.short_id).collect();
    assert_eq!(next_ids, vec![1], "only the unblocked, non-backlog task");
    assert_eq!(d.blocked.rows.len(), 1);
    assert_eq!(d.blocked.rows[0].short_id, 2);
}

/// The ramp denominator is computed the way `render` computes it, so the
/// dashboard and `tasqx list` shade the same task the same colour.
#[test]
fn the_urgency_ramp_denominator_never_falls_below_one() {
    let d = build_with(
        task_list(vec![task_row(1, "zero urgency")]),
        summary(vec![]),
        project_list(vec![]),
    );
    assert_eq!(
        d.next.max_urgency, 1.0,
        "an all-zero store must not divide by zero"
    );

    let d = build_with(
        task_list(vec![with(task_row(1, "hot"), "urgency", json!(17.6))]),
        summary(vec![]),
        project_list(vec![]),
    );
    assert_eq!(d.next.max_urgency, 17.6);
}

/// The NOW card exists only while a timer runs, and reports time INCLUDING the
/// open interval — `tracked` alone reads as the final answer when it is only
/// the total so far.
#[test]
fn the_now_card_adds_the_running_interval_to_the_tracked_total() {
    let d = build_with(
        task_list(vec![task_row(1, "idle")]),
        summary(vec![]),
        project_list(vec![]),
    );
    assert!(d.now.is_none(), "no timer, no card");

    let tasks = task_list(vec![with(
        with(
            with(task_row(1, "running"), "status", json!("active")),
            "active_since",
            json!("2026-08-05T11:00:00Z"),
        ),
        "tracked",
        json!("PT30M"),
    )]);
    let d = build_with(tasks, summary(vec![]), project_list(vec![]));
    let card = d.now.expect("a running timer produces a card");
    assert_eq!(card.elapsed_secs, 3600, "one hour since 11:00 at 12:00");
    assert_eq!(
        card.total_secs(),
        1800 + 3600,
        "the card shows tracked PLUS the interval still running"
    );
}

/// Estimates are stored verbatim-normalised, not canonicalised, so the one
/// checked reader is the only thing that gets them right.
#[test]
fn estimates_go_through_the_shared_duration_reader() {
    for (iso, want) in [("PT3H", 10_800), ("PT90M", 5_400), ("P2D", 172_800)] {
        let tasks = task_list(vec![with(task_row(1, "estimated"), "estimate", json!(iso))]);
        let d = build_with(tasks, summary(vec![]), project_list(vec![]));
        assert_eq!(
            d.next.rows[0].estimate_secs,
            Some(want),
            "{iso} must parse through the shared reader"
        );
    }
}

/// An unrecognised status is passed through by the engine beside a flag rather
/// than refused, so a closed enum here would drop a row the store is showing
/// everyone else.
#[test]
fn an_unrecognised_status_is_carried_rather_than_dropped() {
    let tasks = task_list(vec![with(
        task_row(1, "from the future"),
        "status",
        json!("quantum"),
    )]);
    let d = build_with(tasks, summary(vec![]), project_list(vec![]));
    assert_eq!(d.recent.rows.len(), 1, "the row must survive the mapping");
    assert_eq!(d.recent.rows[0].status, Status::Other("quantum".into()));
    assert!(
        d.recent.rows[0].status.is_open(),
        "an unknown status counts as open, as it does everywhere else"
    );
}

/// D19, one surface over: a ratatui cell is written to the terminal verbatim, so
/// a title from `store.import` or an MCP write tool must be sanitised at
/// construction — not at draw time, where eight panels would each have to
/// remember.
#[test]
fn a_row_sanitises_the_untrusted_text_it_is_built_from() {
    let hostile = "evil\u{1b}]0;pwned\u{7}title";
    let tasks = task_list(vec![with(
        with(task_row(1, hostile), "project", json!("proj\u{1b}[31m")),
        "tags",
        json!(["tag\u{7}bell"]),
    )]);
    let d = build_with(tasks, summary(vec![]), project_list(vec![]));
    let row = &d.recent.rows[0];
    assert!(
        !row.title().contains('\u{1b}') && !row.title().contains('\u{7}'),
        "the title still carries control bytes: {:?}",
        row.title()
    );
    assert!(
        !row.project().unwrap().contains('\u{1b}'),
        "the project name still carries control bytes"
    );
    // Tags are deliberately not carried by the model — no panel draws them, and
    // a field the model holds without showing is a field nobody sanitises on
    // the day someone starts showing it.
}

// ============================================================================
// The projects/tokens join
// ============================================================================

/// A project with no tasks has no summary group at all. It still gets a row —
/// an empty project that vanished from the panel would read as deleted.
#[test]
fn a_project_with_no_tasks_still_gets_a_row() {
    let d = build_with(
        task_list(vec![]),
        summary(vec![]),
        project_list(vec![project("empty", true, false)]),
    );
    let row = d
        .projects
        .rows
        .iter()
        .find(|r| r.name() == Some("empty"))
        .expect("an empty project keeps its row");
    assert_eq!(row.open, 0);
    assert!(row.is_default);
}

/// An archived project that still holds tasks does get a summary group, so it
/// must be shown — flagged, and after the live ones.
#[test]
fn an_archived_project_that_still_holds_tasks_is_shown_last() {
    let tasks = task_list(vec![
        with(task_row(1, "live work"), "project", json!("live")),
        with(task_row(2, "old work"), "project", json!("retired")),
    ]);
    let d = build_with(
        tasks,
        summary(vec![
            group("live", "PT0S", "PT0S", [0; 4]),
            group("retired", "PT0S", "PT0S", [0; 4]),
        ]),
        project_list(vec![
            project("live", true, false),
            project("retired", false, true),
        ]),
    );
    let names: Vec<Option<&str>> = d.projects.rows.iter().map(|r| r.name()).collect();
    assert_eq!(
        names,
        vec![Some("live"), Some("retired")],
        "archived projects sort after live ones"
    );
    assert!(d.projects.rows[1].archived);
}

/// `report.summary`'s `count` excludes cancelled work (D24) while the snapshot
/// does not. The panel counts from the SNAPSHOT, so a PROJECTS row can never
/// disagree with NEXT UP about the same project.
#[test]
fn the_project_open_count_comes_from_the_snapshot_not_from_the_summary() {
    let tasks = task_list(vec![
        with(task_row(1, "open"), "project", json!("work")),
        with(
            with(task_row(2, "cancelled"), "status", json!("cancelled")),
            "project",
            json!("work"),
        ),
    ]);
    // The summary says count:1 — D24 dropped the cancelled row.
    let d = build_with(
        tasks,
        summary(vec![group("work", "PT1H", "PT30M", [0; 4])]),
        project_list(vec![project("work", true, false)]),
    );
    let row = &d.projects.rows[0];
    assert_eq!(row.open, 1, "one open task, counted from the snapshot");
    assert_eq!(row.est_secs, 3600, "durations still come from the summary");
    assert_eq!(row.tracked_secs, 1800);
}

/// `(none)` is how the summary spells the project-less bucket — and it is also
/// a name a user can really create. The real project wins the key.
#[test]
fn the_none_bucket_maps_to_no_project_unless_a_real_project_is_named_that() {
    let d = build_with(
        task_list(vec![with(task_row(1, "loose"), "project", Value::Null)]),
        summary(vec![group("(none)", "PT0S", "PT0S", [0; 4])]),
        project_list(vec![]),
    );
    assert!(
        d.projects.rows.iter().any(|r| r.name().is_none()),
        "with no real `(none)` project the bucket is the project-less one"
    );

    let d = build_with(
        task_list(vec![with(task_row(1, "odd"), "project", json!("(none)"))]),
        summary(vec![group("(none)", "PT0S", "PT0S", [0; 4])]),
        project_list(vec![project("(none)", false, false)]),
    );
    assert!(
        d.projects.rows.iter().any(|r| r.name() == Some("(none)")),
        "a real project named `(none)` keeps its own identity"
    );
}

/// The four buckets stay apart, in the fixed D48 order, and are never summed
/// into one number — `tokens_total` was removed on purpose (D50).
#[test]
fn the_four_token_buckets_stay_in_order_and_are_never_blended() {
    let d = build_with(
        task_list(vec![with(task_row(1, "spendy"), "project", json!("work"))]),
        summary(vec![group("work", "PT0S", "PT0S", [900, 80, 7, 5])]),
        project_list(vec![project("work", true, false)]),
    );
    assert_eq!(d.tokens.rows.len(), 1);
    assert_eq!(
        d.tokens.rows[0].buckets,
        [900, 80, 7, 5],
        "cacheR, cacheW, in, out — the D48 order"
    );
    assert_eq!(d.tokens.totals, [900, 80, 7, 5]);
}

/// A project that spent nothing is not a row in TOKENS. A list of zeroes is
/// noise on a panel whose whole job is to say where the spend went.
#[test]
fn a_project_that_spent_no_tokens_is_left_out_of_the_tokens_panel() {
    let d = build_with(
        task_list(vec![
            with(task_row(1, "spendy"), "project", json!("work")),
            with(task_row(2, "free"), "project", json!("side")),
        ]),
        summary(vec![
            group("work", "PT0S", "PT0S", [10, 0, 0, 0]),
            group("side", "PT0S", "PT0S", [0, 0, 0, 0]),
        ]),
        project_list(vec![
            project("work", true, false),
            project("side", false, false),
        ]),
    );
    let names: Vec<Option<&str>> = d.tokens.rows.iter().map(|r| r.name()).collect();
    assert_eq!(names, vec![Some("work")]);
    assert_eq!(
        d.projects.rows.len(),
        2,
        "but both projects keep their PROJECTS row"
    );
}

/// The event page coming back full means the window may be incomplete, and the
/// panel has to be able to say so rather than drawing a confident wrong line.
#[test]
fn a_full_event_page_marks_the_burndown_as_truncated() {
    let events = json!({ "count": 100, "events": [] });
    let d = build(
        Sources {
            tasks: &task_list(vec![]),
            summary: &summary(vec![]),
            projects: &project_list(vec![]),
            events: &events,
            event_limit: 100,
            days: 7,
        },
        now(),
        today(),
    );
    assert!(
        d.burndown.truncated,
        "count == limit means possibly clipped"
    );

    let events = json!({ "count": 12, "events": [] });
    let d = build(
        Sources {
            tasks: &task_list(vec![]),
            summary: &summary(vec![]),
            projects: &project_list(vec![]),
            events: &events,
            event_limit: 100,
            days: 7,
        },
        now(),
        today(),
    );
    assert!(!d.burndown.truncated);
}

/// RECENT is deliberately unfiltered by status: a task finished four minutes
/// ago is exactly what "where was I" means.
#[test]
fn recent_is_newest_first_and_keeps_finished_work() {
    let tasks = task_list(vec![
        with(
            task_row(1, "old"),
            "modified",
            json!("2026-08-01T09:00:00Z"),
        ),
        with(
            with(task_row(2, "just done"), "status", json!("done")),
            "modified",
            json!("2026-08-05T11:56:00Z"),
        ),
        with(
            task_row(3, "middle"),
            "modified",
            json!("2026-08-03T09:00:00Z"),
        ),
    ]);
    let d = build_with(tasks, summary(vec![]), project_list(vec![]));
    let ids: Vec<i64> = d.recent.rows.iter().map(|t| t.short_id).collect();
    assert_eq!(ids, vec![2, 3, 1], "newest `modified` first");
    assert_eq!(
        d.recent.rows[0].status,
        Status::Done,
        "finished work belongs here"
    );
}

/// No panel is given more rows than it has content for.
///
/// Growers used to take rows until the column ran out, so on a big terminal
/// against a small store the space went to whoever was early in `RAISE_ORDER`
/// rather than to whoever could use it. Measured at 120x40 before the fix:
/// BLOCKED held two tasks in a twelve-row box, DUE swallowed the rest of its
/// column, and BURNDOWN — whose neighbour had nothing left to show — sat on its
/// floor. A third of the screen was blank INSIDE panels.
///
/// The floor is the other half of the bound: a panel is never given LESS than
/// the level it was placed at costs, or it would be drawn clipped.
#[test]
fn no_panel_is_given_more_rows_than_it_has_content_for() {
    // Deliberately less content than any of these terminals can hold, which is
    // the case that exposed the bug — a store big enough to fill the screen
    // hides it completely.
    let small: &dyn Fn(PanelId) -> u16 = &|id| match id {
        PanelId::Now => 3,
        PanelId::Next => 2,
        PanelId::Due => 2,
        PanelId::Blocked => 1,
        PanelId::Recent => 2,
        PanelId::Projects => 1,
        PanelId::Tokens => 1,
        PanelId::Burndown => 2,
        PanelId::Slot => 2,
    };
    for w in [56u16, 80, 96, 120, 160, 200] {
        for h in [14u16, 20, 24, 30, 40, 50] {
            let screen = layout(w, h, &all_panels(), small).expect("above the floor");
            for p in &screen.panels {
                let (_, _, _, body) = p.body();
                let floor = floor_body(p.id);
                let ceiling = small(p.id).max(floor);
                assert!(
                    body <= ceiling,
                    "{w}x{h}: {:?} was given {body} rows for {} lines of content \
                     (floor {floor})",
                    p.id,
                    small(p.id)
                );
                assert!(
                    body >= floor,
                    "{w}x{h}: {:?} was given {body} rows, under its floor of {floor}",
                    p.id
                );
            }
        }
    }
}

/// The slot is sized for the tallest member the reader configured, not for the
/// one currently in it.
///
/// `6`, `7` and `8` swap the occupant in place. A slot that fitted only the
/// current one would have to resize under the reader on every such keypress —
/// or fail to find the rows and vanish, which is a key that deletes a panel.
#[test]
fn the_slot_is_sized_for_its_tallest_member() {
    // Six projects against one token row: PROJECTS is much the taller member,
    // so a slot sized from any other member — or from whichever happens to be
    // showing — would come out short.
    let d = build_with(
        task_list(vec![task_row(1, "one")]),
        summary(vec![group("a", "PT1H", "PT0S", [0, 0, 0, 0])]),
        project_list(vec![
            project("a", true, false),
            project("b", false, false),
            project("c", false, false),
            project("d", false, false),
            project("e", false, false),
            project("f", false, false),
        ]),
    );
    let members = PanelId::SLOT_MEMBERS.to_vec();
    let tall = demand(&d, &members, PanelId::Projects);
    // Read off the model, not written down: the panel also carries a row for
    // the project the task names but the project list does not, and a literal
    // here would be asserting my count of the fixture rather than the rule.
    assert_eq!(
        tall as usize,
        d.projects.rows.len(),
        "PROJECTS wants one row per row it holds"
    );
    assert!(
        demand(&d, &members, PanelId::Tokens) < tall
            && demand(&d, &members, PanelId::Burndown) < tall,
        "the fixture must have one clearly tallest member for this to mean anything"
    );

    // The function itself, because the layout below would be satisfied by a
    // slot that merely happened to be big enough.
    assert_eq!(
        demand(&d, &members, PanelId::Slot),
        tall,
        "the slot's demand is the tallest member's, not the shortest or the current one"
    );

    let screen =
        layout(80, 30, &all_panels(), &|id| demand(&d, &members, id)).expect("above the floor");
    let slot = screen
        .placement(PanelId::Slot)
        .expect("80x30 uses the slot on the one-column rung");
    let (_, _, _, body) = slot.body();
    assert_eq!(body, tall, "and the layout gives it those rows");
}

/// A taller terminal never shows LESS of any panel.
///
/// Within one rung this is absolute. It was not: raising a panel a whole level
/// cost up to three rows at once and emptied the pool feeding its neighbours,
/// so one extra row of terminal could take a row away from two panels at once.
/// Measured before the fix, at 80 columns:
///
///     80x28   Now:3  Next:4  Due:2  Blocked:2  Slot:6  Recent:2
///     80x29   Now:3  Next:4  Due:5  Blocked:1  Slot:6  Recent:1
///
/// Five such inversions existed between 14 and 40 rows. Rows are handed out one
/// at a time now, so one more row in the budget is one more row for exactly one
/// panel and no panel can lose one.
#[test]
fn a_taller_terminal_never_shrinks_a_panel() {
    for w in [56u16, 60, 80, 96, 120, 150, 200] {
        let mut prev: Option<(Rung, std::collections::HashMap<PanelId, u16>)> = None;
        for h in MIN_HEIGHT..=60 {
            let screen = layout(w, h, &all_panels(), ANY).unwrap();
            let bodies: std::collections::HashMap<PanelId, u16> = screen
                .panels
                .iter()
                .map(|p| {
                    let (_, _, _, body) = p.body();
                    (p.id, body)
                })
                .collect();

            if let Some((prev_rung, prev_bodies)) = &prev {
                if *prev_rung == rung_for(w, h) {
                    for (id, was) in prev_bodies {
                        let now = bodies.get(id).copied().unwrap_or(0);
                        assert!(
                            now >= *was,
                            "{w}x{h}: {id:?} shrank from {was} to {now} when the terminal \
                             grew by one row (rung {:?})",
                            rung_for(w, h)
                        );
                    }
                } else {
                    // Across a rung boundary the panel SET changes — the
                    // analytics slot appears and claims its floor — so a light
                    // panel giving rows back is a deliberate trade. The trade
                    // still has to be worth something: it may not lose a panel.
                    assert!(
                        bodies.len() >= prev_bodies.len(),
                        "{w}x{h}: crossing into {:?} dropped a panel ({} -> {})",
                        rung_for(w, h),
                        prev_bodies.len(),
                        bodies.len()
                    );
                }
            }
            prev = Some((rung_for(w, h), bodies));
        }
    }
}

/// A panel's detail level is what its rows can pay for.
///
/// The two used to be decided separately, which allowed a panel labelled `Full`
/// to be drawn in two rows. The level is derived from the allocation now, so
/// they cannot disagree.
#[test]
fn the_detail_level_matches_the_rows_the_panel_actually_got() {
    for (w, h) in [(56u16, 14u16), (80, 24), (96, 28), (120, 32), (200, 50)] {
        for p in &layout(w, h, &all_panels(), ANY).unwrap().panels {
            let (_, _, _, body) = p.body();
            let claimed = spec_body(p.id, p.detail);
            assert!(
                body >= claimed,
                "{w}x{h}: {:?} claims {:?} ({claimed} rows) but was given {body}",
                p.id,
                p.detail
            );
        }
    }
}

/// An idle NOW asks for the one row it draws, not the three a running timer
/// needs.
///
/// The empty store is the first screen anybody sees, and NOW is its first
/// panel. `now_body` answers "no timer running · p to pick one" in one line;
/// demanding the card's full three left two blank rows directly under the
/// title on exactly that screen.
#[test]
fn an_idle_now_card_asks_for_one_row_not_three() {
    let members = PanelId::SLOT_MEMBERS.to_vec();

    let idle = build_with(
        task_list(vec![]),
        summary(vec![]),
        project_list(vec![project("a", true, false)]),
    );
    assert!(idle.now.is_none(), "the fixture must have no running timer");
    assert_eq!(
        demand(&idle, &members, PanelId::Now),
        1,
        "an idle NOW draws one sentence"
    );

    let running = build_with(
        task_list(vec![with(
            with(task_row(1, "the running one"), "status", json!("active")),
            "active_since",
            json!("2026-08-05T11:00:00Z"),
        )]),
        summary(vec![group("work", "PT1H", "PT0S", [0, 0, 0, 0])]),
        project_list(vec![project("work", true, false)]),
    );
    assert!(
        running.now.is_some(),
        "the fixture must have a running timer"
    );
    assert_eq!(
        demand(&running, &members, PanelId::Now),
        spec_body(PanelId::Now, Detail::Full),
        "a running NOW is a three-line card"
    );
}
