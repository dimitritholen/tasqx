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

Save this as `~/.claude/skills/retro/SKILL.md`. If a skill named `retro`
already lives there, save it under another name and invoke it by that name;
nothing in the text depends on it:

```markdown
---
name: retro
description: Retrospective on the tasqx tasks completed in this session — review the decisions and rework that got them done, record the lessons on each task, and decide whether a procedure earns a skill. Use at the end of a session, or when the user says retro, retrospective, post-mortem, lessons learned, or asks what we learned. Never triggered by completing a task.
---

This skill runs once per session, on request (`/retro`), over every task the
session completed — not after each `tasqx_complete_task`. Until 2026-09-16 a
PostToolUse hook nudged a retro per completion; each run cost ~56k tokens, 14
completions in 36 h cost ~800k, and a measurement (memory doc
01a0a754-e116) found the docs those retros wrote were never read back. Decided
in claude-config #675: one run, one agent, one bundle per task that earned it,
and nothing written on a task that went straight through. Its evidence is an
evidence bundle the main session assembles from tasqx and the executors'
reports (Step 0); the session transcript is not read. Its outputs are an
annotation on each reviewed task, and optionally a memory entry, a hookify
recommendation, or a skill candidate. A skill is promoted only on the second
sighting of the same procedure — retros overrate their own novelty.

## Step 0: Pick the tasks, then delegate to a fresh agent on an evidence bundle

**Which tasks.** With a ref (`/retro 123`), that task. With none, every task
whose `tasqx_complete_task` this session made; if unsure, `tasqx list
status:done` newest first and take the ones completed since the session began.

**Which of those earn a bundle.** Only a task whose work had rework (a
reverted edit, a re-run command, a wrong assumption), a discovery written down
nowhere, a blocked or refused step, or an executor that came back wrong. The
main session judges this from what it watched. A task that went straight
through gets no bundle and no annotation; name it in one line of the verdict.
If no task qualifies, say so and spawn nothing.

**Why a fresh agent, not a fork.** A fork inherits the whole session
transcript and its cost grows with the session (measured at 247k, 494k and
559k tokens for three runs, the last for a single task), while the evidence a
retro needs is a few thousand tokens per task.

Assemble one bundle per qualifying task:

1. The task read whole: `tasqx_get_task` with `include_json: false` and
   `annotations_limit` set to the task's `annotations_total`. That asks for
   every annotation (approach, decisions, blockers, delivery) and every check
   with its evidence; a long history is still cut to the response budget
   (D148), and the answer says so with a non-null `annotations_next_offset`,
   so page from there until it is null (or raise `max_body_bytes` when one
   long body was the cut) before calling the bundle complete.
2. The executor's final hand-back report for that task, pasted verbatim, with
   the token count and wall time from its task notification.
3. The `tasqx_outcomes` result for the task's project (calibration, rework,
   unproven, cost), once per project.
4. Anything the planner itself reworked or reverted for this task, in two or
   three lines. The fresh agent cannot see the transcript, so rework that was
   never annotated is invisible unless it is written here; the habit to keep
   is to annotate such rework on the task as it happens.

Then call the `Agent` tool once with `subagent_type: "general-purpose"` and
the prompt "Run the retro skill for tasks #N, #M" followed by all the bundles.
Relay the agent's verdict in one or two sentences per task.

You are the fresh agent when the prompt you were given starts with "Run the
retro skill for task". In that case run steps 1-5 directly on each bundle,
citing the annotation, check or report line that supports each answer, and
end your final message with the verdicts from Step 5.

## Step 1: Answer five questions from the evidence bundle

Cite what actually happened for each answer — the file, the command, the
reverted edit, the annotation, check or report line where an assumption
broke. Answer only the questions with evidence to cite.

1. **Goal vs delivered**: what was asked, what shipped, the gap if any.
2. **Rework**: every edit reverted, every command re-run, every wrong
   assumption. This is the only honest signal of a hard step.
3. **Discovery**: what was learned that is written nowhere — a convention, a
   gotcha, a reason behind a choice.
4. **Redo**: which steps would be skipped, which reordered.
5. **Procedure**: is any part a repeatable sequence with a verifiable end that
   someone else would need.

## Step 2: Scope gate

If questions 2 and 3 produced no evidence for a task, write nothing on it:
report it as "straight through" in the verdict and move to the next bundle.

## Step 3: Route each finding

- A fact, convention, or gotcha from Q3: search `tasqx_search_memory` first
  with two or three keywords. If it is not already there, call
  `tasqx_add_memory` with a title, a body (the ruling on the first line, the
  reasoning under it), and `source` naming the repo path.
- A mistake worth preventing mechanically (a "never run X against Y" rule):
  name the pattern to the user as a hookify candidate. Hooks are the user's
  call to create.
- A procedure from Q5: run it through the skill-worthiness test in Step 4.

## Step 4: The skill-worthiness test

Gates 1 and 2 must both pass:

1. **Procedural** — a sequence of steps with a verifiable end. A fact belongs
   in memory instead; a prohibition belongs in a hook instead.
2. **Non-obvious** — it cost at least one wrong attempt or a discovery to get
   right. Getting it right first time from general knowledge means a skill
   would add nothing.

Then search for a prior sighting: `tasqx_search_memory` with the procedure's
key terms, two or three words, in two different wordings. A hit whose
`source` is `task:#N` on a task titled `skill candidate: …` is a prior
sighting.

**A gate fails**: record which gate and why in the retro annotation (Step 5)
and stop.

**No prior sighting**: create the candidate.
- `tasqx_add_task` titled `skill candidate: <short name>`, same project as the
  completed task, priority L, tag `skill-candidate`.
- `tasqx_annotate_task` on it with the procedure as numbered steps, the
  trigger phrase a user would say, and `sighting 1: task #N`.

Completion criterion: the candidate task exists with that annotation.

**Prior sighting, candidate open**: promote it, in this order.
- `tasqx_annotate_task` on the candidate with `sighting 2: task #N` and
  whatever this run added to the procedure.
- Draft the skill with the `skill-creator` skill (invoke it via the `Skill`
  tool), using the candidate's annotation as the spec.
- `tasqx_annotate_task` on the candidate with the path to the new SKILL.md.
- `tasqx_cancel_task` the candidate.

Completion criterion: the SKILL.md file exists and the candidate task is
cancelled.

**Prior sighting, candidate already cancelled with a SKILL.md path in its
annotation**: the skill exists. Record the path in the retro annotation and
stop.

## Step 5: Write the retro annotation

`tasqx_annotate_task` on each reviewed task. Body starts with `retro:`,
followed by the five answers (only those with evidence) and the routing
outcome — the memory id, the hookify recommendation, the candidate task ref,
or the skill path. The verdict relayed by the fresh agent names any hookify
pattern verbatim, since that recommendation reaches the user only through the
verdict.

Completion criterion: the annotation is stored, and if this run is the fresh
agent, its first lines are the verdicts relayed to the main agent.
```

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
(`tasqx_search_memory` first, Step 3) precisely so the same convention is not
written twice under two titles — the second-sighting rule for skills exists
for the same reason, one level up: a procedure earns a skill only once it has
shown up twice.

## Other clients

The skill needs nothing but the `tasqx_*` tools and an agent that can be
handed an evidence bundle, so it works the same way in any MCP client — the
only Claude Code-specific piece was the retired hook, and nothing replaces
it. A client without a skills mechanism can still run the same procedure: put
the five questions, the scope gate and the routing rules from the block above
into the agent's system prompt, next to the block from
[Giving an agent memory in any client](agent-starter-prompt.md), and tell the
agent to run them at the end of a session against the same evidence bundle
(the task read whole, the executor's report, `tasqx_outcomes`, and any
unannotated rework) rather than after every completion.
