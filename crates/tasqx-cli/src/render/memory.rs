//! `tasqx memory list`/`memory search` (task #727 split of `render.rs`).

use super::*;

use jiff::Timestamp;
use serde_json::Value;

use crate::columns::{self, Column};
use crate::theme::Ctx;

/// One line that says what a memory doc is about, out of the start of its body.
///
/// The frontmatter's `description`, when the doc has one: imported agent
/// memories open with a `---` block, and printing it verbatim is how
/// `name: vh-mcp-standaard description: "…` came to be every preview's first
/// words. Otherwise the first line of prose, without its markdown markers.
///
/// The fence itself is found by `tasqx_core::frontmatter::split` (task
/// #12/D135) — the same parser `tui::memory::doc_lines` and
/// `verbs::memory_docs_from_path`'s import cut both call — rather than a
/// second hand-rolled `---` scan reading the same bytes its own way.
pub(crate) fn doc_summary(body: &str) -> String {
    let (pairs, rest) = tasqx_core::frontmatter::split(body);
    if let Some(d) = pairs
        .iter()
        .find_map(|(k, v)| (*k == Some("description")).then_some(*v))
        .filter(|d| !d.is_empty())
    {
        return san(d);
    }
    rest.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '-', '*', '>', ' '])
                .trim()
        })
        .find(|l| !l.is_empty() && !l.starts_with("```") && *l != "---")
        .map(|l| san(&l.replace("**", "").replace('`', "")))
        .unwrap_or_default()
}

/// The floor of `memory list`'s title beside the id, which never goes
/// (D125(b)): as much of what the title asks for as the terminal can give it
/// beside the id, so the other columns go before the title gives way (rule 1,
/// D120(c)), and never below [`MIN_TITLE_CELLS`], where a title stops telling
/// one doc from another. Past that the row overflows, as every table's does
/// (D120). The first version had no lower bound and printed `...` for every
/// title at 40 columns. `memory search` does not need it: each of its records
/// fits its own head line ([`memory_hits`]).
pub(crate) fn lead_floor(asked: usize, cols: usize, fixed: usize) -> usize {
    asked.min(
        cols.saturating_sub(fixed + columns::GAP)
            .max(MIN_TITLE_CELLS),
    )
}

/// Twelve cells: `android-t...`, still a name. The floor `memory list` shipped
/// with (D121), kept as the lower bound of [`lead_floor`].
pub(crate) const MIN_TITLE_CELLS: usize = 12;

/// `tasqx memory list` off a terminal: one line per doc under a header (D121).
///
/// It used to print three lines per doc, the title with its source in
/// parentheses, a snippet with frontmatter leaking into it, and a whole line
/// for the id, and closed on `N doc(s) of M`. Every row weighed the same and
/// a grep for a title found a line without the id `memory show` needs. Now
/// the title carries the row, the id sits on it (dim, last, never dropped:
/// it is the handle), and the summary says what the reader cannot count.
pub fn memory_table(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let empty = Vec::new();
    let docs = result
        .get("docs")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let total = result.get("total").and_then(Value::as_u64).unwrap_or(0);
    if docs.is_empty() {
        return if total == 0 {
            "No memory docs yet. `tasqx memory add <title> <body>` stores one.\n".to_string()
        } else {
            format!("No docs on this page; the store holds {total}.\n")
        };
    }
    struct Row {
        title: String,
        project: String,
        updated: String,
        id: String,
    }
    let rows: Vec<Row> = docs
        .iter()
        .map(|d| Row {
            title: s(d, "title"),
            project: s(d, "project"),
            updated: field_ts(d, "modified").map_or_else(String::new, |t| day_ago(t, now)),
            id: s(d, "id"),
        })
        .collect();
    let widest = |f: fn(&Row) -> &str, label: &str| {
        let content = rows.iter().map(|r| width(f(r))).max().unwrap_or(0);
        if content == 0 {
            0
        } else {
            content.max(width(label))
        }
    };
    let (title_w, id_w) = (widest(|r| &r.title, "TITLE"), widest(|r| &r.id, "ID"));
    let w = columns::fit(
        &[
            Column::shrinks(title_w, lead_floor(title_w, ctx.cols, id_w)),
            Column::drops(widest(|r| &r.project, "PROJECT"), 8),
            Column::drops(widest(|r| &r.updated, "UPDATED"), 7),
            Column::fixed(id_w),
        ],
        ctx.cols,
    );
    let line = |cells: [(Option<&str>, &str); 4]| {
        join_cells(
            cells
                .iter()
                .zip(&w)
                .filter(|(_, w)| **w > 0)
                .map(|((role, text), w)| cell(ctx, *role, text, *w))
                .collect(),
        )
    };

    let shown = docs.len() as u64;
    let mut parts = vec![(
        "card.strong",
        format!("{total} {}", if total == 1 { "doc" } else { "docs" }),
    )];
    if shown < total {
        parts.push(("muted", format!("{shown} shown")));
    }
    let mut out = format!("{}\n\n", summary_line(ctx, None, parts));
    out.push_str(&ctx.paint(
        "table.label",
        &line([
            (None, "TITLE"),
            (None, "PROJECT"),
            (None, "UPDATED"),
            (None, "ID"),
        ]),
    ));
    out.push('\n');
    for r in &rows {
        out.push_str(&line([
            (None, &r.title),
            (Some("project"), &r.project),
            (Some("muted"), &r.updated),
            (Some("muted"), &r.id),
        ]));
        out.push('\n');
    }
    out
}

/// `tasqx memory search`: one record per hit (D125(a)).
///
/// The head line is the title, where it came from, and the handle that opens
/// it, fitted like a table row: the source goes first, the title gives way
/// last ([`lead_floor`]), and the handle never goes. Under it are the words
/// that matched, cut to the terminal, because they are the reason the hit is
/// on the screen. The handle is a doc's id, which `memory show` takes, or
/// `annotation on #N` for an annotation, whose own id `memory show` refuses
/// and whose task `tasqx show N` opens.
///
/// It printed three lines per hit (the title with `(doc · source)`, the
/// snippet, and `id <uuid>` on a line of its own), every one at the same
/// weight, and closed on `N hit(s)`. #346's first cut made it a table, and at
/// 60 and 80 columns the 36-cell id left room for the title alone.
pub fn memory_hits(ctx: &Ctx, result: &Value, query: &str, raw: bool) -> String {
    let empty = Vec::new();
    let hits = result
        .get("hits")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let count = hits.len() as u64;
    let total = result
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or(count)
        .max(count);

    let mut parts = vec![(
        "card.strong",
        format!("{total} {}", if total == 1 { "hit" } else { "hits" }),
    )];
    if count < total {
        parts.push(("muted", format!("{count} shown")));
    }
    // The expression that ran (D69), not the words as typed: a plain query
    // comes back with each term quoted, which sets it off from the count
    // where dim does not reach the screen, and it is what a miss has to name.
    let asked = result.get("matched").and_then(Value::as_str).map_or_else(
        || san(query),
        |m| {
            if raw {
                format!("\"{}\"", san(m))
            } else {
                san(m)
            }
        },
    );
    let mut out = summary_line(ctx, Some(&asked), parts);
    out.push('\n');

    if !hits.is_empty() {
        struct Hit {
            title: String,
            source: String,
            handle: String,
            snippet: String,
        }
        let rows: Vec<Hit> = hits
            .iter()
            .map(|h| {
                let source = s(h, "source");
                let task = source
                    .strip_prefix("task:#")
                    .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
                let (mut source, handle) = match (s(h, "kind").as_str(), task) {
                    // The source already names the task, and the handle says
                    // it, so the source column stays empty (rule 11).
                    ("annotation", Some(n)) => (String::new(), format!("annotation on #{n}")),
                    _ => (source, s(h, "id")),
                };
                // #790/D180: a doc hit whose file moved on since it was
                // imported. Nothing when false or null — an annotation and a
                // doc `memory.add` wrote both have no origin to be behind.
                if h.get("stale").and_then(Value::as_bool).unwrap_or(false) {
                    source = if source.is_empty() {
                        "stale".to_string()
                    } else {
                        format!("{source} stale")
                    };
                }
                Hit {
                    title: s(h, "title"),
                    source,
                    handle,
                    // The engine's snippet keeps the body's line breaks,
                    // which a one-line cell cannot.
                    snippet: s(h, "snippet")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                }
            })
            .collect();
        out.push('\n');
        for r in &rows {
            // Each record's head line is fitted to its OWN handle (round 2
            // review): fitted as one table, a 36-cell doc id set the title
            // width for an annotation whose handle is 17 cells, and cut its
            // title beside 19 empty ones. The title is data and is not cut
            // to make room for the handle: where the two cannot share a line,
            // the handle takes the next one, and the title is cut only where
            // it is wider than the terminal itself. A path cut in the middle
            // names no file, so SOURCE is whole or gone, and it goes first.
            let (title_w, handle_w) = (width(&r.title), width(&r.handle));
            if title_w + columns::GAP + handle_w <= ctx.cols {
                let source_w = width(&r.source);
                let w = columns::fit(
                    &[
                        Column::fixed(title_w),
                        Column::drops(source_w, source_w),
                        Column::fixed(handle_w),
                    ],
                    ctx.cols,
                );
                out.push_str(&join_cells(
                    [
                        (None, r.title.as_str()),
                        (Some("muted"), r.source.as_str()),
                        (Some("muted"), r.handle.as_str()),
                    ]
                    .iter()
                    .zip(&w)
                    .filter(|(_, w)| **w > 0)
                    .map(|((role, text), w)| cell(ctx, *role, text, *w))
                    .collect(),
                ));
                out.push('\n');
            } else {
                out.push_str(&truncate(&r.title, ctx.cols, ctx.caps.unicode));
                out.push('\n');
                out.push_str(&format!("  {}\n", ctx.paint("muted", &r.handle)));
            }
            if !r.snippet.is_empty() {
                // Four cells, under the handle's two: `muted` draws nothing
                // under NO_COLOR, so indent is what ranks these lines there.
                out.push_str(&format!(
                    "    {}\n",
                    ctx.paint(
                        "muted",
                        &truncate(&r.snippet, ctx.cols.saturating_sub(4), ctx.caps.unicode)
                    )
                ));
            }
        }
    }

    // Prose after the records stands off them by a blank line (rule 7), and
    // wraps rather than running past the terminal.
    let mut notes: Vec<(Option<&str>, String)> = Vec::new();
    // D193: the engine fell back to any word because nothing held them all.
    // The summary names the OR that ran; this says why it is not the words
    // as typed, so a partial match is never read as a full one.
    let relaxed = result
        .get("relaxed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if relaxed && count > 0 {
        notes.push((
            None,
            "no hit had every word — these match any word".to_string(),
        ));
    }
    if count < total {
        // At the terminal's own weight, like the miss hint below: both name
        // the command that shows what this screen could not.
        notes.push((None, format!("--limit {total} shows every hit")));
    }
    // On a miss, name the expression that ran and why it came back empty
    // (D69). The summary's label is cut to half the width, so a twelve-term
    // question showed only its first terms there; this note carries the
    // expression WHOLE, wrapped by `prose` rather than cut, because "use
    // fewer" cannot be acted on by a reader who cannot see which terms there
    // were. That is why it is not a repeat of the summary (rule 11): it is
    // the only complete copy. At the terminal's own weight, because it is the
    // only line that says what to do next. The advice fits the search that
    // ran: every word of a plain query is a required phrase, so dropping
    // terms widens it, while a raw expression is already the caller's own and
    // is widened with OR.
    if count == 0 {
        if let Some(matched) = result.get("matched").and_then(Value::as_str) {
            let expr = san(matched);
            notes.push((
                None,
                if raw {
                    // Quoted like the summary's label: the engine quotes a
                    // plain query's terms for us, a raw expression it does not.
                    format!("nothing matched \"{expr}\" — OR widens it")
                } else if relaxed {
                    // D193: the any-word search missed too, so fewer words
                    // cannot help; only other words can.
                    format!("no entry has any of these words: {expr}")
                } else {
                    format!("every term was required: {expr} — use fewer, or --raw with OR")
                },
            ));
        }
    }
    if !notes.is_empty() {
        out.push('\n');
        for (role, note) in notes {
            out.push_str(&prose(ctx, role, &note, ""));
        }
    }
    out
}

#[cfg(test)]
mod tests;
