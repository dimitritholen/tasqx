# Typed command and domain boundaries implementation plan

## Task 1: Centralize mutation ownership

- [x] Add a failing source guard covering every public mutation handler and forbidding authoritative reads before lock acquisition.
- [x] Add `MutationContext` and replace direct transaction starts with `begin_mutation`.
- [x] Preserve immediate transaction and rollback-on-drop behavior.

## Task 2: Split the engine by domain

- [x] Move task lifecycle/read/snapshot methods to `engine/task.rs`.
- [x] Move project methods to `engine/projects.rs`.
- [x] Move tags/annotations/dependencies to `engine/relationships.rs`.
- [x] Move import/export to `engine/transfer.rs` and summary aggregation to `engine/reports.rs`.
- [x] Update the params drift guard to scan all handler modules.

## Task 3: Type internal lifecycle commands

- [x] Add typed task-reference/start/stop/cancel/reopen requests.
- [x] Add typed lifecycle response structs and explicit JSON conversions.
- [x] Keep public JSON wrappers and exact wire shapes unchanged.

## Task 4: Separate CLI declarations

- [x] Move `Cli`, `Command`, and nested Clap enums into `command.rs`.
- [x] Keep declaration-only dependencies in that module and execution/orchestration outside it.
- [x] Run CLI parse/help/JSON contract tests after the move.

## Task 5: Verify and integrate

- [x] Run focused core contract, concurrency, CLI parse/help, and JSON suites.
- [x] Run full workspace tests, Clippy, and diff checks.
- [x] Update Medium #5 and design status with evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
