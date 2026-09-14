# Changelog

What changed in each tasqx release, newest first. Every release also lists its
commits on the [releases page](https://github.com/dimitritholen/tasqx/releases),
where the binaries, checksums and installers are.

## 0.9.0

One addition aimed at the first minute after `claude mcp add tasqx` — the
server now tells an agent *when* to use it, not only what it can call — and one
ordering defect that could hand any reader the wrong annotation.

### Added

- **`initialize` carries `instructions`.** Every MCP host that surfaces server
  instructions (Claude Code and Cursor put them in the agent's system prompt)
  now receives a condensed workflow with no setup: search memory before
  deciding, track multi-step work as tasks, brief, start, annotate and
  complete each one, store decisions with their reasons, import your markdown
  on day one, and always spell the verb. The text is built from the server's
  scope: under the default read-only scope it names no write tool, says once
  that the server is read-only, and tells the agent to put what it could not
  store into its reply instead of dropping it. A test binds every tool name in
  the text to the live roster, so a renamed tool cannot ship inside a system
  prompt that calls the old name. The paste-anywhere block in
  [Giving an agent memory in any client](docs/guides/agent-starter-prompt.md)
  remains the fuller version, for hosts that ignore or truncate the field.

### Fixed

- **Annotations could come back out of order, with a newer note paged out and
  an older one in its place.** The sort key was the stored timestamp text,
  whose fractional second is variable-length — trailing zeros trimmed, the
  fraction gone at a whole second — and compared as text an older stamp can
  sort above a newer one. Two notes written inside the same millisecond, which
  is what an agent annotating in a burst produces, were enough to trigger it.
  Ordering now follows the time-ordered id, so `show`, `brief`'s "what the
  prerequisite concluded", `task.get` paging and `store.export` all agree on
  which note is the newest. Token measurements, ordered the same way, got the
  same fix.

## 0.8.0

Five things an AI agent does with tasqx got cheaper, provable, or both — and
one of them was a bug. Nothing here changes how tasqx behaves for a person at a
terminal, apart from two new verbs and two new rows on `tasqx show`.

### Added

- **`tasqx report --outcomes`** answers the question `tasqx report` never did:
  not what the work cost, but whether it worked. Rework (completions that came
  back), estimate calibration, token cost, completions with no annotation,
  abandoned work, budget overruns and unproven completions. It reads history
  the store already holds, so it has something to say the first time you run
  it. Every rate prints as `count/n` rather than a percentage, deliberately:
  3/12 and 3/3 are the same percentage and very different news.
- **`tasqx brief <ref>`** is everything needed before starting a task, in one
  read: the task, each prerequisite with **what it concluded** (its newest
  annotation), what this task blocks, and relevant memory — under a query tasqx
  derives from the task's own title, tags and project. You do not supply search
  terms, which matters because a guessed term that finds nothing looks exactly
  like a store with nothing in it.
- **`tasqx check add|set|rm`** puts acceptance criteria somewhere a completion
  can be held to them. tasqx never RUNS a check: the criterion is a claim and
  the evidence is a citation, both stored verbatim. Completing with one still
  open is not refused — it is counted by `report --outcomes`.
- **`--budget-tokens`** on `add` and `modify`: a size gauge over *fresh* tokens
  (input, output and cache creation; cache reads are not counted). It stops
  nothing. An overrun is a signal that the task was probably too big to hand
  over whole.
- **Five MCP tools**: `tasqx_brief_task` and `tasqx_outcomes` on the read
  scope, `tasqx_add_check` / `tasqx_set_check` / `tasqx_remove_check` on write.
  `tasqx_complete_task` takes `checks_passed` and `evidence`.
- **`memory.search` gains `include_unscoped`**, which widens a project scope to
  documents belonging to no project — what `tasqx memory import` produces, so a
  strict scope used to hide every imported ADR.

### Fixed

- **A timer another session was holding is no longer stopped silently.** Two
  agents on one store, neither passing `--keep`: the second `start` stopped
  every active task, so the first agent's task left `active` while it was still
  working, its remaining time went untracked, and only the second agent was
  told. Starting against a clock a different session holds is now refused;
  `--keep` still runs both deliberately. A person at a shell sees no change.
- A dashboard test asserted a panel equalled itself and had stopped guarding
  what its name claimed.

## 0.7.0

The terminal was rebuilt around a single house style: every table, card and
chart now follows the same rules. This release also adds a task browser, a
memory browser and a much more useful HTML report.

### Changed

- **Every screen reads the same way.** `list` and `agenda`:
  - carry a state rail on the left and an urgency gauge beside each figure
  - spell dates as calendar days (`today 23:59`, `tomorrow`, `Sun`)
  - draw no table rules

  `projects`, `report`, `memory search`, `config list` and the theme screens now
  follow the same layout. Every table fits the terminal it is printed on, and no
  number is ever cut to make a row fit.
- **Write commands answer with a card.** `add` and the eighteen verbs that change
  a task print the task they touched, with what changed in bold. Output piped
  from a write verb is different from 0.6.
- **`show`, `next` and `why`** spell dates as calendar days, say each fact once,
  and explain themselves.
- **`pick` is a task browser.** It shows `list`'s rows, searches with `/`, opens a
  task's card on Enter and starts it with `s`. Closing it without starting
  anything exits 0.
- **The dashboard** is one scoped task list with PROJECTS, BURNDOWN, PULSE and
  EFFORT beside it. The burndown now draws a real shape.
- **`tasqx manual`** fits the terminal it is read on and opens every page its
  contents list.
- **Charts:** the heatmap no longer paints a good day in the danger colour, and
  the throughput chart labels its series properly.

### Added

- **`tasqx memory list`** on a terminal is a browser with a live preview and a
  search.
- **Memory search:**
  - words are stemmed, so `test` also finds `tests`
  - results report `total` and `has_more`
  - search can be scoped to a project
  - a stored document can be updated in place
- **HTML report (`tasqx report --html`):**
  - opens with what needs attention
  - header tiles jump to their sections
  - search, cross-filtering, a sortable table and per-task panels, all offline
  - light by default, with a dark switch
  - a totals row and human-readable durations
- **`tasqx about`**: who made tasqx, which build you are running, and where its
  store lives.
- **`list` flags:** `--sort`, `--limit`, `--offset` and `--fields`.
- **Filtering on `priority:H|M|L`.**
- **Correcting tracked time** with `modify`: set it or clear it, and the change is
  logged.
- **Dependencies:** a blocking task can now say which tasks it blocks.
- **`tasqx config describe`**, for settings whose unit a bare number does not
  show.
- **Escaping in titles:** `\!` and `\+` put a literal `!` or `+` in a title.
- **Tokens:**
  - `task.list` has a `tokens` field and a `-tokens` sort
  - a self-reported token count can be removed
  - `otlp.status` answers whether telemetry is reaching tasqx
  - the daemon announces a first-run attribution backlog instead of working
    through it silently
- **MCP and the JSON API:**
  - `tasqx_cancel_task`
  - `tasqx_remove_annotation`, which scrubs the annotation's body from storage
  - `task.modify` echoes the resolved value of every field it set
  - `task.add` echoes the title, due date, tags and scheduled date it stored
  - the annotation body echo can be switched off with `include_body: false`

### Fixed

More than a hundred fixes across the CLI, memory, reports, the dashboard, `pick`,
filters, themes, the daemon, MCP and token attribution. The most visible:

- `undo` could reverse the wrong event after an import.
- Several write commands reported values other than the ones they stored.
- Some screens drew clipped or blank rows at narrow widths.

The full list is in the commits on the release page.
