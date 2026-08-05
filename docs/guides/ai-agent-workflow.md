# Driving tasqx from an AI agent

tasqx ships an MCP server, so an agent reads and mutates your tasks through the same
core API your shell uses. No glue code, no scraping `--json` output.

## Wire it up

For Claude Code, one line:

```console
claude mcp add tasqx -- tasqx mcp serve --scope write
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

```console
tasqx list                    # the verb always means the table
TASQX_DASHBOARD=false tasqx   # or switch the screen off for the whole image
```

The MCP server above is unaffected: it speaks JSON-RPC over pipes, so it is on
the non-interactive side by construction.

## The loop an agent runs

The tool surface is designed around one loop — work the backlog one task at a time:

1. `tasqx_list_tasks` with `"project:myapp.checkout"` — see the feature, blocked
   tasks marked.
2. `tasqx_get_task` — read the annotations: acceptance criteria, links, context.
   The answer is two content blocks: tasqx-rendered markdown first, then the raw
   JSON. The markdown is the intended reading — layout is tasqx's job, so the
   agent uses it as-is rather than recomposing the detail from JSON — and the
   `detail.time_format` config key (`iso`, `relative` or `both`) decides how it
   writes timestamps.
3. `tasqx_start_timer`, do the work, `tasqx_complete_task` — the completion result
   names any tasks it unblocked, which is the agent's cue for what to pick up next.
   Pass the turn's token counts on completion (`input_tokens`, `output_tokens`,
   `cache_read_tokens`, `cache_creation_tokens`): the agent is the only party
   that knows which task the spend served, so self-report is the primary
   measurement channel — completing without counts earns a `tokens_hint` in the
   response saying exactly that.
4. `tasqx_annotate_task` — write back what was done, decisions made, anything the
   next session needs.

`tasqx_add_dependency` lets the agent decompose a feature itself: capture subtasks
with `tasqx_add_task`, wire the order, then work the chain.

## Give the agent memory

`tasqx memory import docs/` turns a directory of markdown docs — ADRs, runbooks,
company patterns — into a searchable knowledge store, and `tasqx_search_memory`
lets the agent consult it mid-task (it works even read-only, deliberately). The
import is not recursive: a directory means its own `*.md` files, so run it once
per folder that holds docs rather than pointing it at the root of a tree. Task
annotations are searchable through the same tool, so decisions written down
during one task resurface during the next:

1. `tasqx_search_memory` with `"payment idempotency"` before touching payment code.
2. Hits come back bm25-ranked with snippets — docs and past annotations alike.
3. After the work, `tasqx_add_memory` stores what the next session should know.

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
