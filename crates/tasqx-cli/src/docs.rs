//! `tasqx docs` — the English user guide, as ONE self-contained HTML file.
//!
//! Same idiom as `html.rs` (the `report --html` generator): one document with an
//! inline `<style>`, an inline `<script>`, a system-font stack, and zero external
//! requests — no CDN, no web fonts, no images, no server. Everything the browser
//! needs is in the file, so it opens off a temp path, a USB stick, or an air-gapped
//! box identically. Light/dark via `prefers-color-scheme` over the same CSS custom
//! properties. Every string that could contain markup goes through [`html::esc`] —
//! the one escaper both surfaces share.
//!
//! **Multi-page without a server.** The guide is eleven pages, each a `<section>`
//! with a stable `id`. The inline script shows one at a time and drives
//! `location.hash`, so every page is cross-linkable (`docs.html#filters`) and the
//! back button works. With JavaScript off, the `:target` CSS fallback still selects
//! a page and, absent any hash, the page simply renders as one long scrollable
//! document — the content is never *hidden behind* the script.
//!
//! **The doc-drift guard lives here, not in prose.** [`VERBS`], [`METHODS`], and
//! [`DOCUMENTED_CLEAR_FIELDS`] are the lists the page renders *from*, and the
//! tests at the bottom of this file assert each one equals the real surface it
//! claims to describe — clap's subcommand table, the core's `core.capabilities`,
//! and `crate::CLEARABLE`. Adding a verb without documenting it fails the
//! build, which is the cheapest honest guard available: the docs cannot
//! silently fall behind the CLI, because the CLI's own table is the assertion.
//!
//! Beyond the *names*, the guards now reach some of the descriptive columns too:
//! each verb's one-line description is rendered from [`crate::cmddoc`] rather
//! than restated here (so `-h` and this guide cannot disagree), and every
//! method's documented **required** parameters are checked against the engine's
//! own "missing required field" complaint. Two honest gaps remain, both named at
//! their tests: *optional* parameter names are unguarded (the engine ignores
//! unknown keys, so there is no failure to observe), and only the seven
//! bare-callable methods have their return shapes checked.
//!
//! Every command and every block of output on this page was executed against the
//! real binary on an isolated store; nothing here is illustrative.
//!
//! **The prose pages are the repository's own markdown.** `docs/wiki`
//! and `docs/guides` are `include_str!`d by [`markdown`] and rendered into
//! pages of this same document at generation time, rather than retyped as Rust
//! literals — one artifact, and no second copy of a sentence to drift.

use std::sync::LazyLock;

use crate::html::esc;

mod api_ref;
mod cli_ref;
mod markdown;
mod mcp_ref;
mod obj_ref;

/// The verb table the Commands page renders: `(verb, aliases, method)`.
///
/// This is the single source — the page is generated from it and
/// [`documented_verbs`] reads it, so there is no parallel list to fall out of
/// sync. Order is reading order, not clap's.
///
/// **There is deliberately no "what it does" column.** There used to be, and it
/// was a second hand-written prose description of every verb sitting next to
/// [`crate::cmddoc::CmdDoc::summary`] — the one `tasqx <verb> -h` prints — with
/// nothing comparing them. They had already diverged in wording on most rows
/// (`init` read "Create a project." here and "Create a project — just a name, no
/// folder." under `-h`), and nothing could ever have caught a divergence in
/// *meaning*: the guide and the terminal simply describing the same verb
/// differently is not a state any test can distinguish from correct.
///
/// A guard comparing the two would have had to accept "merely consistent",
/// which is unassertable prose-equivalence. So the column is gone and the page
/// renders [`crate::cmddoc`]'s summary instead. One string per verb, used by
/// both surfaces, with no second copy left to drift.
const VERBS: [(&str, &str, &str); 44] = [
    ("init", "—", "project.create"),
    ("use", "—", "project.use"),
    ("archive", "—", "project.archive"),
    ("add", "<code>a</code>, <code>new</code>", "task.add"),
    (
        "modify",
        "<code>mod</code>, <code>m</code>, <code>edit</code>",
        "task.modify",
    ),
    ("list", "<code>ls</code>, <code>l</code>", "task.list"),
    ("agenda", "<code>ag</code>, <code>cal</code>", "task.list"),
    ("next", "—", "task.list"),
    (
        "dashboard",
        "<code>dash</code>",
        "task.list + report.summary + project.list + event.list",
    ),
    (
        "pick",
        "<code>p</code>, <code>fzf</code>",
        "task.list + task.get + task.start",
    ),
    ("show", "<code>get</code>", "task.get"),
    ("why", "—", "task.get"),
    ("start", "<code>s</code>", "task.start"),
    ("stop", "<code>st</code>", "task.stop"),
    (
        "done",
        "<code>d</code>, <code>x</code>, <code>complete</code>",
        "task.done",
    ),
    (
        "cancel",
        "<code>delete</code>, <code>del</code>, <code>rm</code>",
        "task.cancel",
    ),
    ("reopen", "—", "task.reopen"),
    ("undo", "<code>u</code>", "event.revert"),
    ("annotate", "<code>note</code>", "annotation.add"),
    ("unannotate", "—", "annotation.remove"),
    ("tag", "—", "tag.add"),
    ("untag", "—", "tag.remove"),
    ("dep", "—", "dependency.add"),
    ("undep", "—", "dependency.remove"),
    ("projects", "—", "project.list"),
    ("brief", "—", "task.brief"),
    ("check", "—", "check.add + check.set + check.remove"),
    ("report", "—", "report.summary + report.outcomes"),
    ("chart", "—", "event.list"),
    ("theme", "—", "— (no store)"),
    ("config", "—", "— (registry + core.capabilities)"),
    (
        "memory",
        "—",
        // The suffix list is read as `memory.<suffix>` by the API reference's
        // verb map (#647), so `import` belongs in it: `tasqx memory import` is
        // the ONLY way to reach `memory.import`, and leaving it out printed
        // "no verb reaches this method" under the one method whose documented
        // use is a command line.
        "memory.search + get/add/remove/list/update + memory.import",
    ),
    ("tokens", "—", "tokens.recompute"),
    ("export", "—", "store.export"),
    ("import", "—", "store.import"),
    ("api", "—", "(any)"),
    ("daemon", "—", "(serves all)"),
    ("watch", "—", "task.list + push"),
    ("mcp", "—", "(subset)"),
    ("setup", "—", "— (no store)"),
    ("docs", "—", "— (no store)"),
    ("manual", "<code>man</code>", "— (no store)"),
    ("about", "—", "— (no store)"),
    ("completions", "—", "— (no store)"),
];

/// The method table the JSON API page renders: `(method, params, returns)`.
/// Single source, same reason as [`VERBS`].
const METHODS: [(&str, &str, &str); 42] = [
    (
        "project.create",
        "<code>name</code>, <code>description?</code>",
        "The project, plus <code>default</code> (did it claim the default?) and \
         <code>current_default</code>.",
    ),
    (
        "project.list",
        "<code>include_archived?</code>",
        "<code>{projects}</code>; each row carries <code>default</code>.",
    ),
    (
        "project.use",
        "<code>name</code>",
        "<code>{name, default, previous}</code>. Sets the default project.",
    ),
    (
        "project.archive",
        "<code>name</code>",
        "The archived project, plus <code>default_cleared</code>.",
    ),
    (
        "task.add",
        "<code>title</code>, <code>project?</code>, <code>priority?</code>, <code>due?</code>, \
         <code>scheduled?</code>, <code>wait?</code>, <code>recurrence?</code>, \
         <code>remind?</code>, <code>estimate?</code>, <code>tags?</code>, \
         <code>budget_tokens?</code>",
        "The new task, incl. the <code>project</code> it landed in (the default, if none given). \
         <code>budget_tokens</code> (D139) is a size gauge over FRESH tokens — input, output and \
         cache creation, never cache reads — which stops nothing and is read back as \
         <code>fresh_tokens</code> and <code>over</code> on <code>task.get</code>.",
    ),
    (
        "task.list",
        "<code>filter?</code>, <code>sort?</code>, <code>limit?</code>, <code>offset?</code>, \
         <code>fields?</code>",
        "<code>{count, total, next_offset, tasks}</code>. An omitted <code>filter</code> matches \
         everything; <code>count</code> is how many rows came back and <code>total</code> how many \
         matched, so a windowed list is never mistaken for a complete one. \
         <code>next_offset</code> is null once nothing is left. An omitted <code>limit</code> \
         defaults to 100; a named <code>limit</code> is clamped to 10,000. \
         <code>fields</code> may include <code>depends_on</code>, which no other projection emits.",
    ),
    (
        "task.get",
        "<code>ref</code>, <code>annotations_limit?</code>, <code>annotations_offset?</code>, \
         <code>max_body_bytes?</code>, <code>explain?</code>",
        "Full detail incl. annotations, deps, <code>blocked</code>. A limit takes the newest \
         annotations; <code>annotations_total</code> and <code>annotations_next_offset</code> \
         say what was left out. <code>max_body_bytes</code> caps each annotation body IN THE \
         RESPONSE (D148): a longer one is cut on a character boundary and the row gains \
         <code>body_bytes</code> (its real size) and <code>body_truncated</code>, so a single \
         enormous note cannot blow a caller's payload limit; omit it and every body comes back \
         whole, which is what the CLI and <code>tasqx api</code> do. \
         <code>annotations_removed</code> lists the tombstones <code>annotation.remove</code> \
         left — <code>{id, removed}</code> per scrubbed note, never its text — while \
         <code>annotations</code> and <code>annotations_total</code> keep excluding them. \
         <code>explain: true</code> adds <code>urgency_breakdown</code> \
         (<code>priority</code>, <code>due_proximity</code>, <code>age</code>, <code>total</code>) \
         — the terms <code>urgency</code> sums (D1); the plumbing <code>tasqx why --json</code> \
         uses (#150).",
    ),
    (
        "task.start",
        "<code>ref</code>, <code>keep?</code>, <code>session_id?</code>, \
         <code>transcript_path?</code>, <code>client?</code>, <code>actor?</code>",
        "The task, timer running. Correlation params land in the start event. \
         <code>auto_stopped</code> lists whichever other task D6's single-active \
         rule just stopped to make room for this one — empty unless <code>keep</code> \
         was omitted and something else was running. <code>actor</code> (D140) says who \
         is asking for the clock: a start against a timer a DIFFERENT actor holds is \
         refused <code>conflict</code> rather than silently stopping it, since that would \
         leave the other party's work untracked. Absent on both sides — a shell — keeps \
         D6's behaviour exactly; the MCP server fills it in with its own connection id.",
    ),
    (
        "task.stop",
        "<code>ref</code>",
        "<code>{status, interval, tracked}</code>. <code>interval</code> is the duration just \
         closed; <code>tracked</code> is the running total, the same word <code>task.get</code> \
         uses for it.",
    ),
    (
        "task.done",
        "<code>ref</code>, <code>force?</code>, <code>session_id?</code>, \
         <code>transcript_path?</code>, <code>client?</code>, <code>tool?</code>, \
         <code>model?</code>, <code>input_tokens?</code>, <code>output_tokens?</code>, \
         <code>cache_read_tokens?</code>, <code>cache_creation_tokens?</code>, \
         <code>checks_passed?</code>, <code>evidence?</code>",
        "The task; plus the spawned next instance if recurring. Correlation params \
         land in the done event, and so do <code>tool</code> and <code>model</code> on \
         their own (D65) — a caller that cannot count its tokens still records who did \
         the work. Any present token count additionally records a self-report \
         measurement — the primary channel: only the caller knows which task a \
         turn's spend served, and the log-parse fallback refuses samples claimed \
         by more than one task's window. A task with open blockers is refused \
         <code>conflict</code> naming them; <code>force: true</code> completes it \
         anyway, and the response then carries <code>forced: true</code> and \
         <code>blocked_by</code> (D150).",
    ),
    (
        "task.modify",
        "<code>ref</code>, <code>set</code>, <code>expected_rev?</code>",
        "<code>{short_id, _rev, set}</code>; <code>set</code> echoes the RESOLVED value \
         actually stored for each field this call named (e.g. <code>due:\"friday\"</code> \
         comes back as its ISO instant). <code>null</code> in the request's own \
         <code>set</code> clears a field.",
    ),
    (
        "task.cancel",
        "<code>ref</code>",
        "<code>{short_id, status}</code>.",
    ),
    (
        "task.reopen",
        "<code>ref</code>",
        "<code>{short_id, status, blocked}</code>. <code>blocked</code> names the open dependents \
         this reopen put back — the mirror of <code>unblocked</code> on <code>task.done</code>.",
    ),
    (
        "tag.add",
        "<code>ref</code>, <code>tags</code>",
        "The task's tags.",
    ),
    (
        "tag.remove",
        "<code>ref</code>, <code>tags</code>",
        "The task's remaining tags, plus <code>removed</code>. A tag the task \
         does not have is <code>not_found</code> and removes <em>nothing</em> — \
         all or nothing, so a typo can never answer ok.",
    ),
    (
        "annotation.add",
        "<code>ref</code>, <code>body</code>",
        "The annotation.",
    ),
    (
        "check.add",
        "<code>ref</code>, <code>body</code>",
        "<code>{short_id, check{id, body, state, evidence, position, created, modified}}</code>. \
         One acceptance criterion (D138), appended at the end and starting <code>open</code>. \
         tasqx NEVER RUNS a check: the body is a claim and <code>evidence</code> is a citation, \
         both stored verbatim and neither interpreted. What runs commands is a hook the operator \
         installed, calling <code>check.set</code> like any other client.",
    ),
    (
        "check.set",
        "<code>ref</code>, <code>check_id</code>, <code>state</code>, <code>evidence?</code>",
        "<code>{short_id, check_id, state}</code>. <code>state</code> is \
         <code>open|passed|failed</code>; <code>failed</code> is a normal outcome, not an error. \
         <code>evidence</code> is optional — some criteria are met by something nobody can \
         quote, and inventing a citation is worse than an unproven pass.",
    ),
    (
        "check.remove",
        "<code>ref</code>, <code>check_id</code>",
        "<code>{short_id, check_id, removed}</code>. For a criterion that was the wrong thing to \
         ask; use <code>check.set failed</code> when the criterion was right and the work did \
         not meet it.",
    ),
    (
        "annotation.remove",
        "<code>ref</code>, <code>annotation_id</code>",
        "<code>{short_id, removed}</code>. Scrubs the annotation's body in the store (D113) — \
         a hard delete, not a hide — and <code>removed</code> names only the id and the \
         instant, never the text. An unknown or already-removed id is \
         <code>not_found</code>; <code>event.revert</code> does not cover this op.",
    ),
    (
        "token.add",
        "<code>ref</code>, <code>tool</code>, <code>source</code>, <code>confidence</code>, \
         <code>model?</code>, <code>input_tokens?</code>, <code>output_tokens?</code>, \
         <code>cache_read_tokens?</code>, <code>cache_creation_tokens?</code>, \
         <code>idempotency_key?</code>",
        "<code>{short_id, measurement}</code>. Records AI token spend; never bumps \
         <code>_rev</code>. A repeated <code>idempotency_key</code> on the same task \
         returns the measurement it already banked, unchanged.",
    ),
    (
        "token.remove",
        "<code>measurement_id</code>",
        "<code>{short_id, removed}</code>. Deletes one measurement by id — the correction path \
         a wrong self-report otherwise has none of (D50, D67; #210). <code>removed</code> is \
         the measurement object that is now gone.",
    ),
    (
        "dependency.add",
        "<code>ref</code>, <code>depends_on</code>",
        "Dep state + <code>blocked</code>.",
    ),
    (
        "dependency.remove",
        "<code>ref</code>, <code>depends_on</code>",
        "Dep state + <code>blocked</code>.",
    ),
    (
        "memory.add",
        "<code>title</code>, <code>body</code>, <code>source?</code>, <code>project?</code>, \
         <code>standing?</code>",
        "<code>{id, title, project, standing, created}</code>. Body stored verbatim (D41). \
         <code>project</code> is optional free-standing scoping (#134); an unset doc stays \
         global rather than defaulting onto whatever project is current. \
         <code>standing</code> (default false) marks a ruling meant for every session of its \
         scope, and above fifteen such docs in one scope the result also carries a \
         <code>hint</code> to merge or retract (D156).",
    ),
    (
        "memory.get",
        "<code>id</code>",
        "<code>{id, title, body, source, project, rev, created, modified}</code> — one doc \
         whole, by the id a search hit carries. An annotation id is refused, naming the task to \
         read it from.",
    ),
    (
        "memory.search",
        "<code>query</code>, <code>limit?</code>, <code>scope?</code>, <code>raw?</code>, \
         <code>project?</code>, <code>include_unscoped?</code>",
        "<code>{count, total, has_more, hits, matched}</code> — bm25-ranked over docs + \
         annotations, stemmed (porter tokenizer, #128) so \"reviewing\" matches a doc that only \
         says \"review\". <code>matched</code> is the FTS5 expression actually run, which is how \
         <code>count: 0</code> is told apart from a store holding nothing on the subject. \
         <code>total</code> is every row matched before <code>limit</code> truncates (#132), and \
         <code>project</code> scopes to one project's docs plus its tasks' annotations (#134).",
    ),
    (
        "memory.remove",
        "<code>id</code>",
        "<code>{id, removed}</code>.",
    ),
    (
        "memory.import",
        "<code>docs</code>",
        "<code>{imported, replaced, docs}</code>, each doc <code>{id, title, source, replaced, _rev}</code>. \
         One transaction; same <code>source</code> replaces IN PLACE (id and creation date kept), \
         bumps that doc's <code>_rev</code> (D143) and is counted in <code>replaced</code>; \
         a batch naming one <code>source</code> twice is refused whole (D174).",
    ),
    (
        "memory.list",
        "<code>limit?</code>, <code>offset?</code>, <code>project?</code>, <code>standing?</code>",
        "<code>{count, total, next_offset, docs}</code> — the same paging shape as \
         <code>task.list</code> (#133). Browses docs newest-modified first, without a query; \
         each row carries a <code>body_preview</code>, not the full body, and a \
         <code>standing</code> flag; <code>standing</code> filters to only standing docs \
         (<code>true</code>) or only the rest (<code>false</code>), omitted for all (D156).",
    ),
    (
        "memory.update",
        "<code>id</code>, <code>title?</code>, <code>body?</code>, <code>source?</code>, \
         <code>project?</code>, <code>standing?</code>, <code>expected_rev?</code>",
        "<code>{id, title, source, project, standing, rev, modified}</code> — replaces a doc's \
         fields IN PLACE (#135), the correction path <code>memory.remove</code>'s permanence has \
         none of. <code>standing</code> sets or clears the flag and bumps <code>rev</code> like \
         any other edit (D156). <code>expected_rev</code> is <code>task.modify</code>'s \
         optimistic-concurrency guard, unchanged: mismatched, it is a <code>conflict</code> \
         naming both revs.",
    ),
    (
        "tokens.recompute",
        "<code>dry_run?</code>",
        "<code>{dry_run, tasks, totals}</code>. Re-runs log-parse attribution over stored \
         windows under the refusal rule (D50). <code>dry_run</code> defaults to \
         <em>true</em> — report the per-task delta, write nothing; send \
         <code>false</code> to apply. In-process only: a daemon refuses this \
         method over the socket, so stop any daemon on the store first.",
    ),
    (
        "report.summary",
        "<code>group_by?</code>, <code>filter?</code>, <code>metrics?</code>, <code>all?</code>, \
         <code>since?</code>, <code>until?</code>",
        "<code>{groups, generated, filter, all, since, until}</code>. <code>group_by</code> \
         defaults to <code>project</code>; the result echoes the scope it applied, so a total \
         cannot be read against the wrong period. <code>since</code>/<code>until</code> \
         (D97) window <code>tracked_total</code> and the token buckets by WHEN the time or \
         spend happened — a different axis from <code>filter</code>'s \
         <code>completed.after:</code>/<code>completed.before:</code>, which selects tasks by \
         completion date; both are <code>null</code> unless the caller sets them.",
    ),
    (
        "task.brief",
        "<code>ref</code>, <code>memory_limit?</code>, <code>max_body_bytes?</code>",
        "<code>{task, neighbourhood, memory}</code>. Everything needed before starting one task \
         (D136): <code>task</code> is <code>task.get</code>'s own result verbatim; \
         <code>neighbourhood.depends_on</code> names each prerequisite with its NEWEST \
         annotation — what that task concluded — and <code>neighbourhood.blocks</code> names \
         what this one releases, title and status only; <code>memory</code> is a \
         <code>memory.search</code> result under an expression tasqx DERIVES from the task's \
         title, tags and project, echoed in <code>matched</code>, scoped to that project and \
         reported in <code>project</code>. The derived expression is a disjunction: a caller's \
         query states what they want and is ANDed, a derived one is a bag of the task's own \
         words and would answer nothing if it were. Half the page (rounded up) is reserved for \
         knowledge docs and annotations fill the rest, either kind taking the other's unused \
         slots, docs listed first (D147); <code>reserved_docs</code>, <code>docs_total</code> \
         and <code>annotations_total</code> say what was done. <code>memory_limit</code> \
         defaults to 5 (D154). <code>max_body_bytes</code> caps \
         each annotation body of the TASK half in the response, exactly as on \
         <code>task.get</code> (D148) — no <code>annotations_limit</code>, because a brief is \
         what you read BEFORE starting and dropping whole notes from it is the wrong cut.",
    ),
    (
        "report.outcomes",
        "<code>group_by?</code>, <code>filter?</code>, <code>metrics?</code>, \
         <code>since?</code>, <code>until?</code>",
        "<code>{groups, group_by, metrics, generated, filter, since, until, store_empty}</code>. \
         What the work DID, as against what it cost (D137): <code>rework</code> (completions that \
         were reopened), <code>calibration</code> (median tracked-over-estimate), \
         <code>cost</code> (the four token buckets, never blended), <code>silent</code> \
         (completions carrying no annotation), <code>abandonment</code> (started, then \
         cancelled) and <code>forced</code> (completions that overrode open blockers, D150). \
         Every rate comes back beside the <code>n</code> it was computed over. \
         Scope is tasks that CLOSED, by the instant they closed — so a completion that was \
         reopened still counts, which is the whole point. Omitting <code>metrics</code> emits \
         all of them; there is no <code>all</code>, because here a cancellation is a measured \
         outcome rather than noise D24 excludes.",
    ),
    (
        "store.export",
        "<code>filter?</code>",
        "<code>{tasks, projects, docs, events, default_project, dropped_dependencies}</code>. \
         <code>events</code> is the whole audit log except the bookkeeping rows a `store.import` \
         itself writes.",
    ),
    (
        "store.import",
        "<code>tasks</code>, <code>projects?</code>, <code>default_project?</code>, \
         <code>docs?</code>, <code>events?</code>",
        "<code>{imported, projects_imported, projects_created, docs_imported, docs_declared, \
         events_imported, default_project}</code>. A task already in the store at a higher \
         <code>_rev</code> than the payload's refuses the whole import (conflict) rather than \
         silently discarding the annotations, tags and edges added since.",
    ),
    (
        "event.list",
        "<code>limit?</code>, <code>ref?</code>, <code>entity?</code>, <code>from?</code>",
        "<code>{count, events}</code> — the append-only log. <code>from</code> takes \
         the usual date grammar and is a lower bound, not an exact filter: it \
         promises no events older than roughly that instant.",
    ),
    (
        "event.revert",
        "—",
        "<code>{reverted, short_id, title, restored}</code> — <code>reverted</code> carries \
         the event id, its op and its timestamp. Undoes the <em>newest</em> event by \
         APPENDING a compensating one, so the reversed event stays in the log. Four ops are \
         undoable (<code>stop</code>, <code>tag.remove</code>, <code>dependency.remove</code>, \
         <code>annotation.add</code>); every other one is <code>conflict</code> naming itself \
         and what does take it back. The newest <em>event</em>, not the last call you made: a \
         call that changed nothing writes no event, so undo reaches past it — which is why the \
         answer names what it undid instead of saying ok.",
    ),
    (
        "reminder.fire",
        "<code>ref</code>, <code>at</code>",
        "<code>{fired, short_id, at}</code>. Idempotent.",
    ),
    (
        "core.capabilities",
        "—",
        "<code>{api, methods, params, features, default_project, store}</code>.",
    ),
    (
        "otlp.status",
        "—",
        "<code>{received, attributed, orphaned, last_seen}</code>. Read-only visibility into the \
         opt-in local OTLP receiver's buffer (#18): <code>received</code> is every sample still \
         inside the 30-day retention window, <code>attributed</code> how many turned into a \
         <code>source=otel</code> measurement, <code>orphaned</code> the rest, and \
         <code>last_seen</code> the newest sample's timestamp (<code>null</code> on an empty \
         buffer) — the only way to tell a misconfigured exporter from a healthy, quiet one.",
    ),
];

/// The CLI verbs this guide documents — read straight off the rendered table.
/// Test-only: the page renders from [`VERBS`] directly, so this exists purely to
/// let the drift guard compare that same table against clap.
#[cfg(test)]
fn documented_verbs() -> Vec<&'static str> {
    VERBS.iter().map(|(v, _, _)| *v).collect()
}

/// The JSON API methods this guide documents — read straight off [`METHODS`].
/// Test-only, same reason as [`documented_verbs`].
#[cfg(test)]
fn documented_methods() -> Vec<&'static str> {
    METHODS.iter().map(|(m, _, _)| *m).collect()
}

/// The one-line description of `verb`, taken from the terminal command registry
/// so the guide and `tasqx <verb> -h` cannot disagree.
///
/// Falls back to the empty string rather than panicking: a missing entry is
/// already a hard test failure in `html_verbs_agree_with_cmddoc`, and the
/// generator has no business aborting `tasqx docs` over a documentation gap.
fn verb_summary(verb: &str) -> &'static str {
    crate::cmddoc::find(verb).map(|d| d.summary).unwrap_or("")
}

/// The fields `modify --clear` accepts. Asserted equal to `crate::CLEARABLE`.
pub const DOCUMENTED_CLEAR_FIELDS: [&str; 10] = [
    "project",
    "priority",
    "due",
    "scheduled",
    "wait",
    "remind",
    "recurrence",
    "estimate",
    "tracked",
    "budget_tokens",
];

/// The global-flags table the Commands page renders: `(flag, effect)`.
///
/// Single source, same reason as [`VERBS`]. These four hang off the top-level
/// `Cli`, so `cmddoc`'s per-verb flag guard is structurally blind to them —
/// this table is their one documented home, and the sibling guard in `cmddoc`
/// binds it to clap's own global argument list, both directions.
pub(crate) const GLOBAL_FLAGS: [(&str, &str); 5] = [
    (
        "<code>--json</code>",
        "Print the raw JSON API result instead of the human table.",
    ),
    (
        "<code>--theme &lt;name&gt;</code>",
        "Override the theme (<code>nord</code>, <code>gruvbox</code>, <code>dracula</code>, <code>solarized</code>, <code>mono</code>, or a user file). Beats <code>$TASQX_THEME</code> and the config.",
    ),
    (
        "<code>--socket &lt;addr&gt;</code>",
        "Socket / named pipe of a daemon. Overrides <code>$TASQX_SOCK</code>. Refused by <code>api</code>, <code>mcp</code>, <code>chart</code> and <code>report --html</code>, which open the store in-process and cannot honour it (D73).",
    ),
    (
        "<code>--no-daemon</code>",
        "Never route through a daemon; always run in-process. The escape hatch for scripts.",
    ),
    (
        "<code>--help</code>, <code>--version</code>",
        "The source of truth for this page.",
    ),
];

/// The `add` flag/sugar table the Commands page renders: `(flag, sugar, notes)`.
///
/// Single source, same reason as [`VERBS`]. The sugar column is the one place
/// a reader learns every colon-key alias, and it is bound to the parser's own
/// key table (`sugar::VALUE_KEYS`) the same way the `--clear` list is bound to
/// `crate::CLEARABLE` — because an alias the parser gains and no page names is
/// invisible, and one the parser drops leaves the page teaching a spelling
/// that silently lands in the title.
const ADD_FIELDS: [(&str, &str, &str); 9] = [
    (
        "<code>--project &lt;p&gt;</code>",
        "<code>project:</code>, <code>proj:</code>",
        "Free-form dotted name.",
    ),
    (
        "<code>--priority &lt;p&gt;</code> / <code>-p</code>",
        "<code>!high</code>, <code>!h</code>",
        "<code>H</code>, <code>M</code>, <code>L</code> (or high/medium/low).",
    ),
    (
        "<code>--tag &lt;t&gt;</code> / <code>-t</code>",
        "<code>+tag</code>",
        "Repeatable.",
    ),
    (
        "<code>--due &lt;d&gt;</code>",
        "<code>due:</code>",
        "<a href=\"#scheduling\">Natural language</a>.",
    ),
    (
        "<code>--scheduled &lt;d&gt;</code>",
        "<code>scheduled:</code>, <code>sched:</code>",
        "When you plan to start.",
    ),
    (
        "<code>--wait &lt;d&gt;</code>",
        "<code>wait:</code>",
        "Hide in the backlog until then.",
    ),
    (
        "<code>--repeat &lt;r&gt;</code>",
        "<code>repeat:</code>, <code>every:</code>, <code>recur:</code>",
        "<a href=\"#scheduling\">Recurrence rule</a>.",
    ),
    (
        "<code>--remind &lt;r&gt;</code>",
        "<code>remind:</code>",
        "<a href=\"#reminders\">Offset or absolute</a>.",
    ),
    (
        "<code>--estimate &lt;e&gt;</code> / <code>-e</code>",
        "<code>est:</code>, <code>estimate:</code>",
        "<code>4h</code>, <code>90m</code>, <code>1h30m</code>, <code>2d</code>, or ISO <code>PT4H</code>.",
    ),
];

/// A sidebar section: a heading in the nav, and the pages filed under it.
///
/// The sections are the reader's mental model of the guide — learn it, use it,
/// look it up — and they are deliberately declared even when empty: an empty
/// section's heading still renders with a placeholder, so the shape of the
/// finished guide is visible from the first screen and the pages that fill it
/// later need no shell change.
struct Section {
    id: &'static str,
    title: &'static str,
}

/// The sidebar, top to bottom. [`PAGES`] order *within* a section is the order
/// they appear under it, and the concatenation of the two is the reading order
/// [`page_close`]'s prev/next links walk.
const SECTIONS: [Section; 7] = [
    Section {
        id: "get-started",
        title: "Get started",
    },
    Section {
        id: "using",
        title: "Using tasqx",
    },
    Section {
        id: "guides",
        title: "Guides",
    },
    Section {
        id: "ref-cli",
        title: "Reference: CLI",
    },
    Section {
        id: "ref-api",
        title: "Reference: JSON API",
    },
    Section {
        id: "ref-mcp",
        title: "Reference: MCP",
    },
    Section {
        id: "objects",
        title: "Objects",
    },
];

/// One page of the guide.
///
/// `id` is the hash anchor — `docs.html#filters` — and is **frozen**: every
/// link anyone has ever shared is one of these. Regrouping the sidebar moves a
/// page under a different heading and changes nothing a link depends on.
/// `label` is the sidebar entry (trusted markup: the fixed table below carries
/// `&amp;`, and a markdown page's label is escaped when it is built), and
/// `title` is the `<h2>` the page opens with.
///
/// Owned rather than `&'static str` because half the rows are no longer
/// literals: a markdown page's id and title are computed from the file name and
/// its `# ` heading, and there is no const that can hold them.
struct Page {
    id: String,
    section: &'static str,
    label: String,
    title: String,
}

/// The pages whose HTML is a function in this file: `(id, section, label, title)`.
///
/// Tuple table for the same reason [`VERBS`] and [`METHODS`] are: the page
/// builders read it, [`PAGES`] is built from it, and there is no second list.
const FIXED_PAGES: [(&str, &str, &str, &str); 11] = [
    ("overview", "get-started", "Overview", "What tasqx is"),
    (
        "install",
        "get-started",
        "Install &amp; quickstart",
        "Install and quickstart",
    ),
    ("filters", "using", "Filter grammar", "The filter grammar"),
    (
        "scheduling",
        "using",
        "Scheduling &amp; recurrence",
        "Dates and recurrence",
    ),
    ("reminders", "using", "Reminders", "Reminders"),
    (
        "daemon",
        "using",
        "Daemon &amp; watch",
        "The daemon and live watch",
    ),
    ("data", "using", "Export &amp; import", "Export and import"),
    (
        "themes",
        "using",
        "Themes &amp; reports",
        "Themes, charts and reports",
    ),
    ("commands", "ref-cli", "Commands", "Every command"),
    ("api", "ref-api", "JSON API", "The JSON API"),
    ("mcp", "ref-mcp", "MCP", "The MCP server"),
];

/// The guide's pages, in sidebar order, each naming its [`SECTIONS`] entry.
///
/// Built by walking [`SECTIONS`], so this order *is* the sidebar's by
/// construction rather than by a table that has to be kept parallel to one —
/// which is what [`pages_are_emitted_in_sidebar_order`] used to be the only
/// thing standing between. Within a section the hand-written pages come first
/// and the markdown ones follow, in their tables' order.
static PAGES: LazyLock<Vec<Page>> = LazyLock::new(|| {
    let mut out = Vec::with_capacity(FIXED_PAGES.len() + markdown::PAGES.len());
    for sec in SECTIONS {
        for (id, section, label, title) in FIXED_PAGES {
            if section == sec.id {
                out.push(Page {
                    id: id.to_string(),
                    section,
                    label: label.to_string(),
                    title: title.to_string(),
                });
            }
        }
        // The object pages are titled from their own table (#648), so there is
        // no second list of their ids to keep beside it.
        for o in obj_ref::OBJECTS.iter().filter(|_| sec.id == "objects") {
            out.push(Page {
                id: o.id.to_string(),
                section: sec.id,
                label: esc(o.name),
                title: o.name.to_string(),
            });
        }
        for md in markdown::PAGES.iter().filter(|p| p.section == sec.id) {
            out.push(Page {
                id: md.id.clone(),
                section: md.section,
                // The fixed labels above are trusted markup; a title lifted out
                // of a markdown file is not, so it is escaped once, here.
                label: esc(&md.title),
                title: md.title.clone(),
            });
        }
    }
    out
});

/// Render the whole guide as one self-contained HTML string.
pub fn generate() -> String {
    // Table ids (`next_table_id`) are unique per call, not globally — reset
    // so two `generate()` calls in one test process don't disagree with
    // themselves.
    TABLE_SEQ.with(|c| c.set(0));
    let mut body = String::new();

    body.push_str(&header());
    body.push_str("<div class=\"shell\">");
    body.push_str(&nav());
    body.push_str("<main>");

    // Emitted in PAGES order, which is sidebar order: with JavaScript off the
    // document is one long page and that order is the only reading order there
    // is. Adding a page means a line here and a row in FIXED_PAGES (or a file
    // in one of the markdown tables); the test below asserts the two agree.
    body.push_str(&page_overview());
    body.push_str(&page_install());
    body.push_str(&page_filters());
    body.push_str(&page_scheduling());
    body.push_str(&page_reminders());
    body.push_str(&page_daemon());
    body.push_str(&page_data());
    body.push_str(&page_themes());
    for md in markdown::PAGES.iter().filter(|p| p.section == "using") {
        body.push_str(&page_markdown(md));
    }
    for md in markdown::PAGES.iter().filter(|p| p.section == "guides") {
        body.push_str(&page_markdown(md));
    }
    body.push_str(&page_commands());
    body.push_str(&page_api());
    body.push_str(&page_mcp());
    for o in &obj_ref::OBJECTS {
        body.push_str(&obj_ref::page(o));
    }

    body.push_str("</main></div>");
    body.push_str(&format!(
        "<footer>tasqx {ver} · every command and every block of output on this page was \
         executed against this binary. One file, no external requests.</footer>",
        ver = esc(env!("CARGO_PKG_VERSION")),
    ));

    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>tasqx — user guide</title>\n<style>\n{css}\n</style>\n</head>\n\
         <body>\n{body}\n<script>\n{js}\n</script>\n</body>\n</html>\n",
        css = css(),
        js = js(),
    )
}

// ============================================================================
// Chrome
// ============================================================================

/// The top bar: the drawer toggle, the brand, the version, and the theme
/// switch.
///
/// The three theme buttons are a *stored preference*, not a style: the CSS
/// still follows `prefers-color-scheme` on its own, and `Auto` is the absence
/// of the `data-theme` attribute rather than a third palette. So a reader who
/// never touches the switch — or who has JavaScript off, or a browser that
/// refuses `localStorage` — gets exactly the behaviour this page has always
/// had.
fn header() -> String {
    let mut themer = String::new();
    for (mode, label) in [("light", "Light"), ("dark", "Dark"), ("system", "Auto")] {
        themer.push_str(&format!(
            "<button class=\"themebtn\" type=\"button\" data-theme-set=\"{mode}\" \
             aria-pressed=\"false\">{label}</button>"
        ));
    }
    format!(
        "<header class=\"top\">\
           <button id=\"navtoggle\" type=\"button\" aria-label=\"Toggle navigation\" \
             aria-expanded=\"false\">Menu</button>\
           <div class=\"brand\">tasqx <span class=\"muted\">user guide</span></div>\
           <div class=\"ver muted\">v{}</div>\
           <div class=\"themer\" role=\"group\" aria-label=\"Colour theme\">{themer}</div>\
         </header>",
        esc(env!("CARGO_PKG_VERSION"))
    )
}

/// The sidebar: a search box, then one block per [`SECTIONS`] entry.
///
/// A section with no pages renders its heading and says so, rather than being
/// skipped. That is the point of declaring it — the reader can see that a
/// Guides section is coming, and the day a page lands under it nothing about
/// this function changes.
fn nav() -> String {
    let mut tree = String::new();
    for sec in SECTIONS {
        let mut links = String::new();
        for pg in PAGES.iter().filter(|p| p.section == sec.id) {
            // `label` is a literal above and already entity-safe; ids are literals too.
            links.push_str(&format!(
                "<a href=\"#{id}\" data-page=\"{id}\">{label}</a>",
                id = pg.id,
                label = pg.label,
            ));
        }
        if links.is_empty() {
            links.push_str("<div class=\"navsec-soon\">Coming soon</div>");
        }
        tree.push_str(&format!(
            "<div class=\"navsec\" data-section=\"{id}\">\
               <div class=\"navsec-h\">{title}</div>{links}\
             </div>",
            id = sec.id,
            title = esc(sec.title),
        ));
    }
    format!(
        "<nav id=\"nav\" aria-label=\"Guide\">\
           <div class=\"navsearch\">\
             <input id=\"navq\" type=\"search\" autocomplete=\"off\" \
               placeholder=\"Search the guide\" aria-label=\"Search the guide\">\
           </div>\
           <div id=\"navhits\" class=\"navhits\" hidden></div>\
           <div id=\"navtree\">{tree}</div>\
         </nav>"
    )
}

// ============================================================================
// Page 1 — Overview
// ============================================================================

fn page_overview() -> String {
    let mut s = page_open("overview");

    s.push_str(&lead(
        "tasqx is a fast, terminal-first, AI-native task manager. It is a headless Rust \
         core engine that exposes one stable, versioned JSON API — and every surface you \
         touch (the CLI, the MCP server, the HTML report, this guide) is a client of that \
         one contract.",
    ));

    s.push_str(&h3("The five ideas that shape everything else"));
    s.push_str(&table(
        &["Principle", "What it means for you"],
        &[
            &["Fast", "One-shot commands open SQLite, do the work, and exit. No daemon required, no warmup."],
            &["Local-first", "Your data is one SQLite file on your disk. No account, no cloud, works offline forever."],
            &["One API", "The CLI does nothing you cannot do over JSON. Every verb below names the method it calls."],
            &["AI-native", "The same typed API drives a bundled MCP server, so an agent is a first-class user."],
            &["Honest", "Every command speaks human text and <code>--json</code>. Exit codes are contract, not decoration."],
        ],
    ));

    s.push_str(&h3("How the pieces fit"));
    s.push_str(&p(
        "The core is a plain Rust library. The CLI links it and calls functions in-process — \
         no IPC on the hot path. The JSON API is a thin envelope over that <em>same</em> dispatch \
         layer, so \"call a function\" and \"send a JSON command\" run identical code. There is \
         exactly one dispatch table.",
    ));
    s.push_str(ARCH_SVG);

    s.push_str(&h3("Every mutation is logged, in the same transaction"));
    s.push_str(&p(
        "Adding, modifying, completing — each writes its row to an append-only <code>events</code> \
         table inside the <em>same</em> SQLite transaction as the change itself. The log cannot \
         drift from the data, because there is no window in which one exists without the other. \
         That is what makes <a href=\"#themes\">charts</a>, <a href=\"#reminders\">reminder dedupe</a>, \
         and a future sync possible without a migration.",
    ));

    s.push_str(&h3("Where your data lives"));
    s.push_str(&table(
        &["What", "Where"],
        &[
            &["Store", "<code>$TASQX_DB</code> if set, else the platform data dir: <code>%APPDATA%\\tasqx\\tasqx\\data\\tasks.db</code> on Windows (the doubled segment is what the <code>directories</code> crate produces from organization + application), <code>~/.local/share/tasqx/tasks.db</code> on Linux, <code>~/Library/Application Support/dev.tasqx.tasqx/tasks.db</code> on macOS"],
            &["Config", "<code>$TASQX_CONFIG_DIR/config.toml</code>, else the platform config dir"],
            &["Themes", "<code>$TASQX_CONFIG_DIR/themes/*.toml</code>"],
            &["Socket", "<code>$TASQX_SOCK</code>, else a platform default (see <a href=\"#daemon\">Daemon</a>)"],
        ],
    ));
    s.push_str(&note(
        "Point <code>$TASQX_DB</code> at a scratch file to try anything in this guide without \
         touching your real store. Every example below was run exactly that way.",
    ));

    // Rendered from `config::SETTINGS` rather than hand-written. That makes
    // drift structurally impossible rather than merely detected: a new setting
    // appears here the moment it is registered, with no second list to forget.
    // Note what that means for the guard below — since both sides derive from
    // one constant, it CANNOT catch an undocumented setting, and does not claim
    // to. It catches the section being deleted.
    s.push_str(&h3("Settings"));
    s.push_str(&p(
        "<code>tasqx config list</code> shows every setting with the layer that supplied it. \
         Resolution order is the CLI flag, then the <code>TASQX_*</code> environment variable, \
         then <code>config.toml</code>, then the built-in default.",
    ));
    let setting_rows: Vec<Vec<String>> = crate::config::SETTINGS
        .iter()
        .map(|st| {
            vec![
                format!("<code>{}</code>", st.key),
                match st.home {
                    crate::config::Home::Toml => "<code>config.toml</code>".to_string(),
                    crate::config::Home::Store => "the store".to_string(),
                },
                if st.default.is_empty() {
                    "—".to_string()
                } else {
                    format!("<code>{}</code>", st.default)
                },
                api_ref::describe(st.summary),
            ]
        })
        .collect();
    s.push_str(&table_owned(
        &["Setting", "Home", "Default", "What it does"],
        &setting_rows,
    ));
    s.push_str(&p(
        "<code>tasqx config edit</code> opens the same settings on a full-screen editor: up and \
         down move, enter toggles a switch or opens a theme picker, escape leaves. Moving through \
         the theme list repaints the screen in that theme <em>before</em> anything is written, \
         which is the one thing editing <code>config.toml</code> by hand cannot do. \
         <code>default_project</code> is shown there but not editable — it lives in the store and \
         is set with <code>tasqx use</code>. Piped or redirected, <code>config edit</code> refuses \
         and exits 2 instead of writing escape codes into your pipe; scripts should use \
         <code>config set</code>.",
    ));

    s.push_str(&page_close("overview"));
    s
}

/// The Overview page's "How the pieces fit" picture, drawn as inline SVG so it
/// scales with the column and takes its colours from the theme variables: no
/// asset file, and the light/dark switch repaints it like any other element.
const ARCH_SVG: &str = r##"<figure class="arch"><figcaption class="scrollhint">Scroll sideways for the whole picture →</figcaption><svg viewBox="0 0 820 244" role="img" aria-labelledby="arch-t arch-d">
<title id="arch-t">How the pieces fit</title>
<desc id="arch-d">The tasqx CLI, the report, plugins and the MCP server each send one JSON envelope over stdio; a TUI or GUI talks to the daemon over a socket or named pipe. Both paths reach the same dispatch layer, which calls storage, which writes SQLite (tasks.db plus its WAL) and the append-only events log.</desc>
<g class="wire">
<path d="M110,31 C130,31 130,94 150,94"/><path d="M110,73 C130,73 130,94 150,94"/>
<path d="M110,115 C130,115 130,94 150,94"/><path d="M110,157 C130,157 130,94 150,94"/>
<path d="M110,217 H150"/><path d="M320,94 H370"/><path d="M320,217 H375"/>
<path d="M420,200 V114"/><path d="M470,94 H510"/><path d="M610,94 H650"/>
<path d="M560,114 V202 H650"/>
</g>
<g class="head">
<path d="M142,90 L150,94 L142,98 z"/><path d="M142,213 L150,217 L142,221 z"/><path d="M362,90 L370,94 L362,98 z"/>
<path d="M367,213 L375,217 L367,221 z"/><path d="M416,122 L420,114 L424,122 z"/><path d="M502,90 L510,94 L502,98 z"/>
<path d="M642,90 L650,94 L642,98 z"/><path d="M642,198 L650,202 L642,206 z"/>
</g>
<g class="box">
<rect x="0" y="14" width="110" height="34" rx="7"/><rect x="0" y="56" width="110" height="34" rx="7"/>
<rect x="0" y="98" width="110" height="34" rx="7"/><rect x="0" y="140" width="110" height="34" rx="7"/>
<rect x="0" y="200" width="110" height="34" rx="7"/>
<rect x="150" y="74" width="170" height="40" rx="7"/><rect x="150" y="200" width="170" height="34" rx="7"/>
<rect x="375" y="200" width="90" height="34" rx="7"/><rect x="510" y="74" width="100" height="40" rx="7"/>
<rect x="650" y="60" width="170" height="68" rx="7"/><rect x="650" y="176" width="170" height="52" rx="7"/>
<rect class="hub" x="370" y="74" width="100" height="40" rx="7"/>
</g>
<g class="label">
<text x="55" y="36">tasqx CLI</text><text x="55" y="78">report</text><text x="55" y="120">plugins</text>
<text x="55" y="162">MCP</text><text x="55" y="222">TUI / GUI</text>
<text x="235" y="91">stdio</text><text class="sub" x="235" y="106">one JSON envelope</text>
<text x="235" y="222">socket / named pipe</text><text x="420" y="222">daemon</text>
<text class="hub" x="420" y="99">dispatch</text><text x="560" y="99">storage</text>
<text x="735" y="90">SQLite</text><text class="sub" x="735" y="108">tasks.db + WAL</text>
<text x="735" y="199">events</text><text class="sub" x="735" y="216">append-only log</text>
</g>
</svg></figure>"##;

// ============================================================================
// Page 2 — Install & quickstart
// ============================================================================

fn page_install() -> String {
    let mut s = page_open("install");

    s.push_str(&lead(
        "tasqx is a single static binary with no runtime and no dynamic linking. Install it \
         with a package manager, an installer script, or build it from source.",
    ));

    s.push_str(&h3("With a package manager"));
    s.push_str(&p(
        "Updates then come from <code>brew upgrade tasqx</code> / <code>scoop update tasqx</code>, \
         and brew switches Tab completion on by itself. macOS and x86-64 Linux, with Homebrew \
         (there is no prebuilt ARM Linux binary yet, so ARM Linux builds from source):",
    ));
    s.push_str(&snippet("brew install dimitritholen/tasqx/tasqx", ""));
    s.push_str(&p("Windows, with Scoop:"));
    s.push_str(&snippet(
        "scoop bucket add tasqx https://github.com/dimitritholen/scoop-tasqx\nscoop install tasqx",
        "",
    ));

    s.push_str(&h3("With the installer script"));
    s.push_str(&p("macOS and x86-64 Linux:"));
    s.push_str(&snippet(
        "curl -fsSL https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.sh | sh",
        "",
    ));
    s.push_str(&p(
        "Windows (the first statement makes older PowerShell able to download at all):",
    ));
    s.push_str(&snippet(
        "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; irm https://raw.githubusercontent.com/dimitritholen/tasqx/main/install.ps1 | iex",
        "",
    ));
    s.push_str(&p(
        "Prebuilt archives for Linux, macOS and Windows are also on the project's GitHub \
         Releases page.",
    ));

    s.push_str(&h3("Or build from source"));
    s.push_str(&snippet(
        "git clone https://github.com/dimitritholen/tasqx.git\ncd tasqx\ncargo install --path crates/tasqx-cli --force",
        "",
    ));
    s.push_str(&p(
        "Requires Rust 1.95 or newer. On Windows the MSVC toolchain is discovered \
         automatically — you do not need it on your PATH.",
    ));

    s.push_str(&h3("Sixty seconds with tasqx"));
    s.push_str(&p("Create a project, capture some work, and look at it."));

    s.push_str(&snippet(
        "tasqx init work.tasqx --desc \"The tasqx project itself\"",
        "work.tasqx\n\
         created   now your default project",
    ));

    s.push_str(&p(
        "Now capture a task. Everything after <code>add</code> is the title — except the bits \
         tasqx recognises as structure. That is the <em>inline sugar</em>: <code>+tag</code>, \
         <code>project:</code>, <code>!priority</code>, <code>due:</code>, <code>est:</code>, \
         <code>repeat:</code>, <code>remind:</code>. Whatever is left over is the title.",
    ));

    s.push_str(&snippet(
        "tasqx add \"Ship the v1 JSON API freeze +api +release project:work.tasqx !high due:friday est:4h\"",
        "▌ #1  Ship the v1 JSON API freeze\n\
         ▌ added   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h",
    ));

    s.push_str(&p(
        "The sugar was consumed; only the real title remains. Tags and a due date, no project — \
         <code>init</code> made <code>work.tasqx</code> the default, so it is filled in for you:",
    ));

    s.push_str(&snippet(
        "tasqx add \"Write the user guide +docs due:friday\"",
        "▌ #2  Write the user guide\n\
         ▌ added   - ▄▄▄▁ 9.7   work.tasqx   due Fri   +docs",
    ));

    s.push_str(&p(
        "Every field is also available as a flag, which is what you want when a value contains \
         spaces or comes from a variable:",
    ));

    s.push_str(&snippet(
        "tasqx add \"Renew the TLS cert\" project:work.tasqx +ops --due -1d",
        "▌ #3  Renew the TLS cert\n\
         ▌ added   - ▄▄▄▄ 12.0   work.tasqx   due yesterday   +ops",
    ));

    s.push_str(&warn(
        "Flags go <em>outside</em> the quoted title. <code>tasqx add \"Renew the cert --due -1d\"</code> \
         puts the literal text <code>--due -1d</code> in your title — the shell handed tasqx one \
         argument, and tasqx believed it. Quote the title, leave the flags bare.",
    ));

    s.push_str(&h3("Look at the working set"));
    s.push_str(&p(
        "<code>tasqx list</code> shows the <code>@working</code> filter: everything pending or \
         active that is not blocked, hottest first. A bare <code>tasqx</code> prints the same \
         table wherever it is not talking to a person — piped, redirected, or under \
         <code>--json</code> — and opens the <a href=\"#cli-dashboard\">dashboard</a> when it is.",
    ));
    s.push_str(&snippet(
        "tasqx list",
        "@working   3 tasks · 1 overdue\n\
         \n\
         \x20 ID          URG  TASK                         PROJECT     DUE        TAGS\n\
         \x20  1  H ▄▄▄▄ 15.7  Ship the v1 JSON API freeze  work.tasqx  Fri        +api +release\n\
         \x20  3  - ▄▄▄▄ 12.0  Renew the TLS cert           work.tasqx  yesterday  +ops\n\
         \x20  2  - ▄▄▄▁  9.7  Write the user guide         work.tasqx  Fri        +docs",
    ));

    s.push_str(&p(
        "The line above the table says which filter answered and what the answer holds beyond \
         its own size — how much of it is late, due before the day is out, running, or blocked. \
         The <code>DUE</code> cells are dated from Tuesday 14 July 2026, the day this page's \
         narrative runs on: a deadline inside the coming week is named by its weekday, one \
         further out by its date, and today and tomorrow carry a clock when the store holds one.",
    ));
    s.push_str(&p(
        "That <code>URG</code> column is urgency — a computed score, not something you set. The \
         letter in front of it is the priority you did set. <code>tasqx next</code> is the \
         \"what now\" button: the single hottest unblocked task.",
    ));
    s.push_str(&snippet(
        "tasqx next",
        "next    #1  Ship the v1 JSON API freeze\n\
         \x20       H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release\n\
         \x20       tasqx start 1  ·  tasqx why 1",
    ));

    s.push_str(&p(
        "And <code>tasqx why</code> shows its arithmetic, so the ranking is never a mystery:",
    ));
    s.push_str(&snippet(
        "tasqx why 1",
        "#1  Ship the v1 JSON API freeze\n\
         \n\
         \x20 priority   H                 6.0\n\
         \x20 deadline   due Fri           9.7\n\
         \x20 age        created today     0.0\n\
         \x20 urgency                     15.7",
    ));

    s.push_str(&h3("Work it, finish it"));
    s.push_str(&snippet(
        "tasqx start 1\ntasqx stop 1\ntasqx done 1",
        "▌ #1  Ship the v1 JSON API freeze\n\
         ▶ started   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h\n\
         ▌ #1  Ship the v1 JSON API freeze\n\
         ▌ stopped   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h\n\
         ▌ #1  Ship the v1 JSON API freeze\n\
         ▌ done today   work.tasqx   due Fri   +api +release   est 4h",
    ));

    s.push_str(&h3("Where to go next"));
    s.push_str(&table(
        &["If you want to…", "Read"],
        &[
            &[
                "Know every verb and flag",
                "<a href=\"#commands\">Commands</a>",
            ],
            &[
                "Ask precise questions of your store",
                "<a href=\"#filters\">Filter grammar</a>",
            ],
            &[
                "Say \"friday\" or \"every 3 days\"",
                "<a href=\"#scheduling\">Scheduling &amp; recurrence</a>",
            ],
            &[
                "Be told about a task before it is late",
                "<a href=\"#reminders\">Reminders</a>",
            ],
            &["Give an AI agent access", "<a href=\"#mcp\">MCP</a>"],
            &[
                "Script tasqx from another language",
                "<a href=\"#api\">JSON API</a>",
            ],
        ],
    ));

    s.push_str(&page_close("install"));
    s
}

// ============================================================================
// Page 3 — Commands
// ============================================================================

/// The Commands page: the CLI reference, one section per verb.
///
/// The body lives in [`cli_ref`], which derives every section from
/// [`crate::cmddoc::COMMAND_REF`] and clap rather than spelling verbs out here.
/// What was in this function was a hand-picked dozen verbs with hand-typed
/// output beside them, which is two drift surfaces at once: a verb could be
/// missing and a block could describe a screen this build no longer prints.
fn page_commands() -> String {
    cli_ref::page()
}

// ============================================================================
// Page 4 — Filters
// ============================================================================

fn page_filters() -> String {
    let mut s = page_open("filters");

    s.push_str(&lead(
        "One small grammar, used everywhere a query is taken: <code>list</code>, <code>report</code>, \
         <code>export</code>, <code>watch</code>, <code>task.list</code>, and the MCP tools. Learn it \
         once.",
    ));

    s.push_str(&h3("The grammar"));
    // Rendered from the parser's own const, never transcribed. The transcription
    // that used to live here drifted: it still said a tag took a bare `WORD`
    // after quoted tags shipped, while the paragraph directly below it — this
    // same page — advertised `+"needs paint"`.
    s.push_str(&pre_plain(tasqx_core::filter::GRAMMAR));

    s.push_str(&h3("Predicates"));
    s.push_str(&table(
        &["Predicate", "Matches"],
        &[
            &["<code>+api</code>", "Tasks tagged <code>api</code>."],
            &["<code>-api</code>", "Tasks <em>not</em> tagged <code>api</code>."],
            &["<code>project:work.tasqx</code>", "Exact project match."],
            &["<code>status:pending</code>", "Exact status: <code>backlog</code>, <code>pending</code>, <code>active</code>, <code>done</code>, <code>cancelled</code>."],
            &["<code>@working</code>", "Status pending or active, <em>and</em> not blocked. The default filter."],
            &["<code>@blocked</code>", "Open, with at least one dependency that is not yet done or cancelled. Also spelled <code>+blocked</code> or <code>status:blocked</code>."],
            &["<code>due.before:&lt;date&gt;</code>", "Due strictly before that instant. Takes any date <code>due:</code> takes — <code>tomorrow</code>, <code>friday</code>, <code>2026-07-25</code>, <code>eom</code>, <code>\"in 3 days\"</code>, or a full RFC3339 instant."],
            &["<code>due.after:&lt;date&gt;</code>", "Due strictly after that instant. Same date grammar."],
        ],
    ));

    s.push_str(&h3("Values with spaces"));
    s.push_str(&p(
        "A space separates predicates, so a project or tag whose name contains one must be \
         double-quoted — <code>project:\"Home Renovation\"</code>, <code>+\"needs paint\"</code>. \
         The rule is the shell's: inside quotes, spaces and parentheses are ordinary characters \
         and <code>and</code>/<code>or</code> are ordinary words, so a project named \
         <code>a (b)</code> no longer breaks the grouping. Write <code>\\\"</code> for a literal \
         quote and <code>\\\\</code> for a literal backslash. Quoting changes where a predicate \
         <em>ends</em>, not what it means: <code>\"project:x\"</code> is still a project match. The quotes must \
         REACH tasqx: on a read path the argument boundary is not enough, because the reader \
         does not guess that a space belongs inside a value rather than between two predicates. \
         Protect them from your shell — <code>'project:\"Home Renovation\"'</code>.",
    ));
    s.push_str(&p(
        "One scanner, not a filter dialect: <code>tasqx add</code> and <code>tasqx modify</code> \
         split their inline sugar with the same code the filter uses. The two sides are not \
         symmetric, though, and the difference is deliberate. The <em>write</em> side also honours \
         the argument boundary your shell drew, so <code>tasqx add \"paint\" project:Home \
         Renovation</code> files the task; the <em>read</em> side refuses the same words, because \
         there <code>project:Home Renovation</code> is equally a spaced name and a project match \
         plus a stray token, and guessing would answer with the wrong rows at exit 0. A refused \
         read costs a retype; a wrong one is unfalsifiable. So the spelling that works on both \
         sides is the quoted one, and it is the one to learn. The value that needs the escaped \
         form on both sides is a name containing a quote — \
         <code>project:\"My \\\"Big\\\" Project\"</code> — because an argument carrying a literal \
         quote is read by the scanner rather than taken whole.",
    ));
    s.push_str(&snippet(
        "tasqx list project:\"Home Renovation\" +\"needs paint\"",
        "",
    ));

    s.push_str(&h3("Combining"));
    s.push_str(&p(
        "A space is an implicit <code>and</code>. <code>or</code> and parentheses are explicit, and \
         both keywords are case-insensitive.",
    ));
    s.push_str(&snippet(
        "tasqx list \"(+api or +ops) and status:pending\"",
        "(+api or +ops) and status:pending   2 tasks · 1 overdue\n\
         \n\
         \x20 ID          URG  TASK                         PROJECT     DUE        TAGS\n\
         \x20  1  H ▄▄▄▄ 15.7  Ship the v1 JSON API freeze  work.tasqx  Fri        +api +release\n\
         \x20  3  - ▄▄▄▄ 12.0  Renew the TLS cert           work.tasqx  yesterday  +ops",
    ));

    s.push_str(&h3("Two behaviours worth knowing"));
    s.push_str(&p(
        "<strong>Dates compare as instants, not strings.</strong> <code>due.before:</code> and \
         <code>due.after:</code> parse both sides to timestamps and compare those. \
         <code>2026-07-17T00:00:00Z</code> and <code>2026-07-17T02:00:00+02:00</code> are the same \
         instant, and the filter knows it — a lexicographic comparison would not. \
         A relative bound resolves once, when the filter is parsed, so every row in one \
         query is compared against the same <code>tomorrow</code>. A date the parser cannot \
         read is refused by name rather than quietly matching nothing.",
    ));
    s.push_str(&warn(
        "<strong>Unknown tokens are rejected.</strong> A token the grammar does not recognise \
         is an error naming the token, not a term that matches everything. So \
         <code>tasqx list \"priority:H\"</code> fails — <code>priority:</code> is not a \
         predicate — instead of silently listing every task. The same goes for a dangling \
         <code>or</code> or an unclosed <code>(</code>. Values follow the same rule when the set \
         of them is <em>closed</em>: <code>status:</code> and a date bound have a fixed list of \
         accepted values, so <code>status:pendign</code> is an error naming the value and the \
         five statuses, not an empty table. A <code>project:</code> or a tag is different on \
         purpose — those names are made at runtime, so an unknown one simply matches no row.",
    ));

    s.push_str(&h3("Where the grammar stops"));
    s.push_str(&p(
        "On purpose, and permanently: no arithmetic, no computed expressions, no subqueries. \
         A filter language that grows those becomes a query language nobody can predict. \
         For anything beyond this, <a href=\"#data\">export</a> to JSON and use a real tool — \
         <code>jq</code>, a script, whatever you like. The store is yours.",
    ));

    s.push_str(&page_close("filters"));
    s
}

// ============================================================================
// Page 5 — Scheduling
// ============================================================================

fn page_scheduling() -> String {
    let mut s = page_open("scheduling");

    s.push_str(&lead(
        "Every date field — <code>due</code>, <code>scheduled</code>, <code>wait</code> — takes the \
         same natural-language grammar, through the flag or through the sugar. It resolves to \
         RFC3339 at the moment you type it, and everything is <strong>UTC</strong>: a bare DATE \
         resolves to midnight UTC, and a TIME with no offset of its own is a UTC clock — \
         <code>due:17:00</code> means 17:00 UTC whatever zone the machine is set to, and every \
         screen prints it back as 17:00. Write an offset \
         (<code>2026-07-20T17:00:00+02:00</code>) to mean another zone's clock.",
    ));

    s.push_str(&h3("The four date fields"));
    s.push_str(&table(
        &["Field", "Means"],
        &[
            &[
                "<code>due</code>",
                "The deadline. Drives urgency and is the anchor for relative reminders.",
            ],
            &[
                "<code>scheduled</code>",
                "When you intend to start. Not informational: a date still ahead holds the task \
                 in <code>backlog</code>, out of the <code>@working</code> set, until it passes.",
            ],
            &[
                "<code>wait</code>",
                "Hide this task until then — it stays out of the working set.",
            ],
            &[
                "<code>estimate</code>",
                "Not a date: an effort duration, totalled by <code>report</code>.",
            ],
        ],
    ));

    s.push_str(&h3("What you can write"));
    s.push_str(&table(
        &["Form", "Examples"],
        &[
            &["Absolute", "<code>2026-07-20</code>, <code>2026-07-20T17:00</code>, <code>\"2026-07-20 17:00\"</code>, any RFC3339"],
            &["Relative words", "<code>today</code>, <code>tomorrow</code>, <code>yesterday</code>, <code>now</code>"],
            &["Weekdays", "<code>monday</code>…<code>sunday</code>, <code>mon</code>…<code>sun</code> — <code>this</code>/<code>next</code>/<code>last</code> are refused rather than guessed"],
            &["Long offsets", "<code>\"in 3 days\"</code>, <code>\"in 2 weeks\"</code>, <code>\"in 1 month\"</code>"],
            &["Short offsets", "<code>3d</code>, <code>2w</code>, <code>1mo</code>, <code>1y</code> — signed: <code>+3d</code>, <code>-1d</code>"],
            &["Boundaries", "<code>eom</code> / <code>\"end of month\"</code>, <code>eow</code> / <code>\"end of week\"</code> (ISO week ends Sunday)"],
            &["Trailing time", "<code>\"friday 17:00\"</code>, <code>\"tomorrow 9am\"</code>, <code>\"monday 5pm\"</code>"],
            &["Leading filler", "<code>\"at 6pm\"</code>, <code>\"on friday\"</code>, <code>\"by monday 5pm\"</code> — <code>at</code>/<code>on</code>/<code>by</code>/<code>@</code> are ignored"],
        ],
    ));

    s.push_str(&h3("The rules that resolve ambiguity"));
    s.push_str(&table(
        &["Situation", "Resolution"],
        &[
            &[
                "A date with no time",
                "00:00:00 UTC — the start of that day, whatever your zone.",
            ],
            &[
                "A bare time (<code>9am</code>)",
                "09:00 UTC today, or tomorrow if that UTC time already passed.",
            ],
            &[
                "A weekday that <em>is</em> today",
                "The next one — seven days out, not zero.",
            ],
            &[
                "A naive date <em>with</em> a time",
                "A UTC clock, whatever your zone. An explicit offset (<code>+02:00</code>) is \
                 honoured and converted to UTC instead.",
            ],
            &[
                "The literal <code>now</code>",
                "This exact instant — not midnight, unlike every other keyword here. It is what \
                 <code>due.before:now</code> means, so it can find a task due earlier today.",
            ],
        ],
    ));

    s.push_str(&warn(
        "Short offsets carry <strong>day</strong>-and-larger units only: <code>d</code>, <code>w</code>, \
         <code>mo</code>, <code>y</code>. <code>-2h</code> is not a date — it is a reminder offset, and \
         a different grammar. Feeding it to <code>--due</code> is a clean error, not a guess:",
    ));
    s.push_str(&snippet(
        "tasqx add \"Overdue ping\" --due -2h",
        "error [bad_request]: could not parse date: \"-2h\" (try e.g. tomorrow, friday, 2026-07-20, \"in 3 days\" (day/week/month/year offsets only, no hours/minutes), eom, or 2026-07-20T17:00)",
    ));

    s.push_str(&h3("A leading hyphen needs no escaping"));
    s.push_str(&p(
        "<code>--due -1d</code> works. It looks like it should trip the argument parser into \
         reading <code>-1d</code> as a flag — every date-taking flag opts out of that explicitly, \
         so a signed offset is always a value:",
    ));
    s.push_str(&snippet(
        "tasqx add \"Renew the TLS cert\" project:work.tasqx +ops --due -1d",
        "▌ #3  Renew the TLS cert\n\
         ▌ added   - ▄▄▄▄ 12.0   work.tasqx   due yesterday   +ops",
    ));

    s.push_str(&h3("Estimates"));
    s.push_str(&p(
        "Human durations, parsed at the edge and stored as ISO-8601 so <code>report</code> can \
         total them. <code>4h</code>, <code>90m</code>, <code>1h30m</code>, <code>2d</code>, <code>1w</code>, \
         or ISO <code>PT4H</code> directly.",
    ));
    s.push_str(&snippet(
        "tasqx add \"Nail down the schema\" est:soon",
        "error [bad_request]: could not parse duration: \"soon\" (try e.g. 4h, 90m, 1h30m, 2d, 1w, or ISO PT4H)",
    ));

    s.push_str(&h3("Recurrence"));
    s.push_str(&p(
        "A recurring task is a <strong>template</strong>. Completing an instance spawns the next \
         one with its date advanced by the rule. Set it with <code>repeat:</code> / <code>every:</code> \
         / <code>--repeat</code>, and stop it with <code>--clear recurrence</code>.",
    ));
    s.push_str(&table(
        &["Rule", "Example"],
        &[
            &["<code>every N days|weeks|months</code>", "<code>\"every 3 days\"</code>, <code>\"every week\"</code>"],
            &["<code>weekly on &lt;days&gt;</code>", "<code>\"weekly on mon,wed,fri\"</code>"],
            &["<code>monthly on day &lt;D&gt;</code>", "<code>\"monthly on day 15\"</code>"],
            &["<code>monthly on the &lt;Nth&gt; &lt;weekday&gt;</code>", "<code>\"monthly on the 2nd tuesday\"</code>, <code>\"monthly on the last friday\"</code>"],
        ],
    ));
    s.push_str(&p(
        "This is a deliberate subset — not full RRULE. Anything outside it is a clean error.",
    ));
    s.push_str(&snippet(
        "tasqx add \"Water the plants project:home repeat:\\\"every 3 days\\\" due:today\"\ntasqx done 4\ntasqx show 5",
        "▌ #4  Water the plants\n\
         ▌ added   - ▄▄▄▃ 11.4   home   due today 23:59   ↻ every 3 days\n\
         ▌ #4  Water the plants\n\
         ▌ done today   home   due today 23:59\n\
         \x20 #5  next, due Fri\n\
         ▌ #5  Water the plants\n\
         ▌\n\
         ▌ status      pending           urgency     - ▄▄▃▁ 8.9\n\
         ▌ project     home              due         Fri (in 4 days)\n\
         ▌ repeats     every 3 days      created     today 07:51 (just now)\n\
         ▌ modified    today 07:51 (just now)\n\
         ▌ rev         1",
    ));

    s.push_str(&h3("Missed occurrences collapse"));
    s.push_str(&p(
        "If your machine was off for a week, a daily task does <em>not</em> hand you seven \
         instances. The rule advances at least once and then skips every slot at or before now, \
         so you get exactly one future instance. A backfill storm is never useful.",
    ));

    s.push_str(&h3("Month-end, precisely"));
    s.push_str(&p(
        "The two monthly forms differ on purpose, and the difference matters at month boundaries:",
    ));
    s.push_str(&table(
        &["Rule", "From Jan 31", "Why"],
        &[
            &[
                "<code>monthly on day 31</code>",
                "Jan 31 → Feb 28 → <strong>Mar 31</strong>",
                "Re-clamps against the stored target day every step, so month-end recovers.",
            ],
            &[
                "<code>every 1 month</code>",
                "Jan 31 → Feb 28 → <strong>Mar 28</strong>",
                "Advances from the previous (already clamped) date, so it drifts and stays there.",
            ],
        ],
    ));
    s.push_str(&note(
        "Pick <code>monthly on day 31</code> when you mean \"the last-ish day of every month\". \
         Pick <code>every 1 month</code> when you mean \"same slot, one month on\".",
    ));

    s.push_str(&page_close("scheduling"));
    s
}

// ============================================================================
// Page 6 — Reminders
// ============================================================================

fn page_reminders() -> String {
    let mut s = page_open("reminders");

    s.push_str(&lead(
        "tasqx is quiet by default. A task notifies you only if you gave it a <code>remind</code>, \
         and nothing else ever puts a task on the reminder heap.",
    ));

    s.push_str(&h3("The two forms"));
    s.push_str(&table(
        &["Form", "Example", "Behaviour"],
        &[
            &["Offset from <code>due</code>", "<code>-1h</code>, <code>-30m</code>, <code>-2d</code>, <code>+15m</code>", "Stays symbolic in the store. Move <code>due</code> and the reminder moves with it."],
            &["Absolute instant", "<code>\"friday 9am\"</code>, <code>2026-07-20T17:00</code>", "Resolved once, at set time, through the same date grammar."],
        ],
    ));
    s.push_str(&p(
        "The <strong>sign</strong> is what disambiguates them: a leading <code>-</code> or \
         <code>+</code> means offset, anything else goes to the date parser. Without that rule \
         <code>3d</code> would be ambiguous — \"3 days before due\" or \"in 3 days\"?",
    ));
    s.push_str(&p(
        "Offsets take <code>s</code>, <code>m</code>, <code>h</code>, <code>d</code>, <code>w</code>. \
         Negative is before due, which is what you almost always want.",
    ));

    s.push_str(&snippet(
        "tasqx modify 1 --remind -1h\ntasqx show 1",
        "▌ #1  Ship the v1 JSON API freeze\n\
         ▌ modified   remind -1h   H ▄▄▄▄ 15.7   work.tasqx   due Fri   +api +release   est 4h   rev 2\n\
         ▌ #1  Ship the v1 JSON API freeze\n\
         ▌\n\
         ▌ status      pending           urgency     H ▄▄▄▄ 15.7\n\
         ▌ project     work.tasqx        due         Fri (in 3 days)\n\
         ▌ remind      -1h               estimate    4h\n\
         ▌ tags        +api +release     created     today 07:51 (just now)\n\
         ▌ modified    today 07:51 (just now)\n\
         ▌ rev         2",
    ));
    s.push_str(&note(
        "Notice <code>remind</code> shows as <code>-1h</code>, not as a resolved timestamp. That is \
         the point: it is still an offset. Push <code>due</code> back a week and the reminder \
         follows, with no second edit.",
    ));

    s.push_str(&h3("Who delivers them"));
    s.push_str(&p(
        "The <a href=\"#daemon\">daemon</a>. It keeps an in-memory min-heap of upcoming reminder \
         instants, rebuilt from the store on start and whenever a task changes. A one-shot \
         <code>tasqx</code> command never fires anything — there would be nobody to fire it.",
    ));
    s.push_str(&snippet(
        "tasqx add \"Deploy the release\" --due \"2026-07-16T09:00\" --remind -1h\ntasqx daemon --socket tasqx-remdemo",
        "▌ #1  Deploy the release\n\
         ▌ added   no project · set a default with tasqx use <project>   - ▄▄▄▂ 10.3   due Thu\n\
         tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)\n\
         tasqx daemon: store ~/.local/share/tasqx/tasks.db\n\
         tasqx reminder: [#1] Deploy the release (due 2026-07-16T09:00:00Z)",
    ));
    s.push_str(&p(
        "That reminder had already ripened (due 09:00, minus 1h, and it was past 08:00) — so it \
         fired on the next daemon start. <strong>A reminder that ripened while the daemon was \
         down still fires, once, on the next start.</strong> Sleeping your laptop does not lose it.",
    ));

    s.push_str(&h3("It fires exactly once"));
    s.push_str(&p(
        "Firing writes a <code>reminded</code> event, and that event row <em>is</em> the dedupe \
         record. Restart the daemon and the same reminder does not come back:",
    ));
    s.push_str(&snippet(
        "tasqx daemon --socket tasqx-remdemo   # second start, same store",
        "tasqx daemon: listening on tasqx-remdemo (Ctrl-C to stop)\n\
         tasqx daemon: store ~/.local/share/tasqx/tasks.db",
    ));
    s.push_str(&p(
        "Silence — correct. The key is the (task, <em>instant</em>) pair, not just the task: \
         moving <code>due</code> moves a relative reminder to a genuinely new instant, which \
         <em>should</em> fire again.",
    ));

    s.push_str(&h3("Delivery never fails"));
    s.push_str(&p(
        "The always-compiled backend writes one line to stderr and returns. That is the \
         headless/CI-safe path: with no notification transport anywhere, delivery degrades to a \
         logged line and exit 0 — never an error.",
    ));
    s.push_str(&p(
        "Native OS toasts (Windows, macOS, Linux/D-Bus) live behind the off-by-default \
         <code>notify-os</code> build feature <em>and</em> need an explicit opt-in:",
    ));
    s.push_str(&pre_plain("# config.toml\n[notify]\nenabled = true"));
    s.push_str(&p(
        "Even then the stderr line still comes first, so the verifiable surface never depends on \
         which backend is live. Two opt-ins, both off by default — that is the quiet-by-default \
         rule taken seriously.",
    ));

    s.push_str(&h3("Firing one by hand"));
    s.push_str(&p(
        "<code>reminder.fire</code> takes a <code>ref</code> and an <code>at</code> instant, and it is \
         idempotent — the second call reports <code>fired: false</code> rather than notifying twice:",
    ));
    s.push_str(&snippet(
        "echo '{\"tasqx\":\"1\",\"id\":\"r1\",\"method\":\"reminder.fire\",\"params\":{\"ref\":\"1\",\"at\":\"2026-07-16T00:00:00Z\"}}' | tasqx api\necho '{\"tasqx\":\"1\",\"id\":\"r2\",\"method\":\"reminder.fire\",\"params\":{\"ref\":\"1\",\"at\":\"2026-07-16T00:00:00Z\"}}' | tasqx api",
        "{\"id\":\"r1\",\"ok\":true,\"result\":{\"at\":\"2026-07-16T00:00:00Z\",\"fired\":true,\"short_id\":1},\"tasqx\":\"1\"}\n\
         {\"id\":\"r2\",\"ok\":true,\"result\":{\"at\":\"2026-07-16T00:00:00Z\",\"fired\":false,\"short_id\":1},\"tasqx\":\"1\"}",
    ));
    s.push_str(&p(
        "And the event is in the log, where the dedupe check reads it:",
    ));
    s.push_str(&pre_plain(
        "{\n\
         \x20 \"actor\": \"user\",\n\
         \x20 \"entity\": \"task\",\n\
         \x20 \"op\": \"reminded\",\n\
         \x20 \"payload\": {\n\
         \x20   \"at\": \"2026-07-16T00:00:00Z\",\n\
         \x20   \"due\": \"2026-07-17T00:00:00Z\",\n\
         \x20   \"remind\": \"-1h\",\n\
         \x20   \"short_id\": 1,\n\
         \x20   \"title\": \"Deploy the release\"\n\
         \x20 },\n\
         \x20 \"ts\": \"2026-07-16T08:52:35.4429051Z\"\n\
         }",
    ));

    s.push_str(&page_close("reminders"));
    s
}

// ============================================================================
// Page 7 — Daemon & watch
// ============================================================================

fn page_daemon() -> String {
    let mut s = page_open("daemon");

    s.push_str(&lead(
        "The daemon is optional. It holds one database connection, serves the JSON API over a \
         local socket to many concurrent clients, pushes change notifications, and runs the \
         reminder scheduler. The CLI never requires it.",
    ));

    s.push_str(&h3("One-shot or daemon?"));
    s.push_str(&table(
        &["Mode", "When", "Why"],
        &[
            &[
                "One-shot",
                "The default. Scripts, cron, the HTML report.",
                "No process to manage. Open the DB, run one command, exit.",
            ],
            &[
                "Daemon",
                "Long-lived clients: a TUI, a GUI, <code>watch</code>, reminders.",
                "One writer, warm caches, live push, and something to fire reminders.",
            ],
        ],
    ));
    s.push_str(&p(
        "If a daemon is reachable, one-shot commands route through it automatically — single \
         writer, live-update semantics for free. If not, they open the store in-process. \
         <strong>Same command surface either way</strong>, and a missing or stale socket falls back \
         immediately rather than hanging. <code>--no-daemon</code> forces the in-process path.",
    ));
    s.push_str(&p(
        "Four surfaces never route through a daemon: <code>api</code>, <code>mcp serve</code>, \
         <code>chart</code> and <code>report --html</code> open the store in-process, always. \
         They refuse an explicit <code>--socket</code> rather than ignore it (D73), because a \
         flag that names a daemon and is silently discarded aims your write at a different \
         store than the one you asked for.",
    ));

    s.push_str(&h3("Socket addresses"));
    s.push_str(&p("Resolution order: <code>--socket</code>, then <code>$TASQX_SOCK</code>, then the platform default."));
    s.push_str(&table(
        &["Platform", "Default"],
        &[
            &["Windows", "The named pipe <code>tasqx-default</code>"],
            &[
                "Linux",
                "<code>$XDG_RUNTIME_DIR/tasqx/tasqx.sock</code> (falls back to the data dir)",
            ],
            &[
                "macOS",
                "<code>&lt;data dir&gt;/tasqx.sock</code> — macOS has no runtime dir",
            ],
        ],
    ));

    s.push_str(&h3("Running it"));
    s.push_str(&p(
        "Diagnostics go to stderr; the socket carries the newline-delimited JSON API. Ctrl-C \
         stops it cleanly, unwinding the accept loop and removing the socket file (a no-op for \
         Windows named pipes). <code>--db</code> points it at a specific store.",
    ));
    s.push_str(&snippet(
        "tasqx daemon --socket tasqx-docsdemo",
        "tasqx daemon: listening on tasqx-docsdemo (Ctrl-C to stop)\n\
         tasqx daemon: store ~/.local/share/tasqx/tasks.db",
    ));
    s.push_str(&p(
        "It stays until you stop it. Set <code>[daemon] idle_timeout</code> to a number of \
         minutes and it will instead exit by itself once that long has passed with no client \
         connected, no subscriber attached, no reminder about to ripen and no telemetry \
         posted to its OTLP receiver — and it says so on stderr on the way out. \
         <code>0</code>, the default, means it never does. Turning the receiver on does not \
         hold it open: only exports that actually arrive count as work. An idle exit also \
         leaves a note beside <code>config.toml</code>: the first command that then fails to \
         reach the daemon reports the retirement and which store is no longer being served, \
         because a daemon leaving changes where the same command line writes (D74).",
    ));
    s.push_str(&p(
        "Now route a command through it — note this is the ordinary <code>add</code>, unchanged:",
    ));
    s.push_str(&snippet(
        "tasqx --socket tasqx-docsdemo add \"Wire up the docs page +docs project:work.tasqx due:tomorrow\"",
        "▌ #6  Wire up the docs page\n\
         ▌ added   - ▄▄▄▃ 11.4   work.tasqx   due tomorrow   +docs",
    ));

    s.push_str(&h3("watch"));
    s.push_str(&p(
        "<code>watch</code> subscribes to a daemon and re-renders on every <code>task.changed</code> \
         push. It takes a <a href=\"#filters\">filter</a>, defaulting to the working set. It needs a \
         running daemon and will never auto-spawn one — it hints instead:",
    ));
    s.push_str(&snippet(
        "tasqx watch --socket nope",
        "tasqx watch: no daemon reachable at nope\nhint: start one with `tasqx daemon` (add `--socket nope` to match)",
    ));
    s.push_str(&p(
        "On a terminal it clears the screen and repaints the table. Through a pipe it streams one \
         line per event instead — so it composes with everything else in your shell:",
    ));
    s.push_str(&snippet(
        "tasqx watch --socket tasqx-docsdemo | cat\n# ... meanwhile, in another shell:\n#   tasqx --socket tasqx-docsdemo add \"Wire up the docs page ...\"\n#   tasqx --socket tasqx-docsdemo done 3",
        "4 tasks · 1 overdue\n\
         \n\
         \x20 ID          URG  TASK                         PROJECT     DUE        TAGS\n\
         \x20  1  H ▄▄▄▄ 15.7  Ship the v1 JSON API freeze  work.tasqx  Fri        +api +release\n\
         \x20  3  - ▄▄▄▄ 12.0  Renew the TLS cert           work.tasqx  yesterday  +ops\n\
         \x20  5  - ▄▄▃▁  8.9  Water the plants             home        Fri\n\
         \x20  2  - ▄▄▂▁  7.1  Write the user guide         work.tasqx  Mon        +docs\n\
         task.changed op=add short_id=6\n\
         task.changed op=done short_id=3",
    ));

    s.push_str(&page_close("daemon"));
    s
}

// ============================================================================
// Page 8 — MCP
// ============================================================================

/// The MCP reference — [`mcp_ref`] builds it: the setup prose, then one
/// section per tool, generated from the server's own roster (#647).
fn page_mcp() -> String {
    mcp_ref::page()
}

// ============================================================================
// Page 9 — JSON API
// ============================================================================

/// The JSON API reference — [`api_ref`] builds it: the envelope and error
/// prose, then one section per method with its parameters, its response shape
/// and a real request/response pair (#647).
fn page_api() -> String {
    api_ref::page()
}

// ============================================================================
// Page 10 — Export & import
// ============================================================================

fn page_data() -> String {
    let mut s = page_open("data");

    s.push_str(&lead(
        "Your data is yours. <code>export</code> emits canonical JSON — stable UUIDs, every field, \
         sorted keys — that is git-diffable, greppable, and round-trips exactly.",
    ));

    s.push_str(&h3("Export"));
    s.push_str(&p(
        "With no filter you get everything. With a <a href=\"#filters\">filter</a> you get a slice. \
         Human output <em>is</em> the JSON array — there is no separate pretty mode to drift from it.",
    ));
    s.push_str(&snippet(
        "tasqx export +api",
        "[\n\
         \x20 {\n\
         \x20   \"_rev\": 4,\n\
         \x20   \"annotations\": [\n\
         \x20     {\n\
         \x20       \"body\": \"Blocked on the D12 decision\",\n\
         \x20       \"created\": \"2026-07-16T08:51:09.6830568Z\",\n\
         \x20       \"id\": \"019f6a1f-62f3-75f0-bf57-e5ff9c7c452a\"\n\
         \x20     }\n\
         \x20   ],\n\
         \x20   \"completed\": null,\n\
         \x20   \"created\": \"2026-07-16T08:51:09.2509427Z\",\n\
         \x20   \"depends_on\": [],\n\
         \x20   \"due\": \"2026-07-17T00:00:00Z\",\n\
         \x20   \"estimate\": \"PT4H\",\n\
         \x20   \"id\": \"019f6a1f-6142-70d3-be5b-e28dc6060e6c\",\n\
         \x20   \"modified\": \"2026-07-16T08:51:09.6830568Z\",\n\
         \x20   \"priority\": \"H\",\n\
         \x20   \"project\": \"work.tasqx\",\n\
         \x20   \"recurrence\": null,\n\
         \x20   \"remind\": null,\n\
         \x20   \"scheduled\": null,\n\
         \x20   \"short_id\": 1,\n\
         \x20   \"status\": \"pending\",\n\
         \x20   \"tags\": [\n\
         \x20     \"api\",\n\
         \x20     \"release\"\n\
         \x20   ],\n\
         \x20   \"title\": \"Ship the v1 JSON API freeze\",\n\
         \x20   \"urgency\": 17.5,\n\
         \x20   \"wait\": null\n\
         \x20 }\n\
         ]",
    ));

    s.push_str(&h3("Filtered exports and dependency edges"));
    s.push_str(&p(
        "A filter selects a subset, so a dependency pointing <em>out</em> of that subset cannot \
         travel with it — the target is not in the document. Those edges are trimmed, and you are \
         told, on <strong>stderr</strong>:",
    ));
    s.push_str(&snippet(
        "tasqx export +docs > slice.json",
        "note: 1 dependency points outside the export, left out — widen the filter to keep it",
    ));
    s.push_str(&note(
        "The note is on stderr <em>because</em> stdout is the JSON. A note there would corrupt \
         every pipe. <code>tasqx export +docs > slice.json</code> gives you a clean file and a \
         visible warning, both.",
    ));
    s.push_str(&p(
        "The <code>--json</code> form reports the same thing as data — <code>dropped_dependencies</code> \
         is always present, and is <code>0</code> for an unfiltered export:",
    ));
    s.push_str(&snippet(
        "tasqx export +api --json",
        "{\n  \"default_project\": \"work\",\n  \"docs\": [ ... ],\n  \
         \"dropped_dependencies\": 0,\n  \
         \"projects\": [ ... ],\n  \"tasks\": [ ... ]\n}",
    ));
    s.push_str(&h3("What a document carries"));
    s.push_str(&p(
        "An export is a <strong>self-contained document</strong>, and a project is part of it \
         (D37): the <code>projects</code> array carries every project row — name, description, \
         archived state, identity — and <code>default_project</code> names the project a bare \
         <code>tasqx add</code> inherits. So are your <a href=\"#commands\">memory docs</a> \
         (D41): the <code>docs</code> array carries every knowledge document, all of them \
         regardless of any filter — a filter selects tasks, and knowledge is not attached to a \
         task. Restoring gives you back the store you exported, not just its tasks. These \
         sections are <strong>optional on import</strong>, so a file written by an older tasqx \
         still restores: with no <code>projects</code> section there is nothing to check a \
         task&rsquo;s <code>project</code> against, so the row is created from the tasks and \
         <code>import</code> says which ones it made. With one, the document is authoritative — \
         a task naming a project it does not define is refused, exactly as \
         <code>tasqx add --project</code> refuses a name no <code>init</code> ever created.",
    ));
    s.push_str(&note(
        "An import never moves your default. If the destination store already has one, the \
         document&rsquo;s is ignored and the result names the default that stands.",
    ));

    s.push_str(&h3("Import"));
    s.push_str(&p(
        "Takes a file, or <code>-</code> for stdin. It accepts either the document \
         <code>export</code> prints or a bare array of tasks (which is what every older \
         <code>export</code> wrote). Import is an \
         <strong>upsert on the UUID</strong> — re-importing the same document is a no-op, not a \
         duplicate.",
    ));
    s.push_str(&snippet(
        "tasqx import slice.json",
        "imported   2 tasks · 1 project · 3 memory docs",
    ));
    s.push_str(&snippet(
        "tasqx export +api | TASQX_DB=/tmp/other.db tasqx import -",
        "imported   1 task · 1 project · no memory docs",
    ));
    s.push_str(&note(
        "The doc count is printed even when it is zero, and a document with no <code>docs</code> \
         section at all (written by a tasqx older than D41) says so explicitly: <code>note: the \
         document carried no `docs` section, so no memory docs were restored</code>. \
         Present-and-empty and absent used to print the identical line.",
    ));

    s.push_str(&h3("A field the schema does not name is rejected"));
    // Rendered from `IMPORT_TASK_KEYS` for the same reason `sort` and `fields`
    // are rendered from their constants: a hand-typed list here would be wrong
    // the day a field is added, and nothing would fail. D15's rule.
    s.push_str(&p(&format!(
        "A task object may carry {} and nothing else; an annotation may carry <code>id</code>, \
         <code>body</code>, <code>created</code>. A key outside that set is a <code>bad_request</code> \
         naming the task and the field, because the alternative is worse than an error: a \
         misspelled <code>tags</code> used to import as <code>ok</code> with the tags silently \
         gone, so the report said your data arrived when it had not.",
        tasqx_core::engine::IMPORT_TASK_KEYS
            .iter()
            .map(|k| format!("<code>{k}</code>"))
            .collect::<Vec<_>>()
            .join(", ")
    )));
    s.push_str(&pre_plain(
        "error [bad_request]: store.import: task 019f6a0f-99df-…, unknown task field `tag`\n\
         \x20 (accepted: id, short_id, title, …) — check the spelling or drop it; it was silently\n\
         \x20 ignored before, so the import reported success and the value never arrived",
    ));
    s.push_str(&p(
        "Two of those keys are accepted and deliberately <em>ignored</em>: <code>urgency</code> is \
         recomputed on the way in (it is derived from priority, due date and age, so a supplied \
         value could contradict the ranking rule) and <code>status_unrecognized</code> is a \
         read-side flag rather than stored state. They are listed because <code>export</code> \
         emits them and a round trip has to keep working.",
    ));

    s.push_str(&h3("A dangling edge is rejected, not repaired"));
    s.push_str(&p(
        "A dependency target must be in the payload <em>or</em> already in the store. Anything \
         else fails with <code>bad_request</code> naming the id — and because the whole import is \
         one transaction, a rejection writes <strong>nothing</strong>.",
    ));
    s.push_str(&pre_plain(
        "error [bad_request]: store.import: task 019f6a0f-99df-… depends on 019f6a0f-99b5-…,\n\
         \x20 which is neither in the payload nor in the store (export the dependency too, or drop the edge)",
    ));
    s.push_str(&p(
        "That is a deliberate choice. An edge to an unknown id means the wrong slice was \
         exported; repairing it quietly would hide exactly the mistake worth seeing. Payload \
         order does not matter — tasks are written first, edges second, so a forward reference \
         is fine.",
    ));

    s.push_str(&h3("Recipes"));
    s.push_str(&table(
        &["Want", "Do"],
        &[
            &["Back up", "<code>tasqx export > backup.json</code>"],
            &["Version your tasks in git", "<code>tasqx export > tasks.json &amp;&amp; git commit -am wip</code> — canonical output means clean diffs."],
            &["Move to another machine", "<code>tasqx export > all.json</code>, copy, <code>tasqx import all.json</code>"],
            &["Hand one project to a colleague", "<code>tasqx export project:work.tasqx > slice.json</code> — mind the edge note."],
            &["Query beyond the filter grammar", "<code>tasqx export | jq '[.[] | select(.urgency > 15)]'</code>"],
        ],
    ));

    s.push_str(&page_close("data"));
    s
}

// ============================================================================
// Page 11 — Themes & reports
// ============================================================================

fn page_themes() -> String {
    let mut s = page_open("themes");

    s.push_str(&lead(
        "Default output should be something you want to look at. Themes drive the terminal and \
         the HTML report from the same palette, and degrade honestly when the terminal cannot \
         keep up.",
    ));

    s.push_str(&h3("Themes"));
    s.push_str(&p("Five built-ins. Resolution order: <code>--theme</code>, <code>$TASQX_THEME</code>, <code>config.toml</code>, default."));
    s.push_str(&snippet(
        "tasqx theme list",
        "\x20 THEME\n* nord\n  gruvbox\n  dracula\n  solarized\n  mono",
    ));
    s.push_str(&p(
        "The <code>*</code> marks the theme in effect. <code>tasqx theme show [name]</code> \
         previews every role, a sample drawn in it beside its colour and emphasis, plus \
         the urgency ramp's bands, rendered at your terminal's <em>real</em> capability. Set one permanently:",
    ));
    s.push_str(&pre_plain("# config.toml\n[theme]\nname = \"gruvbox\""));
    s.push_str(&p(
        "Drop a <code>.toml</code> in <code>$TASQX_CONFIG_DIR/themes/</code> and it appears in \
         <code>theme list</code> alongside the built-ins.",
    ));
    s.push_str(&note(
        "Capability is detected, not assumed. Pipe tasqx into <code>cat</code> and the colour goes \
         away; on a terminal without Unicode the block glyphs degrade to ASCII rather than \
         emitting mojibake. <code>mono</code> is there for when you want that unconditionally.",
    ));
    s.push_str(&note(
        "Width is detected too. <code>tasqx list</code> sizes its columns to what is actually in \
         them and to the terminal it is printing into: a column no task fills — <code>DUE</code> on \
         a store with no due dates — is not drawn at all, and the space goes to the titles. Through \
         a pipe there is no width to detect, so the table lays out for a fixed 100 columns and two \
         runs of the same store stay diffable.",
    ));
    s.push_str(&p("Four environment variables override the detection:"));
    s.push_str(&table(
        &["Variable", "Effect"],
        &[
            &["<code>NO_COLOR</code>", "Set to anything: drop all colour, keep bold/underline. Wins over everything below."],
            &["<code>CLICOLOR_FORCE</code>", "Set to anything but <code>0</code>: force colour even through a pipe — for <code>less -R</code> and CI logs."],
            &["<code>TASQX_FORCE_COLOR</code>", "Set to anything: same as <code>CLICOLOR_FORCE</code>, scoped to tasqx."],
            &["<code>COLUMNS</code>", "How many columns wide to lay tables out. Beats both the terminal's own answer and the piped default — for a multiplexer that misreports its size, or to pin the width of captured output. Clamped to 40–160."],
        ],
    ));

    s.push_str(&h3("Reports"));
    s.push_str(&p(
        "<code>tasqx report [group_by] [filter] [--all]</code> — group by <code>project</code> (default), \
         <code>status</code>, or <code>priority</code>. Estimates total as ISO-8601 durations, which \
         is why <a href=\"#scheduling\"><code>est:</code></a> is parsed at the edge rather than stored \
         as opaque text.",
    ));
    s.push_str(&p(
        "<strong>What counts.</strong> A report is an aggregation, so it leaves <em>cancelled</em> \
         tasks out — tasqx has no hard delete, and without this every task you ever threw away \
         would inflate your totals forever. <em>Done</em> tasks still count: completed work is real \
         work, and it carries nearly all your tracked time. Two ways to override that: pass \
         <code>--all</code> to count everything including cancelled, or name a status in the filter \
         — <code>tasqx report status:cancelled</code> means what it says and is taken literally.",
    ));
    s.push_str(&snippet(
        "tasqx report",
        "PROJECT               COUNT         EST  OVERDUE     TRACKED        TOKENS\n\
         home                      1        PT0S        1        PT0S             -\n\
         work.tasqx                3     PT5H30M        1        PT0S   cacheR 1.2M",
    ));
    s.push_str(&snippet(
        "tasqx report status",
        "STATUS                COUNT         EST  OVERDUE     TRACKED        TOKENS\npending                   4     PT5H30M        2        PT0S   cacheR 1.2M",
    ));
    s.push_str(&p(
        "TOKENS names the group's largest bucket with that bucket's own count — \
         <code>cacheR 1.2M</code>, or <code>-</code> when nothing was spent. The four buckets are \
         never blended into one figure; <code>--json</code> and the HTML report carry the full \
         split (D48/D50).",
    ));

    s.push_str(&h3("Charts"));
    s.push_str(&p(
        "All three read the append-only event log, so they are history, not a snapshot — and they \
         are pure reads that never touch your tasks.",
    ));
    s.push_str(&snippet(
        "tasqx chart throughput --weeks 12",
        "Weekly throughput   added [#]  done [#]\n\
         \x20 W27  added   0   done   0   net   0\n\
         \x20 W28  added   0   done   0   net   0\n\
         \x20 W29  added ##########  5   done ##  1   net  +4\n\
         \x20 > 4-wk velocity 0.2 done/wk - WIP trending up",
    ));
    s.push_str(&snippet(
        "tasqx chart heatmap --weeks 4",
        "Completions - last 4 weeks   . 0  : 1-2  + 3-4  # 5+\n\
         \x20 Mon . . . . \n\
         \x20     . . . . \n\
         \x20 Wed . . . . \n\
         \x20     . . . : \n\
         \x20 Fri . . . . \n\
         \x20     . . . . \n\
         \x20 Sun . . . . \n\
         \x20 > 1 done - current streak 1 days - best 1",
    ));
    s.push_str(&snippet(
        "tasqx chart burndown --days 7",
        "Remaining open - all tasks\n\
         \x20   4  ______#\n\
         \x20   0  2026-07-10 -> 2026-07-16\n\
         \x20 > 4 left - up 4 over 7 days",
    ));
    s.push_str(&table(
        &["Chart", "Flags"],
        &[
            &[
                "<code>throughput</code>",
                "<code>--weeks &lt;n&gt;</code> (default 12)",
            ],
            &[
                "<code>heatmap</code>",
                "<code>--weeks &lt;n&gt;</code> (default 12), <code>--year</code> (52 weeks)",
            ],
            &[
                "<code>burndown</code>",
                "<code>--days &lt;n&gt;</code> (default 30), <code>--project &lt;p&gt;</code>",
            ],
        ],
    ));

    s.push_str(&h3("The HTML report"));
    s.push_str(&p(
        "<code>tasqx report --html</code> emits a weekly review as one self-contained file — inline \
         CSS, inline SVG charts, no external requests — themed from the same palette as your \
         terminal. Exactly like the page you are reading.",
    ));
    s.push_str(&snippet(
        "tasqx report --html --out review.html",
        "Wrote self-contained HTML report → review.html",
    ));
    s.push_str(&p(
        "Without <code>--out</code> it writes to stdout. The page reads in decision order — an \
         assessment line, what needs attention (in progress, overdue, due within 7 days), what is \
         actionable now, then weekly throughput, the open backlog, token spend, the per-project \
         table, completed this week and top tags — and every panel is a pure read of the core API. \
         A search box and clickable project and tag chips filter the task lists in place, the table \
         sorts by any column, and every task id opens a detail panel with its dates, dependencies \
         and newest annotations. One small inline script does that; the page renders fully \
         without it.",
    ));
    s.push_str(&note(
        "Both HTML surfaces hold the same line: no CDN, no web fonts, no remote images, no \
         scripts fetched from anywhere. Mail the file, commit it, open it on a plane — it renders \
         the same.",
    ));

    s.push_str(&page_close("themes"));
    s
}

// ============================================================================
// Small HTML builders — every caller-supplied string goes through `esc`
// ============================================================================

/// Open a page section, titled from its [`PAGES`] row.
///
/// The title used to be passed in beside the id, which meant every page's `h2`
/// existed twice — once here, once in the table the sidebar and the prev/next
/// links read — with nothing comparing them. One source, and a page id that is
/// not in the table is a panic rather than an untitled section: both are bugs
/// in this file, and only one of them is visible.
fn page_open(id: &str) -> String {
    let title = PAGES
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("page `{id}` is not in PAGES"))
        .title
        .as_str();
    format!(
        "<section class=\"page\" id=\"{id}\"><h2>{}</h2>",
        esc(title)
    )
}

/// Close a page, appending prev/next links derived from [`PAGES`].
fn page_close(id: &str) -> String {
    let idx = PAGES.iter().position(|p| p.id == id);
    let mut links = String::new();
    if let Some(i) = idx {
        if i > 0 {
            let p = &PAGES[i - 1];
            links.push_str(&format!(
                "<a class=\"prev\" href=\"#{id}\"><span class=\"pn-k\">Previous</span>\
                 <span class=\"pn-l\">{label}</span></a>",
                id = p.id,
                label = p.label,
            ));
        }
        if i + 1 < PAGES.len() {
            let n = &PAGES[i + 1];
            links.push_str(&format!(
                "<a class=\"next\" href=\"#{id}\"><span class=\"pn-k\">Next</span>\
                 <span class=\"pn-l\">{label}</span></a>",
                id = n.id,
                label = n.label,
            ));
        }
    }
    format!("<div class=\"pagenav\">{links}</div></section>")
}

/// A page whose body came from a markdown file: the same shell every other
/// page gets, around HTML [`markdown`] already built.
fn page_markdown(md: &markdown::MdPage) -> String {
    let mut s = page_open(&md.id);
    s.push_str(&md.body);
    s.push_str(&page_close(&md.id));
    s
}

/// A lead paragraph, emitted as trusted HTML so it can carry `<code>`/`<em>` like
/// the table cells already do. Every caller is a compile-time literal in this file
/// and this page renders no store data, so there is no untrusted input to escape.
/// Escaping here silently printed the tags as text instead. Note the contrast with
/// `html.rs`, which renders task titles and must keep escaping them.
fn lead(text: &str) -> String {
    format!("<p class=\"lead\">{text}</p>")
}

/// A section heading.
fn h3(text: &str) -> String {
    let anchor: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("<h3 id=\"h-{anchor}\">{}</h3>", esc(text))
}

/// A prose paragraph. **Trusted markup**: the argument is a literal in this file,
/// never user input, so inline `<code>`/`<a>` are intentional. Nothing reaching
/// these builders comes from the store or from argv.
fn p(html: &str) -> String {
    format!("<p>{html}</p>")
}

/// A small count as an English word, for prose that states a number the code
/// owns. Panics past the table rather than dropping a digit into a sentence
/// written for a word — extending it is a one-line edit the day a roster grows
/// that far, which is cheaper than the sentence going stale unwatched.
fn count_word(n: usize) -> &'static str {
    const WORDS: [&str; 14] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
        // The thirteenth: the methods the MCP page says an agent cannot reach
        // (#647). Extending the table is the one-line edit this panic asks for.
        "thirteen",
    ];
    WORDS
        .get(n)
        .copied()
        .unwrap_or_else(|| panic!("count {n} is past the number-word table; extend it"))
}

fn note(html: &str) -> String {
    format!("<div class=\"callout note\"><span class=\"tag\">Note</span><p>{html}</p></div>")
}

fn warn(html: &str) -> String {
    format!("<div class=\"callout warn\"><span class=\"tag\">Careful</span><p>{html}</p></div>")
}

/// A command + its real output. Both are **escaped** — they are verbatim terminal
/// text, and an unescaped `<` in a snippet would be markup rather than a character.
fn snippet(cmd: &str, output: &str) -> String {
    let out = if output.is_empty() {
        String::new()
    } else {
        format!("<pre class=\"out\"><code>{}</code></pre>", esc(output))
    };
    format!(
        "<div class=\"snip\">\
           <div class=\"snip-h\"><span class=\"dollar\">$</span><button class=\"copy\" type=\"button\">Copy</button></div>\
           <pre class=\"cmd\"><code>{}</code></pre>{out}\
         </div>",
        esc(cmd),
    )
}

/// The `core.capabilities` handshake, as this build would really answer it.
///
/// Derived from [`tasqx_core::capabilities`] instead of pasted, and the reason
/// is a bug this page shipped: when `tag.remove` was added (D52) the pasted copy
/// stayed at twenty-nine methods with no `tag.remove` in either `methods` or
/// `params`. Nothing went red — [`documented_methods_match_core_capabilities`]
/// compares the [`METHODS`] *table* to the real report and never looks at this
/// string. So the one section headed "do not guess what a build supports — ask
/// it" was itself guessing, and an API author reading it would have written a
/// client that never calls `tag.remove`. A literal that no test can falsify has
/// to stop being a literal; nothing short of deriving it can rot again.
///
/// `default_project` is the single substituted value. The real answer depends on
/// whichever store the reader happens to have open, and the rest of the page is
/// written against an example store whose default project is `work.tasqx` — so
/// printing this machine's answer would be less true, not more.
fn capabilities_snippet() -> String {
    let mut caps = tasqx_core::capabilities();
    caps["default_project"] = serde_json::Value::String("work.tasqx".into());
    // D74: like `default_project`, `store` belongs to the reader's machine —
    // the page shows a plausible one rather than this build's.
    caps["store"] = serde_json::Value::String("~/.local/share/tasqx/tasks.db".into());
    serde_json::to_string(&serde_json::json!({
        "tasqx": "1",
        "id": "c1",
        "ok": true,
        "result": caps,
    }))
    .expect("a serde_json::Value always serialises")
}

/// A preformatted block with no command line (grammar, config, JSON). Escaped.
fn pre_plain(text: &str) -> String {
    format!("<pre class=\"plain\"><code>{}</code></pre>", esc(text))
}

/// A table. Headers are escaped; cells are **trusted markup** (literals in this
/// file) so they can carry `<code>` and cross-page links.
fn table(headers: &[&str], rows: &[&[&str]]) -> String {
    let owned: Vec<Vec<String>> = rows
        .iter()
        .map(|r| r.iter().map(|c| (*c).to_string()).collect())
        .collect();
    table_owned(headers, &owned)
}

/// [`table`] for rows built at runtime (the verb and method tables, generated
/// from `VERBS` / `METHODS`). Same contract: headers escaped, cells trusted.
fn table_owned(headers: &[&str], rows: &[Vec<String>]) -> String {
    let tid = next_table_id();
    let mut h = String::new();
    let mut header_ids: Vec<String> = Vec::with_capacity(headers.len());
    for (i, x) in headers.iter().enumerate() {
        let hid = format!("{tid}-h{i}");
        h.push_str(&format!("<th id=\"{hid}\">{}</th>", esc(x)));
        header_ids.push(hid);
    }
    let mut b = String::new();
    for row in rows {
        b.push_str("<tr>");
        // `data-label` is the column header a cell shows under 60rem, where a
        // row stacks into a card; `headers` is the same pairing for
        // assistive tech, which the stacked breakpoint keeps `thead` visible
        // to (PR #61 review) rather than `display: none`.
        for (i, cell) in row.iter().enumerate() {
            let label = headers.get(i).copied().unwrap_or_default();
            let hid = header_ids.get(i).map(String::as_str).unwrap_or_default();
            b.push_str(&format!(
                "<td data-label=\"{}\" headers=\"{hid}\">{cell}</td>",
                esc(label)
            ));
        }
        b.push_str("</tr>");
    }
    // The wrapper is what scrolls, so a wide table never scrolls the page.
    format!(
        "<div class=\"tw\"><table class=\"grid\"><thead><tr>{h}</tr></thead><tbody>{b}</tbody></table></div>"
    )
}

/// A fresh table id, unique within one [`generate`] call: `tbl-<n>`, reset at
/// its top. [`docs::markdown`]'s table rewrite uses its own `mdtbl-` counter
/// (its pages are built once, lazily, ahead of any particular `generate`
/// call) — the two prefixes can never collide, so the ids stay unique across
/// the whole single-file site without the two builders sharing state.
fn next_table_id() -> String {
    TABLE_SEQ.with(|c| {
        let n = c.get();
        c.set(n + 1);
        format!("tbl-{n}")
    })
}

thread_local! {
    static TABLE_SEQ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// ============================================================================
// Reference building blocks — the two-column template, tabs, params, terminal
// ============================================================================

/// An id-safe slug: lowercase alphanumerics, everything else one `-`.
///
/// Deliberately *not* [`h3`]'s anchor scheme, which maps every non-alphanumeric
/// to its own dash and so is not collapsing. Changing that would rewrite the
/// `#h-…` anchors already shipped, and the ids it produces (`install---
/// quickstart`) are only ever machine-read. New ids get the readable spelling;
/// the old ones keep the links they have.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// A two-column reference block: prose left, the code that goes with it right.
///
/// The shape every API reference the reader already knows uses, and the reason
/// is the same one: the sentence explaining a call and the call itself are read
/// together, not one after the other. `.ref-code` is `position: sticky` inside
/// its own grid row, so the example stays put while its prose scrolls and is
/// gone the moment the next section starts — a page-level sticky panel would
/// instead show the previous section's code next to this one's words.
///
/// Under 60rem the grid collapses to one column and the code follows the prose,
/// because two 20rem columns are worse than one of either.
///
/// `title` is escaped; `left_html` and `right_html` are trusted markup built by
/// the helpers above, like every other builder in this file.
fn ref_section(id: &str, title: &str, left_html: &str, right_html: &str) -> String {
    format!(
        "<section class=\"ref\" id=\"{id}\">\
           <div class=\"ref-main\"><h3 class=\"ref-h\" id=\"h-{id}\">{title}</h3>{left_html}</div>\
           <div class=\"ref-code\">{right_html}</div>\
         </section>",
        title = esc(title),
    )
}

/// A tab strip over alternative renderings of one thing: `(label, html)`.
///
/// The first tab is the one a fresh reader sees. After that the *label* — CLI,
/// JSON API, MCP — is remembered in `localStorage` and applied to every strip
/// on the page, so a reader who came for the JSON API is not re-choosing it in
/// every section. It is a label and not an index on purpose: strips do not all
/// carry the same tabs in the same order, and an index would land on whatever
/// happened to be third.
fn tabs(panels: &[(&str, &str)]) -> String {
    let mut strip = String::new();
    let mut body = String::new();
    for (i, (label, html)) in panels.iter().enumerate() {
        let on = if i == 0 { " active" } else { "" };
        let key = esc(label);
        strip.push_str(&format!(
            "<button class=\"tab{on}\" type=\"button\" data-tab=\"{key}\">{key}</button>"
        ));
        body.push_str(&format!(
            "<div class=\"tabpanel{on}\" data-tab=\"{key}\">{html}</div>"
        ));
    }
    format!("<div class=\"tabs\"><div class=\"tabstrip\">{strip}</div>{body}</div>")
}

/// A tab panel for content that does not exist yet.
///
/// Marked with a class the script reads: a remembered tab label is *not*
/// applied to a strip whose panel for that label is one of these. Otherwise one
/// click on "JSON API" would turn every code column on the page into a promise,
/// which reads as a broken page rather than an unfinished section.
fn soon(what: &str) -> String {
    format!("<div class=\"soon\"><p>{}</p></div>", esc(what))
}

/// One row of a [`param_table`].
struct Param<'a> {
    /// The parameter or flag, as the reader types it.
    name: &'a str,
    /// Its shape — `string`, `&lt;name&gt;`, `bool` — in a muted mono badge.
    ty: &'a str,
    required: bool,
    /// The value assumed when it is omitted, when there is one to state.
    default: Option<&'a str>,
    /// Trusted markup, like every other description in this file.
    html_desc: &'a str,
}

/// A parameter list: one addressable row per parameter.
///
/// Each row carries `id="<section>-<name>"`, so a section can link to a single
/// parameter the way an API reference is actually quoted ("see `--socket`"),
/// and `data-param`, which is what the sidebar search scans — a parameter is
/// the thing a reader looks for by name, and it is not a heading.
///
/// `section` is the extra argument the id scheme needs: the row cannot know
/// which block it was rendered into, and two sections may well both document a
/// `--json`.
fn param_table(section: &str, rows: &[Param]) -> String {
    let mut out = String::new();
    for r in rows {
        let name = esc(r.name);
        let pill = if r.required {
            "<span class=\"pill req\">required</span>"
        } else {
            "<span class=\"pill opt\">optional</span>"
        };
        let def = match r.default {
            Some(d) => format!(
                "<span class=\"pdef\">default <code>{}</code></span>",
                esc(d)
            ),
            None => String::new(),
        };
        // `dt`/`dd` inside a `<div>` is a valid group in a `<dl>` (the div
        // carries the id, target highlight and border; the list gives a
        // screen reader "term, definition" instead of an anonymous pair of
        // divs — PR #61 review).
        out.push_str(&format!(
            "<div class=\"param\" id=\"{section}-{slug}\" data-param=\"{name}\">\
               <dt class=\"param-h\"><code class=\"pname\">{name}</code>\
                 <span class=\"badge\">{ty}</span>{pill}{def}</dt>\
               <dd class=\"param-d\">{desc}</dd>\
             </div>",
            slug = slug(r.name),
            ty = esc(r.ty),
            desc = r.html_desc,
        ));
    }
    format!("<dl class=\"params\">{out}</dl>")
}

/// The opening tag of one [`field_list`] row, which tests split a page on.
const FIELD_ROW: &str = "<div class=\"param field\">";

/// A response field list: the [`param_table`] look for what comes back.
///
/// Each row is `(head, desc)`, both trusted markup — the head is the name, its
/// type badge, presence pill and any variant label, the desc goes under them.
/// A four-column table in the half-width reference column let a long field
/// name push the description off the edge at every width; stacked, the name
/// and the sentence each get the whole column. No id and no `data-param`: a
/// response key is not what the sidebar search is for.
fn field_list(rows: &[(String, String)]) -> String {
    let mut out = String::new();
    for (head, desc) in rows {
        out.push_str(&format!(
            "{FIELD_ROW}<dt class=\"param-h\">{head}</dt><dd class=\"param-d\">{desc}</dd></div>"
        ));
    }
    format!("<dl class=\"params\">{out}</dl>")
}

/// Split a `(flag, effect)` cell like `<code>--theme &lt;name&gt;</code>` into
/// the plain name a [`Param`] row shows and the value shape its badge carries.
///
/// The alternative was a second table spelling the two halves out beside
/// [`GLOBAL_FLAGS`], which the cmddoc guard already binds to clap — and a copy
/// of a guarded list is exactly the drift this file exists to refuse. A flag
/// with no value gets the badge `flag`, which is what it is.
fn split_flag(cell: &str) -> (String, String) {
    let plain = cell
        .replace("<code>", "")
        .replace("</code>", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    match plain.find(" <") {
        Some(i) => (plain[..i].to_string(), plain[i + 1..].to_string()),
        None => (plain, "flag".to_string()),
    }
}

/// A terminal block: the palette of a terminal, and a copy button.
///
/// [`snippet`] is the shape for *a command and the output it really produced*.
/// This is the shape for terminal text that is not that pair — the invocation
/// on its own in a code column, next to the prose describing it. `html_inner`
/// is trusted markup; callers pass [`esc`]aped terminal text.
fn term_block(html_inner: &str) -> String {
    // The button rides in a strip above the text, the same shape [`snippet`]
    // uses, rather than floating over the first line: a long line scrolling
    // under a translucent button is unreadable exactly where it matters.
    format!(
        "<div class=\"termbox\"><div class=\"snip-h\">\
           <button class=\"copy\" type=\"button\">Copy</button></div>\
         <pre class=\"term\"><code>{html_inner}</code></pre></div>"
    )
}

/// A captured screen, placed: the command that produced it, then the real
/// output — a fixture from `crates/tasqx-cli/docs-fixtures` rendered by
/// [`crate::ansi_html::render`], which is text and not a picture (D149).
///
/// ONE call, because #646 and #647 place one of these per verb and per method
/// and the wrapper is not decoration. `pre.term` sets a colour and a padding
/// and no background: a rendered screen dropped into the page bare would sit on
/// the CARD background, which is white in light mode, where `--term-fg`'s pale
/// grey is unreadable. The container is what carries `background:
/// var(--term-bg)` — dark in BOTH themes — the border, the rounded corners and
/// the block's own horizontal scroll, and `.snip` is the one that also has a
/// place to put the command line, so the table underneath can never be read as
/// the output of whatever command happened to be quoted above it.
///
/// Panics when `name` is not a captured screen, which is a build-time mistake:
/// the manifest, the directory and the embedded list are already held equal by
/// a test in [`crate::fixtures`], so a name that misses is a typo here.
fn term_screen(cmd: &str, name: &str) -> String {
    let screen = crate::fixtures::screen(name).unwrap_or_else(|| {
        panic!(
            "no captured screen named {name:?}; add a row to \
             crates/tasqx-cli/docs-fixtures/manifest.tsv and re-run \
             scripts/docs-capture.sh"
        )
    });
    format!(
        "<div class=\"snip\"><div class=\"snip-h\"><span class=\"dollar\">$</span></div>\
           <pre class=\"cmd\"><code>{}</code></pre>{}</div>",
        esc(cmd),
        crate::ansi_html::render(screen),
    )
}

// ============================================================================
// Inline CSS — light/dark, responsive, wide content scrolls in its own box
// ============================================================================

/// The light palette, and the default one: `:root` carries it, so a browser
/// with no colour preference and a document with no stored theme still gets a
/// complete set of variables.
const LIGHT_VARS: &str = r#"color-scheme: light;
--accent: #4c6ef5; --accent2: #1c7ed6; --warn: #b58900; --danger: #bf616a;
--bg: #ffffff; --fg: #1a1d23; --muted: #6b7280; --card: #f6f7f9; --line: #e3e6ea;
--code-bg: #f2f4f7; --term-bg: #23262d; --term-fg: #d8dee9;
--rail: #fbfbfc; --hover: #f0f2f5; --shadow: rgba(16, 20, 30, 0.10);
--badge-bg: #eceff3; --badge-fg: #55607a;
--pill-req: #9a5b00; --pill-req-bg: #fdf1dd;
--pill-opt: #5a6474; --pill-opt-bg: #eef0f3;
"#;

/// The dark palette. Used three times — as the `prefers-color-scheme` answer,
/// and as the explicit `[data-theme="dark"]` override — from one place, because
/// a palette maintained in three copies is a palette that disagrees with itself
/// the first time a token is added.
const DARK_VARS: &str = r#"color-scheme: dark;
--accent: #88c0d0; --accent2: #81a1c1; --warn: #ebcb8b; --danger: #bf616a;
--bg: #22262e; --fg: #d8dee9; --muted: #8b93a3; --card: #2b3039; --line: #3a4150;
--code-bg: #2b3039; --term-bg: #1b1e24; --term-fg: #d8dee9;
--rail: #1e222a; --hover: #2f3540; --shadow: rgba(0, 0, 0, 0.35);
--badge-bg: #333a46; --badge-fg: #a6b0c0;
--pill-req: #ebcb8b; --pill-req-bg: #3a3527;
--pill-opt: #9aa4b4; --pill-opt-bg: #2f3540;
"#;

/// Everything that is not a colour.
///
/// The type is tuned to be read for an hour: a 15px base at 1.55, headings with
/// tight tracking, prose capped at 46rem (a measure, not a window width) while
/// the reference grid uses the whole 78rem shell. Sizes are in `rem` so a
/// reader who has raised their browser's font size gets a bigger guide.
const RULES: &str = r##"
* { box-sizing: border-box; }
/* No `scroll-behavior: smooth`: every jump here is a page switch or an anchor
   the script also has to reason about, and an animated scroll means the hash
   target and the scroll position disagree for a third of a second. */
html { scroll-margin-top: 4.5rem; }
body { margin: 0; background: var(--bg); color: var(--fg);
  font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  font-size: 15px; line-height: 1.55; -webkit-text-size-adjust: 100%;
  -webkit-font-smoothing: antialiased; }
code, pre, .mono { font-family: ui-monospace, "Cascadia Code", "SF Mono", Consolas, "Liberation Mono", monospace; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
.muted { color: var(--muted); }
:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 4px; }

/* ---- top bar ---- */
header.top { position: sticky; top: 0; z-index: 40; height: 3.25rem;
  background: color-mix(in srgb, var(--bg) 92%, transparent); backdrop-filter: blur(8px);
  border-bottom: 1px solid var(--line); padding: 0 1.5rem;
  display: flex; align-items: center; gap: 0.9rem; }
.brand { font-weight: 700; font-size: 1.02rem; letter-spacing: -0.015em;
  min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.brand .muted { font-weight: 400; }
.ver { margin-left: auto; font-size: 0.76rem; letter-spacing: 0.01em; }
#navtoggle { display: none; background: var(--card); color: var(--fg);
  border: 1px solid var(--line); border-radius: 8px; padding: 0.3rem 0.65rem;
  font: inherit; font-size: 0.8rem; cursor: pointer; }
.themer { display: flex; gap: 0.1rem; padding: 0.15rem;
  background: var(--card); border: 1px solid var(--line); border-radius: 999px; }
.themebtn { background: transparent; border: 0; border-radius: 999px; color: var(--muted);
  font: inherit; font-size: 0.7rem; font-weight: 600; letter-spacing: 0.01em;
  padding: 0.16rem 0.5rem; cursor: pointer; }
.themebtn:hover { color: var(--fg); }
.themebtn.active { background: var(--bg); color: var(--fg); box-shadow: 0 1px 2px var(--shadow); }

/* ---- layout ---- */
.shell { display: flex; align-items: flex-start; gap: 2.5rem;
  max-width: 78rem; margin: 0 auto; padding: 0 1.5rem; }
nav { position: sticky; top: 3.25rem; flex: 0 0 15.5rem; padding: 1.4rem 0 2rem;
  max-height: calc(100vh - 3.25rem); overflow-y: auto; }
main { flex: 1 1 auto; min-width: 0; padding: 2.1rem 0 4rem; }

/* ---- sidebar ---- */
.navsearch { padding: 0 0.15rem 0.9rem; }
#navq { width: 100%; background: var(--card); color: var(--fg); border: 1px solid var(--line);
  border-radius: 8px; padding: 0.38rem 0.6rem; font: inherit; font-size: 0.82rem; }
#navq::placeholder { color: var(--muted); }
#navq:focus { border-color: var(--accent); outline: none;
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent); }
.navsec { margin: 0 0 1.05rem; }
.navsec.off { display: none; }
.navsec-h { font-size: 0.66rem; font-weight: 700; text-transform: uppercase;
  letter-spacing: 0.09em; color: var(--muted); padding: 0 0.7rem 0.3rem; }
.navsec-soon { padding: 0.2rem 0.7rem; font-size: 0.8rem; color: var(--muted); opacity: 0.7; }
nav a { display: block; padding: 0.26rem 0.7rem; border-radius: 7px;
  color: var(--fg); font-size: 0.855rem; border-left: 2px solid transparent; }
nav a:hover { background: var(--hover); text-decoration: none; }
nav a.off { display: none; }
nav a.active { background: var(--hover); border-left-color: var(--accent);
  color: var(--accent); font-weight: 600; }
.navhits { margin: 0 0 1rem; padding: 0.3rem 0; border-bottom: 1px solid var(--line); }
.navhits a.hit { padding: 0.3rem 0.7rem; font-size: 0.82rem; }
.navhits a.hit:hover { background: var(--hover); }
.hit-x { display: block; font-size: 0.68rem; color: var(--muted); }
.nohits { padding: 0.3rem 0.7rem; font-size: 0.8rem; color: var(--muted); }

/* ---- pages: JS shows one; without JS everything renders ---- */
.js .page { display: none; }
.js .page.active { display: block; animation: fade 0.18s ease-out; }
@keyframes fade { from { opacity: 0; transform: translateY(3px); } to { opacity: 1; } }
@media (prefers-reduced-motion: reduce) {
  .js .page.active { animation: none; }
}

/* ---- type: prose keeps a measure, the reference grid does not ---- */
/* Anything the hash can name clears the sticky top bar when it is jumped to. */
.page, h2, h3, h4, .ref, .param { scroll-margin-top: 4.5rem; }
.page > *:not(.ref) { max-width: 46rem; }
h2 { font-size: 1.75rem; font-weight: 600; letter-spacing: -0.022em; margin: 0 0 0.5rem; }
h3 { font-size: 1.06rem; font-weight: 600; letter-spacing: -0.012em; margin: 2.4rem 0 0.6rem;
  padding-top: 0.5rem; border-top: 1px solid var(--line); }
h4 { font-size: 0.92rem; font-weight: 600; letter-spacing: -0.008em; margin: 1.6rem 0 0.4rem; }
p { margin: 0 0 0.95rem; }
p.lead { font-size: 1.06rem; line-height: 1.6; color: var(--muted); margin-bottom: 1.6rem; }
p code, td code, li code, .param-d code { background: var(--code-bg); border: 1px solid var(--line);
  border-radius: 5px; padding: 0.05em 0.35em; font-size: 0.84em; white-space: nowrap; }
ul, ol { margin: 0 0 0.95rem; padding-left: 1.2rem; }
li { margin: 0 0 0.3rem; }

/* ---- snippets: the terminal look ---- */
.snip { margin: 0 0 1.2rem; border: 1px solid var(--line); border-radius: 10px;
  overflow: hidden; background: var(--term-bg); }
.snip-h { display: flex; align-items: center; padding: 0.35rem 0.75rem;
  border-bottom: 1px solid color-mix(in srgb, var(--term-fg) 15%, transparent); }
.snip-h .dollar { color: var(--accent2); font-family: ui-monospace, monospace;
  font-size: 0.8rem; font-weight: 700; }
.copy { margin-left: auto; background: transparent; color: var(--term-fg);
  border: 1px solid color-mix(in srgb, var(--term-fg) 25%, transparent);
  border-radius: 6px; padding: 0.1rem 0.5rem; font: inherit; font-size: 0.7rem;
  cursor: pointer; opacity: 0.7; }
.copy:hover { opacity: 1; }
.copy.ok { opacity: 1; color: var(--accent2); border-color: var(--accent2); }
/* Every wide block scrolls itself — the page never scrolls sideways. */
.snip pre { margin: 0; padding: 0.7rem 0.85rem; overflow-x: auto;
  font-size: 0.82rem; line-height: 1.5; }
.snip pre.cmd { color: var(--accent2); font-weight: 600; }
.snip pre.out { color: var(--term-fg); opacity: 0.92;
  border-top: 1px dashed color-mix(in srgb, var(--term-fg) 15%, transparent); }
pre.plain { background: var(--card); border: 1px solid var(--line); border-radius: 10px;
  padding: 0.8rem 0.9rem; margin: 0 0 1.2rem; overflow-x: auto;
  font-size: 0.82rem; line-height: 1.5; color: var(--fg); }
figure.arch { margin: 0 0 1.2rem; padding: 0.9rem; overflow-x: auto;
  background: var(--card); border: 1px solid var(--line); border-radius: 10px; }
figure.arch svg { display: block; width: 100%; min-width: 36rem; height: auto; }
/* Only where the picture is wider than its box (the 40rem query below): a
   phone draws no scrollbar, so the cut-off edge would look like the end. */
.scrollhint { display: none; position: sticky; left: 0; margin: 0 0 0.5rem;
  font-size: 0.74rem; color: var(--muted); }
figure.arch .wire path { fill: none; stroke: var(--muted); stroke-width: 1.5; }
figure.arch .head path { fill: var(--muted); }
figure.arch rect { fill: var(--bg); stroke: var(--line); stroke-width: 1.5; }
figure.arch rect.hub { stroke: var(--accent); stroke-width: 2; }
figure.arch text { fill: var(--fg); font-size: 13px; font-weight: 600; text-anchor: middle;
  font-family: ui-monospace, "Cascadia Code", "SF Mono", Consolas, "Liberation Mono", monospace; }
figure.arch text.sub { fill: var(--muted); font-size: 11px; font-weight: 400; }
figure.arch text.hub { fill: var(--accent); }
pre code { background: none; border: 0; padding: 0; white-space: pre; }
.termbox { margin: 0 0 1.2rem; border: 1px solid var(--line);
  border-radius: 10px; overflow: hidden; background: var(--term-bg); }
pre.term { margin: 0; padding: 0.75rem 0.9rem; overflow-x: auto; color: var(--term-fg);
  font-size: 0.82rem; line-height: 1.5; }

/* ---- callouts ---- */
.callout { border: 1px solid var(--line); border-left-width: 3px; border-radius: 8px;
  background: var(--card); padding: 0.75rem 0.9rem; margin: 0 0 1.2rem; }
.callout p { margin: 0.25rem 0 0; font-size: 0.9rem; }
.callout .tag { font-size: 0.66rem; font-weight: 700; text-transform: uppercase;
  letter-spacing: 0.07em; }
.callout.note { border-left-color: var(--accent); }
.callout.note .tag { color: var(--accent); }
.callout.warn { border-left-color: var(--warn); }
.callout.warn .tag { color: var(--warn); }

/* ---- tables: the wrapper scrolls, not the page ---- */
.tw { overflow-x: auto; margin: 0 0 1.2rem; border: 1px solid var(--line);
  border-radius: 10px; }
table.grid { width: 100%; border-collapse: collapse; font-size: 0.86rem;
  min-width: 26rem; }
table.grid th { text-align: left; color: var(--muted); font-weight: 600;
  font-size: 0.68rem; text-transform: uppercase; letter-spacing: 0.06em;
  background: var(--card); border-bottom: 1px solid var(--line);
  padding: 0.5rem 0.7rem; white-space: nowrap; }
table.grid td { padding: 0.5rem 0.7rem; border-bottom: 1px solid var(--line);
  vertical-align: top; }
table.grid tr:last-child td { border-bottom: 0; }

/* ---- the two-column reference block ---- */
.ref { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 27rem);
  gap: 2.5rem; align-items: start; margin: 2.4rem 0 0; padding: 0 0 0.6rem; }
.ref-main { min-width: 0; }
/* The section rule belongs to the heading, so it is the width of the prose
   column and every section on the page is ruled the same way. */
.ref-code { min-width: 0; position: sticky; top: 4.4rem; align-self: start;
  padding-top: 0.5rem; }
.ref-h { margin-top: 0; }
.ref-main > *:last-child, .ref-code > *:last-child { margin-bottom: 0; }

/* ---- code tabs ---- */
.tabs { border: 1px solid var(--line); border-radius: 10px; overflow: hidden;
  background: var(--card); margin: 0 0 1.2rem; }
.tabstrip { display: flex; flex-wrap: wrap; gap: 0.1rem; padding: 0.3rem 0.35rem;
  border-bottom: 1px solid var(--line); }
.tab { background: transparent; border: 0; border-radius: 6px; color: var(--muted);
  font: inherit; font-size: 0.74rem; font-weight: 600; letter-spacing: 0.01em;
  padding: 0.22rem 0.55rem; cursor: pointer; white-space: nowrap; }
.tab:hover { color: var(--fg); }
.tab.active { background: var(--bg); color: var(--fg); box-shadow: 0 1px 2px var(--shadow); }
.tabpanel { display: none; padding: 0.55rem; }
.tabpanel.active { display: block; }
.tabpanel > * { margin-bottom: 0.55rem; }
.tabpanel > *:last-child { margin-bottom: 0; }
.soon { border: 1px dashed var(--line); border-radius: 8px; background: var(--bg);
  padding: 0.85rem 0.9rem; color: var(--muted); font-size: 0.84rem; }
.soon p { margin: 0; }

/* ---- parameter list ---- */
.params { border: 1px solid var(--line); border-radius: 10px; overflow: hidden;
  margin: 0 0 1.2rem; }
.param { padding: 0.7rem 0.85rem; border-bottom: 1px solid var(--line); }
.param:last-child { border-bottom: 0; }
.param:target { background: var(--hover); }
.param-h { display: flex; flex-wrap: wrap; align-items: baseline; gap: 0.45rem; }
code.pname, .param-h code.fname { font-size: 0.82rem; font-weight: 600; color: var(--fg);
  background: none; border: 0; padding: 0; }
.badge { font-family: ui-monospace, "SF Mono", Consolas, monospace; font-size: 0.68rem;
  color: var(--badge-fg); background: var(--badge-bg); border-radius: 5px;
  padding: 0.05rem 0.34rem; }
.pill { font-size: 0.62rem; font-weight: 700; text-transform: uppercase;
  letter-spacing: 0.06em; border-radius: 999px; padding: 0.05rem 0.42rem; }
.pill.req { color: var(--pill-req); background: var(--pill-req-bg); }
.pill.opt { color: var(--pill-opt); background: var(--pill-opt-bg); }
.pdef { font-size: 0.72rem; color: var(--muted); }
.pdef code { font-size: 0.95em; }
.param-d { margin-top: 0.32rem; font-size: 0.86rem; }
.param-d p { margin: 0 0 0.4rem; }
.param-d p:last-child { margin-bottom: 0; }

/* ---- page nav ---- */
.pagenav { display: flex; gap: 1rem; margin-top: 3rem; padding-top: 1rem;
  border-top: 1px solid var(--line); }
.pagenav a { display: block; flex: 0 1 18rem; padding: 0.55rem 0.8rem;
  border: 1px solid var(--line); border-radius: 10px; color: var(--fg); }
.pagenav a:hover { background: var(--hover); text-decoration: none; border-color: var(--accent); }
.pagenav .next { margin-left: auto; text-align: right; }
.pn-k { display: block; font-size: 0.66rem; font-weight: 700; text-transform: uppercase;
  letter-spacing: 0.08em; color: var(--muted); }
.pn-l { display: block; font-size: 0.9rem; font-weight: 600; color: var(--accent); }
footer { max-width: 78rem; margin: 0 auto; padding: 1.5rem 1.5rem 3rem;
  border-top: 1px solid var(--line); color: var(--muted); font-size: 0.78rem; }

/* ---- responsive: the sidebar becomes a drawer ---- */
@media (max-width: 60rem) {
  /* `align-items: flex-start` sizes a COLUMN flex container's items to their
     content, so `main` came out 46rem wide on a 390px screen and took the
     whole document off the side of it. In one column they stretch. */
  .shell { flex-direction: column; align-items: stretch; gap: 0; padding: 0 1.1rem; }
  header.top { padding: 0 1.1rem; gap: 0.7rem; }
  #navtoggle { display: block; }
  .ver { display: none; }
  .themer { margin-left: auto; }
  nav { position: fixed; top: 3.25rem; left: 0; right: 0; bottom: 0; z-index: 30;
    flex: none; width: 100%; max-height: none; display: none;
    background: var(--rail); border-bottom: 1px solid var(--line);
    padding: 1rem 1.1rem 2.5rem; overflow-y: auto; }
  nav.open { display: block; }
  main { padding: 1.4rem 0 3rem; }
  h2 { font-size: 1.45rem; }
  .ref { grid-template-columns: minmax(0, 1fr); gap: 1.1rem;
    margin-top: 2rem; padding-top: 1.1rem; }
  .ref-code { position: static; }
  .pagenav { flex-direction: column; }
  .pagenav .next { margin-left: 0; }
  .pagenav a { flex: 1 1 auto; }
  /* A grid table stacks into one card per row: side by side, the last column
     (usually the sentence) sat off-screen and every row was a tall strip of
     mostly empty cells. Each cell names its column from `data-label`, which
     `table_owned` and the markdown table rewrite put on every `td`. */
  table.grid { min-width: 0; }
  /* Visually hidden, not `display: none`: a screen reader still has the
     header row to announce, even though the sighted layout below gets its
     labels from `data-label` instead (PR #61 review). */
  table.grid thead { position: absolute; width: 1px; height: 1px; overflow: hidden;
    clip: rect(0 0 0 0); clip-path: inset(50%); white-space: nowrap; }
  table.grid, table.grid tbody, table.grid tr, table.grid td { display: block; }
  table.grid tr { padding: 0.55rem 0.8rem; border-bottom: 1px solid var(--line); }
  table.grid tbody tr:last-child { border-bottom: 0; }
  table.grid td { padding: 0.1rem 0; border-bottom: 0; }
  table.grid td::before { content: attr(data-label); display: block; color: var(--muted);
    font-size: 0.62rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em; }
  /* A card is the column's width, so a long path wraps rather than runs off. */
  table.grid td code { white-space: normal; overflow-wrap: anywhere; }
}

@media (max-width: 40rem) {
  .scrollhint { display: block; }
}

/* ---- and on a small phone, the top bar sheds what it can spare ----
   At 320px the bar is Menu + brand + a three-way theme switch, and the brand
   was the only part that could shrink: it wrapped to three lines and spilled
   out of the fixed 3.25rem the sticky offsets below it are measured from. The
   subtitle goes instead. The switch keeps its three labels, because a single
   cycling button costs the reader a guess at what the next press does. */
@media (max-width: 30rem) {
  .shell { padding: 0 1rem; }
  header.top { padding: 0 1rem; gap: 0.6rem; }
  .brand .muted { display: none; }
  .themebtn { padding: 0.16rem 0.42rem; }
}
"##;

fn css() -> String {
    // A system-font stack: no web font can be requested, so none can be missing.
    //
    // Three palettes from two blocks: `:root` is light, the media query is the
    // reader's system preference, and `[data-theme]` — set only by the header's
    // switch — overrides both. The media query stays the default on purpose, so
    // "Auto" is the absence of an override rather than a mode of its own.
    let mut s = String::new();
    s.push_str(":root {\n");
    s.push_str(LIGHT_VARS);
    s.push_str("}\n@media (prefers-color-scheme: dark) {\n:root {\n");
    s.push_str(DARK_VARS);
    s.push_str("}\n}\n");
    s.push_str("html[data-theme=\"light\"] {\n");
    s.push_str(LIGHT_VARS);
    s.push_str("}\n");
    s.push_str("html[data-theme=\"dark\"] {\n");
    s.push_str(DARK_VARS);
    s.push_str("}\n");
    s.push_str(RULES);
    // The JSON blocks the API reference renders carry their own four colours
    // and a height cap; they live beside the code that emits the spans (#647).
    s.push_str(api_ref::CSS);
    s
}

/// One captured screen as a standalone page: the site's terminal styling, a
/// dark background, and nothing else — no header, no sidebar, no script.
///
/// This is the README's rasterisation path (`scripts/snap.sh`). GitHub's
/// markdown cannot carry a styled span, so those pictures stay raster — but
/// they are rasterised from the SAME renderer the site reads, rather than from
/// a second toolchain: `freeze` ignored SGR 39 and drew bold at normal weight,
/// so a picture taken that way disagreed with the bytes the binary printed
/// (D149, and the two defects `ansi_html` has a unit test for).
///
/// The stylesheet is the site's own consts and no copy of them: [`DARK_VARS`]
/// for the palette — always the dark one, because a terminal is dark in both
/// site themes — and [`RULES`] for `pre.term`. Three overrides follow them,
/// and only these three: the page IS the screen, so it carries `--term-bg`,
/// is sized to its content (a screenshot has no window to fill), and does not
/// scroll — a scroll container in a screenshot is a cropped screen.
///
/// `None` when nothing was captured under `name`; the caller lists
/// [`crate::fixtures::names`] and exits 2.
pub(crate) fn screen_page(name: &str) -> Option<String> {
    let screen = crate::fixtures::screen(name)?;
    Some(format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>tasqx — {}</title>\n<style>\n:root {{\n{DARK_VARS}}}\n{RULES}\n\
         /* The page IS the screen: content-sized, unscrolled, nothing around it. */\n\
         html, body {{ background: var(--term-bg); }}\n\
         body {{ margin: 0; width: max-content; }}\n\
         pre.term {{ width: max-content; overflow: visible; padding: 0.9rem 1.1rem; }}\n\
         </style>\n</head>\n<body>\n{}\n</body>\n</html>\n",
        esc(name),
        crate::ansi_html::render(screen),
    ))
}

// ============================================================================
// Inline JS — client-side page switching, no framework, no external anything
// ============================================================================

/// Written defensively: if anything here throws, the `js` class is never added
/// and the document degrades to one long readable page rather than a blank one.
///
/// Navigation is driven by `location.hash` and the `hashchange` event, and NOT
/// by `history.pushState`. That is load-bearing, not stylistic: this file is
/// opened over `file://`, whose origin is `null`, and `pushState` throws a
/// SecurityError there. Letting each `<a>` do its ordinary default thing sets
/// the hash, fires `hashchange`, and gives us real history entries — so the
/// back button and deep links work on the exact transport `tasqx docs` uses.
///
/// `localStorage` is reached through `readStore`/`writeStore` and nothing else.
/// On a `file://` document with a null origin, or under a privacy setting that
/// refuses storage, merely *touching* `window.localStorage` throws — so a
/// remembered theme must never be able to take the page down with it.
///
/// The search index is the document: `querySelectorAll` over the headings and
/// parameter rows already in the file. There is nothing to build, nothing to
/// fetch, and nothing that can disagree with what is on the page.
const SCRIPT: &str = r##"(function () {
  function list(sel, root) {
    return Array.prototype.slice.call((root || document).querySelectorAll(sel));
  }
  var pages = list('.page');
  if (!pages.length) { return; }
  var navEl = document.getElementById('nav');
  var tree = document.getElementById('navtree');
  var hits = document.getElementById('navhits');
  var queryBox = document.getElementById('navq');
  var navLinks = list('nav a[data-page]');
  var THEME_KEY = 'tasqx-docs-theme';
  var TAB_KEY = 'tasqx-docs-tab';

  // Only hide pages once we know we can show them again.
  document.documentElement.classList.add('js');

  function readStore(k) {
    try { return window.localStorage.getItem(k); } catch (e) { return null; }
  }
  function writeStore(k, v) {
    try { window.localStorage.setItem(k, v); } catch (e) { /* storage refused */ }
  }

  // ---- theme: light / dark / system ---------------------------------------
  var themeBtns = list('.themebtn');
  function applyTheme(mode) {
    var root = document.documentElement;
    if (mode === 'light' || mode === 'dark') {
      root.setAttribute('data-theme', mode);
    } else {
      root.removeAttribute('data-theme');
      mode = 'system';
    }
    themeBtns.forEach(function (b) {
      var on = b.getAttribute('data-theme-set') === mode;
      b.classList.toggle('active', on);
      b.setAttribute('aria-pressed', on ? 'true' : 'false');
    });
  }
  applyTheme(readStore(THEME_KEY) || 'system');

  // ---- code tabs -----------------------------------------------------------
  function applyTab(box, label) {
    var panels = list('.tabpanel', box), found = null;
    panels.forEach(function (p) {
      if (p.getAttribute('data-tab') === label) { found = p; }
    });
    if (!found) { return false; }
    panels.forEach(function (p) { p.classList.toggle('active', p === found); });
    list('.tab', box).forEach(function (b) {
      b.classList.toggle('active', b.getAttribute('data-tab') === label);
    });
    return true;
  }
  var storedTab = readStore(TAB_KEY);
  if (storedTab) {
    list('.tabs').forEach(function (box) {
      var panels = list('.tabpanel', box), wanted = null;
      panels.forEach(function (p) {
        if (p.getAttribute('data-tab') === storedTab) { wanted = p; }
      });
      // A remembered label whose panel is a placeholder is not applied: the
      // reader would land on "coming soon" in every section at once.
      if (wanted && !wanted.querySelector('.soon')) { applyTab(box, storedTab); }
    });
  }

  // ---- one page at a time, driven by the hash ------------------------------
  function pageFor(id) {
    var direct = null;
    pages.forEach(function (p) { if (p.id === id) { direct = p; } });
    if (direct) { return { page: direct, target: null }; }
    var el = id ? document.getElementById(id) : null;
    var owner = el && el.closest ? el.closest('.page') : null;
    return { page: owner, target: owner ? el : null };
  }
  function show(id) {
    pages.forEach(function (p) { p.classList.toggle('active', p.id === id); });
    navLinks.forEach(function (a) {
      a.classList.toggle('active', a.getAttribute('data-page') === id);
    });
    if (navEl) { navEl.classList.remove('open'); }
  }
  function go(hash) {
    var hit = pageFor(hash);
    var page = hit.page || pages[0];
    show(page.id);
    // A heading or a parameter row inside a page that was hidden a moment ago:
    // reveal the page first, then scroll, or the browser measures nothing.
    if (hit.target) { hit.target.scrollIntoView(); } else { window.scrollTo(0, 0); }
  }

  // The browser sets the hash for us; we only react. Nav links, prev/next,
  // search hits and cross-references inside prose therefore all take one path.
  window.addEventListener('hashchange', function () { go(location.hash.slice(1)); });

  // ---- sidebar search ------------------------------------------------------
  // No index: the document is the index. Sidebar entries filter by label, and a
  // typed query also lists every matching heading and parameter across pages.
  var HIT_LIMIT = 14;
  function labelOfPage(page) {
    if (!page) { return ''; }
    var a = null;
    navLinks.forEach(function (x) {
      if (x.getAttribute('data-page') === page.id) { a = x; }
    });
    return a ? a.textContent : page.id;
  }
  function clearHits() {
    while (hits.firstChild) { hits.removeChild(hits.firstChild); }
  }
  function search() {
    var text = (queryBox.value || '').trim().toLowerCase();
    clearHits();
    if (!text) {
      hits.hidden = true;
      navLinks.forEach(function (a) { a.classList.remove('off'); });
      list('.navsec', tree).forEach(function (s) { s.classList.remove('off'); });
      return;
    }
    navLinks.forEach(function (a) {
      a.classList.toggle('off', a.textContent.toLowerCase().indexOf(text) < 0);
    });
    // A section with nothing left showing goes too — including the empty ones,
    // whose "coming soon" is an answer to nobody's query.
    list('.navsec', tree).forEach(function (s) {
      s.classList.toggle('off', !list('a[data-page]', s).some(function (a) {
        return !a.classList.contains('off');
      }));
    });
    var n = 0;
    list('.page h3, .page h4, .page [data-param]').forEach(function (el) {
      if (n >= HIT_LIMIT) { return; }
      var name = el.getAttribute('data-param') || el.textContent;
      if (name.toLowerCase().indexOf(text) < 0) { return; }
      var page = el.closest ? el.closest('.page') : null;
      var id = el.id || (page ? page.id : '');
      if (!id) { return; }
      n++;
      var a = document.createElement('a');
      a.className = 'hit';
      a.setAttribute('href', '#' + id);
      a.textContent = name;
      var where = document.createElement('span');
      where.className = 'hit-x';
      where.textContent = labelOfPage(page);
      a.appendChild(where);
      hits.appendChild(a);
    });
    if (!n) {
      var none = document.createElement('div');
      none.className = 'nohits';
      none.textContent = 'No matches';
      hits.appendChild(none);
    }
    hits.hidden = false;
  }
  if (queryBox && hits && tree) {
    queryBox.addEventListener('input', search);
    queryBox.addEventListener('search', search);
  }

  // ---- one click handler for the whole document ----------------------------
  document.addEventListener('click', function (e) {
    var t = e.target;
    if (!t || !t.classList) { return; }

    if (t.classList.contains('themebtn')) {
      var mode = t.getAttribute('data-theme-set');
      applyTheme(mode);
      writeStore(THEME_KEY, mode);
      return;
    }
    if (t.classList.contains('tab')) {
      var box = t.closest ? t.closest('.tabs') : null;
      var label = t.getAttribute('data-tab');
      if (box && applyTab(box, label)) { writeStore(TAB_KEY, label); }
      return;
    }
    if (t.classList.contains('copy')) { copyFrom(t); return; }
    if (t.id === 'navtoggle') {
      e.stopPropagation();
      if (navEl) {
        var open = navEl.classList.toggle('open');
        t.setAttribute('aria-expanded', open ? 'true' : 'false');
      }
      return;
    }
    // Clicking the page you are already on fires no hashchange; close the
    // drawer anyway so the tap is not a no-op.
    var link = t.closest ? t.closest('a[href^="#"]') : null;
    if (link && navEl) { navEl.classList.remove('open'); }
  });

  // ---- copy buttons: clipboard where available, a textarea where not -------
  function copyFrom(btn) {
    var box = btn.closest ? btn.closest('.snip, .termbox') : null;
    if (!box) { return; }
    var node = box.querySelector('pre.cmd code') || box.querySelector('pre.term');
    if (!node) { return; }
    var text = node.textContent;
    function done() {
      btn.textContent = 'Copied';
      btn.classList.add('ok');
      setTimeout(function () {
        btn.textContent = 'Copy';
        btn.classList.remove('ok');
      }, 1200);
    }
    function fallback() {
      try {
        var ta = document.createElement('textarea');
        ta.value = text;
        ta.setAttribute('readonly', '');
        ta.style.position = 'fixed';
        ta.style.top = '-1000px';
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
        done();
      } catch (err) { /* nothing left to try; the text is on screen */ }
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, fallback);
    } else {
      fallback();
    }
  }

  go(location.hash.slice(1) || pages[0].id);
}());"##;

fn js() -> String {
    String::from(SCRIPT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    // ---- doc-drift guards ---------------------------------------------------
    //
    // The cheapest honest guard available: the docs render FROM these lists, and
    // each list is asserted equal to the real surface it claims to describe. A
    // new verb or method cannot ship undocumented, because the CLI's own tables
    // are the assertion.

    /// THE drift guard. `VERBS` is what the Commands page renders; clap's
    /// subcommand table is the truth. Adding a verb without documenting it, or
    /// documenting one that does not exist, fails here.
    #[test]
    fn documented_verbs_match_the_cli_surface() {
        let mut real: Vec<String> = crate::Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        let mut documented: Vec<String> =
            documented_verbs().iter().map(|s| s.to_string()).collect();
        real.sort();
        documented.sort();

        let undocumented: Vec<&String> = real.iter().filter(|v| !documented.contains(v)).collect();
        assert!(
            undocumented.is_empty(),
            "these CLI verbs exist but `tasqx docs` does not document them: {undocumented:?}"
        );
        let invented: Vec<&String> = documented.iter().filter(|v| !real.contains(v)).collect();
        assert!(
            invented.is_empty(),
            "`tasqx docs` documents these verbs, but the CLI has no such subcommand: {invented:?}"
        );
        assert_eq!(real, documented);
    }

    /// The HTML verb table and the terminal registry may not disagree on the
    /// structural fields: verb/aliases/method are asserted equal here. Prose is
    /// no longer comparable because it is no longer duplicated — the page
    /// renders `cmddoc`'s summary directly (see [`VERBS`]).
    #[test]
    fn html_verbs_agree_with_cmddoc() {
        use crate::cmddoc::COMMAND_REF;
        for (verb, aliases_html, method) in VERBS {
            let d = COMMAND_REF
                .iter()
                .find(|d| d.verb == verb)
                .unwrap_or_else(|| panic!("VERBS has `{verb}`, cmddoc does not"));
            assert_eq!(d.method, method, "method drift on `{verb}`");
            let mut html_aliases: Vec<String> = if aliases_html == "—" {
                vec![]
            } else {
                aliases_html
                    .split(',')
                    .map(|a| a.trim().replace("<code>", "").replace("</code>", ""))
                    .collect()
            };
            let mut ours: Vec<String> = d.aliases.iter().map(|s| s.to_string()).collect();
            html_aliases.sort();
            ours.sort();
            assert_eq!(
                html_aliases, ours,
                "alias drift (html vs cmddoc) on `{verb}`"
            );
        }
        // reverse direction: every cmddoc verb (incl. `manual`) must be documented
        // in the HTML guide too.
        for d in COMMAND_REF {
            assert!(
                VERBS.iter().any(|(v, ..)| *v == d.verb),
                "cmddoc verb `{}` missing from the HTML VERBS table",
                d.verb
            );
        }
    }

    /// Aliases are part of the documented surface too: the table claims specific
    /// aliases per verb, and clap knows the real ones.
    #[test]
    fn documented_aliases_match_the_cli_surface() {
        let cmd = crate::Cli::command();
        for (verb, aliases, _) in VERBS {
            let sub = cmd
                .get_subcommands()
                .find(|c| c.get_name() == verb)
                .unwrap_or_else(|| panic!("no such subcommand: {verb}"));
            let mut real: Vec<String> = sub.get_all_aliases().map(|a| a.to_string()).collect();
            real.sort();

            // "—" is the table's way of saying "no aliases".
            let mut claimed: Vec<String> = if aliases == "—" {
                vec![]
            } else {
                aliases
                    .split(',')
                    .map(|a| a.trim().replace("<code>", "").replace("</code>", ""))
                    .collect()
            };
            claimed.sort();
            assert_eq!(real, claimed, "alias drift on verb `{verb}`");
        }
    }

    /// Every documented verb must actually reach the rendered page.
    #[test]
    fn every_documented_verb_appears_on_the_commands_page() {
        let doc = generate();
        for verb in documented_verbs() {
            assert!(
                doc.contains(&format!("<code>{verb}</code>")),
                "verb `{verb}` is in the VERBS table but never rendered onto the page"
            );
        }
    }

    /// Strip a `params`/`returns` cell down to the bare identifiers it names, so
    /// a test can compare the documented parameter names against the engine.
    /// `<code>x</code>, <code>y?</code>` → `["x", "y?"]`; the em-dash "no
    /// params" marker yields an empty list.
    #[cfg(test)]
    fn param_names(cell: &str) -> Vec<String> {
        cell.replace("<code>", "")
            .replace("</code>", "")
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && p != "—")
            .collect()
    }

    /// The `params` column claims which arguments each JSON API method takes and
    /// which of them are mandatory (no trailing `?`). Nothing checked either.
    ///
    /// Two real failures this guards, both silent today:
    ///
    /// 1. A row claims a **required** parameter the engine no longer demands.
    ///    Found exactly this on `task.list` (`filter`) and `report.summary`
    ///    (`group_by`) — both are optional in the engine and were documented as
    ///    required, sending readers hunting for an argument they do not need.
    /// 2. A row claims a required parameter under a **name the engine renamed**.
    ///    The engine's own "missing required field: X" message names the field
    ///    it wanted, so calling each method with `{}` and matching that message
    ///    against the documented name pins the spelling, not just the count.
    ///
    /// What this does NOT cover, stated plainly: the *optional* parameters
    /// (`project?`, `sort?`, `expected_rev?`, …) are unguarded. The engine reads
    /// them with `opt_str`-style lookups that succeed by ignoring anything they
    /// do not recognise, so a renamed optional param produces no error to
    /// observe — proving those names would mean a real call per parameter with a
    /// value round-tripped back out, which is an integration suite, not a
    /// doc-drift guard.
    /// An in-memory store whose newest event is undoable.
    ///
    /// The two guards below probe every method with `{}` and read the answer as
    /// a statement about its PARAMS. `event.revert` is the one method whose bare
    /// call can fail for a reason that has nothing to do with params — an empty
    /// log has nothing to undo — so on a virgin store it would look like a
    /// method with an undocumented required argument. Seeding one undoable event
    /// removes that confound without weakening either guard: no other method in
    /// [`METHODS`] can be called bare AND append an event, so the `tag.remove`
    /// left here is still the newest event when the loop reaches `event.revert`.
    #[cfg(test)]
    fn probe_engine() -> tasqx_core::Engine {
        let e = tasqx_core::Engine::open_in_memory().expect("in-memory store");
        let sid = e
            .task_add(&serde_json::json!({ "title": "probe" }))
            .expect("seed a task")["short_id"]
            .clone();
        e.tag_add(&serde_json::json!({ "ref": sid.clone(), "tags": ["probe"] }))
            .expect("seed a tag");
        e.tag_remove(&serde_json::json!({ "ref": sid, "tags": ["probe"] }))
            .expect("seed an undoable event");
        e
    }

    #[test]
    fn documented_required_params_match_what_the_engine_demands() {
        let e = probe_engine();

        for (method, params, _returns) in METHODS {
            let names = param_names(params);
            let required: Vec<&String> = names.iter().filter(|p| !p.ends_with('?')).collect();
            let outcome = tasqx_core::dispatch(&e, method, &serde_json::json!({}));

            match required.first() {
                // The table says every argument is optional. That is a checkable
                // claim: the method must succeed with no arguments at all.
                None => assert!(
                    outcome.is_ok(),
                    "`{method}` documents no required params, but rejects an empty call: {:?}",
                    outcome.err()
                ),
                // The table names a mandatory argument. The engine must refuse
                // the empty call *and* name that same argument when it does.
                Some(first) => {
                    let err = outcome.err().unwrap_or_else(|| {
                        panic!("`{method}` documents required param `{first}`, but an empty call succeeded")
                    });
                    assert!(
                        err.message.contains(first.as_str()),
                        "`{method}` documents required param `{first}`, but the engine's \
                         complaint names something else: {}",
                        err.message
                    );
                }
            }
        }
    }

    /// The `returns` column describes each method's response shape. Where it
    /// spells the shape as an explicit `{a, b}` brace list, those really must be
    /// top-level keys of the real response.
    ///
    /// Partial by construction, and worth naming precisely: this only covers the
    /// methods callable with no arguments, because those are the ones a
    /// doc-drift test can invoke without inventing fixture data. That is seven
    /// of the [`METHODS`] rows. The write methods' return shapes, and every prose
    /// `returns` cell that describes rather than enumerates ("The task, timer
    /// running."), stay unguarded — asserting on English is not a thing a test
    /// can do, and asserting on the write shapes needs a fixture store per
    /// method.
    ///
    /// The failure it does catch: a renamed response key. `{count, tasks}`
    /// becoming `{count, rows}` in the engine leaves the guide confidently
    /// telling every API client to read a field that is no longer there.
    #[test]
    fn documented_return_shapes_match_the_real_response_where_checkable() {
        let e = probe_engine();
        let mut checked = 0;

        for (method, params, returns) in METHODS {
            // Only the no-required-params methods are callable here.
            if param_names(params).iter().any(|p| !p.ends_with('?')) {
                continue;
            }
            // Only the rows that enumerate a shape, e.g. "<code>{count, tasks}</code>".
            let Some(open) = returns.find('{') else {
                continue;
            };
            let Some(close) = returns[open..].find('}') else {
                continue;
            };
            let keys = param_names(&returns[open + 1..open + close]);
            if keys.is_empty() {
                continue;
            }

            let result = tasqx_core::dispatch(&e, method, &serde_json::json!({}))
                .unwrap_or_else(|err| panic!("`{method}` should be callable bare: {err:?}"));
            for key in &keys {
                assert!(
                    result.get(key).is_some(),
                    "the guide says `{method}` returns `{key}`, but the response has no such \
                     top-level key: {result}"
                );
            }
            checked += 1;
        }

        // Pin the coverage claim itself. If a future edit makes this loop skip
        // everything, the test would pass while guarding nothing. Re-derive from
        // the count this guard reports rather than adding the rows you wrote:
        // it went 7 -> 8 when `event.revert` joined, 8 -> 9 when `otlp.status`
        // (#222) did, 9 -> 10 when `memory.list` (#133) did, 10 -> 11 when
        // `report.outcomes` (D137) did, and a floor that drifts below the
        // truth is a guard that has stopped guarding.
        assert_eq!(
            checked, 11,
            "expected to check all 11 bare-callable return shapes; a row that stopped being \
             checkable is coverage lost silently"
        );
    }

    /// D128: the guide's `pick` page carried D55's rule — that leaving exits 4
    /// — for the whole life of the browser D124 made of that chooser. Both
    /// halves are asserted, because dropping "exit 4" altogether would take
    /// the empty-set refusal with it, and that one has not moved.
    #[test]
    fn the_pick_page_says_closing_the_browser_is_exit_0() {
        // Flattened: the paragraph is one string whose line breaks are the
        // source file's continuations, not the page's.
        let flat = page_commands()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            flat.contains("Leaving without starting a task exits 0"),
            "the guide's pick page must say leaving is exit 0"
        );
        assert!(
            !flat.contains("Leaving without starting a task, or a filter"),
            "the guide's pick page still states D55's rule"
        );
        assert!(
            flat.contains("still exits 4"),
            "the guide's pick page must keep the empty-set refusal non-zero"
        );
    }

    /// The Commands page's description column is rendered from `cmddoc`, not
    /// stored in [`VERBS`]. That single-sourcing is the whole point of deleting
    /// the old prose column, so it needs a guard of its own.
    ///
    /// The failure: [`verb_summary`] returns `""` for a verb `cmddoc` does not
    /// know, deliberately (the generator must not panic over a doc gap). Without
    /// this test that fallback is invisible — the page renders with a blank
    /// column and every other guard still passes, because they all check verb
    /// names and never the prose. This asserts the terminal's exact string
    /// reaches the page, which is only possible while there is one copy of it.
    #[test]
    fn the_commands_page_shows_the_same_summary_as_the_terminal() {
        let doc = generate();
        for verb in documented_verbs() {
            let summary = verb_summary(verb);
            assert!(
                !summary.is_empty(),
                "verb `{verb}` renders an empty description — cmddoc has no summary for it"
            );
            assert!(
                doc.contains(&api_ref::describe(summary)),
                "verb `{verb}`'s -h summary ({summary:?}) never reaches the Commands page"
            );
        }
    }

    /// The API page's method table against the core's own capability report.
    #[test]
    fn documented_methods_match_core_capabilities() {
        let caps = tasqx_core::capabilities();
        let mut real: Vec<String> = caps["methods"]
            .as_array()
            .expect("capabilities.methods is an array")
            .iter()
            .map(|m| m.as_str().unwrap_or_default().to_string())
            .collect();
        let mut documented: Vec<String> =
            documented_methods().iter().map(|s| s.to_string()).collect();
        real.sort();
        documented.sort();
        assert_eq!(
            real, documented,
            "the JSON API page has drifted from core.capabilities"
        );
    }

    /// The feature-detection snippet on the API page, against the envelope the
    /// binary really emits for that exact request.
    ///
    /// [`capabilities_snippet`] derives its body, so a new method reaches the
    /// page for free — but derivation cannot notice the *envelope* drifting
    /// around it. Rename `ok`, drop `tasqx`, nest `result` one level deeper, and
    /// the snippet would go on rendering a shape no build has emitted since.
    /// This pushes the request through `handle_envelope` — the same function
    /// `tasqx api` calls — and compares the bytes, so the module's claim that
    /// every block of output here was executed against the real binary holds for
    /// this block by construction rather than by anyone's diligence.
    ///
    /// The method-name assertion is the D52 regression itself: the pasted copy
    /// this replaced was missing `tag.remove` from both `methods` and `params`,
    /// and every other guard on this page stayed green through it.
    #[test]
    fn the_capabilities_snippet_is_the_envelope_the_api_really_emits() {
        let e = tasqx_core::Engine::open_in_memory().expect("in-memory store");
        let mut real = tasqx_core::handle_envelope(
            &e,
            r#"{"tasqx":"1","id":"c1","method":"core.capabilities"}"#,
        );
        // The two values the page substitutes: they belong to the reader's
        // machine, not to this test's empty in-memory one (which has no store
        // path at all — the very field D74 added).
        real["result"]["default_project"] = serde_json::Value::String("work.tasqx".into());
        real["result"]["store"] = serde_json::Value::String("~/.local/share/tasqx/tasks.db".into());
        let shown = capabilities_snippet();
        assert_eq!(
            serde_json::to_string(&real).expect("a Value serialises"),
            shown,
            "the API page's core.capabilities snippet is not what `tasqx api` answers"
        );

        for m in tasqx_core::capabilities()["methods"]
            .as_array()
            .expect("capabilities.methods is an array")
        {
            let m = m.as_str().expect("a method name is a string");
            assert!(
                shown.contains(&format!("\"{m}\"")),
                "method `{m}` is missing from the page's core.capabilities handshake — a client \
                 author reading it would conclude this build cannot do it"
            );
        }

        assert!(
            generate().contains(&esc(&shown)),
            "the derived capabilities snippet never reaches the rendered page"
        );
    }

    /// The API page's Params column against the set the core now ENFORCES
    /// (D33). Both directions bite, and neither is cosmetic: a param the guide
    /// names and the gate refuses sends a reader to a `bad_request` for doing
    /// what they were told, and a param the gate accepts and the guide omits is
    /// a capability nobody can find — the invisible-field failure, on the one
    /// surface whose whole job is to be the published answer.
    #[test]
    fn documented_params_match_the_accepted_set() {
        for (method, params, _) in METHODS {
            let mut documented: Vec<&str> = Vec::new();
            for chunk in params.split("<code>").skip(1) {
                let name = chunk.split("</code>").next().unwrap_or_default();
                documented.push(name.trim_end_matches('?'));
            }
            documented.sort_unstable();
            let (_, accepted, _) = tasqx_core::PARAMS
                .iter()
                .find(|(m, _, _)| *m == method)
                .unwrap_or_else(|| panic!("the guide documents `{method}`, which the core lacks"));
            let mut expected: Vec<&str> = accepted.to_vec();
            expected.sort_unstable();
            assert_eq!(
                documented, expected,
                "`{method}`: the guide's Params column has drifted from the accepted key set"
            );
        }
    }

    /// Every documented method must reach the page too.
    #[test]
    fn every_documented_method_appears_on_the_api_page() {
        let doc = generate();
        for m in documented_methods() {
            assert!(
                doc.contains(&format!("<code>{m}</code>")),
                "method `{m}` is in the METHODS table but never rendered"
            );
        }
    }

    /// The verb table's method column must name a method the core actually has —
    /// the mapping in that column IS the contract the guide claims.
    #[test]
    fn verb_table_only_names_real_methods() {
        let methods = documented_methods();
        for (verb, _, method) in VERBS {
            // Verbs that frame their own transport have no single method.
            if method.starts_with('—') || method.starts_with('(') {
                continue;
            }
            let named = method.split(" + ").next().unwrap_or(method);
            assert!(
                methods.contains(&named),
                "verb `{verb}` claims method `{named}`, which core.capabilities does not list"
            );
        }
    }

    /// The MCP page against the roster `tools/list` actually serves.
    ///
    /// This used to compare a hand-written `MCP_TOOLS` table with the server's
    /// roster. The table is gone (#647): the page is GENERATED from
    /// `mcp::tool_docs()`, so the name and the read/write fence cannot
    /// disagree with the server by construction, and the per-tool guards live
    /// beside the generator in [`mcp_ref`]. What is left here is what only the
    /// whole document can answer — that every tool really reaches the rendered
    /// page, and that the counted split sentence is counted from the roster
    /// rather than typed.
    ///
    /// Both halves have failed before. The split sentence was prose for three
    /// releases while the roster grew, and a tool could be added, renamed or
    /// moved across the scope fence with every gate green.
    #[test]
    fn documented_mcp_tools_match_the_servers_roster() {
        let real = tasqx_core::mcp::tool_roster();
        // Floor: an empty roster would green both loops below while guarding
        // nothing. Fifteen tools shipped; going below that is a decision.
        assert!(
            real.len() >= 15,
            "the MCP roster shrank below the shipped tool set: {}",
            real.len()
        );

        let doc = generate();
        for (name, _) in &real {
            assert!(
                doc.contains(&format!("<code>{name}</code>")),
                "tool `{name}` is served and never rendered onto the page"
            );
            assert!(
                doc.contains(&format!("id=\"mcp-{name}\"")),
                "tool `{name}` has no section to link to"
            );
        }
        let reads = real.iter().filter(|(_, write)| !write).count();
        assert!(
            doc.contains(&format!(
                "{reads} read tools always; {} write tools",
                real.len() - reads
            )),
            "the MCP page's read/write split sentence is not counted from the roster"
        );
    }

    /// A smoke test on the settings section, and deliberately not more.
    ///
    /// Read this before trusting it: the table is GENERATED from
    /// `config::SETTINGS`, so both sides of the assertion come from one
    /// constant through one format string and a new setting can never be
    /// "missing". Proven by mutation — adding a fourth Setting with no guide
    /// entry leaves this green; deleting the `table_owned` call turns it red.
    ///
    /// That is the right state (generated docs cannot drift) but it makes this
    /// a check that the section still renders, NOT a coverage guard. An earlier
    /// version of this comment invoked the flags guard that "found eleven real
    /// gaps" as its peer, which presented a structurally impossible result as
    /// evidence of health. The real coverage guard for this area is
    /// `every_env_var_is_either_a_registered_setting_or_a_named_exception`,
    /// which is mutation-proven load-bearing.
    #[test]
    fn the_settings_section_renders_every_registered_setting() {
        let doc = generate();
        let missing: Vec<&str> = crate::config::SETTINGS
            .iter()
            .map(|s| s.key)
            .filter(|k| !doc.contains(&format!("<code>{k}</code>")))
            .collect();
        assert!(
            missing.is_empty(),
            "settings the guide never names: {missing:?}"
        );
    }

    /// Every `TASQX_*` variable the code reads must be declared in SETTINGS or
    /// listed here as a deliberate exception. Without this, an env var that
    /// overrides behaviour can exist with nothing documenting it — which is
    /// exactly the state TASQX_FORCE_COLOR was in when this guard was written.
    ///
    /// Widened since, because the prefix was doing the guarding and the prefix
    /// is not the property that matters: `NO_COLOR` and `CLICOLOR_FORCE`
    /// override behaviour with exactly the same force as any `TASQX_*`
    /// variable, and both escaped this scan for the sole reason that they do
    /// not spell `TASQX_` — the guide documented them only after a human
    /// noticed. A non-prefixed variable the code reads must now be documented
    /// on the page or named an exception here. The scan also covers `tui.rs`
    /// and `tokens.rs`, which did not exist when the source list was drawn up.
    #[test]
    fn every_env_var_is_either_a_registered_setting_or_a_named_exception() {
        // Not settings: these select a whole store/transport rather than tuning
        // behaviour, and giving them a config layer is a separate decision
        // (see the spec's "out of scope").
        const EXCEPTIONS: &[&str] = &[
            "TASQX_DB",
            "TASQX_SOCK",
            "TASQX_CONFIG_DIR",
            "TASQX_FORCE_COLOR",
            // Different in kind from the rest of this list: set by `build.rs`
            // and read by `env!` at compile time, so it is baked into the
            // binary and cannot be set by a user at all. A config layer for it
            // is not "out of scope" but meaningless.
            "TASQX_BUILD_ID",
            // The shell-completion pair, and neither can be a setting.
            //
            // `TASQX_COMPLETE` selects a transport in the same sense TASQX_SOCK
            // does — it is written by the activation line a shell sources, not
            // by a human tuning behaviour, and `complete::intercept` reads it
            // before argv exists, let alone a config file.
            //
            // `TASQX_NO_COMPLETE_LOOKUP` looks like a setting and deliberately
            // is not: it disables opening the store on a keystroke, and reading
            // `config.toml` to discover that would do the very work being
            // disabled. A switch that can only be honoured after performing the
            // thing it turns off is a switch that does not work. Both are
            // user-facing and belong on the guide page — Task 10 of the
            // completion plan owns that; this list is about the SETTINGS
            // registry, which is a different question.
            "TASQX_COMPLETE",
            "TASQX_NO_COMPLETE_LOOKUP",
            // Test scaffolding of the same kind as TASQX_PANIC_PROBE_CHILD
            // below, and for a related reason: the completion suite drives the
            // shipped binary and asks what it OFFERS, while the lookup budget
            // is a latency property with its own guard. Left alone, the content
            // tests race the machine, and losing that race is spelled as zero
            // candidates at exit 0 — the same output as a broken completer.
            // Documenting it as a switch would invite a user to lengthen the
            // budget, which is the stall it exists to prevent.
            "TASQX_COMPLETE_BUDGET_MS",
            // Test scaffolding, not a switch: `complete.rs`'s panic-silencing
            // guard re-runs itself as a child process and this is how the child
            // knows which side of the fork it is on. Named here rather than
            // spelled without the prefix to dodge the scan, because dodging a
            // drift guard is how the things it hunts get in.
            "TASQX_PANIC_PROBE_CHILD",
            // The capture pin (D149): an RFC 3339 instant that both
            // `clock::now` doors answer with instead of the wall clock, so a
            // documentation screen rendered today and rendered next month is
            // the same text — writes included, since the engine stamps
            // `created` and `completed` from the same door.
            //
            // Emphatically not a setting, and not on the page. A user who
            // pinned it in `config.toml` would have `list` insist it is still
            // September while their tasks went overdue, which is the one
            // reading of a task list that must never be wrong — and every task
            // they added would be stamped with that day. It belongs with the
            // capture tooling that sets it for one process (`CONTRIBUTING.md`,
            // `scripts/demo-store.py`), is documented in both `clock.rs`
            // modules, and `tasqx about` states it whenever it is in effect.
            "TASQX_NOW",
        ];
        // Read by the capability detector but deliberately not documented as
        // switches: they describe what the terminal IS (set by the terminal,
        // not by a user aiming at tasqx), so a row for them in the override
        // table would invite exactly the hand-tuning the detector exists to
        // make unnecessary.
        const TERMINAL_IDENTITY: &[&str] = &["TERM", "COLORTERM"];
        // Read by `complete/install.rs` to find the user's OWN files, and
        // deliberately not switches. Neither one changes what tasqx does: they
        // answer "which shell are you running" and "where did you put your
        // config", questions whose answers belong to the operating system and
        // to fish respectively. Documenting them in the override table would
        // invite a user to set `SHELL=fish` to make `--install` target fish,
        // which is a worse spelling of `tasqx completions fish --install` and
        // would edit the wrong file for the shell they are actually in.
        //
        // `XDG_CONFIG_HOME` is honoured rather than merely tolerated: fish
        // honours it, and writing to `~/.config/fish` for a user who moved
        // their fish config produces a file fish never reads — completion
        // silently not working, which is the symptomless failure this project
        // hunts.
        const USER_ENVIRONMENT: &[&str] = &["SHELL", "XDG_CONFIG_HOME"];
        // Hand-kept, and that is the guard's own weak spot: `complete.rs` was
        // absent until the completion feature added two variables to it, and
        // for the length of that branch `TASQX_NO_COMPLETE_LOOKUP` existed with
        // nothing documenting it and nothing noticing — the exact state
        // TASQX_FORCE_COLOR was in when this test was written. It surfaced only
        // because a comment in `lib.rs` came to mention `$TASQX_COMPLETE`. A
        // file added tomorrow has the same hole; the list is here rather than
        // derived because `include_str!` needs a literal path.
        let sources = [
            include_str!("lib.rs"),
            include_str!("theme.rs"),
            include_str!("config.rs"),
            include_str!("tui.rs"),
            include_str!("tokens.rs"),
            include_str!("complete.rs"),
            // Added with the `completions` verb, which is the first code in the
            // crate to read a variable it does not own (`$SHELL`,
            // `$XDG_CONFIG_HOME`). `complete/candidates.rs` is here for nothing
            // it does today and everything it might: it is the file a provider
            // author edits, and the hole this list keeps having is a file
            // nobody added.
            include_str!("complete/install.rs"),
            include_str!("complete/candidates.rs"),
            // The hole this list keeps having is a file nobody added, and
            // `clock.rs` is the newest file that reads a variable (D149).
            include_str!("clock.rs"),
        ];

        // TASQX_* rule: a textual scan, comments included — a prefixed mention
        // is either a real variable or documentation of one, and both must
        // resolve to something registered or excepted.
        let mut found: Vec<String> = Vec::new();
        for src in sources {
            let mut rest = src;
            while let Some(i) = rest.find("TASQX_") {
                let tail = &rest[i..];
                let end = tail
                    .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                    .unwrap_or(tail.len());
                // `TASQX_` with nothing after it is prose, not a variable —
                // doc comments write `TASQX_*` and the `*` ends the scan.
                if end > "TASQX_".len() {
                    found.push(tail[..end].to_string());
                }
                rest = &tail[end..];
            }
        }
        found.sort();
        found.dedup();
        let registered: Vec<&str> = crate::config::SETTINGS
            .iter()
            .filter_map(|s| s.env)
            .collect();
        let orphans: Vec<&String> = found
            .iter()
            .filter(|v| !registered.contains(&v.as_str()) && !EXCEPTIONS.contains(&v.as_str()))
            .collect();
        assert!(
            orphans.is_empty(),
            "env vars with no setting and no exception: {orphans:?}"
        );

        // Non-prefixed rule: only real reads count — `env::var("X")` /
        // `env::var_os("X")` with a literal — because prose mentions a
        // variable in order to explain it, and comments are where NO_COLOR
        // appears most.
        let mut read_vars: Vec<String> = Vec::new();
        for src in sources {
            for pat in ["env::var(\"", "env::var_os(\""] {
                let mut rest = src;
                while let Some(i) = rest.find(pat) {
                    let tail = &rest[i + pat.len()..];
                    if let Some(end) = tail.find('"') {
                        read_vars.push(tail[..end].to_string());
                    }
                    rest = tail;
                }
            }
        }
        read_vars.sort();
        read_vars.dedup();
        // Floor: the scan must actually see the colour pair, or an edit to the
        // read pattern leaves this half of the guard green and empty.
        for known in ["NO_COLOR", "CLICOLOR_FORCE"] {
            assert!(
                read_vars.iter().any(|v| v == known),
                "the env-read scan no longer finds {known} — pattern rot in this test"
            );
        }
        let doc = generate();
        let undocumented: Vec<&String> = read_vars
            .iter()
            // Prefixed vars answer to the registered-or-excepted rule above.
            .filter(|v| !v.starts_with("TASQX_"))
            .filter(|v| !TERMINAL_IDENTITY.contains(&v.as_str()))
            .filter(|v| !USER_ENVIRONMENT.contains(&v.as_str()))
            .filter(|v| !doc.contains(&format!("<code>{v}</code>")))
            .collect();
        assert!(
            undocumented.is_empty(),
            "non-TASQX env vars the code reads but the guide never documents: {undocumented:?}"
        );
    }

    /// The Errors table's Exit column must be `ErrorCode::exit_code`.
    ///
    /// It printed `—` for `unsupported_version` — "this code has no exit
    /// status" — while the binary exits 6:
    ///
    /// ```console
    /// $ echo '{"tasqx":"2","method":"task.list","params":{}}' | tasqx api ; echo $?
    /// 6
    /// ```
    ///
    /// Every other row carried a real number, so the dash read as a documented
    /// fact rather than a gap. The column is now compared against the mapping
    /// that decides it, cell by cell, so renumbering a code reddens here
    /// instead of leaving the page asserting a stale literal.
    #[test]
    fn the_errors_table_exit_column_matches_the_code_mapping() {
        use tasqx_core::ErrorCode;
        // The `headers` attribute `table_owned` now stamps on every `<td>`
        // carries a table-generation-order id, which this test does not
        // predict — strip it so the adjacency check below still pins the two
        // cells next to each other regardless of that id (PR #61 review).
        fn strip_headers_attr(html: &str) -> String {
            let mut out = String::with_capacity(html.len());
            let mut rest = html;
            while let Some(idx) = rest.find(" headers=\"") {
                out.push_str(&rest[..idx]);
                let after = &rest[idx + " headers=\"".len()..];
                let end = after.find('"').expect("a closed headers attribute");
                rest = &after[end + 1..];
            }
            out.push_str(rest);
            out
        }
        let doc = strip_headers_attr(&generate());
        // Membership from the enum. Retyped here, a sixth variant reached this
        // page with the guard green and no row to show for it.
        for code in ErrorCode::all() {
            // The row as `table_owned` renders it: the code cell, then the
            // exit cell. Pinning them ADJACENTLY is the point — asserting the
            // number appears somewhere on the page would pass on any table.
            let want = format!(
                "<td data-label=\"Code\"><code>{}</code></td><td data-label=\"Exit\">{}</td>",
                code.as_str(),
                code.exit_code()
            );
            assert!(
                doc.contains(&want),
                "the Errors table does not give `{}` exit {} — the CLI does",
                code.as_str(),
                code.exit_code()
            );
        }
    }

    /// `modify --clear` takes a closed set; the page prints it. If
    /// `crate::CLEARABLE` gains a field, the docs must say so.
    #[test]
    fn documented_clear_fields_match_the_parser() {
        assert_eq!(
            crate::CLEARABLE.to_vec(),
            DOCUMENTED_CLEAR_FIELDS.to_vec(),
            "`modify --clear` accepts a different set than the docs claim"
        );
        let doc = generate();
        for f in DOCUMENTED_CLEAR_FIELDS {
            assert!(doc.contains(f), "clearable field `{f}` is not on the page");
        }
    }

    /// The sugar column of the `add` table against the parser's own key table —
    /// the same binding [`documented_clear_fields_match_the_parser`] gives
    /// `--clear`. The rot this stops has already happened once: `recur:`
    /// shipped as a `VALUE_KEYS` alias that no documented surface named, so the
    /// alias class could grow (or shrink, leaving the page teaching a spelling
    /// that silently lands in the title) with every gate green.
    ///
    /// Both directions via set equality, and the keys must reach the rendered
    /// page — a table constant nothing renders documents nothing.
    #[test]
    fn documented_sugar_keys_match_the_parser() {
        let mut real: Vec<String> = crate::sugar::value_key_spellings()
            .iter()
            .map(|k| k.to_string())
            .collect();
        real.sort();
        // Floor: an emptied parser table must not green the equality below by
        // meeting an emptied doc column halfway.
        assert!(
            real.len() >= 12,
            "the parser's sugar key table shrank: {real:?}"
        );

        // The colon keys of the sugar column. `+tag` and `!high` are sugar too,
        // but not VALUE_KEYS sugar — they have no colon and are skipped.
        let mut documented: Vec<String> = Vec::new();
        for (_, sugar, _) in ADD_FIELDS {
            for chunk in sugar.split("<code>").skip(1) {
                let key = chunk.split("</code>").next().unwrap_or_default().trim();
                if key.ends_with(':') {
                    documented.push(key.to_string());
                }
            }
        }
        documented.sort();
        documented.dedup();
        assert_eq!(
            documented, real,
            "the add table's sugar column has drifted from sugar::VALUE_KEYS"
        );

        let doc = generate();
        for key in &real {
            assert!(
                doc.contains(&format!("<code>{key}</code>")),
                "sugar key `{key}` never reaches the rendered page"
            );
        }
    }

    /// The Filters page must show the parser's grammar, not a copy of it.
    ///
    /// It used to show a copy, and the copy rotted exactly as copies do: it still
    /// claimed a tag took a bare word after quoted tags shipped, and it named a
    /// `WORD` symbol it never defined. Rendering the const removes the second
    /// copy; this asserts nobody quietly reintroduces one.
    #[test]
    fn the_filter_grammar_on_the_page_is_the_parsers_own() {
        let doc = generate();
        let rendered = crate::html::esc(tasqx_core::filter::GRAMMAR);
        assert!(
            doc.contains(&rendered),
            "the Filters page is not rendering `tasqx_core::filter::GRAMMAR` verbatim"
        );
    }

    // ---- self-containment ---------------------------------------------------

    /// The self-containment and well-formedness guards both pass on an
    /// over-escaped page — `&lt;code&gt;` is valid HTML, it just renders the tag
    /// as literal text to the reader. Eleven leads shipped that way. Only a
    /// check on the *rendered* text catches it.
    #[test]
    fn no_markup_leaks_into_the_page_as_visible_text() {
        let doc = generate();
        // Only tag names that cannot double as a prose placeholder. `<p>` is out:
        // it is the value placeholder in `--project <p>`, and `<addr>`/`<ref>`/`<n>`
        // are the same kind of thing — those must STAY escaped.
        for tag in ["code", "em", "strong", "pre"] {
            let leaked = format!("&lt;{tag}&gt;");
            assert!(
                !doc.contains(&leaked),
                "escaped <{tag}> tag renders as literal text to the reader"
            );
        }
        assert!(
            !doc.contains("&lt;a href"),
            "escaped <a href> renders as literal text"
        );
        assert!(
            doc.contains("&lt;addr&gt;"),
            "prose placeholders must stay escaped"
        );
    }

    /// The filter page tells the reader which values `status:` accepts, and it
    /// listed four of the five — `backlog` was missing. That is not a drift risk,
    /// it was already wrong in shipped output: DESIGN.md defines `backlog` as a
    /// status and documents `tasqx ls status:backlog`, so the guide taught a
    /// reader there was no way to find work parked behind a future `wait:` or
    /// `scheduled:`. None of the twenty-odd docs guards enumerated statuses, so
    /// nothing noticed.
    ///
    /// Derived from `Status::ALL` rather than restating the names: a guard that
    /// hand-lists what it checks is the same parallel list it exists to police.
    #[test]
    fn the_filter_page_documents_every_status_value() {
        let doc = generate();
        let missing: Vec<&str> = tasqx_core::types::Status::ALL
            .into_iter()
            .map(|s| s.as_str())
            .filter(|name| !doc.contains(&format!("<code>{name}</code>")))
            .collect();
        assert!(
            missing.is_empty(),
            "statuses a user can filter on but the guide never names: {missing:?}"
        );
    }

    /// A reader who cannot see D24 in the guide has no way to explain a report
    /// count that looks too low — the tasks are still in the store, still listed
    /// by `tasqx list`, just absent from the roll-up. The VERBS/METHODS drift
    /// guards cannot catch this: the rule is prose, not a table they render from.
    #[test]
    fn reports_section_states_which_statuses_count() {
        let doc = generate();
        let reports = doc
            .split(&h3("Reports"))
            .nth(1)
            .expect("a Reports section")
            .split("<h3")
            .next()
            .unwrap();
        assert!(
            reports.contains("cancelled"),
            "the reports section must name the excluded status: {reports}"
        );
        assert!(
            reports.contains("--all"),
            "and the escape hatch from it: {reports}"
        );
    }

    /// The store path the guide prints must be the one the binary actually opens.
    /// It said `%APPDATA%\tasqx\tasks.db` while `db_path()` opens
    /// `%APPDATA%\tasqx\tasqx\data\tasks.db` — a reader following the page looked
    /// for their data in a directory that does not exist. The VERBS/METHODS drift
    /// guards cannot see this: a path is prose, not a table they render from.
    #[cfg(windows)]
    #[test]
    fn documented_store_path_matches_the_real_one() {
        let dirs = directories::ProjectDirs::from("dev", "tasqx", "tasqx")
            .expect("a data dir on this platform");
        let real = dirs.data_dir().join("tasks.db");
        // Compare the tail below %APPDATA%, which is the part the page spells out.
        let tail: Vec<_> = real
            .components()
            .rev()
            .take(4)
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\\");
        assert_eq!(
            tail, "tasqx\\tasqx\\data\\tasks.db",
            "db_path()'s shape changed"
        );

        // The page holds the path as ordinary text (the `\\` in the source literal
        // is one backslash at runtime), so a plain substring check is the whole test.
        let doc = generate();
        assert!(
            doc.contains(&tail),
            "the guide does not print the real store path ({tail})"
        );
    }

    /// The unix sibling of the guard above, for the same reason: the page used
    /// to spell only the Windows path, so a Linux or macOS reader had nothing
    /// to check theirs against — and `ProjectDirs` flattens differently per
    /// platform (one plain `tasqx` segment on Linux, `dev.tasqx.tasqx` on
    /// macOS), which is exactly the kind of fact prose gets wrong.
    #[cfg(unix)]
    #[test]
    fn documented_store_path_matches_the_real_one() {
        let proj = directories::ProjectDirs::from("dev", "tasqx", "tasqx")
            .expect("a data dir on this platform");
        let base = directories::BaseDirs::new().expect("a home dir on this platform");
        let real = proj.data_dir().join("tasks.db");
        // Compare the tail below the platform data root. `$XDG_DATA_HOME` moves
        // the root, never the project-specific part, so this stays true on a
        // machine that relocates its data dir.
        let tail = real
            .strip_prefix(base.data_dir())
            .expect("the project dir lives under the platform data dir")
            .to_string_lossy()
            .into_owned();
        let documented = if cfg!(target_os = "macos") {
            "~/Library/Application Support/dev.tasqx.tasqx/tasks.db"
        } else {
            "~/.local/share/tasqx/tasks.db"
        };
        assert!(
            documented.ends_with(&tail),
            "db_path()'s shape changed: the real path ends in {tail:?}, the page prints {documented:?}"
        );

        let doc = generate();
        assert!(
            doc.contains(documented),
            "the guide does not print the real store path ({documented})"
        );
    }

    /// Everything outside a `<pre>` or a `<code>`: the document's own markup,
    /// with the terminal text it quotes taken out.
    ///
    /// The distinction [`docs_page_is_self_contained`] needs. A URL the browser
    /// would *act on* lives in an attribute or a tag; a URL inside a code block
    /// is a character sequence the reader copies into a shell, and the browser
    /// never looks at it.
    fn outside_code(doc: &str) -> String {
        let mut out = String::with_capacity(doc.len());
        let mut rest = doc;
        // The *earlier* of the two openers, not the first one that happens to
        // be present: taking `<pre` when a `<code>` comes before it would copy
        // that code span into the output.
        while let Some(i) = [rest.find("<pre"), rest.find("<code")]
            .into_iter()
            .flatten()
            .min()
        {
            let (before, tail) = rest.split_at(i);
            out.push_str(before);
            let close = if tail.starts_with("<pre") {
                "</pre>"
            } else {
                "</code>"
            };
            rest = match tail.find(close) {
                Some(j) => &tail[j + close.len()..],
                // An unterminated block: keep the tail rather than hide it.
                None => break,
            };
        }
        out.push_str(rest);
        out
    }

    /// The whole promise of the file: it opens anywhere, offline, forever. Any
    /// external reference — a CDN script, a web font, a remote image — breaks it.
    ///
    /// **Scheme URLs are judged by where they sit, not by their spelling.** The
    /// guide used to refuse the string `https://` anywhere in the document,
    /// which is a proxy for the property that matters and stopped being an
    /// honest one when `docs/wiki/Getting-Started.md` became a page: its install
    /// commands are `curl -fsSL https://…/install.sh | sh`, and a command with
    /// its scheme filed off is not a shorter command, it is a wrong one. A URL
    /// inside a `<pre>`/`<code>` is text the reader copies; the file still makes
    /// no request, and every *href* is checked separately by
    /// [`every_link_is_an_internal_anchor`], which is the rule that keeps a link
    /// from leaving the file. Outside those blocks — in an attribute, a tag or
    /// the inline CSS and JS — a scheme URL is still refused.
    #[test]
    fn docs_page_is_self_contained() {
        let doc = generate();
        // The stripper is load-bearing, so it is checked before it is trusted:
        // an attribute must survive it and a code block must not.
        assert!(
            outside_code("<a href=\"https://cdn.example\">x</a>").contains("https://"),
            "the stripper hides an attribute — this guard would pass on a CDN link"
        );
        assert!(
            !outside_code("<pre class=\"cmd\"><code>curl https://x | sh</code></pre>")
                .contains("https://"),
            "the stripper does not remove quoted terminal text"
        );
        let chrome = outside_code(&doc);
        assert!(
            !chrome.contains("http://"),
            "contains an http:// reference outside a code block"
        );
        assert!(
            !chrome.contains("https://"),
            "contains an https:// reference outside a code block"
        );
        assert!(!doc.contains("src="), "contains src= (an external asset)");
        assert!(!doc.contains("@import"), "contains a CSS @import");
        assert!(!doc.contains("url("), "contains a CSS url() reference");
        assert!(!doc.contains("//fonts."), "contains a font host");
        assert!(
            !doc.contains("integrity="),
            "contains an SRI attr — implies a CDN"
        );
        assert!(
            !doc.contains("<link"),
            "contains a <link> (stylesheet/font/icon)"
        );
        assert!(!doc.contains("<iframe"), "contains an <iframe>");
    }

    /// Every `href` must be an in-page anchor. This is the rule `report --html`
    /// enforces by having no links at all; the guide has links, so it has to check
    /// them one by one instead.
    #[test]
    fn every_link_is_an_internal_anchor() {
        let doc = generate();
        for (i, _) in doc.match_indices("href=\"") {
            let rest = &doc[i + 6..];
            let end = rest.find('"').expect("an href must be terminated");
            let target = &rest[..end];
            assert!(
                target.starts_with('#'),
                "external link `{target}` — the guide must not reference anything off-file"
            );
            assert!(target.len() > 1, "empty anchor href");
        }
    }

    /// Every anchor lands on an id that is really in the document.
    ///
    /// [`every_link_is_an_internal_anchor`] proves a link stays inside the
    /// file; this proves it arrives somewhere. The two are different failures
    /// and only one of them was checked. It matters most for the pages built
    /// from `docs/wiki`: those links were written as `Finding-Tasks.md#tasqx-why`
    /// against GitHub's heading anchors and are rewritten by
    /// [`markdown`] into `#wiki-finding-tasks--tasqx-why` — a rewrite whose
    /// failure mode is silent, because a hash naming nothing simply leaves the
    /// reader where they were. A renamed heading breaks it, and renaming a
    /// heading is an ordinary edit to a prose file.
    #[test]
    fn every_anchor_resolves_to_an_id_in_the_document() {
        let doc = generate();
        let mut ids: Vec<&str> = Vec::new();
        for (i, _) in doc.match_indices("id=\"") {
            let rest = &doc[i + 4..];
            let end = rest.find('"').expect("an id must be terminated");
            ids.push(&rest[..end]);
        }
        let mut checked = 0usize;
        let mut dangling: Vec<&str> = Vec::new();
        for (i, _) in doc.match_indices("href=\"#") {
            let rest = &doc[i + 7..];
            let end = rest.find('"').expect("an href must be terminated");
            let target = &rest[..end];
            checked += 1;
            if !ids.contains(&target) {
                dangling.push(target);
            }
        }
        dangling.sort_unstable();
        dangling.dedup();
        assert!(
            dangling.is_empty(),
            "anchors pointing at no id in the document: {dangling:?}"
        );
        // Floor: the scan must see the cross-page links the wiki pages carry,
        // or a change to the markup leaves this walking an empty document.
        assert!(
            checked > 120,
            "only {checked} anchors scanned — this guard is checking almost nothing"
        );
    }

    /// Exactly one well-formed document, with its CSS and JS inline.
    #[test]
    fn docs_page_is_one_well_formed_document() {
        let doc = generate();
        assert!(doc.starts_with("<!doctype html>"));
        assert!(doc.trim_end().ends_with("</html>"));
        assert_eq!(doc.matches("<html").count(), 1);
        assert_eq!(doc.matches("</html>").count(), 1);
        assert_eq!(doc.matches("<style>").count(), 1, "CSS is inline, once");
        assert_eq!(doc.matches("<script>").count(), 1, "JS is inline, once");
        assert_eq!(doc.matches("<script").count(), 1, "no second script tag");
    }

    /// Regression, found by driving the real page over `file://`: navigation used
    /// `history.pushState`, which throws a **SecurityError** on a `file://`
    /// document (its origin is `null`) — the exact transport `tasqx docs` opens.
    /// The symptom was quiet and nasty: the page still switched (the toggles ran
    /// first), so it *looked* fine, but the URL never updated, which meant no deep
    /// links and no back button — the one thing anchors are for. Navigation is
    /// hash-driven now; nothing here may reach for the History API again.
    #[test]
    fn navigation_does_not_use_the_history_api() {
        let doc = generate();
        assert!(
            !doc.contains("pushState"),
            "pushState throws a SecurityError on file://, which is how `tasqx docs` opens the guide"
        );
        assert!(
            !doc.contains("replaceState"),
            "replaceState throws on file:// for the same reason"
        );
        assert!(
            doc.contains("addEventListener('hashchange'"),
            "navigation must be driven by hashchange, the only mechanism that works on file://"
        );
    }

    /// Both colour schemes, same as the report.
    #[test]
    fn docs_page_has_both_color_schemes() {
        let doc = generate();
        assert!(doc.contains(":root {"), "light scheme root vars");
        assert!(
            doc.contains("@media (prefers-color-scheme: dark)"),
            "dark scheme"
        );
        assert!(doc.matches("--bg:").count() >= 2, "--bg for both schemes");
    }

    /// Every page in the nav exists as a section, and vice versa — a nav link to a
    /// missing page is a dead end the reader finds before we do.
    #[test]
    fn every_nav_link_has_a_page() {
        let doc = generate();
        for pg in PAGES.iter() {
            let id = &pg.id;
            assert!(
                doc.contains(&format!("id=\"{id}\"")),
                "no section for nav page `{id}`"
            );
            assert!(
                doc.contains(&format!("href=\"#{id}\"")),
                "no nav link to page `{id}`"
            );
        }
        assert_eq!(
            doc.matches("class=\"page\"").count(),
            PAGES.len(),
            "the number of rendered pages does not match PAGES"
        );
    }

    /// Every page is filed under a section that exists, and every section is
    /// rendered — including the empty ones.
    ///
    /// The empty half is the point. `guides` and `objects` have no pages yet,
    /// and the cheap way to write [`nav`] is to iterate the pages and emit a
    /// heading when the section changes — which silently drops a section
    /// nobody has filled, so the day someone adds the first Guides page the
    /// heading appears out of nowhere and nothing was ever watched failing. A
    /// section with no pages must render its heading and say so.
    #[test]
    fn every_page_is_in_a_declared_section_and_every_section_renders() {
        let nav = nav();
        for pg in PAGES.iter() {
            assert!(
                SECTIONS.iter().any(|s| s.id == pg.section),
                "page `{}` names section `{}`, which is not in SECTIONS",
                pg.id,
                pg.section
            );
        }
        for sec in SECTIONS {
            assert!(
                nav.contains(&format!("data-section=\"{}\"", sec.id)),
                "section `{}` is not rendered in the sidebar",
                sec.id
            );
            assert!(
                nav.contains(sec.title),
                "section `{}` renders no heading",
                sec.id
            );
        }
        let empty: Vec<&str> = SECTIONS
            .iter()
            .filter(|s| !PAGES.iter().any(|p| p.section == s.id))
            .map(|s| s.id)
            .collect();
        // Every section has pages since the Objects section filled (#648), so
        // this compares zero with zero until a section is declared ahead of
        // its writing again — which is when it matters.
        assert_eq!(
            nav.matches("navsec-soon").count(),
            empty.len(),
            "an empty section ({empty:?}) renders no placeholder, so its heading is \
             followed by the next section's entries"
        );
    }

    /// The sidebar is the reading order, and prev/next walks the same line.
    /// A page emitted in a different order than PAGES declares would give a
    /// JavaScript-less reader one order and the Next link another.
    #[test]
    fn pages_are_emitted_in_sidebar_order() {
        let doc = generate();
        let mut at = 0usize;
        for pg in PAGES.iter() {
            let needle = format!("<section class=\"page\" id=\"{}\">", pg.id);
            let found = doc[at..]
                .find(&needle)
                .unwrap_or_else(|| panic!("page `{}` is out of PAGES order, or missing", pg.id));
            at += found + needle.len();
        }
        // Prev/next: every page but the first names its predecessor, and every
        // page but the last names its successor.
        for (i, pg) in PAGES.iter().enumerate() {
            let foot = page_close(&pg.id);
            assert_eq!(
                foot.contains("class=\"prev\""),
                i > 0,
                "page `{}` has the wrong prev link",
                pg.id
            );
            assert_eq!(
                foot.contains("class=\"next\""),
                i + 1 < PAGES.len(),
                "page `{}` has the wrong next link",
                pg.id
            );
            if i > 0 {
                assert!(
                    foot.contains(&format!("href=\"#{}\"", PAGES[i - 1].id)),
                    "page `{}` does not link back to `{}`",
                    pg.id,
                    PAGES[i - 1].id
                );
            }
        }
    }

    /// Wide content must scroll inside its own box. Both mechanisms must be present
    /// or a long table drags the whole page sideways on a phone.
    #[test]
    fn wide_content_scrolls_in_its_own_container() {
        let doc = generate();
        assert!(
            doc.contains(".tw { overflow-x: auto;"),
            "table wrapper scrolls"
        );
        assert!(doc.contains("overflow-x: auto"), "pre blocks scroll");
        // Every table is wrapped in the scroller.
        assert_eq!(
            doc.matches("<table class=\"grid\">").count(),
            doc.matches("<div class=\"tw\">").count(),
            "a table is not wrapped in a scroll container"
        );
    }

    /// The guide is escaped by the same rule as the report: a `<` in terminal
    /// output is a character, never markup.
    #[test]
    fn snippet_output_is_escaped() {
        let snip = snippet("tasqx list \"a<b\"", "<script>alert(1)</script> & done");
        assert!(
            !snip.contains("<script>"),
            "raw markup survived into a snippet"
        );
        assert!(snip.contains("&lt;script&gt;"));
        assert!(snip.contains("&amp; done"));
        assert!(snip.contains("a&lt;b"));
    }

    /// Pull every `snippet()` block out of a page as (command, output) pairs.
    /// The page is the only source of truth here — the test reads what a reader
    /// reads, not some parallel list an author has to remember to update.
    ///
    /// A CAPTURED screen is not one of these, although [`term_screen`] wraps it
    /// in the same `.snip` container: it carries a `pre.term` instead of a
    /// `pre.out`, its command was executed against the demo store rather than
    /// written for a reader to follow along with, and every claim it makes is
    /// already guarded by the capture job re-running it (D149). The guards below
    /// are about hand-written worked examples — whether the ids line up, whether
    /// a project was created first — and a recording answers none of those
    /// questions.
    fn snippets_of(page: &str) -> Vec<(String, String)> {
        // The header `snippet()` writes, and `term_screen` does not.
        const SNIPPET_HEAD: &str =
            "<div class=\"snip-h\"><span class=\"dollar\">$</span><button class=\"copy\"";
        assert!(
            snippet("x", "y").contains(SNIPPET_HEAD)
                && !term_screen("x", "list").contains(SNIPPET_HEAD),
            "the two block shapes are no longer told apart by their header"
        );
        let mut out = Vec::new();
        for chunk in page.split("<div class=\"snip\">").skip(1) {
            // The two shapes differ in their header: `snippet` puts a Copy
            // button beside the `$`, `term_screen` does not, and the header is
            // the first thing in the chunk — which a `<pre class="term">` test
            // is not, since the next block on the page may be a `term_block`.
            if !chunk.starts_with(SNIPPET_HEAD) {
                continue;
            }
            let Some(c0) = chunk.find("<pre class=\"cmd\"><code>") else {
                continue;
            };
            let c0 = c0 + "<pre class=\"cmd\"><code>".len();
            let Some(c1) = chunk[c0..].find("</code></pre>") else {
                continue;
            };
            let cmd = unesc(&chunk[c0..c0 + c1]);
            let body = match chunk.find("<pre class=\"out\"><code>") {
                Some(o0) => {
                    let o0 = o0 + "<pre class=\"out\"><code>".len();
                    match chunk[o0..].find("</code></pre>") {
                        Some(o1) => unesc(&chunk[o0..o0 + o1]),
                        None => String::new(),
                    }
                }
                None => String::new(),
            };
            out.push((cmd, body));
        }
        out
    }

    /// Inverse of `esc` for the entities `esc` emits — enough to compare text.
    fn unesc(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    /// The quickstart's ids must be the ids a reader actually gets. short_ids are
    /// assigned 1,2,3… in creation order, so the Nth `add` on the page is #N — and
    /// the page claimed the 2nd add printed "Added #3", because the blocks had been
    /// captured against a store seeded with extra out-of-band tasks. A reader
    /// following along desynchronised at step three: every later `why 1` / `done 4`
    /// / `dep 2 1` on the page pointed at something else in their store.
    #[test]
    fn quickstart_add_ids_are_the_ids_a_reader_gets() {
        let adds: Vec<(String, String)> = snippets_of(&page_install())
            .into_iter()
            .filter(|(cmd, _)| cmd.starts_with("tasqx add "))
            .collect();
        assert!(adds.len() >= 2, "the quickstart must actually add tasks");
        for (i, (cmd, out)) in adds.iter().enumerate() {
            // D126: an add echoes its card, which opens `▌ #N  Title`.
            let expected = format!("▌ #{}  ", i + 1);
            assert!(
                out.starts_with(&expected),
                "quickstart add #{} ({cmd:?}) shows {:?} — short_ids are handed out in \
                 creation order, so it must show {expected:?}",
                i + 1,
                out.lines().next().unwrap_or("")
            );
        }
    }

    /// Nothing may appear in a quickstart output block that no quickstart command
    /// creates. The working-set table used to list "Write the user guide", a task
    /// added by no command on the page — a ghost row inherited from the scratch
    /// store the blocks were captured against, which also made the row count wrong.
    #[test]
    fn quickstart_output_never_shows_a_task_no_command_creates() {
        let snips = snippets_of(&page_install());
        // Titles the page creates: the first line of each `add` block is its
        // card, `▌ #N  Title` (D126), which echoes the stored title back.
        let created: Vec<String> = snips
            .iter()
            .filter(|(cmd, _)| cmd.starts_with("tasqx add "))
            .filter_map(|(_, out)| {
                out.lines()
                    .next()
                    .and_then(|l| l.split_once("  "))
                    .map(|(_, title)| title.trim().to_string())
            })
            .collect();
        assert!(
            !created.is_empty(),
            "no add blocks found — did the page change shape?"
        );

        // Rows of the working-set table.
        //
        // Matched on either spelling: the quickstart showed a bare `tasqx` until
        // D58 gave that a second meaning on a terminal, and the page now spells
        // the verb. What this guard is about — D20, a documented table showing a
        // task no documented command creates — is the same either way, so it
        // accepts both rather than pinning the page to one wording.
        let (_, table) = snips
            .iter()
            .find(|(cmd, _)| matches!(cmd.trim(), "tasqx" | "tasqx list"))
            .expect("the quickstart must show the working set");
        let rows: Vec<&str> = table
            .lines()
            .filter(|l| {
                l.starts_with("   ") && l.trim().chars().next().is_some_and(|c| c.is_ascii_digit())
            })
            .collect();

        for row in &rows {
            let named = created.iter().any(|t| row.contains(t.as_str()));
            assert!(
                named,
                "working-set row {row:?} shows a task no documented command creates"
            );
        }
        // …and the summary line the table OPENS with must agree with the rows
        // under it. It is the first line rather than the last since the
        // trailer moved to the top of the table; the count reads `1 task` /
        // `N tasks`, which is what `render::plural_tasks` prints.
        let want = if rows.len() == 1 {
            "1 task".to_string()
        } else {
            format!("{} tasks", rows.len())
        };
        assert!(
            table
                .lines()
                .next()
                .is_some_and(|first| first.contains(&want)),
            "the table shows {} rows but its summary line disagrees:\n{table}",
            rows.len()
        );
        assert_eq!(
            rows.len(),
            created.len(),
            "every created task should be in the working set"
        );
    }

    /// D23: a documented command that *files* a task into a project may only
    /// name a project a documented `init` creates. The guide is one narrative
    /// against one store, so this reads every page's commands, not just the
    /// quickstart's.
    ///
    /// The blocks were captured when `project:` was free-form, so
    /// `project:home` appeared on two pages with no `init home` anywhere — a
    /// command that, under D23, exits 4 for any reader who types it. The page
    /// claims nothing on it is illustrative; this is the same class of rot D20's
    /// guards exist for, one rule later.
    #[test]
    fn every_documented_project_is_one_a_documented_init_creates() {
        let doc = generate();
        let snips = snippets_of(&doc);
        let commands: Vec<String> = snips
            .iter()
            .flat_map(|(cmd, _)| cmd.lines())
            .map(|l| l.trim().to_string())
            .collect();

        // Projects the page creates: `tasqx init <name> [--desc ...]`.
        let created: Vec<String> = commands
            .iter()
            .filter_map(|c| c.strip_prefix("tasqx init "))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(str::to_string)
            .collect();
        assert!(
            !created.is_empty(),
            "the guide must create a project somewhere"
        );

        // Only the verbs that WRITE a project have to name a live one: `list`,
        // `export` and friends take a filter, and a filter may name anything.
        let writes: Vec<&String> = commands
            .iter()
            .filter(|c| {
                ["add", "a", "new", "modify", "mod", "m", "edit"]
                    .iter()
                    .any(|v| c.starts_with(&format!("tasqx {v} ")))
            })
            .collect();
        assert!(
            !writes.is_empty(),
            "the guide must show an add — did the page change shape?"
        );

        let clean = |s: &str| {
            s.trim_matches(|c| c == '"' || c == '\\' || c == '\'')
                .to_string()
        };
        let mut named: Vec<String> = Vec::new();
        for c in &writes {
            for tok in c.split_whitespace() {
                let tok = tok.trim_start_matches(['"', '\\']);
                if let Some(p) = tok
                    .strip_prefix("project:")
                    .or_else(|| tok.strip_prefix("proj:"))
                {
                    named.push(clean(p));
                }
            }
            let mut it = c.split_whitespace();
            while let Some(tok) = it.next() {
                if tok == "--project" {
                    if let Some(p) = it.next() {
                        named.push(clean(p));
                    }
                }
            }
        }
        assert!(
            !named.is_empty(),
            "the guide must show a project: on an add — did the sugar change?"
        );

        for p in &named {
            assert!(
                created.contains(p),
                "a documented command files a task into project {p:?}, which no documented \
                 `tasqx init` creates — under D23 that command exits 4 for the reader. \
                 Documented inits: {created:?}"
            );
        }
    }

    /// Three surfaces claim `archive` refuses an already-archived project — the
    /// Commands page, the terminal note behind `tasqx archive --help`, and the
    /// worked example's output block — and this asks the engine whether that is
    /// true, rather than trusting the prose.
    ///
    /// The review of #53 is why it exists. Both doc surfaces asserted "no write
    /// may name it" about an archived project while `project.archive` was itself
    /// a write that named one and answered `ok`: a documented protection the
    /// code did not have, which is the failure class this repo fails builds
    /// over. Deleting the refusal from the engine now reddens the docs too,
    /// which is the only arrangement in which the sentence stays true.
    ///
    /// The output block is compared to the engine's real message rather than to
    /// a fragment, because the page shows it as literal terminal output — a
    /// reader who cannot find that string in their own terminal has been taught
    /// something false about the tool.
    #[test]
    fn the_archive_pages_refusal_is_the_one_the_engine_actually_gives() {
        let e = tasqx_core::Engine::open_in_memory().expect("in-memory store");
        tasqx_core::dispatch(&e, "project.create", &serde_json::json!({ "name": "old" }))
            .expect("create");
        tasqx_core::dispatch(&e, "project.archive", &serde_json::json!({ "name": "old" }))
            .expect("first archive");
        let err =
            tasqx_core::dispatch(&e, "project.archive", &serde_json::json!({ "name": "old" }))
                .expect_err("a second archive of the same project must be refused");

        // The verb's own note in `cmddoc` names the refusal, so a reader of
        // `--help` learns it without running the command twice.
        let note = crate::cmddoc::after_help("archive");
        assert!(
            note.contains("already archived"),
            "`tasqx archive --help` does not mention the refusal: {note}"
        );

        // And the guide's worked output block is the engine's message verbatim,
        // with only the project name swapped for the one the page uses.
        let doc = generate();
        let shown = err.message.replace("old", "home");
        assert!(
            doc.contains(&esc(&shown)),
            "the Commands page shows an `archive` refusal the engine does not give.\n\
             engine: {shown}"
        );
    }

    /// tasqx audit 2026-09 #226.3: the archive page claimed "no verb may name
    /// [an archived project] any more" — but `list`, `report` and `agenda`
    /// still show it and its tasks (verified against the binary); only
    /// WRITES are refused. `cmddoc`'s terminal NOTE carried the identical
    /// overbroad claim and is guarded the same way in `cmddoc::tests`.
    #[test]
    fn archive_page_scopes_the_refusal_to_writes() {
        let doc = generate();
        // One verb, one `<section class="ref" id="cli-archive">`, which ends
        // where the next section opens.
        let section = doc
            .split("id=\"cli-archive\"")
            .nth(1)
            .expect("an archive section")
            .split("<section class=\"ref\"")
            .next()
            .expect("the archive section ends before the next one");
        assert!(
            !section.contains("no verb may name"),
            "the archive page must not claim every verb refuses an archived \
             project — list/report/agenda still show it: {section}"
        );
        assert!(
            section.to_lowercase().contains("read")
                || section.contains("list")
                || section.contains("report"),
            "the archive page must say that reads still see an archived \
             project and its tasks: {section}"
        );
    }

    /// tasqx audit 2026-09 #226.6: neither `tasqx manual` nor this HTML guide
    /// named a single dashboard key binding beyond `p`/enter/q/esc/ctrl-c —
    /// the rest (`1-8`, `tab`/`S-tab`, `j`/`k`, `g`/`G`, `r`/`R`, `w`, `l`)
    /// lived only behind the in-screen `?` overlay. Checked against the live
    /// `KEYS` table, not a retyped list, so a binding added to the overlay
    /// and not here fails the build.
    #[test]
    fn dashboard_page_names_every_key_binding_from_the_live_table() {
        let doc = generate();
        for key in crate::tui::dashboard::KEYS {
            let spelling = key.keys.split(" / ").next().unwrap_or(key.keys);
            assert!(
                doc.contains(&esc(spelling)),
                "the dashboard page never mentions the `{}` binding (help: {:?})",
                key.keys,
                key.help
            );
        }
    }

    /// Every urgency gauge in a sample agrees with the renderer at the
    /// figure printed beside it. The agenda block drew `▄▄▃▁ 12.0` from before
    /// D119, where the bar fills at `urgency::DUE_WEIGHT` (12) — a sample
    /// nobody had replayed, showing a screen this build cannot produce (#562).
    #[test]
    fn every_gauge_in_a_sample_matches_the_renderer() {
        let wrong = crate::render::gauges_disagreeing(&generate());
        assert!(
            wrong.is_empty(),
            "{} gauge(s) in the guide disagree with the renderer:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }

    /// The page must be substantial — a guard against a refactor quietly rendering
    /// an empty shell that still passes every structural assertion above.
    #[test]
    fn docs_page_has_real_content() {
        let doc = generate();
        assert!(
            doc.len() > 40_000,
            "the guide is suspiciously small: {} bytes",
            doc.len()
        );
        assert!(
            doc.matches("class=\"snip\"").count() >= 25,
            "too few worked examples"
        );
    }

    // ---- the shell's building blocks ---------------------------------------

    /// The id slug collapses, where [`h3`]'s deliberately does not.
    #[test]
    fn slug_collapses_punctuation_and_trims_it() {
        assert_eq!(slug("Global flags"), "global-flags");
        assert_eq!(slug("--no-daemon"), "no-daemon");
        assert_eq!(slug("--theme <name>"), "theme-name");
        assert_eq!(slug("Install &amp; quickstart"), "install-amp-quickstart");
        assert_eq!(slug("!!!"), "");
    }

    /// One flag cell, two halves — over every shape [`GLOBAL_FLAGS`] really
    /// holds, so a new one that parses badly is a red test and not a badge
    /// reading `--socket <addr>` next to an empty name.
    #[test]
    fn split_flag_separates_the_name_from_the_value_shape() {
        assert_eq!(
            split_flag("<code>--json</code>"),
            ("--json".into(), "flag".into())
        );
        assert_eq!(
            split_flag("<code>--theme &lt;name&gt;</code>"),
            ("--theme".into(), "<name>".into())
        );
        assert_eq!(
            split_flag("<code>--socket &lt;addr&gt;</code>"),
            ("--socket".into(), "<addr>".into())
        );
        assert_eq!(
            split_flag("<code>--help</code>, <code>--version</code>"),
            ("--help, --version".into(), "flag".into())
        );
        // Every real row yields a name that starts like a flag: the guarantee
        // the id scheme and the badge both rest on.
        for (flag, _) in GLOBAL_FLAGS {
            let (name, ty) = split_flag(flag);
            assert!(name.starts_with("--"), "`{flag}` parsed to name `{name}`");
            assert!(!ty.is_empty(), "`{flag}` parsed to an empty type badge");
        }
    }

    /// A parameter row is addressable, typed, and says whether it is needed.
    #[test]
    fn param_table_renders_an_addressable_typed_row() {
        let html = param_table(
            "demo",
            &[
                Param {
                    name: "filter",
                    ty: "string",
                    required: true,
                    default: None,
                    html_desc: "A <code>filter</code> expression.",
                },
                Param {
                    name: "limit",
                    ty: "integer",
                    required: false,
                    default: Some("50"),
                    html_desc: "How many rows.",
                },
            ],
        );
        assert!(html.contains("id=\"demo-filter\""), "{html}");
        assert!(html.contains("data-param=\"filter\""), "{html}");
        assert!(html.contains("<span class=\"pill req\">required</span>"));
        assert!(html.contains("<span class=\"pill opt\">optional</span>"));
        assert!(html.contains("<span class=\"badge\">string</span>"));
        assert!(html.contains("default <code>50</code>"), "{html}");
        // The required row has no default, and says nothing about one.
        assert_eq!(html.matches("class=\"pdef\"").count(), 1);
        // Descriptions are trusted markup, like every other cell in this file.
        assert!(html.contains("A <code>filter</code> expression."));
    }

    /// A name is the one thing in a parameter row that a future generated
    /// reference could take from a schema rather than a literal here.
    #[test]
    fn param_table_escapes_the_name_and_the_type() {
        let html = param_table(
            "demo",
            &[Param {
                name: "<script>x</script>",
                ty: "<img>",
                required: false,
                default: Some("\"quoted\""),
                html_desc: "safe",
            }],
        );
        assert!(!html.contains("<script>"), "raw markup survived: {html}");
        assert!(!html.contains("<img>"), "raw markup survived: {html}");
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&quot;quoted&quot;"));
    }

    /// A parameter/field list is a real definition list, not anonymous divs:
    /// every `dt` (name and type) is paired with a following `dd`
    /// (description), so assistive tech announces "term, definition" instead
    /// of two unlabelled groups (PR #61 review).
    #[test]
    fn every_param_row_pairs_a_dt_with_a_following_dd() {
        let params = param_table(
            "demo",
            &[Param {
                name: "filter",
                ty: "string",
                required: true,
                default: None,
                html_desc: "A filter expression.",
            }],
        );
        let fields = field_list(&[("head one".to_string(), "desc one".to_string())]);
        for html in [params, fields] {
            assert!(html.starts_with("<dl class=\"params\">"), "{html}");
            let dts = html.matches("<dt class=\"param-h\">").count();
            let dds = html.matches("<dd class=\"param-d\">").count();
            assert_eq!(dts, dds, "{html}");
            assert!(dts > 0, "no rows rendered: {html}");
            for after in html.split("<dt class=\"param-h\">").skip(1) {
                let (_, rest) = after.split_once("</dt>").expect("a closed dt");
                assert!(
                    rest.trim_start().starts_with("<dd class=\"param-d\">"),
                    "a dt with no following dd: {html}"
                );
            }
        }
    }

    /// Exactly one tab and one panel start active, and it is the first.
    #[test]
    fn tabs_open_on_the_first_panel_only() {
        let html = tabs(&[("CLI", "<p>one</p>"), ("JSON API", "<p>two</p>")]);
        assert_eq!(html.matches("class=\"tab active\"").count(), 1);
        assert_eq!(html.matches("class=\"tabpanel active\"").count(), 1);
        assert!(
            html.find("class=\"tab active\"").unwrap() < html.find("JSON API").unwrap(),
            "the second tab opened instead of the first: {html}"
        );
        // Button and panel are paired by label, which is what the stored
        // preference is spelled in.
        assert_eq!(html.matches("data-tab=\"JSON API\"").count(), 2);
    }

    /// Labels reach an attribute as well as the visible strip.
    #[test]
    fn tabs_escape_their_labels() {
        let html = tabs(&[("a\"b", "<p>x</p>")]);
        assert!(
            !html.contains("a\"b"),
            "an unescaped quote broke out: {html}"
        );
        assert!(html.contains("data-tab=\"a&quot;b\""));
    }

    /// The two-column block keeps prose and code in their own columns, and
    /// escapes the one string a caller supplies as text.
    #[test]
    fn ref_section_splits_prose_from_code() {
        let html = ref_section("demo-id", "A <title>", "<p>prose</p>", "<p>code</p>");
        assert!(html.contains("<section class=\"ref\" id=\"demo-id\">"));
        assert!(html.contains("<div class=\"ref-main\">"));
        assert!(html.contains("<div class=\"ref-code\"><p>code</p></div>"));
        assert!(
            html.contains("id=\"h-demo-id\""),
            "the heading is not linkable"
        );
        assert!(!html.contains("<title>"), "raw markup in the title: {html}");
        assert!(html.contains("A &lt;title&gt;"));
        // Prose precedes code in the source, so a JavaScript-less reader and a
        // one-column phone both get the sentence before the example.
        assert!(html.find("ref-main").unwrap() < html.find("ref-code").unwrap());
    }

    /// A terminal block is copyable, and its text is escaped by its caller —
    /// this asserts the wrapper the copy handler looks for.
    #[test]
    fn term_block_is_a_copyable_terminal() {
        let html = term_block(&esc("tasqx api < req.json"));
        assert!(html.contains("<div class=\"termbox\">"));
        assert!(html.contains("<button class=\"copy\" type=\"button\">Copy</button>"));
        assert!(html.contains("<pre class=\"term\">"));
        assert!(html.contains("api &lt; req.json"), "{html}");
    }

    /// The wrapper is the whole point: a bare `<pre class="term">` has no
    /// background of its own, so in LIGHT mode the screen would be drawn in
    /// `--term-fg` (a pale grey) on the page's white card. The container that
    /// carries `--term-bg` is what makes the block a terminal in both themes,
    /// and the command line above it is what keeps the screen from being read
    /// as the output of a neighbouring snippet.
    #[test]
    fn a_captured_screen_is_placed_in_a_terminal_container_with_its_command() {
        let html = term_screen("tasqx list", "list");
        assert!(html.starts_with("<div class=\"snip\">"), "{}", &html[..80]);
        assert!(html.contains("<pre class=\"cmd\"><code>tasqx list</code></pre>"));
        assert!(html.contains("<pre class=\"term\">"));
        // Real captured output, rendered as styled text rather than escaped raw
        // ANSI: a span with a colour, and no escape byte anywhere.
        assert!(
            html.contains("<span style=\"color:"),
            "no styled run in {name}",
            name = "list"
        );
        assert!(!html.contains('\u{1b}'));
        // The CSS this markup depends on is in the same file; if `.snip` ever
        // stops carrying the terminal background, this is the pair to fix.
        assert!(css().contains(".snip { margin"));
        assert!(css().contains("background: var(--term-bg)"));
    }

    /// The placeholder panel is marked, because the script refuses to open a
    /// remembered tab that turns out to be one.
    #[test]
    fn a_placeholder_panel_is_marked_as_one() {
        let html = tabs(&[("CLI", "<p>real</p>"), ("JSON API", &soon("Coming later."))]);
        assert!(html.contains("<div class=\"soon\"><p>Coming later.</p></div>"));
        let js = js();
        assert!(
            js.contains("querySelector('.soon')"),
            "a remembered tab label is applied without checking for a placeholder"
        );
    }

    // ---- the shell, as rendered --------------------------------------------

    /// The global flags render as parameter rows, each one addressable by name.
    /// This is the one place [`param_table`] is exercised by a real page, so it
    /// is the one place a broken id scheme would show.
    #[test]
    fn the_global_flags_render_as_addressable_parameter_rows() {
        let page = page_commands();
        for (flag, _) in GLOBAL_FLAGS {
            let (name, _) = split_flag(flag);
            assert!(
                page.contains(&format!("id=\"global-flags-{}\"", slug(&name))),
                "no addressable row for `{name}`"
            );
            assert!(
                page.contains(&format!("data-param=\"{}\"", esc(&name))),
                "`{name}` is not findable by the sidebar search"
            );
        }
        // Counted over the global-flags rows alone. The page carries a
        // parameter row for every argument of every verb now (the CLI
        // reference, #646), so counting `class="param"` across the whole page
        // would be counting clap's surface, not this table's.
        assert_eq!(
            page.matches("id=\"global-flags-").count(),
            GLOBAL_FLAGS.len(),
            "the parameter list and GLOBAL_FLAGS disagree"
        );
    }

    /// Both reference pages use the two-column template, more than once each.
    /// One use is an accident; two is the template being the shape of the page.
    #[test]
    fn both_reference_pages_use_the_two_column_template() {
        for (name, page) in [("api", page_api()), ("mcp", page_mcp())] {
            assert!(
                page.matches("<section class=\"ref\"").count() >= 2,
                "the {name} page uses the reference template {} time(s)",
                page.matches("<section class=\"ref\"").count()
            );
            assert!(
                page.contains("class=\"tabstrip\""),
                "the {name} page has no code tabs"
            );
        }
    }

    /// Every id the document defines is unique.
    ///
    /// Not pedantry: the sidebar search jumps with `getElementById`, `:target`
    /// is the no-JavaScript page fallback, and both silently pick the first of
    /// a pair. A second `id="api"` would send half the guide's links to the
    /// wrong place with every structural guard still green.
    #[test]
    fn every_id_in_the_document_is_unique() {
        let doc = generate();
        let mut seen: Vec<&str> = Vec::new();
        let mut dupes: Vec<&str> = Vec::new();
        for (i, _) in doc.match_indices("id=\"") {
            let rest = &doc[i + 4..];
            let end = rest.find('"').expect("an id must be terminated");
            let id = &rest[..end];
            if seen.contains(&id) {
                dupes.push(id);
            } else {
                seen.push(id);
            }
        }
        assert!(dupes.is_empty(), "duplicate ids: {dupes:?}");
        assert!(seen.len() > 20, "suspiciously few ids: {}", seen.len());
    }

    /// The theme switch offers three modes and the stylesheet answers all
    /// three — two overrides plus the system default it must not replace.
    #[test]
    fn the_theme_switch_and_the_stylesheet_agree_on_three_modes() {
        let doc = generate();
        for mode in ["light", "dark", "system"] {
            assert!(
                doc.contains(&format!("data-theme-set=\"{mode}\"")),
                "no `{mode}` button in the top bar"
            );
        }
        assert!(
            doc.contains("html[data-theme=\"light\"] {"),
            "no light override"
        );
        assert!(
            doc.contains("html[data-theme=\"dark\"] {"),
            "no dark override"
        );
        assert!(
            doc.contains("@media (prefers-color-scheme: dark)"),
            "`system` has nothing to fall back to"
        );
        // Auto is the ABSENCE of the attribute, not a third palette: the
        // script must be able to take the override off again.
        assert!(
            doc.contains("removeAttribute('data-theme')"),
            "`system` cannot be returned to"
        );
    }

    /// Every reach for `localStorage` is inside a `try`.
    ///
    /// On a `file://` document — how this guide is opened — a browser set to
    /// block site data throws on the *property access*, before any method is
    /// called. An unguarded read at the top of the script takes the whole page
    /// down to a blank screen, and the feature it was serving is a remembered
    /// tab. Two call sites, both wrapped, and nothing else may touch it.
    #[test]
    fn stored_preferences_never_take_the_page_down() {
        let js = js();
        assert_eq!(
            js.matches("localStorage").count(),
            2,
            "localStorage is reached outside readStore/writeStore"
        );
        assert!(js.contains("try { return window.localStorage.getItem"));
        assert!(js.contains("try { window.localStorage.setItem"));
        // Both helpers swallow the failure rather than rethrowing it.
        assert!(
            js.matches("catch (e)").count() >= 2,
            "a storage helper rethrows"
        );
    }

    /// The search has no index to fall out of date: it queries the document.
    #[test]
    fn the_sidebar_search_reads_the_document_itself() {
        let doc = generate();
        assert!(
            doc.contains("'.page h3, .page h4, .page [data-param]'"),
            "the search does not scan headings and parameter rows"
        );
        assert!(
            doc.contains("id=\"navq\""),
            "no search input in the sidebar"
        );
        assert!(
            doc.contains("data-param=\""),
            "nothing carries data-param, so half the search matches nothing"
        );
        // Nothing may be fetched to make it work.
        assert!(!doc.contains("fetch("), "the search fetches something");
        assert!(
            !doc.contains("XMLHttpRequest"),
            "the search fetches something"
        );
    }

    /// One copy handler, both block shapes, and a fallback for the browsers
    /// (and file:// contexts) where the async clipboard is unavailable.
    #[test]
    fn copy_buttons_cover_snippets_and_terminal_blocks() {
        let doc = generate();
        assert!(
            doc.contains("closest('.snip, .termbox')"),
            "the copy handler knows only one kind of block"
        );
        assert!(doc.contains("navigator.clipboard"), "no clipboard path");
        assert!(
            doc.contains("createElement('textarea')"),
            "no fallback where the clipboard API is missing"
        );
        assert!(doc.contains("'Copied'"), "no copied state");
    }

    /// The top bar cannot wrap, and on a small phone it sheds its subtitle.
    ///
    /// Measured on a real 390px viewport, not a 390-wide crop of one: headless
    /// Chrome on macOS clamps `--window-size` to 500px, so a screenshot at 390
    /// is a 500px layout with the right edge cut off and *looks* broken while
    /// the layout is fine. Driven over CDP with the metrics overridden, the
    /// document's `scrollWidth` equals `innerWidth` on every page — but at
    /// 320px the brand wrapped to three lines and spilled out of the bar,
    /// because the bar has a FIXED height that `nav`'s and `.ref-code`'s
    /// sticky offsets are measured from. Anything in it that can wrap moves
    /// the whole page's idea of where the viewport starts.
    #[test]
    fn the_top_bar_cannot_wrap_out_of_its_own_height() {
        let doc = generate();
        assert!(
            doc.contains("header.top { position: sticky; top: 0; z-index: 40; height: 3.25rem;"),
            "the bar's height is what the sticky offsets below it are measured from"
        );
        let (_, brand) = doc.split_once(".brand {").expect("no brand rule");
        let brand = &brand[..brand.find('}').expect("unterminated brand rule")];
        assert!(
            brand.contains("white-space: nowrap"),
            "the brand can wrap and push itself out of a fixed-height bar: {brand}"
        );
        assert!(
            brand.contains("text-overflow: ellipsis") && brand.contains("min-width: 0"),
            "a nowrap brand that cannot shrink overflows instead: {brand}"
        );
        let (_, small) = doc
            .split_once("@media (max-width: 30rem) {")
            .expect("no small-phone breakpoint");
        assert!(
            small.contains(".brand .muted { display: none; }"),
            "the bar keeps its subtitle where there is no room for it"
        );
        // The switch itself stays: it is the only way to overrule the system
        // palette, and it must not be what gets dropped.
        for mode in ["light", "dark", "system"] {
            assert!(doc.contains(&format!("data-theme-set=\"{mode}\"")));
        }
    }

    /// Under 60rem the sidebar is a drawer, and the button that opens it is
    /// only visible there.
    #[test]
    fn the_sidebar_becomes_a_reachable_drawer_on_a_phone() {
        let doc = generate();
        let (_, narrow) = doc
            .split_once("@media (max-width: 60rem) {")
            .expect("no narrow breakpoint");
        assert!(
            doc.contains("#navtoggle { display: none;"),
            "the drawer button shows on a wide screen too"
        );
        assert!(
            narrow.contains("#navtoggle { display: block; }"),
            "nothing opens the drawer under 60rem"
        );
        assert!(
            narrow.contains("nav.open { display: block; }"),
            "the drawer has no open state"
        );
        assert!(
            narrow.contains(".ref-code { position: static; }"),
            "the sticky code column stays sticky in one column, where it overlaps"
        );
        assert!(
            doc.contains("<button id=\"navtoggle\""),
            "no drawer button in the top bar"
        );
    }

    /// Every cell of every `table.grid` the site renders — the builders' and
    /// the markdown pages' alike — names its column in `data-label` (for the
    /// stacked phone layout) and in `headers` (for assistive tech, which
    /// keeps a visually hidden `thead` instead of a `display: none` one — PR
    /// #61 review). A cell without either is a bare value with no name.
    #[test]
    fn every_grid_cell_is_labelled_with_its_column_header() {
        fn text(html: &str) -> String {
            let mut out = String::new();
            let mut tag = false;
            for c in html.chars() {
                match c {
                    '<' => tag = true,
                    '>' => tag = false,
                    _ if !tag => out.push(c),
                    _ => {}
                }
            }
            out.trim().to_string()
        }
        let doc = generate();
        let mut cells = 0usize;
        let mut all_th_ids: Vec<&str> = Vec::new();
        for table in doc.split("<table class=\"grid\">").skip(1) {
            let table = table.split("</table>").next().expect("a closed table");
            let (head, body) = table.split_once("</thead>").expect("a table head");
            let mut headers: Vec<String> = Vec::new();
            let mut th_ids: Vec<&str> = Vec::new();
            for th in head.split("<th ").skip(1) {
                let (open, rest) = th.split_once('>').expect("a closed th open tag");
                let id = open
                    .split_once("id=\"")
                    .and_then(|(_, r)| r.split('"').next())
                    .unwrap_or_else(|| panic!("a th with no id: <th {open}>"));
                th_ids.push(id);
                headers.push(text(rest.split("</th>").next().unwrap_or(rest)));
            }
            all_th_ids.extend(&th_ids);
            for row in body.split("<tr>").skip(1) {
                for (i, td) in row.split("<td").skip(1).enumerate() {
                    let label = td
                        .split_once("data-label=\"")
                        .filter(|(before, _)| !before.contains('>'))
                        .and_then(|(_, rest)| rest.split('"').next())
                        .unwrap_or_else(|| panic!("an unlabelled cell under {headers:?}: <td{td}"));
                    assert!(!label.is_empty(), "an empty label under {headers:?}");
                    // A backtick span a builder forgot to run through
                    // `describe`/`md` shows up as a literal backtick.
                    assert!(
                        !td.contains('`'),
                        "a raw backtick in a cell under {headers:?}: <td{td}"
                    );
                    assert_eq!(
                        Some(label),
                        headers.get(i).map(String::as_str),
                        "cell {i} is labelled for the wrong column: <td{td}"
                    );
                    let headers_attr = td
                        .split_once("headers=\"")
                        .filter(|(before, _)| !before.contains('>'))
                        .and_then(|(_, rest)| rest.split('"').next())
                        .unwrap_or_else(|| {
                            panic!("a cell with no headers attribute under {headers:?}: <td{td}")
                        });
                    assert_eq!(
                        Some(headers_attr),
                        th_ids.get(i).copied(),
                        "cell {i}'s headers attribute names the wrong th: <td{td}"
                    );
                    cells += 1;
                }
            }
        }
        assert!(cells > 500, "only {cells} grid cells checked");
        let unique: std::collections::HashSet<&&str> = all_th_ids.iter().collect();
        assert_eq!(
            unique.len(),
            all_th_ids.len(),
            "duplicate th ids across the site"
        );
        let (_, narrow) = doc
            .split_once("@media (max-width: 60rem) {")
            .expect("no narrow breakpoint");
        assert!(
            narrow.contains("table.grid td::before { content: attr(data-label);"),
            "a stacked cell does not show its label"
        );
        assert!(
            !narrow.contains("table.grid thead { display: none;"),
            "the header row is display:none, not just visually hidden, so a screen reader loses it too"
        );
    }

    /// The architecture figure does not fit a phone at a legible size, so it
    /// scrolls — and says so, since a phone draws no scrollbar to hint at it.
    #[test]
    fn the_architecture_figure_says_it_scrolls_on_a_phone() {
        let doc = generate();
        assert!(doc.contains("<figure class=\"arch\"><figcaption class=\"scrollhint\">"));
        assert!(doc.contains(".scrollhint { display: none;"));
        let (_, narrow) = doc
            .split_once("@media (max-width: 40rem) {")
            .expect("no figure breakpoint");
        assert!(narrow.contains(".scrollhint { display: block;"));
    }
}
