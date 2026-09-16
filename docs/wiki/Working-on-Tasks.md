# Working on Tasks

The lifecycle: start, stop, done — with cancel, reopen, pick and undo around
it. Nothing here destroys data; every change is recorded and reversible.

## tasqx brief

Everything you need before starting a task, in one read.

```console
tasqx brief 42
tasqx brief 42 --memory-limit 3
```

You get the task's own card, then two things `tasqx show` never carried:

- **Depends on** — each prerequisite with **what it concluded**: its newest
  annotation, printed under it. That is usually the single most useful thing to
  read before picking work up, and it used to cost one `tasqx show` per
  prerequisite.
- **From memory** — documents and past annotations relevant to this task, found
  under a query tasqx builds from the task's own title, tags and project. You
  do not supply search terms, which matters because a guessed term that finds
  nothing looks exactly like a store with nothing in it. Half the slots are
  held for docs, so an imported ruling still reaches the page when a project's
  own task notes share its words; annotations take the rest, and docs take the
  whole page when more of them matched than that.

Memory is scoped to the task's project and stays scoped when that finds
nothing — it will not quietly search everything instead. When you want wider,
[`tasqx memory search`](Memory.md) is the read for it.

`tasqx show` is still the right command when you only want the task.

`--card` works here too, and prints the same box-drawn card `tasqx show
--card` prints, followed by the brief's own **Depends on** / **Blocks** /
**From memory** sections.

## tasqx start

*Alias: `s`*

Mark a task active and start its timer.

- `tasqx start 42 --keep`: keep other active tasks running too

```console
tasqx start 42
tasqx start 42 --keep
```

By default starting one task stops any other active one ("single-active") —
`--keep` opts out.

That stays true for you at a shell. It stops being true across *sessions*: if
an AI agent holds a running timer, starting a task from another session is
refused rather than silently stopping their clock, because stopping it would
leave the rest of their work untracked and tell only you about it. `--keep`
runs both deliberately.

The extra flags (`--client`, `--session-id`, `--transcript-path`) are for AI
agents reporting who is doing the work; see
[AI Agents and Automation](AI-Agents-and-Automation.md#token-accounting).

## tasqx stop

*Alias: `st`*

Pause an active task. The tracked time is kept.

```console
tasqx stop 42
```

## tasqx done

*Aliases: `d`, `x`, `complete`*

Complete a task.

```console
tasqx done 42
```

- If the task recurs, completing it spawns the next occurrence — the answer
  shows it.
- If other tasks were waiting on this one, they unblock now, and the answer
  names them. Finishing work tells you what it made possible.
- A task with dependencies still open is refused, naming them. `tasqx done 42
  --force` completes it anyway; the override is recorded on the completion
  and `tasqx report --outcomes` counts it under FORCED. Cancelling a blocked
  task never needs the flag.

## tasqx cancel

*Aliases: `delete`, `del`, `rm`*

Cancel a task. This is as close to "delete" as tasqx gets, on purpose: the
task keeps its history, stays in the event log, and comes back with `reopen`.

```console
tasqx cancel 42
```

If other tasks were waiting on the cancelled one, they're released — a dead
prerequisite shouldn't block live work.

## tasqx reopen

Bring a done or cancelled task back to pending.

```console
tasqx reopen 42
```

If open tasks depended on it, they become blocked again — the mirror image of
what `done` unblocked. The answer names them.

## tasqx pick

*Aliases: `p`, `fzf`*

A full-screen browser over your tasks, each row the row `tasqx list` prints.
`j`/`k` or the arrows move. Enter opens the task's `tasqx show` card (Esc goes
back), and `s` starts the task under the cursor. `/` searches, fuzzy-search
style — `wac` finds "**W**rite **A**PI **c**onformance tests" — and Enter or Esc
keeps the filter. Esc in the list clears it, and `q` leaves.

| Command | What it does |
|---|---|
| `tasqx pick` | Browse the working set |
| `tasqx pick project:work` | Narrow the candidates first |

`pick` needs a real terminal (it draws a screen), so in scripts use
`tasqx next` to ask the same question and `tasqx start <ref>` to act on it.

Closing the browser without starting anything exits 0 — reading your tasks and
starting none of them is an ordinary way to use it, so `tasqx pick && …` and a
prompt indicator survive `q`. A filter that matches no task exits 4 instead:
that is a question tasqx could not answer, not a session you ended.

## tasqx undo

*Alias: `u`*

Take back the newest recorded change.

```console
tasqx undo
```

`undo` is deliberately narrow, and honest about it:

- Four operations are undoable: `stop`, `untag`, `undep` and `annotate`.
  Everything else is refused *by name*, with the command that does take it
  back — undoing a `done` is `tasqx reopen`, undoing a `modify` is a second
  `modify`.
- It reverses the newest *recorded* event, which is not always the last
  command you typed: a command that changed nothing recorded nothing, so undo
  reaches past it. That's why the answer names exactly what it undid — read it.
- Undo appends its inverse to the event log rather than erasing anything, so
  your history reads "X happened, then it was undone".
- There is no redo.
