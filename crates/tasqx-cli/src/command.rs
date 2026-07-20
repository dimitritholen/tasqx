//! Command-line syntax declarations.
//!
//! This module owns clap parsing only. Execution, transport selection, and
//! rendering remain in the parent orchestration module.

use clap::{Parser, Subcommand};

use super::{CLEARABLE, VERSION};

#[derive(Parser)]
#[command(
    name = "tasqx",
    version = VERSION,
    // `-V` on a subcommand LISTED TASKS before this. Nothing was wrong at any
    // one layer: version was declared on the root only, so `-V` was not a flag
    // `list` knew, and the argv pre-pass then read it — correctly, by the dash
    // grammar — as the tag exclusion `-V`. But `-h` IS propagated by clap, so
    // the tool's two shortest conventions disagreed with each other.
    // Propagating declares `-V` everywhere, and `no_declared_short_flag_is_ever_escaped`
    // then exempts it from the pre-pass automatically, because that guard reads
    // clap's own arg table rather than a list of letters (D30).
    propagate_version = true,
    about = "A fast, terminal-first, AI-native task manager.",
    after_help = "Run `tasqx manual` for the full in-terminal guide, or `tasqx <command> -h` for examples.",
    disable_help_subcommand = true
)]
pub(super) struct Cli {
    /// Print the raw JSON API result instead of the human table.
    #[arg(long, global = true)]
    pub(super) json: bool,

    /// Theme name (built-in nord|gruvbox|dracula|solarized|mono, or a user file).
    /// Overrides $TASQX_THEME and the config file (DESIGN.md §8).
    #[arg(long, global = true)]
    pub(super) theme: Option<String>,

    /// Local socket / named-pipe address of a tasqx daemon. Overrides $TASQX_SOCK.
    /// When a daemon is reachable, one-shot commands route through it (single
    /// writer); otherwise they run in-process. Also selects the address for
    /// `daemon` and `watch`.
    #[arg(long, global = true)]
    pub(super) socket: Option<String>,

    /// Never route through a daemon; always run one-shot in-process (the
    /// pre-daemon behaviour). Escape hatch for scripts.
    #[arg(long, global = true)]
    pub(super) no_daemon: bool,

    #[command(subcommand)]
    pub(super) command: Option<Command>,
}

#[derive(Subcommand)]
pub(super) enum Command {
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
        ///
        /// Deliberately NOT `allow_hyphen_values`, unlike `--due`/`--remind`.
        /// `-tag` is core filter grammar and must be typable, but the setting
        /// buys that by making this positional swallow every later hyphen
        /// token INCLUDING clap's own flags, which broke `list @working
        /// --json`. The dash is hidden by the `argv` pre-pass instead, leaving
        /// clap full authority over every `--flag` in any position.
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
    #[command(
        alias = "delete",
        alias = "del",
        alias = "rm",
        after_help = crate::cmddoc::after_help("cancel")
    )]
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
        ///
        /// Hyphen-tolerant because the tail is filter DSL and `-tag` is part of
        /// it — via the `argv` pre-pass, not `allow_hyphen_values`, which would
        /// swallow `--html`; see `List::filter`.
        args: Vec<String>,
        /// Emit a single self-contained HTML report (inline CSS + SVG, no
        /// external requests) instead of the terminal table.
        #[arg(long)]
        html: bool,
        /// Write the HTML report to this file (default: stdout).
        #[arg(long)]
        out: Option<String>,
        /// Count cancelled tasks too. By default a report excludes cancelled
        /// tasks, unless the filter itself names a status (DESIGN D24).
        ///
        /// Rejected alongside `--html`: the HTML page builds its own scope and
        /// has no way to honour this yet, and accepting a flag we then ignore is
        /// exactly the silent omission D24 exists to stop.
        #[arg(long, conflicts_with = "html")]
        all: bool,
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
    /// Read and change tasqx settings (DESIGN.md §12-D25).
    #[command(after_help = crate::cmddoc::after_help("config"))]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Export tasks as canonical JSON (maps to store.export).
    #[command(after_help = crate::cmddoc::after_help("export"))]
    Export {
        /// Optional filter DSL.
        ///
        /// Hyphen-tolerant via the `argv` pre-pass so `-tag` is typable; see
        /// `List::filter`.
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
        ///
        /// Hyphen-tolerant via the `argv` pre-pass so `-tag` is typable; see
        /// `List::filter`.
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
    /// Browse the complete manual in your terminal: a themed, navigable guide.
    /// `tasqx manual` prints the table of contents; `tasqx manual <command|topic>`
    /// opens one section. Needs no store and no network.
    #[command(alias = "man", after_help = crate::cmddoc::after_help("manual"))]
    Manual {
        /// A command (e.g. `init`), an alias, or a topic slug (e.g. `filters`).
        topic: Option<String>,
    },
}

#[derive(Subcommand)]
pub(super) enum ChartKind {
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
pub(super) enum ThemeAction {
    /// List available themes (built-ins + user files).
    List,
    /// Preview a theme's roles (default: the active theme).
    Show {
        /// Theme name to preview.
        name: Option<String>,
    },
    /// Persist a theme choice to `config.toml`.
    Set {
        /// Theme name: a built-in or a user file in the themes directory.
        name: String,
    },
}

#[derive(Subcommand)]
pub(super) enum ConfigAction {
    /// Show every setting with its value, source and default.
    List,
    /// Print one setting's resolved value.
    Get {
        /// Setting key, e.g. `theme.name`.
        key: String,
    },
    /// Set a setting in `config.toml`.
    Set {
        /// Setting key, e.g. `theme.name`.
        key: String,
        /// New value.
        value: String,
    },
    /// Remove a setting so it falls back to its default.
    Unset {
        /// Setting key.
        key: String,
    },
    /// Print the path of `config.toml` (it may not exist yet).
    Path,
    /// Edit settings on an interactive screen, previewing themes live.
    Edit,
}

#[derive(Subcommand)]
pub(super) enum McpAction {
    /// Run the MCP stdio server: newline-delimited JSON-RPC 2.0 on stdin/stdout.
    Serve {
        /// Operator-selected capability scope for this stdio process.
        #[arg(long, default_value = "read", value_parser = ["read", "write"])]
        scope: String,
    },
}

