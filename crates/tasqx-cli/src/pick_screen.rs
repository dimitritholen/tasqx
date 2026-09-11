//! The `tasqx pick` driver (DESIGN.md §10, D55, D124): rows out of every
//! task.list page, the alt-screen loop and the one read the screen asks for,
//! and the started-task summary. The widget
//! itself lives in `tui::pick`; the structural TTY gate stays in `execute`,
//! above the store-open, where its ordering is the property.

use super::*;

/// The refusal a non-interactive `tasqx pick` gives.
///
/// A constant because it is asserted from two places — the unit test below and
/// `tests/help.rs`, which drives the real binary — and a message pinned by a
/// hand-copied substring in each is a message that drifts out from under both.
/// It names the two commands that answer the same question without a screen,
/// because "needs a terminal" on its own leaves a scripter with nothing to type
/// next; `config edit`'s refusal earns its exit 2 the same way.
pub(crate) const PICK_NEEDS_A_TERMINAL: &str =
    "`tasqx pick` needs an interactive terminal on stdin and stdout \
     (one of them is piped, redirected, or TERM=dumb). `tasqx next` picks the \
     highest-urgency task for you and `tasqx start <ref>` starts it.";

/// `tasqx pick` — browse tasks on a full screen, and start one (D124).
///
/// The pieces this function owns are the ones the state machine must not: the
/// candidate snapshot, the card read, and the write. Everything between them
/// is `tui::pick`.
///
/// # The TTY gate is NOT here
///
/// It runs in [`execute`], above `open_backend`, and the position is load-bearing
/// rather than tidy: reaching this function at all means the store has already
/// been opened — created and migrated, if the machine had none — so a gate here
/// would refuse a pipe *after* writing a database the caller never asked for.
/// That is what shipped, and what D55 and `help.rs` both claimed did not happen.
/// This function may therefore be called only when `tui::is_interactive` has
/// already said yes; `pick_refuses_a_piped_stdout_with_a_nonzero_exit` asserts
/// the ordering by pointing `$TASQX_DB` at a path that must still not exist
/// afterwards.
pub(crate) fn run_pick(be: &mut Backend, ctx: &Ctx, filter: &[String]) -> CmdOutcome {
    // The same default and the same argv-preserving parse as `list`: `pick` is
    // a chooser over the working set, so the set it offers must be the set
    // `tasqx` shows, token for token.
    let filter_str = if filter.is_empty() {
        "@working".to_string()
    } else {
        tasqx_core::filter::from_argv(filter)
    };
    let listed = pick_candidates(be, &filter_str)?;
    let rows = pick_rows(&listed);
    if rows.is_empty() {
        return Err(no_candidates(&filter_str));
    }

    let mut app = tui::pick::App::new(rows, ctx, &filter_str, jiff::Timestamp::now());
    let chosen = tui::with_terminal(|term| pick_loop(term, be, &mut app))
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;

    // Cancelling is exit 4, not exit 0. `pick` exists to produce one task; when
    // it produced none, answering ok is a command reporting success for work it
    // did not do — this project's named recurring defect, and the reason
    // `config edit`'s "no changes" exit 0 is NOT the precedent to copy. That
    // screen is a session where zero edits is a legitimate outcome; this one is
    // a selection whose entire output is the choice.
    let Some(short_id) = chosen else {
        return Err(ApiError::not_found(
            "nothing picked — no task was started",
            None,
        ));
    };
    let result = be.call(
        "task.start",
        &json!({ "ref": short_id.to_string(), "keep": false }),
    )?;
    let text = picked_summary(ctx, &result);
    // The title out of the snapshot the screen was built from, for the
    // `--json` body's identity fields (below).
    let title = app
        .rows()
        .iter()
        .find(|r| r.short_id == short_id)
        .map(|r| r.title.clone())
        .unwrap_or_default();
    Ok((pick_result(short_id, &title, result), text))
}

/// An empty candidate set is a refusal, not an empty screen.
///
/// Exit 4 with the filter quoted back, because the two ways to get here look
/// identical from the outside — a store with nothing pending, and a filter that
/// excludes everything — and only the text can tell them apart. Opening the
/// screen on zero rows instead would put the user in an alt screen whose only
/// available action is leaving it.
pub(crate) fn no_candidates(filter: &str) -> ApiError {
    ApiError::not_found(
        format!("no task matches `{filter}` — nothing to pick (try `tasqx list {filter}`)"),
        None,
    )
}

/// Every task `filter` selects, in `-urgency` order, read one `task.list` page
/// at a time: `{tasks, count}` with `count` the number read.
///
/// All of them and not the first page, because this is a browser with a
/// search (D124), and a search cannot rank what was never read — the memory
/// screen pages for the same reason. It used to make one call, which D110
/// bounds at 100 rows, so on a larger working set the task the reader was
/// searching for could simply not be there, and the header counted the page
/// as though it were the set.
pub(crate) fn pick_candidates(be: &mut Backend, filter: &str) -> Result<Value, ApiError> {
    // D110's ceiling, so the common store is read in one call: `task.list`
    // scans the whole store each time (D58), and a small page made opening
    // the screen cost one scan per page.
    candidates_in_pages(be, filter, 10_000)
}

/// [`pick_candidates`] with the page size named, so a test can make the loop
/// run without seeding ten thousand tasks.
///
/// Pages are read at different instants and a write can land between them,
/// so a task can arrive twice across a boundary; it is kept once, where it
/// first appeared.
pub(crate) fn candidates_in_pages(
    be: &mut Backend,
    filter: &str,
    page: u64,
) -> Result<Value, ApiError> {
    let mut pages: Vec<Value> = Vec::new();
    let mut offset = 0u64;
    loop {
        let listed = be.call(
            "task.list",
            &json!({ "filter": filter, "sort": ["-urgency"], "limit": page, "offset": offset }),
        )?;
        let next = listed["next_offset"].as_u64();
        pages.push(listed);
        match next {
            Some(next) if next > offset => offset = next,
            _ => break,
        }
    }
    Ok(merge_pages(&pages))
}

/// `task.list` pages as one answer, each task once, in the order it first
/// appeared: `{tasks, count}`.
pub(crate) fn merge_pages(pages: &[Value]) -> Value {
    let mut seen = std::collections::HashSet::new();
    let tasks: Vec<Value> = pages
        .iter()
        .flat_map(|p| p["tasks"].as_array().cloned().unwrap_or_default())
        .filter(|t| seen.insert(t["short_id"].as_i64()))
        .collect();
    let count = tasks.len();
    json!({ "tasks": tasks, "count": count })
}

/// The `task.list` answer, as the rows the screen draws.
///
/// Extracted so it is reachable from a test at all: everything around it needs
/// a real terminal, and a mapping that read `id` where it meant `short_id` —
/// which would make every `s` start the wrong task or none at all — would leave
/// the whole suite green with the screen unusable. That is the same hole
/// `settings_rows` was pulled out of `run_config_edit` to close. Each row keeps
/// the task as the store sent it, because `render::task_row` lays it out (D124).
pub(crate) fn pick_rows(result: &Value) -> Vec<tui::pick::Row> {
    result
        .get("tasks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .cloned()
        .map(tui::pick::Row::new)
        .collect()
}

/// Draw, serve the one read the screen asks for, read one key, fold it in.
/// Returns the `short_id` to start, or `None` when the reader left without
/// starting anything.
pub(crate) fn pick_loop(
    term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    be: &mut Backend,
    app: &mut tui::pick::App,
) -> std::io::Result<Option<i64>> {
    use ratatui::crossterm::event::{self, Event};

    loop {
        // Re-read every iteration, so a resize between key presses changes
        // the page size and the card's width along with everything else.
        let size = term.size()?;
        app.observe(size.width, size.height);
        if let Some(id) = app.wanted() {
            // The card `tasqx show` prints (D122), read when it is opened. A
            // task that cannot be read says so on the card rather than ending
            // the session: the other tasks are still there to pick.
            let got = be
                .call("task.get", &json!({ "ref": id.to_string() }))
                .map_err(|e| format!("#{id} could not be read: {}", e.message));
            app.set_detail(id, got);
        }
        term.draw(|f| tui::pick::render(app, f))?;
        // Resize and paste events just redraw; only keys are decisions.
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match app.on_key(key) {
            Some(tui::pick::Action::Start { short_id }) => return Ok(Some(short_id)),
            Some(tui::pick::Action::Cancel) => return Ok(None),
            None => {}
        }
    }
}

/// The scrollback record `pick` leaves behind once the alt screen is gone:
/// `render::started`, the line `tasqx start` prints.
///
/// Since #75, `task.start` answers with the task's id and title and with what
/// it auto-stopped (D6), and `render::started` prints both. This used to add
/// a `#N  title` header over that, and D101's screen-local stand-in printed
/// the stop again from a snapshot of its own, so the task was named twice and
/// the stop said twice with two different durations. D101 said its stand-in
/// would be deleted once #75 landed; D124 did.
pub(crate) fn picked_summary(ctx: &Ctx, result: &Value) -> String {
    render::started(ctx, result)
}

/// The `--json` body: `task.start`'s own answer, plus the identity of the task
/// that was picked.
///
/// `task.start` returns `{id, interval_started}` — a UUID and a timestamp. That
/// is the right answer for a caller who supplied the ref, and a useless one
/// here, because the ref is the thing `pick` was asked to determine. The two
/// added fields are the CLI's own composition (as `agenda`'s `--json` body is),
/// never a change to what the method returns: the method's keys are passed
/// through untouched beside them.
pub(crate) fn pick_result(short_id: i64, title: &str, mut started: Value) -> Value {
    if let Some(obj) = started.as_object_mut() {
        obj.insert("short_id".to_string(), json!(short_id));
        obj.insert("title".to_string(), json!(title));
    }
    started
}
