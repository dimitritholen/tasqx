//! # tasqx-core
//!
//! The headless engine behind tasqx (DESIGN.md §2). A plain Rust library: the
//! CLI links it and calls [`dispatch()`] in-process with no serialization tax,
//! while the stdio `api` transport wraps that same dispatch table in the JSON
//! envelope via [`handle_envelope`]. There is exactly one dispatch table, so
//! "call a function" and "send a JSON command" run identical code.
//!
//! `dispatch` is written with parentheses throughout this header, and that is
//! not decoration: the name is BOTH the [`mod@dispatch`] module and the function
//! re-exported from it, so a bare ``[`dispatch`]`` is an ambiguous link that
//! rustdoc renders as plain text. The module map's first entry was silently
//! unlinked for exactly that reason.
//!
//! Module map — every `pub mod`, because a partial map is worse than none: a
//! reader who found ten of fifteen listed had no way to tell the five absent
//! ones from five that do not exist.
//!  * [`types`]     — domain types + the request envelope (the serde contract).
//!  * [`error`]     — the five stable API error codes.
//!  * [`storage`]   — SQLite setup (WAL, busy_timeout), schema, row primitives.
//!  * [`engine`]    — [`Engine`] and the per-method mutation/query logic.
//!  * [`mod@dispatch`] — the single dispatch table + envelope handling.
//!  * [`filter`]    — the filter DSL subset used by `task.list`.
//!  * [`urgency`]   — the fixed urgency formula.
//!  * [`datetime`]  — the natural-language date grammar, `now` always injected.
//!  * [`recur`]     — the v1 recurrence subset (D2) and the next-occurrence rule.
//!  * [`remind`]    — reminder specs: `due`-anchored offsets + absolute instants.
//!  * [`scheduler`] — the daemon's reminder min-heap (§9), ripeness by injected now.
//!  * [`notify`]    — the `Notifier` trait; log backend always, OS behind `notify-os`.
//!  * [`daemon`]    — the long-lived socket transport over that same table.
//!  * [`mcp`]       — the bundled stdio MCP server (§7), a thin dispatch client.
//!  * [`markdown`]  — the one task-detail rendering: pure, caller-independent.
//!  * [`tokens`]    — the token vocabulary + the per-tool transcript parsers.
//!  * [`otlp`]      — the opt-in local OTLP receiver that buffers live samples.
//!  * [`attribution`] — a task's window turned into a measured token spend (#17).
//!  * [`util`]      — the shared time/JSON param readers (D17, D32).
//!
//! The full §4 method catalogue is implemented (task add/list/get/start/stop/
//! done/modify/cancel/reopen, project create/list/archive/use, tag add/remove,
//! annotation.add, dependency add/remove, memory add/search/remove/import,
//! report.summary, store export/import, event.list, reminder.fire,
//! core.capabilities). Real dependency/blocked logic and the §12-D8
//! boolean/grouping filter grammar are wired in, as are the daemon transport,
//! the MCP server and recurrence — all three of which this paragraph listed as
//! unbuilt long after they shipped.
//!
//! Not built yet (clear seams, no build-breaking stubs): hooks/plugins,
//! `event.revert`/undo, and configurable urgency weights. Adding them stays
//! additive — new match arms in [`dispatch()`] and new engine methods — with no
//! change to the envelope.

// tasqx-core is a library other code links: DESIGN.md's §"Native Rust" binding
// hands out `use tasqx_core::{Engine, Command};` and accepts semver coupling to
// this crate in exchange for skipping serialization. That makes every `pub`
// item here somebody else's compile-time dependency, and
// `types` below is literally the JSON wire contract. `missing_docs` is
// allow-by-default and clippy never enables it, so until this line every field
// name WAS its own documentation. `warn` and not `deny`: the CI test job runs
// with `RUSTFLAGS: -D warnings`, which is what makes this fatal where it
// matters, while a local `cargo check` mid-refactor still compiles.
#![warn(missing_docs)]

pub mod attribution;
pub mod daemon;
pub mod datetime;
pub mod dispatch;
pub mod engine;
pub mod error;
pub mod filter;
pub mod markdown;
pub mod mcp;
pub mod notify;
pub mod otlp;
pub mod recur;
pub mod remind;
pub mod scheduler;
pub mod storage;
pub mod tokens;
pub mod types;
pub mod urgency;
pub mod util;

pub use dispatch::{capabilities, dispatch, handle_envelope, API_VERSION, PARAMS};
pub use engine::Engine;
pub use error::{ApiError, ErrorCode};
pub use mcp::{McpServer, Scope};
pub use notify::{Notification, Notifier};
pub use scheduler::ReminderScheduler;
pub use types::{Entity, Priority, Status, Task};

#[cfg(test)]
mod doc_gate_tests {
    /// This crate's own source, and the workflow its gates live in. Both are
    /// `include_str!` rather than a runtime read: a renamed workflow or a moved
    /// `lib.rs` has to be a COMPILE error here. A guard that reads a path at run
    /// time answers "file missing" with the same failure as "gate removed", and
    /// on a refactor the first is the likelier of the two — at which point
    /// somebody deletes the assertion instead of the drift.
    const SRC: &str = include_str!("lib.rs");
    const CI: &str = include_str!("../../../.github/workflows/ci.yml");

    /// The two places the last mutation sweep is written down, and the config
    /// that decides what a sweep covers at all. Same `include_str!` reasoning as
    /// above.
    const MUTATION_DOC: &str = include_str!("../../../docs/mutation-testing.md");
    const MUTANTS_TOML: &str = include_str!("../../../.cargo/mutants.toml");

    /// The lines of the one workflow step whose `run:` mentions `needle`,
    /// including the `env:` block that follows it. Steps are `- ` items at a
    /// fixed indent inside `steps:`, so the step ends at the next line whose
    /// trimmed form starts with `- `.
    ///
    /// Whole-file `contains` is not enough for either guard below: both assert
    /// that a command and an environment variable travel TOGETHER, and a file
    /// that happens to carry each of them in different jobs would satisfy a
    /// pair of independent substring checks while gating nothing.
    fn step_with(needle: &str) -> String {
        let lines: Vec<&str> = CI.lines().collect();
        // `- run:` and not merely `contains`: this workflow's comments quote the
        // commands they explain, and matching one of those found a block of
        // prose with no `env:` in it — a guard that fails on the documentation
        // rather than on the gate.
        let start = lines
            .iter()
            .position(|l| l.trim_start().starts_with("- run:") && l.contains(needle))
            .unwrap_or_else(|| panic!("no CI step runs {needle:?}"));
        let mut out = vec![lines[start]];
        for line in &lines[start + 1..] {
            if line.trim_start().starts_with("- ") {
                break;
            }
            out.push(line);
        }
        out.join("\n")
    }

    /// `cargo clippy` never invokes rustdoc, so the `-D warnings` clippy job
    /// cannot see a single broken intra-doc link. Without a `cargo doc` step the
    /// rendered documentation is the one contract in this repo with no drift
    /// guard: `Engine::begin` sat in the engine module header pointing at a
    /// method renamed to `begin_mutation`, and nothing anywhere went red.
    ///
    /// `RUSTDOCFLAGS` is asserted on the same step because rustdoc's link lints
    /// are warn-by-default — without it `cargo doc` prints the drift and exits 0,
    /// which is the state this repo already had.
    #[test]
    fn ci_fails_the_build_on_a_rustdoc_warning() {
        let step = step_with("cargo doc");
        assert!(
            step.contains("--workspace"),
            "the doc gate must cover BOTH crates. It was scoped to -p tasqx-core \
             while tasqx-cli still had public docs linking to private items; those \
             are code spans now, and narrowing the gate again would let the CLI's \
             rendered docs rot with every gate green:\n{step}"
        );
        assert!(
            step.contains("--all-features"),
            "without --all-features the `notify-os` backend is documented by \
             nothing, which is the same hole the notify-feature job exists to \
             close for the build:\n{step}"
        );
        assert!(
            step.contains("RUSTDOCFLAGS: -D warnings"),
            "a `cargo doc` run that only WARNS exits 0 and gates nothing:\n{step}"
        );
    }

    /// `missing_docs` is allow-by-default in rustc and clippy never turns it on,
    /// so a public field added to a serde-carrying struct in `types.rs` — the
    /// JSON wire contract — arrives undocumented with every gate still green.
    ///
    /// Two halves, and BOTH are required. The attribute alone only warns; the
    /// test job's `RUSTFLAGS: -D warnings` is what makes it fatal. Asserting
    /// them together is the point: removing either one silently reopens the gap,
    /// and each looks harmless on its own in a diff.
    #[test]
    fn undocumented_public_items_fail_the_build() {
        // Only the part of the file above this module: the assertions below
        // quote the attribute they look for, so searching the whole source would
        // find the test's own text and pass with the attribute absent.
        let head = SRC
            .split("#[cfg(test)]")
            .next()
            .expect("split yields a head");
        assert!(
            head.contains("#![warn(missing_docs)]"),
            "tasqx-core is a library DESIGN.md tells clients to link \
             (`use tasqx_core::{{Engine, Command}}`); an undocumented public \
             item must not compile quietly"
        );
        let step = step_with("cargo test --workspace --all-targets");
        assert!(
            step.contains("RUSTFLAGS: -D warnings"),
            "`warn(missing_docs)` is only a gate while the test job denies \
             warnings:\n{step}"
        );
    }

    // ----- the mutation-sweep note -----------------------------------------
    //
    // Two files write down the same `cargo mutants` run in prose: the comment
    // above the `mutants` job in ci.yml, and the "Known surviving mutants"
    // section of docs/mutation-testing.md. Both read "160 mutants, 143 caught,
    // 0 missed" from an undated local sweep while two consecutive CI sweeps
    // reported 203 mutants and a survivor — and the sentence a reader would
    // have acted on, "`missed` is now 0", sat directly under the heading BEFORE
    // THIS BECOMES A GATE. Nothing could see it: the audit that corrected 66
    // comparable claims scanned `crates/**/*.rs`, and a workflow comment is
    // neither Rust nor rendered.
    //
    // None of the guards below can know what the newest sweep found; that costs
    // the better part of an hour on a runner. What they can do is make a note
    // that has stopped being RE-DERIVED fail out loud rather than keep reading
    // plausibly: the counts have to add up as arithmetic, the two files have to
    // be quoting one run and to name which, and every survivor has to still
    // resolve to a function in a file the sweep actually covers. A snapshot
    // left to rot breaks at least one of those as soon as the code moves under
    // it, which is precisely when it stops being safe to believe.

    /// The workspace root, for the reads whose path is not known until a note
    /// has been parsed. Everything with a fixed path is `include_str!`ed above.
    fn workspace_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// A YAML comment or markdown line reduced to the prose in it: leading `#`
    /// gone, runs of whitespace collapsed. One sentence indented in a workflow
    /// comment and fenced in markdown has to compare equal, or the cross-file
    /// check fails on the gutter instead of on the numbers.
    fn as_prose(line: &str) -> String {
        line.trim()
            .trim_start_matches('#')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The one line in `text` that restates a `cargo mutants` summary: where it
    /// is, what it says, and every `<number> <label>` pair on it.
    ///
    /// The pairs are read off the line positionally rather than by a fixed
    /// template, so this does not encode the order cargo-mutants happens to
    /// print its counts in today. A guard pinned to a tool's output shape fails
    /// on the next tool upgrade, and a failure about nothing is how a guard
    /// gets deleted.
    ///
    /// The anchor is `mutants tested` WITHOUT the colon that follows it in the
    /// prose here, because the tool does not print one there: the real line is
    /// `203 mutants tested in 53m: 1 missed, ...`, with the wall clock between
    /// the noun and the counts. Anchoring on `mutants tested:` meant that
    /// pasting in the summary line — the one thing the comment above the
    /// `mutants` job tells you to do — failed every check here with "the file
    /// has no sweep line" while the correct line sat in it. An error that names
    /// the opposite of the caller's mistake gets fixed by mangling the input
    /// into a shape only the guard knows, or by deleting the guard.
    fn sweep_summary(text: &str, whose: &str) -> (usize, String, Vec<(u64, String)>) {
        let hits: Vec<(usize, &str)> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains("mutants tested"))
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "{whose} must restate the last sweep on exactly one line, verbatim \
             as `cargo mutants` prints it (`N mutants tested in <duration>: N \
             missed, N caught, N unviable, N timeouts`), and it has {}. Exactly \
             one, because a second copy in the same file drifts away from the \
             first and the reader cannot tell which of them is the newer.",
            hits.len()
        );
        let (idx, raw) = hits[0];
        let prose = as_prose(raw);
        let flat = prose.replace([',', ':'], " ");
        let words: Vec<&str> = flat.split_whitespace().collect();
        let counts = words
            .windows(2)
            .filter_map(|p| p[0].parse::<u64>().ok().map(|n| (n, p[1].to_string())))
            .collect();
        (idx, prose, counts)
    }

    /// One labelled count off a sweep summary — `missed`, `caught`, and so on.
    fn sweep_count(counts: &[(u64, String)], label: &str, whose: &str) -> u64 {
        let hits: Vec<u64> = counts
            .iter()
            .filter(|(_, l)| l == label)
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "{whose}'s sweep line must state `<n> {label}` exactly once; it \
             states it {} times. The counts are the whole reason anyone reads \
             the line, so a dropped or doubled one is not a formatting quibble.",
            hits.len()
        );
        hits[0]
    }

    /// Every GitHub run id cited in the lines that introduce the sweep summary
    /// at `idx`.
    ///
    /// Provenance is the actual repair for this class of rot. A figure that
    /// names the run it came from can be re-derived by anyone who doubts it; a
    /// figure attributed to "the last local sweep" can only be believed, and
    /// this one was believed for long enough to become false. The window is
    /// joined into a single string before scanning, so `run` and its id may sit
    /// on opposite sides of a line wrap — which is where reflowing a paragraph
    /// puts them about half the time.
    fn cited_runs(text: &str, idx: usize, whose: &str) -> std::collections::BTreeSet<String> {
        let lines: Vec<&str> = text.lines().collect();
        let window = lines[idx.saturating_sub(12)..=idx].join(" ");
        let words: Vec<&str> = window.split_whitespace().collect();
        let runs: std::collections::BTreeSet<String> = words
            .windows(2)
            .filter(|p| {
                p[0].trim_end_matches([',', ':'])
                    .eq_ignore_ascii_case("run")
            })
            .map(|p| p[1].trim_matches(|c: char| !c.is_ascii_digit()).to_string())
            .filter(|id| id.len() >= 8 && id.chars().all(|c| c.is_ascii_digit()))
            .collect();
        assert!(
            !runs.is_empty(),
            "{whose} states sweep figures without naming the run they came \
             from. An undated snapshot is exactly how this note came to claim \
             zero missed mutants while CI was reporting one — name the run \
             (`run 30811736799 on <sha>`) in the lines that introduce the \
             figures, so the next reader can check them instead of trusting \
             them."
        );
        runs
    }

    /// The body lines of the TOML array `key` in `.cargo/mutants.toml`, between
    /// its assignment and the `]` that closes it.
    ///
    /// Ending at the first `]` in the remaining text was the obvious parse and
    /// the wrong one, and it failed silently in the direction that reports
    /// green: `exclude_re`'s justification comments discuss `And([a, Always,
    /// b])`, so the list looked closed before its first entry, the suppression
    /// count came out 0, and the floor built on it could not fail. Caught only
    /// by mutating the docs to violate that floor and watching nothing happen.
    /// The closer is therefore a line that IS `]`.
    fn config_list(key: &str) -> Vec<&'static str> {
        let opener = format!("{key} = [");
        let mut lines = MUTANTS_TOML.lines().skip_while(|l| !l.starts_with(&opener));
        assert!(
            lines.next().is_some(),
            ".cargo/mutants.toml must set `{opener}` at the start of a line; \
             the sweep notes are checked against it"
        );
        let body: Vec<&str> = lines.take_while(|l| l.trim() != "]").collect();
        assert!(
            !body.is_empty(),
            "`{key}` parsed to an empty list, so every check reading it would \
             pass by iterating over nothing"
        );
        body
    }

    /// The paths `.cargo/mutants.toml` puts in scope. A survivor outside them
    /// cannot exist, so this doubles as the resolver for the bare file names the
    /// docs table uses.
    fn examined_paths() -> Vec<String> {
        // One quoted path per line; the rest of the block is comments.
        let paths: Vec<String> = config_list("examine_globs")
            .iter()
            .filter_map(|l| l.trim().strip_prefix('"'))
            .filter_map(|l| l.split('"').next())
            .map(str::to_string)
            .collect();
        assert!(
            !paths.is_empty(),
            "`examine_globs` holds no quoted paths, so every scope check would \
             pass by iterating over an empty list"
        );
        paths
    }

    /// The survivors a note lists, one prose line each.
    ///
    /// Matched on uppercase `MISSED`, as cargo-mutants prints it: the prose
    /// around these lines says "missed" in lowercase a dozen times.
    fn survivor_lines(text: &str) -> Vec<String> {
        text.lines()
            .map(as_prose)
            .filter(|l| l.starts_with("MISSED"))
            .collect()
    }

    /// The function a `MISSED` line records its mutant against.
    fn recorded_fn(line: &str) -> &str {
        line.split('`').nth(1).unwrap_or_else(|| {
            panic!("a MISSED line must name the function in backticks:\n  {line}")
        })
    }

    /// Each `exclude_re` entry in `.cargo/mutants.toml`, as the function whose
    /// mutant it suppresses.
    ///
    /// cargo-mutants names every mutant `<the change> in <path::to::fn>` and
    /// the regexes are written against that text, so the tail after the last
    /// ` in ` is the function. Reading it back is what lets the triage table be
    /// checked for a row per suppression BY NAME. The count it replaced could
    /// not: the table's TIMEOUT row answers to neither a suppression nor a
    /// miss, so `rows >= suppressed + missed` carried a permanent row of slack,
    /// and a third suppression with no triage row anywhere — the exact silence
    /// the config forbids two comments above `exclude_re` — stayed green.
    ///
    /// Every non-comment line in the block is an entry, not only the
    /// single-quoted ones. Filtering on a leading `'` would have read zero
    /// entries off an equally valid double-quoted TOML string, and a check with
    /// nothing to check passes.
    fn suppressed_fns() -> Vec<String> {
        config_list("exclude_re")
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|entry| {
                let re = entry.trim_end_matches(',').trim_matches(['\'', '"']);
                assert!(
                    re.contains(" in "),
                    "the `exclude_re` entry `{re}` does not say which function \
                     it suppresses a mutant in. cargo-mutants writes every \
                     mutant as `<the change> in <path::to::fn>` — keep that \
                     tail in the regex, so the known-survivors table can be \
                     checked for the row this suppression is required to have."
                );
                let tail = re.rsplit(" in ").next().expect("rsplit yields a tail");
                let name = tail.rsplit("::").next().expect("rsplit yields a tail");
                // Whatever regex punctuation the entry anchors with (`$`, an
                // escaped `.`) is not part of the identifier being looked up.
                name.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .to_string()
            })
            .collect()
    }

    /// Whether `src` defines a function called `name`.
    ///
    /// A textual `fn <name>(` and nothing cleverer, because that is all these
    /// guards can honestly claim: whether the place a survivor was recorded
    /// against still exists. An earlier version sliced the function body out
    /// and asserted the mutated operator still appeared in it — which sounds
    /// stronger and is not. Rewriting `spacing_hint`'s `||` into an iterator
    /// chain, the likeliest shape of actually fixing tasqx #48, left the guard
    /// green: the function has an unrelated `||` two lines up, and the check
    /// could not tell them apart. A guard that survives the change it exists to
    /// notice is worse than no guard, so it is gone rather than weakened.
    fn defines_fn(src: &str, name: &str) -> bool {
        src.contains(&format!("fn {name}("))
    }

    /// The sweep figures are a reading of one run, and both files that carry
    /// them must be reading the SAME run.
    ///
    /// Three properties, and each of them failed on the note this replaced. The
    /// counts must add up (`160 mutants, 143 caught, 0 missed` accounted for
    /// neither the unviable nor the timed-out ones, so nothing about it could
    /// be checked). The two files must quote one identical line (they held two
    /// separately maintained copies, and the markdown one added `10 unviable,
    /// 2-5 timeouts` — figures that do not sum to 160 under any reading of the
    /// range). And both must name the run, because the previous attribution was
    /// "the last local sweep", which no one can look up.
    #[test]
    fn the_mutation_sweep_note_is_one_dated_reading_both_files_agree_on() {
        let ci_where = ".github/workflows/ci.yml";
        let doc_where = "docs/mutation-testing.md";
        let (ci_idx, ci_line, ci_counts) = sweep_summary(CI, ci_where);
        let (doc_idx, doc_line, _) = sweep_summary(MUTATION_DOC, doc_where);
        assert_eq!(
            ci_line, doc_line,
            "{ci_where} and {doc_where} must quote the same sweep line \
             verbatim. Two independently maintained copies is how one of them \
             ends up a scope-change out of date while the other is right, with \
             nothing on either page telling a reader which to believe."
        );

        let total = sweep_count(&ci_counts, "mutants", ci_where);
        let parts = ["missed", "caught", "unviable", "timeouts"];
        let split: Vec<u64> = parts
            .iter()
            .map(|l| sweep_count(&ci_counts, l, ci_where))
            .collect();
        assert_eq!(
            total,
            split.iter().sum::<u64>(),
            "the sweep line must account for every mutant it tested: {total} \
             tested against {parts:?} = {split:?}. A total that does not match \
             its own breakdown means the line was edited in part — which is the \
             half-update that leaves a stale figure standing next to a fresh \
             one:\n  {ci_line}"
        );

        assert_eq!(
            cited_runs(CI, ci_idx, ci_where),
            cited_runs(MUTATION_DOC, doc_idx, doc_where),
            "both files must attribute their figures to the same run(s). \
             Different ids mean one of the two was re-derived and the other was \
             not, and the counts agreeing anyway is a coincidence rather than a \
             check."
        );
    }

    /// A survivor is recorded as a place in the code, and that place must still
    /// exist.
    ///
    /// The sweep's one missed mutant is `spacing_hint`'s `||` in filter.rs
    /// (tasqx #48). If that function is renamed, moved, deleted or dropped out
    /// of `examine_globs`, this fails and the whole note has to be re-derived —
    /// which is roughly the only moment anybody would think to do it. It is why
    /// the note records a FUNCTION rather than the `file:line:col`
    /// cargo-mutants prints: the docs table carried line numbers, and every one
    /// of them had rotted (`parse_and` recorded at 227, living at 824) without
    /// ever failing.
    ///
    /// What it does NOT do, so nobody mistakes it for a sweep: it cannot tell
    /// whether the survivor is still missed. That answer costs an hour on a
    /// runner and lives in the run this note names. The guard checks that the
    /// note still describes reachable code and admits to as many survivors as
    /// its own count claims; believing the count is still a matter of trusting
    /// whoever last re-derived it.
    #[test]
    fn a_surviving_mutant_is_recorded_where_it_still_lives() {
        let whose = ".github/workflows/ci.yml";
        let (_, _, counts) = sweep_summary(CI, whose);
        let missed = sweep_count(&counts, "missed", whose);
        let survivors = survivor_lines(CI);
        assert_eq!(
            survivors.len() as u64,
            missed,
            "the note says {missed} missed mutant(s) and lists {}. A count \
             without the survivors under it is a number nobody can act on, and \
             a survivor the count does not admit to is how `missed is now 0` \
             survived two sweeps that disagreed:\n{survivors:#?}",
            survivors.len()
        );

        let scope = examined_paths();
        for line in &survivors {
            let path = line
                .split_whitespace()
                .find(|w| w.ends_with(".rs"))
                .unwrap_or_else(|| panic!("a MISSED line must name a source file:\n  {line}"));
            assert!(
                scope.contains(&path.to_string()),
                "{path} is not in `examine_globs`, so no sweep could have \
                 reported a mutant there. Either the note or the config is \
                 stale:\n  {line}\n  scope: {scope:?}"
            );
            let src = std::fs::read_to_string(workspace_root().join(path))
                .unwrap_or_else(|e| panic!("{path}, named by a MISSED line, is unreadable: {e}"));
            let name = recorded_fn(line);
            assert!(
                defines_fn(&src, name),
                "{path} has no `fn {name}`, so this survivor is recorded \
                 against code that no longer exists. Re-derive the note from \
                 the newest sweep rather than editing this line to match:\n  {line}"
            );
            assert!(
                line.split_whitespace().any(|w| {
                    w.starts_with('#') && w.len() > 1 && w[1..].chars().all(|c| c.is_ascii_digit())
                }),
                "a survivor left in place must name the task tracking it \
                 (`tasqx #48`). An untracked known gap is one nobody has \
                 decided about — it just accumulated:\n  {line}"
            );
        }
    }

    /// The docs table names every survivor that is being left in place, and
    /// each name still resolves to a function in a file the sweep covers.
    ///
    /// The table is the triage record: it is what stops a suppressed mutant in
    /// `.cargo/mutants.toml`, or a missed one in the note, from being an
    /// unexplained silence. So each of those is looked up in it BY NAME.
    ///
    /// It used to be a count — `rows >= suppressed + missed` — and a count was
    /// the wrong shape twice over. The table's TIMEOUT row answers to neither a
    /// suppression nor a miss, so the floor carried one row of permanent slack:
    /// deleting the row for the ONE missed mutant, the entire subject of the
    /// note, left 3 rows against a floor of 3 and passed, and a third
    /// `exclude_re` entry with no triage row anywhere passed for the same
    /// reason. Both are the silence this check exists to prevent, and a floor
    /// that sits below the real requirement is a guard that has stopped
    /// guarding while still reporting green — the failure this repo has shipped
    /// three times.
    ///
    /// What it still cannot see: a function that two rows name. The `pos +=`
    /// timeout row also names `parse_and`, so it would satisfy the requirement
    /// that `parse_and`'s suppression have a row if the row explaining that
    /// suppression were deleted. Matching a suppression to the row that
    /// justifies IT would mean reading the Mutation and Verdict cells, and
    /// those are prose with escaped pipes in them.
    #[test]
    fn every_known_survivor_in_the_docs_names_a_function_that_still_exists() {
        let scope = examined_paths();
        let mut named: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for line in MUTATION_DOC.lines() {
            // Only the Location column, which is the first cell. Cells beyond
            // it are not addressable by splitting on `|`: a mutation like
            // `||` -> `&&` has to be written with escaped pipes inside the row.
            let Some(location) = line.strip_prefix("| ").and_then(|l| l.split('|').next()) else {
                continue;
            };
            let spans: Vec<&str> = location.split('`').skip(1).step_by(2).collect();
            // A row of the OTHER table on that page (`caught`, `MISSED`, ...)
            // opens the same way and names no file; those are not survivors.
            let Some((file, names)) = spans.split_first().filter(|(f, _)| f.ends_with(".rs"))
            else {
                continue;
            };
            let path = scope
                .iter()
                .find(|p| p.ends_with(&format!("/{file}")))
                .unwrap_or_else(|| {
                    panic!(
                        "the known-survivors table names {file}, which is not \
                         in `examine_globs` — no sweep reports mutants there:\n  {line}"
                    )
                });
            let src = std::fs::read_to_string(workspace_root().join(path))
                .unwrap_or_else(|e| panic!("{path} is unreadable: {e}"));
            for name in names {
                assert!(
                    defines_fn(&src, name),
                    "the known-survivors table records a mutant in `{name}`, \
                     and {path} has no such function. Line numbers were dropped \
                     from this table because they rotted invisibly; a function \
                     name is only better while something checks it:\n  {line}"
                );
                named.insert(name);
            }
        }

        // Two things oblige the table to carry a row, and neither of them is
        // recorded anywhere else: a suppression is invisible in the sweep
        // output by construction, and a missed mutant left in place is a
        // decision somebody has to be able to read back.
        let survivors = survivor_lines(CI);
        let required = suppressed_fns()
            .into_iter()
            .map(|f| {
                (
                    f,
                    "suppressed by `exclude_re` in .cargo/mutants.toml, which \
                     that file says is legitimate only for a provably \
                     unkillable mutant — anything merely hard to test belongs \
                     in the table instead",
                )
            })
            .chain(survivors.iter().map(|l| {
                (
                    recorded_fn(l).to_string(),
                    "reported MISSED by the sweep .github/workflows/ci.yml \
                     records, and a survivor left in place is a decision the \
                     next reader has to be able to read back",
                )
            }));
        for (name, why) in required {
            assert!(
                named.contains(name.as_str()),
                "`{name}` is {why}. No row of the known-survivors table in \
                 docs/mutation-testing.md names it: either write the row or \
                 stop leaving the mutant alone. Rows name {named:?}"
            );
        }
    }
}
