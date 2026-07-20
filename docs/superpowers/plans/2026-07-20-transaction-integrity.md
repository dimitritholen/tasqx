# Transaction Integrity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every tasqx mutation a transactionally serialized read-modify-write operation and surface SQLite configuration read failures instead of treating them as absent state.

**Architecture:** Request-only validation stays outside the lock; every store-dependent read and derived calculation moves behind `BEGIN IMMEDIATE`. Connection-parameterized loaders work with both the engine connection and a `rusqlite::Transaction`, avoiding a repository framework. Deterministic file-backed concurrency tests hold a third write transaction so two worker operations reach the same lock boundary before release.

**Tech Stack:** Rust 1.80+, rusqlite 0.40 with bundled SQLite/WAL, serde_json, standard-library threads/channels, Cargo test and Clippy.

## Global Constraints

- Work only on `fix/transaction-integrity`; never implement on `main`.
- Preserve all public JSON shapes, API error codes, CLI exit codes, MCP schemas, and documented lifecycle semantics.
- Add no runtime dependency, async runtime, repository trait, or process-local serialization mutex.
- Follow strict red-green-refactor: each production behavior change must first be demonstrated by a failing real-store test.
- Use `BEGIN IMMEDIATE` as the cross-process serialization primitive.
- Do not mix daemon, performance, formatting, or CI remediation into this branch.

## File Structure

- Modify `crates/tasqx-core/src/engine.rs`: connection-parameterized loaders; transaction-first mutation flows; fallible default-project propagation.
- Modify `crates/tasqx-core/src/storage.rs`: fallible `get_config`.
- Create `crates/tasqx-core/tests/concurrency.rs`: deterministic two-connection regression harness and race tests.
- Modify `crates/tasqx-core/tests/increment.rs`: store-error and public-contract regression coverage where direct connection corruption is useful.
- Modify `TODO_CRITICAL.md`: mark the two accepted-slice issues only after verification.
- Modify `docs/specs/2026-07-20-transaction-integrity-design.md`: approved status only; behavior decisions already live there.

---

### Task 1: Build the deterministic race harness and prove the stale-revision defect

**Files:**
- Create: `crates/tasqx-core/tests/concurrency.rs`

**Interfaces:**
- Consumes: `Engine::open`, `Engine::conn`, public task methods, `rusqlite::Connection::busy_handler`.
- Produces: reusable `Store`, `block_writers`, `install_busy_signal`, and worker helpers used by later race tests.

- [ ] **Step 1: Add a file-backed test-store harness**

Use a unique path without adding `tempfile`:

```rust
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

struct Store { path: PathBuf }

impl Store {
    fn new(label: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "tasqx-concurrency-{label}-{}-{n}.db",
            std::process::id()
        ));
        Store { path }
    }

    fn engine(&self) -> Engine {
        Engine::open(self.path.to_str().expect("UTF-8 temp path")).expect("open test store")
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
```

Keep a module-level mutex so tests using the function-pointer busy handler cannot overlap. Use a static `Mutex<Option<Sender<()>>>` and a non-capturing `fn busy_signal(count: i32) -> bool` that sends only when `count == 0`, then returns `true`.

- [ ] **Step 2: Write the guarded modify race test**

Seed one task at `_rev == 1`. Have a blocker connection execute `BEGIN IMMEDIATE`, install the busy signal on two worker engines, and spawn:

```rust
engine.task_modify(&json!({
    "ref": 1,
    "expected_rev": 1,
    "set": { "title": title }
}))
```

Wait for two busy signals before committing the blocker. Assert exactly one `Ok`, exactly one `ErrorCode::Conflict`, final `_rev == 2`, and exactly one `modify` event.

- [ ] **Step 3: Run the single test and verify RED**

Run:

```text
cargo test -p tasqx-core --test concurrency two_guarded_modifies_from_one_revision_have_one_winner -- --exact --nocapture
```

Expected on the old implementation: FAIL because both calls return `Ok` after checking revision 1 before either transaction begins.

- [ ] **Step 4: Commit the red regression test**

```text
git add crates/tasqx-core/tests/concurrency.rs
git commit -m "test(core): expose stale concurrent task writes"
```

---

### Task 2: Move reference resolution and guarded modify inside the write transaction

**Files:**
- Modify: `crates/tasqx-core/src/engine.rs:130-196`
- Modify: `crates/tasqx-core/src/engine.rs:775-967`

**Interfaces:**
- Produces:
  - `fn resolve_ref_on(&self, conn: &Connection, p: &Value) -> Result<Task, ApiError>`
  - `fn resolve_ref_value_on(&self, conn: &Connection, r: &Value) -> Result<Task, ApiError>`
  - `fn task_by_short_on(&self, conn: &Connection, short_id: i64) -> Result<Task, ApiError>`
  - `fn task_by_id_on(&self, conn: &Connection, id: &str) -> Result<Task, ApiError>`
- Preserves thin read-only wrappers using `&self.conn` where helpful.

- [ ] **Step 1: Parameterize the loaders over a connection view**

Change SQL calls from `self.conn.query_row(...)` to `conn.query_row(...)`. Keep the current not-found mapping exactly. Make existing read-only `resolve_ref` delegate:

```rust
fn resolve_ref(&self, p: &Value) -> Result<Task, ApiError> {
    self.resolve_ref_on(&self.conn, p)
}
```

- [ ] **Step 2: Make `task_modify` transaction-first for store state**

Parse and validate the `set` object before locking, but do not read task status, fields, revision, project existence, or derived urgency yet. Then:

```rust
let tx = self.begin()?;
let task = self.resolve_ref_on(&tx, p)?;
if let Some(exp) = expected_rev {
    if exp != task.rev { return Err(ApiError::conflict(...)); }
}
// derive assignments from the locked task, validate project through &tx,
// update, append event, commit
```

Read `expected_rev` into an `Option<i64>` before the transaction because it is request-only data; compare it only afterward.

- [ ] **Step 3: Run the guarded race test and verify GREEN**

Run the exact Task 1 command. Expected: PASS with one success, one conflict, revision 2, one event.

- [ ] **Step 4: Run focused modify contracts**

```text
cargo test -p tasqx-core task_modify -- --nocapture
cargo test -p tasqx-cli modify -- --nocapture
```

Expected: all selected tests pass.

- [ ] **Step 5: Commit**

```text
git add crates/tasqx-core/src/engine.rs
git commit -m "fix(core): validate task edits inside write transaction"
```

---

### Task 3: Serialize lifecycle reads and prove idempotence/recurrence behavior

**Files:**
- Modify: `crates/tasqx-core/tests/concurrency.rs`
- Modify: `crates/tasqx-core/src/engine.rs:453-774`
- Modify: `crates/tasqx-core/src/engine.rs:1185-1247`

**Interfaces:**
- Consumes: Task 2 transaction-scoped reference loaders.
- Produces: transaction-first `task_start`, `task_stop`, `task_done`, `task_cancel`, and `task_reopen`.

- [ ] **Step 1: Write start/start and recurring done/done race tests**

Use the blocker harness for both tests.

For start/start assert both calls may return success/idempotent success, but final task is active at rev 2 with one `start` event and zero auto-stop events for itself.

For done/done seed a recurring task and assert one call succeeds, one conflicts, the template has one `done` event, and exactly one successor carries `spawned_from` in its add-event payload.

- [ ] **Step 2: Run both tests and verify RED**

```text
cargo test -p tasqx-core --test concurrency racing_starts_create_one_interval -- --exact --nocapture
cargo test -p tasqx-core --test concurrency racing_recurring_completions_spawn_once -- --exact --nocapture
```

Expected old behavior: extra lifecycle events/revision corruption for start; two successors or two successful completions for done.

- [ ] **Step 3: Move lifecycle task reads behind `begin()`**

For each lifecycle method, parse request-only fields first, begin, then resolve and validate. Move `task_done`'s `task_tags` call to `task_tags(&tx, &task.id)`. Return idempotent start from the locked row; dropping an untouched transaction rolls it back cleanly.

Do not change lifecycle transition sets or response JSON.

- [ ] **Step 4: Verify GREEN and focused lifecycle suite**

Run both exact tests, then:

```text
cargo test -p tasqx-core --test increment task_start -- --nocapture
cargo test -p tasqx-core --test increment task_done -- --nocapture
cargo test -p tasqx-core --test increment task_cancel -- --nocapture
cargo test -p tasqx-core --test increment task_reopen -- --nocapture
```

- [ ] **Step 5: Commit**

```text
git add crates/tasqx-core/src/engine.rs crates/tasqx-core/tests/concurrency.rs
git commit -m "fix(core): serialize task lifecycle transitions"
```

---

### Task 4: Serialize relationship, annotation, tag, and reminder mutations

**Files:**
- Modify: `crates/tasqx-core/tests/concurrency.rs`
- Modify: `crates/tasqx-core/src/engine.rs:969-1005`
- Modify: `crates/tasqx-core/src/engine.rs:1321-1432`
- Modify: `crates/tasqx-core/src/engine.rs:2203-2254`

**Interfaces:**
- Consumes: transaction-scoped task/reference loaders.
- Produces: monotonic revision behavior for `tag_add`, `annotation_add`, `dependency_add`, and `dependency_remove`; current task payload for `reminder_fire`.

- [ ] **Step 1: Write annotation/modify race test**

Race `annotation_add` and an unguarded `task_modify` from revision 1. Assert both payload changes survive, final revision is 3, and there is one event of each operation.

- [ ] **Step 2: Run test and verify RED**

```text
cargo test -p tasqx-core --test concurrency annotation_and_modify_advance_two_revisions -- --exact --nocapture
```

Expected old behavior: final revision is 2 because both methods write stale `task.rev + 1`.

- [ ] **Step 3: Move all relationship reads behind `begin()`**

Parse tags/body/reference JSON first. Begin before resolving task/target. Keep dependency cycle detection in the existing transaction. For a removed edge, increment revision only when `removed > 0`, as today.

For `reminder_fire`, parse/normalize `at` first, then begin and resolve the task before dedupe and event payload construction.

- [ ] **Step 4: Verify GREEN and relationship/reminder tests**

```text
cargo test -p tasqx-core --test concurrency annotation_and_modify_advance_two_revisions -- --exact --nocapture
cargo test -p tasqx-core dependency -- --nocapture
cargo test -p tasqx-core reminder -- --nocapture
cargo test -p tasqx-core tag -- --nocapture
```

- [ ] **Step 5: Commit**

```text
git add crates/tasqx-core/src/engine.rs crates/tasqx-core/tests/concurrency.rs
git commit -m "fix(core): serialize task relationship mutations"
```

---

### Task 5: Make default-project reads fallible and route task adds from the locked snapshot

**Files:**
- Modify: `crates/tasqx-core/tests/increment.rs`
- Modify: `crates/tasqx-core/tests/concurrency.rs`
- Modify: `crates/tasqx-core/src/storage.rs:298-303`
- Modify: `crates/tasqx-core/src/engine.rs:198-451`
- Modify: `crates/tasqx-core/src/engine.rs:1249-1319`
- Modify: `crates/tasqx-core/src/engine.rs:1582-1640`
- Modify: `crates/tasqx-core/src/engine.rs:2256-2278`

**Interfaces:**
- Produces: `get_config(...) -> Result<Option<String>, ApiError>` and `default_project(...) -> Result<Option<String>, ApiError>`.
- Preserves: absent configuration remains `Ok(None)`.

- [ ] **Step 1: Write the swallowed-error regression**

In a fresh in-memory engine, execute `DROP TABLE config`, call `task_add` without a project, and assert `ErrorCode::Internal`. Query task/event counts directly and assert both remain zero.

- [ ] **Step 2: Run the test and verify RED**

```text
cargo test -p tasqx-core --test increment config_read_failure_aborts_task_add -- --exact --nocapture
```

Expected old behavior: `task_add` succeeds because `get_config(...).ok()` returns `None`.

- [ ] **Step 3: Make configuration reads fallible**

Implement with `OptionalExtension`:

```rust
pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>, ApiError> {
    Ok(conn
        .query_row("SELECT value FROM config WHERE key = ?1", params![key], |row| row.get(0))
        .optional()?)
}
```

Add `?` at every caller. Change `default_project` to return `Result<Option<String>, ApiError>` and propagate through project list/export/capabilities rather than defaulting.

- [ ] **Step 4: Move inherited-project resolution into `task_add`'s transaction**

Parse `explicit_project` and all request fields first. Begin before choosing the destination:

```rust
let tx = self.begin()?;
let project = match explicit_project {
    Some(ref name) => {
        require_live_project(&tx, name)?;
        Some(name.clone())
    }
    None => get_config(&tx, DEFAULT_PROJECT_KEY)?,
};
if let Some(name) = &project {
    require_live_project(&tx, name)?;
}
```

Do not read a default through `self.conn` on this path.

- [ ] **Step 5: Add the ordered archive/add race test**

Use two distinct busy-handler functions backed by atomics so the add worker remains in its busy callback until the archive worker commits. Both workers must have reached blocked `BEGIN IMMEDIATE` before the initial blocker releases. Assert the archived project is not the new task's project and the default is absent.

- [ ] **Step 6: Verify GREEN and configuration/project contracts**

```text
cargo test -p tasqx-core --test increment config_read_failure_aborts_task_add -- --exact --nocapture
cargo test -p tasqx-core --test concurrency archive_winning_the_lock_prevents_inherited_routing -- --exact --nocapture
cargo test -p tasqx-core default_project -- --nocapture
cargo test -p tasqx-core project -- --nocapture
```

- [ ] **Step 7: Commit**

```text
git add crates/tasqx-core/src/storage.rs crates/tasqx-core/src/engine.rs crates/tasqx-core/tests/increment.rs crates/tasqx-core/tests/concurrency.rs
git commit -m "fix(core): make default project reads transactional"
```

---

### Task 6: Audit the mutation boundary and verify the slice

**Files:**
- Modify: `TODO_CRITICAL.md`
- Modify if needed: `crates/tasqx-core/src/engine.rs`
- Test: full workspace

**Interfaces:**
- Consumes all preceding tasks.
- Produces a verified branch suitable for human acceptance.

- [ ] **Step 1: Mechanically audit remaining mutation entry points**

Run:

```text
rg -n "pub fn (project_|task_|tag_|annotation_|dependency_|store_import|reminder_fire)|let task = self\.resolve|let target = self\.resolve|let tx = self\.begin" crates/tasqx-core/src/engine.rs
```

For each mutation, confirm all mutable store reads occur after its `begin()`. `project_create`, `project_use`, `store_import`, and existing cycle/default checks already read inside their transactions; preserve that behavior.

- [ ] **Step 2: Add a regression before fixing any missed entry point**

If the mechanical audit finds another stale read, add the smallest deterministic test to `concurrency.rs`, run it red, then move that read behind the transaction and run it green. Do not make an untested production correction.

- [ ] **Step 3: Run formatting diff inspection without rewriting house style**

```text
cargo fmt --all -- --check
```

Expected: the repository's known whole-tree formatting failure may remain. Inspect the diff to ensure newly added Rust is rustfmt-compatible or locally consistent; do not format unrelated files in this branch.

- [ ] **Step 4: Run full verification**

```text
cargo test --workspace --all-targets --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Expected: tests pass with zero failures, Clippy exits 0, and diff check emits nothing.

- [ ] **Step 5: Update critical progress truthfully**

Mark Critical #1 and #2 complete only if every acceptance criterion in `TODO_CRITICAL.md` is evidenced. Add concise verification notes with the exact commands and test names; do not mark later-severity work.

- [ ] **Step 6: Commit verification metadata**

```text
git add TODO_CRITICAL.md crates/tasqx-core/src/engine.rs crates/tasqx-core/tests/concurrency.rs crates/tasqx-core/tests/increment.rs
git commit -m "docs: record transaction integrity verification"
```

- [ ] **Step 7: Present the acceptance gate**

Hand the user exact commands and observable outcomes: one winner/one conflict, one recurring successor, monotonic revisions, no write after config failure, full test/Clippy results. Do not merge or start the daemon slice until the user explicitly accepts this branch.
