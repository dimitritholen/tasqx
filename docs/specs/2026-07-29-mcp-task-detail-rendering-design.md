# MCP task-detail rendering — design

**Date:** 2026-07-29
**Status:** designed, not implemented
**Scope:** `tasqx_get_task` only. No other MCP tool, no CLI surface, no protocol change.

## Problem

Every MCP tool returns the same shape (`mcp.rs:728`):

```rust
fn tool_ok(result: &Value) -> Value {
    json!({ "content": [{ "type": "text",
             "text": serde_json::to_string_pretty(result).unwrap_or_default() }],
            "isError": false })
}
```

Pretty-printed JSON in one text block. Whatever a user sees when they ask an agent
for task details is therefore authored by the agent, in that conversation, from
that JSON. Two people asking the same question about the same task get different
layouts; the same person gets a different layout tomorrow. There is no artifact
tasqx owns and can hold still.

## Decision

tasqx renders the task-detail view itself, in Rust, and ships it as the first
content block of the `tasqx_get_task` result. The existing JSON stays as the
second block.

Determinism is the point: the same task, read with the same settings, produces
byte-identical markdown regardless of which model or client asked. What that
markdown then *looks like* remains the client's business — MCP carries content,
not formatting, and Claude Code, Claude Desktop and Cursor all present text
differently. Owning the text is the achievable half, and it is the half that was
missing.

## Module and data flow

New module `crates/tasqx-core/src/markdown.rs`, one public function:

```rust
pub struct DetailOpts { pub time: TimeFormat, pub now: Timestamp }
pub enum TimeFormat { Iso, Relative, Both }

pub fn task_detail(result: &Value, opts: &DetailOpts) -> String
```

Pure: same inputs, same output. No store access, no clock call, no environment,
no theme. `now` is a parameter rather than a `now()` call precisely so relative
formatting stays testable — the same reasoning that put `now` in
`compute_attribution(pa, now)` (`attribution.rs:324`).

No second query is introduced. One `task.get` result feeds both blocks:

```
task.get ──► Value ──┬──► markdown::task_detail(&v, &opts) ──► content[0]  (human)
                     └──► to_string_pretty(&v)              ──► content[1]  (machine)
```

`mcp.rs` gains a `tool_ok_with_view(view, result)` beside `tool_ok`. Only
`tasqx_get_task` calls it; every other tool's wire shape is untouched.

**Block order is markdown first, JSON second.** Clients that surface only the
first block prominently then surface the readable one, and a model reading in
order takes its cue from what leads.

**Why this cannot reuse `render::task_detail`** (`render.rs:382`): it lives in the
CLI crate, and it takes a `&Ctx` because it paints theme colours
(`ctx.paint("header", ...)`, `render.rs:385`). Output that depends on a theme is
by definition not identical between users. The markdown renderer takes no `Ctx`;
that omission is the design, not a simplification.

**Failure is not an option the renderer has.** No `unwrap` on shape, no panic on a
missing or mistyped field. If rendering somehow yields an empty string, the call
site falls back to the JSON block alone. A `get_task` that breaks on presentation
would be strictly worse than today's plain JSON.

## The rendered view

Four rules decide the layout.

1. **Core fields always, optional fields only when set.** `status`, `priority`,
   `project`, `created`, `modified` and `_rev` always appear. `due`, `scheduled`,
   `wait`, `remind`, `recurrence`, `active_since`, `completed`, `blocked`,
   `depends_on`, `tags` and `tokens` appear only when they hold a value — for
   `blocked` that means only when true, since "not blocked" is the silent norm. A
   task with none of them would otherwise be mostly dashes. Output stays
   deterministic — same input, same rules — but not uniform in length.

2. **Annotations verbatim, and outside the heading hierarchy.** Annotation bodies
   in this project *are* markdown, with their own `##` headings and fenced code.
   A blockquote would break their tables; demoting their headings would rewrite
   text the store deliberately keeps verbatim. Each annotation therefore gets a
   horizontal rule and a bold timestamp line — not a markdown heading — with the
   body emitted untouched below it.

3. **The title goes in the `##` heading**, as `## #<short_id> · <title>`. A detail
   view without its title on top reads wrong; a long title wrapping is a client
   concern, not a content one.

4. **An unrecognized status is flagged**, naming the valid set derived from
   `Status::ALL`, exactly as `render.rs:390` already does for the terminal.
   Otherwise the MCP view would be more forgiving than the CLI about the same
   fault.

Rendered example, `time: Iso`, for a task with no dates, dependencies or
measurements:

```markdown
## #76 · Three field-test papercuts: empty fields[], show flattens annotations, otlp.enabled without a daemon

| | |
|---|---|
| status | pending |
| priority | L (urgency 1.8) |
| project | tasqx-field-test-2026-07 |
| tags | field-test, papercut |
| estimate | PT2H |
| tracked | PT0S |
| created | 2026-07-29T09:00:58Z |
| modified | 2026-07-29T09:01:45Z |
| rev | 2 |

### Annotations (1)

---
**2026-07-29T09:01:45Z**

Three small ones from the 2026-07-25 field test, grouped because each is a few
lines and none justifies its own task. ...
```

## Configuration

New setting `detail.time_format`, values `iso | relative | both`, default `both`.
One entry in `SETTINGS` (`config.rs:112`) with `kind: Kind::Str` and a closed
`choices` list, so `tasqx config set detail.time_format xyz` is refused rather
than silently accepted. It governs timestamps *and* durations, which is why
`PT2H` versus `2h` is not a separate decision.

| value | `created` | `estimate` |
|---|---|---|
| `both` (default) | `2026-07-29T09:00:58Z (2 hours ago)` | `PT2H (2h)` |
| `iso` | `2026-07-29T09:00:58Z` | `PT2H` |
| `relative` | `2 hours ago` | `2h` |

Named for the *screen*, not the transport. `mcp.time_format` would be more honest
about what it touches today, but if `tasqx show` later shares this renderer — that
is papercut 2 of task #76 — the name would be wrong, and renaming a config key
breaks every existing file.

**Wiring.** `run_mcp_serve` (`lib.rs:2565`) reads the setting and passes it to
`McpServer::new`, which currently takes `(&engine, scope)`. Core stays
config-agnostic; the CLI, which already owns config, injects the choice. `now` is
stamped per call at the call site.

This is a deliberate, narrow retreat from "identical for everyone": output is now
deterministic *per configuration*, and two colleagues with different settings see
different text. That is their choice rather than a model's whim, which was the
actual problem.

## Drift guard

One test builds a task with every field set — due, scheduled, wait, remind,
recurrence, dependencies, annotations, tokens, tags, estimate — reads it back
through `task.get`, and iterates the keys of the result. Every key must be
*accounted for*: either it maps to a row the renderer emits, or it sits on an
explicit `OMITTED` list with a stated reason.

"Accounted for" is a declared key→row mapping, not a substring search for the key
name. Several keys render under a different label (`_rev` as `rev`) or fold into
another row (`urgency` inside the priority cell, `status_unrecognized` inside the
status line), and a naive text search would both miss those and match by accident
on values.

Today the `OMITTED` list holds one entry: `id`, the UUID — `short_id` is the
handle users type. A field added to `task.get` later fails the build naming
itself, rather than quietly missing from the view.

The list of expected keys is derived from a live `task.get` result, never
hand-maintained. A hand-written list would fall behind within two commits and
then reassure instead of warn, which is worse than having no test.

## Testing

- **Golden tests:** three fixtures, one per `TimeFormat`, with a pinned `now`,
  compared byte-for-byte over the whole output. This is the byte-identical
  promise expressed as a test rather than as an intention.
- **Robustness:** `task_detail` fed `{}`, `null`, and fields of the wrong type.
  Expected outcome is never a panic and never an empty string — at worst a thin
  view.
- **Wire shape:** a test asserting `tasqx_get_task` returns exactly two content
  blocks in the stated order, and that every other tool still returns one.
- **Config:** the closed `choices` list rejects an unknown value; the default is
  `both` when the key is absent.

## Costs and trade-offs

- **Payload roughly doubles for `tasqx_get_task`**, since the data crosses the
  wire twice. On a task with many long annotations that is not nothing. Accepted:
  the JSON block is what keeps the tool usable for agents that act on fields
  rather than read prose.
- **Output is no longer identical across users** once `detail.time_format`
  differs. Accepted above, deliberately.
- **`relative` output is time-dependent by construction.** Two reads of an
  unchanged task minutes apart differ. That is what relative time means; the
  golden tests pin `now` so it stays testable.

## Deliberately not in scope

- `list_tasks`, `summary`, `list_projects`, `search_memory` keep returning plain
  JSON. One rendered surface first, judged in use, before deciding whether the
  pattern generalises.
- No render registry or presentation trait. There is one tool; the abstraction
  would be built before the second case exists to shape it.
- No CLI change. `tasqx show` flattening multi-line annotations is a real defect,
  but it is task #76 in `tasqx-field-test-2026-07`, not this design.

## Decision number

When implemented, this needs an entry in DESIGN.md §12 at the next free
D-number. `main` currently runs to **D47**, and `feat/reporting-redesign` already
holds an unlanded proposal that must be renumbered to D48 — so check both before
claiming one. That branch's doc still calls itself D27, a number DESIGN.md had
already assigned to "an unrecognised filter token is an error" before the branch
was cut; do not repeat the mistake by hard-coding a number here.
