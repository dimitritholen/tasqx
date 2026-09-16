//! Golden tests for D146's box card.
//!
//! Whole-output comparisons, for `markdown_detail.rs`' reason: the promise is
//! "the same task reads the same for everyone", and a substring assertion
//! cannot tell that promise from a near-miss. A card makes a second promise on
//! top of it — every line is exactly `CARD_WIDTH` display cells — which is
//! checked separately over every fixture here, because a golden string proves
//! the width only for the text it happens to contain.

use serde_json::{json, Value};
use tasqx_core::markdown::{
    brief_tail_text, task_brief, task_brief_card, task_card, task_detail, Borders, CardOpts,
    DetailOpts, TimeFormat, CARD_WIDTH,
};
use unicode_width::UnicodeWidthStr;

/// A fixed instant, so nothing here calls the clock.
fn at(iso: &str) -> jiff::Timestamp {
    iso.parse().expect("fixture timestamp parses")
}

fn detail_opts() -> DetailOpts {
    DetailOpts {
        time: TimeFormat::Iso,
        now: at("2026-09-15T18:00:00Z"),
    }
}

fn unicode() -> CardOpts {
    CardOpts {
        detail: detail_opts(),
        borders: Borders::Unicode,
    }
}

fn ascii() -> CardOpts {
    CardOpts {
        detail: detail_opts(),
        borders: Borders::Ascii,
    }
}

/// The fields `task.get` always returns, and nothing optional.
fn minimal() -> Value {
    json!({
        "short_id": 76,
        "title": "Three field-test papercuts",
        "status": "pending",
        "priority": "L",
        "project": "tasqx",
        "tracked": "PT0S",
        "created": "2026-09-15T09:00:58Z",
        "modified": "2026-09-15T09:01:45Z",
        "_rev": 2
    })
}

/// Every row the card can draw, at once.
///
/// `estimate` and `tracked` are chosen so the card's Status line proves both
/// that durations are compact under EVERY `TimeFormat` (D146: `card_duration`
/// ignores `opts.time`) and that `humanize_secs` rounds: `PT1H30M` is 5400
/// seconds, which is exactly the halfway point between 1h and 2h, and
/// `round_div`'s "round half up" reads `tracked 2h`, not `1h30m` — there is no
/// such spelling — and not `1h`.
fn full() -> Value {
    json!({
        "short_id": 69,
        "title": "unmet_blockers ignored the dependent's own status, so a done task answered blocked:false",
        "status": "active",
        "priority": "H",
        "project": "tasqx",
        "estimate": "PT45M",
        "tracked": "PT1H30M",
        "tags": ["render", "markdown"],
        "due": "2026-09-20T17:00:00Z",
        "depends_on": [1, 2],
        "blocks": [70, 71],
        "unmet_blockers": [
            { "short_id": 1, "title": "one blocked predicate, three readers" },
            { "short_id": 2, "title": "the dependent's own status" }
        ],
        "blocked": true,
        "checks": [
            { "body": "done dependent answers with an empty unmet_blockers", "state": "passed", "evidence": "cargo test" },
            { "body": "open dependent, one done and one open blocker", "state": "passed" },
            { "body": "the four shipped texts no longer say \"not yet done\"", "state": "failed" },
            { "body": "conformance", "state": "open" }
        ],
        "budget_tokens": 120000,
        "fresh_tokens": 150000,
        "over": true,
        "annotations": [
            { "id": "a1", "created": "2026-09-14T08:00:00Z", "body": "Derive blocked from unmet_blockers so the two cannot disagree.\nOne predicate, three readers.\n\nThe second paragraph argues for it and is not the summary." },
            { "id": "a2", "created": "2026-09-15T12:00:00Z", "body": "Landed the predicate." },
            { "id": "a3", "created": "2026-09-15T17:08:22Z", "body": "Four shipped texts still said \"not yet done\"." }
        ],
        "annotations_total": 3,
        "annotations_offset": 0,
        "created": "2026-09-14T07:00:00Z",
        "modified": "2026-09-15T17:08:22Z",
        "_rev": 9
    })
}

/// Closed, so the card has an outcome to report.
fn done() -> Value {
    json!({
        "short_id": 69,
        "title": "one blocked predicate",
        "status": "done",
        "priority": "H",
        "project": "tasqx",
        "depends_on": [1, 2],
        "unmet_blockers": [],
        "annotations": [
            { "id": "a1", "created": "2026-09-14T08:00:00Z", "body": "Approach: derive the flag from the list.\n\nWhy: two fields, one query." },
            { "id": "a2", "created": "2026-09-15T17:08:22Z", "body": "Shipped: one predicate spelled once, read three ways.\n\nSkipped the CLI rail card, which D78 owns." }
        ],
        "annotations_total": 2,
        "annotations_offset": 0,
        "created": "2026-09-14T07:00:00Z",
        "modified": "2026-09-15T17:08:22Z",
        "_rev": 12
    })
}

/// The same task after `annotation.remove` scrubbed a third note: two readable
/// annotations and one tombstone (D113/D148).
fn scrubbed() -> Value {
    let mut task = done();
    task["annotations_removed"] = json!([{ "id": "a0", "removed": "2026-09-15T17:30:00Z" }]);
    task
}

/// A page of a longer history, taken from the recent end: the first note is ten
/// annotations older than anything on it.
fn paged() -> Value {
    let page: Vec<Value> = (0..20)
        .map(|i| {
            json!({
                "id": format!("a{i}"),
                "created": format!("2026-09-15T1{}:00:00Z", i % 10),
                "body": format!("note {i}")
            })
        })
        .collect();
    json!({
        "short_id": 7,
        "title": "a long history",
        "status": "active",
        "priority": "M",
        "project": "tasqx",
        "annotations": page,
        "annotations_total": 30,
        "annotations_offset": 0,
        "created": "2026-09-01T07:00:00Z",
        "modified": "2026-09-15T17:08:22Z",
        "_rev": 40
    })
}

/// The four scheduling rows, which no other fixture sets.
fn scheduled() -> Value {
    json!({
        "short_id": 12,
        "title": "the weekly sweep",
        "status": "pending",
        "priority": "M",
        "project": "tasqx",
        "tracked": "PT0S",
        "due": "2026-09-20T17:00:00Z",
        "scheduled": "2026-09-18T09:00:00Z",
        "wait": "2026-09-17T09:00:00Z",
        "recurrence": "FREQ=WEEKLY",
        "created": "2026-09-01T07:00:00Z",
        "modified": "2026-09-01T07:00:00Z",
        "_rev": 1
    })
}

/// Text chosen to break a naive renderer: double-width CJK, an emoji ZWJ
/// sequence, an unbroken URL longer than the column, a tab and an ANSI escape.
fn hostile() -> Value {
    json!({
        "short_id": 404,
        "title": "日本語のタイトル 👨‍👩‍👧 with emoji and 漢字 mixed into a line long enough to wrap twice over",
        "status": "active",
        "priority": "H",
        "project": "tasqx",
        "tracked": "PT0S",
        "annotations": [{
            "id": "a1",
            "created": "2026-09-15T10:00:00Z",
            "body": "See https://example.com/a/very/long/path/that/never/breaks/anywhere/at/all/because/it/has/no/spaces/in/it/x for the argument."
        }],
        "annotations_total": 1,
        "annotations_offset": 0,
        "checks": [
            { "body": "tab\there and an escape \u{1b}[31mred\u{1b}[0m in the body", "state": "passed" },
            { "body": "漢字の受入基準がセル幅を二倍に数える場合でも枠は崩れない", "state": "open" }
        ],
        "created": "2026-09-15T07:00:00Z",
        "modified": "2026-09-15T09:00:00Z",
        "_rev": 3
    })
}

/// A brief: the task above, plus the neighbourhood and memory halves.
fn brief() -> Value {
    json!({
        "task": full(),
        "neighbourhood": {
            "depends_on": [
                { "short_id": 1, "title": "one blocked predicate", "status": "done",
                  "annotation": { "created": "2026-09-14T08:00:00Z", "body": "Derived, not asked twice." } }
            ],
            "blocks": [
                { "short_id": 70, "title": "conformance test for the frozen fields", "status": "pending" },
                { "short_id": 71, "title": "the docs that still say otherwise", "status": "pending" }
            ]
        },
        "memory": {
            "hits": [
                { "title": "blocked is unmet_blockers non-empty", "source": "DESIGN.md", "snippet": "D145 rules that…" }
            ],
            "total": 1
        }
    })
}

/// `passed` of `total` criteria, so the bar is the only thing under test.
fn with_checks(passed: usize, total: usize) -> Value {
    let checks: Vec<Value> = (0..total)
        .map(|i| {
            json!({
                "body": format!("criterion {i}"),
                "state": if i < passed { "passed" } else { "open" }
            })
        })
        .collect();
    json!({
        "short_id": 5,
        "title": "a task with criteria",
        "status": "active",
        "checks": checks
    })
}

#[test]
fn a_minimal_task_is_a_four_row_box() {
    assert_eq!(
        task_card(&minimal(), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #76    │ Three field-test papercuts                             │
├─────────────┼────────────────────────────────────────────────────────┤
│ Status      │ pending · L · project tasqx                            │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
}

/// The same card in the style for destinations that mangle box-drawing
/// characters. Same geometry, different glyphs — and the middle rule is
/// indistinguishable from the others, which is the price of an alphabet with
/// one corner.
#[test]
fn the_ascii_style_draws_the_same_geometry() {
    assert_eq!(
        task_card(&minimal(), &ascii()),
        "\
+-------------+--------------------------------------------------------+
| Task #76    | Three field-test papercuts                             |
+-------------+--------------------------------------------------------+
| Status      | pending · L · project tasqx                            |
+-------------+--------------------------------------------------------+
"
    );
}

#[test]
fn a_full_task_draws_every_row_it_has_content_for() {
    assert_eq!(
        task_card(&full(), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #69    │ unmet_blockers ignored the dependent's own status, so  │
│             │ a done task answered blocked:false                     │
├─────────────┼────────────────────────────────────────────────────────┤
│ Status      │ active · H · est 45m · tracked 2h · project tasqx      │
│ Tags        │ render, markdown                                       │
│ Due         │ 2026-09-20T17:00:00Z                                   │
│ Description │ Derive blocked from unmet_blockers so the two cannot   │
│             │ disagree. One predicate, three readers.                │
│ Blocked by  │ #1 one blocked predicate, three readers                │
│             │ #2 the dependent's own status                          │
│ Unblocks    │ #70, #71                                               │
│ Checks      │ [#####-----] 2/4                                       │
│             │ [x] done dependent answers with an empty               │
│             │     unmet_blockers                                     │
│             │ [x] open dependent, one done and one open blocker      │
│             │ [!] the four shipped texts no longer say \"not yet      │
│             │     done\"                                              │
│             │ [ ] conformance                                        │
│ Budget      │ 150000 / 120000 fresh tokens — over                    │
│ Notes       │ 3 annotations, newest 2026-09-15T17:08:22Z             │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
}

/// The scheduling rows, and the proof they are omitted rather than blanked:
/// [`minimal`] sets none of them and has none.
#[test]
fn the_scheduling_rows_appear_only_when_they_are_set() {
    assert_eq!(
        task_card(&scheduled(), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #12    │ the weekly sweep                                       │
├─────────────┼────────────────────────────────────────────────────────┤
│ Status      │ pending · M · project tasqx                            │
│ Due         │ 2026-09-20T17:00:00Z                                   │
│ Scheduled   │ 2026-09-18T09:00:00Z                                   │
│ Wait        │ 2026-09-17T09:00:00Z                                   │
│ Repeats     │ FREQ=WEEKLY                                            │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
    let bare = task_card(&minimal(), &unicode());
    for label in ["Due", "Scheduled", "Wait", "Repeats"] {
        assert!(
            !bare.contains(label),
            "{label} on a task that has none:\n{bare}"
        );
    }
}

/// A closed task reports what came of it; an open one does not, because on an
/// open task the newest annotation is a progress note and "Delivered" would
/// claim work that has not happened.
#[test]
fn only_a_closed_task_reports_what_was_delivered() {
    assert_eq!(
        task_card(&done(), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #69    │ one blocked predicate                                  │
├─────────────┼────────────────────────────────────────────────────────┤
│ Status      │ done · H · project tasqx                               │
│ Description │ Approach: derive the flag from the list.               │
│ Depends on  │ #1, #2 — all done                                      │
│ Delivered   │ Shipped: one predicate spelled once, read three ways.  │
│ Notes       │ 2 annotations, newest 2026-09-15T17:08:22Z             │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
    assert!(!task_card(&full(), &unicode()).contains("Delivered"));
}

/// A scrubbed note is counted on the Notes row, because the card's own count
/// cannot show it any other way (D148).
///
/// `annotations_total` excludes removed rows on purpose — that is the number
/// every client's paging arithmetic reads — so a card that said "2
/// annotations" over a task that has had three is telling the truth about the
/// page and hiding the audit trail. The suffix is the whole change: no new
/// row, so every other card is byte-identical.
#[test]
fn the_notes_row_counts_what_was_removed() {
    assert_eq!(
        task_card(&scrubbed(), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #69    │ one blocked predicate                                  │
├─────────────┼────────────────────────────────────────────────────────┤
│ Status      │ done · H · project tasqx                               │
│ Description │ Approach: derive the flag from the list.               │
│ Depends on  │ #1, #2 — all done                                      │
│ Delivered   │ Shipped: one predicate spelled once, read three ways.  │
│ Notes       │ 2 annotations, newest 2026-09-15T17:08:22Z, 1 removed  │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
    assert!(
        !task_card(&done(), &unicode()).contains("removed"),
        "a task with nothing removed keeps the row it always had"
    );
}

/// The oldest annotation PRESENT is not the first note when the page was taken
/// from the recent end. The row says so rather than quoting the wrong one.
#[test]
fn the_description_says_so_when_the_first_note_is_off_the_page() {
    assert_eq!(
        task_card(&paged(), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #7     │ a long history                                         │
├─────────────┼────────────────────────────────────────────────────────┤
│ Status      │ active · M · project tasqx                             │
│ Description │ (first note not on this page — re-read with            │
│             │ annotations_offset)                                    │
│ Notes       │ 30 annotations, newest 2026-09-15T19:00:00Z            │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
}

/// The bar's two pinned ends: full means all of them, empty means none of
/// them, and rounding may not contradict either.
#[test]
fn the_progress_bar_never_rounds_to_full_or_to_empty() {
    let bar = |passed, total| {
        let card = task_card(&with_checks(passed, total), &unicode());
        let line = card.lines().find(|l| l.contains('[')).unwrap_or_default();
        // The value cell: between the second and third border.
        line.split('│')
            .nth(2)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    assert_eq!(bar(1, 30), "[#---------] 1/30");
    assert_eq!(bar(29, 30), "[#########-] 29/30");
    assert_eq!(bar(30, 30), "[##########] 30/30");
    assert_eq!(bar(0, 30), "[----------] 0/30");
    // Under three criteria the bar says less than the list under it, so there
    // is none: the first line is the first criterion.
    assert_eq!(bar(1, 2), "[x] criterion 0");
}

/// The card half of a brief is the card, and the tail is `task_brief`'s own
/// tail — computed here by stripping the table half off `task_brief`, so a
/// change to either renderer's tail breaks this rather than quietly forking it.
#[test]
fn a_brief_card_is_the_card_plus_the_briefs_own_tail() {
    let b = brief();
    let table = task_brief(&b, &detail_opts());
    let head = task_detail(&full(), &detail_opts());
    let tail = table
        .strip_prefix(&head)
        .expect("task_brief still opens with task_detail");

    let expected = format!("{}{tail}", task_card(&b, &unicode()));
    assert_eq!(task_brief_card(&b, &unicode()), expected);
    // And the card half really is a card: the neighbourhood gives Unblocks the
    // titles a bare `task.get` cannot.
    assert!(task_brief_card(&b, &unicode())
        .contains("│ Unblocks    │ #70 conformance test for the frozen fields             │"));
}

/// The tail is reachable on its own, and composing it is the same bytes as
/// asking for the whole thing.
///
/// `brief_tail_text` is public for one caller — the MCP transport, which fences
/// ONLY the card half (D146) and so cannot use `task_brief_card` whole. Two ways
/// to build a brief card is two things to keep in step, and this is what keeps
/// them: a tail that appeared under one and not the other would otherwise show
/// up only in a chat reply nobody diffs.
#[test]
fn a_brief_card_is_the_card_composed_with_the_tail_on_its_own() {
    let b = brief();
    assert_eq!(
        task_brief_card(&b, &unicode()),
        format!("{}{}", task_card(&b, &unicode()), brief_tail_text(&b))
    );
    // A brief with no neighbourhood and no hits has no tail at all — the empty
    // string, not a heading over nothing.
    assert_eq!(brief_tail_text(&json!({ "task": minimal() })), "");
}

/// Nothing to render is still a card. A renderer that answered with an empty
/// string here would be worse than the JSON it replaces — the same rule
/// `task_detail` follows.
#[test]
fn a_task_with_no_readable_fields_is_still_a_box() {
    assert_eq!(
        task_card(&json!({}), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #0     │                                                        │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
}

/// The width promise, over every fixture: a card is a box, and a box is only a
/// box while every line is the same number of cells. Asserted per line so the
/// failure names the line that broke it.
#[test]
fn every_line_of_every_card_is_exactly_the_card_width() {
    let fixtures = [
        ("minimal", minimal()),
        ("full", full()),
        ("done", done()),
        ("scrubbed", scrubbed()),
        ("paged", paged()),
        ("scheduled", scheduled()),
        ("hostile", hostile()),
        ("brief", brief()),
        ("empty", json!({})),
        ("thirty checks", with_checks(1, 30)),
    ];
    for (name, value) in fixtures {
        for borders in [Borders::Unicode, Borders::Ascii] {
            let card = task_card(
                &value,
                &CardOpts {
                    detail: detail_opts(),
                    borders,
                },
            );
            assert!(!card.is_empty(), "{name}: empty card");
            for line in card.lines() {
                assert_eq!(
                    UnicodeWidthStr::width(line),
                    CARD_WIDTH,
                    "{name} ({borders:?}): line is not {CARD_WIDTH} cells:\n{line}"
                );
                assert!(
                    !line.chars().any(char::is_control),
                    "{name} ({borders:?}): control character in:\n{line:?}"
                );
            }
        }
    }
}

/// The hostile fixture's golden, so the width test above is not the only thing
/// standing between a CJK title and a broken box: this pins WHERE the breaks
/// fall, which is what a reader notices.
#[test]
fn wide_characters_and_unbreakable_words_break_at_the_column() {
    assert_eq!(
        task_card(&hostile(), &unicode()),
        "\
┌─────────────┬────────────────────────────────────────────────────────┐
│ Task #404   │ 日本語のタイトル 👨‍👩‍👧 with emoji and 漢字 mixed into a   │
│             │ line long enough to wrap twice over                    │
├─────────────┼────────────────────────────────────────────────────────┤
│ Status      │ active · H · project tasqx                             │
│ Description │ See https://example.com/a/very/long/path/that/never/br │
│             │ eaks/anywhere/at/all/because/it/has/no/spaces/in/it/x  │
│             │ for the argument.                                      │
│ Checks      │ [x] tab here and an escape [31mred[0m in the body      │
│             │ [ ] 漢字の受入基準がセル幅を二倍に数える場合でも枠は崩 │
│             │     れない                                             │
│ Notes       │ 1 annotation, newest 2026-09-15T10:00:00Z              │
└─────────────┴────────────────────────────────────────────────────────┘
"
    );
}

/// The existing markdown views are untouched by D146: the card sits beside
/// them, and `task_brief`'s tail moved into a shared helper without moving a
/// byte of its output. `tests/markdown_detail.rs` is the full proof; this is
/// the tripwire in the file that did the moving.
#[test]
fn the_table_views_are_unchanged_by_the_card() {
    let b = brief();
    let table = task_brief(&b, &detail_opts());
    assert!(table.starts_with(&task_detail(&full(), &detail_opts())));
    assert!(table.contains("\n### Depends on\n\n- **#1** one blocked predicate · done\n"));
    assert!(table.contains("\n### From memory\n\n"));
}

/// One annotation is both the first note and the last word, and a closed task
/// is entitled to both readings of it: the row labels say which question each
/// answers, and dropping one would leave a card that never quotes the only
/// thing written on the task.
#[test]
fn a_single_annotation_is_both_the_description_and_the_delivery() {
    let task = json!({
        "short_id": 3,
        "title": "one note only",
        "status": "done",
        "annotations": [{ "created": "2026-09-15T10:00:00Z", "body": "The only note." }],
        "annotations_total": 1,
        "annotations_offset": 0
    });
    let card = task_card(&task, &unicode());
    assert_eq!(
        card.matches("The only note.").count(),
        2,
        "expected it under both labels:\n{card}"
    );
    assert!(card.contains("│ Description │ The only note."), "{card}");
    assert!(card.contains("│ Delivered   │ The only note."), "{card}");
}

/// A card is a summary, so a paragraph longer than the box is cut at eight
/// lines with the marker that says so. The annotation itself is one `task.get`
/// away, which is why this may be lossy at all.
#[test]
fn a_paragraph_longer_than_the_card_is_cut_with_an_ellipsis() {
    let body = "lorem ipsum dolor sit amet ".repeat(40);
    let task = json!({
        "short_id": 3,
        "title": "a long note",
        "status": "active",
        "annotations": [{ "created": "2026-09-15T10:00:00Z", "body": body }],
        "annotations_total": 1,
        "annotations_offset": 0
    });
    let card = task_card(&task, &unicode());
    let described: Vec<&str> = card
        .lines()
        .skip_while(|l| !l.contains("Description"))
        .take_while(|l| l.contains("lorem") || l.contains("Description"))
        .collect();
    assert_eq!(
        described.len(),
        8,
        "eight lines, not {}:\n{card}",
        described.len()
    );
    let last = described.last().expect("a last line");
    assert!(
        last.trim_end_matches(['│', ' ']).ends_with('…'),
        "the cut line must say it was cut:\n{last}"
    );
    assert!(!card.contains("……"), "one marker, not two:\n{card}");
}

/// One fixture rendered under all three `TimeFormat`s, pinning the two halves
/// of D146's rule: `card_duration` (the Status row's `est`) is the SAME
/// compact spelling under every format, while `card_instant` (Due, and the
/// Notes row's newest) follows `opts.time` — except that `Both` prints the
/// calendar date rather than the full timestamp `fmt_instant` would, per
/// `docs/maintainers/terminal-style.md` §3: the stored clock is UTC and reads
/// as the wall clock to a reader who is not.
#[test]
fn card_durations_are_always_compact_and_card_instants_follow_time_format() {
    let task = json!({
        "short_id": 90,
        "title": "one fixture, three formats",
        "status": "pending",
        "priority": "H",
        "project": "demo",
        "estimate": "PT4H",
        "due": "2026-09-18T00:00:00Z",
        "annotations": [{ "created": "2026-09-15T23:59:30Z", "body": "note" }],
        "annotations_total": 1,
        "annotations_offset": 0
    });
    let now = at("2026-09-16T00:00:00Z");

    let value_of = |time: TimeFormat, label: &str| -> String {
        let opts = CardOpts {
            detail: DetailOpts { time, now },
            borders: Borders::Unicode,
        };
        let card = task_card(&task, &opts);
        let line = card
            .lines()
            .find(|l| l.contains(&format!("│ {label}")))
            .unwrap_or_else(|| panic!("no {label} row in:\n{card}"));
        line.split('│')
            .nth(2)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    // The Status row's `est 4h`: unchanged across all three, because a
    // duration a reader compares at a glance is one compact unit whatever the
    // task's own `TimeFormat` is.
    for time in [TimeFormat::Iso, TimeFormat::Relative, TimeFormat::Both] {
        assert_eq!(
            value_of(time, "Status"),
            "pending · H · est 4h · project demo",
            "{time:?}: card_duration must not vary with TimeFormat"
        );
    }

    assert_eq!(value_of(TimeFormat::Iso, "Due"), "2026-09-18T00:00:00Z");
    assert_eq!(value_of(TimeFormat::Relative, "Due"), "in 2 days");
    assert_eq!(value_of(TimeFormat::Both, "Due"), "2026-09-18 (in 2 days)");

    assert_eq!(
        value_of(TimeFormat::Iso, "Notes"),
        "1 annotation, newest 2026-09-15T23:59:30Z"
    );
    assert_eq!(
        value_of(TimeFormat::Relative, "Notes"),
        "1 annotation, newest just now"
    );
    assert_eq!(
        value_of(TimeFormat::Both, "Notes"),
        "1 annotation, newest 2026-09-15 (just now)"
    );
}

/// The two kinds of memory hit are told apart in the view, and the docs are
/// told first (D147).
///
/// The reservation is worth nothing to a reader who cannot see that it
/// happened: a ruling and a sibling's note render as the same bullet, and
/// "half the page is held for rulings" is a claim the page must be able to
/// back. Each label carries how many of that kind were shown out of how many
/// matched, so a reader can tell a page holding every ruling from one holding
/// the first of forty.
#[test]
fn the_memory_tail_labels_the_docs_and_lists_them_before_the_annotations() {
    let b = json!({
        "task": minimal(),
        "memory": {
            "count": 2,
            "total": 2,
            "has_more": false,
            "reserved_docs": 5,
            "docs_total": 1,
            "annotations_total": 1,
            "hits": [
                { "kind": "doc", "title": "Retry audit ruling", "source": "DESIGN.md",
                  "snippet": "every retry is bounded" },
                { "kind": "annotation", "title": "audit the retry path", "source": "task:#12",
                  "snippet": "the retry path tests…" }
            ]
        }
    });
    assert_eq!(
        brief_tail_text(&b),
        "\
\n### From memory\n\n\
**Docs** — 1 of 1\n\n\
- **Retry audit ruling** · `DESIGN.md`\n  every retry is bounded\n\n\
**Annotations** — 1 of 1\n\n\
- **audit the retry path** · `task:#12`\n  the retry path tests…\n"
    );
    let tail = brief_tail_text(&b);
    assert!(
        tail.find("**Docs**") < tail.find("**Annotations**"),
        "the reserved half is the half read first: {tail}"
    );
}

/// A hit that names no kind is still printed, unlabelled — the shape the tail
/// rendered before D147, and the one an older recorded response still has.
/// Dropping it would make a renderer that silently loses a hit it does not
/// recognise, which is the failure mode the labels exist to prevent.
#[test]
fn a_hit_with_no_kind_keeps_the_flat_list_it_had_before_the_labels() {
    let tail = brief_tail_text(&brief());
    assert_eq!(
        tail,
        "\
\n### Depends on\n\n- **#1** one blocked predicate · done\n  > Derived, not asked twice.\n\
\n### Blocks\n\n- **#70** conformance test for the frozen fields · pending\n\
- **#71** the docs that still say otherwise · pending\n\
\n### From memory\n\n\
- **blocked is unmet_blockers non-empty** · `DESIGN.md`\n  D145 rules that…\n"
    );
}
