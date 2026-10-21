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

## tasqx claim

Claim a task so other agents skip it. A claim is a lease: 30 minutes unless
`--lease` names another length, at most 24 hours.

- `tasqx claim 42 --holder builder-2`: hold #42 for builder-2
- `tasqx claim 42 --holder builder-2 --lease 2h`: renew it for two hours

```console
tasqx claim 42 --holder builder-2
```

While the lease is live, anyone else's `claim`, `start`, `stop`, `done`,
`cancel` or `modify` to cancelled on the task exits 5 naming the holder and when
the lease runs out (`--force-claim` goes ahead and the log names whose claim it
overrode; `done --force` is only for open dependencies), and starting another task
leaves it running instead of stopping it. `tasqx next` skips it for anyone but
the holder, and so does `tasqx pick`. `tasqx list` still shows it, with its
holder and expiry: a list is a view, and only `list --holder <id> @working` —
an agent choosing its own work — leaves other holders' claims out. `tasqx list @claimed` lists every live
claim, and `tasqx show` names the holder and the expiry.

Without `--holder` the shell holds the claim as `user`, and `tasqx next` from
the same shell still shows it. `next`, `list` and `pick` take `--holder` to
choose work as a named holder, and `start`, `done` and `cancel` take it to act
on a task claimed under one (the timer itself stays the shell's):

```console
tasqx start 42 --holder builder-2
```

An AI agent on the MCP server holds it as its connection unless it names a
stable `actor`. Nothing sweeps an expired lease: once it runs out, anyone may
claim the task, and the log records that as a takeover.
Claim, renew, release and takeover each land in the event log with the holder.

## tasqx release

Give a claim up. Only its holder can: a task with no claim, or someone else's,
exits 5. `done` and `cancel` end a claim without it. `--force-claim` releases a claim
someone else holds — one left by an agent that crashed or reconnected under a
new id — and the log records it as forced. A claim whose lease has run out can
be released by anyone, no `--force-claim` needed.

```console
tasqx release 42 --holder builder-2
```

## tasqx stop

*Alias: `st`*

Pause an active task. The tracked time is kept.

```console
tasqx stop 42
```

While a timer runs, `tracked` already counts it: `list`, `show`, `report` and
the card all include the open interval up to the moment you ask.

## tasqx adjust

Correct a task's tracked time when the clock was wrong — a timer left running
over lunch, or work that never reached it.

```console
tasqx adjust 42 -2h25m --reason "idle gap"
tasqx adjust 42 +30m --reason "forgot to start"
```

- The reason is required and stays in the task's history with the correction.
- It works on done tasks too, so `report --outcomes` calibration can be fixed
  after the fact. A correction that would take the total below zero is refused.
- `show` prints the net of every correction beside the total:
  `tracked 2h30m (adjusted -2h25m)`.
- `tasqx undo` takes the newest correction back.

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

## tasqx git

Record what happened to a task's pull request, and let the task follow it.

```console
tasqx git event opened --branch task/42-pricing --pr https://github.com/acme/web/pull/7
tasqx git event merged --branch task/42-pricing --pr https://github.com/acme/web/pull/7 --sha 4f9c2e1
tasqx git event closed --branch fix-typo --pr https://github.com/acme/web/pull/8 --task 43
```

A branch named `task/<id>-<slug>` names its task, whatever follows the id
(`task/42-pricing-rebased` too); `--task` names it when the branch does not.

- **opened** starts a pending task. It stops no other timer.
- **merged** completes the task, with `Merged <pr> (<sha>)` as its delivery
  note, but only when every check passed (or it has none) and nothing blocks
  it. Otherwise the task stays open and a note names the checks that did not
  pass.
- **closed** adds a note and leaves the status alone.

`tasqx show` lists the branch, the PR and the merge commit under `git`.
Reporting `merged` again after the checks are fixed completes the task. A PR
starts its task once: reporting `opened` again after you stopped it does
nothing, and a task a merge completed that you reopened stays open. An event
whose outcome already holds changes nothing, and so does one a recorded merge
settled (`closed` after `merged`); both say why. When someone holds a claim on
the task, the event acts as that holder.

tasqx never asks GitHub, or any network. Something that watched the PR tells
it: a script reading `gh pr view --json`, a CI step, a git hook.

## tasqx triage

Work you or an agent finds mid-task can be filed as a **proposal** instead of
going straight into the backlog: `tasqx add … --triage` (an agent passes
`status: "triage"`, or every MCP add lands there once `mcp.add_status` is
`triage`). A proposal stays out of `@working`, `next` and the urgency ranking,
and cannot be started or completed until it is accepted.
`--discovered-from <id>` links it to the task it was found on.

```console
tasqx triage
```

lists the inbox oldest first, with the tool that filed each proposal and the
task it was found on. `tasqx list status:triage` is the same set as a table.
Settle each one with a single command, below.

## tasqx accept

```console
tasqx accept 12
```

Makes the proposal pending — or backlog, while a future `wait` or `scheduled`
still holds it. `tasqx undo` puts it back in triage.

## tasqx decline

```console
tasqx decline 12 --reason "out of scope for this release"
```

Cancels the proposal and keeps the reason as a note. `tasqx undo` puts it back
in triage; the note stays.

## tasqx duplicate

```console
tasqx duplicate 12 --of 7
```

Cancels the proposal, moves its notes to the task it repeats and links the two
with `duplicates`. Undo does not take this one back.

The dashboard's header, the HTML report and the desktop app count what is
waiting in triage.

## tasqx pick

*Aliases: `p`, `fzf`*

A full-screen browser over your tasks, each row the row `tasqx list` prints.

| Command | What it does |
|---|---|
| `tasqx pick` | Browse the working set |
| `tasqx pick project:work` | Narrow the candidates first |

| Key | What it does |
|---|---|
| `j`/`k` | move, as do the arrows; `g`/`G` jump to the ends |
| `/` | search as you type, fuzzy-search style: `wac` finds "**W**rite **A**PI **c**onformance tests"; Enter or Esc keeps the filter |
| `enter` | open that task's card, the one `tasqx show` prints (Esc goes back) |
| `s` | start the task under the cursor, from the list or from its card |
| `q` | leave; `esc` clears a search first, and only then leaves |

The bar along the bottom names the keys that can do something where you are,
so it is shorter on an empty list, on a card, and on a narrow terminal — the
way out is named at every width.

`s` is the only key that writes. A filter can list work that is done or
cancelled, and `s` on such a row is refused on the key bar's own row with the
screen still open.

Captured on a terminal 120 columns wide, since this screen cannot be piped:

<!-- fixture: pick -->
```text
 pick   @working   14 tasks · 1 overdue · 1 due today · ▶ #48 running

       ID          URG  TASK                                           PROJECT  DUE             TAGS
 ▸     47  H ▄▄▄▄ 18.1  Renew the TLS certificate for api.example.dev  infra    2d ago          +ops
   ▶   48  H ▄▄▄▄ 17.8  Ship the v2 pricing page                       website  today 17:00     +launch
```

The key bar is the screen's bottom row, below the rest of the list:

<!-- fixture: pick -->
```text
 j/k move   / search   enter open   s start   g/G ends   q leave
```

`pick` needs a real terminal (it draws a screen), so in scripts use
`tasqx next` to ask the same question and `tasqx start <ref>` to act on it.

Leaving without starting a task exits 0: reading your tasks and starting none
of them is an ordinary way to use it, so `tasqx pick && …` and a prompt
indicator survive `q`. A filter that matches no task still exits 4, and so
does an empty working set: that is a question tasqx could not answer, not a
session you ended.

## tasqx undo

*Alias: `u`*

Take back the newest recorded change.

```console
tasqx undo
```

`undo` is deliberately narrow, and honest about it:

- Twelve operations are undoable: `done`, `cancel`, `modify`, `tag`, `stop`,
  `untag`, `undep`, `annotate`, `annotate --edit`, `annotate --move`,
  `adjust` and `accept`. Everything else is refused *by name*, with the command that does
  take it back — undoing a `start` is `tasqx stop`, undoing an `archive` is
  `tasqx unarchive`.
- Undoing a **`done`** puts the task back in the status it left. If it was
  running, the interval reopens and its time comes back off the total; criteria
  the completion proved (`--check`) go back to open. If completing it spawned
  the next instance of a recurring task, that instance is removed too — but only
  while nothing has touched it. Touch it (annotate it, change it, start it) and
  undo refuses, naming it, rather than throw your work away. A completion that
  also recorded a token measurement is refused too: a measurement is the record
  of what a turn cost, and `undo` does not retract one.
- Undoing a **`cancel`** puts back the status it left. Undoing a **`modify`**
  puts back every field it changed, and the answer lists them, so a cleared
  `project` is back and `due` is cleared again. Undoing a **`tag`** takes off
  only the tags that call attached, not one the task already had.
- A `modify` or `tag` written by an older tasqx did not record what it
  replaced, so undo refuses those and says so; from this version on every one
  does.
- It reverses the newest *recorded* event, which is not always the last
  command you typed: a command that changed nothing recorded nothing, so undo
  reaches past it. That's why the answer names exactly what it undid — read it.
- Undo appends its inverse to the event log rather than erasing anything, so
  your history reads "X happened, then it was undone".
- There is no redo.
