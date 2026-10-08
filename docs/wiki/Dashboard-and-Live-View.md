# Dashboard and Live View

Two ways to watch your work instead of querying it, and the daemon that makes
the live one possible.

## Screens and pipes

Five commands open a screen instead of printing a table, and each answers a
pipe in its own way. `tasqx pick`, `tasqx dashboard` and `tasqx config edit`
refuse one outright rather than write escape codes into it.
`tasqx memory list` prints its one-line-per-doc table instead, and
`tasqx watch` prints each update as it arrives rather than repainting a screen
— it needs a running daemon either way. `tasqx board` is the same live view in
a browser.

| Command | What it opens |
|---|---|
| `tasqx pick` | browse tasks, search them, read one, start one |
| `tasqx dashboard` | the overview, and what a bare `tasqx` opens |
| `tasqx config edit` | settings, previewing a theme as you move over it |
| `tasqx memory list` | your docs, with the one under the cursor beside them |
| `tasqx watch` | a table repainted on every change (needs a daemon) |

`tasqx list` never opens a screen. It is the verb that always prints the
table, on a terminal and through a pipe alike.

## tasqx dashboard

*Alias: `dash`*

A full-screen overview of everything: your tasks, your projects, a burndown,
recent activity, effort and token spend — six panels (`tasks`, `projects`,
`burndown`, `pulse`, `effort`, `tokens`) over one snapshot, with a header that
counts what matters
(`17 open · 1 active · 2 overdue · 3 blocked · 8 done/week`).

| Command | What it does |
|---|---|
| `tasqx` | On a terminal, a bare `tasqx` opens the dashboard |
| `tasqx dashboard` | The same screen, spelled out |
| `tasqx --json dashboard` | The panel data as one JSON document, no screen |

- It's read-only, with one exception: `p` opens the task browser,
  [`pick`](Working-on-Tasks.md#tasqx-pick). Enter there reads a task, and `s`
  starts it and brings you back. `q`, Esc and Ctrl-C all close the dashboard.
- Blocked work stays visible here, which is the point: the default task list
  hides it. D80 folded the old NOW, NEXT UP, DUE, BLOCKED and RECENT panels
  into the one TASKS panel, so a blocked task keeps its row there (and its
  `"blocked": true` under `--json`), and the header counts them (`3 blocked`).
- `--json` is not the screen in text. The document carries a payload for four
  of the six panels — `tasks`, `projects`, `burndown` and `tokens` — beside the
  `status` header it always writes and a `panels` array naming what you asked
  for. `pulse` and `effort` are drawn from the screen's own model, so
  `tasqx --json dashboard --panels pulse,effort` is a valid request that
  answers with no panel payload at all.
- Layout adapts to your window; below 56×14 it won't open.
- Configure it under `[dashboard]` in the config: which panels, in what order,
  refresh mode and time window (`tasqx config list` shows the options).

**For scripts and agents:** the dashboard only opens when both stdin and
stdout are an interactive terminal. Piped, redirected, or with `--json`, a
bare `tasqx` prints the plain task table instead. But an agent or harness that
allocates a pty looks interactive, and a bare `tasqx` there opens a
full-screen program that waits for a keypress. So in anything automated, spell
the verb: `tasqx list` always means the table.

## tasqx watch

A task list that redraws itself the moment anything changes — from another
terminal, from an AI agent, from anywhere.

| Command | What it does |
|---|---|
| `tasqx daemon` | In one terminal: the server |
| `tasqx watch project:work` | In another: the live view |

`watch` subscribes to a [daemon](AI-Agents-and-Automation.md#tasqx-daemon) and
re-renders on every `task.changed` push; the daemon pushes a notification on
every write. It takes a [filter](Finding-Tasks.md#the-filter-language),
defaulting to the working set, and any filter narrows what it follows. Ctrl-C
stops it. It needs a running daemon and will never auto-spawn one — it hints
instead:

```console
$ tasqx watch --socket nope
tasqx watch: no daemon reachable at nope
hint: start one with `tasqx daemon` (add `--socket nope` to match)
```

On a terminal it clears the screen and repaints the table. Through a pipe it
streams one line per event instead — so it composes with everything else in your
shell:

```console
$ tasqx watch --socket tasqx-docsdemo | cat
$ # ... meanwhile, in another shell:
$ #   tasqx --socket tasqx-docsdemo add "Wire up the docs page ..."
$ #   tasqx --socket tasqx-docsdemo done 3
4 tasks · 1 overdue

  ID          URG  TASK                         PROJECT     DUE        TAGS
   1  H ▄▄▄▄ 15.7  Ship the v1 JSON API freeze  work.tasqx  Fri        +api +release
   3  - ▄▄▄▄ 12.0  Renew the TLS cert           work.tasqx  yesterday  +ops
   5  - ▄▄▃▁  8.9  Water the plants             home        Fri
   2  - ▄▄▂▁  7.1  Write the user guide         work.tasqx  Mon        +docs
task.changed op=add short_id=6
task.changed op=done short_id=3
```

Leave it open on a second monitor while an agent works through your backlog —
you see every task start, complete and unblock as it happens.

## tasqx board

The same live view in a browser: a kanban with a column per state, updating as
tasks change, for the moments a terminal is not where you want to look.

| Command | What it does |
|---|---|
| `tasqx board` | Print the board's URL and open it in your browser |
| `tasqx board --no-open` | Print the URL only |
| `tasqx board --port 8123` | Serve on a fixed port (or set `board.port`) |

Like `watch`, `board` needs a running
[daemon](AI-Agents-and-Automation.md#tasqx-daemon). Ctrl-C stops it.

The columns are the states tasqx already derives, each headed by the filter
that fills it: **Backlog** `status:backlog`, **Blocked** `@blocked`, **Ready**
`@working`, **Active** `status:active` (one at a time) and **Done**
`completed.after:-7d`. Inside a column cards are ordered by urgency, on the
same scale and in the same three bands as `tasqx list`; a `▶` marks the running
task and `⊘` a blocked one. A card opens (click, or Enter) into its checks,
blocked-by list and opening note. The search box filters by title, tag, project
and id, and the lanes menu groups a column by project or by priority. The page
follows your terminal theme, light or dark by your system setting, and shows
**daemon · offline** if the connection drops.

Keys: `j`/`k` move between cards, `h`/`l` between columns, Enter opens, `/`
searches, Esc closes.

The board only reads. It cannot change a task, and a request that tries is
refused by the server, not just hidden by the page.

**Who can reach it.** The board binds `127.0.0.1` and nothing else, so it is
never reachable from another machine. On a shared computer another user could
still connect to a loopback port, so the page is also behind a random secret
token: the URL `tasqx board` prints carries it once, the browser trades it for
a cookie, and every other request without that cookie is refused. Requests
from another website, or under a hostname other than `127.0.0.1` or
`localhost`, are refused too, which is what stops a web page you are browsing
from reading your backlog through your own browser. The board is single-user
and local. Put it behind a reverse proxy and authentication is the proxy's job.

Without a fixed port, each run takes a free port and a fresh token. With one
(`--port`, or `board.port` in the config) the token is kept in a file next to
`config.toml`, readable only by you, so a bookmark of the printed URL keeps
working after a restart.

The page is one self-contained file: nothing is fetched from the internet.

## The daemon

The daemon is optional. It holds one database connection, serves the JSON API
over a local socket to many concurrent clients, pushes change notifications, and
runs the reminder scheduler. The CLI never requires it. The agents page covers
the rest of it, including the OTLP receiver:
[`tasqx daemon`](AI-Agents-and-Automation.md#tasqx-daemon).

### One-shot or daemon?

| Mode | When | Why |
|---|---|---|
| One-shot | The default. Scripts, cron, the HTML report. | No process to manage. Open the DB, run one command, exit. |
| Daemon | Long-lived clients: a TUI, a GUI, `watch`, reminders. | One writer, warm caches, live push, and something to fire reminders. |

If a daemon is reachable, one-shot commands route through it automatically —
single writer, live-update semantics for free. If not, they open the store
in-process. **Same command surface either way**, and a missing or stale socket
falls back immediately rather than hanging. `--no-daemon` forces the in-process
path.

`$TASQX_DB` always wins over a daemon on another store. Naming the file the
daemon serves, commands route through it as usual. Naming a different file, a
daemon found on the default socket is passed over and the command runs
in-process against `$TASQX_DB`; a daemon named by `--socket` or `$TASQX_SOCK` is
refused with exit 2, naming both files (D204).

Four surfaces never route through a daemon: `api`, `mcp serve`, `chart` and
`report --html` open the store in-process, always. They refuse an explicit
`--socket` rather than ignore it (D73), because a flag that names a daemon and
is silently discarded aims your write at a different store than the one you
asked for.

### Socket addresses

Resolution order: `--socket`, then `$TASQX_SOCK`, then the platform default.

| Platform | Default |
|---|---|
| Windows | The named pipe `tasqx-default` |
| Linux | `$XDG_RUNTIME_DIR/tasqx/tasqx.sock` (falls back to the data dir) |
| macOS | `<data dir>/tasqx.sock` — macOS has no runtime dir |

### Running it

Diagnostics go to stderr; the socket carries the newline-delimited JSON API.
Ctrl-C stops it cleanly, unwinding the accept loop and removing the socket file
(a no-op for Windows named pipes). `--db` points it at a specific store.

```console
$ tasqx daemon --socket tasqx-docsdemo
tasqx daemon: listening on tasqx-docsdemo (Ctrl-C to stop)
tasqx daemon: store ~/.local/share/tasqx/tasks.db
```

It stays until you stop it. Set `[daemon] idle_timeout` to a number of minutes
and it will instead exit by itself once that long has passed with no client
connected, no subscriber attached, no reminder about to ripen and no telemetry
posted to its OTLP receiver — and it says so on stderr on the way out. `0`, the
default, means it never does. Turning the receiver on does not hold it open:
only exports that actually arrive count as work. An idle exit also leaves a note
beside `config.toml`: the first command that then fails to reach the daemon
reports the retirement and which store is no longer being served, because a
daemon leaving changes where the same command line writes (D74).

Now route a command through it — note this is the ordinary `add`, unchanged:

```console
$ tasqx --socket tasqx-docsdemo add "Wire up the docs page +docs project:work.tasqx due:tomorrow"
▌ #6  Wire up the docs page
▌ added   - ▄▄▄▃ 11.4   work.tasqx   due tomorrow   +docs
```
