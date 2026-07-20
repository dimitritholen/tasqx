# Bounded task snapshots implementation plan

## Task 1: Establish the performance contract

- [ ] Add a failing regression that expects a shared snapshot loader and a task-count-independent statement count.
- [ ] Add an ignored 1,000/10,000-task fixture covering list, report, and export.

## Task 2: Implement the shared snapshot loader

- [ ] Add the private `TaskSnapshot` aggregate.
- [ ] Load task rows, tags, blocked ids, dependency ids, and annotations with five set-based statements.
- [ ] Preserve deterministic tag, dependency, and annotation ordering.

## Task 3: Migrate bulk readers

- [ ] Make `task.list` filter, sort, project, and limit snapshots without point queries.
- [ ] Make `report.summary` aggregate snapshots without point queries.
- [ ] Make `store.export` filter and serialize snapshots without point queries.

## Task 4: Verify and integrate

- [ ] Run focused snapshot and list/report/export tests.
- [ ] Run full workspace tests, Clippy, and diff checks.
- [ ] Update Medium #1 and the design status with evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
