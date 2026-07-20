# Bounded task snapshots — design

**Date:** 2026-07-20
**Status:** approved for implementation
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

- Query-count regression for empty, small, and large stores.
- Existing list/report/export behavior tests.
- Ignored 1,000/10,000-task benchmark fixture.
- Full workspace tests, Clippy with warnings denied, and diff checks.
