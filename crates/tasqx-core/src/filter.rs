//! Filter DSL (DESIGN.md §5, §12-D8).
//!
//! Recursive-descent; case-insensitive keywords `and`/`or`. The grammar itself
//! is [`GRAMMAR`], immediately below — a `const`, not a comment, because the
//! `tasqx docs` guide had its own transcription of it and the two drifted: both
//! still said a tag took a bare word long after the parser took a quoted value,
//! and both used a `WORD` symbol neither defined. One copy cannot disagree with
//! itself, so the page now renders this string verbatim.
//!
//! Boolean `or` and parentheses grouping let you write
//! `(+api or +infra) and due.before:2026-07-17T00:00:00Z`. Implicit AND by
//! space is preserved. Per §12-D8 the grammar deliberately stops here: no
//! arithmetic, computed expressions, or subqueries.
//!
//! **Grouping is bounded**: more than `MAX_NESTING` open `(` is a parse
//! error, on the same terms as an unclosed `(` or a stray `)`. Unbounded here
//! did not mean "generous", it meant a filter string could abort the process —
//! see that constant.
//!
//! **A value may be double-quoted**, which is the only way to name a project or
//! tag containing a space: `project:"Home Renovation"`, `+"needs paint"`. The
//! rule is the shell's, so it is one rule and not a table: inside quotes,
//! whitespace and `(`/`)` are ordinary characters and `and`/`or` are ordinary
//! words; `\"` is a literal quote and `\\` a literal backslash. Quotes may cover
//! part of a token, which keeps the predicate prefix outside them where a reader
//! expects it. Quoting affects word *splitting* only — `"project:x"` is still
//! the project predicate, exactly as `"echo"` is still echo in a shell.
//!
//! Without this a value with a space was not expressible at all, and — worse —
//! `project:Home Renovation` had a *meaning*: it silently became `project:Home`
//! AND a stray token, which before D27 matched everything and after it errored.
//! The same hole made a project named `a (b)` break the grouping.
//!
//! Callers composing a filter from a value they did not write must go through
//! [`quote`] rather than interpolating; see its docs.
//!
//! Evaluation is done in Rust against each candidate row (not compiled to SQL)
//! so that: (a) `due.before/after` compare as **instants** — RFC3339 strings are
//! parsed to timestamps and compared, never lexicographically (offsets differ);
//! and (b) the boolean/grouping tree is trivial to evaluate.
//!
//! **An unrecognised token is an error, not an always-true term.** It used to
//! be the latter, "to keep the surface forgiving" — but a filter's whole job is
//! to narrow, so a token that matches everything makes a typo *widen* the
//! result set and present the wrong answer as the right one. `tasqx list onzin`
//! returned every task; `tasqx report onzin` silently grouped by project. The
//! JSON API already rejected the equivalent `group_by`, so the two surfaces
//! disagreed about the same input. This follows §12-D23's precedent, where an
//! unknown `--project` became an error for the same reason: on a *read* path
//! nothing is lost by refusing, while a silent wrong answer is unfalsifiable.
//!
//! **The split about values, refined.** The original rule was "an unknown value
//! just fails to match, because values are data and the set of valid ones is a
//! runtime question". Half of that is true, and the half that is not was doing
//! real damage. The rule now turns on whether the vocabulary is closed:
//!
//! * A value from a **closed, compile-time** set is refused, naming it and the
//!   accepted set. `status:` is such a value — [`Status::ALL`] is five variants
//!   fixed at compile time, no more of a runtime question than the token grammar
//!   itself — and so is a date bound, which has its own closed grammar.
//! * A value from an **open, runtime** set (a project name, a tag) still simply
//!   does not match, because there the set genuinely is a runtime question, and
//!   the write path already refuses an unknown project (D23) so a filter naming
//!   one is not hiding an answer the store had.
//!
//! `status:pendign` printing `No tasks.` at exit 0 was the last closed
//! vocabulary in the tool answering a typo with silence: `parse_sort` refuses an
//! unknown sort key, `Status::parse` refuses on `task.modify` and `store.import`,
//! `Priority::parse` beside it — so the *same string* was a `bad_request` when
//! written and a confident empty table when read. Worse than either alone is the
//! pair: "no tasks are pending" and "you misspelled pending" are different facts
//! and the tool printed one sentence for both.
//!
//! **A date bound belongs on the closed side for the same reason.**
//! `due.before:`/`due.after:` take whatever [`crate::datetime::parse_when`]
//! takes — the same parser `due:` writes through — so `due.before:tomorrow`,
//! `friday`, `2026-07-25`, `in 3 days` and `eom` all work, and an unreadable one
//! is refused by name. It used to be strict RFC3339 and nothing else, which made
//! "what is due soon" — the primary query of a task manager — answer `No tasks`
//! at exit 0 for five of the six spellings the tool prints in its OWN error
//! message when a date fails to parse. `due.before:tomorow` is not a date at
//! all, and answering it with the same silence `due IS NULL` earns is the D27
//! collapse one layer down — as `status:pendign` was, one predicate over.
//!
//! `completed.before:`/`completed.after:` are that same pair on the completion
//! instant, taking the same parser and refusing on the same terms. They exist
//! because the field did: every closed task stores when it closed, `task.get`
//! returns it, DESIGN.md presents `completed.after:-7d` as the query behind the
//! weekly report — and the parser answered it `unknown filter token`. "What did
//! I finish this week" is the only question the field is for, and it was the
//! one question the filter could not be asked.
//!
//! **The bound is resolved once, at parse time, into a [`Timestamp`].** Two
//! reasons, and only the first is about speed: the filter is evaluated per row,
//! so a bound re-read at match time could let `tomorrow` shift across midnight
//! mid-query and answer two identical rows differently. And once `Pred` holds an
//! instant rather than a string, "the caller's bound was unreadable" is not a
//! state `matches` can be in — which is why `parse` takes a `now`. It is a
//! parameter, never `Timestamp::now()`, for the reason [`crate::datetime`]
//! states: no hidden clock in logic that has to be testable.

use jiff::Timestamp;

use crate::datetime;
use crate::types::Status;
use crate::util::parse_ts;

/// The filter grammar, in one place, rendered verbatim by `tasqx docs`.
///
/// `VALUE` is a *sequence* of chunks, not one alternative of the two, because
/// quoting is lexical — resolved in `tokenize` before any production below
/// sees the text — so a quoted run may cover a whole token or any part of one:
/// `+"needs paint"`, `project:"Home Renovation"`, `"+api"` and
/// `project:Home" "Renovation` all work and all mean what they look like. See
/// the module comment for what quoting does and does not change.
///
/// Every value-taking predicate takes a `VALUE`, tags included. There is no
/// form restricted to bare words, which is what the old `WORD` implied.
///
/// A VALUE holding a space MUST be quoted, and the quotes have to REACH this
/// parser — so on a command line they need protecting from the shell as well:
///
/// ```text
/// tasqx list 'project:"Home Renovation"'
/// tasqx list '+"needs paint"'
/// ```
///
/// Nothing puts back quoting the shell removed. `from_argv` explains why at
/// length; the short version is that `project:Home Renovation` is also a valid
/// reading of a whole expression passed as one argument, so guessing meant
/// answering one of the two silently and wrongly. The stray word is refused,
/// and the refusal names the spelling above.
///
/// The example lines live here and not inside the const because
/// `value_prefixes_match_the_grammar` scans it for `key:"` shapes and would
/// count an example as a fifth predicate.
pub const GRAMMAR: &str = "\
filter     := or_expr
or_expr    := and_expr ( \"or\" and_expr )*
and_expr   := term ( \"and\"? term )*        # juxtaposition = implicit AND
term       := \"(\" or_expr \")\" | predicate
predicate  := \"+\" VALUE                    # require tag; VALUE not empty
            | \"-\" VALUE                    # exclude tag; VALUE not empty, not starting with a dash
            | \"@working\"                   # status in {pending,active} AND not blocked
            | \"@blocked\" | \"+blocked\" | \"status:blocked\"   # the blocked flag
            | \"project:\" VALUE
            | \"status:\" VALUE
            | \"due.before:\" DATE
            | \"due.after:\"  DATE
            | \"completed.before:\" DATE       # when the task was finished
            | \"completed.after:\"  DATE

VALUE      := CHUNK*                       # chunks abut; nothing may come between them
CHUNK      := WORD | QUOTED
WORD       := a run of characters, none of them whitespace, a quote or a paren
QUOTED     := a run between double quotes; backslash escapes a quote or a backslash
DATE       := any date `due:` accepts      # tomorrow, friday, 2026-07-25,
                                           # \"in 3 days\", eom, 2026-07-20T17:00";

/// A single leaf predicate.
#[derive(Debug, Clone, PartialEq)]
pub enum Pred {
    /// `status:VALUE`, with VALUE already resolved to the enum. A [`Status`] and
    /// not a `String` on purpose, for the reason `DueBefore` holds a `Timestamp`:
    /// while it was a string, `eval_pred` reparsed it per row and answered a typo
    /// with the same "no" that a genuinely non-matching row earns. Holding the
    /// variant makes "the caller named a status that does not exist" a state
    /// `matches` cannot be in — the refusal is structural, not a validation call
    /// somebody has to remember.
    Status(Status),
    /// `project:VALUE`, and deliberately still a `String`. A project name is an
    /// **open, runtime** vocabulary, so an unknown one legitimately matches no
    /// row rather than being refused — see the module comment's split.
    Project(String),
    /// `+VALUE` — the row must carry this tag. A `String` for the same reason
    /// `Project` is: tags are an open runtime vocabulary.
    TagInclude(String),
    /// `-VALUE` — the row must NOT carry this tag.
    TagExclude(String),
    /// `due.before:VALUE`, with VALUE already resolved against the query's
    /// `now`. A `Timestamp` and not a `String` on purpose: while it was a
    /// string, `instant_cmp` had to reparse it per row and spelled "unreadable
    /// bound" with the same `false` as "this task has no due date". Holding the
    /// instant makes the first of those unrepresentable — the only way to build
    /// this variant is through a parse that already succeeded.
    DueBefore(Timestamp),
    /// `due.after:VALUE`, the other side of the same bound and holding a
    /// `Timestamp` for the same reason as [`Pred::DueBefore`].
    DueAfter(Timestamp),
    /// `completed.before:VALUE` / `completed.after:VALUE`, resolved exactly as
    /// the `due` pair is and holding a `Timestamp` for the same reason (D33).
    ///
    /// DESIGN.md advertised `completed.after:-7d` as the query behind the
    /// weekly report while the parser answered `unknown filter token` — the
    /// completion instant was stored on every closed task and returned by the
    /// API, and there was no way to ask about it. Answering "what did I finish
    /// this week" is the field's only purpose.
    CompletedBefore(Timestamp),
    /// `completed.after:VALUE` — the bound the weekly report is built on.
    CompletedAfter(Timestamp),
    /// `@working`: pending|active AND not blocked.
    Working,
    /// The blocked flag: a task with >=1 dependency not yet `done` (DESIGN §3).
    Blocked,
    /// Always matches. Reachable from exactly one place: the empty filter,
    /// meaning the caller asked for no filtering. It is deliberately NOT what
    /// an unrecognised token maps to any more — that is an error.
    Always,
}

/// The parsed filter expression tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Every child must match. Juxtaposition builds this as well as the literal
    /// `and` keyword; it binds TIGHTER than `Or`, which is the precedence a
    /// mutation sweep found inverted here once already.
    And(Vec<Expr>),
    /// At least one child must match.
    Or(Vec<Expr>),
    /// A leaf.
    Pred(Pred),
}

/// The fields a predicate is evaluated against.
pub struct MatchCtx<'a> {
    /// The row's status as the typed enum, never a bare string. While this was
    /// `&str` the whole module compared status with `==` against hand-typed
    /// literals, so `Status` could not participate in its own matching rules and
    /// a renamed or added variant went unnoticed here.
    pub status: Status,
    /// The row's project name, `None` when it belongs to none.
    pub project: Option<&'a str>,
    /// Every tag on the row. Order is irrelevant; membership is the only
    /// question asked of it.
    pub tags: &'a [String],
    /// The row's `due` as stored (RFC3339). Still a string here because the
    /// BOUND is what had to become a `Timestamp` — the row side is parsed once
    /// per comparison and a task with no due date simply satisfies no bound.
    pub due: Option<&'a str>,
    /// The completion instant, `None` on anything not closed. `None` is a real
    /// answer here, not a missing one: a task that was never completed cannot
    /// satisfy any bound on when it was, which is the rule `due` already has
    /// for a task with no due date.
    pub completed: Option<&'a str>,
    /// Whether the row has at least one dependency that is not yet `done`.
    /// Precomputed by the caller: it needs a join, and re-deriving it per
    /// predicate would run that join once for `@working` and again for
    /// `@blocked` in the same expression.
    pub blocked: bool,
}

/// A parsed filter. `Filter::parse` rejects what it cannot parse; `matches`
/// evaluates what it accepted.
#[derive(Debug, Clone)]
pub struct Filter {
    root: Expr,
}

impl Filter {
    /// Parse a filter string, or say why it is not a filter.
    ///
    /// An empty string matches everything — that is the caller passing no
    /// filter at all, not a malformed one. Every other input must parse: a
    /// token this grammar does not recognise is an error rather than a term
    /// that quietly matches every row (see the module comment). So is a
    /// `due.before:`/`due.after:` bound the date grammar cannot read.
    ///
    /// `now` is the instant every relative bound in `input` resolves against.
    /// It is a parameter for the two reasons the module comment gives: the
    /// bound must resolve once per query rather than once per row, and this
    /// codebase keeps no hidden clock in logic that has to be testable.
    pub fn parse(input: &str, now: Timestamp) -> Result<Filter, String> {
        let toks = tokenize(input)?;
        if toks.is_empty() {
            return Ok(Filter {
                root: Expr::Pred(Pred::Always),
            });
        }
        let mut p = Parser {
            toks,
            pos: 0,
            now,
            depth: 0,
        };
        let root = p.parse_or()?;
        // `parse_or` stops at the first token it cannot continue on. Anything
        // left is unbalanced — a stray `)` — and dropping it would silently
        // evaluate a filter the user did not write.
        if let Some(t) = p.peek() {
            return Err(format!("unexpected {t:?} in filter"));
        }
        Ok(Filter { root })
    }

    /// True when `ctx` satisfies the filter.
    pub fn matches(&self, ctx: &MatchCtx) -> bool {
        eval(&self.root, ctx)
    }

    /// True when this filter already constrains status, so `report.summary`'s
    /// exclude-cancelled default must step aside rather than silently narrowing
    /// what the caller asked for (DESIGN §12-D24, rule 2). Without it,
    /// `tasqx report status:cancelled` would return an empty table — which reads
    /// as a bug no matter how well documented the default is.
    ///
    /// The walk is structural, over the parsed tree, including `Or` branches: a
    /// lexical `input.contains("status")` would both over-match (`+status-page`)
    /// and under-match (`@working`, which carries no such substring).
    pub fn constrains_status(&self) -> bool {
        constrains_status(&self.root)
    }
}

fn constrains_status(e: &Expr) -> bool {
    match e {
        Expr::And(v) | Expr::Or(v) => v.iter().any(constrains_status),
        // `@working` counts: it expands to `status in {pending,active}`, so the
        // caller has named a status set just as explicitly as `status:pending`.
        Expr::Pred(Pred::Status(_) | Pred::Working) => true,
        Expr::Pred(_) => false,
    }
}

fn eval(e: &Expr, ctx: &MatchCtx) -> bool {
    match e {
        Expr::And(v) => v.iter().all(|x| eval(x, ctx)),
        Expr::Or(v) => v.iter().any(|x| eval(x, ctx)),
        Expr::Pred(p) => eval_pred(p, ctx),
    }
}

fn eval_pred(p: &Pred, ctx: &MatchCtx) -> bool {
    match p {
        Pred::Always => true,
        // A plain enum comparison: an unreadable value never reaches here,
        // because `predicate()` refused it at parse time.
        Pred::Status(s) => *s == ctx.status,
        Pred::Project(pr) => ctx.project == Some(pr.as_str()),
        Pred::TagInclude(t) => ctx.tags.iter().any(|x| x == t),
        Pred::TagExclude(t) => !ctx.tags.iter().any(|x| x == t),
        Pred::Working => matches!(ctx.status, Status::Pending | Status::Active) && !ctx.blocked,
        Pred::Blocked => ctx.blocked,
        Pred::DueBefore(bound) => instant_cmp(ctx.due, *bound, true),
        Pred::DueAfter(bound) => instant_cmp(ctx.due, *bound, false),
        // The same comparator on a different column — deliberately not a second
        // one, so the two date fields cannot answer a boundary differently.
        Pred::CompletedBefore(bound) => instant_cmp(ctx.completed, *bound, true),
        Pred::CompletedAfter(bound) => instant_cmp(ctx.completed, *bound, false),
    }
}

/// Compare one of a task's date fields against an already-resolved bound as
/// instants. `before=true` => field < bound; else field > bound.
///
/// Shared by the `due.` and `completed.` pairs rather than duplicated per
/// field, so a boundary case cannot be answered two ways.
///
/// Exactly ONE `false`-without-comparing remains, and it means one thing: the
/// task has no readable value in that field, so no date bound can select it —
/// an uncompleted task is outside every `completed.` bound, which is the same
/// rule an undated task has always had for `due.`. The bound side
/// used to share that answer — a caller's typo and a task with no due date were
/// the same `return false` — which is the collapse D27 rules out for a filter
/// token, here applied to a bound value. It is gone by construction rather than
/// by care: `bound` is a `Timestamp`, so there is nothing left to fail.
fn instant_cmp(field: Option<&str>, bound: Timestamp, before: bool) -> bool {
    let Some(d) = field.and_then(parse_ts) else {
        return false;
    };
    if before {
        d < bound
    } else {
        d > bound
    }
}

/// Render `value` as a filter literal that parses back to exactly `value`.
///
/// This is the ONE escaping helper. Every caller that composes a filter from a
/// value it did not itself write — a project name, a tag — goes through it
/// rather than interpolating, because interpolation is correct right up until
/// the day someone names a project `Home Renovation` and the composed filter
/// starts answering a different question without saying so.
///
/// It quotes unconditionally, including values that would not have needed it.
/// A "does this need quoting?" branch is one more thing to get wrong for no
/// gain: the composed filter is machine-read, never shown.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        // Only these two are special inside quotes, so only these two are escaped.
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Value-taking predicate prefixes, i.e. the ones a quoted VALUE can follow.
///
/// Kept beside the grammar it mirrors and pinned to it by
/// `value_prefixes_match_the_grammar`, because a fifth `key:` predicate added
/// to `GRAMMAR` alone would silently lose shell quoting on that key only.
/// `+`/`-` are here for the same reason they are in the grammar: a tag is a
/// VALUE like any other, it just spells its key as punctuation.
const VALUE_PREFIXES: [&str; 8] = [
    "project:",
    "status:",
    "due.before:",
    "due.after:",
    "completed.before:",
    "completed.after:",
    "+",
    "-",
];

/// The token shapes an error message offers when it refuses one.
///
/// One string, two call sites. It was two hand-typed copies of the same
/// sentence, which is the parallel-list shape D30 rules against: adding
/// `completed.before:`/`completed.after:` meant editing both, and a filter that
/// accepts a token its own error message does not mention teaches the user that
/// the token does not exist. `token_shapes_name_every_value_prefix` pins it to
/// `VALUE_PREFIXES` so a seventh `key:` predicate cannot be advertised by the
/// grammar and omitted from the refusal.
const TOKEN_SHAPES: &str = "+tag, -tag, @working, @blocked, project:, status:, \
                            due.before:, due.after:, completed.before: or completed.after:";

/// Compose one filter string from argv by joining the elements with a space.
///
/// It deliberately does NOT guess. An earlier version tried to put back the
/// quoting the shell removed: an element carrying whitespace and leading with a
/// value-taking prefix had its value re-quoted, so `list +"needs paint"` (which
/// reaches us as the single element `+needs paint`) meant the spaced tag.
///
/// That heuristic was reverted because the two readings it chose between are
/// GENUINELY ambiguous, and it therefore had to be wrong for somebody.
/// `project:Work and (+bug or +review)` arriving as one element is a valid
/// reading of both "one project name containing spaces" and "a whole filter
/// expression the user quoted as one argument" — and the heuristic answered
/// `list "+api or +web"` with a confident `No tasks.`, because it read the
/// expression as one tag literally named `api or +web`. It caused two bugs that
/// way. A guess that returns a silent wrong answer is worse than a refusal:
/// that is D27's own rule, and this heuristic was the thing D27 exists to
/// forbid, one layer up at the argv boundary.
///
/// So the ambiguity is handed to the user, who is the only one who knows which
/// they meant, and the grammar already gives them a way to say it: LITERAL
/// quotes, which survive the shell and reach `tokenize` intact.
///
/// ```text
/// tasqx list 'project:"Home Renovation"'   # the spaced value
/// tasqx list '+"needs paint"'              # the spaced tag
/// tasqx list "+api or +web"                # the expression
/// ```
///
/// The consequence, which is intended: `list project:Home Renovation` with the
/// shell eating the quotes is `project:Home` plus a stray token `Renovation`,
/// and is REFUSED. It fails loudly instead of answering wrongly, and
/// `spacing_hint` makes that refusal name the literal-quote spelling.
pub fn from_argv(args: &[String]) -> String {
    args.join(" ")
}

// ---- tokenizer --------------------------------------------------------------

/// One token, plus whether any part of it arrived quoted.
///
/// The flag is not decoration: a quoted token must never be read as the keyword
/// `and`/`or` or as a `(`/`)`, otherwise a project named `and` would still be
/// unfilterable after all this. Quoting suppresses metacharacter meaning, which
/// is the whole point of quoting.
struct Tok {
    text: String,
    quoted: bool,
}

/// Split into tokens, breaking out parentheses as their own tokens even when
/// glued to a word (`(+api` => `(`, `+api`), and honouring double quotes.
///
/// Inside `"..."`, whitespace and parentheses are ordinary characters, `\"` is a
/// literal quote and `\\` a literal backslash. Quotes may cover part of a token
/// (`project:"Home Renovation"`), which is what keeps the predicate prefixes
/// (`project:`, `+`, `-`) outside the quoted run where a reader expects them.
///
/// Fallible now: an unterminated quote is refused rather than closed at end of
/// input, for the same reason an unclosed `(` is — silently guessing evaluates
/// a filter the user did not write.
fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    scan(input, Parens::Break, "filter")
}

/// Does a bare `(`/`)` end the current token?
///
/// Only the read side has grouping. On the write side a paren is ordinary text
/// in a title (`add "call (mom)"`), so breaking there would be a new bug in
/// service of sharing code. This is the ONLY axis on which the two sides differ,
/// and it is lexical bookkeeping, not quoting — the quoting rule below is shared
/// character for character, which is the whole point of [`split_words`].
#[derive(PartialEq)]
enum Parens {
    Break,
    Ordinary,
}

/// A word produced by [`split_words`], plus whether any part of it arrived quoted.
///
/// The flag travels with the word because callers need to distinguish a value
/// the user *delimited* from one that merely happens to be one word: the read
/// side uses it to stop a quoted `and` being read as the keyword, the write side
/// to know whether a project name it failed to resolve could have been cut at a
/// space it never saw.
pub struct Word {
    /// The word with its quoting removed and its escapes resolved — what the
    /// user meant, not what they typed.
    pub text: String,
    /// True when ANY part of the word arrived inside `"…"`. A whole-word flag
    /// because that is the granularity every caller asks at; a partially quoted
    /// value like `project:"Home Renovation"` is still "the user delimited this".
    pub quoted: bool,
}

/// Split `input` into words under the ONE quoting rule of [`GRAMMAR`]'s
/// `QUOTED`, with `(`/`)` treated as ordinary characters.
///
/// This exists so the write side (`cli/sugar.rs`) can obey the rule the read
/// side documents instead of carrying a second, subtly different tokenizer.
/// It did carry one, and the two disagreed about the same syntax: a `"` was a
/// pure delimiter there with no escape at all, so `add '+say"hi'` stored the tag
/// `sayhi` — a value the user never typed — and `project:"My \"Big\" Project"`
/// stored the mangled `My \Big\ Project`. A value containing a quote was
/// therefore unrepresentable on the write side, which made the escape this
/// grammar documents unmatchable for tags: `filter::quote` could emit it and no
/// write path could produce a value needing it.
///
/// `context` names the surface in the error text, because "unterminated quote in
/// filter" is a lie when the line being refused was a `tasqx add`.
pub fn split_words(input: &str, context: &str) -> Result<Vec<Word>, String> {
    scan(input, Parens::Ordinary, context).map(|ts| {
        ts.into_iter()
            .map(|t| Word {
                text: t.text,
                quoted: t.quoted,
            })
            .collect()
    })
}

fn scan(input: &str, parens: Parens, context: &str) -> Result<Vec<Tok>, String> {
    let unterminated = || {
        format!(
            "unterminated '\"' in {context} (a quoted value must be closed; \
             write \\\" for a literal quote)"
        )
    };
    let mut toks = Vec::new();
    let mut cur = String::new();
    // Tracked apart from `cur.is_empty()` so that `""` is an empty token — which
    // `predicate` then rejects by name — rather than no token at all.
    let mut started = false;
    let mut quoted = false;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                started = true;
                quoted = true;
                loop {
                    match chars.next() {
                        None => return Err(unterminated()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '\\')) => cur.push(e),
                            None => return Err(unterminated()),
                            Some(other) => {
                                return Err(format!(
                                    "unknown escape \"\\{other}\" in {context} (inside quotes only \
                                     \\\" and \\\\ are escapes)"
                                ));
                            }
                        },
                        Some(x) => cur.push(x),
                    }
                }
            }
            '(' | ')' if parens == Parens::Break => {
                if started {
                    toks.push(Tok {
                        text: std::mem::take(&mut cur),
                        quoted,
                    });
                    started = false;
                    quoted = false;
                }
                toks.push(Tok {
                    text: c.to_string(),
                    quoted: false,
                });
            }
            c if c.is_whitespace() => {
                if started {
                    toks.push(Tok {
                        text: std::mem::take(&mut cur),
                        quoted,
                    });
                    started = false;
                    quoted = false;
                }
            }
            _ => {
                started = true;
                cur.push(c);
            }
        }
    }
    if started {
        toks.push(Tok { text: cur, quoted });
    }
    Ok(toks)
}

// ---- parser -----------------------------------------------------------------

/// How many `(` groups may be open at once before the filter is refused.
///
/// This is not a taste limit, it is the only thing standing between a filter
/// string and `fatal runtime error: stack overflow`. The parser is recursive
/// descent, so each open group costs three stack frames — `parse_or` ->
/// `parse_and` -> `parse_term` -> `parse_or` — and a few thousand `(` in debug
/// (about fifty thousand in release, i.e. ~50 KB of input) walks off the end of
/// the thread stack. Rust turns that into SIGABRT, not a panic, so the daemon's
/// `catch_unwind` around dispatch cannot see it: one `task.list` kills the
/// process for every connected client, drops every `watch` stream and leaves
/// the unix socket behind, with no error ever reaching the caller. The daemon's
/// 1 MiB frame cap does not help, because the abort arrives an order of
/// magnitude below it.
///
/// The counter lives here rather than in the daemon's request validation
/// because the same input reaches the same parser through `--no-daemon` and
/// through `argv.rs`, which re-parses an offending token purely to build an
/// error message.
///
/// 64 is chosen to be unreachable by hand and by composition — `html.rs` wraps
/// a caller's filter in one further group — while staying two orders of
/// magnitude under the depth that hurts. Per D8 the grammar is not going to
/// grow expressions that nest deeper.
const MAX_NESTING: u32 = 64;

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    /// The reference instant every relative date bound in this filter resolves
    /// against. Carried on the parser, not read per predicate, so one query
    /// cannot resolve `tomorrow` twice and get two answers.
    now: Timestamp,
    /// How many `(` groups are open at this point in the parse — a depth, not a
    /// total, so a flat run of sibling groups never approaches the cap. See
    /// [`MAX_NESTING`] for what it prevents.
    depth: u32,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.pos).map(|t| t.text.as_str())
    }

    fn is_kw(&self, kw: &str) -> bool {
        self.toks
            .get(self.pos)
            .is_some_and(|t| !t.quoted && t.text.eq_ignore_ascii_case(kw))
    }

    /// An *unquoted* token equal to `s` — i.e. `s` used as punctuation. A quoted
    /// `")"` is a value that happens to look like punctuation and must stay one.
    fn is_sym(&self, s: &str) -> bool {
        self.toks
            .get(self.pos)
            .is_some_and(|t| !t.quoted && t.text == s)
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut parts = vec![self.parse_and()?];
        while self.is_kw("or") {
            self.pos += 1; // consume 'or'
            parts.push(self.parse_and()?);
        }
        if parts.len() == 1 {
            Ok(parts.pop().unwrap())
        } else {
            Ok(Expr::Or(parts))
        }
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut parts = Vec::new();
        loop {
            if self.peek().is_none() || self.is_sym(")") || self.is_kw("or") {
                break;
            }
            if self.is_kw("and") {
                self.pos += 1; // explicit AND separator, skip
                continue;
            }
            parts.push(self.parse_term()?);
        }
        if parts.is_empty() {
            // Reached from a dangling operator or an empty group — `+api or`,
            // `()`. This used to yield the always-true term, so a trailing
            // `or` widened the result to every task instead of failing. The
            // genuinely empty filter never arrives here: `Filter::parse`
            // returns early on no tokens at all, which is the one case that
            // legitimately means "match everything".
            return Err("expected a filter term".to_string());
        }
        if parts.len() == 1 {
            Ok(parts.pop().unwrap())
        } else {
            Ok(Expr::And(parts))
        }
    }

    fn parse_term(&mut self) -> Result<Expr, String> {
        if self.is_sym("(") {
            self.pos += 1; // consume '('
            // Checked before the increment, so `depth` is only ever raised on a
            // path that also lowers it again — the two halves cannot drift.
            if self.depth == MAX_NESTING {
                return Err(format!(
                    "filter nests more than {MAX_NESTING} '(' groups deep"
                ));
            }
            self.depth += 1;
            let inner = self.parse_or();
            // Given back before the `?`, not after. An error unwinds this frame
            // exactly as a success does, and a counter returned only on the
            // happy path is one that silently drifts upward the moment anything
            // above here recovers from a failed sub-parse.
            self.depth -= 1;
            let inner = inner?;
            if !self.is_sym(")") {
                // Previously the missing `)` was skipped in silence, which let
                // `(+api or +infra` parse as though the group were closed.
                return Err("unclosed '(' in filter".to_string());
            }
            self.pos += 1; // consume ')'
            return Ok(inner);
        }
        let tok = self.toks[self.pos].text.clone();
        let prev = self.pos.checked_sub(1).and_then(|p| self.toks.get(p));
        let hint = spacing_hint(prev, &self.toks[self.pos]);
        self.pos += 1;
        Ok(Expr::Pred(predicate(&tok, self.now).map_err(
            |e| match hint {
                Some(h) => format!("{e} — {h}"),
                None => e,
            },
        )?))
    }
}

/// The one hint that turns the cost of refusing a shell-stripped value into a fix.
///
/// `from_argv` no longer guesses (see its comment), so `list project:Home
/// Renovation` — the shell having eaten the quotes — arrives as `project:Home`
/// followed by a bare `Renovation` and is refused. That refusal is correct and
/// is the whole point, but "unknown filter token" alone tells a user who typed
/// a perfectly reasonable project name nothing about what to do next. The
/// preceding token is the evidence: a value-taking prefix carrying an UNQUOTED
/// value, immediately followed by a bare word, is overwhelmingly one value the
/// shell split rather than two predicates. So the message names the literal
/// spelling that survives the shell.
///
/// It is a hint and not a repair on purpose. Guessing here is what was just
/// reverted; suggesting is free, because a wrong suggestion costs a glance and
/// a wrong guess costs a wrong answer.
///
/// `prev.quoted` disqualifies the hint: after `project:"Home Renovation"` the
/// value was already spelled correctly and the stray word is genuinely stray,
/// so proposing to swallow it into the value would be advice to break a working
/// filter.
fn spacing_hint(prev: Option<&Tok>, tok: &Tok) -> Option<String> {
    let prev = prev?;
    if prev.quoted || tok.quoted {
        return None;
    }
    // Only a bare word is a plausible fragment of a split value. Anything that
    // opens a predicate of its own is a token the user meant as a token, and
    // its error should stand unadorned.
    if VALUE_PREFIXES.iter().any(|p| tok.text.starts_with(p)) || tok.text.starts_with('@') {
        return None;
    }
    let p = VALUE_PREFIXES
        .iter()
        .find(|p| prev.text.strip_prefix(**p).is_some_and(|v| !v.is_empty()))?;
    let value = prev.text.strip_prefix(*p).expect("just matched");
    Some(format!(
        "did you mean {p}{}? quote a value that contains a space, so the shell hands it over whole",
        quote(&format!("{value} {}", tok.text))
    ))
}

/// Map a single token to a leaf predicate, or say why it is not one.
///
/// The error names the token and the shapes that would have worked. A filter
/// is typed by hand far more often than it is generated, so the message is the
/// whole user experience of a typo.
fn predicate(tok: &str, now: Timestamp) -> Result<Pred, String> {
    if tok == "@working" {
        return Ok(Pred::Working);
    }
    if tok == "@blocked" || tok == "+blocked" || tok == "status:blocked" {
        return Ok(Pred::Blocked);
    }
    if let Some(rest) = tok.strip_prefix('+') {
        if !rest.is_empty() {
            return Ok(Pred::TagInclude(rest.to_string()));
        }
    }
    if let Some(rest) = tok.strip_prefix('-') {
        // A tag name may not itself begin with `-`, so `--anything` is not an
        // exclusion — it is a mistyped flag. This rule is load-bearing, not
        // tidiness. It is what lets the CLI tell a filter token apart from a
        // flag one token at a time (`cli/argv.rs` hides the single dash of
        // `-tag` from clap and leaves every `--x` for clap to judge), and it is
        // the only check at all on the API and MCP paths, where a filter string
        // arrives with no clap in front of it. Parsed as an exclusion,
        // `--jsn` meant "exclude the tag `-jsn`", excluded nothing, and
        // returned EVERY task with exit 0 — a typo silently widening the result
        // set, the exact failure this module refuses unknown tokens to prevent.
        //
        // The message says "flag", not "token", because that is what a user who
        // hits this actually typed. Note it offers no quoted escape hatch: the
        // tokenizer resolves quotes before this point, so `-"-x"` arrives here
        // as `--x` and a tag whose name begins with `-` is genuinely not
        // excludable. Claiming otherwise would be worse than saying nothing.
        if rest.starts_with('-') {
            return Err(format!(
                "unknown flag {tok:?} (a tag exclusion takes one dash, as -tag; \
                 filter tokens are {TOKEN_SHAPES})"
            ));
        }
        if !rest.is_empty() {
            return Ok(Pred::TagExclude(rest.to_string()));
        }
    }
    if let Some(v) = tok.strip_prefix("project:") {
        return Ok(Pred::Project(v.to_string()));
    }
    if let Some(v) = tok.strip_prefix("status:") {
        // Note this is reached only AFTER `status:blocked` was claimed above:
        // `blocked` is a derived flag, not a member of the status set, so it
        // must not be offered here as a status and must not be refused either.
        //
        // The empty value falls in here too and is refused with everything else.
        // `status:` names no status, and the grammar already treats an empty
        // value that way one predicate over: a bare `+` or `-` is an unknown
        // token, not a tag that matches nothing.
        return Status::parse(v).map(Pred::Status).ok_or_else(|| {
            format!(
                "unknown status {v:?} (expected one of: {} — or `status:blocked` \
                 for the derived blocked flag)",
                Status::accepted()
            )
        });
    }
    if let Some(v) = tok.strip_prefix("due.before:") {
        return Ok(Pred::DueBefore(bound(v, "due.before", now)?));
    }
    if let Some(v) = tok.strip_prefix("due.after:") {
        return Ok(Pred::DueAfter(bound(v, "due.after", now)?));
    }
    // The completion pair, tested AFTER `due.` so neither prefix can shadow the
    // other. Same `bound` parser, same D33 refusal on an unreadable value: this
    // is the `due.` shape one field over, not a second date grammar.
    if let Some(v) = tok.strip_prefix("completed.before:") {
        return Ok(Pred::CompletedBefore(bound(v, "completed.before", now)?));
    }
    if let Some(v) = tok.strip_prefix("completed.after:") {
        return Ok(Pred::CompletedAfter(bound(v, "completed.after", now)?));
    }
    Err(format!(
        "unknown filter token {tok:?} (expected {TOKEN_SHAPES})"
    ))
}

/// Resolve a date bound against `now`, or say why it is not a date.
///
/// It delegates to [`datetime::parse_when`] rather than restating what a date
/// may look like, which is the whole point: the bound accepts exactly what
/// `due:` accepts because it is the same parser, so the two cannot drift and
/// the tool cannot advertise a spelling its own filter rejects.
///
/// The refusal is D27's rule applied to a value: a bound nobody can read is a
/// caller error, and a read path loses nothing by refusing it — the user
/// retypes. Matching nothing instead is a wrong answer shaped exactly like a
/// right one. `parse_when`'s message already names the offending value and
/// lists the accepted forms, so it is passed through rather than reworded.
fn bound(value: &str, prefix: &str, now: Timestamp) -> Result<Timestamp, String> {
    let resolved = datetime::parse_when(value, now)
        .map_err(|e| format!("`{prefix}:` needs a date — {}", e.message))?;
    // `parse_when` promises an RFC3339 `…Z` string, so this cannot fail; it is
    // an `Option` only because `parse_ts` is total for untrusted input.
    parse_ts(&resolved)
        .ok_or_else(|| format!("`{prefix}:{value}` resolved to an unreadable instant {resolved:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a filter that the test asserts is valid.
    ///
    /// Every filter below is a hand-written literal, so a parse failure here
    /// means the test itself is wrong — worth panicking on rather than
    /// threading a Result through assertions about matching.
    /// The grammar block, as the docs show it — the same string, by construction.
    fn grammar() -> &'static str {
        GRAMMAR
    }

    /// A fixed reference instant for tests that do not care about dates, so
    /// nothing in this module resolves a bound against the wall clock.
    fn anchor() -> Timestamp {
        "2026-07-19T12:00:00Z".parse().expect("anchor")
    }

    fn parsed(s: &str) -> Filter {
        Filter::parse(s, anchor()).unwrap_or_else(|e| panic!("test filter {s:?} must parse: {e}"))
    }

    /// `report.summary` only applies its exclude-cancelled default when the
    /// caller has *not* said anything about status (D24 rule 2). Getting this
    /// predicate wrong is silent: too eager and `tasqx report status:cancelled`
    /// returns an empty table, too lax and the default never fires. The check
    /// must be structural — a lexical `contains("status")` would call
    /// `+status-page` a status constraint and miss `@working` entirely.
    #[test]
    fn constrains_status_sees_only_real_status_predicates() {
        for (input, want) in [
            ("", false),
            ("project:x", false),
            ("+api -infra", false),
            ("status:pending", true),
            ("@working", true),                 // expands to pending|active
            ("project:x or status:done", true), // must reach into Or branches
            ("(project:x and +api) or @working", true),
        ] {
            assert_eq!(
                parsed(input).constrains_status(),
                want,
                "constrains_status({input:?})"
            );
        }
    }

    fn ctx_for(status: Status) -> MatchCtx<'static> {
        MatchCtx {
            status,
            project: None,
            tags: &[],
            due: None,
            completed: None,
            blocked: false,
        }
    }

    fn ctx_tagged(tags: &[String]) -> MatchCtx<'_> {
        MatchCtx {
            status: Status::Pending,
            project: None,
            tags,
            due: None,
            completed: None,
            blocked: false,
        }
    }

    /// Grouping has to actually GROUP. Nothing in this suite ever *evaluated* a
    /// parenthesised filter — the only paren case was inside a
    /// `constrains_status` assertion, which returns true either way — so the
    /// `Some(")") => break` arm in `parse_and` could be deleted with the whole
    /// workspace staying green. Without it, `)` is swallowed as an ordinary
    /// token (becoming `Pred::Always`) instead of closing the group, and
    /// `(a or b) and c` silently reassociates to `a or (b and c)`.
    ///
    /// That is the worst shape a filter bug can take: no error, no crash, just a
    /// full and entirely credible table containing exactly the rows the user
    /// asked to exclude. Found by cargo-mutants, not by review.
    #[test]
    fn parentheses_group_rather_than_reassociating() {
        // a = project:home -> TRUE, b = +api -> FALSE, c = status:done -> FALSE.
        // Correct: (a or b) and c == (T or F) and F == FALSE.
        // Reassociated: a or (b and c) == T or (F and F) == TRUE.
        let ctx = MatchCtx {
            status: Status::Pending,
            project: Some("home"),
            tags: &[],
            due: None,
            completed: None,
            blocked: false,
        };
        assert!(
            !parsed("(project:home or +api) and status:done").matches(&ctx),
            "the group must bind before `and` — this row was filtered out"
        );
        // The same tokens without parentheses DO match, which is what proves the
        // assertion above is about grouping and not about the predicates.
        assert!(parsed("project:home or +api and status:done").matches(&ctx));
    }

    /// `due.before`/`due.after` are documented as STRICT, and the boundary was
    /// the one input no test supplied: every fixture used a due either clearly
    /// inside or clearly outside the bound, so `<` -> `<=` and `>` -> `>=`
    /// both survived. A task due at exactly the bound appears as one extra row
    /// in an otherwise correct list — the kind of off-by-one a user blames on
    /// themselves rather than reporting.
    #[test]
    fn due_bounds_are_strict_at_the_exact_instant() {
        let bound = "2026-07-17T00:00:00Z";
        let ctx = MatchCtx {
            status: Status::Pending,
            project: None,
            tags: &[],
            due: Some(bound),
            completed: None,
            blocked: false,
        };
        assert!(
            !parsed(&format!("due.before:{bound}")).matches(&ctx),
            "before is strict"
        );
        assert!(
            !parsed(&format!("due.after:{bound}")).matches(&ctx),
            "after is strict"
        );
        // One second either side still resolves the way the names promise.
        let earlier = MatchCtx {
            due: Some("2026-07-16T23:59:59Z"),
            ..ctx
        };
        assert!(parsed(&format!("due.before:{bound}")).matches(&earlier));
        let later = MatchCtx {
            due: Some("2026-07-17T00:00:01Z"),
            ..ctx
        };
        assert!(parsed(&format!("due.after:{bound}")).matches(&later));
    }

    /// J1/D27+D28 — a `due.before:`/`due.after:` bound takes the SAME grammar
    /// `due:` takes, and the five spellings the tool's own error message
    /// advertises must all select the row `due:tomorrow` wrote.
    ///
    /// Before this, the bound was `s.parse::<Timestamp>().ok()` — strict RFC3339
    /// and nothing else — so `tasqx list due.before:tomorrow` printed "No tasks"
    /// with a task due tomorrow sitting in the store, exit 0. The accepted
    /// spelling was the one a human is least likely to type, and every spelling
    /// the tool *recommends* on a date error was rejected in silence.
    #[test]
    fn a_due_bound_accepts_every_spelling_the_tool_advertises() {
        // A Sunday, so `friday` and `eom` both resolve clear of the due below.
        let anchor: Timestamp = "2026-07-19T12:00:00Z".parse().expect("anchor");
        // Tomorrow MORNING, not tomorrow midnight. The bound is strict at the
        // exact instant (see `due_bounds_are_strict_at_the_exact_instant`, an
        // off-by-one this project has already paid for), and a bare date or
        // `tomorrow` resolves to 00:00 — so a task due at exactly 00:00 sits on
        // the boundary and is legitimately outside both sides of it. Putting the
        // fixture on that instant would test the boundary rule, not this one.
        let ctx = MatchCtx {
            status: Status::Pending,
            project: None,
            tags: &[],
            due: Some("2026-07-20T09:00:00Z"),
            completed: None,
            blocked: false,
        };
        // Composed through `quote`, because a date is a VALUE like any other and
        // a multi-word one (`in 3 days`) is expressible only under D30's quoting
        // rule — unquoted it is three tokens, and the second is not a filter
        // term at all. That is the grammar working, not a gap: `due:` gets the
        // same protection from the shell's own quotes.
        for spelling in [
            "friday",
            "2026-07-25",
            "in 3 days",
            "eom",
            "2026-07-20T17:00",
        ] {
            let f = Filter::parse(&format!("due.before:{}", quote(spelling)), anchor)
                .unwrap_or_else(|e| panic!("due.before:{spelling:?} must parse: {e}"));
            assert!(
                f.matches(&ctx),
                "due.before:{spelling:?} must select a task due tomorrow"
            );
        }
        // The same grammar on the other side of the comparison — `tomorrow`
        // included, so no spelling in the advertised set goes unexercised.
        for spelling in ["today", "yesterday", "2026-07-19", "tomorrow"] {
            let f = Filter::parse(&format!("due.after:{spelling}"), anchor)
                .unwrap_or_else(|e| panic!("due.after:{spelling:?} must parse: {e}"));
            assert!(
                f.matches(&ctx),
                "due.after:{spelling:?} must select a task due tomorrow"
            );
        }
    }

    /// The other half, and the reason (a) alone would not be a fix: a bound the
    /// date grammar cannot read is a caller error, and `instant_cmp` used to
    /// spell it with the same `false` it uses for "this task has no due date".
    /// One of those is a legitimate no-match; the other is a typo silently
    /// answering "nothing is due" — unfalsifiable, exit 0. D27's rule for a
    /// filter TOKEN applies unchanged to a bound VALUE: on a read path nothing
    /// is lost by refusing, and the message must name the offending value.
    #[test]
    fn an_unparseable_due_bound_is_refused_and_named() {
        let anchor: Timestamp = "2026-07-19T12:00:00Z".parse().expect("anchor");
        for (input, offender) in [
            ("due.before:tomorow", "tomorow"),
            ("due.after:notadate", "notadate"),
            ("due.before:2026-13-45", "2026-13-45"),
            ("+api and due.before:fridya", "fridya"),
        ] {
            let err = Filter::parse(input, anchor)
                .expect_err("an unreadable date bound must be refused, not matched against");
            assert!(
                err.contains(offender),
                "the error must name {offender:?}: {err}"
            );
        }
    }

    /// A relative bound must resolve ONCE for the whole query. It is evaluated
    /// per row, so a bound re-resolved at match time would let `tomorrow` shift
    /// mid-evaluation across a midnight boundary — two rows with the same `due`
    /// answering differently in one list. Resolving at parse time is what makes
    /// that unrepresentable, and this pins it: the parsed filter holds an
    /// instant, so a later `now` cannot move it.
    #[test]
    fn a_relative_bound_is_resolved_once_at_parse_time() {
        let monday: Timestamp = "2026-07-20T12:00:00Z".parse().expect("anchor");
        let f = Filter::parse("due.before:tomorrow", monday).expect("parses");
        let just_inside = MatchCtx {
            status: Status::Pending,
            project: None,
            tags: &[],
            due: Some("2026-07-21T00:00:00Z"),
            completed: None,
            blocked: false,
        };
        // `tomorrow` at Monday noon is 2026-07-21T00:00:00Z, and the bound is
        // strict — so the row exactly on it is out and one second earlier is in,
        // no matter how much later `matches` runs.
        assert!(
            !f.matches(&just_inside),
            "the bound must stay at the instant parse resolved"
        );
        let earlier = MatchCtx {
            due: Some("2026-07-20T23:59:59Z"),
            ..just_inside
        };
        assert!(f.matches(&earlier));
    }

    /// `-tag` was the one predicate whose *evaluation* nothing exercised. The
    /// engine's `task.list` test covers `+tag`, and every context built in this
    /// module used an empty tag list — the single case where including and
    /// excluding return the same answer — so `Pred::TagExclude` was only ever
    /// asked about tasks that had no tags to exclude.
    ///
    /// Mutation testing found three separate one-character edits that the whole
    /// 299-test suite accepted: deleting the `!` in `eval_pred`, flipping its
    /// `==` to `!=`, and deleting the `!` in `predicate`'s emptiness check (which
    /// routes `-infra` to the always-true token instead of an exclusion). All
    /// three ship the same user-visible bug, and it is the worst possible shape:
    /// `tasqx list -infra` returns a full, plausible-looking table consisting of
    /// exactly the tasks the user asked to hide.
    #[test]
    fn tag_exclusion_hides_tagged_tasks_and_keeps_every_other_task() {
        let has_infra: Vec<String> = vec!["infra".into(), "api".into()];
        let other_tag: Vec<String> = vec!["docs".into()];
        let no_tags: Vec<String> = vec![];

        let f = parsed("-infra");
        assert!(
            !f.matches(&ctx_tagged(&has_infra)),
            "-infra must hide a task tagged infra"
        );
        // Load-bearing: a task carrying some *other* tag is what separates a
        // correct exclusion from one whose comparison has been inverted.
        assert!(
            f.matches(&ctx_tagged(&other_tag)),
            "-infra must keep a task tagged only docs"
        );
        assert!(
            f.matches(&ctx_tagged(&no_tags)),
            "-infra must keep an untagged task"
        );

        // The include/exclude pair must stay exact opposites on the same rows.
        let inc = parsed("+infra");
        for tags in [&has_infra, &other_tag, &no_tags] {
            assert_ne!(
                inc.matches(&ctx_tagged(tags)),
                f.matches(&ctx_tagged(tags)),
                "+infra and -infra disagree on {tags:?}"
            );
        }
    }

    /// **A closed vocabulary refuses a typo; an open one merely fails to match.**
    ///
    /// This test used to pin the opposite rule — `status:pendign` matched no row
    /// and said nothing — on D27's stated ground that "values are data and the
    /// set of valid ones is a runtime question". That ground is simply false for
    /// `status:`. [`Status::ALL`] is five compile-time variants; the set is as
    /// closed as the token grammar itself, and every *other* closed vocabulary in
    /// this codebase already refuses an unknown member — `parse_sort` on a sort
    /// key, `Status::parse` on `task.modify` and `store.import`, `Priority::parse`
    /// beside it. `status:` answering a typo with silence made it the last one,
    /// and made the same input a `bad_request` on the write path and a confident
    /// empty table on the read path.
    ///
    /// A project name or a tag stays on the old rule, and that is not an
    /// inconsistency: those vocabularies genuinely *are* runtime questions, and
    /// the write path already refuses an unknown project (D23), so a filter
    /// naming one cannot be answering a question the store could have answered.
    ///
    /// The empty value is in this list on purpose. `status:` with nothing after
    /// it is not a member of the closed set, so it takes the same refusal — which
    /// also matches how the grammar already treats an empty value for `+` and
    /// `-`, where a bare `+` is an unknown token rather than a match-nothing tag.
    #[test]
    fn an_unrecognised_status_value_is_refused_naming_the_accepted_set() {
        // All whitespace-free: a value containing a space is split by the
        // tokenizer long before status parsing sees it, so those inputs would
        // exercise the tokenizer rather than this rule.
        for bogus in ["bogus", "canceled", "PENDING", "Done", "pending2", ""] {
            let err = Filter::parse(&format!("status:{bogus}"), anchor()).expect_err(&format!(
                "status:{bogus:?} must be refused, not silently match nothing"
            ));
            assert!(
                err.contains(&format!("{bogus:?}")),
                "the refusal must name the offending value; got {err:?}"
            );
            // Driven off `Status::ALL`, so a sixth variant joins the message the
            // day it exists rather than when someone remembers this string.
            for s in Status::ALL {
                assert!(
                    err.contains(s.as_str()),
                    "the refusal must list {:?} as an accepted value; got {err:?}",
                    s.as_str()
                );
            }
        }
    }

    /// Every real status name must select exactly its own rows — the property the
    /// old `ctx.status == s` string compare gave for free and that parsing must
    /// not quietly lose. Driven off `Status::ALL`, so a new variant is covered
    /// the moment it exists rather than when someone remembers to add a case.
    #[test]
    fn each_status_value_selects_exactly_that_status() {
        for want in Status::ALL {
            let f = parsed(&format!("status:{}", want.as_str()));
            for have in Status::ALL {
                assert_eq!(
                    f.matches(&ctx_for(have)),
                    want == have,
                    "status:{} vs a {have:?} row",
                    want.as_str()
                );
            }
        }
    }

    /// `@working` is documented as "pending|active AND not blocked". It used to
    /// be two string equality checks; now it is a `matches!` on the enum, and the
    /// set it covers is exactly the thing a new `Status` variant would perturb.
    /// Pinned by enumeration so the doc comment and the code cannot drift apart.
    #[test]
    fn working_covers_pending_and_active_only_and_never_when_blocked() {
        let f = parsed("@working");
        for status in Status::ALL {
            let want = matches!(status, Status::Pending | Status::Active);
            assert_eq!(f.matches(&ctx_for(status)), want, "@working vs {status:?}");

            let blocked = MatchCtx {
                blocked: true,
                ..ctx_for(status)
            };
            assert!(
                !f.matches(&blocked),
                "@working must exclude blocked {status:?}"
            );
        }
    }

    /// `status:blocked` is the one input where the token a user typed and the
    /// predicate they get diverge: `predicate()` maps it to `Pred::Blocked`, not
    /// `Pred::Status`, so D24's default still fires even though the word
    /// `status:` was typed. That is deliberate, not an oversight. `blocked` is a
    /// *derived flag* (has >=1 unresolved dependency), not a member of the
    /// status set — and a cancelled task can still carry unresolved edges, so
    /// without the default "show me blocked work" would quietly include work
    /// nobody will ever unblock. Pinned here so a future editor who spots the
    /// asymmetry changes it on purpose rather than by accident.
    #[test]
    fn blocked_is_a_derived_flag_not_a_status_constraint() {
        for input in ["status:blocked", "@blocked", "+blocked"] {
            assert!(
                !parsed(input).constrains_status(),
                "{input:?} must not suppress the exclude-cancelled default"
            );
        }
    }

    /// A project or tag whose name contains a space was simply not expressible.
    /// `project:Home Renovation` tokenized to `project:Home` + a stray
    /// `Renovation`, and no quoting form worked because there was no quoting at
    /// all — so every filter naming such a project was silently wrong before
    /// D27 (the stray was the always-true term) and a hard error after it.
    ///
    /// The values here are deliberately hostile: a space needs the quoting, a
    /// `VALUE_PREFIXES` is a hand-maintained list, so a fifth `key:` predicate
    /// added to the grammar alone would lose shell quoting on that key with no
    /// other symptom than a confusing "unknown filter token".
    #[test]
    fn value_prefixes_match_the_grammar() {
        let mut seen = 0;
        for line in GRAMMAR.lines() {
            // `key:" VALUE` and `key:"  DATE` alike: both take an argument
            // after the colon, so both can carry a space the shell ate.
            //
            // The argument is recognised by SHAPE — a nonterminal starts with a
            // capital — rather than by listing the nonterminal names. Listing
            // them was itself the hand-maintained-parallel-list shape this guard
            // exists to police: renaming `RFC3339` to `DATE` made the scan stop
            // seeing two of the four predicates, and only the `seen == 4` floor
            // below caught it.
            let Some((lhs, rhs)) = line.split_once(":\"") else {
                continue;
            };
            if !rhs
                .trim_start()
                .starts_with(|c: char| c.is_ascii_uppercase())
            {
                continue;
            }
            let key = format!("{}:", lhs.rsplit('"').next().unwrap_or_default());
            seen += 1;
            assert!(
                VALUE_PREFIXES.contains(&key.as_str()),
                "`{key}` takes an argument in GRAMMAR but is not in VALUE_PREFIXES, so a                  shell-quoted value would be re-split at its spaces"
            );
        }
        // Without this the guard passes by matching nothing if GRAMMAR is
        // reformatted — the failure mode every text-scanning guard has.
        assert_eq!(seen, 6, "expected six `key:`-shaped predicates in GRAMMAR");
    }

    /// The refusal message must offer every token the grammar accepts.
    ///
    /// `TOKEN_SHAPES` was two hand-typed copies of one sentence, and a filter
    /// that accepts a token its own error does not list teaches the user the
    /// token does not exist — the read-side twin of the drift D30 rules against.
    /// Pinned to `VALUE_PREFIXES` rather than to a second list, so the check has
    /// nothing of its own to fall out of date.
    #[test]
    fn token_shapes_name_every_value_prefix() {
        for p in VALUE_PREFIXES {
            // `+`/`-` spell themselves as `+tag`/`-tag` in prose, since a bare
            // `+` is not something a user types.
            let needle = if p == "+" || p == "-" {
                format!("{p}tag")
            } else {
                p.to_string()
            };
            assert!(
                TOKEN_SHAPES.contains(&needle),
                "`{p}` is an accepted filter prefix but no refusal message offers it: {TOKEN_SHAPES}"
            );
        }
    }

    /// `from_argv` joins and nothing else. Documents the revert, so a future
    /// reader sees the absence is a decision rather than an omission.
    ///
    /// The guard that matters is the e2e one in `cli/tests/regressions.rs`:
    /// this test builds the argv split itself and so would agree with a wrong
    /// split.
    #[test]
    fn from_argv_joins_and_does_not_guess() {
        let go = |args: &[&str]| from_argv(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        // Several elements stay several tokens, as they always did.
        assert_eq!(go(&["+api", "status:done"]), "+api status:done");
        // Literal quotes pass through to the tokenizer, which understands them.
        assert_eq!(go(&[r#"+"needs paint""#]), r#"+"needs paint""#);
        // An expression in one element stays an expression. The re-quoting
        // heuristic read this as one tag named `api or +web` and answered
        // "No tasks." — the silent wrong answer that got it reverted.
        assert_eq!(go(&["+api or +web"]), "+api or +web");
        assert_eq!(go(&["(+api or +web)"]), "(+api or +web)");
        // And the shell-stripped value is NOT put back together. It is two
        // tokens, which is what `spacing_hint` then explains.
        assert_eq!(go(&["project:Home Renovation"]), "project:Home Renovation");
    }

    /// The literal-quote spelling is the one the tool teaches, so it must work
    /// on both a `key:` value and a tag — and the shell-stripped spelling must
    /// FAIL, loudly, naming the literal spelling that fixes it.
    ///
    /// Both halves in one test on purpose: "it is refused" without "and here is
    /// the fix" is the state N1a would have shipped if the hint were optional.
    #[test]
    fn a_shell_stripped_spaced_value_is_refused_and_taught_the_quoted_spelling() {
        let tags = ["needs paint".to_string()];
        let ctx = MatchCtx {
            status: Status::Pending,
            project: Some("Home Renovation"),
            tags: &tags,
            due: None,
            completed: None,
            blocked: false,
        };
        for (stripped, literal) in [
            ("+needs paint", r#"+"needs paint""#),
            ("project:Home Renovation", r#"project:"Home Renovation""#),
        ] {
            assert!(
                Filter::parse(literal, anchor())
                    .expect("the literal form parses")
                    .matches(&ctx),
                "{literal:?} must select"
            );
            let err = Filter::parse(stripped, anchor()).expect_err("{stripped:?} must be refused");
            assert!(
                err.contains(literal),
                "{stripped:?} must name the spelling that works: {err}"
            );
            assert!(err.contains("quote"), "{stripped:?} must say why: {err}");
        }
    }

    /// The hint must not fire where it would be wrong advice. A value already
    /// spelled with literal quotes is correct, so a stray word after it is
    /// genuinely stray — proposing to swallow it would advise breaking a
    /// working filter — and a token that opens a predicate of its own is a
    /// token the user meant.
    #[test]
    fn the_quoting_hint_stays_out_of_filters_it_would_mislead() {
        for input in [
            r#"project:"Home Renovation" Renovation"#,
            "+api Renovation",
            "Renovation",
        ] {
            let err = Filter::parse(input, anchor()).expect_err("still refused");
            assert!(err.contains("unknown filter token"), "{input:?}: {err}");
        }
        // Only the first of those three is about a quoted value; the middle one
        // legitimately DOES get the hint (`+"api Renovation"`), so pin the two
        // that must not.
        for input in [r#"project:"Home Renovation" Renovation"#, "Renovation"] {
            let err = Filter::parse(input, anchor()).expect_err("still refused");
            assert!(
                !err.contains("did you mean"),
                "{input:?} must not be hinted at: {err}"
            );
        }
    }

    /// `(` proves quoting also suppresses grouping, and a `"` proves the escape
    /// exists. Asserted through `matches` rather than on the token list, so this
    /// guards what a user gets back and not how the tokenizer spells it.
    #[test]
    fn a_quoted_value_carries_spaces_parens_and_quotes_through_to_matching() {
        for name in [
            "Home Renovation",
            "a (b)",
            "say \"hi\"",
            "back\\slash",
            "  padded  ",
        ] {
            let f = parsed(&format!("project:{}", quote(name)));
            let ctx = MatchCtx {
                status: Status::Pending,
                project: Some(name),
                tags: &[],
                due: None,
                completed: None,
                blocked: false,
            };
            assert!(
                f.matches(&ctx),
                "project:{name:?} must match its own project"
            );
            // Load-bearing: without it, a filter that lost everything after the
            // first space would still "pass" against a project named `Home`.
            let other = MatchCtx {
                project: Some("Home"),
                ..ctx
            };
            assert!(
                !f.matches(&other),
                "project:{name:?} must not match a mere prefix"
            );
        }
    }

    /// The same round trip for a tag, which reaches `predicate` down the `+`/`-`
    /// arm instead of the `project:` one — a quoting fix applied only where the
    /// bug was reported would pass the test above and leave this broken.
    #[test]
    fn a_quoted_tag_value_round_trips_through_include_and_exclude() {
        let name = "needs paint";
        let tags: Vec<String> = vec![name.to_string()];
        let none: Vec<String> = vec![];
        assert!(parsed(&format!("+{}", quote(name))).matches(&ctx_tagged(&tags)));
        assert!(!parsed(&format!("+{}", quote(name))).matches(&ctx_tagged(&none)));
        assert!(!parsed(&format!("-{}", quote(name))).matches(&ctx_tagged(&tags)));
        assert!(parsed(&format!("-{}", quote(name))).matches(&ctx_tagged(&none)));
    }

    /// Quoting suppresses the *word-splitting* metacharacters — whitespace and
    /// parentheses — exactly as a shell does, which is the rule that makes it
    /// teachable. A project named `a (b)` otherwise breaks grouping, which is
    /// the space bug wearing a different hat: the `(` opens a group nobody
    /// opened and the parse either fails or silently reassociates.
    #[test]
    fn quoting_suppresses_grouping_and_keyword_meaning() {
        let ctx = MatchCtx {
            status: Status::Done,
            project: Some("a (b) or c"),
            tags: &[],
            due: None,
            completed: None,
            blocked: false,
        };
        // The whole value is one predicate: if `(`, `)` or `or` kept their
        // meaning this is a different — and matching — filter tree.
        let f = parsed(&format!("project:{} and status:done", quote("a (b) or c")));
        assert!(f.matches(&ctx));
        // And the unquoted spelling must NOT quietly do something plausible.
        assert!(Filter::parse("project:a (b) or c and status:done", anchor()).is_err());
    }

    /// An unterminated quote is an error, not a quote silently closed at end of
    /// input. Same reasoning as the unclosed `(` above it: guessing what the
    /// user meant evaluates a filter they did not write.
    #[test]
    fn an_unterminated_quote_is_refused() {
        for bad in ["project:\"Home", "project:\"", "+\"a b", "project:\"a\\"] {
            let err = Filter::parse(bad, anchor()).unwrap_err();
            assert!(
                err.contains("unterminated"),
                "{bad:?} must be refused as unterminated, got {err:?}"
            );
        }
    }

    /// `quote` is the one escaping helper every composition site uses, so its
    /// round trip is the property the whole fix rests on. Asserted over values
    /// chosen to hit each escape rule, and by *parsing back*, not by comparing
    /// against a second hand-written escaping — that would only prove `quote`
    /// agrees with itself.
    #[test]
    fn quote_round_trips_every_value_through_the_parser() {
        for v in [
            "plain",
            "two words",
            "a (b)",
            "quote\"inside",
            "back\\slash",
            "\\",
            "\"",
            "",
            "and",
            "or",
            "(",
            ")",
            "+tag",
            "project:x",
            "tab\there",
        ] {
            let f = Filter::parse(&format!("project:{}", quote(v)), anchor())
                .unwrap_or_else(|e| panic!("quote({v:?}) must parse back: {e}"));
            let ctx = MatchCtx {
                status: Status::Pending,
                project: Some(v),
                tags: &[],
                due: None,
                completed: None,
                blocked: false,
            };
            assert!(f.matches(&ctx), "quote({v:?}) did not round trip");
        }
    }

    /// C8: the WRITE side must close the same round trip over the same values.
    ///
    /// `quote` composes a literal that the read side parses back; this asserts
    /// the write side's scanner reads that identical literal back to the same
    /// value, as ONE word — so anything `quote` can emit, `tasqx add` can type.
    /// Before the two sides shared a scanner, `quote("quote\"inside")` was
    /// unreadable by sugar (it stored `quoteinside`), which made the escape this
    /// grammar documents unmatchable for tags.
    ///
    /// The same list as the read-side round trip above, deliberately: one value
    /// added there and not here is exactly how the two would drift apart again.
    #[test]
    fn split_words_reads_back_everything_quote_can_emit() {
        for v in [
            "plain",
            "two words",
            "a (b)",
            "quote\"inside",
            "back\\slash",
            "\\",
            "\"",
            "",
            "and",
            "or",
            "(",
            ")",
            "+tag",
            "project:x",
            "tab\there",
        ] {
            let src = format!("project:{}", quote(v));
            let words = split_words(&src, "task text")
                .unwrap_or_else(|e| panic!("quote({v:?}) must scan on the write side: {e}"));
            assert_eq!(words.len(), 1, "quote({v:?}) must stay ONE word: {src}");
            assert_eq!(
                words[0].text,
                format!("project:{v}"),
                "quote({v:?}) lost its value"
            );
            assert!(words[0].quoted, "quote({v:?}) is quoted by construction");
        }
    }

    /// Parens are the ONE difference between the two callers, and it is lexical:
    /// the read side groups with them, the write side has a title that may
    /// contain them (`tasqx add "call (mom)"`). Everything about QUOTED is shared.
    #[test]
    fn parens_break_tokens_only_on_the_read_side() {
        let words = split_words("call (mom)", "task text").expect("scans");
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(
            texts,
            ["call", "(mom)"],
            "a paren is ordinary text in a title"
        );

        let toks = tokenize("call (mom)").expect("scans");
        let texts: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(
            texts,
            ["call", "(", "mom", ")"],
            "but grouping on the read side"
        );
    }

    /// An unterminated quote is refused on both sides, and the message names the
    /// surface it was refused on — "in filter" is a lie about a `tasqx add`.
    #[test]
    fn an_unterminated_quote_is_refused_and_names_its_surface() {
        // `.err()` rather than `expect_err`, which would demand Debug on the
        // success type purely to serve a test.
        let e = split_words("+say\"hi", "task text")
            .err()
            .expect("must be refused");
        assert!(e.contains("unterminated") && e.contains("task text"), "{e}");
        let e = tokenize("+say\"hi").err().expect("must be refused");
        assert!(e.contains("unterminated") && e.contains("filter"), "{e}");
    }

    /// A doubled dash is a mistyped flag, not a tag exclusion.
    ///
    /// The dash count is the whole discriminator between filter text and a
    /// flag: `cli/argv.rs` reads it to decide what to hide from clap, and on
    /// the API and MCP paths this parser is the only thing that reads it at
    /// all. Parsed as a tag exclusion, `--json` excluded nothing and so matched
    /// everything, turning a typo into a silently wider result set. The
    /// single-dash form must keep working exactly as before.
    #[test]
    fn a_doubled_dash_is_rejected_rather_than_matching_everything() {
        let err =
            Filter::parse("--json", anchor()).expect_err("`--json` is a flag, not a tag exclusion");
        assert!(
            err.contains("--json"),
            "the error must name what was typed: {err}"
        );
        assert!(
            err.contains("-tag"),
            "and point at the shape that works: {err}"
        );

        let f = Filter::parse("-needs", anchor()).expect("one dash is still a tag exclusion");
        let tagged = vec!["needs".to_string()];
        fn ctx(tags: &[String]) -> MatchCtx<'_> {
            MatchCtx {
                status: Status::Pending,
                project: None,
                tags,
                due: None,
                completed: None,
                blocked: false,
            }
        }
        assert!(
            !f.matches(&ctx(&tagged)),
            "-needs must exclude the tagged task"
        );
        assert!(f.matches(&ctx(&[])), "-needs must keep everything else");
    }

    // ---- the grammar block is documentation, and documentation drifts --------

    /// The grammar's own RHS symbols, minus comments and quoted literals.
    ///
    /// Only ALL-CAPS names are collected. Lowercase productions are the shape of
    /// the parser and change when it does; the names that rotted here were the
    /// primitives (`WORD`, `CHAR`), which nothing forced anyone to define.
    fn referenced_symbols(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let rhs = line.split_once(":=").map_or(line, |(_, r)| r);
            // Drop `"literal"` runs so a terminal is never mistaken for a symbol.
            let mut outside = String::new();
            let mut in_quote = false;
            for c in rhs.chars() {
                if c == '"' {
                    in_quote = !in_quote;
                    outside.push(' ');
                } else if !in_quote {
                    outside.push(c);
                }
            }
            for word in outside.split(|c: char| !c.is_ascii_alphanumeric()) {
                let is_symbol = word.len() >= 2
                    && word.starts_with(|c: char| c.is_ascii_uppercase())
                    && word
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
                if is_symbol {
                    out.push(word.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn defined_symbols(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|l| l.split_once(":=").map(|(lhs, _)| lhs.trim().to_string()))
            .collect()
    }

    /// A grammar that uses a name it never defines is not a grammar, it is a
    /// gesture at one. `WORD` was used in five productions and defined nowhere,
    /// for as long as the block has existed, so a reader had to guess whether it
    /// admitted a space, a quote, or a parenthesis — the exact question the block
    /// is there to answer.
    #[test]
    fn the_grammar_defines_every_symbol_it_uses() {
        let text = grammar();
        let defined = defined_symbols(text);
        let missing: Vec<String> = referenced_symbols(text)
            .into_iter()
            .filter(|s| !defined.contains(s))
            .collect();
        assert!(
            missing.is_empty(),
            "grammar uses undefined symbol(s) {missing:?}:\n{text}"
        );
    }

    /// Every predicate that takes a value must *say* it takes a value, because
    /// the parser accepts a quoted one for all of them. The block claimed `WORD`
    /// for the two tag forms while the prose two paragraphs below advertised
    /// `+"needs paint"` — the block and its own surrounding text disagreed, and
    /// the code sided with the text.
    ///
    /// The probe value is per-prefix rather than one shared `"two words"`,
    /// because `status:` draws from a closed vocabulary and now refuses a value
    /// outside it. The quoting is still what is under test — `status:"pending"`
    /// only parses if the tokenizer delivered the quoted value whole — and using
    /// a valid member keeps this guard about the grammar block instead of
    /// accidentally re-testing the status refusal.
    #[test]
    fn every_value_taking_predicate_is_written_as_taking_a_value() {
        let text = grammar();
        for (prefix, value) in [
            ("+", "two words"),
            ("-", "two words"),
            ("project:", "two words"),
            ("status:", "pending"),
        ] {
            let filter = format!("{prefix}\"{value}\"");
            Filter::parse(&filter, anchor())
                .unwrap_or_else(|e| panic!("the parser accepts {filter:?}, so: {e}"));
            let line = text
                .lines()
                .find(|l| l.contains(&format!("\"{prefix}\"")))
                .unwrap_or_else(|| panic!("no grammar line for the {prefix:?} predicate:\n{text}"));
            assert!(
                line.contains("VALUE"),
                "{prefix:?} takes a quoted value but the grammar says otherwise: {line}"
            );
        }
    }

    /// A filter nested past the cap must come back as an `Err`, and the process
    /// must still be alive to return it.
    ///
    /// Every `(` costs three stack frames — `parse_or` -> `parse_and` ->
    /// `parse_term` -> `parse_or` — so before the cap this input did not fail,
    /// it ABORTED: `fatal runtime error: stack overflow`, SIGABRT, which Rust
    /// gives no way to catch. The filter string arrives verbatim from
    /// `task.list` / `store.export` / `report.*` / `watch`, so any client could
    /// end the daemon process for every other client with one call, defeating
    /// the `catch_unwind` in `handle_conn` that exists precisely so a bad
    /// request never takes the daemon down (an abort is not an unwind).
    ///
    /// 100_000 and not a friendlier number on purpose: a release build survived
    /// 10_000, so a smaller case here would have passed against the unfixed
    /// parser and proved nothing. ~50 KB of input is also well under the
    /// daemon's `MAX_FRAME_BYTES` (1 MiB), which is why the frame cap was not
    /// already a guard.
    #[test]
    fn deep_nesting_is_refused_rather_than_overflowing_the_stack() {
        let err = Filter::parse(&"(".repeat(100_000), anchor())
            .expect_err("100k nested groups must be refused, not parsed");
        assert!(
            err.contains("nest") && err.contains(&MAX_NESTING.to_string()),
            "the refusal must name nesting and the cap, got {err:?}"
        );
        // The unclosed-`(` check must not shadow it: a *closed* nest is equally
        // fatal and equally refused, so the cap is what is being asserted here
        // rather than the balance check that happens to sit on the same path.
        let closed = format!("{}@working{}", "(".repeat(100_000), ")".repeat(100_000));
        let err = Filter::parse(&closed, anchor())
            .expect_err("a balanced 100k-deep nest is still too deep");
        assert!(
            err.contains("nest"),
            "a balanced deep nest must be refused for nesting, not for balance, got {err:?}"
        );
    }

    /// The cap has to sit far above anything a person or a composer writes, and
    /// the boundary has to be exact — a cap that refuses one group fewer than it
    /// advertises turns a working filter into a `bad_request` for no reason.
    ///
    /// `html.rs` wraps the caller's filter in one more paren (`({f}) and
    /// @working`), so the usable depth for a client is the cap minus one; at 64
    /// that is still two orders of magnitude past any real filter.
    #[test]
    fn nesting_up_to_the_cap_still_parses_and_groups() {
        let deep = format!(
            "{}project:home{}",
            "(".repeat(MAX_NESTING as usize),
            ")".repeat(MAX_NESTING as usize)
        );
        let ctx = MatchCtx {
            status: Status::Pending,
            project: Some("home"),
            tags: &[],
            due: None,
            completed: None,
            blocked: false,
        };
        // Parses AND still means what it says: a cap that quietly truncated the
        // tree would also "parse".
        assert!(parsed(&deep).matches(&ctx));
        assert!(!parsed(&deep).matches(&ctx_for(Status::Pending)));

        let over = format!(
            "{}project:home{}",
            "(".repeat(MAX_NESTING as usize + 1),
            ")".repeat(MAX_NESTING as usize + 1)
        );
        assert!(
            Filter::parse(&over, anchor()).is_err(),
            "one group past the cap must be refused, or the cap is not the cap"
        );
    }

    /// Depth is a depth, not a running total. Incrementing on `(` without
    /// giving the count back on `)` is the easy way to write this cap, and it
    /// refuses `(+a) (+b) (+c) ...` — a flat list of sibling groups that never
    /// nests at all and costs no stack — once the list is longer than the cap.
    /// That failure mode is invisible to the deep-nesting test above, and it
    /// breaks filters people actually write.
    #[test]
    fn sibling_groups_do_not_accumulate_depth() {
        let flat = vec!["(project:home)"; 5_000].join(" or ");
        let ctx = MatchCtx {
            status: Status::Pending,
            project: Some("home"),
            tags: &[],
            due: None,
            completed: None,
            blocked: false,
        };
        // Not through `parsed`: its panic message echoes the filter, and this
        // one is 70 KB of `(project:home)`.
        let f = Filter::parse(&flat, anchor())
            .unwrap_or_else(|e| panic!("5000 sibling groups nest one deep, so: {e}"));
        assert!(f.matches(&ctx));
    }
}
