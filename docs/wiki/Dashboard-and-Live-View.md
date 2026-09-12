# Dashboard and Live View

Two ways to watch your work instead of querying it.

## tasqx dashboard

*Alias: `dash`*

A full-screen overview of everything: your tasks, your projects, a burndown,
recent activity, effort and token spend — six panels (`tasks`, `projects`,
`burndown`, `pulse`, `effort`, `tokens`) over one snapshot, with a header that
counts what matters
(`17 open · 1 active · 2 overdue · 3 blocked · 8 done/week`).

```console
tasqx                    # on a terminal, a bare tasqx opens the dashboard
tasqx dashboard          # the same screen, spelled out
tasqx --json dashboard   # the panel data as one JSON document, no screen
```

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

```console
tasqx daemon              # in one terminal: the server
tasqx watch project:work  # in another: the live view
```

`watch` needs a running [daemon](AI-Agents-and-Automation.md#tasqx-daemon);
the daemon pushes a change notification on every write, and `watch`
re-renders. Any [filter](Finding-Tasks.md#the-filter-language) narrows what it
follows. Ctrl-C stops it.

Leave it open on a second monitor while an agent works through your backlog —
you see every task start, complete and unblock as it happens.
