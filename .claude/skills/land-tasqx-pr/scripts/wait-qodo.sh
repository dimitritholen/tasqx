#!/usr/bin/env bash
#
# Wait for Qodo to finish reviewing a PR, then list its unresolved review
# threads (land-tasqx-pr step 4).
#
#   .claude/skills/land-tasqx-pr/scripts/wait-qodo.sh <pr-number> [--bodies]
#
# Qodo counts as finished only when it has commented on the PR at all and none
# of its comments still says "Qodo is busy" — so a PR Qodo has not reached yet
# is still waited on. Then it prints `qodo=done` and one JSON object per
# unresolved thread: {id, path, line, author}. With --bodies it prints each
# unresolved thread's id and its first comment as text instead (HTML tags
# removed, anything else — `Vec<T>`, `a < b` — kept): the finding to verify.
# Every page of threads is read.
#
# Exit 0 when Qodo finished and the threads were listed (no thread lines means
# nothing is left to answer); 3 when Qodo had not finished before the time
# limit or GitHub kept failing — do NOT merge on a 3; 2 on a usage error.
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
set -uo pipefail

usage() {
    sed -n '3,28p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

bodies=0
case "$#" in
1) ;;
2) [ "$2" = "--bodies" ] || usage; bodies=1 ;;
*) usage ;;
esac
[[ "$1" =~ ^[0-9]+$ ]] || usage
pr=$1
tries=${WAIT_QODO_TRIES:-40}
settle=${WAIT_QODO_SETTLE:-30}

# `<total qodo comments> <busy qodo comments>`, or nothing when gh failed.
qodo_state() {
    gh pr view "$pr" --json comments \
        --jq '[.comments[] | select(.author.login=="qodo-code-review")] | "\(length) \([.[] | select(.body|test("Qodo is busy"))] | length)"' \
        2>/dev/null
}

done_=0
for i in $(seq 1 "$tries"); do
    state=$(qodo_state) || state=""
    if [ -n "$state" ]; then
        total=${state% *}
        busy=${state#* }
        if [ "$total" -gt 0 ] && [ "$busy" -eq 0 ]; then
            done_=1
            break
        fi
    fi
    [ "$i" -lt "$tries" ] && sleep 30
done
if [ "$done_" -ne 1 ]; then
    if [ -n "$state" ]; then
        echo "qodo=unfinished: ${state% *} Qodo comments, ${state#* } still busy — do not merge" >&2
    else
        echo "qodo=unfinished: gh could not read the PR — do not merge" >&2
    fi
    exit 3
fi
echo "qodo=done"
sleep "$settle"

repo=$(gh repo view --json owner,name --jq '.owner.login + " " + .name') || {
    echo "wait-qodo: gh repo view failed" >&2
    exit 3
}
owner=${repo% *}
name=${repo#* }

query='query($o:String!,$n:String!,$p:Int!,$after:String){repository(owner:$o,name:$n){pullRequest(number:$p){reviewThreads(first:100,after:$after){pageInfo{hasNextPage endCursor} nodes{id isResolved path line comments(first:1){nodes{author{login} body}}}}}}}'

# Removes the HTML tags reviewers' comments are built from, by name, and
# nothing else: `Vec<T>`, `<ref>` and `a < b` survive because `T`, `ref` and a
# bare `<` are not in the list. A finding that quotes one of these exact tag
# names as code loses that tag — rare, and the thread on GitHub has the text.
strip_html() {
    sed -E \
        -e 's#</?(img|details|summary|pre|code|b|i|u|br|hr|p|a|table|thead|tbody|tr|td|th|div|span|sup|sub|strong|em|h[1-6]|ul|ol|li|blockquote)( [^>]*)?/?>##g' \
        -e '/^[[:space:]]*$/d'
}

after=""
while :; do
    if [ -n "$after" ]; then
        page=$(gh api graphql -f query="$query" -f o="$owner" -f n="$name" -F p="$pr" -f after="$after") || {
            echo "wait-qodo: listing threads failed" >&2
            exit 3
        }
    else
        page=$(gh api graphql -f query="$query" -f o="$owner" -f n="$name" -F p="$pr") || {
            echo "wait-qodo: listing threads failed" >&2
            exit 3
        }
    fi
    if [ "$bodies" = "1" ]; then
        printf '%s' "$page" | jq -r '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false) | "=== \(.id) \(.path):\(.line) (\(.comments.nodes[0].author.login))\n\(.comments.nodes[0].body)"' | strip_html
    else
        printf '%s' "$page" | jq -c '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false) | {id,path,line,author:.comments.nodes[0].author.login}'
    fi
    next=$(printf '%s' "$page" | jq -r '.data.repository.pullRequest.reviewThreads.pageInfo | if .hasNextPage then .endCursor else "" end')
    [ -n "$next" ] || break
    after=$next
done
exit 0
