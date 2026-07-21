# Personal task management with tasqx

The capture-everything, trust-the-list workflow. The design goal: adding a task is
one line with no ceremony, and the list you look at shows only what is actionable
now.

## Capture without friction

Everything after `add` is the title, except the parts tasqx recognises as structure:

```console
tasqx add Buy milk
tasqx add "Renew passport due:friday !high +errands"
tasqx add "Water the plants" repeat:"every 3 days"
tasqx add "Call the dentist due:tomorrow remind:-1h"
```

Dates take natural language — `tomorrow`, `friday 17:00`, `in 3 days`, `eom` — and
reminders anchor to the due date, so when the due date moves, the reminder moves
with it.

## Look only at what matters

Bare `tasqx` is the working set: pending and active tasks that are not blocked,
hottest first. Urgency is computed from priority, due proximity and age — and
`tasqx why 42` shows the arithmetic, so the ordering is never a mystery.

Things you cannot act on yet stay out of sight until they become actionable:

```console
tasqx add "Book the campsite wait:2026-08-01"   # backlog until August 1
tasqx add "Prep the demo scheduled:monday"      # surfaces on Monday
```

## Defer, don't delete

```console
tasqx modify 42 due:monday        # push it out, reminder moves along
tasqx cancel 17                   # reversible, kept in history
tasqx reopen 17                   # changed your mind
```

`cancel` is not delete: the task stays in the event log and out of your reports'
way. There is no destructive path in daily use.

## The five-minute weekly review

```console
tasqx list "status:done completed.after:-7d"    # what got finished
tasqx report status                             # open vs done vs cancelled
tasqx chart heatmap                             # completion density, calendar-style
```
