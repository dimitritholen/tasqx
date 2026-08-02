//! `tasqx manual` — the complete guide, in the terminal, themed and navigable.
//!
//! Renders from [`crate::cmddoc::COMMAND_REF`] (per-command reference) plus a
//! handful of concept sections. Navigation is the table of contents plus
//! `tasqx manual <name>`; there is no pager (kept dependency-free and portable).
//! For the exhaustive browser guide, `tasqx docs`.

use crate::cmddoc::{self, CmdDoc, Topic};
use crate::theme::Ctx;

pub fn render(ctx: &Ctx, arg: Option<&str>) -> Result<String, String> {
    match arg {
        None => Ok(toc(ctx)),
        Some(name) => {
            if let Some(d) = cmddoc::find(name) {
                Ok(command_section(ctx, d))
            } else if let Some(t) = Topic::ALL.iter().find(|t| t.slug() == name) {
                Ok(topic_section(ctx, *t))
            } else {
                Err(unknown(name))
            }
        }
    }
}

fn toc(ctx: &Ctx) -> String {
    let mut s = String::new();
    s.push_str(&ctx.paint("header", "TASQX MANUAL"));
    s.push('\n');
    s.push_str(&ctx.paint("muted", &ctx.hrule(40)));
    s.push_str("\n\nTOPICS\n");
    for (i, t) in Topic::ALL.iter().enumerate() {
        s.push_str(&format!(
            "  {:>2}  {}   {}\n",
            i + 1,
            ctx.paint("accent", t.slug()),
            ctx.paint("muted", t.title()),
        ));
    }
    s.push_str("\nCOMMANDS\n");
    for d in cmddoc::COMMAND_REF {
        s.push_str(&format!(
            "  {}   {}\n",
            ctx.paint("accent", &format!("{:<9}", d.verb)),
            d.summary,
        ));
    }
    s.push_str(&format!(
        "\n{} Jump to a topic or command:  {}\n",
        ctx.arrow(),
        ctx.paint("accent", "tasqx manual <name>"),
    ));
    s.push_str(&format!(
        "{} Full browser guide:          {}\n",
        ctx.arrow(),
        ctx.paint("accent", "tasqx docs"),
    ));
    s
}

fn command_section(ctx: &Ctx, d: &CmdDoc) -> String {
    let mut s = String::new();
    let alias = if d.aliases.is_empty() {
        String::new()
    } else {
        format!("  (aliases: {})", d.aliases.join(", "))
    };
    s.push_str(&ctx.paint("header", &format!("tasqx {}", d.verb)));
    s.push_str(&ctx.paint("muted", &alias));
    s.push('\n');
    s.push_str(&ctx.paint("muted", &format!("API method: {}", d.method)));
    s.push_str("\n\n");
    s.push_str(d.summary);
    s.push_str("\n\n");
    s.push_str(&ctx.paint("accent", "USAGE"));
    s.push_str(&format!("\n  {}\n\n", d.usage));
    s.push_str(&ctx.paint("accent", "EXAMPLES"));
    s.push('\n');
    for e in d.examples {
        s.push_str(&format!("  {}", e.cmd));
        if let Some(n) = e.note {
            s.push_str(&ctx.paint("muted", &format!("    {} {}", ctx.mid(), n)));
        }
        s.push('\n');
    }
    if !d.notes.is_empty() {
        s.push('\n');
        for n in d.notes {
            s.push_str(&format!("  {}\n", ctx.paint("muted", n)));
        }
    }
    if !d.see_also.is_empty() {
        s.push_str(&format!(
            "\nSee also: {}\n",
            d.see_also.join(&format!(" {} ", ctx.mid()))
        ));
    }
    s
}

fn topic_section(ctx: &Ctx, t: Topic) -> String {
    let mut s = String::new();
    s.push_str(&ctx.paint("header", &t.title().to_uppercase()));
    s.push('\n');
    s.push_str(&ctx.paint("muted", &ctx.hrule(t.title().len())));
    s.push_str("\n\n");
    s.push_str(topic_body(t));
    s.push('\n');
    // Which commands belong to this topic, as a follow-on.
    let verbs: Vec<&str> = cmddoc::COMMAND_REF
        .iter()
        .filter(|d| d.topic == t)
        .map(|d| d.verb)
        .collect();
    if !verbs.is_empty() {
        s.push_str(&format!(
            "\n{} Commands: {}\n",
            ctx.arrow(),
            ctx.paint("accent", &verbs.join(", ")),
        ));
    }
    s
}

fn unknown(name: &str) -> String {
    let mut valid: Vec<String> = Topic::ALL.iter().map(|t| t.slug().to_string()).collect();
    valid.extend(cmddoc::COMMAND_REF.iter().map(|d| d.verb.to_string()));
    format!(
        "no manual page for {name:?}. Try one of: {}",
        valid.join(", ")
    )
}

fn topic_body(t: Topic) -> &'static str {
    match t {
        Topic::GettingStarted => {
            "\
tasqx is a fast, terminal-first, AI-native task manager.

The whole loop is four commands:
  tasqx init <project>   create a project (just a name)
  tasqx add <title>      capture a task into the default project
  tasqx next             see the one thing to do now
  tasqx done <ref>       complete it

A project is just a name in the store — no folder is created.
The store lives at $TASQX_DB, else your platform data dir.

Deeper: `tasqx manual capturing` for the add grammar, or
`tasqx docs` for the full browser guide."
        }

        Topic::Projects => {
            "\
A project is just a name in the store — no folder, no path.

  tasqx init <name>      claim a new project name
  tasqx use <name>       make it the default for bare adds
  tasqx projects         list them; the default is marked `*`

A bare `tasqx add …` lands in the default project. The default
is claimed only if the store had none yet; archiving the default
project clears it, so a later add has no home until you `use`
another one."
        }

        Topic::Capturing => {
            "\
Capture with a title plus inline sugar:
  tasqx add Ship it due:friday +api !high project:work

Inline sugar:
  +tag            add a tag
  project:p       (or proj:p) set the project
  !high           priority (!high / !med / !low)
  due:…           a due date (natural language)
  est:4h          an effort estimate
  repeat:…        a recurrence rule
  remind:…        a reminder offset or time

`tasqx modify <ref>` sets fields; `--clear <field>` removes them.
Lifecycle: start · stop · done · cancel · reopen."
        }

        Topic::Dates => {
            "\
Dates take natural language: `friday`, \"in 3 days\", `eom`,
signed offsets like `-1d`.

Four date fields carry meaning:
  due        when it's due
  scheduled  when you can start
  wait       hide until this instant
  remind     when to nudge you

Recurrence forms:
  repeat:\"every 3 days\"
  repeat:\"weekly on mon,wed,fri\"
  repeat:\"monthly on day 15\"

Missed occurrences collapse to a single next one. `every N
months` can drift across short months — anchor by day of month."
        }

        Topic::Filters => {
            "\
Filters narrow any list:
  project:work   status:pending   +api
  due.before:friday   due.after:monday

Combine with boolean `or` and group with parentheses:
  tasqx list \"project:work and (+api or +ui)\"

Double-quote a value containing a space, and the quotes also
hide parentheses and the and/or keywords, as a shell does.
The quotes must REACH tasqx, so protect them from your shell —
wrap the whole token in single quotes (or backslash-escape it):
  tasqx list 'project:\"Home Renovation\"' '+\"needs paint\"'

tasqx does not guess where a value ended. Letting the shell eat
the quotes leaves `project:Home Renovation`, which is
`project:Home` plus a stray word, and that is refused with the
spelling above rather than answered with the wrong rows. The
same rule is what lets you pass a whole expression as one
argument: `tasqx list \"+api or +web\"` is the expression.

`add`/`modify` sugar is split by the same scanner, but the
write side ALSO honours the argument boundary your shell drew,
so it accepts one spelling the filter does not: `tasqx add
\"paint\" project:Home Renovation` files the task, while
`tasqx list project:Home Renovation` is refused. Reading is
where a guess would return wrong rows silently, so only the
read side refuses. Use the quoted spelling and both sides
agree. Write `\\\"` for a literal quote and
`\\\\` for a literal backslash — a name holding a quote needs
that form on both sides:
  tasqx add \"paint\" project:\"My \\\"Big\\\" Project\"

`due` is compared as an instant, not a calendar day. A bare
`tasqx` (or `tasqx list`) shows the working set."
        }

        Topic::Reminders => {
            "\
`remind:` takes a signed offset (`-1h`, `-30m`) kept symbolic:
moving `due` moves the reminder with it. An absolute time is
also accepted.

Reminders fire only while the daemon is running (`tasqx daemon`).
They are quiet by default — the OS toast lives behind the
off-by-default `notify-os` build feature."
        }

        Topic::Reports => {
            "\
  tasqx report [group_by]   counts, optionally grouped
                            (project | status | priority)
  tasqx report --html       one self-contained HTML file
  tasqx chart throughput    completions over time
  tasqx chart heatmap       activity calendar
  tasqx chart burndown      remaining work over time
  tasqx why <ref>           explain a task's urgency score
  tasqx theme list / show   browse and preview themes"
        }

        Topic::Daemon => {
            "\
`tasqx daemon` binds a local socket (a named pipe on Windows)
and serves the one JSON API as the single writer.

One-shot commands auto-route through a running daemon, so your
edits serialize safely. `tasqx watch` is a live view fed by the
daemon's push stream. `--no-daemon` is the escape hatch: run a
command directly against the store instead."
        }

        Topic::Automation => {
            "\
Every surface is a client of the one JSON API.

  tasqx mcp serve            stdio JSON-RPC for AI agents;
                             read-only by default
  tasqx mcp serve --scope write
                             explicitly expose write tools
  tasqx api                  one JSON envelope in → one out

Scope configures this local process; it is not authentication."
        }

        Topic::JsonApi => {
            "\
Everything speaks one envelope:
  {\"tasqx\":\"1\",\"id\":\"1\",\"method\":\"task.list\",\"params\":{}}

method + params go in; a result or an error comes out. Exit
codes mirror the error model: 0 ok, 2 bad_request, 4 not_found,
5 conflict.

`tasqx export` / `tasqx import` round-trip canonical JSON. See
`tasqx docs` for the full method table."
        }

        Topic::Completion => {
            "\
Tab completion for bash, zsh, fish, elvish and PowerShell — the
same five on Linux, macOS and Windows.

  tasqx completions <shell>              print the line
  tasqx completions --install            edit the startup file
  tasqx completions <shell> --uninstall  take it back out

The line, and the file it belongs in:

  bash        ~/.bashrc
              source <(TASQX_COMPLETE=bash tasqx)
  zsh         ~/.zshrc
              source <(TASQX_COMPLETE=zsh tasqx)
  fish        ~/.config/fish/completions/tasqx.fish
              TASQX_COMPLETE=fish tasqx | source
  elvish      ~/.elvish/rc.elv
              eval (E:TASQX_COMPLETE=elvish tasqx | slurp)
  powershell  $PROFILE
              $env:TASQX_COMPLETE = \"powershell\"; tasqx |
              Out-String | Invoke-Expression;
              Remove-Item Env:\\TASQX_COMPLETE

`--install` finds the shell from $SHELL, prints the exact block
it would add, and asks. A stdin that is not a terminal is a
refusal, not an implied yes — pass `--yes` from a script. It
writes ONE marked block and replaces it on a re-run rather than
appending a second, and `--uninstall` restores the file byte
for byte.

No Windows shell sets $SHELL, so name the shell there. And
PowerShell's `--install` refuses to guess where your profile
is: $PROFILE is a PowerShell variable, not an environment
variable, and it differs between Windows PowerShell 5.1,
PowerShell 7 and the ISE. Let the shell expand it for you:
  tasqx completions powershell --install --profile $PROFILE

cmd.exe is a permanent NON-GOAL, not a gap: no program can
register a completer with it at all. nushell is a real gap —
it completes external commands through its own `extern`
definitions, and nothing tasqx prints today activates them.
Asking for either says so, rather than \"unknown shell\".

What completes: verbs and flags, closed value sets
(`--priority`, `--scope`, `status:`), file paths, your task ids
carrying their titles, project and tag names, the capture sugar
(`+tag`, `project:x`, `!high`) and the whole filter grammar
including `-tag` exclusions.

Two details that are not what you would guess. An alias only
surfaces when no canonical name claims the prefix: `ls<TAB>`
gives `ls`, `mod<TAB>` gives `modify`. And the id menu is EVERY
task by urgency, not only the open ones — `reopen` and `why`
want the closed ones, and a menu that hid them would look like
an answer.

BEFORE YOU SWITCH IT ON

The variable is TASQX_COMPLETE, deliberately not the generic
COMPLETE that clap tools usually take. The protocol cannot tell
a callback from a real command: with a recognised shell name in
that variable, `tasqx add -- \"a real task\"` writes nothing,
exits 0, and does not add the task. A tasqx-specific name makes
that state improbable rather than impossible, so do not export
it by hand. COMPLETE on its own does nothing to tasqx.

A Tab press READS your store — a running daemon if there is
one, else the SQLite file opened read-only — inside a 150 ms
budget, and answers with no candidates rather than an error
whenever any of that fails, because a message on stderr lands
in the middle of the line you are typing. That read leaves
tasks.db-shm and tasks.db-wal beside your store: SQLite's
doing, and an ordinary `tasqx list` creates the same two and
removes them again. Your database is not altered — no
migration, not a byte. Set TASQX_NO_COMPLETE_LOOKUP=1 to turn
the value lookups off; verbs, flags and value sets still
complete."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{default_theme, Caps, Ctx};

    fn plain() -> Ctx {
        Ctx::new(default_theme(), Caps::PLAIN)
    }

    #[test]
    fn toc_lists_topics_and_commands() {
        let s = render(&plain(), None).unwrap();
        assert!(s.contains("TASQX MANUAL"), "{s}");
        assert!(s.contains("Getting started"));
        assert!(s.contains("init"));
        assert!(s.contains("tasqx manual"), "footer points at itself");
    }

    #[test]
    fn command_section_renders_examples() {
        let s = render(&plain(), Some("init")).unwrap();
        assert!(s.contains("tasqx init keuken-verbouwen"), "{s}");
        assert!(s.contains("project.create"), "shows the API method");
    }

    #[test]
    fn alias_resolves_to_its_command_section() {
        let s = render(&plain(), Some("edit")).unwrap(); // alias of modify
        assert!(s.contains("task.modify"), "{s}");
    }

    #[test]
    fn topic_section_renders() {
        let s = render(&plain(), Some("projects")).unwrap();
        assert!(s.to_lowercase().contains("project"), "{s}");
    }

    #[test]
    fn unknown_arg_is_an_error_naming_valid_targets() {
        let e = render(&plain(), Some("bogus")).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
        assert!(
            e.contains("init") || e.contains("projects"),
            "lists valid names: {e}"
        );
    }

    #[test]
    fn plain_caps_emit_no_escape_bytes() {
        for arg in [None, Some("init"), Some("filters")] {
            let s = render(&plain(), arg).unwrap();
            assert!(!s.contains('\x1b'), "plain render leaked ANSI for {arg:?}");
        }
    }
}
