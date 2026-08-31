# tasqx wiki

Every tasqx command, explained in plain language. Each page covers a group of
commands that belong together; every command has its own heading, with its
aliases next to it.

New here? Start with **[Getting Started](Getting-Started.md)** — install, first
task, and where to find help.

## The commands, by what you're trying to do

**Set up and organize**

| Command | What it does | Page |
|---|---|---|
| `init` | Create a project | [Projects](Projects.md#tasqx-init) |
| `use` | Choose your default project | [Projects](Projects.md#tasqx-use) |
| `projects` | List your projects | [Projects](Projects.md#tasqx-projects) |
| `archive` | Retire a project | [Projects](Projects.md#tasqx-archive) |

**Write things down**

| Command | What it does | Page |
|---|---|---|
| `add` | Capture a task | [Adding and Editing Tasks](Adding-and-Editing-Tasks.md#tasqx-add) |
| `modify` | Change a task | [Adding and Editing Tasks](Adding-and-Editing-Tasks.md#tasqx-modify) |
| `tag` / `untag` | Label a task, or unlabel it | [Adding and Editing Tasks](Adding-and-Editing-Tasks.md#tasqx-tag) |
| `annotate` | Attach a note to a task | [Adding and Editing Tasks](Adding-and-Editing-Tasks.md#tasqx-annotate) |

**Decide what to do**

| Command | What it does | Page |
|---|---|---|
| `list` | See tasks, filtered any way you like | [Finding Tasks](Finding-Tasks.md#tasqx-list) |
| `next` | The one task to do now | [Finding Tasks](Finding-Tasks.md#tasqx-next) |
| `agenda` | What's coming up, day by day | [Finding Tasks](Finding-Tasks.md#tasqx-agenda) |
| `show` | One task, in full detail | [Finding Tasks](Finding-Tasks.md#tasqx-show) |
| `why` | Why a task ranks where it does | [Finding Tasks](Finding-Tasks.md#tasqx-why) |

**Do the work**

| Command | What it does | Page |
|---|---|---|
| `start` / `stop` | Track time on a task | [Working on Tasks](Working-on-Tasks.md#tasqx-start) |
| `done` | Complete a task | [Working on Tasks](Working-on-Tasks.md#tasqx-done) |
| `cancel` | Cancel a task (reversibly) | [Working on Tasks](Working-on-Tasks.md#tasqx-cancel) |
| `reopen` | Bring a finished task back | [Working on Tasks](Working-on-Tasks.md#tasqx-reopen) |
| `pick` | Pick a task from a full-screen list and start it | [Working on Tasks](Working-on-Tasks.md#tasqx-pick) |
| `undo` | Take back the last change | [Working on Tasks](Working-on-Tasks.md#tasqx-undo) |
| `dep` / `undep` | Say "this waits on that" | [Dependencies](Dependencies.md) |

**See the bigger picture**

| Command | What it does | Page |
|---|---|---|
| `dashboard` | Full-screen overview of everything | [Dashboard and Live View](Dashboard-and-Live-View.md#tasqx-dashboard) |
| `watch` | A task list that updates itself | [Dashboard and Live View](Dashboard-and-Live-View.md#tasqx-watch) |
| `report` | Summary counts, in the terminal or as HTML | [Reports and Charts](Reports-and-Charts.md#tasqx-report) |
| `chart` | Throughput, heatmap and burndown charts | [Reports and Charts](Reports-and-Charts.md#tasqx-chart) |

**Remember and automate**

| Command | What it does | Page |
|---|---|---|
| `memory` | Store and search knowledge | [Memory](Memory.md) |
| `mcp` | The built-in server for AI agents | [AI Agents and Automation](AI-Agents-and-Automation.md#tasqx-mcp) |
| `api` | Call the JSON API directly | [AI Agents and Automation](AI-Agents-and-Automation.md#tasqx-api) |
| `daemon` | A long-running server for live updates | [AI Agents and Automation](AI-Agents-and-Automation.md#tasqx-daemon) |
| `tokens` | Repair AI token accounting | [AI Agents and Automation](AI-Agents-and-Automation.md#tasqx-tokens) |

**Housekeeping**

| Command | What it does | Page |
|---|---|---|
| `export` / `import` | Back up and restore as JSON | [Import and Export](Import-and-Export.md) |
| `config` | Read and change settings | [Settings and Themes](Settings-and-Themes.md#tasqx-config) |
| `theme` | Browse and pick a color theme | [Settings and Themes](Settings-and-Themes.md#tasqx-theme) |
| `completions` | Turn on Tab completion | [Shell Completion](Shell-Completion.md) |
| `manual` / `docs` | The built-in guides | [Getting Started](Getting-Started.md#getting-help) |

## Topic pages

- [Dates, Reminders and Recurrence](Dates-Reminders-and-Recurrence.md) — how
  `due:friday`, `repeat:"every monday"` and `remind:-1h` work.
- [Finding Tasks](Finding-Tasks.md#the-filter-language) — the filter language
  that every listing command understands.

## A few things that are true everywhere

- **Task references.** Wherever a command wants `<ref>`, give it the short id
  you see in every list (`tasqx done 42`). The full UUID works too.
- **`--json` everywhere.** Every command that produces a result accepts
  `--json` and prints the raw API answer instead of the human table. Great for
  scripts.
- **Exit codes mean something.** `0` ok, `2` bad request, `4` not found,
  `5` conflict. They don't change between releases.
- **Nothing is silently destroyed.** There is no hard delete. `cancel` is
  reversible with `reopen`, and every change lands in an append-only event log.
