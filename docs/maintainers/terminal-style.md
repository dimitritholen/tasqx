# The terminal house style

How a tasqx screen is laid out, and why. `DESIGN.md` §12 **D117** is the ruling
that settled these; this file is the working reference you read before touching
a screen, so the next one does not re-invent a look.

Every rule below came out of judging a rendered screen as an *image* at 80, 100
and 140 columns. None of them were visible to a fully green test suite, and two
of them — the gauge's resolution and its inverted ranking — could not have been
found any other way. §14 is the loop that makes that cheap.

`list`, `agenda`, the dashboard, the memory browser, `memory search`,
`pick`, `show`, `next`, `why`, `projects`, `report`, `theme list`,
`theme show`, `tasqx manual`, `tasqx about` and the write echoes (`add` and
every verb that changes something, D126) carry the style. `config list` and `tokens recompute`
do not yet.
The manual, the one screen that is mostly prose, adds conventions of its own
(a prose measure, two heading levels, copied code never split); they are
written down in `crates/tasqx-cli/src/manual.rs`'s module doc, and named by
`DESIGN.md` §12 **D123**, which is the ruling that carried the style to it.

---

## 1. One thing per row is the thing being read

The title carries the row. Everything else is context and must recede: dim the
project, the tags and the dates, keep the title at the terminal's own
foreground. A row where six cells print at the same brightness gives the eye no
path, which is what the old table did.

## 2. Spend cells on meaning

A column's width should track how much a reader gets from it. Twenty cells for
`2026-09-11T00:00:00Z` starved the title of the same twenty, on the one column
the row is actually read for. Before widening anything, ask what the column
would say in half the space.

Every table is fitted by one function, `columns::fit` (`DESIGN.md` D120), and
gives way in one order: cells come off the widest column above its floor, then
droppable columns go from the right. A table only decides what each column asks
for, where its floor is, and whether it may go. Three rules come with it:

- **A number never gives way.** Cut, it is a different number; dropped, it may
  be the one the reader asked for by name. The key column shrinks instead, and
  past its floor the row overflows.
- **Data is cut only where the terminal cannot hold it.** A wrapped row breaks
  every column at once, so a name or a value gets an ellipsis first, and the
  column that says where something came from goes before the data does.
- **A record is not a table.** Each record is fitted to itself. Its name
  stays, where it came from goes first, the handle that opens it never goes,
  and the line under it is cut to the width. `memory search` is the record
  (D125): title, source and handle on one line, the handle on the next where
  the two cannot share one, the words that matched under them.

After a drop the survivors get the freed cells back (D125), and `memory
list`'s title floor is what the terminal can give it beside the id, never
below twelve cells, so every other column goes before the title gives way.

## 3. Dates are calendar days — not instants, not elapsed hours

`today 23:59`, `tomorrow`, `yesterday`, `2d ago`, `Sun`, `17 Sep`, `4 Jan 27`.

Weekday alone inside the coming week (there is exactly one Sunday in any
six-day window); the date once the weekday stops being unambiguous; the year
only when it changes. A clock only where "when today" is still a live question
— today and tomorrow — and only when the store holds one, since a date typed
without a time resolves to 00:00 UTC and midnight is the store's spelling of
"no time given".

Calendar days rather than elapsed hours is the point: a deadline at 09:00
tomorrow is "tomorrow" to the person reading it, and `markdown::fmt_instant`'s
"in 14 hours" hands them the arithmetic the cell exists to do.

A clock is for what is still **ahead**. A moment that has already happened —
a completion, a note, an event — is a day and no clock: the reader who just
ran the command knows what time it is, and the clock a deadline carries is
UTC, so on a past moment it reads as the wall clock and is wrong by the
offset for everyone who is not on it (`done today 16:22`, rendered at 18:22
CEST). Where the interesting quantity is a span rather than an instant, print
the span (`for 12m`), which has no timezone to be wrong about.

`render::due_cell` is the implementation for what is ahead and `render::day_ago`
for what is past; `agenda`'s `day_heading` is the vocabulary both views share.
`DESIGN.md` §12 **D126 (l)** is the ruling behind the split.

## 4. State lives in a left rail, never in a droppable column

`▶` running, `⊘` blocked, two cells, at the far left where the eye crosses
first. Never dropped by the width fit. Not drawn at all when no row has one.
`pick` sizes it over every candidate rather than the rows a search leaves on
screen, so the rows do not shift sideways as a query crosses the one running
task; there the rail can stand empty (`DESIGN.md` D124(c)).

Without Unicode they are `*` and `B` — and NOT `>`, however natural `>` looks
for "running". `>` is the CURSOR on a terminal that has no better glyph: `pick`
and the dashboard both reserve it for the row the reader is on. A state marker
drawn as the cursor is two meanings on one character, on the exact terminal
that has no colour left to tell them apart.

The glyphs must differ in SHAPE as well as in role. `NO_COLOR` (§8 of
`DESIGN.md`, the degradation table) keeps emphasis and drops every hue, so a
rail that said "red bar or green bar" would say nothing at all to the reader
who most needs the terminal to behave.

This is what the old arrangement cost: the running timer — the single piece of
state a work block depends on — sat to the RIGHT of a title that can run 72
cells, in a column `TaskCols::fit` drops on a narrow terminal.

A table of choices has one state worth a rail: which one is in effect.
`projects` marks the default and `theme list` the active theme with `*`, the
way `git branch` marks the branch that is checked out. It replaced a
seven-cell DEFAULT column holding one `*`, and nine cells of `← active`. The
same glyph is `list`'s running marker without Unicode, which is the
two-meanings case this rule argues against — and the dashboard does draw
both at once: `*` for a running task in TASKS, `*` for the default project
in PROJECTS. `DESIGN.md` D125(d) rules that it stands, and says why: they
are two panels and two columns, never the same column of one row, which is
the collision the rail exists to prevent.

## 5. A modifier belongs in the cell it modifies

Priority went inside the urgency cell rather than keeping a column of its own
plus a gap on either side to describe a number two columns away. The cell reads
`H ▄▄▄▄ 17.9`: letter, gauge, figure.

## 6. A magnitude gets a mark, and the mark must rank the way the number does

`render::urgency_meter` draws urgency as a four-cell bar on a `▁` track. Three
things it had to get right, all found by looking at it rather than by reasoning
about it:

- **Resolution where the ranking happens.** Whole cells drew 17.9 and 15.8
  identically against a top of 17.9, the pair a reader compared hardest. Three
  steps inside each cell fixed it.
- **Visual mass must rise with the value.** Drawing the remainder as a glyph
  TALLER than the bar's own `▄` made a 22 % gauge the heaviest mark in the
  column. Remainders are shorter (`▂`, `▃`), and a test weighs the glyphs across
  the whole scale rather than reading a value off them.
- **The scale belongs to the task, not to the screen** (`DESIGN.md` D119). The
  bar is `render::urgency_scale`: full at the due term's saturation, which is
  "as urgent as an overdue task", and never relative to the other rows. The
  denominator used to be the hottest row on screen, so one outlier flattened a
  hundred rows, and a filtered view painted its top row in `danger` whatever
  that row held. The same task is the same mark on `list`, `agenda` and the
  dashboard. The bar floors rather than rounds, so it is never full before the
  task is as urgent as overdue.

The colour says less than the bar, on purpose. The ramp is read in BANDS, one
per anchor, never blended: quiet grey below H priority alone, `warn` from there,
`danger` from overdue, and bold on top of `danger` so the band survives
`NO_COLOR`. A blend from grey to `warn` passes through a tan that reads more
orange than `warn` itself, which drew M rows warmer than H rows. The bar carries
the magnitude, which leaves the colour to say only which threshold was crossed.
It is the heatmap's rule again: one channel per number.

The price is at the top. Everything at or past overdue draws the same full bar,
and the figure beside it tells them apart. The precise figure always prints. The
mark is for the scan down the column, not for reading a value off. Where no
glyph set degrades honestly, draw nothing and hand the cells back: `caps.unicode`
false drops the gauge.

## 7. No rules. Weight and whitespace separate

The table was bracketed by two full-width rules; both are gone. A dim
`table.label` header over rows that start immediately under it separates them
without drawing anything.

Where a group needs separating, use a blank line — one ahead of every heading
but the first, because flush against the group above a heading reads as one
more of its rows. Prose that follows a table (omission notes, store-health
warnings) gets a blank line too: with no closing rule, a note starting flush
against the last row reads as a row whose columns broke.

## 8. A count is the least useful summary available

The reader can count the rows. Say what they cannot see:

- which question was asked — the filter for `list`, the horizon for `agenda`
- how much is late, due before the day is out, running, blocked

Print only the facts that are non-zero. A line that always says `0 overdue` is
one the reader learns to skip, and then it is not there on the day it matters.
Never `N task(s)`; `1 task` / `N tasks` (`render::plural_tasks`).

## 9. The summary must fit, and drops rather than truncates

It sits above the header, where a wrap would put a line between the labels and
the rows they name. Facts are dropped from the right until the line fits —
which is why they are built in falling order of what a reader loses by not
seeing them. Dropping says less; truncating mid-word says something else.

Every PRINTED line of facts fits by this one rule (`render::keep_ranked`,
D125(e)); the dashboard's status bar fits its own, over ratatui spans.
`next` and `add`'s echo print their facts in reading order but rank them by
what a reader loses (the urgency cell, the deadline, a running timer, then the
project and the tags). The first fact in rank that does not fit ends the line,
so nothing less important survives it.

## 10. Facts counted over the rows on screen say so

When the frame is bounded (`serve::bound_to_viewport`, `--limit`), name both
numbers: `44 tasks · 20 shown`. A fact counted over twenty rows must never read
as a claim about forty-four.

## 11. Never say the same thing twice on one screen

`agenda`'s day heading names the day, so a `WHEN` cell holding a bare `due`
under it says nothing — it is blank. Its `due today` fact is suppressed for the
same reason.

But `sched` still prints bare, because "you meant to start here" is not the
default reading of a row, and the overdue group keeps full dates, because it
spans many days and has no heading to defer to. The rule is *redundant with
what is already on screen*, not *short*.

## 12. A column label is not a title

`table.label` — achromatic in every built-in — carries column headers. The
`header` role is for titles (`TASQX MANUAL`, `tasqx settings`, a task's own
name). One role was painting both, and a column label that competes with its
own rows is structure refusing to recede.

D76's principle, stated for the task card and applying here unchanged:
structure recedes, only what you act on is emphasized.

## 13. Verify by rendering

Structural tests cannot see weight, spacing or contrast. Render the screen and
look at it — at 80 and 140 as well as at the default 100, and in `mono` and
under `NO_COLOR`.

Mock in pixels, not in prose. Both layout decisions behind D117 were made from
rendered images: three candidates were drawn before any Rust moved, and the
gauge that replaced the winner was picked the same way.

## 14. The loop

`freeze` v0.2.2 segfaults on WSL2 for any PNG output and for `--execute` — its
SVG-to-PNG step runs `resvg` through a WASM runtime that faults there. So:

1. Drive the binary yourself and pipe its ANSI into `freeze` on stdin, with
   `TASQX_FORCE_COLOR=1` and `COLUMNS` set to the width under test.
2. Ask `freeze` for `.svg`, not `.png`.
3. Rasterize with headless Chrome at `--force-device-scale-factor=2`, sized
   from the SVG's own `width`/`height` attributes.

`scripts/snap.sh` does all three:

```console
$ scripts/snap.sh list 100 -- list
$ scripts/snap.sh narrow 80 -- list "+design"
$ THEME=mono scripts/snap.sh mono 100 -- list
```

It writes into `target/snaps/`, so the pictures never reach a commit. Driving a
dev build needs `TASQX` pointed at it **and** a scratch `TASQX_DB` —
`CLAUDE.md`'s rule, which this script does not relax.

TUI screens (`dashboard`, `pick`) need a pty, which this path does not give.
`scripts/snap-tui.sh` holds them in tmux and captures the pane.

One trap in that path: freeze ignores SGR 39 (default foreground), so a cell
that resets to the terminal's own colour keeps whatever colour came before it.
The dashboard's titles rendered tinted in the ramp colour of the figure beside
them for exactly this reason, although the bytes carried `ESC[39m` and a real
terminal drew them plain. `snap-tui.sh` now rewrites SGR 39 to freeze's own
default foreground before rendering. If a colour still looks as if it bled,
read the ANSI (`tmux capture-pane -p -e | cat -v`) before filing it.

A second trap: freeze draws SGR 1 at normal weight (its SVG says
`font-weight: normal`), so nothing is bold in its pictures. Where bold carries
meaning — on the write echoes it means "this write changed it", and under
`NO_COLOR` it is the only emphasis left — judge the screen from an HTML render
of the same ANSI instead, rasterized the same way.

---

## Contract

The strings this document quotes, so a change to one of them reddens
`the_house_style_doc_still_describes_the_screens` in `render.rs` rather than
leaving a guide describing a screen that no longer exists.

| Thing | Value |
|---|---|
| rail, running | `▶` (`*` without Unicode) |
| rail, blocked | `⊘` (`B` without Unicode) |
| rail, the one in effect | `*` (`projects`, `theme list`) |
| gauge, full cell | `▄` |
| gauge, track | `▁` |
| gauge, remainder | `▂` `▃` |
| gauge width | 4 cells |
| gauge full at | urgency 12 (`urgency::DUE_WEIGHT`) |
| ramp bands, built-ins | from 0 · from 6 · from 12 |
| column-header role | `table.label` |

## Where it is not carried yet

- `config list` and `tokens recompute` — not judged as images. `config list`
  fits the width (D120) and takes `table.label`; `tokens recompute` still
  counts `N task(s)`.
- The dashboard carries it. The disagreement its spec had with rule 4 (`⛔` for
  blocked where this file says `⊘`) is settled in favour of `⊘`, which is what
  `list`, `agenda` and TASKS all now draw.
