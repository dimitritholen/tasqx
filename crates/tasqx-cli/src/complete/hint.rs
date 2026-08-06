//! The one line that tells a fresh install Tab completion exists (D57).
//!
//! [`super::install`] can set completion up in a single command, and
//! [`super`] serves the Tab press once it is on. Neither of them is any use to
//! a user who does not know the feature is there — and until this module,
//! nothing in the binary ever said so. The README says it, `tasqx completions
//! -h` says it, and both are places you only look once you already know.
//!
//! # Why the binary has to be the one to speak
//!
//! The two documented install routes are a release archive and `cargo install
//! --path`. cargo has no post-install hook at all, so for the from-source route
//! the running binary is the *only* thing that can ever mention this. Packaging
//! (a formula that drops the activation line into a directory the shell already
//! reads) fixes the case where a package manager was involved and nothing else.
//!
//! # Three rules, and each one is a promise about the note
//!
//!  1. **It is one line, and it never touches a file the user owns.**
//!     `--install` edits a startup file and therefore asks first ([D57], and
//!     `install.rs`'s module doc for what is at stake). A note that offered to
//!     do the editing would be a fresh binary asking to edit a profile unbidden,
//!     on a run the user started for some other reason entirely.
//!  2. **It is said once, and tasqx will not say a thing it cannot promise to
//!     stop saying.** The marker recording that it was said is written BEFORE
//!     the note is printed, and a marker that cannot be written means the note
//!     is not printed at all — otherwise a read-only config dir turns a one-time
//!     note into a permanent one, which is the shape users rightly hate.
//!  3. **It is silent everywhere silence is the contract.** Not on the Tab path
//!     (D33: there, every failure is zero candidates and exit 0), not under
//!     `--json`, not when stderr is not a terminal, and not after a command that
//!     failed — an error is what the user is reading, and an unrelated nudge
//!     under it is noise at the worst moment.
//!
//! # What the probe can and cannot see
//!
//! [`state`] reads the ONE file `--install` would have edited for `$SHELL` and
//! asks whether `TASQX_COMPLETE` appears in it. That catches both the marked
//! block and a line the user pasted from the README by hand. It cannot see an
//! activation line that lives anywhere else — `~/.zprofile`, an oh-my-zsh custom
//! file, a system-wide `/etc` snippet, a Homebrew-managed completions directory
//! — and there is no way for a process to ask the shell that spawned it whether
//! a completer is registered. So the probe answers [`State::Unknown`] wherever
//! it cannot see, and an unknown is treated as "not on": one line, once, is the
//! right cost for a wrong guess in that direction, where nagging a user who is
//! already set up would not be.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// What the probe could establish about the file `--install` targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum State {
    /// The activation line is in the file, marked block or hand-pasted.
    On,
    /// The file exists and does not mention the variable, or does not exist.
    Off,
    /// No shell to resolve, no home to look under, no readable file, or a shell
    /// whose target tasqx will not guess (PowerShell — see
    /// `install::Target::OnlyTheHostKnows`). Treated as [`State::Off`] by
    /// [`should_speak`]; kept distinct so the tests can say which case they mean.
    Unknown,
}

/// The occasion a note is being considered for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Occasion {
    /// `tasqx init` — the one moment a user is deliberately reading setup
    /// output. Not gated by the marker: `init` is rare, deliberate, and the
    /// place this belongs, so it says its piece even on a machine where an
    /// ordinary command said it months ago.
    Setup,
    /// Any other verb: gated by the marker, so exactly one of them ever says it.
    Ordinary,
}

/// Everything [`should_speak`] is allowed to consult, gathered so the rule is a
/// pure function of six bools and can be tested without a terminal, a config
/// dir or a shell.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Conditions {
    /// stderr is a terminal. The note goes there, so this is the question about
    /// the note's own medium — not about stdout, which may well be a pipe on a
    /// run whose stderr a human is watching.
    pub terminal: bool,
    /// `--json` was passed. A machine reader gets nothing conversational.
    pub json: bool,
    /// `[completion] hint` from `config.toml`.
    pub enabled: bool,
    pub state: State,
    /// The marker recording that the note has been made.
    pub already_said: bool,
    pub occasion: Occasion,
}

/// The whole rule.
pub(crate) fn should_speak(c: &Conditions) -> bool {
    if !c.terminal || c.json || !c.enabled {
        return false;
    }
    if c.state == State::On {
        return false;
    }
    c.occasion == Occasion::Setup || !c.already_said
}

/// The note. Two lines: what to run, and the fact that this is the only time it
/// will be said — without which a one-time note reads like the start of a habit.
pub(crate) const NOTE: &str =
    "hint: Tab completion does not look switched on — `tasqx completions --install` \
     sets it up for your shell.\n      \
     Said once. `tasqx config set completion.hint false` stops it being said at all.";

/// Is `[completion] hint` on? Every failure mode — no config dir, no file,
/// malformed TOML, wrong type — lands on the registered default (`true`), which
/// is the opposite direction from `notify.enabled` and deliberately so: the
/// failure of a *notification* setting must be silence, and the failure of a
/// setting that governs one line of help must be the help.
fn enabled() -> bool {
    let s =
        crate::config::find("completion.hint").expect("completion.hint is a registered setting");
    let (v, _) = crate::config::resolve(s, None, crate::config::toml_value(s).as_deref());
    v == "true"
}

/// Where the marker lives: beside `config.toml`, never inside it.
///
/// `config.toml` is the user's file — hand-written, hand-edited, and the thing
/// `tasqx config set` is careful with. A "we have said this" flag is state, not
/// a preference, and writing state into a preferences file the user maintains is
/// how a tool ends up rewriting comments and key order on a run that had nothing
/// to do with configuration. A sibling file costs one path and keeps
/// `config.toml` a file tasqx only writes when asked.
fn marker_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("completion-hint-said"))
}

/// What goes in the marker, because somebody will find it and wonder.
const MARKER_BODY: &str = "\
# tasqx wrote this file the one time it mentioned `tasqx completions --install`.
# Delete it to be told again. `tasqx config set completion.hint false` silences
# the note for good, marker or no marker.
";

/// Read the file `--install` targets for `$SHELL` and answer whether the
/// activation line is in it.
///
/// Every refusal on the way — no `$SHELL`, a shell tasqx does not complete, no
/// home directory, PowerShell's deliberate refusal to guess a profile path — is
/// an [`State::Unknown`], not an error. This runs on the way out of an ordinary
/// command that has already succeeded; nothing it discovers is worth a word on
/// its own.
fn state() -> State {
    state_of(super::install::probe_target(), |p| {
        std::fs::read_to_string(p).ok()
    })
}

/// [`state`] with the file lookup and the reader injected, so both branches are
/// reachable from a test without a `$SHELL`, a home directory or a real file.
fn state_of(target: Option<PathBuf>, read: impl FnOnce(&Path) -> Option<String>) -> State {
    let Some(path) = target else {
        return State::Unknown;
    };
    match read(&path) {
        // A file that is not there is a file with no activation line in it. The
        // commonest shape of "completion is off" is a `.bashrc` that does not
        // exist yet, and calling that Unknown would be true but useless.
        None if !path.exists() => State::Off,
        None => State::Unknown,
        Some(text) if text.contains(super::COMPLETE_VAR) => State::On,
        Some(_) => State::Off,
    }
}

/// Consider the note, and make it if every condition holds.
///
/// The marker is written first and a failure to write it is a decision not to
/// speak — see rule 2 in the module doc.
pub(crate) fn offer(occasion: Occasion, json: bool) {
    let already_said = marker_path().is_some_and(|p| p.exists());
    let conditions = Conditions {
        terminal: std::io::stderr().is_terminal(),
        json,
        enabled: enabled(),
        // Ordered last of the cheap checks on purpose: the probe reads a file,
        // and a run that is silent for any other reason must not pay for it.
        state: State::Unknown,
        already_said,
        occasion,
    };
    // Everything but the probe, so the file read happens only when it can change
    // the answer.
    if !should_speak(&Conditions {
        state: State::Off,
        ..conditions
    }) {
        return;
    }
    if !should_speak(&Conditions {
        state: state(),
        ..conditions
    }) {
        return;
    }
    if !record() {
        return;
    }
    eprintln!("{NOTE}");
}

/// Write the marker, reporting whether the note may now be made.
///
/// Creates the config directory when it is missing, which is the ordinary case
/// on a fresh machine — the same directory `tasqx config set` would create. No
/// config dir at all (`TASQX_CONFIG_DIR=`, how the tests isolate themselves)
/// means no marker, which means no note.
fn record() -> bool {
    record_at(marker_path())
}

/// [`record`] with the path handed in, so the three outcomes — no config dir, a
/// directory that has to be created, a write that fails — are reachable from a
/// test without setting a process-global `$TASQX_CONFIG_DIR` in a test binary
/// whose tests run in parallel threads (the same refusal `install.rs` makes
/// about `$XDG_CONFIG_HOME`).
fn record_at(path: Option<PathBuf>) -> bool {
    let Some(path) = path else {
        return false;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    std::fs::write(&path, MARKER_BODY).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Conditions {
        Conditions {
            terminal: true,
            json: false,
            enabled: true,
            state: State::Off,
            already_said: false,
            occasion: Occasion::Ordinary,
        }
    }

    #[test]
    fn a_fresh_terminal_run_with_completion_off_gets_the_note() {
        assert!(should_speak(&base()));
    }

    /// The medium is stderr, so the question is about stderr. A user running
    /// `tasqx list > file` in a terminal is still a user reading stderr.
    #[test]
    fn nothing_is_said_when_stderr_is_not_a_terminal() {
        assert!(!should_speak(&Conditions {
            terminal: false,
            ..base()
        }));
    }

    #[test]
    fn nothing_is_said_to_a_machine_reader() {
        assert!(!should_speak(&Conditions {
            json: true,
            ..base()
        }));
    }

    #[test]
    fn the_setting_switches_it_off_before_anything_else_is_consulted() {
        assert!(!should_speak(&Conditions {
            enabled: false,
            ..base()
        }));
        // Including on the one occasion that ignores the marker.
        assert!(!should_speak(&Conditions {
            enabled: false,
            occasion: Occasion::Setup,
            ..base()
        }));
    }

    #[test]
    fn a_user_who_already_has_completion_is_never_told_about_it() {
        assert!(!should_speak(&Conditions {
            state: State::On,
            ..base()
        }));
        assert!(!should_speak(&Conditions {
            state: State::On,
            occasion: Occasion::Setup,
            ..base()
        }));
    }

    /// The probe cannot see every place an activation line can live, so it
    /// answers Unknown for most of them. Unknown speaks: one line once is the
    /// cheap direction to be wrong in.
    #[test]
    fn an_unreadable_setup_is_told_rather_than_assumed_to_be_fine() {
        assert!(should_speak(&Conditions {
            state: State::Unknown,
            ..base()
        }));
    }

    #[test]
    fn an_ordinary_verb_says_it_once() {
        assert!(!should_speak(&Conditions {
            already_said: true,
            ..base()
        }));
    }

    /// `init` is the setup moment and is rare enough to repeat on.
    #[test]
    fn init_says_it_even_when_an_ordinary_verb_already_did() {
        assert!(should_speak(&Conditions {
            already_said: true,
            occasion: Occasion::Setup,
            ..base()
        }));
    }

    #[test]
    fn a_shell_with_no_target_to_read_is_unknown() {
        assert_eq!(state_of(None, |_| unreachable!()), State::Unknown);
    }

    #[test]
    fn a_startup_file_naming_the_variable_is_on_however_it_got_there() {
        let target = Some(PathBuf::from("/nonexistent/.zshrc"));
        // The marked block.
        assert_eq!(
            state_of(target.clone(), |_| Some(
                "# >>> tasqx completions >>>\nsource <(TASQX_COMPLETE=zsh tasqx)\n".into()
            )),
            State::On
        );
        // A line pasted out of the README, with no block around it.
        assert_eq!(
            state_of(target, |_| Some(
                "source <(TASQX_COMPLETE=zsh tasqx)\n".into()
            )),
            State::On
        );
    }

    #[test]
    fn a_startup_file_without_the_variable_is_off() {
        assert_eq!(
            state_of(Some(PathBuf::from("/nonexistent/.zshrc")), |_| Some(
                "export PATH=$PATH:/usr/local/bin\n".into()
            )),
            State::Off
        );
    }

    /// A scratch directory this test binary owns, unique per call — the same
    /// idiom `install::tests::scratch` uses, and for the same reason: nothing
    /// here may go near a real startup file.
    fn scratch(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tasqx-hint-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        dir
    }

    /// A `.bashrc` that does not exist yet is the commonest shape of "not set
    /// up", and reporting it as Unknown would be true and useless.
    #[test]
    fn a_startup_file_that_does_not_exist_is_off_rather_than_unknown() {
        let dir = scratch("missing");
        assert_eq!(state_of(Some(dir.join(".bashrc")), |_| None), State::Off);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that IS there and cannot be read — permissions, a directory in its
    /// place, invalid UTF-8 — is the case where tasqx genuinely does not know.
    #[test]
    fn a_file_that_exists_but_will_not_read_is_unknown() {
        let dir = scratch("unreadable");
        let path = dir.join("rc");
        std::fs::create_dir(&path).expect("a directory where the rc file would be");
        assert_eq!(state_of(Some(path), |_| None), State::Unknown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The config directory does not exist on a machine that has never run
    /// `tasqx config set`, which is exactly the machine this note is for.
    #[test]
    fn the_marker_creates_the_directory_it_lives_in() {
        let dir = scratch("record");
        let nested = dir.join("never-created").join("completion-hint-said");
        assert!(record_at(Some(nested.clone())));
        assert!(nested.exists());
        assert!(
            std::fs::read_to_string(&nested)
                .expect("the marker reads back")
                .contains("tasqx completions --install"),
            "somebody will find this file and need it to say what it is"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No config dir means nowhere to record that the note was made, and rule 2
    /// says a note tasqx cannot promise to stop making is a note it does not
    /// make. `TASQX_CONFIG_DIR=` reaches this, and so does a home directory the
    /// platform will not give up.
    #[test]
    fn nothing_is_recorded_when_there_is_nowhere_to_record_it() {
        assert!(!record_at(None));
    }

    /// A read-only config dir is the other half of rule 2: the write fails, and
    /// the caller must take that as "do not speak" rather than speaking anyway
    /// on this run and every run after it.
    #[test]
    fn a_marker_that_cannot_be_written_is_a_decision_not_to_speak() {
        let dir = scratch("readonly");
        let path = dir.join("completion-hint-said");
        // A directory where the marker file goes: `create_dir_all` on the parent
        // succeeds, the write cannot. Portable — no permission bits involved,
        // which Windows would ignore anyway.
        std::fs::create_dir(&path).expect("a directory in the marker's place");
        assert!(!record_at(Some(path)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The note has to name the command that fixes it and the way to stop it
    /// being said; without the second it reads as the start of a habit.
    #[test]
    fn the_note_names_the_command_and_its_own_off_switch() {
        assert!(NOTE.contains("tasqx completions --install"));
        assert!(NOTE.contains("completion.hint"));
    }
}
