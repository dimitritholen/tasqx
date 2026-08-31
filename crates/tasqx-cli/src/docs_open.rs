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
pub(crate) fn run_docs(out: Option<&str>, no_open: bool, to_stdout: bool) -> CmdOutcome {
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

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("error: cannot create {}: {e}", parent.display());
                exit(1);
            }
        }
    }
    // A failed *write* is a real error: there is no deliverable at all.
    if let Err(e) = std::fs::write(&path, &doc) {
        eprintln!("error: cannot write {}: {e}", path.display());
        exit(1);
    }

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
