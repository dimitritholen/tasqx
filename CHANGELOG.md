# Changelog

What changed in each tasqx release, newest first. Every release also lists its
commits on the [releases page](https://github.com/dimitritholen/tasqx/releases),
where the binaries, checksums and installers are.

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
