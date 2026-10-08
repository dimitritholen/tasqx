# Export and import

Your data is yours. `export` emits canonical JSON — stable UUIDs, every field,
sorted keys — that is git-diffable, greppable, and round-trips exactly.

## Export

With no filter you get everything. With a [filter](filters.md) you get a slice.
Human output *is* the JSON array — there is no separate pretty mode to drift
from it.

```console
$ tasqx export +api
[
  {
    "_rev": 4,
    "annotations": [
      {
        "body": "Blocked on the D12 decision",
        "created": "2026-07-16T08:51:09.6830568Z",
        "id": "019f6a1f-62f3-75f0-bf57-e5ff9c7c452a"
      }
    ],
    "completed": null,
    "created": "2026-07-16T08:51:09.2509427Z",
    "depends_on": [],
    "due": "2026-07-17T00:00:00Z",
    "estimate": "PT4H",
    "id": "019f6a1f-6142-70d3-be5b-e28dc6060e6c",
    "modified": "2026-07-16T08:51:09.6830568Z",
    "priority": "H",
    "project": "work.tasqx",
    "recurrence": null,
    "remind": null,
    "scheduled": null,
    "short_id": 1,
    "status": "pending",
    "tags": [
      "api",
      "release"
    ],
    "title": "Ship the v1 JSON API freeze",
    "urgency": 17.5,
    "wait": null
  }
]
```

## Filtered exports and dependency edges

A filter selects a subset, so a dependency pointing *out* of that subset cannot
travel with it — the target is not in the document. Those edges are trimmed, and
you are told, on **stderr**:

```console
$ tasqx export +docs > slice.json
note: 1 dependency points outside the export, left out — widen the filter to keep it
```

> **Note** The note is on stderr *because* stdout is the JSON. A note there
> would corrupt every pipe. `tasqx export +docs > slice.json` gives you a
> clean file and a visible warning, both.

The `--json` form reports the same thing as data — `dropped_dependencies` is
always present, and is `0` for an unfiltered export:

```console
$ tasqx export +api --json
{
  "default_project": "work",
  "docs": [ ... ],
  "dropped_dependencies": 0,
  "projects": [ ... ],
  "tasks": [ ... ]
}
```

## What a document carries

An export is a **self-contained document**, and a project is part of it (D37):
the `projects` array carries every project row — name, description, archived
state, identity — and `default_project` names the project a bare `tasqx add`
inherits. So are your [memory docs](#commands) (D41): the `docs` array carries
every knowledge document, all of them regardless of any filter — a filter
selects tasks, and knowledge is not attached to a task. Restoring gives you back
the store you exported, not just its tasks. These sections are **optional on
import**, so a file written by an older tasqx still restores: with no `projects`
section there is nothing to check a task’s `project` against, so the row is
created from the tasks and `import` says which ones it made. With one, the
document is authoritative — a task naming a project it does not define is
refused, exactly as `tasqx add --project` refuses a name no `init` ever created.

> **Note** An import never moves your default. If the destination store
> already has one, the document’s is ignored and the result names the default
> that stands.

## Import

Takes a file, or `-` for stdin. It accepts either the document `export` prints
or a bare array of tasks (which is what every older `export` wrote). Import is
an **upsert on the UUID** — re-importing the same document is a no-op, not a
duplicate.

```console
$ tasqx import slice.json
imported   2 tasks · 1 project · 3 memory docs
```

```console
$ tasqx export +api | TASQX_DB=/tmp/other.db tasqx import -
imported   1 task · 1 project · no memory docs
```

> **Note** The doc count is printed even when it is zero, and a document with
> no `docs` section at all (written by a tasqx older than D41) says so
> explicitly: ``note: the document carried no `docs` section, so no memory
> docs were restored``. Present-and-empty and absent used to print the
> identical line.

## A field the schema does not name is rejected

A task object may carry the keys below and nothing else:

<!-- generated: import-task-keys -->

An annotation may carry `id`, `body`, `created`. A key outside that set is a
`bad_request` naming the task and the field, because the alternative is worse
than an error: a misspelled `tags` used to import as `ok` with the tags silently
gone, so the report said your data arrived when it had not.

```
error [bad_request]: store.import: task 019f6a0f-99df-…, unknown task field `tag`
  (accepted: id, short_id, title, …) — check the spelling or drop it; it was silently
  ignored before, so the import reported success and the value never arrived
```

Two of those keys are accepted and deliberately *ignored*: `urgency` is
recomputed on the way in (it is derived from priority, due date and age, so a
supplied value could contradict the ranking rule) and `status_unrecognized` is a
read-side flag rather than stored state. They are listed because `export` emits
them and a round trip has to keep working.

## A dangling edge is rejected, not repaired

A dependency target must be in the payload *or* already in the store. Anything
else fails with `bad_request` naming the id — and because the whole import is
one transaction, a rejection writes **nothing**.

```
error [bad_request]: store.import: task 019f6a0f-99df-… depends on 019f6a0f-99b5-…,
  which is neither in the payload nor in the store (export the dependency too, or drop the edge)
```

That is a deliberate choice. An edge to an unknown id means the wrong slice was
exported; repairing it quietly would hide exactly the mistake worth seeing.
Payload order does not matter — tasks are written first, edges second, so a
forward reference is fine.

## Recipes

| Want | Do |
|---|---|
| Back up | `tasqx export > backup.json` |
| Version your tasks in git | `tasqx export > tasks.json && git commit -am wip` — canonical output means clean diffs. |
| Move to another machine | `tasqx export > all.json`, copy, `tasqx import all.json` |
| Hand one project to a colleague | `tasqx export project:work.tasqx > slice.json` — mind the edge note. |
| Query beyond the filter grammar | `tasqx export \| jq '[.[] \| select(.urgency > 15)]'` |
