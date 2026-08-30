# 🔵 Findings from a full working day as an MCP client

**Source:** Not a code audit. These come from one Claude Code session (2026-08-30) that used
tasqx over MCP for real work all day — a research deliverable, five task creations, a dependency
chain, ten annotations, three memory writes and two summaries — on the live store
(182 tasks, 14 projects). Every finding below is something that was hit, not something that was
read about.
**Reporter:** Claude Code session `98b3caa9`, working from `C:\dev\raiders`.
**Bias to correct for:** an agent notices friction that a human at the CLI never meets, and misses
everything the CLI does well. This is one client's view of one surface.

Existing audits: `docs/reviews/TODO_CRITICAL.md`, `TODO_MEDIUM.md`, `TODO_LOW.md`
(Code Audit 2026-07-20). No overlap found with those; this is a different lens.

---

## #1: The MCP surface is additive-only — every corrective method is unreachable

**Severity:** HIGH
**Category:** API surface / correctness
**File:** `crates/tasqx-core/src/dispatch.rs:100-130` vs `crates/tasqx-core/src/mcp.rs`
**Estimated Effort:** 1-2 days

### Problem

`dispatch::PARAMS` carries **25 methods**. The MCP server exposes **15**. The ten omissions are
not a random subset — with the exception of the internal ones, *every* omitted method is the
corrective or destructive half of a pair that is otherwise exposed:

| Exposed over MCP | Omitted counterpart |
|---|---|
| `memory.add`, `memory.search` | **`memory.remove`**, `memory.import` |
| `dependency.add` | **`dependency.remove`** |
| `tag.add` | **`tag.remove`** |
| `task.modify`, complete | **`task.cancel`**, **`task.reopen`** |
| `project.create`, `project.list` | **`project.archive`**, `project.use` |
| — | **`event.revert`**, `store.export`, `tokens.recompute` |

An agent driving tasqx can create records and cannot fix them. That is not a missing convenience;
it is a correctness property. An agent that writes something wrong has no path back.

**This happened today, twice.**

1. A memory was written asserting that three skills had been archived. Twenty minutes later the
   claim turned out to be wrong for one of them. With no `memory.remove` over MCP, the only
   available repair was to **write a second memory contradicting the first**. Both now sit in the
   store; `memory.search` returns both, BM25-ranked, with no recency weighting and no supersession
   relation. The next agent to search that topic gets a true document and a false one and no signal
   which is which. This is precisely the failure mode that, earlier in the same session, justified
   deleting a stale `.remember/` directory from another repo.
2. Three tasks were created with `scheduled` set to a review date, which would have hidden them
   from the working set for four weeks. That one *was* recoverable — `task.modify` is exposed.
   The contrast is the point: the repair path existed for the field and not for the memory.

`event.revert` is the sharpest instance. tasqx **has** an undo. Agents cannot reach it.

### Acceptance Criteria

- [ ] `memory.remove` is exposed over MCP. This one alone closes the correctness hole.
- [ ] `dependency.remove`, `tag.remove`, `task.cancel` and `task.reopen` are exposed, or `DESIGN.md`
      records a decision explaining why an MCP client is deliberately denied them.
- [ ] A test asserts the MCP tool list against `dispatch::PARAMS` with an explicit allow-list of
      intentional omissions, so the surfaces cannot drift apart silently again. The repo already
      does exactly this for docs drift (`doc_gate_tests`) — the same guard applied to the MCP table
      would have surfaced this.
- [ ] If corrective methods stay unexposed, `memory.add`'s description says so, so an agent knows
      before it writes that the write is permanent.

### Recommended Approach

Expose `memory.remove` first and separately; it is the highest ratio of correctness to effort.
Consider a `supersedes: <id>` field on `memory.add` so a correction can demote its predecessor in
search ranking even where removal is not wanted — an append-only store with no supersession
relation degrades as it grows.

---

## #2: `tasqx_get_task` is unbounded, and `tool_ok_with_view` emits the payload twice

**Severity:** HIGH
**Category:** API surface / usability
**File:** `crates/tasqx-core/src/mcp.rs:788-802` (`tool_ok_with_view`)
**Related:** `docs/specs/2026-07-29-mcp-task-detail-rendering-design.md` (D49)
**Estimated Effort:** 1-2 days

### Problem

`tasqx_get_task 59` returned **58,159 characters** and exceeded the client's tool-output limit.
The client spooled it to a file and instructed the model to read it back in chunks. The task was
not pathological — it is a real feature task with a normal number of annotations accumulated over
five days.

Two causes compound:

1. **No bound of any kind.** `grep -niE "truncat|max_bytes|size_limit|limit"` over `mcp.rs` finds
   `limit` on `tasqx_list_tasks` and `tasqx_search_memory` and nothing on `tasqx_get_task`. There
   is no annotation `limit`, no `offset`, no newest-first, no byte budget. A caller who knows the
   task is large **cannot ask for less**.
2. **D49 doubles it.** `tool_ok_with_view` ships the rendered markdown as block one and
   `serde_json::to_string_pretty(result)` as block two. Every annotation body is therefore
   transmitted twice in the same response, once rendered and once as escaped JSON. The rendering
   decision is sound; paying for it twice on an unbounded field is what makes the limit arrive at
   half the task size it otherwise would.

The practical result: the task with the richest history — exactly the one whose history is most
worth reading — is the one the tool cannot return.

### Acceptance Criteria

- [ ] `tasqx_get_task` accepts `annotations_limit` and `annotations_offset` (or an equivalent),
      defaulting to the most recent N rather than all.
- [ ] When annotations are elided the response says so explicitly, with the total count and the
      call that fetches the next page. Silent truncation is worse than the current failure.
- [ ] The JSON block is suppressible (`view_only`, or omitted when a view rendered successfully) —
      or the two blocks share one copy of annotation bodies.
- [ ] A regression test builds a task with ~200 annotations and asserts the response stays under a
      configured byte budget.

### Recommended Approach

Pagination is worth more than a byte cap, because a cap still leaves the caller unable to reach the
rest. Default to newest-first: in every use this session, the recent annotations were the wanted
ones, and the oldest were the reason the payload was large.

---

## #3: `complete_task` refuses attribution metadata unless the caller also knows its token spend

**Severity:** MEDIUM
**Category:** API ergonomics
**File:** `crates/tasqx-core/src/mcp.rs` — `tasqx_complete_task` parameter validation
**Estimated Effort:** 2-4 hours

### Problem

Completing task #207 with `model`, `tool` and `session_id` — all documented as optional — failed:

```
error [bad_request]: `tool` was given without any token count — send input_tokens,
output_tokens, cache_read_tokens or cache_creation_tokens alongside it, or drop `tool`
```

The coupling is real and intentional (`tool` defaults to `client` "when token counts are present"),
but it is discoverable only by failing. The parameter descriptions present the fields as
independent optionals.

The deeper issue is that the coupling makes the feature unusable in its main case. **An agent
generally does not know its own token spend**; the harness does not expose a running count to the
model. So the requirement is: supply a number you cannot observe, or forfeit recording the model
and tool you *can* observe. The retry that succeeded dropped `model` and `tool` entirely — the
store now records the completion with neither, which is strictly worse than recording what was
known.

The store bears this out: **84 done tasks carry 15 h 20 m of tracked time against 413 h of
completed estimate** — 3.7%. Attribution is, in practice, not being captured.

### Acceptance Criteria

- [ ] `model`, `tool` and the correlation fields are accepted without token counts, and recorded.
- [ ] If the coupling is kept, it is stated in the `tool` and `model` parameter descriptions, not
      only in the rejection message.
- [ ] The completion response says what was and was not recorded, rather than only hinting at
      log-parse fallback.

---

## #4: `scheduled` / `due` / `wait` semantics are not stated where the caller chooses between them

**Severity:** LOW
**Category:** Documentation / schema
**File:** `crates/tasqx-core/src/mcp.rs` — `tasqx_add_task` parameter descriptions
**Estimated Effort:** 1 hour

### Problem

All three take an identical description: *"Date/time in the tool's date grammar."* Nothing says
what each one *does*. Three tasks were created with `scheduled` set to a four-week review date,
intending "check back then"; the effect would have been to hide work meant for that week from the
working set until late September. Caught only by reading the returned `status: "backlog"` and
reasoning backwards.

`due` and `wait` both plausibly fit the same sentence, which is the tell.

### Acceptance Criteria

- [ ] Each of `scheduled`, `due` and `wait` gets one clause naming its effect on visibility and
      urgency — not just its accepted format.
- [ ] The `add_task` description notes that a task with a future `scheduled` lands in `backlog`.

---

## #5: Annotations have no type, so they accrete into one undifferentiated blob

**Severity:** LOW — design suggestion, not a defect
**Category:** Data model
**Estimated Effort:** unscoped

### Problem

Annotations are the best thing about tasqx in agent use. This session reconstructed five weeks of
Unity-bridge history out of annotations on #29, #59, #82, #84 and #138, and cited a specific dated
technical fact found by search without knowing which task held it. Nothing in a flat file does that.

But every annotation is one untyped markdown body, so a task's history is retrievable only whole —
which is how #59 reached 58 KB (see #2). There is no way to ask for the decisions, or the
blockers, or the last three.

### Acceptance Criteria

- [ ] Consider an optional `kind` on `annotation.add` (`approach` / `decision` / `blocker` /
      `result` / `correction`), filterable from `get_task`.
- [ ] Keep it optional and untyped by default. The convention in use — approach on start, decisions
      as they happen, outcome on finish — already works; typing it should make it queryable, not
      mandatory.

---

## Investigated and **not** a tasqx defect

Recorded so nobody chases them.

- **Mojibake in the spooled `get_task` output** (`—` rendering as `?`). Reproduced only through the
  client's file-spooling path and a Python reader on a Windows `cp1252` stdout; the second failure
  (`UnicodeEncodeError: '\u25b8'`) was in the reporter's own script, not in tasqx output. No
  evidence tasqx emits anything but UTF-8. Not filed.
- **`demo.checkout` (4 tasks) and `demo.darkmode` (5 tasks) appear in `list_projects`.** Usage, not
  a defect — `project.archive` exists and `list_projects` already takes `include_archived`. Worth
  noting only because `project.archive` is one of the methods MCP cannot reach (#1), so an agent
  asked to tidy the project list has no way to do it.

---

## Usage observations, offered without a fix

Not defects; context on how the store looks from outside after a day in it.

- **97 pending tasks / 1,352 h of estimate against a stated 20 h/week** is 67 weeks of backlog.
  That is an inbox rather than a plan. No tool change follows from this, but any feature justified
  by "helps you get through the backlog" should be weighed against the fact that the backlog is not
  being got through.
- **`tasqx` + `tasqx.dashboard` hold 53 of 182 tasks — 29%.** Worth knowing when prioritising.
- **Nothing in the store is in version control.** The design docs, the spec and the handoff of the
  consuming project are all in git and survive tool loss; 84 tasks' worth of annotated reasoning is
  not. `store.export` exists in dispatch and is the obvious primitive, and is also unreachable over
  MCP (#1), so an agent cannot snapshot what it just wrote.

---

## Suggested order

1. **#1, `memory.remove` only** — smallest change here, and the only correctness hole. Half a day.
2. **#2 pagination** — unblocks the tasks whose history is worth most.
3. **#3 and #4** — hours each, pure ergonomics.
4. **#1 remainder** behind a drift test, so the two surfaces cannot separate again.
5. **#5** only if the annotation convention starts straining.
