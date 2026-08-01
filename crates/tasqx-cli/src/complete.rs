//! Shell completion: the `$TASQX_COMPLETE` callback path.
//!
//! The shell calls back into this binary on every Tab press. [`intercept`] is
//! the first statement of [`crate::run`], ahead of the argv pre-pass, so that a
//! completion request never reaches the dispatcher, never opens the ordinary
//! read-write store, and never runs a command. Without `$TASQX_COMPLETE` set it
//! is one environment lookup and a return, which is what keeps every real
//! invocation byte-identical to a build without this module.
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
//! command. An unrecognised `$TASQX_COMPLETE` is explicitly NOT in the list
//! above: it cannot be a callback, so it is an ordinary run and it executes
//! normally. What that check does *not* buy is the section below headed *The
//! residual hazard*: a silent drop this module makes improbable and cannot
//! remove. Read it before concluding that the silent-drop class is closed here.
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
//! `$TASQX_COMPLETE` naming a shell it has no completer for. That is the policy
//! above violated by the convenience wrapper, so [`intercept`] uses
//! `try_complete`.
//!
//! # The residual hazard: a RECOGNISED `$TASQX_COMPLETE` and a real command
//!
//! [`names_a_shell_clap_can_complete`] closes the *unrecognised* half of this
//! and nothing more. Once the variable names a shell clap does have a completer
//! for, the process is handed to `try_complete_` and what happens next is
//! decided by `clap_complete`'s protocol rather than by anything this module
//! chooses. There are two outcomes and neither runs the command. Both were
//! measured against the built binary, not reasoned about:
//!
//!  * **No `--` anywhere — the registration script, on stdout, exit 0.**
//!    `TASQX_COMPLETE=zsh tasqx done 1` prints `#compdef tasqx` and a page of
//!    zsh instead of closing task 1. This follows from the third fact above:
//!    `try_complete_` drains argv through the first `--` and "what is left is
//!    empty" is *the* signal it uses to mean "emit the registration", so with no
//!    `--` everything drains and that branch is taken whatever the words were.
//!    It is also the activation call itself (`source <(TASQX_COMPLETE=zsh
//!    tasqx)`), so it cannot be removed without removing the feature. It is at
//!    least LOUD: a page of shell script on stdout is not mistakable for a
//!    command that ran.
//!  * **A `--` present — nothing at all, exit 0, and the command is DROPPED.**
//!    `TASQX_COMPLETE=bash tasqx add -- "a real task"` writes zero bytes to
//!    stdout, zero bytes to stderr, exits 0, and does not add the task. Here
//!    `try_complete_` takes `shell.write_complete` instead, the engine completes
//!    the word `a real task` in `add`'s title position, finds no candidates, and
//!    prints the empty list. Nothing distinguishes that from a command that
//!    succeeded quietly.
//!
//! The second one is a silent drop — the class this codebase hunts — sitting in
//! the file whose whole argument is that it is not committing one. It is named
//! here rather than argued away, because the previous version of this section
//! described only the first case, called it "the one accepted edge", and
//! justified it as "LOUD"; both claims were false of the `--` spelling, and a
//! doc asserting a property the code lacks is the same defect shape as the bug
//! it was written to explain.
//!
//! ## Why there is no stronger discriminator
//!
//! The obvious repair is to demand more evidence that a shell is really asking —
//! `_CLAP_COMPLETE_INDEX` is the natural candidate, since it carries the cursor
//! position and no human exports it. It cannot be required, because only three
//! of the five registrations set it. Measured in
//! `clap_complete-4.6.7/src/env/shells.rs`: bash (`:33`, `:46`), elvish (`:163`)
//! and zsh (`:425`, `:430`) pass it and read it back (`:79`, `:180`, `:474`),
//! while fish (`:231`) and PowerShell (`:381`) compute `index = args.len() - 1`
//! from the words themselves and never set the variable at all. Confirmed
//! against the binary: `TASQX_COMPLETE=fish tasqx -- tasqx lis` completes with
//! no `_CLAP_COMPLETE_INDEX` in the environment, and so does the PowerShell
//! spelling. Requiring it would silently kill completion in two of the five
//! shells this feature ships for — trading a rare dropped command for a
//! permanently dead feature on macOS's fish users and on Windows.
//!
//! Nor can `--` be used the other way round. It is the documented POSIX way to
//! pass a leading-dash value and tasqx tells users to reach for it; treating its
//! presence as proof of a callback is the *previous* bug, fixed in
//! [`names_a_shell_clap_can_complete`], and reinstating it here would swallow
//! `tasqx add -- "-leading dash"` for everyone rather than for the few.
//!
//! ## What the variable name buys, and what it does not
//!
//! So the mitigation is the variable, and it is a mitigation only. The
//! completion protocol is activated by `$TASQX_COMPLETE` and not by
//! `clap_complete`'s default `$COMPLETE` (see [`COMPLETE_VAR`]). `COMPLETE` is a
//! generic name that another tool, a dotfile, or a half-run activation line can
//! plausibly leave in the environment; the spec's own PowerShell activation line
//! sets it and unsets it in a single statement, so an interrupted paste or a
//! profile that errors between the two leaves it set for the rest of the
//! session. `TASQX_COMPLETE` is a name nothing else has a reason to write, which
//! makes the hazardous state improbable.
//!
//! Improbable is not impossible, and this module claims nothing more. A user who
//! sets `TASQX_COMPLETE` by hand, or whose activation line dies between its two
//! statements, still loses the next `tasqx … -- …` silently. The divergence from
//! clap's convention costs users nothing because tasqx prints its own activation
//! lines (`tasqx completions <shell>`); they paste what tasqx tells them to
//! paste and never type the variable name.

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
///
/// **Deliberately not `clap_complete`'s default `COMPLETE`.** That default is
/// what makes the residual hazard in this module's documentation reachable: any
/// `tasqx … -- …` run with a recognised shell name in the variable is swallowed,
/// silently, exit 0. `COMPLETE` is generic enough that a stale export is a real
/// scenario — the PowerShell activation line sets it and removes it in one
/// statement, so an interrupted paste leaves it set for the session, and any
/// other clap-based tool activated in the same profile writes the same name.
/// `TASQX_COMPLETE` is a name nothing else has cause to set, which is the whole
/// of the mitigation: it makes the hazardous state improbable, not impossible.
///
/// The divergence from clap's documented convention is free here because tasqx
/// generates its own activation lines rather than asking users to remember one.
/// Nothing a user types contains this string.
const COMPLETE_VAR: &str = "TASQX_COMPLETE";

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
    // $TASQX_COMPLETE" is a property of the control flow rather than of the code
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
        // `$TASQX_COMPLETE` unset, empty, or "0". Unreachable in practice: the guard
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

/// Does `$TASQX_COMPLETE`'s value name a shell `clap_complete` has a completer
/// for?
///
/// This is the discriminator between "a shell is asking for candidates" and "a
/// human is running a command with a stale variable in the environment", and it
/// replaces an earlier test that asked whether argv contained `--`.
///
/// It is a PARTIAL discriminator and the module doc says where it stops: a
/// recognised shell name plus a `--` still swallows a real command. This
/// function closes the half that can be closed for free.
///
/// **Why recognisability decides it.** A completion callback is only ever
/// launched by a registration script, and the only thing that emits a
/// registration script is `clap_complete` itself — for a shell it has an
/// `EnvCompleter` for. `TASQX_COMPLETE=nushell tasqx` does not print a script,
/// it errors, so no `nushell` registration has ever existed anywhere and nothing
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
/// that a `$TASQX_COMPLETE` copied from `$SHELL` (`/usr/bin/zsh`) still
/// resolves. The
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
///
/// The claim in the second paragraph — that a completer not built with this
/// fails the build — is enforced by [`escaping_drift`], which recognises one of
/// these by ANSWERING [`RESTORE_PROBE`] rather than by any label it wears. See
/// that constant for why the recognition has to be behavioural.
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
        let word = crate::argv::unescaped(&current.to_string_lossy());
        // Answered before `candidates` is consulted, so the probe never reaches
        // a provider and never costs a store lookup. See `RESTORE_PROBE`.
        if word == RESTORE_PROBE {
            return vec![CompletionCandidate::new(RESTORE_PROBE_ANSWER)];
        }
        candidates(&word)
    })
}

/// The word [`escaping_drift`] hands every attached completer, in its RESTORED
/// spelling: the guard sends `argv::escaped(RESTORE_PROBE)` and a completer
/// built by [`escaped_word_completer`] sees this.
///
/// # Why the guard cannot just look for a marker
///
/// The obvious guard is structural — tag the arg when the wrapper is attached,
/// then assert the tag is there. It was rejected because the tag and the
/// completer are two separate `ArgExt`s (`clap::Arg`'s extensions are keyed by
/// `TypeId`, and the engine looks up `ArgValueCompleter` and nothing else), so
/// they can only ever be attached as two independent acts. A guard on the tag
/// then checks that somebody remembered to write the tag, which is a different
/// proposition from "the dash is restored" and fails in the same silent
/// direction the whole seam exists to prevent.
///
/// # Why it cannot compare candidate sets either
///
/// The other obvious guard is `complete(escaped) == complete(raw)`, which needs
/// no cooperation from the code under test. It is vacuous here: every provider
/// this seam will carry answers `-tag` out of the user's live store, the guard
/// runs in-process with no store, and both sides come back empty. A bare
/// completer would pass it.
///
/// # So the completer answers
///
/// What is left is a word the wrapper recognises AFTER restoring, whose answer
/// is therefore proof the restore ran inside the shipped closure. It costs one
/// string comparison per Tab press, on a path that is about to spend up to
/// 150 ms reading a database, and it is the only shape measured to fail for a
/// bare `ArgValueCompleter` — the mistake Task 7 is most likely to make, which
/// compiles, reads correctly, and leaves `list -ne<TAB>` empty.
///
/// U+007F for the same reason [`crate::argv`] picked U+0001 for its sentinel: a
/// control character no shell produces by accident, so no real completion can
/// collide with it. A user who somehow types it gets one nonsense candidate
/// instead of a wrong one.
const RESTORE_PROBE: &str = "-\u{7f}restore-probe";

/// What a completer built by [`escaped_word_completer`] answers [`RESTORE_PROBE`]
/// with. Distinct from the probe so a completer that merely echoes its input
/// cannot pass by accident.
const RESTORE_PROBE_ANSWER: &str = "\u{7f}restored";

/// Did the completer attached to `pos` restore the escaped dash before its
/// provider saw the word? `None` when nothing is attached.
///
/// Separate from the guard that calls it so the guard's own failure mode is
/// testable: `the_restore_probe_tells_a_wrapped_completer_from_a_bare_one`
/// drives this both ways, because a guard that has only ever returned `true` is
/// a guard nobody has checked can return `false`.
#[cfg(test)]
fn restores_the_escaped_dash(pos: &clap::Arg) -> Option<bool> {
    let completer = pos.get::<ArgValueCompleter>()?;
    let probe = OsString::from(crate::argv::escaped(RESTORE_PROBE));
    let answered =
        |got: Vec<CompletionCandidate>| got.iter().any(|c| c.get_value() == RESTORE_PROBE_ANSWER);
    // `complete_at` is the entry point the engine actually uses
    // (`engine/complete.rs:360`), so it is the one that must hold. `complete` is
    // probed too: `ValueCompleter`'s default `complete_at` delegates to it, but a
    // hand-written impl can override one and not the other, and this seam is
    // exactly where that asymmetry would go unnoticed.
    Some(answered(completer.complete_at(0, &probe)) && answered(completer.complete(&probe)))
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
    ///
    /// # The fixture must never stand in for a shipped completer
    ///
    /// `Arg::add` REPLACES: extensions are keyed by `TypeId`, so installing this
    /// wrapper over a completer `command.rs` already attached silently discards
    /// it and every test below then measures the fixture. That is not
    /// hypothetical — it was measured. With a bare `ArgValueCompleter` attached
    /// to `List::filter`, the two `a_partial_tag_exclusion_*` tests below stayed
    /// green across three forced rebuilds while the real binary offered nothing
    /// at all for `list -ne<TAB>` and `list -needs<TAB>`.
    ///
    /// So the fixture refuses to mask. The shipped surface is
    /// [`escaping_drift`]'s job; these tests own the seam, and the assertion is
    /// the line between the two. When Task 7 attaches the live provider this
    /// fires, and the right answer then is to drive it against a seeded store
    /// rather than to delete the assertion.
    fn candidates_for(words: &[&str], index: usize) -> Vec<String> {
        let mut cmd = Cli::command().mut_subcommand("list", |sc| {
            sc.mut_arg("filter", |a| {
                assert!(
                    a.get::<ArgValueCompleter>().is_none()
                        && a.get::<ArgValueCandidates>().is_none(),
                    "`list filter` already carries a completer, and `Arg::add` \
                     would replace it with this fixture — leaving these tests \
                     measuring themselves. Drive the shipped one instead."
                );
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
    /// carry a provider that never sees the dash.
    ///
    /// Two shapes fail, and only the first used to be checked.
    ///
    /// **`ArgValueCandidates`** yields a fixed list the ENGINE prefix-filters
    /// (`complete_custom_arg_value`: `retain(|c| c.starts_with(value))`) against
    /// the word as the engine holds it — still escaped. Nothing the provider does
    /// can reach that filter, so every `-tag` comes back empty.
    ///
    /// **A bare `ArgValueCompleter`** is the likelier mistake, because it is the
    /// right TYPE: it is handed the word and filters itself, and it is what Task
    /// 7 will reach for. Without [`escaped_word_completer`] around it, the word
    /// it filters against is `\u{1}ne` and it matches nothing. This is not a
    /// worry, it is a measurement — attaching one to `List::filter` left the four
    /// tests in this module green across three forced rebuilds while the real
    /// binary answered `list -ne<TAB>` and `list -needs<TAB>` with nothing, and
    /// answered `list +<TAB>` correctly. Four of five prefixes working is the
    /// worst version of this defect: it looks finished.
    ///
    /// The second shape is caught by [`restores_the_escaped_dash`], which asks
    /// the completer to prove the restore rather than to carry a label saying so.
    /// The commands come from the same set `argv` derives its escaping from, so a
    /// filter command added tomorrow is covered the day it is declared.
    #[test]
    fn escaping_drift() {
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
                // `None` is no completer at all, which is the state today and is
                // not a defect: the positional simply offers clap's own flags.
                if let Some(restored) = restores_the_escaped_dash(pos) {
                    assert!(
                        restored,
                        "`{} {}` carries an ArgValueCompleter that did not restore \
                         the escaped dash — it will be handed `\\u{{1}}ne` where the \
                         user typed `-ne` and match nothing, silently, for every tag \
                         exclusion while `+tag` and `project:x` keep working. Build \
                         it with `complete::escaped_word_completer`.",
                        sc.get_name(),
                        pos.get_id()
                    );
                }
            }
        }
        assert!(
            checked >= crate::argv::FILTER_COMMANDS.len(),
            "every filter command declares a positional; the guard matched {checked} \
             and would otherwise be vacuous"
        );
    }

    /// The guard's own guard: [`restores_the_escaped_dash`] must answer `false`
    /// for the shape it exists to catch, not merely `true` for the good one.
    ///
    /// The previous version of [`escaping_drift`] asserted
    /// `get::<ArgValueCandidates>().is_none()` and nothing else, which is a
    /// condition the tree satisfies whether or not anyone is paying attention —
    /// it had never failed, and when the failing case was constructed by hand it
    /// still did not fail. A guard nobody has watched fail is a comment with a
    /// `#[test]` on it, so the failure is exercised here rather than trusted.
    ///
    /// The `bare` arm is the mutation verbatim: an `ArgValueCompleter` that
    /// filters against the word it was handed, which is the correct and obvious
    /// way to write one and is wrong only because of the pre-pass.
    #[test]
    fn the_restore_probe_tells_a_wrapped_completer_from_a_bare_one() {
        let wrapped = clap::Arg::new("filter").add(escaped_word_completer(|_| {
            Vec::<CompletionCandidate>::new()
        }));
        assert_eq!(
            restores_the_escaped_dash(&wrapped),
            Some(true),
            "a completer built by `escaped_word_completer` must answer the probe"
        );

        let bare = clap::Arg::new("filter").add(ArgValueCompleter::new(|current: &OsStr| {
            let current = current.to_string_lossy().into_owned();
            ["-needs", "+home"]
                .iter()
                .filter(|c| c.starts_with(&current))
                .map(|c| CompletionCandidate::new(*c))
                .collect::<Vec<_>>()
        }));
        assert_eq!(
            restores_the_escaped_dash(&bare),
            Some(false),
            "a bare ArgValueCompleter must NOT answer the probe; if it does, the \
             guard cannot tell the two apart and passes on the defect"
        );

        assert_eq!(
            restores_the_escaped_dash(&clap::Arg::new("filter")),
            None,
            "an unattached positional is not a defect and must be distinguishable \
             from a failing one"
        );
    }
}
