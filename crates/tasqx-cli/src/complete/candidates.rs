//! What the shell offers, as opposed to how it is fetched.
//!
//! [`super::lookup`] owns the safety — the budget, the read-only open, the
//! caught panic, the silence. This module owns the answer to "what could come
//! next here?", and every provider in it is a thin map from one JSON API result
//! to a list of [`CompletionCandidate`]s. It adds no API method: a shell
//! callback is not a reason to widen a contract surface D50 has been narrowing.

use clap_complete::engine::{ArgValueCandidates, ArgValueCompleter, CompletionCandidate};
use serde_json::{json, Value};

use tasqx_core::filter::{self, Vocabulary};
use tasqx_core::{Priority, Status};

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
/// word is filtered FIRST and the cap applies to what survives ([`prefixed`]),
/// so it only matters when two hundred names share a prefix. On [`projects`] the
/// engine does the filtering after [`project_candidates`] has returned, so a
/// store with more than this many projects would have some of them cut off
/// before the user's prefix was ever consulted. Two hundred projects is not a
/// store this tool has seen; the asymmetry is recorded because it is invisible
/// at the call site.
///
/// That paragraph described the intended design and, for one commit, not the
/// code: the cap sat in [`tags_from`] and [`project_names_from`] — before the
/// prefix on BOTH shapes — so on a store with 251 tags `+api<TAB>` answered
/// nothing while an api-tagged task sat in the store. A doc asserting a
/// protection the code does not have is the same defect shape as the bug it
/// describes, which is why the guard below now measures the ordering instead of
/// this text promising it.
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
/// Archived projects are excluded, and the reason is what the RECEIVING command
/// does rather than what `project.list` defaults to: `add`, `modify` and `use`
/// all refuse an archived project outright (`engine.rs`), so offering one would
/// be a menu entry whose only outcome is an error. It is also the set
/// `tasqx projects` shows, so the shell and the tool answer "which projects are
/// there" the same way.
///
/// A command that ACCEPTS an archived project therefore needs the other
/// constructor — see [`projects_including_archived`]. Splitting them rather than
/// widening this one keeps each attachment site offering exactly what it takes.
pub(crate) fn projects() -> ArgValueCandidates {
    ArgValueCandidates::new(|| project_candidates(false))
}

/// [`projects`] plus the archived ones, for a command that genuinely accepts one.
///
/// `chart burndown --project` is the only such site today, and it is a READ:
/// `tasqx chart burndown --project oldstuff` charts an archived project and
/// exits 0. Serving it the narrow set made completion offer LESS than the
/// command accepts — an under-offer rather than a wrong answer, but the kind
/// that looks like the project no longer exists.
pub(crate) fn projects_including_archived() -> ArgValueCandidates {
    ArgValueCandidates::new(|| project_candidates(true))
}

/// The whole-word surface, so two things happen here that the sugar arm does not
/// want: a name that would be read as a flag is dropped ([`standalone_word`]),
/// and the cap is applied BEFORE any prefix, because the engine does its
/// filtering after this function has returned and there is nowhere later to put
/// it. That asymmetry with [`prefixed`] is real and is recorded on
/// [`MAX_VALUE_CANDIDATES`]; it is the one place a name can be cut off without
/// the user's prefix having been consulted.
fn project_candidates(include_archived: bool) -> Vec<CompletionCandidate> {
    project_names(include_archived)
        .into_iter()
        .filter(|name| standalone_word(name))
        .take(MAX_VALUE_CANDIDATES)
        .map(CompletionCandidate::new)
        .collect()
}

/// The one `project.list` read, shared by [`projects`] and the `project:` arm of
/// [`sugar_words`] so the two can never offer different sets for one value type.
fn project_names(include_archived: bool) -> Vec<String> {
    super::lookup(move |backend| {
        backend
            .call(
                "project.list",
                // Explicit rather than omitted, exactly as `task_id_candidates`
                // spells its empty filter: this is the decision above, written
                // where a reader can disagree with it, not a forgotten param.
                &json!({ "include_archived": include_archived }),
            )
            .ok()
    })
    .map(|result| project_names_from(&result))
    .unwrap_or_default()
}

/// Tag names, for every argument whose value IS a tag name.
///
/// Its existence is not optional once [`sugar_words`] answers `+<TAB>`. Leaving
/// `--tag` silent while the `+` one positional away offered the whole vocabulary
/// was precisely the shape this module argues against elsewhere — a surface that
/// serves most of its prefixes looks finished, so nobody goes looking for the
/// one it drops.
///
/// # Why this one is a COMPLETER and [`projects`] is not
///
/// The obvious shape is [`projects`]'s — a flag value is the whole word with no
/// prefix grammar of its own, so the engine's own filter should be the right
/// filter. It was tried and MEASURED, and it reinstated the bug this module had
/// just removed one surface over. An [`ArgValueCandidates`] is filtered by the
/// engine AFTER the provider returns, so the cap has to be applied first, and on
/// a store with 252 distinct tags `--tag zeb<TAB>` answered nothing while
/// `+zeb<TAB>` answered `+zebra`: the two hundred alphabetically-first tags used
/// up the whole menu before the word was consulted.
///
/// That is tolerable for projects and not for tags, and the difference is how
/// many there plausibly are. Two hundred projects is not a store this tool has
/// seen. Two hundred tags is an ordinary year of use, which is exactly why
/// [`tag_names`] sends no `limit` — a cap that drops TAGS is a wrong answer
/// rather than a shorter one, and moving it from the read to the menu does not
/// change that.
///
/// So this seam takes the partial word and filters before capping, like
/// [`prefixed`] does. Built through [`super::escaped_word_completer`] for the
/// reason [`sugar_words`] is: `--tag`'s value is never escaped today (`add` and
/// `modify` are not filter commands, and the pre-pass steps over flag values in
/// the ones that are), the restore is a no-op without a sentinel, and the
/// wrapper is the shape this module has committed to for anything handed a raw
/// word.
pub(crate) fn tags() -> ArgValueCompleter {
    super::escaped_word_completer(tag_candidates)
}

fn tag_candidates(typed: &str) -> Vec<CompletionCandidate> {
    tag_names()
        .into_iter()
        .filter(|name| standalone_word(name) && name.starts_with(typed))
        .take(MAX_VALUE_CANDIDATES)
        .map(CompletionCandidate::new)
        .collect()
}

/// The one `task.list` read, shared by [`tags`] and the `+` arm of
/// [`sugar_words`], gathered from the `tags` field `task.list` rows already carry.
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
        .filter(|name| deliverable_as_one_word(name))
        .map(str::to_string)
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
        .filter(|tag| deliverable_as_one_word(tag))
        .map(str::to_string)
        .collect();
    tags.sort_unstable();
    tags.dedup();
    // NOT capped here. The cap belongs after the user's prefix has been applied
    // ([`prefixed`]), and truncating the sorted vocabulary first is the defect
    // this function's own doc argues against one paragraph up: it reinstates the
    // silent tag loss that sending no `limit` was chosen to avoid, merely
    // ordering it alphabetically instead of by row order. Measured on a store
    // with 251 tags: `+api<TAB>` answered nothing while an api-tagged task sat
    // in the store, because `api` sorted past the two-hundredth name.
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
/// # What this does NOT answer
///
/// Only whether a SHELL can deliver the name as one word. Whether the composed
/// candidate then survives tasqx's own grammar is a second, independent question
/// — `:x` is perfectly deliverable and `project::x` is still not a project
/// reference — and it is answered by `sugar::parsed_value_of` in [`prefixed`].
/// Conflating the two is how a leading-`:` project name shipped as a candidate
/// that filed the task under the default project at exit 0.
fn deliverable_as_one_word(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || SAFE_PUNCTUATION.contains(c))
}

/// Can this value stand as a whole argv element — the shape `--project x` and
/// `use x` take?
///
/// [`deliverable_as_one_word`] plus one more refusal: a leading `-`, because
/// such a name is typeable in the middle of a word and unusable at the start of
/// one — clap reads the completed value as a flag.
///
/// Deliberately NOT applied on the sugar surface. There the name is never the
/// whole element; it arrives welded to a prefix, so a tag called `-x` completes
/// to `+-x` and a project called `-x` to `project:-x`, neither of which is
/// flag-shaped and both of which [`prefixed`]'s round-trip gate then confirms
/// the parser accepts. Applying the flag rule there too withheld those names for
/// a reason that does not hold, which is an under-offer rather than a wrong
/// answer, but it is still a rule stated in a place it is not true.
fn standalone_word(value: &str) -> bool {
    deliverable_as_one_word(value) && !value.starts_with('-')
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
            // The narrow set, for the same reason `--project` takes it: this
            // sugar reaches `add` and `modify`, both of which refuse an
            // archived project.
            ValueKey::Project => prefixed(spelling, typed, project_names(false)),
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
/// # The round-trip gate, and why it is not a character rule
///
/// A composed word is not automatically a word `sugar::parse_add` accepts. The
/// case that shipped: a project may legally be named `:x`, `:x` is perfectly
/// deliverable by a shell, and `project:` + `:x` is `project::x` — which
/// `sugar::split_key` refuses as a Rust path. Accepting that candidate filed the
/// task under the DEFAULT project and appended the word to the TITLE, at exit 0.
/// Measured against the built binary, on a store holding such a project.
///
/// So every candidate is handed back to `sugar::parsed_value_of` and kept only
/// if the parser takes the same value out of it that went in. Reading the answer
/// out of the parser rather than adding `!name.starts_with(':')` here is the
/// point: the `::` refusal already moved once, and a rule restated in two places
/// is the drift shape this repository keeps paying for. It also covers every
/// future key and refusal without this function being touched.
///
/// # Where the cap goes, and why it is here rather than in the readers
///
/// After the prefix filter, so it bounds the MENU rather than the vocabulary.
/// The readers ([`tags_from`], [`project_names_from`]) deliberately do not cap:
/// capping a sorted vocabulary before the user's prefix is consulted makes a tag
/// that exists and uniquely matches what was typed silently absent from its own
/// menu, which is a wrong answer rather than a shorter one.
fn prefixed(spelling: &str, typed: &str, values: Vec<String>) -> Vec<CompletionCandidate> {
    values
        .into_iter()
        .filter(|value| value.starts_with(typed))
        .filter_map(|value| {
            let word = format!("{spelling}{value}");
            (crate::sugar::parsed_value_of(&word) == Some(value.as_str())).then_some(word)
        })
        .take(MAX_VALUE_CANDIDATES)
        .map(CompletionCandidate::new)
        .collect()
}

/// `!high` and friends, read out of [`Priority::SPELLINGS`] — the engine's table,
/// which `--priority`'s `value_parser` and `sugar`'s `!` branch already share.
///
/// The long forms are HIDDEN, by the same rule and for the same reason
/// `priority_parser` hides them on `--priority`: a three-valued concept showing
/// seven candidates is noise, and the canonical spelling is the one to teach.
/// `hide` is asked of the same comparison there — the spelling differing from
/// `Priority::as_str` — rather than a second list of which four are long.
///
/// Hiding does not delete them, which had to be MEASURED because clap's engine
/// drops every hidden candidate whenever a visible one survives
/// (`complete::complete_arg`) and an earlier version of this comment argued from
/// that rule that a hidden `!high` would be unreachable at every prefix. It is
/// not: for a non-empty word that does not start with `-`, `complete_option`
/// contributes nothing, so there are no surrounding visible flags to trigger the
/// drop. Measured against the built binary — `!` answers `!H !M !L`, `!hi`
/// answers `!high`, `!me` answers `!medium !med` — which is exactly the shape
/// `--priority` has, and is why the two surfaces now agree instead of diverging
/// with a justification that was false.
///
/// Needs no [`super::lookup`]: the vocabulary is compiled in, so `!<TAB>` is the
/// one sugar prefix that answers without reading the store at all.
fn priority_candidates(typed: &str) -> Vec<CompletionCandidate> {
    Priority::SPELLINGS
        .iter()
        .filter(|(spelling, _)| spelling.starts_with(typed))
        .map(|(spelling, p)| {
            CompletionCandidate::new(format!("!{spelling}")).hide(*spelling != p.as_str())
        })
        .collect()
}

// ---- the read-side filter grammar ------------------------------------------

/// The filter DSL, for the positional tail of a command in
/// `argv::FILTER_COMMANDS` whose words are filter grammar and nothing else.
///
/// `report` is the one member that is NOT this — its first word may be a
/// `group_by` axis — and it takes [`report_words`] instead.
///
/// # Where the vocabulary comes from
///
/// `tasqx_core::filter`, variant by variant, and none of it is restated here.
/// The eight value prefixes, the two valueless keywords and the two operators
/// were all private to that module until this provider needed them; the
/// alternative was a second copy of twelve strings in a second crate, which is
/// the drift `filter::tests::token_shapes_name_every_value_prefix` already
/// caught ONCE inside core — where a guard could see it. Across a crate
/// boundary no guard here could. See `filter::VALUE_PREFIXES` for why the
/// visibility change is the cheaper half of that trade.
///
/// # Why an `ArgValueCompleter`, through `escaped_word_completer`
///
/// A completer because the seam has to READ the partial word: `+`, `-`,
/// `project:` and a bare `@w` are four different questions and only the word
/// says which. Through [`super::escaped_word_completer`] because this is a
/// filter positional and the argv pre-pass escapes the leading dash of every
/// `-tag` before the engine sees it — a bare `ArgValueCompleter` here is handed
/// `\u{1}ne` where the user typed `-ne` and answers nothing for every tag
/// EXCLUSION while `+tag` and `project:x` keep working. Four of five prefixes is
/// the worst version of this defect because it looks finished;
/// `complete::escaping_drift` fails the build for it.
pub(crate) fn filter_words() -> ArgValueCompleter {
    super::escaped_word_completer(filter_candidates)
}

/// `report`'s tail: [`filter_words`] plus the `group_by` axis its FIRST word may
/// be.
///
/// `report_params` (`lib.rs`) reads `args[0]` as a `group_by` when it names one
/// and treats the whole tail as a filter otherwise, so at the first word BOTH
/// vocabularies are legal — `tasqx report project` and `tasqx report +api` are
/// each valid and mean different things. At every later word only filter grammar
/// is: `tasqx report project status` exits 2 with `unknown filter token
/// "status"`. Measured against the built binary, both spellings.
///
/// # The residual over-offer, named rather than papered over
///
/// The axes are offered at `arg_index == 0`, and that index does not mean what
/// its name suggests: it is `0` for the first word AND for the second (measured;
/// see [`super::escaped_word_completer_at`] for the table and the upstream
/// lines). So `tasqx report project st<TAB>` — the second word, where a
/// `group_by` is no longer legal — is also offered `status`.
///
/// That is an over-offer and it is the lesser of the two available failures:
///
///  * choosing the candidate produces `unknown filter token "status"` on stderr
///    at exit 2, which is LOUD. Nothing is silently misfiled, nothing is
///    silently narrowed, and core's own message names the offending token and
///    lists the shapes that would have worked.
///  * withholding the axes entirely to avoid it would leave `tasqx report <TAB>`
///    — the primary spelling, and a closed compile-time vocabulary the tool
///    knows exactly — answering with filter tokens only. That is an under-offer
///    on the common case bought with a wrong menu removed from the rare one.
///
/// There is no third option: a completer is handed the index and the partial
/// word and nothing else, so "is this the first word?" is not a question this
/// seam can ask. If `clap_complete` ever passes a true word index, the `0` arm
/// below becomes exact and
/// `tests/completion.rs::a_report_group_by_is_offered_where_it_is_legal` is
/// where that shows up.
pub(crate) fn report_words() -> ArgValueCompleter {
    super::escaped_word_completer_at(|arg_index, word| {
        // The axes first: at the first word they are the likelier intent, and
        // clap's engine preserves the order a provider returns within one tag.
        let mut out = match arg_index {
            0 => group_by_candidates(word),
            _ => Vec::new(),
        };
        out.extend(filter_candidates(word));
        out
    })
}

/// The `report` axes, read out of `engine::SUMMARY_GROUP_BY` — the same const
/// `report_params` dispatches on and the MCP schema renders from.
///
/// Needs no [`super::lookup`]: the vocabulary is compiled in, like `!` on the
/// sugar surface.
fn group_by_candidates(typed: &str) -> Vec<CompletionCandidate> {
    tasqx_core::engine::SUMMARY_GROUP_BY
        .iter()
        .filter(|axis| axis.starts_with(typed))
        .map(|axis| CompletionCandidate::new(*axis))
        .collect()
}

/// The dispatcher: what filter token can follow the partial word `typed`?
///
/// A word carrying a value prefix asks about that prefix's vocabulary; anything
/// else is asking what shapes exist at all.
fn filter_candidates(typed: &str) -> Vec<CompletionCandidate> {
    let Some((prefix, vocabulary, value)) = value_prefix_of(typed) else {
        return token_shapes(typed);
    };
    // Matched variant by variant with no wildcard, which is why
    // `filter::Vocabulary` is deliberately not `#[non_exhaustive]`: a ninth
    // prefix added to core has to be a compile error here rather than a menu
    // that is silently empty for exactly the predicate nobody remembered.
    match vocabulary {
        // Both tag prefixes take the same vocabulary and the composed word is
        // what tells them apart — which is the round-trip gate's business, not
        // this arm's. `-` is the one that only works because of the escape
        // seam above.
        Vocabulary::Tag => composed(prefix, value, tag_names()),
        // ARCHIVED PROJECTS INCLUDED, unlike `--project` on a write command, and
        // the difference is what the receiving command does. `add`, `modify` and
        // `use` refuse an archived project outright, so offering one there is a
        // menu entry whose only outcome is an error. A filter is a READ and the
        // engine really does serve it: measured against the built binary, with a
        // project archived through `project.archive`, `tasqx list
        // project:oldstuff` prints its task, `export` exports it and `report`
        // counts it, all at exit 0. Withholding the name would make completion
        // offer LESS than the command accepts, which reads as "that project is
        // gone" — the same under-offer `projects_including_archived` was split
        // out to fix on `chart burndown`.
        Vocabulary::Project => composed(prefix, value, project_names(true)),
        // The one closed vocabulary in the grammar, and the only arm that needs
        // no store at all. `Status::ALL` is the same five variants `Status::parse`
        // accepts, so the menu and the refusal cannot disagree.
        //
        // `status:blocked` is deliberately absent although the grammar accepts
        // it: it is a third spelling of `@blocked`, which the keyword menu
        // already offers, and `Status::parse` refuses it anyway — so it falls out
        // of this list rather than being excluded by a rule stated here.
        Vocabulary::Status => composed(
            prefix,
            value,
            Status::ALL.iter().map(|s| s.as_str().to_string()).collect(),
        ),
        // An OPEN natural-language vocabulary, for the reason the sugar
        // dispatcher's date keys record: `due.before:` takes whatever
        // `datetime::parse_when` takes — `tomorrow`, `friday`, `in 3 days`,
        // `eom`, `2026-07-25` — and no module exports a list of accepted words
        // because there is no list. A hand-written menu of three of them would
        // be a fifth copy of a vocabulary that has four already (D30), and it
        // would teach that the other spellings do not work.
        //
        // This arm is belt-and-braces, and that was found by mutation rather
        // than reasoned: replacing it with `composed(prefix, value, [today,
        // tomorrow, friday])` changes NOTHING, because the round-trip gate
        // refuses every one of them — a date bound resolves its value away at
        // parse time, so `Filter::sole_value` answers `None` and no composed
        // date can ever survive. Emitting one takes bypassing `composed`
        // entirely, which is the shape `a_date_bound_offers_nothing_because_its
        // _vocabulary_is_open` was re-pointed at once that was measured.
        Vocabulary::Date => Vec::new(),
    }
}

/// The value prefix `word` carries, LONGEST first, plus what has been typed of
/// its value.
///
/// Longest-match for the reason `sugar::classify_partial` orders `VALUE_KEYS`
/// that way: the registry holds prefixes that could shadow one another, and
/// resolving by first match makes the answer depend on the order somebody
/// happened to write them in. Nothing in today's eight overlaps; that is a
/// property of today's eight, not of the lookup.
fn value_prefix_of(word: &str) -> Option<(&'static str, Vocabulary, &str)> {
    filter::VALUE_PREFIXES
        .iter()
        .filter_map(|&(prefix, vocabulary)| Some((prefix, vocabulary, word.strip_prefix(prefix)?)))
        .max_by_key(|(prefix, _, _)| prefix.len())
}

/// The token SHAPES the grammar accepts, for a word that has not committed to
/// one yet.
///
/// Three registries, joined and filtered by what has been typed, and no fourth
/// list of its own. A `key:` shape is offered as a stub the user goes on typing
/// — `p<TAB>` gives `project:`, and `project:<TAB>` then gives the names — which
/// is why this menu is NOT run through the round-trip gate that
/// [`composed`] applies: `status:` and `due.before:` do not parse on their own
/// and are not meant to, exactly as `and` and `or` do not. They are prefixes of
/// a token, not tokens, and the question this menu answers is "what may I type
/// here", not "what may I press Enter on".
///
/// **Not run through [`deliverable_as_one_word`] either**, and that is a
/// decision rather than an omission. That rule asks whether a shell can deliver
/// a value out of the USER'S STORE, where the answer is unknown and a wrong
/// guess files a task somewhere else; these twelve strings are tasqx's own fixed
/// grammar, printed in its help, its manual and its refusal messages.
///
/// # `@` in PowerShell, which is a real hazard and was measured wrongly
///
/// An earlier version of this comment claimed the rule would drop `@working` and
/// `@blocked` "for a hazard that does not exist", on the strength of running
/// `tasqx list @working` in PowerShell and seeing tasks. That probe cannot fail:
/// `@working` is pending|active, so an ordinary store returns the same rows
/// whether the token arrives or is thrown away.
///
/// With a probe that can fail, the hazard is there and it is the silent-drop
/// class:
///
/// ```text
///   PS> tasqx list @nonsensetoken     -> every task, exit 0
///   PS> tasqx list '@nonsensetoken'   -> unknown filter token, exit 2
/// ```
///
/// PowerShell's splatting operator claims a leading `@` even in an argument to a
/// native executable, and the token does not arrive mangled — it disappears, and
/// the command runs without it. Tab-completing `@blocked` there listed EVERY
/// task at exit 0.
///
/// So the two `@` shapes are emitted QUOTED under PowerShell and bare
/// everywhere else. That is possible only because this menu is reached through
/// an `ArgValueCompleter` and owns its own prefix filter: it matches on the bare
/// spelling — which is what the user is typing — and writes the quoted one. An
/// `ArgValueCandidates` is prefix-filtered by the engine against the word, so a
/// quoted candidate would vanish from the menu at the first keystroke, which is
/// the same trap `deliverable_as_one_word`'s doc records for quoted values.
///
/// Withholding them under PowerShell was the alternative and is worse: it is a
/// four-of-five under-offer on tasqx's own grammar, which is the shape this
/// module argues against everywhere else. Re-measure with a token that can fail
/// before changing any of this. The other eleven shapes were re-measured in
/// PowerShell and pass through untouched, so the special case stays as narrow as
/// the defect.
///
/// `(` and `)` are grammar terminals and are deliberately in none of the three
/// registries, so they are not offered. Both reasons point the same way: they
/// are lexical structure rather than predicates, and a bare `(` is the one
/// candidate no shell delivers — bash calls it a syntax error and PowerShell
/// opens a subexpression, which is precisely the class
/// [`deliverable_as_one_word`] refuses for a stored value.
fn token_shapes(typed: &str) -> Vec<CompletionCandidate> {
    let quote_at_signs = super::shell_is_powershell();
    filter::KEYWORDS
        .into_iter()
        .chain(filter::OPERATORS)
        .chain(filter::VALUE_PREFIXES.iter().map(|&(prefix, _)| prefix))
        // Matched on the BARE spelling, always: it is what the user is typing,
        // and filtering on the quoted one would make `@<TAB>` answer nothing.
        .filter(|shape| shape.starts_with(typed))
        .map(|shape| {
            if quote_at_signs && shape.starts_with('@') {
                CompletionCandidate::new(format!("'{shape}'"))
            } else {
                CompletionCandidate::new(shape)
            }
        })
        .collect()
}

/// Filter `values` by what has been typed, weld the prefix back on, and keep
/// only the words the FILTER PARSER reads back as the value they were built
/// from.
///
/// The twin of [`prefixed`] on the read side, and it is a separate function
/// rather than a parameter of that one because the two answer to different
/// parsers: `prefixed` asks `sugar::parsed_value_of` (the write path), this asks
/// `filter::Filter` (the read path), and a single function taking "which parser"
/// would be one call site deciding a question that belongs to the grammar.
///
/// # The round-trip gate, and the case that makes it a VALUE check
///
/// Asking only whether the composed word PARSES is not enough, and the counter-
/// example is not hypothetical: a tag genuinely named `blocked` composes
/// `+blocked`, which parses perfectly — as the derived blocked flag
/// (`filter::predicate` claims that spelling before the tag branch), not as the
/// tag. Offering it means Tab silently swaps "tasks tagged blocked" for "tasks
/// with an unresolved dependency", at exit 0, which is the silent-drop class
/// manufactured by the feature built to prevent it. `Filter::sole_value` answers
/// `None` there and the candidate is withheld.
///
/// The gate also covers the refusals a character rule would have had to restate:
/// a tag named `-lead` composes `--lead`, which the grammar refuses as a
/// mistyped flag (and *must* — that refusal is what lets `argv` tell a filter
/// token from a flag one token at a time), and an empty value composes a bare
/// `+`, which is an unknown token. Neither is spelled out here, because reading
/// the answer out of the parser is what keeps this from drifting when the
/// parser's refusals move — and they have moved once already.
///
/// # Where the cap goes
///
/// AFTER the prefix filter, exactly as [`prefixed`] does and for the reason
/// [`MAX_VALUE_CANDIDATES`] records: capping the sorted vocabulary first makes a
/// tag that exists and uniquely matches what was typed silently absent from its
/// own menu, which is a wrong answer rather than a shorter one. Measured at 252
/// tags on the sugar surface, where `+api<TAB>` answered nothing.
fn composed(prefix: &str, typed: &str, values: Vec<String>) -> Vec<CompletionCandidate> {
    values
        .into_iter()
        .filter(|value| value.starts_with(typed))
        .filter_map(|value| {
            let word = format!("{prefix}{value}");
            // TWO parsers, because two of them stand between the candidate and
            // the filter. `crate::argv` decides whether the word reaches the
            // filter tail at all on the CLI; `filter::Filter` decides what the
            // tail then means. Asking only the second offered `-h` for a tag
            // named `h` — valid filter grammar, and clap's help flag before it
            // ever gets there, so choosing it printed help at exit 0.
            (crate::argv::reaches_the_filter_tail(&word) && parses_back_to(&word, &value))
                .then_some(word)
        })
        .take(MAX_VALUE_CANDIDATES)
        .map(CompletionCandidate::new)
        .collect()
}

/// Does the filter parser take `value` back out of `word`?
///
/// `Timestamp::now()` for the reason `argv::filter_flag_error` gives for the
/// same call: `Filter::parse` needs an instant to resolve a relative date bound
/// against, and no composed candidate is ever a date — the [`Vocabulary::Date`]
/// arm returns nothing — so which instant it is cannot matter.
fn parses_back_to(word: &str, value: &str) -> bool {
    filter::Filter::parse(word, jiff::Timestamp::now()).is_ok_and(|f| f.sole_value() == Some(value))
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

    /// The value name that means "this argument's value IS a tag name".
    const TAG_VALUE_NAME: &str = "TAG";

    /// Drift guard: an argument announcing that its value is a tag must say how
    /// to complete one.
    ///
    /// The twin of [`every_project_valued_arg_offers_project_names`], and it
    /// exists because the pair was NOT symmetric for one commit: `+<TAB>` in the
    /// title position offered the whole tag vocabulary while `--tag` one
    /// argument away offered nothing, and no guard could have noticed, because
    /// the project convention keys on `<PROJECT>` and the derive renders these
    /// two `<TAGS>` off the plural field name.
    ///
    /// So the `value_name = "TAG"` at each declaration is load-bearing rather
    /// than cosmetic — it is what puts the argument inside this guard — and that
    /// is said at the declaration too, since an annotation whose purpose lives
    /// only in a test is one refactor from being tidied away.
    ///
    /// Mutation-proven: removing either attachment, or either `value_name`,
    /// reddens this test naming the argument (the `value_name` case through the
    /// floor below).
    #[test]
    fn every_tag_valued_arg_offers_tag_names() {
        let mut cmd = crate::Cli::command();
        cmd.build();

        let mut checked = 0;
        for (trail, sc) in all_subcommands_trailed(&cmd, "") {
            for arg in sc.get_arguments() {
                let name = arg
                    .get_value_names()
                    .and_then(|n| n.first().map(|s| s.as_str().to_string()))
                    .unwrap_or_else(|| arg.get_id().as_str().to_ascii_uppercase());
                if name != TAG_VALUE_NAME {
                    continue;
                }
                checked += 1;
                // An `ArgValueCompleter`, specifically, and not merely "some
                // provider": an `ArgValueCandidates` here is filtered by the
                // engine after the cap has already been applied, which is the
                // measured defect `tags()` documents. Accepting either shape
                // would let that regress silently.
                assert!(
                    arg.get::<ArgValueCompleter>().is_some(),
                    "`{trail} --{}` takes a tag name and offers no candidates \
                     from a completer, while `+<TAB>` on the same command offers \
                     every tag. Attach `add = crate::complete::candidates::tags()` \
                     — and note it must be a completer, so the typed word is \
                     applied before the candidate cap.",
                    arg.get_id()
                );
            }
        }
        assert!(
            checked >= KNOWN_TAG_VALUED_ARGS,
            "the guard found {checked} tag-valued arguments and the tree declares \
             at least {KNOWN_TAG_VALUED_ARGS}; a `value_name = \"TAG\"` was \
             dropped and this test is guarding less than it claims"
        );
    }

    /// Floor, not a list: `add --tag` and `modify --tag`.
    const KNOWN_TAG_VALUED_ARGS: usize = 2;

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
                // under `home`; see `deliverable_as_one_word`.
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
    /// [`deliverable_as_one_word`] before changing any of these: the `false`
    /// cases are values that would complete into a command that runs and does
    /// something else.
    #[test]
    fn only_values_a_shell_delivers_whole_are_offered() {
        for ok in [
            "work", "work.api", "a_b", "a-b", "C++", "v1/2", "ns:thing", "café",
        ] {
            assert!(
                deliverable_as_one_word(ok),
                "{ok:?} is deliverable unquoted"
            );
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
            // Word-splitting and expansion characters, one per family.
            "a;b",
            "a|b",
            "a$b",
            "a*b",
            "a(b",
        ] {
            assert!(
                !deliverable_as_one_word(bad),
                "{bad:?} must not be a candidate"
            );
        }

        // The leading-dash rule belongs to the WHOLE-WORD surface only. As a
        // standalone `--project` value clap reads it as a flag; welded behind a
        // sugar prefix it is `+-lead`, which is neither flag-shaped nor a
        // problem, and withholding it there was a rule stated where it is not
        // true.
        assert!(deliverable_as_one_word("-lead"));
        assert!(!standalone_word("-lead"));
        assert!(standalone_word("work"));
    }

    /// A candidate the SHELL can deliver is not automatically a candidate the
    /// PARSER accepts, and conflating the two shipped a silent misfile.
    ///
    /// `:x` is a legal project name and is perfectly deliverable; `project:` +
    /// `:x` is `project::x`, which `sugar::split_key` refuses as a Rust path. The
    /// task then went to the DEFAULT project with the candidate appended to its
    /// title, at exit 0 — measured against the built binary. The gate reads the
    /// answer out of `sugar::parsed_value_of` rather than restating a `::` rule
    /// that already moved once.
    ///
    /// Mutation-proven: dropping the `parsed_value_of` filter in [`prefixed`]
    /// reddens this test, and `every_offered_candidate_produces_the_command_it_promises`
    /// end to end.
    #[test]
    fn a_candidate_the_sugar_parser_would_refuse_is_not_offered() {
        let offered = |spelling: &str, typed: &str, names: &[&str]| -> Vec<String> {
            prefixed(
                spelling,
                typed,
                names.iter().map(|s| (*s).to_string()).collect(),
            )
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
        };

        assert_eq!(
            offered("project:", "", &[":x", "work"]),
            ["project:work"],
            "`project::x` is a Rust path to the parser, so it must not be a menu \
             entry — accepting it files the task under the default project"
        );
        assert_eq!(offered("proj:", "", &[":x", "work"]), ["proj:work"]);

        // The tag arm shares the gate and needs it for nothing today; asserted
        // anyway, because `tag_of`'s refusals are the ones most likely to grow.
        assert_eq!(offered("+", "", &["api", "docs"]), ["+api", "+docs"]);
        // A dash-led name survives here and is withheld at the flag surface —
        // the asymmetry the test above states, proven through the real seam.
        assert_eq!(offered("+", "", &["-lead"]), ["+-lead"]);
    }

    /// The cap bounds the MENU, not the vocabulary: a name that exists and
    /// uniquely matches what the user typed must be offered however late it
    /// sorts.
    ///
    /// This is the ordering [`MAX_VALUE_CANDIDATES`]'s doc describes and the code
    /// did not have for one commit: the cap sat in [`tags_from`], so on a store
    /// with more than two hundred tags `+api<TAB>` answered nothing while an
    /// api-tagged task sat in the store — the exact silent tag loss [`tag_names`]
    /// sends no `limit` to avoid, merely reordered alphabetically.
    ///
    /// Mutation-proven: putting `truncate(MAX_VALUE_CANDIDATES)` back into
    /// `tags_from` reddens this naming the tag it swallowed.
    #[test]
    fn the_cap_applies_after_the_prefix_not_before_it() {
        let many: Vec<String> = (0..MAX_VALUE_CANDIDATES * 2)
            .map(|i| format!("a{i:04}"))
            .chain(["zebra".to_string()])
            .collect();

        let late = prefixed("+", "zeb", many.clone());
        assert_eq!(
            late.len(),
            1,
            "`zebra` sorts past the cap but uniquely matches `zeb`, so Tab must \
             still find it; got {late:?}"
        );

        // And the cap is still a cap for what DOES survive the prefix.
        assert_eq!(prefixed("+", "a", many).len(), MAX_VALUE_CANDIDATES);

        // The readers hand the whole vocabulary over; capping there is what put
        // the cut before the prefix.
        let rows: Vec<Value> = many_tag_rows(MAX_VALUE_CANDIDATES * 2 + 1);
        assert_eq!(
            tags_from(&json!({ "tasks": rows })).len(),
            MAX_VALUE_CANDIDATES * 2 + 1,
            "`tags_from` must not truncate — the prefix has not been applied yet"
        );
    }

    /// One task per tag, which is the shape `task.list` really returns.
    fn many_tag_rows(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| json!({ "tags": [format!("t{i:04}")] }))
            .collect()
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

    // ---- the filter grammar ------------------------------------------------

    /// A bare partial word offers the SHAPES the grammar accepts, and it offers
    /// them out of core's registries rather than out of a list kept here.
    ///
    /// Asserted as an exact set built from those same registries, which sounds
    /// circular and is not: what it pins is that every entry reaches the menu and
    /// nothing else does. A prefix dropped from `VALUE_PREFIXES` reddens core's
    /// own `token_shapes_name_every_value_prefix`; a prefix silently skipped
    /// HERE — filtered out by some rule added later — reddens this.
    #[test]
    fn a_bare_partial_offers_the_grammar_shapes() {
        let want: Vec<String> = filter::KEYWORDS
            .into_iter()
            .chain(filter::OPERATORS)
            .chain(filter::VALUE_PREFIXES.iter().map(|&(p, _)| p))
            .map(str::to_string)
            .collect();
        assert_eq!(values(filter_candidates("")), want);
        // The registries really do carry the tokens a reader expects to see, so
        // the assertion above is not satisfiable by two empty lists.
        for shape in ["@working", "@blocked", "and", "or", "project:", "status:"] {
            assert!(want.iter().any(|w| w == shape), "{shape:?} missing");
        }

        // Filtered by what has been typed, case-sensitively, exactly as clap's
        // engine filters a possible value.
        assert_eq!(values(filter_candidates("@")), ["@working", "@blocked"]);
        assert_eq!(values(filter_candidates("@w")), ["@working"]);
        assert_eq!(values(filter_candidates("pro")), ["project:"]);
        assert_eq!(
            values(filter_candidates("due.")),
            ["due.before:", "due.after:"]
        );
        assert_eq!(values(filter_candidates("@W")), Vec::<String>::new());
        // Prose is not a filter shape and must offer nothing rather than the
        // whole grammar — the same rule the sugar dispatcher applies to a title.
        assert_eq!(values(filter_candidates("zzq")), Vec::<String>::new());
    }

    /// `(` and `)` are grammar terminals and are deliberately NOT offered.
    ///
    /// Pinned rather than left implicit, because "the registries happen not to
    /// contain them" is a fact somebody could undo in one line while believing
    /// they were completing the grammar. A bare `(` is the one candidate no shell
    /// delivers: bash calls it a syntax error and PowerShell opens a
    /// subexpression, which is the class `deliverable_as_one_word` refuses.
    #[test]
    fn parentheses_are_not_offered_because_no_shell_delivers_one() {
        for typed in ["", "(", ")"] {
            for got in values(filter_candidates(typed)) {
                assert!(
                    !got.contains('(') && !got.contains(')'),
                    "`list {typed}<TAB>` offered {got:?}, which a shell cannot \
                     insert as one word"
                );
            }
        }
        assert!(!deliverable_as_one_word("("));
    }

    /// `status:` is the one filter vocabulary that is closed and compiled in, so
    /// it answers without a store — and it answers with exactly `Status::ALL`.
    ///
    /// Driven off the enum rather than a list of five spellings, so a sixth
    /// variant joins the menu the day it exists rather than when someone
    /// remembers this test.
    #[test]
    fn a_status_prefix_offers_the_closed_status_set() {
        let want: Vec<String> = Status::ALL
            .iter()
            .map(|s| format!("status:{}", s.as_str()))
            .collect();
        assert_eq!(values(filter_candidates("status:")), want);
        assert_eq!(values(filter_candidates("status:d")), ["status:done"]);
        assert_eq!(
            values(filter_candidates("status:zz")),
            Vec::<String>::new(),
            "a value no status starts with must offer nothing, not everything"
        );
        // `status:blocked` is a third spelling of `@blocked` and is not a member
        // of the status set; the keyword menu is where the blocked flag lives.
        assert!(!values(filter_candidates("status:")).contains(&"status:blocked".to_string()));
    }

    /// The four date-shaped keys offer NOTHING, and that is the answer rather
    /// than a gap: they take whatever `datetime::parse_when` takes, which is an
    /// open natural-language vocabulary no module exports.
    ///
    /// Driven off the registry so the day a fifth date key is added it is covered
    /// without this test being touched — and so that a key whose vocabulary was
    /// mis-declared as `Date` shows up here as a menu that went quiet.
    #[test]
    fn a_date_bound_offers_nothing_because_its_vocabulary_is_open() {
        let mut seen = 0;
        for (prefix, vocabulary) in filter::VALUE_PREFIXES {
            if vocabulary != Vocabulary::Date {
                continue;
            }
            seen += 1;
            assert!(
                filter_candidates(prefix).is_empty(),
                "`{prefix}<TAB>` offered a menu for a vocabulary that has no list"
            );
            assert!(filter_candidates(&format!("{prefix}tomo")).is_empty());
        }
        assert_eq!(seen, 4, "the grammar declares four date bounds");
    }

    /// The round-trip gate, and the case that makes it a VALUE check rather than
    /// a parse check.
    ///
    /// A tag genuinely named `blocked` composes `+blocked`, which parses
    /// perfectly — as the derived blocked FLAG, because `filter::predicate`
    /// claims that spelling before it reaches the tag branch. Offering it means
    /// Tab silently swaps "tasks tagged blocked" for "tasks with an unresolved
    /// dependency", at exit 0. A gate that only asked "does this parse?" would
    /// ship it.
    ///
    /// Mutation-proven: dropping the `parses_back_to` filter in [`composed`]
    /// reddens this naming `+blocked`, and `-lead` with it.
    #[test]
    fn a_candidate_the_filter_parser_would_read_differently_is_not_offered() {
        let offered = |prefix: &str, typed: &str, names: &[&str]| -> Vec<String> {
            values(composed(
                prefix,
                typed,
                names.iter().map(|s| (*s).to_string()).collect(),
            ))
        };

        assert_eq!(
            offered("+", "", &["blocked", "api"]),
            ["+api"],
            "`+blocked` is the derived blocked flag, not the tag `blocked`, so \
             offering it answers a different question at exit 0"
        );
        // The exclusion spelling of the same tag is fine and must stay offered:
        // only `+blocked` is claimed by the flag.
        assert_eq!(offered("-", "", &["blocked"]), ["-blocked"]);
        // A tag whose name begins with a dash composes `--lead`, which the
        // grammar refuses as a mistyped flag — the refusal that lets `argv` tell
        // a filter token from a flag one token at a time.
        assert_eq!(offered("-", "", &["-lead", "api"]), ["-api"]);
        // ...and is perfectly includable, because `+-lead` is not flag-shaped.
        assert_eq!(offered("+", "", &["-lead"]), ["+-lead"]);
        // An empty value composes a bare `+`, an unknown token.
        assert_eq!(offered("+", "", &[""]), Vec::<String>::new());
        // The ordinary case still works, or the assertions above would be
        // satisfied by a gate that refuses everything.
        assert_eq!(
            offered("project:", "wo", &["work", "home"]),
            ["project:work"]
        );
    }

    /// The cap bounds the MENU, not the vocabulary — the filter surface's copy of
    /// the property `the_cap_applies_after_the_prefix_not_before_it` pins on the
    /// sugar surface, and it needs its own because it is a second `take` in a
    /// second function.
    ///
    /// Mutation-proven: moving the `take` above the `starts_with` filter in
    /// [`composed`] reddens this naming the tag it swallowed.
    #[test]
    fn the_filter_cap_applies_after_the_typed_word() {
        let many: Vec<String> = (0..MAX_VALUE_CANDIDATES * 2)
            .map(|i| format!("a{i:04}"))
            .chain(["zebra".to_string()])
            .collect();

        let late = values(composed("-", "zeb", many.clone()));
        assert_eq!(
            late,
            ["-zebra"],
            "`zebra` sorts past the cap but uniquely matches `zeb`, so \
             `list -zeb<TAB>` must still find it; got {late:?}"
        );
        assert_eq!(composed("+", "a", many).len(), MAX_VALUE_CANDIDATES);
    }

    /// `report`'s tail is the filter grammar PLUS the group_by axes, and only at
    /// the first word.
    ///
    /// Both positions are asserted because offering filter tokens where an axis
    /// belongs and offering an axis where only a filter token is legal are two
    /// different wrong menus, and a test of one position cannot see the other.
    /// The axes come from `engine::SUMMARY_GROUP_BY`, the same const
    /// `report_params` dispatches on.
    #[test]
    fn report_offers_its_axes_at_the_first_word_and_not_later() {
        // `report_words` is an `ArgValueCompleter`; drive it the way the engine
        // does rather than calling a private helper, so the wiring is included.
        let completer = report_words();
        let at = |index: usize, word: &str| -> Vec<String> {
            values(completer.complete_at(index, std::ffi::OsStr::new(word)))
        };

        let first = at(0, "");
        for axis in tasqx_core::engine::SUMMARY_GROUP_BY {
            assert!(
                first.iter().any(|c| c == axis),
                "`report <TAB>` must offer the axis {axis:?}, got {first:?}"
            );
        }
        // ...and the filter grammar alongside it, because `report +api` is a
        // valid first word too.
        assert!(first.iter().any(|c| c == "@working"), "got {first:?}");
        assert_eq!(at(0, "pri"), ["priority"]);
        // `project` the axis and `project:` the predicate are both legal at the
        // first word and both must be offered; they are different tokens.
        assert_eq!(at(0, "pro"), ["project", "project:"]);

        // Past the first word an axis is no longer legal — `tasqx report project
        // status` exits 2 with `unknown filter token "status"`, measured — so the
        // menu is filter grammar only.
        let later = at(1, "");
        for axis in tasqx_core::engine::SUMMARY_GROUP_BY {
            assert!(
                !later.iter().any(|c| c == axis),
                "the axis {axis:?} is offered where only a filter token parses, \
                 got {later:?}"
            );
        }
        assert!(later.iter().any(|c| c == "@working"), "got {later:?}");
        assert_eq!(at(1, "pro"), ["project:"]);
    }

    /// The two seams must agree about the grammar: whatever `list` offers for a
    /// word, `report` offers too, plus the axes at the first word and nothing
    /// else.
    ///
    /// It is the guard against the likelier of the two `report` mistakes — not a
    /// wrong axis, but a second copy of the filter dispatcher that drifts. There
    /// is one dispatcher and this says so.
    ///
    /// # Why `+` and `-` are not in the word list
    ///
    /// Those two reach the [`Vocabulary::Tag`] arm, which calls [`tag_names`] and
    /// so [`super::lookup`] — and `lookup` prefers a reachable DAEMON before it
    /// falls back to `$TASQX_DB`. In an in-process unit test that means opening
    /// whatever store the machine running `cargo test` points at. This was not a
    /// worry: it was measured, by copying a store to a directory holding only
    /// `tasks.db` and watching the `-shm`/`-wal` sidecars appear when this test
    /// ran alone.
    ///
    /// Two things were wrong with that, and the second is the one that bites.
    /// The suite must not read the developer's live data — the sibling test
    /// `a_title_word_offers_nothing_and_a_bang_offers_priorities` says so 300
    /// lines up and excludes the same arms for the same reason. And the body
    /// performs several independent reads and asserts them EQUAL, so on the
    /// maintainer's own machine, where tasqx is the live task manager and a
    /// daemon is the single writer, a task added between two of them reddens a
    /// test with nothing wrong in the code.
    ///
    /// The store-free words below already prove the relation this test is about
    /// — including a value arm, via `status:`, whose vocabulary is compiled in.
    /// The tag arms are proven against a SEEDED store through the real binary in
    /// `tests/completion.rs::a_filter_position_completes_the_shipped_grammar`,
    /// which is the only place that proof means anything anyway.
    #[test]
    fn report_adds_to_the_filter_menu_rather_than_replacing_it() {
        let filter = filter_words();
        let report = report_words();
        for word in ["", "@", "pro", "status:", "due.", "zzq"] {
            let os = std::ffi::OsStr::new(word);
            let shared = values(filter.complete_at(0, os));
            let extended = values(report.complete_at(0, os));
            assert!(
                extended.ends_with(&shared),
                "`report {word}<TAB>` must offer everything `list {word}<TAB>` \
                 does; got {extended:?} against {shared:?}"
            );
            assert_eq!(
                values(report.complete_at(1, os)),
                shared,
                "past the first word `report` is the filter grammar and nothing \
                 more"
            );
        }
    }
}
