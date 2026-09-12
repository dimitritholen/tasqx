//! The dashboard driver (D58): the terminal-fitness questions, the settings
//! that shape the screen, the four-payload data fetch, the auto-refresh loop
//! and the `--json` document. The pure model and the panels live under
//! `tui::dashboard`; the bare-invocation decision stays in `execute`, where
//! it must order against the store-open.

use super::*;

pub(crate) const DASHBOARD_NEEDS_A_TERMINAL: &str =
    "`tasqx dashboard` needs an interactive terminal on stdin and stdout \
     (one of them is piped, redirected, or TERM=dumb). `tasqx --json dashboard` \
     gives the same panels as data, and `tasqx list` prints the working set.";

/// Why this terminal cannot show the dashboard, or `None` when it can.
///
/// The policy, with the two facts it depends on injected — the split
/// `is_interactive_with` exists for, and for the same reason: under cargo the
/// process has a piped stdout, so a predicate that asked the process directly
/// would be untestable at exactly the point it can go wrong.
///
/// `size` is `None` when it could not be measured, which is a refusal rather
/// than a default: entering the alternate screen on a guess is how you paint a
/// half-drawn frame onto a window nobody can read.
pub(crate) fn dashboard_refusal(
    caps: &Caps,
    stdout_tty: bool,
    stdin_tty: bool,
    size: Option<(u16, u16)>,
) -> Option<String> {
    use tui::dashboard::model::{MIN_HEIGHT, MIN_WIDTH};
    if !tui::is_interactive_with(caps, stdout_tty, stdin_tty) {
        return Some(DASHBOARD_NEEDS_A_TERMINAL.to_string());
    }
    match size {
        Some((w, h)) if w >= MIN_WIDTH && h >= MIN_HEIGHT => None,
        Some((w, h)) => Some(format!(
            "this terminal is {w}x{h}; `tasqx dashboard` needs at least \
             {MIN_WIDTH}x{MIN_HEIGHT}. Resize the window, or run `tasqx list`."
        )),
        None => Some(
            "`tasqx dashboard` could not measure this terminal, so it will not enter \
             the alternate screen. Run `tasqx list` instead."
                .to_string(),
        ),
    }
}

/// The terminal's size, asked only once there is a terminal to ask about.
///
/// `crossterm::terminal::size()` consults `/dev/tty` and can fall back to
/// spawning `tput`, so in a pipe it answers about a window the bytes are not
/// going to. Asking it there would be worse than not asking.
pub(crate) fn terminal_size(caps: &Caps, stdout_tty: bool, stdin_tty: bool) -> Option<(u16, u16)> {
    if tui::is_interactive_with(caps, stdout_tty, stdin_tty) {
        ratatui::crossterm::terminal::size().ok()
    } else {
        None
    }
}

/// Whether a bare `tasqx` opens the dashboard rather than printing the table.
///
/// Four signals: a human is at the keyboard, no machine is reading the output,
/// the user has not switched it off, and the window is big enough to draw on.
/// Pure, and separate from everything that touches a terminal or a store, so
/// the condition is testable without either.
///
/// `fits` is the one that was missing when the screen first shipped, and its
/// absence was a real bug rather than a rough edge: bare `tasqx` in a 40x10
/// window entered the alternate screen, painted NOTHING — `layout` returns
/// `None` below 56x14 and `render` returns early — blocked until `q`, and
/// created a 208 KB store on the way in. That is D55's refused-screen-leaves-a-
/// store failure, rebuilt one screen over. Below the minimum a bare invocation
/// falls through to `run_list`: whoever typed nothing did not ask for a
/// dashboard, so silence is the right answer and the table is still useful.
///
/// `is_interactive` and nothing else answers the first: it asks about **stdout
/// and stdin both**, because the alternate screen is written to one and the key
/// loop blocks on the other. `Caps::detect() != PLAIN` is NOT the same question
/// — `CLICOLOR_FORCE=1` says "colour even when piped", and conflating the two is
/// what made `config edit | cat` hang forever (D26).
pub(crate) fn dashboard_active(
    caps: &Caps,
    json: bool,
    enabled: bool,
    fits: bool,
    stdout_tty: bool,
    stdin_tty: bool,
) -> bool {
    enabled && !json && fits && tui::is_interactive_with(caps, stdout_tty, stdin_tty)
}

/// Read `dashboard.enabled` — the escape hatch a breaking change owes its users.
///
/// Env before file so a CI image can switch it off in one line, and the default
/// is on. A malformed value reads as "on" for the same reason D57's hint does:
/// the failure of a setting that governs one screen must be the screen, not
/// silence.
///
/// The doc above said "env before file" while the code read the environment and
/// stopped. `dashboard.enabled` is a registered `Home::Toml` setting: `config
/// edit` draws it, `config list` prints it, `config set` writes it to
/// `config.toml` and reports success — and none of that did anything. A setting
/// that acknowledges a write and then ignores it is worse than one that does not
/// exist. It now goes through `config::resolve` like its three siblings, which
/// is where the precedence lives.
pub(crate) fn dashboard_enabled() -> bool {
    let s = config::find("dashboard.enabled").expect("dashboard.enabled is registered");
    dashboard_enabled_with(config::toml_value(s).as_deref())
}

/// [`dashboard_enabled`] over an explicit file value.
///
/// Split for the reason `config::toml_value_in` is: the file half must be
/// testable without mutating process-global env, which cargo's parallel test
/// threads make racy.
pub(crate) fn dashboard_enabled_with(file: Option<&str>) -> bool {
    let s = config::find("dashboard.enabled").expect("dashboard.enabled is registered");
    let (v, _) = config::resolve(s, None, file);
    // The env layer arrives verbatim — `resolve` does not coerce it — so the
    // three spellings a shell user reaches for are matched here rather than
    // leaving `TASQX_DASHBOARD=0` reading as "on". The file layer is already
    // normalised to `true`/`false` by `coerce`.
    !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no")
}

/// Read `dashboard.panels` — which panels the screen draws, in which order.
///
/// Total rather than fallible, and that is `coerce`'s doing: a value outside the
/// vocabulary is not a value, so it never reaches here and the resolver hands
/// back the default. What arrives is either the user's list or the built-in one.
pub(crate) fn dashboard_panels() -> Vec<tui::dashboard::model::PanelId> {
    let s = config::find("dashboard.panels").expect("dashboard.panels is registered");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    parse_panel_list(&v)
}

/// Parse a comma-separated panel list into the panels it names, in the order
/// given — the same vocabulary and the same "unknown word is dropped, not
/// refused" rule `dashboard.panels` applies through `coerce`, so a `--panels`
/// typo on the CLI and a bad value in `config.toml` fail the same quiet way
/// rather than the CLI inventing a second policy for the same string.
pub(crate) fn parse_panel_list(v: &str) -> Vec<tui::dashboard::model::PanelId> {
    use tui::dashboard::model::PanelId;
    // Deduplicated, in first-mention order. Five of D80's retired names now
    // resolve to the same panel, so a config saying `now,next,due` would
    // otherwise place TASKS three times — and a panel placed twice is a column
    // drawn twice over the same rows.
    let mut out: Vec<PanelId> = Vec::new();
    for name in v.split(',') {
        if let Some(id) = PanelId::from_slug(name.trim()) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Read `dashboard.window` as a day count.
///
/// Converted through `WINDOW_CHOICES` BY NAME, never by inventing a number:
/// `App::new` maps a day count back to an index with `unwrap_or(0)`, so a count
/// that is not in the list would silently become "week" and the `w` key would
/// start from somewhere the config never asked for.
pub(crate) fn dashboard_window_days() -> usize {
    let s = config::find("dashboard.window").expect("dashboard.window is registered");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    tui::dashboard::WINDOW_CHOICES
        .iter()
        .find(|(name, _)| *name == v)
        .map(|(_, days)| *days)
        .unwrap_or(7)
}

/// Read `dashboard.refresh` — whether the screen re-reads on a timer.
pub(crate) fn dashboard_auto_refresh() -> bool {
    let s = config::find("dashboard.refresh").expect("dashboard.refresh is registered");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v != "manual"
}

/// The four reads one dashboard refresh makes.
///
/// One `task.list {}` — UNFILTERED, because the burndown reconstructs backwards
/// from current state and needs every task's status — feeding five panels, and
/// one `report.summary` feeding two. `Engine::task_list` loads every row and
/// filters in Rust, so five limited calls would be five full scans; and two
/// summaries would double the heaviest read in the set.
pub(crate) fn dashboard_data(
    be: &mut Backend,
    days: usize,
    now: jiff::Timestamp,
    today: jiff::civil::Date,
) -> Result<tui::dashboard::model::Dashboard, ApiError> {
    use jiff::ToSpan;
    const EVENT_LIMIT: usize = 100_000;

    let tasks = be.call("task.list", &json!({}))?;
    let summary = be.call(
        "report.summary",
        &json!({
            "group_by": "project",
            "metrics": [
                "est_total", "tracked_total",
                "tokens_cache_read", "tokens_cache_creation", "tokens_in", "tokens_out"
            ]
        }),
    )?;
    // Archived projects still hold tasks and still get a summary group, so
    // hiding them here would leave that group unjoinable.
    let projects = be.call("project.list", &json!({ "include_archived": true }))?;
    let from = today.saturating_sub((days as i64 + 1).days());
    let events = be.call(
        "event.list",
        &json!({ "limit": EVENT_LIMIT, "from": format!("{from}T00:00:00Z") }),
    )?;

    Ok(tui::dashboard::model::build(
        tui::dashboard::model::Sources {
            tasks: &tasks,
            summary: &summary,
            projects: &projects,
            events: &events,
            event_limit: EVENT_LIMIT,
            days,
        },
        now,
        today,
    ))
}

/// How long the dashboard waits for a key before re-reading, with auto-refresh
/// on.
///
/// Five seconds rather than a configurable interval: every refresh is four
/// reads, one of which (`report.summary`) aggregates token measurements across
/// the store, so a knob here is an invitation to set a number that makes the
/// screen slow and blame the screen. `r` is always available, and `R` turns the
/// tick off entirely.
pub(crate) const REFRESH_TICK: u64 = 5;

/// Open the dashboard and stay in it until the user leaves.
///
/// Returns whatever should be printed into the scrollback afterwards — nothing,
/// normally, because a viewer writes nothing and there is nothing to audit.
pub(crate) fn run_dashboard(be: &mut Backend, ctx: &Ctx) -> Result<Option<String>, ApiError> {
    use ratatui::crossterm::event::{self, Event};
    use tui::dashboard::{Action, App};

    // One instant per data fetch, `today` DERIVED from it — never a second
    // clock read beside it, and never captured once for the process: a session
    // left open across UTC midnight kept bucketing DUE against yesterday
    // (OVERDUE/TODAY wrong until restart) while `now` moved on without it.
    // Each refresh below re-derives the pair the same way.
    let now = jiff::Timestamp::now();
    let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
    let days = dashboard_window_days();
    let mut app = App::new(
        dashboard_data(be, days, now, today)?,
        dashboard_panels(),
        days,
        dashboard_auto_refresh(),
    );

    // OUTER loop: the screen comes back after the picker.
    //
    // `p` used to hand the terminal back and let `run_pick`'s result become the
    // command's result — so backing out of the picker exited the whole process
    // (with the `not_found` that leaving was until D128), and choosing a task
    // started it and then quit.
    // Neither is what a key on a read-only overview should do. `pick` opens its
    // own `with_terminal`, so the dashboard's has to be closed first and
    // reopened after; D58 asks for one session with a `Screen` enum, which is
    // the remaining half of this work and would remove the flicker.
    let mut picked: Option<String> = None;
    loop {
        // What the user asked for on the way out, if anything. `Refresh` never
        // reaches here — it is served inside the loop, because a refresh that
        // tore the screen down and rebuilt it would flicker on every `r` and on
        // every tick of auto-refresh.
        let mut want: Option<Action> = None;
        let mut failed: Option<ApiError> = None;
        tui::with_terminal(|term| {
            loop {
                let mut placed = Vec::new();
                let mut has_slot = false;
                // Whether the LAST draw could lay panels out at all. `render`
                // draws the "too small" sentence instead of a blank frame when
                // it cannot (#201), but the state machine is never told the
                // terminal's size — this is what lets the check below answer
                // the same question without teaching `App` about pixels.
                let mut fits = true;
                term.draw(|f| {
                    tui::dashboard::render(&app, &ctx.theme, &ctx.caps, f);
                    match app.screen(f.area().width, f.area().height) {
                        Some(s) => {
                            placed = s.panels.iter().map(|p| p.id).collect();
                            has_slot = s.has_slot();
                        }
                        None => fits = false,
                    }
                })?;
                // The state machine is told what was drawn, as data — that is what
                // keeps Tab from stopping on a panel this size cannot show.
                app.observe(&placed, has_slot);

                // With auto-refresh on, wait at most one tick for a key and then
                // re-read; otherwise block until the user does something. Polling
                // rather than a background thread keeps this loop the only thing
                // that touches the terminal, which is the invariant that lets the
                // existing panic hook and `Restore` guard stay sufficient.
                if app.auto_refresh() && !event::poll(std::time::Duration::from_secs(REFRESH_TICK))?
                {
                    let now = jiff::Timestamp::now();
                    let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
                    match dashboard_data(be, app.window_days(), now, today) {
                        Ok(data) => app.replace(data),
                        Err(e) => {
                            failed = Some(e);
                            return Ok(());
                        }
                    }
                    continue;
                }
                // Resize and paste just redraw; only keys are decisions.
                let Event::Key(key) = event::read()? else {
                    continue;
                };
                // #201: below the floor the frame carries only the "too
                // small" sentence, so `?` or `⏎` would open an overlay
                // nobody can see — and a `q` meant to leave would then close
                // that invisible overlay instead of the program, which is
                // indistinguishable from a hang. `q`/`esc`/`ctrl-c` are the
                // only keys this state answers to, matched here rather than
                // through `app.on_key` so a modal `App` was already holding
                // open cannot swallow the one key that gets out.
                if !fits {
                    let press = key.kind == event::KeyEventKind::Press;
                    let quits = press
                        && (matches!(key.code, event::KeyCode::Char('q') | event::KeyCode::Esc)
                            || (key.code == event::KeyCode::Char('c')
                                && key.modifiers.contains(event::KeyModifiers::CONTROL)));
                    if quits {
                        return Ok(());
                    }
                    continue;
                }
                match app.on_key(key) {
                    Some(Action::Quit) => return Ok(()),
                    Some(Action::Refresh) => {
                        // A read that fails mid-session ends the session rather
                        // than redrawing stale numbers under a live-looking screen.
                        // The error is carried out and reported on the normal
                        // terminal, because a message printed here is wiped by the
                        // restore that follows it.
                        let now = jiff::Timestamp::now();
                        let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
                        match dashboard_data(be, app.window_days(), now, today) {
                            Ok(data) => app.replace(data),
                            Err(e) => {
                                failed = Some(e);
                                return Ok(());
                            }
                        }
                    }
                    Some(Action::Detail(id)) => {
                        // Served HERE, beside `Refresh`, and not carried out
                        // through `want`: everything that reaches the outer
                        // loop leaves this screen, so a missing arm would make
                        // `⏎` quit the dashboard.
                        match be.call("task.get", &json!({ "ref": id })) {
                            Ok(v) => match tui::dashboard::model::TaskDetail::from_json(&v) {
                                Some(card) => app.show_detail(card),
                                None => app.say(format!(
                                    "#{id} came back in a shape this build cannot read"
                                )),
                            },
                            // A row can be gone by the time the key arrives —
                            // auto-refresh redraws every five seconds and
                            // another terminal can finish the task in between.
                            // Expected, so it is a line in the footer; anything
                            // else ends the session as `Refresh` does.
                            Err(e) if e.code == tasqx_core::ErrorCode::NotFound => {
                                app.say(format!("#{id} is gone — r to refresh"));
                            }
                            Err(e) => {
                                failed = Some(e);
                                return Ok(());
                            }
                        }
                    }
                    Some(a) => {
                        want = Some(a);
                        return Ok(());
                    }
                    None => {}
                }
            }
        })
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;

        if let Some(e) = failed {
            return Err(e);
        }
        match want {
            // `l` is the one key that means "leave", so it does.
            Some(Action::List(scope)) => {
                return run_list(be, ctx, &scope, &[], None, None, &[]).map(|(_, r)| Some(r))
            }
            // `⏎` on PROJECTS (#204): the same leave-and-print shape as `l`,
            // scoped to the row the cursor was on. `filter::quote` and not a
            // raw `{name}`, exactly as `burndown_members` already does for a
            // project name that can hold a space or a quote.
            Some(Action::ListProject(name)) => {
                let filter = format!("project:{}", tasqx_core::filter::quote(&name));
                return run_list(be, ctx, &[filter], &[], None, None, &[]).map(|(_, r)| Some(r));
            }
            Some(Action::Pick(scope)) => {
                if let Some(said) = after_pick(run_pick(be, ctx, &scope), &mut picked)? {
                    app.say(said);
                }
                // Whatever happened, the screen shows it: a started task turns
                // up in NOW, and a cancelled pick redraws unchanged.
                let now = jiff::Timestamp::now();
                app.replace(dashboard_data(
                    be,
                    app.window_days(),
                    now,
                    now.to_zoned(jiff::tz::TimeZone::UTC).date(),
                )?);
            }
            // Quit, or nothing left to do.
            _ => return Ok(picked),
        }
    }
}

/// What the dashboard does with `pick`'s outcome, pulled out of the loop so a
/// test can reach it: returns a sentence for the dashboard's status line, or
/// the error that ends the session.
pub(crate) fn after_pick(
    outcome: CmdOutcome,
    picked: &mut Option<String>,
) -> Result<Option<String>, ApiError> {
    match outcome {
        // Backing out of the picker leaves nothing behind. Since D128 that is
        // an ordinary `Ok` rather than a `not_found`, so this arm has to come
        // first — without it the empty render below turns `picked` into
        // `Some("")`, and the dashboard prints a blank line into the
        // scrollback of a user who pressed `q`.
        //
        // The test is `short_id`, NOT D128's `started`, because the two answer
        // different questions. `started` is "did this invocation open a timer",
        // and it is false on an idempotent re-start (D105) — but the reader
        // pressed `s` there and got `render::started`'s "already running"
        // card back, which may not vanish on the way out of the screen. What
        // this arm asks is whether the picker acted on a task at all, and only
        // a leave comes back without one. Reading the render's emptiness
        // instead would work by accident, and would quietly stop recording a
        // start whose echo came back empty.
        Ok((body, _)) if body["short_id"].is_null() => {}
        // Every start of the session reaches the scrollback, not only the
        // last: each one can have auto-stopped another timer (D124(g)).
        Ok((_, render)) => picked.get_or_insert_with(String::new).push_str(&render),
        // An empty candidate set still refuses (`no_candidates`, exit 4), and
        // reached from a screen the user is going back to, that is not the
        // dashboard's error either: `p` over a project with nothing pending
        // redraws unchanged rather than ending the session.
        Err(e) if e.code == tasqx_core::ErrorCode::NotFound => {}
        // A start `task.start` refused: the task changed under the browser,
        // a race with another writer, since `pick` refuses what it can see
        // itself. Said on the dashboard's status line; the dashboard stays.
        Err(e) if e.code == tasqx_core::ErrorCode::Conflict => return Ok(Some(e.message)),
        // Anything else ends the session, as a failed read does everywhere
        // else in this loop.
        Err(e) => return Err(e),
    }
    Ok(None)
}

/// `tasqx --json dashboard` — the panels as data, with no screen involved.
///
/// This is why the verb is not a `JSON_CARVE_OUTS` entry (D58): the whole data
/// layer — every mapper, every join, the burndown reconstruction — becomes
/// reachable from a script and from a test that has no terminal, which is
/// otherwise only testable through a pty.
///
/// The human rendering is the document too. A `--json` carve-out would have
/// been dishonest, and a prose summary here would be a second surface to keep
/// true; anyone who wants prose has the screen.
///
/// `panels` is the CLI's `--panels`, honoured on this path only (#152): the
/// interactive screen keeps reading `dashboard.panels` from config no matter
/// what was typed alongside the bare verb, because a flag that reached a
/// screen already sized and drawn on this terminal would need a redraw
/// nothing here triggers. `None` falls back to `dashboard_panels()`, same as
/// before this parameter existed.
pub(crate) fn run_dashboard_json(be: &mut Backend, _ctx: &Ctx, panels: Option<&str>) -> CmdOutcome {
    let days = dashboard_window_days();
    let order = panels
        .map(parse_panel_list)
        .unwrap_or_else(dashboard_panels);
    let data = dashboard_data(be, days, jiff::Timestamp::now(), chart::today())?;
    let doc = tui::dashboard::json::document(&data, days, &order);
    let render = serde_json::to_string_pretty(&doc).unwrap_or_default();
    Ok((doc, render))
}

/// The event log from `days_back` days before `anchor`, for a chart that only
/// draws that far (D59).
///
/// Before `event.list` took a bound, every chart read `{limit: 100000}` — a full
/// scan of a log that is append-only and never pruned, on a table that grows
/// with every mutation the store has ever recorded. The bound is per-arm and not
/// hoisted: throughput, heatmap and burndown draw three different windows, and
/// one shared `from` would silently be the narrowest of them.
///
/// `days_back` is passed generously by callers (a week of slack on the
/// week-bucketed charts). The bound is an optimisation and not semantics —
/// which became TRUE only with D60: under the forwards reconstruction a
/// narrower window really did change the answer, and this comment claimed
/// otherwise for as long as that was the case. Backwards, existence comes from
/// `created` and today's state from the snapshot, so a clipped window costs
/// only the days whose changes it hid.
///
/// `limit` stays as the belt to this parameter's braces: `ORDER BY id DESC
/// LIMIT n` drops the OLDEST rows if it ever binds, which is the direction the
/// burndown can absorb.
pub(crate) fn events_since(
    engine: &Engine,
    anchor: jiff::civil::Date,
    days_back: usize,
) -> Result<Value, ApiError> {
    use jiff::ToSpan;
    let from = anchor.saturating_sub((days_back as i64).days());
    dispatch(
        engine,
        "event.list",
        &json!({ "limit": 100000, "from": format!("{from}T00:00:00Z") }),
    )
}

/// Resolve the task ids a burndown covers, plus its label. Split out of the
/// `ChartKind::Burndown` arm so the scope rule is testable on its own.
///
/// Both branches go through `task.list` with no status filter (D60). The `None`
/// branch previously used an unfiltered `store.export`, which is what let
/// cancelled tasks inflate the whole-store burndown's "remaining work" line.
pub(crate) fn burndown_members(
    engine: &Engine,
    project: &Option<String>,
) -> Result<(Vec<chart::Member>, String), ApiError> {
    let (filter, label) = match project {
        // Through `filter::quote`, never interpolated: a project may be named
        // `Home Renovation` or `a (b)`, and a raw `{p}` composes a filter that
        // asks a different question (or none at all) without saying so.
        Some(p) => (
            Some(format!("project:{}", tasqx_core::filter::quote(p))),
            p.clone(),
        ),
        None => (None, "all tasks".to_string()),
    };
    // No `NOT_CANCELLED` any more. It existed because the old reconstruction
    // guessed "open" for a task whose closing event it could not see, so a
    // cancelled task hung open on the chart forever and had to be excluded
    // wholesale. Reconstructing backwards from the snapshot status closes it on
    // its cancel date instead — and keeping the filter would now DELETE the task
    // from the days it was genuinely open, which is a different wrong answer.
    let mut params = json!({ "fields": ["id", "status", "created"] });
    if let Some(f) = filter {
        params["filter"] = Value::String(f);
    }
    let listed = dispatch(engine, "task.list", &params)?;
    Ok((chart::members_of(&listed), label))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `run_pick` hands back when `s` started a task — `pick_result`'s
    /// body. `short_id` is the load-bearing field here: it is what says the
    /// reader ACTED on a task, which is the dashboard's question. `started`
    /// answers a different one (D128, D105) and rides along.
    fn started(n: i64) -> CmdOutcome {
        Ok((
            json!({ "short_id": n, "started": true, "already_running": false }),
            format!("Started #{n}\n"),
        ))
    }

    /// What `run_pick` hands back when `s` hit a task that was ALREADY running
    /// — D105's idempotent re-start. Nothing started, so `started` is false,
    /// but `task.start` answered and `render::started` drew its "already
    /// running" card, which the reader pressed a key to get.
    fn already_running(n: i64) -> CmdOutcome {
        Ok((
            json!({ "short_id": n, "started": false, "already_running": true }),
            format!("#{n} already running\n"),
        ))
    }

    /// D124(g): every start of a dashboard session reaches the scrollback
    /// when it closes. Each start can have auto-stopped another timer, and
    /// keeping only the last lost the earlier "Stopped" lines.
    #[test]
    fn every_start_of_a_session_reaches_the_scrollback() {
        let mut picked = None;
        after_pick(started(1), &mut picked).unwrap();
        after_pick(started(2), &mut picked).unwrap();
        let text = picked.unwrap();
        assert!(text.contains("#1") && text.contains("#2"), "{text}");
    }

    /// D124(g): a start `task.start` refuses — the task changed under the
    /// browser, a race with another writer — is said on the dashboard's
    /// status line and the dashboard stays. Backing out is silent. Anything
    /// else still ends the session, as a failed read does elsewhere in it.
    #[test]
    fn a_refused_start_is_said_and_anything_else_still_ends_the_session() {
        let mut picked = None;
        let said = after_pick(
            Err(ApiError::conflict("cannot start a done task")),
            &mut picked,
        )
        .expect("a refused start must not end the dashboard");
        assert_eq!(said.as_deref(), Some("cannot start a done task"));
        // `no_candidates` is what reaches the `NotFound` arm now: since D128
        // leaving is an ordinary `Ok`, and the phrase this fixture used to
        // carry ("nothing picked") is an outcome `run_pick` can no longer
        // produce.
        assert_eq!(
            after_pick(Err(no_candidates("+nosuchtag")), &mut picked).unwrap(),
            None
        );
        assert!(picked.is_none());
        assert!(after_pick(Err(ApiError::internal("disk")), &mut picked).is_err());
    }

    /// D128: leaving `pick` is exit 0 now, so the dashboard's `p` gets an `Ok`
    /// where it used to get a `not_found`. It must still leave nothing behind
    /// — backing out of the picker is silent (D124(g)) — and an outcome that
    /// started nothing must not make the dashboard print an empty line into
    /// the scrollback on its way out.
    #[test]
    fn a_picker_closed_without_starting_anything_leaves_nothing_behind() {
        let mut picked = None;
        assert_eq!(
            after_pick(nothing_picked(), &mut picked).unwrap(),
            None,
            "backing out of the picker is silent"
        );
        assert!(
            picked.is_none(),
            "an empty render may not become a line in the scrollback: {picked:?}"
        );
        // And a start still reaches it, through the same arm.
        after_pick(started(1), &mut picked).unwrap();
        assert!(picked.unwrap().contains("#1"));
    }

    /// The dashboard's question is not D128's. `started` answers "did this
    /// invocation start a task", and is false on an idempotent re-start
    /// (D105); what `p` has to decide is whether the reader ACTED on a task at
    /// all. `s` on an already-running row gets `render::started`'s "already
    /// running · running for X" card back, and a key the reader pressed and
    /// got an answer to may not vanish on the way out of the screen. Keying
    /// this arm on `started` swallowed it.
    #[test]
    fn a_restart_of_an_already_running_task_still_reaches_the_scrollback() {
        let mut picked = None;
        assert_eq!(
            after_pick(already_running(7), &mut picked).unwrap(),
            None,
            "it is not a refusal either — nothing goes on the status line"
        );
        assert!(
            picked.as_deref().is_some_and(|t| t.contains("#7")),
            "the reader pressed `s` and got an answer; it may not be dropped: {picked:?}"
        );
    }
}
