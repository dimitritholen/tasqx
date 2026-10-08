#!/usr/bin/env bash
# Release checks for a Tasqx Desktop bundle built on this host (#695): run
# after scripts/desktop-bundle.sh, here or in CI. It verifies, in order:
#
#   1. the workspace, the desktop app and (on a tag) the tag name agree on
#      one version
#   2. this host's bundles exist and carry that version; on macOS the .app
#      is sealed (codesign --verify)
#   3. the release smoke suite (apps/tasqx-desktop/src/test/integration):
#      the app's own code against a real daemon on a disposable store —
#      daemon connection, dashboard read, one mutation, a daemon restart and
#      reconnect, graph load, export/import, a migrated store
#   4. install the bundle the way a user would, launch it against a scratch
#      daemon, check it stays up and dials that daemon, quit it, uninstall
#      it, and check nothing it installed or created is left behind
#
# Data the launch creates under the user's home (WebKit storage, caches) is
# removed only if it did not exist before the run, so a maintainer's own
# install of the app is never touched. The daemon and the app run on a
# scratch store and socket, and are stopped by their own PIDs.
#
# It never tags, pushes or publishes.
#
# Usage:
#   scripts/desktop-release-check.sh [--no-suite] [--no-launch]
#     --no-suite   skip step 3 (CI ran the suite in `npm test` already)
#     --no-launch  skip step 4 (a host with no display)
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
APP_DIR=apps/tasqx-desktop
BUNDLE="$APP_DIR/src-tauri/target/release/bundle"

suite=true
launch=true
for arg in "$@"; do
    case "$arg" in
    --no-suite) suite=false ;;
    --no-launch) launch=false ;;
    *)
        echo "usage: $0 [--no-suite] [--no-launch]" >&2
        exit 2
        ;;
    esac
done

ok() { echo "ok    $*"; }
fail() {
    echo "FAIL  $*" >&2
    exit 1
}

case "$(uname -s)" in
Darwin) os=macos ;;
Linux) os=linux ;;
MINGW* | MSYS* | CYGWIN*) os=windows ;;
*) fail "unsupported host $(uname -s)" ;;
esac

# --- 1. one version ---------------------------------------------------------
first_version() { grep -m1 -E "$2" "$1" | sed -E 's/.*"([^"]+)".*/\1/'; }
workspace="$(first_version Cargo.toml '^version = "')"
for file in "$APP_DIR/package.json" "$APP_DIR/src-tauri/tauri.conf.json"; do
    v="$(first_version "$file" '^  "version": "')"
    [[ "$v" == "$workspace" ]] || fail "$file says $v, the workspace says $workspace (scripts/bump-version.sh --apply writes both)"
done
v="$(first_version "$APP_DIR/src-tauri/Cargo.toml" '^version = "')"
[[ "$v" == "$workspace" ]] || fail "$APP_DIR/src-tauri/Cargo.toml says $v, the workspace says $workspace"
if [[ "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
    [[ "${GITHUB_REF_NAME#v}" == "$workspace" ]] || fail "tag $GITHUB_REF_NAME, but the workspace says $workspace"
fi
ok "one version: $workspace"

# --- 2. bundles -------------------------------------------------------------
only() {
    # Exactly one file matches, or the check cannot say which one shipped.
    local matches=("$@")
    [[ ${#matches[@]} -eq 1 && -e "${matches[0]}" ]] || fail "expected one bundle, found: ${matches[*]}"
    echo "${matches[0]}"
}
case "$os" in
macos)
    app="$(only "$BUNDLE"/macos/*.app)"
    dmg="$(only "$BUNDLE"/dmg/*_"${workspace}"_*.dmg)"
    codesign --verify --deep --strict "$app" || fail "$app is not sealed"
    ok "bundles: $app (sealed), $dmg"
    ;;
linux)
    deb="$(only "$BUNDLE"/deb/*_"${workspace}"_*.deb)"
    rpm="$(only "$BUNDLE"/rpm/*-"${workspace}"-*.rpm)"
    appimage="$(only "$BUNDLE"/appimage/*_"${workspace}"_*.AppImage)"
    ok "bundles: $deb, $rpm, $appimage"
    ;;
windows)
    msi="$(only "$BUNDLE"/msi/*_"${workspace}"_*.msi)"
    nsis="$(only "$BUNDLE"/nsis/*_"${workspace}"_*-setup.exe)"
    ok "bundles: $msi, $nsis"
    ;;
esac

# --- 3. the smoke suite -----------------------------------------------------
if $suite; then
    (cd "$APP_DIR" && npx vitest --run src/test/integration) || fail "the release smoke suite"
    ok "release smoke suite"
fi

$launch || exit 0

# --- 4. install, launch, uninstall ------------------------------------------
# A short path: a Unix socket path is capped at 103 bytes.
scratch="$(mktemp -d "${TMPDIR:-/tmp}/tqx-rc-XXXX" 2>/dev/null || mktemp -d)"
tasqx="$ROOT/target/debug/tasqx"
[[ "$os" == windows ]] && tasqx="$tasqx.exe"
[[ -x "$tasqx" ]] || cargo build -p tasqx-cli --quiet
if [[ "$os" == windows ]]; then
    sock="tqx-rc-$$"
else
    sock="$scratch/d.sock"
fi
export TASQX_DB="$scratch/tasks.db" TASQX_SOCK="$sock"

daemon_pid=""
app_pid=""
xvfb_pid=""
data_dirs=()
existed=()
# What the launch created under the home directory, and only that.
remove_created_data() {
    local dir kept mine
    for dir in ${data_dirs[@]+"${data_dirs[@]}"}; do
        mine=true
        for kept in ${existed[@]+"${existed[@]}"}; do [[ "$kept" == "$dir" ]] && mine=false; done
        # The webview's helper processes can hold files open for a moment
        # after the app is gone (WebView2 on Windows), so retry briefly.
        if $mine; then
            for _ in 1 2 3 4 5 6 7 8 9 10; do
                rm -rf "$dir" 2>/dev/null && break
                sleep 1
            done
        fi
    done
}
# Stop the app by its own PID. On Windows that PID is Git Bash's view of the
# process; taskkill /T on the real one takes WebView2's helper processes with
# it, which a plain kill leaves holding the app's data directory open.
stop_app() {
    [[ -n "$app_pid" ]] || return 0
    if [[ "$os" == windows && -r "/proc/$app_pid/winpid" ]]; then
        taskkill //PID "$(cat "/proc/$app_pid/winpid")" //T //F >/dev/null 2>&1 || true
    fi
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
    app_pid=""
}
cleanup() {
    stop_app
    for pid in $xvfb_pid $daemon_pid; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
    remove_created_data
    rm -rf "$scratch"
}
trap cleanup EXIT

"$tasqx" --no-daemon add "release check" >/dev/null
"$tasqx" --no-daemon --socket "$sock" daemon --db "$TASQX_DB" 2>"$scratch/daemon.err" &
daemon_pid=$!
sleep 2
kill -0 "$daemon_pid" 2>/dev/null || fail "the scratch daemon did not start: $(cat "$scratch/daemon.err")"

case "$os" in
macos)
    data_dirs=("$HOME/Library/WebKit/dev.tasqx.desktop" "$HOME/Library/Caches/dev.tasqx.desktop"
        "$HOME/Library/Application Support/dev.tasqx.desktop" "$HOME/Library/HTTPStorages/dev.tasqx.desktop"
        "$HOME/Library/Saved Application State/dev.tasqx.desktop.savedState")
    ;;
linux) data_dirs=("$HOME/.local/share/dev.tasqx.desktop" "$HOME/.cache/dev.tasqx.desktop") ;;
windows) data_dirs=("${LOCALAPPDATA:-$HOME/AppData/Local}/dev.tasqx.desktop") ;;
esac
for dir in "${data_dirs[@]}"; do if [[ -e "$dir" ]]; then existed+=("$dir"); fi; done

# Install. Each branch sets `exe` (what to launch) and `installed` (what
# must be gone after the uninstall).
case "$os" in
macos)
    hdiutil attach -nobrowse -readonly -mountpoint "$scratch/mnt" "$dmg" >/dev/null
    mkdir "$scratch/Applications"
    ditto "$scratch/mnt/$(basename "$app")" "$scratch/Applications/$(basename "$app")"
    hdiutil detach "$scratch/mnt" >/dev/null
    installed="$scratch/Applications/$(basename "$app")"
    exe="$(only "$installed"/Contents/MacOS/*)"
    ;;
linux)
    package="$(dpkg-deb -f "$deb" Package)"
    sudo dpkg -i "$deb" >/dev/null
    exe="$(dpkg -L "$package" | grep -m1 '^/usr/bin/')"
    installed="$exe"
    ;;
windows)
    win_msi="$(cygpath -w "$msi")"
    powershell -NoProfile -Command "exit (Start-Process msiexec -ArgumentList '/i','\"$win_msi\"','/qn','/norestart' -Wait -PassThru).ExitCode" ||
        fail "msiexec /i"
    installed="/c/Program Files/Tasqx Desktop"
    exe="$(find "$installed" -maxdepth 1 -iname '*.exe' ! -iname 'uninstall*' | head -1)"
    ;;
esac
[[ -n "$exe" && -e "$exe" ]] || fail "installed, but no executable to launch"
ok "installed: $exe"

# Launch, and give the webview time to come up and dial the daemon.
# `|| true`: lsof exits 1 when it lists nothing, and under pipefail that
# would end the script. Windows has no lsof at all, so it never calls this.
unix_fds() { { lsof -a -U -p "$daemon_pid" 2>/dev/null || true; } | tail -n +2 | wc -l | tr -d ' '; }
before_fds=0
[[ "$os" == windows ]] || before_fds="$(unix_fds)"
if [[ "$os" == linux ]]; then
    Xvfb :97 >/dev/null 2>&1 &
    xvfb_pid=$!
    sleep 1
    # WebKitGTK on a GPU-less Xvfb can fail to render (and so never run the
    # app's script); keep it off DMA-BUF, compositing and the GPU.
    # The runner's Ubuntu restricts unprivileged user namespaces, which
    # WebKitGTK's bubblewrap sandbox needs, so the page may never load there.
    DISPLAY=:97 WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
        WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 "$exe" >"$scratch/app.log" 2>&1 &
else
    "$exe" >"$scratch/app.log" 2>&1 &
fi
app_pid=$!
# A cold start takes several seconds before the webview dials, so poll. The
# daemon gains a Unix socket per client; on Windows a named pipe has no lsof
# view, and the smoke suite above is the connection check there.
connected=false
for _ in $(seq 30); do
    sleep 1
    kill -0 "$app_pid" 2>/dev/null || fail "the app exited during start-up: $(tail -5 "$scratch/app.log")"
    if [[ "$os" != windows ]] && (($(unix_fds) > before_fds)); then
        connected=true
        break
    fi
done
if [[ "$os" != windows ]]; then
    if ! $connected; then
        echo "--- the app's output" >&2
        tail -20 "$scratch/app.log" >&2
        echo "--- the app's child processes" >&2
        ps -o pid,stat,args -p "$(pgrep -d, -P "$app_pid" || echo "$app_pid")" >&2 || true
        echo "--- the daemon's Unix sockets" >&2
        lsof -a -U -p "$daemon_pid" >&2 || true
        fail "the app never connected to the scratch daemon in 30 s"
    fi
    ok "launched, and the app connected to the scratch daemon"
else
    ok "launched and still running after 30 s"
fi
stop_app

# Uninstall, then check for leftovers.
case "$os" in
macos) rm -rf "$installed" ;;
linux) sudo dpkg -r "$package" >/dev/null ;;
windows)
    powershell -NoProfile -Command "exit (Start-Process msiexec -ArgumentList '/x','\"$win_msi\"','/qn','/norestart' -Wait -PassThru).ExitCode" ||
        fail "msiexec /x"
    ;;
esac
[[ ! -e "$installed" ]] || fail "uninstalled, but $installed is still there"
remove_created_data
for dir in "${data_dirs[@]}"; do
    [[ ! -e "$dir" ]] || [[ " ${existed[*]-} " == *" $dir "* ]] || fail "could not remove $dir"
done
ok "uninstalled clean: $installed is gone, and so is every data directory this run created"
