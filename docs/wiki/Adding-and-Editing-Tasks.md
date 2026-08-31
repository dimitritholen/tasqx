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

```console
tasqx modify 42 due:friday !high     # set fields
tasqx modify 42 Fix the login bug    # bare words replace the title
```

**Setting and clearing are different moves.** Setting is `due:friday`;
removing is `--clear due`. There is no magic empty value:

```console
tasqx modify 42 --clear due --clear remind
```

`--clear` works for: `project`, `priority`, `due`, `scheduled`, `wait`,
`remind`, `recurrence`, `estimate`. Tags are the exception — a tag comes off
by name, with [`tasqx untag`](#tasqx-untag).

For scripts that must not clobber a concurrent edit: `--expected-rev` makes
the modify fail (exit 5) if the task changed since you last read it.

## tasqx tag

Attach one or more tags.

```console
tasqx tag 42 api release    # two tags, one command
tasqx tag 42 +api           # the leading + is optional
```

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
