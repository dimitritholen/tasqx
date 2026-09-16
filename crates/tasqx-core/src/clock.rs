//! The engine's one wall-clock read, and the pin that makes a run repeatable.
//!
//! Every "what time is it" in `tasqx-core` comes through [`now`] — the stamps
//! `util::now` writes, the instant urgency is scored at, the reference a
//! relative filter resolves against, the daemon's event and reminder clocks. A
//! guard test at the bottom of this file reads every `.rs` file under `src/`
//! and fails on a `Timestamp::now` or `Zoned::now` anywhere else, because the
//! value of a single door is exactly that nothing walks around it.
//!
//! # `TASQX_NOW`
//!
//! Set `TASQX_NOW` to an RFC 3339 instant (`2026-09-16T09:00:00Z`, or any
//! offset spelling jiff accepts) and [`now`] answers that instead of the clock.
//! It exists so the documentation pipeline can render its screens reproducibly:
//! a captured `list`, `agenda`, `why`, `chart` or write echo prints `due
//! tomorrow`, `3d`, `created today`, a burndown window and an urgency score, so
//! the same store rendered on two days produces two different files and the
//! drift job that keeps the pictures honest would fire on every calendar day
//! rather than on a real change.
//!
//! **The pin reaches the store, on purpose.** A capture that writes — `add`,
//! `start`, `done`, `annotate`, all of which the manifest captures — stamps
//! `created`, `completed` and `active_since` through `util::now`. Left on the
//! wall clock while the render stands on a pinned day in the past, a task added
//! during capture is spelled "created in 15 days" and its `tracked` interval
//! runs backwards. Half a pin is not a pin.
//!
//! **Which makes it a hazard, so it is validated and visible.** A stray
//! `TASQX_NOW` in somebody's shell would backdate real work. Three things stand
//! against that: the CLI validates the variable at its own door and exits 2 on
//! anything unparsable (`tasqx-cli/src/clock.rs`) before an engine exists;
//! `tasqx about` states the pin beside the store and the build, so the tool
//! that answers "which store am I on" answers "which clock" too; and the
//! variable is documented only as a capture and testing hook — in this module,
//! in `CONTRIBUTING.md` and in DESIGN.md D148 — never as a setting or on a user
//! page.
//!
//! Here an unparsable value falls back to the wall clock rather than being an
//! error. A library cannot exit a process it does not own, and every process
//! that reaches this engine — the one-shot CLI, `api`, `mcp serve`, `daemon`,
//! `watch` — enters through `tasqx-cli`, which refuses the bad value first. The
//! fallback is the unreachable branch's safest answer, not a second policy.

use jiff::Timestamp;

/// The reference instant: `TASQX_NOW` when it is set and parsable, the wall
/// clock otherwise.
///
/// The environment is read on every call rather than once into a `OnceLock`: a
/// long-lived daemon reads the clock per event, and a cached wall-clock reading
/// would stamp every event of a day-long process with the second it started.
pub fn now() -> Timestamp {
    match pin_from_env() {
        Ok(Some(pin)) => pin,
        // The one wall-clock read in the workspace.
        _ => Timestamp::now(),
    }
}

/// `TASQX_NOW` as this process sees it — the validation door the CLI calls
/// before it trusts [`now`].
///
/// `Ok(None)` means "no pin, use the clock".
pub fn pin_from_env() -> Result<Option<Timestamp>, String> {
    pinned(std::env::var("TASQX_NOW").ok().as_deref())
}

/// The parse behind [`now`], split out because the environment is
/// process-global: a test that set `TASQX_NOW` would be setting it for every
/// other test in the same binary, which run in parallel threads.
///
/// An unset variable and an empty or blank one are the same answer —
/// `TASQX_NOW=` is how a script clears it, and `$TASQX_DB` already treats empty
/// as unset — so the error is reserved for a value somebody meant.
pub fn pinned(raw: Option<&str>) -> Result<Option<Timestamp>, String> {
    let Some(value) = raw else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.parse::<Timestamp>() {
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

    /// The error text is the CLI's to print — it exits 2 on it — so it names
    /// the variable and quotes the value it could not read.
    #[test]
    fn an_unparsable_pin_is_an_error_that_quotes_the_value() {
        for bad in ["tomorrow", "2026-09-16", "2026-09-16 09:00:00", "now"] {
            assert_eq!(
                pinned(Some(bad)).unwrap_err(),
                format!("tasqx: TASQX_NOW is not an RFC 3339 instant: {bad}"),
                "{bad:?}"
            );
        }
    }

    /// Every `.rs` file under `crates/tasqx-core/src`, this one excluded.
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
    /// The CLI's own guard (`tasqx-cli/src/clock.rs`) says the same thing about
    /// the renderers; this one covers what they render. A single
    /// `Timestamp::now()` left in the engine stamps one row, scores one task or
    /// bounds one query against the real day while everything beside it stands
    /// on the pinned one — drift in a single field, which a reader takes for a
    /// real change rather than for a broken capture.
    ///
    /// Test code is held to the same rule rather than exempted: deciding
    /// whether a line sits inside a `#[cfg(test)]` block means balancing braces
    /// over text full of format strings, and a guard that can be fooled by a
    /// brace in a string literal is not a guard. `crate::clock::now()` in a test
    /// behaves identically unless the pin is set, in which case the test gets
    /// the same instant the code under test does — which is what the fixtures
    /// in `daemon.rs` and `attribution.rs` actually want.
    ///
    /// Comment lines are skipped so prose may keep naming the call it is
    /// talking about (`filter.rs` and `markdown.rs` both do, and should). A
    /// test helper that returns a fixed literal instant — `datetime.rs`'s and
    /// `remind.rs`'s `fn now()` — is not a clock read and is left alone.
    #[test]
    fn no_other_file_in_the_engine_reads_the_wall_clock() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&src, &mut files);
        assert!(
            files.len() > 15,
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
             it and a captured run drifts by the day — call `crate::clock::now()` \
             instead:\n{}",
            offenders.join("\n")
        );
    }
}
