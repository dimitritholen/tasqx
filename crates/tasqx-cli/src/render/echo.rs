//! The write echoes (D126): what a terminal answers after a verb changed
//! something.
//!
//! Every one of them is `add`'s card from D122(b), and this module is the one
//! builder behind all of them. A task echo is two lines inside `show`'s rail:
//!
//! ```text
//! ▌ #50  Fix token refresh race on Android
//! ▶ started   H ▄▄▄▄ 17.2   mobile   due tomorrow 17:00   +bug   est 4h
//!   #49  stopped after 5m · Ship the v2 pricing page
//! ```
//!
//! The first line is the task the command named, in its state AFTER the write.
//! The second opens with the outcome and what changed, which are bold, and then
//! `list`'s facts in `list`'s order (urgency cell, project, due, tags, est),
//! which are not. Bold means "this write changed it" and nothing else, because
//! under NO_COLOR bold is the only emphasis that survives. The rail cell of the
//! second line carries `▶` or `⊘` when the task is running or blocked after the
//! write, and `▌` otherwise; those two glyphs never appear anywhere else on the
//! card. Any other task the write moved gets one line underneath, with its own
//! `▶`/`⊘` in the same rail column.
//!
//! The eighteen verbs used to answer in eighteen dialects: instants in
//! nanoseconds, `blocked=true`, `task(s)`, `->`, a title on the second line or
//! none at all. That is what this replaced, and why there is one builder.
//!
//! Off a terminal the same words print in ASCII (`*`/`B` at column 0, no rail)
//! and are never fitted, because a pipe has no width.

use std::collections::HashMap;

use super::{
    due_cell, duration_value, field_ts, plural_tasks, rail_marker, rail_role, s, san,
    status_is_open, truncate, urgency_meter, urgency_scale, width, wrap_words,
};
use crate::columns::{self, Column};
use crate::theme::Ctx;
use jiff::Timestamp;
use serde_json::Value;

/// Titles of the other tasks a write moved, read back by the verb, keyed by
/// short id. A missing entry prints the id alone.
pub type Titles = HashMap<i64, String>;

/// One fact on a card's second line: its text as measured, and as painted.
#[derive(Clone, Debug)]
struct Fact {
    plain: String,
    painted: String,
    /// What joins it to the fact before it: a gap for a fact of its own,
    /// ` · ` for the detail of the one before, one space for the next member
    /// of the same set.
    sep: &'static str,
}

impl Fact {
    fn new(plain: impl Into<String>, painted: impl Into<String>) -> Fact {
        Fact {
            plain: plain.into(),
            painted: painted.into(),
            sep: GAP_SEP,
        }
    }

    /// The detail of the fact before it, such as the title of the task that
    /// blocks this one: `blocked by #50 · Fix the thing`.
    fn detail(ctx: &Ctx, text: &str) -> Fact {
        Fact {
            plain: text.to_string(),
            painted: ctx.paint("card.label", text),
            sep: ATTACH,
        }
    }

    /// A fact in one role.
    fn role(ctx: &Ctx, role: &str, text: &str) -> Fact {
        Fact::new(text, ctx.paint(role, text))
    }

    /// A fact the write changed: its role, made bold.
    fn changed(ctx: &Ctx, role: &str, text: &str) -> Fact {
        Fact::new(text, bold(ctx, role, text))
    }
}

fn bold(ctx: &Ctx, role: &str, text: &str) -> String {
    ctx.theme.role(role).bold().paint(text, &ctx.caps)
}

/// Which of `list`'s facts a card draws after the change. A fact the change
/// already carries is switched off, so nothing is said twice (rule 11).
#[derive(Clone, Copy, Debug)]
struct Context {
    urgency: bool,
    project: bool,
    due: bool,
    tags: bool,
    est: bool,
    recur: bool,
    rev: bool,
}

impl Context {
    const LIST: Context = Context {
        urgency: true,
        project: true,
        due: true,
        tags: true,
        est: true,
        recur: false,
        rev: false,
    };
    const NONE: Context = Context {
        urgency: false,
        project: false,
        due: false,
        tags: false,
        est: false,
        recur: false,
        rev: false,
    };
}

/// A card: the task, what happened to it, and what else moved.
struct Card<'a> {
    task: &'a Value,
    outcome: Fact,
    /// The change. Never dropped.
    lead: Vec<Fact>,
    /// The user's own words (an annotation): wrapped, never cut.
    words: Option<String>,
    /// Droppable, but only after `list`'s context has gone: detail belonging
    /// to the change, such as the title of the task that now blocks this one.
    /// It sits left of the context, and the fit drops from the right.
    detail: Vec<Fact>,
    context: Context,
    below: Vec<String>,
}

impl<'a> Card<'a> {
    fn new(task: &'a Value, outcome: Fact) -> Card<'a> {
        Card {
            task,
            outcome,
            lead: Vec::new(),
            words: None,
            detail: Vec::new(),
            context: Context::LIST,
            below: Vec::new(),
        }
    }
}

/// The outcome word, bold.
fn outcome(ctx: &Ctx, text: &str) -> Fact {
    Fact::changed(ctx, "card.strong", text)
}

fn sid(v: &Value) -> i64 {
    v.get("short_id").and_then(Value::as_i64).unwrap_or(0)
}

fn secs(iso: &str) -> i64 {
    tasqx_core::util::duration_secs(iso).unwrap_or(0).max(0)
}

fn tag_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(san).collect())
        .unwrap_or_default()
}

fn plus(tags: &[String]) -> String {
    tags.iter()
        .map(|t| format!("+{t}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// --------------------------------------------------------------- list's facts

/// `list`'s urgency cell, `H ▄▄▄▄ 17.2` (D117c, D119), with the priority letter
/// bold when this write set the priority. Without Unicode the gauge goes and
/// the cell is `H 17.2`.
fn urgency_fact(ctx: &Ctx, task: &Value, changed: bool) -> Fact {
    let urg = task.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    let prio = task
        .get("priority")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .unwrap_or("-");
    let prio_role = match prio {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    // The letter's role is bold for H in every built-in, which on a card
    // would claim a change that did not happen: bold here is the write's.
    let mut style = ctx.theme.role(prio_role);
    style.bold = changed;
    let letter = style.paint(prio, &ctx.caps);
    let ramp = ctx.theme.ramp_style(urgency_scale(urg));
    let figure = ramp.paint(&format!("{urg:.1}"), &ctx.caps);
    if !ctx.caps.unicode {
        return Fact::new(format!("{prio} {urg:.1}"), format!("{letter} {figure}"));
    }
    let (bar, track) = urgency_meter(urgency_scale(urg));
    Fact::new(
        format!("{prio} {bar}{track} {urg:.1}"),
        format!(
            "{letter} {}{} {figure}",
            ramp.paint(&bar, &ctx.caps),
            ctx.paint("muted", &track)
        ),
    )
}

fn project_fact(ctx: &Ctx, task: &Value, changed: bool) -> Option<Fact> {
    let p = s(task, "project");
    if p.is_empty() {
        return None;
    }
    Some(if changed {
        Fact::changed(ctx, "project", &p)
    } else {
        Fact::role(ctx, "project", &p)
    })
}

/// `due Fri`, in `overdue` when an open task is past it, as its row in
/// `list` is.
fn due_fact(ctx: &Ctx, task: &Value, now: Timestamp, changed: bool) -> Option<Fact> {
    let due = field_ts(task, "due")?;
    let cell = due_cell(due, now);
    let late = due < now && status_is_open(&s(task, "status"));
    let plain = format!("due {cell}");
    // A changed fact is bold whole, label and all; an unchanged one keeps
    // `list`'s weights, where only a late deadline is loud.
    let painted = match (late, changed) {
        (true, true) => ctx.paint("overdue", &plain),
        (false, true) => ctx.paint("card.strong", &plain),
        (true, false) => format!(
            "{} {}",
            ctx.paint("card.label", "due"),
            ctx.paint("overdue", &cell)
        ),
        (false, false) => format!("{} {cell}", ctx.paint("card.label", "due")),
    };
    Some(Fact::new(plain, painted))
}

fn tags_fact(ctx: &Ctx, task: &Value) -> Option<Fact> {
    let tags = tag_list(task, "tags");
    if tags.is_empty() {
        return None;
    }
    Some(Fact::role(ctx, "tag", &plus(&tags)))
}

fn est_fact(ctx: &Ctx, task: &Value, now: Timestamp, changed: bool) -> Option<Fact> {
    let est = s(task, "estimate");
    if est.is_empty() {
        return None;
    }
    let value = duration_value(ctx, &est, now);
    let plain = format!("est {value}");
    let painted = if changed {
        ctx.paint("card.strong", &plain)
    } else {
        format!("{} {value}", ctx.paint("card.label", "est"))
    };
    Some(Fact::new(plain, painted))
}

fn recur_fact(ctx: &Ctx, task: &Value) -> Option<Fact> {
    let rec = s(task, "recurrence");
    if rec.is_empty() {
        return None;
    }
    let mark = if ctx.caps.unicode { "↻" } else { "repeats" };
    Some(Fact::new(
        format!("{mark} {rec}"),
        format!("{} {rec}", ctx.paint("card.label", mark)),
    ))
}

/// `list`'s facts that are still to say, in `list`'s order.
fn context_facts(ctx: &Ctx, task: &Value, c: Context, now: Timestamp) -> Vec<Fact> {
    let mut out = Vec::new();
    if c.urgency && task.get("urgency").is_some() {
        out.push(urgency_fact(ctx, task, false));
    }
    if c.project {
        out.extend(project_fact(ctx, task, false));
    }
    if c.due {
        out.extend(due_fact(ctx, task, now, false));
    }
    if c.tags {
        out.extend(tags_fact(ctx, task));
    }
    if c.est {
        out.extend(est_fact(ctx, task, now, false));
    }
    if c.recur {
        out.extend(recur_fact(ctx, task));
    }
    if c.rev {
        if let Some(rev) = task.get("_rev").and_then(Value::as_i64) {
            out.push(Fact::role(ctx, "card.label", &format!("rev {rev}")));
        }
    }
    out
}

// ------------------------------------------------------------------- drawing

/// Cells between two facts: one more than a table's [`columns::GAP`], so that
/// a fact's own inner spaces (`H ▄▄▄▄ 17.2`, `due Fri`) never read as a gap.
const FACT_GAP: usize = 3;
const GAP_SEP: &str = "   ";

/// What joins a detail to the fact it details.
const ATTACH: &str = " · ";

/// What joins the members of one set (a tag set) that were laid out as facts
/// of their own so that a long set can continue on the next line.
const MEMBER: &str = " ";

/// The cells in front of fact `i`.
fn gap_before(i: usize, f: &Fact) -> usize {
    if i == 0 {
        0
    } else {
        width(f.sep)
    }
}

/// Fit the second line's facts into `budget` cells with the one fitter every
/// table uses (D120). The outcome and the change are fixed; everything else
/// drops whole from the right. The extra cell a card puts between facts rides
/// on each fact after the first, so `columns::fit`'s arithmetic stays its own.
fn fit_facts(fixed: &[Fact], droppable: &[Fact], budget: usize) -> Vec<Fact> {
    let all: Vec<(&Fact, bool)> = fixed
        .iter()
        .map(|f| (f, false))
        .chain(droppable.iter().map(|f| (f, true)))
        .collect();
    let cols: Vec<Column> = all
        .iter()
        .enumerate()
        .map(|(i, (f, drop))| {
            // `fit` counts GAP between columns; the card's own gap before
            // this fact rides on its width instead, so the sums agree.
            let w = (width(&f.plain) + gap_before(i, f)).saturating_sub(if i == 0 {
                0
            } else {
                columns::GAP
            });
            if *drop {
                Column::drops(w, w)
            } else {
                Column::fixed(w)
            }
        })
        .collect();
    let kept = columns::fit(&cols, budget);
    all.into_iter()
        .zip(kept)
        .filter(|(_, w)| *w > 0)
        .map(|((f, _), _)| f.clone())
        .collect()
}

/// Facts laid into lines of at most `budget` cells, greedily and whole; every
/// line after the first starts `indent` cells in. A fact wider than a line
/// gets one to itself.
fn pack(facts: &[Fact], budget: usize, indent: usize) -> Vec<Vec<Fact>> {
    let mut lines: Vec<Vec<Fact>> = vec![Vec::new()];
    let mut used = 0;
    for f in facts {
        let line = lines.last_mut().expect("never empty");
        let need = gap_before(line.len(), f) + width(&f.plain);
        if !line.is_empty() && used + need > budget {
            used = indent + width(&f.plain);
            lines.push(vec![f.clone()]);
        } else {
            used += need;
            line.push(f.clone());
        }
    }
    lines
}

fn join(facts: &[Fact]) -> String {
    let mut out = String::new();
    for (i, f) in facts.iter().enumerate() {
        if i > 0 {
            out.push_str(f.sep);
        }
        out.push_str(&f.painted);
    }
    out
}

/// The rail cell of a card's second line or of a line under it: `▶`/`⊘` for a
/// task that is running or blocked, in the rail's colour.
fn state_glyph(ctx: &Ctx, task: &Value) -> Option<String> {
    rail_marker(task, ctx.caps.unicode).map(|(role, glyph)| ctx.paint(role, glyph))
}

fn draw(ctx: &Ctx, card: Card, now: Timestamp) -> String {
    let task = card.task;
    let id = format!("#{}", sid(task));
    let title = s(task, "title");
    let glyph = state_glyph(ctx, task);
    let mut fixed = vec![card.outcome];
    fixed.extend(card.lead);
    let mut droppable = card.detail;
    droppable.extend(context_facts(ctx, task, card.context, now));
    let mut out = String::new();

    if !ctx.caps.unicode {
        // The plain path: the same words, never fitted.
        out.push_str(format!("{id}  {title}").trim_end());
        out.push('\n');
        let lead = glyph.map(|g| format!("{g} ")).unwrap_or_default();
        let mut facts = fixed;
        if let Some(w) = &card.words {
            facts.push(Fact::new(w.clone(), w.clone()));
        }
        facts.extend(droppable);
        out.push_str(&format!("{lead}{}\n", join(&facts)));
        for line in card.below {
            out.push_str(&line);
        }
        return out;
    }

    let rail = ctx.paint(rail_role(task), "▌");
    let budget = ctx.cols.saturating_sub(2).max(20);
    let title = truncate(&title, budget.saturating_sub(width(&id) + 2), true);
    out.push_str(&format!(
        "{rail} {}  {}\n",
        ctx.paint("card.label", &id),
        ctx.paint("card.strong", &title)
    ));
    let cell = glyph.unwrap_or_else(|| rail.clone());

    if let Some(words) = &card.words {
        // The note is the whole of the change: wrapped under itself, and no
        // context after it, since facts trailing someone's sentence read as
        // part of it.
        let head = join(&fixed);
        let indent = width(
            &fixed
                .iter()
                .map(|f| f.plain.clone())
                .collect::<Vec<_>>()
                .join("   "),
        ) + FACT_GAP;
        let lines = wrap_words(words, budget.saturating_sub(indent).max(12));
        for (i, l) in lines.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("{cell} {head}{}{l}\n", " ".repeat(FACT_GAP)));
            } else {
                out.push_str(&format!("{rail} {}{l}\n", " ".repeat(indent)));
            }
        }
    } else {
        let kept = fit_facts(&fixed, &droppable, budget);
        // A change too long for one line (a long tag set, several fields at
        // once) continues under itself on the rail rather than be cut, since
        // the change is what the echo is for.
        let indent = width(&kept[0].plain) + FACT_GAP;
        for (i, line) in pack(&kept, budget, indent).iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("{cell} {}\n", join(line)));
            } else {
                out.push_str(&format!("{rail} {}{}\n", " ".repeat(indent), join(line)));
            }
        }
    }
    for line in card.below {
        out.push_str(&line);
    }
    out
}

/// A line under a card for another task the write moved: its `▶`/`⊘` in the
/// rail column, then `#N  <what happened> · <title>`, the title cut to fit.
fn moved(ctx: &Ctx, glyph: Option<(&str, &str)>, id: i64, what: Fact, titles: &Titles) -> String {
    let cell = match glyph {
        Some((role, g)) => format!("{} ", ctx.paint(role, g)),
        None => "  ".to_string(),
    };
    let head = format!("#{id}");
    let title = titles.get(&id).cloned().unwrap_or_default();
    let used = 2 + width(&head) + 2 + width(&what.plain);
    let tail = if title.is_empty() {
        String::new()
    } else if !ctx.caps.unicode {
        format!(" · {title}")
    } else {
        let room = ctx.cols.saturating_sub(used + 3);
        if room < 8 {
            String::new()
        } else {
            format!(
                " {} {}",
                ctx.paint("card.label", "·"),
                ctx.paint("card.label", &truncate(&title, room, true))
            )
        }
    };
    format!(
        "{cell}{}  {}{tail}\n",
        ctx.paint("card.label", &head),
        what.painted
    )
}

fn blocked_glyph(ctx: &Ctx) -> (&'static str, &'static str) {
    ("danger", if ctx.caps.unicode { "⊘" } else { "B" })
}

/// The others a closing verb released (D11, D69).
fn released(ctx: &Ctx, result: &Value, titles: &Titles) -> Vec<String> {
    result
        .get("unblocked")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .map(|n| moved(ctx, None, n, Fact::role(ctx, "accent", "unblocked"), titles))
                .collect()
        })
        .unwrap_or_default()
}

// --------------------------------------------------------------- the verbs

/// `tasqx add`. `task` is the task as `task.get` reads it back; the write's own
/// five-field result carries no priority or estimate.
pub fn added(ctx: &Ctx, task: &Value, now: Timestamp) -> String {
    let mut card = Card::new(task, outcome(ctx, "added"));
    card.context.recur = true;
    // Finding #10 (audit-2026-09): a task that landed in no project must say
    // so and name the way out, not just omit the project, which reads like
    // every other add.
    // A task filed behind a future `wait` is out of `list`'s default view, so
    // the echo is the one place the reader learns it went to the backlog.
    if s(task, "status") == "backlog" {
        let until = field_ts(task, "wait")
            .map(|w| format!("backlog until {}", due_cell(w, now)))
            .unwrap_or_else(|| "backlog".to_string());
        card.lead.push(Fact::role(ctx, "muted", &until));
    }
    if s(task, "project").is_empty() {
        card.lead.push(Fact::role(ctx, "warn", "no project"));
        card.detail
            .push(Fact::detail(ctx, "set a default with tasqx use <project>"));
    }
    draw(ctx, card, now)
}

/// `tasqx start`. `task` is the read-back (the write's result when that failed);
/// `titles` names the tasks D6 auto-stopped to make room.
pub fn started(ctx: &Ctx, result: &Value, task: &Value, titles: &Titles, now: Timestamp) -> String {
    let already = result
        .get("already_running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut card = if already {
        // Finding #9: a re-run start is not a start, and nothing changed, so
        // nothing is bold. Since when is the one thing it can add.
        let mut c = Card::new(task, Fact::new("already running", "already running"));
        if let Some(since) =
            field_ts(result, "interval_started").or_else(|| field_ts(task, "active_since"))
        {
            let when = due_cell(since, now);
            c.lead.push(Fact::new(
                format!("since {when}"),
                format!("{} {when}", ctx.paint("card.label", "since")),
            ));
        }
        c
    } else {
        Card::new(task, outcome(ctx, "started"))
    };
    card.below = result
        .get("auto_stopped")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| {
                    // `tracked` on an auto-stopped entry is the interval just
                    // closed, not the task's total (task.rs: `elapsed_iso`).
                    // Not bold: bold is the card's, for what this write
                    // changed on the task it named.
                    let what = interval_fact(ctx, &s(x, "tracked"), now);
                    let what = Fact::role(ctx, "timer.active", &what.plain);
                    moved(ctx, None, sid(x), what, titles)
                })
                .collect()
        })
        .unwrap_or_default();
    draw(ctx, card, now)
}

/// `stopped after 5m`: the one spelling of a closed interval (D126), whatever
/// closed it. A zero interval says `stopped` and no more.
fn interval_fact(ctx: &Ctx, interval: &str, now: Timestamp) -> Fact {
    if secs(interval) == 0 {
        return Fact::changed(ctx, "timer.active", "stopped");
    }
    let text = format!("stopped after {}", duration_value(ctx, interval, now));
    Fact::changed(ctx, "timer.active", &text)
}

/// `tracked 2h of 6h`, when the total says more than the interval beside it
/// does; `None` for a zero total.
fn tracked_fact(ctx: &Ctx, tracked: &str, est: &str, now: Timestamp) -> Option<Fact> {
    if secs(tracked) == 0 {
        return None;
    }
    let mut text = format!("tracked {}", duration_value(ctx, tracked, now));
    if !est.is_empty() {
        text.push_str(&format!(" of {}", duration_value(ctx, est, now)));
    }
    Some(Fact::changed(ctx, "card.strong", &text))
}

/// `tasqx stop`: the interval it closed, and the total when that says more.
pub fn stopped(ctx: &Ctx, result: &Value, task: &Value, now: Timestamp) -> String {
    let interval = s(result, "interval");
    let tracked = s(result, "tracked");
    let est = s(task, "estimate");
    let mut card = Card::new(task, interval_fact(ctx, &interval, now));
    // "Says more" is judged on what the reader sees: a total that renders the
    // same as the interval beside it is the same fact twice (rule 11).
    if duration_value(ctx, &tracked, now) != duration_value(ctx, &interval, now) {
        if let Some(f) = tracked_fact(ctx, &tracked, &est, now) {
            card.lead.push(f);
            card.context.est = false;
        }
    }
    draw(ctx, card, now)
}

/// `tasqx done`: tracked against the estimate when anything was tracked, the
/// dependents it released, and the next occurrence of a recurring task. A
/// closed task has no urgency to rank, so the cell goes (D126).
pub fn done(ctx: &Ctx, result: &Value, task: &Value, titles: &Titles, now: Timestamp) -> String {
    let mut card = Card::new(task, outcome(ctx, "done"));
    card.context.urgency = false;
    let est = s(result, "estimate");
    if let Some(f) = tracked_fact(ctx, &s(result, "tracked"), &est, now) {
        card.lead.push(f);
        if !est.is_empty() {
            card.context.est = false;
        }
    }
    card.below = released(ctx, result, titles);
    if let Some(sp) = result.get("spawned") {
        let when = field_ts(sp, "due")
            .map(|d| format!("next, due {}", due_cell(d, now)))
            .or_else(|| {
                field_ts(sp, "scheduled").map(|d| format!("next, sched {}", due_cell(d, now)))
            })
            .unwrap_or_else(|| "next".to_string());
        card.below.push(moved(
            ctx,
            None,
            sid(sp),
            Fact::role(ctx, "accent", &when),
            &Titles::new(),
        ));
    }
    draw(ctx, card, now)
}

/// The one line `done`'s `tokens_hint` becomes on a terminal (D126), on
/// stderr: its first clause and where the flags are. The "a self-report
/// already covers this task" variant asks nothing of the reader, so it prints
/// nothing. `--json` keeps core's text whole (D56).
pub fn tokens_note(hint: &str, cols: usize) -> Option<String> {
    if hint.starts_with(ALREADY_COVERED) {
        return None;
    }
    let first = hint
        .split(';')
        .next()
        .unwrap_or(hint)
        .split(" — ")
        .next()
        .unwrap_or(hint)
        .trim();
    Some(fit_note(
        &format!("note: {first}"),
        " — tasqx done --help names the flags",
        cols,
    ))
}

/// A note and its pointer, the pointer dropped whole when the two do not fit.
fn fit_note(note: &str, pointer: &str, cols: usize) -> String {
    if width(note) + width(pointer) <= cols {
        format!("{note}{pointer}")
    } else {
        note.to_string()
    }
}

/// How core's hint opens when there is nothing for the reader to do. Pinned
/// against core by `the_token_note_tracks_cores_hint_wording`.
pub(crate) const ALREADY_COVERED: &str = "a self-report already covers";

/// `tasqx cancel` / `tasqx reopen`. Cancelling releases dependents (D11) and
/// reopening puts them back into `blocked` (D69), each on its own line under
/// the card.
pub fn status_changed(
    ctx: &Ctx,
    verb: &str,
    result: &Value,
    task: &Value,
    titles: &Titles,
    now: Timestamp,
) -> String {
    let mut card = Card::new(task, outcome(ctx, verb));
    if !status_is_open(&s(task, "status")) {
        card.context.urgency = false;
    }
    card.below = released(ctx, result, titles);
    if let Some(a) = result.get("blocked").and_then(Value::as_array) {
        for n in a.iter().filter_map(Value::as_i64) {
            card.below.push(moved(
                ctx,
                Some(blocked_glyph(ctx)),
                n,
                Fact::role(ctx, "danger", "blocked again"),
                titles,
            ));
        }
    }
    draw(ctx, card, now)
}

/// `tasqx modify`: every field it set, bold and in `list`'s order where
/// `list` has one, then `list`'s facts it did not touch, then the rev
/// (#188: `--expected-rev` needs it).
///
/// Values come from the task as stored, not as typed, so `due:friday`
/// echoes the day it became, which is where a misread date shows.
pub fn modified(
    ctx: &Ctx,
    task: &Value,
    set: &serde_json::Map<String, Value>,
    tags: &[String],
    now: Timestamp,
) -> String {
    let mut card = Card::new(task, outcome(ctx, "modified"));
    card.context.rev = true;
    let cleared = |k: &str| set.get(k).is_some_and(Value::is_null);
    let has = |k: &str| set.get(k).is_some_and(|v| !v.is_null());

    if has("priority") {
        card.lead.push(urgency_fact(ctx, task, true));
        card.context.urgency = false;
    }
    if has("project") {
        card.lead.extend(project_fact(ctx, task, true));
        card.context.project = false;
    }
    if has("due") {
        card.lead.extend(due_fact(ctx, task, now, true));
        card.context.due = false;
    }
    if !tags.is_empty() {
        card.lead
            .extend(tag_set(ctx, &tag_list(task, "tags"), tags));
        card.context.tags = false;
    }
    if has("estimate") {
        card.lead.extend(est_fact(ctx, task, now, true));
        card.context.est = false;
    }
    for (k, what) in [("scheduled", "sched"), ("wait", "wait")] {
        if let Some(at) = has(k).then(|| field_ts(task, k)).flatten() {
            let cell = due_cell(at, now);
            card.lead
                .push(Fact::changed(ctx, "card.strong", &format!("{what} {cell}")));
        }
    }
    if has("recurrence") {
        card.lead.extend(
            recur_fact(ctx, task)
                .map(|f| Fact::new(f.plain.clone(), ctx.paint("card.strong", &f.plain))),
        );
    }
    if has("remind") {
        let r = s(task, "remind");
        card.lead
            .push(Fact::changed(ctx, "card.strong", &format!("remind {r}")));
    }
    if has("tracked") {
        let t = duration_value(ctx, &s(task, "tracked"), now);
        card.lead
            .push(Fact::changed(ctx, "card.strong", &format!("tracked {t}")));
    }
    let mut keys: Vec<&String> = set.keys().filter(|k| cleared(k)).collect();
    keys.sort();
    for k in keys {
        card.lead.push(Fact::changed(
            ctx,
            "card.strong",
            &format!("{} cleared", field_label(k)),
        ));
        match k.as_str() {
            "project" => card.context.project = false,
            "due" => card.context.due = false,
            "estimate" => card.context.est = false,
            _ => {}
        }
    }
    draw(ctx, card, now)
}

/// How a card names a field `modify` cleared: `list`'s word where it has one.
fn field_label(k: &str) -> &str {
    match k {
        "scheduled" => "sched",
        "estimate" => "est",
        "recurrence" => "repeat",
        other => other,
    }
}

/// A tag set drawn once: the tags this write added bold, the others quiet.
/// One fact per tag, joined by a space, so a set longer than the line can
/// continue on the next one instead of running past the edge.
fn tag_set(ctx: &Ctx, all: &[String], new: &[String]) -> Vec<Fact> {
    let mut all = all.to_vec();
    for n in new {
        if !all.contains(n) {
            all.push(n.clone());
        }
    }
    all.iter()
        .enumerate()
        .map(|(i, t)| {
            let text = format!("+{t}");
            let painted = if new.contains(t) {
                bold(ctx, "tag", &text)
            } else {
                ctx.paint("muted", &text)
            };
            let mut f = Fact::new(text, painted);
            if i > 0 {
                f.sep = MEMBER;
            }
            f
        })
        .collect()
}

/// `tasqx tag` / `tasqx untag`. A tag names what it added inside the whole
/// set, once (rule 11); an untag names what went, and `list`'s tags fact
/// says what remains.
pub fn tag_changed(
    ctx: &Ctx,
    result: &Value,
    task: &Value,
    added: bool,
    asked: &[String],
    now: Timestamp,
) -> String {
    if added {
        let mut card = Card::new(task, outcome(ctx, "tagged"));
        let all = if task.get("tags").is_some() {
            tag_list(task, "tags")
        } else {
            tag_list(result, "tags")
        };
        card.lead.extend(tag_set(ctx, &all, asked));
        card.context.tags = false;
        return draw(ctx, card, now);
    }
    let removed = match tag_list(result, "removed") {
        r if r.is_empty() => asked.to_vec(),
        r => r,
    };
    let mut card = Card::new(task, outcome(ctx, "untagged"));
    card.lead.push(Fact::changed(ctx, "tag", &plus(&removed)));
    draw(ctx, card, now)
}

/// The first task still blocking `task`, as `task.get` reports it.
fn first_blocker(task: &Value) -> Option<(i64, String)> {
    task.get("unmet_blockers")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|b| (sid(b), s(b, "title")))
}

fn blocker_title(task: &Value, id: i64) -> Option<String> {
    task.get("unmet_blockers")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|b| sid(b) == id))
        .map(|b| s(b, "title"))
        .filter(|t| !t.is_empty())
}

/// `tasqx dep` / `tasqx undep`. `dep` says what the task waits on now; an
/// `undep` that leaves another blocker names it and never calls the task free.
pub fn dep_changed(
    ctx: &Ctx,
    result: &Value,
    task: &Value,
    added: bool,
    target: &str,
    now: Timestamp,
) -> String {
    let target: i64 = san(target.trim_start_matches('#')).parse().unwrap_or(0);
    let blocked = task
        .get("blocked")
        .or_else(|| result.get("blocked"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if added {
        // Finding #9: an edge that already existed is not a new one.
        let already = result
            .get("inserted")
            .and_then(Value::as_bool)
            .is_some_and(|inserted| !inserted);
        // Blocked BY THIS edge: the target is among the unmet blockers, or,
        // when the read-back failed and there is no such list, `blocked`
        // is all there is to go on.
        let by_target = blocked
            && (task.get("unmet_blockers").is_none() || blocker_title(task, target).is_some());
        let what = match (by_target, already) {
            (true, false) => Fact::role(ctx, "danger", &format!("blocked by #{target}")),
            (true, true) => Fact::new(
                format!("already blocked by #{target}"),
                format!("already blocked by #{target}"),
            ),
            (false, false) => outcome(ctx, &format!("depends on #{target}")),
            (false, true) => Fact::new(
                format!("already depends on #{target}"),
                format!("already depends on #{target}"),
            ),
        };
        let mut card = Card::new(task, what);
        if let Some(t) = blocker_title(task, target) {
            card.detail.push(Fact::detail(ctx, &t));
        }
        return draw(ctx, card, now);
    }
    let mut card = Card::new(task, outcome(ctx, &format!("no longer waits on #{target}")));
    if blocked {
        if let Some((id, title)) = first_blocker(task).or_else(|| {
            result
                .get("depends_on")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_i64)
                .map(|n| (n, String::new()))
        }) {
            card.lead.push(Fact::role(
                ctx,
                "danger",
                &format!("still blocked by #{id}"),
            ));
            if !title.is_empty() {
                card.detail.push(Fact::detail(ctx, &title));
            }
        }
    }
    draw(ctx, card, now)
}

/// `tasqx annotate`: the note, wrapped under itself.
pub fn annotated(ctx: &Ctx, result: &Value, task: &Value, now: Timestamp) -> String {
    let body = san(result
        .get("annotation")
        .and_then(|a| a.get("body"))
        .and_then(Value::as_str)
        .unwrap_or(""));
    let mut card = Card::new(task, outcome(ctx, "annotated"));
    card.words = Some(body);
    card.context = Context::NONE;
    draw(ctx, card, now)
}

/// `tasqx unannotate` (D113). The id the user typed is not echoed back, and
/// the text never is: what the line owes the reader is that it is permanent.
pub fn annotation_removed(ctx: &Ctx, task: &Value, now: Timestamp) -> String {
    let mut card = Card::new(task, outcome(ctx, "note removed"));
    card.lead.push(Fact::role(
        ctx,
        "card.label",
        "for good; undo cannot bring it back",
    ));
    card.context = Context::NONE;
    draw(ctx, card, now)
}

/// `tasqx undo`. Names the operation it reversed and what came back, because
/// undo takes no argument and this line is the reader's only check that it
/// reversed the thing they meant. The line is driven by the reverted op, not
/// by which keys `restored` carries; an op this build has no words for still
/// prints its `restored` object rather than nothing.
pub fn undone(ctx: &Ctx, result: &Value, task: &Value, now: Timestamp) -> String {
    let op = result
        .get("reverted")
        .and_then(|r| r.get("op"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let restored = result.get("restored").cloned().unwrap_or(Value::Null);
    let verb = match op {
        "stop" => "stop",
        "tag.remove" => "untag",
        "dependency.remove" => "undep",
        "annotation.add" => "annotate",
        other => other,
    };
    let mut card = Card::new(task, outcome(ctx, &format!("undid {}", san(verb))));
    match op {
        "stop" => {
            // `▶` in the rail says it is running again (rule 11); what the
            // reader cannot see is since when. `restored.tracked` is the
            // interval put back on the clock, not the task's total, so it is
            // not printed as `tracked`.
            if let Some(since) = field_ts(&restored, "interval_started") {
                let when = due_cell(since, now);
                card.lead.push(Fact::new(
                    format!("since {when}"),
                    format!("{} {when}", ctx.paint("card.label", "since")),
                ));
            }
        }
        "tag.remove" => {
            // The set once, the tags that came back bold (rule 11: not the
            // restored tags and then `list`'s tags fact holding them again).
            let back = tag_list(&restored, "tags");
            let all = if task.get("tags").is_some() {
                tag_list(task, "tags")
            } else {
                back.clone()
            };
            card.lead.extend(tag_set(ctx, &all, &back));
            card.context.tags = false;
        }
        "dependency.remove" => {
            let n = restored
                .get("depends_on")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            card.lead
                .push(outcome(ctx, &format!("waits on #{n} again")));
        }
        "annotation.add" => {
            let note = s(&restored, "annotation");
            card.lead
                .push(Fact::changed(ctx, "card.strong", "note removed"));
            if !note.is_empty() {
                card.detail.push(Fact::detail(ctx, &note));
            }
            card.context = Context::NONE;
        }
        _ => {
            card.lead.push(Fact::new(
                format!("restored {}", san(&restored.to_string())),
                format!("restored {}", san(&restored.to_string())),
            ));
        }
    }
    draw(ctx, card, now)
}

// --------------------------------------------------------- not a task: no rail

/// A record that is not a task (D126): its name on the first line in the
/// title's role, what happened under it, and no rail, because the rail is a
/// task's state.
fn record(ctx: &Ctx, name: Option<&str>, fixed: Vec<Fact>, droppable: Vec<Fact>) -> String {
    let mut out = String::new();
    if let Some(n) = name {
        out.push_str(&ctx.paint("card.strong", n));
        out.push('\n');
    }
    if !ctx.caps.unicode {
        let all: Vec<Fact> = fixed.into_iter().chain(droppable).collect();
        out.push_str(&join(&all));
        out.push('\n');
        return out;
    }
    let budget = ctx.cols.max(20);
    let kept = fit_facts(&fixed, &droppable, budget);
    let indent = width(&kept[0].plain) + FACT_GAP;
    for (i, line) in pack(&kept, budget, indent).iter().enumerate() {
        let lead = if i == 0 {
            String::new()
        } else {
            " ".repeat(indent)
        };
        out.push_str(&format!("{lead}{}\n", join(line)));
    }
    out
}

/// `tasqx init` (D21): whether the new project took the default, and if not,
/// the command that would make it. The name is quoted with `filter::quote`
/// (#229.13) so a padded name survives the round trip through a shell.
pub fn project_created(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let became = result
        .get("default")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut fixed = vec![outcome(ctx, "created")];
    let mut droppable = Vec::new();
    if became {
        fixed.push(Fact::new(
            "now your default project",
            "now your default project",
        ));
    } else {
        let current = result
            .get("current_default")
            .and_then(Value::as_str)
            .map(san);
        fixed.push(match current {
            Some(d) => Fact::new(
                format!("default stays {d}"),
                format!("default stays {}", ctx.paint("project", &d)),
            ),
            None => Fact::new("default stays unset", "default stays unset"),
        });
        droppable.push(Fact::role(
            ctx,
            "card.label",
            &format!("tasqx use {}", tasqx_core::filter::quote(&name)),
        ));
    }
    record(ctx, Some(&name), fixed, droppable)
}

/// `tasqx use` (D21): the new default, and the old one, because a silent
/// switch is the bug D21 closed.
pub fn default_switched(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let mut fixed = vec![outcome(ctx, "now the default")];
    if let Some(p) = result
        .get("previous")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        let p = san(p);
        fixed.push(Fact::new(
            format!("was {p}"),
            format!("was {}", ctx.paint("project", &p)),
        ));
    }
    let droppable = vec![Fact::role(ctx, "card.label", "a bare tasqx add lands here")];
    record(ctx, Some(&name), fixed, droppable)
}

/// `tasqx archive` (D22, D89): the open work it leaves behind, and what became
/// of the default. Both outcomes of the default are named, because silence
/// cannot be read as "the default is fine" (D39); `default_cleared` and the
/// open counts are fields core always sends.
pub fn project_archived(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let cleared = result
        .get("default_cleared")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let open = result
        .get("open_tasks")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let overdue = result
        .get("open_overdue")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut fixed = vec![outcome(ctx, "archived")];
    let mut droppable = Vec::new();
    if open > 0 {
        let left = if open == 1 {
            "1 open task left in it".to_string()
        } else {
            format!("{open} open tasks left in it")
        };
        let text = if overdue > 0 {
            format!("{left}, {overdue} overdue")
        } else {
            left
        };
        fixed.push(Fact::role(
            ctx,
            if overdue > 0 { "overdue" } else { "warn" },
            &text,
        ));
    }
    if cleared {
        // D22: a store with no default is valid; one the user cannot leave
        // is not, so the way out is named, and it never drops.
        fixed.push(Fact::changed(ctx, "warn", "it was your default project"));
        fixed.push(Fact::detail(ctx, "tasqx use <project> sets another"));
    } else {
        fixed.push(Fact::role(ctx, "card.label", "default unchanged"));
    }
    if open > 0 {
        droppable.push(Fact::role(
            ctx,
            "card.label",
            &format!("tasqx list project:{name}"),
        ));
    }
    record(ctx, Some(&name), fixed, droppable)
}

/// `tasqx import`: what came in, counted in words. `no memory docs` prints
/// even when zero, because a restore that silently brought back no memory
/// read exactly like a document that never had any (#179). The notes about a
/// section the document did not carry follow, as D37 requires.
pub fn imported(ctx: &Ctx, result: &Value) -> String {
    let n = result.get("imported").and_then(Value::as_i64).unwrap_or(0);
    let p = result
        .get("projects_imported")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let d = result
        .get("docs_imported")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let tasks = if n == 0 {
        "no tasks".to_string()
    } else {
        plural_tasks(n)
    };
    let mut what = vec![tasks];
    match p {
        0 => {}
        1 => what.push("1 project".into()),
        p => what.push(format!("{p} projects")),
    }
    what.push(match d {
        0 => "no memory docs".into(),
        1 => "1 memory doc".into(),
        d => format!("{d} memory docs"),
    });
    let dot = format!(" {} ", ctx.paint("card.label", "·"));
    let mut out = format!(
        "{}   {}\n",
        ctx.paint("card.strong", "imported"),
        what.join(&dot)
    );
    if !result
        .get("docs_declared")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        out.push_str(&format!(
            "{} the document carried no `docs` section, so no memory docs were restored\n",
            ctx.paint("card.label", "note:")
        ));
    }
    let minted: Vec<String> = result
        .get("projects_created")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(san).collect())
        .unwrap_or_default();
    if !minted.is_empty() {
        out.push_str(&format!(
            "{} the document carried no `projects` section, so {} created from the tasks: {}\n",
            ctx.paint("card.label", "note:"),
            if minted.len() == 1 {
                "1 project was"
            } else {
                "projects were"
            },
            minted.join(", ")
        ));
    }
    out
}

/// `tasqx export`'s note, on stderr because stdout IS the document.
pub fn export_note(dropped: i64, cols: usize) -> String {
    let what = if dropped == 1 {
        "1 dependency points outside the export, left out".to_string()
    } else {
        format!("{dropped} dependencies point outside the export, left out")
    };
    let pointer = if dropped == 1 {
        " — widen the filter to keep it"
    } else {
        " — widen the filter to keep them"
    };
    fit_note(&format!("note: {what}"), pointer, cols)
}

#[cfg(test)]
mod tests;
