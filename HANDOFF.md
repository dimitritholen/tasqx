# tasqx — Session Handoff

_Last updated: 2026-07-16 (D23 pass). `DESIGN.md` is the authoritative spec: §4 JSON API contract, §8 filter grammar, §9 notifications, §11 roadmap + build status, §11a explicit deferrals, §12 locked decisions **D1–D23**. Durable context also lives in the memory dir `C:\Users\dimitri\.claude\projects\C--dev-tasqx\memory\`._

## Current state

Verified this session by running them, not by recollection:

```
cargo clean -p tasqx-core -p tasqx-cli && cargo build --workspace   # 0 warnings, TRUE clean rebuild
cargo test --workspace                                              # 241 passed / 0 failed
```

| Test binary | Passed |
|---|---|
| `tasqx-cli` unittests (`src/main.rs`) | 93 |
| `tasqx-core` unittests (`src/lib.rs`) | 56 |
| `tests/daemon.rs` | 7 |
| `tests/engine.rs` | 14 |
| `tests/increment.rs` | 63 |
| `tests/mcp.rs` | 8 |
| **Total** | **241 / 0 failed** |

> **On test counts: measure, do not inherit.** This session's brief stated a baseline of "219 (88 cli + … + 46 increment)" and `DESIGN.md` §11 claimed 232 (92 cli + … + 55 increment). Both were wrong. A real run at session start had **230** (92 cli + 56 core-lib + 7 daemon + 14 engine + 53 increment + 8 mcp); 230 + 11 added here (10 increment + 1 docs guard) = **241**. Two independent stale numbers in two places is the same lesson the 159→219 note below records, so it is now written twice: the only true count is one you just ran. Also note `cargo test` **stops at the first failing target** — an early failure in `tests/engine.rs` hid `tests/increment.rs` entirely; use `--no-fail-fast` while iterating or you will read a partial picture.

Everything below is built **and driven end-to-end against the real `tasqx.exe`** on an isolated store, not merely test-green.

- **Core engine** — SQLite (rusqlite `bundled`, WAL, IMMEDIATE txns); every mutation writes its event row in the *same* transaction (the load-bearing invariant — keep it); UUIDv7 ids + never-recycled `short_id`. `"tasqx":"1"` JSON API, single dispatch layer.
- **Lifecycle & data** — project/task CRUD, start/stop/done/cancel/reopen, tags, dependencies + blocked/unblock (**D11**: a cancelled dep is *resolved* and releases dependents), annotations, `report.summary`, `store.export`/`import` (byte-identical round-trip), `event.list`.
- **Filters (D8)** — `project:`/`status:`/`+tag`/`due.before|after`, boolean `or`, parentheses; `due` compared as instants.
- **MCP server** — `tasqx mcp serve` (stdio JSON-RPC, 11 §7 tools); read/write scope **fails closed**; optimistic-concurrency-by-default on modify.
- **Presentation** — 5 semantic themes + full degradation (truecolor→256→16→NO_COLOR→plain/piped→legacy-Windows); terminal charts; self-contained HTML report.
- **Scheduling** — NL dates (`due:friday`, `eom`, offsets) + **D2 recurrence**; missed occurrences collapse to ONE catch-up; spawn-on-completion is transactional.
- **Daemon (§2)** — socket / Windows named pipe, runtime-free, no tokio; one-shot CLI auto-routes when a socket is present; live event push from daemon-applied *and* external writes; `tasqx watch`.
- **Notifications & reminders (§9a)** — `remind:` offsets/absolutes, daemon min-heap, additive idempotent `reminder.fire`. `notify-rust` stays behind the off-by-default `notify-os` feature and is **absent from the default cargo tree**.
- **CLI editing surface (D13/D14)** — `modify` with the same sugar/parsers as `add`; `--clear <field>`; `--expected-rev`.
- **User guide (D15)** — `tasqx docs` renders 11 pages as ONE self-contained HTML file.
- **Explicit default-project control (D21/D22)** — `init` claims the default only when the store has none; `tasqx use <project>` is the one way to move it; archiving the default clears it.
- **Every project is a project you can see (D23, this session)** — see below.

### This session: four reviewer findings on the D21/D22 `use` work — three genuine, one rejected

Each was **reproduced on the real binary before any code moved**; each fix got a test that was **watched fail against the reverted code**, then watched go green. All three genuine findings were the same shape: **D22's rule enforced at one edge and not its siblings.**

| # | Finding | Ruling | Fix |
|---|---|---|---|
| 1 | A store written by **old** code can hold `default_project` naming an **archived** project. `tasqx projects` then shows **no default at all** (the archived row is filtered out), `core.capabilities` reports the ghost, and every bare `add` lands in it. Unescapable: the user has no reason to run `use` when every surface says there is no default | **genuine** | **D23(b)** — `storage::repair_stale_default_project`, a migration step run on open, deletes a `default_project` naming an archived/missing project. Pinned by a test that seeds the legacy row directly (no sequence of current calls can reach that state) |
| 2 | `task.add`/`task.modify` never validated an **explicit** `project`: `add "x" --project totally-not-a-project` → exit **0**, and `tasqx projects` → `No projects.` A typo silently loses the task. With an archived project, `use` was `conflict` (exit 5) while `add --project` was exit 0 — D22 half-applied | **genuine, major** | **D23(a)** — one shared `require_live_project` reader, called inside the write transaction on **both** paths: unknown → `not_found` (4), archived → `conflict` (5) |
| 3 | `project.create` accepted a whitespace-only name (`req_str` only rejects `""`): it claimed the default, printed as a blank row, and `use "   "` then **refused the exact name `init` accepted** — D21's one-way door at a narrower edge | **genuine** | **D23(c)** — emptiness is checked where names are *born*; `project.use` drops its special-case, so a whitespace name is simply `not_found` and `use` can target anything `init` can create |
| 4 | `project.create`'s event omitted whether that create claimed the default, while `use` records `previous` and `archive` records `default_cleared` | **genuine** | **D23(d)** — the event carries the same `default` boolean the result does |
| — | *(reviewer's alternative to #1)*: make `default_project()` resolve the key against the table on every read instead of repairing the file | **rejected** | It leaves the stale key on disk, so the next `create` sees a non-empty key, declines to claim (D21), and **strands the store with no default and no way to get one but `use`** — a worse version of the reported bug. Repair the file once at the edge; do not teach every reader to squint |

**Beyond the findings** (found while fixing them): the guide promised "a task's `project:` field is free-form text — it does not have to be registered here", which D23(a) makes false, and the blocks were captured under it — `project:home` appeared on two pages with **no `init home` anywhere**, so that documented command would exit 4 for any reader who typed it. The prose is corrected, `tasqx init home` is on the page (its output captured from the real binary), and a new guard, `every_documented_project_is_one_a_documented_init_creates`, fails the build if any documented `add`/`modify` files a task into a project no documented `init` creates. `task.modify`'s `project` arm got the same guard as `task.add` in the same pass — reviewers only reported `add`, and shipping the guard on one of two sibling writers is precisely the bug being fixed.

**Blast radius, recorded honestly:** D23(a) is a behavior change, not just a guard. Six existing tests filed tasks into never-created projects (`project: "P"`, `"other"`, `"p"`) and now `init` them first; one test asserted `use "   "` → `bad_request` and now asserts `not_found`. Any existing store with tasks in unregistered projects keeps them — nothing rewrites task rows — but **new** adds into those names now exit 4 until `tasqx init <name>` is run. That is the intended door: the alternative was deriving `project.list` from `SELECT DISTINCT project FROM tasks`, which resurrects archived projects and invents a second kind of project row (no id, no description) to avoid rejecting a typo.

## Next steps (most important first)

1. **`overflow-checks = true` in `[profile.release]`.** `Cargo.toml` has no `[profile]` section, so release silently wraps. D17 made the *known* duration path total, but the release profile is what turns the *next* one from a loud panic into a wrong number on a user's screen. Deliberately left out of this pass because it changes the shipped profile and deserves its own decision.
2. **Audit the remaining `report.summary` metric paths** the way D17 audited estimates — `tracked_total` is saturating now, but `tracked_seconds` enters the store as a raw i64 column from `store.import` with no edge validation at all.
3. **Extend the D20 guard past the quickstart.** The `add`-id and ghost-row guards still cover `page_install` only (D23's project guard is the first that reads *every* page). The other ten pages' output blocks are still author-maintained: the daemon page's `watch` block (`docs.rs` ~1090) shows ids 1/3/5/2 from the cross-page narrative — self-consistent, but nothing mechanically holds it. While verifying D23 the quickstart's `Ship the v1 JSON API freeze` printed **urgency 17.6** where the page says 17.5 (`due:friday` is relative to capture day) — harmless, but it is exactly the drift a guard would catch.
4. **Decide whether `store.import` owes D23 the same guard.** `store.import` writes `tasks.project` straight from the payload with no project check, and `store.export` **does not export project rows at all** — so an export→import into a fresh store lands every task in a project the new store has never heard of, which is the very state D23 rejects at the `add` edge. Not fixed here: it needs its own decision (does export carry projects? does import create them? does it reject?), and inventing one silently under a review pass is how half-applied guards happen. `require_live_project` is ready to be called there once the question is answered.
5. **CLI `tag`/`untag` verb + a `tag.remove` API method** — tags are addable but not removable from the CLI.
6. **Daemon idle-timeout auto-shutdown (D5)** — `serve` already takes an `Arc<AtomicBool>` shutdown seam; additive.
7. Then the §11a deferrals, in whatever order suits: plugins/hooks (§6), ratatui TUI, sync (D3).

## Explicitly deferred (decided, not forgotten) — see §11a

Recorded in `DESIGN.md` §11a with full rationale. In short: **git-first sync (D3)** — safe to defer because sync is a pure consumer of the already-shipped transactional event log, so it needs no migration; **full ratatui TUI** — a client over the proven daemon/push transport, and `watch` is that path in miniature; **plugins/hooks (§6)** — the capability model already ships and is proven by `tasqx mcp serve` authenticating as a scoped plugin, so freezing an ABI now would be premature; **the no-daemon OS-scheduler notification path (§9b)** — §9a covers everyone with a daemon, and `reminder.fire` is already additive and idempotent so the scheduler can call it later unchanged; **actionable toast buttons (§9b)** — hinge on the same process-ownership question as §9b, and are `notify-rust`'s least portable surface.

## Open decisions & questions

- All **D1–D23** are settled (`DESIGN.md` §12). Nothing is blocked on a user answer.
- **`store.import` + projects is the one open seam D23 leaves** — see next-step 4. It is a gap in the *same* invariant, deliberately left with a decision attached rather than a silent patch.
- `every N months` recurrence intentionally *drifts* after a short-month clamp (Jan31→Feb28→Mar28…), while `monthly on day D` re-clamps and recovers — documented in `recur.rs`, not a bug.
- **D17 vs. release builds:** the release profile still has `overflow-checks` off. The known hole is closed, but see next-step 1.

## How to resume

Working dir: `C:\dev\tasqx` (**NOT a git repo** — there are no commits; "current state" evidence is the test run plus driving the binary, never a hash). Rust 1.95, `stable-x86_64-pc-windows-msvc` (MSVC is discovered via **vswhere, not PATH** — verify a toolchain by running `cargo build`, never `Get-Command`).

```bash
cd /c/dev/tasqx

# honest warning count: incremental builds DO NOT re-emit warnings
cargo clean -p tasqx-core -p tasqx-cli && cargo build --workspace   # expect 0 warnings
cargo test --workspace                                              # expect 241 passed / 0 failed (add --no-fail-fast while iterating)
cargo build -p tasqx-cli                                            # REBUILD before driving the binary
BIN=./target/debug/tasqx.exe

# drive on an isolated DB, ALL IN ONE SHELL CALL (env vars do not persist between calls)
export TASQX_DB="$(mktemp -d)/t.db"
"$BIN" init work && "$BIN" add 'Ship it due:friday +api !high' && "$BIN"

# regression probes for the D16-D20 fixes (each was a real failure)
"$BIN" add "x" -e "1000000000000000000w"   # D17: bad_request exit 2 (was: accepted, then report exit 101)
"$BIN" modify 1 --project ""              # D18: bad_request exit 2, points at --clear project
"$BIN" report --html | od -An -tx1 -v | tr ' ' '\n' | grep -c '^1b$'   # D19: expect 0 ESC bytes
# D16: import a payload whose task depends_on its own id  -> conflict exit 5, store untouched

# regression probes for D23 (all four were exit 0 / silent before)
"$BIN" add "typo" --project not-a-project           # exit 4, names it + suggests `tasqx init`
"$BIN" modify 1 project:not-a-project               # exit 4 - the sibling writer, same reader
echo '{"tasqx":"1","id":"c","method":"project.create","params":{"name":"   "}}' | "$BIN" api  # bad_request
echo '{"tasqx":"1","id":"e","method":"event.list","params":{"entity":"project"}}' | "$BIN" api # create carries "default"

# D23's legacy-store repair needs a store the CURRENT binary cannot produce - seed it:
"$BIN" init work >/dev/null
echo '{"tasqx":"1","id":"a","method":"project.create","params":{"name":"prive"}}' | "$BIN" api >/dev/null
sqlite3 "$TASQX_DB" "UPDATE config SET value='prive' WHERE key='default_project'; \
                     UPDATE projects SET archived=1 WHERE name='prive';"
"$BIN" projects && "$BIN" add "orphan"   # expect: no default marked, add is projectless (was: landed in `prive`)
```

Key files: spec `DESIGN.md`; core `crates/tasqx-core/src/` (`engine.rs` handlers, `dispatch.rs` method table, `storage.rs` schema, `datetime.rs`/`recur.rs`/`remind.rs` scheduling — all take an explicit `now` for deterministic tests, `util.rs` **now `pub`**, `scheduler.rs`, `notify.rs`, `daemon.rs`, `mcp.rs`, `filter.rs`, `types.rs`, `urgency.rs`); CLI `crates/tasqx-cli/src/` (`main.rs`, `sugar.rs`, `render.rs`, `theme.rs`, `chart.rs`, `html.rs`, `docs.rs`); tests `crates/tasqx-core/tests/` + unit tests inside the cli crate.

## Watch out for

- **Rebuild the binary before driving it.** `cargo test` refreshes the test harness but NOT `target/debug/tasqx.exe`. A stale exe shows old behavior while tests pass. Bitten twice.
- **Incremental builds do not re-emit warnings.** To honestly claim 0 warnings you must `cargo clean -p tasqx-core -p tasqx-cli` first.
- **Shell env vars do not persist between tool calls.** A multi-step repro (temp DB → export → import) must run in ONE call or it silently probes the wrong store.
- **clap reads a leading-hyphen value as a flag** (`--remind -30m` broke while the inline form worked, and no test noticed). Use `allow_hyphen_values` and test the *flag* form, not just the sugar form.
- **Tests have stayed green here while the product was broken** — three times now, most recently D17 (205 tests green over a `report` that exited 101 on a value `add` accepted) and D20 (16 docs guards, none checking snippet output). "Working" means observed behavior of the real binary. Prefer a probe that would have caught the bug over a probe that confirms the fix. When adding a guard, **revert the fix and watch it fail** — two of this session's guards passed against the buggy page on the first attempt because a scripted revert silently no-op'd.
- **`duration_secs` has exactly one home** (`tasqx_core::util`). It had two, they drifted, and the copy did the damage. Do not reintroduce a local one.
- **Keep `notify-rust` behind `notify-os`** — it must stay absent from the default cargo tree.
- **Sanitize untrusted text and HTML-escape everything rendered into HTML** — and remember `report --html` defaults to **stdout**, so the HTML path is a terminal path too (D19).
- **Isolate demo data** with `TASQX_DB=<temp>`; the real store is a platform data-dir path. `sample-report.html` in the repo root is a generated demo artifact, safe to regenerate/delete.
- **Windows color through a pipe** degrades to 16-color (can't probe the console); a real Windows Terminal renders truecolor. Don't judge theme colors from piped `cat -v` output.
- **Backslashes get mangled in Bash-tool heredocs.** A `python - <<'PY'` patch script whose search string contained `\n` / `\\"` silently matched **nothing**, the "revert and watch it fail" check then passed against unpatched code, and the guard looked verified when it was not — the exact trap the D20 note below already warns about, via a new route. Build such literals with `chr(92)`, and **always `assert s.count(old)==1`** in the patch script.
- **`cargo test` stops at the first failing target.** One failure in `tests/engine.rs` meant `tests/increment.rs` and `tests/mcp.rs` never ran, so a "6 failures" picture was really 7 targets' worth of unknown. Use `--no-fail-fast` while iterating.
- **Workflow scripts:** never put a backtick inside a `String.raw` block — it breaks parsing.
- **Multi-agent review reliability:** the conformance-reviewer channel has twice returned placeholder `"test"` output, and this session's brief carried a stale test baseline. Do not trust a single reviewer or an inherited number; independently `cargo build`/`test` and drive the real CLI before declaring done. (That said: every finding across both review passes reproduced on the real binary — the D23 pass's four included, though one of the four's *suggested fix* was wrong in a way that would have made the bug worse, and one overstated its evidence: it claimed `project.list` emits no `default:true` row for a stale default, when `--include_archived` does emit one. **Verify the suggested fix, not just the finding.**)
