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
