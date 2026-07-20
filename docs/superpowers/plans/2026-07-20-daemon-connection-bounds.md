# Daemon connection bounds implementation plan

## Task 1: Establish admission behavior

- [x] Add failing unit tests for a saturating scoped admission counter and permit release.
- [x] Add a failing integration stress test for overload rejection and admitted-client continuity.
- [x] Teach the client to surface an id-less transport rejection message.

## Task 2: Bound connection lifetime and I/O

- [x] Configure native receive/send timeouts on Unix.
- [x] Add a Windows connection watchdog that cancels blocking I/O on shutdown, idle expiry, or a stalled write.
- [x] Keep the admission permit until reader, writer, and optional watchdog cleanup completes.

## Task 3: Harden local socket access

- [x] Force Unix socket permissions to mode `0600` after bind.
- [x] Add a Unix-only permission regression test.
- [x] Document custom-path and Windows named-pipe limitations.

## Task 4: Verify and integrate

- [x] Run focused daemon unit/integration and stress tests.
- [x] Run full workspace tests, Clippy, and diff checks.
- [x] Update Medium #4 and the design status with evidence.
- [x] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
