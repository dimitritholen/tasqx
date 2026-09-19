---
name: tasqx-workflow
description: Use tasqx (MCP tools + CLI) for backlogs, task tracking and knowledge memory. Use it whenever the user starts or continues feature work, wants work planned or tracked, asks what is open or what to pick up next, wants a status report, or wants a decision or convention stored or looked up — even when they never say "tasqx". Skip it for one-off questions and one-line fixes.
---

# tasqx workflow

The MCP server sends the loop itself on connect — search before you decide,
track multi-step work as tasks, brief then start then annotate then complete,
store a decision with its why — and CLAUDE.md carries the setup-specific
rules. This file holds only what neither says.

tasqx is one JSON API behind three clients: the CLI, an MCP server
(thirty-two `tasqx_*` tools), and HTML reports. Prefer the MCP tools. Fall back
to the CLI for the verbs the MCP deliberately lacks: `next`, `why`, `agenda`,
`chart`, `export`, `import`, `report --html`, `memory import`, `undo`, `use`,
`archive`, and `tasqx --no-daemon tokens recompute` (refused over the daemon
socket).

## Checks are claims, annotations are prose

An acceptance criterion goes in `tasqx_add_check`, never in an annotation:
a check has a state that completion can be measured against. tasqx never runs
a check — the criterion is a claim and the evidence a citation — so mark them
with `tasqx_set_check` or `checks_passed` on completion, each with `evidence`.
`failed` is a normal outcome worth recording; remove a check only when the
criterion was the wrong thing to ask. Everything else is an annotation, stored verbatim.

## Reading a task

The answer is the rendered markdown alone (D151); use it as-is, and pass
`include_json: true` only when a script needs the raw JSON beside it.
`tasqx_brief_task` derives its memory query from the task's title, tags and
project, so you never guess terms that silently find nothing. Use `tasqx_search_memory` for what the brief cannot see (another
project, a subject the title does not name); `raw: true` passes FTS5 syntax.

## Completing

`tasqx_complete_task` refuses a task with open blockers; `force: true`
completes it anyway and the override is counted (D150). Its `unblocked` field
is the loop's engine: pick the next task from it instead of re-querying.
Self-report token counts when you know them; only you know which task the
spend served (D50).

## Reporting

`tasqx_summary` groups open work by project, status or priority. Without a
`status:` term it counts done work in every metric and skips only cancelled
tasks (D24), so do not add filters you do not need.

`tasqx_outcomes` says how closed work went: rework (completions later
reopened), calibration (tracked over estimate), silent (no annotation),
unproven (a criterion nobody marked), forced, overrun, abandonment. Every rate
comes with its `n`; three completions is not evidence.

## Tasks are never destroyed, memory is

A task has no hard delete: `delete` and `rm` alias `cancel`, which keeps the
history, and `tasqx_reopen_task` undoes it. A cancelled dependency releases
its dependents. Memory is the exception: `tasqx_remove_memory` and
`tasqx memory rm` delete the body for good, and `tasqx undo` does not cover it.
