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
    s.chars()
        .filter(|&c| c == '\t' || !c.is_control())
        .collect()
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
    tasqx_core::types::Status::parse(status).is_none_or(tasqx_core::types::Status::is_open)
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
    let became_default = result
        .get("default")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let trailer = if became_default {
        "  ·  now your default project".to_string()
    } else {
        // Name the verb that would do it: the user's complaint was being left
        // with no way to steer this and no hint that one existed.
        format!(
            "  ·  default is still {}  (tasqx use {name})",
            default_label(ctx, result)
        )
    };
    format!(
        "{} created{trailer}\n",
        ctx.paint("accent", &format!("Project {name}"))
    )
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
    let previous = result
        .get("previous")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty());
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
    let proj = match result
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
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
    format!(
        "{}  ·  tracked {tracked}\n",
        ctx.paint("timer.active", "Stopped")
    )
}

/// The dependents a closing verb just released, or nothing if it released none.
///
/// Shared by `done` and `status_line` because BOTH `task.done` and
/// `task.cancel` return this list from the same helper (`compute_unblocked`),
/// and only `done` used to render it. D11 makes cancelling a blocker release
/// its dependents precisely so the graph stays honest, so a `cancel` that
/// printed `#1 -> cancelled` and nothing else hid the very effect the decision
/// exists to produce — and hid it from a reader who had already learned from
/// `done` that a release gets announced, so silence read as "nothing changed".
///
/// One helper rather than a second copy: the two verbs answering differently is
/// the failure, so they cannot have two renderers to drift between. Empty in,
/// empty out — a verb that released nothing must not claim a heading either.
fn unblocked_line(ctx: &Ctx, result: &Value) -> String {
    let refs: Vec<String> = result
        .get("unblocked")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect()
        })
        .unwrap_or_default();
    if refs.is_empty() {
        return String::new();
    }
    format!(
        "  {} {}\n",
        ctx.paint("accent", "now actionable:"),
        refs.join(" ")
    )
}

pub fn done(ctx: &Ctx, result: &Value) -> String {
    let completed = s(result, "completed");
    let mut out = format!(
        "{}  ·  completed {completed}\n",
        ctx.paint("timer.active", "Done")
    );
    out.push_str(&unblocked_line(ctx, result));
    // A recurring task spawns its next instance on completion (DESIGN §10, D2).
    if let Some(sp) = result.get("spawned") {
        let sid = sp.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        let when = sp
            .get("due")
            .and_then(Value::as_str)
            .or_else(|| sp.get("scheduled").and_then(Value::as_str))
            .unwrap_or("");
        let tail = if when.is_empty() {
            String::new()
        } else {
            format!(" due {when}")
        };
        out.push_str(&format!(
            "  {} #{sid}{tail}\n",
            ctx.paint(
                "accent",
                if ctx.caps.unicode {
                    "\u{21b3} next:"
                } else {
                    "-> next:"
                }
            )
        ));
    }
    out
}

/// Render a `task.list` result as an aligned, themed table.
pub fn task_table(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
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
    let rule_len = width(&header).min(120);
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
        let title = fit(&s(t, "title"), 36, ctx.caps.unicode);
        let project = fit(&s(t, "project"), 14, ctx.caps.unicode);
        let due_raw = s(t, "due");
        let is_overdue = t
            .get("due")
            .and_then(Value::as_str)
            .and_then(|d| d.parse::<jiff::Timestamp>().ok())
            .map(|d| d < now)
            .unwrap_or(false)
            && status_is_open(&s(t, "status"));
        let due = fit(&due_raw, 22, ctx.caps.unicode);
        let tags = t
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                san(&a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "))
            })
            .unwrap_or_default();

        // Painted cells (paint after width-formatting so ANSI never skews columns).
        let urg_cell = format!("{urg:>5.1}");
        let urg_p = ctx
            .theme
            .ramp_style(urg / max_urg)
            .paint(&urg_cell, &ctx.caps);
        let prio_role = match prio {
            "H" => "priority.H",
            "M" => "priority.M",
            "L" => "priority.L",
            _ => "muted",
        };
        let prio_p = ctx.paint(prio_role, &format!("{prio:<1}"));
        let project_p = ctx.paint("project", &project);
        let tags_p = if tags.is_empty() {
            String::new()
        } else {
            ctx.paint("tag", &tags)
        };
        // Painted or bare, the cell is the SAME already-fitted string, so the
        // overdue branch cannot drift out of width from the ordinary one.
        let due_p = if is_overdue {
            ctx.paint("overdue", &due)
        } else {
            due
        };

        out.push_str(&format!(
            "{sid:>4}  {urg_p}  {prio_p}  {title}  {project_p}  {due_p}  {tags_p}\n"
        ));
    }

    let count = result
        .get("count")
        .and_then(Value::as_i64)
        .unwrap_or(tasks.len() as i64);
    out.push_str(&ctx.hrule(rule_len));
    out.push('\n');
    out.push_str(&ctx.paint("muted", &format!("{count} task(s)")));
    out.push('\n');

    // The table has no status column, so a row the store could not read would
    // otherwise sit in the default view indistinguishable from ordinary open
    // work — the invisible-field failure this project keeps rebuilding. The
    // note is conditional: on a healthy store it never appears.
    let broken: Vec<String> = tasks
        .iter()
        .filter(|t| status_is_unrecognized(t))
        .map(|t| {
            format!(
                "#{} ({})",
                t.get("short_id").and_then(Value::as_i64).unwrap_or(0),
                s(t, "status")
            )
        })
        .collect();
    if !broken.is_empty() {
        // Names the offending values and the way out. It is deliberately not
        // "run tasqx modify": status is not freely settable (§10 routes every
        // transition through start/stop/done), and the correct value is the
        // user's call, not ours — export, edit that one field, import.
        out.push_str(&ctx.paint(
            "warn",
            &format!(
                "unrecognized status in the store: {} — `tasqx export` still works; \
                 fix the status there and `tasqx import` it back\n",
                broken.join(", ")
            ),
        ));
    }

    // The same failure shape one field over, and the worse one: a blank title
    // renders as an EMPTY cell, so the row is not merely unlabelled in the
    // default view — it is invisible in it. D36 closed every door that could
    // write one, but a store predating D36 can already hold one, and then its
    // `tasqx export` is a document that fails its own `tasqx import`, which
    // costs the user the escape hatch D28 relies on.
    //
    // Detection rather than repair, deliberately: D28 allows repair-on-open
    // only where the correct value is KNOWABLE (D23's stale `default_project`
    // could only be cleared), and nothing here knows what the title was meant
    // to say. Guessing would overwrite the user's bytes with no undo.
    //
    // `modify` is named as the way out, unlike the status note above, because a
    // title IS freely settable — there is no reason to route the user through
    // export/edit/import for a field one command can fix.
    let blank: Vec<String> = tasks
        .iter()
        .filter(|t| s(t, "title").trim().is_empty())
        .map(|t| {
            format!(
                "#{}",
                t.get("short_id").and_then(Value::as_i64).unwrap_or(0)
            )
        })
        .collect();
    if !blank.is_empty() {
        out.push_str(&ctx.paint(
            "warn",
            &format!(
                "blank title in the store: {} — written before this rule existed, and an export \
                 holding one will not import; fix with `tasqx modify <ref> \"<a real title>\"`\n",
                blank.join(", ")
            ),
        ));
    }
    out
}

/// True when the core flagged this task's `status` as text it could not
/// recognize. Reads the explicit boolean rather than re-parsing the string, so
/// one answer comes from the core and the CLI does not grow a second opinion.
fn status_is_unrecognized(t: &Value) -> bool {
    t.get("status_unrecognized")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Full task detail (task.get): fields plus tags, deps, annotations, blocked.
pub fn task_detail(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let mut out = String::new();
    out.push_str(&ctx.paint("header", &format!("#{sid}  {}", s(result, "title"))));
    out.push('\n');
    // `Done` on its own line reads like a status this build has not heard of
    // yet, so say what it actually is and what the real ones are. The list of
    // real ones is derived, never retyped: `Status::ALL` is the canonical list.
    let status_cell = if status_is_unrecognized(result) {
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
    };
    out.push_str(&format!("  status     {status_cell}\n"));
    let prio = result
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let prio_role = match prio {
        "H" => "priority.H",
        "M" => "priority.M",
        "L" => "priority.L",
        _ => "muted",
    };
    out.push_str(&format!("  priority   {}\n", ctx.paint(prio_role, prio)));
    if !s(result, "project").is_empty() {
        out.push_str(&format!(
            "  project    {}\n",
            ctx.paint("project", &s(result, "project"))
        ));
    }
    let urg = result.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
    out.push_str(&format!("  urgency    {urg:.1}\n"));
    if !s(result, "due").is_empty() {
        out.push_str(&format!("  due        {}\n", s(result, "due")));
    }
    if !s(result, "remind").is_empty() {
        out.push_str(&format!(
            "  remind     {}\n",
            ctx.paint("accent", &s(result, "remind"))
        ));
    }
    if !s(result, "scheduled").is_empty() {
        out.push_str(&format!("  scheduled  {}\n", s(result, "scheduled")));
    }
    if !s(result, "wait").is_empty() {
        out.push_str(&format!("  wait       {}\n", s(result, "wait")));
    }
    if !s(result, "recurrence").is_empty() {
        out.push_str(&format!(
            "  repeats    {}\n",
            ctx.paint("accent", &s(result, "recurrence"))
        ));
    }
    if !s(result, "estimate").is_empty() {
        out.push_str(&format!("  estimate   {}\n", s(result, "estimate")));
    }
    // Conditional for the reason `tracked` is: only a closed task HAS a
    // completion moment, and an empty `completed` row on every pending task is
    // noise. It was stored, returned by `task.get` and rendered by `done` — the
    // one surface that scrolls away — so the detail view, whose whole job is
    // showing a task's fields, was the only place the moment could be looked up
    // later and the only place it did not appear.
    if !s(result, "completed").is_empty() {
        out.push_str(&format!("  completed  {}\n", s(result, "completed")));
    }
    // Conditional, unlike `blocked`: every task has a blocked answer worth
    // reading, but "tracked PT0S" on the many tasks that were never timed is
    // noise on the detail of every one of them. Shown from the first tracked
    // second onward, which is when the number starts meaning something.
    let tracked = s(result, "tracked");
    if !tracked.is_empty() && tracked != "PT0S" {
        out.push_str(&format!("  tracked    {tracked}\n"));
    }
    // The open interval is NOT folded into `tracked` (see `task_to_json`), so
    // an active task must say the clock is still running or its tracked total
    // reads as the final answer when it is only the total so far.
    if !s(result, "active_since").is_empty() {
        out.push_str(&format!(
            "  running    {}\n",
            ctx.paint("accent", &format!("since {}", s(result, "active_since")))
        ));
    }
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    out.push_str(&format!("  blocked    {blocked}\n"));
    if let Some(tags) = result.get("tags").and_then(Value::as_array) {
        if !tags.is_empty() {
            let names: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
            out.push_str(&format!(
                "  tags       {}\n",
                ctx.paint("tag", &san(&names.join(" ")))
            ));
        }
    }
    if let Some(deps) = result.get("depends_on").and_then(Value::as_array) {
        if !deps.is_empty() {
            let refs: Vec<String> = deps
                .iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect();
            out.push_str(&format!("  depends_on {}\n", refs.join(" ")));
        }
    }
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
            out.push_str(&format!(
                "  tokens     in {} · out {} · cacheR {} · cacheW {}\n",
                sum("input_tokens"),
                sum("output_tokens"),
                sum("cache_read_tokens"),
                sum("cache_creation_tokens")
            ));
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
        out.push_str(&format!("  {} <- {shown}\n", pad(k, 11)));
    }
    if !tags.is_empty() {
        let all: Vec<String> = result
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|t| format!("+{}", san(t)))
                    .collect()
            })
            .unwrap_or_else(|| tags.iter().map(|t| format!("+{}", san(t))).collect());
        out.push_str(&format!(
            "  {} <- {}\n",
            pad("tags", 11),
            ctx.paint("tag", &all.join(" "))
        ));
    }
    out
}

/// The one-line result of a verb that only changes status — and the cascade it
/// caused, when it caused one.
///
/// `unblocked` is appended rather than branched on by verb: the key is present
/// only when the method returns it (today `task.cancel`), so the shared
/// renderer stays correct for the verbs that release nothing without needing a
/// list of which ones those are.
pub fn status_line(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let mut out = format!(
        "{}  ->  {}\n",
        ctx.paint("accent", &format!("#{sid}")),
        s(result, "status")
    );
    out.push_str(&unblocked_line(ctx, result));
    out
}

pub fn annotated(ctx: &Ctx, result: &Value) -> String {
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let body = san(result
        .get("annotation")
        .and_then(|a| a.get("body"))
        .and_then(Value::as_str)
        .unwrap_or(""));
    format!(
        "{}: {body}\n",
        ctx.paint("accent", &format!("Annotated #{sid}"))
    )
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
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .map(|n| format!("#{n}"))
                .collect()
        })
        .unwrap_or_default();
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let list = if deps.is_empty() {
        "(none)".to_string()
    } else {
        deps.join(" ")
    };
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
    let projects = result
        .get("projects")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if projects.is_empty() {
        return "No projects.\n".to_string();
    }
    let mut out = String::new();
    // D21: the leading column is the default marker. `projects` is THE read
    // surface for "where does a bare `tasqx add` land?" — a fact that drove
    // behavior while being shown nowhere.
    out.push_str(&ctx.paint(
        "header",
        &format!(
            "{:<7}  {:<24}  {:<9}  {}",
            "DEFAULT", "PROJECT", "ARCHIVED", "DESCRIPTION"
        ),
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
            ctx.paint("project", &pad(&name, 24)),
            if archived { "yes" } else { "no" }
        ));
    }
    out
}

pub fn report(ctx: &Ctx, result: &Value, group_by: &str) -> String {
    let empty = Vec::new();
    let groups = result
        .get("groups")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if groups.is_empty() {
        return "No matching tasks.\n".to_string();
    }
    let mut out = String::new();
    // D48a: the four buckets are never blended on any output surface, and this
    // column used to be the blend. `tokens_total` answered "how much did this
    // cost?" with a number that cannot mean that — cache read is 98% of this
    // project's own volume and 68% of its cost, so the blend is wrong in the
    // flattering direction.
    //
    // Four columns is what the HTML report gets; here they would take the row
    // from 74 characters to ~104, past any usable terminal. So the terminal
    // names the largest bucket and its own count instead: no blend, no derived
    // figure, same width. `tasqx report --json` and the HTML page carry all four
    // for anyone who needs the split.
    out.push_str(&ctx.paint(
        "header",
        &format!(
            "{:<20}  {:>5}  {:>10}  {:>7}  {:>10}  {:>12}",
            group_by.to_uppercase(),
            "COUNT",
            "EST",
            "OVERDUE",
            "TRACKED",
            "TOKENS"
        ),
    ));
    out.push('\n');
    for g in groups {
        let key = san(g.get(group_by).and_then(Value::as_str).unwrap_or(""));
        let count = g.get("count").and_then(Value::as_i64).unwrap_or(0);
        let est = g.get("est_total").and_then(Value::as_str).unwrap_or("-");
        let overdue = g.get("overdue").and_then(Value::as_i64).unwrap_or(0);
        let tracked = g
            .get("tracked_total")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let tokens = crate::tokens::dominant_cell(g);
        let overdue_cell = format!("{overdue:>7}");
        let overdue_p = if overdue > 0 {
            ctx.paint("warn", &overdue_cell)
        } else {
            ctx.paint("muted", &overdue_cell)
        };
        out.push_str(&format!(
            "{}  {count:>5}  {est:>10}  {overdue_p}  {tracked:>10}  {tokens:>12}\n",
            ctx.paint("project", &pad(&key, 20))
        ));
    }
    out
}

pub fn next_task(ctx: &Ctx, result: &Value) -> String {
    let empty = Vec::new();
    let tasks = result
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
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
    let prio = result
        .get("priority")
        .and_then(Value::as_str)
        .and_then(Priority::parse);
    let due = result.get("due").and_then(Value::as_str);
    let created = result.get("created").and_then(Value::as_str).unwrap_or("");
    why_rows(ctx, sid, &urgency::breakdown(prio, due, created))
}

/// Render a breakdown the caller has already computed.
///
/// `parts` is a PARAMETER for the same reason `chart::render_throughput`'s
/// series is: `urgency::breakdown` reads the wall clock internally, so a test
/// that could only reach this code through [`why`] would not get to choose the
/// value under test — and the value that broke this display (`-0.0`, from
/// `(-age).max(0.0)` when `created` lands in the very second the clock is read)
/// is one a test cannot schedule.
fn why_rows(ctx: &Ctx, sid: i64, parts: &[(&'static str, f64)]) -> String {
    let total: f64 = parts.iter().map(|(_, v)| v).sum();
    let total = (total * 10.0).round() / 10.0;

    let mut out = String::new();
    out.push_str(&ctx.paint(
        "header",
        &format!("Why #{sid} has urgency {}", signed(total, 1)),
    ));
    out.push('\n');
    for (name, val) in parts {
        out.push_str(&format!("  {name:<14} {:>6}\n", signed(*val, 2)));
    }
    out.push_str(&format!("  {:<14} {:>6}\n", "= total", signed(total, 1)));
    out
}

/// `v` to `places` decimals, without a minus sign the printed number does not
/// earn.
///
/// IEEE 754 has two zeros, and `-0.0` compares EQUAL to `0.0` while keeping its
/// sign bit, so `{:.2}` renders it `-0.00` — which tells the reader a term
/// subtracted urgency when it contributed none. The age term reaches that value
/// honestly: it is `(-age_days).max(0.0)`, and a task created inside the second
/// the clock is read has `age_days == 0.0`.
///
/// The sign is judged AFTER rounding rather than on the value, because the two
/// disagree: `-0.004` is a genuinely negative number that still prints as a row
/// of zeros, so a `v == 0.0` test (which `-0.0` passes) would keep leaking a
/// minus for it. And it is a sign STRIP rather than `.abs()`, because a term
/// that rounds to something non-zero must keep its sign — D1's formula is free
/// to grow a negative one, and a display that quietly dropped the minus would
/// report that change wrong.
fn signed(v: f64, places: usize) -> String {
    let out = format!("{v:.places$}");
    match out.strip_prefix('-') {
        Some(rest) if rest.chars().all(|c| c == '0' || c == '.') => rest.to_string(),
        _ => out,
    }
}

/// How many terminal CELLS this text occupies — the only unit a column can be
/// measured in.
///
/// A column is a grid position, and a grid is made of cells, not of `char`s.
/// The two disagree in every direction: a CJK ideograph is one char in two
/// cells, a combining mark is a char in none, and an emoji ZWJ sequence is five
/// chars forming one two-cell cluster. Every padded column in this module goes
/// through here (or through [`pad`]/[`fit`], which do) so there is one answer to
/// "how wide is this" rather than one per call site.
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Pad `s` out to at least `max` cells. Never truncates.
///
/// This is the treatment for a column whose content is DATA the reader came for
/// — a project name, a config value. Overflowing such a cell pushes the columns
/// to its right, which is ugly; silently cutting the value would be worse, so
/// this half of the pair only ever adds spaces.
pub fn pad(s: &str, max: usize) -> String {
    let mut out = s.to_string();
    // `push_str` on a run of spaces rather than `format!("{s:<max$}")`, because
    // that macro pads by char count — the exact bug this function exists to end.
    out.push_str(&" ".repeat(max.saturating_sub(width(s))));
    out
}

/// Truncate `s` to `max` cells with a trailing ellipsis, then pad out to exactly
/// `max` — a fixed-size box whatever text lands in it.
///
/// This is the treatment for a column with a stated budget the table's layout
/// depends on (`TASK` is 36 cells wide and the header says so). The ellipsis
/// degrades to ASCII `...` when the terminal can't render Unicode
/// (piped/dumb/legacy), so the script-safe path never leaks a stray `…` —
/// matching the rest of the glyph gating (hrule/arrow/mid/chart bars).
///
/// The cut is made by `unicode_truncate`, which walks GRAPHEME CLUSTERS: half a
/// ZWJ sequence is not a shorter emoji but a different one — or a dangling
/// joiner the terminal draws as tofu — and it would still overflow the column,
/// so a cluster is never sliced. The trailing `pad` is not redundant with the
/// truncation: cutting a 36-cell budget just before a double-width glyph leaves
/// 35 cells, and the spaces make up the difference.
fn fit(s: &str, max: usize, unicode: bool) -> String {
    pad(&truncate(s, max, unicode), max)
}

fn truncate(s: &str, max: usize, unicode: bool) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    // `…` is one cell, `...` is three; reserve the room either way so the
    // ellipsis lands INSIDE the budget instead of blowing it by its own width.
    let ellipsis = if unicode { "…" } else { "..." };
    let (head, _) = unicode_truncate::UnicodeTruncateStr::unicode_truncate(
        s,
        max.saturating_sub(width(ellipsis)),
    );
    format!("{head}{ellipsis}")
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
            assert!(
                status_is_open(unknown),
                "{unknown:?} should fall back to open"
            );
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

    /// D50: `tokens_hint` targets machine callers who see raw JSON. The CLI
    /// `done` verb has no token flags, so printing the hint would recommend
    /// the impossible. The fixture carries the key deliberately — present and
    /// deliberately unrendered, the same shape as the D48a tokens_total guard:
    /// a payload without it could not tell rendering from absence.
    #[test]
    fn done_never_renders_the_tokens_hint() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = done(
            &ctx,
            &json!({
                "status": "done",
                "completed": "2026-07-31T10:00:00Z",
                "unblocked": [],
                "tokens_hint": "no token counts were self-reported; log-parse \
                    attribution is a best-effort fallback"
            }),
        );
        assert!(
            out.contains("Done"),
            "the completion line itself went missing: {out:?}"
        );
        assert!(
            !out.contains("tokens_hint") && !out.contains("self-reported"),
            "the machine-only hint reached the terminal: {out:?}"
        );
    }

    /// B1: `tasqx list` prints no status column, so a row the store could not
    /// read would flow through the default view looking like ordinary open work.
    /// The core flags it; the table has to say so, and name the way out — the
    /// value cannot be corrected in place, only exported, edited and imported.
    #[test]
    fn task_table_reports_a_status_the_store_could_not_read() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let result = json!({
            "tasks": [{
                "short_id": 7, "urgency": 5.0, "priority": "M", "title": "important work",
                "project": "work", "due": "", "tags": [],
                "status": "Done", "status_unrecognized": true
            }],
            "count": 1
        });
        let out = task_table(&ctx, &result);
        assert!(
            out.contains("Done"),
            "the offending value must be named: {out:?}"
        );
        assert!(
            out.contains("#7"),
            "the affected task must be identified: {out:?}"
        );
        assert!(out.contains("export"), "the way out must be named: {out:?}");

        // A clean table stays clean — the note is conditional, not a banner.
        let mut ok = result.clone();
        ok["tasks"][0] = json!({
            "short_id": 7, "urgency": 5.0, "priority": "M", "title": "important work",
            "project": "work", "due": "", "tags": [], "status": "pending"
        });
        assert!(
            !task_table(&ctx, &ok).contains("export"),
            "clean table grew a warning"
        );
    }

    /// The same anomaly on the detail view, which does print status: `Done`
    /// alone reads like a status the reader simply has not heard of yet.
    #[test]
    fn task_detail_marks_a_status_the_store_could_not_read() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = task_detail(
            &ctx,
            &json!({
                "short_id": 7, "title": "important work", "status": "Done",
                "status_unrecognized": true, "urgency": 1.0
            }),
        );
        assert!(
            out.contains("Done"),
            "the stored value must survive to the screen: {out:?}"
        );
        assert!(
            out.contains("unrecognized"),
            "the anomaly must be labelled: {out:?}"
        );
        assert!(
            out.contains("pending"),
            "the five real statuses must be named: {out:?}"
        );
    }

    /// P4d: a store written before D36 can hold a BLANK title — every door
    /// refuses one now, but the old ones did not. Such a row renders as an empty
    /// TASK cell, so it is invisible in the one view the user is looking at,
    /// and its export is a document that fails its own import (D36 refuses the
    /// blank title on the way back in). That makes `export` useless as the
    /// escape hatch D28 leans on, and the user cannot even tell which row is at
    /// fault.
    ///
    /// Detection, not repair: D28 already ruled that repair-on-open needs the
    /// correct value to be KNOWABLE, and nothing here knows what the title was
    /// meant to say. So the table names the row and the one command that fixes
    /// it. Unlike the status case, `modify` really is the way out — a title is
    /// freely settable, so there is no need to send the user through export.
    #[test]
    fn task_table_reports_a_blank_title_the_store_should_not_hold() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        for blank in ["", "   ", "	"] {
            let result = json!({
                "tasks": [{
                    "short_id": 4, "urgency": 5.0, "priority": "M", "title": blank,
                    "project": "work", "due": "", "tags": [], "status": "pending"
                }],
                "count": 1
            });
            let out = task_table(&ctx, &result);
            assert!(
                out.contains("#4"),
                "the affected task must be identified for {blank:?}: {out:?}"
            );
            assert!(
                out.contains("blank title"),
                "the anomaly must be labelled for {blank:?}: {out:?}"
            );
            assert!(
                out.contains("modify"),
                "the way out must be named for {blank:?}: {out:?}"
            );
        }

        // Conditional, not a banner: an ordinary table stays clean.
        let ok = json!({
            "tasks": [{
                "short_id": 4, "urgency": 5.0, "priority": "M", "title": "real work",
                "project": "work", "due": "", "tags": [], "status": "pending"
            }],
            "count": 1
        });
        assert!(
            !task_table(&ctx, &ok).contains("blank title"),
            "clean table grew a warning"
        );
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
        assert!(
            !out.contains('\x1b'),
            "raw escape reached the terminal: {out:?}"
        );
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
        assert!(
            not_claimed.contains("use"),
            "must name the way to switch: {not_claimed:?}"
        );
    }

    /// The default is state; a switch must show both sides of it.
    #[test]
    fn default_switched_names_the_new_default_and_the_old_one() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = default_switched(
            &ctx,
            &json!({ "name": "work", "previous": "prive.klussen" }),
        );
        assert!(out.contains("work"), "missing the new default: {out:?}");
        assert!(
            out.contains("prive.klussen"),
            "missing the previous default: {out:?}"
        );

        // First-ever switch has no previous — no dangling "was" clause.
        let fresh = default_switched(&ctx, &json!({ "name": "work", "previous": null }));
        assert!(fresh.contains("work"));
        assert!(
            !fresh.contains("was"),
            "invented a previous default: {fresh:?}"
        );
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
        assert!(
            out.contains("DEFAULT"),
            "no default column on the projects table: {out:?}"
        );
        let work_line = out.lines().find(|l| l.contains("work")).expect("work row");
        let other_line = out
            .lines()
            .find(|l| l.contains("prive.klussen"))
            .expect("other row");
        assert!(
            work_line.contains('*'),
            "the default row is unmarked: {work_line:?}"
        );
        assert!(
            !other_line.contains('*'),
            "a non-default row is marked: {other_line:?}"
        );
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
        assert!(
            out.contains("work"),
            "the landing project is invisible: {out:?}"
        );

        // Projectless stays quiet rather than printing an empty field.
        let none = task_added(
            &ctx,
            &json!({ "short_id": 4, "status": "pending", "urgency": 5.0, "project": null }),
            "homeless",
        );
        assert!(
            !none.contains("project"),
            "printed an empty project row: {none:?}"
        );
    }

    /// Text whose char count and terminal-cell count disagree, one entry per
    /// way they can disagree. Every table guard below runs the whole list, so a
    /// fix that measures CJK correctly but splits an emoji cluster still fails.
    ///
    /// `chars != cells` in four different directions:
    ///  * a CJK ideograph is 1 char, 2 cells;
    ///  * a combining mark is a char with 0 cells;
    ///  * an emoji ZWJ sequence is 5 chars and one 2-cell cluster;
    ///  * a skin-tone modifier is 2 chars and one 2-cell cluster.
    const AWKWARD: &[&str] = &[
        "plain ascii",
        "漢字テスト",
        "e\u{301}accent",                                  // e + COMBINING ACUTE
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466} fam", // family ZWJ sequence
        "\u{1f44d}\u{1f3fd} ok",                           // thumbs up + skin tone modifier
        "中文",
    ];

    fn cells(s: &str) -> usize {
        unicode_width::UnicodeWidthStr::width(s)
    }

    /// A terminal column is a grid of CELLS. `format!("{s:<36}")` pads by CHAR
    /// COUNT, so one CJK title or one emoji shifted every column to its right
    /// and the table stopped being a table.
    ///
    /// The assertion is on the whole ROW rather than on a helper: the rows here
    /// differ ONLY in the title, and every other cell is identical, so equal
    /// display width across rows is exactly "the title column holds its budget".
    /// That is true no matter how the padding is implemented, which is the point
    /// — it cannot be satisfied by a helper that is correct while a call site
    /// still formats with `{:<36}`.
    #[test]
    fn task_table_title_column_holds_its_width_in_cells() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let tasks: Vec<Value> = AWKWARD
            .iter()
            .enumerate()
            .map(|(i, title)| {
                json!({ "short_id": i + 1, "urgency": 5.0, "priority": "M", "title": title,
                        "project": "work", "due": "2026-07-20T17:00:00Z", "tags": ["t"],
                        "status": "pending" })
            })
            .collect();
        let out = task_table(&ctx, &json!({ "tasks": tasks, "count": tasks.len() }));
        let rows: Vec<&str> = out.lines().skip(2).take(AWKWARD.len()).collect();
        assert_eq!(
            rows.len(),
            AWKWARD.len(),
            "expected one row per title: {out:?}"
        );
        let want = cells(rows[0]);
        for (row, title) in rows.iter().zip(AWKWARD) {
            assert_eq!(
                cells(row),
                want,
                "row for {title:?} is {} cells, not {want}: {row:?}",
                cells(row)
            );
        }
    }

    /// The same rule on the OTHER columns of the same table: `project` is
    /// padded to 14 and `due` to 22, and a fix that only widened `title` would
    /// leave both of them shifting the columns to their right.
    #[test]
    fn task_table_project_and_due_columns_hold_their_width_in_cells() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        for field in ["project", "due"] {
            let tasks: Vec<Value> = AWKWARD
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let mut t = json!({ "short_id": i + 1, "urgency": 5.0, "priority": "M",
                                        "title": "same", "project": "work", "due": "",
                                        "tags": ["t"], "status": "pending" });
                    t[field] = json!(v);
                    t
                })
                .collect();
            let out = task_table(&ctx, &json!({ "tasks": tasks, "count": tasks.len() }));
            let rows: Vec<&str> = out.lines().skip(2).take(AWKWARD.len()).collect();
            let want = cells(rows[0]);
            for (row, v) in rows.iter().zip(AWKWARD) {
                assert_eq!(cells(row), want, "{field}={v:?} broke alignment: {row:?}");
            }
        }
    }

    /// `projects` and `report` pad a user-authored string into a column too, so
    /// the rule is theirs as well — fixing only `list` would leave two tables
    /// with the old bug and no test able to see it.
    #[test]
    fn project_and_report_tables_hold_their_widths_in_cells() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);

        let projects: Vec<Value> = AWKWARD
            .iter()
            .map(|n| json!({ "name": n, "archived": false, "default": false, "description": "d" }))
            .collect();
        let out = project_table(
            &ctx,
            &json!({ "count": projects.len(), "projects": projects }),
        );
        let rows: Vec<&str> = out.lines().skip(1).collect();
        let want = cells(rows[0]);
        for (row, n) in rows.iter().zip(AWKWARD) {
            assert_eq!(cells(row), want, "project {n:?} broke alignment: {row:?}");
        }

        let groups: Vec<Value> = AWKWARD
            .iter()
            .map(|k| {
                json!({ "project": k, "count": 1, "est_total": "PT1H", "overdue": 0,
                             "tracked_total": "PT2H", "tokens_total": 123456 })
            })
            .collect();
        let out = report(&ctx, &json!({ "groups": groups }), "project");
        let rows: Vec<&str> = out.lines().skip(1).collect();
        let want = cells(rows[0]);
        for (row, k) in rows.iter().zip(AWKWARD) {
            assert_eq!(
                cells(row),
                want,
                "report group {k:?} broke alignment: {row:?}"
            );
        }
    }

    /// D48a: the TOKENS column names the largest bucket, and the blend it used to
    /// print is gone from this surface.
    ///
    /// This test replaces `report_shows_a_tokens_total_column`, which asserted
    /// the opposite and was correct until D48. The fixture's `tokens_total` is
    /// deliberately present and deliberately unrendered: a store carrying the
    /// field is exactly the case where the old behaviour could creep back, and a
    /// fixture that omitted it could not tell the difference.
    #[test]
    fn report_names_the_largest_bucket_instead_of_blending() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = report(
            &ctx,
            &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H",
                  "tokens_in": 136, "tokens_out": 83_479,
                  "tokens_cache_read": 13_630_240, "tokens_cache_creation": 186_965,
                  "tokens_total": 13_900_820 }
            ] }),
            "project",
        );
        assert!(out.contains("TOKENS"), "TOKENS header missing: {out:?}");
        let row = out.lines().nth(1).unwrap();
        assert!(
            row.contains("cacheR 13.6M"),
            "the dominant bucket is not named: {row:?}"
        );
        assert!(
            !row.contains("13900820") && !row.contains("13.9M"),
            "the blended total reached the terminal: {row:?}"
        );
    }

    /// A group with no measurement must read as "nothing to report", not as a
    /// bucket that spent zero — the difference between an unmeasured project and
    /// a free one.
    #[test]
    fn report_shows_a_dash_for_a_group_that_spent_no_tokens() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = report(
            &ctx,
            &json!({ "groups": [
                { "project": "P", "count": 1, "est_total": "PT1H", "overdue": 0,
                  "tracked_total": "PT2H" }
            ] }),
            "project",
        );
        let row = out.lines().nth(1).unwrap();
        assert!(row.trim_end().ends_with('-'), "expected a dash: {row:?}");
    }

    /// Truncation has to cut on a GRAPHEME boundary and budget in cells. Half a
    /// ZWJ sequence is not a shorter emoji — it is a different one, or a lone
    /// joiner the terminal draws as tofu, and it still overflows the column.
    #[test]
    fn truncation_budgets_cells_and_never_splits_a_cluster() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466}";
        for unicode in [true, false] {
            // Long enough to force a cut, with the cut landing inside a cluster.
            let s = format!("ab{family}{family}cd");
            let got = fit(&s, 7, unicode);
            assert_eq!(
                cells(&got),
                7,
                "cell budget blown (unicode={unicode}): {got:?}"
            );
            assert!(
                !got.ends_with('\u{200d}'),
                "cut left a dangling joiner (unicode={unicode}): {got:?}"
            );
            // A CJK string: 5 ideographs are 10 cells, so 6 cells must fit at
            // most 2 of them plus the ellipsis — a char-counting truncate keeps 5.
            let cjk = fit("漢字テスト", 6, unicode);
            assert_eq!(
                cells(&cjk),
                6,
                "CJK cell budget blown (unicode={unicode}): {cjk:?}"
            );
        }
        // A string already inside its budget is padded, not cut.
        assert_eq!(
            fit("中文", 6, true),
            "中文  ",
            "short cell should be padded to 6 cells"
        );
        assert_eq!(cells(&fit("中文", 6, true)), 6);
    }

    /// `tasqx why` printed `age             -0.00`.
    ///
    /// The age term is `(-age_days).max(0.0)`, and when `created` falls in the
    /// very second the clock is read, `age_days` is `0.0`, so the negation is
    /// `-0.0` — a value that compares EQUAL to zero while keeping its sign bit,
    /// which `{:.2}` then faithfully renders with a minus in front. A reader
    /// cannot act on "minus zero": it says a term subtracted urgency when it
    /// contributed none.
    ///
    /// Both spellings are covered because they are separate format calls with
    /// separate precisions — the component rows at 2 decimals and the total
    /// (which appears TWICE, in the heading and in the `= total` row) at 1. A
    /// fix applied to one of them leaves the other printing `-0`.
    #[test]
    fn why_never_renders_a_component_or_a_total_as_negative_zero() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = why_rows(
            &ctx,
            1,
            &[("priority", 0.0), ("due_proximity", 0.0), ("age", -0.0)],
        );
        assert!(
            !out.contains("-0"),
            "a component rendered as negative zero: {out:?}"
        );
        assert!(
            out.contains("0.00"),
            "the zero itself must still be shown: {out:?}"
        );

        // Every part negative-zero makes the SUM negative zero too, which is the
        // heading and the total row — the twin the component fix does not reach.
        let all_neg = why_rows(&ctx, 1, &[("priority", -0.0), ("age", -0.0)]);
        assert!(
            !all_neg.contains("-0"),
            "the total rendered as negative zero: {all_neg:?}"
        );

        // The rule is about a sign that survived ROUNDING, not about the value
        // being exactly zero: -0.004 is genuinely negative and still prints as a
        // row of zeros, so `v == 0.0` would not have caught it.
        let tiny = why_rows(&ctx, 1, &[("age", -0.004)]);
        assert!(
            !tiny.contains("-0"),
            "a rounded-to-zero negative kept its sign: {tiny:?}"
        );
    }

    /// The twin of the above, and the reason it is not spelled `.abs()`: a term
    /// that really is negative must keep its minus. Nothing in today's formula
    /// produces one, but the formula is D1's to change and a display that
    /// silently drops signs would report the change wrong.
    #[test]
    fn why_keeps_the_sign_of_a_value_that_is_actually_negative() {
        let ctx = Ctx::new(theme::default_theme(), Caps::PLAIN);
        let out = why_rows(&ctx, 1, &[("penalty", -1.5), ("priority", 6.0)]);
        assert!(
            out.contains("-1.50"),
            "a real negative lost its sign: {out:?}"
        );
        assert!(out.contains("4.5"), "the total must still net out: {out:?}");
    }

    #[test]
    fn truncate_uses_ascii_ellipsis_without_unicode() {
        let long = "a".repeat(50);
        let uni = truncate(&long, 10, true);
        assert!(uni.ends_with('…') && !uni.contains("..."));
        let ascii = truncate(&long, 10, false);
        assert!(ascii.ends_with("...") && !ascii.contains('…'));
        assert!(ascii.is_ascii(), "no non-ASCII in plain path");
        assert_eq!(ascii.chars().count(), 10);
    }
}
