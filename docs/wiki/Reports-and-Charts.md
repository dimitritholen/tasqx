# Reports and Charts

What happened, what's left, and where the time and tokens went — in the
terminal, or as an HTML page you can send to someone.

## tasqx report

Summary counts, optionally grouped.

```console
tasqx report                    # totals
tasqx report project            # grouped by project
tasqx report status             # or by status, or priority
tasqx report +urgent            # any filter narrows the scope
```

Two output modes, same numbers:

```console
tasqx report --html --out review.html   # one self-contained HTML file
```

The HTML report is a single file with inline styling and no external
requests — it works from a mail attachment, a chat upload, or a USB stick.
A filter scopes both modes identically, so the page and the terminal table
always answer the same question.

Cancelled tasks are not counted unless you pass `--all` or your filter names a
status explicitly — a report about work shouldn't be padded by work you
decided not to do.

## tasqx chart

Charts drawn right in the terminal, from the event log.

```console
tasqx chart throughput      # tasks added vs done, per week
tasqx chart heatmap         # GitHub-style activity calendar
tasqx chart burndown        # open tasks over the last N days
```

- **throughput** answers "am I finishing as fast as I'm adding?"
- **heatmap** answers "when do I actually get things done?"
- **burndown** answers "is the pile shrinking?" (`--days 30` widens the
  window)

The charts degrade cleanly from truecolor terminals down to no color at all.

## tasqx why

Not a report, but the same spirit: `tasqx why <ref>` breaks a task's urgency
score into its components, so the ordering of every list stays explainable.
See [Finding Tasks](Finding-Tasks.md#tasqx-why).
