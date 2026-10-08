# Dates and recurrence

Every date field — `due`, `scheduled`, `wait` — takes the same natural-language
grammar, through the flag or through the sugar. It resolves to RFC3339 at the
moment you type it, and everything is **UTC**: a bare DATE resolves to midnight
UTC, and a TIME with no offset of its own is a UTC clock — `due:17:00` means
17:00 UTC whatever zone the machine is set to, and every screen prints it back
as 17:00. Write an offset (`2026-07-20T17:00:00+02:00`) to mean another zone's
clock.

## The four date fields

| Field | Means |
|---|---|
| `due` | The deadline. Drives urgency and is the anchor for relative reminders. |
| `scheduled` | When you intend to start. Not informational: a date still ahead holds the task in `backlog`, out of the `@working` set, until it passes. |
| `wait` | Hide this task until then — it stays out of the working set. |
| `estimate` | Not a date: an effort duration, totalled by `report`. |

## What you can write

| Form | Examples |
|---|---|
| Absolute | `2026-07-20`, `2026-07-20T17:00`, `"2026-07-20 17:00"`, any RFC3339 |
| Relative words | `today`, `tomorrow`, `yesterday`, `now` |
| Weekdays | `monday`…`sunday`, `mon`…`sun` — the next one after today. `"next friday"` is that day in the following Monday-to-Sunday week, `"next week"` that week's Monday; `this`/`last` are refused rather than guessed |
| Long offsets | `"in 3 days"`, `"in 2 weeks"`, `"in 1 month"` |
| Short offsets | `3d`, `2w`, `1mo`, `1y` — signed: `+3d`, `-1d` |
| Boundaries | `eom` / `"end of month"`, `eow` / `"end of week"` (ISO week ends Sunday) |
| Trailing time | `"friday 17:00"`, `"tomorrow 9am"`, `"monday 5pm"` |
| Leading filler | `"at 6pm"`, `"on friday"`, `"by monday 5pm"` — `at`/`on`/`by`/`@` are ignored |

## The rules that resolve ambiguity

| Situation | Resolution |
|---|---|
| A date with no time | 00:00:00 UTC — the start of that day, whatever your zone. |
| A bare time (`9am`) | 09:00 UTC today, or tomorrow if that UTC time already passed. |
| A weekday that *is* today | The next one — seven days out, not zero. |
| A naive date *with* a time | A UTC clock, whatever your zone. An explicit offset (`+02:00`) is honoured and converted to UTC instead. |
| The literal `now` | This exact instant — not midnight, unlike every other keyword here. It is what `due.before:now` means, so it can find a task due earlier today. |

> **Careful** Short offsets carry **day**-and-larger units only: `d`, `w`,
> `mo`, `y`. `-2h` is not a date — it is a reminder offset, and a different
> grammar. Feeding it to `--due` is a clean error, not a guess:

```console
$ tasqx add "Overdue ping" --due -2h
error [bad_request]: could not parse date: "-2h" (try e.g. tomorrow, friday, 2026-07-20, "in 3 days" (day/week/month/year offsets only, no hours/minutes), eom, or 2026-07-20T17:00)
```

## A leading hyphen needs no escaping

`--due -1d` works. It looks like it should trip the argument parser into reading
`-1d` as a flag — every date-taking flag opts out of that explicitly, so a
signed offset is always a value:

```console
$ tasqx add "Renew the TLS cert" project:work.tasqx +ops --due -1d
▌ #3  Renew the TLS cert
▌ added   - ▄▄▄▄ 12.0   work.tasqx   due yesterday   +ops
```

## Estimates

Human durations, parsed at the edge and stored as ISO-8601 so `report` can total
them. `4h`, `90m`, `1h30m`, `2d`, `1w`, or ISO `PT4H` directly.

```console
$ tasqx add "Nail down the schema" est:soon
error [bad_request]: could not parse duration: "soon" (try e.g. 4h, 90m, 1h30m, 2d, 1w, or ISO PT4H)
```

## Recurrence

A recurring task is a **template**. Completing an instance spawns the next one
with its date advanced by the rule. Set it with `repeat:` / `every:` /
`--repeat`, and stop it with `--clear recurrence`.

| Rule | Example |
|---|---|
| `every N days\|weeks\|months` | `"every 3 days"`, `"every week"` |
| `weekly on <days>` | `"weekly on mon,wed,fri"` |
| `monthly on day <D>` | `"monthly on day 15"` |
| `monthly on the <Nth> <weekday>` | `"monthly on the 2nd tuesday"`, `"monthly on the last friday"` |

This is a deliberate subset — not full RRULE. Anything outside it is a clean
error.

```console
$ tasqx add "Water the plants project:home repeat:\"every 3 days\" due:today"
$ tasqx done 4
$ tasqx show 5
▌ #4  Water the plants
▌ added   - ▄▄▄▃ 11.4   home   due today 23:59   ↻ every 3 days
▌ #4  Water the plants
▌ done today   home   due today 23:59
  #5  next, due Fri
▌ #5  Water the plants
▌
▌ status      pending           urgency     - ▄▄▃▁ 8.9
▌ project     home              due         Fri (in 4 days)
▌ repeats     every 3 days      created     today 07:51 (just now)
▌ modified    today 07:51 (just now)
▌ rev         1
```

## Missed occurrences collapse

If your machine was off for a week, a daily task does *not* hand you seven
instances. The rule advances at least once and then skips every slot at or
before now, so you get exactly one future instance. A backfill storm is never
useful.

## Month-end, precisely

The two monthly forms differ on purpose, and the difference matters at month
boundaries:

| Rule | From Jan 31 | Why |
|---|---|---|
| `monthly on day 31` | Jan 31 → Feb 28 → **Mar 31** | Re-clamps against the stored target day every step, so month-end recovers. |
| `every 1 month` | Jan 31 → Feb 28 → **Mar 28** | Advances from the previous (already clamped) date, so it drifts and stays there. |

> **Note** Pick `monthly on day 31` when you mean "the last-ish day of every
> month". Pick `every 1 month` when you mean "same slot, one month on".
