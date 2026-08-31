# Working on Tasks

The lifecycle: start, stop, done — with cancel, reopen, pick and undo around
it. Nothing here destroys data; every change is recorded and reversible.

## tasqx start

*Alias: `s`*

Mark a task active and start its timer.

```console
tasqx start 42
tasqx start 42 --keep    # keep other active tasks running too
```

By default starting one task stops any other active one ("single-active") —
`--keep` opts out.

The extra flags (`--client`, `--session-id`, `--transcript-path`,
`--prompt-id`) are for AI agents reporting who is doing the work; see
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

A full-screen list to choose a task from, fuzzy-search style. Type to narrow —
`wac` finds "**W**rite **A**PI **c**onformance tests". Enter starts the
highlighted task; Esc leaves.

```console
tasqx pick                  # pick from the working set
tasqx pick project:work     # narrow the candidates first
```

`pick` needs a real terminal (it draws a screen), so in scripts use
`tasqx next` to ask the same question and `tasqx start <ref>` to act on it.

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
