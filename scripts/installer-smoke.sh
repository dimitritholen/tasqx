#!/usr/bin/env bash
#
# Run install.sh for real: download a published release, compare its checksum,
# unpack it, and run the binary that comes out.
#
# WHY THIS EXISTS, and why it is not a `--dry-run` check. A dry run prints four
# lines and touches nothing, so every assertion against it is a string compared
# against a string — and this repository already has the receipt for what that
# proves. Moving completion off clap's generic `COMPLETE` variable (cf96c81) was
# a real behavioural change that every text guard passed clean, because a string
# that matches a string that is also wrong is still a match
# (`.github/workflows/ci.yml:83-91`). The four things install.sh can get wrong in
# a way no printed line reveals are the four a dry run never reaches: the
# download URL it builds, the checksum comparison, the nested-archive unpack, and
# the atomic move into place. So this downloads, verifies, unpacks and RUNS.
#
# WHAT IT ASSERTS, as named cases:
#   * dry-run contract       — the four field labels install.ps1 and CI both
#                              assert on, and no file at the destination after
#   * real install           — v0.3.0 fetched, verified, unpacked, and the
#                              installed binary answers `--version` with 0.3.0
#   * truncated pipe         — the first 60% of the script, fed to `sh`, must
#                              run NOTHING. That is what the `main "$@"`
#                              structure exists for
#   * no hasher              — with sha256sum, shasum and openssl all off PATH
#                              the install must ABORT. There is no skip path:
#                              an installer that reports success having
#                              verified nothing is worse than one that never
#                              claimed to verify
#   * completions, no shell  — `--completions` where no shell can be identified
#                              must warn and still exit 0, because the binary is
#                              already installed by then
#
# WHAT THIS DOES NOT COVER, said out loud rather than left for a reader to
# assume. The version is PINNED to $PINNED_TAG below, so the `/releases/latest`
# redirect path — `resolve_latest_tag`, its four checks, and the CR strip on the
# `Location` header — is NOT exercised here. That trade is deliberate: resolving
# "latest" makes this check depend on a release existing, and a check that needs
# a fresh release can never gate the pull request that breaks it. The redirect
# path is covered by unit-level proofs elsewhere; if you change it, this script
# will not tell you.
#
# Also not covered: install.ps1. It is driven by the Windows leg of the CI
# installers job, because a PowerShell script wants a PowerShell host.
#
# And the last case cannot run at all on a host whose /bin/sh is bash, macOS
# included — see the comment above it. It says so on its own line when that
# happens rather than passing; a green run there has one case fewer in it.
#
# SAFETY. This never touches the caller's installation, PATH, dotfiles or store.
# Every run happens inside one `mktemp -d` with a `trap` cleanup, with
# `TASQX_INSTALL`, `TASQX_DB` and `HOME` all pointed inside it, and the binaries
# it installs are only ever invoked by absolute path.
#
# LOCAL USE. This is deliberately a plain script and not a `#[test]`, so that the
# CI job and a developer run the identical thing — a check only CI can run is one
# nobody debugs:
#
#     scripts/installer-smoke.sh                       # builds nothing, tests install.sh
#     scripts/installer-smoke.sh path/to/install.sh    # tests a specific script
#     REQUIRE=curl,shasum scripts/installer-smoke.sh   # a missing tool is a failure
#
# Needs a Unix host that the installer maps to a published target: macOS, or
# Linux on x86_64. A tool that is missing makes the cases needing it report NOT
# COVERED and skip, loudly, rather than passing quietly — unless it is named in
# $REQUIRE, which is what CI uses so that a broken `apt-get install` cannot
# degrade into a green run that measured nothing (`ci.yml:99-102`).

set -euo pipefail

# PINNED, and the header says what that costs. A tag that already exists is what
# lets this check gate the pull request that breaks the installer; "latest" would
# make it depend on a release being cut first.
PINNED_TAG="v0.3.0"
PINNED_VERSION="${PINNED_TAG#v}"

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
require=${REQUIRE:-}
failures=0
covered=()
uncovered=()

say() { printf '%s\n' "$*"; }
pass() { printf 'PASS: %s\n' "$*"; covered+=("$1"); }
fail() { printf 'FAIL: %s\n' "$*" >&2; failures=$((failures + 1)); }
have() { command -v "$1" >/dev/null 2>&1; }

# ---- the script under test ---------------------------------------------------

if [ $# -ge 1 ]; then
  install_sh=$(cd -- "$(dirname -- "$1")" && pwd)/$(basename -- "$1")
else
  install_sh="$repo_root/install.sh"
fi
[ -f "$install_sh" ] || { fail "not a file: $install_sh"; exit 1; }
say "testing $install_sh"

# ---- prerequisites -----------------------------------------------------------

# `$REQUIRE` is checked here, against PATH, before any case decides to skip.
# Naming a tool that is not installed is a failure on its own, so that a CI job
# whose `apt-get install` half-failed cannot reach the summary line reporting
# that everything it managed to run went green.
for tool in $(printf '%s\n' "$require" | tr ',' ' '); do
  if ! have "$tool"; then
    fail "REQUIRE names '$tool', which is not on PATH. A required tool is never a skip."
  fi
done

fetcher=""
if have curl; then
  fetcher="curl"
elif have wget; then
  fetcher="wget"
fi
hasher=""
for h in sha256sum shasum openssl; do
  if [ -z "$hasher" ] && have "$h"; then hasher="$h"; fi
done

# A case that cannot run says so on its own line and is listed again in the
# summary. Never silently, and never as a pass.
skip() {
  local case_name=$1 why=$2
  uncovered+=("$case_name — $why")
  say "SKIP: $case_name ($why — this case measured NOTHING)"
}

# ---- an isolated world -------------------------------------------------------

tmp=$(mktemp -d "${TMPDIR:-/tmp}/tasqx-installer-smoke.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/home"

# HOME is redirected before anything runs, not per case: `--completions` writes
# to a shell startup file under $HOME, and the one outcome this script must never
# have is editing the dotfiles of the person running it.
export HOME="$tmp/home"
export TASQX_DB="$tmp/tasks.db"
export TASQX_VERSION="$PINNED_TAG"
# The caller may have either of these pointing at their real installation. Every
# case sets TASQX_INSTALL itself; unsetting them here means a case that forgets
# to fails loudly instead of installing over somebody's binary.
unset TASQX_INSTALL

# A PATH built out of symlinks to exactly the named tools and nothing else.
# Removal is the point in two cases below, and it cannot be done by prepending a
# directory: PATH order can add a command, never hide one.
#
# A tool that is not on this machine is skipped rather than reported, because the
# assertions that use these sandboxes name the message they expect — a tool
# forgotten here turns into a loud FAIL naming the wrong message, never a pass.
sandbox_path() {
  local name=$1 dir tool src
  shift
  dir="$tmp/path-$name"
  mkdir -p "$dir"
  for tool in "$@"; do
    src=$(command -v "$tool" 2>/dev/null) || src=""
    [ -n "$src" ] && ln -sf "$src" "$dir/$tool"
  done
  printf '%s\n' "$dir"
}

# Everything install.sh reaches for on a full install, minus the hashers and
# minus `ps`, both of which the cases below add back or leave out deliberately.
# `gzip` is in the list because `tar xzf` shells out to it rather than
# decompressing itself: without it tar reports "gzip: Cannot exec", the install
# fails, and the case below would blame the completion step for it.
#
# `dash` and `ash` are in it for probe_sh_without_shell_var, which needs a
# candidate to find before it can pick one.
base_tools=(sh dash ash uname mktemp chmod grep awk sed tr rm cat mkdir cp mv tar gzip gunzip find wc dirname basename curl wget)

# The name of a POSIX sh inside $1 that arrives with $SHELL EMPTY, or "" if this
# host has none.
#
# MEASURED, never assumed from the operating system, because the answer is a
# property of the shell binary: bash calls getpwuid at startup and binds $SHELL
# to the login shell out of the passwd entry whenever the environment does not
# already carry one, and macOS ships bash as /bin/sh. Verified rather than read
# off the manual — invoked through a symlink named `sh`, bash still reports
# `SHELL=[/bin/bash]`, so the value comes from the passwd database and not from
# argv[0]. That is also why install.sh's `sh | dash | ash` guard does not catch
# it: the name it sees is a real shell.
#
# dash and ash leave it empty, which is the ordinary `curl … | sh` case on every
# Debian, Ubuntu and Alpine host.
probe_sh_without_shell_var() {
  local sandbox=$1 home=$2 probe candidate out
  probe="$tmp/probe-shell-var.sh"
  cat > "$probe" <<'PROBE'
printf 'SHELL=[%s]\n' "${SHELL:-}"
PROBE
  for candidate in sh dash ash; do
    [ -x "$sandbox/$candidate" ] || continue
    out=$(env -i PATH="$sandbox" HOME="$home" "$sandbox/$candidate" "$probe" 2>&1) || continue
    if [ "$out" = "SHELL=[]" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  printf '%s\n' ""
}

# ---- case: the dry-run contract ---------------------------------------------
#
# The four field labels are a contract rather than a debugging aid: install.ps1
# prints the same four and the CI installers job asserts on them. The second half
# of the case is the half a text comparison cannot fake — after a dry run there
# is no file.
case_dry_run() {
  local name="dry-run contract" dest="$tmp/dry/bin" out code label
  out=$(TASQX_INSTALL="$dest" sh "$install_sh" --dry-run 2>&1) && code=0 || code=$?
  if [ "$code" -ne 0 ]; then
    fail "$name: --dry-run exited $code:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  for label in "version" "platform" "archive" "install to"; do
    if ! printf '%s\n' "$out" | grep -q "^  ${label} "; then
      fail "$name: no '$label' line in:
$(printf '%s\n' "$out" | sed 's/^/      /')"
      return
    fi
  done
  if [ -e "$dest/tasqx" ]; then
    fail "$name: --dry-run created $dest/tasqx"
    return
  fi
  pass "$name"
}

# ---- case: a real install ----------------------------------------------------
#
# The only case that reaches the download, the checksum comparison, the unpack of
# a nested archive and the atomic move. The version is read back out of the
# binary that was installed, because "a file arrived" and "the right file
# arrived, executable" are different claims.
case_real_install() {
  local name="real install of $PINNED_TAG" dest="$tmp/real/bin" out code version
  if [ -z "$fetcher" ]; then
    skip "$name" "neither curl nor wget is on PATH"
    return
  fi
  if [ -z "$hasher" ]; then
    skip "$name" "none of sha256sum, shasum or openssl is on PATH"
    return
  fi
  if ! have tar; then
    skip "$name" "tar is not on PATH"
    return
  fi
  out=$(TASQX_INSTALL="$dest" sh "$install_sh" 2>&1) && code=0 || code=$?
  if [ "$code" -ne 0 ]; then
    fail "$name: the install exited $code:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if [ ! -x "$dest/tasqx" ]; then
    fail "$name: nothing executable at $dest/tasqx after an install that reported success:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  version=$("$dest/tasqx" --version </dev/null 2>&1) || version="(--version failed: $version)"
  if ! printf '%s\n' "$version" | grep -q "$PINNED_VERSION"; then
    fail "$name: the installed binary reports '$version', which does not name $PINNED_VERSION"
    return
  fi
  say "  installed binary reports: $version"
  pass "$name"
}

# ---- case: a truncated pipe --------------------------------------------------
#
# `sh` executes a pipe incrementally, so a connection dropped at byte N runs
# bytes 0..N. install.sh answers that by putting every statement inside `main`
# and calling it on the last line.
#
# "Nothing was installed" is NOT a sufficient assertion here, and that is
# measured rather than assumed: moving `main "$@"` to the top of the file — the
# mutation this case exists to catch — still installs nothing and still exits
# non-zero, because `main` is not defined yet at that point. The difference
# between the two is that the mutant RAN something, and said so:
#
#     original   sh: 446: Syntax error: end of file unexpected (expecting "}")
#     mutated    sh: 1: main: not found
#                sh: 447: Syntax error: end of file unexpected (expecting "}")
#
# So what is asserted is that no command executed at all. `not found` is the
# wording of both shells this can run under (dash says `main: not found`, bash
# says `main: command not found`), and a truncated prefix of the real script
# produces it for no other reason.
case_truncated_pipe() {
  local name="truncated pipe" dest="$tmp/trunc/bin" size cut out code
  size=$(wc -c < "$install_sh")
  cut=$((size * 60 / 100))
  out=$(head -c "$cut" "$install_sh" | TASQX_INSTALL="$dest" sh 2>&1) && code=0 || code=$?
  if [ "$code" -eq 0 ]; then
    fail "$name: a script cut off at ${cut} of ${size} bytes exited 0"
    return
  fi
  if [ -e "$dest/tasqx" ]; then
    fail "$name: a truncated script installed $dest/tasqx"
    return
  fi
  if printf '%s\n' "$out" | grep -q 'not found'; then
    fail "$name: the truncated script EXECUTED something before it failed:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  pass "$name"
}

# ---- case: no SHA-256 tool ---------------------------------------------------
#
# The message is asserted as well as the exit status, and that is what makes this
# case honest: a curated PATH that accidentally omits `tar` would also exit
# non-zero, and without the message this would pass while proving nothing about
# verification.
case_no_hasher() {
  local name="no hasher on PATH" dest="$tmp/nohash/bin" sandbox out code
  if [ -z "$fetcher" ]; then
    skip "$name" "neither curl nor wget is on PATH"
    return
  fi
  # Every tool the script needs up to the point where it looks for a digest, and
  # not one of sha256sum, shasum or openssl.
  sandbox=$(sandbox_path nohash sh uname mktemp chmod grep awk rm cat curl wget)
  out=$(env -i PATH="$sandbox" HOME="$HOME" TASQX_VERSION="$PINNED_TAG" TASQX_INSTALL="$dest" \
    sh "$install_sh" 2>&1) && code=0 || code=$?
  if [ "$code" -eq 0 ]; then
    fail "$name: the install exited 0 with no way to verify the download:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if ! printf '%s\n' "$out" | grep -q 'no SHA-256 tool on PATH'; then
    fail "$name: exited $code, but not for the documented reason:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if [ -e "$dest/tasqx" ]; then
    fail "$name: an unverified binary was installed at $dest/tasqx"
    return
  fi
  pass "$name"
}

# ---- case: --completions where no shell can be identified --------------------
#
# `--completions` runs AFTER the binary is in place and reported, so every
# failure in it is a warning and an exit 0: the install succeeded, and the only
# thing that did not happen is a convenience one command adds.
#
# The environment is emptied AND `ps` is off PATH, because install.sh falls back
# to the parent process name when $SHELL is empty — and the parent here is this
# bash script, which would answer "bash" and send the case down the success path
# instead. A container image with no procps and no $SHELL is the population
# install.sh's own comment names for that fallback, and it is the one this case
# reproduces.
#
# WHICH sh RUNS IT IS PART OF THE CASE, and this is the correction a macOS runner
# made to an earlier version that ran `sh` and assumed the rest. An empty
# environment is not enough: bash binds $SHELL from the passwd entry on the way
# up, so on a host whose /bin/sh is bash — every Mac — install.sh arrived with
# $SHELL=/bin/bash, identified a shell, wrote the block, and this case failed
# holding the evidence that install.sh had behaved correctly. So the shell is
# probed for rather than named, and where no shell on the host leaves $SHELL
# empty the case reports NOT COVERED.
#
# It skips rather than adapting because the only remaining way to create the
# condition is to set $SHELL in this harness, and that is the one fix this case
# forbids: it would turn the run green while proving nothing about a real user.
case_completions_without_shell() {
  local name="--completions with no shell to set up" dest="$tmp/nocomp/bin"
  local home="$tmp/nocomp/home" sandbox posix_sh out code
  if [ -z "$fetcher" ] || [ -z "$hasher" ] || ! have tar; then
    skip "$name" "a real install is needed first, and curl/wget, a hasher or tar is missing"
    return
  fi
  mkdir -p "$home"
  sandbox=$(sandbox_path noshell "${base_tools[@]}" sha256sum shasum openssl)
  posix_sh=$(probe_sh_without_shell_var "$sandbox" "$home")
  if [ -z "$posix_sh" ]; then
    skip "$name" "every POSIX sh on this host arrives with \$SHELL already populated — bash binds it from the passwd entry, and this host's /bin/sh is bash. The condition cannot be created without setting \$SHELL, which is the one fix this case forbids"
    return
  fi
  say "  driving install.sh with '$posix_sh', which leaves \$SHELL empty"
  out=$(env -i PATH="$sandbox" HOME="$home" TASQX_DB="$tmp/nocomp/tasks.db" \
    TASQX_VERSION="$PINNED_TAG" TASQX_INSTALL="$dest" \
    "$sandbox/$posix_sh" "$install_sh" --completions 2>&1) && code=0 || code=$?
  if [ "$code" -ne 0 ]; then
    fail "$name: exited $code; a completion it could not switch on must not fail the install:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if [ ! -x "$dest/tasqx" ]; then
    fail "$name: no binary at $dest/tasqx, so this case never reached the completion step:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if ! printf '%s\n' "$out" | grep -q 'cannot tell which shell to set up'; then
    fail "$name: exited 0 without warning that it could not tell which shell to set up:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  # The other half of the promise: a step that could not decide which startup
  # file to edit must not have edited one.
  if [ -n "$(find "$home" -type f 2>/dev/null)" ]; then
    fail "$name: it wrote into the home directory it could not identify a shell for:
$(find "$home" -type f | sed 's/^/      /')"
    return
  fi
  pass "$name"
}

# ---- run ---------------------------------------------------------------------

case_dry_run
case_real_install
case_truncated_pipe
case_no_hasher
case_completions_without_shell

say ""
# Counted before it is expanded. bash 3.2 — which is the bash macOS still ships,
# and therefore the one this runs under on a macOS runner — treats `${arr[*]}` on
# an empty array as an unbound variable under `set -u`, so the guard is what
# keeps the summary from aborting on exactly the run that had nothing to say.
if [ ${#covered[@]} -gt 0 ]; then
  say "cases executed: ${covered[*]}"
else
  say "cases executed: (none)"
fi
if [ ${#uncovered[@]} -gt 0 ]; then
  say "NOT covered by this run:"
  for u in "${uncovered[@]}"; do say "  - $u"; done
fi
say "NOT covered by design: the /releases/latest redirect (this run pins $PINNED_TAG), and install.ps1."

if [ "$failures" -ne 0 ]; then
  say ""
  say "$failures check(s) failed"
  exit 1
fi
# "no failures" and "nothing ran" must not print the same sentence. A run that
# installed nothing has measured nothing, and saying so is the point.
if [ ${#covered[@]} -eq 0 ]; then
  say "NOTHING WAS EXECUTED — this run proved nothing about install.sh."
  say "Install curl or wget and a SHA-256 tool, or set REQUIRE=curl,shasum to make absence a failure."
else
  say "all executed cases passed"
fi
