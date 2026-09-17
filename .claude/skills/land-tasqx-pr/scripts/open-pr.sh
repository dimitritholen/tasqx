#!/usr/bin/env bash
#
# Open a PR against main with the title and body read from files
# (land-tasqx-pr step 2).
#
#   .claude/skills/land-tasqx-pr/scripts/open-pr.sh <head-branch> <title-file> <body-file>
#
# Write both files with the Write tool first. The title is the commit subject
# and often carries an apostrophe or backticks; typed into a quoted shell
# argument it breaks the command or gets expanded, and the worktree-isolation
# guard refuses `$(cat …)` inside a `gh` call. Here the file is read inside
# the script, so any subject arrives verbatim. Prints the PR URL; exit 2 on a
# usage error, gh's status otherwise.
set -euo pipefail

if [ "$#" -ne 3 ] || [ ! -s "$2" ] || [ ! -s "$3" ]; then
    sed -n '3,13p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi
title=$(head -n 1 "$2")
[ -n "$title" ] || {
    echo "open-pr: the first line of $2 is empty" >&2
    exit 2
}
exec gh pr create --head "$1" --base main --title "$title" --body-file "$3"
