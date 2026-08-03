# Mutation testing

A green test suite tells you the tests **ran**. It does not tell you they would
have **noticed** if the code were wrong. Mutation testing answers the second
question: it edits the source in small, plausible ways — flips a `<` to `<=`,
deletes a `!`, makes a function return a constant — and reports every edit the
suite still accepted.

That report is the useful artefact. An edit nobody caught is a piece of
behaviour nothing asserts.

## Why this repo bothers

Three times in one week a change here shipped with a fully green suite and a
hand-run mutation pass found a real gap:

- a CLI flag could be severed from its only consumer;
- `backlog` could be dropped from two generated SQL `IN` lists, so a reminder on
  a waiting task would silently never fire;
- a test had stopped running entirely.

All three share a shape: the failure is **silent**. Nothing crashes, nothing
turns red — a user just gets a plausible-looking answer to a question they did
not ask. That is the class of bug mutation testing catches and review does not,
which is why it is now a config file instead of somebody's shell history.

## Running it

```sh
cargo install cargo-mutants     # once
cargo mutants                   # scope + timeouts come from .cargo/mutants.toml
```

A bare `cargo mutants` is the whole command — `.cargo/mutants.toml` carries the
scope, the timeouts, and `cap_lints`. Useful variations:

```sh
cargo mutants --list                       # what would be tested, without testing it
cargo mutants --jobs 6                     # parallelise (24-core dev box: ~5 min)
cargo mutants --file crates/tasqx-core/src/filter.rs   # one file while iterating
cargo mutants --in-place                   # skip the tree copy; faster, dirties the tree
```

Results land in `mutants.out/`, with `missed.txt`, `caught.txt`, `timeout.txt`
and `unviable.txt` broken out.

**Exit code 3 means mutants survived**, not that the run failed. That is why the
CI job (below) is not a gate.

### Scope, and why it is narrow

cargo-mutants rebuilds and reruns the suite **once per mutant**. The full
workspace is ~16.5k lines; the scoped set is four files, a few minutes of wall
clock on a 24-core machine at `--jobs 6` and just under an hour on a
GitHub-hosted CI runner at `--jobs 2`. How many mutants that is today is in
[the sweep below](#known-surviving-mutants) — stated once, in the one place
that is dated, because the copy of the figure that used to sit here outlived
the sweep it came from.

The four files — `types.rs`, `filter.rs`, `remind.rs`, `urgency.rs` — were
chosen because they are small, pure, heavily depended on, and they fail quietly.
`.cargo/mutants.toml` explains the inclusion of each and what was deliberately
left out. **Widening the scope has a real, superlinear cost**: measure the wall
clock before you add a file, and prefer running a candidate file ad hoc with
`--file` first.

## Reading the output

| Status | Meaning |
| --- | --- |
| `caught` | A test failed. This is the good outcome — the suite noticed. |
| `MISSED` | Every test still passed. Something is unasserted. **Triage it.** |
| `TIMEOUT` | The mutant made the program hang (usually a loop that stopped terminating). |
| `unviable` | The mutant did not compile. Not a signal; ignore. |

## Triaging a survivor

Every `MISSED` line is one of three things. Decide which **before** writing a
test — roughly half of all survivors do not deserve one.

**1. A real gap.** Behaviour nothing asserts; a bug in this code could ship.
Write a test. State the user-visible failure in its doc comment, per house style
— name the bug, not the mechanism.

**2. An equivalent mutant.** The edit does not change observable behaviour, so
no test could catch it and none should try. Example from this repo:
`remind.rs`'s `spec_to_string` computes `let sign = if *secs < 0 {'-'} else {'+'}` and the
mutant makes it `<=`. Those differ only at `secs == 0`, and the `n == 0` branch
returns a hardcoded `"+0s"` without ever reading `sign`. Leave it. If a class of
equivalent mutants gets noisy, silence it with `exclude_re` in the config **and
a comment saying why** — never by weakening a test.

**3. Uninteresting.** A doc-only or genuinely unreachable path, or a mutant
whose "failure" is loud rather than silent. The four `pos += 1` → `pos -= 1`
timeouts in `filter.rs`'s parser are this: they make the parser loop forever,
and a real version of that bug would hang the CLI visibly rather than returning
a wrong answer quietly.

The trap to avoid: **do not write a test that simply restates the
implementation.** A test asserting `score(...) == breakdown(...).sum()` kills the
mutant and guards nothing. Pin the expected value as a literal instead, so the
test disagrees with the code when the code changes.

## Known surviving mutants

The figures below are a **dated reading of one sweep**, not a standing property
of this repo. Re-derive them from the newest run whenever you touch this
section, and name the run they came from. The previous version of this
paragraph read "160 mutants, 143 caught, 0 missed" off an undated local sweep
and stayed there while the scope grew by dozens of mutants and a survivor
appeared in two consecutive CI sweeps — which is the failure this page's last
line warns about, committed on the page itself.

Last sweep: run 30811736799 on `a2f767b` (2026-08-03, on a CI runner at
`--jobs 2`), count for count identical to the scheduled run 30791446542 on
`ce3f284` earlier the same morning. The line below is quoted verbatim from that
run's log, wall clock and all — the same line appears in the comment on the
`mutants` job in `.github/workflows/ci.yml`, and a test compares the two.

```text
203 mutants tested in 53m: 1 missed, 184 caught, 14 unviable, 4 timeouts
```

`types.rs` and `urgency.rs` are clean, and so is every real gap the first sweep
found.

The one missed mutant is a real gap, not an equivalent one — `spacing_hint` in
the table below, open as tasqx #48. The two equivalent mutants are suppressed
via `exclude_re` in `.cargo/mutants.toml` — legitimate only because they are
provably unkillable, not merely hard to test. Anything that is merely hard to
test belongs in this table, not in the config.

The timeout row is why this is still not a CI gate: `cargo mutants` exits 3 on
timeouts as well as survivors, and the timeout count varies run to run (three
local sweeps gave 5, 4 and 2 from identical code, because a mutant that loses
the race is scored caught instead). See the comment on the `mutants` job in
`.github/workflows/ci.yml` for the two honest ways to fix that.

Locations are given as file plus **function**, and the line numbers this table
used to carry have been dropped rather than refreshed. Every one of them had
rotted: `parse_and` was recorded at `filter.rs:227` and lives at 824, and the
`pos += 1` row named three sites that had moved and multiplied to five. A line
number in prose is wrong the moment anything above it moves, and it silently
misdirects the next reader instead of failing; a function name can be — and is,
in `tasqx-core`'s `lib.rs` — checked against the source by a test.

| Location | Mutation | Verdict |
| --- | --- | --- |
| `filter.rs` `spacing_hint` | `\|\|` → `&&` on the suppression guard | **MISSED — a real gap, tracked as tasqx #48.** The hint is withheld from any token that opens a predicate of its own; under `&&` both halves would have to hold at once, so the suppression never fires again. Nothing in the suite pairs a value predicate with a following token that BOTH opens a predicate and fails to parse, so every test stays green while `project:Home @wroking` is answered with "did you mean `project:"Home @wroking"`?" — advice to quote a typo into the project name. |
| `filter.rs` `parse_and` | `and` keyword guard → `false` | Equivalent. Without the guard an explicit `and` falls through to `parse_term`, matches no predicate prefix, and becomes `Pred::Always` — the identity of the enclosing `And`. `eval`'s `.all()` is unchanged, and `constrains_status`'s `.any()` reads `Pred::Always` as false either way. |
| `remind.rs` `spec_to_string` | `<` → `<=` | Equivalent. The two operators differ only at `secs == 0`, and that is exactly the input where `sign` is never read: the next lines compute `n = secs.abs()` and return the literal `"+0s"` before `sign` reaches any `format!`. |
| `filter.rs` `parse_or`, `parse_and`, `parse_term` | `pos += 1` → `-=` / `*=` | Uninteresting, and reported as TIMEOUT rather than MISSED. The parser's cursor stops advancing and it loops forever. Non-termination is loud and immediately diagnosable, unlike every other finding here — a test for it would be a hang with a stopwatch. Worth knowing that loop termination rests entirely on monotonic `pos` advance. |

### Gaps this sweep closed

The first sweep found two real gaps, both now fixed and both mutation-verified:

- **`filter.rs` `parse_and`, the significant one.** Deleting the loop's
  break-on-`)` left the whole workspace green. Nothing *evaluated* a parenthesised
  filter — the only paren case in the suite sat inside a `constrains_status`
  assertion, which returns true either way. Without the arm `(a or b) and c`
  reassociates to `a or (b and c)`, so `tasqx list "(+api or +infra) and
  status:done"` would return every `+api` task regardless of status: no error,
  no crash, just a credible table containing exactly the rows the user filtered
  out. Closed by `parentheses_group_rather_than_reassociating`.
- **`filter.rs` `instant_cmp`.** The exact-instant boundary of `due.before:` /
  `due.after:` was unasserted, so both comparisons could silently become
  non-strict. Closed by `due_bounds_are_strict_at_the_exact_instant`.

Neither was found by review, by 299 passing tests, or by adversarial reading.
Both were found by mutation testing. That is the argument for this file
existing.

Keep this table current when the sweep changes — a stale known-survivors list is
worse than none, because it trains people to skim the report.
