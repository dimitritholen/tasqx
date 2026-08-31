# Memory

tasqx can remember more than tasks. The memory system stores knowledge —
runbooks, decisions, conventions, research — and makes it searchable together
with every task annotation you've ever written. For AI agents this is the
difference between starting cold every session and starting informed.

## tasqx memory add

Store one knowledge document. The body is kept exactly as given.

```console
tasqx memory add "Deploy runbook" "Deploys go through the blue-green pipeline"
```

## tasqx memory search

Full-text search over your documents *and* your task annotations, ranked by
relevance (bm25).

```console
tasqx memory search blue-green
```

- Plain words are matched as phrases, so hyphens and dots are safe to type.
- Power users can pass `--raw` for FTS5 operator syntax.
- The answer includes the query that actually ran, so "no hits" is
  distinguishable from "nothing stored about this".

## tasqx memory show

Read one document whole, by the id a search hit gave you.

```console
tasqx memory show 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12
```

## tasqx memory import

Turn a folder of markdown into searchable memory — one document per file, the
title taken from the first `#` heading.

```console
tasqx memory import docs/adr
```

- One transaction: if any file fails, nothing is imported.
- Re-importing the same directory *replaces* those documents instead of
  duplicating them, so it's safe to re-run whenever the sources change.

## tasqx memory rm

Remove one document, permanently, by id.

```console
tasqx memory rm 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12
```

This is the one genuinely permanent delete in tasqx, and it exists on purpose:
something stored wrong needs a way to be retracted. It is outside `undo`.

## Why this matters for AI agents

An agent connected over [MCP](AI-Agents-and-Automation.md) reaches this same
store — searching works even on a read-only connection. Feed it your ADRs and
runbooks with `memory import`, and past decisions surface while the agent
works. And because annotations are indexed too, an agent that documents its
work as it completes tasks is building the knowledge base as a side effect.
