#!/usr/bin/env bash
#
# Capture every screen the documentation site shows, as the ANSI the binary
# really printed, into crates/tasqx-cli/docs-fixtures/.
#
#   TASQX=./target/debug/tasqx scripts/docs-capture.sh            # regenerate
#   TASQX=./target/debug/tasqx scripts/docs-capture.sh --check    # CI: diff only
#
# Env:
#   TASQX     binary to drive. MANDATORY — see "Why TASQX is required" below.
#   SETTLE    seconds to wait for a full-screen row's first paint (default 2)
#
# Flags:
#   --check       capture into a temp dir and diff against the committed
#                 fixtures; exit 1 naming every row that differs.
#   --no-daemon   accepted and ignored. Every invocation below already passes
#                 it; the flag exists so the CLAUDE.md dev-build rule can be
#                 satisfied by the command line that calls this script.
#
# What it is
#
# One row of docs-fixtures/manifest.tsv is one screen. The clock is pinned to a
# single Wednesday, the store is rebuilt under that pin, and every row is
# rendered at that instant — so a capture is the same bytes on any calendar day
# and a diff means the SCREEN changed (DESIGN.md D149). `tasqx docs` embeds the
# result with `include_str!` and renders it through `ansi_html`, which is why
# these files are committed rather than built: a build of the guide must not
# need tmux, a demo store, or a second copy of the binary.
#
# Why TASQX is required
#
# The other capture scripts default to the `tasqx` on PATH, which on a
# maintainer's machine is their real, installed build. Here that default would
# be actively wrong twice over: the fixtures are meant to show THIS tree's
# screens, and the mutating rows below run `add`, `done`, `start` and
# `annotate`. So the binary is named explicitly or nothing runs.
set -euo pipefail

# Every fixture is a picture of this Wednesday: 2026-09-16, 09:00 UTC. The
# store's history, the dates the screens spell and the stamps the write echoes
# produce all come from it (crates/tasqx-cli/src/clock.rs). Changing it
# regenerates every fixture, which is a whole-tree diff — do it deliberately.
PIN=2026-09-16T09:00:00Z

check=0
for arg in "$@"; do
    case "$arg" in
    --check) check=1 ;;
    --no-daemon) ;;
    -h | --help)
        sed -n '3,36p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "docs-capture.sh: unknown argument $arg" >&2
        exit 2
        ;;
    esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
fixtures=$root/crates/tasqx-cli/docs-fixtures
manifest=$fixtures/manifest.tsv
demo_db=$root/target/demo/tasks.db
demo_config=$root/target/demo/config

if [ -z "${TASQX:-}" ]; then
    echo "docs-capture.sh: set TASQX to the binary to capture, e.g." >&2
    echo "  cargo build -p tasqx-cli" >&2
    echo "  TASQX=target/debug/tasqx scripts/docs-capture.sh" >&2
    exit 2
fi
command -v "$TASQX" >/dev/null 2>&1 || [ -x "$TASQX" ] || {
    echo "docs-capture.sh: TASQX=$TASQX is not an executable" >&2
    exit 2
}
# Absolute from here on. `TASQX=target/debug/tasqx` (what CI passes, and what
# anyone types from the repo root) is relative to THIS shell's directory, and a
# tmux pane does not necessarily start in it.
case "$TASQX" in
*/*) TASQX=$(cd "$(dirname "$TASQX")" && pwd)/$(basename "$TASQX") ;;
esac
# `-e` (a variable straight into the session environment, no shell in between)
# arrived in tmux 3.2. The older `printf %q` path snap-tui.sh keeps is not
# carried here: this script runs on CI and on maintainer machines, both of
# which have 3.2 or newer, and one quoting path is one place to get it wrong.
command -v tmux >/dev/null || {
    echo "docs-capture.sh: tmux is not on PATH (the full-screen rows need a pty)" >&2
    exit 1
}
tmux_ver=$(tmux -V | sed 's/[^0-9.]*//g')
tmux_major=${tmux_ver%%.*}
tmux_minor=${tmux_ver#*.}
tmux_minor=${tmux_minor%%.*}
if [ "${tmux_major:-0}" -lt 3 ] ||
    { [ "${tmux_major:-0}" -eq 3 ] && [ "${tmux_minor:-0}" -lt 2 ]; }; then
    echo "docs-capture.sh: tmux $tmux_ver is too old; 3.2+ is needed for \`-e\`" >&2
    exit 1
fi

work=$(mktemp -d)
# A tmux server of our own, on its own socket. A pane inherits the SERVER's
# environment, not this shell's, so a maintainer's already-running tmux would
# have handed every full-screen row whatever that server was started with —
# `COLORTERM`, `NO_COLOR`, `TASQX_THEME` — and the capture would then differ
# between a machine with a tmux open and a runner without one. It also keeps
# this script from appearing in anybody's session list. The server is started
# with `-f /dev/null` for the same reason one layer down: `~/.tmux.conf` is read
# even on a private socket, and a maintainer's `default-terminal`, status line or
# pane border would be part of what the capture records.
tmux_sock=tasqx-docs-$$
cleanup() {
    tmux -L "$tmux_sock" kill-server 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

# The environment every row is rendered in, and nothing else. `-u` matters as
# much as the assignments: `NO_COLOR` would strip every colour, `CLICOLOR_FORCE`
# is read beside TASQX_FORCE_COLOR, and `TASQX_THEME` would render the screens
# in whatever theme the capturing machine prefers.
#
# COLORTERM is the one that bites silently, and it is why this function exists.
# The depth detection reads it first and falls back to `TERM=…256color`, so a
# maintainer's terminal (COLORTERM=truecolor) captures `38;2;r;g;b` where a CI
# runner captures `38;5;n` — the same screen, every byte different, and the drift
# job red for everyone except whoever captured last. Truecolor is what these
# screens are designed in, so it is stated here rather than inherited.
clean_env() {
    env -u NO_COLOR -u CLICOLOR -u CLICOLOR_FORCE -u TASQX_THEME -u TASQX_SOCK \
        COLORTERM=truecolor "$@"
}

if [ "$check" -eq 1 ]; then
    out=$work/new
    mkdir -p "$out"
else
    out=$fixtures
    mkdir -p "$out"
fi

# The store is rebuilt from scratch on every run, under the pin: a fixture has
# to be reproducible from the repository alone, and a store left over from an
# earlier run has whatever the mutating rows of that run did to it.
echo "docs-capture: rebuilding the demo store at $PIN" >&2
TASQX="$TASQX" TASQX_NOW="$PIN" python3 "$root/scripts/demo-store.py" >/dev/null

# A copy of the store for one mutating row. `add`, `done`, `start` and
# `annotate` write, and a row that wrote into the shared store would change
# every row captured after it — the screens would then depend on the order of
# the manifest, which is the one thing a fixture must not do.
copy_store() {
    local dest=$1
    cp "$demo_db" "$dest"
    # The write-ahead log and the shared-memory file are part of the store's
    # state: copying tasks.db alone hands the row a snapshot that predates
    # whatever demo-store.py last committed.
    for suffix in -wal -shm; do
        if [ -f "$demo_db$suffix" ]; then
            cp "$demo_db$suffix" "$dest$suffix"
        fi
    done
}

# A full-screen row: tmux holds a real pty, `capture-pane -e` gives back the
# cells WITH their SGR. The pane's shell inherits nothing from here, so every
# variable the screen reads is handed over with `-e`, never interpolated into
# shell source (one apostrophe in a value used to be executed there).
#
# The pane command holds the screen open with a trailing `sleep`, and that is
# exactly what makes a FAILURE invisible: `tasqx dashboard --bogus-flag` exits
# 2 in a tenth of a second, the sleep keeps the pane alive anyway, and what gets
# captured and committed is a picture of clap's error message. So the command's
# exit status is written to a file the moment it exits, and a row whose status
# file exists by capture time is a failed row — the same refusal a piped row
# gets — rather than a fixture. A screen that is still running has written
# nothing, which is the whole signal.
capture_tui() {
    local name=$1 cols=$2 rows=$3 keys=$4 db=$5 dest=$6
    shift 6
    local session="docs-$name"
    local status_file="$work/$name.status"
    rm -f "$status_file"
    local pane_cmd
    pane_cmd=$(printf '%q ' "$TASQX" --no-daemon "$@")
    # `\$?` is the PANE's shell reading its own exit status, not this one's.
    pane_cmd="$pane_cmd; echo \$? >$(printf '%q' "$status_file"); sleep 300"
    clean_env tmux -f /dev/null -L "$tmux_sock" new-session -d -s "$session" -x "$cols" -y "$rows" \
        -e "TASQX_DB=$db" \
        -e "TASQX_CONFIG_DIR=$demo_config" \
        -e "TASQX_NOW=$PIN" \
        -e "TASQX_FORCE_COLOR=1" \
        -e "COLORTERM=truecolor" \
        -e "COLUMNS=$cols" \
        "$pane_cmd"
    sleep "${SETTLE:-2}"
    for key in $keys; do
        tmux -L "$tmux_sock" send-keys -t "$session" "$key"
        sleep 0.4
    done
    # Checked here, after the keys and immediately before the capture, so a
    # screen that dies ON a keystroke is caught too and a slow first paint is
    # not mistaken for a death.
    if [ -f "$status_file" ]; then
        local code
        code=$(tr -d '[:space:]' <"$status_file")
        echo "docs-capture: row '$name' exited ($code) instead of holding a" >&2
        echo "screen, so the pane below is an error, not a fixture:" >&2
        tmux -L "$tmux_sock" capture-pane -p -t "$session" 2>/dev/null |
            sed 's/^/  /' | sed '/^ *$/d' >&2 || true
        tmux -L "$tmux_sock" kill-session -t "$session" 2>/dev/null || true
        return 1
    fi
    tmux -L "$tmux_sock" capture-pane -p -e -t "$session" >"$work/pane"
    tmux -L "$tmux_sock" kill-session -t "$session" 2>/dev/null || true
    # Trailing blank rows belong to the PANE, not to the screen: a `pick` list
    # of twelve rows in a forty-row pane would otherwise carry twenty-eight
    # empty lines into the page.
    awk '{ buf[NR] = $0 }
         END { last = 0
               for (i = 1; i <= NR; i++) if (buf[i] ~ /[^[:space:]]/) last = i
               for (i = 1; i <= last; i++) print buf[i] }' "$work/pane" >"$dest"
}

# A piped row: stdout AND stderr, because a warning line is part of the screen
# (`add` prints one when it declines a sugar-shaped word, and the site should
# show what a reader would actually see).
capture_pipe() {
    local cols=$1 db=$2 stdin=$3 dest=$4
    shift 4
    local status=0
    if [ "$stdin" = "-" ]; then
        clean_env TASQX_DB="$db" TASQX_CONFIG_DIR="$demo_config" TASQX_NOW="$PIN" \
            TASQX_FORCE_COLOR=1 TERM=xterm-256color COLUMNS="$cols" \
            "$TASQX" --no-daemon "$@" >"$dest" 2>&1 </dev/null || status=$?
    else
        printf '%s' "$stdin" |
            clean_env TASQX_DB="$db" TASQX_CONFIG_DIR="$demo_config" TASQX_NOW="$PIN" \
                TASQX_FORCE_COLOR=1 TERM=xterm-256color COLUMNS="$cols" \
                "$TASQX" --no-daemon "$@" >"$dest" 2>&1 || status=$?
    fi
    return $status
}

names=()
while IFS=$'\t' read -r name kind cols rows args keys stdin <&3 || [ -n "${name:-}" ]; do
    case "$name" in '' | '#'*) continue ;; esac
    names+=("$name")
    dest=$out/$name.ansi
    db=$demo_db
    case "$kind" in
    *-mut)
        db=$work/$name.db
        copy_store "$db"
        ;;
    esac
    # The manifest is repository content, read by a repository script, so its
    # argument column is split by the shell that will run it — which is the only
    # way `add "Draft the Q4 roadmap" project:website` reaches the binary as
    # three words with the quotes doing their job. A hand-rolled splitter would
    # be a second, worse quoting dialect to learn.
    eval "set -- $args"
    case "$kind" in
    pipe | pipe-mut)
        if ! capture_pipe "$cols" "$db" "$stdin" "$dest" "$@"; then
            echo "docs-capture: row '$name' exited nonzero; its output was:" >&2
            cat "$dest" >&2
            exit 1
        fi
        ;;
    tui)
        if [ "$keys" = "-" ]; then
            keys=""
        fi
        # Refused the same way a piped row that exits nonzero is: a screen that
        # did not stay on the screen has no fixture worth writing.
        if ! capture_tui "$name" "$cols" "$rows" "$keys" "$db" "$dest" "$@"; then
            exit 1
        fi
        ;;
    *)
        echo "docs-capture: row '$name' has unknown kind '$kind'" >&2
        exit 2
        ;;
    esac
    echo "docs-capture: $name" >&2
done 3<"$manifest"

if [ "${#names[@]}" -eq 0 ]; then
    echo "docs-capture: $manifest describes no rows" >&2
    exit 2
fi

# A fixture that quotes this machine cannot be reproduced on another one, so
# the drift job would go red on a runner for a capture that was perfectly
# correct here. `about` was dropped from the manifest for exactly this (it
# prints the store path and the build's commit); this catches the next one.
leaks=$(grep -l -F -e "$root" -e "$HOME" "$out"/*.ansi 2>/dev/null || true)
if [ -n "$leaks" ]; then
    echo "docs-capture: these fixtures carry a path from this machine, so no" >&2
    echo "other machine can reproduce them. Drop the row or narrow the screen:" >&2
    printf '%s\n' "$leaks" | sed 's/^/  /' >&2
    exit 1
fi

# The same rule, one step subtler: an FTS `rank` is bm25, computed through the
# platform's `log()`, and its last digits differ between macOS and Linux. A
# fixture carrying one is reproducible on the machine that captured it and
# nowhere else — which is how `api-task-brief` was found, by the drift job, on a
# runner. The SEARCH SCREENS are fine and stay: they print snippets, not the
# number. It is the JSON responses that quote it.
ranked=$(grep -l -F '"rank":' "$out"/*.ansi 2>/dev/null || true)
if [ -n "$ranked" ]; then
    echo "docs-capture: these fixtures quote an FTS bm25 rank, whose last digits" >&2
    echo "are the platform's libm. Drop the row, or capture a screen instead:" >&2
    printf '%s\n' "$ranked" | sed 's/^/  /' >&2
    exit 1
fi

if [ "$check" -eq 0 ]; then
    echo "docs-capture: wrote ${#names[@]} fixtures to $fixtures" >&2
    exit 0
fi

# --check: every captured row against its committed twin, both directions. An
# extra committed file is drift too — it is a screen the manifest stopped
# describing, which `fixtures.rs` would still embed.
differs=()
for name in "${names[@]}"; do
    if [ ! -f "$fixtures/$name.ansi" ]; then
        differs+=("$name (no committed fixture)")
    elif ! cmp -s "$out/$name.ansi" "$fixtures/$name.ansi"; then
        differs+=("$name")
    fi
done
for file in "$fixtures"/*.ansi; do
    [ -e "$file" ] || continue
    base=$(basename "$file" .ansi)
    listed=0
    for name in "${names[@]}"; do
        if [ "$name" = "$base" ]; then
            listed=1
            break
        fi
    done
    if [ "$listed" -eq 0 ]; then
        differs+=("$base (committed, but not in the manifest)")
    fi
done

if [ "${#differs[@]}" -eq 0 ]; then
    echo "docs-capture: ${#names[@]} fixtures match" >&2
    exit 0
fi

echo "docs-capture: the committed screens no longer match the binary:" >&2
printf '  %s\n' "${differs[@]}" >&2
echo >&2
echo "If the change was intended, regenerate them in the same commit:" >&2
echo "  cargo build -p tasqx-cli" >&2
echo "  TASQX=target/debug/tasqx scripts/docs-capture.sh" >&2
exit 1
