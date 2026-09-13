#!/usr/bin/env bash
#
# Render a full-screen tasqx screen (dashboard, pick, settings) to a PNG.
# The pty half of the loop in docs/maintainers/terminal-style.md §14.
#
#   scripts/snap-tui.sh <name> <cols> <rows> -- <tasqx args...>
#
#   scripts/snap-tui.sh dash 120 40 --
#   scripts/snap-tui.sh dash-narrow 80 24 --
#   scripts/snap-tui.sh pick 120 40 -- pick
#   KEYS="]j j" scripts/snap-tui.sh dash-scoped 120 40 --
#
# Env:
#   TASQX     binary to drive (default: the tasqx on PATH)
#   TASQX_DB  store to read (MANDATORY for a dev build — see CLAUDE.md)
#   TASQX_CONFIG_DIR  config to read, passed into the pane like TASQX_DB (the
#             pane's shell does not inherit this one's environment)
#   THEME     passed through as --theme
#   KEYS      space-separated keys to send before capturing, e.g. "] j"
#   SETTLE    seconds to wait for the first paint (default 2)
#   OUT       output directory (default: target/snaps)
#
# These screens enter the alternate screen and draw with cursor addressing, so
# the pipe-into-freeze path cannot reach them: freeze renders a stream, it does
# not emulate a terminal. tmux does emulate one, so we let it hold the screen
# and ask it what is on there — `capture-pane -e` returns the cells WITH their
# SGR, which is exactly the ANSI freeze wants.
set -euo pipefail

if [ "$#" -lt 4 ] || [ "$4" != "--" ]; then
    sed -n '3,22p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

name=$1
cols=$2
rows=$3
shift 4

# Refuse rather than render from the real store. An empty TASQX_DB reached the
# pane as `TASQX_DB=''`, which the binary reads as "use the default store", and
# a live daemon answers from its own store whatever TASQX_DB says. `pick` STARTS
# a task on `s`, so KEYS=s against the real store would have written to it.
if [ -z "${TASQX_DB:-}" ]; then
    echo "snap-tui.sh: TASQX_DB must name a scratch store (see CLAUDE.md)" >&2
    exit 2
fi
case " $* " in
    *" --no-daemon "*) ;;
    *) set -- --no-daemon "$@" ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/target/snaps}
mkdir -p "$out"
# Absolute, because Chrome is handed a file:// URL built from it and a relative
# path there is ERR_INVALID_URL, which it screenshots as a picture of an error.
out=$(cd "$out" && pwd)
ansi=$(mktemp)
svg="$out/$name-${cols}x${rows}.svg"
png="$out/$name-${cols}x${rows}.png"
session="tasqx-snap-$$"

cleanup() {
    tmux kill-session -t "$session" 2>/dev/null || true
    rm -f "$ansi"
}
trap cleanup EXIT

# `-x`/`-y` size the pane rather than the outer terminal, which is what the
# screen reads. Without them a detached session gets tmux's 80x24 default and
# every width test measures the same screen.
tmux new-session -d -s "$session" -x "$cols" -y "$rows" \
    "TASQX_DB='${TASQX_DB:-}' ${TASQX_CONFIG_DIR:+TASQX_CONFIG_DIR='$TASQX_CONFIG_DIR'} ${TASQX:-tasqx} ${THEME:+--theme $THEME} $* ; sleep 300"

sleep "${SETTLE:-2}"

for key in ${KEYS:-}; do
    tmux send-keys -t "$session" "$key"
    sleep 0.4
done

tmux capture-pane -p -e -t "$session" >"$ansi"

# freeze ignores SGR 39 ("default foreground"), so a cell that resets to the
# terminal's own colour kept whatever colour came before it: the dashboard's
# titles rendered in the ramp colour of the figure beside them, which no real
# terminal draws. Spelled out as freeze's own default foreground instead, so
# the picture shows what the bytes say.
sed -i -e 's/\x1b\[39m/\x1b[38;2;196;196;196m/g' "$ansi"

# Trailing blank rows are the pane's, not the screen's: a chart that fills 18
# of 40 rows would otherwise render with 22 rows of empty PNG under it.
sed -i -e :a -e '/^[[:space:]]*$/{$d;N;ba' -e '}' "$ansi"

# `--language ansi`: see snap.sh. Without it an unpainted screen gets no picture.
freeze --language ansi --output "$svg" --window=false --padding 14 <"$ansi" >/dev/null

w=$(grep -om1 'width="[0-9.]*"' "$svg" | grep -o '[0-9.]*' | cut -d. -f1)
h=$(grep -om1 'height="[0-9.]*"' "$svg" | grep -o '[0-9.]*' | cut -d. -f1)
google-chrome --headless --disable-gpu --no-sandbox --hide-scrollbars \
    --force-device-scale-factor=2 --window-size="$((w + 2)),$((h + 2))" \
    --screenshot="$png" "file://$svg" >/dev/null 2>&1

rm -f "$svg"
echo "$png"
