//! tasqx — the reference CLI client (DESIGN.md §5).
//!
//! Every subcommand is a thin translation to exactly one core API `method`:
//! it builds a params object, calls `tasqx_core::dispatch` in-process, then
//! renders either a human table (default) or the raw envelope `result`
//! (`--json`). Exit codes mirror the §4 error model (0 ok, 2 bad_request,
//! 4 not_found, 5 conflict). The `api` subcommand is the stdio one-shot
//! transport: one JSON envelope in on stdin, one out on stdout.
//!
//! Store location: `$TASQX_DB` if set, else the per-platform data dir via
//! `directories` (e.g. `%APPDATA%\tasqx\tasqx\data\tasks.db` on Windows — the
//! segment repeats because `ProjectDirs::from("dev", "tasqx", "tasqx")` passes
//! `tasqx` as both organization and application).

mod chart;
mod cmddoc;
mod docs;
mod html;
mod render;
mod sugar;
mod theme;

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use tasqx_core::{daemon, datetime, dispatch, handle_envelope, notify, ApiError, Engine, ErrorCode, McpServer, Scope};

use theme::{Caps, Ctx};

/// The real reference instant handed to the natural-language date parser.
fn now_ts() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Fields `modify --clear` may unset (DESIGN.md §12-D13).
///
/// `title` is absent on purpose and clap therefore rejects `--clear title` with
/// the list of what *is* clearable: a task with no title is not a task, and the
/// core would reject the null anyway — better to say so at parse time than to
/// round-trip a `bad_request`. `status` is absent for the same reason it is not
/// a general modify field: lifecycle moves through start/stop/done/cancel so
/// their invariants hold (D6).
const CLEARABLE: [&str; 8] =
    ["project", "priority", "due", "scheduled", "wait", "remind", "recurrence", "estimate"];

#[derive(Parser)]
#[command(
    name = "tasqx",
    version,
    about = "A fast, terminal-first, AI-native task manager.",
    after_help = "Run `tasqx manual` for the full in-terminal guide, or `tasqx <command> -h` for examples.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Print the raw JSON API result instead of the human table.
    #[arg(long, global = true)]
    json: bool,

    /// Theme name (built-in nord|gruvbox|dracula|solarized|mono, or a user file).
    /// Overrides $TASQX_THEME and the config file (DESIGN.md §8).
    #[arg(long, global = true)]
    theme: Option<String>,

    /// Local socket / named-pipe address of a tasqx daemon. Overrides $TASQX_SOCK.
    /// When a daemon is reachable, one-shot commands route through it (single
    /// writer); otherwise they run in-process. Also selects the address for
    /// `daemon` and `watch`.
    #[arg(long, global = true)]
    socket: Option<String>,

    /// Never route through a daemon; always run one-shot in-process (the
    /// pre-daemon behaviour). Escape hatch for scripts.
    #[arg(long, global = true)]
    no_daemon: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create a project (maps to project.create).
    #[command(after_help = crate::cmddoc::after_help("init"))]
    Init {
        /// Project name, e.g. work.tasqx
        name: String,
        /// Optional description.
        #[arg(long)]
        desc: Option<String>,
    },
    /// Add a task (maps to task.add). Supports inline +tag / project: / !prio sugar.
    #[command(alias = "a", alias = "new", after_help = crate::cmddoc::after_help("add"))]
    Add {
        /// The task title (may carry inline sugar).
        title: Vec<String>,
        #[arg(long)]
        project: Option<String>,
        /// Priority: H, M, or L (or high/medium/low).
        #[arg(long, short)]
        priority: Option<String>,
        /// Due date — natural language ok (e.g. friday, "in 3 days", eom, -1d).
        ///
        /// `allow_hyphen_values` for the same reason as `--remind` below: a
        /// signed offset (`--due -1d`, capturing something already overdue) is a
        /// leading-hyphen value, and clap reads those as unknown flags.
        #[arg(long, allow_hyphen_values = true)]
        due: Option<String>,
        /// Scheduled date — natural language ok.
        #[arg(long, allow_hyphen_values = true)]
        scheduled: Option<String>,
        /// Wait-until date — natural language ok (task stays in backlog).
        #[arg(long, allow_hyphen_values = true)]
        wait: Option<String>,
        /// Recurrence rule, e.g. "every 3 days" or "weekly on mon,wed,fri".
        #[arg(long)]
        repeat: Option<String>,
        /// Reminder: a due-anchored offset (-1h, -30m, -2d) or an absolute date
        /// ("friday 9am"). Without this, the task never notifies (§9).
        ///
        /// `allow_hyphen_values` is required, not cosmetic: the common form
        /// starts with `-`, and clap would otherwise read `--remind -1h` as an
        /// unknown `-1` flag and reject the command.
        #[arg(long, allow_hyphen_values = true)]
        remind: Option<String>,
        /// Effort estimate — human duration (4h, 90m, 1h30m, 2d) or ISO PT4H.
        #[arg(long, short = 'e')]
        estimate: Option<String>,
        /// Repeatable tag flag.
        #[arg(long = "tag", short = 't')]
        tags: Vec<String>,
    },
    /// Change a task (maps to task.modify). Takes the same inline sugar and
    /// natural-language dates as `add`; clear a field with `--clear <field>`.
    ///
    /// Setting and clearing are deliberately different shapes (DESIGN.md §12-D13):
    /// a value is `due:friday` / `--due friday`, and removal is only ever
    /// `--clear due`. There is no magic empty value — `--due ""` is a bad date,
    /// not an erasure, so a shell that expands a variable to nothing can never
    /// silently wipe a field it meant to set.
    ///
    ///   tasqx modify 42 due:friday !high est:4h
    ///   tasqx modify 42 --due -1d --remind -30m
    ///   tasqx modify 42 --clear due --clear remind
    ///   tasqx modify 42 repeat:"every monday"     # set a recurrence
    ///   tasqx modify 42 --clear recurrence        # stop it recurring
    #[command(alias = "mod", alias = "m", alias = "edit", after_help = crate::cmddoc::after_help("modify"))]
    Modify {
        /// short_id or UUID.
        r#ref: String,
        /// New title words and/or inline sugar (due:friday, +tag, project:p,
        /// !high, est:4h, repeat:"every week", remind:-1h). Bare words become the
        /// new title; omit them to leave the title alone.
        rest: Vec<String>,
        #[arg(long)]
        project: Option<String>,
        /// Priority: H, M, or L (or high/medium/low).
        #[arg(long, short)]
        priority: Option<String>,
        /// Due date — natural language ok (friday, "in 3 days", eom, -1d).
        #[arg(long, allow_hyphen_values = true)]
        due: Option<String>,
        /// Scheduled date — natural language ok.
        #[arg(long, allow_hyphen_values = true)]
        scheduled: Option<String>,
        /// Wait-until date — natural language ok.
        #[arg(long, allow_hyphen_values = true)]
        wait: Option<String>,
        /// Recurrence rule, e.g. "every 3 days" or "weekly on mon,wed,fri".
        /// Clear it with `--clear recurrence`.
        #[arg(long)]
        repeat: Option<String>,
        /// Reminder: a due-anchored offset (-1h, -30m) or an absolute date.
        ///
        /// `allow_hyphen_values` here for the same reason as on `add`: the common
        /// value starts with `-` and clap would read it as an unknown flag.
        #[arg(long, allow_hyphen_values = true)]
        remind: Option<String>,
        /// Effort estimate — human duration (4h, 90m, 1h30m, 2d) or ISO PT4H.
        #[arg(long, short = 'e')]
        estimate: Option<String>,
        /// Add a tag (repeatable). Routed to tag.add.
        #[arg(long = "tag", short = 't')]
        tags: Vec<String>,
        /// Clear a field back to unset (repeatable). The ONLY removal syntax.
        #[arg(
            long = "clear",
            value_name = "FIELD",
            value_parser = CLEARABLE,
        )]
        clear: Vec<String>,
        /// Optimistic concurrency: fail with `conflict` (exit 5) unless the task
        /// is still at this rev, so a concurrent edit is reported instead of
        /// clobbered. `tasqx show <ref> --json` reports the current `_rev`, and
        /// every successful modify prints the new one.
        #[arg(long, value_name = "REV")]
        expected_rev: Option<i64>,
    },
    /// List tasks (maps to task.list). Bare `tasqx` shows the working set.
    #[command(alias = "ls", alias = "l", after_help = crate::cmddoc::after_help("list"))]
    List {
        /// Filter DSL, e.g. "project:work status:pending +api".
        filter: Vec<String>,
    },
    /// Start a task timer (maps to task.start).
    #[command(alias = "s", after_help = crate::cmddoc::after_help("start"))]
    Start {
        /// short_id or UUID.
        r#ref: String,
        /// Keep other active tasks running (opt out of single-active).
        #[arg(long)]
        keep: bool,
    },
    /// Stop the task timer (maps to task.stop).
    #[command(alias = "st", after_help = crate::cmddoc::after_help("stop"))]
    Stop {
        /// short_id or UUID.
        r#ref: String,
    },
    /// Complete a task (maps to task.done).
    #[command(alias = "d", alias = "x", alias = "complete", after_help = crate::cmddoc::after_help("done"))]
    Done {
        /// short_id or UUID.
        r#ref: String,
    },
    /// Show a task's full detail incl. tags/annotations/deps (maps to task.get).
    #[command(alias = "get", after_help = crate::cmddoc::after_help("show"))]
    Show {
        /// short_id or UUID.
        r#ref: String,
    },
    /// Cancel a task (maps to task.cancel).
    #[command(after_help = crate::cmddoc::after_help("cancel"))]
    Cancel {
        /// short_id or UUID.
        r#ref: String,
    },
    /// Reopen a done/cancelled task (maps to task.reopen).
    #[command(after_help = crate::cmddoc::after_help("reopen"))]
    Reopen {
        /// short_id or UUID.
        r#ref: String,
    },
    /// Annotate a task (maps to annotation.add).
    #[command(alias = "note", after_help = crate::cmddoc::after_help("annotate"))]
    Annotate {
        /// short_id or UUID.
        r#ref: String,
        /// The annotation text.
        text: Vec<String>,
    },
    /// Add a dependency: <ref> depends on <depends_on> (maps to dependency.add).
    #[command(after_help = crate::cmddoc::after_help("dep"))]
    Dep {
        /// The dependent task (short_id or UUID).
        r#ref: String,
        /// The task it depends on (short_id or UUID).
        depends_on: String,
    },
    /// Remove a dependency (maps to dependency.remove).
    #[command(after_help = crate::cmddoc::after_help("undep"))]
    Undep {
        /// The dependent task (short_id or UUID).
        r#ref: String,
        /// The task it depended on (short_id or UUID).
        depends_on: String,
    },
    /// Set the default project — where a bare `tasqx add` lands (maps to
    /// project.use).
    ///
    /// The project must already exist (`tasqx init <name>`) and must not be
    /// archived. `tasqx projects` marks the current default with `*`.
    #[command(after_help = crate::cmddoc::after_help("use"))]
    Use {
        /// An existing, non-archived project name.
        name: String,
    },
    /// List projects (maps to project.list).
    #[command(after_help = crate::cmddoc::after_help("projects"))]
    Projects {
        /// Include archived projects.
        #[arg(long)]
        all: bool,
    },
    /// Grouped summary report (maps to report.summary), or a self-contained
    /// HTML review with `--html` (DESIGN.md §8).
    #[command(after_help = crate::cmddoc::after_help("report"))]
    Report {
        /// Optional group_by (project|status|priority) then optional filter DSL.
        args: Vec<String>,
        /// Emit a single self-contained HTML report (inline CSS + SVG, no
        /// external requests) instead of the terminal table.
        #[arg(long)]
        html: bool,
        /// Write the HTML report to this file (default: stdout).
        #[arg(long)]
        out: Option<String>,
    },
    /// Native terminal charts from the event log (DESIGN.md §8).
    #[command(after_help = crate::cmddoc::after_help("chart"))]
    Chart {
        #[command(subcommand)]
        kind: ChartKind,
    },
    /// Theme tools: list built-ins or preview a theme's roles (DESIGN.md §8).
    #[command(after_help = crate::cmddoc::after_help("theme"))]
    Theme {
        #[command(subcommand)]
        action: ThemeAction,
    },
    /// Export tasks as canonical JSON (maps to store.export).
    #[command(after_help = crate::cmddoc::after_help("export"))]
    Export {
        /// Optional filter DSL.
        filter: Vec<String>,
    },
    /// Import tasks from a file, or `-` for stdin (maps to store.import).
    #[command(after_help = crate::cmddoc::after_help("import"))]
    Import {
        /// Path to a canonical JSON file (array of tasks), or `-` for stdin.
        file: String,
    },
    /// Print the single highest-urgency unblocked task (the "what now" button).
    #[command(after_help = crate::cmddoc::after_help("next"))]
    Next,
    /// Explain a task's urgency breakdown (maps to task.get + the D1 formula).
    #[command(after_help = crate::cmddoc::after_help("why"))]
    Why {
        /// short_id or UUID.
        r#ref: String,
    },
    /// stdio one-shot: read ONE JSON request envelope on stdin, write ONE response.
    #[command(after_help = crate::cmddoc::after_help("api"))]
    Api,
    /// Run the long-lived daemon: bind a local socket / named pipe and serve the
    /// JSON API to many concurrent clients, pushing live change notifications
    /// (DESIGN.md §2). Ctrl-C stops it cleanly.
    #[command(after_help = crate::cmddoc::after_help("daemon"))]
    Daemon {
        /// Store path for the daemon's single Engine (default: $TASQX_DB or the
        /// platform data dir). The socket address comes from the global
        /// `--socket` / $TASQX_SOCK / the platform default.
        #[arg(long)]
        db: Option<String>,
    },
    /// Live view: connect to a daemon, subscribe, and re-render the working set
    /// on every `task.changed` push (DESIGN.md §6a). Needs a running daemon.
    #[command(after_help = crate::cmddoc::after_help("watch"))]
    Watch {
        /// Filter DSL (default: the working set).
        filter: Vec<String>,
    },
    /// Bundled MCP server (DESIGN.md §7, §12-D7).
    #[command(after_help = crate::cmddoc::after_help("mcp"))]
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Open the English user guide in your browser: one self-contained HTML file
    /// (inline CSS + JS, no external requests), generated from this binary.
    ///
    /// Needs no store and no network. The file IS the deliverable and opening it
    /// is a courtesy, so a missing browser is never an error — the launch is
    /// attempted, a failure is reported on stderr with the path, and the command
    /// exits 0. That keeps `docs` headless/CI-safe by default rather than by flag.
    ///
    ///   tasqx docs                    # write a temp file and open it
    ///   tasqx docs --out guide.html   # write it there; never opens a browser
    ///   tasqx docs --no-open          # write the temp file, print the path
    ///   tasqx docs --stdout           # write the HTML to stdout
    #[command(after_help = crate::cmddoc::after_help("docs"))]
    Docs {
        /// Write the guide to this path instead of a temp file. Implies --no-open:
        /// naming an output file is asking for the file, not for a browser.
        #[arg(long, value_name = "PATH")]
        out: Option<String>,
        /// Write the file but never launch a browser. Prints the path instead.
        #[arg(long)]
        no_open: bool,
        /// Write the HTML to stdout instead of a file (pipe it anywhere).
        #[arg(long)]
        stdout: bool,
    },
}

#[derive(Subcommand)]
enum ChartKind {
    /// Tasks added vs done per ISO week (from the events table).
    Throughput {
        /// Weekly buckets (default view; kept for parity with the spec).
        #[arg(long)]
        weekly: bool,
        /// Number of weeks to show (default 12).
        #[arg(long)]
        weeks: Option<usize>,
    },
    /// GitHub-style completion density per day (from done events).
    Heatmap {
        /// Show a full year (52 weeks).
        #[arg(long)]
        year: bool,
        /// Number of weeks to show (default 12; overrides --year).
        #[arg(long)]
        weeks: Option<usize>,
    },
    /// Remaining open tasks over the last N days (from the events table).
    Burndown {
        /// Restrict to a project (else all tasks).
        #[arg(long)]
        project: Option<String>,
        /// Number of days to show (default 30).
        #[arg(long)]
        days: Option<usize>,
    },
}

#[derive(Subcommand)]
enum ThemeAction {
    /// List available themes (built-ins + user files).
    List,
    /// Preview a theme's roles (default: the active theme).
    Show {
        /// Theme name to preview.
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Run the MCP stdio server: newline-delimited JSON-RPC 2.0 on stdin/stdout.
    Serve {
        /// Scoped token from `tasqx mcp token`. Falls back to $TASQX_MCP_TOKEN.
        #[arg(long)]
        token: Option<String>,
    },
    /// Mint and print a scoped token for `serve --token`.
    Token {
        /// Capability scope the token grants.
        #[arg(long, value_parser = ["read", "write"])]
        scope: String,
    },
}

fn main() {
    let cli = Cli::parse();

    // `api`, `mcp`, and `daemon` are special: they frame their own I/O (response
    // envelopes / JSON-RPC / the socket server) and do not go through the normal
    // render path. `daemon` opens its own Engine and blocks.
    match &cli.command {
        Some(Command::Api) => {
            run_api();
            return;
        }
        Some(Command::Mcp { action }) => {
            run_mcp(action);
            return;
        }
        Some(Command::Daemon { db }) => {
            run_daemon(cli.socket.as_deref(), db.as_deref());
            return;
        }
        _ => {}
    }

    // `docs` is pure static content — no store, no theme, no network. Handle it
    // before anything that could fail for reasons the reader is trying to look up.
    if let Some(Command::Docs { out, no_open, stdout }) = &cli.command {
        run_docs(out.as_deref(), *no_open, *stdout);
        return;
    }

    // Build the render context: resolve the active theme (flag > env > config >
    // default) and detect the terminal's real capability (DESIGN.md §8).
    let ctx = build_ctx(cli.theme.as_deref());

    // `theme` needs no store; handle it before opening the engine.
    if let Some(Command::Theme { action }) = &cli.command {
        run_theme(&ctx, action);
        return;
    }

    // `watch` is socket-only: it subscribes to a daemon and re-renders on push.
    if let Some(Command::Watch { filter }) = &cli.command {
        run_watch(cli.socket.as_deref(), cli.no_daemon, filter, &ctx);
        return;
    }

    // Charts and the HTML report are pure local reads that frame their own
    // output; they render straight from a direct Engine (safe under WAL even if
    // a daemon is also running).
    if matches!(
        &cli.command,
        Some(Command::Chart { .. }) | Some(Command::Report { html: true, .. })
    ) {
        let engine = match open_engine() {
            Ok(e) => e,
            Err(msg) => {
                eprintln!("error: {msg}");
                exit(1);
            }
        };
        match cli.command {
            Some(Command::Chart { kind }) => run_chart(&engine, &ctx, kind),
            Some(Command::Report { html: true, out, .. }) => run_html_report(&engine, &ctx, out),
            _ => unreachable!(),
        }
        return;
    }

    // Everything else routes through a reachable daemon (single writer), else
    // falls back to the in-process Engine exactly as before.
    let mut backend = match open_backend(cli.socket.as_deref(), cli.no_daemon) {
        Ok(b) => b,
        Err(msg) => {
            eprintln!("error: {msg}");
            exit(1);
        }
    };

    let outcome = match cli.command {
        None => run_list(&mut backend, &ctx, &[]),
        Some(Command::Init { name, desc }) => run_init(&mut backend, &ctx, name, desc),
        Some(Command::Add { title, project, priority, due, scheduled, wait, repeat, remind, estimate, tags }) => {
            run_add(
                &mut backend,
                &ctx,
                title,
                sugar::AddFlags { project, priority, tags, due, scheduled, wait, repeat, remind, estimate },
            )
        }
        Some(Command::Modify {
            r#ref,
            rest,
            project,
            priority,
            due,
            scheduled,
            wait,
            repeat,
            remind,
            estimate,
            tags,
            clear,
            expected_rev,
        }) => run_modify(
            &mut backend,
            &ctx,
            r#ref,
            rest,
            sugar::AddFlags { project, priority, tags, due, scheduled, wait, repeat, remind, estimate },
            &clear,
            expected_rev,
        ),
        Some(Command::List { filter }) => run_list(&mut backend, &ctx, &filter),
        Some(Command::Start { r#ref, keep }) => run_start(&mut backend, &ctx, r#ref, keep),
        Some(Command::Stop { r#ref }) => run_stop(&mut backend, &ctx, r#ref),
        Some(Command::Done { r#ref }) => run_done(&mut backend, &ctx, r#ref),
        Some(Command::Show { r#ref }) => run_show(&mut backend, &ctx, r#ref),
        Some(Command::Cancel { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.cancel", r#ref),
        Some(Command::Reopen { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.reopen", r#ref),
        Some(Command::Annotate { r#ref, text }) => run_annotate(&mut backend, &ctx, r#ref, text),
        Some(Command::Dep { r#ref, depends_on }) => run_dep(&mut backend, &ctx, "dependency.add", r#ref, depends_on),
        Some(Command::Undep { r#ref, depends_on }) => run_dep(&mut backend, &ctx, "dependency.remove", r#ref, depends_on),
        Some(Command::Use { name }) => run_use(&mut backend, &ctx, name),
        Some(Command::Projects { all }) => run_projects(&mut backend, &ctx, all),
        Some(Command::Report { args, .. }) => run_report(&mut backend, &ctx, args),
        Some(Command::Export { filter }) => run_export(&mut backend, &filter),
        Some(Command::Import { file }) => run_import(&mut backend, file),
        Some(Command::Next) => run_next(&mut backend, &ctx),
        Some(Command::Why { r#ref }) => run_why(&mut backend, &ctx, r#ref),
        Some(Command::Chart { .. }) => unreachable!("handled above"),
        Some(Command::Theme { .. }) => unreachable!("handled above"),
        Some(Command::Docs { .. }) => unreachable!("handled above"),
        Some(Command::Watch { .. }) => unreachable!("handled above"),
        Some(Command::Api) => unreachable!("handled above"),
        Some(Command::Daemon { .. }) => unreachable!("handled above"),
        Some(Command::Mcp { .. }) => unreachable!("handled above"),
    };

    match outcome {
        Ok((result, render)) => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else {
                print!("{render}");
            }
        }
        Err(e) => {
            let code = serde_json::to_value(e.code)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            eprintln!("error [{code}]: {}", e.message);
            exit(e.exit_code());
        }
    }
}

/// Resolve the active theme (flag > $TASQX_THEME > config > default) and detect
/// terminal capability, producing the render context every command shares.
fn build_ctx(flag: Option<&str>) -> Ctx {
    let env = std::env::var("TASQX_THEME").ok();
    let config = config_theme_name();
    let name = theme::resolve_name(flag, env.as_deref(), config.as_deref());
    let dir = themes_dir();
    let theme = theme::load(&name, dir.as_deref());
    Ctx::new(theme, Caps::detect())
}

/// The user's `themes/` directory: `$TASQX_CONFIG_DIR/themes` or the platform
/// config dir. Missing dir is fine — built-ins need no files.
fn themes_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("TASQX_CONFIG_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d).join("themes"));
        }
    }
    directories::ProjectDirs::from("dev", "tasqx", "tasqx")
        .map(|dirs| dirs.config_dir().join("themes"))
}

/// Read the user's `config.toml` (`$TASQX_CONFIG_DIR` or the platform config
/// dir). A missing, unreadable, or invalid file is simply "no config" — config
/// is never load-bearing enough to fail a command over.
fn config_table() -> Option<toml::Table> {
    let base: PathBuf = if let Ok(d) = std::env::var("TASQX_CONFIG_DIR") {
        if d.is_empty() {
            return None;
        }
        PathBuf::from(d)
    } else {
        directories::ProjectDirs::from("dev", "tasqx", "tasqx")?.config_dir().to_path_buf()
    };
    let src = std::fs::read_to_string(base.join("config.toml")).ok()?;
    src.parse::<toml::Table>().ok()
}

/// Read `[theme] name` from `config.toml`, if present.
fn config_theme_name() -> Option<String> {
    let val = config_table()?;
    val.get("theme")
        .and_then(|t| t.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string)
}

/// Read `[notify] enabled` from `config.toml` (DESIGN.md §9).
///
/// Absent means **false**: quiet by default is the whole point, so every failure
/// mode here — no config dir, no file, malformed TOML, wrong type — has to land
/// on "don't notify", never on "notify anyway".
fn config_notify_enabled() -> bool {
    config_table()
        .and_then(|val| {
            val.get("notify")
                .and_then(|n| n.get("enabled"))
                .and_then(toml::Value::as_bool)
        })
        .unwrap_or(false)
}

/// Result of a rendered command: the raw API result (for `--json`) plus the
/// pre-rendered human string.
type CmdOutcome = Result<(Value, String), tasqx_core::ApiError>;

/// Where a one-shot command's dispatch runs: in-process against a local
/// [`Engine`] (default / no daemon), or over the socket against a running
/// daemon (single writer). Both return the identical `result` value, so every
/// `run_*` renders the same regardless of transport.
enum Backend {
    Local(Engine),
    Remote(daemon::Conn),
}

impl Backend {
    /// Route one method+params to the core dispatch, locally or via the daemon.
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, ApiError> {
        match self {
            Backend::Local(engine) => dispatch(engine, method, params),
            Backend::Remote(conn) => {
                let env = conn
                    .request(method, params)
                    .map_err(|e| ApiError::internal(format!("daemon transport error: {e}")))?;
                if env.get("ok") == Some(&Value::Bool(true)) {
                    Ok(env.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    Err(api_error_from_env(&env))
                }
            }
        }
    }
}

/// Reconstruct a typed [`ApiError`] from a daemon error-response envelope, so a
/// routed command yields the same exit code + message as the in-process path.
fn api_error_from_env(env: &Value) -> ApiError {
    let body = env.get("error");
    let code = match body.and_then(|b| b.get("code")).and_then(Value::as_str) {
        Some("not_found") => ErrorCode::NotFound,
        Some("conflict") => ErrorCode::Conflict,
        Some("unsupported_version") => ErrorCode::UnsupportedVersion,
        Some("internal") => ErrorCode::Internal,
        _ => ErrorCode::BadRequest,
    };
    let message = body
        .and_then(|b| b.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("daemon error")
        .to_string();
    let data = body.and_then(|b| b.get("data")).cloned();
    ApiError::new(code, message, data)
}

/// Resolve the socket address: `--socket` > `$TASQX_SOCK` > platform default.
fn resolve_socket(flag: Option<&str>) -> String {
    flag.map(str::to_string)
        .or_else(|| std::env::var("TASQX_SOCK").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(default_socket)
}

/// The stable default socket address (DESIGN.md §2). Documented targets:
///  * Windows: the named pipe `\\.\pipe\tasqx-default`.
///  * Linux:   `$XDG_RUNTIME_DIR/tasqx/tasqx.sock` (falls back to the data dir).
///  * macOS:   `<data dir>/tasqx.sock` (no runtime dir on macOS).
fn default_socket() -> String {
    #[cfg(windows)]
    {
        "tasqx-default".to_string()
    }
    #[cfg(unix)]
    {
        if let Some(dirs) = directories::ProjectDirs::from("dev", "tasqx", "tasqx") {
            if let Some(rt) = dirs.runtime_dir() {
                return rt.join("tasqx.sock").to_string_lossy().into_owned();
            }
            return dirs.data_dir().join("tasqx.sock").to_string_lossy().into_owned();
        }
        "/tmp/tasqx.sock".to_string()
    }
}

/// Build the backend: prefer a reachable daemon (single writer), else fall back
/// to a direct in-process Engine — the pre-daemon behaviour, unchanged. A
/// missing/stale socket falls back immediately (no hang).
fn open_backend(socket_flag: Option<&str>, no_daemon: bool) -> Result<Backend, String> {
    if !no_daemon {
        let target = resolve_socket(socket_flag);
        if let Some(conn) = daemon::try_connect(&target) {
            return Ok(Backend::Remote(conn));
        }
    }
    Ok(Backend::Local(open_engine()?))
}

fn run_init(be: &mut Backend, ctx: &Ctx, name: String, desc: Option<String>) -> CmdOutcome {
    let mut params = json!({ "name": name });
    if let Some(d) = desc {
        params["description"] = Value::String(d);
    }
    let result = be.call("project.create", &params)?;
    let text = render::project_created(ctx, &result);
    Ok((result, text))
}

fn run_add(be: &mut Backend, ctx: &Ctx, title: Vec<String>, flags: sugar::AddFlags) -> CmdOutcome {
    // argv goes in unjoined: the shell's argument boundaries are information the
    // parser needs (see `sugar::parse_add`), and joining destroys them.
    let parsed = sugar::parse_add(&title, flags);

    // Resolve every natural-language date through the ONE core parser, using the
    // real `now` (deterministic in tests, which call the parser directly).
    let now = now_ts();
    let mut params = json!({ "title": parsed.title });
    if let Some(p) = parsed.project {
        params["project"] = Value::String(p);
    }
    if let Some(p) = parsed.priority {
        params["priority"] = Value::String(p);
    }
    if let Some(d) = parsed.due {
        params["due"] = Value::String(datetime::parse_when(&d, now)?);
    }
    if let Some(s) = parsed.scheduled {
        params["scheduled"] = Value::String(datetime::parse_when(&s, now)?);
    }
    if let Some(w) = parsed.wait {
        params["wait"] = Value::String(datetime::parse_when(&w, now)?);
    }
    if let Some(r) = parsed.recurrence {
        params["recurrence"] = Value::String(r);
    }
    // Passed through raw: unlike due/scheduled/wait, a reminder may be a
    // due-anchored offset that must STAY symbolic (so it re-anchors when due
    // moves), so the core — not the CLI — decides offset vs. absolute (§9).
    if let Some(r) = parsed.remind {
        params["remind"] = Value::String(r);
    }
    if let Some(e) = parsed.estimate {
        params["estimate"] = Value::String(datetime::parse_duration(&e)?);
    }
    if !parsed.tags.is_empty() {
        params["tags"] = Value::Array(parsed.tags.into_iter().map(Value::String).collect());
    }
    let result = be.call("task.add", &params)?;
    let text = render::task_added(ctx, &result, &parsed.title);
    Ok((result, text))
}

/// `tasqx modify <ref> [words / sugar] [--flags] [--clear FIELD]…`
///
/// Builds ONE `set` map and issues ONE `task.modify` — every field goes through
/// the same sugar parser and the same core date/duration/recurrence/reminder
/// parsers as `add`, so a token means the same thing in both verbs.
///
/// `+tag` is the one exception to one-verb-one-method: tags do not live in the
/// tasks row and `task.modify` has no `tags` field, so tags are applied with a
/// follow-up `tag.add`. Dropping them silently to preserve the purity of the
/// mapping would be the worse trade — the user typed `+tag` and meant it.
fn run_modify(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    rest: Vec<String>,
    flags: sugar::AddFlags,
    clear: &[String],
    expected_rev: Option<i64>,
) -> CmdOutcome {
    let parsed = sugar::parse_add(&rest, flags);
    let now = now_ts();

    let mut set = serde_json::Map::new();

    // Clearing first, so a field named in BOTH is caught rather than resolved by
    // map-insertion order — "set it and clear it" is a mistake, not a precedence
    // question, and guessing an answer would be the un-forgiving kind of clever.
    for field in clear {
        set.insert(field.clone(), Value::Null);
    }

    // Leftover bare words are the new title. An explicit empty title can't be
    // expressed (and shouldn't be): no words means "leave the title alone".
    if !parsed.title.is_empty() {
        set.insert("title".into(), Value::String(parsed.title.clone()));
    }
    if let Some(p) = parsed.project {
        guard_set_and_clear(&set, "project", &p)?;
        set.insert("project".into(), Value::String(p));
    }
    if let Some(p) = parsed.priority {
        guard_set_and_clear(&set, "priority", &p)?;
        set.insert("priority".into(), Value::String(p));
    }
    if let Some(d) = parsed.due {
        guard_set_and_clear(&set, "due", &d)?;
        set.insert("due".into(), Value::String(datetime::parse_when(&d, now)?));
    }
    if let Some(s) = parsed.scheduled {
        guard_set_and_clear(&set, "scheduled", &s)?;
        set.insert("scheduled".into(), Value::String(datetime::parse_when(&s, now)?));
    }
    if let Some(w) = parsed.wait {
        guard_set_and_clear(&set, "wait", &w)?;
        set.insert("wait".into(), Value::String(datetime::parse_when(&w, now)?));
    }
    if let Some(r) = parsed.recurrence {
        guard_set_and_clear(&set, "recurrence", &r)?;
        // Validated + normalized by the core, exactly as in task.add.
        set.insert("recurrence".into(), Value::String(r));
    }
    // Stays symbolic: an offset must re-anchor when `due` moves (§9).
    if let Some(r) = parsed.remind {
        guard_set_and_clear(&set, "remind", &r)?;
        set.insert("remind".into(), Value::String(r));
    }
    if let Some(e) = parsed.estimate {
        guard_set_and_clear(&set, "estimate", &e)?;
        set.insert("estimate".into(), Value::String(datetime::parse_duration(&e)?));
    }

    if set.is_empty() && parsed.tags.is_empty() {
        return Err(ApiError::bad_request(
            "modify needs something to change — a title, inline sugar (due:friday, !high, \
             +tag, est:4h), a flag, or --clear <field>",
        ));
    }

    let mut result = Value::Null;
    if !set.is_empty() {
        let mut params = json!({ "ref": r#ref, "set": Value::Object(set.clone()) });
        if let Some(rev) = expected_rev {
            params["expected_rev"] = Value::from(rev);
        }
        result = be.call("task.modify", &params)?;
    }

    // Tags: a second call, and deliberately AFTER the modify — if the modify is
    // rejected (bad value, or a lost `expected_rev` race) nothing at all should
    // have happened, and a tag applied first would survive the failure.
    if !parsed.tags.is_empty() {
        let tag_params = json!({ "ref": r#ref, "tags": parsed.tags.clone() });
        let tag_result = be.call("tag.add", &tag_params)?;
        if result.is_null() {
            result = tag_result;
        } else if let Some(tags) = tag_result.get("tags") {
            result["tags"] = tags.clone();
        }
    }

    let text = render::modified(ctx, &result, &set, &parsed.tags);
    Ok((result, text))
}

/// Reject "set X and clear X in one command". Both were typed on purpose and
/// they contradict; picking a winner would silently discard half the intent.
fn guard_set_and_clear(
    set: &serde_json::Map<String, Value>,
    field: &str,
    value: &str,
) -> Result<(), ApiError> {
    if set.get(field) == Some(&Value::Null) {
        return Err(ApiError::bad_request(format!(
            "cannot both set and clear `{field}` (got --clear {field} and a value of {value:?})"
        )));
    }
    Ok(())
}

fn run_list(be: &mut Backend, ctx: &Ctx, filter: &[String]) -> CmdOutcome {
    // Bare `tasqx` (and `tasqx list` with no filter) => the working set.
    let filter_str = if filter.is_empty() { "@working".to_string() } else { filter.join(" ") };
    let params = json!({ "filter": filter_str, "sort": ["-urgency"] });
    let result = be.call("task.list", &params)?;
    let text = render::task_table(ctx, &result);
    Ok((result, text))
}

fn run_start(be: &mut Backend, ctx: &Ctx, r#ref: String, keep: bool) -> CmdOutcome {
    let params = json!({ "ref": r#ref, "keep": keep });
    let result = be.call("task.start", &params)?;
    let text = render::started(ctx, &result);
    Ok((result, text))
}

fn run_stop(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let params = json!({ "ref": r#ref });
    let result = be.call("task.stop", &params)?;
    let text = render::stopped(ctx, &result);
    Ok((result, text))
}

fn run_done(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let params = json!({ "ref": r#ref });
    let result = be.call("task.done", &params)?;
    let text = render::done(ctx, &result);
    Ok((result, text))
}

fn run_show(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let result = be.call("task.get", &json!({ "ref": r#ref }))?;
    let text = render::task_detail(ctx, &result);
    Ok((result, text))
}

/// A method taking only `{ref}` and returning `{short_id, status}`.
fn run_simple_ref(be: &mut Backend, ctx: &Ctx, method: &str, r#ref: String) -> CmdOutcome {
    let result = be.call(method, &json!({ "ref": r#ref }))?;
    let text = render::status_line(ctx, &result);
    Ok((result, text))
}

fn run_annotate(be: &mut Backend, ctx: &Ctx, r#ref: String, text: Vec<String>) -> CmdOutcome {
    let body = text.join(" ");
    let result = be.call("annotation.add", &json!({ "ref": r#ref, "body": body }))?;
    let out = render::annotated(ctx, &result);
    Ok((result, out))
}

fn run_dep(be: &mut Backend, ctx: &Ctx, method: &str, r#ref: String, depends_on: String) -> CmdOutcome {
    let result = be.call(method, &json!({ "ref": r#ref, "depends_on": depends_on }))?;
    let text = render::dep_result(ctx, &result, method == "dependency.add", &depends_on);
    Ok((result, text))
}

/// D21: the one explicit way to move the default project. Validation (exists,
/// not archived) lives in the core, not here — the CLI is one of three callers
/// of `project.use` and the rule has to hold for all of them.
fn run_use(be: &mut Backend, ctx: &Ctx, name: String) -> CmdOutcome {
    let result = be.call("project.use", &json!({ "name": name }))?;
    let text = render::default_switched(ctx, &result);
    Ok((result, text))
}

fn run_projects(be: &mut Backend, ctx: &Ctx, all: bool) -> CmdOutcome {
    let result = be.call("project.list", &json!({ "include_archived": all }))?;
    let text = render::project_table(ctx, &result);
    Ok((result, text))
}

fn run_report(be: &mut Backend, ctx: &Ctx, args: Vec<String>) -> CmdOutcome {
    // First token, if a known group_by keyword, selects grouping; the rest is
    // the filter. Otherwise everything is the filter (group_by defaults).
    let mut group_by = "project".to_string();
    let mut rest: &[String] = &args;
    if let Some(first) = args.first() {
        if matches!(first.as_str(), "project" | "status" | "priority") {
            group_by = first.clone();
            rest = &args[1..];
        }
    }
    let mut params = json!({
        "group_by": group_by,
        "metrics": ["count", "est_total", "overdue", "tracked_total"],
    });
    if !rest.is_empty() {
        params["filter"] = Value::String(rest.join(" "));
    }
    let result = be.call("report.summary", &params)?;
    let text = render::report(ctx, &result, &group_by);
    Ok((result, text))
}

fn run_export(be: &mut Backend, filter: &[String]) -> CmdOutcome {
    let mut params = json!({});
    if !filter.is_empty() {
        params["filter"] = Value::String(filter.join(" "));
    }
    let result = be.call("store.export", &params)?;
    // A filter selects a subset, so edges pointing out of it are trimmed to keep
    // the document self-contained. Warn on stderr, never stdout: stdout IS the
    // JSON and a note there would corrupt every pipe.
    let dropped = result.get("dropped_dependencies").and_then(Value::as_i64).unwrap_or(0);
    if dropped > 0 {
        eprintln!(
            "note: dropped {dropped} dependency edge(s) pointing outside the exported set; \
             widen the filter to keep them"
        );
    }
    // Human output IS the canonical JSON array (git-diffable, greppable).
    let arr = result.get("tasks").cloned().unwrap_or(Value::Array(vec![]));
    let text = format!("{}\n", serde_json::to_string_pretty(&arr).unwrap_or_default());
    Ok((result, text))
}

fn run_import(be: &mut Backend, file: String) -> CmdOutcome {
    let raw = if file == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read stdin: {e}")))?;
        s
    } else {
        std::fs::read_to_string(&file)
            .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read {file}: {e}")))?
    };
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| tasqx_core::ApiError::bad_request(format!("invalid JSON: {e}")))?;
    // Accept either a bare array (export output) or a {"tasks":[...]} object.
    let tasks = match parsed {
        Value::Array(_) => parsed,
        Value::Object(ref o) => o.get("tasks").cloned().unwrap_or(Value::Array(vec![])),
        _ => Value::Array(vec![]),
    };
    let result = be.call("store.import", &json!({ "tasks": tasks }))?;
    let n = result.get("imported").and_then(Value::as_i64).unwrap_or(0);
    Ok((result, format!("Imported {n} task(s)\n")))
}

fn run_next(be: &mut Backend, ctx: &Ctx) -> CmdOutcome {
    // @working already excludes blocked tasks; highest urgency first, take one.
    let params = json!({ "filter": "@working", "sort": ["-urgency"], "limit": 1 });
    let result = be.call("task.list", &params)?;
    let text = render::next_task(ctx, &result);
    Ok((result, text))
}

fn run_why(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let result = be.call("task.get", &json!({ "ref": r#ref }))?;
    let text = render::why(ctx, &result);
    Ok((result, text))
}

// ---- charts, HTML report, and theme tools (DESIGN.md §8) --------------------

/// `tasqx chart <kind>`: read the event log and render a native terminal chart.
fn run_chart(engine: &Engine, ctx: &Ctx, kind: ChartKind) {
    let events = match dispatch(engine, "event.list", &json!({ "limit": 100000 })) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error [{}]: {}", code_str(&e), e.message);
            exit(e.exit_code());
        }
    };
    let out = match kind {
        ChartKind::Throughput { weeks, .. } => {
            chart::render_throughput(ctx, &events, chart::default_weeks(false, weeks))
        }
        ChartKind::Heatmap { year, weeks } => {
            chart::render_heatmap(ctx, &events, chart::default_weeks(year, weeks))
        }
        ChartKind::Burndown { project, days } => {
            let days = days.unwrap_or(30);
            // Resolve project membership (task ids) via a pure task.list read.
            let (members, label) = match &project {
                Some(p) => {
                    let filter = format!("project:{p}");
                    let listed = dispatch(
                        engine,
                        "task.list",
                        &json!({ "filter": filter, "fields": ["id"] }),
                    );
                    let ids = listed
                        .ok()
                        .and_then(|v| v.get("tasks").and_then(Value::as_array).cloned())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
                                .collect::<std::collections::HashSet<String>>()
                        })
                        .unwrap_or_default();
                    (ids, p.clone())
                }
                None => {
                    // All tasks: gather ids from the export.
                    let all = dispatch(engine, "store.export", &json!({}));
                    let ids = all
                        .ok()
                        .and_then(|v| v.get("tasks").and_then(Value::as_array).cloned())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
                                .collect::<std::collections::HashSet<String>>()
                        })
                        .unwrap_or_default();
                    (ids, "all tasks".to_string())
                }
            };
            chart::render_burndown(ctx, &events, &members, days, &label)
        }
    };
    print!("{out}");
}

/// `tasqx report --html`: write the self-contained HTML review.
fn run_html_report(engine: &Engine, ctx: &Ctx, out: Option<String>) {
    let doc = match html::generate(engine, &ctx.theme) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error [{}]: {}", code_str(&e), e.message);
            exit(e.exit_code());
        }
    };
    match out {
        Some(path) => {
            if let Some(parent) = PathBuf::from(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, doc) {
                Ok(()) => println!("Wrote self-contained HTML report → {path}"),
                Err(e) => {
                    eprintln!("error: cannot write {path}: {e}");
                    exit(1);
                }
            }
        }
        None => print!("{doc}"),
    }
}

// ---- docs (the user guide) --------------------------------------------------

/// `tasqx docs`: generate the self-contained guide, then (usually) open it.
///
/// Never fails for the absence of a browser. Writing the file is the job;
/// launching a viewer is a convenience layered on top, so every failure below the
/// write degrades to a printed path and exit 0. That is what makes this command
/// safe to run in CI without a flag — the headless path is the default path with
/// one fewer step, not a separate mode.
fn run_docs(out: Option<&str>, no_open: bool, to_stdout: bool) {
    let doc = docs::generate();

    if to_stdout {
        // NOT `print!`: that panics if stdout closes mid-write, and at ~87KB the
        // guide is comfortably large enough for `tasqx docs --stdout | head` to
        // close the pipe before we finish. A downstream reader that stops early
        // is a normal shell idiom, not a crash — so treat BrokenPipe as success
        // and let any other write error be the real error it is.
        let mut out = std::io::stdout();
        match out.write_all(doc.as_bytes()).and_then(|()| out.flush()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                eprintln!("error: cannot write the guide to stdout: {e}");
                exit(1);
            }
        }
        return;
    }

    // An explicit --out means "give me the file"; opening a browser onto a path
    // the user chose (and may be about to commit, or serve) would be presumptuous.
    let explicit = out.is_some();
    let path = match out {
        Some(p) => PathBuf::from(p),
        None => docs_temp_path(),
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

    if explicit || no_open {
        println!("Wrote the tasqx user guide → {}", path.display());
        return;
    }

    match open_in_browser(&path) {
        Ok(()) => println!("Opened the tasqx user guide → {}", path.display()),
        Err(e) => {
            // The whole point: no browser is not an error. Say what happened, say
            // where the file is, and exit 0 so a CI step never goes red over it.
            eprintln!("note: could not open a browser ({e})");
            println!("The tasqx user guide is at → {}", path.display());
        }
    }
}

/// Where a browser-bound guide gets written: the OS temp dir, under our own
/// folder. Stable per version, so re-running `tasqx docs` reuses the path rather
/// than littering temp with one file per invocation.
fn docs_temp_path() -> PathBuf {
    std::env::temp_dir()
        .join("tasqx-docs")
        .join(format!("tasqx-guide-{}.html", env!("CARGO_PKG_VERSION")))
}

/// The platform's browser launchers, in preference order, for `path`.
///
/// Split out from [`spawn_first`] so the degrade path is testable: a test can
/// hand `spawn_first` a launcher that certainly does not exist and assert we
/// report Err rather than panicking or hanging.
fn browser_candidates(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let p = path.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd builtin, not an exe. The empty "" is the window title —
        // without it, cmd reads a quoted path AS the title and opens nothing.
        vec![("cmd".to_string(), vec!["/C".into(), "start".into(), String::new(), p])]
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
fn spawn_first(candidates: &[(String, Vec<String>)]) -> Result<(), String> {
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
fn open_in_browser(path: &std::path::Path) -> Result<(), String> {
    spawn_first(&browser_candidates(path))
}

/// `tasqx theme list|show`.
fn run_theme(ctx: &Ctx, action: &ThemeAction) {
    match action {
        ThemeAction::List => {
            println!("{}", ctx.paint("header", "Built-in themes"));
            for name in theme::BUILTINS {
                let marker = if name == ctx.theme.name { " ← active" } else { "" };
                println!("  {}{}", name, ctx.paint("muted", marker));
            }
            if let Some(dir) = themes_dir() {
                let mut user: Vec<String> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                            p.file_stem().and_then(|x| x.to_str()).map(str::to_string)
                        } else {
                            None
                        }
                    })
                    .collect();
                user.sort();
                if !user.is_empty() {
                    println!("{}", ctx.paint("header", "User themes"));
                    println!("  {}", ctx.paint("muted", &dir.to_string_lossy()));
                    for name in user {
                        println!("  {name}");
                    }
                }
            }
        }
        ThemeAction::Show { name } => {
            // Preview the requested theme (or the active one) at current caps.
            let preview = match name {
                Some(n) => {
                    let resolved = theme::resolve_name(None, None, Some(n));
                    Ctx::new(theme::load(&resolved, themes_dir().as_deref()), ctx.caps)
                }
                None => Ctx::new(ctx.theme.clone(), ctx.caps),
            };
            // Block glyphs are Unicode; degrade the swatch to ASCII on the plain/
            // legacy path so `theme show | cat` never emits mojibake.
            let swatch = if preview.caps.unicode { "████" } else { "####" };
            let bar = if preview.caps.unicode { "█" } else { "#" };
            println!("{}", preview.paint("header", &format!("Theme: {}", preview.theme.name)));
            for role in preview.theme.role_names() {
                let sample = preview.theme.paint(&role, &format!("{swatch} sample text"), &preview.caps);
                println!("  {:<14} {sample}", role);
            }
            // Show the urgency ramp as a cold→hot strip.
            let strip: String = (0..=10)
                .map(|i| {
                    let t = i as f64 / 10.0;
                    preview.theme.ramp_style(t).paint(bar, &preview.caps)
                })
                .collect();
            println!("  {:<14} {strip}  {}", "urgency.ramp", preview.paint("muted", "cold → hot"));
        }
    }
}

/// The stable error `code` as a string (for CLI diagnostics).
fn code_str(e: &tasqx_core::ApiError) -> String {
    serde_json::to_value(e.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

// ---- daemon + watch (DESIGN.md §2, §6a) -------------------------------------

/// `tasqx daemon`: open one Engine and serve the local socket until Ctrl-C.
/// Diagnostics go to stderr; the socket carries the newline-delimited JSON API.
fn run_daemon(socket_flag: Option<&str>, db: Option<&str>) {
    let socket = resolve_socket(socket_flag);
    let engine = match open_engine_at(db) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("tasqx daemon: {msg}");
            exit(1);
        }
    };

    // Ctrl-C flips the shutdown flag; `serve` then unwinds its accept loop and
    // removes the Unix socket file (a no-op for Windows named pipes).
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let sd = shutdown.clone();
        if let Err(e) = ctrlc::set_handler(move || sd.store(true, Ordering::SeqCst)) {
            eprintln!("tasqx daemon: could not install Ctrl-C handler: {e}");
        }
    }

    // Quiet by default (DESIGN.md §9): reminders always emit their event + log
    // line, but a native OS notification needs an explicit `[notify] enabled`
    // opt-in. Without the `notify-os` feature compiled in, this is inert and the
    // log backend is used regardless — the flag can never resurrect a backend
    // that isn't in the binary.
    let os_notify = config_notify_enabled();
    let notifier = notify::default_notifier(os_notify);

    eprintln!("tasqx daemon: listening on {socket} (Ctrl-C to stop)");
    match daemon::serve_with_notifier(engine, &socket, shutdown, notifier) {
        Ok(()) => eprintln!("tasqx daemon: stopped"),
        Err(e) => {
            eprintln!("tasqx daemon: bind/serve failed on {socket}: {e}");
            exit(1);
        }
    }
}

/// `tasqx watch [filter]`: subscribe to a daemon and re-render on every push.
/// On a TTY it clears + reprints the working set; on a pipe it streams one line
/// per event (DESIGN.md §6a). It never auto-spawns a daemon — it hints instead.
fn run_watch(socket_flag: Option<&str>, no_daemon: bool, filter: &[String], ctx: &Ctx) {
    if no_daemon {
        eprintln!("tasqx watch: --no-daemon is set, but watch requires a running daemon");
        exit(1);
    }
    let socket = resolve_socket(socket_flag);
    let mut conn = match daemon::try_connect(&socket) {
        Some(c) => c,
        None => {
            eprintln!("tasqx watch: no daemon reachable at {socket}");
            eprintln!("hint: start one with `tasqx daemon` (add `--socket {socket}` to match)");
            exit(1);
        }
    };
    if let Err(e) = conn.subscribe() {
        eprintln!("tasqx watch: subscribe failed: {e}");
        exit(1);
    }

    let filter_str = if filter.is_empty() { "@working".to_string() } else { filter.join(" ") };
    let tty = std::io::stdout().is_terminal();

    // Initial paint.
    if let Err(e) = watch_render(&mut conn, &filter_str, ctx, tty) {
        eprintln!("tasqx watch: {e}");
        exit(1);
    }

    // Live loop: block on the next frame; on each change, refresh.
    loop {
        match conn.next_frame() {
            Ok(Some(daemon::Frame::Event(evt))) => {
                if tty {
                    if let Err(e) = watch_render(&mut conn, &filter_str, ctx, true) {
                        eprintln!("tasqx watch: {e}");
                        exit(1);
                    }
                } else {
                    let data = evt.get("data").cloned().unwrap_or(Value::Null);
                    let op = data.get("op").and_then(Value::as_str).unwrap_or("change");
                    match data.get("short_id").and_then(Value::as_i64) {
                        Some(s) => println!("task.changed op={op} short_id={s}"),
                        None => println!("task.changed op={op}"),
                    }
                    let _ = std::io::stdout().flush();
                }
            }
            // A stray response (none expected here) is harmless; ignore it.
            Ok(Some(daemon::Frame::Response(_))) => {}
            Ok(None) => {
                eprintln!("tasqx watch: daemon closed the connection");
                exit(1);
            }
            Err(e) => {
                eprintln!("tasqx watch: read error: {e}");
                exit(1);
            }
        }
    }
}

/// Fetch the working set over the socket and (re)paint it, reusing render.rs so
/// themes + degradation behave exactly as in the one-shot list view.
fn watch_render(conn: &mut daemon::Conn, filter: &str, ctx: &Ctx, tty: bool) -> Result<(), String> {
    let params = json!({ "filter": filter, "sort": ["-urgency"] });
    let env = conn.request("task.list", &params).map_err(|e| format!("task.list: {e}"))?;
    if env.get("ok") != Some(&Value::Bool(true)) {
        return Err(format!(
            "daemon error: {}",
            env.get("error").map(|e| e.to_string()).unwrap_or_default()
        ));
    }
    let result = env.get("result").cloned().unwrap_or(Value::Null);
    let text = render::task_table(ctx, &result);
    if tty {
        // Clear screen + cursor home, then reprint the fresh working set.
        print!("\x1b[2J\x1b[H");
    }
    print!("{text}");
    let _ = std::io::stdout().flush();
    Ok(())
}

/// The stdio one-shot transport.
fn run_api() {
    let engine = match open_engine() {
        Ok(e) => e,
        Err(msg) => {
            let env = json!({
                "tasqx": "1", "ok": false,
                "error": { "code": "internal", "message": msg }
            });
            println!("{env}");
            exit(1);
        }
    };

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        let env = json!({
            "tasqx": "1", "ok": false,
            "error": { "code": "bad_request", "message": "could not read stdin" }
        });
        println!("{env}");
        exit(2);
    }

    let response = handle_envelope(&engine, &input);
    println!("{}", serde_json::to_string(&response).unwrap_or_default());
}

/// The `tasqx mcp` subcommand family (DESIGN.md §7, D7).
fn run_mcp(action: &McpAction) {
    match action {
        McpAction::Token { scope } => {
            let sc = if scope == "read" { Scope::Read } else { Scope::Write };
            // The token IS the deliverable; print it to stdout only.
            println!("{}", sc.mint_token());
        }
        McpAction::Serve { token } => run_mcp_serve(token.as_deref()),
    }
}

/// Run the MCP stdio server. Scope precedence: `--token`, then $TASQX_MCP_TOKEN,
/// else a least-privilege default of read-only. Write access is an explicit
/// opt-in: mint a write token with `tasqx mcp token --scope write`. This fails
/// closed — an unwired server never silently exposes destructive tools to an
/// LLM. Diagnostics go to stderr ONLY; stdout carries nothing but
/// newline-delimited JSON-RPC responses.
fn run_mcp_serve(token: Option<&str>) {
    let tok = token.map(str::to_string).or_else(|| {
        std::env::var("TASQX_MCP_TOKEN").ok().filter(|s| !s.is_empty())
    });
    let scope = match tok.as_deref() {
        Some(t) => match Scope::from_token(t) {
            Some(s) => s,
            None => {
                eprintln!("tasqx mcp: unrecognized token; refusing to start");
                exit(2);
            }
        },
        None => {
            eprintln!(
                "tasqx mcp: no token provided; defaulting to READ-ONLY scope. \
                 For write access, pass a token from `tasqx mcp token --scope write`."
            );
            Scope::Read
        }
    };

    let engine = match open_engine() {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("tasqx mcp: {msg}");
            exit(1);
        }
    };
    eprintln!("tasqx mcp: serving over stdio (scope={})", scope.as_str());

    let server = McpServer::new(&engine, scope);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF: peer closed stdin.
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let out = match serde_json::from_str::<Value>(trimmed) {
                    Ok(msg) => server.handle_message(&msg),
                    Err(e) => Some(json!({
                        "jsonrpc": "2.0", "id": Value::Null,
                        "error": { "code": -32700, "message": format!("Parse error: {e}") }
                    })),
                };
                if let Some(resp) = out {
                    let mut w = stdout.lock();
                    let _ = writeln!(w, "{}", serde_json::to_string(&resp).unwrap_or_default());
                    let _ = w.flush();
                }
            }
            Err(_) => break,
        }
    }
}

/// Resolve the store path and open the engine.
fn open_engine() -> Result<Engine, String> {
    let path = db_path()?;
    Engine::open(&path.to_string_lossy()).map_err(|e| e.message)
}

/// Open an Engine at an explicit `--db` path (the daemon), else the default
/// store. Creates parent directories so a fresh `--db path/to/tasks.db` works.
fn open_engine_at(db: Option<&str>) -> Result<Engine, String> {
    match db {
        Some(p) => {
            if let Some(parent) = PathBuf::from(p).parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            Engine::open(p).map_err(|e| e.message)
        }
        None => open_engine(),
    }
}

fn db_path() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("TASQX_DB") {
        if !p.is_empty() {
            if let Some(parent) = PathBuf::from(&p).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            return Ok(PathBuf::from(p));
        }
    }
    let dirs = directories::ProjectDirs::from("dev", "tasqx", "tasqx")
        .ok_or_else(|| "cannot determine a data directory".to_string())?;
    let dir = dirs.data_dir();
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create data dir: {e}"))?;
    Ok(dir.join("tasks.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_of(argv: &[&str]) -> Command {
        Cli::try_parse_from(argv).expect("argv should parse").command.expect("a subcommand")
    }

    /// Regression: clap reads a leading `-` as a flag, so `--remind -1h` parsed
    /// as an unknown `-1` and the command was rejected — breaking the single most
    /// common reminder form. Guarded by `allow_hyphen_values` on the arg.
    #[test]
    fn remind_flag_accepts_a_leading_hyphen_offset() {
        for (argv, want) in [
            (vec!["tasqx", "add", "Ship it", "--remind", "-1h"], "-1h"),
            (vec!["tasqx", "add", "Ship it", "--remind", "-30m"], "-30m"),
            (vec!["tasqx", "add", "Ship it", "--remind", "-2d"], "-2d"),
            // An absolute value must keep working through the same flag.
            (vec!["tasqx", "add", "Ship it", "--remind", "friday 9am"], "friday 9am"),
        ] {
            match add_of(&argv) {
                Command::Add { remind, .. } => {
                    assert_eq!(remind.as_deref(), Some(want), "argv: {argv:?}")
                }
                _ => panic!("expected an add command"),
            }
        }
    }

    /// Quiet by default (§9): the flag is optional and absent means no reminder.
    #[test]
    fn remind_flag_is_optional() {
        match add_of(&["tasqx", "add", "Ship it", "--due", "friday"]) {
            Command::Add { remind, .. } => assert_eq!(remind, None),
            _ => panic!("expected an add command"),
        }
    }

    // ---- modify (DESIGN.md §5, §12-D13) -------------------------------------

    /// THE trap, re-armed for the new verb: clap treats a leading `-` value as a
    /// flag, so `--remind -30m` / `--due -1d` parse as unknown `-3` / `-1` flags
    /// unless the arg opts into `allow_hyphen_values`. This shipped broken once
    /// on `add` because only the inline `remind:-1h` form was tested — every
    /// hyphen-taking flag on `modify` is asserted through the FLAG form here.
    #[test]
    fn modify_flags_accept_leading_hyphen_values() {
        for (argv, want_due, want_sched, want_wait, want_remind) in [
            (
                vec!["tasqx", "modify", "42", "--remind", "-30m"],
                None,
                None,
                None,
                Some("-30m"),
            ),
            (vec!["tasqx", "modify", "42", "--due", "-1d"], Some("-1d"), None, None, None),
            (vec!["tasqx", "modify", "42", "--due", "-2w"], Some("-2w"), None, None, None),
            (
                vec!["tasqx", "modify", "42", "--scheduled", "-1d"],
                None,
                Some("-1d"),
                None,
                None,
            ),
            (vec!["tasqx", "modify", "42", "--wait", "-3d"], None, None, Some("-3d"), None),
            // Combined, and still with a non-hyphen value in the mix.
            (
                vec!["tasqx", "modify", "42", "--due", "-1d", "--remind", "-1h"],
                Some("-1d"),
                None,
                None,
                Some("-1h"),
            ),
            (
                vec!["tasqx", "modify", "42", "--remind", "friday 9am"],
                None,
                None,
                None,
                Some("friday 9am"),
            ),
        ] {
            match add_of(&argv) {
                Command::Modify { due, scheduled, wait, remind, .. } => {
                    assert_eq!(due.as_deref(), want_due, "due — argv: {argv:?}");
                    assert_eq!(scheduled.as_deref(), want_sched, "scheduled — argv: {argv:?}");
                    assert_eq!(wait.as_deref(), want_wait, "wait — argv: {argv:?}");
                    assert_eq!(remind.as_deref(), want_remind, "remind — argv: {argv:?}");
                }
                _ => panic!("expected a modify command for {argv:?}"),
            }
        }
    }

    /// `add` takes hyphen dates through its flags too — `--due -1d` was never
    /// guarded there, only `--remind`.
    #[test]
    fn add_date_flags_accept_leading_hyphen_values() {
        match add_of(&["tasqx", "add", "Late thing", "--due", "-1d", "--scheduled", "-2d"]) {
            Command::Add { due, scheduled, .. } => {
                assert_eq!(due.as_deref(), Some("-1d"));
                assert_eq!(scheduled.as_deref(), Some("-2d"));
            }
            _ => panic!("expected an add command"),
        }
    }

    #[test]
    fn modify_aliases_all_resolve() {
        for verb in ["modify", "mod", "m", "edit"] {
            match add_of(&["tasqx", verb, "42", "--priority", "H"]) {
                Command::Modify { r#ref, priority, .. } => {
                    assert_eq!(r#ref, "42");
                    assert_eq!(priority.as_deref(), Some("H"), "verb: {verb}");
                }
                _ => panic!("expected a modify command for {verb}"),
            }
        }
    }

    #[test]
    fn modify_collects_clear_fields_and_sugar() {
        match add_of(&[
            "tasqx", "modify", "42", "New", "title", "due:friday", "--clear", "remind", "--clear",
            "recurrence",
        ]) {
            Command::Modify { r#ref, rest, clear, .. } => {
                assert_eq!(r#ref, "42");
                assert_eq!(rest, vec!["New", "title", "due:friday"]);
                assert_eq!(clear, vec!["remind".to_string(), "recurrence".to_string()]);
            }
            _ => panic!("expected a modify command"),
        }
    }

    /// `--clear` is a closed set. A typo names the real options rather than
    /// silently clearing nothing, and `title` is rejected by construction —
    /// a task without a title is not a task.
    #[test]
    fn modify_clear_rejects_unknown_and_unclearable_fields() {
        for bad in ["title", "status", "tags", "dew", "id"] {
            let e = Cli::try_parse_from(["tasqx", "modify", "42", "--clear", bad]);
            assert!(e.is_err(), "--clear {bad} must be rejected");
        }
        for good in CLEARABLE {
            assert!(
                Cli::try_parse_from(["tasqx", "modify", "42", "--clear", good]).is_ok(),
                "--clear {good} must be accepted"
            );
        }
    }

    #[test]
    fn modify_takes_expected_rev_for_optimistic_concurrency() {
        match add_of(&["tasqx", "modify", "42", "--priority", "L", "--expected-rev", "7"]) {
            Command::Modify { expected_rev, .. } => assert_eq!(expected_rev, Some(7)),
            _ => panic!("expected a modify command"),
        }
    }

    // ---- docs (the user guide) ----------------------------------------------

    /// THE headless guarantee, at the seam where it is decided: with no launcher
    /// on the box, `spawn_first` must *report* failure rather than panicking or
    /// hanging — which is what lets `run_docs` degrade to a printed path and
    /// exit 0. Asserting this on a real machine is otherwise impossible: every
    /// platform we support ships a launcher, and on Windows `cmd.exe` resolves
    /// from System32 even with an empty PATH, so "no browser" cannot be staged
    /// by manipulating the environment. It has to be injected.
    #[test]
    fn a_missing_browser_is_reported_not_fatal() {
        let bogus = vec![(
            "tasqx-no-such-launcher-9f3a1c".to_string(),
            vec!["/tmp/guide.html".to_string()],
        )];
        let err = spawn_first(&bogus).expect_err("a nonexistent launcher cannot spawn");
        assert!(
            err.contains("tasqx-no-such-launcher-9f3a1c"),
            "the error should name the launcher it tried, got: {err}"
        );
    }

    /// Fallbacks are tried in order: an earlier miss must not abort the walk, or a
    /// Linux box without `xdg-open` would never reach `gio`.
    #[test]
    fn a_later_launcher_still_wins_after_an_earlier_miss() {
        // A real, harmless program is the last candidate; the first cannot exist.
        let real = if cfg!(windows) { "cmd" } else { "true" };
        let args: Vec<String> =
            if cfg!(windows) { vec!["/C".into(), "exit".into()] } else { vec![] };
        let candidates = vec![
            ("tasqx-no-such-launcher-9f3a1c".to_string(), vec![]),
            (real.to_string(), args),
        ];
        assert!(
            spawn_first(&candidates).is_ok(),
            "the walk must continue past a launcher that does not exist"
        );
    }

    /// Every platform must offer at least one launcher, and each must carry the
    /// file path — a candidate list that forgot the path would open a blank
    /// browser and look like a content bug.
    #[test]
    fn browser_candidates_exist_and_carry_the_path() {
        let path = std::path::PathBuf::from("/tmp/tasqx-guide.html");
        let cands = browser_candidates(&path);
        assert!(!cands.is_empty(), "this platform has no browser launcher at all");
        for (bin, args) in &cands {
            assert!(!bin.is_empty(), "empty launcher name");
            assert!(
                args.iter().any(|a| a.contains("tasqx-guide.html")),
                "launcher `{bin}` never receives the file path: {args:?}"
            );
        }
    }

    /// `--out` and `--no-open` are the two headless doors, and `--stdout` the pipe.
    #[test]
    fn docs_flags_parse() {
        match add_of(&["tasqx", "docs", "--out", "guide.html"]) {
            Command::Docs { out, no_open, stdout } => {
                assert_eq!(out.as_deref(), Some("guide.html"));
                assert!(!no_open, "--out implies no-open at the behaviour level, not the flag");
                assert!(!stdout);
            }
            _ => panic!("expected a docs command"),
        }
        match add_of(&["tasqx", "docs", "--no-open"]) {
            Command::Docs { no_open, .. } => assert!(no_open),
            _ => panic!("expected a docs command"),
        }
        match add_of(&["tasqx", "docs", "--stdout"]) {
            Command::Docs { stdout, .. } => assert!(stdout),
            _ => panic!("expected a docs command"),
        }
        // Bare `docs` is the browser path.
        match add_of(&["tasqx", "docs"]) {
            Command::Docs { out, no_open, stdout } => {
                assert!(out.is_none() && !no_open && !stdout);
            }
            _ => panic!("expected a docs command"),
        }
    }

    /// The temp path must be stable across runs (so `docs` does not litter) and
    /// must actually be an HTML file (so a browser renders rather than downloads).
    #[test]
    fn docs_temp_path_is_stable_and_html() {
        let a = docs_temp_path();
        let b = docs_temp_path();
        assert_eq!(a, b, "the temp path must not vary between invocations");
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("html"));
    }
}
