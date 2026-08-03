# tasqx

A task manager that lives in the terminal. It does what Taskwarrior does, with less friction, and it treats an AI agent as a normal user instead of an add-on.

```console
$ tasqx add Ship the release notes due:friday +docs !high --project work
Added #42  ·  pending  ·  urgency 11.4  ·  work
  Ship the release notes

$ tasqx next
#42  (urgency 11.4)  Ship the release notes

$ tasqx why 42
Why #42 has urgency 11.4
  priority         6.00
  due_proximity    5.40
  age              0.00
  = total          11.4
```

## The idea

There is one JSON API, and everything else is a client of it. The CLI, the HTML report, the MCP server an agent talks to. The two surfaces go through the same dispatch, so where they overlap they behave identically — but they are not mirrors. The CLI keeps the human and maintenance verbs (`next`, `why`, charts, `cancel`/`reopen`, export/import, `tokens recompute`, the HTML report); the MCP server exposes a deliberately small tool set, and is currently the only surface with a standalone tag verb.

Your tasks are a SQLite file on your disk. It works offline and there's nothing to sign up for. Every change is written to an append-only event log in the same transaction as the change itself, which is why cancelling a task is reversible and why sync can be added later without a migration.

## Install

Tagged releases get prebuilt binaries for Linux, macOS and Windows on the [Releases page](https://github.com/dimitritholen/tasqx/releases) — download, unpack, put `tasqx` on your PATH. There is no runtime to install and no dynamic linking; SQLite is bundled.

Or build from source. Needs Rust 1.95 or newer — a measured floor, not a guess — which `Cargo.toml` enforces:

```console
git clone https://github.com/dimitritholen/tasqx.git
cd tasqx
cargo install --path crates/tasqx-cli --force
```

CI runs the suite on Linux, Windows and macOS on every push — the same three platforms the release builds for.

## Getting started

```console
tasqx init work              # a project is just a name, no folder
tasqx use work               # make it the default
tasqx add Buy milk           # lands in the default project
tasqx                        # bare `tasqx` lists your working set
tasqx done 1
```

`tasqx manual` is a real manual, not a wall of flags. `tasqx <verb> -h` gives you per-command help with examples you can copy. `tasqx docs` renders the same content as a single HTML file you can open in a browser.

## Tab completion

Verbs and flags, closed value sets, file paths, your task ids, project and tag names, the capture sugar (`+tag`, `project:x`, `!high`) and the whole filter grammar including `-tag` exclusions. bash, zsh, fish, elvish and PowerShell, the same five on Linux, macOS and Windows.

Task ids come with their titles in zsh, fish and PowerShell. bash and elvish show bare ids: their registrations write candidate values only, so there is nowhere for the title to go. That is upstream's protocol rather than a tasqx setting, and it is why `tasqx done 4<TAB>` is more useful in some of these shells than others.

Two details that aren't what you'd guess. Aliases come along, but a canonical name wins the prefix: `tasqx ls<TAB>` gives `ls` and `tasqx mod<TAB>` gives `modify` rather than `mod`. And the id menu is every task sorted by urgency, not just the open ones — `reopen` and `why` want the closed ones, and a menu that hid them would look like an answer.

`tasqx completions <shell>` prints one line. Put it in your startup file:

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

The zsh ordering is not a nicety. The registration ends in `compdef`, which only exists once `compinit` has run — source it earlier and zsh prints `command not found: compdef`, registers nothing, and carries on. Most setups (oh-my-zsh, prezto) run `compinit` for you; a hand-written `.zshrc` may not.

Or let tasqx edit the file. `tasqx completions --install` finds the shell from `$SHELL`, shows the exact block, and asks first — a stdin that isn't a terminal is a refusal rather than an implied yes, so pass `--yes` from a script. Running it twice leaves one block; `tasqx completions <shell> --uninstall` takes it back out and restores the file byte for byte.

No Windows shell sets `$SHELL`, so name the shell there. PowerShell's `--install` also refuses to guess where your profile is, because `$PROFILE` is a PowerShell variable rather than an environment one and it differs between Windows PowerShell 5.1, PowerShell 7 and the ISE. Let the shell expand it:

```console
tasqx completions powershell --install --profile $PROFILE
```

PowerShell also has to be allowed to run `$PROFILE` at all. A stock Windows client ships with the execution policy set to `Restricted`, and then the profile is never executed — so the line is in the right file, nothing errors, and completion simply never turns on. `Get-ExecutionPolicy` tells you; `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` is the minimum that runs it.

cmd.exe is a permanent non-goal, not a gap: no program can register a completer with it at all. nushell is a real gap — it completes external commands through its own `extern` definitions and there's nothing tasqx can print today that turns that on. Asking for either says so instead of "unknown shell".

Two things worth knowing before you switch it on.

**The variable is `TASQX_COMPLETE`, not clap's generic `COMPLETE`**, and that is deliberate rather than a naming preference. The completion protocol gives a program no way to tell a callback from a real command: with a recognised shell name in that variable, `tasqx add -- "a real task"` writes nothing, exits 0, and doesn't add the task. A name nothing else has a reason to set makes that state improbable — it does not make it impossible, so don't export it by hand. `COMPLETE` on its own does nothing to tasqx.

**A Tab press reads your store.** It prefers a running daemon, otherwise opens the SQLite file read-only, gives the whole lookup 150 ms, and answers with no candidates rather than an error when any of that fails — a message on stderr would land in the middle of the line you're typing. That read leaves `tasks.db-shm` and `tasks.db-wal` beside your store, which is SQLite rather than tasqx: an ordinary `tasqx list` creates the same two files and removes them on the way out, and a read-only connection can't, because deleting them is a write. Your database is not altered — no migration, not a byte. `TASQX_NO_COMPLETE_LOOKUP=1` turns the value lookups off and leaves verbs, flags and value sets completing.

## Use cases

Five worked scenarios, each a five-minute read with copy-pasteable commands:

- [Feature development](docs/guides/feature-development.md) — a backlog per feature, tasks ordered by dependencies, acceptance criteria in annotations. The solo alternative to a board.
- [Driving tasqx from an AI agent](docs/guides/ai-agent-workflow.md) — wire up the MCP server and let an agent work the backlog: read context, do the task, complete it, pick up what that unblocked.
- [Personal task management](docs/guides/personal-gtd.md) — frictionless capture, a working set that hides what you can't act on yet, and a five-minute weekly review.
- [Standups and reports](docs/guides/standup-reporting.md) — yesterday's output, terminal charts from the event log, and a self-contained HTML review you can send someone.
- [Token accounting](docs/guides/token-accounting.md) — measuring what agent work costs, and how attribution decides who pays.

## What works

Capture is where most of the ergonomics went. You can write `tasqx add Ship it due:friday +api !high est:4h repeat:"every monday" remind:-1h` and it parses the whole thing, or use flags for any of it if you prefer.

Dates take natural language: `tomorrow`, `friday 17:00`, `in 3 days`, `eom`, `at 6pm`, `-1d`, and full RFC3339 when you want to be exact. A bare time rolls to tomorrow if it's already passed.

Beyond that: start/stop with time tracking, dependencies that mark tasks blocked and announce them when they unblock, recurrence (`every 3 days`, `weekly on Mon,Wed`, `monthly on the 2nd tuesday`), reminders anchored to the due date so they move when it moves, and a filter language with `and`, `or` and parentheses.

Token accounting tracks what agent work costs: agents self-report counts when completing a task, log-parse attribution is the fallback that refuses contested samples rather than guess, and `tasqx tokens recompute` replays history under the current rules — a dry run unless you pass `--apply`.

Reports come as grouped summaries in the terminal, three chart types (throughput, heatmap, burndown), or a self-contained themed HTML page. The terminal table carries a TOKENS column, and the HTML report's header tiles end with the four token buckets — never a blended total. Five built-in themes, and the output degrades cleanly from truecolor down to a terminal that can't do color at all.

Settings live in a TOML file, but you don't hand-edit it: `tasqx config set theme.name nord` works, `tasqx config list` shows every setting with its value, source and default, and `tasqx config edit` opens an interactive screen that previews themes live.

Every command prints readable text, and every command with a result to hand over takes `--json`. The declared exceptions refuse the flag loudly rather than ignore it: `api`, `mcp` and `daemon` already speak a machine protocol, `watch` never finishes, `manual` is prose for a human. Exit codes mean something and don't change.

## For agents

```console
tasqx mcp serve                  # read-only by default
tasqx mcp serve --scope write    # explicit write access
```

Wiring it into Claude Code is one line:

```console
claude mcp add tasqx -- tasqx mcp serve --scope write
```

Any other MCP client takes the same shape:

```json
{
  "mcpServers": {
    "tasqx": { "command": "tasqx", "args": ["mcp", "serve", "--scope", "write"] }
  }
}
```

Fifteen tools, one verb each. Five reads: `list_tasks`, `get_task`, `summary`, `list_projects`, `search_memory`. Ten writes: `add_task`, `modify_task`, `complete_task`, `start_timer`, `stop_timer`, `tag_task`, `annotate_task`, `add_dependency`, `add_memory`, `create_project` (all prefixed `tasqx_`). The interesting ones for agent work: `complete_task` returns which tasks its completion unblocked, `annotate_task` stores long-form markdown context verbatim, `add_dependency` lets an agent decompose a feature into an ordered chain, and `search_memory` gives even a read-only agent bm25-ranked retrieval over imported docs and task annotations — company patterns and past decisions surface while the agent works (`tasqx memory import docs/` to feed it).

`complete_task` also takes optional token counts (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`) plus `tool` and `model` to say who spent them. Pass them: the agent is the only party that knows which task a turn's spend served, so self-report is the primary measurement channel, and completing without counts gets a `tokens_hint` in the response saying so. Log-parse attribution exists as a fallback, but it refuses any sample claimed by more than one task's window rather than guess an owner.

A read-only session never sees the write tools in its tool list, so an agent can't call what it isn't allowed to call. Scope configures this local stdio child process; it is not an authentication credential. There's no bulk-delete tool on purpose. Cancelling goes through the same reversible, logged path everything else does, so an agent can't quietly destroy a week of work.

The MCP server tells an agent what it can call. The skill in [`.claude/skills/tasqx-workflow/`](.claude/skills/tasqx-workflow/SKILL.md) tells it how to work: what deserves a backlog entry, the search-memory-first work loop, and why an annotation goes on before `complete_task` — annotations feed the same search index as imported docs, so an agent that completes tasks well is building the knowledge base as a side effect. Claude Code picks the skill up automatically when working inside this repo; for your own projects, copy the folder into `~/.claude/skills/`, or paste `SKILL.md` into whatever instructions file your client reads.

You can also talk to the API directly:

```console
echo '{"tasqx":"1","method":"task.list","params":{"filter":"@working"}}' | tasqx api
```

## Development

```console
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo mutants                    # see docs/mutation-testing.md
```

CI runs the suite on Linux, Windows and macOS, executes the zsh and fish activation lines in those real shells rather than comparing their text, builds the `notify-os` feature that's off by default, and gates on clippy and rustfmt. A `cargo deny` job gates dependency advisories, licenses and sources (`docs/dependency-policy.md` has the policy), and a coverage job publishes a line-and-branch report on every push — report only, no threshold. Rustc warnings are fatal, which sounds strict until you've had a `#[test]` get separated from its function by a careless edit. The test stops running, rustc says "function is never used" in every build after that, and nobody reads it.

A good chunk of the suite is drift guards: tests that break the build when the docs and the code disagree. Every CLI flag has to show up in its verb's usage line. Every example in the docs has to parse, and the ones marked safe get executed for real. Status sets in SQL, in the filter language and in Rust all come from one enum, so you can't add a status and forget one of them.

`cargo mutants` is the interesting one. It breaks the code on purpose and checks whether any test notices. It found a bug where deleting one line made `(a or b) and c` silently parse as `a or (b and c)`, which would have returned a perfectly normal-looking table full of the rows you filtered out. 316 passing tests hadn't caught it.

`DESIGN.md` is the spec and carries the decision log, D1 through D50, explaining why things are the way they are.

## License

[FSL-1.1-MIT](LICENSE.md) — use tasqx for anything except selling a competing product, and every release automatically becomes plain MIT two years after it ships. Using tasqx inside your company, scripting it, building on its API: all fine.

## What's missing

`tasqx pick` is specified and not built, and there's no `undo` — `cancel` and `reopen` are the reversible paths for now. `tasqx agenda` shipped as a day-grouped list, not the week grid `DESIGN.md` sketched. There's no `unarchive`: `tasqx archive` retires a project one way, and importing a saved export is the way back.

The rest of the TUI (the settings screen behind `tasqx config edit` is the only piece built), plugins and sync are all specified in `DESIGN.md` and don't exist. They were designed together so that adding them later doesn't touch the data model, but "designed" is doing a lot of work in that sentence.
