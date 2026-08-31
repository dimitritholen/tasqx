# Getting Started

tasqx is a task manager that lives in your terminal. Your tasks are stored in a
single SQLite file on your own disk — no account, no cloud, works offline.

## Install

Linux and macOS:

```console
curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh
```

Windows (the first line makes older PowerShell able to download at all):

```console
[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1 | iex
```

Prebuilt binaries for Linux, macOS and Windows are also on the
[Releases page](https://github.com/dimitritholen/tasqx/releases), and you can
build from source with `cargo install --path crates/tasqx-cli`.

## The whole loop is four commands

```console
tasqx init work        # create a project — just a name, no folder
tasqx add Buy milk     # capture a task (lands in the default project)
tasqx next             # the one thing to do now
tasqx done 1           # complete it
```

That's a working task manager. Everything else on this wiki is optional depth.

A few things worth knowing on day one:

- **A project is just a name.** `init` creates no folder and touches nothing on
  disk except the task store. The first project you create becomes the default,
  so bare `tasqx add` knows where to file things.
- **Every task gets a short id** (`#1`, `#42`). That id is how you refer to it
  in every other command: `tasqx done 42`, `tasqx show 42`.
- **Capture takes shortcuts inline.** `tasqx add Ship it due:friday +api !high`
  sets a due date, a tag and a priority in one line. See
  [Adding and Editing Tasks](Adding-and-Editing-Tasks.md).
- **A bare `tasqx` in a terminal opens the dashboard** — a full-screen overview.
  In a script or pipe it prints the task table instead. See
  [Dashboard and Live View](Dashboard-and-Live-View.md).

## Getting help

tasqx ships its own documentation — no internet needed.

### tasqx manual

*Alias: `man`*

The complete guide, in your terminal. `tasqx manual` shows the table of
contents; `tasqx manual <command>` or `tasqx manual <topic>` opens one section.

```console
tasqx manual            # table of contents
tasqx manual add        # everything about `add`
tasqx manual filters    # the filter language
```

### tasqx docs

The same guide as one self-contained HTML page, opened in your browser. No
external requests, nothing tracked — it's generated from the binary itself.

```console
tasqx docs              # open in the browser
tasqx docs --out guide.html   # write the file instead
```

### Per-command help

Every command answers `-h` with usage, flags and copy-pasteable examples:

```console
tasqx add -h
```

## Where your data lives

The store is a SQLite file at `$TASQX_DB` if you set it, otherwise in your
platform's data directory. `tasqx config store` tells you exactly which file a
command would write to. Back it up any time with
[`tasqx export`](Import-and-Export.md).
