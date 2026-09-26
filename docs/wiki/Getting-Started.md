# Getting Started

tasqx is the organiser for your AI: a backlog, a memory and a brief for your
coding agent, and a task manager that lives in your terminal. Your tasks are
stored in a single SQLite file on your own disk — no account, no cloud, works
offline.

## Install

With a package manager — updates then come from `brew upgrade tasqx` /
`scoop update tasqx`, and brew switches Tab completion on by itself.

macOS and Linux, with Homebrew:

```console
brew install dimitritholen/tasqx/tasqx
```

Windows, with Scoop:

```console
scoop bucket add tasqx https://github.com/dimitritholen/scoop-tasqx
scoop install tasqx
```

Or without one — Linux and macOS:

```console
curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh
```

Windows (the first statement makes older PowerShell able to download at all):

```console
[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1 | iex
```

Prebuilt binaries for Linux, macOS and Windows are also on the
[Releases page](https://github.com/dimitritholen/tasqx/releases), and you can
build from source with `cargo install --path crates/tasqx-cli`.

## Install fine print

Both installer scripts pick the newest release, resolve your target triple,
verify the archive against its published checksum, and put files only in the
install directory: `~/.local/bin`, or `%LOCALAPPDATA%\Programs\tasqx\bin` on
Windows. The Windows installer also adds that one directory to your user PATH,
a setting outside it, and `-Uninstall` takes it back out. Neither touches a
shell startup file unless you ask.

A pipe passes no arguments, so flags need the longer form:

```console
curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh -s -- --dry-run
```

```console
&([scriptblock]::Create((irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1))) -DryRun
```

The rest are `--uninstall`/`-Uninstall`, `--completions`/`-Completions` and
`--help`/`-Help`; every switch also has an environment variable
(`TASQX_UNINSTALL`, `TASQX_DRY_RUN`, …), `TASQX_VERSION` pins a tag, and
`TASQX_INSTALL` moves the destination.

Honesty about what that buys you:

- **The checksum is integrity, not provenance.** It catches a truncated
  transfer or a corrupt CDN object; it is served from the same host as the
  archive, so it proves nothing about who built it. Nothing here is signed.
- **The binaries are unsigned.** On macOS the curl route goes *around*
  Gatekeeper rather than passing it — that is why the install just works.
- **The Linux build links your system glibc** (SQLite is bundled; there is no
  other runtime). The floor is whatever GitHub's current `ubuntu-latest`
  provides; re-derive it from a release binary with
  `objdump -T tasqx | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1`. There is
  no musl build — an older distro builds from source.

All of it applies to the package-manager routes too: the Homebrew formula and
the Scoop manifest are generated per release (`scripts/brew-formula.sh`,
`scripts/scoop-manifest.sh`) from the same published checksums, and point at
the same unsigned archives.

## The whole loop is four commands

- `tasqx init work`: create a project — just a name, no folder
- `tasqx add Buy milk`: capture a task (lands in the default project)
- `tasqx next`: the one thing to do now
- `tasqx done 1`: complete it

```console
tasqx init work
tasqx add Buy milk
tasqx next
tasqx done 1
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
`projects` and `daemon` are both a command and a topic, and open both pages.

| Command | What it does |
|---|---|
| `tasqx manual` | Table of contents |
| `tasqx manual add` | Everything about `add` |
| `tasqx manual filters` | The filter language |

### tasqx docs

The same guide as one self-contained HTML page, opened in your browser. No
external requests, nothing tracked — it's generated from the binary itself.

| Command | What it does |
|---|---|
| `tasqx docs` | Open in the browser |
| `tasqx docs --out guide.html` | Write the file instead |

### tasqx about

Who made tasqx, where to find it, and what build you are on. Five labelled
rows: the author, his LinkedIn, the project on GitHub, the build this binary
was made from, and the store it would open. It opens no store and no network —
the store line is the path a command *would* use, resolved without creating
anything.

```console
tasqx about
```

The build line is the same string `tasqx --version` prints: the crate version
plus the commit it was built from, or `unknown` for a build from a source
tarball, which has no git to ask. Quote it when you report a bug.

`tasqx about --notices` prints the third-party notices: the attribution and
licence texts of the embedding model built into the binary for memory search.

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
