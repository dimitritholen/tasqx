//! Drift guards for the command wiki under `docs/wiki/`.
//!
//! The wiki is prose with no generator behind it, exactly like the README —
//! and the README's history says what happens next: sentences go stale in the
//! commit that makes them wrong, and nothing in the build can see it. Prose
//! *content* still cannot be asserted, but two structural claims can be, and
//! both are the kind whose failure a visitor hits before anyone here does:
//! a command with no wiki section, and a link that 404s on the repo page.
//!
//! Same conventions as `readme.rs`: deliberately minimal parsing (heading
//! prefixes and `](…)` spans, no markdown model), and every scan pins a floor
//! so an empty iteration cannot pass as a clean one.

use std::fs;
use std::path::{Path, PathBuf};

/// The wiki directory, two levels above this crate's manifest.
fn wiki_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/wiki")
}

/// Every wiki page, as `(file name, content)`.
fn pages() -> Vec<(String, String)> {
    let mut pages: Vec<(String, String)> = fs::read_dir(wiki_dir())
        .expect("docs/wiki exists")
        .map(|e| e.expect("readable dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("a file has a name")
                .to_string_lossy()
                .into_owned();
            let text = fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("{} is readable: {e}", p.display()));
            (name, text)
        })
        .collect();
    pages.sort();
    // Floor: the wiki shipped with Home plus thirteen topic pages. Fewer means
    // pages were lost, not that the scans below may quietly cover less.
    assert!(
        pages.len() >= 14,
        "docs/wiki holds only {} .md pages — where did the rest go?",
        pages.len()
    );
    pages
}

/// Every wiki page, plus the workspace README.
///
/// For the prose guards whose README twin D162 retired: the README stopped
/// carrying an exit-code roster and the dashboard's panel history, and a guard
/// that insisted on finding them there would have had to be deleted with them.
/// Scanning the README beside the wiki keeps the rule on it — a roster or a
/// retired panel that creeps back is judged exactly like one on a page — while
/// the floors stay the wiki's.
fn pages_and_readme() -> Vec<(String, String)> {
    let mut all = pages();
    let readme = wiki_dir().join("../../README.md");
    let text = fs::read_to_string(&readme)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", readme.display()));
    all.push(("README.md".to_string(), text));
    all
}

/// Every CLI verb must have a heading in the wiki.
///
/// The verb list comes from clap via [`tasqx_cli::subcommand_names`], the same
/// derivation the `--json` contract guard uses (D30), so a verb joins this
/// check on the day it is added. A heading counts when its text is exactly
/// `tasqx <verb>` or opens a subcommand of it (`tasqx memory add` documents
/// `memory`) — the shape every page already uses, and the one a reader scans
/// a page for.
///
/// One-directional on purpose: a heading for a verb clap no longer has is NOT
/// caught here, because the wiki also writes headings that are not verb
/// sections and telling those apart would need the prose itself to be
/// machine-readable. The misleading direction — a verb a visitor cannot look
/// up — is the one that fails.
#[test]
fn every_cli_verb_has_a_wiki_heading() {
    let verbs = tasqx_cli::subcommand_names();
    assert!(
        verbs.len() >= 30,
        "clap reports only {} subcommands — the derivation broke and this \
         guard is checking almost nothing",
        verbs.len()
    );

    let headings: Vec<String> = pages()
        .iter()
        .flat_map(|(_, text)| {
            text.lines()
                .filter(|l| l.starts_with('#'))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect();

    for verb in &verbs {
        let exact = format!("tasqx {verb}");
        let prefixed = format!("tasqx {verb} ");
        assert!(
            headings
                .iter()
                .any(|h| *h == exact || h.starts_with(&prefixed)),
            "no wiki page has a `tasqx {verb}` heading. A visitor told the wiki \
             explains every command looks this verb up and finds nothing — add \
             a section (heading `## tasqx {verb}`) to the page it belongs on, \
             and its row to Home.md."
        );
    }
}

/// Every relative link in the wiki must name a file that exists.
///
/// The wiki lives or dies by its cross-links: Home.md is a table of links, and
/// every page points sideways at its neighbours. A renamed page leaves the
/// index 404-ing, and nothing else reads these paths — the README link guard
/// covers only the links the README itself makes.
///
/// Anchors are stripped, not verified: `Page.md#tasqx-add` is checked as
/// `Page.md`. Verifying anchors would mean modelling GitHub's slugger, and a
/// wrong anchor still lands the reader on the right page.
#[test]
fn wiki_relative_links_point_at_files_that_exist() {
    let dir = wiki_dir();
    let mut checked = 0;
    for (name, text) in pages() {
        let mut rest = text.as_str();
        while let Some(i) = rest.find("](") {
            let tail = &rest[i + 2..];
            let Some(end) = tail.find(')') else { break };
            let target = &tail[..end];
            rest = &tail[end..];
            // Relative paths only; http(s) targets and same-page anchors are
            // not this test's claim.
            if target.starts_with("http") || target.starts_with('#') || target.is_empty() {
                continue;
            }
            let path = target.split('#').next().expect("split yields a first part");
            assert!(
                dir.join(path).exists(),
                "{name} links {target:?}, and {path:?} does not exist relative \
                 to docs/wiki — the repo page 404s right where the wiki points"
            );
            checked += 1;
        }
    }
    // Floor: Home.md alone carries a link per command row. A scan that finds
    // fewer has lost links, not gained tidiness.
    assert!(
        checked >= 30,
        "the link scan checked only {checked} targets"
    );
}

/// Every wiki page must be reachable from Home.md.
///
/// Home.md is the index the README points at. A page no index names still
/// renders, still passes the link guard above, and is still unreachable by
/// anyone who did not already know it existed — which is how a page silently
/// vanishes while staying in the tree.
#[test]
fn home_links_every_wiki_page() {
    let pages = pages();
    let home = &pages
        .iter()
        .find(|(name, _)| name == "Home.md")
        .expect("docs/wiki/Home.md exists")
        .1;
    for (name, _) in &pages {
        if name == "Home.md" {
            continue;
        }
        assert!(
            home.contains(&format!("({name}")),
            "Home.md never links {name} — the page exists but no visitor can \
             find it from the index the README points at"
        );
    }
}

/// Every exit code the CLI can leave with.
///
/// Derived: [`ErrorCode::exit_code`] owns the mapping, so a renumbered code
/// reaches the prose guards without anyone retyping a literal. `0` is success
/// and belongs to no error code. The variants are listed here because
/// `ErrorCode::ALL` is test-local to `tasqx-core` on purpose ("a `pub ALL`
/// would be API surface with no consumer"), and a guard is not reason enough
/// to widen a published API.
fn expected_exit_codes() -> Vec<i32> {
    use tasqx_core::ErrorCode;
    // MEMBERSHIP is derived, not just the numbers. This used to retype the
    // five variants, so adding a sixth left every roster green; and the floor
    // underneath (`>= 5`, against six codes) meant deleting `6` from all three
    // documents passed too. Both halves came from the same mistake: asking how
    // MANY instead of asking WHICH.
    let mut codes: Vec<i32> = ErrorCode::all().iter().map(|c| c.exit_code()).collect();
    codes.push(0);
    codes.sort_unstable();
    codes.dedup();
    codes
}

/// Paragraph-ish chunks: blank-line blocks, split again at bullet starts.
///
/// Without the second split a bullet list is one paragraph, and a claim made
/// in one bullet satisfies a check aimed at another — which is how a roster
/// guard passes over a roster that is missing an entry.
fn chunks(text: &str) -> Vec<String> {
    text.split("\n\n")
        .flat_map(|para| para.split("\n- "))
        .map(str::to_string)
        .collect()
}

/// Every field `modify --clear` accepts must be named where the wiki lists them.
///
/// It drifted to eight of nine. `tracked` joined `CLEARABLE`, the generated
/// guide picked it up through the binding it already had
/// (`documented_clear_fields_match_the_parser`), and the hand-written page
/// went on listing the set it was written against. The flag refuses nothing —
/// a reader simply never learns the field can be cleared at all.
#[test]
fn the_wiki_lists_every_clearable_field() {
    let fields = tasqx_cli::clearable_fields();
    assert!(
        fields.len() >= 8,
        "the parser reports only {} clearable fields — the derivation broke",
        fields.len()
    );

    const OPENS: &str = "`--clear` works for:";
    let (page, text) = pages()
        .into_iter()
        .find(|(_, t)| t.contains(OPENS))
        .expect("some wiki page lists what `--clear` accepts");
    // The ROSTER, not the paragraph: the list ends at its own sentence break,
    // so a field dropped from it cannot be satisfied by the same word
    // appearing in the prose underneath ("Tags are also not `tracked`").
    // Equality, not containment, so a field the parser drops must leave too.
    let list = text
        .split(OPENS)
        .nth(1)
        .expect("checked above")
        .split(". ")
        .next()
        .expect("the list ends at the first sentence break");
    let named: Vec<&str> = list.split('`').skip(1).step_by(2).collect();

    assert_eq!(
        named, fields,
        "{page}'s `--clear` roster is not the parser's. The page lists \
         {named:?}; `CLEARABLE` is {fields:?}. A reader cannot clear a field \
         the page omits, and will try to clear one it invents."
    );
}

/// Every `tasqx memory` subcommand must have a wiki section.
///
/// The page documented five of seven: `memory list` — a full-screen browser
/// (D121) — and `memory update` appeared nowhere under `docs/wiki/`, which
/// left the page calling `rm` the only way to deal with a wrong document when
/// `update` corrects one in place.
///
/// The verb-level guard above cannot catch this: `memory` counts as covered
/// the moment any `tasqx memory <anything>` heading exists.
#[test]
fn the_wiki_documents_every_memory_subcommand() {
    let subs = tasqx_cli::memory_subcommand_names();
    assert!(
        subs.len() >= 5,
        "clap reports only {} memory subcommands — the derivation broke",
        subs.len()
    );

    let headings: Vec<String> = pages()
        .iter()
        .flat_map(|(_, text)| {
            text.lines()
                .filter(|l| l.starts_with('#'))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect();

    for sub in &subs {
        let want = format!("tasqx memory {sub}");
        assert!(
            headings.contains(&want),
            "no wiki page has a `{want}` heading — the command ships and the \
             wiki that claims to explain every command never mentions it"
        );
    }
}

/// A wiki (or README) chunk that lists exit codes must list every code the CLI
/// can leave with.
///
/// Two were missing, and for the same reason. `tasqx --no-daemon watch` exits
/// 1; `echo '{"tasqx":"2",…}' | tasqx api` exits 6. Every roster jumped 0 → 2,
/// so a script author reads it as an enumeration they may rely on and treats
/// both as impossible.
///
/// The numbers are DERIVED from [`ErrorCode::exit_code`], the mapping that
/// decides them, so renumbering a code moves this guard rather than leaving it
/// asserting a stale literal. The variant list is spelled out because
/// `ErrorCode::ALL` is deliberately test-local to `tasqx-core`; a floor below
/// catches a list that stops enumerating.
///
/// Backticked spellings only. The first version also accepted a bare `" 1 "`,
/// which "and 1 other thing to know" satisfies while exit 1 stays
/// undocumented.
#[test]
fn every_wiki_exit_code_roster_names_every_code() {
    let codes = expected_exit_codes();
    let mut rosters = 0;
    for (name, text) in pages_and_readme() {
        for chunk in chunks(&text) {
            let lower = chunk.to_lowercase();
            if !lower.contains("exit code")
                || !lower.contains("not found")
                || !lower.contains("conflict")
            {
                continue;
            }
            rosters += 1;
            for code in &codes {
                assert!(
                    chunk.contains(&format!("`{code}`")),
                    "{name} lists exit codes and never names `{code}`. The CLI \
                     can leave with {codes:?}, and a roster a script author \
                     reads as exhaustive must be:\n{chunk}"
                );
            }
        }
    }
    assert!(
        rosters >= 2,
        "found only {rosters} exit-code rosters in the wiki — the scan stopped \
         finding them and this guard is checking almost nothing"
    );
}

/// The dashboard page must say what `--json` actually carries.
///
/// The screen has six panels and the document writes a payload for four:
/// `pulse` and `effort` have no `want(…)` block in `json::document`, so
/// `--panels pulse,effort` answers with an envelope and nothing else. A page
/// that calls the document "all panels" sends a script author looking for two
/// keys that are never written.
#[test]
fn the_dashboard_page_says_what_the_json_document_carries() {
    let payload = tasqx_cli::dashboard_json_panel_names();
    assert!(
        payload.len() >= 3,
        "only {} payload panels — the derivation broke",
        payload.len()
    );

    let (name, text) = pages()
        .into_iter()
        .find(|(n, _)| n == "Dashboard-and-Live-View.md")
        .expect("docs/wiki/Dashboard-and-Live-View.md exists");

    assert!(
        !text.contains("all panels as one JSON document"),
        "{name} still calls the document \"all panels\" — it carries {payload:?}"
    );

    // The claim lives in ONE chunk, and that chunk has to carry it whole.
    // Asserting the names appear somewhere on the page passed with the entire
    // bullet deleted, because the six-panel sentence above already names all
    // four — the page said nothing about `--json` and the guard was green.
    let claim = chunks(&text)
        .into_iter()
        .find(|c| c.contains("--json") && c.to_lowercase().contains("payload"))
        .unwrap_or_else(|| {
            panic!("{name} never states what the `--json` document carries:\n{text}")
        });

    for p in &payload {
        assert!(
            claim.contains(&format!("`{p}`")),
            "{name} states what `--json` carries without naming `{p}`:\n{claim}"
        );
    }

    // Both counts, in the one sentence that contrasts them: the screen's and
    // the document's are different numbers, and the claim is the contrast.
    let screen = tasqx_cli::dashboard_panel_names();
    const WORDS: [&str; 9] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
    ];
    for (count, which) in [(payload.len(), "payload"), (screen.len(), "screen")] {
        assert!(
            claim.contains(WORDS[count]),
            "{name}'s `--json` claim never says {:?}, the {which} panel count:\n{claim}",
            WORDS[count]
        );
    }
}

/// No wiki (or README) chunk may present a panel D80 retired as a panel that
/// ships.
///
/// Two pages sent readers hunting for a BLOCKED panel that D80 folded into
/// TASKS. Naming one of the retired panels is fine — saying what happened to
/// it is the point — so the rule is that the chunk which calls it a panel must
/// also say it was retired.
#[test]
fn no_wiki_chunk_presents_a_retired_panel_as_current() {
    let retired = tasqx_cli::retired_dashboard_panel_names();
    assert!(
        retired.len() >= 5,
        "only {} retired panel names — the derivation broke",
        retired.len()
    );

    let mut mentions = 0;
    for (name, text) in pages_and_readme() {
        for chunk in chunks(&text) {
            let lower = chunk.to_lowercase();
            if !lower.contains("panel") {
                continue;
            }
            for upper in retired.iter().map(|n| n.to_uppercase()) {
                if !chunk.contains(&upper) {
                    continue;
                }
                mentions += 1;
                assert!(
                    lower.contains("d80") || lower.contains("retired") || lower.contains("folded"),
                    "{name} calls {upper} a panel without saying D80 folded it into \
                     TASKS, so a reader goes looking for a panel that does not \
                     ship:\n{chunk}"
                );
            }
        }
    }
    assert!(
        mentions >= 1,
        "no wiki chunk names a retired panel beside the word panel — this guard \
         is checking nothing"
    );
}

/// The dashboard page's panel roster must be the shipped one, count included.
///
/// It said "eight panels" and named what the pre-D80 screen had. The count is
/// `PANEL_NAMES.len()`; the names are `PANEL_NAMES`.
#[test]
fn the_dashboard_page_names_every_panel_that_ships() {
    let panels = tasqx_cli::dashboard_panel_names();
    assert!(
        panels.len() >= 4,
        "only {} panels — the derivation broke",
        panels.len()
    );

    let (name, text) = pages()
        .into_iter()
        .find(|(n, _)| n == "Dashboard-and-Live-View.md")
        .expect("docs/wiki/Dashboard-and-Live-View.md exists");

    let word = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
    ][panels.len()];
    assert!(
        text.contains(&format!("{word} panels")),
        "{name} must say {:?} — `tasqx --json dashboard` answers {panels:?}",
        format!("{word} panels")
    );
    for p in &panels {
        assert!(
            text.contains(&format!("`{p}`")),
            "{name} never names the `{p}` panel, though the screen ships it"
        );
    }
}

/// The `why` sample must be what `why` prints, not what it printed before D122.
///
/// The page showed a "Why #42 has urgency" header, a `due_proximity` row and a
/// `= total` footer. The command prints no header, spells the row `deadline`,
/// and closes with `urgency` — so a reader matching the page against their own
/// terminal finds three differences and no explanation for any of them.
#[test]
fn the_why_sample_uses_the_rows_the_command_prints() {
    let (name, text) = pages()
        .into_iter()
        .find(|(_, t)| t.contains("$ tasqx why"))
        .expect("some wiki page shows a `tasqx why` sample");

    for gone in ["Why #", "due_proximity", "= total"] {
        assert!(
            !text.contains(gone),
            "{name} still shows {gone:?}, which `tasqx why` stopped printing at D122"
        );
    }
    // Same reason as the README's twin: the figures move with the clock, so
    // the page says so rather than printing a number that is wrong by
    // teatime.
    assert!(
        text.contains("are an illustration; the rows are the contract"),
        "{name} prints urgency figures from a capture without saying they move \
         with the clock — the numbers are wrong within hours"
    );
    for row in ["priority", "deadline", "age", "urgency"] {
        assert!(
            text.contains(row),
            "{name}'s `why` sample is missing the {row:?} row the command prints"
        );
    }
}
