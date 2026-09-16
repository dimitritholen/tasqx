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
| `est:4h` | effort estimate (`90m`, `1h30m`, `2d` also work) |
| `repeat:"every monday"` | recurrence rule |
| `remind:-1h` | remind me one hour before it's due |

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

## tasqx check

Acceptance criteria: the things that have to be true for a task to count as
done.

```console
tasqx check add 42 the notes name every breaking change
tasqx check set 42 <id> passed --evidence "cargo test: 0 failed"
tasqx check rm 42 <id>
```

**tasqx never runs a check.** The criterion is a claim and the evidence is a
citation — both are stored exactly as you typed them and neither is
interpreted. If you want something run, a hook you installed runs it and calls
`tasqx check set` with the result, like any other client would.

Criteria in an annotation look the same and are not: an annotation is prose,
so nothing can ask at completion time whether it was met. A check has a state.

Completing a task with a criterion still open is **not refused** — tasqx is a
file on your disk, not a supervisor, and a refusal is something a script routes
around. It is counted instead: `tasqx report --outcomes` has an OPEN column for
completions whose criteria nobody marked, which is where the pattern shows up.

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

## tasqx unannotate

Permanently scrub one annotation's text, by id:

```console
tasqx unannotate 42 018f2f7e-1234-7abc-9def-0123456789ab
```

This is a **hard delete**, not a hide: the text is overwritten in the store,
not merely removed from what tasqx shows you — use it to take back a secret, a
customer name, or a wrong root cause pasted into a note by mistake. What stays
behind is a tombstone (the id and when it was removed), for audit, with no
text in it. The tombstone is not hidden away: `tasqx show` and `task.get` both
list it, and the box card's Notes row counts it.

The annotation's id isn't printed by `tasqx show` — read it from `tasqx show
42 --json` (each row under `annotations[].id`) or from a prior `tasqx
annotate` response. `tasqx undo` does **not** cover this: by the time the
removal is recorded, there is nothing left in the log to restore. An unknown
id, or one already removed, exits 4 rather than silently doing nothing.
