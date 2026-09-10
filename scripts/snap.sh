#!/usr/bin/env bash
#
# Render a tasqx screen to a PNG so it can be judged as an image.
# The loop behind docs/terminal-style.md §13-14.
#
#   scripts/snap.sh <name> <width> -- <tasqx args...>
#
#   scripts/snap.sh list 100 -- list
#   scripts/snap.sh narrow 80 -- list "+design"
#   THEME=mono scripts/snap.sh mono 100 -- list
#   TASQX=./target/debug/tasqx scripts/snap.sh dev 100 -- list
#
# Env:
#   TASQX     binary to drive (default: the tasqx on PATH)
#   TASQX_DB  store to read (MANDATORY for a dev build — see CLAUDE.md)
#   THEME     passed through as --theme
#   OUT       output directory (default: target/snaps, gitignored)
#
# Why not `freeze --execute`: freeze v0.2.2 rasterizes SVG through a WASM
# build of resvg that segfaults under WSL2, taking --execute and every PNG
# output with it. So we drive the binary ourselves, hand freeze the ANSI on
# stdin, ask for SVG, and rasterize with headless Chrome.
set -euo pipefail

if [ "$#" -lt 3 ] || [ "$3" != "--" ]; then
    sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

name=$1
width=$2
shift 3

root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/target/snaps}
mkdir -p "$out"
svg="$out/$name-w$width.svg"
png="$out/$name-w$width.png"

for tool in freeze google-chrome; do
    command -v "$tool" >/dev/null || {
        echo "snap.sh: $tool is not on PATH" >&2
        exit 1
    }
done

# COLUMNS is read by theme::detect_cols; TASQX_FORCE_COLOR makes the binary
# paint even though its stdout is a pipe. Both have to agree with the --width
# freeze lays out for, or the render and the layout assume different terminals.
COLUMNS="$width" \
TASQX_FORCE_COLOR=1 \
TERM=xterm-256color \
    "${TASQX:-tasqx}" ${THEME:+--theme "$THEME"} "$@" 2>&1 |
    freeze --output "$svg" --window=false --padding 14 >/dev/null

# Sized from the SVG's own attributes: Chrome will not grow its viewport to
# fit, so a window smaller than the drawing silently crops the right-hand
# columns — which are exactly the ones a width test is about.
w=$(grep -om1 'width="[0-9.]*"' "$svg" | grep -o '[0-9.]*' | cut -d. -f1)
h=$(grep -om1 'height="[0-9.]*"' "$svg" | grep -o '[0-9.]*' | cut -d. -f1)

google-chrome --headless --disable-gpu --no-sandbox --hide-scrollbars \
    --force-device-scale-factor=2 --window-size="$((w + 2)),$((h + 2))" \
    --screenshot="$png" "file://$svg" >/dev/null 2>&1

rm -f "$svg"
echo "$png"
