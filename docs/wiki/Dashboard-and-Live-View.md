# Dashboard and Live View

Two ways to watch your work instead of querying it.

## tasqx dashboard

*Alias: `dash`*

A full-screen overview of everything: your working set, what's due, what's
blocked, recent activity, projects, a burndown and token spend — eight panels
over one snapshot, with a header that counts what matters
(`17 open · 1 active · 2 overdue · 3 blocked · 8 done/week`).

```console
tasqx                    # on a terminal, a bare tasqx opens the dashboard
tasqx dashboard          # the same screen, spelled out
tasqx --json dashboard   # all panels as one JSON document, no screen
```

- It's read-only, with one exception: `p` opens the
  [picker](Working-on-Tasks.md#tasqx-pick), and Enter there starts a task.
  `q`, Esc and Ctrl-C all close it.
- The BLOCKED panel exists because blocked tasks are hidden from the default
  task list — the dashboard is where work that's standing still stays visible.
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
