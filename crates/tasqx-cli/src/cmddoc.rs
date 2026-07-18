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
    /// Classification only: the executable-examples guard in `tests/help.rs`
    /// (a separate crate that cannot see these internals) mirrors the Safe set
    /// by hand, so this field is never read inside the crate.
    #[allow(dead_code)]
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
}

impl Topic {
    pub const ALL: [Topic; 10] = [
        Topic::GettingStarted, Topic::Projects, Topic::Capturing, Topic::Dates,
        Topic::Filters, Topic::Reminders, Topic::Reports, Topic::Daemon,
        Topic::Automation, Topic::JsonApi,
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
const fn ex(cmd: &'static str) -> Example { Example { cmd, note: None, run: Safe } }
#[allow(dead_code)]
const fn exn(cmd: &'static str, note: &'static str) -> Example {
    Example { cmd, note: Some(note), run: Safe }
}
const fn ex_norun(cmd: &'static str, note: &'static str) -> Example {
    Example { cmd, note: Some(note), run: NoRun }
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
        usage: "tasqx add <title…> [--project p] [--due d] [-p H|M|L] [-t tag]… [--repeat r] [--remind r] [-e est]",
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
        usage: "tasqx modify <ref> [words/sugar…] [--clear <field>]… [--expected-rev N]",
        examples: &[
            ex_norun("tasqx modify 42 due:friday !high est:4h", "set fields"),
            ex_norun("tasqx modify 42 --clear due --clear remind", "clear fields"),
            ex_norun("tasqx modify 42 repeat:\"every monday\"", "set a recurrence"),
        ],
        notes: &[
            "Setting is `due:friday`/`--due friday`; removal is only ever `--clear <field>` — there is no magic empty value.",
            "`--expected-rev` fails with conflict (exit 5) if the task moved on.",
        ],
        see_also: &["add", "show", "why"],
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
        notes: &["Bare `tasqx` is `tasqx list` over the working set."],
        see_also: &["next", "report", "show"],
        topic: Topic::Filters,
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
        usage: "tasqx start <ref> [--keep]",
        examples: &[
            ex_norun("tasqx start 1", "single-active by default"),
            ex_norun("tasqx start 1 --keep", "keep others running"),
        ],
        notes: &[],
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
        usage: "tasqx done <ref>",
        examples: &[ex_norun("tasqx done 1", "completes; spawns the next recurrence if any")],
        notes: &[],
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
        see_also: &["init", "projects", "add"],
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
        notes: &[],
        see_also: &["init", "use"],
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
        ],
        notes: &[
            "group_by ∈ project|status|priority. `--html` defaults to stdout.",
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
        usage: "tasqx chart <throughput|heatmap|burndown> [opts]",
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
        usage: "tasqx theme <list|show [name]>",
        examples: &[
            ex("tasqx theme list"),
            ex("tasqx theme show nord"),
        ],
        notes: &[],
        see_also: &["report", "manual"],
        topic: Topic::Reports,
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
        notes: &["Canonical JSON; a filtered export trims edges leaving the set and reports `dropped_dependencies` (D12)."],
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
        summary: "Serve MCP, or mint a scoped token.",
        usage: "tasqx mcp <serve [--token T] | token --scope read|write>",
        examples: &[
            ex_norun("tasqx mcp token --scope write", "mint a scoped token"),
            ex_norun("tasqx mcp serve --token \"$TASQX_MCP_TOKEN\"", "serve over stdio JSON-RPC"),
        ],
        notes: &["Read/write scope fails closed: no token ⇒ read-only."],
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
    let Some(d) = find(verb) else { return String::new() };
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
                assert!(e.cmd.trim_start().starts_with("tasqx "),
                    "{}: example {:?} must start with `tasqx `", d.verb, e.cmd);
            }
        }
    }

    #[test]
    fn find_resolves_verbs_and_aliases() {
        assert_eq!(find("init").unwrap().verb, "init");
        assert_eq!(find("a").unwrap().verb, "add");     // alias
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
        assert!(d.usage.contains("--all"), "usage must offer --all: {}", d.usage);
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

    #[test]
    fn command_ref_covers_exactly_the_clap_surface() {
        use clap::CommandFactory;
        let mut real: Vec<String> = crate::Cli::command()
            .get_subcommands().map(|c| c.get_name().to_string()).collect();
        let mut doc: Vec<String> = verbs().iter().map(|s| s.to_string()).collect();
        real.sort(); doc.sort();
        let missing: Vec<_> = real.iter().filter(|v| !doc.contains(v)).collect();
        assert!(missing.is_empty(), "verbs with no cmddoc entry: {missing:?}");
        let invented: Vec<_> = doc.iter().filter(|v| !real.contains(v)).collect();
        assert!(invented.is_empty(), "cmddoc entries with no clap subcommand: {invented:?}");
    }

    #[test]
    fn command_ref_aliases_match_clap() {
        use clap::CommandFactory;
        let cmd = crate::Cli::command();
        for d in COMMAND_REF {
            let sub = cmd.get_subcommands().find(|c| c.get_name() == d.verb)
                .unwrap_or_else(|| panic!("no clap subcommand: {}", d.verb));
            let mut real: Vec<String> = sub.get_all_aliases().map(|a| a.to_string()).collect();
            let mut ours: Vec<String> = d.aliases.iter().map(|a| a.to_string()).collect();
            real.sort(); ours.sort();
            assert_eq!(real, ours, "alias drift on `{}`", d.verb);
        }
    }
}
