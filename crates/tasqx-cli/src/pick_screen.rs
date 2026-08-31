//! The `tasqx pick` driver (DESIGN.md §10, D55): rows out of a task.list
//! answer, the alt-screen loop, and the started-task summary. The widget
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

/// `tasqx pick` — choose a task on a full-screen list, and start it.
///
/// The two pieces this function owns are the two the state machine must not:
/// the candidate snapshot and the write. Everything between them is `tui::pick`.
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
    let listed = be.call(
        "task.list",
        &json!({ "filter": filter_str, "sort": ["-urgency"] }),
    )?;
    let rows = pick_rows(&listed);
    if rows.is_empty() {
        return Err(no_candidates(&filter_str));
    }

    let mut app = tui::pick::App::new(rows);
    let caps = ctx.caps;
    let chosen = tui::with_terminal(|term| pick_loop(term, &mut app, &ctx.theme, caps))
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
    // The title is read back out of the snapshot the screen was built from,
    // because the screen is gone by the time this prints and `task.start`
    // answers with a UUID and a timestamp, neither of which a human recognises.
    let title = app
        .rows()
        .iter()
        .find(|r| r.short_id == short_id)
        .map(|r| r.title.clone())
        .unwrap_or_default();

    let result = be.call(
        "task.start",
        &json!({ "ref": short_id.to_string(), "keep": false }),
    )?;
    let text = picked_summary(ctx, short_id, &title, &result);
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

/// The `task.list` answer, flattened into the rows the screen draws.
///
/// Extracted so it is reachable from a test at all: everything around it needs
/// a real terminal, and a mapping that dropped a field — or read `id` where it
/// meant `short_id`, which would make every Enter start the wrong task or none
/// at all — would leave the whole suite green with the screen unusable. That is
/// the same hole `settings_rows` was pulled out of `run_config_edit` to close.
pub(crate) fn pick_rows(result: &Value) -> Vec<tui::pick::Row> {
    let field = |t: &Value, key: &str| -> String {
        t.get(key).and_then(Value::as_str).unwrap_or("").to_string()
    };
    result
        .get("tasks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|t| {
            let urgency = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
            tui::pick::Row::new(
                t.get("short_id").and_then(Value::as_i64).unwrap_or(0),
                &field(t, "title"),
                &field(t, "project"),
                // `-` and not an empty cell: a task with no priority is a fact,
                // and a blank column reads as a rendering bug.
                match field(t, "priority").as_str() {
                    "" => "-",
                    p => p,
                },
                &format!("{urgency:.1}"),
                &t.get("tags")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Draw, read one key, fold it in. Returns the chosen `short_id`, or `None`
/// when the user left without choosing.
///
/// The theme is resolved once, outside, and not per frame: unlike the settings
/// screen there is nothing here whose value depends on repainting in a
/// different theme, so re-loading it every keystroke would be work with no
/// observable effect.
pub(crate) fn pick_loop(
    term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::pick::App,
    theme: &theme::Theme,
    caps: Caps,
) -> std::io::Result<Option<i64>> {
    use ratatui::crossterm::event::{self, Event};

    loop {
        term.draw(|f| tui::pick::render(app, theme, &caps, f))?;
        // Resize and paste events just redraw; only keys are decisions.
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match app.on_key(key) {
            Some(tui::pick::Action::Choose { short_id }) => return Ok(Some(short_id)),
            Some(tui::pick::Action::Cancel) => return Ok(None),
            None => {}
        }
    }
}

/// The scrollback record `pick` leaves behind once the alt screen is gone.
///
/// `render::started` alone is not enough here, and that is not a style
/// preference: it prints "Started task · timer running (since …)" and names no
/// task, which is right for `tasqx start 42` — the user typed the ref — and
/// wrong for a screen that has just been wiped off the display. Without the
/// identity line an interactive session leaves no trace of WHICH task it
/// started, which is exactly the auditability `saved_summary` exists to give
/// `config edit`.
///
/// Extracted for the same reason as that function: the rest of `run_pick` needs
/// a real terminal, so this line would otherwise be the one piece of it no test
/// could ever see.
pub(crate) fn picked_summary(ctx: &Ctx, short_id: i64, title: &str, result: &Value) -> String {
    format!(
        "{}\n{}",
        ctx.paint("header", &format!("#{short_id}  {title}")),
        render::started(ctx, result)
    )
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
