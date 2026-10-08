//! Self-contained HTML report (DESIGN.md §8, D212).
//!
//! One file: inline `<style>`, inline SVG, a system-font stack, one inline
//! script — zero external requests. It leads with what changed in a period
//! (a band of five numbers against the span before it), then a standup, a
//! per-project grid whose rows scope the page, a net-flow chart, outcomes and
//! a search over every task in scope.
//!
//! Every number is computed here in Rust from pure core reads
//! (`store.export`, `report.outcomes`, `task.list`) and rendered into the
//! markup — once for the whole report and once per project. The script picks
//! which of those blocks shows, searches a compact index of the tasks and
//! opens a task in an overlay; it never aggregates (D116's ruling against a
//! second roll-up in the browser).

use std::collections::{HashMap, HashSet};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use serde_json::{json, Value};
use tasqx_core::{dispatch, ApiError, Engine};

use crate::chart::{self, Lifecycle, Member};
use crate::theme::{Rgb, Theme};

/// Newest annotations embedded per task under `--with-notes`, and the
/// characters kept of each.
const ANNOTATIONS_PER_TASK: usize = 3;
const ANNOTATION_CHARS: usize = 2000;
/// Weekly bins the chart and the sparklines draw at most; a younger store
/// draws fewer, so no bin predates the store.
const MAX_BINS: i64 = 26;
/// Bins the phone-width chart keeps, newest last.
const NARROW_BINS: usize = 13;
/// Rows a standup list shows before "+N more".
const STANDUP_ROWS: usize = 6;
/// The inline script's ceiling in bytes, so growth is a red test rather than
/// a slow drift.
#[cfg(test)]
const SCRIPT_BUDGET: usize = 8 * 1024;

/// What `report --html` embeds beyond the default page.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// `--with-notes`: annotation bodies go into the file, behind a toggle.
    pub with_notes: bool,
    /// `--all`: cancelled tasks join the search index.
    pub all: bool,
}

/// `params` is the SAME payload the terminal `tasqx report` builds in
/// `report_params`, so a filter scopes both modes alike; `filter`, `since` and
/// `until` are read back out of it. Here `since`/`until` are the page's
/// period (D212), resolved to instants already.
pub fn generate(
    engine: &Engine,
    theme: &Theme,
    params: &Value,
    opts: Options,
) -> Result<String, ApiError> {
    generate_at(engine, theme, params, opts, crate::clock::now())
}

/// [`generate`] at an injected `now`, so a fixture renders the same page on
/// any calendar day.
fn generate_at(
    engine: &Engine,
    theme: &Theme,
    params: &Value,
    opts: Options,
    now: Timestamp,
) -> Result<String, ApiError> {
    let filter = params.get("filter").and_then(Value::as_str);
    let period = Period::new(params, now)?;
    let export = dispatch(engine, "store.export", &scoped(json!({}), filter))?;
    // The engine's own outcomes over the same period, so every figure in that
    // section is the one `report --outcomes --since … --until …` prints.
    let outcomes = dispatch(
        engine,
        "report.outcomes",
        &scoped(
            json!({
                "group_by": "project",
                "since": period.since.to_string(),
                "until": period.until.to_string(),
            }),
            filter,
        ),
    )?;
    // Blocked and startable are the engine's answers, not a re-derivation.
    let blocked = short_ids(engine, filter, "@blocked")?;
    let working = short_ids(engine, filter, "@working")?;
    let members = chart::members_of(&export);
    let model = Model::new(&export, &members, &blocked, &working, now, period);
    let page = Page {
        theme,
        filter,
        opts,
        m: &model,
        outcomes: &outcomes,
    };
    Ok(page.render())
}

/// Add `filter` to a params object, or leave it absent. Absent, not `null`:
/// core reads a missing key as "no filter" and would reject a null.
fn scoped(mut params: Value, filter: Option<&str>) -> Value {
    if let Some(f) = filter {
        params["filter"] = Value::String(f.to_string());
    }
    params
}

/// Every short id `task.list` answers for `extra` ANDed with the report's
/// filter, page by page. Parenthesised because the DSL has `or`.
fn short_ids(engine: &Engine, filter: Option<&str>, extra: &str) -> Result<HashSet<i64>, ApiError> {
    let f = match filter {
        Some(f) => format!("({f}) and {extra}"),
        None => extra.to_string(),
    };
    let mut out = HashSet::new();
    let mut offset = 0u64;
    loop {
        let page = dispatch(
            engine,
            "task.list",
            &json!({ "filter": f, "fields": ["short_id"], "offset": offset,
                     "limit": tasqx_core::engine::task::MAX_TASK_LIST_LIMIT }),
        )?;
        out.extend(
            array_at(&page, "tasks")
                .iter()
                .filter_map(|t| t.get("short_id").and_then(Value::as_i64)),
        );
        match page.get("next_offset").and_then(Value::as_u64) {
            Some(next) if next > offset => offset = next,
            _ => return Ok(out),
        }
    }
}

// ============================================================================
// The period
// ============================================================================

/// The days the page counts, inclusive, in UTC. `until` is exclusive, so
/// `--until 2026-10-01` ends the period on 30 Sep.
#[derive(Clone, Copy, Debug)]
struct Period {
    since: Timestamp,
    until: Timestamp,
    first: Date,
    last: Date,
    /// Days in the period; the prior period is as long and ends the day
    /// before `first`.
    days: i64,
    /// Whether `--since`/`--until` chose it, or it is the default 7 days.
    custom: bool,
}

impl Period {
    fn new(params: &Value, now: Timestamp) -> Result<Period, ApiError> {
        let read = |key: &str| -> Result<Option<Timestamp>, ApiError> {
            params
                .get(key)
                .and_then(Value::as_str)
                .map(|s| {
                    s.parse::<Timestamp>().map_err(|_| {
                        ApiError::bad_request(format!("`{key}` is not an instant: {s:?}"))
                    })
                })
                .transpose()
        };
        let (since_p, until_p) = (read("since")?, read("until")?);
        let until = until_p.unwrap_or(now);
        let last = day_of(until.checked_sub(1.nanosecond()).unwrap_or(until));
        let since = since_p.unwrap_or_else(|| midnight(last.saturating_sub(6.days())));
        if until <= since {
            return Err(ApiError::bad_request(format!(
                "`until` ({until}) must be after `since` ({since})"
            )));
        }
        let first = day_of(since);
        Ok(Period {
            since,
            until,
            first,
            last,
            days: days_between(first, last) + 1,
            custom: since_p.is_some() || until_p.is_some(),
        })
    }

    fn prior(&self) -> (Date, Date) {
        (
            self.first.saturating_sub(self.days.days()),
            self.first.saturating_sub(1.day()),
        )
    }

    /// `the prior 7 days`, the comparison every delta on the page is against.
    fn against(&self) -> String {
        match self.days {
            1 => "the prior day".to_string(),
            n => format!("the prior {n} days"),
        }
    }

    /// The instant a day's state is read at: its end, or `until` when the
    /// period stops inside it (today, by default).
    fn read_at(&self, d: Date) -> Timestamp {
        midnight(d.saturating_add(1.day())).min(self.until)
    }
}

fn day_of(t: Timestamp) -> Date {
    t.to_zoned(TimeZone::UTC).date()
}

fn midnight(d: Date) -> Timestamp {
    d.to_zoned(TimeZone::UTC)
        .map(|z| z.timestamp())
        .unwrap_or(Timestamp::UNIX_EPOCH)
}

fn days_between(a: Date, b: Date) -> i64 {
    b.since(a).map(|s| i64::from(s.get_days())).unwrap_or(0)
}

// ============================================================================
// The model: one record per exported task, and the questions asked of it
// ============================================================================

struct Task<'a> {
    v: &'a Value,
    short: i64,
    title: &'a str,
    project: &'a str,
    status: &'a str,
    priority: &'a str,
    created: Timestamp,
    completed: Option<Timestamp>,
    due: Option<Timestamp>,
    est: Option<i64>,
    tracked: i64,
    /// Indexes of the in-export tasks this one depends on, and of the ones
    /// that depend on it.
    deps: Vec<usize>,
    blocks: Vec<usize>,
    /// The engine says it is blocked now, by something this export does not
    /// carry (a filter drops edges that leave its scope).
    blocked_outside: bool,
    blocked_now: bool,
    working: bool,
    started: Option<Timestamp>,
    reopened: Option<Timestamp>,
    checks: (usize, usize),
    member: Option<&'a Member>,
}

struct Model<'a> {
    tasks: Vec<Task<'a>>,
    moves: HashMap<&'a str, Vec<(Timestamp, Lifecycle)>>,
    now: Timestamp,
    p: Period,
    /// The earliest `created` in the export — the chart's left edge.
    store_start: Date,
    /// `(project, task indexes)`, busiest first; the page's scopes.
    projects: Vec<(&'a str, Vec<usize>)>,
}

/// One weekly bin of the chart and the sparklines.
struct Bin {
    start: Date,
    added: i64,
    done: i64,
    net: i64,
    open: i64,
    blocked: i64,
    overdue: i64,
    /// Overlaps the period.
    hl: bool,
}

impl<'a> Model<'a> {
    fn new(
        export: &'a Value,
        members: &'a [Member],
        blocked: &HashSet<i64>,
        working: &HashSet<i64>,
        now: Timestamp,
        p: Period,
    ) -> Model<'a> {
        let rows = array_at(export, "tasks");
        let by_id: HashMap<&str, &Member> = members.iter().map(|m| (m.id.as_str(), m)).collect();
        let moves = chart::lifecycle_moves(export, members);
        let ts = |t: &Value, k: &str| t.get(k).and_then(Value::as_str).and_then(parse_ts);
        let s = |t: &'a Value, k: &str| t.get(k).and_then(Value::as_str).unwrap_or("");

        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut tasks: Vec<Task<'a>> = Vec::with_capacity(rows.len());
        for t in rows {
            let (Some(short), Some(created)) =
                (t.get("short_id").and_then(Value::as_i64), ts(t, "created"))
            else {
                continue;
            };
            let id = s(t, "id");
            index.insert(id, tasks.len());
            let checks = t
                .get("checks")
                .and_then(Value::as_array)
                .map(|cs| {
                    let passed = cs
                        .iter()
                        .filter(|c| c.get("state").and_then(Value::as_str) == Some("passed"))
                        .count();
                    (cs.len(), passed)
                })
                .unwrap_or((0, 0));
            tasks.push(Task {
                v: t,
                short,
                title: s(t, "title"),
                project: s(t, "project"),
                status: s(t, "status"),
                priority: s(t, "priority"),
                created,
                completed: ts(t, "completed"),
                due: ts(t, "due"),
                est: t
                    .get("estimate")
                    .and_then(Value::as_str)
                    .and_then(duration_secs)
                    .filter(|e| *e > 0),
                tracked: t
                    .get("tracked_seconds")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                deps: Vec::new(),
                blocks: Vec::new(),
                blocked_outside: false,
                blocked_now: blocked.contains(&short),
                working: working.contains(&short),
                started: None,
                reopened: None,
                checks,
                member: by_id.get(id).copied(),
            });
        }
        for i in 0..tasks.len() {
            let deps: Vec<usize> = tasks[i]
                .v
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|ds| {
                    ds.iter()
                        .filter_map(Value::as_str)
                        .filter_map(|d| index.get(d).copied())
                        .collect()
                })
                .unwrap_or_default();
            for &d in &deps {
                tasks[d].blocks.push(i);
            }
            tasks[i].deps = deps;
        }
        // The newest start and reopen per task, off the export's own events.
        for ev in array_at(export, "events") {
            let op = ev.get("op").and_then(Value::as_str).unwrap_or("");
            if op != "start" && op != "reopen" {
                continue;
            }
            let (Some(i), Some(at)) = (
                ev.get("entity_id")
                    .and_then(Value::as_str)
                    .and_then(|id| index.get(id)),
                ts(ev, "ts"),
            ) else {
                continue;
            };
            let slot = if op == "start" {
                &mut tasks[*i].started
            } else {
                &mut tasks[*i].reopened
            };
            if slot.is_none_or(|was| at > was) {
                *slot = Some(at);
            }
        }
        let store_start = tasks
            .iter()
            .map(|t| day_of(t.created))
            .min()
            .unwrap_or_else(|| day_of(now));
        let mut m = Model {
            tasks,
            moves,
            now,
            p,
            store_start,
            projects: Vec::new(),
        };
        let today = day_of(now);
        for i in 0..m.tasks.len() {
            let outside =
                m.tasks[i].blocked_now && !m.tasks[i].deps.iter().any(|&d| m.open_on(d, today));
            m.tasks[i].blocked_outside = outside;
        }
        let mut by_project: HashMap<&'a str, Vec<usize>> = HashMap::new();
        for (i, t) in m.tasks.iter().enumerate() {
            by_project.entry(t.project).or_default().push(i);
        }
        let mut projects: Vec<(&str, Vec<usize>)> = by_project.into_iter().collect();
        let last = m.p.last;
        projects.sort_by(|a, b| {
            let open = |ix: &[usize]| ix.iter().filter(|&&i| m.open_on(i, last)).count();
            open(&b.1).cmp(&open(&a.1)).then(a.0.cmp(b.0))
        });
        m.projects = projects;
        m
    }

    /// Whether task `i` was open at the end of day `d` — the burndown's own
    /// replay (`chart::open_on`), so the page and `tasqx chart burndown`
    /// cannot disagree about a day.
    fn open_on(&self, i: usize, d: Date) -> bool {
        let t = &self.tasks[i];
        match t.member {
            Some(m) => chart::open_on(m, d, self.moves.get(m.id.as_str()).map(Vec::as_slice)),
            None => false,
        }
    }

    /// Open at the end of `d` with an in-export blocker open then too, or
    /// blocked now by one outside the export. ponytail: past days read
    /// today's dependency edges; a dependency removed since is not replayed.
    fn blocked_on(&self, i: usize, d: Date) -> bool {
        self.open_on(i, d)
            && (self.tasks[i].blocked_outside
                || self.tasks[i].deps.iter().any(|&j| self.open_on(j, d)))
    }

    /// ponytail: past days read today's `due`; a due date moved since is not
    /// replayed.
    fn overdue_on(&self, i: usize, d: Date) -> bool {
        self.open_on(i, d)
            && self.tasks[i]
                .due
                .is_some_and(|due| tasqx_core::filter::overdue_at(due, self.p.read_at(d)))
    }

    fn is(&self, i: usize, status: &str) -> bool {
        self.tasks[i].status == status
    }

    fn done_in(&self, ix: &[usize], a: Date, b: Date) -> i64 {
        ix.iter()
            .filter(|&&i| {
                self.is(i, "done")
                    && self.tasks[i]
                        .completed
                        .is_some_and(|c| (a..=b).contains(&day_of(c)))
            })
            .count() as i64
    }

    /// D24: a task cancelled since is not new work.
    fn added_in(&self, ix: &[usize], a: Date, b: Date) -> i64 {
        ix.iter()
            .filter(|&&i| {
                !self.is(i, "cancelled") && (a..=b).contains(&day_of(self.tasks[i].created))
            })
            .count() as i64
    }

    fn count(&self, ix: &[usize], d: Date, f: impl Fn(&Self, usize, Date) -> bool) -> i64 {
        ix.iter().filter(|&&i| f(self, i, d)).count() as i64
    }

    fn open_at(&self, ix: &[usize], d: Date) -> i64 {
        self.count(ix, d, Model::open_on)
    }

    fn bins(&self, ix: &[usize]) -> Vec<Bin> {
        let span = days_between(self.store_start, self.p.last) + 1;
        let n = ((span + 6) / 7).clamp(1, MAX_BINS);
        (0..n)
            .rev()
            .map(|k| {
                let end = self.p.last.saturating_sub((7 * k).days());
                let start = end.saturating_sub(6.days());
                let before = start.saturating_sub(1.day());
                let open = self.open_at(ix, end);
                Bin {
                    start,
                    added: self.added_in(ix, start, end),
                    done: self.done_in(ix, start, end),
                    net: open - self.open_at(ix, before),
                    open,
                    blocked: self.count(ix, end, Model::blocked_on),
                    overdue: self.count(ix, end, Model::overdue_on),
                    hl: end >= self.p.first && start <= self.p.last,
                }
            })
            .collect()
    }

    /// Tracked ÷ estimate of every task done in the period with both.
    fn ratios(&self, ix: &[usize]) -> Vec<f64> {
        let mut out: Vec<f64> = ix
            .iter()
            .map(|&i| &self.tasks[i])
            .filter(|t| {
                t.status == "done"
                    && t.tracked > 0
                    && t.completed
                        .is_some_and(|c| (self.p.first..=self.p.last).contains(&day_of(c)))
            })
            .filter_map(|t| t.est.map(|e| t.tracked as f64 / e as f64))
            .collect();
        out.sort_by(|a, b| a.total_cmp(b));
        out
    }

    /// Length of the blocked chain under task `i`, itself included.
    fn depth(&self, i: usize, seen: &mut HashSet<usize>) -> usize {
        if !seen.insert(i) {
            return 1;
        }
        let today = day_of(self.now);
        let d = self.tasks[i]
            .deps
            .iter()
            .filter(|&&j| self.open_on(j, today) && self.tasks[j].blocked_now)
            .map(|&j| 1 + self.depth(j, seen))
            .max()
            .unwrap_or(1);
        seen.remove(&i);
        d
    }
}

/// The engine's median: the middle sample, or the mean of the middle two.
fn median(sorted: &[f64]) -> Option<f64> {
    let n = sorted.len();
    match n {
        0 => None,
        n if n.is_multiple_of(2) => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
        n => Some(sorted[n / 2]),
    }
}

// ============================================================================
// The page
// ============================================================================

struct Page<'a> {
    theme: &'a Theme,
    filter: Option<&'a str>,
    opts: Options,
    m: &'a Model<'a>,
    outcomes: &'a Value,
}

/// One scope a block is rendered for: `*` for the whole report, else a
/// project's name.
struct Scope<'s> {
    key: &'s str,
    ix: &'s [usize],
}

impl Scope<'_> {
    fn all(&self) -> bool {
        self.key == "*"
    }
}

impl<'a> Page<'a> {
    /// One project in the export and a filter that chose it: the page is that
    /// project's own, with no scopes to switch between.
    fn single(&self) -> Option<&'a str> {
        match (self.filter, self.m.projects.as_slice()) {
            (Some(_), [(p, _)]) => Some(p),
            _ => None,
        }
    }

    fn render(&self) -> String {
        let all: Vec<usize> = (0..self.m.tasks.len()).collect();
        let mut scopes = vec![Scope { key: "*", ix: &all }];
        if self.single().is_none() {
            scopes.extend(self.m.projects.iter().map(|(p, ix)| Scope {
                key: p,
                ix: ix.as_slice(),
            }));
        }
        let each = |f: &dyn Fn(&Scope) -> String, tag: &str, class: &str| -> String {
            scopes
                .iter()
                .map(|s| {
                    format!(
                        "<{tag} data-sc=\"{key}\"{hidden}{class}>{body}</{tag}>",
                        key = esc(s.key),
                        hidden = if s.all() { "" } else { " hidden" },
                        class = if class.is_empty() {
                            String::new()
                        } else {
                            format!(" class=\"{class}\"")
                        },
                        body = f(s),
                    )
                })
                .collect()
        };

        let mut b = String::with_capacity(256 * 1024);
        b.push_str(&self.header());
        b.push_str(&format!(
            "<p class=\"lede\" id=\"lede\">{}</p>",
            each(&|s| self.lede(s), "span", "")
        ));
        b.push_str(&format!(
            "<div id=\"band\" aria-label=\"The period at a glance\">{}<p class=\"storeline\">Store started {} · {} · sparklines are one point per 7 days, shaded = the period · times in UTC</p></div>",
            each(&|s| self.band(s), "div", "band"),
            long_date(self.m.store_start),
            count(self.m.bins(&[]).len(), "week") + " of history",
        ));
        b.push_str(&format!(
            "<section id=\"standup\" aria-labelledby=\"h-su\"><h2 id=\"h-su\">Standup</h2>\
             <p class=\"sub\">As of the snapshot, {}, whatever the period.</p>{}</section>",
            esc(&stamp(self.m.now)),
            each(&|s| self.standup(s), "div", "su"),
        ));
        b.push_str(&self.grid(&all));
        b.push_str(&format!(
            "<section id=\"flow\" aria-labelledby=\"h-ch\"><h2 id=\"h-ch\">Net flow per 7 days</h2>\
             <p class=\"sub\">Since the store started; shaded bins overlap the period. Bars and the net line share one scale.</p>\
             <ul class=\"legend\"><li><i class=\"k-add\"></i>added</li><li><i class=\"k-done\"></i>done</li>\
             <li><i class=\"k-net\"></i>net backlog change</li><li><i class=\"k-bl\"></i>open backlog (lower lane)</li>\
             <li><i class=\"k-hl\"></i>the period</li></ul>{}</section>",
            each(&|s| self.chart(s), "div", ""),
        ));
        b.push_str(&format!(
            "<section id=\"outcomes\" aria-labelledby=\"h-out\"><h2 id=\"h-out\">Outcomes</h2>\
             <p class=\"sub\">Tasks closed in the period, as <code>tasqx report --outcomes</code> counts them. \
             Unproven = done without every acceptance check passed.</p>{}</section>",
            each(&|s| self.outcomes(s), "div", ""),
        ));
        b.push_str(&self.search());
        b.push_str(&self.footer());
        b.push_str("</div><dialog id=\"dd\" aria-labelledby=\"dd-title\"><div id=\"dd-body\"></div></dialog>");

        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>tasqx · {scope} · {range}</title>\n<style>\n{palette}{CSS}</style>\n</head>\n\
             <body>\n<div class=\"wrap\">{b}\n<script>\n{SCRIPT}\n</script>\n</body>\n</html>\n",
            scope = esc(&self.scope_label()),
            range = esc(&range(self.m.p.first, self.m.p.last)),
            palette = palette(self.theme),
        )
    }

    fn scope_label(&self) -> String {
        self.filter
            .map(str::to_string)
            .unwrap_or_else(|| "all projects".to_string())
    }

    fn header(&self) -> String {
        let p = &self.m.p;
        let name = if p.custom { "Period" } else { "Last 7 days" };
        let notes = if self.opts.with_notes {
            "<label class=\"tog\"><input type=\"checkbox\" id=\"notes\"><span>Show annotation bodies</span></label>"
        } else {
            ""
        };
        let scope = match self.single() {
            Some(p) => format!(
                "<span class=\"chip scoped\">{} only</span>",
                esc(p)
            ),
            None => "<button type=\"button\" class=\"chip on\" id=\"allp\" data-reset aria-pressed=\"true\">All projects</button> \
                     <span class=\"chip scoped needs-js\" id=\"scoped\" hidden><span id=\"scoped-name\"></span>\
                     <button type=\"button\" data-reset aria-label=\"Clear project scope\">×</button></span>"
                .to_string(),
        };
        let filter = self
            .filter
            .map(|f| format!("<span>filter <code>{}</code></span>", esc(f)))
            .unwrap_or_default();
        format!(
            "<header class=\"top\"><div><h1>tasqx <span>review</span></h1>\
             <p class=\"win\">{name}: {range} · against {against} · snapshot <span title=\"{iso}\">{stamp}</span></p></div>\
             <div class=\"controls needs-js\">{notes}<button type=\"button\" class=\"btn\" id=\"theme\">Theme: auto</button></div></header>\
             <div class=\"scopebar\"><span>Scope</span>{scope}{filter}</div>",
            range = esc(&range(p.first, p.last)),
            against = p.against(),
            iso = esc(&self.m.now.to_string()),
            stamp = esc(&stamp(self.m.now)),
        )
    }

    // ---- band -------------------------------------------------------------

    /// The period's five numbers, each against the prior period.
    fn figures(&self, ix: &[usize]) -> [(&'static str, i64, i64); 5] {
        let m = self.m;
        let p = &m.p;
        let (pa, pb) = p.prior();
        let before = p.first.saturating_sub(1.day());
        let before_prior = pa.saturating_sub(1.day());
        let net = m.open_at(ix, p.last) - m.open_at(ix, before);
        let net_prior = m.open_at(ix, pb) - m.open_at(ix, before_prior);
        [
            (
                "done",
                m.done_in(ix, p.first, p.last),
                m.done_in(ix, pa, pb),
            ),
            (
                "added",
                m.added_in(ix, p.first, p.last),
                m.added_in(ix, pa, pb),
            ),
            ("net", net, net_prior),
            (
                "blocked",
                m.count(ix, p.last, Model::blocked_on),
                m.count(ix, pb, Model::blocked_on),
            ),
            (
                "overdue",
                m.count(ix, p.last, Model::overdue_on),
                m.count(ix, pb, Model::overdue_on),
            ),
        ]
    }

    fn band(&self, s: &Scope) -> String {
        let m = self.m;
        let figs = self.figures(s.ix);
        let bins = m.bins(s.ix);
        let at = m.p.read_at(m.p.last);
        let soon =
            s.ix.iter()
                .filter(|&&i| {
                    m.open_on(i, m.p.last)
                        && m.tasks[i].due.is_some_and(|d| {
                            d > at && d <= at.checked_add(72.hours()).unwrap_or(at)
                        })
                })
                .count();
        let cmp = format!("vs {}", m.p.against());
        let mut out = String::new();
        for (key, cur, prior) in figs {
            let (label, val, extra, judge): (&str, String, String, Judge) = match key {
                "done" => ("Done", cur.to_string(), String::new(), Judge::UpGood),
                "added" => ("Added", cur.to_string(), String::new(), Judge::Neutral),
                "net" => (
                    "Net backlog",
                    signed(cur),
                    format!("open {}", m.open_at(s.ix, m.p.last)),
                    Judge::UpBad,
                ),
                "blocked" => ("Blocked", cur.to_string(), String::new(), Judge::UpBad),
                _ => (
                    "Overdue",
                    cur.to_string(),
                    if soon > 0 {
                        format!("{soon} due in 3 d")
                    } else {
                        String::new()
                    },
                    Judge::UpBad,
                ),
            };
            let series: Vec<i64> = bins
                .iter()
                .map(|b| match key {
                    "done" => b.done,
                    "added" => b.added,
                    "net" => b.net,
                    "blocked" => b.blocked,
                    _ => b.overdue,
                })
                .collect();
            let hl: Vec<bool> = bins.iter().map(|b| b.hl).collect();
            out.push_str(&format!(
                "<div class=\"metric\" data-k=\"{key}\"><div class=\"mlabel\">{label}</div>\
                 <div class=\"mval\">{val}{extra}</div><div class=\"mdelta\">{chip} {cmp}</div>{spark}</div>",
                extra = if extra.is_empty() {
                    String::new()
                } else {
                    format!("<small>{extra}</small>")
                },
                chip = delta_chip(cur - prior, judge),
                spark = sparkline(&series, &hl),
            ));
        }
        out
    }

    /// The biggest change against the prior period, in one sentence.
    fn lede(&self, s: &Scope) -> String {
        let figs = self.figures(s.ix);
        let against = self.m.p.against();
        let weight = |k: &str| match k {
            "done" => 1.2,
            "added" => 0.9,
            "overdue" => 1.1,
            _ => 1.0,
        };
        let mut best: Option<(&str, i64, i64, f64)> = None;
        for (k, c, p) in figs {
            let score = (c - p).abs() as f64 / (p.abs().max(5) as f64) * weight(k);
            if best.is_none_or(|b| score > b.3) {
                best = Some((k, c, p, score));
            }
        }
        let Some((k, c, p, _)) = best else {
            return String::new();
        };
        if c == p {
            return format!(
                "Nothing moved much against {against}: {} done, {} added.",
                count(figs[0].1 as usize, "task"),
                figs[1].1
            );
        }
        let up = c > p;
        let pct = if p > 0 {
            format!(" {}%", ((c - p).abs() * 100 + p / 2) / p)
        } else {
            String::new()
        };
        let rose = if up { "rose" } else { "fell" };
        let what = match k {
            "done" => format!("completions {rose}{pct} to {c} (from {p})"),
            "added" => format!(
                "new work {rose}{pct} to {} (from {p})",
                count(c as usize, "task")
            ),
            "net" if c >= 0 => format!("the backlog grew by {c}, against {} before", signed(p)),
            "net" => format!("the backlog shrank by {}, against {} before", -c, signed(p)),
            "blocked" => format!(
                "blocked tasks went {} from {p} to {c}",
                if up { "up" } else { "down" }
            ),
            _ => format!("overdue tasks {rose} from {p} to {c}"),
        };
        let mut driver = String::new();
        if s.all() && self.single().is_none() {
            let mut top: Option<(&str, i64)> = None;
            for (name, ix) in &self.m.projects {
                let f = self.figures(ix);
                let (_, pc, pp) = f
                    .iter()
                    .find(|(key, _, _)| *key == k)
                    .copied()
                    .unwrap_or(("", 0, 0));
                let d = pc - pp;
                if (d > 0) == up && top.is_none_or(|t| d.abs() > t.1.abs()) {
                    top = Some((name, d));
                }
            }
            if let Some((name, d)) = top.filter(|t| t.1.abs() >= 2) {
                driver = format!("; {} accounts for {}", esc(name), signed(d));
            }
        }
        format!("Biggest change against {against}: {what}{driver}.")
    }

    // ---- standup ----------------------------------------------------------

    fn standup(&self, s: &Scope) -> String {
        let m = self.m;
        let now = m.now;
        let today = day_of(now);
        let yesterday = today.saturating_sub(1.day());
        let t = |i: usize| &m.tasks[i];
        let pj = |i: usize| {
            if s.all() && self.single().is_none() {
                format!("{} · ", esc(t(i).project))
            } else {
                String::new()
            }
        };

        let mut done: Vec<usize> = s
            .ix
            .iter()
            .copied()
            .filter(|&i| m.is(i, "done") && t(i).completed.is_some_and(|c| day_of(c) == yesterday))
            .collect();
        done.sort_by_key(|&i| std::cmp::Reverse(t(i).completed));
        let done_rows = list(&done, "Nothing finished yesterday.", |i| {
            let x = t(i);
            let unproven = x.checks.0 > 0 && x.checks.1 < x.checks.0;
            let time = match (x.tracked > 0, x.est) {
                (true, Some(e)) => format!(
                    "{} tracked of {} est.",
                    minutes(Some(x.tracked)),
                    minutes(Some(e))
                ),
                (true, None) => format!("{} tracked, no estimate", minutes(Some(x.tracked))),
                (false, Some(e)) => format!("no time tracked, {} est.", minutes(Some(e))),
                (false, None) => "no time tracked, no estimate".to_string(),
            };
            format!(
                "<li class=\"st-done\">{}<span class=\"meta\">{}{time}{}</span></li>",
                tlink(x),
                pj(i),
                if unproven { " · <b>unproven</b>" } else { "" }
            )
        });

        let since = |x: &Task| x.started.unwrap_or(x.created);
        let mut active: Vec<usize> =
            s.ix.iter()
                .copied()
                .filter(|&i| m.is(i, "active"))
                .collect();
        active.sort_by_key(|&i| since(t(i)));
        let active_rows = list(&active, "Nothing started.", |i| {
            let x = t(i);
            let age = secs(now, since(x));
            let stale = age > 5 * 86_400;
            format!(
                "<li class=\"{}\">{}<span class=\"meta\">{}started {} ago{}</span></li>",
                if stale { "st-over" } else { "st-active" },
                tlink(x),
                pj(i),
                age_text(age),
                if stale { " · <b>stale</b>" } else { "" }
            )
        });

        let mut blocked: Vec<(usize, usize)> =
            s.ix.iter()
                .copied()
                .filter(|&i| t(i).blocked_now)
                .map(|i| (i, m.depth(i, &mut HashSet::new())))
                .collect();
        blocked.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(prio_rank(t(a.0).priority).cmp(&prio_rank(t(b.0).priority)))
                .then(t(a.0).created.cmp(&t(b.0).created))
        });
        let ids: Vec<usize> = blocked.iter().map(|b| b.0).collect();
        let depth: HashMap<usize, usize> = blocked.iter().copied().collect();
        let blocked_rows = list(&ids, "Nothing blocked.", |i| {
            let x = t(i);
            let on = x
                .deps
                .iter()
                .copied()
                .find(|&j| m.open_on(j, today))
                .map(|j| format!("#{} <b>{}</b>", t(j).short, esc(t(j).title)))
                .unwrap_or_else(|| {
                    "<span class=\"redact\">a task outside this report</span>".to_string()
                });
            let d = depth.get(&i).copied().unwrap_or(1);
            format!(
                "<li class=\"st-blocked\">{}<span class=\"meta\">{}waiting on {on}{}</span></li>",
                tlink(x),
                pj(i),
                if d > 1 {
                    format!(" · chain of {}", d + 1)
                } else {
                    String::new()
                }
            )
        });

        let mut next: Vec<(usize, f64, String)> =
            s.ix.iter()
                .copied()
                .filter(|&i| t(i).working && m.is(i, "pending") && !t(i).blocked_now)
                .map(|i| {
                    let x = t(i);
                    let mut score = 0.0;
                    let mut why: Vec<String> = Vec::new();
                    if let Some(due) = x.due {
                        if due < now {
                            score += 100.0 + secs(now, due) as f64 / 86_400.0;
                            why.push(format!("overdue by {}", age_text(secs(now, due))));
                        } else if secs(due, now) < 4 * 86_400 {
                            score += 60.0;
                            why.push(format!("due {}", day_name(day_of(due))));
                        }
                    }
                    match x.priority {
                        "H" => {
                            score += 40.0;
                            why.push("high priority".to_string());
                        }
                        "M" => score += 10.0,
                        _ => {}
                    }
                    let unblocks = x.blocks.iter().filter(|&&j| m.open_on(j, today)).count();
                    if unblocks > 0 {
                        score += 12.0 * unblocks as f64;
                        why.push(format!("unblocks {}", count(unblocks, "task")));
                    }
                    if let Some(r) = x.reopened {
                        score += 15.0;
                        why.push(format!("reopened {} ago", age_text(secs(now, r))));
                    }
                    score += secs(now, x.created) as f64 / 86_400.0 * 0.3;
                    if why.is_empty() {
                        why.push(format!(
                            "oldest ready task, open {}",
                            age_text(secs(now, x.created))
                        ));
                    }
                    why.truncate(2);
                    (i, score, why.join(" · "))
                })
                .collect();
        next.sort_by(|a, b| b.1.total_cmp(&a.1).then(t(a.0).short.cmp(&t(b.0).short)));
        next.truncate(5);
        let reasons: HashMap<usize, String> = next.iter().map(|n| (n.0, n.2.clone())).collect();
        let next_ids: Vec<usize> = next.iter().map(|n| n.0).collect();
        let next_rows = list(&next_ids, "Nothing ready.", |i| {
            let x = t(i);
            let over = x.due.is_some_and(|d| d < now);
            format!(
                "<li class=\"{}\">{}<span class=\"meta\">{}<b>{}</b></span></li>",
                if over { "st-over" } else { "" },
                tlink(x),
                pj(i),
                esc(reasons.get(&i).map_or("", String::as_str))
            )
        });

        format!(
            "<div><h3>Done yesterday <span class=\"count\">{}</span></h3>{done_rows}</div>\
             <div><h3>In progress <span class=\"count\">{}</span></h3>{active_rows}</div>\
             <div><h3>Blocked <span class=\"count\">{}</span></h3>{blocked_rows}</div>\
             <div><h3>Next 5</h3>{next_rows}</div>",
            done.len(),
            active.len(),
            ids.len(),
        )
    }

    // ---- projects ---------------------------------------------------------

    fn group(&self, project: &str) -> Option<&'a Value> {
        array_at(self.outcomes, "groups")
            .iter()
            .find(|g| g.get("project").and_then(Value::as_str) == Some(project))
    }

    /// The engine's per-project median and its `n`.
    fn calibration(&self, project: &str) -> Option<(f64, i64)> {
        let c = self.group(project)?.get("calibration")?;
        Some((c.get("median_ratio")?.as_f64()?, c.get("n")?.as_i64()?))
    }

    /// Tokens per measured completion in the period, as its largest bucket —
    /// never one blended number (D48a).
    fn tokens_per_done(&self, projects: &[&str]) -> String {
        let mut sum: HashMap<&str, i64> = HashMap::new();
        let mut n = 0i64;
        for p in projects {
            let Some(cost) = self.group(p).and_then(|g| g.get("cost")) else {
                continue;
            };
            n += cost.get("n").and_then(Value::as_i64).unwrap_or(0);
            for (key, _, _) in crate::tokens::BUCKETS {
                *sum.entry(key).or_insert(0) += cost.get(key).and_then(Value::as_i64).unwrap_or(0);
            }
        }
        if n == 0 {
            return "<span class=\"dz\">—</span>".to_string();
        }
        let per: serde_json::Map<String, Value> = sum
            .into_iter()
            .map(|(k, v)| (k.to_string(), json!(v / n)))
            .collect();
        match crate::tokens::dominant(&Value::Object(per)) {
            Some((label, v)) => format!(
                "<span title=\"over {}\">{label} {}</span>",
                count(n as usize, "measured completion"),
                crate::tokens::compact(v)
            ),
            None => "<span class=\"dz\">—</span>".to_string(),
        }
    }

    fn grid(&self, all: &[usize]) -> String {
        let m = self.m;
        let p = &m.p;
        let (pa, pb) = p.prior();
        let single = self.single().is_some();
        let cells = |ix: &[usize], ratio: Option<(f64, i64)>, projects: &[&str]| {
            let done = m.done_in(ix, p.first, p.last);
            let od = m.count(ix, p.last, Model::overdue_on);
            format!(
                "<td>{}</td><td>{done}{}</td><td>{}</td><td>{}</td><td{}>{od}</td>",
                m.open_at(ix, p.last),
                delta_chip(done - m.done_in(ix, pa, pb), Judge::UpGood),
                ratio_chip(ratio),
                self.tokens_per_done(projects),
                if od > 0 { " class=\"od\"" } else { "" },
            )
        };
        let mut rows = String::new();
        for (name, ix) in &m.projects {
            let n = esc(name);
            let ratio = self.calibration(name).filter(|c| c.1 > 0);
            if single {
                rows.push_str(&format!(
                    "<tr><th scope=\"row\">{n}</th>{}</tr>",
                    cells(ix, ratio, &[name])
                ));
            } else {
                rows.push_str(&format!(
                    "<tr data-p=\"{n}\"><th scope=\"row\"><button type=\"button\" class=\"plink\" data-p=\"{n}\" aria-pressed=\"false\">{n}</button></th>{}</tr>",
                    cells(ix, ratio, &[name])
                ));
            }
        }
        let foot = if single {
            String::new()
        } else {
            let rs = m.ratios(all);
            let names: Vec<&str> = m.projects.iter().map(|(p, _)| *p).collect();
            format!(
                "<tfoot><tr><td>All projects</td>{}</tr></tfoot>",
                cells(all, median(&rs).map(|v| (v, rs.len() as i64)), &names)
            )
        };
        let label = if p.days == 7 {
            "7 d".to_string()
        } else {
            format!("{} d", p.days)
        };
        format!(
            "<section id=\"projects\" aria-labelledby=\"h-pj\"><h2 id=\"h-pj\">Projects</h2>\
             <p class=\"sub\">Done and its Δ cover the period against {against}; tracked ÷ est. is the median over the tasks done in it, \
             and tokens are per measured completion, as their largest bucket.{hint}</p>\
             <div class=\"scroll\"><table><caption class=\"vh\">Per-project numbers for the period</caption>\
             <thead><tr><th scope=\"col\">Project</th><th scope=\"col\">Open</th><th scope=\"col\">Done {label} · Δ</th>\
             <th scope=\"col\">Tracked ÷ est.</th><th scope=\"col\">Tokens / done</th><th scope=\"col\">Overdue</th></tr></thead>\
             <tbody>{rows}</tbody>{foot}</table></div></section>",
            against = p.against(),
            hint = if single {
                ""
            } else {
                "<span class=\"needs-js\"> Choose a project to scope every section to it.</span>"
            },
        )
    }

    // ---- chart ------------------------------------------------------------

    fn chart(&self, s: &Scope) -> String {
        let bins = self.m.bins(s.ix);
        let narrow = &bins[bins.len().saturating_sub(NARROW_BINS)..];
        format!(
            "<div class=\"cw\">{}</div><div class=\"cn\">{}</div>",
            net_flow(&bins, 1080.0, self.m.store_start),
            net_flow(narrow, 360.0, self.m.store_start),
        )
    }

    // ---- outcomes ---------------------------------------------------------

    fn outcomes(&self, s: &Scope) -> String {
        let m = self.m;
        let names: Vec<&str> = if s.all() {
            m.projects.iter().map(|(p, _)| *p).collect()
        } else {
            vec![s.key]
        };
        let groups: Vec<&Value> = names.iter().filter_map(|p| self.group(p)).collect();
        let sum =
            |f: &dyn Fn(&Value) -> Option<i64>| -> i64 { groups.iter().filter_map(|g| f(g)).sum() };
        let at =
            |g: &Value, a: &str, b: &str| g.get(a).and_then(|x| x.get(b)).and_then(Value::as_i64);
        let reopened = sum(&|g| at(g, "rework", "count"));
        let cancelled =
            sum(&|g| Some(g.get("closed")?.as_i64()? - g.get("completions")?.as_i64()?));
        let unproven = sum(&|g| at(g, "unproven", "count"));
        let with_checks = sum(&|g| at(g, "unproven", "n"));
        let failed = sum(&|g| at(g, "unproven", "failed"));
        let mut refs: Vec<i64> = groups
            .iter()
            .filter_map(|g| g.get("unproven")?.get("refs")?.as_array())
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        refs.sort_unstable_by(|a, b| b.cmp(a));

        let rs = m.ratios(s.ix);
        let med = if s.all() {
            median(&rs).map(|v| (v, rs.len() as i64))
        } else {
            self.calibration(s.key).filter(|c| c.1 > 0)
        };
        const BUCKETS: [(&str, f64, f64, &str); 5] = [
            ("under 0.5×", 0.0, 0.5, ""),
            ("0.5–0.8×", 0.5, 0.8, ""),
            ("0.8–1.25× on target", 0.8, 1.25, "target"),
            ("1.25–2× over", 1.25, 2.0, "over"),
            ("over 2× way over", 2.0, f64::INFINITY, "way"),
        ];
        let counts: Vec<usize> = BUCKETS
            .iter()
            .map(|b| rs.iter().filter(|r| **r >= b.1 && **r < b.2).count())
            .collect();
        let max = counts.iter().copied().max().unwrap_or(0).max(1);
        let mut bars = String::new();
        for (b, n) in BUCKETS.iter().zip(&counts) {
            bars.push_str(&format!(
                "<span class=\"lab{}\">{}</span><span class=\"bar\"><i class=\"{}\" style=\"width:{:.1}%\"></i></span><span class=\"n\">{n}{}</span>",
                if b.3 == "target" { " target" } else { "" },
                b.0,
                b.3,
                *n as f64 / max as f64 * 100.0,
                if rs.is_empty() {
                    String::new()
                } else {
                    format!(" · {}%", (n * 100 + rs.len() / 2) / rs.len())
                }
            ));
        }
        let medline = match med {
            Some((v, n)) => format!(
                "Median {v:.2}× over {}; done work without an estimate or tracked time is left out.",
                count(n as usize, "task")
            ),
            None => "No task done in the period has both an estimate and tracked time.".to_string(),
        };
        let stat = |k: &str, label: &str, v: i64, small: String| {
            format!(
                "<div data-k=\"{k}\"><div class=\"mlabel\">{label}</div><div class=\"mval\">{v}{small}</div></div>"
            )
        };
        let unproven_small = if with_checks > 0 {
            format!(
                "<small>of {with_checks} with checks{}</small>",
                if failed > 0 {
                    format!(", {failed} failed")
                } else {
                    String::new()
                }
            )
        } else {
            String::new()
        };
        let by_short: HashMap<i64, &Task> =
            s.ix.iter()
                .map(|&i| (m.tasks[i].short, &m.tasks[i]))
                .collect();
        let shown: Vec<&Task> = refs
            .iter()
            .filter_map(|r| by_short.get(r).copied())
            .collect();
        let list = if shown.is_empty() {
            "<p class=\"empty\">Every completion in the period that had checks was proven.</p>"
                .to_string()
        } else {
            let mut l = String::from("<ul class=\"rows\">");
            for x in shown.iter().take(4) {
                l.push_str(&format!(
                    "<li class=\"st-over\">{}<span class=\"meta\">{}/{} checks passed</span></li>",
                    tlink(x),
                    x.checks.1,
                    x.checks.0
                ));
            }
            if shown.len() > 4 {
                l.push_str(&format!(
                    "<li class=\"more\">+{} more</li>",
                    shown.len() - 4
                ));
            }
            l + "</ul>"
        };
        format!(
            "<div class=\"out\"><div><h3>Estimate calibration · tracked ÷ estimate</h3><div class=\"bars\">{bars}</div>\
             <p class=\"storeline\">{medline}</p></div><div><div class=\"ostats\">{}{}{}</div>\
             <div class=\"olist\"><h3>Unproven completions</h3>{list}</div></div></div>",
            stat("reopened", "Reopened", reopened, String::new()),
            stat("cancelled", "Cancelled", cancelled, String::new()),
            stat("unproven", "Unproven", unproven, unproven_small),
        )
    }

    // ---- search -----------------------------------------------------------

    /// The compact index the search and the overlay read: one array per task,
    /// as text inside a hidden element (the guard's one-script rule, and no
    /// title is ever parsed as markup or code). Days count from the store's
    /// start and durations are minutes; `-1` is "none" for both.
    fn index(&self) -> String {
        let m = self.m;
        let projects: Vec<&str> = m.projects.iter().map(|(p, _)| *p).collect();
        let pidx: HashMap<&str, usize> =
            projects.iter().enumerate().map(|(i, p)| (*p, i)).collect();
        let d0 = m.store_start;
        let day = |t: Option<Timestamp>| match t {
            Some(t) => json!(days_between(d0, day_of(t))),
            None => json!(-1),
        };
        let shown = |i: usize| self.opts.all || !m.is(i, "cancelled");
        let today = day_of(m.now);
        let mut rows = Vec::new();
        let mut notes = serde_json::Map::new();
        for (i, t) in m.tasks.iter().enumerate() {
            if !shown(i) {
                continue;
            }
            let deps: Vec<i64> = t
                .deps
                .iter()
                .filter(|&&j| shown(j))
                .map(|&j| m.tasks[j].short)
                .collect();
            let blocks: Vec<i64> = t
                .blocks
                .iter()
                .filter(|&&j| shown(j))
                .map(|&j| m.tasks[j].short)
                .collect();
            let status = if t.blocked_now { "blocked" } else { t.status };
            let anns =
                t.v.get("annotations")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
            rows.push(json!([
                t.short,
                clean(t.title),
                pidx.get(t.project).copied().unwrap_or(0),
                status,
                t.priority,
                day(Some(t.created)),
                day(t.completed),
                day(t.due),
                t.est.map_or(-1, |e| e / 60),
                if t.tracked > 0 { t.tracked / 60 } else { -1 },
                day(t.started),
                day(t.reopened),
                deps,
                usize::from(t.blocked_outside),
                blocks,
                t.checks.0,
                t.checks.1,
                anns.len(),
                u8::from(m.overdue_on(i, today)),
            ]));
            if self.opts.with_notes && !anns.is_empty() {
                let mut newest: Vec<&Value> = anns.iter().collect();
                newest.sort_by_key(|a| std::cmp::Reverse(a.get("created").and_then(Value::as_str)));
                let kept: Vec<Value> = newest
                    .into_iter()
                    .take(ANNOTATIONS_PER_TASK)
                    .map(|a| {
                        let body = clean(a.get("body").and_then(Value::as_str).unwrap_or(""));
                        let cut: String = body.chars().take(ANNOTATION_CHARS).collect();
                        let more = body.chars().count().saturating_sub(ANNOTATION_CHARS);
                        json!([
                            a.get("created")
                                .and_then(Value::as_str)
                                .and_then(parse_ts)
                                .map(|c| long_date(day_of(c))),
                            if more > 0 {
                                format!("{cut}… ({more} more characters)")
                            } else {
                                cut
                            }
                        ])
                    })
                    .collect();
                notes.insert(t.short.to_string(), Value::Array(kept));
            }
        }
        let doc = json!({
            "p": projects.iter().map(|p| clean(p)).collect::<Vec<_>>(),
            "d0": [d0.year(), d0.month(), d0.day()],
            "notes": self.opts.with_notes,
            "t": rows,
            "n": notes,
        });
        // Text-node escaping only: a quote needs none there, and `&quot;` on
        // every JSON string cost a fifth of the index.
        doc.to_string()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn search(&self) -> String {
        format!(
            "<section id=\"search\" class=\"needs-js\" aria-labelledby=\"h-se\"><h2 id=\"h-se\">Find a task</h2>\
             <p class=\"sub\">Every task in scope{}, by title, project, status or #id.</p>\
             <div class=\"sbar\" role=\"search\"><label class=\"vh\" for=\"q\">Search tasks</label>\
             <input type=\"search\" id=\"q\" placeholder=\"Search title, project, status, #id\" autocomplete=\"off\">\
             <label class=\"vh\" for=\"sf\">Status</label><select id=\"sf\"><option value=\"\">any status</option>\
             <option>pending</option><option>active</option><option>blocked</option><option>backlog</option><option>done</option>{}</select></div>\
             <p class=\"empty\" id=\"hitcount\" aria-live=\"polite\"></p><ol class=\"hits\" id=\"hits\"></ol></section>\
             <div id=\"ix\" hidden>{}</div>",
            if self.opts.all { ", cancelled included" } else { "" },
            if self.opts.all { "<option>cancelled</option>" } else { "" },
            self.index(),
        )
    }

    fn footer(&self) -> String {
        let m = self.m;
        format!(
            "<footer>{} in this report{} · period {} · generated <span title=\"{}\">{}</span> by tasqx {} · \
             every number is a read of the tasqx core API; another period is one flag away \
             (<code>tasqx report --html --since … --until …</code>) · annotation bodies {}.</footer>",
            count(m.tasks.len(), "task"),
            self.filter
                .map(|f| format!(" (filter <code>{}</code>)", esc(f)))
                .unwrap_or_default(),
            esc(&range(m.p.first, m.p.last)),
            esc(&m.now.to_string()),
            esc(&stamp(m.now)),
            env!("CARGO_PKG_VERSION"),
            if self.opts.with_notes {
                "embedded (--with-notes)"
            } else {
                "not embedded"
            },
        )
    }
}

// ============================================================================
// Small renderers
// ============================================================================

#[derive(Clone, Copy)]
enum Judge {
    UpGood,
    UpBad,
    Neutral,
}

/// A Δ chip. Colour carries the judgement and the border style carries it
/// again, so the mono theme still tells good from bad (D212).
fn delta_chip(d: i64, judge: Judge) -> String {
    let class = match (d, judge) {
        (0, _) | (_, Judge::Neutral) => "flat",
        (d, Judge::UpGood) if d > 0 => "good",
        (_, Judge::UpGood) => "warn",
        (d, Judge::UpBad) if d > 0 => "bad",
        (_, Judge::UpBad) => "good",
    };
    format!("<span class=\"delta {class}\">{}</span>", signed(d))
}

fn ratio_chip(r: Option<(f64, i64)>) -> String {
    match r {
        None => "<span class=\"dz\">—</span>".to_string(),
        Some((v, n)) => {
            let class = if (0.8..=1.25).contains(&v) {
                "good"
            } else if v <= 2.0 {
                "warn"
            } else {
                "bad"
            };
            format!(
                "<span class=\"rt {class}\" title=\"median over {}\">{v:.2}×</span>",
                count(n as usize, "task with an estimate and tracked time")
            )
        }
    }
}

fn sparkline(vals: &[i64], hl: &[bool]) -> String {
    let (w, h) = (100.0, 28.0);
    let lo = vals.iter().copied().min().unwrap_or(0).min(0) as f64;
    let hi = vals.iter().copied().max().unwrap_or(1).max(1) as f64;
    let bw = w / vals.len().max(1) as f64;
    let y = |v: f64| h - 2.0 - (v - lo) / (hi - lo).max(1.0) * (h - 4.0);
    let mut s = String::new();
    for (k, on) in hl.iter().enumerate() {
        if *on {
            s.push_str(&format!(
                "<rect class=\"hl\" x=\"{:.0}\" y=\"0\" width=\"{bw:.0}\" height=\"{h}\"/>",
                k as f64 * bw
            ));
        }
    }
    if lo < 0.0 {
        s.push_str(&format!(
            "<line class=\"zero\" x1=\"0\" x2=\"{w}\" y1=\"{0:.0}\" y2=\"{0:.0}\" vector-effect=\"non-scaling-stroke\"/>",
            y(0.0)
        ));
    }
    let pts: Vec<String> = vals
        .iter()
        .enumerate()
        .map(|(k, v)| format!("{:.0},{:.0}", (k as f64 + 0.5) * bw, y(*v as f64)))
        .collect();
    s.push_str(&format!(
        "<polyline class=\"ln\" vector-effect=\"non-scaling-stroke\" points=\"{}\"/>",
        pts.join(" ")
    ));
    format!("<svg class=\"spark\" viewBox=\"0 0 {w} {h}\" preserveAspectRatio=\"none\" aria-hidden=\"true\">{s}</svg>")
}

fn nice_step(max: f64, n: f64) -> f64 {
    let raw = max.max(1.0) / n;
    let mag = 10f64.powf(raw.log10().floor());
    let f = raw / mag;
    let step = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    (step * mag).max(1.0)
}

/// Added above the zero line, done below it, the net change as a line over
/// both, and the open backlog in its own lane underneath. Drawn at `w` user
/// units wide so its text stays legible at the width it is shown at.
fn net_flow(bins: &[Bin], w: f64, start: Date) -> String {
    let n = bins.len().max(1);
    let narrow = w < 480.0;
    let (l, r, top, gap, bot) = (40.0, 10.0, 10.0, 30.0, 22.0);
    let h1 = if narrow { 150.0 } else { 180.0 };
    let h2 = if narrow { 80.0 } else { 100.0 };
    let h = top + h1 + gap + h2 + bot;
    let bw = (w - l - r) / n as f64;
    let x = |k: usize| l + k as f64 * bw;
    let mut up = 1i64;
    let mut dn = 1i64;
    for b in bins {
        up = up.max(b.added).max(b.net);
        dn = dn.max(b.done).max(-b.net);
    }
    let sc = h1 / (up + dn) as f64;
    let y0 = top + up as f64 * sc;
    let step = nice_step((up + dn) as f64, 5.0) as i64;
    let mut s = String::new();
    for (k, b) in bins.iter().enumerate() {
        if b.hl {
            s.push_str(&format!(
                "<rect class=\"hl\" x=\"{:.0}\" y=\"{top}\" width=\"{bw:.0}\" height=\"{}\"/>",
                x(k),
                h1 + gap + h2
            ));
        }
    }
    let tick = |s: &mut String, yy: f64, v: i64, class: &str| {
        s.push_str(&format!(
            "<line class=\"{class}\" x1=\"{l}\" x2=\"{:.0}\" y1=\"{yy:.0}\" y2=\"{yy:.0}\"/><text class=\"ax\" x=\"{:.0}\" y=\"{yy:.0}\" dy=\"0.35em\" text-anchor=\"end\">{v}</text>",
            w - r,
            l - 6.0
        ));
    };
    let mut v = step;
    while v <= up {
        tick(&mut s, y0 - v as f64 * sc, v, "grid");
        v += step;
    }
    v = step;
    while v <= dn {
        tick(&mut s, y0 + v as f64 * sc, v, "grid");
        v += step;
    }
    tick(&mut s, y0, 0, "zero");
    s.push_str(&format!(
        "<text class=\"lane\" x=\"{:.0}\" y=\"{:.0}\">added ↑</text><text class=\"lane\" x=\"{:.0}\" y=\"{:.0}\">done ↓</text>",
        l + 4.0,
        top + 10.0,
        l + 4.0,
        top + h1 - 4.0
    ));
    let pad = bw * 0.2;
    let bwi = bw - 2.0 * pad;
    let mut pts = Vec::new();
    for (k, b) in bins.iter().enumerate() {
        let xx = x(k) + pad;
        // Tooltips on the wide chart only: a phone has no hover to show them.
        let tip = |n: i64, what: &str| {
            if narrow {
                String::new()
            } else {
                format!("<title>From {}: {n} {what}</title>", short_date(b.start))
            }
        };
        s.push_str(&format!(
            "<rect class=\"add\" x=\"{xx:.0}\" y=\"{:.0}\" width=\"{bwi:.0}\" height=\"{:.0}\">{}</rect>\
             <rect class=\"done\" x=\"{xx:.0}\" y=\"{y0:.0}\" width=\"{bwi:.0}\" height=\"{:.0}\">{}</rect>",
            y0 - b.added as f64 * sc,
            b.added as f64 * sc,
            tip(b.added, "added"),
            b.done as f64 * sc,
            tip(b.done, "done"),
        ));
        pts.push((x(k) + bw / 2.0, y0 - b.net as f64 * sc, b));
    }
    let line: Vec<String> = pts
        .iter()
        .map(|p| format!("{:.0},{:.0}", p.0, p.1))
        .collect();
    s.push_str(&format!(
        "<polyline class=\"net\" points=\"{}\"/>",
        line.join(" ")
    ));
    for p in &pts {
        s.push_str(&format!(
            "<circle class=\"netd\" cx=\"{:.0}\" cy=\"{:.0}\" r=\"3\">{}</circle>",
            p.0,
            p.1,
            if narrow {
                String::new()
            } else {
                format!(
                    "<title>From {}: net {}</title>",
                    short_date(p.2.start),
                    signed(p.2.net)
                )
            }
        ));
    }
    let t2 = top + h1 + gap;
    let yb = t2 + h2;
    let bmax = bins.iter().map(|b| b.open).max().unwrap_or(0).max(1);
    let st2 = nice_step(bmax as f64, 3.0) as i64;
    let s2 = h2 / bmax as f64;
    let mut v = 0;
    while v <= bmax {
        tick(
            &mut s,
            yb - v as f64 * s2,
            v,
            if v == 0 { "zero" } else { "grid" },
        );
        v += st2;
    }
    let bp: Vec<(f64, f64)> = bins
        .iter()
        .enumerate()
        .map(|(k, b)| (x(k) + bw / 2.0, yb - b.open as f64 * s2))
        .collect();
    if let (Some(first), Some(last)) = (bp.first(), bp.last()) {
        let path: String = bp
            .iter()
            .map(|p| format!("L{:.0},{:.0}", p.0, p.1))
            .collect();
        s.push_str(&format!(
            "<path class=\"bla\" d=\"M{:.0},{yb:.0}{path}L{:.0},{yb:.0}Z\"/>",
            first.0, last.0
        ));
        let line: Vec<String> = bp
            .iter()
            .map(|p| format!("{:.0},{:.0}", p.0, p.1))
            .collect();
        s.push_str(&format!(
            "<polyline class=\"bl\" points=\"{}\"/>",
            line.join(" ")
        ));
    }
    let now_open = bins.last().map_or(0, |b| b.open);
    s.push_str(&format!(
        "<text class=\"lane\" x=\"{:.0}\" y=\"{:.0}\">open backlog · {now_open} now</text>",
        l + 4.0,
        t2 - 6.0
    ));
    let every = if bw < 46.0 { 2 } else { 1 };
    for (k, b) in bins.iter().enumerate() {
        if !(n - 1 - k).is_multiple_of(every) {
            continue;
        }
        s.push_str(&format!(
            "<text class=\"ax\" x=\"{:.0}\" y=\"{:.0}\" text-anchor=\"middle\">{}</text>",
            x(k) + bw / 2.0,
            h - 6.0,
            short_date(b.start)
        ));
    }
    let added: i64 = bins.iter().map(|b| b.added).sum();
    let done: i64 = bins.iter().map(|b| b.done).sum();
    format!(
        "<svg class=\"chart\" viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"Net flow per 7 days since {}: {added} added, {done} done; open backlog now {now_open}\">{s}</svg>",
        long_date(start)
    )
}

/// The standup's rows, `STANDUP_ROWS` of them, and how many more.
fn list(ix: &[usize], none: &str, row: impl Fn(usize) -> String) -> String {
    if ix.is_empty() {
        return format!("<p class=\"empty\">{none}</p>");
    }
    let mut s = String::from("<ul class=\"rows\">");
    for &i in ix.iter().take(STANDUP_ROWS) {
        s.push_str(&row(i));
    }
    if ix.len() > STANDUP_ROWS {
        s.push_str(&format!(
            "<li class=\"more\">+{} more — search below</li>",
            ix.len() - STANDUP_ROWS
        ));
    }
    s + "</ul>"
}

/// A task's id, which opens it in the overlay, and its title.
fn tlink(t: &Task) -> String {
    format!(
        "<button type=\"button\" class=\"tid\" data-id=\"{0}\">#{0}</button> {1}",
        t.short,
        esc(t.title)
    )
}

fn prio_rank(p: &str) -> u8 {
    match p {
        "H" => 0,
        "M" => 1,
        "L" => 2,
        _ => 3,
    }
}

/// Seconds from `b` to `a`, never negative.
fn secs(a: Timestamp, b: Timestamp) -> i64 {
    (a.as_second() - b.as_second()).max(0)
}

/// `5h`, `3d`, `2w`.
fn age_text(s: i64) -> String {
    let h = s as f64 / 3600.0;
    if h < 24.0 {
        format!("{}h", (h.round() as i64).max(1))
    } else if h / 24.0 < 14.0 {
        format!("{}d", (h / 24.0).round() as i64)
    } else {
        format!("{}w", (h / 168.0).round() as i64)
    }
}

/// `45m`, `1h`, `1.5h`, or `—`.
fn minutes(s: Option<i64>) -> String {
    match s {
        None => "—".to_string(),
        Some(s) if s < 3600 => format!("{}m", (s + 30) / 60),
        Some(s) if s % 3600 == 0 => format!("{}h", s / 3600),
        Some(s) => {
            let tenths = (s * 10 + 1800) / 3600;
            if tenths % 10 == 0 {
                format!("{}h", tenths / 10)
            } else {
                format!("{}.{}h", tenths / 10, tenths % 10)
            }
        }
    }
}

/// `n task` / `n tasks`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `+3`, `−2` (a real minus sign), `±0`.
fn signed(d: i64) -> String {
    match d {
        d if d > 0 => format!("+{d}"),
        d if d < 0 => format!("−{}", -d),
        _ => "±0".to_string(),
    }
}

fn short_date(d: Date) -> String {
    d.strftime("%-d %b").to_string()
}

fn long_date(d: Date) -> String {
    d.strftime("%-d %b %Y").to_string()
}

fn day_name(d: Date) -> String {
    d.strftime("%a %-d %b").to_string()
}

/// `24 Sep – 30 Sep 2026`, or both years when they differ.
fn range(a: Date, b: Date) -> String {
    if a == b {
        long_date(b)
    } else if a.year() == b.year() {
        format!("{} – {}", short_date(a), long_date(b))
    } else {
        format!("{} – {}", long_date(a), long_date(b))
    }
}

fn stamp(t: Timestamp) -> String {
    t.to_zoned(TimeZone::UTC)
        .strftime("%-d %b %Y %H:%M UTC")
        .to_string()
}

/// Control bytes out of text bound for the JSON index — `esc`'s rule, for a
/// string that is serialized rather than escaped.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

fn array_at<'a>(payload: &'a Value, key: &str) -> &'a [Value] {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

// ============================================================================
// Palette: the theme's roles onto the page's tokens
// ============================================================================

/// The light and dark token blocks. `accent`, `warn`, `danger` and `good`
/// (`timer.active`) come from the active theme, moved just far enough to clear
/// WCAG AA against the ground they sit on (#163); the neutrals are the page's
/// own on light and derived from the theme's `bg`/`fg`/`muted` on dark.
pub(crate) fn palette(theme: &Theme) -> String {
    let color = |name: &str, fallback: Rgb| theme.palette_color(name).unwrap_or(fallback);
    let accent = color("accent", Rgb::new(0x88, 0xc0, 0xd0));
    let warn = color("warn", Rgb::new(0xeb, 0xcb, 0x8b));
    let danger = color("danger", Rgb::new(0xbf, 0x61, 0x6a));
    let good = theme
        .role("timer.active")
        .fg
        .unwrap_or(Rgb::new(0xa3, 0xbe, 0x8c));
    let bg = color("bg", Rgb::new(0x2e, 0x34, 0x40));
    let fg = color("fg", Rgb::new(0xd8, 0xde, 0xe9));
    let muted = color("muted", Rgb::new(0x4c, 0x56, 0x6a));
    let white = Rgb::new(0xff, 0xff, 0xff);
    let light = format!(
        "--bg:#f7f8fa;--surface:#ffffff;--sunken:#eceff3;--fg:#1a1f29;--muted:#576071;--line:#dce1e8;--line-strong:#bcc4cf;\
         --accent:{};--warn:{};--danger:{};--good:{};--on-accent:#ffffff;--shadow:#1a1f2933;color-scheme:light;",
        darkened_for_contrast(accent, white, 4.5).hex(),
        darkened_for_contrast(warn, white, 4.5).hex(),
        darkened_for_contrast(danger, white, 4.5).hex(),
        darkened_for_contrast(good, white, 4.5).hex(),
    );
    let b = bg.hex();
    let dark = format!(
        "--bg:color-mix(in srgb,{b} 82%,#000000);--surface:{b};--sunken:color-mix(in srgb,{b} 90%,#ffffff);\
         --fg:{};--muted:{};--line:color-mix(in srgb,{b} 86%,#ffffff);--line-strong:color-mix(in srgb,{b} 70%,#ffffff);\
         --accent:{};--warn:{};--danger:{};--good:{};--on-accent:{b};--shadow:#00000080;color-scheme:dark;",
        adjusted_for_contrast(fg, bg, 7.0).hex(),
        adjusted_for_contrast(muted, bg, 4.5).hex(),
        adjusted_for_contrast(accent, bg, 4.5).hex(),
        adjusted_for_contrast(warn, bg, 4.5).hex(),
        adjusted_for_contrast(danger, bg, 4.5).hex(),
        adjusted_for_contrast(good, bg, 4.5).hex(),
    );
    format!(
        ":root{{{light}}}\n\
         @media screen and (prefers-color-scheme:dark){{:root:not([data-theme=\"light\"]){{{dark}}}}}\n\
         @media screen{{:root[data-theme=\"dark\"]{{{dark}}}}}\n"
    )
}

/// The page's static styles; the colour tokens come from [`palette`]. Soft
/// tints are mixed from the role colours here, so they follow the scheme.
const CSS: &str = r#":root{--accent-soft:color-mix(in srgb,var(--accent) 16%,var(--surface));--good-soft:color-mix(in srgb,var(--good) 14%,var(--surface));
--warn-soft:color-mix(in srgb,var(--warn) 14%,var(--surface));--danger-soft:color-mix(in srgb,var(--danger) 14%,var(--surface));
--c-add:color-mix(in srgb,var(--muted) 70%,var(--surface));--c-done:var(--accent);--c-net:var(--fg);--c-backlog:var(--muted);--c-backlog-fill:var(--sunken)}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;-webkit-text-size-adjust:100%}
.mono,.tid,code{font-family:ui-monospace,"SF Mono","Cascadia Code",Consolas,monospace}
.mval,.tid,.delta,td,.count{font-variant-numeric:tabular-nums}
code{font-size:.92em}
.wrap{max-width:1120px;margin-inline:auto;padding-inline:clamp(16px,3vw,32px);padding-bottom:40px}
.vh{position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);white-space:nowrap}
:root:not(.js) .needs-js{display:none}
[hidden]{display:none!important}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px;border-radius:2px}
button{font:inherit;color:inherit}
h1{font-size:18px;margin:0;font-weight:700;letter-spacing:-.01em}
h1 span{font-weight:400;color:var(--muted)}
h2{font-size:15px;margin:0;font-weight:650}
h3{font-size:12px;margin:0 0 8px;font-weight:600;text-transform:uppercase;letter-spacing:.06em;color:var(--muted)}
.sub{margin:2px 0 12px;color:var(--muted);font-size:13px}
section{border-top:1px solid var(--line);padding-top:16px;margin-top:28px}
.top{display:flex;flex-wrap:wrap;gap:12px 24px;align-items:center;justify-content:space-between;padding-block:18px 12px}
.win{margin:2px 0 0;color:var(--muted);font-size:13px}
.controls{display:flex;flex-wrap:wrap;gap:8px 16px;align-items:center}
.btn{background:var(--surface);border:1px solid var(--line-strong);border-radius:6px;padding:4px 10px;font-size:13px;cursor:pointer}
.btn:hover{border-color:var(--accent)}
.tog{display:inline-flex;gap:6px;align-items:center;font-size:13px;color:var(--muted)}
.tog input{accent-color:var(--accent);margin:0}
.scopebar{display:flex;flex-wrap:wrap;gap:8px;align-items:center;font-size:13px;color:var(--muted);padding-bottom:4px}
.chip{display:inline-flex;align-items:center;gap:4px;border:1px solid var(--line-strong);border-radius:999px;padding:1px 10px;font-size:13px;background:var(--surface);color:var(--fg);cursor:pointer;line-height:1.6}
.chip.on{background:var(--accent);border-color:var(--accent);color:var(--on-accent)}
.chip.scoped{background:var(--accent-soft);border-color:var(--accent);cursor:default}
.chip.scoped button{background:none;border:0;padding:0 0 0 2px;cursor:pointer;font-size:15px;line-height:1}
.lede{font-size:clamp(17px,2.2vw,21px);line-height:1.4;margin:14px 0 18px;max-width:62ch;font-weight:500}
.band{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:1px;border-block:1px solid var(--line);overflow:hidden}
.metric{padding:12px 14px 10px;box-shadow:1px 0 0 var(--line),0 1px 0 var(--line)}
.mlabel{font-size:12px;color:var(--muted);text-transform:uppercase;letter-spacing:.06em;font-weight:600}
.mval{font-size:28px;font-weight:650;line-height:1.15;margin-top:2px}
.mval small{font-size:13px;font-weight:500;color:var(--muted);margin-left:6px}
.mdelta{font-size:12px;color:var(--muted);margin-top:2px;min-height:20px}
.delta,.rt{display:inline-block;padding:0 6px;border-radius:3px;font-weight:600;font-size:12px;line-height:18px;border:1px solid transparent}
.good{color:var(--good);background:var(--good-soft)}
.warn{color:var(--warn);background:var(--warn-soft);border-style:dashed!important;border-color:currentColor!important}
.bad{color:var(--danger);background:var(--danger-soft);border-color:currentColor!important;font-weight:800}
.flat{color:var(--muted);background:var(--sunken)}
.spark{display:block;width:100%;height:28px;margin-top:6px;overflow:visible}
.spark .ln{fill:none;stroke:var(--muted);stroke-width:1.5}
.spark .zero{stroke:var(--line-strong);stroke-width:1}
.hl{fill:var(--accent-soft)}
.storeline{font-size:12px;color:var(--muted);margin:8px 0 0}
.su{display:grid;grid-template-columns:repeat(auto-fit,minmax(250px,1fr));gap:20px 28px}
.rows{list-style:none;margin:0;padding:0}
.rows li{border-left:3px solid var(--line-strong);padding:2px 0 2px 9px;margin-bottom:8px;font-size:14px;line-height:1.35;overflow-wrap:anywhere}
.rows li.st-done{border-color:var(--good)}
.rows li.st-active{border-color:var(--accent)}
.rows li.st-blocked{border-color:var(--danger);border-left-style:double;border-left-width:5px}
.rows li.st-over{border-color:var(--warn);border-left-style:dashed}
.rows .meta{display:block;font-size:12px;color:var(--muted);margin-top:1px}
.rows .meta b{font-weight:600;color:var(--fg)}
.rows .more{border:0;padding-left:12px;font-size:12px;color:var(--muted)}
.tid{background:none;border:0;padding:0;color:var(--accent);font-size:13px;font-weight:600;cursor:pointer;text-decoration:underline;text-decoration-color:transparent;text-underline-offset:2px}
.tid:hover{text-decoration-color:currentColor}
.count{font-weight:600;color:var(--fg);margin-left:4px}
.empty{font-size:13px;color:var(--muted);margin:0}
.redact{color:var(--muted);font-style:italic}
.scroll{overflow-x:auto;-webkit-overflow-scrolling:touch}
table{border-collapse:collapse;width:100%;min-width:560px;font-size:14px}
th,td{padding:6px 10px;text-align:right;border-bottom:1px solid var(--line);white-space:nowrap}
th:first-child,td:first-child{text-align:left;padding-left:8px}
thead th{font-size:12px;font-weight:600;color:var(--muted);border-bottom-color:var(--line-strong);vertical-align:bottom}
tbody tr[data-p]{cursor:pointer}
tbody tr:hover{background:var(--sunken)}
tbody tr.sel{background:var(--accent-soft);box-shadow:inset 3px 0 0 var(--accent)}
tfoot td{font-weight:600;border-bottom:0;border-top:1px solid var(--line-strong)}
.plink{background:none;border:0;padding:0;font-weight:600;cursor:pointer;color:var(--fg)}
.dz{color:var(--muted)}
.rt{min-width:3.6em;text-align:center;font-size:13px}
td .delta{margin-left:6px;font-size:11px;line-height:16px}
.od{color:var(--danger);font-weight:700}
.out{display:grid;grid-template-columns:minmax(0,1.6fr) minmax(0,1fr);gap:24px 32px}
.bars{display:grid;grid-template-columns:auto minmax(60px,1fr) auto;gap:6px 10px;align-items:center;font-size:13px}
.bars .lab{color:var(--muted);white-space:nowrap}
.bars .lab.target{color:var(--fg);font-weight:600}
.bar{height:12px;background:var(--sunken);border-radius:2px;overflow:hidden}
.bar i{display:block;height:100%;background:var(--c-add)}
.bar i.target{background:var(--good)}
.bar i.over{background:repeating-linear-gradient(135deg,var(--warn) 0 3px,transparent 3px 6px)}
.bar i.way{background:var(--danger)}
.bars .n{text-align:right;min-width:5.5em}
.ostats{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:1px;background:var(--line);border-block:1px solid var(--line)}
.ostats>div{background:var(--bg);padding:8px 10px}
.ostats .mval{font-size:22px}
.olist{margin-top:12px}
.legend{display:flex;flex-wrap:wrap;gap:4px 16px;font-size:12px;color:var(--muted);margin:0 0 6px;padding:0;list-style:none}
.legend i{display:inline-block;width:10px;height:10px;margin-right:5px;vertical-align:-1px;border-radius:2px}
.legend .k-add{background:var(--c-add)}.legend .k-done{background:var(--c-done)}
.legend .k-net{background:var(--c-net);height:2px;width:14px;vertical-align:3px}
.legend .k-bl{background:var(--c-backlog);height:2px;width:14px;vertical-align:3px}
.legend .k-hl{background:var(--accent-soft);border:1px solid var(--line-strong)}
.chart{display:block;width:100%;height:auto;max-width:100%}
.cn{display:none}
.chart .ax{font-size:11px;fill:var(--muted)}
.chart .lane{font-size:11px;fill:var(--fg);font-weight:600;stroke:var(--bg);stroke-width:3px;paint-order:stroke;stroke-linejoin:round}
.chart .grid{stroke:var(--line);stroke-width:1}
.chart .zero{stroke:var(--line-strong);stroke-width:1}
.chart .add{fill:var(--c-add)}.chart .done{fill:var(--c-done)}
.chart .net{fill:none;stroke:var(--c-net);stroke-width:1.75}
.chart .netd{fill:var(--bg);stroke:var(--c-net);stroke-width:1.5}
.chart .bl{fill:none;stroke:var(--c-backlog);stroke-width:2}
.chart .bla{fill:var(--c-backlog-fill)}
.sbar{display:flex;flex-wrap:wrap;gap:8px}
.sbar input,.sbar select{font:inherit;font-size:14px;color:var(--fg);background:var(--surface);border:1px solid var(--line-strong);border-radius:6px;padding:6px 10px}
.sbar input{flex:1 1 220px;min-width:0}
.hits{list-style:none;margin:8px 0 0;padding:0;border-top:1px solid var(--line)}
.hits li{border-bottom:1px solid var(--line)}
.hit{display:flex;gap:10px;align-items:baseline;width:100%;text-align:left;background:none;border:0;padding:7px 4px;cursor:pointer;font-size:14px}
.hit:hover{background:var(--sunken)}
.hit .tid{flex:none;min-width:3.6em}
.hit .ttl{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.hit .pj{flex:none;font-size:12px;color:var(--muted)}
.st{flex:none;display:inline-block;font-size:11px;font-weight:600;border-radius:3px;padding:0 6px;line-height:18px}
.st.done{color:var(--good);background:var(--good-soft)}
.st.active{color:var(--accent);background:var(--accent-soft)}
.st.blocked{color:var(--danger);background:var(--danger-soft);border:1px solid currentColor}
.st.pending,.st.backlog{color:var(--muted);background:var(--sunken)}
.st.cancelled{color:var(--muted);border:1px solid var(--line-strong);text-decoration:line-through}
dialog{margin:0;position:fixed;width:min(560px,calc(100vw - 32px));max-height:min(70vh,560px);overflow:auto;background:var(--surface);color:var(--fg);border:1px solid var(--line-strong);border-radius:8px;padding:14px 16px 16px;box-shadow:0 10px 30px var(--shadow)}
dialog::backdrop{background:transparent}
.dhead{display:flex;gap:10px;align-items:flex-start}
.dhead h2{flex:1;font-size:16px;line-height:1.35;overflow-wrap:anywhere}
.x{background:none;border:1px solid var(--line-strong);border-radius:6px;cursor:pointer;padding:0 8px;font-size:16px;line-height:24px}
dl.facts{display:grid;grid-template-columns:auto 1fr;gap:3px 14px;margin:12px 0;font-size:13px}
dl.facts dt{color:var(--muted)}dl.facts dd{margin:0}
.dsec{margin-top:12px;font-size:13px}
.dsec ul{list-style:none;margin:4px 0 0;padding:0}
.dsec li{padding:3px 0;border-bottom:1px solid var(--line);display:flex;gap:8px;align-items:baseline}
.dsec li span.t{flex:1;min-width:0;overflow-wrap:anywhere}
.note{white-space:pre-wrap}
footer{margin-top:36px;border-top:1px solid var(--line);padding-top:12px;font-size:12px;color:var(--muted)}
@media (max-width:720px){.out{grid-template-columns:1fr}}
@media (max-width:600px){.cw{display:none}.cn{display:block}}
@media (prefers-reduced-motion:no-preference){.chip,.btn,.hit,tbody tr{transition:background-color .12s,border-color .12s}}
@media print{
body{font-size:11px;-webkit-print-color-adjust:exact;print-color-adjust:exact}
.wrap{max-width:none;padding-inline:0}
.controls,#search,dialog,.scopebar,.more,.spark,.cn,.olist,.storeline{display:none!important}
.su{grid-template-columns:repeat(4,minmax(0,1fr));gap:8px 14px}.rows .meta{font-size:9px}
.cw{display:block!important}
.scroll{overflow:visible}
table{min-width:0;font-size:10px}th,td{padding:2px 6px}
section{margin-top:12px;padding-top:6px;break-inside:avoid}
.mval{font-size:18px}.lede{font-size:14px;margin:8px 0}
.rows li{font-size:10px;margin-bottom:3px}
.chart{max-height:200px;width:auto}
tbody tr.sel{box-shadow:none}
@page{margin:12mm}
}
"#;

/// The page's one inline script (D48b): flips which scope's server-rendered
/// blocks show, searches the index, and opens a task in an overlay. Nothing
/// here aggregates, fetches, or touches the History API. Readable on purpose:
/// the generated file is a document a reader may need to trust.
const SCRIPT: &str = r##"(function () {
"use strict";
var root = document.documentElement, $ = function (id) { return document.getElementById(id); };
root.classList.add("js");
var IX = JSON.parse($("ix").textContent), P = IX.p, T = IX.t, N = IX.n, BY = {};
T.forEach(function (t) { BY[t[0]] = t; });
var D0 = Date.UTC(IX.d0[0], IX.d0[1] - 1, IX.d0[2]);
var MON = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
var scope = "*", notes = false, cur = 0;
function esc(s) { return String(s).replace(/[&<>"]/g, function (c) { return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]; }); }
function day(n) { if (n < 0) return "—"; var d = new Date(D0 + n * 864e5); return d.getUTCDate() + " " + MON[d.getUTCMonth()] + " " + d.getUTCFullYear(); }
function mins(m) { return m < 0 ? "—" : m < 60 ? m + "m" : +(m / 60).toFixed(1) + "h"; }
/* Scope: every block is rendered per project already; this only picks one. */
function setScope(s) {
  scope = s;
  document.querySelectorAll("[data-sc]").forEach(function (el) { el.hidden = el.getAttribute("data-sc") !== s; });
  document.querySelectorAll("#projects tr[data-p]").forEach(function (tr) {
    var on = tr.getAttribute("data-p") === s;
    tr.classList.toggle("sel", on);
    tr.querySelector(".plink").setAttribute("aria-pressed", on);
  });
  var all = $("allp"), chip = $("scoped");
  if (all) { all.classList.toggle("on", s === "*"); all.setAttribute("aria-pressed", s === "*"); chip.hidden = s === "*"; $("scoped-name").textContent = s; }
  search();
}
function search() {
  var q = $("q").value.trim().toLowerCase(), f = $("sf").value, terms = q ? q.split(/\s+/) : [];
  var pool = T.filter(function (t) { return scope === "*" || P[t[2]] === scope; });
  if (!terms.length && !f) { $("hitcount").textContent = pool.length + " tasks in scope. Type to search."; $("hits").innerHTML = ""; return; }
  var hits = pool.filter(function (t) {
    if (f && t[3] !== f) return false;
    var s = ("#" + t[0] + " " + t[1] + " " + P[t[2]] + " " + t[3]).toLowerCase();
    return terms.every(function (m) { return /^#?\d+$/.test(m) ? String(t[0]) === m.replace("#", "") : s.indexOf(m) >= 0; });
  }).sort(function (a, b) { return b[0] - a[0]; });
  $("hitcount").textContent = hits.length + (hits.length === 1 ? " match" : " matches") + (hits.length > 40 ? ", showing the newest 40" : "") + ".";
  $("hits").innerHTML = hits.slice(0, 40).map(function (t) {
    return '<li><button type="button" class="hit" data-id="' + t[0] + '"><span class="tid">#' + t[0] + '</span><span class="ttl">' + esc(t[1]) + "</span>" +
      (scope === "*" ? '<span class="pj">' + esc(P[t[2]]) + "</span>" : "") + '<span class="st ' + t[3] + '">' + t[3] + "</span></button></li>";
  }).join("");
}
/* The overlay: one task's facts, its dependencies by title, its notes if embedded. */
var dlg = $("dd"), opener = null;
function dep(id) { var b = BY[id]; return '<li><button type="button" class="tid" data-id="' + id + '">#' + id + '</button><span class="t">' + esc(b ? b[1] : "") + "</span>" + (b ? '<span class="st ' + b[3] + '">' + b[3] + "</span>" : "") + "</li>"; }
function render(id) {
  var t = BY[id], ns = N[id] || [];
  cur = id;
  var rows = [["Project", esc(P[t[2]])], ["Priority", t[4] || "none"], ["Created", day(t[5])], ["Started", day(t[10])], ["Closed", day(t[6])],
    ["Due", day(t[7]) + (t[18] ? ' · <b class="od">overdue</b>' : "")], ["Estimate / tracked", mins(t[8]) + " / " + mins(t[9])],
    ["Checks", t[15] ? t[16] + " of " + t[15] + " passed" : "none"], ["Reopened", t[11] < 0 ? "no" : day(t[11])]];
  var out = t[13] ? '<li><span class="t redact">a task outside this report</span></li>' : "";
  var s = '<div class="dhead"><h2 id="dd-title"><span class="mono dz">#' + id + "</span> " + esc(t[1]) + '</h2><span class="st ' + t[3] + '">' + t[3] +
    '</span><button type="button" class="x" id="dd-x" aria-label="Close">×</button></div><dl class="facts">' +
    rows.map(function (r) { return "<dt>" + r[0] + "</dt><dd>" + r[1] + "</dd>"; }).join("") + "</dl>";
  s += '<div class="dsec"><h3>Blocked by</h3>' + (t[12].length || out ? "<ul>" + t[12].map(dep).join("") + out + "</ul>" : '<p class="empty">No dependencies.</p>') + "</div>";
  s += '<div class="dsec"><h3>Blocks</h3>' + (t[14].length ? "<ul>" + t[14].map(dep).join("") + "</ul>" : '<p class="empty">Nothing waits on it.</p>') + "</div>";
  s += '<div class="dsec"><h3>Annotations ' + t[17] + "</h3>" + (!t[17] ? '<p class="empty">None.</p>' :
    notes ? "<ul>" + ns.map(function (n) { return '<li><span class="dz">' + n[0] + '</span><span class="t note">' + esc(n[1]) + "</span></li>"; }).join("") +
      (t[17] > ns.length ? '<li class="dz">' + (t[17] - ns.length) + " older: tasqx show " + id + "</li>" : "") + "</ul>" :
    '<p class="empty">' + (IX.notes ? "Hidden. Turn on “Show annotation bodies” to read them." : "Not in this file. Run tasqx show " + id + ", or regenerate with --with-notes.") + "</p>") + "</div>";
  $("dd-body").innerHTML = s;
}
function place(el) {
  var r = el.getBoundingClientRect(), vw = root.clientWidth, vh = window.innerHeight, dw = dlg.offsetWidth, dh = dlg.offsetHeight;
  var top = r.bottom + 6;
  if (top + dh > vh - 8) top = Math.max(8, r.top - dh - 6);
  if (top + dh > vh - 8) top = Math.max(8, vh - dh - 8);
  dlg.style.top = top + "px"; dlg.style.left = Math.max(16, Math.min(r.left, vw - dw - 16)) + "px";
}
function open(id, el) { if (!BY[id]) return; if (!dlg.open) opener = el; render(id); if (!dlg.open) dlg.showModal(); place(opener || el); $("dd-x").focus(); }
dlg.addEventListener("close", function () { if (opener && document.contains(opener)) opener.focus(); opener = null; });
dlg.addEventListener("click", function (e) { if (e.target === dlg) dlg.close(); });
document.addEventListener("click", function (e) {
  var el = e.target.closest("button,tr");
  if (!el) return;
  if (el.id === "dd-x") { dlg.close(); return; }
  if (el.hasAttribute("data-id")) { open(+el.getAttribute("data-id"), el); return; }
  if (el.hasAttribute("data-reset")) { setScope("*"); return; }
  var p = el.getAttribute("data-p");
  if (p !== null && el.closest("#projects")) {
    setScope(p === scope ? "*" : p);
    var b = document.querySelector('#projects .plink[data-p="' + CSS.escape(p) + '"]');
    if (b) b.focus();
  }
});
var nt = $("notes");
if (nt) nt.addEventListener("change", function () { notes = nt.checked; if (dlg.open) render(cur); });
$("q").addEventListener("input", search);
$("sf").addEventListener("change", search);
/* Light by default, dark by the OS; the button overrides either, kept per browser. */
var THEMES = ["auto", "light", "dark"], th = 0, KEY = "tasqx-report-theme";
function theme() { if (th) root.setAttribute("data-theme", THEMES[th]); else root.removeAttribute("data-theme"); $("theme").textContent = "Theme: " + THEMES[th]; }
try { th = Math.max(0, THEMES.indexOf(localStorage.getItem(KEY))); } catch (e) {}
theme();
$("theme").addEventListener("click", function () { th = (th + 1) % 3; try { localStorage.setItem(KEY, THEMES[th]); } catch (e) {} theme(); });
setScope("*");
})();"##;

// ============================================================================
// Escaping, contrast and durations
// ============================================================================

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
pub(crate) fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
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
    let mix = |t: f64| -> Rgb {
        let ch = |v: u8| -> u8 { (f64::from(v) * (1.0 - t)).round() as u8 };
        Rgb::new(ch(c.r), ch(c.g), ch(c.b))
    };
    least_mix_clearing(c, bg, min_contrast, mix)
}

/// The mirror of `darkened_for_contrast` for a dark ground: lighten toward
/// white just enough to clear `min_contrast`.
fn lightened_for_contrast(c: Rgb, bg: Rgb, min_contrast: f64) -> Rgb {
    let mix = |t: f64| -> Rgb {
        let ch = |v: u8| -> u8 { (f64::from(v) + (255.0 - f64::from(v)) * t).round() as u8 };
        Rgb::new(ch(c.r), ch(c.g), ch(c.b))
    };
    least_mix_clearing(c, bg, min_contrast, mix)
}

/// Move `c` toward whichever pole `bg` is not — a dark ground wants a
/// lighter colour, a light ground a darker one — so one call covers a theme
/// whose "dark" scheme is in fact light.
fn adjusted_for_contrast(c: Rgb, bg: Rgb, min_contrast: f64) -> Rgb {
    if relative_luminance(bg) < 0.18 {
        lightened_for_contrast(c, bg, min_contrast)
    } else {
        darkened_for_contrast(c, bg, min_contrast)
    }
}

/// The smallest `t` in `[0, 1]` for which `mix(t)` clears `min_contrast`
/// against `bg`, given that contrast rises monotonically with `t`; `c`
/// itself when it already clears.
fn least_mix_clearing(c: Rgb, bg: Rgb, min_contrast: f64, mix: impl Fn(f64) -> Rgb) -> Rgb {
    if contrast_ratio(c, bg) >= min_contrast {
        return c;
    }
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
mod tests;
