use super::*;
use crate::render::test_support::*;
use crate::theme::{self, Caps, Ctx};
use crate::AGENDA_DEFAULT_DAYS;
use serde_json::json;

/// #234 item 10 (= bundle #228's tab-alignment item, same root cause,
/// D19): a raw TAB in a title survives `san` and expands to the next
/// 8-column stop in any real terminal, shifting every column to the
/// RIGHT of it on that one row — the exact misalignment D51 exists to
/// end. `html::esc` keeps tab deliberately (D19: "legitimate document
/// whitespace" — a `<table>` cell has no fixed-width grid to break), so
/// this is a `render::san`-only fix, not a second D19 sanitizer standard.
///
/// Dropping the tab outright (a prior fix's behaviour) is its own bug:
/// `"a\tb"` became `"ab"`, silently welding two words together — audit
/// #229 item 11 caught this as a regression on the rebased tree. The
/// Direction was explicit: map TAB (and newline, see the sibling test
/// below) to a single visible separator space, not delete it.
#[test]
fn san_replaces_tab_with_a_space_instead_of_deleting_it() {
    assert_eq!(
            san("tab\there"),
            "tab here",
            "a dropped tab welds two words together (#229 item 11), a table's column boundary must stay visible"
        );
    assert_eq!(
        san("bell\x07 and \x1b]0;PWNED\x07title"),
        "bell and ]0;PWNEDtitle",
        "other control bytes are still dropped outright, not spaced"
    );
}

/// D122: the add card is `show`'s rail in two lines, and it fits: every line
/// starts on the rail, no line runs past the terminal, and what the reader
/// typed comes back in `list`'s spelling rather than the store's. Since
/// D126 the second line's rail cell carries the task's `⊘`/`▶` (this
/// fixture is blocked).
#[test]
fn the_add_card_is_two_rail_lines_in_lists_spelling() {
    let ctx = Ctx::new(theme::default_theme(), card_caps()).with_cols(80);
    let now: Timestamp = "2026-09-01T12:00:00Z".parse().unwrap();
    let out = added(&ctx, &full_task(), now);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "title line and facts line:\n{out}");
    for line in &lines {
        assert!(
            line.starts_with('▌') || line.starts_with('⊘'),
            "off the rail: {line:?}"
        );
        assert!(width(line) <= 80, "{} cells at 80: {line:?}", width(line));
    }
    assert!(
        lines[1].contains("due Fri 17:00") || lines[1].contains("due Fri"),
        "{out}"
    );
    assert!(lines[1].contains("est 4h"), "{out}");
    for iso in ["PT4H", "2026-09-04T17:00:00Z", "╭", "▲"] {
        assert!(!out.contains(iso), "{iso:?} in the add card:\n{out}");
    }

    let mut long = full_task();
    long["title"] = json!("x".repeat(300));
    let out = added(&ctx, &long, now);
    assert!(
        out.lines().all(|l| width(l) <= 80),
        "a long title ran past the terminal:\n{out}"
    );
}

/// B1: `tasqx list` prints no status column, so a row the store could not
/// read would flow through the default view looking like ordinary open work.
/// The core flags it; the table has to say so, and name the way out — the
/// value cannot be corrected in place, only exported, edited and imported.
/// The overdue flag is measured against the CALLER's instant — pinned on
/// both sides of the boundary for one stored row, which the internal
/// clock read this replaces made unschedulable.
#[test]
fn the_overdue_flag_flips_at_the_callers_instant_not_the_wall_clock() {
    let t = json!({
        "short_id": 1, "urgency": 1.0, "title": "x",
        "status": "pending", "due": "2026-08-31T12:00:00Z"
    });
    let at = |s: &str| s.parse::<Timestamp>().unwrap();
    assert!(!task_row(&t, at("2026-08-31T11:59:59Z"), true).overdue);
    assert!(task_row(&t, at("2026-08-31T12:00:01Z"), true).overdue);
}

/// #233.1: a genuinely fresh store (never held a task) must not answer
/// `list` with the same bare "No tasks." a filter that matched nothing
/// gets — the two are indistinguishable dead ends otherwise, and this is
/// every user's very first command.
#[test]
fn task_table_on_a_genuinely_empty_store_names_the_onboarding_commands() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let empty_store = json!({ "tasks": [], "count": 0, "total": 0, "store_empty": true });
    let out = task_table(&ctx, &empty_store, crate::clock::now());
    assert!(out.contains("tasqx init"), "{out:?}");
    assert!(out.contains("tasqx add"), "{out:?}");
    assert!(out.contains("tasqx manual"), "{out:?}");

    // A filter that matched nothing on a non-empty store keeps the plain
    // empty-result phrasing — the onboarding hint would be misleading
    // there. #229 item 1 aligned this wording with `report`'s.
    let filtered_empty = json!({ "tasks": [], "count": 0, "total": 0, "store_empty": false });
    let out2 = task_table(&ctx, &filtered_empty, crate::clock::now());
    assert_eq!(out2, "No matching tasks.\n");
}

/// #229 item 1: `task_table` (`list`, `watch`) said "No tasks." while
/// `report` said "No matching tasks." for the identical situation — an
/// empty result set from a read verb, filtered or not. One phrasing
/// across the read verbs, matching what `report` already used.
#[test]
fn an_empty_task_table_matches_reports_empty_phrasing() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let text = task_table(&ctx, &json!({ "tasks": [] }), crate::clock::now());
    assert_eq!(
        text, "No matching tasks.\n",
        "list/watch's empty phrasing must match report's: {text:?}"
    );
}

/// Finding #4 (audit-2026-09): `tasqx list +nosuchtag` answered only "No
/// tasks." — indistinguishable from "nothing is pending" — while D55
/// already drew this exact distinction for `pick` (DESIGN.md:1554): "the
/// empty-set one quotes the filter back". `task_table` itself is unchanged
/// for every caller with no single filter string to name.
#[test]
fn an_empty_list_with_a_filter_quotes_it_back() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let empty = json!({ "tasks": [], "count": 0 });

    let out = task_table_filtered(&ctx, &empty, crate::clock::now(), Some("+nosuchtag"));
    assert!(
        out.contains("+nosuchtag"),
        "the filter must be quoted back: {out:?}"
    );

    // The unfiltered caller (task_table itself) is untouched.
    assert_eq!(
        task_table(&ctx, &empty, crate::clock::now()),
        "No matching tasks.\n"
    );
}

#[test]
fn task_table_reports_a_status_the_store_could_not_read() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "tasks": [{
            "short_id": 7, "urgency": 5.0, "priority": "M", "title": "important work",
            "project": "work", "due": "", "tags": [],
            "status": "Done", "status_unrecognized": true
        }],
        "count": 1
    });
    let out = task_table(&ctx, &result, crate::clock::now());
    assert!(
        out.contains("Done"),
        "the offending value must be named: {out:?}"
    );
    assert!(
        out.contains("#7"),
        "the affected task must be identified: {out:?}"
    );
    assert!(out.contains("export"), "the way out must be named: {out:?}");

    // A clean table stays clean — the note is conditional, not a banner.
    let mut ok = result.clone();
    ok["tasks"][0] = json!({
        "short_id": 7, "urgency": 5.0, "priority": "M", "title": "important work",
        "project": "work", "due": "", "tags": [], "status": "pending"
    });
    assert!(
        !task_table(&ctx, &ok, crate::clock::now()).contains("export"),
        "clean table grew a warning"
    );
}

/// P4d: a store written before D36 can hold a BLANK title — every door
/// refuses one now, but the old ones did not. Such a row renders as an empty
/// TASK cell, so it is invisible in the one view the user is looking at,
/// and its export is a document that fails its own import (D36 refuses the
/// blank title on the way back in). That makes `export` useless as the
/// escape hatch D28 leans on, and the user cannot even tell which row is at
/// fault.
///
/// Detection, not repair: D28 already ruled that repair-on-open needs the
/// correct value to be KNOWABLE, and nothing here knows what the title was
/// meant to say. So the table names the row and the one command that fixes
/// it. Unlike the status case, `modify` really is the way out — a title is
/// freely settable, so there is no need to send the user through export.
#[test]
fn task_table_reports_a_blank_title_the_store_should_not_hold() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    for blank in ["", "   ", "	"] {
        let result = json!({
            "tasks": [{
                "short_id": 4, "urgency": 5.0, "priority": "M", "title": blank,
                "project": "work", "due": "", "tags": [], "status": "pending"
            }],
            "count": 1
        });
        let out = task_table(&ctx, &result, crate::clock::now());
        assert!(
            out.contains("#4"),
            "the affected task must be identified for {blank:?}: {out:?}"
        );
        assert!(
            out.contains("blank title"),
            "the anomaly must be labelled for {blank:?}: {out:?}"
        );
        assert!(
            out.contains("modify"),
            "the way out must be named for {blank:?}: {out:?}"
        );
    }

    // Conditional, not a banner: an ordinary table stays clean.
    let ok = json!({
        "tasks": [{
            "short_id": 4, "urgency": 5.0, "priority": "M", "title": "real work",
            "project": "work", "due": "", "tags": [], "status": "pending"
        }],
        "count": 1
    });
    assert!(
        !task_table(&ctx, &ok, crate::clock::now()).contains("blank title"),
        "clean table grew a warning"
    );
}

#[test]
fn task_table_neutralizes_escape_in_title() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let result = json!({
        "tasks": [{
            "short_id": 1, "urgency": 5.0, "priority": "M",
            "title": "hi\x1b[31mRED\x1b[0m", "project": "p",
            "due": "", "tags": ["\x1b[5mblink"], "status": "pending"
        }],
        "count": 1
    });
    let out = task_table(&ctx, &result, crate::clock::now());
    assert!(
        !out.contains('\x1b'),
        "raw escape reached the terminal: {out:?}"
    );
}

/// How many lines of chrome a `task_table` draws before its first row:
/// summary, blank, header.
///
/// A constant rather than a `skip(3)` written out at each call site. These
/// tests are about a COLUMN — how wide it is, whether it is drawn at all —
/// and every one of them that spelled its own row offset had to be edited
/// when the summary line moved in front of the header, none of them for a
/// reason to do with what they assert.
const CHROME: usize = 3;

/// The `ID … TAGS` label line of a rendered table.
fn header_of(text: &str) -> &str {
    text.lines().nth(CHROME - 1).expect("header line")
}

/// The data rows of a rendered table. Not trailer-aware on purpose — the
/// health notes print flush against the last row, so a caller that cares
/// takes exactly as many rows as it put in.
fn rows_of(text: &str) -> impl Iterator<Item = &str> {
    text.lines().skip(CHROME)
}

/// A terminal column is a grid of CELLS. `format!("{s:<36}")` pads by CHAR
/// COUNT, so one CJK title or one emoji shifted every column to its right
/// and the table stopped being a table.
///
/// The assertion is on the whole ROW rather than on a helper: the rows here
/// differ ONLY in the title, and every other cell is identical, so equal
/// display width across rows is exactly "the title column holds its budget".
/// That is true no matter how the padding is implemented, which is the point
/// — it cannot be satisfied by a helper that is correct while a call site
/// still formats with `{:<36}`.
#[test]
fn task_table_title_column_holds_its_width_in_cells() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let tasks: Vec<Value> = AWKWARD
        .iter()
        .enumerate()
        .map(|(i, title)| {
            json!({ "short_id": i + 1, "urgency": 5.0, "priority": "M", "title": title,
                        "project": "work", "due": "2026-07-20T17:00:00Z", "tags": ["t"],
                        "status": "pending" })
        })
        .collect();
    let out = task_table(
        &ctx,
        &json!({ "tasks": tasks, "count": tasks.len() }),
        crate::clock::now(),
    );
    let rows: Vec<&str> = rows_of(&out).take(AWKWARD.len()).collect();
    assert_eq!(
        rows.len(),
        AWKWARD.len(),
        "expected one row per title: {out:?}"
    );
    let want = cells(rows[0]);
    for (row, title) in rows.iter().zip(AWKWARD) {
        assert_eq!(
            cells(row),
            want,
            "row for {title:?} is {} cells, not {want}: {row:?}",
            cells(row)
        );
    }
}

/// The same rule on the OTHER columns of the same table: a fix that only
/// sized `title` correctly would leave `project` and `due` shifting the
/// columns to their right.
#[test]
fn task_table_project_and_due_columns_hold_their_width_in_cells() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    for field in ["project", "due"] {
        let tasks: Vec<Value> = AWKWARD
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut t = json!({ "short_id": i + 1, "urgency": 5.0, "priority": "M",
                                        "title": "same", "project": "work", "due": "",
                                        "tags": ["t"], "status": "pending" });
                t[field] = json!(v);
                t
            })
            .collect();
        let out = task_table(
            &ctx,
            &json!({ "tasks": tasks, "count": tasks.len() }),
            crate::clock::now(),
        );
        let rows: Vec<&str> = rows_of(&out).take(AWKWARD.len()).collect();
        let want = cells(rows[0]);
        for (row, v) in rows.iter().zip(AWKWARD) {
            assert_eq!(cells(row), want, "{field}={v:?} broke alignment: {row:?}");
        }
    }
}

/// #228.16: the table used to print a tag bare (`cardtag`), the one
/// spelling `list`'s own filter grammar rejects (`unknown filter token
/// "cardtag"`) — copying what the tool just printed into the tool's own
/// query language was an error. `+tag` is what `tag.add`/`modify` already
/// echo and the only spelling the filter parses.
#[test]
fn the_table_renders_tags_in_the_spelling_the_filter_accepts() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let tasks = vec![json!({ "short_id": 1, "urgency": 5.0, "priority": "M",
                                  "title": "t", "project": "", "due": "",
                                  "tags": ["cardtag"], "status": "pending" })];
    let out = task_table(
        &ctx,
        &json!({ "tasks": tasks, "count": 1 }),
        crate::clock::now(),
    );
    assert!(
        out.contains("+cardtag"),
        "the TAGS column must render `+cardtag`, not bare `cardtag`: {out:?}"
    );
}

// ---- what the list screen says without being read closely --------------

/// `due_cell`'s whole vocabulary, dated from one fixed instant.
///
/// The table is the assertion: every branch is one line, so a branch added
/// without a spelling — or a spelling changed without a reason — is visible
/// as a diff on this list rather than as a cell nobody looked at.
#[test]
fn the_due_cell_dates_a_deadline_the_way_a_reader_would() {
    // A Thursday, mid-afternoon: far enough into the day that "today" and
    // "already today" are both reachable from it.
    let now: Timestamp = "2026-09-10T15:00:00Z".parse().unwrap();
    for (iso, want) in [
        ("2026-09-10T23:59:00Z", "today 23:59"),
        ("2026-09-10T09:00:00Z", "today 09:00"),
        ("2026-09-10T00:00:00Z", "today"),
        ("2026-09-11T00:00:00Z", "tomorrow"),
        ("2026-09-11T09:00:00Z", "tomorrow 09:00"),
        ("2026-09-09T00:00:00Z", "yesterday"),
        ("2026-09-08T00:00:00Z", "2d ago"),
        ("2026-07-29T00:00:00Z", "43d ago"),
        // Inside the week the weekday is the whole answer: there is
        // exactly one Sunday between here and next Thursday.
        ("2026-09-13T00:00:00Z", "Sun"),
        ("2026-09-16T09:00:00Z", "Wed"),
        // Past it, the day needs naming; the year only when it changes.
        ("2026-09-17T00:00:00Z", "17 Sep"),
        ("2026-11-04T00:00:00Z", "4 Nov"),
        ("2027-01-04T00:00:00Z", "4 Jan 27"),
    ] {
        let at: Timestamp = iso.parse().unwrap();
        assert_eq!(due_cell(at, now), want, "{iso}");
    }
}

/// The `DUE` column holds a date a reader can act on, not the instant the
/// store happens to keep.
///
/// `task_row` used to pass `s(t, "due")` through untouched, so this column
/// spent twenty cells per row on `2026-09-11T00:00:00Z` — while `TASK`, the
/// column the row is read for, was the one `TaskCols::fit` cut to its floor
/// on an 80-cell terminal. The assertion is on the SHAPE of the stamp
/// rather than on the word, so it holds whatever [`due_cell`]'s vocabulary
/// grows into.
#[test]
fn the_due_column_no_longer_prints_the_stored_instant() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let now: Timestamp = "2026-09-10T15:00:00Z".parse().unwrap();
    let out = task_table(
        &ctx,
        &json!({ "tasks": [
                task_json(1, "ship it", "work", "2026-09-11T00:00:00Z", &["t"]),
            ], "count": 1 }),
        now,
    );
    assert!(
        !out.contains("T00:00:00Z"),
        "the RFC-3339 instant is still in the table: {out:?}"
    );
    assert!(out.contains("tomorrow"), "{out:?}");
}

/// A `due` this build cannot parse is printed as it stands rather than
/// dropped — the policy `field_ts` already states for the overdue test, and
/// the one `markdown::fmt_instant` follows on the `show` card. A cell that
/// went blank instead would hide a field the store plainly holds, which is
/// the invisible-field failure D36 exists to prevent.
#[test]
fn an_unreadable_due_falls_back_to_itself() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = task_table(
        &ctx,
        &json!({ "tasks": [
                task_json(1, "ship it", "work", "next tuesday-ish", &["t"]),
            ], "count": 1 }),
        crate::clock::now(),
    );
    assert!(
        out.contains("next tuesday-ish"),
        "an unparseable due vanished from the table: {out:?}"
    );
}

/// The running task and the blocked one are marked in the left rail, and
/// the rail survives the terminal that drops every other optional column.
///
/// Audit finding #147: nothing in `tasqx list` marked the one running
/// timer — the single piece of state a work block depends on. `STATUS` did
/// carry it, to the RIGHT of a title that can run 72 cells, and
/// `TaskCols::fit` drops that column outright on a narrow terminal. So the
/// assertion is made at 40 cells: a rail that only shows up when there is
/// room for it is the bug moving the glyph was meant to fix.
#[test]
fn the_rail_marks_the_running_and_the_blocked_row_at_any_width() {
    let mut ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    ctx.cols = 40;
    let result = json!({ "tasks": [
            { "short_id": 1, "urgency": 9.0, "priority": "H", "title": "running one",
              "project": "work", "due": "", "tags": [], "status": "active" },
            { "short_id": 2, "urgency": 8.0, "priority": "H", "title": "stuck one",
              "project": "work", "due": "", "tags": [], "status": "pending", "blocked": true },
            { "short_id": 3, "urgency": 7.0, "priority": "M", "title": "ordinary one",
              "project": "work", "due": "", "tags": [], "status": "pending" },
        ], "count": 3 });
    let out = task_table(&ctx, &result, crate::clock::now());
    let rows: Vec<&str> = rows_of(&out).take(3).collect();
    assert!(rows[0].starts_with('*'), "no timer glyph: {:?}", rows[0]);
    assert!(rows[1].starts_with('B'), "no blocked glyph: {:?}", rows[1]);
    assert!(
        rows[2].starts_with("  "),
        "an ordinary row grew a rail: {:?}",
        rows[2]
    );
}

/// The house style's Contract table is BUILT from the renderer, then
/// looked for in the doc.
///
/// `docs/maintainers/terminal-style.md` is what anyone touching a screen reads first,
/// and a guide describing a screen the binary no longer prints is worse
/// than no guide — it is a wrong answer carrying the repo's authority. So
/// the rows are generated here and asserted present there, which catches
/// the drift in BOTH directions: a glyph the code stopped drawing, and a
/// glyph the doc stopped naming. Checking only "every glyph the code emits
/// appears somewhere in the doc" was tried first and let the Contract
/// table go stale while the prose above it still happened to quote the
/// right character.
///
/// `include_str!` reaches outside `src/` on purpose, the same way
/// `doc_gate_tests` in `tasqx-core` embeds `.github/workflows/ci.yml`:
/// moving or renaming the file is then a compile error rather than a
/// silent orphaning.
#[test]
fn the_house_style_doc_still_describes_the_screens() {
    const DOC: &str = include_str!("../../../../../docs/maintainers/terminal-style.md");

    let task = |status: &str, blocked: bool| {
        json!({ "short_id": 1, "urgency": 1.0, "priority": "M", "title": "t",
                    "project": "p", "due": "", "tags": [], "status": status,
                    "blocked": blocked })
    };
    let rail = |status: &str, blocked: bool, unicode: bool| {
        rail_marker(&task(status, blocked), unicode)
            .expect("a rail glyph")
            .1
            .to_string()
    };

    // Every glyph the gauge can emit, swept across its range, split into
    // the three roles the doc names them by.
    let full = urgency_meter(1.0).0.chars().next().expect("a full cell");
    let track = urgency_meter(0.0).1.chars().next().expect("a track cell");
    let mut remainder: Vec<char> = Vec::new();
    for step in 0..=100 {
        let (bar, tail) = urgency_meter(f64::from(step) / 100.0);
        assert_eq!(
            bar.chars().count() + tail.chars().count(),
            4,
            "the gauge is not 4 cells wide at {step}%"
        );
        for c in bar.chars().chain(tail.chars()) {
            if c != full && c != track && !remainder.contains(&c) {
                remainder.push(c);
            }
        }
    }
    remainder.sort_unstable();
    let remainder: String = remainder
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(" ");

    // Where each of the built-in ramp's bands begins, on the urgency scale.
    let anchors = theme::builtin("nord").unwrap().ramp().len();
    let bands = (0..anchors)
        .map(|i| {
            let from = i as f64 / (anchors - 1) as f64 * tasqx_core::urgency::DUE_WEIGHT;
            format!("from {from:.0}")
        })
        .collect::<Vec<_>>()
        .join(" · ");

    for row in [
        format!(
            "| rail, running | `{}` (`{}` without Unicode) |",
            rail("active", false, true),
            rail("active", false, false)
        ),
        format!(
            "| rail, blocked | `{}` (`{}` without Unicode) |",
            rail("pending", true, true),
            rail("pending", true, false)
        ),
        format!(
            "| rail, the one in effect | `{}` (`projects`, `theme list`) |",
            CurrentRail::MARK
        ),
        format!("| gauge, full cell | `{full}` |"),
        format!("| gauge, track | `{track}` |"),
        format!("| gauge, remainder | {remainder} |"),
        "| gauge width | 4 cells |".to_string(),
        format!(
            "| gauge full at | urgency {:.0} (`urgency::DUE_WEIGHT`) |",
            tasqx_core::urgency::DUE_WEIGHT
        ),
        format!("| ramp bands, built-ins | {bands} |"),
        "| column-header role | `table.label` |".to_string(),
    ] {
        assert!(
            DOC.contains(&row),
            "docs/maintainers/terminal-style.md's Contract table is missing the row the \
                 renderer produces:\n{row}"
        );
    }

    assert!(
        theme::default_theme()
            .role_names()
            .iter()
            .any(|r| r == "table.label"),
        "the doc names a theme role no built-in defines"
    );
    assert!(
        DOC.contains(&plural_tasks(1)) && DOC.contains("N tasks"),
        "the doc lost the count spelling"
    );
}

/// The gauge separates the scores a reader is actually ranking.
///
/// The first mock drew whole cells only, and 17.9 and 15.8 against a top of
/// 17.9 both came out `▄▄▄▄`, the two rows a reader compared hardest drawn
/// identically. Three steps inside each cell is what fixed it. Under D119's
/// absolute scale both of those now saturate, on purpose, so the pairs that
/// matter are the landmarks below the top: no priority, L, M and H alone,
/// an H task ten days from its deadline, and overdue. Each must draw its
/// own mark. A step is one urgency point, so the age term's hundredths are
/// not asked to show.
#[test]
fn the_urgency_gauge_separates_the_scores_the_reader_is_ranking() {
    let marks: Vec<(String, String)> = [0.0, 1.8, 3.9, 6.0, 9.0, 12.0]
        .iter()
        .map(|&u| urgency_meter(urgency_scale(u)))
        .collect();
    for (i, a) in marks.iter().enumerate() {
        for b in &marks[i + 1..] {
            assert_ne!(a, b, "two landmarks draw the same gauge: {marks:?}");
        }
    }
}

/// More urgency is never less ink.
///
/// The first fix for the resolution problem drew the remainder as a TALLER
/// glyph than the bar's own `▄`, which made a nearly-empty gauge (`▇▁▁▁` at
/// 22% of the range) the heaviest mark in a column whose hottest row was a
/// flat `▄▄▄▄`. That is the ranking inverted, and it is invisible to any
/// assertion about the value — so this one weighs the glyphs.
#[test]
fn the_gauge_never_draws_more_ink_for_less_urgency() {
    let mass = |ramp: f64| -> usize {
        let (bar, track) = urgency_meter(ramp);
        format!("{bar}{track}")
            .chars()
            .map(|c| match c {
                '▁' => 1,
                '▂' => 2,
                '▃' => 3,
                '▄' => 4,
                other => panic!("unexpected gauge glyph {other:?}"),
            })
            .sum()
    };
    let mut last = 0;
    for step in 0..=100 {
        let here = mass(f64::from(step) / 100.0);
        assert!(
            here >= last,
            "the gauge got lighter going up the scale at {step}%: {here} after {last}"
        );
        last = here;
    }
    assert_eq!(mass(0.0), 4, "an empty gauge is a bare track");
    assert_eq!(mass(1.0), 16, "a full gauge is four full cells");
}

/// On a tie between PROJECT and DUE, PROJECT gives the cell.
///
/// `columns::fit` breaks ties to the right by default, which cut `agenda`'s
/// overdue dates to `due 2026-0…` on an 80-column terminal while the
/// project beside them kept its full width. The order the table had before
/// the shared fitter, STATUS then TAGS then PROJECT then DUE, is carried
/// as `tie_rank`s. A cut project name still identifies itself; a cut date
/// says nothing.
#[test]
fn on_a_tie_the_project_gives_before_the_date() {
    let mut r = task_row(
        &json!({ "short_id": 1, "urgency": 1.0, "priority": "M",
                     "title": "t".repeat(14), "project": "p".repeat(14),
                     "status": "pending" }),
        crate::clock::now(),
        false,
    );
    r.due = "d".repeat(14);
    let natural = TaskCols::fit(std::slice::from_ref(&r), 200, "DUE", false);
    assert_eq!((natural.project, natural.due), (14, 14), "not a tie");
    let budget = columns::total(&[natural.head(), natural.title, natural.project, natural.due]) - 1;
    let c = TaskCols::fit(std::slice::from_ref(&r), budget, "DUE", false);
    assert_eq!((c.project, c.due), (13, 14));
}

/// The gauge glyphs on the row carrying `title`, and nothing else.
fn gauge_of(out: &str, title: &str) -> String {
    out.lines()
        .find(|l| l.contains(title))
        .unwrap_or_else(|| panic!("no row for {title:?} in {out}"))
        .chars()
        .filter(|c| matches!(c, '▁' | '▂' | '▃' | '▄'))
        .collect()
}

/// D119: a task's gauge and colour are a property of the TASK, not of the
/// other rows on screen.
///
/// The denominator used to be the hottest visible row, so one overdue task
/// at 18.5 flattened every other gauge on a real store, and a filtered view
/// gave its top row a full bar in the danger colour whatever that row held.
/// On a project with nothing pressing that meant done tasks were painted
/// in `danger`. Compared painted, colour included, so a scale that moved
/// only the colour would fail here too.
#[test]
fn the_same_task_draws_the_same_urgency_cell_whatever_else_is_on_screen() {
    let ctx = Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    );
    let row = |id: i64, title: &str, urgency: f64| {
        let mut t = task_json(id, title, "work", "", &[]);
        t["urgency"] = json!(urgency);
        t
    };
    let subject = "the task under test";
    let alone = json!({ "tasks": [row(1, subject, 6.0)], "count": 1 });
    let beside = json!({ "tasks": [
            row(2, "an outlier elsewhere", 9.9), row(1, subject, 6.0),
        ], "count": 2 });
    let cell = |r: &Value| {
        let out = task_table(&ctx, r, crate::clock::now());
        let line = out.lines().find(|l| l.contains(subject)).unwrap();
        line.split(subject).next().unwrap().to_string()
    };
    assert_eq!(
        cell(&alone),
        cell(&beside),
        "a hotter row elsewhere on screen changed this task's urgency cell"
    );
}

/// D119: a full gauge means "as urgent as an overdue task". The scale is
/// the due term's saturation, read from core, so it cannot drift from the
/// formula it describes.
///
/// Scored through `urgency::score_at` rather than written as literals, so
/// a change to the formula moves both sides of this test together. What H
/// priority alone earns is half of that, and draws half a gauge. That half
/// is also where the built-in ramp's middle band begins, so if the formula
/// stops making H exactly half the due weight, this fails, and D119's
/// "warn starts at H alone" needs revisiting rather than this assertion.
#[test]
fn a_full_gauge_means_as_urgent_as_an_overdue_task() {
    use tasqx_core::types::Priority;
    use tasqx_core::urgency::score_at;

    let mut ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    ctx.caps.unicode = true;
    let now: Timestamp = "2026-09-11T12:00:00Z".parse().unwrap();
    let created = "2026-09-11T12:00:00Z";
    let gauge = |urgency: f64| {
        let mut t = task_json(1, "subject", "work", "", &[]);
        t["urgency"] = json!(urgency);
        gauge_of(
            &task_table(&ctx, &json!({ "tasks": [t], "count": 1 }), now),
            "subject",
        )
    };

    let overdue = score_at(None, Some("2026-09-01T00:00:00Z"), created, now);
    let h_alone = score_at(Some(Priority::H), None, created, now);
    assert_eq!(gauge(overdue), "▄▄▄▄", "overdue at {overdue}");
    assert_eq!(gauge(h_alone), "▄▄▁▁", "H priority alone at {h_alone}");
    assert_ne!(
        gauge(overdue - 0.5),
        "▄▄▄▄",
        "the gauge fills before the task is as urgent as an overdue one"
    );
}

/// No glyph set degrades honestly here, so a terminal without Unicode gets
/// the figure and no gauge — and the column narrows by exactly what the
/// gauge would have cost, rather than leaving five cells of nothing.
#[test]
fn a_terminal_without_unicode_gets_no_gauge_and_the_cells_back() {
    let result = json!({ "tasks": [
            task_json(1, "a long enough title to notice", "work", "", &["t"]),
        ], "count": 1 });
    let plain = task_table(
        &Ctx::new(theme::default_theme(), Caps::PLAIN),
        &result,
        crate::clock::now(),
    );
    assert!(
        !plain.contains('▄') && !plain.contains('▁'),
        "block glyphs on a terminal that cannot draw them: {plain:?}"
    );

    let mut uni = Ctx::new(theme::default_theme(), Caps::PLAIN);
    uni.caps.unicode = true;
    let drawn = task_table(&uni, &result, crate::clock::now());
    assert!(drawn.contains('▄'), "no gauge with Unicode: {drawn:?}");
    assert_eq!(
        cells(header_of(&drawn)) - cells(header_of(&plain)),
        TaskCols::METER,
        "the gauge's cells were not handed back:\n{plain}\n{drawn}"
    );
}

/// A store with nothing running and nothing blocked pays nothing for the
/// rail — D51's rule for `DUE`, applied to the column left of the ids.
#[test]
fn a_store_with_no_state_to_show_is_not_indented_by_the_rail() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = task_table(
        &ctx,
        &json!({ "tasks": [task_json(1, "ordinary", "work", "", &[])], "count": 1 }),
        crate::clock::now(),
    );
    assert!(
        header_of(&out).starts_with("  ID"),
        "an unused rail still indented the table: {:?}",
        header_of(&out)
    );
}

/// The summary line answers the questions a count cannot.
///
/// `N task(s)` was the whole trailer: a reader with the rows in front of
/// them can count them, and what they cannot see at a glance is how much of
/// the list is late, due before the day is out, running, or stuck.
#[test]
fn the_summary_names_what_the_rows_do_not_show_at_a_glance() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let now: Timestamp = "2026-09-10T15:00:00Z".parse().unwrap();
    let result = json!({ "tasks": [
            { "short_id": 1, "urgency": 9.0, "priority": "H", "title": "late one",
              "project": "work", "due": "2026-09-08T00:00:00Z", "tags": [], "status": "pending" },
            { "short_id": 2, "urgency": 8.0, "priority": "H", "title": "today one",
              "project": "work", "due": "2026-09-10T23:00:00Z", "tags": [], "status": "active" },
            { "short_id": 3, "urgency": 7.0, "priority": "M", "title": "stuck one",
              "project": "work", "due": "", "tags": [], "status": "pending", "blocked": true },
        ], "count": 3 });
    let table = task_table(&ctx, &result, now);
    let summary = table.lines().next().unwrap();
    for want in [
        "3 tasks",
        "1 overdue",
        "1 due today",
        "#2 running",
        "1 blocked",
    ] {
        assert!(summary.contains(want), "{want:?} missing from {summary:?}");
    }
    assert!(
        !summary.contains("task(s)"),
        "the programmer's plural survived: {summary:?}"
    );
}

/// A fact that is zero is not printed. A line that always said `0 overdue`
/// would train the reader to skip it, and then it is not there on the day
/// it says something.
#[test]
fn the_summary_prints_only_the_facts_that_are_true() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = task_table(
        &ctx,
        &json!({ "tasks": [task_json(1, "ordinary", "work", "", &[])], "count": 1 }),
        crate::clock::now(),
    );
    let summary = out.lines().next().unwrap();
    assert_eq!(summary.trim(), "1 task", "{summary:?}");
}

/// `list` echoes the filter it answered, on every run rather than only on
/// an empty result: `tasqx list` defaults to `@working` (`verbs::list`),
/// and a reader who cannot see which question was asked cannot tell a short
/// answer from a narrow one.
#[test]
fn the_summary_names_the_filter_that_produced_it() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = task_table_filtered(
        &ctx,
        &json!({ "tasks": [task_json(1, "ordinary", "work", "", &[])], "count": 1 }),
        crate::clock::now(),
        Some("project:raid +design"),
    );
    assert!(
        out.lines().next().unwrap().contains("project:raid +design"),
        "{out:?}"
    );
}

/// A column no row fills is not drawn, and its cells go to the columns that
/// have something to show.
///
/// This is what the reader was looking at when they said the table "doesn't
/// look aligned": on a store with no due dates, `DUE` still held 22 cells
/// plus its gaps, so the widest gap in the table sat where there was no
/// data — a hole between `PROJECT` and `TAGS` that reads as a broken grid —
/// while `TASK` was cut to 36 cells and ellipsised titles that had 40 cells
/// of empty terminal to their right.
#[test]
fn a_column_no_task_fills_is_not_drawn_and_its_room_goes_to_the_titles() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let long = "Level-1 gameplay vision walkthrough, progression and escalation";
    let with_due = json!({ "tasks": [
            task_json(1, long, "raid.game", "2026-07-20T17:00:00Z", &["design"]),
        ], "count": 1 });
    let without_due = json!({ "tasks": [
            task_json(1, long, "raid.game", "", &["design"]),
        ], "count": 1 });

    let head = |t: &Value| header_of(&task_table(&ctx, t, crate::clock::now())).to_string();
    assert!(head(&with_due).contains("DUE"), "{}", head(&with_due));
    assert!(
        !head(&without_due).contains("DUE"),
        "an empty DUE column was still drawn: {:?}",
        head(&without_due)
    );
    // And the title survives whole once the dead column is gone.
    let table = task_table(&ctx, &without_due, crate::clock::now());
    let row = rows_of(&table).next().unwrap().to_string();
    assert!(
        row.contains(long),
        "the title was truncated with room to spare: {row:?}"
    );
}

/// The table lays out for the terminal it is printing into, and never
/// overruns it — including the rule, which is drawn from the same widths.
#[test]
fn the_table_fits_the_width_it_was_given() {
    for cols in [Ctx::MIN_COLS, 60, 80, Ctx::DEFAULT_COLS, Ctx::MAX_COLS] {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols);
        let tasks: Vec<Value> = (1..=4)
            .map(|i| {
                task_json(
                    i,
                    "a title long enough that it cannot possibly fit any of these budgets \
                         without being cut somewhere",
                    "some.rather.long.project.name",
                    "2026-07-20T17:00:00Z",
                    &["one", "two", "three", "four", "five", "six"],
                )
            })
            .collect();
        let out = task_table(
            &ctx,
            &json!({ "tasks": tasks, "count": tasks.len() }),
            crate::clock::now(),
        );
        for line in out.lines() {
            assert!(
                cells(line) <= cols,
                "a {}-cell line in a {cols}-cell terminal: {line:?}",
                cells(line)
            );
        }
    }
}

/// Wider is not a licence to spread: past `MAX_COLS` the extra cells are
/// left alone rather than poured into one enormous title column.
#[test]
fn an_ultrawide_terminal_does_not_get_an_ultrawide_table() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(400);
    assert_eq!(ctx.cols, Ctx::MAX_COLS, "the width must be clamped");
    let title = "x".repeat(300);
    let out = task_table(
        &ctx,
        &json!({ "tasks": [task_json(1, &title, "p", "", &["t"])], "count": 1 }),
        crate::clock::now(),
    );
    for line in out.lines() {
        assert!(cells(line) <= Ctx::MAX_COLS, "{line:?}");
    }
}

/// Truncation has to cut on a GRAPHEME boundary and budget in cells. Half a
/// ZWJ sequence is not a shorter emoji — it is a different one, or a lone
/// joiner the terminal draws as tofu, and it still overflows the column.
///
/// Asserted through `cell` — the function the table actually builds its
/// columns with — rather than through the truncation helper underneath it,
/// so a correct helper called with the wrong budget still fails here.
#[test]
fn truncation_budgets_cells_and_never_splits_a_cluster() {
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466}";
    for unicode in [true, false] {
        let ctx = Ctx::new(
            theme::default_theme(),
            theme::Caps {
                depth: theme::ColorDepth::None,
                ansi: false,
                unicode,
            },
        );
        // Long enough to force a cut, with the cut landing inside a cluster.
        let s = format!("ab{family}{family}cd");
        let got = cell(&ctx, None, &s, 7);
        assert_eq!(
            cells(&got),
            7,
            "cell budget blown (unicode={unicode}): {got:?}"
        );
        assert!(
            !got.ends_with('\u{200d}'),
            "cut left a dangling joiner (unicode={unicode}): {got:?}"
        );
        // A CJK string: 5 ideographs are 10 cells, so 6 cells must fit at
        // most 2 of them plus the ellipsis — a char-counting truncate keeps 5.
        let cjk = cell(&ctx, None, "漢字テスト", 6);
        assert_eq!(
            cells(&cjk),
            6,
            "CJK cell budget blown (unicode={unicode}): {cjk:?}"
        );
        // A string already inside its budget is padded, not cut.
        assert_eq!(
            cell(&ctx, None, "中文", 6),
            "中文  ",
            "short cell should be padded to 6 cells"
        );
    }
}

#[test]
fn truncate_uses_ascii_ellipsis_without_unicode() {
    let long = "a".repeat(50);
    let uni = truncate(&long, 10, true);
    assert!(uni.ends_with('…') && !uni.contains("..."));
    let ascii = truncate(&long, 10, false);
    assert!(ascii.ends_with("...") && !ascii.contains('…'));
    assert!(ascii.is_ascii(), "no non-ASCII in plain path");
    assert_eq!(ascii.chars().count(), 10);
}

/// One phrasing per op in the core's closed set, selected by the reverted op
/// rather than by sniffing which keys `restored` happens to carry — and a
/// fallback that still prints the payload, because an op this build has no
/// phrasing for must not reach the terminal as a blank line reading "it
/// restored nothing".
#[test]
fn every_undoable_op_gets_a_line_that_says_what_it_restored() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let line = |op: &str, restored: Value| -> String {
        let result = json!({
            "reverted": { "event": "e1", "op": op, "ts": "t" },
            "short_id": 7,
            "title": "t",
            "restored": restored,
        });
        // The task as read back after the revert: running again for a stop.
        let mut task = result.clone();
        if op == "stop" {
            task["status"] = json!("active");
        }
        undone(&ctx, &result, &task, &Titles::new(), crate::clock::now())
    };

    assert!(line("dependency.remove", json!({ "depends_on": 3 })).contains("#3"));
    // D126: `*` in the rail says it runs again, and the line says how long
    // it has been running — elapsed, not an instant, because `due_cell`'s
    // clock is UTC and read as the wall clock (D126 l); `restored.tracked`
    // is the interval put back, not a total.
    let stopped = line(
        "stop",
        json!({ "tracked": "PT30M", "status": "active",
                    "interval_started": "2026-09-09T10:00:00Z" }),
    );
    assert!(
        stopped.contains("* undid stop") && stopped.contains(" for "),
        "{stopped:?}"
    );
    let noted = line(
        "annotation.add",
        json!({ "annotation": "called the plumber" }),
    );
    assert!(noted.contains("called the plumber"), "{noted:?}");

    // The core's closed set can grow; this renderer must degrade to showing
    // the data rather than to showing nothing.
    let unknown = line("some.future.op", json!({ "whatever": 1 }));
    assert!(
        unknown.contains("whatever"),
        "an op with no phrasing must still print what it restored: {unknown:?}"
    );
}

/// Both status-less tables print the SAME store-health notes, because they
/// call the same function.
///
/// The defect this pins: `agenda` shipped as a second table over `list`'s
/// rows and carried neither note. An unreadable status has no column to
/// appear in and a blank title draws as an empty cell, so the row sat under
/// a day heading indistinguishable from ordinary open work and the run
/// exited 0 — the invisible-field failure rebuilt one view over. Asserting
/// the two views AGREE, rather than asserting each one's text, is what makes
/// this a guard: a third view that forgets the notes fails it too, and
/// rewording a note cannot pass it on one side only.
#[test]
fn every_status_less_table_carries_the_same_store_health_notes() {
    let mut unreadable = dated(7, "important work", "2026-08-05T00:00:00Z", "");
    unreadable["status"] = json!("Done");
    unreadable["status_unrecognized"] = json!(true);
    let untitled = dated(4, "", "2026-08-06T00:00:00Z", "");
    let tasks = vec![unreadable, untitled];

    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let list = task_table(
        &ctx,
        &json!({ "tasks": tasks, "count": 2 }),
        crate::clock::now(),
    );
    let agenda = agenda_out(tasks.clone(), AGENDA_DEFAULT_DAYS);

    let notes = store_health_notes(&tasks);
    assert_eq!(
        notes.len(),
        2,
        "the fixture must trip both notes: {notes:?}"
    );
    // Compared with the line breaks folded into spaces: both views wrap
    // their notes to the terminal (#346), so a note longer than the width
    // is the same words over two lines, and agreement is about the words.
    let fold = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let (list, agenda) = (fold(&list), fold(&agenda));
    for note in &notes {
        let note = fold(note);
        assert!(
            list.contains(note.as_str()),
            "`list` dropped a store-health note:\n{list}"
        );
        assert!(
            agenda.contains(note.as_str()),
            "`agenda` draws the same rows in the same layout, so it owes the \
                 reader the same note:\n{agenda}"
        );
    }
    // Named ids, not just a count: "1 unreadable row" leaves the reader with
    // a store to search by hand.
    assert!(agenda.contains("#7 (Done)"), "{agenda}");
    assert!(agenda.contains("#4"), "{agenda}");
}

/// #145: `tasqx list project:x` (a literal filter, no `@working`) returns
/// whatever the project holds — done and cancelled included, D24's rule
/// being about `report`, not `list`. Leaving that literal is defensible;
/// what is not is a cancelled row printing identically to open work at
/// the top of the table, ranked by urgency, with nothing to tell them
/// apart short of a `show`.
#[test]
fn task_table_marks_a_row_whose_status_is_not_open() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let row = |status: &str| {
        json!({ "short_id": 1, "urgency": 5.0, "priority": "M", "title": "same work",
                    "project": "p", "due": "", "tags": [], "status": status })
    };
    let open = task_table(
        &ctx,
        &json!({ "tasks": [row("pending")], "count": 1 }),
        crate::clock::now(),
    );
    let closed = task_table(
        &ctx,
        &json!({ "tasks": [row("cancelled")], "count": 1 }),
        crate::clock::now(),
    );
    assert_ne!(
        open, closed,
        "a cancelled row must not render identically to a pending one: {closed:?}"
    );
    assert!(
        closed.contains("cancelled"),
        "the task's own status must be visible in the table, not only via \
             `tasqx show`: {closed:?}"
    );
}

/// #146: `blocked` sits on the JSON of every row `list`/`agenda` print and
/// is rendered on neither. A blocked task can rank #1 by urgency and look
/// exactly like ordinary open work, and `why` explains the score without
/// ever mentioning the task cannot be started.
#[test]
fn task_table_and_why_surface_a_blocked_task() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let row = |blocked: bool| {
        json!({ "short_id": 1, "urgency": 18.0, "priority": "H",
                    "title": "BLOCKER-HIGH must not be next", "project": "p",
                    "due": "", "tags": [], "status": "pending", "blocked": blocked })
    };
    let open = task_table(
        &ctx,
        &json!({ "tasks": [row(false)], "count": 1 }),
        crate::clock::now(),
    );
    let blocked = task_table(
        &ctx,
        &json!({ "tasks": [row(true)], "count": 1 }),
        crate::clock::now(),
    );
    assert_ne!(
        open, blocked,
        "a blocked row must not render identically to an unblocked one: {blocked:?}"
    );

    let get_result = json!({
        "short_id": 1, "title": "BLOCKER-HIGH must not be next", "status": "pending",
        "urgency": 18.0, "blocked": true, "depends_on": [2],
        "unmet_blockers": [{ "short_id": 2, "title": "the blocker" }]
    });
    let why_out = why(&ctx, &get_result, crate::clock::now());
    assert!(
        why_out.contains("blocked") && why_out.contains("#2"),
        "`why` must say the task is blocked and name what it is blocked by, \
             since the breakdown alone explains a score `next` will never offer: \
             {why_out:?}"
    );
}

/// #147: the one piece of state a work block depends on — which task is
/// already running — has no mark on `list`'s row for it, and `next`
/// returning that same task says nothing to distinguish it from a fresh
/// recommendation.
#[test]
fn task_table_and_next_surface_the_running_task() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let row = |status: &str| {
        json!({ "short_id": 2, "urgency": 5.0, "priority": "M", "title": "in progress",
                    "project": "p", "due": "", "tags": [], "status": status })
    };
    let pending = task_table(
        &ctx,
        &json!({ "tasks": [row("pending")], "count": 1 }),
        crate::clock::now(),
    );
    let active = task_table(
        &ctx,
        &json!({ "tasks": [row("active")], "count": 1 }),
        crate::clock::now(),
    );
    assert_ne!(
        pending, active,
        "an active row must not render identically to a pending one: {active:?}"
    );

    let mut running = row("active");
    running["active_since"] = json!("2026-08-31T12:00:00Z");
    let next_out = next_task(&ctx, &json!({ "tasks": [running] }), crate::clock::now());
    assert!(
        next_out.to_lowercase().contains("already running"),
        "`next` handing back the task that is already running must say so: \
             {next_out:?}"
    );
}
