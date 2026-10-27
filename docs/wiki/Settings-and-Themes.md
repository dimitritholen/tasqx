# Settings and Themes

Settings live in a TOML file, but you never have to hand-edit it — every read
and write goes through a command, and there's a full-screen editor with live
theme preview.

Default output should be something you want to look at. Themes drive the
terminal and the HTML report from the same palette, and degrade honestly when
the terminal cannot keep up.

## tasqx config

| Command | What it does |
|---|---|
| `tasqx config list` | Every setting: value, source, default |
| `tasqx config get theme.name` | One resolved value |
| `tasqx config set theme.name gruvbox` | Write it (your file comments survive) |
| `tasqx config unset theme.name` | Back to the default |
| `tasqx config path` | Where config.toml lives |
| `tasqx config store` | Which task store you'd be writing to |
| `tasqx config edit` | Interactive full-screen editor |

Worth knowing:

- **Resolution order** is: command-line flag, then `$TASQX_*` environment
  variable, then `config.toml`, then the built-in default. `config list`'s
  SOURCE column names the layer that won, so "why is this setting on?" has a
  lookup, not a hunt.
- **`config edit`** opens an interactive screen — arrow through themes and
  watch the whole screen repaint in each one before anything is written. It
  needs a real terminal; scripts use `set`/`unset`.
- **`config store`** answers which SQLite file a command would actually write
  to, and why: through a running daemon, or in-process — including the case
  where your `$TASQX_DB` names a different file than the daemon serves, so
  the daemon was passed over.

### Every setting

`tasqx config list` prints these with their live value and source.

| Setting | Default | What it does |
|---|---|---|
| `theme.name` | `nord` | The terminal theme ([below](#tasqx-theme)). Also `--theme` and `$TASQX_THEME` |
| `dashboard.enabled` | `true` | A bare `tasqx` on a terminal opens the dashboard; `false` prints the list instead. `$TASQX_DASHBOARD` overrides it |
| `dashboard.panels` | `tasks,projects,burndown,pulse,effort,tokens` | Which panels the dashboard draws, in tab order |
| `dashboard.refresh` | `auto` | `auto` re-reads on a timer, `manual` only on `r` |
| `dashboard.window` | `week` | The burndown window it opens on (`week`, `14d`, `30d`); `w` cycles it |
| `notify.enabled` | `false` | Lets the daemon raise OS notifications for reminders; needs a build with the `notify-os` feature ([Reminders](Dates-Reminders-and-Recurrence.md#who-delivers-them)) |
| `tokens.enabled` | `false` | Lets the daemon attribute AI token use from local tool transcripts |
| `otlp.enabled` | `false` | Runs a local OTLP/HTTP receiver in the daemon for token telemetry |
| `otlp.port` | `4318` | The port it binds on 127.0.0.1 |
| `board.port` | `0` | The port `tasqx board` binds; `0` takes a free one each run, a fixed one keeps the board's token so a bookmark survives |
| `daemon.idle_timeout` | `0` | Minutes of idleness after which `tasqx daemon` exits; `0` stays up |
| `default_project` | none | Where a bare `tasqx add` files; kept in the store, set with `tasqx use` |
| `mcp.add_status` | `pending` | Status of a task added over MCP when the call names none; `triage` holds agent proposals until `tasqx accept` |
| `detail.time_format` | `both` | How `show` and the MCP task view write timestamps: `iso`, `relative` or `both` |
| `work.agent` | unset | The agent `tasqx work` launches without `--agent`: `claude` or `codex`. Unset, it asks once and remembers |
| `work.codex_hook_hint` | `true` | Whether `tasqx work` tells you how to trust its codex hook the next time codex skips it; set to `false` once it has |

## tasqx theme

| Command | What it does |
|---|---|
| `tasqx theme list` | Built-ins + your own theme files |
| `tasqx theme show nord` | Preview a theme's colors and roles |
| `tasqx theme set nord` | Persist the choice |

Built-ins: `nord`, `gruvbox`, `dracula`, `solarized`, `mono` — plus any theme
file you drop in yourself. A one-off try is
`tasqx --theme dracula list`, and `$TASQX_THEME` sets it per environment.

### Themes

```console
$ tasqx theme list
  THEME
* nord
  gruvbox
  dracula
  solarized
  mono
```

The `*` marks the theme in effect. `tasqx theme show [name]` previews every
role, a sample drawn in it beside its colour and emphasis, plus the urgency
ramp's bands, rendered at your terminal's *real* capability. Set one
permanently:

```
# config.toml
[theme]
name = "gruvbox"
```

Drop a `.toml` in `$TASQX_CONFIG_DIR/themes/` and it appears in `theme list`
alongside the built-ins.

> **Note** Capability is detected, not assumed. Pipe tasqx into `cat` and the
> colour goes away; on a terminal without Unicode the block glyphs degrade to
> ASCII rather than emitting mojibake. `mono` is there for when you want that
> unconditionally.

> **Note** Width is detected too. `tasqx list` sizes its columns to what is
> actually in them and to the terminal it is printing into: a column no task
> fills — `DUE` on a store with no due dates — is not drawn at all, and the
> space goes to the titles. Through a pipe there is no width to detect, so the
> table lays out for a fixed 100 columns and two runs of the same store stay
> diffable.

Four environment variables override the detection:

| Variable | Effect |
|---|---|
| `NO_COLOR` | Set to anything: drop all colour, keep bold/underline. Wins over everything below. |
| `CLICOLOR_FORCE` | Set to anything but `0`: force colour even through a pipe — for `less -R` and CI logs. |
| `TASQX_FORCE_COLOR` | Set to anything: same as `CLICOLOR_FORCE`, scoped to tasqx. |
| `COLUMNS` | How many columns wide to lay tables out. Beats both the terminal's own answer and the piped default — for a multiplexer that misreports its size, or to pin the width of captured output. Clamped to 40–160. |
