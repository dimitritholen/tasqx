# Dependency policy

`cargo deny --all-features check` is the executable dependency gate. The committed `deny.toml` permits only reviewed licenses and sources and checks the live RustSec advisory database.

## Exceptions

The repository currently has no advisory or license exceptions. A future exception must be scoped to one advisory/crate and include all three fields in its adjacent rationale:

```text
owner=@maintainer; reason=<specific mitigation or migration blocker>; expires=YYYY-MM-DD
```

The expiry is a review deadline, not permission to leave the exception indefinitely. The change must also state the removal condition. Blanket advisory IDs, wildcard crate ranges, unbounded git/registry sources, and rationale such as “CI is red” are not acceptable.

For an advisory, use cargo-deny's structured table form rather than a bare ID:

```toml
ignore = [
  { id = "RUSTSEC-YYYY-NNNN", reason = "owner=@name; reason=<specific mitigation>; expires=YYYY-MM-DD" },
]
```

For a license exception, add the narrowest `licenses.exceptions` package constraint and place the same owner/reason/expiry metadata directly above it. Removing or renewing an exception is an explicit reviewed change.

## Coverage policy

Coverage is diagnostic evidence, not a correctness score. CI publishes line and branch reports without a threshold. Review the uncovered code by risk—transaction ordering, daemon supervision, cancellation, and error paths first—before considering any aggregate gate.
