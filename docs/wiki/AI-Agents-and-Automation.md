# AI Agents and Automation

tasqx treats an AI agent as a normal user. There's one JSON API underneath
everything; the CLI is one client of it, and the built-in MCP server is
another. An agent gets the same data, the same rules and the same safety
properties you do.

## tasqx setup

Connects tasqx to Claude Code in one step: it registers the MCP server (user
scope, write access) and installs the `tasqx-workflow` and `retro` skills.

```console
tasqx setup
```

It's a checklist screen — Space toggles an item, Enter installs what's
ticked, q quits. Run it again later and it shows what's already in place.
Without a terminal it prints that list and exits.

- `tasqx setup --list`: what is installed, per item
- `tasqx setup --yes`: install everything not yet present, no screen
- `tasqx setup --only retro`: limit to one item (`mcp`, `tasqx-workflow`, `retro`); repeatable
- `tasqx setup --yes --force`: also overwrite a skill file that differs from the one tasqx carries

The skill text is compiled into tasqx, so an upgrade brings new skill text
with it. A skill file you edited is kept unless you pass `--force` or tick it
on the screen.

## tasqx mcp

The built-in [MCP](https://modelcontextprotocol.io) server — the standard way
AI tools like Claude Code connect to external systems.

- `tasqx mcp serve`: read-only by default
- `tasqx mcp serve --scope write`: explicit write access

```console
tasqx mcp serve
tasqx mcp serve --scope write
```

To wire it into Claude Code by hand, without the skills that
[`tasqx setup`](#tasqx-setup) adds:

```console
claude mcp add --scope user tasqx -- tasqx mcp serve --scope write
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

Thirty tools, one verb each. Nine reads: `list_tasks`, `get_task`,
`brief_task`, `summary`, `outcomes`, `list_projects`, `search_memory`,
`get_memory`, `list_memory`.
Twenty-one writes: `add_task`, `modify_task`, `complete_task`, `reopen_task`,
`cancel_task`, `start_timer`, `stop_timer`, `tag_task`, `untag_task`,
`annotate_task`, `update_annotation`, `remove_annotation`, `add_check`, `set_check`,
`remove_check`, `add_dependency`, `remove_dependency`, `add_memory`,
`update_memory`, `remove_memory`, `create_project` (all prefixed `tasqx_`).

What makes this more than remote CRUD:

- **One call before starting, instead of five.** `brief_task` returns the task,
  each prerequisite with **what that task concluded**, what this one blocks, and
  relevant memory — under a query tasqx derives from the task's own title, tags
  and project. The agent supplies no search terms, which matters because a
  guessed term that finds nothing looks exactly like a store with nothing in it.
- **Completing a task returns what it unblocked**, so an agent can decompose a
  feature into a dependency chain with `add_dependency` and then walk it,
  picking up each task the moment its prerequisites clear.
- **"Done" can mean something.** Acceptance criteria are rows with a state and a
  citation (`add_check`, `set_check`), not prose in an annotation that nothing
  can ask about. tasqx never *runs* a check — the criterion is a claim and the
  evidence is text it stores verbatim. Completing with one still open is not
  refused; it is counted.
- **The agent can read its own record.** `outcomes` is on the read scope, so
  even a read-only session can ask what its rework rate on this project is
  before deciding how carefully to work.
- **Agents get long-term memory.** `search_memory` gives even a read-only
  agent bm25-ranked retrieval over your imported docs *and* every task
  annotation — feed it your ADRs with `tasqx memory import docs/`, and past
  decisions surface while it works. Annotations feed the same index, so an
  agent that documents its work is building the knowledge base as a side
  effect.
- **Token spend lands on the task.** `complete_task` takes token counts plus
  who spent them; a log-parse fallback fills gaps and refuses contested
  samples rather than guess. The table, the dashboard and the HTML report all
  show what each task cost.

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
- **`initialize` answers with `instructions`,** a scope-aware workflow the
  host may inject into the agent's system prompt; under read-only scope it
  names no write tool. It also carries the session's standing memory docs,
  for the project named by the directory `mcp serve` runs in (or one of its
  parents), else the default project, within a 3 KB budget.

`tasqx_get_task` and `tasqx_brief_task` take a `view` argument, `"markdown"`
by default. Pass `view: "card"` when a person has to decide on the task — one
the agent proposes adding, or one they asked about: the rendered block becomes
a fixed-width, box-drawn card meant to be pasted whole into a chat reply or a
document, instead of the usual prose. The same card is available from a
shell: `tasqx show 42 --card` prints the identical bytes.

Routine events do not need a card. Starting or completing a task is one line
the agent writes from what it already read — the brief and the completion's
`unblocked` list:

```text
▶ #711 Add unc output style · M · 20m · 0/2 checks
✔ #711 done · 2/2 checks · unblocked #712
```

A subagent handed a task runs `tasqx show <id> --card` itself instead of
receiving the card pasted into its brief.

Both tools answer the rendered view and nothing else. The machine-readable
JSON block is the same result a second time, so it is sent only when you ask
for it with `include_json: true` — worth doing when a script parses the
answer, and a waste of the response budget when an agent is going to read it.

A few more places the response is shaped for the reader that is actually
paying for it:

- **`tasqx_list_tasks`** answers a compact row by default — `short_id`,
  `title`, `status`, `priority`, `urgency`, `blocked`, `due`, `project`,
  `tags` — with any null key omitted; an explicit `fields` list, `[]`
  included, returns the engine's own full row.
- **`tasqx_complete_task`** refuses a task with open blockers and names them;
  `force: true` completes it anyway, and the override is recorded and counted
  in `tasqx_outcomes`. Pass `view: "card"` when a person has to decide on
  the outcome: the answer then leads with the box card of the task as
  completed, so no `tasqx_get_task` re-read is needed. A routine completion
  leaves it out and prints the one `✔` line above.
- A long annotation body is cut in a `tasqx_get_task`/`tasqx_brief_task`
  response with a marker naming its real size; `max_body_bytes` raises the
  cap, and a removed annotation is listed as a tombstone rather than
  disappearing from the count.
- `tasqx_brief_task` holds half its memory page for knowledge docs (from
  `memory add` / `memory import`), so a ruling is not buried under a
  project's own, more numerous, task annotations.

### Why the tool descriptions are short

Every tool description states its contract — what the call does, what it
returns, and the one rule you need to call it right — and cites a `D`-number
when a design decision is behind that rule. The reasoning for the rule is not
in the description: it is in `DESIGN.md` §12 under that number, and
`tasqx_search_memory` finds it once the repository's docs have been imported
with `tasqx memory import`. The descriptions are kept short because they are
not free. Some clients fetch a tool's schema on first use; most inject the
whole roster into every request, where it costs on the order of thirty
kilobytes — a few thousand tokens — on every prompt. A guard test in the test
suite holds that bound, and a `tools/list` handshake against your own build
measures it exactly.

A few facts the short descriptions no longer spell out, that you may still
want to act on:

- **`annotations_limit` and `include_json` spend the same budget.** A
  `tasqx_get_task` response is fitted to a byte budget, and the machine-readable
  JSON block is charged first. A big annotation page together with
  `include_json: true` therefore buys less history than the page you asked for.
- **`budget_tokens` stops nothing.** tasqx is called between turns and cannot
  interrupt one. It counts fresh tokens only — input, output and cache
  creation, never cache reads — so a task that blows its budget is a signal it
  was too large to hand over whole, not an error.
- **`tasqx_start_timer` takes `actor` and `keep`.** Each MCP connection gets
  its own actor id, and starting a task while a *different* actor holds a
  running clock is refused rather than silently stopping their timer. Pass
  `keep: true` to run both deliberately — which is what several agents working
  in parallel need.
- **`tasqx_remove_annotation` redacts the event too.** It overwrites the body
  in the annotation row *and* in the original write's audit event, in one
  transaction, so a secret pasted into a note is also gone from an export.
  `tasqx undo` cannot bring it back.
- **`tasqx_update_annotation` corrects a note in place.** The id, timestamp and
  position stay, so a corrected first note is still the card's Description,
  and memory search stops finding the old sentence. `tasqx undo` puts the
  previous text back.

The server's `instructions` block is the short version of how to work, so a
fresh install already nudges the agent to search before deciding and to write
as it works. The skill in
[`.claude/skills/tasqx-workflow/`](../../.claude/skills/tasqx-workflow/SKILL.md)
— `tasqx setup` installs it for you, or Claude Code picks it up automatically
inside the tasqx repository — is the fuller version.

For the full workflow — what deserves a backlog entry, searching memory before
starting, annotating before completing — see the
[AI agent guide](../guides/ai-agent-workflow.md) and the
[agent starter prompt](../guides/agent-starter-prompt.md). For the session-end
retrospective that has the agent record what each task taught, see
[A self-improving agent](../guides/self-improving-agent.md).

## tasqx api

The JSON API without a server: one request envelope in on stdin, one response
out on stdout.

```console
echo '{"tasqx":"1","method":"task.list","params":{"filter":"@working"}}' | tasqx api
```

Every method the engine has is callable this way — `tasqx docs` carries the
full method table. Exit codes mirror the error model: `0` ok, `2` bad request,
`4` not found, `5` conflict, `6` unsupported API version — plus `1` for
`internal`, or for a failure beneath the request such as a store that would
not open.

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

| Command | What it does |
|---|---|
| `tasqx tokens recompute` | Dry run: shows what would change, writes nothing |
| `tasqx tokens recompute --apply` | Actually rewrite the log-parse attributions |

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
