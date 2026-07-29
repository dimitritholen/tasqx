# MCP task-detail rendering — design

**Date:** 2026-07-29
**Status:** implemented as **D49** on `feat/mcp-task-detail`, with seven departures
from this document — see [Divergences](#divergences-from-what-landed) at the end.
The decision, the rendering rules and the config surface all landed as written; the
wiring and two of the test claims did not. Departures 5–7 are of a different kind
from the first four: they are not wiring, they are defects two adversarial reviews
found after D49 was already written down — a duplicated duration reader, a drift
guard that was the substring search it promised never to be, and an overflow that
survived the first fix by moving one function down. Every one of them was found by
an agent asked to refute the work, and none by an agent asked to build or test it.
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

**Landed as D49, and the gap it left is closed.** D48 was held free for the
`feat/reporting-redesign` proposal, which the audit found was the rightful next
claimant — but that reservation lived only in `HANDOFF.md`, which is gitignored,
so §12 read D47 → D49 with nothing tracked saying why. The reporting decision
landed as **D48** on 2026-07-29, in the same session, which resolves the open
question by making the reservation moot rather than by choosing between the three
options this section used to list. The lesson survives the resolution: a number
reserved in an untracked file is not reserved, and the only reason this one held
is that the claimant arrived within the day.

## Divergences from what landed

Recorded rather than edited into the prose above, so the design stays readable as
what was *decided* and this section carries what was *built* differently.

1. **The setting reaches the server through a builder, not `McpServer::new`.**
   "Wiring" above says `run_mcp_serve` passes the value to `McpServer::new`.
   `new(&engine, scope)` is unchanged; the value arrives via a new
   `McpServer::with_time_format` (`mcp.rs`), because widening `new` would have
   edited every call site in the test suite to hand the default straight back.

2. **A closed `choices` list needed a new variant, not just an entry.** The
   design assumed `SETTINGS` could carry one. `Choices` had only `Free` and
   `Themes`, neither of which the writer consults, so `Choices::OneOf(&[…])` was
   added along with the refusal in `write_value_in` — and the two exhaustive
   `match`es outside `config.rs` (`cli/lib.rs`'s `build_row`, `tui/settings.rs`)
   had to grow an arm.

3. **The byte-for-byte goldens cover `Iso` only.** "Testing" promises three
   whole-output fixtures, one per `TimeFormat`. Four landed
   (`tests/markdown_detail.rs`) and all four use `TimeFormat::Iso`; `Relative`
   and `Both` are asserted by substring on the rows that differ. The
   byte-identical promise is therefore pinned for one format and sampled for the
   other two.

4. **"Every other tool still returns one block" is sampled, not swept.** One
   test checks `tasqx_list_tasks`. The rest of the coverage is incidental:
   `tests/mcp.rs`'s `tool_text` helper reads block zero and parses it as JSON for
   every other tool, so a second block appearing anywhere else would fail there.

5. **The renderer wrote its own duration reader instead of using the one in the
   same crate.** "Failure is not an option the renderer has" above, and the
   configuration table's promise that `relative` turns `PT2H` into `2h`, were
   both implemented by a private `iso_duration_secs` in `markdown.rs` — the
   implementation plan prescribed it, and nobody asked why `crate::util::duration_secs`,
   public and one module over, was not enough. The copy diverged the two ways a
   copy always diverges. It knew less: only `D/H/M/S`, while `datetime::parse_duration`
   validates against `duration_secs` and therefore accepts and stores `Y`, `W`
   and date-position `M` verbatim — so a stored `estimate:P2W` reached the
   renderer, failed to parse, and `TimeFormat::Relative` handed back the raw ISO
   string it exists to replace. And it was less careful: `* 86_400`, `* 3600`,
   `* 60` and `+=` unchecked, against seven `checked_*` calls in the original,
   whose doc comment records that unchecked arithmetic there once made `report`
   panic in debug and print a wrapped total in release. That defect was rebuilt
   one module over, under a public function whose stated contract is that it
   never panics: `PT999999999999999999H` panicked, `P999999999999999999D`
   printed `-4549241255539855744`. Removed; `fmt_duration` now calls
   `crate::util::duration_secs`, no golden expectation changed, and the plan is
   annotated in place so the listing is not copied again.

   **Removing the fork did not close the panic — it moved it one function down**,
   and a second adversarial pass caught that. `round_div`'s `secs + unit / 2` was
   still unchecked, and `parse_duration` puts no ceiling on an estimate, so
   `estimate:PT9223372036854775807S` was storable and then aborted `task_detail`.
   Two things made that worse than the first round. In debug, `tasqx_get_task`
   answered `{"error":{"code":-32603}}` instead of the task — the view degrading
   to strictly less than the JSON it replaced, which is what this design forbids.
   In the **release profile the binaries actually ship as**, `[profile.release]`
   sets no `overflow-checks`, so there was no panic at all: it wrapped and
   rendered `| estimate | -106751991167300d |`. And the trigger was never limited
   to a hand-typed absurd estimate — `store.import` reaches the same value through
   `tracked_seconds`, a key on `IMPORT_TASK_KEYS` that no user ever types.

   The test written alongside the first fix did not catch this, and the reason is
   worth keeping: all four of its inputs were ones `duration_secs` *refuses*, so
   none reached the arithmetic. It asserted the property on the half of the input
   space that could not violate it. `round_div` now divides first and decides on
   the remainder — overflow-free for any non-negative input, rather than checked
   arithmetic with a fallback, which would have left "what do we print when it
   overflows" live forever on a path whose contract is that it cannot fail.

6. **The drift guard was the substring search it says here it is not.** The
   section above states that "accounted for" is a declared key→row mapping "not
   a substring search for the key name", and DESIGN.md's D49 entry repeated the
   claim. What shipped declared nine keys and, for every other key, *generated*
   the needle `| {key} |` — a substring search for the key name, taken by
   roughly fifteen of the keys `task.get` returns. The needles matched the row's
   **label**, which `row()` writes from a literal, so they stayed satisfied after
   the cell stopped arriving: emitting `row("project", "")` left the guard green.
   That is exactly the drift the mapping exists to catch, which makes the cost
   worse than a missing test — the guard, and the design entry vouching for it,
   were both reassuring. A guard that passes for the wrong reason is worse than
   no guard, and this one shipped that way. The `status_unrecognized` mapping was
   dead on top of that: its declared needle `| status |` was satisfied by the
   always-present status row, and the fixture only ever drove
   pending→active→done, so the flag was never emitted and rule 4 could be deleted
   from `status_cell` outright with everything green. Fixed: the fallback arm is
   gone, so a key in neither table fails naming itself; needles carry fixture
   **values** (`Shows::Cell` a literal containing one, `Shows::Row` a
   `| label | value |` built from that snapshot's own JSON), so a row that keeps
   its label and loses its cell fails, and a key the fixture leaves null
   everywhere is reported rather than passed; and a third snapshot, whose status
   is written straight through the connection the way `tests/increment.rs`
   reaches the same D28 state, makes the unrecognized-status mapping fire against
   the suffix the flag actually produces.

Two smaller notes. The drift guard reads the task back **three times** — running,
finished, and once more from a store whose status was written behind the API —
and unions the keys, because `active_since` and `completed` cannot both hold a
value on one task, `status_unrecognized` can be produced by no writer of this
build at all, and a single snapshot would leave whichever field it cannot hold
permanently unchecked. And rule 1 above lists neither
`estimate` nor `tracked` among the always-present or the optional fields, though
its own example shows them; both are rendered only when set.

Checked while recording the above, since a rewrite of the guard is the obvious
place for an escape hatch to widen: the `OMITTED` list still holds exactly one
entry, `id` (`crates/tasqx-core/tests/markdown_detail.rs:431`), unchanged from
the day it was written. Every other key `task.get` returns is now declared in
`RENDERED_AS` with a needle, which is the opposite of widening — before the fix
only nine keys were declared at all.

7. **The drift guard's state coverage, found only by attacking it from the
   unused direction.** Every check on the guard so far had removed something and
   watched it fail. Nobody had *added* a field and watched it fail — which is the
   guard's actual purpose. Doing that exposed a hole the value-needle fix did not
   touch: the fixture read the task in `active`, `done` and the anomalous `Done`
   state, so a key `task.get` emits **only** for a `pending` or `backlog` task
   escaped entirely. `pending` is the status every task tasqx creates starts in.
   A probe emitting a field for pending tasks only passed the guard green; the
   same probe emitted unconditionally failed it, so the gap was state coverage,
   not the mapping. Fixed by snapshotting the task before it is started, plus a
   second task parked in `backlog` behind a future `wait` — a state task 2 can
   never visit, since a backlog task cannot be started.

   Worth stating plainly, because it generalises past this feature: the guard was
   verified three times by deletion and zero times by addition, and deletion was
   the direction that could not find this. A test's blind spot tends to sit in
   whichever direction nobody exercised, not in the one everyone re-ran.
