//! Drift guards for the workspace README.
//!
//! The README is the one surface with no generator behind it: every figure in
//! it is a restated copy of something the code owns, which is exactly the
//! state the in-crate doc guards exist to forbid. These tests read the file a
//! visitor reads and bind each restated claim to its source — the measured
//! `rust-version`, the MCP roster, the theme table, the files the links name.
//! Parsing is deliberately minimal (anchored phrases and backtick spans, no
//! markdown model), and every scan pins a floor so an empty iteration cannot
//! pass as a clean one.

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
    const WORDS: [&str; 21] = [
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

/// The README's "For agents" section restates the whole MCP roster: the total,
/// the read/write split, and every tool by name. All of it was free prose with
/// nothing behind it — the same state the HTML guide's tool table was in, on
/// the page most likely to be an agent operator's first contact with tasqx.
#[test]
fn readme_mcp_tool_roster_matches_the_server() {
    let readme = readme();
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
            readme.contains(claim.as_str()),
            "the README no longer says {claim:?} — the roster moved and the prose did not"
        );
    }

    // The listed names, both directions. The README spells them unprefixed and
    // says "(all prefixed `tasqx_`)" once, so the prefix is restored before
    // comparing. The reads list runs from its claim to the writes claim; the
    // writes list runs to the prefix note that closes it.
    let r0 = readme.find(&reads_claim).expect("a reads list") + reads_claim.len();
    let r1 = readme[r0..]
        .find(&writes_claim)
        .expect("a writes list after the reads")
        + r0;
    let w1 = readme[r1..]
        .find("(all prefixed")
        .expect("the prefix note that closes the writes list")
        + r1;
    let mut listed_reads: Vec<String> = ticked(&readme[r0..r1])
        .into_iter()
        .map(|t| format!("tasqx_{t}"))
        .collect();
    let mut listed_writes: Vec<String> = ticked(&readme[r1..w1])
        .into_iter()
        .map(|t| format!("tasqx_{t}"))
        .collect();
    listed_reads.sort();
    listed_writes.sort();
    reads.sort();
    writes.sort();
    assert_eq!(
        listed_reads, reads,
        "the README's read-tool list has drifted from the roster"
    );
    assert_eq!(
        listed_writes, writes,
        "the README's write-tool list has drifted from the roster"
    );

    // Anywhere else the README names a tool by its full `tasqx_*` spelling,
    // that tool must exist — a renamed tool leaves its old name behind in
    // running prose, which the list comparison above cannot see.
    for span in ticked(&readme) {
        let Some(rest) = span.strip_prefix("tasqx_") else {
            continue;
        };
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            continue; // `tasqx_` itself (the prefix note), or not a tool name
        }
        assert!(
            roster.iter().any(|(n, _)| *n == span),
            "the README names `{span}`, which the MCP server does not serve"
        );
    }
}

/// The README's Tab-completion section prints an activation line per shell, by
/// hand, and that is the worst place in the file for a hand-kept copy.
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
/// README line edited by hand all fail here.
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

        for (name, text) in [("README", &readme()), ("the manual", &manual)] {
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
#[test]
fn readme_relative_links_point_at_files_that_exist() {
    let readme = readme();
    let root = root();
    let mut guides = 0;
    let mut checked = 0;
    let mut rest = readme.as_str();
    while let Some(i) = rest.find("](") {
        let tail = &rest[i + 2..];
        let Some(end) = tail.find(')') else { break };
        let target = &tail[..end];
        rest = &tail[end..];
        // Relative repo paths only; http(s) targets are not this test's claim.
        if target.starts_with("docs/") || target.starts_with(".claude/") || target == "LICENSE.md" {
            assert!(
                root.join(target).exists(),
                "the README links {target:?}, which does not exist"
            );
            checked += 1;
            if target.starts_with("docs/guides/") {
                guides += 1;
            }
        }
    }
    // Floors: six worked guides plus the skill and the license. A scan that
    // finds fewer has lost links, not gained tidiness — move these on purpose
    // or not at all, in either direction: a floor left behind when a guide is
    // added is a guide that can silently vanish again.
    assert!(
        guides >= 6,
        "the README links only {guides} guides under docs/guides/"
    );
    assert!(checked >= 7, "the link scan checked only {checked} paths");
}

/// The README must not tell a reader that a bare `tasqx` prints the table.
///
/// D58 gave that invocation a second meaning, and this file is the surface a
/// visitor reads first. It is pinned here because NOTHING else looks at prose:
/// mutating any sentence in README.md or under `docs/` leaves the whole suite
/// green — measured, by rewriting four of them in a scratch clone. That is the
/// class of drift this repo otherwise has no answer to, and rather than pretend
/// the general case is covered, this guard pins the one claim that just became
/// wrong and the one word that makes it conditional.
#[test]
fn the_readme_does_not_promise_a_table_from_a_bare_tasqx() {
    let text = readme();

    // The dashboard must be documented at all.
    assert!(
        text.contains("## The dashboard"),
        "the README must tell a reader what a bare `tasqx` now opens"
    );

    // And the condition must be stated as the streams, not as a guess about
    // intent: nothing reads a `CI` variable, so a caller with a pty is on the
    // interactive side however unattended it is.
    let dashboard = text
        .split("## The dashboard")
        .nth(1)
        .expect("checked above")
        .split("\n## ")
        .next()
        .expect("a section has a body");
    for needed in ["stdin", "stdout", "pty", "tasqx list"] {
        assert!(
            dashboard.contains(needed),
            "the dashboard section must mention {needed:?} — a reader who skips it \
             and shells out from an agent gets a hang, not a table"
        );
    }

    // The old sentence, in any of its spellings, is now false.
    for stale in [
        "bare `tasqx` lists your working set",
        "Bare `tasqx` is the working set",
        "bare `tasqx` shows your working set",
    ] {
        assert!(
            !text.contains(stale),
            "README still says {stale:?}, which is only true off a terminal"
        );
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
