# tasqx

A terminal task manager for people who live in the shell — Taskwarrior's power without its friction, and built so an AI agent is a first-class user rather than a bolted-on afterthought.

```console
$ tasqx add Ship the release notes due:friday +docs !high --project work
Added #42  ·  pending  ·  urgency 11.7  ·  work
  Ship the release notes

$ tasqx next
#42  (urgency 11.7)  Ship the release notes

$ tasqx why 42
Why #42 has urgency 11.7
  priority         6.00
  due_proximity    5.40
  age              0.30
  = total          11.7
```

## Why it exists

Every surface — the CLI, the HTML report, the MCP server for agents — is a **client of one JSON API**. There is no logic in the CLI that an agent cannot reach, and no agent capability the CLI lacks. That constraint is the whole design: the API is the load-bearing artifact, and UIs are replaceable.

Your data is a single SQLite file on your disk. No account, no cloud, no lock-in. Every mutation is recorded in an append-only event log, which is what makes cancellation reversible, agent actions auditable, and future sync an addition rather than a migration.

## Install

Requires Rust 1.80 or newer (enforced by `rust-version` in `Cargo.toml`).

```console
git clone git@github.com:dimitritholen/tasqx.git
cd tasqx
cargo install --path crates/tasqx-cli --force
```

## Getting started

```console
tasqx init work              # a project is just a name
tasqx use work               # make it the default
tasqx add Buy milk           # lands in the default project
tasqx                        # bare `tasqx` lists your working set
tasqx done 1
```

`tasqx manual` is a full terminal manual; `tasqx <verb> -h` gives per-command help with runnable examples. `tasqx docs` renders the same material as a self-contained HTML page.

## What it does

| | |
|---|---|
| **Capture** | Inline sugar — `+tag`, `project:p`, `!high`, `due:friday`, `est:4h`, `repeat:"every monday"`, `remind:-1h` |
| **Dates** | Natural language: `tomorrow`, `friday 17:00`, `in 3 days`, `eom`, `at 6pm`, `-1d`, plus RFC3339 |
| **Lifecycle** | `start`/`stop` with time tracking, `done`, `cancel`, `reopen`, dependencies with automatic blocked/unblocked |
| **Recurrence** | `every N days`, `weekly on Mon,Wed`, `monthly on day 15`, `monthly on the 2nd tuesday` |
| **Reminders** | Anchored to `due` (`-1h`, re-anchors when the date moves) or absolute, fired by an optional daemon |
| **Filters** | `project:work status:pending +api -infra due.before:friday`, with `and`/`or` and parentheses |
| **Reports** | Grouped summaries, throughput / heatmap / burndown charts in the terminal, and a themed self-contained HTML report |
| **Themes** | Five built-ins, user themes, graceful degradation from truecolor down to a dumb terminal |
| **Agents** | `tasqx mcp serve` — an MCP server over stdio with read/write scoping that fails closed |
| **Scripting** | Every command speaks human-readable text *and* `--json`; exit codes are contract, not decoration |

## For agents

```console
tasqx mcp token --scope read     # or --scope write
tasqx mcp serve
```

Tokens encode a scope, and a read-only session never sees the write tools advertised at all. There is deliberately no bulk-delete tool: cancellation goes through the normal reversible, logged path, so an agent cannot quietly destroy work.

The same API is available directly:

```console
echo '{"tasqx":"1","method":"task.list","params":{"filter":"@working"}}' | tasqx api
```

## Development

```console
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo mutants                    # see docs/mutation-testing.md
```

CI runs the suite on Linux and Windows, builds the off-by-default `notify-os` feature, and gates on clippy. `RUSTFLAGS=-D warnings` is deliberate: an orphaned `#[test]` once stopped a guard from running and surfaced only as a warning nobody read.

The suite includes a layer of **drift guards** — tests that fail the build when documentation and reality disagree. Every CLI flag must appear in its verb's usage line, every documented example must parse, every `RunKind::Safe` example is executed for real, and status sets across SQL, the filter DSL and Rust all derive from one enum. Documentation rot is a build failure here, not a reader's problem.

`DESIGN.md` is the specification and carries the numbered decision log (D1–D24) explaining why things are the way they are.

## Status

Daily-usable. 316 tests. Not yet built: a full TUI, plugins, and sync — all three are specified in `DESIGN.md` and none has data-model implications, by design.
