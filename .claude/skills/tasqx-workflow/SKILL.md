---
name: tasqx-workflow
description: Use tasqx (MCP tools + CLI) as the primary system for task management, backlogs, and knowledge memory. Use this skill whenever the user starts or continues feature or project work, wants work planned, decomposed, or tracked, asks what is open or what to pick up next, wants a progress or status report, or wants decisions, conventions, or lessons stored or looked up later — even when they never say "tasqx". Also use it when resuming work across sessions ("where were we?"). Skip it for one-off questions and trivial single-step fixes; those don't belong in a backlog.
---

# tasqx workflow

tasqx is a local task manager with one JSON API behind three clients: a CLI (`tasqx`), an MCP server (twenty-nine `tasqx_*` tools), and HTML reports. Tasks live in a SQLite file on this machine; every change lands in an append-only event log, which is why nothing here is ever truly destructive. Treat it as the system of record for multi-step work: the backlog outlives the session, so work you record here is work a future session can pick up.

Prefer the MCP tools when they're available in the session — they return structured JSON and skip shell quoting. Fall back to the CLI for the verbs the MCP deliberately lacks: `next`, `why` and `agenda` (picking and explaining), `chart`, `export`, `import`, `report --html`, `memory import`, `undo`, `use`, `archive`, and `tokens recompute` — that last one is stricter still: it is refused over the daemon socket, so it runs in-process as `tasqx --no-daemon tokens recompute`. `reopen`, `undep`, `memory rm` and `cancel` are NOT on that list — they are `tasqx_reopen_task`, `tasqx_remove_dependency`, `tasqx_remove_memory` and `tasqx_cancel_task` (D64, D67, D114). Fall back to the CLI too when the MCP server isn't connected. If neither responds, say so and track the work conversationally instead — don't fake it.

## Scope boundary

A backlog entry costs attention every time someone reads the list, so only real units of work go in: features, multi-step tasks, anything that could outlive the session or block something else. Answering a question, running one command, or a one-line fix is not a task — do it and move on.

## Backlog discipline

- One tasqx project per repo or initiative (`tasqx_create_project`, or `tasqx init <name>`). Check `tasqx_list_projects` before creating — the project may already exist.
- Decompose a feature into ordered tasks: `tasqx_add_task` (title, project, priority H/M/L, estimate, tags), then `tasqx_add_dependency` to chain them. A dependency marks the dependent task `blocked`; a cycle is refused as a conflict, so you can build chains without checking for loops yourself.
- Acceptance criteria go in `tasqx_add_check`, not in an annotation. An annotation is prose, so nothing can ask at completion time whether it was met; a check has a state. tasqx never RUNS a check — the criterion is a claim and the evidence is a citation, both stored verbatim — so mark them yourself with `tasqx_set_check`, or pass `checks_passed` (with `evidence`) on completion. `failed` is a normal outcome worth recording; remove a check only when the criterion was the wrong thing to ask.
- Long-form context — links, design notes, anything that is not a pass/fail criterion — goes in `tasqx_annotate_task`. The body is stored verbatim, multi-line markdown included, so write it the way you'd want to read it back.
- Open a task's first annotation with one plain-language paragraph saying what the work is and why. There is no separate description field; that paragraph is what the D146 card shows as its Description row.

## The work loop

Ask for the working set, not the whole list: `tasqx_list_tasks` with filter `"project:<name> @working"`. Blocked, waiting, and completed tasks are invisible there **by design** — an empty working set with open tasks elsewhere means everything is blocked, not that the work is gone.

To read one task, `tasqx_get_task` answers with two content blocks (D49): tasqx-rendered markdown first, then the JSON. Use the markdown as-is — layout is tasqx's job — instead of recomposing the detail from the JSON fields. When you are about to START the task rather than just look at it, use `tasqx_brief_task` instead: same task, same rendering, plus what its prerequisites decided and the memory you would otherwise have had to guess a query for. When the task is being shown to a HUMAN rather than read by you, ask for `view: "card"` instead and paste the fenced card back verbatim — it is a fixed-width box-drawn document meant for a document, not a screen (D146).

For each task the loop is:

1. **Brief yourself.** `tasqx_brief_task` on the ref, and that is the whole step: it returns the task, each prerequisite with **what that task concluded** (its newest annotation), what this task blocks, and relevant memory — under a query tasqx derives from the task's own title, tags and project. You do not supply search terms, which is the point: a guessed term that finds nothing looks exactly like a store with nothing in it, and you would never know. Pass `include_json: false` when you only need to read it.

   Reach for `tasqx_search_memory` yourself when you want something the brief's scope does not cover — another project, a subject the task's own words do not name, or FTS5 operators via `raw: true` (plain queries are matched as phrases, so `grep-check` and `tokens.css` are safe as-is; invalid raw syntax is a `bad_request`, not silent weirdness).
2. **Start the timer**: `tasqx_start_timer`. This moves the task to `active` and makes tracked time honest. Starting also auto-stops whatever timer was already running — one clock at a time — unless you pass `keep`.
3. Do the work.
4. **Record the outcome** as an annotation before completing: what was done, what was measured, what a future reader needs. This matters more than it looks — annotations feed the same search index as memory docs (`scope: "annotations"`, source `task:#N`), so every completed task becomes retrievable knowledge automatically.
5. **Complete**: `tasqx_complete_task`, naming in `checks_passed` the criteria this work actually proved and citing them in `evidence`. A task with dependencies still open is refused (`conflict`, naming them); `force: true` completes it anyway and the override is counted (D149). Its response includes `unblocked` — the tasks this completion released. That field is the loop's engine: pick the next task from it directly instead of re-querying. Completing an `active` task is fine; the timer stops implicitly. **Self-report token counts when you know them** (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`): you are the only party that knows which task this turn's spend served, so self-report is the primary measurement channel (D50) — a completion without counts gets a `tokens_hint` back saying so, and the log-parse fallback refuses samples claimed by more than one task rather than guess.

## Memory

Two kinds of knowledge, one search index:

- **Docs** — things worth finding again independent of any task: decisions, conventions, runbooks, lessons learned. Store them with `tasqx_add_memory` (title, body, and a `source` — a path, URL, or ticket). When a project has relevant markdown lying around (ADRs, docs/, guides), feed the directory via the CLI: `tasqx memory import <dir>`. Import is not recursive — a directory means its own `*.md` files, so run it once per folder that holds docs. It is one transaction — a bad file imports nothing — and re-importing the same directory replaces docs from the same source instead of duplicating, so it's safe to re-run after the docs change.
- **Annotations** — the automatic byproduct of step 4 above. You never add these to memory explicitly; completing tasks well is what builds this half of the index.

Wrote something that turned out to be wrong? Retract it with `tasqx_remove_memory` (D64), using the id search printed — or `tasqx memory rm <id>` from the shell. Do retract rather than filing a correction beside it: search ranks both and says nothing about which is true. The removal is **permanent** — `tasqx undo` does not cover memory documents and the body is not recoverable from the event log — so read the doc before removing it.

## Reporting

`tasqx_summary` groups open work by project, status, or priority with count/estimate/tracked metrics. One default worth knowing (D24): a report with no status term in its filter counts done work in every metric and skips only cancelled tasks — so don't add `status:` filters you don't need, and don't be surprised that finished work shows up. For a shareable artifact, the CLI emits a self-contained HTML page: `tasqx report --html --out review.html` (optionally scoped by a filter).

Completing with a criterion still open is **not refused** — nothing blocks you — but it is counted as an unproven completion, and that count is read back through `tasqx_outcomes` below. Mark what you proved rather than everything.

`tasqx_outcomes` answers the other question — not what the work cost but how it went: **rework** (completions that were later reopened), **calibration** (median tracked-over-estimate), **cost**, **silent** (completions carrying no annotation), **abandonment** (started, then cancelled), **overrun** (past a token budget), **unproven** (completed with a criterion nobody marked) and **forced** (completions that overrode open blockers). It reads only tasks that CLOSED, so an open backlog is invisible to it by design; `tasqx_summary` is the read for work in flight. Every rate comes back beside the `n` it was computed over, so check the denominator before you act on a rate — three completions is not evidence. It is read-scoped, so it works without write access, and it is worth consulting before a big piece of work on a project you have touched before: a high `silent` count says the annotations a future session will search for are not being written, and a high `rework` count says completions are being called too early.

## Nothing is ever destroyed

tasqx has no hard delete — `tasqx delete` and `rm` are aliases for `cancel`, which keeps the task's history in the event log, and `tasqx reopen <ref>` undoes it. A cancelled dependency releases its dependents. So when work becomes obsolete, cancel it without ceremony; when a backlog looks cluttered, reach for a better filter, never for deletion.
