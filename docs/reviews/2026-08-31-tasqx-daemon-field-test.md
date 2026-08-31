# Field test: the daemon path, driven adversarially for a day

**What ran.** The mode `DESIGN.md` §2 specifies and nobody had driven: `tasqx daemon` on a
non-default named pipe, with real clients on it. Against tasqx 0.5.0 (`d982b64cadcf`), on Windows
11. Ten daemons over ten scratch stores; **130 clean shutdowns with a write in flight**; 953 tasks
written by nine concurrent writers; 1,400-event floods against a deliberately stalled subscriber;
144 hostile connections; seven reminders across five restarts and a hard kill; a two-second window
on the *default* address to reproduce the 2026-07-25 wrong-store incident. Clients were the real
`tasqx` binary, a real `tasqx watch`, a real `tasqx mcp serve` over stdio, and — where a process
spawn (~30 ms) would have been wider than the window under test — a direct newline-delimited-JSON
client on `//./pipe/<name>`, which is the same wire the CLI uses.

Every write went to a scratch store under the session scratchpad, on an address no stray client
could reach, with `tasqx config store` run on all three surfaces before the first write. The live
store was read three times and never written, except the annotations on `#234`-`#244` and the
finding tasks this report ends with: **244 tasks / 16 projects / 60 docs before, and the same
after**.

**How long.** About two hours of wall clock. Most of it was waiting — a one-minute idle timeout
cannot be hurried, and 130 shutdown rounds are 130 daemon starts.

**Bias to correct for.** A daemon killed a hundred and thirty times in two hours is not a daemon
that has served one person for a month, and this report says nothing about the second. What a long
quiet life does to a daemon — WAL growth, a laptop sleeping and waking, a subscriber attached for
days, memory over weeks — was not touched. Neither was Linux or macOS: the transport is a Windows
named pipe throughout, and two of the observations below (the binary that cannot be replaced under
a running process; `Access is denied` as the second-bind refusal) are Windows-specific in their
*symptoms* even where the behaviour underneath is not.

Findings are split into **defects** (something is wrong against a claim the project itself makes),
**detours** (it works, and the surface that describes it says otherwise) and **absences** (nothing
is broken; a thing that would make the rest coherent is not there). Conflating them is how a report
gets argued with instead of acted on.

**Checked before writing, not after.** Every candidate was held against `UNEXPOSED_METHODS`, the
D-entries it touches (D5, D7, D28, D30, D43, D44, D46, D47, D63, D66) and §11's not-built list.
Several candidates died there and are recorded under *Investigated and not a defect*, along with
one false alarm this run raised against itself and then disproved. The previous field report's
header records that its predecessor filed five findings of which two were wrong — a capability it
called missing was reachable another way, and an omission it called an oversight was a documented
decision. Both shapes were specifically hunted for here.

**Filed as** `#245`-`#255` on the live store, project `tasqx`, tags `field-test` and `daemon`, one
task per finding with the reproduction in an annotation — the route `#224`-`#233` took. The driving
record is on `#234`-`#243` in project `ft-tasqx-daemon`: each carries the approach, the decisions,
and the result with its evidence.

---

# Defects

## #1: the clean shutdown commits writes it never answers — #58, reproduced by hand

**Severity:** HIGH · **Category:** defect / correctness · **Effort:** 4-8h

### Problem

D5 states the guarantee in one sentence:

> The exit goes through the same shutdown flag Ctrl-C sets, so a request that arrives in the race
> window is refused with `unavailable` rather than committed unanswered.

It is not always true. Driven 130 times — start a daemon, have one client `task.add` in a tight
loop over a held-open connection, deliver `CTRL_BREAK_EVENT` to the daemon's own process group at a
random point between 50 and 500 ms in, then read the store back with `tasqx --no-daemon export` and
compare the highest title the client saw **answered** against the highest **committed**:

```
run 1, seed 11, 30 rounds: {'unavailable': 27, 'eof': 3, 'applied_unanswered': 2}
  round  7  end=eof  last_ok=102  highest_committed=103  gap=1  dense=True
  round 17  end=eof  last_ok=200  highest_committed=201  gap=1  dense=True
run 2, seed 11, 40 rounds: {'unavailable': 38, 'eof': 2, 'applied_unanswered': 0}
run 3, seed 23, 60 rounds: {'unavailable': 60, 'eof': 0, 'applied_unanswered': 0}

130 rounds: 125 refused correctly; 5 (3.8%) ended in EOF instead of the refusal
envelope; 2 (1.5%) had a write COMMITTED that the client was never told about.
```

The correct answer, seen 125 times, is exact and actionable:

```
unavailable: daemon is shutting down; the request was not applied
```

On the failing rounds the client has an EOF, the store has the task, and nothing anywhere connects
the two. The daemon's stderr on those rounds carries only its two ordinary lines:

```
tasqx daemon: listening on tasqx-ft-d4 (Ctrl-C to stop)
tasqx daemon: stopped
```

This is task **#58** — `a_request_arriving_after_shutdown_is_refused_instead_of_committed_unanswered`,
which fails about 10% of full lib runs with `UnexpectedEof` — with a reproduction that does not go
through cargo. Real binary, real named pipe, real Ctrl-C, store read back afterwards. The rate is
lower here (3.8% EOF, 1.5% committed-unanswered) because the harness is different; the shape is
identical. A flake with a manual reproduction stops being a flake.

Why it leads: it is the only finding that contradicts an explicit D-entry sentence with store-level
evidence, and the failure is silent on both sides. A client that retries a write it believes was
refused writes it twice.

### Acceptance criteria

- [ ] A request that arrives during shutdown is either refused with `unavailable` **or** answered
      after being applied — never committed with the connection closed instead of answered.
- [ ] `#58` passes 200 consecutive runs, and the fix is watched failing against the original code
      using a reproduction of the shape above rather than only the in-crate test.
- [ ] If the window cannot be closed entirely, the daemon says so on stderr when it drops a
      committed-but-unanswered response, so an operator has something to correlate against.

---

## #2: nothing on the write path names the store, and the idle timeout adds a second silent trigger

**Severity:** HIGH · **Category:** defect / safety · **Effort:** 2-4h

### Problem

D47 is a decision, not an oversight: `tasqx config store` was built for exactly this question, it
answers correctly, and it deliberately declines to print the inert local path on the daemon branch.
All of that was verified working. The finding is that **the answer requires asking**, and that a
second trigger now exists which D47 did not consider.

**The original trap, reproduced move for move.** Daemon on the default address owning `trap.db`;
`TASQX_DB` set to `mine.db`, which is what the operator believes they are writing to:

```
$ TASQX_DB=mine.db tasqx config store
  daemon at tasqx-default
    the daemon owns the store; $TASQX_DB is NOT in effect here.
    Pass --no-daemon to work on your own store instead.

$ TASQX_DB=mine.db tasqx add 'does this land where I think'
  Added #1  ·  pending  ·  urgency 0.0

$ TASQX_DB=mine.db tasqx list
     1  0.0  -  does this land where I think
  1 task(s)

trap.db (the daemon's store) : ['does this land where I think', 'env-sock write']
mine.db ($TASQX_DB)          : ['no-daemon write', 'agent write via mcp serve']
```

The write landed in the daemon's store, exit 0, and the read *confirmed the operator in the wrong
belief*. The whole of what the write path says is `Added #1 · pending · urgency 0.0`.

**The agent shape is sharper.** `tasqx mcp serve` never consults the socket (finding #8), so an
agent and the human's CLI beside it, in one environment, are on two different files:

```
mcp  tasqx_add_task   -> { "short_id": 2, ... }
mcp  tasqx_list_tasks -> { "count": 2, ... }      # ['no-daemon write', 'agent write via mcp serve']

$ tasqx list      # same shell, same TASQX_DB, right next to that agent
     1  0.0  -  does this land where I think
     2  0.0  -  env-sock write
  2 task(s)
```

Both answer "2 tasks". They are two different pairs in two different files, and the matching counts
make it look like agreement. An agent asked "did you file that?" answers yes, truthfully, about a
store the human cannot see.

**The new trigger: a sixty-second fuse.** With `[daemon] idle_timeout = 1` and a daemon owning
`d242-daemon.db` while `TASQX_DB` names `d242-mine.db`, the daemon retires on its own and the
identical command line then means something else:

```
with no client: daemon exited after 61s (rc=0)
tasqx daemon: no clients and no work for 60s; shutting down (`[daemon] idle_timeout`)

$ tasqx --socket tasqx-ft-d8 add "written AFTER the daemon retired"
  Added #1  ·  pending  ·  urgency 0.0      rc=0

daemon store d242-daemon.db: ['seed through the daemon']
$TASQX_DB    d242-mine.db  : ['written AFTER the daemon retired']
```

Two stores, each holding a task `#1`, from one command run twice sixty seconds apart. `TASQX_DB`
was inert while the daemon lived and became authoritative the moment it retired. Nobody touched a
flag or an environment variable; a timer fired.

### Acceptance criteria

- [ ] A write says which store it landed in, or the routing is unambiguous from the command itself.
      `task.add` already returns the `project` it landed in (D21); the store is the same class of
      invisible field, and D47 has already recorded it as the seventh instance.
- [ ] `$TASQX_DB` is never silently inert: when it is set and being ignored, some surface on the
      path that ignores it says the word.
- [ ] The idle-timeout fallback either addresses the same store it did before, or says on the first
      command after the transition that the target changed.
- [ ] Whatever shape the fix takes, it is quiet in the common case — a note on every `add` would be
      noise, and D57's marker-then-print discipline is the precedent for saying a thing once.

---

## #3: `tasqx api --socket <addr>` accepts the flag and silently ignores it

**Severity:** MEDIUM · **Category:** defect / routing · **Effort:** 1-2h

### Problem

`--socket` is a global flag and clap accepts it on every subcommand. `execute` returns for `api`,
`mcp` and `daemon` before `open_backend` is ever reached:

```
$ grep -n "open_backend\|open_engine" crates/tasqx-cli/src/lib.rs
497:        let engine = match open_engine() {      # chart / report --html
522:    let mut backend = match open_backend(cli.socket.as_deref(), cli.no_daemon) {
3481:    let engine = match open_engine_at(db) {     # the daemon itself
3643:    let engine = match open_engine() {          # run_api
3688:    let engine = match open_engine() {          # run_mcp_serve
```

Driven, with a daemon on `tasqx-ft-d0` owning `tasks.db` and `TASQX_DB` pointing at an empty
`probe235.db` — all three writers carry the same flag:

```
== before ==  daemon_db: 3  probe_db: 0

$ tasqx --socket tasqx-ft-d0 add 'A: cli verb with --socket'
Added #4  ·  pending  ·  urgency 0.0  ·  ftproj

$ echo '{"tasqx":"1","id":"b","method":"task.add",...}' | tasqx --socket tasqx-ft-d0 api
{"id":"b","ok":true,"result":{...,"short_id":1,...},"tasqx":"1"}

== after ==   daemon_db: 4  probe_db: 2
 daemon_db titles: [..., 'A: cli verb with --socket']
 probe_db  titles: ['B: api with --socket', 'C: mcp with --socket']
```

The CLI verb routed to the daemon. `api`, given the same flag, opened `$TASQX_DB` and wrote there —
returning `short_id: 1` because it had just **created a second store**. Two live tasks numbered 1,
in two files, from two commands that differ only in a flag one of them discards.

§11's build status says "one-shot commands auto-route through it when a socket is present", and
`tasqx api` is the project's own "stdio one-shot transport". Whichever way that is resolved, a flag
that is accepted and does nothing is the worst of the three options, and it does nothing on the one
verb where the wrong store is the entire hazard of finding #2.

### Acceptance criteria

- [ ] `tasqx api --socket <addr>` either routes through the daemon, or refuses the flag naming why.
      Accepting it silently is not one of the options.
- [ ] The same ruling is applied to `mcp serve` (finding #8) and recorded once, since they are the
      same seam.
- [ ] A guard fails the build if a global flag is accepted by a subcommand that cannot honour it.

---

## #4: the push-gap marker waits for an event that may never come

**Severity:** MEDIUM · **Category:** defect / push · **Effort:** 2-4h

### Problem

`Hub::broadcast` announces a subscriber's lost events with a `task.changed.gap` frame carrying the
count, and its comment says why: for a non-redrawing consumer "a silent drop is not recoverable".
But the marker can only be sent while sending something else, so if the burst is the last thing
that happens, it is never sent at all.

Two subscribers, one deliberately not draining; 1,400 writes; it resumes reading the instant the
flood ends (inside `CLIENT_SEND_TIMEOUT`, which is 5 s — a longer stall gets the connection torn
down instead):

```
1400 writes in 3.07s, subscriber resumed the instant the flood ended
  fast subscriber: 1400 frames, eof=None
  slow subscriber AFTER draining, BEFORE any further write: 1028 frames, 0 gap frames
  -> it is short by 372 events and has been told nothing
```

One further write, and the mechanism works perfectly:

```
after ONE further write: 2 new frames
{"data":{"dropped":372,"op":"gap"},"event":"task.changed.gap","tasqx":"1"}
{"data":{...,"op":"add","short_id":4377},"event":"task.changed","tasqx":"1"}

daemon stderr: subscriber 9 is not draining its queue (cap 1024); dropping events
               subscriber 9 resumed after dropping 372 event(s)
```

The count is exact and the daemon knows it on its own stderr. The subscriber does not, for as long
as the store stays quiet. The scenario the code comment names — "a bulk import through the
external-writer path plus a slow consumer" — is precisely the write that tends to be the last one
for a while.

### Acceptance criteria

- [ ] A subscriber that has fallen behind learns so without depending on a later event: the marker
      is delivered when queue pressure clears, not only when the next broadcast happens.
- [ ] The existing behaviour is kept where it already works — the marker still precedes the first
      frame the subscriber can take again, so the gap is reported at the position it happened.
- [ ] A test drives the quiet case: flood, drain, and assert the marker arrives with no further
      write.

---

## #5: `tasqx watch` receives the number of dropped events and prints it away

**Severity:** MEDIUM · **Category:** defect / CLI · **Effort:** 1h

### Problem

The gap frame carries `"dropped": 372`. The non-TTY branch of `run_watch` reads `data.op` and
`data.short_id` and nothing else (`crates/tasqx-cli/src/lib.rs:3595-3599`), so the line a script
sees is:

```
task.changed op=gap
```

Driven with a real `tasqx watch` whose stdout pipe was deliberately not drained, so it blocked in
`println` and congested its own queue:

```
1400 writes in 3.07s while `tasqx watch` was blocked on its stdout
lines watch emitted before any further write: 5408      any gap line yet: []
lines after ONE further write: 5410
  gap lines: ['task.changed op=gap']
  last 4: [... 'op=add short_id=5405', 'task.changed op=gap', 'op=add short_id=5778']
  watch stderr: (none)
daemon stderr: subscriber 10 resumed after dropping 372 event(s)
```

`daemon.rs`'s own comment says a silent drop leaves "nothing in the stream to attribute the
difference to". The daemon put the attribution *in* the stream. The CLI is where it stops. A script
tallying that stream learns it lost some events and cannot learn how many — while the number
existed, was computed, was sent, and was logged on the other side of the socket.

Separate from #4 on purpose: different fix, different file, and either one can land without the
other.

### Acceptance criteria

- [ ] The non-TTY branch renders the count, e.g. `task.changed op=gap dropped=372`.
- [ ] The TTY branch is considered too: it re-renders the whole working set, so the count is not
      load-bearing there, but a silently-absorbed 372 is worth a line either way.
- [ ] A test asserts the rendered line against a gap frame, so the field cannot be dropped again
      without something going red.

---

## #6: `tasqx daemon` announces "listening" before it has bound

**Severity:** LOW · **Category:** defect / diagnostics · **Effort:** 30m

### Problem

`crates/tasqx-cli/src/lib.rs:3524` prints the listening line ahead of `serve`. A second daemon on
an address already held therefore claims success and then contradicts itself:

```
second daemon exited rc=1
its output: 'tasqx daemon: listening on tasqx-ft-d3 (Ctrl-C to stop)
             tasqx daemon: bind/serve failed on tasqx-ft-d3: Access is denied. (os error 5)'
```

The refusal itself is correct and important — the store is never split between two daemons, which
was the thing worth checking. But in a log, a service unit or a scrollback, `listening on <addr>`
is the line an operator reads, and it is false. It cost this run real time: a leaked daemon plus
this message sent me chasing a daemon death that had not happened (see *Investigated and not a
defect*).

### Acceptance criteria

- [ ] The line is printed after the bind succeeds, or it says "binding" and a second line confirms.
- [ ] The failure line keeps naming the address and the OS error, which is the half that already
      works.

---

# Detours

## #7: the MCP schema calls `expected_rev` optional while the server injects it on every modify

**Severity:** MEDIUM · **Category:** detour / agent surface · **Effort:** 1h

### Problem

§7 documents the injection, and the server does it (`crates/tasqx-core/src/mcp.rs:1064-1072`): when
a caller omits `expected_rev`, the server reads `_rev` and pins it. The defect is only that the
surface an agent actually reads says the opposite:

```
"expected_rev": { "description": "Optional optimistic-concurrency guard.", "type": "integer" }
description: "Change fields on a task via a `set` map. Pass expected_rev for optimistic
              concurrency (a stale rev yields a conflict instead of clobbering)."
```

Read literally: pass it and you get the guard, omit it and you do not. The truth is the reverse.
Under contention that is not a footnote. 200 barrier-synchronised rounds, agent on a live
`tasqx mcp serve`, human on a daemon connection, both modifying one task:

```
A. agent = mcp serve (in-process on the file); human = daemon client
   outcomes (agent, human): {('conflict','ok'): 199, ('ok','ok'): 1}
   round 1: agent=error [conflict]: expected_rev 5 but task is at rev 6 | human=ok
```

An agent that omitted the parameter believing the guard was off gets 199 conflicts in 200 attempts
it never opted into, and there is no documented retry protocol on the surface to meet them with
(see #9).

Checked before filing: not in the tool description, not in `annotations`, and `initialize` returns
`"instructions": null`.

### Acceptance criteria

- [ ] The `expected_rev` description states that the server supplies it when the caller does not,
      and what a caller who wants last-writer-wins can do about it — or that there is no way, said
      plainly.
- [ ] The tool description names the retry the conflict expects.
- [ ] Whatever the wording, a guard ties it to the injection site so the two cannot drift, in the
      shape D30 asks for.

---

## #8: §2 and §7 route the MCP server through a socket the code never opens

**Severity:** LOW · **Category:** detour / spec · **Effort:** 1-2h (a ruling, then a guard)

### Problem

§2's diagram has `MCP --> SOCK`, beside the TUI and the GUI. §7 opens: "The MCP server is **a
long-lived socket client of the core** (§4)."

```
$ grep -n "fn run_mcp_serve" -A 3 crates/tasqx-cli/src/lib.rs
3687:fn run_mcp_serve(scope: Scope) {
3688-    let engine = match open_engine() {

$ grep -n "fn open_engine" -A 3 crates/tasqx-cli/src/lib.rs
3781:fn open_engine() -> Result<Engine, String> {
3782-    let path = db_path()?;
3783-    Engine::open(&path.to_string_lossy()).map_err(|e| e.message)
```

`open_engine`, never `open_backend`. `crates/tasqx-core/src/mcp.rs` contains the string "socket"
exactly once, in prose about `reminder.fire`. `McpServer::new` takes `&Engine`; there is no remote
variant to reach even if the wiring existed. Driven as well as read: with a daemon listening and
`--socket` passed explicitly, `mcp serve` wrote to `$TASQX_DB` (finding #3's evidence block).

The plan that commissioned this test allowed for the routing being a deliberate simplification
nobody wrote down. It is not recorded as one. §11's not-built list names "the socket-client daemon
auto-spawn half of D5" and says nothing about the MCP server being exempt from the socket entirely,
and D7 — the entry that is *about* how `mcp serve` ships — says "A future socket/network transport
must define separate peer authentication", i.e. assumes the socket transport is not there yet. So
the spec is ahead of the code.

This is not merely a diagram. Every concurrency result in this report was measured on a
configuration §2 says does not exist: an MCP agent and a daemon-backed CLI are two processes with
two connections on one SQLite file, never one writer. That configuration turns out to be safe (see
*Investigated and not a defect*), which is the argument for correcting the spec rather than the
code.

### Acceptance criteria

- [ ] One ruling, in a D-entry: either `mcp serve` grows the `open_backend` routing §2 describes,
      or §2's diagram and §7's opening sentence are corrected to say it is a stdio host over an
      in-process engine.
- [ ] Either way, a guard. A prose claim about wiring is worth nothing until something fails when
      it stops being true — the repo's own rule.

---

## #9: §7 quotes a conflict message the tool has never emitted

**Severity:** LOW · **Category:** detour / spec · **Effort:** 30m

### Problem

§7: "the core returns `conflict`; the tool surfaces *'task changed under me, re-read and retry'*
instead of silently overwriting."

```
{"result": {"isError": true,
  "content": [{"type":"text","text":"error [conflict]: expected_rev 1 but task is at rev 1401"}]}}
```

The revs are there, which is the load-bearing half and better than the quoted sentence. The
"re-read and retry" protocol is not, anywhere: not in the message, not in the tool description, not
in `annotations`, and `initialize` returns `"instructions": null`. Per finding #7, an agent hits
this path 199 times in 200 under real contention, so the missing half is the half it needs most.

### Acceptance criteria

- [ ] §7 quotes what the tool emits, or the tool emits what §7 quotes.
- [ ] Whichever, the retry instruction reaches the agent — plausibly by folding into #7's tool
      description rather than into the error string, which the conformance suite freezes.

---

# Absences

## #10: nothing can name the store a running daemon owns

**Severity:** MEDIUM · **Category:** absence · **Effort:** 2h

### Problem

D47 ruled, correctly, that a *client* may not print the local path on the daemon branch: it cannot
know the daemon's file, and printing the inert one restates the falsehood the surface exists to
kill. But the daemon knows, and nothing asks it. Three routes checked before calling this missing:

```
$ tasqx daemon --socket tasqx-default --db trap.db
tasqx daemon: listening on tasqx-default (Ctrl-C to stop)        # the whole of its stderr

$ echo '{"tasqx":"1","id":"1","method":"core.capabilities","params":{}}' | tasqx api
result keys: api, default_project, features, methods, params, ...  # no store path

$ tasqx --socket <addr> config store
daemon at tasqx-default
  the daemon owns the store; $TASQX_DB is NOT in effect here.     # deliberately declines, D47
```

So there is no way for anyone — operator, client, or agent — to learn which file a running daemon
is writing to. That is the missing half of finding #2: the write path cannot name the store partly
because, on the daemon branch, nothing can.

`core.capabilities` is on `UNEXPOSED_METHODS` for MCP with a reason that does not bear on this (the
handshake already answers what MCP asks it). Adding the daemon's store to it, or to the
`config store` daemon branch, is additive and does not change the frozen result shapes the
conformance suite guards.

### Acceptance criteria

- [ ] A running daemon can be asked which store it owns, over the socket, by an ordinary client.
- [ ] `tasqx config store` on the daemon branch reports the daemon's file — which is not what D47
      forbade; D47 forbade printing the client's own inert path.
- [ ] The daemon names its store on startup, beside the address.

---

## #11: nothing tells you a daemon should be running and is not

**Severity:** LOW · **Category:** absence · **Effort:** part of D5's missing half

### Problem

After the idle timeout retires a daemon, every verb but one succeeds silently in-process:

```
$ tasqx config store   rc=0   <path> / in-process; this file is the store.
$ tasqx list           rc=0   (a normal table; nothing about routing)
$ tasqx watch          rc=1   tasqx watch: no daemon reachable at tasqx-default
                              hint: start one with `tasqx daemon` (add `--socket tasqx-default` to match)
```

Only `watch` — the verb that structurally cannot work without a daemon — says anything, and it says
it well.

**This is not a re-filing of "D5 is half-built".** §11 already records that the auto-spawn half has
not landed, and saying it again has not moved anyone. What this run adds is that the two halves are
not independent: the shipped half (idle shutdown) **changes the target store of an unchanged
command on a sixty-second fuse** (finding #2), and the unshipped half is exactly what would keep
the address live so that transition never happens. The idle timeout without the auto-spawn is not
half a feature; it is a silent store switch on a timer.

### Acceptance criteria

- [ ] Either the auto-spawn half lands, at which point #2's second trigger disappears on its own,
      or the idle timeout's fallback announces itself (which is #2's third criterion).
- [ ] If neither, D5 records that the shipped half has this consequence, so the next person to read
      §11 knows the two halves are coupled.

---

# Measurements

All on Windows 11, tasqx 0.5.0 (`d982b64cadcf`), release build, scratch stores on a local SSD.

## Latency: socket versus in-process

```
warm socket round trip, task.get x500     median 0.414 ms   p95 0.523 ms   max 19.182 ms
tasqx --socket <addr> list                median 40.4 ms    min 39.6       max 53.4
tasqx --no-daemon list                    median 28.7 ms    min 26.7       max 31.0
```

Two numbers answering two questions. On a warm connection the socket costs 0.41 ms a call —
nothing. But a one-shot `tasqx list` routed through the daemon is **~12 ms slower than the same
command in-process** (15 runs each, same 953-task store): connect, handshake and frame cost more
than opening SQLite does. §2's table sells the daemon on "holds the DB connection + warm caches";
for the plain one-shot CLI the daemon is a net loss on latency, and its value is entirely in the
push stream and the warm connection a long-lived client keeps.

## The serialization constant: there isn't one

Same total work (120 adds), split over K daemon clients released off one barrier:

```
K=1  120 adds  wall 0.281s  426.7 adds/s
K=2  120 adds  wall 0.253s  474.6 adds/s
K=4  120 adds  wall 0.262s  458.5 adds/s
K=8  120 adds  wall 0.263s  455.5 adds/s
```

Flat. A write serializes at about **2.2 ms** and one client already saturates that, so K clients
are neither K times slower nor any faster. The pass condition anticipated "if serialization makes K
clients K times slower, that is the design working" — it does not; fan-out is free and buys
nothing.

## Push latency

```
daemon-applied write, n=10                       median 2.23 ms   min 2.05   max 12.28
external write, n=30, de-phased from the tick    min 9.3   p25 158.8   median 262.5
                                                 p75 346.5   p95 381.9   max 399.8 ms
```

Exactly a uniform draw against the ~400 ms poller §2 describes ("event-log rowid watermark +
poll"). A first sequential measurement read `median 399.4 ms`, which was the loop phase-locking
onto the tick; the de-phased distribution above is the honest one. A daemon-applied write is pushed
in ~2 ms, an external one costs up to 400 ms more — a 200× difference worth knowing before building
on `watch` as if it were uniform.

## Write throughput and integrity under concurrency

```
K=8 daemon clients x M=25 adds     200 tasks, 200 events, short_id dense 1..200, 0.456s (438 adds/s)
mixed load (add/annotate/tag/dep/  48 tasks, 104 events for 104 mutations, dense
  done/reopen), K=8
8 daemon clients + 1 NON-daemon    225 tasks, 225 events, short_id dense 1..953, 0.607s
  writer (live `tasqx mcp serve`)
```

529 mutations from up to nine concurrent writers across a process boundary. Zero errors of any
kind — no `conflict`, no `busy`, no `UnexpectedEof`. `short_id` stayed dense over the whole store
every time.

## Contention on one task

200 barrier-synchronised rounds per configuration:

```
A. agent = mcp serve (in-process), human = daemon client   {('conflict','ok'): 199, ('ok','ok'): 1}
B. both on the daemon, agent read-then-modify              {('ok','ok'): 199, ('conflict','ok'): 1}
C. both in-process, two mcp serve processes                {('conflict','ok'): 140, ('ok','conflict'): 60}
```

Event deltas reconcile exactly in all three (401, 599, 400 for 200 rounds plus 200 resets). In C,
where both writers carry `expected_rev`, exactly one wins every round — never both, never neither.
The A-versus-B inversion is the number worth keeping: on the daemon the agent's read-then-write
pair completes intact because one accept loop serializes it; across processes the human's single
round trip beats the agent's two, every time.

## Lifecycle

```
clean Ctrl-C (CTRL_BREAK to the daemon's group)  rc=0 in 0.02s, "tasqx daemon: stopped"
idle shutdown, [daemon] idle_timeout = 1         exits at 61s, announced at startup and at exit
idle shutdown with a subscriber attached         still running at 150s  (correct, D5)
hostile input, 144 concurrent connections        daemon alive, bystander 226 reads / 0 failures
reminders, 7 tasks, 5 restarts, 1 hard kill      exactly 1 `reminded` event each, no double fire
```

---

# Investigated and not a defect

**The daemon dies under concurrent connection churn.** It does not, and the first run of the
hostility harness said it did: `daemon alive: False` with
`bind/serve failed on tasqx-ft-d7: Access is denied. (os error 5)` — while a brand-new connection
still answered, which is the contradiction that made it worth chasing rather than filing. Cause: an
earlier run of the same script had crashed on a Python encoding error before its cleanup and leaked
a daemon on that address. The "dead" process was the *second* one, correctly refusing to bind an
address already held; the first served the entire attack unharmed. Isolating each payload class
against a fresh daemon — plain churn, non-JSON, truncated-then-hangup, NUL and 0xFF bytes, a
100,000-deep filter, a 1.5 MiB frame, 144 connections each, plus three repeats of plain churn —
killed nothing. Every run: `daemon alive=True, new conn ok=True`. The same leak later invalidated a
whole reminder sheet, which is why the harness ended up refusing to return a daemon it did not
itself start. Two findings' worth of time, and neither was a finding.

**Two processes on one SQLite file lose writes or duplicate `short_id`.** They do not. §2 makes the
daemon "the *only* writer when running", and per finding #8 an MCP agent is never inside that
guarantee — so this was the first thing to check. Eight daemon clients plus a live `tasqx mcp serve`
on the same file, released together, produced 225 tasks for 225 attempts, 225 events, and
`short_id` dense from 1 to 953 across the whole store with no duplicate. SQLite's own file locking
holds the dense sequence across the process boundary; the single-writer property is not
load-bearing for it.

**Optimistic concurrency loses a write under a real race.** It does not. Across 600 raced rounds in
three configurations, every refusal arrived as `conflict` naming both revs, never as a transport
error, and the event log reconciles to the round in every configuration. Where both writers
succeeded, both events are in the log and the final state is explainable from it — which is the
second branch the pass condition allows, not a lost update.

**Hostile input reaches a `watch` terminal.** It does not. A title carrying
`\x1b]0;OWNED\x07\x1b[2J\x1b[31mred\x07\r` renders through the CLI with no escape byte surviving:

```
`tasqx --socket list` stdout contains ESC(0x1b): False ; BEL(0x07): False ; CR: False
rendered row: '   3  0.0  -  pwn]0;OWNED[2J[31mred'
watch output bytes: 178 ; contains ESC: False ; contains BEL: False ; contains 'OWNED': False
```

**D43's guards do not hold against the real socket.** They do. The 50 KB of `(` that D43 says could
abort the daemon for every other client is refused with `filter nests more than 64 '(' groups
deep`; `every 99999999 days` and `1000000000000000000w` are refused at their parse boundaries
rather than stored.

**Invalid UTF-8 in a title should be refused.** It is defused instead: `\xff\xfe\x80bad` returns
`ok:true` and stores `U+FFFD U+FFFD U+FFFD b a d`. That is D28's inversion working as designed —
refuse *or defuse* at the boundary — and it is recorded here only because the replacement is
silent: the caller is told `ok`, not that its title was altered. Not filed.

**`limit: 2^63-1` is a memory hazard.** Not on this evidence. `limit: -1` is refused with an exact
message; the response is bounded by the store; and the JSON API answering whole is the documented
v1 behaviour that D63/D66 addressed at the *MCP transport* rather than at the core, which is where
the client's payload limit actually lives.

**The reminder scheduler double-fires across a restart.** It does not. Seven reminders, five daemon
restarts including one `taskkill /F` 1.5 seconds before ripeness, and one reminder that came due
while no daemon existed at all: exactly one `reminded` event each, counted in the event log, with
no double fire after any restart. The symbolic offset re-anchored within two seconds of an external
`tasqx --no-daemon modify` moving `due` — the rebuild-on-change heap seeing a write from a process
it has no connection to.

**A restarted daemon replays, or silently resumes a stream mid-gap.** Neither. A restarted daemon
starts its watermark at the current max, so 20 writes made while it was down are not replayed; and
a non-TTY `tasqx watch` does not quietly resume across the restart, because it exits first
(`tasqx watch: daemon closed the connection`, exit 1). A script sees a non-zero exit, not a stream
missing 20 rows.

**Panic isolation was verified.** It was not, and that is worth saying rather than implying. No
input constructed here panicked the daemon's dispatch: across every hostility run the daemon's
stderr contained only `listening` and `stopped`. The `catch_unwind` seam is real (D40, D44) and
covered in-crate. This pass found no way in from the wire, which is a good result and not a
verification of the seam.

---

# Is the daemon worth running for an agent workflow?

No, as things stand — and the reason is finding #8 rather than anything wrong with the daemon.

The daemon's three products are a warm connection, a single writer, and a push stream. An MCP agent
gets none of them and needs two of them less than it looks. It cannot reach the socket at all, so
the warm connection is unavailable to it; it already holds a long-lived in-process `Engine`, which
is faster than the socket would be (0.41 ms per socket round trip is cheap, but a one-shot through
the daemon measured 12 ms *slower* than in-process). Single-writer turns out not to be load-bearing:
nine writers across a process boundary, one of them outside the daemon entirely, produced a dense
`short_id` sequence and one event per mutation. And the push stream reaches subscribers from an
agent's writes anyway — the external-writer poller picks them up within 400 ms without the agent
knowing a daemon exists.

What the agent *does* get from a daemon running beside it is the hazard: two stores with nothing
saying so (#2), and a race it loses 199 times in 200 because its two round trips lose to the
human's one (#7's measurement). The honest summary is that the daemon is for the human's live
surfaces — `watch`, and whatever eventually replaces it — and the agent should be told plainly that
it is not a client of it. That is a documentation ruling (#8) more than an engineering one, and it
is cheap.

The one thing that would change this answer is the auto-spawn half of D5 combined with routing
`mcp serve` through `open_backend`: then agent and human share one writer, one store and one push
stream, and #2, #8 and #11 all close at once. That is a considerably larger piece of work than
anything else in this report, and nothing here argues it is urgent.
