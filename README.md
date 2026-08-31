# tasqx

**Task management for your terminal — and for the AI agents working beside
you.**

One binary. One SQLite file on your own disk. No account, no cloud, no service
reading your backlog. Capture a task in one line, ask what to do next, and get
an answer you can interrogate:

```console
$ tasqx add Ship the release notes due:friday +docs !high --project work
╭─ #42 · Ship the release notes ─╮
│ ● pending   ! high   ▲ 11.4    │
│ work · #docs                   │
│ due 2026-09-04T00:00:00Z       │
╰────────────────────────────────╯

$ tasqx next
#42  (urgency 11.4)  Ship the release notes

$ tasqx why 42
Why #42 has urgency 11.4
  priority         6.00
  due_proximity    5.40
  age              0.00
  = total          11.4
```

And when you point an AI agent at the same backlog, it isn't scraping your
CLI: tasqx ships an MCP server, and the agent becomes a first-class user of
the same JSON API every other surface goes through.

## Why tasqx

- **It answers "what now?" and shows its work.** Every open task gets an
  urgency score from priority, deadline pressure and age; `tasqx next` hands
  you the top one, and `tasqx why` breaks the score into its components
  instead of asking you to trust it.
- **Your data is a file you own.** SQLite on your disk, offline by design.
  Every change lands in an append-only event log in the same transaction —
  which is why there is no destructive delete, why `cancel` is reversible,
  and why `tasqx undo` can tell you exactly what it took back.
- **AI agents are users, not an afterthought.** An agent reads the backlog,
  completes a task, learns what that unblocked, and stores what it figured
  out — searchable next session. Token accounting tells you what the agent
  work actually cost, per task.
- **Capture in one line.**
  `tasqx add Ship it due:friday +api !high est:4h` parses as it reads, dates
  take natural language (`tomorrow`, `in 3 days`, `eom`), and Tab completion
  knows your task ids, projects, tags and the whole filter grammar.

## Sixty seconds to a working setup

Install — with a package manager, which owns the update path from then on
(`brew upgrade tasqx` / `scoop update tasqx`) and, through brew, switches Tab
completion on without another step:

```console
brew install dimitritholen/tasqx/tasqx    # macOS and Linux
```

```console
scoop bucket add tasqx https://github.com/dimitritholen/scoop-tasqx
scoop install tasqx                       # Windows
```

No package manager? The scripts do the same job on a bare machine — Linux and
macOS:

```console
curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh
```

Windows (the first line lets older PowerShell negotiate TLS at all):

```console
[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1 | iex
```

Then:

```console
tasqx init work              # a project is just a name, no folder
tasqx add Buy milk           # lands in the default project
tasqx next                   # the one thing to do now
tasqx done 1                 # complete it
```

That's the whole loop. When you want depth: `tasqx manual` is a real manual in
your terminal, every verb answers `-h` with copy-pasteable examples, and
`tasqx docs` renders the full guide as one self-contained HTML page.

Prebuilt binaries for Linux, macOS and Windows are on the
[Releases page](https://github.com/dimitritholen/tasqx/releases). Building
from source needs Rust 1.95 or newer:

```console
git clone https://github.com/dimitritholen/tasqx.git
cd tasqx
cargo install --path crates/tasqx-cli --force
```

<details>
<summary>Install fine print — flags, checksums, what is and isn't promised</summary>

Both installer scripts pick the newest release, resolve your target triple,
verify the archive against its published checksum, and write nothing outside
the install directory (`~/.local/bin`, or
`%LOCALAPPDATA%\Programs\tasqx\bin` on Windows, where that one directory is
added to your user PATH and `-Uninstall` takes it back out). Neither touches a
shell startup file unless you ask.

A pipe passes no arguments, so flags need the longer form:

```console
curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh -s -- --dry-run
```

```console
&([scriptblock]::Create((irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1))) -DryRun
```

The rest are `--uninstall`/`-Uninstall`, `--completions`/`-Completions` and
`--help`/`-Help`; every switch also has an environment variable
(`TASQX_UNINSTALL`, `TASQX_DRY_RUN`, …), `TASQX_VERSION` pins a tag, and
`TASQX_INSTALL` moves the destination.

Honesty about what that buys you:

- **The checksum is integrity, not provenance.** It catches a truncated
  transfer or a corrupt CDN object; it is served from the same host as the
  archive, so it proves nothing about who built it. Nothing here is signed.
- **The binaries are unsigned.** On macOS the curl route goes *around*
  Gatekeeper rather than passing it — that is why the install just works.
- **The Linux build links your system glibc** (SQLite is bundled; there is no
  other runtime). The floor is whatever GitHub's current `ubuntu-latest`
  provides; re-derive it from a release binary with
  `objdump -T tasqx | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1`. There is
  no musl build — an older distro builds from source.

All of it applies to the package-manager routes too: the Homebrew formula and
the Scoop manifest are generated per release (`scripts/brew-formula.sh`,
`scripts/scoop-manifest.sh`) from the same published checksums, and point at
the same unsigned archives.

</details>

## The dashboard

On a terminal, a bare `tasqx` opens a full-screen overview instead of printing
a table: your working set, deadlines, blocked work, recent activity, projects,
a burndown and token spend, under a header that counts what matters
(`17 open · 1 active · 2 overdue · 3 blocked · 8 done/week`) and a footer that
names every key. The BLOCKED panel earns its place: the default list filter
hides blocked tasks, so the dashboard is where work that is standing still
stays visible. Press `p` to pick a task and start it; `q` closes.

Anything that is *not* a person at a keyboard gets the plain table instead —
the dashboard opens only when stdin and stdout are both interactive terminals,
and `tasqx --json dashboard` returns all panels as one JSON document. One
caveat for automation: a script or agent that allocates a pty looks
interactive, and a bare `tasqx` there opens a screen that waits for a key. So
in anything automated, spell the verb — `tasqx list` always means the table.

## Give your agent a backlog and a memory

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

Twenty tools, one verb each. Six reads: `list_tasks`, `get_task`, `summary`,
`list_projects`, `search_memory`, `get_memory`. Fourteen writes: `add_task`,
`modify_task`, `complete_task`, `reopen_task`, `start_timer`, `stop_timer`,
`tag_task`, `untag_task`, `annotate_task`, `add_dependency`,
`remove_dependency`, `add_memory`, `remove_memory`, `create_project`
(all prefixed `tasqx_`).

What makes this more than remote CRUD:

- **Completing a task returns what it unblocked**, so an agent can decompose a
  feature into a dependency chain with `add_dependency` and then walk it,
  picking up each task the moment its prerequisites clear.
- **Agents get long-term memory.** `search_memory` gives even a read-only
  agent bm25-ranked retrieval over your imported docs *and* every task
  annotation — feed it your ADRs with `tasqx memory import docs/`, and past
  decisions surface while it works. Annotations feed the same index, so an
  agent that documents its work is building the knowledge base as a side
  effect.
- **Token spend lands on the task.** `complete_task` takes token counts plus
  who spent them; a log-parse fallback fills gaps and refuses contested
  samples rather than guess. The table, the dashboard and the HTML report all
  show what each task cost.
- **Guardrails are structural.** A read-only session never sees the write
  tools in its tool list, there is no bulk-delete tool, and cancelling goes
  through the same reversible, logged path as everything else — an agent
  cannot quietly destroy a week of work. The one permanent delete
  (`remove_memory`, for retracting a wrongly stored document) says so in its
  own description.

The server tells an agent what it can call; the skill in
[`.claude/skills/tasqx-workflow/`](.claude/skills/tasqx-workflow/SKILL.md)
tells it how to *work* — Claude Code picks it up automatically inside this
repo, and the paste-anywhere block in
[Giving an agent memory in any client](docs/guides/agent-starter-prompt.md)
does the same for any other client. Scripts can skip MCP entirely and talk to
the API directly:

```console
echo '{"tasqx":"1","method":"task.list","params":{"filter":"@working"}}' | tasqx api
```

## The details that add up

- **Real scheduling, plain words.** Recurrence (`every 3 days`,
  `weekly on mon,wed`, `monthly on the 2nd tuesday`), reminders anchored to
  the due date so they move when it moves, and `wait:`/`scheduled:` dates
  that keep future work out of today's view until it's actionable.
- **A filter language, not a flag zoo.** `project:work and (+api or +ui)` —
  with `and`, `or`, parentheses and `-tag` exclusions — works on every
  listing command, `report` and `export` included.
- **Reports you can send.** Grouped summaries in the terminal, throughput /
  heatmap / burndown charts drawn from the event log, or a self-contained
  themed HTML page with zero external requests. Five built-in themes, and
  output degrades cleanly down to a colorless terminal.
- **Built for scripts too.** Every command with a result takes `--json`;
  exit codes mean something (`0` ok, `2` bad request, `4` not found,
  `5` conflict) and don't change. A live daemon (`tasqx daemon`) gives many
  concurrent clients one writer and pushes changes to `tasqx watch`.
- **Tab completion that knows your data.** bash, zsh, fish, elvish and
  PowerShell: verbs, flags, file paths, your task ids (with titles where the
  shell allows), projects, tags, the capture sugar and the filter grammar.
  `tasqx completions --install` sets it up and asks before touching anything;
  or add the line yourself:

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

Platform notes (zsh ordering, Windows profiles, execution policy) are in the
[Shell Completion](docs/wiki/Shell-Completion.md) page.

## Learn more

The **[wiki](docs/wiki/Home.md)** explains every command in plain language.
The worked guides each take five minutes and end with commands you can paste:

- [Feature development](docs/guides/feature-development.md) — a backlog per
  feature, ordered by dependencies. The solo alternative to a board.
- [Driving tasqx from an AI agent](docs/guides/ai-agent-workflow.md) — wire up
  MCP and let an agent work the backlog end to end.
- [Giving an agent memory in any client](docs/guides/agent-starter-prompt.md)
  — a paste-anywhere block for clients without a tasqx skill.
- [Personal task management](docs/guides/personal-gtd.md) — frictionless
  capture and a five-minute weekly review.
- [Standups and reports](docs/guides/standup-reporting.md) — yesterday's
  output, terminal charts, an HTML review you can send.
- [Token accounting](docs/guides/token-accounting.md) — measuring what agent
  work costs, and how attribution decides who pays.

## Under the hood

There is one JSON API; the CLI, the MCP server and the HTML report are all
clients of the same dispatch, so where surfaces overlap they behave
identically. CI runs the suite on Linux, Windows and macOS on every push, and
a good chunk of it is drift guards: tests that break the build when docs and
code disagree — every flag must appear in its usage line, every in-binary doc
example must parse, and the safe ones are executed for real. `cargo mutants`
breaks the code on purpose to check the tests notice; it once caught a
one-line deletion that made `(a or b) and c` silently parse as
`a or (b and c)` — a bug that would have returned a perfectly normal-looking
table of exactly the wrong rows. `DESIGN.md` is the spec and carries the
decision log explaining why things are the way they are.

## License

[FSL-1.1-MIT](LICENSE.md) — use tasqx for anything except selling a competing
product, and every release automatically becomes plain MIT two years after it
ships. Using it inside your company, scripting it, building on its API: all
fine.

## Honest edges

`tasqx undo` is narrow on purpose: it reverses the newest event only, over
four operations, and refuses everything else by name along with the verb that
does take it back. `tasqx agenda` is a day-grouped list, not the week grid the
spec sketched. There is no `unarchive` — importing a saved export is the way
back. The rest of the TUI beyond `tasqx config edit`, plugins and sync are
specified in `DESIGN.md` and don't exist yet; they were designed together so
adding them later doesn't touch the data model, but "designed" is doing a lot
of work in that sentence.
