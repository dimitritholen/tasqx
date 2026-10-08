# Dates, Reminders and Recurrence

tasqx reads dates the way you'd say them, and four different date fields let
you say four different things about *when*.

Every date field — `due`, `scheduled`, `wait` — takes the same natural-language
grammar, through the flag or through the sugar. It resolves to RFC3339 at the
moment you type it.

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
clock — `due:17:00` is 17:00 UTC wherever you type it, whatever zone the
machine is set to, and every screen prints it back as 17:00. An offset you
write yourself (`+02:00`, or a trailing `Z`) is honoured as written:
`2026-07-20T17:00:00+02:00` means another zone's clock, and is converted to UTC
on the way in.

### What you can write

| Form | What you can write |
|---|---|
| Absolute | `2026-07-20`, `2026-07-20T17:00`, `"2026-07-20 17:00"`, any RFC3339 |
| Relative days | `today`, `tomorrow`, `yesterday`, `now` |
| Boundaries | `eom` / `"end of month"`, `eow` / `"end of week"` (ISO week ends Sunday) |
| Weekday names | `monday`..`sunday` or `mon`..`sun` — the next one after today. `"next friday"` is that day in the following Monday-to-Sunday week, `"next week"` that week's Monday; `this`/`last` are refused rather than guessed |
| Counted spans | `in 1 day`, `"in 3 days"`, `"in 2 weeks"`, `"in 1 month"`, `in 3 months` — days, weeks and months only; `in 2 hours` is rejected |
| Signed offsets | `-1d`, `+3d`, `3d` (no sign means the future); the units are `d`, `w`, `mo`, `y`, so `3d`, `2w`, `1mo` and `1y` all work |
| Times | `17:00`, `5pm`, attached to a day with a space — `"tomorrow 17:00"`, `"friday 9am"`, `"tomorrow 9am"`, `"monday 5pm"` — or a full instant — `"2026-09-09 17:00"`, `2026-09-09T17:00:00+02:00` (an explicit offset is converted to UTC on the way in) |
| Leading filler | `"at 6pm"`, `"on friday"`, `"by monday 5pm"` — `at`/`on`/`by`/`@` are ignored |

### The rules that resolve ambiguity

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

### A leading hyphen needs no escaping

`--due -1d` works. It looks like it should trip the argument parser into reading
`-1d` as a flag — every date-taking flag opts out of that explicitly, so a
signed offset is always a value:

```console
$ tasqx add "Renew the TLS cert" project:work.tasqx +ops --due -1d
▌ #3  Renew the TLS cert
▌ added   - ▄▄▄▄ 12.0   work.tasqx   due yesterday   +ops
```

## The four date fields

| Field | What it says | Effect |
|---|---|---|
| `due` | When it must be finished | The deadline. Drives urgency up as it approaches, and is the anchor for relative reminders; overdue tasks stay loudly visible. A deadline with a time is overdue once it passes; a date without a time is due by the end of that day (UTC) |
| `scheduled` | When you plan to start | A future value parks the task in the backlog, out of the `@working` set, until it passes |
| `wait` | Hide it until this moment | Same parking, different intent: "not my problem yet" |
| `remind` | When to nudge you | Fires a reminder; see below |
| `estimate` | How long it will take | Not a date: an effort duration, totalled by `report` |

`scheduled` and `wait` both keep a task out of your working set until their
moment arrives — the difference is what you *mean*, and
[`tasqx agenda`](Finding-Tasks.md#tasqx-agenda) places tasks on `scheduled`
(or `due`), never on `wait`.

### Estimates

Human durations, parsed at the edge and stored as ISO-8601 so `report` can total
them. `4h`, `90m`, `1h30m`, `2d`, `1w`, or ISO `PT4H` directly.

```console
$ tasqx add "Nail down the schema" est:soon
error [bad_request]: could not parse duration: "soon" (try e.g. 4h, 90m, 1h30m, 2d, 1w, or ISO PT4H)
```

## Reminders

`remind:` takes an offset from the due date, or an absolute time:

| Command | What it does |
|---|---|
| `tasqx add Call the bank due:"friday 9am" remind:-30m` | 30 minutes before |
| `tasqx add Water plants remind:"friday 8am"` | At an exact time |

The offset stays *symbolic*: move the due date and the reminder moves with it.

Reminders fire only while [`tasqx daemon`](AI-Agents-and-Automation.md#tasqx-daemon)
is running, and they're quiet by default — the OS toast notification lives
behind an off-by-default build feature (`notify-os`). tasqx is quiet by
default: a task notifies you only if you gave it a `remind`, and nothing else
ever puts a task on the reminder heap.

### The two forms

| Form | Example | Behaviour |
|---|---|---|
| Offset from `due` | `-1h`, `-30m`, `-2d`, `+15m` | Stays symbolic in the store. Move `due` and the reminder moves with it. |
| Absolute instant | `"friday 9am"`, `2026-07-20T17:00` | Resolved once, at set time, through the same date grammar. |

The **sign** is what disambiguates them: a leading `-` or `+` means offset,
anything else goes to the date parser. Without that rule `3d` would be ambiguous
— "3 days before due" or "in 3 days"?

Offsets take `s`, `m`, `h`, `d`, `w`. Negative is before due, which is what you
almost always want.

```console
$ tasqx modify 1 --remind -1h
$ tasqx show 1
▌ #1  Ship the v1 JSON API freeze
▌ modified   remind -1h   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h   rev 2
▌ #1  Ship the v1 JSON API freeze
▌
▌ status      pending           urgency     H ▄▄▄▄ 15.7
▌ project     work.tasqx        due         Fri (in 3 days)
▌ remind      -1h               estimate    4h
▌ tags        +api +release     created     today 07:51 (just now)
▌ modified    today 07:51 (just now)
▌ rev         2
```

> **Note** Notice `remind` shows as `-1h`, not as a resolved timestamp. That
> is the point: it is still an offset. Push `due` back a week and the reminder
> follows, with no second edit.

### Who delivers them

The [daemon](Dashboard-and-Live-View.md#running-it). It keeps an
in-memory min-heap of upcoming reminder instants, rebuilt from the store on
start and whenever a task changes. A one-shot `tasqx` command never fires
anything — there would be nobody to fire it.

```console
$ tasqx add "Deploy the release" --due "2026-07-16T09:00" --remind -1h
$ tasqx daemon --socket tasqx-remdemo
▌ #1  Deploy the release
▌ added   no project · set a default with tasqx use <project>   - ▄▄▄▂ 10.3   due Thu
tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)
tasqx daemon: store ~/.local/share/tasqx/tasks.db
tasqx reminder: [#1] Deploy the release (due 2026-07-16T09:00:00Z)
```

That reminder had already ripened (due 09:00, minus 1h, and it was past 08:00) —
so it fired on the next daemon start. **A reminder that ripened while the daemon
was down still fires, once, on the next start.** Sleeping your laptop does not
lose it.

### It fires exactly once

Firing writes a `reminded` event, and that event row *is* the dedupe record.
Restart the daemon and the same reminder does not come back:

```console
$ tasqx daemon --socket tasqx-remdemo   # second start, same store
tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)
tasqx daemon: store ~/.local/share/tasqx/tasks.db
```

Silence — correct. The key is the (task, *instant*) pair, not just the task:
moving `due` moves a relative reminder to a genuinely new instant, which
*should* fire again.

### Delivery never fails

The always-compiled backend writes one line to stderr and returns. That is the
headless/CI-safe path: with no notification transport anywhere, delivery
degrades to a logged line and exit 0 — never an error.

Native OS toasts (Windows, macOS, Linux/D-Bus) live behind the off-by-default
`notify-os` build feature *and* need an explicit opt-in:

```
# config.toml
[notify]
enabled = true
```

Even then the stderr line still comes first, so the verifiable surface never
depends on which backend is live. Two opt-ins, both off by default — that is the
quiet-by-default rule taken seriously.

### Firing one by hand

`reminder.fire` takes a `ref` and an `at` instant, and it is idempotent — the
second call reports `fired: false` rather than notifying twice:

```console
$ echo '{"tasqx":"1","id":"r1","method":"reminder.fire","params":{"ref":"1","at":"2026-07-16T00:00:00Z"}}' | tasqx api
$ echo '{"tasqx":"1","id":"r2","method":"reminder.fire","params":{"ref":"1","at":"2026-07-16T00:00:00Z"}}' | tasqx api
{"id":"r1","ok":true,"result":{"at":"2026-07-16T00:00:00Z","fired":true,"short_id":1},"tasqx":"1"}
{"id":"r2","ok":true,"result":{"at":"2026-07-16T00:00:00Z","fired":false,"short_id":1},"tasqx":"1"}
```

And the event is in the log, where the dedupe check reads it:

```
{
  "actor": "user",
  "entity": "task",
  "op": "reminded",
  "payload": {
    "at": "2026-07-16T00:00:00Z",
    "due": "2026-07-17T00:00:00Z",
    "remind": "-1h",
    "short_id": 1,
    "title": "Deploy the release"
  },
  "ts": "2026-07-16T08:52:35.4429051Z"
}
```

## Recurrence

`repeat:` makes a task come back. A recurring task is a **template**:
completing an instance spawns the next one with its date advanced by the rule.
Set it with `repeat:` / `every:` / `--repeat`, and stop it with
`tasqx modify <ref> --clear recurrence`.

| Rule | Example |
|---|---|
| `every N days\|weeks\|months` | `"every 3 days"`, `"every week"` |
| `weekly on <days>` | `"weekly on mon,wed,fri"` |
| `monthly on day <D>` | `"monthly on day 15"` |
| `monthly on the <Nth> <weekday>` | `"monthly on the 2nd tuesday"`, `"monthly on the last friday"` |

This is a deliberate subset — not full RRULE. Anything outside it is a clean
error.

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

### Missed occurrences collapse

If the computer was off for a week, a daily task does *not* hand you seven
instances. The rule advances at least once and then skips every slot at or
before now, so you get exactly one future instance. A backfill storm is never
useful.

### Month-end, precisely

The two monthly forms differ on purpose, and the difference matters at month
boundaries:

| Rule | From Jan 31 | Why |
|---|---|---|
| `monthly on day 31` | Jan 31 → Feb 28 → **Mar 31** | Re-clamps against the stored target day every step, so month-end recovers. |
| `every 1 month` | Jan 31 → Feb 28 → **Mar 28** | Advances from the previous (already clamped) date, so it drifts and stays there. |

> **Note** Pick `monthly on day 31` when you mean "the last-ish day of every
> month". Pick `every 1 month` when you mean "same slot, one month on".
