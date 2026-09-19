#!/usr/bin/env bash
# Stop-gate for CLAUDE.md's "one builder, one review": a task/* branch with
# commits ahead of main must have been reviewed with /1337:review
# AFTER its last commit before the turn may end. A branch already pushed to
# origin passed through land-tasqx-pr, which runs after review, so it is
# exempt. Exit 2 is the only code that blocks a Stop hook, so every refusal
# funnels there. Reads the hook's JSON on stdin for transcript_path.
set -u
branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null) || exit 0
case "$branch" in task/*) ;; *) exit 0 ;; esac
ahead=$(git rev-list --count main..HEAD 2>/dev/null || echo 0)
[ "$ahead" -gt 0 ] || exit 0
if [ "$(git rev-parse HEAD)" = "$(git rev-parse "origin/$branch" 2>/dev/null)" ]; then
  exit 0
fi
transcript=$(jq -r '.transcript_path // empty' 2>/dev/null)
[ -n "$transcript" ] && [ -r "$transcript" ] || exit 0

last_commit=$(TZ=UTC git log -1 --format=%cd --date=format-local:%Y-%m-%dT%H:%M:%SZ)
# Only a real invocation counts: the Skill tool's input, or the slash command
# typed by the user. Never the words alone, or this hook's own refusal text
# would satisfy it on the next turn.
last_review=$(jq -r '
  select(.type == "assistant" or .type == "user")
  | select((.message.content | tostring)
      | test("\"skill\":\"1337:review\"|<command-name>/1337:review</command-name>"))
  | .timestamp // empty' "$transcript" 2>/dev/null | sort | tail -n 1)

if [ -z "$last_review" ]; then
  echo "review gate: $branch is $ahead commit(s) ahead of main and the 1337 review skill has not run this session — review the branch diff, send findings to the builder, then end the turn" >&2
  exit 2
fi
if [[ "$last_review" < "$last_commit" ]]; then
  echo "review gate: $branch gained a commit at $last_commit after the last review at $last_review — review again before ending the turn" >&2
  exit 2
fi
exit 0
