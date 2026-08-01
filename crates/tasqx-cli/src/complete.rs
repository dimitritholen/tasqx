//! Shell completion: the `$COMPLETE` callback path.
//!
//! The shell calls back into this binary on every Tab press. [`intercept`] is
//! the first statement of [`crate::run`], ahead of the argv pre-pass, so that a
//! completion request never reaches the dispatcher, never opens the ordinary
//! read-write store, and never runs a command. Without `$COMPLETE` set it is one
//! environment lookup and a return, which is what keeps every real invocation
//! byte-identical to a build without this module.
//!
//! # Failure policy: D33 is INVERTED here, deliberately
//!
//! On this path, **every** failure produces zero candidates, exit 0, and nothing
//! on stderr. A missing store, an unreachable daemon, a locked database, a
//! lookup that blows its time budget — all of them are silence.
//!
//! "On this path" is load-bearing and is not a figure of speech. The silence
//! applies once we know a shell is asking; deciding *that* is
//! [`names_a_shell_clap_can_complete`], and getting it wrong costs a real
//! command. An unrecognised `$COMPLETE` is explicitly NOT in the list above: it
//! cannot be a callback, so it is an ordinary run and it executes normally.
//!
//! That is the exact opposite of the rule the rest of this codebase is built on.
//! D33 says a value that changes nothing must not answer `ok`, and the
//! silent-drop class — a command that quietly does less than it was asked and
//! reports success — is the recurring defect this project names and hunts. Nine
//! instances of it have been fixed here. So a reader arriving at this file with
//! that history in mind will read the silence as the tenth, and "fix" it.
//!
//! It is not the tenth, and the reason is the medium rather than the code.
//! This path does not run when a user asks for something; it runs when a user
//! presses Tab in the middle of typing. At that moment the terminal is drawing a
//! command line, and:
//!
//!  * **stderr corrupts it.** Anything written while the shell is composing
//!    completions lands in the middle of the line the user is looking at. The
//!    shell does not redraw it. The user is left with a mangled prompt and a
//!    half-typed command, and the "error" they were shown is that their store
//!    happens to be locked by another process.
//!  * **a non-zero exit makes the shell beep** — or unset `COMPREPLY`, or print
//!    its own diagnostic — at somebody who was only typing. The bash
//!    registration `clap_complete` emits does `if [[ $? != 0 ]]; then unset
//!    COMPREPLY; fi`, so a non-zero exit is already indistinguishable from "no
//!    candidates" to the shell; all the error achieves is the noise.
//!  * **there is nothing to report anyway.** The user did not ask a question, so
//!    there is no answer being withheld. Offering no candidates is a complete and
//!    honest response to "what could come next here": we do not know.
//!
//! So the inversion is scoped precisely to this path and to nothing else. The
//! `completions` verb, which a human runs deliberately, is an ordinary command
//! and reports its errors loudly with a non-zero exit like every other verb.
//! Everything reached through [`crate::run`]'s normal route keeps D33 intact —
//! `tests/completion.rs` asserts that an unknown flag is still a stderr message
//! and a non-zero exit, so the silence cannot leak sideways unnoticed.
//!
//! # What was verified against `clap_complete` 4.6.7 before building on it
//!
//! Two upstream facts this module depends on, checked in the vendored source
//! rather than assumed, because the dependency is pinned against exactly the
//! kind of change that would invalidate them:
//!
//!  * `CompleteEnv::try_complete(args, current_dir)` takes CALLER-SUPPLIED args
//!    (`env/mod.rs`); only `complete()` reads `std::env::args_os()` itself. That
//!    is what makes the pre-pass fix below possible at all.
//!  * `try_complete_` removes `args[0]` as the completer path, then drains
//!    everything up to and including the first `--`, and selects the
//!    registration branch by what remains being EMPTY. So the pre-pass must be
//!    applied to the tail after `--` and nowhere else — see [`prepassed`].
//!
//! `complete()` itself is unusable here: it ends in `Error::exit`, which prints
//! a clap error to stderr and exits non-zero for, among other things, a
//! `$COMPLETE` naming a shell it has no completer for. That is the policy above
//! violated by the convenience wrapper, so [`intercept`] uses `try_complete`.
//!
//! # The one accepted edge: a RECOGNISED `$COMPLETE` and no `--`
//!
//! `COMPLETE=bash tasqx add "a real task"` does not add a task. It prints the
//! bash registration script and exits 0.
//!
//! That follows from the third fact above rather than from anything this module
//! chooses: `try_complete_` drains argv through the first `--`, and "what is
//! left is empty" is *the* signal it uses to mean "emit the registration". With
//! no `--` anywhere, everything drains, the remainder is empty, and the
//! registration branch is taken no matter what the words were. It is
//! `clap_complete`'s protocol — `source <(COMPLETE=bash tasqx)` is the
//! documented activation line and is exactly this call — so it stays.
//!
//! It is recorded here because it is the same *shape* as the defect
//! [`names_a_shell_clap_can_complete`] fixes and must not be mistaken for it.
//! Two things separate them. It requires a `$COMPLETE` naming a shell that is
//! genuinely installed and set for this process, which is a state a shell only
//! reaches while actually completing. And it is LOUD: a page of shell script on
//! stdout is impossible to mistake for a command that ran. The bug that was
//! fixed was silent, exit 0, and reachable by a user simply trying `nushell`.

use std::ffi::{OsStr, OsString};

use clap::CommandFactory;
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use clap_complete::env::Shells;
use clap_complete::CompleteEnv;

use crate::Cli;

/// The environment variable that turns this path on.
///
/// Named here and handed to `CompleteEnv::var` rather than relying on its
/// default, so the cheap guard at the top of [`intercept`] and the machinery it
/// guards can never disagree about which variable they are reading.
const COMPLETE_VAR: &str = "COMPLETE";

/// Serve a completion request and exit, or return so the ordinary run proceeds.
///
/// Must stay the FIRST statement of [`crate::run`]. Two reasons, and only the
/// second is obvious: `clap_complete` warns that stdout must not be written to
/// before it runs, and — the one that matters more here — this has to happen
/// before [`crate::argv::prepass`] so the completion words get their own
/// pre-pass rather than the command line's.
pub(crate) fn intercept() {
    // The whole cost of this feature on every ordinary `tasqx` invocation. It
    // is not merely an optimisation: returning here means argv is never even
    // collected on the command path, so "behaves exactly as before without
    // $COMPLETE" is a property of the control flow rather than of the code
    // downstream being careful.
    let Some(shell) = std::env::var_os(COMPLETE_VAR) else {
        return;
    };

    // An unrecognised value is not a completion request, and this is the whole
    // discriminator. See [`names_a_shell_clap_can_complete`] for why that
    // implication holds; the consequence is that we return and the ordinary
    // command RUNS, exactly as it would with the variable unset.
    if !names_a_shell_clap_can_complete(&shell) {
        return;
    }

    let raw: Vec<OsString> = std::env::args_os().collect();
    let cwd = std::env::current_dir().ok();
    let result = CompleteEnv::with_factory(Cli::command)
        .var(COMPLETE_VAR)
        // Explicit rather than left to `get_bin_name().or(get_name())`. Both
        // fallbacks happen to yield "tasqx" today, but which one fires depends
        // on whether clap has run its bin-name pass, and the string this
        // produces is what the shell registers `complete -F ... <bin>` against.
        // A registration bound to the wrong name completes nothing, silently.
        .bin("tasqx")
        .try_complete(prepassed(raw), cwd.as_deref());

    match result {
        // Candidates (or a registration script) were written. Exiting here is
        // what keeps the completion path from ever reaching the dispatcher.
        Ok(true) => std::process::exit(0),
        // `$COMPLETE` unset, empty, or "0". Unreachable in practice: the guard
        // above already returned for all three (none of them names a shell), so
        // this arm agrees with it rather than second-guessing it.
        Ok(false) => {}
        // The shell IS one clap completes, so this is a genuine completion run
        // that failed late — `try_complete_` has passed its shell lookup and the
        // only remaining faults are I/O errors writing the candidates or the
        // registration to stdout. The failure policy above applies in full:
        // nothing on stderr, exit 0. Falling through instead would hand the
        // dispatcher a completion argv (`tasqx -- tasqx list -ne`) and run it as
        // a command, which is the worse of the two failures by a distance.
        Err(_) => std::process::exit(0),
    }
}

/// Does `$COMPLETE`'s value name a shell `clap_complete` has a completer for?
///
/// This is the discriminator between "a shell is asking for candidates" and "a
/// human is running a command with a stale variable in the environment", and it
/// replaces an earlier test that asked whether argv contained `--`.
///
/// **Why recognisability decides it.** A completion callback is only ever
/// launched by a registration script, and the only thing that emits a
/// registration script is `clap_complete` itself — for a shell it has an
/// `EnvCompleter` for. `COMPLETE=nushell tasqx` does not print a script, it
/// errors, so no `nushell` registration has ever existed anywhere and nothing
/// can have invoked us on its behalf. An unrecognised value therefore cannot
/// possibly be a callback. It can only have been exported by hand, left behind
/// by an experiment, or typed by a user trying the shell this project documents
/// as a known gap. Every one of those is somebody running a real command, and a
/// real command must RUN.
///
/// **Why the `--` test it replaces was wrong.** `--` is not a completion marker.
/// It is the documented, POSIX way to pass a value that begins with a dash, and
/// tasqx tells users to reach for it: `tasqx add -- "-leading dash title"`. So
/// the old check classified an ordinary command as a callback and the error arm
/// swallowed it — measured, with a real store: `COMPLETE=nushell tasqx add --
/// "a real task"` printed nothing, exited 0, and never added the task. That is
/// precisely the silent-drop class this codebase hunts, committed by the code
/// whose comment claimed to be preventing it.
///
/// Read out of `Shells::builtins()` — clap's own registry, the same one
/// `CompleteEnv` consults, since [`intercept`] never calls `.shells()` to
/// override the default. A hand-kept list of shell names here would be the drift
/// shape this repository keeps paying for, and it would drift the day upstream
/// adds a shell.
///
/// `file_stem` mirrors `Shells::completer_for_path`, which strips a directory so
/// that a `$COMPLETE` copied from `$SHELL` (`/usr/bin/zsh`) still resolves. The
/// two must agree: a value this function accepted but `try_complete` then
/// rejected would take the late-error arm above and exit 0 on a real command —
/// the very bug being fixed, reintroduced one layer down.
fn names_a_shell_clap_can_complete(value: &OsStr) -> bool {
    let stem = std::path::Path::new(value)
        .file_stem()
        .unwrap_or(value)
        .to_string_lossy()
        .into_owned();
    Shells::builtins().completer(&stem).is_some()
}

/// Run the completion words through [`crate::argv::prepass`], leaving the
/// callback's own framing alone.
///
/// The words a shell hands a completer are RAW ARGV, and tasqx cannot parse raw
/// argv: `-needs` is valid filter grammar and looks exactly like a flag, which
/// is why [`crate::run`] does not call `Cli::parse()` at all. `clap_complete`'s
/// engine parses the words it is given against the same `clap::Command`, so it
/// inherits the same problem and needs the same fix. Without this,
/// `tasqx list -needs<TAB>` misbehaves in a place no other guard looks, because
/// every existing test exercises the parse path and not the completion path.
///
/// The pre-pass is applied to the tail AFTER `--` and to nothing else, which is
/// forced by how `try_complete_` reads its input: it drops `args[0]`, drains
/// through the first `--`, and treats an empty remainder as "emit the
/// registration". Pre-passing the whole vector would rewrite the sentinel's own
/// neighbourhood, and pre-passing an invocation that has no `--` would disturb
/// the registration branch — which is selected precisely by that emptiness.
///
/// The tail is a complete argv in its own right: the shell passes the command
/// line starting at the program name (bash's `words=("${COMP_WORDS[@]}")`), and
/// [`crate::argv::prepass`] expects exactly that shape, scanning from index 1.
///
/// # The word being completed is escaped too, and that is deliberate
///
/// The obvious reading of this function is that escaping the *partial* word is
/// an accident — the pre-pass exists to keep clap's PARSE honest, and the engine
/// never parses the current word (`complete()` returns the moment its cursor
/// reaches the target, so words at and after it are never classified). Leaving
/// it raw was tried and MEASURED, against the real engine with a provider
/// attached to `list`'s filter positional:
///
/// ```text
///   current word escaped (this code)   raw (the "obvious fix")
///   -ne      -> []                     -> [-needs, -neh, -neV]
///   -needs   -> []                     -> [-needs, -needsh, -needsV]
/// ```
///
/// Both columns are wrong, in opposite directions. Raw reaches the provider but
/// drags in junk: `complete_arg` calls `complete_option` for the current word
/// whenever no `--` has been seen, `complete_option` sends anything matching
/// `-x…` down its short-flag branch, and `parse_shortflags` does not check that
/// the letters name real flags — it just prefixes every short the command
/// declares, so `-needs` grows `-needsh` and `-needsV`. Neither is a flag and
/// neither is a tag exclusion; they are unusable strings.
///
/// The escape is what makes that branch unreachable, and it is not a trick: it
/// states tasqx's grammar to the engine. A single-dash token longer than one
/// character IS a tag exclusion and can never be a flag (`argv.rs`), so flags
/// are not candidates for it, and the sentinel is how the engine is told.
///
/// So the escape stays and the missing half is the RESTORE. The command path
/// already has one — `run()` calls [`crate::argv::unescape`] on the parsed
/// filter tail before any value is used — and the Tab path needs the same thing
/// at its own boundary: [`escaped_word_completer`], which restores the dash
/// before a candidate provider ever sees the word. Escape and restore are
/// symmetric on both paths or they are broken on one; this is the second half of
/// that pair, and its absence is what made a provider on a filter positional
/// return nothing.
fn prepassed(raw: Vec<OsString>) -> Vec<OsString> {
    let Some(sep) = raw.iter().position(|a| a == "--") else {
        return raw;
    };
    let mut out: Vec<OsString> = raw[..=sep].to_vec();
    out.extend(crate::argv::prepass(raw[sep + 1..].iter().cloned()).argv);
    out
}

/// Wrap a candidate provider for a positional whose words [`prepassed`] escapes,
/// restoring the leading dash before the provider sees the partial word.
///
/// **Every completer on a filter or capture-sugar positional must be built with
/// this**, and the drift guard below fails the build for one that is not. The
/// reason is [`prepassed`]'s: by the time the engine asks "what can follow
/// `-ne`?", the word is `\u{1}ne`, and a provider handed that matches nothing.
/// It is the completion-path twin of [`crate::argv::unescape`].
///
/// Deliberately an `ArgValueCompleter` rather than an `ArgValueCandidates`, and
/// the two are not interchangeable here. `ArgValueCandidates` yields a fixed list
/// that the ENGINE then prefix-filters (`complete_custom_arg_value`:
/// `retain(|c| c.starts_with(value))`) against the word as the engine holds it —
/// still escaped, and nothing this seam does can reach that filter, because it
/// runs after the provider has returned. `ArgValueCompleter` is handed the word
/// and filters itself, which is the only shape where restoring the dash first
/// can have any effect at all.
///
/// That difference is invisible at the call site and fails in exactly the
/// direction this codebase keeps getting hurt by: `+home` and `project:x` would
/// work (no escape applies, nothing starts with a dash) while every tag
/// EXCLUSION quietly returned nothing. A completer that serves four of five
/// prefixes is worse than one that serves none, because it looks finished.
// Dead until Tasks 6-7 attach the sugar and filter providers, in the shape
// `theme.rs`'s `FileOutcome` uses: landed with the defect it fixes rather than
// held back, because the tests below are what prove the two properties hold, and
// they can only prove them against a real seam. The `allow` comes off with the
// first live attachment.
#[allow(dead_code)]
pub(crate) fn escaped_word_completer<F>(candidates: F) -> ArgValueCompleter
where
    F: Fn(&str) -> Vec<CompletionCandidate> + Send + Sync + 'static,
{
    ArgValueCompleter::new(move |current: &OsStr| {
        candidates(&crate::argv::unescaped(&current.to_string_lossy()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap_complete::engine::ArgValueCandidates;

    /// Drive the REAL engine over the REAL command tree, through [`prepassed`],
    /// with a test-only provider on `list`'s filter positional.
    ///
    /// Test-only because Task 7 has not attached the live one yet, and the
    /// property under test belongs to the seam rather than to whichever
    /// candidates the provider eventually returns. `words` is the command line
    /// the shell passes (program name included) and `index` is the cursor word,
    /// both exactly as `clap_complete`'s registrations supply them.
    fn candidates_for(words: &[&str], index: usize) -> Vec<String> {
        let mut cmd = Cli::command().mut_subcommand("list", |sc| {
            sc.mut_arg("filter", |a| {
                a.add(escaped_word_completer(|current| {
                    ["-needs", "+home", "project:work"]
                        .iter()
                        .filter(|c| c.starts_with(current))
                        .map(|c| CompletionCandidate::new(*c))
                        .collect()
                }))
            })
        });

        // The framing `try_complete_` strips before the engine sees anything:
        // argv[0] is the completer path and everything through the first `--` is
        // the callback's own envelope. Reproduced rather than hand-writing the
        // tail, so the test exercises `prepassed` on the shape it really gets.
        let mut raw: Vec<OsString> = vec![OsString::from("tasqx"), OsString::from("--")];
        raw.extend(words.iter().map(OsString::from));
        let tail = prepassed(raw).split_off(2);

        clap_complete::engine::complete(&mut cmd, tail, index, None)
            .expect("the engine must not fail")
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
    }

    /// The defect: a partial tag exclusion reached the provider as `\u{1}ne`, so
    /// nothing matched and Tab produced an empty list. Task 7 attaches the live
    /// filter provider to this exact positional and would have inherited it.
    ///
    /// Asserted at every length, because the old behaviour was length-dependent
    /// and so looked like it worked: the pre-pass only escapes tokens longer than
    /// one character, so a bare `-` matched and a two-character `-ne` did not.
    #[test]
    fn a_partial_tag_exclusion_reaches_the_provider_with_its_dash_intact() {
        for typed in ["-", "-n", "-ne", "-need", "-needs"] {
            let got = candidates_for(&["tasqx", "list", typed], 2);
            assert!(
                got.iter().any(|c| c == "-needs"),
                "`list {typed}<TAB>` must reach the filter provider with {typed:?} \
                 as the prefix and offer `-needs`, got {got:?}"
            );
        }
    }

    /// The other half, and the one the obvious fix breaks: restoring the dash for
    /// the PROVIDER must not restore it for the ENGINE, which would send the word
    /// down `complete_option`'s short-flag branch and grow it into strings that
    /// are neither flags nor filter tokens (`-needsh`, `-needsV`).
    #[test]
    fn a_partial_tag_exclusion_offers_no_short_flag_junk() {
        for typed in ["-n", "-ne", "-need", "-needs"] {
            for got in candidates_for(&["tasqx", "list", typed], 2) {
                assert!(
                    got == "-needs" || !got.starts_with(typed),
                    "`list {typed}<TAB>` grew {got:?} out of clap's short flags; \
                     the word reached the engine unescaped"
                );
            }
        }
    }

    /// The prefixes that never carried a dash must keep working unchanged — the
    /// restore is a no-op for them, and a seam that only handled the dash case
    /// would be a second code path to get wrong.
    #[test]
    fn the_other_sugar_prefixes_are_untouched_by_the_restore() {
        assert_eq!(candidates_for(&["tasqx", "list", "+"], 2), ["+home"]);
        assert_eq!(
            candidates_for(&["tasqx", "list", "project:"], 2),
            ["project:work"]
        );
        // A bare word is not completable and must stay that way; offering the
        // whole vocabulary for a title word would be noise, not help.
        assert!(candidates_for(&["tasqx", "list", "zz"], 2).is_empty());
        // And clap's own flags still reach the user in filter position.
        assert!(candidates_for(&["tasqx", "list", ""], 2)
            .iter()
            .any(|c| c == "--json"));
    }

    /// Drift guard, read out of clap's own arg table rather than a list kept
    /// here: no positional that [`crate::argv::prepass`] can escape into may
    /// carry a bare `ArgValueCandidates`.
    ///
    /// That combination compiles, reads correctly, and silently returns nothing
    /// for every tag exclusion, because the engine prefix-filters its output
    /// against the still-escaped word (see [`escaped_word_completer`]). It is the
    /// silent-drop shape, so it is a build failure rather than a comment. The
    /// commands come from the same set `argv` derives its escaping from, so a
    /// filter command added tomorrow is covered the day it is declared.
    #[test]
    fn no_escapable_positional_uses_an_engine_filtered_provider() {
        let mut cmd = Cli::command();
        cmd.build();
        let mut checked = 0;
        for sc in cmd.get_subcommands() {
            if !crate::argv::FILTER_COMMANDS.contains(&sc.get_name()) {
                continue;
            }
            for pos in sc.get_positionals() {
                checked += 1;
                assert!(
                    pos.get::<ArgValueCandidates>().is_none(),
                    "`{} {}` is a positional the pre-pass escapes into, so an \
                     ArgValueCandidates on it is prefix-filtered against the \
                     sentinel and returns nothing for every `-tag`. Build it with \
                     `complete::escaped_word_completer` instead.",
                    sc.get_name(),
                    pos.get_id()
                );
            }
        }
        assert!(
            checked >= crate::argv::FILTER_COMMANDS.len(),
            "every filter command declares a positional; the guard matched {checked} \
             and would otherwise be vacuous"
        );
    }
}
