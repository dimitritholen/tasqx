//! Drift guards for the command wiki under `docs/wiki/`.
//!
//! The wiki is prose with no generator behind it, exactly like the README —
//! and the README's history says what happens next: sentences go stale in the
//! commit that makes them wrong, and nothing in the build can see it. Prose
//! *content* still cannot be asserted, but two structural claims can be, and
//! both are the kind whose failure a visitor hits before anyone here does:
//! a command with no wiki section, and a link that 404s on the repo page.
//! A third is that every `tasqx …` command the wiki and the guides show
//! still parses (#705).
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

// ---- Every `tasqx …` command the docs show parses (#705) -------------------

/// Every wiki page and every guide, as (`wiki/<page>` or `guides/<page>`,
/// content) — the two folders a reader copies commands out of.
fn wiki_and_guides() -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = pages()
        .into_iter()
        .map(|(name, text)| (format!("wiki/{name}"), text))
        .collect();
    let dir = wiki_dir().join("../guides");
    let mut guides: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("docs/guides exists")
        .map(|e| e.expect("readable dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    guides.sort();
    // Floor: seven guides shipped when this guard was written.
    assert!(
        guides.len() >= 7,
        "docs/guides holds only {} .md pages",
        guides.len()
    );
    for p in guides {
        let name = p.file_name().expect("a file has a name").to_string_lossy();
        let text =
            fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} is readable: {e}", p.display()));
        all.push((format!("guides/{name}"), text));
    }
    all
}

/// One piece of code a page shows a reader.
struct Code {
    /// 1-based line in the page.
    line: usize,
    text: String,
    /// `Some(info string)` for a line of a fenced block, `None` for an inline
    /// code span.
    fence: Option<String>,
}

/// Every fenced-block line and every inline code span on a page.
///
/// Deliberately small, like the rest of this file: a line opening with
/// three backticks toggles a fence and its remainder is the info string;
/// inline spans are the odd-numbered pieces of a line split on single
/// backticks, so a span never crosses a line. Inline spans are read in prose
/// and inside `markdown` fences (a page quoting a prompt is still markdown);
/// indented code blocks are not read at all. #707 reads the same spans for API
/// methods, MCP tools and config keys.
fn code_in(text: &str) -> Vec<Code> {
    let mut out = Vec::new();
    let mut fence: Option<String> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        if let Some(info) = raw.trim_start().strip_prefix("```") {
            fence = match fence {
                None => Some(info.trim().to_string()),
                Some(_) => None,
            };
            continue;
        }
        if let Some(info) = &fence {
            out.push(Code {
                line,
                text: raw.to_string(),
                fence: Some(info.clone()),
            });
            if info != "markdown" {
                continue;
            }
        }
        for span in raw.split('`').skip(1).step_by(2) {
            out.push(Code {
                line,
                text: span.to_string(),
                fence: None,
            });
        }
    }
    out
}

/// A shell word, or an operator (`|`, `&&`, `;`, `>`, …) as its own token.
#[derive(Debug, PartialEq)]
enum Sh {
    Word(String),
    Op(String),
}

/// POSIX-ish word splitting: single quotes literal, double quotes with `\`
/// escapes, `\` outside quotes escapes one character (a `\` before a newline
/// joins lines), a `#` opening a word comments out the rest, and `|&;<>` runs
/// are operators. `$VAR` and `$(…)` stay literal text — the value is not ours
/// to know, only that it lands in one argument. Returns `None` when a quote or
/// a trailing `\` is still open, so the caller can join the next line.
fn shell_split(s: &str) -> Option<Vec<Sh>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => word.push(c),
                            '\n' => {}
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => match chars.next()? {
                '\n' => {}
                c => {
                    in_word = true;
                    word.push(c);
                }
            },
            '#' if !in_word => break,
            '|' | '&' | ';' | '<' | '>' => {
                if in_word {
                    out.push(Sh::Word(std::mem::take(&mut word)));
                    in_word = false;
                }
                let mut op = c.to_string();
                while let Some(&n) = chars.peek() {
                    if !"|&;<>".contains(n) {
                        break;
                    }
                    op.push(n);
                    chars.next();
                }
                out.push(Sh::Op(op));
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(Sh::Word(std::mem::take(&mut word)));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        out.push(Sh::Word(word));
    }
    Some(out)
}

/// The `tasqx` invocations in one shell line, as argv vectors.
///
/// The line splits into commands at `|`, `||`, `&&`, `;` and `&`; a
/// redirection operator takes the word after it with it; leading `NAME=value`
/// words are environment, not arguments. A command whose first remaining word
/// is `tasqx` is kept, so `tasqx export | tasqx import -` is two.
fn tasqx_invocations(tokens: &[Sh]) -> Vec<Vec<String>> {
    let mut commands: Vec<Vec<String>> = vec![Vec::new()];
    let mut redirect = false;
    for t in tokens {
        match t {
            Sh::Op(op) if op.contains(['<', '>']) => redirect = true,
            Sh::Op(_) => commands.push(Vec::new()),
            Sh::Word(_) if redirect => redirect = false,
            Sh::Word(w) => commands.last_mut().expect("never empty").push(w.clone()),
        }
    }
    commands
        .into_iter()
        .map(|c| c.into_iter().skip_while(|w| is_env(w)).collect::<Vec<_>>())
        .filter(|c| c.first().is_some_and(|w| w == "tasqx"))
        .collect()
}

/// A `NAME=value` word: environment for the command after it.
fn is_env(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
    })
}

/// What a doc's placeholder stands for when the line is parsed.
///
/// Substituted in the text before it is split, since an unquoted `<id>` is
/// a redirection to a shell. A `<word…` left over afterwards fails the guard
/// rather than reaching clap, so a new placeholder is a decision someone makes
/// here, not a word that happens to parse.
const PLACEHOLDERS: &[(&str, &str)] = &[
    ("<id>", "42"),
    ("<ref>", "42"),
    ("<shell>", "bash"),
    ("<topic>", "filters"),
    ("<command>", "add"),
];

/// `text` with every [`PLACEHOLDERS`] entry substituted. Done before
/// splitting, since an unquoted `<id>` is a redirection to a shell; a
/// `<word…` left over is a placeholder nobody decided on, and fails.
fn filled(page: &str, line: usize, text: &str) -> String {
    let mut out = text.to_string();
    for (p, v) in PLACEHOLDERS {
        out = out.replace(p, v);
    }
    for (at, _) in out.match_indices('<') {
        assert!(
            !out[at + 1..].starts_with(|c: char| c.is_ascii_alphabetic()),
            "{page}:{line}: placeholder {:?} is not in PLACEHOLDERS",
            &out[at..]
        );
    }
    out
}

/// Every `tasqx …` command a reader can copy out of the docs, as
/// (`page:line`, the text with placeholders substituted, argv, inline?).
///
/// Fenced lines count in `console`, `sh`, `bash` and `shell` fences; an
/// optional `$ ` prompt is dropped, a line is a command when its first
/// word after any `NAME=value` is `tasqx`, and an open quote or a trailing `\` pulls in the
/// next line. Inline spans count when they open with `tasqx `. A trailing
/// `...`/`…` word ("and so on") is dropped.
fn doc_commands() -> Vec<(String, String, Vec<String>, bool)> {
    let mut out = Vec::new();
    for (page, text) in wiki_and_guides() {
        let code = code_in(&text);
        let mut i = 0;
        while i < code.len() {
            let c = &code[i];
            i += 1;
            let shell = match &c.fence {
                Some(info) => matches!(info.as_str(), "console" | "sh" | "bash" | "shell"),
                None => false,
            };
            let line = c.text.trim().trim_start_matches("$ ");
            let starts_tasqx = line.split_whitespace().find(|w| !is_env(w)) == Some("tasqx");
            let mut src = match (&c.fence, shell) {
                // Output lines share the fence, and an apostrophe in one would
                // read as an open quote: only a line that starts a command is
                // split at all.
                (Some(_), true) if starts_tasqx => filled(&page, c.line, line),
                (None, _) if c.text.starts_with("tasqx ") => filled(&page, c.line, &c.text),
                _ => continue,
            };
            let tokens = loop {
                if let Some(t) = shell_split(&src) {
                    break t;
                }
                // Only a fenced line continues, and only onto the same fence.
                let next = code.get(i).filter(|n| shell && n.fence == c.fence);
                match next {
                    Some(n) => {
                        src.push('\n');
                        src.push_str(&filled(&page, n.line, &n.text));
                        i += 1;
                    }
                    None => panic!("{page}:{}: unterminated shell line {src:?}", c.line),
                }
            };
            for mut argv in tasqx_invocations(&tokens) {
                if argv.last().is_some_and(|w| w == "..." || w == "…") {
                    argv.pop();
                }
                out.push((
                    format!("{page}:{}", c.line),
                    src.clone(),
                    argv,
                    c.fence.is_none(),
                ));
            }
        }
    }
    out
}

/// Every `tasqx …` command in `docs/wiki` and `docs/guides` parses with the
/// binary's own clap tree, and `add`/`modify` words pass the sugar parser.
///
/// A renamed verb or flag used to break the docs silently: nothing in the
/// build read them as commands. This parses each one through
/// [`tasqx_cli::parse_argv`] — the same prepass and command tree `tasqx` runs —
/// and executes nothing.
///
/// A fenced line is a command to run, so it must parse whole. An inline span
/// is often a verb's name in prose (`tasqx modify`, `tasqx check set`), so a
/// span may stop short of its required arguments; any other refusal — an
/// unknown verb, flag or value — fails it just the same.
#[test]
fn every_command_the_docs_show_parses() {
    use clap::error::ErrorKind;
    let commands = doc_commands();
    let mut failures = Vec::new();
    for (at, src, argv, inline) in &commands {
        match tasqx_cli::parse_argv(argv) {
            Ok(()) => {}
            Err(e)
                if *inline
                    && matches!(
                        e.kind(),
                        ErrorKind::MissingRequiredArgument
                            | ErrorKind::MissingSubcommand
                            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    ) => {}
            Err(e) => failures.push(format!("{at}: {src:?}\n{}", e.render())),
        }
    }
    assert!(
        failures.is_empty(),
        "{} documented command(s) do not parse:\n\n{}",
        failures.len(),
        failures.join("\n")
    );
    // Floor: the docs showed 356 when this guard was written. Far fewer means
    // the extraction stopped seeing commands, not that the docs got cleaner.
    assert!(
        commands.len() >= 350,
        "only {} `tasqx …` commands found in docs/wiki and docs/guides",
        commands.len()
    );
}

/// The splitter's rules, each one a doc shape the guard depends on.
#[test]
fn shell_split_reads_the_shapes_the_docs_use() {
    let argv = |s: &str| tasqx_invocations(&shell_split(s).expect("closed"));
    assert_eq!(
        argv(r#"tasqx list 'project:"Home Renovation"' # a comment"#),
        [vec!["tasqx", "list", r#"project:"Home Renovation""#]]
    );
    assert_eq!(
        argv("tasqx export | tasqx import -"),
        [vec!["tasqx", "export"], vec!["tasqx", "import", "-"]]
    );
    assert_eq!(
        argv("TASQX_DB=/tmp/x.db tasqx export > out.json"),
        [vec!["tasqx", "export"]]
    );
    assert_eq!(argv("cargo build && tasqx about"), [vec!["tasqx", "about"]]);
    assert_eq!(shell_split("tasqx annotate 2 'open"), None);
    assert_eq!(shell_split("tasqx add one \\"), None);
}
