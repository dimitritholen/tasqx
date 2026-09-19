# Changelog

What changed in each tasqx release, newest first. Every release also lists its
commits on the [releases page](https://github.com/dimitritholen/tasqx/releases),
where the binaries, checksums and installers are.

## 0.11.0

This release is mostly about getting tasqx set up and understood. `tasqx setup`
installs the Claude Code integration in one screen, the API and MCP reference
pages are generated from the engine itself instead of hand-typed prose, and the
README is now a short pitch that links out to the wiki for everything else.
Four bugs are fixed, from a sort order that could favour an older task to a
memory snippet that read as two paragraphs.

### Added

- **`tasqx setup` installs the Claude Code integration in one screen.** It
  registers the MCP server (`claude mcp add --scope user`) and writes the
  bundled `tasqx-workflow` and `retro` skills to `~/.claude/skills`, shown as
  an interactive checklist on a terminal or a flat list with `--list`. A skill
  file that differs from the bundled one is kept unless you tick it or pass
  `--force`, and setup never touches `~/.claude.json` itself — it drives
  `claude mcp add`, because Claude Code owns that file.
- **A generated API and MCP reference.** The reference pages `tasqx docs` and
  the documentation site render now come from the engine's own parameter
  table, the MCP tool schemas and the documented response shapes, instead of
  prose that could drift from what the server actually does. Objects also
  gained their own pages — Task, Project, Annotation, Check, Dependency,
  Memory document, Event and Token measurement — each listing the fields that
  belong to it, exactly once, alongside the methods that read or write it.
- **The README is the pitch; the wiki is the manual.** It opens on why you'd
  want tasqx and links out to `docs/wiki` for install fine print, the MCP tool
  roster and everything else you only need once you're using it. It now opens
  on a VHS-recorded hero GIF showing the loop a user repeats — add a task with
  its project, due date, priority, tag and estimate, make a second task wait
  on it, then find the first again by fuzzy search down to its card — and
  several feature rows carry their own short GIFs.

### Changed

- **Agent guidance now recommends the task card only when a person is
  deciding on a task** — one being proposed, or one asked about by name.
  Starting or completing a task is one line instead
  (`▶ #<id> <title> · <priority> · <estimate> · <passed>/<total> checks` /
  `✔ #<id> done · <passed>/<total> checks · unblocked #<n>`), built from
  information the agent already holds. The card itself renders the same as
  before.

### Fixed

- **`task.list`/`tasqx list` sorted by created or modified time could put an
  older task above a newer one** when one timestamp landed on a whole second
  and the other didn't — the comparison ran on the timestamp text, where a
  trailing `Z` outranks a digit. Both sort orders now compare true time.
- **The dashboard's detail overlay showed "blocked by #N" in red for every
  dependency,** even when the task itself was done or its blocker was done or
  cancelled. It now shows only the blockers that are actually still open.
- **`tasqx brief` and `tasqx show` on a terminal packed acceptance checks into
  the two-column field grid**, crowding a short check beside a field like
  `rev`. Checks now print in their own block below the fields, one per line.
- **A memory hit's snippet in a task brief could split across a blank line
  and read as two paragraphs,** in the MCP/markdown and card views alike. It
  now prints on one line.
- **Every API method's guard against an unknown parameter key and a missing
  required one now has test coverage across the board,** closing gaps where
  25 of 42 methods went unchecked for an unknown key and only the first of
  several missing required keys was ever probed. The guards themselves were
  already correct; no method's accepted input changed.
- **The documentation site's tables no longer run off a phone screen,** and
  the install section on GitHub Pages offers the right installer for each OS
  and architecture instead of only Homebrew or a `cargo build` line.

## 0.10.0

This release is mostly about AI agents. A correction you give once is now
remembered: a memory doc can be marked standing, and every new MCP session
starts with that project's standing rulings. The tool list an agent pays for
on every prompt is about a quarter smaller, and a task can be shown as a card.
Two defaults changed in ways a script or an agent setup can notice, so read
the first section before upgrading.

### Changed: check these before upgrading

- **`tasqx done` refuses a task that still has open blockers.** It used to
  complete it silently, which left a dependent done ahead of the work it
  depended on. It now exits with `conflict` (exit 5) and names each open
  blocker. `--force` (MCP `force: true`) completes it anyway, the override is
  recorded on the event, and `tasqx report --outcomes` counts it under a new
  `forced` metric. A script that completes tasks out of dependency order needs
  `--force` or a reorder.
- **MCP reads answer less by default.** `tasqx_get_task` and `tasqx_brief_task`
  send the rendered view only, and `include_json: true` brings back the JSON
  block. `tasqx_list_tasks` sends nine fields per row with null keys left out,
  and `fields: []` returns the whole row. `tasqx_search_memory` and the brief
  leave out each hit's bm25 `rank`, and `include_rank: true` restores it.
  `tasqx_list_memory` pages twenty rows of `id`, `title`, `source` and
  `modified`, and `include_preview: true` restores the rest. The JSON API and
  the CLI still return every field, so this affects only agents that parsed
  the dropped ones.
- **The brief shows five memory hits instead of ten,** in the CLI, the JSON
  API and MCP alike. `--memory-limit` (`memory_limit`) still asks for more.

### Added

- **Standing memory.** `tasqx memory add --standing` marks a doc as a ruling
  for every session of its project, or of every project when it has none.
  `memory update --standing true|false` sets or clears the mark, and
  `memory list --standing` shows the marked docs. Over MCP, `tasqx_add_memory`,
  `tasqx_update_memory` and `tasqx_list_memory` take `standing`. An existing
  store gains the flag when it is opened, and exports carry it. Past fifteen
  standing docs in one project, `add` answers with a hint to merge or clear
  some.
- **New MCP sessions start with the project's standing rulings.** The
  `initialize` instructions now end with them. The project is taken from the
  folder `tasqx mcp serve` runs in, or the nearest parent folder named after a
  project, so a git worktree finds its repo. Failing that, the default project
  is used. Standing rulings come first and are never dropped. The project's
  newest other docs fill the rest of a 3 KB budget, under a separate heading
  that marks them as context rather than rules. With nothing to show, the
  instructions are exactly what 0.9.0 sent.
- **Task cards.** `tasqx show`, `brief`, `next` and `why` take `--card` and
  print a task as a fixed 72-column box, with `--ascii` for plain borders.
  `tasqx_get_task`, `tasqx_brief_task` and `tasqx_complete_task` take
  `view: "card"`, so an agent can hand a person the same box. A completion
  asked for the card answers with the closing card, which saves a second read.
- **Shorter tool descriptions.** Every MCP tool description now states only
  its contract. The whole tool list shrank from 42.7 KB to about 31 KB, which
  is roughly 3,000 fewer tokens on every prompt for clients that load all
  tools up front. The reasoning moved to DESIGN.md, where
  `tasqx_search_memory` finds it once the docs are imported.
- **The brief keeps room for rulings.** Half of the brief's memory hits are
  reserved for knowledge docs, so a project's many task notes can no longer
  bury the decisions that govern it.
- **A documentation site.** The guide `tasqx docs` opens offline is also
  published at https://dimitritholen.github.io/tasqx/ on every push to `main`.
  Its screens are real captured terminal output shown as text, and CI fails
  when a screen no longer matches what the binary prints.
- **`TASQX_NOW` pins the clock.** Set it to an RFC 3339 instant and every date
  tasqx prints or stores uses that instant. It exists for reproducible
  screenshots and tests, and `tasqx about` says when a pin is active. An
  invalid value exits 2 before anything is written.

### Fixed

- **A done task could still report unmet blockers,** so `task.get` answered
  `blocked: false` next to a non-empty blocker list. A closed task now has
  none, and every reader uses the same rule.
- **`tasqx_get_task` could exceed the MCP response budget** when the caller
  named its own page size. The budget now always holds. An oversized note is
  cut in the response with a marker, the store keeps it whole, and
  `max_body_bytes` raises the cap.
- **`memory list` could show an older doc above a newer one** when their
  timestamps had different lengths. It now orders by the actual instant.
- **Re-importing a markdown folder did not bump a doc's revision,** so an
  update holding the old revision could overwrite the import. The import now
  bumps it, and that update gets a conflict instead.
- **An MCP call with `arguments: null` skipped `tasqx_list_tasks`' defaults.**
- **`tasqx docs` could exit outside its JSON envelope** when the page could
  not be written.

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
