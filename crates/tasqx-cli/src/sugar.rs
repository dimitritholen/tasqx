//! Inline capture sugar for `tasqx add` / `modify` (DESIGN.md §5, §10).
//!
//! Extracts structured fields from the free-text title, leaving the remaining
//! words as the title:
//!  * `+tag` — quote to include spaces, e.g. `+"needs paint"`, exactly like the
//!    value keys below and like the filter grammar's `+` on the read side.
//!  * `project:<name>` / `proj:<name>`
//!  * `!<prio>` (`!high`, `!h`)
//!  * date keys `due:` / `scheduled:` / `wait:` — values are **natural-language
//!    date expressions** resolved later by [`tasqx_core::datetime`]; quote to
//!    include spaces, e.g. `due:"in 3 days"` or `due:friday`.
//!  * recurrence keys `repeat:` / `every:` — a D2 rule string, e.g.
//!    `repeat:"every 3 days"` or `every:"weekly on mon,wed,fri"`.
//!  * reminder key `remind:` — either a `due`-anchored offset (`remind:-1h`,
//!    `remind:-30m`) or an absolute date expression (`remind:"friday 9am"`).
//!    §9's quiet-by-default rule means this key is the *only* thing that puts a
//!    task on the reminder heap.
//!  * estimate keys `est:` / `estimate:` — a human duration (`est:4h`,
//!    `est:1h30m`), resolved by [`tasqx_core::datetime::parse_duration`].
//!
//! Quoting is ONE rule, not a write-side dialect: this module splits with
//! [`tasqx_core::filter::split_words`], the scanner the read-side grammar
//! documents and owns. So `\"` is a literal quote and `\\` a literal backslash
//! here exactly as in a filter, an unterminated quote is refused here exactly as
//! there, and any value `filter::quote` can emit is a value `add` can type.
//! Before they shared a scanner they disagreed: `add '+say"hi'` stored the tag
//! `sayhi` under a zero exit code, and a value containing a quote could not be
//! written at all — which made the escape the read side documents unmatchable.
//!
//! Two spellings reach the same value, as on the read side: the shell-stripped
//! `project:"Home Renovation"` (the shell eats the quotes and the argument
//! boundary tells us it is one value) and the literal `project:"Home
//! Renovation"` passed through intact. The one value that cannot use the first
//! spelling is one containing a `"` — an element carrying a literal quote goes
//! to the scanner on both sides, so `project:'My "Big" Project'` is three tokens
//! on both. Its spelling is the escaped one, `project:"My \"Big\" Project"`, and
//! that is the same on both sides too.
//!
//! A key needs a value to be a key, and its colon must stand alone. `due:` with
//! nothing after it is the word `due:`, and `recur::advance_once` is a Rust
//! path, not the recurrence rule `:advance_once` — every one of these keys is
//! also a plausible first segment of a module path, which is the vocabulary this
//! project's own tasks are written in. A `+` naming no tag is likewise the
//! character `+`, as in `Display + Error`. Sugar that declines a token LEAVES
//! it, in the title, spelled as typed; it never consumes and discards one.
//!
//! Explicit flags win over inline sugar. Date/recurrence/reminder/estimate
//! *values* are carried out verbatim (unparsed); the caller resolves them through
//! the one core parser so sugar and flags share identical parsing.
//!
//! `add` and `modify` share this parser exactly (DESIGN.md §12-D13): the same
//! token means the same thing in both, and only the *absence* of a token differs
//! — for `add` it means "no value", for `modify` it means "leave this field
//! alone". Clearing is therefore not expressible here; it is `--clear <field>`.

use tasqx_core::ApiError;

pub struct ParsedAdd {
    pub title: String,
    pub project: Option<String>,
    pub priority: Option<String>,
    pub tags: Vec<String>,
    /// Raw NL date expressions (unresolved) — the caller parses these.
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub wait: Option<String>,
    /// Raw recurrence rule string (unvalidated) — the core validates it.
    pub recurrence: Option<String>,
    /// Raw reminder spec (unvalidated): a `due`-anchored offset or an absolute
    /// date expression. The core validates + normalizes it (§9).
    pub remind: Option<String>,
    /// Raw estimate (unparsed), e.g. `4h` — the caller resolves it to ISO-8601.
    pub estimate: Option<String>,
    /// The project name came from an UNQUOTED `project:`/`proj:` sugar token,
    /// which ends at the first space. If the core then cannot find that name we
    /// genuinely do not know whether it is a typo or the first word of a longer
    /// name the tokenizer never saw, and the message must not pick one and state
    /// it as fact — `project:My "Big" Project` used to answer `no project named
    /// My (create it with `tasqx init My`)` about a project that existed.
    /// False when the name was quoted or came from `--project`: those are whole
    /// by construction, so a miss there really is a typo.
    ///
    /// Also false when NOTHING followed the token. The tokenizer can only cut a
    /// name short by leaving the rest of it in a LATER token, so `add task
    /// project:Zzz` — where `project:` is last — had nothing to lose, and
    /// hedging there described a cut that could not have happened and sent the
    /// user looking for a longer name that never existed.
    pub project_may_be_truncated: bool,
}

/// Flags supplied explicitly on the command line. Each wins over inline sugar.
#[derive(Default)]
pub struct AddFlags {
    pub project: Option<String>,
    pub priority: Option<String>,
    pub tags: Vec<String>,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub wait: Option<String>,
    pub repeat: Option<String>,
    pub remind: Option<String>,
    pub estimate: Option<String>,
}

/// Which field a value key fills.
///
/// The key's *spelling* is data; the field it fills is a type. Dispatching on
/// this rather than on the string is what makes the loop's arm list exhaustive
/// — the compiler checks every key has a home, so an alias cannot be added to
/// the table and then quietly go nowhere.
/// `Copy` so the table can be read by value; the loop matches on patterns and
/// needs no `PartialEq`.
#[derive(Clone, Copy)]
enum ValueKey {
    Project,
    Due,
    Scheduled,
    Wait,
    /// The value IS the rule (`repeat:`/`recur:`).
    Repeat,
    /// The value is the rule's tail; `every ` is prepended (`every:`).
    Every,
    Remind,
    Estimate,
}

/// Sugar keys that take a *value*, longest-first so `estimate:` is tested before
/// `est:` and `project:` before `proj:`. Used to spot an argv element the shell
/// already quoted for us, and — via [`split_key`] — to dispatch the parse loop,
/// so the two cannot disagree about what counts as sugar (D30).
///
/// Longest-first is not cosmetic: it is the ONLY thing that stops `estimate:x`
/// being read as the estimate `imate:x`, and it is load-bearing again now that a
/// declined key must not fall through to its own shorter alias. See [`split_key`].
const VALUE_KEYS: [(&str, ValueKey); 12] = [
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

/// Parse argv words plus explicit flags into structured fields.
///
/// Taking the argv **slice** rather than a pre-joined string is load-bearing.
/// `tasqx modify 4 repeat:"every 3 days"` reaches us as the single argv element
/// `repeat:every 3 days` — the shell already consumed the quotes and told us,
/// by way of the argument boundary, that this is one value. Joining argv back
/// into a string and re-splitting on whitespace threw that away and silently
/// mis-parsed: `project:"my big project"` set the project to `my` and renamed
/// the task to `big project`, with no error at all.
///
/// So: an element that opens with a value key that OWNS a value — or with `+`,
/// which is a value key spelled without a colon (C2) — and carries spaces (and
/// no embedded quotes of its own) is honored whole. Everything else is tokenized
/// as before, which keeps the classic one-big-quoted-string capture form —
/// `add "Ship it due:friday +api"` — parsing exactly as it always has.
/// "Owns a value" is [`split_key`]'s judgement and not `starts_with`'s: an
/// element the loop will hand to the title must not first be honored whole here.
///
/// The trade: `add "due:friday Ship it"` (a value key opening a quoted title)
/// now reads the whole remainder as the date and fails with a clean
/// `could not parse date: "friday Ship it"`. Title-first is the documented and
/// universally exemplified form; a loud error on the rare inversion is a better
/// deal than silent corruption on the common quoted value.
///
/// C6: an unusable token is refused, never dropped. `!urgent` used to be
/// consumed by the `!` branch, fail `normalize_prio`, and vanish — not applied,
/// not reported, not even left in the title. See [`normalize_prio`].
///
/// C6 has a quieter twin, and every arm below now obeys it: a token a sugar arm
/// declines must reach the TITLE. The arms used to claim a token on its first
/// character and then drop it on failing their own inner check, so a bare `+`
/// and a valueless `due:` were deleted outright at exit 0. The claim and the
/// check are now one decision — [`tag_of`] and [`split_key`] — so there is no
/// longer a state between "claimed" and "used". Only `!` still refuses loudly
/// instead, because a bang-word has no escape into title text.
pub fn parse_add(args: &[String], flags: AddFlags) -> Result<ParsedAdd, ApiError> {
    let mut title_words: Vec<String> = Vec::new();
    let mut tags: Vec<String> = flags.tags;
    let mut project = flags.project;
    let mut priority = flags.priority;
    let mut due = flags.due;
    let mut scheduled = flags.scheduled;
    let mut wait = flags.wait;
    // `every:X` is a shorthand for the rule `every X`; `repeat:X` is the full rule.
    let mut recurrence = flags.repeat;
    let mut remind = flags.remind;
    let mut estimate = flags.estimate;
    let mut project_may_be_truncated = false;

    // Collected rather than streamed because the truncation hedge is a question
    // about POSITION: a `project:` token can only have been cut short if some
    // later token holds the rest of the name, and that is unknowable until the
    // end of the stream.
    let toks = tokenize_argv(args)?;
    let last_tok = toks.len().saturating_sub(1);

    for (i, SugarTok { text: tok, quoted }) in toks.into_iter().enumerate() {
        if let Some(tag) = tag_of(&tok) {
            if !tags.iter().any(|t| t == tag) {
                tags.push(tag.to_string());
            }
        } else if let Some((key, v)) = split_key(&tok) {
            match key {
                ValueKey::Project => {
                    if project.is_none() {
                        project = Some(v.to_string());
                        // Unquoted AND something follows it: only then is there
                        // a word the tokenizer could have taken off the end of
                        // this name.
                        project_may_be_truncated = !quoted && i < last_tok;
                    }
                }
                ValueKey::Due => set_if_empty(&mut due, v),
                ValueKey::Scheduled => set_if_empty(&mut scheduled, v),
                ValueKey::Wait => set_if_empty(&mut wait, v),
                ValueKey::Repeat => set_if_empty(&mut recurrence, v),
                // `every:X` is the rule `every X`; the key carries the tail only.
                // Not routed through `set_if_empty`, which would build the rule
                // string before discovering an explicit `--repeat` already won.
                ValueKey::Every => {
                    if recurrence.is_none() {
                        recurrence = Some(format!("every {v}"));
                    }
                }
                ValueKey::Remind => set_if_empty(&mut remind, v),
                ValueKey::Estimate => set_if_empty(&mut estimate, v),
            }
        } else if let Some(p) = tok.strip_prefix('!') {
            // Validated even when an explicit --priority already won, so a typo
            // is never excused by the flag that happened to outrank it: the flag
            // decides which *valid* value applies, not whether the token parses.
            let v = normalize_prio(p).ok_or_else(|| bad_priority(p))?;
            if priority.is_none() {
                priority = Some(v);
            }
        } else {
            title_words.push(tok);
        }
    }

    Ok(ParsedAdd {
        title: title_words.join(" "),
        project,
        priority,
        tags,
        due,
        scheduled,
        wait,
        recurrence,
        remind,
        estimate,
        project_may_be_truncated,
    })
}

fn set_if_empty(slot: &mut Option<String>, v: &str) {
    if slot.is_none() && !v.is_empty() {
        *slot = Some(v.to_string());
    }
}

/// The tag a `+` token names, or `None` if it names none.
///
/// C6's rule for `+`: a token this declines must reach the TITLE. A bare `+`
/// used to be claimed by the tag branch, fail the non-empty check, and then be
/// unreachable by the title branch — pure deletion, at exit 0, with no warning.
/// `tasqx add -- "Implement Display + std::error::Error"` stored a title with no
/// `+` in it and created no tag. `+` is ordinary prose in a technical title
/// ("Display + Error", "C++", "a + b"), so the loss is not exotic.
fn tag_of(tok: &str) -> Option<&str> {
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
fn split_key(tok: &str) -> Option<(ValueKey, &str)> {
    let (key, value) = VALUE_KEYS
        .iter()
        .find_map(|&(spelling, key)| Some((key, tok.strip_prefix(spelling)?)))?;
    (!value.is_empty() && !value.starts_with(':')).then_some((key, value))
}

/// Turn argv into sugar tokens, respecting boundaries the shell already drew.
///
/// An element like `repeat:every 3 days` only exists because the user wrote
/// `repeat:"every 3 days"` and the shell stripped the quotes — re-splitting it
/// would discard their intent. An element that still carries its own quotes
/// (`repeat:"every 3 days"` passed through literally) goes to [`tokenize`],
/// which understands them.
fn tokenize_argv(args: &[String]) -> Result<Vec<SugarTok>, ApiError> {
    let mut out = Vec::new();
    for arg in args {
        // `split_key`, not a bare `starts_with` over VALUE_KEYS: an element the
        // parse loop will hand to the title must not first be honored whole as a
        // "value". Otherwise `add "fix recur::advance_once and bound it"` became
        // ONE title word carrying the whole element, since the value it was
        // honored as was then declined downstream.
        let shell_quoted_value = !arg.contains('"')
            && arg.chars().any(char::is_whitespace)
            && (split_key(arg).is_some() || is_spaced_tag(arg));
        if shell_quoted_value {
            // The shell drew this boundary, so the value is quoted in every
            // sense that matters here — nothing about it was guessed.
            out.push(SugarTok {
                text: arg.clone(),
                quoted: true,
            });
        } else {
            let toks = tokenize(arg)?;
            // D36's STORAGE half — "accepted values are stored as given; the
            // trim decides validity, not storage" — which held on the JSON door
            // and not here. An element carrying no sugar IS the title text the
            // user typed, so it is handed over whole instead of as words that
            // the title branch later rejoins with a single space. Rejoining did
            // not merely trim the ends: it rewrote the MIDDLE of the title
            // (`a    b` became `a b`, a literal tab became a space), so
            // `tasqx add "  x  "` and `task.add {"title":"  x  "}` wrote
            // different bytes for one intent — the CLI-vs-API divergence this
            // project keeps paying for.
            //
            // Deliberately narrow. The moment ANY token in the element is sugar
            // the classic one-big-quoted-string capture form is in play
            // (`add "Ship it due:friday +api"`), and there the words are all
            // that can honestly be reconstructed — the sugar tokens are being
            // removed from the middle, so the original spacing no longer
            // describes the title that remains.
            //
            // An element that tokenizes to NOTHING (whitespace only) must keep
            // falling through: dropping it is what makes `tasqx add "   "`
            // reach `req_str` empty and be refused, which D36 requires.
            //
            // The element ITSELF is tested, not only its words, because it is
            // the element that gets pushed. `+ foo` tokenizes to two innocent
            // words (`+` names no tag, `foo` is a word) yet is sugar whole — the
            // loop would read the pushed element as the tag `" foo"`, which is
            // the very mint-a-space-tag outcome `is_spaced_tag` exists to stop.
            let is_pure_title = !arg.contains('"')
                && !toks.is_empty()
                && !is_sugar_token(arg)
                && !toks.iter().any(|t| is_sugar_token(&t.text));
            if is_pure_title {
                out.push(SugarTok {
                    text: arg.clone(),
                    quoted: false,
                });
            } else {
                out.extend(toks);
            }
        }
    }
    Ok(out)
}

/// One sugar token, plus whether any part of it arrived quoted.
///
/// The flag exists for exactly one consumer: an unquoted `project:` value ends
/// at a space the user may well have typed, so a name that fails to resolve may
/// be a FRAGMENT rather than a typo, and the error has to say which it cannot
/// tell. See `ParsedAdd::project_may_be_truncated`.
struct SugarTok {
    text: String,
    quoted: bool,
}

/// Does this token reach a sugar branch of [`parse_add`]'s loop rather than the
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
fn is_sugar_token(t: &str) -> bool {
    tag_of(t).is_some() || t.starts_with('!') || split_key(t).is_some()
}

/// `+tag` is a value key without the colon, so it obeys the same whole-element
/// rule as one.
///
/// `add "painting job" +"needs paint"` reaches us as the element `+needs paint`;
/// re-splitting it stored the tag `needs` and, because the leftover word fell
/// through to the title branch, silently renamed the task to `painting job
/// paint`. On `modify` the same split rewrote the title to `job` outright.
///
/// Stricter than [`tag_of`] on exactly one point: the `+` must be followed by
/// actual content, not by the space itself. `+ foo` does name no tag either way,
/// but honouring it WHOLE here would mint the tag `" foo"` instead.
fn is_spaced_tag(arg: &str) -> bool {
    tag_of(arg).is_some_and(|t| !t.starts_with(char::is_whitespace))
}

/// Whitespace-split, but keep double-quoted spans together and strip the quotes,
/// so `due:"in 3 days"` becomes the single token `due:in 3 days`.
///
/// C8: this is [`tasqx_core::filter::split_words`] and nothing else — the ONE
/// quoting rule, owned by the read side's grammar, which documents it and is
/// tested against it. It used to be a second tokenizer written here, and the two
/// disagreed about the same syntax: `"` was a pure delimiter with no escapes, so
/// `add '+say"hi'` stored the tag `sayhi` and `project:"My \"Big\" Project"`
/// stored `My \Big\ Project`. A value containing a quote could not be written at
/// all, which made the escape the read side documents unmatchable for tags.
///
/// Fallible for the reason the read side is: an unterminated quote is refused,
/// not closed at end of input. Guessing where the user meant it to end is
/// exactly how `sayhi` got stored under a zero exit code.
fn tokenize(raw: &str) -> Result<Vec<SugarTok>, ApiError> {
    let words = tasqx_core::filter::split_words(raw, "task text").map_err(ApiError::bad_request)?;
    Ok(words
        .into_iter()
        .map(|w| SugarTok {
            text: w.text,
            quoted: w.quoted,
        })
        .collect())
}

/// Names the value and every spelling that would have worked, because `!` has no
/// escape: there is no way to mean a literal bang-word in a title, so the
/// message has to carry the whole way out rather than assume a retype is obvious.
fn bad_priority(s: &str) -> ApiError {
    ApiError::bad_request(format!(
        "invalid priority: {s:?} (try !h/!high, !m/!medium, !l/!low — or drop the ! to keep it as title text)"
    ))
}

/// `None` is a *rejection*, not a shrug — the caller must turn it into
/// [`bad_priority`]. Returning the token to the title instead would be the
/// quieter failure: the user asked for a priority and would get a renamed task.
fn normalize_prio(s: &str) -> Option<String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "h" | "high" => Some("H".into()),
        "m" | "medium" | "med" => Some("M".into()),
        "l" | "low" => Some("L".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classic capture form: ONE shell argument carrying the whole title and
    /// its sugar, which the parser re-tokenizes itself.
    fn parse1(raw: &str, flags: AddFlags) -> ParsedAdd {
        parse_add(&[raw.to_string()], flags).expect("parses")
    }

    /// The shell-tokenized form: several argv words, quotes already consumed by
    /// the shell — what `tasqx modify 4 repeat:"every 3 days"` really delivers.
    fn parse_argv(args: &[&str], flags: AddFlags) -> ParsedAdd {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_add(&owned, flags).expect("parses")
    }

    /// The same argv, kept as an error so the rejection itself can be asserted on.
    fn parse_err(args: &[&str], flags: AddFlags) -> ApiError {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        // `.err()` rather than `expect_err`, which would demand Debug on the
        // success type purely to serve a test.
        parse_add(&owned, flags).err().expect("must be refused")
    }

    /// C6: the token must not evaporate. The message names the value and every
    /// spelling that works, since a bare `!` has no escape into title text.
    #[test]
    fn an_unusable_priority_token_is_refused_not_dropped() {
        let e = parse_err(&["urgent thing", "!urgent"], AddFlags::default());
        assert!(
            e.message.contains("urgent"),
            "names the value: {}",
            e.message
        );
        assert!(
            e.message.contains("!high"),
            "lists the way out: {}",
            e.message
        );

        // A lone `!` named no priority either, and vanished just as quietly.
        assert!(parse_err(&["!"], AddFlags::default())
            .message
            .contains("invalid priority"));
    }

    /// An explicit flag outranks sugar on *value*, never on *validity* — the
    /// alternative excuses a typo whenever a flag happens to be present too.
    #[test]
    fn a_winning_flag_does_not_excuse_an_invalid_sugar_token() {
        let flags = AddFlags {
            priority: Some("H".into()),
            ..AddFlags::default()
        };
        assert!(parse_err(&["Task", "!urgent"], flags)
            .message
            .contains("invalid priority"));
    }

    /// Every accepted spelling, pinned so the error's promised way out is real.
    #[test]
    fn every_documented_priority_spelling_still_parses() {
        for (tok, want) in [
            ("!h", "H"),
            ("!high", "H"),
            ("!m", "M"),
            ("!med", "M"),
            ("!medium", "M"),
            ("!l", "L"),
            ("!low", "L"),
            ("!HIGH", "H"),
        ] {
            let p = parse_argv(&["Task", tok], AddFlags::default());
            assert_eq!(p.priority.as_deref(), Some(want), "{tok} must parse");
            assert_eq!(p.title, "Task", "{tok} must not leak into the title");
        }
    }

    /// The walk found this by typing what the docs show. `modify 4 repeat:"every
    /// 3 days"` arrives as ONE argv element with the quotes already gone; the
    /// old join-then-resplit read `every` as the whole rule. Worse,
    /// `project:"my big project"` silently set project=`my` and RENAMED the task
    /// to `big project` — no error, wrong data.
    #[test]
    fn a_shell_quoted_value_survives_as_one_token() {
        let p = parse_argv(&["repeat:every 3 days"], AddFlags::default());
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));
        assert_eq!(p.title, "", "the rule's words must not leak into the title");

        let p = parse_argv(&["project:my big project"], AddFlags::default());
        assert_eq!(p.project.as_deref(), Some("my big project"));
        assert_eq!(p.title, "", "the project's words must not become a title");

        let p = parse_argv(&["due:in 3 days"], AddFlags::default());
        assert_eq!(p.due.as_deref(), Some("in 3 days"));
        assert_eq!(p.title, "");

        let p = parse_argv(&["est:1h 30m"], AddFlags::default());
        assert_eq!(p.estimate.as_deref(), Some("1h 30m"));

        let p = parse_argv(&["every:3 days"], AddFlags::default());
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));

        let p = parse_argv(&["remind:friday 9am"], AddFlags::default());
        assert_eq!(p.remind.as_deref(), Some("friday 9am"));
    }

    /// The shell-tokenized multi-word form: new title words and sugar, mixed.
    #[test]
    fn argv_words_mix_title_and_sugar() {
        let p = parse_argv(
            &["Renamed", "task", "due:friday", "+api", "!high", "est:4h"],
            AddFlags::default(),
        );
        assert_eq!(p.title, "Renamed task");
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.priority.as_deref(), Some("H"));
        assert_eq!(p.estimate.as_deref(), Some("4h"));
        assert_eq!(p.tags, vec!["api".to_string()]);
    }

    /// A value element that kept its literal quotes still works — the quote-aware
    /// tokenizer handles it, and both paths must agree.
    #[test]
    fn quoted_and_shell_stripped_forms_agree() {
        let literal = parse_argv(&[r#"repeat:"every 3 days""#], AddFlags::default());
        let stripped = parse_argv(&["repeat:every 3 days"], AddFlags::default());
        assert_eq!(literal.recurrence, stripped.recurrence);
        assert_eq!(literal.recurrence.as_deref(), Some("every 3 days"));
    }

    /// C2: the same argv-boundary promise D13 made for `key:value`, for `+tag`.
    /// The whole element is the tag, and nothing leaks into the title.
    #[test]
    fn a_shell_quoted_tag_survives_as_one_token() {
        let p = parse_argv(&["painting job", "+needs paint"], AddFlags::default());
        assert_eq!(p.tags, vec!["needs paint".to_string()]);
        assert_eq!(
            p.title, "painting job",
            "the tag's words must not become title words"
        );

        // Literal quotes take the tokenizer path and must land in the same place,
        // which is what lets a tag round-trip through C1's filter quoting.
        let literal = parse_argv(&[r#"+"needs paint""#], AddFlags::default());
        assert_eq!(literal.tags, vec!["needs paint".to_string()]);
        assert_eq!(literal.title, "");
    }

    /// A `+` that names nothing is not a tag, and must not become the tag
    /// `" foo"` just because the element happens to contain a space.
    ///
    /// The title now keeps the `+`. It used to read `foo`: the `+` was claimed
    /// by the tag branch, failed the non-empty check, and was deleted — which is
    /// the same defect as `add "Display + Error"`, just with the space on the
    /// other side. Not naming a tag is a reason to leave a token alone, never a
    /// reason to consume it.
    #[test]
    fn a_bare_plus_names_no_tag() {
        let p = parse_argv(&["+ foo"], AddFlags::default());
        assert_eq!(p.tags, Vec::<String>::new());
        assert_eq!(p.title, "+ foo");
    }

    /// The classic one-argument capture form is untouched: there the user never
    /// drew a boundary, so `+needs` is still one tag and `paint` a title word.
    #[test]
    fn the_single_string_capture_form_still_splits_on_spaces() {
        let p = parse1("painting job +needs paint", AddFlags::default());
        assert_eq!(p.tags, vec!["needs".to_string()]);
        assert_eq!(p.title, "painting job paint");

        // …and quoting inside that one string is how you spell a spaced tag there.
        let q = parse1(r#"painting job +"needs paint""#, AddFlags::default());
        assert_eq!(q.tags, vec!["needs paint".to_string()]);
        assert_eq!(q.title, "painting job");
    }

    /// A value key with no spaces is untouched by the whole-element rule.
    #[test]
    fn unspaced_value_elements_are_unaffected() {
        let p = parse_argv(
            &["due:friday", "remind:-30m", "est:4h"],
            AddFlags::default(),
        );
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.remind.as_deref(), Some("-30m"));
        assert_eq!(p.estimate.as_deref(), Some("4h"));
        assert_eq!(p.title, "");
    }

    #[test]
    fn keeps_existing_tag_project_prio_sugar() {
        let p = parse1("Do the thing +x project:work !high", AddFlags::default());
        assert_eq!(p.title, "Do the thing");
        assert_eq!(p.project.as_deref(), Some("work"));
        assert_eq!(p.priority.as_deref(), Some("H"));
        assert_eq!(p.tags, vec!["x".to_string()]);
    }

    #[test]
    fn parses_due_keyword_and_strips_it_from_title() {
        let p = parse1("Do it due:friday +x project:p", AddFlags::default());
        assert_eq!(p.title, "Do it");
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.project.as_deref(), Some("p"));
        assert_eq!(p.tags, vec!["x".to_string()]);
    }

    #[test]
    fn parses_quoted_due_value_with_spaces() {
        let p = parse1(r#"Pay taxes due:"in 3 days""#, AddFlags::default());
        assert_eq!(p.title, "Pay taxes");
        assert_eq!(p.due.as_deref(), Some("in 3 days"));
    }

    #[test]
    fn parses_scheduled_wait_and_repeat() {
        let p = parse1(
            r#"Ship it scheduled:"tomorrow 9am" wait:friday repeat:"every 3 days""#,
            AddFlags::default(),
        );
        assert_eq!(p.title, "Ship it");
        assert_eq!(p.scheduled.as_deref(), Some("tomorrow 9am"));
        assert_eq!(p.wait.as_deref(), Some("friday"));
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));
    }

    #[test]
    fn every_key_prefixes_the_rule() {
        let p = parse1(r#"Water plants every:"3 days""#, AddFlags::default());
        assert_eq!(p.title, "Water plants");
        assert_eq!(p.recurrence.as_deref(), Some("every 3 days"));
    }

    #[test]
    fn explicit_flags_win_over_sugar() {
        let flags = AddFlags {
            due: Some("2026-01-01".into()),
            repeat: Some("every week".into()),
            remind: Some("-2h".into()),
            ..AddFlags::default()
        };
        let p = parse1("Task due:friday repeat:\"every 3 days\" remind:-1h", flags);
        assert_eq!(p.due.as_deref(), Some("2026-01-01"));
        assert_eq!(p.recurrence.as_deref(), Some("every week"));
        assert_eq!(p.remind.as_deref(), Some("-2h"));
    }

    #[test]
    fn parses_remind_offset_and_strips_it_from_title() {
        // The leading `-` must survive tokenizing — it is what marks the value
        // as a due-anchored offset rather than a date (see core `remind`).
        let p = parse1(
            r#"Standup due:"friday 9am" remind:-15m"#,
            AddFlags::default(),
        );
        assert_eq!(p.title, "Standup");
        assert_eq!(p.due.as_deref(), Some("friday 9am"));
        assert_eq!(p.remind.as_deref(), Some("-15m"));
    }

    #[test]
    fn parses_quoted_absolute_remind_value() {
        let p = parse1(r#"Call mum remind:"friday 9am""#, AddFlags::default());
        assert_eq!(p.title, "Call mum");
        assert_eq!(p.remind.as_deref(), Some("friday 9am"));
    }

    /// DESIGN §5's own capture example carries `est:4h`; without this key the
    /// token landed in the title, so the task was literally named "… est:4h".
    #[test]
    fn parses_est_key_and_strips_it_from_title() {
        let p = parse1("Ship the API est:4h +api", AddFlags::default());
        assert_eq!(p.title, "Ship the API");
        assert_eq!(p.estimate.as_deref(), Some("4h"));
        assert_eq!(p.tags, vec!["api".to_string()]);

        let long = parse1(r#"Ship it estimate:"1h 30m""#, AddFlags::default());
        assert_eq!(long.title, "Ship it");
        assert_eq!(long.estimate.as_deref(), Some("1h 30m"));
    }

    #[test]
    fn explicit_estimate_flag_wins_over_sugar() {
        let flags = AddFlags {
            estimate: Some("2h".into()),
            ..AddFlags::default()
        };
        let p = parse1("Task est:4h", flags);
        assert_eq!(p.estimate.as_deref(), Some("2h"));
    }

    #[test]
    fn no_remind_key_means_no_reminder() {
        // Quiet by default (§9): nothing infers a reminder from a due date.
        let p = parse1("Do it due:friday", AddFlags::default());
        assert_eq!(p.remind, None);
    }

    /// C6 again, for `+`: a token that names no tag must reach the title, not be
    /// deleted. `add "Implement Display + std::error::Error"` stored the title
    /// without its `+` and created no tag — a success, a correct-looking task,
    /// and a quietly corrupted title.
    #[test]
    fn a_bare_plus_is_title_text_not_a_dropped_tag() {
        let p = parse1("Display + Error", AddFlags::default());
        assert_eq!(p.title, "Display + Error", "the + must survive verbatim");
        assert_eq!(p.tags, Vec::<String>::new());

        // …and in the shell-tokenized form, where the `+` is its own argv word.
        let q = parse_argv(&["Display", "+", "Error"], AddFlags::default());
        assert_eq!(q.title, "Display + Error");
        assert_eq!(q.tags, Vec::<String>::new());
    }

    /// A Rust path is not sugar: the separator is `::`, and every value key is
    /// also a plausible first module segment. `recur::advance_once` was read as
    /// the recurrence rule `":advance_once"` and the whole command refused.
    #[test]
    fn a_rust_path_is_not_a_sugar_value() {
        let p = parse1("fix recur::advance_once", AddFlags::default());
        assert_eq!(p.title, "fix recur::advance_once");
        assert_eq!(p.recurrence, None);

        // Worse than a hard error: this one was ACCEPTED, set project=":config"
        // and removed the word from the title.
        let q = parse1("see project::config", AddFlags::default());
        assert_eq!(q.title, "see project::config");
        assert_eq!(q.project, None);

        // The shorter alias must not pick up what the longer one just declined:
        // `strip_prefix("proj:")` on `project::config` yields `ect::config`.
        assert_ne!(q.project.as_deref(), Some("ect::config"));

        for tok in [
            "scheduled::at", "estimate::of", "remind::me", "repeat::forever",
            "every::other", "sched::at", "proj::x", "wait::for", "due::soon",
            "est::of",
        ] {
            let r = parse_argv(&["Task", tok], AddFlags::default());
            assert_eq!(r.title, format!("Task {tok}"), "{tok} is title text");
        }
    }

    /// One colon is still sugar — the fix must not cost the common spelling.
    #[test]
    fn a_single_colon_is_still_sugar() {
        let p = parse_argv(&["due:friday", "ship", "it"], AddFlags::default());
        assert_eq!(p.due.as_deref(), Some("friday"));
        assert_eq!(p.title, "ship it");
    }

    /// A key with nothing after it names no value, so it is a word. It used to
    /// be swallowed by its own branch and dropped — the `+` bug with a colon.
    #[test]
    fn a_valueless_key_stays_in_the_title() {
        let p = parse1("meeting due: soon", AddFlags::default());
        assert_eq!(p.title, "meeting due: soon");
        assert_eq!(p.due, None);

        let q = parse_argv(&["notes", "est:"], AddFlags::default());
        assert_eq!(q.title, "notes est:");
        assert_eq!(q.estimate, None);
    }
}
