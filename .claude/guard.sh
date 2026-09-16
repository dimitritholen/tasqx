#!/usr/bin/env bash
# PreToolUse guard for Bash commands. The hook payload arrives as JSON on
# stdin; exit 2 blocks the call and feeds stderr back to the agent.
set -u
cmd=$(jq -r '.tool_input.command // empty' 2>/dev/null) || exit 0
[ -z "$cmd" ] && exit 0

deny() {
  echo "$1" >&2
  exit 2
}

# Force pushes, every flag order. The permission deny rules are prefix
# matches and miss `git push origin main --force`; this regex is the layer
# that actually holds. `git[^;&|]*[[:space:]]push` (not `git[[:space:]]+push`)
# so a leading `git -C <dir> push`, as used throughout this repo's workflow,
# still counts as the push it is.
if echo "$cmd" | grep -Eq '(^|[;&|[:space:]])git[^;&|]*[[:space:]]push[^;&|]*([[:space:]]--force([[:space:]=]|$)|[[:space:]]-f([[:space:]]|$))'; then
  deny "blocked: force push rewrites shared history. If genuinely needed, the user runs it themselves."
fi

# --force-with-lease is safer than a bare --force (it fails if the remote
# moved since the last fetch), so it is allowed — but only for reshaping a
# task branch's own history, never main. Refused unless this push's own
# segment names a task/<id>-<slug> ref and does not also name main/master
# (a push naming both is still a push to main, lease or not).
if echo "$cmd" | grep -Eq '(^|[;&|[:space:]])git[^;&|]*[[:space:]]push[^;&|]*--force-with-lease'; then
  segment=$(printf '%s\n' "$cmd" | grep -Eo '[[:space:]]push[^;&|]*')
  if printf '%s\n' "$segment" | grep -Eq '(^|[[:space:]/:=])(main|master)([[:space:]]|$)' \
    || ! printf '%s\n' "$segment" | grep -Eq '(^|[[:space:]/:=])task/[0-9]+-'; then
    deny "blocked: --force-with-lease without a task/<id>-<slug> ref. Name the task branch explicitly (git push --force-with-lease origin task/<id>-<slug>); main is never force-pushed."
  fi
fi

# gh release create/delete/edit/upload and cargo publish are refused by
# permissions.deny in .claude/settings.json, which matches those exact
# verbs — the guard used to duplicate this with a substring match that also
# caught read-only commands like `gh release list`, so it no longer does.

# Dev builds of tasqx must never touch the real store. Both TASQX_DB and
# --no-daemon are required INLINE in the same command, because every Bash
# call is a fresh shell. The bare installed `tasqx` is exempt: that one is
# doing real task tracking on purpose.
# Checked on the command with its comments stripped: `... # --no-daemon` used to
# satisfy the grep while the flag never reached the binary. A `#` inside quotes
# (`add "Fix #42"`) is stripped too, which can only refuse more, never less.
code=$(printf '%s\n' "$cmd" | sed -E 's/(^|[[:space:]])#.*$//')
# scripts/demo-store.py, snap.sh and snap-tui.sh pass --no-daemon on every call
# they make, so naming the dev binary as their TASQX= value needs only the
# scratch store. Any OTHER mention of it still needs both.
direct=$(printf '%s\n' "$code" | sed -E 's/TASQX=[^[:space:]]*//g')
if printf '%s\n' "$code" | grep -Eq 'target/(debug|release)/tasqx|cargo[[:space:]]+run'; then
  if printf '%s\n' "$code" | grep -Eq 'scripts/(demo-store\.py|snap\.sh|snap-tui\.sh)' \
    && printf '%s\n' "$code" | grep -q 'TASQX_DB=' \
    && ! printf '%s\n' "$direct" | grep -Eq 'target/(debug|release)/tasqx|cargo[[:space:]]+run'; then
    :
  elif ! { printf '%s\n' "$code" | grep -q 'TASQX_DB=' && printf '%s\n' "$code" | grep -qe '--no-daemon'; }; then
    deny "blocked: dev-build tasqx without a scratch store. Prefix TASQX_DB=<scratch>/tasks.db and pass --no-daemon in this same command, or it writes to the real store."
  fi
fi

exit 0
