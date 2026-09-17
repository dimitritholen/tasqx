#!/usr/bin/env bash
#
# Reply to one PR review thread and resolve it (land-tasqx-pr step 4).
#
#   .claude/skills/land-tasqx-pr/scripts/reply-resolve.sh <thread-id> <body>
#
#   reply-resolve.sh PRRT_kwDOTcoVP86jQ1jE "Fixed in d2e5a30: <one line>"
#
# Prints `true` when the thread is resolved. The reply is posted first, so a
# failed reply never leaves a thread resolved without its answer; exit 1 if
# either call fails, 2 on a usage error.
#
# The body travels as a GraphQL variable, so quotes, backticks and newlines in
# it are safe — an inline mutation with the body spliced into the query string
# breaks on the first quote, and the worktree-isolation guard refuses the
# quoted form anyway.
#
# Only for threads from automated reviewers (qodo-code-review, coderabbitai).
# A human's thread is the user's to answer: stop and show it instead.
set -euo pipefail

# Usage text is lines 3-19 of this file.
if [ "$#" -ne 2 ] || [ -z "$1" ] || [ -z "$2" ]; then
    sed -n '3,19p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

gh api graphql \
    -f query='mutation($t:ID!,$b:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$t,body:$b}){comment{id}}}' \
    -f t="$1" -f b="$2" --jq .data >/dev/null

gh api graphql \
    -f query='mutation($t:ID!){resolveReviewThread(input:{threadId:$t}){thread{isResolved}}}' \
    -f t="$1" --jq .data.resolveReviewThread.thread.isResolved
