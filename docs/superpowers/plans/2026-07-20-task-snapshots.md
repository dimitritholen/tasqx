# Bounded task snapshots implementation plan

## Task 1: Establish the performance contract

- [x] Add a failing regression that expects a shared snapshot loader and a task-count-independent statement count.
- [x] Add an ignored 1,000/10,000-task fixture covering list, report, and export.

## Task 2: Implement the shared snapshot loader

- [x] Add the private `TaskSnapshot` aggregate.
- [x] Load task rows, tags, blocked ids, dependency ids, and annotations with five set-based statements.
- [x] Preserve deterministic tag, dependency, and annotation ordering.

## Task 3: Migrate bulk readers

- [x] Make `task.list` filter, sort, project, and limit snapshots without point queries.
- [x] Make `report.summary` aggregate snapshots without point queries.
- [x] Make `store.export` filter and serialize snapshots without point queries.

## Task 4: Verify and integrate

- [x] Run focused snapshot and list/report/export tests.
- [x] Run full workspace tests, Clippy, and diff checks.
- [x] Update Medium #1 and the design status with evidence.
- [x] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
