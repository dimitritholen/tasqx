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
workspace is ~16.5k lines; the scoped set is four files, 159 mutants, about
5 minutes wall clock on a 24-core machine at `--jobs 6` and roughly 25-40
minutes on a 2-core CI runner.

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
`remind.rs:75` computes `let sign = if *secs < 0 {'-'} else {'+'}` and the
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

As of the last full sweep: **160 mutants, 143 caught, 0 missed, 10 unviable, 2-5 timeouts.**
`types.rs` and `urgency.rs` are clean, and so is every real gap the first sweep
found.

There are no missed mutants left. The two equivalent ones below are suppressed
via `exclude_re` in `.cargo/mutants.toml` — legitimate only because they are
provably unkillable, not merely hard to test. Anything that is merely hard to
test belongs in this table, not in the config.

The timeout row is why this is still not a CI gate: `cargo mutants` exits 3 on
timeouts as well as survivors, and the timeout count varies run to run (three
local sweeps gave 5, 4 and 2 from identical code, because a mutant that loses
the race is scored caught instead). See the comment on the `mutants` job in
`.github/workflows/ci.yml` for the two honest ways to fix that.

| Location | Mutation | Verdict |
| --- | --- | --- |
| `filter.rs:227` `parse_and` | `and` keyword guard → `false` | Equivalent. Without the guard an explicit `and` falls through to `parse_term`, matches no predicate prefix, and becomes `Pred::Always` — the identity of the enclosing `And`. `eval`'s `.all()` is unchanged, and `constrains_status`'s `.any()` reads `Pred::Always` as false either way. |
| `remind.rs:75` `spec_to_string` | `<` → `<=` | Equivalent. The two operators differ only at `secs == 0`, and that is exactly the input where `sign` is never read: the next lines compute `n = secs.abs()` and return the literal `"+0s"` before `sign` reaches any `format!`. |
| `filter.rs:210/228/253` | `pos += 1` → `-=` / `*=` | Uninteresting, and reported as TIMEOUT rather than MISSED. The parser's cursor stops advancing and it loops forever. Non-termination is loud and immediately diagnosable, unlike every other finding here — a test for it would be a hang with a stopwatch. Worth knowing that loop termination rests entirely on monotonic `pos` advance. |

### Gaps this sweep closed

The first sweep found two real gaps, both now fixed and both mutation-verified:

- **`filter.rs:225`, the significant one.** Deleting the `Some(")") => break`
  arm left the whole workspace green. Nothing *evaluated* a parenthesised
  filter — the only paren case in the suite sat inside a `constrains_status`
  assertion, which returns true either way. Without the arm `(a or b) and c`
  reassociates to `a or (b and c)`, so `tasqx list "(+api or +infra) and
  status:done"` would return every `+api` task regardless of status: no error,
  no crash, just a credible table containing exactly the rows the user filtered
  out. Closed by `parentheses_group_rather_than_reassociating`.
- **`filter.rs:156/158`.** The exact-instant boundary of `due.before:` /
  `due.after:` was unasserted, so both comparisons could silently become
  non-strict. Closed by `due_bounds_are_strict_at_the_exact_instant`.

Neither was found by review, by 299 passing tests, or by adversarial reading.
Both were found by mutation testing. That is the argument for this file
existing.

Keep this table current when the sweep changes — a stale known-survivors list is
worse than none, because it trains people to skim the report.
