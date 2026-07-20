# Typed command and domain boundaries implementation plan

## Task 1: Centralize mutation ownership

- [ ] Add a failing source guard covering every public mutation handler and forbidding authoritative reads before lock acquisition.
- [ ] Add `MutationContext` and replace direct transaction starts with `begin_mutation`.
- [ ] Preserve immediate transaction and rollback-on-drop behavior.

## Task 2: Split the engine by domain

- [ ] Move task lifecycle/read/snapshot methods to `engine/task.rs`.
- [ ] Move project methods to `engine/projects.rs`.
- [ ] Move tags/annotations/dependencies to `engine/relationships.rs`.
- [ ] Move import/export to `engine/transfer.rs` and summary aggregation to `engine/reports.rs`.
- [ ] Update the params drift guard to scan all handler modules.

## Task 3: Type internal lifecycle commands

- [ ] Add typed task-reference/start/stop/cancel/reopen requests.
- [ ] Add typed lifecycle response structs and explicit JSON conversions.
- [ ] Keep public JSON wrappers and exact wire shapes unchanged.

## Task 4: Separate CLI declarations

- [ ] Move `Cli`, `Command`, and nested Clap enums into `command.rs`.
- [ ] Keep declaration-only dependencies in that module and execution/orchestration outside it.
- [ ] Run CLI parse/help/JSON contract tests after the move.

## Task 5: Verify and integrate

- [ ] Run focused core contract, concurrency, CLI parse/help, and JSON suites.
- [ ] Run full workspace tests, Clippy, and diff checks.
- [ ] Update Medium #5 and design status with evidence.
- [ ] Commit, fast-forward merge into `main`, verify merged state, and delete the branch.
