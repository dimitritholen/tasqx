//! The `completions` verb: print the line that turns Tab completion on.
//!
//! [`super`] owns the Tab path. This module owns the line a user adds to their
//! shell's startup file once, so that the Tab path exists at all. It prints
//! that line and nothing else; adding it is the user's (D223).
//!
//! # D33 is NOT inverted here
//!
//! `complete.rs`'s module doc argues that on the callback path every failure
//! must be silence, zero candidates and exit 0, because the medium is a
//! half-typed command line rather than a question a user asked. This verb is
//! the exemption: a human typed `tasqx completions`, is looking at the output,
//! and a shell tasqx cannot serve is a message on stderr and **exit 2**.
//!
//! # Where the shapes come from
//!
//! The five activation lines are `clap_complete`'s own, copied out of
//! `clap_complete-4.6.7/src/env/mod.rs` (the "To source your completions"
//! section) with `COMPLETE` replaced by `TASQX_COMPLETE` and `your_program` by
//! `tasqx`. They are NOT one shape with a name swapped: bash and zsh source a
//! line from an rc file, elvish `eval`s from `~/.elvish/rc.elv`, fish reads its
//! OWN completions directory, and PowerShell runs a three-statement one-liner
//! from `$PROFILE`. See [`ACTIVATIONS`].
//!
//! The list of shells is read from `Shells::builtins()` — clap's registry, the
//! same one `complete::intercept` resolves `$TASQX_COMPLETE` against — and
//! [`tests::every_builtin_shell_has_an_activation_line`] fails the build if the
//! two ever disagree.

use std::path::PathBuf;

use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use clap_complete::env::Shells;
use serde_json::json;
use tasqx_core::ApiError;

/// The five shell names, for completing `tasqx completions <TAB>` itself.
///
/// Read out of clap's registry rather than restated, for the reason
/// [`ACTIVATIONS`] gives: this module must not be the sixth place a list of
/// shells is kept. An [`ArgValueCandidates`] because the whole word is the
/// value — no prefix grammar of its own — so the engine's own filter is the
/// right filter (see `candidates::projects` for the same reasoning at length).
pub(crate) fn shells() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        Shells::builtins()
            .names()
            .map(CompletionCandidate::new)
            .collect()
    })
}

/// Where each shell reads its activation line from. Three shapes, because
/// upstream has three.
#[cfg_attr(test, derive(Debug))]
enum Target {
    /// A fixed path under the user's home directory, given as path segments.
    /// bash (`~/.bashrc`), zsh (`~/.zshrc`) and elvish (`~/.elvish/rc.elv`).
    UnderHome(&'static [&'static str]),
    /// fish's own completions directory —
    /// `${XDG_CONFIG_HOME:-~/.config}/fish/completions/tasqx.fish`.
    ///
    /// Not an rc file, and the difference is fish's design rather than a
    /// preference of ours: fish LAZY-loads a file named after the command being
    /// completed, so the activation line only runs when the user actually
    /// completes `tasqx`. Putting the same line in `config.fish` would work and
    /// would run at every shell start, which is upstream's documented shape
    /// abandoned for no gain.
    ///
    /// `$XDG_CONFIG_HOME` is honoured because fish honours it. A user who moved
    /// their fish config and got an activation line in `~/.config/fish` anyway
    /// would have a file that is never read and a feature that silently does not
    /// work — the failure mode with no symptom, which is the one this project
    /// hunts.
    FishCompletions,
    /// The shell knows and we do not. PowerShell only: `$PROFILE` is a
    /// PowerShell variable, not an environment variable, and its value differs
    /// between Windows PowerShell, PowerShell 7 and the ISE, so `--json`
    /// reports no target rather than a guess.
    OnlyTheHostKnows,
}

/// One shell's activation line and the file it belongs in.
#[cfg_attr(test, derive(Debug))]
struct Activation {
    /// `clap_complete`'s canonical name for the shell — the string
    /// `EnvCompleter::name` returns, which is also what `$TASQX_COMPLETE` is
    /// resolved to by `complete::canonical_shell_name`. Matching on this rather
    /// than on the user's spelling is why `pwsh` and `powershell` reach the same
    /// row.
    shell: &'static str,
    /// The line the shell must run at startup, verbatim.
    line: &'static str,
    target: Target,
}

/// The five activation lines, copied from `clap_complete-4.6.7/src/env/mod.rs`
/// with `COMPLETE` → `TASQX_COMPLETE` and `your_program` → `tasqx`.
///
/// Upstream writes them as complete `echo … >> file` shell commands; what is
/// stored here is the line that ends up IN the file, and the file is the
/// [`Target`] beside it, which `--json` reports.
///
/// Copied, deliberately, rather than derived: `clap_complete` exposes the
/// registration script (`EnvCompleter::write_registration`) but not the
/// activation line that sources it, so there is nothing to read. That makes this
/// the one table in the completion feature that CAN drift from upstream, and the
/// guards below are sized to that: the pin (`=4.6.7` in Cargo.toml) makes an
/// upstream change a deliberate act, and
/// [`tests::every_builtin_shell_has_an_activation_line`] fails the build the day
/// clap gains a sixth shell — the case where a missing row would otherwise mean
/// `tasqx completions <newshell>` answering "unknown shell" for a shell tasqx
/// really does complete.
///
/// The two `TASQX_COMPLETE` spellings that are NOT `VAR=value` are not
/// typos. Elvish sets environment variables through its `E:` namespace, and
/// PowerShell through `$env:`; a POSIX `VAR=value cmd` prefix is a syntax error
/// in both.
const ACTIVATIONS: &[Activation] = &[
    Activation {
        shell: "bash",
        line: "source <(TASQX_COMPLETE=bash tasqx)",
        target: Target::UnderHome(&[".bashrc"]),
    },
    Activation {
        shell: "elvish",
        line: "eval (E:TASQX_COMPLETE=elvish tasqx | slurp)",
        target: Target::UnderHome(&[".elvish", "rc.elv"]),
    },
    Activation {
        shell: "fish",
        line: "TASQX_COMPLETE=fish tasqx | source",
        target: Target::FishCompletions,
    },
    Activation {
        shell: "powershell",
        line: "$env:TASQX_COMPLETE = \"powershell\"; tasqx | Out-String | Invoke-Expression; Remove-Item Env:\\TASQX_COMPLETE",
        target: Target::OnlyTheHostKnows,
    },
    Activation {
        shell: "zsh",
        line: "source <(TASQX_COMPLETE=zsh tasqx)",
        target: Target::UnderHome(&[".zshrc"]),
    },
];

/// Run the `completions` verb: write the activation line to stdout and nothing
/// else.
///
/// Exactly one line, with no heading and no advice, because the shape this has
/// to support is `tasqx completions bash >> ~/.bashrc`. Anything else on stdout
/// lands in the user's profile as garbage the shell then tries to run. The
/// guidance a first-time reader needs lives in `tasqx completions -h`, where it
/// cannot be redirected into a file.
///
/// `--json` comes free from the ordinary [`crate::CmdOutcome`] path and carries
/// the `target` — the file the line belongs in — so a dotfile manager can ask
/// tasqx instead of hard-coding five paths of its own.
pub(crate) fn run(shell: Option<&str>) -> crate::CmdOutcome {
    let a = resolve_shell(shell)?;
    Ok((
        json!({
            "shell": a.shell,
            "line": a.line,
            "target": target_path(a).map(|p| p.to_string_lossy().into_owned()),
        }),
        format!("{}\n", a.line),
    ))
}

/// Which shell, or a refusal that says why.
///
/// `$SHELL` when none is named, and only `$SHELL`: it is the variable every
/// POSIX login shell sets to its own path, and `file_stem` is applied to it by
/// `complete::canonical_shell_name` exactly as `Shells::completer_for_path`
/// does, so `/usr/bin/zsh` resolves. No Windows shell sets it, and the refusal
/// says so rather than leaving it as a puzzle.
fn resolve_shell(requested: Option<&str>) -> Result<&'static Activation, ApiError> {
    let known = || Shells::builtins().names().collect::<Vec<_>>().join(", ");
    let name = match requested {
        Some(name) => name.to_string(),
        None => std::env::var("SHELL").unwrap_or_default(),
    };
    if name.is_empty() {
        return Err(ApiError::bad_request(format!(
            "cannot tell which shell: $SHELL is not set (no Windows shell sets it, \
             and neither do most non-login sessions). Name it yourself — one of \
             {} — for example `tasqx completions bash`.",
            known()
        )));
    }
    activation_for(&name).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown shell {name:?}: tasqx completes {}.",
            known()
        ))
    })
}

/// The row for `name`, resolved through clap's own alias set so `pwsh` and
/// `/usr/bin/zsh` land on the same rows `$TASQX_COMPLETE` would.
///
/// Going through `complete::canonical_shell_name` rather than comparing
/// [`Activation::shell`] against the raw string is what keeps this verb and the
/// callback path from disagreeing: a `pwsh` this function refused but
/// `intercept` accepted would print "unknown shell" for a shell that completes
/// perfectly well.
fn activation_for(name: &str) -> Option<&'static Activation> {
    let canonical = super::canonical_shell_name(std::ffi::OsStr::new(name))?;
    ACTIVATIONS.iter().find(|a| a.shell == canonical)
}

/// The file the activation line belongs in for this shell, or `None` where
/// only the shell knows (PowerShell) or there is no home directory.
fn target_path(a: &'static Activation) -> Option<PathBuf> {
    match &a.target {
        Target::UnderHome(segments) => {
            let mut path = home()?;
            for segment in *segments {
                path.push(segment);
            }
            Some(path)
        }
        Target::FishCompletions => {
            let moved = std::env::var("XDG_CONFIG_HOME").ok();
            fish_target_under(moved.as_deref(), home)
        }
        Target::OnlyTheHostKnows => None,
    }
}

/// fish's completions file under `xdg_config_home`, falling back to
/// `~/.config` when the variable is unset or empty.
///
/// **Split out purely so the XDG branch can be guarded.** It could not be
/// before: the variable was read inline, and the only way to exercise the moved
/// case was to set a process-global environment variable in a test binary whose
/// tests run in parallel threads — which this module refuses to do for the same
/// reason `complete.rs` refuses it. So the branch shipped untested, and a review
/// mutation proved it: deleting `$XDG_CONFIG_HOME` support outright left all 63
/// tests green while the test's own doc claimed the case was covered.
///
/// The value and the home lookup are both parameters, so both branches are
/// reachable from a test without touching the process.
fn fish_target_under(
    xdg_config_home: Option<&str>,
    home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    // Empty is treated as unset, which is what the XDG spec says and what fish
    // does: an exported-but-blank variable is not a relocation.
    let base = match xdg_config_home.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home()?.join(".config"),
    };
    Some(base.join("fish").join("completions").join("tasqx.fish"))
}

/// The user's home directory, via the same crate that resolves the store's
/// location (`directories`), so this verb and `tasqx config path` cannot
/// disagree about where the user lives.
fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shell `clap_complete` can generate a registration for must have an
    /// activation line here.
    ///
    /// This is the drift guard for the one table in the completion feature that
    /// is copied rather than derived. Upstream gaining a sixth shell would
    /// otherwise mean `TASQX_COMPLETE=<newshell> tasqx` printing a working
    /// registration while `tasqx completions <newshell>` answered "unknown
    /// shell" — the tool refusing to tell users about a feature it has.
    ///
    /// The reverse direction is asserted too: a row here for a shell clap cannot
    /// complete would print an activation line that activates nothing.
    #[test]
    fn every_builtin_shell_has_an_activation_line() {
        let mut clap_names: Vec<&str> = Shells::builtins().names().collect();
        let mut ours: Vec<&str> = ACTIVATIONS.iter().map(|a| a.shell).collect();
        clap_names.sort_unstable();
        ours.sort_unstable();
        assert_eq!(
            clap_names, ours,
            "the activation table and `Shells::builtins()` disagree. A shell clap \
             can complete and this table cannot name is a feature tasqx has and \
             refuses to explain; the other direction is a line that activates \
             nothing."
        );
    }

    /// Every activation line must name the variable that actually turns the
    /// callback path on.
    ///
    /// The failure this catches is total and silent: a line carrying clap's
    /// default `COMPLETE` looks right, is what every clap tutorial shows, and
    /// activates nothing at all — `complete::intercept` reads `TASQX_COMPLETE`
    /// and returns immediately for anything else. The user pastes it, restarts
    /// their shell, presses Tab, and gets the shell's own filename completion
    /// with no error anywhere.
    ///
    /// It also pins the divergence itself. `COMPLETE_VAR`'s doc argues that the
    /// tasqx-specific name is what makes the residual `--` hazard improbable;
    /// emitting `COMPLETE` here would hand every user the generic variable and
    /// reinstate it.
    #[test]
    fn every_activation_line_names_the_tasqx_variable_and_the_binary() {
        for a in ACTIVATIONS {
            assert!(
                a.line.contains("TASQX_COMPLETE"),
                "the {} activation line must set $TASQX_COMPLETE, not clap's \
                 generic $COMPLETE which `intercept` ignores: {:?}",
                a.shell,
                a.line
            );
            assert!(
                a.line.contains("tasqx"),
                "the {} activation line must invoke the binary: {:?}",
                a.shell,
                a.line
            );
            assert!(
                a.line.contains(a.shell),
                "the {} activation line must ask for its own shell, or the shell \
                 registers another one's protocol: {:?}",
                a.shell,
                a.line
            );
        }
    }

    /// A shell nobody has a story for is still exit 2, and the message lists
    /// what would have worked — read from clap, so it can never name a shell
    /// tasqx does not actually complete.
    #[test]
    fn an_unknown_shell_is_refused_naming_the_supported_set() {
        let err = resolve_shell(Some("ksh")).expect_err("ksh is not completable");
        assert_eq!(err.exit_code(), 2);
        for shell in Shells::builtins().names() {
            assert!(
                err.message.contains(shell),
                "the refusal must name {shell:?}: {}",
                err.message
            );
        }
    }

    /// clap's alias set decides, not a string comparison here: `pwsh` is the
    /// usual spelling outside Windows and must reach the PowerShell row, and a
    /// `$SHELL`-shaped path must resolve through `file_stem` exactly as
    /// `complete::intercept` resolves it.
    #[test]
    fn shell_spellings_resolve_the_way_the_callback_path_resolves_them() {
        for spelling in ["pwsh", "powershell", "powershell_ise"] {
            assert_eq!(
                resolve_shell(Some(spelling)).expect(spelling).shell,
                "powershell"
            );
        }
        assert_eq!(resolve_shell(Some("/usr/bin/zsh")).unwrap().shell, "zsh");
    }

    /// PowerShell's target is unknowable from here, so `--json` reports none
    /// rather than a guess a dotfile manager would then write to; the other
    /// four must name an absolute file, or the field is useless on Linux and
    /// macOS.
    #[test]
    fn every_shell_but_powershell_names_its_file() {
        for a in ACTIVATIONS {
            let path = target_path(a);
            if matches!(a.target, Target::OnlyTheHostKnows) {
                assert!(path.is_none(), "{}: {path:?}", a.shell);
                continue;
            }
            let path = path.unwrap_or_else(|| panic!("{}: no target", a.shell));
            assert!(
                path.is_absolute(),
                "{}'s target must be absolute, got {}",
                a.shell,
                path.display()
            );
        }
    }

    /// fish's own completions directory, not an rc file, and `$XDG_CONFIG_HOME`
    /// honoured because fish honours it.
    ///
    /// Asserted on the shape rather than on a literal path so it holds on every
    /// platform.
    ///
    /// The XDG half is asserted in the sibling below. It used to be claimed
    /// HERE — this doc said the case was "checked through the same function with
    /// the variable injected" — and there was no such assertion and no such
    /// injection: `target_path` read the variable inline and took no argument.
    /// A review mutation deleted `$XDG_CONFIG_HOME` support outright and all 63
    /// tests stayed green. A doc claiming coverage that does not exist is worse
    /// than an admitted gap, because it stops anyone looking.
    #[test]
    fn fish_targets_its_completions_directory() {
        let a = resolve_shell(Some("fish")).unwrap();
        let path = target_path(a).expect("fish has a knowable target");
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(
            text.ends_with("fish/completions/tasqx.fish"),
            "fish lazy-loads a file named after the command; got {text}"
        );
    }

    /// A user who moved their fish config gets the line where fish will read it.
    ///
    /// Driven through [`fish_target_under`] with the value and the home lookup
    /// both injected, which is the whole reason that function was split out of
    /// `target_path`: the alternative is setting a process-global environment
    /// variable in a test binary whose tests run in parallel threads, and this
    /// module refuses that for the same reason `complete.rs` does.
    ///
    /// The failure it guards has no symptom. An activation line written to
    /// `~/.config/fish` for a user whose config lives elsewhere sits in a file
    /// fish never reads: the file exists, and Tab does nothing forever.
    ///
    /// Mutation-proven: replacing the body with the unconditional
    /// `home()?.join(".config")` reddens this naming the path it produced.
    #[test]
    fn a_moved_fish_config_gets_the_line_where_fish_reads_it() {
        let home = || Some(PathBuf::from("/home/u"));
        let shape = |p: PathBuf| p.to_string_lossy().replace('\\', "/");

        assert_eq!(
            shape(fish_target_under(Some("/xdg"), home).unwrap()),
            "/xdg/fish/completions/tasqx.fish",
            "$XDG_CONFIG_HOME must decide the base, or the line lands in a file \
             fish never reads and completion silently does not work"
        );

        // Unset and empty are the same thing, per the XDG spec and per fish: an
        // exported-but-blank variable is not a relocation, and treating it as
        // one would resolve the target to `fish/completions/tasqx.fish`
        // RELATIVE to the working directory.
        for blank in [None, Some("")] {
            assert_eq!(
                shape(fish_target_under(blank, home).unwrap()),
                "/home/u/.config/fish/completions/tasqx.fish",
                "{blank:?} must fall back to ~/.config"
            );
        }
    }

    /// The whole output of the default mode is the activation line, so
    /// `tasqx completions bash >> ~/.bashrc` is a working setup command rather
    /// than a way to put a banner in a startup file.
    #[test]
    fn printing_emits_exactly_the_activation_line() {
        let (json, text) = run(Some("bash")).expect("print");
        assert_eq!(text, "source <(TASQX_COMPLETE=bash tasqx)\n");
        assert_eq!(json["shell"], "bash");
        assert_eq!(json["line"], "source <(TASQX_COMPLETE=bash tasqx)");
        let target = json["target"]
            .as_str()
            .unwrap_or_default()
            .replace('\\', "/");
        assert!(target.ends_with("/.bashrc"), "got {target:?}");
        assert_eq!(text.lines().count(), 1, "one line, no advice: {text:?}");
    }
}
