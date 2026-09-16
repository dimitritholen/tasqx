//! The CLI's door onto the clock: validation, and then [`tasqx_core::clock`],
//! which is where the clock itself lives.
//!
//! Every "what time is it" in `tasqx-cli` comes through [`now`]. A guard test at
//! the bottom of this file reads every `.rs` file under `src/` and fails on a
//! `Timestamp::now` or `Zoned::now` anywhere else; the engine has the same guard
//! over its own `src/`, so the two crates have one clock between them.
//!
//! # `TASQX_NOW`
//!
//! Set `TASQX_NOW` to an RFC 3339 instant (`2026-09-16T09:00:00Z`, or any offset
//! spelling jiff accepts) and every instant this process reads is that one: the
//! dates the CLI spells, the stamps the engine writes, the instant urgency is
//! scored at. It exists so the documentation pipeline can render its screens
//! reproducibly — a captured `list`, `agenda`, `why`, `chart` or write echo
//! prints `due tomorrow`, `3d`, `created today`, a burndown window and an
//! urgency score, so the same store rendered on two different days produces two
//! different files and a drift job comparing them would fire on every calendar
//! day rather than on a real change. `scripts/demo-store.py` reads the same
//! variable, so the store's dates and the render's reference instant come from
//! one pinned day.
//!
//! It is a testing and capture hook, not a user feature: it is documented here,
//! in [`tasqx_core::clock`] and in `CONTRIBUTING.md`, and deliberately not in
//! `tasqx docs`, the wiki or the guides. Nobody tracking real work wants their
//! overdue tasks frozen — and because the pin reaches the store too (DESIGN.md
//! D148), `tasqx about` states it beside the store and the build whenever it is
//! set.
//!
//! **This is the door that validates.** An unparsable value is fatal here: one
//! line on stderr and exit 2, before an engine exists. A typo that silently fell
//! back to the wall clock would produce exactly the drift the pin was set to
//! prevent, and the capture would look like it worked. The engine cannot exit a
//! process it does not own, so it falls back there instead — and every process
//! that reaches the engine (the one-shot CLI, `api`, `mcp serve`, `daemon`,
//! `watch`) comes through this door first.

/// The reference instant: `TASQX_NOW` when it is set, the wall clock otherwise.
///
/// Exits 2 — the CLI's `bad_request` code (DESIGN.md §4) — with one line on
/// stderr when `TASQX_NOW` holds something that is not an RFC 3339 instant, so
/// the engine's own fallback for that case is unreachable from a real run.
///
/// The environment is read on every call rather than once into a `OnceLock`:
/// the dashboard re-reads the clock on every tick and a cached wall-clock
/// reading would freeze a live screen.
pub fn now() -> jiff::Timestamp {
    match tasqx_core::clock::pin_from_env() {
        Ok(Some(pin)) => pin,
        Ok(None) => tasqx_core::clock::now(),
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    }
}

/// The pin as `tasqx about` reports it (D132's clock row): `Some` only when
/// `TASQX_NOW` names an instant [`now`] would use. An unparsable value cannot
/// reach here — `now` exits on it — so the row never spells a value the process
/// is not actually running on.
pub fn pin() -> Option<jiff::Timestamp> {
    tasqx_core::clock::pin_from_env().ok().flatten()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    /// The parse itself belongs to the engine (`tasqx_core::clock::pinned`, and
    /// tested there); what this crate owns is that the value `about` reports is
    /// the value the clock would use, and that a bad one exits 2 — the latter
    /// driven through the real binary in `tests/regressions.rs`.
    #[test]
    fn the_reported_pin_is_the_instant_the_clock_would_use() {
        let want: jiff::Timestamp = "2026-09-16T09:00:00Z".parse().unwrap();
        assert_eq!(
            tasqx_core::clock::pinned(Some("2026-09-16T09:00:00Z")).unwrap(),
            Some(want)
        );
        assert_eq!(tasqx_core::clock::pinned(Some("  ")).unwrap(), None);
    }

    /// Every `.rs` file under `crates/tasqx-cli/src`, this one excluded.
    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension() == Some(OsStr::new("rs"))
                && path.file_name() != Some(OsStr::new("clock.rs"))
            {
                out.push(path);
            }
        }
    }

    /// One door, and nothing walks around it.
    ///
    /// `TASQX_NOW` can only pin what it is asked for: a single
    /// `jiff::Timestamp::now()` left in a renderer, a verb or a TUI screen makes
    /// that one date move while the rest of the capture stands still, which is
    /// worse than no pin at all — the drift is now in one row of one screen
    /// instead of all of them, where a reader will read it as a real change.
    ///
    /// Test code is held to the same rule rather than exempted: deciding
    /// whether a line sits inside a `#[cfg(test)]` block means balancing braces
    /// over text full of format strings, and a guard that can be fooled by a
    /// brace in a string literal is not a guard. `crate::clock::now()` in a test
    /// behaves identically to `Timestamp::now()` unless the pin is set, in which
    /// case the test gets the pinned instant the rest of the process is using.
    ///
    /// Comment lines are skipped so prose may keep naming the call it is talking
    /// about (`render::task_table`'s doc comment does, and should).
    /// `Instant::now` is untouched: it is a monotonic stopwatch, not a clock.
    #[test]
    fn no_other_file_in_the_cli_reads_the_wall_clock() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&src, &mut files);
        assert!(
            files.len() > 20,
            "only {} sources found under {} — the walk is broken, not the crate",
            files.len(),
            src.display()
        );

        let mut offenders = Vec::new();
        for file in &files {
            let text = std::fs::read_to_string(file)
                .unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains("Timestamp::now") || line.contains("Zoned::now") {
                    let name = file.strip_prefix(&src).unwrap_or(file);
                    offenders.push(format!("{}:{}: {}", name.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the wall clock is read outside src/clock.rs, so TASQX_NOW cannot pin \
             it and a captured screen drifts by the day — call `crate::clock::now()` \
             instead:\n{}",
            offenders.join("\n")
        );
    }
}
