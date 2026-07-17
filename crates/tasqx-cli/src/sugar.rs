//! Inline capture sugar for `tasqx add` / `modify` (DESIGN.md §5, §10).
//!
//! Extracts structured fields from the free-text title, leaving the remaining
//! words as the title:
//!  * `+tag`
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
//! Explicit flags win over inline sugar. Date/recurrence/reminder/estimate
//! *values* are carried out verbatim (unparsed); the caller resolves them through
//! the one core parser so sugar and flags share identical parsing.
//!
//! `add` and `modify` share this parser exactly (DESIGN.md §12-D13): the same
//! token means the same thing in both, and only the *absence* of a token differs
//! — for `add` it means "no value", for `modify` it means "leave this field
//! alone". Clearing is therefore not expressible here; it is `--clear <field>`.

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
/// So: an element that begins with a value key and carries spaces (and no
/// embedded quotes of its own) is honored whole. Everything else is tokenized as
/// before, which keeps the classic one-big-quoted-string capture form —
/// `add "Ship it due:friday +api"` — parsing exactly as it always has.
///
/// The trade: `add "due:friday Ship it"` (a value key opening a quoted title)
/// now reads the whole remainder as the date and fails with a clean
/// `could not parse date: "friday Ship it"`. Title-first is the documented and
/// universally exemplified form; a loud error on the rare inversion is a better
/// deal than silent corruption on the common quoted value.
pub fn parse_add(args: &[String], flags: AddFlags) -> ParsedAdd {
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

    for tok in tokenize_argv(args) {
        if let Some(tag) = tok.strip_prefix('+') {
            if !tag.is_empty() && !tags.iter().any(|t| t == tag) {
                tags.push(tag.to_string());
            }
        } else if let Some(p) = tok.strip_prefix("project:").or_else(|| tok.strip_prefix("proj:")) {
            if project.is_none() && !p.is_empty() {
                project = Some(p.to_string());
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
            if priority.is_none() {
                priority = normalize_prio(p);
            }
        } else {
            title_words.push(tok);
        }
    }

    ParsedAdd {
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
    }
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
fn tokenize_argv(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for arg in args {
        let shell_quoted_value = !arg.contains('"')
            && arg.chars().any(char::is_whitespace)
            && VALUE_KEYS.iter().any(|k| arg.starts_with(k));
        if shell_quoted_value {
            out.push(arg.clone());
        } else {
            out.extend(tokenize(arg));
        }
    }
    out
}

/// Whitespace-split, but keep double-quoted spans together and strip the quotes,
/// so `due:"in 3 days"` becomes the single token `due:in 3 days`.
fn tokenize(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_content = false;
    for c in raw.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_content = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_content {
                    tokens.push(std::mem::take(&mut cur));
                    has_content = false;
                }
            }
            c => {
                cur.push(c);
                has_content = true;
            }
        }
    }
    if has_content {
        tokens.push(cur);
    }
    tokens
}

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
        parse_add(&[raw.to_string()], flags)
    }

    /// The shell-tokenized form: several argv words, quotes already consumed by
    /// the shell — what `tasqx modify 4 repeat:"every 3 days"` really delivers.
    fn parse_argv(args: &[&str], flags: AddFlags) -> ParsedAdd {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_add(&owned, flags)
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
