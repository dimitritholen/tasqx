---
name: tasqx-multi-agent-fix
description: Fix a batch of open tasqx findings with parallel builder agents — settle every design question with the user first, allocate D-numbers in merge order, run one builder per task in its own worktree cut from origin/main, review each branch with /1337:review until clean, then land each one as its own PR through land-tasqx-pr. Use it when the user hands over several tasks at once ("fix all these tasks with sub-agents and a reviewer", "orchestrate the open tasqx tasks", "run builders on #12, #13 and #14 in parallel"). Not for a single task — CLAUDE.md's one-task loop covers that. Sighted on task #11 (merge 0f903c1); written up as task #61.
---

# Fix a batch of tasqx findings with parallel builders

The orchestrator (the main session) stays on `main` in `~/projects/tasqx` and
never edits the repo; builders do. Each task still follows CLAUDE.md's "one
task, one branch, one builder, one review" — the batch only runs several of
those loops side by side, so nothing here replaces a per-task rule.

## Steps

1. **Settle every design question before launching.** Brief each task
   (`tasqx_brief_task`), list the questions a builder would otherwise guess
   at, and ask the user. Write each ruling into that builder's brief and
   onto the task as a ruling annotation. A builder that meets an unsettled
   question stops and reports; it does not pick.

2. **Cut the batch into branches.** One task, one branch
   `task/<id>-<slug>`. Tasks that edit the same function or file region
   (#13 and #14 both in `chart.rs`) go to one builder in sequence, not two in
   parallel. Order the branches by dependency: that is the merge order.

3. **Allocate D-numbers in merge order.** Read
   `git show origin/main:DESIGN.md | grep -E '^### D[0-9]+' | tail -1` and
   give the first branch to merge the next number, the second the one after.
   Put the number in the brief. A number is only claimed when its PR merges,
   so step 6 checks it again.

4. **Brief and launch the builders**, two or three at a time, each with
   `isolation: "worktree"` or told to create
   `~/projects/worktrees/tasqx/<id>-<slug>` from `origin/main`. Do not
   `EnterWorktree` yourself while they run: the isolation applies to their
   Bash too. Each brief starts with `tasqx show <id> --card`, then carries:
   - the files, the ruling, the D-number, and the verification;
   - test first: a test watched fail against the original code (CONTRIBUTING.md);
   - dev builds carry `TASQX_DB=<scratch>/tasks.db` and `--no-daemon` inline
     in the same command (`.claude/guard.sh` refuses otherwise); the installed
     `tasqx` is real tracking and stays read-only to builders;
   - `cargo` comes from `~/.cargo/env` when not on PATH; one plain command
     per call (zsh, and the isolation guard refuses chained or nested quotes);
   - any docs, `--help`, MCP or screen text change re-captures BOTH corpora in
     the same commit: `scripts/docs-capture.sh` and
     `scripts/docs-capture.sh --dir=crates/tasqx-cli/docs-fixtures/baseline`
     (each with `TASQX=target/debug/tasqx … --no-daemon`), and `--check` both;
   - narrow-viewport screenshots through `scripts/snap-web.mjs`, not
     `--window-size`;
   - all four gates from CONTRIBUTING.md with `--no-fail-fast`, commit in the
     repo's style citing the task, no AI attribution, no push until told.

5. **Review each branch as it finishes.** Read `git diff origin/main...<branch>`,
   run `/1337:review`, and check the new tests bite: with the non-test changes
   reverted they must fail. Send findings back to the same builder
   (SendMessage keeps its context) and review again after every new commit —
   `.claude/review.sh` blocks the turn while an unpushed `task/*` branch has
   a commit newer than the last review. Repeat until clean and gated.

6. **Land each branch on its own, in merge order.** Before the push,
   `git fetch origin` and re-read main's highest D heading; if the number was
   taken, the builder renumbers and rebases locally (`rebase-tasqx-pr` steps
   1–5) — a local rebase before the first push is fine. Then the builder
   pushes, and you follow `land-tasqx-pr` from step 2, with
   `GH_TOKEN="$(gh auth token --hostname github.com --user dimitritholen)"`
   on every `gh pr create` and `gh pr merge --auto --rebase` — never
   `gh auth switch`. Move on to the next branch without waiting on CI.

7. **When main moves under an open PR** (CONFLICTING, or its D-number taken),
   the owning builder runs `rebase-tasqx-pr`: renumber, rebase, re-gate, push
   under a new name (`task/<id>-<slug>-rebased`), and you open the
   replacement PR and close the old one. Never force-push a pushed task
   branch.

8. **Close each task at the next task boundary**, per `land-tasqx-pr` steps
   4–7: MERGED, reinstall, verify, delivery annotation, cleanup, complete
   once. Before deleting a branch, `git cherry origin/main <branch>`; a `+`
   line after a rebase merge can be the same change with shifted context, so
   compare the changed lines (`git show <sha>` against the merged commit on
   main) and delete only when every one of them is on main. Check
   `scratchpad/tok` is gone if a builder parked a token there.

## Verifiable end

Every task's PR is MERGED, `git log origin/main` shows each change with its
D-number in ascending merge order, both docs-capture `--check` runs pass on
main, `git worktree list` shows no batch worktree, no `task/<id>-*` branch is
left local or remote, the installed `tasqx --version` is the new build, and
every task is `done` with its checks marked.

## Why this order

- **Sighting 1 (task #11, workflow wf_bd5b0624-aee, merge 0f903c1), recorded
  on #61:** #11's UTC question would have stayed open without asking first
  (step 1); D132–D135 were pre-assigned so parallel `DESIGN.md` §12 appends
  did not collide (step 3); #13 and #14 shared `chart.rs` and went to one
  author (step 2); agents needed environment facts they could not infer
  (step 4).
- **Where #61 and the current rules disagree, the rules win:**
  - #61 merged every branch onto one integration branch and the orchestrator
    pushed the trial merge to main. `main` is now protected (PR, linear
    history, twelve checks) and CLAUDE.md wants one PR per task with
    auto-merge, so steps 6–8 land each branch alone and there is no
    integrator or completeness-critic agent; the per-branch review covers
    each task against its ruling.
  - #61 allowed one fix round on blocking findings. CLAUDE.md and
    `review.sh` require review until clean, after every commit (step 5).
  - #61 pre-assigned D-numbers once. Other sessions take numbers while PRs
    wait (land-tasqx-pr sighting 4, rebase-tasqx-pr on #647 and #648), so
    step 3 allocates in merge order and step 6 re-checks before each push.
  - #61's "re-render affected screens" is now a hard rule over two corpora:
    the site fixtures and `docs-fixtures/baseline`.
  - #61's builders never pushed. In the agent-worktree practice (2026-09-21)
    the orchestrator's guard refuses repo writes from the main session, so
    the builder pushes once its branch is reviewed, and owns any rebase.
  - #61 trial-merged onto the moved main and pushed main itself. A moved
    main is now handled per PR by `rebase-tasqx-pr` with a
    replacement branch; `guard.sh` would let `--force-with-lease` through
    on a task branch, but a pushed branch is never rewritten.
  - #61 removed merged branches outright. Rebase merges rewrite hashes, so
    cleanup goes through `git cherry` and a line-level compare (step 8).
  - #61 did not name the gh account. A parallel session switched the active
    account to a read-only one on 2026-10-07, so `GH_TOKEN` is set per
    command (step 6).
