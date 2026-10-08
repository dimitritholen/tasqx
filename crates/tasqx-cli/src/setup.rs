//! `tasqx setup` (DESIGN.md D159, amended by D158's successor for #696): the
//! Claude Code, Codex CLI and Gemini CLI integrations, installed from what
//! this binary carries.
//!
//! Three items per tool. `mcp` is the user-level MCP registration, read out of
//! the tool's own config file and written only through that tool's
//! `mcp add` — each tool rewrites its config while it runs, so a second writer
//! would race it. The two skills are the repository's own `SKILL.md` files,
//! compiled in, so the skills a user installs are the ones that match the
//! tasqx they run. Claude Code is always listed; Codex and Gemini only when
//! their binary is on `PATH`.
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

/// The server every tool is told to run, after its own `tasqx` command.
const SERVE: [&str; 4] = ["mcp", "serve", "--scope", "write"];

/// Where a tool keeps its user-level MCP servers.
#[derive(Clone, Copy)]
pub enum Config {
    /// `mcpServers` in a JSON file.
    Json(&'static str),
    /// `mcp_servers` in a TOML file.
    Toml(&'static str),
}

/// One agent tool setup can wire tasqx into.
///
/// Argv and paths below were checked against the real CLIs on 2026-10-08, not
/// from memory (see DESIGN.md, the D158 amendment):
/// - claude: `claude mcp add --help`; config `~/.claude.json`, skills `~/.claude/skills`.
/// - codex 0.154.0: `codex mcp add --help` reads `add [OPTIONS] <NAME> (--url <URL> | -- <COMMAND>...)`
///   and has no scope flag (user-level only); `HOME=<tmp> codex mcp add tasqx -- tasqx mcp
///   serve --scope write` wrote `[mcp_servers.tasqx]` with `command`/`args` to
///   `$HOME/.codex/config.toml`; skills in `$CODEX_HOME/skills` (`~/.codex/skills`), per
///   the skill-creator text inside the binary.
/// - gemini 0.63.0 (`npx -y @google/gemini-cli@latest mcp add --help`): `-s, --scope user|project`
///   with default `project`; `HOME=<tmp> gemini mcp add --scope user tasqx tasqx -- mcp serve
///   --scope write` wrote `mcpServers.tasqx` to `$HOME/.gemini/settings.json`. Without the
///   `--` its parser swallows `--scope write` as its own flag and adds nothing. Skills:
///   `~/.gemini/skills/<name>/SKILL.md` (its `docs/cli/skills.md`).
pub struct Tool {
    /// The prefix on `--only` names (`codex:mcp`); Claude's are bare.
    pub id: &'static str,
    pub label: &'static str,
    /// The command looked up on `PATH` and run to register.
    pub bin: &'static str,
    /// The tool's directory under the home; its skills go in `<dir>/skills`.
    pub dir: &'static str,
    pub config: Config,
    pub mcp_add: &'static [&'static str],
    pub mcp_remove: &'static [&'static str],
}

const CLAUDE: Tool = Tool {
    id: "claude",
    label: "Claude Code",
    bin: "claude",
    dir: ".claude",
    config: Config::Json(".claude.json"),
    mcp_add: &[
        "mcp", "add", "--scope", "user", "tasqx", "--", "tasqx", "mcp", "serve", "--scope", "write",
    ],
    mcp_remove: &["mcp", "remove", "--scope", "user", "tasqx"],
};

const CODEX: Tool = Tool {
    id: "codex",
    label: "Codex",
    bin: "codex",
    dir: ".codex",
    config: Config::Toml(".codex/config.toml"),
    mcp_add: &[
        "mcp", "add", "tasqx", "--", "tasqx", "mcp", "serve", "--scope", "write",
    ],
    mcp_remove: &["mcp", "remove", "tasqx"],
};

const GEMINI: Tool = Tool {
    id: "gemini",
    label: "Gemini",
    bin: "gemini",
    dir: ".gemini",
    config: Config::Json(".gemini/settings.json"),
    mcp_add: &[
        "mcp", "add", "--scope", "user", "tasqx", "tasqx", "--", "mcp", "serve", "--scope", "write",
    ],
    mcp_remove: &["mcp", "remove", "--scope", "user", "tasqx"],
};

impl Tool {
    /// Claude Code is always offered (its skills need no binary, and without
    /// one the mcp item prints the command); the others only when found.
    fn offered(&self) -> bool {
        self.id == "claude" || on_path(self.bin)
    }
}

fn on_path(bin: &str) -> bool {
    let name = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(&name).is_file()))
}

/// The nudge `run` prints when `ripwire` is not on `PATH`, and the same words
/// `tasqx setup --help` shows for the same case (D178, `cmddoc.rs`), so the
/// two cannot drift apart. Names the upstream repository — ripwire ships no
/// Homebrew formula and no Scoop manifest tasqx could depend on — but never a
/// package name tasqx would have to keep current, and runs no download.
pub(crate) const RIPWIRE_INSTALL_HINT: &str =
    "ripwire not found on PATH: install it from https://github.com/redhat-et/ripwire and put \
     it on PATH.";

/// One thing setup can install.
pub struct Item {
    /// What `--only` takes: bare for Claude Code, `codex:mcp` for the rest.
    pub name: &'static str,
    /// `mcp`, `tasqx-workflow` or `retro`: the skill's directory name.
    pub base: &'static str,
    pub tool: &'static Tool,
    /// A few words on what it is.
    pub what: &'static str,
    /// The bundled `SKILL.md`, or `None` for the MCP registration.
    pub skill: Option<&'static str>,
}

const WORKFLOW: &str = include_str!("../../../.claude/skills/tasqx-workflow/SKILL.md");
const RETRO: &str = include_str!("../../../.claude/skills/retro/SKILL.md");

macro_rules! items {
    ($(($tool:expr, $p:literal)),*) => {[$(
        Item { name: concat!($p, "mcp"), base: "mcp", tool: &$tool,
               what: "MCP server, write access", skill: None },
        Item { name: concat!($p, "tasqx-workflow"), base: "tasqx-workflow", tool: &$tool,
               what: "search, track, annotate", skill: Some(WORKFLOW) },
        Item { name: concat!($p, "retro"), base: "retro", tool: &$tool,
               what: "end-of-session lessons", skill: Some(RETRO) },
    )*]};
}

pub const ITEMS: [Item; 9] = items!((CLAUDE, ""), (CODEX, "codex:"), (GEMINI, "gemini:"));

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    NotInstalled,
    /// The MCP registration runs `tasqx mcp serve --scope write`.
    Installed,
    /// A skill byte-equal to the bundled one.
    Current,
    /// A skill that is not the bundled one, or a registration that runs
    /// something other than the write-scope server.
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

fn skill_path(home: &Path, item: &Item) -> PathBuf {
    home.join(item.tool.dir)
        .join("skills")
        .join(item.base)
        .join("SKILL.md")
}

/// The `tasqx` entry among the tool's user-level MCP servers, whatever the
/// file format.
fn mcp_entry(tool: &Tool, home: &Path) -> Option<Value> {
    match tool.config {
        Config::Json(f) => {
            serde_json::from_str::<Value>(&std::fs::read_to_string(home.join(f)).ok()?)
                .ok()?
                .get("mcpServers")?
                .get("tasqx")
                .cloned()
        }
        Config::Toml(f) => {
            let t: toml::Table = std::fs::read_to_string(home.join(f)).ok()?.parse().ok()?;
            serde_json::to_value(t.get("mcp_servers")?.get("tasqx")?).ok()
        }
    }
}

pub fn status(item: &Item, home: &Path) -> Status {
    match item.skill {
        None => {
            match mcp_entry(item.tool, home) {
                None => Status::NotInstalled,
                Some(e) if is_ours(&e) => Status::Installed,
                // A read-only or hand-made registration is kept, not trusted.
                Some(_) => Status::Differs,
            }
        }
        Some(body) => match std::fs::read(skill_path(home, item)) {
            Ok(bytes) if bytes == body.as_bytes() => Status::Current,
            Ok(_) => Status::Differs,
            // A dangling symlink is the user's own link, not an absent file.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && !std::fs::symlink_metadata(skill_path(home, item))
                        .is_ok_and(|m| m.file_type().is_symlink()) =>
            {
                Status::NotInstalled
            }
            // Unreadable is not absent: never overwrite what cannot be compared.
            Err(_) => Status::Differs,
        },
    }
}

/// Whether a `tasqx` MCP entry is the one `mcp_add` writes: a `tasqx` command
/// (bare or by path) with the write-scope `mcp serve` arguments.
fn is_ours(entry: &Value) -> bool {
    entry.get("args") == Some(&json!(SERVE))
        && entry
            .get("command")
            .and_then(Value::as_str)
            .and_then(|c| Path::new(c).file_stem())
            .is_some_and(|s| s == "tasqx")
}

/// Runs `<tool> <argv>` with its home set to setup's, so `--home` reaches the
/// profile the tool writes as well as the one setup reads.
fn cli(tool: &Tool, home: &Path, argv: &[&str]) -> Result<(), CliError> {
    let mut c = std::process::Command::new(tool.bin);
    c.env("HOME", home);
    // Codex puts its config under $CODEX_HOME when set, which would split what
    // setup reads (`<home>/.codex`) from where `codex mcp add` writes.
    c.env_remove("CODEX_HOME");
    #[cfg(windows)]
    c.env("USERPROFILE", home);
    match c.args(argv).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(CliError::Failed(format!(
            "failed: `{} {} {}` exited {}: {}",
            tool.bin,
            argv[0],
            argv[1],
            out.status
                .code()
                .map_or("on a signal".into(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(CliError::Missing),
        Err(e) => Err(CliError::Failed(format!(
            "failed: cannot run `{}`: {e}",
            tool.bin
        ))),
    }
}

enum CliError {
    /// The tool's binary is not on PATH.
    Missing,
    /// It ran and failed, or could not be started; the result line.
    Failed(String),
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
        (None, Status::Differs) if !force => ok(
            "kept, does not run `tasqx mcp serve --scope write` — rerun with --force to replace it",
        ),
        (Some(body), _) => {
            let path = skill_path(home, item);
            match crate::verbs::write_atomically(&path, body) {
                Ok(()) if st == Status::Differs => ok("updated"),
                Ok(()) => ok("installed"),
                Err(e) => Outcome {
                    said: format!("failed: {}", e.message),
                    failed: true,
                },
            }
        }
        (None, st) => register_mcp(item.tool, home, st == Status::Differs),
    }
}

// ponytail: `Command::new("claude")` finds `claude` and `claude.exe` but not an
// npm `claude.cmd` shim on Windows; that user gets the printed command.
/// `<tool> mcp add`, after `<tool> mcp remove` when `replace`; a failed remove
/// adds nothing.
fn register_mcp(tool: &Tool, home: &Path, replace: bool) -> Outcome {
    let steps: &[&[&str]] = if replace {
        &[tool.mcp_remove, tool.mcp_add]
    } else {
        &[tool.mcp_add]
    };
    for argv in steps {
        match cli(tool, home, argv) {
            Ok(()) => {}
            Err(CliError::Failed(said)) => return Outcome { said, failed: true },
            Err(CliError::Missing) => {
                let run: Vec<String> = steps
                    .iter()
                    .map(|a| format!("`{} {}`", tool.bin, a.join(" ")))
                    .collect();
                return Outcome {
                    said: format!(
                        "skipped: {} CLI not found — run {}",
                        tool.label,
                        run.join(", then ")
                    ),
                    failed: false,
                };
            }
        }
    }
    Outcome {
        said: if replace { "updated" } else { "installed" }.to_string(),
        failed: false,
    }
}

/// The items `--only` names (`claude:` is optional), in `ITEMS` order, among
/// the tools on offer; all of those when it names none.
fn select(only: &[String]) -> Result<Vec<&'static Item>, ApiError> {
    let offered: Vec<&'static Item> = ITEMS.iter().filter(|i| i.tool.offered()).collect();
    let bare = |n: &str| n.strip_prefix("claude:").unwrap_or(n).to_string();
    for n in only {
        let n = bare(n);
        match ITEMS.iter().find(|i| i.name == n) {
            None => {
                let valid: Vec<&str> = offered.iter().map(|i| i.name).collect();
                return Err(ApiError::bad_request(format!(
                    "unknown setup item {n:?}; the items are {}",
                    valid.join(", ")
                )));
            }
            Some(i) if !i.tool.offered() => {
                return Err(ApiError::bad_request(format!(
                    "{n:?} needs `{}` on PATH, and it is not",
                    i.tool.bin
                )));
            }
            Some(_) => {}
        }
    }
    Ok(offered
        .into_iter()
        .filter(|i| only.is_empty() || only.iter().any(|n| bare(n) == i.name))
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

/// The tools on offer: `TOOL  CLI  UNDER`, one row each.
fn tools_table(ctx: &Ctx, home: &Path, items: &[&Item]) -> String {
    let mut tools: Vec<&Tool> = Vec::new();
    for t in items.iter().map(|i| i.tool) {
        if !tools.iter().any(|x| x.id == t.id) {
            tools.push(t);
        }
    }
    let w = tools
        .iter()
        .map(|t| render::width(t.label))
        .max()
        .unwrap_or(0)
        .max(4)
        + 2;
    let cw = render::width("not on PATH") + 2;
    let mut out = ctx.paint(
        "table.label",
        &format!("{}{}UNDER", render::pad("TOOL", w), render::pad("CLI", cw)),
    );
    out.push('\n');
    for t in tools {
        out.push_str(&render::pad(t.label, w));
        out.push_str(&render::pad(
            if on_path(t.bin) {
                "on PATH"
            } else {
                "not on PATH"
            },
            cw,
        ));
        out.push_str(&ctx.paint("muted", &home.join(t.dir).display().to_string()));
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
    let mut out = tools_table(ctx, home, items);
    out.push('\n');
    out.push_str(&ctx.paint(
        "table.label",
        &format!(
            "{}{}WHAT",
            render::pad("ITEM", name_w),
            render::pad("STATUS", status_w)
        ),
    ));
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
    let mut tools: Vec<Value> = Vec::new();
    for t in items.iter().map(|i| i.tool) {
        if !tools.iter().any(|v| v["id"] == t.id) {
            tools.push(
                json!({ "id": t.id, "label": t.label, "cli": t.bin, "found": on_path(t.bin) }),
            );
        }
    }
    let json = json!({
        "home": home.display().to_string(),
        "tools": tools,
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
    let (json, mut text) = if a.yes {
        let chosen: Vec<(&Item, bool)> = items.iter().map(|i| (*i, a.force)).collect();
        apply(ctx, &home, &chosen)?
    } else if a.list || a.json || !tui::is_interactive(&ctx.caps) {
        list(ctx, &home, &items)
    } else {
        screen(ctx, &home, &items)?
    };
    // D178: tasqx never fetches ripwire, only says once whether it is there;
    // silent when found, so this never grows into a nag on every run.
    if !tasqx_core::mcp::ripwire_on_path() {
        text.push('\n');
        text.push_str(&ctx.paint("muted", RIPWIRE_INSTALL_HINT));
        text.push('\n');
    }
    Ok((json, text))
}

fn screen(ctx: &Ctx, home: &Path, items: &[&'static Item]) -> crate::CmdOutcome {
    let real_home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    let at = if real_home.as_deref() == Some(home) {
        "~".to_string()
    } else {
        home.display().to_string()
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
