# Working on tasqx

`DESIGN.md` is the spec and carries the decision log (§12, D1 onwards). It is the
authority; this file is only the things that will bite you before you have read
it.

One JSON API, and every surface is a client of it. `dispatch::PARAMS` is the
method table, `Engine` implements the methods, and the CLI, the MCP server and
the HTML report all go through the same dispatch. There is exactly one dispatch
table — if you find yourself adding a second path to the data, you have taken a
wrong turn.

## Gates

All of these run in CI on Linux, Windows and macOS. Run them before you claim
anything is done.

```console
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo test --workspace --all-targets --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps --all-features
```

Stock stable rustfmt is the sole formatter and there is deliberately no
`rustfmt.toml` (`docs/specs/2026-07-20-rustfmt-policy-design.md`). Note that
`CODE_REVIEW.md` still describes a "non-rustfmt house style" — that audit is a
dated snapshot from before the policy landed, not current guidance.

`-D warnings` on the *test* job is load-bearing, not tidiness: an edit once
separated a `#[test]` from its function, the test silently stopped running, and
`function is never used` was the only signal anybody had.

`--no-fail-fast` is mandatory. cargo stops after the first failing test *binary*,
so one break hides every failure in the largest suite and turns one red run into
a sequence of them.

Advisory, not blocking: `cargo mutants` (weekly cron, see
`docs/mutation-testing.md`), coverage (report only, no threshold), `cargo deny`
(`docs/dependency-policy.md`).

## Things that fail the build for non-obvious reasons

**Docs drift is a compile error.** The `tasqx docs` pages render *from* the
`VERBS`/`METHODS` tables in `crates/tasqx-cli/src/docs.rs`, which tests assert
equal to clap's subcommand **and alias** tables, `core.capabilities`, and
`crate::CLEARABLE`. Add a verb, an alias or a `--clear` field without
documenting it and the build goes red. That is the guard working.

**Some gates `include_str!` files outside `src/`.** `doc_gate_tests` in
`crates/tasqx-core/src/lib.rs` embeds `.github/workflows/ci.yml`,
`docs/mutation-testing.md` and `.cargo/mutants.toml`, so editing CI or those
docs can redden a core unit test. Renaming or moving them is a compile error on
purpose.

**MSRV is measured, not declared.** The floor in `Cargo.toml` was found by
compiling on real toolchains; the binding constraint declares no `rust-version`
and so is invisible to a scan. Re-measure after any lockfile bump. Do not reason
about the number.

**The conformance suite freezes the JSON API's shape**
(`crates/tasqx-core/tests/conformance.rs`, D56), deriving its method floor from
`dispatch::PARAMS`. MCP tool *names,
descriptions and input schemas* stay free to move; MCP tool *results* do not.

## When a decision lands

Add it to `DESIGN.md` §12 as the next D-number with the ruling and the one-line
why, then **walk the phase tables in §11**. A phase table is read as a checklist,
so a line in it that contradicts a §12 ruling is not a stale note — it is a plan
nobody can execute. The note closing §11 records that this has already cost time
twice.

Do not date figures into prose. Counts and measurements go stale silently and
then read as present tense; state how to re-derive them instead.

## Commits

Subject names the defect or the behaviour in the repo's own voice, lowercase,
with the D-number in parens when a decision applies. The body reproduces the
problem before describing the fix.

```
fix(pick): the dash it never restored, the rows it never drew, and the store a refusal wrote
feat(undo): a safety net that appends its inverse, over four operations and no more (D54)
test(filter): kill the spacing_hint survivor, one test per disjunct
```

Not `fix: resolve pick rendering issue`. Read `git log` before writing one.

## Test-first

Fixes land with a test that was **watched fail against the original code**. When
a guard is added, verify it bites by injecting the drift it claims to catch. A
test asserted but never seen red is a test whose failure mode is unknown.

## Task tracking

Work is tracked in tasqx itself, not in files — see
`.claude/skills/tasqx-workflow/SKILL.md`. No `TODO.md`, no checklists in the
repo. Note that the installed `tasqx` binary is a build of this tree, so after
changing anything under `crates/` it needs
`cargo install --path crates/tasqx-cli --force` to reflect your edits.
