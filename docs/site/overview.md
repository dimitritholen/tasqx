# What tasqx is

tasqx is the organiser for your AI: a backlog, a memory and a brief for your
coding agent, and a fast, terminal-first task manager for you. It is a headless
Rust core engine that exposes one stable, versioned JSON API — and every surface
you touch (the CLI, the MCP server, the HTML report, this guide) is a client of
that one contract.

## The five ideas that shape everything else

| Principle | What it means for you |
|---|---|
| Fast | One-shot commands open SQLite, do the work, and exit. No daemon required, no warmup. |
| Local-first | Your data is one SQLite file on your disk. No account, no cloud, works offline forever. |
| One API | The CLI does nothing you cannot do over JSON. Every verb below names the method it calls. |
| AI-native | The same typed API drives a bundled MCP server, so an agent is a first-class user. |
| Honest | Every command speaks human text and `--json`. Exit codes are contract, not decoration. |

## How the pieces fit

The core is a plain Rust library. The CLI links it and calls functions
in-process — no IPC on the hot path. The JSON API is a thin envelope over that
*same* dispatch layer, so "call a function" and "send a JSON command" run
identical code. There is exactly one dispatch table.

<!-- generated: architecture -->

## Every mutation is logged, in the same transaction

Adding, modifying, completing — each writes its row to an append-only `events`
table inside the *same* SQLite transaction as the change itself. The log cannot
drift from the data, because there is no window in which one exists without the
other. That is what makes [charts](Reports-and-Charts.md#tasqx-chart), [reminder dedupe](Dates-Reminders-and-Recurrence.md#it-fires-exactly-once),
and a future sync possible without a migration.

## Where your data lives

| What | Where |
|---|---|
| Store | `$TASQX_DB` if set, else the platform data dir: `%APPDATA%\tasqx\tasqx\data\tasks.db` on Windows (the doubled segment is what the `directories` crate produces from organization + application), `~/.local/share/tasqx/tasks.db` on Linux, `~/Library/Application Support/dev.tasqx.tasqx/tasks.db` on macOS |
| Config | `$TASQX_CONFIG_DIR/config.toml`, else the platform config dir |
| Themes | `$TASQX_CONFIG_DIR/themes/*.toml` |
| Socket | `$TASQX_SOCK`, else a platform default (see [Daemon](Dashboard-and-Live-View.md#socket-addresses)) |

> **Note** Point `$TASQX_DB` at a scratch file to try anything in this guide
> without touching your real store. Every example below was run exactly that
> way.

## Settings

`tasqx config list` shows every setting with the layer that supplied it.
Resolution order is the CLI flag, then the `TASQX_*` environment variable, then
`config.toml`, then the built-in default.

<!-- generated: settings -->

`tasqx config edit` opens the same settings on a full-screen editor: up and down
move, enter toggles a switch or opens a theme picker, escape leaves. Moving
through the theme list repaints the screen in that theme *before* anything is
written, which is the one thing editing `config.toml` by hand cannot do.
`default_project` is shown there but not editable — it lives in the store and is
set with `tasqx use`. Piped or redirected, `config edit` refuses and exits 2
instead of writing escape codes into your pipe; scripts should use `config set`.
A Sync section sits below the settings: `c` connects a `tasqx-remote-*`
connector found on `PATH` through a form (the connector's own fields, then the
sync passphrase, twice), `s` runs `tasqx sync`, and `d` disconnects after
confirming — the remote itself is untouched either way.
