//! `tasqx next` and `tasqx why`: the urgency breakdown (task #727 split of
//! `render.rs`).

use super::*;

use jiff::Timestamp;
use serde_json::Value;
use tasqx_core::filter::overdue_at;

use crate::theme::Ctx;

/// `tasqx next` picks the highest-urgency `@working` row, and `@working`
/// includes `active` — a task already running can BE that row, with nothing
/// on screen saying so. Paired with D6's single-active default (`start`
/// auto-stops whatever was running), a caller who does not already know #9 is
/// the one they started an hour ago reads this as a fresh recommendation and
/// starts timing the wrong thing.
pub fn next_task(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let Some(t) = tasks.first() else {
        return if store_is_empty(result) {
            onboarding_hint(ctx)
        } else {
            "Nothing actionable — you're clear.\n".to_string()
        };
    };
    // D122: the "what now" answer says why this one, and what to type next.
    // It printed `#48  (urgency 18.1)  <title>`: no deadline (the task was two
    // days overdue), no project, and a score in parentheses that explained
    // nothing. The title comes first because it is the answer; the reasons
    // follow on `list`'s own terms (the urgency cell, the project, the
    // deadline as a calendar day); the last line is the two commands a reader
    // reaches for next, so neither has to be remembered.
    let sid = t.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    // The lines under the title start where `#N` does, measured rather than
    // counted: a hand-counted indent drew the facts one column short of it.
    let label = "next  ";
    let pad = " ".repeat(width(label) + 2);
    // Fitted to the terminal (#346): the title is cut where the width ends,
    // as `add`'s echo cuts it, and the facts go through `fit_facts`.
    let id = format!("#{sid}");
    let title = truncate(
        &s(t, "title"),
        ctx.cols.saturating_sub(width(&pad) + width(&id) + 2),
        ctx.caps.unicode,
    );
    let mut out = format!(
        "{}  {}  {}\n",
        ctx.paint("table.label", label),
        ctx.paint("card.label", &id),
        ctx.paint("card.strong", &title)
    );
    let prio = t.get("priority").and_then(Value::as_str).unwrap_or("-");
    let prio_role = match prio {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    let ramp = ctx.theme.ramp_style(urgency_scale(urg));
    // Ranked by what the reader loses without each (`fit_facts`): the
    // urgency cell and the deadline say why this task, the running timer what
    // state it is in (the command line below says so too), the project and
    // the tags only where it lives.
    let mut facts: Vec<(u8, String, String)> = vec![if ctx.caps.unicode {
        let (bar, track) = urgency_meter(urgency_scale(urg));
        (
            0,
            format!("{prio} {bar}{track} {urg:.1}"),
            format!(
                "{} {}{} {}",
                ctx.paint(prio_role, prio),
                ramp.paint(&bar, &ctx.caps),
                ctx.paint("muted", &track),
                ramp.paint(&format!("{urg:.1}"), &ctx.caps)
            ),
        )
    } else {
        (
            0,
            format!("{prio} {urg:.1}"),
            format!(
                "{} {}",
                ctx.paint(prio_role, prio),
                ramp.paint(&format!("{urg:.1}"), &ctx.caps)
            ),
        )
    }];
    let proj = s(t, "project");
    if !proj.is_empty() {
        facts.push((3, proj.clone(), ctx.paint("project", &proj)));
    }
    if let Some(due) = field_ts(t, "due") {
        let cell = format!("due {}", due_cell(due, now));
        let painted = if overdue_at(due, now) {
            ctx.paint("overdue", &cell)
        } else {
            cell.clone()
        };
        facts.push((1, cell, painted));
    }
    if s(t, "status") == "active" {
        let since = field_ts(t, "active_since").map(|at| due_cell(at, now));
        let text = match since {
            Some(when) => format!("already running, since {when}"),
            None => "already running".to_string(),
        };
        facts.push((2, text.clone(), ctx.paint("timer.active", &text)));
    }
    if let Some(tags) = t.get("tags").and_then(Value::as_array) {
        let names: Vec<String> = tags
            .iter()
            .filter_map(Value::as_str)
            .map(|g| format!("+{}", san(g)))
            .collect();
        if !names.is_empty() {
            let joined = names.join(" ");
            facts.push((4, joined.clone(), ctx.paint("tag", &joined)));
        }
    }
    out.push_str(&format!(
        "{pad}{}\n",
        fit_facts(facts, ctx.cols.saturating_sub(width(&pad)))
    ));
    let start = if s(t, "status") == "active" {
        format!("tasqx done {sid}")
    } else {
        format!("tasqx start {sid}")
    };
    out.push_str(&format!(
        "{pad}{}\n",
        ctx.paint("muted", &format!("{start}  {}  tasqx why {sid}", ctx.mid()))
    ));
    out
}

/// Urgency breakdown (`tasqx why`): the task by name, then [`why_terms`].
///
/// D122: the task by name, and each term by what drove it. `why` used to say
/// `Why #48 has urgency 18.1` over `due_proximity 12.00`, which named neither
/// the task nor the fact that it was two days overdue.
pub fn why(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let title = s(result, "title");
    let mut out = String::new();
    if title.is_empty() {
        out.push_str(&ctx.paint("card.label", &format!("#{sid}")));
    } else {
        out.push_str(&format!(
            "{}  {}",
            ctx.paint("card.label", &format!("#{sid}")),
            ctx.paint("card.strong", &title)
        ));
    }
    out.push_str("\n\n");
    out.push_str(&why_terms(ctx, result, now));
    out
}

/// The urgency arithmetic alone: each term, what drove it, and the total,
/// followed by the blocked-by line — everything [`why`] prints after its
/// title header. Split out so `tasqx why --card` (D146+) can follow the box
/// card, which already carries the title as its own header row, with this
/// alone rather than repeating it.
///
/// #150: reads the `urgency_breakdown` the engine returns for a `task.get
/// {explain: true}` call — the same numbers `--json` carries, one clock read
/// for both surfaces — falling back to recomputing via the D1 formula only
/// when the field is absent (a caller that fetched the task without
/// `explain`, e.g. an older daemon on the wire).
///
/// The breakdown alone answers "why is the number 18.0" and says nothing
/// about whether `next` will ever hand this task out — `@working` excludes
/// every blocked row (D53's rule), so a task can score highest here and still
/// never be offered. `task.get` already carries `blocked`/`depends_on`
/// (`show` renders both), so the one line this appends costs no extra call.
pub fn why_terms(ctx: &Ctx, result: &Value, now: Timestamp) -> String {
    use tasqx_core::{urgency, Priority};

    let from_breakdown_field = result
        .get("urgency_breakdown")
        .and_then(Value::as_object)
        .map(|b| {
            [
                ("priority", "priority"),
                ("due_proximity", "due_proximity"),
                ("age", "age"),
            ]
            .iter()
            .filter_map(|(name, key)| b.get(*key).and_then(Value::as_f64).map(|v| (*name, v)))
            .collect::<Vec<(&'static str, f64)>>()
        })
        .filter(|parts| !parts.is_empty());

    let parts = match from_breakdown_field {
        Some(parts) => parts,
        None => {
            let prio = result
                .get("priority")
                .and_then(Value::as_str)
                .and_then(Priority::parse);
            let due = result.get("due").and_then(Value::as_str);
            let created = result.get("created").and_then(Value::as_str).unwrap_or("");
            urgency::breakdown(prio, due, created)
        }
    };

    let rows: Vec<(&str, String, f64)> = parts
        .iter()
        .map(|(name, v)| match *name {
            "priority" => (
                "priority",
                result
                    .get("priority")
                    .and_then(Value::as_str)
                    .unwrap_or("none")
                    .to_string(),
                *v,
            ),
            "due_proximity" => (
                "deadline",
                match field_ts(result, "due") {
                    None => "none".to_string(),
                    Some(due) if overdue_at(due, now) => format!("overdue, {}", day_ago(due, now)),
                    Some(due) => format!("due {}", due_cell(due, now)),
                },
                *v,
            ),
            "age" => (
                "age",
                field_ts(result, "created").map_or_else(String::new, |c| {
                    let days = (now.as_second() - c.as_second()).max(0) / 86_400;
                    match days {
                        0 => "created today".to_string(),
                        1 => "1 day".to_string(),
                        n => format!("{n} days"),
                    }
                }),
                *v,
            ),
            other => (other, String::new(), *v),
        })
        .collect();
    let mut out = why_table(ctx, &rows);
    out.push_str(&blocked_line_for_why(ctx, result));
    out
}

/// Finding #8 (audit-2026-09): `why` explained a blocked task's urgency
/// arithmetic and never mentioned that `next` will skip it anyway — the least
/// relevant half of the answer, with the more relevant half left unsaid. Empty
/// when the task is not blocked, or its blockers are not attached to `result`
/// (an older core, or a caller that trimmed fields).
pub(crate) fn blocked_line_for_why(ctx: &Ctx, result: &Value) -> String {
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !blocked {
        return String::new();
    }
    let empty = Vec::new();
    let names: Vec<String> = result
        .get("unmet_blockers")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .map(|b| {
            let sid = b.get("short_id").and_then(Value::as_i64).unwrap_or(0);
            format!("#{sid} {}", s(b, "title"))
        })
        .collect();
    if names.is_empty() {
        return String::new();
    }
    format!(
        "  {} {} — `next` skips this task\n",
        ctx.paint("overdue", "blocked by"),
        names.join(", ")
    )
}

/// The breakdown as a table: each term, what drove it, and its share, then
/// the urgency. One decimal throughout, so the total is the number `list`,
/// `show` and `next` print for the same task (D122).
///
/// The parts are rounded by largest remainder so the rounded shares add up to
/// the rounded total, which is what audit finding #6 asked of this table
/// (`3.90 + 11.52` must not read as `15.4`), kept at one decimal rather than
/// bought with two. Two decimals made the total `18.09` under a heading, and
/// beside a `list`, that said `18.1`.
///
/// `parts` is a PARAMETER for the reason `chart::render_throughput`'s series
/// is: [`why`] reaches `urgency::breakdown` through the wall clock, and the
/// value that once broke this display (`-0.0`, from `(-age).max(0.0)` when
/// `created` lands in the very second the clock is read) is one a test can
/// only stage through this seam.
pub(crate) fn why_table(ctx: &Ctx, parts: &[(&str, String, f64)]) -> String {
    let shares = apportion(&parts.iter().map(|(_, _, v)| *v).collect::<Vec<_>>());
    let total: f64 = parts.iter().map(|(_, _, v)| v).sum();
    let desc_w = parts.iter().map(|(_, d, _)| width(d)).max().unwrap_or(0);
    let mut out = String::new();
    for ((name, desc, _), share) in parts.iter().zip(&shares) {
        out.push_str(&format!(
            "  {} {}  {:>6}\n",
            ctx.paint("table.label", &pad(name, 10)),
            pad(desc, desc_w),
            signed(*share, 1)
        ));
    }
    out.push_str(&format!(
        "  {} {}  {:>6}\n",
        ctx.paint("table.label", &pad("urgency", 10)),
        " ".repeat(desc_w),
        ctx.paint("card.strong", &format!("{:>6}", signed(total, 1)))
    ));
    out
}

/// `values` rounded to one decimal so that the rounded parts add up to the
/// rounded total: round each down to a tenth, then hand the tenths still
/// missing to the parts that lost the most in rounding. Largest remainder, the
/// method percentages that must add to 100 use.
pub(crate) fn apportion(values: &[f64]) -> Vec<f64> {
    let target = (values.iter().sum::<f64>() * 10.0).round() as i64;
    let mut floors: Vec<i64> = values.iter().map(|v| (v * 10.0).floor() as i64).collect();
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| {
        let ra = values[a] * 10.0 - floors[a] as f64;
        let rb = values[b] * 10.0 - floors[b] as f64;
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });
    let missing = target - floors.iter().sum::<i64>();
    for &i in order.iter().cycle().take(missing.max(0) as usize) {
        floors[i] += 1;
    }
    floors.into_iter().map(|t| t as f64 / 10.0).collect()
}

/// `v` to `places` decimals, without a minus sign the printed number does not
/// earn.
///
/// IEEE 754 has two zeros, and `-0.0` compares EQUAL to `0.0` while keeping its
/// sign bit, so `{:.2}` renders it `-0.00` — which tells the reader a term
/// subtracted urgency when it contributed none. The age term reaches that value
/// honestly: it is scaled from `(-age).max(0.0)`, and a task created inside the
/// second the clock is read has `age == 0.0`, so `max` may hand back the `-0.0`
/// the negation just made.
///
/// The sign is judged AFTER rounding rather than on the value, because the two
/// disagree: `-0.004` is a genuinely negative number that still prints as a row
/// of zeros, so a `v == 0.0` test (which `-0.0` passes) would keep leaking a
/// minus for it. And it is a sign STRIP rather than `.abs()`, because a term
/// that rounds to something non-zero must keep its sign — D1's formula is free
/// to grow a negative one, and a display that quietly dropped the minus would
/// report that change wrong.
pub(crate) fn signed(v: f64, places: usize) -> String {
    let out = format!("{v:.places$}");
    match out.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => out,
    }
}

#[cfg(test)]
mod tests;
