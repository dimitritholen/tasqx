# Settings and Themes

Settings live in a TOML file, but you never have to hand-edit it — every read
and write goes through a command, and there's a full-screen editor with live
theme preview.

## tasqx config

```console
tasqx config list                     # every setting: value, source, default
tasqx config get theme.name           # one resolved value
tasqx config set theme.name gruvbox   # write it (your file comments survive)
tasqx config unset theme.name         # back to the default
tasqx config path                     # where config.toml lives
tasqx config store                    # which task store you'd be writing to
tasqx config edit                     # interactive full-screen editor
```

Worth knowing:

- **Resolution order** is: command-line flag, then `$TASQX_*` environment
  variable, then `config.toml`, then the built-in default. `config list`'s
  SOURCE column names the layer that won, so "why is this setting on?" has a
  lookup, not a hunt.
- **`config edit`** opens an interactive screen — arrow through themes and
  watch the whole screen repaint in each one before anything is written. It
  needs a real terminal; scripts use `set`/`unset`.
- **`config store`** answers which SQLite file a command would actually write
  to — including the case where a running daemon owns the store and your
  `$TASQX_DB` is being ignored because of it.

## tasqx theme

```console
tasqx theme list          # built-ins + your own theme files
tasqx theme show nord     # preview a theme's colors and roles
tasqx theme set nord      # persist the choice
```

Built-ins: `nord`, `gruvbox`, `dracula`, `solarized`, `mono` — plus any theme
file you drop in yourself. A one-off try is
`tasqx --theme dracula list`, and `$TASQX_THEME` sets it per environment.

Output degrades cleanly from truecolor terminals down to terminals with no
color at all, so themes are a nicety, never a requirement.
