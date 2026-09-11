# Carrying the house style to a screen

The procedure behind `docs/terminal-style.md`: how a family of terminal screens
is brought onto the style, and what has to be true before it counts as done.

Written to be followed unattended. Every stop rule here exists because the
alternative is a run that spends a night on one screen, or that reports success
it cannot show.

---

## The bar

A family is done when **all** of these hold for **every surface in it**, and
not before:

1. A **before/after render** exists at 80, 100 and 140 columns, plus one in
   `mono`, for each surface. You have looked at all of them.
2. A **rule-by-rule audit** is written: for each of the thirteen rules in
   `docs/terminal-style.md`, one line — *carried*, *not applicable (why)*, or
   *deliberately not (why)*. No rule is left unmentioned.
3. Every behaviour you changed has a **test that was watched fail** against the
   old behaviour. Not asserted — watched. See §"Guards must bite".
4. All four gates green, from a clean run:
   ```console
   cargo fmt --all -- --check
   RUSTFLAGS="-D warnings" cargo test --workspace --all-targets --no-fail-fast
   cargo clippy --workspace --all-targets -- -D warnings
   cargo doc --workspace --no-deps --all-features
   ```
5. A **fresh reviewer agent**, given only the style doc, the after-images and
   the diff, and told to refute, has returned a verdict — and every real defect
   it named is either fixed or recorded as open with a reason.
6. The family is **committed on its own**, and the task annotated with what
   landed, what did not, and why.

## A run is a family, not a verb

`list` and `agenda` share one renderer, so they were one design problem and
were solved once. The same is true of the twelve lifecycle echo lines, of the
three charts, of the tables that are not the task table. Sweeping verb by verb
solves the same problem up to twelve times and lets the answers drift apart,
which is the thing the style exists to stop.

So a run covers a family, and **every surface in it** — not a representative
one. Render them all. Audit them all. If two surfaces in a family want
different answers to the same rule, that is the finding: say which one is
right and make them agree.

The families, and what is peculiar about each:

- **tables** — `projects`, `report`, `config list`, `theme list`/`show`,
  `tokens`, `memory`. Rules 1, 2, 7, 8, 9 and 12 carry directly. They are not
  task tables, so rules 3, 4, 5 and 6 mostly do not apply; say so per rule
  rather than skipping them.
- **detail** — `show`, `next`, `why`, the `add` card. D78 already rules the
  card's layout and is not overturned here. Rule 11 is the sharp one: a card
  that repeats in its body what its own header line said.
- **echoes** — the one- and two-line answers of every write verb. Most layout
  rules are inapplicable; rules 8 and 11 are the whole job, plus one that is
  not in the style doc because it only shows up here: **eighteen verbs should
  sound like one program**. Read them all in a row before changing any.
- **charts** — rule 6 is not a guideline here, it is the subject. Everything
  the urgency gauge got wrong (resolution where the ranking happens, visual
  mass rising with the value) is a question these three already answer, well
  or badly. Check whether they answer it the same way.
- **tui** — `dashboard`, `pick`, `settings`. All three need a pty. The
  dashboard also has an open disagreement with the style; see below.
- **manual** — prose, not layout. Rules 1, 7 and 12 still apply to its headings
  and tables; the rest largely do not.

## The loop, per family

1. **Read `docs/terminal-style.md` in full.** Every time, not from memory. It
   is short and it is the contract.
2. **Render every surface as it stands.** `scripts/snap.sh <name>-before <width>
   -- <args>`, at 80, 100 and 140, plus `THEME=mono`. Read the PNGs. Do not
   skip this because you can read the code: the code is what made the screen
   look like that.
3. **Write the audit** (bar item 2) into the task's annotation, before
   touching anything. It is the plan.
4. **Implement**, in the files the task names and no others.
5. **Add the guards** and prove they bite.
6. **Run the gates.** All four, in full.
7. **Render again**, same widths.
8. **Send it to a reviewer** (§"The reviewer").
9. **Fix what the reviewer found**, at most three rounds. Then stop, whatever
   state it is in — commit what is good, record the rest.
10. **Commit, annotate, move on.**

## Guards must bite

A test asserted but never seen red is a test whose failure mode is unknown —
`CLAUDE.md`'s rule, and the one that caught two broken fixes on this branch
already.

For each guard: revert the production behaviour it claims to hold, run that one
test, confirm it fails, restore. Script it so the evidence is a table you can
paste:

```
BITES   the_gauge_never_draws_more_ink_for_less_urgency  <- remainder drawn taller
SILENT  the_doc_still_describes_the_screens              <- doc keeps a dead glyph
```

A `SILENT` row is a finding about your test, not a formality. Fix the test.

## The reviewer

A separate agent, fresh context, told to **refute**. Not "review this" —
"find what is wrong with this". Give it exactly three things:

- `docs/terminal-style.md`
- the after-images (paths; it reads them)
- the diff for this screen

Ask for: rule violations, anything that reads worse than before, anything the
audit claims that the image does not support. Ask it to say plainly when it
finds nothing.

Record its verdict **verbatim** in the annotation. A summary of a review you
wrote yourself is not a review.

Run the reviewer on the strongest model available even when the main loop is
on a cheaper one. Judging a screen from a picture is the part that needs it.

## Stop rules

These are not suggestions. Each one is a way a night gets lost.

- **Three refinement rounds per family. Then stop.** Commit what works, write
  what does not, and let the next run have the night.
- **Three attempts at the same gate failure.** Then `git checkout --` the
  files, annotate why, and stop. A change you reverted is a result.
- **Never edit a test so it passes.** If a test disagrees with your change,
  one of you is wrong and it is usually not the test. Say so and move on.
- **Never rewrite the Contract table in `docs/terminal-style.md` to match new
  code.** That table is generated from the renderer by a guard; changing the
  doc to silence it removes the only thing keeping the guide honest. If the
  contract genuinely should change, change the code and let the guard tell you
  which rows moved.
- **A D-number is for a changed ruling, and only then.** Carrying D117 to
  another screen executes a ruling and needs none. A task that changes a
  ruling records the next free D-number in `DESIGN.md` §12, with the ruling,
  its one-line why and every ruling it amends, then walks the §11 phase tables
  and every other §12 entry that still describes the old behaviour.
- **Workers never push, merge or switch to `main`.** Commits stay on the
  branch, and the coordinator pushes.
- **A surface you cannot render is not a surface you skip.** Do the code work
  from the rules, and say in the annotation that the visual check did not
  happen. Then it is visible rather than assumed.

## Screens that need a pty

`dashboard`, `pick` and the settings screen draw into the alternate screen, so
the pipe-into-freeze loop does not reach them. Capture through `script(1)` first:

```console
$ script -qec "COLUMNS=100 tasqx pick" /dev/null > /tmp/pick.ansi
```

then feed that file to `freeze` the way `scripts/snap.sh` feeds it stdin. If
that does not work inside two attempts, stop trying: do the code work, say the
visual check did not happen, and leave the render for a human.

The dashboard has one open disagreement: its spec
(`docs/specs/2026-09-02-dashboard-redesign-design.md`) spells blocked `⛔`
where rule 4 says `⊘`. Use `⊘` — it is the newer decision, it is guarded, and
it is what `list` and `agenda` already draw — and record the change as a
**proposal for review**, not a settled ruling.

## Things that will waste your turns

Learned the hard way on the branch that produced the style.

- **`.claude/guard.sh` blocks on substrings.** A command containing the text of
  a dev-build path, or `git` in a shape it cannot verify, is refused whole.
  Keep shell commands plain and separate: `git add` with explicit paths (never
  `-A`), then `git commit -F <file>` as its own call. Write commit messages to a file rather than passing
  them inline. Avoid command substitution and heredocs in anything git- or
  binary-adjacent.
- **Driving a dev build needs both `TASQX_DB` pointed at a scratch store and
  `--no-daemon`,** in the same command. The guard enforces it and it is right
  to: without them you write to the real store.
- **Docs drift is a compile error here.** `docs.rs` carries hand-written
  samples of screen output, and tests cross-check them. If you change what a
  screen prints, its samples in `docs.rs` are now wrong — regenerate them, do
  not delete the guard.
- **`cargo fmt` after every edit round**, before the gates. Otherwise the fmt
  gate fails on something unrelated to what you are debugging.

## Reporting

One run is one screen. End every turn with one line, exactly this shape, so a
reader — or a `/goal` evaluator, which sees only what you print — can tell
where the run is without reading the turn:

```
SCREEN <name> · turn N/12 · round R/3 · gates <green|red|not run> · <one clause>
```

When the screen is finished, or when a stop rule fires, print **one message**
carrying all five of these under those exact labels, and then `SCREEN DONE` on
its own line:

- `AUDIT:` thirteen lines, one per rule in `docs/terminal-style.md`
- `BITES:` the table from §"Guards must bite"
- `GATES:` the four commands and their result lines
- `REVIEW:` the reviewer's verdict, verbatim
- `COMMIT:` the sha of the single commit holding this screen

The evaluator cannot read files or run commands. It reads what you printed. A
gate you ran but did not paste did not happen, as far as anything downstream is
concerned — and as far as the person reading the log in the morning is
concerned, which is the same thing.

Print `SCREEN DONE` when a stop rule fires too, with whatever you have and a
plain sentence saying which rule fired. A screen that stopped after two rounds
and said why is a result. A run that keeps going rather than admit it is out of
budget is how a night gets spent on one screen.

## Models

Run the reviewer on the strongest model available even when the main loop is on
a cheaper one — `Agent` takes a `model` override. The rules in
`docs/terminal-style.md` are written down, so carrying them is execution; the
part that still needs judgement is looking at a picture and saying what is
wrong with it, and that is the reviewer's whole job.
