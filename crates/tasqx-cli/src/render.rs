//! Human-readable rendering of API results (DESIGN.md §5, §8).
//!
//! Every function takes a [`Ctx`] (active theme + detected terminal capability)
//! and paints via semantic *role* lookups — `header`, `project`, `overdue`,
//! `urgency.ramp` — never a literal color. The one render pipeline adapts to the
//! terminal: truecolor/256/16 color, `NO_COLOR` emphasis-only, or byte-plain
//! when piped (script-safe). Unicode rules degrade to ASCII on the same signal.

use serde_json::Value;

use crate::theme::Ctx;

/// Strip terminal-control bytes from untrusted text before it is painted, so an
/// imported or agent-authored task field can't smuggle ANSI/OSC escapes that
/// clear the screen, move the cursor, set the window title, or spoof CLI output.
/// This is the terminal-path analogue of `html::esc`. C0 controls (except tab),
/// DEL, and C1 controls are dropped; ordinary printable text is untouched.
pub fn san(s: &str) -> String {
    s.chars().filter(|&c| c == '\t' || !c.is_control()).collect()
}

/// Is this task still open, given the `status` string as it arrived in a JSON
/// payload? The one place the CLI turns a wire status back into a [`Status`].
///
/// The fallback is the whole point. The CLI reads these strings from JSON that
/// may come from a *different build of core* over the daemon socket, so a status
/// this binary has never heard of is a real possibility — and the previous
/// `!matches!(status, "done" | "cancelled")` treated it as open by accident,
/// simply because it was not one of the two names it knew. Guessing "open" is
/// the safe guess: an unknown status shows up in the open counts and on the
/// overdue list, where a human will notice it, instead of vanishing from every
/// total and leaving a report that is quietly short a task. That behaviour is
/// preserved here deliberately rather than inherited, so a future editor changes
/// it on purpose.
pub fn status_is_open(status: &str) -> bool {
    tasqx_core::types::Status::parse(status).map_or(true, tasqx_core::types::Status::is_open)
}

/// Extract a string field, sanitized — every field pulled here is display text
/// that may originate from `store.import` or an MCP write tool.
fn s(v: &Value, key: &str) -> String {
    san(v.get(key).and_then(Value::as_str).unwrap_or(""))
}

/// D21: the trailer is driven by the `default` field the core returns, not
/// printed unconditionally. `project.create` claims the default only when the
/// store has none, so this line was a lie on every `init` but the first.
pub fn project_created(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let became_default = result.get("default").and_then(Value::as_bool).unwrap_or(false);
    let trailer = if became_default {
        "  ·  now your default project".to_string()
    } else {
        // Name the verb that would do it: the user's complaint was being left
        // with no way to steer this and no hint that one existed.
        format!("  ·  default is still {}  (tasqx use {name})", default_label(ctx, result))
    };
    format!("{} created{trailer}\n", ctx.paint("accent", &format!("Project {name}")))
}

/// The name of the default at the time of a `project.create` that did not claim
/// it. The core reports it on the same result so the CLI never has to guess.
fn default_label(ctx: &Ctx, result: &Value) -> String {
    match result.get("current_default").and_then(Value::as_str) {
        Some(d) => ctx.paint("project", &san(d)),
        None => "unset".to_string(),
    }
}

/// D21: `use` moved the default. Both sides are printed — the new one because
/// it is the answer, the old one because a silent switch is the bug.
pub fn default_switched(ctx: &Ctx, result: &Value) -> String {
    let name = s(result, "name");
    let previous = result.get("previous").and_then(Value::as_str).filter(|p| !p.is_empty());
    let trailer = match previous {
        Some(p) => format!("  ·  was {}", ctx.paint("project", &san(p))),
        None => String::new(),
    };
    format!(
        "{}  ·  a bare `tasqx add` lands here{trailer}\n",
        ctx.paint("accent", &format!("Default project is now {name}"))
    )
}

pub fn task_added(ctx: &Ctx, result: &Value, title: &str) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    let status = s(result, "status");
    // D21: name where it landed. With no explicit `project:`, the task inherits
    // the default, and this is the only place the user finds out which project
    // that was — "silently lands in prive.klussen" is this text not existing.
    let proj = match result.get("project").and_then(Value::as_str).filter(|p| !p.is_empty()) {
        Some(p) => format!("  ·  {}", ctx.paint("project", &san(p))),
        None => String::new(),
    };
    format!(
        "{}  ·  {status}  ·  urgency {urg:.1}{proj}\n  {}\n",
        ctx.paint("accent", &format!("Added #{sid}")),
        san(title)
    )
}

pub fn started(ctx: &Ctx, result: &Value) -> String {
    let started = s(result, "interval_started");
    format!(
        "{}  ·  timer running (since {started})\n",
        ctx.paint("timer.active", "Started task")
    )
}

pub fn stopped(ctx: &Ctx, result: &Value) -> String {
    let tracked = s(result, "tracked");
    format!("{}  ·  tracked {tracked}\n", ctx.paint("timer.active", "Stopped"))
}

pub fn done(ctx: &Ctx, result: &Value) -> String {
    let completed = s(result, "completed");
    let mut out = format!("{}  ·  completed {completed}\n", ctx.paint("timer.active", "Done"));
    if let Some(unblocked) = result.get("unblocked").and_then(Value::as_array) {
        if !unblocked.is_empty() {
            let refs: Vec<String> = unblocked
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            out.push_str(&format!(
                "  {} {}\n",
                ctx.paint("accent", "now actionable:"),
                refs.join(" ")
            ));
        }
    }
    // A recurring task spawns its next instance on completion (DESIGN §10, D2).
    if let Some(sp) = result.get("spawned") {
        let sid = sp.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        let when = sp
            .get("due")
            .and_then(Value::as_str)
            .or_else(|| sp.get("scheduled").and_then(Value::as_str))
            .unwrap_or("");
        let tail = if when.is_empty() { String::new() } else { format!(" due {when}") };
        out.push_str(&format!(
            "  {} #{sid}{tail}\n",
            ctx.paint("accent", if ctx.caps.unicode { "\u{21b3} next:" } else { "-> next:" })
        ));
    }
    out
}

/// Render a `task.list` result as an aligned, themed table.
pub fn task_table(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let tasks = result.get("tasks").and_then(Value::as_array).unwrap_or(&empty);
    if tasks.is_empty() {
        return "No tasks.\n".to_string();
    }

    // Normalize the urgency ramp across the visible rows.
    let max_urg = tasks
        .iter()
        .filter_map(|t| t.get("urgency").and_then(Value::as_f64))
        .fold(0.0_f64, f64::max)
        .max(1.0);

    let header = format!(
        "{:>4}  {:>5}  {:<1}  {:<36}  {:<14}  {:<22}  {}",
        "ID", "URG", "P", "TASK", "PROJECT", "DUE", "TAGS"
    );
    let rule_len = header.len().min(120);
    let mut out = String::new();
    out.push_str(&ctx.paint("header", &header));
    out.push('\n');
    out.push_str(&ctx.hrule(rule_len));
    out.push('\n');

    let now = jiff::Timestamp::now();
    for t in tasks {
        let sid = t.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
        let prio = t.get("priority").and_then(Value::as_str).unwrap_or("-");
        let title = truncate(&s(t, "title"), 36, ctx.caps.unicode);
        let project = truncate(&s(t, "project"), 14, ctx.caps.unicode);
        let due_raw = s(t, "due");
        let is_overdue = t
            .get("due")
            .and_then(Value::as_str)
            .and_then(|d| d.parse::<jiff::Timestamp>().ok())
            .map(|d| d < now)
            .unwrap_or(false)
            && status_is_open(&s(t, "status"));
        let due = truncate(&due_raw, 22, ctx.caps.unicode);
        let tags = t
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| san(&a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")))
            .unwrap_or_default();

        // Painted cells (paint after width-formatting so ANSI never skews columns).
        let urg_cell = format!("{urg:>5.1}");
        let urg_p = ctx.theme.ramp_style(urg / max_urg).paint(&urg_cell, &ctx.caps);
        let prio_role = match prio {
            "H" => "priority.H",
            "M" => "priority.M",
            "L" => "priority.L",
            _ => "muted",
        };
        let prio_p = ctx.paint(prio_role, &format!("{prio:<1}"));
        let project_p = ctx.paint("project", &format!("{project:<14}"));
        let tags_p = if tags.is_empty() { String::new() } else { ctx.paint("tag", &tags) };
        let due_p = if is_overdue {
            ctx.paint("overdue", &format!("{due:<22}"))
        } else {
            format!("{due:<22}")
        };

        out.push_str(&format!(
            "{sid:>4}  {urg_p}  {prio_p}  {title:<36}  {project_p}  {due_p}  {tags_p}\n"
        ));
    }

    let count = result.get("count").and_then(Value::as_i64).unwrap_or(tasks.len() as i64);
    out.push_str(&ctx.hrule(rule_len));
    out.push('\n');
    out.push_str(&ctx.paint("muted", &format!("{count} task(s)")));
    out.push('\n');
    out
}

/// Full task detail (task.get): fields plus tags, deps, annotations, blocked.
pub fn task_detail(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let mut out = String::new();
    out.push_str(&ctx.paint("header", &format!("#{sid}  {}", s(result, "title"))));
    out.push('\n');
    out.push_str(&format!("  status     {}\n", s(result, "status")));
    let prio = result.get("priority").and_then(Value::as_str).unwrap_or("-");
    let prio_role = match prio {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    out.push_str(&format!("  priority   {}\n", ctx.paint(prio_role, prio)));
    if !s(result, "project").is_empty() {
        out.push_str(&format!("  project    {}\n", ctx.paint("project", &s(result, "project"))));
    }
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    out.push_str(&format!("  urgency    {urg:.1}\n"));
    if !s(result, "due").is_empty() {
        out.push_str(&format!("  due        {}\n", s(result, "due")));
    }
    if !s(result, "remind").is_empty() {
        out.push_str(&format!("  remind     {}\n", ctx.paint("accent", &s(result, "remind"))));
    }
    if !s(result, "scheduled").is_empty() {
        out.push_str(&format!("  scheduled  {}\n", s(result, "scheduled")));
    }
    if !s(result, "wait").is_empty() {
        out.push_str(&format!("  wait       {}\n", s(result, "wait")));
    }
    if !s(result, "recurrence").is_empty() {
        out.push_str(&format!("  repeats    {}\n", ctx.paint("accent", &s(result, "recurrence"))));
    }
    if !s(result, "estimate").is_empty() {
        out.push_str(&format!("  estimate   {}\n", s(result, "estimate")));
    }
    let blocked = result.get("blocked").and_then(Value::as_bool).unwrap_or(false);
    out.push_str(&format!("  blocked    {blocked}\n"));
    if let Some(tags) = result.get("tags").and_then(Value::as_array) {
        if !tags.is_empty() {
            let names: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
            out.push_str(&format!("  tags       {}\n", ctx.paint("tag", &san(&names.join(" ")))));
        }
    }
    if let Some(deps) = result.get("depends_on").and_then(Value::as_array) {
        if !deps.is_empty() {
            let refs: Vec<String> =
                deps.iter().filter_map(Value::as_i64).map(|n| format!("#{n}")).collect();
            out.push_str(&format!("  depends_on {}\n", refs.join(" ")));
        }
    }
    if let Some(anns) = result.get("annotations").and_then(Value::as_array) {
        for a in anns {
            out.push_str(&format!(
                "  {} {}\n",
                ctx.paint("muted", "·"),
                san(a.get("body").and_then(Value::as_str).unwrap_or(""))
            ));
        }
    }
    out
}

/// A generic `{short_id, status}` confirmation line (cancel/reopen).
/// `tasqx modify` — echo back exactly what changed, and at what rev.
///
/// The echo is the point: `modify` is the one verb that can quietly do the wrong
/// thing (a misparsed date, a field cleared that you meant to set), and a bare
/// "ok" would hide it. Cleared fields print as `field ← (cleared)` so a removal
/// never reads like a set. Values shown are the RESOLVED ones the core stored —
/// `due:friday` echoes the timestamp it actually became, which is where a
/// natural-language misread becomes visible.
pub fn modified(
    ctx: &Ctx,
    result: &Value,
    set: &serde_json::Map<String, Value>,
    tags: &[String],
) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let rev = result.get("_rev").and_then(Value::as_i64);

    let mut out = match rev {
        Some(r) => format!(
            "{}  ·  rev {r}\n",
            ctx.paint("accent", &format!("Modified #{sid}"))
        ),
        None => format!("{}\n", ctx.paint("accent", &format!("Modified #{sid}"))),
    };

    // Stable order: whatever the user typed, the report reads the same every time.
    let mut keys: Vec<&String> = set.keys().collect();
    keys.sort();
    for k in keys {
        let v = &set[k];
        let shown = if v.is_null() {
            ctx.paint("muted", "(cleared)")
        } else {
            san(v.as_str().unwrap_or(""))
        };
        out.push_str(&format!("  {:<11} <- {shown}\n", k));
    }
    if !tags.is_empty() {
        let all: Vec<String> = result
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(|t| format!("+{}", san(t))).collect())
            .unwrap_or_else(|| tags.iter().map(|t| format!("+{}", san(t))).collect());
        out.push_str(&format!("  {:<11} <- {}\n", "tags", ctx.paint("tag", &all.join(" "))));
    }
    out
}

pub fn status_line(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    format!("{}  ->  {}\n", ctx.paint("accent", &format!("#{sid}")), s(result, "status"))
}

pub fn annotated(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let body = san(
        result.get("annotation").and_then(|a| a.get("body")).and_then(Value::as_str).unwrap_or(""),
    );
    format!("{}: {body}\n", ctx.paint("accent", &format!("Annotated #{sid}")))
}

/// `tasqx dep` / `undep`.
///
/// `depends_on` in the result is the set that REMAINS, which made the removal
/// line read `#2 no longer depends on: (none)` — indistinguishable from "the
/// removal did nothing" at a glance. So the removed edge is named from the
/// request, and the remaining set is labelled as such rather than being silently
/// substituted for it.
pub fn dep_result(ctx: &Ctx, result: &Value, added: bool, target: &str) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let deps: Vec<String> = result
        .get("depends_on")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).map(|n| format!("#{n}")).collect())
        .unwrap_or_default();
    let blocked = result.get("blocked").and_then(Value::as_bool).unwrap_or(false);
    let list = if deps.is_empty() { "(none)".to_string() } else { deps.join(" ") };
    let target = san(target.trim_start_matches('#'));

    if added {
        format!(
            "{} now depends on #{target}   ·   depends on: {list}   blocked={blocked}\n",
            ctx.paint("accent", &format!("#{sid}"))
        )
    } else {
        format!(
            "{} no longer depends on #{target}   ·   still depends on: {list}   blocked={blocked}\n",
            ctx.paint("accent", &format!("#{sid}"))
        )
    }
}

pub fn project_table(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let projects = result.get("projects").and_then(Value::as_array).unwrap_or(&empty);
    if projects.is_empty() {
        return "No projects.\n".to_string();
    }
    let mut out = String::new();
    // D21: the leading column is the default marker. `projects` is THE read
    // surface for "where does a bare `tasqx add` land?" — a fact that drove
    // behavior while being shown nowhere.
    out.push_str(&ctx.paint(
        "header",
        &format!("{:<7}  {:<24}  {:<9}  {}", "DEFAULT", "PROJECT", "ARCHIVED", "DESCRIPTION"),
    ));
    out.push('\n');
    for p in projects {
        let name = s(p, "name");
        let archived = p.get("archived").and_then(Value::as_bool).unwrap_or(false);
        let is_default = p.get("default").and_then(Value::as_bool).unwrap_or(false);
        let desc = san(p.get("description").and_then(Value::as_str).unwrap_or(""));
        out.push_str(&format!(
            "{:<7}  {}  {:<9}  {desc}\n",
            if is_default { "*" } else { "" },
            ctx.paint("project", &format!("{name:<24}")),
            if archived { "yes" } else { "no" }
        ));
    }
    out
}

pub fn report(ctx: &Ctx, result: &Value, group_by: &str) -> String {
    let empty = Vec::new();
    let groups = result.get("groups").and_then(Value::as_array).unwrap_or(&empty);
    if groups.is_empty() {
        return "No matching tasks.\n".to_string();
    }
    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!("{:<20}  {:>5}  {:>10}  {:>7}  {:>10}", group_by.to_uppercase(), "COUNT", "EST", "OVERDUE", "TRACKED"),
    ));
    out.push('\n');
    for g in groups {
        let key = san(g.get(group_by).and_then(Value::as_str).unwrap_or(""));
        let count = g.get("count").and_then(Value::as_i64).unwrap_or(0);
        let est = g.get("est_total").and_then(Value::as_str).unwrap_or("-");
        let overdue = g.get("overdue").and_then(Value::as_i64).unwrap_or(0);
        let tracked = g.get("tracked_total").and_then(Value::as_str).unwrap_or("-");
        let overdue_cell = format!("{overdue:>7}");
        let overdue_p = if overdue > 0 {
            ctx.paint("warn", &overdue_cell)
        } else {
            ctx.paint("muted", &overdue_cell)
        };
        out.push_str(&format!(
            "{}  {count:>5}  {est:>10}  {overdue_p}  {tracked:>10}\n",
            ctx.paint("project", &format!("{key:<20}"))
        ));
    }
    out
}

pub fn next_task(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let tasks = result.get("tasks").and_then(Value::as_array).unwrap_or(&empty);
    match tasks.first() {
        None => "Nothing actionable — you're clear.\n".to_string(),
        Some(t) => {
            let sid = t.get("short_id").and_then(Value::as_i64).unwrap_or(0);
            let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
            format!(
                "{}  (urgency {urg:.1})  {}\n",
                ctx.paint("accent", &format!("#{sid}")),
                s(t, "title")
            )
        }
    }
}

/// Urgency breakdown (`tasqx why`), computed from the task.get fields via the
/// same D1 formula the engine uses — so ranking is never a black box.
pub fn why(ctx: &Ctx, result: &Value) -> String {
    use tasqx_core::{urgency, Priority};
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let prio = result.get("priority").and_then(Value::as_str).and_then(Priority::parse);
    let due = result.get("due").and_then(Value::as_str);
    let created = result.get("created").and_then(Value::as_str).unwrap_or("");
    let parts = urgency::breakdown(prio, due, created);
    let total: f64 = parts.iter().map(|(_, v)| v).sum();
    let total = (total * 10.0).round() / 10.0;

    let mut out = String::new();
    out.push_str(&ctx.paint("header", &format!("Why #{sid} has urgency {total:.1}")));
    out.push('\n');
    for (name, val) in parts {
        out.push_str(&format!("  {name:<14} {val:>6.2}\n"));
    }
    out.push_str(&format!("  {:<14} {total:>6.1}\n", "= total"));
    out
}

/// Truncate to `max` chars with a trailing ellipsis. The ellipsis degrades to
/// ASCII `...` when the terminal can't render Unicode (piped/dumb/legacy), so the
/// script-safe path never leaks a stray `…` — matching the rest of the glyph
/// gating (hrule/arrow/mid/chart bars).
fn truncate(s: &str, max: usize, unicode: bool) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if unicode {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    } else {
        // "..." is 3 chars; reserve room for it so the cell width still holds.
        let mut t: String = s.chars().take(max.saturating_sub(3)).collect();
        t.push_str("...");
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{self, Caps, Ctx};
    use serde_json::json;

    /// The CLI and core are separately deployable — `tasqx` talks to a daemon it
    /// did not necessarily ship with — so a status string this binary cannot
    /// parse can genuinely arrive on the wire. The old `!matches!(status, "done"
    /// | "cancelled")` called such a status open purely as a side effect of not
    /// recognising it; now it is a stated rule, and this test is what stops
    /// someone "tidying" the fallback to `false`. Flip the `map_or` default and
    /// an unknown status disappears from open counts, the overdue list and the
    /// tag roll-up at once — three wrong numbers, no error.
    #[test]
    fn an_unknown_wire_status_is_treated_as_open() {
        for unknown in ["", "snoozed", "DONE", "canceled", "archived"] {
            assert!(status_is_open(unknown), "{unknown:?} should fall back to open");
        }
        // ...while the statuses core actually defines still answer for themselves.
        for s in tasqx_core::types::Status::ALL {
            assert_eq!(status_is_open(s.as_str()), s.is_open(), "{s:?}");
        }
    }

    #[test]
    fn san_strips_control_and_escape_bytes() {
        // A title carrying a screen-clear + OSC title-set + cursor move: every
        // control byte is removed, printable text survives.
        let malicious = "\x1b[2Jpwned\x1b]0;evil\x07\x08 ok\ttab";
        let clean = san(malicious);
        assert!(!clean.contains('\x1b'), "escape byte leaked: {clean:?}");
        assert!(!clean.contains('\x07') && !clean.contains('\x08'));
        assert_eq!(clean, "[2Jpwned]0;evil ok\ttab", "printable kept, tab kept");
    }

    #[test]
    fn task_detail_shows_remind_when_set() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let base = json!({
            "short_id": 1, "title": "probe", "status": "pending", "urgency": 8.2,
            "project": "work", "due": "2026-07-20T17:00:00Z", "remind": "-1h",
            "estimate": "PT4H"
        });
        let out = task_detail(&ctx, &base);
        assert!(out.contains("remind"), "remind row missing: {out:?}");
        assert!(out.contains("-1h"), "remind value missing: {out:?}");
        // `estimate` is settable via `est:` sugar and totalled by `report`, so the
        // detail view must show it too — an invisible field is how the dependency
        // bug stayed hidden.
        assert!(out.contains("estimate"), "estimate row missing: {out:?}");
        assert!(out.contains("PT4H"), "estimate value missing: {out:?}");

        // Absent remind must stay absent — the row is conditional, like `due`.
        let mut bare = base.clone();
        bare["remind"] = json!("");
        assert!(!task_detail(&ctx, &bare).contains("remind"));
    }

    #[test]
    fn task_table_neutralizes_escape_in_title() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "tasks": [{
                "short_id": 1, "urgency": 5.0, "priority": "M",
                "title": "hi\x1b[31mRED\x1b[0m", "project": "p",
                "due": "", "tags": ["\x1b[5mblink"], "status": "pending"
            }],
            "count": 1
        });
        let out = task_table(&ctx, &result);
        assert!(!out.contains('\x1b'), "raw escape reached the terminal: {out:?}");
    }

    /// D21: the copy must be TRUE. This line printed unconditionally, so it was
    /// a lie on every `init` after the first — the user read "now your default
    /// project" while the default had not moved (or, before the core fix, while
    /// it had been silently stolen).
    #[test]
    fn project_created_only_claims_the_default_when_it_actually_became_it() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

        let claimed = project_created(&ctx, &json!({ "name": "work", "default": true }));
        assert!(claimed.contains("work"));
        assert!(
            claimed.contains("default project"),
            "the first project really is the default; say so: {claimed:?}"
        );

        let not_claimed =
            project_created(&ctx, &json!({ "name": "prive.klussen", "default": false }));
        assert!(not_claimed.contains("prive.klussen"));
        assert!(
            !not_claimed.contains("default project"),
            "claimed the default when it did not become it: {not_claimed:?}"
        );
        // And it must point at the verb that would do it, so the user is not
        // left guessing (this is the whole complaint).
        assert!(not_claimed.contains("use"), "must name the way to switch: {not_claimed:?}");
    }

    /// The default is state; a switch must show both sides of it.
    #[test]
    fn default_switched_names_the_new_default_and_the_old_one() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = default_switched(&ctx, &json!({ "name": "work", "previous": "prive.klussen" }));
        assert!(out.contains("work"), "missing the new default: {out:?}");
        assert!(out.contains("prive.klussen"), "missing the previous default: {out:?}");

        // First-ever switch has no previous — no dangling "was" clause.
        let fresh = default_switched(&ctx, &json!({ "name": "work", "previous": null }));
        assert!(fresh.contains("work"));
        assert!(!fresh.contains("was"), "invented a previous default: {fresh:?}");
    }

    /// The invisible-field trap: `projects` is the read surface for the default,
    /// so the table must mark it.
    #[test]
    fn project_table_marks_the_default_project() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = project_table(
            &ctx,
            &json!({
                "count": 2,
                "projects": [
                    { "name": "prive.klussen", "archived": false, "default": false, "description": "" },
                    { "name": "work", "archived": false, "default": true, "description": "" },
                ]
            }),
        );
        assert!(out.contains("DEFAULT"), "no default column on the projects table: {out:?}");
        let work_line = out.lines().find(|l| l.contains("work")).expect("work row");
        let other_line = out.lines().find(|l| l.contains("prive.klussen")).expect("other row");
        assert!(work_line.contains('*'), "the default row is unmarked: {work_line:?}");
        assert!(!other_line.contains('*'), "a non-default row is marked: {other_line:?}");
    }

    /// A bare `add` inherits the default, so the confirmation has to say where
    /// the task actually went — otherwise the landing project stays invisible
    /// at the exact moment it matters.
    #[test]
    fn task_added_names_the_project_it_landed_in() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_added(
            &ctx,
            &json!({ "short_id": 3, "status": "pending", "urgency": 5.0, "project": "work" }),
            "a task",
        );
        assert!(out.contains("work"), "the landing project is invisible: {out:?}");

        // Projectless stays quiet rather than printing an empty field.
        let none = task_added(
            &ctx,
            &json!({ "short_id": 4, "status": "pending", "urgency": 5.0, "project": null }),
            "homeless",
        );
        assert!(!none.contains("project"), "printed an empty project row: {none:?}");
    }

    #[test]
    fn truncate_uses_ascii_ellipsis_without_unicode() {
        let long = "a".repeat(50);
        let uni = truncate(&long, 10, true);
        assert!(uni.ends_with('…') && !uni.contains("..."));
        let ascii = truncate(&long, 10, false);
        assert!(ascii.ends_with("...") && !ascii.contains('…'));
        assert!(!ascii.chars().any(|c| !c.is_ascii()), "no non-ASCII in plain path");
        assert_eq!(ascii.chars().count(), 10);
    }
}
