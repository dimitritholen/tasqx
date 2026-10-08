# Dashboard and Live View

Two ways to watch your work instead of querying it.

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

`watch` needs a running [daemon](AI-Agents-and-Automation.md#tasqx-daemon);
the daemon pushes a change notification on every write, and `watch`
re-renders. Any [filter](Finding-Tasks.md#the-filter-language) narrows what it
follows. Ctrl-C stops it.

Leave it open on a second monitor while an agent works through your backlog —
you see every task start, complete and unblock as it happens.

## tasqx board

The same live view in a browser: a kanban with a column per state, updating as
tasks change, where you move a task by dragging its card.

| Command | What it does |
|---|---|
| `tasqx board` | Print the board's URL and open it in your browser |
| `tasqx board --no-open` | Print the URL only |
| `tasqx board --port 8123` | Serve on a fixed port (or set `board.port`) |
| `tasqx board --scope read` | Serve the same board with every change refused |

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
searches, Esc closes, `s` starts the focused card and `d` completes it.

**Moving a task.** Drag a card to another column and the board sends the same
command you would type:

| Drop on | From | What runs |
|---|---|---|
| Active | any | `tasqx start` (the running task stops, as it always does) |
| Done | any | `tasqx done` |
| Ready | Active | `tasqx stop` |
| Ready | Done | `tasqx reopen` |
| Ready | Backlog | clears its wait and scheduled dates |
| Backlog | Ready | waits a week (change the date with `tasqx modify`) |
| Blocked | — | refused: a task is blocked by its dependencies, not by a drag |

While you drag, a tray appears under the card: drop on **H**, **M** or **L**
to set the priority, **clear** to remove it, or **cancel** to cancel the task.
Within a column cards stay ordered by urgency; there is no manual order to drag
into. Every drag has a button in the card's panel (click it, or Enter), which
is also how you move a task on a touch screen.

A message at the bottom says what happened in the command's own words — which
task a start stopped and after how long, which tasks a completion unblocked —
with **Undo** for a stop, completion, cancel or priority/date change. Undo is
`tasqx undo` with one extra condition: it only takes back your drag if nothing
has been written since, so it can never undo someone else's change. A start or
reopen is undone by dragging the card back.

If something else changed the task after the board drew it — you, in a
terminal, or an agent — the drop is refused, nothing is written, and the board
says "another session changed it" and shows the task as it now is. Every change
the board makes is recorded with `board` as its actor, so `tasqx api
event.list` tells a drag apart from a command or an agent.

`--scope read` serves the board for a wall screen or a standup: no drag, no
buttons, and the server refuses any change that is sent anyway.

**Who can reach it.** The board binds `127.0.0.1` and nothing else, so it is
never reachable from another machine. On a shared computer another user could
still connect to a loopback port, so the page is also behind a random secret
token: the URL `tasqx board` prints carries it once, the browser trades it for
a cookie, and every other request without that cookie is refused. Requests
from another website, or under a hostname other than `127.0.0.1` or
`localhost`, are refused too, which is what stops a web page you are browsing
from reading or changing your backlog through your own browser. The server
forwards only the handful of commands a drag needs, each carrying the revision
the card was drawn at. The board is single-user
and local. Put it behind a reverse proxy and authentication is the proxy's job.

Without a fixed port, each run takes a free port and a fresh token. With one
(`--port`, or `board.port` in the config) the token is kept in a file next to
`config.toml`, readable only by you, so a bookmark of the printed URL keeps
working after a restart.

The page is one self-contained file: nothing is fetched from the internet.
