#!/usr/bin/env bash
#
# Carry the D117 house style to one FAMILY of terminal screens per run,
# unattended.
#
#   scripts/screen-sweep.sh                # every family still pending
#   scripts/screen-sweep.sh tables charts  # only these
#
# One `claude -p "/goal ..."` per family, sequentially. Separate runs rather
# than one goal over all of them, for one reason: `/goal` clears itself on a
# context overflow auto-compaction cannot resolve, and six families of
# rendering, auditing and reviewing in a single context will reach that. A
# fresh context per family turns "the night died" into "one family died".
#
# The runs share one worktree and one branch, so a later family builds on an
# earlier one's renderer changes and the morning is a single diff. Nothing is
# pushed and `main` is never checked out; the worktree keeps your own checkout
# free while this runs.
#
# UNATTENDED RUNS NEED A PERMISSION DECISION FROM YOU.
#
# With no extra flags, each run stops at the first tool call your settings do
# not already allow — which, overnight, means it stops. Two ways to grant it,
# in falling order of how much you keep:
#
#   1. Widen `permissions.allow` in .claude/settings.json to the commands this
#      work needs (cargo, git add/commit, scripts/snap.sh, freeze, chrome) and
#      run with nothing extra. Slowest to set up, and the only option where a
#      command nobody anticipated still stops and waits.
#
#   2. SWEEP_CLAUDE_ARGS=--dangerously-skip-permissions scripts/screen-sweep.sh
#      Every tool call runs. What still stands between that and your machine:
#      .claude/guard.sh (the PreToolUse hook, which refuses a dev-build tasqx
#      without a scratch store), the worktree, and the playbook's rule against
#      pushing. That is real but it is not a sandbox. Decide deliberately.
#
# There is no default here on purpose.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
tree=$root/.claude/worktrees/screen-sweep
branch=style/screen-sweep
logs=$root/target/sweep-logs
playbook=docs/agents/carry-the-style.md

# A run is a FAMILY, not a verb. `list` and `agenda` share one renderer and so
# were one design problem; the same is true of the twelve lifecycle echo lines,
# of the three charts, of the tables that are not the task table. Sweeping verb
# by verb would solve the same problem up to twelve times and let the answers
# drift apart, which is the thing the house style exists to stop.
#
# task id : family : the surfaces in it, and the files it may touch
SCREENS=(
    "346:tables:tasqx projects, report, config list, theme list, theme show, tokens, memory search/list — every table that is NOT the task table. crates/tasqx-cli/src/render.rs (project_table, report, tokens_recompute), crates/tasqx-cli/src/settings.rs (render_config_table, theme list/show), crates/tasqx-cli/src/verbs.rs (memory)"
    "347:detail:tasqx show, next, why, and the card add echoes — the single-task answers. crates/tasqx-cli/src/render.rs (task_detail, task_added_card, next_task, why)"
    "348:echoes:the one- and two-line answers every write verb prints — start, stop, done, cancel, reopen, modify, tag, untag, dep, undep, annotate, unannotate, undo, init, use, archive, import, export. crates/tasqx-cli/src/render.rs (started, stopped, done, status_line, modified, tag_result, dep_result, annotated, annotation_removed, undone, project_created, default_switched, project_archived), crates/tasqx-cli/src/verbs.rs"
    "349:charts:tasqx chart throughput, heatmap and burndown. crates/tasqx-cli/src/chart.rs"
    "350:tui:tasqx dashboard, pick, and the settings screen — all three need a pty, see the playbook. crates/tasqx-cli/src/tui/dashboard.rs, tui/pick.rs, tui/settings.rs, dashboard_screen.rs, pick_screen.rs"
    "351:manual:tasqx manual, its table of contents and every section. crates/tasqx-cli/src/manual.rs, crates/tasqx-cli/src/cmddoc.rs"
)

want=("$@")
mkdir -p "$logs"

if [ ! -d "$tree" ]; then
    echo "creating worktree $tree on $branch"
    git -C "$root" worktree add -b "$branch" "$tree" main || exit 1
fi

for entry in "${SCREENS[@]}"; do
    id=${entry%%:*}
    rest=${entry#*:}
    screen=${rest%%:*}
    about=${rest#*:}

    if [ ${#want[@]} -gt 0 ]; then
        match=no
        for w in "${want[@]}"; do [ "$w" = "$screen" ] && match=yes; done
        [ "$match" = yes ] || continue
    fi

    log=$logs/$screen.log
    echo "=== $screen (task #$id) -> $log"

    read -r -d '' condition <<EOF
The $screen family of tasqx screens carries the house style, and you have
printed the proof. Every surface in the family, not a representative one. Work only in $tree, on branch $branch. The tasqx task is
#$id. The family: $about

FIRST read $playbook and follow it exactly. It is the procedure, its acceptance
bar is the bar, and its stop rules are binding. It points at
docs/terminal-style.md, which is the style itself — read that in full too,
every time, rather than working from memory.

The condition is met when you have printed, in ONE message, all five of these
under exactly these labels, and then the line SCREEN DONE on its own:

AUDIT: thirteen lines, one per rule in docs/terminal-style.md, each marked
carried / not applicable / deliberately not, with a reason for the last two.
BITES: the table proving every guard you added fails against the old behaviour.
GATES: the four gate commands and their result lines, pasted.
REVIEW: the verdict of a fresh reviewer subagent, verbatim, run on the
strongest model available and told to refute rather than review.
COMMIT: the sha of the single commit holding this screen.

Also print SCREEN DONE, with whatever you have and a sentence naming which rule
fired, if you reach turn 20 or if any stop rule in the playbook fires. Stopping
early and saying why is a result. Do not keep working to avoid saying it.

Never push, never merge, never switch to main, never edit a test so it passes,
never rewrite the Contract table in docs/terminal-style.md to match new code,
and never claim a new D-number.
EOF

    # Permissions are yours to grant, not this script's to assume. With
    # SWEEP_CLAUDE_ARGS unset the run asks about tool calls your settings do
    # not already allow, which means it stalls the moment you stop watching.
    # Read the note at the top of this file before widening it.
    (
        cd "$tree" || exit 1
        # shellcheck disable=SC2086
        claude -p "/goal $condition" \
            --model "${SWEEP_MODEL:-sonnet}" \
            --output-format stream-json --verbose \
            ${SWEEP_CLAUDE_ARGS:-}
    ) >"$log" 2>&1

    echo "    exit $? · $(grep -c 'SCREEN DONE' "$log" 2>/dev/null || echo 0) sentinel(s) in the log"
done

echo
echo "branch $branch in $tree:"
git -C "$tree" log --oneline main..HEAD
echo
echo "renders in $tree/target/snaps, logs in $logs"
