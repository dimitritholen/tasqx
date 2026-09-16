---
name: land-tasqx-pr
description: Land a tasqx task's branch on protected main — push, open the PR, wait out the twelve required checks, handle every review thread, rebase-merge without --delete-branch, reinstall and verify, then close the task once. Use it ONLY when the user explicitly asks to land, merge, ship or open the PR for a task ("open the PR and merge it", "land task #N", "merge the branch", "ship it"); a finished implementation on its own is not a request to publish it. Sighted on tasks #95, #645 and #650 — each time a step was skipped, misread or run in the wrong order and cost a retry, so follow the sequence even when a shortcut looks safe.
---

# Land a tasqx task PR

`main` in tasqx is protected: pull request required, linear history, twelve
required status checks, no force pushes, every review thread resolved before
merge. This is the checklist that gets a task's branch through that gate
without a wasted round trip. It assumes the task branch `task/<id>-<slug>`
lives in a worktree under `~/projects/worktrees/tasqx/` and that the user
asked for the landing; if they only asked for the implementation, stop after
the work is committed and ask before pushing.

Work through the steps in order — three prior runs each lost time to
skipping or reordering one of them (see "Why this order" at the end).

## Steps

1. **Push.** Confirm the last commit carries no AI attribution trailer
   (`git -C <worktree> log -1 --format=%B`), then
   `git -C <worktree> push -u origin task/<id>-<slug>`. Use `git -C` for
   every git command: the guard hook reads the declared shell cwd, not an
   in-command `cd`.

2. **Open the PR.**
   `gh pr create --head task/<id>-<slug> --base main --title "<commit subject>" --body "<problem, fix, tests, skipped>"`.
   No AI attribution in the body either.

3. **Wait for checks.** `gh pr checks <n> --watch --interval 30` until every
   required check is green. `cargo-mutants` showing "skipping" is fine, not a
   failure. Auto-merge is disabled on the repository, so there is no `--auto`
   shortcut; the merge in step 5 is explicit.

4. **Handle every review thread before merging.** A clean check run does
   not mean the reviewers are satisfied, and GitHub refuses the merge over
   any unresolved thread regardless of check status.

   List them (replace the owner, name and number):

   ```console
   $ gh api graphql -f query='{repository(owner:"dimitritholen",name:"tasqx"){pullRequest(number:26){reviewThreads(first:50){nodes{id isResolved path line comments(first:1){nodes{author{login} body}}}}}}}' --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false)'
   ```

   For each unresolved thread:
   - From an automated reviewer (`qodo-code-review`, `coderabbitai`): read
     the whole finding, verify it against the code, and decide fix or
     reject. A fix is committed in the worktree — never the primary
     checkout — pushed, and step 3 repeats against the new head. Then reply
     and resolve:

     ```console
     $ gh api graphql -f query='mutation{addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:"<thread id>",body:"Fixed in <sha>: <one line>"}){comment{id}}}'
     $ gh api graphql -f query='mutation{resolveReviewThread(input:{threadId:"<thread id>"}){thread{isResolved}}}'
     ```

     A rejected finding gets the reason in the reply, then the same resolve.
   - From a human, or from any reviewer you do not recognise: do not resolve
     it yourself. Stop, show the user the thread, and wait for their answer
     or the reviewer's.

5. **Merge.** `gh pr merge <n> --rebase` — **without `--delete-branch`.** The
   branch lives in a worktree; `--delete-branch` deletes it (remote and
   local) out from under that worktree before step 7 has checked, with
   `git cherry`, that every commit really reached `main`. Step 7 deletes
   the branch after that check.
   If GitHub answers "the base branch policy prohibits the merge" while the
   checks are green, a thread is still unresolved — go back to step 4, do
   not retry the merge blind. If it answers with an HTTP 502, or a retry
   says "Merge already in progress", the merge is usually running — another
   PR landing on `main` at the same moment makes it wait. Poll
   `gh pr view <n> --json state --jq .state` every ten seconds until it
   says `MERGED` instead of issuing the merge again.

6. **Leave the worktree, then reinstall and verify from the primary
   checkout.** A session that entered the task worktree with `EnterWorktree`
   is isolated to it: the harness refuses any git command aimed at the
   shared checkout, `git -C ~/projects/tasqx pull` included, and refuses a
   chained command it cannot verify stays inside the worktree. So first
   `ExitWorktree` with `action: "keep"` (the worktree stays on disk for
   step 7), then, from `~/projects/tasqx`, one plain command per call:

   ```console
   $ git pull --ff-only
   $ cargo install --path crates/tasqx-cli --force
   ```

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
   to 390 and proves nothing about 390. Drive a true narrow viewport through
   CDP's `Emulation.setDeviceMetricsOverride` before trusting it.

7. **Annotate, clean up by hand, then close the task exactly once.** Write
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

   A `+` line from `git cherry` means a commit did not land; stop and find
   out why before deleting anything. Then `tasqx_complete_task` with
   `evidence` (its `checks_passed` wants check ids, not bodies; the checks
   are already marked, so evidence alone is enough). Complete once — a
   second completion of a done task is refused.

8. **Show the closing card.** `tasqx_get_task` with `view: "card"`, pasted
   verbatim, with what the completion unblocked under it.

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
