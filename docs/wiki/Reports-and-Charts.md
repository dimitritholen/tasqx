# Reports and Charts

What happened, what's left, and where the time and tokens went — in the
terminal, or as an HTML page you can send to someone.

## tasqx report

Summary counts, optionally grouped.

| Command | What it does |
|---|---|
| `tasqx report` | Totals |
| `tasqx report project` | Grouped by project |
| `tasqx report status` | Grouped by status (or `priority`) |
| `tasqx report +urgent` | Any filter narrows the scope |

Two output modes, same numbers:

One self-contained HTML file:

```console
tasqx report --html --out review.html
```

The HTML report is a single file with inline styling and no external
requests — it works from a mail attachment, a chat upload, or a USB stick.
A filter scopes both modes identically, so the page and the terminal table
always answer the same question.

Cancelled tasks are not counted unless you pass `--all` or your filter names a
status explicitly — a report about work shouldn't be padded by work you
decided not to do.

## tasqx report --outcomes

`tasqx report` says what the work cost. `--outcomes` says how it went.

```console
tasqx report --outcomes
```

```
PROJECT  CLOSED  DONE  REWORK  SILENT     CALIB  DROPPED  TOKENS
work          4     3     1/3     2/3  ×1.00 n2      1/4       -
```

| Column | What it counts |
|---|---|
| REWORK | Completions that were later reopened |
| SILENT | Completions carrying no annotation |
| CALIB | Median tracked-over-estimate, and how many completions had both |
| DROPPED | Work that was started and then cancelled |
| TOKENS | The largest token bucket, as everywhere else |

Every rate reads `count/n` rather than a percentage, deliberately: "3/12" and
"3/3" are the same percentage and very different news, and a rate over three
completions is not evidence of anything. Check the denominator before you act
on the number.

The scope is tasks that **closed**, by the date they closed — so a completion
that was later reopened still counts, which is the whole point of the REWORK
column. Open work is invisible here; plain `tasqx report` is the view for work
in flight. Group and filter as usual (`tasqx report --outcomes project`,
`tasqx report --outcomes +api`), and window with `--since`/`--until`.

## tasqx chart

Charts drawn right in the terminal, from the event log.

| Command | What it does |
|---|---|
| `tasqx chart throughput` | Tasks added vs done, per week |
| `tasqx chart heatmap` | GitHub-style activity calendar |
| `tasqx chart burndown` | Open tasks over the last N days |

- **throughput** answers "am I finishing as fast as I'm adding?"
- **heatmap** answers "when do I actually get things done?"
- **burndown** answers "is the pile shrinking?" (`--days 30` widens the
  window)

The charts degrade cleanly from truecolor terminals down to no color at all.

## tasqx why

Not a report, but the same spirit: `tasqx why <ref>` breaks a task's urgency
score into its components, so the ordering of every list stays explainable.
See [Finding Tasks](Finding-Tasks.md#tasqx-why).
