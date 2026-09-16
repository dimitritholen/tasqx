# Contributing to tasqx

Thanks for looking. This file is the practical part: how the repository is laid
out, what has to pass before a change is done, and the handful of things that
fail the build for reasons that are not obvious from the diff.

`DESIGN.md` is the specification and carries the decision log (§12, D1 onwards).
When this file and `DESIGN.md` disagree, `DESIGN.md` wins — and the disagreement
is a bug worth reporting.

## Layout

| Path | What it is |
|---|---|
| `crates/tasqx-core` | The engine: storage, the JSON API (`dispatch::PARAMS` is the method table), the daemon and the MCP server. |
| `crates/tasqx-cli` | The `tasqx` binary: argument parsing, rendering, the TUI screens and the in-binary docs. |
| `docs/wiki`, `docs/guides` | User documentation. Drift guards in `crates/tasqx-cli/tests/` check both. |
| `docs/maintainers` | How the project itself is run: terminal house style, mutation testing, dependency policy, the Homebrew tap. |
| `scripts/` | Release helpers, installer smoke tests and screen-capture tools. |

There is one JSON API, and every surface — the CLI, the MCP server, the HTML
report — is a client of the same dispatch. If a change needs a second path to the
data, it has taken a wrong turn.

## Building and running a dev build

```console
cargo build
TASQX_DB="$PWD/target/scratch.db" target/debug/tasqx --no-daemon init demo
```

Always point a dev build at a scratch store with `TASQX_DB` and pass
`--no-daemon`. Without both, a dev build opens your real store, or routes through
a daemon serving it.

`TASQX_NOW` pins the clock. Set it to an RFC 3339 instant and that is what the
process reads everywhere: the dates the CLI spells (`due tomorrow`, `2d ago`, an
agenda heading, a chart's axis), the urgency it scores, **and the stamps the
engine writes** — `created`, `completed`, `active_since`, every event. So a
store generated at instant P and rendered at instant P is the same bytes on any
calendar day, writes included (DESIGN.md D148).
`scripts/demo-store.py` reads the same variable and passes it on.

That last part makes it dangerous, so treat it as live ammunition: a pin left
exported in your shell backdates real work. `tasqx about` states the pin
whenever one is set, right under the `times UTC` row, and a value that is not an
RFC 3339 instant is refused rather than quietly ignored — `tasqx` exits 2 (the
`bad_request` code), and `scripts/demo-store.py` exits 2 with it, so one
pipeline answers one mistake with one code.

It is a capture and testing hook, not a user feature. There are two clock
gateways and no third: `tasqx_core::clock::now` is the workspace's single
physical wall-clock read, and `tasqx_cli::clock::now` delegates to it after
validating the variable once at start-up. Each crate carries a guard test that
fails on any other `Timestamp::now`, `Zoned::now` or `Uuid::now_v7` under its
`src/`, and the variable is deliberately absent from `tasqx docs`, the wiki and
the guides.

To use your build as your everyday `tasqx`, install it over the released one:

```console
cargo install --path crates/tasqx-cli --force
```

## Gates

CI runs all four on Linux, Windows and macOS. Run them before calling a change done:

```console
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo test --workspace --all-targets --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

- **Stock stable rustfmt** is the only formatter. There is deliberately no
  `rustfmt.toml`.
- **`-D warnings` on the test run is load-bearing.** A `#[test]` attribute once
  got separated from its function, and the test silently stopped running. The
  only signal was a `function is never used` warning.
- **`--no-fail-fast` is mandatory.** Without it, cargo stops after the first
  failing test binary, so one break hides every other failure.

These checks are advisory, not blocking:

- `cargo mutants` runs on a weekly schedule. See
  [`docs/maintainers/mutation-testing.md`](docs/maintainers/mutation-testing.md).
- Coverage is reported, with no threshold.
- `cargo deny` checks dependencies. See
  [`docs/maintainers/dependency-policy.md`](docs/maintainers/dependency-policy.md).

## Things that fail the build for non-obvious reasons

- **Docs drift is a compile error.** `tasqx docs` renders its pages from the
  `VERBS`/`METHODS` tables in `crates/tasqx-cli/src/docs.rs`. Tests assert those
  tables equal four other lists:
  - clap's subcommand and alias tables
  - `core.capabilities`
  - `CLEARABLE`
  - the live MCP roster

  Add a verb, an alias or a `--clear` field without documenting it, and the build
  goes red.
- **Some tests `include_str!` files outside `src/`.** These are `ci.yml`,
  `docs/maintainers/mutation-testing.md`, `.cargo/mutants.toml`,
  `docs/maintainers/terminal-style.md` and `DESIGN.md`. Editing them can redden a
  unit test, and moving one is a compile error on purpose.
- **The README, wiki and guides are guarded.** Tests check relative links, the
  MCP tool roster, exit codes and command samples against the binary.
- **The conformance suite freezes the JSON API's shape**
  (`crates/tasqx-core/tests/conformance.rs`). New methods and new fields are
  fine; renaming or removing a response field is not. MCP tool names,
  descriptions and input schemas may change; MCP tool *results* may not.
- **The MSRV is measured, not declared.** The floor in `Cargo.toml` was found by
  compiling on real toolchains. Re-measure it after any lockfile bump rather than
  reasoning about the number.

## Terminal screens

[`docs/maintainers/terminal-style.md`](docs/maintainers/terminal-style.md) is the
house style for everything tasqx prints: how a row is weighted, what the left
rail carries, how a date is spelled. Read it before laying out or changing a
screen. Its Contract table is checked against the renderer by a test.

## Tests first

A fix lands with a test that was **watched fail against the original code**. When
you add a guard, prove it bites by injecting the drift it claims to catch. A test
nobody has seen go red has an unknown failure mode.

## Decisions

When a change settles a design question, add it to `DESIGN.md` §12 as the next
D-number: the ruling, plus a line saying why. Then walk the phase tables in §11,
because those tables are read as a checklist. A line that contradicts a §12
ruling is a plan nobody can carry out.

Do not write dated figures (test counts, sizes, timings) into prose. They go stale
silently and then read as present tense. Say how to re-derive the figure instead.

## Commit messages

The subject names the defect or the behaviour, lowercase, in a conventional-commit
prefix. Add the D-number in parentheses when a decision applies. The body
reproduces the problem before it describes the fix.

```
fix(pick): the dash it never restored, the rows it never drew, and the store a refusal wrote
feat(undo): a safety net that appends its inverse, over four operations and no more (D54)
test(filter): kill the spacing_hint survivor, one test per disjunct
```

Not `fix: resolve pick rendering issue`. `git log` has plenty more examples.

## Releasing

Releases are cut from `main` by pushing a tag:

1. `scripts/bump-version.sh` prints the next version, computed from the
   conventional commits since the last tag. `--apply` writes it to `Cargo.toml`
   and refreshes `Cargo.lock`.
2. Add a `## X.Y.Z` section to `CHANGELOG.md`. A test fails the build when the
   workspace version has no section.
3. Commit, push `main`, and wait for CI to go green.
4. Tag and push: `git tag -a vX.Y.Z -m "tasqx X.Y.Z" && git push origin vX.Y.Z`.

The tag runs `.github/workflows/release.yml`:
- It checks that the tag matches `Cargo.toml`.
- It tests and builds four targets.
- It publishes the GitHub release, with that version's `CHANGELOG.md` section
  above the generated notes.
- It opens pull requests that bump the Homebrew tap and the Scoop bucket. Each
  merges itself once that repository's own CI has installed the new version. See
  [`docs/maintainers/homebrew-tap.md`](docs/maintainers/homebrew-tap.md).

## License

By contributing you agree that your contribution is licensed under the project's
[FSL-1.1-MIT](LICENSE.md) license.
