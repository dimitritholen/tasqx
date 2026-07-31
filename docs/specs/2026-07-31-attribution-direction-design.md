# Attribution direction — self-report primary, log-parse refuses contested (D50)

**Date:** 2026-07-31
**Status:** approved in interactive session, pending implementation
**Closes:** #78 (by policy), #79 (by mechanism), the `tokens_total` open question from D48
**Required reading:** the `attribution-window-wrong-abstraction` memory; #78's
2026-07-30 annotation (the parked-attempt post-mortem); `DESIGN.md` §12 D48.

## Context

Log-parse attribution infers a task's token spend from a time window
`[start_event_ts, done_event_ts]` over a shared transcript. Measurement against a
real daemon showed the window is milliseconds wide (20 ms observed) while the
paying agent turn lasts minutes, so the spend lands outside the window as a rule
(#78). `UsageSample` carries no task identity and nothing dedupes, so overlapping
windows bill one spend to several tasks at `confidence: high` (#79) — ~1.5 M
tokens are double-counted in the live store today. Every pure-window fix (grace
period, close-at-next-event, terminal `unmeasured` marker) was refuted by measured
data; see the post-mortem on `wip/attribution-78-attempt` (`be5d1c3`, must never
be merged).

The conclusion this design acts on: **time correlation cannot establish ownership
of a token spend.** Ownership is provenance, and only the caller has it.

## Decision 1 — Self-report becomes the primary channel

`task.done` already accepts `tool`, `model`, `input_tokens`, `output_tokens`,
`cache_read_tokens`, `cache_creation_tokens` and writes a
`source=self-report, confidence=medium` row in the done transaction (#13,
`ea6993e` + `0799bdb`). Rank changes, not shape:

- The MCP tool description for `tasqx_complete_task` instructs agent callers to
  pass their turn's token counts at completion. Log-parse is described as the
  fallback for multi-turn and untended work.
- Self-report stays `confidence: medium`. Confidence describes verifiability, not
  preference: a self-report is an unverified claim. The trust hierarchy lives in
  `source`; display and roll-up precedence is by source, not confidence.
- A task that already carries a self-report measurement is skipped by log-parse
  entirely. One task never accumulates rows from both channels.

## Decision 2 — Fallback log-parse refuses contested samples

- Contestedness is decided purely by window overlap among open tasks: a sample
  falling inside more than one open task's window is banked for **no one**.
- Window overlap alone is not cross-tick coherent, so a second rule composes
  with it: **global identity claims**. A banked measurement records the sample
  ids it consumed in its `tokens.attributed` payload, and a claimed id is
  refused store-wide on every later tick regardless of what its current
  timestamp says. Identity is the backbone: transcript stamps move between
  mid-write reads, so a banked decision is final only when pinned to sample
  identity, not to the stamp of the hour. The claim set is deliberately global —
  not joined through path or session equality — because source identity
  re-derived from a live filesystem dissolves when a path stops resolving
  (dangling symlink, deleted transcript). Samples without an id (parsers other
  than Claude Code, plus the undocumented Claude Code line shape without
  `message.id`) keep window-only semantics; their stamps were verified stable
  across re-reads, so identity adds nothing there — recorded as an assumption,
  not a guarantee.
- Affected tasks stay **transient** on the existing #73 give-up deadline. No
  terminal marker of any kind — a terminal state is what sank the parked attempt,
  because mid-write transcript timestamps are not monotonic (38 of 192 real
  transcripts) and a sample can become readable or move into range later.
- The daemon's `done` response for a task that has no measurement carries an
  informational nudge to self-report. Text only; it asserts nothing about
  ownership or spend (the parked attempt's daemon line asserted "spent tokens" on
  evidence that included zero-token lines — 16 exist across 10 real transcripts).

**Accepted limitation, documented rather than solved:** if task A self-reports
and task B log-parses the same transcript over the same turn, the same spend can
appear under both sources. A self-report does not identify which samples it
covers, so sample-level reconciliation across channels is not attemptable. The
same is true of a turn that genuinely advances three tasks: no partition of a
time axis recovers the split, and this design stops pretending one exists.

**Disposition of the open defects:** #79's double-count mechanism is closed by
the refusal rule. #78's right-edge miss is closed by policy: a solo late-flush
stays unmeasured by the window, transiently, with the nudge — self-report is the
fix, and the tool contract now says so.

## Decision 3 — Historical repair: one-shot recompute migration

A migration re-runs attribution over the stored windows under the refusal rule:

- It recomputes **every** log-parse measurement, not only those from overlapping
  windows, in deterministic order, rebuilding the identity-claim set as it goes.
  This is load-bearing: measurements banked before the identity fix carry no
  `sample_ids`, so a moved-stamp theft against a pre-upgrade bank is precisely
  *not* window-contested — only a full recompute with claims rebuilt closes that
  upgrade window, and it backfills `sample_ids` on surviving rows as a side
  effect.
- Contested samples drop out; uncontested measurements survive unchanged.
- Rows whose transcript is no longer readable cannot be recomputed. Those are
  downgraded — `confidence` set to `low` — never deleted blind.
- **Dry-run mode first**: print the per-task delta (who loses what, who keeps
  what) before anything is written.

This removes the ~1.5 M-token double-count from 2026-07-25 (windows
`019f98a4-5ddf` ⊂ `019f98a4-5dca`, and the 7-of-15 overlapping pairs) or, where
recompute is impossible, at least strips its `high` label.

## Decision 4 — `tokens_total` leaves `--json` and the API

The field is removed; the four buckets remain. D48(a)'s "never blended on any
output surface" then holds uniformly. The D48 decision-log entry in `DESIGN.md`
§12 is updated to note the API exception was closed. Breaking, accepted
deliberately at 0.2.0 with effectively one consumer: a downstream sum becomes an
explicit choice instead of an ambient default.

## Rollout — four slices, in order

1. **Refusal** — contested-sample refusal in `attribution.rs` plus
   skip-if-self-reported. Closes #79's mechanism.
2. **Contract** — MCP tool description, daemon nudge line, docs. Closes #78 by
   policy.
3. **Migration** — the recompute with dry-run. Repairs history.
4. **`tokens_total` removal** — independent and smallest, last.

Each slice ships alone: `cargo test --workspace`, clippy, reinstall, restart the
MCP server. Attribution changes are verified against a real daemon
(`scratchpad/attack78.sh` ATTACK 3 is the reproduction for the #79 case), not
only by test — three green suites shipped live defects during the field test.

## Out of scope

- `prompt_id` stays as-is: accepted everywhere, read by nothing, harmless. Whether
  to remove it or make it load-bearing is a new backlog task, not part of D50.
- #74 (OTLP outranking a complete transcript) is adjacent but separate; the
  source-precedence rule introduced here should inform it, not solve it.
- Cross-source deduplication (the accepted limitation above).

## Constraints carried forward

- Never trade a transient state for a terminal one (`has_attributed_event` makes
  any marker permanent).
- Any timestamp rule must survive non-monotonic mid-write reads at 2 Hz.
- The D-number D50 was checked against `main`'s `DESIGN.md` (D49 is the last
  heading), not a branch copy.
