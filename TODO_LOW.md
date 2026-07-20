# 🟢 Low Priority Issues

**Source:** Code Audit 2026-07-20  
**Estimated Total Effort:** 1-2 days

---

## #1: MCP “opaque tokens” are unchecked scope strings, not capability credentials

**Severity:** LOW  
**Category:** Security semantics / clean API  
**File:** `crates/tasqx-core/src/mcp.rs:64-78`  
**Estimated Effort:** 2-4 hours

### Problem

`mint_token` appends a UUID, but `from_token` checks only whether the suffix starts with `read_` or `write_`. Values such as `tasqx_mcp_write_` or `tasqx_mcp_write_anything` are accepted. The UUID is never validated or looked up, so the token is forgeable and carries no authority beyond an explicit scope flag.

Because the MCP server uses stdio and the launcher supplies the token, this is not currently a remote authentication bypass. It is still misleading API/security language and may become dangerous if reused for a socket transport.

### Acceptance Criteria

- [ ] Decide and document whether this is operator intent or authentication.
- [ ] If it is operator intent, replace token minting with an explicit `--scope read|write` and remove false opacity/credential language.
- [ ] If it is authentication, validate and compare a real secret/capability rather than trusting a self-declared prefix.
- [ ] Invalid/truncated/random tokens are covered by tests.
- [ ] No future socket transport inherits the current parser as an auth boundary.

### Recommended Approach

Prefer the simpler `--scope` design unless there is a real party from whom scope must be protected. Cryptographic/token storage machinery would be YAGNI for the current stdio process model.

### Files to Modify

- `crates/tasqx-core/src/mcp.rs`
- `crates/tasqx-cli/src/lib.rs`
- MCP tests and documentation

---

## #2: Formatting is intentionally nonstandard and not mechanically enforceable

**Severity:** LOW  
**Category:** Code quality / maintainability  
**File:** `.github/workflows/ci.yml:50-66`  
**Estimated Effort:** 2-4 hours plus one mechanical commit

### Problem

`cargo fmt --all -- --check` fails across the tree. CI explicitly declines rustfmt to preserve compact one-line literals. The result is a style that reviewers and agents cannot reproduce mechanically; unrelated edits can create broad formatting churn, and standard Rust tooling reports the repository as unformatted.

This is not a runtime defect, and a whole-tree format change should not be smuggled into another fix.

### Acceptance Criteria

- [ ] Team explicitly accepts either rustfmt or a documented, enforceable alternative.
- [ ] If adopting rustfmt, apply it in one behavior-free commit and add `cargo fmt --all -- --check` to CI.
- [ ] Any rustfmt configuration is minimal and stable-compatible.
- [ ] If retaining house style, document the exact formatter/check that reproduces it; prose alone is not a gate.

### Recommended Approach

Adopt stock rustfmt. The compact literal preference does not outweigh deterministic ecosystem tooling for a multi-agent/multi-developer codebase.

### Files to Modify

- Entire Rust tree (mechanical only, if accepted)
- `.github/workflows/ci.yml`
- Optional `rustfmt.toml`

---

## #3: CI has no measured coverage or dependency/license vulnerability gate

**Severity:** LOW  
**Category:** Test quality / supply chain  
**File:** `.github/workflows/ci.yml:15-112`  
**Estimated Effort:** 4-8 hours

### Problem

The project has 547 tests and useful mutation testing, but no line/branch coverage report. The audit's 80%/70% target cannot be evaluated. `cargo llvm-cov`, `cargo audit`, and `cargo deny` are not installed locally, and CI has no equivalent jobs. Dependency duplication is small and explainable, but vulnerability/license status is currently assumed rather than continuously evidenced.

Coverage percentage should not become a vanity target; the concrete concurrency and daemon gaps in the medium/critical files matter more. The report is useful for finding untouched code, not declaring correctness.

### Acceptance Criteria

- [ ] CI publishes line and branch coverage, with exclusions documented for generated/static documentation if appropriate.
- [ ] Start with report-only coverage; set thresholds only after establishing a baseline and inspecting meaningful gaps.
- [ ] CI checks RustSec advisories and the project's intended license/source policy with pinned tooling/action versions.
- [ ] Findings have an explicit allowlist format with owner/reason/expiry rather than blanket ignores.
- [ ] Critical concurrency/error-path tests are added regardless of aggregate percentage.

### Recommended Approach

Add `cargo llvm-cov` reporting and either `cargo audit` plus a license check, or a single configured `cargo deny` job. Pin install versions. Keep the existing mutation job focused rather than broadening it until the high-risk transaction/daemon paths are directly testable.

### Files to Modify

- `.github/workflows/ci.yml`
- `deny.toml` and/or advisory configuration
- Coverage configuration/documentation

---

## Progress Tracking

- [ ] Issue #1: Clarify or replace MCP token semantics
- [ ] Issue #2: Adopt an enforceable formatting policy
- [ ] Issue #3: Add coverage and dependency/license evidence

**Total:** 0/3 completed
