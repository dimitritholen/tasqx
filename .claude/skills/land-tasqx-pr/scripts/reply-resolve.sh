#!/usr/bin/env bash
#
# Reply to one PR review thread and resolve it (land-tasqx-pr step 4).
#
#   .claude/skills/land-tasqx-pr/scripts/reply-resolve.sh <thread-id> <reply-file>
#
# Write the reply into a file first (the Write tool, not a shell echo), then
# pass its path. The text never enters shell source: a reply typed inside
# double quotes on the command line would have its backticks, `$(...)` and
# variables expanded by the calling shell before this script saw it. The
# script reads the file and sends its contents as a GraphQL variable, so any
# text arrives verbatim.
#
# Prints `true` when the thread is resolved. The reply is posted first, so a
# failed reply never leaves a thread resolved without its answer; exit 1 if
# either call fails, 2 on a usage error.
#
# Only for threads from automated reviewers (qodo-code-review, coderabbitai).
# A human's thread is the user's to answer: stop and show it instead.
set -euo pipefail

usage() {
    sed -n '3,19p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

[ "$#" -eq 2 ] || usage
thread=$1
file=$2
[[ "$thread" =~ ^[A-Za-z0-9_-]+$ ]] || usage
[ -f "$file" ] && [ -s "$file" ] || {
    echo "reply-resolve: $file is not a non-empty file" >&2
    exit 2
}

gh api graphql \
    -f query='mutation($t:ID!,$b:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$t,body:$b}){comment{id}}}' \
    -f t="$thread" -F b=@"$file" --jq .data >/dev/null

gh api graphql \
    -f query='mutation($t:ID!){resolveReviewThread(input:{threadId:$t}){thread{isResolved}}}' \
    -f t="$thread" --jq .data.resolveReviewThread.thread.isResolved
