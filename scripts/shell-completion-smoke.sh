#!/usr/bin/env bash
#
# Execute the REAL activation lines for zsh and fish, and drive the completion
# they install against a seeded store.
#
# WHY THIS EXISTS, given that tests/completion.rs already has 2400 lines about
# these shells. Everything in there is a TEXT comparison or an assertion that a
# registration was emitted and is non-empty. Neither proves the line WORKS. The
# same feature already shipped a text-checked copy of these lines that a real
# mutation walked straight past: moving completion off clap's generic `COMPLETE`
# variable (commit cf96c81) left every text guard green, because a string that
# matches a string that is also wrong is still a match. Only bash and PowerShell
# have ever had their activation line actually EXECUTED, so zsh, fish and elvish
# — three of the five shells tasqx claims to support — rested entirely on
# `assert_eq!` against a constant.
#
# So this runs the line. Not a copy of the line: the script asks the binary for
# it (`tasqx completions <shell>`), the way the manual tells a user to, and
# evaluates whatever comes back. If the printed line is wrong, this reddens; if
# it is right but the completer it installs cannot answer, this reddens too.
#
# WHAT IT ASSERTS, behaviourally, in each shell:
#   * `tasqx li`   completes to `list`               — the static-subcommand path
#   * `tasqx done ` completes to a SEEDED task id    — the dynamic store lookup
# The second is the one that matters most: it is the only assertion that proves
# the completer reaches the store at all, and a completer that silently answers
# nothing is indistinguishable from one that answers correctly if all you check
# is "a registration was emitted".
#
# SAFETY. This never touches the caller's dotfiles. It does not run `completions
# --install`, it writes no profile, and every shell it starts is started without
# rc files (`zsh -f`, `fish --no-config`) inside a throwaway HOME. The only
# thing it evaluates is the one line the binary printed to stdout.
#
# LOCAL USE. This is deliberately a plain script and not a `#[test]`, so that the
# CI job and a developer run the identical thing — a check only CI can run is one
# nobody debugs:
#
#     scripts/shell-completion-smoke.sh                  # builds, then checks
#     scripts/shell-completion-smoke.sh path/to/tasqx    # checks an existing binary
#     REQUIRE=zsh,fish scripts/shell-completion-smoke.sh # missing shell = failure
#
# Needs a Unix host with zsh and/or fish installed. A shell that is not installed
# is reported as NOT COVERED and skipped, loudly, rather than passing quietly —
# unless it is named in $REQUIRE, which is what CI uses so that a broken
# `apt-get install` cannot degrade into a green run that measured nothing.

set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
require=${REQUIRE:-}
failures=0
covered=()
uncovered=()

say() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; failures=$((failures + 1)); }

# ---- the binary under test --------------------------------------------------

if [ $# -ge 1 ]; then
  bin=$(cd -- "$(dirname -- "$1")" && pwd)/$(basename -- "$1")
else
  say "building tasqx (pass a path to skip)..."
  (cd "$repo_root" && cargo build -p tasqx-cli --quiet)
  bin="$repo_root/target/debug/tasqx"
fi
[ -x "$bin" ] || { fail "not an executable: $bin"; exit 1; }

# ---- an isolated world ------------------------------------------------------

tmp=$(mktemp -d "${TMPDIR:-/tmp}/tasqx-completion-smoke.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/home" "$tmp/bin"

# The activation lines invoke a BARE `tasqx`, because that is what a user's
# profile will contain. Put the binary under test on PATH under that name so the
# line resolves to it and not to whatever the developer happens to have
# installed — this check must measure the working tree, never ~/.cargo/bin.
ln -s "$bin" "$tmp/bin/tasqx"
export PATH="$tmp/bin:$PATH"
export HOME="$tmp/home"
export TASQX_DB="$tmp/tasks.db"

seed_title="Seeded completion target"
seed_json=$("$bin" --no-daemon --json add "$seed_title")
# short_id is the id a user types and therefore the id completion must offer.
seed_id=$(printf '%s' "$seed_json" | tr -d ' \n' | sed -n 's/.*"short_id":\([0-9]*\).*/\1/p')
[ -n "$seed_id" ] || { fail "could not read short_id out of: $seed_json"; exit 1; }
say "seeded task #$seed_id \"$seed_title\" in $TASQX_DB"

# ---- assertions -------------------------------------------------------------

# Candidates arrive as `value<SEP>description`; only the value is the
# completion. zsh separates with ':' and fish with a tab, so each driver
# normalises to one value per line before we get here.
assert_candidate() {
  local shell=$1 what=$2 want=$3 got=$4
  if printf '%s\n' "$got" | grep -qxF -- "$want"; then
    say "  ok   $shell: $what -> $want"
  else
    fail "$shell: completing '$what' did not offer '$want'. got:
$(printf '%s\n' "$got" | sed 's/^/      /')"
  fi
}

have() { command -v "$1" >/dev/null 2>&1; }

# A shell we cannot run is reported, never assumed. `$REQUIRE` turns "missing"
# into a failure for the environments (CI) that just installed it on purpose.
skip_or_fail() {
  local shell=$1 why=$2
  uncovered+=("$shell — $why")
  case ",$require," in
    *",$shell,"*) fail "$shell was REQUIRED but $why" ;;
    *) say "  SKIP $shell: $why (completion for $shell is NOT covered by this run)" ;;
  esac
}

# ---- zsh --------------------------------------------------------------------
#
# The driver sources the real line and then calls the completer the way zsh's
# own completion system would: `words` holds the command line, `CURRENT` is the
# 1-based index of the word under the cursor.
#
# Two things are deliberately NOT hardcoded. The completer's function name is
# read out of zsh's `$_comps` registry rather than restated from clap's script —
# if `compdef` did not actually register anything, that lookup comes back empty
# and this fails, which is the whole point. And `_describe` is the real compsys
# sink the generated function calls; here it is replaced by one that prints its
# array, because a non-interactive zsh has no line editor to render into. That
# substitution is the honest boundary of this check: everything up to and
# including the completer producing candidates is real, the rendering is not.
run_zsh() {
  have zsh || { skip_or_fail zsh "zsh is not installed"; return; }
  say "zsh: $(zsh --version)"

  "$bin" completions zsh > "$tmp/zsh-activation"
  say "  activation line: $(cat "$tmp/zsh-activation")"

  cat > "$tmp/drive.zsh" <<'ZSH'
emulate -L zsh
setopt err_exit no_unset
autoload -Uz compinit
compinit -u -d "${TMP_DIR}/zcompdump"

# THE REAL LINE, as printed by the binary.
source "${TMP_DIR}/zsh-activation"

local fn=${_comps[tasqx]:-}
if [[ -z $fn ]]; then
  print -u2 "compdef registered no completer for tasqx"
  exit 1
fi

# Stand in for compsys's renderer: print the candidate array it was handed.
_describe() { local n=$3; print -rl -- ${(P)n} }

drive() {
  words=("$@")
  CURRENT=$#
  local -a out
  out=("${(@f)$($fn)}")
  # Keep the value, drop the description, so the caller compares completions.
  print -rl -- ${out%%:*}
}

print -r -- "--- li ---"
drive tasqx li
print -r -- "--- done ---"
drive tasqx done ''
ZSH

  # stderr is kept OUT of the parsed stream on purpose: compinit is chatty on
  # some installs, and a warning landing between the section markers would be
  # read as a completion candidate.
  local out
  if ! out=$(TMP_DIR="$tmp" zsh -f "$tmp/drive.zsh" 2>"$tmp/zsh.err"); then
    fail "zsh driver exited non-zero:
$(sed 's/^/      /' "$tmp/zsh.err")"
    return
  fi
  local li done_
  li=$(printf '%s\n' "$out" | sed -n '/^--- li ---$/,/^--- done ---$/p' | sed '1d;$d')
  done_=$(printf '%s\n' "$out" | sed -n '/^--- done ---$/,$p' | sed '1d')
  assert_candidate zsh "tasqx li" "list" "$li"
  assert_candidate zsh "tasqx done " "$seed_id" "$done_"
  covered+=(zsh)
}

# ---- fish -------------------------------------------------------------------
#
# fish is the one shell that can be driven end to end without any substitution
# at all: `complete -C <line>` runs the real completion machinery and prints
# what it would offer. Nothing here stands in for anything.
run_fish() {
  have fish || { skip_or_fail fish "fish is not installed"; return; }
  say "fish: $(fish --version)"

  "$bin" completions fish > "$tmp/fish-activation"
  say "  activation line: $(cat "$tmp/fish-activation")"

  cat > "$tmp/drive.fish" <<'FISH'
# THE REAL LINE, as printed by the binary.
source $TMP_DIR/fish-activation
or begin; echo "sourcing the activation line failed" >&2; exit 1; end

echo "--- li ---"
complete -C 'tasqx li'
echo "--- done ---"
complete -C 'tasqx done '
FISH

  local out
  if ! out=$(TMP_DIR="$tmp" fish --no-config "$tmp/drive.fish" 2>"$tmp/fish.err"); then
    fail "fish driver exited non-zero:
$(sed 's/^/      /' "$tmp/fish.err")"
    return
  fi
  # fish separates candidate from description with a tab.
  local li done_
  li=$(printf '%s\n' "$out" | sed -n '/^--- li ---$/,/^--- done ---$/p' | sed '1d;$d' | cut -f1)
  done_=$(printf '%s\n' "$out" | sed -n '/^--- done ---$/,$p' | sed '1d' | cut -f1)
  assert_candidate fish "tasqx li" "list" "$li"
  assert_candidate fish "tasqx done " "$seed_id" "$done_"
  covered+=(fish)
}

# ---- elvish -----------------------------------------------------------------
#
# Not driven, and said out loud rather than left for a reader to assume. elvish's
# activation installs `$edit:completion:arg-completer[tasqx]`, and the `edit:`
# namespace belongs to elvish's interactive line editor: a non-interactive
# `elvish -c` has no editor, so there is no map to install into and nothing to
# call back. Driving it would mean a pty harness, which is a different and much
# flakier kind of test than the two above.
#
# What that leaves elvish with is what zsh and fish had until this script: text
# comparison in tests/completion.rs, plus the registry guard that fails the build
# if clap gains or loses a shell. That is weaker, and the weakness is the report.
run_elvish() {
  if have elvish; then
    uncovered+=("elvish — installed, but its arg-completer lives in the interactive edit: namespace, which a non-interactive elvish cannot populate")
    say "  SKIP elvish: installed, but its completer lives in the interactive"
    say "       edit: namespace — a non-interactive elvish has no line editor to"
    say "       register into, so there is nothing to call. NOT COVERED here;"
    say "       elvish rests on the text guards in tests/completion.rs."
  else
    uncovered+=("elvish — not installed")
    say "  SKIP elvish: not installed. NOT COVERED here."
  fi
}

# ---- run --------------------------------------------------------------------

run_zsh
run_fish
run_elvish

say ""
say "executed activation lines: ${covered[*]:-(none)}"
if [ ${#uncovered[@]} -gt 0 ]; then
  say "NOT covered by this run:"
  for u in "${uncovered[@]}"; do say "  - $u"; done
fi

if [ "$failures" -ne 0 ]; then
  say ""
  say "$failures check(s) failed"
  exit 1
fi
# "no failures" and "nothing ran" must not print the same sentence. A run that
# executed no activation line has measured nothing, and saying so is the point
# of the whole exercise.
if [ ${#covered[@]} -eq 0 ]; then
  say "NOTHING WAS EXECUTED — this run proved nothing about any shell."
  say "Install zsh and/or fish, or set REQUIRE=zsh,fish to make absence a failure."
else
  say "all executed activation lines completed correctly"
fi
