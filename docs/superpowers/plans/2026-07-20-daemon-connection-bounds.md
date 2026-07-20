# Daemon connection bounds implementation plan

## Task 1: Establish admission behavior

- [ ] Add failing unit tests for a saturating scoped admission counter and permit release.
- [ ] Add a failing integration stress test for overload rejection and admitted-client continuity.
- [ ] Teach the client to surface an id-less transport rejection message.

## Task 2: Bound connection lifetime and I/O

- [ ] Configure receive and send timeouts before splitting a stream.
- [ ] Poll shutdown on receive timeout and expire clients after 15 minutes without an inbound frame.
- [ ] Keep the admission permit until reader/writer cleanup completes.

## Task 3: Harden local socket access

- [ ] Force Unix socket permissions to mode `0600` after bind.
- [ ] Add a Unix-only permission regression test.
- [ ] Document custom-path and Windows named-pipe limitations.

## Task 4: Verify and integrate

- [ ] Run focused daemon unit/integration and stress tests.
- [ ] Run full workspace tests, Clippy, and diff checks.
- [ ] Update Medium #4 and the design status with evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
