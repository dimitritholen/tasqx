//! Full task detail (`task.get`), `brief`'s prerequisite summaries and the
//! interactive `show` card (task #727 split of `render.rs`).

use super::*;

use jiff::tz::TimeZone;
use jiff::Timestamp;
use serde_json::Value;
use tasqx_core::filter::overdue_at;

use crate::theme::Ctx;

/// A note printed under a screen, wrapped at words to the terminal and
/// painted in `role`, each line prefixed with `indent` (#346).
///
/// The prose under `agenda`, `next`, `report` and `memory search` used to be
/// printed at whatever length it was written, and the longest of them, the
/// onboarding hint, ran to 100 cells: a 60-column terminal broke it mid-word.
/// Every such note goes through here, so none of them can come to wrap on its
/// own again.
///
/// A command quoted in backticks is one word to the wrap: split across two
/// lines it is a command nobody can paste, and these notes exist to hand the
/// reader one.
pub(crate) fn prose(ctx: &Ctx, role: Option<&str>, text: &str, indent: &str) -> String {
    /// Stands in for a space inside a quoted span while the line is wrapped. A
    /// private-use character, so `split_whitespace` cannot see it and no note
    /// can contain it.
    const HELD: char = '\u{E000}';
    // Only PAIRED backticks open and close a span. A stray one (raw store
    // text, a query as typed) would otherwise hold the rest of the note as
    // one word, and the note would run past the terminal it exists to fit.
    let mut toggles = text.matches('`').count() / 2 * 2;
    let mut quoted = false;
    let held: String = text
        .chars()
        .map(|c| {
            if c == '`' && toggles > 0 {
                toggles -= 1;
                quoted = !quoted;
            }
            if quoted && c == ' ' {
                HELD
            } else {
                c
            }
        })
        .collect();
    let mut out = String::new();
    for line in wrap_words(&held, ctx.cols.saturating_sub(width(indent))) {
        let line = line.replace(HELD, " ");
        out.push_str(indent);
        match role {
            Some(r) => out.push_str(&ctx.paint(r, &line)),
            None => out.push_str(&line),
        }
        out.push('\n');
    }
    out
}

/// True when the core flagged this task's `status` as text it could not
/// recognize. Reads the explicit boolean rather than re-parsing the string, so
/// one answer comes from the core and the CLI does not grow a second opinion.
pub(crate) fn status_is_unrecognized(t: &Value) -> bool {
    t.get("status_unrecognized")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Full task detail (task.get): fields plus tags, deps, annotations, blocked.
/// The status cell both detail layouts print: the plain string, unless the
/// wire sent a status this build cannot parse — then say so, and say what the
/// real ones are. The list of real ones is derived, never retyped:
/// `Status::ALL` is the canonical list. One helper, because the card layout
/// (D76) renders the same fact and two copies of the unrecognized branch is
/// how they would drift apart.
///
/// `now` is a parameter rather than a `Timestamp::now()` call inside — the same
/// reason [`task_table`] takes one: relative rendering (`detail.time_format`)
/// must be reproducible and testable, not a hidden clock.
pub(crate) fn status_cell(ctx: &Ctx, result: &Value) -> String {
    if status_is_unrecognized(result) {
        ctx.paint(
            "warn",
            &format!(
                "{}  (unrecognized — not one of {})",
                s(result, "status"),
                tasqx_core::types::Status::ALL
                    .map(tasqx_core::types::Status::as_str)
                    .join(", ")
            ),
        )
    } else {
        s(result, "status")
    }
}

pub fn task_detail(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    // The card is the interactive rendering (D76); everything below it is the
    // byte-stable plain layout every pipe, script and docs example reads.
    if ctx.caps.unicode {
        return task_detail_card(ctx, result, now);
    }
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let mut out = String::new();
    out.push_str(&ctx.paint("header", &format!("#{sid}  {}", s(result, "title"))));
    out.push('\n');
    for row in detail_rows(ctx, result, now) {
        if matches!(row.field, DetailField::Annotation) {
            out.push_str(&format!("  {} {}\n", ctx.paint("muted", "·"), row.value));
            continue;
        }
        // D138: the marker carries the state, so it is painted and the
        // criterion is not — a failed check should catch the eye at the marker
        // rather than shouting its whole line.
        if matches!(row.field, DetailField::Check) {
            let role = match row.label {
                "[x]" => "ok",
                "[!]" => "danger",
                _ => "muted",
            };
            out.push_str(&format!("  {} {}\n", ctx.paint(role, row.label), row.value));
            continue;
        }
        // The plain layout's emphasis map over the SAME row set the card
        // renders — the facts and their conditions live in `detail_rows`.
        let cell = match row.field {
            DetailField::Project => ctx.paint("project", &row.value),
            DetailField::Remind | DetailField::Repeats => ctx.paint("accent", &row.value),
            DetailField::Blocked => ctx.paint("danger", &row.value),
            DetailField::Tags => ctx.paint("tag", &row.value),
            _ => row.value,
        };
        out.push_str(&format!("  {:<11}{cell}\n", row.label));
    }
    out
}

/// `tasqx brief` (D136): `show`'s card, then what the prerequisites concluded,
/// then what the store already knows about this subject.
///
/// The task half is [`task_detail`] unchanged, for D49's reason one surface
/// over: one task reads the same however it was asked for, and a second layout
/// for the same object is how two screens start disagreeing about one row.
/// Each section is omitted when empty rather than printed with "none" under it
/// — house style rule 12, and a brief is read before work starts, so a heading
/// that says nothing costs attention at the worst moment.
pub fn task_brief(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let mut out = task_detail(ctx, result.get("task").unwrap_or(result), now);

    let list = |key: &str| -> Vec<Value> {
        result
            .get("neighbourhood")
            .and_then(|v| v.get(key))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let heading = |out: &mut String, text: &str| {
        out.push('\n');
        out.push_str(&ctx.paint("table.label", text));
        out.push('\n');
    };
    let ref_line = |v: &Value| {
        let sid = v.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        format!(
            "  {}  {}  {}",
            ctx.paint("accent", &format!("#{sid}")),
            san(&s(v, "title")),
            ctx.paint("muted", &san(&s(v, "status")))
        )
    };

    // D170: what the previous occurrence of a recurring task delivered.
    if let Some(last) = result.get("last_time").filter(|v| !v.is_null()) {
        heading(&mut out, "LAST TIME");
        let sid = last.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        out.push_str(&format!(
            "  {}  {}\n",
            ctx.paint("accent", &format!("#{sid}")),
            san(&s(last, "title"))
        ));
        for line in wrap_words(
            &san(&s(last, "delivered")),
            ctx.cols.saturating_sub(6).max(20),
        ) {
            out.push_str(&format!("    {}\n", ctx.paint("muted", &line)));
        }
    }

    let depends_on = list("depends_on");
    if !depends_on.is_empty() {
        heading(&mut out, "DEPENDS ON");
        for d in &depends_on {
            out.push_str(&ref_line(d));
            out.push('\n');
            // The prerequisite's last word, wrapped to the terminal under it.
            // This is the row the section exists for: what the upstream task
            // concluded is what a reader needs before starting, and reading it
            // used to cost a second `tasqx show`.
            if let Some(a) = d.get("annotation").filter(|a| !a.is_null()) {
                for line in wrap_words(&san(&s(a, "body")), ctx.cols.saturating_sub(6).max(20)) {
                    out.push_str(&format!("    {}\n", ctx.paint("muted", &line)));
                }
            }
        }
    }

    let blocks = list("blocks");
    if !blocks.is_empty() {
        heading(&mut out, "BLOCKS");
        for b in &blocks {
            out.push_str(&ref_line(b));
            out.push('\n');
        }
    }

    let hits = result
        .get("memory")
        .and_then(|m| m.get("hits"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !hits.is_empty() {
        heading(&mut out, "FROM MEMORY");
        for h in &hits {
            let source = s(h, "source");
            // #657: which project this hit is scoped to — empty (global, or
            // an older recorded response with no `project` key) prints
            // nothing, since a bracket that never says anything is noise.
            let project = s(h, "project");
            // #790/D180: nothing when false or null — an annotation and a
            // doc `memory.add` wrote both have no origin to be behind.
            let stale = h.get("stale").and_then(Value::as_bool).unwrap_or(false);
            out.push_str(&format!(
                "  {}{}{}{}\n",
                san(&s(h, "title")),
                if source.is_empty() {
                    String::new()
                } else {
                    format!("  {}", ctx.paint("muted", &san(&source)))
                },
                if project.is_empty() {
                    String::new()
                } else {
                    format!("  {}", ctx.paint("muted", &format!("[{}]", san(&project))))
                },
                if stale {
                    format!("  {}", ctx.paint("muted", "stale"))
                } else {
                    String::new()
                }
            ));
            let snippet = san(&s(h, "snippet"));
            if !snippet.is_empty() {
                for line in wrap_words(&snippet, ctx.cols.saturating_sub(6).max(20)) {
                    out.push_str(&format!("    {}\n", ctx.paint("muted", &line)));
                }
            }
        }
        // D70's rule one surface over: a bounded page that does not say it is
        // bounded is read as the whole answer.
        let total = result
            .get("memory")
            .and_then(|m| m.get("total"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if total > hits.len() as i64 {
            out.push('\n');
            out.push_str(&prose(
                ctx,
                Some("muted"),
                &format!(
                    "{} of {total} matches shown — tasqx memory search reaches the rest.",
                    hits.len()
                ),
                "",
            ));
        }
    }
    out
}

/// An instant as a detail view spells it (D122): the stored text under
/// `detail.time_format = iso`, and otherwise the calendar day `list` would
/// print (`Wed 16 Sep`, `today 17:00`, `3d ago`), followed by the elapsed
/// words where they add something (`Wed 16 Sep (in 5 days)`).
///
/// It used to be `fmt_instant`'s spelling: the raw instant, nanoseconds
/// included (`2026-09-11T09:36:45.809321632Z (just now)`), under the default
/// `both`. That is rule 3 broken on the one screen that exists to read a
/// task's dates, and the width it cost is what made `show` wrap at 60
/// columns. The MCP text view keeps core's spelling; this is the terminal's.
pub(crate) fn instant_value(ctx: &Ctx, v: &str, now: Timestamp) -> String {
    use tasqx_core::markdown::{fmt_instant, DetailOpts, TimeFormat};
    let Ok(at) = v.parse::<Timestamp>() else {
        return v.to_string();
    };
    if ctx.time_format == TimeFormat::Iso {
        return v.to_string();
    }
    let day = if at >= now {
        due_cell(at, now)
    } else {
        let today = now.to_zoned(TimeZone::UTC).date() == at.to_zoned(TimeZone::UTC).date();
        if today {
            due_cell(at, now)
        } else {
            day_ago(at, now)
        }
    };
    if ctx.time_format == TimeFormat::Relative {
        return day;
    }
    // `both`: the elapsed words, unless the day already is elapsed words
    // (`yesterday`, `3d ago`), where they would say it twice.
    let days = (at.as_second() - now.as_second()).abs() / 86_400;
    if at < now && days < 7 && !day.starts_with("today") {
        return day;
    }
    let rel = fmt_instant(
        v,
        &DetailOpts {
            time: TimeFormat::Relative,
            now,
        },
    );
    format!("{day} ({rel})")
}

/// A duration as a detail view spells it (D122): `PT5H` under `iso`, `5h`
/// otherwise. `both` used to print `PT5H (5h)`, the same duration twice.
pub(crate) fn duration_value(ctx: &Ctx, v: &str, now: Timestamp) -> String {
    use tasqx_core::markdown::{fmt_duration, DetailOpts, TimeFormat};
    let time = if ctx.time_format == TimeFormat::Iso {
        TimeFormat::Iso
    } else {
        TimeFormat::Relative
    };
    fmt_duration(v, &DetailOpts { time, now })
}

/// Which fact a detail row names, so each layout can map the same row set to
/// its own emphasis without restating the row conditions.
pub(crate) enum DetailField {
    Status,
    Project,
    Urgency,
    Due,
    Remind,
    Scheduled,
    Wait,
    Repeats,
    Estimate,
    Completed,
    Tracked,
    Blocked,
    Tags,
    DependsOn,
    Blocks,
    Created,
    Modified,
    Rev,
    Budget,
    Tokens,
    Check,
    Annotation,
}

/// One row of a task detail: the label spelling both layouts print, the
/// rendered value, and which fact it is.
pub(crate) struct DetailRow {
    label: &'static str,
    field: DetailField,
    value: String,
}

/// The rows a task detail names, in order, CONDITIONS INCLUDED — one walk for
/// both layouts. The plain and card views used to keep ~18 conditions in sync
/// by hand (`tracked != "PT0S"`, non-empty `completed`, …) with a parity test
/// that checked label presence only against an all-fields fixture, so a
/// condition drifting in one layout passed it. A row that renders in one
/// layout and not the other is unrepresentable now.
pub(crate) fn detail_rows(ctx: &Ctx, result: &Value, now: Timestamp) -> Vec<DetailRow> {
    // `detail.time_format` (D49's "on the one retreat", now reaching `show` —
    // see `Ctx::with_time_format`), read through the same pure formatters
    // `tasqx_get_task` uses, so `PT4H` becomes `4h` and an ISO instant becomes
    // `Thu 10 Sep` / `in 2 days` identically on both surfaces. Only the
    // *values* converge here; the layout stays each renderer's own (D78's rail
    // card is untouched by this).
    // D132: every clock on this screen is UTC, and the first one that shows a
    // clock says so — once, after the clock, not on every cell. `iso` prints
    // the stored `…Z`, which already says it, and a card with no clock on it
    // has nothing to mark.
    let marked = std::cell::Cell::new(false);
    let fmt_i = |v: &str| {
        let out = instant_value(ctx, v, now);
        if marked.get()
            || ctx.time_format == tasqx_core::markdown::TimeFormat::Iso
            || v.parse::<Timestamp>().is_err()
        {
            return out;
        }
        let (day, rest) = out.split_at(out.find(" (").unwrap_or(out.len()));
        if !day.contains(':') {
            return out;
        }
        marked.set(true);
        format!("{day} UTC{rest}")
    };
    let fmt_d = |v: &str| duration_value(ctx, v, now);

    let mut rows = Vec::new();
    let mut row = |label: &'static str, field: DetailField, value: String| {
        rows.push(DetailRow {
            label,
            field,
            value,
        });
    };

    // D122: a running task says so in its status, not on a `running` row of
    // its own beside `status active`, which said the same thing twice. Since
    // D166 `tracked` includes the open interval, and the moment it started
    // rides on the status: that is what tells "running" from "done running".
    let mut status = status_cell(ctx, result);
    if !s(result, "active_since").is_empty() && !status_is_unrecognized(result) {
        status = format!("{status} · since {}", fmt_i(&s(result, "active_since")));
    }
    row("status", DetailField::Status, status);
    // D122: blocked is a line only when it is true, and it names what blocks
    // the task. It used to be `blocked true` beside `depends_on #51`, two rows
    // for one fact, and `blocked false` on every other task, which is noise.
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if blocked {
        let empty = Vec::new();
        let blockers: Vec<String> = result
            .get("unmet_blockers")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
            .iter()
            .map(|b| {
                let sid = b.get("short_id").and_then(Value::as_i64).unwrap_or(0);
                let title = san(b.get("title").and_then(Value::as_str).unwrap_or(""));
                if title.is_empty() {
                    format!("#{sid}")
                } else {
                    format!("#{sid} · {title}")
                }
            })
            .collect();
        let named = if blockers.is_empty() {
            // An older core, or a caller that trimmed the field: the edge is
            // still in `depends_on`.
            result
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|d| {
                    d.iter()
                        .filter_map(Value::as_i64)
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        } else {
            blockers.join(", ")
        };
        row("blocked", DetailField::Blocked, format!("by {named}"));
    }
    // D122 (rule 5): priority sits in the urgency cell it modifies, as in
    // `list`, rather than on a row of its own describing a number two rows
    // away.
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    let prio = result
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("-");
    row("urgency", DetailField::Urgency, format!("{prio} {urg:.1}"));
    if !s(result, "project").is_empty() {
        row("project", DetailField::Project, s(result, "project"));
    }
    if !s(result, "due").is_empty() {
        row("due", DetailField::Due, fmt_i(&s(result, "due")));
    }
    if !s(result, "remind").is_empty() {
        row("remind", DetailField::Remind, fmt_i(&s(result, "remind")));
    }
    if !s(result, "scheduled").is_empty() {
        row(
            "scheduled",
            DetailField::Scheduled,
            fmt_i(&s(result, "scheduled")),
        );
    }
    if !s(result, "wait").is_empty() {
        row("wait", DetailField::Wait, fmt_i(&s(result, "wait")));
    }
    if !s(result, "recurrence").is_empty() {
        // D170: a spawn names the occurrence it came from.
        let repeats = match result.get("spawned_from").and_then(Value::as_i64) {
            Some(from) => format!("{} from #{from}", s(result, "recurrence")),
            None => s(result, "recurrence"),
        };
        row("repeats", DetailField::Repeats, repeats);
    }
    if !s(result, "estimate").is_empty() {
        row(
            "estimate",
            DetailField::Estimate,
            fmt_d(&s(result, "estimate")),
        );
    }
    // Conditional for the reason `tracked` is: only a closed task HAS a
    // completion moment, and an empty `completed` row on every pending task is
    // noise. It was stored, returned by `task.get` and rendered by `done` — the
    // one surface that scrolls away — so the detail view, whose whole job is
    // showing a task's fields, was the only place the moment could be looked up
    // later and the only place it did not appear.
    if !s(result, "completed").is_empty() {
        row(
            "completed",
            DetailField::Completed,
            fmt_i(&s(result, "completed")),
        );
    }
    // Conditional, unlike `blocked`: every task has a blocked answer worth
    // reading, but "tracked PT0S" on the many tasks that were never timed is
    // noise on the detail of every one of them. Shown from the first tracked
    // second onward, which is when the number starts meaning something.
    let tracked = s(result, "tracked");
    if !tracked.is_empty() && tracked != "PT0S" {
        // D166: the net of the `adjust` corrections inside the total, signed.
        let adj = s(result, "tracked_adjustment");
        let value = match adj.strip_prefix('-') {
            _ if adj.is_empty() || adj == "PT0S" => fmt_d(&tracked),
            Some(m) => format!("{} (adjusted -{})", fmt_d(&tracked), fmt_d(m)),
            None => format!("{} (adjusted +{})", fmt_d(&tracked), fmt_d(&adj)),
        };
        row("tracked", DetailField::Tracked, value);
    }
    if let Some(tags) = result.get("tags").and_then(Value::as_array) {
        if !tags.is_empty() {
            // `+tag`, matching `list`'s table and its own filter grammar — see
            // the `tags:` field comment on `TaskRow` (#228.16).
            let names: Vec<String> = tags
                .iter()
                .filter_map(Value::as_str)
                .map(|t| format!("+{}", san(t)))
                .collect();
            row("tags", DetailField::Tags, names.join(" "));
        }
    }
    // Only while the task is not blocked: a blocked task's `blocked` line
    // already names what it waits on, and the edges to finished tasks no
    // longer stop anything.
    if let Some(deps) = result.get("depends_on").and_then(Value::as_array) {
        if !deps.is_empty() && !blocked {
            let refs: Vec<String> = deps
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            row("depends_on", DetailField::DependsOn, refs.join(" "));
        }
    }
    // The reverse edge (tasqx audit #159): `depends_on` names what blocks
    // this task, `blocks` names what THIS task blocks. Conditional like its
    // sibling above — a leaf that blocks nothing is the common case and an
    // empty row on every one of them would be noise.
    if let Some(blocks) = result.get("blocks").and_then(Value::as_array) {
        if !blocks.is_empty() {
            let refs: Vec<String> = blocks
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            row("blocks", DetailField::Blocks, refs.join(" "));
        }
    }
    // D139: the gauge, and only when a threshold was set — `fresh_tokens`
    // alone is a number with nothing to read it against, and the TOKENS row
    // below already says what was spent. Sits above `created` for the reason
    // `tracked` sits where it does: it is a fact about the work, not about the
    // row's bookkeeping.
    if let Some(budget) = result.get("budget_tokens").and_then(Value::as_i64) {
        let fresh = result
            .get("fresh_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let over = result.get("over").and_then(Value::as_bool).unwrap_or(false);
        row(
            "budget",
            DetailField::Budget,
            format!(
                "{} / {} fresh{}",
                crate::tokens::compact(fresh),
                crate::tokens::compact(budget),
                if over { " — over" } else { "" }
            ),
        );
    }
    // Always rendered, unconditionally — the same rule D49 states for
    // `tasqx_get_task`: `status`, `priority`, `project`, `created`, `modified`
    // and `_rev` are never the noisy zero a reader would want hidden, and
    // `_rev` in particular is the value `--expected-rev` needs, which used to
    // force a `--json` round trip just to read it back (audit #188).
    row(
        "created",
        DetailField::Created,
        fmt_i(&s(result, "created")),
    );
    row(
        "modified",
        DetailField::Modified,
        fmt_i(&s(result, "modified")),
    );
    row(
        "rev",
        DetailField::Rev,
        result
            .get("_rev")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .to_string(),
    );
    // D39: AI token spend renders here or it is data nobody reported.
    // Conditional like `tracked`: most tasks never get a measurement, and four
    // zeroes on every one of them is noise. Totals, not per-measurement rows —
    // the detail view answers "what did this task cost", and `--json` carries
    // the individual measurements for anyone who needs them.
    if let Some(tokens) = result.get("tokens").and_then(Value::as_array) {
        if !tokens.is_empty() {
            let sum = |key: &str| -> u64 {
                tokens
                    .iter()
                    .filter_map(|m| m.get(key).and_then(Value::as_u64))
                    .fold(0u64, u64::saturating_add)
            };
            // #217: the sum above blends every measurement's counts together
            // with no way back to which row contributed what, so the WORST
            // confidence across them is appended — the one fact a reader
            // needs to know before budgeting against this total. Silent when
            // every measurement is `high`: a marker on the common case would
            // train the reader to stop noticing it.
            let worst_confidence = tokens
                .iter()
                .filter_map(|m| m.get("confidence").and_then(Value::as_str))
                .min_by_key(|c| tasqx_core::tokens::confidence_rank(c));
            let confidence_suffix = match worst_confidence {
                Some(c) if c != tasqx_core::tokens::CONFIDENCE_HIGH => format!(" [{c} confidence]"),
                _ => String::new(),
            };
            row(
                "tokens",
                DetailField::Tokens,
                format!("{}{confidence_suffix}", token_figures(sum)),
            );
        }
    }
    // D138: the criteria, above the annotations and below the facts — each on
    // its own line with the state as a marker, and its evidence indented under
    // it, because a citation is only meaningful beside the claim it supports.
    if let Some(checks) = result.get("checks").and_then(Value::as_array) {
        for c in checks {
            let mark = match s(c, "state").as_str() {
                "passed" => "[x]",
                "failed" => "[!]",
                _ => "[ ]",
            };
            row(
                mark,
                DetailField::Check,
                san_multiline(c.get("body").and_then(Value::as_str).unwrap_or("")),
            );
            let evidence = s(c, "evidence");
            if !evidence.is_empty() {
                row("   ", DetailField::Check, san_multiline(&evidence));
            }
        }
    }
    if let Some(anns) = result.get("annotations").and_then(Value::as_array) {
        for a in anns {
            // #76.2: `san` neutralises a stray `\n` into a space because most
            // fields it guards (title, source, a search snippet) are meant to
            // be ONE line, and a newline in them is itself part of the hazard
            // (see `san`'s doc). An annotation body is not one of those — it
            // round-trips `\n` through storage and `task.get` intact, and
            // holds markdown a human wrote on purpose (headers, lists,
            // tables) — so it wants `san_multiline`, exactly the swap #195
            // already made for `memory show`.
            row(
                "·",
                DetailField::Annotation,
                san_multiline(a.get("body").and_then(Value::as_str).unwrap_or("")),
            );
        }
    }
    rows
}

// ============================================================================
// The task card (D76)
// ============================================================================
//
// The interactive body of `show` (the write echoes, `add`'s among them, are
// built in `echo`, D126). Gated on
// `caps.unicode` — true exactly when stdout is a VT-capable terminal — so
// every piped, dumb-terminal and legacy-console caller keeps the byte-stable
// plain rendering above, and a script diffing two runs sees what it always
// saw. Output-only formatting, so stdout alone decides; the dashboard demands
// stdin too because it reads keys, and that stricter gate must not be copied
// here. Tones come from the `card.*` roles — see `theme::builtin` for why
// they are deliberately achromatic in every built-in.

/// Which parts of a line of facts survive the width, by the one rule every
/// such line follows (`docs/maintainers/terminal-style.md` rule 9): the parts are taken
/// in rank order, lowest rank first, and the first that does not fit ends the
/// line, so nothing less important survives a part that was dropped.
/// Dropping says less; truncating mid-word would say something else. The
/// lowest-ranked part is always kept, since a line with nothing on it says
/// less than one that runs over.
///
/// `summary_line` ranks its parts in the order they print, which makes this
/// "drop from the right". `fit_facts` ranks `next`'s and `add`'s facts by what
/// the reader loses without each, which is not where each prints: the first
/// version took facts left to right, and a long project name pushed the
/// deadline off `next`'s line. A second version skipped a fact that did not
/// fit and carried on, so `next` kept the tags after dropping the project
/// (round 2 review of #346). There is one rule now, and it lives here.
pub(crate) fn keep_ranked(widths: &[usize], ranks: &[u8], sep: usize, budget: usize) -> Vec<bool> {
    let mut order: Vec<usize> = (0..widths.len()).collect();
    order.sort_by_key(|&i| ranks[i]);
    let mut keep = vec![false; widths.len()];
    let mut used = 0usize;
    for (n, i) in order.into_iter().enumerate() {
        let need = widths[i] + if n == 0 { 0 } else { sep };
        if n > 0 && used + need > budget {
            break;
        }
        keep[i] = true;
        used += need;
    }
    keep
}

/// A line of facts, three cells apart, fitted to `avail` cells by
/// [`keep_ranked`]. Each fact is `(rank, plain text for measuring, painted
/// text)` and is taken whole or not at all. `add`'s echo and `next` both fit
/// their facts here and rank the facts they share the same way (urgency,
/// deadline, then project and tags), which is how they cannot drift apart.
pub(crate) fn fit_facts(facts: Vec<(u8, String, String)>, avail: usize) -> String {
    let widths: Vec<usize> = facts.iter().map(|(_, plain, _)| width(plain)).collect();
    let ranks: Vec<u8> = facts.iter().map(|(rank, _, _)| *rank).collect();
    let keep = keep_ranked(&widths, &ranks, 3, avail);
    facts
        .into_iter()
        .zip(keep)
        .filter_map(|((_, _, painted), k)| k.then_some(painted))
        .collect::<Vec<_>>()
        .join("   ")
}

// The `show` card: the status-colored left rail (the C variant that
// superseded the D76 ledger — D78). One loud signal per card — the rail
// down the left edge, keyed by [`rail_role`] — with everything inside it
// quiet.

/// The rail's one decision: which role colors the edge. Blocked outranks
/// every status (it is the fact that stops the reader working), an
/// unrecognized status is the warning it is everywhere else, `active` gets
/// the timer's green, `pending` the theme accent, and the parked or closed
/// states recede into muted.
pub(crate) fn rail_role(result: &Value) -> &'static str {
    if result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "danger";
    }
    if status_is_unrecognized(result) {
        return "warn";
    }
    match s(result, "status").as_str() {
        "active" => "timer.active",
        "pending" => "accent",
        _ => "muted",
    }
}

/// Greedy word-wrap into lines of at most `max` cells, measured with
/// [`width`]. Wraps, never truncates: this feeds the card's title and
/// annotation blocks, which are the user's own words. A single word wider
/// than `max` gets its own overlong line rather than being cut — the
/// enclosing layout budgets generously enough that only pathological input
/// reaches that arm.
pub(crate) fn wrap_words(text: &str, max: usize) -> Vec<String> {
    wrap_pieces(text.split_whitespace().map(|word| (" ", word)), max)
}

/// The greedy fill behind [`wrap_words`], over pieces that each carry what
/// joins them to the piece before: a space between words, ` · ` between the
/// names in a list, nothing before a usage line's `|alternative`. The joiner
/// is dropped where a piece starts a line. Like `wrap_words` it never cuts: a
/// piece wider than `max` gets an overlong line of its own.
pub(crate) fn wrap_pieces<'a>(
    pieces: impl IntoIterator<Item = (&'a str, &'a str)>,
    max: usize,
) -> Vec<String> {
    wrap_hanging(pieces, max, max)
}

/// [`wrap_pieces`] with a first line of its own width: a synopsis whose
/// continuation lines hang further in than its first.
pub(crate) fn wrap_hanging<'a>(
    pieces: impl IntoIterator<Item = (&'a str, &'a str)>,
    first: usize,
    rest: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (joiner, piece) in pieces {
        if line.is_empty() {
            line = piece.to_string();
            continue;
        }
        let max = if lines.is_empty() { first } else { rest }.max(1);
        if width(&line) + width(joiner) + width(piece) <= max {
            line.push_str(joiner);
            line.push_str(piece);
        } else {
            lines.push(std::mem::take(&mut line));
            line = piece.to_string();
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn task_detail_card(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    // The label column: "depends_on" (the widest label) plus a two-cell gap.
    const LW: usize = 12;
    // Where the second column of a paired line starts.
    const COL2: usize = 30;
    let rail = ctx.paint(rail_role(result), "▌");
    // Everything renders inside the rail's two-cell gutter.
    let avail = ctx.cols.saturating_sub(2).max(20);
    let push = |out: &mut String, content: String| {
        out.push_str(&rail);
        if !content.is_empty() {
            out.push(' ');
            out.push_str(&content);
        }
        out.push('\n');
    };
    let lab = |name: &str| {
        format!(
            "{}{}",
            ctx.paint("card.label", name),
            " ".repeat(LW.saturating_sub(width(name)))
        )
    };
    let mut out = String::new();

    // Title block: `#N` in gray, the title strong and WRAPPED — the ledger
    // let a long title run past the terminal edge.
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let id = format!("#{sid}");
    let ind = width(&id) + 2;
    let title_w = avail.saturating_sub(ind).max(10);
    for (i, line) in wrap_words(&s(result, "title"), title_w).iter().enumerate() {
        let painted = ctx.paint("card.strong", line);
        if i == 0 {
            push(
                &mut out,
                format!("{}  {painted}", ctx.paint("card.label", &id)),
            );
        } else {
            push(&mut out, format!("{}{painted}", " ".repeat(ind)));
        }
    }
    push(&mut out, String::new());

    // The same row set the plain layout renders — the facts and their
    // conditions live in `detail_rows`, once. The card only re-maps emphasis
    // and geometry: short facts pair two to a line, long values wrap with a
    // hanging indent, annotations become their own block below.
    struct MetaCell {
        label: &'static str,
        value: String,
        role: Option<&'static str>,
        // The one pre-painted value (an unrecognized status arrives from
        // `status_cell` with its warn SGR already applied), whose width
        // cannot be measured — it is emitted alone and never wrapped.
        prepainted: bool,
        // A value painted here whose width is known from its plain form (the
        // urgency cell), so it can still pair with a neighbour.
        width: Option<usize>,
    }
    let unrecognized = status_is_unrecognized(result);
    let mut cells: Vec<MetaCell> = Vec::new();
    let mut annotations: Vec<String> = Vec::new();
    let mut checks: Vec<(&'static str, String)> = Vec::new();
    for r in detail_rows(ctx, result, now) {
        let (role, prepainted): (Option<&'static str>, bool) = match r.field {
            DetailField::Annotation => {
                annotations.push(r.value);
                continue;
            }
            // #714: a check is a sentence, not a fact — through the pairing
            // below it landed beside `rev`, and short ones filled the left
            // column. Its own block under the facts, like the notes.
            DetailField::Check => {
                checks.push((r.label, r.value));
                continue;
            }
            // D122: the one fact that stops the reader working opens the
            // card, under the title, in the rail's own colour: `⊘ blocked by
            // #51 · <title>`. A row among the others was the same fact at the
            // same weight as `created`.
            DetailField::Blocked => {
                let glyph = if ctx.caps.unicode { "⊘ " } else { "B " };
                push(
                    &mut out,
                    format!(
                        "{}{}",
                        ctx.paint("danger", &format!("{glyph}{}", r.label)),
                        ctx.paint("danger", &format!(" {}", r.value)),
                    ),
                );
                push(&mut out, String::new());
                continue;
            }
            // D122: `list`'s urgency cell, priority letter and gauge
            // included, so a task reads the same on both screens. Painted
            // here and measured from the plain value, whose width it shares
            // once the gauge's cells are added.
            DetailField::Urgency => {
                let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
                let prio = r.value.split(' ').next().unwrap_or("-").to_string();
                let prio_role = match prio.as_str() {
                    "H" => "priority.H",
                    "M" => "priority.M",
                    "L" => "priority.L",
                    _ => "muted",
                };
                let (bar, track) = urgency_meter(urgency_scale(urg));
                let ramp = ctx.theme.ramp_style(urgency_scale(urg));
                let painted = format!(
                    "{} {}{} {}",
                    ctx.paint(prio_role, &prio),
                    ramp.paint(&bar, &ctx.caps),
                    ctx.paint("muted", &track),
                    ramp.paint(&format!("{urg:.1}"), &ctx.caps),
                );
                let plain_w = width(&format!("{prio} {bar}{track} {urg:.1}"));
                cells.push(MetaCell {
                    label: r.label,
                    value: painted,
                    role: None,
                    prepainted: false,
                    width: Some(plain_w),
                });
                continue;
            }
            DetailField::Status => (None, unrecognized),
            // An open task past its deadline reads in `overdue`, as its row
            // does in `list`: the same fact in the same colour on both.
            DetailField::Due => (
                Some(
                    if field_ts(result, "due").is_some_and(|d| overdue_at(d, now))
                        && status_is_open(&s(result, "status"))
                    {
                        "overdue"
                    } else {
                        "card.strong"
                    },
                ),
                false,
            ),
            _ => (None, false),
        };
        cells.push(MetaCell {
            label: r.label,
            value: r.value,
            role,
            prepainted,
            width: None,
        });
    }

    let paint_val = |cell: &MetaCell, text: &str| -> String {
        match cell.role {
            Some(role) => ctx.paint(role, text),
            None => text.to_string(),
        }
    };
    let value_w = |c: &MetaCell| c.width.unwrap_or_else(|| width(&c.value));
    let mut i = 0;
    while i < cells.len() {
        let a = &cells[i];
        let aw = LW + value_w(a);
        // Pair two short facts on one line when both fit their columns.
        if !a.prepainted && aw + 3 <= COL2 && i + 1 < cells.len() {
            let b = &cells[i + 1];
            if !b.prepainted && COL2 + LW + value_w(b) <= avail {
                push(
                    &mut out,
                    format!(
                        "{}{}{}{}{}",
                        lab(a.label),
                        paint_val(a, &a.value),
                        " ".repeat(COL2 - aw),
                        lab(b.label),
                        paint_val(b, &b.value)
                    ),
                );
                i += 2;
                continue;
            }
        }
        if a.prepainted || a.width.is_some() || aw <= avail {
            push(
                &mut out,
                format!("{}{}", lab(a.label), paint_val(a, &a.value)),
            );
        } else {
            // A value wider than the line wraps under itself — wrapped, not
            // truncated, because the value is the user's own text.
            let vw = avail.saturating_sub(LW).max(10);
            for (j, line) in wrap_words(&a.value, vw).iter().enumerate() {
                if j == 0 {
                    push(&mut out, format!("{}{}", lab(a.label), paint_val(a, line)));
                } else {
                    push(
                        &mut out,
                        format!("{}{}", " ".repeat(LW), paint_val(a, line)),
                    );
                }
            }
        }
        i += 1;
    }

    // D138: one criterion per line, the marker painted as the plain layout
    // paints it, a long one wrapped under itself and its evidence under it.
    if !checks.is_empty() {
        push(&mut out, String::new());
        let cw = avail.saturating_sub(4).max(10);
        for (mark, body) in &checks {
            let evidence = mark.trim().is_empty();
            // Each source line wraps on its own, as the plain layout keeps
            // them, and an empty one is still a row so its marker shows.
            let lines = body.split('\n').flat_map(|l| {
                let w = wrap_words(l, cw);
                if w.is_empty() {
                    vec![String::new()]
                } else {
                    w
                }
            });
            for (j, line) in lines.enumerate() {
                if j == 0 && !evidence {
                    let role = match *mark {
                        "[x]" => "ok",
                        "[!]" => "danger",
                        _ => "muted",
                    };
                    push(&mut out, format!("{} {line}", ctx.paint(role, mark)));
                } else {
                    push(&mut out, format!("    {line}"));
                }
            }
        }
    }

    // Annotations: the content-rich block the ledger rendered as one
    // unwrapped line each. A blank rail line above, `·` markers, a two-cell
    // hanging indent, wrapped at the terminal width.
    if !annotations.is_empty() {
        push(&mut out, String::new());
        let aw = avail.saturating_sub(2).max(10);
        for body in &annotations {
            // #76.2: `wrap_words` reflows on `str::split_whitespace`, which
            // reads a `\n` as just another space — so a markdown annotation
            // (headers, a table, a list) came out as one run-on paragraph,
            // its line breaks gone. Each of the AUTHOR'S OWN lines is now
            // wrapped on its own; only a line too wide for the terminal still
            // gets `wrap_words`'s reflow, and a blank line (a paragraph
            // break) still prints as one.
            let mut first = true;
            for src_line in body.split('\n') {
                if src_line.is_empty() {
                    push(&mut out, String::new());
                    first = false;
                    continue;
                }
                for line in wrap_words(src_line, aw) {
                    if first {
                        push(&mut out, format!("{} {line}", ctx.paint("card.label", "·")));
                        first = false;
                    } else {
                        push(&mut out, format!("  {line}"));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
