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

The page is a review of a period, the 7 days ending today unless you pick
another:

```console
tasqx report --html --since 2026-09-01 --until 2026-10-01 --out september.html
```

| Section | What it answers |
|---|---|
| The band | Done, added, net backlog change, blocked and overdue in the period, each against the period before it, with a sparkline per 7 days and a one-line summary of the biggest change |
| Standup | Done yesterday (tracked against estimate, unproven completions flagged), in progress and for how long, blocked and by which task, the next five and why each is next |
| Projects | Open, done in the period and its change, median tracked ÷ estimate, tokens per completion and overdue, per project. Click a row and every section shows that project alone |
| Net flow | Added and done per 7 days since the store started, the net change, and the open backlog under it |
| Outcomes | How tracked time compared with the estimate, and what was reopened, cancelled or completed unproven — the same counts `report --outcomes` prints |
| Find a task | A search over every task in scope; a task opens in an overlay with its dependencies by title |

Annotation bodies are not put in the file, because a report is meant to be
shared and that is where client detail lives. `--with-notes` embeds the
newest three per task behind a toggle. A filter that names one project
(`tasqx report project:acme --html`) makes a page about that project alone,
and a task it depends on elsewhere shows as "a task outside this report".

`--all` adds cancelled tasks to the search. `--metrics` picks terminal
columns and is refused with `--html`, and the page is always grouped by
project, so `tasqx report status --html` is refused too.

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

Charts drawn right in the terminal, from the event log. A bare `tasqx chart`
draws `throughput`.

| Command | What it does |
|---|---|
| `tasqx chart throughput` | Tasks added vs done, per week |
| `tasqx chart heatmap` | GitHub-style activity calendar |
| `tasqx chart burndown` | Open tasks over the last N days |

- **throughput** answers "am I finishing as fast as I'm adding?"
- **heatmap** answers "when do I actually get things done?"
- **burndown** answers "is the pile shrinking?" (`--days 30` widens the
  window)

Each takes the same filter DSL `list`/`report`/`agenda` do, so a chart can be
scoped to one slice of the store:

```console
tasqx chart burndown project:work
```

only counts `work`'s own tasks and events — the whole store otherwise.
`tasqx chart burndown --project work` is shorthand for the same thing;
`project:work` on the filter is the more general form, since it combines with
a second predicate (`tasqx chart burndown project:work +urgent`).

The charts degrade cleanly from truecolor terminals down to no color at all.

## tasqx why

Not a report, but the same spirit: `tasqx why <ref>` breaks a task's urgency
score into its components, so the ordering of every list stays explainable.
See [Finding Tasks](Finding-Tasks.md#tasqx-why).
