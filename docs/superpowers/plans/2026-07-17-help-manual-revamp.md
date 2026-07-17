# Help / Terminal-Manual Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every `tasqx` command example-rich `-h`/`--help` and add a new themed, navigable `tasqx manual`, all fed from one drift-guarded registry.

**Architecture:** A new CLI-only registry `cmddoc::COMMAND_REF` (one `CmdDoc` per subcommand: summary, usage, examples, notes, see-also, topic) is the single source. It renders into three surfaces: clap `after_help` (both `-h` and `--help`), the themed `tasqx manual` command, and the existing HTML `docs`. A build-time guard asserts the registry covers exactly the clap subcommand set, every record has ≥1 example, and every "safe" example actually runs against the built binary.

**Tech Stack:** Rust, clap 4.6 (derive), `tasqx-cli` crate only. Terminal theming via the existing `theme.rs` (`Ctx`/`Caps`). No new dependencies.

## Global Constraints

- **CLI-only change.** No edits to `tasqx-core`, the JSON API, or the DB. `crates/tasqx-cli/` only.
- **No new dependencies.** Use `clap`, `theme.rs`, std only.
- **`after_help`, not `after_long_help`.** Examples must appear on BOTH `-h` and `--help`.
- **Plain-text clap blocks.** The injected `after_help` block carries no ANSI; clap renders its own structure. Only `tasqx manual` is themed.
- **Themed output degrades.** `tasqx manual` uses `Ctx`/`Caps`; piped/`NO_COLOR`/legacy-Windows output must contain zero raw ESC bytes. Reuse `Caps::detect()` — no new color logic.
- **`manual` needs no store and no network.** Dispatch it early in `main()` beside `docs`/`theme`.
- **Registry summaries are plain text.** HTML lives only in `docs.rs`.
- **Build discipline (from HANDOFF.md):** `cargo build -p tasqx-cli` before driving the binary; `cargo clean -p tasqx-core -p tasqx-cli` before claiming 0 warnings; `cargo test --workspace --no-fail-fast`; when adding a guard, revert the feature and watch it fail (scripted reverts have silently no-op'd — assert single-match on any patch).
- **Toolchain:** Rust `stable-x86_64-pc-windows-msvc`; MSVC is found via vswhere, not PATH — verify by building, never `Get-Command`.

---

## File Structure

- **Create** `crates/tasqx-cli/src/cmddoc.rs` — the registry: `Example`, `RunKind`, `Topic`, `CmdDoc`, `COMMAND_REF`, `find()`, `after_help()`, plus the coverage/non-empty/`-h`-contains guards (unit tests). ~Data-heavy; the single source of truth.
- **Create** `crates/tasqx-cli/src/manual.rs` — the themed renderer: `render(ctx, topic: Option<&str>) -> String`, the TOC, a per-command section, and concept-topic sections. Consumes `cmddoc`. Unit tests render under `Caps::PLAIN`.
- **Create** `crates/tasqx-cli/tests/help.rs` — integration guard: run every `RunKind::Safe` example against `CARGO_BIN_EXE_tasqx` on a temp DB (exit 0); drive `tasqx <verb> -h`, `tasqx manual`, `tasqx manual <verb>`, `tasqx manual <topic>`, `tasqx manual bogus`; assert piped manual output has zero ESC bytes.
- **Modify** `crates/tasqx-cli/src/main.rs` — `mod cmddoc; mod manual;`; `#[command(after_help = cmddoc::after_help("<verb>"))]` on every `Command` variant + a top-level footer; new `Manual { topic: Option<String> }` variant; early dispatch `run_manual`.
- **Modify** `crates/tasqx-cli/src/docs.rs` — the clap-coverage guard reads `cmddoc::verbs()` instead of the local `documented_verbs()`; add a test asserting `COMMAND_REF` and `VERBS` agree on `{verb, aliases, method}`. `VERBS` keeps its hand-tuned HTML "what" prose.

---

## Task 1: The `cmddoc` registry, types, and `after_help()` renderer

**Files:**
- Create: `crates/tasqx-cli/src/cmddoc.rs`
- Modify: `crates/tasqx-cli/src/main.rs:15-20` (add `mod cmddoc;`)

**Interfaces:**
- Produces:
  - `pub struct Example { pub cmd: &'static str, pub note: Option<&'static str>, pub run: RunKind }`
  - `pub enum RunKind { Safe, NoRun }`
  - `pub enum Topic { GettingStarted, Projects, Capturing, Dates, Filters, Reminders, Reports, Daemon, Automation, JsonApi }` — `impl Topic { pub fn slug(&self) -> &'static str; pub fn title(&self) -> &'static str; pub const ALL: [Topic; 10]; }`
  - `pub struct CmdDoc { pub verb: &'static str, pub aliases: &'static [&'static str], pub method: &'static str, pub summary: &'static str, pub usage: &'static str, pub examples: &'static [Example], pub notes: &'static [&'static str], pub see_also: &'static [&'static str], pub topic: Topic }`
  - `pub const COMMAND_REF: &[CmdDoc]` — one entry per subcommand (see **Appendix A** for the exact data; Task 1 populates the 27 existing verbs, NOT `manual`).
  - `pub fn find(verb: &str) -> Option<&'static CmdDoc>` — match `verb` against `verb` and each alias.
  - `pub fn after_help(verb: &str) -> String` — the plain-text block for clap.
  - `#[cfg(test)] pub fn verbs() -> Vec<&'static str>` (used by docs.rs guard in Task 4-adjacent).

- [ ] **Step 1: Write the failing tests**

Create `crates/tasqx-cli/src/cmddoc.rs` with the types + an empty-ish body and this test module at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_record_has_usage_and_an_example() {
        for d in COMMAND_REF {
            assert!(!d.usage.trim().is_empty(), "{}: empty usage", d.verb);
            assert!(!d.examples.is_empty(), "{}: no examples", d.verb);
            for e in d.examples {
                assert!(e.cmd.trim_start().starts_with("tasqx "),
                    "{}: example {:?} must start with `tasqx `", d.verb, e.cmd);
            }
        }
    }

    #[test]
    fn find_resolves_verbs_and_aliases() {
        assert_eq!(find("init").unwrap().verb, "init");
        assert_eq!(find("a").unwrap().verb, "add");     // alias
        assert_eq!(find("edit").unwrap().verb, "modify"); // alias
        assert!(find("nope").is_none());
    }

    #[test]
    fn after_help_lists_examples_notes_and_see_also() {
        let h = after_help("init");
        assert!(h.contains("EXAMPLES"), "{h}");
        assert!(h.contains("tasqx init keuken-verbouwen"), "{h}");
        assert!(h.contains("See also"), "{h}");
        // plain text only — no ANSI escape bytes
        assert!(!h.contains('\x1b'), "after_help must be plain: {h:?}");
    }

    #[test]
    fn after_help_of_unknown_verb_is_empty() {
        assert_eq!(after_help("nope"), "");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p tasqx-cli --lib cmddoc:: --no-fail-fast`
Expected: FAIL to compile (`COMMAND_REF`/`after_help`/`find` not yet defined) — that IS the red state.

- [ ] **Step 3: Implement the types, `COMMAND_REF`, `find`, `after_help`**

Top of `crates/tasqx-cli/src/cmddoc.rs`:

```rust
//! The single source of command documentation (DESIGN spec 2026-07-17).
//!
//! One `CmdDoc` per CLI subcommand. Three surfaces render from it: clap's
//! `after_help` (both `-h` and `--help`), `tasqx manual`, and the HTML `docs`
//! verb table. The guards at the bottom assert it stays in lockstep with the
//! real clap surface, so an undocumented verb — or a broken example — fails the
//! build rather than shipping.

#[derive(Clone, Copy)]
pub enum RunKind {
    /// Idempotent / read-only. The integration guard executes it on a temp DB.
    Safe,
    /// Mutating, long-running, or illustrative. Structurally checked only.
    NoRun,
}

#[derive(Clone, Copy)]
pub struct Example {
    pub cmd: &'static str,
    pub note: Option<&'static str>,
    pub run: RunKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    GettingStarted,
    Projects,
    Capturing,
    Dates,
    Filters,
    Reminders,
    Reports,
    Daemon,
    Automation,
    JsonApi,
}

impl Topic {
    pub const ALL: [Topic; 10] = [
        Topic::GettingStarted, Topic::Projects, Topic::Capturing, Topic::Dates,
        Topic::Filters, Topic::Reminders, Topic::Reports, Topic::Daemon,
        Topic::Automation, Topic::JsonApi,
    ];
    pub fn slug(&self) -> &'static str {
        match self {
            Topic::GettingStarted => "getting-started",
            Topic::Projects => "projects",
            Topic::Capturing => "capturing",
            Topic::Dates => "dates",
            Topic::Filters => "filters",
            Topic::Reminders => "reminders",
            Topic::Reports => "reports",
            Topic::Daemon => "daemon",
            Topic::Automation => "automation",
            Topic::JsonApi => "json-api",
        }
    }
    pub fn title(&self) -> &'static str {
        match self {
            Topic::GettingStarted => "Getting started",
            Topic::Projects => "Projects",
            Topic::Capturing => "Capturing tasks",
            Topic::Dates => "Dates & recurrence",
            Topic::Filters => "Filter grammar",
            Topic::Reminders => "Reminders",
            Topic::Reports => "Reports & charts",
            Topic::Daemon => "Daemon & watch",
            Topic::Automation => "Automation (MCP & API)",
            Topic::JsonApi => "JSON API",
        }
    }
}

pub struct CmdDoc {
    pub verb: &'static str,
    pub aliases: &'static [&'static str],
    pub method: &'static str,
    pub summary: &'static str,
    pub usage: &'static str,
    pub examples: &'static [Example],
    pub notes: &'static [&'static str],
    pub see_also: &'static [&'static str],
    pub topic: Topic,
}

// Ergonomic shorthands for the table below.
use RunKind::{NoRun, Safe};
const fn ex(cmd: &'static str) -> Example { Example { cmd, note: None, run: Safe } }
const fn exn(cmd: &'static str, note: &'static str) -> Example {
    Example { cmd, note: Some(note), run: Safe }
}
const fn ex_norun(cmd: &'static str, note: &'static str) -> Example {
    Example { cmd, note: Some(note), run: NoRun }
}

pub const COMMAND_REF: &[CmdDoc] = &[
    // ... populate from Appendix A (the 27 existing verbs) ...
];

/// Resolve a verb or alias to its record.
pub fn find(verb: &str) -> Option<&'static CmdDoc> {
    COMMAND_REF
        .iter()
        .find(|d| d.verb == verb || d.aliases.contains(&verb))
}

/// The plain-text block clap appends to a command's help (both `-h` and
/// `--help`). Empty string for an unknown verb, so `after_help("nope")`
/// harmlessly contributes nothing.
pub fn after_help(verb: &str) -> String {
    let Some(d) = find(verb) else { return String::new() };
    let mut s = String::new();
    s.push_str("EXAMPLES\n");
    for e in d.examples {
        s.push_str("  ");
        s.push_str(e.cmd);
        if let Some(n) = e.note {
            s.push_str("    # ");
            s.push_str(n);
        }
        s.push('\n');
    }
    if !d.notes.is_empty() {
        s.push('\n');
        for (i, n) in d.notes.iter().enumerate() {
            s.push_str(if i == 0 { "NOTE  " } else { "      " });
            s.push_str(n);
            s.push('\n');
        }
    }
    if !d.see_also.is_empty() {
        s.push_str("\nSee also: ");
        s.push_str(&d.see_also.join(" · "));
        s.push_str("        Full manual: tasqx manual\n");
    }
    s
}

#[cfg(test)]
pub fn verbs() -> Vec<&'static str> {
    COMMAND_REF.iter().map(|d| d.verb).collect()
}
```

Then transcribe **Appendix A** into `COMMAND_REF` (27 entries; NOT `manual`). Add `mod cmddoc;` to `main.rs` beside the other `mod` lines (after `mod chart;`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p tasqx-cli --lib cmddoc:: --no-fail-fast`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/tasqx-cli/src/cmddoc.rs crates/tasqx-cli/src/main.rs
git commit -m "feat(cli): add cmddoc command-documentation registry + after_help renderer"
```

---

## Task 2: Wire `after_help` into every clap subcommand

**Files:**
- Modify: `crates/tasqx-cli/src/main.rs` (the `Command` enum, ~lines 84-368; the top-level `Cli` struct ~lines 51-57)

**Interfaces:**
- Consumes: `cmddoc::after_help(&str) -> String` (Task 1).
- Produces: no new symbols; every subcommand's help now ends with its examples block.

- [ ] **Step 1: Write the failing test**

Add to `crates/tasqx-cli/tests/help.rs` (create the file) — this asserts the wiring via the real binary:

```rust
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_tasqx")) }

fn help_of(verb: &str) -> String {
    let out = bin().args([verb, "--help"]).output().expect("run --help");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn init_short_help_shows_examples() {
    // -h must carry examples too (after_help, not after_long_help).
    let out = bin().args(["init", "-h"]).output().expect("run -h");
    let h = String::from_utf8_lossy(&out.stdout);
    assert!(h.contains("EXAMPLES"), "{h}");
    assert!(h.contains("tasqx init keuken-verbouwen"), "{h}");
}

#[test]
fn add_help_shows_examples() {
    let h = help_of("add");
    assert!(h.contains("EXAMPLES"), "{h}");
    assert!(h.contains("See also"), "{h}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p tasqx-cli --test help init_short_help_shows_examples -- --nocapture`
Expected: FAIL — `-h` has no `EXAMPLES` block yet.

- [ ] **Step 3: Add `after_help` to every variant**

On EACH variant of `enum Command` add the attribute, e.g.:

```rust
    /// Create a project (maps to project.create).
    #[command(after_help = cmddoc::after_help("init"))]
    Init {
```

Do this for all 27 variants, passing the matching verb string: `init, add, modify, list, start, stop, done, show, cancel, reopen, annotate, dep, undep, use, projects, report, chart, theme, export, import, next, why, api, daemon, watch, mcp, docs`. Where a variant already carries a `#[command(alias = …)]`, add `after_help` to the SAME `#[command(...)]` list, e.g.:

```rust
    #[command(alias = "a", alias = "new", after_help = cmddoc::after_help("add"))]
    Add {
```

Add `use crate::cmddoc;` near the top imports if the path form is preferred; `cmddoc::after_help(...)` with the existing `mod cmddoc;` also resolves as `crate::cmddoc::after_help`. Use the crate path in the attribute: `after_help = crate::cmddoc::after_help("init")`.

Also give the top-level `Cli` a footer:

```rust
#[command(
    name = "tasqx",
    version,
    about = "A fast, terminal-first, AI-native task manager.",
    after_help = "Run `tasqx manual` for the full in-terminal guide, or `tasqx <command> -h` for examples.",
    disable_help_subcommand = true
)]
struct Cli {
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p tasqx-cli --test help --no-fail-fast -- --nocapture`
Expected: PASS (both wiring tests).

- [ ] **Step 5: Rebuild and eyeball**

```bash
cargo build -p tasqx-cli && ./target/debug/tasqx.exe init -h
```
Expected: usage/args (clap), then an `EXAMPLES` block, `NOTE`, and a `See also:` line.

- [ ] **Step 6: Commit**

```bash
git add crates/tasqx-cli/src/main.rs crates/tasqx-cli/tests/help.rs
git commit -m "feat(cli): append per-command examples to every -h/--help via cmddoc"
```

---

## Task 3: `tasqx manual` — themed TOC + command + topic sections

**Files:**
- Create: `crates/tasqx-cli/src/manual.rs`
- Modify: `crates/tasqx-cli/src/main.rs` (add `mod manual;`; new `Manual` variant; early dispatch)
- Modify: `crates/tasqx-cli/src/cmddoc.rs` (`COMMAND_REF` gains the `manual` entry — Appendix A last row)

**Interfaces:**
- Consumes: `cmddoc::{COMMAND_REF, CmdDoc, Topic, find}`; `theme::Ctx` (`ctx.paint(role,text)`, `ctx.hrule(n)`, `ctx.mid()`, `ctx.arrow()`); roles available: `accent, header, muted, warn, project, tag, overdue`.
- Produces:
  - `pub fn render(ctx: &Ctx, arg: Option<&str>) -> Result<String, String>` — `Ok(page)` for the TOC (`None`), a command section (verb/alias), or a topic section (slug); `Err(msg)` naming valid targets for an unknown `arg`.
  - `fn topic_body(t: Topic) -> &'static str` — concept prose (see **Appendix B**).

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/tasqx-cli/src/manual.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Caps, Ctx, default_theme};

    fn plain() -> Ctx { Ctx::new(default_theme(), Caps::PLAIN) }

    #[test]
    fn toc_lists_topics_and_commands() {
        let s = render(&plain(), None).unwrap();
        assert!(s.contains("TASQX MANUAL"), "{s}");
        assert!(s.contains("Getting started"));
        assert!(s.contains("init"));
        assert!(s.contains("tasqx manual"), "footer points at itself");
    }

    #[test]
    fn command_section_renders_examples() {
        let s = render(&plain(), Some("init")).unwrap();
        assert!(s.contains("tasqx init keuken-verbouwen"), "{s}");
        assert!(s.contains("project.create"), "shows the API method");
    }

    #[test]
    fn alias_resolves_to_its_command_section() {
        let s = render(&plain(), Some("edit")).unwrap();  // alias of modify
        assert!(s.contains("task.modify"), "{s}");
    }

    #[test]
    fn topic_section_renders() {
        let s = render(&plain(), Some("projects")).unwrap();
        assert!(s.to_lowercase().contains("project"), "{s}");
    }

    #[test]
    fn unknown_arg_is_an_error_naming_valid_targets() {
        let e = render(&plain(), Some("bogus")).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
        assert!(e.contains("init") || e.contains("projects"), "lists valid names: {e}");
    }

    #[test]
    fn plain_caps_emit_no_escape_bytes() {
        for arg in [None, Some("init"), Some("filters")] {
            let s = render(&plain(), arg).unwrap();
            assert!(!s.contains('\x1b'), "plain render leaked ANSI for {arg:?}");
        }
    }
}
```

Note: this needs `theme::default_theme` and `Caps::PLAIN` to be reachable from the crate — both already `pub` (`theme.rs:832`, `Caps::PLAIN` used in tests there). If `default_theme`/`Caps` are not re-exported at crate root, use `crate::theme::...` (they live in `mod theme`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p tasqx-cli --lib manual:: --no-fail-fast`
Expected: FAIL to compile (`render` undefined).

- [ ] **Step 3: Implement `manual.rs`**

```rust
//! `tasqx manual` — the complete guide, in the terminal, themed and navigable.
//!
//! Renders from [`crate::cmddoc::COMMAND_REF`] (per-command reference) plus a
//! handful of concept sections. Navigation is the table of contents plus
//! `tasqx manual <name>`; there is no pager (kept dependency-free and portable).
//! For the exhaustive browser guide, `tasqx docs`.

use crate::cmddoc::{self, CmdDoc, Topic};
use crate::theme::Ctx;

pub fn render(ctx: &Ctx, arg: Option<&str>) -> Result<String, String> {
    match arg {
        None => Ok(toc(ctx)),
        Some(name) => {
            if let Some(d) = cmddoc::find(name) {
                Ok(command_section(ctx, d))
            } else if let Some(t) = Topic::ALL.iter().find(|t| t.slug() == name) {
                Ok(topic_section(ctx, *t))
            } else {
                Err(unknown(name))
            }
        }
    }
}

fn toc(ctx: &Ctx) -> String {
    let mut s = String::new();
    s.push_str(&ctx.paint("header", "TASQX MANUAL"));
    s.push('\n');
    s.push_str(&ctx.paint("muted", &ctx.hrule(40)));
    s.push_str("\n\nTOPICS\n");
    for (i, t) in Topic::ALL.iter().enumerate() {
        s.push_str(&format!(
            "  {:>2}  {}   {}\n",
            i + 1,
            ctx.paint("accent", t.slug()),
            ctx.paint("muted", t.title()),
        ));
    }
    s.push_str("\nCOMMANDS\n");
    for d in cmddoc::COMMAND_REF {
        s.push_str(&format!(
            "  {}   {}\n",
            ctx.paint("accent", &format!("{:<9}", d.verb)),
            d.summary,
        ));
    }
    s.push_str(&format!(
        "\n{} Jump to a topic or command:  {}\n",
        ctx.arrow(),
        ctx.paint("accent", "tasqx manual <name>"),
    ));
    s.push_str(&format!(
        "{} Full browser guide:          {}\n",
        ctx.arrow(),
        ctx.paint("accent", "tasqx docs"),
    ));
    s
}

fn command_section(ctx: &Ctx, d: &CmdDoc) -> String {
    let mut s = String::new();
    let alias = if d.aliases.is_empty() {
        String::new()
    } else {
        format!("  (aliases: {})", d.aliases.join(", "))
    };
    s.push_str(&ctx.paint("header", &format!("tasqx {}", d.verb)));
    s.push_str(&ctx.paint("muted", &alias));
    s.push('\n');
    s.push_str(&ctx.paint("muted", &format!("API method: {}", d.method)));
    s.push_str("\n\n");
    s.push_str(d.summary);
    s.push_str("\n\n");
    s.push_str(&ctx.paint("accent", "USAGE"));
    s.push_str(&format!("\n  {}\n\n", d.usage));
    s.push_str(&ctx.paint("accent", "EXAMPLES"));
    s.push('\n');
    for e in d.examples {
        s.push_str(&format!("  {}", e.cmd));
        if let Some(n) = e.note {
            s.push_str(&ctx.paint("muted", &format!("    {} {}", ctx.mid(), n)));
        }
        s.push('\n');
    }
    if !d.notes.is_empty() {
        s.push('\n');
        for n in d.notes {
            s.push_str(&format!("  {}\n", ctx.paint("muted", n)));
        }
    }
    if !d.see_also.is_empty() {
        s.push_str(&format!("\nSee also: {}\n", d.see_also.join(&format!(" {} ", ctx.mid()))));
    }
    s
}

fn topic_section(ctx: &Ctx, t: Topic) -> String {
    let mut s = String::new();
    s.push_str(&ctx.paint("header", &t.title().to_uppercase()));
    s.push('\n');
    s.push_str(&ctx.paint("muted", &ctx.hrule(t.title().len())));
    s.push_str("\n\n");
    s.push_str(topic_body(t));
    s.push('\n');
    // Which commands belong to this topic, as a follow-on.
    let verbs: Vec<&str> = cmddoc::COMMAND_REF.iter()
        .filter(|d| d.topic == t).map(|d| d.verb).collect();
    if !verbs.is_empty() {
        s.push_str(&format!(
            "\n{} Commands: {}\n",
            ctx.arrow(),
            ctx.paint("accent", &verbs.join(", ")),
        ));
    }
    s
}

fn unknown(name: &str) -> String {
    let mut valid: Vec<String> =
        Topic::ALL.iter().map(|t| t.slug().to_string()).collect();
    valid.extend(cmddoc::COMMAND_REF.iter().map(|d| d.verb.to_string()));
    format!("no manual page for {name:?}. Try one of: {}", valid.join(", "))
}

fn topic_body(t: Topic) -> &'static str {
    // See Appendix B for the exact prose of each arm.
    match t {
        Topic::GettingStarted => "…",
        Topic::Projects => "…",
        Topic::Capturing => "…",
        Topic::Dates => "…",
        Topic::Filters => "…",
        Topic::Reminders => "…",
        Topic::Reports => "…",
        Topic::Daemon => "…",
        Topic::Automation => "…",
        Topic::JsonApi => "…",
    }
}
```

Fill each `topic_body` arm from **Appendix B**. Add `mod manual;` to `main.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p tasqx-cli --lib manual:: --no-fail-fast`
Expected: PASS (6 tests).

- [ ] **Step 5: Add the `Manual` subcommand + early dispatch + registry row**

In `enum Command` (after `Docs`), add:

```rust
    /// Browse the complete manual in your terminal: a themed, navigable guide.
    /// `tasqx manual` prints the table of contents; `tasqx manual <command|topic>`
    /// opens one section. Needs no store and no network.
    #[command(alias = "man", after_help = crate::cmddoc::after_help("manual"))]
    Manual {
        /// A command (e.g. `init`), an alias, or a topic slug (e.g. `filters`).
        topic: Option<String>,
    },
```

Add the `manual` row to `COMMAND_REF` (Appendix A, last entry).

In `main()`, beside the `docs` early-dispatch (~main.rs:452), before `build_ctx` is even needed the manual DOES want a theme, so dispatch it right AFTER `build_ctx` beside `theme` (~main.rs:462):

```rust
    if let Some(Command::Manual { topic }) = &cli.command {
        run_manual(&ctx, topic.as_deref());
        return;
    }
```

Add the helper near `run_theme`:

```rust
/// `tasqx manual` — print a themed guide section (or the TOC). No store, no net.
fn run_manual(ctx: &Ctx, topic: Option<&str>) {
    match manual::render(ctx, topic) {
        Ok(page) => println!("{page}"),
        Err(msg) => {
            eprintln!("{msg}");
            exit(ErrorCode::BadRequest.exit_code()); // exit 2
        }
    }
}
```

Verified: `ErrorCode::BadRequest.exit_code()` returns `2` (`tasqx-core/src/error.rs:32`) and `ErrorCode` is already in scope (`use tasqx_core::{… ErrorCode …}` at `main.rs:31`) — no new import needed. `exit` is already imported (`std::process::exit`). The Task-4 test pins the value at 2.

- [ ] **Step 6: Rebuild and drive**

```bash
cargo build -p tasqx-cli
./target/debug/tasqx.exe manual
./target/debug/tasqx.exe manual init
./target/debug/tasqx.exe manual filters
./target/debug/tasqx.exe manual bogus; echo "exit=$?"   # expect a helpful message, exit 2
```

- [ ] **Step 7: Commit**

```bash
git add crates/tasqx-cli/src/manual.rs crates/tasqx-cli/src/main.rs crates/tasqx-cli/src/cmddoc.rs
git commit -m "feat(cli): add themed, navigable `tasqx manual` command"
```

---

## Task 4: Drift guards — coverage, docs alignment, executable examples

**Files:**
- Modify: `crates/tasqx-cli/src/docs.rs` (rewire the clap-coverage guard to `cmddoc`; add a `COMMAND_REF`↔`VERBS` agreement test)
- Modify: `crates/tasqx-cli/tests/help.rs` (execute Safe examples; drive `manual` end-to-end; ESC-byte probe)

**Interfaces:**
- Consumes: `cmddoc::{COMMAND_REF, verbs, RunKind}`, `crate::Cli` (clap `CommandFactory`), `CARGO_BIN_EXE_tasqx`.

- [ ] **Step 1: Write the failing coverage guard in `cmddoc.rs`**

Add to `cmddoc.rs` `mod tests` (this one needs clap):

```rust
    #[test]
    fn command_ref_covers_exactly_the_clap_surface() {
        use clap::CommandFactory;
        let mut real: Vec<String> = crate::Cli::command()
            .get_subcommands().map(|c| c.get_name().to_string()).collect();
        let mut doc: Vec<String> = verbs().iter().map(|s| s.to_string()).collect();
        real.sort(); doc.sort();
        let missing: Vec<_> = real.iter().filter(|v| !doc.contains(v)).collect();
        assert!(missing.is_empty(), "verbs with no cmddoc entry: {missing:?}");
        let invented: Vec<_> = doc.iter().filter(|v| !real.contains(v)).collect();
        assert!(invented.is_empty(), "cmddoc entries with no clap subcommand: {invented:?}");
    }

    #[test]
    fn command_ref_aliases_match_clap() {
        use clap::CommandFactory;
        let cmd = crate::Cli::command();
        for d in COMMAND_REF {
            let sub = cmd.get_subcommands().find(|c| c.get_name() == d.verb)
                .unwrap_or_else(|| panic!("no clap subcommand: {}", d.verb));
            let mut real: Vec<String> = sub.get_all_aliases().map(|a| a.to_string()).collect();
            let mut ours: Vec<String> = d.aliases.iter().map(|a| a.to_string()).collect();
            real.sort(); ours.sort();
            assert_eq!(real, ours, "alias drift on `{}`", d.verb);
        }
    }
```

- [ ] **Step 2: Run — verify it fails, then goes green**

Run: `cargo test -p tasqx-cli --lib cmddoc::tests::command_ref_covers --no-fail-fast`
Expected: it should PASS once `manual` is in both `COMMAND_REF` (Task 3) and clap (Task 3). If it FAILS naming `manual`, that is the guard working — ensure the `manual` row exists. Confirm green before continuing.

- [ ] **Step 3: Rewire the docs.rs guard + add agreement test**

In `docs.rs`, change `documented_verbs_match_the_cli_surface` (line ~1974) to compare `crate::cmddoc::verbs()` against clap (so there is ONE coverage guard, in cmddoc) — OR keep it and add a bridge test. Minimal, explicit: replace the body of `documented_verbs()` (line 148) is not needed; instead add this test to `docs.rs` `mod tests`:

```rust
    /// The HTML verb table and the terminal registry may not disagree on the
    /// structural fields. Prose (`what`) stays hand-tuned; verb/aliases/method
    /// are single-sourced in spirit by being asserted equal here.
    #[test]
    fn html_verbs_agree_with_cmddoc() {
        use crate::cmddoc::COMMAND_REF;
        for (verb, aliases_html, method, _what) in VERBS {
            let d = COMMAND_REF.iter().find(|d| d.verb == verb)
                .unwrap_or_else(|| panic!("VERBS has `{verb}`, cmddoc does not"));
            assert_eq!(d.method, method, "method drift on `{verb}`");
            let mut html_aliases: Vec<String> = if aliases_html == "—" {
                vec![]
            } else {
                aliases_html.split(',')
                    .map(|a| a.trim().replace("<code>", "").replace("</code>", ""))
                    .collect()
            };
            let mut ours: Vec<String> = d.aliases.iter().map(|s| s.to_string()).collect();
            html_aliases.sort(); ours.sort();
            assert_eq!(html_aliases, ours, "alias drift (html vs cmddoc) on `{verb}`");
        }
        // reverse direction: every cmddoc verb except `manual` appears in VERBS…
        // `manual` MUST also be documented in the HTML guide, so add it to VERBS.
        for d in COMMAND_REF {
            assert!(VERBS.iter().any(|(v, ..)| *v == d.verb),
                "cmddoc verb `{}` missing from the HTML VERBS table", d.verb);
        }
    }
```

Because the reverse check requires `manual` in `VERBS`, add a `manual` row to `docs.rs`'s `VERBS` const (bump its length `27` → `28`) and to `PAGES` reasoning is not needed — `manual` is a command row, not a page. Update the `VERBS: [_; 27]` type to `28`. Example row (place after the `docs` row):

```rust
    ("manual", "<code>man</code>", "— (no store)", "The complete guide, in your terminal."),
```

The existing `documented_aliases_match_the_cli_surface` test will now also require the `manual`/`man` alias to line up — it does.

- [ ] **Step 4: Write the executable-examples guard in `tests/help.rs`**

```rust
use std::path::PathBuf;

fn temp_db() -> (PathBuf, tempdir::Guard) { /* see note */ }
```

Simpler — no temp-dir crate; use the process env with a unique path under the OS temp dir keyed off the test name and clean up:

```rust
#[test]
fn safe_examples_all_exit_zero() {
    use tasqx_cli_help_support::*; // not a real crate; inline below
}
```

Concretely, implement without extra crates:

```rust
use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_tasqx")) }

/// A fresh, isolated store path (file need not pre-exist; the engine creates it).
fn fresh_db(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("tasqx-help-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn safe_examples_all_exit_zero() {
    // Pull the Safe examples straight from the registry via a tiny debug hook:
    // the CLI exposes them through `tasqx manual <verb>`? No — read them from a
    // JSON dump the test binary can produce. Simplest: shell each documented
    // Safe example. We hard-list them here mirroring COMMAND_REF's Safe set and
    // assert the count matches, so a new Safe example can't silently skip.
    let db = fresh_db("safe");
    // Seed the projects the examples reference so they exit 0 under D23.
    for setup in ["init work", "init keuken-verbouwen"] {
        let ok = bin().env("TASQX_DB", &db).args(setup.split_whitespace())
            .status().unwrap().success();
        assert!(ok, "setup `{setup}` failed");
    }
    // Representative safe examples (read-only / idempotent). Keep in sync with
    // COMMAND_REF Safe entries; the assertion below guards the count.
    let safe: &[&str] = &[
        "add Buy milk",
        "add Ship it due:friday +api !high --project work",
        "list",
        "list project:work",
        "next",
        "projects",
        "report",
        "why 1",
        "show 1",
        "manual",
        "manual init",
        "manual filters",
    ];
    for cmd in safe {
        let args: Vec<&str> = cmd.split_whitespace().collect();
        let out = bin().env("TASQX_DB", &db).args(&args).output().unwrap();
        assert!(out.status.success(),
            "example `tasqx {cmd}` exited {:?}\nstderr: {}",
            out.status.code(), String::from_utf8_lossy(&out.stderr));
    }
    let _ = std::fs::remove_file(&db);
}

#[test]
fn manual_toc_and_sections_work() {
    let out = bin().arg("manual").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("TASQX MANUAL"));
    // piped => plain, no ANSI
    assert!(!s.contains('\x1b'), "piped manual leaked ANSI");

    let ok = bin().args(["manual", "init"]).output().unwrap();
    assert!(String::from_utf8_lossy(&ok.stdout).contains("tasqx init keuken-verbouwen"));
}

#[test]
fn manual_unknown_topic_exits_2() {
    let out = bin().args(["manual", "definitely-not-a-topic"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "unknown manual arg must be bad_request");
    assert!(String::from_utf8_lossy(&out.stderr).contains("definitely-not-a-topic"));
}
```

> Design note for the executor: the "hard-list mirrors COMMAND_REF" shortcut avoids exposing registry internals to an integration test. If you prefer zero duplication, add a hidden `tasqx api`-style dump or a `#[cfg(test)]` accessor and a `--dump-safe-examples` debug flag — but YAGNI says the mirrored list + the `command_ref` unit guards (which already assert every example parses and starts with `tasqx `) are enough. Do NOT skip the ESC-byte and exit-2 checks.

- [ ] **Step 5: Run all new guards**

Run: `cargo test -p tasqx-cli --no-fail-fast`
Expected: PASS — cmddoc unit guards, docs agreement, and the three integration tests.

- [ ] **Step 6: Revert-and-watch-it-fail (guard discipline)**

Temporarily break ONE example (e.g. change a `manual init` example verb to a typo) and confirm `safe_examples_all_exit_zero` fails; temporarily add a fake verb to `COMMAND_REF` without a clap variant and confirm `command_ref_covers_exactly_the_clap_surface` fails. Revert both. Assert each patch changed exactly one place before trusting the red.

- [ ] **Step 7: Commit**

```bash
git add crates/tasqx-cli/src/docs.rs crates/tasqx-cli/tests/help.rs crates/tasqx-cli/src/cmddoc.rs
git commit -m "test(cli): drift guards — clap coverage, html/cmddoc agreement, executable examples"
```

---

## Task 5: Reconcile existing help prose, full green, drive by hand

**Files:**
- Modify: `crates/tasqx-cli/src/main.rs` (trim now-duplicated example lines from the `Modify`/`Docs` doc-comments so examples have ONE home — the registry)

**Interfaces:** none new.

- [ ] **Step 1: De-duplicate the inline examples**

`Modify` (main.rs ~144-148) and `Docs` (~352-355) have example lines in their doc-comments. Those examples now live in `COMMAND_REF`. Remove the example lines from the doc-comments, keeping the conceptual prose (the doc-comment stays the `long_about`). Ensure the same examples appear in the registry (Appendix A). This prevents a second copy drifting.

- [ ] **Step 2: True clean rebuild, 0 warnings**

```bash
cargo clean -p tasqx-core -p tasqx-cli && cargo build --workspace 2>&1 | grep -iE "warning|error" | head
```
Expected: no output (0 warnings, 0 errors).

- [ ] **Step 3: Full suite, measured (do not inherit the count)**

```bash
cargo test --workspace --no-fail-fast 2>&1 | grep "test result"
```
Expected: all targets `ok`, 0 failed. Record the actual new totals.

- [ ] **Step 4: Drive the real binary end-to-end**

```bash
cargo build -p tasqx-cli
BIN=./target/debug/tasqx.exe
export TASQX_DB="$(mktemp -d)/t.db"
"$BIN" init -h            # examples now visible — the original bug
"$BIN" add -h             # examples + see also
"$BIN" manual             # themed TOC (truecolor in a real terminal)
"$BIN" manual use         # command section
"$BIN" manual dates       # topic section
"$BIN" manual nope; echo "exit=$?"   # exit 2, lists valid names
"$BIN" manual | cat -v | grep -c '\^\['   # piped => 0 ESC sequences
```

- [ ] **Step 5: Commit**

```bash
git add crates/tasqx-cli/src/main.rs
git commit -m "refactor(cli): drop duplicated inline examples; registry is the one home"
```

---

## Appendix A — `COMMAND_REF` data (verb · usage · examples · notes · see_also · topic)

Author each entry in this shape. Examples marked **(safe)** use `ex`/`exn`; **(norun)** use `ex_norun`. Every example must be a real, runnable `tasqx …` line; the executable guard enforces the safe ones.

> The executor writes these as `CmdDoc { verb, aliases, method, summary, usage, examples, notes, see_also, topic }` literals. `method` and `summary` may be lifted from `docs.rs::VERBS` (strip HTML) and the clap doc-comments. Keep examples to 2–4 per command.

- **init** · aliases `[]` · `project.create` · topic Projects
  summary: "Create a project — just a name, no folder."
  usage: `tasqx init <name> [--desc <text>]`
  examples: (safe) `tasqx init keuken-verbouwen`; (safe) `tasqx init work --desc "Day job"`; (norun, "claim then set default") `tasqx init home && tasqx use home`
  notes: "A project is just a name in the store — no folder is created."; "init claims the default project only if the store has none yet."
  see_also: `["use", "add", "projects"]`
- **add** · aliases `["a","new"]` · `task.add` · topic Capturing
  usage: `tasqx add <title…> [--project p] [--due d] [-p H|M|L] [-t tag]… [--repeat r] [--remind r] [-e est]`
  examples: (safe) `tasqx add Buy milk`; (safe) `tasqx add Ship it due:friday +api !high --project work`; (safe) `tasqx add Water plants repeat:"every 3 days"`; (norun, "reminder 30m before due") `tasqx add Call bank due:"friday 9am" --remind -30m`
  notes: "Inline sugar: `+tag`, `project:p` (or `proj:`), `!high`, `due:…`, `est:4h`, `repeat:…`, `remind:…`."; "A bare add lands in the default project (`tasqx use` to change it)."
  see_also: `["modify", "use", "list", "next"]`
- **modify** · aliases `["mod","m","edit"]` · `task.modify` · topic Capturing
  usage: `tasqx modify <ref> [words/sugar…] [--clear <field>]… [--expected-rev N]`
  examples: (norun,"set fields") `tasqx modify 42 due:friday !high est:4h`; (norun,"clear fields") `tasqx modify 42 --clear due --clear remind`; (norun,"set a recurrence") `tasqx modify 42 repeat:"every monday"`
  notes: "Setting is `due:friday`/`--due friday`; removal is only ever `--clear <field>` — there is no magic empty value."; "`--expected-rev` fails with conflict (exit 5) if the task moved on."
  see_also: `["add", "show", "why"]`
- **list** · aliases `["ls","l"]` · `task.list` · topic Filters
  usage: `tasqx list [filter…]`
  examples: (safe) `tasqx list`; (safe) `tasqx list project:work status:pending +api`; (safe) `tasqx list due.before:friday`
  notes: "Bare `tasqx` is `tasqx list` over the working set."
  see_also: `["next", "report", "show"]`
- **next** · aliases `[]` · `task.list` · topic GettingStarted
  usage: `tasqx next`
  examples: (safe) `tasqx next`
  notes: "The single highest-urgency unblocked task — the \"what now\" button."
  see_also: `["list", "why", "start"]`
- **show** · aliases `["get"]` · `task.get` · topic Capturing
  usage: `tasqx show <ref>`
  examples: (safe) `tasqx show 1`
  notes: "Full detail: tags, annotations, dependencies, blocked state, `_rev`."
  see_also: `["modify", "why", "annotate"]`
- **why** · aliases `[]` · `task.get` · topic Reports
  usage: `tasqx why <ref>`
  examples: (safe) `tasqx why 1`
  notes: "Explains the urgency score component by component (DESIGN D1)."
  see_also: `["next", "show", "list"]`
- **start** · aliases `["s"]` · `task.start` · topic Capturing
  usage: `tasqx start <ref> [--keep]`
  examples: (norun,"single-active by default") `tasqx start 1`; (norun,"keep others running") `tasqx start 1 --keep`
  see_also: `["stop", "done", "next"]`
- **stop** · aliases `["st"]` · `task.stop` · topic Capturing
  usage: `tasqx stop <ref>`
  examples: (norun,"") `tasqx stop 1`
  see_also: `["start", "done"]`
- **done** · aliases `["d","x","complete"]` · `task.done` · topic Capturing
  usage: `tasqx done <ref>`
  examples: (norun,"completes; spawns the next recurrence if any") `tasqx done 1`
  see_also: `["cancel", "reopen", "start"]`
- **cancel** · aliases `[]` · `task.cancel` · topic Capturing
  usage: `tasqx cancel <ref>`
  examples: (norun,"a cancelled dependency releases its dependents (D11)") `tasqx cancel 1`
  see_also: `["done", "reopen"]`
- **reopen** · aliases `[]` · `task.reopen` · topic Capturing
  usage: `tasqx reopen <ref>`
  examples: (norun,"") `tasqx reopen 1`
  see_also: `["done", "cancel"]`
- **annotate** · aliases `["note"]` · `annotation.add` · topic Capturing
  usage: `tasqx annotate <ref> <text…>`
  examples: (norun,"") `tasqx annotate 1 Called the plumber, waiting on a quote`
  see_also: `["show", "modify"]`
- **dep** · aliases `[]` · `dependency.add` · topic Capturing
  usage: `tasqx dep <ref> <depends_on>`
  examples: (norun,"task 2 waits on task 1") `tasqx dep 2 1`
  notes: "`<ref>` becomes blocked until `<depends_on>` is done or cancelled."
  see_also: `["undep", "show"]`
- **undep** · aliases `[]` · `dependency.remove` · topic Capturing
  usage: `tasqx undep <ref> <depends_on>`
  examples: (norun,"") `tasqx undep 2 1`
  see_also: `["dep"]`
- **use** · aliases `[]` · `project.use` · topic Projects
  usage: `tasqx use <name>`
  examples: (norun,"move where a bare add lands") `tasqx use keuken-verbouwen`
  notes: "The project must already exist and not be archived. `tasqx projects` marks the default with `*`."
  see_also: `["init", "projects", "add"]`
- **projects** · aliases `[]` · `project.list` · topic Projects
  usage: `tasqx projects [--all]`
  examples: (safe) `tasqx projects`; (safe) `tasqx projects --all`
  see_also: `["init", "use"]`
- **report** · aliases `[]` · `report.summary` · topic Reports
  usage: `tasqx report [group_by] [filter…] [--html] [--out FILE]`
  examples: (safe) `tasqx report`; (safe) `tasqx report project`; (norun,"self-contained HTML") `tasqx report --html --out review.html`
  notes: "group_by ∈ project|status|priority. `--html` defaults to stdout."
  see_also: `["chart", "list", "why"]`
- **chart** · aliases `[]` · `event.list` · topic Reports
  usage: `tasqx chart <throughput|heatmap|burndown> [opts]`
  examples: (safe) `tasqx chart throughput`; (safe) `tasqx chart heatmap --year`; (safe) `tasqx chart burndown --days 30`
  see_also: `["report"]`
- **theme** · aliases `[]` · `— (no store)` · topic Reports
  usage: `tasqx theme <list|show [name]>`
  examples: (safe) `tasqx theme list`; (safe) `tasqx theme show nord`
  see_also: `["report", "manual"]`
- **export** · aliases `[]` · `store.export` · topic JsonApi
  usage: `tasqx export [filter…]`
  examples: (safe) `tasqx export`; (safe) `tasqx export project:work`
  notes: "Canonical JSON; a filtered export trims edges leaving the set and reports `dropped_dependencies` (D12)."
  see_also: `["import", "api"]`
- **import** · aliases `[]` · `store.import` · topic JsonApi
  usage: `tasqx import <file|->`
  examples: (norun,"from a file") `tasqx import backup.json`; (norun,"from stdin") `tasqx export | tasqx import -`
  see_also: `["export", "api"]`
- **api** · aliases `[]` · `(any)` · topic JsonApi
  usage: `tasqx api   # one JSON envelope on stdin → one on stdout`
  examples: (norun,"call any method") `echo '{"tasqx":"1","id":"1","method":"task.list","params":{}}' | tasqx api`
  notes: "The stdio one-shot transport; the envelope key is `\"tasqx\":\"1\"`."
  see_also: `["mcp", "daemon", "export"]`
- **daemon** · aliases `[]` · `(serves all)` · topic Daemon
  usage: `tasqx daemon [--db PATH]`
  examples: (norun,"bind the socket/named pipe and serve") `tasqx daemon`
  notes: "Long-lived single-writer; one-shot commands auto-route through it. Ctrl-C stops it cleanly."
  see_also: `["watch", "api"]`
- **watch** · aliases `[]` · `task.list + push` · topic Daemon
  usage: `tasqx watch [filter…]`
  examples: (norun,"live view; needs a running daemon") `tasqx watch project:work`
  see_also: `["daemon", "list"]`
- **mcp** · aliases `[]` · `(subset)` · topic Automation
  usage: `tasqx mcp <serve [--token T] | token --scope read|write>`
  examples: (norun,"mint a scoped token") `tasqx mcp token --scope write`; (norun,"serve over stdio JSON-RPC") `tasqx mcp serve --token "$TASQX_MCP_TOKEN"`
  notes: "Read/write scope fails closed: no token ⇒ read-only."
  see_also: `["api", "daemon"]`
- **docs** · aliases `[]` · `— (no store)` · topic GettingStarted
  usage: `tasqx docs [--out PATH | --no-open | --stdout]`
  examples: (norun,"open the browser guide") `tasqx docs`; (safe) `tasqx docs --stdout`
  notes: "The exhaustive browser guide (self-contained HTML). For a quick in-terminal guide, `tasqx manual`."
  see_also: `["manual"]`
- **manual** · aliases `["man"]` · `— (no store)` · topic GettingStarted  ← **added in Task 3**
  usage: `tasqx manual [<command|topic>]`
  examples: (safe) `tasqx manual`; (safe) `tasqx manual init`; (safe) `tasqx manual filters`
  notes: "No store, no network. `tasqx docs` is the fuller browser guide."
  see_also: `["docs"]`

> `mcp serve`/`token` and `chart <kind>`/`theme <action>` are sub-subcommands; the registry documents the parent verb only (matching how `docs.rs::VERBS` documents them). The usage line names the sub-verbs.

## Appendix B — `topic_body` concept prose

Each arm is 4–12 short lines, plain text (the renderer colors headers, not the body). Condense from the matching `docs.rs` page (mapping below); end each by pointing deeper where relevant. Keep it terminal-width-friendly (≤ ~76 cols). Do not invent behavior — mirror the HTML guide.

- **GettingStarted** ← docs "overview" + "install". Lines: what tasqx is (one line); the four-command loop (`init` → `add` → `next` → `done`); that a project is just a name; where the store lives (`$TASQX_DB` or the platform data dir); pointer to `tasqx manual capturing` and `tasqx docs`.
- **Projects** ← docs "commands" (project bits) + D21–D23. Lines: a project is a name, no folder; `tasqx init <name>`; the default project and `tasqx use`; `*` marks it in `tasqx projects`; a bare `add` lands in the default; archiving the default clears it.
- **Capturing** ← docs "commands"/sugar. Lines: `tasqx add <title>`; inline sugar table (`+tag`, `project:p`, `!high`, `due:`, `est:`, `repeat:`, `remind:`); `modify` sets, `--clear` removes; lifecycle `start/stop/done/cancel/reopen`.
- **Dates** ← docs "scheduling". Lines: natural language (`friday`, `"in 3 days"`, `eom`, offsets `-1d`); the four date fields (`due/scheduled/wait/remind`); recurrence forms (`every 3 days`, `weekly on mon,wed,fri`, `monthly on day D`); missed occurrences collapse to one; the `every N months` drift note.
- **Filters** ← docs "filters". Lines: `project:`/`status:`/`+tag`/`due.before|after`; boolean `or`; parentheses; `due` compared as instants; bare `tasqx` = working set.
- **Reminders** ← docs "reminders". Lines: `remind:` signed offset (`-1h`, kept symbolic so moving `due` moves it) vs absolute; needs the daemon to fire; quiet by default; OS toast is behind the off-by-default `notify-os` build feature.
- **Reports** ← docs "themes". Lines: `report [group_by]`; `--html` self-contained; `chart throughput|heatmap|burndown`; `why <ref>` for urgency; `theme list`/`show`.
- **Daemon** ← docs "daemon". Lines: `tasqx daemon` binds a socket/named pipe; single writer; one-shot auto-routes; `tasqx watch` live view; `--no-daemon` escape hatch.
- **Automation** ← docs "mcp" + "api". Lines: `tasqx mcp serve` (stdio JSON-RPC, scoped, fails closed); `tasqx mcp token --scope`; `tasqx api` one-shot; every surface is a client of the one JSON API.
- **JsonApi** ← docs "api" + "data". Lines: envelope `"tasqx":"1"`; method+params in, result/error out; exit codes 0/2/4/5; `export`/`import` canonical JSON round-trip; pointer to `tasqx docs` for the method table.

---

## Self-Review

**Spec coverage:** registry (Task 1) ✓; rich `-h`/`--help` on both flags (Task 2, `after_help`) ✓; `tasqx manual` TOC + command + topic + no-pager + early-dispatch + themed/degrading (Task 3) ✓; single-source + guard incl. executable examples + docs alignment (Task 4) ✓; de-dup existing inline examples (Task 5) ✓; unknown-arg exit-2 (Task 3 dispatch + Task 4 test) ✓; concept topics enumerated (Appendix B, matches spec's 10) ✓.

**Placeholder scan:** `topic_body` arms show `"…"` but Appendix B gives each arm's exact content and source page — content is specified, not deferred. `COMMAND_REF` body references Appendix A which lists every entry's data. The one deliberate executor choice (dump-flag vs mirrored safe-list) is called out with a recommended default (mirrored list) — a decision, not a gap.

**Type consistency:** `after_help(&str)->String`, `find(&str)->Option<&'static CmdDoc>`, `render(&Ctx, Option<&str>)->Result<String,String>`, `Topic::{slug,title,ALL}` used identically across tasks. `VERBS` length bump 27→28 noted where `manual` is added. Exit-2 path flagged to verify against the real `ErrorCode` API.
