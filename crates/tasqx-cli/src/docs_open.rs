//! `tasqx docs` and `tasqx manual`: writing the rendered guide somewhere a
//! browser can open, finding a browser without a dependency, and printing a
//! themed manual page. The guide's content and drift guards live in `docs`
//! and `manual`; this is the opening-things half.

use super::*;

/// `tasqx docs`: generate the self-contained guide, then (usually) open it.
///
/// Never fails for the absence of a browser. Writing the file is the job;
/// launching a viewer is a convenience layered on top, so every failure below the
/// write degrades to a printed path and exit 0. That is what makes this command
/// safe to run in CI without a flag — the headless path is the default path with
/// one fewer step, not a separate mode.
pub(crate) fn run_docs(
    out: Option<&str>,
    no_open: bool,
    to_stdout: bool,
    screen: Option<&str>,
) -> CmdOutcome {
    // `--screen` asks for one captured screen rather than the guide, so it
    // answers before `generate()` runs: the page it writes shares the site's
    // stylesheet but none of its content, and building the whole guide first
    // would be work nobody asked for.
    if let Some(name) = screen {
        return run_docs_screen(name, out);
    }

    let doc = docs::generate();

    if to_stdout {
        // The human rendering is the guide itself; `emit` in the terminal is what
        // keeps `tasqx docs --stdout | head` from panicking on a closed pipe.
        // Under `--json` the same bytes travel as a string, so a script gets the
        // guide without having to distinguish this mode from the others.
        return Ok((
            json!({ "path": Value::Null, "opened": false, "bytes": doc.len(), "html": doc }),
            doc,
        ));
    }

    // An explicit --out means "give me the file"; opening a browser onto a path
    // the user chose (and may be about to commit, or serve) would be presumptuous.
    let explicit = out.is_some();
    let path = match out {
        Some(p) => PathBuf::from(p),
        // No home dir means no private place to put it. Say so and name the way
        // out rather than silently writing into a shared directory.
        None => docs_default_path().ok_or_else(|| {
            ApiError::internal(
                "cannot determine a cache directory for the guide — write it somewhere explicit \
                 with `tasqx docs --out PATH`",
            )
        })?,
    };

    write_page(&path, &doc);

    // The machine-relevant facts are the same in all three branches — where the
    // guide is, and whether a viewer was launched — so they are one shape, and
    // only the sentence differs.
    let result = |opened: bool| json!({ "path": path.to_string_lossy(), "opened": opened, "bytes": doc.len() });

    if explicit || no_open {
        return Ok((
            result(false),
            format!("Wrote the tasqx user guide → {}\n", path.display()),
        ));
    }

    match open_in_browser(&path) {
        Ok(()) => Ok((
            result(true),
            format!("Opened the tasqx user guide → {}\n", path.display()),
        )),
        Err(e) => {
            // The whole point: no browser is not an error. Say what happened, say
            // where the file is, and exit 0 so a CI step never goes red over it.
            eprintln!("note: could not open a browser ({e})");
            Ok((
                result(false),
                format!("The tasqx user guide is at → {}\n", path.display()),
            ))
        }
    }
}

/// `tasqx docs --screen <name>`: one captured screen as a standalone page.
///
/// The README's three pictures are raster because GitHub's markdown cannot
/// carry a styled span, and this is where they come from — the same
/// `ansi_html` renderer the site uses, wrapped in the site's own terminal
/// styling, so a picture and the page agree by construction rather than by a
/// second toolchain's approximation (`scripts/snap.sh` rasterises the result).
///
/// No store, no clock, no network: the screens are `include_str!`ed fixtures.
/// An unknown name is the caller's mistake, not a missing file, so it is a
/// `bad_request` (exit 2) that lists every name there is.
fn run_docs_screen(name: &str, out: Option<&str>) -> CmdOutcome {
    let page = docs::screen_page(name).ok_or_else(|| {
        ApiError::bad_request(format!(
            "no captured screen named `{name}` — the captured screens are: {}",
            crate::fixtures::names().join(", ")
        ))
    })?;

    let Some(path) = out.map(PathBuf::from) else {
        // Same shape as `--stdout` for the guide: the page is the human
        // rendering, and under `--json` the same bytes travel as `html`.
        return Ok((
            json!({ "path": Value::Null, "screen": name, "bytes": page.len(), "html": page }),
            page,
        ));
    };
    write_page(&path, &page);
    Ok((
        json!({ "path": path.to_string_lossy(), "screen": name, "bytes": page.len() }),
        format!("Wrote the {name} screen → {}\n", path.display()),
    ))
}

/// Write one generated page, creating the directory it names.
///
/// A failed write is a real error rather than a degraded success: unlike a
/// missing browser, it leaves no deliverable at all. Exits 1 on the spot
/// because there is nothing for the caller to report — the guide and the
/// screen page both want exactly this, which is why it is not written twice.
fn write_page(path: &std::path::Path, bytes: &str) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("error: cannot create {}: {e}", parent.display());
                exit(1);
            }
        }
    }
    if let Err(e) = std::fs::write(path, bytes) {
        eprintln!("error: cannot write {}: {e}", path.display());
        exit(1);
    }
}

/// Where a browser-bound guide gets written: the user's own cache directory.
/// Stable per version, so re-running `tasqx docs` reuses the one path rather
/// than piling up a file per invocation.
///
/// Deliberately NOT `$TMPDIR/tasqx-docs/tasqx-guide-<ver>.html`, which is what
/// this was. That name is fully predictable inside a world-writable directory:
/// another local account can pre-create `tasqx-docs/` as its own non-sticky
/// directory — so the kernel's `fs.protected_symlinks`, which only guards
/// sticky world-writable dirs, never engages — holding a symlink at the guide's
/// name. Neither `create_dir_all` (happy with a directory it does not own) nor
/// `fs::write` (follows symlinks, no `O_NOFOLLOW`, no `create_new`) refuses
/// that, so the victim's next `tasqx docs` truncates whatever the link names.
/// The cheap variant is the same directory at mode 0755, which wedges every
/// other user's `tasqx docs` on EACCES. The cache dir lives under the home of
/// the single user running the command, which removes the shared-directory
/// exposure outright instead of patching around it with a `create_new` dance,
/// and keeps the path stable and browser-openable (D15: the file is the
/// deliverable, the browser is a courtesy).
///
/// `None` only when no home directory can be determined at all. The caller
/// turns that into an error pointing at `--out`; falling back to the temp dir
/// would reinstate exactly the hole this closes.
pub(crate) fn docs_default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "tasqx", "tasqx").map(|dirs| {
        dirs.cache_dir()
            .join(format!("tasqx-guide-{}.html", env!("CARGO_PKG_VERSION")))
    })
}

/// The platform's browser launchers, in preference order, for `path`.
///
/// Split out from [`spawn_first`] so the degrade path is testable: a test can
/// hand `spawn_first` a launcher that certainly does not exist and assert we
/// report Err rather than panicking or hanging.
pub(crate) fn browser_candidates(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let p = path.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd builtin, not an exe. The empty "" is the window title —
        // without it, cmd reads a quoted path AS the title and opens nothing.
        vec![(
            "cmd".to_string(),
            vec!["/C".into(), "start".into(), String::new(), p],
        )]
    }

    #[cfg(target_os = "macos")]
    {
        vec![("open".to_string(), vec![p])]
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // xdg-open is the standard; the rest cover desktops that lack it. A
        // headless box has none of them — exactly the degrade-to-a-path case.
        vec![
            ("xdg-open".to_string(), vec![p.clone()]),
            ("gio".to_string(), vec!["open".into(), p.clone()]),
            ("wslview".to_string(), vec![p.clone()]),
            ("x-www-browser".to_string(), vec![p.clone()]),
            ("www-browser".to_string(), vec![p]),
        ]
    }
}

/// Spawn the first launcher that starts. Fire-and-forget: we deliberately do NOT
/// wait, because on Linux `xdg-open` can block for as long as the browser lives
/// and `tasqx docs` must return to the prompt.
///
/// Shelling out rather than taking a dependency: one command per platform, and
/// the caller already handles the failure.
pub(crate) fn spawn_first(candidates: &[(String, Vec<String>)]) -> Result<(), String> {
    use std::process::{Command as Proc, Stdio};

    let mut last = String::from("no launcher available");
    for (bin, args) in candidates {
        match Proc::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_child) => return Ok(()),
            Err(e) => last = format!("{bin}: {e}"),
        }
    }
    Err(last)
}

/// Hand a local file to the platform's default browser.
pub(crate) fn open_in_browser(path: &std::path::Path) -> Result<(), String> {
    spawn_first(&browser_candidates(path))
}

/// `tasqx manual` — print a themed guide section (or the TOC). No store, no net.
pub(crate) fn run_manual(ctx: &Ctx, topic: Option<&str>) {
    match manual::render(ctx, topic) {
        Ok(page) => emit(&format!("{page}\n")),
        Err(msg) => {
            eprintln!("{msg}");
            exit(ErrorCode::BadRequest.exit_code()); // exit 2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page `scripts/snap.sh` rasterises has to be a *screenshot* of a
    /// terminal, not a page with a screen on it: a header, a scrollbar or a
    /// light background would all end up in the README's picture. And the
    /// styling has to be the site's own consts, because a second copy of the
    /// palette is a picture that drifts from the page it is meant to mirror.
    #[test]
    fn a_screen_page_is_a_bare_dark_terminal_sized_to_its_content() {
        let page = docs::screen_page("list").expect("`list` is a captured screen");

        assert!(page.starts_with("<!doctype html>"), "not a whole document");
        assert!(page.contains("<meta charset=\"utf-8\">"), "no charset");
        assert!(
            page.contains("<pre class=\"term\">"),
            "the screen is not rendered by ansi_html"
        );
        // The dark palette, from DARK_VARS — a terminal is dark in both site
        // themes, and the picture has no switch to offer.
        assert!(
            page.contains("--term-bg: #1b1e24;"),
            "the dark terminal token is missing, so the page is not DARK_VARS"
        );
        assert!(
            !page.contains("color-scheme: light;"),
            "the light palette reached a picture of a terminal"
        );
        // RULES, reused rather than copied: `pre.term`'s own rule is the one
        // line that proves the site's stylesheet is what styles this.
        assert!(
            page.contains("pre.term { margin: 0;"),
            "the site's pre.term rule is not in the page"
        );
        // …with the three overrides that make it a screenshot.
        assert!(page.contains("width: max-content"), "not content-sized");
        assert!(page.contains("overflow: visible"), "the block can scroll");
        for absent in ["<script", "header class=\"top\"", "id=\"nav", "<aside"] {
            assert!(
                !page.contains(absent),
                "the standalone screen page carries `{absent}` — it must be the \
                 screen and nothing else"
            );
        }
        // Colour survives the trip: the fixture's SGR is inline `color:#…`.
        assert!(
            page.contains("color:#"),
            "the screen rendered colourless, so the picture would be grey"
        );
    }

    /// Every embedded screen renders, because `scripts/snap.sh <name>` accepts
    /// any of them and a name that panicked would do so inside a shell script.
    #[test]
    fn every_captured_screen_has_a_page_and_nothing_else_does() {
        for name in crate::fixtures::names() {
            let page = docs::screen_page(name).unwrap_or_else(|| panic!("no page for {name}"));
            assert!(page.ends_with("</html>\n"), "{name} is a truncated page");
        }
        assert!(docs::screen_page("no-such-screen").is_none());
    }

    /// An unknown name is a typo at a shell prompt, so the refusal has to carry
    /// the way out — exit 2 (`bad_request`) listing the screens there are, not
    /// a panic from `term_screen`'s unwrap or a zero-byte file.
    #[test]
    fn an_unknown_screen_name_exits_two_listing_the_screens() {
        let err = run_docs(None, false, false, Some("dashbored"))
            .expect_err("an unknown screen is refused");
        assert_eq!(err.exit_code(), 2);
        assert!(err.message.contains("dashbored"), "{}", err.message);
        assert!(err.message.contains("dashboard"), "{}", err.message);
        assert!(err.message.contains("list"), "{}", err.message);
    }

    /// `--screen` without `--out` is the stdout path: the human rendering is
    /// the page itself, and `--json` carries the same bytes as `html`.
    #[test]
    fn a_screen_without_out_travels_as_the_page_on_both_surfaces() {
        let (value, text) = run_docs(None, false, false, Some("list")).expect("a captured screen");
        assert_eq!(value["screen"], "list");
        assert!(value["path"].is_null(), "stdout wrote a file");
        assert_eq!(value["html"].as_str(), Some(text.as_str()));
        assert_eq!(value["bytes"].as_u64(), Some(text.len() as u64));
        assert!(text.starts_with("<!doctype html>"));
    }

    /// `--out` writes the page and says where, creating the directory: snap.sh
    /// hands it a path in a temp dir it just made, and a silent zero-byte file
    /// there would be rasterised into a blank PNG.
    #[test]
    fn a_screen_with_out_writes_the_file_and_names_it() {
        let dir = std::env::temp_dir().join(format!("tasqx-screen-test-{}", std::process::id()));
        let path = dir.join("nested").join("list.html");
        let (value, text) = run_docs(Some(&path.to_string_lossy()), false, false, Some("report"))
            .expect("a captured screen");
        let written = std::fs::read_to_string(&path).expect("the page was written");
        assert_eq!(value["screen"], "report");
        assert_eq!(value["bytes"].as_u64(), Some(written.len() as u64));
        assert!(written.contains("<pre class=\"term\">"));
        assert!(text.contains(&path.display().to_string()), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
