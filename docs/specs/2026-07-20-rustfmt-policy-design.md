# Enforceable Rust formatting policy — design

**Date:** 2026-07-20
**Status:** approved for implementation
**Scope:** Low #2 only: make Rust formatting deterministic and CI-enforced.

## Decision

The repository adopts stock stable rustfmt. `cargo fmt --all` is the sole formatter and `cargo fmt --all -- --check` is the gate. No `rustfmt.toml` is added: the default stable style is reproducible, familiar to Rust contributors and agents, and avoids creating a second house policy.

All existing Rust sources, tests, build scripts, and examples are formatted in one behavior-free mechanical commit. The CI change lands separately and removes the comment that explicitly excluded rustfmt. A dedicated `fmt` job installs the stable rustfmt component and runs the same check developers run locally.

## Safety and compatibility

- The pre-format merged tree has already passed the full workspace test and Clippy gates.
- The formatting commit contains only output produced by `cargo fmt --all`.
- Full tests and strict Clippy run again after formatting to catch accidental semantic or macro-sensitive changes.
- Markdown, generated HTML strings, Cargo metadata, and non-Rust assets are outside the formatter's scope.

## Verification

- The previously failing `cargo fmt --all -- --check` passes.
- CI contains an executable formatting gate using the exact same command.
- Full workspace tests, Clippy with warnings denied, and diff checks pass.
