//! The argv pre-pass that makes `-tag` typable without disarming clap.
//!
//! `-tag` is core filter grammar (`filter.rs`), so the exclusion half of the
//! tag predicate begins with a hyphen and clap reads it as an unknown flag:
//! `tasqx list -needs` never reached the parser at all. The obvious fix,
//! `allow_hyphen_values` on the filter positionals, buys that at a price the
//! name hides. It does not mean "let a leading hyphen through when it is not a
//! flag"; it means "once this positional starts consuming, every remaining
//! hyphen token is one of its values" — and clap does not exempt its OWN
//! declared flags. So `tasqx list @working --json` stopped emitting JSON and
//! started failing, because `--json` arrived at the filter grammar as text.
//!
//! There is no clap setting that does both: `trailing_var_arg` is strictly
//! worse (it takes the whole tail raw), and `num_args` does not change how a
//! leading hyphen is classified. The two properties are only separable by
//! deciding, before clap sees the tokens, which hyphen tokens are filter text.
//!
//! That decision is already made, and is already the documented grammar: a tag
//! name may not begin with `-`, so a SINGLE dash is a tag exclusion and a
//! DOUBLE dash is a flag (`filter.rs::predicate` refuses `--x` for exactly this
//! reason). So the pre-pass hides the leading dash of single-dash tokens behind
//! [`ESCAPED_DASH`] for the duration of the parse and restores it afterwards.
//! Clap then sees an ordinary positional value and keeps full authority over
//! every `--flag`: known ones parse as flags in any position, unknown ones are
//! still rejected rather than silently becoming filter text that widens the
//! result set.
//!
//! The trade-off this accepts: a filter token that genuinely begins with `--`
//! remains untypable. It always was — `filter.rs` says so in the grammar and in
//! its error — so nothing that ever worked is lost.
//!
//! One rule makes the escape safe, and it is the whole of C7: a token is only
//! escaped if it will reach the filter tail. [`unescape`] runs there and
//! nowhere else, so escaping a token clap consumes as a FLAG VALUE strands the
//! sentinel in a value the user then sees (`--theme -nord` was answered
//! `unknown theme "\u{1}nord"`). The alternative — unescaping at every site a
//! value can land — is the hand-maintained registry shape that has already
//! leaked twice here, and the next flag re-opens it. So the escape is narrowed
//! at the source instead, from clap's own arg table, which no new flag can be
//! declared outside of. `no_flag_value_is_ever_escaped` asserts the property
//! over every value-taking flag the filter commands declare.

use std::ffi::OsString;

use clap::CommandFactory;

use crate::Cli;

/// Stands in for the leading `-` of a filter token while clap parses.
///
/// U+0001 and not something typeable: whatever we choose, a user who types it
/// literally gets it turned back into a dash. A control character no shell
/// produces by accident makes that case unreachable in practice.
const ESCAPED_DASH: char = '\u{1}';

/// Subcommands whose positional tail is filter DSL, by canonical name.
///
/// Aliases are resolved through clap, not listed here, so `ls` and `l` follow
/// `list` automatically. Kept honest by `every_filter_positional_is_registered`.
pub(crate) const FILTER_COMMANDS: [&str; 4] = ["list", "export", "report", "watch"];

/// argv rewritten for clap, plus whether it addresses a filter-taking command.
pub struct Prepass {
    pub argv: Vec<OsString>,
    /// Drives the error message for an unknown flag: only these commands can
    /// sensibly be told about `-tag`.
    pub filter_command: bool,
}

/// Hide the leading dash of filter tokens so clap keeps judging real flags.
pub fn prepass<I: IntoIterator<Item = OsString>>(raw: I) -> Prepass {
    let mut argv: Vec<OsString> = raw.into_iter().collect();
    let mut cmd = Cli::command();
    // Globals are attached to subcommands during build; without this the
    // root-flag lookup below would miss `--theme`'s value and mistake it for
    // the subcommand.
    cmd.build();

    let Some(i) = subcommand_index(&cmd, &argv) else {
        return Prepass {
            argv,
            filter_command: false,
        };
    };
    let Some(name) = canonical_name(&cmd, &argv[i]) else {
        return Prepass {
            argv,
            filter_command: false,
        };
    };
    if !FILTER_COMMANDS.contains(&name.as_str()) {
        return Prepass {
            argv,
            filter_command: false,
        };
    }

    let sub = cmd
        .find_subcommand(&name)
        .expect("canonical_name resolved it from this tree");
    // Escape only the tokens that will reach the filter tail. A token clap
    // consumes as a FLAG VALUE must be left alone: `unescape` runs on the tail
    // and nothing else, so an escaped value keeps its sentinel all the way to
    // the user (`--theme -nord` answered `unknown theme "\u{1}nord"`). Which
    // tokens those are is read out of clap's own arg table here rather than
    // listed, so a flag added tomorrow is handled the day it is declared.
    let mut j = i + 1;
    while j < argv.len() {
        // A non-UTF-8 token is neither a flag we know nor a tag; step over it.
        let Some(tok) = argv[j].to_str().map(str::to_string) else {
            j += 1;
            continue;
        };
        if tok == "--" {
            // Clap stops classifying after this, so every later dash already
            // reaches the positional intact and escaping would only add work.
            break;
        }
        if let Some(long) = tok.strip_prefix("--") {
            // `--flag=value` carries its value inside the token; only the
            // separated spelling puts it in the NEXT one.
            if !long.contains('=') && long_value_follows(sub, long) {
                j += 1;
            }
        } else if let Some(short) = declared_short(sub, &tok) {
            // Clap's own short flag, left alone for the same reason a `--flag`
            // is: escaping it made it unreachable. `-h` is the case that bit —
            // `tasqx list -h` ran the command and `tasqx export -h` DUMPED THE
            // STORE instead of printing help, because the help flag was hidden
            // behind the sentinel and clap never saw it. Read out of clap's arg
            // table rather than spelled `-h` here: a hardcoded exception is the
            // hand-maintained registry shape that produced this bug in the
            // first place (D30 — when a fix can be "derive it from clap", do).
            // The trade, which the dash grammar already implies: a tag whose
            // name is exactly one character that clap declares as a short flag
            // is not excludable from the CLI. `-h` was never a usable tag
            // exclusion anyway, and the API path takes the filter verbatim.
            if short_takes_value(sub, short) {
                j += 1;
            }
        } else if is_tag_exclusion(&tok) {
            argv[j] = OsString::from(escaped(&tok));
        }
        j += 1;
    }
    Prepass {
        argv,
        filter_command: true,
    }
}

/// Will clap take the NEXT token as this long flag's value?
///
/// `require_equals` flags are excluded because their value can only ride in the
/// same token, so the next one is a positional and must still be escaped. An
/// unknown long is not a flag at all; clap rejects it either way, and treating
/// it as valueless keeps a following `-tag` typable.
fn long_value_follows(cmd: &clap::Command, long: &str) -> bool {
    cmd.get_arguments()
        .find(|a| {
            a.get_long() == Some(long) || a.get_all_aliases().is_some_and(|v| v.contains(&long))
        })
        .is_some_and(|a| takes_value(a) && !a.is_require_equals_set())
}

/// Hide the leading dash of ONE filter token: the inverse of [`unescaped`].
///
/// Lifted out of [`prepass`]'s loop for the reason [`unescaped`] gives for being
/// one function with two callers — the sentinel rule exists once, so nothing can
/// drift away from [`ESCAPED_DASH`]. The second caller is `complete`'s
/// `escaping_drift` guard, which drives every completer attached to a filter
/// positional with an escaped word and checks the dash came back. A guard that
/// spelled the escaped form itself would agree with a broken [`ESCAPED_DASH`] by
/// construction and prove nothing.
///
/// Tokens [`is_tag_exclusion`] rejects are returned unchanged. Escaping a `--`
/// flag or a bare `-` would hand it a sentinel that [`unescape`] never strips
/// back off, which is the failure mode the caller's `else if` already avoids;
/// stating it here keeps a second caller from having to know that.
pub fn escaped(tok: &str) -> String {
    match is_tag_exclusion(tok) {
        true => format!("{ESCAPED_DASH}{}", &tok[1..]),
        false => tok.to_string(),
    }
}

/// Restore the dash [`prepass`] hid on ONE token.
///
/// The whole of the sentinel's inverse, in one place, because there are now two
/// callers with nothing else in common and a second copy of this three-line rule
/// would be a second thing to keep in step with [`ESCAPED_DASH`]:
///
///  * [`unescape`], on the filter tail clap has finished parsing (the command
///    path);
///  * `complete::escaped_word_completer`, on the single partial word the
///    completion engine hands a candidate provider (the Tab path).
///
/// The second exists because escaping and restoring must stay symmetric on BOTH
/// paths. `run()` restores what it parsed; the completion seam has to restore
/// what it is about to match against, or a provider sees `\u{1}ne` where the
/// user typed `-ne` and matches nothing.
pub fn unescaped(tok: &str) -> String {
    match tok.strip_prefix(ESCAPED_DASH) {
        Some(rest) => format!("-{rest}"),
        None => tok.to_string(),
    }
}

/// Restore the dashes [`prepass`] hid, in place, on a parsed filter tail.
///
/// Every hyphen-tolerant positional must be run through this before its value
/// is used; missing one leaves a control character in the filter string and the
/// token fails as unknown. `run()` does them all in one place for that reason.
pub fn unescape(tokens: &mut [String]) {
    for tok in tokens {
        *tok = unescaped(tok);
    }
}

/// Would `tok` reach a filter positional as filter text, or does [`prepass`]
/// hand it to clap as a flag?
///
/// **The authority on that question is this module, not the filter grammar**,
/// and the difference is a silent drop. `filter::Filter::parse` accepts `-h` as
/// a tag exclusion — it is valid filter grammar and the JSON API takes it
/// verbatim — but on the CLI the loop above deliberately leaves a one-character
/// dash token alone when clap declares that letter, so `-h` reaches clap as the
/// HELP FLAG. That trade is stated at the site and is right; what was missing is
/// that anything COMPOSING a filter token for the user has to know about it.
///
/// The shell completer did not, and the result was measured: with a task tagged
/// `h`, `tasqx list -<TAB>` offered `-h`, and choosing it printed the help text
/// at exit 0 instead of filtering. The completer had gated the candidate on the
/// filter parser, which is the wrong parser — the same shape as the sugar-path
/// defect where a candidate was gated on a character allowlist instead of
/// `sugar::parsed_value_of`.
///
/// The cheap test first: everything that is not a one-character dash token
/// reaches the tail, so the clap tree is built only for the rare word that could
/// collide. Membership is the UNION over [`FILTER_COMMANDS`] rather than one
/// subcommand, because a candidate provider is handed the word and nothing else.
/// Today the union is exact — `-h` and `-V` are declared on all four — and if
/// that ever stops being true this errs toward withholding a candidate rather
/// than offering one that runs the wrong command.
pub(crate) fn reaches_the_filter_tail(tok: &str) -> bool {
    // Not dash-led, or longer than one character after the dash: the loop above
    // escapes it, so it reaches the positional intact. No tree needed.
    let Some(rest) = tok.strip_prefix('-') else {
        return true;
    };
    if rest.chars().count() != 1 {
        return true;
    }
    let mut cmd = Cli::command();
    cmd.build();
    !FILTER_COMMANDS.iter().any(|name| {
        cmd.find_subcommand(name)
            .is_some_and(|sub| declared_short(sub, tok).is_some())
    })
}

/// The short flag `tok` names, if `cmd` declares one by that letter.
///
/// Deliberately exact-match on a SINGLE character: `-needs` is a tag exclusion
/// even where `-n` is a flag, because the dash grammar says a one-dash token is
/// filter text and clusters are not part of it. Only the spelling a user
/// reaching for a flag actually types is handed back to clap.
fn declared_short(cmd: &clap::Command, tok: &str) -> Option<char> {
    let mut chars = tok.strip_prefix('-')?.chars();
    let (c, None) = (chars.next()?, chars.next()) else {
        return None;
    };
    cmd.get_arguments()
        .any(|a| a.get_short() == Some(c))
        .then_some(c)
}

/// `-x…`: a tag exclusion. `--x` is a flag (real or mistyped) and stays clap's
/// to judge; a bare `-` is the stdin sentinel `import` uses.
fn is_tag_exclusion(s: &str) -> bool {
    s.len() > 1 && s.starts_with('-') && !s.starts_with("--")
}

/// Index of the subcommand token, skipping global flags and their values.
fn subcommand_index(cmd: &clap::Command, argv: &[OsString]) -> Option<usize> {
    let mut i = 1;
    while i < argv.len() {
        // A non-UTF-8 token cannot be a flag we know; let clap have it.
        let Some(tok) = argv[i].to_str() else {
            return Some(i);
        };
        if tok == "--" {
            return (i + 1 < argv.len()).then_some(i + 1);
        }
        if let Some(long) = tok.strip_prefix("--") {
            match long.split_once('=') {
                Some(_) => i += 1,
                None => i += 1 + usize::from(long_takes_value(cmd, long)),
            }
        } else if is_tag_exclusion(tok) {
            // A short flag, possibly a cluster; only the last one can take a value.
            let last = tok.chars().next_back().unwrap_or_default();
            i += 1 + usize::from(short_takes_value(cmd, last));
        } else {
            return Some(i);
        }
    }
    None
}

fn takes_value(arg: &clap::Arg) -> bool {
    arg.get_num_args().is_some_and(|r| r.takes_values())
}

fn long_takes_value(cmd: &clap::Command, long: &str) -> bool {
    cmd.get_arguments()
        .find(|a| {
            a.get_long() == Some(long) || a.get_all_aliases().is_some_and(|v| v.contains(&long))
        })
        .is_some_and(takes_value)
}

fn short_takes_value(cmd: &clap::Command, short: char) -> bool {
    cmd.get_arguments()
        .find(|a| a.get_short() == Some(short))
        .is_some_and(takes_value)
}

/// Resolve a token to a canonical subcommand name, following aliases.
fn canonical_name(cmd: &clap::Command, tok: &OsString) -> Option<String> {
    let t = tok.to_str()?;
    cmd.get_subcommands()
        .find(|sc| sc.get_name() == t || sc.get_all_aliases().any(|a| a == t))
        .map(|sc| sc.get_name().to_string())
}

/// The error a rejected flag deserves on a filter-taking command.
///
/// Clap's own text ("unexpected argument '--bogus' found", plus a tip to pass
/// it as a value with `--`) is wrong here twice over: the tip suggests turning
/// a typo into filter text, which is the failure mode this whole module exists
/// to prevent, and it never mentions that the shape the user probably wanted
/// takes ONE dash. The message comes from `filter.rs` rather than being copied,
/// so the token list cannot drift from the grammar that enforces it.
pub fn filter_flag_error(offender: &str) -> Option<String> {
    // The instant is irrelevant here — this parses a rejected FLAG to borrow
    // the grammar's own wording for it, and a flag never reaches a date bound.
    tasqx_core::filter::Filter::parse(offender, jiff::Timestamp::now()).err()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set above is a hand-maintained registry, so a new filter-taking
    /// command would silently lose `-tag` support. Any positional named
    /// `filter` must be registered; `report` spells its tail `args` and is
    /// asserted by name.
    #[test]
    fn every_filter_positional_is_registered() {
        let mut cmd = Cli::command();
        cmd.build();
        for sc in cmd.get_subcommands() {
            if sc.get_positionals().any(|a| a.get_id() == "filter") {
                assert!(
                    FILTER_COMMANDS.contains(&sc.get_name()),
                    "`{}` has a filter positional but is not in FILTER_COMMANDS, so `-tag` \
                     would not be typable there",
                    sc.get_name()
                );
            }
        }
        assert!(
            FILTER_COMMANDS.contains(&"report"),
            "report's `args` tail is filter DSL too"
        );
        for name in FILTER_COMMANDS {
            assert!(
                canonical_name(&cmd, &OsString::from(name)).is_some(),
                "no such subcommand: {name}"
            );
        }
    }

    /// Documents the rule. The guard that matters is the e2e one in
    /// `tests/regressions.rs`: this test builds the argv split itself and so
    /// would agree with a wrong split.
    #[test]
    fn only_single_dash_tokens_after_a_filter_command_are_escaped() {
        let go = |args: &[&str]| -> Vec<String> {
            prepass(args.iter().map(OsString::from))
                .argv
                .iter()
                .map(|s| s.to_string_lossy().replace(ESCAPED_DASH, "<esc>"))
                .collect()
        };
        assert_eq!(
            go(&["tasqx", "list", "-needs", "--json"]),
            ["tasqx", "list", "<esc>needs", "--json"]
        );
        assert_eq!(
            go(&["tasqx", "ls", "-needs"]),
            ["tasqx", "ls", "<esc>needs"],
            "aliases follow list"
        );
        assert_eq!(
            go(&["tasqx", "--theme", "nord", "list", "-a"]),
            ["tasqx", "--theme", "nord", "list", "<esc>a"]
        );
        // `--x` stays clap's to judge, and a non-filter command is untouched:
        // `add --remind -1h` must keep reaching `allow_hyphen_values`.
        assert_eq!(
            go(&["tasqx", "list", "--bogus"]),
            ["tasqx", "list", "--bogus"]
        );
        assert_eq!(
            go(&["tasqx", "add", "x", "--remind", "-1h"]),
            ["tasqx", "add", "x", "--remind", "-1h"]
        );
        assert_eq!(go(&["tasqx", "import", "-"]), ["tasqx", "import", "-"]);
    }

    /// N1b: the pre-pass escaped EVERY single-dash token, clap's own help flag
    /// included, so `-h` never reached clap on a filter command. `tasqx list -h`
    /// listed tasks and `tasqx export -h` dumped the whole store to stdout.
    ///
    /// Asserted over every short flag each filter command DECLARES, read out of
    /// clap rather than listed here. Listing `-h` as an exception is the
    /// hand-maintained shape that caused the bug; this guard covers `-V` and
    /// anything else the day it is declared, including on a filter command
    /// added later.
    #[test]
    fn no_declared_short_flag_is_ever_escaped() {
        let mut cmd = Cli::command();
        cmd.build();
        let mut seen = 0;
        for name in FILTER_COMMANDS {
            let sub = cmd.find_subcommand(name).expect("registered");
            let shorts: Vec<char> = sub
                .get_arguments()
                .filter_map(clap::Arg::get_short)
                .collect();
            // A filter command with no short flag at all would let this pass by
            // matching nothing — `-h` alone makes that unreachable in practice,
            // and the floor below makes it unreachable in fact.
            for c in shorts {
                seen += 1;
                let argv = [
                    OsString::from("tasqx"),
                    OsString::from(name),
                    OsString::from(format!("-{c}")),
                ];
                let out = prepass(argv);
                assert_eq!(
                    out.argv[2].to_string_lossy(),
                    format!("-{c}"),
                    "`{name} -{c}` is a flag {name} declares; escaping it hides it from clap"
                );
            }
        }
        assert!(
            seen >= FILTER_COMMANDS.len(),
            "every filter command declares at least `-h`"
        );
    }

    /// The other half of N1b: exempting clap's shorts must not re-break `-tag`.
    /// A multi-character token is filter text even when its first letter names a
    /// flag, because the dash grammar counts dashes, not letters.
    #[test]
    fn a_multi_character_dash_token_is_still_a_tag_exclusion() {
        let go = |args: &[&str]| -> Vec<String> {
            prepass(args.iter().map(OsString::from))
                .argv
                .iter()
                .map(|s| s.to_string_lossy().replace(ESCAPED_DASH, "<esc>"))
                .collect()
        };
        // `h` is `--help`'s short; `-hotfix` is a tag named `hotfix`.
        assert_eq!(
            go(&["tasqx", "list", "-hotfix"]),
            ["tasqx", "list", "<esc>hotfix"]
        );
        assert_eq!(
            go(&["tasqx", "list", "-h"]),
            ["tasqx", "list", "-h"],
            "clap's own flag reaches clap"
        );
        assert_eq!(
            go(&["tasqx", "list", "-needs", "-h"]),
            ["tasqx", "list", "<esc>needs", "-h"]
        );
    }

    /// C7: a sentinel that lands in a flag VALUE is never restored, because
    /// `unescape` only sees the filter tail — it is used, and printed, raw.
    ///
    /// Asserted over every value-taking flag each filter command declares,
    /// read out of clap rather than listed here, so a flag added tomorrow is
    /// covered the day it is declared. That is the point: the registry shape
    /// (`FILTER_COMMANDS`) has already leaked twice in this cluster, and a
    /// second hand-maintained list of "flags whose values must be skipped"
    /// would leak the same way.
    #[test]
    fn no_flag_value_is_ever_escaped() {
        let mut cmd = Cli::command();
        cmd.build();
        for name in FILTER_COMMANDS {
            let sub = cmd.find_subcommand(name).expect("registered above");
            let longs: Vec<String> = sub
                .get_arguments()
                .filter(|a| takes_value(a))
                .filter_map(|a| a.get_long().map(str::to_string))
                .collect();
            assert!(
                !longs.is_empty(),
                "`{name}` declares no value-taking flag; the guard would be vacuous"
            );
            for long in longs {
                let flag = format!("--{long}");
                // BOTH orders: the flag before the filter tail and after it.
                for argv in [
                    vec![
                        "tasqx".to_string(),
                        name.to_string(),
                        flag.clone(),
                        "-nord".to_string(),
                    ],
                    vec![
                        "tasqx".to_string(),
                        name.to_string(),
                        "-needs".to_string(),
                        flag.clone(),
                        "-nord".to_string(),
                    ],
                ] {
                    let out = prepass(argv.iter().map(|s| OsString::from(s.as_str())));
                    let last = out.argv.last().unwrap().to_string_lossy().into_owned();
                    assert_eq!(
                        last, "-nord",
                        "`{name} {flag} -nord` escaped the VALUE, which nothing unescapes"
                    );
                }
            }
        }
    }

    #[test]
    fn unescape_restores_the_dash() {
        let mut v = vec![format!("{ESCAPED_DASH}needs"), "+home".to_string()];
        unescape(&mut v);
        assert_eq!(v, ["-needs", "+home"]);
    }
}
