# Shell completion — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `tasqx` completes its commands, aliases, flags, fixed value sets, task ids, project names, tags and filter tokens in bash, zsh, fish, PowerShell and elvish on Windows, Linux and macOS — with the value lookups reading live data and never creating, migrating or writing anything.

**Architecture:** `clap_complete`'s `unstable-dynamic` `CompleteEnv` intercepts at the top of `run()`, before the argv pre-pass. Candidate providers attached to individual clap args fetch live values through a hard-guarded read path: daemon if reachable, else a strictly read-only SQLite open, all inside a 150 ms budget, degrading to zero candidates and exit 0 on every failure. A new `completions` verb prints or installs the shell activation line.

**Tech Stack:** Rust 2021, clap 4.6.1 (derive), `clap_complete` 4.6.7 pinned exactly with `unstable-dynamic`, `rusqlite` (already present), no other new dependencies.

**Spec:** `docs/specs/2026-08-01-shell-completion-design.md`

## Global Constraints

- One new crate dependency only: `clap_complete`, pinned `=4.6.7`. Nothing else.
- The `$COMPLETE` callback path must **never** create a database, migrate a schema, or write user data. This is asserted against the filesystem in Task 9, not merely intended. The stronger form this constraint used to carry — *"no file may appear as a result of a Tab press"* — is **false, and was measured to be false**: a read-only connection creates the `-shm`/`-wal` sidecars on first query (`SQLITE_OPEN_READ_ONLY` governs the database file, while the shm layer opens `-shm` with `RDWR|CREATE` first) and cannot delete them on close, because deleting them is a write. An ordinary `tasqx list` creates the same two files and *does* clean them up, so completion is not doing anything to the store that a normal read does not already do. See the spec's *Known limitation: read-only opens and the WAL*.
- The callback path must **never** print to stderr and must always exit 0, including on panic, timeout, missing store and unreachable daemon. This inverts D33 and must be documented as deliberate in `complete.rs`'s module doc, or a future review will correctly flag it as a silent-drop defect and "fix" it.
- The `completions` verb is exempt from the above: it is an ordinary command and reports errors loudly with a non-zero exit.
- Completion words must go through `argv::prepass` before reaching clap's completion engine, for the same reason `run()` does not call `Cli::parse()`.
- `cargo clippy --workspace --all-targets` must stay clean; the workspace denies warnings in CI. `cargo fmt --check` must pass.
- MSRV is 1.95 and is measured, not read. If `clap_complete` raises the real floor, re-measure and update the comment rather than reasoning about the number.
- Commit messages: conventional-commit subject, lowercase, imperative. Bodies explain *why*. **No Claude attribution trailers.**
- Work on branch `feat/shell-completion`, which already holds the spec commit.

## Note for the implementer: two things the spec asserts that must be verified first

1. **`ArgValueCompleter` receives the partial word.** The prefix dispatcher in Task 6 depends on it. Confirm the signature against `clap_complete-4.6.7/src/engine/custom.rs` before building on it; if the partial word is not available, the sugar and filter completion in Tasks 6–7 need a different shape and the spec must be amended rather than the code bent.
2. **`CompleteEnv` can be handed pre-transformed args.** Task 2 depends on `try_complete(args, current_dir)` accepting our pre-passed words rather than reading `std::env::args_os()` itself. Confirm against `src/env/mod.rs:202-210`. If it cannot, the pre-pass fix needs to happen inside the completer instead, and that changes Task 2's shape.

Both are cheap to check and expensive to assume.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/tasqx-cli/Cargo.toml` | **Modify.** The pinned `clap_complete` dependency plus the comment explaining why the pin is a guard. |
| `crates/tasqx-core/src/storage.rs` | **Modify.** `open_read_only`: `SQLITE_OPEN_READ_ONLY`, no `migrate`, error on absent path. |
| `crates/tasqx-core/src/engine.rs` | **Modify.** `Engine::open_read_only` wrapping it. |
| `crates/tasqx-cli/src/complete.rs` | **Create.** `intercept()`, `CompleteEnv` wiring, the guarded `lookup()` and its budget. Owns *how* a candidate is fetched safely. |
| `crates/tasqx-cli/src/complete/candidates.rs` | **Create.** The five providers and the prefix dispatcher. Owns *what* the candidates are. |
| `crates/tasqx-cli/src/complete/install.rs` | **Create.** The `completions` verb: print, install, uninstall, detection, the marked block. |
| `crates/tasqx-cli/src/command.rs` | **Modify.** `Completions` variant; `value_parser`/`ValueHint`; completer attachments. |
| `crates/tasqx-cli/src/lib.rs:234` | **Modify.** `complete::intercept()` as the first statement of `run()`; `mod complete;`; dispatch for `Completions`. |
| `crates/tasqx-cli/src/cmddoc.rs` | **Modify.** `COMMAND_REF` entry for `completions`. Mandatory — the existing guard fails the build without it. |
| `crates/tasqx-cli/tests/completion.rs` | **Create.** Registration, candidate, never-create and drift guards. |
| `README.md`, manual topic | **Modify.** Setup instructions per shell. |

Tasks 1–3 deliver slice 1 (commands and flags complete everywhere). Tasks 4–5 deliver slice 2 (task ids). Tasks 6–7 deliver slices 3–4 (projects, tags, sugar, filters). Task 8 delivers slice 5 (install). Task 9 is the guard suite. Task 10 is documentation.

---

### Task 1: The read-only core seam

**Files:**
- Modify: `crates/tasqx-core/src/storage.rs:34-41`
- Modify: `crates/tasqx-core/src/engine.rs:187`
- Test: `crates/tasqx-core/tests/` (a new or existing storage test file)

**Interfaces:**
- Produces: `pub fn storage::open_read_only(path: &str) -> Result<Connection, ApiError>`, `pub fn Engine::open_read_only(path: &str) -> Result<Engine, ApiError>`.

- [ ] **Step 1: Write the failing tests**
  - Opening a nonexistent path read-only returns `Err` and **does not create the file** — assert with `Path::exists` afterwards.
  - Opening an existing seeded store read-only can read tasks and projects.
  - A write attempted through a read-only connection fails rather than succeeding silently.
- [ ] **Step 2: Implement.** `Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)`. Call `configure`'s safe parts only — **do not** `pragma_update(journal_mode, WAL)` on a read-only connection, and do not call `migrate`. Set `busy_timeout` low (the caller has its own budget; a long busy wait inside a keystroke is the failure this is meant to avoid).
- [ ] **Step 3: Verify.** `cargo test -p tasqx-core`, clippy clean.
- [ ] **Step 4: Commit** — `feat(core): add a read-only store open that never creates or migrates`.

### Task 2: `CompleteEnv` wired, with the pre-pass fix

**Files:**
- Modify: `crates/tasqx-cli/Cargo.toml`
- Create: `crates/tasqx-cli/src/complete.rs`
- Modify: `crates/tasqx-cli/src/lib.rs:234`

**Interfaces:**
- Produces: `pub(crate) fn complete::intercept()`.
- Consumes: `argv::prepass`, `command::Cli::command()`.

- [ ] **Step 1: Verify the two upstream assumptions** listed in the note above. Record what was found in the module doc.
- [ ] **Step 2: Write the failing test.** `COMPLETE=bash <binary>` emits a non-empty registration containing the binary name; without `$COMPLETE` the binary behaves exactly as before (a plain `tasqx --version` still works).
- [ ] **Step 3: Implement.** Add the pinned dependency with its guard comment. Write `complete.rs` with the module doc that states the inverted-D33 failure policy **and why**. `intercept()` builds `CompleteEnv::with_factory(command::Cli::command).bin("tasqx")`, runs the words through `argv::prepass`, and completes.
- [ ] **Step 4: Wire it** as the first statement of `run()`, before `argv::prepass`.
- [ ] **Step 5: Verify.** Full suite green — this touches the entry point of every command, so a regression here breaks everything. Confirm all five shells emit a registration.
- [ ] **Step 6: Commit** — `feat(cli): intercept shell completion before the argv pre-pass`.

### Task 3: Fixed value sets and path hints

**Files:**
- Modify: `crates/tasqx-cli/src/command.rs`

- [ ] **Step 1: Write the failing test.** `tasqx add x --priority bogus` exits non-zero with a message naming the accepted values.
- [ ] **Step 2: Implement.** `value_parser` for `--priority` (`H|M|L` plus the long spellings the help text already promises at `command.rs:112`). `ValueHint::FilePath`/`DirPath` on every path-taking arg — `out` at `command.rs:440`, `--transcript-path`, import/export paths. Audit the whole tree for other closed vocabularies rather than fixing only the two named here.
- [ ] **Step 3: Verify.** Existing tests still pass — a `value_parser` can reject inputs that examples in `COMMAND_REF` rely on, and the executable-examples guard will catch it.
- [ ] **Step 4: Commit** — `feat(cli): declare closed value sets and path hints so shells can complete them`.

### Task 4: The guarded lookup

**Files:**
- Modify: `crates/tasqx-cli/src/complete.rs`

**Interfaces:**
- Produces: `fn lookup<T>(f: impl FnOnce(&mut Backend) -> Option<T> + Send) -> Option<T>`, or equivalent.

- [ ] **Step 1: Write the failing tests.** A lookup against an absent store returns `None` and creates no file. A lookup whose closure sleeps past the budget returns `None`. A lookup whose closure panics returns `None` rather than unwinding out.
- [ ] **Step 2: Implement.** `$TASQX_NO_COMPLETE_LOOKUP` short-circuit; daemon `try_connect`; read-only fallback; worker thread with `recv_timeout` at 150 ms; `catch_unwind` around the provider. Comment the detached-thread-on-timeout decision explicitly, including why it is acceptable *here specifically* — this codebase does not otherwise leak threads and the exception needs its reason attached.
- [ ] **Step 3: Verify.** `cargo test -p tasqx-cli`, clippy clean.
- [ ] **Step 4: Commit** — `feat(cli): add the budgeted, never-writing completion lookup`.

### Task 5: Task-id candidates with titles

**Files:**
- Create: `crates/tasqx-cli/src/complete/candidates.rs`
- Modify: `crates/tasqx-cli/src/command.rs`

- [ ] **Step 1: Write the failing test.** Against a seeded temp store, the completer for `done`'s positional emits the seeded task's `short_id` with its title as help text.
- [ ] **Step 2: Implement.** The provider calls `task.list`, maps rows to `CompletionCandidate::new(short_id).help(title)`, caps at 200. Attach `ArgValueCandidates` to the id positional of `done`, `start`, `stop`, `show`, `modify`, `annotate`, `cancel`, `reopen`, `dep`, `undep`.
- [ ] **Step 3: Verify.** Manual check in at least one real shell — this is the slice's whole point and a passing unit test does not prove a shell renders it.
- [ ] **Step 4: Commit** — `feat(cli): complete open task ids with their titles`.

### Task 6: Projects, tags, and the sugar prefix dispatcher

**Files:**
- Modify: `crates/tasqx-cli/src/complete/candidates.rs`
- Modify: `crates/tasqx-cli/src/command.rs`

- [ ] **Step 1: Write the failing tests.** `--project <TAB>` emits seeded project names. A partial word `+` in `add`'s title position emits seeded tags. A partial word `project:` emits project names. A plain title word emits nothing.
- [ ] **Step 2: Implement.** `projects()` from `project.list`; `tags()` from the `tags` field already on `task.list` rows — **do not add a `tag.list` API method**. The dispatcher reads the partial word and branches on prefix: `+`, `project:`/`proj:`, `!`, the date keys, else empty.
- [ ] **Step 3: Verify.** Confirm the quoting rules `sugar.rs` documents are respected — a project name with a space must complete to a form `add` can actually parse.
- [ ] **Step 4: Commit** — `feat(cli): complete projects and tags, including inline capture sugar`.

### Task 7: Filter grammar tokens

**Files:**
- Modify: `crates/tasqx-cli/src/complete/candidates.rs`
- Modify: `crates/tasqx-cli/src/command.rs`

- [ ] **Step 1: Write the failing test.** In `list`'s filter position, a bare partial emits the grammar keywords; `+` emits tags; `project:` emits projects. Crucially: a partial `-` emits tag exclusions rather than being read as a flag — this is what the Task 2 pre-pass fix exists for, and it is the test that proves it works.
- [ ] **Step 2: Implement.** Attach the filter completer to the filter positionals of `list`, `export`, `watch` — the same three `argv::unescape` already lists at `lib.rs:248`. Reuse that list rather than restating it if the shape allows.
- [ ] **Step 3: Verify.** Full suite; the filter grammar has its own guards that must stay green.
- [ ] **Step 4: Commit** — `feat(cli): complete the read-side filter grammar`.

### Task 8: The `completions` verb

**Files:**
- Create: `crates/tasqx-cli/src/complete/install.rs`
- Modify: `crates/tasqx-cli/src/command.rs`, `crates/tasqx-cli/src/lib.rs`, `crates/tasqx-cli/src/cmddoc.rs`

- [ ] **Step 1: Write the failing tests.** `completions bash` prints the activation line and exits 0. `completions cmd` exits 2 with the unsupported message. Installing into a temp profile twice leaves exactly one marked block. Uninstall restores the file byte-for-byte. A non-interactive `--install` refuses with a non-zero exit.
- [ ] **Step 2: Implement.** The verb, the five activation lines (**copied from `clap_complete-4.6.7/src/env/mod.rs:38-63`, not from memory**), detection with a refusal on ambiguity, the marked block with replace-not-append semantics, confirmation before writing.
- [ ] **Step 3: Add the `COMMAND_REF` entry.** The existing guard fails the build without it; add examples with the correct `RunKind` (printing is `Safe`, installing is `NoRun`).
- [ ] **Step 4: Verify.** Full suite including the executable-examples guard.
- [ ] **Step 5: Commit** — `feat(cli): add the completions verb with guided, idempotent install`.

### Task 9: The guard suite

**Files:**
- Create: `crates/tasqx-cli/tests/completion.rs`

- [ ] **Step 1: Implement the guards** described in the spec:
  - All five shells emit a non-empty registration naming the binary (catches an upstream change at the pin).
  - A seeded store yields its project, tag and task id through the real binary.
  - `TASQX_DB` at a nonexistent path: exit 0, empty stdout, empty stderr, **and no file created**.
  - `TASQX_DB` at a store that **exists**: the database file stays byte-identical and its schema unmigrated across a callback that really queries. The nonexistent-path guard above cannot reach this — that open fails before SQLite touches its WAL layer — and its absence is exactly how the false "read-only cannot create the sidecars" claim survived review. Already covered at the core seam by `crates/tasqx-core/tests/read_only.rs`; assert it end-to-end through the binary here.
  - Drift guard iterating clap's arg table: any subcommand taking a task-id positional with no completer attached fails the build. Read the arg table, as `no_declared_short_flag_is_ever_escaped` does — do not hand-keep a list.
- [ ] **Step 2: Verify** the drift guard actually fails when a completer is removed. A guard that has never failed is a guard that has not been tested.
- [ ] **Step 3: Commit** — `test(cli): guard the completion surface against drift and upstream change`.

### Task 10: Documentation

**Files:**
- Modify: `README.md`, the manual topic, `cmddoc.rs`

- [ ] **Step 1: Write** the per-shell setup instructions, the cmd.exe and nushell gaps stated plainly, and the `$TASQX_NO_COMPLETE_LOOKUP` escape hatch.
- [ ] **Step 2: Verify** the README guard (`tests/readme.rs`) stays green — it checks claims against reality.
- [ ] **Step 3: Commit** — `docs: document shell completion setup on all three platforms`.

---

## Definition of done

- `cargo test --workspace --all-targets` green on Windows, Linux and macOS.
- `cargo clippy --workspace --all-targets` clean; `cargo fmt --check` clean.
- Manual confirmation in at least bash/zsh (unix) and PowerShell (Windows) that commands, flags, and task ids actually complete in a real terminal.
- No Tab press creates a database, migrates a schema, or writes user data, proven by test — including against a store that already exists, where the database stays byte-identical. (The `-shm`/`-wal` sidecars are the documented exception; they are pinned as observed, not forbidden.)
- The inverted-D33 policy is documented at the point where a reader would otherwise file it as a defect.
