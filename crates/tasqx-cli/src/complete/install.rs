//! The `completions` verb: print the activation line, or edit the user's own
//! startup file with it.
//!
//! [`super`] owns the Tab path. This module owns the ONE-TIME act that makes the
//! Tab path exist at all — and the two could hardly be more different in what a
//! mistake costs.
//!
//! # D33 is NOT inverted here, and that is the whole point of the file boundary
//!
//! `complete.rs`'s module doc argues at length that on the callback path every
//! failure must be silence, zero candidates and exit 0, because the medium is a
//! half-typed command line rather than a question a user asked. It then says, in
//! one sentence, that the `completions` verb is exempt. This is that exemption,
//! written out:
//!
//!  * a shell tasqx cannot serve is a message on stderr and **exit 2**;
//!  * a `--install` with no way to know which file to edit **refuses** rather
//!    than picking one;
//!  * a `--uninstall` that removed nothing **exits 4** rather than reporting a
//!    success it did not have.
//!
//! Nothing about that is inconsistent with the sibling module. A human typed
//! `tasqx completions`, is looking at the output, and asked a question that has
//! an answer. Silence here would be the silent-drop class in its purest form:
//! the user believes completion is installed, presses Tab for a week, and gets
//! nothing, with no error ever having been printed.
//!
//! # What is at stake, which is different from every other verb
//!
//! `--install` writes to a file the user did not hand us. A shell startup file
//! is often years old, hand-edited, and not in version control; a tool that
//! mangles it has destroyed something the user cannot reconstruct. Every design
//! decision below falls out of that single fact:
//!
//!  * **Nothing is written without consent.** [`consent`] requires either `--yes`
//!    on the command line or a `y` typed at a prompt, and REFUSES outright when
//!    stdin is not a terminal — so a CI job, a piped invocation or a
//!    `curl … | sh` cannot edit a profile by accident.
//!  * **Everything written is inside one marked block**, and installing again
//!    REPLACES that block rather than appending a second ([`with_block`]).
//!  * **`--uninstall` restores the file byte for byte**, with one measured
//!    exception recorded on [`with_block`]: a file that did not end in a newline
//!    gains one. Every other shape — CRLF, a UTF-8 BOM, an empty file, a file
//!    holding text that merely LOOKS like the block — round-trips exactly, and
//!    `tests/completion.rs` hashes the bytes to prove it.
//!  * **Ambiguity refuses.** [`resolve_shell`] never guesses, because guessing
//!    wrong writes an activation line into the wrong file — and the wrong file
//!    is somebody's `.zshrc` getting a line of bash.
//!
//! # Where the shapes come from
//!
//! The five activation lines are `clap_complete`'s own, copied out of
//! `clap_complete-4.6.7/src/env/mod.rs` (the "To source your completions"
//! section) with `COMPLETE` replaced by `TASQX_COMPLETE` and `your_program` by
//! `tasqx`. They are NOT one shape with a name swapped, which is the mistake a
//! reader (or an author) working from memory makes: bash and zsh append a source
//! line to an rc file, elvish appends an `eval` to `~/.elvish/rc.elv`, fish
//! writes into its OWN completions directory, and PowerShell appends a
//! three-statement one-liner to `$PROFILE`. Two of the five do not target a
//! shell rc file at all. See [`ACTIVATIONS`].
//!
//! The list of shells is read from `Shells::builtins()` — clap's registry, the
//! same one `complete::intercept` resolves `$TASQX_COMPLETE` against — and
//! [`tests::every_builtin_shell_has_an_activation_line`] fails the build if the
//! two ever disagree. A hand-kept list of five names is the drift shape D30
//! rules against and this repository has paid for repeatedly.

use std::path::{Path, PathBuf};

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

/// Where `--install` writes, per shell. Three shapes, because upstream has three.
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
    /// The shell knows and we do not. PowerShell only.
    ///
    /// `$PROFILE` is a PowerShell *variable*, not an environment variable, and
    /// its value differs between hosts: Windows PowerShell 5.1 uses
    /// `Documents\WindowsPowerShell\`, PowerShell 7 uses `Documents\PowerShell\`,
    /// the ISE has a third profile again, and OneDrive-redirected Documents
    /// folders move all three. `clap_complete` collapses `pwsh`, `powershell`
    /// and `powershell_ise` to the single name `powershell`
    /// (`env/shells.rs`: `EnvCompleter::is`), so by the time the request reaches
    /// this table the host is not even knowable in principle.
    ///
    /// Every way of guessing is a way of writing an activation line into a file
    /// PowerShell will never read — completion silently not working — or, worse,
    /// into the profile of a host the user does not use while they wonder why
    /// nothing happened. So `--install` REFUSES for PowerShell and prints the
    /// one command that cannot be wrong, because the shell that knows the answer
    /// is the one that supplies it:
    ///
    /// ```text
    ///   tasqx completions powershell --install --profile $PROFILE
    /// ```
    ///
    /// PowerShell expands `$PROFILE` into the argument before tasqx starts. The
    /// marked block, the idempotence and the byte-exact `--uninstall` all work
    /// normally from there; the only thing that is refused is the guess.
    OnlyTheHostKnows,
}

/// One shell's activation line and install target.
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
/// [`Target`] beside it. Splitting them that way is what lets one marked-block
/// implementation serve all five.
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

/// Shells a user may plausibly ask for that tasqx deliberately does not serve,
/// each with the reason it is a non-goal rather than an oversight.
///
/// The distinction is the whole point of the table. "unknown shell `cmd`" reads
/// as *tasqx has not got round to it yet*, and a user goes looking for a version
/// that has. The truth is that cmd.exe has no mechanism a program can register a
/// completer with at all — its Tab handling is filename completion built into
/// the console host — so no release will ever add it, and saying so is the
/// answer to the question actually being asked.
///
/// A hand-kept list, and it cannot be otherwise: it names shells clap's registry
/// does NOT contain, so there is nothing to read it out of. What can be checked
/// is that it stays disjoint from that registry —
/// [`tests::no_non_goal_is_a_shell_clap_can_complete`] fails the build if
/// upstream ever adds one of these, which is the moment the row must be deleted
/// rather than left telling users a supported shell is impossible.
const NON_GOALS: &[(&[&str], &str)] = &[
    (
        &["cmd", "command"],
        "cmd.exe has no way for a program to supply completions — its Tab key is \
         filename completion inside the console host, with no hook to register \
         against — so this is a permanent non-goal rather than a gap. Use \
         PowerShell, which tasqx does complete.",
    ),
    (
        &["nu", "nushell"],
        "nushell is a known gap rather than a non-goal: it completes external \
         commands through its own `extern` definitions, and `clap_complete` has \
         no generator for them (`Shells::builtins()` is bash, elvish, fish, \
         powershell and zsh). Nothing tasqx can print today activates it.",
    ),
];

/// The line that opens the block tasqx owns inside a user's startup file.
///
/// The `>>>`/`<<<` shape is conda's, and the borrowing is on purpose: it is the
/// marker millions of shell profiles already carry, so a user opening their
/// `.bashrc` recognises what the block is and that a tool put it there before
/// reading a word of the comment inside it.
///
/// Recognised by WHOLE-LINE equality after trimming, never by `contains`. A
/// profile that holds `echo "# >>> tasqx completions >>>"` — a line about the
/// marker rather than the marker — must not be read as a block boundary, or
/// `--uninstall` would delete from the middle of somebody's script. That is not
/// a hypothetical distinction; it is the difference between a text edit and data
/// loss, and `tests::text_that_merely_mentions_the_marker_is_not_a_block` pins
/// it.
const BEGIN: &str = "# >>> tasqx completions >>>";

/// The line that closes it. See [`BEGIN`].
const END: &str = "# <<< tasqx completions <<<";

/// The comment lines inside the block, between [`BEGIN`] and the activation
/// line.
///
/// A block a user finds in their profile must answer three questions without
/// them going anywhere else: what put this here, how do I get rid of it, and is
/// it safe to just delete. Nothing here carries a version, a date or a path —
/// the block is regenerated verbatim on every `--install`, and a byte that
/// changes between two installs would make the idempotence in [`with_block`] a
/// claim rather than a property.
const PREAMBLE: &[&str] = &[
    "# Added by `tasqx completions --install`. Remove it with",
    "# `tasqx completions --uninstall`, or just delete these five lines.",
];

/// Run the `completions` verb.
///
/// The three modes are one function because they share every decision that can
/// go wrong — which shell, which file, what the line is — and only differ in what
/// they do once those are settled. Splitting them would put [`resolve_shell`]
/// and [`target_path`] behind two call sites each, which is where the two would
/// start disagreeing about what `--profile` means.
pub(crate) fn run(
    shell: Option<String>,
    install: bool,
    uninstall: bool,
    profile: Option<String>,
    yes: bool,
) -> crate::CmdOutcome {
    let activation = resolve_shell(shell.as_deref())?;
    if !install && !uninstall {
        return Ok(printed(activation));
    }
    let path = match &profile {
        Some(p) => PathBuf::from(p),
        None => target_path(activation)?,
    };
    match install {
        true => install_into(activation, &path, yes),
        false => uninstall_from(activation, &path, yes),
    }
}

/// The default mode: write the activation line to stdout and nothing else.
///
/// Exactly one line, with no heading and no advice, because the shape this has
/// to support is `tasqx completions bash >> ~/.bashrc`. Anything else on stdout
/// lands in the user's profile as garbage the shell then tries to run. The
/// guidance a first-time reader needs lives in `tasqx completions -h`, where it
/// cannot be redirected into a file.
///
/// `--json` comes free from the ordinary [`crate::CmdOutcome`] path and carries
/// the target alongside the line, so a dotfile manager can ask tasqx where the
/// line belongs instead of hard-coding five paths of its own.
fn printed(a: &'static Activation) -> (serde_json::Value, String) {
    let target = target_path(a).ok();
    (
        json!({
            "shell": a.shell,
            "line": a.line,
            "target": target.as_ref().map(|p| p.to_string_lossy().into_owned()),
        }),
        format!("{}\n", a.line),
    )
}

/// Which shell, or a refusal that says why.
///
/// # Why an ambiguous answer refuses instead of picking
///
/// The cost of guessing is not a wasted keystroke. `--install` edits a startup
/// file, so a wrong guess appends bash's `source <(…)` to a `.zshrc`, or writes
/// a fish completions file for a user who runs elvish. The user then has a line
/// in a file they did not write, aimed at a shell they do not use, and no
/// completion — a change with no symptom pointing back at its cause. Refusing
/// costs them typing one word.
///
/// # Where the detection comes from
///
/// `$SHELL`, and only `$SHELL`. It is the variable every POSIX login shell sets
/// to its own path, and `file_stem` is applied to it by
/// `complete::canonical_shell_name` exactly as `Shells::completer_for_path`
/// does, so `/usr/bin/zsh` resolves. The parent-process walk that would work on
/// Windows is not attempted: it needs a platform-specific process API, it is
/// wrong whenever tasqx is run from a script, and it would answer for the
/// process that spawned tasqx rather than for the shell whose profile is about
/// to be edited.
///
/// Windows therefore has no detection at all — no Windows shell sets `$SHELL` —
/// and that is stated in the refusal rather than left as a puzzle.
fn resolve_shell(requested: Option<&str>) -> Result<&'static Activation, ApiError> {
    let Some(name) = requested else {
        let from_env = std::env::var("SHELL").unwrap_or_default();
        if from_env.is_empty() {
            return Err(ApiError::bad_request(format!(
                "cannot tell which shell to set up: $SHELL is not set (no Windows \
                 shell sets it, and neither do most non-login sessions). Name it \
                 yourself — one of {} — for example `tasqx completions bash`.",
                known_shells()
            )));
        }
        return activation_for(&from_env).ok_or_else(|| {
            ApiError::bad_request(format!(
                "$SHELL is {from_env:?}, which tasqx cannot complete. {} Name a \
                 shell yourself: one of {}.",
                why_not(&from_env),
                known_shells()
            ))
        });
    };
    activation_for(name).ok_or_else(|| {
        ApiError::bad_request(format!(
            "{} tasqx completes {}.",
            why_not(name),
            known_shells()
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

/// The sentence explaining why `name` is not served — the [`NON_GOALS`] reason
/// when there is one, and a plain statement otherwise.
///
/// Split out because both refusal paths in [`resolve_shell`] need it and the
/// difference between "never will" and "not yet" is the part users act on.
fn why_not(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .unwrap_or(std::ffi::OsStr::new(name))
        .to_string_lossy()
        .to_ascii_lowercase();
    for (spellings, reason) in NON_GOALS {
        if spellings.contains(&stem.as_str()) {
            return (*reason).to_string();
        }
    }
    format!("unknown shell {stem:?}.")
}

/// The five names, as clap spells them, for a refusal message.
fn known_shells() -> String {
    Shells::builtins().names().collect::<Vec<_>>().join(", ")
}

/// The file `--install` would edit for this shell, or a refusal naming what to
/// pass instead.
///
/// Every path here is upstream's; see [`Target`] for what each one is and why
/// PowerShell has none.
fn target_path(a: &'static Activation) -> Result<PathBuf, ApiError> {
    match &a.target {
        Target::UnderHome(segments) => {
            let mut path = home()?;
            for segment in *segments {
                path.push(segment);
            }
            Ok(path)
        }
        Target::FishCompletions => {
            let base = match std::env::var("XDG_CONFIG_HOME") {
                Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
                _ => home()?.join(".config"),
            };
            Ok(base.join("fish").join("completions").join("tasqx.fish"))
        }
        Target::OnlyTheHostKnows => Err(ApiError::bad_request(
            "tasqx will not guess where your PowerShell profile is: $PROFILE is a \
             PowerShell variable rather than an environment variable, and its \
             value differs between Windows PowerShell, PowerShell 7 and the ISE. \
             Let PowerShell answer instead — it expands the path before tasqx \
             starts:\n    tasqx completions powershell --install --profile $PROFILE",
        )),
    }
}

/// The user's home directory, via the same crate that resolves the store's
/// location (`directories`), so this verb and `tasqx config path` cannot
/// disagree about where the user lives.
fn home() -> Result<PathBuf, ApiError> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| {
            ApiError::bad_request(
                "cannot determine your home directory; pass the file to edit with \
                 --profile <PATH>",
            )
        })
}

// ---- the marked block ------------------------------------------------------

/// The block, rendered with `eol` as its line terminator.
///
/// Whole lines, always terminated: the block is defined as a run of complete
/// lines so that [`block_regions`] can find it by line equality and remove it by
/// byte range. A block whose last line lacked its terminator would make removal
/// a special case, and a special case in the removal path is where a byte of
/// somebody's profile goes missing.
fn block(line: &str, eol: &str) -> String {
    let mut out = String::new();
    for text in std::iter::once(BEGIN)
        .chain(PREAMBLE.iter().copied())
        .chain([line, END])
    {
        out.push_str(text);
        out.push_str(eol);
    }
    out
}

/// The line terminator `text` already uses, so an edit does not mix two.
///
/// A single `\r\n` anywhere decides it. That is deliberately eager: a file with
/// mixed endings is already in trouble, and adding LF lines to a mostly-CRLF
/// file is the change most likely to make an editor "fix" the whole file on the
/// next save — which would rewrite bytes tasqx never touched and destroy the
/// byte-exact `--uninstall` this module promises.
///
/// A new or empty file gets `\n`. Not a coin toss: bash, zsh, fish and elvish
/// all treat a trailing `\r` as part of the command, so a CRLF startup file is
/// broken for four of the five shells regardless of platform, and PowerShell
/// reads LF happily.
fn eol_of(text: &str) -> &'static str {
    match text.contains("\r\n") {
        true => "\r\n",
        false => "\n",
    }
}

/// Every line in `text` as `(start, end_including_terminator, content)`.
///
/// Byte offsets rather than a `lines()` iterator because every caller here has
/// to reconstruct the file EXACTLY, and `str::lines` throws away which
/// terminator each line had — which is precisely the information a byte-exact
/// round trip cannot afford to lose.
fn lines_with_spans(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        match text[start..].find('\n') {
            Some(offset) => {
                let end = start + offset + 1;
                out.push((start, end, &text[start..start + offset]));
                start = end;
            }
            None => {
                if start < text.len() {
                    out.push((start, text.len(), &text[start..]));
                }
                return out;
            }
        }
    }
}

/// The byte ranges of every tasqx block in `text`, outermost boundaries
/// included.
///
/// # A dangling begin marker is a refusal, not a guess
///
/// A [`BEGIN`] with no [`END`] after it means somebody edited the file by hand
/// and stopped halfway, or a write was interrupted. The two available guesses
/// are both destructive: treating the block as running to end-of-file deletes
/// every line the user added below it, and ignoring the marker leaves
/// `--install` appending a second block under a broken first one. So neither is
/// taken — the file is left untouched and the user is told which line to look
/// at, which is the only outcome that cannot lose anything.
fn block_regions(text: &str) -> Result<Vec<std::ops::Range<usize>>, ApiError> {
    let lines = lines_with_spans(text);
    let mut out = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    for (index, (start, end, content)) in lines.iter().enumerate() {
        match content.trim() {
            BEGIN if open.is_none() => open = Some((*start, index)),
            END => {
                if let Some((block_start, _)) = open.take() {
                    out.push(block_start..*end);
                }
            }
            _ => {}
        }
    }
    match open {
        Some((_, index)) => Err(ApiError::bad_request(format!(
            "line {} opens a tasqx block ({BEGIN}) that is never closed by \
             {END}. tasqx will not guess where the block ends — deleting to the \
             end of the file would take whatever you added below it. Close or \
             remove that block by hand and run this again.",
            index + 1
        ))),
        None => Ok(out),
    }
}

/// `text` with exactly one tasqx block, carrying `line`.
///
/// # Replace, never append
///
/// The block is rebuilt from scratch and put back where the FIRST existing one
/// was; any further blocks are removed. Two consequences, and both are the
/// point: running `--install` twice leaves one block rather than two, and an
/// activation line left over from an older tasqx (or from a different shell) is
/// corrected in place rather than shadowed by a second line further down the
/// file. Appending would be simpler and would produce a profile that grows a
/// line every time somebody re-runs the setup command they were told to run.
///
/// Position is preserved rather than normalised to the end of the file, because
/// where a line sits in a startup file is semantic — a user who put the block
/// above their `PATH` exports meant to.
///
/// # The one shape that does not round-trip byte for byte, MEASURED
///
/// When the file does not end in a line terminator, one is added before the
/// block, and `--uninstall` cannot take it away again. That is not an oversight
/// that could be fixed with more care: `"A"` (no newline) and `"A\n"` both
/// install to the identical bytes `"A\n<block>"`, so no removal function can
/// distinguish them. Restoring both would require recording the original state
/// somewhere — inside the block, in a comment the user can edit — which trades a
/// missing newline for a file whose correct restoration depends on a user not
/// touching a magic string.
///
/// So the newline is added, this is stated rather than claimed away, and
/// `tests::a_file_with_no_trailing_newline_gains_one` pins the behaviour as
/// observed. It loses nothing: no byte of the user's content is altered, moved
/// or dropped, and appending to a file with no final newline is what every shell
/// `>>` redirection does too — worse, in fact, since `>>` would join the
/// activation line onto the user's last line.
fn with_block(text: &str, line: &str) -> Result<String, ApiError> {
    let regions = block_regions(text)?;
    let eol = eol_of(text);
    let rendered = block(line, eol);

    let Some(first) = regions.first() else {
        let mut out = String::with_capacity(text.len() + rendered.len() + 2);
        out.push_str(text);
        if !text.is_empty() && !text.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&rendered);
        return Ok(out);
    };

    let mut out = String::with_capacity(text.len() + rendered.len());
    out.push_str(&text[..first.start]);
    out.push_str(&rendered);
    let mut cursor = first.end;
    for region in &regions[1..] {
        out.push_str(&text[cursor..region.start]);
        cursor = region.end;
    }
    out.push_str(&text[cursor..]);
    Ok(out)
}

/// `text` with every tasqx block removed, and how many were removed.
///
/// Nothing outside a block is touched — not the surrounding blank lines, not the
/// terminator style, not a BOM. The count is returned rather than a boolean
/// because the caller has to tell "removed something" from "there was nothing
/// here", and answering `ok` to the second is the D33 shape this codebase rules
/// against.
fn without_blocks(text: &str) -> Result<(String, usize), ApiError> {
    let regions = block_regions(text)?;
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for region in &regions {
        out.push_str(&text[cursor..region.start]);
        cursor = region.end;
    }
    out.push_str(&text[cursor..]);
    Ok((out, regions.len()))
}

// ---- reading, consent, writing ---------------------------------------------

/// The file's current contents, or the empty string when it does not exist yet.
///
/// # Why a file that is not UTF-8 is refused rather than handled
///
/// `--install` rewrites the WHOLE file: it reads the bytes, produces new text,
/// and replaces the original. Any lossy step in that round trip silently
/// rewrites bytes the user never asked to change — a `String::from_utf8_lossy`
/// would turn every undecodable byte into `U+FFFD` and hand back a file that
/// looks fine and has been quietly corrupted, which is the exact failure shape
/// this module exists to avoid.
///
/// UTF-16 is the realistic case, not a contrived one: Windows PowerShell 5.1
/// wrote `$PROFILE` as UTF-16LE by default for years. Refusing names the file
/// and leaves it exactly as it was.
///
/// # The NUL check is not belt-and-braces, it is the half `from_utf8` misses
///
/// Written first with `String::from_utf8` alone, and the test built to prove it
/// PASSED against a UTF-16 file — because UTF-16LE of pure ASCII is valid UTF-8.
/// `"# x"` encodes to `23 00 20 00 78 00`, every byte of which is a legal UTF-8
/// scalar; the decode succeeds and hands back a string full of NULs. Appending a
/// UTF-8 block to that and writing it back produces a file PowerShell reads as
/// mojibake from the join onwards, with no error anywhere. What makes the real
/// article refusable is the `FF FE` byte-order mark Windows writes in front of
/// it, and a BOM-less UTF-16 file has nothing but the NULs to give it away.
///
/// So both are checked. No shell startup file contains a NUL byte; a file that
/// does is not the text file this code is prepared to rewrite.
///
/// A UTF-8 BOM is NOT refused — it decodes to `U+FEFF`, survives the round trip
/// as an ordinary character, and `tests::a_bom_survives_the_round_trip` proves
/// the bytes come back identical.
fn read_text(path: &Path) -> Result<String, ApiError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => {
            return Err(ApiError::bad_request(format!(
                "cannot read {}: {e}",
                path.display()
            )))
        }
    };
    let refuse = || {
        ApiError::bad_request(format!(
            "{} is not UTF-8 text (Windows PowerShell 5.1 wrote profiles as \
             UTF-16 by default). tasqx rewrites the whole file when it edits it, \
             and will not re-encode yours behind your back — nothing was \
             changed. Add the line by hand, or re-save the file as UTF-8 first.",
            path.display()
        ))
    };
    let text = String::from_utf8(bytes).map_err(|_| refuse())?;
    match text.contains('\0') {
        true => Err(refuse()),
        false => Ok(text),
    }
}

/// What the user said about writing to their profile.
#[cfg_attr(test, derive(Debug))]
enum Consent {
    /// Go ahead.
    Given,
    /// A human was asked and said no. Not an error: the command did what it was
    /// asked to do, which was to ask.
    Withheld,
}

/// Decide whether the write may happen.
///
/// # Why non-interactive REFUSES instead of defaulting either way
///
/// Defaulting to yes lets any pipeline edit a profile: a `curl | sh` installer,
/// a CI job that runs `tasqx completions --install` to "make sure", a Dockerfile
/// layer. None of those has a human who could have said no. Defaulting to no
/// would be safe and dishonest — the command would exit 0 having done nothing,
/// which is precisely the silent-drop class.
///
/// So the third answer is the right one: refuse, exit non-zero, and name the
/// flag that expresses the consent the missing terminal cannot. `--yes` is not a
/// bypass of the confirmation, it IS the confirmation — typed by the user, on
/// the command line, before anything ran.
///
/// # Why the streams are injected
///
/// Same reason `tui::is_interactive_with` takes them: under a test harness
/// stdout and stdin are always pipes, so the real function can only ever answer
/// one way and the policy would be untestable at exactly the point it matters.
/// The prompt's reader is injected for the same reason — a `y` typed at a
/// terminal is not something an integration test can produce.
fn consent(
    yes: bool,
    stdin_tty: bool,
    ask: impl FnOnce() -> Option<String>,
) -> Result<Consent, ApiError> {
    if yes {
        return Ok(Consent::Given);
    }
    if !stdin_tty {
        return Err(ApiError::bad_request(
            "refusing to edit a startup file without a confirmation: stdin is not \
             a terminal, so there is nobody here to ask. Nothing was written. Pass \
             --yes if you really mean it from a script.",
        ));
    }
    let answer = ask().unwrap_or_default();
    match matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        true => Ok(Consent::Given),
        false => Ok(Consent::Withheld),
    }
}

/// Show the exact bytes about to be added and read the answer from the terminal.
///
/// Everything goes to STDERR. stdout is the command's result — a `--json`
/// caller parses it, and `tasqx completions bash >> ~/.bashrc` redirects it —
/// so a prompt written there would either corrupt the JSON or land in the very
/// file being edited.
fn ask_at_the_terminal(what: &str, path: &Path) -> Option<String> {
    eprintln!("tasqx will change {}:\n\n{what}", path.display());
    eprint!("Continue? [y/N] ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut line = String::new();
    match std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// Replace `path`'s contents with `text`, without a window in which the file is
/// half-written.
///
/// Written to a sibling temp file and renamed over the original, because the
/// alternative — truncate and write — leaves a shell startup file EMPTY if the
/// process dies, the disk fills, or the machine loses power in between. A
/// truncated `.bashrc` is the loss this whole module is arranged to prevent, and
/// it would be caused by the one line that does the saving.
///
/// The temp file is a sibling rather than in the system temp directory so the
/// rename stays within one filesystem; `std::fs::rename` across mount points
/// fails, and on Windows it replaces the destination atomically only on the same
/// volume.
fn write_atomically(path: &Path, text: &str) -> Result<(), ApiError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ApiError::bad_request(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
    }
    let mut temp = path.to_path_buf();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    temp.set_file_name(format!(".{name}.tasqx-{}", std::process::id()));
    std::fs::write(&temp, text)
        .map_err(|e| ApiError::bad_request(format!("cannot write {}: {e}", temp.display())))?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        ApiError::bad_request(format!("cannot replace {}: {e}", path.display()))
    })
}

/// `--install`: put the block in `path`, asking first.
///
/// The already-correct case exits 0 and says so without touching the file. That
/// is not a D33 violation dressed up: the command's contract is "the block is
/// present and current", the postcondition holds, and the message states plainly
/// that nothing was written rather than implying an edit. Rewriting identical
/// bytes would change the file's mtime for nothing, which is a real cost in a
/// directory people back up and sync.
fn install_into(a: &'static Activation, path: &Path, yes: bool) -> crate::CmdOutcome {
    let current = read_text(path)?;
    let wanted = with_block(&current, a.line)?;
    if wanted == current {
        return Ok((
            json!({
                "shell": a.shell, "path": path.to_string_lossy(),
                "action": "install", "changed": false,
            }),
            format!(
                "{} already carries the current tasqx completion block; nothing \
                 was written.\n",
                path.display()
            ),
        ));
    }

    let preview = block(a.line, eol_of(&current));
    let stdin_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if let Consent::Withheld = consent(yes, stdin_tty, || ask_at_the_terminal(&preview, path))? {
        return Ok((
            json!({
                "shell": a.shell, "path": path.to_string_lossy(),
                "action": "install", "changed": false,
            }),
            "nothing was written.\n".to_string(),
        ));
    }
    write_atomically(path, &wanted)?;
    Ok((
        json!({
            "shell": a.shell, "path": path.to_string_lossy(),
            "action": "install", "changed": true,
        }),
        format!(
            "{} completion installed in {}.\nOpen a new shell to use it.\n",
            a.shell,
            path.display()
        ),
    ))
}

/// `--uninstall`: take the block back out of `path`, asking first.
///
/// # Nothing to remove is exit 4, not a cheerful zero
///
/// D33: a command that changed nothing must not answer `ok`. "Removed the tasqx
/// block" printed over a file that never had one teaches the user something
/// false, and the case where it matters is the one where it is most likely — a
/// `--uninstall` aimed at the wrong file, or at a profile whose block a dotfile
/// manager already rewrote. Answering success there means the user believes the
/// line is gone while it sits in another file still activating completion.
///
/// So it is a `not_found` (exit 4) naming the file, and the file is not
/// rewritten at all — no mtime change, no temp file, no chance of a write
/// failing on a file nobody needed to touch.
fn uninstall_from(a: &'static Activation, path: &Path, yes: bool) -> crate::CmdOutcome {
    let current = read_text(path)?;
    let (wanted, removed) = without_blocks(&current)?;
    if removed == 0 {
        return Err(ApiError::not_found(
            format!(
                "no tasqx completion block in {} — nothing was removed and the \
                 file is unchanged. If completion still works, the activation \
                 line is in another file (or was added by hand); `tasqx \
                 completions {}` prints the line to look for.",
                path.display(),
                a.shell
            ),
            None,
        ));
    }

    let preview = format!(
        "the tasqx completion block ({removed} occurrence(s)) will be removed \
         from this file.\n"
    );
    let stdin_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if let Consent::Withheld = consent(yes, stdin_tty, || ask_at_the_terminal(&preview, path))? {
        return Ok((
            json!({
                "shell": a.shell, "path": path.to_string_lossy(),
                "action": "uninstall", "changed": false,
            }),
            "nothing was written.\n".to_string(),
        ));
    }
    write_atomically(path, &wanted)?;
    Ok((
        json!({
            "shell": a.shell, "path": path.to_string_lossy(),
            "action": "uninstall", "changed": true, "removed": removed,
        }),
        format!("tasqx completion removed from {}.\n", path.display()),
    ))
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

    /// A [`NON_GOALS`] entry claims a shell will never be served. The day clap
    /// gains it, that claim becomes a lie the tool tells while refusing a
    /// feature it has.
    #[test]
    fn no_non_goal_is_a_shell_clap_can_complete() {
        for (spellings, _) in NON_GOALS {
            for spelling in *spellings {
                assert!(
                    Shells::builtins().completer(spelling).is_none(),
                    "`{spelling}` is listed as a shell tasqx does not serve, but \
                     `Shells::builtins()` has a completer for it — delete the \
                     NON_GOALS row and add an ACTIVATIONS one"
                );
            }
        }
    }

    /// The cmd.exe refusal must say cmd.exe is a non-goal, not that the shell is
    /// unknown. "unknown shell" reads as *not yet*, and sends a Windows user
    /// looking for a newer tasqx that will never exist.
    #[test]
    fn cmd_is_refused_as_a_non_goal_rather_than_as_an_unknown_shell() {
        let err = resolve_shell(Some("cmd")).expect_err("cmd.exe is not completable");
        assert_eq!(err.exit_code(), 2, "an unsupported shell is a usage error");
        let message = err.message.to_lowercase();
        assert!(
            message.contains("cmd.exe") && message.contains("non-goal"),
            "the message must say cmd.exe is a permanent non-goal: {message}"
        );
        assert!(
            message.contains("powershell"),
            "it must point at the Windows shell that DOES complete: {message}"
        );
        // `cmd.exe` spelled with its extension resolves to the same row.
        assert!(resolve_shell(Some("cmd.exe"))
            .expect_err("cmd.exe is not completable")
            .message
            .to_lowercase()
            .contains("non-goal"));
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

    /// PowerShell's install target is a refusal, and the refusal must carry the
    /// command that works. A user told only "cannot determine your profile" has
    /// been given a dead end; `$PROFILE` in an argument is expanded by the shell
    /// that knows it.
    #[test]
    fn powershell_refuses_to_guess_a_profile_and_says_what_to_run_instead() {
        let a = resolve_shell(Some("powershell")).unwrap();
        let err = target_path(a).expect_err("no host-independent PowerShell profile exists");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.message.contains("--profile $PROFILE"),
            "the refusal must hand over the working command: {}",
            err.message
        );
    }

    /// The four shells whose target IS knowable must produce a path — a refusal
    /// there would be this feature quietly not existing on Linux and macOS.
    #[test]
    fn every_other_shell_names_a_file_to_edit() {
        for a in ACTIVATIONS {
            if matches!(a.target, Target::OnlyTheHostKnows) {
                continue;
            }
            let path = target_path(a).unwrap_or_else(|e| panic!("{}: {}", a.shell, e.message));
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
    /// platform; the XDG half is checked through the same function with the
    /// variable injected, because the env is process-global and this test binary
    /// runs in parallel threads.
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

    // ---- the marked block --------------------------------------------------

    /// The property the whole module is arranged around, on the shape that is
    /// most common: a file that already has content and ends with a newline.
    #[test]
    fn install_then_uninstall_restores_the_bytes_exactly() {
        let original = "export PATH=$PATH:/opt/bin\nalias ll='ls -l'\n";
        let installed = with_block(original, "source <(TASQX_COMPLETE=bash tasqx)").unwrap();
        assert_ne!(installed, original, "the install must change something");
        let (restored, removed) = without_blocks(&installed).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            restored, original,
            "uninstall must restore the file byte for byte"
        );
    }

    /// CRLF is not a curiosity here: `$PROFILE` on Windows is a CRLF file, and
    /// the whole PowerShell half of this feature edits one. A block written with
    /// LF into a CRLF file would also invite the next editor that opens it to
    /// normalise the entire file, rewriting bytes tasqx never touched.
    #[test]
    fn a_crlf_file_keeps_its_line_endings_and_round_trips() {
        let original = "$env:EDITOR = 'vim'\r\nSet-Alias ll Get-ChildItem\r\n";
        let installed = with_block(original, "$env:TASQX_COMPLETE = \"powershell\"").unwrap();
        assert!(
            !installed.replace("\r\n", "").contains('\n'),
            "the block introduced a bare LF into a CRLF file: {installed:?}"
        );
        assert_eq!(without_blocks(&installed).unwrap().0, original);
    }

    /// An absent or empty file installs to the block alone, with no leading
    /// blank line, and uninstalls back to empty. This is fish's ordinary case —
    /// `completions/tasqx.fish` does not exist until tasqx creates it.
    #[test]
    fn an_empty_file_round_trips_and_gains_no_leading_blank_line() {
        let installed = with_block("", "TASQX_COMPLETE=fish tasqx | source").unwrap();
        assert!(
            installed.starts_with(BEGIN),
            "an empty file must not gain a leading blank line: {installed:?}"
        );
        assert_eq!(without_blocks(&installed).unwrap().0, "");
    }

    /// A UTF-8 BOM is content, not framing: it decodes to one ordinary character
    /// and must come back untouched. Windows editors write it into profiles
    /// routinely.
    #[test]
    fn a_bom_survives_the_round_trip() {
        let original = "\u{feff}# my profile\r\n";
        let installed = with_block(original, "line").unwrap();
        assert!(installed.starts_with('\u{feff}'));
        assert_eq!(without_blocks(&installed).unwrap().0, original);
    }

    /// The measured exception, pinned as observed rather than claimed away.
    ///
    /// `"A"` and `"A\n"` install to identical bytes, so no uninstall can restore
    /// both; the newline is added and stays. See [`with_block`] for why encoding
    /// the original state inside the block was rejected. Nothing of the user's
    /// content is altered, moved or lost — the file gains exactly one byte at
    /// its end.
    #[test]
    fn a_file_with_no_trailing_newline_gains_one() {
        let original = "alias ll='ls -l'";
        let installed = with_block(original, "line").unwrap();
        let (restored, removed) = without_blocks(&installed).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            restored, "alias ll='ls -l'\n",
            "the documented exception: a missing final newline is added and \
             cannot be taken back"
        );
        assert!(
            restored.starts_with(original),
            "every byte the user wrote must still be there, in order"
        );
    }

    /// Installing twice leaves exactly ONE block, in the position the first one
    /// had. Appending is the obvious implementation and would grow a line into
    /// the user's profile every time they re-ran the command the setup docs tell
    /// them to run.
    #[test]
    fn installing_twice_leaves_exactly_one_block_where_the_first_one_was() {
        let original = "first\nsecond\n";
        let once = with_block(original, "line").unwrap();
        let twice = with_block(&once, "line").unwrap();
        assert_eq!(once, twice, "a second install must be a no-op");
        assert_eq!(block_regions(&twice).unwrap().len(), 1);
        // And the block stayed at the end, where the first install put it —
        // rather than being lifted out and re-appended.
        assert!(twice.starts_with("first\nsecond\n"));
    }

    /// An activation line that changed — a new tasqx, or the same file being
    /// pointed at a different shell — is CORRECTED in place, not shadowed by a
    /// second block further down. A stale line left above a fresh one is a shell
    /// running both.
    #[test]
    fn a_stale_activation_line_is_replaced_rather_than_shadowed() {
        let once = with_block("keep me\n", "OLD_LINE").unwrap();
        let twice = with_block(&once, "NEW_LINE").unwrap();
        assert!(twice.contains("NEW_LINE"));
        assert!(
            !twice.contains("OLD_LINE"),
            "the stale line survived: {twice}"
        );
        assert_eq!(block_regions(&twice).unwrap().len(), 1);
        assert_eq!(without_blocks(&twice).unwrap().0, "keep me\n");
    }

    /// A file that somehow collected two blocks (a hand edit, a dotfile manager,
    /// a merge) collapses to one, and the content between them is kept.
    #[test]
    fn several_blocks_collapse_to_one_without_losing_what_sat_between_them() {
        let mut text = with_block("top\n", "line").unwrap();
        text.push_str("middle\n");
        text = text.replace("middle\n", &format!("{}middle\n", block("line", "\n")));
        assert_eq!(block_regions(&text).unwrap().len(), 2);

        let collapsed = with_block(&text, "line").unwrap();
        assert_eq!(block_regions(&collapsed).unwrap().len(), 1);
        assert!(collapsed.contains("middle\n"), "{collapsed}");
        assert_eq!(without_blocks(&collapsed).unwrap().0, "top\nmiddle\n");
    }

    /// Text that MENTIONS the marker is not the marker. The boundary is
    /// whole-line equality after trimming, because a `contains` test would let
    /// `--uninstall` cut from the middle of somebody's script.
    ///
    /// The conda block is here for the same reason: `>>> … >>>` is a shape other
    /// tools use, and only the words between the arrows make a block ours.
    #[test]
    fn text_that_merely_mentions_the_marker_is_not_a_block() {
        let original = concat!(
            "echo \"# >>> tasqx completions >>>\"\n",
            "# >>> conda initialize >>>\n",
            "eval \"$(conda shell.bash hook)\"\n",
            "# <<< conda initialize <<<\n",
        );
        assert!(
            block_regions(original).unwrap().is_empty(),
            "no tasqx block is present here"
        );
        let installed = with_block(original, "line").unwrap();
        assert_eq!(block_regions(&installed).unwrap().len(), 1);
        let (restored, removed) = without_blocks(&installed).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            restored, original,
            "the conda block and the echoed marker must be untouched"
        );
    }

    /// Indentation does not hide a block from its own uninstall: the markers are
    /// compared after trimming, so a user (or an editor) that indented the block
    /// still gets it removed rather than left behind with a success message.
    #[test]
    fn an_indented_block_is_still_recognised() {
        let text = format!("  {BEGIN}\n  line\n\t{END}\nafter\n");
        let (restored, removed) = without_blocks(&text).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(restored, "after\n");
    }

    /// A begin marker with no end marker is a refusal, and the file is untouched.
    ///
    /// Both alternatives destroy something: deleting to end-of-file takes
    /// whatever the user wrote below the broken block, and ignoring the marker
    /// leaves `--install` stacking a second block under a broken first one.
    #[test]
    fn an_unterminated_block_refuses_rather_than_guessing_where_it_ends() {
        let text = format!("{BEGIN}\nline\nimportant user content\n");
        let err = block_regions(&text).expect_err("a dangling begin marker must refuse");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.message.contains("line 1"),
            "the refusal must name the line to look at: {}",
            err.message
        );
        // Both editing paths inherit the refusal — neither may quietly proceed.
        assert!(with_block(&text, "line").is_err());
        assert!(without_blocks(&text).is_err());
    }

    /// A block at the very end of a file whose last line lost its terminator
    /// still uninstalls cleanly. Hand-edited files end this way often enough
    /// that a panic or a leftover marker here would be a real report.
    #[test]
    fn a_block_whose_end_marker_lacks_a_terminator_is_still_removed() {
        let text = format!("before\n{BEGIN}\nline\n{END}");
        let (restored, removed) = without_blocks(&text).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(restored, "before\n");
    }

    // ---- consent -----------------------------------------------------------

    /// The refusal that keeps a pipeline out of a user's profile. Asserted as
    /// exit 2 and a message naming the flag, because a script author reading it
    /// needs to know there is a way to say yes on purpose.
    #[test]
    fn a_non_interactive_install_refuses_instead_of_assuming_yes() {
        let err = consent(false, false, || panic!("must not prompt with no terminal"))
            .expect_err("a pipeline must not be able to edit a profile");
        assert_eq!(err.exit_code(), 2);
        assert!(
            err.message.contains("--yes") && err.message.contains("Nothing was written"),
            "{}",
            err.message
        );
    }

    /// `--yes` is the confirmation, so it does not prompt — and it does not need
    /// a terminal, which is what makes it usable from the automation the refusal
    /// above turns away.
    #[test]
    fn the_yes_flag_is_itself_the_confirmation() {
        assert!(matches!(
            consent(true, false, || panic!("--yes must not prompt")),
            Ok(Consent::Given)
        ));
    }

    /// A human at a terminal decides, and only an explicit yes counts. Every
    /// other answer — including an empty line, which is what Enter produces —
    /// leaves the file alone, which is why the prompt reads `[y/N]`.
    #[test]
    fn only_an_explicit_yes_at_a_terminal_permits_the_write() {
        for answer in ["y\n", "Y\n", "yes\n", "  yes  \n"] {
            assert!(
                matches!(
                    consent(false, true, || Some(answer.to_string())),
                    Ok(Consent::Given)
                ),
                "{answer:?} is a yes"
            );
        }
        for answer in ["", "\n", "n\n", "no\n", "later\n", "ye\n"] {
            assert!(
                matches!(
                    consent(false, true, || Some(answer.to_string())),
                    Ok(Consent::Withheld)
                ),
                "{answer:?} is not a yes"
            );
        }
        // A closed stdin (EOF) is not consent either.
        assert!(matches!(
            consent(false, true, || None),
            Ok(Consent::Withheld)
        ));
    }

    // ---- the whole verb ----------------------------------------------------

    /// A scratch path this test binary owns, unique per call. Nothing in this
    /// module's tests may go near a real startup file.
    fn scratch(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tasqx-install-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        dir.join("profile")
    }

    /// The end-to-end round trip against a real file on disk, through the same
    /// entry point the CLI calls.
    ///
    /// The in-memory tests above prove the string transformation; this proves the
    /// bytes that reach the filesystem, which is where the atomic rename, the
    /// UTF-8 decode and the encoding of the write all live. A transformation that
    /// is exact and a writer that is not would pass every test above.
    #[test]
    fn the_verb_round_trips_a_real_file_byte_for_byte() {
        let path = scratch("roundtrip");
        let original = b"# my rc\nexport EDITOR=vim\n".to_vec();
        std::fs::write(&path, &original).expect("seed the profile");

        let profile = Some(path.to_string_lossy().into_owned());
        run(
            Some("bash".into()),
            true,
            false,
            profile.clone(),
            /* yes */ true,
        )
        .expect("install");
        assert_ne!(std::fs::read(&path).unwrap(), original, "nothing was added");

        run(Some("bash".into()), false, true, profile, true).expect("uninstall");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the file must come back byte for byte"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// `--uninstall` over a file with no block changes nothing and says so with
    /// a non-zero exit (D33). The file must not even be rewritten — an identical
    /// rewrite still moves the mtime, which is a visible event in a directory
    /// people sync and back up.
    #[test]
    fn uninstalling_nothing_is_a_loud_failure_that_leaves_the_file_alone() {
        let path = scratch("nothing");
        std::fs::write(&path, b"# untouched\n").expect("seed the profile");
        let before = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        let err = run(
            Some("bash".into()),
            false,
            true,
            Some(path.to_string_lossy().into_owned()),
            true,
        )
        .expect_err("removing nothing is not a success");
        assert_eq!(err.exit_code(), 4, "not_found, not a cheerful zero");
        assert!(
            err.message.contains("nothing was removed"),
            "{}",
            err.message
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"# untouched\n");
        assert_eq!(
            std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
            before,
            "the file was rewritten although nothing changed"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A second `--install` reports that it changed nothing, and leaves the file
    /// alone rather than rewriting identical bytes.
    #[test]
    fn a_second_install_reports_no_change_and_does_not_rewrite() {
        let path = scratch("idempotent");
        let profile = Some(path.to_string_lossy().into_owned());
        run(Some("zsh".into()), true, false, profile.clone(), true).expect("install");
        let after_first = std::fs::read(&path).unwrap();
        let before = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        let (json, text) = run(Some("zsh".into()), true, false, profile, true).expect("re-install");
        assert_eq!(json["changed"], false);
        assert!(text.contains("nothing was written"), "{text}");
        assert_eq!(std::fs::read(&path).unwrap(), after_first);
        assert_eq!(
            std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
            before
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The default mode writes exactly the activation line and nothing else, so
    /// `tasqx completions bash >> ~/.bashrc` is a working setup command rather
    /// than a way to put a banner in a startup file.
    #[test]
    fn printing_emits_exactly_the_activation_line() {
        let (json, text) = run(Some("bash".into()), false, false, None, false).expect("print");
        assert_eq!(text, "source <(TASQX_COMPLETE=bash tasqx)\n");
        assert_eq!(json["shell"], "bash");
        assert_eq!(json["line"], "source <(TASQX_COMPLETE=bash tasqx)");
        assert_eq!(text.lines().count(), 1, "one line, no advice: {text:?}");
    }

    /// A file tasqx cannot decode is refused with nothing written. Rewriting the
    /// whole file is what install does, so a lossy read would hand back a
    /// silently re-encoded profile that looks fine.
    ///
    /// Both spellings of the real article are exercised, and the second is why
    /// this test exists in this form. With the BOM, `String::from_utf8` refuses
    /// and the guard is easy. WITHOUT it, UTF-16LE of ASCII is valid UTF-8 —
    /// `23 00 20 00 78 00` decodes cleanly — so the first version of this test
    /// used a BOM-less fixture, PASSED nothing, and reported success while the
    /// code happily appended a UTF-8 block to a UTF-16 file. The NUL check in
    /// [`read_text`] is what closed it.
    #[test]
    fn a_file_that_is_not_utf8_is_refused_with_nothing_written() {
        // "# x\r\n" as UTF-16LE, which is what Windows PowerShell 5.1 wrote.
        let utf16: Vec<u8> = "# x\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let with_bom: Vec<u8> = [0xff, 0xfe].iter().copied().chain(utf16.clone()).collect();

        for (label, original) in [("utf16-bom", with_bom), ("utf16-bare", utf16)] {
            let path = scratch(label);
            std::fs::write(&path, &original).expect("seed the profile");

            let err = run(
                Some("powershell".into()),
                true,
                false,
                Some(path.to_string_lossy().into_owned()),
                true,
            )
            .unwrap_err();
            assert_eq!(err.exit_code(), 2, "{label}");
            assert!(err.message.contains("UTF-8"), "{label}: {}", err.message);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                original,
                "{label}: the file must be exactly as it was"
            );
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }
}
