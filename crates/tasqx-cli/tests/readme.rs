//! Drift guards for the workspace README, and for the claims that left it.
//!
//! The README is the one surface with no generator behind it: every figure in
//! it is a restated copy of something the code owns, which is exactly the
//! state the in-crate doc guards exist to forbid. These tests read the file a
//! visitor reads and bind each restated claim to its source — the measured
//! `rust-version`, the theme table, the files the links name.
//! Parsing is deliberately minimal (anchored phrases and backtick spans, no
//! markdown model), and every scan pins a floor so an empty iteration cannot
//! pass as a clean one.
//!
//! D162 made the README a pitch and moved the reference detail into the wiki.
//! A guard follows its claim to its new home rather than leaving with it: the
//! MCP roster is read out of `AI-Agents-and-Automation.md`, the activation
//! lines out of `Shell-Completion.md`, the bare-`tasqx` condition out of
//! `Dashboard-and-Live-View.md`. The exit-code and retired-panel guards had a
//! wiki twin already, and that twin now scans the README too (`wiki.rs`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The workspace root, two levels above this crate's manifest.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn readme() -> String {
    fs::read_to_string(root().join("README.md")).expect("../../README.md is readable")
}

/// A wiki page by file name, for the claims D162 moved out of the README.
fn wiki(page: &str) -> String {
    fs::read_to_string(root().join("docs/wiki").join(page))
        .unwrap_or_else(|e| panic!("docs/wiki/{page} is readable: {e}"))
}

/// The README names the platforms CI tests on, and that sentence must agree with
/// the matrix in `.github/workflows/ci.yml`.
///
/// # Why this is worth a guard
///
/// It has now drifted twice, in opposite directions. It said "Linux and Windows"
/// while the Windows job had been red for a week, and then it went on saying
/// "macOS binaries are built and released but not yet covered by the test matrix"
/// in the very commit that added `macos-latest` to the matrix. Both readings were
/// wrong at the moment somebody would have relied on them, and no gate could see
/// either: the workflow is YAML that no Rust test parses, and the README is prose.
///
/// The matrix is the registry and the README restates it, which is the D30 shape
/// this repository keeps paying for. So the list is READ out of the workflow — a
/// deliberately small parse, since pulling in a YAML crate for one line would be
/// a dependency the supply-chain job then has to carry — and every platform in it
/// must be named in the sentence.
///
/// # The check is scoped to the CI SENTENCES, and that is load-bearing
///
/// The first version of this guard asked whether the README contained the word
/// "macos" anywhere. It passed while the CI sentence still said "Linux and
/// Windows", because the install section mentions macOS binaries three
/// paragraphs earlier — a guard that cannot fail, written in the commit whose
/// whole purpose was to stop unguarded prose. So membership is decided by the
/// lines that ANNOUNCE the matrix, found by the phrase they open with.
///
/// Deliberately one-directional: it fails when the workflow gains a platform
/// those lines do not name, which is the direction that misleads a reader. It
/// cannot catch a line naming a platform the workflow dropped; that would need
/// the sentence itself to be machine-readable, and rewriting English to suit a
/// parser is a worse trade than the half-guard.
#[test]
fn readme_names_every_platform_the_ci_matrix_tests() {
    let ci = fs::read_to_string(root().join(".github/workflows/ci.yml"))
        .expect("the CI workflow is readable");
    let line = ci
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("os: ["))
        .expect("ci.yml declares an `os: [...]` matrix; if that moved, fix this guard");
    let platforms: Vec<&str> = line
        .trim_start_matches("os: [")
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    assert!(
        platforms.len() >= 2,
        "parsed {platforms:?} out of {line:?} — the matrix format changed and \
         this guard is checking almost nothing"
    );

    // The sentences that announce the matrix, not the whole document. Both
    // README paragraphs about CI open with this phrase; anything else that
    // happens to mention a platform (the install section lists the release
    // targets) is none of this guard's business and must not satisfy it.
    const ANNOUNCES_THE_MATRIX: &str = "ci runs the suite on";
    let readme = readme().to_lowercase();
    let claims: Vec<&str> = readme
        .lines()
        .filter(|l| l.contains(ANNOUNCES_THE_MATRIX))
        .collect();
    assert!(
        !claims.is_empty(),
        "no README line contains {ANNOUNCES_THE_MATRIX:?}, so this guard is \
         checking nothing. Either the sentence was reworded — update the phrase \
         here — or the README stopped saying which platforms CI covers."
    );

    for platform in &platforms {
        // `ubuntu-latest` is called Linux in prose, and rightly: the README is
        // for humans choosing a machine, not for someone reading a runner label.
        let prose = match *platform {
            p if p.starts_with("ubuntu") => "linux",
            p if p.starts_with("windows") => "windows",
            p if p.starts_with("macos") => "macos",
            other => panic!(
                "unknown runner {other:?} in the CI matrix; teach this guard what \
                 to call it in prose rather than dropping it from the check"
            ),
        };
        let named = claims.iter().filter(|c| c.contains(prose)).count();
        assert_eq!(
            named,
            claims.len(),
            "the CI matrix tests on {platform}, and only {named} of the \
             {} README sentence(s) announcing the matrix say {prose:?}. A reader \
             deciding whether their platform is covered is given the wrong \
             answer, and nothing else in the build can tell.",
            claims.len()
        );
    }
}

/// The README writes counts as words ("Fifteen tools"), so the guards that
/// count for themselves need the same spelling. Panics past the table's end
/// rather than guessing: extending it is a one-line edit at the moment a
/// roster actually grows that far.
fn word(n: usize) -> &'static str {
    const WORDS: [&str; 33] = [
        "Zero",
        "One",
        "Two",
        "Three",
        "Four",
        "Five",
        "Six",
        "Seven",
        "Eight",
        "Nine",
        "Ten",
        "Eleven",
        "Twelve",
        "Thirteen",
        "Fourteen",
        "Fifteen",
        "Sixteen",
        "Seventeen",
        "Eighteen",
        "Nineteen",
        "Twenty",
        "Twenty-one",
        "Twenty-two",
        "Twenty-three",
        "Twenty-four",
        "Twenty-five",
        "Twenty-six",
        "Twenty-seven",
        "Twenty-eight",
        "Twenty-nine",
        "Thirty",
        "Thirty-one",
        "Thirty-two",
    ];
    WORDS
        .get(n)
        .copied()
        .unwrap_or_else(|| panic!("count {n} is past the number-word table; extend it"))
}

/// The backtick-quoted spans of `s`, in order.
fn ticked(s: &str) -> Vec<String> {
    s.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The README's "Rust N.M or newer" is a hand-copy of the workspace
/// `rust-version` — a MEASURED floor (see the comment on the field) that moves
/// with the lockfile. A stale figure sends a builder to a toolchain the build
/// then fails on with an error that names neither the README nor the floor.
#[test]
fn readme_rust_floor_equals_the_workspace_rust_version() {
    let readme = readme();
    let manifest = fs::read_to_string(root().join("Cargo.toml")).expect("workspace Cargo.toml");
    let real = manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("rust-version = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("the workspace Cargo.toml declares rust-version");

    // The claim must be present with the real figure…
    let needle = format!("Rust {real} or newer");
    assert!(
        readme.contains(&needle),
        "the README no longer says {needle:?} — did the measured floor move?"
    );
    // …and no other versioned "Rust N.M" claim may contradict it. Scanned, not
    // assumed singular: a second paragraph restating the floor is exactly how
    // the first copy went stale.
    let mut checked = 0;
    for (i, _) in readme.match_indices("Rust ") {
        let tail = &readme[i + "Rust ".len()..];
        if !tail.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let claimed: String = tail
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        assert_eq!(
            claimed, real,
            "the README claims Rust {claimed} somewhere, but the measured floor is {real}"
        );
        checked += 1;
    }
    assert!(checked >= 1, "the version scan found no 'Rust N.M' claim");
}

/// The wiki's AI Agents page restates the whole MCP roster: the total, the
/// read/write split, and every tool by name. All of it was free prose with
/// nothing behind it — the same state the HTML guide's tool table was in, on
/// the page the README's "Connect your agent" sends an agent operator to. The
/// roster lived in the README itself until D162 moved it there.
#[test]
fn the_agents_page_mcp_tool_roster_matches_the_server() {
    const PAGE: &str = "AI-Agents-and-Automation.md";
    let page = wiki(PAGE);
    let roster = tasqx_core::mcp::tool_roster();
    // Floor: fifteen tools shipped; a shrunken roster greening the loops below
    // would be a change worth failing on anyway.
    assert!(
        roster.len() >= 15,
        "the MCP roster shrank below the shipped tool set: {}",
        roster.len()
    );
    let mut reads: Vec<String> = roster
        .iter()
        .filter(|(_, write)| !write)
        .map(|(n, _)| n.to_string())
        .collect();
    let mut writes: Vec<String> = roster
        .iter()
        .filter(|(_, write)| *write)
        .map(|(n, _)| n.to_string())
        .collect();

    // The counts are counted, not trusted: "Fifteen tools", "Five reads:",
    // "Ten writes:" must all be the words the roster adds up to.
    let total_claim = format!("{} tools", word(roster.len()));
    let reads_claim = format!("{} reads:", word(reads.len()));
    let writes_claim = format!("{} writes:", word(writes.len()));
    for claim in [&total_claim, &reads_claim, &writes_claim] {
        assert!(
            page.contains(claim.as_str()),
            "{PAGE} no longer says {claim:?} — the roster moved and the prose did not"
        );
    }

    // The listed names, both directions. The page spells them unprefixed and
    // says "(all prefixed `tasqx_`)" once, so the prefix is restored before
    // comparing. The reads list runs from its claim to the writes claim; the
    // writes list runs to the prefix note that closes it.
    let r0 = page.find(&reads_claim).expect("a reads list") + reads_claim.len();
    let r1 = page[r0..]
        .find(&writes_claim)
        .expect("a writes list after the reads")
        + r0;
    let w1 = page[r1..]
        .find("(all prefixed")
        .expect("the prefix note that closes the writes list")
        + r1;
    let mut listed_reads: Vec<String> = ticked(&page[r0..r1])
        .into_iter()
        .map(|t| format!("tasqx_{t}"))
        .collect();
    let mut listed_writes: Vec<String> = ticked(&page[r1..w1])
        .into_iter()
        .map(|t| format!("tasqx_{t}"))
        .collect();
    listed_reads.sort();
    listed_writes.sort();
    reads.sort();
    writes.sort();
    assert_eq!(
        listed_reads, reads,
        "{PAGE}'s read-tool list has drifted from the roster"
    );
    assert_eq!(
        listed_writes, writes,
        "{PAGE}'s write-tool list has drifted from the roster"
    );

    // Anywhere else the page, or the README that sends readers to it, names a
    // tool by its full `tasqx_*` spelling, that tool must exist — a renamed
    // tool leaves its old name behind in running prose, which the list
    // comparison above cannot see.
    for (name, text) in [(PAGE, page), ("README", readme())] {
        for span in ticked(&text) {
            let Some(rest) = span.strip_prefix("tasqx_") else {
                continue;
            };
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                continue; // `tasqx_` itself (the prefix note), or not a tool name
            }
            assert!(
                roster.iter().any(|(n, _)| *n == span),
                "{name} names `{span}`, which the MCP server does not serve"
            );
        }
    }
}

/// The wiki's Shell Completion page prints an activation line per shell, by
/// hand, and that is the worst place in it for a hand-kept copy. (The README
/// carried the same lines until D162; the page is where they live now.)
///
/// The failure it invites is total and silent. A line carrying `clap_complete`'s
/// generic `COMPLETE` instead of `TASQX_COMPLETE` looks right, is what every
/// clap tutorial shows, and activates nothing at all — `complete::intercept`
/// reads `TASQX_COMPLETE` and returns immediately for anything else. The reader
/// pastes it into their startup file, opens a new shell, presses Tab, and gets
/// the shell's own filename completion, with no error printed anywhere and
/// nothing to search for. The same is true of a line that goes stale by one
/// character.
///
/// So the lines are not compared against a second copy kept in this test. They
/// are compared against what the BINARY prints, for every shell
/// `Shells::builtins()` names — the same registry `tasqx completions` resolves
/// its argument out of, and the same one `install::ACTIVATIONS` is guarded
/// against. Upstream gaining a sixth shell, the activation shape changing, or a
/// wiki line edited by hand all fail here.
/// # Both documents, and both halves of each row
///
/// The first version of this guard checked the README only, and only the LINE.
/// Review measured both gaps and both were reachable with the whole suite green:
///
///  * `manual.rs`'s `Topic::Completion` keeps a SECOND hand-written copy of all
///    five lines and targets, and it is the copy `tasqx manual completion` and
///    `tasqx docs` render — the primary in-tool surface for this feature, since
///    `COMMAND_REF`'s `completions` entry points at that topic. A mutation
///    putting clap's generic `COMPLETE` into the manual's bash line left every
///    test passing.
///  * The TARGET file was unguarded in both. `~/.bashrc` drifting to
///    `~/.bash_profile` is the classic silent failure here: a non-login
///    interactive bash reads `.bashrc` and never the other, so the reader pastes
///    a correct line into a file their shell does not source and gets no error
///    anywhere. A mutation making exactly that edit also passed.
///
/// So one loop covers both documents and asserts both halves, out of the binary
/// rather than out of a list kept here. The target comes from `--json`, which is
/// `install::target_path` — the same resolution `--install` writes to — and is
/// `null` for PowerShell, which deliberately has no knowable target.
#[test]
fn both_documents_carry_the_activation_lines_and_targets_the_binary_prints() {
    let manual = std::process::Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .args(["manual", "completion"])
        .output()
        .expect("render the manual's completion topic");
    assert!(manual.status.success(), "`tasqx manual completion` failed");
    let manual = String::from_utf8_lossy(&manual.stdout).into_owned();

    let mut checked = 0;
    for shell in clap_complete::env::Shells::builtins().names() {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .args(["--json", "completions", shell])
            .output()
            .unwrap_or_else(|e| panic!("run `tasqx completions {shell}`: {e}"));
        assert!(
            out.status.success(),
            "`tasqx completions {shell}` failed: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let printed: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("completions prints one JSON object");
        let line = printed["line"].as_str().unwrap_or_default();
        assert!(
            !line.is_empty(),
            "`tasqx completions {shell}` reported no line to compare against"
        );

        let page = wiki("Shell-Completion.md");
        for (name, text) in [("Shell-Completion.md", &page), ("the manual", &manual)] {
            assert!(
                text.contains(line),
                "{name} does not carry the {shell} activation line the binary \
                 prints, so a reader who pastes what it says gets a shell that \
                 completes nothing and says nothing. Expected to find:\n  {line}"
            );
            // `null` for PowerShell, which refuses to guess `$PROFILE`; both
            // documents say `$PROFILE` in prose instead and there is nothing to
            // compare that against.
            if let Some(target) = printed["target"].as_str() {
                // Compared on the `~`-relative tail rather than the absolute
                // path: the documents write `~/.bashrc`, which is what a reader
                // needs, while the binary reports this machine's home.
                let tail = target.replace('\\', "/");
                let tail = tail.rsplit_once("/.").map(|(_, t)| format!(".{t}"));
                let Some(tail) = tail else { continue };
                assert!(
                    text.contains(&tail),
                    "{name} does not name the file the {shell} line belongs in. \
                     The binary installs to {target}, whose tail is {tail:?}. A \
                     document naming a different file sends the reader to one \
                     their shell never reads — no error, no completion, nothing \
                     to search for."
                );
            }
        }
        checked += 1;
    }
    // Floor: the five shells this feature ships for. An empty registry would
    // otherwise make the loop above a clean pass over nothing.
    assert!(
        checked >= 5,
        "only {checked} shells were checked; `Shells::builtins()` shrank and \
         this guard is covering less than the documents claim"
    );
}

/// "Five built-in themes" is a count of `theme::BUILTINS`, restated by hand.
/// The verb table taught this lesson already: a spelled-out number reads
/// exactly like a right one after the list underneath it grows.
#[test]
fn readme_theme_count_matches_the_builtins() {
    let builtins = tasqx_cli::THEME_BUILTINS.len();
    assert!(builtins >= 5, "the built-in theme set shrank: {builtins}");
    let claim = format!("{} built-in themes", word(builtins));
    assert!(
        readme().contains(&claim),
        "the README no longer says {claim:?} — did theme::BUILTINS change?"
    );
}

/// Every relative link in the README must name a file that exists. The guides
/// list is the load-bearing case: a guide renamed or moved leaves the README
/// 404-ing on the repo's own landing page, and nothing else reads those paths.
/// This is also what pins `docs/guides/token-accounting.md`.
///
/// Since D162 the README sends readers into the wiki for everything it no
/// longer says (`Getting-Started.md#install-fine-print`, `Home.md#honest-edges`),
/// so every relative target is checked, not just the ones under `docs/`, and
/// an anchor must name a heading on the page it points at: the README's own
/// `#install` row and the wiki sections are exactly what a heading rename
/// would silently break.
#[test]
fn readme_relative_links_point_at_files_that_exist() {
    let readme = readme();
    let root = root();
    let mut guides = 0;
    let mut checked = 0;
    let mut anchors = 0;
    let mut rest = readme.as_str();
    while let Some(i) = rest.find("](") {
        let tail = &rest[i + 2..];
        let Some(end) = tail.find(')') else { break };
        let target = &tail[..end];
        rest = &tail[end..];
        // Relative repo paths and anchors only; http(s) is not this test's claim.
        if target.starts_with("http") || target.is_empty() {
            continue;
        }
        let (path, anchor) = match target.split_once('#') {
            Some((p, a)) => (p, Some(a)),
            None => (target, None),
        };
        let page = if path.is_empty() {
            readme.clone()
        } else {
            assert!(
                root.join(path).exists(),
                "the README links {target:?}, and {path:?} does not exist"
            );
            checked += 1;
            fs::read_to_string(root.join(path)).unwrap_or_default()
        };
        if let Some(anchor) = anchor {
            assert!(
                heading_slugs(&page).contains(anchor),
                "the README links {target:?}, and no heading on that page has the anchor {anchor:?}"
            );
            anchors += 1;
        }
        if path.is_empty() {
            continue;
        }
        if path.starts_with("docs/guides/") {
            guides += 1;
        }
    }
    // Floors: the seven worked guides plus the wiki, the license and the three
    // project documents. A scan that finds fewer has lost links, not gained
    // tidiness — move these on purpose or not at all, in either direction: a
    // floor left behind when a guide is added is a guide that can silently
    // vanish again.
    assert!(
        guides >= 7,
        "the README links only {guides} guides under docs/guides/"
    );
    assert!(checked >= 7, "the link scan checked only {checked} paths");
    assert!(anchors >= 4, "the link scan checked only {anchors} anchors");
}

/// GitHub's heading anchors: lowercased, punctuation other than `-` and `_`
/// dropped, spaces turned into hyphens. Lines inside code fences are skipped.
fn heading_slugs(page: &str) -> BTreeSet<String> {
    let mut in_fence = false;
    let mut slugs = BTreeSet::new();
    for line in page.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        let Some(text) = line.strip_prefix('#') else {
            continue;
        };
        if in_fence {
            continue;
        }
        let slug: String = text
            .trim_start_matches('#')
            .trim()
            .to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
                _ => None,
            })
            .collect();
        slugs.insert(slug);
    }
    slugs
}

/// Neither the README nor the dashboard page may tell a reader that a bare
/// `tasqx` prints the table.
///
/// D58 gave that invocation a second meaning, and the README is the surface a
/// visitor reads first. It is pinned here because NOTHING else looks at prose:
/// mutating any sentence in README.md or under `docs/` leaves the whole suite
/// green — measured, by rewriting four of them in a scratch clone. That is the
/// class of drift this repo otherwise has no answer to, and rather than pretend
/// the general case is covered, this guard pins the one claim that just became
/// wrong and the one word that makes it conditional.
///
/// The condition used to be stated in the README's own dashboard section. D162
/// moved that detail to the wiki's dashboard page, so the condition is read
/// there, and the stale spellings are refused in both files.
#[test]
fn the_dashboard_page_does_not_promise_a_table_from_a_bare_tasqx() {
    const PAGE: &str = "Dashboard-and-Live-View.md";
    const HEADING: &str = "## tasqx dashboard";
    let page = wiki(PAGE);

    // The dashboard must be documented at all.
    assert!(
        page.contains(HEADING),
        "{PAGE} must tell a reader what a bare `tasqx` now opens"
    );

    // And the condition must be stated as the streams, not as a guess about
    // intent: nothing reads a `CI` variable, so a caller with a pty is on the
    // interactive side however unattended it is.
    let dashboard = page
        .split(HEADING)
        .nth(1)
        .expect("checked above")
        .split("\n## ")
        .next()
        .expect("a section has a body");
    for needed in ["stdin", "stdout", "pty", "tasqx list"] {
        assert!(
            dashboard.contains(needed),
            "{PAGE}'s dashboard section must mention {needed:?} — a reader who \
             skips it and shells out from an agent gets a hang, not a table"
        );
    }

    // The old sentence, in any of its spellings, is now false.
    for (name, text) in [("README", readme()), (PAGE, page)] {
        for stale in [
            "bare `tasqx` lists your working set",
            "Bare `tasqx` is the working set",
            "bare `tasqx` shows your working set",
        ] {
            assert!(
                !text.contains(stale),
                "{name} still says {stale:?}, which is only true off a terminal"
            );
        }
    }
}

/// Every `tasqx_*` tool the agent starter prompt tells an agent to call must
/// exist, and the two halves it splits on must have the scopes it claims.
///
/// That guide is a block of text a reader pastes into a client's instructions
/// file, where it becomes the only thing telling an agent to use memory at all.
/// Nothing downstream validates it: a renamed tool leaves the paste naming a
/// method the server answers `unknown tool` to, and the reader finds out when an
/// agent stops storing anything — which looks exactly like an agent that chose
/// not to. Prose is the one surface in this repo with no generator behind it,
/// so it gets a gate instead.
///
/// The scope halves are pinned as well as the names, because the guide's whole
/// structure rests on them: it promises the searching half works under a
/// read-only server and that the storing half is what `--scope write` buys. If
/// `tasqx_search_memory` ever became a write tool, the advice to run read-only
/// first would quietly stop working while every name in the file still resolved.
#[test]
fn the_agent_starter_prompt_names_tools_that_exist() {
    let guide = fs::read_to_string(root().join("docs/guides/agent-starter-prompt.md"))
        .expect("docs/guides/agent-starter-prompt.md is readable");

    let roster = tasqx_core::mcp::tool_roster();
    let scope_of = |name: &str| roster.iter().find(|(n, _)| *n == name).map(|(_, w)| *w);

    // Every `tasqx_…` run in the prose, however it is punctuated around.
    let mut named: Vec<String> = Vec::new();
    let mut rest = guide.as_str();
    while let Some(i) = rest.find("tasqx_") {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(tail.len());
        let run = &tail[..end];
        // The guide writes `tasqx_*` for "the tool family", which stops the run
        // at the underscore and is not a tool name. Anything with nothing after
        // the prefix is that, not a rename to chase.
        if run.len() > "tasqx_".len() {
            named.push(run.to_string());
        }
        rest = &tail[end..];
    }
    named.sort();
    named.dedup();
    assert!(
        named.len() >= 5,
        "the scan found only {named:?} — an empty scan must not pass as a clean one"
    );

    for name in &named {
        assert!(
            scope_of(name).is_some(),
            "the starter prompt tells an agent to call {name:?}, which is not in \
             the MCP roster: {:?}",
            roster.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        );
    }

    assert_eq!(
        scope_of("tasqx_search_memory"),
        Some(false),
        "the guide promises the searching half works under a read-only server"
    );
    for w in ["tasqx_add_memory", "tasqx_annotate_task"] {
        assert_eq!(
            scope_of(w),
            Some(true),
            "the guide promises {w} is what `--scope write` buys"
        );
    }
}

/// Every relative link in a guide under `docs/guides/` must resolve, and every
/// `tasqx_*` tool it tells an agent to call must exist.
///
/// The README link guard above covers only the README's own links, and the
/// starter-prompt guard right above this one covers only that one guide's
/// tool names. Between the two, a guide under `docs/guides/` can link a
/// renamed neighbour, or tell an agent to call a renamed tool, and nothing in
/// the build notices — the reader hits the 404, or the MCP server's `unknown
/// tool`, before either of those guards would.
///
/// Parsing is deliberately minimal, matching the two guards it generalises:
/// `](target)` spans for links and `tasqx_` runs for tool names, no markdown
/// model. Every scan pins a floor so an empty pass cannot read as a clean one.
#[test]
fn every_guide_links_files_that_exist_and_names_tools_that_exist() {
    let dir = root().join("docs/guides");
    let mut guides: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("docs/guides is readable")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    guides.sort();
    assert!(
        guides.len() >= 6,
        "found only {} guides under docs/guides/ — this guard is checking \
         almost nothing",
        guides.len()
    );

    let roster = tasqx_core::mcp::tool_roster();
    let mut checked_links = 0;
    let mut named: Vec<(String, String)> = Vec::new();

    for guide in &guides {
        let text = fs::read_to_string(guide)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", guide.display()));
        let name = guide.file_name().unwrap().to_string_lossy();

        // Every `](target)` span. http(s) links, bare anchors and empty
        // targets are not this guard's business.
        let mut rest = text.as_str();
        while let Some(i) = rest.find("](") {
            let tail = &rest[i + 2..];
            let Some(end) = tail.find(')') else { break };
            let target = &tail[..end];
            rest = &tail[end..];
            if target.starts_with("http") || target.starts_with('#') || target.is_empty() {
                continue;
            }
            let target = target.split('#').next().unwrap_or(target);
            assert!(
                dir.join(target).exists(),
                "{name} links {target:?}, which does not exist — a reader \
                 following that link from the guide's page on the repo gets a \
                 404"
            );
            checked_links += 1;
        }

        // Every `tasqx_…` run, the same loop as the starter-prompt guard
        // above.
        let mut rest = text.as_str();
        while let Some(i) = rest.find("tasqx_") {
            let tail = &rest[i..];
            let end = tail
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(tail.len());
            let run = &tail[..end];
            rest = &tail[end..];
            // `tasqx_*` alone is the tool family, not a name to chase.
            if run == "tasqx_" {
                continue;
            }
            // `mcp__tasqx__tasqx_complete_task`, the hook matcher spelling in
            // self-improving-agent.md, names the real tool after the double
            // underscore, so the tool name is what follows it.
            let tool = run.strip_prefix("tasqx__").unwrap_or(run);
            named.push((name.to_string(), tool.to_string()));
        }
    }

    assert!(
        checked_links >= 5,
        "the link scan across docs/guides/ checked only {checked_links} paths \
         — an empty scan must not pass as a clean one"
    );

    named.sort();
    named.dedup();
    let distinct: BTreeSet<&str> = named.iter().map(|(_, tool)| tool.as_str()).collect();
    assert!(
        distinct.len() >= 10,
        "the tool-name scan across docs/guides/ found only {distinct:?} — an \
         empty scan must not pass as a clean one"
    );

    for (guide, tool) in &named {
        assert!(
            roster.iter().any(|(n, _)| *n == tool),
            "{guide} tells an agent to call {tool:?}, which is not in the MCP \
             roster: {:?}",
            roster.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        );
    }
}

/// The version this tree builds has a `CHANGELOG.md` section, spelled the way
/// the release workflow looks for it.
///
/// `release.yml` lifts the `## X.Y.Z` section into the release notes, and a tag
/// whose section is missing still publishes — with only the generated commit
/// list, which is the release page nobody reads. The version bump is the moment
/// the section has to exist, and nothing else notices its absence until the
/// release is already public. So the bump is what goes red.
#[test]
fn the_changelog_has_a_section_for_the_workspace_version() {
    let changelog =
        fs::read_to_string(root().join("CHANGELOG.md")).expect("CHANGELOG.md is readable");
    let heading = format!("## {}", env!("CARGO_PKG_VERSION"));
    let body: Vec<&str> = changelog
        .lines()
        .skip_while(|l| *l != heading)
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(
        changelog.lines().any(|l| l == heading),
        "CHANGELOG.md has no `{heading}` line, so the release workflow would publish \
         this version with generated notes only"
    );
    assert!(
        !body.is_empty(),
        "CHANGELOG.md's `{heading}` section is empty"
    );
}

/// Every archive the Homebrew formula points at is one the release workflow
/// actually builds.
///
/// `scripts/brew-formula.sh` names three targets and `release.yml` builds four;
/// nothing connected the two lists. A target renamed or dropped in the matrix
/// leaves the formula rendering URLs to files the release never published, and
/// the formula is generated per tag precisely so that it is never checked in
/// and never reviewed — so the first reader of the mistake is somebody running
/// `brew install`, getting a 404, and having no reason to suspect the tap.
///
/// One-directional on purpose, like the CI-platform guard above: it fails when
/// the formula names something the matrix does not build, which is the
/// direction that ships a broken install. The matrix building a target the
/// formula ignores is deliberate — Homebrew has nowhere to put the Windows zip.
#[test]
fn the_brew_formula_names_targets_the_release_workflow_builds() {
    let script = fs::read_to_string(root().join("scripts/brew-formula.sh"))
        .expect("scripts/brew-formula.sh is readable");
    let workflow = fs::read_to_string(root().join(".github/workflows/release.yml"))
        .expect("the release workflow is readable");

    let built: Vec<&str> = workflow
        .lines()
        .filter_map(|l| l.trim().strip_prefix("target: "))
        .map(str::trim)
        .collect();
    assert!(
        built.len() >= 3,
        "parsed {built:?} out of release.yml — the matrix format changed and this \
         guard is checking almost nothing"
    );

    // The archive names the script builds its URLs from, read out of the
    // `tasqx-${TAG}-<target>.tar.gz` assignments rather than re-listed here.
    let named: Vec<&str> = script
        .lines()
        .filter_map(|l| l.split_once("=\"tasqx-${TAG}-"))
        .filter_map(|(_, rest)| rest.split(".tar.gz").next())
        .collect();
    assert_eq!(
        named.len(),
        3,
        "found {named:?} archive names in brew-formula.sh; expected the three \
         Homebrew can serve"
    );

    for target in &named {
        assert!(
            built.contains(target),
            "the formula points at a {target} archive, which release.yml does not \
             build — it builds {built:?}. `brew install` would 404."
        );
    }

    // And the stem the script assumes is the stem the workflow writes.
    assert!(
        workflow.contains("STAGE=\"tasqx-${VERSION}-${{ matrix.target }}\""),
        "release.yml no longer names archives `tasqx-<version>-<target>`, which is \
         the shape brew-formula.sh builds its URLs from"
    );
}

/// The one target the Scoop manifest serves is one the release workflow
/// actually builds.
///
/// `scripts/scoop-manifest.sh` is a fourth declaration site for the platform
/// list, with the same failure shape as the brew formula above: the manifest is
/// generated per tag precisely so it is never checked in and never reviewed,
/// so a target renamed or dropped in the matrix leaves it rendering a URL —
/// and an `autoupdate` template — to files the release never published, and
/// the first reader of the mistake is somebody running `scoop install`,
/// getting a 404, and having no reason to suspect the bucket.
///
/// The script funnels every use of the triple (URL, `extract_dir`, the
/// autoupdate template) through a single `TARGET=` assignment, which is the
/// line read here — so one declaration site is guarded and covers all four
/// uses. One-directional on purpose, like both guards above: the matrix
/// building targets Scoop ignores is deliberate — Scoop has nowhere to put a
/// darwin or linux archive.
#[test]
fn the_scoop_manifest_names_a_target_the_release_workflow_builds() {
    let script = fs::read_to_string(root().join("scripts/scoop-manifest.sh"))
        .expect("scripts/scoop-manifest.sh is readable");
    let workflow = fs::read_to_string(root().join(".github/workflows/release.yml"))
        .expect("the release workflow is readable");

    let built: Vec<&str> = workflow
        .lines()
        .filter_map(|l| l.trim().strip_prefix("target: "))
        .map(str::trim)
        .collect();
    assert!(
        built.len() >= 3,
        "parsed {built:?} out of release.yml — the matrix format changed and this \
         guard is checking almost nothing"
    );

    let named: Vec<&str> = script
        .lines()
        .filter_map(|l| l.strip_prefix("TARGET=\""))
        .filter_map(|l| l.strip_suffix('"'))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "found {named:?} TARGET assignments in scoop-manifest.sh; expected exactly \
         the one Windows target Scoop can serve"
    );

    for target in &named {
        assert!(
            built.contains(target),
            "the manifest points at a {target} archive, which release.yml does not \
             build — it builds {built:?}. `scoop install` would 404."
        );
    }

    // And the stem the script assumes is the stem the workflow writes.
    assert!(
        workflow.contains("STAGE=\"tasqx-${VERSION}-${{ matrix.target }}\""),
        "release.yml no longer names archives `tasqx-<version>-<target>`, which is \
         the shape scoop-manifest.sh builds its URLs from"
    );
}

/// Every target triple an installer script can emit, read out of the script
/// rather than re-listed here.
///
/// A triple is recognised as a maximal run of `[a-z0-9_-]` that is a *whole*
/// quoted string — a quote character immediately on both sides — and that splits
/// into three or more non-empty `-` components. Both halves of that rule are
/// load-bearing:
///
/// - Quoted-content-only, because the scripts *talk about* triples they do not
///   map. `install.ps1` explains in a comment that "there is no
///   aarch64-pc-windows-msvc in the release matrix", and a scan that matched
///   triples anywhere in the file would read that sentence as a mapping and
///   redden a clean tree.
/// - Whole-string, because a bare run scan also matches the `0-9a-f` inside a
///   `[0-9a-fA-F]{64}` character class.
///
/// The cost of the rule is that a quoted string with three or more hyphenated
/// lowercase parts and no other characters would be read as a target. Nothing in
/// either script is shaped that way today, and the failure mode is a loud red
/// build naming the string, not a silent miss.
fn targets_a_script_can_emit(script: &str) -> BTreeSet<&str> {
    let bytes = script.as_bytes();
    let in_run = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-';
    let is_quote = |c: u8| c == b'\'' || c == b'"';

    let mut found = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        if !in_run(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && in_run(bytes[i]) {
            i += 1;
        }
        let quoted =
            start > 0 && is_quote(bytes[start - 1]) && i < bytes.len() && is_quote(bytes[i]);
        // Every byte of the run is ASCII, so these indices are char boundaries.
        let span = &script[start..i];
        if quoted && span.split('-').count() >= 3 && !span.split('-').any(str::is_empty) {
            found.insert(span);
        }
    }
    found
}

/// Every target an installer can hand to the download URL is one the release
/// workflow actually builds.
///
/// The installers are a third and fourth declaration site for the platform list,
/// after the release matrix and the Homebrew formula, and they are the two that
/// run on a stranger's machine with a pipe into `sh`. Adding a mapping here
/// costs one line and looks harmless; if the matrix has no matching job the
/// script resolves a real-looking archive name, requests it, and the user gets a
/// 404 from a URL that reads correctly. Nothing else in the tree would have
/// noticed — the scripts are shell and PowerShell that no Rust test parses.
///
/// One-directional on purpose, exactly like the brew guard above: it fails when
/// an installer names something the matrix does not build, which is the
/// direction that ships a broken install. A matrix target no installer maps yet
/// is the opposite case and is deliberate — the user gets the scripts' own "no
/// prebuilt binary for <platform>" message, which is a clean answer rather than
/// a broken one, and is not this test's business.
///
/// Read at runtime rather than through `include_str!`. The `include_str!` gates
/// live in `tasqx-core` and every file they embed is part of that crate's own
/// testing regime; pulling shell scripts in there would make the headless engine
/// fail to *compile* when an installer is renamed, and would turn a missing file
/// into a compile error instead of a readable message.
///
/// It also checks the other end of the same two files: the release job attaches
/// them to every tagged release, so a pinned URL exists beside the `main` one the
/// README fetches, and rename either end alone and this reddens. That half is a
/// read of the workflow's *text* — it never reaches the network, so it says
/// nothing about whether any published release really carries the assets. Only a
/// tag can answer that.
#[test]
fn the_installers_map_only_targets_the_release_workflow_builds() {
    let sh = fs::read_to_string(root().join("install.sh")).expect("install.sh is readable");
    let ps1 = fs::read_to_string(root().join("install.ps1")).expect("install.ps1 is readable");
    let workflow = fs::read_to_string(root().join(".github/workflows/release.yml"))
        .expect("the release workflow is readable");

    let built: Vec<&str> = workflow
        .lines()
        .filter_map(|l| l.trim().strip_prefix("target: "))
        .map(str::trim)
        .collect();
    assert!(
        built.len() >= 3,
        "parsed {built:?} out of release.yml — the matrix format changed and this \
         guard is checking almost nothing"
    );

    for (name, script) in [("install.sh", &sh), ("install.ps1", &ps1)] {
        let mapped = targets_a_script_can_emit(script);
        println!("{name} can emit {mapped:?}; release.yml builds {built:?}");
        assert!(
            !mapped.is_empty(),
            "parsed no target triples out of {name} — the way it names targets \
             changed and this guard is now checking nothing"
        );

        for target in &mapped {
            assert!(
                built.contains(target),
                "{name} maps {target}, which release.yml does not build — it builds \
                 {built:?}. The installer would resolve a download URL that 404s."
            );
        }
    }

    // And every script the README bootstraps from is one the release job
    // attaches, so the tagged URL and the moving `main` one name the same file.
    // The names come out of the README's raw `.../main/<file>` URLs rather than
    // being re-listed here, and are matched as whole arguments of the publish
    // command — renaming the file on either side alone reddens.
    let readme = readme();
    let bootstrapped: BTreeSet<&str> = readme
        .match_indices("/main/")
        .filter_map(|(at, sep)| {
            let rest = &readme[at + sep.len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-'))
                .unwrap_or(rest.len());
            (end > 0).then_some(&rest[..end])
        })
        .collect();
    assert_eq!(
        bootstrapped.len(),
        2,
        "found {bootstrapped:?} behind the README's raw `/main/` URLs; expected the \
         two bootstrap scripts, and a scan that finds anything else is reading \
         something other than the one-liners"
    );

    let publish = workflow
        .lines()
        .find(|l| l.trim_start().starts_with("gh release create"))
        .expect("release.yml still publishes with `gh release create`");
    let attached: BTreeSet<&str> = publish.split_whitespace().collect();
    for script in &bootstrapped {
        assert!(
            attached.contains(script),
            "the README bootstraps from {script}, which the release job does not \
             upload — it runs `{}`. Anyone pinning a tagged installer URL would get \
             a 404 while the `main` one kept working.",
            publish.trim()
        );
    }
}

/// The README's `why` sample must use the rows `why` prints.
///
/// Same drift as the wiki's: the pre-D122 spellings (`due_proximity`, a
/// `= total` footer, a "Why #42 has urgency" header) read as current on the
/// first screen a visitor sees.
#[test]
fn the_readme_why_sample_uses_the_rows_the_command_prints() {
    let text = readme();
    assert!(
        text.contains("$ tasqx why"),
        "the README no longer shows a `tasqx why` sample — this guard is \
         checking nothing"
    );
    for gone in ["Why #", "due_proximity", "= total"] {
        assert!(
            !text.contains(gone),
            "the README still shows {gone:?}, which `tasqx why` stopped printing at D122"
        );
    }
    // The SCORES in the sample rot by the hour — urgency is recomputed from
    // the deadline on every read, so a captured 13.5 reads 13.6 the same
    // afternoon. Pinning the figures would redden the build daily; the sample
    // instead says the numbers illustrate and the rows are the contract, and
    // that sentence is what this pins.
    assert!(
        text.contains("are an illustration; the rows are the contract"),
        "the README prints urgency figures from a capture without saying they \
         move with the clock — the numbers are wrong within hours"
    );
    for row in ["priority", "deadline", "age", "urgency"] {
        assert!(
            text.contains(row),
            "the README's `why` sample is missing the {row:?} row the command prints"
        );
    }
}
