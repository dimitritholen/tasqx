use super::*;
use crate::render::test_support::*;
use crate::theme::{self, Caps, Ctx};
use serde_json::json;
use tasqx_core::markdown::TimeFormat;

#[test]
fn task_detail_shows_remind_when_set() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let base = json!({
        "short_id": 1, "title": "probe", "status": "pending", "urgency": 8.2,
        "project": "work", "due": "2026-07-20T17:00:00Z", "remind": "-1h",
        "estimate": "PT4H"
    });
    let out = task_detail(&ctx, &base, crate::clock::now());
    assert!(out.contains("remind"), "remind row missing: {out:?}");
    assert!(out.contains("-1h"), "remind value missing: {out:?}");
    // `estimate` is settable via `est:` sugar and totalled by `report`, so the
    // detail view must show it too — an invisible field is how the dependency
    // bug stayed hidden.
    assert!(out.contains("estimate"), "estimate row missing: {out:?}");
    assert!(out.contains("4h"), "estimate value missing: {out:?}");

    // Absent remind must stay absent — the row is conditional, like `due`.
    let mut bare = base.clone();
    bare["remind"] = json!("");
    assert!(!task_detail(&ctx, &bare, crate::clock::now()).contains("remind"));
}

/// audit #188: `tasqx show` was strictly poorer than the MCP
/// `tasqx_get_task` detail view for the exact same `task.get` result —
/// `created`/`modified`/`_rev` are in `--json show`'s payload and the human
/// renderer simply had no row for any of them, so `--expected-rev` (whose
/// own `--help` text points at `--json show` for the value) could not be
/// read without dropping to JSON.
#[test]
fn task_detail_includes_created_modified_and_rev() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let t = json!({
        "short_id": 169, "title": "raw shape probe", "status": "pending",
        "urgency": 11.5, "project": "dates",
        "created": "2026-09-09T10:38:04Z",
        "modified": "2026-09-09T11:02:00Z",
        "_rev": 3
    });
    let out = task_detail(&ctx, &t, crate::clock::now());
    assert!(
        out.lines().any(|l| l.trim_start().starts_with("created")),
        "created row missing: {out:?}"
    );
    assert!(
        out.lines().any(|l| l.trim_start().starts_with("modified")),
        "modified row missing: {out:?}"
    );
    assert!(
        out.lines()
            .any(|l| l.trim_start().starts_with("rev") && l.contains('3')),
        "rev row missing or wrong value — this is the value `--expected-rev` \
             needs: {out:?}"
    );
}

/// audit #143: `detail.time_format` is wired to the MCP `tasqx_get_task`
/// renderer only (`serve.rs`'s `McpServer::with_time_format`) — `show`
/// rendered byte-identical output under `iso`/`relative`/`both`, always the
/// raw ISO instant and the raw ISO-8601 duration.
#[test]
fn task_detail_honours_configured_time_format() {
    let now: Timestamp = "2026-09-09T12:00:00Z".parse().unwrap();
    let t = json!({
        "short_id": 2, "title": "timed task", "status": "pending",
        "urgency": 11.5, "project": "apollo",
        "due": "2026-09-10T00:00:00Z", "estimate": "PT4H"
    });

    let iso_ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_time_format(TimeFormat::Iso);
    let out_iso = task_detail(&iso_ctx, &t, now);
    assert!(
        out_iso.contains("2026-09-10T00:00:00Z"),
        "iso mode must keep the exact instant: {out_iso:?}"
    );
    assert!(
        out_iso.contains("PT4H"),
        "iso mode must keep the exact duration: {out_iso:?}"
    );

    let rel_ctx =
        Ctx::new(theme::default_theme(), Caps::PLAIN).with_time_format(TimeFormat::Relative);
    let out_rel = task_detail(&rel_ctx, &t, now);
    assert!(
        !out_rel.contains("2026-09-10T00:00:00Z"),
        "relative mode leaked the raw ISO instant: {out_rel:?}"
    );
    assert!(
        out_rel.contains("4h"),
        "estimate must read as a humanized duration, not PT4H: {out_rel:?}"
    );
    // D122: in the terminal, the relative forms are calendar days, rule 3
    // of docs/maintainers/terminal-style.md, not elapsed prose.
    assert!(
        out_rel.contains("tomorrow"),
        "a future due date must read as a calendar day: {out_rel:?}"
    );

    // The bug, precisely: every mode rendered the identical bytes.
    assert_ne!(
        out_iso, out_rel,
        "detail.time_format had no effect on `show`'s output"
    );
}

/// D132: every clock `show` prints is UTC, and the screen says so once —
/// on the first time value that carries a clock, not on every cell. Before
/// it, `today 17:00` read as the wall clock to anyone not on UTC.
#[test]
fn show_says_utc_once_on_its_first_clock() {
    let now: Timestamp = "2026-09-13T12:00:00Z".parse().unwrap();
    let t = json!({
        "short_id": 55, "title": "C", "status": "pending",
        "urgency": 8.0, "project": "work",
        "due": "2026-09-13T17:00:00Z", "scheduled": "2026-09-14T09:00:00Z",
        "created": "2026-09-01T10:00:00Z", "modified": "2026-09-13T11:00:00Z",
        "_rev": 2
    });
    for (layout, ctx) in [
        ("plain", Ctx::new(theme::default_theme(), Caps::PLAIN)),
        ("card", Ctx::new(theme::default_theme(), card_caps())),
    ] {
        let out = task_detail(&ctx, &t, now);
        assert_eq!(
            out.matches("UTC").count(),
            1,
            "{layout}: UTC must be said exactly once:\n{out}"
        );
        assert!(
            out.contains("today 17:00 UTC"),
            "{layout}: the marker is not on the first clock:\n{out}"
        );
    }
    // No clock on the screen, nothing to mark: a date is a date.
    let mut dated = t.clone();
    dated["due"] = json!("2026-09-20T00:00:00Z");
    dated["scheduled"] = json!("");
    dated["modified"] = json!("2026-09-10T11:00:00Z");
    let out = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &dated, now);
    assert!(
        !out.contains("UTC"),
        "a clockless card grew a marker:\n{out}"
    );
    // `iso` prints the stored `…Z`, which already says it.
    let iso = Ctx::new(theme::default_theme(), Caps::PLAIN).with_time_format(TimeFormat::Iso);
    let out = task_detail(&iso, &t, now);
    assert!(!out.contains("UTC"), "iso mode doubled its Z:\n{out}");
}

/// D76's gate: the card belongs to VT terminals only. The plain path is
/// what every pipe, script and executed docs example reads, so it must
/// keep its exact old layout and never leak a frame glyph.
#[test]
fn the_cards_render_only_on_a_unicode_terminal() {
    let t = full_task();
    let plain = task_detail(
        &Ctx::new(theme::default_theme(), Caps::PLAIN),
        &t,
        crate::clock::now(),
    );
    assert!(
        plain.contains("  status     "),
        "the plain detail layout changed: {plain:?}"
    );
    assert!(
        !plain.contains('╭') && !plain.contains('─') && !plain.contains('▌'),
        "card glyphs leaked into the plain path: {plain:?}"
    );
    let card = task_detail(
        &Ctx::new(theme::default_theme(), card_caps()),
        &t,
        crate::clock::now(),
    );
    assert!(
        card.contains('▌') && !card.contains("  status     "),
        "unicode caps should render the rail card: {card:?}"
    );
    let added = added(
        &Ctx::new(theme::default_theme(), card_caps()),
        &t,
        crate::clock::now(),
    );
    assert!(
        added.starts_with('▌'),
        "the add card lost its rail: {added:?}"
    );
}

/// The rail card (the C variant that superseded the D76 ledger): every
/// line of the detail begins with the status rail, so the card reads as
/// one object even where a value is blank.
#[test]
fn the_show_card_draws_the_rail_on_every_line() {
    let ctx = Ctx::new(theme::default_theme(), card_caps());
    let out = task_detail(&ctx, &full_task(), crate::clock::now());
    for line in out.lines() {
        assert!(
            line.starts_with('▌'),
            "a line escaped the rail: {line:?}\n{out}"
        );
    }
}

/// The ledger never wrapped: a long title ran past the terminal edge and
/// the rule under it stopped at 80 while the title did not. The rail card
/// wraps title, values and annotations at the terminal width — and wraps
/// rather than truncates, so no word of the user's own text is lost.
#[test]
fn the_show_card_wraps_instead_of_overflowing_or_truncating() {
    let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(60);
    let mut t = full_task();
    t["title"] = json!("word ".repeat(40).trim().to_string());
    t["annotations"] = json!([{"body": "note ".repeat(50).trim().to_string()}]);
    let out = task_detail(&ctx, &t, crate::clock::now());
    for line in out.lines() {
        assert!(
            width(line) <= 60,
            "a line overflowed the terminal: {line:?}\n{out}"
        );
    }
    assert_eq!(
        out.matches("word").count(),
        40,
        "the wrap dropped or cut part of the title:\n{out}"
    );
    assert_eq!(
        out.matches("note").count(),
        50,
        "the wrap dropped or cut part of the annotation:\n{out}"
    );
}

/// #76.2: a multi-line annotation (markdown headers, a list, a table)
/// used to come out as one unbroken run — `san` turned every `\n` into a
/// space before the card ever saw it, and the card's own wrap reflowed on
/// whitespace besides. Both had to change: the body must round-trip its
/// line breaks, and the card must respect them rather than re-flowing
/// them away.
#[test]
fn the_show_card_keeps_annotation_line_breaks() {
    let ctx = Ctx::new(theme::default_theme(), card_caps());
    let mut t = full_task();
    t["annotations"] = json!([{"body": "# Heading\n\n- one\n- two\n\n| a | b |\n|---|---|"}]);
    let out = task_detail(&ctx, &t, crate::clock::now());
    assert!(
        out.contains("# Heading"),
        "the heading line is gone:\n{out}"
    );
    assert!(out.contains("- one"), "the first list item is gone:\n{out}");
    assert!(
        out.contains("- two"),
        "the second list item is gone:\n{out}"
    );
    assert!(out.contains("| a | b |"), "the table row is gone:\n{out}");
    // The two list items must land on SEPARATE lines, not fused by a
    // whitespace-only reflow into "- one - two".
    assert!(
        !out.contains("- one - two"),
        "the line break between list items was collapsed:\n{out}"
    );
}

/// Same defect, the byte-stable plain path: a stray `\n` in an annotation
/// body must survive to stdout rather than being neutralised to a space
/// by `san` (that treatment is right for a one-line field like a title,
/// wrong for a body that is legitimately more than one line — #195 made
/// the same call for `memory show`).
#[test]
fn the_plain_detail_keeps_annotation_line_breaks() {
    let t = json!({
        "short_id": 1, "title": "multi-line note", "status": "pending",
        "annotations": [{"body": "line one\nline two"}],
    });
    let out = task_detail(
        &Ctx::new(theme::default_theme(), Caps::PLAIN),
        &t,
        crate::clock::now(),
    );
    assert!(
        out.contains("line one\nline two"),
        "the annotation's line break did not survive to the plain layout: {out:?}"
    );
}

/// Short facts pair up two to a line (status beside priority), so the
/// card spends its height on content, not on a one-fact-per-line ledger.
#[test]
fn the_show_card_pairs_short_facts_on_one_line() {
    let ctx = Ctx::new(theme::default_theme(), card_caps());
    let out = task_detail(
        &ctx,
        &json!({
            "short_id": 7, "title": "pairing", "status": "pending",
            "priority": "M", "project": "work", "urgency": 4.3
        }),
        crate::clock::now(),
    );
    // D122 folded priority into the urgency cell, so the short pair is now
    // status and urgency.
    let paired = out
        .lines()
        .any(|l| l.contains("status") && l.contains("urgency"));
    assert!(paired, "status and urgency did not share a line:\n{out}");
}

/// D122, rule 3: a detail view spells its instants as calendar days and
/// its durations as `5h`. Under the default `both` it printed
/// `2026-09-11T09:36:55.218763722Z (just now)` and `PT5H (5h)`.
#[test]
fn show_spells_dates_as_calendar_days_and_durations_once() {
    let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
    for caps in [Caps::PLAIN, card_caps()] {
        let ctx = Ctx::new(theme::default_theme(), caps).with_time_format(TimeFormat::Both);
        let out = task_detail(&ctx, &detail_fixture(), now);
        assert!(out.contains("Wed (in 5 days)"), "{out}");
        assert!(out.contains("1 Sep (10 days ago)"), "{out}");
        for raw in ["T00:00:00Z", "T09:36", "PT5H", ".218763722"] {
            assert!(!out.contains(raw), "{raw:?} leaked into show:\n{out}");
        }
    }
}

/// D122, rule 11: a blocked task says what blocks it, once, by name. It
/// was `blocked true` beside `depends_on #51`; and every task that was
/// not blocked carried `blocked false`.
#[test]
fn a_blocked_task_names_its_blocker_once_and_others_say_nothing() {
    let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
    let card = task_detail(
        &Ctx::new(theme::default_theme(), card_caps()),
        &detail_fixture(),
        now,
    );
    assert!(
        card.contains("⊘ blocked by #51 · Rate-limit the /search endpoint"),
        "{card}"
    );
    for gone in ["depends_on", "true", "false"] {
        assert!(!card.contains(gone), "{gone:?} still in the card:\n{card}");
    }
    let mut free = detail_fixture();
    free["blocked"] = json!(false);
    free["unmet_blockers"] = json!([]);
    let plain = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &free, now);
    assert!(!plain.contains("blocked"), "{plain}");
    assert!(
        plain.contains("depends_on"),
        "a met dependency still shows: {plain}"
    );
}

/// D122: a running task says so in its status line, and priority sits in
/// the urgency cell it modifies (rule 5). Two rows each used to say half
/// of one fact.
#[test]
fn running_joins_the_status_and_priority_joins_the_urgency() {
    let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
    let mut t = detail_fixture();
    t["status"] = json!("active");
    t["active_since"] = json!("2026-09-11T09:00:00Z");
    t["blocked"] = json!(false);
    let plain = task_detail(&Ctx::new(theme::default_theme(), Caps::PLAIN), &t, now);
    let status = plain.lines().find(|l| l.contains("status")).unwrap();
    assert!(
        status.contains("active") && status.contains("since"),
        "{plain}"
    );
    assert!(
        !plain.contains("running"),
        "a running row beside the status: {plain}"
    );
    assert!(!plain.contains("priority"), "{plain}");
    assert!(plain.contains("urgency    M 12.1"), "{plain}");
}

/// An open task past its deadline reads in `overdue` on the card, as its
/// row does in `list`. The card painted it in the plain emphasis every
/// other date got.
#[test]
fn an_overdue_deadline_is_painted_overdue_on_the_card() {
    let now: Timestamp = "2026-09-11T11:00:00Z".parse().unwrap();
    let ctx = Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    );
    let mut t = detail_fixture();
    t["due"] = json!("2026-09-09T00:00:00Z");
    let out = task_detail(&ctx, &t, now);
    assert!(out.contains(&ctx.paint("overdue", "2d ago")), "{out:?}");
}

/// The rail is the card's one loud signal, keyed to what the reader most
/// needs to know: blocked beats everything, an unreadable status is a
/// warning, and the open/active/closed states each carry their own tone.
#[test]
fn the_rail_role_is_keyed_to_status_and_blocked() {
    let t = |v: serde_json::Value| rail_role(&v);
    assert_eq!(
        t(json!({"status": "pending", "blocked": true})),
        "danger",
        "blocked outranks every status"
    );
    assert_eq!(
        t(json!({"status": "Done", "status_unrecognized": true})),
        "warn"
    );
    assert_eq!(t(json!({"status": "active"})), "timer.active");
    assert_eq!(t(json!({"status": "pending"})), "accent");
    assert_eq!(t(json!({"status": "backlog"})), "muted");
    assert_eq!(t(json!({"status": "done"})), "muted");
    assert_eq!(t(json!({"status": "cancelled"})), "muted");
}

/// Every conditional row toggled off one at a time, plus the value edges
/// (PT0S, empty collections, each priority, an unrecognized status) — the
/// fixture matrix the parity guard walks, so a condition drifting between
/// the layouts cannot hide behind the all-fields-set case.
fn detail_matrix() -> Vec<(String, serde_json::Value)> {
    let mut m = vec![("full".to_string(), full_task())];
    for key in [
        "project",
        "due",
        "remind",
        "scheduled",
        "wait",
        "recurrence",
        "estimate",
        "completed",
        "tracked",
        "active_since",
        "tags",
        "depends_on",
        "tokens",
        "annotations",
        "priority",
    ] {
        let mut t = full_task();
        t.as_object_mut().unwrap().remove(key);
        m.push((format!("without-{key}"), t));
    }
    let mut t = full_task();
    t["tracked"] = json!("PT0S");
    m.push(("tracked-zero".to_string(), t));
    let mut t = full_task();
    t["blocked"] = json!(false);
    m.push(("unblocked".to_string(), t));
    for p in ["M", "L"] {
        let mut t = full_task();
        t["priority"] = json!(p);
        m.push((format!("prio-{p}"), t));
    }
    let mut t = full_task();
    t["status"] = json!("Done");
    t["status_unrecognized"] = json!(true);
    m.push(("status-unrecognized".to_string(), t));
    let mut t = full_task();
    t["tags"] = json!([]);
    t["depends_on"] = json!([]);
    t["tokens"] = json!([]);
    t["annotations"] = json!([]);
    m.push(("empty-collections".to_string(), t));
    m
}

/// The two detail layouts must name the same facts, in the same
/// spellings. The plain view is the contract (its labels are what docs
/// and muscle memory know); this walks its label column and demands each
/// one appear in the card. Rename a row in one layout and not the other
/// and this is what goes red.
#[test]
fn the_detail_card_names_every_field_the_plain_view_names() {
    // Over the whole fixture matrix, not one all-fields task: with every
    // conditional toggled off in turn, a row CONDITION drifting between
    // the layouts fails here too, not only a renamed label.
    for (name, t) in detail_matrix() {
        let plain = task_detail(
            &Ctx::new(theme::default_theme(), Caps::PLAIN),
            &t,
            crate::clock::now(),
        );
        let card = task_detail(
            &Ctx::new(theme::default_theme(), card_caps()),
            &t,
            crate::clock::now(),
        );
        for line in plain.lines().skip(1) {
            let Some(first) = line.split_whitespace().next() else {
                continue;
            };
            if first == "·" {
                continue; // an annotation marker, not a field label
            }
            assert!(
                card.contains(first),
                "[{name}] the card lost the {first:?} row:\n{card}"
            );
        }
    }
    // And the full fixture still exercises the conditional rows at all.
    let plain = task_detail(
        &Ctx::new(theme::default_theme(), Caps::PLAIN),
        &full_task(),
        crate::clock::now(),
    );
    let labels = plain
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| *w != "·")
        .count();
    assert!(
        labels >= 15,
        "the parity scan saw only {labels} labels — full_task stopped \
             exercising the conditional rows and this guard is checking little"
    );
}

/// #217: `task.get`'s `tokens` array carries `confidence` per measurement
/// (the field MCP's markdown table already renders), but `tasqx show`
/// summed the four buckets and printed nothing else — a task whose only
/// measurement was a low-confidence discovery-pass guess read exactly
/// like one a verified `otel` correlation produced. The worst confidence
/// across the task's measurements must show on this line.
#[test]
fn show_marks_a_low_confidence_token_measurement() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let t = json!({
        "short_id": 109, "title": "t", "status": "pending", "urgency": 1.0,
        "tokens": [{"input_tokens": 10, "output_tokens": 0,
                    "cache_read_tokens": 0, "cache_creation_tokens": 0,
                    "confidence": "low"}],
    });
    let out = task_detail(&ctx, &t, crate::clock::now());
    let line = out
        .lines()
        .find(|l| l.contains("tokens"))
        .expect("tokens row");
    assert!(
        line.contains("low"),
        "the low confidence never reached the tokens line: {line:?}"
    );
}

/// A high-confidence-only task must NOT get a confidence marker at all —
/// the whole point is that the flag distinguishes the two.
#[test]
fn show_does_not_mark_a_fully_high_confidence_measurement() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let t = json!({
        "short_id": 1, "title": "t", "status": "pending", "urgency": 1.0,
        "tokens": [{"input_tokens": 10, "output_tokens": 0,
                    "cache_read_tokens": 0, "cache_creation_tokens": 0,
                    "confidence": "high"}],
    });
    let out = task_detail(&ctx, &t, crate::clock::now());
    let line = out
        .lines()
        .find(|l| l.contains("tokens"))
        .expect("tokens row");
    assert!(
        !line.contains("high") && !line.contains("low") && !line.contains("medium"),
        "an unwarranted confidence marker appeared: {line:?}"
    );
}

/// The same anomaly on the detail view, which does print status: `Done`
/// alone reads like a status the reader simply has not heard of yet.
#[test]
fn task_detail_marks_a_status_the_store_could_not_read() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = task_detail(
        &ctx,
        &json!({
            "short_id": 7, "title": "important work", "status": "Done",
            "status_unrecognized": true, "urgency": 1.0
        }),
        crate::clock::now(),
    );
    assert!(
        out.contains("Done"),
        "the stored value must survive to the screen: {out:?}"
    );
    assert!(
        out.contains("unrecognized"),
        "the anomaly must be labelled: {out:?}"
    );
    assert!(
        out.contains("pending"),
        "the five real statuses must be named: {out:?}"
    );
}

/// D138: the state is the marker, and the evidence sits under the claim it
/// supports.
#[test]
fn a_check_renders_with_its_state_as_a_marker_and_its_evidence_under_it() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let t = json!({
        "short_id": 1, "title": "t", "status": "pending",
        "checks": [
            { "body": "passed one", "state": "passed", "evidence": "the proof" },
            { "body": "failed one", "state": "failed", "evidence": null },
            { "body": "open one", "state": "open", "evidence": null },
        ],
    });
    let out = task_detail(&ctx, &t, crate::clock::now());
    assert!(out.contains("[x] passed one"), "{out}");
    assert!(out.contains("[!] failed one"), "{out}");
    assert!(out.contains("[ ] open one"), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let at = lines.iter().position(|l| l.contains("passed one")).unwrap();
    assert!(
        lines[at + 1].contains("the proof"),
        "the citation sits under its claim: {out}"
    );
}

/// #714: on a terminal the card paired short facts two to a line, and a
/// check went through the same pairing — so `rev 1` shared its line with
/// `[x] Every renamed…` and short checks filled the left column. Checks,
/// their evidence and the notes print below the facts, one per line, at
/// every width.
#[test]
fn the_card_prints_checks_and_notes_below_the_facts_one_per_line() {
    let t = json!({
        "short_id": 51, "title": "Rename the symbols", "status": "pending",
        "priority": "M", "project": "tasqx", "urgency": 4.3,
        "created": "2026-09-01T10:00:00Z", "modified": "2026-09-02T10:00:00Z",
        "_rev": 1,
        "checks": [
            { "body": "Every renamed symbol has a row in the table", "state": "passed", "evidence": "the proof" },
            { "body": "ok", "state": "open", "evidence": null },
            { "body": "tests", "state": "failed", "evidence": null },
            { "body": "first\nsecond", "state": "open", "evidence": null },
            { "body": "", "state": "open", "evidence": null },
        ],
        "annotations": [{ "body": "a note" }],
    });
    for cols in [80, 94, 120, 200] {
        let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(cols);
        let out = task_detail(&ctx, &t, crate::clock::now());
        let body: Vec<&str> = out
            .lines()
            .map(|l| l.trim_start_matches('▌').trim_start())
            .collect();
        let rev = body.iter().position(|l| l.starts_with("rev")).unwrap();
        for needle in [
            "[x] Every renamed symbol has a row in the table",
            "[ ] ok",
            "[!] tests",
            "the proof",
            "· a note",
        ] {
            let at = body.iter().position(|l| l.contains(needle));
            let at = at.unwrap_or_else(|| panic!("{needle:?} missing at {cols}:\n{out}"));
            assert!(
                at > rev && body[at].trim_start().starts_with(needle),
                "{needle:?} not on its own line below the facts at {cols} cols:\n{out}"
            );
        }
        // PR #78 review: a check body keeps its line breaks, as the
        // plain layout does, and an empty one still shows its marker.
        let second = body.iter().position(|l| *l == "second");
        assert!(
            second.is_some_and(|i| body[i - 1] == "[ ] first"),
            "a multi-line check body lost its break at {cols} cols:\n{out}"
        );
        assert!(
            body.iter().filter(|l| l.trim_end() == "[ ]").count() == 1,
            "an empty check lost its marker at {cols} cols:\n{out}"
        );
    }
}

/// A task with no criteria gets no marker lines at all — the same rule the
/// budget row follows.
#[test]
fn a_task_without_criteria_renders_no_check_lines() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let t = json!({ "short_id": 1, "title": "t", "status": "pending", "checks": [] });
    let out = task_detail(&ctx, &t, crate::clock::now());
    for marker in ["[x]", "[!]", "[ ]"] {
        assert!(
            !out.contains(marker),
            "{marker} printed over nothing: {out}"
        );
    }
}

/// D139: the pair renders as one row, and only when a threshold was set —
/// `fresh_tokens` alone is a number with nothing to read it against, and
/// the TOKENS row already reports the spend.
#[test]
fn a_budget_renders_as_one_row_and_only_when_it_exists() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let with = json!({
        "short_id": 1, "title": "t", "status": "pending",
        "budget_tokens": 1000, "fresh_tokens": 1300, "over": true,
    });
    let out = task_detail(&ctx, &with, crate::clock::now());
    assert!(out.contains("1.3K / 1.0K fresh"), "{out}");
    assert!(out.contains("over"), "the verdict is on the row: {out}");

    let without = json!({
        "short_id": 1, "title": "t", "status": "pending",
        "budget_tokens": null, "fresh_tokens": 1300, "over": null,
    });
    let out = task_detail(&ctx, &without, crate::clock::now());
    assert!(
        !out.contains("budget"),
        "no threshold, no row — 1300 against nothing says nothing: {out}"
    );
}

/// The gauge must not be readable as a bill: cache reads are excluded from
/// it, so the row it prints and the TOKENS row are different numbers on
/// purpose and both have to be legible at once.
#[test]
fn the_budget_row_and_the_token_row_coexist() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let t = json!({
        "short_id": 1, "title": "t", "status": "pending",
        "budget_tokens": 1000, "fresh_tokens": 1300, "over": true,
        "tokens": [{
            "input_tokens": 900, "output_tokens": 400,
            "cache_read_tokens": 500_000, "cache_creation_tokens": 0,
            "confidence": "medium", "tool": "claude-code", "source": "self-report"
        }],
    });
    let out = task_detail(&ctx, &t, crate::clock::now());
    assert!(out.contains("1.3K / 1.0K fresh"), "the gauge: {out}");
    assert!(
        out.contains("cacheR 500.0K") || out.contains("cacheR 500000"),
        "and the spend, undiminished by the gauge's definition: {out}"
    );
}

/// The brief's reason for existing, on the screen: the prerequisite's own
/// last word, under the prerequisite.
#[test]
fn a_brief_prints_what_each_prerequisite_concluded() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "task": { "short_id": 2, "title": "Audit the retry path", "status": "pending" },
        "neighbourhood": {
            "depends_on": [{
                "short_id": 1, "title": "Freeze the envelope", "status": "done",
                "annotation": { "body": "the key is the request id, never the order id",
                                "created": "2026-09-01T00:00:00Z" }
            }],
            "blocks": [{ "short_id": 3, "title": "Ship the notes", "status": "pending" }],
        },
        "memory": { "hits": [
            { "title": "Idempotency", "source": "docs/adr/014.md",
              "snippet": "retries must not double-charge" }
        ], "total": 1 },
    });
    let out = task_brief(&ctx, &result, crate::clock::now());
    assert!(out.contains("DEPENDS ON"), "{out}");
    assert!(
        out.contains("the key is the request id, never the order id"),
        "the prerequisite's conclusion is the row this section exists for: {out}"
    );
    assert!(
        out.contains("BLOCKS") && out.contains("Ship the notes"),
        "{out}"
    );
    assert!(
        out.contains("FROM MEMORY") && out.contains("Idempotency"),
        "{out}"
    );
}

/// D170: a recurrence spawn's brief quotes what the previous occurrence
/// delivered, and its detail names that occurrence on the repeats row.
#[test]
fn a_spawns_brief_prints_last_times_delivery_and_its_predecessor() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "task": { "short_id": 625, "title": "ledger", "status": "pending",
                  "recurrence": "every week", "spawned_from": 604 },
        "neighbourhood": { "depends_on": [], "blocks": [] },
        "memory": { "hits": [], "total": 0 },
        "last_time": { "short_id": 604, "title": "ledger",
                       "completed": "2026-09-14T08:00:00Z",
                       "delivered": "Balanced to the cent." },
    });
    let out = task_brief(&ctx, &result, crate::clock::now());
    assert!(out.contains("LAST TIME"), "{out}");
    assert!(out.contains("Balanced to the cent."), "{out}");
    assert!(out.contains("every week from #604"), "{out}");
}

/// An empty section is omitted, not printed with nothing under it: a brief
/// is read before work starts and a heading that says nothing costs
/// attention at the worst moment (house style rule 12).
#[test]
fn a_brief_with_no_neighbours_prints_no_empty_headings() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "task": { "short_id": 1, "title": "alone", "status": "pending" },
        "neighbourhood": { "depends_on": [], "blocks": [] },
        "memory": { "hits": [], "total": 0 },
    });
    let out = task_brief(&ctx, &result, crate::clock::now());
    for heading in ["DEPENDS ON", "BLOCKS", "FROM MEMORY"] {
        assert!(
            !out.contains(heading),
            "{heading} printed over nothing: {out}"
        );
    }
}

/// D70's rule one surface over: a bounded page that does not say it is
/// bounded is read as the whole answer.
#[test]
fn a_brief_says_when_it_withheld_memory_hits() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "task": { "short_id": 1, "title": "t", "status": "pending" },
        "neighbourhood": { "depends_on": [], "blocks": [] },
        "memory": { "hits": [{ "title": "one", "snippet": "s" }], "total": 9 },
    });
    let out = task_brief(&ctx, &result, crate::clock::now());
    assert!(out.contains("1 of 9 matches shown"), "{out}");
}

/// #346: the prose `next`, `agenda`, `projects` and `report` print wraps at
/// words to the terminal. The onboarding hint was one 100-cell line and the
/// agenda's undated note 95, so a 60-column terminal broke both mid-word.
#[test]
fn prose_under_a_screen_wraps_at_the_terminal_width() {
    use unicode_width::UnicodeWidthStr;
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
    let empty = json!({ "tasks": [], "projects": [], "groups": [], "store_empty": true });
    let payload = agenda_payload(vec![
        dated(1, "on a day", "2026-08-05T00:00:00Z", ""),
        dated(2, "no dates at all", "", ""),
        dated(3, "far out", "2026-12-24T00:00:00Z", ""),
    ]);
    let mut over = Vec::new();
    for (name, out) in [
        ("next", next_task(&ctx, &empty, anchor())),
        ("projects", project_table(&ctx, &empty)),
        ("report", report(&ctx, &empty, "project", None)),
        ("agenda", agenda_text(&ctx, &agenda_of(&payload, 14))),
    ] {
        assert!(!out.trim().is_empty(), "{name} printed nothing");
        for l in out.lines().filter(|l| l.width() > 60) {
            over.push(format!("{name} ({}): {l}", l.width()));
        }
    }
    assert!(over.is_empty(), "wider than 60:\n{}", over.join("\n"));
}

/// #346: a command quoted in a note is not broken across two lines. The
/// first wrap of the agenda's undated note at 80 columns ended one line on
/// `` `tasqx `` and began the next on `` list` shows them ``, which is a
/// command a reader cannot paste.
#[test]
fn prose_never_breaks_a_quoted_command() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(80);
    let note = "7 undated — no due or scheduled date, so nothing puts them on a day; \
                    `tasqx list` shows them";
    let out = prose(&ctx, None, note, "");
    assert!(out.lines().count() > 1, "the fixture must wrap: {out:?}");
    for l in out.lines() {
        assert_eq!(
            l.matches('`').count() % 2,
            0,
            "a quoted command was split: {out:?}"
        );
    }
    assert_eq!(
        out.split_whitespace().collect::<Vec<_>>().join(" "),
        note,
        "the words changed: {out:?}"
    );
}

/// #346 review: a stray backtick does not turn the rest of a note into one
/// word. The unrecognized-status note embeds raw store text and the miss
/// hint the query as typed, so either can carry one.
#[test]
fn prose_holds_only_paired_backticks() {
    use unicode_width::UnicodeWidthStr;
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
    let note = "every term was required: \"it`s\" and then enough further words \
                    that the note cannot fit on one sixty-column line";
    let out = prose(&ctx, None, note, "");
    for l in out.lines() {
        assert!(l.width() <= 60, "{} cells: {l:?}\n{out}", l.width());
    }
}

/// Rule 9, through the one fitter since round 2 of #346 (`keep_ranked`):
/// the summary drops facts from the right until it fits, and the first
/// fact, the count, is kept whatever the width.
#[test]
fn the_summary_drops_facts_from_the_right() {
    // 40 is the narrowest width a Ctx takes (`Ctx::MIN_COLS`).
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(40);
    let parts = vec![
        ("card.strong", "44 tasks".to_string()),
        ("muted", "20 shown".to_string()),
        ("timer.active", "#49 #50 #51 running".to_string()),
        ("muted", "2 blocked".to_string()),
    ];
    // `2 blocked` would fit after the running fact is dropped; it goes
    // anyway, because a fact never outlives one ranked above it.
    let mid = ctx.mid().to_string();
    assert_eq!(
        summary_line(&ctx, None, parts),
        format!("44 tasks {mid} 20 shown")
    );
}
