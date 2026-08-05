//! Self-contained HTML report (DESIGN.md §8).
//!
//! One file: inline `<style>`, inline SVG charts, a system-font stack — zero
//! external requests (no CDN, no remote fonts/images/scripts). Dark/light via
//! `prefers-color-scheme` over CSS custom properties whose palette is derived
//! from the active tasqx theme, so terminal and web match. All data comes from
//! pure core reads (`report.summary`, `task.list`, `store.export`, `event.list`).

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use tasqx_core::{dispatch, ApiError, Engine};

use crate::chart::{self, today};
use crate::theme::{Rgb, Theme};

/// Generate the full report document as one self-contained HTML string.
///
/// `params` is the SAME `report.summary` payload the terminal `tasqx report`
/// sends — built once by `report_params` and handed here verbatim, not rebuilt.
/// It used to be built here from scratch, which is why `report <filter> --html`
/// ignored its filter entirely: the two output modes of one command were two
/// independent code paths, so a filter honoured on one was invisible to the
/// other. Threading the filter through a second time would have recreated that
/// divergence; taking core's own request object removes it, and any future
/// `report` knob (a new metric, a new group_by) reaches both modes for free.
///
/// `group_by` and `filter` are read back OUT of `params` rather than passed
/// alongside it, so there is exactly one statement of each.
pub fn generate(engine: &Engine, theme: &Theme, params: &Value) -> Result<String, ApiError> {
    let group_by = params
        .get("group_by")
        .and_then(Value::as_str)
        .unwrap_or(tasqx_core::engine::SUMMARY_GROUP_BY[0]);
    let filter = params.get("filter").and_then(Value::as_str);

    // ---- gather data — all pure reads --------------------------------------
    // The summary keeps core's own scope rules on top of the filter (D24:
    // cancelled excluded unless the filter names a status). No `status:pending`
    // narrowing here — `pending` does not include `active`, so the task you were
    // working on right now used to vanish from the roll-up.
    let summary = dispatch(engine, "report.summary", params)?;
    let export = dispatch(engine, "store.export", &scoped(json!({}), filter))?;
    // `@working` is this panel's own question ("unblocked and startable"), so it
    // is ANDed with the user's scope rather than replacing it. Parenthesised
    // because the DSL has `or`: `project:a or project:b and @working` would
    // otherwise bind the wrong half.
    let actionable_filter = match filter {
        Some(f) => format!("({f}) and @working"),
        None => "@working".to_string(),
    };
    let actionable = dispatch(
        engine,
        "task.list",
        &json!({ "filter": actionable_filter, "sort": ["-urgency"], "limit": 12 }),
    )?;
    // ONE clock read, and the event bound is derived from it rather than from a
    // second one. The report's charts draw 12 weeks of throughput and 30 days of
    // burndown, so 13 weeks of slack covers the wider of the two; before D59 gave
    // `event.list` a bound this read the entire log, which grows with every
    // mutation the store has ever recorded.
    let now = jiff::Timestamp::now().to_string();
    let from = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .saturating_sub(jiff::ToSpan::days(91i64));
    let events = dispatch(
        engine,
        "event.list",
        &json!({ "limit": 100000, "from": format!("{from}T00:00:00Z") }),
    )?;
    let doc = Report {
        theme,
        group_by,
        summary: &summary,
        export: &export,
        actionable: &actionable,
        events: &events,
        now: &now,
    };
    Ok(doc.render())
}

/// Add `filter` to a params object, or leave it absent. Absent, not `null`:
/// core reads a missing key as "no filter" and would reject a null.
fn scoped(mut params: Value, filter: Option<&str>) -> Value {
    if let Some(f) = filter {
        params["filter"] = Value::String(f.to_string());
    }
    params
}

struct Report<'a> {
    theme: &'a Theme,
    /// Which column `summary`'s groups are keyed by — `report.summary` names the
    /// key after the axis, so reading `project` out of a `status` roll-up would
    /// quietly render a table of `(none)`.
    group_by: &'a str,
    summary: &'a Value,
    export: &'a Value,
    actionable: &'a Value,
    events: &'a Value,
    now: &'a str,
}

impl<'a> Report<'a> {
    fn render(&self) -> String {
        let tasks = array_at(self.export, "tasks");

        // Derived counts. All windows are measured against the injected `now`,
        // not the wall clock — rendering must stay a pure function of the
        // struct's inputs, or a fixture pinned to one date starts answering
        // differently as real time passes.
        let now_ts = parse_ts(self.now).unwrap_or_else(jiff::Timestamp::now);
        let mut open = 0usize;
        let mut overdue = 0usize;
        let mut completed_recent: Vec<&Value> = Vec::new();
        let mut overdue_tasks: Vec<&Value> = Vec::new();
        // 7-day window, computed with time-based units (calendar spans can't be
        // added to a bare Timestamp without a zone).
        let cutoff = now_ts
            .checked_sub(jiff::ToSpan::hours(168i64))
            .unwrap_or(now_ts);

        for t in tasks {
            let status = t.get("status").and_then(Value::as_str).unwrap_or("");
            if crate::render::status_is_open(status) {
                open += 1;
                if let Some(due) = t.get("due").and_then(Value::as_str).and_then(parse_ts) {
                    if due < now_ts {
                        overdue += 1;
                        overdue_tasks.push(t);
                    }
                }
            }
            // Via the enum, like the open/overdue counters three lines up. A bare
            // `status == "done"` here would be a second spelling of the same
            // question inside one loop, and invisible to any Status-derived guard.
            if tasqx_core::types::Status::parse(status) == Some(tasqx_core::types::Status::Done) {
                if let Some(c) = t
                    .get("completed")
                    .and_then(Value::as_str)
                    .and_then(parse_ts)
                {
                    if c >= cutoff {
                        completed_recent.push(t);
                    }
                }
            }
        }
        completed_recent.sort_by_key(|t| {
            t.get("completed")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        completed_recent.reverse();

        // Velocity: done events in the last 7 days.
        let velocity = self
            .events
            .get("events")
            .and_then(Value::as_array)
            .map(|evs| {
                evs.iter()
                    .filter(|e| e.get("op").and_then(Value::as_str) == Some("done"))
                    .filter(|e| {
                        e.get("ts")
                            .and_then(Value::as_str)
                            .and_then(parse_ts)
                            .map(|t| t >= cutoff)
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);

        // Top tags across open tasks.
        let mut tag_counts: HashMap<String, u32> = HashMap::new();
        for t in tasks {
            let status = t.get("status").and_then(Value::as_str).unwrap_or("");
            if !crate::render::status_is_open(status) {
                continue;
            }
            if let Some(tags) = t.get("tags").and_then(Value::as_array) {
                for tg in tags.iter().filter_map(Value::as_str) {
                    *tag_counts.entry(tg.to_string()).or_insert(0) += 1;
                }
            }
        }
        let mut top_tags: Vec<(String, u32)> = tag_counts.into_iter().collect();
        top_tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        top_tags.truncate(10);

        // ---- charts ----
        let all_ids: HashSet<String> = tasks
            .iter()
            .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let throughput = chart::throughput(self.events, 12, today());
        let burndown = chart::burndown(self.events, &all_ids, 30, today());

        // ---- assemble ----
        let css = self.css();
        let mut body = String::new();

        // Report-wide token totals, one per bucket, summed across the summary's
        // groups (which already carry core's D24 scope — cancelled work is
        // excluded unless the caller asked for `all`). Saturating, like the
        // per-group roll-up.
        //
        // D48a: four sums, not one. This used to fold `tokens_total` into a
        // single headline tile, which is the number core deliberately keeps apart
        // until emit — and the one that cannot mean what its label said.
        let groups = self
            .summary
            .get("groups")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let bucket_totals: Vec<(&str, i64)> = crate::tokens::BUCKETS
            .iter()
            .map(|(key, _, long)| {
                let sum = groups
                    .iter()
                    .map(|g| g.get(key).and_then(Value::as_i64).unwrap_or(0))
                    .fold(0i64, i64::saturating_add);
                (*long, sum)
            })
            .collect();

        body.push_str(&self.header(
            open,
            completed_recent.len(),
            velocity,
            overdue,
            &bucket_totals,
        ));
        body.push_str("<main>");

        body.push_str(&section(
            "This week's throughput",
            "Tasks opened versus closed, by ISO week.",
            &svg_throughput(&throughput, self.theme),
        ));
        body.push_str(&section(
            "Open work, burning down",
            "Remaining open tasks over the last 30 days.",
            &svg_burndown(&burndown, self.theme),
        ));

        body.push_str(&self.completed_section(&completed_recent));
        body.push_str(&self.overdue_section(&overdue_tasks));
        body.push_str(&self.per_group_section());
        body.push_str(&self.actionable_section());
        body.push_str(&self.tags_section(&top_tags));

        body.push_str("</main>");
        body.push_str(&format!(
            "<footer>Generated {} · every panel is a pure read of the tasqx core API.</footer>",
            esc(&pretty_ts(self.now))
        ));

        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>tasqx report · {theme}</title>\n<style>\n{css}\n</style>\n</head>\n\
             <body>\n{body}\n</body>\n</html>\n",
            theme = esc(&self.theme.name),
            css = css,
            body = body,
        )
    }

    /// The header tiles. `buckets` is one `(label, total)` per token bucket, in
    /// `tokens::BUCKETS` order, so this row and the terminal report name the four
    /// in the same sequence. That order is fixed but carries no cost meaning —
    /// see `tokens::BUCKETS` for why it must not be read as a price gradient.
    ///
    /// D48a: four tiles rather than one blended "AI tokens". They are rendered
    /// even when every bucket is zero: a report whose token tiles vanish on an
    /// unmeasured store would leave a reader guessing whether the work was free
    /// or simply never measured, and those are different answers.
    fn header(
        &self,
        open: usize,
        done: usize,
        velocity: usize,
        overdue: usize,
        buckets: &[(&str, i64)],
    ) -> String {
        let token_tiles: String = buckets
            .iter()
            .map(|(label, n)| stat(&crate::tokens::compact(*n), label))
            .collect();
        format!(
            "<header class=\"summary\">\
               <div class=\"brand\">tasqx <span class=\"muted\">weekly review</span></div>\
               <div class=\"stats\">{}{}{}{}{token_tiles}</div>\
             </header>",
            stat(&open.to_string(), "open"),
            stat(&done.to_string(), "done this week"),
            stat(&velocity.to_string(), "velocity /wk"),
            stat_flag(&overdue.to_string(), "overdue", overdue > 0),
        )
    }

    fn completed_section(&self, tasks: &[&Value]) -> String {
        if tasks.is_empty() {
            return section(
                "Completed this week",
                "Nothing closed in the last 7 days — a quiet week.",
                "",
            );
        }
        let mut rows = String::new();
        for t in tasks {
            rows.push_str(&format!(
                "<li><span class=\"id\">#{id}</span> <span class=\"ttl\">{title}</span>{proj}</li>",
                id = t.get("short_id").and_then(Value::as_i64).unwrap_or(0),
                title = esc(t.get("title").and_then(Value::as_str).unwrap_or("")),
                proj = proj_chip(t),
            ));
        }
        section(
            "Completed this week",
            "What actually shipped in the last 7 days.",
            &format!("<ul class=\"tasklist\">{rows}</ul>"),
        )
    }

    fn overdue_section(&self, tasks: &[&Value]) -> String {
        if tasks.is_empty() {
            return section(
                "Carried over & overdue",
                "Nothing overdue. You're current.",
                "",
            );
        }
        let mut rows = String::new();
        for t in tasks {
            rows.push_str(&format!(
                "<li class=\"over\"><span class=\"id\">#{id}</span> <span class=\"ttl\">{title}</span> \
                 <span class=\"due\">due {due}</span>{proj}</li>",
                id = t.get("short_id").and_then(Value::as_i64).unwrap_or(0),
                title = esc(t.get("title").and_then(Value::as_str).unwrap_or("")),
                due = esc(&pretty_ts(t.get("due").and_then(Value::as_str).unwrap_or(""))),
                proj = proj_chip(t),
            ));
        }
        section(
            "Carried over & overdue",
            "Past their due date and still open — triage these first.",
            &format!("<ul class=\"tasklist\">{rows}</ul>"),
        )
    }

    fn per_group_section(&self) -> String {
        // Derived from `group_by`, never hardcoded: `report.summary` names the
        // group key after the axis it grouped on, so `tasqx report status --html`
        // returned rows keyed `status` while this read `project` and rendered a
        // column of `(none)` under a heading that said "By project".
        let axis = self.group_by;
        let title = format!("By {axis}");
        let groups = array_at(self.summary, "groups");
        if groups.is_empty() {
            return section(&title, &format!("Nothing to report grouped by {axis}."), "");
        }
        let mut rows = String::new();
        for g in groups {
            let name = g.get(axis).and_then(Value::as_str).unwrap_or("(none)");
            let count = g.get("count").and_then(Value::as_i64).unwrap_or(0);
            let est = humanize_iso(g.get("est_total").and_then(Value::as_str).unwrap_or("PT0S"));
            let tracked = humanize_iso(
                g.get("tracked_total")
                    .and_then(Value::as_str)
                    .unwrap_or("PT0S"),
            );
            let overdue = g.get("overdue").and_then(Value::as_i64).unwrap_or(0);
            let od = if overdue > 0 {
                format!("<td class=\"warn\">{overdue}</td>")
            } else {
                "<td class=\"muted\">0</td>".to_string()
            };
            // The four buckets, and only the four. The `tokens_total` column that
            // used to lead them is gone (D48a): sitting first and unmuted, it read
            // as the answer and the four as its footnotes, when it is the one
            // number of the five that cannot mean what its header said.
            //
            // Column order follows `tokens::BUCKETS` so the table, the header
            // tiles and the terminal's tie-break cannot disagree about which
            // bucket is which.
            let tokens_cells: String = crate::tokens::BUCKETS
                .iter()
                .map(|(key, _, _)| {
                    let n = g.get(key).and_then(Value::as_i64).unwrap_or(0);
                    // Compacted, like the header tiles. Rendering 13720240 here
                    // under a tile reading 13.7M put two formats for one quantity
                    // on one page, which only opening it showed.
                    format!("<td class=\"muted\">{}</td>", crate::tokens::compact(n))
                })
                .collect();
            rows.push_str(&format!(
                "<tr><td class=\"proj\">{name}</td><td>{count}</td><td>{est}</td><td>{tracked}</td>{od}{tokens_cells}</tr>",
                name = esc(name),
            ));
        }
        let table = format!(
            "<table class=\"grid\"><thead><tr><th>{head}</th><th>Tasks</th><th>Est</th><th>Tracked</th><th>Overdue</th>\
             <th>Cache read</th><th>Cache write</th><th>In</th><th>Out</th></tr></thead><tbody>{rows}</tbody></table>",
            // The axis name, title-cased — `esc` because it reaches markup, even
            // though core has already restricted it to SUMMARY_GROUP_BY.
            head = esc(&title_case(axis)),
        );
        // "Tasks", not "Open": under D24 this count includes `done`, because
        // completed work is real work and carries nearly all the tracked time.
        // Only `cancelled` is left out. The header stat above says "open" and
        // means something narrower (html.rs's own derivation excludes done too),
        // so this column must not borrow that word for a different number.
        section(
            &title,
            &format!(
                "Task count (cancelled excluded), estimate vs. tracked time, overdue, and the four AI token buckets per {axis}. The buckets are never summed: cache tokens cost a fraction of input and output, so one blended figure would misprice any mix."
            ),
            &table,
        )
    }

    fn actionable_section(&self) -> String {
        let tasks = array_at(self.actionable, "tasks");
        if tasks.is_empty() {
            return section(
                "Now actionable",
                "Nothing unblocked and pending — you're clear.",
                "",
            );
        }
        let mut rows = String::new();
        for t in tasks {
            let urg = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
            rows.push_str(&format!(
                "<li><span class=\"id\">#{id}</span> <span class=\"ttl\">{title}</span> \
                 <span class=\"urg\">urg {urg:.1}</span>{proj}</li>",
                id = t.get("short_id").and_then(Value::as_i64).unwrap_or(0),
                title = esc(t.get("title").and_then(Value::as_str).unwrap_or("")),
                proj = proj_chip(t),
            ));
        }
        section(
            "Now actionable",
            "The highest-urgency unblocked tasks — start at the top.",
            &format!("<ul class=\"tasklist\">{rows}</ul>"),
        )
    }

    fn tags_section(&self, tags: &[(String, u32)]) -> String {
        if tags.is_empty() {
            return String::new();
        }
        let mut chips = String::new();
        for (name, n) in tags {
            chips.push_str(&format!(
                "<span class=\"tag\">{name} <span class=\"tagn\">{n}</span></span>",
                name = esc(name),
            ));
        }
        section(
            "Top tags",
            "Where your open work clusters.",
            &format!("<div class=\"tags\">{chips}</div>"),
        )
    }

    /// CSS with a palette derived from the active theme, for both color schemes.
    fn css(&self) -> String {
        let p = |name: &str, fallback: Rgb| -> String {
            self.theme.palette_color(name).unwrap_or(fallback).hex()
        };
        let accent = p("accent", Rgb::new(0x88, 0xc0, 0xd0));
        let warn = p("warn", Rgb::new(0xeb, 0xcb, 0x8b));
        let danger = p("danger", Rgb::new(0xbf, 0x61, 0x6a));
        let muted_dark = p("muted", Rgb::new(0x4c, 0x56, 0x6a));
        let bg_dark = p("bg", Rgb::new(0x2e, 0x34, 0x40));
        let fg_dark = p("fg", Rgb::new(0xd8, 0xde, 0xe9));

        format!(
            ":root {{\n\
             --accent: {accent};\n--warn: {warn};\n--danger: {danger};\n\
             /* light scheme (default) */\n\
             --bg: #ffffff;\n--fg: #1a1d23;\n--muted: #6b7280;\n--card: #f6f7f9;\n--line: #e3e6ea;\n\
             }}\n\
             @media (prefers-color-scheme: dark) {{\n:root {{\n\
             --bg: {bg_dark};\n--fg: {fg_dark};\n--muted: {muted_dark};\n\
             --card: color-mix(in srgb, {bg_dark} 82%, #ffffff 18%);\n\
             --line: color-mix(in srgb, {bg_dark} 60%, #ffffff 40%);\n\
             }}\n}}\n\
             * {{ box-sizing: border-box; }}\n\
             body {{ margin: 0; background: var(--bg); color: var(--fg);\n\
             font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif;\n\
             line-height: 1.55; }}\n\
             .id, .urg, .due, code, .mono {{ font-family: ui-monospace, \"Cascadia Code\", \"SF Mono\", \"Consolas\", monospace; }}\n\
             main {{ max-width: 72ch; margin: 0 auto; padding: 1.5rem 1.25rem 3rem; }}\n\
             header.summary {{ position: sticky; top: 0; z-index: 5; background: color-mix(in srgb, var(--bg) 88%, transparent);\n\
             backdrop-filter: blur(8px); border-bottom: 1px solid var(--line);\n\
             padding: 0.9rem 1.25rem; display: flex; align-items: center; justify-content: space-between; gap: 1rem; flex-wrap: wrap; }}\n\
             .brand {{ font-weight: 700; font-size: 1.15rem; letter-spacing: -0.01em; }}\n\
             .brand .muted {{ font-weight: 400; }}\n\
             .stats {{ display: flex; gap: 1.4rem; }}\n\
             .stat {{ text-align: right; }}\n\
             .stat .n {{ font-size: 1.5rem; font-weight: 700; line-height: 1; font-variant-numeric: tabular-nums;\n\
             font-family: ui-monospace, monospace; }}\n\
             .stat .l {{ font-size: 0.72rem; color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; }}\n\
             .stat.flag .n {{ color: var(--danger); }}\n\
             section {{ margin-top: 2.2rem; }}\n\
             section > h2 {{ font-size: 1.05rem; margin: 0 0 0.15rem; letter-spacing: -0.01em; }}\n\
             section > .sub {{ color: var(--muted); font-size: 0.85rem; margin: 0 0 0.9rem; }}\n\
             .muted {{ color: var(--muted); }} .warn {{ color: var(--warn); }}\n\
             figure {{ margin: 0; border: 1px solid var(--line); border-radius: 12px; background: var(--card); padding: 0.9rem; overflow-x: auto; }}\n\
             figure svg {{ display: block; width: 100%; height: auto; }}\n\
             ul.tasklist {{ list-style: none; margin: 0; padding: 0; }}\n\
             ul.tasklist li {{ padding: 0.45rem 0.1rem; border-bottom: 1px solid var(--line); display: flex; align-items: baseline; gap: 0.5rem; flex-wrap: wrap; }}\n\
             ul.tasklist li:last-child {{ border-bottom: 0; }}\n\
             .id {{ color: var(--accent); font-weight: 600; }}\n\
             .ttl {{ flex: 1; min-width: 12ch; }}\n\
             .urg {{ color: var(--muted); font-size: 0.82rem; }}\n\
             .due {{ color: var(--danger); font-size: 0.82rem; }}\n\
             li.over .ttl {{ font-weight: 500; }}\n\
             .chip {{ font-size: 0.72rem; color: var(--muted); border: 1px solid var(--line); border-radius: 999px; padding: 0.05rem 0.5rem; }}\n\
             table.grid {{ width: 100%; border-collapse: collapse; font-size: 0.9rem; }}\n\
             table.grid th {{ text-align: left; color: var(--muted); font-weight: 600; font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.05em; border-bottom: 1px solid var(--line); padding: 0.4rem 0.5rem; }}\n\
             table.grid td {{ padding: 0.4rem 0.5rem; border-bottom: 1px solid var(--line); font-variant-numeric: tabular-nums; }}\n\
             table.grid td.proj {{ font-weight: 600; }}\n\
             .tags {{ display: flex; flex-wrap: wrap; gap: 0.5rem; }}\n\
             .tag {{ background: var(--card); border: 1px solid var(--line); border-radius: 999px; padding: 0.2rem 0.7rem; font-size: 0.85rem; }}\n\
             .tag .tagn {{ color: var(--accent); font-weight: 700; }}\n\
             footer {{ max-width: 72ch; margin: 0 auto; padding: 1rem 1.25rem 3rem; color: var(--muted); font-size: 0.8rem; }}\n"
        )
    }
}

// ---- small HTML/format helpers ---------------------------------------------

/// Borrow one array field out of a core payload, as a slice tied to the payload.
///
/// Every reader in this module is read-only, so nothing here may own its rows.
/// The three call sites each used to `.cloned().unwrap_or_default()` the array
/// and drop the copy at the end of the function — on a 2000-task store that
/// duplicates the whole `store.export` document (every task with its tags,
/// annotations, dependency ids and token rows) so the next ninety lines can read
/// it once. The `&[Value]` return type is what forbids that: a cloning body
/// cannot compile against it.
///
/// A missing key, a null, or a non-array yields an empty slice rather than an
/// error, exactly as the `unwrap_or_default()` it replaces — the sections read
/// `.is_empty()` and render their empty state, which is what a scoped export
/// with no matching tasks must produce.
fn array_at<'a>(payload: &'a Value, key: &str) -> &'a [Value] {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// ASCII title-case for a group_by axis (`status` -> `Status`) — a table header,
/// not prose, so the one-letter rule is all that is needed.
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn section(title: &str, sub: &str, body: &str) -> String {
    format!(
        "<section><h2>{}</h2><p class=\"sub\">{}</p>{}</section>",
        esc(title),
        esc(sub),
        body
    )
}

fn stat(n: &str, label: &str) -> String {
    format!(
        "<div class=\"stat\"><div class=\"n\">{}</div><div class=\"l\">{}</div></div>",
        esc(n),
        esc(label)
    )
}
fn stat_flag(n: &str, label: &str, flag: bool) -> String {
    let cls = if flag { "stat flag" } else { "stat" };
    format!(
        "<div class=\"{cls}\"><div class=\"n\">{}</div><div class=\"l\">{}</div></div>",
        esc(n),
        esc(label)
    )
}

fn proj_chip(t: &Value) -> String {
    match t
        .get("project")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(p) => format!(" <span class=\"chip\">{}</span>", esc(p)),
        None => String::new(),
    }
}

/// HTML-escape text so titles/tags can never inject markup (also keeps the file
/// well-formed as a single document).
///
/// `pub(crate)` because `docs.rs` renders the same kind of document and must
/// escape by the same rule — one escaper, one place, so the two surfaces can
/// never drift into two different notions of "safe".
pub(crate) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        // Drop terminal control bytes before escaping markup. `report --html`
        // defaults to stdout, so this text lands in the same terminal
        // `render::san` protects — escaping `<` while passing `ESC ]0;` through
        // would leave the two output paths holding different standards. Tab and
        // newline are legitimate document whitespace and stay.
        if c.is_control() && c != '\t' && c != '\n' {
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn parse_ts(s: &str) -> Option<jiff::Timestamp> {
    s.parse().ok()
}

/// A friendlier timestamp: `2026-07-15 11:06 UTC` from RFC3339.
fn pretty_ts(s: &str) -> String {
    match s.parse::<jiff::Timestamp>() {
        Ok(t) => {
            let z = t.to_zoned(jiff::tz::TimeZone::UTC);
            let d = z.date();
            let ti = z.time();
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02} UTC",
                d.year(),
                d.month(),
                d.day(),
                ti.hour(),
                ti.minute()
            )
        }
        Err(_) => s.to_string(),
    }
}

/// ISO-8601 duration → `19h 30m` (or `—` for zero).
fn humanize_iso(iso: &str) -> String {
    let secs = duration_secs(iso).unwrap_or(0);
    if secs <= 0 {
        return "—".to_string();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    match (h, m) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// The ONE duration reader — `tasqx_core::util::duration_secs`, the same one
/// `report` and urgency roll-ups use. This file used to carry its own copy,
/// which drifted (it silently ignored years/months) and did unchecked i64
/// multiplies, so `report --html` over a store holding a huge estimate exited
/// 101 while the core reader was being hardened separately. One reader, one
/// overflow rule, no second copy to forget.
use tasqx_core::util::duration_secs;

// ============================================================================
// Inline SVG charts (same numbers + urgency.ramp as the terminal)
// ============================================================================

fn ramp_stops(theme: &Theme, id: &str) -> String {
    let anchors = theme.ramp();
    if anchors.is_empty() {
        // mono: a single accent-derived stop so the gradient is still valid.
        let a = theme
            .palette_color("fg")
            .unwrap_or(Rgb::new(0x88, 0x88, 0x88));
        return format!(
            "<linearGradient id=\"{id}\" x1=\"0\" y1=\"1\" x2=\"0\" y2=\"0\">\
             <stop offset=\"0%\" stop-color=\"{c}\"/><stop offset=\"100%\" stop-color=\"{c}\"/></linearGradient>",
            c = a.hex()
        );
    }
    let n = anchors.len();
    let mut stops = String::new();
    for (i, c) in anchors.iter().enumerate() {
        let off = (i as f64 / (n - 1).max(1) as f64) * 100.0;
        stops.push_str(&format!(
            "<stop offset=\"{off:.0}%\" stop-color=\"{}\"/>",
            c.hex()
        ));
    }
    format!(
        "<linearGradient id=\"{id}\" x1=\"0\" y1=\"1\" x2=\"0\" y2=\"0\">{stops}</linearGradient>"
    )
}

fn svg_throughput(buckets: &[chart::WeekBucket], theme: &Theme) -> String {
    let w = 720.0;
    let h = 220.0;
    let pad_l = 34.0;
    let pad_b = 26.0;
    let pad_t = 12.0;
    let plot_w = w - pad_l - 12.0;
    let plot_h = h - pad_b - pad_t;
    let max = buckets
        .iter()
        .map(|b| b.added.max(b.done))
        .max()
        .unwrap_or(1)
        .max(1) as f64;

    let accent = theme
        .palette_color("accent")
        .unwrap_or(Rgb::new(0x88, 0xc0, 0xd0))
        .hex();
    let done_c = theme
        .ramp()
        .first()
        .copied()
        .unwrap_or(Rgb::new(0xa3, 0xbe, 0x8c))
        .hex();

    let n = buckets.len().max(1);
    let slot = plot_w / n as f64;
    let bar_w = (slot * 0.32).min(26.0);

    let mut bars = String::new();
    let mut labels = String::new();
    for (i, b) in buckets.iter().enumerate() {
        let cx = pad_l + slot * (i as f64 + 0.5);
        let added_h = (b.added as f64 / max) * plot_h;
        let done_h = (b.done as f64 / max) * plot_h;
        let base = pad_t + plot_h;
        // added bar (left), done bar (right)
        bars.push_str(&format!(
            "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{bw:.1}\" height=\"{hh:.1}\" rx=\"2\" fill=\"{accent}\"/>",
            x = cx - bar_w - 1.0, y = base - added_h, bw = bar_w, hh = added_h,
        ));
        bars.push_str(&format!(
            "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{bw:.1}\" height=\"{hh:.1}\" rx=\"2\" fill=\"{done_c}\"/>",
            x = cx + 1.0, y = base - done_h, bw = bar_w, hh = done_h,
        ));
        labels.push_str(&format!(
            "<text x=\"{cx:.1}\" y=\"{ly:.1}\" text-anchor=\"middle\" class=\"axl\">{lbl}</text>",
            ly = h - 8.0,
            lbl = esc(&b.label()),
        ));
    }

    let axis = format!(
        "<line x1=\"{pad_l}\" y1=\"{y0:.1}\" x2=\"{pad_l}\" y2=\"{y1:.1}\" class=\"axis\"/>\
         <line x1=\"{pad_l}\" y1=\"{y1:.1}\" x2=\"{xr:.1}\" y2=\"{y1:.1}\" class=\"axis\"/>\
         <text x=\"{tx:.1}\" y=\"{ty:.1}\" text-anchor=\"end\" class=\"axl\">{max:.0}</text>",
        y0 = pad_t,
        y1 = pad_t + plot_h,
        xr = pad_l + plot_w,
        tx = pad_l - 6.0,
        ty = pad_t + 8.0,
    );

    let legend = format!(
        "<rect x=\"{lx:.0}\" y=\"6\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{accent}\"/>\
         <text x=\"{lxx:.0}\" y=\"15\" class=\"axl\">added</text>\
         <rect x=\"{lx2:.0}\" y=\"6\" width=\"10\" height=\"10\" rx=\"2\" fill=\"{done_c}\"/>\
         <text x=\"{lx2x:.0}\" y=\"15\" class=\"axl\">done</text>",
        lx = w - 150.0,
        lxx = w - 136.0,
        lx2 = w - 78.0,
        lx2x = w - 64.0,
    );

    svg_wrap(w, h, &format!("{axis}{bars}{labels}{legend}"), theme, "tp")
}

fn svg_burndown(series: &[chart::RemainingPoint], theme: &Theme) -> String {
    let w = 720.0;
    let h = 220.0;
    let pad_l = 34.0;
    let pad_b = 26.0;
    let pad_t = 12.0;
    let plot_w = w - pad_l - 12.0;
    let plot_h = h - pad_b - pad_t;
    let max = series.iter().map(|p| p.remaining).max().unwrap_or(1).max(1) as f64;
    let n = series.len().max(1);

    let x_at = |i: usize| pad_l + plot_w * (i as f64 / (n - 1).max(1) as f64);
    let y_at = |v: u32| pad_t + plot_h * (1.0 - (v as f64 / max));

    let mut line = String::new();
    let mut area = format!("M {:.1} {:.1}", x_at(0), pad_t + plot_h);
    for (i, p) in series.iter().enumerate() {
        let cmd = if i == 0 { "M" } else { "L" };
        line.push_str(&format!("{cmd} {:.1} {:.1} ", x_at(i), y_at(p.remaining)));
        area.push_str(&format!(" L {:.1} {:.1}", x_at(i), y_at(p.remaining)));
    }
    area.push_str(&format!(" L {:.1} {:.1} Z", x_at(n - 1), pad_t + plot_h));

    let stroke = theme
        .ramp()
        .last()
        .copied()
        .unwrap_or(Rgb::new(0xbf, 0x61, 0x6a))
        .hex();
    let axis = format!(
        "<line x1=\"{pad_l}\" y1=\"{y0:.1}\" x2=\"{pad_l}\" y2=\"{y1:.1}\" class=\"axis\"/>\
         <line x1=\"{pad_l}\" y1=\"{y1:.1}\" x2=\"{xr:.1}\" y2=\"{y1:.1}\" class=\"axis\"/>\
         <text x=\"{tx:.1}\" y=\"{ty:.1}\" text-anchor=\"end\" class=\"axl\">{max:.0}</text>\
         <text x=\"{tx:.1}\" y=\"{by:.1}\" text-anchor=\"end\" class=\"axl\">0</text>",
        y0 = pad_t,
        y1 = pad_t + plot_h,
        xr = pad_l + plot_w,
        tx = pad_l - 6.0,
        ty = pad_t + 8.0,
        by = pad_t + plot_h,
    );

    let first = series.first().map(|p| p.date);
    let last = series.last().map(|p| p.date);
    // ISO date for an axis label. Named rather than inlined so the two ends of
    // the axis cannot be formatted differently by accident.
    let ymd = |d: jiff::civil::Date| format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day());
    let date_labels = match (first, last) {
        (Some(f), Some(l)) => format!(
            "<text x=\"{pad_l}\" y=\"{ly:.1}\" class=\"axl\">{fs}</text>\
             <text x=\"{xr:.1}\" y=\"{ly:.1}\" text-anchor=\"end\" class=\"axl\">{ls}</text>",
            ly = h - 8.0,
            xr = pad_l + plot_w,
            fs = ymd(f),
            ls = ymd(l),
        ),
        _ => String::new(),
    };

    let body = format!(
        "{axis}<path d=\"{area}\" fill=\"url(#burn_ramp)\" opacity=\"0.28\"/>\
         <path d=\"{line}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"2.5\" stroke-linejoin=\"round\"/>\
         {date_labels}"
    );
    svg_wrap(w, h, &body, theme, "burn")
}

/// Wrap chart geometry in a themed `<svg>` with the ramp gradient + axis style.
fn svg_wrap(w: f64, h: f64, inner: &str, theme: &Theme, prefix: &str) -> String {
    let gid = format!("{prefix}_ramp");
    let defs = ramp_stops(theme, &gid);
    format!(
        "<figure><svg viewBox=\"0 0 {w:.0} {h:.0}\" role=\"img\">\
         <defs>{defs}<style>\
         .axis {{ stroke: var(--line); stroke-width: 1; }}\
         .axl {{ fill: var(--muted); font: 11px ui-monospace, monospace; }}\
         </style></defs>{inner}</svg></figure>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    fn synthetic() -> (Value, Value, Value, Value) {
        let summary = json!({
            "groups": [
                { "project": "work.tasqx", "count": 3, "est_total": "PT9H",
                  "tracked_total": "PT3H30M", "overdue": 1 }
            ],
            "generated": "2026-07-15T12:00:00Z"
        });
        let export = json!({
            "tasks": [
                { "id": "018f-a", "short_id": 42, "title": "Ship <the> v1 & freeze",
                  "status": "done", "project": "work.tasqx", "tags": ["release", "api"],
                  "completed": "2026-07-14T09:00:00Z", "due": null, "urgency": 11.8 },
                { "id": "018f-b", "short_id": 43, "title": "Overdue thing",
                  "status": "pending", "project": "work.tasqx", "tags": ["api"],
                  "due": "2020-01-01T00:00:00Z", "urgency": 9.0 }
            ]
        });
        let actionable = json!({
            "tasks": [
                { "short_id": 43, "title": "Overdue thing", "project": "work.tasqx", "urgency": 9.0 }
            ]
        });
        let events = json!({
            "events": [
                { "op": "add",  "ts": "2026-07-10T09:00:00Z", "entity": "task", "entity_id": "018f-a" },
                { "op": "done", "ts": "2026-07-14T09:00:00Z", "entity": "task", "entity_id": "018f-a" },
                { "op": "add",  "ts": "2026-07-11T09:00:00Z", "entity": "task", "entity_id": "018f-b" }
            ]
        });
        (summary, export, actionable, events)
    }

    fn render_with(theme_name: &str) -> String {
        let (summary, export, actionable, events) = synthetic();
        let th = theme::builtin(theme_name).unwrap();
        let now = "2026-07-15T12:00:00Z".to_string();
        Report {
            theme: &th,
            group_by: "project",
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render()
    }

    /// The bug the D24 rework inherits: the summary was fetched with a
    /// hardcoded `status:pending` filter, and `pending` does not include
    /// `active` — so the one task you are working on right now vanished from the
    /// "By project" roll-up. The counts silently disagreed with the Rust-side
    /// open/overdue derivation a few lines below, which uses
    /// `render::status_is_open`. The fix is to pass no filter
    /// and inherit core's default, so both sides answer the same question.
    #[test]
    fn project_summary_counts_the_task_being_worked_on() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "P" })).unwrap(); // D23
        for title in ["waiting", "in-flight", "finished", "abandoned"] {
            e.task_add(&json!({ "title": title, "project": "P" }))
                .unwrap();
        }
        e.task_start(&json!({ "ref": "2" })).unwrap();
        e.task_done(&json!({ "ref": "3" })).unwrap();
        e.task_cancel(&json!({ "ref": "4" })).unwrap();

        let summary = dispatch(
            &e,
            "report.summary",
            &json!({ "group_by": "project", "metrics": ["count"] }),
        )
        .unwrap();
        let g = &summary["groups"][0];
        assert_eq!(g["project"], "P");
        // pending + active + done. The active task is the regression; the
        // cancelled one must stay out (D24).
        assert_eq!(
            g["count"], 3,
            "active must be counted, cancelled must not: {summary:?}"
        );

        // And the generator must actually ask for that unfiltered summary — the
        // rendered row is where the hardcoded filter used to show up as a 1.
        let html = generate(
            &e,
            &theme::builtin("nord").unwrap(),
            &json!({ "group_by": "project", "metrics": ["count"] }),
        )
        .unwrap();
        assert!(
            html.contains("<td class=\"proj\">P</td><td>3</td>"),
            "the By-project row must show 3, not the pending-only 1: {html}"
        );
    }

    /// Now that the HTML path takes the terminal path's own params, `group_by`
    /// arrives with them — and `report.summary` names each group's key after the
    /// axis. A section that kept reading `project` would render a full column of
    /// `(none)` under a heading saying "By project" for `tasqx report status
    /// --html`: correct data, silently mislabelled and unreadable. The axis is
    /// walked from core's own list so a fourth one cannot be added without this
    /// failing (D30).
    #[test]
    fn the_group_section_follows_the_axis_the_caller_asked_for() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "P" })).unwrap();
        e.task_add(&json!({ "title": "one", "project": "P", "priority": "high" }))
            .unwrap();

        for axis in tasqx_core::engine::SUMMARY_GROUP_BY {
            let doc = generate(
                &e,
                &theme::builtin("nord").unwrap(),
                &json!({ "group_by": axis, "metrics": ["count"] }),
            )
            .unwrap();
            let head = title_case(axis);
            assert!(
                doc.contains(&format!("<th>{head}</th>")),
                "{axis}: header not relabelled"
            );
            assert!(
                !doc.contains("<td class=\"proj\">(none)</td>"),
                "{axis}: the row key was read from the wrong column"
            );
        }
    }

    /// #19/D39: the token metrics core rolls up must be rendered on a human
    /// surface. The HTML report carries the full four-bucket breakdown in the
    /// per-group table and four per-bucket header tiles — never a blended
    /// total (D48a) — all as escaped integers, no external references.
    #[test]
    fn per_group_table_and_header_render_token_metrics() {
        let summary = json!({
            "groups": [
                { "project": "work.tasqx", "count": 2, "est_total": "PT1H",
                  "tracked_total": "PT0S", "overdue": 0,
                  "tokens_in": 1000, "tokens_out": 200, "tokens_cache_read": 50,
                  "tokens_cache_creation": 5, "tokens_total": 1255 }
            ],
            "generated": "2026-07-15T12:00:00Z"
        });
        let export = json!({ "tasks": [] });
        let actionable = json!({ "tasks": [] });
        let events = json!({ "events": [] });
        let th = theme::builtin("nord").unwrap();
        let now = "2026-07-15T12:00:00Z".to_string();
        let doc = Report {
            theme: &th,
            group_by: "project",
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render();

        assert!(
            doc.contains("<th>Cache read</th><th>Cache write</th><th>In</th><th>Out</th>"),
            "token columns missing from the By-project table: {doc}"
        );
        // Fixture: in 1000, out 200, cacheR 50, cacheW 5, total 1255. Columns run
        // in `tokens::BUCKETS` order — cacheR, cacheW, in, out.
        assert!(
            doc.contains(
                "<td class=\"muted\">50</td><td class=\"muted\">5</td>\
                 <td class=\"muted\">1.0K</td><td class=\"muted\">200</td>"
            ),
            "token cells missing or mis-ordered: {doc}"
        );
        // D48a, from the other direction: the blend must not survive anywhere on
        // the page. `1255` is the fixture's `tokens_total` and appears in no
        // other field, so its absence is the assertion — a column-shape check
        // alone would pass if the total merely moved.
        assert!(
            !doc.contains("1255"),
            "the blended total is still on the page: {doc}"
        );
        assert!(
            !doc.contains("AI tokens"),
            "the blended header tile is still on the page: {doc}"
        );
        // The four tiles that replaced it, compacted.
        for (n, label) in [
            ("50", "cache read"),
            ("5", "cache write"),
            ("1000", "input"),
            ("200", "output"),
        ] {
            let expected = format!(
                "<div class=\"n\">{}</div><div class=\"l\">{label}</div>",
                crate::tokens::compact(n.parse().unwrap())
            );
            assert!(doc.contains(&expected), "missing tile {label}: {doc}");
        }
    }

    #[test]
    fn report_is_self_contained() {
        let doc = render_with("nord");
        // No external requests of any kind.
        assert!(!doc.contains("http://"), "contains http://");
        assert!(!doc.contains("https://"), "contains https://");
        assert!(!doc.contains("src="), "contains src=");
        assert!(!doc.contains("href="), "contains href=");
        assert!(!doc.contains("<script"), "contains <script");
        // Parses as one document.
        assert!(doc.starts_with("<!doctype html>"));
        assert!(doc.trim_end().ends_with("</html>"));
        assert_eq!(doc.matches("<html").count(), 1);
        assert_eq!(doc.matches("</html>").count(), 1);
    }

    #[test]
    fn report_has_both_color_schemes() {
        let doc = render_with("nord");
        assert!(doc.contains(":root {"), "light scheme root vars");
        assert!(
            doc.contains("@media (prefers-color-scheme: dark)"),
            "dark scheme media query"
        );
        // Palette tokens present for both schemes (light default + dark override).
        assert!(
            doc.matches("--bg:").count() >= 2,
            "--bg defined for both schemes"
        );
        assert!(doc.contains("--accent:"), "accent token present");
    }

    /// The report takes `now` precisely so rendering is a pure function of its
    /// inputs, but the derived 7-day "completed recently" window read the wall
    /// clock instead. The synthetic fixture (completed 2026-07-14, now pinned
    /// 2026-07-15) therefore aged out of the window when the REAL date passed
    /// 2026-07-21, and `report_escapes_user_content` failed on unchanged code.
    /// Pin: a completion recent by the wall clock but ancient relative to the
    /// injected `now` must not render as recent.
    #[test]
    fn recent_window_follows_injected_now_not_wall_clock() {
        let (summary, mut export, actionable, events) = synthetic();
        let wall_yesterday = jiff::Timestamp::now()
            .checked_sub(jiff::ToSpan::hours(24i64))
            .unwrap()
            .to_string();
        export["tasks"][0]["title"] = json!("Wall-clock straggler");
        export["tasks"][0]["completed"] = json!(wall_yesterday);

        let th = theme::builtin("nord").unwrap();
        let now = "2030-01-01T00:00:00Z".to_string();
        let doc = Report {
            theme: &th,
            group_by: "project",
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render();
        assert!(
            !doc.contains("Wall-clock straggler"),
            "a completion years before the injected now is not 'recent'"
        );
    }

    #[test]
    fn report_escapes_user_content() {
        let doc = render_with("nord");
        // The task title's angle brackets/ampersand must be escaped, never raw.
        assert!(doc.contains("Ship &lt;the&gt; v1 &amp; freeze"));
        assert!(!doc.contains("Ship <the> v1"));
    }

    /// `report --html` defaults to **stdout** — the same terminal `render.rs`
    /// carefully sanitizes. Markup escaping alone is not enough: a title holding
    /// OSC/CSI bytes (titles arrive via import, the JSON API and MCP) reached the
    /// terminal raw and was executed by it — `ESC ]0;HIJACKED BEL` rewrites the
    /// window title, `ESC [2J` clears the screen. The terminal path has held
    /// this since `render::san`, the analogue `esc` is modelled on, pinned by
    /// `san_strips_control_and_escape_bytes`; the HTML path holds the same
    /// standard now. Named rather than cited by line number, because a line
    /// number is the reference that rots on the next insertion above it.
    #[test]
    fn html_escaper_strips_terminal_control_bytes() {
        let hostile = "pwn\u{1b}]0;HIJACKED\u{7}\u{1b}[2Jgone";
        let out = esc(hostile);
        assert!(!out.contains('\u{1b}'), "ESC reached the terminal: {out:?}");
        assert!(!out.contains('\u{7}'), "BEL reached the terminal: {out:?}");
        // The readable text survives — this strips control bytes, not content.
        assert!(out.contains("pwn"), "{out:?}");
        assert!(out.contains("gone"), "{out:?}");
        // Newline and tab are legitimate whitespace and must pass through.
        assert_eq!(esc("a\tb\nc"), "a\tb\nc");
        // Markup escaping is unchanged.
        assert_eq!(esc("<a & 'b'>"), "&lt;a &amp; &#39;b&#39;&gt;");
    }

    /// The end-to-end shape of the same bug: a whole rendered report over a
    /// store whose title carries an escape sequence must contain no ESC byte.
    #[test]
    fn a_rendered_report_never_emits_an_escape_byte() {
        let (summary, mut export, actionable, events) = synthetic();
        export["tasks"][0]["title"] = json!("pwn\u{1b}]0;HIJACKED\u{7}gone");
        export["tasks"][1]["project"] = json!("ev\u{1b}[2Jil");
        let th = theme::builtin("nord").unwrap();
        let now = "2026-07-15T12:00:00Z".to_string();
        let doc = Report {
            theme: &th,
            group_by: "project",
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render();
        assert!(
            !doc.contains('\u{1b}'),
            "report --html writes to stdout — no ESC may survive"
        );
    }

    #[test]
    fn report_renders_for_mono_theme() {
        // mono has an empty ramp — the SVG gradient must still be valid.
        let doc = render_with("mono");
        assert!(doc.contains("<linearGradient"));
        assert!(!doc.contains("http://"));
    }

    /// `store.export` is the largest structure the CLI ever holds — every task
    /// with its tags, annotations, dependency ids and token rows. Every reader in
    /// this module is read-only, so the array must be BORROWED out of the payload,
    /// never deep-copied: the three sections used to `.cloned()` it and drop the
    /// copy a few lines later, which on a 2000-task store duplicates the whole
    /// document for nothing.
    ///
    /// Pointer identity is the only way to see that from a test — the rendered
    /// HTML is byte-identical either way. `as_ptr()` equality proves the returned
    /// slice IS the payload's buffer rather than a copy of it, and no cloning
    /// implementation can even satisfy the `-> &[Value]` signature (E0515: it
    /// would return a reference to a local).
    #[test]
    fn the_task_array_is_borrowed_out_of_the_payload_not_copied() {
        let (summary, export, ..) = synthetic();

        let tasks = array_at(&export, "tasks");
        let inside = export["tasks"].as_array().unwrap();
        // Guard the guard: two EMPTY slices share one dangling pointer, so an
        // empty fixture would make the identity check below pass for free.
        assert!(
            !tasks.is_empty(),
            "fixture must carry tasks to prove anything"
        );
        assert_eq!(tasks.len(), inside.len());
        assert!(
            std::ptr::eq(tasks.as_ptr(), inside.as_ptr()),
            "the task array was copied, not borrowed"
        );

        let groups = array_at(&summary, "groups");
        assert!(!groups.is_empty(), "fixture must carry groups");
        assert!(
            std::ptr::eq(
                groups.as_ptr(),
                summary["groups"].as_array().unwrap().as_ptr()
            ),
            "the group array was copied, not borrowed"
        );

        // The absent and wrong-typed cases must stay as forgiving as the
        // `unwrap_or_default()` they replace: an export without `tasks` renders
        // the empty-state sections, it does not panic.
        assert!(array_at(&json!({}), "tasks").is_empty());
        assert!(array_at(&json!({ "tasks": "not an array" }), "tasks").is_empty());
    }
}
