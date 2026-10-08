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

The TOKENS column names a group's largest bucket with that bucket's own count
(`cacheR 1.2M`), or `-` when nothing was spent. The four buckets — in, out,
cacheR (cache read) and cacheW (cache creation) — are never blended into one
figure; `--json` and `report --html` carry the full split. They are filled by
self-reported counts on `task.done` (the primary source, via `token.add` — see
[`tasqx tokens`](AI-Agents-and-Automation.md#tasqx-tokens)), falling back to
parsing local AI-tool transcripts when `tokens.enabled = true`, or by the
daemon's OTLP receiver when `otlp.enabled = true`
([`tasqx daemon`](AI-Agents-and-Automation.md#tasqx-daemon)).

### Grouping and what counts

`tasqx report [group_by] [filter] [--all]` — group by `project` (default),
`status`, or `priority`. Estimates total as ISO-8601 durations, which is why
[`est:`](Dates-Reminders-and-Recurrence.md#estimates) is parsed at the edge rather
than stored as opaque text.

**What counts.** A report is an aggregation, so it leaves *cancelled* tasks out
— tasqx has no hard delete, and without this every task you ever threw away
would inflate your totals forever. *Done* tasks still count: completed work is
real work, and it carries nearly all your tracked time. Two ways to override
that: pass `--all` to count everything including cancelled, or name a status in
the filter — `tasqx report status:cancelled` means what it says and is taken
literally.

```console
$ tasqx report
PROJECT               COUNT         EST  OVERDUE     TRACKED        TOKENS
home                      1        PT0S        1        PT0S             -
work.tasqx                3     PT5H30M        1        PT0S   cacheR 1.2M
```

```console
$ tasqx report status
STATUS                COUNT         EST  OVERDUE     TRACKED        TOKENS
pending                   4     PT5H30M        2        PT0S   cacheR 1.2M
```

TOKENS names the group's largest bucket with that bucket's own count — `cacheR
1.2M`, or `-` when nothing was spent. The four buckets are never blended into
one figure; `--json` and the HTML report carry the full split (D48/D50).

### The HTML report

`tasqx report --html` emits a weekly review as one self-contained file — inline
CSS, inline SVG charts, no external requests — themed from the same palette as
your terminal. Exactly like the `tasqx docs` guide.

```console
$ tasqx report --html --out review.html
Wrote self-contained HTML report → review.html
```

Without `--out` it writes to stdout. The page leads with what changed in the
period — done, added, net backlog, blocked and overdue, each against the period
before — then a standup (done yesterday, in progress, blocked and by what, the
next five and why), a per-project grid, a net-flow chart and the outcomes
`report --outcomes` counts. The period is the 7 days ending today; `--since` and
`--until` choose another. Clicking a project scopes every section to it, a
search covers every task in scope, and a task opens in an overlay with its
dependencies by title. Annotation bodies stay out of the file unless you pass
`--with-notes`; a filter naming one project makes a page about that project
alone. One small inline script does the clicking; the page reads fully without
it.

> **Note** Both HTML surfaces — this report and the `tasqx docs` guide — hold
> the same line: no CDN, no web fonts, no
> remote images, no scripts fetched from anywhere. Mail the file, commit it,
> open it on a plane — it renders the same.

## tasqx report --outcomes

`tasqx report` says what the work cost. `--outcomes` says how it went.

<!-- fixture: report-outcomes -->
```console
$ tasqx report --outcomes
PROJECT  CLOSED  DONE  REWORK  FORCED  SILENT     CALIB  DROPPED  OVER  UNPROVEN  UNBRIEFED               TOKENS
api           7     6     0/6     0/6     0/6  ×1.17 n3      1/7   1/2         -        6/6  cacheR 7.0M ~medium
```

| Column | What it counts |
|---|---|
| REWORK | Completions that were later reopened |
| FORCED | Completions that overrode open blockers |
| SILENT | Completions carrying no annotation |
| UNBRIEFED | Completions of a task that no `tasqx brief`, or `tasqx_start_timer` that carried memory, was ever read for |
| CALIB | Median tracked-over-estimate, and how many completions had both |
| DROPPED | Work that was started and then cancelled |
| OVER | Completions that went past their token budget, of those that had one |
| UNPROVEN | Completions with a check still open or failed, of those that had checks |
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

### Chart samples

All three read the append-only event log, so they are history, not a snapshot —
and they are pure reads that never touch your tasks.

```console
$ tasqx chart throughput --weeks 12
Weekly throughput   added [#]  done [#]
  W27  added   0   done   0   net   0
  W28  added   0   done   0   net   0
  W29  added ##########  5   done ##  1   net  +4
  > 4-wk velocity 0.2 done/wk - WIP trending up
```

```console
$ tasqx chart heatmap --weeks 4
Completions - last 4 weeks   . 0  : 1-2  + 3-4  # 5+
  Mon . . . . 
      . . . . 
  Wed . . . . 
      . . . : 
  Fri . . . . 
      . . . . 
  Sun . . . . 
  > 1 done - current streak 1 days - best 1
```

```console
$ tasqx chart burndown --days 7
Remaining open - all tasks
    4  ______#
    0  2026-07-10 -> 2026-07-16
  > 4 left - up 4 over 7 days
```

| Chart | Flags |
|---|---|
| `throughput` | `--weeks <n>` (default 12) |
| `heatmap` | `--weeks <n>` (default 12), `--year` (52 weeks) |
| `burndown` | `--days <n>` (default 30), `--project <p>` |

## tasqx why

Not a report, but the same spirit: `tasqx why <ref>` breaks a task's urgency
score into its components, so the ordering of every list stays explainable.
See [Finding Tasks](Finding-Tasks.md#tasqx-why).
