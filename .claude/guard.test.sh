#!/usr/bin/env bash
# Tests for the guard.sh PreToolUse hook. Feeds crafted Bash command payloads
# and asserts the exit code. Exit 2 = refused, exit 0 = allowed.
set -u

GUARD="$(dirname "$0")/guard.sh"

if [ ! -x "$GUARD" ]; then
  printf 'FATAL: %s missing or not executable\n' "$GUARD" >&2
  exit 1
fi

fail=0

# run <expected-exit> <label> <payload-json>
run() {
  local want="$1" label="$2" payload="$3" got out
  out=$(printf '%s' "$payload" | "$GUARD" 2>&1)
  got=$?
  if [ "$got" -eq "$want" ]; then
    printf 'PASS  %s (exit %s)\n' "$label" "$got"
  else
    fail=1
    printf 'FAIL  %s (want exit %s, got %s)\n      output: %s\n' "$label" "$want" "$got" "$out"
  fi
}

cmd_payload() { # $1 command
  jq -cn --arg c "$1" '{tool_input:{command:$c}}'
}

printf -- '--- refused\n'
run 2 "1. git push --force origin main" \
  "$(cmd_payload 'git push --force origin main')"

run 2 "2. git push origin main --force" \
  "$(cmd_payload 'git push origin main --force')"

run 2 "3. git push -f origin task/669-x" \
  "$(cmd_payload 'git push -f origin task/669-x')"

run 2 "4. git push --force-with-lease (bare)" \
  "$(cmd_payload 'git push --force-with-lease')"

run 2 "5. git push --force-with-lease origin main" \
  "$(cmd_payload 'git push --force-with-lease origin main')"

run 2 "6. git push --force-with-lease origin task/669-x main" \
  "$(cmd_payload 'git push --force-with-lease origin task/669-x main')"

run 2 "7. cargo run -- add x (dev build without scratch store)" \
  "$(cmd_payload 'cargo run -- add x')"

run 2 "8. force in a later segment" \
  "$(cmd_payload 'git fetch && git push --force origin task/669-x')"

printf -- '--- allowed\n'
run 0 "9. git push --force-with-lease origin task/669-guard-sh" \
  "$(cmd_payload 'git push --force-with-lease origin task/669-guard-sh')"

run 0 "10. git -C /tmp/wt push --force-with-lease --force-if-includes origin task/669-x" \
  "$(cmd_payload 'git -C /tmp/wt push --force-with-lease --force-if-includes origin task/669-x')"

run 0 "11. git push --force-with-lease=task/669-x origin task/669-x" \
  "$(cmd_payload 'git push --force-with-lease=task/669-x origin task/669-x')"

run 0 "12. gh release list" \
  "$(cmd_payload 'gh release list')"

run 0 "13. gh release view v1.0.0" \
  "$(cmd_payload 'gh release view v1.0.0')"

run 0 "14. git push origin task/669-x" \
  "$(cmd_payload 'git push origin task/669-x')"

run 0 "15. TASQX_DB + --no-daemon cargo run (existing allowance)" \
  "$(cmd_payload 'TASQX_DB=/tmp/s/tasks.db cargo run -- --no-daemon add x')"

# The guard no longer refuses `cargo publish` by string match — that verb is
# refused by permissions.deny in .claude/settings.json instead, which never
# reaches the guard at all. This case only proves the guard itself is silent
# on it; it is not proof the command is actually blocked end to end.
run 0 "16. echo cargo publish (permission deny handles cargo publish, not the guard)" \
  "$(cmd_payload 'echo cargo publish')"

if [ "$fail" -ne 0 ]; then
  printf -- '--- RESULT: FAILURES\n'
  exit 1
fi
printf -- '--- RESULT: all cases passed\n'
