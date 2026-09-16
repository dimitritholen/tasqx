---
name: land-tasqx-pr
description: Land a tasqx task's branch on protected main — push, open the PR, wait out the eleven required checks, resolve bot review threads, rebase-merge without --delete-branch, clean up the worktree, reinstall, and verify against the real store. Use this whenever the user says "open the PR and merge it", "land task #N", "merge the branch", "ship it", or a task's implementation is done and just needs to go to main. Sighted on tasks #95, #645 and #650 — each time a step was skipped, misread, or run in the wrong order and cost a retry, so follow the sequence even when it looks like you could shortcut it.
---

# Land a tasqx task PR

`main` in tasqx is protected: pull request required, linear history, eleven
required status checks, no force pushes, every review thread resolved before
merge. This is the checklist that gets a task's branch through that gate
without a wasted round trip. It assumes you're on the task's worktree branch
`task/<id>-<slug>`.

Work through the steps in order — two prior runs each lost time to skipping
or reordering one of them (see "Why this order" at the end).

## Steps

1. **Push.** Confirm the last commit carries no AI attribution trailer
   (`git log -1 --format=%B`), then `git push -u origin task/<id>-<slug>`.

2. **Open the PR.**
   `gh pr create --head task/<id>-<slug> --base main --title "<commit subject>" --body "<problem, fix, tests, skipped>"`.
   No AI attribution in the body either.

3. **Wait for checks.** `gh pr checks <n> --watch --interval 30` until every
   required check is green. `cargo-mutants` showing "skipping" is fine, not a
   failure.

4. **Resolve every review thread before merging.** This is the step both
   prior runs almost skipped — a clean check run does not mean the bot
   reviewers are satisfied, and GitHub will refuse the merge over an
   unresolved thread regardless of check status.
   - List threads: `gh api graphql` with
     `pullRequest(number:n){ reviewThreads(first:50){ nodes { id isResolved comments(first:1){ nodes { path body } } } } }`.
   - For each unresolved thread from qodo-code-review / CodeRabbit: decide
     fix or reject, reply with
     `addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId, body})`
     stating the commit that fixed it or the reason it's rejected, then
     `resolveReviewThread(input:{threadId})`.
   - A fix is committed in the worktree — never the primary checkout —
     pushed, and step 3 repeats against the new head.

5. **Merge.** `gh pr merge <n> --rebase` — **without `--delete-branch`.** The
   branch lives in a worktree; `--delete-branch` deletes it (remote and
   local) out from under that worktree, and `tasqx-work --done` then finds
   nothing to clean up. Let step 6 delete the branch instead, which does it
   safely (it refuses anything dirty or unmerged, the check `--delete-branch`
   skips). If GitHub answers "the base branch policy prohibits the merge"
   while checks are green, that means step 4 was skipped — go back and
   resolve the threads, don't retry the merge blind. Auto-merge is disabled
   on this repo, so there's no `--auto` shortcut either.

6. **Clean up.** `tasqx-work --done <id>` removes the worktree and local
   branch and completes the task in one call. By hand, that's
   `git worktree remove <path>`, `git -C <primary> pull --ff-only`,
   `git branch -D task/<id>-<slug>`.

   If you (or an earlier run) already merged with `--delete-branch` and the
   worktree is gone, `--done` will report "no worktree" instead of doing its
   cleanup — that's not a bug to chase, the branch is already gone. Complete
   the task directly with `tasqx_complete_task` (MCP) or `tasqx complete
   <id>` (CLI) instead.

7. **Reinstall and verify against the real store.**
   `cargo install --path crates/tasqx-cli --force`, then exercise the change
   in-process: `tasqx api <<< '{"tasqx":"1","id":"1","method":"<method>","params":{...}}'`.
   The daemon and MCP server still run the old binary until restarted, so
   this in-process call is the only way to see the new build's behavior
   immediately.

   If the task's evidence includes a narrow-viewport screenshot: headless
   Chrome on macOS clamps `--window-size` to a 500px floor, so a
   `--window-size=390` screenshot is actually laid out at 500px and any
   "looks fine at 390" claim from it is unverified. Drive a true narrow
   viewport with CDP's `Emulation.setDeviceMetricsOverride` instead before
   trusting that evidence.

8. **Close out.** Write the delivery annotation (commit hashes, diff stat,
   what was deliberately skipped), `tasqx_complete_task` with
   `checks_passed` and evidence, then fetch the closing card.

## Verifiable end

PR state is MERGED, `git log main` shows the commits, `git worktree list`
has no entry for the task, and the task's status is `done`.

## Why this order

- **Sighting 1 (task #95, PR #24):** cost two failed merge attempts before
  step 4 — resolving bot review threads before merge — was understood. One
  review fix landed through a hand-made worktree because `tasqx-work` didn't
  exist yet.
- **Sighting 2 (task #645, PR #28, and task #650, PR #27 — both on
  2026-09-16):** confirmed the rest of the procedure, and both hit the same
  `--delete-branch` problem independently, which is why step 5 now says not
  to pass it at all rather than work around its side effect. #645 also
  carried the CDP viewport caveat in step 7, learned the hard way after a
  screenshot at "390px" was actually rendered at 500px.
