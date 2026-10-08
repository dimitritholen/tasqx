# The filter grammar

One small grammar, used everywhere a query is taken: `list`, `report`, `export`,
`watch`, `task.list`, and the MCP tools. Learn it once.

## The grammar

<!-- generated: filter-grammar -->

## Predicates

| Predicate | Matches |
|---|---|
| `+api` | Tasks tagged `api`. |
| `-api` | Tasks *not* tagged `api`. |
| `project:work.tasqx` | Exact project match. |
| `status:pending` | Exact status: `backlog`, `pending`, `active`, `done`, `cancelled`. |
| `status:any` | Every status, `done` and `cancelled` included; also spelled `status:all`. Overrides a default that hides closed tasks. |
| `weekly review` | Free text: a bare word is a case-insensitive substring of the title, and several words must all appear. Quote a phrase to match it as one — `"memory explorer"`. `title:review` and `title:"weekly planning"` are the explicit spelling; `%` and `_` are ordinary characters. |
| `@working` | Status pending or active, *and* not blocked. The default filter. |
| `@blocked` | Open, with at least one dependency that is not yet done or cancelled. Also spelled `+blocked` or `status:blocked`. |
| `due.before:<date>` | Due strictly before that instant. Takes any date `due:` takes — `tomorrow`, `friday`, `2026-07-25`, `eom`, `"in 3 days"`, or a full RFC3339 instant. |
| `due.after:<date>` | Due strictly after that instant. Same date grammar. |

## Values with spaces

A space separates predicates, so a project whose name contains one must be
double-quoted — `project:"Home Renovation"`. (A tag never contains one: tags are
stored lowercased and a spaced tag is refused, so `+API` matches `api`.) The
rule is the shell's: inside quotes, spaces and parentheses are ordinary
characters and `and`/`or` are ordinary words, so a project named `a (b)` no
longer breaks the grouping. Write `\"` for a literal quote and `\\` for a
literal backslash. Quoting changes where a predicate *ends*, not what it means:
`"project:x"` is still a project match. The quotes must REACH tasqx: on a read
path the argument boundary is not enough, because the reader does not guess that
a space belongs inside a value rather than between two predicates. Protect them
from your shell — `'project:"Home Renovation"'`.

One scanner, not a filter dialect: `tasqx add` and `tasqx modify` split their
inline sugar with the same code the filter uses. The two sides are not
symmetric, though, and the difference is deliberate. The *write* side also
honours the argument boundary your shell drew, so `tasqx add "paint"
project:Home Renovation` files the task; the *read* side refuses the same words,
because there `project:Home Renovation` is equally a spaced name and a project
match plus a stray token, and guessing would answer with the wrong rows at exit
0. A refused read costs a retype; a wrong one is unfalsifiable. So the spelling
that works on both sides is the quoted one, and it is the one to learn. The
value that needs the escaped form on both sides is a name containing a quote —
`project:"My \"Big\" Project"` — because an argument carrying a literal quote is
read by the scanner rather than taken whole.

```console
tasqx list project:"Home Renovation" +paint
```

## Combining

A space is an implicit `and`. `or` and parentheses are explicit, and both
keywords are case-insensitive.

```console
$ tasqx list "(+api or +ops) and status:pending"
(+api or +ops) and status:pending   2 tasks · 1 overdue

  ID          URG  TASK                         PROJECT     DUE        TAGS
   1  H ▄▄▄▄ 15.7  Ship the v1 JSON API freeze  work.tasqx  Fri        +api +release
   3  - ▄▄▄▄ 12.0  Renew the TLS cert           work.tasqx  yesterday  +ops
```

## Two behaviours worth knowing

**Dates compare as instants, not strings.** `due.before:` and `due.after:` parse
both sides to timestamps and compare those. `2026-07-17T00:00:00Z` and
`2026-07-17T02:00:00+02:00` are the same instant, and the filter knows it — a
lexicographic comparison would not. A relative bound resolves once, when the
filter is parsed, so every row in one query is compared against the same
`tomorrow`. A date the parser cannot read is refused by name rather than quietly
matching nothing.

> **Careful** **Unknown tokens are rejected.** A token the grammar does not
> recognise is an error naming the token, not a term that matches everything.
> So `tasqx list "priority:H"` fails — `priority:` is not a predicate —
> instead of silently listing every task. The same goes for a dangling `or` or
> an unclosed `(`. Values follow the same rule when the set of them is
> *closed*: `status:` and a date bound have a fixed list of accepted values,
> so `status:pendign` is an error naming the value and the five statuses, not
> an empty table. A `project:` or a tag is different on purpose — those names
> are made at runtime, so an unknown one simply matches no row.

## Where the grammar stops

On purpose, and permanently: no arithmetic, no computed expressions, no
subqueries. A filter language that grows those becomes a query language nobody
can predict. For anything beyond this, [export](data.md) to JSON and use a real
tool — `jq`, a script, whatever you like. The store is yours.
