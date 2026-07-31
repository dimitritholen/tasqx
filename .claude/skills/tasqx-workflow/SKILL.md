---
name: tasqx-workflow
description: Use tasqx (MCP tools + CLI) as the primary system for task management, backlogs, and knowledge memory. Use this skill whenever the user starts or continues feature or project work, wants work planned, decomposed, or tracked, asks what is open or what to pick up next, wants a progress or status report, or wants decisions, conventions, or lessons stored or looked up later — even when they never say "tasqx". Also use it when resuming work across sessions ("where were we?"). Skip it for one-off questions and trivial single-step fixes; those don't belong in a backlog.
---

# tasqx workflow

tasqx is a local task manager with one JSON API behind three clients: a CLI (`tasqx`), an MCP server (fifteen `tasqx_*` tools), and HTML reports. Tasks live in a SQLite file on this machine; every change lands in an append-only event log, which is why nothing here is ever truly destructive. Treat it as the system of record for multi-step work: the backlog outlives the session, so work you record here is work a future session can pick up.

Prefer the MCP tools when they're available in the session — they return structured JSON and skip shell quoting. Fall back to the CLI for the few verbs the MCP deliberately lacks (`memory import`, `memory rm`, `report --html`) or when the MCP server isn't connected. If neither responds, say so and track the work conversationally instead — don't fake it.

## Scope boundary

A backlog entry costs attention every time someone reads the list, so only real units of work go in: features, multi-step tasks, anything that could outlive the session or block something else. Answering a question, running one command, or a one-line fix is not a task — do it and move on.

## Backlog discipline

- One tasqx project per repo or initiative (`tasqx_create_project`, or `tasqx init <name>`). Check `tasqx_list_projects` before creating — the project may already exist.
- Decompose a feature into ordered tasks: `tasqx_add_task` (title, project, priority H/M/L, estimate, tags), then `tasqx_add_dependency` to chain them. A dependency marks the dependent task `blocked`; a cycle is refused as a conflict, so you can build chains without checking for loops yourself.
- Long-form context — acceptance criteria, links, design notes — goes in `tasqx_annotate_task`. The body is stored verbatim, multi-line markdown included, so write it the way you'd want to read it back.

## The work loop

Ask for the working set, not the whole list: `tasqx_list_tasks` with filter `"project:<name> @working"`. Blocked, waiting, and completed tasks are invisible there **by design** — an empty working set with open tasks elsewhere means everything is blocked, not that the work is gone.

For each task the loop is:

1. **Consult memory first.** `tasqx_search_memory` on the task's key terms — conventions, prior decisions, and annotations from earlier work all come back bm25-ranked with snippets. Plain-text queries are matched as phrases, so hyphenated and dotted terms (`grep-check`, `tokens.css`) are safe as-is; only pass `raw: true` when you actually want FTS5 operators (`prefix*`, `AND`/`OR`), and expect a `bad_request` on invalid syntax rather than silent weirdness.
2. **Start the timer**: `tasqx_start_timer`. This moves the task to `active` and makes tracked time honest.
3. Do the work.
4. **Record the outcome** as an annotation before completing: what was done, what was measured, what a future reader needs. This matters more than it looks — annotations feed the same search index as memory docs (`scope: "annotations"`, source `task:#N`), so every completed task becomes retrievable knowledge automatically.
5. **Complete**: `tasqx_complete_task`. Its response includes `unblocked` — the tasks this completion released. That field is the loop's engine: pick the next task from it directly instead of re-querying. Completing an `active` task is fine; the timer stops implicitly. **Self-report token counts when you know them** (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`): you are the only party that knows which task this turn's spend served, so self-report is the primary measurement channel (D50) — a completion without counts gets a `tokens_hint` back saying so, and the log-parse fallback refuses samples claimed by more than one task rather than guess.

## Memory

Two kinds of knowledge, one search index:

- **Docs** — things worth finding again independent of any task: decisions, conventions, runbooks, lessons learned. Store them with `tasqx_add_memory` (title, body, and a `source` — a path, URL, or ticket). When a project has relevant markdown lying around (ADRs, docs/, guides), feed the whole directory once via the CLI: `tasqx memory import <dir>`. Import is one transaction — a bad file imports nothing — and re-importing the same directory replaces docs from the same source instead of duplicating, so it's safe to re-run after the docs change.
- **Annotations** — the automatic byproduct of step 4 above. You never add these to memory explicitly; completing tasks well is what builds this half of the index.

Removal is CLI-only: `tasqx memory rm <id>`, using the id that search printed. The MCP deliberately has no remove tool.

## Reporting

`tasqx_summary` groups open work by project, status, or priority with count/estimate/tracked metrics. One default worth knowing (D24): a report with no status term in its filter counts done work in every metric and skips only cancelled tasks — so don't add `status:` filters you don't need, and don't be surprised that finished work shows up. For a shareable artifact, the CLI emits a self-contained HTML page: `tasqx report --html --out review.html` (optionally scoped by a filter).

## Nothing is ever destroyed

tasqx has no hard delete — `tasqx delete` and `rm` are aliases for `cancel`, which keeps the task's history in the event log, and `tasqx reopen <ref>` undoes it. A cancelled dependency releases its dependents. So when work becomes obsolete, cancel it without ceremony; when a backlog looks cluttered, reach for a better filter, never for deletion.
