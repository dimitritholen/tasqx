# Import and Export

Your data, in and out, as plain JSON. This is the backup story, the migration
story, and the "way back" for anything archived.

## tasqx export

Dump tasks as canonical JSON to stdout.

```console
tasqx export > backup.json
tasqx export project:work > work.json    # any filter narrows it
```

- The document carries projects and your default-project setting too, so a
  restore gives back the store, not just its tasks.
- A filtered export that cuts across a dependency (one task in, its
  prerequisite out) drops that edge and *says so* in the answer
  (`dropped_dependencies`), rather than exporting a broken reference.

## tasqx import

Load tasks from a JSON file, or from stdin with `-`.

```console
tasqx import backup.json
tasqx export | tasqx import -     # round-trip
```

Import is also the one way to un-archive a project: the export document
records each project's archived flag, and importing restores it. See
[Projects](Projects.md#tasqx-archive).

## Good habits

```console
tasqx export > "backup-$(date +%F).json"   # dated backup, one line
```

The store itself is a single SQLite file (`tasqx config store` prints its
path), so file-level backups work too — but the JSON export is
human-readable, diff-able, and version-control friendly.
