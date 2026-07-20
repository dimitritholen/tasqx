# Enforceable Rust formatting policy implementation plan

## Task 1: Establish the baseline

- [x] Run `cargo fmt --all -- --check` and observe the existing failure.
- [x] Record stock stable rustfmt as the accepted policy with no custom configuration.

## Task 2: Apply the mechanical rewrite

- [x] Run `cargo fmt --all` over the complete workspace.
- [x] Confirm the mechanical commit changes only Rust source files.
- [x] Commit the formatter output without CI or behavior edits.

## Task 3: Enforce the policy

- [x] Add a dedicated CI formatting job using the stable rustfmt component.
- [x] Remove the obsolete CI rationale for intentionally skipping rustfmt.
- [x] Verify the workflow command matches the documented local command.

## Task 4: Verify and integrate

- [x] Run format check, full workspace tests, Clippy, and diff checks.
- [x] Update Low #2 with verification evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
