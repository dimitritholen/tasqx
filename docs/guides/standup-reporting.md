# Standups, reviews and reports

Everything below is a pure read of the same API the CLI uses — reports never mutate.

## The morning standup

What you finished, what is in flight, what is stuck:

- `tasqx list "status:done completed.after:yesterday"`: yesterday's output
- `tasqx list`: today's working set

```console
tasqx list "status:done completed.after:yesterday"
tasqx list
tasqx list "status:pending" --json | jq '[.tasks[] | select(.blocked)]'
```

## The weekly view

| Command | What it does |
|---|---|
| `tasqx report` | Per-project: count, estimates, overdue, tracked, tokens |
| `tasqx report status` | The same, grouped by lifecycle state |
| `tasqx chart throughput` | Added vs done per ISO week |
| `tasqx chart heatmap` | Completion density, calendar-style |
| `tasqx chart burndown` | Remaining open tasks over the last N days |

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
The header's stat row ends with four token tiles — cache read, cache write,
input, output — one per bucket, never blended into a single total.

## Time tracking honesty

`tasqx start`/`stop` accumulate tracked time per task, and reports show tracked
against estimate per project — which is how you find out your "4h" tasks are 9h
tasks before you promise the next deadline.

## The retrospective the numbers can carry

`tasqx report` says what the work cost. `--outcomes` says how it went:

```console
tasqx report --outcomes project --since -30d
```

REWORK counts completions that were later reopened. SILENT counts completions
nobody annotated — the ones that taught the next session nothing. CALIB is the
median tracked-over-estimate ratio, which is the "4h tasks are 9h tasks"
sentence above as a number. DROPPED is work that was started and then
cancelled. FORCED counts completions that overrode an open blocker with
`--force`.

Every rate reads `count/n`, never a percentage, and the reason is worth a
sentence at a standup: 3/12 and 3/3 are the same percentage and very different
news. Check the denominator before you act on the number — a rework rate over
three completions is not evidence of anything.
