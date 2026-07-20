# Transaction integrity remediation — design

**Date:** 2026-07-20  
**Status:** implemented and verified 2026-07-20; pending human acceptance
**Scope:** first remediation slice on `fix/transaction-integrity`: Critical #1 (stale read-modify-write races) and Critical #2 (swallowed SQLite configuration errors). Daemon delivery, query performance, architectural decomposition beyond what this fix needs, and lower-severity audit work are explicitly deferred to later accepted branches.

## Problem

tasqx claims concurrent one-shot writers serialize through SQLite `BEGIN IMMEDIATE`. The transaction does acquire the write reservation immediately, but most mutations read the task before opening it. Two processes can therefore read the same revision and lifecycle state, queue at the transaction boundary, and later write sequentially from the same stale input.

The repeated shape is:

```text
resolve task on Connection       outside serialization
validate status / expected_rev   outside serialization
derive totals / next state       outside serialization
BEGIN IMMEDIATE                  writers serialize here, too late
write task.rev = stale_rev + 1
append event
COMMIT
```

Consequences include revision rollback, stale urgency/tracked-time calculations, duplicate lifecycle events, and duplicate successors when two processes complete the same recurring task. `task_add` has the analogous project race: it resolves the inherited default before the transaction and validates only explicitly supplied projects after locking.

A second failure weakens the same state boundary. `storage::get_config` calls `.ok()`, so every SQLite read error is indistinguishable from a genuinely absent configuration key. A write can then proceed from a fabricated “no default project” state.

## Decision

**Every mutation's authoritative state read, concurrency check, derived-state calculation, write, and event append form one `BEGIN IMMEDIATE` transaction. Store configuration reads are fallible and propagate real SQLite failures.**

Request-shape parsing that depends only on caller bytes may happen before the transaction to minimize lock time. Anything whose answer can change in the database must happen after `begin()`.

This is a focused correctness repair, not a repository-pattern rewrite. The public JSON API, CLI commands, MCP schemas, response shapes, and error/exit codes remain unchanged.

## Considered approaches

### Chosen: transaction-scoped loaders plus in-transaction validation

Generalize reference/config loaders to accept the connection view used for the operation. Because `rusqlite::Transaction` dereferences to `Connection`, existing row mappers and query helpers can be reused without a repository abstraction. Each mutation begins once, resolves against `&tx`, validates and derives against that row, writes, logs, and commits.

This has the smallest semantic blast radius and makes the transaction boundary visible in each command.

### Rejected: rely only on `UPDATE ... WHERE rev = ?`

Conditional updates are useful for caller-supplied `expected_rev`, but they are insufficient alone. Lifecycle validation, recurrence spawning, tracked-time calculation, auto-stop behavior, relationship checks, and default-project routing all require a fresh consistent read. Adding predicates to final updates would detect some conflicts while leaving stale side calculations and multi-row mutations exposed.

### Rejected: serialize all public methods with a process mutex

A mutex would protect only threads in one process. The important race is between separate one-shot CLI processes with separate SQLite connections. The database transaction is the cross-process serialization primitive and must own the full read-modify-write sequence.

## Transaction flow

Every mutating command follows this order:

1. Validate the JSON parameter shape and parse values that do not consult store state.
2. Start `BEGIN IMMEDIATE`.
3. Resolve references and configuration through the transaction.
4. Enforce lifecycle, relationship, project, and optional `expected_rev` rules.
5. Calculate values derived from current rows (revision, tracked time, urgency, recurrence successor, unblock set).
6. Apply all state changes.
7. Append the corresponding event rows in the same transaction.
8. Commit and build the response from values established by that transaction.

Idempotent lifecycle behavior is decided from the locked row. For example, a second `task.start` that waits behind the first sees `active` and returns the existing interval without adding another event. A second `task.done` sees `done` and returns `conflict`; it cannot spawn another recurrence instance.

## Loader boundaries

Reference resolution becomes connection-parameterized rather than implicitly tied to `self.conn`:

```rust
fn resolve_ref_on(&self, conn: &Connection, params: &Value) -> Result<Task, ApiError>;
fn resolve_ref_value_on(&self, conn: &Connection, value: &Value) -> Result<Task, ApiError>;
fn task_by_short_on(&self, conn: &Connection, short_id: i64) -> Result<Task, ApiError>;
fn task_by_id_on(&self, conn: &Connection, id: &str) -> Result<Task, ApiError>;
```

Read-only methods may keep thin wrappers that call these with `&self.conn`. Mutations call them with `&tx`. Helpers that inspect tags, dependencies, annotations, or blocking state receive a connection argument when they participate in mutation decisions.

`get_config` changes to:

```rust
pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>, ApiError>
```

Only `QueryReturnedNoRows` maps to `Ok(None)` via `OptionalExtension`; every other error propagates. Consequently `Engine::default_project` becomes `Result<Option<String>, ApiError>`, and wire-facing callers propagate an `internal` error instead of inventing absence.

## Revision semantics

- An effective task mutation increments `_rev` exactly once.
- An idempotent no-op does not create an event or increment `_rev`.
- Revisions never decrease.
- A caller-supplied `expected_rev` is evaluated against the locked row.
- Two changes without `expected_rev` may both succeed in serialized order. Each sees the prior change and produces the next revision; same-field writes remain explicit last-writer-wins behavior.
- Two changes with the same `expected_rev` result in exactly one success and one `conflict`.

Use `rev = rev + 1` where it makes the invariant clearer, but do not force every multi-column update into dynamic SQL solely for stylistic uniformity. The authoritative in-transaction row is sufficient when the update is guaranteed to target that locked snapshot.

## Default-project semantics

`task_add` reads the default only after beginning the transaction. Whether the project was explicit or inherited, a non-null destination must exist and be live in the locked snapshot. A racing archive therefore orders cleanly:

- archive first: add observes no/default-cleared project and creates a projectless task;
- add first: add files the task in the then-live project, commits, and archive follows.

No task is newly routed into a project that was already archived when its transaction obtained the write lock.

## Deterministic concurrency testing

Tests use a real temporary file-backed SQLite store and two independent `Engine` connections. Timing-only sleeps are not an acceptable proof.

To force the vulnerable interleaving without production test hooks:

1. A third SQLite connection holds `BEGIN IMMEDIATE`.
2. Install a per-test `busy_handler` on each worker connection that signals a channel/barrier the first time its command reaches the blocked `BEGIN IMMEDIATE`, then continues retrying.
3. Start both mutations on separate threads.
4. Wait until both workers report that they reached the lock boundary.
5. Commit the blocker and collect both results.

On the old implementation both workers have already read stale task/config state before reporting blocked. On the corrected implementation they have not performed the authoritative read yet. This gives the regression tests a reliable red/green distinction without sleeping or adding test-only branches to production.

Required interleavings:

- two modifies with the same `expected_rev`: one success, one conflict;
- two unguarded modifications: monotonic consecutive revisions and no lost unrelated field;
- start/start: one start event, one active interval, one revision increment;
- done/done on a recurring task: one done event and one successor;
- annotation/modify: both changes survive and revision advances twice;
- inherited add/project archive: result matches transaction order and never files new work into an already archived project;
- configuration query failure: returns `internal` and writes no task/project/event.

## Error handling

SQLite busy behavior continues to use the existing three-second timeout. Exhausting it remains an `internal` storage error in this slice; changing the public error taxonomy is out of scope.

No database error is logged and then ignored. Public methods return `ApiError`; CLI, daemon, API, and MCP surfaces retain their existing mapping. Rollback-on-drop remains the failure behavior for any error before commit.

## Scope control and module structure

The audit recommends later decomposition of `engine.rs`, but this slice introduces only the smallest helpers needed to make transaction ownership explicit and testable. It does not add traits, a generic repository, a unit-of-work framework, or an async runtime.

If the resulting helper set is cohesive enough to warrant a private module, extraction may happen as a behavior-neutral refactor after all new concurrency tests are green. It is not a prerequisite for the correctness fix.

## Acceptance contract

- All affected mutators read mutable store state only after `BEGIN IMMEDIATE`.
- Deterministic two-connection tests demonstrate the old stale-read behavior and pass with the fix.
- Revisions are monotonic and event counts match effective mutations.
- A recurring task cannot spawn twice under racing completion.
- Inherited default-project routing is transactionally consistent with archive.
- SQLite configuration read failures are surfaced and cannot drive writes.
- `cargo test --workspace --all-targets --no-fail-fast` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- Public JSON response shapes, error codes, and CLI exit codes do not change.

## Implementation verification

The mutation audit confirmed that mutable task, lifecycle, relationship, configuration, and project-state reads now occur after `BEGIN IMMEDIATE`. `project.archive` still parses the requested name and reads the project's immutable identity before beginning; project IDs cannot be changed by public mutations or import, while its mutable archived/default state is handled under the transaction.

The branch is evidenced by six deterministic file-backed concurrency tests, the deliberately damaged-config regression, the full all-target workspace suite, and Clippy with warnings denied. The repository-wide rustfmt check continues to expose pre-existing formatting drift outside this slice, so unrelated files were not reformatted. Human acceptance remains the final gate before integration or starting the next remediation branch.

## Deferred to later branches

- Watch event buffering/correlation and daemon background-health behavior.
- Daemon connection admission bounds.
- Set-based task snapshots and performance benchmarks.
- Broader `engine.rs`/CLI module decomposition.
- MCP token semantics, formatting policy, coverage, and dependency/license CI.
