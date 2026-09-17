# A self-improving agent: a session-end retrospective

Every completed task is a moment where the agent knows what worked and what
did not: the edit it reverted, the command it re-ran twice, the convention it
found out the hard way. By the next session all of it is gone, unless
something writes it down where `tasqx_search_memory` can find it again.

Until 2026-09-16 that something was a hook: it fired after every
`tasqx_complete_task` and told the agent to run a retrospective for the task
it had just finished, in a `fork` subagent that inherited the whole session
transcript. It worked, but it was expensive and mostly wasted — each run cost
roughly 56,000 tokens, fourteen completions in thirty-six hours cost about
800,000, and a later measurement found that the memory docs those retros
wrote were never read back by anything. The hook script and its settings.json
entry are gone.

What replaced it is a skill, `retro`, that runs once per session — on
request, not on every completion — over whichever of the session's completed
tasks actually earned a closer look.

## When it runs

`/retro` at the end of a session, or `/retro 123` for one task. Nothing
triggers it automatically; completing a task no longer starts anything.

Given no task number, the skill considers every task the session completed,
but only *writes* about the ones that had rework (a reverted edit, a re-run
command, a wrong assumption), a discovery written down nowhere, a blocked or
refused step, or an executor whose result came back wrong. A task that went
straight through gets named as such in the verdict and no annotation.

## The evidence bundle

The old hook's fork read the transcript because that was the only evidence it
had. The skill instead assembles a bundle per qualifying task, small enough
to hand to a **fresh** agent rather than a fork:

1. The task itself, read whole — `tasqx_get_task` with `include_json: false`
   and `annotations_limit` set to the task's own `annotations_total`. That
   asks for the whole history; a long one is still cut to the response
   budget, and the answer says so with a non-null `annotations_next_offset`,
   so page from there until it is null (or raise `max_body_bytes` when one
   long body is what was cut) before calling the bundle complete.
2. The executor's final hand-back report for that task, pasted verbatim.
3. `tasqx_outcomes` for the task's project, once per project.
4. Two or three lines on anything the planning session itself reworked or
   reverted, since a fresh agent cannot see that unless it is written here.

A fresh agent, not a fork, because a fork's cost grows with the whole session
transcript — measured at 247k, 494k and 559k tokens for three real runs —
while the bundle above is a few thousand tokens regardless of how long the
session ran. The session calls the `Agent` tool once with
`subagent_type: "general-purpose"` and the prompt "Run the retro skill for
tasks #N, #M" followed by every bundle, and relays the agent's verdict back in
a sentence or two per task.

This guide assumes tasqx is already wired into Claude Code over MCP. If it is
not, start with [Driving tasqx from an AI agent](ai-agent-workflow.md).

## The skill

Install it with:

```console
tasqx setup --only retro
```

The skill text is compiled into tasqx, so each release installs the version
that matches it — read it at
[`.claude/skills/retro/SKILL.md`](../../.claude/skills/retro/SKILL.md). A
`~/.claude/skills/retro/SKILL.md` that differs from it, your own edit or an
older copy, is kept; `tasqx setup --yes --only retro --force` replaces it. It
runs once per session, on request (`/retro`), over every task the session
completed — not after each `tasqx_complete_task`. Until 2026-09-16 a
PostToolUse hook nudged a retro per completion; each run cost ~56k tokens, 14
completions in 36 h cost ~800k, and a measurement (memory doc
01a0a754-e116) found the docs those retros wrote were never read back. Decided
in claude-config #675: one run, one agent, one bundle per task that earned it,
and nothing written on a task that went straight through. Its evidence is an
evidence bundle the main session assembles from tasqx and the executors'
reports; the session transcript is not read. Its outputs are an annotation on
each reviewed task, and optionally a memory entry, a hookify recommendation,
or a skill candidate. A skill is promoted only on the second sighting of the
same procedure — retros overrate their own novelty.

The skill picks the tasks to review, assembles an evidence bundle per task
(the task read whole, the executor's report, `tasqx_outcomes`, any unannotated
rework), delegates to a fresh agent rather than a fork so cost does not grow
with the session, answers five questions per task (goal vs delivered, rework,
discovery, redo, procedure), and routes each finding — a fact to memory, a
mistake to a hookify candidate, a procedure through a skill-worthiness test
that only promotes on a second sighting — before writing a `retro:` annotation
on the task.

## What accumulates, and how to read it

| What | Where it lands | How to find it |
|---|---|---|
| What a task taught | A `retro:` annotation on that task | `tasqx_get_task`, or `tasqx show <id>` |
| Facts and conventions | Memory docs | `tasqx_search_memory`, or `tasqx memory search` |
| Procedures | Tasks titled `skill candidate: …`, tagged `skill-candidate` | `tasqx list +skill-candidate` |

The agent finds the first two by itself: both live in the store
`tasqx_search_memory` covers, so the next task on the same code reads last
time's annotation ([Memory](../wiki/Memory.md) explains the ranking). Candidates
are the part you look at. On a second sighting the skill drafts a real SKILL.md,
records the path on the candidate and cancels it; list the open ones now and
then, and promote one by hand when you already know it recurs.

A fact worth storing is checked against memory before it is added
(`tasqx_search_memory` first) precisely so the same convention is not
written twice under two titles — the second-sighting rule for skills exists
for the same reason, one level up: a procedure earns a skill only once it has
shown up twice.

## Other clients

The skill needs nothing but the `tasqx_*` tools and an agent that can be
handed an evidence bundle, so it works the same way in any MCP client — the
only Claude Code-specific piece was the retired hook, and nothing replaces
it. A client without a skills mechanism can still run the same procedure: put
the five questions, the scope gate and the routing rules from
[`.claude/skills/retro/SKILL.md`](../../.claude/skills/retro/SKILL.md) into
the agent's system prompt, next to the block from
[Giving an agent memory in any client](agent-starter-prompt.md), and tell the
agent to run them at the end of a session against the same evidence bundle
(the task read whole, the executor's report, `tasqx_outcomes`, and any
unannotated rework) rather than after every completion.
