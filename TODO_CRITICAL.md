# 🔴 Critical Priority Issues

**Source:** Code Audit 2026-07-20  
**Estimated Total Effort:** 3-5 days

---

## #1: Mutations serialize after reading, allowing stale writes and revision rollback

**Severity:** CRITICAL  
**Category:** Data integrity / concurrency  
**File:** `crates/tasqx-core/src/engine.rs:453-966`  
**Estimated Effort:** 2-4 days

### Problem

`Engine::begin` correctly uses `BEGIN IMMEDIATE`, but most methods resolve the task and derive their new state before calling it. The lock therefore serializes only the final writes, not the read-modify-write operation.

Examples:

- `task_start` reads status/revision at line 454 and begins at line 476.
- `task_done` reads the task/tags at lines 558-582 and begins at line 583.
- `task_modify` resolves and checks `expected_rev` at lines 776-793, but begins only at line 947.
- `task_cancel`, `task_reopen`, `annotation_add`, and dependency methods follow the same shape.
- `task_add` reads the inherited default project before the transaction (lines 341-342) and validates only an explicitly supplied project inside it (lines 391-393). A concurrent archive can therefore retire/clear the inherited project between those operations.

Two processes can both load revision N, then sequentially write revision N+1 from stale state. A racing `task_start` is worse: the second writer can auto-stop the first writer's newly active task (raising its rev), then overwrite it with its stale `task.rev + 1`, making the revision move backward. Racing completion of a recurring task can also spawn more than one successor because recurrence eligibility was read outside the write lock.

This violates the comments and user documentation claiming concurrent one-shot edits serialize safely. It also weakens MCP's optimistic-concurrency promise because `expected_rev` is checked before the protected snapshot.

### Acceptance Criteria

- [x] Every mutation starts `BEGIN IMMEDIATE` before reading any mutable state used to validate or calculate that mutation.
- [x] Reference resolution can operate against `&Connection`/`&Transaction` so the transaction's snapshot is authoritative.
- [x] `expected_rev` is checked inside the same transaction as the update, against the freshly loaded revision.
- [x] Revision updates use a fresh in-transaction value; stale Rust values cannot lower a revision.
- [x] The inherited default project is read and validated inside `task_add`'s transaction.
- [x] Two-connection tests deterministically cover modify/modify, start/start, done/done on a recurring task, annotation/modify, and add/archive races.
- [x] Tests assert state, revision, event count, tracked time, and recurrence-spawn count—not merely that one call returned an error.
- [x] Existing single-process behavior and exit-code contracts remain unchanged.

### Recommended Approach

1. Introduce transaction-aware loaders such as `task_by_ref(conn: &Connection, value: &Value)`; `Transaction` dereferences to `Connection`.
2. Make each public mutation validate cheap request shape first, then begin, reload authoritative rows, enforce lifecycle/revision rules, mutate, append events, and commit.
3. Use conditional SQL where a caller supplied a revision:

```sql
UPDATE tasks
SET title = ?1, rev = rev + 1, modified = ?2
WHERE id = ?3 AND rev = ?4
```

Treat zero affected rows as `conflict` after distinguishing deletion if necessary.
4. Build deterministic tests with two file-backed `Engine` connections and barriers/hooks immediately after both readers acquire their snapshots. Avoid timing-only sleeps.
5. Audit all `let task = self.resolve_ref(...)` sites mechanically; fixing only `task_modify` leaves lifecycle and relationship writes exposed.

### Files to Modify

- `crates/tasqx-core/src/engine.rs` (transaction-scoped reads and mutations)
- `crates/tasqx-core/src/storage.rs` (shared transactional primitives if useful)
- `crates/tasqx-core/tests/engine.rs` or a new `tests/concurrency.rs` (two-connection regressions)
- `crates/tasqx-core/tests/increment.rs` (preserve contract coverage)

---

## #2: Store configuration reads silently convert database errors into “unset”

**Severity:** CRITICAL  
**Category:** Error handling / data integrity  
**File:** `crates/tasqx-core/src/storage.rs:298-303`  
**Estimated Effort:** 1 day

### Problem

`get_config` returns `Option<String>` and ends the SQLite query with `.ok()`. `QueryReturnedNoRows` and every real storage error—corruption, schema damage, I/O error, or an unexpected type—become the same `None`.

Callers use `None` as legitimate state. `task_add` then creates a projectless task instead of using the configured default; `project_create` may believe no default exists and claim it; capability/project-list output can report an absent default. This is a critical-path silent failure with user-visible writes based on false state.

The CLI config-file reader deliberately has silent and strict modes, but the SQLite store is the system of record and has no equivalent recovery rationale here.

### Acceptance Criteria

- [x] `get_config` returns `Result<Option<String>, ApiError>` and only maps `QueryReturnedNoRows` to `Ok(None)`.
- [x] Every engine caller propagates real storage failures; no mutation proceeds from a fabricated “unset” value.
- [x] `default_project` is fallible, and dispatch/CLI/MCP/daemon paths preserve the `internal` error.
- [x] A deliberately damaged-schema test proves a config read error is surfaced and no task/project/event is written.
- [x] Normal absent-key behavior remains `Ok(None)` and existing output contracts remain stable.

### Recommended Approach

Use `rusqlite::OptionalExtension`:

```rust
pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>, ApiError> {
    Ok(conn
        .query_row("SELECT value FROM config WHERE key = ?1", [key], |row| row.get(0))
        .optional()?)
}
```

Propagate the result rather than hiding it behind `unwrap_or_default`. Where a read is used inside a mutation, combine this with issue #1 so the value is obtained inside the write transaction.

### Files to Modify

- `crates/tasqx-core/src/storage.rs` (fallible read)
- `crates/tasqx-core/src/engine.rs` (propagate result)
- `crates/tasqx-core/src/dispatch.rs` and CLI render paths if signatures change
- `crates/tasqx-core/tests/engine.rs` (error-path regression)

---

## Progress Tracking

- [x] Issue #1: Mutations serialize after reading
- [x] Issue #2: Store configuration errors are swallowed

**Total:** 2/2 completed and verified.

### Verification evidence (2026-07-20)

- `cargo test -p tasqx-core --test concurrency`: 6 deterministic concurrency tests passed.
- `cargo test --workspace --all-targets --no-fail-fast`: passed with zero failed targets.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed with zero warnings.
- `git diff --check`: passed.
- `cargo fmt --all -- --check`: now passes after the separately reviewed formatting-policy task; the original correctness branch remained behavior-focused.
