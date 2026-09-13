//! The dashboard's data layer and its geometry — both pure, neither aware that
//! a terminal exists (D58).
//!
//! Two halves, and the split is the point. [`build`] turns the four JSON
//! results into typed submodels; [`layout`] turns two integers into a set of
//! absolute rectangles. Neither takes a `Frame`, neither reads the clock, and
//! neither knows a colour. Everything that draws lives in the sibling module and
//! decides nothing — the same shape `pick` and `settings` have, for the reason
//! `tui.rs` states: it is what makes a screen testable in a repo that fails the
//! build on a warning.
//!
//! **`layout` deliberately does not use `ratatui::layout::Layout`.** That is not
//! purity for its own sake. `Constraint::Length` is a *soft* cassowary
//! constraint: asked for eight panels in a twenty-row column, ratatui keeps all
//! eight and silently shrinks them, and at fourteen rows every panel becomes a
//! two-row box with nothing inside it. `Constraint::Min(n)` does not vanish
//! below its floor either — it wins and starves its neighbour. D58 requires the
//! opposite rule, *a panel that does not fit is omitted, never drawn clipped*,
//! and that is only reachable by doing the arithmetic here, in integers a test
//! can read.

use std::collections::{HashMap, HashSet};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use serde_json::Value;

use crate::chart;
use crate::render;
use crate::tokens::BUCKETS;

// ============================================================================
// Row-level types
// ============================================================================

/// A task's lifecycle status.
///
/// `Other` is not defensive padding: `engine.rs` passes an unrecognised stored
/// status through verbatim beside a `status_unrecognized` flag rather than
/// refusing to list the row, so a closed enum here would panic or silently drop
/// a task the store is showing everyone else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Backlog,
    Pending,
    Active,
    Done,
    Cancelled,
    Other(String),
}

impl Status {
    fn parse(s: &str) -> Self {
        match s {
            "backlog" => Status::Backlog,
            "pending" => Status::Pending,
            "active" => Status::Active,
            "done" => Status::Done,
            "cancelled" => Status::Cancelled,
            other => Status::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Status::Backlog => "backlog",
            Status::Pending => "pending",
            Status::Active => "active",
            Status::Done => "done",
            Status::Cancelled => "cancelled",
            Status::Other(s) => s,
        }
    }

    /// Whether this status counts as still-open work.
    ///
    /// Delegates to `render::status_is_open` rather than re-deriving the rule:
    /// one answer, and an unknown status counts as open there too.
    pub fn is_open(&self) -> bool {
        render::status_is_open(self.as_str())
    }
}

/// Priority, as the store spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prio {
    H,
    M,
    L,
}

impl Prio {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "H" => Some(Prio::H),
            "M" => Some(Prio::M),
            "L" => Some(Prio::L),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Prio::H => "H",
            Prio::M => "M",
            Prio::L => "L",
        }
    }
}

/// One task, projected from the shared snapshot.
///
/// There is exactly one row type; every list panel holds `Vec<Task>` and differs
/// only in how it selects and orders. The display strings are private with no
/// public constructor, which is `pick::Row`'s rule and its reason: a public
/// struct literal would let a caller build a row whose title still carries the
/// control bytes an imported or agent-authored task can contain. Eight panels
/// multiply that hole by eight.
#[derive(Clone, Debug)]
pub struct Task {
    pub short_id: i64,
    /// The UUID — the only key that joins to `event.entity_id`.
    pub id: String,
    title: String,
    project: Option<String>,
    pub priority: Option<Prio>,
    pub urgency: f64,
    pub status: Status,
    pub blocked: bool,
    pub due: Option<Timestamp>,
    pub completed: Option<Timestamp>,
    pub modified: Timestamp,
    /// When the task was created — where the burndown gets existence from, so a
    /// window that clipped the `add` event off costs nothing.
    pub created: Timestamp,
    /// `Some` exactly when the timer is running.
    pub active_since: Option<Timestamp>,
    pub estimate_secs: Option<i64>,
    pub tracked_secs: i64,
    /// Tracked time PLUS the interval still running, on the one task whose
    /// timer is going; `None` on every other row.
    ///
    /// The NOW card carried this and NOW is gone (D80). The number is not: a
    /// row marked `▶` with no elapsed beside it answers "which one" and drops
    /// "for how long", which is the half a reader glancing at the screen is
    /// usually after. Computed in the builder, where `now` is — a renderer that
    /// read the clock would disagree with the row above it across a redraw.
    pub running_secs: Option<i64>,
    /// Sanitised at construction like every other display string, and spelled
    /// `+tag` at draw time — the one spelling `list`'s filter grammar accepts
    /// (#228.16), so a tag read off this screen can be pasted into a query.
    tags: Vec<String>,
}

impl Task {
    /// Sanitised title. The only way to read it.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The tags, already sanitised. The only way to read them.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// Sanitised project name, `None` for the project-less bucket.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// The calendar date this task is due, in UTC.
    ///
    /// Bucketing is by DATE, never by instant. A date-only `due` normalises to
    /// midnight UTC, so an instant comparison calls a task due *today* overdue
    /// from one second past midnight — which is exactly what `report.summary`'s
    /// `overdue` metric does, and the reason this dashboard derives its own.
    pub fn due_date(&self) -> Option<Date> {
        self.due.map(|t| t.to_zoned(TimeZone::UTC).date())
    }

    fn from_json(v: &Value) -> Option<Self> {
        let id = v.get("id")?.as_str()?.to_string();
        let modified = v.get("modified").and_then(Value::as_str)?.parse().ok()?;
        Some(Task {
            short_id: v.get("short_id").and_then(Value::as_i64).unwrap_or(0),
            id,
            // Every display string goes through the shared sanitiser at
            // construction (D19), not at draw time — a ratatui cell is written
            // to the terminal verbatim.
            title: render::san(v.get("title").and_then(Value::as_str).unwrap_or("")),
            project: v.get("project").and_then(Value::as_str).map(render::san),
            priority: v
                .get("priority")
                .and_then(Value::as_str)
                .and_then(Prio::parse),
            urgency: v.get("urgency").and_then(Value::as_f64).unwrap_or(0.0),
            status: Status::parse(v.get("status").and_then(Value::as_str).unwrap_or("")),
            running_secs: None,
            tags: v
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(render::san)
                        .collect()
                })
                .unwrap_or_default(),
            blocked: v.get("blocked").and_then(Value::as_bool).unwrap_or(false),
            due: v
                .get("due")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok()),
            completed: v
                .get("completed")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok()),
            modified,
            created: v
                .get("created")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
                .unwrap_or(modified),
            active_since: v
                .get("active_since")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok()),
            // The one checked duration reader (D17/D14). A hand-rolled parse
            // here would be the fourth copy, and the third one was a bug.
            estimate_secs: v
                .get("estimate")
                .and_then(Value::as_str)
                .and_then(tasqx_core::util::duration_secs),
            tracked_secs: v
                .get("tracked")
                .and_then(Value::as_str)
                .and_then(tasqx_core::util::duration_secs)
                .unwrap_or(0),
        })
    }
}

// ============================================================================
// Panel submodels
// ============================================================================

/// The header line: the numbers that say whether anything needs attention now.
#[derive(Clone, Debug, Default)]
pub struct StatusBar {
    project: Option<String>,
    pub open: usize,
    pub active: usize,
    pub overdue: usize,
    pub blocked: usize,
    pub done_week: usize,
}

impl StatusBar {
    /// The default project's name, sanitised.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }
}

/// The one task list, grouped by project.
///
/// D80's ruling, and the reason it is one: NOW, NEXT UP, DUE, BLOCKED and
/// RECENT were five panels over the same rows, so the same task could be drawn
/// three times on one screen while the panel that answers "what now" truncated
/// at fifteen of twenty-four. They fold in as a marker, a column and a sort.
#[derive(Clone, Debug, Default)]
pub struct Tasks {
    pub groups: Vec<TaskGroup>,
    /// Every row across every group, so a panel can size itself without
    /// walking them.
    pub total: usize,
    pub sort: Sort,
}

/// One project's tasks, and the two counts its heading carries.
#[derive(Clone, Debug)]
pub struct TaskGroup {
    project: Option<String>,
    pub overdue: usize,
    pub rows: Vec<Task>,
}

impl TaskGroup {
    /// Sanitised project name, `None` for the project-less bucket.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }
}

/// What the list is ordered by. `RECENT` was a panel; it is this.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sort {
    /// Hottest first — the question the screen exists to answer.
    #[default]
    Urgency,
    /// Soonest deadline first, undated last.
    Due,
    /// Most recently touched first. What RECENT drew, without a panel.
    Touched,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Urgency => "urgency",
            Sort::Due => "due",
            Sort::Touched => "touched",
        }
    }

    /// The next one `s` cycles to.
    pub fn next(self) -> Sort {
        match self {
            Sort::Urgency => Sort::Due,
            Sort::Due => Sort::Touched,
            Sort::Touched => Sort::Urgency,
        }
    }
}

/// Group `rows` by project and order both the groups and the rows in them.
///
/// Groups are ordered by their hottest task, not alphabetically: the list is
/// read from the top, and a project whose most urgent row is a 0.1 does not
/// belong above one holding an 18.5 because its name starts earlier.
///
/// Under `Sort::Touched` the grouping is DROPPED — "what did I touch last" is a
/// question about the store, not about a project, and grouping it would sort
/// the answer away from the top of the screen.
pub fn group_tasks(rows: Vec<Task>, today: Date, sort: Sort) -> Tasks {
    // Which rows belong in the list is a property of the ORDER it is in.
    //
    // Under urgency and due it is open work: a finished task has no urgency to
    // rank and no deadline left to meet. Under `touched` it is everything —
    // that ordering is what RECENT was a panel for, and RECENT's whole point
    // was that "a task finished four minutes ago is exactly what this answers".
    // Dropping closed rows there would fold the panel in and lose the question
    // it existed to answer.
    let mut rows: Vec<Task> = match sort {
        Sort::Touched => rows,
        _ => rows.into_iter().filter(|t| t.status.is_open()).collect(),
    };
    let total = rows.len();
    let overdue_of = |t: &Task| t.due_date().is_some_and(|d| d < today);

    let order = |a: &Task, b: &Task| match sort {
        Sort::Urgency => b
            .urgency
            .partial_cmp(&a.urgency)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.short_id.cmp(&b.short_id)),
        // Undated last rather than first: `None` sorts before `Some` by
        // default, which would open the list with every task that has no
        // deadline at all under a heading about deadlines.
        Sort::Due => a
            .due
            .is_none()
            .cmp(&b.due.is_none())
            .then(a.due.cmp(&b.due))
            .then(a.short_id.cmp(&b.short_id)),
        Sort::Touched => b
            .modified
            .cmp(&a.modified)
            .then(a.short_id.cmp(&b.short_id)),
    };
    rows.sort_by(order);

    if sort == Sort::Touched {
        let overdue = rows.iter().filter(|t| overdue_of(t)).count();
        return Tasks {
            groups: vec![TaskGroup {
                project: None,
                overdue,
                rows,
            }],
            total,
            sort,
        };
    }

    let mut groups: Vec<TaskGroup> = Vec::new();
    for t in rows {
        let key = t.project().map(str::to_string);
        match groups.iter_mut().find(|g| g.project == key) {
            Some(g) => g.rows.push(t),
            None => groups.push(TaskGroup {
                project: key,
                overdue: 0,
                rows: vec![t],
            }),
        }
    }
    for g in &mut groups {
        g.overdue = g.rows.iter().filter(|t| overdue_of(t)).count();
    }
    // The first row of each group is its hottest (or soonest), so the groups
    // are already comparable by it.
    groups.sort_by(|a, b| match (a.rows.first(), b.rows.first()) {
        (Some(x), Some(y)) => order(x, y),
        _ => std::cmp::Ordering::Equal,
    });

    Tasks {
        groups,
        total,
        sort,
    }
}

/// How the work is MOVING, as opposed to what is on the list (D80).
///
/// Every figure is derived from the snapshot the other panels already read —
/// no call was added for it, which is what the spec checked before naming it.
#[derive(Clone, Debug, Default)]
pub struct Pulse {
    /// Completions in the last 7, 14 and 30 days.
    pub done: [usize; 3],
    /// Days from `created` to `completed`, median, over everything finished —
    /// and over the last fortnight, so a change of pace is visible as one.
    pub cycle_days: Option<f64>,
    pub cycle_days_recent: Option<f64>,
    /// The oldest open task, in days since it was created, and its id.
    pub oldest: Option<(i64, i64)>,
    /// Open tasks nobody has touched in a fortnight.
    pub untouched: usize,
    /// Opened minus closed over the burndown's own window: is the pile growing.
    pub churn: i64,
}

/// What the open work is going to COST, and what it has cost.
#[derive(Clone, Debug, Default)]
pub struct Effort {
    /// Estimate summed over OPEN rows — hours left, not hours ever.
    pub est_open_secs: i64,
    /// Tracked, over the same scope `report.summary` reports it for.
    pub tracked_secs: i64,
    /// Per project, biggest estimate first: (name, estimate, tracked).
    pub by_project: Vec<(Option<String>, i64, i64)>,
}

/// Deadlines, bucketed by calendar date.
#[derive(Clone, Debug)]
pub struct ProjectRow {
    name: Option<String>,
    pub archived: bool,
    pub is_default: bool,
    pub open: usize,
    pub overdue: usize,
    pub est_secs: i64,
    pub tracked_secs: i64,
}

impl ProjectRow {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Projects {
    pub rows: Vec<ProjectRow>,
}

/// The remaining-open series, straight from `chart::burndown`.
#[derive(Clone, Debug, Default)]
pub struct Burndown {
    pub series: Vec<chart::RemainingPoint>,
    pub days: usize,
    /// The event page came back full, so the window may be incomplete.
    pub truncated: bool,
}

/// One project's four token buckets, in the fixed D48 order.
#[derive(Clone, Debug)]
pub struct TokenRow {
    name: Option<String>,
    pub buckets: [i64; 4],
    // #217: D50's trust hierarchy, carried from `report.summary`'s
    // `tokens_confidence` — the group's WORST measurement, since the four
    // buckets above are already a blend across every measurement that fed
    // them and cannot answer "how sure are we" on their own.
    confidence: Option<String>,
}

impl TokenRow {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The blended total across all four buckets — display and sort order
    /// only; D50 removed the API's own `tokens_total` because the buckets do
    /// not mean the same thing, and this is not a second one.
    ///
    /// Saturating, not `sum()` (#209): a single `token.add` with no ceiling on
    /// its input can plant a bucket near `i64::MAX`, and plain addition then
    /// wraps NEGATIVE — sorting the biggest spender to the bottom of the one
    /// panel built to surface it, silently, in release; in debug the same
    /// unchecked `sum()` panics the whole screen instead. Saturating pins the
    /// unrepresentable total at `i64::MAX`, which is still the biggest number
    /// on the panel rather than the smallest.
    pub fn total(&self) -> i64 {
        self.buckets
            .iter()
            .fold(0i64, |acc, &n| acc.saturating_add(n))
    }

    /// The group's worst confidence, or `None` when `report.summary` sent
    /// none — no token metric was requested, or nothing was measured.
    pub fn confidence(&self) -> Option<&str> {
        self.confidence.as_deref()
    }
}

/// Token spend per project.
///
/// The four buckets are never blended into one number and never priced (D48):
/// they have different economic meanings and tasqx has no price list.
#[derive(Clone, Debug, Default)]
pub struct Tokens {
    pub rows: Vec<TokenRow>,
    /// Column-wise totals, in `BUCKETS` order. Summed here because the API has
    /// no grand total — D50 removed `tokens_total` on purpose.
    pub totals: [i64; 4],
}

/// Everything the screen draws.
#[derive(Clone, Debug, Default)]
pub struct Dashboard {
    /// The date every relative cell is measured against.
    ///
    /// Carried on the model rather than read at draw time, for the reason the
    /// mappers take it as a parameter: a renderer that called `today()` itself
    /// would be untestable at a fixed instant, and would disagree with the
    /// buckets it is drawing whenever a redraw straddles midnight.
    pub today: Date,
    pub status: StatusBar,
    pub projects: Projects,
    pub burndown: Burndown,
    pub tokens: Tokens,
    /// The one task list D80 folds NOW, NEXT UP, DUE, BLOCKED and RECENT into.
    pub tasks: Tasks,
    pub pulse: Pulse,
    pub effort: Effort,
}

// ============================================================================
// The mappers
// ============================================================================

fn rows_of<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter())
        .unwrap_or_else(|| [].iter())
}

/// The four results one refresh reads, plus the two bounds that shaped them.
///
/// Grouped because they are one snapshot and are only meaningful together:
/// `events` was fetched with `event_limit` and a `from` derived from `days`, so
/// mixing results from two refreshes would give a burndown whose window
/// disagreed with its own truncation flag.
pub struct Sources<'a> {
    /// `task.list {}` — unfiltered, and that matters: the burndown reconstructs
    /// backwards from current state and needs every task's status, done and
    /// cancelled included.
    pub tasks: &'a Value,
    /// `report.summary {group_by:"project", metrics:[…]}` — feeds PROJECTS and
    /// TOKENS both, in one call, because it is the heaviest read in the set.
    pub summary: &'a Value,
    /// `project.list {include_archived:true}` — an archived project can still
    /// hold tasks and still get a summary group, so hiding it here would leave
    /// that group unjoinable.
    pub projects: &'a Value,
    /// `event.list {from, limit}` — the burndown's window.
    pub events: &'a Value,
    /// The `limit` that was sent, so a full page can be recognised as possibly
    /// clipped rather than reported as complete.
    pub event_limit: usize,
    /// The burndown window, in days.
    pub days: usize,
}

/// Build every panel from one snapshot.
///
/// `now` and `today` are PARAMETERS. Every due bucket, the elapsed clock and the
/// week's completions are functions of the clock, and a mapper that read it
/// itself could not be tested at a fixed instant — the rule `datetime.rs` states
/// and `remind.rs` follows.
pub fn build(src: Sources<'_>, now: Timestamp, today: Date) -> Dashboard {
    build_sorted(src, now, today, Sort::default())
}

/// [`build`] with the list order the reader last chose, which `r` and the
/// auto-refresh have to carry across a rebuild — a refresh that silently reset
/// the sort would be the screen undoing a keypress.
pub fn build_sorted(src: Sources<'_>, now: Timestamp, today: Date, sort: Sort) -> Dashboard {
    let Sources {
        tasks,
        summary,
        projects,
        events,
        event_limit,
        days: burndown_days,
    } = src;
    let all: Vec<Task> = rows_of(tasks, "tasks")
        .filter_map(Task::from_json)
        .collect();

    // ---- the one list -----------------------------------------------------
    //
    // NOW, NEXT UP, DUE, BLOCKED and RECENT each built their own Vec over these
    // same rows, and each cloned. One grouped list replaces all five (D80);
    // what they selected on is on the row — `active_since`, `blocked`, `due`,
    // `modified` — so the marker, the column and the sort read it directly.
    let mut rows: Vec<Task> = all.clone();
    for t in &mut rows {
        if let Some(started) = t.active_since {
            // Saturating, and signed on purpose: a store written by a machine
            // whose clock has since moved back would otherwise produce a
            // negative elapsed that formats as nonsense.
            let elapsed = (now.as_second() - started.as_second()).max(0);
            t.running_secs = Some(t.tracked_secs + elapsed);
        }
    }
    let overdue_now = rows
        .iter()
        .filter(|t| t.status.is_open() && t.due_date().is_some_and(|d| d < today))
        .count();
    let blocked_now = rows
        .iter()
        .filter(|t| t.status.is_open() && t.blocked)
        .count();

    // ---- PULSE: how the work is moving -----------------------------------
    //
    // Medians, not means: one task that sat open for a year drags a mean into
    // uselessness, and a cycle time is read to answer "how long does this
    // usually take".
    let day = 24 * 3600;
    let since = |days: i64| now.as_second() - days * day;
    let done_in = |days: i64| {
        all.iter()
            .filter(|t| t.completed.is_some_and(|c| c.as_second() >= since(days)))
            .count()
    };
    let median = |mut v: Vec<f64>| -> Option<f64> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(f64::total_cmp);
        let mid = v.len() / 2;
        Some(if v.len().is_multiple_of(2) {
            (v[mid - 1] + v[mid]) / 2.0
        } else {
            v[mid]
        })
    };
    let cycle_of = |from: i64| -> Option<f64> {
        median(
            all.iter()
                .filter_map(|t| t.completed.map(|c| (t.created, c)))
                .filter(|(_, c)| c.as_second() >= from)
                .map(|(created, c)| (c.as_second() - created.as_second()) as f64 / day as f64)
                .collect(),
        )
    };
    let oldest = all
        .iter()
        .filter(|t| t.status.is_open())
        .min_by_key(|t| t.created)
        .map(|t| (t.short_id, (now.as_second() - t.created.as_second()) / day));
    let pulse = Pulse {
        done: [done_in(7), done_in(14), done_in(30)],
        cycle_days: cycle_of(i64::MIN / 2),
        cycle_days_recent: cycle_of(since(14)),
        oldest,
        untouched: all
            .iter()
            .filter(|t| t.status.is_open() && t.modified.as_second() < since(14))
            .count(),
        churn: {
            let from = since(burndown_days as i64);
            let opened = all.iter().filter(|t| t.created.as_second() >= from).count() as i64;
            let closed = all
                .iter()
                .filter(|t| t.completed.is_some_and(|c| c.as_second() >= from))
                .count() as i64;
            opened - closed
        },
    };

    // ---- PROJECTS + TOKENS: one summary, joined to the snapshot ----------
    let (projects_panel, tokens_panel) = build_projects_and_tokens(&all, summary, projects, today);

    // ---- BURNDOWN ---------------------------------------------------------
    // Status and creation date come from the SAME snapshot the status bar
    // counts, which is what stops the two from disagreeing about a task.
    let members: Vec<chart::Member> = all
        .iter()
        .map(|t| chart::Member {
            id: t.id.clone(),
            created: t.created.to_zoned(TimeZone::UTC).date(),
            open_now: t.status.is_open(),
            status: t.status.as_str().to_string(),
        })
        .collect();
    let event_count = events.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
    let burndown = Burndown {
        series: chart::burndown(events, &members, burndown_days, today),
        days: burndown_days,
        truncated: event_limit > 0 && event_count >= event_limit,
    };

    // ---- the header, counted from the panels ------------------------------
    let week_ago = now.as_second() - 7 * 24 * 3600;
    let status = StatusBar {
        project: rows_of(projects, "projects")
            .find(|p| p.get("default").and_then(Value::as_bool).unwrap_or(false))
            .and_then(|p| p.get("name").and_then(Value::as_str))
            .map(render::san),
        open: all.iter().filter(|t| t.status.is_open()).count(),
        active: all.iter().filter(|t| t.active_since.is_some()).count(),
        // Counted from the SAME rows the list draws, so the header cannot
        // disagree with the body under it — the rule that used to be "taken
        // from the panels", now that there is one panel to take them from.
        overdue: overdue_now,
        blocked: blocked_now,
        done_week: all
            .iter()
            .filter(|t| t.completed.is_some_and(|c| c.as_second() >= week_ago))
            .count(),
    };

    // ---- EFFORT: what the open work will cost ----------------------------
    //
    // The estimate is summed over OPEN rows here rather than taken from
    // `report.summary`, and the difference is the point: D24's scope includes
    // finished work, so the summary's `est_total` answers "how much was ever
    // estimated" where this panel asks "how much is left".
    let mut by_project: Vec<(Option<String>, i64, i64)> = Vec::new();
    for t in all.iter().filter(|t| t.status.is_open()) {
        let key = t.project().map(str::to_string);
        match by_project.iter_mut().find(|(p, _, _)| *p == key) {
            Some(row) => {
                row.1 += t.estimate_secs.unwrap_or(0);
                row.2 += t.tracked_secs;
            }
            None => by_project.push((key, t.estimate_secs.unwrap_or(0), t.tracked_secs)),
        }
    }
    by_project.retain(|(_, est, tracked)| *est > 0 || *tracked > 0);
    by_project.sort_by_key(|(_, est, tracked)| std::cmp::Reverse(*est + *tracked));
    let effort = Effort {
        est_open_secs: by_project.iter().map(|(_, e, _)| e).sum(),
        tracked_secs: by_project.iter().map(|(_, _, t)| t).sum(),
        by_project,
    };

    Dashboard {
        today,
        status,
        pulse,
        effort,
        tasks: group_tasks(rows, today, sort),
        projects: projects_panel,
        burndown,
        tokens: tokens_panel,
    }
}

/// The full-outer join between the snapshot, `report.summary` and `project.list`.
///
/// Three mismatches, each with a stated rule:
///
/// 1. A project with **no tasks** has no summary group — emit the row with zeros.
/// 2. An **archived** project that still holds tasks *does* get a summary group —
///    emit it, flagged, sorted after the live ones.
/// 3. Summary `count` **excludes cancelled work** (D24) while the snapshot does
///    not, so `open` and `overdue` are derived from the snapshot instead. That is
///    not a preference: a PROJECTS row that disagreed with NEXT UP about the same
///    project would make both untrustworthy. `est_total`/`tracked_total` stay on
///    the summary, because deriving those locally would fork the D24 rule itself.
fn build_projects_and_tokens(
    all: &[Task],
    summary: &Value,
    projects: &Value,
    today: Date,
) -> (Projects, Tokens) {
    // `report.summary` spells the project-less bucket "(none)" — a name a user
    // can really create, so it only maps to `None` when no real project owns it.
    let real_names: HashSet<&str> = rows_of(projects, "projects")
        .filter_map(|p| p.get("name").and_then(Value::as_str))
        .collect();
    let key_of = |raw: &str| -> Option<String> {
        if raw == "(none)" && !real_names.contains("(none)") {
            None
        } else {
            Some(raw.to_string())
        }
    };

    struct Agg {
        est: i64,
        tracked: i64,
        buckets: [i64; 4],
        confidence: Option<String>,
    }
    let mut agg: HashMap<Option<String>, Agg> = HashMap::new();
    for g in rows_of(summary, "groups") {
        let Some(raw) = g.get("project").and_then(Value::as_str) else {
            continue;
        };
        let mut buckets = [0i64; 4];
        for (i, (metric, _, _)) in BUCKETS.iter().enumerate() {
            buckets[i] = g.get(*metric).and_then(Value::as_i64).unwrap_or(0);
        }
        agg.insert(
            key_of(raw),
            Agg {
                est: g
                    .get("est_total")
                    .and_then(Value::as_str)
                    .and_then(tasqx_core::util::duration_secs)
                    .unwrap_or(0),
                tracked: g
                    .get("tracked_total")
                    .and_then(Value::as_str)
                    .and_then(tasqx_core::util::duration_secs)
                    .unwrap_or(0),
                buckets,
                confidence: g
                    .get("tokens_confidence")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
        );
    }

    // Every key any of the three sources knows about.
    let mut keys: Vec<Option<String>> = Vec::new();
    let mut seen: HashSet<Option<String>> = HashSet::new();
    let mut remember = |k: Option<String>, keys: &mut Vec<Option<String>>| {
        if seen.insert(k.clone()) {
            keys.push(k);
        }
    };
    for p in rows_of(projects, "projects") {
        if let Some(n) = p.get("name").and_then(Value::as_str) {
            remember(Some(n.to_string()), &mut keys);
        }
    }
    for t in all {
        remember(t.project().map(str::to_string), &mut keys);
    }
    for k in agg.keys() {
        remember(k.clone(), &mut keys);
    }

    let meta: HashMap<&str, (bool, bool)> = rows_of(projects, "projects")
        .filter_map(|p| {
            Some((
                p.get("name").and_then(Value::as_str)?,
                (
                    p.get("archived").and_then(Value::as_bool).unwrap_or(false),
                    p.get("default").and_then(Value::as_bool).unwrap_or(false),
                ),
            ))
        })
        .collect();

    let mut rows: Vec<ProjectRow> = keys
        .iter()
        .map(|k| {
            let (archived, is_default) = k
                .as_deref()
                .and_then(|n| meta.get(n).copied())
                .unwrap_or((false, false));
            let mine = || all.iter().filter(|t| t.project().map(str::to_string) == *k);
            let a = agg.get(k);
            ProjectRow {
                name: k.as_ref().map(|n| render::san(n)),
                archived,
                is_default,
                open: mine().filter(|t| t.status.is_open()).count(),
                overdue: mine()
                    .filter(|t| t.status.is_open() && t.due_date().is_some_and(|d| d < today))
                    .count(),
                est_secs: a.map(|a| a.est).unwrap_or(0),
                tracked_secs: a.map(|a| a.tracked).unwrap_or(0),
            }
        })
        .collect();
    // Live projects first, then by open count, then by name — a stable order, so
    // a refresh does not reshuffle rows under the reader's eye.
    rows.sort_by(|a, b| {
        a.archived
            .cmp(&b.archived)
            .then(b.open.cmp(&a.open))
            .then(a.name.cmp(&b.name))
    });

    let mut token_rows: Vec<TokenRow> = keys
        .iter()
        .filter_map(|k| {
            let a = agg.get(k)?;
            (a.buckets.iter().any(|&n| n > 0)).then(|| TokenRow {
                name: k.as_ref().map(|n| render::san(n)),
                buckets: a.buckets,
                confidence: a.confidence.clone(),
            })
        })
        .collect();
    token_rows.sort_by_key(|r| std::cmp::Reverse(r.total()));
    let mut totals = [0i64; 4];
    for r in &token_rows {
        for (i, n) in r.buckets.iter().enumerate() {
            totals[i] = totals[i].saturating_add(*n);
        }
    }

    (
        Projects { rows },
        Tokens {
            rows: token_rows,
            totals,
        },
    )
}

// ============================================================================
// Geometry
// ============================================================================

/// The panels, in the order they are numbered on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum PanelId {
    /// The one task list (D80). NOW, NEXT UP, DUE, BLOCKED and RECENT were
    /// five panels over the same rows — a task could be drawn three times on
    /// one screen while the panel answering "what now" truncated at fifteen of
    /// twenty-four — and they fold in here as a marker, a column and a sort.
    Tasks,
    Projects,
    Burndown,
    /// How the work is moving: throughput, cycle time, what is aging.
    Pulse,
    /// What the open work will cost, and what it has cost.
    Effort,
    Tokens,
    /// The shared analytics slot: whichever of Projects/Burndown/Tokens is
    /// showing when there is only room for one of them.
    Slot,
}

/// The panels `dashboard.panels` may name, in the built-in order.
///
/// Hand-written because a `&'static [&'static str]` cannot be derived from the
/// enum at const time — so a test binds the two instead
/// (`the_panel_vocabulary_round_trips`), the shape `docs.rs` already uses
/// against clap. `Slot` is absent on purpose: it is a layout artefact, not a
/// panel anyone can ask for.
pub const PANEL_NAMES: &[&str] = &["tasks", "projects", "burndown", "pulse", "effort", "tokens"];

/// The panel names D80 retired, and the panel each now means.
///
/// A config that says `dashboard.panels = "now,next,due"` was written against a
/// screen that existed; rejecting it as five unknown words would be the tool
/// telling a reader their setting is wrong when what happened is that it moved.
/// They all resolve to `tasks`, which is where their rows went.
pub const RETIRED_PANEL_NAMES: [&str; 5] = ["now", "next", "due", "blocked", "recent"];

/// Every word `dashboard.panels` accepts: the panels, plus the names D80
/// retired.
///
/// Separate from [`PANEL_NAMES`] because the two answer different questions.
/// `PANEL_NAMES` is what the setting OFFERS — what `config list` prints and
/// what a reader should type. This is what it TOLERATES, so a file written
/// against the eight-panel screen still validates instead of being refused
/// word by word for naming panels that used to exist.
pub const ACCEPTED_PANEL_NAMES: [&str; 11] = [
    "tasks", "projects", "burndown", "pulse", "effort", "tokens", "now", "next", "due", "blocked",
    "recent",
];

impl PanelId {
    /// The name this panel goes by in config and in the `--json` document.
    /// `Slot` has none — nothing configures it.
    pub fn slug(self) -> Option<&'static str> {
        Some(match self {
            PanelId::Tasks => "tasks",
            PanelId::Projects => "projects",
            PanelId::Pulse => "pulse",
            PanelId::Effort => "effort",
            PanelId::Burndown => "burndown",
            PanelId::Tokens => "tokens",
            PanelId::Slot => return None,
        })
    }

    /// The panel a config value names, or `None` for a word that is not one.
    pub fn from_slug(s: &str) -> Option<PanelId> {
        if RETIRED_PANEL_NAMES.contains(&s) {
            return Some(PanelId::Tasks);
        }
        // Walked over `PANEL_NAMES` rather than a second list of the same
        // panels written out here — that pair is exactly the drift
        // `the_panel_vocabulary_round_trips` exists to catch, and the cheapest
        // way to pass it is not to have the pair.
        PANEL_NAMES
            .iter()
            .find(|name| **name == s)
            .and_then(|name| {
                [
                    PanelId::Tasks,
                    PanelId::Projects,
                    PanelId::Burndown,
                    PanelId::Pulse,
                    PanelId::Effort,
                    PanelId::Tokens,
                ]
                .into_iter()
                .find(|p| p.slug() == Some(*name))
            })
    }

    /// The digit that focuses (or, in the slot, places) this panel.
    pub fn digit(self) -> Option<u8> {
        Some(match self {
            PanelId::Tasks => 1,
            PanelId::Projects => 2,
            PanelId::Burndown => 3,
            PanelId::Pulse => 4,
            PanelId::Effort => 5,
            PanelId::Tokens => 6,
            PanelId::Slot => return None,
        })
    }

    /// The three panels that share the analytics slot when space is short.
    pub const SLOT_MEMBERS: [PanelId; 5] = [
        PanelId::Projects,
        PanelId::Burndown,
        PanelId::Pulse,
        PanelId::Effort,
        PanelId::Tokens,
    ];

    pub fn title(self) -> &'static str {
        match self {
            PanelId::Tasks => "TASKS",
            PanelId::Projects => "PROJECTS",
            PanelId::Pulse => "PULSE",
            PanelId::Effort => "EFFORT",
            PanelId::Burndown => "BURNDOWN",
            PanelId::Tokens => "TOKENS",
            PanelId::Slot => "ANALYTICS",
        }
    }
}

/// How much of a panel is drawn. Ordered, so `>` means "more detail".
///
/// There is deliberately no `Hidden`: the fit expresses hiding by not placing
/// the panel at all, so a `Hidden` variant would be a second way to say the same
/// thing — and the one a renderer could accidentally draw.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Detail {
    OneLine,
    Compact,
    Full,
}

/// A panel's body-line cost at each level. `None` means the level does not exist
/// for that panel — a burndown in one line is not a burndown.
struct Spec {
    id: PanelId,
    full: u16,
    compact: Option<u16>,
    oneline: Option<u16>,
    /// Whether leftover rows may be given to this panel.
    grows: bool,
}

const SPECS: [Spec; 7] = [
    Spec {
        // The list. It is the screen, so it takes rows until it runs out of
        // tasks and it never stops at a level: `Detail` reads as a richness
        // setting, and this panel has exactly one richness.
        id: PanelId::Tasks,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        id: PanelId::Projects,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        id: PanelId::Burndown,
        full: 3,
        compact: Some(2),
        oneline: None,
        grows: true,
    },
    Spec {
        id: PanelId::Pulse,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        id: PanelId::Effort,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        id: PanelId::Tokens,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        // No level below `full`. The slot holds a chart or a table, and either
        // of those in one line is neither — so below three rows it is omitted
        // rather than shrunk, which is the fit's own rule for a panel that
        // cannot be drawn whole.
        id: PanelId::Slot,
        full: 3,
        compact: None,
        oneline: None,
        grows: true,
    },
];

fn spec(id: PanelId) -> &'static Spec {
    SPECS
        .iter()
        .find(|s| s.id == id)
        .expect("every PanelId has a Spec")
}

impl Spec {
    /// Body lines at a level, or `None` if the level does not exist here.
    fn body(&self, d: Detail) -> Option<u16> {
        match d {
            Detail::Full => Some(self.full),
            Detail::Compact => self.compact,
            Detail::OneLine => self.oneline,
        }
    }

    /// The lowest level this panel can be drawn at.
    fn floor(&self) -> Detail {
        if self.oneline.is_some() {
            Detail::OneLine
        } else if self.compact.is_some() {
            Detail::Compact
        } else {
            Detail::Full
        }
    }
}

/// Where one panel goes. Coordinates are absolute and OUTER: row `y` is the
/// panel's title rule, and [`Placement::body`] is what may hold task text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Placement {
    pub id: PanelId,
    pub detail: Detail,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Placement {
    /// `(x, y, w, h)` of the rows this panel may write into. Never zero-height
    /// for a placed panel — that is the invariant the fit exists to keep.
    pub fn body(&self) -> (u16, u16, u16, u16) {
        (self.x, self.y + 1, self.w, self.h.saturating_sub(1))
    }
}

/// Which rung of the ladder a terminal is on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Rung {
    Xs,
    S,
    M,
    L,
    Xl,
}

/// Which rung a terminal of this size lands on.
///
/// A function rather than a field on `Screen`, because the renderer stopped
/// asking: the footer used to branch on the rung to choose between two literal
/// hint lists, and now the width alone decides how many hints fit (D62). The
/// layout still classifies, and the ladder guards still assert against the
/// classification — so it lives where both can call it rather than as a field
/// only the tests read.
pub fn rung_for(width: u16, height: u16) -> Rung {
    // Read out of the table rather than repeated as an if-chain: the same
    // numbers used to live in both, and `every_rung_gives_its_columns_room_to_read`
    // asserts against the table — so a lowered breakpoint here would have passed
    // the guard that exists to catch exactly that.
    let by_width = RUNG_MIN_WIDTH
        .iter()
        .find(|(_, min)| width >= *min)
        .map(|(r, _)| *r)
        .unwrap_or(Rung::Xs);
    let by_height = if height >= 40 {
        Rung::Xl
    } else if height >= 32 {
        Rung::L
    } else if height >= 28 {
        Rung::M
    } else if height >= 22 {
        Rung::S
    } else {
        Rung::Xs
    };
    // The most constraining axis wins: a 200x16 tmux split has room for columns
    // and none for panels.
    by_width.min(by_height)
}

/// The whole screen's geometry.
#[derive(Clone, Debug)]
pub struct Screen {
    pub columns: u16,
    /// Row 0, full width.
    pub status: Placement,
    pub panels: Vec<Placement>,
    /// The frame's closing rule.
    pub rule_y: u16,
    /// The key line.
    pub footer_y: u16,
}

impl Screen {
    pub fn placement(&self, id: PanelId) -> Option<&Placement> {
        self.panels.iter().find(|p| p.id == id)
    }

    /// Whether the analytics slot is in use — i.e. Projects/Burndown/Tokens are
    /// sharing one rectangle rather than each having their own.
    pub fn has_slot(&self) -> bool {
        self.placement(PanelId::Slot).is_some()
    }
}

/// The narrowest a column may be. Below this a task title is all ellipsis and
/// the column says nothing.
///
/// There is deliberately **no runtime guard** enforcing this. The width
/// breakpoints below already satisfy it by construction — the tightest case is
/// `L`, three columns from 120 cells, giving 40 — so a guard would be a branch
/// no input can reach, and unreachable defensive code is worse than none: it
/// reads as a handled case and is never exercised. The invariant is held by
/// `every_rung_gives_its_columns_room_to_read` instead, which walks the
/// breakpoint table itself and goes red if one is ever lowered.
#[cfg(test)]
pub(crate) const MIN_COLUMN: u16 = 34;

/// Rows the frame always spends: the status bar, the closing rule, the footer.
const CHROME_ROWS: u16 = 3;

/// Each column's `(x, width)`, left to right, covering the full width with no
/// gap — the last one absorbs the remainder of an uneven division.
///
/// Extracted so the chrome compositor and [`layout`] cannot disagree about
/// where a column boundary is. They did not have to: the same three lines
/// existed in both, and a boundary the chrome drew one cell away from the
/// panel edge would put a seam glyph through a task title on every row.
pub(crate) fn column_extents(width: u16, columns: u16) -> Vec<(u16, u16)> {
    let mut out = Vec::with_capacity(columns as usize);
    let mut x = 0u16;
    for i in 0..columns {
        let w = if i + 1 == columns {
            width - x
        } else {
            width / columns
        };
        out.push((x, w));
        x += w;
    }
    out
}

/// The floor below which the alternate screen is never entered (D58).
pub const MIN_WIDTH: u16 = 56;
pub const MIN_HEIGHT: u16 = 14;

/// Turn a terminal size into a set of absolute rectangles, or `None` when the
/// terminal is too small to draw on at all.
///
/// `order` is the configured panel order (`dashboard.panels`); a panel absent
/// from it is never placed. The returned rectangles are final — the renderer
/// does no splitting of its own, which is what keeps this function's tests
/// meaningful rather than describing a geometry the screen then re-derives.
pub fn layout(
    width: u16,
    height: u16,
    order: &[PanelId],
    demand: &dyn Fn(PanelId) -> u16,
) -> Option<Screen> {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return None;
    }

    let rung = rung_for(width, height);
    let columns = columns_for(rung);
    let column_rows = height - CHROME_ROWS;

    let wanted = |id: PanelId| order.contains(&id);
    let slot_members_wanted = PanelId::SLOT_MEMBERS.iter().copied().any(wanted);
    let cols = column_table(rung);

    let mut panels = Vec::new();
    let extents = column_extents(width, columns);
    for (i, col) in cols.iter().enumerate() {
        let (x, w) = extents[i];
        let members: Vec<PanelId> = col
            .iter()
            .copied()
            .filter(|id| {
                if *id == PanelId::Slot {
                    slot_members_wanted
                } else {
                    wanted(*id)
                }
            })
            .collect();
        for p in fit(&members, column_rows, x, w, demand) {
            panels.push(p);
        }
    }

    Some(Screen {
        columns,
        status: Placement {
            id: PanelId::Tasks, // the status bar is not a panel; `id` is unused
            detail: Detail::OneLine,
            x: 0,
            y: 0,
            w: width,
            h: 1,
        },
        panels,
        rule_y: height - 2,
        footer_y: height - 1,
    })
}

/// Which panel goes in which column, top to bottom, per rung. The slot exists
/// only where there is not room for its three members separately.
fn column_table(rung: Rung) -> Vec<Vec<PanelId>> {
    match rung {
        // Two columns on every wide rung, not three.
        //
        // Three columns divided 120 cells into forty, and forty cells is where
        // a task title stops being a title. Widening the terminal did not fix
        // it — at 160 the same three columns held the same content and left the
        // same holes — because the shortage was never width.
        //
        // The list gets a column to itself; everything that gives it context —
        // where the work is, how it is burning down, what it has cost — shares
        // the other. That is D80's
        // arrangement.
        Rung::Xl | Rung::L => vec![
            vec![
                PanelId::Projects,
                PanelId::Burndown,
                PanelId::Pulse,
                PanelId::Effort,
                PanelId::Tokens,
            ],
            vec![PanelId::Tasks],
        ],
        // Under 120 a context column would starve the list, so all three
        // analytics panels fold into the slot — which is what the slot is for,
        // and drawing PROJECTS both beside the slot and inside it would be the
        // same panel twice.
        Rung::M => vec![vec![PanelId::Slot], vec![PanelId::Tasks]],
        Rung::S => vec![vec![PanelId::Tasks, PanelId::Slot]],
        // The floor: nothing but the list. Every other panel is context, and
        // context is what you drop first.
        Rung::Xs => vec![vec![PanelId::Tasks]],
    }
}

pub(crate) fn columns_for(r: Rung) -> u16 {
    // The count IS [`column_table`]'s length. It used to be a second per-rung
    // literal kept in sync by hand, and the drift mode of that pair was
    // `extents[i]` indexing out of bounds mid-frame, inside the raw-mode alt
    // screen. Derived, the two cannot disagree.
    u16::try_from(column_table(r).len()).expect("column_table holds at most a handful of columns")
}

/// The narrowest terminal that can reach each rung — the other half of the
/// breakpoint table, exposed so a test can prove the two agree rather than
/// re-typing the numbers beside them.
pub(crate) const RUNG_MIN_WIDTH: [(Rung, u16); 5] = [
    (Rung::Xl, 150),
    (Rung::L, 120),
    (Rung::M, 96),
    (Rung::S, 72),
    (Rung::Xs, MIN_WIDTH),
];

/// The order panels are raised out of their floor level when there is room.
///
/// Not the display order: this is what a reader wants *more* of first. NEXT UP
/// earns rows before RECENT does, because choosing what to do next is the
/// question the screen exists to answer.
const RAISE_ORDER: [PanelId; 7] = [
    // The list first, and by a distance: it is the question the screen exists
    // to answer, and everything under it is context for it. Then where the work
    // is, how it is burning down, how it is moving, what it will cost, what it
    // has cost — falling order of how often a reader acts on the answer.
    PanelId::Tasks,
    PanelId::Projects,
    PanelId::Burndown,
    PanelId::Pulse,
    PanelId::Effort,
    PanelId::Tokens,
    PanelId::Slot,
];

/// How many body rows `id` could actually fill, given this data.
///
/// The ceiling [`layout`] grows a panel to. It counts the lines the matching
/// builder in `panels` would produce at its richest level — headers included,
/// because DUE spends a row on each non-empty bucket name and a ceiling that
/// forgot them would stop the panel one row short of its last task.
///
/// Deliberately an over-estimate where it cannot be exact: a panel handed one
/// row too many shows a blank line, a panel handed one too few hides a task
/// behind a `…1 more` that the space was there for.
///
/// `slot_members` is which analytics panels the reader configured. The slot is
/// sized for the TALLEST of them, not for whichever one is showing: `6`, `7` and
/// `8` swap the occupant in place, and a slot that shrank to fit the current one
/// would have to grow again on the next keypress — or fail to, and vanish. A key
/// that makes a panel disappear is not a key anybody presses twice.
/// The body rows a panel costs at the lowest level it can be drawn at.
///
/// The bound `fit` never goes under, and the one a test can compare a placement
/// against without copying the numbers out of `SPECS`.
pub(crate) fn floor_body(id: PanelId) -> u16 {
    let s = spec(id);
    s.body(s.floor()).unwrap_or(0)
}

pub fn demand(dash: &Dashboard, slot_members: &[PanelId], id: PanelId) -> u16 {
    let n = |len: usize| -> u16 { u16::try_from(len).unwrap_or(u16::MAX).max(1) };
    match id {
        // The card is three lines about one task; there is no fourth. This is
        // the one panel whose height is a property of the panel rather than of
        // how much there is to list — except when there is no task, where
        // `now_body` has one sentence and asking for three rows leaves two
        // blank at the top of the first screen anyone ever sees.
        // Every task, plus a heading per group. An over-estimate where it
        // cannot be exact, per this function's own rule: a row too many is a
        // blank line, a row too few hides a task behind a `…1 more` the space
        // was there for.
        PanelId::Tasks => {
            let headings = if dash.tasks.sort == Sort::Touched {
                0
            } else {
                dash.tasks.groups.len()
            };
            n(dash.tasks.total + headings)
        }
        // A step line reads better the taller it is — the shape IS the answer,
        // and eight rows is where a swing stops being a texture and becomes a
        // slope (`chart::BURNDOWN_ROWS`). The sparkline this replaced could not
        // use height at all, and the ceiling said so: "a wider window means a
        // longer sparkline, never a taller one". It does now.
        PanelId::Burndown => {
            let plot = u16::try_from(crate::chart::BURNDOWN_ROWS).unwrap_or(8);
            plot + 1 + u16::from(dash.burndown.truncated)
        }
        // Only the projects with work in them, plus the line that accounts for
        // the rest. A dashboard answers "what now", and a project with nothing
        // open does not participate in that question — on a real store fourteen
        // of twenty said `0 open`, and the panel was handed a row for each.
        PanelId::Projects => {
            let live = dash.projects.rows.iter().filter(|r| r.open > 0).count();
            n(live) + u16::from(live < dash.projects.rows.len())
        }
        PanelId::Tokens => n(dash.tokens.rows.len()),
        // Throughput, cycle time, what is aging, and the churn — four lines,
        // each of which is a sentence rather than a row of a list, so this one
        // is a property of the panel and not of the store.
        PanelId::Pulse => 4,
        // The totals, then a bar per project that has any.
        PanelId::Effort => n(dash.effort.by_project.len() + 1),
        PanelId::Slot => slot_members
            .iter()
            .map(|m| demand(dash, &[], *m))
            .max()
            .unwrap_or(1),
    }
}

/// How many SELECTABLE rows `id` has — what the cursor counts.
///
/// A sibling of [`demand`] and not a use of it: `demand` counts body LINES,
/// because that is what a rectangle has to be tall enough to hold, and for DUE
/// the two differ by one line per non-empty bucket name. A cursor clamped to
/// the line count walks onto a heading; `Enter` then has nothing to open.
///
/// The panels absent from the match have no selectable row at all. NOW is
/// present despite drawing a fixed body (#228.13): its one row IS the
/// running task, so it needs no cursor position of its own, only the 0/1 a
/// row count already gives every panel for free — `row_at` never reads a
/// position back for it, only whether one exists. BURNDOWN has no such row
/// to give. TOKENS is here for a duller reason: `panels::body` hands
/// `tokens_body` no position and never has, so a cursor in it moved an
/// integer that nothing read.
pub fn row_count(dash: &Dashboard, id: PanelId) -> usize {
    match id {
        PanelId::Tasks => dash.tasks.total,
        // #228.13: NOW is the one row a reader looks at most — the task
        // actively running — and it drew no cursor at all, so it was the
        // only row-bearing panel Enter never reached. It carries at most one
        // row (the running task, if any), which is exactly what a cursor of
        // 0/1 already expresses without a new code path.
        PanelId::Projects => dash.projects.rows.len(),
        _ => 0,
    }
}

/// One annotation, as `task.get` returns it.
#[derive(Clone, Debug)]
pub struct Note {
    pub created: Option<Timestamp>,
    pub body: String,
}

/// What `⏎` shows: one `task.get`, mapped.
///
/// A second reader beside [`Task`] rather than fields bolted onto it, because
/// the two come from different calls and the difference matters. `task.list`
/// fills every panel and carries no `depends_on`, no tags and no annotations;
/// `task.get` is one row, fetched once, and carries all three. Widening `Task`
/// would give every row on the screen three fields that are `None` for all of
/// them and populated for one.
///
/// `depends_on` is why this exists. It lives on `task.get` alone, so it is the
/// first thing on this screen that can answer BLOCKED's question.
#[derive(Clone, Debug)]
pub struct TaskDetail {
    pub short_id: i64,
    title: String,
    project: Option<String>,
    pub status: Status,
    pub priority: Option<Prio>,
    pub urgency: f64,
    pub blocked: bool,
    pub due: Option<Timestamp>,
    pub scheduled: Option<Timestamp>,
    pub wait: Option<Timestamp>,
    pub created: Timestamp,
    pub modified: Timestamp,
    pub completed: Option<Timestamp>,
    pub active_since: Option<Timestamp>,
    pub estimate_secs: Option<i64>,
    pub tracked_secs: i64,
    recurrence: Option<String>,
    depends_on: Vec<i64>,
    tags: Vec<String>,
    annotations: Vec<Note>,
}

impl TaskDetail {
    /// Sanitised title. The only way to read it.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Sanitised project name, `None` for the project-less bucket.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// Sanitised recurrence rule, `None` where the task does not repeat.
    pub fn recurrence(&self) -> Option<&str> {
        self.recurrence.as_deref()
    }

    /// The short_ids this task waits on — the answer BLOCKED could never give.
    pub fn depends_on(&self) -> &[i64] {
        &self.depends_on
    }

    /// Sanitised tag names.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// Annotations, newest last, bodies sanitised.
    pub fn annotations(&self) -> &[Note] {
        &self.annotations
    }

    /// Map one `task.get` result.
    ///
    /// Every display string goes through the shared sanitiser HERE (D19), not
    /// at draw time — a ratatui cell is written to the terminal verbatim, and
    /// annotation bodies and tag names arrive on this call and on no other, so
    /// [`Task::from_json`]'s care does not cover them.
    pub fn from_json(v: &Value) -> Option<Self> {
        let ts = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
        };
        let modified: Timestamp = ts("modified")?;
        Some(TaskDetail {
            short_id: v.get("short_id").and_then(Value::as_i64)?,
            title: render::san(v.get("title").and_then(Value::as_str).unwrap_or("")),
            project: v.get("project").and_then(Value::as_str).map(render::san),
            status: Status::parse(v.get("status").and_then(Value::as_str).unwrap_or("")),
            priority: v
                .get("priority")
                .and_then(Value::as_str)
                .and_then(Prio::parse),
            urgency: v.get("urgency").and_then(Value::as_f64).unwrap_or(0.0),
            blocked: v.get("blocked").and_then(Value::as_bool).unwrap_or(false),
            due: ts("due"),
            scheduled: ts("scheduled"),
            wait: ts("wait"),
            created: ts("created").unwrap_or(modified),
            modified,
            completed: ts("completed"),
            active_since: ts("active_since"),
            // The one checked duration reader (D17/D14), for the reason
            // `Task::from_json` gives: a hand-rolled parse here would be the
            // next copy, and a previous copy was a bug.
            estimate_secs: v
                .get("estimate")
                .and_then(Value::as_str)
                .and_then(tasqx_core::util::duration_secs),
            tracked_secs: v
                .get("tracked")
                .and_then(Value::as_str)
                .and_then(tasqx_core::util::duration_secs)
                .unwrap_or(0),
            recurrence: v.get("recurrence").and_then(Value::as_str).map(render::san),
            depends_on: v
                .get("depends_on")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default(),
            tags: v
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(render::san)
                        .collect()
                })
                .unwrap_or_default(),
            annotations: v
                .get("annotations")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|n| Note {
                            created: n
                                .get("created")
                                .and_then(Value::as_str)
                                .and_then(|s| s.parse().ok()),
                            body: render::san(n.get("body").and_then(Value::as_str).unwrap_or("")),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// The task at row `idx` of `id`, or `None` where that row is not a task.
///
/// The other half of [`row_count`], and the reason the cursor counts rows: this
/// is what `⏎` opens. DUE walks its four buckets in the order `due_body` draws
/// them, so the index the reader moved is the index resolved here — the same
/// map, not a second one that agrees today.
///
/// PROJECTS answers `None` on every row. It has rows and a cursor, and they are
/// projects; a detail overlay for one is a different decision and is not this
/// one. [`project_at`] is `⏎`'s answer there instead (#204).
pub fn row_at(dash: &Dashboard, id: PanelId, idx: usize) -> Option<&Task> {
    match id {
        // Group headings are not rows: the cursor walks the tasks, and the
        // headings between them are drawn, not landed on.
        PanelId::Tasks => dash.tasks.groups.iter().flat_map(|g| &g.rows).nth(idx),
        _ => None,
    }
}

/// The project name at row `idx` of PROJECTS, for the `⏎` [`row_at`] refuses
/// there (#204).
///
/// `Option<Option<&str>>`, and both layers mean something different: the OUTER
/// `None` is "no such row" (past the end, or the wrong panel), the same
/// question `row_at` answers; the INNER `None` is the "(none)" bucket —
/// tasks with no project at all — which is a real row with a cursor on it and
/// nothing `project:VALUE` can express, because that predicate never matches a
/// task with no project (`filter::Pred::Project`). `⏎` there has nothing to
/// open either, for a different reason than an out-of-range row does.
pub fn project_at(dash: &Dashboard, id: PanelId, idx: usize) -> Option<Option<&str>> {
    match id {
        PanelId::Projects => dash.projects.rows.get(idx).map(ProjectRow::name),
        _ => None,
    }
}

/// The body cost of one level, for a test that checks the two agree.
#[cfg(test)]
pub(crate) fn spec_body(id: PanelId, d: Detail) -> u16 {
    spec(id).body(d).unwrap_or(0)
}

/// The highest detail level `body` rows can pay for.
///
/// Derived rather than tracked, so the level and the space can never disagree:
/// a panel drawn as `Full` in two rows was possible while the two were decided
/// separately.
fn detail_for(id: PanelId, body: u16) -> Detail {
    let s = spec(id);
    for level in [Detail::Full, Detail::Compact, Detail::OneLine] {
        if s.body(level).is_some_and(|n| body >= n) {
            return level;
        }
    }
    Detail::OneLine
}

/// Fit one column's panels into `budget` rows: floor, then one row at a time.
///
/// Every placement carries its own title rule, so a panel showing `n` body lines
/// occupies `n + 1` rows. Adjacent panels share that rule visually — the rule
/// under one panel is the title of the next — which is why the fixed cost is
/// three rows for the whole frame rather than two per panel. A `Block` with
/// `Borders::ALL` would cost two rows each and make this ladder impossible; it
/// also cannot draw the tee junctions the design calls for.
fn fit(
    members: &[PanelId],
    budget: u16,
    x: u16,
    w: u16,
    demand: &dyn Fn(PanelId) -> u16,
) -> Vec<Placement> {
    if members.is_empty() || budget == 0 {
        return Vec::new();
    }
    // Phase 1 — everything at its lowest level, dropping from the BOTTOM of the
    // column until it fits. A panel that cannot be drawn whole is not drawn.
    let mut live: Vec<(PanelId, Detail)> =
        members.iter().map(|id| (*id, spec(*id).floor())).collect();
    let cost = |v: &[(PanelId, Detail)]| -> u16 {
        v.iter()
            .map(|(id, d)| spec(*id).body(*d).unwrap_or(0) + 1)
            .sum()
    };
    while !live.is_empty() && cost(&live) > budget {
        live.pop();
    }
    if live.is_empty() {
        return Vec::new();
    }

    // Phase 2 — hand out the leftover rows ONE AT A TIME, in priority order,
    // cycling until nobody can use another.
    //
    // One row at a time is the whole point. This used to be two phases: raise a
    // panel a whole level (Compact costs +1, Full costs +3), then scatter what
    // was left over the growers. Both were reasonable and together they were
    // not MONOTONIC — a taller terminal could show LESS. Measured on the real
    // ladder at 80 columns:
    //
    //     80x28   Now:3  Next:4  Due:2  Blocked:2  Slot:6  Recent:2
    //     80x29   Now:3  Next:4  Due:5  Blocked:1  Slot:6  Recent:1
    //
    // One extra row let DUE jump to Full, which cost three, which emptied the
    // pool that had been giving BLOCKED and RECENT their extra row each. Five
    // such inversions existed between 14 and 40 rows.
    //
    // Handing out single rows removes the class: one more row in the budget is
    // one more row for exactly one panel, so no panel can ever lose one.
    let mut rows: HashMap<PanelId, u16> = live
        .iter()
        .map(|(id, d)| (*id, spec(*id).body(*d).unwrap_or(0)))
        .collect();
    let mut leftover = budget - cost(&live);
    // Priority order first, then anything else in column order — a panel absent
    // from RAISE_ORDER must still be able to grow.
    let order: Vec<PanelId> = RAISE_ORDER
        .into_iter()
        .filter(|id| live.iter().any(|(l, _)| l == id))
        .chain(
            live.iter()
                .map(|(id, _)| *id)
                .filter(|id| !RAISE_ORDER.contains(id)),
        )
        .collect();
    // Strict priority, not a round robin.
    //
    // This loop used to cycle: one row to NEXT UP, one to RECENT, one to NEXT
    // UP, and so on until both were capped. That is monotonic, which is what it
    // was written for — but it is not a PRIORITY, and `RAISE_ORDER`'s own doc
    // claims to be one ("what a reader wants more of first"). Sharing equally
    // only lets the order decide who takes the last odd row. Measured at 120x40
    // on a real store: NEXT UP stopped at fifteen of its twenty-four tasks with
    // `…9 more`, while RECENT — history, and last in the order — was handed
    // seventeen rows in the same column.
    //
    // Draining one panel to its ceiling before starting the next keeps the
    // monotonicity (a row added to the budget still only ever ADDS to some
    // panel, so none can lose one) and makes the order mean what it says.
    for id in &order {
        if leftover == 0 {
            break;
        }
        let have = rows[id];
        // A panel takes rows until it is Full; a grower then keeps taking
        // them until it runs out of CONTENT.
        //
        // Without the second half a grower took rows forever. At 120x40
        // against a store with two blocked tasks, BLOCKED was handed a
        // twelve-row box holding two lines, DUE swallowed the rest of its
        // column, and BURNDOWN — whose neighbour had nothing left to show —
        // was left on its floor of two. A third of the screen was blank
        // INSIDE panels while the panel that could have used the space was
        // squeezed.
        //
        // Capping at demand moves that slack to the bottom of the column,
        // outside every panel, and lets it reach whichever panel in the
        // column still has something to put there.
        //
        // The floor, not `full`, is the other bound. `Detail` reads as a
        // richness setting but only [`panels::now_body`] branches on it —
        // every other builder is handed a height and fills it — so raising
        // a two-item list to `Full` buys nothing but blank rows. NOW keeps
        // its three by demanding them, which is where that belongs: it is a
        // property of the card, not of the ladder.
        // `grows` is the CEILING, not a switch: every panel takes what it
        // has content for and at least its floor, and a non-grower simply
        // stops at `full`. TOKENS saying "no token spend attributed yet" is
        // one line, and it used to be given three because it was a
        // non-grower and non-growers took `full` unconditionally.
        let s = spec(*id);
        let want = demand(*id).max(floor_body(*id));
        let want = if s.grows { want } else { want.min(s.full) };
        if have >= want {
            continue;
        }
        // Everything it can use, or everything that is left. Rows nobody can
        // use stay unspent and show as a gap at the bottom of the column.
        let take = (want - have).min(leftover);
        *rows.get_mut(id).expect("seeded above") = have + take;
        leftover -= take;
    }

    let mut out = Vec::with_capacity(live.len());
    let mut y = 1u16; // row 0 is the status bar
    for (id, _) in live {
        let body = rows[&id];
        // The detail level is a FUNCTION of the rows a panel got, not a
        // separate decision — that is what keeps the two from disagreeing about
        // what is drawn in the space it was given.
        let detail = detail_for(id, body);
        let h = body + 1;
        out.push(Placement {
            id,
            detail,
            x,
            y,
            w,
            h,
        });
        y += h;
    }
    out
}

#[cfg(test)]
mod tests;
