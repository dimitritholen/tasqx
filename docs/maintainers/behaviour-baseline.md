# Behaviour baseline

A green test suite proves the tests ran, not that they watch the behaviour a
de-bloat change touches. `scripts/docs-capture.sh` already renders the
documentation site's screens from the real binary against a pinned demo store
and fails on any byte difference (D149). `crates/tasqx-cli/docs-fixtures/baseline/`
is the same mechanism pointed at the whole user-visible surface instead of the
66 screens the guide shows, so that after a refactor the answer to "did
anything change" is a diff, not an opinion (D187).

## What it covers

One row of `crates/tasqx-cli/docs-fixtures/baseline/manifest.tsv` is one
captured screen, same seven columns as the site's own
`crates/tasqx-cli/docs-fixtures/manifest.tsv`. Between the two manifests:

- every top-level verb and subcommand's `--help` — the flag inventory, and so
  the feature inventory;
- `--json` for every read verb whose JSON shape no `api.<method>` row already
  covers;
- `--card` and `--card --ascii` for the four verbs that take them;
- `pipe-plain`: the render path a script or agent actually gets piping tasqx
  with no terminal behind it, as distinct from every `pipe`/`pipe-mut` row's
  forced coloured terminal layout;
- `tasqx docs --stdout`, one static page carrying the CLI reference, the API
  reference, the MCP tool reference, the object reference, the filter grammar
  and the config key list;
- an `initialize`-then-`tools/list` handshake piped into `tasqx mcp serve`, at
  both the read and write scope;
- default-mode output for every verb neither manifest showed before.

Run it the same way as the site's own:

```console
cargo build -p tasqx-cli
TASQX=target/debug/tasqx scripts/docs-capture.sh \
    --dir=crates/tasqx-cli/docs-fixtures/baseline --no-daemon    # regenerate
TASQX=target/debug/tasqx scripts/docs-capture.sh \
    --dir=crates/tasqx-cli/docs-fixtures/baseline --check --no-daemon
```

It is a second directory rather than more rows in the site's own manifest on
purpose: `crates/tasqx-cli/src/fixtures.rs` `include_str!`s every name the site
manifest lists, so a row added there ships inside `tasqx docs` whether or not
it belongs on a page a reader browses. The `--dir` flag exists so this corpus
can share the capture script without ever reaching the guide.

## How a de-bloat PR uses it

Capture the baseline on `main` before a cut lands, and again after:

- **#734, #735, #736, #737** remove or restructure code with no user-visible
  change promised. The pass condition is an **empty diff** against the
  baseline — that is the proof nothing broke and nothing was lost.
- **#732, #733, #738** remove or rewrite user-visible behaviour on purpose. The
  diff there is expected to be non-empty; the rule is that **every line of it
  is named in the task's closing annotation** as intended. An unexplained line
  is a regression, not a rewrite.

## What it does NOT cover, and why

An empty diff proves the CAPTURED surface is unchanged. These stay on human
judgement, which is exactly why **#738 is a decide-task with manual
verification** rather than a refactor with an empty-diff gate:

- the Windows console path (this harness runs on the maintainer's/CI's own
  OS, not the Windows terminal `#738`'s `kernel32` item touches);
- signal dispositions (a captured screen is a finished pane, not a process
  reacting to `SIGINT`/`SIGTERM`);
- daemon concurrency (`tasqx daemon` holds a socket open and `tasqx watch`
  streams until killed — neither is a pipe or a `tui` row with a `sleep` to
  end on; see "Excluded verbs" below);
- the real agent token files behind `tokens/copilot.rs` and `tokens/gemini.rs`
  (they parse a live tool's transcript format, which this store never
  produces).

## Excluded verbs and rows

Interactive/TUI verbs cannot be captured outside a real pty holding still:
bare `tasqx` (the dashboard), `tasqx dashboard`, `tasqx pick` and the memory
browser (`tasqx memory list` on a terminal) are captured as `tui` rows in the
site's own manifest already, and are not repeated here. `tasqx daemon` and
`tasqx watch` are excluded outright — both run until killed, with nothing to
end a capture on.

A few rows were captured once and then dropped, found by this corpus's own
determinism check (capture twice, diff):

- **`export`** — the demo store's one RUNNING task is left running by a live
  `tasqx start` inside `scripts/demo-store.py`, which mints a real event id at
  the pin instant. `export`'s full-store dump quotes that id's random tail,
  the same reason the site manifest excludes `task.add`.
- **`memory add`** — its echo quotes the fresh doc UUID it just minted, with
  no short-id form the way a task's `add-echo` has.
- **`config set` / `config unset` / `theme set`** — each writes
  `config.toml`, which (unlike `tasks.db`) this harness does not copy per row,
  so a mutating config row would make every row captured after it depend on
  the manifest's order.
- **`config path` / `config store`** — each prints an absolute
  `TASQX_CONFIG_DIR`/`TASQX_DB` path, this machine's, the same reason the site
  manifest drops `about`.

`setup --list` is captured with a fixed, nonexistent `--home` so its table
never reads a real machine's `~/.claude`. `completions bash` and
`completions powershell` stand in for the other three shells `clap_complete`
emits (elvish, fish, zsh): same code path, a different grammar, not worth
three more near-identical fixtures.

## Proving it

Two things are worth re-running by hand after a change to the harness itself,
not only to the manifest:

- **Determinism** — capture twice on the same commit; the two runs must be
  byte-identical. `--check` after a plain capture does exactly this.
- **It bites** — change one visible string in a verb's rendered output,
  rebuild, run `--check`, and confirm it fails naming that one fixture; then
  revert and confirm it is green again.
