#!/usr/bin/env bash
#
# Rasterise ONE captured screen into a PNG: the README's pictures, and the way
# to judge a screen as an image (docs/maintainers/terminal-style.md §13-14).
#
#   scripts/snap.sh <name> [scale]
#
#   scripts/snap.sh list                 # → target/snaps/list@2x.png
#   scripts/snap.sh dashboard
#   TASQX_DB=$PWD/target/scratch.db TASQX=./target/debug/tasqx scripts/snap.sh pick 3
#
# Env:
#   TASQX   binary to render with (default: the tasqx on PATH)
#   CHROME  headless browser (default: google-chrome, chromium, or the macOS
#           Google Chrome.app — whichever is found first)
#   OUT     output directory (default: target/snaps, gitignored)
#
# <name> is a row of crates/tasqx-cli/docs-fixtures/manifest.tsv; an unknown one
# is refused by the binary, which lists them.
#
# The loop is one renderer, not two: `scripts/docs-capture.sh` captures the
# screen once as the ANSI the binary really printed, `tasqx docs --screen`
# renders that fixture into a standalone page with the documentation site's own
# terminal styling, and Chrome rasterises the page. So a picture cannot disagree
# with the site (DESIGN.md D149) — which the previous `freeze` path did, twice:
# it ignored SGR 39 and drew bold at normal weight.
#
# It opens no store and no daemon: a fixture is compiled into the binary. The
# repo's dev-build guard still wants `TASQX_DB=<scratch>/tasks.db` on the
# command line when TASQX names a dev build, and that is all it is for here.
set -euo pipefail

case "${1:-}" in
'' | -*)
    sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
if [ "$#" -gt 2 ]; then
    sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

name=$1
scale=${2:-2}

root=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT:-$root/target/snaps}
mkdir -p "$out"
# Absolute, because Chrome is handed a file:// URL built from it and a relative
# path there is ERR_INVALID_URL, which it screenshots as a picture of an error.
out=$(cd "$out" && pwd)
png="$out/$name@${scale}x.png"

chrome=${CHROME:-}
if [ -z "$chrome" ]; then
    # `command -v` answers for an absolute path too, so the macOS app bundle —
    # which is never on PATH — is found by the same loop as the Linux binaries.
    for candidate in \
        google-chrome \
        google-chrome-stable \
        chromium \
        chromium-browser \
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"; do
        if command -v "$candidate" >/dev/null 2>&1; then
            chrome=$candidate
            break
        fi
    done
fi
if [ -z "$chrome" ]; then
    echo "snap.sh: no headless Chrome found; set CHROME to one" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

page=$work/page.html
"${TASQX:-tasqx}" docs --screen "$name" --out "$page" >/dev/null

# Chrome does the work and then does not leave: on macOS a headless run writes
# its screenshot (or its DOM dump) in well under a second and then sits there
# forever, so a foreground call never returns — with `--timeout`, with
# `--virtual-time-budget` and with `--headless=old` alike. So it runs in the
# background, and the file it was asked for is the signal: once that file has
# stopped growing it is complete, and the browser has nothing left to do.
chrome_run() {
    local result=$1 stdout=$2
    shift 2
    rm -f "$result"
    "$chrome" --headless --disable-gpu --no-sandbox --hide-scrollbars \
        --user-data-dir="$work/profile" "$@" >"$stdout" 2>/dev/null &
    local pid=$! size=-1 prev=-2 waited=0
    while [ "$waited" -lt 150 ]; do # 30s, then give up
        sleep 0.2
        waited=$((waited + 1))
        prev=$size
        # Guarded rather than `wc -c <"$result" 2>/dev/null`: the file does not
        # exist for the first poll or two, and a failed REDIRECTION is the
        # shell's own error, which that stderr redirect does not cover.
        if [ -f "$result" ]; then
            size=$(wc -c <"$result" | tr -d '[:space:]')
        else
            size=-1
        fi
        if [ "$size" -gt 0 ] && [ "$size" -eq "$prev" ]; then
            break
        fi
        # A browser that died has nothing left to write; stop waiting for it.
        kill -0 "$pid" 2>/dev/null || break
    done
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if [ ! -s "$result" ]; then
        echo "snap.sh: $chrome wrote no $result for '$name'" >&2
        exit 1
    fi
}

# What the picture must be exactly as wide and as tall as. Chrome will not grow
# its window to fit the page, so a guess that is too small crops the right-hand
# columns — the ones a width check is about — and one that is too large frames
# the screen in empty background. The page sizes its body to its content
# (`docs::screen_page`), so the body's own box IS the answer: a one-line script
# appended to a COPY of the page reports it through the title, which
# `--dump-dom` prints. The page `tasqx docs --screen` writes stays script-free.
probe=$work/probe.html
cat "$page" >"$probe"
printf '<script>document.title=document.body.offsetWidth+"x"+document.body.offsetHeight;</script>\n' >>"$probe"
dom=$work/dom.html
# A small window on purpose: `offsetWidth` is the content's, but only while the
# viewport is not wider than it.
chrome_run "$dom" "$dom" --window-size=400,400 --dump-dom "file://$probe"

box=$(sed -n 's:.*<title>\([0-9]*x[0-9]*\)</title>.*:\1:p' "$dom" | head -1)
if [ -z "$box" ]; then
    echo "snap.sh: could not measure the page (no size in the dumped title)" >&2
    exit 1
fi
w=${box%x*}
h=${box#*x}

# One pixel of slack each way: `offsetWidth` is an integer and the text it
# measures is not, so an exact window can shave the last column's stem.
chrome_run "$png" /dev/null \
    --force-device-scale-factor="$scale" \
    --window-size="$((w + 1)),$((h + 1))" \
    --screenshot="$png" "file://$page"

echo "$png"
