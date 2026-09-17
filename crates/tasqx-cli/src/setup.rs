//! `tasqx setup` (DESIGN.md D158): the Claude Code integration, installed from
//! what this binary carries.
//!
//! Three items. `mcp` is the user-scope MCP registration, read out of
//! `~/.claude.json` and written only through `claude mcp add` — Claude Code
//! rewrites that file while it runs, so a second writer would race it. The two
//! skills are the repository's own `SKILL.md` files, compiled in, so the skills
//! a user installs are the ones that match the tasqx they run.
//!
//! A skill's state is a byte comparison and nothing more: absent, equal, or
//! different. A different file is either an older bundled copy or the user's
//! own edit, and without a record of what was installed those two cannot be
//! told apart, so both are kept unless `--force` (or a tick on the screen)
//! says to replace them.
// ponytail: no install manifest, so an outdated copy reads as `differs`; a
// hash of each shipped version would tell "old" from "edited" if that matters.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tasqx_core::ApiError;

use crate::render;
use crate::theme::Ctx;
use crate::tui;

/// The argv `claude` is called with, and the command printed when it is absent.
const MCP_ADD: [&str; 11] = [
    "mcp", "add", "--scope", "user", "tasqx", "--", "tasqx", "mcp", "serve", "--scope", "write",
];

/// One thing setup can install.
pub struct Item {
    pub name: &'static str,
    /// The screen's group heading.
    pub group: &'static str,
    /// A few words on what it is.
    pub what: &'static str,
    /// The bundled `SKILL.md`, or `None` for the MCP registration.
    pub skill: Option<&'static str>,
}

pub const ITEMS: [Item; 3] = [
    Item {
        name: "mcp",
        group: "Connection",
        what: "MCP server, write access",
        skill: None,
    },
    Item {
        name: "tasqx-workflow",
        group: "Skills",
        what: "search, track, annotate",
        skill: Some(include_str!(
            "../../../.claude/skills/tasqx-workflow/SKILL.md"
        )),
    },
    Item {
        name: "retro",
        group: "Skills",
        what: "end-of-session lessons",
        skill: Some(include_str!("../../../.claude/skills/retro/SKILL.md")),
    },
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    NotInstalled,
    /// The MCP registration is present (it has no "current" to compare with).
    Installed,
    /// A skill byte-equal to the bundled one.
    Current,
    /// A skill that is there and is not the bundled one.
    Differs,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::NotInstalled => "not installed",
            Status::Installed => "installed",
            Status::Current => "current",
            Status::Differs => "differs",
        }
    }

    /// Whether the reader has something to act on in this state.
    pub fn wants_attention(self) -> bool {
        matches!(self, Status::NotInstalled | Status::Differs)
    }
}

fn skill_path(home: &Path, name: &str) -> PathBuf {
    home.join(".claude")
        .join("skills")
        .join(name)
        .join("SKILL.md")
}

pub fn status(item: &Item, home: &Path) -> Status {
    match item.skill {
        None => {
            let registered = std::fs::read_to_string(home.join(".claude.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_some_and(|v| v.get("mcpServers").and_then(|m| m.get("tasqx")).is_some());
            if registered {
                Status::Installed
            } else {
                Status::NotInstalled
            }
        }
        Some(body) => match std::fs::read(skill_path(home, item.name)) {
            Ok(bytes) if bytes == body.as_bytes() => Status::Current,
            Ok(_) => Status::Differs,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Status::NotInstalled,
            // Unreadable is not absent: never overwrite what cannot be compared.
            Err(_) => Status::Differs,
        },
    }
}

/// `claude` with its home set to setup's, so `--home` reaches the profile
/// Claude Code writes as well as the one setup reads.
fn claude(home: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("claude");
    c.env("HOME", home);
    #[cfg(windows)]
    c.env("USERPROFILE", home);
    c
}

/// What installing one item did, as the word a result line prints.
struct Outcome {
    said: String,
    failed: bool,
}

fn install(item: &Item, home: &Path, force: bool) -> Outcome {
    let ok = |s: &str| Outcome {
        said: s.to_string(),
        failed: false,
    };
    let st = status(item, home);
    match (item.skill, st) {
        (_, Status::Current) => ok("already current"),
        (None, Status::Installed) => ok("already installed"),
        (Some(_), Status::Differs) if !force => {
            ok("kept, differs from the bundled copy — rerun with --force to replace it")
        }
        (Some(body), _) => {
            let path = skill_path(home, item.name);
            match crate::complete::install::write_atomically(&path, body) {
                Ok(()) if st == Status::Differs => ok("updated"),
                Ok(()) => ok("installed"),
                Err(e) => Outcome {
                    said: format!("failed: {}", e.message),
                    failed: true,
                },
            }
        }
        (None, _) => register_mcp(home),
    }
}

// ponytail: `Command::new("claude")` finds `claude` and `claude.exe` but not an
// npm `claude.cmd` shim on Windows; that user gets the printed command.
fn register_mcp(home: &Path) -> Outcome {
    match claude(home).args(MCP_ADD).output() {
        Ok(out) if out.status.success() => Outcome {
            said: "installed".to_string(),
            failed: false,
        },
        Ok(out) => Outcome {
            said: format!(
                "failed: `claude mcp add` exited {}: {}",
                out.status
                    .code()
                    .map_or("on a signal".into(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            failed: true,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Outcome {
            said: format!(
                "skipped: Claude Code CLI not found — run `claude {}`",
                MCP_ADD.join(" ")
            ),
            failed: false,
        },
        Err(e) => Outcome {
            said: format!("failed: cannot run `claude`: {e}"),
            failed: true,
        },
    }
}

/// The items `--only` names, in `ITEMS` order; all of them when it names none.
fn select(only: &[String]) -> Result<Vec<&'static Item>, ApiError> {
    if let Some(bad) = only.iter().find(|n| !ITEMS.iter().any(|i| i.name == *n)) {
        let valid: Vec<&str> = ITEMS.iter().map(|i| i.name).collect();
        return Err(ApiError::bad_request(format!(
            "unknown setup item {bad:?}; the items are {}",
            valid.join(", ")
        )));
    }
    Ok(ITEMS
        .iter()
        .filter(|i| only.is_empty() || only.iter().any(|n| n == i.name))
        .collect())
}

fn home_dir(flag: Option<&Path>) -> Result<PathBuf, ApiError> {
    match flag {
        Some(p) => Ok(p.to_path_buf()),
        None => directories::BaseDirs::new()
            .map(|d| d.home_dir().to_path_buf())
            .ok_or_else(|| {
                ApiError::bad_request("cannot determine your home directory; pass --home <DIR>")
            }),
    }
}

/// `name  said` rows, the name column fitted to the widest name shown.
fn rows(ctx: &Ctx, cells: &[(&str, String, bool)]) -> String {
    let w = cells
        .iter()
        .map(|(n, ..)| render::width(n))
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = String::new();
    for (name, said, loud) in cells {
        out.push_str(&render::pad(name, w));
        out.push_str(&ctx.paint(if *loud { "warn" } else { "muted" }, said));
        out.push('\n');
    }
    out
}

fn list(ctx: &Ctx, home: &Path, items: &[&Item]) -> (Value, String) {
    let found: Vec<(&Item, Status)> = items.iter().map(|i| (*i, status(i, home))).collect();
    let name_w = found
        .iter()
        .map(|(i, _)| render::width(i.name))
        .max()
        .unwrap_or(0)
        + 2;
    let status_w = render::width("not installed") + 2;
    let mut out = ctx.paint(
        "table.label",
        &format!(
            "{}{}WHAT",
            render::pad("ITEM", name_w),
            render::pad("STATUS", status_w)
        ),
    );
    out.push('\n');
    for (item, st) in &found {
        out.push_str(&render::pad(item.name, name_w));
        let label = render::pad(st.label(), status_w);
        out.push_str(&ctx.paint(
            if st.wants_attention() {
                "warn"
            } else {
                "muted"
            },
            &label,
        ));
        out.push_str(&ctx.paint("muted", item.what));
        out.push('\n');
    }
    let json = json!({
        "home": home.display().to_string(),
        "items": found.iter().map(|(i, s)| json!({ "name": i.name, "status": s.label() })).collect::<Vec<_>>(),
    });
    (json, out)
}

fn apply(ctx: &Ctx, home: &Path, chosen: &[(&Item, bool)]) -> crate::CmdOutcome {
    let mut cells = Vec::new();
    let mut doc = Vec::new();
    let mut failed = false;
    for (item, force) in chosen {
        let o = install(item, home, *force);
        failed |= o.failed;
        doc.push(
            json!({ "name": item.name, "outcome": o.said, "status": status(item, home).label() }),
        );
        cells.push((item.name, o.said, o.failed));
    }
    let text = rows(ctx, &cells);
    if failed {
        return Err(ApiError::internal(format!("setup did not finish:\n{text}")));
    }
    Ok((
        json!({ "home": home.display().to_string(), "items": doc }),
        text,
    ))
}

/// The flags, as `execute` hands them over.
pub struct Args<'a> {
    pub list: bool,
    pub yes: bool,
    pub force: bool,
    pub only: &'a [String],
    pub home: Option<&'a Path>,
    pub json: bool,
}

pub fn run(ctx: &Ctx, a: Args) -> crate::CmdOutcome {
    let items = select(a.only)?;
    let home = home_dir(a.home)?;
    if a.yes {
        let chosen: Vec<(&Item, bool)> = items.iter().map(|i| (*i, a.force)).collect();
        return apply(ctx, &home, &chosen);
    }
    if a.list || a.json || !tui::is_interactive(&ctx.caps) {
        return Ok(list(ctx, &home, &items));
    }
    screen(ctx, &home, &items)
}

fn screen(ctx: &Ctx, home: &Path, items: &[&'static Item]) -> crate::CmdOutcome {
    let real_home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    let at = if real_home.as_deref() == Some(home) {
        "~/.claude".to_string()
    } else {
        home.join(".claude").display().to_string()
    };
    let rows = items.iter().map(|i| (*i, status(i, home))).collect();
    let mut app = tui::setup::App::new(rows, at);
    let go = tui::with_terminal(|term| {
        use ratatui::crossterm::event::{self, Event};
        loop {
            term.draw(|f| tui::setup::render(&app, &ctx.theme, &ctx.caps, f))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            match app.on_key(key) {
                Some(tui::setup::Action::Install) => return Ok(true),
                Some(tui::setup::Action::Quit) => return Ok(false),
                None => {}
            }
        }
    })
    .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;
    if !go {
        return Ok((json!({ "items": [] }), String::new()));
    }
    // A tick is the consent `--force` gives on the command line.
    let chosen: Vec<(&Item, bool)> = app.ticked().map(|i| (i, true)).collect();
    apply(ctx, home, &chosen)
}
