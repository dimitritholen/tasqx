---
name: land-tasqx-pr
description: Land a tasqx task's branch on protected main without waiting on it — push, open the PR, arm auto-merge (rebase, without --delete-branch) and move on to the next task; at the next task boundary check the PR, send failed checks or real Qodo findings back to a builder, and once it is MERGED reinstall, verify, clean up and close the task once. Use it when a task's branch has passed review under CLAUDE.md's "One task, one branch" rule, or when the user asks to land, merge, ship or open the PR for a task ("land task #N", "merge the branch", "ship it"). Ships `scripts/wait-qodo.sh` and `scripts/reply-resolve.sh` for the review threads. Sighted on tasks #95, #645, #650, #679 and #646–#652 — each time a step was skipped, misread or run in the wrong order and cost a retry, so follow the sequence even when a shortcut looks safe.
---

# Land a tasqx task PR

`main` in tasqx is protected: pull request required, linear history, twelve
required status checks, no force pushes. Auto-merge is on and open review
threads do not block a merge (both since 2026-09-18), so nobody waits on CI:
steps 1–3 arm the PR and the session moves on to the next task, and steps 4–7
run at the next task boundary. It assumes the task branch `task/<id>-<slug>`
lives in a worktree under `~/projects/worktrees/tasqx/` and has passed the
review in `CLAUDE.md` ("One task, one branch, one builder, one review").

Work through the steps in order — earlier runs each lost time to skipping or
reordering one of them (see "Why this order" at the end).

## Steps

Two helpers ship with this skill, in `.claude/skills/land-tasqx-pr/scripts/`
(`$S` below). They exist because a session that entered the task worktree
with `EnterWorktree` is isolated to it, and the isolation guard refuses an
inline `gh api graphql … --jq` with nested quotes, a `$(cat …)` inside `gh`
arguments, and chained commands as "too complex to verify":

- `$S/open-pr.sh <branch> <title-file> <body-file>` opens the PR with the
  title and body read from files, so an apostrophe or backtick in the commit
  subject cannot break or expand the command.
- `$S/wait-qodo.sh <n>` waits until Qodo has commented and nothing of its is
  still "busy", then lists the unresolved threads (every page);
  `$S/wait-qodo.sh <n> --bodies` prints each one's finding as text. Exit 3
  means Qodo had not finished or GitHub kept failing: do not merge on it.
- `$S/reply-resolve.sh <thread-id> <reply-file>` replies and resolves. Write
  the reply into a file with the Write tool first; it never goes on the
  command line, where the calling shell would expand backticks and `$(…)`
  copied from a finding.

Run commands from the worktree, one plain command per call.

1. **Push.** Confirm the last commit carries no AI attribution trailer
   (`git log -1 --format=%B`), then `git push -u origin task/<id>-<slug>`.

2. **Open the PR.** Write the title (the commit subject, one line) and the
   body (problem, fix, verification, skipped) to two files in the scratchpad
   with the Write tool, then
   `$S/open-pr.sh task/<id>-<slug> <title-file> <body-file>`.
   No AI attribution in the body either.

3. **Arm auto-merge, then move on.** Read
   `gh pr view <n> --json mergeable,mergeStateStatus` (`UNKNOWN` right after
   opening: read it again a few seconds later). A CONFLICTING PR queues no CI
   and never merges — go to the `rebase-tasqx-pr` skill, then arm the
   replacement PR. If the branch adds a D-number to `DESIGN.md` §12,
   `git fetch origin` and check it is still free on `origin/main`; if another
   session took it, `rebase-tasqx-pr` too. Then
   `gh pr merge <n> --auto --rebase` — **without `--delete-branch`.** The
   branch lives in a worktree; `--delete-branch` deletes it (remote and
   local) out from under that worktree before step 6 has checked, with
   `git cherry`, that every commit really reached `main`.
   GitHub merges once the twelve checks are green (`cargo-mutants`
   "skipping" is fine). Do not watch the checks or wait for Qodo: leave the
   task open and start the next one.

4. **At the next task boundary, check the PR.**
   First read the review threads without waiting: `WAIT_QODO_TRIES=1
   $S/wait-qodo.sh <n> --bodies`. On exit 3 Qodo has not finished: still
   run the state check below. A `MERGED` PR goes on to step 5 with
   "Qodo review unavailable" in its delivery annotation; every other state
   follows its bullet below, and the threads are read again at the next
   boundary.
   Findings from `qodo-code-review` or `coderabbitai` are advisory: verify
   each against the code, and act only on a real defect — a builder fix on
   the branch while the PR is open, a new tasqx task naming the PR and file
   once it has merged. No reply or resolve is needed; `$S/reply-resolve.sh`
   is there when the user asks for one. Findings on a generated-docs PR are
   usually a sentence true on most paths only ("never", "always", "every");
   brief the fix to state what each method does. A thread from a human, or
   a reviewer you do not recognise, goes to the user before anything else.

   Then `gh pr view <n> --json state,mergeStateStatus,autoMergeRequest` and
   `gh pr checks <n>`:
   - `MERGED` → step 5.
   - A required check failed → brief a builder with the failing output to
     fix it on the branch, in the worktree, and push. Auto-merge stays armed
     on the new head; confirm `autoMergeRequest` is not null.
   - CONFLICTING → `rebase-tasqx-pr`, then arm the replacement PR (step 3).
   - Checks still running → leave it for the next boundary.

5. **Leave the worktree, then reinstall and verify from the primary
   checkout.** A session that entered the task worktree with `EnterWorktree`
   is isolated to it: the harness refuses any git command aimed at the
   shared checkout, `git -C ~/projects/tasqx pull` included, and refuses a
   chained command it cannot verify stays inside the worktree. So first
   `ExitWorktree` with `action: "keep"` (the worktree stays on disk for
   step 6), then, from `~/projects/tasqx`, one plain command per call:

   ```console
   $ git fetch origin
   $ git merge --ff-only origin/main
   $ cargo install --path crates/tasqx-cli --force
   ```

   Not `git pull --ff-only`: on 2026-09-17 it answered "Cannot fast-forward to
   multiple branches", and a reinstall started beside it built the old main.
   Reinstall only after the merge line prints the new commits.

   Then exercise the change in-process against the real store, for example
   `tasqx api <<< '{"tasqx":"1","id":"1","method":"<method>","params":{...}}'`
   or the verb the task changed. The daemon and MCP server keep running the
   old binary until they restart, so the in-process call is the only way to
   see the new build immediately — and a new tool argument (a `view` on a
   write, say) is refused by the MCP server as an unknown key until the
   client reconnects it. A verification that fails here is a new commit on
   the branch and a new PR, not a reason to leave the task open with a
   half-landed change.

   If the task's evidence includes a narrow-viewport screenshot of
   `tasqx docs` output: headless Chrome on macOS floors `--window-size` at
   500px wide, so a `--window-size=390` screenshot is a 500px layout cropped
   to 390 and proves nothing about 390. `scripts/snap-web.mjs` drives a true
   narrow viewport through CDP (see `docs/maintainers/terminal-style.md`
   §14); use it rather than `--window-size`.

6. **Annotate, clean up by hand, then close the task exactly once.** Write
   the delivery annotation first (plain-language paragraph on top, then the
   commit hashes, the diff stat, what was deliberately skipped) and mark the
   checks with `tasqx_set_check` (`check_id`, `state: "passed"`, `evidence`).

   Do not reach for `tasqx-work --done <id>`: a rebase merge rewrites the
   commit hashes, so the task branch is never an ancestor of `main` and the
   launcher refuses with "not merged yet" every time (task #665 tracks
   that). Check by patch instead, then remove the pieces yourself, from
   `~/projects/tasqx`:

   ```console
   $ git cherry origin/main task/<id>-<slug>        # every line must start with "-"
   $ git worktree remove ~/projects/worktrees/tasqx/<id>-<slug>
   $ git branch -D task/<id>-<slug>
   $ git push origin --delete task/<id>-<slug>
   ```

   If the PR superseded earlier ones (`rebase-tasqx-pr` pushes a rebased head
   under a new remote name), delete every remote name the task used, and any
   `backup/<id>-*` branch, in the same pass.

   A `+` line from `git cherry` means a commit did not land; stop and find
   out why before deleting anything. Then `tasqx_complete_task` with
   `evidence` (its `checks_passed` wants check ids, not bodies; the checks
   are already marked, so evidence alone is enough). Complete once — a
   second completion of a done task is refused.

7. **Print one closing line, no card.** A landed task is a routine event,
   not a decision (D164). Checks ticked or left open honestly, and what the
   completion unblocked. The counts come from the completed task's checks:
   `✔ #<id> done · <passed>/<total> checks · unblocked #<next>`.

## Verifiable end

PR state is MERGED, `git log main` shows the commits, `git worktree list`
has no entry for the task, the installed `tasqx --version` reports the new
build, and the task's status is `done` with its checks marked.

## Why this order

- **Sighting 1 (task #95, PR #24):** two failed merge attempts before
  step 4 — resolving review threads before the merge — was understood. One
  review fix landed through a hand-made worktree because `tasqx-work` did
  not exist yet.
- **Sighting 2 (task #645, PR #28, and task #650, PR #27, both on
  2026-09-16):** confirmed the rest of the procedure, and both hit the same
  `--delete-branch` problem independently, which is why step 5 says not to
  pass it at all rather than work around its side effect. #645 also carried
  the CDP viewport caveat in step 6, learned after a screenshot at "390px"
  turned out to be rendered at 500px.
- **The first draft of this skill** reinstalled from a path inside the
  worktree it had just removed, and completed the task twice; the reviewer
  on its own PR (#30) caught both, which is why reinstall now precedes
  cleanup and completion happens once.
- **Sighting 3 (task #679, PR #40, 2026-09-16):** the first landing run
  from a session that had used `EnterWorktree`. Step 6's `git -C
  ~/projects/tasqx pull` was refused by the worktree isolation, a chained
  git command was refused as unverifiable, `gh pr merge` answered 502 while
  the merge was in fact waiting behind another PR, and `tasqx-work --done`
  refused after the rebase merge as it does every time. Steps 5 to 7 now
  say what worked: leave the worktree first, one plain command per call,
  poll the PR state, clean up by hand after `git cherry`.
- **Sighting 4 (tasks #646, #647, #648, #652, PRs #47, #54, #59, #61,
  2026-09-17):** seven Qodo rounds. The inline thread-listing and resolve
  commands were refused by the isolation guard every time, so the session
  wrote `wait-qodo.sh` and `reply-resolve.sh` — now shipped in `scripts/`.
  A CONFLICTING PR was waited on with no CI queued, D-numbers were taken by
  other sessions while PRs sat in review (once minutes before the merge),
  the Stop hook's gate run overlapped the session's own background run three
  times, and `git pull --ff-only` refused. Steps 3, 5 and 6 carry those.
- **2026-09-18, auto-merge:** the user found landing too slow — each task
  waited ~4 minutes of CI plus Qodo rounds before the next could start. Auto-merge
  was turned on and required conversation resolution off; the old explicit
  merge step and the reply-and-resolve round went away. The sightings above
  cite the step numbers of that older eight-step order.
