use super::*;
use crate::render::test_support::*;
use crate::theme::{self, Caps, Ctx};
use crate::AGENDA_DEFAULT_DAYS;
use serde_json::json;

/// #233.1: an empty agenda on a fresh store gets the same hint, appended
/// after the footer that already states the horizon on every run.
#[test]
fn agenda_on_a_genuinely_empty_store_names_the_onboarding_commands() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let empty_store = json!({ "tasks": [], "store_empty": true });
    let out = agenda_text(&ctx, &agenda_select(&empty_store, 14, anchor()));
    assert!(out.contains("tasqx add"), "{out:?}");

    let filtered_empty = json!({ "tasks": [], "store_empty": false });
    let out2 = agenda_text(&ctx, &agenda_select(&filtered_empty, 14, anchor()));
    assert!(!out2.contains("tasqx add"), "{out2:?}");
}

/// The ordering rule, and the reason the view reads both fields.
///
/// A task scheduled for Tuesday with a deadline three weeks out comes
/// BEFORE a task due on Wednesday, because Tuesday is the first day it asks
/// anything of you. Reading `due` alone would sort it last and file it in
/// the wrong week; reading `scheduled` alone would lose the Wednesday
/// deadline entirely. Both halves are asserted here, plus the label, since
/// "Tuesday" means something different under each field.
#[test]
fn agenda_places_a_task_on_the_earlier_of_due_and_scheduled() {
    let payload = agenda_payload(vec![
        dated(1, "deadline only", "2026-08-05T00:00:00Z", ""),
        dated(
            2,
            "starts tuesday, due much later",
            "2026-08-24T00:00:00Z",
            "2026-08-04T00:00:00Z",
        ),
        dated(3, "scheduled only", "", "2026-08-10T00:00:00Z"),
    ]);
    let a = agenda_of(&payload, 30);
    let order: Vec<i64> = a
        .entries
        .iter()
        .map(|e| e.task["short_id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        order,
        vec![2, 1, 3],
        "the agenda instant is min(due, scheduled), so #2's Tuesday leads"
    );
    assert_eq!(
        a.entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![When::Scheduled, When::Due, When::Scheduled],
        "the row must name the field that placed it"
    );
}

/// When one instant carries both meanings the label is `due`: the deadline
/// is the more consequential reading, and a row that said `sched` would
/// send the reader looking for a deadline that is right there.
#[test]
fn a_task_due_and_scheduled_at_the_same_instant_is_labelled_due() {
    let payload = agenda_payload(vec![dated(
        1,
        "same instant",
        "2026-08-05T09:00:00Z",
        "2026-08-05T09:00:00Z",
    )]);
    let a = agenda_of(&payload, 30);
    assert_eq!(a.entries[0].kind, When::Due);
}

/// The rule that keeps this view from being a lie: a task the filter
/// matched and the agenda cannot place is COUNTED, not dropped. A deadline
/// that is not on the screen is indistinguishable from a deadline that does
/// not exist, and the undated are the largest group this view omits.
#[test]
fn an_undated_task_is_counted_and_the_way_to_see_it_is_named() {
    let out = agenda_out(
        vec![
            dated(1, "on a day", "2026-08-05T00:00:00Z", ""),
            dated(2, "no dates at all", "", ""),
            dated(3, "no dates either", "", ""),
        ],
        14,
    );
    assert!(
        !out.contains("no dates at all"),
        "an undated task has no day to sit on: {out}"
    );
    assert!(
        out.contains("2 undated"),
        "the count of what was left out must be on the screen: {out}"
    );
    assert!(
        out.contains("tasqx list"),
        "the note must name the view that does show them: {out}"
    );
}

/// The horizon half of the same rule, plus the part that makes it
/// actionable: the note names the exact `--days` that reaches the furthest
/// thing it is holding, so widening the window is a paste and not a guess.
#[test]
fn a_task_past_the_horizon_is_counted_with_the_days_that_would_reach_it() {
    let tasks = vec![
        dated(1, "inside", "2026-08-05T00:00:00Z", ""),
        dated(2, "just outside", "2026-08-18T00:00:00Z", ""),
        dated(3, "far outside", "2026-11-01T00:00:00Z", ""),
    ];
    let out = agenda_out(tasks.clone(), AGENDA_DEFAULT_DAYS);
    assert!(!out.contains("just outside"), "{out}");
    assert!(out.contains("2 further out"), "{out}");
    // 2026-08-03 -> 2026-11-01 is 90 days. Anything short of the real
    // distance is a note that sends the reader back for another guess.
    assert!(
        out.contains("--days 90"),
        "the note must name the window that reaches the furthest row: {out}"
    );
    // ...and the horizon really is a fortnight by default, stated on screen.
    assert!(out.contains("through 17 Aug (+14d)"), "{out}");

    // Raising it brings them in, which is the other half of the promise.
    let wide = agenda_out(tasks, 90);
    assert!(wide.contains("far outside"), "{wide}");
    assert!(
        !wide.contains("further out"),
        "nothing is beyond a horizon that reaches everything: {wide}"
    );
}

/// An agenda row does not repeat the day its own heading just named.
///
/// A deadline the store holds no time for, placed under `Tomorrow · Fri
/// 2026-08-04`, printed the bare word `due` — and three rows of it in a
/// column read as a column that failed to render rather than one saying
/// anything. `sched` still prints bare: being on a day because you meant to
/// START there is not the default reading of a row.
#[test]
fn an_agenda_row_does_not_repeat_the_day_its_heading_names() {
    let out = agenda_out(
        vec![
            dated(1, "no time on it", "2026-08-05T00:00:00Z", ""),
            dated(2, "planned for then", "", "2026-08-06T00:00:00Z"),
            dated(3, "at a real time", "2026-08-05T17:00:00Z", ""),
        ],
        14,
    );
    let row = |title: &str| {
        out.lines()
            .find(|l| l.contains(title))
            .unwrap_or_else(|| panic!("no row for {title:?}:\n{out}"))
            .to_string()
    };
    assert!(
        !row("no time on it").contains("due"),
        "the heading already said the day: {:?}",
        row("no time on it")
    );
    assert!(
        row("planned for then").contains("sched"),
        "`sched` is not the default reading and still says so: {:?}",
        row("planned for then")
    );
    assert!(
        row("at a real time").contains("due 17:00"),
        "a time the store holds is still shown: {:?}",
        row("at a real time")
    );
}

/// D131: `list` and `agenda` answer "how many are overdue" with one rule.
/// Found by rendering a store: a task due at 17:00 read `today 17:00` in
/// red with `2 overdue` in `list`, while `agenda` filed it under `Today`
/// and said `1 overdue` — `list` compared instants and `agenda` compared
/// days. A deadline with a time is late once it passes; a date-only one
/// (midnight UTC) is late once its day has.
#[test]
fn list_and_agenda_agree_on_what_is_overdue_today() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let payload = json!({
        "tasks": [
            dated(1, "missed this morning", "2026-08-03T08:00:00Z", ""),
            dated(2, "due today no time", "2026-08-03T00:00:00Z", ""),
            dated(3, "due yesterday no time", "2026-08-02T00:00:00Z", ""),
            dated(4, "due this evening", "2026-08-03T17:00:00Z", ""),
        ],
        "count": 4,
        "total": 4,
    });
    let listed = task_table(&ctx, &payload, anchor());
    let agenda = agenda_text(&ctx, &agenda_of(&payload, 14));
    let head = |out: &str| out.lines().next().unwrap_or_default().to_string();

    assert!(head(&listed).contains("2 overdue"), "list:\n{listed}");
    assert!(head(&agenda).contains("2 overdue"), "agenda:\n{agenda}");
    assert!(
        head(&listed).contains("2 due today"),
        "the date-only deadline is still due today:\n{listed}"
    );

    let lines: Vec<&str> = agenda.lines().collect();
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing:\n{agenda}"))
    };
    let today = at("Today");
    assert!(
        at("missed this morning") < today,
        "a deadline that passed this morning is in the Overdue group:\n{agenda}"
    );
    assert!(
        at("due today no time") > today,
        "a date-only deadline today stays under Today:\n{agenda}"
    );
}

/// Overdue rows lead the table, under ONE heading, and the horizon does not
/// apply to them: `--days 1` must not hide work that was due last month.
/// The cells in that group carry the date, because the heading cannot.
#[test]
fn overdue_rows_lead_the_table_and_ignore_the_horizon() {
    let out = agenda_out(
        vec![
            dated(1, "today thing", "2026-08-03T17:00:00Z", ""),
            dated(2, "late by a week", "2026-07-27T00:00:00Z", ""),
            dated(3, "late by a month", "2026-07-01T00:00:00Z", ""),
        ],
        1,
    );
    let lines: Vec<&str> = out.lines().collect();
    let overdue = lines
        .iter()
        .position(|l| l.trim() == "Overdue")
        .unwrap_or_else(|| panic!("no Overdue heading:\n{out}"));
    let today = lines
        .iter()
        .position(|l| l.starts_with("Today"))
        .unwrap_or_else(|| panic!("no Today heading:\n{out}"));
    assert!(overdue < today, "overdue comes first:\n{out}");
    assert_eq!(
        lines.iter().filter(|l| l.trim() == "Overdue").count(),
        1,
        "past days collapse into one heading:\n{out}"
    );
    assert!(out.contains("late by a month"), "{out}");
    // The day, not a bare `due`: how late it is is the whole content of
    // this group — spelled as `list` spells it (D133).
    assert!(out.contains("due 33d ago"), "{out}");
    assert!(out.contains("due 7d ago"), "{out}");
    // A time-of-day is shown only where the store has one. `2026-07-01` was
    // typed without a time and resolves to midnight, so printing `00:00`
    // would be a time nobody entered.
    assert!(!out.contains("00:00"), "{out}");
    assert!(out.contains("due 17:00"), "a real time is shown: {out}");
}

/// Today and tomorrow get their words, and every heading carries the date
/// anyway -- "Today" alone is the one label that means something else
/// tomorrow, and terminal output gets pasted into tickets. The date is
/// `list`'s calendar spelling, not ISO (D133), with the two-digit year
/// only once it leaves the current one.
#[test]
fn day_headings_name_the_relative_day_and_the_date() {
    let out = agenda_out(
        vec![
            dated(1, "a", "2026-08-03T00:00:00Z", ""),
            dated(2, "b", "2026-08-04T00:00:00Z", ""),
            dated(3, "c", "2026-08-06T00:00:00Z", ""),
            dated(4, "d", "2027-01-04T00:00:00Z", ""),
        ],
        200,
    );
    assert!(out.contains("\nToday · Mon 3 Aug\n"), "{out}");
    assert!(out.contains("\nTomorrow · Tue 4 Aug\n"), "{out}");
    assert!(out.contains("\nThu 6 Aug\n"), "{out}");
    assert!(out.contains("\nMon 4 Jan 27\n"), "{out}");
    assert!(
        out.starts_with("through 19 Feb 27 (+200d)"),
        "the horizon is spelled the same way: {out}"
    );
    assert!(
        !out.contains("2026-"),
        "no ISO date on the text agenda: {out}"
    );
    assert!(
        !out.contains("2027-"),
        "no ISO date on the text agenda: {out}"
    );
}

/// The Overdue group has no heading to carry a day, so its cells do — in
/// `list`'s words (D133). It used to print `due 2026-08-02` beside a
/// `list` that said `yesterday` for the same row.
#[test]
fn overdue_when_cells_speak_lists_calendar_words() {
    let out = agenda_out(
        vec![
            dated(1, "missed this morning", "2026-08-03T08:00:00Z", ""),
            dated(2, "due yesterday", "2026-08-02T00:00:00Z", ""),
            dated(3, "meant to start", "", "2026-08-01T00:00:00Z"),
        ],
        14,
    );
    let row = |title: &str| {
        out.lines()
            .find(|l| l.contains(title))
            .unwrap_or_else(|| panic!("{title:?} missing:\n{out}"))
            .to_string()
    };
    assert!(
        row("missed this morning").contains("due today 08:00"),
        "{out}"
    );
    assert!(row("due yesterday").contains("due yesterday"), "{out}");
    assert!(row("meant to start").contains("sched 2d ago"), "{out}");
    assert!(
        !out.contains("2026-"),
        "no ISO date on the text agenda: {out}"
    );
}

/// D133's guard: `list` and `agenda` spell the same day identically for
/// the same row, because both go through one spelling. Asserted as
/// agreement between the two renders rather than as either's text, so a
/// third spelling on either side reddens it.
#[test]
fn list_and_agenda_spell_the_same_day_the_same_way() {
    let ctx = || Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(200);
    let rows = [
        (1, "late by two days", "2026-08-01T00:00:00Z"),
        (2, "missed at eight", "2026-08-03T08:00:00Z"),
        (3, "on thursday", "2026-08-06T00:00:00Z"),
        (4, "in a fortnight", "2026-08-15T00:00:00Z"),
        (5, "next year", "2027-01-04T00:00:00Z"),
    ];
    let payload = json!({
        "tasks": rows.iter().map(|(id, t, d)| dated(*id, t, d, "")).collect::<Vec<_>>(),
        "count": rows.len(),
    });
    let listed = task_table(&ctx(), &payload, anchor());
    let agenda = agenda_text(&ctx(), &agenda_of(&payload, 200));
    let line_of = |out: &str, title: &str| {
        out.lines()
            .position(|l| l.contains(title))
            .unwrap_or_else(|| panic!("{title:?} missing:\n{out}"))
    };
    for (_, title, due) in rows {
        let spelled = due_cell(due.parse().unwrap(), anchor());
        let l = listed.lines().nth(line_of(&listed, title)).unwrap();
        assert!(
            l.contains(&spelled),
            "list spells {due} as {spelled:?}: {l:?}"
        );

        let agenda_lines: Vec<&str> = agenda.lines().collect();
        let at = line_of(&agenda, title);
        let row = agenda_lines[at];
        if row.contains("due ") {
            // An Overdue row carries the day in its own cell.
            assert!(
                row.contains(&format!("due {spelled}")),
                "agenda must spell {due} as list does ({spelled:?}):\n{agenda}"
            );
        } else {
            // A row under a day heading: the heading carries the day.
            let heading = agenda_lines[..at]
                .iter()
                .rev()
                .find(|l| !l.starts_with(' ') && !l.is_empty())
                .unwrap();
            assert!(
                heading.contains(spelled.as_str()),
                "agenda heading {heading:?} must carry list's {spelled:?}:\n{agenda}"
            );
        }
    }
}

/// One layout for every group, and it is the D51 one: the columns are
/// fitted once across all the rows, and nothing -- heading, row, rule or
/// count line -- overruns the terminal it was given. A second table layout
/// written for this view is exactly what this asserts is absent.
#[test]
fn the_agenda_fits_the_width_it_was_given_across_every_group() {
    for cols in [Ctx::MIN_COLS, 60, 80, Ctx::DEFAULT_COLS] {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols);
        let tasks: Vec<Value> = (1..=6)
            .map(|i| {
                json!({
                    "short_id": i, "urgency": 5.0, "priority": "M",
                    "title": "a title far too long to fit any of these budgets without a cut",
                    "project": "some.rather.long.project.name",
                    "tags": ["one", "two", "three", "four"],
                    "status": "pending",
                    "due": format!("2026-08-{:02}T17:00:00Z", i + 2),
                    "scheduled": Value::Null,
                })
            })
            .collect();
        // Nothing here is undated or beyond the horizon, so the whole
        // render is table: the omission notes are deliberately prose that
        // carries a command to paste, and cutting one would cut the way out.
        let out = agenda_text(
            &ctx,
            &agenda_select(&json!({ "tasks": tasks }), 14, anchor()),
        );
        // EVERY line, and no bookkeeping to say where the table stops:
        // D117 left it with no rules at all, and the summary line that
        // opens it is width-bounded like the rows are (facts are dropped
        // from the right until it fits, since a wrap there would put a line
        // between the header and the rows it labels). What used to be
        // exempt was the trailer, which is gone. The fixture has no
        // omission notes, so nothing here is prose.
        for line in out.lines() {
            assert!(
                cells(line) <= cols,
                "a {}-cell line in a {cols}-cell terminal: {line:?}",
                cells(line)
            );
        }
        assert!(
            !out.lines()
                .any(|l| { !l.is_empty() && l.chars().all(|c| c == '-' || c == '\u{2500}') }),
            "the table draws no rules any more: {out}"
        );
    }
}

/// `--json` and the table answer the same question. The raw `task.list`
/// result would have made `tasqx agenda --json | jq .tasks` count every
/// matching task -- horizon and undated rows included -- while the table
/// beside it showed one.
#[test]
fn agenda_json_holds_exactly_the_rows_the_table_drew() {
    let payload = agenda_payload(vec![
        dated(1, "shown", "2026-08-05T00:00:00Z", ""),
        dated(2, "beyond", "2026-11-01T00:00:00Z", ""),
        dated(3, "undated", "", ""),
    ]);
    let a = agenda_of(&payload, 14);
    let v = agenda_json(&a);
    assert_eq!(v["count"], json!(1));
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(v["tasks"][0]["short_id"], json!(1));
    // Every number the footer prints is also a field, so a script never has
    // to scrape the prose to learn what was left out.
    assert_eq!(v["agenda"]["undated"], json!(1));
    assert_eq!(v["agenda"]["beyond_horizon"], json!(1));
    assert_eq!(v["agenda"]["reach_days"], json!(90));
    assert_eq!(v["agenda"]["through"], json!("2026-08-17"));
    assert_eq!(v["agenda"]["days"], json!(14));
    // The ceiling too, read out of the constant rather than typed: without
    // it a script has no way to tell a `reach_days` it can pass to `--days`
    // from one the parser would refuse.
    assert_eq!(v["agenda"]["max_days"], json!(AGENDA_MAX_DAYS));
}

/// A row the horizon cut but no `--days` can reach: the footer must not hand
/// the reader a command the CLI refuses.
///
/// `--days` is bounded at `AGENDA_MAX_DAYS`, and the reach is a raw distance
/// to the furthest cut row, so a task due decades out produced ``tasqx
/// agenda --days 12204`` — pasted, that exits 2 with `12204 is not in
/// 1..=3650`, and the row was unreachable in this view by any window.
/// D53 rule 2 promises the count is unconditional and the advice actionable;
/// past the ceiling the actionable advice is a different view.
#[test]
fn a_row_no_window_can_reach_is_counted_without_quoting_a_refused_days() {
    // Comfortably past the ceiling from the 2026-08-03 anchor.
    let out = agenda_out(
        vec![dated(1, "retirement party", "2060-01-01T00:00:00Z", "")],
        AGENDA_DEFAULT_DAYS,
    );
    assert!(out.contains("1 further out"), "still counted: {out}");
    assert!(
        out.contains("tasqx list"),
        "the note must name a view that actually shows it: {out}"
    );

    // The real check, and it reads the ceiling rather than a literal: no
    // `--days N` in the footer may be one `window_parser` would refuse.
    for n in recommended_days(&out) {
        assert!(
            (1..=AGENDA_MAX_DAYS).contains(&n),
            "the footer recommended `--days {n}`, which the CLI refuses \
                 (1..={AGENDA_MAX_DAYS}): {out}"
        );
    }

    // ...and inside the ceiling the exact window is still quoted, so the
    // clamp did not buy the fix by making every note useless.
    let near = agenda_out(
        vec![dated(1, "far outside", "2026-11-01T00:00:00Z", "")],
        AGENDA_DEFAULT_DAYS,
    );
    assert_eq!(recommended_days(&near), vec![90], "{near}");
}

/// Every `--days N` the rendered footer tells the reader to run.
fn recommended_days(out: &str) -> Vec<usize> {
    out.split("--days ")
        .skip(1)
        .filter_map(|tail| {
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

/// Two tasks on the same instant keep the engine's `-urgency` ranking: the
/// sort is by the agenda instant alone and `sort_by_key` is stable, so the
/// order `task.list` returned survives a tie.
#[test]
fn tasks_at_the_same_instant_keep_the_urgency_order_they_arrived_in() {
    let mut hot = dated(1, "hot", "2026-08-05T09:00:00Z", "");
    hot["urgency"] = json!(20.0);
    let mut cold = dated(2, "cold", "2026-08-05T09:00:00Z", "");
    cold["urgency"] = json!(1.0);
    // As `task.list {sort:["-urgency"]}` would return them.
    let payload = agenda_payload(vec![hot, cold]);
    let a = agenda_of(&payload, 14);
    assert_eq!(
        a.entries
            .iter()
            .map(|e| e.task["short_id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

/// #182: `agenda`'s Overdue group has no horizon (rule 3), which on a
/// store with years of history means no cap either — the field report's
/// 10,000-task store printed 1903 rows before the first `Today` heading.
/// Reproduced at a size a unit test can afford: enough all-overdue rows
/// to exceed [`AGENDA_OVERDUE_CAP`], each ranked so the cut is checkable.
#[test]
fn agenda_caps_the_overdue_group_and_stops_naming_a_horizon_over_it() {
    let n = AGENDA_OVERDUE_CAP + 5;
    let tasks: Vec<Value> = (0..n)
        .map(|i| {
            let mut t = dated(
                i as i64 + 1,
                &format!("ancient #{i}"),
                "2023-01-01T00:00:00Z",
                "",
            );
            // Distinct urgency: the cap must keep the hottest N, and the
            // count below only proves a cap exists, not which end it cut.
            t["urgency"] = json!(100.0 - i as f64);
            t
        })
        .collect();
    let out = agenda_out(tasks, 14);
    let shown = out.matches("ancient #").count();
    assert_eq!(
        shown, AGENDA_OVERDUE_CAP,
        "the overdue group must stop at the cap: {out:?}"
    );
    assert!(
        out.contains("more overdue"),
        "the rows the cap held back must be counted and named, the same way \
             undated and beyond-horizon rows already are: {out:?}"
    );
    // D133: the oldest is spelled as `list` spells a date, the year kept
    // because it is not this one.
    assert!(
        out.contains("5 more overdue, oldest 1 Jan 23 "),
        "the footer spells its oldest day in list's words: {out:?}"
    );
    assert!(
        !out.contains("+14d"),
        "a set that is 100% overdue must not have a footer advertising a \
             horizon none of the visible rows are within: {out:?}"
    );
}
