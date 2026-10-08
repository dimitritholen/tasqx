# Dates, Reminders and Recurrence

tasqx reads dates the way you'd say them, and four different date fields let
you say four different things about *when*.

## Writing a date

Anywhere a date is expected — `due:`, `--scheduled`, `wait:`, `remind:` — you
can write:

```text
tomorrow            friday              friday 17:00
in 3 days           eom                 at 6pm
-1d                 2026-09-15          2026-09-15T17:00
```

`eom` is end of month. `-1d` is yesterday. A bare weekday is the next one
after today, so on a Wednesday `friday` is in two days. `next friday` is that
weekday in the following Monday-to-Sunday week (nine days out), and
`next week` is that week's Monday; `this friday` and `last friday` are
refused rather than guessed. A bare time that has already passed
today rolls to tomorrow. Full RFC3339 works when you want to be exact.

Everything is UTC. A bare date is midnight UTC, and a clock time is a UTC
clock — `due:17:00` is 17:00 UTC wherever you type it, and every screen prints
it back as 17:00. An offset you write yourself (`+02:00`, or a trailing `Z`)
is honoured as written.

| Form | What you can write |
|---|---|
| Relative days | `today`, `tomorrow`, `yesterday`, `now`, `eom` (end of month), `eow` (end of week) |
| Weekday names | `monday`..`sunday` or `mon`..`sun` — the next one after today |
| Counted spans | `in 1 day`, `"in 3 days"`, `in 2 weeks`, `in 3 months` — days, weeks and months only; `in 2 hours` is rejected |
| Signed offsets | `-1d`, `+3d`, `3d` (no sign means the future) |
| Times | `17:00`, `5pm`, attached to a day with a space — `"tomorrow 17:00"`, `"friday 9am"` — or a full instant — `"2026-09-09 17:00"`, `2026-09-09T17:00:00+02:00` (an explicit offset is converted to UTC on the way in) |

## The four date fields

| Field | What it says | Effect |
|---|---|---|
| `due` | When it must be finished | Drives urgency up as it approaches; overdue tasks stay loudly visible. A deadline with a time is overdue once it passes; a date without a time is due by the end of that day (UTC) |
| `scheduled` | When you plan to start | A future value parks the task in the backlog until then |
| `wait` | Hide it until this moment | Same parking, different intent: "not my problem yet" |
| `remind` | When to nudge you | Fires a reminder; see below |

`scheduled` and `wait` both keep a task out of your working set until their
moment arrives — the difference is what you *mean*, and
[`tasqx agenda`](Finding-Tasks.md#tasqx-agenda) places tasks on `scheduled`
(or `due`), never on `wait`.

## Reminders

`remind:` takes an offset from the due date, or an absolute time:

| Command | What it does |
|---|---|
| `tasqx add Call the bank due:"friday 9am" remind:-30m` | 30 minutes before |
| `tasqx add Water plants remind:"friday 8am"` | At an exact time |

The offset stays *symbolic*: move the due date and the reminder moves with it.

Reminders fire only while [`tasqx daemon`](AI-Agents-and-Automation.md#tasqx-daemon)
is running, and they're quiet by default — the OS toast notification lives
behind an off-by-default build feature (`notify-os`).

## Recurrence

`repeat:` makes a task come back:

```console
tasqx add Water plants repeat:"every 3 days"
tasqx add Standup repeat:"weekly on mon,wed,fri"
tasqx add Pay rent due:"2026-09-01" repeat:"monthly on day 1"
tasqx add Team retro repeat:"monthly on the 2nd tuesday"
```

- Completing a recurring task spawns the next occurrence; the answer shows it.
  The occurrence keeps the task's first note (its description) and its checks,
  reset to open, and its Repeats row names the one it came from
  (`every week from #604`). `tasqx brief` on it adds a "Last time" section
  quoting what the previous occurrence delivered.
- Missed occurrences don't pile up — they collapse into a single next one.
- `every N months` can drift across short months; anchor with
  `monthly on day 15` when the day of month matters.
- Stop a recurrence with `tasqx modify <ref> --clear recurrence`.
