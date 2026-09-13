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

`tasqx list` is the working set: pending and active tasks that are not blocked,
hottest first. (A bare `tasqx` opens the dashboard on a terminal, and prints this
same table everywhere else — in a pipe, a redirect, or under `--json`.) Urgency is computed from priority, due proximity and age — and
`tasqx why 42` shows the arithmetic, so the ordering is never a mystery.

Things you cannot act on yet stay out of sight until they become actionable:

| Command | What it does |
|---|---|
| `tasqx add "Book the campsite wait:2026-08-01"` | Backlog until August 1 |
| `tasqx add "Prep the demo scheduled:monday"` | Surfaces on Monday |

## Defer, don't delete

| Command | What it does |
|---|---|
| `tasqx modify 42 due:monday` | Push it out, reminder moves along |
| `tasqx cancel 17` | Reversible, kept in history |
| `tasqx reopen 17` | Changed your mind |

`cancel` is not delete: the task stays in the event log and out of your reports'
way. There is no destructive path in daily use.

## The five-minute weekly review

| Command | What it does |
|---|---|
| `tasqx list "status:done completed.after:-7d"` | What got finished |
| `tasqx report status` | Open vs done; cancelled stays out |
| `tasqx chart heatmap` | Completion density, calendar-style |
