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

- `--standing` marks the doc as standing: a ruling that belongs in every
  session of its project (or every project, when unscoped) until it is
  cleared, so a correction given once is not forgotten by recency. MCP
  clients receive standing docs when a session starts. Past 15
  standing docs in one scope, `add` answers with a hint to merge or retract.
- `--source` names where the doc came from, and names one doc: a source
  another document already holds is refused with `conflict` (exit 5), naming
  that document, so `import` always knows which one to replace. The same
  goes for `update --source`.

## tasqx memory list

Browse your documents without a query — the enumeration `search` cannot do,
since ranking needs words to rank.

```console
tasqx memory list
```

- On a terminal this opens a full-screen browser: `j`/`k` move, `/` searches
  as you type, Enter reads the document beside the list, `q` leaves.
- Piped, under `--json`, or with `--limit`/`--offset`, it prints one line per
  document instead — a screen would be the wrong answer to a pipe.
- Newest-modified first, paged the same way as `tasqx list`.
- `--standing` lists only the standing docs (and prints the table).

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
- A doc hit whose origin file has changed since it was imported (or last
  refreshed) is marked `stale`, so you can tell a ruling that is behind its
  file from one that still matches it.

## tasqx memory show

Read one document whole, by the id a search hit gave you.

```console
tasqx memory show 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12
```

## tasqx memory update

Correct a document in place, keeping its id.

```console
tasqx memory update 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12 --body "corrected text"
```

- `--title`, `--body`, `--source`, `--project` and `--standing true|false`
  each replace that field; omit one and it stays as it was.
- Guarded by the same optimistic-concurrency rev `tasqx modify` uses:
  `--expected-rev` fails with `conflict` (exit 5) if the document moved under
  you since you read it.
- This is the correction path, and the reason `rm` is rarely the right verb:
  adding a fixed copy leaves the wrong one searchable, and removing loses the
  id every search hit and annotation pointed at.

## tasqx memory import

Turn a folder of markdown into searchable memory — one document per file, the
title taken from the first `#` heading.

```console
tasqx memory import docs/adr
```

- One transaction: if any file fails, nothing is imported.
- Re-importing the same directory *replaces* those documents instead of
  duplicating them, so it's safe to re-run whenever the sources change.
  Over the JSON API, a batch that names one source twice is refused whole.
- A replace bumps the document's revision, so an `update --expected-rev`
  taken before the re-import is refused with `conflict` instead of silently
  overwriting the freshly imported text.
- `--project` scopes every document in the batch, so an agent's own repo docs
  land in that project's memory instead of unscoped. Omitted, a new document
  stays global and an existing one keeps whatever scope it already had (like
  `--standing` on a re-import); naming a project moves an existing document's
  scope there, so re-pointing an import at a different project rescopes it.
- The stored `source` is the path relative to the git toplevel above the
  file, or, outside a git work tree, relative to the current directory — so
  the same folder imported as `docs/`, `./docs/` or its absolute path, from
  any directory and any machine, is one document per file, not several. An
  import that finds a doc already holding an older spelling of the same file
  name never removes it, but prints a `note:` line naming it so it can be
  retired by hand. A symlink pointing at another file in the same import
  collapses onto it too — same `source` once resolved — so only the real file
  becomes a document, and the alias is named in a `note:` line instead of
  refusing the batch.
- `tasqx memory import --refresh` takes no path. It walks the documents
  already imported, compares each against the file it was read from, and
  re-reads the ones whose file has changed — keeping the document's id, its
  project and its standing flag, and bumping its revision. A file that was
  only touched, without its text changing, counts as unchanged: the stored
  size and timestamp are a fast filter, and the bytes are what decides. A
  document whose file can no longer be read is *named and kept*, never
  deleted — the file may be on another machine or a volume that is not
  mounted — and the command still exits 0, so a refresh in a script does not
  fail over a file somebody moved. An MCP session runs this same sweep on its
  own `initialize` when its scope allows writes, so an agent's session opens
  on the working tree without a watcher or a daemon job (D180).
- Each imported document also records where it came from: `origin_path` (the
  file's absolute path on this machine), `origin_mtime` (its modification
  time in unix seconds) and `origin_size` (its size in bytes), all shown by
  `memory show --json` and carried by `export`. They say what the file looked
  like when it was read, so a later check can tell whether it has changed
  since; identity is still `source`, and `memory add` never sets them.

Put together, this is one model, not three separate features: memory
remembers which file a document came from, and checks that file only when
you ask — a search, a brief, `--refresh`, or an agent session starting — never
by watching the filesystem in the background. Nothing is ever deleted because
a file moved or disappeared; it's named so you can decide what to do about it.

## tasqx memory rm

Remove one document, permanently, by id.

```console
tasqx memory rm 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12
```

This is the one genuinely permanent delete in tasqx, and it exists on purpose:
something stored wrong needs a way to be retracted. It is outside `undo`.

Reach for it only when the document should not exist at all. To fix what one
*says*, [`tasqx memory update`](#tasqx-memory-update) rewrites it in place and
keeps the id — no permanent loss, and nothing stale left in the index.

## Why this matters for AI agents

An agent connected over [MCP](AI-Agents-and-Automation.md) reaches this same
store — searching works even on a read-only connection. Feed it your ADRs and
runbooks with `memory import`, and past decisions surface while the agent
works. And because annotations are indexed too, an agent that documents its
work as it completes tasks is building the knowledge base as a side effect.
