# Driving tasqx from an AI agent

tasqx ships an MCP server, so an agent reads and mutates your tasks through the same
core API your shell uses. No glue code, no scraping `--json` output.

## Wire it up

For Claude Code, one command sets up the server and installs the
`tasqx-workflow` and `retro` skills:

```console
tasqx setup
```

It's a checklist screen — Space toggles an item, Enter installs what's
ticked, `--yes` installs everything not yet present without the screen. Run
it again later and it shows what's already in place. To wire up only the
server by hand:

```console
claude mcp add --scope user tasqx -- tasqx mcp serve --scope write
```

Any other MCP client takes the same command/args shape:

```json
{
  "mcpServers": {
    "tasqx": {
      "command": "tasqx",
      "args": ["mcp", "serve", "--scope", "write"]
    }
  }
}
```

Leave off `--scope write` and the server is read-only: the write tools are not just
refused, they are absent from the tool list, so the agent cannot even try. Start
read-only; grant write once you have watched what the agent does with read.

## Shelling out: always spell the verb

An agent that runs shell commands must write `tasqx list`, never a bare `tasqx`.

A bare `tasqx` opens a full-screen dashboard whenever stdin and stdout are both
terminals — and a harness that gives its child a pty (pexpect, node-pty, tmux,
`docker run -t`) satisfies exactly that, however unattended it is. The child then
switches to the alternate screen and blocks on a keypress that never comes:
measured at 80x24 and 120x40, no exit, no parseable output, and the tool call
times out. Setting `CI=true` does not help, because nothing reads it — the stream
check is the whole condition.

Killing it does not undo the screen either: the leave-alternate-screen sequence
is never sent, so a harness that reuses one pty runs its *next* command inside
the alternate buffer.

Either of these is enough:

- `tasqx list`: the verb always means the table
- `TASQX_DASHBOARD=false tasqx`: switches the screen off for the whole image instead

```console
tasqx list
TASQX_DASHBOARD=false tasqx
```

The MCP server above is unaffected: it speaks JSON-RPC over pipes, so it is on
the non-interactive side by construction.

## The loop an agent runs

The tool surface is designed around one loop — work the backlog one task at a time:

1. `tasqx_list_tasks` with `"project:myapp.checkout"` — see the feature, blocked
   tasks marked. The default row is compact — `short_id, title, status, priority,
   urgency, blocked, due, project, tags` — so pass `fields` for any other column,
   or `fields: []` for the whole row.
2. `tasqx_brief_task` — everything needed before starting, in one call: the task
   and its annotations, each prerequisite with **what that task concluded**, what
   this one blocks, and relevant memory under a query tasqx derives from the
   task's own title, tags and project. The answer is one content block:
   tasqx-rendered markdown. That is the intended reading — layout is tasqx's job,
   so the agent uses it as-is rather than recomposing the detail from JSON — and
   the `detail.time_format` config key (`iso`, `relative` or `both`) decides how
   it writes timestamps. Pass `include_json: true` when a script needs the raw
   JSON beside it; it is the same result again, so the agent that reads the
   markdown is paying twice for it.

   `tasqx_get_task` is still the right call when you only want the task itself.
3. `tasqx_start_timer`, do the work, `tasqx_complete_task` — the completion result
   names any tasks it unblocked, which is the agent's cue for what to pick up next.
   Pass the turn's token counts on completion (`input_tokens`, `output_tokens`,
   `cache_read_tokens`, `cache_creation_tokens`): the agent is the only party
   that knows which task the spend served, so self-report is the primary
   measurement channel — completing without counts earns a `tokens_hint` in the
   response saying exactly that.
   Pass `view: "card"` when a person will read the outcome: the completion then
   leads with the box card of the task as completed (Delivered row, checks), so
   no `tasqx_get_task` re-read is needed.
4. `tasqx_annotate_task` — write back what was done, decisions made, anything the
   next session needs.

`tasqx_add_check` puts acceptance criteria somewhere a completion can be held to
them: an annotation is prose and nothing can ask whether it was met, while a
check has a state. tasqx never runs one — the criterion is a claim and the
evidence a citation, both stored verbatim — so the agent marks them itself, or
names them in `checks_passed` on completion. Completing with a criterion still
open is not refused; it is counted, and `tasqx_outcomes` reports the count.

`tasqx_add_dependency` lets the agent decompose a feature itself: capture subtasks
with `tasqx_add_task`, wire the order, then work the chain.

## Give the agent memory

`tasqx memory import docs/` turns a directory of markdown docs — ADRs, runbooks,
company patterns — into a searchable knowledge store. `tasqx_brief_task` consults
it for you at the moment you pick a task up; `tasqx_search_memory` is for
consulting it mid-task (both work read-only, deliberately). Imported docs carry
no project, which makes them *global*: a brief scoped to one project still
surfaces them, and only another project's notes are held back. The
import is not recursive: a directory means its own `*.md` files, so run it once
per folder that holds docs rather than pointing it at the root of a tree. Task
annotations are searchable through the same tool, so decisions written down
during one task resurface during the next:

1. `tasqx_brief_task` before touching payment code — the query is derived from the
   task, so nothing is guessed.
2. Hits come back bm25-ranked with snippets — docs and past annotations alike.
   Half the slots are held for docs, so an imported ruling still reaches the page
   when a project's own task notes share its words; annotations take the rest.
   The brief's page defaults to five hits; a caller-named `memory_limit` widens it.
3. `tasqx_search_memory` with `"payment idempotency"` for anything the brief's
   scope did not reach.
4. After the work, `tasqx_add_memory` stores what the next session should know.

## Read how the last run went

`tasqx_outcomes` is the other half of measurement: `tasqx_summary` answers what
the work cost, this answers whether it worked. Over the tasks that **closed**,
it reports rework (completions that were later reopened), estimate calibration,
token cost, silent completions (no annotation written), abandoned work, token
overruns, unproven completions and forced completions (completions that
overrode open blockers) — each rate beside the `n` it was computed over,
because a rework rate over three completions is not evidence.

```console
tasqx report --outcomes project --since -30d
```

It is on the **read** scope, so a read-only agent has it, and it is worth a
call before a big piece of work on a project you have touched before: a high
silent count means the annotations your next session will search for are not
being written, and a high rework count means completions are being called too
early. A retrospective ([A self-improving agent](self-improving-agent.md)) that
cites these numbers is answering from evidence rather than from its own memory
of the session.

## Safety properties you get for free

- `tasqx_modify_task` is optimistic-concurrency-checked: the server pins the
  task's revision before writing, so if you edited the task in another shell
  mid-flight the agent gets a `conflict` and re-reads instead of clobbering.
  The other writes — complete, tag, annotate, timers, dependencies — carry no
  revision guard and are last-write-wins, which is why field edits belong on
  modify.
- There is deliberately no bulk-delete tool. Cancelling goes through the same
  reversible, logged path as everything else.
- Every agent action lands in the append-only event log, so `tasqx chart` and the
  history are as true for agent work as for yours.
