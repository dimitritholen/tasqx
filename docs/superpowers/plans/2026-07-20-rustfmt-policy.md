# Enforceable Rust formatting policy implementation plan

## Task 1: Establish the baseline

- [x] Run `cargo fmt --all -- --check` and observe the existing failure.
- [ ] Record stock stable rustfmt as the accepted policy with no custom configuration.

## Task 2: Apply the mechanical rewrite

- [ ] Run `cargo fmt --all` over the complete workspace.
- [ ] Confirm the mechanical commit changes only Rust source files.
- [ ] Commit the formatter output without CI or behavior edits.

## Task 3: Enforce the policy

- [ ] Add a dedicated CI formatting job using the stable rustfmt component.
- [ ] Remove the obsolete CI rationale for intentionally skipping rustfmt.
- [ ] Verify the workflow command matches the documented local command.

## Task 4: Verify and integrate

- [ ] Run format check, full workspace tests, Clippy, and diff checks.
- [ ] Update Low #2 with verification evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
