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
if echo "$cmd" | grep -Eq 'target/(debug|release)/tasqx|cargo[[:space:]]+run'; then
  if ! { echo "$cmd" | grep -q 'TASQX_DB=' && echo "$cmd" | grep -qe '--no-daemon'; }; then
    deny "blocked: dev-build tasqx without a scratch store. Prefix TASQX_DB=<scratch>/tasks.db and pass --no-daemon in this same command, or it writes to the real store."
  fi
fi

exit 0
