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
//! lookup that blows its time budget, an unrecognised `$COMPLETE` value — all of
//! them are silence.
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

use std::ffi::OsString;

use clap::CommandFactory;
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
    if std::env::var_os(COMPLETE_VAR).is_none() {
        return;
    }

    let raw: Vec<OsString> = std::env::args_os().collect();
    // A registration request (`COMPLETE=bash tasqx`) carries no `--`; a callback
    // request always does, because the script clap emits invokes
    // `<completer> -- "${words[@]}"`. The distinction is needed once, in the
    // error arm below, so it is taken before `raw` is consumed.
    let is_callback = raw.iter().any(|a| a == "--");

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
        // `$COMPLETE` was unset, empty, or "0": not a completion run at all.
        Ok(false) => {}
        // The only error `try_complete` produces before writing anything is a
        // `$COMPLETE` naming a shell it has no completer for. Nothing has been
        // printed and nothing will be.
        //
        // Which of the two right answers applies depends on who set the
        // variable. A real callback (it carried `--`) gets the policy above:
        // silence, exit 0. A plain `tasqx list` that merely inherited a stale
        // or hand-set `$COMPLETE` gets to RUN — swallowing it into exit 0 there
        // would turn an ordinary command into a silent no-op, which is the
        // silent-drop defect this file is at pains not to commit outside the
        // callback path.
        Err(_) if is_callback => std::process::exit(0),
        Err(_) => {}
    }
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
fn prepassed(raw: Vec<OsString>) -> Vec<OsString> {
    let Some(sep) = raw.iter().position(|a| a == "--") else {
        return raw;
    };
    let mut out: Vec<OsString> = raw[..=sep].to_vec();
    out.extend(crate::argv::prepass(raw[sep + 1..].iter().cloned()).argv);
    out
}
