//! The notes browser's driver (D241, task #929): `tasqx notes` on a terminal,
//! and `N` from the dashboard. The widget lives in `tui::notes`; this reads
//! the notes, runs the alt-screen loop and makes the writes the screen asks
//! for, through the same `note.*` calls `tasqx note` makes.

use super::*;

use tui::notes::{Filter, Note, Op, Outcome};

/// Every note of the project the screen was opened on (or of the store),
/// open and archived alike: the screen filters by status itself, so `a`
/// can bring a note back in the same session.
fn read_notes(be: &mut Backend, project: Option<&str>) -> Result<Vec<Note>, ApiError> {
    let mut params = json!({ "status": "all" });
    if let Some(p) = project {
        params["project"] = json!(p);
    }
    let page = be.call("note.list", &params)?;
    let now = crate::clock::now();
    Ok(page["notes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|n| {
            let added = n["created"]
                .as_str()
                .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                .map_or_else(String::new, |t| render::day_ago(t, now));
            Note::new(n, added)
        })
        .collect())
}

/// Browse notes on a full screen until the reader leaves.
pub(crate) fn run_notes_screen(
    be: &mut Backend,
    ctx: &Ctx,
    filter: Filter,
) -> Result<(), ApiError> {
    let project = filter.project.clone();
    let mut app = tui::notes::App::new(
        read_notes(be, project.as_deref())?,
        filter,
        ctx.caps.unicode,
    );
    let mut failed = None;
    tui::with_terminal(|term| {
        use ratatui::crossterm::event::{self, Event};
        loop {
            let size = term.size()?;
            app.observe(size.width, size.height);
            term.draw(|f| tui::notes::render(&app, &ctx.theme, &ctx.caps, f))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            match app.on_key(key) {
                Outcome::Quit => return Ok(()),
                Outcome::Stay => {}
                Outcome::Do(op) => {
                    // A refusal is the reader's to see, never the end of the
                    // session; only a list that cannot be read again ends it.
                    let said = apply(be, &op).map_err(|e| e.message);
                    match read_notes(be, project.as_deref()) {
                        Ok(notes) => app.reload(notes),
                        Err(e) => {
                            failed = Some(e);
                            return Ok(());
                        }
                    }
                    app.said(said);
                }
            }
        }
    })
    .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;
    failed.map_or(Ok(()), Err)
}

/// One write, and the line that says what it did.
fn apply(be: &mut Backend, op: &Op) -> Result<String, ApiError> {
    match op {
        Op::Status {
            id,
            short_id,
            rev,
            to,
        } => {
            be.call(
                "note.update",
                &json!({ "id": id, "status": to, "expected_rev": rev }),
            )?;
            let verb = if *to == "open" {
                "Unarchived"
            } else {
                "Archived"
            };
            Ok(format!("{verb} note #{short_id}"))
        }
        Op::Promote {
            id,
            short_id,
            rev,
            to,
        } => {
            let out = be.call(
                "note.promote",
                &json!({ "id": id, "to": to, "expected_rev": rev }),
            )?;
            Ok(match out.get("task") {
                Some(t) => format!(
                    "Promoted note #{short_id} to task #{} and archived it",
                    t["short_id"]
                ),
                None => format!(
                    "Promoted note #{short_id} to the memory doc {:?} and archived it",
                    render::san(out["doc"]["title"].as_str().unwrap_or(""))
                ),
            })
        }
        Op::Remove { id, short_id, rev } => {
            // `note.remove` takes no rev, so the one read is compared here:
            // a note edited since the screen read it is not removed unseen.
            let held = be.call("note.get", &json!({ "id": id }))?;
            if held["_rev"].as_i64() != Some(*rev) {
                return Err(ApiError::conflict(format!(
                    "note #{short_id} changed since this screen read it, so it was not removed; \
                     it is shown as it is now"
                )));
            }
            be.call("note.remove", &json!({ "id": id }))?;
            Ok(format!("Removed note #{short_id}"))
        }
    }
}
