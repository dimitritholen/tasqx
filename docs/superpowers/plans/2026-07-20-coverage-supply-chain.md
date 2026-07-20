# Coverage and supply-chain evidence implementation plan

## Task 1: Establish executable policy

- [ ] Add `deny.toml` with explicit advisory, license, duplicate, and source policy.
- [ ] Document the owner/reason/expiry format for future narrow exceptions.
- [ ] Run pinned cargo-deny locally and resolve every initial finding without blanket ignores.

## Task 2: Publish coverage

- [ ] Add a pinned nightly cargo-llvm-cov job with branch instrumentation.
- [ ] Publish JSON, HTML, and text summary artifacts without a threshold.
- [ ] Record the initial line/branch baseline or a precise local-platform limitation.

## Task 3: Gate the dependency graph

- [ ] Add the pinned cargo-deny action as a required CI job.
- [ ] Check advisories, licenses, bans/duplicates, and sources over all features.
- [ ] Keep advisory failures gating rather than silently advisory.

## Task 4: Preserve risk-directed evidence

- [ ] Document the existing transaction/concurrency and daemon/error-path regression tests.
- [ ] Confirm those suites run in both the normal test matrix and coverage job.

## Task 5: Verify and integrate

- [ ] Run format check, full workspace tests, Clippy, cargo-deny, and diff checks.
- [ ] Update Low #3 with verification evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
