# A self-improving agent: a session-end retrospective

Every completed task is a moment where the agent knows what worked and what
did not: the edit it reverted, the command it re-ran twice, the convention it
found out the hard way. By the next session all of it is gone, unless
something writes it down where `tasqx_search_memory` can find it again.
tasqx gives the agent two ways to do that: a retrospective block on every
completion, and an optional skill for a closer look at the end of a session.

This guide assumes tasqx is already wired into your agent over MCP. If it is
not, start with [Driving tasqx from an AI agent](ai-agent-workflow.md).

## The retro block

On a write-scope connection, the result of `tasqx_complete_task` carries a
`retro` block of about 1 KB. It holds six questions, the routing rules and a
retraction step, so any MCP client runs the retro with no hook and no skill
installed:

1. Did the user correct or redirect you?
2. Goal versus delivered?
3. Rework?
4. A discovery written down nowhere?
5. Would you redo anything?
6. A procedure others will repeat?

A task that went straight through answers in one line (`straight through,
nothing to record`) and stops. Anything worth keeping goes in a `retro:`
annotation on the task. Each correction from the user becomes a `standing:
true` memory doc; the write-scope instructions tell the agent to store it the
moment it lands. Before adding a ruling, the agent searches memory for a doc
on the same subject and, if the new one supersedes it, removes the old one with
`tasqx_remove_memory`, so two contradictory docs never stand side by side.

Pass `include_retro: false` to leave the block out, for a batch of completions
that already ran one retro. A read-scope connection cannot complete tasks and
never receives the block.

## The retro skill

The `retro` skill is an optional convenience for Claude Code, Codex and Gemini.
Nothing triggers it automatically: run `/retro` at the end of a session, or
`/retro 123` for one task. It looks at every task the session completed, but
only writes about the ones that had rework, an undocumented discovery, a
blocked or refused step, or an executor whose result came back wrong. A task
that went straight through is named as such and gets no annotation.

For each task that qualifies, the session assembles a small evidence bundle:

1. The task read whole: `tasqx_get_task` with `include_json: false` and
   `annotations_limit` set to the task's `annotations_total`. A long history is
   still cut to the response budget; a non-null `annotations_next_offset`
   means there is more, so page until it is null.
2. The executor's final report for that task, verbatim.
3. `tasqx_outcomes` for the task's project.
4. Two or three lines on anything the planning session itself reworked.

It hands the bundles to a fresh `general-purpose` agent, not a fork, so the
cost does not grow with the length of the session. That agent answers five
questions per task (goal versus delivered, rework, discovery, redo,
procedure), routes each finding, and writes the `retro:` annotation. A fact
goes to memory after a search shows it is not already there. A prohibition is
named to you as a hook candidate; creating the hook is your call. A procedure
must pass two gates, procedural and non-obvious, and is promoted to a skill
only on its second sighting.

Install it with:

```console
tasqx setup --only retro
```

The skill text is compiled into tasqx, so each release installs the version
that matches it; the source is
[`.claude/skills/retro/SKILL.md`](../../.claude/skills/retro/SKILL.md) in the
repository. With Claude Code
on your PATH it installs `~/.claude/skills/retro`; with Codex or Gemini it also
installs `~/.codex/skills/retro` and `~/.gemini/skills/retro` (`--only
codex:retro`, `--only gemini:retro`). A `SKILL.md` that differs from the
bundled one, your own edit or an older copy, is kept;
`tasqx setup --yes --only retro --force` replaces it.

## What accumulates, and how to read it

| What | Where it lands | How to find it |
|---|---|---|
| What a task taught | A `retro:` annotation on that task | `tasqx_get_task`, or `tasqx show <id>` |
| Facts and conventions | Memory docs | `tasqx_search_memory`, or `tasqx memory search` |
| Procedures | Tasks titled `skill candidate: …`, tagged `skill-candidate` | `tasqx list +skill-candidate` |

The agent finds the first two by itself: both live in the store
`tasqx_search_memory` covers, so the next task on the same code reads last
time's annotation ([Memory](../wiki/Memory.md) explains the ranking). Candidates
are the part you look at. On a second sighting the skill drafts a real
`SKILL.md`, records the path on the candidate and cancels it; list the open
ones now and then, and promote one by hand when you already know it recurs.

## Other clients

A client without a skills mechanism needs nothing more: the block arrives with
every completion. To run the longer procedure there, put the five questions,
the scope gate and the routing rules from
[`.claude/skills/retro/SKILL.md`](../../.claude/skills/retro/SKILL.md) into the
agent's system prompt, next to the block from
[Giving an agent memory in any client](agent-starter-prompt.md), and tell it to
run them at the end of a session against the same evidence bundle.
