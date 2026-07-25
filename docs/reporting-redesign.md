# Redesigning `tasqx report --html`

Research, design spec, framework evaluation and a proposed decision entry for the
reporting page. Written alongside a field test of the new token accounting, which
generated the data every screenshot and number in here is drawn from.

**Status:** design only. Nothing in `crates/tasqx-cli/src/html.rs` has been
changed. The prototype lives in `docs/reporting-redesign-prototype.py` and renders
`docs/reporting-redesign-prototype.html` from real store data.

---

## 0. What the code actually does today

Every claim below was read out of the tree at `dee2c48`, not inferred.

| Claim | Verified | Where |
|---|---|---|
| `report.summary` aggregates four separate token buckets | ✅ | `engine/reports.rs:73-141` — `Agg` keeps `tokens_in/out/cache_read/cache_creation` apart, `tokens_total` derived only at emit (`:186`) |
| The core comments that a blended total would lie | ✅ | `engine/reports.rs:73-75` |
| Charts are pure clients of `report.summary` / `task.list` | ✅ | DESIGN.md §8; `html.rs:42-57` issues four reads and templates the result |
| Theming is semantic roles with light/dark and 5 built-ins | ✅ | DESIGN.md §8; `theme.rs:577-605` |
| `urgency.ramp` is used as an SVG `<linearGradient>` | ✅ | `html.rs:643-668` (`ramp_stops`) |
| The timeline draws on the event log | ✅ | `event.list` returns `op ∈ {add, start, stop, done, modify, tokens.attributed, …}` |

Three things the brief got wrong or under-stated, corrected here:

1. **The token metrics are opt-in per call.** `report.summary` defaults `metrics`
   to `["count"]` (`reports.rs:26`). A caller that does not name the `tokens_*`
   metrics gets a payload with no token fields at all — not zeros, *absent keys*.
   `html.rs` gets them today only because `report_params` asks for them.

2. **`group_by` has no time axis.** It is a closed set of
   `project | status | priority` (`engine.rs:59`). "Tokens over time" therefore
   cannot be a pure client of `report.summary`; it has to come off the event log
   or be added to the API. See §5.

3. **The self-containment guard is stricter than the invariant.**
   `html.rs:1014` asserts the document contains no `href=` **and** no `<script`
   at all. That is not what DESIGN.md §8/§8a promises — `docs.rs` ships an inline
   `<script>` and in-page `#anchor` links and is fully self-contained. See §6.

And one thing the presentation layer gets wrong today:

> `engine/reports.rs:73` keeps the four buckets separate specifically because
> "cache tokens cost a fraction, so a blended total would lie" — and then
> `html.rs:273` renders exactly one blended `stat("AI tokens")` in the sticky
> header, and `render.rs` prints one blended `TOKENS` column in the terminal
> table. The core's care is discarded at both output surfaces.

Measured from this session's own store, **all time** (no range filter):

```
tasqx-reporting-redesign | in 136 | out 83 479 | cacheR 13 630 240 | cacheW 186 965 | total 13 900 820
```

The label matters now that the prototype defaults to a 30-day window: these are
unwindowed figures. They happen to be identical at `--range 30d` today, because
every measurement in this store belongs to a task that closed inside the last
day — but that is an accident of a young store, not an invariant, and the page
prints its own window precisely so the two can be told apart.

Weighted by the published relative prices (input 1.0, output ~5.0, cache read 0.1,
cache write 1.25):

| bucket | share of volume | share of cost |
|---|---:|---:|
| cache read | 98.1 % | 67.7 % |
| cache write | 1.3 % | 11.6 % |
| input | 0.0 % | 0.0 % |
| output | **0.6 %** | **20.7 %** |

Output is 0.6 % of the blended total and a fifth of the bill. A single number
cannot carry that, and the blended headline is wrong in the flattering direction.

---

## 1. Research — time-management and task reporting

### What the incumbents actually show

| Product | Views | Recurring charts | Controls |
|---|---|---|---|
| **Toggl Track** | Summary, Detailed, Weekly | stacked bar over time (stackable by billable/member/client/project/task/tag), pie for proportion | date-range selector top-left with presets; a "show / stack by" pair top-right |
| **Clockify** | Summary, Detailed, Weekly + a Team Dashboard | bar + pie, time breakdown per person by billability or project | filter by team, client, project, task, tag, status, description |
| **Harvest** | Dashboard + custom time reports | totals and billable split per project/client | filter by period, client, project, task, member; CSV/PDF/Excel export |
| **Jira** | Sprint Report, Burndown, Velocity, Cumulative Flow | burndown, burnup, CFD, velocity, cycle time, throughput | sprint/board scope |

The **Summary / Detailed / Weekly** triad is close to universal in the time-tracking
half: an aggregate with a chart, a raw list of entries, and a grid. The
**date-range control** is the one component every single product ships, always in
the same place.

### What people use versus what looks impressive

- Atlassian's own tutorial positions the burndown as the daily-tracking primitive,
  and practitioner guidance is consistent: **start with cycle time and burndown**;
  CFD, velocity and Monte Carlo are explicitly framed as "for mature teams". The
  impressive charts are the ones added last and consulted least.
- The academic anchor is Bach et al., *Dashboard Design Patterns* (IEEE VIS 2022;
  TVCG 2023), a systematic review of **144 dashboards** yielding eight groups of
  design patterns across screenspace, interaction and information shown.
- The practitioner literature converges on **5–7 primary KPIs per view** and on a
  retirement test for metrics: no one mentions it in a meeting, it has not
  influenced a decision in 90 days, or nobody remembers why it was added.

The honest summary is that the evidence base for "what people use" is
practitioner consensus rather than controlled study, and it should be cited that
way. What it consistently says: **fewer numbers, chosen for the decision they
drive.**

### Conventional UX patterns worth inheriting

- A stated range applying to the whole page. Every product ships this as a
  *control*; a generated static document cannot (re-querying needs `fetch`,
  re-aggregating in the browser needs a second implementation of core's roll-up,
  and reflecting the choice in the URL needs `replaceState`, which throws
  `SecurityError` on `file://`). It becomes a **generation-time parameter** that
  the page prints prominently, including the literal filter clause so the reader
  can reproduce the set.
- A grouping selector adjacent to the chart it regroups.
- Drill-down from aggregate → entity → raw entries.
- Empty states that name the next action rather than showing a blank panel.
- Detailed view always exports.

---

## 2. Research — AI token-usage reporting

### Anthropic Console / Usage & Cost API

- Token consumption is broken down as **uncached input, cached input, cache
  creation, and output** — the same four tasqx stores.
- Grouping by API key, workspace, model, service tier, context window, data
  residency, speed; **time buckets of `1m` / `1h` / `1d`**.
- **Usage and cost are two different endpoints** (`/usage_report/messages` and
  `/cost_report`). Cost is daily-granularity only and is grouped by workspace or
  description. The separation is deliberate: usage is a measurement, cost is a
  derivation over a price list that changes.
- Documented cache economics: **cache read = 0.1× input**, **cache write = 1.25×
  input**.

### Langfuse

- Buckets are **mutually exclusive by contract**: "each token must be counted in
  exactly one key. In particular, `input` must exclude tokens that are already
  counted in another `input_*` key."
- Cost is attributed per generation observation — either ingested from the
  provider or inferred from token counts × model pricing at ingestion — and rolls
  up per trace.

### LangSmith / Helicone

- LangSmith: the trace tree is the detail view; per-project dashboards are
  auto-generated over trace counts, error rates, token usage and cost, with
  cost/token **trends over time** as the headline chart.
- Helicone: wire-level proxy, automatic cost tracking, weakest agent-level
  attribution.

### The shared shape

Four buckets, never blended. Cost separated from usage. Time as the primary axis.
Attribution down to the smallest unit of work the system knows about (a trace, a
generation, a run).

---

## 3. Where the two conventions conflict — and which wins

| # | Conflict | Time-tracking says | AI-token reporting says | Winner for tasqx | Why |
|---|---|---|---|---|---|
| 1 | **Blending** | Blend freely — hours are hours, "12h this week" is true | Never blend — the buckets differ 0.1×–5× in price | **Token convention** | The core already refuses to blend (`reports.rs:73`). The presentation layer must stop undoing it. |
| 2 | **Primary axis** | Entity-first (per project/client/task), time secondary | Time-first (`bucket_width` is close to mandatory) | **Time-tracking convention** | tasqx's unit of work is the *task*. It is also what the API supports: `group_by` has no time axis at all. Time becomes the secondary axis, on the event-log timeline. |
| 3 | **Money** | Show revenue/cost when the user supplied rates | Always show currency | **Time-tracking convention, in its strict form: show nothing** | tasqx has no price list, no model-price feed, and prices move. A wrong dollar figure is worse than no dollar figure. Show the two costs tasqx actually measures — tokens and wall-clock time — and name which *bucket* dominates the bill without pricing it. |
| 4 | **Granularity of drill-down** | Down to the time entry | Down to the individual request/trace | **Down to the task, then the measurement** | tasqx stores per-measurement rows (`token_usage`) but attributes them to a task. The task is the join point between the two worlds and is the right leaf. |

Conflict 1 is the load-bearing one. Everything else follows from it.

**A note on conflict 3.** The page does render one derived cost claim: *"cache
read drives ~68% of the cost"*. That is a ratio over published relative weights
(0.1 / 1.25 / 1.0 / ~5.0), not a currency figure, and it is far more stable than a
dollar amount — the *relative* prices have held across model generations while the
absolute ones have not. It answers the question a total cannot: which bucket
should you attack.

---

## 4. Design spec

### Layout

One centred column, widened from the current 72ch to **92ch** — the cost table
needs six columns and the current measure forces a horizontal scroll on the panel
that matters most. Sticky header. Sections in decision order, not data order.

```
┌─ sticky header ─────────────────────────────────────────────┐
│ tasqx review    open · done · tracked · overdue             │  ← four, tagged
│                 (each tile says "now" or "in range")        │
└─────────────────────────────────────────────────────────────┘
  §range      the stated window                  (label · boundary · clause;
                                                  OUTSIDE the sticky element)
  §activity   Completions, <range>               (inline SVG, urgency.ramp)
  §tokens     Token burn by project · <range>    (stacked bars + legend)
              cache read · cache write · input · output   ← four, never one
  §cost       Cost per task · <range>            (table, two currencies)
  §unattributed  Completions with no attribution (count + two lists, hidden at zero)
  §timeline   Started / stopped / completed · <range>  (event log, filterable)
  ─────────────────────────────────────────────────────────────
  #task-N     detail panels                      (:target, hidden until linked)
```

§activity, §tokens, §cost, §unattributed and §timeline all carry **the same**
range label. The header stats are as-of-now and say so on each tile — backlog is
a state, throughput and tokens are a window, and no completion bound can select a
task that never completed.

### Colour: the theme is not a chart palette

This is the finding with the most rework in it.

tasqx themes are **terminal** palettes, tuned against a dark terminal background.
Feeding `nord`'s roles straight into SVG fills on the report's light `--card`
surface fails hard. Measured with the dataviz validator:

```
palette #88c0d0,#b48ead,#a3be8c,#ebcb8b  (accent, tag, timer.active, warn)
  [FAIL] Lightness band    #88c0d0 0.775, #ebcb8b 0.855  (band 0.43–0.77)
  [FAIL] Chroma floor      all four below 0.10 — they read as grey
  [FAIL] Normal-vision floor  worst adjacent #ebcb8b↔#a3be8c ΔE 10.9 (floor 15)
  [WARN] Contrast          all four below 3:1 against the surface
```

The existing report already fills its SVGs with `accent` / `warn` / `danger` on
`--card`, so this is a pre-existing defect, not one the redesign introduces.

The fix is what a design system means by *snap each slot to the nearest passing
step*: keep the theme's **hues**, re-step **lightness and chroma per colour
scheme**. Both rows below pass all five checks.

| Bucket | Role hue | Light (`#fcfcfb`) | Dark (`#2e3440`) |
|---|---|---|---|
| `tokens_cache_read` | accent / cyan | `#00688f` | `#2f9fc6` |
| `tokens_cache_creation` | warn / amber | `#a35d0a` | `#c07a1e` |
| `tokens_in` | tag / purple | `#8e4b9c` | `#a962c0` |
| `tokens_out` | timer.active / green | `#41762b` | `#5fa036` |

Two consequences worth stating plainly:

- **Stack order is a colour-vision decision, not an aesthetic one.** Purple↔cyan
  collapses under deuteranopia and amber↔green under protanopia. The order
  cyan → amber → purple → green keeps both confusable pairs non-adjacent; worst
  adjacent pair is then ΔE 15.8 (deutan). A different order would need secondary
  encoding to be legal.
- **In dark mode the chart surface must be `--bg`, not `--card`.** The dark band
  is L 0.48–0.67, and against the current `--card` (≈`#545962`) every step either
  leaves the band or drops under 3:1. On `#2e3440` the same four pass.

`urgency.ramp` stays exactly what it is — a **sequential** ramp — and is used only
where magnitude is encoded (the activity sparkline gradient), never as four
categorical fills. That is the one thing the brief's example widget spec got
backwards.

### Widget inventory

---

#### Widget: Header stat row
- **Question it answers:** is anything on fire, and what did this range cost?
- **Data source:** `store.export` (derived open/overdue/done counts) + `task.list`
  `tracked` + `report.summary` → four `tokens_*` metrics summed across groups.
- **Mark:** four stat tiles, mono tabular numerals, each tagged `now` or
  `in range` on a sub-line. No plot. The four token tiles used to sit here too —
  eight tiles, two different *kinds* of number sharing one row with no read order
  between them, 181px of a 726px viewport at 420px wide. They now live in
  §tokens, beside the bars that decompose them.
- **Theme roles:** `overdue` (→ `--danger`) on the overdue tile when non-zero.
- **Interaction:** none. It is the 5-second read.
- **Replaces:** the current single blended `stat("AI tokens")` at `html.rs:273`.

#### Widget: Completions, in range
- **Question it answers:** is work still closing, and when did it stop?
- **Data source:** `event.list` → `op == "done"`, bucketed by calendar day.
- **Mark:** inline SVG column chart, 4px rounded tops, 2px baseline stub on empty
  days so a gap reads as *measured zero* rather than *missing* (the stub now
  carries its own `<title>`, so hovering a zero says `0 done` instead of nothing).
- **Axis:** **every day carries a number**, and the label step is *derived* from
  the viewBox geometry rather than hardcoded to every third day. On this file's
  684u plot width a two-digit 11px monospace label needs 17.2u including gutter,
  so break-even is at **39 days**; past that the step grows one whole day at a
  time and the figcaption says so in words. `html.rs`'s plot is 674u, i.e. 39.2 —
  recompute it there, do not copy this number.
- **Month rules:** day-of-month numbers stop being identifiers as soon as the
  range can repeat one, so each month's first slot gets a 1px rule and a `%b`
  label (with the year on the first mark only).
- **Range cap:** the chart draws at most `CHART_DAYS_CAP` (90) days. When the
  page range is wider — `--range all`, or anything over 90 — the figcaption says
  so explicitly, because the section heading would otherwise claim "all time"
  over a 90-day chart. The svg's `min-width` is proportional to the day count
  and emitted inline, so a 7-day chart no longer forces a phone scrollbar.
- **Theme roles:** `urgency.ramp` as a real `<linearGradient id="ramp">`, matching
  `html.rs::ramp_stops`.
- **Empty state:** *"Nothing completed in the last N days"* + *"The bars appear as
  `tasqx done` events land."* (N is the real day count, not a hardcoded 21.)
- **Interaction:** `<title>` per bar (native SVG tooltip, zero JS).

#### Widget: Token burn by project
- **Question it answers:** where did this range's tokens actually go, and which
  bucket is driving the bill?
- **Data source:** `report.summary` `group_by=project` → the four `tokens_*`
  metrics.
- **Mark:** the **four bucket tiles** (re-homed from the sticky header, so the
  totals sit directly above the bars that decompose them), then **two**
  horizontal stacked bars per project row — a `volume` bar and a `cost` bar of
  the same four segments weighted by relative price. 2px surface gap between
  segments, 4px rounded data-ends.
- **Both bars are now scaled**, each to the heaviest project *on its own axis*:
  volume in raw tokens, cost in weighted tokens. The cost bar used to be pinned
  at `width:100%` by construction — correct within a row, but down the column it
  claimed every project cost the same, and it claimed it before any label was
  read. The consequence has to be stated rather than left implicit: the two bars
  now have **different denominators**, so lengths are comparable *down a column*
  and never *between the two bars of one row*. Two things close that: a per-row
  cost-share percentage (unit-free, correct across rows, needs no axis) and a
  `.scalenote` under the bars naming both denominators in words.
- **Why two bars:** this started as one bar and a footnote. Rendering it against
  real data settled it — with cache read at 98 % of volume the other three
  buckets measured 7px, 2px and 3px wide, so three of the four buckets were
  visually nil and the single claim the panel exists to make was carried entirely
  by a sentence underneath. The second bar makes the argument the way a chart is
  supposed to: by looking *different* from the one above it.
- **Theme roles:** the four derived bucket steps above; `project` for the row label.
- **Empty state:** the four bucket tiles collapse to a single `—` tile labelled
  *"tokens · not measured yet"* — four zeros would imply four measurements of
  zero, which is a different and false statement. Plus *"No token data in
  &lt;range&gt;."* + *"Token accounting is opt-in: `tasqx config set
  tokens.enabled true`, then run `tasqx daemon` so the attribution thread can
  reconstruct spend after each completion. Or widen the window with `--range
  all`."* — the second and third clauses matter, see §7; the third is the only
  place the empty token panel tells the reader that the *range* may be the
  reason.
- **Interaction:** click a row → filters the timeline below to that project;
  clicking the active row clears. Legend chips toggle a bucket across every bar at
  once — *and dim the matching tile*, because with the tiles now sitting directly
  above the bars, toggling `cache read` off visibly empties the bars while an
  undimmed tile still reads 13.63M.
- **Sub-label:** *"cache read drives ~68% of the cost"* — the weighted-dominance
  sentence from §3.

#### Widget: Cost per task
- **Question it answers:** what did this task cost me, in both currencies?
- **Data source:** `store.export` → `tokens[]`; **plus** `task.list` → `tracked`
  (see the API delta in §5 — no single read has both).
- **Row selection, stated on the page:** a task appears if it was **started,
  stopped or completed inside the range**, or carries a measurement created
  inside it. Not a pure completion bound — that would drop the task you started
  on Monday and have not finished, which is the work a weekly review is most
  about. A row set nobody can predict is a row set nobody can trust, so the
  sub-line says this in words.
- **Mark:** table; mono tabular time column; a four-segment micro-bar per row
  scaled to the heaviest task; a confidence badge.
- **The "skip rows with nothing" gate is deleted.** The old
  `if not toks and tracked <= 0: continue` silently dropped exactly the tasks
  this panel exists to surface — a completion with no spend and no timer. On this
  store it hid 2 of 15 in-range completions.
- **`tracked` has three states, not two.** A duration; `0s ⚠` (a timer ran and
  measured nothing — `tasqx start` on another task stops the running one
  silently, so a worked interval lands on the wrong task); or `never timed` (no
  `start` event anywhere in the log). `tracked` alone cannot tell the last two
  apart — both are `PT0S` and both used to render as one dash — but the event log
  can. Measured: #12/#13 never timed, #21/#23 timed zero.
- **The confidence badge is the WEAKEST measurement in the row**, not the first.
  The bug it replaces is `sorted(conf)[0]`: over the closed vocabulary
  `tokens.rs:40-43` publishes, alphabetical order does not pick arbitrarily — it
  reliably picks the *strongest* value, so a row mixing one `high` transcript
  parse with one `low` directory-scan guess rendered HIGH. A total is only as
  trustworthy as its weakest input (§5 D-3 argues the same one level up). A mixed
  row carries a `*` and names every grade in its title; a value outside the
  vocabulary ranks below `low` and renders `unknown` with a dashed border,
  because `require_confidence` should have refused it at write time.
- **Sortable columns.** Every `<th>` that sorts carries `aria-sort` and a real
  `<button>`; the sort keys are the raw integers on `data-tracked` /
  `data-tokens`, never the rendered `3h 20m` / `1.23M` strings, so a sort
  reorders `<tr>` nodes and never re-parses or rewrites a cell. Server-side order
  stays tokens-descending, so the page answers *"what cost most?"* with JS
  disabled.
- **Row cap, ranked in both currencies.** `COST_ROW_LIMIT` = 60. The cap ranks by
  whichever currency puts a task highest — a pure tokens-descending cut would
  hide the longest-tracked task *and* decapitate the zero-token `never timed`
  rows the panel now exists to show. The clip note states the count and that
  sorting reorders the kept rows only.
- **Theme roles:** the four bucket steps; `timer.active` (→ `--ok`) for a `high`
  confidence badge.
- **Empty state:** *"No measured work in this range."* + *"A task appears here
  once it is started, stopped or completed inside the range. Widen it with
  `--range 90d` or `--range all`, or start something with `tasqx start &lt;id&gt;`."*
- **Interaction:** the id cell is `href="#task-N"` → opens that task's detail
  panel. Emitted through `TaskRefs.link()` like every other `#task-N` reference on
  the page — see guard 6.
- **Why confidence is on the row and not in a footnote:** `high` means the
  transcript was parsed *and* the session id was verified against it
  (`attribution.rs:171`); `low` means a directory scan guessed. Those are different
  numbers and must not look alike.

#### Widget: Completions with no attribution
- **Question it answers:** which completions in this range recorded no token
  spend at all — and therefore, how incomplete is every token number on this page?
- **Data source:** `store.export` → `tokens[]`; `event.list` → `tokens.attributed`.
- **Mark:** a headline count in the shape *"12 of 15 completions in range"* (a
  bare `12` cannot say whether that is most of them or a rounding error), then
  **two** lists.
- **Two populations, because they need two different fixes:**
  - *attribution never ran* — no `tokens.attributed` event exists at all. No
    daemon was listening, or `tokens.enabled` is false. Measured on this store:
    **10** of 15. These appeared **nowhere** on the old page, not even as a
    footnote, because the footnote is keyed off an event that was never written.
  - *attribution ran and found nothing* — a `tokens.attributed` event carrying
    `samples: 0`. Measured: **2**. Causes named in the order worth checking: the
    transcript had not flushed when attribution fired; the `done` event carried no
    correlation, leaving only a fuzzy directory scan; or the work genuinely
    happened outside any instrumented tool.
- **Theme roles:** `--danger` for the border, the headline and the per-list
  counts — the panel only exists when there is something to fix.
- **Empty state:** **the absence of the section itself.** At zero it renders
  nothing — no heading, no anchor. A panel that says "all good" every week is a
  panel nobody reads on the week it matters. Verified: absent from the sparse
  fixture.
- **Interaction:** each id links to its task panel, through `TaskRefs.link()`.

#### Widget: Lifecycle timeline
- **Question it answers:** what happened, in order, and what did each interval cost?
- **Data source:** `event.list` → `op ∈ {start, stop, done, tokens.attributed}`,
  joined to `store.export` by `entity_id`.
- **Mark:** day-grouped vertical list, 2px rule, one 8px dot per event.
- **Theme roles:** `timer.active` for `start`, `accent` for `stop`,
  `urgency.ramp` hot end for `done`, `muted` for `tokens.attributed`.
- **Empty state:** *"No lifecycle events in this range."*
- **Interaction:** filtered live by the project row selected above; each row links
  to its task panel.
- **Deliberate inclusion:** a `tokens.attributed` marker with **no** measurement
  renders as *"no spend found in window"*. That is the difference between *cost
  nothing* and *was never measured*, and hiding it is how a token report becomes
  quietly wrong.

#### Widget: Task detail panel
- **Question it answers:** what is this task, what did it cost, what did I write
  down about it?
- **Data source:** `store.export` → the full task object incl. `annotations[]` and
  `tokens[]`.
- **Mark:** one panel per **reachable** task — the page collects its own `#task-N`
  references through `TaskRefs` and renders exactly that set, bounded by
  `PANEL_BUDGET` (160). `display:none` until `:target` selects it. Metadata as a
  `<dl>`, the four buckets as a small grid, annotations as a timestamped list with
  `white-space: pre-wrap`. Rendering one panel per *exported* task put 24 panels in
  a 94 KB page with >90% of the bytes never displayed; on a 500-task fixture it was
  1 866 752 B / 500 panels.
- **`tracked` carries the same three states as the cost table.** #21's panel used
  to read `tracked —` while carrying 8.9M attributed tokens and a `start`+`stop`
  pair: the single most misleading cell on the page, since it read as *no work
  happened* on the task that consumed the most tokens in the store.
- **Annotations clamp to six lines** with a CSS-only disclosure over a **single**
  copy of the text: a clipped checkbox before the body, a `<label>` after it, and
  `.annx:checked ~ .body` lifting the clamp. The obvious `<details>`/`<summary>`
  route would duplicate the body into the summary — doubling exactly the bytes
  this change exists to shed — and a `<summary>` holding a 300-line body announces
  that entire body as the button's accessible name. Short annotations get no
  control and no clamp markup at all.
- **A reference the export does not carry renders an "outside this report's
  scope" stub**, not a dead anchor.
- **Theme roles:** `accent` for the panel border and the id.
- **Empty state:** *"No token measurement. A task is only attributed when its
  `done` event carries correlation (client / session_id / transcript_path)."*
- **Interaction:** `:target` for the **reveal**; ~20 lines of JS for **focus and
  announcement**. No browser focuses a fragment target, so without it a keyboard
  user is left tabbing the page *behind* an open panel and a screen-reader user is
  told nothing. The panel is `tabindex="-1"` + `role="region"` +
  `aria-labelledby`; the script focuses it on `hashchange` and at load, and
  Escape closes. The History API stays untouched — assigning `location.hash` is a
  **navigation**, not History state, and is `file://`-safe; `pushState` throws
  `SecurityError` there (DESIGN.md §8a).

---

## 5. `report.summary` API delta

Three additions the design needs. None are computed in the page; each is listed
here rather than smuggled into the presentation layer, per DESIGN.md §8.

### D-1. `store.export` should carry `tracked_seconds`

**Needed by:** *Cost per task*.
**Today:** `store.export` returns `tokens[]` and `annotations[]` but **no** tracked
time. `task.list` returns `tracked` and `active_since` but **no** tokens and **no**
annotations. No single read returns all three, so the prototype issues a fifth call
and joins on `id`.
**Delta:** add `tracked_seconds` (or `tracked`, matching `task.list`'s spelling) to
the canonical export task object.
**Note:** the export is the round-trip form, so this needs a decision about whether
tracked time is *derived* state that `store.import` should recompute from the event
log rather than accept. That is why this is a listed delta and not a patch.
**Now load-bearing for the range:** the in-range tracked total in the header comes
off this join, so D-1 is no longer only about one table cell.

### D-2. A time axis for token roll-ups

**Needed by:** any "tokens over time" panel — the one chart every AI observability
product leads with, and the one this design deliberately omits.
**Today:** `SUMMARY_GROUP_BY` is `project | status | priority`. There is no way to
ask for tokens per day.
**Delta:** either add `day` / `week` to `SUMMARY_GROUP_BY`, or add a
`bucket_width` param the way the Anthropic Usage API does (`1h` / `1d` / `1w`),
emitting one row per bucket per group.
**Recommendation:** `bucket_width` rather than a `group_by` value, so the time axis
composes with the existing entity axis instead of replacing it — the interesting
question is *tokens per project per day*, which a single-axis `group_by` cannot
express.
**Until then:** the activity sparkline counts `done` events off the event log,
which is honest but is not the same chart.
**And the page reads `report.summary` twice.** This is a *consequence of D-2*, not
a design preference. The page mixes two kinds of question and they do not share a
window: `summary_now` (scope only) answers the backlog — open, overdue, about
*now* — and `summary_window` (scope **and** the range clause) answers throughput
and tokens, about the *window*. One filtered summary cannot serve both, and the
failure is silent rather than loud: under any `completed.` predicate the non-token
metrics change meaning without changing name (`count` stops being "tasks" and
becomes "completions in window"), and `overdue` goes **structurally** to zero
because `reports.rs:142` guards it with `status.is_open()` while `filter.rs:335`
makes a null `completed` fail every completion bound — the two conditions are
mutually exclusive by construction. Measured: `completed.after:-1d` returns
`overdue: 0` for every group. A page that fed the windowed summary to the header
would print a confident `0 OVERDUE` meaning *"we filtered out everything that
could have been overdue"*. With a real time axis one read could carry both.

### D-3. `report.summary` should expose measurement provenance

**Needed by:** the confidence badge, at the aggregate level.
**Today:** confidence and source live on individual `token_usage` rows, reachable
via `task.get` / `store.export` but not through `report.summary`. A group's token
total can silently mix one `high`-confidence transcript parse with three `low`
directory-scan guesses, and the roll-up says nothing.
**Delta:** an optional metric such as `tokens_confidence` returning the *weakest*
confidence contributing to the group (or a small `{high, medium, low}` count map).
**Why weakest, not average:** a total is only as trustworthy as its worst input,
and averaging confidence produces a number with no meaning.

---

## 6. Framework choice

**Recommendation: hand-rolled vanilla JS.** ~4.7 KB inline, no vendored library.
The generated prototype's script block measures **4 669 bytes** raw (3 259 bytes
comment-free). It grew from 2 129 B when drill-down focus management and column
sorting were added; that is a 2.2× jump in one change and was taken as a
deliberate budget decision, not absorbed silently. It is still ~9% of uPlot and
~10% of Alpine, and §6's case against those was never about size.

### Measured sizes

Fetched and measured on 2026-07-25 (`curl … | wc -c`), not quoted from a bundle-size
site:

| Library | Raw | gzip | License |
|---|---:|---:|---|
| `uplot@1.6.32` `uPlot.iife.min.js` | 51 081 B | 21 991 B | MIT |
| `uplot@1.6.32` `uPlot.min.css` | 1 857 B | 758 B | MIT |
| `preact@10.26.9` `preact.min.js` | 11 195 B | 4 762 B | MIT |
| `htm@3.1.1` `htm.js` | 1 265 B | 678 B | Apache-2.0 |
| `alpinejs@3.14.9` `cdn.min.js` | 44 758 B | 16 171 B | MIT |
| **this design, inline** | **4 669 B** | ~1 700 B | — |

**Correction to the framing.** Gzipped size is the wrong metric here. The page is
opened over `file://` with no HTTP transport, so nothing decompresses it — the
browser parses raw bytes — and `include_str!` embeds raw bytes into the binary.
Gzip only matters if someone mails the `.html` and the mail client compresses it in
transit. **Judge on raw.**

### Why not uPlot (the strongest candidate)

1. It would be a **second chart implementation**. `chart.rs` already computes the
   geometry in Rust and `html.rs` already emits inline SVG from it
   (`svg_throughput`, `svg_burndown`, `ramp_stops`).
2. uPlot draws to `<canvas>` **at runtime**, so the charts stop being part of the
   document. `report --html | head`, a mail-client preview, text extraction, a
   print stylesheet and any archived copy all get an empty box. DESIGN.md §8 sells
   this file as *mailable, committable, air-gapped-friendly* — i.e. as a
   **document**, and a document's charts have to exist when nothing is executing.
3. Canvas output is not selectable, not searchable, and not themeable by the CSS
   custom properties the rest of the page uses.

### Why not Preact + htm (12 460 B raw)

It buys a component model and a VDOM diff for a page that is generated **once,
server-side, from immutable data, and never re-renders**. There is no state to
diff. Worse: Rust already owns templating and owns *the one escaper*
(`html::esc`, D19). Moving rendering into JS templates creates a **second escaping
path** — precisely the drift D19 exists to prevent.

### Why not Alpine (44 758 B raw)

21× the vanilla budget to express show/hide. Its directives live in HTML
attributes — the same strings that carry untrusted task titles — and an
attribute-directive framework sharing a surface with untrusted text is a
combination to avoid on principle rather than on measurement.

### The license point is sharper than it looks

All four are on `deny.toml`'s `allow` list (MIT, Apache-2.0). **But `deny.toml` is
`cargo-deny` config and only sees the Rust crate graph.** A vendored `.js` blob
behind `include_str!` is invisible to `cargo deny check licenses` forever. Vendoring
any of these means taking on a license obligation no CI gate can see, plus a manual
upgrade path with no `cargo update` and no advisory feed — for a project whose
`docs/dependency-policy.md` bans bare advisory ignores. That asymmetry is close to
decisive on its own.

### What vanilla has to do — the entire interaction budget

| # | Interaction | Mechanism | JS? |
|---|---|---|---|
| 1 | Toggle a bucket in the legend | flip `data-off` on `.page`; CSS attribute selectors hide `.seg-*` | ~15 lines |
| 2 | Click a project → filter timeline | set `data-filter` + a `.match` class; CSS does the rest | ~20 lines |
| 3 | Open a task detail | `:target` — the reveal itself is pure CSS | **none** (row 5 covers the focus move) |
| 4 | Per-bar tooltips | native SVG `<title>` | **none** |
| 5 | Focus the revealed drill-down panel | `hashchange` + `focus()` + a polite live region | ~20 lines |
| 6 | Sort the cost table in place | reorder `<tr>` by `data-*` integers | ~20 lines |

The column is summable: rows 3 and 5 are the *same* ~20 lines seen from two
sides — an earlier draft counted them twice and implied ~95 lines where the
script has ~75. Measured total: **4 669 B raw, 3 259 B comment-free.**

No build step, no minifier in the build, and the shipped script is readable in the
generated file — which matters for a document a reader may need to trust.

### Binary and page cost, plainly

Vanilla adds ~4.7 KB to the binary and ~4.7 KB to every page. uPlot would add
~53 KB, Alpine ~45 KB. Against a release binary in the megabytes those are small
in isolation — **the argument against them is not size.** It is the second chart
implementation, the second escaper, the license no gate can see, and the chart that
ceases to exist when JavaScript does not run.

---

## 7. Drift guards a future `html.rs` would need

Not written — this is design only — but named, with the failure each one catches.
Two are corrections to guards that exist.

1. **`report_is_self_contained` must become structural, not a substring scan.**
   The current test greps the whole document for `href=` / `<script` / `http://`.
   Once untrusted task text reaches the page that produces **false positives**:
   this prototype "failed" a naïve substring check purely because an annotation
   body quotes the string `https://`. The guard must parse the document and assert
   over **attribute values, `<style>` content and `<script>` content** — never over
   text nodes. Verified empirically while building the prototype.

2. **Every `href` is an in-page anchor** (`docs.rs`'s rule), replacing the blanket
   `href=` ban. Catches: a future contributor linking to a hosted asset.

3. **Exactly one inline `<script>`, containing no network or History API.**
   Assert absence of `fetch(`, `XMLHttpRequest`, `WebSocket`, `EventSource`,
   `importScripts`, `pushState`, `replaceState`, `eval(`, `new Function`. Catches
   the `file://` `SecurityError` regression DESIGN.md §8a already paid for once.

4. **No `url(` in CSS except in-document `url(#…)` refs; no `<link>`, no `@import`,
   no `src=`.** `html.rs` currently checks none of these three — `docs.rs` does.
   Catches: a background-image data URI or webfont creeping in.

5. **The four token buckets are never summed into a headline stat.** Assert the
   rendered **page** contains four distinct bucket labels *adjacent to the bars
   that decompose them* (they are no longer in the header), and that no element
   carries a blended token total. **Two stated exceptions, both `.bval`:** the
   cost table's per-row magnitude label beside its four-segment micro-bar
   (`prototype.py:969`), and the per-project one beside the volume bar in
   §tokens (`prototype.py:780`, reading e.g. `13.90M`). Each is a scale hint for
   a bar the reader is already looking at, scoped to that bar's own row, and in
   both cases the four buckets it labels are rendered separately right beside it.
   Neither is a headline. The guard as written would fail on both, so both are
   named here rather than discovered later — and the count matters: an earlier
   draft of this paragraph named only the cost table's, which is exactly the
   half-stated exception that gets a guard weakened by whoever hits it next.
   Catches the *current* defect: the core refuses to blend and the presentation
   layer blends anyway.

6. **Every `#task-N` link resolves to a `#task-N` panel in the same document —
   and its dual, `set(panel ids) == set(href ids)`.** This is now true *by
   construction*: `TaskRefs.link()` only emits an `href` for an id it
   simultaneously registers for a panel, and past the budget it degrades to inert
   `.nolink` text instead of dangling. So the guard is a cheap regression test
   rather than a design worry. The dual catches the other direction — the old
   24-panels/15-links waste coming back. **The enforcement rule, in prose:
   *every* `#task-N` emitter calls `refs.link()`.** A panel that links to a task
   any other way is the defect; that is exactly how the unattributed panel first
   shipped a set of hrefs the collector had never seen.

7. **The chart palette passes the colour checks for both schemes.** The four steps
   per scheme are constants; a unit test can assert OKLCH lightness band, chroma
   floor and adjacent-pair CVD ΔE without any rendering. Catches: someone
   "simplifying" the derived palette back to raw theme roles.

8. **Sparse and empty render as deliberate states.** Render the page against a
   fixture with zero measurements and assert each token panel emits its empty
   state, and that **§tokens** shows one `—` tile rather than four zeros. Assert
   too that §unattributed is *absent* when the count is zero — its empty state is
   the absence of the section.

9. **Every windowed panel states its window, and all of them state the SAME one.**
   Render at a known range and assert each of the throughput/token sections
   carries the range label, and that no two windowed panels carry different
   labels. Catches the defect this change fixes: header tiles at all time, a
   chart at 21 days, a timeline at 60 events, and nothing on the page saying so.

10. **A backlog metric is never read off a windowed summary.** Assert the header's
    `overdue` comes from a `report.summary` call whose filter carries no
    `completed.` predicate. Catches a confident `0 OVERDUE` produced by the
    structural fact that `reports.rs:142` guards overdue with `status.is_open()`
    while `filter.rs:335` makes a null `completed` fail every completion bound —
    the two are mutually exclusive, so a windowed summary always answers 0.

11. **Panel count is bounded independently of store size.** Render a 500-task
    fixture and assert the document holds at most `PANEL_BUDGET` `.detail`
    panels. Measured at the old budget of 120: 1 866 752 B / 500 panels →
    465 005 B / 112 panels. Re-measure at the current budget of 160.

---

## 8. Proposed decision entry

To be appended to DESIGN.md §12 in the existing style.

---

### D27 — the report page renders four token buckets, and earns one inline script to do it

**Decision:** Three parts, one rule.

**(a) The four token buckets are never blended on any output surface.** The page
renders `cache read`, `cache write`, `input` and `output` as four separate stats,
**adjacent to the bars that decompose them**; the per-project chart is a
four-segment stacked bar; the per-task table
carries a four-segment micro-bar. The single blended `stat("AI tokens")`
(`html.rs:273`) and the single blended `TOKENS` column in the terminal report are
deleted. Where a page needs to say which bucket matters, it renders a **weighted
dominance ratio** ("cache read drives ~68% of the cost") from the published
relative weights — never a currency figure, because tasqx has no price list and a
wrong one is worse than none.

**(b) `report_is_self_contained` is replaced by the `docs.rs` guard shape.** The
blanket `!contains("href=")` and `!contains("<script")` assertions go; in their
place: every `href` must be an in-page `#anchor`, exactly one inline `<script>`
containing no network API, no History API and no `eval`, plus the three bans
`html.rs` was missing entirely — `<link>`, `@import` and `url(` other than an
in-document `url(#…)`. The guard is applied **structurally**, over attribute values
and `<style>`/`<script>` content, never as a substring scan over the whole
document. That buys drill-down (`:target` task panels) and cross-widget filtering
(project row → timeline) while making the invariant *stricter* than it was.

**(c) The chart palette is derived from the theme, not taken from it.** Four
categorical steps per colour scheme, keeping each role's hue and re-stepping
lightness and chroma so both schemes pass an OKLCH lightness band, a chroma floor,
an adjacent-pair CVD floor and 3:1 contrast against their own surface. Stack order
is fixed as cyan → amber → purple → green so the deutan-confusable and
protan-confusable pairs are never adjacent. In dark mode the chart surface is
`--bg`, not `--card`. `urgency.ramp` stays a **sequential** ramp and is used only
for magnitude (the activity gradient), never as four categorical fills.

**(d) Every panel states its window, and the windowed ones share one range.** The
split is honest rather than uniform: **backlog is a state, throughput and tokens
are a window.** Open and overdue are as-of-now and cannot be windowed by
completion at all — `filter.rs::instant_cmp` returns false for a null
`completed`, so `completed.after:` excludes every open task by construction, and
`reports.rs:142` guards `overdue` with `status.is_open()`; the two conditions are
mutually exclusive, so a windowed summary answers `overdue: 0` structurally. So
each header tile is tagged `now` or `in range`, and every windowed panel prints
the same range label. **The range is a generation-time parameter, not an in-page
control.** Every product in §1 ships that control; a static `file://` document
cannot: re-querying needs `fetch` (banned, and dead on `file://`), re-aggregating
in the browser needs a second implementation of core's roll-up beside the Rust
one — the same "second implementation" objection that killed uPlot — and even
reflecting the choice in the URL needs `replaceState`, which throws
`SecurityError` there. What a static page *can* do is state its window
unmissably, print the literal filter clause so the reader can paste it into
`tasqx list` and reproduce the set, and make regenerating at another window one
flag. An unreadable range refuses and names the accepted spellings rather than
silently defaulting — a page whose stated window is not the window you asked for
is the exact class of defect this part exists to fix.

**No library is vendored.** The interaction budget is ~4.7 KB of inline vanilla JS.

**Why (a):** `engine/reports.rs:73` already carries the comment — "cache tokens
cost a fraction, so a blended total would lie" — and keeps `tokens_in`,
`tokens_out`, `tokens_cache_read` and `tokens_cache_creation` apart through the
entire aggregation, deriving `tokens_total` only at emit. Both presentation layers
then took that derived field and rendered it as **the** headline number, which
discards the exact care the core took. Measured on this project's own store during
the field test that produced this document: `in 136 · out 83 479 · cacheR 13 630 240
· cacheW 186 965`. The blended total is 13.9 M. Weighted by published relative
prices, cache read is **98.1 % of that volume but 67.7 % of the cost**, while
output is **0.6 % of the volume and 20.7 % of the cost**. One number cannot carry
a 35× spread in price per token, and the blend is wrong in the flattering direction — the same class of failure as
D18/D21/D23/D24: a number that drives a decision and does not mean what its label
says. The four buckets are safe to stack because they are genuinely disjoint:
Claude Code reports them as four separate counters, and `otlp.rs:449` subtracts
Codex's `cached_input_tokens` from its `input_tokens` precisely so the four-field
schema stays comparable across tools. **On not showing money:** every AI
observability product shows currency, and every one of them ships a maintained
price list to do it. tasqx has none, prices move per model generation, and a stale
multiplier renders a confident wrong number. The *relative* weights have been far
more stable than the absolute ones, which is why a dominance ratio is defensible
where a dollar figure is not.

**Why (b):** DESIGN.md §8/§8a define self-contained as inline `<style>`, inline
`<script>`, system fonts, and zero external requests — and `docs.rs` ships an
inline script and in-page anchors while satisfying it completely. `html.rs`'s test
encoded something else: a blanket ban on links and scripts, which is neither what
the invariant says nor sufficient to enforce it (it checks for `src=` but never
for `<link>`, `@import` or `url(`, any of which fetches). So the guard was
simultaneously too strict to allow drill-down and too loose to prevent a webfont.
Replacing it with the `docs.rs` shape fixes both ends. Making it structural fixes a
third problem found while building the prototype: a substring scan over the whole
document produces **false positives** the moment untrusted task text mentions
`https://` — the page fails its own self-containment test because of an
annotation's contents. A guard that fails on innocent user data will be weakened by
whoever hits it next, and a weakened guard is worse than a correct one. **On the
History API:** DESIGN.md §8a already records that its state-pushing methods throw
`SecurityError` on a `file://` document, discovered the expensive way. Here,
`:target` CSS does the **reveal**; a small focus handler (~20 lines) moves the
caret into the revealed panel and announces it, because no browser focuses a
fragment target and a keyboard user is otherwise left tabbing the page behind an
open panel. The History API stays untouched — assigning `location.hash` is a
navigation, not History state — and the guard asserts that, so the lesson cannot
be re-learned.

**Why (c):** themes are terminal palettes. Fed straight into SVG fills on the
report's light surface, `nord`'s roles fail four of five colour checks — worst
adjacent pair `#ebcb8b`↔`#a3be8c` at ΔE 10.9 against a floor of 15, every step
below the chroma floor, two outside the lightness band, all four under 3:1
contrast. The current report already does this, so this is a pre-existing defect
the redesign surfaces rather than one it introduces. The fix keeps the theme
semantic — each bucket still *means* a role, and a user theme still drives the
page — while accepting that a colour tuned for a dark 24-bit terminal is not a
colour tuned for a white card. Stack order is treated as a correctness property
rather than a preference because purple↔cyan and amber↔green are the two pairs
that collapse under the common dichromacies; ordering them apart is free, whereas
discovering the collision after release costs a re-palette. **On the dark
surface:** the dark lightness band (L 0.48–0.67) and a 3:1 contrast floor are not
jointly satisfiable against the current `--card`; they are against `--bg`. Drawing
charts on the page background rather than the card is the cheaper of the two fixes
and leaves the rest of the card styling alone.

---

## 8a. What only opening the page caught

The prototype passed fifteen structural checks — self-containment, escaping, link
resolution, both colour schemes — before anyone looked at it. Opening it in a
browser then found five defects in ten minutes, none of which any of those checks
could have expressed. Two more of the same shape have been added since. Recorded here because it is an argument about *method*, and
because a future `html.rs` port will hit the same class of thing.

| Found | Why no check caught it |
|---|---|
| The cost-table micro-bar collapsed to **14px** — its four segments ~2px each. `td.tok` is a flex row and `.btrack.mini` had no flex basis. | Every element was present, correctly classed and correctly coloured. Only its computed width was wrong. |
| The cost-share bar rendered **in the name column**. `.btrack:nth-of-type(2)` counts among siblings of the same *element type*, and every cell in the row is a `<span>` — so it selected the second span, not the second track. | Valid CSS, valid selector, wrong set. |
| The sticky header measures **114px**; `scroll-margin-top: 5rem` (80px) left every drill-down target's own heading clipped underneath it. **Stale twice over since:** the header has gone 114px → 181px (eight tiles) → ~81px (four tiles with a sub-line), and each move silently re-broke or over-corrected the clearance. The fix is a single `--hh` custom property — hardcoding a measured height in three places is *how* it went stale — and it must be **re-measured in a browser after any header change**, which is why it is a variable and not a fourth guess. | The anchor resolved, the panel displayed. It was just under something. |
| Header stats sat at x≈1375 on a 2055px viewport while the content column ended at 1417 — full-bleed header against a centred `92ch` main. | Both are legitimate layouts. Only together are they wrong. |
| One bar could not carry the volume-vs-cost argument at all (above). | A design failure, not an implementation one. |
| The confidence badge showed **HIGH** for a row containing a `low` measurement. `sorted({'high','low','medium'})[0]` is `'high'` — over this exact vocabulary alphabetical order reliably selects the *strongest* value. | Invisible to every structural check: the element was present, correctly classed and correctly coloured — for the wrong value. Only a human who knew the row's inputs could see it. |
| The range band placed **inside** the `position: sticky` header: two wrapped rows of italic caveat at 420px, ~120-150px of sticky chrome against an 88px `--hh`. It silently re-broke the scroll-margin the same file had already fixed twice. | Every rule was valid and the anchor still resolved. The band is now emitted as the first child of `.page`, outside the sticky element. |

**The `--hh` measurement, on the record.** Taken in Chrome against the shipped
fixture on 2026-07-25, which is the whole point of making it a variable:

| viewport | measured header | `--hh` | clearance |
|---|---:|---:|---:|
| 1576px | 82.45px | 5.5rem = 88px | 7.55px |
| 700px (stats do not wrap) | 80.45px | 5.5rem = 88px | 7.55px |
| 500px (`≤600px` branch) | 73.28px | 5rem = 80px | 6.72px |

The values the spec carried in were `6.5rem` / `8rem` — safe, and never clipped,
but the mobile one threw away **55px of a 726px phone screen** on every
drill-down. "Safe over-estimate" is not the same as measured, and a variable
whose value is still a guess has only moved the guess to one place.

Two of these — the `nth-of-type` selector and the flex-basis collapse — are the
kind of thing a screenshot test would pin cheaply. That is worth considering
alongside the guards in §7, though a pixel baseline for a themed page with five
built-ins and two colour schemes is ten baselines, not one, and is probably worth
it only for the two chart panels.

---

## 9. Regenerating the prototype

```bash
# full page, 30-day window (the default), from the live store
python3 docs/reporting-redesign-prototype.py > docs/reporting-redesign-prototype.html

# a different window — the range is a GENERATION-TIME parameter, not an
# in-page control (see §8 D27(d))
python3 docs/reporting-redesign-prototype.py --range 7d  > weekly.html
python3 docs/reporting-redesign-prototype.py --range all > alltime.html

# the empty / sparse variant — filter and range compose
python3 docs/reporting-redesign-prototype.py 'project:finly-next' --range 30d \
  > docs/reporting-redesign-prototype-empty.html
```

`--range` accepts `Nd`, `Nw` or `all` (`-30d` is a synonym for `30d`, because
that is the shape of the `completed.after:-30d` clause the page prints). An
unreadable value is refused with the accepted spellings, never silently
defaulted.

The generator shells out to `tasqx api` for every panel — **five reads**:
`report.summary` ×2 (one unwindowed for the backlog, one windowed for the flow —
see §5 D-2), `store.export`, `task.list` (the tracked join, §5 D-1) and
`event.list`. It used to issue six: a `@working` `task.list` fed a panel that was
never built, so it was one subprocess per page for nothing and has been deleted.
Five reads is the same claim DESIGN.md §8 makes about the real generator: *"the
report generator is just another client — anything it shows, a plugin or the MCP
server could compute the same way."*

It deliberately mirrors `html.rs`'s structure so the port is mechanical:

| `html.rs` | prototype |
|---|---|
| `generate()` | `gather()` |
| `Report::render()` | `render()` |
| `Report::css()` | `css()` |
| `esc()` (D19) | `esc()` — same rule: strip C0/C1 keeping tab+newline, then escape the five |
| `svg_*()` | `svg_activity()` |
| — | `Range` / `parse_range()` — the one window, ported as a struct |
| — | `derive_counts()` — every count the page states, each tagged NOW or RANGE |
| — | `TaskRefs` — the `#task-N` collector that bounds the panel set |

---

## Sources

**Time-management / task reporting**

- [Toggl Track — Summary Report](https://support.toggl.com/en-us/article/summary-report-1emjk2m/) · [Detailed report](https://support.toggl.com/en/articles/2216289-detailed-report) · [Analyzing time and reporting](https://support.toggl.com/en/collections/1148692-analyzing-time-and-reporting)
- [Clockify — Dashboard](https://clockify.me/help/reports/dashboard)
- [Harvest — Reports and analysis](https://www.getharvest.com/features/reports-and-analysis)
- [Atlassian — Agile burndown chart tutorial](https://www.atlassian.com/agile/tutorials/burndown-charts) · [View and understand the burndown chart](https://support.atlassian.com/jira-software-cloud/docs/view-and-understand-the-burndown-chart/)
- [Jira Cumulative Flow Diagram](https://community.atlassian.com/forums/App-Central-articles/Jira-Cumulative-Flow-Diagram-X-ray-your-workflow-with-deeper/ba-p/3136345) · [Jira burndown: what it does and where it has limits](https://community.atlassian.com/forums/App-Central-articles/Jira-burndown-chart-What-it-does-where-it-has-limits-and-how-to/ba-p/3201866)
- [The Ultimate Guide to Jira Metrics for Agile Teams](https://axify.io/blog/jira-metrics)
- Bach, Freeman, Abdul-Rahman, Turkay, Khan, Fan, Chen — [*Dashboard Design Patterns*](https://arxiv.org/abs/2205.00757), IEEE VIS 2022 / TVCG 2023 (systematic review of 144 dashboards, eight pattern groups)
- [Dashboard design best practices for product teams](https://figr.design/blog/dashboard-design-best-practices) · [Why less data often leads to better decisions](https://www.sigmacomputing.com/blog/data-analysis-less-more)

**AI token-usage reporting**

- [Anthropic — Usage and Cost API](https://platform.claude.com/docs/en/manage-claude/usage-cost-api) (four buckets; `1m`/`1h`/`1d`; usage and cost as separate endpoints)
- [Anthropic — cost and usage reporting in Console](https://support.anthropic.com/en/articles/9534590-cost-and-usage-reporting-in-console)
- [Langfuse — Token & cost tracking](https://langfuse.com/docs/observability/features/token-and-cost-tracking) (mutually-exclusive bucket contract)
- [LangSmith — Cost tracking](https://docs.langchain.com/langsmith/cost-tracking)
- [Helicone vs Langfuse vs LangSmith, 2026](https://particula.tech/blog/helicone-vs-langfuse-vs-langsmith-llm-observability)
- [Claude API pricing, cache read/write multipliers](https://www.cloudzero.com/blog/claude-api-pricing/)

**Library sizes** — measured directly from `unpkg.com` on 2026-07-25 with
`curl -sL <url> | wc -c` and `| gzip -9 | wc -c`; see the table in §6.
