//! Drift guards for `.claude/skills/tasqx-workflow/SKILL.md`.
//!
//! tasqx audit 2026-09 #171: the skill's own MCP roster count ("fifteen
//! `tasqx_*` tools") and its "the MCP deliberately lacks" fallback list both
//! went stale independently of `README.md`, which carries the equivalent
//! claim under a drift guard (`readme_mcp_tool_roster_matches_the_server`) and
//! stayed correct. This file gives SKILL.md the same binding, read out of the
//! live roster rather than trusted by eye — CLAUDE.md routes every session's
//! task tracking through this file, so a stale count here misleads an agent
//! that never opens a terminal to check.

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn skill() -> String {
    fs::read_to_string(root().join(".claude/skills/tasqx-workflow/SKILL.md"))
        .expect(".claude/skills/tasqx-workflow/SKILL.md is readable")
}

/// Same number-word table `readme.rs` uses, so the two guards read one claim
/// the same way. Duplicated rather than shared across the two test binaries —
/// `tests/*.rs` are separate crates and cannot import each other's helpers.
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

/// The skill claims a specific tool count in prose ("an MCP server (fifteen
/// `tasqx_*` tools)"); it must equal the roster the server actually answers
/// `tools/list` with, spelled the same way the README counts it.
#[test]
fn skill_tool_count_matches_the_mcp_roster() {
    let doc = skill();
    let roster = tasqx_core::mcp::tool_roster();
    let claim = format!("{} `tasqx_*` tools", word(roster.len()).to_lowercase());
    assert!(
        doc.contains(&claim),
        "SKILL.md no longer says {claim:?} — the MCP roster is {} tools now, \
         and the skill's own count did not move with it",
        roster.len()
    );
}

/// The skill's "fall back to the CLI" sentence names verbs the MCP
/// "deliberately lacks" — every one of those must actually be missing from
/// the roster. `reopen`, `undep` and `memory rm` shipped as MCP tools (D64,
/// D67) and were never walked out of this sentence, so an agent reading only
/// SKILL.md would shell out for a capability already sitting in its tool
/// list, or — worse, in an MCP-only integration with no shell — conclude the
/// capability does not exist at all.
#[test]
fn skill_cli_only_fallback_list_names_no_verb_the_mcp_already_serves() {
    let doc = skill();
    let roster = tasqx_core::mcp::tool_roster();
    let has = |tool: &str| roster.iter().any(|(n, _)| *n == tool);

    let start = doc
        .find("deliberately lacks")
        .expect("SKILL.md names what the MCP deliberately lacks");
    // The clause runs from there to the end of that sentence (the next
    // period), so a verb mentioned in some later, unrelated sentence cannot
    // produce a false failure here.
    let clause_end = doc[start..]
        .find(". ")
        .map(|i| start + i)
        .unwrap_or(doc.len());
    let clause = &doc[start..clause_end];

    // (CLI verb spelling as it appears in the fallback clause, the MCP tool
    // it would collide with if the fallback claim is now false).
    let claims: [(&str, &str); 3] = [
        ("reopen", "tasqx_reopen_task"),
        ("undep", "tasqx_remove_dependency"),
        ("memory rm", "tasqx_remove_memory"),
    ];
    for (verb, tool) in claims {
        assert!(
            has(tool),
            "precondition: {tool} should be on the MCP roster (D64/D67)"
        );
        assert!(
            !clause.contains(verb),
            "SKILL.md still lists `{verb}` among the verbs \"the MCP \
             deliberately lacks\" ({clause:?}), but {tool} is on the live \
             roster"
        );
    }
}
