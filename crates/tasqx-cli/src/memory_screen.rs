//! The `tasqx memory list` screen driver (D121): the docs out of `memory.list`,
//! the alt-screen loop, and the one read the screen cannot do for itself. The
//! widget lives in `tui::memory`; whether the screen opens at all is decided
//! in `execute`, next to the dashboard's own decision.

use super::*;

/// Browse memory on a full screen until the reader leaves.
///
/// Every doc is listed up front, one `memory.list` page at a time, because the
/// search ranks the whole store and a query cannot rank what has not been
/// read. Bodies are read one at a time, when the cursor reaches a doc, since a
/// store of long notes would otherwise be read in full to show one preview.
pub(crate) fn run_memory_screen(
    be: &mut Backend,
    ctx: &Ctx,
    project: Option<&str>,
) -> Result<(), ApiError> {
    const PAGE: u64 = 200;
    let mut listed: Vec<Value> = Vec::new();
    let mut offset = 0u64;
    loop {
        let mut params = json!({ "offset": offset, "limit": PAGE });
        if let Some(p) = project {
            params["project"] = json!(p);
        }
        let page = be.call("memory.list", &params)?;
        listed.extend(page["docs"].as_array().cloned().unwrap_or_default());
        match page["next_offset"].as_u64() {
            Some(next) if next > offset => offset = next,
            _ => break,
        }
    }

    let now = crate::clock::now();
    let when = |d: &Value, key: &str| {
        d[key]
            .as_str()
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .map_or_else(String::new, |t| render::day_ago(t, now))
    };
    let docs = listed
        .iter()
        .map(|d| {
            tui::memory::Doc::new(
                d["id"].as_str().unwrap_or(""),
                d["title"].as_str().unwrap_or(""),
                d["project"].as_str(),
                d["source"].as_str(),
                when(d, "modified"),
                when(d, "created"),
                d["body_preview"].as_str().unwrap_or(""),
            )
        })
        .collect();

    let mut app = tui::memory::App::new(docs, ctx.caps.unicode);
    tui::with_terminal(|term| {
        use ratatui::crossterm::event::{self, Event};
        loop {
            let size = term.size()?;
            app.observe(size.width, size.height);
            if let Some(id) = app.wanted().map(str::to_string) {
                // A doc that cannot be read says so in its own preview rather
                // than ending the session: the other docs are still readable.
                let body = match be.call("memory.get", &json!({ "id": id })) {
                    Ok(doc) => doc["body"].as_str().unwrap_or("").to_string(),
                    Err(e) => format!("This doc could not be read: {}", e.message),
                };
                app.set_body(&id, &body);
            }
            term.draw(|f| tui::memory::render(&app, &ctx.theme, &ctx.caps, f))?;
            // Resize and paste events just redraw; only keys are decisions.
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if app.on_key(key) == Some(tui::memory::Action::Quit) {
                return Ok(());
            }
        }
    })
    .map_err(|e| ApiError::internal(format!("terminal error: {e}")))
}

/// Whether `memory list` opens the screen (D121), D58's rule for the bare
/// `tasqx`, one verb over: both streams on a terminal, no `--json`, a window
/// big enough to draw in. And no `--limit` or `--offset`: asking for a page
/// is a script's question, and the answer to it is the table.
pub(crate) fn memory_screen_active(
    caps: &Caps,
    json: bool,
    paged: bool,
    size: Option<(u16, u16)>,
    stdout_tty: bool,
    stdin_tty: bool,
) -> bool {
    !json
        && !paged
        && tui::is_interactive_with(caps, stdout_tty, stdin_tty)
        && size.is_some_and(|(w, h)| w >= tui::memory::MIN_WIDTH && h >= tui::memory::MIN_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Caps {
        Caps {
            depth: theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        }
    }

    /// Every condition, each one flipped alone: a screen that opened for a
    /// pipe would hang a script on a key it can never press.
    #[test]
    fn the_screen_opens_only_for_a_person_at_a_terminal() {
        let size = Some((120, 40));
        assert!(memory_screen_active(
            &caps(),
            false,
            false,
            size,
            true,
            true
        ));
        assert!(
            !memory_screen_active(&caps(), true, false, size, true, true),
            "--json"
        );
        assert!(
            !memory_screen_active(&caps(), false, true, size, true, true),
            "--limit"
        );
        assert!(
            !memory_screen_active(&caps(), false, false, size, false, true),
            "piped stdout"
        );
        assert!(
            !memory_screen_active(&caps(), false, false, size, true, false),
            "piped stdin"
        );
        assert!(
            !memory_screen_active(&Caps::PLAIN, false, false, size, true, true),
            "TERM=dumb"
        );
        assert!(
            !memory_screen_active(&caps(), false, false, Some((40, 40)), true, true),
            "narrow"
        );
        assert!(
            !memory_screen_active(&caps(), false, false, Some((120, 8)), true, true),
            "short"
        );
        assert!(
            !memory_screen_active(&caps(), false, false, None, true, true),
            "no size"
        );
    }
}
