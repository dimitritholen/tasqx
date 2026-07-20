# Daemon background health implementation plan

## Task 1: Expose event-pump failures

- [ ] Add a failing test that damages the event schema and expects `pump` to return an error without advancing the watermark.
- [ ] Change `max_event_rowid` to `Result<i64, ApiError>`.
- [ ] Change `pump` to `Result<(), ApiError>` and collect mapped rows as `Result<Vec<_>, _>` before broadcasting.
- [ ] Propagate the result through immediate pump callers and reminder scheduling.

## Task 2: Supervise background components

- [ ] Add a deterministic integration test that damages the event table after startup and expects the serve result to report a poller failure.
- [ ] Add a supervisor channel shared by the poller/reminder threads and the serve loop.
- [ ] Return non-`WouldBlock` accept errors with context.
- [ ] Ensure cleanup runs on every serve-loop exit.

## Task 3: Rate-limit transient reminder diagnostics

- [ ] Introduce a small error-transition tracker with recovery reset.
- [ ] Log identical reminder rebuild/fire failures only on entry; log changed failures once.
- [ ] Preserve retry/watermark behavior and existing notifier semantics.

## Task 4: Verify and integrate

- [ ] Run focused daemon unit/integration tests.
- [ ] Run full workspace tests, Clippy, and diff checks.
- [ ] Update Medium #3 and the design status with evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.

