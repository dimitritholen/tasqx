# Reminders

tasqx is quiet by default. A task notifies you only if you gave it a `remind`,
and nothing else ever puts a task on the reminder heap.

## The two forms

| Form | Example | Behaviour |
|---|---|---|
| Offset from `due` | `-1h`, `-30m`, `-2d`, `+15m` | Stays symbolic in the store. Move `due` and the reminder moves with it. |
| Absolute instant | `"friday 9am"`, `2026-07-20T17:00` | Resolved once, at set time, through the same date grammar. |

The **sign** is what disambiguates them: a leading `-` or `+` means offset,
anything else goes to the date parser. Without that rule `3d` would be ambiguous
— "3 days before due" or "in 3 days"?

Offsets take `s`, `m`, `h`, `d`, `w`. Negative is before due, which is what you
almost always want.

```console
$ tasqx modify 1 --remind -1h
$ tasqx show 1
▌ #1  Ship the v1 JSON API freeze
▌ modified   remind -1h   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h   rev 2
▌ #1  Ship the v1 JSON API freeze
▌
▌ status      pending           urgency     H ▄▄▄▄ 15.7
▌ project     work.tasqx        due         Fri (in 3 days)
▌ remind      -1h               estimate    4h
▌ tags        +api +release     created     today 07:51 (just now)
▌ modified    today 07:51 (just now)
▌ rev         2
```

> **Note** Notice `remind` shows as `-1h`, not as a resolved timestamp. That
> is the point: it is still an offset. Push `due` back a week and the reminder
> follows, with no second edit.

## Who delivers them

The [daemon](daemon.md). It keeps an in-memory min-heap of upcoming reminder
instants, rebuilt from the store on start and whenever a task changes. A
one-shot `tasqx` command never fires anything — there would be nobody to fire
it.

```console
$ tasqx add "Deploy the release" --due "2026-07-16T09:00" --remind -1h
$ tasqx daemon --socket tasqx-remdemo
▌ #1  Deploy the release
▌ added   no project · set a default with tasqx use <project>   - ▄▄▄▂ 10.3   due Thu
tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)
tasqx daemon: store ~/.local/share/tasqx/tasks.db
tasqx reminder: [#1] Deploy the release (due 2026-07-16T09:00:00Z)
```

That reminder had already ripened (due 09:00, minus 1h, and it was past 08:00) —
so it fired on the next daemon start. **A reminder that ripened while the daemon
was down still fires, once, on the next start.** Sleeping your laptop does not
lose it.

## It fires exactly once

Firing writes a `reminded` event, and that event row *is* the dedupe record.
Restart the daemon and the same reminder does not come back:

```console
$ tasqx daemon --socket tasqx-remdemo   # second start, same store
tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)
tasqx daemon: store ~/.local/share/tasqx/tasks.db
```

Silence — correct. The key is the (task, *instant*) pair, not just the task:
moving `due` moves a relative reminder to a genuinely new instant, which
*should* fire again.

## Delivery never fails

The always-compiled backend writes one line to stderr and returns. That is the
headless/CI-safe path: with no notification transport anywhere, delivery
degrades to a logged line and exit 0 — never an error.

Native OS toasts (Windows, macOS, Linux/D-Bus) live behind the off-by-default
`notify-os` build feature *and* need an explicit opt-in:

```
# config.toml
[notify]
enabled = true
```

Even then the stderr line still comes first, so the verifiable surface never
depends on which backend is live. Two opt-ins, both off by default — that is the
quiet-by-default rule taken seriously.

## Firing one by hand

`reminder.fire` takes a `ref` and an `at` instant, and it is idempotent — the
second call reports `fired: false` rather than notifying twice:

```console
$ echo '{"tasqx":"1","id":"r1","method":"reminder.fire","params":{"ref":"1","at":"2026-07-16T00:00:00Z"}}' | tasqx api
$ echo '{"tasqx":"1","id":"r2","method":"reminder.fire","params":{"ref":"1","at":"2026-07-16T00:00:00Z"}}' | tasqx api
{"id":"r1","ok":true,"result":{"at":"2026-07-16T00:00:00Z","fired":true,"short_id":1},"tasqx":"1"}
{"id":"r2","ok":true,"result":{"at":"2026-07-16T00:00:00Z","fired":false,"short_id":1},"tasqx":"1"}
```

And the event is in the log, where the dedupe check reads it:

```
{
  "actor": "user",
  "entity": "task",
  "op": "reminded",
  "payload": {
    "at": "2026-07-16T00:00:00Z",
    "due": "2026-07-17T00:00:00Z",
    "remind": "-1h",
    "short_id": 1,
    "title": "Deploy the release"
  },
  "ts": "2026-07-16T08:52:35.4429051Z"
}
```
