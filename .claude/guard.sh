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
# that actually holds.
if echo "$cmd" | grep -Eq '(^|[;&|[:space:]])git[[:space:]]+push[^;&|]*([[:space:]]--force([[:space:]=]|$)|[[:space:]]-f([[:space:]]|$)|--force-with-lease)'; then
  deny "blocked: force push rewrites shared history. If genuinely needed, the user runs it themselves."
fi

case "$cmd" in
*"gh release"*)
  deny "blocked: gh release — releases ship via the tag-triggered workflow, and pushing that tag is the user's call."
  ;;
*"cargo publish"*)
  deny "blocked: cargo publish — publishing is a user decision."
  ;;
esac

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
