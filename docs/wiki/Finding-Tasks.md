# Finding Tasks

Five commands answer "what's on my plate?", each from a different angle —
plus a small filter language they all share.

## tasqx list

*Aliases: `ls`, `l`*

The task table. With no filter it shows your **working set**: open tasks you
can actually act on right now (blocked and hidden-until-later tasks are left
out).

```console
tasqx list                              # the working set
tasqx list project:work +api            # narrowed
tasqx list due.before:friday            # deadline pressure only
```

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

```console
tasqx agenda              # the next 14 days
tasqx agenda --days 3     # just the next few
```

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
Why #42 has urgency 11.4
  priority         6.00
  due_proximity    5.40
  age              0.00
  = total          11.4
```

If tasqx ranks something surprisingly high or low, this is where the answer
is.

## The filter language

Everything that lists tasks (`list`, `agenda`, `pick`, `watch`, `report`,
`export`) takes the same filter expressions:

```console
project:work            # in a project
status:pending          # by status
+api                    # has a tag
-api                    # does NOT have a tag
due.before:friday       # due before a date
due.after:monday        # due after a date
```

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
