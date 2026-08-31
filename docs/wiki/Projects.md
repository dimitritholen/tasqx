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

```console
tasqx use home     # from now on, `tasqx add ...` lands in home
```

- The project must already exist and not be archived.
- `tasqx projects` marks the current default with a `*`.

## tasqx projects

List your projects.

```console
tasqx projects          # the live ones, default marked with *
tasqx projects --all    # include archived ones
```

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
- There is no `unarchive` command. The way back is a data restore: a saved
  [`tasqx export`](Import-and-Export.md) contains the project's state, and
  importing it puts things back the way they were.
