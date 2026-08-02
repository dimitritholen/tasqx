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

use std::fs;
use std::path::{Path, PathBuf};

/// The workspace root, two levels above this crate's manifest.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn readme() -> String {
    fs::read_to_string(root().join("README.md")).expect("../../README.md is readable")
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
#[test]
fn readme_activation_lines_are_the_ones_the_binary_prints() {
    let readme = readme();
    let mut checked = 0;
    for shell in clap_complete::env::Shells::builtins().names() {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .args(["completions", shell])
            .output()
            .unwrap_or_else(|e| panic!("run `tasqx completions {shell}`: {e}"));
        assert!(
            out.status.success(),
            "`tasqx completions {shell}` failed: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        // `trim_end` and nothing more: the line is printed with a trailing
        // newline and is otherwise the exact text that has to reach the user's
        // startup file, spaces and quotes included.
        let printed = String::from_utf8_lossy(&out.stdout);
        let line = printed.trim_end();
        assert!(
            !line.is_empty(),
            "`tasqx completions {shell}` printed nothing to compare against"
        );
        assert!(
            readme.contains(line),
            "the README does not carry the {shell} activation line the binary \
             prints, so a reader who pastes what the README says gets a shell \
             that completes nothing and says nothing. Expected to find:\n  {line}"
        );
        checked += 1;
    }
    // Floor: the five shells this feature ships for. An empty registry would
    // otherwise make the loop above a clean pass over nothing.
    assert!(
        checked >= 5,
        "only {checked} shells were checked; `Shells::builtins()` shrank and \
         this guard is covering less than the README claims"
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
    // Floors: five worked guides plus the skill and the license. A scan that
    // finds fewer has lost links, not gained tidiness — lower these on purpose
    // or not at all.
    assert!(
        guides >= 5,
        "the README links only {guides} guides under docs/guides/"
    );
    assert!(checked >= 7, "the link scan checked only {checked} paths");
}
