# AI Agents and Automation

tasqx treats an AI agent as a normal user. There's one JSON API underneath
everything; the CLI is one client of it, and the built-in MCP server is
another. An agent gets the same data, the same rules and the same safety
properties you do.

## tasqx mcp

The built-in [MCP](https://modelcontextprotocol.io) server — the standard way
AI tools like Claude Code connect to external systems.

```console
tasqx mcp serve                  # read-only by default
tasqx mcp serve --scope write    # explicit write access
```

Wiring it into Claude Code is one line:

```console
claude mcp add tasqx -- tasqx mcp serve --scope write
```

Any other MCP client takes the same shape in its config:

```json
{
  "mcpServers": {
    "tasqx": { "command": "tasqx", "args": ["mcp", "serve", "--scope", "write"] }
  }
}
```

What the agent can do: list and read tasks, add and modify them, complete
them (and learn what that unblocked), start and stop timers, tag, annotate,
wire up dependencies, create projects, and search and store
[memory](Memory.md).

Safety properties worth knowing:

- **Read-only sessions can't see the write tools at all** — an agent can't
  call what it isn't offered. (Scope configures the local process; it's not
  authentication.)
- **There's no bulk-delete tool, on purpose.** Cancelling goes through the
  same reversible, logged path as everything else, so an agent can't quietly
  destroy a week of work.
- The one permanent delete (`remove_memory`, for retracting a wrongly stored
  document) says so in its own description, so the host's confirmation gate
  applies.

For the full workflow — what deserves a backlog entry, searching memory before
starting, annotating before completing — see the
[AI agent guide](../guides/ai-agent-workflow.md) and the
[agent starter prompt](../guides/agent-starter-prompt.md).

## tasqx api

The JSON API without a server: one request envelope in on stdin, one response
out on stdout.

```console
echo '{"tasqx":"1","method":"task.list","params":{"filter":"@working"}}' | tasqx api
```

Every method the engine has is callable this way — `tasqx docs` carries the
full method table. Exit codes mirror the error model: 0 ok, 2 bad request,
4 not found, 5 conflict.

## tasqx daemon

A long-lived server on a local socket (or named pipe on Windows). You don't
need it for everyday use — every command works standalone — but it adds three
things:

```console
tasqx daemon
```

1. **One writer, many clients.** While it runs, ordinary commands
   automatically route through it, so several agents and terminals can hammer
   the same store without stepping on each other.
2. **Live pushes.** Every change is announced to subscribers — that's what
   [`tasqx watch`](Dashboard-and-Live-View.md#tasqx-watch) listens to.
3. **Reminders fire.** The daemon is the process that watches the clock; see
   [Dates, Reminders and Recurrence](Dates-Reminders-and-Recurrence.md#reminders).

Ctrl-C stops it cleanly. `--no-daemon` on any command skips the routing when
you need a command to run strictly in-process.

## tasqx tokens

Maintenance for token accounting — the feature that answers "what did agent
work on this backlog actually cost?"

Agents self-report token counts when completing a task (that's the primary
channel — only the agent knows which task a conversation served). A
log-parsing fallback fills gaps by reading session transcripts, and it
*refuses* to guess: a sample claimed by two tasks' time windows is dropped
rather than attributed to the wrong one.

```console
tasqx tokens recompute            # dry run: shows what would change, writes nothing
tasqx tokens recompute --apply    # actually rewrite the log-parse attributions
```

Stop any running daemon before `--apply` — this one runs strictly in-process.

Where the numbers surface: the task table's TOKENS column, the dashboard's
TOKENS panel, and the HTML report's header tiles — always as separate buckets
(input, output, cache read, cache creation), never a misleading blended total.

### Token accounting

When an agent (or script) drives the CLI instead of MCP, the same reporting
travels as flags on `start` and `done`:

```console
tasqx start 42 --client 'claude-code 2.1' --session-id $SID
tasqx done 42 --client 'claude-code 2.1' --session-id $SID
```

`--client` selects the transcript parser; without these flags no transcript is
ever read and the task simply reports zero tokens.

## One rule for anything automated

A bare `tasqx` on what looks like an interactive terminal opens the
full-screen [dashboard](Dashboard-and-Live-View.md#tasqx-dashboard) and waits
for a key. Harnesses that allocate a pty look interactive. So in scripts and
agent tooling, always spell the verb: `tasqx list`, never bare `tasqx`.
