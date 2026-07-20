# Typed command and domain boundaries — design

**Date:** 2026-07-20
**Status:** approved for implementation
**Scope:** Medium #5 only: behavior-neutral core/CLI decomposition and typed mutation boundaries.

## Decision

`Engine` remains the concrete owner of one SQLite connection and the public JSON method façade remains compatible. Its implementation is split into child modules by cohesive domain:

- `task` — task lifecycle, live snapshots, task reads and projections;
- `projects` — project creation, selection, listing and archive policy;
- `relationships` — tags, annotations and dependency edges;
- `transfer` — canonical store export/import;
- `reports` — summary aggregation.

Events, reminder firing, capabilities, connection-level lookup helpers, and shared wire helpers remain in `engine.rs`. Child modules use the same concrete `Engine`, SQLite transaction, and helpers; no repository trait or dependency-injection layer is introduced.

All mutations acquire a `MutationContext` through `Engine::begin_mutation`. The context owns the `BEGIN IMMEDIATE` transaction, dereferences to the concrete transaction for existing SQL, and is the sole commit path. A source-level guard enumerates public mutation handlers and refuses authoritative task/project reads before `begin_mutation`; parsing and value validation may still happen first because they do not inspect store state.

The simple task lifecycle boundary (`start`, `stop`, `cancel`, `reopen`) gains typed request and response structs. Raw `serde_json::Value` is parsed once in the public wire wrapper, then internal policy operates on typed task references/flags and returns typed results that convert back to the frozen JSON shape. Complex modify/import documents remain JSON-shaped in this slice; typing them safely requires dedicated domain types rather than a cosmetic wrapper around `Value`.

The CLI moves `Cli`, `Command`, and its nested Clap subcommand enums into `command.rs`. That module owns declaration/parsing types only. Execution matching, transport selection, rendering, configuration, daemon/watch/MCP orchestration, browser launching, and process exit remain in `lib.rs` and existing cohesive modules.

## Compatibility strategy

- Public method names, params, envelopes, result shapes, error codes, and exit codes do not change.
- Existing contract tests remain the primary acceptance suite.
- The params drift guard reads every engine domain source rather than assuming all handlers live in `engine.rs`.
- Extraction commits are mechanical first; typed lifecycle conversion lands separately under focused tests.

## Alternatives

A generic repository/service layer was rejected because there is one SQLite implementation and no second consumer. Typing every API document at once was rejected because import/modify have intentionally tolerant and tri-state wire semantics that deserve focused designs. Splitting solely by line count was rejected; the chosen modules follow business capabilities and transaction ownership.

## Verification

- Source guards pin mutation lock ordering and dispatch/params coverage across modules.
- Existing lifecycle, project, relationship, report, export/import, CLI parse, JSON contract and integration suites pass unchanged.
- Full workspace tests, Clippy with warnings denied, and diff checks.
