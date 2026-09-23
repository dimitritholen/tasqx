//! `tasqx report`, `projects` and the `tokens` verbs (task #727 split of
//! `render.rs`).

use super::*;

use serde_json::{json, Value};

use crate::columns::{self, Column};
use crate::theme::Ctx;

pub fn project_table(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let projects = result
        .get("projects")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if projects.is_empty() {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            "No projects.\n".to_string()
        };
    }
    // D21: `projects` is THE read surface for "where does a bare `tasqx add`
    // land?" — a fact that drove behavior while being shown nowhere. #346
    // moved the mark from a seven-cell DEFAULT column into the rail.
    struct Row {
        default: bool,
        name: String,
        archived: bool,
        desc: String,
    }
    let flag = |p: &Value, key: &str| p.get(key).and_then(Value::as_bool).unwrap_or(false);
    let rows: Vec<Row> = projects
        .iter()
        .map(|p| Row {
            default: flag(p, "default"),
            name: s(p, "name"),
            archived: flag(p, "archived"),
            desc: san(p.get("description").and_then(Value::as_str).unwrap_or("")),
        })
        .collect();
    let rail = CurrentRail::over(rows.iter().map(|r| r.default));

    // Laid out on `columns::fit` like `list` (#352). It used to be
    // `format!("{:<7}  {:<24}  {:<9}  {}")`: a project name past 24 cells
    // pushed its row out of line, and DESCRIPTION had no end at all, so on an
    // 80-column terminal every described project wrapped and took the grid
    // with it. The name gives way before it would wrap, down to a floor that
    // still tells projects apart. DESCRIPTION gives first, being the widest,
    // and is the first to go. A store where no project has one gets no
    // column for it (D51), and the same goes for STATUS, which says
    // `archived` on the rows it is true of (#346). It was an ARCHIVED column
    // that, without `--all`, could only ever say `no`, on every row.
    const MIN_PROJECT: usize = 12;
    const MAX_PROJECT: usize = 32;
    const MIN_DESC: usize = 12;
    const ARCHIVED: &str = "archived";
    let desc_w = rows.iter().map(|r| width(&r.desc)).max().unwrap_or(0);
    let name_w = rows
        .iter()
        .map(|r| width(&r.name))
        .max()
        .unwrap_or(0)
        .max(width("PROJECT"))
        .min(MAX_PROJECT);
    let status_w = if rows.iter().any(|r| r.archived) {
        width(ARCHIVED).max(width("STATUS"))
    } else {
        0
    };
    let w = columns::fit(
        &[
            Column::shrinks(name_w, MIN_PROJECT.min(name_w)),
            Column::drops(status_w, status_w),
            Column::drops(
                if desc_w == 0 {
                    0
                } else {
                    desc_w.max(width("DESCRIPTION"))
                },
                MIN_DESC,
            ),
        ],
        ctx.cols.saturating_sub(rail.width()),
    );
    let row = |cells: [(Option<&str>, &str); 3]| {
        join_cells(
            cells
                .iter()
                .zip(&w)
                .filter(|(_, w)| **w > 0)
                .map(|((role, text), w)| cell(ctx, *role, text, *w))
                .collect(),
        )
    };

    let mut out = format!(
        "{}{}",
        rail.cell(ctx, false),
        ctx.paint(
            "table.label",
            &row([(None, "PROJECT"), (None, "STATUS"), (None, "DESCRIPTION")]),
        )
    );
    out.push('\n');
    for r in &rows {
        out.push_str(&rail.cell(ctx, r.default));
        out.push_str(&row([
            (Some("project"), &r.name),
            (Some("muted"), if r.archived { ARCHIVED } else { "" }),
            (None, &r.desc),
        ]));
        out.push('\n');
    }
    out
}

/// The rail that marks the one row in a table of choices that is in effect:
/// `projects`' default and `theme list`'s active theme (#346). `*`, the way
/// `git branch` marks the branch that is checked out.
///
/// It is `docs/maintainers/terminal-style.md` rule 4 applied to a table that is not a task
/// table: two cells at the far left, never dropped by the width fit, and not
/// drawn at all when no row carries it. `projects` spent a seven-cell DEFAULT
/// column on one `*`, and `theme list` nine cells of `← active` on one row.
///
/// `*` is also the running marker in `list` without Unicode, and the dashboard
/// draws both at once: `*` for a running task in TASKS, `*` for the default
/// project in PROJECTS. They are two panels and two columns, never the same
/// column of one row, which is the collision this rail exists to prevent. An
/// earlier cut of the ruling justified the glyph with "no screen draws both",
/// which is false; `DESIGN.md` D125(d) carries the true reason.
pub(crate) struct CurrentRail {
    drawn: bool,
}

impl CurrentRail {
    /// The glyph the rail draws on the row in effect.
    pub(crate) const MARK: &'static str = "*";

    /// A rail for these rows, drawn only when one of them is in effect.
    pub(crate) fn over(current: impl IntoIterator<Item = bool>) -> Self {
        Self {
            drawn: current.into_iter().any(|c| c),
        }
    }

    /// The cells the rail takes from the row: two, or none.
    pub(crate) fn width(&self) -> usize {
        if self.drawn {
            2
        } else {
            0
        }
    }

    /// The rail's cell on one row, padding included. The padding sits outside
    /// the paint, so a row still trims cleanly.
    pub(crate) fn cell(&self, ctx: &Ctx, current: bool) -> String {
        match (self.drawn, current) {
            (false, _) => String::new(),
            (true, true) => format!("{} ", ctx.paint("accent", Self::MARK)),
            (true, false) => "  ".to_string(),
        }
    }
}

/// `"—"` (the HTML/dashboard "nothing" glyph, via `html::humanize_iso`)
/// rewritten to the terminal's own dash — matching the TOKENS column's `-`
/// for an absent value (#234 item 11), rather than mixing two "nothing"
/// glyphs on one row.
pub(crate) fn human_or_dash(iso: &str) -> String {
    let h = crate::html::humanize_iso(iso);
    if h == "—" {
        "-".to_string()
    } else {
        h
    }
}

/// `tasqx report` — the terminal table for `report.summary`.
///
/// `token_metrics` is `--metrics`, filtered to the request's `Vec<String>` as
/// given (validated against `SUMMARY_METRICS` before this is called): when it
/// names any of the four `tokens_*` buckets, all four render as their own
/// columns — the same four the HTML report and `--json` already carry —
/// instead of the single dominant-bucket cell (#212, D48a). `--metrics`
/// otherwise leaves this table alone: COUNT/EST/OVERDUE/TRACKED are its fixed
/// axis, and the terminal's actual gap with the other two surfaces is
/// specifically the token ranking, not those four.
/// `tasqx report --outcomes` — D137's scorecard as one fitted table.
///
/// Every rate is printed as `count/n`, never as a percentage. That is the
/// ruling's own requirement made typographic: "3/12" carries its denominator
/// in the cell, and "25%" does not — and the difference between a rework rate
/// over twelve completions and one over three is the difference between a
/// figure a maintainer should act on and one they should ignore.
///
/// Same skeleton as [`report`] above (D117/D120): the key column is the one
/// that gives when the terminal is narrow, every other column is a number and
/// a number is never cut to fit.
pub fn outcomes(ctx: &Ctx, result: &Value, group_by: &str) -> String {
    let empty = Vec::new();
    let groups = result
        .get("groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if groups.is_empty() {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            // Not "no matching tasks": the scope test here is that a task
            // CLOSED, so an open backlog matching the filter perfectly still
            // lands here, and a reader told "no matching tasks" would go
            // looking for a filter bug that is not there.
            "No closed work in scope — outcomes are measured on tasks that finished.\n".to_string()
        };
    }

    // `a/b` — the count over the denominator it was computed against. A cell
    // with nothing to divide by prints `-`, not `0/0`.
    let over = |m: &Value, key: &str| -> String {
        let n = m.get("n").and_then(Value::as_i64).unwrap_or(0);
        if n <= 0 {
            return "-".to_string();
        }
        let c = m.get(key).and_then(Value::as_i64).unwrap_or(0);
        format!("{c}/{n}")
    };

    struct Row {
        key: String,
        rework: i64,
        cells: Vec<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut labels: Vec<String> = vec!["CLOSED".into(), "DONE".into()];
    let first = &groups[0];
    let has = |m: &str| first.get(m).is_some();
    if has("rework") {
        labels.push("REWORK".into());
    }
    if has("forced") {
        labels.push("FORCED".into());
    }
    if has("silent") {
        labels.push("SILENT".into());
    }
    if has("calibration") {
        labels.push("CALIB".into());
    }
    if has("abandonment") {
        labels.push("DROPPED".into());
    }
    if has("overrun") {
        labels.push("OVER".into());
    }
    if has("unproven") {
        labels.push("UNPROVEN".into());
    }
    if has("cost") {
        labels.push("TOKENS".into());
    }

    for g in groups {
        let key = san(g.get(group_by).and_then(Value::as_str).unwrap_or(""));
        let closed = g.get("closed").and_then(Value::as_i64).unwrap_or(0);
        let done = g.get("completions").and_then(Value::as_i64).unwrap_or(0);
        let mut cells = vec![closed.to_string(), done.to_string()];
        let mut rework_count = 0;
        if let Some(m) = g.get("rework") {
            rework_count = m.get("count").and_then(Value::as_i64).unwrap_or(0);
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("forced") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("silent") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("calibration") {
            let n = m.get("n").and_then(Value::as_i64).unwrap_or(0);
            // `×1.8 n7`: the ratio and the sample size it came from, in the
            // cell, for the same reason the rates carry their denominator. A
            // median over one completion is not a calibration.
            cells.push(match m.get("median_ratio").and_then(Value::as_f64) {
                Some(r) if n > 0 => format!("×{r:.2} n{n}"),
                _ => "-".to_string(),
            });
        }
        if let Some(m) = g.get("abandonment") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("overrun") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("unproven") {
            cells.push(over(m, "count"));
        }
        if let Some(m) = g.get("cost") {
            // The group's dominant bucket, the D92 cell — `cost` is keyed with
            // `report.summary`'s own bucket names precisely so this helper
            // reads it unchanged, and so the reader is never shown a blend.
            let cell = crate::tokens::dominant_cell(m);
            cells.push(match m.get("confidence").and_then(Value::as_str) {
                Some(c) if c != tasqx_core::tokens::CONFIDENCE_HIGH => format!("{cell} ~{c}"),
                _ => cell,
            });
        }
        rows.push(Row {
            key,
            rework: rework_count,
            cells,
        });
    }

    const MIN_KEY: usize = 8;
    const MAX_KEY: usize = 32;
    let header_label = group_by.to_uppercase();
    let key_w = rows
        .iter()
        .map(|r| width(&r.key))
        .chain([width(&header_label)])
        .max()
        .unwrap_or(0)
        .min(MAX_KEY);
    let mut cols = vec![Column::shrinks(key_w, MIN_KEY.min(key_w))];
    for (n, label) in labels.iter().enumerate() {
        let w = rows
            .iter()
            .map(|r| width(&r.cells[n]))
            .chain([width(label)])
            .max()
            .unwrap_or(0);
        cols.push(Column::fixed(w));
    }
    let w = columns::fit(&cols, ctx.cols);

    let line = |key: String, cells: Vec<(Option<&str>, String)>| {
        let mut parts = vec![key];
        for ((role, c), cw) in cells.into_iter().zip(&w[1..]) {
            if *cw == 0 {
                continue;
            }
            let pad = " ".repeat(cw.saturating_sub(width(&c)));
            let c = match role {
                Some(r) => ctx.paint(r, &c),
                None => c,
            };
            parts.push(format!("{pad}{c}"));
        }
        join_cells(parts)
    };

    let rework_col = labels.iter().position(|l| l == "REWORK");
    let mut out = ctx.paint(
        "table.label",
        &line(
            pad(&header_label, w[0]),
            labels.iter().map(|l| (None, l.clone())).collect(),
        ),
    );
    out.push('\n');
    for r in rows {
        // REWORK is the one figure here that is a problem when it is nonzero,
        // so it takes `warn` then and `muted` otherwise — the rule the OVERDUE
        // column already follows in `report`. Every other column is neutral:
        // a high SILENT count is a habit to fix, not an alarm, and DROPPED
        // work is often the right call.
        let cells = r
            .cells
            .into_iter()
            .enumerate()
            .map(|(n, c)| {
                let role =
                    (Some(n) == rework_col).then_some(if r.rework > 0 { "warn" } else { "muted" });
                (role, c)
            })
            .collect::<Vec<_>>();
        out.push_str(&line(cell(ctx, Some("project"), &r.key, w[0]), cells));
        out.push('\n');
    }

    // The legend earns its place for the same reason D92's does: `3/12` and
    // `×1.8 n7` are compact because they are dense, and a dense cell nobody
    // can read is not compact. Set off by a blank line (rule 7).
    out.push('\n');
    out.push_str(&prose(
        ctx,
        Some("muted"),
        "REWORK / FORCED / SILENT / DROPPED / OVER / UNPROVEN read count over the completions \
         or closings they were counted against — a rate with no denominator beside it is not \
         a rate. OVER counts only the completions that had a budget and UNPROVEN only those \
         that had acceptance criteria. CALIB is the median tracked-over-estimate ratio and \
         the number of completions carrying both figures.",
        "",
    ));
    out
}

pub fn report(
    ctx: &Ctx,
    result: &Value,
    group_by: &str,
    token_metrics: Option<&[String]>,
) -> String {
    let empty = Vec::new();
    let groups = result
        .get("groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if groups.is_empty() {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            "No matching tasks.\n".to_string()
        };
    }

    let show_all_tokens = token_metrics.is_some_and(|m| {
        m.iter().any(|s| {
            matches!(
                s.as_str(),
                "tokens_in" | "tokens_out" | "tokens_cache_read" | "tokens_cache_creation"
            )
        })
    });

    // Totals (#234 item 5), accumulated alongside the rows — the report
    // already holds every group before rendering, so this cannot drift from
    // what the rows above it show the way a second, independent sum could.
    let mut total_count = 0i64;
    let mut total_est_secs = 0i64;
    let mut total_tracked_secs = 0i64;
    let mut total_overdue = 0i64;
    let mut total_bucket = std::collections::HashMap::<&str, i64>::new();
    let mut any_tokens = false;
    let mut total_confidence: Option<&str> = None;

    // #217: `report.summary` names the group's WORST confidence in
    // `tokens_confidence` (D50's trust hierarchy). Silent when it is
    // `high`, or absent — a marker on the common case teaches the reader
    // to stop noticing it.
    let mark_confidence = |cell: String, confidence: Option<&str>| match confidence {
        Some(c) if c != tasqx_core::tokens::CONFIDENCE_HIGH => format!("{cell} ~{c}"),
        _ => cell,
    };
    // D167: with all four shown, an unsplit total gets its own column after
    // them — only when some group carries one, so every other table is as it was.
    let unsplit_of = |g: &Value| {
        g.get(crate::tokens::UNSPLIT.0)
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let show_unsplit = show_all_tokens && groups.iter().any(|g| unsplit_of(g) != 0);
    // The token cells of one row: all four buckets, or the one that dominates.
    let token_cells = |g: &Value, confidence: Option<&str>| -> Vec<String> {
        if show_all_tokens {
            let mut cells: Vec<String> = crate::tokens::BUCKETS
                .iter()
                .map(|(bkey, _, _)| {
                    crate::tokens::compact(g.get(*bkey).and_then(Value::as_i64).unwrap_or(0))
                })
                .collect();
            if show_unsplit {
                cells.push(crate::tokens::compact(unsplit_of(g)));
            }
            cells
        } else {
            vec![mark_confidence(crate::tokens::dominant_cell(g), confidence)]
        }
    };

    // Every cell is measured before any is printed, so the columns can be
    // sized to what they hold (#352). The numeric columns were a fixed 10 or
    // 12 cells whatever they held, so on a 60-column terminal the key was
    // crushed to `code-...` and the row still ran two cells past the edge.
    struct Row {
        key: String,
        overdue: i64,
        cells: Vec<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    for g in groups {
        let key = san(g.get(group_by).and_then(Value::as_str).unwrap_or(""));
        let count = g.get("count").and_then(Value::as_i64).unwrap_or(0);
        let est_iso = g.get("est_total").and_then(Value::as_str).unwrap_or("PT0S");
        let tracked_iso = g
            .get("tracked_total")
            .and_then(Value::as_str)
            .unwrap_or("PT0S");
        let overdue = g.get("overdue").and_then(Value::as_i64).unwrap_or(0);
        let confidence = g.get("tokens_confidence").and_then(Value::as_str);

        total_count += count;
        total_overdue += overdue;
        if let Some(s) = tasqx_core::util::duration_secs(est_iso) {
            total_est_secs = total_est_secs.saturating_add(s);
        }
        if let Some(s) = tasqx_core::util::duration_secs(tracked_iso) {
            total_tracked_secs = total_tracked_secs.saturating_add(s);
        }
        for (bkey, _, _) in crate::tokens::BUCKETS
            .iter()
            .chain([&crate::tokens::UNSPLIT])
        {
            let n = g.get(bkey).and_then(Value::as_i64).unwrap_or(0);
            if n != 0 {
                any_tokens = true;
            }
            *total_bucket.entry(bkey).or_insert(0) += n;
        }
        if let Some(c) = confidence {
            let worse = total_confidence.is_none_or(|cur| {
                tasqx_core::tokens::confidence_rank(c) < tasqx_core::tokens::confidence_rank(cur)
            });
            if worse {
                total_confidence = Some(c);
            }
        }

        let mut cells = vec![
            count.to_string(),
            human_or_dash(est_iso),
            overdue.to_string(),
            human_or_dash(tracked_iso),
        ];
        cells.extend(token_cells(g, confidence));
        rows.push(Row {
            key,
            overdue,
            cells,
        });
    }

    // The totals row — same columns, same widths, "TOTAL" where a group name
    // would be.
    let total_row = json!({
        "tokens_cache_read": total_bucket.get("tokens_cache_read").copied().unwrap_or(0),
        "tokens_cache_creation": total_bucket.get("tokens_cache_creation").copied().unwrap_or(0),
        "tokens_in": total_bucket.get("tokens_in").copied().unwrap_or(0),
        "tokens_out": total_bucket.get("tokens_out").copied().unwrap_or(0),
        "tokens_unsplit": total_bucket.get("tokens_unsplit").copied().unwrap_or(0),
    });
    let mut total_cells = vec![
        total_count.to_string(),
        human_or_dash(&tasqx_core::util::iso_duration(total_est_secs)),
        total_overdue.to_string(),
        human_or_dash(&tasqx_core::util::iso_duration(total_tracked_secs)),
    ];
    total_cells.extend(token_cells(&total_row, total_confidence));

    let header_label = group_by.to_uppercase();
    let mut labels = vec![
        "COUNT".to_string(),
        "EST".to_string(),
        "OVERDUE".to_string(),
        "TRACKED".to_string(),
    ];
    if show_all_tokens {
        labels.extend(
            crate::tokens::BUCKETS
                .iter()
                .map(|(_, short, _)| short.to_uppercase()),
        );
        if show_unsplit {
            labels.push(crate::tokens::UNSPLIT.1.to_uppercase());
        }
    } else {
        labels.push("TOKENS".to_string());
    }

    // #234 item 2: the group_by column was once a hardcoded 20 cells, and a
    // name past it pushed every column after it to the right. It is sized to
    // its names, capped, and it is the one column that gives when the
    // terminal is narrow. Every other column is a number, and a number cut to
    // fit is a different number, while one dropped to fit hides a bucket the
    // reader may have asked for by name (`--metrics tokens_in`). Past the
    // key's floor the row overflows.
    const MIN_KEY: usize = 8;
    const MAX_KEY: usize = 32;
    let key_w = rows
        .iter()
        .map(|r| width(&r.key))
        .chain([width(&header_label), width("TOTAL")])
        .max()
        .unwrap_or(0)
        .min(MAX_KEY);
    let mut cols = vec![Column::shrinks(key_w, MIN_KEY.min(key_w))];
    for (n, label) in labels.iter().enumerate() {
        let w = rows
            .iter()
            .map(|r| width(&r.cells[n]))
            .chain([width(label), width(&total_cells[n])])
            .max()
            .unwrap_or(0);
        cols.push(Column::fixed(w));
    }
    let w = columns::fit(&cols, ctx.cols);

    // One line: the key cell, then every other cell right-aligned. The padding
    // goes OUTSIDE any paint, so `join_cells` can trim the end and the
    // escapes never count as width.
    let line = |key: String, cells: Vec<(Option<&str>, String)>| {
        let mut parts = vec![key];
        for ((role, c), cw) in cells.into_iter().zip(&w[1..]) {
            if *cw == 0 {
                continue;
            }
            let pad = " ".repeat(cw.saturating_sub(width(&c)));
            let c = match role {
                Some(r) => ctx.paint(r, &c),
                None => c,
            };
            parts.push(format!("{pad}{c}"));
        }
        join_cells(parts)
    };
    let plain = |cells: Vec<String>| cells.into_iter().map(|c| (None, c)).collect::<Vec<_>>();

    let mut out = ctx.paint(
        "table.label",
        &line(pad(&header_label, w[0]), plain(labels)),
    );
    out.push('\n');
    // OVERDUE is `warn` when there is any and `muted` when there is none, on
    // the TOTAL row as on every other.
    let with_overdue = |overdue: i64, cells: Vec<String>| {
        let role = if overdue > 0 { "warn" } else { "muted" };
        cells
            .into_iter()
            .enumerate()
            .map(|(n, c)| ((n == 2).then_some(role), c))
            .collect::<Vec<_>>()
    };
    for r in rows {
        let cells = with_overdue(r.overdue, r.cells);
        out.push_str(&line(cell(ctx, Some("project"), &r.key, w[0]), cells));
        out.push('\n');
    }
    // Set off by a blank line (rule 7): in `mono` and under NO_COLOR the
    // dim label alone did not tell TOTAL from a group named in capitals.
    out.push('\n');
    // A row with a label, not a title (#346, `docs/maintainers/terminal-style.md`
    // rule 12). The whole line was painted `header`, the role for `TASQX
    // MANUAL` and a task's own name, so the sums shouted over the rows they
    // sum. `TOTAL` is structure and takes the column labels' role; the
    // figures print as every other row's do.
    out.push_str(&line(
        cell(ctx, Some("table.label"), "TOTAL", w[0]),
        with_overdue(total_overdue, total_cells),
    ));
    out.push('\n');

    // Footnotes — printed only when they have something to say, set off from
    // the table by a blank line (rule 7: a note flush against the last row
    // reads as a row whose columns broke), and wrapped at words to the
    // terminal: the legend is one 171-cell sentence, which a narrow terminal
    // otherwise broke mid-word on its own (#352).
    let mut first_note = true;
    let mut footnote = |out: &mut String, text: &str| {
        if std::mem::take(&mut first_note) {
            out.push('\n');
        }
        out.push_str(&prose(ctx, Some("muted"), text, ""));
    };
    if any_tokens && !show_all_tokens {
        // #212 (D48a, challenges-design — see the commit and the report for
        // the reasoning): the cell above is one bucket of four, ranked by
        // volume rather than a priced ratio (D48a's own wording), and until
        // now the terminal gave no way to see the other three at all.
        footnote(
            &mut out,
            "TOKENS shows the largest of four buckets (cacheR/cacheW/in/out) by volume; \
             --metrics tokens_in,tokens_out,tokens_cache_read,tokens_cache_creation or --html shows all four.",
        );
    }
    let excluded = result
        .get("tokens_excluded_cancelled_tasks")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if excluded > 0 {
        // #234 item 12: D24 excludes cancelled tasks from this report by
        // default (right for counting abandoned work; silent about the spend
        // already incurred on it, which this line stops being silent about).
        footnote(
            &mut out,
            &if excluded == 1 {
                "1 cancelled task excluded; --all includes its spend.".to_string()
            } else {
                format!("{excluded} cancelled tasks excluded; --all includes their spend.")
            },
        );
    }
    out
}

/// The four buckets as `tokens.recompute` spells them in `before`/`after`,
/// in the same reading order `task_detail`'s tokens line uses. NOT
/// `crate::tokens::BUCKETS`: those are `report.summary`'s aggregate keys
/// (`tokens_in`, …) ordered by price for picking a dominant bucket, and this
/// is a per-task delta over the measurement-row keys, where the plan's own
/// example reads input-first.
pub(crate) const RECOMPUTE_BUCKETS: [(&str, &str); 4] = [
    ("input_tokens", "in"),
    ("output_tokens", "out"),
    ("cache_read_tokens", "cacheR"),
    ("cache_creation_tokens", "cacheW"),
];

/// `tasqx tokens recompute` — the migration delta a user reads before granting
/// `--apply` (D50 Decision 3).
///
/// One line per CHANGED task; `unchanged` tasks are counted in the totals line
/// but not listed, because the list exists to answer "who loses what" and a
/// task losing nothing would bury the ones that do. Counts are printed raw,
/// never `tokens::compact`ed: this is the one surface whose numbers someone
/// must be able to audit against `--json` before approving a deletion.
///
/// The totals line sums every task's `before`/`after` bucket maps and renders
/// them through the same [`bucket_delta`] the per-task lines use (#219) —
/// deliberately NOT the engine's blended `totals.before`/`totals.after`
/// figure, which is exactly the aggregate this module's header exists to
/// forbid and cannot even detect the failure it is meant to catch: a repair
/// that drops 200k output tokens and adds 200k cache-read tokens nets to the
/// SAME blended number on both sides and reads as a no-op, having swapped the
/// cheapest bucket for one of the most expensive.
/// The four buckets as one line, and the D167 unsplit total beside them —
/// alone when nothing was reported split, so a total-only task does not read
/// as four zeroes and a number.
pub(crate) fn token_figures(sum: impl Fn(&str) -> u64) -> String {
    let split = [
        sum("input_tokens"),
        sum("output_tokens"),
        sum("cache_read_tokens"),
        sum("cache_creation_tokens"),
    ];
    let unsplit = sum("total_tokens");
    let four = format!(
        "in {} · out {} · cacheR {} · cacheW {}",
        split[0], split[1], split[2], split[3]
    );
    match (split.iter().all(|n| *n == 0), unsplit) {
        (_, 0) => four,
        (true, t) => format!("total (unsplit) {t}"),
        (false, t) => format!("{four} · total (unsplit) {t}"),
    }
}

/// `tasqx tokens add` (D167): what was recorded, on which task, graded how.
pub fn token_added(result: &Value) -> String {
    let m = &result["measurement"];
    format!(
        "Recorded on #{}: {} · {}, {} confidence\n",
        result["short_id"].as_i64().unwrap_or(0),
        token_figures(|k| m.get(k).and_then(Value::as_u64).unwrap_or(0)),
        san(m["source"].as_str().unwrap_or("?")),
        san(m["confidence"].as_str().unwrap_or("?")),
    )
}

pub fn tokens_recompute(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let dry_run = result
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if tasks.is_empty() {
        return "No log-parse measurements to recompute.\n".to_string();
    }

    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!(
            "Token recompute ({})",
            if dry_run { "dry-run" } else { "applied" }
        ),
    ));
    out.push('\n');

    let mut unchanged = 0usize;
    for t in tasks {
        let sid = t.get("task").and_then(Value::as_i64).unwrap_or(0);
        let action = t.get("action").and_then(Value::as_str).unwrap_or("");
        let cell = |side: &str| -> &Value { t.get(side).unwrap_or(&Value::Null) };
        let line = match action {
            "unchanged" => {
                unchanged += 1;
                continue;
            }
            "recomputed" => format!(
                "recomputed   {}",
                bucket_delta(cell("before"), cell("after"))
            ),
            "downgraded" => {
                // The result carries counts, not confidences; the engine only
                // ever downgrades TO low, so the destination is safe to name
                // and the origin is not restated.
                "downgraded   confidence -> low (transcript unreadable; counts kept)".to_string()
            }
            "channel_conflict" => {
                "conflict     log-parse rows removed; the self-report is the measurement"
                    .to_string()
            }
            // A verb action this build has not heard of: show it rather than
            // silently dropping a task from a report about deletions.
            other => format!("{other}   {}", bucket_delta(cell("before"), cell("after"))),
        };
        out.push_str(&format!(
            "  {:>5}  {line}\n",
            ctx.paint("accent", &format!("#{sid}"))
        ));
    }

    out.push_str(&format!(
        "totals  {}  ·  {} task(s) in scope, {unchanged} unchanged\n",
        bucket_delta(&sum_buckets(tasks, "before"), &sum_buckets(tasks, "after")),
        tasks.len()
    ));
    if dry_run {
        out.push_str("Dry-run: nothing was written. Run `tasqx tokens recompute --apply` to perform this repair.\n");
    }
    out
}

/// Sum every task's `side` bucket map (`"before"` or `"after"`) across the
/// whole report, so the totals line carries the same four buckets the
/// per-task lines do rather than blending them into the one number #219
/// flags — a `channel_conflict` task's `null` "after" reads as zero in every
/// bucket, same as [`bucket_delta`] already treats it per task.
pub(crate) fn sum_buckets(tasks: &[Value], side: &str) -> Value {
    let mut totals = serde_json::json!({});
    for (key, _) in RECOMPUTE_BUCKETS {
        let sum: i64 = tasks
            .iter()
            .map(|t| {
                t.get(side)
                    .and_then(|v| v.get(key))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            })
            .fold(0i64, i64::saturating_add);
        totals[key] = serde_json::json!(sum);
    }
    totals
}

/// `in 1500->500 · out 2600->600` — only the buckets that carry a number on
/// either side; a bucket at 0->0 is noise on every line it would join. `after`
/// may be the null a `channel_conflict` reports, which reads as 0.
pub(crate) fn bucket_delta(before: &Value, after: &Value) -> String {
    let n = |v: &Value, key: &str| v.get(key).and_then(Value::as_i64).unwrap_or(0);
    let cells: Vec<String> = RECOMPUTE_BUCKETS
        .iter()
        .filter_map(|(key, label)| {
            let (b, a) = (n(before, key), n(after, key));
            (b != 0 || a != 0).then(|| format!("{label} {b}->{a}"))
        })
        .collect();
    if cells.is_empty() {
        // All-zero rows exist only in strange stores, but a blank cell would
        // read as a rendering bug rather than an empty measurement.
        return "empty".to_string();
    }
    cells.join(" · ")
}

#[cfg(test)]
mod tests;
