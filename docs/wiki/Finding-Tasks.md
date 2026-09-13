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

A bare `tasqx` in a pipe or script does the same thing; on an interactive
terminal it opens the [dashboard](Dashboard-and-Live-View.md) instead.

## tasqx next

The "what now" button. Prints the single most urgent task that isn't blocked.

```console
tasqx next
```

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
  them.

## tasqx show

*Alias: `get`*

One task, in full: description, tags, annotations, dependencies, whether it's
blocked, and its revision number.

```console
tasqx show 42
```

## tasqx why

Every open task gets an urgency score, and the ordering of every list comes
from it. `why` shows the arithmetic instead of asking you to trust it:

```console
$ tasqx why 42
#42  Ship the release notes

  priority   H                 6.0
  deadline   due Fri           7.5
  age        created today     0.0
  urgency                     13.5
```

Each row names the input in the words the rest of the terminal uses — the
priority letter, how the deadline reads on a list, how old the task is — and
the last row is the score every list sorts by.

Urgency is recomputed on every read, so the deadline row — and the total it
feeds — climb as the due date approaches. The scores in any capture, including
this one, are an illustration; the rows are the contract.

If tasqx ranks something surprisingly high or low, this is where the answer
is.

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

Combine with `and`, `or` and parentheses:

```console
tasqx list "project:work and (+api or +ui)"
```

**Values with spaces need quotes that actually reach tasqx** — so wrap the
whole thing in single quotes to protect it from your shell:

```console
tasqx list 'project:"Home Renovation"'
```

tasqx never guesses where a quoted value was supposed to end. If the shell
eats your quotes, the filter is refused with the correct spelling in the error
message — better than silently returning the wrong rows.
