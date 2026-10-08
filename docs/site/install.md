# Install and quickstart

tasqx is a single static binary with no runtime and no dynamic linking. Install
it with a package manager, an installer script, or build it from source.

## With a package manager

Updates then come from `brew upgrade tasqx` / `scoop update tasqx`, and brew
switches Tab completion on by itself. macOS and x86-64 Linux, with Homebrew
(there is no prebuilt ARM Linux binary yet, so ARM Linux builds from source):

```console
brew install dimitritholen/tasqx/tasqx
```

Windows, with Scoop:

```console
scoop bucket add tasqx https://github.com/dimitritholen/scoop-tasqx
scoop install tasqx
```

## With the installer script

macOS and x86-64 Linux:

```console
curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh
```

Windows (the first statement makes older PowerShell able to download at all):

```console
[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1 | iex
```

Prebuilt archives for Linux, macOS and Windows are also on the project's GitHub
Releases page.

## Or build from source

```console
git clone https://github.com/dimitritholen/tasqx.git
cd tasqx
cargo install --path crates/tasqx-cli --force
```

Requires Rust 1.95 or newer. On Windows the MSVC toolchain is discovered
automatically — you do not need it on your PATH.

## Sixty seconds with tasqx

Create a project, capture some work, and look at it.

```console
$ tasqx init work.tasqx --desc "The tasqx project itself"
work.tasqx
created   now your default project
```

Now capture a task. Everything after `add` is the title — except the bits tasqx
recognises as structure. That is the *inline sugar*: `+tag`, `project:`,
`!priority`, `due:`, `est:`, `repeat:`, `remind:`. Whatever is left over is the
title.

```console
$ tasqx add "Ship the v1 JSON API freeze +api +release project:work.tasqx !high due:friday est:4h"
▌ #1  Ship the v1 JSON API freeze
▌ added   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h
```

The sugar was consumed; only the real title remains. Tags and a due date, no
project — `init` made `work.tasqx` the default, so it is filled in for you:

```console
$ tasqx add "Write the user guide +docs due:friday"
▌ #2  Write the user guide
▌ added   - ▄▄▄▁ 9.7   work.tasqx   due Fri   +docs
```

Every field is also available as a flag, which is what you want when a value
contains spaces or comes from a variable:

```console
$ tasqx add "Renew the TLS cert" project:work.tasqx +ops --due -1d
▌ #3  Renew the TLS cert
▌ added   - ▄▄▄▄ 12.0   work.tasqx   due yesterday   +ops
```

> **Careful** Flags go *outside* the quoted title. `tasqx add "Renew the cert
> --due -1d"` puts the literal text `--due -1d` in your title — the shell
> handed tasqx one argument, and tasqx believed it. Quote the title, leave the
> flags bare.

## Look at the working set

`tasqx list` shows the `@working` filter: everything pending or active that is
not blocked, hottest first. A bare `tasqx` prints the same table wherever it is
not talking to a person — piped, redirected, or under `--json` — and opens the
[dashboard](#cli-dashboard) when it is.

```console
$ tasqx list
@working   3 tasks · 1 overdue

  ID          URG  TASK                         PROJECT     DUE        TAGS
   1  H ▄▄▄▄ 15.7  Ship the v1 JSON API freeze  work.tasqx  Fri        +api +release
   3  - ▄▄▄▄ 12.0  Renew the TLS cert           work.tasqx  yesterday  +ops
   2  - ▄▄▄▁  9.7  Write the user guide         work.tasqx  Fri        +docs
```

The line above the table says which filter answered and what the answer holds
beyond its own size — how much of it is late, due before the day is out,
running, or blocked. The `DUE` cells are dated from Tuesday 14 July 2026, the
day this page's narrative runs on: a deadline inside the coming week is named by
its weekday, one further out by its date, and today and tomorrow carry a clock
when the store holds one.

That `URG` column is urgency — a computed score, not something you set. The
letter in front of it is the priority you did set. `tasqx next` is the "what
now" button: the single hottest unblocked task.

```console
$ tasqx next
next    #1  Ship the v1 JSON API freeze
        H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release
        tasqx start 1  ·  tasqx why 1
```

And `tasqx why` shows its arithmetic, so the ranking is never a mystery:

```console
$ tasqx why 1
#1  Ship the v1 JSON API freeze

  priority   H                 6.0
  deadline   due Fri           9.7
  age        created today     0.0
  urgency                     15.7
```

## Work it, finish it

```console
$ tasqx start 1
$ tasqx stop 1
$ tasqx done 1
▌ #1  Ship the v1 JSON API freeze
▶ started   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h
▌ #1  Ship the v1 JSON API freeze
▌ stopped   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h
▌ #1  Ship the v1 JSON API freeze
▌ done today   work.tasqx   due Fri   +api +release   est 4h
```

## Where to go next

| If you want to… | Read |
|---|---|
| Know every verb and flag | [Commands](#commands) |
| Ask precise questions of your store | [Filter grammar](filters.md) |
| Say "friday" or "every 3 days" | [Dates & recurrence](Dates-Reminders-and-Recurrence.md) |
| Be told about a task before it is late | [Reminders](Dates-Reminders-and-Recurrence.md#reminders) |
| Give an AI agent access | [MCP](#mcp) |
| Script tasqx from another language | [JSON API](#api) |
