#!/usr/bin/env bash
# Stop-gate for agent sessions: the four CLAUDE.md gate commands, verbatim.
# A Stop hook blocks the turn ONLY on exit code 2, so every failure path
# below must funnel into that — a bare `&&` chain would fail with 1 and
# never bite. The harness overrides after 8 consecutive blocks; CI stays
# the real gate.
set -u
cd "$(dirname "$0")/.." || exit 0

# Only pay the cargo runs when something gate-relevant is dirty: source,
# manifests, the lockfile, workflows and docs/mutation-testing.md (both
# include_str!'d by doc_gate_tests in tasqx-core).
if ! git status --porcelain | awk '{print $NF}' | grep -Eq \
  '\.rs$|\.toml$|^Cargo\.lock$|\.github/workflows/|^docs/mutation-testing\.md$'; then
  exit 0
fi

out=$(mktemp)
trap 'rm -f "$out"' EXIT
fail() {
  echo "gate failed: $1 — fix before ending the turn. Last output:" >&2
  tail -n 60 "$out" >&2
  exit 2
}

cargo fmt --all -- --check >"$out" 2>&1 \
  || fail 'cargo fmt --all -- --check'
cargo clippy --workspace --all-targets -- -D warnings >"$out" 2>&1 \
  || fail 'cargo clippy --workspace --all-targets -- -D warnings'
RUSTFLAGS="-D warnings" cargo test --workspace --all-targets --no-fail-fast >"$out" 2>&1 \
  || fail 'RUSTFLAGS="-D warnings" cargo test --workspace --all-targets --no-fail-fast'
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features >"$out" 2>&1 \
  || fail 'RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features'
exit 0
