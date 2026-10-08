# Shell Completion

Tab completion for bash, zsh, fish, elvish and PowerShell — the same five
shells on Linux, macOS and Windows. It completes more than most tools: verbs,
flags, value sets, file paths, *your actual task ids*, project and tag names,
the capture sugar (`+tag`, `project:x`, `!high`) and the whole filter grammar.

## tasqx completions

Completion isn't on after install unless Homebrew installed tasqx. Turn it
on by adding one line to your shell's startup file. `tasqx completions <shell>`
prints that line, so for bash this is the whole setup:

```console
tasqx completions bash >> ~/.bashrc
```

tasqx never edits the file itself. This is where each line belongs.

bash, in `~/.bashrc`:

```console
source <(TASQX_COMPLETE=bash tasqx)
```

zsh, in `~/.zshrc`, after your `compinit` line:

```console
source <(TASQX_COMPLETE=zsh tasqx)
```

fish, in `~/.config/fish/completions/tasqx.fish`:

```console
TASQX_COMPLETE=fish tasqx | source
```

elvish, in `~/.elvish/rc.elv`:

```console
eval (E:TASQX_COMPLETE=elvish tasqx | slurp)
```

PowerShell, in `$PROFILE`:

```console
$env:TASQX_COMPLETE = "powershell"; tasqx | Out-String | Invoke-Expression; Remove-Item Env:\TASQX_COMPLETE
```

## Platform notes

**zsh:** the line must come *after* `compinit` runs. Earlier, zsh prints
`command not found: compdef` and registers nothing. oh-my-zsh and prezto run
`compinit` for you; a hand-written `.zshrc` may not.

**Windows:** no Windows shell sets `$SHELL`, so name the shell, and let
PowerShell expand its own profile path:

```console
tasqx completions powershell >> $PROFILE
```

And PowerShell must be *allowed* to run your profile at all: a stock Windows
client ships with execution policy `Restricted`, which silently never runs it.
`Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` is the minimum that
does.

**cmd.exe** can't be completed by any program (that's cmd, not tasqx), and
**nushell** completes external commands through its own mechanism that tasqx
can't activate yet.

## How it behaves

- **Task ids come with their titles** in zsh, fish and PowerShell (bash and
  elvish can only show bare ids — their completion protocol has nowhere to
  put a title).
- **A Tab press reads your store** — through a running daemon if there is one,
  otherwise the SQLite file opened read-only — inside a 150 ms budget. If
  anything fails, you get no candidates rather than an error smeared across
  the line you're typing. Your database is never altered by a Tab press.
- The id menu shows *every* task by urgency, not just open ones — `reopen` and
  `why` need the closed ones.
- `TASQX_NO_COMPLETE_LOOKUP=1` turns the store lookups off; verbs, flags and
  value sets still complete.
- The activation variable is `TASQX_COMPLETE`, deliberately not the generic
  `COMPLETE` some tools use — don't export it by hand, it's how the shell
  callback is recognized.
