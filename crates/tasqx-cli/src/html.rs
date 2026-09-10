//! Self-contained HTML report (DESIGN.md §8).
//!
//! One file: inline `<style>`, inline SVG charts, a system-font stack — zero
//! external requests (no CDN, no remote fonts/images/scripts). Dark/light via
//! `prefers-color-scheme` over CSS custom properties whose palette is derived
//! from the active tasqx theme, so terminal and web match. All data comes from
//! pure core reads (`report.summary`, `task.list`, `store.export`, `event.list`).

use std::collections::HashMap;

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
    let now_ts = jiff::Timestamp::now();
    let now = now_ts.to_string();
    let from = now_ts
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
        filter,
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

/// The stats `Report::render` prints, computed by [`Report::derive`]: a pure
/// value the assembly half consumes, and the seam that makes each number
/// directly testable.
struct Derived<'a> {
    open: usize,
    overdue: usize,
    overdue_tasks: Vec<&'a Value>,
    completed_recent: Vec<&'a Value>,
    velocity: usize,
    top_tags: Vec<(String, u32)>,
    /// How many DISTINCT tags matched, before `top_tags` was cut to 10
    /// (#235/2) — the section needs this to say how many it left out.
    tags_total: usize,
    bucket_totals: Vec<(&'static str, i64)>,
}

struct Report<'a> {
    theme: &'a Theme,
    /// Which column `summary`'s groups are keyed by — `report.summary` names the
    /// key after the axis, so reading `project` out of a `status` roll-up would
    /// quietly render a table of `(none)`.
    group_by: &'a str,
    /// The report's own filter DSL string, verbatim — `None` for an
    /// unfiltered ("all projects") report. Threaded through so the title and
    /// the header can say what this page is scoped to (#235/1): every report
    /// used to carry the identical title and header regardless of filter, so
    /// four teams' reports were four identically-named tabs with nothing on
    /// the page itself saying which team each covered.
    filter: Option<&'a str>,
    summary: &'a Value,
    export: &'a Value,
    actionable: &'a Value,
    events: &'a Value,
    now: &'a str,
}

impl<'a> Report<'a> {
    /// What this report covers, as shown to a reader — the filter DSL
    /// verbatim, or "all projects" when there is none. Not a translation of
    /// the DSL into prose (`project:a or project:b and @working` has no
    /// tidy English name); the raw string still answers the question the
    /// audit raised (#235/1) — which report, of several, is this one.
    fn scope_label(&self) -> String {
        self.filter
            .map(str::to_string)
            .unwrap_or_else(|| "all projects".to_string())
    }

    /// Every number the header tiles and lists print, derived once — a pure
    /// function of the injected payloads and `now`, split out of `render` so
    /// each stat is reachable by a direct assertion instead of only through a
    /// full-document string test. `render` assembles; this decides.
    fn derive(&self) -> Derived<'a> {
        let tasks = array_at(self.export, "tasks");

        // Derived counts. All windows are measured against the injected `now`,
        // not the wall clock — the derivation must stay a pure function of the
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

        // Velocity: the SAME count as "done this week" (#165), not a second
        // computation over the audit log. This used to count
        // `done` events off the unscoped `event.list` result in the same
        // 7-day window — two tables, two windows that happened to agree only
        // by coincidence, and disagreed by exactly one on the audited store
        // (12 done-this-week vs. 13 velocity/wk in the same minute). Worse,
        // `event.list` carries no filter (D59 bounds it by time only), so a
        // report scoped to a project or tag still counted every task's
        // events — the throughput chart had the identical bug (#162), fixed
        // the same way one call up in `render` via `chart::throughput`'s
        // `members`. Reading it off `completed_recent` fixes both at once:
        // one source, one window, and a number the filter actually scopes.
        let velocity = completed_recent.len();

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
        // Captured before truncating (#235/2): "Top tags" used to cut to 10
        // with nothing saying how many distinct tags were left out.
        let tags_total = top_tags.len();
        top_tags.truncate(10);

        // Report-wide token totals, one per bucket, summed across the summary's
        // groups (which already carry core's D24 scope — cancelled work is
        // excluded unless the caller asked for `all`). Saturating, like the
        // per-group roll-up.
        //
        // D48a: four sums, not one. This used to fold `tokens_total` into a
        // single headline tile, which is the number core deliberately keeps apart
        // until emit — and the one that cannot mean what its label said.
        // `array_at`, not a deep clone: this module's pointer-identity test
        // exists to forbid copying a payload out just to read scalars from it,
        // and the header tiles were the one call site that escaped.
        let groups = array_at(self.summary, "groups");
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
        Derived {
            open,
            overdue,
            overdue_tasks,
            completed_recent,
            velocity,
            top_tags,
            tags_total,
            bucket_totals,
        }
    }

    fn render(&self) -> String {
        let tasks = array_at(self.export, "tasks");
        let d = self.derive();

        // ---- charts ----
        // Through the shared projection, so the report and the dashboard cannot
        // disagree about whether a task was open on a given day — and (#164)
        // so `throughput` can tell a currently-cancelled task from a
        // currently-done one, which the event log alone cannot. `members` also
        // scopes it (#162): `self.events` is `event.list`'s store-wide result
        // (D59 bounds it by time, not by the report's filter), so without this
        // a filtered report drew throughput bars for every task in the store —
        // a zero-match filter still showed the whole store's W30. `burndown`
        // already took this scoping; the chart had no equivalent parameter
        // until now.
        let members = chart::members_of(&json!({ "tasks": tasks }));
        // Anchored on the injected `now`, not the wall clock, for the same
        // reason `derive` is: a fixture pinned to one date must draw the same
        // chart tomorrow.
        let anchor = parse_ts(self.now)
            .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date())
            .unwrap_or_else(today);
        let throughput = chart::throughput(self.events, &members, 12, anchor);
        let burndown = chart::burndown(self.events, &members, 30, anchor);

        // ---- assemble ----
        let css = self.css();
        let mut body = String::new();

        body.push_str(&self.header(
            d.open,
            d.completed_recent.len(),
            d.velocity,
            d.overdue,
            &d.bucket_totals,
        ));
        body.push_str("<main>");
        body.push_str(&self.overdue_section(&d.overdue_tasks));
        body.push_str(&self.actionable_section());

        // "Weekly throughput" — matching the terminal chart's own heading
        // (`chart::render_throughput`) — not "This week's throughput" (#165):
        // the series is 12 WEEKS, and titling it as a single week put a third,
        // disagreeing sense of "this week" on the same page as "done this
        // week" (rolling 7 days) and this very chart's own ISO-week buckets.
        body.push_str(&section(
            "Weekly throughput",
            "Tasks opened versus closed, by ISO week.",
            &svg_throughput(&throughput, self.theme),
        ));
        body.push_str(&section(
            "Open backlog",
            "Remaining open tasks over the last 30 days.",
            // #234 item 6: a store that never held a task is not "cleared",
            // and a chart whose only y-axis label is an invented "1" teaches a
            // wrong mental model to a brand-new user's very first report.
            &if tasks.is_empty() {
                "<p class=\"muted\">No open tasks yet.</p>".to_string()
            } else {
                svg_burndown(&burndown, self.theme)
            },
        ));

        body.push_str(&self.per_group_section());
        body.push_str("<details><summary>Completed in the last 7 days</summary>");
        body.push_str(&self.completed_section(&d.completed_recent));
        body.push_str("</details>");
        body.push_str(&self.tags_section(&d.top_tags, d.tags_total));

        body.push_str("</main>");
        // The raw UTC instant stays reachable in `title` (#235/4) — the
        // visible text switches to local time, which a viewer opening the
        // file minutes after generation reads as fresh rather than "2 hours
        // old" on a UTC+2 machine; `title` is the machine-readable escape
        // hatch `pretty_local_ts` itself does not carry.
        body.push_str(&format!(
            "<footer>Generated <span title=\"{utc}\">{local}</span> · every panel is a pure read of the tasqx core API.</footer>",
            utc = esc(self.now),
            local = esc(&pretty_local_ts(self.now)),
        ));

        // The title names the SCOPE and the date (#235/1), not the theme —
        // every report used to carry the identical `tasqx report · {theme}`
        // regardless of filter, so four teams' reports were four
        // identically-titled browser tabs.
        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>tasqx · {scope} · {date}</title>\n<style>\n{css}\n</style>\n</head>\n\
             <body>\n{body}\n</body>\n</html>\n",
            scope = esc(&self.scope_label()),
            date = date_part(self.now),
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
    ///
    /// `velocity` carries its window in the label (`velocity /wk (7d)`, #165):
    /// it used to be silent about it, which read as directly comparable to
    /// the terminal's differently-windowed "4-wk velocity" — two numbers
    /// nobody could tell apart without opening both. `velocity` also now
    /// equals `done` (both come from `Report::derive`'s `completed_recent`),
    /// so the two tiles can no longer disagree either.
    ///
    /// The brand block carries a `.scope` line under "weekly review" (#235/1)
    /// — the same string the `<title>` uses — so a printed or screenshotted
    /// page (which loses the tab title) still says what it covers.
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
               <div class=\"brand-block\">\
                 <div class=\"brand\">tasqx <span class=\"muted\">weekly review</span></div>\
                 <div class=\"scope\">{scope}</div>\
               </div>\
               <div class=\"stats\">{}{}{}{}{token_tiles}</div>\
             </header>",
            stat(&open.to_string(), "open"),
            stat(&done.to_string(), "done this week"),
            stat(&velocity.to_string(), "velocity /wk (7d)"),
            stat_flag(&overdue.to_string(), "overdue", overdue > 0),
            scope = esc(&self.scope_label()),
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
            // #129: this used to be `class="warn"`, styled with `--warn`
            // (a pale gold, even after the #163 contrast fix) and no
            // weight — a different color and weight from the header's
            // overdue tile (`--danger`, bold), so the same signal read as
            // urgent up top and as a footnote down here. `.overdue-flag`
            // reuses the header's own treatment so a non-zero cell reads
            // as loudly as the count it agrees with.
            let od = if overdue > 0 {
                format!("<td class=\"overdue-flag\">{overdue}</td>")
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
            // #217: `report.summary` carries the group's WORST confidence
            // (D50's trust hierarchy) in `tokens_confidence` whenever a token
            // metric was requested. Rendered as its own column rather than
            // folded into a bucket cell — the page had a `confidence` string
            // nowhere on it before, only a same-named CSS/JS token, so this
            // must be unmistakably a data column.
            let confidence_cell = match g.get("tokens_confidence").and_then(Value::as_str) {
                Some(c) => format!("<td class=\"muted\">{}</td>", esc(c)),
                None => "<td class=\"muted\">—</td>".to_string(),
            };
            rows.push_str(&format!(
                "<tr><td class=\"proj\">{name}</td><td>{count}</td><td>{est}</td><td>{tracked}</td>{od}{tokens_cells}{confidence_cell}</tr>",
                name = esc(name),
            ));
        }
        // `.table-wrap` (#166 + #235/3): a nine-column table cannot shrink
        // below its content's intrinsic width, so on a 375px phone it forced
        // the whole PAGE into horizontal scroll — dragging the sticky header
        // sideways with it. Scoping `overflow-x: auto` to this wrapper keeps
        // an overflowing table's scroll local to the table, on any viewport.
        let table = format!(
            "<div class=\"table-wrap\"><table class=\"grid\"><thead><tr><th>{head}</th><th>Tasks</th><th>Est</th><th>Tracked</th><th>Overdue</th>\
             <th>Cache read</th><th>Cache write</th><th>In</th><th>Out</th><th>Confidence</th></tr></thead><tbody>{rows}</tbody></table></div>",
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

    /// #235/2: this list truncates at `task.list`'s own `limit: 12` with
    /// nothing saying so — a stakeholder reading "12 actionable" beside a
    /// header saying "46 open" cannot tell whether the list is complete or
    /// merely cut. `total` is `task.list`'s own answer to that (D70: rows
    /// matched, vs. `count`'s rows returned); a trailing muted row states the
    /// gap when the two differ.
    fn actionable_section(&self) -> String {
        let tasks = array_at(self.actionable, "tasks");
        if tasks.is_empty() {
            return section(
                "Now actionable",
                "Nothing unblocked and pending — you're clear.",
                "",
            );
        }
        let total = self
            .actionable
            .get("total")
            .and_then(Value::as_u64)
            .map_or(tasks.len(), |n| n as usize);
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
        if total > tasks.len() {
            rows.push_str(&format!(
                "<li class=\"more muted\">…and {} more</li>",
                total - tasks.len()
            ));
        }
        section(
            "Now actionable",
            "The highest-urgency unblocked tasks — start at the top.",
            &format!("<ul class=\"tasklist\">{rows}</ul>"),
        )
    }

    /// `total` is the DISTINCT tag count before `derive` truncated to 10
    /// (#235/2) — the same "how much did this cut" question as
    /// `actionable_section`, over a list core never gets to paginate for us,
    /// so this half of the fix is computed locally rather than read off a
    /// server-side `total`.
    fn tags_section(&self, tags: &[(String, u32)], total: usize) -> String {
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
        if total > tags.len() {
            chips.push_str(&format!(
                "<span class=\"tag muted\">+{} more</span>",
                total - tags.len()
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
        let color = |name: &str, fallback: Rgb| -> Rgb {
            self.theme.palette_color(name).unwrap_or(fallback)
        };
        let accent = color("accent", Rgb::new(0x88, 0xc0, 0xd0));
        let warn = color("warn", Rgb::new(0xeb, 0xcb, 0x8b));
        let danger = color("danger", Rgb::new(0xbf, 0x61, 0x6a));
        let muted_dark = color("muted", Rgb::new(0x4c, 0x56, 0x6a)).hex();
        let bg_dark = color("bg", Rgb::new(0x2e, 0x34, 0x40)).hex();
        let fg_dark = color("fg", Rgb::new(0xd8, 0xde, 0xe9)).hex();

        // #163: these three roles are picked for a dark terminal ground and
        // reused verbatim on the light scheme used to make mono's white
        // accent/warn/danger literally invisible on the white card (1:1) and
        // put every other built-in's `warn` under 3.3:1 — nowhere near WCAG
        // AA's 4.5:1 text floor. The dark-scheme value is untouched (it is
        // the theme's own color, at its own contrast against its own
        // background, exactly as before); only the light scheme gets a
        // darkened variant computed to clear AA against white.
        let white = Rgb::new(0xff, 0xff, 0xff);
        let accent_l = darkened_for_contrast(accent, white, 4.5).hex();
        let warn_l = darkened_for_contrast(warn, white, 4.5).hex();
        let danger_l = darkened_for_contrast(danger, white, 4.5).hex();
        let accent_d = accent.hex();
        let warn_d = warn.hex();
        let danger_d = danger.hex();

        format!(
            ":root {{\n\
             /* light scheme (default) */\n\
             --accent: {accent_l};\n--warn: {warn_l};\n--danger: {danger_l};\n\
             --bg: #ffffff;\n--fg: #1a1d23;\n--muted: #6b7280;\n--card: #f6f7f9;\n--line: #e3e6ea;\n\
             }}\n\
             @media (prefers-color-scheme: dark) {{\n:root {{\n\
             --accent: {accent_d};\n--warn: {warn_d};\n--danger: {danger_d};\n\
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
             .scope {{ color: var(--muted); font-size: 0.78rem; margin-top: 0.15rem; }}\n\
             .stats {{ display: flex; gap: 1.4rem; flex-wrap: wrap; row-gap: 0.6rem; }}\n\
             .stat {{ text-align: right; flex: 0 0 auto; }}\n\
             .stat .n {{ font-size: 1.5rem; font-weight: 700; line-height: 1; font-variant-numeric: tabular-nums;\n\
             font-family: ui-monospace, monospace; }}\n\
             .stat .l {{ font-size: 0.72rem; color: var(--muted); text-transform: uppercase; letter-spacing: 0.06em; }}\n\
             .stat.flag .n {{ color: var(--danger); }}\n\
             section {{ margin-top: 2.2rem; }}\n\
             section > h2 {{ font-size: 1.05rem; margin: 0 0 0.15rem; letter-spacing: -0.01em; }}\n\
             section > .sub {{ color: var(--muted); font-size: 0.85rem; margin: 0 0 0.9rem; }}\n\
             .muted {{ color: var(--muted); }} .warn {{ color: var(--warn); }}\n\
             .overdue-flag {{ color: var(--danger); font-weight: 700; }}\n\
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
             .table-wrap {{ overflow-x: auto; }}\n\
             table.grid {{ width: 100%; border-collapse: collapse; font-size: 0.9rem; }}\n\
             table.grid th {{ text-align: left; color: var(--muted); font-weight: 600; font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.05em; border-bottom: 1px solid var(--line); padding: 0.4rem 0.5rem; }}\n\
             table.grid td {{ padding: 0.4rem 0.5rem; border-bottom: 1px solid var(--line); font-variant-numeric: tabular-nums; }}\n\
             table.grid td.proj {{ font-weight: 600; }}\n\
             /* #235/3: the table and the charts are data, not prose — the same\n\
                ~72ch measure that suits running text forced a nine-column table\n\
                to wrap project names to three lines beside ~660px of unused\n\
                viewport at 1280px. Above ~900px both break out of `main`'s\n\
                column toward 1100px; narrower than that they stay the prose\n\
                width and (for the table) scroll in their own container. */\n\
             @media (min-width: 900px) {{\n\
             .table-wrap, figure {{ width: 100vw; max-width: min(1100px, calc(100vw - 2.5rem));\n\
             margin-left: 50%; transform: translateX(-50%); }}\n\
             }}\n\
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

// ---- WCAG contrast (#163) --------------------------------------------------
//
// Theme roles (`accent`/`warn`/`danger`) are colors picked for a dark
// terminal ground. The report used to hand them to the light scheme
// verbatim, so `mono`'s white accent — 21:1 against its own dark background —
// became 1:1 (invisible) on the light card, and every other built-in theme's
// `warn` landed between 1.1:1 and 3.2:1 on white, all under the 4.5:1 WCAG AA
// floor for text. These three functions compute that ratio and, where it
// fails, darken the color just enough to clear it — one algorithm covering
// every current and future theme rather than a second hand-picked palette.

/// WCAG relative luminance of an sRGB color (0.0 = black, 1.0 = white).
fn relative_luminance(c: Rgb) -> f64 {
    let chan = |v: u8| -> f64 {
        let v = f64::from(v) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * chan(c.r) + 0.7152 * chan(c.g) + 0.0722 * chan(c.b)
}

/// WCAG contrast ratio between two colors, order-independent, in `[1.0, 21.0]`.
fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Darken `c` toward black just enough that it clears `min_contrast` against
/// `bg` — `c` unchanged if it already does. Binary search over the mix
/// fraction rather than a closed-form solve: contrast against a light `bg`
/// rises monotonically as a color darkens toward black (which always clears
/// AA against white/near-white), so 24 bisection steps land within
/// 1/16-million of the mix ratio, far tighter than an 8-bit channel can
/// represent — plenty for a value that only has to clear a threshold, not
/// hit one exactly.
fn darkened_for_contrast(c: Rgb, bg: Rgb, min_contrast: f64) -> Rgb {
    if contrast_ratio(c, bg) >= min_contrast {
        return c;
    }
    let mix = |t: f64| -> Rgb {
        let ch = |v: u8| -> u8 { (f64::from(v) * (1.0 - t)).round() as u8 };
        Rgb::new(ch(c.r), ch(c.g), ch(c.b))
    };
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if contrast_ratio(mix(mid), bg) >= min_contrast {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(hi)
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

/// The footer's "Generated" timestamp, in the generating machine's OWN local
/// zone with its abbreviation (`2026-09-09 12:42 CEST`) — #235/4. Due dates
/// stay in `pretty_ts`'s UTC (D53 fixes UTC as the store's and the parser's
/// zone so a typed date round-trips; converting a `due` midnight-UTC instant
/// to local time can roll the CALENDAR DAY a reader sees backward west of
/// Greenwich, the exact failure D53 exists to prevent). The footer carries no
/// such risk — it names a moment, not a day — and "generated 2 hours ago"
/// reading as fresh rather than stale is worth doing here even though the
/// full cross-surface humanizing D76's recorded edges left as follow-up work
/// is not. A report is generated on one machine and opened on another, so
/// "local" means the generator's zone, not the reader's; the raw UTC instant
/// stays reachable in the `title` attribute the caller wraps this in for
/// exactly that gap. Falls back to the plain instant on any parse/format
/// failure — never a panic in a read path.
fn pretty_local_ts(s: &str) -> String {
    match s.parse::<jiff::Timestamp>() {
        Ok(t) => t
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%Y-%m-%d %H:%M %Z")
            .to_string(),
        Err(_) => s.to_string(),
    }
}

/// The `YYYY-MM-DD` (UTC) prefix of an RFC3339 instant, for the report
/// `<title>` (#235/1) — a calendar date reads better in a browser tab/PDF
/// export than a full timestamp, and UTC keeps it a pure function of `now`
/// rather than of the rendering machine's zone.
fn date_part(s: &str) -> String {
    match s.parse::<jiff::Timestamp>() {
        Ok(t) => {
            let d = t.to_zoned(jiff::tz::TimeZone::UTC).date();
            format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
        }
        Err(_) => s.to_string(),
    }
}

/// ISO-8601 duration → `19h 30m` (or `—` for zero).
///
/// `pub(crate)` so `render::report` (#234 item 11) can print the same human
/// duration the HTML report and the dashboard already use, instead of the
/// machine `PT5H53S` form the terminal used to be alone in showing — `PT5H53S`
/// is five hours and fifty-three SECONDS and reads at a glance as five hours
/// fifty-three minutes, an 87x error in a column a freelancer invoices from.
pub(crate) fn humanize_iso(iso: &str) -> String {
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

/// Structural self-containment check over a rendered document (D48b).
///
/// Walks the markup and judges attribute values, `<style>` bodies and
/// `<script>` bodies only. Text nodes are never inspected: task titles and
/// annotation bodies reach the page, and real ones already quote
/// `https://`, `@import` and `pushState` as prose.
#[cfg(test)]
pub(crate) mod guard {
    const BANNED_TAGS: &[&str] = &["link", "iframe", "object", "embed", "base"];
    const FETCHING_ATTRS: &[&str] = &[
        "src",
        "srcset",
        "poster",
        "action",
        "formaction",
        "ping",
        "data",
    ];
    /// What the one inline script may not reach for: the network, the History
    /// API (a `SecurityError` on `file://`), and dynamic code.
    const SCRIPT_BANS: &[&str] = &[
        "fetch(",
        "XMLHttpRequest",
        "WebSocket",
        "EventSource",
        "importScripts",
        "import(",
        "pushState",
        "replaceState",
        "eval(",
        "new Function",
    ];

    struct Tag {
        name: String,
        closing: bool,
        attrs: Vec<(String, String)>,
        /// Byte offset just past the closing `>`.
        end: usize,
    }

    pub(crate) fn violations(doc: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut scripts = 0usize;
        let mut rest = doc;
        while let Some(lt) = rest.find('<') {
            rest = &rest[lt..];
            if let Some(after) = rest.strip_prefix("<!--") {
                rest = after.find("-->").map_or("", |i| &after[i + 3..]);
                continue;
            }
            let Some(tag) = parse_tag(rest) else {
                rest = &rest[1..];
                continue;
            };
            rest = &rest[tag.end..];
            if tag.closing {
                continue;
            }
            let name = tag.name.as_str();
            if BANNED_TAGS.contains(&name) {
                out.push(format!("<{name}> fetches or frames off-file content"));
            }
            for (attr, value) in &tag.attrs {
                let attr = attr.as_str();
                if attr == "href" || attr == "xlink:href" {
                    if !value.starts_with('#') || value.len() < 2 {
                        out.push(format!(
                            "<{name} {attr}=\"{value}\"> is not an in-page anchor"
                        ));
                    }
                } else if FETCHING_ATTRS.contains(&attr) {
                    out.push(format!(
                        "<{name} {attr}=\"{value}\"> loads an external resource"
                    ));
                } else if attr.starts_with("on") {
                    out.push(format!("<{name} {attr}> is an inline event handler"));
                } else if attr == "style" {
                    css_violations(value, &format!("<{name} style>"), &mut out);
                }
            }
            match name {
                "style" => {
                    let (body, next) = raw_body(rest, "</style>");
                    css_violations(body, "<style>", &mut out);
                    rest = next;
                }
                "script" => {
                    scripts += 1;
                    let (body, next) = raw_body(rest, "</script>");
                    for ban in SCRIPT_BANS {
                        if body.contains(ban) {
                            out.push(format!("<script> uses `{ban}`"));
                        }
                    }
                    rest = next;
                }
                _ => {}
            }
        }
        if scripts > 1 {
            out.push(format!("{scripts} <script> elements; the budget is one"));
        }
        out
    }

    fn css_violations(css: &str, at: &str, out: &mut Vec<String>) {
        if css.contains("@import") {
            out.push(format!("{at} contains @import"));
        }
        for (i, _) in css.match_indices("url(") {
            let target = css[i + 4..].trim_start_matches([' ', '\t', '\n', '"', '\'']);
            if !target.starts_with('#') {
                out.push(format!("{at} contains url() to something off-file"));
            }
        }
    }

    /// The raw body of a `<style>`/`<script>` element and what follows its
    /// closing tag. Raw text may hold a bare `<`, so it is skipped as one
    /// span rather than parsed for tags.
    fn raw_body<'a>(rest: &'a str, close: &str) -> (&'a str, &'a str) {
        match rest.find(close) {
            Some(i) => (&rest[..i], &rest[i + close.len()..]),
            None => (rest, ""),
        }
    }

    /// `s` starts at `<`. `None` for a `<` that opens no element
    /// (`<!doctype`, a stray bracket), which the caller steps over.
    fn parse_tag(s: &str) -> Option<Tag> {
        let b = s.as_bytes();
        let mut i = 1;
        let closing = b.get(i) == Some(&b'/');
        if closing {
            i += 1;
        }
        let name_start = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b':' || b[i] == b'-') {
            i += 1;
        }
        if i == name_start {
            return None;
        }
        let name = s[name_start..i].to_ascii_lowercase();
        let mut attrs = Vec::new();
        loop {
            while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
                i += 1;
            }
            if i >= b.len() {
                return None;
            }
            if b[i] == b'>' {
                return Some(Tag {
                    name,
                    closing,
                    attrs,
                    end: i + 1,
                });
            }
            let attr_start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/')
            {
                i += 1;
            }
            let attr = s[attr_start..i].to_ascii_lowercase();
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let mut value = String::new();
            if b.get(i) == Some(&b'=') {
                i += 1;
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if let Some(&q) = b.get(i).filter(|q| **q == b'"' || **q == b'\'') {
                    i += 1;
                    let v_start = i;
                    while i < b.len() && b[i] != q {
                        i += 1;
                    }
                    value = s[v_start..i].to_string();
                    i += 1;
                } else {
                    let v_start = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    value = s[v_start..i].to_string();
                }
            }
            attrs.push((attr, value));
        }
    }
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
                // The title quotes, as prose, every string the self-containment
                // guard bans in markup: a substring scan over the document
                // fails on this row; the structural guard must not.
                { "id": "018f-b", "short_id": 43,
                  "title": "Overdue thing: see https://example.com, no @import, no url(x), never pushState or fetch(",
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
            filter: None,
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
            filter: None,
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

    /// #129: the header's overdue tile is `--danger` (red) and bold
    /// (`.stat.flag .n`); the By-project table's OVERDUE column used
    /// `--warn`, a pale gold even after #163's contrast fix, at ordinary
    /// weight — the same signal read as urgent up top and as a footnote in
    /// the table. A non-zero cell must now carry the header's own treatment
    /// (`.overdue-flag`, `--danger`, bold); a zero cell must stay the plain
    /// `muted` styling zero already had.
    #[test]
    fn overdue_table_cells_match_the_headers_warning_treatment() {
        let summary = json!({
            "groups": [
                { "project": "loud", "count": 1, "est_total": "PT0S",
                  "tracked_total": "PT0S", "overdue": 3 },
                { "project": "quiet", "count": 1, "est_total": "PT0S",
                  "tracked_total": "PT0S", "overdue": 0 }
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
            filter: None,
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render();

        assert!(
            doc.contains("<td class=\"overdue-flag\">3</td>"),
            "a non-zero overdue cell must carry the header's warning treatment: {doc}"
        );
        assert!(
            doc.contains("<td class=\"muted\">0</td>"),
            "a zero overdue cell must stay neutral: {doc}"
        );
        assert!(
            !doc.contains("<td class=\"warn\">"),
            "the low-contrast `.warn` class must no longer be used for the overdue cell: {doc}"
        );
        assert!(
            doc.contains(".overdue-flag")
                && doc.contains("var(--danger)")
                && doc.contains("font-weight: 700"),
            "the CSS must give `.overdue-flag` the header's own color and weight: {doc}"
        );
    }

    /// #217: `report.summary` carries `tokens_confidence` (D50's trust
    /// hierarchy, the group's worst measurement), but the HTML by-project
    /// table had no column for it at all — a `grep -c confidence` over the
    /// rendered page found one hit, and it was a CSS/JS token, not data. A
    /// low-confidence group must get a visible, named marker.
    #[test]
    fn per_group_table_renders_a_confidence_column() {
        let summary = json!({
            "groups": [
                { "project": "work.tasqx", "count": 2, "est_total": "PT1H",
                  "tracked_total": "PT0S", "overdue": 0,
                  "tokens_in": 1000, "tokens_out": 200, "tokens_cache_read": 50,
                  "tokens_cache_creation": 5, "tokens_confidence": "low" }
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
            filter: None,
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render();

        assert!(
            doc.contains("<th>Confidence</th>"),
            "the by-project table has no confidence column: {doc}"
        );
        assert!(
            doc.contains(">low<"),
            "the group's low confidence never reached the page: {doc}"
        );
    }

    /// D48b: judged structurally, over attribute values and `<style>`/`<script>`
    /// bodies. The fixture's overdue title quotes `https://`, `@import`, `url(`,
    /// `pushState` and `fetch(` as prose, which the substring scan this
    /// replaced failed on — a guard that fails on prose is one that gets
    /// weakened by whoever hits it next.
    #[test]
    fn report_is_self_contained() {
        let doc = render_with("nord");
        let found = guard::violations(&doc);
        assert!(found.is_empty(), "{found:#?}");
        // Parses as one document.
        assert!(doc.starts_with("<!doctype html>"));
        assert!(doc.trim_end().ends_with("</html>"));
        assert_eq!(doc.matches("<html").count(), 1);
        assert_eq!(doc.matches("</html>").count(), 1);
    }

    /// The guard's own contract: silent on prose, and it bites on every drift
    /// class D48b names — each one injected here, so a rule that stops firing
    /// is a red test rather than a quiet gap.
    #[test]
    fn self_containment_guard_judges_markup_not_prose() {
        let prose = "<p>https://x.example @import url(x.png) pushState fetch( &lt;script&gt; src=</p>\
                     <style>.a { fill: url(#ramp); }</style>\
                     <svg><defs><linearGradient id=\"ramp\"/></defs><rect fill=\"url(#ramp)\"/></svg>\
                     <a href=\"#task-1\">t</a>\
                     <script>window.addEventListener('hashchange', () => { if (a < b) {} });</script>";
        assert_eq!(guard::violations(prose), Vec::<String>::new());

        let drifts = [
            ("<link rel=\"stylesheet\" href=\"x.css\">", "a <link>"),
            ("<a href=\"https://x.example\">x</a>", "an external href"),
            ("<a href=\"#\">x</a>", "an empty anchor"),
            ("<img src=\"x.png\">", "a src="),
            ("<style>@import url(x.css);</style>", "a CSS @import"),
            (
                "<style>.a { background: url(x.png); }</style>",
                "a CSS url()",
            ),
            (
                "<div style=\"background: url('x.png')\"></div>",
                "a url() in a style attribute",
            ),
            ("<script>fetch('x')</script>", "fetch in the script"),
            (
                "<script>history.pushState({}, '')</script>",
                "pushState in the script",
            ),
            ("<script>1</script><script>2</script>", "a second script"),
            (
                "<button onclick=\"go()\">x</button>",
                "an inline event handler",
            ),
            ("<iframe></iframe>", "an <iframe>"),
        ];
        for (markup, what) in drifts {
            assert!(
                !guard::violations(markup).is_empty(),
                "the guard missed {what}: {markup}"
            );
        }
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
            filter: None,
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

    /// #162 (scope) + #165 (two windows, two answers): the header's "velocity
    /// /wk" tile counted `done` events straight off the unscoped `event.list`
    /// result, while the neighbouring "done this week" tile counted completed
    /// tasks off the (correctly scoped) export — two tables, two windows that
    /// happened to agree only by coincidence, and the audit's own repro
    /// (46 open / 12 done this week / 13 velocity /wk on one store, in the
    /// same minute) is this exact drift. A `done` event for a task the
    /// filter excluded inflated velocity alone. After the fix both tiles read
    /// off `completed_recent`, so they cannot disagree and an out-of-scope
    /// event can no longer move only one of them.
    #[test]
    fn velocity_matches_completed_this_week_and_ignores_events_outside_the_scoped_export() {
        let (summary, export, actionable, mut events) = synthetic();
        // A 'done' event for a task NOT in the scoped export — as if it
        // belonged to a project this report's filter excluded.
        events["events"].as_array_mut().unwrap().push(json!({
            "op": "done", "ts": "2026-07-14T09:00:00Z", "entity": "task",
            "entity_id": "OUT-OF-SCOPE"
        }));
        let th = theme::builtin("nord").unwrap();
        let now = "2026-07-15T12:00:00Z".to_string();
        let report = Report {
            theme: &th,
            group_by: "project",
            filter: None,
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        };
        let d = report.derive();
        assert_eq!(
            d.velocity,
            d.completed_recent.len(),
            "velocity ({}) must equal done-this-week ({}) — same source, same window",
            d.velocity,
            d.completed_recent.len()
        );
        assert_eq!(
            d.velocity, 1,
            "the out-of-scope task's done event must not inflate velocity: {}",
            d.velocity
        );
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
            filter: None,
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

    /// #163: `--accent`/`--warn`/`--danger` were emitted once, with the
    /// theme's dark-terminal value, and reused verbatim on the light
    /// scheme's white ground — mono's white-on-white accent/warn/danger are
    /// 1:1 (invisible; the overdue count, task ids, tag counts and the
    /// throughput chart's "added" bars all use these roles). Every built-in's
    /// light-mode value for these three roles must clear the WCAG AA text
    /// floor (4.5:1) against white.
    #[test]
    fn theme_roles_meet_aa_contrast_on_the_light_scheme_background() {
        let white = Rgb::new(0xff, 0xff, 0xff);
        for name in theme::BUILTINS {
            let doc = render_with(name);
            let dark_at = doc
                .find("@media (prefers-color-scheme: dark)")
                .unwrap_or_else(|| panic!("{name}: no dark media block: {doc}"));
            // The LIGHT scheme's declarations come first in `:root {}`, before
            // the dark block; searching only that prefix cannot pick up the
            // dark-block redefinition of the same property by accident.
            let light_css = &doc[..dark_at];
            for role in ["--accent:", "--warn:", "--danger:"] {
                let at = light_css
                    .find(role)
                    .unwrap_or_else(|| panic!("{name}: {role} missing from light css: {doc}"));
                let rest = &light_css[at + role.len()..];
                let hex = rest[..rest.find(';').unwrap()].trim();
                let rgb = Rgb::parse_hex(hex)
                    .unwrap_or_else(|| panic!("{name}: unparseable {role} {hex:?}"));
                let ratio = contrast_ratio(rgb, white);
                assert!(
                    ratio >= 4.5,
                    "{name} {role} {hex} on white is {ratio:.2}:1, under WCAG AA's 4.5:1 — #163"
                );
            }
        }
    }

    /// #165: "velocity /wk" carried no window, reading as directly comparable
    /// to the terminal's differently-windowed "4-wk velocity" with nothing on
    /// either surface saying they measure different spans. And "This week's
    /// throughput" mislabeled a 12-WEEK series as a single week — a third,
    /// disagreeing sense of "this week" beside "done this week" (rolling 7
    /// days) and the chart's own ISO-week buckets.
    #[test]
    fn velocity_states_its_window_and_the_throughput_heading_does_not_claim_a_single_week() {
        let doc = render_with("nord");
        assert!(
            doc.contains("velocity /wk (7d)"),
            "the velocity tile must name its window: {doc}"
        );
        assert!(
            !doc.contains("This week's throughput"),
            "a 12-week series must not be titled as a single week: {doc}"
        );
        assert!(
            doc.contains("Weekly throughput"),
            "retitled to match the terminal chart's own heading: {doc}"
        );
    }

    /// #166: at 390px the eight-tile `.stats` strip (628px, unwrappable) drags
    /// the WHOLE PAGE into horizontal scroll, which is also why the sticky
    /// header (which only sticks vertically) slides sideways with it. And the
    /// nine-column by-project table sits bare in `<section>` with no scroll
    /// container of its own, so it is the page — not the table — that
    /// scrolls. Both must be fixed for the phone-width symptom to go away:
    /// letting `.stats` wrap keeps the page's own width fixed, and giving the
    /// table its own `overflow-x: auto` box keeps an overflowing table's
    /// scroll local to the table.
    #[test]
    fn stats_strip_wraps_and_the_wide_table_gets_its_own_scroll_container() {
        let doc = render_with("nord");
        let stats_at = doc.find(".stats {").expect(".stats rule missing");
        assert!(
            doc[stats_at..stats_at + 200].contains("flex-wrap"),
            "the header stat strip must be allowed to wrap onto more than one row: {doc}"
        );
        assert!(
            doc.contains("<div class=\"table-wrap\"><table class=\"grid\">"),
            "the by-project table must scroll inside its own container, not the page: {doc}"
        );
    }

    /// #235/1: every report carried the identical `<title>tasqx report ·
    /// {theme}</title>` regardless of its filter, and the rendered header
    /// read "tasqx weekly review" on all of them — so four teams' reports
    /// were four identically-titled tabs with nothing on the page itself
    /// saying which team each covered. The title must name the scope (or
    /// "all projects") and drop the theme name; the same scope string must
    /// also appear on the page.
    #[test]
    fn title_and_header_name_the_reports_scope_not_its_theme() {
        let doc_all = render_with("nord");
        assert!(
            !doc_all.contains("<title>tasqx report · nord</title>"),
            "the theme name must not be the thing distinguishing two reports: {doc_all}"
        );
        assert!(
            doc_all.contains("all projects"),
            "an unfiltered report must say so, on the page: {doc_all}"
        );

        let (summary, export, actionable, events) = synthetic();
        let th = theme::builtin("nord").unwrap();
        let now = "2026-07-15T12:00:00Z".to_string();
        let doc_scoped = Report {
            theme: &th,
            group_by: "project",
            filter: Some("project:finly-mail-agent"),
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render();
        assert!(
            doc_scoped.contains("project:finly-mail-agent"),
            "the filter must reach the title or header: {doc_scoped}"
        );
        assert!(
            !doc_scoped.contains("all projects"),
            "a scoped report must not also claim to cover everything: {doc_scoped}"
        );
    }

    /// #235/4: the footer stamped UTC unconditionally, so a report generated
    /// at 12:42 CEST read "Generated ... 10:42 UTC" — two hours stale-looking
    /// the moment it was opened. The human-facing text may now show local
    /// time, but the raw instant must stay reachable somewhere a machine (or
    /// a curious reader) can still read it exactly.
    #[test]
    fn footer_carries_the_raw_utc_instant_for_machine_readers_while_showing_local_time() {
        let doc = render_with("nord");
        assert!(
            doc.contains("<footer>Generated <span title=\"2026-07-15T12:00:00Z\">"),
            "the footer must keep the raw UTC instant reachable, e.g. in a title attribute: {doc}"
        );
    }

    /// #235/2: "Now actionable" truncates at `task.list`'s own `limit: 12`
    /// and "Top tags" at `.truncate(10)`, both with no indication that
    /// anything was left out — a stakeholder reading "12 actionable" beside a
    /// header saying "46 open" cannot tell whether the list is complete or
    /// merely cut. `task.list` already answers `total` (D70); this test uses
    /// that plus more than 10 distinct tags to pin the trailing note both
    /// sections must now render.
    #[test]
    fn actionable_and_tags_sections_note_how_much_they_truncated() {
        let actionable = json!({
            "tasks": [{ "short_id": 1, "title": "one", "project": "P", "urgency": 5.0 }],
            "total": 46
        });
        let export = json!({
            "tasks": (1..=12).map(|i| json!({
                "id": format!("t{i}"), "short_id": i, "title": format!("task {i}"),
                "status": "pending", "tags": [format!("tag{i:02}")],
            })).collect::<Vec<_>>()
        });
        let summary = json!({ "groups": [] });
        let events = json!({ "events": [] });
        let th = theme::builtin("nord").unwrap();
        let now = "2026-07-15T12:00:00Z".to_string();
        let doc = Report {
            theme: &th,
            group_by: "project",
            filter: None,
            summary: &summary,
            export: &export,
            actionable: &actionable,
            events: &events,
            now: &now,
        }
        .render();
        assert!(
            doc.contains("…and 45 more"),
            "the actionable list (1 of 46 shown) must say how many more matched: {doc}"
        );
        assert!(
            doc.contains("+2 more"),
            "top tags (10 of 12 distinct shown) must say how many more: {doc}"
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
