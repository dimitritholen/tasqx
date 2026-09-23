use super::*;
use crate::render::test_support::*;
use crate::theme::{self, Caps, Ctx};
use serde_json::json;

/// Task #12/D135: `docs.body` is never rewritten, so `doc_summary` reads
/// exactly the body `memory.get` would hand back (the demo store's own
/// `release-process` doc, verbatim) — a labelled `description` value,
/// not the whole `key: value` line and not the fence. Shares
/// `tasqx_core::frontmatter::split` with `tui::memory::doc_lines`
/// instead of its own `---` scan (review finding).
#[test]
fn doc_summary_reads_the_bare_description_from_a_body_memory_get_would_return() {
    let body = "---\ndescription: How an SDK release is cut, tagged and announced\n---\n\
                     Cut the release branch on Monday.";
    assert_eq!(
        doc_summary(body),
        "How an SDK release is cut, tagged and announced"
    );
}

/// No `description` key: the summary falls back to the first line of
/// prose AFTER the fence, not the fence's other fields.
#[test]
fn doc_summary_falls_back_to_the_first_prose_line_when_there_is_no_description() {
    let body = "---\nname: deploy\nauthor: infra\n---\nRoll out the change on Monday.";
    assert_eq!(doc_summary(body), "Roll out the change on Monday.");
}

/// #346 review: a memory title gives way only once the columns beside it
/// have gone. Widest-first shrinking cut `Cut build time under five
/// minutes` to 22 cells at 100 columns while a 20-cell match fragment and
/// the source printed beside it, and `memory list` cut titles to 13 cells
/// at 60 to keep PROJECT. The title's floor is now what the terminal can
/// give it beside the id, in both tables.
#[test]
fn a_memory_title_gives_way_only_after_the_columns_beside_it() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(80);
    let hit = |title: &str, source: &str| {
        json!({ "id": "01a0903d-243b-7842-8f5c-184005a2d8f2", "kind": "doc",
                    "title": title, "source": source,
                    "snippet": "Blocked on the release of the new runner image; revisit after the infra release train" })
    };
    let out = memory_hits(
        &ctx,
        &json!({ "count": 2, "total": 2, "hits": [
                hit("Cut build time under five minutes", "docs/ci/build-time.md"),
                hit("release-process", "docs/release.md"),
            ] }),
        "release",
        false,
    );
    assert!(
        out.contains("Cut build time under five minutes"),
        "the title was cut while other columns kept room: {out}"
    );
    assert!(!out.contains("docs/ci/build-time.md"), "{out}");

    let ctx = ctx.with_cols(60);
    let doc = |title: &str| {
        json!({ "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "title": title,
                    "project": "website", "modified": "2026-08-01T00:00:00Z" })
    };
    let out = memory_table(
        &ctx,
        &json!({ "total": 2, "docs": [doc("pricing-page-decisions"), doc("dentist")] }),
        anchor(),
    );
    assert!(
        out.contains("pricing-page-decisions"),
        "the title was cut to keep PROJECT: {out}"
    );
    assert!(!out.contains("PROJECT"), "{out}");
}

/// Round 1 review of #346: a title never gives way below twelve cells,
/// the floor at which it stops telling one doc from another; past it the
/// row overflows, as every table's does (D120). The title floor had no
/// lower bound, so at 40 columns `memory list` printed `...` on every row
/// and `memory search` printed `r…`, and in ASCII the row still wrapped.
#[test]
fn a_memory_title_keeps_twelve_cells_on_a_narrow_terminal() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(40);
    let doc = |title: &str| {
        json!({ "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "title": title,
                    "project": "website", "modified": "2026-08-01T00:00:00Z" })
    };
    let list = memory_table(
        &ctx,
        &json!({ "total": 1, "docs": [doc("android-token-refresh")] }),
        anchor(),
    );
    assert!(list.contains("android-t"), "{list}");
    let hits = memory_hits(
        &ctx,
        &json!({ "count": 1, "total": 1, "hits": [
                { "id": "01a0903c-bff0-76a2-9bcb-5428786a56c4", "kind": "doc",
                  "title": "release-process", "source": "docs/release.md",
                  "snippet": "How an SDK release is cut" } ] }),
        "release",
        false,
    );
    assert!(hits.contains("release-p"), "{hits}");
    // And the handle, the one thing a search record never gives up
    // (D125(a)), is still on the line: the row overflows instead.
    assert!(
        hits.contains("01a0903c-bff0-76a2-9bcb-5428786a56c4"),
        "the handle was dropped to fit: {hits}"
    );
}

/// D125: the summary names the expression that ran, which a plain query
/// quotes, so it stands apart from the count even where dim does not
/// reach the screen (`mono`, NO_COLOR). It printed the query bare, and
/// `the   5 hits · 2 shown` read as a sentence.
#[test]
fn a_search_summary_sets_the_expression_off() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
    let out = memory_hits(
        &ctx,
        &json!({ "count": 0, "total": 0, "hits": [], "matched": "\"the\"" }),
        "the",
        false,
    );
    assert!(out.starts_with("\"the\"   0 hits"), "{out}");
}

/// #346 review: on a miss the D69 hint is the only thing on the screen
/// that says what to do next, so it is not painted in the dimmest role.
#[test]
fn a_search_miss_does_not_dim_its_hint() {
    let ctx = Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        },
    );
    let out = memory_hits(
        &ctx,
        &json!({ "count": 0, "total": 0, "hits": [], "matched": "\"zebra\"" }),
        "zebra",
        false,
    );
    let hint = out
        .lines()
        .find(|l| l.contains("every term was required"))
        .unwrap_or_else(|| panic!("no hint: {out:?}"));
    let muted = ctx.paint("muted", "\u{0}");
    let muted = muted.split('\u{0}').next().expect("an SGR prefix");
    assert!(!hint.contains(muted), "the hint is muted: {hint:?}");

    // Round 2 review of #346: the bounded note does the same job (it
    // names the next command), so it is painted the same way.
    let hit = json!({ "id": "01a0903c-bff0-76a2-9bcb-5428786a56c4", "kind": "doc",
                          "title": "release-process", "source": "", "snippet": "cut" });
    let out = memory_hits(
        &ctx,
        &json!({ "count": 1, "total": 5, "hits": [hit], "matched": "\"cut\"" }),
        "cut",
        false,
    );
    let note = out
        .lines()
        .find(|l| l.contains("--limit 5"))
        .unwrap_or_else(|| panic!("no note: {out:?}"));
    assert!(!note.contains(muted), "the --limit note is muted: {note:?}");
}

/// D69, and the round-2 review of D125: a miss names the WHOLE expression
/// it ran. The summary's label is cut to half the width, so a twelve-term
/// question printed `"how" "do" ... "rel...   0 hits` and the advice to use
/// fewer terms could not be acted on: the reader could not see which terms
/// there were. The note carries it whole, wrapped, never cut.
#[test]
fn a_search_miss_names_the_whole_expression() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
    let expr = "\"how\" \"do\" \"i\" \"cut\" \"a\" \"release\" \"for\" \"the\" \"sdk\"";
    let out = memory_hits(
        &ctx,
        &json!({ "count": 0, "total": 0, "hits": [], "matched": expr }),
        "how do i cut a release for the sdk",
        false,
    );
    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(expr),
        "the expression is not on the screen whole: {out}"
    );
}

/// D125(a) rests on the summary's label being CUT: that is why the note
/// carrying the expression whole is its only complete copy rather than the
/// summary said twice (rule 11). Nothing failed if `summary_line` stopped
/// truncating, so this pins the half the other guard does not.
#[test]
fn a_search_summary_keeps_its_label_cut() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
    let expr = "\"how\" \"do\" \"i\" \"cut\" \"a\" \"release\" \"for\" \"the\" \"sdk\"";
    let out = memory_hits(
        &ctx,
        &json!({ "count": 0, "total": 0, "hits": [], "matched": expr }),
        "how do i cut a release for the sdk",
        false,
    );
    let summary = out.lines().next().expect("a summary line");
    assert!(
        !summary.contains(expr),
        "the label is not cut, so the note repeats it (rule 11): {summary:?}"
    );
    assert!(
        summary.contains("0 hits"),
        "the count left the summary: {summary:?}"
    );
    // ...and the whole of it is still on the screen, in the note.
    let rest = out
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(rest.contains(expr), "no complete copy anywhere: {out}");
}

/// Round 2 review of D125: the advice fits the search that was run. Telling
/// someone who already passed `--raw` to use `--raw` is advice they cannot
/// take, and a raw expression is not quoted by the engine, so the summary
/// quotes it here to set it off.
#[test]
fn a_raw_search_miss_advises_something_the_reader_can_do() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(60);
    let out = memory_hits(
        &ctx,
        &json!({ "count": 0, "total": 0, "hits": [],
                     "matched": "title:release OR body:ship" }),
        "title:release OR body:ship",
        true,
    );
    assert!(
        !out.contains("--raw"),
        "advises the flag the reader already used: {out}"
    );
    let summary = out.lines().next().expect("a summary line");
    assert!(
        summary.contains("\"title:release OR body:ship\""),
        "the summary does not set the raw expression off: {summary:?}"
    );
    // Flattened: `prose` wraps the note at words, and a raw expression has
    // spaces in it.
    let note = out
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        note.contains("nothing matched \"title:release OR body:ship\""),
        "the note does not name the expression it ran: {note:?}"
    );
}

/// Round 2 review of D125: a record's lines rank by indent, so the
/// hierarchy survives a terminal that draws no colour. `muted` emits
/// nothing under NO_COLOR, and the handle's own line and the matched words
/// under it were both indented two cells and both unpainted.
#[test]
fn a_search_record_ranks_its_lines_by_indent() {
    let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(40);
    let out = memory_hits(
        &ctx,
        &json!({ "count": 1, "total": 1, "hits": [
                { "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "kind": "doc",
                  "title": "pricing-page-decisions", "source": "",
                  "snippet": "The release of v2 waits for legal" } ] }),
        "release",
        false,
    );
    let lines: Vec<&str> = out.lines().collect();
    let handle = lines
        .iter()
        .find(|l| l.contains("01a0903c-"))
        .unwrap_or_else(|| panic!("no handle line: {out}"));
    let words = lines
        .iter()
        .find(|l| l.contains("The release of v2"))
        .unwrap_or_else(|| panic!("no matched-words line: {out}"));
    assert!(
        handle.starts_with("  ") && !handle.starts_with("   "),
        "handle line: {handle:?}"
    );
    assert!(
        words.starts_with("    ") && !words.starts_with("     "),
        "the matched words sit at the handle's depth: {words:?}"
    );
}

/// Round 2 review of #346: each search record fits its head line to its
/// OWN handle. The head lines were fitted as one table, so a 36-cell doc
/// id set the title width for an annotation whose handle is 17 cells:
/// `Cut build time unde...  annotation on #55` beside 19 empty cells at 60
/// columns. And where a title and its handle cannot share a line, the
/// handle takes the next one rather than the title being cut to make
/// room: at 40 columns every title printed whole on main.
#[test]
fn a_search_record_fits_its_head_line_to_its_own_handle() {
    use unicode_width::UnicodeWidthStr;
    let hits = json!({ "count": 2, "total": 2, "hits": [
            { "id": "01a0903c-c020-70e3-8c9a-7f625ab84c91", "kind": "doc",
              "title": "pricing-page-decisions", "source": "notes/pricing.md",
              "snippet": "The release of v2 waits for legal to sign off" },
            { "id": "01a0903d-243b-7842-8f5c-184005a2d8f2", "kind": "annotation",
              "title": "Cut build time under five minutes", "source": "task:#55",
              "snippet": "Blocked on the release of the new runner image" },
        ] });
    let at = |cols: usize| {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols);
        memory_hits(&ctx, &hits, "release", false)
    };
    let out = at(60);
    assert!(
        out.lines()
            .any(|l| l.starts_with("Cut build time under five minutes")
                && l.contains("annotation on #55")),
        "the annotation's title was cut beside room it did not need: {out}"
    );
    let out = at(40);
    for l in out.lines() {
        assert!(l.width() <= 40, "{} cells at 40: {l:?}\n{out}", l.width());
    }
    let lines: Vec<&str> = out.lines().collect();
    let doc = lines
        .iter()
        .position(|l| *l == "pricing-page-decisions")
        .unwrap_or_else(|| panic!("the doc title was cut or shares a line: {out}"));
    assert_eq!(
        lines[doc + 1].trim(),
        "01a0903c-c020-70e3-8c9a-7f625ab84c91",
        "the handle is not on the line under the title: {out}"
    );
    assert!(
        lines.contains(&"Cut build time under five minutes"),
        "{out}"
    );
}
