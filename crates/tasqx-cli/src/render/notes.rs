//! `tasqx notes` and `tasqx note <ID>` (D241).

use super::*;

use jiff::Timestamp;
use serde_json::Value;

use crate::columns::{self, Column};
use crate::theme::Ctx;

/// A note's first non-blank line, trimmed: what a row and an echo show of it.
pub(crate) fn note_first_line(body: &str) -> &str {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// A line that is one link and nothing else. Cut, it no longer opens, so its
/// row overflows instead (rule 2's "past its floor the row overflows").
fn lone_url(line: &str) -> bool {
    (line.starts_with("http://") || line.starts_with("https://"))
        && !line.contains(char::is_whitespace)
}

/// `tasqx notes`: one line per note under a header, newest first, built like
/// `memory list` (D125). The text carries the row; a link mark, the project
/// and the day it was added recede. An archived note (under `--all`) is dim.
pub fn notes_table(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let empty = Vec::new();
    let notes = result
        .get("notes")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let total = result.get("total").and_then(Value::as_u64).unwrap_or(0);
    if notes.is_empty() {
        return if total == 0 {
            "No notes here. `tasqx note <text…>` captures one; `--all` lists the archived too.\n"
                .to_string()
        } else {
            format!("No notes on this page; {total} match.\n")
        };
    }
    struct Row {
        id: String,
        body: String,
        link: &'static str,
        project: String,
        added: String,
        archived: bool,
    }
    let mark = if ctx.caps.unicode { "↗" } else { "L" };
    let rows: Vec<Row> = notes
        .iter()
        .map(|n| Row {
            id: n["short_id"].as_i64().unwrap_or(0).to_string(),
            body: san(note_first_line(n["body"].as_str().unwrap_or(""))),
            link: if n["has_links"].as_bool().unwrap_or(false) {
                mark
            } else {
                ""
            },
            project: s(n, "project"),
            added: field_ts(n, "created").map_or_else(String::new, |t| day_ago(t, now)),
            archived: n["status"] == "archived",
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
    // A lone link overflows its own row rather than widening the column, so
    // one long URL does not drop the project and the date off every row.
    let body_w = rows
        .iter()
        .filter(|r| !lone_url(&r.body))
        .map(|r| width(&r.body))
        .max()
        .unwrap_or(0)
        .max(width("NOTE"));
    let (id_w, link_w) = (widest(|r| &r.id, "ID"), widest(|r| r.link, ""));
    let added_w = widest(|r| &r.added, "ADDED");
    let w = columns::fit(
        &[
            Column::fixed(id_w),
            // Half the terminal, not `memory list`'s all of it: a note's line
            // is long prose, and the project and day beside it are short, so
            // the text gives a little before they go.
            Column::shrinks(
                body_w,
                lead_floor(body_w, ctx.cols, id_w + link_w)
                    .min((ctx.cols / 2).max(MIN_TITLE_CELLS)),
            ),
            Column::fixed(link_w),
            Column::drops(widest(|r| &r.project, "PROJECT"), 8),
            // A day cut to `yesterd…` is no day: it goes whole or not at all.
            Column::drops(added_w, added_w),
        ],
        ctx.cols,
    );

    let shown = notes.len() as u64;
    let mut parts = vec![(
        "card.strong",
        format!("{total} {}", if total == 1 { "note" } else { "notes" }),
    )];
    if shown < total {
        parts.push(("muted", format!("{shown} shown")));
    }
    let mut out = format!("{}\n\n", summary_line(ctx, None, parts));
    out.push_str(&ctx.paint(
        "table.label",
        &row_cells(
            ctx,
            &w,
            &[
                (None, "ID"),
                (None, "NOTE"),
                (None, ""),
                (None, "PROJECT"),
                (None, "ADDED"),
            ],
        ),
    ));
    out.push('\n');
    for r in &rows {
        let mut rw = w.clone();
        if lone_url(&r.body) {
            rw[1] = rw[1].max(width(&r.body));
        }
        out.push_str(&row_cells(
            ctx,
            &rw,
            &[
                (Some("muted"), r.id.as_str()),
                (r.archived.then_some("muted"), &r.body),
                // No paint on an empty cell, which would be escapes around nothing.
                ((!r.link.is_empty()).then_some("accent"), r.link),
                ((!r.project.is_empty()).then_some("project"), &r.project),
                (Some("muted"), &r.added),
            ],
        ));
        out.push('\n');
    }
    out
}

/// `tasqx note <ID>`: the note whole, under one dim line of what it is.
pub fn note_shown(ctx: &Ctx, note: &Value, now: Timestamp) -> String {
    let mut facts = vec![
        format!("#{}", note["short_id"].as_i64().unwrap_or(0)),
        s(note, "status"),
    ];
    let project = s(note, "project");
    if !project.is_empty() {
        facts.push(project);
    }
    if let Some(t) = field_ts(note, "created") {
        facts.push(format!("added {}", day_ago(t, now)));
    }
    if let Some(n) = note["promoted_task_short_id"].as_i64() {
        facts.push(format!("promoted to task #{n}"));
    } else if s(note, "promoted_to").starts_with("memory:") {
        facts.push("promoted to memory".to_string());
    }
    format!(
        "{}\n{}\n",
        ctx.paint("muted", &facts.join(&format!(" {} ", ctx.mid()))),
        san_multiline(note["body"].as_str().unwrap_or(""))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(notes: Value) -> Value {
        let total = notes.as_array().map_or(0, Vec::len);
        json!({ "count": total, "total": total, "next_offset": null, "notes": notes })
    }

    fn note(sid: i64, body: &str, links: bool) -> Value {
        json!({
            "short_id": sid, "body": body, "has_links": links, "project": null,
            "status": "open", "created": "2026-10-24T09:00:00Z",
        })
    }

    /// A body that is just a link is never cut, at any width: cut, it does not
    /// open. Prose beside it is cut to the column as usual.
    #[test]
    fn a_lone_url_overflows_its_row_and_prose_is_cut() {
        let url = "https://example.com/a/very/long/path/that/goes/on/and/on/well/past/forty/cells";
        let prose = "a long line of prose that goes on and on well past forty cells of terminal";
        let ctx = Ctx::new(crate::theme::default_theme(), crate::theme::Caps::PLAIN).with_cols(40);
        let out = notes_table(
            &ctx,
            &page(json!([note(2, url, true), note(1, prose, false)])),
            "2026-10-24T12:00:00Z".parse().unwrap(),
        );
        assert!(out.contains(url), "the URL was cut:\n{out}");
        assert!(!out.contains(prose), "the prose was not cut:\n{out}");
        assert!(out.contains("NOTE"), "{out}");
    }
}
