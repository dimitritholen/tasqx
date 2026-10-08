# Adding and Editing Tasks

Capture fast, refine later. `add` and `modify` share the same tricks: inline
shortcuts in the title, natural-language dates, and flags for everything if you
prefer being explicit.

## tasqx add

*Aliases: `a`, `new`*

Create a task. The simplest form is just a title:

```console
tasqx add Buy milk
```

The title can carry inline shortcuts ("sugar"), so one line captures
everything:

```console
tasqx add Ship the release due:friday +api !high est:4h
```

| Shortcut | Meaning |
|---|---|
| `+api` | add the tag `api` |
| `project:work` (or `proj:work`) | file it under `work` |
| `!high` / `!med` / `!low` | priority |
| `due:friday` | due date — natural language is fine |
| `scheduled:friday` (or `sched:`) | when you can start — parks the task in backlog until then |
| `wait:monday` | hide it until then — also parks it in backlog |
| `est:4h` (or `estimate:4h`) | effort estimate (`90m`, `1h30m`, `2d` also work) |
| `repeat:"every monday"` (or `every:` / `recur:`) | recurrence rule |
| `remind:-1h` | remind me one hour before it's due, or at a time you name |

Every shortcut also exists as a flag (`--due friday`, `--tag api`,
`--priority H`, …) — same result, pick whichever reads better in your scripts.

More on dates, recurrence and reminders:
[Dates, Reminders and Recurrence](Dates-Reminders-and-Recurrence.md).

A task without a project lands in your default project
([`tasqx use`](Projects.md#tasqx-use) changes which one that is).

## tasqx modify

*Aliases: `mod`, `m`, `edit`*

Change an existing task. Takes the same inline sugar and dates as `add`:

| Command | What it does |
|---|---|
| `tasqx modify 42 due:friday !high` | Set fields |
| `tasqx modify 42 Fix the login bug` | Bare words replace the title |

**Setting and clearing are different moves.** Setting is `due:friday`;
removing is `--clear due`. There is no magic empty value:

```console
tasqx modify 42 --clear due --clear remind
```

`--clear` works for: `project`, `priority`, `due`, `scheduled`, `wait`,
`remind`, `recurrence`, `estimate`, `tracked`, `budget_tokens`. Tags are the
exception — a tag comes off by name, with [`tasqx untag`](#tasqx-untag).

A task's status is not a field you set: it moves through `start`, `stop`,
`done`, `cancel` and `reopen` — see [Working on Tasks](Working-on-Tasks.md).

## What a write prints

Every write answers with the same card: the task it named, then the outcome
and what changed, then the rest of that row. Bold is the write's own mark — it
says THIS is what changed, and nothing else wears it, because bold is the one
emphasis that survives `NO_COLOR`.

```console
$ tasqx start 1
▌ #1  Ship the v2 pricing page
▶ started   H ▄▄▄▄ 16.7   work   due Mon   +launch
```

The rail at the left carries the state: `▶` while the task runs, `⊘` while it
is blocked, `▌` otherwise. Another task the write moved gets a line of its own
under the card:

```console
$ tasqx done 1
▌ #1  Ship the v2 pricing page
▌ done today   tracked 5m   work   due Mon   +launch
  #2  unblocked · Rate-limit the search endpoint
```

Through a pipe the same words print, unfitted: the rail spells itself `*` for
running and `B` for blocked, and the gauge and the `▌` go. `--json` is
unchanged.

## tasqx check

Acceptance criteria: the things that have to be true for a task to count as
done.

```console
tasqx check add 42 the notes name every breaking change
tasqx check set 42 <id> passed --evidence "cargo test: 0 failed"
tasqx check rm 42 <id>
```

`tasqx show` numbers each check (`2 [ ] the notes name every breaking
change`). `<id>` is that position, counting from 1 — `tasqx check set 42 2
failed` marks the second one — or the check's full id. A mistyped id is
refused with the task's real checks listed.

`check add`, `check set` and `check rm` answer with the one check they
touched and how many have passed, not the whole task; `tasqx show` has the
rest. A criterion may begin with `--` (`tasqx check add 42 --since 2026-01 is
honoured`): after the task, a dash-led word that is not one of `check add`'s
own flags (`--json`, `--no-daemon`, …) is part of the criterion.

**tasqx never runs a check.** The criterion is a claim and the evidence is a
citation — both are stored exactly as you typed them and neither is
interpreted. If you want something run, a hook you installed runs it and calls
`tasqx check set` with the result, like any other client would.

Criteria in an annotation look the same and are not: an annotation is prose,
so nothing can ask at completion time whether it was met. A check has a state.

Completing a task with a criterion still open or marked failed is **not
refused** — tasqx is a file on your disk, not a supervisor, and a refusal is
something a script routes around. `tasqx done` says so in one line, naming how
many failed and how many are still open, and it is counted: a completion is
proven only when every check passed, and `tasqx report --outcomes` has an
UNPROVEN column for the rest, adding `N failed` when any had a failed check.
That column is where the pattern shows up.

`failed` is a normal outcome and worth recording. Remove a check only when the
criterion was the wrong thing to ask — deleting one the work failed hides the
finding.

## Token budgets

`--budget-tokens` sets a size gauge on a task:

```console
tasqx add Port the payment adapter --budget-tokens 200000
```

It counts **fresh** tokens — input, output and cache creation — and ignores
cache reads, because a budget dominated by re-reads measures how often an agent
re-read its own context rather than how big the work was. `tasqx show` prints
the pair once a budget is set.

**It stops nothing.** tasqx is a file on your disk that an agent talks to
between turns; it never sees the turn and could not interrupt one. An overrun
is a signal, and usually one signal in particular: the task was too big to hand
over whole. `tasqx report --outcomes` counts overruns so you can see which kinds
of work keep being cut too large.

For scripts that must not clobber a concurrent edit: `--expected-rev` makes
the modify fail (exit 5) if the task changed since you last read it.

## tasqx tag

Attach one or more tags.

| Command | What it does |
|---|---|
| `tasqx tag 42 api release` | Two tags, one command |
| `tasqx tag 42 +api` | The leading + is optional |

Re-adding a tag the task already has is fine — the answer is simply the
resulting tag set.

Tags are stored lowercase, so `+API` and `+api` are the same tag, and a tag
cannot contain a space — `tasqx tag 42 "needs paint"` is refused; write
`needs-paint`. A filter's `+API` matches it too. A store written by an older
tasqx is folded into this form the first time it is opened (spaces become
hyphens, duplicates merge), and each task it touched gets a `tag.normalize`
event naming every `from` → `to` it applied — `event.list` with that task's
`ref` shows them (see [`tasqx api`](AI-Agents-and-Automation.md#tasqx-api)).

## tasqx untag

Remove one or more tags.

```console
tasqx untag 42 api
```

All or nothing: if any named tag isn't on the task, *none* are removed and the
command tells you which tags the task does have. A typo never quietly
succeeds.

## tasqx annotate

*Alias: `note`*

Attach a timestamped note to a task. The text is stored exactly as you typed
it — multi-line, markdown, links, all preserved.

```console
tasqx annotate 42 Called the plumber, waiting on a quote
```

Annotations show up in `tasqx show`, and they're searchable: the
[memory system](Memory.md) indexes them alongside your knowledge documents, so
"what did we decide about the plumber" is one `tasqx memory search` away.

For a long or multi-line note, give the text on stdin. `-` reads all of stdin,
as does a bare `annotate` when stdin is a pipe:

```console
tasqx annotate 42 - < decision.md
```

On a terminal, a bare `tasqx annotate 42` opens `$VISUAL` or `$EDITOR` (else
`vi`) on a temporary file. Saving an empty or unchanged file stores nothing, and
so does empty stdin. A note that is only `-` can't be written as an argument.

`tasqx show` numbers the notes, oldest first, with a dim `[n]` in front of
each. That number, a unique prefix of at least eight characters of the note's
id, or the full id names a note anywhere a note is named. Notes written within
about a minute share their first eight characters, so a prefix is often
ambiguous (exit 5, listing the candidates); the number is the short way.

To correct a note instead of adding another, name it with `--edit`:

```console
tasqx annotate 42 --edit 2 Plumber quoted 400, booked for Friday
```

The text is replaced in place: the note keeps its id, its timestamp and its
place in the history, so the first note is still the box card's Description,
and search finds the new text rather than the old. `tasqx undo` puts the old
text back. `--edit` takes the same stdin and editor forms; the editor opens on
the note's current text.

Put a note on the wrong task? Move it instead of retyping it:

```console
tasqx annotate 42 --move 2 --to 57
```

The note keeps its id, text and timestamp and lands on #57 in its place by
age; search finds it there. `tasqx undo` puts it back on #42.

## tasqx unannotate

Permanently scrub one annotation's text:

```console
tasqx unannotate 42 2
```

This is a **hard delete**, not a hide: the text is overwritten in the store,
not merely removed from what tasqx shows you — use it to take back a secret, a
customer name, or a wrong root cause pasted into a note by mistake. What stays
behind is a tombstone (the id and when it was removed), for audit, with no
text in it. The tombstone is not hidden away: `tasqx show` and `task.get` both
list it, and the box card's Notes row counts it.

Name the note by the `[n]` position `tasqx show` prints, a unique id prefix of
eight or more characters, or its full id (`annotations[].id` in `--json`).
`tasqx undo` does **not** cover this: by the time the
removal is recorded, there is nothing left in the log to restore. An unknown
id, or one already removed, exits 4 rather than silently doing nothing.
