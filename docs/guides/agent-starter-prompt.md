# Giving an agent memory in any client

The MCP server tells an agent what it *can* call and nothing tells it *when*. The
`initialize` result carries no instructions field, and the `tasqx_search_memory`
description says what the tool searches, not when to reach for it — so a fresh install
has no in-band nudge toward memory at all. What makes memory actually get used today is
a client-specific instructions file, and those do not travel between clients.

The block below is that nudge, written so it works anywhere the tools are: `CLAUDE.md`,
`AGENTS.md`, `.cursorrules` or `.cursor/rules/`, a Zed rules file, a Codex instructions
file, or the system prompt of a bare MCP host. Paste it whole and fill the one
placeholder. The searching half works under a read-only server; the storing half needs
`tasqx mcp serve --scope write`, which [Driving tasqx from an AI agent](ai-agent-workflow.md)
argues you grant after watching a read-only session rather than before — until then the
block tells the agent to hand you the text it could not store.

## Paste this

```markdown
## tasqx is your long-term memory

tasqx holds one searchable index over two things: knowledge docs, and the annotations
written on tasks. Reach it over MCP (`tasqx_*` tools) and, for the verbs MCP does not
carry, the `tasqx` CLI. Reading it and writing to it are equally mandatory — an agent
that only searches leaves the store as empty as it found it. If `tasqx_add_memory` and
`tasqx_annotate_task` are missing from your tool list the server is read-only: say so
once, keep searching, and put what you would have stored into your reply. Never
silently drop the write.

### Search before you decide

Call `tasqx_search_memory` before you:

- resume work, or answer "where were we"
- choose between two designs, libraries, schemas or file layouts
- touch a convention-bearing file (config, CI, migration, public API)
- contradict something that looks deliberate, or answer "why is it built this way"
- assert that something is "how this project does it"

Search first, form the opinion second. Report what you found; if a decision contradicts
a hit, say which one and why.

Query with two or three keywords, not a sentence: plain words are matched as quoted
phrases joined by AND, so `retry idempotency` and `tokens.css` work as typed while a
sentence has to match every one of its words at once and usually returns nothing. Run
two searches with different wording before concluding nothing is there — no tool lists
the store, so searching is the only way in. A hit whose `source` reads `task:#<id>` is
an annotation: read the whole task with `tasqx_get_task` before acting on the snippet.

### Day one: import what already exists

Memory starts empty, and an empty store makes the rule above a no-op — if searches keep
returning nothing, it was never seeded:

    tasqx memory import [YOUR DOCS DIRECTORY]

No MCP tool imports; run it in the shell, or ask me to run it. It is non-recursive — a
directory means its own `*.md` files — so run it once per folder that holds docs.
Re-running replaces docs from the same source rather than duplicating, keyed on each
file's path as you spelled it, so spell the directory the same way every time.

### Store what the next session needs

Call `tasqx_add_memory` (`title`, `body`, optional `source`) when a decision is made and
we stop arguing about it, when you learn a convention written down nowhere, when a
session ends with work in flight, and when you get something wrong and find out why.
Record the ruling *and* the why; a ruling without its reason gets reopened. Write for
retrieval: the words a future search will use in the title, the ruling in the first
line, the reasoning under it. Do not store what is already a file in the repo — import
it instead — and do not store transcripts, task lists or progress narration.

`tasqx_add_memory` appends, it never replaces. To correct an entry, add one that names
what it supersedes, and ask before running `tasqx memory rm <id>` on the stale one — it
is the only verb here that removes anything (removal is CLI-only, by the id search
printed).

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
something wrong.

Running the server read-only on purpose? Cut the `tasqx_add_memory` and
`tasqx_annotate_task` sections and keep the search half, which works read-only by
design. A store the agent only reads is thinner than one it maintains, and still worth
the paste.

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
and `tasqx memory rm <id>` on the wrong-spelling copy is the fix.

## Where this stops

A pasted prompt is a suggestion the client re-reads at its own discretion, so it will
not survive an aggressively compacted context the way a tool description does. In Claude
Code the skill at [`.claude/skills/tasqx-workflow/SKILL.md`](../../.claude/skills/tasqx-workflow/SKILL.md)
covers this loop with the backlog discipline attached and loads on demand instead of
sitting in context; it carries no trigger list for when to search and no read-only
fallback, so keep those two and let the skill do the rest. Read
[Driving tasqx from an AI agent](ai-agent-workflow.md) first for the wiring, the work
loop and the safety properties this file assumes you already have.
