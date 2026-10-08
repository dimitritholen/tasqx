//! `tasqx manual` — the complete guide, in the terminal, themed and navigable.
//!
//! Renders from [`crate::cmddoc::COMMAND_REF`] (per-command reference) plus
//! twelve concept topics, each read out of the `docs/wiki` pages the HTML
//! guide renders too ([`topic_source`]). Navigation is the table of contents plus
//! `tasqx manual <name>`; there is no pager (kept dependency-free and portable).
//! For the exhaustive browser guide, `tasqx docs`.
//!
//! The page follows `docs/maintainers/terminal-style.md`, plus six conventions of its
//! own, because it is the one screen that is mostly prose:
//!
//! - Prose keeps a measure (`MEASURE`) rather than running to the edge.
//! - Two levels of heading: the page's title in `header`, a section in
//!   `table.label` and capitals, and no row painted like either. The two
//!   levels differ in rendered bytes in every built-in theme.
//! - A topic's table stacks each definition under its term when the
//!   terminal is too narrow for two readable columns.
//! - What a reader copies (an example, a code line, inline code in
//!   backticks) is never wrapped or cut: it overflows instead.
//! - An example's notes sit beside their commands, or under them, decided
//!   once per block.
//! - A name that is a topic and a command opens both pages, set apart by two
//!   blank lines.

use crate::cmddoc::{self, CmdDoc, Topic};
use crate::columns::{self, Column, GAP};
use crate::render::{pad, truncate, width, wrap_hanging, wrap_pieces};
use crate::theme::Ctx;

/// Where the text under a heading starts.
const INDENT: usize = 2;
/// The widest a line of prose is drawn, indent included. On a 140-column
/// terminal a paragraph run to the edge is a line the eye loses on its way
/// back; the source was typed at about this width for the same reason.
/// Tables of contents and code are not prose and do not take it.
const MEASURE: usize = 72;
/// Below this a TOC description or a definition stops saying anything.
const TEXT_FLOOR: usize = 12;
/// How far a wrapped usage line, or an example's note that had to move under
/// its command, sits in from the line it continues.
const HANG: usize = 4;
/// A definition column narrower than this stacks a topic's table: each
/// definition goes under its term. Narrower, two columns orphan a word ("edit
/// the startup" over "file" at 60 columns) or run past their floor at 40.
const STACK_BELOW: usize = 24;

pub fn render(ctx: &Ctx, arg: Option<&str>) -> Result<String, String> {
    let Some(name) = arg else {
        return Ok(toc(ctx));
    };
    // A name can be a topic AND a command (`projects`, `daemon`). Both pages
    // print, topic first. The command used to win outright, so the topic page
    // the table of contents listed under that name could not be opened.
    let topic = Topic::ALL.iter().find(|t| t.slug() == name).copied();
    match (topic, cmddoc::find(name)) {
        (None, None) => Err(unknown(name)),
        (Some(t), None) => Ok(topic_section(ctx, t)),
        (None, Some(d)) => Ok(command_section(ctx, d)),
        // Two pages, set apart by two blank lines where a page's own sections
        // take one. The command's See also leaves out what the topic's
        // Commands line has just offered (house style rule 11).
        (Some(t), Some(d)) => Ok(format!(
            "{}\n\n{}",
            topic_section(ctx, t),
            command_section_after(ctx, d, &topic_verbs(t))
        )),
    }
}

/// A page's title: `header` (house style rule 12), whose bold is what
/// survives `NO_COLOR` and `mono`.
fn title(ctx: &Ctx, text: &str) -> String {
    format!("{}\n", ctx.paint("header", text))
}

/// A section heading: `table.label`, in capitals, flush over what it heads.
/// It is structure, so it recedes (house style rule 12's reason), and the
/// page's title keeps the only emphasis. The two differ in the bytes drawn in
/// every built-in theme and under `NO_COLOR`: bold against `table.label`'s
/// grey, its dim in `mono`, and plain capitals without colour. `accent` was
/// tried first; `mono` draws it as plain bold, the very bytes of the title.
fn heading(ctx: &Ctx, text: &str) -> String {
    format!("{}\n", ctx.paint("table.label", text))
}

/// Prose split where it may wrap: at a space, but never inside a backtick
/// span. Inline code is copied like any other code, and a span split across
/// two lines pastes as two fragments. A span wider than the line stands alone
/// and overflows rather than being cut.
fn prose_pieces(text: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    let mut open = false;
    for word in text.split_whitespace() {
        match pieces.last_mut() {
            Some(last) if open => {
                last.push(' ');
                last.push_str(word);
            }
            _ => pieces.push(word.to_string()),
        }
        if word.matches('`').count() % 2 == 1 {
            open = !open;
        }
    }
    pieces
}

/// `text` wrapped at words to `max` cells, inline code kept whole.
fn wrap_prose(text: &str, max: usize) -> Vec<String> {
    let pieces = prose_pieces(text);
    wrap_pieces(pieces.iter().map(|p| (" ", p.as_str())), max)
}

/// `text` as a paragraph `indent` cells in, wrapped at words to the measure.
fn prose(ctx: &Ctx, indent: usize, text: &str) -> String {
    let lead = " ".repeat(indent);
    wrap_prose(text, ctx.cols.min(MEASURE).saturating_sub(indent))
        .iter()
        .map(|line| format!("{lead}{line}\n"))
        .collect()
}

fn toc(ctx: &Ctx) -> String {
    let topics: Vec<(&str, &str)> = Topic::ALL.iter().map(|t| (t.slug(), t.title())).collect();
    let commands: Vec<(&str, &str)> = cmddoc::COMMAND_REF
        .iter()
        .map(|d| (d.verb, d.summary))
        .collect();
    let mut s = title(ctx, "TASQX MANUAL");
    for (group, rows) in [("TOPICS", &topics), ("COMMANDS", &commands)] {
        // Each group is fitted on its own. The name is what `tasqx manual
        // <name>` takes, so it never gives way: cut, it names a different
        // page. The description takes an ellipsis before its row would wrap
        // (D120(c)), and only then (rule 2): fitted across both groups,
        // `getting-started` widened the command column and cut summaries a
        // 60-column terminal had room for. The whole of a description is one
        // `tasqx manual <name>` away.
        //
        // No paint on a row: `mono` draws `accent` as plain bold, so names in
        // `accent` weighed as much as the heading over them.
        let name_w = rows.iter().map(|(n, _)| width(n)).max().unwrap_or(0);
        let text_w = rows.iter().map(|(_, d)| width(d)).max().unwrap_or(0);
        let w = columns::fit(
            &[Column::fixed(name_w), Column::shrinks(text_w, TEXT_FLOOR)],
            ctx.cols.saturating_sub(INDENT),
        );
        s.push('\n');
        s.push_str(&heading(ctx, group));
        for (name, text) in rows.iter() {
            s.push_str(&format!(
                "{}{}{}{}\n",
                " ".repeat(INDENT),
                pad(name, w[0]),
                " ".repeat(GAP),
                truncate_prose(text, w[1], ctx.caps.unicode),
            ));
        }
    }
    // The footer: a label and the command it names. The command is copied, so
    // it stays whole; where both do not fit on one line, each command goes
    // under its label.
    let footer = [
        ("Jump to a topic or command:", "tasqx manual <name>"),
        ("Full browser guide:", "tasqx docs"),
    ];
    let label_w = footer.iter().map(|(l, _)| width(l)).max().unwrap_or(0) + GAP;
    let lead = width(ctx.arrow()) + 1;
    let side_by_side = footer
        .iter()
        .all(|(_, cmd)| lead + label_w + width(cmd) <= ctx.cols);
    s.push('\n');
    for (label, cmd) in footer {
        if side_by_side {
            s.push_str(&format!("{} {}{cmd}\n", ctx.arrow(), pad(label, label_w)));
        } else {
            s.push_str(&format!(
                "{} {label}\n{}{cmd}\n",
                ctx.arrow(),
                " ".repeat(lead)
            ));
        }
    }
    s
}

/// [`truncate`], but never inside a backtick span: a cut that would land in
/// one moves back to before the span opens, so a reader never sees half of
/// something to type, or a lone backtick.
fn truncate_prose(text: &str, max: usize, unicode: bool) -> String {
    let cut = truncate(text, max, unicode);
    let ellipsis = if unicode { "…" } else { "..." };
    let Some(head) = cut.strip_suffix(ellipsis).filter(|_| cut != text) else {
        return cut;
    };
    match head.rfind('`') {
        Some(open) if head.matches('`').count() % 2 == 1 => {
            format!("{}{ellipsis}", head[..open].trim_end())
        }
        _ => cut,
    }
}

fn command_section(ctx: &Ctx, d: &CmdDoc) -> String {
    command_section_after(ctx, d, &[])
}

/// A command's page under another page that has already offered `offered`,
/// so that its See also does not offer them a second time.
fn command_section_after(ctx: &Ctx, d: &CmdDoc, offered: &[&str]) -> String {
    let mut s = ctx.paint("header", &format!("tasqx {}", d.verb));
    if !d.aliases.is_empty() {
        let aliases = format!("  (aliases: {})", d.aliases.join(", "));
        s.push_str(&ctx.paint("muted", &aliases));
    }
    s.push('\n');
    // `dashboard` answers from four methods, which ran past a narrow terminal.
    // They break between methods, each continuation starting `+ `, rather
    // than being cut: a cut list hides a method the page exists to name.
    let lead = "API method: ";
    let methods: Vec<String> = d
        .method
        .split(" + ")
        .enumerate()
        .map(|(i, m)| {
            if i == 0 {
                m.to_string()
            } else {
                format!("+ {m}")
            }
        })
        .collect();
    let pieces = methods.iter().map(|m| (" ", m.as_str()));
    for (i, line) in wrap_pieces(pieces, ctx.cols.saturating_sub(width(lead)))
        .iter()
        .enumerate()
    {
        let head = if i == 0 {
            lead.to_string()
        } else {
            " ".repeat(width(lead))
        };
        s.push_str(&ctx.paint("muted", &format!("{head}{line}")));
        s.push('\n');
    }
    s.push('\n');
    s.push_str(&prose(ctx, 0, d.summary));

    s.push('\n');
    s.push_str(&heading(ctx, "USAGE"));
    s.push_str(&usage(ctx, d.usage));

    s.push('\n');
    s.push_str(&heading(ctx, "EXAMPLES"));
    let lead = " ".repeat(INDENT);
    let marker = format!("{} ", ctx.mid());
    // The command is copied, so it prints whole whatever the width. The notes
    // go beside their commands when every one of them fits there, and under
    // every command when one does not: decided once for the block, because a
    // block with a note on either side reads as two layouts.
    let beside = d.examples.iter().all(|e| {
        e.note
            .filter(|n| !n.is_empty())
            .is_none_or(|n| INDENT + width(e.cmd) + HANG + width(&marker) + width(n) <= ctx.cols)
    });
    for e in d.examples {
        s.push_str(&format!("{lead}{}", e.cmd));
        let Some(note) = e.note.filter(|n| !n.is_empty()) else {
            s.push('\n');
            continue;
        };
        if beside {
            let gap = " ".repeat(HANG);
            s.push_str(&ctx.paint("muted", &format!("{gap}{marker}{note}")));
            s.push('\n');
            continue;
        }
        // Under the command, with a wrapped note hung under its own text
        // rather than under the marker.
        s.push('\n');
        let under = " ".repeat(INDENT + HANG);
        let width_left = ctx.cols.saturating_sub(INDENT + HANG + width(&marker));
        for (i, line) in wrap_prose(note, width_left).iter().enumerate() {
            let head = if i == 0 {
                marker.clone()
            } else {
                " ".repeat(width(&marker))
            };
            s.push_str(&under);
            s.push_str(&ctx.paint("muted", &format!("{head}{line}")));
            s.push('\n');
        }
    }

    // The notes are the page's prose, so they read at the foreground and
    // under a heading of their own; flush under EXAMPLES they read as more
    // examples. One paragraph each: wrapped, two notes set flush read as one.
    if !d.notes.is_empty() {
        s.push('\n');
        s.push_str(&heading(ctx, "NOTES"));
        for (i, note) in d.notes.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.push_str(&prose(ctx, INDENT, note));
        }
    }
    let see_also: Vec<&str> = d
        .see_also
        .iter()
        .copied()
        .filter(|name| !offered.contains(name))
        .collect();
    if !see_also.is_empty() {
        s.push('\n');
        s.push_str(&follow_on(ctx, "See also:", &see_also));
    }
    s
}

/// A usage synopsis, whole on one line when it fits, and otherwise broken
/// between arguments with the rest hung under the first line.
fn usage(ctx: &Ctx, synopsis: &str) -> String {
    let lead = " ".repeat(INDENT);
    if INDENT + width(synopsis) <= ctx.cols {
        return format!("{lead}{synopsis}\n");
    }
    let hang = " ".repeat(INDENT + HANG);
    wrap_hanging(
        usage_pieces(synopsis),
        ctx.cols.saturating_sub(INDENT),
        ctx.cols.saturating_sub(INDENT + HANG),
    )
    .iter()
    .enumerate()
    .map(|(i, line)| format!("{}{line}\n", if i == 0 { &lead } else { &hang }))
    .collect()
}

/// Where a usage synopsis may break: at a space, or before a `|`, but never
/// inside an innermost bracket group, so `[--project p]` and `<title…>` stay
/// whole. A group holding other groups (`<add … |search …>`) may break inside,
/// between its alternatives and their arguments, or `memory`'s synopsis would
/// be one unbreakable piece of 260 cells.
///
/// Each piece carries what joins it to the one before, for
/// [`wrap_pieces`]: a space, or nothing before a `|`, which stays on the
/// piece it introduces.
fn usage_pieces(synopsis: &str) -> Vec<(&'static str, &str)> {
    let bytes = synopsis.as_bytes();
    // The innermost group each byte sits in, and whether each group holds
    // another. Bytes, because every delimiter here is ASCII; a multi-byte
    // `…` never matches one.
    let mut compound: Vec<bool> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut inside: Vec<Option<usize>> = Vec::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'[' | b'<' => {
                if let Some(&outer) = open.last() {
                    compound[outer] = true;
                }
                inside.push(open.last().copied());
                open.push(compound.len());
                compound.push(false);
            }
            b']' | b'>' => {
                open.pop();
                inside.push(open.last().copied());
            }
            _ => inside.push(open.last().copied()),
        }
    }
    let may_break = |i: usize| inside[i].is_none_or(|g| compound[g]);

    let mut pieces = Vec::new();
    let (mut start, mut joiner) = (0, "");
    for (i, &b) in bytes.iter().enumerate() {
        if b == b' ' && may_break(i) {
            if i > start {
                pieces.push((joiner, &synopsis[start..i]));
            }
            (start, joiner) = (i + 1, " ");
        } else if b == b'|' && may_break(i) && i > start {
            pieces.push((joiner, &synopsis[start..i]));
            (start, joiner) = (i, "");
        }
    }
    if start < synopsis.len() {
        pieces.push((joiner, &synopsis[start..]));
    }
    pieces
}

/// `▸ See also: a · b` under a command, `▸ Commands: a · b` under a topic:
/// the pages to open next, unpainted as the table of contents' names are (a
/// row carries no paint of its own), and wrapped under their label.
fn follow_on(ctx: &Ctx, label: &str, names: &[&str]) -> String {
    let lead = format!("{} {label} ", ctx.arrow());
    let hang = width(&lead);
    let sep = format!(" {} ", ctx.mid());
    let pieces = names.iter().map(|name| (sep.as_str(), *name));
    wrap_pieces(pieces, ctx.cols.min(MEASURE).saturating_sub(hang))
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let head = if i == 0 {
                lead.clone()
            } else {
                " ".repeat(hang)
            };
            format!("{head}{line}\n")
        })
        .collect()
}

fn topic_section(ctx: &Ctx, t: Topic) -> String {
    // In the topic's own case: it names the page, and a section heading is
    // the line in capitals.
    let mut s = title(ctx, t.title());
    s.push('\n');
    s.push_str(&body(ctx, topic_body(t)));
    let verbs = topic_verbs(t);
    if !verbs.is_empty() {
        s.push('\n');
        s.push_str(&follow_on(ctx, "Commands:", &verbs));
    }
    s
}

/// A topic's body, fitted to the terminal rather than to the width its source
/// happened to be typed at. [`shapes`] writes it in four shapes, told apart by
/// how a line starts:
///
/// - **prose**, at the margin. Consecutive lines are one paragraph, reflowed
///   to the measure by `wrap_prose`, which keeps inline code whole.
/// - a **heading**: a margin line with no lowercase letter in it.
/// - a **row**, `  term\tdefinition`. Consecutive rows are one two-column
///   table, fitted by `columns::fit`, with the definition wrapped in its own
///   column: it is prose. A row with no term, `  \ttext`, adds a line to the
///   row above, kept as written.
/// - **code**: any other indented line, printed as written. A reader copies
///   it, so it is never wrapped or cut.
///
/// A blank line separates blocks and is kept.
fn body(ctx: &Ctx, src: &str) -> String {
    let mut out = String::new();
    let mut para: Vec<&str> = Vec::new();
    let mut rows: Vec<(&str, &str)> = Vec::new();
    for line in src.lines() {
        let row = line.strip_prefix("  ").and_then(|l| l.split_once('\t'));
        if row.is_none() && !rows.is_empty() {
            out.push_str(&table(ctx, &rows));
            rows.clear();
        }
        let at_margin = !line.is_empty() && !line.starts_with(' ');
        let is_heading = at_margin
            && line.chars().any(char::is_alphabetic)
            && !line.chars().any(char::is_lowercase);
        if at_margin && !is_heading {
            para.push(line);
            continue;
        }
        if !para.is_empty() {
            out.push_str(&prose(ctx, 0, &para.join(" ")));
            para.clear();
        }
        match row {
            Some((term, text)) => rows.push((term.trim(), text.trim())),
            None if is_heading => out.push_str(&heading(ctx, line)),
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    if !rows.is_empty() {
        out.push_str(&table(ctx, &rows));
    }
    if !para.is_empty() {
        out.push_str(&prose(ctx, 0, &para.join(" ")));
    }
    out
}

/// A topic's two-column table: the term whole (it is what gets typed), the
/// definition wrapped in whatever `columns::fit` leaves it, or stacked under
/// its term where that is narrower than `STACK_BELOW`, narrower than a piece
/// of a definition that cannot break, or too narrow for a kept code line
/// that would fit stacked.
fn table(ctx: &Ctx, rows: &[(&str, &str)]) -> String {
    let term_w = rows.iter().map(|(t, _)| width(t)).max().unwrap_or(0);
    let text_w = rows.iter().map(|(_, d)| width(d)).max().unwrap_or(0);
    let w = columns::fit(
        &[Column::fixed(term_w), Column::shrinks(text_w, TEXT_FLOOR)],
        ctx.cols.min(MEASURE).saturating_sub(INDENT),
    );
    let lead = " ".repeat(INDENT);
    let mut out = String::new();
    // A piece that cannot break (a path, a variable) and is wider than the
    // column would run past the terminal beside its term, so it stacks too.
    let widest_piece = rows
        .iter()
        .filter(|(term, _)| !term.is_empty())
        .flat_map(|(_, text)| prose_pieces(text))
        .map(|piece| width(&piece))
        .max()
        .unwrap_or(0);
    // A code line continuing a row is kept as written. Where it does not fit
    // beside its term but would fit under it, the table stacks: left in the
    // definition column, it ran past a terminal that could have held it.
    let kept_fits_only_stacked =
        rows.iter()
            .filter(|(term, _)| term.is_empty())
            .any(|(_, text)| {
                INDENT + w[0] + GAP + width(text) > ctx.cols
                    && INDENT + HANG + width(text) <= ctx.cols
            });
    if w[1] < STACK_BELOW.min(text_w) || widest_piece > w[1] || kept_fits_only_stacked {
        let under = " ".repeat(INDENT + HANG);
        let width_left = ctx.cols.min(MEASURE).saturating_sub(INDENT + HANG);
        for (term, text) in rows {
            if term.is_empty() {
                out.push_str(&format!("{under}{text}\n"));
                continue;
            }
            out.push_str(&format!("{lead}{term}\n"));
            for line in wrap_prose(text, width_left) {
                out.push_str(&format!("{under}{line}\n"));
            }
        }
        return out;
    }
    let hang = " ".repeat(INDENT + w[0] + GAP);
    for (term, text) in rows {
        if term.is_empty() {
            out.push_str(&format!("{hang}{text}\n"));
            continue;
        }
        for (i, line) in wrap_prose(text, w[1]).iter().enumerate() {
            if i == 0 {
                let gap = " ".repeat(GAP);
                out.push_str(&format!("{lead}{}{gap}{line}\n", pad(term, w[0])));
            } else {
                out.push_str(&format!("{hang}{line}\n"));
            }
        }
    }
    out
}

/// The commands a topic's page offers next.
fn topic_verbs(t: Topic) -> Vec<&'static str> {
    cmddoc::COMMAND_REF
        .iter()
        .filter(|d| d.topic == t)
        .map(|d| d.verb)
        .collect()
}

fn unknown(name: &str) -> String {
    let mut valid: Vec<String> = Topic::ALL.iter().map(|t| t.slug().to_string()).collect();
    valid.extend(cmddoc::COMMAND_REF.iter().map(|d| d.verb.to_string()));
    format!(
        "no manual page for {name:?}. Try one of: {}",
        valid.join(", ")
    )
}

/// Where each topic's text lives: `(wiki page, sections)`, a section named by
/// its `## ` heading and `""` standing for the page's opening above the first
/// one. No sections named means the whole page.
///
/// The wiki is the one copy of this prose: the site renders the same files,
/// so the terminal and the browser cannot tell a reader two different things.
fn topic_source(t: Topic) -> &'static [(&'static str, &'static [&'static str])] {
    match t {
        Topic::GettingStarted => &[(
            "Getting-Started.md",
            &[
                "",
                "The whole loop is four commands",
                "Getting help",
                "Where your data lives",
            ],
        )],
        Topic::Projects => &[("Projects.md", &[])],
        Topic::Capturing => &[(
            "Adding-and-Editing-Tasks.md",
            &["", "tasqx add", "tasqx modify", "What a write prints"],
        )],
        Topic::Dates => &[
            (
                "Dates-Reminders-and-Recurrence.md",
                &["", "Writing a date", "The four date fields", "Recurrence"],
            ),
            ("Finding-Tasks.md", &["tasqx agenda"]),
        ],
        Topic::Filters => &[("Finding-Tasks.md", &["The filter language"])],
        Topic::Screens => &[
            (
                "Dashboard-and-Live-View.md",
                &[
                    "",
                    "Screens and pipes",
                    "tasqx dashboard",
                    "tasqx watch",
                    "tasqx board",
                ],
            ),
            ("Working-on-Tasks.md", &["tasqx pick"]),
        ],
        Topic::Reminders => &[("Dates-Reminders-and-Recurrence.md", &["Reminders"])],
        Topic::Reports => &[
            ("Reports-and-Charts.md", &[]),
            ("Settings-and-Themes.md", &["tasqx theme"]),
        ],
        Topic::Daemon => &[
            ("AI-Agents-and-Automation.md", &["tasqx daemon"]),
            ("Dashboard-and-Live-View.md", &["The daemon", "tasqx watch"]),
        ],
        Topic::Automation => &[(
            "AI-Agents-and-Automation.md",
            &[
                "",
                "tasqx setup",
                "tasqx mcp",
                "tasqx api",
                "One rule for anything automated",
            ],
        )],
        Topic::JsonApi => &[(
            "AI-Agents-and-Automation.md",
            &["tasqx api", "tasqx tokens"],
        )],
        Topic::Completion => &[("Shell-Completion.md", &[])],
    }
}

/// Each topic's text, in the four shapes [`body`] reads — prose at the
/// margin, a heading in capitals, `  term\tdefinition` rows, and indented
/// code — read out of the wiki pages [`topic_source`] names. Parsed once.
fn topic_body(t: Topic) -> &'static str {
    static BODIES: std::sync::LazyLock<Vec<String>> =
        std::sync::LazyLock::new(|| Topic::ALL.iter().map(|t| read_topic(*t)).collect());
    let i = Topic::ALL
        .iter()
        .position(|x| *x == t)
        .expect("every topic is in Topic::ALL");
    &BODIES[i]
}

fn read_topic(t: Topic) -> String {
    let mut out = String::new();
    for (file, sections) in topic_source(t) {
        let src = crate::docs::wiki_source(file);
        out.push_str(&shapes(&wiki_sections(file, src, sections)));
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// The named sections of a wiki page, whole: each from its `## ` heading to
/// the next one. Panics on a name the page does not have, which is a typo in
/// [`topic_source`] or a heading renamed under it.
fn wiki_sections(file: &str, src: &str, wanted: &[&str]) -> String {
    // `(heading, text)`, the opening first under `""`. A `#` inside a fence
    // is code, not a heading.
    let mut parts: Vec<(String, String)> = vec![(String::new(), String::new())];
    let mut fenced = false;
    for line in src.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
        }
        if !fenced && line.starts_with("# ") {
            continue;
        }
        if !fenced {
            if let Some(h) = line.strip_prefix("## ") {
                parts.push((h.trim().to_string(), String::new()));
            }
        }
        let text = &mut parts.last_mut().expect("never empty").1;
        text.push_str(line);
        text.push('\n');
    }
    if wanted.is_empty() {
        return parts.into_iter().map(|(_, text)| text).collect();
    }
    wanted
        .iter()
        .map(|w| {
            parts
                .iter()
                .find(|(h, _)| h == w)
                .map(|(_, text)| text.clone())
                .unwrap_or_else(|| panic!("docs/wiki/{file} has no section `## {w}`"))
        })
        .collect()
}

/// Markdown, rewritten into [`body`]'s four shapes. A heading is its text in
/// capitals; a paragraph is one line at the margin, its inline code kept in
/// backticks; a table row is `term\tdefinition` (a term that is one code span
/// loses its backticks, a third column joins the definition after an em
/// dash); a list item is a row under `-`; a fence is code, its `$ ` prompts
/// dropped as everywhere else in the manual. Links keep their text.
fn shapes(md: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut lines: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut cells: Vec<String> = Vec::new();
    let mut code: Option<String> = None;
    let mut in_head = false;
    let mut items: Vec<Option<u64>> = Vec::new();
    for ev in Parser::new_ext(md, Options::ENABLE_TABLES) {
        if let Some(buf) = code.as_mut() {
            match ev {
                Event::Text(t) => buf.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    for line in buf.trim_end_matches('\n').lines() {
                        let line = line
                            .strip_prefix("$ ")
                            .unwrap_or(line)
                            .replace('\t', "    ");
                        lines.push(if line.is_empty() {
                            line
                        } else {
                            format!("  {line}")
                        });
                    }
                    lines.push(String::new());
                    code = None;
                }
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Text(t) => text.push_str(&t),
            Event::Code(c) => {
                text.push('`');
                text.push_str(&c);
                text.push('`');
            }
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            Event::Start(Tag::CodeBlock(_)) => code = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => {
                lines.push(text.replace('`', "").trim().to_uppercase());
                lines.push(String::new());
                text.clear();
            }
            Event::End(TagEnd::Paragraph) if items.is_empty() => {
                lines.push(text.trim().to_string());
                lines.push(String::new());
                text.clear();
            }
            Event::End(TagEnd::Paragraph) => text.push(' '),
            Event::Start(Tag::TableHead) => in_head = true,
            Event::End(TagEnd::TableHead) => {
                in_head = false;
                cells.clear();
            }
            Event::End(TagEnd::TableCell) => {
                cells.push(text.trim().to_string());
                text.clear();
            }
            Event::End(TagEnd::TableRow) if !in_head => {
                let term = match cells.first().map(String::as_str).unwrap_or_default() {
                    t if t.len() > 1
                        && t.starts_with('`')
                        && t.ends_with('`')
                        && t.matches('`').count() == 2 =>
                    {
                        t[1..t.len() - 1].to_string()
                    }
                    t => t.to_string(),
                };
                lines.push(format!("  {term}\t{}", cells[1..].join(" — ")));
                cells.clear();
            }
            Event::End(TagEnd::Table) => lines.push(String::new()),
            Event::Start(Tag::List(first)) => {
                if !text.trim().is_empty() {
                    lines.push(format!("  -\t{}", text.trim()));
                }
                text.clear();
                items.push(first);
            }
            Event::End(TagEnd::Item) => {
                let mark = match items.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}.", *n - 1)
                    }
                    _ => "-".to_string(),
                };
                if !text.trim().is_empty() {
                    lines.push(format!("  {mark}\t{}", text.trim()));
                }
                text.clear();
            }
            Event::End(TagEnd::List(_)) => {
                items.pop();
                if items.is_empty() {
                    lines.push(String::new());
                }
            }
            _ => {}
        }
    }
    let mut out = lines.join("\n").trim_end().to_string();
    out.push_str("\n\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render;
    use crate::test_common::strip_sgr;
    use crate::theme::{default_theme, Caps, ColorDepth, Ctx};

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

    /// Plain bytes, Unicode glyphs, laid out for `cols`. What a UTF-8 terminal
    /// with its colour switched off would draw, so widths can be measured.
    fn at(cols: usize) -> Ctx {
        let caps = Caps {
            depth: ColorDepth::None,
            ansi: false,
            unicode: true,
        };
        Ctx::new(default_theme(), caps).with_cols(cols)
    }

    /// `NO_COLOR` on a real terminal: emphasis kept, every hue gone.
    fn no_color() -> Ctx {
        let caps = Caps {
            depth: ColorDepth::None,
            ansi: true,
            unicode: true,
        };
        Ctx::new(default_theme(), caps)
    }

    fn colour(cols: usize) -> Ctx {
        let caps = Caps {
            depth: ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        };
        Ctx::new(default_theme(), caps).with_cols(cols)
    }

    /// The table of contents, every topic and every command page, by name.
    /// Topics are rendered directly rather than through [`render`], so a topic
    /// whose name a command shadows is still judged.
    fn every_page(ctx: &Ctx) -> Vec<(String, String)> {
        let mut pages = vec![("toc".to_string(), toc(ctx))];
        for t in Topic::ALL {
            pages.push((format!("topic {}", t.slug()), topic_section(ctx, t)));
        }
        for d in cmddoc::COMMAND_REF {
            pages.push((format!("command {}", d.verb), command_section(ctx, d)));
        }
        pages
    }

    /// Lines a reader copies, read out of the SOURCE rather than out of the
    /// renderer's own idea of what is code, so a renderer that misfiles prose
    /// as code cannot excuse its own overflow: every example command, and every
    /// indented topic line that is not a table row (a row is `term\tdefinition`;
    /// a row with no term is a line kept as written).
    fn kept_row_lines() -> Vec<String> {
        let mut kept = Vec::new();
        for t in Topic::ALL {
            for line in topic_body(t).lines().filter(|l| l.starts_with("  ")) {
                if let Some((term, text)) = line.split_once('\t') {
                    if term.trim().is_empty() {
                        kept.push(text.trim().to_string());
                    }
                }
            }
        }
        kept
    }

    fn copied_lines() -> Vec<String> {
        let mut copied: Vec<String> = cmddoc::COMMAND_REF
            .iter()
            .flat_map(|d| d.examples.iter().map(|e| e.cmd.to_string()))
            .collect();
        // A usage piece that cannot break, `[--out PATH | --no-open |
        // --stdout]`, alone on its line.
        copied.extend(
            cmddoc::COMMAND_REF
                .iter()
                .flat_map(|d| usage_pieces(d.usage))
                .map(|(_, piece)| piece.to_string()),
        );
        for t in Topic::ALL {
            for line in topic_body(t).lines().filter(|l| l.starts_with("  ")) {
                match line.split_once('\t') {
                    None => copied.push(line.trim().to_string()),
                    Some((term, kept)) if term.trim().is_empty() => {
                        copied.push(kept.trim().to_string())
                    }
                    // A term is what gets typed, so `table` prints it whole
                    // on its own line where it is wider than the terminal.
                    Some((term, _)) => copied.push(term.trim().to_string()),
                }
            }
        }
        copied
    }

    /// A command page's lines after its EXAMPLES heading: where its notes
    /// are, and where no summary or usage line can be mistaken for one.
    fn below_examples(page: &str) -> Vec<&str> {
        page.lines()
            .skip_while(|l| strip_sgr(l).trim() != "EXAMPLES")
            .skip(1)
            .collect()
    }

    /// A line that is exactly one inline code span, and at most the
    /// punctuation that opens and closes it: `` `tasqx list --sort due …`. ``,
    /// `` (`17 open · 1 active`). ``. That is code a reader copies, so when it
    /// is wider than the line it stands alone and overflows rather than being
    /// split. Nothing else is exempt: not a long word, not a line with a span
    /// in it.
    fn a_whole_code_span(line: &str) -> bool {
        let t = line
            .trim()
            .trim_start_matches(|c: char| c.is_ascii_punctuation() && c != '`');
        let Some(rest) = t.strip_prefix('`') else {
            return false;
        };
        let Some(close) = rest.find('`') else {
            return false;
        };
        close > 0 && rest[close + 1..].chars().all(|c| c.is_ascii_punctuation())
    }

    /// The lines under `heading` on `page`, up to the next blank line.
    fn block_under<'a>(page: &'a str, heading: &str) -> Vec<&'a str> {
        page.lines()
            .skip_while(|l| l.trim() != heading)
            .skip(1)
            .take_while(|l| !l.trim().is_empty())
            .collect()
    }

    /// Each group of the table of contents is a table of its own: a topic's
    /// title, or a command's summary, starts at one column across its group,
    /// whatever the longest name happens to be.
    ///
    /// Rewritten twice. The original, `both_toc_columns_start_their_text_at_one_column`,
    /// caught the literal `{:<9}` that pushed `completions` out of line and an
    /// unpadded TOPICS column, and asserted each block agreed with itself;
    /// that is the intent kept here. The first rewrite demanded one column
    /// across both groups, which made `getting-started` widen the command
    /// column and cut summaries a 60-column terminal could hold (rule 2).
    #[test]
    fn each_toc_group_is_a_table_whose_descriptions_share_one_column() {
        for cols in [60, 100, 140] {
            let s = toc(&at(cols));
            let rows = |from: &str, to: &str| -> Vec<String> {
                let rest = &s[s.find(from).expect("group heading") + from.len()..];
                let rest = rest.find(to).map_or(rest, |end| &rest[..end]);
                rest.lines()
                    .filter(|l| l.starts_with("  "))
                    .map(str::to_string)
                    .collect()
            };
            let topics = rows("TOPICS\n", "\nCOMMANDS");
            let commands = rows("COMMANDS\n", "\n\n");
            assert_eq!(topics.len(), Topic::ALL.len(), "every topic:\n{s}");
            assert_eq!(
                commands.len(),
                cmddoc::COMMAND_REF.len(),
                "every command:\n{s}"
            );
            for group in [&topics, &commands] {
                let text_x: Vec<usize> = group
                    .iter()
                    .map(|l| {
                        let name = l.split_whitespace().next().expect("a name");
                        let after = l.find(name).expect("name on its line") + name.len();
                        after + (l[after..].len() - l[after..].trim_start().len())
                    })
                    .collect();
                assert!(
                    text_x.windows(2).all(|w| w[0] == w[1]),
                    "descriptions start at {text_x:?}, not one column, at {cols}:\n{s}"
                );
            }
        }
    }

    /// House style rule 2: a TOC description is cut only where the terminal
    /// cannot hold it. Checked against the widest name in the row's own group:
    /// a cut the terminal had room for is a cut some other column caused.
    #[test]
    fn a_toc_description_is_cut_only_where_the_terminal_cannot_hold_it() {
        let groups: [Vec<(&str, &str)>; 2] = [
            Topic::ALL.iter().map(|t| (t.slug(), t.title())).collect(),
            cmddoc::COMMAND_REF
                .iter()
                .map(|d| (d.verb, d.summary))
                .collect(),
        ];
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            let s = toc(&at(cols));
            // In order, not by name: `projects` and `daemon` are in both groups.
            let mut rows = s.lines().filter(|l| l.starts_with("  "));
            for group in &groups {
                let name_w = group.iter().map(|(n, _)| render::width(n)).max().unwrap();
                for (name, text) in group {
                    let row = rows
                        .next()
                        .unwrap_or_else(|| panic!("no row for {name} at {cols}:\n{s}"));
                    assert_eq!(row.split_whitespace().next(), Some(*name), "order:\n{s}");
                    if row.ends_with('…') {
                        assert!(
                            2 + name_w + 2 + render::width(text) > cols,
                            "{name}'s description is cut at {cols} though its group has \
                             room for it: {row:?}"
                        );
                    }
                }
            }
        }
    }

    /// Every TOC row leads with something `tasqx manual` opens. The topics
    /// used to lead with an index number, which it refuses.
    #[test]
    fn every_toc_row_leads_with_a_name_the_manual_opens() {
        let s = toc(&plain());
        for row in s.lines().filter(|l| l.starts_with("  ")) {
            let name = row.split_whitespace().next().expect("a name");
            assert!(
                render(&plain(), Some(name)).is_ok(),
                "the TOC row {row:?} leads with {name:?}, which `tasqx manual` does not open"
            );
        }
    }

    /// Every entry the TOC lists opens the page it lists. `projects` and
    /// `daemon` are a topic AND a command, and the command used to win, so
    /// two topic pages the TOC advertised could not be reached at all.
    #[test]
    fn every_toc_entry_opens_the_page_it_lists() {
        for t in Topic::ALL {
            let page = render(&plain(), Some(t.slug())).unwrap();
            assert!(
                page.contains(t.title()),
                "`tasqx manual {}` does not open the {:?} topic:\n{page}",
                t.slug(),
                t.title()
            );
        }
        for d in cmddoc::COMMAND_REF {
            let page = render(&plain(), Some(d.verb)).unwrap();
            assert!(
                page.contains(&format!("tasqx {}", d.verb)) && page.contains(d.method),
                "`tasqx manual {}` does not open its command page:\n{page}",
                d.verb
            );
        }
    }

    /// House style rule 7: no rules. The TOC drew one under `TASQX MANUAL`
    /// and every topic drew one under its title.
    #[test]
    fn the_manual_draws_no_rules() {
        for ctx in [plain(), at(100)] {
            for (name, page) in every_page(&ctx) {
                for line in page.lines() {
                    let t = line.trim();
                    assert!(
                        !(t.chars().count() >= 3 && t.chars().all(|c| c == '─' || c == '-')),
                        "{name} draws a rule: {line:?}"
                    );
                }
            }
        }
    }

    /// House style rule 2, over the whole manual: nothing runs past the
    /// terminal, at any width from `Ctx::MIN_COLS` to `Ctx::MAX_COLS`, with
    /// two exemptions.
    ///
    /// The `kept` branch is unreachable while the renderer is right: `table`
    /// stacks precisely when a kept code line fits under its term and not
    /// beside it, so every overflow that survives is one no layout could have
    /// held. The branch is there for the drift that drops that rule, which is
    /// how it bit when `kept_fits_only_stacked` was added — twenty injections
    /// against correct code never reddened it, and that is the guard working,
    /// not a hole in it.
    /// What the renderer prints verbatim may overflow: an
    /// example command, a topic's code block or captured screen, a usage piece
    /// that cannot break, a line that is one whole inline code span. What the
    /// renderer PLACES may not: a row's kept continuation line overflows only
    /// where even the stacked indent could not hold it. Those are never
    /// wrapped or cut, since a command that has been cut is a different
    /// command (rule 2's "a number never gives way", for commands).
    ///
    /// Collects every violation before failing, so one red run names them all.
    #[test]
    fn every_page_fits_the_terminal_except_what_is_copied() {
        let copied = copied_lines();
        let kept_rows = kept_row_lines();
        let mut over = Vec::new();
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            for (name, page) in every_page(&at(cols)) {
                for line in page.lines() {
                    // What the renderer prints VERBATIM — an example, a
                    // topic's code block or captured screen, a usage piece
                    // that cannot break, a whole inline code span — may run
                    // past the terminal at any width: no layout choice of
                    // ours placed it, and a captured screen cannot be
                    // re-laid out at the reader's width at all.
                    //
                    // A line the renderer PLACES — a row's kept continuation
                    // — may overflow only where even the stacked indent could
                    // not hold it.
                    let verbatim =
                        copied.iter().any(|c| c == line.trim()) || a_whole_code_span(line);
                    let kept = kept_rows.iter().any(|c| c == line.trim());
                    let holdable = render::width(line.trim()) <= cols.saturating_sub(INDENT + HANG);
                    let exempt = if kept { !holdable } else { verbatim };
                    if render::width(line) > cols && !exempt {
                        over.push(format!("{name} @{cols}: {line}"));
                    }
                }
            }
        }
        assert!(
            over.is_empty(),
            "{} lines run past the terminal:\n{}",
            over.len(),
            over.join("\n")
        );
    }

    /// Prose keeps a measure on a wide terminal: a topic page, and the notes
    /// on a command page, never run a line past 72 cells (`MEASURE`). A 200-cell
    /// line of prose is a line the eye loses on its way back.
    #[test]
    fn prose_keeps_its_measure_on_a_wide_terminal() {
        let ctx = at(Ctx::MAX_COLS);
        let copied = copied_lines();
        let mut over = Vec::new();
        for t in Topic::ALL {
            for line in topic_section(&ctx, t).lines() {
                if render::width(line) > 72 && !copied.iter().any(|c| c == line.trim()) {
                    over.push(format!("topic {}: {line}", t.slug()));
                }
            }
        }
        for d in cmddoc::COMMAND_REF {
            let page = command_section(&ctx, d);
            let notes: Vec<&str> = page
                .lines()
                .skip_while(|l| l.trim() != "NOTES")
                .skip(1)
                .take_while(|l| !l.trim_start().starts_with(ctx.arrow()))
                .collect();
            if !d.notes.is_empty() && notes.is_empty() {
                over.push(format!("command {}: no NOTES heading", d.verb));
            }
            for line in notes {
                if render::width(line) > 72 {
                    over.push(format!("command {}: {line}", d.verb));
                }
            }
        }
        assert!(over.is_empty(), "{}", over.join("\n"));
    }

    /// What is copied is printed whole, on one line of its own, at every
    /// width: an example command (its note may follow it, two cells on), and
    /// every code line a topic carries.
    #[test]
    fn what_is_copied_is_printed_whole_on_one_line() {
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            let ctx = at(cols);
            for d in cmddoc::COMMAND_REF {
                let page = command_section(&ctx, d);
                for e in d.examples {
                    assert!(
                        page.lines().any(|l| l
                            .trim_start()
                            .strip_prefix(e.cmd)
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with("  "))),
                        "`{}` is not printed whole at {cols}:\n{page}",
                        e.cmd
                    );
                }
            }
            for t in Topic::ALL {
                let page = topic_section(&ctx, t);
                for code in topic_body(t).lines().filter(|l| l.starts_with("  ")) {
                    let code = match code.split_once('\t') {
                        None => code.trim(),
                        Some((term, kept)) if term.trim().is_empty() => kept.trim(),
                        Some(_) => continue,
                    };
                    assert!(
                        page.lines().any(|l| l.trim() == code),
                        "topic {}: {code:?} is not printed whole at {cols}:\n{page}",
                        t.slug()
                    );
                }
            }
        }
    }

    /// A usage line too long for the terminal breaks BETWEEN arguments: no
    /// `[--project p]` or `<title…>` is split across two lines, nothing is
    /// lost, and every line fits unless it is one piece that cannot break. At
    /// every width from `Ctx::MIN_COLS` to `Ctx::MAX_COLS`: sampled at 60 alone,
    /// a break inside a group went unseen at 101 of the 121 widths.
    #[test]
    fn usage_breaks_between_arguments_and_keeps_every_one() {
        let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            for d in cmddoc::COMMAND_REF {
                let page = command_section(&at(cols), d);
                let lines = block_under(&page, "USAGE");
                for l in &lines {
                    assert!(
                        render::width(l) <= cols || usage_pieces(l.trim()).len() == 1,
                        "{} at {cols}: usage line wider than the terminal: {l:?}",
                        d.verb
                    );
                }
                let joined = lines
                    .iter()
                    .map(|l| l.trim())
                    .fold(String::new(), |acc, l| {
                        if acc.is_empty() || l.starts_with('|') {
                            acc + l
                        } else {
                            acc + " " + l
                        }
                    });
                assert_eq!(
                    words(&joined),
                    words(d.usage),
                    "{} at {cols}: the wrapped usage lost or changed something",
                    d.verb
                );
                // Every innermost bracket group survives on one line.
                let b = d.usage.as_bytes();
                let mut open: Vec<usize> = Vec::new();
                for (i, c) in b.iter().enumerate() {
                    match c {
                        b'[' | b'<' => open.push(i),
                        b']' | b'>' => {
                            let Some(start) = open.pop() else { continue };
                            let group = &d.usage[start..=i];
                            if !group[1..group.len() - 1].contains(['[', '<']) {
                                assert!(
                                    lines.iter().any(|l| l.contains(group)),
                                    "{} at {cols}: {group:?} was split across lines:\n{}",
                                    d.verb,
                                    lines.join("\n")
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// An example whose note does not fit beside it puts the note on the line
    /// under it, rather than running past the terminal or losing the note.
    #[test]
    fn an_example_note_that_does_not_fit_beside_its_command_goes_under_it() {
        let page = command_section(&at(60), cmddoc::find("add").unwrap());
        let cmd = r#"tasqx add Call bank due:"friday 9am" --remind -30m"#;
        let lines: Vec<&str> = page.lines().collect();
        let at_cmd = lines
            .iter()
            .position(|l| l.trim() == cmd)
            .unwrap_or_else(|| panic!("the command must stand alone on its line:\n{page}"));
        assert!(
            lines[at_cmd + 1].contains("reminder 30m before due"),
            "the note belongs on the line under its command:\n{page}"
        );
    }

    /// An example block puts its notes on one side: beside every command, or
    /// under every one. `dashboard` at 60 columns set one note beside its
    /// command and the next one under, which reads as two layouts in one block.
    #[test]
    fn an_example_block_keeps_its_notes_on_one_side() {
        for cols in [60, 80, 100, 140] {
            let ctx = at(cols);
            for d in cmddoc::COMMAND_REF {
                let page = command_section(&ctx, d);
                let block = block_under(&page, "EXAMPLES");
                let sides: Vec<bool> = d
                    .examples
                    .iter()
                    .filter_map(|e| e.note.filter(|n| !n.is_empty()).map(|n| (e.cmd, n)))
                    .map(|(cmd, note)| {
                        let head = note.split_whitespace().next().expect("a word");
                        let line = block
                            .iter()
                            .find(|l| l.trim_start().starts_with(cmd))
                            .unwrap_or_else(|| panic!("{}: {cmd:?} missing:\n{page}", d.verb));
                        line[line.find(cmd).unwrap() + cmd.len()..].contains(head)
                    })
                    .collect();
                assert!(
                    sides.windows(2).all(|w| w[0] == w[1]),
                    "{} at {cols}: notes beside some commands and under others:\n{page}",
                    d.verb
                );
            }
        }
    }

    /// A note moved under its command, and wrapped, hangs its second line
    /// under its own text rather than under the `·` that marks it.
    #[test]
    fn a_displaced_note_hangs_under_its_own_text() {
        let ctx = at(60);
        let marker = format!("{} ", ctx.mid());
        let mut continuations = 0;
        for d in cmddoc::COMMAND_REF {
            let page = command_section(&ctx, d);
            let block = block_under(&page, "EXAMPLES");
            let mut text_x: Option<usize> = None;
            for line in block {
                let body = line.trim_start();
                let indent = line.len() - body.len();
                if d.examples.iter().any(|e| body.starts_with(e.cmd)) {
                    text_x = None;
                } else if body.starts_with(&marker) {
                    // In cells: the marker `·` is one cell and two bytes.
                    text_x = Some(indent + render::width(&marker));
                } else if let Some(x) = text_x {
                    continuations += 1;
                    assert_eq!(
                        indent, x,
                        "{}: a wrapped note hangs at {indent}, not under its text at {x}:\n{page}",
                        d.verb
                    );
                }
            }
        }
        assert!(
            continuations > 0,
            "fixture: no wrapped note at 60 columns to judge"
        );
    }

    /// Inline code is copied like any other code, so a backtick span is
    /// never split across two lines. Prose wrapped at every space split them:
    /// `tasqx add` on one line and `-- "a real task"` on the next.
    ///
    /// Every width from `Ctx::MIN_COLS` to `Ctx::MAX_COLS`, not a sample: the
    /// table of contents cut summaries inside a span at 50, 52 and 58 while
    /// 60, 80, 100 and 140 were clean.
    #[test]
    fn inline_code_is_never_split_across_lines() {
        let mut split = Vec::new();
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            for (name, page) in every_page(&at(cols)) {
                for line in page.lines() {
                    if line.matches('`').count() % 2 == 1 {
                        split.push(format!("{name} @{cols}: {line}"));
                    }
                }
            }
        }
        assert!(split.is_empty(), "{}", split.join("\n"));
    }

    /// Where there is room, an example's note sits beside its command.
    #[test]
    fn a_note_sits_beside_its_command_when_there_is_room() {
        let page = command_section(&at(100), cmddoc::find("init").unwrap());
        assert!(
            page.lines()
                .any(|l| l.contains("tasqx init home && tasqx use home")
                    && l.contains("claim then set default")),
            "the note belongs beside its command at 100 columns:\n{page}"
        );
    }

    /// A usage synopsis that fits prints whole, exactly as written.
    #[test]
    fn usage_that_fits_prints_whole() {
        let cols = 140;
        for d in cmddoc::COMMAND_REF {
            if 2 + render::width(d.usage) > cols {
                continue;
            }
            let page = command_section(&at(cols), d);
            assert_eq!(
                block_under(&page, "USAGE"),
                vec![format!("  {}", d.usage)],
                "{}: a usage that fits is printed whole",
                d.verb
            );
        }
    }

    /// A wrapped usage fills each line before it breaks: the piece that opens
    /// a line would not have fitted on the one above. The first line is only
    /// indented two cells, so it gets the width the hung lines do not.
    #[test]
    fn usage_lines_are_filled_before_they_break() {
        for cols in [60, 80] {
            for d in cmddoc::COMMAND_REF {
                let page = command_section(&at(cols), d);
                let lines = block_under(&page, "USAGE");
                if lines.len() < 2 {
                    continue;
                }
                let pieces = usage_pieces(d.usage);
                let mut it = pieces.iter().peekable();
                let mut above: Option<usize> = None;
                for line in &lines {
                    let (joiner, first) = **it.peek().expect("a piece opens every line");
                    if let Some(w) = above {
                        assert!(
                            w + render::width(joiner) + render::width(first) > cols,
                            "{} at {cols}: {first:?} would have fitted on the line above:\n{}",
                            d.verb,
                            lines.join("\n")
                        );
                    }
                    let mut rest = line.trim_start();
                    let mut opening = true;
                    while let Some(&&(j, p)) = it.peek() {
                        let expect = if opening {
                            p.to_string()
                        } else {
                            format!("{j}{p}")
                        };
                        let Some(r) = rest.strip_prefix(expect.as_str()) else {
                            break;
                        };
                        rest = r;
                        opening = false;
                        it.next();
                    }
                    assert!(rest.is_empty(), "{}: unparsed {rest:?}", d.verb);
                    above = Some(render::width(line));
                }
            }
        }
    }

    /// A name that is a topic and a command opens both pages: they are set
    /// apart by more than the blank line that separates a page's own sections,
    /// and the command's See also does not repeat a name the topic's Commands
    /// line has just offered (house style rule 11).
    #[test]
    fn a_name_that_opens_two_pages_sets_them_apart_and_says_nothing_twice() {
        let ctx = at(100);
        let names_on = |page: &str, label: &str| -> Vec<String> {
            page.lines()
                .skip_while(|l| !l.contains(label))
                .take_while(|l| !l.trim().is_empty())
                .flat_map(|l| {
                    l.replace(label, "")
                        .replace(ctx.arrow(), "")
                        .split(ctx.mid())
                        .map(|n| n.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        for name in ["projects", "daemon"] {
            let page = render(&ctx, Some(name)).unwrap();
            let seam = format!("\n\n\ntasqx {name}");
            assert!(
                page.contains(&seam),
                "`manual {name}`: the command page must start after two blank lines:\n{page}"
            );
            let offered = names_on(&page, "Commands:");
            let again: Vec<String> = names_on(&page, "See also:")
                .into_iter()
                .filter(|n| offered.contains(n))
                .collect();
            assert!(
                again.is_empty(),
                "`manual {name}` offers {again:?} twice:\n{page}"
            );
        }
    }

    /// A date a reader types is code, quotes and all where main quoted it
    /// (`"friday 9am"`: a value with a space must reach tasqx quoted, which
    /// the capturing examples spell `due:"friday 9am"`), and is never split
    /// across lines at any width.
    #[test]
    fn typed_date_literals_are_never_split() {
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            let page = topic_section(&at(cols), Topic::Dates);
            for literal in [
                "`\"in 3 days\"`",
                "`\"tomorrow 17:00\"`",
                "`\"friday 9am\"`",
                "`\"2026-09-09 17:00\"`",
            ] {
                assert!(
                    page.lines().any(|l| l.contains(literal)),
                    "{literal} is split or missing at {cols}:\n{page}"
                );
            }
        }
    }

    /// The manual draws gauges of its own — in the captured `pick` screen and
    /// in the write echoes — and they must agree with the renderer at the
    /// figure beside them, exactly as the HTML guide's samples must (#562).
    /// The guide's guard walks `generate()` and never reached these.
    #[test]
    fn every_gauge_in_the_manual_matches_the_renderer() {
        let mut wrong = Vec::new();
        for (name, page) in every_page(&at(100)) {
            for bad in render::gauges_disagreeing(&page) {
                wrong.push(format!("{name}: {bad}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} gauge(s) in the manual disagree with the renderer:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
    }

    /// Every `term\tdefinition` line of every topic renders as a table row,
    /// at every width: the term two cells in, then its definition beside it
    /// or, where the terminal is too narrow for two columns, under it.
    ///
    /// Checked on the rendered page, not the source: `Reports` opened its body
    /// with `"\`, Rust's line continuation stripped the first row's indent, and
    /// `body` read that row as prose at the margin, which the source, read
    /// back by a test, could not show.
    #[test]
    fn every_table_row_renders_as_a_table_row() {
        for cols in Ctx::MIN_COLS..=Ctx::MAX_COLS {
            for t in Topic::ALL {
                let page = topic_section(&at(cols), t);
                for (term, _) in topic_body(t)
                    .lines()
                    .filter_map(|l| l.split_once('\t'))
                    .map(|(term, def)| (term.trim(), def))
                    .filter(|(term, _)| !term.is_empty())
                {
                    assert!(
                        page.lines().any(|l| l.strip_prefix("  ").is_some_and(|r| {
                            r.strip_prefix(term)
                                .is_some_and(|after| after.is_empty() || after.starts_with("  "))
                        })),
                        "topic {} at {cols}: the row {term:?} is not a table row:\n{page}",
                        t.slug()
                    );
                }
            }
        }
    }

    /// A definition is not wrapped into a column too narrow to read: at 60
    /// columns the completion topic's (since removed) `--install` row broke
    /// "edit the startup" from "file". Too narrow for two columns, a table
    /// stacks each definition under its term instead.
    #[test]
    fn a_narrow_table_stacks_rather_than_orphaning_a_word() {
        let page = topic_section(&at(60), Topic::Completion);
        assert!(
            page.lines()
                .any(|l| l.contains("print the line for your startup file")),
            "the definition was split into a column too narrow for it:\n{page}"
        );
    }

    /// The manual has a page about the screens you open, and it names each
    /// one. `pick`'s keys, its search and its key bar were documented only on
    /// the command page, so a reader of the contents could not find the
    /// browser at all.
    #[test]
    fn the_screens_topic_says_which_screens_refuse_a_pipe_and_which_degrade() {
        let page = render(&plain(), Some("screens")).expect("`tasqx manual screens` opens");
        // Verified against the binary: `pick | cat` and `dashboard | cat`
        // refuse with exit 2 (`help.rs` drives both), while `memory list`
        // prints its table (D121: "a one-line-per-doc table everywhere else")
        // and `watch` has a non-tty branch that skips the screen.
        let refusing = page
            .lines()
            .position(|l| l.contains("refuse") || l.contains("refuses"))
            .unwrap_or_else(|| panic!("the page never says which screens refuse a pipe:\n{page}"));
        let refusal_para: String = page
            .lines()
            .skip(refusing.saturating_sub(2))
            .take(6)
            .collect();
        // `config edit` joined the list: verified against the binary, it
        // refuses a piped stdout with exit 2 and names `config list`/`config
        // set` instead. The topic whose job is enumerating screens had left it
        // out entirely.
        for verb in ["pick", "dashboard", "config edit"] {
            assert!(
                refusal_para.contains(verb),
                "{verb} is not named among the screens that refuse a pipe:\n{page}"
            );
        }
        let lower = page.to_lowercase();
        assert!(
            !lower.contains("every one of them says so"),
            "the page still claims every screen refuses a pipe:\n{page}"
        );
        for plain_one in ["memory list", "watch"] {
            assert!(
                page.contains(plain_one),
                "{plain_one} is not named:\n{page}"
            );
        }
    }

    #[test]
    fn the_screens_topic_names_every_screen_you_can_open() {
        assert!(
            Topic::ALL.iter().any(|t| t.slug() == "screens"),
            "the contents has no screens topic"
        );
        let page = render(&plain(), Some("screens")).expect("`tasqx manual screens` opens");
        let screens = [
            "tasqx pick",
            "tasqx dashboard",
            "tasqx config edit",
            "tasqx memory list",
            "tasqx watch",
        ];
        for screen in screens {
            assert!(
                page.contains(screen),
                "the screens topic never names {screen}:\n{page}"
            );
        }
        // And the count it opens with is the length of that roster. The page
        // said "Four commands" while listing four and omitting `config edit`,
        // so a reader counting along got a consistent, wrong answer.
        let word = [
            "zero", "one", "two", "three", "four", "five", "six", "seven",
        ][screens.len()];
        let opener = format!("{} commands open a screen", ucfirst(word));
        assert!(
            page.contains(&opener),
            "the screens topic must open with {opener:?} — it names {} screens:\n{page}",
            screens.len()
        );
    }

    /// `Five` from `five`, for a count word that opens a sentence.
    fn ucfirst(s: &str) -> String {
        let mut c = s.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => String::new(),
        }
    }

    /// The `pick` block is a capture of a real screen, not a replay: it needs
    /// a terminal on both ends, which the sample path cannot give it. The page
    /// says so, as the daemon page does for `watch`, and shows the key bar,
    /// which is the thing the topic exists to teach.
    #[test]
    fn the_pick_sample_shows_its_key_bar_and_says_it_was_captured() {
        let page = render(&plain(), Some("screens")).unwrap();
        for key in ["j/k move", "/ search", "enter open", "s start", "q leave"] {
            // `g/G ends` is not asserted: D124 ranks the bar by what a key
            // does, so a narrow terminal drops it and keeps the way out.
            assert!(
                page.contains(key),
                "the key bar is missing {key:?}:\n{page}"
            );
        }
        assert!(
            page.to_lowercase().contains("captured"),
            "the page must say the screen was captured, not replayed:\n{page}"
        );
    }

    /// D128: the page has to say that closing the browser is an ordinary end
    /// to a session, and that a filter matching nothing is not. It stated the
    /// opposite rule for as long as `pick` was D55's chooser, and a reader who
    /// believes it will not write `tasqx pick && …`.
    #[test]
    fn the_screens_topic_says_closing_pick_is_not_a_failure() {
        let page = render(&plain(), Some("screens")).expect("`tasqx manual screens` opens");
        // Flattened: the page is wrapped to a measure, so where a line breaks
        // is the renderer's business and an assertion that reads a sentence
        // whole may not depend on it.
        let flat = page.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("Leaving without starting a task exits 0"),
            "the page must say leaving is exit 0:\n{page}"
        );
        assert!(
            !flat.contains("Leaving without starting a task exits 4"),
            "the page still states D55's rule:\n{page}"
        );
        assert!(
            flat.contains("still exits 4"),
            "the page must keep the refusal non-zero:\n{page}"
        );
    }

    /// Capturing says what a write prints: the card, and that bold names what
    /// the write changed. Eighteen verbs answer this way and no topic said so.
    #[test]
    fn capturing_says_what_a_pipe_keeps_of_the_card() {
        let page = render(&plain(), Some("capturing")).unwrap();
        // Verified: piped, `tasqx start 1` prints `* started   H 16.7  …` —
        // the rail spells itself in ASCII (rule 4) and only the gauge and the
        // `▌` go.
        assert!(
            page.contains("`*`") || page.contains("spells"),
            "the page must say the rail survives a pipe as ASCII:\n{page}"
        );
        assert!(
            !page.contains("without the glyphs or the fitting"),
            "the page still claims a pipe drops every glyph:\n{page}"
        );
    }

    #[test]
    fn capturing_says_what_a_write_prints() {
        let page = render(&plain(), Some("capturing")).unwrap();
        assert!(
            page.contains("started") && page.contains("#1"),
            "no write echo is shown:\n{page}"
        );
        // The SENTENCE, not the word: "bold" survives later in the
        // paragraph, so a guard on the word alone stayed green when the
        // sentence that defines it was reworded away.
        assert!(
            page.contains("Bold is the write's"),
            "the page must say, in its own sentence, what bold means on a \
             write echo:\n{page}"
        );
    }

    /// The memory page says what opens a hit: a doc's id, or the task an
    /// annotation is on.
    #[test]
    fn the_memory_page_says_what_opens_a_hit() {
        let page = render(&plain(), Some("memory")).unwrap();
        assert!(
            page.contains("memory show"),
            "the page never says a doc id opens with `memory show`:\n{page}"
        );
        assert!(
            page.contains("annotation on #"),
            "the page never names the annotation handle:\n{page}"
        );
    }

    /// The theme page says what `*` marks, as the projects topic does.
    #[test]
    fn the_theme_page_says_what_the_star_marks() {
        let page = render(&plain(), Some("theme")).unwrap();
        let marked = page
            .lines()
            .any(|l| l.contains('*') && l.to_lowercase().contains("in effect"));
        assert!(marked, "the page never says what `*` marks:\n{page}");
    }

    /// The dates topic states the rule the parser follows (D132): a clock
    /// time with no offset is UTC, and a bare date is midnight UTC. It said a
    /// clock time was read in the machine's own zone, which was true until
    /// D132 and would now be a page describing a parser that no longer exists.
    #[test]
    fn the_dates_topic_states_the_zone_rule_the_parser_follows() {
        let page = render(&plain(), Some("dates")).unwrap();
        let lower = page.to_lowercase();
        for stale in ["your machine", "own zone", "amsterdam"] {
            assert!(
                !lower.contains(stale),
                "the page still says a clock time is local ({stale:?}):\n{page}"
            );
        }
        assert!(
            page.contains("`due:17:00` is 17:00 UTC"),
            "the page must say a clock time is UTC:\n{page}"
        );
        assert!(
            lower.contains("midnight utc"),
            "the page must keep the rule for a bare date:\n{page}"
        );
    }

    /// Each note on a command page is a paragraph of its own. Wrapped, two
    /// notes set flush against each other read as one.
    #[test]
    fn each_note_is_a_paragraph_of_its_own() {
        let ctx = at(60);
        for d in cmddoc::COMMAND_REF.iter().filter(|d| d.notes.len() > 1) {
            let page = command_section(&ctx, d);
            let lines = below_examples(&page);
            for n in &d.notes[1..] {
                let head = n.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
                let i = lines
                    .iter()
                    .position(|l| l.trim_start().starts_with(&head))
                    .unwrap_or_else(|| panic!("{}: note {head:?} not found:\n{page}", d.verb));
                assert_eq!(
                    lines[i - 1].trim(),
                    "",
                    "{}: the note starting {head:?} runs on from the one above:\n{page}",
                    d.verb
                );
            }
        }
    }

    /// A page has two levels of heading, and they must look it in RENDERED
    /// BYTES in every built-in theme and under `NO_COLOR`, not only in the
    /// roles asked for: `mono` draws `accent` and `header` alike as `ESC[1m`,
    /// so a test of roles passed while TASQX MANUAL, TOPICS and COMMANDS were
    /// three identical bold lines in capitals.
    ///
    /// Also: a section heading is set apart from what it heads without
    /// colour, flush over indented content or a blank line ahead of prose;
    /// no row carries paint of its own; a topic's title keeps its own case.
    ///
    /// Rewritten twice from this change's first cut, which asserted `header`
    /// on every heading; the intent (a heading is painted as one and keeps a
    /// weight without colour) is kept, and the comparison moved from roles to
    /// bytes.
    #[test]
    fn section_headings_sit_one_step_under_the_page_title() {
        let truecolor = Caps {
            depth: ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        };
        let mut ctxs: Vec<(String, Ctx)> = crate::theme::BUILTINS
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    Ctx::new(crate::theme::builtin(n).unwrap(), truecolor),
                )
            })
            .collect();
        ctxs.push(("NO_COLOR".to_string(), no_color()));
        for (theme, ctx) in &ctxs {
            let add = command_section(ctx, cmddoc::find("add").unwrap());
            let completion = topic_section(ctx, Topic::Completion);
            let toc = toc(ctx);
            // The escapes a line opens with, before its first visible text.
            let sgr = |line: &str, text: &str| -> String {
                line[..line.find(text).expect("text on its line")].to_string()
            };
            for (page, title, sections) in [
                (&toc, "TASQX MANUAL", &["TOPICS", "COMMANDS"][..]),
                (&add, "tasqx add", &["USAGE", "EXAMPLES", "NOTES"][..]),
                (
                    &completion,
                    "Shell completion",
                    &["BEFORE YOU SWITCH IT ON"][..],
                ),
            ] {
                let lines: Vec<&str> = page.lines().collect();
                let title_line = lines[0];
                assert!(
                    strip_sgr(title_line).starts_with(title),
                    "{theme}: {title} is not the page's first line:\n{page}"
                );
                let title_sgr = sgr(title_line, title);
                assert!(
                    !title_sgr.is_empty(),
                    "{theme}: {title} carries no emphasis"
                );
                for section in sections {
                    let at = lines
                        .iter()
                        .position(|l| strip_sgr(l) == *section)
                        .unwrap_or_else(|| panic!("{theme}: no {section} heading:\n{page}"));
                    assert_ne!(
                        sgr(lines[at], section),
                        title_sgr,
                        "{theme}: {section} is drawn exactly like the title {title}"
                    );
                    // Set apart without colour (rule 7): flush over content
                    // that is indented, or a blank line ahead of prose at the
                    // heading's own margin.
                    let next = lines[at + 1];
                    let set_apart = if next.trim().is_empty() {
                        lines
                            .get(at + 2)
                            .is_some_and(|l| !l.is_empty() && !l.starts_with(' '))
                    } else {
                        next.starts_with("  ")
                    };
                    assert!(
                        set_apart,
                        "{theme}: {section} is not set apart from what it heads:\n{page}"
                    );
                }
            }
            let rows = toc
                .lines()
                .filter(|l| l.starts_with("  "))
                .chain(add.lines().filter(|l| l.contains("See also:")))
                .chain(completion.lines().filter(|l| l.contains("Commands:")));
            for row in rows {
                assert!(!row.contains('\x1b'), "{theme}: a row is painted: {row:?}");
            }
        }
    }

    /// House style rule 1: what is read is not dimmed. The TOC dimmed its
    /// topic titles while printing its command summaries at full weight, and
    /// every command page dimmed its notes, which are the page's prose.
    #[test]
    fn what_is_read_is_not_dimmed() {
        let ctx = colour(Ctx::MAX_COLS);
        let toc = toc(&ctx);
        // In order, not by name: `projects` and `daemon` are a topic AND a
        // command, so a lookup by name finds the wrong row for one of them.
        let rows: Vec<&str> = toc.lines().filter(|l| l.starts_with("  ")).collect();
        let expected: Vec<(&str, &str)> = Topic::ALL
            .iter()
            .map(|t| (t.slug(), t.title()))
            .chain(cmddoc::COMMAND_REF.iter().map(|d| (d.verb, d.summary)))
            .collect();
        assert_eq!(rows.len(), expected.len(), "one TOC row per entry:\n{toc}");
        for (row, (name, text)) in rows.iter().zip(expected) {
            assert_eq!(
                strip_sgr(row).split_whitespace().next(),
                Some(name),
                "TOC rows out of order:\n{toc}"
            );
            assert!(
                row.ends_with(text),
                "the TOC paints {name:?}'s description instead of leaving it at the \
                 foreground: {row:?}"
            );
        }
        for d in cmddoc::COMMAND_REF {
            let page = command_section(&ctx, d);
            for n in d.notes {
                let head = n.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
                let line = below_examples(&page)
                    .into_iter()
                    .find(|l| l.contains(&head))
                    .unwrap_or_else(|| panic!("{}: note {head:?} not found", d.verb));
                assert!(
                    !line.contains('\x1b'),
                    "{}: a note is painted, where it is the page's prose: {line:?}",
                    d.verb
                );
            }
        }
    }

    /// Markdown becomes the four shapes `body` reads: a heading in capitals,
    /// a paragraph at the margin with its code kept in backticks, a table row
    /// and a list item as `term\tdefinition` rows, a fence as code with its
    /// prompt dropped.
    #[test]
    fn markdown_becomes_the_four_shapes() {
        let src = "## tasqx add\n\nUse `due:` and [links](X.md).\n\n\
                   | Command | What |\n|---|---|\n| `tasqx list` | the table |\n\n\
                   - one\n- two\n\n```console\n$ tasqx next\nnext #1\n```\n";
        assert_eq!(
            shapes(src),
            "TASQX ADD\n\nUse `due:` and links.\n\n  tasqx list\tthe table\n\n\
             \x20 -\tone\n  -\ttwo\n\n  tasqx next\n  next #1\n\n"
        );
    }

    /// Every topic is read out of the wiki, and every section it names is
    /// there: a renamed heading is a panic here, not an empty page.
    #[test]
    fn every_topic_reads_its_wiki_sections() {
        for t in Topic::ALL {
            assert!(
                topic_body(t).len() > 200,
                "topic {} read almost nothing out of the wiki",
                t.slug()
            );
        }
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

    /// tasqx audit 2026-09 #196: the manual's own worked example for the one
    /// asymmetry between reading and writing a `project:` filter claimed the
    /// unquoted spelling "files the task" on the write side. Run for real
    /// (`tasqx add "paint" project:Home Renovation`), it either `not_found`s
    /// or — once a project happens to be named exactly the leading word —
    /// silently mis-files the task and welds the remainder onto the title:
    /// the D30 defect, reconstructed through the documentation the D30 fix
    /// was supposed to teach around. The recommended spelling must be the
    /// quoted one, which actually works on both sides.
    #[test]
    fn filters_topic_does_not_claim_the_unquoted_write_spelling_files_the_task() {
        let s = topic_body(Topic::Filters);
        assert!(
            !s.contains("project:Home Renovation` files the task"),
            "the manual must not claim the unquoted `project:Home Renovation` \
             spelling on `add`/`modify` unconditionally succeeds — run for \
             real, it either not_found's or mis-files the task into a wrong \
             project with a mangled title: {s}"
        );
        assert!(
            s.contains("project:\"Home Renovation\""),
            "the manual must show the quoted spelling that actually works on \
             both `list` and `add`/`modify`: {s}"
        );
    }

    /// tasqx audit 2026-09 #226.1: `wait:` and `scheduled:`/`sched:` are the
    /// two sugar tokens with the largest behavioural consequence — they park
    /// a new task in `backlog`, invisible to `@working` — and both parse
    /// (verified against the binary), but the terminal manual's `capturing`
    /// topic never mentioned either, teaching seven of the nine working sugar
    /// tokens and silently dropping the two riskiest ones.
    #[test]
    fn capturing_topic_documents_wait_and_scheduled_sugar() {
        let s = topic_body(Topic::Capturing);
        for token in ["wait:", "scheduled:", "sched:"] {
            assert!(
                s.contains(token),
                "`tasqx manual capturing` must document `{token}` sugar, \
                 which the parser accepts: {s}"
            );
        }
    }

    /// tasqx audit 2026-09 #223: token accounting is the one feature this
    /// tool claims no other task manager has, and the terminal manual never
    /// mentioned it anywhere reachable — not the report topic that shows the
    /// TOKENS column, not the daemon topic that documents the OTLP receiver,
    /// and not the JSON API topic that documents `token.add`. The generated
    /// HTML guide already explains the four buckets (`docs.rs`), so the two
    /// surfaces disagreed about whether the feature was documented at all.
    #[test]
    fn reports_daemon_and_json_api_topics_document_token_accounting() {
        let reports = topic_body(Topic::Reports);
        assert!(
            reports.to_lowercase().contains("token"),
            "`tasqx manual reports` must explain the TOKENS column: {reports}"
        );

        let daemon = topic_body(Topic::Daemon);
        assert!(
            daemon.to_uppercase().contains("OTLP"),
            "`tasqx manual daemon` must mention the OTLP receiver it can run: {daemon}"
        );

        let json_api = topic_body(Topic::JsonApi);
        assert!(
            json_api.contains("token.add"),
            "`tasqx manual json-api` must document `token.add`: {json_api}"
        );
    }

    /// tasqx audit 2026-09 #226.4: `tasqx manual dates` covers the four date
    /// fields and recurrence, but omits most of the vocabulary its own parser
    /// accepts — every relative day word, every weekday name, and every time
    /// form — so the one manual page dedicated to dates never mentions that
    /// you can give a time at all, which is exactly where the biggest
    /// surprise lives (`17:00` means 17:00 UTC).
    #[test]
    fn dates_topic_documents_relative_days_weekdays_and_times() {
        let s = topic_body(Topic::Dates);
        for word in [
            "today",
            "tomorrow",
            "yesterday",
            "monday",
            "in 3 days",
            "17:00",
            "UTC",
        ] {
            assert!(
                s.to_lowercase().contains(&word.to_lowercase()),
                "`tasqx manual dates` must mention {word:?}: {s}"
            );
        }
    }
}
