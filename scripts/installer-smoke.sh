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
#   * uninstall cuts block   — `--uninstall` removes the completion block an
#                              older tasqx wrote (D223), byte for byte, and
#                              leaves every other byte, link and mode alone
#   * dry-run uninstall      — `--dry-run --uninstall` names the block it would
#                              cut and leaves the file's bytes alone
#   * ripwire hint           — a PATH without `ripwire` (D178/#795) prints the
#                              same install-it-from-here sentence setup.rs does,
#                              after an install that otherwise succeeded
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

# HOME is redirected before anything runs, not per case: `--uninstall` edits
# shell startup files under $HOME, and the one outcome this script must never
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

# Everything install.sh reaches for on a full install, minus the hashers, which
# the cases below add back deliberately. `gzip` is in the list because `tar xzf`
# shells out to it rather than decompressing itself: without it tar reports
# "gzip: Cannot exec" and the install fails for a reason no case is about.
base_tools=(sh uname mktemp chmod grep awk sed tr rm cat mkdir cp mv tar gzip gunzip find wc dirname basename curl wget)

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
  # Exit 0 is only possible when the cut lands between two functions or in a
  # comment: the prefix then parses whole, defines functions and calls none. It
  # is acceptable only in silence, since a prefix that ran anything says so.
  if [ "$code" -eq 0 ] && [ -n "$out" ]; then
    fail "$name: a script cut off at ${cut} of ${size} bytes exited 0 and printed:
$(printf '%s\n' "$out" | sed 's/^/      /')"
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

# ---- case: --uninstall cuts the old completion block -------------------------
#
# D223: tasqx no longer edits startup files, so the uninstaller removes the
# block an older `tasqx completions --install` wrote. What is asserted is BYTES,
# against a file built by hand: every line outside the block survives — a stray
# end marker, a line that only mentions the marker, CRLF, a missing final
# newline, a symlinked dotfile and its mode — and a begin marker never closed
# leaves its file exactly as it was. No download: the cut runs before the
# binary is looked at, so a placeholder binary is enough.
case_uninstall_cuts_block() {
  local name="--uninstall cuts the completion block" base="$tmp/cutblock"
  local home="$base/home" dest="$base/bin" out code begin end block
  local fish="$home/.config/fish/completions/tasqx.fish" mode
  begin='# >>> tasqx completions >>>'
  end='# <<< tasqx completions <<<'
  block="$begin
# Added by \`tasqx completions --install\`. Remove it with
# \`tasqx completions --uninstall\`, or just delete these five lines.
source <(TASQX_COMPLETE=bash tasqx)
$end
"
  mkdir -p "$home/.elvish" "$(dirname "$fish")" "$base/dots" "$dest"
  printf 'echo placeholder\n' > "$dest/tasqx"
  chmod +x "$dest/tasqx"

  # bash: a symlink into a "dotfiles repo", mode 600, block indented between
  # user lines, a line that merely quotes the marker, a stray end marker, and
  # no final newline.
  printf 'export A=1\n  %secho "%s"\n%s\nlast line' "$block" "$begin" "$end" > "$base/dots/bashrc"
  printf 'export A=1\necho "%s"\n%s\nlast line' "$begin" "$end" > "$base/bashrc.want"
  chmod 600 "$base/dots/bashrc"
  ln -s "$base/dots/bashrc" "$home/.bashrc"
  # zsh: CRLF throughout, so the markers carry a trailing \r.
  printf 'a\r\n%s\r\n# x\r\n%s\r\nb\r\n' "$begin" "$end" > "$home/.zshrc"
  printf 'a\r\nb\r\n' > "$base/zshrc.want"
  # elvish: never closed, so untouched.
  printf 'keep\n%s\nnot ours to guess\n' "$begin" > "$home/.elvish/rc.elv"
  cp "$home/.elvish/rc.elv" "$base/elv.want"
  # fish: the file was only ever the block.
  printf '%s' "$block" > "$fish"

  out=$(HOME="$home" TASQX_INSTALL="$dest" XDG_CONFIG_HOME="" sh "$install_sh" --uninstall 2>&1) && code=0 || code=$?
  if [ "$code" -ne 0 ]; then
    fail "$name: --uninstall exited $code:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if ! cmp -s "$base/dots/bashrc" "$base/bashrc.want"; then
    fail "$name: ~/.bashrc is not the file minus its block:
$(od -c "$base/dots/bashrc" | sed 's/^/      /')"
    return
  fi
  if [ ! -L "$home/.bashrc" ]; then
    fail "$name: the symlinked ~/.bashrc was replaced by a regular file"
    return
  fi
  mode=$(find "$base/dots/bashrc" -perm 600)
  if [ -z "$mode" ]; then
    fail "$name: ~/.bashrc's mode is no longer 600"
    return
  fi
  if ! cmp -s "$home/.zshrc" "$base/zshrc.want"; then
    fail "$name: the CRLF ~/.zshrc is not the file minus its block:
$(od -c "$home/.zshrc" | sed 's/^/      /')"
    return
  fi
  if ! cmp -s "$home/.elvish/rc.elv" "$base/elv.want"; then
    fail "$name: an unclosed block's file was changed"
    return
  fi
  if ! printf '%s\n' "$out" | grep -q 'never closed'; then
    fail "$name: no warning about the unclosed block in:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if [ -s "$fish" ]; then
    fail "$name: the fish file still holds bytes:
$(od -c "$fish" | sed 's/^/      /')"
    return
  fi
  if [ -e "$dest/tasqx" ]; then
    fail "$name: the binary was not removed"
    return
  fi
  if [ -n "$(find "$home" "$base/dots" -name '*.tasqx-uninstall*' 2>/dev/null)" ]; then
    fail "$name: a temp or backup file was left behind:
$(find "$home" "$base/dots" -name '*.tasqx-uninstall*' | sed 's/^/      /')"
    return
  fi
  pass "$name"
}

# ---- case: --dry-run --uninstall cuts nothing ---------------------------------
#
# The dry run's whole promise is "removes nothing", and since D223 an uninstall
# edits startup files. So a marked block is seeded and the BYTES are compared
# after the dry run, along with the binary still being there, and the dry run
# must name the file it would cut rather than staying quiet about it.
case_dry_run_uninstall_cuts_nothing() {
  local name="--dry-run --uninstall cuts nothing" base="$tmp/drycut"
  local home="$base/home" dest="$base/bin" out code
  mkdir -p "$home" "$dest"
  printf 'echo placeholder\n' > "$dest/tasqx"
  printf 'export A=1\n# >>> tasqx completions >>>\nsource <(TASQX_COMPLETE=bash tasqx)\n# <<< tasqx completions <<<\nlast\n' > "$home/.bashrc"
  cp "$home/.bashrc" "$base/bashrc.want"

  out=$(HOME="$home" TASQX_INSTALL="$dest" XDG_CONFIG_HOME="" sh "$install_sh" --dry-run --uninstall 2>&1) && code=0 || code=$?
  if [ "$code" -ne 0 ]; then
    fail "$name: exited $code:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if ! cmp -s "$home/.bashrc" "$base/bashrc.want"; then
    fail "$name: the dry run changed ~/.bashrc:
$(od -c "$home/.bashrc" | sed 's/^/      /')"
    return
  fi
  if [ ! -e "$dest/tasqx" ]; then
    fail "$name: the dry run removed the binary"
    return
  fi
  if ! printf '%s\n' "$out" | grep -q "the tasqx block in $home/.bashrc would be cut"; then
    fail "$name: the dry run does not name the block it would cut:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  pass "$name"
}

# ---- case: the ripwire hint on a PATH without ripwire ------------------------
#
# D178/#795: install.sh names https://github.com/redhat-et/ripwire, the same
# sentence RIPWIRE_INSTALL_HINT (crates/tasqx-cli/src/setup.rs) carries, once an
# install otherwise succeeds. The sandbox never symlinks `ripwire` in — it is
# not in $base_tools — so the assertion holds whether or not this host happens
# to have ripwire installed for real.
case_ripwire_hint() {
  local name="ripwire hint on a PATH without ripwire" dest="$tmp/ripwire/bin" sandbox out code
  if [ -z "$fetcher" ] || [ -z "$hasher" ] || ! have tar; then
    skip "$name" "a real install is needed first, and curl/wget, a hasher or tar is missing"
    return
  fi
  sandbox=$(sandbox_path ripwire "${base_tools[@]}" "$hasher")
  out=$(env -i PATH="$sandbox" HOME="$HOME" TASQX_DB="$tmp/ripwire/tasks.db" \
    TASQX_VERSION="$PINNED_TAG" TASQX_INSTALL="$dest" \
    sh "$install_sh" 2>&1) && code=0 || code=$?
  if [ "$code" -ne 0 ]; then
    fail "$name: the install exited $code:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  if ! printf '%s\n' "$out" | grep -q 'install it from https://github.com/redhat-et/ripwire'; then
    fail "$name: no ripwire hint in:
$(printf '%s\n' "$out" | sed 's/^/      /')"
    return
  fi
  pass "$name"
}

# ---- run ---------------------------------------------------------------------

case_dry_run
case_real_install
case_truncated_pipe
case_no_hasher
case_uninstall_cuts_block
case_dry_run_uninstall_cuts_nothing
case_ripwire_hint

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
