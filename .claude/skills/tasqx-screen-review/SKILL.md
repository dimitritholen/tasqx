---
name: tasqx-screen-review
description: Render tasqx screens (CLI output, full-screen TUI views, the HTML report, the generated docs site) as PNGs and judge them by looking, then file each finding as a tasqx task with evidence. Use it when the user asks to review how the screens look, to screenshot or "use freeze on" tasqx output, to check a layout at 80/100/140 columns or a mobile width, or after a change to terminal output, the report or the docs. Wraps `scripts/snap.sh`, `scripts/docs-capture.sh` and `scripts/snap-web.mjs`. Sighted on task #11 (found #10-#15 by rendering the demo store); written on the user's ruling under task #60 without waiting for a second sighting.
---

# Review tasqx screens as images

Structural tests cannot see weight, spacing or contrast
(`docs/maintainers/terminal-style.md` §13). Render, then look. Run from the
repo root (or the task worktree), one plain command per call.

## Guard rules

- A dev binary (`target/debug/tasqx`) never touches the real store:
  `TASQX_DB=$PWD/target/scratch.db` inline on every command that names it,
  and `--no-daemon` on every direct `tasqx` call. `.claude/guard.sh` refuses
  the call otherwise, and the scripts do not relax it.
- Build with `cargo build -p tasqx-cli` and do not name the binary path in
  that command, or the guard demands the env there too.
- A narrow viewport comes from `scripts/snap-web.mjs` (CDP), never
  `--window-size`: headless Chrome on macOS floors that at 500px, so
  "390px" is a cropped 500px layout and proves nothing.

## Steps

1. **Build:** `cargo build -p tasqx-cli`.

2. **Terminal screens.** A screen is a row of
   `crates/tasqx-cli/docs-fixtures/manifest.tsv` (pipe, write-echo and
   full-screen `tui` rows alike). If the screen moved, recapture first; it
   needs tmux and builds the invented store itself, under a pinned clock:

   ```console
   $ TASQX_DB=$PWD/target/scratch.db TASQX=target/debug/tasqx scripts/docs-capture.sh --no-daemon
   ```

   Then one PNG per screen (`scale` defaults to 2):

   ```console
   $ TASQX_DB=$PWD/target/scratch.db TASQX=target/debug/tasqx scripts/snap.sh <name> [scale]
   ```

   Output is `target/snaps/<name>@<scale>x.png` (`OUT` moves it; `CHROME`
   names a browser, found on its own on macOS). Widths and key sequences are
   manifest columns, not flags: to judge another width or a TUI state, add a
   row (`list-narrow` is `list` at 80) and recapture. Check 80, 100 and 140.

3. **HTML report** (no fixture behind it): build the demo store, then render
   under the demo config, pinned and in UTC:

   ```console
   $ TASQX="$PWD/target/debug/tasqx" TASQX_DB="$PWD/target/demo/tasks.db" python3 scripts/demo-store.py
   $ TASQX_DB=$PWD/target/demo/tasks.db TASQX_CONFIG_DIR=$PWD/target/demo/config TZ=UTC TASQX_NOW=2026-09-16T09:00:00Z target/debug/tasqx --no-daemon report --html --out target/report.html
   ```

   Rasterise `target/report.html` with headless Chrome
   (`--headless --hide-scrollbars --force-device-scale-factor=2
   --window-size=1200,760 --screenshot=<png> file://…`) or, for a mobile
   width, open it through the same CDP approach as step 4.

4. **Docs site, every page, two widths, two themes:**

   ```console
   $ TASQX_DB=$PWD/target/scratch.db target/debug/tasqx --no-daemon docs --stdout > target/site.html
   $ node scripts/snap-web.mjs target/site.html target/snaps/site [anchor,anchor,...]
   ```

   Anchors shoot only those in-page ids (the tail of a long page). Read
   `target/snaps/site/report.json` for overflow and console errors.

5. **Look at every PNG** with the Read tool. The renderer draws bold and
   SGR 39 correctly; for a colour question read the raw bytes
   (`TASQX_FORCE_COLOR=1 … | LC_ALL=C cat -v`). Pass `THEME=mono` or
   `NO_COLOR=1` for the mono pass on a live render.

6. **Before filing, check the ruling.** Search `DESIGN.md` §12 (and
   `tasqx_search_memory`): UTC display was deliberate (D53). Confirm the
   defect in plain output, then file one task per finding with the PNG path,
   width and screen name in its first annotation.

## Verifiable end

Each screen in scope has a PNG under `target/snaps/` that you opened, and
each finding is a tasqx task with that evidence; nothing was written to the
real store.

## Why this order

- **Sighting 1 (task #11):** a session rendered the demo store and found
  #10-#15 that tests could not. The original recipe used `freeze` and a
  `google-chrome` shim on PATH; `freeze` ignored SGR 39 and drew no bold
  (D149), so rendering moved to `docs --screen` + `snap.sh`, which finds
  macOS Chrome itself, and `snap-tui.sh` was retired into `docs-capture.sh`.
- **Task #60:** captured as a skill candidate on 2026-09-13 and written on
  the user's ruling on 2026-10-08. Recapture before snapping, because a
  picture is made from the fixture, not from the live store.
- **Task #645:** the 390px-via-`--window-size` screenshot that was really
  500px wide is why step 4 uses CDP.
