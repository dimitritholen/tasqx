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
//! **A value may be double-quoted**, which is the only way to name a project
//! containing a space: `project:"Home Renovation"`. (A tag cannot contain one
//! since D172; a `+`/`-` value is lowercased to match the stored form.) The
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
use crate::types::{Priority, Status};
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
/// `value_prefixes_match_the_grammar` scans it for `key:"` shapes: an example
/// moved inside would count as a seventh predicate, and would then fail that
/// guard's per-line assert outright — the key it derives from
/// `tasqx list 'project:"Home Renovation"'` is `tasqx list 'project:`, which is
/// in no list of prefixes and never will be.
pub const GRAMMAR: &str = "\
filter     := or_expr
or_expr    := and_expr ( \"or\" and_expr )*
and_expr   := term ( \"and\"? term )*        # juxtaposition = implicit AND
term       := \"(\" or_expr \")\" | predicate
predicate  := \"+\" VALUE                    # require tag; VALUE not empty, lowercased (D172)
            | \"-\" VALUE                    # exclude tag; VALUE not empty, not starting with a dash, lowercased
            | \"@working\"                   # status in {pending,active} AND not blocked
            | \"@blocked\" | \"+blocked\" | \"status:blocked\"   # the blocked flag
            | \"project:\" VALUE
            | \"proj:\" VALUE                  # alias of project: (#229 item 5)
            | \"status:\" VALUE                # `any` or `all` = every status, done and cancelled too (D210)
            | \"priority:\" VALUE
            | \"title:\" VALUE                 # substring of the title, case-insensitive (D210)
            | WORD | QUOTED                    # a bare word or quoted phrase is \"title:\" of itself
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
    /// `priority:VALUE`, with VALUE already resolved to the enum through
    /// [`Priority::parse`] — the same forgiving parser `task.add`/`task.modify`
    /// use, so `priority:H`, `priority:high` and `priority:HIGH` all work and a
    /// caller cannot type a spelling the write side accepts and the filter
    /// refuses. A [`Priority`] and not a `String` for the reason [`Pred::Status`]
    /// gives: it is a closed, compile-time set (D34), so holding the variant
    /// makes an unreadable value unrepresentable rather than a validation call
    /// `eval_pred` would have to remember to make per row.
    Priority(Priority),
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
    /// The blocked flag: an open task with >=1 dependency not yet resolved —
    /// `done` or `cancelled` (DESIGN §3, D11, D145); a closed task is never
    /// blocked.
    Blocked,
    /// `status:any` / `status:all` (D210): no restriction on status, done and
    /// cancelled included. Not [`Pred::Always`], so [`Filter::constrains_status`]
    /// can see the caller named a status and let the report and agenda defaults
    /// step aside.
    AnyStatus,
    /// A title term (D210): the text, already lowercased, must be a substring of
    /// the lowercased [`MatchCtx::title`]. A bare word, a quoted phrase and
    /// `title:VALUE` all land here. Matching is in memory, so `%` and `_` are
    /// ordinary characters; there is no SQL `LIKE` to escape for.
    Title(String),
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
    /// The row's priority, `None` when it carries none — the same value
    /// `urgency::score_at` already reads, so `priority:` filters on exactly what
    /// every other reader of this field sees.
    pub priority: Option<Priority>,
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
    /// Whether the row is an open task with at least one dependency not yet
    /// resolved — `done` or `cancelled` (DESIGN §3, D11, D145); a closed task
    /// is never blocked. Precomputed by the caller: it needs a join, and
    /// re-deriving it per predicate would run that join once for `@working`
    /// and again for `@blocked` in the same expression.
    pub blocked: bool,
    /// The row's title, which the title terms (D210) are substring-matched
    /// against, case-insensitively.
    pub title: &'a str,
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

    /// True when `ctx` satisfies the filter, title terms (a bare word, a quoted
    /// phrase, `title:`; D210) included: they match against [`MatchCtx::title`].
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

    /// Whether `@working` appears anywhere in this filter, `Or` branches
    /// included (D215: the working set leaves out an archived project's tasks
    /// unless the filter names the project).
    pub fn mentions_working(&self) -> bool {
        fn walk(e: &Expr) -> bool {
            match e {
                Expr::And(v) | Expr::Or(v) => v.iter().any(walk),
                Expr::Pred(Pred::Working) => true,
                Expr::Pred(_) => false,
            }
        }
        walk(&self.root)
    }

    /// Every `project:`/`proj:` value this filter names, duplicates included,
    /// in the order they appear in the tree.
    ///
    /// D109's seam: the vocabulary is open at PARSE time (a project name is
    /// runtime data, not [`Status`]'s closed set — see the module comment's
    /// split), but the caller of `parse` — `task.list`, `report.summary`,
    /// `store.export`, the only three call sites (D109) — holds a live
    /// project table a parsed [`Filter`] does not, and is the seam where an
    /// unknown or wrong-case name can finally be told apart from a legitimate
    /// zero-row answer. This is what a caller walks to ask that question,
    /// rather than restating the tree shape at every call site.
    pub fn project_names(&self) -> Vec<&str> {
        let mut out = Vec::new();
        collect_project_names(&self.root, &mut out);
        out
    }

    /// True when the caller asked for no filtering at all — the empty string,
    /// parsed to `Pred::Always` (see [`Filter::parse`]).
    ///
    /// D171's seam: `store.export` must keep scoping `docs`/`projects`/`events`
    /// OFF for this one case, because D12/D37 promise a byte-identical,
    /// nothing-trimmed round trip for an unfiltered export — including a
    /// project or doc that no task references. Any other filter, even one that
    /// happens to match every row, narrows on purpose and is scoped.
    pub fn is_unfiltered(&self) -> bool {
        matches!(self.root, Expr::Pred(Pred::Always))
    }

    /// The VALUE carried by the single predicate this filter is — `None` when it
    /// is not exactly one predicate, or when that predicate carries no value.
    ///
    /// # What it is for, and why a bare "does it parse?" is not enough
    ///
    /// This is the read-side twin of `cli/sugar.rs`'s `parsed_value_of`, and it
    /// exists for one caller: the shell completion in `tasqx-cli`, which
    /// composes a candidate out of a prefix from [`VALUE_PREFIXES`] and a value
    /// out of the user's store and must not offer one this parser reads
    /// DIFFERENTLY. Asking only whether the composed word parses is not the same
    /// question, and the difference is not theoretical:
    ///
    ///  * a tag genuinely named `blocked` composes `+blocked`, which parses
    ///    perfectly — as [`Pred::Blocked`], the derived flag, not as that tag.
    ///    Offering it means Tab silently swaps "tasks tagged blocked" for "tasks
    ///    with an unresolved dependency", at exit 0. `sole_value` answers `None`
    ///    there, because `Blocked` carries no value, and the candidate is
    ///    withheld.
    ///  * a tag named `-lead` composes `--lead` under the exclusion prefix,
    ///    which `predicate` refuses as a mistyped flag. That one a
    ///    parse check would also catch; the first one it would not.
    ///
    /// So the gate is "the parser takes the same value back out", and it reads
    /// the answer from the parser rather than from a rule restated in the CLI.
    ///
    /// A date bound answers `None` even though it took a value: the bound is
    /// resolved to a [`Timestamp`] at parse time (see [`Pred::DueBefore`]) and
    /// the string is gone by design. Nothing needs it — the date vocabulary is
    /// open, so no caller composes a date candidate to check.
    pub fn sole_value(&self) -> Option<&str> {
        let Expr::Pred(pred) = &self.root else {
            return None;
        };
        match pred {
            Pred::Project(v) | Pred::TagInclude(v) | Pred::TagExclude(v) => Some(v.as_str()),
            Pred::Status(s) => Some(s.as_str()),
            Pred::Priority(p) => Some(p.as_str()),
            // Matched variant by variant with no wildcard: a predicate added
            // tomorrow must be classified as carrying a value or not, rather
            // than inheriting `None` and quietly dropping its own candidates.
            Pred::DueBefore(_)
            | Pred::DueAfter(_)
            | Pred::AnyStatus
            | Pred::Title(_)
            | Pred::CompletedBefore(_)
            | Pred::CompletedAfter(_)
            | Pred::Working
            | Pred::Blocked
            | Pred::Always => None,
        }
    }
}

fn constrains_status(e: &Expr) -> bool {
    match e {
        Expr::And(v) | Expr::Or(v) => v.iter().any(constrains_status),
        // `@working` counts: it expands to `status in {pending,active}`, so the
        // caller has named a status set just as explicitly as `status:pending`.
        Expr::Pred(Pred::AnyStatus | Pred::Status(_) | Pred::Working) => true,
        Expr::Pred(_) => false,
    }
}

/// [`Filter::project_names`]'s walk, over borrowed [`Pred::Project`] strings so
/// the caller pays no allocation for a filter that names none.
fn collect_project_names<'a>(e: &'a Expr, out: &mut Vec<&'a str>) {
    match e {
        Expr::And(v) | Expr::Or(v) => v.iter().for_each(|c| collect_project_names(c, out)),
        Expr::Pred(Pred::Project(name)) => out.push(name.as_str()),
        Expr::Pred(_) => {}
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
        Pred::Always | Pred::AnyStatus => true,
        Pred::Title(t) => ctx.title.to_lowercase().contains(t.as_str()),
        // A plain enum comparison: an unreadable value never reaches here,
        // because `predicate()` refused it at parse time.
        Pred::Status(s) => *s == ctx.status,
        // No priority never satisfies a `priority:` bound, the same rule an
        // undated task has for `due.before:`/`due.after:` — there is no value to
        // compare, so it is not "H" and it is not "not H" either.
        Pred::Priority(p) => ctx.priority == Some(*p),
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

/// Where a value-taking predicate's VALUE comes from.
///
/// The point of the enum, rather than a bare list of prefixes, is that a
/// consumer can ANSWER the question "what values are legal after this prefix?"
/// — which is what the CLI's shell completion needs and what nothing here
/// previously said out loud. `project:` and `+` both take a runtime name, but
/// from two different vocabularies, and `status:` takes a compile-time one; a
/// consumer that could not tell them apart would have to restate the mapping,
/// which is the parallel-list shape `TOKEN_SHAPES`'s own guard exists to
/// prevent one field over.
///
/// **Deliberately NOT `#[non_exhaustive]`.** That attribute would force every
/// out-of-crate `match` to carry a wildcard arm, and the wildcard is precisely
/// what must not exist: a ninth prefix added to [`VALUE_PREFIXES`] has to be a
/// COMPILE ERROR in `tasqx-cli`'s completion dispatcher, not a silently empty
/// menu for the one prefix nobody remembered. The cost is that adding a variant
/// is a breaking change for downstream crates; the workspace has exactly one
/// consumer, and a breaking change that names its own call sites is the cheap
/// half of this trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vocabulary {
    /// A tag name: open, runtime, and drawn from whatever tasks carry.
    Tag,
    /// A project name: open and runtime, exactly as [`Pred::Project`] holding a
    /// `String` rather than an enum records.
    Project,
    /// A [`Status`]: closed and compile-time, which is why a typo here is
    /// refused by name (see the module comment's split) and why it is the one
    /// vocabulary a caller can enumerate without touching a store.
    Status,
    /// A [`Priority`]: closed and compile-time on the same terms as `Status`,
    /// so a typo here is refused rather than answered with a silent "no tasks".
    Priority,
    /// A date bound, taking whatever [`crate::datetime::parse_when`] takes.
    /// Open in the strongest sense — the grammar is natural language
    /// (`tomorrow`, `in 3 days`, `eom`) and no module exports a list of accepted
    /// words, because there is no list.
    Date,
    /// Free text matched against the task title (D210): open, runtime, and
    /// offered no candidates, since the title is not a closed list.
    Text,
}

/// Value-taking predicate prefixes, i.e. the ones a quoted VALUE can follow,
/// each with the vocabulary its value is drawn from.
///
/// Kept beside the grammar it mirrors and pinned to it by
/// `value_prefixes_match_the_grammar`, because a seventh `key:` predicate added
/// to `GRAMMAR` alone would silently lose shell quoting on that key only.
/// `+`/`-` are here for the same reason they are in the grammar: a tag is a
/// VALUE like any other, it just spells its key as punctuation.
///
/// **Public, and the visibility is the decision.** It was private and read by
/// the (since deleted) spacing hint alone until `tasqx-cli` grew shell completion, which has to
/// answer "what token shapes may follow here?" on every Tab press. The
/// alternative was a second copy of these eight strings in the CLI — the exact
/// drift `token_shapes_name_every_value_prefix` was written to police after the
/// `completed.` pair was added to the grammar and forgotten in the refusal
/// message. A second copy in another crate would have been worse than the one
/// that guard already caught, because no guard in this file can see it. So the
/// list is exported instead, and its consumers read it rather than restating it.
///
/// The order is the grammar's, and is what a completion menu shows.
pub const VALUE_PREFIXES: [(&str, Vocabulary); 11] = [
    ("project:", Vocabulary::Project),
    ("proj:", Vocabulary::Project),
    ("status:", Vocabulary::Status),
    ("priority:", Vocabulary::Priority),
    ("due.before:", Vocabulary::Date),
    ("due.after:", Vocabulary::Date),
    ("completed.before:", Vocabulary::Date),
    ("completed.after:", Vocabulary::Date),
    ("+", Vocabulary::Tag),
    ("-", Vocabulary::Tag),
    ("title:", Vocabulary::Text),
];

/// The predicates that are a whole token by themselves — no value, no prefix.
///
/// The other half of the vocabulary [`VALUE_PREFIXES`] exports, and public for
/// the same reason: a consumer offering the grammar to a user needs all of it,
/// and half a menu is the shape this codebase keeps paying for.
///
/// `+blocked` and `status:blocked` are deliberately absent although
/// `predicate` accepts them. They are alternative spellings of `@blocked`, and
/// a menu that offers one concept three ways is noise; `@blocked` is the
/// canonical one, and it is the one the grammar lists first.
///
/// Pinned to [`GRAMMAR`] in both directions by `keywords_match_the_grammar`.
pub const KEYWORDS: [&str; 2] = ["@working", "@blocked"];

/// The boolean operators that join terms.
///
/// Not predicates: each one is an error on its own (`expected a filter term`)
/// and means something only between two terms, which is what
/// `operators_join_terms_rather_than_being_ones` pins. Exported beside the
/// predicates because a consumer teaching the grammar has to teach these too —
/// `or` is the only way to express a disjunction, and a completion menu that
/// silently omitted it would leave the boolean half of D8's grammar
/// undiscoverable.
pub const OPERATORS: [&str; 2] = ["and", "or"];

/// The token shapes an error message offers when it refuses one.
///
/// One string, two call sites. It was two hand-typed copies of the same
/// sentence, which is the parallel-list shape D30 rules against: adding
/// `completed.before:`/`completed.after:` meant editing both, and a filter that
/// accepts a token its own error message does not mention teaches the user that
/// the token does not exist. `token_shapes_name_every_value_prefix` pins it to
/// `VALUE_PREFIXES` so a seventh `key:` predicate cannot be advertised by the
/// grammar and omitted from the refusal.
const TOKEN_SHAPES: &str = "+tag, -tag, @working, @blocked, project: (or proj:), status:, \
                            priority:, title:, due.before:, due.after:, completed.before: or \
                            completed.after:";

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
/// the project check (D109) is what refuses `project:Home Renovation`.
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
        // #229 item 12: `and` used to be a word this loop SKIPPED regardless
        // of position — so `+pr and` (nothing after it), `and +pr` (nothing
        // before it), and `+pr and or +review` (the `and`'s operand slot
        // swallowed by the `or` right after it) all parsed as though `and`
        // had never been typed, silently widening the result exactly the way
        // a dangling `or` — already refused — would. `parse_and_operand`
        // makes a missing operand on EITHER side of `and` the same
        // `"expected a filter term"` a dangling `or` already produces.
        let mut parts = vec![self.parse_and_operand()?];
        loop {
            if self.peek().is_none() || self.is_sym(")") || self.is_kw("or") {
                break;
            }
            if self.is_kw("and") {
                self.pos += 1; // consume the explicit operator; an operand MUST follow
            }
            parts.push(self.parse_and_operand()?);
        }
        if parts.len() == 1 {
            Ok(parts.pop().unwrap())
        } else {
            Ok(Expr::And(parts))
        }
    }

    /// One operand of an `and_expr`. Refuses to start on `and`/`or`/`)`/
    /// end-of-input — rather than falling into [`Self::parse_term`], which
    /// indexes `self.toks[self.pos]` unconditionally and would panic on an
    /// empty tail — so every "nothing here" case this grammar can reach
    /// (a dangling `and`, a leading `and`, an empty group `()`, a bare `or`)
    /// answers with the one message, `"expected a filter term"`.
    fn parse_and_operand(&mut self) -> Result<Expr, String> {
        if self.peek().is_none() || self.is_sym(")") || self.is_kw("or") || self.is_kw("and") {
            return Err("expected a filter term".to_string());
        }
        self.parse_term()
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
        let Tok { text, quoted } = &self.toks[self.pos];
        let node = leaf(text, *quoted, self.now)?;
        self.pos += 1;
        Ok(node)
    }
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
            return Ok(Pred::TagInclude(rest.to_lowercase()));
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
            return Ok(Pred::TagExclude(rest.to_lowercase()));
        }
    }
    // `proj:` is the write side's alias for `project:` (`tasqx-cli`'s
    // `sugar.rs`, and the manual documents it); the read side used to refuse
    // it, which meant an alias learned from `add` or the manual hit a wall
    // on every read verb sharing this one parser (#229 item 5).
    if let Some(v) = tok
        .strip_prefix("project:")
        .or_else(|| tok.strip_prefix("proj:"))
    {
        // The empty value is refused, for the reason the `status:` arm below
        // states and every other value prefix already obeys: `project:` names no
        // project, and a token that names nothing is unknown rather than a
        // constraint that matches nothing.
        //
        // It was the ONE prefix that accepted it, and the exception was silent
        // in the way this codebase hunts: `tasqx list project:` printed
        // "No tasks." at exit 0, `tasqx list +api project:` narrowed a real
        // filter to nothing just as quietly, and `tasqx export project:` wrote a
        // well-formed backup envelope containing none of the store's tasks — a
        // backup that looks fine and holds nothing. Every sibling exits 2.
        //
        // Found because shell completion began offering `project:` as a stub the
        // user goes on typing, which put the one silent token in the grammar a
        // single keystroke away from Enter.
        if v.is_empty() {
            // The token IS recognised — `project:` is one of `TOKEN_SHAPES` —
            // so "unknown filter token" contradicts the message's own list.
            // The fault is the missing value, and saying so is the only
            // repair an agent can retry into something that actually parses.
            return Err("`project:` needs a value — e.g. project:finly-next, or \
                 project:\"tasqx review 2026-09\" when it contains a space"
                .to_string());
        }
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
    if let Some(v) = tok.strip_prefix("priority:") {
        // Delegates to `Priority::parse` rather than restating its table, for
        // the reason its own docs give: the write side (`task.add`, `!high`) and
        // this predicate must accept exactly the same spellings, or a caller
        // could set a priority through one door that the other cannot find.
        // The empty value falls through to `None` and is refused with the rest,
        // the same rule `status:` and `project:` already apply.
        return Priority::parse(v).map(Pred::Priority).ok_or_else(|| {
            format!(
                "unknown priority {v:?} (expected one of: {})",
                Priority::SPELLINGS
                    .iter()
                    .map(|(s, _)| *s)
                    .collect::<Vec<_>>()
                    .join(", ")
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
    Err(format!("{UNKNOWN_TOKEN} {tok:?} (expected {TOKEN_SHAPES})"))
}

/// The start of [`predicate`]'s refusal of a token it does not know, which
/// [`leaf`] reads to tell "not a filter token" from "a malformed one".
const UNKNOWN_TOKEN: &str = "unknown filter token";

/// Map one token to a tree leaf: the D210 terms first, then [`predicate`].
///
/// A token `predicate` does not know is free text for the title when it is a
/// bare word or a quoted phrase. A word that merely LOOKS like a token stays
/// refused: a `key:value` with an unknown key (`remind:any`) is a mistyped
/// predicate, and a leading `+`/`-`/`@` is a mistyped tag, flag or keyword. A
/// quoted phrase is taken at its word, colon and all. Every other refusal
/// (`status:nope`, `due.before:`) passes through untouched.
fn leaf(tok: &str, quoted: bool, now: Timestamp) -> Result<Expr, String> {
    if let Some(v) = tok.strip_prefix("title:") {
        if v.is_empty() {
            return Err("`title:` needs a value — e.g. title:review, or \
                 title:\"weekly planning\" when it contains a space"
                .to_string());
        }
        return Ok(Expr::Pred(Pred::Title(v.to_lowercase())));
    }
    if let Some(v) = tok.strip_prefix("status:") {
        if v.eq_ignore_ascii_case("any") || v.eq_ignore_ascii_case("all") {
            return Ok(Expr::Pred(Pred::AnyStatus));
        }
    }
    match predicate(tok, now) {
        Ok(p) => Ok(Expr::Pred(p)),
        Err(e) if e.starts_with(UNKNOWN_TOKEN) => {
            let looks_like_token =
                tok.starts_with(['+', '-', '@']) || (!quoted && tok.contains(':'));
            if tok.is_empty() || looks_like_token {
                Err(e)
            } else {
                Ok(Expr::Pred(Pred::Title(tok.to_lowercase())))
            }
        }
        Err(e) => Err(e),
    }
}

/// Resolve a date bound against `now`, or say why it is not a date.
///
/// It delegates to [`datetime::parse_when`] rather than restating what a date
/// may look like, which is the whole point: the bound accepts exactly what
/// `due:` accepts because it is the same parser, so the two cannot drift and
/// the tool cannot advertise a spelling its own filter rejects. That includes
/// the zone: a clock time in a bound is UTC, as it is in `due:` (D132).
///
/// The refusal is D27's rule applied to a value: a bound nobody can read is a
/// caller error, and a read path loses nothing by refusing it — the user
/// retypes. Matching nothing instead is a wrong answer shaped exactly like a
/// right one. `parse_when`'s message already names the offending value and
/// lists the accepted forms, so it is passed through rather than reworded.
fn bound(value: &str, prefix: &str, now: Timestamp) -> Result<Timestamp, String> {
    let resolved = datetime::parse_when(value, now)
        .map_err(|e| format!("`{prefix}:` needs a date — {}", e.message))?;
    // `parse_when` promises an RFC3339 `…Z` string, so this cannot fail;
    // it is an `Option` only because `parse_ts` is total for untrusted input.
    parse_ts(&resolved)
        .ok_or_else(|| format!("`{prefix}:{value}` resolved to an unreadable instant {resolved:?}"))
}

/// Whether a deadline at `due` has been missed at `now` — the ONE definition of
/// "overdue" every surface reads (D131): `report.summary`, `project.archive`'s
/// count, `list`, `agenda`, `show`, `why`, the write echoes, the HTML report
/// and the dashboard.
///
/// A deadline with a time is missed once that instant passes. A deadline typed
/// without one is stored as midnight UTC (`datetime.rs`, D53) and means "by the
/// end of that day", so it is missed only once its UTC day has ended. The
/// instant rule alone called a task due today overdue one second past midnight,
/// which the dashboard refused and bucketed by date instead; the date rule alone
/// kept a task due at 09:00 out of "overdue" until the next day, which `list`
/// refused. The two screens then disagreed about the same row. This rule is
/// what each of them was protecting.
///
/// `due.before:now` is a filter over a literal instant and is deliberately NOT
/// this: it still matches a date-only deadline from midnight. #148 aligned the
/// report with it for timed deadlines, where the two still agree.
pub fn overdue_at(due: Timestamp, now: Timestamp) -> bool {
    let tz = jiff::tz::TimeZone::UTC;
    let at = due.to_zoned(tz.clone());
    let t = at.time();
    if t.hour() == 0 && t.minute() == 0 && t.second() == 0 && t.subsec_nanosecond() == 0 {
        at.date() < now.to_zoned(tz).date()
    } else {
        due < now
    }
}

/// Whether an OPEN task with this `due` is overdue at `now`, by [`overdue_at`].
pub fn is_overdue(status_is_open: bool, due: Option<&str>, now: Timestamp) -> bool {
    status_is_open && due.and_then(parse_ts).is_some_and(|d| overdue_at(d, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grammar block, as the docs show it — the same string, by construction.
    fn grammar() -> &'static str {
        GRAMMAR
    }

    /// A fixed reference instant for tests that do not care about dates, so
    /// nothing in this module resolves a bound against the wall clock.
    fn anchor() -> Timestamp {
        "2026-07-19T12:00:00Z".parse().expect("anchor")
    }

    /// Parse a filter that the test asserts is valid.
    ///
    /// Every filter below is a hand-written literal, so a parse failure here
    /// means the test itself is wrong — worth panicking on rather than
    /// threading a Result through assertions about matching.
    fn parsed(s: &str) -> Filter {
        Filter::parse(s, anchor()).unwrap_or_else(|e| panic!("test filter {s:?} must parse: {e}"))
    }

    /// The refusal message for a filter the test asserts is invalid.
    ///
    /// The `Ok` arm panics rather than returning an `Option`, for the same
    /// reason `parsed` does: a filter written here to be refused that suddenly
    /// parses is a change in the grammar, and the test that named it should say
    /// so by name instead of quietly asserting nothing.
    fn refused(s: &str) -> String {
        match Filter::parse(s, anchor()) {
            Err(e) => e,
            Ok(_) => panic!("test filter {s:?} must be refused, but it parsed"),
        }
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

    /// D210: a bare word is a case-insensitive substring of the title; several
    /// bare words are ANDed, in any order.
    #[test]
    fn bare_words_are_title_terms_anded() {
        let f = parsed("weekly review");
        assert!(titled(&f, "Weekly planning REVIEW"));
        assert!(!titled(&f, "Weekly planning"));
        assert!(titled(&parsed("PLAN"), "weekly planning review"));
        // A term composes with the rest of the grammar.
        assert!(titled(&parsed("plan or status:done"), "a plan"));
        assert!(!titled(&parsed("plan -x"), "nothing"));
    }

    /// D210: a quoted phrase is ONE substring, spaces included; it may hold a
    /// colon, which an unquoted word may not.
    #[test]
    fn a_quoted_phrase_is_one_substring() {
        let f = parsed(r#""memory explorer""#);
        assert!(titled(&f, "The Memory Explorer screen"));
        assert!(!titled(&f, "explorer of memory"));
        assert!(titled(&parsed(r#""fix: it""#), "Fix: it now"));
    }

    /// D210: `title:` is the explicit spelling, quoted or not.
    #[test]
    fn the_title_key_matches_like_a_bare_word() {
        assert!(titled(&parsed("title:plan"), "A Plan"));
        assert!(titled(&parsed(r#"title:"a plan""#), "is A Plan"));
        assert!(!titled(&parsed(r#"title:"a plan""#), "plan a"));
        assert!(refused("title:").contains("needs a value"));
    }

    /// D210: matching is in memory, so `%` and `_` are plain characters. Pinned
    /// because the ruling named LIKE escaping and a future SQL push-down must
    /// not turn them into wildcards.
    #[test]
    fn percent_and_underscore_in_a_title_term_are_literal() {
        assert!(titled(&parsed("100%"), "reach 100% coverage"));
        assert!(!titled(&parsed("100%"), "reach 1000 coverage"));
        assert!(titled(&parsed("a_b"), "see a_b"));
        assert!(!titled(&parsed("a_b"), "see axb"));
    }

    /// Only a bare word becomes a title term. A word that is a malformed token
    /// of the grammar stays an error.
    #[test]
    fn malformed_tokens_are_still_refused() {
        for input in [
            "due.before:",
            "bogus:thing",
            "@nonsense",
            "--flag",
            "-",
            "+",
            "status:nope",
            r#""""#,
        ] {
            assert!(Filter::parse(input, anchor()).is_err(), "{input:?}");
        }
    }

    /// D210: `status:any` / `status:all` restrict nothing, and count as having
    /// named a status so the report and agenda defaults step aside.
    #[test]
    fn status_any_matches_every_status_and_overrides_defaults() {
        for word in ["status:any", "status:all", "status:ANY"] {
            let f = parsed(word);
            assert!(f.constrains_status(), "{word}");
            for s in Status::ALL {
                assert!(f.matches(&ctx_for(s)), "{word} {s:?}");
            }
        }
    }

    /// Evaluate `f` against a row that differs from `ctx_for` only in its title.
    fn titled(f: &Filter, title: &str) -> bool {
        f.matches(&MatchCtx {
            title,
            ..ctx_for(Status::Pending)
        })
    }

    fn ctx_for(status: Status) -> MatchCtx<'static> {
        MatchCtx {
            title: "",
            status,
            priority: None,
            project: None,
            tags: &[],
            due: None,
            completed: None,
            blocked: false,
        }
    }

    fn ctx_tagged(tags: &[String]) -> MatchCtx<'_> {
        MatchCtx {
            title: "",
            status: Status::Pending,
            priority: None,
            project: None,
            tags,
            due: None,
            completed: None,
            blocked: false,
        }
    }

    fn ctx_with_priority(priority: Option<Priority>) -> MatchCtx<'static> {
        MatchCtx {
            title: "",
            status: Status::Pending,
            priority,
            project: None,
            tags: &[],
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
            title: "",
            status: Status::Pending,
            priority: None,
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

    /// #229 item 5: `proj:` is a valid write-side sugar (`tasqx-cli`'s
    /// `sugar.rs`, and the manual documents it) but the read side refused it
    /// with `unknown filter token`, and the refusal's own "expected" list did
    /// not even mention it — a user who learns the alias from `add` or the
    /// manual hits a wall on `list`/`report`/`export`/`watch`, all of which
    /// share this one parser. Accepting it here, rather than dropping the
    /// write-side alias, is the cheaper of the two fixes the finding names.
    #[test]
    fn proj_is_accepted_as_an_alias_of_project_on_the_read_side_too() {
        let ctx = MatchCtx {
            title: "",
            status: Status::Pending,
            priority: None,
            project: Some("home"),
            tags: &[],
            due: None,
            completed: None,
            blocked: false,
        };
        assert!(parsed("proj:home").matches(&ctx));
        assert!(!parsed("proj:elsewhere").matches(&ctx));
    }

    /// #229 item 12: `or` with a missing operand is already refused
    /// (`+api or` -> `expected a filter term`), but `and` was silently
    /// SKIPPED as a no-op separator regardless of position — so a truncated
    /// `project:web and`, a leading `and +api`, or `+pr and or +review` (the
    /// `and`'s operand slot swallowed by the `or` that follows) all parsed
    /// and answered the WIDER set silently, exactly the failure mode this
    /// module refuses `or` to prevent.
    #[test]
    fn a_dangling_and_is_refused_exactly_like_a_dangling_or() {
        for bad in ["+pr and", "and +pr", "+pr and or +review", "and", "and and"] {
            let err = refused(bad);
            assert!(
                err.contains("expected a filter term"),
                "{bad:?} must be refused the same way a dangling `or` is: {err}"
            );
        }
        // The well-formed forms must still work exactly as before.
        let ctx = MatchCtx {
            title: "",
            status: Status::Pending,
            priority: None,
            project: None,
            tags: &["pr".into()],
            due: None,
            completed: None,
            blocked: false,
        };
        assert!(parsed("+pr and status:pending").matches(&ctx));
        assert!(parsed("+pr status:pending").matches(&ctx), "implicit and");
        assert!(!parsed("+pr and status:done").matches(&ctx));

        // The refusal's own "expected" list is generated from VALUE_PREFIXES
        // (`token_shapes_name_every_value_prefix`), so once `proj:` is a real
        // prefix it must be named there too — asserted directly, since a
        // filter that stops being refused cannot exercise `TOKEN_SHAPES` any
        // other way.
        assert!(
            TOKEN_SHAPES.contains("proj:"),
            "the refusal's expected-token list must name every accepted \
             spelling, `proj:` included: {TOKEN_SHAPES}"
        );
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
            title: "",
            status: Status::Pending,
            priority: None,
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
            title: "",
            due: Some("2026-07-16T23:59:59Z"),
            ..ctx
        };
        assert!(parsed(&format!("due.before:{bound}")).matches(&earlier));
        let later = MatchCtx {
            title: "",
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
            title: "",
            status: Status::Pending,
            priority: None,
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
            title: "",
            status: Status::Pending,
            priority: None,
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
            title: "",
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

    /// #187 — priority was first-class on write (`!high`, the `P` column, a
    /// `report priority` grouping axis) and unwritable in the filter DSL:
    /// `tasqx list priority:H` was `unknown filter token`. `priority:` now
    /// exists on the same terms `status:` does — a closed, compile-time
    /// vocabulary (D34) — and reuses [`Priority::parse`] so a caller cannot
    /// find a spelling the write side accepts and this predicate refuses.
    #[test]
    fn each_priority_value_selects_exactly_that_priority() {
        for want in Priority::ALL {
            let f = parsed(&format!("priority:{}", want.as_str()));
            for have in Priority::ALL {
                assert_eq!(
                    f.matches(&ctx_with_priority(Some(have))),
                    want == have,
                    "priority:{} vs a {have:?} row",
                    want.as_str()
                );
            }
            // A task carrying no priority satisfies no `priority:` bound — the
            // same rule an undated task already has for `due.before:`/
            // `due.after:` (see `instant_cmp`'s doc).
            assert!(
                !f.matches(&ctx_with_priority(None)),
                "priority:{} must not select a bare row",
                want.as_str()
            );
        }
        // The forgiving spellings `Priority::parse` accepts on write must work
        // here too, since a caller must not find one door narrower than the
        // other.
        assert!(parsed("priority:high").matches(&ctx_with_priority(Some(Priority::H))));
    }

    /// An unrecognised priority is refused naming the accepted set, on D34's
    /// terms: priority is closed and compile-time, so a typo must not be
    /// answered with the same silent empty table an open vocabulary earns.
    #[test]
    fn an_unrecognised_priority_value_is_refused_naming_the_accepted_set() {
        let err = refused("priority:X");
        assert!(err.contains("unknown priority"), "{err}");
        assert!(
            err.contains('H') && err.contains('M') && err.contains('L'),
            "{err}"
        );
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
                title: "",
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

    /// `VALUE_PREFIXES` is a hand-maintained mirror of the grammar, so a
    /// seventh `key:` predicate added to `GRAMMAR` alone would lose shell
    /// quoting on that key with no other symptom than a confusing "unknown
    /// filter token". The guard reads the grammar text rather than trusting the
    /// list, so the two cannot drift apart in silence.
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
            // seeing two of the four predicates there were then, and only the
            // floor below — `seen == 4` at the time — caught it.
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
                VALUE_PREFIXES.iter().any(|(p, _)| *p == key),
                "`{key}` takes an argument in GRAMMAR but is not in VALUE_PREFIXES, so a                  shell-quoted value would be re-split at its spaces"
            );
        }
        // Without this the guard passes by matching nothing if GRAMMAR is
        // reformatted — the failure mode every text-scanning guard has.
        assert_eq!(seen, 9, "expected nine `key:`-shaped predicates in GRAMMAR");
    }

    /// The refusal message must offer every token the grammar accepts.
    ///
    /// `TOKEN_SHAPES` was two hand-typed copies of one sentence, and a filter
    /// that accepts a token its own error does not list teaches the user the
    /// token does not exist — the read-side twin of the drift D30 rules against.
    /// Pinned to `VALUE_PREFIXES` rather than to a second list, so the check has
    /// nothing of its own to fall out of date.
    ///
    /// The list now has a SECOND consumer outside this crate — `tasqx-cli`'s
    /// shell completion builds its filter menu from it — which does not change
    /// what this guard checks but does change what it is worth: a prefix missing
    /// from the registry is no longer only an error message that under-lists, it
    /// is also a Tab press that silently offers nothing for that one predicate.
    #[test]
    fn token_shapes_name_every_value_prefix() {
        for (p, _) in VALUE_PREFIXES {
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

    /// [`KEYWORDS`] and [`GRAMMAR`] must name the same valueless predicates, in
    /// BOTH directions.
    ///
    /// The twin of [`value_prefixes_match_the_grammar`], and it exists for the
    /// same reason one level over: `KEYWORDS` is now the menu `tasqx-cli` offers
    /// for a bare partial word in a filter position, so a keyword added to the
    /// parser and not to the list is a predicate the tool accepts and never
    /// teaches, while one listed and not accepted is a Tab press that completes
    /// to an error.
    ///
    /// Both directions matter and only one of them is the obvious one. Scanning
    /// GRAMMAR for `"@…"` catches the addition; requiring each entry to PARSE
    /// catches the removal, which no amount of text scanning can — a keyword
    /// deleted from [`predicate`] leaves the grammar block, and this list, and
    /// the menu, all still saying it works.
    #[test]
    fn keywords_match_the_grammar() {
        let mut in_grammar: Vec<String> = Vec::new();
        for line in GRAMMAR.lines() {
            // `"@working"` and `"@blocked"` are the only `"@` shapes; the
            // alternative spellings on the blocked line (`"+blocked"`,
            // `"status:blocked"`) do not start with `@` and are deliberately not
            // in the list — see KEYWORDS for why one concept gets one entry.
            let mut rest = line;
            while let Some(at) = rest.find("\"@") {
                rest = &rest[at + 1..];
                let Some(end) = rest[1..].find('"') else {
                    break;
                };
                in_grammar.push(rest[..=end].to_string());
            }
        }
        assert_eq!(
            in_grammar, KEYWORDS,
            "GRAMMAR names {in_grammar:?} as valueless predicates and KEYWORDS \
             says {KEYWORDS:?}; one of them moved, and the completion menu reads \
             KEYWORDS"
        );
        for kw in KEYWORDS {
            Filter::parse(kw, anchor()).unwrap_or_else(|e| {
                panic!("KEYWORDS offers {kw:?} and the parser refuses it: {e}")
            });
        }
    }

    /// An operator joins terms and is not one, which is exactly what a consumer
    /// offering it in a menu has to know.
    ///
    /// Pinned behaviourally rather than by scanning [`GRAMMAR`], because the
    /// operators appear there inside EBNF repetition (`( "or" and_expr )*`) and
    /// a scan of that shape would be pinning the formatting. Renaming `or` to
    /// something else reddens this in the only way that matters: the spelling
    /// the menu offers stops joining two terms.
    #[test]
    fn operators_join_terms_rather_than_being_ones() {
        for op in OPERATORS {
            Filter::parse(&format!("+api {op} +infra"), anchor()).unwrap_or_else(|e| {
                panic!("OPERATORS offers {op:?} and it does not join two terms: {e}")
            });
            assert!(
                Filter::parse(op, anchor()).is_err(),
                "{op:?} parses as a term on its own, so it is a predicate and \
                 does not belong in OPERATORS"
            );
        }
    }

    /// Every value prefix refuses an empty value, so no token in the grammar
    /// parses into a constraint that matches nothing.
    ///
    /// Read out of [`VALUE_PREFIXES`] rather than listed, so a ninth prefix is
    /// covered the day it is declared. That is the whole point: `project:` was
    /// the one prefix that accepted its empty value, and it stayed that way
    /// through several rounds of work on this grammar because nothing compared
    /// the arms against each other. It exited 0 and matched nothing —
    /// `tasqx list project:` said "No tasks.", `tasqx export project:` wrote a
    /// well-formed backup containing none of them — while every sibling exited 2.
    ///
    /// The stubs a shell now offers are exactly these prefixes, which is how it
    /// was found and why the guard belongs here rather than in the CLI: the menu
    /// made an old silence reachable, it did not create it.
    #[test]
    fn no_value_prefix_accepts_an_empty_value() {
        let now = crate::clock::now();
        for (prefix, _) in VALUE_PREFIXES {
            assert!(
                Filter::parse(prefix, now).is_err(),
                "`{prefix}` parses with no value, so it is a token that names \
                 nothing and matches nothing at exit 0 — the shape every other \
                 prefix refuses, and the one a completion menu offers as a stub"
            );
        }
    }

    /// The round-trip accessor the CLI's completion gate is built on, including
    /// the case that makes it a VALUE check rather than a parse check.
    ///
    /// `+blocked` parses — as the derived blocked flag, not as a tag named
    /// `blocked` — so a completion that only asked "does this parse?" would
    /// offer it for such a tag and silently answer a different question at exit
    /// 0. That is the shape this accessor exists to make refusable.
    #[test]
    fn sole_value_hands_back_what_the_token_carried() {
        let value = |s: &str| {
            Filter::parse(s, anchor())
                .ok()
                .and_then(|f| f.sole_value().map(str::to_string))
        };
        assert_eq!(value("+api"), Some("api".to_string()));
        assert_eq!(value("-api"), Some("api".to_string()));
        assert_eq!(value("project:work"), Some("work".to_string()));
        assert_eq!(value("status:done"), Some("done".to_string()));
        // The trap: a tag literally named `blocked` cannot be spelled `+blocked`.
        assert_eq!(value("+blocked"), None);
        assert_eq!(value("@working"), None);
        // A date bound resolved its value away at parse time, by design.
        assert_eq!(value("due.before:tomorrow"), None);
        // Not one predicate at all, so there is no single value to hand back.
        assert_eq!(value("+api +infra"), None);
        assert_eq!(value("+api or +infra"), None);
        // The empty filter is `Pred::Always`, which carries nothing.
        assert_eq!(value(""), None);
    }

    /// D109's seam: every `project:`/`proj:` value in the tree, including
    /// both sides of an `or` and duplicates, is what the three engine call
    /// sites walk to validate against the live projects table.
    #[test]
    fn project_names_collects_every_project_value_in_the_tree() {
        let names = |s: &str| {
            Filter::parse(s, anchor())
                .unwrap_or_else(|e| panic!("{s:?} must parse: {e}"))
                .project_names()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(names("project:work"), vec!["work".to_string()]);
        assert_eq!(names("proj:work"), vec!["work".to_string()]);
        assert_eq!(
            names("project:work or project:home"),
            vec!["work".to_string(), "home".to_string()]
        );
        assert_eq!(
            names("project:work and project:work"),
            vec!["work".to_string(), "work".to_string()],
            "duplicates are not collapsed — the caller decides what to do with them"
        );
        // No `project:` predicate at all: nothing to validate.
        assert_eq!(names("status:done"), Vec::<String>::new());
        assert_eq!(names(""), Vec::<String>::new());
    }

    /// D171: only the empty string is unfiltered — a predicate that happens to
    /// match every row (e.g. a status every task has) still counts as a filter
    /// the caller wrote on purpose.
    #[test]
    fn is_unfiltered_is_true_only_for_the_empty_filter() {
        let parses = |s: &str| Filter::parse(s, anchor()).unwrap_or_else(|e| panic!("{s:?}: {e}"));
        assert!(parses("").is_unfiltered());
        assert!(!parses("status:pending").is_unfiltered());
        assert!(!parses("project:work").is_unfiltered());
        assert!(!parses("+tag").is_unfiltered());
    }

    /// Every vocabulary in the registry must be reachable, or the CLI's
    /// exhaustive match carries an arm for a case that cannot happen while some
    /// real prefix quietly shares another arm's answer.
    #[test]
    fn every_vocabulary_is_named_by_some_prefix() {
        for want in [
            Vocabulary::Tag,
            Vocabulary::Project,
            Vocabulary::Status,
            Vocabulary::Priority,
            Vocabulary::Date,
            Vocabulary::Text,
        ] {
            assert!(
                VALUE_PREFIXES.iter().any(|(_, v)| *v == want),
                "no prefix draws from {want:?}, so a consumer matching on it is \
                 writing an unreachable arm"
            );
        }
        // And each prefix really does take the vocabulary it claims: the value
        // is fed back through the parser, which is the only authority on it.
        for (prefix, vocabulary) in VALUE_PREFIXES {
            let sample = match vocabulary {
                Vocabulary::Tag | Vocabulary::Project => "sample",
                Vocabulary::Status => Status::ALL[0].as_str(),
                Vocabulary::Priority => Priority::ALL[0].as_str(),
                Vocabulary::Date => "tomorrow",
                Vocabulary::Text => "sample",
            };
            Filter::parse(&format!("{prefix}{sample}"), anchor()).unwrap_or_else(|e| {
                panic!("`{prefix}` claims {vocabulary:?} and refuses {sample:?}: {e}")
            });
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
        // tokens: the value's first word and a title term (D210).
        assert_eq!(go(&["project:Home Renovation"]), "project:Home Renovation");
    }

    /// The literal-quote spelling selects a spaced value on both a `key:` value
    /// and a tag. The shell-stripped spelling is no longer refused (D210: a
    /// bare word is a title term), so it reads as the value's first word AND a
    /// title term, and must NOT select the spaced row on the strength of the
    /// stripped words alone.
    #[test]
    fn a_spaced_value_needs_literal_quotes_and_the_stripped_form_is_a_title_term() {
        let tags = ["needs paint".to_string()];
        let ctx = MatchCtx {
            title: "",
            status: Status::Pending,
            priority: None,
            project: Some("Home Renovation"),
            tags: &tags,
            due: None,
            completed: None,
            blocked: false,
        };
        for literal in [r#"+"needs paint""#, r#"project:"Home Renovation""#] {
            assert!(parsed(literal).matches(&ctx), "{literal:?} must select");
        }
        for stripped in ["+needs paint", "project:Home Renovation"] {
            assert!(
                !parsed(stripped).matches(&ctx),
                "{stripped:?} is a one-word value plus a title term, not the spaced value"
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
                title: "",
                status: Status::Pending,
                priority: None,
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
                title: "",
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
            title: "",
            status: Status::Done,
            priority: None,
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
        // And the unquoted spelling must NOT select the row: it is project `a`
        // and title terms (D210), not the spaced name.
        assert!(!parsed("project:a (b) or c and status:done").matches(&ctx));
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
                title: "",
                status: Status::Pending,
                priority: None,
                project: Some(v),
                tags: &[],
                due: None,
                completed: None,
                blocked: false,
            };
            assert!(f.matches(&ctx), "quote({v:?}) did not round trip");
        }

        // The empty value is NOT in the list above and is asserted here instead,
        // moved rather than deleted when `project:` stopped accepting it.
        //
        // It never named a reachable project: `project.create` refuses an empty
        // name (`missing or empty required field: name`), so `project:""` asked
        // for something that cannot exist and answered "No tasks." at exit 0.
        // The round-trip property this test is about — that `quote` escapes a
        // value the parser reads back — is vacuous for a value no project can
        // hold, and keeping the case as a success pinned the one silent token in
        // the grammar as though it were intended.
        //
        // Asserted as a refusal so the case is still covered: if `project:`
        // starts accepting an empty value again, this reddens rather than
        // quietly passing.
        assert!(
            Filter::parse(&format!("project:{}", quote("")), anchor()).is_err(),
            "an empty project name cannot exist, so `project:\"\"` must be \
             refused rather than matching nothing at exit 0"
        );
    }

    /// `project:` with no value at all must not be refused as an "unknown
    /// filter token" — that message then lists `project:` itself among the
    /// tokens it claims not to recognise, a contradiction that leaves an
    /// agent with no repair that converges (re-sending `project:` fails
    /// identically). The real fault is the missing value, so the message
    /// must say that instead.
    #[test]
    fn bare_project_colon_names_the_missing_value_not_an_unknown_token() {
        let err = Filter::parse("project:", anchor()).unwrap_err();
        assert!(
            !err.contains("unknown filter token"),
            "the token is recognised (it's `project:`); the value is missing: {err:?}"
        );
        assert!(
            err.contains("project:") && err.to_lowercase().contains("needs a value"),
            "expected a message naming `project:` and that it needs a value, got {err:?}"
        );
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
                title: "",
                status: Status::Pending,
                priority: None,
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
            title: "",
            status: Status::Pending,
            priority: None,
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
            title: "",
            status: Status::Pending,
            priority: None,
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

    /// D131: a deadline with a time is late once that instant has passed; a
    /// deadline typed without one is stored as midnight UTC and is late only
    /// once its whole day has. The instant rule alone called a task due today
    /// overdue one second past midnight; the day rule alone kept a task due at
    /// 09:00 out of "overdue" until the next day.
    #[test]
    fn a_date_only_deadline_is_due_by_the_end_of_its_day() {
        let at = |s: &str| s.parse::<Timestamp>().expect("a real instant");
        let noon = at("2026-07-19T12:00:00Z");
        assert!(
            !is_overdue(true, Some("2026-07-19T00:00:00Z"), noon),
            "a date-only deadline today is not late at noon"
        );
        assert!(
            is_overdue(true, Some("2026-07-19T09:00:00Z"), noon),
            "a deadline at 09:00 today has passed by noon"
        );
        assert!(
            !is_overdue(true, Some("2026-07-19T17:00:00Z"), noon),
            "a deadline this evening has not"
        );
        assert!(
            is_overdue(true, Some("2026-07-18T00:00:00Z"), noon),
            "yesterday's date-only deadline is late"
        );
        assert!(
            is_overdue(
                true,
                Some("2026-07-19T00:00:00Z"),
                at("2026-07-20T00:00:00Z")
            ),
            "a date-only deadline is late from the first second of the next day"
        );
        assert!(
            !is_overdue(false, Some("2026-07-18T00:00:00Z"), noon),
            "a closed task is never overdue"
        );
    }
}
