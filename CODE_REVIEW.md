# tasqx Code Review

**Audit date:** 2026-07-20  
**Scope:** all committed Rust, manifests, CI, tests, and architecture documents  
**Reviewer stance:** correctness and data integrity first; then operability, performance, and maintainability

## Executive summary

tasqx has a much stronger behavioral test suite and contract discipline than most projects of this size. Input validation, error codes, documentation drift guards, terminal degradation, atomic event/state writes, and mutation testing are all treated seriously. The default workspace test suite currently exposes 547 tests, and both the normal test run and the repository's default `clippy -D warnings` gate pass.

The principal risk is concurrency. The storage layer advertises serialized writers through `BEGIN IMMEDIATE`, but many mutations load the task (and sometimes validate `_rev`) **before** starting that transaction. Two independent CLI processes can therefore read the same revision, serialize only after both reads, and then apply stale derived values in sequence. This can lose updates, make revisions move backward, duplicate lifecycle events, and spawn recurrence instances more than once. This is the first issue to fix.

The next tier is operational: database read errors are sometimes converted into ordinary absence, daemon/watch delivery can silently lose state changes, and list/report/export issue per-task queries after loading the entire table. These are manageable at small personal-task volumes but are not enterprise-grade failure or scaling behavior.

## Findings index

| Priority | Count | Theme |
|---|---:|---|
| Critical | 2 | stale concurrent writes; swallowed store errors |
| Medium | 5 | query scaling; watch consistency; daemon resilience; unbounded connection threads; architecture/test seam |
| Low | 3 | token semantics; formatting policy; missing automated dependency/coverage evidence |

Implementation-ready tasks are in:

- `TODO_CRITICAL.md`
- `TODO_MEDIUM.md`
- `TODO_LOW.md`

## Recommended order

1. Move every mutation's authoritative read/validation into its `BEGIN IMMEDIATE` transaction and add deterministic two-connection tests.
2. Make configuration reads fallible instead of translating SQLite errors to `None`.
3. Fix watch request/event coordination and make daemon background failures observable.
4. Replace task-list/report/export N+1 reads with set-based snapshots; measure before choosing deeper filter-SQL work.
5. Extract a transaction-scoped mutation layer only as far as needed to make steps 1-2 hard to regress.

## Evidence gathered

- `cargo test --workspace --all-targets --no-fail-fast`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo clippy --workspace --all-targets -- -W clippy::too_many_lines -W clippy::cognitive_complexity -W clippy::large_stack_frames`: reported 16 long functions, including `task_modify` (135 lines), `store_import` (245), `tool_specs` (203), and `execute` (125).
- `cargo fmt --all -- --check`: failed with a large diff. CI explicitly documents the non-rustfmt house style, so this audit did not rewrite it.
- Coverage could not be measured: `cargo llvm-cov` is not installed and no coverage job exists in CI.
- Dependency vulnerability and license checks could not be run locally: `cargo audit` and `cargo deny` are not installed and no equivalent CI job exists.

Rust/SQLite reference points used during the review: `BEGIN IMMEDIATE` starts the write transaction immediately; reads performed before it are outside that serialization boundary. Clippy's standard `too_many_lines` threshold is 100 and its cognitive-complexity threshold defaults to 25.

## What is already good

- Mutations generally keep state and event-log writes in one SQLite transaction.
- JSON parameter extraction is centralized and rejects wrong types rather than treating them as absent.
- The filter grammar, status sets, documentation, MCP schemas, and CLI surface have unusually good drift guards.
- CLI error codes and machine-readable output are intentional and tested end to end.
- Terminal output handles Unicode cell widths, escape injection, broken pipes, and terminal restoration thoughtfully.
- Mutation testing is scoped and its known equivalent/time-out behavior is documented honestly.

## Review boundaries

This was a static review plus existing automated checks. It did not alter production code, benchmark large real stores, fuzz the filter/import parsers, or run Linux/macOS-specific daemon behavior from this Windows workspace. Findings that depend on those activities are phrased as gaps and acceptance tests, not as fabricated defects.
