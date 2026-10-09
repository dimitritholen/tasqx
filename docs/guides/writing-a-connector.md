# Writing a connector

`tasqx sync` keeps its remote behind an interface, not a library: a connector
is a small standalone executable that speaks one JSON protocol on stdin and
stdout (DESIGN.md D198). Point it at a database, an object store, an SFTP
drop, whatever holds bytes for you — tasqx never links against it, and it
never links against tasqx.

## The protocol

tasqx runs your executable once per verb. It writes one JSON request to its
stdin, reads one JSON reply from its stdout, and judges you by the exit code:
`0` is an answer, `2` means "push conflict" and nothing else, anything else is
a failure whose stderr is shown to the person running `tasqx sync`. Every
request and every reply carries `"protocol": 1` — the version this tasqx
speaks; answering with anything else, or nothing, is treated as a protocol
mismatch.

| verb | request | reply |
|---|---|---|
| `describe` | — | `{name, version, protocol, fields: [{key, label, secret, help, help_url?}]}` |
| `configure` | `{values: {key: string}}` | `{ok: true}` or `{ok: false, error}` |
| `pull` | `{out_dir}` | `{snapshots: [{version, path}]}` |
| `push` | `{in_path, expected_version: string \| null}` | `{version}` or `{conflict: true}` |

These are copied straight from the request and reply types in `remote.rs`
(the crate's `remote` module) — `Request`, `Description`, `Field`,
`ConfigureReply`, `PullReply`, `Snapshot` and `PushReply` — so a mismatch
here is a bug in this page, not a second source of truth.

**`describe`** — say who you are and what you need. A real reply, spelled the
way `tasqx-remote-dir` answers it:

```json
{
  "protocol": 1,
  "name": "dir",
  "version": "0.14.0",
  "fields": [
    {
      "key": "path",
      "label": "Folder",
      "secret": false,
      "help": "An existing folder every machine can reach: a network share, or one Dropbox, Syncthing or iCloud Drive keeps in step."
    }
  ]
}
```

Every field needs a non-empty `key` and `label`; keys must be unique. Mark a
field `secret: true` when it's a password or a token — tasqx then masks it
while typing and refuses it on `sync setup --set`'s command line.

**`configure`** — validate the user's answers (by actually connecting to the
remote, typically) and keep whatever you need to `pull`/`push` later, in your
own state directory (below). A rejection is an *answer*, not a failure: exit
0 with `{"ok": false, "error": "…"}`. tasqx stores none of these values
itself.

```json
{"protocol": 1, "verb": "configure", "values": {"path": "/mnt/share/tasqx"}}
```
```json
{"protocol": 1, "ok": true}
```

**`pull`** — write every snapshot the remote holds into `out_dir` (created
for you) and name each file, absolute or relative to `out_dir`; tasqx refuses
a name that resolves outside it. An empty remote answers with an empty list.
Normally there is exactly one; a connector may hand back more (D200: a folder
connector's conflict copies).

```json
{"protocol": 1, "verb": "pull", "out_dir": "/tmp/tasqx-sync-x1/pull"}
```
```json
{
  "protocol": 1,
  "snapshots": [{"version": "3f9c2e…", "path": "3f9c2e….tqx"}]
}
```

**`push`** — upload `in_path` if, and only if, the remote's current version
equals `expected_version`; `null` means "only if the remote is empty". This
is the whole conflict story: a real conditional write (S3-style
`If-Match`/`If-None-Match`, a database row's own version column, a lock file
plus a compare), not read-then-write with a gap in the middle. Losing the
race answers `{"conflict": true}` and exit `2`, and changes nothing; the
caller pulls, merges, and tries again.

```json
{"protocol": 1, "verb": "push", "in_path": "/tmp/tasqx-sync-x1/push/out.tqx", "expected_version": "3f9c2e…"}
```
```json
{"protocol": 1, "version": "8a71b0…"}
```

**The blob is opaque.** Since D202 it's always sealed ciphertext by the time
your connector sees it — you never need to parse or validate it, only store
and hand back exactly the bytes you were given.

## On PATH, named `tasqx-remote-<name>`

Build a binary named `tasqx-remote-<name>` (`.exe` on Windows) and put it on
`PATH`, the way `git` finds `git-remote-<scheme>`. `tasqx sync setup <name>`
and `tasqx config edit`'s connector picker both find it by searching every
directory on `PATH` for that exact name — on Unix it also has to carry an
execute bit, or it's treated as though it weren't there.

## State directory

Every call carries `TASQX_REMOTE_STATE_DIR` in the environment: a directory
of your own, created for you before the first call, where you keep whatever
`configure` decided to save. It's scoped per store and per remote name
(`remotes/<name>/` beside the store file), so two stores never share one, and
it's created private to its owner (`0700` on Unix) — keep secrets in the OS
keyring rather than a file there when you can; the state directory is meant
as a fallback, not a vault. `tasqx-remote-r2` uses the keyring for its Secret
Access Key and only a non-secret config in the state directory;
`tasqx-remote-dir` has nothing secret to keep at all.

## Environment rules

You inherit tasqx's environment — `HOME`, `PATH`, the keyring's session bus
(`XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`), proxies, CA bundles,
`SYSTEMROOT` on Windows, and any override you define yourself
(`tasqx-remote-r2`'s own `TASQX_R2_SECRET_ACCESS_KEY`) — with one exception:
every variable whose name starts with `TASQX_SYNC_` is stripped before you're
run. Those are `tasqx sync`'s own secrets — the encryption passphrase and the
`TASQX_SYNC_<KEY>` answers to your `secret` fields — and you're never meant
to see them; a secret field's value reaches you exactly once, in the JSON
`configure` request.

## Timeouts

The runner kills a call that runs too long: 120 seconds by default, except
`pull` and `push`, which `tasqx sync` gives 10 minutes — a multi-megabyte
snapshot on a slow link needs the room. Keep your own per-request timeout,
if you have one, comfortably under that: `tasqx-remote-r2`'s HTTP timeout is
540 seconds, held there by a unit test that compares it against the runner's
constant.

## Testing against the conformance suite

The suite every connector is held to lives behind a feature flag in the
core crate, not copied into each connector: add it as a dev-dependency with
`features = ["conformance"]`, same as both shipped connectors do, and call
`run` on your own built binary.

```toml
[dev-dependencies]
tasqx-core = { version = "0.14", features = ["conformance"] }
```

Then, in a test of your own, import `remote::conformance::{self, Setup}` off
that dev-dependency, build a `Setup { program, work_dir, good, bad }` — the
executable under test, a scratch directory the suite may fill, the values
that configure an empty remote, and at least one value set `configure` must
refuse — and call `conformance::run(&setup)`.
`crates/tasqx-remote-dir/tests/conformance.rs` is a real, working copy of
exactly that, `Setup` and all, ready to copy and point at your own binary.
Run it the same way any other test runs: `cargo test -p tasqx-remote-dir`
(or your own crate's name). The suite walks `describe`, `configure` with both
good and bad values, an empty pull, a push-then-pull round trip, and a
conflicting push, stopping at the first check that fails and naming it —
everything goes through the same `Connector` runner tasqx itself uses, so a
connector that passes is one tasqx can actually drive.

## Two reference connectors

Read `crates/tasqx-remote-dir/src/main.rs` first — the simplest connector
that does something real, one setting (a folder path), content-addressed
blobs, and D200's handling of the conflict copies a Dropbox/Syncthing/iCloud
client leaves behind. `crates/tasqx-remote-r2/src/main.rs` is the other
shape: a hand-rolled SigV4 signer over a cloud object store, a secret kept in
the OS keyring instead of the state directory, and conditional PUT for
compare-and-swap. Between the two they cover every decision this page
describes in working code.
