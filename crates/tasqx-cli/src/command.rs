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
        /// Write the HTML report to this file (default: stdout). Requires --html.
        ///
        /// `requires` because only the `--html` branch of `execute` ever reads
        /// it: the terminal-table branch destructures it away with `..`, so
        /// `report --out r.html` used to print the table, write no file, and
        /// exit 0. Under a CI redirect that reads as a report that was written.
        /// Same silent omission `--all` below is guarded against.
        #[arg(long, requires = "html")]
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
    /// Memory: store and search knowledge docs + annotations (DESIGN.md §12-D41).
    #[command(after_help = crate::cmddoc::after_help("memory"))]
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
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
        ///
        /// Excludes --stdout: `run_docs` returns on the stdout branch before it
        /// ever looks at `out`, so the pair wrote no file and exited 0. Two
        /// destinations for one document is a usage error — which is what the
        /// manual's `[--out PATH | --no-open | --stdout]` already promised.
        #[arg(long, value_name = "PATH", conflicts_with = "stdout")]
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

/// Widest chart window we will draw, in weeks (a decade). The ceiling is not
/// cosmetic: every window ends up inside `jiff`'s `ToSpan::days`, which PANICS
/// outside ±7,304,484 — an abort whose message names neither tasqx nor the flag.
/// Well below that threshold the same number is a hang instead, because
/// `chart::heatmap` sizes a `Vec` by `weeks * 7` and `chart::throughput`
/// allocates one `WeekBucket` per week.
///
/// Enforced here rather than clamped in `chart::default_weeks` for the reason
/// D17 gives for `--estimate`: silently rewriting `--weeks 100000` into 520
/// would answer a question the user did not ask, and label the chart as if it
/// had. The floor is 1 because a zero-wide window charts nothing; `weeks.max(1)`
/// downstream hid that request instead of refusing it.
const MAX_CHART_WEEKS: u64 = 520;
/// Same reasoning for `burndown --days`, which never passes through
/// `default_weeks` at all — a decade of daily points.
const MAX_CHART_DAYS: u64 = 3650;

/// Bound a window flag at parse time. Yields `RangedU64ValueParser<usize>` so
/// the fields stay `Option<usize>` and every call site keeps its type.
fn window_parser(max: u64) -> clap::builder::RangedU64ValueParser<usize> {
    clap::builder::RangedU64ValueParser::<usize>::new().range(1..=max)
}

#[derive(Subcommand)]
pub(super) enum ChartKind {
    /// Tasks added vs done per ISO week (from the events table).
    Throughput {
        /// Weekly buckets (default view; kept for parity with the spec).
        #[arg(long)]
        weekly: bool,
        /// Number of weeks to show (1-520; default 12).
        #[arg(long, value_parser = window_parser(MAX_CHART_WEEKS))]
        weeks: Option<usize>,
    },
    /// GitHub-style completion density per day (from done events).
    Heatmap {
        /// Show a full year (52 weeks).
        #[arg(long)]
        year: bool,
        /// Number of weeks to show (1-520; default 12; overrides --year).
        #[arg(long, value_parser = window_parser(MAX_CHART_WEEKS))]
        weeks: Option<usize>,
    },
    /// Remaining open tasks over the last N days (from the events table).
    Burndown {
        /// Restrict to a project (else all tasks).
        #[arg(long)]
        project: Option<String>,
        /// Number of days to show (1-3650; default 30).
        #[arg(long, value_parser = window_parser(MAX_CHART_DAYS))]
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
pub(super) enum MemoryAction {
    /// Store a knowledge doc (maps to memory.add). Body is stored verbatim.
    Add {
        /// Doc title.
        title: String,
        /// Body text — multi-line markdown is fine.
        body: String,
        /// Where this came from: a path, URL, or ticket.
        #[arg(long)]
        source: Option<String>,
    },
    /// Search docs + annotations, bm25-ranked (maps to memory.search).
    Search {
        /// Search words. Matched as phrases, so hyphens and dots are safe.
        ///
        /// Deliberately NOT named `filter`: this is FTS text, not the filter
        /// DSL, so the argv hyphen pre-pass must leave it alone.
        #[arg(required = true)]
        query: Vec<String>,
        /// Max hits (default 10).
        #[arg(long)]
        limit: Option<u64>,
        /// What to search: all | docs | annotations.
        #[arg(long)]
        scope: Option<String>,
        /// Treat the query as raw FTS5 syntax (prefix*, AND/OR, columns).
        #[arg(long)]
        raw: bool,
    },
    /// Remove a doc by id (maps to memory.remove).
    Rm {
        /// The doc UUID, as printed by `memory add` and `memory search`.
        id: String,
    },
    /// Import markdown files as docs: one doc per file (maps to memory.add).
    Import {
        /// A file, or a directory whose *.md files are imported (non-recursive).
        path: String,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    /// Chart windows are fed straight into `jiff`'s `ToSpan::days`, which PANICS
    /// outside ±7,304,484, and long before that a `weeks * 7` window is a
    /// multi-gigabyte allocation that never returns. The window therefore has to
    /// be a parse-time constraint, not a runtime clamp: a silent clamp would
    /// print a 520-week chart labelled as if `--weeks 100000` had been honoured.
    #[test]
    fn chart_windows_outside_the_supported_range_are_usage_errors() {
        for argv in [
            // Reproduced as aborts against the real binary: the jiff panic names
            // neither tasqx nor the flag.
            &["tasqx", "chart", "heatmap", "--weeks", "2000000"][..],
            &["tasqx", "chart", "burndown", "--days", "99999999"][..],
            // Reproduced as an 8s timeout: one `WeekBucket` allocated per week.
            &["tasqx", "chart", "throughput", "--weeks", "100000000"][..],
            // The low end matters too: a zero-wide window is a chart of nothing,
            // and `weeks.max(1)` merely hid the request instead of refusing it.
            &["tasqx", "chart", "heatmap", "--weeks", "0"][..],
            &["tasqx", "chart", "throughput", "--weeks", "0"][..],
            &["tasqx", "chart", "burndown", "--days", "0"][..],
        ] {
            let err = Cli::try_parse_from(argv)
                .err()
                .unwrap_or_else(|| panic!("{argv:?} must be rejected"));
            assert_eq!(
                err.kind(),
                ErrorKind::ValueValidation,
                "{argv:?} must fail as a usage error, not a panic or a hang"
            );
            // The whole point of the clap route over a clamp: the message names
            // the flag the user mistyped.
            let msg = err.to_string();
            assert!(
                msg.contains("--weeks") || msg.contains("--days"),
                "{argv:?}: the error must name the flag, got {msg:?}"
            );
        }
    }

    /// The bound must not eat the documented windows: DESIGN.md §8 promises the
    /// 12-week default and `--year` (52), and a decade is still a sane ask.
    #[test]
    fn chart_windows_inside_the_supported_range_still_parse() {
        for argv in [
            &["tasqx", "chart", "heatmap", "--weeks", "1"][..],
            &["tasqx", "chart", "heatmap", "--weeks", "52"][..],
            &["tasqx", "chart", "throughput", "--weeks", "520"][..],
            &["tasqx", "chart", "burndown", "--days", "30"][..],
            &["tasqx", "chart", "burndown", "--days", "3650"][..],
        ] {
            assert!(
                Cli::try_parse_from(argv).is_ok(),
                "{argv:?} is a supported window"
            );
        }
    }

    /// `--out` is read on the `--html` branch of `execute` only; the terminal
    /// table branch destructures it away with `..`. `tasqx report --out r.html`
    /// therefore printed the table, wrote no file, and exited 0 — a CI step that
    /// redirects stdout sees success and a missing report. Same silent-omission
    /// class as `--all` next door, and it must fail at parse time for the same
    /// reason.
    #[test]
    fn report_out_without_html_is_rejected() {
        let err = Cli::try_parse_from(["tasqx", "report", "--out", "r.html"])
            .err()
            .expect("--out with no --html writes nothing");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(
            err.to_string().contains("--html"),
            "the error must name the flag that makes --out mean something"
        );
        // The documented spelling (DESIGN.md §8) keeps working.
        assert!(Cli::try_parse_from(["tasqx", "report", "--html", "--out", "review.html"]).is_ok());
        assert!(Cli::try_parse_from(["tasqx", "report", "--html"]).is_ok());
    }

    /// `run_docs` returns on `to_stdout` before it ever looks at `out`, so
    /// `--out X --stdout` wrote no file either. Two contradictory output sinks
    /// are a usage error, which is what the manual's `[--out PATH | --no-open |
    /// --stdout]` already promised without clap enforcing it.
    #[test]
    fn docs_out_with_stdout_is_rejected() {
        let err = Cli::try_parse_from(["tasqx", "docs", "--out", "g.html", "--stdout"])
            .err()
            .expect("--out and --stdout are two different destinations");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
        // Each sink on its own is untouched.
        assert!(Cli::try_parse_from(["tasqx", "docs", "--out", "g.html"]).is_ok());
        assert!(Cli::try_parse_from(["tasqx", "docs", "--stdout"]).is_ok());
        assert!(Cli::try_parse_from(["tasqx", "docs", "--no-open"]).is_ok());
    }
}
