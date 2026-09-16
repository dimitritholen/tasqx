//! The CLI's one wall-clock read, and the pin that makes a render repeatable.
//!
//! Every "what time is it" in `tasqx-cli` comes through [`now`]. A guard test at
//! the bottom of this file reads every `.rs` file under `src/` and fails on a
//! `Timestamp::now` or `Zoned::now` anywhere else, because the value of a single
//! door is exactly that nothing walks around it.
//!
//! # `TASQX_NOW`
//!
//! Set `TASQX_NOW` to an RFC 3339 instant (`2026-09-16T09:00:00Z`, or any offset
//! spelling jiff accepts) and [`now`] answers that instead of the clock. It
//! exists so `scripts/docs-capture.sh` can render the documentation screens
//! reproducibly: a captured `list`, `agenda`, `why` or `chart` prints `due
//! tomorrow`, `3d`, a burndown window and a relative annotation age, so the same
//! store rendered on two different days produces two different files and a drift
//! job that compares them would fire on every calendar day rather than on a real
//! change. `scripts/demo-store.py` reads the same variable, so the store's dates
//! and the render's reference instant come from one pinned day.
//!
//! It is a testing and capture hook, not a user feature: it is documented here
//! and in `CONTRIBUTING.md`, and deliberately not in `tasqx docs`, the wiki or
//! the guides. Nobody tracking real work wants their overdue tasks frozen.
//!
//! An unparsable value is fatal rather than ignored. A typo that silently fell
//! back to the wall clock would produce exactly the drift the pin was set to
//! prevent, and the capture would look like it worked.

/// The reference instant: `TASQX_NOW` when it is set, the wall clock otherwise.
///
/// Exits 2 — the CLI's `bad_request` code (DESIGN.md §4) — with one line on
/// stderr when `TASQX_NOW` holds something that is not an RFC 3339 instant.
///
/// The environment is read on every call rather than once into a `OnceLock`:
/// the dashboard re-reads the clock on every tick and a cached wall-clock
/// reading would freeze a live screen.
pub fn now() -> jiff::Timestamp {
    match pinned(std::env::var("TASQX_NOW").ok().as_deref()) {
        Ok(Some(pin)) => pin,
        // The one wall-clock read in the crate.
        Ok(None) => jiff::Timestamp::now(),
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    }
}

/// The parse behind [`now`], split out because the environment is
/// process-global: a test that set `TASQX_NOW` would be setting it for every
/// other test in the same binary, which run in parallel threads.
///
/// `Ok(None)` means "no pin, use the clock". An unset variable and an empty or
/// blank one are the same answer — `TASQX_NOW=` is how a script clears it, and
/// `$TASQX_DB` already treats empty as unset (`backend::db_path`) — so the
/// error is reserved for a value somebody meant.
fn pinned(raw: Option<&str>) -> Result<Option<jiff::Timestamp>, String> {
    let Some(value) = raw else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.parse::<jiff::Timestamp>() {
        Ok(ts) => Ok(Some(ts)),
        Err(_) => Err(format!(
            "tasqx: TASQX_NOW is not an RFC 3339 instant: {value}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    #[test]
    fn an_unset_or_blank_pin_leaves_the_wall_clock_alone() {
        assert_eq!(pinned(None), Ok(None));
        assert_eq!(pinned(Some("")), Ok(None));
        assert_eq!(pinned(Some("   ")), Ok(None));
    }

    #[test]
    fn a_pinned_instant_is_the_instant_it_names() {
        let ts = pinned(Some("2026-09-16T09:00:00Z")).unwrap().unwrap();
        assert_eq!(ts.to_string(), "2026-09-16T09:00:00Z");
        // Whitespace comes free with `VAR=$(date ...)` in a capture script.
        assert_eq!(pinned(Some("  2026-09-16T09:00:00Z\n")).unwrap(), Some(ts));
        // An offset spelling names the same instant as its UTC one.
        assert_eq!(pinned(Some("2026-09-16T11:00:00+02:00")).unwrap(), Some(ts));
    }

    /// A typo must not fall back to the clock: a capture that drifts is the one
    /// failure the pin exists to remove, and a silent fallback hides it behind
    /// output that looks right.
    #[test]
    fn an_unparsable_pin_is_an_error_that_quotes_the_value() {
        for bad in ["tomorrow", "2026-09-16", "2026-09-16 09:00:00", "now"] {
            let err = pinned(Some(bad)).unwrap_err();
            assert_eq!(
                err,
                format!("tasqx: TASQX_NOW is not an RFC 3339 instant: {bad}"),
                "{bad:?}"
            );
        }
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
