# Working on tasqx

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) first. It holds the layout, the four
gates, the things that fail the build for non-obvious reasons, the commit style
and the test-first rule, and all of it applies to you. `DESIGN.md` is the spec
and carries the decision log (§12); when anything disagrees with it, it wins.

This file is only what is specific to working here as an agent.

## Before you say something is done

Run the four gates from `CONTRIBUTING.md`, every one, with `--no-fail-fast` on
the tests. `.claude/gate.sh` runs them as a Stop hook when source, manifests,
workflows or `docs/maintainers/mutation-testing.md` are dirty, but do not treat
the hook as the check. If you could not run a gate, say so; do not report green.

## Dev builds never touch the real store

The installed `tasqx` is doing real task tracking. A dev build
(`target/debug/tasqx`, `cargo run`) must carry both `TASQX_DB=<scratch>/tasks.db`
and `--no-daemon` inline in the same command, because every shell call is fresh.
`.claude/guard.sh` refuses the call otherwise.

The installed binary is a build of this tree. After changing anything under
`crates/`, `cargo install --path crates/tasqx-cli --force` makes it reflect your
edits.

## Task tracking

Work is tracked in tasqx itself — see `.claude/skills/tasqx-workflow/SKILL.md`.
No `TODO.md`, no checklists, plans or session notes committed to the repo; they
are what the tasqx backlog and its annotations are for.

## One task, one branch, one builder, one review

Each task gets its own feature branch, `task/<id>-<slug>`, cut from `main`.
A builder agent does the implementation on that branch. When it finishes,
review the branch diff with `/ponytail:ponytail-review` plus your own judgement
on correctness, and send the findings back to the builder until the diff is
clean and all four gates pass. Then merge it to `main` without asking first.
This rule is standing permission for that merge. `main` requires a PR and the
twelve CI checks, but nobody waits on them: push the branch, `gh pr create`,
then `gh pr merge --auto --rebase`, and move straight on to the next task.
GitHub merges it when the checks pass. At the next task boundary, check the
previous PR: a failed check or a Qodo comment worth acting on goes back to a
builder on that branch; the rest is advisory and needs no reply.

## Docs are public

This repository is public. Everything under `docs/` is read by users or
maintainers: `docs/wiki` and `docs/guides` for users, `docs/maintainers` for
people running the project. Do not add research notes, review reports, specs or
implementation plans as files — record the ruling in `DESIGN.md` §12 and the
working detail in tasqx annotations.
