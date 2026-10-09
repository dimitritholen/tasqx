# Syncing Between Machines

Your tasks live in a single SQLite file on the machine you're on — see
[Import and Export](Import-and-Export.md). Sync is how a second machine gets
the same store without you carrying a file around: each machine keeps its own
copy, both agree on one shared remote, and `tasqx sync` merges the two through
it. It is local-first — every machine works offline and merges when it next
syncs — never a shared live database, and each store has at most one remote.

## The no-account start: a shared folder

The simplest remote is a folder every machine can already reach: a dedicated
folder inside your Dropbox, iCloud Drive or Syncthing share — not the one your
other files live in, so nothing else in it collides with tasqx's own files.
`tasqx-remote-dir` keeps the snapshot there.

```console
tasqx sync setup dir --set path="$HOME/Dropbox/tasqx-sync"
tasqx sync
```

Nothing to sign up for: the folder's own sync client (Dropbox, iCloud Drive,
Syncthing) is what moves the bytes between machines: `tasqx sync` on one
machine writes into the folder, and the next `tasqx sync` on the other reads
it back once that client has caught up. When two machines write near the same
moment, the folder's client can leave more than one copy of the current
snapshot behind — a "conflicted copy" file, a `HEAD 2`, a
`.sync-conflict-…` name, depending on which client it is. `tasqx-remote-dir`
recognises all three shapes, hands every one of them back on the next pull
beside the real one, and a push that merges them removes exactly the ones it
merged — so a conflict from the folder client heals itself on the next sync
instead of piling up.

## Cloudflare R2, step by step

R2 is the other connector, `tasqx-remote-r2`: one object in an R2 bucket
instead of a folder, for when there's no shared folder to reuse.

1. **Create a bucket.** In the Cloudflare dashboard, under R2, create a
   bucket. tasqx does not create it for you.
2. **Create an API token scoped to that one bucket.** Under R2 > Manage API
   tokens, create a token with **Object Read & Write** permission, scoped to
   the bucket from step 1 and nothing wider. Copy its Access Key ID and
   Secret Access Key — the secret is shown once.
3. **Find your account ID.** It's on the R2 overview page: 32 hexadecimal
   characters.
4. **Run setup:**

   ```console
   tasqx sync setup r2
   ```

   You'll be asked for the account ID, the bucket, the Access Key ID and the
   Secret Access Key (masked, not echoed), and an object key — leave it empty
   and it defaults to `tasqx/snapshot.tqx` inside the bucket.
5. **The passphrase.** Every setup, whichever connector, ends by asking for a
   sync passphrase twice. This is what encrypts the snapshot before either
   connector ever sees it — the folder or the bucket holds ciphertext, never
   your tasks in the clear. **Give every machine that syncs with this remote
   the same passphrase.** Lose it and the remote's snapshots cannot be read by
   anyone, on any machine — each machine's own store is untouched, but there
   is nothing to recover the remote from. It's kept in a file beside your
   store, readable by you alone; it is never sent anywhere.

Repeat `tasqx sync setup r2` with the same account/bucket/token and the same
passphrase on every other machine that should join this remote.

## `config edit`'s Sync section

`tasqx config edit` shows a Sync section below the settings: the connector in
use, the last version synced and how long ago, and whether a passphrase is
set up.

- `c` — connect: lists the `tasqx-remote-*` connectors found on your `PATH`,
  and once you pick one, shows its fields as a form (label, help text, a
  masked field for a secret), then the passphrase twice. This is the same
  write `tasqx sync setup` makes — there's exactly one way "set up" happens
  either screen uses.
- `s` — sync now: runs the same sync `tasqx sync` runs and reports one line.
- `d` — disconnect: after a confirmation, removes this store's sync setup
  (the connector record and the passphrase file). The remote itself is
  untouched — another machine still synced to it keeps working.

## tasqx sync

```console
tasqx sync
tasqx sync --status
```

`tasqx sync` pulls every snapshot the remote holds, merges each one into this
store exactly the way `tasqx import --merge` does (see
[Import and Export](Import-and-Export.md#tasqx-import)), then exports and
pushes the result back. If another machine pushed in between, it merges that
too and tries again, three times at most; failing on the third names it a
conflict and says to run `tasqx sync` again — nothing already merged is lost,
since merging is idempotent. When the remote already holds exactly this store
(nothing changed on either side), nothing is pushed.

`tasqx sync --status` prints the connector, the last version synced
(eight characters) and when (`today`, `2d ago`, `never synced`, or
`not set up` before the first setup).

## What merges, and what wins

The same rules `tasqx import --merge` applies (see
[Import and Export](Import-and-Export.md#tasqx-import) for the full account):

- A field that can only hold one value — title, status, priority, project,
  the dates — is taken from whichever side changed *that* field last.
- Notes, checks, tags and dependency edges are unioned: nothing either side
  wrote is thrown away.
- Tracked time adds up across both stores' histories.
- Removals travel: a tag, check, note, edge, link or memory doc removed on
  one machine stays removed once its later history reaches the other.
- A recurring task completed on two machines, which each spawned its own next
  occurrence, folds the two duplicates into one.

## Secrets

A connector's secret fields (an R2 Secret Access Key, for instance) are never
accepted on the command line: `sync setup --set` refuses a field the
connector marks secret, since argv lands in shell history and `ps`. Give it
at the no-echo prompt, or set `TASQX_SYNC_<KEY>` (the field's key,
upper-cased, with anything that isn't a letter or a digit turned into `_` —
`secret_access_key` becomes `TASQX_SYNC_SECRET_ACCESS_KEY`) when there's no
terminal to prompt at. `tasqx sync setup` reads it once and hands it straight
to the connector; it is never stored by tasqx itself. `tasqx-remote-r2` keeps
its own copy of the secret in the OS keyring, not in a file.

## Headless machines and CI

Off a terminal, every setup field must come from `--set` (a missing one is
refused, naming it) or the environment — a secret field still cannot come
from `--set`. For `tasqx-remote-r2` specifically, `TASQX_R2_SECRET_ACCESS_KEY`
answers the Secret Access Key without a keyring, which a headless box may not
have. The sync passphrase itself, needed by `sync setup` and read again by
every later `sync`, comes from `TASQX_SYNC_PASSPHRASE`.

```console
TASQX_R2_SECRET_ACCESS_KEY=... TASQX_SYNC_PASSPHRASE=... \
  tasqx sync setup r2 --set account_id=... --set bucket=tasqx \
    --set access_key_id=... --set object_key=
TASQX_SYNC_PASSPHRASE=... tasqx sync
```

## Limits

- **One remote per store.** Two stores on one machine (a second `$TASQX_DB`,
  or a scratch store) sync to their own remotes and never see each other's
  setup.
- **No background sync.** `tasqx sync` runs once and returns; nothing polls
  or watches a remote on its own. Schedule it yourself — cron on Linux/macOS,
  Task Scheduler on Windows:

  ```console
  # crontab -e — sync every 15 minutes
  */15 * * * * TASQX_SYNC_PASSPHRASE=... /usr/local/bin/tasqx sync >>~/tasqx-sync.log 2>&1
  ```

  ```console
  schtasks /create /tn "tasqx sync" /sc minute /mo 15 /tr "tasqx sync"
  ```

- **A running daemon is fine.** `tasqx sync` is a client of the same
  dispatch every other command uses, so with `tasqx daemon` running its
  merges go through the daemon like any other write instead of opening the
  store beside it.
