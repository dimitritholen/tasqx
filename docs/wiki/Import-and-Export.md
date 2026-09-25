# Import and Export

Your data, in and out, as plain JSON. This is the backup story, the migration
story, and the "way back" for anything archived.

## tasqx export

Dump tasks as canonical JSON to stdout.

- `tasqx export project:work > work.json`: any filter narrows it

```console
tasqx export > backup.json
tasqx export project:work > work.json
```

- The document carries projects, your memory docs, the graph's links and your
  default-project setting too, so a restore gives back the store, not just its
  tasks.
- A filtered export that cuts across a dependency (one task in, its
  prerequisite out) drops that edge and *says so* in the answer
  (`dropped_dependencies`), rather than exporting a broken reference. A link
  (`tasqx link`) is kept only when *both* of its ends are in the document, and
  the rest are counted under `dropped_links` for the same reason.
- **An unfiltered export carries every project, every memory doc, every link
  and the whole event log** — the full backup. The one link it leaves out is
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
unioned — nothing here is thrown away, and nothing in the document is. A
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

Without `--merge`, the document stays authoritative about a task it names: its
notes, checks, tags and edges replace what is here. That is what a restore
should do, so it stays the default.

Either way, a note or a check id the document hands to one task while a
*different* task here already holds it refuses the whole import, naming both
tasks: one note, and one criterion, belongs to exactly one task.

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

## Good habits

A dated backup, in one line:

```console
tasqx export > "backup-$(date +%F).json"
```

The store itself is a single SQLite file (`tasqx config store` prints its
path), so file-level backups work too — but the JSON export is
human-readable, diff-able, and version-control friendly.
