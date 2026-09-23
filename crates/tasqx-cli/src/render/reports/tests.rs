use super::*;
use crate::render::test_support::*;
use crate::theme::{self, Caps, Ctx};
use serde_json::json;

/// A completion that DID self-report (or a plain response with no hint
/// key at all) must not grow a hint line from nothing, and the variant
/// that asks nothing of the reader prints nothing on a terminal.
#[test]
fn done_omits_the_tokens_hint_line_when_the_response_has_none() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "short_id": 1, "title": "t",
        "status": "done",
        "completed": "2026-07-31T10:00:00Z",
        "unblocked": [],
    });
    let out = done(&ctx, &result, &result, &Titles::new(), crate::clock::now());
    assert!(
        !out.contains("tokens_hint") && !out.contains("self-reported"),
        "a hint appeared where the response carried none: {out:?}"
    );
    assert_eq!(
        tokens_note(
            "a self-report already covers this task; nothing further",
            200,
            true
        ),
        None
    );
}

/// #233.1: `projects` on a fresh store, same treatment as `list`.
#[test]
fn project_table_on_a_genuinely_empty_store_names_the_onboarding_commands() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let empty_store = json!({ "projects": [], "count": 0, "store_empty": true });
    assert!(project_table(&ctx, &empty_store).contains("tasqx init"));

    let none_archived = json!({ "projects": [], "count": 0, "store_empty": false });
    assert_eq!(project_table(&ctx, &none_archived), "No projects.\n");
}

/// #233.1: `report` on a fresh store, same treatment as `list`.
#[test]
fn report_on_a_genuinely_empty_store_names_the_onboarding_commands() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let empty_store = json!({ "groups": [], "store_empty": true });
    assert!(report(&ctx, &empty_store, "project", None).contains("tasqx add"));

    let filtered_empty = json!({ "groups": [], "store_empty": false });
    assert_eq!(
        report(&ctx, &filtered_empty, "project", None),
        "No matching tasks.\n"
    );
}

/// The same rule one verb over: archiving the default MOVES where a bare
/// `tasqx add` lands, so the line may not be the same either way.
///
/// D22 reserved this copy in writing ("when one lands it renders
/// `default_cleared`") while `project.archive` had no CLI verb at all. The
/// failure this pins is the cheap one: render the name, drop the field, and
/// the user reads "Project work archived" while their default silently
/// became nothing.
#[test]
fn project_archived_says_when_it_cleared_the_default() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

    let cleared = project_archived(
        &ctx,
        &json!({ "name": "work", "archived": true, "default_cleared": true }),
    );
    assert!(cleared.contains("work"), "missing the project: {cleared:?}");
    assert!(
        cleared.contains("default project") && !cleared.contains("unchanged"),
        "the default moved and the line does not say so: {cleared:?}"
    );
    // And it must name the way back, exactly as `project_created` does when
    // it declines to claim the default: a store with no default is valid,
    // being unable to leave that state is not.
    assert!(
        cleared.contains("tasqx use"),
        "must name the verb that re-points the default: {cleared:?}"
    );

    let kept = project_archived(
        &ctx,
        &json!({ "name": "side", "archived": true, "default_cleared": false }),
    );
    assert!(kept.contains("side"));
    assert!(
        !kept.contains("no home"),
        "invented a default change that did not happen: {kept:?}"
    );
    // The two outcomes must be DISTINGUISHABLE, not merely different in
    // what they omit — "Project side archived" on its own is also the line
    // a cleared default would print if the field were dropped.
    assert_ne!(
        kept.replace("side", "work"),
        cleared,
        "the cleared and untouched cases print the same line"
    );
    assert!(
        kept.contains("unchanged"),
        "the untouched case must say the default is untouched: {kept:?}"
    );
}

/// D89/#161: archiving a project with open work said NOTHING about it —
/// "Project acme archived  ·  your default project is unchanged" was the
/// whole line for a project holding an overdue, high-priority task, and
/// the two tasks stayed fully visible in `list`, `agenda` and `report`
/// while `tasqx projects` (without `--all`) stopped mentioning acme at
/// all. The one moment a user is thinking about this project is the one
/// line that must say what got left behind.
#[test]
fn project_archived_says_what_open_work_it_leaves_behind() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

    let with_overdue = project_archived(
        &ctx,
        &json!({
            "name": "acme", "archived": true, "default_cleared": false,
            "open_tasks": 2, "open_overdue": 1,
        }),
    );
    assert!(
        with_overdue.contains("2 open tasks"),
        "must count the tasks left behind: {with_overdue:?}"
    );
    assert!(
        with_overdue.contains("1 overdue"),
        "must call out how many of them are overdue: {with_overdue:?}"
    );
    assert!(
        with_overdue.contains("tasqx list project:acme"),
        "must name how to find them: {with_overdue:?}"
    );
    // The default-project fact from D22 must survive alongside the new one.
    assert!(
        with_overdue.contains("unchanged"),
        "must not drop the pre-existing default-project fact: {with_overdue:?}"
    );

    // No overdue among the open tasks: no false "(0 overdue)".
    let no_overdue = project_archived(
        &ctx,
        &json!({
            "name": "acme", "archived": true, "default_cleared": false,
            "open_tasks": 1, "open_overdue": 0,
        }),
    );
    assert!(no_overdue.contains("1 open task"));
    assert!(!no_overdue.contains("open task,"));
    assert!(
        !no_overdue.contains("overdue"),
        "zero overdue must not be printed as a fact: {no_overdue:?}"
    );
    // The number must agree with its noun: "1 open tasks" is not English.
    // Assert the exact clause in both numbers, not a substring a mismatch
    // would still satisfy. (D126 spells it `left in it`, which has no verb
    // to disagree; it was `remain(s)`.)
    assert!(
        no_overdue.contains("1 open task left in it"),
        "singular: {no_overdue:?}"
    );
    assert!(
        with_overdue.contains("2 open tasks left in it, 1 overdue"),
        "plural: {with_overdue:?}"
    );

    // Nothing left behind: the line is exactly what it was before D89,
    // unchanged — no empty "0 open tasks" clause invented.
    let clean = project_archived(
        &ctx,
        &json!({
            "name": "acme", "archived": true, "default_cleared": false,
            "open_tasks": 0, "open_overdue": 0,
        }),
    );
    assert!(
        !clean.contains("open task"),
        "nothing was left behind, so nothing should be claimed: {clean:?}"
    );
    assert!(clean.contains("unchanged"));
}

/// The invisible-field trap: `projects` is the read surface for the default,
/// so the table must mark it.
#[test]
fn project_table_marks_the_default_project() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = project_table(
        &ctx,
        &json!({
            "count": 2,
            "projects": [
                { "name": "prive.klussen", "archived": false, "default": false, "description": "" },
                { "name": "work", "archived": false, "default": true, "description": "" },
            ]
        }),
    );
    let work_line = out.lines().find(|l| l.contains("work")).expect("work row");
    let other_line = out
        .lines()
        .find(|l| l.contains("prive.klussen"))
        .expect("other row");
    assert!(
        work_line.contains('*'),
        "the default row is unmarked: {work_line:?}"
    );
    assert!(
        !other_line.contains('*'),
        "a non-default row is marked: {other_line:?}"
    );
}

/// #346: the default is marked in a two-cell rail, the way `git branch`
/// marks the checked-out branch, and archived is a word on the rows it is
/// true of (`docs/maintainers/terminal-style.md` rules 2 and 4).
///
/// The table spent a seven-cell DEFAULT column on one `*`, and an
/// eight-cell ARCHIVED column that, without `--all`, could only ever say
/// `no` on every row.
#[test]
fn project_table_spends_a_rail_on_the_default_and_a_word_on_archived() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let row = |name: &str, default: bool, archived: bool| json!({ "name": name, "archived": archived, "default": default, "description": "" });
    let out = project_table(
        &ctx,
        &json!({ "projects": [row("home", false, false), row("work", true, false)] }),
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "  PROJECT", "{out}");
    assert_eq!(lines[1], "  home", "{out}");
    assert_eq!(lines[2], "* work", "{out}");

    let out = project_table(
        &ctx,
        &json!({ "projects": [row("old", false, true), row("work", true, false)] }),
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "  PROJECT  STATUS", "{out}");
    assert_eq!(lines[1], "  old      archived", "{out}");
    assert_eq!(lines[2], "* work", "{out}");
    assert!(!out.contains(" no"), "{out}");
}

/// #346: the TOTAL row is a row with a label, not a title. It was painted
/// in `header`, the role for `TASQX MANUAL` and a task's own name, so the
/// sums competed with the rows they sum (`docs/maintainers/terminal-style.md`
/// rule 12). The label takes `table.label` like the column labels, and the
/// figures print at the terminal's own foreground.
#[test]
fn report_total_row_is_labelled_not_titled() {
    let ctx = Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    );
    let groups = json!({ "groups": [
            { "project": "a", "count": 3, "est_total": "PT1H", "overdue": 1, "tracked_total": "PT0S" },
            { "project": "b", "count": 5, "est_total": "PT2H", "overdue": 0, "tracked_total": "PT0S" },
        ] });
    let out = report(&ctx, &groups, "project", None);
    let total = out.lines().find(|l| l.contains("TOTAL")).expect("TOTAL");
    let header_sgr = ctx.paint("header", "\u{0}");
    let header_sgr = header_sgr.split('\u{0}').next().expect("an SGR prefix");
    assert!(!header_sgr.is_empty(), "the test ctx paints nothing");
    assert!(
        !total.contains(header_sgr),
        "TOTAL is still painted as a title: {total:?}"
    );
    assert!(
        total.contains(&ctx.paint("table.label", "TOTAL")),
        "the TOTAL label is not a table label: {total:?}"
    );
    assert!(total.contains(" 8 "), "the count total: {total:?}");
    // Separated by whitespace, not by weight alone (rule 7): in `mono`
    // and under NO_COLOR a dim label is all that told TOTAL from a group
    // named in capitals.
    let lines: Vec<&str> = out.lines().collect();
    let at = lines.iter().position(|l| l.contains("TOTAL")).unwrap();
    assert_eq!(lines[at - 1], "", "TOTAL runs flush under the rows: {out}");
}

/// #346: prose after the table gets a blank line (rule 7), and counts are
/// spelled `1 cancelled task` / `2 cancelled tasks`, never `task(s)`
/// (rule 8).
#[test]
fn report_footnotes_stand_off_the_table_and_count_in_words() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let groups = |n: i64| {
        json!({ "tokens_excluded_cancelled_tasks": n, "groups": [
                { "project": "a", "count": 3, "est_total": "PT1H", "overdue": 0, "tracked_total": "PT0S" },
            ] })
    };
    let out = report(&ctx, &groups(1), "project", None);
    let lines: Vec<&str> = out.lines().collect();
    let total = lines.iter().position(|l| l.contains("TOTAL")).unwrap();
    assert_eq!(lines[total + 1], "", "no air between table and note: {out}");
    assert!(
        lines[total + 2].starts_with("1 cancelled task excluded"),
        "{out}"
    );
    let out = report(&ctx, &groups(2), "project", None);
    assert!(out.contains("2 cancelled tasks excluded"), "{out}");
    assert!(!out.contains("task(s)"), "{out}");
}

/// D137's denominator rule, made typographic. `3/12` and `3/3` are the same
/// percentage and very different news, so the cell carries the `n` it was
/// computed against and no cell anywhere prints a `%`.
#[test]
fn outcome_rates_print_over_their_denominator_and_never_as_a_percentage() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({ "groups": [{
            "project": "work",
            "closed": 4, "completions": 3,
            "rework": { "count": 1, "n": 3, "rate": 0.333, "refs": [2] },
            "forced": { "count": 1, "n": 3, "rate": 0.333, "refs": [5] },
            "silent": { "count": 2, "n": 3, "rate": 0.667, "refs": [2, 3] },
            "calibration": { "median_ratio": 1.5, "n": 2 },
            "abandonment": { "count": 1, "n": 4, "rate": 0.25, "tracked_total": "PT20M", "refs": [4] },
            "cost": { "tokens_in": 1, "tokens_out": 2, "tokens_cache_read": 900, "tokens_cache_creation": 3, "n": 1 },
        }] });
    let out = outcomes(&ctx, &result, "project");
    assert!(out.contains("1/3"), "rework reads count over n: {out}");
    assert!(out.contains("FORCED"), "forced gets its own column: {out}");
    assert!(out.contains("2/3"), "silent reads count over n: {out}");
    assert!(out.contains("1/4"), "abandonment reads count over n: {out}");
    assert!(
        out.contains("×1.50 n2"),
        "calibration carries its sample size: {out}"
    );
    assert!(
        !out.contains('%'),
        "a percentage drops the denominator D137 requires: {out}"
    );
    // D48/D50/D103: the cell names one bucket, never a blend.
    assert!(out.contains("cacheR"), "{out}");
}

/// A rate with nothing to divide by prints `-`, not `0/0`: "none of them"
/// and "there were none" are different answers.
#[test]
fn an_outcome_rate_with_no_denominator_prints_a_dash() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({ "groups": [{
            "project": "work",
            "closed": 1, "completions": 0,
            "rework": { "count": 0, "n": 0, "rate": null, "refs": [] },
            "calibration": { "median_ratio": null, "n": 0 },
        }] });
    let out = outcomes(&ctx, &result, "project");
    assert!(!out.contains("0/0"), "{out}");
    assert!(out.contains('-'), "{out}");
}

/// An empty outcomes report may not say "no matching tasks": the scope test
/// is that a task CLOSED, so a perfectly matching open backlog lands here
/// and that wording would send the reader hunting a filter bug.
#[test]
fn an_empty_outcomes_report_says_the_scope_is_closed_work() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = outcomes(
        &ctx,
        &json!({ "groups": [], "store_empty": false }),
        "project",
    );
    assert!(out.contains("No closed work in scope"), "{out}");
    assert!(!out.contains("No matching tasks"), "{out}");
}

/// Round 1 review of #346: the `*` rail is not drawn when no row is in
/// effect, so a table with no default spends no cells on it (rule 4).
#[test]
fn the_current_rail_is_not_drawn_when_nothing_is_in_effect() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = project_table(
        &ctx,
        &json!({ "projects": [
                { "name": "home", "archived": false, "default": false, "description": "" },
            ] }),
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, ["PROJECT", "home"], "{out}");
}

/// `projects` and `report` pad a user-authored string into a column too, so
/// the rule is theirs as well — fixing only `list` would leave two tables
/// with the old bug and no test able to see it.
#[test]
fn project_and_report_tables_hold_their_widths_in_cells() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

    let projects: Vec<Value> = AWKWARD
        .iter()
        .map(|n| json!({ "name": n, "archived": false, "default": false, "description": "d" }))
        .collect();
    let out = project_table(
        &ctx,
        &json!({ "count": projects.len(), "projects": projects }),
    );
    let rows: Vec<&str> = out.lines().skip(1).collect();
    let want = cells(rows[0]);
    for (row, n) in rows.iter().zip(AWKWARD) {
        assert_eq!(cells(row), want, "project {n:?} broke alignment: {row:?}");
    }

    let groups: Vec<Value> = AWKWARD
        .iter()
        .map(|k| {
            json!({ "project": k, "count": 1, "est_total": "PT1H", "overdue": 0,
                             "tracked_total": "PT2H", "tokens_total": 123456 })
        })
        .collect();
    let out = report(&ctx, &json!({ "groups": groups }), "project", None);
    let rows: Vec<&str> = out.lines().skip(1).collect();
    let want = cells(rows[0]);
    for (row, k) in rows.iter().zip(AWKWARD) {
        assert_eq!(
            cells(row),
            want,
            "report group {k:?} broke alignment: {row:?}"
        );
    }
}

/// D48a: the TOKENS column names the largest bucket, and the blend it used to
/// print is gone from this surface.
///
/// This test replaces `report_shows_a_tokens_total_column`, which asserted
/// the opposite and was correct until D48. The fixture's `tokens_total` is
/// deliberately present and deliberately unrendered: a store carrying the
/// field is exactly the case where the old behaviour could creep back, and a
/// fixture that omitted it could not tell the difference. Since D50 the
/// engine no longer emits the field at all; the fixture stands in for a
/// pre-D50 payload, so keep it.
#[test]
fn report_names_the_largest_bucket_instead_of_blending() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = report(
        &ctx,
        &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H",
                  "tokens_in": 136, "tokens_out": 83_479,
                  "tokens_cache_read": 13_630_240, "tokens_cache_creation": 186_965,
                  "tokens_total": 13_900_820 }
            ] }),
        "project",
        None,
    );
    assert!(out.contains("TOKENS"), "TOKENS header missing: {out:?}");
    let row = out.lines().nth(1).unwrap();
    assert!(
        row.contains("cacheR 13.6M"),
        "the dominant bucket is not named: {row:?}"
    );
    assert!(
        !row.contains("13900820") && !row.contains("13.9M"),
        "the blended total reached the terminal: {row:?}"
    );
}

/// #352: a narrow terminal never hides a bucket that was asked for.
///
/// On the shared fitter the four token columns were first made droppable,
/// and at 60 columns `report --metrics tokens_in` drew CACHER and CACHEW
/// and silently lost IN and OUT: the very bucket the flag named, and half
/// of D48(a)'s "four, never blended". A number is not a column to give way.
/// The key does, down to its floor, and past that the row overflows.
#[test]
fn a_narrow_report_keeps_every_bucket_it_was_asked_for() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
    let metrics = vec!["tokens_in".to_string()];
    let out = report(
        &ctx,
        &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H",
                  "tokens_in": 136, "tokens_out": 83_479,
                  "tokens_cache_read": 13_630_240, "tokens_cache_creation": 186_965 }
            ] }),
        "project",
        Some(&metrics),
    );
    let header = out.lines().next().unwrap();
    for (_, short, _) in crate::tokens::BUCKETS {
        assert!(
            header.contains(&short.to_uppercase()),
            "{short} was dropped at 60 columns: {header:?}"
        );
    }
}

/// #352: the report's footnotes wrap at word boundaries to the terminal.
///
/// The TOKENS legend is one 171-cell sentence, which a 60-column terminal
/// broke mid-word on its own, three times.
#[test]
fn a_narrow_reports_footnotes_wrap_at_words() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
    let out = report(
        &ctx,
        &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H", "tokens_cache_read": 13_630_240 }
            ], "tokens_excluded_cancelled_tasks": 3 }),
        "project",
        None,
    );
    assert!(out.contains("largest of four"), "no legend: {out}");
    assert!(out.contains("--all includes"), "no exclusion note: {out}");
    for line in out.lines() {
        assert!(width(line) <= 60, "{} cells at 60: {line:?}", width(line));
    }
}

/// #217: `report.summary` carries a `tokens_confidence` field alongside
/// the four buckets — the group's worst measurement — but the terminal
/// table only ever named the dominant bucket, so a project whose entire
/// spend came from a low-confidence discovery pass read identically to
/// one built from verified `otel` correlations.
#[test]
fn report_marks_a_low_confidence_group() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = report(
        &ctx,
        &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H", "tokens_in": 100, "tokens_out": 0,
                  "tokens_cache_read": 0, "tokens_cache_creation": 0,
                  "tokens_confidence": "low" }
            ] }),
        "project",
        None,
    );
    let row = out.lines().nth(1).unwrap();
    assert!(
        row.contains("low"),
        "the group's low confidence never reached the terminal row: {row:?}"
    );
}

/// A group with no measurement must read as "nothing to report", not as a
/// bucket that spent zero — the difference between an unmeasured project and
/// a free one.
#[test]
fn report_shows_a_dash_for_a_group_that_spent_no_tokens() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = report(
        &ctx,
        &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H" }
            ] }),
        "project",
        None,
    );
    let row = out.lines().nth(1).unwrap();
    assert!(row.trim_end().ends_with('-'), "expected a dash: {row:?}");
}

// ========================================================================
// Regression tests — tasqx audit 2026-09 (#212, #234)
// ========================================================================

/// #234 item 2: a project name past the old hardcoded 20-cell column must
/// not shift every following column, and every row (long name or short)
/// must hold the SAME width so a straight-down scan of COUNT/EST/OVERDUE/
/// TRACKED is possible — the exact repro from the field
/// (`code-review-2026-07` 19 cells, `eblinqx-claude-plugins` 22).
#[test]
fn report_column_does_not_shift_for_a_name_past_the_old_fixed_width() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let groups = json!({ "groups": [
            { "project": "code-review-2026-07", "count": 38, "est_total": "PT90H15M",
              "overdue": 0, "tracked_total": "PT0S" },
            { "project": "eblinqx-claude-plugins", "count": 3, "est_total": "PT7H",
              "overdue": 0, "tracked_total": "PT0S" },
            { "project": "fin-10034", "count": 1, "est_total": "PT0S",
              "overdue": 0, "tracked_total": "PT0S" },
        ] });
    let out = report(&ctx, &groups, "project", None);
    let rows: Vec<&str> = out.lines().skip(1).take(3).collect();
    // Every row's TOTAL display width must match: that is exactly "every
    // fixed-width column starts and ends in the same place", regardless
    // of how a right-justified number happens to sit inside its own
    // field (a 1-digit count naturally starts one cell later than a
    // 2-digit one in the SAME 5-cell field — that is correct alignment,
    // not a shift).
    let want = cells(rows[0]);
    for row in &rows {
        assert_eq!(
            cells(row),
            want,
            "a row's total width differs — the columns are not aligned: {row:?}\n{out}"
        );
    }
}

/// #234 item 11: the terminal used to print raw ISO-8601 (`PT5H53S`,
/// which reads at a glance as 5h53m and is actually 5h and 53 SECONDS —
/// an 87x error). It must use the same human duration the HTML report and
/// the dashboard already use, and `-` (matching the TOKENS column) rather
/// than `PT0S` for an absent value.
#[test]
fn report_prints_human_durations_not_iso8601() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let groups = json!({ "groups": [
            { "project": "p", "count": 1, "est_total": "PT1H30M", "overdue": 0,
              "tracked_total": "PT5H53S" },
        ] });
    let out = report(&ctx, &groups, "project", None);
    assert!(
        !out.contains("PT1H30M") && !out.contains("PT5H53S"),
        "raw ISO leaked: {out:?}"
    );
    assert!(out.contains("1h 30m"), "expected a human duration: {out:?}");
    assert!(
        out.contains("5h 53s") || out.contains("5h"),
        "expected a human duration for 5h53s: {out:?}"
    );

    let zero = json!({ "groups": [
            { "project": "p", "count": 1, "est_total": "PT0S", "overdue": 0, "tracked_total": "PT0S" },
        ] });
    let out = report(&ctx, &zero, "project", None);
    assert!(
        !out.contains("PT0S"),
        "PT0S leaked instead of a dash: {out:?}"
    );
}

/// #234 item 5: neither renderer had a totals row, forcing "how much
/// estimated work is on the board" to be summed by hand across every
/// group.
#[test]
fn report_carries_a_totals_row() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let groups = json!({ "groups": [
            { "project": "a", "count": 3, "est_total": "PT1H", "overdue": 1, "tracked_total": "PT30M" },
            { "project": "b", "count": 5, "est_total": "PT2H", "overdue": 0, "tracked_total": "PT1H" },
        ] });
    let out = report(&ctx, &groups, "project", None);
    assert!(out.contains("TOTAL"), "no totals row: {out:?}");
    let total_line = out.lines().find(|l| l.contains("TOTAL")).unwrap();
    assert!(
        total_line.contains('8'),
        "count total (3+5=8) missing: {total_line:?}"
    );
    assert!(
        total_line.contains('1'),
        "overdue total (1+0=1) missing: {total_line:?}"
    );
}

/// #212 (D48a, challenges-design): `--metrics` naming a token bucket must
/// show all four as their own columns, and the terminal must otherwise
/// carry a legend saying the single TOKENS cell is one bucket of four —
/// today nothing on the page says that, and it is the number a lead reads
/// weekly to ask "which project is burning the budget".
#[test]
fn report_metrics_flag_shows_all_four_buckets_and_dominant_mode_carries_a_legend() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let groups = json!({ "groups": [
            { "project": "budget-a", "count": 1, "est_total": "PT0S", "overdue": 0,
              "tracked_total": "PT0S", "tokens_in": 50_000, "tokens_out": 200_000,
              "tokens_cache_read": 300_000, "tokens_cache_creation": 40_000 },
        ] });

    let dominant = report(&ctx, &groups, "project", None);
    assert!(
        dominant.to_lowercase().contains("largest of four buckets"),
        "expected a legend explaining the TOKENS cell: {dominant:?}"
    );

    let metrics = vec!["tokens_in".to_string(), "tokens_out".to_string()];
    let all_four = report(&ctx, &groups, "project", Some(&metrics));
    for label in ["CACHER", "CACHEW", "IN", "OUT"] {
        assert!(
            all_four.contains(label),
            "expected a {label} column with --metrics: {all_four:?}"
        );
    }
    assert!(
        all_four.contains("200.0K") && all_four.contains("50.0K"),
        "expected the out/in counts as their own cells: {all_four:?}"
    );
}

/// D167: an unsplit total is its own column beside the four, and only
/// when some group carries one; the dominant cell names it as unsplit.
#[test]
fn report_shows_an_unsplit_total_apart_from_the_four_buckets() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let groups = json!({ "groups": [
            { "project": "late", "count": 1, "est_total": "PT0S", "overdue": 0,
              "tracked_total": "PT0S", "tokens_in": 0, "tokens_out": 0,
              "tokens_cache_read": 0, "tokens_cache_creation": 0,
              "tokens_unsplit": 37_898 },
        ] });
    let dominant = report(&ctx, &groups, "project", None);
    assert!(dominant.contains("unsplit 37.8K"), "{dominant:?}");

    let metrics = vec!["tokens_in".to_string()];
    let all = report(&ctx, &groups, "project", Some(&metrics));
    assert!(all.contains("UNSPLIT") && all.contains("37.8K"), "{all:?}");

    let mut split = groups.clone();
    split["groups"][0]["tokens_unsplit"] = json!(0);
    let all = report(&ctx, &split, "project", Some(&metrics));
    assert!(!all.contains("UNSPLIT"), "{all:?}");
}

/// #234 item 12: a cancelled task's spend must not vanish from the
/// default report with nothing saying it was excluded.
#[test]
fn report_footnotes_cancelled_spend_excluded_by_default() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "groups": [
            { "project": "budget-a", "count": 1, "est_total": "PT0S", "overdue": 0,
              "tracked_total": "PT0S" },
        ],
        "tokens_excluded_cancelled_tasks": 3,
    });
    let out = report(&ctx, &result, "project", None);
    assert!(
        out.contains('3') && out.to_lowercase().contains("cancelled"),
        "expected a footnote naming the 3 excluded cancelled tasks: {out:?}"
    );

    let clean = json!({
        "groups": [
            { "project": "budget-a", "count": 1, "est_total": "PT0S", "overdue": 0,
              "tracked_total": "PT0S" },
        ],
        "tokens_excluded_cancelled_tasks": 0,
    });
    let out = report(&ctx, &clean, "project", None);
    assert!(
        !out.to_lowercase().contains("cancelled"),
        "no footnote should print when nothing was excluded: {out:?}"
    );
}

/// D50: the recompute delta lists every CHANGED task with auditable raw
/// numbers, counts the unchanged ones instead of listing them, and names
/// `--apply` only while nothing has been written yet.
#[test]
fn tokens_recompute_lists_changes_and_names_apply_only_on_dry_run() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let b4 = |i: i64, o: i64| {
        json!({ "input_tokens": i, "output_tokens": o,
                    "cache_read_tokens": 0, "cache_creation_tokens": 0 })
    };
    let mut result = json!({
        "dry_run": true,
        "tasks": [
            { "task": 3, "action": "recomputed", "before": b4(1500, 2600), "after": b4(500, 600) },
            { "task": 4, "action": "downgraded", "before": b4(800, 900), "after": b4(800, 900) },
            { "task": 5, "action": "channel_conflict", "before": b4(70, 0), "after": serde_json::Value::Null },
            { "task": 6, "action": "unchanged", "before": b4(1, 1), "after": b4(1, 1) },
        ],
        // The engine's blended figure is still ignored by the renderer
        // (#219) — present here to prove the totals line does not read it.
        "totals": { "before": 7101, "after": 1101 },
    });

    let out = tokens_recompute(&ctx, &result);
    assert!(out.contains("#3"), "{out:?}");
    assert!(
        out.contains("in 1500->500") && out.contains("out 2600->600"),
        "raw auditable numbers, no compaction: {out:?}"
    );
    assert!(
        out.contains("confidence -> low"),
        "the downgrade must say what happens to the label: {out:?}"
    );
    assert!(
        out.contains("self-report"),
        "a channel conflict must say which measurement stands: {out:?}"
    );
    assert!(
        !out.contains("#6"),
        "an unchanged task earns no line of its own: {out:?}"
    );
    assert!(out.contains("1 unchanged"), "{out:?}");
    // #219: the totals line sums the four buckets across every task
    // (1500+800+70+1 in, 2600+900+0+1 out; 500+800+0+1 / 600+900+0+1
    // after) — never the engine's blended 7101 -> 1101 figure above.
    assert!(
        !out.contains("7101") && !out.contains("blended"),
        "the totals line must not carry the engine's blended figure: {out:?}"
    );
    assert!(
        out.contains("in 2371->1301") && out.contains("out 3501->1501"),
        "the totals line must sum the four buckets, not blend them: {out:?}"
    );
    assert!(
        out.contains("--apply"),
        "a dry-run must name the flag that makes it real: {out:?}"
    );

    result["dry_run"] = json!(false);
    let out = tokens_recompute(&ctx, &result);
    assert!(
        !out.contains("--apply"),
        "an applied run advertising --apply invites running it twice: {out:?}"
    );
    assert!(out.contains("applied"), "{out:?}");
}

/// #219: the totals line used to blend all four buckets into the single
/// aggregate `tokens.rs`'s own header exists to forbid, and the blend
/// cannot even detect the failure it is there to catch — a repair that
/// drops 200k OUTPUT tokens and adds 200k CACHE-READ tokens nets to the
/// same blended figure on both sides and reads as a no-op, while having
/// swapped the cheapest bucket for one of the most expensive.
#[test]
fn tokens_recompute_totals_never_blend_the_four_buckets() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let bucket = |i: i64, o: i64, cr: i64, cw: i64| {
        json!({ "input_tokens": i, "output_tokens": o,
                    "cache_read_tokens": cr, "cache_creation_tokens": cw })
    };
    let result = json!({
        "dry_run": true,
        "tasks": [
            {
                "task": 20,
                "action": "recomputed",
                "before": bucket(0, 200_000, 0, 0),
                "after": bucket(0, 0, 200_000, 0),
            },
        ],
        // The blended figure nets to the SAME number on both sides for
        // exactly this swap — the reading the totals line must not give.
        "totals": { "before": 200_000, "after": 200_000 },
    });

    let out = tokens_recompute(&ctx, &result);
    let totals_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("totals"))
        .unwrap_or_else(|| panic!("no totals line: {out:?}"));
    assert!(
        !totals_line.contains("blended"),
        "the totals line must not blend the four buckets: {totals_line:?}"
    );
    assert!(
        totals_line.contains("out 200000->0") && totals_line.contains("cacheR 0->200000"),
        "the totals line must show the real per-bucket change, not a blended no-op: \
             {totals_line:?}"
    );
}

/// Nothing in scope is an answer, not an empty table — and an action this
/// build has never heard of still reaches the reader, because this report
/// is the approval surface for a deletion.
#[test]
fn tokens_recompute_answers_when_empty_and_shows_unknown_actions() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = tokens_recompute(&ctx, &json!({ "dry_run": true, "tasks": [] }));
    assert!(out.contains("No log-parse measurements"), "{out:?}");

    let out = tokens_recompute(
        &ctx,
        &json!({
            "dry_run": true,
            "tasks": [ { "task": 9, "action": "quarantined",
                         "before": { "input_tokens": 5 }, "after": { "input_tokens": 5 } } ],
            "totals": { "before": 5, "after": 5 },
        }),
    );
    assert!(
        out.contains("quarantined") && out.contains("#9"),
        "an unknown action may not drop its task from the report: {out:?}"
    );
}
