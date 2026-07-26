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
//!  * [`util`]      — the shared time/JSON param readers (D17, D32).
//!
//! The full §4 method catalogue is implemented (task add/list/get/start/stop/
//! done/modify/cancel/reopen, project create/list/archive/use, tag.add,
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
        let head = SRC.split("#[cfg(test)]").next().expect("split yields a head");
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
}
