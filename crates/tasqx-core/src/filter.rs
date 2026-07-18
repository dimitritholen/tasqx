//! Filter DSL (DESIGN.md §5, §12-D8).
//!
//! Grammar (recursive-descent; case-insensitive keywords `and`/`or`):
//!
//! ```text
//!   filter     := or_expr
//!   or_expr    := and_expr ( "or" and_expr )*
//!   and_expr   := term ( "and"? term )*        # juxtaposition = implicit AND
//!   term       := "(" or_expr ")" | predicate
//!   predicate  := "+" WORD                      # require tag
//!               | "-" WORD                      # exclude tag
//!               | "@working"                    # status in {pending,active} AND not blocked
//!               | "@blocked" | "+blocked" | "status:blocked"   # the blocked flag
//!               | "project:" VALUE
//!               | "status:" VALUE
//!               | "due.before:" RFC3339
//!               | "due.after:"  RFC3339
//! ```
//!
//! Boolean `or` and parentheses grouping let you write
//! `(+api or +infra) and due.before:2026-07-17T00:00:00Z`. Implicit AND by
//! space is preserved. Per §12-D8 the grammar deliberately stops here: no
//! arithmetic, computed expressions, or subqueries.
//!
//! Evaluation is done in Rust against each candidate row (not compiled to SQL)
//! so that: (a) `due.before/after` compare as **instants** — RFC3339 strings are
//! parsed to timestamps and compared, never lexicographically (offsets differ);
//! and (b) the boolean/grouping tree is trivial to evaluate. Unknown tokens are
//! ignored (treated as the always-true term) to keep the surface forgiving.

use crate::types::Status;
use crate::util::parse_ts;

/// A single leaf predicate.
#[derive(Debug, Clone, PartialEq)]
pub enum Pred {
    /// `status:VALUE`. Deliberately still a `String`, not a [`Status`]: this is
    /// raw user input from the DSL and may be a typo (`status:pendign`). Parsing
    /// at match time — where an unrecognised name matches nothing — keeps the
    /// "unknown tokens are forgiving, unknown *values* just don't match" split
    /// that the rest of the grammar already has.
    Status(String),
    Project(String),
    TagInclude(String),
    TagExclude(String),
    DueBefore(String),
    DueAfter(String),
    /// `@working`: pending|active AND not blocked.
    Working,
    /// The blocked flag: a task with >=1 dependency not yet `done` (DESIGN §3).
    Blocked,
    /// Unknown/ignored token — always matches (forgiving).
    Always,
}

/// The parsed filter expression tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Pred(Pred),
}

/// The fields a predicate is evaluated against.
pub struct MatchCtx<'a> {
    /// The row's status as the typed enum, never a bare string. While this was
    /// `&str` the whole module compared status with `==` against hand-typed
    /// literals, so `Status` could not participate in its own matching rules and
    /// a renamed or added variant went unnoticed here.
    pub status: Status,
    pub project: Option<&'a str>,
    pub tags: &'a [String],
    pub due: Option<&'a str>,
    pub blocked: bool,
}

/// A parsed filter. `Filter::parse` never fails; `matches` evaluates it.
#[derive(Debug, Clone)]
pub struct Filter {
    root: Expr,
}

impl Filter {
    /// Parse a filter string. An empty string matches everything.
    pub fn parse(input: &str) -> Filter {
        let toks = tokenize(input);
        if toks.is_empty() {
            return Filter { root: Expr::Pred(Pred::Always) };
        }
        let mut p = Parser { toks, pos: 0 };
        let root = p.parse_or();
        Filter { root }
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
        // An unparseable value (`status:bogus`) yields `None`, which never
        // equals `Some(..)` — so it matches nothing, exactly as the previous
        // string comparison against a always-valid `ctx.status` did.
        Pred::Status(s) => Status::parse(s) == Some(ctx.status),
        Pred::Project(pr) => ctx.project == Some(pr.as_str()),
        Pred::TagInclude(t) => ctx.tags.iter().any(|x| x == t),
        Pred::TagExclude(t) => !ctx.tags.iter().any(|x| x == t),
        Pred::Working => matches!(ctx.status, Status::Pending | Status::Active) && !ctx.blocked,
        Pred::Blocked => ctx.blocked,
        Pred::DueBefore(bound) => instant_cmp(ctx.due, bound, true),
        Pred::DueAfter(bound) => instant_cmp(ctx.due, bound, false),
    }
}

/// Compare a task's `due` against a bound as instants. `before=true` => due <
/// bound; else due > bound. Any missing/unparseable side => no match.
fn instant_cmp(due: Option<&str>, bound: &str, before: bool) -> bool {
    let (Some(d), Some(b)) = (due.and_then(parse_ts), parse_ts(bound)) else {
        return false;
    };
    if before {
        d < b
    } else {
        d > b
    }
}

// ---- tokenizer --------------------------------------------------------------

/// Split into tokens, breaking out parentheses as their own tokens even when
/// glued to a word (`(+api` => `(`, `+api`).
fn tokenize(input: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    for c in input.chars() {
        match c {
            '(' | ')' => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                toks.push(c.to_string());
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    toks
}

// ---- parser -----------------------------------------------------------------

struct Parser {
    toks: Vec<String>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.pos).map(String::as_str)
    }

    fn is_kw(&self, kw: &str) -> bool {
        self.peek().map(|t| t.eq_ignore_ascii_case(kw)).unwrap_or(false)
    }

    fn parse_or(&mut self) -> Expr {
        let mut parts = vec![self.parse_and()];
        while self.is_kw("or") {
            self.pos += 1; // consume 'or'
            parts.push(self.parse_and());
        }
        if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            Expr::Or(parts)
        }
    }

    fn parse_and(&mut self) -> Expr {
        let mut parts = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some(")") => break,
                Some(t) if t.eq_ignore_ascii_case("or") => break,
                Some(t) if t.eq_ignore_ascii_case("and") => {
                    self.pos += 1; // explicit AND separator, skip
                    continue;
                }
                _ => parts.push(self.parse_term()),
            }
        }
        if parts.is_empty() {
            Expr::Pred(Pred::Always)
        } else if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            Expr::And(parts)
        }
    }

    fn parse_term(&mut self) -> Expr {
        if self.peek() == Some("(") {
            self.pos += 1; // consume '('
            let inner = self.parse_or();
            if self.peek() == Some(")") {
                self.pos += 1; // consume ')'
            }
            return inner;
        }
        let tok = self.toks[self.pos].clone();
        self.pos += 1;
        Expr::Pred(predicate(&tok))
    }
}

/// Map a single token to a leaf predicate.
fn predicate(tok: &str) -> Pred {
    if tok == "@working" {
        return Pred::Working;
    }
    if tok == "@blocked" || tok == "+blocked" || tok == "status:blocked" {
        return Pred::Blocked;
    }
    if let Some(rest) = tok.strip_prefix('+') {
        if !rest.is_empty() {
            return Pred::TagInclude(rest.to_string());
        }
    }
    if let Some(rest) = tok.strip_prefix('-') {
        if !rest.is_empty() {
            return Pred::TagExclude(rest.to_string());
        }
    }
    if let Some(v) = tok.strip_prefix("project:") {
        return Pred::Project(v.to_string());
    }
    if let Some(v) = tok.strip_prefix("status:") {
        return Pred::Status(v.to_string());
    }
    if let Some(v) = tok.strip_prefix("due.before:") {
        return Pred::DueBefore(v.to_string());
    }
    if let Some(v) = tok.strip_prefix("due.after:") {
        return Pred::DueAfter(v.to_string());
    }
    Pred::Always // unknown token — ignored
}

#[cfg(test)]
mod tests {
    use super::*;

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
                Filter::parse(input).constrains_status(),
                want,
                "constrains_status({input:?})"
            );
        }
    }

    fn ctx_for(status: Status) -> MatchCtx<'static> {
        MatchCtx { status, project: None, tags: &[], due: None, blocked: false }
    }

    fn ctx_tagged(tags: &[String]) -> MatchCtx<'_> {
        MatchCtx { status: Status::Pending, project: None, tags, due: None, blocked: false }
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
            blocked: false,
        };
        assert!(
            !Filter::parse("(project:home or +api) and status:done").matches(&ctx),
            "the group must bind before `and` — this row was filtered out"
        );
        // The same tokens without parentheses DO match, which is what proves the
        // assertion above is about grouping and not about the predicates.
        assert!(Filter::parse("project:home or +api and status:done").matches(&ctx));
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
            blocked: false,
        };
        assert!(!Filter::parse(&format!("due.before:{bound}")).matches(&ctx), "before is strict");
        assert!(!Filter::parse(&format!("due.after:{bound}")).matches(&ctx), "after is strict");
        // One second either side still resolves the way the names promise.
        let earlier = MatchCtx { due: Some("2026-07-16T23:59:59Z"), ..ctx };
        assert!(Filter::parse(&format!("due.before:{bound}")).matches(&earlier));
        let later = MatchCtx { due: Some("2026-07-17T00:00:01Z"), ..ctx };
        assert!(Filter::parse(&format!("due.after:{bound}")).matches(&later));
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

        let f = Filter::parse("-infra");
        assert!(!f.matches(&ctx_tagged(&has_infra)), "-infra must hide a task tagged infra");
        // Load-bearing: a task carrying some *other* tag is what separates a
        // correct exclusion from one whose comparison has been inverted.
        assert!(f.matches(&ctx_tagged(&other_tag)), "-infra must keep a task tagged only docs");
        assert!(f.matches(&ctx_tagged(&no_tags)), "-infra must keep an untagged task");

        // The include/exclude pair must stay exact opposites on the same rows.
        let inc = Filter::parse("+infra");
        for tags in [&has_infra, &other_tag, &no_tags] {
            assert_ne!(
                inc.matches(&ctx_tagged(tags)),
                f.matches(&ctx_tagged(tags)),
                "+infra and -infra disagree on {tags:?}"
            );
        }
    }

    /// `Pred::Status` holds raw DSL text but is now compared by *parsing* it
    /// against a typed `Status`. The regression that buys: a value the parser
    /// does not recognise must keep matching nothing. If `Status::parse` were
    /// ever made lenient (trimming, case-folding, aliasing `canceled`), or if the
    /// comparison fell back to "unparseable means match anything", then
    /// `status:bogus` would start selecting rows — and a filter that silently
    /// widens is far worse than one that returns nothing, because the caller
    /// gets a plausible-looking answer to a question they did not ask.
    #[test]
    fn an_unrecognised_status_value_matches_no_row() {
        // All whitespace-free: a value containing a space is split by the
        // tokenizer long before status parsing sees it, so those inputs would
        // exercise the tokenizer rather than this rule.
        for bogus in ["bogus", "canceled", "PENDING", "Done", "pending2", ""] {
            let f = Filter::parse(&format!("status:{bogus}"));
            for status in Status::ALL {
                assert!(
                    !f.matches(&ctx_for(status)),
                    "status:{bogus:?} must match nothing, but matched {status:?}"
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
            let f = Filter::parse(&format!("status:{}", want.as_str()));
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
        let f = Filter::parse("@working");
        for status in Status::ALL {
            let want = matches!(status, Status::Pending | Status::Active);
            assert_eq!(f.matches(&ctx_for(status)), want, "@working vs {status:?}");

            let blocked = MatchCtx { blocked: true, ..ctx_for(status) };
            assert!(!f.matches(&blocked), "@working must exclude blocked {status:?}");
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
                !Filter::parse(input).constrains_status(),
                "{input:?} must not suppress the exclude-cancelled default"
            );
        }
    }
}
