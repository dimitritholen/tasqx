# Finding Tasks

Five commands answer "what's on my plate?", each from a different angle —
plus a small filter language they all share.

## tasqx list

*Aliases: `ls`, `l`*

The task table. With no filter it shows your **working set**: open tasks you
can actually act on right now (blocked and hidden-until-later tasks are left
out).

| Command | What it does |
|---|---|
| `tasqx list` | The working set |
| `tasqx list project:work +api` | Narrowed |
| `tasqx list due.before:friday` | Deadline pressure only |

A list shows at most 100 rows. When more match, the head says so (`100 of 192
tasks`) and a last line gives the command for the rest: `--offset 100` for the
next page, `--limit 192` for all of them (`--limit` tops out at 10,000).
`tasqx agenda` reads every match, so it is never cut.

A bare `tasqx` in a pipe or script does the same thing; on an interactive
terminal it opens the [dashboard](Dashboard-and-Live-View.md) instead.

## tasqx next

The "what now" button. Prints the single most urgent task that isn't blocked.

```console
tasqx next
```

`--card` prints that task as the same 72-column box-drawn card `tasqx show
--card` draws (D146), meant for pasting into a document rather than reading
at a terminal; `--ascii` draws its borders with `+ - |`.

## tasqx agenda

*Aliases: `ag`, `cal`*

What's coming up, ordered by time and grouped by day — the calendar view of
your tasks.

| Command | What it does |
|---|---|
| `tasqx agenda` | The next 14 days |
| `tasqx agenda --days 3` | Just the next few |

- Each task appears on the *earlier* of its due date and its scheduled date —
  the first day it asks something of you — and the WHEN column says which of
  the two that was.
- Overdue tasks always show, no matter the window.
- Tasks it can't place (no date, or past the horizon) are counted under the
  table rather than silently dropped, with the exact command that would reach
  them: the `--days` that reaches the furthest one, or `tasqx list` when it is
  further out than `--days` goes.
- Days are UTC days: a bare date is midnight UTC, so a day groups the same way
  the store holds it.

## tasqx show

*Alias: `get`*

One task, in full: description, tags, annotations, dependencies, whether it's
blocked, and its revision number.

```console
tasqx show 42
```

`--card` prints the task as a fixed 72-column box-drawn card instead of the
usual screen — meant for pasting into a document, a chat reply or a pull
request rather than reading at a terminal. `--ascii` draws its borders with
`+ - |` for destinations that mangle box-drawing characters.

```console
tasqx show 42 --card
tasqx show 42 --card --ascii
```

When an annotation was removed with
[`tasqx unannotate`](Adding-and-Editing-Tasks.md#tasqx-unannotate), `show`
prints one line per removed annotation under the history — a tombstone naming
its id and when it was removed — and the box card's Notes row appends
", N removed".

## tasqx why

Every open task gets an urgency score, and the ordering of every list comes
from it. `why` shows the arithmetic instead of asking you to trust it:

<!-- fixture: why -->
```console
$ tasqx why 51
#51  Write the migration guide for SDK 3.0

  priority   M           3.9
  deadline   due Mon     8.0
  age        30 days     0.3
  urgency               12.2
```

Each row names the input in the words the rest of the terminal uses — the
priority letter, how the deadline reads on a list, how old the task is — and
the last row is the score every list sorts by.

Urgency is recomputed on every read, so the deadline row — and the total it
feeds — climb as the due date approaches. The scores in any capture, including
this one, are an illustration; the rows are the contract.

If tasqx ranks something surprisingly high or low, this is where the answer
is.

`--card` prints the D146 box card first, with this same arithmetic underneath
it instead of the plain text header above; `--ascii` draws the card's borders
with `+ - |`.

## The filter language

Everything that lists tasks (`list`, `agenda`, `pick`, `watch`, `report`,
`export`) takes the same filter expressions:

| Filter | Matches |
|---|---|
| `project:work` | In a project |
| `status:pending` | By status |
| `+api` | Has a tag |
| `-api` | Does NOT have a tag |
| `due.before:friday` | Due before a date |
| `due.after:monday` | Due after a date |
| `weekly review` | Title contains every word, any case |
| `'"memory explorer"'` | Title contains the phrase |
| `title:review` | The same, spelled as a key |
| `status:any` | Every status, done and cancelled too (`status:all` is the same) |
| `@working` | Pending or active and not blocked — what a bare `list` shows |
| `@blocked` | Open, with a dependency that is not yet done or cancelled |

A bare word is a title search, so `tasqx list weekly review` finds "Weekly
planning review". A word that looks like a token but is not one — `remind:x`,
`due.before:` with no value — is still refused. `%` and `_` in a word are plain
characters.

Combine with `and`, `or` and parentheses:

```console
tasqx list "project:work and (+api or +ui)"
```

**Values with spaces need quotes that actually reach tasqx** — so wrap the
whole thing in single quotes (or backslash-escape it) to protect it from your
shell. Inside the quotes, parentheses and the `and`/`or` keywords are ordinary
characters too, as in a shell:

```console
tasqx list 'project:"Home Renovation"'
```

tasqx never guesses where a quoted value was supposed to end. If the shell
eats your quotes, `project:Home Renovation` is the project `Home` plus the title
word `Renovation`; a project that does not exist is refused by name, which is
the signal to quote. `+needs paint` is the tag `needs` plus the word `paint`;
when that matches nothing, `list` prints a `hint:` line naming `+"needs paint"`.
The same rule is what lets you pass a whole expression as one argument:
`tasqx list "+api or +web"` is the expression.

`report` takes one axis, as its first word, and refuses `report project priority`;
write `title:priority` to match that word in titles.

`add` and `modify` split their sugar with the same scanner, but the write side
also honours the argument boundary your shell drew, so an unquoted multi-word
value is not refused there: it either `not_found`s (no project named by the
leading word) or — once a project happens to be named exactly that leading
word — files the task there and welds the remainder onto the title.
`tasqx add "paint" project:Home Renovation` becomes project `Home`, title
"paint Renovation". Use the quoted spelling on both sides and this cannot
happen: `tasqx add "paint" project:"Home Renovation"`.

Write `\"` for a literal quote and `\\` for a literal backslash — a name
holding a quote needs that form on both sides:
`tasqx add "paint" project:"My \"Big\" Project"`.

`due` is compared as an instant, not a calendar day.
