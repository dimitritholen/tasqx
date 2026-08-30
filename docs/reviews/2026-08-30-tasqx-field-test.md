# Field test: a working day through tasqx, from a client that had not seen the source

**What ran.** One simulated project day against tasqx 0.4.0 (`886b4e2b592c`): a new project
`acme.ledger`, a feature decomposed into 7 tasks, 7 dependency edges, two tasks driven
start-to-finish with timers and four-annotation histories, two memory docs written and searched,
four deliberate mistakes repaired (a mis-tag, a dependency that should not exist, a task closed
too early, a memory that turned out to be false), and status asked three ways. **59 MCP calls**
against a scratch store over stdio JSON-RPC (`TASQX_DB` under the session scratchpad,
`--no-daemon` on every invocation, verified with `tasqx config store` before the first write).
Reads also ran against the live store — 223 tasks, 15 projects, real annotation histories — and
nothing there was created, changed or removed except the finding tasks this report ends with.

**How long.** About 90 minutes of wall clock, most of it deciding what to try and checking claims
against `DESIGN.md`. The tool time is negligible and that is itself a result: 230 `tasqx_get_task`
calls completed in 51 ms inside one server process.

**Bias to correct for.** This is one MCP client's view, and an agent meets friction a human at the
CLI never does — nobody typing `tasqx show 102` cares that the JSON block doubles the payload.
The CLI surface was exercised only where it settled a question the MCP surface raised. Findings
are separated into **defects**, **detours** (it works, it cost me calls or bytes I should not have
spent) and **absences** (I wanted something that is not there), because conflating them is how a
report gets argued with instead of acted on.

Existing filings cross-checked before writing: `#213`–`#221` from
`docs/reviews/2026-08-30-mcp-client-findings.md`, all shipped as D63–D67. Two of the findings
below sit next to closed ones (`#216`, `#217`) and say exactly what is left over.

**Filed as** `#224`-`#233` on the live store, project `tasqx`, tag `field-test`, one task per
finding with the reproduction in an annotation. Those ten tasks and their annotations are the only
writes this exercise made to the live store.

**Resolved in `fix/field-test-findings` (v0.5.0), as D68–D72.** Nine of the ten are code
changes; #10 is ruled deliberate with the reason recorded, which is the other outcome its own
acceptance criteria allowed. A verifier re-runs all ten against a chosen binary: **1/11 checks
pass against the v0.4.0 build, 11/11 against this branch.** Where a measurement below differs
from the fix that shipped, the D-entry carries the later number — read this document as the
report that prompted the work, not as a description of the code.

---

## #1: `tasqx_list_tasks` has no default limit and no response budget

**Severity:** HIGH · **Category:** defect / API surface · **Effort:** 1-2h

### Problem

The first call a new agent makes is "show me my tasks". On the live store that returns
**180,412 bytes** — roughly 45,000 tokens — in a single content block with no limit applied, no
elision, and no notice that anything was large:

```
  180412 B  count=223   tasqx_list_tasks {}
   62117 B  count=223   tasqx_list_tasks {"fields":["short_id","title","project","status","blocked","urgency","due"]}
   43236 B  count=54    tasqx_list_tasks {"filter":"@working"}
   10948 B  count=54    tasqx_list_tasks {"filter":"@working","fields":["short_id","title","project","urgency"]}
```

```
list_tasks blocks: 1 [159859]
  notice present: False
```

The tool's own schema invites the call: *"Omit it (or send `""`) for every task: no filter means
no filtering."* `limit` exists and defaults to nothing.

This is the shape D63 fixed for `task.get`, on the argument that *"bytes and not rows are the unit
a client's limit is expressed in"*. The transport-side page size, the byte budget and the bisection
were all built for one tool. `task.list`'s worst case is larger than `task.get`'s and it grows with
the store rather than with one task's history. `UNEXPOSED_METHODS` refuses to expose `event.list`
in these words — *"the audit log is unbounded and has no paging, so exposing it would repeat the
`task.get` mistake D63 fixed"* — while the exposed tool with the same property ships unbounded.

**And the escape hatch truncates silently.** `count` is the number of rows returned, not the
number that matched, and the result carries no total and no offset:

```
count= 233 rows= 233 keys=['count', 'tasks']  <- {"fields":["short_id"]}
count=   5 rows=   5 keys=['count', 'tasks']  <- {"fields":["short_id"],"limit":5}
count=  64 rows=  64 keys=['count', 'tasks']  <- {"filter":"@working","fields":["short_id"]}
count=   5 rows=   5 keys=['count', 'tasks']  <- {"filter":"@working","fields":["short_id"],"limit":5}
```

So a caller who does the right thing and passes `limit` gets a list that looks complete, cannot
tell that 228 rows were dropped, and has no way to ask for the next page. `task.get` answers all
three questions — `annotations_total`, `annotations_offset`, `annotations_next_offset` — and
`task.list` answers none of them.

Per-row cost on the live store is ~800 B, of which six fields are `null` on most rows
(`recurrence`, `remind`, `scheduled`, `wait`, `completed`, `active_since`) and two are machine
identity (`id`, `_rev`). `fields` cuts it 4×, but an agent only passes `fields` after it has been
burned once.

### Acceptance criteria

- [ ] The MCP transport supplies a default `limit` when the caller names none, as it already
      supplies `annotations_limit`, `expected_rev` and `client`.
- [ ] The response says how many rows were withheld and how to get the next page, in the block a
      model reads — the same contract `task.get` already keeps.
- [ ] A caller that names its own `limit` is respected, including one asking for everything.
- [ ] The result carries the matched total and an offset, so a limited list is distinguishable
      from a complete one — the contract `task.get` already keeps.
- [ ] The JSON API's frozen shape is untouched: this is transport policy, per D63's own reasoning.

---

## #2: a memory doc's body cannot be read back through any per-document route

**Severity:** HIGH · **Category:** defect / API surface · **Effort:** 2-4h

### Problem

`memory.search` returns a snippet of 60–88 characters and an id. No verb anywhere takes that id and
returns the document:

```console
$ tasqx --no-daemon memory search "CAMT" --json | ...
doc 75 ['id', 'kind', 'rank', 'snippet', 'source', 'title']
doc 60 ['id', 'kind', 'rank', 'snippet', 'source', 'title']
```

```console
$ grep -n '"memory\.' crates/tasqx-core/src/dispatch.rs
128:    ("memory.add", &["title", "body", "source"], false),
129:    ("memory.search", &["query", "limit", "scope", "raw"], false),
130:    ("memory.remove", &["id"], false),
131:    ("memory.import", &["docs"], false),
```

`tasqx memory --help` lists `add | search | rm | import`. There is no `get` and no `show`.

I wrote a 653-character decision doc in the morning and in the afternoon the search found it and
handed me 60 characters of it. The part I wrote it for — the paragraph explaining *why* matching on
the qualified XML namespace fails silently — is in the store and is not reachable. Annotations
escape this, because a hit names `task:#1` and `task.get` returns the body. Docs do not: `source`
is free text.

**One route does exist and it is worth naming precisely, because it is the reason this is a
2-4 hour fix and not a redesign.** `store.export` carries doc bodies (D41 recorded that omitting
them was a bug):

```console
$ tasqx --no-daemon export | ...
top keys: ['default_project', 'docs', 'dropped_dependencies', 'projects', 'tasks']
docs: 1 first has body chars: 653
```

That route is CLI-only, and `UNEXPOSED_METHODS` keeps `store.export` off MCP because *"the payload
is the whole store"*. So the recovery path for one document is: dump every task, project and
document in the store, over a surface the client cannot reach. The same table already concedes the
client is elsewhere — its reason for withholding `memory.import` is *"the filesystem the CLI reads
is not the one an MCP client is on"*.

An agent can write to memory and cannot read what it wrote. That is the D64 asymmetry one noun
over: D64 fixed *write without retract*, and *write without read* is still open.

### Acceptance criteria

- [ ] `memory.get {id}` returns one document's title, body, source and timestamps, exposed as a
      read-scope MCP tool.
- [ ] Or `memory.search` gains `include_body` / a body field on doc hits, with the same byte
      discipline `task.get` uses.
- [ ] Whichever lands, the search result documents how to get from a hit to the text.

---

## #3: every write tool is `destructiveHint: true`, so the hint D64 depends on says nothing

**Severity:** MEDIUM · **Category:** defect / protocol conformance · **Effort:** 1-2h

### Problem

All nineteen tools, read from the running server's `tools/list`:

```
tool                       ro    destr  idem
tasqx_add_task             False True   False
tasqx_annotate_task        False True   False
tasqx_add_memory           False True   False
tasqx_create_project       False True   False
tasqx_start_timer          False True   False
tasqx_remove_memory        False True   False
...
```

Every read tool is `false`, every write tool is `true`, because the value is the write flag:

```console
$ grep -rn "destructiveHint" crates/
crates/tasqx-core/src/mcp.rs:1139:                    "destructiveHint": s.write,
```

The MCP specification defines `destructiveHint` as *may perform destructive updates*, with `false`
meaning *additive only*. Creating a task, attaching an annotation, starting a timer and writing a
memory doc are additive. They are labelled the same as permanently deleting a document.

This is not cosmetic, because D64 rests on the hint doing work:

> `tasqx_remove_memory` ships as a write-scoped MCP tool […] `destructiveHint` true **so the host
> applies its confirmation policy** (§7).

A host that gates on `destructiveHint` gates on all fourteen writes or on none. In practice the
operator turns the gate off — a confirmation prompt on every annotation is unusable — and
`tasqx_remove_memory` loses the safeguard D64 chose for it. The documentation page states the
opposite as a feature:

```console
$ sed -n '1862,1870p' crates/tasqx-cli/src/docs.rs
         "{reads} read tools always; {writes} write tools only with write scope. Each carries MCP \
          annotations (<code>readOnlyHint</code>, <code>destructiveHint</code>) so a client can reason \
          about them before calling.",
```

There is nothing to reason about: `destructiveHint` is `!readOnlyHint`.

Same field, secondary: `idempotentHint` is `false` on all fourteen writes, including
`tasqx_tag_task` and `tasqx_stop_timer`, where repeating the call converges.

### Acceptance criteria

- [ ] `destructiveHint` becomes a per-tool property. True for `tasqx_remove_memory`,
      `tasqx_remove_dependency`, `tasqx_untag_task`, `tasqx_modify_task` and `tasqx_complete_task`;
      false for the additive ones.
- [ ] The `ToolSpec` field is set at each definition so an added tool must state it, the way
      `UNEXPOSED_METHODS` makes silence unavailable.
- [ ] A test asserts `tasqx_add_task` is not destructive and `tasqx_remove_memory` is, so the
      distinction cannot collapse back into the write flag unnoticed.

---

## #4: the JSON-omission notice names a recovery that removes the budget it just enforced

**Severity:** MEDIUM · **Category:** defect / documentation-in-payload · **Effort:** 1h

### Problem

`tasqx_get_task` on live `#102` (20 annotations) stays under budget and says so:

```
args={'ref': 102}  envelope=22932 B  blocks=1
### Annotations (5 of 20)
_Showing the 5 most recent, oldest first. 15 older elided - re-read this task with
 `annotations_offset: 5` for the next page._
_Machine-readable JSON omitted: both blocks together exceeded this tool's response budget, and the
 rendered view above carries the same annotations. Pass `annotations_limit` to get the JSON block
 back._
```

Following that last sentence with the page size the server itself just chose:

```
args={'ref': 102, 'annotations_limit': 5}   envelope= 46512 B  blocks=2
args={'ref': 102, 'annotations_limit': 20}  envelope=173032 B  blocks=2
```

The server refused to ship more than ~24 KB, then instructed the caller into a 46 KB response —
and, if the caller passes `annotations_total` as the tool's own description recommends for reading
a full history, a **173 KB** one. That is 7× the budget and past most clients' tool-output limit.

D66 rules that naming a limit is the deliberate escape hatch — *"a caller that named its own
`annotations_limit` gets both blocks, however large"* — and that ruling is not in dispute. The
defect is that the notice does not say so. It reads as a bounded retry, and the most obvious value
to retry with is the one printed two lines above it.

### Acceptance criteria

- [ ] The notice states that naming `annotations_limit` returns both blocks *unbounded*, so the
      caller knows it is opting out of the budget rather than paging within it.
- [ ] A test asserts the notice contains that clause, on the D64 precedent that a warning nothing
      pins survives exactly as long as the next edit.

---

## #5: below the budget, `tasqx_get_task` pays the duplicate block in full — and D66's cost model says otherwise

**Severity:** MEDIUM · **Category:** detour / measurement correction · **Effort:** 2-4h

### Problem

`#217` closed "D49 ships every annotation body twice" and D66 answered it *above* the budget. Below
it, nothing changed, and D66's estimate of what that costs is wrong. Measured on the live store:

```
args={'ref': 177}                          envelope=6375 B  blocks=2
  block[0]  2823 B   (rendered view)
  block[1]  3254 B   (the same content as escaped JSON)
  annotations returned=1 of total=1
  duplication: JSON block is 54% of the payload

args={'ref': 177, 'annotations_limit': 0}  envelope=1351 B  blocks=2
  block[0]   380 B
  block[1]   736 B
  duplication: JSON block is 66% of the payload
```

D66 states:

> The second block is the first block again. D49 ships the rendered markdown and then
> `to_string_pretty` of the same result. **On an ordinary task that costs a few hundred bytes of
> field names.**

On a task with one annotation it costs 3,254 bytes, and on a task read with `annotations_limit: 0`
— the documented way to read a task's fields without its history — the JSON block is two thirds of
a 1.3 KB response, for a table that is 380 bytes. The budget engages at ~24 KB, so a response
landing at 20 KB pays ~10 KB of duplication and no notice appears. Every `task.get` an agent makes
during ordinary work is in that range.

There is no `format` or `blocks` parameter. The only way to halve a `task.get` is to push it over
24 KB, which is not a thing a caller can do on purpose.

### Acceptance criteria

- [ ] `tasqx_get_task` takes a parameter selecting the rendered view, the JSON, or both, defaulting
      to today's behaviour so nothing shipped changes shape.
- [ ] Or the transport drops the JSON block whenever the response exceeds a much lower threshold
      than the hard budget, on D66's own reasoning that the dropped block is the redundant one.
- [ ] D66's "a few hundred bytes of field names" is corrected with a measured figure, since it is
      the sentence that sized the fix.

---

## #6: `task.list` cannot report what a blocked task is blocked by

**Severity:** MEDIUM · **Category:** absence · **Effort:** 2-4h

### Problem

"What is blocked?" is one call. "Blocked by what?" is one call per blocked task.

```
tasqx_list_tasks {"filter":"@blocked project:tasqx"}
-> 1 task, blocked: true, and nothing about the cause
```

`depends_on` is not in the `fields` enum — the enum is `_rev, active_since, blocked, completed,
created, due, estimate, id, modified, priority, project, recurrence, remind, scheduled, short_id,
status, status_unrecognized, tags, title, tracked, urgency, wait` — and it is absent from the full
default row too, which carries `blocked` and twenty-two other fields. The only source is
`task.get`, at 1.3 KB for a bare task and up to 23 KB for one with history.

**This is a stated position, not an oversight**, so it is filed as an absence. D58:

> `task.list` already returns `blocked` per row, so the panel costs nothing beyond the shared
> snapshot; `depends_on` lives on `task.get` alone, so the *cause* is fetched lazily for the
> focused row only, rather than N calls ahead of time for a panel that is usually empty.

That argument holds for the dashboard, which has a cursor and fetches one row on demand. It
inverts on the MCP surface: an agent has no cursor, cannot ask a human which row to focus, and
must fetch all N up front to answer the question a person actually asks. The lazy fetch that costs
one `task.get` in the TUI costs N of them here, at a hundred times the bytes an extra array of
integers per row would.

### Acceptance criteria

- [ ] `depends_on` joins the `fields` enum on `task.list`, opt-in so the default row is unchanged
      and the extra join is only paid when asked for.
- [ ] Or `DESIGN.md` records that an agent is expected to spend N `task.get` calls for this, so the
      next client stops looking.

---

## #7: `task.reopen` does not say what it re-blocked, while `task.done` says what it unblocked

**Severity:** MEDIUM · **Category:** defect / silent state change · **Effort:** 1-2h

### Problem

Same task, two calls in one process:

```
===== tasqx_complete_task {"ref": "3"}
{ "completed": "...", "status": "done", "tokens_hint": "...", "unblocked": [ 4 ] }

===== tasqx_reopen_task {"ref": "3"}
{ "short_id": 3, "status": "pending" }
```

`#4` depends on `#3`. Completing `#3` reported that `#4` became actionable. Reopening `#3` put `#4`
back into `blocked` and said nothing. An agent that reopens a task — which is exactly what it does
when it discovers it closed one too early, the scenario D67 exposed the tool for — has just
removed work from its own actionable set with no signal. The next `@working` list is shorter and
nothing explains why.

`unblocked` on completion is one of the genuinely good things about this API: it is the tool
telling you what to do next without being asked. Its inverse is missing, which is the same
symmetry argument D67 is built on, applied to a response field rather than to a tool.

### Acceptance criteria

- [ ] `task.reopen` returns `blocked` (the tasks that returned to blocked because of this reopen),
      mirroring `unblocked` on `task.done`.
- [ ] The same for `task.modify` when a status transition re-blocks dependents.

---

## #8: `report.summary` does not name the window it applied

**Severity:** LOW · **Category:** defect / result shape · **Effort:** 1h

### Problem

```
summary keys: ['generated', 'groups']
```

A report scoped to a date window comes back with the same two keys as one scoped to nothing. The
totals are correct and the period they cover is not in the response. A summary quoted into a
handoff note, an annotation or a status message is a number with no period attached, and the next
reader has no way to check it against the right week.

`generated` is a timestamp of the call, which is easy to misread as the boundary.

### Acceptance criteria

- [ ] The result echoes the `filter` it applied (and `all`), so a total cannot be read against the
      wrong period.

---

## #9: a zero-hit `memory.search` gives no reason, and stopwords are required terms

**Severity:** LOW · **Category:** detour · **Effort:** 1-2h

### Problem

Every token in a plain query is a required term, including the ones carrying no meaning:

```
0 hits  <- 'why did we choose a named pipe instead of TCP for the daemon'
3 hits  <- 'named pipe daemon'
0 hits  <- 'named pipe TCP'
1 hits  <- 'daemon socket named pipe'
3 hits  <- 'named pipe'
```

The first query is how an agent asks a question. It returns `{"count": 0, "hits": []}`, which is
byte-for-byte what the store returns when it genuinely holds nothing on the subject. The document
that answers it is two words away.

**D41's ruling is not in dispute** — FTS5 over embeddings, and phrase-escaping at the door, are
both decided with reasons. The gap is the diagnostic: a caller that over-constrained a query and a
caller that asked about something nobody wrote down get the same answer, and only one of them
should retry.

### Acceptance criteria

- [ ] A zero-hit result carries the terms that were required, so the caller can see it asked for
      thirteen of them.
- [ ] Optionally, a zero-hit query retries without stopwords and labels the result as relaxed.

---

## #10: `annotation.add` echoes the body back; `memory.add` does not

**Severity:** LOW · **Category:** detour · **Effort:** 1h

### Problem

Two writes in the same session, both carrying long-form prose:

```
[1] tasqx_annotate_task          626 B     (body sent: ~350 B)
[2] tasqx_annotate_task          677 B
[5] tasqx_add_memory             292 B     (body sent: ~700 B)
```

`tasqx_annotate_task` returns the whole body it was just given. `tasqx_add_memory`, with twice the
body, returns `{created, id, title}`. `tasqx_add_task` does not echo the title either. Annotations
are the write an agent makes most — this session made six — and each one costs its own text a
second time for nothing the caller does not already hold.

Across the day's ten annotations that is roughly 3.5 KB of my own prose returned to me. Small in
isolation; it is on the hot path of the workflow this tool exists to support.

### Acceptance criteria

- [ ] `annotation.add` returns `{id, created, short_id}` and drops the body, matching `memory.add`.
- [ ] Or the inconsistency is recorded as deliberate with the reason, so it stops reading as drift.

---

# Measurements

All sizes are the full JSON-RPC response envelope in bytes, as a client would receive it.

## Fixed session cost

| | bytes |
|---|---|
| `initialize` | 142 |
| `tools/list`, 19 tools | **17,574** |

That 17.5 KB is paid once per session and sits in context for its whole life — roughly 4,400
tokens before a single task is read. The three largest schemas are `tasqx_complete_task` (2,873 B,
eleven properties, ten of them token accounting), `tasqx_add_task` (2,308 B) and `tasqx_list_tasks`
(1,317 B, most of it the 22-value `fields` enum). Not filed as a finding — every one of those
descriptions earns its place, and the enums are what make the parameters usable — but it is the
single largest line item in the budget and worth knowing.

## Per-call, scratch store (7-task project)

| call | bytes |
|---|---|
| `tasqx_reopen_task` | 153 |
| `tasqx_stop_timer` | 159 |
| `tasqx_remove_memory` | 180 |
| `tasqx_add_dependency` / `tasqx_remove_dependency` | 181 |
| `tasqx_tag_task` | 200 |
| `tasqx_complete_task` (with token counts) | 215 |
| `tasqx_untag_task` | 223 |
| `tasqx_start_timer` | 244 |
| `tasqx_create_project` | 251 |
| `tasqx_add_task` | 285–286 |
| `tasqx_add_memory` | 286–292 |
| `tasqx_summary` | 376–581 |
| `tasqx_complete_task` (no counts, + `tokens_hint`) | 440–500 |
| `tasqx_annotate_task` | 594–680 |
| `tasqx_search_memory` (1–3 hits) | 139–839 |
| `tasqx_get_task`, no annotations | 1,365 |
| `tasqx_list_tasks`, 7 rows, 5 fields | 1,558 |
| `tasqx_list_tasks`, 7 rows, default fields | 5,568 |

**The writes are excellent.** A task creation costs 286 bytes; four repairs — untag, remove
dependency, reopen, remove memory — cost 737 bytes together. Nothing in that column is padded.
Every problem is on the read side, and every one of them is a default: default fields on
`list_tasks`, default both-blocks on `get_task`, default no-limit on `list_tasks`.

## Per-call, live store (223 tasks, 15 projects)

| call | bytes |
|---|---|
| `tasqx_summary` grouped by project | 1,625 |
| `tasqx_search_memory`, 8 hits | 3,401 |
| `tasqx_list_projects` | 5,192 |
| `tasqx_get_task` #177, 1 annotation | 6,375 |
| `tasqx_list_tasks @working` + 4 fields | 10,948 |
| `tasqx_get_task` #102, 20 annotations, budget engaged | 22,932 |
| `tasqx_list_tasks @working`, default fields | 43,236 |
| `tasqx_get_task` #102 with `annotations_limit: 5` | 46,512 |
| `tasqx_list_tasks` + 7 fields | 62,117 |
| `tasqx_get_task` #102 with `annotations_limit: 20` | 173,032 |
| `tasqx_list_tasks` no filter, no fields | **180,412** |

## Calls per workflow

| workflow | calls | bytes |
|---|---|---|
| Create project, decompose into 7 tasks, wire 7 deps, read back | 16 | 9,092 |
| One task start to finish: start, 4 annotations, stop, complete | 7 | 3,544 |
| Four repairs: untag, remove dep, reopen, remove memory | 4 | 737 |
| "What is actionable now" | 1 | 603 |
| "Where were we" on a task with history | 2 | 1.8 KB – 23 KB |
| "What did this week cost" | 1 | ~500 |
| Whole simulated day | **59** | ~35 KB excluding `tools/list` |

## Speed

Nothing was slow enough to notice, at any point, on any surface.

```
230 tasqx_get_task probes in 51 ms, one process     -> 0.22 ms/call
process start + initialize + tools/list             -> 57 ms
8 writes in one process (project + 7 tasks)         -> 50 ms
```

Per-call latency inside a live server process is sub-millisecond. The dominant cost of a one-shot
`tasqx mcp serve` is the ~30 ms of process start, which a long-lived MCP server pays once. **Speed
is not a question this tool has to answer.**

---

# Investigated and not a defect

Chase none of these again.

**`report.summary` cannot answer "what did this week cost".** It can, in one call and about 500
bytes. The filter DSL's date terms reach the report:

```
tasqx_summary {"filter":"completed.after:2026-08-23","group_by":"project",
               "metrics":["count","est_total","tracked_total"]}
-> 3 groups; tasqx: 10 done, PT58H estimated, PT5H39M9S tracked
```

The only thing wrong with the result is that it does not echo the window — filed as #8, which is a
much smaller claim.

**`-blocked` as a filter term is not a silently ignored token.** `blocked:false` is refused with a
list of what the grammar takes; `-blocked` parses correctly as *not tagged `blocked`* and matches
everything, because no task carries that tag. Confusing at first read, correct on inspection. The
actionable-now question is `@working`, which excludes blocked tasks and answered it in one call.

**`task.cancel` is not missing from MCP.** `tasqx_modify_task {"ref":"7","set":{"status":"cancelled"}}`
returned `{"_rev":3,"short_id":7}` and the task reads `cancelled`. Documented in
`UNEXPOSED_METHODS` and verified working.

**`event.revert`, `store.export`, `project.use`, `memory.import`, `token.add`, `event.list`,
`project.archive`, `store.import`, `tokens.recompute`, `core.capabilities` are absent by decision.**
`UNEXPOSED_METHODS` in `mcp.rs` names each with its reason, and a guard binds that table to the
tool list in both directions (D67). I read all thirteen entries before filing anything about a
missing capability.

**D63 paging and D66 budget work, and the notices are good.** On `#102` (20 annotations) and `#40`
(12), the response came back as one block at 22–24 KB with both facts stated where a model reads
them:

```
### Annotations (5 of 20)
_Showing the 5 most recent, oldest first. 15 older elided - re-read this task with
 `annotations_offset: 5` for the next page._
```

Nothing is silently dropped. The only complaint is the recovery sentence (#4).

**Optimistic concurrency works and its error is legible.**
`tasqx_modify_task {"ref":"6","set":{"priority":"H"},"expected_rev":1}` →
`error [conflict]: expected_rev 1 but task is at rev 4`. That is a message a caller can act on
without reading source.

**Timers record real intervals.** Start, five seconds, stop → `{"status":"pending","tracked":"PT6S"}`.
The `PT0S` totals elsewhere in this report are sub-second test intervals, not a bug.

**Scratch isolation holds.** `tasqx config store` reports the resolved path and whether a daemon
owns it, before any write. With no daemon running, `TASQX_DB` was honoured with and without
`--no-daemon`. The live store held 223 tasks before the day's work and 233 after — the ten finding tasks and nothing else.

---

# Not exercised

Stated so nobody reads absence as a pass. **No daemon was running** for any of this, so single-writer
routing, `watch`, push notifications and multi-client concurrency went untested — every call ran
in-process. The interactive surfaces (`dashboard`, `pick`, `agenda`, `why`, `chart`, the HTML
report) were not driven, nor were recurrence, `undo` / `event.revert`, reminders, themes, or an
export/import round trip. The CLI was used only where it settled a question the MCP surface raised.
Everything above is the MCP surface plus what a single in-process client can see of the store
behind it.

# Did it change how the work went?

Yes, in two specific places, and it is worth being precise about which — because the parts that
helped are not the parts a task manager is usually sold on.

The first is `unblocked`. Completing a task and being told, in the same response, which tasks just
became actionable, is the one thing a scratch file cannot do. `{"unblocked":[2,3]}` is the answer
to "what now" arriving without being asked for, computed from a graph I wired once and never had to
re-read. Across the day I never once had to work out what to pick up; the completion told me, and
`@working` confirmed it in 600 bytes. That is a real change in how the work goes, not a
convenience.

The second is annotations as a resume surface. Coming back to `#3` after closing it too early, the
history — approach, the namespace decision, the forty minutes lost, what was deliberately skipped —
was there in the order it happened, attached to the thing it was about. Reading `#102` on the live
store, a task I had nothing to do with, I could reconstruct what somebody had been doing and why
from the tool alone. A scratch file gets that wrong in a week, because nothing keeps the note and
the work together.

What did **not** change: memory. I wrote two docs, searched for them, and got back sixty characters
of what I wrote. The subsystem indexes prose it will not return, so the thing memory is for —
storing the decision *and the why* — half-works: the search finds that a decision exists and the
why stays in the store. In practice I fell back to annotations for everything durable, because
annotations can be read. That is the finding I would fix first, ahead of the 180 KB list.

And the cost is entirely on the read side. Writes are 200–700 bytes and the shape is right. But the
defaults on the reads are set for a small store and a terminal — the two calls an agent makes most
return 180 KB and a doubled payload, and the tool never says so. An agent that knows to pass
`fields`, `limit` and `annotations_limit` gets a genuinely cheap, genuinely fast task manager. An
agent that does not gets its context window spent on nulls before it has done any work, and there
is nothing in the tool descriptions that tells it which one it is about to be.
