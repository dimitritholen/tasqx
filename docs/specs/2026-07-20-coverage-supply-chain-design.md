# Coverage and supply-chain evidence — design

**Date:** 2026-07-20
**Status:** implemented and verified
**Scope:** Low #3 only: publish coverage evidence and enforce dependency advisory/license/source policy.

## Coverage decision

CI gains a Linux coverage job using `cargo-llvm-cov` 0.8.6, installed by immutable `taiki-e/install-action` v2.83.4. Branch instrumentation currently requires nightly, so nightly plus `llvm-tools-preview` is isolated to this reporting job; normal build, test, format, and Clippy jobs remain stable.

The job runs all workspace targets with `--branch`, then publishes machine-readable JSON, an HTML report, and a text summary as a GitHub Actions artifact and job summary. It is report-only: no aggregate threshold is set until the baseline has been inspected for meaningful gaps. Test code is excluded by cargo-llvm-cov's standard report behavior; generated/static documentation is not separately instrumented because it is embedded data rather than executable Rust logic.

## Supply-chain decision

CI gains a cargo-deny gate pinned to `EmbarkStudios/cargo-deny-action` v2.1.1 (cargo-deny 0.20.2). `deny.toml` covers the complete all-features graph and enforces:

- current RustSec advisories and yanked releases;
- an explicit permissive-license allowlist;
- crates.io-only dependencies unless a reviewed source is added;
- duplicate-version warnings rather than an immediate blanket failure, because current duplicates must be evaluated by impact rather than count.

There are no initial advisory or license exceptions. Any future exception must be narrow and carry owner, rationale, and ISO expiry metadata as documented in `docs/dependency-policy.md`; blanket ignores are prohibited.

## Existing high-risk evidence

Aggregate coverage does not replace risk-directed tests. The transaction suite covers competing lifecycle/default-project/revision mutations; daemon tests cover retained events, supervised background failures, event decode watermarking, client admission bounds, idle cancellation, and socket permissions. Those tests remain mandatory regardless of the reported percentage.

## Verification

- Run cargo-deny 0.20.2 locally against the committed configuration and resolve every finding explicitly.
- Run cargo-llvm-cov 0.8.6 locally with nightly branch instrumentation and capture the initial line/branch baseline when supported by the host.
- Validate workflow syntax structurally and ensure artifacts are uploaded even when a report step fails.
- Run format check, full workspace tests, strict Clippy, and diff checks.

## Primary references

- https://github.com/taiki-e/cargo-llvm-cov
- https://github.com/taiki-e/install-action
- https://github.com/EmbarkStudios/cargo-deny-action
- https://embarkstudios.github.io/cargo-deny/
