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

use crate::util::parse_ts;

/// A single leaf predicate.
#[derive(Debug, Clone, PartialEq)]
pub enum Pred {
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
    pub status: &'a str,
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
        Pred::Status(s) => ctx.status == s,
        Pred::Project(pr) => ctx.project == Some(pr.as_str()),
        Pred::TagInclude(t) => ctx.tags.iter().any(|x| x == t),
        Pred::TagExclude(t) => !ctx.tags.iter().any(|x| x == t),
        Pred::Working => (ctx.status == "pending" || ctx.status == "active") && !ctx.blocked,
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
