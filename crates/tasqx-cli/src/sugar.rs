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

/// Sugar keys that take a *value*, longest-first so `estimate:` is tested before
/// `est:`. Used to spot an argv element the shell already quoted for us.
const VALUE_KEYS: [&str; 12] = [
    "scheduled:",
    "estimate:",
    "project:",
    "remind:",
    "repeat:",
    "every:",
    "recur:",
    "sched:",
    "proj:",
    "wait:",
    "due:",
    "est:",
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
/// So: an element that begins with a value key — or with `+`, which is a value
/// key spelled without a colon (C2) — and carries spaces (and no embedded quotes
/// of its own) is honored whole. Everything else is tokenized as
/// before, which keeps the classic one-big-quoted-string capture form —
/// `add "Ship it due:friday +api"` — parsing exactly as it always has.
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

    for SugarTok { text: tok, quoted } in tokenize_argv(args)? {
        if let Some(tag) = tok.strip_prefix('+') {
            if !tag.is_empty() && !tags.iter().any(|t| t == tag) {
                tags.push(tag.to_string());
            }
        } else if let Some(p) = tok.strip_prefix("project:").or_else(|| tok.strip_prefix("proj:")) {
            if project.is_none() && !p.is_empty() {
                project = Some(p.to_string());
                project_may_be_truncated = !quoted;
            }
        } else if let Some(v) = tok.strip_prefix("due:") {
            set_if_empty(&mut due, v);
        } else if let Some(v) = tok.strip_prefix("scheduled:").or_else(|| tok.strip_prefix("sched:")) {
            set_if_empty(&mut scheduled, v);
        } else if let Some(v) = tok.strip_prefix("wait:") {
            set_if_empty(&mut wait, v);
        } else if let Some(v) = tok.strip_prefix("repeat:").or_else(|| tok.strip_prefix("recur:")) {
            if recurrence.is_none() && !v.is_empty() {
                recurrence = Some(v.to_string());
            }
        } else if let Some(v) = tok.strip_prefix("every:") {
            if recurrence.is_none() && !v.is_empty() {
                recurrence = Some(format!("every {v}"));
            }
        } else if let Some(v) = tok.strip_prefix("remind:") {
            set_if_empty(&mut remind, v);
        } else if let Some(v) = tok.strip_prefix("est:").or_else(|| tok.strip_prefix("estimate:")) {
            set_if_empty(&mut estimate, v);
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
        let shell_quoted_value = !arg.contains('"')
            && arg.chars().any(char::is_whitespace)
            && (VALUE_KEYS.iter().any(|k| arg.starts_with(k)) || is_spaced_tag(arg));
        if shell_quoted_value {
            // The shell drew this boundary, so the value is quoted in every
            // sense that matters here — nothing about it was guessed.
            out.push(SugarTok { text: arg.clone(), quoted: true });
        } else {
            out.extend(tokenize(arg)?);
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

/// `+tag` is a value key without the colon, so it obeys the same rule.
///
/// `add "painting job" +"needs paint"` reaches us as the element `+needs paint`;
/// re-splitting it stored the tag `needs` and, because the leftover word fell
/// through to the title branch, silently renamed the task to `painting job
/// paint`. On `modify` the same split rewrote the title to `job` outright.
///
/// The `+` must be followed by actual content, not by the space itself: a bare
/// `+ foo` names no tag, and honouring it whole would mint the tag `" foo"`.
fn is_spaced_tag(arg: &str) -> bool {
    arg.strip_prefix('+').is_some_and(|t| !t.starts_with(char::is_whitespace) && !t.is_empty())
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
    Ok(words.into_iter().map(|w| SugarTok { text: w.text, quoted: w.quoted }).collect())
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
        assert!(e.message.contains("urgent"), "names the value: {}", e.message);
        assert!(e.message.contains("!high"), "lists the way out: {}", e.message);

        // A lone `!` named no priority either, and vanished just as quietly.
        assert!(parse_err(&["!"], AddFlags::default()).message.contains("invalid priority"));
    }

    /// An explicit flag outranks sugar on *value*, never on *validity* — the
    /// alternative excuses a typo whenever a flag happens to be present too.
    #[test]
    fn a_winning_flag_does_not_excuse_an_invalid_sugar_token() {
        let flags = AddFlags { priority: Some("H".into()), ..AddFlags::default() };
        assert!(parse_err(&["Task", "!urgent"], flags).message.contains("invalid priority"));
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
        assert_eq!(p.title, "painting job", "the tag's words must not become title words");

        // Literal quotes take the tokenizer path and must land in the same place,
        // which is what lets a tag round-trip through C1's filter quoting.
        let literal = parse_argv(&[r#"+"needs paint""#], AddFlags::default());
        assert_eq!(literal.tags, vec!["needs paint".to_string()]);
        assert_eq!(literal.title, "");
    }

    /// A `+` that names nothing is not a tag, and must not become the tag
    /// `" foo"` just because the element happens to contain a space.
    #[test]
    fn a_bare_plus_names_no_tag() {
        let p = parse_argv(&["+ foo"], AddFlags::default());
        assert_eq!(p.tags, Vec::<String>::new());
        assert_eq!(p.title, "foo");
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
        let p = parse_argv(&["due:friday", "remind:-30m", "est:4h"], AddFlags::default());
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
        let p = parse1(r#"Standup due:"friday 9am" remind:-15m"#, AddFlags::default());
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
        let flags = AddFlags { estimate: Some("2h".into()), ..AddFlags::default() };
        let p = parse1("Task est:4h", flags);
        assert_eq!(p.estimate.as_deref(), Some("2h"));
    }

    #[test]
    fn no_remind_key_means_no_reminder() {
        // Quiet by default (§9): nothing infers a reminder from a due date.
        let p = parse1("Do it due:friday", AddFlags::default());
        assert_eq!(p.remind, None);
    }
}
