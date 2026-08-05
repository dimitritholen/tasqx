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
use jiff::{Timestamp, ToSpan};
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
    /// `Some` exactly when the timer is running.
    pub active_since: Option<Timestamp>,
    pub estimate_secs: Option<i64>,
    pub tracked_secs: i64,
}

impl Task {
    /// Sanitised title. The only way to read it.
    pub fn title(&self) -> &str {
        &self.title
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

/// The running timer, or nothing.
#[derive(Clone, Debug)]
pub struct NowCard {
    pub task: Task,
    /// Seconds since the timer started, derived from the injected `now`.
    ///
    /// The instant itself stays on `task.active_since`; carrying it twice would
    /// be two answers to one question the moment a refresh replaced one of them.
    pub elapsed_secs: i64,
}

impl NowCard {
    /// Time on this task including the interval still running.
    ///
    /// The open interval is deliberately NOT folded into `tracked` by the core,
    /// and `render.rs` records why: an active task must say the clock is still
    /// running, or its tracked total reads as the final answer when it is only
    /// the total so far.
    pub fn total_secs(&self) -> i64 {
        self.task.tracked_secs.saturating_add(self.elapsed_secs)
    }
}

/// What to pick up next: the working set, by urgency.
#[derive(Clone, Debug, Default)]
pub struct NextUp {
    pub rows: Vec<Task>,
    /// The ramp denominator, computed the way `render` computes it so the
    /// dashboard and `tasqx list` shade the same task the same colour.
    pub max_urgency: f64,
}

/// Deadlines, bucketed by calendar date.
#[derive(Clone, Debug, Default)]
pub struct Due {
    pub overdue: Vec<Task>,
    pub today: Vec<Task>,
    pub tomorrow: Vec<Task>,
    pub week: Vec<Task>,
}

impl Due {
    pub fn is_empty(&self) -> bool {
        self.overdue.is_empty()
            && self.today.is_empty()
            && self.tomorrow.is_empty()
            && self.week.is_empty()
    }
}

/// Work that is standing still — invisible on every other default surface,
/// because `@working` excludes blocked tasks.
#[derive(Clone, Debug, Default)]
pub struct Blocked {
    pub rows: Vec<Task>,
}

/// Recently touched tasks, newest first.
///
/// Unfiltered by status on purpose: a task finished four minutes ago is exactly
/// what this panel answers. Its blind spot is documented rather than papered
/// over — `token.add` does not bump `modified`, so tasks whose only recent
/// activity is attributed AI spend do not appear. The rows are absent, not
/// approximate, which is why there is no hedge in the panel.
#[derive(Clone, Debug, Default)]
pub struct Recent {
    pub rows: Vec<Task>,
}

/// One project's roll-up.
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
}

impl TokenRow {
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn total(&self) -> i64 {
        self.buckets.iter().sum()
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
    pub now: Option<NowCard>,
    pub next: NextUp,
    pub due: Due,
    pub blocked: Blocked,
    pub recent: Recent,
    pub projects: Projects,
    pub burndown: Burndown,
    pub tokens: Tokens,
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

    // ---- NOW: the newest running timer -----------------------------------
    let now_card = all
        .iter()
        .filter(|t| t.active_since.is_some())
        .max_by_key(|t| t.active_since)
        .map(|t| {
            let started = t.active_since.expect("filtered on is_some");
            NowCard {
                task: t.clone(),
                // Saturating, and signed on purpose: a store written by a
                // machine whose clock has since moved back would otherwise
                // produce a negative elapsed that formats as nonsense.
                elapsed_secs: (now.as_second() - started.as_second()).max(0),
            }
        });

    // ---- NEXT UP: the working set ----------------------------------------
    let mut next_rows: Vec<Task> = all
        .iter()
        .filter(|t| t.status.is_open() && !t.blocked && t.status != Status::Backlog)
        .cloned()
        .collect();
    next_rows.sort_by(|a, b| b.urgency.total_cmp(&a.urgency));
    let max_urgency = next_rows
        .iter()
        .map(|t| t.urgency)
        .fold(0.0_f64, f64::max)
        .max(1.0);

    // ---- DUE: bucketed on the calendar ------------------------------------
    let mut due = Due::default();
    let tomorrow = today.saturating_add(1.days());
    let week_end = today.saturating_add(7.days());
    let mut dated: Vec<&Task> = all
        .iter()
        .filter(|t| t.status.is_open() && t.due.is_some())
        .collect();
    dated.sort_by_key(|t| t.due);
    for t in dated {
        let Some(d) = t.due_date() else { continue };
        if d < today {
            due.overdue.push(t.clone());
        } else if d == today {
            due.today.push(t.clone());
        } else if d == tomorrow {
            due.tomorrow.push(t.clone());
        } else if d <= week_end {
            due.week.push(t.clone());
        }
    }

    // ---- BLOCKED ----------------------------------------------------------
    let blocked = Blocked {
        rows: all
            .iter()
            .filter(|t| t.status.is_open() && t.blocked)
            .cloned()
            .collect(),
    };

    // ---- RECENT -----------------------------------------------------------
    let mut recent_rows: Vec<Task> = all.clone();
    recent_rows.sort_by_key(|t| std::cmp::Reverse(t.modified));
    let recent = Recent { rows: recent_rows };

    // ---- PROJECTS + TOKENS: one summary, joined to the snapshot ----------
    let (projects_panel, tokens_panel) = build_projects_and_tokens(&all, summary, projects, today);

    // ---- BURNDOWN ---------------------------------------------------------
    let member_ids: HashSet<String> = all.iter().map(|t| t.id.clone()).collect();
    let event_count = events.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
    let burndown = Burndown {
        series: chart::burndown(events, &member_ids, burndown_days, today),
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
        // Taken FROM the panels, never recounted, so the header cannot
        // disagree with the body it sits above.
        overdue: due.overdue.len(),
        blocked: blocked.rows.len(),
        done_week: all
            .iter()
            .filter(|t| t.completed.is_some_and(|c| c.as_second() >= week_ago))
            .count(),
    };

    Dashboard {
        today,
        status,
        now: now_card,
        next: NextUp {
            rows: next_rows,
            max_urgency,
        },
        due,
        blocked,
        recent,
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
    Now,
    Next,
    Due,
    Blocked,
    Recent,
    Projects,
    Burndown,
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
pub const PANEL_NAMES: &[&str] = &[
    "now", "next", "due", "blocked", "recent", "projects", "burndown", "tokens",
];

impl PanelId {
    /// The name this panel goes by in config and in the `--json` document.
    /// `Slot` has none — nothing configures it.
    pub fn slug(self) -> Option<&'static str> {
        Some(match self {
            PanelId::Now => "now",
            PanelId::Next => "next",
            PanelId::Due => "due",
            PanelId::Blocked => "blocked",
            PanelId::Recent => "recent",
            PanelId::Projects => "projects",
            PanelId::Burndown => "burndown",
            PanelId::Tokens => "tokens",
            PanelId::Slot => return None,
        })
    }

    /// The panel a config value names, or `None` for a word that is not one.
    pub fn from_slug(s: &str) -> Option<PanelId> {
        [
            PanelId::Now,
            PanelId::Next,
            PanelId::Due,
            PanelId::Blocked,
            PanelId::Recent,
            PanelId::Projects,
            PanelId::Burndown,
            PanelId::Tokens,
        ]
        .into_iter()
        .find(|p| p.slug() == Some(s))
    }

    /// The digit that focuses (or, in the slot, places) this panel.
    pub fn digit(self) -> Option<u8> {
        Some(match self {
            PanelId::Now => 1,
            PanelId::Next => 2,
            PanelId::Due => 3,
            PanelId::Blocked => 4,
            PanelId::Recent => 5,
            PanelId::Projects => 6,
            PanelId::Burndown => 7,
            PanelId::Tokens => 8,
            PanelId::Slot => return None,
        })
    }

    /// The three panels that share the analytics slot when space is short.
    pub const SLOT_MEMBERS: [PanelId; 3] = [PanelId::Projects, PanelId::Burndown, PanelId::Tokens];

    pub fn title(self) -> &'static str {
        match self {
            PanelId::Now => "NOW",
            PanelId::Next => "NEXT UP",
            PanelId::Due => "DUE",
            PanelId::Blocked => "BLOCKED",
            PanelId::Recent => "RECENT",
            PanelId::Projects => "PROJECTS",
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

const SPECS: [Spec; 9] = [
    Spec {
        id: PanelId::Now,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: false,
    },
    Spec {
        id: PanelId::Next,
        full: 4,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        id: PanelId::Due,
        full: 5,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        id: PanelId::Blocked,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: true,
    },
    Spec {
        id: PanelId::Recent,
        full: 4,
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
    // No one-line burndown: a sparkline needs an axis to mean anything.
    Spec {
        id: PanelId::Burndown,
        full: 5,
        compact: Some(3),
        oneline: None,
        grows: true,
    },
    Spec {
        id: PanelId::Tokens,
        full: 3,
        compact: Some(2),
        oneline: Some(1),
        grows: false,
    },
    // The slot is all-or-nothing: six body lines or it is not drawn.
    Spec {
        id: PanelId::Slot,
        full: 6,
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

/// The whole screen's geometry.
#[derive(Clone, Debug)]
pub struct Screen {
    pub rung: Rung,
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
pub fn layout(width: u16, height: u16, order: &[PanelId]) -> Option<Screen> {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return None;
    }

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
    let rung = by_width.min(by_height);
    let columns = columns_for(rung);
    let column_rows = height - CHROME_ROWS;

    // Which panel goes in which column, top to bottom. The slot exists only
    // where there is not room for its three members separately.
    let wanted = |id: PanelId| order.contains(&id);
    let slot_members_wanted = PanelId::SLOT_MEMBERS.iter().copied().any(wanted);
    let cols: Vec<Vec<PanelId>> = match rung {
        Rung::Xl | Rung::L => vec![
            vec![
                PanelId::Now,
                PanelId::Blocked,
                PanelId::Projects,
                PanelId::Tokens,
            ],
            vec![PanelId::Next, PanelId::Recent],
            vec![PanelId::Due, PanelId::Burndown],
        ],
        Rung::M => vec![
            vec![PanelId::Now, PanelId::Next, PanelId::Blocked],
            vec![PanelId::Due, PanelId::Recent, PanelId::Slot],
        ],
        Rung::S => vec![vec![
            PanelId::Now,
            PanelId::Next,
            PanelId::Due,
            PanelId::Blocked,
            PanelId::Slot,
            PanelId::Recent,
        ]],
        Rung::Xs => vec![vec![
            PanelId::Now,
            PanelId::Next,
            PanelId::Due,
            PanelId::Blocked,
            PanelId::Recent,
        ]],
    };

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
        for p in fit(&members, column_rows, x, w) {
            panels.push(p);
        }
    }

    Some(Screen {
        rung,
        columns,
        status: Placement {
            id: PanelId::Now, // the status bar is not a panel; `id` is unused
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

pub(crate) fn columns_for(r: Rung) -> u16 {
    match r {
        Rung::Xl | Rung::L => 3,
        Rung::M => 2,
        Rung::S | Rung::Xs => 1,
    }
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
const RAISE_ORDER: [PanelId; 9] = [
    PanelId::Next,
    PanelId::Now,
    PanelId::Due,
    PanelId::Recent,
    PanelId::Blocked,
    PanelId::Burndown,
    PanelId::Projects,
    PanelId::Tokens,
    PanelId::Slot,
];

/// Fit one column's panels into `budget` rows, three phases: floor, raise, grow.
///
/// Every placement carries its own title rule, so a panel showing `n` body lines
/// occupies `n + 1` rows. Adjacent panels share that rule visually — the rule
/// under one panel is the title of the next — which is why the fixed cost is
/// three rows for the whole frame rather than two per panel. A `Block` with
/// `Borders::ALL` would cost two rows each and make this ladder impossible; it
/// also cannot draw the tee junctions the design calls for.
fn fit(members: &[PanelId], budget: u16, x: u16, w: u16) -> Vec<Placement> {
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

    // Phase 2 — spend what is left raising panels, most-wanted first.
    let mut leftover = budget - cost(&live);
    for want in RAISE_ORDER {
        let Some(idx) = live.iter().position(|(id, _)| *id == want) else {
            continue;
        };
        for target in [Detail::Compact, Detail::Full] {
            let (id, current) = live[idx];
            if target <= current {
                continue;
            }
            let (Some(now_cost), Some(next_cost)) = (spec(id).body(current), spec(id).body(target))
            else {
                continue;
            };
            let delta = next_cost.saturating_sub(now_cost);
            if delta <= leftover {
                leftover -= delta;
                live[idx].1 = target;
            }
        }
    }

    // Phase 3 — hand any remaining rows to the growers, one at a time, cycling,
    // so a tall terminal fills rather than leaving a gap under the last panel.
    let mut extra: HashMap<PanelId, u16> = HashMap::new();
    let growers: Vec<PanelId> = live
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| spec(*id).grows)
        .collect();
    if !growers.is_empty() {
        let mut i = 0usize;
        while leftover > 0 {
            *extra.entry(growers[i % growers.len()]).or_insert(0) += 1;
            leftover -= 1;
            i += 1;
        }
    }

    let mut out = Vec::with_capacity(live.len());
    let mut y = 1u16; // row 0 is the status bar
    for (id, detail) in live {
        let h = spec(id).body(detail).unwrap_or(0) + 1 + extra.get(&id).copied().unwrap_or(0);
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
