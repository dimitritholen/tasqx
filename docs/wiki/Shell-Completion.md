# Shell Completion

Tab completion for bash, zsh, fish, elvish and PowerShell — the same five
shells on Linux, macOS and Windows. It completes more than most tools: verbs,
flags, value sets, file paths, *your actual task ids*, project and tag names,
the capture sugar (`+tag`, `project:x`, `!high`) and the whole filter grammar.

## tasqx completions

Completion isn't on after install (no install route can turn it on for you —
only a package manager could). One command fixes that:

```console
tasqx completions --install
```

This detects your shell from `$SHELL`, shows you the exact block it would add
to your startup file, and asks before writing. Run twice it leaves one block;
`--uninstall` restores the file byte for byte.

Prefer doing it by hand? `tasqx completions <shell>` prints the one line, and
this is where each line belongs:

```console
# bash — ~/.bashrc
source <(TASQX_COMPLETE=bash tasqx)

# zsh — ~/.zshrc, after your compinit line
source <(TASQX_COMPLETE=zsh tasqx)

# fish — ~/.config/fish/completions/tasqx.fish
TASQX_COMPLETE=fish tasqx | source

# elvish — ~/.elvish/rc.elv
eval (E:TASQX_COMPLETE=elvish tasqx | slurp)

# PowerShell — $PROFILE
$env:TASQX_COMPLETE = "powershell"; tasqx | Out-String | Invoke-Expression; Remove-Item Env:\TASQX_COMPLETE
```

## Platform notes

**zsh:** the line must come *after* `compinit` runs. Earlier, zsh prints
`command not found: compdef` and registers nothing. oh-my-zsh and prezto run
`compinit` for you; a hand-written `.zshrc` may not.

**Windows:** no Windows shell sets `$SHELL`, so name the shell. For
PowerShell, also pass the profile path and let the shell expand it —
`$PROFILE` is a PowerShell variable tasqx refuses to guess:

```console
tasqx completions powershell --install --profile $PROFILE
```

And PowerShell must be *allowed* to run your profile at all: a stock Windows
client ships with execution policy `Restricted`, which silently never runs it.
`Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` is the minimum that
does.

**cmd.exe** can't be completed by any program (that's cmd, not tasqx).
**nushell** completes external commands through its own mechanism that tasqx
can't activate yet. Asking for either gets a straight answer instead of
"unknown shell".

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
