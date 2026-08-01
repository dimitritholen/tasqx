//! What the shell offers, as opposed to how it is fetched.
//!
//! [`super::lookup`] owns the safety — the budget, the read-only open, the
//! caught panic, the silence. This module owns the answer to "what could come
//! next here?", and every provider in it is a thin map from one JSON API result
//! to a list of [`CompletionCandidate`]s. It adds no API method: a shell
//! callback is not a reason to widen a contract surface D50 has been narrowing.

use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};
use serde_json::{json, Value};

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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap_complete::engine::ArgValueCompleter;

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
}
