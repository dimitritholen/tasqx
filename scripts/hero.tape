# The README's hero GIF: capture a task, ask what's next, open the dashboard.
#
#   vhs scripts/hero.tape            # from the repo root → docs/img/hero.gif
#
# Needs vhs 0.11.0 with ttyd and ffmpeg, and a `tasqx` on PATH built from this
# tree (`cargo install --path crates/tasqx-cli --force`). vhs 0.12.0 prints
# "Creating docs/img/hero.gif...", exits 0 and writes nothing
# (charmbracelet/vhs#787); take 0.11.0 from its GitHub release until that ships.
#
# It records only the invented demo store: the hidden prelude rebuilds
# target/demo/tasks.db under the same pinned clock as scripts/docs-capture.sh,
# points every variable tasqx reads at it, and wraps `tasqx` so each call passes
# --no-daemon — a reachable daemon would answer from the real store instead.

Output docs/img/hero.gif

Set Shell bash
Set FontSize 14
Set Width 1200
Set Height 720
Set Padding 16
Set Framerate 12
Set TypingSpeed 45ms
Set Theme "Catppuccin Mocha"

Hide
Type "export TASQX_DB=$PWD/target/demo/tasks.db TASQX_CONFIG_DIR=$PWD/target/demo/config TASQX_NOW=2026-09-16T09:00:00Z TZ=UTC COLORTERM=truecolor"
Enter
Type "unset NO_COLOR CLICOLOR CLICOLOR_FORCE TASQX_THEME TASQX_SOCK TASQX_DASHBOARD"
Enter
Type "python3 scripts/demo-store.py >/dev/null"
Enter
Type `tasqx() { command tasqx --no-daemon "$@"; }`
Enter
Type "set +H; PS1='$ '; clear"
Enter
Sleep 2s
Show

Type 'tasqx add "Record the launch demo" project:website due:friday +docs !H'
Sleep 300ms
Enter
Sleep 1500ms

Type "tasqx next"
Sleep 300ms
Enter
Sleep 3s

Type "tasqx"
Sleep 300ms
Enter
# The recording ends on the dashboard, not on `q`: a GIF loops, and its last
# frame is the one a reader sits on.
Sleep 5s
