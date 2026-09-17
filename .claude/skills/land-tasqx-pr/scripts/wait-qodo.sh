#!/usr/bin/env bash
#
# Wait for Qodo to finish reviewing a PR, then list its unresolved review
# threads (land-tasqx-pr step 4).
#
#   .claude/skills/land-tasqx-pr/scripts/wait-qodo.sh <pr-number> [--bodies]
#
# Prints `busy=0` once Qodo's "Qodo is busy" comment is gone (`busy=1` if it
# was still there after the time limit), then one JSON object per unresolved
# thread: {id, path, line, author}. No thread lines means nothing is left to
# answer. With --bodies it prints each unresolved thread's id and its first
# comment as plain text (HTML tags stripped) instead — the finding to verify.
# Exit 0 either way; exit 2 on a usage error.
#
# Run it after `gh pr checks <n> --watch`: CI and Qodo finish independently,
# and a clean check run says nothing about the reviewers. It exists as a file
# because the worktree-isolation guard refuses an inline `gh … --jq` with
# nested quotes as "too complex to verify".
#
# Env:
#   WAIT_QODO_TRIES     polls before giving up (default 40, 30 s apart)
#   WAIT_QODO_SETTLE    seconds to wait after Qodo finishes, so its threads
#                       are posted before they are listed (default 30)
set -euo pipefail

bodies=0
if [ "$#" -eq 2 ] && [ "$2" = "--bodies" ]; then
    bodies=1
elif [ "$#" -ne 1 ]; then
    set -- ""
fi
if ! [[ "${1:-}" =~ ^[0-9]+$ ]]; then
    sed -n '3,23p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi
pr=$1
tries=${WAIT_QODO_TRIES:-40}
settle=${WAIT_QODO_SETTLE:-30}

busy=1
for _ in $(seq 1 "$tries"); do
    count=$(gh pr view "$pr" --json comments \
        --jq '[.comments[] | select(.author.login=="qodo-code-review") | select(.body|test("Qodo is busy"))] | length' \
        2>/dev/null || echo 1)
    if [ "$count" = "0" ]; then
        busy=0
        break
    fi
    sleep 30
done
echo "busy=$busy"
sleep "$settle"

repo=$(gh repo view --json owner,name --jq '.owner.login + " " + .name')
owner=${repo% *}
name=${repo#* }

query='query($o:String!,$n:String!,$p:Int!){repository(owner:$o,name:$n){pullRequest(number:$p){reviewThreads(first:100){nodes{id isResolved path line comments(first:1){nodes{author{login} body}}}}}}}'

if [ "$bodies" = "1" ]; then
    gh api graphql -f query="$query" -f o="$owner" -f n="$name" -F p="$pr" \
        --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false) | "=== \(.id) \(.path):\(.line) (\(.comments.nodes[0].author.login))\n\(.comments.nodes[0].body)"' |
        sed -e 's/<[^>]*>//g' -e '/^[[:space:]]*$/d'
else
    gh api graphql -f query="$query" -f o="$owner" -f n="$name" -F p="$pr" \
        --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false) | {id,path,line,author:.comments.nodes[0].author.login}'
fi
