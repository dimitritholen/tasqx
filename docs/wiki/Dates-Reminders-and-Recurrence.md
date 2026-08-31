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

`eom` is end of month. `-1d` is yesterday. A bare time that has already passed
today rolls to tomorrow. Full RFC3339 works when you want to be exact.

## The four date fields

| Field | What it says | Effect |
|---|---|---|
| `due` | When it must be finished | Drives urgency up as it approaches; overdue tasks stay loudly visible |
| `scheduled` | When you plan to start | A future value parks the task in the backlog until then |
| `wait` | Hide it until this moment | Same parking, different intent: "not my problem yet" |
| `remind` | When to nudge you | Fires a reminder; see below |

`scheduled` and `wait` both keep a task out of your working set until their
moment arrives — the difference is what you *mean*, and
[`tasqx agenda`](Finding-Tasks.md#tasqx-agenda) places tasks on `scheduled`
(or `due`), never on `wait`.

## Reminders

`remind:` takes an offset from the due date, or an absolute time:

```console
tasqx add Call the bank due:"friday 9am" remind:-30m   # 30 minutes before
tasqx add Water plants remind:"friday 8am"             # at an exact time
```

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
```

- Completing a recurring task spawns the next occurrence; the answer shows it.
- Missed occurrences don't pile up — they collapse into a single next one.
- `every N months` can drift across short months; anchor with
  `monthly on day 15` when the day of month matters.
- Stop a recurrence with `tasqx modify <ref> --clear recurrence`.
