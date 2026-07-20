# Bounded task snapshots — design

**Date:** 2026-07-20
**Status:** implemented and verified 2026-07-20
**Scope:** Medium #1 only: remove task-count-dependent reads from `task.list`, `report.summary`, and `store.export`.

## Decision

The three bulk readers share one private `TaskSnapshot` loader. A snapshot contains the task row, sorted tags, unresolved-dependency state, dependency UUIDs, and annotations. The loader uses five set-based statements regardless of task count:

1. all task rows;
2. all task/tag relationships;
3. the distinct task ids with a non-terminal dependency;
4. all visible dependency relationships;
5. all annotations.

Relationships are grouped by task id in Rust and attached to the task rows. `task.list` and `report.summary` consume tags and blocked state; `store.export` also consumes dependency ids and annotations. Existing single-task helpers remain for point reads and mutation responses.

The loader reports its executed statement count internally. A regression test compares stores of different sizes and asserts that both execute the same fixed count. An ignored 1,000/10,000-task fixture records end-to-end timings for the three affected APIs without making wall-clock thresholds part of CI.

## Semantics preserved

- Filtering remains in Rust with the existing grammar and `MatchCtx`.
- List urgency is recomputed before filtering/sorting, and requested sort, limit, and field projection are unchanged.
- Report defaults, grouping, metrics, and cancelled-task behavior are unchanged.
- Export remains ordered by `short_id`, trims edges outside the selected set, reports the dropped count, and preserves annotation/dependency ordering.

## Alternatives

One SQL statement with JSON aggregation was rejected because it couples domain serialization to SQLite JSON support and multiplies relationship rows. Per-call feature flags were rejected as needless complexity: five bounded indexed scans are predictable and the richer snapshot is required by export. Keeping export's dependency and annotation point queries was rejected because it would leave a second N+1 path under a supposedly bounded primitive.

## Verification

- `task_snapshot_statement_count_is_independent_of_task_count` passes for empty, one-task, and 32-task stores at five statements each.
- Existing list/report/export behavior and byte-identical round-trip tests pass.
- Ignored `benchmark_task_snapshot_bulk_readers` covers 1,000/10,000 tasks with tags and dependencies. The 10k debug-build verification sample measured approximately 275 ms list, 68 ms report, and 291 ms export.
- Full workspace tests, Clippy with warnings denied, and diff checks pass.
