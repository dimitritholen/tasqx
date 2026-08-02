//! What the shell offers, as opposed to how it is fetched.
//!
//! [`super::lookup`] owns the safety — the budget, the read-only open, the
//! caught panic, the silence. This module owns the answer to "what could come
//! next here?", and every provider in it is a thin map from one JSON API result
//! to a list of [`CompletionCandidate`]s. It adds no API method: a shell
//! callback is not a reason to widen a contract surface D50 has been narrowing.

use clap_complete::engine::{ArgValueCandidates, ArgValueCompleter, CompletionCandidate};
use serde_json::{json, Value};

use tasqx_core::Priority;

use crate::sugar::{Partial, ValueKey};

/// Most task ids one Tab press will offer.
///
/// A cap rather than a page: there is no "next page" in a completion menu, so
/// the only question is where usefulness stops. A shell that is handed several
/// thousand candidates draws a full-screen list nobody reads, and the cost is
/// paid twice — once serialising the rows, once by the shell laying them out —
/// on a path with a 150 ms budget for all of it.
///
/// Two hundred is above any plausible working set and below the point where the
/// menu stops being a menu. It is asked of the ENGINE as well as applied here:
/// sending `limit` means a store with ten thousand tasks never puts ten thousand
/// rows on the socket, and the local `take` is what makes the cap a property of
/// this function rather than a promise the engine keeps on its behalf.
const MAX_TASK_CANDIDATES: usize = 200;

/// Task ids, each carrying its title as the shell-rendered help text.
///
/// # Why the title is not optional
///
/// `tasqx done 4` is unforgiving in the way that matters: 4 is a real task, just
/// not the one meant, and the command succeeds. That is the defect this whole
/// feature exists for, and a completion that offers a column of bare integers
/// solves the TYPING problem while leaving the REMEMBERING problem exactly where
/// it was. The title is what turns the menu into something a user can choose
/// from rather than something they have to recognise.
///
/// Which shells show it is upstream's business and is not uniform: zsh renders
/// `value:help` and fish `value\thelp`, while bash's registration writes values
/// only (`clap_complete-4.6.7/src/env/shells.rs`). The help costs nothing where
/// it is dropped, so it is always attached.
///
/// # Why one provider for every verb
///
/// `reopen` wants terminal tasks and `done` wants open ones, so a filter tuned
/// for either makes the other useless — `reopen <TAB>` offering only pending
/// tasks would be worse than offering everything, because it would look like an
/// answer. So the provider filters nothing and sorts by urgency, the same order
/// `tasqx list` shows, and the cap takes the hottest [`MAX_TASK_CANDIDATES`].
/// Per-verb scoping is a real improvement and is a later decision with its own
/// evidence; guessing at it here would ship a menu that silently omits the row
/// the user was reaching for.
pub(crate) fn task_ids() -> ArgValueCandidates {
    ArgValueCandidates::new(task_id_candidates)
}

fn task_id_candidates() -> Vec<CompletionCandidate> {
    super::lookup(|backend| {
        backend
            .call(
                "task.list",
                &json!({
                    // Explicitly empty rather than omitted. D27 ruled the empty
                    // filter matches everything, so this states "every task" as
                    // a decision a reader can disagree with, instead of leaving
                    // it to look like a forgotten parameter.
                    "filter": "",
                    "sort": ["-urgency"],
                    "limit": MAX_TASK_CANDIDATES,
                    // The projection is half the budget argument: without it a
                    // store's every task ships its tags, dates, urgency and
                    // rev over the socket to have all of it thrown away.
                    "fields": ["short_id", "title"],
                }),
            )
            .ok()
    })
    .map(|rows| from_rows(&rows))
    .unwrap_or_default()
}

/// The pure half: one `task.list` result to candidates.
///
/// Split from the call so the mapping — which is where the cap and the help
/// text live — is testable without a store, a socket or a budget.
///
/// Every layer is fallible and none of it complains. A row without a usable
/// `short_id` is skipped rather than rendered as some placeholder: an id the
/// user cannot pass to `done` is not a candidate, and the failure policy has
/// already ruled out saying so.
fn from_rows(result: &Value) -> Vec<CompletionCandidate> {
    let Some(rows) = result.get("tasks").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .take(MAX_TASK_CANDIDATES)
        .filter_map(|row| {
            let short_id = row.get("short_id")?.as_i64()?;
            let candidate = CompletionCandidate::new(short_id.to_string());
            // `help` takes `Option<StyledStr>` — NOT an `impl Into<…>` — and
            // `StyledStr` has no `From<&'a str>` for a borrow that is not
            // 'static, so the owned round trip is required rather than clumsy.
            Some(match row.get("title").and_then(Value::as_str) {
                Some(title) if !title.is_empty() => candidate.help(Some(title.to_string().into())),
                _ => candidate,
            })
        })
        .collect()
}

// ---- project and tag values, and the sugar prefix dispatcher ---------------

/// Most project or tag names one Tab press will offer.
///
/// Same reasoning as [`MAX_TASK_CANDIDATES`] and a different failure mode, which
/// is why it is a second constant rather than the same one reused: task ids are
/// capped where the ENGINE can also be asked to cap (`task.list` takes `limit`),
/// while `project.list` takes no limit and the tag vocabulary is derived rather
/// than paged. So this cap is purely about the size of the menu, and it is the
/// only one there is.
///
/// It bites differently on the two attachment shapes, and the difference is
/// worth knowing before raising or lowering it. In [`sugar_words`] the partial
/// word is filtered FIRST and the cap applies to what survives, so it only
/// matters when two hundred names share a prefix. On [`projects`] the engine
/// does the filtering after this function has returned, so a store with more
/// than this many projects would have some of them cut off before the user's
/// prefix was ever consulted. Two hundred projects is not a store this tool has
/// seen; the asymmetry is recorded because it is invisible at the call site.
const MAX_VALUE_CANDIDATES: usize = 200;

/// Project names, for every argument whose value IS a project name.
///
/// An [`ArgValueCandidates`] rather than an [`ArgValueCompleter`], and the
/// choice is not stylistic. This is a fixed list with no prefix grammar of its
/// own: the whole word is the value, so the engine's own filter
/// (`complete_custom_arg_value`: `retain(|c| c.starts_with(value))`) is exactly
/// the right filter, and writing a second one here would be a second thing to
/// get wrong. The seam that must NOT be used this way is a positional the argv
/// pre-pass escapes into — there the engine filters against the sentinel — and
/// `complete::escaping_drift` fails the build for one. No `--project` and no
/// `use` positional is such a place: `--project`'s value is stepped over by the
/// pre-pass (`argv::prepass` reads clap's arg table to find flag values), and
/// `use` is not a filter command at all.
///
/// Archived projects are excluded, which is `project.list`'s default and the
/// same set `tasqx projects` shows. `use` refuses an archived project outright,
/// and for `--project` the exclusion follows the tool's own answer to "which
/// projects are there" rather than inventing a second one for the shell.
pub(crate) fn projects() -> ArgValueCandidates {
    ArgValueCandidates::new(project_candidates)
}

fn project_candidates() -> Vec<CompletionCandidate> {
    project_names()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// The one `project.list` read, shared by [`projects`] and the `project:` arm of
/// [`sugar_words`] so the two can never offer different sets for one value type.
fn project_names() -> Vec<String> {
    super::lookup(|backend| {
        backend
            .call(
                "project.list",
                // Explicit rather than omitted, exactly as `task_id_candidates`
                // spells its empty filter: this is the decision above, written
                // where a reader can disagree with it, not a forgotten param.
                &json!({ "include_archived": false }),
            )
            .ok()
    })
    .map(|result| project_names_from(&result))
    .unwrap_or_default()
}

/// Tag names, gathered from the `tags` field `task.list` rows already carry.
///
/// **No `tag.list` API method, deliberately (D50).** A shell callback is not a
/// reason to widen a contract surface that is being narrowed, and there is
/// nothing to widen it for: every tag that exists is on some task, so the rows
/// hold the whole vocabulary already.
///
/// **No `limit`, unlike the task-id provider, and the asymmetry is the point.**
/// A cap on task ids drops the least urgent ids from a menu, and the ids that
/// remain are still ids. A cap on the rows here would drop TAGS — silently,
/// and by which tasks happened to sort first, so `+api<TAB>` would find nothing
/// on a store whose api-tagged tasks are all old. That is a wrong answer rather
/// than a shorter one. The projection is what makes the unbounded read
/// affordable: `fields: ["tags"]` ships one small array per row instead of every
/// column, and the 150 ms budget still governs the whole thing.
fn tag_names() -> Vec<String> {
    super::lookup(|backend| {
        backend
            .call("task.list", &json!({ "filter": "", "fields": ["tags"] }))
            .ok()
    })
    .map(|result| tags_from(&result))
    .unwrap_or_default()
}

/// The pure half of [`project_names`]: one `project.list` envelope to the names
/// a shell can actually deliver.
///
/// Split from the call for the same reason [`from_rows`] is — the filtering is
/// where the decisions live, and they are testable without a store, a socket or
/// a budget.
fn project_names_from(result: &Value) -> Vec<String> {
    let Some(rows) = result.get("projects").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| row.get("name")?.as_str())
        .filter(|name| typeable_unquoted(name))
        .map(str::to_string)
        .take(MAX_VALUE_CANDIDATES)
        .collect()
}

/// The pure half of [`tag_names`]: every distinct tag on any row, sorted.
///
/// Sorted because the rows arrive in whatever order `task.list` produced and a
/// menu whose order changes when an unrelated task is touched is a menu nobody
/// can build muscle memory against. Deduplicated because a tag on forty tasks is
/// one tag.
fn tags_from(result: &Value) -> Vec<String> {
    let Some(rows) = result.get("tasks").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut tags: Vec<String> = rows
        .iter()
        .filter_map(|row| row.get("tags")?.as_array())
        .flatten()
        .filter_map(Value::as_str)
        .filter(|tag| typeable_unquoted(tag))
        .map(str::to_string)
        .collect();
    tags.sort_unstable();
    tags.dedup();
    tags.truncate(MAX_VALUE_CANDIDATES);
    tags
}

/// Characters a name may contain and still be insertable by a shell as ONE word.
///
/// Conservative on purpose, and short on purpose: every entry has to be harmless
/// unquoted in bash, zsh, fish, elvish AND PowerShell, and the cost of leaving a
/// safe character out is that one more name goes un-offered, while the cost of
/// letting an unsafe one in is a command that runs and does the wrong thing.
/// Project names in this tool are dotted paths (`work.api`) and tags are words,
/// so this covers the real vocabulary with room to spare.
const SAFE_PUNCTUATION: &str = "._-/+:";

/// Can this value be handed to a shell as a completion candidate at all?
///
/// # The problem, which is upstream's and cannot be fixed here
///
/// A candidate is inserted into the command line VERBATIM. `clap_complete` has
/// no shell-quoting layer — bash's registration writes bare values separated by
/// `$_CLAP_IFS` and every other shell's does the equivalent — so a candidate
/// containing a space arrives at the command line as a space, and the shell then
/// splits it into two words. For a project named `my big project`, completing
/// `tasqx add x --project my<TAB>` would produce
/// `tasqx add x --project my big project`, which clap reads as the project `my`
/// and two extra title words.
///
/// That is not a cosmetic failure. On a store that also has a project called
/// `my`, it is a task filed under the wrong project with a corrupted title, at
/// exit 0 — the silent-drop class this codebase hunts, manufactured by the
/// feature that was supposed to stop people typing the wrong thing.
///
/// # Why the obvious fix is not taken
///
/// Emitting the quoted spelling (`"my big project"`, which is what a user must
/// type and what `sugar::parse_add` documents) works in bash, zsh, fish and
/// PowerShell for [`sugar_words`], where this module owns the prefix filter. It
/// does NOT work for [`projects`], where the engine filters:
/// `retain(|c| c.starts_with(value))` compares the QUOTED candidate against the
/// unquoted word, so the project vanishes from the menu the moment the user
/// types the first letter of its name. One value type with two spellings and two
/// failure modes depending on which surface you press Tab on is worse than one
/// honest limitation.
///
/// # So the limitation is stated instead
///
/// A name a shell would split is not a candidate. The precedent is one function
/// up: a row without a usable `short_id` is skipped, because an id the user
/// cannot pass to `done` is not a candidate. A project the user cannot pass to
/// `--project` is not one either. It remains perfectly typable — with the quotes
/// the tool has always required — and `tasqx projects` still lists it; what is
/// lost is the Tab, and what is bought is that no Tab ever produces a command
/// that runs and does something else.
///
/// A leading `-` is excluded separately from the character set, because such a
/// name is typeable in the middle of a word and unusable at the start of one:
/// clap would read the completed value as a flag.
fn typeable_unquoted(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || SAFE_PUNCTUATION.contains(c))
}

/// The inline capture sugar, for every positional whose words reach
/// `sugar::parse_add`.
///
/// # Why an `ArgValueCompleter`, and why through `escaped_word_completer`
///
/// An [`ArgValueCompleter`] because this seam has to READ the partial word: `+`,
/// `project:`, `!` and `due:` are four different questions and only the word
/// says which one is being asked. An [`ArgValueCandidates`] is handed nothing
/// and returns a fixed list, so it could only ever answer all four at once —
/// `tasqx add Ship the <TAB>` offering every tag, every project and every
/// priority at a position where the user is typing a title.
///
/// Through [`super::escaped_word_completer`] rather than
/// `ArgValueCompleter::new`, even though the words `add` and `modify` take are
/// never escaped: `argv::prepass` only escapes inside a `FILTER_COMMANDS`
/// subcommand, and neither of these is one. Three reasons, in increasing order
/// of how much they would cost to learn later:
///
///  * `argv::unescaped` is a no-op on a word with no sentinel in it, so the
///    wrapper costs one `strip_prefix` on a path that is about to spend up to
///    150 ms on a database.
///  * It is the house shape. `complete::escaped_word_completer`'s doc says every
///    completer on a filter or capture-sugar positional is built with it, and a
///    reader who finds one that is not has to work out whether that is a
///    decision or an oversight.
///  * `FILTER_COMMANDS` is a set that has already grown once. The day `add`
///    learns a filter tail — or `report`, which is in that set, learns capture
///    sugar — a bare completer here would start returning nothing for every
///    `-tag` while `+tag` and `project:x` kept working, which is the four-of-five
///    failure `escaping_drift` exists to catch and the one this codebase has
///    already paid for once on this branch.
///
/// # One lookup per Tab press
///
/// The branch is decided from the word BEFORE any store is touched, and each arm
/// performs at most one [`super::lookup`]. The 150 ms budget is for the whole
/// lookup, so a dispatcher that fetched projects and tags and then chose between
/// them would be spending two round trips to use one.
pub(crate) fn sugar_words() -> ArgValueCompleter {
    super::escaped_word_completer(sugar_candidates)
}

/// The dispatcher: what can follow the partial word `current`?
///
/// The classification is `sugar::classify_partial`, which reads the same
/// `VALUE_KEYS` table the parser dispatches on, so `proj:` and `project:` are
/// one arm rather than two spellings someone has to remember, and a key added to
/// that table lands here as a non-exhaustive `match` — a compile error — rather
/// than as a prefix that silently offers nothing.
fn sugar_candidates(current: &str) -> Vec<CompletionCandidate> {
    match crate::sugar::classify_partial(current) {
        // Ordinary title text. Offering the sugar vocabulary here would put a
        // menu in front of somebody typing prose, and the user has not asked a
        // question a menu answers.
        None => Vec::new(),
        Some(Partial::Tag(typed)) => prefixed("+", typed, tag_names()),
        Some(Partial::Priority(typed)) => priority_candidates(typed),
        // Matched variant by variant with no wildcard, so the compiler asks
        // whoever adds a key to `VALUE_KEYS` what its Tab press should do.
        Some(Partial::Key(key, spelling, typed)) => match key {
            ValueKey::Project => prefixed(spelling, typed, project_names()),
            // Every remaining key takes an OPEN vocabulary and offers nothing.
            // Dates, recurrence rules, reminder offsets and estimates are
            // natural-language expressions parsed by `tasqx_core::datetime` and
            // `recur`, which hold no list of accepted words — the grammar is
            // `in 3 days`, `every 3 days`, `-30m`, `1h30m`. There is no registry
            // to read, and a hand-written menu of `today`/`tomorrow`/`friday`
            // would be a fourth copy of a vocabulary that has three already
            // (D30). Offering nothing is the honest answer to a question whose
            // answer is "anything a human can write".
            ValueKey::Due
            | ValueKey::Scheduled
            | ValueKey::Wait
            | ValueKey::Repeat
            | ValueKey::Every
            | ValueKey::Remind
            | ValueKey::Estimate => Vec::new(),
        },
    }
}

/// Filter `values` by what has been typed and re-attach the sugar prefix.
///
/// The prefix must come back because the candidate REPLACES the whole word:
/// `+ap<TAB>` has to answer `+api`, not `api`, or Tab deletes the very character
/// that made it a tag. `spelling` is the alias the user typed (`proj:` stays
/// `proj:`), read out of `VALUE_KEYS` by the classifier rather than re-derived
/// here.
///
/// Filtered case-SENSITIVELY, matching what clap's engine does to a possible
/// value (`complete_arg_value`: `name.starts_with(value)`). A completer that
/// filtered more loosely than the rest of the surface would make `+Api<TAB>`
/// behave differently from `--priority Hi<TAB>` for no reason a user could see.
fn prefixed(spelling: &str, typed: &str, values: Vec<String>) -> Vec<CompletionCandidate> {
    values
        .into_iter()
        .filter(|value| value.starts_with(typed))
        .take(MAX_VALUE_CANDIDATES)
        .map(|value| CompletionCandidate::new(format!("{spelling}{value}")))
        .collect()
}

/// `!high` and friends, read out of [`Priority::SPELLINGS`] — the engine's table,
/// which `--priority`'s `value_parser` and `sugar`'s `!` branch already share.
///
/// All seven spellings are offered VISIBLY, unlike `--priority`, which hides the
/// long forms so a three-valued flag does not show seven candidates. The
/// difference is forced by clap's engine rather than chosen: it drops every
/// hidden candidate whenever any visible one survives
/// (`complete::complete_arg`), and in a positional the surrounding flags are
/// always visible candidates — so a hidden `!high` would be invisible at every
/// prefix including `!hi`, where `--priority hi<TAB>` does surface `high`.
/// Marking them hidden here would not tidy the menu, it would delete them.
///
/// Needs no [`super::lookup`]: the vocabulary is compiled in, so `!<TAB>` is the
/// one sugar prefix that answers without reading the store at all.
fn priority_candidates(typed: &str) -> Vec<CompletionCandidate> {
    Priority::SPELLINGS
        .iter()
        .filter(|(spelling, _)| spelling.starts_with(typed))
        .map(|(spelling, _)| CompletionCandidate::new(format!("!{spelling}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// The help text every task-reference positional in the derive tree carries,
    /// and the first of the two signals [`every_task_id_positional_offers_ids`]
    /// reads out of clap's own arg table.
    const TASK_REF_HELP: &str = "short_id or UUID";

    /// The second signal: the field names the derive tree gives those
    /// positionals. Two independent signals rather than one, because either
    /// alone is a single point of drift — a reworded doc comment, or a renamed
    /// field — and the guard asserts they AGREE, so losing one is a red build
    /// rather than a quietly narrower guard.
    const TASK_REF_IDS: [&str; 2] = ["ref", "depends_on"];

    /// Floor, not a list. The tree declares thirteen task-reference positionals
    /// today; the guard finds them itself and this only stops it from passing
    /// vacuously if the discovery breaks. Raise it when the surface grows.
    const KNOWN_TASK_REF_POSITIONALS: usize = 13;

    /// Every subcommand in the tree, at every depth.
    ///
    /// Recursive because `memory`, `config`, `chart`, `theme`, `tokens` and
    /// `mcp` hold their verbs one level down, and a task reference added under
    /// one of them would be invisible to a single-level walk.
    fn all_subcommands(cmd: &clap::Command) -> Vec<&clap::Command> {
        let mut out = Vec::new();
        for sc in cmd.get_subcommands() {
            out.push(sc);
            out.extend(all_subcommands(sc));
        }
        out
    }

    /// The same walk, carrying the path each verb is reached by.
    ///
    /// The bare name is not an identity in this tree and treating it as one is a
    /// measured mistake, not a hypothetical: `memory add` is also called `add`,
    /// and it also declares a positional called `title` — so a guard matching
    /// `("add", "title")` against `get_name()` accuses `memory add` of failing
    /// to complete task capture sugar. The path is what distinguishes them, and
    /// it is what `sugar::SUGAR_POSITIONALS` therefore spells.
    fn all_subcommands_trailed<'a>(
        cmd: &'a clap::Command,
        prefix: &str,
    ) -> Vec<(String, &'a clap::Command)> {
        let mut out = Vec::new();
        for sc in cmd.get_subcommands() {
            let trail = match prefix.is_empty() {
                true => sc.get_name().to_string(),
                false => format!("{prefix} {}", sc.get_name()),
            };
            out.extend(all_subcommands_trailed(sc, &trail));
            out.push((trail, sc));
        }
        out
    }

    /// Drift guard: a positional that takes a task reference must offer task
    /// ids.
    ///
    /// **Read out of clap's arg table, never a list of verbs kept here.** The
    /// hand-kept list is the shape this repository keeps paying for — the
    /// `COMMAND_REF` drift that left fourteen of twenty-seven examples
    /// unexecuted, and `FILTER_COMMANDS` leaking twice in the `argv` cluster —
    /// and a verb added tomorrow with a `ref` positional and no completer would
    /// be exactly that failure: `tasqx newverb <TAB>` silently offering nothing,
    /// with no symptom anywhere else.
    ///
    /// The two signals are asserted to agree BEFORE the attachment is checked.
    /// If only the help text decided membership, rewording one doc comment would
    /// quietly drop a positional out of the guard's scope and the guard would go
    /// on passing; the same is true of the field name alone. Disagreement means
    /// one of them moved, which is the moment to look — not later.
    ///
    /// Mutation-proven: removing the attachment from any single positional
    /// reddens this test naming that positional.
    #[test]
    fn every_task_id_positional_offers_ids() {
        let mut cmd = crate::Cli::command();
        cmd.build();

        let mut checked = 0;
        for sc in all_subcommands(&cmd) {
            for pos in sc.get_positionals() {
                let help = pos.get_help().map(ToString::to_string).unwrap_or_default();
                let by_help = help.contains(TASK_REF_HELP);
                let by_id = TASK_REF_IDS.contains(&pos.get_id().as_str());
                assert_eq!(
                    by_help,
                    by_id,
                    "`{} {}` is a task reference by one signal and not the other \
                     (help {help:?}). One of them moved: either the doc comment \
                     stopped saying {TASK_REF_HELP:?} or the field is no longer \
                     one of {TASK_REF_IDS:?}. Fix the signal, do not narrow the \
                     guard.",
                    sc.get_name(),
                    pos.get_id()
                );
                if !by_id {
                    continue;
                }
                checked += 1;
                assert!(
                    pos.get::<ArgValueCandidates>().is_some()
                        || pos.get::<ArgValueCompleter>().is_some(),
                    "`{} {}` takes a task id and offers no candidates, so \
                     `tasqx {} <TAB>` is silent — the one surface this feature \
                     exists for. Attach \
                     `add = crate::complete::candidates::task_ids()`.",
                    sc.get_name(),
                    pos.get_id(),
                    sc.get_name()
                );
            }
        }
        assert!(
            checked >= KNOWN_TASK_REF_POSITIONALS,
            "the guard found {checked} task-reference positionals and the tree \
             declares at least {KNOWN_TASK_REF_POSITIONALS}; the discovery is \
             broken and this test is guarding nothing"
        );
    }

    /// The title reaches the candidate as help text, which is the whole
    /// difference between a column of integers and a menu.
    #[test]
    fn a_row_carries_its_title_as_help() {
        let got = from_rows(&json!({
            "count": 2,
            "tasks": [
                { "short_id": 4, "title": "ship the completion slice" },
                { "short_id": 17, "title": "write the manual section" },
            ]
        }));
        let seen: Vec<(String, Option<String>)> = got
            .iter()
            .map(|c| {
                (
                    c.get_value().to_string_lossy().into_owned(),
                    c.get_help().map(ToString::to_string),
                )
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                ("4".to_string(), Some("ship the completion slice".into())),
                ("17".to_string(), Some("write the manual section".into())),
            ]
        );
    }

    /// The engine's `limit` and this cap must both hold, because either can be
    /// the one that is missing: a daemon running an older engine, or a `fields`
    /// projection someone widens without noticing the row count is unbounded.
    #[test]
    fn the_candidate_list_is_capped() {
        let tasks: Vec<Value> = (0..MAX_TASK_CANDIDATES * 3)
            .map(|i| json!({ "short_id": i, "title": format!("task {i}") }))
            .collect();
        let got = from_rows(&json!({ "count": tasks.len(), "tasks": tasks }));
        assert_eq!(got.len(), MAX_TASK_CANDIDATES);
    }

    /// Nothing on this path may panic or shout at a malformed row: the failure
    /// policy has ruled out every way of complaining, so a row that cannot
    /// produce a usable id simply is not a candidate.
    #[test]
    fn unusable_rows_are_skipped_rather_than_rendered() {
        let got = from_rows(&json!({
            "tasks": [
                { "title": "no id at all" },
                { "short_id": "7", "title": "an id that is not a number" },
                { "short_id": 9 },
                { "short_id": 11, "title": "" },
                { "short_id": 12, "title": "usable" },
            ]
        }));
        let values: Vec<String> = got
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();
        assert_eq!(values, ["9", "11", "12"]);
        assert_eq!(got[0].get_help(), None, "a row with no title carries none");
        assert_eq!(got[1].get_help(), None, "an empty title is not help text");
    }

    /// A result that is not a task listing at all — an error envelope, a
    /// truncated response, a daemon speaking a different shape — yields no
    /// candidates rather than unwinding into `catch_unwind`.
    #[test]
    fn a_result_that_is_not_a_listing_yields_nothing() {
        assert!(from_rows(&json!({ "count": 0 })).is_empty());
        assert!(from_rows(&Value::Null).is_empty());
        assert!(from_rows(&json!({ "tasks": "not an array" })).is_empty());
    }

    // ---- projects, tags and the sugar dispatcher ---------------------------

    /// The second signal [`every_sugar_positional_offers_sugar`] reads, and the
    /// only one that lives outside `sugar.rs`: the phrase every capture-sugar
    /// positional's help text carries.
    const SUGAR_HELP: &str = "inline sugar";

    /// Drift guard: a positional whose words reach `sugar::parse_add` must offer
    /// the sugar candidates, and must do it through
    /// `complete::escaped_word_completer`.
    ///
    /// **Membership is not a list of verbs kept here.** It is
    /// `sugar::SUGAR_POSITIONALS` — the registry beside the parser those words
    /// are parsed by — checked against a second, independent signal: the
    /// positional's own help text. The two must AGREE before the attachment is
    /// looked at, exactly as [`every_task_id_positional_offers_ids`] makes the
    /// help text and the field name agree, and for the same reason: either
    /// signal alone is one edit from quietly narrowing the guard's scope. A verb
    /// that gains a sugar tail and is added to only one of them is a red build
    /// naming which one it is missing from.
    ///
    /// **The assertion is behavioural, not structural.** It does not ask whether
    /// *something* is attached; it drives the completer with an escaped probe
    /// word through `complete::restores_the_escaped_dash` and requires the dash
    /// back. That is what makes it catch the likelier of the two mistakes — a
    /// bare `ArgValueCompleter`, which is the right type, compiles, reads
    /// correctly, and returns nothing for every `-tag` if a sugar positional
    /// ever becomes a filter positional. `Some(false)` is that defect,
    /// `None` is no completer at all, and both fail here with their own message.
    ///
    /// Mutation-proven: removing either attachment reddens this test naming that
    /// positional, and swapping either for a bare `ArgValueCompleter` reddens it
    /// with the other message.
    #[test]
    fn every_sugar_positional_offers_sugar() {
        let mut cmd = crate::Cli::command();
        cmd.build();

        let mut checked = 0;
        for (trail, sc) in all_subcommands_trailed(&cmd, "") {
            for pos in sc.get_positionals() {
                let help = pos.get_help().map(ToString::to_string).unwrap_or_default();
                let by_help = help.contains(SUGAR_HELP);
                let by_registry = crate::sugar::SUGAR_POSITIONALS
                    .contains(&(trail.as_str(), pos.get_id().as_str()));
                assert_eq!(
                    by_help,
                    by_registry,
                    "`{trail} {}` takes capture sugar by one signal and not the \
                     other (help {help:?}). Either the help stopped saying \
                     {SUGAR_HELP:?} or `sugar::SUGAR_POSITIONALS` was not updated. \
                     Fix the signal, do not narrow the guard.",
                    pos.get_id()
                );
                if !by_registry {
                    continue;
                }
                checked += 1;
                match crate::complete::restores_the_escaped_dash(pos) {
                    Some(true) => {}
                    Some(false) => panic!(
                        "`{trail} {}` carries an ArgValueCompleter that did not \
                         restore the escaped dash. Build it with \
                         `complete::escaped_word_completer`, as \
                         `candidates::sugar_words` does.",
                        pos.get_id()
                    ),
                    None => panic!(
                        "`{trail} {}` is parsed by `sugar::parse_add` and offers no \
                         candidates, so `tasqx {trail} +<TAB>` is silent. Attach \
                         `add = crate::complete::candidates::sugar_words()`.",
                        pos.get_id()
                    ),
                }
            }
        }
        assert_eq!(
            checked,
            crate::sugar::SUGAR_POSITIONALS.len(),
            "the guard found {checked} sugar positionals and the registry names \
             {}; one of them is not in clap's tree under the name it is \
             registered with, so the guard is looking at less than it claims",
            crate::sugar::SUGAR_POSITIONALS.len()
        );
    }

    /// The value name that means "this argument's value IS a project name".
    const PROJECT_VALUE_NAME: &str = "PROJECT";

    /// Drift guard: an argument announcing that its value is a project must say
    /// how to complete one.
    ///
    /// Membership is read off clap's own arg table by the name the argument
    /// gives its value — the same technique, and the same fallback, as
    /// `command.rs`'s `every_path_shaped_arg_declares_how_to_complete_it`. A
    /// list of `--project` sites kept in this test is the shape this repository
    /// keeps paying for: there are four today (`add`, `modify`, `chart
    /// burndown`, `use`) and the fourth is a POSITIONAL, which is exactly the
    /// one a list written from memory forgets.
    ///
    /// It is a naming convention, enforced in the direction that can be
    /// enforced: it cannot know that some argument is secretly a project, but it
    /// can insist that one calling its value `<PROJECT>` completes projects.
    /// `init` is outside it on purpose and says so at the declaration — it
    /// creates a project, so the existing names are the ones it refuses.
    ///
    /// Mutation-proven: removing the attachment from any of the four reddens
    /// this test naming that argument.
    #[test]
    fn every_project_valued_arg_offers_project_names() {
        let mut cmd = crate::Cli::command();
        cmd.build();

        let mut checked = 0;
        for (trail, sc) in all_subcommands_trailed(&cmd, "") {
            for arg in sc.get_arguments() {
                // The same fallback the path-hint guard uses: clap leaves
                // `get_value_names` empty for some arguments and the id,
                // uppercased, is what it then renders.
                let name = arg
                    .get_value_names()
                    .and_then(|n| n.first().map(|s| s.as_str().to_string()))
                    .unwrap_or_else(|| arg.get_id().as_str().to_ascii_uppercase());
                if name != PROJECT_VALUE_NAME {
                    continue;
                }
                checked += 1;
                assert!(
                    arg.get::<ArgValueCandidates>().is_some(),
                    "`{trail} {}` takes a project name and offers no candidates, \
                     so `tasqx {trail} …<TAB>` is silent where the tool knows the \
                     answer. Attach \
                     `add = crate::complete::candidates::projects()`.",
                    arg.get_id()
                );
            }
        }
        assert!(
            checked >= KNOWN_PROJECT_VALUED_ARGS,
            "the guard found {checked} project-valued arguments and the tree \
             declares at least {KNOWN_PROJECT_VALUED_ARGS}; the discovery is \
             broken and this test is guarding nothing"
        );
    }

    /// Floor, not a list: `add --project`, `modify --project`,
    /// `chart burndown --project` and `use <PROJECT>`. Raise it when the surface
    /// grows; the guard finds the members itself.
    const KNOWN_PROJECT_VALUED_ARGS: usize = 4;

    /// The mapping from `project.list`'s envelope, including the one filter that
    /// is a decision rather than plumbing.
    #[test]
    fn project_rows_become_names_and_unquotable_ones_do_not() {
        let got = project_names_from(&json!({
            "count": 4,
            "projects": [
                { "id": "a", "name": "work", "archived": false, "default": true },
                { "id": "b", "name": "work.api", "archived": false, "default": false },
                // A shell would split this into two words and file the task
                // under `home`; see `typeable_unquoted`.
                { "id": "c", "name": "home renovation", "archived": false, "default": false },
                { "id": "d", "archived": false, "default": false },
            ]
        }));
        assert_eq!(got, ["work", "work.api"]);
    }

    /// A result that is not a project listing yields nothing rather than
    /// unwinding — same promise `from_rows` makes for task ids.
    #[test]
    fn a_result_that_is_not_a_project_listing_yields_nothing() {
        assert!(project_names_from(&json!({ "count": 0 })).is_empty());
        assert!(project_names_from(&Value::Null).is_empty());
        assert!(project_names_from(&json!({ "projects": "nope" })).is_empty());
    }

    /// Tags come off the rows `task.list` already returns: distinct, sorted, and
    /// filtered by the same rule project names are.
    #[test]
    fn tags_are_gathered_deduplicated_and_sorted() {
        let got = tags_from(&json!({
            "count": 3,
            "tasks": [
                { "tags": ["docs", "api"] },
                { "tags": ["api", "needs paint"] },
                { "tags": [] },
                { "title": "a row with no tags key at all" },
                { "tags": "not an array" },
            ]
        }));
        assert_eq!(got, ["api", "docs"]);
        assert!(tags_from(&Value::Null).is_empty());
    }

    /// The rule that decides whether a value can be a candidate at all. Read
    /// [`typeable_unquoted`] before changing any of these: the `false` cases are
    /// values that would complete into a command that runs and does something
    /// else.
    #[test]
    fn only_values_a_shell_delivers_whole_are_offered() {
        for ok in [
            "work", "work.api", "a_b", "a-b", "C++", "v1/2", "ns:thing", "café",
        ] {
            assert!(typeable_unquoted(ok), "{ok:?} is deliverable unquoted");
        }
        for bad in [
            "",
            "home renovation",
            "a\tb",
            // Unterminated quotes: the shell would not even reach tasqx.
            "say\"hi",
            "it's",
            // The shell eats the backslash, so the value that arrives is not
            // the value that was offered.
            "a\\b",
            // Clap would read the completed value as a flag.
            "-lead",
            // Word-splitting and expansion characters, one per family.
            "a;b",
            "a|b",
            "a$b",
            "a*b",
            "a(b",
        ] {
            assert!(!typeable_unquoted(bad), "{bad:?} must not be a candidate");
        }
    }

    /// The two dispatcher arms that need no store, so they can be driven in
    /// process: an ordinary title word offers nothing, and `!` offers the
    /// engine's priority table.
    ///
    /// The `+` and `project:` arms are deliberately NOT driven here — they call
    /// `super::lookup`, which would open whatever store this developer's
    /// environment points at. They are proven end to end against a seeded store
    /// in `tests/completion.rs`, through the real binary and the real callback
    /// protocol, which is the only place that proof means anything.
    #[test]
    fn a_title_word_offers_nothing_and_a_bang_offers_priorities() {
        for prose in ["zz", "Ship", "the", "Display", "recur::advance_once", ""] {
            assert!(
                sugar_candidates(prose).is_empty(),
                "{prose:?} is title text and must offer nothing"
            );
        }

        let all: Vec<String> = values(sugar_candidates("!"));
        assert_eq!(all, ["!H", "!high", "!M", "!medium", "!med", "!L", "!low"]);
        // Filtered by what has been typed, prefix re-attached.
        assert_eq!(values(sugar_candidates("!hi")), ["!high"]);
        assert_eq!(values(sugar_candidates("!zz")), Vec::<String>::new());
    }

    /// `project::config` is a Rust path, not a project key — the same refusal
    /// `sugar::split_key` makes on the parse path, made here so a Tab press in
    /// the middle of typing one does not go to the store.
    #[test]
    fn a_rust_path_is_not_a_sugar_prefix() {
        for path in ["project::config", "recur::advance_once", "due::soon"] {
            assert!(
                sugar_candidates(path).is_empty(),
                "{path:?} is a path and must not reach a provider"
            );
        }
    }

    /// Both aliases of the project key survive into the candidate, because the
    /// candidate replaces the whole word and rewriting `proj:` to `project:`
    /// would change what the user chose to type.
    #[test]
    fn the_key_spelling_the_user_typed_is_the_one_that_comes_back() {
        let names = vec!["work".to_string(), "home".to_string()];
        assert_eq!(
            values(prefixed("proj:", "", names.clone())),
            ["proj:work", "proj:home"]
        );
        assert_eq!(values(prefixed("project:", "wo", names)), ["project:work"]);
    }

    fn values(got: Vec<CompletionCandidate>) -> Vec<String> {
        got.iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
    }
}
