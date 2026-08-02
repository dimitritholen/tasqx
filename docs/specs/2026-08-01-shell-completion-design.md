# Shell completion — design

**Date:** 2026-08-01
**Status:** designed. Not yet implemented.
**Scope:** the `tasqx` CLI binary only. No JSON API method is added, no MCP tool
changes, no protocol change. One new core function (`storage::open_read_only`)
and one new CLI verb (`completions`).

## Problem

`tasqx` has forty-odd subcommands, thirty-plus aliases, a per-verb flag surface,
and three value vocabularies that are not flags at all: the capture sugar
(`+tag`, `project:`, `due:`, `!prio`), the read-side filter grammar, and the
short ids that every mutating verb takes as its first positional. All of it must
be typed exactly, from memory, with no feedback until the command either runs or
fails.

The ids are the sharpest edge. `tasqx done 4` is unforgiving in the way that
matters: 4 is a real task, just not the one meant, and the command succeeds. No
amount of help text fixes a surface where the correct value is a small integer
the user has to remember.

Shell completion is the standard answer, and this project is unusually well
placed to give it: `crates/tasqx-cli/src/command.rs` already declares the entire
command surface in one clap derive tree, which is the exact input a completion
generator wants.

## Decision

Ship **dynamic** completion via `clap_complete`'s `unstable-dynamic` feature —
the shell calls back into the `tasqx` binary on each Tab, and candidates are
produced by Rust code attached to individual args.

Static script generation (`clap_complete::aot`) was the alternative and is
rejected. It is stable API and needs no callback, but it can only ever complete
what is knowable at build time. Every value this feature exists to supply — the
user's projects, the user's tags, the ids of the user's open tasks — is knowable
only at Tab time. A static script would complete `--project` as "some string".

The two other routes considered:

- **Hand-written scripts per shell.** Total control, no unstable dependency, and
  the artefact packagers expect. Rejected because it creates five surfaces that
  must be kept in lockstep with `command.rs` by hand. This repository has been
  bitten by that class repeatedly — the `COMMAND_REF` drift that left fourteen
  of twenty-seven `Safe` examples unexecuted is the same shape — and five
  hand-maintained copies of the command tree is the largest instance of it
  anyone has proposed here.
- **Stable `aot` generation plus post-processed hooks.** Generate the scripts,
  then splice value-lookup calls into them. No unstable dependency and the
  command surface stays generated. Rejected because the splice is five
  shell-dialect string patches against clap's own output template: it breaks
  silently whenever clap changes that template, and "silently" is the operative
  word.

The cost of the chosen route is named rather than hidden: `unstable-dynamic` is
semver-exempt, so clap may change the API in a patch release. The mitigation is
an exact version pin and a test that exercises real completion output, so the
breakage is a red build rather than a shipped regression. See
[Guards](#guards).

## Entry point

`complete::intercept()` becomes the first statement of `run()`
(`crates/tasqx-cli/src/lib.rs:234`), before `argv::prepass`:

```rust
pub fn run() {
    complete::intercept();   // returns immediately unless $TASQX_COMPLETE is set
    let pre = argv::prepass(std::env::args_os());
    ...
}
```

`CompleteEnv` is pointed at `$TASQX_COMPLETE` via `CompleteEnv::var`, **not** at
its default `$COMPLETE`. Unset — the ordinary case, every real invocation — it
costs one environment lookup and returns. Set, it writes candidates and exits the
process, so the completion path never reaches `open_backend`, never opens the
ordinary read-write store, and never runs the dispatcher.

### Why the variable is tasqx-specific

`clap_complete`'s protocol offers no way to tell a genuine callback from a human
running a command with the variable left in the environment. Once it names a
shell clap can complete, argv containing a `--` takes `shell.write_complete` and
the command is dropped: **zero bytes out, exit 0, nothing done** — measured
against the built binary when the variable was still clap's `COMPLETE`, with
`COMPLETE=bash tasqx --no-daemon add -- "a real task"`, which added no task, and
`COMPLETE=zsh tasqx --no-daemon done 1`, which printed the zsh registration and
left task 1 open. Argv without a `--` always takes the registration branch and
prints a page of shell script instead of running the verb.

`_CLAP_COMPLETE_INDEX` is not a stronger discriminator. Only bash, elvish and zsh
set it; fish (`clap_complete-4.6.7/src/env/shells.rs:231`) and PowerShell
(`:381`) compute `index = args.len() - 1` from the words and never export it, so
requiring it would kill completion outright in two of the five shells.

What is left is the name. `COMPLETE` is generic — a half-run activation line, a
stale export, or another clap-based tool's profile entry can leave it set, and
the PowerShell activation line below sets and unsets it in a single statement, so
an interrupted paste leaves it set for the session. `TASQX_COMPLETE` is a name
nothing else writes.

**This reduces the hazard; it does not remove it.** A user who sets
`TASQX_COMPLETE` by hand still loses the next `tasqx … -- …` silently. The
divergence from clap's convention is free because tasqx prints its own activation
lines (`tasqx completions <shell>`), so nobody types the variable name.

### The pre-pass applies to completion words too

This is the part that will be got wrong if it is not written down.

The words a shell hands the completer are **raw argv**. tasqx cannot parse raw
argv: filter tokens like `-needs` are valid grammar and look exactly like flags,
which is why `run()` does not call `Cli::parse()` and calls
`argv::prepass(std::env::args_os())` instead (`lib.rs:235-238`).

clap's completion engine parses the words it is given against the same
`clap::Command`. It therefore inherits the same problem, and needs the same fix:
`intercept()` must run the completion words through `argv::prepass` before
handing them to the engine.

Without it, completing after `tasqx list -needs<TAB>` misbehaves — and it
misbehaves in a place no existing test looks, because every current guard
exercises the parse path, not the completion path.

## Modules

| File | Responsibility |
|---|---|
| `crates/tasqx-cli/src/complete.rs` | **Create.** `intercept()`, the `CompleteEnv` wiring, the guarded lookup and its time budget. |
| `crates/tasqx-cli/src/complete/candidates.rs` | **Create.** The five candidate providers and the prefix dispatcher. |
| `crates/tasqx-cli/src/complete/install.rs` | **Create.** The `completions` verb: print, install, uninstall, shell detection, the marked block. |
| `crates/tasqx-cli/src/command.rs` | **Modify.** The `Completions` variant; `ValueHint` and `value_parser` additions; completer attachments. |
| `crates/tasqx-cli/src/cmddoc.rs` | **Modify.** A `COMMAND_REF` entry for `completions`. Not optional — the existing guard fails the build for an undocumented verb. |
| `crates/tasqx-core/src/storage.rs` | **Modify.** Add `open_read_only`. |
| `crates/tasqx-core/src/engine.rs` | **Modify.** Add `Engine::open_read_only`. |
| `crates/tasqx-cli/tests/completion.rs` | **Create.** The guards. |

`complete.rs` owns *how* a candidate is fetched safely; `complete/candidates.rs`
owns *what* the candidates are; `complete/install.rs` shares nothing with either
and only happens to be about the same feature. Three files rather than one
because the first two run inside a keystroke and the third does not, and that
distinction should be visible in the file layout rather than only in comments.

## Data path

```
lookup()
  ├─ $TASQX_NO_COMPLETE_LOOKUP set?               -> []
  ├─ daemon::try_connect(resolve_socket(None))    -> Backend::Remote
  ├─ open_read_only(db_path_read_only())          -> Backend::Local  (READ_ONLY, no create, no migrate)
  └─ absent | locked | error | over budget | panic -> []   exit 0, stderr silent
```

`db_path_read_only()` rather than `db_path()`, and the difference is not
cosmetic: `db_path()` creates the parent directory of `$TASQX_DB`, and the
platform data directory, *before it returns*. A Tab press on a machine that has
never run tasqx would therefore author `%APPDATA%\tasqx\tasqx\data\` even though
no database was opened. The two are one resolution function and a boolean, not
two copies of the `$TASQX_DB`-then-`ProjectDirs` rule, because they must agree
about where the store is or completion offers ids from a store no command reads.

Three properties, each deliberate.

**It reuses the daemon preference.** `open_backend` (`lib.rs:788`) already tries
the socket and falls back immediately on a missing or stale one, and it never
auto-spawns a daemon. Completion wants exactly that behaviour, so it reuses the
logic rather than restating it. When a daemon is up, Tab is a socket round trip
and the store is untouched.

**The fallback is read-only: no database, no schema, no user data.** `storage::open`
creates the file and runs the migration (`storage.rs:34-41`). Reusing it would mean
a Tab press on a machine that has never run tasqx authors a database and a schema.
`storage::open_read_only` opens with `OpenFlags::SQLITE_OPEN_READ_ONLY`, does not
call `migrate`, and returns an error for an absent path — which the caller turns
into an empty candidate list.

The promise is stated as those three things and not as "no file appears", because
the wider claim is false and was measured to be false: a read-only connection
*does* create the `-shm`/`-wal` sidecars on first query. See
[Known limitation](#known-limitation-read-only-opens-and-the-wal). What a Tab
press cannot do is bring a store into existence, change its schema, or alter a
byte of the user's data.

**Everything is inside a wall-clock budget.** 150 ms, measured across the whole
lookup. The lookup runs on a worker thread and the caller waits on
`recv_timeout`; on timeout it emits nothing and exits *without joining*. Leaving
a thread detached is acceptable here and nowhere else in this codebase,
specifically because the process is a short-lived completion callback that is
about to exit — this must be stated in the code, not just here.

### Known limitation: read-only opens and the WAL

**A read-only connection does create the `-shm`/`-wal` sidecars.** An earlier
draft of this section, and the doc comment that followed it, claimed the
opposite. That claim was wrong. Measured on a cleanly-closed WAL store with no
other connection open:

```
writer closed          ["tasks.db"]
opened read-only       ["tasks.db"]                  <- nothing yet
after one SELECT       ["tasks.db", "-shm", "-wal"]  <- both appear
reader dropped         ["tasks.db", "-shm", "-wal"]  <- both remain
```

`SQLITE_OPEN_READ_ONLY` governs the **database file**. It says nothing about the
WAL index: SQLite's shm layer opens `-shm` with `RDWR|CREATE` first and only falls
back to a read-only attempt if that fails, so a writable *directory* is enough for
both files to appear regardless of the connection's flags. They appear on first
query, not at open, because that is when the WAL index is first needed.

They also persist. Removing the pair on last-connection close is itself a write,
and this connection cannot perform one. An ordinary `tasqx list` creates exactly
the same two files while it runs and then removes them on exit (measured: only
`tasks.db` remains afterwards), so completion is not doing anything new to the
store — it is doing the same read and leaving the tidy-up undone.

What is therefore accepted: a Tab press against an existing store can leave two
derived index files beside it, which the next writer reuses or removes. What is
still guaranteed, and is what the caller actually needs: **no database and no
schema are created, and no user data is written.** An absent path remains an
`Err` with nothing left behind, because the open fails before SQLite reaches the
WAL layer at all.

The alternatives are unchanged in verdict but not in reasoning. Opening
read-write reintroduces creation and migration on a keystroke. `immutable=1`
would suppress the sidecars outright, and it is rejected **not** because it
cannot be made to work but because it is unsound against a live writer: it tells
SQLite the file cannot change, and SQLite believes it — caching pages and
skipping locks — so a concurrent write yields stale reads, garbage rows, or a
spurious `database disk image is malformed`. Trading two harmless index files for
silently wrong answers, on a path whose whole purpose is to be harmless, is the
worse bargain.

## Candidate providers

| Provider | Source | Attached to |
|---|---|---|
| Task ids | `task.list`, `short_id` + title as the candidate's help text, capped at 200 | every positional in the tree that takes a task reference — **thirteen**, not the ten below |
| Projects | `project.list` | `--project`, `use`, and the `project:`/`proj:` sugar prefix |
| Tags | union of the `tags` field already present on `task.list` rows (`engine.rs:845`) | the `+` sugar prefix, and `+`/`-` in filter position |
| Themes | the built-in names plus a listing of the user theme directory | `--theme` |
| Filter tokens | the static grammar keywords, plus live projects and tags behind `project:` and `+` | the filter positionals of `list`, `export`, `watch` |

Tags need **no new API method**: `task.list` rows already carry them, so the
provider derives the set from a call that already exists. Adding a `tag.list`
method to the JSON API for the benefit of a shell callback would widen a
contract surface this project has recently been narrowing (D50).

Task-id candidates carry the task title as help text, which shells render beside
the value. This is the difference between `tasqx done <TAB>` offering a column of
bare integers and offering a readable list; a completion that shows only ids
solves the typing problem and not the remembering problem.

Which shells render it is upstream's, and is not uniform: zsh writes
`value:help` and fish writes `value\thelp`, while bash's registration writes
values only (`clap_complete-4.6.7/src/env/shells.rs`). The help costs nothing
where it is dropped, so it is always attached — but a test that wants to prove
the title arrives must drive the zsh or fish protocol, because a bash-driven one
is structurally incapable of seeing it.

**The attachment list above was wrong when it was written, and the drift guard is
what found that out.** An earlier draft named ten verbs. Reading clap's own arg
table finds thirteen positionals that take a task reference: those ten plus
`why`'s `ref` — the same "short_id or UUID" shape, omitted from the list for no
reason anyone recorded — and the second positional of `dep` and `undep`, which
name a task exactly as much as their first does. This is why the guard reads the
table instead of a list: the list was the thing that was wrong.

Membership is decided by TWO signals that must agree — the positional's help text
containing `short_id or UUID`, and its field name being `ref` or `depends_on` —
because either alone is one edit away from silently narrowing the guard's scope.
A disagreement is a red build, not a smaller guard.

**One provider serves all thirteen, and it filters nothing.** `reopen` wants
terminal tasks and `done` wants open ones, so a filter tuned for either makes the
other useless — and `reopen <TAB>` offering only pending tasks is worse than
offering everything, because it looks like an answer. The provider sorts by
urgency, the order `tasqx list` already shows, and the cap takes the hottest 200.
Per-verb scoping is a real improvement and a later decision with its own
evidence.

### Prefix dispatch for sugar and filters

`+tag`, `project:x` and `due:friday` are not clap args — they are words inside a
positional `Vec<String>` that `sugar.rs` parses afterwards. A plain candidate
list cannot serve them, because the right answer depends on what the user has
typed so far *within the current word*.

`ArgValueCompleter` receives the partial word, so the completer attached to those
positionals dispatches on its prefix: `+` yields tags, `project:`/`proj:` yields
projects, `!` yields priorities, and anything else yields nothing (a title word is
not completable).

**The date-shaped keys yield nothing either, and that is a decision rather than an
omission.** `due:`, `scheduled:`, `wait:`, `repeat:`/`every:`/`recur:`, `remind:`
and `est:`/`estimate:` all take an *open* vocabulary: natural-language expressions
parsed by `tasqx_core::datetime` and `recur`, whose grammar is `in 3 days`,
`every 3 days`, `-30m`, `1h30m`. Neither module exports a registry of accepted
words — the words that do appear (`today`, `tomorrow`, `eom`) are match arms
inside a private function — so a menu here would be a hand-written fourth copy of
a vocabulary that already exists in three places, which is the drift shape D30
exists to stop. Offering nothing is the honest answer to a question whose answer
is "anything a human can write". This paragraph was added after the shipped
dispatcher and this document were found to disagree.

The same mechanism serves filter positionals against the read-side grammar.

## Fixed sets, hints, and the free wins

Independent of the dynamic machinery, and stable clap API:

- `--priority` is currently a bare `Option<String>` (`command.rs:112-114`). A
  `value_parser` of `H|M|L` and the long spellings makes it completable *and*
  improves the error message for a bad value. The same applies to other args
  with a closed vocabulary.
- Path-taking args declare `value_name = "PATH"` but no `ValueHint`
  (`command.rs:440` and others), so no shell offers file completion for them.
  `ValueHint::FilePath` / `DirPath` costs one attribute each.

These are worth doing in the same change because they are the same user
complaint, and because a `value_parser` is load-bearing for completion rather
than decorative.

## Failure policy — D33 inverted, on purpose

On the `$TASQX_COMPLETE` callback path, **every** failure produces zero candidates,
exit 0, and nothing on stderr:

| Condition | Behaviour |
|---|---|
| No store at `db_path()` | `[]`, exit 0 |
| Store present but locked / WAL-inaccessible | `[]`, exit 0 |
| Daemon unreachable | fall through to the read-only open |
| Lookup exceeds the 150 ms budget | `[]`, exit 0 |
| Panic inside a provider | caught, `[]`, exit 0 |

This directly inverts the rule the rest of the codebase is built on. D33 says a
value that changes nothing must not answer `ok`; the silent-drop class is the
recurring defect this project names and hunts. Here silence is the *correct*
behaviour, because stderr output during a Tab press corrupts the user's command
line and an error exit makes the shell beep at someone who was only typing.

Because that inversion is genuinely surprising in this codebase, it must be
stated in the module documentation of `complete.rs`. Otherwise a future reader —
or a future adversarial review, which is likelier — will correctly identify it as
a silent-drop violation and "fix" it.

**The `completions` verb is exempt.** It is an ordinary command run by a human,
and it reports its errors loudly and exits non-zero like every other verb.

## The `completions` verb

```
tasqx completions [SHELL]              # print the activation line
tasqx completions [SHELL] --install    # append it to the profile, after confirming
tasqx completions [SHELL] --uninstall  # remove a previously installed block
```

`tasqx completions <shell>` with no flags is the dry run: it prints exactly the
text that `--install` would write, so a user can pipe it, inspect it, or paste it
themselves. Printing is the default because it is the composable, packager-
friendly, unsurprising behaviour.

### Cross-platform matrix

| Shell | Platforms | Activation line | Profile `--install` targets |
|---|---|---|---|
| bash | Linux, macOS, Git Bash | `source <(TASQX_COMPLETE=bash tasqx)` | `~/.bashrc` |
| zsh | macOS (default shell), Linux | `source <(TASQX_COMPLETE=zsh tasqx)` | `~/.zshrc` |
| fish | all three | `TASQX_COMPLETE=fish tasqx \| source` | `~/.config/fish/completions/tasqx.fish` |
| PowerShell | Windows, plus pwsh on macOS/Linux | `$env:TASQX_COMPLETE = "powershell"; tasqx \| Out-String \| Invoke-Expression; Remove-Item Env:\TASQX_COMPLETE` | `$PROFILE`, created with parents if absent |
| elvish | all three | `eval (E:TASQX_COMPLETE=elvish tasqx \| slurp)` | `~/.elvish/rc.elv` |

The activation lines are `clap_complete`'s own shapes, verified against
`clap_complete-4.6.7/src/env/mod.rs:38-63`, with the variable substituted for the
tasqx-specific one above; they are not reproduced from memory and must be
re-verified if the pin moves.

The PowerShell line is the reason the variable name matters: it sets and removes
the variable in a single statement, so an interrupted paste or a profile that
throws between the two leaves it set for the rest of the session. With
`TASQX_COMPLETE` that state is reachable only by pasting tasqx's own line and
having it die halfway; with `COMPLETE` any other clap-based tool's line does it
too.

**cmd.exe is unsupported.** It has no completion protocol to hook. `tasqx
completions cmd` exits 2 saying so, rather than silently succeeding at nothing —
the D33 rule applies here, because this is the verb surface, not the callback.

**nushell is a known gap.** It would need `clap_complete_nushell`, which is
static generation only and therefore could never carry live values. Documented,
not built.

### Shell detection

`--shell` wins when given. Otherwise: `$SHELL` on unix, the parent process name
on Windows. **If detection is ambiguous or fails, `--install` refuses and asks
for `--shell`.** Guessing wrong means editing the wrong profile, which is a
worse outcome than one more word of typing.

### The marked block

```
# >>> tasqx completions >>>
<activation line>
# <<< tasqx completions <<<
```

`--install` **replaces** an existing block rather than appending a second, so
running it twice is a no-op rather than a duplicate source line. `--uninstall`
removes exactly that block and leaves everything else byte-identical. If the
region between the markers has been hand-edited, `--install` shows what it would
overwrite and asks before proceeding.

Installation modifies a file the user owns, so it confirms first, printing the
target path and the exact text. **A non-interactive stdin is a refusal, not an
implied yes** — a piped `tasqx completions --install` exits non-zero telling the
caller to run it interactively or use the printing form.

## Guards

**The pin.** `clap_complete = { version = "=4.6.7", features = ["unstable-dynamic"] }`.
An exact pin, commented in the style of the MSRV block: this feature is
semver-exempt, so cargo's ordinary compatibility rules do not protect against an
API change, and the pin is what turns a silent breakage into a deliberate
upgrade. Moving it is an act with tests attached, not a `cargo update`.

**`crates/tasqx-cli/tests/completion.rs`:**

- For each of the five shells, `TASQX_COMPLETE=<shell> tasqx` emits a non-empty
  registration naming the binary, and the registration names `TASQX_COMPLETE`
  rather than clap's default. This is what catches an upstream API or template
  change at the pin, and what catches `CompleteEnv::var` being dropped.
- `COMPLETE=bash tasqx add -- "…"` still adds the task: clap's default variable
  is not ours, which is the whole mitigation above.
- `TASQX_COMPLETE=bash tasqx add -- "…"` still drops it. The residual hazard is
  **pinned as observed**, not asserted as absent, so a future fix fails the build
  and sends whoever wrote it to the paragraph that has to stop saying it.
- With a temp store seeded with a project, a tag and a task, the callback emits
  the seeded values — the feature works, tested through the real binary. The
  seeded fixture must also set `$TASQX_SOCK` to an address nothing is listening
  on: the lookup prefers a reachable daemon and the remote path never consults
  `$TASQX_DB`, so on a developer machine running `tasqx daemon` the test would
  seed a temp store and assert against the user's live one. `--no-daemon` is not
  available, because the callback path parses no flags.
- With `TASQX_DB` pointing at a nonexistent path, the callback exits 0, writes
  nothing to stderr, offers no ids, **and creates no file at that path**. The
  never-create guarantee is asserted against the filesystem, not documented in a
  comment.

  An earlier version of this bullet said "prints nothing", and that is false —
  measured: `tasqx done <TAB>` with no store answers `--json --theme --socket …`,
  the flags `done` declares, because structure needs no store and clap's engine
  offers it regardless of what the value providers do. That is correct behaviour,
  so the guard asserts the absence of ID candidates rather than emptiness. An
  emptiness assertion would have pinned a property the code does not have, which
  is the defect shape this branch has spent its time removing.
- `$TASQX_NO_COMPLETE_LOOKUP` disables the VALUE lookups and nothing else:
  against the same seeded store the ids are gone and `tasqx lis<TAB>` still
  offers `list`. A short-circuit that also killed structural completion would be
  a far worse trade than the variable advertises, and nothing else would notice.
- Against a store that **exists**, a read-only open plus a real query leaves the
  database byte-identical, the schema unmigrated, and no second database beside
  it. This is the case the missing-store test above cannot reach — that open
  fails before SQLite touches its WAL layer — and its absence is how the false
  sidecar claim survived review. The sidecars themselves are pinned as *observed*
  rather than asserted as required, so a future SQLite that behaves differently
  fails the build instead of silently invalidating the paragraph above.
- A drift guard iterating clap's own arg table, recursively so nested verbs are
  covered: any positional that takes a task reference and has no candidate
  provider attached fails the build, naming the positional. Same technique as
  `no_declared_short_flag_is_ever_escaped`, which reads clap's arg table rather
  than a hand-kept list, and for the same reason — proven by removing one
  attachment and watching it redden.

**`complete::tests::escaping_drift`** guards the other end of the pre-pass, and
it must test BEHAVIOUR rather than a type. Every provider on a positional the
pre-pass escapes into has to be built with `escaped_word_completer`; the two ways
to get that wrong are an `ArgValueCandidates` (the engine prefix-filters it
against the still-escaped word) and a bare `ArgValueCompleter` (it filters
against the sentinel itself). The second is the likelier one, because it is the
right *type* and the obvious way to write a completer.

Checking `get::<ArgValueCandidates>().is_none()` catches only the first, which
was measured: with a bare `ArgValueCompleter` on `List::filter`, `cargo test -p
tasqx-cli --lib complete::` stayed green across three forced rebuilds while the
real binary answered `list -ne<TAB>` and `list -needs<TAB>` with nothing and
`list +<TAB>` correctly.

Neither a marker extension nor a candidate-set comparison closes it. A marker is
a second, independent `ArgExt` — `clap::Arg`'s extensions are keyed by `TypeId`
and the engine reads `ArgValueCompleter` and nothing else — so the guard would
only be checking that somebody remembered to write the marker. Comparing
`complete(escaped)` against `complete(raw)` is vacuous, because every provider
here answers `-tag` out of the user's store and the guard runs without one, so
both sides come back empty. What works is a probe word the wrapper recognises
*after* restoring: answering it is proof the restore ran inside the shipped
closure, and the guard's own failure direction is exercised by a test that drives
a bare completer through it and requires a `false`.

**Out of scope, deliberately:** real PTY round-trip tests against installed
shells (`completest-pty`). They would need five shells installed on three CI
platforms. The registration-emission test plus the candidate test cover the two
halves that can break in this repository; the part they do not cover is whether
each shell *interprets* clap's registration correctly, which is upstream's
responsibility and is tested upstream.

## Distribution

**`release.yml` does not change.** There are no scripts to generate, ship,
version-match or install into per-distro completion directories, because the
completion logic travels inside the binary. This is the strongest practical
argument for the dynamic route over static generation and is recorded here so it
is not re-litigated.

Documentation lands in three existing places: a `COMMAND_REF` entry (mandatory),
a `manual` section under an existing topic, and a README section.

## Slices

Each is independently shippable and human-accepted before the next begins.

1. `CompleteEnv` wired, the pre-pass fix, `completions <shell>` printing.
   Commands, aliases and flags complete in all five shells. Useful on its own.
2. `storage::open_read_only`, the guarded lookup, task ids with titles.
3. Projects, tags, and the sugar prefix dispatcher.
4. Filter grammar tokens.
5. `completions --install` / `--uninstall`, plus `COMMAND_REF`, manual and
   README documentation.

Slice 1 delivers the majority of the typing relief. Slice 2 delivers the
correctness relief — the wrong-id problem — and is the one that earns the
unstable dependency.
