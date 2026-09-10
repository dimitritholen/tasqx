# Dashboard redesign — the per-project board (2026-09-02)

Companion to the interactive mockup at
`docs/specs/2026-09-02-dashboard-redesign-mockup.html` (120×40, real store
data pulled via `tasqx api --no-daemon` on 2026-09-02 and embedded inline).
Supersedes the eight-panel layout of D58 §3b as a *layout*; the D58 data
path, entry conditions, config keys and refusal behaviour all stand.

## 1. Panel inventory

Five panels replace D58's eight. Every panel names the calls it reads.

| # | Panel | Shows | Reads |
|---|-------|-------|-------|
| 1 | PROJECTS | `all` plus every project with open work: open count, overdue dot, default `*`, the active scope marked `▸`. Selecting a row (or `[`/`]`) is the primary narrowing gesture. | `project.list` + the shared `task.list` snapshot |
| 2 | TASKS | The one task list, scoped to the selection. Under `all` it groups by project with a `▍project — n open · n overdue` header per group; under a project it is a flat urgency-sorted list. Columns: id, urgency (ramp), priority, title, tags, ⛔/▶ marker, due delta. Carries the row cursor, all row actions, the filter chips in its rule. Absorbs D58's NOW / NEXT UP / DUE / BLOCKED / RECENT: NOW is the ▶ row, DUE is the right-aligned delta column, BLOCKED is the ⛔ marker plus the `+blocked` filter, RECENT is a sort (`age`asc) rather than a panel. | shared `task.list` snapshot; `task.get` lazily for the cursor row |
| 3 | BURNDOWN | Remaining-open series and cumulative scope-added series, each a connected step-line in box-drawing glyphs with rounded corners (`─ │ ╭ ╮ ╰ ╯`), over an ideal line drawn as a dotted diagonal (first remaining → 0); window `week`/`14d`/`30d` drawn as its state in the rule, cycled with `w`. In-chart legend names each line with a `──` sample in its own color. Store-wide (D62's ruling on why stands). | window-bounded `event.list {from}` reconstructed per D59/D60 semantics |
| 4 | PULSE (statistics panel 1) | Throughput: done 7d/14d/30d on one line over a 28-day sparkline. Cycle time: median open→done overall and over the last 14d, sub-day medians formatted in hours (`2h`, `<1h`), with a trend word. Overdue: count now, worst offender with days over. Aging: oldest open task, count untouched ≥14d. Scope churn: added vs done over the window. | `event.list {from}` + shared `task.list` snapshot (all client-side arithmetic) |
| 5 | EFFORT (statistics panel 2) | Estimate remaining on open work, total and per project as bars, against total tracked. Tokens per project from the store's own accounting. | `task.list` snapshot (estimate on open rows) + `report.summary {group_by:"project", metrics:[est_total, tracked_total, tokens_*]}` |

Data path per refresh: one `task.list`, one `report.summary`, one
window-bounded `event.list {from}`, one `project.list`, plus lazy `task.get`
for the cursor row and `memory.search` only while the search overlay is open
with a query. `project.list` and `memory.search` are the two calls D58 does
not make today — see §6.

## 2. Key table

One table drives the footer and the help overlay (the D62 rule, kept). No
key collides with another; every action key is swallowed by the search and
filter inputs while those are open, so typing `s` into a query never starts
a timer.

| Keys | Action |
|---|---|
| `[` / `]` | previous / next project scope (`all` is first) |
| `1`-`5` | focus a panel |
| `tab` / `S-tab` | next / previous panel |
| `j` / `k` | move the cursor in the focused panel |
| `g` / `G` | cursor to the first / last row |
| `enter` | detail overlay for the cursor row (`task.get`) |
| `/` | fuzzy search overlay |
| `f` | filter bar |
| `F` | clear every filter |
| `s` | start / stop the timer on the cursor row (`task.start` / `task.stop`) |
| `d` | mark the cursor row done (`task.done`) |
| `t` | tag the cursor row (`tag.add`) |
| `p` | set priority on the cursor row, then `H`/`M`/`L` (`task.modify`) |
| `u` | set due on the cursor row (`task.modify`) |
| `w` | cycle the burndown window (`week`/`14d`/`30d`) |
| `r` / `R` | refresh now / toggle auto-refresh |
| `l` | leave and print the task list |
| `?` | help |
| `q` / `esc` | close the overlay, else the screen |
| `ctrl-c` | close, always |

`p` no longer opens `pick`: the dashboard's own cursor plus `s` covers
pick's job from this screen, and `p` for priority matches the mnemonic the
row actions establish. `pick` stays reachable as its own verb. Every write
lands as one existing API call, echoes a one-line confirmation in the
footer naming the call it sent, and names `u`ndo where `event.revert`
applies (the newest event only, per its contract).

## 3. Filter grammar

Space-separated tokens typed into the `f` bar, ANDed, rendered as chips in
the TASKS rule so the active set is always visible. `F` clears all.

```
status:pending|done|cancelled|backlog     pri:H|M|L        tag:<name>
due<Nd        (due inside N days; negative deltas i.e. overdue always match)
age>Nd        (created more than N days ago)
+overdue  +blocked  +waiting  +timer     (flag filters)
```

All tokens evaluate as a `retain` over the loaded snapshot — the same
mechanics as the D62 scope filter, zero extra calls. `status:` values other
than the snapshot default re-issue the one `task.list` with a filter string,
which the method already accepts.

## 4. Search scope and ranking

`/` opens a modal overlay; the query consumes every printable key (the D58
lesson about pick's query line, applied). Matching is subsequence-fuzzy,
in memory, over three fields of the loaded snapshot in this order: title,
tags, project. Each hit names what matched (`title` / `tag` / `project`)
and highlights the matched characters. Ranking: field order first, then
match compactness, then urgency.

Annotations and memory arrive on demand: while the overlay is open with a
query of ≥3 characters, one debounced `memory.search {query, limit}` runs
and its hits append below the snapshot hits, labelled `memory` with their
`source` (`task:#N` or doc). `⏎` on a task hit moves scope to that task's
project and lands the cursor on the row; `⏎` on a memory hit opens the
detail overlay of its source task.

## 5. Rung behaviour, 120×40 down to 56×14

Positively phrased: each rung states what the screen shows.

- ≥120×40: all five panels (the mockup's layout).
- ≥100×30: PROJECTS, TASKS, BURNDOWN, and one statistics slot that `4`/`5`
  or `tab` fills with PULSE or EFFORT.
- ≥80×24: PROJECTS and TASKS; BURNDOWN and the statistics share one slot
  filled by `3`/`4`/`5`.
- ≥56×14 (the D58 floor, unchanged): TASKS alone under the header and
  footer; the scope strip in the header is the project selector, and `[`/`]`
  still walks it. The header degrades by the D62 `build_bar` order and
  collapses to the active scope alone at the strip's minimum.

The mockup renders the 120×40 rung only.

## 6. What needs an API call D58 does not make today

Per item, honestly:

- **`project.list`** (PROJECTS panel, default `*` marker): exists in
  `dispatch::PARAMS`, is read-only, is *not* in D58's declared call set.
  One added read per refresh.
- **`memory.search`** (search overlay's memory hits): exists, read-only,
  not in D58's set. Called on demand only, never on refresh.
- **Throughput, cycle time, aging, overdue-now, scope churn** (PULSE):
  fully derivable from `event.list {from}` + the snapshot. No API change.
- **Estimate vs tracked** (EFFORT): derivable — `report.summary` carries
  `est_total`/`tracked_total`; "hours left on *open* work" sums `estimate`
  over open snapshot rows client-side. No API change.
- **Tokens per project** (EFFORT): derivable from `report.summary`'s
  `tokens_*` metrics. No API change.
- **Tokens per *day*** : NOT derivable today. `report.summary` has no
  time grouping and `tokens.attributed` event payloads carry sample
  metadata, not per-day totals. Needs `report.summary {group_by:"day"}`
  (or equivalent). The panel ships without the per-day row until then.
- **Cost** (per project or per day): NOT derivable anywhere — the store
  accounts tokens, no surface knows a price. Needs a ruling of its own
  (price table in config, or a cost field at attribution time). Drawn in
  the mockup as `cost/day: needs API †` rather than faked.
- **Overdue *trend*** (overdue count over time): not derivable at
  acceptable cost — it needs each task's due *history*, which means
  replaying `modify` payloads; ships as overdue-now plus the worst
  offender instead, which answers the same question the trend was for.

## 7. Theme roles

Existing roles used as ruled (`header`, `project`, `tag`, `priority.*`,
`overdue`, `timer.active`, `urgency.ramp`, `accent`, `muted`). New roles
this design introduces, all with fall-through defaults so every shipped
theme stays complete:

- `filter.chip` — the active-filter chips (defaults to a `muted`-bg block).
- `search.match` — matched characters in search hits (defaults to `accent`).
- `chart.ideal` — the burndown ideal line (defaults to `muted`).
- `chart.scope` — the scope-added series (defaults to `warn`, so the two
  burndown lines hold the accent/warn pairing on every built-in theme).
- `group` — per-project group headers under `all` (defaults to `project`, bold).

The header's rightmost cell reports the running timer as `▶ #N` (or
`no timer`), recomputed from the snapshot on every draw — a timer started
from the cursor row updates it in the same frame.

**Step-line over braille, decided here rather than re-litigated in the
port:** ratatui's `Chart` with `Marker::Braille` draws true diagonals, but
braille dots are one sub-cell thin and grey out at panel sizes; the rounded
step-line keeps the stroke weight and per-series color that make the chart
read as two lines. Its glyphs (`─ │ ╭ ╮ ╰ ╯`) sit in the box-drawing set
whose East-Asian-ambiguous width the dashboard's frames already accepted
(the class D76 records), so no new width edge is introduced.

## 8. §12 entry (landed as D80)

### D80 — The dashboard is a per-project board with a working cursor, and read-only was the scaffolding, not the point

**Decision:** The dashboard's eight panels become five — PROJECTS, TASKS,
BURNDOWN, PULSE, EFFORT — over the same snapshot-plus-summary-plus-events
data path, adding two existing read methods (`project.list`, and
`memory.search` on demand) and no new one. The task list is singular and
scoped: project selection via the PROJECTS panel or `[`/`]` is the primary
narrowing, `all` opens grouped by project. The cursor row acts: `s`, `d`,
`t`, `p`, `u` send the write methods every other surface already sends,
each echoing the call it made and naming undo where `event.revert` applies
— superseding D58's read-only ruling and D62's "`⏎` opens a read" clause,
while D58's entry conditions, config keys, refusal shape and D62's scope
strip, key-table and lazy-`task.get` rulings all stand. Filters are chips
over the snapshot; `/` is modal fuzzy search over title/tags/project with
memory hits on demand. The burndown draws remaining, ideal and scope-added
as series under the `w` window. Five theme roles are added, all defaulting
into existing roles.

**Why:** five separate task panels made every reader scan twelve projects
five times, and a viewer that cannot touch the row it points at sends its
user back out to the CLI mid-thought — the cursor D62 built is the
confirmation-free `expected_rev` carrier D58 said a viewer didn't need.

## 9. §11 phase-table lines this changes

Walked per the CLAUDE.md rule — a phase line contradicting a §12 ruling is
a plan nobody can execute:

- **Full ratatui TUI** row: "the dashboard is ruled (D58) and shipped, its
  legibility pass is ruled (D62) and scheduled" gains "; its per-project
  board and row actions are ruled (D80) and scheduled". **Applied.**
- **CLI** row: the D58 clause "the screen when a human is watching" stands;
  strike nothing.
- **Presentation** row: "The dashboard's panels use the existing semantic
  theme roles only, so every shipped `themes/*.toml` is complete for it on
  day one" becomes "…use the semantic theme roles, five of which (D80)
  default into existing roles, so every shipped `themes/*.toml` remains
  complete."
- The §1105 build-status note's panel count ("eight panels") is not
  restated anywhere in §11 tables; no other line names the panel set.
