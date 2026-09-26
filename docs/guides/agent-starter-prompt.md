# Giving an agent memory in any client

The `initialize` result now carries an `instructions` field: a condensed form of the
block below, so a host that surfaces server instructions — Claude Code and others do —
gets the nudge with no setup at all. The paste block below still earns its place for
three reasons: hosts that ignore `instructions`, or truncate it; the fuller version here,
which carries the reasons behind each rule and not just the rule; and the placeholders
you tune to your setup — the import directory, and cutting the write half for a
read-only server.

The block below is the full nudge, written so it works anywhere the tools are: `CLAUDE.md`,
`AGENTS.md`, `.cursorrules` or `.cursor/rules/`, a Zed rules file, a Codex instructions
file, or the system prompt of a bare MCP host. It covers the backlog as well as memory,
because the two are one store: an annotation written while working a task is what the
next session's search finds. Paste it whole. The searching half works under a read-only
server; the storing and tracking halves need `tasqx mcp serve --scope write`, which
[Driving tasqx from an AI agent](ai-agent-workflow.md) argues you grant after watching a
read-only session rather than before — until then the block tells the agent to hand you
the text it could not store.

## Paste this

```markdown
## tasqx is your organiser: your backlog and your long-term memory

tasqx holds a backlog of tasks and one searchable index over two things: knowledge
docs, and the annotations written on tasks. Reach it over MCP (`tasqx_*` tools) and,
for the verbs MCP does not carry (`next`, `undo`, `memory import`, `report --html`),
the `tasqx` CLI — always spell the verb (`tasqx list`, never a bare `tasqx`, which opens
a full-screen dashboard and hangs a tool call). Use it instead of in-conversation todo
lists and instead of any other memory file.

Reading and writing are equally mandatory — an agent that only searches leaves the
store as empty as it found it. If `tasqx_add_memory` and `tasqx_annotate_task` are
missing from your tool list the server is read-only: say so once, keep searching, and
put what you would have stored into your reply. Never silently drop the write. If no
`tasqx_*` tool responds and the CLI is missing too, say so and track the work in the
conversation — do not pretend it was recorded.

### Search before you decide

Call `tasqx_search_memory` before you:

- resume work, or answer "where were we"
- choose between two designs, libraries, schemas or file layouts
- touch a convention-bearing file (config, CI, migration, public API)
- contradict something that looks deliberate, or answer "why is it built this way"
- assert that something is "how this project does it"

Search first, form the opinion second. Report what you found; if a decision contradicts
a hit, say which one and why.

Query with two or three keywords, not a sentence: every word of a plain query is
required first, so `retry idempotency` and `tokens.css` work as typed. When no entry has
every word, entries with any of them (filler words like "the" aside) come back instead,
marked `relaxed: true` — a sentence mostly returns noise that way. The result's `matched` field shows the expression
that actually ran.
Run two searches with different wording before concluding nothing is there; no tool
lists the store, so searching is the only way in. A hit is a snippet, not the document:
read a doc whole with `tasqx_get_memory` on its `id`, and a hit whose `source` reads
`task:#<id>` is an annotation — read that task with `tasqx_get_task` before acting on it.

### Track multi-step work as tasks

A real unit of work — a feature, anything multi-step, anything that could outlive this
session or block something else — goes in the backlog. A question, a single command or
a one-line fix does not; do it and move on.

- One project per repo or initiative. Check `tasqx_list_projects` before
  `tasqx_create_project`; name it after the repo so every session finds the same one.
- Decompose with `tasqx_add_task` (title, project, priority H/M/L, estimate) and order
  the pieces with `tasqx_add_dependency`. Acceptance criteria go in `tasqx_add_check`,
  where something can ask at completion time whether they were met; other context goes
  in an annotation, not the title.
- Pick work from `tasqx_list_tasks` with filter `project:<name> @working`. Blocked and
  waiting tasks are hidden there by design: an empty working set with open tasks means
  everything is blocked, not that the work is gone. The default row is compact — short_id,
  title, status, priority, urgency, blocked, due, project, tags — so pass `fields` for
  any other column, or `fields: []` for the whole row.
- Per task: `tasqx_brief_task` for everything you need before starting — the task, what
  each prerequisite concluded, and relevant memory under a query tasqx derives, so you
  supply no search terms. Then `tasqx_start_timer`, do the work, annotate the outcome,
  and `tasqx_complete_task` naming in `checks_passed` what you actually proved. Its
  `unblocked` result names what to pick up next; pass token counts when you know them.
- Work that became obsolete is cancelled (`tasqx_cancel_task`, or `tasqx cancel <id>`),
  never left open. Nothing in the backlog is hard-deleted.

### Day one: import what already exists

Memory starts empty, and an empty store makes the search rule a no-op — if searches
keep returning nothing, it was never seeded. From the repo root, once per folder that
holds markdown docs:

    tasqx memory import [YOUR DOCS DIRECTORY]

No MCP tool imports; run it in the shell, or ask me to run it. It is non-recursive — a
directory means its own `*.md` files. Re-running replaces docs from the same source
rather than duplicating, keyed on each file's path as you spelled it, so spell the
directory the same way every time.

### Store what the next session needs

Call `tasqx_add_memory` (`title`, `body`, `source`) when a decision is made and we stop
arguing about it, when you learn a convention written down nowhere, when a session ends
with work in flight, and when you get something wrong and find out why. Record the
ruling *and* the why; a ruling without its reason gets reopened. Write for retrieval:
the words a future search will use in the title, the ruling in the first line, the
reasoning under it, and the repo or path in `source`. Do not store what is already a
file in the repo — import it instead — and do not store transcripts, task lists or
progress narration; the backlog and its annotations already hold those.

When an entry turns out to be wrong, retract it rather than filing a correction beside
it: search ranks both with nothing to say which is true. Read it with
`tasqx_get_memory`, then remove it with `tasqx_remove_memory` on that id and add the
corrected entry. Removal is permanent — `tasqx undo` does not cover memory — so remove
only what you have read, and ask first if you did not write it.

### Annotate as the work happens

`tasqx_annotate_task` (`ref`, `body`) stores the body verbatim and indexes it into the
same store `tasqx_search_memory` reads — not bookkeeping, but how the next session finds
this work at all. Annotate the approach when you start, every decision, blocker or
change of direction as it lands, and what was delivered, measured and deliberately
skipped before `tasqx_complete_task`. Only the body is indexed, never the task title, so
name the files, symbols and terms inside the note.
```

## Tune it

One placeholder, in the import command: the directory holding your markdown. Fill it
before the first paste — an unfilled import command fails rather than importing
something wrong. In a user-level file that applies to every repo, such as
`~/.claude/CLAUDE.md`, there is no one directory to name; replace the placeholder with
the instruction to find the repo's markdown folders and ask before importing them.

Running the server read-only on purpose? Cut the tracking, storing and annotating
sections and keep the search half, which works read-only by design. A store the agent
only reads is thinner than one it maintains, and still worth the paste.

Import is the bulk load; `tasqx_add_memory` in a loop is not.

## The one way re-importing bites

"Spell the directory the same way every time" is a real instruction, not politeness. The
source key is the file path as it was handed in, so two spellings of one directory are
two documents:

    tasqx memory import ~/notes        # 1 hit
    tasqx memory import ~/notes/       # still 1 hit — the trailing slash collapses
    tasqx memory import ~/./notes      # 2 hits, and both are the same file

The middle line is why this is worth spelling out: the obvious variation is harmless, so
the habit that would catch the harmful one never forms. Nothing warns you, and the
duplicate is invisible until a search returns the same paragraph twice under two paths.
`tasqx memory search` prints the source path with each hit, which is how you spot it,
and `tasqx_remove_memory` (or `tasqx memory rm <id>`) on the wrong-spelling copy is the
fix.

## Where this stops

A pasted prompt is a suggestion the client re-reads at its own discretion, so it will
not survive an aggressively compacted context the way a tool description does. In Claude
Code the skill at [`.claude/skills/tasqx-workflow/SKILL.md`](../../.claude/skills/tasqx-workflow/SKILL.md)
covers the backlog loop in more depth and loads on demand instead of sitting in context;
it carries no trigger list for when to search and no read-only fallback, so keep those
two and let the skill do the rest. Read
[Driving tasqx from an AI agent](ai-agent-workflow.md) first for the wiring, the work
loop and the safety properties this file assumes you already have.
