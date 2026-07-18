# Report status filtering — design

**Date:** 2026-07-18
**Status:** approved, ready for implementation
**Scope:** A of a three-part split (A: status filtering · B: writable config + `tasqx config` · C: interactive settings TUI). B and C are explicitly out of scope here.

## Problem

`tasqx report` counts everything. `Engine::report_summary` selects `SELECT {TASK_COLS} FROM tasks` (`engine.rs:1339`) with no status predicate, so `count`, `est_total` and `tracked_total` include done *and* cancelled tasks. Only the `overdue` metric excludes them (`engine.rs:1370`, via `is_open`). On a mature store the headline count is dominated by finished and abandoned work.

This surfaced while capturing throwaway test tasks: cancelling them (tasqx has no hard delete by design — DESIGN.md §725) left them counted in reports forever.

There is no single answer to "which statuses count". Three different hardcoded answers exist today:

| Surface | Filter | Source |
|---|---|---|
| `tasqx list` | `@working` | `main.rs:945` (CLI layer) |
| `report --html` summary | `status:pending` | `html.rs:25` |
| `report`, `chart burndown` | none — everything | `engine.rs:1339`, `main.rs:1123` |

The `--html` case is a latent bug: `status:pending` also excludes `active`, so the task currently being worked on vanishes from the project roll-up.

## Decision

**Report aggregations exclude `cancelled` by default. Done work still counts.**

Done and cancelled are not the same case. Completed work is real work — `tracked_total` is overwhelmingly time logged against done tasks, so excluding them would make time tracking useless. Abandoned work is not work, and should not inflate any total.

### Resolution order

`report.summary` resolves scope in this order:

1. **`all: true`** (CLI `--all`) → no default applied; everything counts, cancelled included.
2. **The caller's filter already constrains status** → the filter is used literally. `status:cancelled` returns cancelled tasks; `@working` means what it says.
3. **Otherwise** → `cancelled` is excluded.

Rule 2 exists so an explicitly-typed filter never yields a surprising empty result. Typing `tasqx report status:cancelled` and getting nothing back reads as a bug even when documented.

### Layer

The rule lives in **core**, in `Engine::report_summary` — not in the CLI.

`tasqx list` applies its `@working` default at the CLI layer (`main.rs:945`) while core's `task.list` stays literal. Report deliberately breaks that pattern: a report is an *aggregation*, whereas `task.list` is a raw query where "no filter = all rows" is an honest contract. Putting the rule in core means the CLI, `--html`, and MCP agents all inherit one answer, which is the point — it collapses the three inconsistent hardcoded filters above into one.

This changes the behaviour of an existing API method, so it is recorded as a design decision (D24), not an implementation detail.

## Mechanism

### `Filter::constrains_status`

New public method on `Filter` (`crates/tasqx-core/src/filter.rs`):

```rust
/// True when this filter already constrains status, so the report default
/// must step aside rather than silently narrowing what the caller asked for.
pub fn constrains_status(&self) -> bool
```

It walks the `Expr` tree (`filter.rs:52`) and returns true if any `Pred::Status(_)` or `Pred::Working` (`filter.rs:36`, `filter.rs:43`) appears anywhere, including inside `Or` branches.

A lexical check (`input.contains("status")`) is explicitly rejected: the AST already carries the answer exactly, and a substring test would misread nested or aliased forms. `Pred::Working` counts because `@working` expands to a status predicate (`filter.rs:42`).

### `report.summary`

`report_summary` already fetches all rows and filters in Rust via `Filter::matches`. The default is one additional skip in that loop — no SQL change:

- Read an optional `all: bool` param (default `false`).
- Compute `apply_default = !all && !filter.constrains_status()` once, before the loop.
- Skip rows whose status is `cancelled` when `apply_default`.

### CLI

`tasqx report` gains `--all`, which sets `all: true`. Its help text states the default in one line: cancelled tasks are excluded unless `--all` is passed or the filter names a status.

## Inherited fixes

- **`html.rs:25`** drops the hardcoded `"filter": "status:pending"` and passes no filter, inheriting the rule. This fixes the `active`-disappears bug as a side effect. The Rust-side open/overdue re-derivation at `html.rs:69` (`!matches!(status, "done" | "cancelled")`) stays as is and must remain consistent with the new default.
- **`chart burndown`** (`main.rs:1123`) passes an explicit filter excluding cancelled when resolving membership via `task.list`. This is a CLI-side fix — `task.list` keeps its literal contract and gains no default.
- **`chart throughput`** is untouched. It counts `done` events from the event log; a cancelled task never emits one.

## Out of scope

- `task.list` keeps "no filter = all rows".
- `tasqx list` keeps applying `@working` at the CLI layer.
- No config key for this. Making the default user-configurable is B (`tasqx config`), and depends on config.toml becoming writable — nothing writes it today.
- No changes to `store.export`, which stays a complete dump.

## Testing

Core (`tasqx-core`):

- `report.summary` with no filter excludes cancelled from `count`, `est_total`, `tracked_total`.
- `all: true` includes cancelled again.
- `status:cancelled` returns cancelled tasks (rule 2 beats rule 3).
- `@working` is honoured literally.
- Done tasks still count in the default case — the regression that would make `tracked_total` useless.
- `constrains_status` units: `""` → false; `project:x` → false; `status:pending` → true; `@working` → true; `project:x or status:done` → true.

CLI:

- `tasqx report --all` reaches core with `all: true`.
- HTML regression: a task in `active` status appears in the project summary (the `html.rs:25` bug).
- `chart burndown` excludes cancelled from its scope.

Drift guards already in the repo (`cmddoc`/`docs` alias and verb agreement) must stay green; `--all` needs its documentation entry alongside the flag.

## Documentation

- `DESIGN.md`: new decision **D24 — report aggregations exclude cancelled by default**, with the done-vs-cancelled reasoning and the resolution order.
- `cmddoc.rs`: the `report` entry gains `--all` and a note stating the default.
- `docs.rs`: the reports section states which statuses count.
