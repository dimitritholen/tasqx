# Themes, charts and reports

Default output should be something you want to look at. Themes drive the
terminal and the HTML report from the same palette, and degrade honestly when
the terminal cannot keep up.

## Themes

Five built-ins. Resolution order: `--theme`, `$TASQX_THEME`, `config.toml`,
default.

```console
$ tasqx theme list
  THEME
* nord
  gruvbox
  dracula
  solarized
  mono
```

The `*` marks the theme in effect. `tasqx theme show [name]` previews every
role, a sample drawn in it beside its colour and emphasis, plus the urgency
ramp's bands, rendered at your terminal's *real* capability. Set one
permanently:

```
# config.toml
[theme]
name = "gruvbox"
```

Drop a `.toml` in `$TASQX_CONFIG_DIR/themes/` and it appears in `theme list`
alongside the built-ins.

> **Note** Capability is detected, not assumed. Pipe tasqx into `cat` and the
> colour goes away; on a terminal without Unicode the block glyphs degrade to
> ASCII rather than emitting mojibake. `mono` is there for when you want that
> unconditionally.

> **Note** Width is detected too. `tasqx list` sizes its columns to what is
> actually in them and to the terminal it is printing into: a column no task
> fills — `DUE` on a store with no due dates — is not drawn at all, and the
> space goes to the titles. Through a pipe there is no width to detect, so the
> table lays out for a fixed 100 columns and two runs of the same store stay
> diffable.

Four environment variables override the detection:

| Variable | Effect |
|---|---|
| `NO_COLOR` | Set to anything: drop all colour, keep bold/underline. Wins over everything below. |
| `CLICOLOR_FORCE` | Set to anything but `0`: force colour even through a pipe — for `less -R` and CI logs. |
| `TASQX_FORCE_COLOR` | Set to anything: same as `CLICOLOR_FORCE`, scoped to tasqx. |
| `COLUMNS` | How many columns wide to lay tables out. Beats both the terminal's own answer and the piped default — for a multiplexer that misreports its size, or to pin the width of captured output. Clamped to 40–160. |

## Reports

`tasqx report [group_by] [filter] [--all]` — group by `project` (default),
`status`, or `priority`. Estimates total as ISO-8601 durations, which is why
[`est:`](scheduling.md) is parsed at the edge rather than stored as opaque text.

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

## Charts

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

## The HTML report

`tasqx report --html` emits a weekly review as one self-contained file — inline
CSS, inline SVG charts, no external requests — themed from the same palette as
your terminal. Exactly like the page you are reading.

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

> **Note** Both HTML surfaces hold the same line: no CDN, no web fonts, no
> remote images, no scripts fetched from anywhere. Mail the file, commit it,
> open it on a plane — it renders the same.
