//! The CLI's inline capture sugar, as a token RECOGNISER (DESIGN.md §5, D169).
//!
//! The parser that turns `add "fix crash +bug due:friday"` into fields lives in
//! the CLI (`tasqx-cli/src/sugar.rs`); only the judgement "is this word sugar"
//! lives here, because the JSON/MCP door needs it too. That door stores a
//! title verbatim — it parses no sugar — and `task.add`/`task.modify` name the
//! sugar-shaped words they stored in a `warnings` entry (#621), so an agent
//! carrying a CLI habit is told rather than silently handed a task with no tag
//! and no due date. One recogniser, so the warning and the CLI can never
//! disagree about what counts as sugar (D30).

/// Which field a value key fills.
///
/// The key's *spelling* is data; the field it fills is a type. Dispatching on
/// this rather than on the string is what makes the loop's arm list exhaustive
/// — the compiler checks every key has a home, so an alias cannot be added to
/// the table and then quietly go nowhere.
/// `Copy` so the table can be read by value; the loop matches on patterns and
/// needs no `PartialEq`.
///
/// Public since the CLI's shell-completion dispatcher branches on it. That is
/// the point of the type rather than a leak of it: `complete::candidates` has
/// to answer "what can follow `project:`?" differently from "what can follow
/// `due:`?", and matching the VARIANT means the compiler asks that question
/// again for every key added to [`VALUE_KEYS`]. Matching the spelling would let
/// a new key quietly inherit whichever arm its string happened to fall into.
#[derive(Clone, Copy)]
pub enum ValueKey {
    /// `project:` / `proj:`.
    Project,
    /// `due:`.
    Due,
    /// `scheduled:` / `sched:`.
    Scheduled,
    /// `wait:`.
    Wait,
    /// The value IS the rule (`repeat:`/`recur:`).
    Repeat,
    /// The value is the rule's tail; `every ` is prepended (`every:`).
    Every,
    /// `remind:`.
    Remind,
    /// `estimate:` / `est:`.
    Estimate,
}

/// Sugar keys that take a *value*, longest-first so `estimate:` is tested before
/// `est:` and `project:` before `proj:`. Used to spot an argv element the shell
/// already quoted for us, and — via [`split_key`] — to dispatch the CLI's parse loop,
/// so the two cannot disagree about what counts as sugar (D30).
///
/// Longest-first is not cosmetic: it is the ONLY thing that stops `estimate:x`
/// being read as the estimate `imate:x`, and it is load-bearing again now that a
/// declined key must not fall through to its own shorter alias. See [`split_key`].
pub const VALUE_KEYS: [(&str, ValueKey); 12] = [
    ("scheduled:", ValueKey::Scheduled),
    ("estimate:", ValueKey::Estimate),
    ("project:", ValueKey::Project),
    ("remind:", ValueKey::Remind),
    ("repeat:", ValueKey::Repeat),
    ("every:", ValueKey::Every),
    ("recur:", ValueKey::Repeat),
    ("sched:", ValueKey::Scheduled),
    ("proj:", ValueKey::Project),
    ("wait:", ValueKey::Wait),
    ("due:", ValueKey::Due),
    ("est:", ValueKey::Estimate),
];

/// The tag a `+` token names, or `None` if it names none.
///
/// C6's rule for `+`: a token this declines must reach the TITLE. A bare `+`
/// used to be claimed by the tag branch, fail the non-empty check, and then be
/// unreachable by the title branch — pure deletion, at exit 0, with no warning.
/// `tasqx add -- "Implement Display + std::error::Error"` stored a title with no
/// `+` in it and created no tag. `+` is ordinary prose in a technical title
/// ("Display + Error", "C++", "a + b"), so the loss is not exotic.
pub fn tag_of(tok: &str) -> Option<&str> {
    tok.strip_prefix('+').filter(|t| !t.is_empty())
}

/// The value key a token opens with, and the value after it — or `None` when the
/// token is not sugar at all and belongs to the title.
///
/// Two refusals, both of which used to be silent corruption:
///
/// **`::` is a path separator, not a key.** Every value key is also a plausible
/// first segment of a Rust module path, which is this project's own task
/// vocabulary. A bare `strip_prefix` read `recur::advance_once` as the
/// recurrence rule `":advance_once"` and refused the whole command, naming a
/// rule the user never wrote; `project::config` was worse — accepted, project
/// silently set to `:config`, and the word removed from the title.
///
/// **An empty value names nothing**, so `due:` alone is a word. It used to be
/// claimed by its branch and then dropped, exactly like the bare `+`.
///
/// The key is resolved ONCE, by first prefix match against [`VALUE_KEYS`], and
/// only then judged. Chaining `strip_prefix` per alias instead — the shape this
/// replaced — re-tested the shorter alias against a token the longer one had
/// already declined, so `project::config` failed `project:` and then matched
/// `proj:`, setting the project to `ect::config`.
pub fn split_key(tok: &str) -> Option<(ValueKey, &str)> {
    let (key, value) = VALUE_KEYS
        .iter()
        .find_map(|&(spelling, key)| Some((key, tok.strip_prefix(spelling)?)))?;
    (!value.is_empty() && !value.starts_with(':')).then_some((key, value))
}

/// Does this token reach a sugar branch of the CLI's `parse_add` loop rather than the
/// title branch?
///
/// Derived from the very functions the loop dispatches on — [`tag_of`],
/// [`split_key`], and the colon-less `!` — so a new sugar key joins this answer
/// by being added to [`VALUE_KEYS`], not by someone remembering a second list,
/// and a token one of them DECLINES is title text here too. That is D30's rule
/// ("when a fix can be spelled 'derive it' or 'keep a list in sync', derive it")
/// at the one place where getting it wrong decides between storing an element
/// verbatim and rejoining its words.
///
/// It matters in both directions. When this said `starts_with('+')` while the
/// loop required a non-empty tag, `add "Display + Error"` was denied the
/// verbatim path AND had its `+` eaten by the loop. When it said
/// `starts_with("recur:")`, `add "fix recur::advance_once"` was likewise denied
/// it and lost the word.
pub fn is_sugar_token(t: &str) -> bool {
    tag_of(t).is_some() || t.starts_with('!') || split_key(t).is_some()
}

/// The words of `title` the CLI would have read as inline sugar, as typed.
///
/// Split with [`crate::filter::split_words`], the scanner the CLI's sugar
/// uses, so `due:"in 3 days"` is one word; a title that scanner refuses (an
/// unterminated quote) falls back to whitespace, since this only names words
/// and never rejects a title the JSON door accepts.
pub fn inline_sugar_tokens(title: &str) -> Vec<String> {
    let words: Vec<String> = match crate::filter::split_words(title, "title") {
        Ok(words) => words.into_iter().map(|w| w.text).collect(),
        Err(_) => title.split_whitespace().map(str::to_string).collect(),
    };
    words.into_iter().filter(|w| is_sugar_token(w)).collect()
}

/// The additive `warnings` entry for a title carrying sugar, or `None`.
pub fn title_sugar_warning(title: &str) -> Option<String> {
    let found = inline_sugar_tokens(title);
    (!found.is_empty()).then(|| {
        format!(
            "title contains CLI inline sugar this door does not parse: {} — stored verbatim; \
             set tags, due, project, priority and estimate through their own fields",
            found.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_sugar_words_and_nothing_else() {
        assert_eq!(
            inline_sugar_tokens("fix crash +bug due:friday project:x !H est:2h"),
            ["+bug", "due:friday", "project:x", "!H", "est:2h"]
        );
        assert_eq!(
            inline_sugar_tokens(r#"ship due:"in 3 days""#),
            ["due:in 3 days"]
        );
        for prose in [
            "Display + Error",
            "fix recur::advance_once",
            "due: soon",
            r"\+x",
            "C++",
        ] {
            assert!(inline_sugar_tokens(prose).is_empty(), "{prose}");
        }
        assert_eq!(inline_sugar_tokens(r#"odd "quote +tag"#), ["+tag"]);
    }
}
