//! The engine's one wall-clock read, and the pin that makes a run repeatable.
//!
//! Every "what time is it" in `tasqx-core` comes through [`now`] — the stamps
//! `util::now` writes, the instant urgency is scored at, the reference a
//! relative filter resolves against, the daemon's event and reminder clocks —
//! and every time-ordered id through [`uuid_v7`], because a UUIDv7 is a clock
//! read wearing an id's clothes. A guard test at the bottom of this file reads
//! every `.rs` file under `src/` and fails on a `Timestamp::now`, `Zoned::now`
//! or `now_v7(` anywhere else, because the value of a single door is exactly
//! that nothing walks around it.
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
//! `TASQX_NOW` in somebody's shell would backdate real work. Four things stand
//! against that: the CLI validates the variable once at the top of `run`, before
//! argv is parsed and before any store is opened, and exits 2 on anything
//! unparsable (`tasqx-cli/src/clock.rs`); `tasqx about` states the pin beside
//! the store and the build, so the tool that answers "which store am I on"
//! answers "which clock" too; a pinned process never talks to a daemon, and
//! `tasqx daemon`/`tasqx watch` refuse to start pinned, so one instant can never
//! be half of a two-process command; and the variable is documented only as a
//! capture and testing hook — in this module, in `CONTRIBUTING.md` and in
//! DESIGN.md D148 — never as a setting or on a user page.
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

/// A UUIDv7 whose embedded time is [`now`]'s — the id half of every row this
/// engine mints, and the ONLY way to mint one.
///
/// **A v7 id is a clock read.** `Uuid::now_v7()` takes the wall clock, so under
/// a pin an event's `id` and its `ts` came from two different clocks, and the
/// two disagree by however far the pin is from today. `event_id_floor` derives
/// the `event.list {from}` bound from a `ts` instant and compares it against
/// `id`, on the D59 rule that `id` is the only column that can carry an order
/// — so a pin ahead of real time puts every freshly written event BELOW the
/// floor, and a bounded read drops rows that exist: history, charts and the
/// exports built on that bound all quietly lose the newest events. The
/// one-second margin `event_id_floor` carries is sized for a millisecond tick
/// between two reads, not for a clock disagreement measured in months.
///
/// Unpinned, this is `Uuid::now_v7()` exactly — same call, same process-global
/// context, so nothing about ordinary ids changes.
pub fn uuid_v7() -> uuid::Uuid {
    match pin_from_env() {
        Ok(Some(pin)) => v7_at(pin),
        _ => uuid::Uuid::now_v7(),
    }
}

/// [`uuid_v7`] at a given instant — the pinned half, split out because the
/// environment is process-global and a test that set it would set it for every
/// other test in the binary.
///
/// The counter comes from a shared [`uuid::ContextV7`] of our own, which is what
/// keeps a burst of ids monotonic within the process. That matters more under a
/// pin than without one: D142 orders annotations and measurements by `id` alone,
/// and with the clock frozen EVERY id in the run falls inside one millisecond,
/// so the counter is the whole order. The remaining bits stay random, as `uuid`
/// fills them.
fn v7_at(pin: Timestamp) -> uuid::Uuid {
    // `Mutex<ContextV7>` is what `uuid`'s own `now_v7` uses behind
    // `shared_context_v7`, which is `pub(crate)` there; this is the same shape,
    // owned here. `ContextV7::new` is `const`, so the static needs no lazy.
    static PINNED: std::sync::Mutex<uuid::ContextV7> =
        std::sync::Mutex::new(uuid::ContextV7::new());
    // Saturating rather than panicking: a pin before 1970 is nonsense, but
    // minting an id is not where nonsense should abort a write.
    let seconds = u64::try_from(pin.as_second()).unwrap_or(0);
    let subsec_nanos = u32::try_from(pin.subsec_nanosecond()).unwrap_or(0);
    uuid::Uuid::new_v7(uuid::Timestamp::from_unix(&PINNED, seconds, subsec_nanos))
}

/// `TASQX_NOW` as this process sees it — the validation door the CLI calls
/// before it trusts [`now`].
///
/// `Ok(None)` means "no pin, use the clock", and only an ABSENT variable means
/// that. A value that is not valid Unicode is an error rather than a shrug:
/// `std::env::var(..).ok()` folds `VarError::NotUnicode` into `None`, so a pin
/// mistyped into a byte string fell through to the wall clock with nothing
/// said — the one outcome this door exists to prevent, and on Unix any byte
/// sequence can reach it. The message is the same one an unparsable value gets,
/// because to a reader they are the same mistake; the value is spelled back
/// lossily, since a message about unreadable bytes has to print them somehow.
pub fn pin_from_env() -> Result<Option<Timestamp>, String> {
    match std::env::var("TASQX_NOW") {
        Ok(value) => pinned(Some(&value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(raw)) => Err(format!(
            "tasqx: TASQX_NOW is not an RFC 3339 instant: {}",
            raw.to_string_lossy()
        )),
    }
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

    /// A pinned id carries the PIN's millisecond, and a burst of them still
    /// climbs.
    ///
    /// Both halves matter and they pull against each other. The first is the
    /// fix: `event_id_floor` derives the `event.list {from}` bound from a `ts`
    /// instant and compares it to `id`, so an id minted off the wall clock while
    /// `ts` follows a future pin sorts below its own window and the row vanishes
    /// from a bounded read. The second is what a naive fix would have broken:
    /// with the clock frozen, every id in the run shares one millisecond, so
    /// without a counter their order would be the order of their random bits —
    /// and D142 orders annotations and measurements by `id` alone.
    #[test]
    fn a_pinned_id_carries_the_pin_and_a_burst_of_them_still_climbs() {
        let pin: Timestamp = "2030-01-01T00:00:00Z".parse().unwrap();
        let ids: Vec<String> = (0..64).map(|_| v7_at(pin).to_string()).collect();

        // The first 48 bits of a v7 are the Unix milliseconds, hex, MSB first.
        let want = format!("{:012x}", pin.as_millisecond());
        let head = |id: &str| id[..8].to_string() + &id[9..13];
        assert_eq!(head(&ids[0]), want, "{} does not carry the pin", ids[0]);

        for pair in ids.windows(2) {
            assert!(
                pair[0] < pair[1],
                "ids minted inside one pinned millisecond are not ordered: {pair:?}"
            );
        }
        assert_eq!(head(ids.last().unwrap()), want, "the 64th id drifted");
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
    /// `Uuid::now_v7(` counts as a clock read and is caught by the same list,
    /// because a v7 id IS a timestamp: minting one from the wall clock under a
    /// pin puts an event's `id` and its `ts` on different clocks, and
    /// `event_id_floor` compares the two. [`uuid_v7`] is the only mint.
    ///
    /// Comment lines are skipped so prose may keep naming the call it is
    /// talking about (`filter.rs`, `markdown.rs` and `storage.rs` all do, and
    /// should). A test helper that returns a fixed literal instant —
    /// `datetime.rs`'s and `remind.rs`'s `fn now()` — is not a clock read and
    /// is left alone.
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
                for needle in ["Timestamp::now", "Zoned::now", "now_v7("] {
                    if line.contains(needle) {
                        let name = file.strip_prefix(&src).unwrap_or(file);
                        offenders.push(format!("{}:{}: {}", name.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the wall clock is read outside src/clock.rs, so TASQX_NOW cannot pin \
             it and a captured run drifts by the day — call `crate::clock::now()` \
             or `crate::clock::uuid_v7()` instead:\n{}",
            offenders.join("\n")
        );
    }
}
