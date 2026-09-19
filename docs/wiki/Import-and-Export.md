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

- The document carries projects and your default-project setting too, so a
  restore gives back the store, not just its tasks.
- A filtered export that cuts across a dependency (one task in, its
  prerequisite out) drops that edge and *says so* in the answer
  (`dropped_dependencies`), rather than exporting a broken reference.
- **An unfiltered export carries every project, every memory doc and the
  whole event log** — the full backup. Any *other* filter scopes those three
  to what the exported tasks actually need, and reports what it left out
  (`dropped_projects`, `dropped_docs`, `dropped_events`): sharing
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

Import is also the one way to un-archive a project: the export document
records each project's archived flag, and importing restores it. See
[Projects](Projects.md#tasqx-archive).

## Good habits

A dated backup, in one line:

```console
tasqx export > "backup-$(date +%F).json"
```

The store itself is a single SQLite file (`tasqx config store` prints its
path), so file-level backups work too — but the JSON export is
human-readable, diff-able, and version-control friendly.
