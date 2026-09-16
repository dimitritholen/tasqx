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

## Every change to main goes through a pull request

`main` is protected: pull request required, linear history, twelve required
status checks, no force pushes. Never merge a branch into `main` locally and
never push to `main` directly, even when the push would be accepted. The flow
is: branch from `main` as `task/<id>-<slug>`, commit there, push the branch,
open a PR with `gh pr create`, wait for the checks, and merge through the PR
(`gh pr merge --rebase` keeps the history linear). Delete the branch after
the merge and pull `main` before starting the next task.

## Releases and pushes are the user's call

Do not push tags, create GitHub releases or `cargo publish` unless the user has
asked for that release in this conversation. The guard blocks `gh release` and
`cargo publish` outright. The release procedure is in `CONTRIBUTING.md`.

## Task tracking

Work is tracked in tasqx itself — see `.claude/skills/tasqx-workflow/SKILL.md`.
No `TODO.md`, no checklists, plans or session notes committed to the repo; they
are what the tasqx backlog and its annotations are for.

## Docs are public

This repository is public. Everything under `docs/` is read by users or
maintainers: `docs/wiki` and `docs/guides` for users, `docs/maintainers` for
people running the project. Do not add research notes, review reports, specs or
implementation plans as files — record the ruling in `DESIGN.md` §12 and the
working detail in tasqx annotations.
