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

## The loop an agent runs

The tool surface is designed around one loop — work the backlog one task at a time:

1. `tasqx_list_tasks` with `"project:myapp.checkout"` — see the feature, blocked
   tasks marked.
2. `tasqx_get_task` — read the annotations: acceptance criteria, links, context.
3. `tasqx_start_timer`, do the work, `tasqx_complete_task` — the completion result
   names any tasks it unblocked, which is the agent's cue for what to pick up next.
4. `tasqx_annotate_task` — write back what was done, decisions made, anything the
   next session needs.

`tasqx_add_dependency` lets the agent decompose a feature itself: capture subtasks
with `tasqx_add_task`, wire the order, then work the chain.

## Safety properties you get for free

- Every mutation is optimistic-concurrency-checked: if you edited a task in another
  shell mid-flight, the agent gets a `conflict` and re-reads instead of clobbering.
- There is deliberately no bulk-delete tool. Cancelling goes through the same
  reversible, logged path as everything else.
- Every agent action lands in the append-only event log, so `tasqx chart` and the
  history are as true for agent work as for yours.
