//! Response-shape documentation: one line of English per key the JSON API
//! answers with (#647).
//!
//! # Why this lives in the engine, beside the freeze
//!
//! `tests/conformance.rs` already knows every key of every response — it is the
//! contract of record, and it fails the build when a key is renamed, removed or
//! added without being written down. What it cannot hold is what a key MEANS,
//! because a test file is not linkable from `src/` and the documentation
//! generator lives in the other crate. So the descriptions live here, in the
//! same crate as the engine that emits the keys, and
//! `documented_response_shapes_match_the_freeze` asserts this module and that
//! suite describe the same shape: same keys, same types, same nullable and
//! optional flags, per method and per nested object. A field added to the
//! freeze without a description is red, and a description with no field behind
//! it is red.
//!
//! That is the whole point. A shape that can change without its description
//! changing is a stale page, and the generated reference
//! (`tasqx-cli/src/docs/api_ref.rs`) renders these tables verbatim — nothing on
//! it is retyped prose about a response somebody once read.
//!
//! # The path vocabulary
//!
//! [`result_shape`] returns `(path, group)` pairs. `result` is the envelope's
//! `result` object itself; `result.tasks[]` is the shape of every element of
//! its `tasks` array; `result.groups[].cost` is an object inside such an
//! element. Several groups may share one path — that is how a composed shape is
//! spelled, exactly as the conformance suite composes it (a task row is the
//! core task, plus its live time, plus its blocked flag), and a renderer walks
//! them as one table per path.

/// One key of a response object: what it is called, what it holds, whether it
/// can be `null` or absent, and one line of what it means.
///
/// The first four are the conformance suite's own vocabulary, mirrored so the
/// guard can compare them; `desc` is the half only a human can write.
#[derive(Debug, Clone, Copy)]
pub struct FieldDoc {
    /// The JSON key, as the wire spells it.
    pub key: &'static str,
    /// `string`, `integer`, `number`, `boolean`, `array` or `object` — the same
    /// coarse names `Ty::name` gives in the conformance suite, and coarse for
    /// the same reason: a client breaks on a string that became a number, not
    /// on the difference between `3` and `3.0`.
    pub ty: &'static str,
    /// The key is always present, and `null` is one of its legal values.
    pub null_ok: bool,
    /// The key may be absent entirely.
    pub optional: bool,
    /// One line: what this key means to a caller. Markdown-free; the renderer
    /// escapes it and spells `backticked` spans as code.
    pub desc: &'static str,
}

/// Always present, never `null`.
const fn f(key: &'static str, ty: &'static str, desc: &'static str) -> FieldDoc {
    FieldDoc {
        key,
        ty,
        null_ok: false,
        optional: false,
        desc,
    }
}

/// Always present, may be `null`.
const fn n(key: &'static str, ty: &'static str, desc: &'static str) -> FieldDoc {
    FieldDoc {
        null_ok: true,
        ..f(key, ty, desc)
    }
}

/// May be absent; when present it is not `null`.
const fn o(key: &'static str, ty: &'static str, desc: &'static str) -> FieldDoc {
    FieldDoc {
        optional: true,
        ..f(key, ty, desc)
    }
}

/// `project.create`'s result.
pub const R_PROJECT_CREATE: &[FieldDoc] = &[
    f("id", "string", "The new project's uuid."),
    f("name", "string", "The project's name, as stored."),
    f("default", "boolean", "Whether this create claimed the default project — true only when the store had none."),
    n("current_default", "string", "The default project after the call, whichever it is. Null only in a store with no default at all."),
];

/// `project.list`'s result.
pub const R_PROJECT_LIST: &[FieldDoc] = &[
    f("count", "integer", "How many projects came back."),
    f("store_empty", "boolean", "Whether the store has ever held a task at all — how \"nothing yet\" is told apart from \"nothing matched\"."),
    f("projects", "array", "One row per project, sorted by name."),
];

/// One project as `project.list` lists it.
pub const PROJECT_LIST_ROW: &[FieldDoc] = &[
    f("id", "string", "The project's uuid."),
    f("name", "string", "The project's name — the value `project:` filters on and `task.add` takes."),
    n("description", "string", "The project's description, or null."),
    f("archived", "boolean", "Whether the project is archived. Archived projects are omitted unless `include_archived` asked for them."),
    f("default", "boolean", "Whether this is the project a bare `task.add` lands in."),
];

/// `project.use`'s result.
pub const R_PROJECT_USE: &[FieldDoc] = &[
    f("name", "string", "The project that is now the default."),
    f(
        "default",
        "boolean",
        "Always true on success — the call's whole effect, echoed.",
    ),
    n(
        "previous",
        "string",
        "The project that was the default before, or null if there was none.",
    ),
];

/// `project.archive`'s result.
pub const R_PROJECT_ARCHIVE: &[FieldDoc] = &[
    f("name", "string", "The project that was archived."),
    f("archived", "boolean", "Always true on success."),
    f(
        "default_cleared",
        "boolean",
        "Whether archiving it also cleared the store's default project (D22).",
    ),
    f(
        "open_tasks",
        "integer",
        "How much open work the archive left behind, counted in the same transaction (D89).",
    ),
    f(
        "open_overdue",
        "integer",
        "How many of those open tasks are past their due date.",
    ),
];

/// `task.add`'s result.
pub const R_TASK_ADD: &[FieldDoc] = &[
    f("id", "string", "The new task's uuid — stable, and what an export and an import agree on."),
    f("short_id", "integer", "The task's short id — the small number every `ref` accepts and the CLI prints."),
    f("status", "string", "The status the task landed in: `pending`, or `backlog` when a future `wait` parks it."),
    n("project", "string", "The project the task actually landed in — for an inherited default this is the only place the caller learns it."),
    f("urgency", "number", "The urgency score the fixed formula gives it (D1)."),
    n("recurrence", "string", "The recurrence rule as stored, or null."),
    f("title", "string", "The title as STORED — the CLI's inline sugar can rewrite what you typed, so this is what to verify against."),
    n("due", "string", "The due date resolved to an instant: `due: \"friday\"` comes back as its ISO timestamp."),
    f("tags", "array", "The tags as stored, lowercased and deduplicated."),
    n("scheduled", "string", "The scheduled date resolved to an instant, the same way `due` is."),
    o("warnings", "array", "Present only when the title carries CLI inline sugar (`+tag`, `due:`, `!prio`…), which this door stores verbatim and never parses: one line naming those words."),
];

/// `task.list`'s result.
pub const R_TASK_LIST: &[FieldDoc] = &[
    f("count", "integer", "How many rows came back."),
    f("total", "integer", "How many tasks matched the filter, before `limit` truncated."),
    n("next_offset", "integer", "The `offset` that fetches the next page; null once nothing is left."),
    f("store_empty", "boolean", "Whether the store has ever held a task at all — how \"nothing yet\" is told apart from \"nothing matched\"."),
    f("tasks", "array", "The matching tasks, one row each."),
];

/// The canonical task object — the shape every task-shaped surface starts from.
pub const TASK_CORE: &[FieldDoc] = &[
    f("id", "string", "The task's uuid, stable across exports and machines."),
    f("short_id", "integer", "The task's short id — the small number every `ref` accepts and the CLI prints."),
    f("title", "string", "The task's title, verbatim."),
    f("status", "string", "`pending`, `backlog`, `active`, `done` or `cancelled` — or, on a stored row whose status text this build does not recognize, that text verbatim, with `status_unrecognized: true` beside it."),
    n("priority", "string", "`H`, `M`, `L`, or null for none."),
    n("project", "string", "The project the task belongs to, or null."),
    n("due", "string", "When it is due, as an instant, or null."),
    n("scheduled", "string", "When work is planned to start, or null. While it is in the future a `pending` task reads as `backlog`, out of the working set, as with `wait`; `active`, `done` and `cancelled` keep their status."),
    n("wait", "string", "Holds the task out of the working set until this instant, or null. While it is in the future a `pending` task reads as `backlog`; `active`, `done` and `cancelled` keep their status."),
    n("estimate", "string", "The estimate as an ISO 8601 duration (`PT4H`), or null."),
    n("recurrence", "string", "The recurrence rule (`every 3 days`), or null."),
    n("remind", "string", "The reminder spec — an offset from `due` or an absolute instant — or null."),
    f("urgency", "number", "The urgency score (D1): priority, due proximity and age, summed."),
    f("tags", "array", "Every tag on the task, as plain strings."),
    f("created", "string", "When the task was captured."),
    f("modified", "string", "When it last changed."),
    n("completed", "string", "When `task.done` completed it, or null. `task.reopen` clears it, and `task.cancel` does not set it."),
    n("budget_tokens", "integer", "The size gauge over FRESH tokens (D139), or null for no threshold. It stops nothing."),
    f("_rev", "integer", "The task's revision counter. The methods that change the task or its tags, notes, checks and dependencies bump it; `token.add`, `token.remove` and `reminder.fire` leave it alone. Send it back as `expected_rev` to make a change conditional."),
];

/// The live-read spelling of tracked time: an ISO duration plus the open interval's anchor.
pub const TASK_LIVE_TIME: &[FieldDoc] = &[
    f(
        "tracked",
        "string",
        "Time on the clock as an ISO duration, the interval still running included up to the moment of the read (D166). `active_since` marks where that one began.",
    ),
    f(
        "tracked_adjustment",
        "string",
        "The net of every `task.adjust_tracked` correction already inside `tracked`, as a signed ISO duration (`-PT2H25M`); `PT0S` when there is none.",
    ),
    n(
        "active_since",
        "string",
        "When the open interval started, or null when the timer is not running.",
    ),
];

/// Whether an unmet dependency is holding the task back.
pub const TASK_BLOCKED: &[FieldDoc] = &[f(
    "blocked",
    "boolean",
    "Whether an unmet dependency is holding this task back.",
)];

/// The flag a row carries when its stored `status` is text this build does not recognize.
pub const TASK_STATUS_FLAG: &[FieldDoc] = &[
    o("status_unrecognized", "boolean", "Present, and true, only on a row whose `status` no writer of this build could have produced — a store an older import wrote unvalidated, or a hand-edited one. `store.import` now refuses such a status."),
];

/// D139's derived pair: what was spent, and whether that is past the budget.
pub const TASK_BUDGET_GAUGE: &[FieldDoc] = &[
    f("fresh_tokens", "integer", "Fresh tokens spent on this task — input, output and cache creation, never cache reads (D139), plus any unsplit `total_tokens` in full (D167)."),
    n("over", "boolean", "Whether `fresh_tokens` is past `budget_tokens`; null when no budget was set, because there is no verdict to give."),
];

/// The acceptance criteria hanging off a task (D138).
pub const TASK_CHECKS: &[FieldDoc] = &[f(
    "checks",
    "array",
    "The acceptance criteria on the task, in position order (D138).",
)];

/// The forward dependency edge: what this task waits on. The same key on a
/// live read and on an export row.
pub const TASK_DEPENDS_ON: &[FieldDoc] = &[f(
    "depends_on",
    "array",
    "The short ids this task waits on.",
)];

/// `task.get`'s page of notes — its rows can carry D148's cut markers.
pub const TASK_ANNOTATIONS: &[FieldDoc] = &[f(
    "annotations",
    "array",
    "The notes on this task, oldest first within the page. Pages are counted back from the newest note, so `annotations_offset: 0` is the most recent page.",
)];

/// The reverse edge: what this task is holding back.
pub const TASK_BLOCKS: &[FieldDoc] = &[f(
    "blocks",
    "array",
    "The short ids this task is holding back — the reverse edge of `depends_on`.",
)];

/// D170: the occurrence a recurrence spawned this task from.
pub const TASK_SPAWNED_FROM: &[FieldDoc] = &[n(
    "spawned_from",
    "integer",
    "The short id of the task whose completion spawned this one (a recurrence, D170); null on any other task.",
)];

/// D170: the same link on an export row, by uuid and only when set.
pub const TASK_EXPORT_SPAWNED_FROM: &[FieldDoc] = &[o(
    "spawned_from",
    "string",
    "The uuid of the task whose completion spawned this one (D170). Omitted on any other task.",
)];

/// D170: what the previous occurrence of a recurring task delivered.
pub const BRIEF_LAST_TIME: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("title", "string", "The task's title, verbatim."),
    n("completed", "string", "When the previous occurrence was completed."),
    n(
        "delivered",
        "string",
        "The first paragraph of its newest note — the delivery note — or null when nobody wrote one.",
    ),
];

/// What `task.get` says about the history it did NOT return — paging, not a
/// property of the task or of any note.
pub const ANNOTATION_PAGING: &[FieldDoc] = &[
    f(
        "annotations_total",
        "integer",
        "How many notes the task has, whether or not this page carries them all.",
    ),
    f(
        "annotations_offset",
        "integer",
        "How many notes this page skipped.",
    ),
    n(
        "annotations_next_offset",
        "integer",
        "The offset of the next page of notes; null on the last one.",
    ),
];

/// The notes a task once carried and no longer does (D113).
pub const TASK_ANNOTATIONS_REMOVED: &[FieldDoc] = &[
    f("annotations_removed", "array", "One tombstone per scrubbed note (D113) — id and instant, never the text. Excluded from `annotations` and its total."),
];

/// D165: the notes the card quotes, read apart from the annotation page, and
/// the pin completion wrote.
pub const TASK_CARD_NOTES: &[FieldDoc] = &[
    n("delivered_annotation_id", "string", "The note `task.done` pinned as the delivery note — the newest one at the instant of completion — or null. `task.reopen` clears it (D165)."),
    n("first_annotation", "object", "The oldest note, whatever page `annotations_limit`/`annotations_offset` asked for — the card's Description — or null when there is none (D165)."),
    n("delivered_annotation", "object", "The pinned delivery note — on a closed task with no pin, the newest note — or null on an open task, or when the pinned note was removed (D165)."),
];

/// D165: on an export row, the pin is present only when there is one.
pub const TASK_EXPORT_PIN: &[FieldDoc] = &[
    o("delivered_annotation_id", "string", "The note `task.done` pinned as the delivery note (D165). Present only on a task a completion pinned one on."),
];

/// The measurements on a task read live — always present, empty when there are none.
pub const TASK_TOKENS: &[FieldDoc] = &[f(
    "tokens",
    "array",
    "Every token measurement banked against this task.",
)];

/// #150's optional breakdown, present only under `explain: true`.
pub const TASK_URGENCY_BREAKDOWN: &[FieldDoc] = &[o(
    "urgency_breakdown",
    "object",
    "The terms `urgency` sums — present only when `explain: true` asked for them.",
)];

/// The open prerequisites `blocked` is blocked on, named rather than counted.
pub const TASK_UNMET_BLOCKERS: &[FieldDoc] = &[f(
    "unmet_blockers",
    "array",
    "The open prerequisites, named — what `blocked: true` is blocked ON.",
)];

/// One acceptance criterion (D138) — a claim tasqx stores and never runs.
pub const TASK_CHECK: &[FieldDoc] = &[
    f("id", "string", "The criterion's uuid — what `check.set` and `check.remove` take."),
    f("body", "string", "The criterion itself, stored verbatim. tasqx never runs it; it is a claim, not a command."),
    f("state", "string", "`open`, `passed` or `failed`. A failure is a normal outcome, not an error."),
    n("evidence", "string", "The citation for the state, or null — some criteria are met by something nobody can quote."),
    f("position", "integer", "Where it sits in the list, from zero."),
    f("created", "string", "When the criterion was added."),
    f("modified", "string", "When its state or evidence last changed."),
];

/// One annotation.
pub const ANNOTATION_ROW: &[FieldDoc] = &[
    f(
        "id",
        "string",
        "The note's uuid — what `annotation.remove` takes.",
    ),
    f(
        "body",
        "string",
        "The note, stored verbatim (D41). `task.get` and `task.brief` given `max_body_bytes` return a prefix of a longer body and mark it with `body_truncated` and `body_bytes` (D148).",
    ),
    f("created", "string", "When the note was written."),
];

/// D148's cut markers, present only on a body this response truncated.
pub const ANNOTATION_CAP: &[FieldDoc] = &[
    o("body_bytes", "integer", "The body's real size in bytes. Present only on a body this response cut (D148)."),
    o("body_truncated", "boolean", "Present and true only on a body this response cut, so \"was this cut?\" stays a presence question."),
];

/// What `annotation.remove` left behind (D113): the id and the instant, never the text.
pub const TOMBSTONE_ROW: &[FieldDoc] = &[
    f("id", "string", "The scrubbed note's id."),
    f(
        "removed",
        "string",
        "When it was scrubbed. Never the text — that is the point of the method.",
    ),
];

/// One token measurement.
pub const MEASUREMENT_ROW: &[FieldDoc] = &[
    f("id", "string", "The measurement's uuid — what `token.remove` takes."),
    f("tool", "string", "Which tool spent the tokens (`claude-code`, `codex`, …)."),
    f("source", "string", "How the figure was obtained: `self-report`, `log-parse` or `otel`."),
    n("model", "string", "The model that did the work, or null."),
    f("input_tokens", "integer", "Fresh input tokens."),
    f("output_tokens", "integer", "Output tokens."),
    f("cache_read_tokens", "integer", "Tokens read from cache — never blended with the others (D48)."),
    f("cache_creation_tokens", "integer", "Tokens spent writing the cache."),
    f("confidence", "string", "How checkable the figure is: `high`, `medium` or `low`. A self-report may not claim `high`."),
    f("created", "string", "When the measurement was banked."),
    f("total_tokens", "integer", "One unsplit count, from a reporter that could not split it — 0 on a split measurement, and never folded into the four (D167)."),
];

/// The terms `urgency` sums (D1).
pub const URGENCY_BREAKDOWN_ROW: &[FieldDoc] = &[
    f("priority", "number", "The priority term."),
    f("due_proximity", "number", "The due-date term."),
    f("age", "number", "The age term."),
    f("total", "number", "The sum — the same number as the row's `urgency`, so nobody has to recompute the rounding."),
];

/// One blocking task: the short id and the title, the vocabulary `task.get` and `task.done` share.
pub const BLOCKER_ROW: &[FieldDoc] = &[
    f("short_id", "integer", "The blocking task's short id."),
    f(
        "title",
        "string",
        "The blocking task's title, so a refusal can be read without a second call.",
    ),
];

/// `task.brief`'s result.
pub const R_TASK_BRIEF: &[FieldDoc] = &[
    f(
        "task",
        "object",
        "`task.get`'s own result for this task, without `urgency_breakdown` — the brief does not take `explain`.",
    ),
    f(
        "neighbourhood",
        "object",
        "What this task waits on and what it releases.",
    ),
    f(
        "memory",
        "object",
        "A `memory.search` result under an expression derived from the task's own words.",
    ),
    o(
        "last_time",
        "object",
        "Present only on a recurrence spawn: the previous occurrence and what it delivered (D170).",
    ),
];

/// D136's neighbourhood: what the task waits on, and what it releases.
pub const BRIEF_NEIGHBOURHOOD: &[FieldDoc] = &[
    f(
        "depends_on",
        "array",
        "Each prerequisite, with its newest annotation — what that task concluded.",
    ),
    f(
        "blocks",
        "array",
        "What this task releases when it completes, title and status only.",
    ),
];

/// One prerequisite, with what it concluded.
pub const BRIEF_PREREQUISITE: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("title", "string", "The task's title, verbatim."),
    f("status", "string", "The prerequisite's status."),
    n(
        "annotation",
        "object",
        "The prerequisite's newest note, or null when nobody wrote one.",
    ),
];

/// One dependent, title and status only.
pub const BRIEF_DEPENDENT: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("title", "string", "The task's title, verbatim."),
    f("status", "string", "The dependent's status."),
];

/// The brief's memory half: a search result plus what D147's reservation did.
pub const BRIEF_MEMORY: &[FieldDoc] = &[
    f("count", "integer", "How many hits came back."),
    f("total", "integer", "How many rows matched before the page was cut."),
    f("has_more", "boolean", "Whether anything was left behind."),
    f("hits", "array", "The hits themselves — the same rows `memory.search` returns."),
    n("matched", "string", "The FTS expression tasqx derived from the task; null when the task had no searchable word."),
    n("project", "string", "The project the search was scoped to, or null."),
    f("reserved_docs", "integer", "How many of the page's slots were held for knowledge docs (D147)."),
    f("docs_total", "integer", "How many docs matched."),
    f("annotations_total", "integer", "How many annotations matched."),
];

/// `task.start`'s result.
pub const R_TASK_START: &[FieldDoc] = &[
    f("id", "string", "The task's uuid."),
    f("status", "string", "`active` once the clock is running."),
    n("interval_started", "string", "When the running interval opened. On the idempotent re-start of a task already running, that interval's existing start. Null only if an active row has lost its start, which no tasqx write produces."),
    f("short_id", "integer", "The task's short id — the small number every `ref` accepts and the CLI prints."),
    f("title", "string", "The task's title, verbatim."),
    f("already_running", "boolean", "True when the call opened no new interval because the timer was already on."),
    f("auto_stopped", "array", "Whatever D6's single-active rule stopped to make room. Empty unless `keep` was omitted and something else was running."),
];

/// One task D6's single-active rule stopped to make room.
pub const AUTO_STOPPED_ROW: &[FieldDoc] = &[
    f("id", "string", "The stopped task's uuid."),
    f("short_id", "integer", "The stopped task's short id."),
    f(
        "tracked",
        "string",
        "That task's running total after the stop.",
    ),
];

/// `task.stop`'s result.
pub const R_TASK_STOP: &[FieldDoc] = &[
    f("status", "string", "The status the task fell back to."),
    f(
        "interval",
        "string",
        "The interval just closed, as an ISO duration.",
    ),
    f(
        "tracked",
        "string",
        "The running total after the stop — the same word `task.get` uses.",
    ),
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("title", "string", "The task's title, verbatim."),
];

/// `task.adjust_tracked`'s result (D166).
pub const R_TASK_ADJUST_TRACKED: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("title", "string", "The task's title, verbatim."),
    f(
        "delta",
        "string",
        "The correction this call applied, as a signed ISO duration.",
    ),
    f(
        "tracked",
        "string",
        "The total after it — the same word `task.get` uses.",
    ),
    f(
        "tracked_adjustment",
        "string",
        "The net of every correction on the task, this one included.",
    ),
    f(
        "_rev",
        "integer",
        "The task's revision after the correction.",
    ),
];

/// `task.done`'s result.
pub const R_TASK_DONE: &[FieldDoc] = &[
    f(
        "status",
        "string",
        "`done` — the status the task is now in.",
    ),
    f(
        "completed",
        "string",
        "The instant recorded as the completion.",
    ),
    f(
        "unblocked",
        "array",
        "The short ids this completion released.",
    ),
    o(
        "spawned",
        "object",
        "The next instance, present only when the completed task carried a recurrence rule (D2).",
    ),
    o(
        "tokens_hint",
        "string",
        "Present only when the completion self-reported no token counts — the nudge, not an error.",
    ),
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("title", "string", "The task's title, verbatim."),
    f("tracked", "string", "Time on the clock when it closed."),
    n(
        "estimate",
        "string",
        "What it was estimated at, or null — beside `tracked`, the calibration in one line.",
    ),
    o(
        "forced",
        "boolean",
        "Present and true only on a completion that overrode still-open blockers (D150).",
    ),
    o(
        "blocked_by",
        "array",
        "The blockers that override left open. Present only beside `forced`.",
    ),
];

/// `task.modify`'s result.
pub const R_TASK_MODIFY: &[FieldDoc] = &[
    f("short_id", "integer", "The task's short id — the small number every `ref` accepts and the CLI prints."),
    f("_rev", "integer", "The task's revision counter. The methods that change the task or its tags, notes, checks and dependencies bump it; `token.add`, `token.remove` and `reminder.fire` leave it alone. Send it back as `expected_rev` to make a change conditional."),
    f("set", "object", "The RESOLVED value stored for each field this call named — `due: \"friday\"` comes back as its instant."),
    o("warnings", "array", "Present only when `set.title` carries CLI inline sugar, stored verbatim: one line naming those words, as on `task.add`."),
];

/// `task.cancel`'s result.
pub const R_TASK_CANCEL: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("status", "string", "`cancelled`."),
    f(
        "unblocked",
        "array",
        "The short ids the cancellation released — a cancelled blocker stops blocking (D114).",
    ),
];

/// `task.reopen`'s result.
pub const R_TASK_REOPEN: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("status", "string", "The status the task went back to."),
    f(
        "blocked",
        "array",
        "The open dependents this reopen put back into `blocked` — the mirror of `unblocked`.",
    ),
];

/// `tag.add`'s result.
pub const R_TAG_ADD: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("tags", "array", "Every tag on the task after the call."),
];

/// `tag.remove`'s result.
pub const R_TAG_REMOVE: &[FieldDoc] = &[
    f("short_id", "integer", "The task's short id — the small number every `ref` accepts and the CLI prints."),
    f("tags", "array", "The tags left on the task."),
    f("removed", "array", "The tags this call took off. A tag the task does not have is `not_found` and removes nothing."),
];

/// `annotation.add`'s result.
pub const R_ANNOTATION_ADD: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "annotation",
        "object",
        "The note as stored, echoed back so a caller can see its bytes survived.",
    ),
];

/// `annotation.remove`'s result.
pub const R_ANNOTATION_REMOVE: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "removed",
        "object",
        "The tombstone: the id and the instant, never the text (D113).",
    ),
];

/// `annotation.update`'s result.
pub const R_ANNOTATION_UPDATE: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "annotation",
        "object",
        "The note as it now stands: the same id and `created`, the new body (D165).",
    ),
    f("_rev", "integer", "The task's revision counter after the edit. Send it back as `expected_rev` to make the next change conditional."),
];

/// `check.add`'s result.
pub const R_CHECK_ADD: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "check",
        "object",
        "The criterion as stored, appended at the end and starting `open`.",
    ),
];

/// `check.set`'s result.
pub const R_CHECK_SET: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("check_id", "string", "The criterion that moved."),
    f("state", "string", "Its new state."),
];

/// `check.remove`'s result.
pub const R_CHECK_REMOVE: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f("check_id", "string", "The criterion that was dropped."),
    f(
        "removed",
        "boolean",
        "Always true on success; an unknown id is `not_found` instead.",
    ),
];

/// `token.add`'s result.
pub const R_TOKEN_ADD: &[FieldDoc] = &[
    f("short_id", "integer", "The task's short id — the small number every `ref` accepts and the CLI prints."),
    f("measurement", "object", "The measurement as banked. A repeated `idempotency_key` returns the one already stored, unchanged."),
];

/// `token.remove`'s result.
pub const R_TOKEN_REMOVE: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "removed",
        "object",
        "The measurement that is now gone — the same object `token.add` hands back (#210).",
    ),
];

/// `tokens.recompute`'s result.
pub const R_TOKENS_RECOMPUTE: &[FieldDoc] = &[
    f(
        "dry_run",
        "boolean",
        "Whether this run wrote anything. Defaults to true: report the delta, change nothing.",
    ),
    f(
        "tasks",
        "array",
        "One row per task whose attribution would change.",
    ),
    f(
        "totals",
        "object",
        "The whole-store totals, before and after.",
    ),
];

/// One task the attribution pass would change.
pub const RECOMPUTE_ROW: &[FieldDoc] = &[
    f("task", "integer", "The task's short id."),
    f(
        "action",
        "string",
        "What the pass would do to it: `insert`, `update` or `delete`.",
    ),
    f("before", "object", "The four buckets as they stand."),
    f("after", "object", "The four buckets the pass computed."),
];

/// The four token buckets, never blended (D48).
pub const TOKEN_BUCKETS_ROW: &[FieldDoc] = &[
    f("input_tokens", "integer", "Fresh input tokens."),
    f("output_tokens", "integer", "Output tokens."),
    f("cache_read_tokens", "integer", "Tokens read from cache."),
    f(
        "cache_creation_tokens",
        "integer",
        "Tokens spent writing the cache.",
    ),
];

/// The whole-store token total, before and after the pass.
pub const RECOMPUTE_TOTALS: &[FieldDoc] = &[
    f(
        "before",
        "integer",
        "Total attributed tokens before the pass.",
    ),
    f("after", "integer", "Total attributed tokens after it."),
];

/// `dependency.add`'s result.
pub const R_DEPENDENCY_ADD: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "depends_on",
        "array",
        "Every short id this task now waits on.",
    ),
    f(
        "blocked",
        "boolean",
        "Whether the task is blocked after the call.",
    ),
    f(
        "inserted",
        "boolean",
        "False when the edge was already there — a re-run told apart from a new edge.",
    ),
];

/// `dependency.remove`'s result.
pub const R_DEPENDENCY_REMOVE: &[FieldDoc] = &[
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "depends_on",
        "array",
        "Every short id this task still waits on.",
    ),
    f(
        "blocked",
        "boolean",
        "Whether the task is still blocked — whether it is actionable now.",
    ),
];

/// One explicit link, as `link.list` pages it (D160).
///
/// The endpoints are ids and not objects: a link spans four kinds of node, and
/// embedding each end's row would make one list answer with four shapes.
/// `graph.query` is where a node's label and summary come from.
pub const LINK_ROW: &[FieldDoc] = &[
    f("id", "string", "The link's uuid — what `link.remove` takes."),
    f("from", "string", "The node the link starts at, as `<type>:<uuid>`: `task:`, `memory:`, `annotation:` or `project:`."),
    f("to", "string", "The node the link points at, spelled the same way."),
    f("relation", "string", "What the link asserts: `references`, `supersedes`, `implements_decision`, `derived_from` or `contradicts`."),
    n("metadata", "object", "The caller's own object, stored verbatim and never interpreted, or null."),
    f("created_at", "string", "When the link was written. Named `created_at` because `link.add` answers with a boolean `created` beside it."),
];

/// `link.add`'s result: the link, plus whether this call is what created it.
pub const R_LINK_ADD: &[FieldDoc] = &[
    f("id", "string", "The link's uuid. On a duplicate this is the id the first call minted, not a new one."),
    f("from", "string", "The node the link starts at, as `<type>:<uuid>` — the resolved form of whatever reference was sent."),
    f("to", "string", "The node the link points at, spelled the same way."),
    f("relation", "string", "The relation as stored, one of the five D160 fixes."),
    n("metadata", "object", "The metadata this link carries, or null. On a duplicate it is what the FIRST call stored — a repeat never overwrites."),
    f("created_at", "string", "When the link was written."),
    f("created", "boolean", "False when the link was already there, so a re-run is told apart from a new edge. Adding the same from/to/relation twice is idempotent, never an error."),
];

/// `link.remove`'s result.
pub const R_LINK_REMOVE: &[FieldDoc] = &[
    f("id", "string", "The link that is gone."),
    f(
        "removed",
        "boolean",
        "Always true on success. Removing a link that is not there is `not_found`, not a no-op.",
    ),
];

/// `link.list`'s result — the same paging shape as `task.list` (D70).
pub const R_LINK_LIST: &[FieldDoc] = &[
    f("count", "integer", "How many links came back in this page."),
    f("total", "integer", "How many matched before `limit` and `offset` cut the page."),
    n("next_offset", "integer", "The offset that keeps walking, or null once nothing is left."),
    f("links", "array", "The links themselves, newest first. A `ref` matches EITHER endpoint, so a node's neighbourhood is one call."),
];

/// One node of a `graph.query` projection (D160).
///
/// `short_id` and `task` are null on the kinds of node that do not have one
/// rather than absent: a key that appears and disappears by node type makes a
/// renderer branch on presence four ways to lay out one table, and "this node
/// has no short id" is exactly what null says.
pub const GRAPH_NODE: &[FieldDoc] = &[
    f("id", "string", "The node's stable id, `<type>:<uuid>` — the same spelling a link's endpoints use, and what a follow-up `graph.query` takes as its `root`."),
    f("type", "string", "Which kind of node: `task`, `memory`, `annotation` or `project`."),
    f("label", "string", "The one line to draw in the box: a task's title, a document's title, a note's first line (cut to 80 characters) or a project's name."),
    n("summary", "string", "The second line: `#<short_id> · <status> · <priority>` for a task, the source for a document, the owning task for a note, the description for a project. Null when a project has no description."),
    n("project", "string", "The project this node is filed under, or null — including on a project node, which is not filed under itself."),
    n("status", "string", "A task's status, read the way every other surface reads it (a future `wait` shows `backlog`). Null on every other kind of node."),
    n("modified", "string", "When the node last changed: `modified` for a task or a document, `created` for a note, which is never edited. Null for a project, which carries no such column — so the date window cannot exclude one."),
    n("short_id", "integer", "The small number a human types for a task. Null on every other kind of node."),
    n("task", "string", "The node id of the task a note is written on. Null on every other kind of node."),
];

/// One edge of a projection.
pub const GRAPH_EDGE: &[FieldDoc] = &[
    f("id", "string", "The edge's id, stable for the same edge across calls: `dep:`, `ann:`, `member:` or `link:` for a stored one, `search:` or `tag:` for a computed one."),
    f("from", "string", "The node id the edge starts at. A `depends_on` edge always points from the task that waits to the one it waits on, whichever end the walk reached it from."),
    f("to", "string", "The node id the edge points at."),
    f("relation", "string", "What the edge asserts: `depends_on`, `has_annotation`, `belongs_to_project`, one of the five link relations, or `search_match`/`shared_tag` when `include_inferred` asked for them."),
    f("kind", "string", "`structural` for an edge read out of a table, `inferred` for one computed for this call and never stored."),
    n("confidence", "number", "How strong a computed edge is, 0 to 1 within this answer. Null on a structural edge: a stored fact has no confidence to report."),
    f("source", "string", "Where the edge came from: the table (`dependencies`, `annotations`, `tasks.project`, `docs.project`, `links`) or what computed it (`memory.search: <words>`, `tags: <shared>`)."),
];

/// `graph.query`'s result.
pub const R_GRAPH_QUERY: &[FieldDoc] = &[
    f("root", "string", "The node the projection is anchored at, resolved to its stable `<type>:<uuid>` id."),
    f("depth", "integer", "How many hops were walked — the `depth` that was asked for, or 2."),
    f("nodes", "array", "The nodes, ordered by depth, then kind, then id. Deterministic: the same call twice is the same list."),
    f("edges", "array", "The edges, ordered by relation, then `from`, then `to`. Only edges with both endpoints among the nodes above."),
    f("node_count", "integer", "How many nodes came back — the length of `nodes`, always present so a caller need not count."),
    f("edge_count", "integer", "How many edges came back."),
    f("truncated", "boolean", "Whether a cap bit, i.e. whether either omitted figure below is non-zero. A client that cannot tell a graph that ends from one that was cut draws the wrong picture."),
    f("omitted_nodes", "integer", "How many nodes the walk found and `max_nodes` cut."),
    f("omitted_edges", "integer", "How many edges `max_edges` cut, after the ones whose endpoints were cut had already gone."),
    f("include_inferred", "boolean", "Whether the computed edges were asked for, echoed so a cached answer says which kind of graph it is."),
];

/// `memory.add`'s result.
pub const R_MEMORY_ADD: &[FieldDoc] = &[
    f(
        "id",
        "string",
        "The new doc's uuid — what a search hit carries and `memory.get` takes.",
    ),
    f("title", "string", "The doc's title, as stored."),
    n(
        "project",
        "string",
        "The project the doc is scoped to, or null for a global one (#134).",
    ),
    f(
        "standing",
        "boolean",
        "Whether the doc was stored as a standing ruling — one meant for every session of its scope (D156).",
    ),
    f("created", "string", "When it was stored."),
    o(
        "hint",
        "string",
        "Present only when this add pushed its scope past the soft cap on standing docs: a nudge to merge or retract one. Test for the key, not its value (D156).",
    ),
];

/// `memory.search`'s result.
pub const R_MEMORY_SEARCH: &[FieldDoc] = &[
    f("count", "integer", "How many hits came back."),
    f("total", "integer", "How many rows matched before `limit` truncated (#132)."),
    f("has_more", "boolean", "Whether anything was left behind."),
    f("hits", "array", "The hits, bm25-ranked, docs and annotations together."),
    f("matched", "string", "The FTS5 expression actually run — how `count: 0` is told apart from a store holding nothing on the subject."),
];

/// One search hit — a doc or an annotation, with its snippet and rank.
pub const MEMORY_HIT_ROW: &[FieldDoc] = &[
    f("id", "string", "The doc's id, or the annotation's."),
    f("kind", "string", "`doc` or `annotation` — which store the hit came from."),
    f("title", "string", "The doc's title; for an annotation, the task it is written on."),
    n("source", "string", "Where the doc came from (a file path, `task:#51`), or null."),
    f("snippet", "string", "The matching passage, with the hit in context. Never the whole body."),
    f("rank", "number", "The bm25 score. Lower is a better match; the number itself is not comparable between searches."),
    n("standing", "boolean", "For a doc hit, whether it is a standing ruling (D156); null on an annotation hit, which has no such flag."),
    n("project", "string", "Which project this hit is scoped to — a doc's own column, or the annotation's task's — or null for global knowledge (#657)."),
];

/// One knowledge doc, whole. The same row `memory.get` and `store.export` both answer with.
pub const DOC_EXPORT_ROW: &[FieldDoc] = &[
    f("id", "string", "The doc's uuid."),
    n("source", "string", "Where it came from — the path `memory.import` keyed on, or null."),
    f("title", "string", "The doc's title."),
    f("body", "string", "The whole body, stored and returned verbatim (D41)."),
    f("created", "string", "When it was first stored."),
    f("modified", "string", "When it last changed."),
    n("project", "string", "The project it is scoped to, or null."),
    f("_rev", "integer", "The row's revision counter, bumped by every write. Send it back as `expected_rev` to make a change conditional."),
    f("standing", "boolean", "Whether it is a standing ruling, meant for every session of its scope (D156). Stated on every doc, so a restore carries it."),
    n("origin_path", "string", "The absolute path of the file `memory.import` read this doc from, or null when no import set one (D180)."),
    n("origin_mtime", "integer", "That file's modification time in unix seconds when it was read, or null (D180)."),
    n("origin_size", "integer", "That file's size in bytes when it was read, or null (D180)."),
];

/// `memory.remove`'s result.
pub const R_MEMORY_REMOVE: &[FieldDoc] = &[
    f("id", "string", "The doc that is gone."),
    f(
        "removed",
        "boolean",
        "Always true on success. Removal is permanent and outside `undo`.",
    ),
];

/// `memory.import`'s result.
pub const R_MEMORY_IMPORT: &[FieldDoc] = &[
    f("imported", "integer", "How many docs the batch stored."),
    f(
        "replaced",
        "integer",
        "How many of those replaced a doc with the same `source`, in place.",
    ),
    f("docs", "array", "One row per document in the batch."),
];

/// One document of an import batch, and whether it replaced one in place.
pub const IMPORTED_DOC_ROW: &[FieldDoc] = &[
    f("id", "string", "The doc's id — the one it already had when it was replaced in place."),
    f("title", "string", "Its title."),
    n("source", "string", "The source it was keyed on."),
    n("project", "string", "The batch's `project`, echoed per doc — null when the import named none (#657)."),
    f("replaced", "boolean", "Whether this row replaced an existing doc rather than creating one (D143)."),
    f("_rev", "integer", "The row's revision counter, bumped by every write. Send it back as `expected_rev` to make a change conditional."),
];

/// `memory.refresh`'s result (#789).
pub const R_MEMORY_REFRESH: &[FieldDoc] = &[
    f(
        "checked",
        "integer",
        "How many docs named an origin file and were examined. `refreshed`, `missing` and `unchanged` account for all of them.",
    ),
    f(
        "refreshed",
        "array",
        "The docs whose file had changed and were re-read from it.",
    ),
    f(
        "missing",
        "array",
        "The docs whose file could not be read — gone, renamed, or on a volume that is not mounted. Reported, never removed.",
    ),
    f(
        "unchanged",
        "integer",
        "How many docs still matched their file, byte for byte, and were left alone — their `rev` did not move.",
    ),
];

/// One doc a refresh re-read from its file.
pub const REFRESHED_DOC_ROW: &[FieldDoc] = &[
    f(
        "id",
        "string",
        "The doc's id, unchanged — a refresh replaces the text in place.",
    ),
    n(
        "source",
        "string",
        "The source it is keyed on, or null for a doc that has an origin file and no source.",
    ),
];

/// One doc whose origin file a refresh could not read.
pub const MISSING_DOC_ROW: &[FieldDoc] = &[
    f(
        "id",
        "string",
        "The doc, still stored with the text it already had.",
    ),
    n("source", "string", "The source it is keyed on, or null."),
    f(
        "origin_path",
        "string",
        "The file that could not be read, as the import recorded it (D180).",
    ),
];

/// `memory.list`'s result.
pub const R_MEMORY_LIST: &[FieldDoc] = &[
    f("count", "integer", "How many docs came back."),
    f("total", "integer", "How many docs the scope holds."),
    n(
        "next_offset",
        "integer",
        "The `offset` that fetches the next page; null once nothing is left.",
    ),
    f("docs", "array", "The docs, newest-modified first."),
];

/// One row of the doc browser: recency metadata and a preview, not the body (#133).
pub const MEMORY_LIST_ROW: &[FieldDoc] = &[
    f("id", "string", "The doc's uuid."),
    f("title", "string", "Its title."),
    n("source", "string", "Where it came from, or null."),
    n("project", "string", "The project it is scoped to, or null."),
    f("created", "string", "When it was stored."),
    f("modified", "string", "When it last changed — the column this list sorts by."),
    f("_rev", "integer", "The row's revision counter, bumped by every write. Send it back as `expected_rev` to make a change conditional."),
    f("body_preview", "string", "The opening of the body, or all of it when the body is short (`body_truncated` says which) — browsing pays for a preview, not a payload."),
    f("body_truncated", "boolean", "Whether the preview is shorter than the body."),
    f("standing", "boolean", "Whether it is a standing ruling (D156) — on every row, not only on a page filtered by `standing`."),
];

/// `memory.update`'s result.
pub const R_MEMORY_UPDATE: &[FieldDoc] = &[
    f("id", "string", "The doc that changed."),
    f("title", "string", "Its title after the update."),
    n("source", "string", "Its source after the update, or null."),
    n("project", "string", "Its project after the update, or null."),
    f("standing", "boolean", "The standing flag as it now stands, whether this call set it, cleared it or left it alone (D156)."),
    f("_rev", "integer", "The row's revision counter, bumped by every write. Send it back as `expected_rev` to make a change conditional."),
    f("modified", "string", "When the update landed."),
];

/// `report.summary`'s result.
pub const R_REPORT_SUMMARY: &[FieldDoc] = &[
    f("groups", "array", "One row per group, on the axis `group_by` named."),
    f("generated", "string", "When the report was computed — the time of the call, not the window."),
    f("filter", "string", "The filter this total was taken over, echoed so it cannot be read against the wrong scope (D69)."),
    f("all", "boolean", "Whether cancelled work was counted (D24)."),
    f("tokens_excluded_cancelled_tasks", "integer", "How many cancelled tasks carrying token spend the default just excluded."),
    n("since", "string", "The start of the time window, or null when the caller set none (D97)."),
    n("until", "string", "The end of that window, or null."),
    f("store_empty", "boolean", "Whether the store has ever held a task at all — how \"nothing yet\" is told apart from \"nothing matched\"."),
];

/// One group of `report.summary`. The key column is NAMED by `group_by`.
pub const SUMMARY_GROUP_ROW: &[FieldDoc] = &[
    f(
        "project",
        "string",
        "The group key — named by `group_by`, so this column is `project`, `status` or `priority`.",
    ),
    f("count", "integer", "How many tasks fell in this group."),
    f(
        "est_total",
        "string",
        "Their estimates summed, as an ISO duration. Only when `metrics` names it — the default is `count` alone.",
    ),
    f("overdue", "integer", "How many of them are past due. Only when `metrics` names it."),
];

/// `report.outcomes`'s result.
pub const R_REPORT_OUTCOMES: &[FieldDoc] = &[
    f("groups", "array", "One row per group, each metric beside the `n` it was computed over."),
    f("group_by", "string", "The axis the rows are grouped on, echoed."),
    f("metrics", "array", "The metrics computed. Omitting `metrics` on the request emits all of them."),
    f("generated", "string", "When the report was computed."),
    f("filter", "string", "The filter applied, echoed."),
    n("since", "string", "The start of the window over when tasks CLOSED, or null."),
    n("until", "string", "The end of that window, or null."),
    f("store_empty", "boolean", "Whether the store has ever held a task at all — how \"nothing yet\" is told apart from \"nothing matched\"."),
];

/// One group of `report.outcomes`, each metric beside the `n` it was computed over.
pub const OUTCOME_GROUP_ROW: &[FieldDoc] = &[
    f("project", "string", "The group key, named by `group_by`."),
    f(
        "completions",
        "integer",
        "How many tasks completed in the window.",
    ),
    f(
        "closed",
        "integer",
        "How many closed — completions plus cancellations.",
    ),
    f("rework", "object", "Completions that were later reopened."),
    f(
        "calibration",
        "object",
        "Tracked time over estimate, as a median.",
    ),
    f("cost", "object", "The four token buckets, never blended."),
    f(
        "silent",
        "object",
        "Completions carrying no annotation — work that left no record of what it decided.",
    ),
    f(
        "abandonment",
        "object",
        "Work that was started and then cancelled, with the time inside it.",
    ),
    f(
        "overrun",
        "object",
        "Completions that went past their `budget_tokens` (D139).",
    ),
    f(
        "unproven",
        "object",
        "Completions whose acceptance criteria were not all passed (D138).",
    ),
    f(
        "forced",
        "object",
        "Completions that overrode still-open blockers (D150).",
    ),
];

/// A rate and its denominator (D137). Never one without the other.
pub const OUTCOME_RATE: &[FieldDoc] = &[
    f("count", "integer", "How many occurrences."),
    f(
        "n",
        "integer",
        "The denominator this rate was computed over. A rate never travels without it.",
    ),
    n(
        "rate",
        "number",
        "`count` over `n`; null rather than zero when there was nothing to divide by.",
    ),
    f(
        "refs",
        "array",
        "The short ids behind the count, so a number can be opened.",
    ),
];

/// Tracked time over estimate, as a median.
pub const OUTCOME_CALIBRATION: &[FieldDoc] = &[
    n(
        "median_ratio",
        "number",
        "Median tracked-over-estimate. Null when nothing in the group carried both.",
    ),
    f(
        "n",
        "integer",
        "The denominator this rate was computed over. A rate never travels without it.",
    ),
];

/// The four buckets, plus how checkable they are.
pub const OUTCOME_COST: &[FieldDoc] = &[
    f("tokens_in", "integer", "Fresh input tokens across the group."),
    f("tokens_out", "integer", "Output tokens."),
    f("tokens_cache_read", "integer", "Cache reads."),
    f("tokens_cache_creation", "integer", "Cache writes."),
    f("tokens_unsplit", "integer", "Unsplit `total_tokens` counts, summed apart from the four (D167)."),
    f("n", "integer", "The denominator this rate was computed over. A rate never travels without it."),
    o("confidence", "string", "How checkable the figures are. Absent — not null — when nothing was measured, because there is nothing to grade."),
];

/// The rate shape plus the time inside the abandoned work.
pub const OUTCOME_ABANDONMENT: &[FieldDoc] = &[
    f(
        "count",
        "integer",
        "How many started tasks were then cancelled.",
    ),
    f(
        "n",
        "integer",
        "The denominator this rate was computed over. A rate never travels without it.",
    ),
    n("rate", "number", "`count` over `n`, or null."),
    f(
        "tracked_total",
        "string",
        "The time spent inside the abandoned work — how much, not only how many.",
    ),
    f("refs", "array", "The short ids behind the count."),
];

/// `store.export`'s result.
pub const R_STORE_EXPORT: &[FieldDoc] = &[
    f(
        "tasks",
        "array",
        "Every task in scope, whole: fields, tags, notes, criteria, edges and measurements.",
    ),
    f(
        "dropped_dependencies",
        "integer",
        "How many dependency edges pointed outside the filtered slice and were dropped.",
    ),
    f(
        "projects",
        "array",
        "Every project this document needs: all of them on an unfiltered export, or the ones named by the filter or by an exported task on a filtered one.",
    ),
    f(
        "dropped_projects",
        "integer",
        "How many project rows a filtered export left out because nothing exported named them.",
    ),
    f(
        "docs",
        "array",
        "Every memory doc this document needs, scoped like `projects`; an unscoped doc ships only with `include_unscoped`.",
    ),
    f(
        "dropped_docs",
        "integer",
        "How many memory docs a filtered export left out, scoped or unscoped.",
    ),
    f(
        "events",
        "array",
        "The whole audit log for what this document carries, minus the bookkeeping rows an import itself writes.",
    ),
    f(
        "dropped_events",
        "integer",
        "How many events a filtered export left out because they belong to a task, doc or project this document does not carry.",
    ),
    n(
        "default_project",
        "string",
        "The store's default project, or null — also null when a filtered export did not carry it among `projects`.",
    ),
];

/// The restore spelling of tracked time (D42): raw seconds, both keys omitted when they would be zero.
pub const TASK_EXPORT_TIME: &[FieldDoc] = &[
    o(
        "tracked_seconds",
        "integer",
        "Tracked time in raw seconds (D42). Omitted when it would be zero.",
    ),
    o(
        "tracked_adjustment_seconds",
        "integer",
        "The signed net of the `task.adjust_tracked` corrections inside `tracked_seconds` (D166). Omitted when it would be zero.",
    ),
    o(
        "active_since",
        "string",
        "The open interval's anchor, omitted when no timer is running.",
    ),
];

/// The same measurements on an export row, where the key is omitted rather than empty.
pub const TASK_EXPORT_TOKENS: &[FieldDoc] = &[o(
    "tokens",
    "array",
    "Every token measurement against this task. Omitted entirely for a task that has none.",
)];

/// `store.export`'s notes, whose rows are never cut or paged.
pub const TASK_EXPORT_ANNOTATIONS: &[FieldDoc] = &[f(
    "annotations",
    "array",
    "Every note on the task — an export carries the whole history, unpaged.",
)];

/// The exported project record (D37).
pub const PROJECT_EXPORT_ROW: &[FieldDoc] = &[
    f("id", "string", "The project's uuid."),
    f(
        "name",
        "string",
        "Its name — what a task's `project` names.",
    ),
    n("description", "string", "Its description, or null."),
    f("archived", "boolean", "Whether it is archived."),
    f("created", "string", "When it was created."),
];

/// One row of the append-only audit log.
pub const EVENT_ROW: &[FieldDoc] = &[
    f("id", "string", "The event's uuid."),
    f("entity", "string", "What kind of thing it happened to: `task`, `project`, `doc` or `link`."),
    f("entity_id", "string", "That thing's uuid."),
    f("op", "string", "What happened: `add`, `done`, `start`, `stop`, `tag.remove`, …"),
    n("payload", "object", "The op's own vocabulary. Null only for a row whose payload will not parse — a corrupt store."),
    f("ts", "string", "When it happened."),
    n("actor", "string", "Who did it, when the caller said, or null."),
];

/// `store.import`'s result.
pub const R_STORE_IMPORT: &[FieldDoc] = &[
    f("imported", "integer", "How many tasks the document brought in."),
    f("projects_imported", "integer", "How many projects it carried."),
    f("projects_created", "array", "The NAMES of projects minted because a task named one the document did not carry — a list, so nothing appears out of nowhere unseen."),
    f("docs_imported", "integer", "How many knowledge docs came in."),
    f("docs_declared", "boolean", "Whether the document had a `docs` section at all — an empty one told apart from a missing one (#179)."),
    f("events_imported", "integer", "How many audit rows came in."),
    n("default_project", "string", "The default project after the import, or null."),
    f("renumbered", "array", "Every task whose `short_id` a DIFFERENT task in this store already held, as `{id, from, to}` — it kept its id and took the next free number (D177). Empty when nothing moved."),
];

/// `event.list`'s result.
pub const R_EVENT_LIST: &[FieldDoc] = &[
    f("count", "integer", "How many events came back."),
    f(
        "events",
        "array",
        "The events, newest first for everything this engine wrote — `id` is a \
         UUIDv7, time-ordered — but a row brought in by `store.import` keeps its \
         document's id and can sort anywhere.",
    ),
];

/// `event.revert`'s result.
pub const R_EVENT_REVERT: &[FieldDoc] = &[
    f("reverted", "object", "The event that was undone."),
    f("short_id", "integer", "The task's short id — the small number every `ref` accepts and the CLI prints."),
    f("title", "string", "The title of the task the undo touched."),
    f("restored", "object", "What the inverse put back — per-op, the undo's own vocabulary."),
    f("_rev", "integer", "The task's revision counter. The methods that change the task or its tags, notes, checks and dependencies bump it; `token.add`, `token.remove` and `reminder.fire` leave it alone. Send it back as `expected_rev` to make a change conditional."),
];

/// The event an undo reversed.
pub const REVERTED_EVENT: &[FieldDoc] = &[
    f("event", "string", "The undone event's id."),
    f(
        "op",
        "string",
        "Its op — undo names what it undid instead of answering ok.",
    ),
    f("ts", "string", "When the undone event happened."),
];

/// `reminder.fire`'s result.
pub const R_REMINDER_FIRE: &[FieldDoc] = &[
    f(
        "fired",
        "boolean",
        "Whether this call fired the reminder. False on a repeat — the method is idempotent.",
    ),
    f(
        "short_id",
        "integer",
        "The task's short id — the small number every `ref` accepts and the CLI prints.",
    ),
    f(
        "at",
        "string",
        "The instant the reminder is recorded against.",
    ),
];

/// `core.capabilities`'s result.
pub const R_CORE_CAPABILITIES: &[FieldDoc] = &[
    f("api", "string", "The API major version this build speaks."),
    f("methods", "array", "Every method name it dispatches."),
    f("params", "object", "The accepted params key set, per method — a key outside it is refused, never ignored (D33)."),
    f("features", "array", "The optional features this build carries."),
    n("default_project", "string", "The store's default project, or null."),
    n("store", "string", "The store file this engine answers from (D74) — the caller's own in-process, the daemon's over a socket. Null in memory."),
];

/// `otlp.status`'s result.
pub const R_OTLP_STATUS: &[FieldDoc] = &[
    f("received", "integer", "Samples still inside the 30-day retention window."),
    f("attributed", "integer", "How many of them turned into a measurement."),
    f("orphaned", "integer", "How many matched no task's window."),
    n("last_seen", "string", "The newest sample's timestamp; null on an empty buffer — a misconfigured exporter told apart from a quiet one."),
];

/// Every documented group of one method's `result`, as `(path, fields)`.
///
/// Composed the way the conformance suite composes the same shape, and checked
/// against it key for key. An unknown method answers with an empty slice rather
/// than panicking: the generator's own guard is what reports a method with no
/// documented shape, with the method's name in the message, which is more use
/// than a panic from inside a lookup.
pub fn result_shape(method: &str) -> &'static [(&'static str, &'static [FieldDoc])] {
    match method {
        "project.create" => &[("result", R_PROJECT_CREATE)],
        "project.list" => &[
            ("result", R_PROJECT_LIST),
            ("result.projects[]", PROJECT_LIST_ROW),
        ],
        "project.use" => &[("result", R_PROJECT_USE)],
        "project.archive" => &[("result", R_PROJECT_ARCHIVE)],
        "task.add" => &[("result", R_TASK_ADD)],
        "task.list" => &[
            ("result", R_TASK_LIST),
            ("result.tasks[]", TASK_CORE),
            ("result.tasks[]", TASK_LIVE_TIME),
            ("result.tasks[]", TASK_BLOCKED),
            ("result.tasks[]", TASK_STATUS_FLAG),
        ],
        "task.get" => &[
            ("result", TASK_CARD_NOTES),
            ("result", TASK_BUDGET_GAUGE),
            ("result", TASK_CHECKS),
            ("result", TASK_CORE),
            ("result", TASK_LIVE_TIME),
            ("result", TASK_DEPENDS_ON),
            ("result", TASK_ANNOTATIONS),
            ("result", TASK_BLOCKS),
            ("result", TASK_SPAWNED_FROM),
            ("result", ANNOTATION_PAGING),
            ("result", TASK_ANNOTATIONS_REMOVED),
            ("result", TASK_TOKENS),
            ("result", TASK_BLOCKED),
            ("result", TASK_STATUS_FLAG),
            ("result", TASK_URGENCY_BREAKDOWN),
            ("result", TASK_UNMET_BLOCKERS),
            ("result.checks[]", TASK_CHECK),
            ("result.annotations[]", ANNOTATION_ROW),
            ("result.annotations[]", ANNOTATION_CAP),
            ("result.first_annotation", ANNOTATION_ROW),
            ("result.first_annotation", ANNOTATION_CAP),
            ("result.delivered_annotation", ANNOTATION_ROW),
            ("result.delivered_annotation", ANNOTATION_CAP),
            ("result.annotations_removed[]", TOMBSTONE_ROW),
            ("result.tokens[]", MEASUREMENT_ROW),
            ("result.urgency_breakdown", URGENCY_BREAKDOWN_ROW),
            ("result.unmet_blockers[]", BLOCKER_ROW),
        ],
        "task.brief" => &[
            ("result", R_TASK_BRIEF),
            // The task half is `task.get`'s result, less `urgency_breakdown`:
            // the brief never forwards `explain`.
            ("result.task", TASK_CARD_NOTES),
            ("result.task", TASK_BUDGET_GAUGE),
            ("result.task", TASK_CHECKS),
            ("result.task", TASK_CORE),
            ("result.task", TASK_LIVE_TIME),
            ("result.task", TASK_DEPENDS_ON),
            ("result.task", TASK_ANNOTATIONS),
            ("result.task", TASK_BLOCKS),
            ("result.task", TASK_SPAWNED_FROM),
            ("result.task", ANNOTATION_PAGING),
            ("result.task", TASK_ANNOTATIONS_REMOVED),
            ("result.task", TASK_TOKENS),
            ("result.task", TASK_BLOCKED),
            ("result.task", TASK_STATUS_FLAG),
            ("result.task", TASK_UNMET_BLOCKERS),
            ("result.task.checks[]", TASK_CHECK),
            ("result.task.annotations[]", ANNOTATION_ROW),
            ("result.task.annotations[]", ANNOTATION_CAP),
            ("result.task.first_annotation", ANNOTATION_ROW),
            ("result.task.first_annotation", ANNOTATION_CAP),
            ("result.task.delivered_annotation", ANNOTATION_ROW),
            ("result.task.delivered_annotation", ANNOTATION_CAP),
            ("result.task.annotations_removed[]", TOMBSTONE_ROW),
            ("result.task.tokens[]", MEASUREMENT_ROW),
            ("result.task.unmet_blockers[]", BLOCKER_ROW),
            ("result.neighbourhood", BRIEF_NEIGHBOURHOOD),
            ("result.neighbourhood.depends_on[]", BRIEF_PREREQUISITE),
            ("result.neighbourhood.blocks[]", BRIEF_DEPENDENT),
            ("result.memory", BRIEF_MEMORY),
            ("result.last_time", BRIEF_LAST_TIME),
            ("result.memory.hits[]", MEMORY_HIT_ROW),
        ],
        "task.start" => &[
            ("result", R_TASK_START),
            ("result.auto_stopped[]", AUTO_STOPPED_ROW),
        ],
        "task.stop" => &[("result", R_TASK_STOP)],
        "task.adjust_tracked" => &[("result", R_TASK_ADJUST_TRACKED)],
        "task.done" => &[
            ("result", R_TASK_DONE),
            ("result.blocked_by[]", BLOCKER_ROW),
        ],
        "task.modify" => &[("result", R_TASK_MODIFY)],
        "task.cancel" => &[("result", R_TASK_CANCEL)],
        "task.reopen" => &[("result", R_TASK_REOPEN)],
        "tag.add" => &[("result", R_TAG_ADD)],
        "tag.remove" => &[("result", R_TAG_REMOVE)],
        "annotation.add" => &[
            ("result", R_ANNOTATION_ADD),
            ("result.annotation", ANNOTATION_ROW),
        ],
        "annotation.remove" => &[
            ("result", R_ANNOTATION_REMOVE),
            ("result.removed", TOMBSTONE_ROW),
        ],
        "annotation.update" => &[
            ("result", R_ANNOTATION_UPDATE),
            ("result.annotation", ANNOTATION_ROW),
        ],
        "check.add" => &[("result", R_CHECK_ADD), ("result.check", TASK_CHECK)],
        "check.set" => &[("result", R_CHECK_SET)],
        "check.remove" => &[("result", R_CHECK_REMOVE)],
        "token.add" => &[
            ("result", R_TOKEN_ADD),
            ("result.measurement", MEASUREMENT_ROW),
        ],
        "token.remove" => &[
            ("result", R_TOKEN_REMOVE),
            ("result.removed", MEASUREMENT_ROW),
        ],
        "tokens.recompute" => &[
            ("result", R_TOKENS_RECOMPUTE),
            ("result.tasks[]", RECOMPUTE_ROW),
            ("result.tasks[].before", TOKEN_BUCKETS_ROW),
            ("result.tasks[].after", TOKEN_BUCKETS_ROW),
            ("result.totals", RECOMPUTE_TOTALS),
        ],
        "dependency.add" => &[("result", R_DEPENDENCY_ADD)],
        "dependency.remove" => &[("result", R_DEPENDENCY_REMOVE)],
        "link.add" => &[("result", R_LINK_ADD)],
        "link.remove" => &[("result", R_LINK_REMOVE)],
        "link.list" => &[("result", R_LINK_LIST), ("result.links[]", LINK_ROW)],
        "graph.query" => &[
            ("result", R_GRAPH_QUERY),
            ("result.nodes[]", GRAPH_NODE),
            ("result.edges[]", GRAPH_EDGE),
        ],
        "memory.add" => &[("result", R_MEMORY_ADD)],
        "memory.search" => &[
            ("result", R_MEMORY_SEARCH),
            ("result.hits[]", MEMORY_HIT_ROW),
        ],
        "memory.get" => &[("result", DOC_EXPORT_ROW)],
        "memory.remove" => &[("result", R_MEMORY_REMOVE)],
        "memory.import" => &[
            ("result", R_MEMORY_IMPORT),
            ("result.docs[]", IMPORTED_DOC_ROW),
        ],
        "memory.refresh" => &[
            ("result", R_MEMORY_REFRESH),
            ("result.refreshed[]", REFRESHED_DOC_ROW),
            ("result.missing[]", MISSING_DOC_ROW),
        ],
        "memory.list" => &[
            ("result", R_MEMORY_LIST),
            ("result.docs[]", MEMORY_LIST_ROW),
        ],
        "memory.update" => &[("result", R_MEMORY_UPDATE)],
        "report.summary" => &[
            ("result", R_REPORT_SUMMARY),
            ("result.groups[]", SUMMARY_GROUP_ROW),
        ],
        "report.outcomes" => &[
            ("result", R_REPORT_OUTCOMES),
            ("result.groups[]", OUTCOME_GROUP_ROW),
            ("result.groups[].rework", OUTCOME_RATE),
            ("result.groups[].calibration", OUTCOME_CALIBRATION),
            ("result.groups[].cost", OUTCOME_COST),
            ("result.groups[].silent", OUTCOME_RATE),
            ("result.groups[].abandonment", OUTCOME_ABANDONMENT),
            ("result.groups[].overrun", OUTCOME_RATE),
            ("result.groups[].unproven", OUTCOME_RATE),
            ("result.groups[].forced", OUTCOME_RATE),
        ],
        "store.export" => &[
            ("result", R_STORE_EXPORT),
            ("result.tasks[]", TASK_CORE),
            ("result.tasks[]", TASK_EXPORT_TIME),
            ("result.tasks[]", TASK_EXPORT_PIN),
            ("result.tasks[]", TASK_EXPORT_TOKENS),
            ("result.tasks[]", TASK_DEPENDS_ON),
            ("result.tasks[]", TASK_EXPORT_ANNOTATIONS),
            ("result.tasks[]", TASK_STATUS_FLAG),
            ("result.tasks[]", TASK_CHECKS),
            ("result.tasks[]", TASK_EXPORT_SPAWNED_FROM),
            ("result.tasks[].tokens[]", MEASUREMENT_ROW),
            ("result.tasks[].annotations[]", ANNOTATION_ROW),
            ("result.tasks[].checks[]", TASK_CHECK),
            ("result.projects[]", PROJECT_EXPORT_ROW),
            ("result.docs[]", DOC_EXPORT_ROW),
            ("result.events[]", EVENT_ROW),
        ],
        "store.import" => &[("result", R_STORE_IMPORT)],
        "event.list" => &[("result", R_EVENT_LIST), ("result.events[]", EVENT_ROW)],
        "event.revert" => &[
            ("result", R_EVENT_REVERT),
            ("result.reverted", REVERTED_EVENT),
        ],
        "reminder.fire" => &[("result", R_REMINDER_FIRE)],
        "core.capabilities" => &[("result", R_CORE_CAPABILITIES)],
        "otlp.status" => &[("result", R_OTLP_STATUS)],
        _ => &[],
    }
}
