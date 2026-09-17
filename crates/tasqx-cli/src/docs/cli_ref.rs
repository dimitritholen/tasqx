//! The CLI reference: one anchored section per verb, derived rather than typed.
//!
//! Everything a section states about a verb comes from one of two places that
//! already exist and are already guarded:
//!
//! - [`crate::cmddoc::COMMAND_REF`] — the summary `tasqx <verb> -h` prints, the
//!   usage line, the topic it is filed under, its examples, its notes and its
//!   see-also list. One string per verb, used by the terminal and by this page.
//! - clap, through [`crate::command::cli_command`] — the aliases, and every
//!   argument the verb takes: its spelling, whether it is required, its default,
//!   its value vocabulary and the help text the terminal shows for it.
//!
//! So a verb that exists cannot be undocumented here (the loop is over clap's
//! own subcommand table, filtered through `COMMAND_REF`, which a guard in
//! `cmddoc` holds equal to it), a flag cannot be missing from a parameter table
//! (the rows ARE clap's arguments), and no sentence on this page describes a
//! verb differently from the way `-h` describes it, because there is one
//! sentence.
//!
//! **Output is never typed.** An example's output is a captured screen — a
//! fixture from `crates/tasqx-cli/docs-fixtures`, rendered as text by
//! [`crate::ansi_html`] (D149) — and the command shown above it is read out of
//! the capture manifest, so the command on the page is by construction the
//! command that produced the bytes under it. [`EXAMPLE_SCREENS`] is the one
//! hand-written table, and it is three columns of names that a guard below
//! checks against the fixture list and against `COMMAND_REF`'s examples.
//!
//! The two hand-written snippets left on the page are both bound to the engine
//! by a test: the `archive` refusal (its message is compared to what
//! `project.archive` really answers) and the `--expected-rev` conflict beside
//! the exit-code table.

use std::sync::LazyLock;

use clap::{Arg, ArgAction, Command as ClapCommand};

use crate::cmddoc::{CmdDoc, Example, Topic, COMMAND_REF};
use crate::html::esc;

use super::{
    h3, lead, p, page_close, page_open, param_table, pre_plain, ref_section, snippet, table,
    table_owned, term_block, term_screen, Param, ADD_FIELDS, DOCUMENTED_CLEAR_FIELDS, GLOBAL_FLAGS,
    VERBS,
};

/// The capture manifest, embedded so the command shown above a screen is the
/// command that was captured.
///
/// Typing it beside the fixture name instead would be a second copy of an
/// argument list — and the copies would differ in exactly the interesting case,
/// where the example in `cmddoc` acts on task 1 and the capture acts on the
/// demo store's task 51. A screen must never be labelled with a command that
/// did not produce it.
const MANIFEST: &str = include_str!("../../docs-fixtures/manifest.tsv");

/// Which captured screen illustrates which example: `(verb, example, fixture)`.
///
/// The middle column is the `cmddoc` example the screen stands for, and it is
/// what keeps the right-hand column from repeating a command the screen below
/// already shows. It is not the command that is rendered — that comes from the
/// manifest — so the two may differ, which they usually do: a capture runs
/// against the demo store and an example is written for a reader's own.
///
/// Guarded three ways by [`tests::example_screens_name_real_fixtures_and_real_examples`]:
/// the fixture exists, the `(verb, example)` pair is a real `COMMAND_REF`
/// example, and the captured command really is a run of that verb.
const EXAMPLE_SCREENS: &[(&str, &str, &str)] = &[
    ("init", "tasqx init keuken-verbouwen", "init-echo"),
    ("add", "tasqx add Buy milk", "add-echo"),
    (
        "modify",
        "tasqx modify 42 due:friday !high est:4h",
        "modify-echo",
    ),
    ("list", "tasqx list", "list"),
    ("agenda", "tasqx agenda", "agenda"),
    ("next", "tasqx next", "next"),
    ("dashboard", "tasqx dashboard", "dashboard"),
    ("pick", "tasqx pick", "pick"),
    ("show", "tasqx show 1", "show"),
    ("why", "tasqx why 1", "why"),
    ("start", "tasqx start 1", "start-echo"),
    ("stop", "tasqx stop 1", "stop-echo"),
    ("done", "tasqx done 1", "done-echo"),
    ("cancel", "tasqx cancel 1", "cancel-echo"),
    ("reopen", "tasqx reopen 1", "reopen-echo"),
    (
        "annotate",
        "tasqx annotate 1 Called the plumber, waiting on a quote",
        "annotate-echo",
    ),
    ("tag", "tasqx tag 1 api release", "tag-echo"),
    ("untag", "tasqx untag 1 api", "untag-echo"),
    ("dep", "tasqx dep 2 1", "dep-echo"),
    ("undep", "tasqx undep 2 1", "undep-echo"),
    ("use", "tasqx use keuken-verbouwen", "use-echo"),
    ("archive", "tasqx archive keuken-verbouwen", "archive-echo"),
    ("projects", "tasqx projects", "projects"),
    (
        "check",
        "tasqx check add 1 the notes name every breaking change",
        "check-echo",
    ),
    ("brief", "tasqx brief 1", "brief"),
    ("report", "tasqx report", "report"),
    ("chart", "tasqx chart burndown --days 30", "chart-burndown"),
    ("theme", "tasqx theme list", "theme-list"),
    ("config", "tasqx config list", "config-list"),
    ("memory", "tasqx memory list", "memory-list"),
    ("memory", "tasqx memory search blue-green", "memory-search"),
    (
        "api",
        "tasqx api <<< '{\"tasqx\":\"1\",\"id\":\"1\",\"method\":\"task.list\",\"params\":{}}'",
        "api-task-list",
    ),
    ("manual", "tasqx manual", "manual"),
];

/// Why a verb has no captured screen, for the ones that cannot have one.
///
/// A section with no output is either a gap or a fact about the verb, and the
/// reader cannot tell which from a blank space. Every verb is in exactly one of
/// these two tables — [`tests::every_verb_either_shows_output_or_says_why`] —
/// so adding a verb forces the question to be answered rather than skipped.
const NO_SCREEN: &[(&str, &str)] = &[
    (
        "undo",
        "it reverses the newest event, so a capture would have to record two commands \
         and the manifest records one.",
    ),
    (
        "unannotate",
        "it takes the id of one annotation, which is a UUID out of the store it is run \
         against.",
    ),
    (
        "import",
        "it names a file, and a path from the machine that captured it is the one thing \
         no second machine can reproduce.",
    ),
    (
        "export",
        "the document carries every project and knowledge doc whatever the filter says, so \
         the shortest export on the demo store runs past two hundred lines.",
    ),
    (
        "daemon",
        "it binds a socket and serves until it is stopped; there is no last line to capture.",
    ),
    (
        "setup",
        "it reports what is installed in the home directory of the machine it runs on, \
         which the demo store knows nothing about.",
    ),
    (
        "watch",
        "it redraws until you leave it, and it needs a running daemon to follow.",
    ),
    (
        "mcp",
        "it speaks JSON-RPC on stdin and stdout for as long as the agent holds the pipe.",
    ),
    (
        "tokens",
        "the demo store records no AI token spend, so the dry run has nothing to \
         repair and its whole screen is the one line that says so.",
    ),
    (
        "completions",
        "it prints one line of shell for your startup file — the same line \
         `--install` writes into it for you.",
    ),
    (
        "about",
        "it prints this machine's store path and the commit this binary was built from.",
    ),
    (
        "docs",
        "its output is this page — either the whole HTML document or the temporary path it \
         was written to.",
    ),
];

/// The whole Commands page.
pub(super) fn page() -> String {
    let cli = crate::command::cli_command();
    let mut s = page_open("commands");

    s.push_str(&lead(
        "Every verb is a thin translation to exactly one core API method: it builds a params \
         object, dispatches, and renders. Add <code>--json</code> to any of them to get the raw \
         API result instead of the table. Each section below is generated from the same \
         command table <code>tasqx &lt;verb&gt; -h</code> prints and from clap's own argument \
         list, so a flag that exists is a row here.",
    ));

    s.push_str(&global_flags());
    s.push_str(&verb_index());

    // One block per topic, in `Topic::ALL`'s order, with the verbs inside it in
    // `COMMAND_REF`'s — the grouping the terminal manual already uses, so a
    // reader moving between `tasqx manual` and this page finds the same
    // neighbourhoods.
    for topic in Topic::ALL {
        let verbs: Vec<&CmdDoc> = COMMAND_REF.iter().filter(|d| d.topic == topic).collect();
        if verbs.is_empty() {
            continue;
        }
        s.push_str(&format!(
            "<h3 id=\"cli-topic-{}\">{}</h3>",
            topic.slug(),
            esc(topic.title())
        ));
        for d in verbs {
            let sub = cli
                .get_subcommands()
                .find(|c| c.get_name() == d.verb)
                .unwrap_or_else(|| panic!("cmddoc documents `{}`, clap does not", d.verb));
            s.push_str(&verb_section(d, sub));
        }
    }

    s.push_str(&exit_codes());
    s.push_str(&page_close("commands"));
    s
}

// ============================================================================
// Page furniture
// ============================================================================

/// The global flags, as parameter rows. Unchanged from the page this file
/// replaced: `GLOBAL_FLAGS` is bound to clap's top-level argument list by a
/// guard in `cmddoc`, and the flag cell is split into a name and a value shape
/// rather than restated beside it.
fn global_flags() -> String {
    let mut s = h3("Global flags");
    s.push_str(&p("These work on every subcommand."));
    let split: Vec<(String, String)> = GLOBAL_FLAGS
        .iter()
        .map(|(flag, _)| super::split_flag(flag))
        .collect();
    let rows: Vec<Param> = split
        .iter()
        .zip(GLOBAL_FLAGS.iter())
        .map(|((name, ty), (_, effect))| Param {
            name,
            ty,
            // Nothing global is required: each has a defined behaviour when it
            // is absent, and that behaviour is what the description states.
            required: false,
            default: None,
            html_desc: effect,
        })
        .collect();
    s.push_str(&param_table("global-flags", &rows));
    s
}

/// The index: every verb, what it maps to, and a link to its section.
///
/// Rendered from `VERBS`, which the drift guards hold equal to clap's
/// subcommand and alias tables, with the description column read out of
/// `cmddoc` — the one string `-h` prints. With 43 sections on one page this
/// table is the page's navigation; the sidebar carries the page, not the verbs.
fn verb_index() -> String {
    let mut s = h3("The verb table");
    s.push_str(&p(&format!(
        "{} verbs, each with a section below. The method column is the API call the verb \
         makes — that mapping is the whole contract.",
        VERBS.len()
    )));
    let rows: Vec<Vec<String>> = VERBS
        .iter()
        .map(|&(verb, aliases, method)| {
            vec![
                format!(
                    "<a href=\"#{}\"><code>{verb}</code></a>",
                    section_id(&[verb])
                ),
                aliases.to_string(),
                method_cell(method),
                esc(super::verb_summary(verb)),
            ]
        })
        .collect();
    s.push_str(&table_owned(
        &["Verb", "Aliases", "Method", "What it does"],
        &rows,
    ));
    s
}

/// The exit codes, and the one worked example of a non-zero one.
///
/// A page-level section rather than a verb's: the codes are the CLI's contract
/// as a whole, and `the_errors_table_exit_column_matches_the_code_mapping`
/// reads this table against `ErrorCode`.
fn exit_codes() -> String {
    let mut s = h3("Exit codes");
    s.push_str(&p("Stable contract. Script against these."));
    s.push_str(&table(
        &["Code", "Meaning"],
        &[
            &["<code>0</code>", "Success."],
            &["<code>1</code>", "<code>internal</code>, or a failure beneath the request: the store would not open, a local write failed, or <code>watch</code> had no daemon to follow."],
            &["<code>2</code>", "<code>bad_request</code> — a bad value, an unparseable date, contradictory flags."],
            &["<code>4</code>", "<code>not_found</code> — no such task, project, or reference."],
            &["<code>5</code>", "<code>conflict</code> — a lost <code>--expected-rev</code> race, or a lifecycle rule."],
            &["<code>6</code>", "<code>unsupported_version</code> — the <code>tasqx</code> major you sent is not this build's."],
        ],
    ));
    s.push_str(&snippet(
        "tasqx modify 1 --expected-rev 1 -p H ; echo \"exit=$?\"",
        "error [conflict]: expected_rev 1 but task is at rev 2\nexit=5",
    ));
    s
}

// ============================================================================
// One verb
// ============================================================================

/// The id of a command's section: `cli-list`, `cli-config--list`.
///
/// `--` between the levels, and a single `-` between a section and one of its
/// parameter rows, so a row can always be told from a nested command's row:
/// `cli-config-json` belongs to `config`, `cli-config--list-json` to
/// `config list`. Machine-read only — the reader sees the heading.
fn section_id(path: &[&str]) -> String {
    format!("cli-{}", path.join("--"))
}

/// One verb: prose and parameters on the left, the commands on the right, the
/// screens they really printed across the bottom.
fn verb_section(d: &CmdDoc, sub: &ClapCommand) -> String {
    let mut left = String::new();

    // The line under the heading: what else it is called, and what it calls.
    let mut meta = String::new();
    if !d.aliases.is_empty() {
        meta.push_str(&format!(
            "Also <code>{}</code>. ",
            d.aliases.join("</code>, <code>")
        ));
    }
    meta.push_str(&format!("Maps to {}.", method_cell(d.method)));
    left.push_str(&p(&meta));
    left.push_str(&p(&esc(d.summary)));

    // A synopsis, not a command: it WRAPS rather than scrolling, because
    // `add`'s runs to twelve optional groups and the prose column is 27rem.
    // Everything else on this page that is terminal text keeps `pre`'s
    // scrolling, since a wrapped command line is a lie about where it ends.
    // The style rides on the `code`, not the `pre`: `pre code { white-space:
    // pre }` is the rule that wins otherwise, and the line went on scrolling.
    left.push_str(&format!(
        "<pre class=\"plain\"><code style=\"white-space: pre-wrap\">{}</code></pre>",
        esc(d.usage)
    ));
    left.push_str(&params_block(&[d.verb], sub));
    left.push_str(&extra_prose(d.verb));

    if !d.notes.is_empty() {
        let mut ul = String::from("<ul class=\"notes\">");
        for n in d.notes {
            ul.push_str(&format!("<li>{}</li>", md(n)));
        }
        ul.push_str("</ul>");
        left.push_str(&ul);
    }
    if !d.see_also.is_empty() {
        let links: Vec<String> = d
            .see_also
            .iter()
            .map(|t| {
                // `see_also` may name an alias; the anchor is the verb it
                // resolves to, and the spelling stays the one the author wrote.
                let verb = crate::cmddoc::find(t).map(|x| x.verb).unwrap_or(t);
                format!(
                    "<a href=\"#{}\"><code>{}</code></a>",
                    section_id(&[verb]),
                    esc(t)
                )
            })
            .collect();
        left.push_str(&p(&format!("See also {}.", links.join(" · "))));
    }

    // The right column: every example, minus the ones a screen below already
    // shows verbatim. A verb whose only example is its captured command keeps
    // it, because an empty code column reads as an unfinished section.
    let screens: Vec<(&str, &str, &str)> = EXAMPLE_SCREENS
        .iter()
        .copied()
        .filter(|(v, ..)| *v == d.verb)
        .collect();
    let shown: Vec<String> = screens.iter().map(|(_, _, f)| screen_cmd(f)).collect();
    let mut wanted: Vec<&Example> = d
        .examples
        .iter()
        .filter(|e| !shown.iter().any(|c| c == e.cmd))
        .collect();
    if wanted.is_empty() {
        wanted = d.examples.iter().collect();
    }
    let mut right = String::new();
    for e in wanted {
        right.push_str(&term_block(&esc(e.cmd)));
        if let Some(n) = e.note.filter(|n| !n.is_empty()) {
            right.push_str(&format!("<p class=\"muted\">{}</p>", md(n)));
        }
    }

    // The wide row: real output. Every fixture is at least 100 columns, which
    // is wider than the 27rem code column, so a screen goes under the section
    // across its whole width rather than scrolling inside a narrow box.
    let mut wide = String::new();
    for (_, _, fixture) in &screens {
        wide.push_str(&term_screen(&screen_cmd(fixture), fixture));
    }
    // A verb with no screen still says why, rather than leaving the reader to
    // wonder whether the section is unfinished.
    if screens.is_empty() {
        if let Some(reason) = NO_SCREEN
            .iter()
            .find(|(v, _)| *v == d.verb)
            .map(|(_, r)| *r)
        {
            wide.push_str(&super::note(&format!(
                "No captured screen: {} Every other block of output on this page is a \
                 recording of this build running against the demo store (D149).",
                md(reason),
            )));
        }
    }
    wide.push_str(&extra_output(d.verb));

    verb_block(&section_id(&[d.verb]), d.verb, &left, &right, &wide)
}

/// [`super::ref_section`] plus a full-width row under the two columns.
///
/// The template is otherwise identical, and
/// [`tests::a_verb_block_with_no_wide_row_is_the_reference_template`] asserts
/// that by comparing the two strings — a divergence in the shared markup fails
/// the build rather than styling one page differently from the other.
fn verb_block(id: &str, title: &str, left: &str, right: &str, wide: &str) -> String {
    let section = ref_section(id, title, left, right);
    if wide.is_empty() {
        return section;
    }
    // The span is inline: it is one declaration on one element in one template,
    // and the stylesheet is shared with the reference pages #647 owns.
    let row = format!("<div class=\"ref-wide\" style=\"grid-column: 1 / -1;\">{wide}</div>");
    match section.strip_suffix("</section>") {
        Some(head) => format!("{head}{row}</section>"),
        None => unreachable!("ref_section always closes its section"),
    }
}

// ============================================================================
// Parameters, from clap
// ============================================================================

/// The arguments this page documents for one command: everything clap declares
/// except its own `--help`/`--version`, positionals first.
///
/// One function, used by the renderer and by the guard, so "the rows are clap's
/// arguments" is a fact about the code and not a claim about it.
fn documented_args(cmd: &ClapCommand) -> Vec<&Arg> {
    let mut args: Vec<&Arg> = cmd
        .get_arguments()
        .filter(|a| !matches!(a.get_long(), Some("help") | Some("version")))
        .collect();
    args.sort_by_key(|a| !a.is_positional());
    args
}

/// A command's parameters, then its sub-commands' — `config`'s own row list is
/// empty and everything it takes lives one level down, so a section that
/// stopped at the top level would document nothing at all for seven verbs.
fn params_block(path: &[&str], cmd: &ClapCommand) -> String {
    let mut s = String::new();
    let args = documented_args(cmd);
    // Said once, for a verb that takes nothing at all (`undo`, `api`, `about`),
    // where the silence between the usage line and the notes would otherwise
    // read as a missing table. A sub-subcommand with no arguments has its own
    // sentence above it already.
    if args.is_empty() && path.len() == 1 && cmd.get_subcommands().next().is_none() {
        s.push_str(&p(
            "Takes no arguments of its own; the global flags still apply.",
        ));
    }
    s.push_str(&param_rows(&section_id(path), &args));

    for sub in cmd.get_subcommands() {
        let mut deeper: Vec<&str> = path.to_vec();
        deeper.push(sub.get_name());
        s.push_str(&format!(
            "<h4 id=\"h-{id}\">tasqx {name}</h4>",
            id = section_id(&deeper),
            name = esc(&deeper.join(" ")),
        ));
        // A nested alias (`check rm`, `memory get`) is accepted like a top-level
        // one, so it is named here the way `verb_section` names those.
        let aliases: Vec<String> = sub.get_all_aliases().map(esc).collect();
        if !aliases.is_empty() {
            s.push_str(&p(&format!(
                "Also <code>{}</code>.",
                aliases.join("</code>, <code>")
            )));
        }
        if let Some(about) = sub.get_about() {
            s.push_str(&p(&md(&about.to_string())));
        }
        s.push_str(&params_block(&deeper, sub));
    }
    s
}

/// One `param_table` over clap's arguments.
fn param_rows(section: &str, args: &[&Arg]) -> String {
    if args.is_empty() {
        return String::new();
    }
    // Owned first: `Param` borrows, and every cell here is computed.
    let cells: Vec<(String, String, bool, Option<String>, String)> = args
        .iter()
        .map(|a| {
            (
                arg_name(a),
                arg_ty(a),
                a.is_required_set(),
                arg_default(a),
                md(&a.get_help().map(|h| h.to_string()).unwrap_or_default()),
            )
        })
        .collect();
    let rows: Vec<Param> = cells
        .iter()
        .map(|(name, ty, required, default, desc)| Param {
            name,
            ty,
            required: *required,
            default: default.as_deref(),
            html_desc: desc,
        })
        .collect();
    param_table(section, &rows)
}

/// How the reader types it: `<ref>`, `<tag…>`, `--clear`, `-p, --priority`.
fn arg_name(a: &Arg) -> String {
    if a.is_positional() {
        let ellipsis = if matches!(a.get_action(), ArgAction::Append) {
            "…"
        } else {
            ""
        };
        return format!("<{}{ellipsis}>", value_name(a));
    }
    let long = a.get_long().unwrap_or_else(|| a.get_id().as_str());
    match a.get_short() {
        Some(c) => format!("-{c}, --{long}"),
        None => format!("--{long}"),
    }
}

/// The badge: `flag` for a switch, the closed vocabulary when it is short
/// enough to read, and the value's shape otherwise.
fn arg_ty(a: &Arg) -> String {
    if matches!(a.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
        return "flag".to_string();
    }
    // Hidden spellings are hidden from the shell's completion for a reason
    // (`--priority high` parses, and offering seven candidates for a
    // three-valued field is worse than offering three); the badge lists what
    // `-h` lists.
    let values: Vec<String> = a
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| v.get_name().to_string())
        .collect();
    if !values.is_empty() && values.len() <= 5 {
        return values.join("|");
    }
    let many = matches!(a.get_action(), ArgAction::Append);
    if a.is_positional() {
        // The name already spells the placeholder — `<ref>`, `<tag…>` — so the
        // badge says what kind of thing it is instead of repeating it.
        return if many { "strings" } else { "string" }.to_string();
    }
    format!("<{}{}>", value_name(a), if many { "…" } else { "" })
}

/// clap's value name, lowercased: `REF` → `ref`.
fn value_name(a: &Arg) -> String {
    a.get_value_names()
        .and_then(|n| n.first())
        .map(|n| n.as_str().to_lowercase())
        .unwrap_or_else(|| a.get_id().as_str().to_lowercase())
}

/// The value assumed when the argument is omitted, when clap declares one.
fn arg_default(a: &Arg) -> Option<String> {
    let values: Vec<String> = a
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().into_owned())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

// ============================================================================
// Screens, and the two things that are neither cmddoc nor clap
// ============================================================================

/// The command a fixture was captured from, read out of the manifest.
///
/// Panics when the name is not a row, which is the same build-time mistake
/// [`term_screen`] panics on and which the guard below catches first.
fn screen_cmd(name: &str) -> String {
    for line in MANIFEST.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let row: Vec<&str> = line.split('\t').collect();
        if row.first() != Some(&name) {
            continue;
        }
        let args = row.get(4).copied().unwrap_or_default();
        let stdin = row.get(6).copied().unwrap_or("-");
        return match stdin {
            "-" => format!("tasqx {args}"),
            body => format!("echo '{body}' | tasqx {args}"),
        };
    }
    panic!("no row named {name:?} in docs-fixtures/manifest.tsv")
}

/// The API page's anchor for a method.
///
/// Deep — `#api-task.add` — as soon as the JSON API reference carries per-method
/// ids, and the page itself until then, rather than a link that lands nowhere.
/// The check is on the rendered page and not on a flag, so nothing has to be
/// switched over on the day those ids arrive.
static API_PAGE: LazyLock<String> = LazyLock::new(super::page_api);

fn api_anchor(method: &str) -> String {
    api_anchor_in(&API_PAGE, method)
}

/// Split out from [`api_anchor`] so both branches can be tested: the page it
/// reads is a `LazyLock` over the real one, and the branch worth testing is the
/// one where the anchor does not exist yet.
fn api_anchor_in(api_page: &str, method: &str) -> String {
    let deep = format!("api-{method}");
    if api_page.contains(&format!("id=\"{deep}\"")) {
        deep
    } else {
        "api".to_string()
    }
}

/// A `method` cell: every real method linked to the API reference, everything
/// else — `(any)`, `— (no store)` — left as the words it is.
fn method_cell(method: &str) -> String {
    if method.starts_with('—') || method.starts_with('(') {
        return esc(method);
    }
    let parts: Vec<String> = method
        .split(" + ")
        .map(|m| {
            if tasqx_core::PARAMS.iter().any(|(name, _, _)| *name == m) {
                format!("<a href=\"#{}\"><code>{m}</code></a>", api_anchor(m))
            } else {
                format!("<code>{m}</code>")
            }
        })
        .collect();
    parts.join(" + ")
}

/// Plain text with backtick spans, as `cmddoc` writes it, turned into markup.
fn md(text: &str) -> String {
    let escaped = esc(text);
    let mut out = String::new();
    for (i, part) in escaped.split('`').enumerate() {
        if i % 2 == 1 {
            out.push_str("<code>");
            out.push_str(part);
            out.push_str("</code>");
        } else {
            out.push_str(part);
        }
    }
    out
}

/// The handful of things a verb's section carries that neither `cmddoc` nor
/// clap can supply, each of them generated from a table this crate owns.
///
/// This is deliberately short. Everything that used to be prose here and is a
/// restatement of a `cmddoc` note is gone: the note is rendered instead.
fn extra_prose(verb: &str) -> String {
    match verb {
        "add" => {
            let mut s = String::new();
            s.push_str(&p(
                "Anywhere a command takes a <code>&lt;ref&gt;</code>, it accepts either the short \
                 id you see in the table (<code>1</code>) or the full UUID. Short ids are for \
                 your fingers; UUIDs are stable forever and are what \
                 <a href=\"#data\">export</a> carries.",
            ));
            s.push_str(&p(
                "Flags win over inline sugar when both name the same field. The sugar column is \
                 bound to the parser's own key table, so a spelling here is a spelling that \
                 parses:",
            ));
            let rows: Vec<Vec<String>> = ADD_FIELDS
                .iter()
                .map(|(flag, sugar, notes)| {
                    vec![
                        (*flag).to_string(),
                        (*sugar).to_string(),
                        (*notes).to_string(),
                    ]
                })
                .collect();
            s.push_str(&table_owned(&["Flag", "Sugar", "Notes"], &rows));
            s
        }
        "modify" => {
            let mut s = p("<code>--clear</code> is repeatable over a closed set:");
            s.push_str(&pre_plain(&DOCUMENTED_CLEAR_FIELDS.join("   ")));
            s.push_str(&p(
                "<code>title</code> and <code>status</code> are absent on purpose: a task \
                 without a title is not a task, and lifecycle moves through \
                 <code>start</code>/<code>done</code>/<code>cancel</code> so their invariants \
                 hold. Naming a field in both a set and a <code>--clear</code> is a \
                 <code>bad_request</code>, not a precedence puzzle.",
            ));
            s
        }
        "list" => {
            // Both lists are rendered from the engine's own constants: this
            // page is where a reader looks up what `sort` and `fields` accept,
            // and a stale list here sends them to a key the engine refuses.
            let mut s = p(&format!(
                "Callers of the JSON API — and <code>--sort</code> here — may use any of {}, each \
                 optionally prefixed with <code>-</code> for descending. An unknown key is \
                 rejected rather than ignored.",
                tasqx_core::engine::SORT_KEYS
                    .iter()
                    .map(|k| format!("<code>{k}</code>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            s.push_str(&p(&format!(
                "<code>--fields</code> trims each row to the keys you need. The valid fields are \
                 {}. An unknown field is rejected rather than dropped, so a typo fails loudly \
                 instead of rendering an empty column forever.",
                tasqx_core::engine::TASK_FIELDS
                    .iter()
                    .map(|k| format!("<code>{k}</code>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
            s
        }
        "dashboard" => {
            // Both counts are DERIVED, and from DIFFERENT tables, which is the
            // whole point: the screen's roster and the document's are not the
            // same list, and a sentence saying so has to read both.
            let mut s = p(&format!(
                "<code>--json</code> is not the screen in text: the screen has {} panels, and the \
                 document carries a payload for {} of them ({}), beside the <code>status</code> \
                 header it always writes and a <code>panels</code> array naming what was asked \
                 for. The other two are drawn from the screen's own model, so \
                 <code>--panels pulse,effort</code> is a valid request that answers with no panel \
                 payload at all.",
                super::count_word(crate::tui::dashboard::model::PANEL_NAMES.len()),
                super::count_word(crate::tui::dashboard::json::PAYLOAD_PANELS.len()),
                crate::tui::dashboard::json::PAYLOAD_PANELS
                    .map(|panel| format!("<code>{panel}</code>"))
                    .join(", "),
            ));
            s.push_str(&p(
                "Every key below is generated from the same table the in-screen <code>?</code> \
                 overlay renders, so this page cannot drift from it.",
            ));
            let rows: Vec<Vec<String>> = crate::tui::dashboard::KEYS
                .iter()
                .map(|k| vec![format!("<code>{}</code>", esc(k.keys)), esc(k.help)])
                .collect();
            s.push_str(&table_owned(&["Key", "Does"], &rows));
            s
        }
        "pick" => {
            let mut s = p(
                "The keys are generated from the tables the screen's own key bar is drawn from.",
            );
            for (mode, keys) in [
                ("In the list", crate::tui::pick::LIST_KEYS),
                (
                    "In the list, with a search kept",
                    crate::tui::pick::LIST_FILTERED_KEYS,
                ),
                ("In the search", crate::tui::pick::SEARCH_KEYS),
                ("On a task's card", crate::tui::pick::DETAIL_KEYS),
            ] {
                let rows: Vec<Vec<String>> = keys
                    .iter()
                    .map(|k| vec![format!("<code>{}</code>", esc(k.keys)), esc(k.help)])
                    .collect();
                s.push_str(&p(mode));
                s.push_str(&table_owned(&["Key", "Does"], &rows));
            }
            s
        }
        _ => String::new(),
    }
}

/// Output a capture cannot produce, kept because a test binds it to the engine.
fn extra_output(verb: &str) -> String {
    match verb {
        // A second `archive` of the same project exits 5, and a row that exits
        // non-zero is refused by the capture script — rightly, since that is how
        // a broken fixture would otherwise be committed. So the refusal is
        // typed, and `the_archive_pages_refusal_is_the_one_the_engine_actually_gives`
        // compares it to the message `project.archive` really answers.
        "archive" => snippet(
            "tasqx archive home",
            "error [conflict]: project is already archived: home (`tasqx projects --all` \
             lists it; archiving it again would change nothing)",
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture named here exists, every example named here is real, and
    /// the captured command is a run of the verb it is filed under.
    ///
    /// The third check is the one worth having: a screen placed under the wrong
    /// verb is output that looks captured, is captured, and documents something
    /// else.
    #[test]
    fn example_screens_name_real_fixtures_and_real_examples() {
        for (verb, cmd, fixture) in EXAMPLE_SCREENS {
            assert!(
                crate::fixtures::names().contains(fixture),
                "EXAMPLE_SCREENS names `{fixture}`, which no row of \
                 docs-fixtures/manifest.tsv captures"
            );
            let d = crate::cmddoc::find(verb).unwrap_or_else(|| panic!("no such verb: {verb}"));
            assert!(
                d.examples.iter().any(|e| e.cmd == *cmd),
                "`{verb}` has no example {cmd:?} — the screen would illustrate nothing"
            );
            let captured = screen_cmd(fixture);
            let word = captured
                .split_whitespace()
                .skip_while(|w| *w != "tasqx")
                .nth(1)
                .unwrap_or_default();
            assert!(
                word == *verb || d.aliases.contains(&word),
                "`{fixture}` captured `{captured}`, which is not a run of `{verb}`"
            );
        }
    }

    /// Every verb clap has is a section on the page, with its heading.
    #[test]
    fn every_clap_verb_has_a_section() {
        let page = page();
        for sub in crate::command::cli_command().get_subcommands() {
            let id = section_id(&[sub.get_name()]);
            assert!(
                page.contains(&format!("id=\"{id}\"")),
                "`{}` has no section on the CLI reference",
                sub.get_name()
            );
        }
    }

    /// Every nested sub-command alias clap accepts is named under that
    /// sub-command's heading, before the next heading.
    #[test]
    fn every_nested_alias_is_named_under_its_heading() {
        let page = page();
        let mut seen = 0;
        for verb in crate::command::cli_command().get_subcommands() {
            for sub in verb.get_subcommands() {
                let id = section_id(&[verb.get_name(), sub.get_name()]);
                let after = page
                    .split(&format!("<h4 id=\"h-{id}\">"))
                    .nth(1)
                    .unwrap_or_else(|| panic!("no heading for {id}"));
                let block = after.split("<h4 ").next().unwrap_or(after);
                let block = block.split("</section>").next().unwrap_or(block);
                for alias in sub.get_all_aliases() {
                    seen += 1;
                    assert!(
                        block.contains(&format!("<code>{}</code>", esc(alias))),
                        "`tasqx {} {}` accepts `{alias}`, which the page never names",
                        verb.get_name(),
                        sub.get_name()
                    );
                }
            }
        }
        assert!(
            seen > 0,
            "no nested alias found: the guard is checking nothing"
        );
    }

    /// Every parameter row on the page, by section.
    fn rendered_params(page: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for chunk in page.split("<div class=\"param\" id=\"").skip(1) {
            let id = chunk.split('"').next().unwrap_or_default().to_string();
            let name = chunk
                .split("data-param=\"")
                .nth(1)
                .and_then(|c| c.split('"').next())
                .unwrap_or_default()
                .to_string();
            out.push((id, name));
        }
        out
    }

    /// Each section's rows are exactly its command's clap arguments.
    ///
    /// Walked over clap's tree rather than over a list of verbs, so a new
    /// sub-subcommand (`chart heatmap`, `memory update`) joins this check the
    /// moment it exists — and set equality in both directions, because a row
    /// the page invents is as wrong as a flag it drops.
    #[test]
    fn every_sections_parameter_rows_are_its_clap_arguments() {
        let page = page();
        let rendered = rendered_params(&page);
        let cli = crate::command::cli_command();
        let mut checked = 0usize;

        fn walk(
            path: &[&str],
            cmd: &ClapCommand,
            rendered: &[(String, String)],
            checked: &mut usize,
        ) {
            let section = section_id(path);
            let prefix = format!("{section}-");
            // A row of THIS section, not of a nested one: `cli-config--list-…`
            // starts with `cli-config-` too, and the `-` left over after the
            // prefix is what tells them apart.
            let mut shown: Vec<String> = rendered
                .iter()
                .filter(|(id, _)| {
                    id.strip_prefix(&prefix)
                        .is_some_and(|r| !r.starts_with('-'))
                })
                .map(|(_, name)| name.clone())
                .collect();
            let mut want: Vec<String> = documented_args(cmd)
                .iter()
                .map(|a| esc(&arg_name(a)))
                .collect();
            shown.sort();
            want.sort();
            assert_eq!(
                shown,
                want,
                "`tasqx {}`: the parameter rows on the page and clap's arguments disagree",
                path.join(" ")
            );
            *checked += want.len();
            for sub in cmd.get_subcommands() {
                let mut deeper: Vec<&str> = path.to_vec();
                deeper.push(sub.get_name());
                walk(&deeper, sub, rendered, checked);
            }
        }

        for sub in cli.get_subcommands() {
            walk(&[sub.get_name()], sub, &rendered, &mut checked);
        }
        // Floor: the walk has to find real arguments, or it is comparing empty
        // sets and passing for it.
        assert!(checked > 100, "only {checked} parameters compared");
    }

    /// Every method link lands on a method the core has, and every see-also
    /// link on a verb the CLI has.
    #[test]
    fn the_cross_links_name_real_methods_and_real_verbs() {
        let page = page();
        let mut methods = 0usize;
        let mut verbs = 0usize;
        for chunk in page.split("href=\"#api-").skip(1) {
            let method = chunk.split('"').next().unwrap_or_default();
            methods += 1;
            assert!(
                tasqx_core::PARAMS.iter().any(|(m, _, _)| *m == method),
                "the page links to `#api-{method}`, which is not a JSON API method"
            );
        }
        let known: Vec<&str> = COMMAND_REF.iter().map(|d| d.verb).collect();
        for chunk in page.split("href=\"#cli-").skip(1) {
            let verb = chunk.split('"').next().unwrap_or_default();
            // Topic headings are `cli-topic-…` and are not cross-references to
            // a verb; every other `#cli-` link is one.
            if verb.starts_with("topic-") {
                continue;
            }
            verbs += 1;
            assert!(
                known.contains(&verb),
                "the page links to `#cli-{verb}`, which is not a verb"
            );
        }
        assert!(verbs > 40, "only {verbs} verb links found");
        assert!(methods + verbs > 40, "the cross-link scan found nothing");
    }

    /// A method links to its own anchor on the JSON API page once that page
    /// carries one, and to the page itself until then — never to nothing.
    #[test]
    fn a_method_links_as_deep_as_the_api_page_allows() {
        assert_eq!(api_anchor_in("<h2>The JSON API</h2>", "task.add"), "api");
        assert_eq!(
            api_anchor_in("<section id=\"api-task.add\">", "task.add"),
            "api-task.add"
        );
        // And the cell around it: a real method is a link, a transport that
        // frames its own calls is words.
        assert!(method_cell("task.add").contains("<code>task.add</code></a>"));
        assert_eq!(method_cell("(any)"), "(any)");
        assert!(method_cell("task.list + push").contains("<code>push</code>"));
    }

    /// The wide row is the only difference between a verb's block and the
    /// two-column template every other reference section uses.
    #[test]
    fn a_verb_block_with_no_wide_row_is_the_reference_template() {
        assert_eq!(
            verb_block("cli-x", "x", "<p>left</p>", "<p>right</p>", ""),
            ref_section("cli-x", "x", "<p>left</p>", "<p>right</p>"),
        );
        let wide = verb_block("cli-x", "x", "<p>left</p>", "<p>right</p>", "<p>wide</p>");
        assert!(
            wide.contains("<div class=\"ref-wide\" style=\"grid-column: 1 / -1;\"><p>wide</p></div></section>"),
            "the wide row must be the section's last child: {wide}"
        );
    }

    /// The command over a screen is the manifest's, verbatim.
    #[test]
    fn a_screens_command_is_read_from_the_manifest() {
        assert_eq!(screen_cmd("list"), "tasqx list");
        assert_eq!(screen_cmd("show"), "tasqx show 51");
        assert!(
            screen_cmd("api-task-list").starts_with("echo '{\"tasqx\":\"1\""),
            "a row with stdin shows how the envelope gets in: {}",
            screen_cmd("api-task-list")
        );
    }

    /// Backticks become code spans, and the text is escaped first.
    #[test]
    fn notes_render_their_backtick_spans() {
        assert_eq!(md("a `b` c"), "a <code>b</code> c");
        assert_eq!(md("<b> & `<i>`"), "&lt;b&gt; &amp; <code>&lt;i&gt;</code>");
    }

    /// A flag is a flag, a closed vocabulary is the vocabulary, and a value is
    /// its shape.
    #[test]
    fn a_parameters_badge_says_what_it_takes() {
        let cli = crate::command::cli_command();
        let find = |verb: &str, name: &str| -> (String, String) {
            let sub = cli
                .get_subcommands()
                .find(|c| c.get_name() == verb)
                .expect("a verb");
            let a = documented_args(sub)
                .into_iter()
                .find(|a| arg_name(a) == name)
                .unwrap_or_else(|| panic!("{verb} has no argument {name}"));
            (arg_name(a), arg_ty(a))
        };
        assert_eq!(find("done", "--force"), ("--force".into(), "flag".into()));
        assert_eq!(
            find("add", "-p, --priority"),
            ("-p, --priority".into(), "H|M|L".into())
        );
        assert_eq!(find("show", "<ref>"), ("<ref>".into(), "string".into()));
        assert_eq!(find("tag", "<tag…>"), ("<tag…>".into(), "strings".into()));
        assert_eq!(find("agenda", "--days"), ("--days".into(), "<days>".into()));
    }

    /// Every verb either shows captured output or states why it cannot.
    #[test]
    fn every_verb_either_shows_output_or_says_why() {
        let page = page();
        for d in COMMAND_REF {
            let shown = EXAMPLE_SCREENS.iter().any(|(v, ..)| *v == d.verb);
            let excused = NO_SCREEN.iter().any(|(v, _)| *v == d.verb);
            assert!(
                shown != excused,
                "`{}` is {} — every verb belongs to exactly one of EXAMPLE_SCREENS and \
                 NO_SCREEN",
                d.verb,
                if shown {
                    "in both tables"
                } else {
                    "in neither table: capture a screen for it, or say why there is none"
                }
            );
        }
        for (verb, reason) in NO_SCREEN {
            assert!(
                page.contains(&md(reason)),
                "`{verb}`'s reason for having no screen never reaches the page"
            );
        }
    }

    /// Nothing on the page is an empty shell: every verb shows its usage, and
    /// well over the dozen the task asked for show real output.
    #[test]
    fn every_verb_shows_its_usage_and_most_show_a_screen() {
        let page = page();
        for d in COMMAND_REF {
            assert!(
                page.contains(&esc(d.usage)),
                "`{}`'s usage line never reaches the page",
                d.verb
            );
        }
        let mut with_output: Vec<&str> = EXAMPLE_SCREENS.iter().map(|(v, ..)| *v).collect();
        with_output.sort_unstable();
        with_output.dedup();
        assert!(
            with_output.len() >= 30,
            "only {} verbs show captured output: {with_output:?}",
            with_output.len()
        );
    }
}
