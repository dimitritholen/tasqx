# Coverage and supply-chain evidence implementation plan

## Task 1: Establish executable policy

- [x] Add `deny.toml` with explicit advisory, license, duplicate, and source policy.
- [x] Document the owner/reason/expiry format for future narrow exceptions.
- [x] Run pinned cargo-deny locally and resolve every initial finding without blanket ignores.

## Task 2: Publish coverage

- [x] Add a pinned nightly cargo-llvm-cov job with branch instrumentation.
- [x] Publish JSON, HTML, and text summary artifacts without a threshold.
- [x] Record the initial line/branch baseline or a precise local-platform limitation.

## Task 3: Gate the dependency graph

- [x] Add the pinned cargo-deny action as a required CI job.
- [x] Check advisories, licenses, bans/duplicates, and sources over all features.
- [x] Keep advisory failures gating rather than silently advisory.

## Task 4: Preserve risk-directed evidence

- [x] Document the existing transaction/concurrency and daemon/error-path regression tests.
- [x] Confirm those suites run in both the normal test matrix and coverage job.

## Task 5: Verify and integrate

- [x] Run format check, full workspace tests, Clippy, cargo-deny, and diff checks.
- [x] Update Low #3 with verification evidence.
- [x] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
