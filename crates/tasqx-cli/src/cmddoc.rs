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
    Screens,
    Reminders,
    Reports,
    Daemon,
    Automation,
    JsonApi,
    Completion,
}

impl Topic {
    pub const ALL: [Topic; 12] = [
        Topic::GettingStarted,
        Topic::Projects,
        Topic::Capturing,
        Topic::Dates,
        Topic::Filters,
        Topic::Screens,
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
            Topic::Screens => "screens",
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
            Topic::Screens => "Screens & browsing",
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
/// A `NoRun` example with no note. `ex_norun` requires one — there was no
/// plain `NoRun` shorthand, so four call sites that had nothing to say
/// passed `""` just to get the `NoRun` behavior, and `after_help` rendered
/// that empty note as a dangling `# ` (#229 item 14).
const fn ex_norun_plain(cmd: &'static str) -> Example {
    Example {
        cmd,
        note: None,
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
        usage: "tasqx add <title…> [--project p] [--due d] [--scheduled s] [--wait w] [-p H|M|L] [-t tag]… [--repeat r] [--remind r] [-e est] [--budget-tokens n]",
        examples: &[
            ex("tasqx add Buy milk"),
            ex("tasqx add Ship it due:friday +api !high --project work"),
            ex("tasqx add Water plants repeat:\"every 3 days\""),
            ex_norun("tasqx add Call bank due:\"friday 9am\" --remind -30m", "reminder 30m before due"),
        ],
        notes: &[
            "Inline sugar: `+tag`, `project:p` (or `proj:`), `!high`, `due:…`, `scheduled:…` (or `sched:`), `wait:…`, `repeat:…` (or `every:`/`recur:`), `remind:…`, `est:4h` (or `estimate:`).",
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
        usage: "tasqx modify <ref> [words/sugar…] [--project p] [--due d] [--scheduled s] [--wait w] [-p H|M|L] [-t tag]… [--repeat r] [--remind r] [-e est] [--tracked t] [--budget-tokens n] [--clear <field>]… [--expected-rev N]",
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
        usage: "tasqx list [filter…] [--sort key]… [--limit N] [--offset N] [--fields f,…]",
        examples: &[
            ex("tasqx list"),
            ex("tasqx list project:work status:pending +api"),
            ex("tasqx list due.before:friday"),
        ],
        notes: &[
            "Bare `tasqx` is `tasqx list` over the working set.",
            "A value containing a space is double-quoted, and the quotes must reach tasqx: `tasqx list 'project:\"Home Renovation\"'`. Nothing is guessed back together, so the shell-stripped form is refused rather than answered wrongly.",
            "`--sort`, `--limit`, `--offset` and `--fields` map straight onto `task.list`'s own params (`core.capabilities` names all five) — `tasqx list --sort due --limit 20 --fields short_id,title,due`. An unknown sort key or field name is refused, naming the valid set.",
            "The piped table (no TTY) is a fixed 100 cells wide; on a terminal it sizes to `$COLUMNS` (clamped 40–160) or the terminal's own width, whichever it can read. Widen a truncated title with `COLUMNS=200 tasqx list` or narrow the row with `--fields`.",
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
        usage: "tasqx next [filter…] [--card [--ascii]]",
        examples: &[
            ex("tasqx next"),
            ex("tasqx next project:work"),
            ex("tasqx next --card"),
        ],
        notes: &[
            "The single highest-urgency unblocked task — the \"what now\" button.",
            "A filter narrows `@working` rather than replacing it: `tasqx next project:work` still skips blocked and backlog tasks in that project, unlike `tasqx list project:work` which shows every status once a filter is given.",
            "`--card` prints the picked task as a fixed 72-column box-drawn card meant to be \
             pasted into a document (a chat, a PR); `--ascii` draws its borders with `+ - |`. \
             The default screen is unchanged (D146).",
        ],
        see_also: &["list", "why", "start"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "dashboard",
        aliases: &["dash"],
        method: "task.list + report.summary + project.list + event.list",
        summary: "Open the overview screen, or ask for its panels as data.",
        usage: "tasqx dashboard [--json] [--panels <LIST>]",
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
            "`--panels tasks,burndown` narrows the `--json` document to those panels, on that one call — it does not touch `dashboard.panels` or the interactive screen. The task rows are row-capped per group with `total`/`truncated` alongside them, because the list tracks the store's size rather than the screen's (#152). The five panel names D80 retired — `now`, `next`, `due`, `blocked`, `recent` — still parse, and all mean `tasks`, which is where their rows went.",
            "Read-only, with one exception: `p` opens `pick`, the task browser, over the working set — or, from a PROJECTS row, over that project. Enter there reads a task and `s` starts it, which brings you back here; `q` or `esc` there comes back having started nothing. `q`, `esc` and ctrl-c close the dashboard.",
            "Every key (also behind `?` in the screen itself): `1-6` focus a panel; `s` cycles the list order between urgency, due and touched; `tab`/`S-tab` cycle panels; `j`/`k` move the cursor; `g`/`G` jump to the first/last row; `r` refreshes now; `R` toggles auto-refresh; `w` cycles the burndown window; `enter` opens the row under the cursor; `l` leaves and prints the task list.",
        ],
        see_also: &["list", "pick", "agenda", "chart"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "pick",
        aliases: &["p", "fzf"],
        method: "task.list + task.get + task.start",
        summary: "Browse tasks on a full-screen list: read one, search them, start one.",
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
            ex_norun("tasqx pick", "browse the working set, and start a task from it"),
            ex_norun("tasqx pick project:work +api", "narrow the candidates first"),
        ],
        notes: &[
            "Each row is the row `tasqx list` prints: the running and blocked rail, the priority and urgency gauge, the title, the project, the deadline as a calendar day and the tags. The header names the filter and what the set holds — overdue, due today, running, blocked — as `list`'s summary does.",
            "`j`/`k` or the arrows move, `g`/`G` jump to the ends. Enter opens the task's `tasqx show` card; there `j`/`k`/space/`b` scroll and `esc` or `q` goes back to the list. `s` starts the task under the cursor, from the list or from its card — the one key on this screen with a side effect, and the same single-active rule `tasqx start` follows. `q` leaves.",
            "`/` opens the search on the header line: a fuzzy SUBSEQUENCE match over id, title, project and tags, so `wac` finds `Write API conformance tests`, and a term found whole ranks above the same letters scattered. Whitespace splits it into terms that must all match. Every letter is a letter there; up/down or ctrl-p/ctrl-n move, ctrl-u clears, ctrl-w deletes a word, and Enter or `esc` goes back to the list with the filter kept. `esc` in the list clears the filter, and only then leaves.",
            "Leaving without starting a task exits 0: `pick` is a browser, and a browser you close is not a failed run — `tasqx pick && …`, a prompt indicator and any script that opens it to look all survive `q`. A filter that matches no task still exits 4, and so does an empty working set: that is a request `pick` could not serve, not a session you ended.",
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
        usage: "tasqx show <ref> [--card [--ascii]]",
        examples: &[ex("tasqx show 1"), ex("tasqx show 1 --card")],
        notes: &[
            "Full detail: tags, annotations, dependencies, blocked state, `_rev`.",
            "`--card` prints a fixed 72-column box-drawn card meant to be pasted into a \
             document (a chat, a PR); `--ascii` draws its borders with `+ - |`. The default \
             screen is unchanged (D146).",
        ],
        see_also: &["modify", "why", "annotate"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "why",
        aliases: &[],
        method: "task.get",
        summary: "Explain a task's urgency score.",
        usage: "tasqx why <ref> [--card [--ascii]]",
        examples: &[ex("tasqx why 1"), ex("tasqx why 1 --card")],
        notes: &[
            "Explains the urgency score component by component (DESIGN D1).",
            "`--card` prints the fixed 72-column box-drawn card (D146) first, with the \
             arithmetic underneath instead of the plain text header; `--ascii` draws the \
             card's borders with `+ - |`.",
        ],
        see_also: &["next", "show", "list"],
        topic: Topic::Reports,
    },
    CmdDoc {
        verb: "start",
        aliases: &["s"],
        method: "task.start",
        summary: "Mark a task active.",
        usage: "tasqx start <ref> [--keep] [--client TOOL] [--session-id ID] [--transcript-path PATH]",
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
        examples: &[ex_norun_plain("tasqx stop 1")],
        notes: &[],
        see_also: &["start", "done", "adjust"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "adjust",
        aliases: &[],
        method: "task.adjust_tracked",
        summary: "Correct a task's tracked time.",
        usage: "tasqx adjust <ref> <delta> --reason TEXT",
        examples: &[
            ex_norun(
                "tasqx adjust 1 -2h25m --reason 'idle gap'",
                "take off time the clock ran while nobody worked",
            ),
            ex_norun(
                "tasqx adjust 1 +30m --reason 'forgot to start'",
                "add time that never reached the clock",
            ),
        ],
        notes: &[
            "Works on any task, done included, so calibration in `report --outcomes` \
             can be corrected after the fact. A correction that would take the banked \
             total below zero is refused.",
            "Each correction is its own event with its reason, and `tasqx undo` takes \
             the newest one back. `show` prints the net of every correction beside the \
             total: `tracked 2h30m (adjusted -2h25m)`.",
        ],
        see_also: &["stop", "undo", "report"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "done",
        aliases: &["d", "x", "complete"],
        method: "task.done",
        summary: "Complete a task.",
        usage: "tasqx done <ref> [--force] [--client TOOL] [--session-id ID] \
                [--transcript-path PATH] [--tool TOOL] [--model MODEL] \
                [--input-tokens N] [--output-tokens N] [--cache-read-tokens N] \
                [--cache-creation-tokens N] [--total-tokens N]",
        examples: &[
            ex_norun("tasqx done 1", "completes; spawns the next recurrence if any"),
            ex_norun(
                "tasqx done 1 --force",
                "complete it although a dependency is still open; the override is recorded",
            ),
            ex_norun(
                "tasqx done 1 --client 'claude-code 2.1' --session-id $SID",
                "close the interval an agent opened with the same ids",
            ),
            ex_norun(
                "tasqx done 1 --tool claude-code --model claude-opus-5 \
                 --input-tokens 4200 --output-tokens 900",
                "self-report the turn's spend — the primary measurement channel (D50)",
            ),
        ],
        notes: &[
            "A task whose dependencies are still open is refused, naming them. \
             --force completes it anyway; the override lands on the done event and \
             `tasqx report --outcomes` counts it under FORCED (D150). Cancelling a \
             blocked task needs no flag.",
            "The correlation flags carry the same meaning as on `start`, and are \
             recorded per occurrence: a task can start and finish many times, and \
             attribution pairs the two events of one interval.",
            "--tool/--model and the four *-tokens flags are the self-report channel \
             (D50/D65): any present token count records a measurement, attributed to \
             --tool (or --client if --tool is omitted). --tool/--model alone, with no \
             count, still land on the completion event. Skip all of them and the \
             response carries a tokens_hint explaining what was not recorded — printed \
             under the Done line.",
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
        examples: &[ex_norun_plain("tasqx reopen 1")],
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
            "Six operations are undoable — `stop`, `untag`, `undep`, `annotate`, `annotate --edit` and `adjust`. Every other one exits 5 naming itself and the verb that does take it back (`done` -> `tasqx reopen`, `modify` -> `tasqx show` then a second `modify`).",
            "Undo APPENDS: the event it reverses stays in the log and a new `undo` event lands behind it, so `tasqx chart` and the audit trail read `X happened, then it was undone`.",
            "There is no redo, so `tasqx undo` twice in a row exits 5: the second one would find the first undo as the newest event and the pair would toggle forever.",
            "It reverses the newest RECORDED event, which is not always the last command you typed. A command that changed nothing records nothing — `tasqx undep 1 2` where no such edge exists, or `tasqx start` on a task already running — so `undo` reaches past it to the previous change. That is why the answer names what it undid: read it before assuming it hit what you were aiming at.",
            "A single event outside the undoable five permanently blocks undo for everything BEFORE it, not just for itself: the newest event on an active store is almost always `add`, `done` or `modify`, and once one of those lands, an annotation from ten seconds earlier can never be reached (#228.2). This is a same-breath affordance — undo the thing you just did — not an undo stack.",
        ],
        see_also: &["untag", "undep", "reopen", "chart"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "annotate",
        aliases: &["note"],
        method: "annotation.add + annotation.update",
        summary: "Attach a timestamped note to a task.",
        usage: "tasqx annotate <ref> [--edit <annotation-id>] <text…>",
        examples: &[ex_norun_plain("tasqx annotate 1 Called the plumber, waiting on a quote")],
        notes: &[
            "Wrote something you shouldn't have? `tasqx unannotate <ref> <annotation-id>` \
             scrubs it — there is no other way back.",
            "Wrote something wrong? `--edit <annotation-id>` replaces that note's text in \
             place (D165): its id, timestamp and position stay, so a corrected first note \
             is still the card's Description, and `tasqx undo` puts the old text back.",
        ],
        see_also: &["show", "modify", "unannotate"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "unannotate",
        aliases: &[],
        method: "annotation.remove",
        summary: "Permanently scrub one annotation's text, by id.",
        usage: "tasqx unannotate <ref> <annotation-id>",
        examples: &[ex_norun_plain(
            "tasqx unannotate 1 018f2f7e-...",
        )],
        notes: &[
            "A HARD delete (D113): the text is overwritten in the store, not merely hidden — \
             use it to take back a secret, a customer name, or a wrong root cause pasted into \
             a note by mistake. This also redacts the original `annotate` event's own body, \
             so `tasqx chart` (`event.list`) and `tasqx export` stop showing it too — not \
             only `tasqx show`.",
            "The annotation's id is not printed by `tasqx show` — read it from `tasqx show \
             <ref> --json` (the `annotations[].id` field) or a prior `tasqx annotate` \
             response.",
            "`tasqx undo` does NOT cover this: the body is already gone from the log by the \
             time the removal event exists, so there is nothing left to restore.",
            "An unknown id, or one already removed, exits 4 (not_found) rather than answering \
             ok for nothing.",
        ],
        see_also: &["annotate", "show", "undo"],
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
        notes: &["`<ref>` becomes blocked until `<depends_on>` is done or cancelled; a `<ref>` that is itself already closed is never blocked."],
        see_also: &["undep", "show"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "undep",
        aliases: &[],
        method: "dependency.remove",
        summary: "Remove a dependency edge.",
        usage: "tasqx undep <ref> <depends_on>",
        examples: &[ex_norun_plain("tasqx undep 2 1")],
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
            "An archived project is out of rotation for WRITES — `use` refuses it (exit 5), and so does an `add`/`modify` that names it, and so does a second `archive` of it (`project is already archived`, exit 5). No write may name an archived project, this one included; `store.import` restoring the flag from a document is the one write that still can. Reads are unaffected: `list`, `report` and `agenda` still show it and its tasks — archiving is a rotation change, not a hide.",
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
        verb: "check",
        aliases: &[],
        method: "check.add + check.set + check.remove",
        summary: "Acceptance criteria on a task: add, mark, drop.",
        usage: "tasqx check <add <ref> <criterion…>|set <ref> <check_id|position> <state> [--evidence e]|remove <ref> <check_id|position>>",
        examples: &[
            ex_norun("tasqx check add 1 the notes name every breaking change", "add a criterion"),
            ex_norun(
                "tasqx check set 1 019f-abc passed --evidence \"suite green\"",
                "mark it",
            ),
            ex_norun("tasqx check rm 1 019f-abc", "drop a criterion"),
            ex_norun("tasqx check set 1 2 failed", "the second criterion, by position"),
        ],
        notes: &[
            "tasqx NEVER RUNS a check. The criterion is a claim and the evidence is a citation; \
             both are stored verbatim and neither is interpreted. A hook you installed is what \
             runs commands, calling `check set` like any other client.",
            "Completing with a criterion still open is not refused — nothing is blocked — but \
             `tasqx report --outcomes` counts it as an unproven completion.",
            "`failed` is a normal outcome. Use `rm` only when the criterion was the wrong thing \
             to ask; removing one the work failed hides the finding.",
        ],
        see_also: &["done", "show", "report"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "brief",
        aliases: &[],
        method: "task.brief",
        summary: "Everything needed before starting a task, in one read.",
        usage: "tasqx brief <ref> [--memory-limit N] [--card [--ascii]]",
        examples: &[
            ex("tasqx brief 1"),
            ex("tasqx brief 1 --memory-limit 3"),
            ex("tasqx brief 1 --card"),
        ],
        notes: &[
            "The task, what each of its prerequisites concluded, what it blocks, and memory \
             found under a query tasqx derives from the task's own title, tags and project — \
             so nobody has to guess search terms.",
            "Half the memory page (rounded up) is reserved for knowledge docs, so a ruling is \
             never buried under a sibling task's own notes; annotations fill whatever slots \
             docs leave (D147).",
            "Memory is scoped to the task's project and does not widen when that finds \
             nothing: `tasqx memory search` is the wider read.",
            "`--card` prints a fixed 72-column box-drawn card meant to be pasted into a \
             document (a chat, a PR); `--ascii` draws its borders with `+ - |`. The default \
             screen is unchanged (D146).",
        ],
        see_also: &["show", "next", "memory"],
        topic: Topic::Capturing,
    },
    CmdDoc {
        verb: "report",
        aliases: &[],
        method: "report.summary + report.outcomes",
        summary: "Summary counts, or outcomes, optionally grouped, as text or HTML.",
        usage: "tasqx report [group_by] [filter…] [--all] [--outcomes] [--since WHEN] \
                [--until WHEN] [--metrics list] [--html] [--out FILE]",
        examples: &[
            ex("tasqx report"),
            ex("tasqx report project"),
            ex("tasqx report --all"),
            ex("tasqx report --metrics tokens_in,tokens_out,tokens_cache_read,tokens_cache_creation"),
            ex("tasqx report --since -7d"),
            ex("tasqx report --outcomes"),
            ex("tasqx report --outcomes project --since -30d"),
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
            "The terminal's TOKENS column names the largest of four buckets by volume; \
             `--metrics` naming a tokens_* bucket shows all four as their own columns instead, \
             same as `--html` and `--json`.",
            "`--since`/`--until` window `tracked_total` and the token buckets by WHEN the \
             time or spend happened (D97) — a different axis from `completed.after:`/\
             `completed.before:` in the filter, which selects tasks by completion date. \
             Rejected alongside `--html`, which has no windowed path yet.",
            "--socket is refused with `--html` (DESIGN.md D73): the HTML page renders from a direct local read of the store, never through a daemon.",
        ],
        see_also: &["chart", "list", "why"],
        topic: Topic::Reports,
    },
    CmdDoc {
        verb: "chart",
        aliases: &[],
        method: "event.list",
        summary: "Render throughput, heatmap, or burndown charts.",
        usage: "tasqx chart <throughput|heatmap|burndown> [filter…] [--weeks n|--days n] [--year] [--project p]",
        examples: &[
            ex("tasqx chart throughput"),
            ex("tasqx chart heatmap --year"),
            ex("tasqx chart burndown --days 30"),
            ex("tasqx chart burndown project:work"),
        ],
        notes: &[
            "--socket is refused here rather than honoured (DESIGN.md D73): charts render from a direct local read of the store, never through a daemon.",
            "Each subcommand takes the same filter DSL `list`/`report`/`agenda` do (DESIGN.md D173), e.g. `tasqx chart burndown project:work` — only that project's tasks and events count. Omit it for the whole store, the same default as before.",
            "`--project work` is shorthand for the `project:work` filter term above, kept for scripts already spelling it that way. The filter term is the more general form — it composes with a second predicate, which `--project` alone cannot.",
        ],
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
        notes: &[
            "`theme list` marks the theme in effect with `*`, the mark `tasqx projects` puts on the default project.",
        ],
        see_also: &["report", "manual"],
        topic: Topic::Reports,
    },
    CmdDoc {
        verb: "config",
        aliases: &[],
        method: "— (registry + core.capabilities)",
        summary: "Read and change tasqx settings.",
        usage: "tasqx config <list|get <key>|describe <key>|set <key> <value>|unset <key>|path|store|edit>",
        examples: &[
            ex("tasqx config list"),
            ex("tasqx config get theme.name"),
            ex("tasqx config describe daemon.idle_timeout"),
            ex("tasqx config path"),
            ex("tasqx config store"),
            ex_norun("tasqx config set theme.name gruvbox", "writes config.toml, preserving your comments"),
            ex_norun("tasqx config edit", "full-screen editor; arrow through themes and watch them apply"),
        ],
        notes: &[
            "`describe` prints a setting's meaning, unit and default — for `daemon.idle_timeout` the number `get`/`list` show is minutes, and the unit is otherwise only visible in the daemon's own startup banner.",
            "`store` answers which store you are actually writing to — and says so when a running daemon owns it, because the remote path never consults $TASQX_DB, so a correct $TASQX_DB is silently inert whenever a daemon is listening.",
            "`list` shows both homes. Most settings live in `config.toml`; `default_project` lives in the store and is set with `tasqx use` (D21).",
            "Resolution order is `--flag`, then `$TASQX_*`, then `config.toml`, then the built-in default (D9). The SOURCE column names the layer that won.",
            "`edit` opens an interactive screen: up/down to move, enter to toggle a switch or open a theme picker, esc to leave. Moving through the theme list repaints the screen in that theme before anything is written.",
            "`edit` needs a real terminal. Piped or redirected it refuses and exits 2 rather than writing escape codes into your pipe — scripts should use `set`/`unset` (D26).",
            "`tokens.enabled` allows the daemon to attribute AI token usage from local tool transcripts; `otlp.enabled` (+ `otlp.port`) runs a local OTLP/HTTP receiver instead — see `tasqx manual reports` and `tasqx manual daemon`. Both require a running daemon; self-reported counts (`token.add`) work with both off.",
        ],
        see_also: &["use", "theme", "manual"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "memory",
        aliases: &[],
        // `memory.import` is named in full, and as its own ` + ` part rather
        // than a seventh suffix, for two reasons: the verb reaches it and
        // nothing else does — the omission printed "no CLI verb reaches this
        // method" on the API reference under the one method whose documented
        // use is a command line (#647) — and a seventh suffix makes the
        // slash-joined token unbreakable past a 40-column manual page.
        method: "memory.search + get/add/remove/list/update + memory.import + memory.refresh",
        summary: "Store and search knowledge: docs, patterns, and your task annotations (D41).",
        usage: "tasqx memory <add <title> <body> [--source s] [--project p] [--standing]|search <words…> [--limit n] [--scope s] [--raw]|list [--limit n] [--offset n] [--project p] [--standing]|show <id>|update <id> [--title t] [--body b] [--source s] [--project p] [--standing true|false] [--expected-rev n]|rm <id>|import <path> [--project p]|import --refresh>",
        examples: &[
            ex("tasqx memory add \"Deploy runbook\" \"deploys go through the blue-green pipeline\""),
            ex("tasqx memory search blue-green"),
            ex_norun(
                "tasqx memory search 'pipel*' --raw",
                "FTS5 operator syntax: prefix search, AND/OR, column filters",
            ),
            ex_norun("tasqx memory list", "browse without a query, newest first"),
            ex_norun(
                "tasqx memory import docs/adr",
                "one doc per .md file; title from the first # heading",
            ),
            ex_norun(
                "tasqx memory import docs/adr --project ledger",
                "scopes every doc in the batch; a re-import naming a different project moves it (#657)",
            ),
            ex_norun(
                "tasqx memory import --refresh",
                "re-reads every imported doc whose file changed; takes no path",
            ),
            ex_norun(
                "tasqx memory update 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12 --body \"corrected text\"",
                "in-place correction; add/remove leave the stale doc searchable or lose it for good",
            ),
            ex_norun(
                "tasqx memory rm 019f8422-7b3e-7c41-a2d9-6f1b0e5c8a12",
                "by the id search printed",
            ),
        ],
        notes: &[
            "A hit is two lines: the title, where it came from and the handle that opens it, then the words that matched. The handle is a doc's id, which `tasqx memory show <id>` reads, or `annotation on #N`, which `tasqx show N` opens — `memory show` refuses an annotation's id and names the task instead (exit 4).",
            "Search covers your imported docs AND task annotations, bm25-ranked, with stemming (\"reviewing\" matches \"review\"). Plain words are matched as phrases (hyphens and dots are safe); pass --raw for FTS5 operator syntax. The response's total/has_more say what --limit left out.",
            "list browses every doc without a query — the enumeration search can't do without one — newest-modified first, paged the same way as `tasqx list`.",
            "update replaces title/body/source/project in place, guarded by the same optimistic-concurrency rev `tasqx modify` uses. rm is permanent; update is the correction path that keeps the id and doesn't pollute search with a stale duplicate.",
            "Import is one transaction: a bad file imports nothing, and re-importing a directory replaces docs from the same source instead of duplicating them. --project scopes the whole batch; omitted, an existing doc keeps whatever scope it already had (like --standing), a new one stays global, and naming a different project on a re-import moves the doc's scope there.",
            "`import --refresh` takes no path: it re-reads every doc that recorded an origin file (D180) whose file has changed since, keeping each doc's id, scope and standing flag and bumping its rev; a file that only got a newer timestamp counts as unchanged, because the bytes are compared. A doc whose file can no longer be read is named and kept, never deleted — the file may be on another machine or an unmounted volume — and the command still exits 0.",
            "A file's stored source is its path relative to the git toplevel above it, or, outside a git work tree, relative to the current directory — so docs/, ./docs/ and its absolute path are one document per file from any starting directory or machine (D179); an older spelling already in memory is named in a note rather than removed.",
            "An MCP agent reaches the same store: tasqx_search_memory works even read-only, so agents can consult knowledge while executing tasks.",
            "A standing doc (`--standing`) is a ruling meant for every session of its scope — a correction given once, sent to MCP clients at session start — and `list --standing` shows them; more than 15 in one scope earns a hint to merge or retract (D156).",
        ],
        see_also: &["annotate", "mcp", "api"],
        topic: Topic::Automation,
    },
    CmdDoc {
        verb: "tokens",
        aliases: &[],
        method: "tokens.recompute + token.add",
        summary: "Record token spend after the fact, or repair stored attribution (D50, D167).",
        usage: "tasqx tokens recompute [--apply] | tasqx tokens add <ref> [--total N] [--in N] \
                [--out N] [--cache-read N] [--cache-creation N] [--tool TOOL] [--model MODEL]",
        examples: &[
            ex_norun(
                "tasqx tokens add 602 --total 37898",
                "a count that arrived after completion, as one unsplit number",
            ),
            ex_norun(
                "tasqx tokens add 602 --in 18400 --out 2600 --tool claude-code",
                "the same, split",
            ),
            ex("tasqx tokens recompute"),
            ex_norun(
                "tasqx tokens recompute --apply",
                "write the repair, after reviewing the dry-run delta",
            ),
        ],
        notes: &[
            "`add` records a self-report at confidence `medium` — what `done`'s token flags record, for a count that arrives later (D167). `--total` is one unsplit number, kept apart from the four buckets and counted in full by a budget; give it alone or give the split flags, never both.",
            "A bare `tasqx tokens recompute` prints the per-task delta and writes NOTHING; `--apply` is the explicit opt-in for the one verb in the API built to delete measurement rows.",
            "The pass runs two phases: rewrite D50-refusal rows (`source=log-parse` only, self-report/OTLP never touched), and downgrade confidence to `low` for any task whose transcript is gone; then locate the transcript of every done Claude Code task with no measured (log-parse or OTLP) row and append a HIGH measurement if found. Tasks it cannot measure — no transcript found, or every sample contested — are reported as `skipped`.",
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
        usage: "tasqx export [filter…] [--include-unscoped]",
        examples: &[
            ex("tasqx export"),
            ex("tasqx export project:work"),
        ],
        notes: &["Canonical JSON; a filtered export trims edges leaving the set and reports `dropped_dependencies` (D12).",
                 "The document carries `projects` and `default_project` too, so a restore gives back the store and not only its tasks (D37).",
                 "A filter narrows `projects`/`docs`/`links`/`events` to what the exported tasks need — an unfiltered export still carries all of them; `--include-unscoped` widens a filtered one back to docs with no project (D171).",
                 "The document carries the graph's explicit links too, so a restore gives back the edges and not only the nodes; a filtered export keeps a link only when BOTH its ends are in it, and counts the rest as `dropped_links` (D181)."],
        see_also: &["import", "api"],
        topic: Topic::JsonApi,
    },
    CmdDoc {
        verb: "import",
        aliases: &[],
        method: "store.import",
        summary: "Load tasks from a JSON file or stdin.",
        usage: "tasqx import <file|-> [--dry-run] [--merge]",
        examples: &[
            ex_norun("tasqx import backup.json", "from a file"),
            ex_norun("tasqx export | tasqx import -", "from stdin"),
            ex_norun("tasqx import backup.json --dry-run", "preview it first"),
            ex_norun("tasqx import other-machine.json --merge", "fold in another store"),
        ],
        notes: &["A task whose number a DIFFERENT task here already holds keeps its id and takes the next free one; every move is printed and reported as `renumbered` (D177). A task this store already holds keeps the number it has here.",
                 "A memory doc whose `source` a DIFFERENT doc here already holds merges onto that doc rather than being refused: the stored id is kept, the later `modified` wins the text, and every merge is printed and reported as `docs_merged` (D183).",
                 "Links are restored after everything they point at, counted in `links_imported`; a link naming an end neither the document nor this store holds refuses the whole import by name (D181).",
                 "`--dry-run` runs the whole import against this store and rolls it back: every renumbering, merge and refusal prints exactly as a real import would, and the run ends with `nothing was written` (D184).",
                 "`--merge` folds a second live store in rather than restoring over this one: a task this store already holds keeps its own annotations, checks, tags and edges and takes the payload's beside them, its `_rev` guard is skipped, and its title, status and the rest follow whichever side's `modified` is later — one line per task, reported as `merged` (D185). Without it the payload replaces a known task's child rows wholesale, which is what a restore wants."],
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
        notes: &[
            "The stdio one-shot transport; the envelope key is `\"tasqx\":\"1\"`.",
            "--socket is refused here rather than honoured (DESIGN.md D73): `api` runs against an in-process engine, never a daemon. --theme is likewise never read — there is nothing here to render.",
        ],
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
        usage: "tasqx docs [--out PATH | --no-open | --stdout] [--screen NAME]",
        examples: &[
            ex_norun("tasqx docs", "open the browser guide"),
            ex("tasqx docs --stdout"),
            exn("tasqx docs --screen list", "one captured screen, standalone"),
        ],
        notes: &[
            "The exhaustive browser guide (self-contained HTML). For a quick in-terminal guide, `tasqx manual`.",
            "`--screen NAME` writes ONE captured screen instead of the guide: a standalone page carrying the guide's terminal styling and nothing else, to `--out` or to stdout. It is the rasterisation path behind the README's pictures (`scripts/snap.sh`), and an unknown name exits 2 listing every screen there is.",
        ],
        see_also: &["manual"],
        topic: Topic::GettingStarted,
    },
    CmdDoc {
        verb: "about",
        aliases: &[],
        method: "— (no store)",
        summary: "Who made tasqx, where to find it, and what build this is.",
        usage: "tasqx about",
        examples: &[ex("tasqx about")],
        notes: &[
            "Six lines: the author, two links, the build this binary was made from, the store it would open, and `times UTC` — every clock tasqx reads and prints is UTC, including one typed without an offset (D132). The build is the string `tasqx --version` prints — the crate version plus the commit — and reads `unknown` on a build from a source tarball, which has no git to ask.",
            "It opens no store and no network. The store line is the path a command WOULD open, resolved without creating anything, so asking where things live never authors a data directory.",
            "A credits screen is not data, so there is no API method and `--json` is declined with a note (D31's carve-out list, D127).",
        ],
        see_also: &["manual", "docs"],
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
    CmdDoc {
        verb: "setup",
        aliases: &[],
        method: "— (no store)",
        summary: "Install the Claude Code integration: the MCP server and the bundled skills.",
        usage: "tasqx setup [--list | --yes [--force]] [--only NAME]... [--home DIR]",
        examples: &[
            // Safe: `--list` only reads `~/.claude.json` and two skill files.
            // Every example that installs is NoRun — `tests/help.rs` runs the
            // Safe ones on the developer's own machine.
            exn("tasqx setup --list", "what is installed; writes nothing"),
            ex_norun(
                "tasqx setup",
                "a checklist: space ticks, enter installs what is ticked",
            ),
            ex_norun(
                "tasqx setup --yes",
                "installs everything not installed, and keeps a skill you edited",
            ),
            ex_norun(
                "tasqx setup --yes --force --only retro",
                "replaces the retro skill with the copy this build carries",
            ),
        ],
        notes: &[
            "Three items: `mcp` registers `tasqx mcp serve --scope write` with Claude Code at user scope by running `claude mcp add`; `tasqx-workflow` and `retro` are skills written to `~/.claude/skills/<name>/SKILL.md` from copies compiled into this binary, so they match the tasqx you run (D159).",
            "A skill that exists and is not byte-equal to the bundled copy reads `differs` — an older copy and your own edit look the same — and is kept unless you pass --force or tick it on the screen. So is an MCP registration that runs anything else, such as the read-only `tasqx mcp serve`; replacing it runs `claude mcp remove` first.",
            "It never writes `~/.claude.json` itself. Without the `claude` command on PATH, the mcp item prints the exact command to run instead. Piped, with no flags, it prints the list and exits 0.",
            crate::setup::RIPWIRE_INSTALL_HINT,
        ],
        see_also: &["mcp"],
        topic: Topic::Automation,
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
        // #229 item 14: `ex_norun(cmd, "")` sets `note: Some("")`, so a
        // bare `if let Some` printed the `#` marker with nothing after it —
        // guarded here so an empty note can never render a dangling comment,
        // on top of fixing the four call sites that produced one.
        if let Some(n) = e.note.filter(|n| !n.is_empty()) {
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

    /// #229 item 14: `stop`, `reopen`, `undep` and `annotate`'s help pages
    /// printed an example with a trailing `#` and nothing after it —
    /// `ex_norun(cmd, "")` sets `note: Some("")`, and `after_help` printed
    /// the `#` marker unconditionally once `note` was `Some`, regardless of
    /// whether there was a comment to introduce. `--help` is the tool's own
    /// front door; a dangling `#` reads as truncated output.
    #[test]
    fn no_rendered_example_ends_in_a_dangling_comment_marker() {
        for d in COMMAND_REF {
            let text = after_help(d.verb);
            for line in text.lines() {
                assert!(
                    !line.trim_end().ends_with('#'),
                    "{}: an example line ends in a bare `#` with no comment \
                     after it: {line:?}",
                    d.verb
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

    /// The `dashboard` page states how many panel names D80 retired, and that
    /// count belongs to `RETIRED_PANEL_NAMES`, not to a sentence.
    ///
    /// It said "The four panel names D80 retired" and then listed five of them
    /// in the same clause. Nothing could see it: the names were right, the
    /// array was right, and only the number between them was wrong — the exact
    /// shape a reader trusts and no gate reads.
    #[test]
    fn the_dashboard_note_counts_the_panel_names_d80_retired() {
        let retired = crate::tui::dashboard::model::RETIRED_PANEL_NAMES;
        let note = find("dashboard")
            .expect("dashboard is documented")
            .notes
            .iter()
            .find(|n| n.contains("D80 retired"))
            .expect("the dashboard page says which panel names D80 retired");

        let word = [
            "zero", "one", "two", "three", "four", "five", "six", "seven", "eight",
        ][retired.len()];
        assert!(
            note.contains(&format!("The {word} panel names D80 retired")),
            "the note miscounts the retired panels — there are {} ({retired:?}):\n{note}",
            retired.len()
        );
        for name in retired {
            assert!(
                note.contains(&format!("`{name}`")),
                "the note never names `{name}`, so the count and the list disagree:\n{note}"
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

    /// #228.2: `undo --help` read as if it undid "the last thing" generally,
    /// but on any active store the newest event is almost always `add`,
    /// `done` or `modify` — none of the five undoable operations — so `undo`
    /// is unreachable in practice the moment one of those lands. The help
    /// must say so plainly rather than let the reader learn it from a
    /// `conflict` error every time.
    #[test]
    fn undo_help_says_one_blocking_event_walls_off_everything_before_it() {
        let h = after_help("undo");
        assert!(
            h.contains("permanently blocks undo"),
            "`undo --help` must say a non-undoable event permanently blocks \
             reaching back further, not just refuse the newest one: {h}"
        );
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

    /// tasqx audit 2026-09 #226.1: `add -h` (rendered from these notes) taught
    /// seven of the nine working sugar tokens and silently dropped `wait:`
    /// and `scheduled:`/`sched:` — the two with the largest behavioural
    /// consequence, since both park a new task in `backlog`. Bound to the
    /// parser the same way `docs.rs`'s `documented_sugar_keys_match_the_parser`
    /// binds the HTML page, so a future alias the parser gains and this note
    /// does not name fails the build instead of rotting quietly a second time.
    #[test]
    fn add_notes_sugar_keys_match_the_parser() {
        let d = find("add").expect("add is documented");
        let mut documented: Vec<String> = Vec::new();
        for note in d.notes {
            for chunk in note.split('`').skip(1).step_by(2) {
                // A backtick span names a key by starting with it and then
                // trailing off (`due:…`) or naming an example value
                // (`project:p`) — the key is everything up to and including
                // the first colon, whichever spelling follows it.
                if let Some(i) = chunk.find(':') {
                    documented.push(chunk[..=i].to_string());
                }
            }
        }
        documented.sort();
        documented.dedup();
        let mut real: Vec<String> = crate::sugar::value_key_spellings()
            .iter()
            .map(|k| k.to_string())
            .collect();
        real.sort();
        assert_eq!(
            documented, real,
            "`add`'s cmddoc notes (what `add -h` and `tasqx manual capturing` \
             both render) have drifted from sugar::VALUE_KEYS"
        );
    }

    /// tasqx audit 2026-09 #226.3: the archive NOTE said "No verb may name an
    /// archived project, this one included" — but `list`, `report` and
    /// `agenda` all still show it and its tasks (verified against the
    /// binary); only WRITES are refused. The overbroad absolute is the first
    /// thing a user reads when deciding whether archiving is safe.
    #[test]
    fn archive_notes_scope_the_refusal_to_writes_and_mention_reads() {
        let d = find("archive").expect("archive is documented");
        let joined = d.notes.join(" ");
        assert!(
            !joined.contains("No verb may name an archived project"),
            "the archive NOTE must not claim every verb refuses an archived \
             project — list/report/agenda still show it: {joined}"
        );
        assert!(
            joined.contains("No write may name an archived project")
                || (joined.to_lowercase().contains("write") && joined.contains("archived")),
            "the archive NOTE must scope the refusal to writes: {joined}"
        );
        assert!(
            joined.contains("list") || joined.contains("read"),
            "the archive NOTE must say that reads (list/report/agenda) still \
             see an archived project and its tasks: {joined}"
        );
    }

    /// tasqx audit 2026-09 #226.6: neither `tasqx manual dashboard` nor the
    /// generated `tasqx docs` guide named a single one of the dashboard's key
    /// bindings beyond `p`/enter/q/esc/ctrl-c — the rest (`1-8`, `tab`/`S-tab`,
    /// `j`/`k`, `g`/`G`, `r`/`R`, `w`, `l`) lived only behind the in-screen `?`
    /// overlay. Checked against the live `KEYS` table, not retyped, so a
    /// binding added to the overlay and not to this note fails the build —
    /// the same docs-drift idiom `docs.rs` already applies to verbs.
    #[test]
    fn dashboard_notes_mention_every_key_binding() {
        let d = find("dashboard").expect("dashboard is documented");
        let joined = d.notes.join(" ");
        for key in crate::tui::dashboard::KEYS {
            let spelling = key.keys.split(" / ").next().unwrap_or(key.keys);
            assert!(
                joined.contains(spelling),
                "`tasqx manual dashboard` never mentions the `{}` binding \
                 (help: {:?}): {joined}",
                key.keys,
                key.help
            );
        }
    }

    /// tasqx audit 2026-09 #223: `tasqx manual config` never mentioned
    /// `tokens.*`/`otlp.*` — a reader who saw `otlp.enabled false default` in
    /// `config list` had nowhere in the manual to learn what it turns on.
    #[test]
    fn config_notes_mention_tokens_and_otlp() {
        let d = find("config").expect("config is documented");
        let joined = d.notes.join(" ");
        for token in ["tokens.enabled", "otlp.enabled"] {
            assert!(
                joined.contains(token),
                "`tasqx manual config` must mention `{token}`: {joined}"
            );
        }
    }

    /// D124: both pages that describe `pick`'s keys say `s` starts and Enter
    /// reads. Enter used to start, and a page nobody re-read after the change
    /// is a page telling the reader to press the key that no longer does it.
    #[test]
    fn pick_and_dashboard_notes_say_s_starts_and_enter_reads() {
        // Positive, per page: each must say what Enter and `s` DO, in the
        // words the screen's own key table uses, rather than merely avoid two
        // old phrasings.
        for (verb, enter) in [
            ("pick", "Enter opens the task's `tasqx show` card"),
            ("dashboard", "Enter there reads a task"),
        ] {
            let joined = find(verb).expect("documented").notes.join(" ");
            assert!(joined.contains(enter), "`tasqx manual {verb}`: {joined}");
            assert!(
                joined.contains("`s` start"),
                "`tasqx manual {verb}`: {joined}"
            );
            let lower = joined.to_lowercase();
            assert!(
                !lower.contains("enter starts") && !lower.contains("enter there starts"),
                "`tasqx manual {verb}` still says Enter starts: {joined}"
            );
        }
    }

    /// D128: `tasqx manual pick` renders from these notes, and they carried
    /// D55's rule — that leaving exits 4 — for the whole life of the browser
    /// D124 made. Both halves are asserted, because saying only "exit 0" would
    /// let the refusal quietly go with it.
    #[test]
    fn pick_notes_say_leaving_exits_0_and_a_refusal_still_does_not() {
        let joined = find("pick").expect("pick is documented").notes.join(" ");
        assert!(
            joined.contains("exits 0"),
            "`tasqx manual pick` must say leaving is exit 0: {joined}"
        );
        assert!(
            !joined.contains("both exit 4"),
            "`tasqx manual pick` still states D55's rule: {joined}"
        );
        assert!(
            joined.contains("still exits 4"),
            "`tasqx manual pick` must keep the empty-set refusal non-zero: {joined}"
        );
    }

    /// `pick --help`'s own clap doc comment already documents ctrl-n/ctrl-p
    /// navigation; `tasqx manual pick` (rendered from these notes, not from
    /// clap's doc comment) did not.
    #[test]
    fn pick_notes_mention_ctrl_navigation() {
        let d = find("pick").expect("pick is documented");
        let joined = d.notes.join(" ");
        for token in ["ctrl-n", "ctrl-p"] {
            assert!(
                joined.contains(token),
                "`tasqx manual pick` must mention `{token}`: {joined}"
            );
        }
    }
}
