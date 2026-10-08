# The daemon and live watch

The daemon is optional. It holds one database connection, serves the JSON API
over a local socket to many concurrent clients, pushes change notifications, and
runs the reminder scheduler. The CLI never requires it.

## One-shot or daemon?

| Mode | When | Why |
|---|---|---|
| One-shot | The default. Scripts, cron, the HTML report. | No process to manage. Open the DB, run one command, exit. |
| Daemon | Long-lived clients: a TUI, a GUI, `watch`, reminders. | One writer, warm caches, live push, and something to fire reminders. |

If a daemon is reachable, one-shot commands route through it automatically —
single writer, live-update semantics for free. If not, they open the store
in-process. **Same command surface either way**, and a missing or stale socket
falls back immediately rather than hanging. `--no-daemon` forces the in-process
path.

`$TASQX_DB` always wins over a daemon on another store. Naming the file the
daemon serves, commands route through it as usual. Naming a different file, a
daemon found on the default socket is passed over and the command runs
in-process against `$TASQX_DB`; a daemon named by `--socket` or `$TASQX_SOCK` is
refused with exit 2, naming both files (D204).

Four surfaces never route through a daemon: `api`, `mcp serve`, `chart` and
`report --html` open the store in-process, always. They refuse an explicit
`--socket` rather than ignore it (D73), because a flag that names a daemon and
is silently discarded aims your write at a different store than the one you
asked for.

## Socket addresses

Resolution order: `--socket`, then `$TASQX_SOCK`, then the platform default.

| Platform | Default |
|---|---|
| Windows | The named pipe `tasqx-default` |
| Linux | `$XDG_RUNTIME_DIR/tasqx/tasqx.sock` (falls back to the data dir) |
| macOS | `<data dir>/tasqx.sock` — macOS has no runtime dir |

## Running it

Diagnostics go to stderr; the socket carries the newline-delimited JSON API.
Ctrl-C stops it cleanly, unwinding the accept loop and removing the socket file
(a no-op for Windows named pipes). `--db` points it at a specific store.

```console
$ tasqx daemon --socket tasqx-docsdemo
tasqx daemon: listening on tasqx-docsdemo (Ctrl-C to stop)
tasqx daemon: store ~/.local/share/tasqx/tasks.db
```

It stays until you stop it. Set `[daemon] idle_timeout` to a number of minutes
and it will instead exit by itself once that long has passed with no client
connected, no subscriber attached, no reminder about to ripen and no telemetry
posted to its OTLP receiver — and it says so on stderr on the way out. `0`, the
default, means it never does. Turning the receiver on does not hold it open:
only exports that actually arrive count as work. An idle exit also leaves a note
beside `config.toml`: the first command that then fails to reach the daemon
reports the retirement and which store is no longer being served, because a
daemon leaving changes where the same command line writes (D74).

Now route a command through it — note this is the ordinary `add`, unchanged:

```console
$ tasqx --socket tasqx-docsdemo add "Wire up the docs page +docs project:work.tasqx due:tomorrow"
▌ #6  Wire up the docs page
▌ added   - ▄▄▄▃ 11.4   work.tasqx   due tomorrow   +docs
```

## watch

`watch` subscribes to a daemon and re-renders on every `task.changed` push. It
takes a [filter](filters.md), defaulting to the working set. It needs a running
daemon and will never auto-spawn one — it hints instead:

```console
$ tasqx watch --socket nope
tasqx watch: no daemon reachable at nope
hint: start one with `tasqx daemon` (add `--socket nope` to match)
```

On a terminal it clears the screen and repaints the table. Through a pipe it
streams one line per event instead — so it composes with everything else in your
shell:

```console
$ tasqx watch --socket tasqx-docsdemo | cat
$ # ... meanwhile, in another shell:
$ #   tasqx --socket tasqx-docsdemo add "Wire up the docs page ..."
$ #   tasqx --socket tasqx-docsdemo done 3
4 tasks · 1 overdue

  ID          URG  TASK                         PROJECT     DUE        TAGS
   1  H ▄▄▄▄ 15.7  Ship the v1 JSON API freeze  work.tasqx  Fri        +api +release
   3  - ▄▄▄▄ 12.0  Renew the TLS cert           work.tasqx  yesterday  +ops
   5  - ▄▄▃▁  8.9  Water the plants             home        Fri
   2  - ▄▄▂▁  7.1  Write the user guide         work.tasqx  Mon        +docs
task.changed op=add short_id=6
task.changed op=done short_id=3
```
