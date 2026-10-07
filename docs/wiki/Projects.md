# Projects

A project in tasqx is just a name that groups tasks — `work`, `home`,
`kitchen-remodel`. No folder is created, nothing appears on your disk. Tasks
always belong to exactly one project.

## tasqx init

Create a project.

```console
tasqx init work
tasqx init home --desc "Everything around the house"
```

- If the store has no default project yet, the new one claims it. So on a fresh
  install, `tasqx init work` is all the setup you need.
- Creating a second project does *not* move the default. Use
  [`tasqx use`](#tasqx-use) for that.

## tasqx use

Choose the default project — the one a bare `tasqx add` files tasks into.

After this, `tasqx add ...` lands in `home`:

```console
tasqx use home
```

- The project must already exist and not be archived.
- `tasqx projects` marks the current default with a `*`.

## tasqx projects

List your projects.

| Command | What it does |
|---|---|
| `tasqx projects` | The live ones, with the default marked `*` |
| `tasqx projects --all` | Include archived ones |

`--all` is the only way to see archived projects — without it the table shows
only the projects that `add` and `use` will accept.

## tasqx archive

Retire a project. Think shelf, not shredder: the tasks keep their history and
stay in the store, the project just leaves the rotation.

```console
tasqx archive kitchen-remodel
```

Things to know before you archive:

- Once archived, no command will accept the project's name anymore — you can't
  `use` it, and you can't `add` or `modify` a task into it.
- If you archive the project that *is* your default, the default is cleared:
  a bare `tasqx add` then has no home until you run `tasqx use` again. The
  command tells you when this happened.
- Tasks in an archived project are left out of `tasqx next` and of the
  `@working` set — the dashboard's working list included — because those answer
  "what should I do now". Name the project and they come back:
  `tasqx next project:kitchen-remodel`. `list`, `report` and `agenda` still
  show them. The rule applies to any filter that mentions `@working`, so
  `@working or +x` also hides a pending `+x` task in an archived project; leave
  `@working` out of the filter to see it.
- `tasqx unarchive` is the way back. `tasqx undo` does not reverse an archive,
  because archiving the default project also cleared the default, which the
  log does not restore.

## tasqx unarchive

Put an archived project back into rotation.

```console
tasqx unarchive kitchen-remodel
```

- The project's tasks never left, so nothing else is restored; the line says
  how many open tasks are back in `next`.
- A project that is not archived exits 5, an unknown name exits 4.
- It does not point the default project back at it. `tasqx use` does that.
