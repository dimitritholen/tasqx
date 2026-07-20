# 🟡 Medium Priority Issues

**Source:** Code Audit 2026-07-20  
**Estimated Total Effort:** 6-10 days

---

## #1: List, report, and export perform unbounded full scans plus N+1 queries

**Severity:** MEDIUM  
**Category:** Performance  
**File:** `crates/tasqx-core/src/engine.rs:1007-1095`  
**Estimated Effort:** 2-3 days

### Problem

`task_list` selects every task, then runs `task_tags` and `is_blocked` once per task. Sorting and `limit` happen only after all rows and related data are loaded. `report_summary` and `store_export` repeat the same per-task pattern. A store with N tasks therefore performs roughly `1 + 2N` queries per list/report and allocates all tasks even for `limit: 1` (`tasqx next`).

The filter grammar makes a pure SQL rewrite non-trivial, but the N+1 portion does not require one. Tags and blocked state can be loaded in two set-based queries and indexed in memory. Common predicates/limits can later be pushed down only when semantically safe.

### Acceptance Criteria

- [ ] Task rows, tags, and unresolved-dependency state are loaded with a bounded number of SQL statements independent of task count.
- [ ] `task.list`, `report.summary`, and `store.export` share the snapshot-loading primitive rather than reimplementing it.
- [ ] A benchmark fixture covers at least 1k and 10k tasks with tags/dependencies.
- [ ] Query count is asserted or instrumented in a regression test.
- [ ] Existing filter, dynamic urgency, effective-status, sort, field projection, and export semantics remain byte/shape compatible.
- [ ] Further SQL pushdown is based on measured benefit; do not duplicate the full filter parser in SQL.

### Recommended Approach

Create a `TaskSnapshot { task, tags, blocked }` loader: one task query, one joined tag query, and one grouped unresolved-dependency query. Reuse it across the three surfaces. Add an optional fast path only for filters/sorts that can be proven equivalent in SQL.

### Files to Modify

- `crates/tasqx-core/src/engine.rs`
- `crates/tasqx-core/src/storage.rs` (snapshot queries)
- `crates/tasqx-core/tests/engine.rs`
- `benches/` or a documented benchmark harness

---

## #2: Watch can drop the only event that would refresh a stale screen

**Severity:** MEDIUM  
**Category:** Correctness / daemon client  
**File:** `crates/tasqx-core/src/daemon.rs:776-797`  
**Estimated Effort:** 1 day

### Problem

`Conn::request` explicitly discards event frames while waiting for a response. TTY watch reacts to an event by issuing `task.list`. If another change commits after that list's SQLite snapshot but its event is queued before the corresponding response reaches the client, `request` drops the event and returns the older list result. With no later event, watch remains stale indefinitely.

The `pending` queue stores responses, not events, so the comment that no reply is lost does not protect change notifications.

### Acceptance Criteria

- [x] Events observed during a request are retained in a dedicated pending-event inbox.
- [x] TTY watch performs another refresh when any event arrived during its prior refresh.
- [x] Non-TTY watch still emits every retained event individually; no implicit coalescing was added.
- [x] A deterministic scripted-socket test covers event -> unrelated response -> requested stale response -> retained event -> fresh response and proves the final state includes the second change.
- [x] Response IDs are correlated rather than accepting the first response frame blindly.

### Recommended Approach

Maintain a separate pending-event queue (or a generation counter) in `Conn`. `request` should correlate by request ID, queue events encountered en route, and let watch drain/coalesce them after rendering.

### Files to Modify

- `crates/tasqx-core/src/daemon.rs`
- `crates/tasqx-cli/src/lib.rs` (`run_watch` / `watch_render`)
- `crates/tasqx-core/tests/daemon.rs`

---

## #3: Daemon background failures are silently ignored and have no health surface

**Severity:** MEDIUM  
**Category:** Error handling / operability  
**File:** `crates/tasqx-core/src/daemon.rs:247-287`  
**Estimated Effort:** 1-2 days

### Problem

The event path turns `MAX(rowid)` errors into 0, returns silently on prepare/query errors, and drops individual row-mapping errors with `filter_map(Result::ok)`. The accept loop retries every non-`WouldBlock` listener error forever without logging or returning it. Serialization/flush/join errors are also discarded.

A permanent database/listener failure can therefore leave the process “running” while pushes stop or the server accepts nothing. There is no health/status endpoint or shared fatal-error state for operators and tests.

### Acceptance Criteria

- [x] Database and listener failures are classified as transient or fatal; fatal errors stop `serve_with_notifier` with component context.
- [x] Transient reminder retries are rate-limited and logged once per state transition.
- [x] Pump advances no watermark past a row it failed to decode and exposes the failure.
- [x] Background thread failures are communicated to the main serve loop through a supervisor channel.
- [x] No degraded health response is needed because required-component failures stop the daemon instead of intentionally remaining alive.
- [x] Tests inject event-query/decode failures and classify non-`WouldBlock` accept failures as contextual fatal errors.

### Recommended Approach

Make `max_event_rowid` and `pump` return `Result`. Add a small supervisor channel from poller/reminder threads to the serve loop and a structured error/backoff policy. Avoid a logging framework unless structured levels/targets are actually needed elsewhere (YAGNI).

### Files to Modify

- `crates/tasqx-core/src/daemon.rs`
- `crates/tasqx-core/tests/daemon.rs`
- `crates/tasqx-cli/src/lib.rs` (daemon diagnostics)

---

## #4: Thread-per-connection has no global admission bound

**Severity:** MEDIUM  
**Category:** Resource management / resilience  
**File:** `crates/tasqx-core/src/daemon.rs:371-382`  
**Estimated Effort:** 1 day

### Problem

Every accepted local connection spawns a reader thread, and each connection spawns another writer thread plus a 1024-element outbound channel. The queue bounds per-client message memory, but nothing bounds clients or threads. A buggy client reconnect loop—or an untrusted local process when a custom socket is broadly accessible—can exhaust threads and memory.

### Acceptance Criteria

- [ ] The daemon has a documented maximum concurrent-client policy.
- [ ] Connections beyond the bound are refused cheaply and observably.
- [ ] Idle/read/write timeouts or shutdown mechanics prevent dead clients from occupying slots forever.
- [ ] A stress test opens beyond the limit and proves memory/thread count remains bounded and existing clients keep working.
- [ ] Unix socket permissions/custom socket threat model are documented and tested where supported.

### Recommended Approach

Keep blocking I/O; a Tokio rewrite is not justified. Add a semaphore/atomic admission guard and scoped connection permit, plus timeouts supported by the transport. Revisit async only if measured client counts require it.

### Files to Modify

- `crates/tasqx-core/src/daemon.rs`
- `crates/tasqx-core/tests/daemon.rs`
- `DESIGN.md` / generated user docs

---

## #5: Large dynamic command modules obscure transaction and contract boundaries

**Severity:** MEDIUM  
**Category:** Maintainability / SOLID / testability  
**File:** `crates/tasqx-core/src/engine.rs:105-2720`  
**Estimated Effort:** 2-3 days, incrementally

### Problem

`engine.rs` is 2,864 lines and owns parsing, validation, lifecycle policy, SQL, event construction, recurrence, import/export, reporting, projection, and sorting. CLI `lib.rs` is 3,611 lines and combines clap declarations, orchestration, transport selection, rendering dispatch, daemon lifecycle, watch, MCP, config TUI, and browser launch. Public core methods accept and return raw `serde_json::Value`, so internal compiler guarantees stop at the API boundary even when callers are in the same workspace.

This is a Single Responsibility and Interface Segregation problem, not merely a line-count complaint. The repeated “resolve before begin” shape survived across many methods because transaction policy has no structural home. Clippy's opt-in `too_many_lines` check confirms the largest behavioral functions: `task_modify` 135 lines and `store_import` 245 lines.

### Acceptance Criteria

- [ ] Transaction ownership is centralized enough that a mutation cannot accidentally perform authoritative reads before locking.
- [ ] Typed request/response structs exist at least for internal mutation paths; JSON conversion remains at dispatch/MCP/CLI boundaries.
- [ ] Engine code is split by cohesive domain (task lifecycle, projects, relationships, import/export, reports) without changing public wire contracts.
- [ ] CLI parsing types are separated from execution/transport/render orchestration.
- [ ] No “framework” or generic repository abstraction is introduced without two real consumers.
- [ ] Each extraction lands with unchanged contract tests; do not combine it with product behavior changes.

### Recommended Approach

Use issue #1 as the extraction driver: introduce a small transaction-scoped mutation context first, then move cohesive methods into private modules. Prefer concrete functions/types over service locators, DI containers, or trait layers with one implementation.

### Files to Modify

- `crates/tasqx-core/src/engine.rs` plus new cohesive modules
- `crates/tasqx-core/src/dispatch.rs` (wire conversion)
- `crates/tasqx-cli/src/lib.rs` plus `command`/`execute` modules
- Existing contract/integration tests

---

## Progress Tracking

- [ ] Issue #1: Remove full-scan N+1 query behavior
- [x] Issue #2: Preserve events during watch requests
- [x] Issue #3: Surface daemon background failures
- [ ] Issue #4: Bound daemon connection resources
- [ ] Issue #5: Establish cohesive typed transaction/command boundaries

**Total:** 2/5 completed

### Issue #2 verification (2026-07-20)

- `daemon::tests::request_correlates_responses_and_retains_events_for_the_next_refresh` proves response correlation, retained event delivery, and the final refreshed state.
- `cargo test --workspace --all-targets --no-fail-fast`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `git diff --check`: passed.

### Issue #3 verification (2026-07-20)

- `background_store_failure_stops_the_daemon_with_context` proves a required background component terminates the serve loop with context.
- `pump_decode_failure_does_not_advance_the_watermark` proves failed batches do not skip event rows.
- Transition and accept-classification unit tests cover retry log suppression and fatal listener context.
- Full workspace tests, Clippy with warnings denied, and `git diff --check` passed.
