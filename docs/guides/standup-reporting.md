# Standups, reviews and reports

Everything below is a pure read of the same API the CLI uses — reports never mutate.

## The morning standup

What you finished, what is in flight, what is stuck:

```console
tasqx list "status:done completed.after:yesterday"    # yesterday's output
tasqx                                                 # today's working set
tasqx list "status:pending" --json | jq '[.tasks[] | select(.blocked)]'
```

## The weekly view

```console
tasqx report                     # per-project: count, estimates, overdue, tracked
tasqx report status              # the same, grouped by lifecycle state
tasqx chart throughput           # added vs done per ISO week
tasqx chart heatmap              # completion density, calendar-style
tasqx chart burndown             # remaining open tasks over the last N days
```

Charts render natively in the terminal from the event log — every add, done and
cancel was recorded transactionally, so the history is complete by construction,
not sampled.

## A report you can send someone

```console
tasqx report --html --out review.html
```

One self-contained file: inline CSS and SVG, no external requests, both light and
dark schemes. Open it in a browser, attach it to a mail, drop it in a channel.
Five built-in themes (`--theme nord`, `gruvbox`, `dracula`, `solarized`, `mono`).

## Time tracking honesty

`tasqx start`/`stop` accumulate tracked time per task, and reports show tracked
against estimate per project — which is how you find out your "4h" tasks are 9h
tasks before you promise the next deadline.
