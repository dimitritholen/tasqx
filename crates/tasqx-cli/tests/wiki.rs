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
