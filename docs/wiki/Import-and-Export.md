# Import and Export

Your data, in and out, as plain JSON. This is the backup story, the migration
story, and the "way back" for anything archived.

## tasqx export

Dump tasks as canonical JSON to stdout, or to a file with `--out`.

- `tasqx export project:work > work.json`: any filter narrows it
- `tasqx export --out backup.json`: writes the same document to a file,
  replacing it whole, so an export that fails halfway leaves the previous
  backup as it was, and prints where it went and what it left out

```console
tasqx export > backup.json
tasqx export project:work > work.json
tasqx export --out backup.json
```

- The document carries projects, your memory docs, the graph's links and your
  default-project setting too, so a restore gives back the store, not just its
  tasks.
- A filtered export that cuts across a dependency (one task in, its
  prerequisite out) drops that edge and *says so* in the answer
  (`dropped_dependencies`), rather than exporting a broken reference. A link
  (`link.add` in the API) is kept only when *both* of its ends are in the document, and
  the rest are counted under `dropped_links` for the same reason.
- **An unfiltered export carries every project, every memory doc, every link
  and the event log** — the full backup. What it leaves out is what you
  removed, and each kind is counted (see [Checking a restore](#checking-a-restore)).
  The one link it leaves out is
  one pointing at an annotation you removed: a removed annotation is not in the
  document, so the edge to it is dropped and counted under `dropped_links` like
  any other. Any *other* filter scopes those
  four to what the exported tasks actually need, and reports what it left out
  (`dropped_projects`, `dropped_docs`, `dropped_links`, `dropped_events`):
  sharing
  `tasqx export project:ledger` no longer ships every other project's
  knowledge along with it. A doc that carries no project ships only with
  `--include-unscoped`, which is refused on an unfiltered export — there is
  nothing to widen from. The store's default project is carried only when
  it is among the projects the filter kept, so a filtered export whose
  scope drops it comes back with no default rather than a document
  `tasqx import` would refuse.

```console
tasqx export project:work --include-unscoped > work-and-global.json
```

## tasqx import

Load tasks from a JSON file, or from stdin with `-`.

- `tasqx export | tasqx import -`: round-trip

```console
tasqx import backup.json
tasqx export | tasqx import -
```

A task whose number is already taken by a *different* task in this store keeps
its id and gets the next free number instead of refusing the import; every move
is listed under `renumbered`, with the number it came in as and the one it took.
Dependencies, annotations and checks follow the id, so nothing breaks — but a
branch name or a `#n` written down somewhere still points at the old number.
A task this store already holds keeps the number it has here, so importing the
same document twice changes nothing.

A memory doc whose `source` a *different* doc in this store already holds is the
same doc under two ids — which is what two machines that each imported the same
`docs/` folder end up with — so it merges onto the doc already here instead of
refusing the import. The stored id is kept, the payload's is dropped, and the
later `modified` wins the text; a payload no newer than the copy here leaves it
untouched. Every merge is listed under `docs_merged`, with the source, the kept
id, the copy that won, and the dropped id — `null` when the payload document
carried no id of its own — and links pointing at the dropped id follow the doc
that kept the source.

Links come in last, after everything they point at, and are counted under
`links_imported`. Both ends are resolved against what this store holds *after*
the import — a project it already knew keeps its own row, and the link follows
it there. A link naming an end that is neither in the document nor already
here refuses the whole import, naming the link and the missing end, rather
than restoring an edge nothing can see.

Import is also the one way to un-archive a project: the export document
records each project's archived flag, and importing restores it. See
[Projects](Projects.md#tasqx-archive).

A project this store already knows keeps its own description when it has one:
projects carry no `modified` stamp the way a task does, so there is no
later-wins comparison to make, only a present-vs-absent one. A non-empty local
description always wins over the payload's, which is dropped and reported
under `project_description_conflicts`; a local description that is empty, or
a project this store has never seen, takes the payload's.

```console
note: project "tasqx" kept its description here; the import's was "B's tasqx description"
```

`tasqx import backup.json --dry-run` runs the whole import against this store
and rolls it back instead of keeping it: every renumbering, merge and refusal
prints exactly as a real import would, and the run ends with `nothing was
written`. There is no separate preview logic — it is the same import, undone.

```console
tasqx import backup.json --dry-run
```

`tasqx import other.json --merge` folds a second live store into this one
instead of restoring over it. For a task this store *already* holds, the
annotations, checks, tags and dependency edges written on either side are
unioned — nothing written on either side is thrown away. A
field that can only have one value (title, status, priority, project, the
dates) is taken from whichever side changed *that field* last, going by each
side's history, so a task completed here and retitled there comes back
completed and retitled. Status travels with its completion time, and the four
dates travel together. Tracked time adds up: the time the other store logged
since the two last met joins the total here instead of replacing it, and
importing the same document again adds nothing. The `_rev` check is skipped
for those tasks, because two stores count their own revisions and neither
number means anything to the other. One line per task names each field that
differed and the side it came from, and how much tracked time arrived. A note or a check both sides hold under the same id stays one row: a check
follows its own last edit, so a `passed` recorded here is not rolled back by a
copy that never saw it pass, and a note follows the task's. A task the document
carries and this store has never seen is imported exactly as it would be
without the flag.

Removals travel too. A tag, check, note, dependency edge, link or memory doc
removed on one machine stays removed when that machine merges the other's
older copy, and is removed on the other machine when it merges this one. Each
side's history decides: a removal counts when it is later than the last time
the row was added or changed, so a tag added back after it was removed
elsewhere stays. A removed note is scrubbed on the other machine the same way
`tasqx unannotate` scrubs it here: its text leaves the file.

Without `--merge`, the document stays authoritative about a task it names: its
notes, checks, tags and edges replace what is here. That is what a restore
should do, so it stays the default.

Either way, a note or a check id the document hands to one task while a
*different* task here already holds it refuses the whole import, naming both
tasks: one note, and one criterion, belongs to exactly one task.

A recurring task completed on both machines spawned its next occurrence on
each, under two different ids. When an import finds two tasks with the same
predecessor and the same due and scheduled dates, with or without `--merge`,
they fold into the older one: it takes the other's notes, checks, tags and
dependency edges, the status of whichever copy was started, stopped or
finished last, and the tracked time of both, and the other copy is deleted.
Each fold prints one line, for example
`note: occurrence #12 duplicated #9 (both spawned from #4); kept #9`, and is
listed under `deduplicated`. Both machines keep the same copy with the same
contents, so importing in the other direction, or the same file twice, ends
in the same place. If the two copies were wired into opposite ends of one
dependency chain, the survivor's edge that would close the loop is dropped and
the note names it. Completing a task again after reopening it does not spawn a
second next occurrence for the same date.

## Checking a restore

A store exported and imported into an empty one comes back with every task,
project, memory doc, link, check, tag, dependency, timer and note, and the
event history behind them. Counted straight from the database, three numbers
still differ, on purpose, and `tasqx export --out backup.json` names each one
under the path it wrote (`--json` gives the counts as fields):

```text
Wrote 240 tasks → /home/me/backup.json
Left out: 3 removed notes, 12 events earlier imports logged about themselves
```

- `removed_annotations`: notes you removed. Removing a note scrubs its text
  and keeps an empty row; that row stays behind, and the record of the
  removal travels in the event log.
- `dropped_events`: on an unfiltered export, the history of a memory doc you
  removed, apart from the removal itself, because it names the title the
  removal took away. This one is also in the document.
- `skipped_events`: the entries an earlier import wrote about itself.

The import adds entries of its own, one per task, project and doc it brought
in, reported as `events_logged` beside `events_imported`. So the store's event
log is the document's `events` plus `dropped_events` and `skipped_events`, and
the restored one holds `events_imported` plus `events_logged`.

The simplest check is to export the restored store and compare: the document
is the same, apart from `dropped_events` and each task's `urgency`, which is
scored when the export runs.

```console
tasqx export --out before.json
tasqx import before.json
tasqx export --out after.json
```

Run the import and the second export against the new store.

## Merging two machines

Each machine exports, and each imports the other's document. Rehearse with
`--dry-run` first — it prints the same report and keeps nothing.

On machine A:

```console
tasqx export > a.json
```

Copy `a.json` to machine B, then there:

```console
tasqx import a.json --merge --dry-run
tasqx import a.json --merge
tasqx export > b.json
```

Copy `b.json` back to machine A and do the reverse:

```console
tasqx import b.json --merge --dry-run
tasqx import b.json --merge
```

Both stores now hold both sides' work.

## tasqx sync

The same merge, without carrying files around: each machine syncs with one
shared remote. Choose the remote once per store, then sync whenever you like.

```console
tasqx sync setup dir --set path=/mnt/share/tasqx
tasqx sync
tasqx sync --status
```

`tasqx sync` pulls what the remote holds, merges it into this store exactly as
`import --merge` does, and pushes the result back. If another machine pushed
in between, it merges that too and tries again, three times at most. The
remote is a connector program, `tasqx-remote-<name>` on your `PATH`:
`tasqx-remote-dir` keeps it in a folder every machine can reach, and
`tasqx-remote-r2` in a Cloudflare R2 bucket.

Every snapshot is encrypted on this machine before the connector sees it, so
the folder or the bucket only ever holds ciphertext. `sync setup` asks for a
passphrase (or reads `TASQX_SYNC_PASSPHRASE` when there is no terminal); give
every machine that syncs with the remote the same one. Lose it and the
remote's snapshots cannot be read, though each machine's own store is
untouched.

## Good habits

A dated backup, in one line:

```console
tasqx export --out "backup-$(date +%F).json"
```

The store itself is a single SQLite file (`tasqx config store` prints its
path), so file-level backups work too — but the JSON export is
human-readable, diff-able, and version-control friendly.
