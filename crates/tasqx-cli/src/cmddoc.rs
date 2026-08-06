//! The single source of command documentation (DESIGN spec 2026-07-17).
//!
//! One `CmdDoc` per CLI subcommand. Three surfaces render from it: clap's
//! `after_help` (both `-h` and `--help`), `tasqx manual`, and the HTML `docs`
//! verb table. The guards at the bottom assert it stays in lockstep with the
//! real clap surface, so an undocumented verb — or a broken example — fails the
//! build rather than shipping.

#[derive(Clone, Copy)]
pub enum RunKind {
    /// Idempotent / read-only. The integration guard executes it on a temp DB.
    Safe,
    /// Mutating, long-running, or illustrative. Structurally checked only.
    NoRun,
}

#[derive(Clone, Copy)]
pub struct Example {
    pub cmd: &'static str,
    pub note: Option<&'static str>,
    /// Whether the executable-examples guard runs this for real. It is read by
    /// `tests/help.rs`, which selects the `Safe` entries straight out of
    /// `COMMAND_REF`. It used to be `#[allow(dead_code)]` because that guard
    /// hand-copied its own list — and that copy had drifted to thirteen of the
    /// twenty-seven Safe examples before the crate grew a lib target the test
    /// could import.
    pub run: RunKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    GettingStarted,
    Projects,
    Capturing,
    Dates,
    Filters,
    Reminders,
    Reports,
    Daemon,
    Automation,
    JsonApi,
    Completion,
}

impl Topic {
    pub const ALL: [Topic; 11] = [
        Topic::GettingStarted,
        Topic::Projects,
        Topic::Capturing,
        Topic::Dates,
        Topic::Filters,
        Topic::Reminders,
        Topic::Reports,
        Topic::Daemon,
        Topic::Automation,
        Topic::JsonApi,
        Topic::Completion,
    ];
    pub fn slug(&self) -> &'static str {
        match self {
            Topic::GettingStarted => "getting-started",
            Topic::Projects => "projects",
            Topic::Capturing => "capturing",
            Topic::Dates => "dates",
            Topic::Filters => "filters",
            Topic::Reminders => "reminders",
            Topic::Reports => "reports",
            Topic::Daemon => "daemon",
            Topic::Automation => "automation",
            Topic::JsonApi => "json-api",
            Topic::Completion => "completion",
        }
    }
    pub fn title(&self) -> &'static str {
        match self {
            Topic::GettingStarted => "Getting started",
            Topic::Projects => "Projects",
            Topic::Capturing => "Capturing tasks",
            Topic::Dates => "Dates & recurrence",
            Topic::Filters => "Filter grammar",
            Topic::Reminders => "Reminders",
            Topic::Reports => "Reports & charts",
            Topic::Daemon => "Daemon & watch",
            Topic::Automation => "Automation (MCP & API)",
            Topic::JsonApi => "JSON API",
            Topic::Completion => "Shell completion",
        }
    }
}

pub struct CmdDoc {
    pub verb: &'static str,
    pub aliases: &'static [&'static str],
    pub method: &'static str,
    pub summary: &'static str,
    pub usage: &'static str,
    pub examples: &'static [Example],
    pub notes: &'static [&'static str],
    pub see_also: &'static [&'static str],
    pub topic: Topic,
}

// Ergonomic shorthands for the table below.
use RunKind::{NoRun, Safe};
const fn ex(cmd: &'static str) -> Example {
    Example {
        cmd,
        note: None,
        run: Safe,
    }
}
/// A `Safe` example with a note. Was `#[allow(dead_code)]` for as long as every
/// annotated example happened to be `NoRun`; `archive` is the first Safe one
/// that needs a note, so the allow is gone.
const fn exn(cmd: &'static str, note: &'static str) -> Example {
    Example {
        cmd,
        note: Some(note),
        run: Safe,
    }
}
const fn ex_norun(cmd: &'static str, note: &'static str) -> Example {
    Example {
        cmd,
        note: Some(note),
        run: NoRun,
    }
}

pub const COMMAND_REF: &[CmdDoc] = &[
    CmdDoc {
        verb: "init",
        aliases: &[],
        method: "project.create",
        summary: "Create a project — just a name, no folder.",
        usage: "tasqx init <name> [--desc <text>]",
        examples: &[
            ex("tasqx init keuken-verbouwen"),
            ex("tasqx init work --desc \"Day job\""),
            ex_norun("tasqx init home && tasqx use home", "claim then set default"),
        ],
        notes: &[
            "A project is just a name in the store — no folder is created.",
            "init claims the default project only if the store has none yet.",
        ],
        see_also: &["use", "add", "projects"],
        topic: Topic::Projects,
    },
    CmdDoc {
        verb: "add",
        aliases: &["a", "new"],
        method: "task.add",
        summary: "Capture a task — title plus inline sugar.",
        usage: "tasqx add <title…> [--project p] [--due d] [--scheduled s] [--wait w] [-p H|M|L] [-t tag]… [--repeat r] [--remind r] [-e est]",
        examples: &[
            ex("tasqx add Buy milk"),
            ex("tasqx add Ship it due:friday +api !high --project work"),
            ex("tasqx add Water plants repeat:\"every 3 days\""),
            ex_norun("tasqx add Call bank due:\"friday 9am\" --remind -30m", "reminder 30m before due"),
        ],
        notes: &[
            "Inline sugar: `+tag`, `project:p` (or `proj:`), `!high`, `due:…`, `est:4h`, `repeat:…`, `remind:…`.",
            "A bare add lands in the default project (`tasqx use` to change it).",
        ],
        see_also: &["modify", "use", "list", "next"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "modify",
        aliases: &["mod", "m", "edit"],
        method: "task.modify",
        summary: "Change a task — set fields or --clear them.",
        usage: "tasqx modify <ref> [words/sugar…] [--project p] [--due d] [--scheduled s] [--wait w] [-p H|M|L] [-t tag]… [--repeat r] [--remind r] [-e est] [--clear <field>]… [--expected-rev N]",
        examples: &[
            ex_norun("tasqx modify 42 due:friday !high est:4h", "set fields"),
            ex_norun("tasqx modify 42 --clear due --clear remind", "clear fields"),
            ex_norun("tasqx modify 42 repeat:\"every monday\"", "set a recurrence"),
        ],
        notes: &[
            "Setting is `due:friday`/`--due friday`; removal is only ever `--clear <field>` — there is no magic empty value.",
            "`--clear` covers the steering fields only. `modify 42 +api` adds a tag; taking one off is `tasqx untag 42 api`.",
            "`--expected-rev` fails with conflict (exit 5) if the task moved on.",
        ],
        see_also: &["add", "show", "why", "untag"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "list",
        aliases: &["ls", "l"],
        method: "task.list",
        summary: "List tasks matching a filter.",
        usage: "tasqx list [filter…]",
        examples: &[
            ex("tasqx list"),
            ex("tasqx list project:work status:pending +api"),
            ex("tasqx list due.before:friday"),
        ],
        notes: &[
            "Bare `tasqx` is `tasqx list` over the working set.",
            "A value containing a space is double-quoted, and the quotes must reach tasqx: `tasqx list 'project:\"Home Renovation\"'`. Nothing is guessed back together, so the shell-stripped form is refused rather than answered wrongly.",
        ],
        see_also: &["next", "report", "show"],
        topic: Topic::Filters,
    },
    CmdDoc {
        verb: "agenda",
        aliases: &["ag", "cal"],
        method: "task.list",
        summary: "What is coming up, when — `list` ordered by time.",
        usage: "tasqx agenda [filter…] [--days N]",
        examples: &[
            // `Safe`, and it runs against the same scratch store the `add`
            // examples above have already filled — including `add Ship it
            // due:friday`, so `safe_examples_all_exit_zero` renders a real day
            // group rather than an empty agenda.
            ex("tasqx agenda"),
            ex("tasqx agenda --days 3"),
            ex("tasqx agenda project:work"),
        ],
        notes: &[
            "A task is placed on the EARLIER of its `due` and `scheduled` — the first day it asks anything of you — and the WHEN column says which of the two that was.",
            "Overdue tasks are always shown, whatever `--days` says: a horizon is a question about the future.",
            "This view holds a row back for exactly two reasons, and both are COUNTED under the table rather than dropped in silence: no date at all, and past the horizon. Each count names the way to see them — the horizon line quotes the exact `--days` that reaches the furthest one, or, when the row is further out than the widest window `--days` accepts, says so and points at `tasqx list`.",
            "Done and cancelled tasks are left out unless your filter names a status, the same rule `report` applies to cancelled tasks (D24). `tasqx agenda status:done` shows them.",
            "Days are UTC days, because a date typed without a time is stored as midnight UTC; grouping by local time would file `--due 2026-08-05` under the 4th west of Greenwich.",
        ],
        see_also: &["list", "next", "add", "modify"],
        // Its own topic page is the one about `due`/`scheduled`, which is the
        // entire subject of this verb — and until now the only topic in the
        // manual with no command on it.
        topic: Topic::Dates,
    },
    CmdDoc {
        verb: "next",
        aliases: &[],
        method: "task.list",
        summary: "The one highest-urgency unblocked task.",
        usage: "tasqx next",
        examples: &[ex("tasqx next")],
        notes: &["The single highest-urgency unblocked task — the \"what now\" button."],
        see_also: &["list", "why", "start"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "dashboard",
        aliases: &["dash"],
        method: "task.list + report.summary + project.list + event.list",
        summary: "Open the overview screen, or ask for its panels as data.",
        usage: "tasqx dashboard [--json]",
        examples: &[
            // `NoRun` for the screen, and `Safe` for the document — the split
            // is the point of this verb. The executable-examples guard runs a
            // Safe example with `Command::output()`, which gives it a piped
            // stdout; that is the exact situation the screen refuses and the
            // exact situation `--json` is built for.
            ex_norun("tasqx dashboard", "open the overview screen"),
            exn(
                "tasqx --json dashboard",
                "the same panels as one JSON document",
            ),
        ],
        notes: &[
            "The same screen a bare `tasqx` opens on a terminal. Spelling it explicitly works even when `dashboard.enabled` is off — that setting protects the meaning of the BARE invocation, and typing the verb is not a breaking change to anything.",
            "It needs a terminal of at least 56x14 on stdin AND stdout, and says which it got when it refuses. A bare `tasqx` in a window that small falls back to the working-set table instead, silently: whoever typed nothing did not ask for a dashboard.",
            "`--json` skips both of those checks, because it opens no screen. It is the only verb where `--json` decides whether the terminal gate applies, and it is what makes the panel data reachable from a script.",
            "Read-only, with one exception: `p` opens the picker, and Enter there starts the highlighted task. `q`, `esc` and ctrl-c all close.",
        ],
        see_also: &["list", "pick", "agenda", "chart"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "pick",
        aliases: &["p", "fzf"],
        method: "task.list + task.start",
        summary: "Choose a task on a full-screen list, and start it.",
        usage: "tasqx pick [filter…]",
        examples: &[
            // `NoRun`, like `config edit`, and for the same reason rather than
            // out of caution: the executable-examples guard runs each Safe
            // example with `Command::output()`, which gives it a piped stdout —
            // the exact situation this verb refuses with exit 2. There is no
            // non-interactive spelling of it to run instead, because the whole
            // command IS the screen. What the refusal does on that path is
            // covered for real by `help.rs::pick_refuses_a_piped_stdout_with_a_
            // nonzero_exit`, which drives the binary and asserts the code.
            ex_norun("tasqx pick", "choose from the working set and start it"),
            ex_norun("tasqx pick project:work +api", "narrow the candidates first"),
        ],
        notes: &[
            "Type to narrow: the query is a fuzzy SUBSEQUENCE match over id, title, project and tags, so `wac` finds `Write API conformance tests`. Whitespace splits it into terms that must all match.",
            "Enter STARTS the highlighted task — the one key on this screen with a side effect, and the same single-active rule `tasqx start` follows. Esc clears the query first, and only then leaves.",
            "Cancelling, and a filter that matches no task, both exit 4 having started nothing. `pick` exists to produce one task; when it produced none, saying ok would be a command reporting success for work it did not do.",
            "It needs a real terminal on stdin AND stdout, so `tasqx pick | …` and `$(tasqx pick)` refuse with exit 2 rather than writing escape codes into your pipe (D26). Non-interactively, `tasqx next` answers the same question and `tasqx start <ref>` acts on it.",
        ],
        see_also: &["next", "list", "start", "agenda"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "show",
        aliases: &["get"],
        method: "task.get",
        summary: "Show one task in full detail.",
        usage: "tasqx show <ref>",
        examples: &[ex("tasqx show 1")],
        notes: &["Full detail: tags, annotations, dependencies, blocked state, `_rev`."],
        see_also: &["modify", "why", "annotate"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "why",
        aliases: &[],
        method: "task.get",
        summary: "Explain a task's urgency score.",
        usage: "tasqx why <ref>",
        examples: &[ex("tasqx why 1")],
        notes: &["Explains the urgency score component by component (DESIGN D1)."],
        see_also: &["next", "show", "list"],
        topic: Topic::Reports,
    },
    CmdDoc {
        verb: "start",
        aliases: &["s"],
        method: "task.start",
        summary: "Mark a task active.",
        usage: "tasqx start <ref> [--keep] [--client TOOL] [--session-id ID] [--prompt-id ID] [--transcript-path PATH]",
        examples: &[
            ex_norun("tasqx start 1", "single-active by default"),
            ex_norun("tasqx start 1 --keep", "keep others running"),
            ex_norun(
                "tasqx start 1 --client 'claude-code 2.1' --session-id $SID",
                "record who is working, for token attribution",
            ),
        ],
        notes: &[
            "The correlation flags are for AI agents: they tell the attribution \
             engine which session and transcript to measure this interval from. \
             Without them a task is never attributed and reports zero tokens.",
            "--session-id and --transcript-path require --client, which is what \
             selects the transcript parser. Given without it, attribution would \
             store a permanent zero instead of a measurement.",
        ],
        see_also: &["stop", "done", "next"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "stop",
        aliases: &["st"],
        method: "task.stop",
        summary: "Pause an active task.",
        usage: "tasqx stop <ref>",
        examples: &[ex_norun("tasqx stop 1", "")],
        notes: &[],
        see_also: &["start", "done"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "done",
        aliases: &["d", "x", "complete"],
        method: "task.done",
        summary: "Complete a task.",
        usage: "tasqx done <ref> [--client TOOL] [--session-id ID] [--prompt-id ID] [--transcript-path PATH]",
        examples: &[
            ex_norun("tasqx done 1", "completes; spawns the next recurrence if any"),
            ex_norun(
                "tasqx done 1 --client 'claude-code 2.1' --session-id $SID",
                "close the interval an agent opened with the same ids",
            ),
        ],
        notes: &[
            "The correlation flags carry the same meaning as on `start`, and are \
             recorded per occurrence: a task can start and finish many times, and \
             attribution pairs the two events of one interval.",
        ],
        see_also: &["cancel", "reopen", "start"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "cancel",
        aliases: &["delete", "del", "rm"],
        method: "task.cancel",
        summary: "Cancel a task.",
        usage: "tasqx cancel <ref>",
        examples: &[
            ex_norun("tasqx cancel 1", "a cancelled dependency releases its dependents (D11)"),
            ex_norun("tasqx delete 1", "same thing — tasqx has no hard delete; reverse it with `reopen`"),
        ],
        notes: &[
            "There is no destructive delete. `delete`/`rm` are aliases for cancel: the task \
             keeps its history, stays in the event log, and `tasqx reopen <ref>` undoes it.",
        ],
        see_also: &["done", "reopen"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "reopen",
        aliases: &[],
        method: "task.reopen",
        summary: "Reopen a completed or cancelled task.",
        usage: "tasqx reopen <ref>",
        examples: &[ex_norun("tasqx reopen 1", "")],
        notes: &[],
        see_also: &["done", "cancel"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "undo",
        aliases: &["u"],
        method: "event.revert",
        summary: "Take back the last thing this store recorded.",
        usage: "tasqx undo",
        examples: &[
            ex_norun("tasqx undo", "reverses the newest event, and says which one"),
            ex_norun(
                "tasqx untag 42 api && tasqx undo",
                "the tag comes back off the shelf",
            ),
        ],
        notes: &[
            "It takes no ref, and that is the design: only the NEWEST event can be reversed exactly, because nothing has happened since to have read or overwritten what the inverse puts back.",
            "Four operations are undoable — `stop`, `untag`, `undep` and `annotate`. Every other one exits 5 naming itself and the verb that does take it back (`done` -> `tasqx reopen`, `modify` -> `tasqx show` then a second `modify`).",
            "Undo APPENDS: the event it reverses stays in the log and a new `undo` event lands behind it, so `tasqx chart` and the audit trail read `X happened, then it was undone`.",
            "There is no redo, so `tasqx undo` twice in a row exits 5: the second one would find the first undo as the newest event and the pair would toggle forever.",
            "It reverses the newest RECORDED event, which is not always the last command you typed. A command that changed nothing records nothing — `tasqx undep 1 2` where no such edge exists, or `tasqx start` on a task already running — so `undo` reaches past it to the previous change. That is why the answer names what it undid: read it before assuming it hit what you were aiming at.",
        ],
        see_also: &["untag", "undep", "reopen", "chart"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "annotate",
        aliases: &["note"],
        method: "annotation.add",
        summary: "Attach a timestamped note to a task.",
        usage: "tasqx annotate <ref> <text…>",
        examples: &[ex_norun("tasqx annotate 1 Called the plumber, waiting on a quote", "")],
        notes: &[],
        see_also: &["show", "modify"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "tag",
        aliases: &[],
        method: "tag.add",
        summary: "Attach tags to a task.",
        usage: "tasqx tag <ref> <tag…>",
        examples: &[
            ex_norun("tasqx tag 1 api release", "two tags, one call"),
            ex_norun("tasqx tag 1 +api", "the leading + is optional — same tag either way"),
        ],
        notes: &[
            "A tag is written the same way here as in `add`/`modify` sugar: `+api` and `api` name one tag, and duplicates collapse.",
            "Re-adding a tag the task already has is not an error — the answer is the resulting tag set, so nothing has to be guessed.",
        ],
        see_also: &["untag", "modify", "list", "show"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "untag",
        aliases: &[],
        method: "tag.remove",
        summary: "Remove tags from a task.",
        usage: "tasqx untag <ref> <tag…>",
        examples: &[
            ex_norun("tasqx untag 1 api", "removes it, and prints what remains"),
            ex_norun("tasqx untag 1 api release", "all or nothing: one unknown tag removes neither"),
        ],
        notes: &[
            "Removing a tag the task does not have exits 4 and removes nothing, naming the tags it does have. A typo may not answer ok.",
            "There is no `--clear tags`: a tag comes off by name, which is why this verb exists.",
        ],
        see_also: &["tag", "show", "list"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "dep",
        aliases: &[],
        method: "dependency.add",
        summary: "Make one task depend on another.",
        usage: "tasqx dep <ref> <depends_on>",
        examples: &[ex_norun("tasqx dep 2 1", "task 2 waits on task 1")],
        notes: &["`<ref>` becomes blocked until `<depends_on>` is done or cancelled."],
        see_also: &["undep", "show"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "undep",
        aliases: &[],
        method: "dependency.remove",
        summary: "Remove a dependency edge.",
        usage: "tasqx undep <ref> <depends_on>",
        examples: &[ex_norun("tasqx undep 2 1", "")],
        notes: &[],
        see_also: &["dep"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "use",
        aliases: &[],
        method: "project.use",
        summary: "Set the default project for bare adds.",
        usage: "tasqx use <name>",
        examples: &[ex_norun("tasqx use keuken-verbouwen", "move where a bare add lands")],
        notes: &["The project must already exist and not be archived. `tasqx projects` marks the default with `*`."],
        see_also: &["init", "projects", "add", "archive"],
        topic: Topic::Projects,
    },
    CmdDoc {
        verb: "archive",
        aliases: &[],
        method: "project.archive",
        summary: "Retire a project — out of rotation, tasks untouched.",
        usage: "tasqx archive <name>",
        examples: &[
            // `Safe`, and it is the only mutating example in the reference that
            // is: `safe_examples_all_exit_zero` runs the Safe set in
            // declaration order against a scratch store, where `init
            // keuken-verbouwen` (the first `init` example, and therefore that
            // store's default project) has already run. So this line executes
            // the default-clearing path for real on every test run, which is
            // the branch worth executing.
            exn(
                "tasqx archive keuken-verbouwen",
                "retire it; archiving your default project clears the default",
            ),
        ],
        notes: &[
            "Archiving is a shelf, not a delete: the tasks keep their history and their project, and `tasqx projects --all` still lists the project.",
            "An archived project is out of rotation — `use` refuses it (exit 5), and so does an `add`/`modify` that names it, and so does a second `archive` of it (`project is already archived`, exit 5). No verb may name an archived project, this one included; `store.import` restoring the flag from a document is the one write that still can.",
            "There is no `unarchive` verb and no `project.unarchive` method: among the project methods, archiving is one-way. `store.import` does write a project's `archived` flag from the document, so restoring a saved export un-archives one — a data restore, not an undo.",
            "Archiving the project that IS the default clears the default: a bare `tasqx add` then has no project until `tasqx use <project>`. The line says which of the two happened.",
        ],
        see_also: &["projects", "use", "init"],
        topic: Topic::Projects,
    },
    CmdDoc {
        verb: "projects",
        aliases: &[],
        method: "project.list",
        summary: "List projects (default marked with `*`).",
        usage: "tasqx projects [--all]",
        examples: &[
            ex("tasqx projects"),
            ex("tasqx projects --all"),
        ],
        notes: &["`--all` is the only way to see an archived project: without it the table shows the live ones, which is what `add` and `use` will accept."],
        see_also: &["init", "use", "archive"],
        topic: Topic::Projects,
    },
    CmdDoc {
        verb: "report",
        aliases: &[],
        method: "report.summary",
        summary: "Summary counts, optionally grouped, as text or HTML.",
        usage: "tasqx report [group_by] [filter…] [--all] [--html] [--out FILE]",
        examples: &[
            ex("tasqx report"),
            ex("tasqx report project"),
            ex("tasqx report --all"),
            ex_norun("tasqx report --html --out review.html", "self-contained HTML"),
            // The usage line always promised `[filter…]` alongside `--html`; for a
            // long time only the terminal path kept that promise. Documented as an
            // example so the scoped form is discoverable, not just legal.
            ex_norun("tasqx report +urgent --html --out sprint.html", "scoped HTML"),
        ],
        notes: &[
            "group_by ∈ project|status|priority. `--html` defaults to stdout.",
            "A filter scopes BOTH output modes — the HTML page and the terminal table \
             answer the same question.",
            "Cancelled tasks are not counted, unless you pass `--all` or your filter names a status.",
        ],
        see_also: &["chart", "list", "why"],
        topic: Topic::Reports,
    },
    CmdDoc {
        verb: "chart",
        aliases: &[],
        method: "event.list",
        summary: "Render throughput, heatmap, or burndown charts.",
        usage: "tasqx chart <throughput [--weeks n]|heatmap [--weeks n] [--year]|burndown [--days n] [--project p]>",
        examples: &[
            ex("tasqx chart throughput"),
            ex("tasqx chart heatmap --year"),
            ex("tasqx chart burndown --days 30"),
        ],
        notes: &[],
        see_also: &["report"],
        topic: Topic::Reports,
    },
    CmdDoc {
        verb: "theme",
        aliases: &[],
        method: "— (no store)",
        summary: "List or preview terminal themes.",
        usage: "tasqx theme <list|show [name]|set <name>>",
        examples: &[
            ex("tasqx theme list"),
            ex("tasqx theme show nord"),
        ],
        notes: &[],
        see_also: &["report", "manual"],
        topic: Topic::Reports,
    },
    CmdDoc {
        verb: "config",
        aliases: &[],
        method: "— (registry + core.capabilities)",
        summary: "Read and change tasqx settings.",
        usage: "tasqx config <list|get <key>|set <key> <value>|unset <key>|path|store|edit>",
        examples: &[
            ex("tasqx config list"),
            ex("tasqx config get theme.name"),
            ex("tasqx config path"),
            ex("tasqx config store"),
            ex_norun("tasqx config set theme.name gruvbox", "writes config.toml, preserving your comments"),
            ex_norun("tasqx config edit", "full-screen editor; arrow through themes and watch them apply"),
        ],
        notes: &[
            "`store` answers which store you are actually writing to — and says so when a running daemon owns it, because the remote path never consults $TASQX_DB, so a correct $TASQX_DB is silently inert whenever a daemon is listening.",
            "`list` shows both homes. Most settings live in `config.toml`; `default_project` lives in the store and is set with `tasqx use` (D21).",
            "Resolution order is `--flag`, then `$TASQX_*`, then `config.toml`, then the built-in default (D9). The SOURCE column names the layer that won.",
            "`edit` opens an interactive screen: up/down to move, enter to toggle a switch or open a theme picker, esc to leave. Moving through the theme list repaints the screen in that theme before anything is written.",
            "`edit` needs a real terminal. Piped or redirected it refuses and exits 2 rather than writing escape codes into your pipe — scripts should use `set`/`unset` (D26).",
        ],
        see_also: &["use", "theme", "manual"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "memory",
        aliases: &[],
        method: "memory.search + add/remove",
        summary: "Store and search knowledge: docs, patterns, and your task annotations (D41).",
        usage: "tasqx memory <add <title> <body> [--source s]|search <words…> [--limit n] [--scope s] [--raw]|rm <id>|import <path>>",
        examples: &[
            ex("tasqx memory add \"Deploy runbook\" \"deploys go through the blue-green pipeline\""),
            ex("tasqx memory search blue-green"),
            ex_norun(
                "tasqx memory import docs/adr",
                "one doc per .md file; title from the first # heading",
            ),
            ex_norun(
                "tasqx memory rm 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12",
                "by the id search printed",
            ),
        ],
        notes: &[
            "Search covers your imported docs AND task annotations, bm25-ranked. Plain words are matched as phrases (hyphens and dots are safe); pass --raw for FTS5 operator syntax.",
            "Import is one transaction: a bad file imports nothing, and re-importing a directory replaces docs from the same source instead of duplicating them.",
            "An MCP agent reaches the same store: tasqx_search_memory works even read-only, so agents can consult knowledge while executing tasks.",
        ],
        see_also: &["annotate", "mcp", "api"],
        topic: Topic::Automation,
    },
    CmdDoc {
        verb: "tokens",
        aliases: &[],
        method: "tokens.recompute",
        summary: "Repair stored token attribution — dry-run by default (D50).",
        usage: "tasqx tokens recompute [--apply]",
        examples: &[
            ex("tasqx tokens recompute"),
            ex_norun(
                "tasqx tokens recompute --apply",
                "write the repair, after reviewing the dry-run delta",
            ),
        ],
        notes: &[
            "A bare `tasqx tokens recompute` prints the per-task delta and writes NOTHING; `--apply` is the explicit opt-in for the one verb in the API built to delete measurement rows.",
            "Scope is `source=log-parse` measurements only: samples claimed by more than one task's window drop out, a task whose transcript is gone keeps its counts at confidence `low`, and self-reported/OTLP rows are never rewritten.",
            "Stop any daemon on the store before `--apply`: the verb parses transcripts and runs in-process only (a daemon refuses it over the socket), and applying beside a live daemon is two writers — convergence on rerun is the safety net, not a license.",
        ],
        see_also: &["report", "done", "api"],
        topic: Topic::Automation,
    },
    CmdDoc {
        verb: "export",
        aliases: &[],
        method: "store.export",
        summary: "Dump tasks as canonical JSON.",
        usage: "tasqx export [filter…]",
        examples: &[
            ex("tasqx export"),
            ex("tasqx export project:work"),
        ],
        notes: &["Canonical JSON; a filtered export trims edges leaving the set and reports `dropped_dependencies` (D12).",
                 "The document carries `projects` and `default_project` too, so a restore gives back the store and not only its tasks (D37)."],
        see_also: &["import", "api"],
        topic: Topic::JsonApi,
    },
    CmdDoc {
        verb: "import",
        aliases: &[],
        method: "store.import",
        summary: "Load tasks from a JSON file or stdin.",
        usage: "tasqx import <file|->",
        examples: &[
            ex_norun("tasqx import backup.json", "from a file"),
            ex_norun("tasqx export | tasqx import -", "from stdin"),
        ],
        notes: &[],
        see_also: &["export", "api"],
        topic: Topic::JsonApi,
    },
    CmdDoc {
        verb: "api",
        aliases: &[],
        method: "(any)",
        summary: "One JSON envelope in on stdin → one out on stdout.",
        usage: "tasqx api   # one JSON envelope on stdin → one on stdout",
        examples: &[
            ex_norun("tasqx api <<< '{\"tasqx\":\"1\",\"id\":\"1\",\"method\":\"task.list\",\"params\":{}}'", "call any method"),
        ],
        notes: &["The stdio one-shot transport; the envelope key is `\"tasqx\":\"1\"`."],
        see_also: &["mcp", "daemon", "export"],
        topic: Topic::JsonApi,
    },
    CmdDoc {
        verb: "daemon",
        aliases: &[],
        method: "(serves all)",
        summary: "Long-lived single-writer server.",
        usage: "tasqx daemon [--db PATH]",
        examples: &[ex_norun("tasqx daemon", "bind the socket/named pipe and serve")],
        notes: &["Long-lived single-writer; one-shot commands auto-route through it. Ctrl-C stops it cleanly."],
        see_also: &["watch", "api"],
        topic: Topic::Daemon,
    },
    CmdDoc {
        verb: "watch",
        aliases: &[],
        method: "task.list + push",
        summary: "Live-updating task view.",
        usage: "tasqx watch [filter…]",
        examples: &[ex_norun("tasqx watch project:work", "live view; needs a running daemon")],
        notes: &[],
        see_also: &["daemon", "list"],
        topic: Topic::Daemon,
    },
    CmdDoc {
        verb: "mcp",
        aliases: &[],
        method: "(subset)",
        summary: "Serve MCP over stdio with an operator-selected scope.",
        usage: "tasqx mcp serve [--scope read|write]",
        examples: &[
            ex_norun("tasqx mcp serve", "serve read-only over stdio JSON-RPC"),
            ex_norun("tasqx mcp serve --scope write", "serve with explicit write access"),
        ],
        notes: &["Scope configures the local process; it is not authentication. Omitted scope is read-only."],
        see_also: &["api", "daemon"],
        topic: Topic::Automation,
    },
    CmdDoc {
        verb: "docs",
        aliases: &[],
        method: "— (no store)",
        summary: "Open the exhaustive browser guide (self-contained HTML).",
        usage: "tasqx docs [--out PATH | --no-open | --stdout]",
        examples: &[
            ex_norun("tasqx docs", "open the browser guide"),
            ex("tasqx docs --stdout"),
        ],
        notes: &["The exhaustive browser guide (self-contained HTML). For a quick in-terminal guide, `tasqx manual`."],
        see_also: &["manual"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "manual",
        aliases: &["man"],
        method: "— (no store)",
        summary: "Browse the complete guide in your terminal.",
        usage: "tasqx manual [<command|topic>]",
        examples: &[
            ex("tasqx manual"),
            ex("tasqx manual init"),
            ex("tasqx manual filters"),
        ],
        notes: &["No store, no network. `tasqx docs` is the fuller browser guide."],
        see_also: &["docs"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "completions",
        aliases: &[],
        method: "— (no store)",
        summary: "Turn on Tab completion for your shell.",
        usage: "tasqx completions [<shell>] [--install | --uninstall] [--profile PATH] [--yes]",
        examples: &[
            // Safe: printing reads nothing and writes nothing, so
            // `tests/help.rs` executes it for real. Every example that EDITS a
            // file is NoRun below — that guard runs on the developer's own
            // machine, and a `--install` example marked Safe would append an
            // activation line to their real `.bashrc` every time the suite ran.
            ex("tasqx completions bash"),
            ex_norun(
                "tasqx completions bash >> ~/.bashrc",
                "the printed line is one line for exactly this",
            ),
            ex_norun(
                "tasqx completions --install",
                "detects the shell from $SHELL, shows the block, asks first",
            ),
            ex_norun(
                "tasqx completions powershell --install --profile $PROFILE",
                "PowerShell expands $PROFILE; tasqx will not guess it",
            ),
            ex_norun(
                "tasqx completions zsh --uninstall",
                "removes the block, restoring the file byte for byte",
            ),
        ],
        notes: &[
            "--install edits your shell's startup file inside a marked block, asks before writing, and refuses when stdin is not a terminal (pass --yes from a script).",
            "cmd.exe cannot be completed by any program and is a permanent non-goal; nushell is a gap clap_complete has no generator for.",
        ],
        see_also: &["manual", "docs"],
        // Its own topic rather than `GettingStarted`, because the prose a user
        // needs here does not fit in a verb's notes: five activation lines, two
        // shells tasqx deliberately does not serve, a variable that is not the
        // one every clap tutorial names, and the fact that a Tab press reads the
        // store. `tasqx manual completion` is where all of that lives.
        topic: Topic::Completion,
    },
];

/// Resolve a verb or alias to its record.
pub fn find(verb: &str) -> Option<&'static CmdDoc> {
    COMMAND_REF
        .iter()
        .find(|d| d.verb == verb || d.aliases.contains(&verb))
}

/// The plain-text block clap appends to a command's help (both `-h` and
/// `--help`). Empty string for an unknown verb, so `after_help("nope")`
/// harmlessly contributes nothing.
pub fn after_help(verb: &str) -> String {
    let Some(d) = find(verb) else {
        return String::new();
    };
    let mut s = String::new();
    s.push_str("EXAMPLES\n");
    for e in d.examples {
        s.push_str("  ");
        s.push_str(e.cmd);
        if let Some(n) = e.note {
            s.push_str("    # ");
            s.push_str(n);
        }
        s.push('\n');
    }
    if !d.notes.is_empty() {
        s.push('\n');
        for (i, n) in d.notes.iter().enumerate() {
            s.push_str(if i == 0 { "NOTE  " } else { "      " });
            s.push_str(n);
            s.push('\n');
        }
    }
    if !d.see_also.is_empty() {
        s.push_str("\nSee also: ");
        s.push_str(&d.see_also.join(" · "));
        s.push_str("        Full manual: tasqx manual\n");
    }
    s
}

#[cfg(test)]
pub fn verbs() -> Vec<&'static str> {
    COMMAND_REF.iter().map(|d| d.verb).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_record_has_usage_and_an_example() {
        for d in COMMAND_REF {
            assert!(!d.usage.trim().is_empty(), "{}: empty usage", d.verb);
            assert!(!d.examples.is_empty(), "{}: no examples", d.verb);
            for e in d.examples {
                assert!(
                    e.cmd.trim_start().starts_with("tasqx "),
                    "{}: example {:?} must start with `tasqx `",
                    d.verb,
                    e.cmd
                );
            }
        }
    }

    /// `agenda`'s prose may promise a count only for something the footer
    /// actually counts.
    ///
    /// The shipped note read "Tasks with no date at all, tasks past the horizon
    /// and done/cancelled tasks are each COUNTED under the table" — and
    /// `render::Agenda` has no done/cancelled counter at all, because those rows
    /// are excluded on the wire by the composed filter and never reach the
    /// renderer. A reader with 500 done tasks looked for that count, found
    /// nothing, and could not tell an empty store from broken accounting: the
    /// exact ambiguity the counters exist to remove. The note even contradicted
    /// the next note in its own array, which correctly says done and cancelled
    /// are *left out* by the filter.
    ///
    /// Asserted over both prose surfaces at once, because they are separate
    /// strings that drifted together: this array and `command::Command::Agenda`'s
    /// doc comment (clap's long help). The rule is narrow on purpose — the
    /// counting sentence must not name the statuses — so rewording the promise
    /// keeps passing while re-adding the false claim does not.
    #[test]
    fn the_agenda_counting_promise_names_only_reasons_the_footer_counts() {
        let clap_about = include_str!("command.rs");
        let agenda_about = clap_about
            .split("What is coming up, when (maps to task.list)")
            .nth(1)
            .expect("Command::Agenda's doc comment")
            .split("#[command(alias = \"ag\"")
            .next()
            .expect("the doc comment ends at the attribute");

        let notes = find("agenda").expect("agenda is documented").notes.concat();
        for (name, surface) in [
            ("cmddoc notes", notes.as_str()),
            ("clap help", agenda_about),
        ] {
            let mut checked = 0;
            for sentence in surface.split('.') {
                let lower = sentence.to_lowercase();
                if !lower.contains("counted") {
                    continue;
                }
                checked += 1;
                for status in ["done", "cancelled"] {
                    assert!(
                        !lower.contains(status),
                        "{name}: the agenda counts undated rows and rows past the horizon, \
                         and nothing else — a sentence promising a count for {status:?} \
                         sends the reader looking for a number that is not there: \
                         {sentence:?}"
                    );
                }
            }
            // Without this the guard passes vacuously the moment someone drops
            // the promise entirely, which is its own regression: D53's whole
            // claim is that nothing is dropped in silence.
            assert!(
                checked > 0,
                "{name} no longer promises anything is counted, so this guard is \
                 asserting nothing"
            );
        }
    }

    #[test]
    fn find_resolves_verbs_and_aliases() {
        assert_eq!(find("init").unwrap().verb, "init");
        assert_eq!(find("a").unwrap().verb, "add"); // alias
        assert_eq!(find("edit").unwrap().verb, "modify"); // alias
        assert!(find("nope").is_none());
    }

    #[test]
    fn after_help_lists_examples_notes_and_see_also() {
        let h = after_help("init");
        assert!(h.contains("EXAMPLES"), "{h}");
        assert!(h.contains("tasqx init keuken-verbouwen"), "{h}");
        assert!(h.contains("See also"), "{h}");
        // plain text only — no ANSI escape bytes
        assert!(!h.contains('\x1b'), "after_help must be plain: {h:?}");
    }

    #[test]
    fn after_help_of_unknown_verb_is_empty() {
        assert_eq!(after_help("nope"), "");
    }

    /// D24 changed what `tasqx report` counts, and a default that silently drops
    /// rows is only defensible if the surface says so. The existing drift guards
    /// compare verbs and aliases, not flags, so a flag can ship undocumented
    /// without anything going red — this pins the one flag whose absence would
    /// leave users unable to explain a count they think is wrong.
    #[test]
    fn report_documents_the_all_flag_and_the_cancelled_default() {
        let d = find("report").unwrap();
        assert!(
            d.usage.contains("--all"),
            "usage must offer --all: {}",
            d.usage
        );
        let notes = d.notes.join(" ").to_lowercase();
        assert!(
            notes.contains("cancelled"),
            "the notes must state the D24 default in the user's own vocabulary: {notes}"
        );
        // `after_help` is what a user actually reads at `tasqx report -h`.
        let h = after_help("report").to_lowercase();
        assert!(h.contains("--all"), "{h}");
        assert!(h.contains("cancelled"), "{h}");
    }

    /// Pull the flag-shaped tokens out of a usage line. Usage strings mix flags
    /// with placeholders and punctuation (`[--due d]`, `[-p H|M|L]…`), so the
    /// separators have to include the bracket/pipe family, not just whitespace.
    fn usage_flags(usage: &str) -> Vec<&str> {
        usage
            .split(|c: char| c.is_whitespace() || "[]<>|()…,".contains(c))
            .filter(|t| t.starts_with('-') && t.len() > 1)
            .collect()
    }

    /// The general form of the guard above. Until this existed, NOTHING in the
    /// repo inspected clap's arguments — `get_arguments` appeared zero times —
    /// so every one of the eighteen drift guards compared verbs, aliases and
    /// methods while a flag could ship entirely undocumented. That is how
    /// `--all` shipped: the suite stayed green with the flag absent from every
    /// user-facing surface, and the gap was found by a human reading the code.
    ///
    /// A flag the user can type must appear in the `usage` line of its verb, so
    /// `tasqx <verb> -h` names it. `--help`/`--version` are clap's own and carry
    /// no documentation obligation.
    #[test]
    fn every_clap_flag_is_documented_in_its_verbs_usage() {
        use clap::CommandFactory;
        let cmd = crate::Cli::command();
        let mut undocumented: Vec<String> = Vec::new();

        for sub in cmd.get_subcommands() {
            let verb = sub.get_name();
            let Some(doc) = find(verb) else { continue }; // covered by the verb guard
            for arg in sub.get_arguments() {
                let Some(long) = arg.get_long() else { continue };
                if matches!(long, "help" | "version") {
                    continue;
                }
                // Either spelling counts as documented: `add`'s usage names
                // `-p H|M|L`, and a reader who sees the short form knows the
                // option exists. The obligation is discoverability, not a
                // particular spelling.
                //
                // Matched against TOKENS, not as a substring. A `contains("-e")`
                // is satisfied by `--expected-rev`, which is how `modify -e`
                // first passed this guard while being genuinely undocumented —
                // the same lexical-vs-structural trap `Filter::constrains_status`
                // exists to avoid.
                let documented = usage_flags(doc.usage).iter().any(|tok| {
                    tok.trim_start_matches('-') == long
                        || arg.get_short().is_some_and(|c| {
                            tok.len() == 2 && tok.starts_with('-') && tok.ends_with(c)
                        })
                });
                if !documented {
                    undocumented.push(match arg.get_short() {
                        Some(c) => format!("{verb} --{long} (or -{c})"),
                        None => format!("{verb} --{long}"),
                    });
                }
            }
        }

        assert!(
            undocumented.is_empty(),
            "flags the CLI accepts but no usage line mentions:\n  {}",
            undocumented.join("\n  ")
        );
    }

    /// The guard above walks SUBCOMMANDS, so the four global flags — declared
    /// on the top-level `Cli` and accepted by every verb — sat structurally
    /// outside it: `--no-daemon` could vanish, or gain a sibling, with every
    /// gate green. Their documented home is the guide's "Global flags" table
    /// (`docs::GLOBAL_FLAGS`, which the Commands page renders); this binds
    /// that table to clap's own top-level argument list, both directions, so
    /// the table can neither omit a real global nor keep advertising a dead one.
    ///
    /// `--help`/`--version` are clap's own and carry no documentation
    /// obligation; the table names them anyway, and they are skipped on both
    /// sides rather than asserted.
    #[test]
    fn every_global_flag_is_documented_in_the_guides_global_table() {
        use clap::CommandFactory;
        let cmd = crate::Cli::command();
        let real: Vec<String> = cmd
            .get_arguments()
            .filter_map(|a| a.get_long())
            .filter(|l| !matches!(*l, "help" | "version"))
            .map(String::from)
            .collect();
        // Floor: the top-level surface is four flags today. Zero would mean
        // this guard is comparing nothing against nothing.
        assert!(
            real.len() >= 4,
            "the top-level Cli lost its global flags: {real:?}"
        );

        let documented: Vec<&str> = crate::docs::GLOBAL_FLAGS
            .iter()
            .flat_map(|(flag, _)| usage_flags(flag))
            .map(|tok| tok.trim_start_matches('-'))
            .filter(|l| !matches!(*l, "help" | "version"))
            .collect();

        let missing: Vec<&String> = real
            .iter()
            .filter(|l| !documented.contains(&l.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "global flags the CLI accepts but the guide's Global flags table never names: {missing:?}"
        );
        let stale: Vec<&&str> = documented
            .iter()
            .filter(|l| !real.contains(&l.to_string()))
            .collect();
        assert!(
            stale.is_empty(),
            "the Global flags table documents flags the top-level Cli does not declare: {stale:?}"
        );
    }

    /// `see_also` is a cross-reference the reader is invited to follow, and
    /// nothing has ever checked that it names a real verb. A dangling entry
    /// renders as a normal suggestion in `-h` and in `tasqx manual`, so the
    /// reader types it, gets "unrecognized subcommand", and concludes the tool
    /// is broken rather than the doc. Aliases count: pointing at `ls` is fine.
    #[test]
    fn every_see_also_names_a_real_verb() {
        let known: Vec<&str> = COMMAND_REF
            .iter()
            .flat_map(|d| std::iter::once(d.verb).chain(d.aliases.iter().copied()))
            .collect();
        let mut dangling = Vec::new();
        for d in COMMAND_REF {
            for target in d.see_also {
                if !known.contains(target) {
                    dangling.push(format!("{} -> {target}", d.verb));
                }
            }
        }
        assert!(
            dangling.is_empty(),
            "see_also entries naming no real verb: {dangling:?}"
        );
    }

    /// `Topic::ALL` drives every topic page in `tasqx manual`. The compiler
    /// forces a new variant to gain `slug()` and `title()` arms — both are
    /// exhaustive matches — but it does NOT force membership in `ALL`, which is
    /// a plain array. A variant missing from it is invisible: its commands
    /// silently vanish from the manual's table of contents while every command
    /// still renders individually, so nothing looks wrong.
    #[test]
    fn topic_all_lists_every_topic() {
        // Distinctness via slug: `ALL` is the hand-written list, so a duplicate
        // entry would satisfy the declared length while dropping a topic.
        let mut slugs: Vec<&str> = Topic::ALL.iter().map(Topic::slug).collect();
        let before = slugs.len();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), before, "Topic::ALL contains a duplicate");

        // Every topic a command actually claims must be reachable from ALL.
        let missing: Vec<&str> = COMMAND_REF
            .iter()
            .map(|d| d.topic.slug())
            .filter(|s| !slugs.contains(s))
            .collect();
        assert!(
            missing.is_empty(),
            "topics used by commands but absent from Topic::ALL: {missing:?}"
        );
    }

    /// The top-level guard above covers VERBS. Nothing covered a verb's
    /// SUB-subcommands, which are enumerated by hand inside a `usage` string —
    /// so adding `config store` left the documented usage line silently wrong,
    /// with every gate green. That is D30's rule (a list kept in sync by hand is
    /// a list that will drift) at the one nesting level it had not reached.
    ///
    /// Derived from clap, not from a second list: a new sub-subcommand joins
    /// this check the moment it exists.
    #[test]
    fn every_nested_subcommand_appears_in_its_verbs_usage_line() {
        use clap::CommandFactory;
        let cli = crate::Cli::command();
        for sub in cli.get_subcommands() {
            let nested: Vec<&str> = sub.get_subcommands().map(|c| c.get_name()).collect();
            if nested.is_empty() {
                continue;
            }
            let Some(doc) = COMMAND_REF.iter().find(|d| d.verb == sub.get_name()) else {
                continue;
            };
            let missing: Vec<&&str> = nested.iter().filter(|n| !doc.usage.contains(*n)).collect();
            assert!(
                missing.is_empty(),
                "`{}` has sub-subcommands its documented usage line never names: {missing:?}\n  \
                 usage: {}",
                sub.get_name(),
                doc.usage
            );
        }
    }

    /// The name guard above reaches one level down; nothing reached the FLAGS
    /// at that level. `every_clap_flag_is_documented_in_its_verbs_usage` walks
    /// `get_subcommands()` and never descends, so a flag on a sub-subcommand —
    /// `chart heatmap --year`, `memory search --raw`, `tokens recompute
    /// --apply` — carried no documentation obligation at all, which is the
    /// exact gap that let `--all` ship undocumented at the top level. Same
    /// rule, one nesting deeper: the flag must appear in the parent verb's
    /// usage line, the only place `tasqx <verb> -h` will show it. Recursive,
    /// so a third nesting level joins the moment it exists.
    #[test]
    fn every_nested_subcommand_flag_is_documented_in_its_verbs_usage() {
        use clap::CommandFactory;

        // (long, short) of every --flag on `cmd`'s descendants, help/version
        // excluded — clap's own, as in the top-level guard.
        fn nested_flags(cmd: &clap::Command, out: &mut Vec<(String, Option<char>)>) {
            for sub in cmd.get_subcommands() {
                for arg in sub.get_arguments() {
                    let Some(long) = arg.get_long() else { continue };
                    if matches!(long, "help" | "version") {
                        continue;
                    }
                    out.push((long.to_string(), arg.get_short()));
                }
                nested_flags(sub, out);
            }
        }

        let cli = crate::Cli::command();
        let mut seen = 0;
        let mut undocumented: Vec<String> = Vec::new();
        for sub in cli.get_subcommands() {
            let Some(doc) = find(sub.get_name()) else {
                continue; // covered by the verb guard
            };
            let mut flags = Vec::new();
            nested_flags(sub, &mut flags);
            seen += flags.len();
            for (long, short) in flags {
                // Token-matched with either spelling, same contract as the
                // top-level guard.
                let documented = usage_flags(doc.usage).iter().any(|tok| {
                    tok.trim_start_matches('-') == long
                        || short.is_some_and(|c| {
                            tok.len() == 2 && tok.starts_with('-') && tok.ends_with(c)
                        })
                });
                if !documented {
                    undocumented.push(format!("{} … --{long}", sub.get_name()));
                }
            }
        }

        // Floor: chart/memory/tokens/mcp carry eleven nested flags today. An
        // iteration that finds none is the guard silently unplugged, not a CLI
        // that lost its nesting.
        assert!(
            seen >= 10,
            "the nested-flag walk found only {seen} flags — did the recursion break?"
        );
        assert!(
            undocumented.is_empty(),
            "sub-subcommand flags no usage line mentions:\n  {}",
            undocumented.join("\n  ")
        );
    }

    #[test]
    fn command_ref_covers_exactly_the_clap_surface() {
        use clap::CommandFactory;
        let mut real: Vec<String> = crate::Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        let mut doc: Vec<String> = verbs().iter().map(|s| s.to_string()).collect();
        real.sort();
        doc.sort();
        let missing: Vec<_> = real.iter().filter(|v| !doc.contains(v)).collect();
        assert!(
            missing.is_empty(),
            "verbs with no cmddoc entry: {missing:?}"
        );
        let invented: Vec<_> = doc.iter().filter(|v| !real.contains(v)).collect();
        assert!(
            invented.is_empty(),
            "cmddoc entries with no clap subcommand: {invented:?}"
        );
    }

    #[test]
    fn command_ref_aliases_match_clap() {
        use clap::CommandFactory;
        let cmd = crate::Cli::command();
        for d in COMMAND_REF {
            let sub = cmd
                .get_subcommands()
                .find(|c| c.get_name() == d.verb)
                .unwrap_or_else(|| panic!("no clap subcommand: {}", d.verb));
            let mut real: Vec<String> = sub.get_all_aliases().map(|a| a.to_string()).collect();
            let mut ours: Vec<String> = d.aliases.iter().map(|a| a.to_string()).collect();
            real.sort();
            ours.sort();
            assert_eq!(real, ours, "alias drift on `{}`", d.verb);
        }
    }
}
