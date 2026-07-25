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

Measured from this session's own store:

```
tasqx-reporting-redesign | in 136 | out 83 479 | cacheR 13 630 240 | cacheW 186 965 | total 13 900 820
```

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

- Date-range control with presets, top-left, applying to the whole page.
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
│ tasqx review    open · done/7d · tracked · overdue          │
│                 cache read · cache write · input · output   │  ← four, never one
└─────────────────────────────────────────────────────────────┘
  §activity   Completions, last 21 days          (inline SVG, urgency.ramp)
  §tokens     Token burn by project              (stacked bars + legend)
  §cost       Cost per task                      (table, two currencies)
  §timeline   Started / stopped / completed      (event log, filterable)
  ─────────────────────────────────────────────────────────────
  #task-N     detail panels                      (:target, hidden until linked)
```

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
- **Mark:** eight stat tiles, mono tabular numerals. No plot.
- **Theme roles:** `overdue` (→ `--danger`) on the overdue tile when non-zero.
- **Empty state:** the four token tiles collapse to a single `—` tile labelled
  *"tokens · not measured yet"*. Four zeros would imply four measurements of zero.
- **Interaction:** none. It is the 5-second read.
- **Replaces:** the current single blended `stat("AI tokens")` at `html.rs:273`.

#### Widget: Completions, last 21 days
- **Question it answers:** is work still closing, and when did it stop?
- **Data source:** `event.list` → `op == "done"`, bucketed by calendar day.
- **Mark:** inline SVG column chart, 4px rounded tops, 2px baseline stub on empty
  days so a gap reads as *measured zero* rather than *missing*.
- **Theme roles:** `urgency.ramp` as a real `<linearGradient id="ramp">`, matching
  `html.rs::ramp_stops`.
- **Empty state:** *"Nothing completed in the last 21 days"* + *"The bars appear as
  `tasqx done` events land."*
- **Interaction:** `<title>` per bar (native SVG tooltip, zero JS).

#### Widget: Token burn by project
- **Question it answers:** where did this range's tokens actually go, and which
  bucket is driving the bill?
- **Data source:** `report.summary` `group_by=project` → the four `tokens_*`
  metrics.
- **Mark:** horizontal stacked bar, one row per project, four segments, 2px surface
  gap between segments, 4px rounded data-ends anchored to the baseline. Row width
  scaled to the largest project so cross-project magnitude is comparable.
- **Theme roles:** the four derived bucket steps above; `project` for the row label.
- **Empty state:** *"No token data in this range."* + *"Token accounting is opt-in:
  `tasqx config set tokens.enabled true`, then run `tasqx daemon` so the
  attribution thread can reconstruct spend after each completion."* — the second
  clause matters, see §7.
- **Interaction:** click a row → filters the timeline below to that project;
  clicking the active row clears. Legend chips toggle a bucket across every bar at
  once.
- **Sub-label:** *"cache read drives ~68% of the cost"* — the weighted-dominance
  sentence from §3.

#### Widget: Cost per task
- **Question it answers:** what did this task cost me, in both currencies?
- **Data source:** `store.export` → `tokens[]`; **plus** `task.list` → `tracked`
  (see the API delta in §5 — no single read has both).
- **Mark:** table; mono tabular time column; a four-segment micro-bar per row
  scaled to the heaviest task; a confidence badge.
- **Theme roles:** the four bucket steps; `timer.active` (→ `--ok`) for a `high`
  confidence badge.
- **Empty state:** *"No measured work yet."* + *"A task is measurable once it has a
  timer interval or an attributed measurement."*
- **Interaction:** the id cell is `href="#task-N"` → opens that task's detail panel.
- **Why confidence is on the row and not in a footnote:** `high` means the
  transcript was parsed *and* the session id was verified against it
  (`attribution.rs:171`); `low` means a directory scan guessed. Those are different
  numbers and must not look alike.

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
- **Mark:** one panel per task, `display:none` until `:target` selects it. Metadata
  as a `<dl>`, the four buckets as a small grid, annotations as a timestamped list
  with `white-space: pre-wrap`.
- **Theme roles:** `accent` for the panel border and the id.
- **Empty state:** *"No token measurement. A task is only attributed when its
  `done` event carries correlation (client / session_id / transcript_path)."*
- **Interaction:** pure `:target` CSS, **zero JS**. Reached from any `#task-N`
  link; closed by a link back to `#cost`. The History API is untouched because its
  state-pushing methods throw `SecurityError` on a `file://` document
  (DESIGN.md §8a).

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

**Recommendation: hand-rolled vanilla JS.** ~2.1 KB inline, no vendored library.
The generated prototype's script block measures **2 129 bytes**.

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
| **this design, inline** | **2 129 B** | ~950 B | — |

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
| 3 | Open a task detail | `:target` | **none** |
| 4 | Per-bar tooltips | native SVG `<title>` | **none** |

No build step, no minifier in the build, and the shipped script is readable in the
generated file — which matters for a document a reader may need to trust.

### Binary and page cost, plainly

Vanilla adds ~2.1 KB to the binary and ~2.1 KB to every page. uPlot would add
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
   rendered header contains four distinct bucket labels and that no element
   carries a blended token total. Catches the *current* defect: the core refuses
   to blend and the presentation layer blends anyway.

6. **Every `#task-N` link resolves to a `#task-N` panel in the same document.**
   Catches a filtered export where the link survives but the panel was scoped out.

7. **The chart palette passes the colour checks for both schemes.** The four steps
   per scheme are constants; a unit test can assert OKLCH lightness band, chroma
   floor and adjacent-pair CVD ΔE without any rendering. Catches: someone
   "simplifying" the derived palette back to raw theme roles.

8. **Sparse and empty render as deliberate states.** Render the page against a
   fixture with zero measurements and assert each token panel emits its empty
   state, and that the header shows one `—` tile rather than four zeros.

---

## 8. Proposed decision entry

To be appended to DESIGN.md §12 in the existing style.

---

### D27 — the report page renders four token buckets, and earns one inline script to do it

**Decision:** Three parts, one rule.

**(a) The four token buckets are never blended on any output surface.** The
sticky header renders `cache read`, `cache write`, `input` and `output` as four
stats; the per-project chart is a four-segment stacked bar; the per-task table
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

**No library is vendored.** The interaction budget is ~2.1 KB of inline vanilla JS.

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
`SecurityError` on a `file://` document, discovered the expensive way; the drill-down
here is `:target` CSS with no JS at all, and the guard asserts the API stays
untouched so that lesson cannot be re-learned.

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

## 9. Regenerating the prototype

```bash
# full page, from the live store
python3 docs/reporting-redesign-prototype.py > docs/reporting-redesign-prototype.html

# the empty / sparse variant — any filter that selects tasks with no token history
python3 docs/reporting-redesign-prototype.py 'project:finly-next' \
  > docs/reporting-redesign-prototype-empty.html
```

The generator shells out to `tasqx api` for every panel — `report.summary`,
`store.export`, `task.list` ×2, `event.list` — which is the same claim DESIGN.md §8
makes about the real generator: *"the report generator is just another client —
anything it shows, a plugin or the MCP server could compute the same way."*

It deliberately mirrors `html.rs`'s structure so the port is mechanical:

| `html.rs` | prototype |
|---|---|
| `generate()` | `gather()` |
| `Report::render()` | `render()` |
| `Report::css()` | `css()` |
| `esc()` (D19) | `esc()` — same rule: strip C0/C1 keeping tab+newline, then escape the five |
| `svg_*()` | `svg_activity()` |

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
