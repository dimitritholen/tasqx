//! Command-line syntax declarations.
//!
//! This module owns clap parsing only. Execution, transport selection, and
//! rendering remain in the parent orchestration module.

use clap::builder::{PossibleValue, PossibleValuesParser};
use clap::{Args, Parser, Subcommand, ValueHint};

use tasqx_core::engine::MEMORY_SCOPES;
use tasqx_core::Priority;

use super::{CLEARABLE, VERSION};

/// The `--priority` vocabulary, built from [`Priority::SPELLINGS`] so it is the
/// engine's list rather than a copy of it.
///
/// Declaring it to clap buys two things that are the same thing seen from two
/// sides. A shell can now complete `--priority <TAB>`, because a completion
/// engine can only offer values something has declared. And a bad value now
/// fails at parse time naming what would have worked, instead of travelling to
/// the engine and coming back as `invalid priority: bogus` — a message that
/// named the mistake and not the way out.
///
/// The long spellings are `hide`d, not omitted: they still parse (dropping them
/// would break `--priority high`, which the help text has always promised), but
/// a shell offering seven candidates for a three-valued field is worse than one
/// offering three. clap's completion engine marks hidden possible values
/// `hide(true)` and surfaces them only when nothing visible matches the partial
/// word, so `--priority hi<TAB>` still finds `high`. Which spellings are
/// canonical is read off `Priority::as_str` rather than listed again here.
///
/// Pair this with `ignore_case = true` on the arg: without it clap would reject
/// `--priority HIGH`, which parses today.
///
/// Wrapped in [`Trimmed`] for the same reason `ignore_case` is set: to keep the
/// declaration from narrowing what already worked. [`Priority::parse`] opens with
/// `s.trim()`, so `--priority " H"` and `--priority "high "` reached the engine
/// and parsed before this parser existed; `PossibleValuesParser` compares the
/// value as given and rejected them.
fn priority_parser() -> Trimmed {
    Trimmed(PossibleValuesParser::new(
        Priority::SPELLINGS
            .iter()
            .map(|(spelling, p)| PossibleValue::new(*spelling).hide(*spelling != p.as_str()))
            .collect::<Vec<_>>(),
    ))
}

/// A [`PossibleValuesParser`] that trims surrounding whitespace before matching.
///
/// Declaring a closed vocabulary to clap is meant to be *additive* — better
/// errors, and values a shell can complete — and it is additive only if clap
/// accepts everything the engine accepts. It did not: [`Priority::parse`] trims
/// and `PossibleValuesParser` does not, so `--priority " H"` went from working to
/// `invalid value ' H'`. Nothing announced that, because a narrowing hidden
/// inside a "declare what we already accept" change looks like a no-op in the
/// diff.
///
/// Trimming here rather than dropping the parser, and here rather than in
/// [`Priority::parse`], because the two must agree and the engine is the one with
/// other callers: the JSON API and MCP reach `task.add` without passing through
/// clap at all, so relaxing the engine to match a stricter CLI would change a
/// contract, while relaxing the CLI to match the engine restores one.
///
/// `possible_values` is forwarded, not reimplemented. It is what the help text
/// lists and what `clap_complete` offers as candidates, so a wrapper that
/// swallowed it would silently un-complete the flag this whole parser exists to
/// make completable.
#[derive(Clone)]
struct Trimmed(PossibleValuesParser);

impl clap::builder::TypedValueParser for Trimmed {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        // A non-UTF-8 value is handed on untouched: it cannot match any spelling
        // anyway, and the inner parser's own error names the real problem better
        // than a lossy conversion of it would.
        match value.to_str() {
            Some(s) => self.0.parse_ref(cmd, arg, std::ffi::OsStr::new(s.trim())),
            None => self.0.parse_ref(cmd, arg, value),
        }
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        self.0.possible_values()
    }
}

/// The `memory search --scope` vocabulary, read out of the engine's own
/// [`MEMORY_SCOPES`] for the same reason as [`priority_parser`].
///
/// The engine keeps its runtime check — the JSON API and MCP reach
/// `memory.search` without passing through clap at all, so removing it would
/// open the surface this closes. Both sides read the one constant, so the
/// duplicated *check* cannot become a duplicated *vocabulary*.
fn scope_parser() -> PossibleValuesParser {
    PossibleValuesParser::new(MEMORY_SCOPES)
}

/// The correlation facts `task.start` / `task.done` accept (#12), declared once
/// and flattened into both so the two verbs cannot drift apart.
///
/// These exist because the attribution engine builds its candidate set only from
/// events carrying `client`, `session_id` or `transcript_path`
/// (`attribution.rs:608`): without them a CLI-driven task is never attributed and
/// silently measures zero. Every one is optional and none is read from the
/// environment — `tasqx done 4` must stay a one-word command, and what lands on
/// the event must stay readable off the command line.
///
/// `--session-id` and `--transcript-path` REQUIRE `--client`, enforced here by
/// clap rather than discovered later. The engine selects its transcript parser
/// from `client` alone (`attribution.rs:367`); given the other two without it,
/// attribution takes that early return and stores a zero-sample marker, which
/// `has_attributed_event` then makes permanent. So the clientless form is not a
/// weaker measurement, it is a silent refusal to measure that also poisons the
/// task against a later, correct attempt — the D33 shape: a value that changes
/// nothing must not answer `ok`. `--prompt-id` is exempt: it is pure correlation
/// metadata and drives no parser selection.
#[derive(Args, Clone, Default)]
pub(super) struct CorrelationArgs {
    /// Calling tool as "<name> <version>", e.g. "claude-code 2.1". Selects the
    /// transcript parser; without it no transcript is ever read.
    #[arg(long, value_name = "TOOL")]
    pub(super) client: Option<String>,

    /// Agent session id, verified against the transcript to earn high confidence.
    /// Requires --client.
    #[arg(long, value_name = "ID", requires = "client")]
    pub(super) session_id: Option<String>,

    /// Id of the prompt/turn driving this call.
    #[arg(long, value_name = "ID")]
    pub(super) prompt_id: Option<String>,

    /// Absolute path to the session transcript the tokens will be found in.
    /// Requires --client.
    #[arg(
        long,
        value_name = "PATH",
        value_hint = ValueHint::FilePath,
        requires = "client"
    )]
    pub(super) transcript_path: Option<String>,
}

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
        // The sugar prefix dispatcher: `+`, `project:`, `!` and the value keys.
        // Both positionals whose words reach `sugar::parse_add` carry this, and
        // `complete::candidates::tests::every_sugar_positional_offers_sugar`
        // reads clap's arg table and `sugar::SUGAR_POSITIONALS` to fail the
        // build for one that does not. A line comment rather than a doc one:
        // clap renders doc comments into `--help`, and where the candidates come
        // from is not something a user reading help needs.
        #[arg(add = crate::complete::candidates::sugar_words())]
        title: Vec<String>,
        /// File the task under an existing project (see `tasqx projects`).
        #[arg(long, add = crate::complete::candidates::projects())]
        project: Option<String>,
        /// Priority: H, M, or L (or high/medium/low).
        #[arg(long, short, value_parser = priority_parser(), ignore_case = true)]
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
        // `value_name` is what brings this into
        // `every_tag_valued_arg_offers_tag_names`'s scope; the derive would
        // otherwise render `<TAGS>` off the plural field name and the guard
        // would silently not cover it.
        #[arg(
            long = "tag",
            short = 't',
            value_name = "TAG",
            add = crate::complete::candidates::tags()
        )]
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
        // Every positional in this tree that takes a task reference carries
        // this, and `complete::candidates::tests::every_task_id_positional_offers_ids`
        // reads clap's own arg table to fail the build for one that does not.
        // A line comment rather than a doc one: clap renders doc comments into
        // `--help`, and where the candidates come from is not something a user
        // reading help needs. The reasoning — why the title travels with the id,
        // why one provider serves `done` and `reopen` alike — is in
        // `candidates::task_ids`.
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
        /// New title words and/or inline sugar (due:friday, +tag, project:p,
        /// !high, est:4h, repeat:"every week", remind:-1h). Bare words become the
        /// new title; omit them to leave the title alone.
        // The same dispatcher `add`'s title carries, for the same reason: this
        // is the other half of `sugar::SUGAR_POSITIONALS`.
        #[arg(add = crate::complete::candidates::sugar_words())]
        rest: Vec<String>,
        /// Move the task to an existing project (see `tasqx projects`).
        #[arg(long, add = crate::complete::candidates::projects())]
        project: Option<String>,
        /// Priority: H, M, or L (or high/medium/low).
        #[arg(long, short, value_parser = priority_parser(), ignore_case = true)]
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
        #[arg(
            long = "tag",
            short = 't',
            value_name = "TAG",
            add = crate::complete::candidates::tags()
        )]
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
        //
        // `filter_words()` and not a bare `ArgValueCompleter`: the pre-pass
        // above escapes into this very positional, so a completer that does not
        // restore the dash is handed `\u{1}ne` where the user typed `-ne` and
        // answers nothing for every tag exclusion. `complete::escaping_drift`
        // fails the build for one, and requires this attachment to exist —
        // membership comes from `argv::FILTER_COMMANDS`, so a filter command
        // added tomorrow is a red build until it is completed too.
        #[arg(add = crate::complete::candidates::filter_words())]
        filter: Vec<String>,
    },
    /// Start a task timer (maps to task.start).
    #[command(alias = "s", after_help = crate::cmddoc::after_help("start"))]
    Start {
        /// short_id or UUID.
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
        /// Keep other active tasks running (opt out of single-active).
        #[arg(long)]
        keep: bool,
        #[command(flatten)]
        correlation: CorrelationArgs,
    },
    /// Stop the task timer (maps to task.stop).
    #[command(alias = "st", after_help = crate::cmddoc::after_help("stop"))]
    Stop {
        /// short_id or UUID.
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
    },
    /// Complete a task (maps to task.done).
    #[command(alias = "d", alias = "x", alias = "complete", after_help = crate::cmddoc::after_help("done"))]
    Done {
        /// short_id or UUID.
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
        #[command(flatten)]
        correlation: CorrelationArgs,
    },
    /// Show a task's full detail incl. tags/annotations/deps (maps to task.get).
    #[command(alias = "get", after_help = crate::cmddoc::after_help("show"))]
    Show {
        /// short_id or UUID.
        #[arg(add = crate::complete::candidates::task_ids())]
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
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
    },
    /// Reopen a done/cancelled task (maps to task.reopen).
    #[command(after_help = crate::cmddoc::after_help("reopen"))]
    Reopen {
        /// short_id or UUID.
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
    },
    /// Annotate a task (maps to annotation.add).
    #[command(alias = "note", after_help = crate::cmddoc::after_help("annotate"))]
    Annotate {
        /// short_id or UUID.
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
        /// The annotation text.
        text: Vec<String>,
    },
    /// Add a dependency: <ref> depends on <depends_on> (maps to dependency.add).
    #[command(after_help = crate::cmddoc::after_help("dep"))]
    Dep {
        /// The dependent task (short_id or UUID).
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
        /// The task it depends on (short_id or UUID).
        #[arg(add = crate::complete::candidates::task_ids())]
        depends_on: String,
    },
    /// Remove a dependency (maps to dependency.remove).
    #[command(after_help = crate::cmddoc::after_help("undep"))]
    Undep {
        /// The dependent task (short_id or UUID).
        #[arg(add = crate::complete::candidates::task_ids())]
        r#ref: String,
        /// The task it depended on (short_id or UUID).
        #[arg(add = crate::complete::candidates::task_ids())]
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
        //
        // `value_name` is what makes this reachable by
        // `every_project_valued_arg_offers_project_names`: the guard decides
        // membership by the name the argument ANNOUNCES for its value, exactly
        // as `every_path_shaped_arg_declares_how_to_complete_it` does for
        // PATH/FILE/DIR, rather than by a list of verbs kept in a test. Without
        // it this positional would announce `<NAME>` and drop out of the guard's
        // scope while still very much taking a project name.
        //
        // `init` deliberately stays `<NAME>`: it CREATES a project, so the
        // existing ones are precisely the names it will refuse.
        #[arg(value_name = "PROJECT", add = crate::complete::candidates::projects())]
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
        //
        // `report_words()` and not `filter_words()`: this is the one filter
        // positional whose FIRST word may be something else, and `report_params`
        // is where that is decided. Offering only filter tokens would leave the
        // three axes — a closed vocabulary the tool knows exactly — unoffered at
        // the position they are legal in.
        #[arg(add = crate::complete::candidates::report_words())]
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
        #[arg(
            long,
            value_name = "PATH",
            value_hint = ValueHint::FilePath,
            requires = "html"
        )]
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
    /// Token accounting maintenance (DESIGN.md §12-D50).
    #[command(after_help = crate::cmddoc::after_help("tokens"))]
    Tokens {
        #[command(subcommand)]
        action: TokensAction,
    },
    /// Export tasks as canonical JSON (maps to store.export).
    #[command(after_help = crate::cmddoc::after_help("export"))]
    Export {
        /// Optional filter DSL.
        ///
        /// Hyphen-tolerant via the `argv` pre-pass so `-tag` is typable; see
        /// `List::filter`.
        #[arg(add = crate::complete::candidates::filter_words())]
        filter: Vec<String>,
    },
    /// Import tasks from a file, or `-` for stdin (maps to store.import).
    #[command(after_help = crate::cmddoc::after_help("import"))]
    Import {
        /// Path to a canonical JSON file (array of tasks), or `-` for stdin.
        // A line comment, not a doc one: clap renders doc comments into
        // `--help`, and why a hint was chosen is not something a user reading
        // help needs. `FilePath` even though `-` is also accepted — a shell
        // offering the stdin sentinel would be noise, and a user who wants it
        // types one character.
        #[arg(value_hint = ValueHint::FilePath)]
        file: String,
    },
    /// Print the single highest-urgency unblocked task (the "what now" button).
    #[command(after_help = crate::cmddoc::after_help("next"))]
    Next,
    /// Explain a task's urgency breakdown (maps to task.get + the D1 formula).
    #[command(after_help = crate::cmddoc::after_help("why"))]
    Why {
        /// short_id or UUID.
        #[arg(add = crate::complete::candidates::task_ids())]
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
        #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
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
        #[arg(add = crate::complete::candidates::filter_words())]
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
        #[arg(
            long,
            value_name = "PATH",
            value_hint = ValueHint::FilePath,
            conflicts_with = "stdout"
        )]
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
    /// Print — or install — the line that turns on Tab completion for tasqx.
    ///
    /// With no flags it writes one line to stdout, so
    /// `tasqx completions bash >> ~/.bashrc` works. `--install` edits the
    /// shell's own startup file for you, inside a marked block it can take back
    /// out again, and asks before writing anything.
    #[command(after_help = crate::cmddoc::after_help("completions"))]
    Completions {
        /// Which shell (bash, elvish, fish, powershell, zsh). Detected from
        /// $SHELL when omitted; on Windows nothing sets $SHELL, so name it.
        // Deliberately NOT a `value_parser` over the five names, although that
        // is the shorter spelling. clap's rejection would read `invalid value
        // 'cmd' for '[SHELL]'`, and `complete::install::NON_GOALS` exists
        // precisely because "unknown shell cmd" is the wrong answer: cmd.exe
        // cannot be completed by any program, ever, and a user who is told it
        // is merely unknown goes looking for a newer tasqx. The refusal is
        // owned by `resolve_shell`, which can say so.
        #[arg(add = crate::complete::install::shells())]
        shell: Option<String>,
        /// Add the activation line to the shell's startup file (asks first).
        #[arg(long)]
        install: bool,
        /// Take the block a previous --install added back out again.
        #[arg(long, conflicts_with = "install")]
        uninstall: bool,
        /// The file to edit instead of the shell's default startup file.
        /// Required for PowerShell: run it as `--profile $PROFILE` and let
        /// PowerShell expand the path it alone knows.
        #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
        profile: Option<String>,
        /// Confirm the edit on the command line. Required when stdin is not a
        /// terminal — a pipeline has nobody to ask, and this feature will not
        /// edit a startup file on a guess.
        #[arg(long, short = 'y')]
        yes: bool,
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
        /// Number of weeks to show (1-520; default 12).
        // Weekly is the only bucketing — the spec's `--weekly` flag was parsed
        // and dropped for two releases, so it is gone rather than documented.
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
        // The archived-inclusive provider, unlike every other `--project`. This
        // is a READ and the engine really does chart an archived project, so the
        // narrow set would offer less than the command accepts; `add`, `modify`
        // and `use` all refuse an archived project outright, which is why they
        // take the narrow one. See `candidates::projects`.
        #[arg(long, add = crate::complete::candidates::projects_including_archived())]
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
    /// Print which store this command would actually write to.
    Store,
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
        /// What to search (default: all).
        // The spellings used to be listed in the line above as well. Clap now
        // prints `[possible values: ...]` from the parser itself, and two
        // renderings of one vocabulary in one help entry is the smaller version
        // of the drift this whole change is about.
        #[arg(long, value_parser = scope_parser())]
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
        // `AnyPath`, not `FilePath`: this verb genuinely takes either, and
        // `FilePath` sorts every directory below every file in the listing —
        // backwards for the form most people use.
        #[arg(value_hint = ValueHint::AnyPath)]
        path: String,
    },
}

#[derive(Subcommand)]
pub(super) enum TokensAction {
    /// Re-run log-parse attribution over stored history under the D50 refusal
    /// rule (maps to tokens.recompute): samples claimed by more than one task's
    /// window drop out, and a task whose transcript is gone keeps its counts
    /// with confidence downgraded to low. Dry-run by default: prints the
    /// per-task delta and writes nothing.
    Recompute {
        /// Actually write the repair. The dry-run default is not a convenience
        /// but the safety: this is the one verb in the API built to delete
        /// measurement rows, so destruction must be asked for by name.
        #[arg(long)]
        apply: bool,
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
    use clap::CommandFactory;

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

    /// A closed vocabulary that clap does not know about is invisible twice
    /// over: no shell can complete it, and the rejection arrives from the engine
    /// as `invalid priority: "bogus"` — a message that names the mistake and not
    /// the way out. Declared, the same typo fails at parse time listing what
    /// would have worked.
    #[test]
    fn a_bad_priority_is_refused_at_parse_time_naming_the_accepted_values() {
        for argv in [
            &["tasqx", "add", "x", "--priority", "bogus"][..],
            &["tasqx", "modify", "4", "--priority", "urgent"][..],
        ] {
            let err = Cli::try_parse_from(argv)
                .err()
                .unwrap_or_else(|| panic!("{argv:?} must be rejected"));
            assert_eq!(err.kind(), ErrorKind::InvalidValue, "{argv:?}");
            let msg = err.to_string();
            for want in ["H", "M", "L"] {
                assert!(
                    msg.contains(want),
                    "{argv:?}: the error must name {want:?}, got {msg:?}"
                );
            }
        }
    }

    /// The other half, and the one a `value_parser` can silently break: every
    /// spelling the engine accepts must still reach it. Derived from
    /// `Priority::SPELLINGS` rather than typed out, so a spelling added to the
    /// engine and not to clap fails here instead of becoming a documented form
    /// the CLI rejects.
    ///
    /// The padded forms are here because this guard once tested only the
    /// untrimmed spellings and therefore missed a real narrowing: `Priority::parse`
    /// opens with `s.trim()`, so `--priority " H"` and `--priority "high "` parsed
    /// before the `value_parser` landed and were rejected after it, while the
    /// change was described as purely additive. A guard that only feeds a parser
    /// the inputs it obviously handles cannot notice the edge being cut off, so
    /// every spelling is now asserted in every shape the ENGINE accepts — the
    /// engine being the definition of "accepted", since the JSON API and MCP reach
    /// it without passing through clap.
    #[test]
    fn every_engine_priority_spelling_still_parses_through_clap() {
        for (spelling, _) in Priority::SPELLINGS {
            for cased in [
                spelling.to_string(),
                spelling.to_ascii_uppercase(),
                spelling.to_ascii_lowercase(),
            ] {
                for form in [
                    cased.clone(),
                    format!(" {cased}"),
                    format!("{cased} "),
                    format!("  {cased}\t"),
                ] {
                    // The engine is the authority: assert it accepts the form
                    // first, so this can never demand more of clap than the tool
                    // actually supports.
                    assert!(
                        Priority::parse(&form).is_some(),
                        "precondition: the engine accepts --priority {form:?}"
                    );
                    assert!(
                        Cli::try_parse_from(["tasqx", "add", "x", "--priority", &form]).is_ok(),
                        "--priority {form:?} parses in the engine and must parse here \
                         (is `ignore_case` still set, and is the parser still `Trimmed`?)"
                    );
                }
            }
        }
    }

    /// Trimming must not become "accept anything with a real value inside it".
    /// `Priority::parse` trims and nothing more, so the CLI must too — otherwise
    /// the pair drifts again, in the other direction this time.
    #[test]
    fn trimming_does_not_widen_the_priority_vocabulary() {
        for bogus in ["h i g h", "h-", "-H", "H,M", ""] {
            assert!(
                Priority::parse(bogus).is_none(),
                "precondition: the engine rejects --priority {bogus:?}"
            );
            assert!(
                Cli::try_parse_from(["tasqx", "add", "x", "--priority", bogus]).is_err(),
                "--priority {bogus:?} is refused by the engine and must be refused here"
            );
        }
    }

    /// Same shape for `memory search --scope`, whose vocabulary lives in
    /// `MEMORY_SCOPES` and is enforced by the engine for API and MCP callers who
    /// never touch clap.
    #[test]
    fn memory_search_scope_is_a_closed_set_matching_the_engines() {
        for scope in MEMORY_SCOPES {
            assert!(
                Cli::try_parse_from(["tasqx", "memory", "search", "x", "--scope", scope]).is_ok(),
                "{scope} is an engine scope and must parse"
            );
        }
        let err = Cli::try_parse_from(["tasqx", "memory", "search", "x", "--scope", "bogus"])
            .err()
            .expect("an unknown scope must be refused");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
    }

    /// Drift guard: an arg whose value name announces a filesystem path must
    /// declare how to complete one.
    ///
    /// Without a `ValueHint`, clap's completion engine takes the
    /// `ValueHint::Unknown` arm and offers nothing at all — so `tasqx docs --out
    /// <TAB>` was silent where every other tool on the system offers files. That
    /// is a per-arg omission with no symptom, which is exactly the shape this
    /// repo keeps getting bitten by, so it is read out of clap's own arg table
    /// rather than kept as a list here. Same technique as
    /// `argv::no_declared_short_flag_is_ever_escaped`, and for the same reason:
    /// an arg declared tomorrow is covered the day it is declared.
    ///
    /// The rule is a naming convention, and it is enforced in the direction that
    /// matters — it cannot know that some arg is secretly a path, but it can
    /// insist that one *announcing* itself as PATH/FILE/DIR says what to do with
    /// it. `--socket` is deliberately outside it: on unix its value is a socket
    /// path, on Windows a named pipe, and its value name says SOCKET rather than
    /// pretending the two are the same thing.
    #[test]
    fn every_path_shaped_arg_declares_how_to_complete_it() {
        const PATH_VALUE_NAMES: [&str; 3] = ["PATH", "FILE", "DIR"];

        fn walk(cmd: &clap::Command, trail: &str, missing: &mut Vec<String>) {
            for arg in cmd.get_arguments() {
                let name = arg
                    .get_value_names()
                    .and_then(|n| n.first().map(|s| s.as_str().to_string()))
                    .unwrap_or_else(|| arg.get_id().as_str().to_ascii_uppercase());
                if !PATH_VALUE_NAMES.contains(&name.as_str()) {
                    continue;
                }
                if arg.get_value_hint() == clap::ValueHint::Unknown {
                    missing.push(format!("{trail} {} <{name}>", arg.get_id()));
                }
            }
            for sub in cmd.get_subcommands() {
                walk(sub, &format!("{trail} {}", sub.get_name()), missing);
            }
        }

        let mut cmd = Cli::command();
        cmd.build();
        let mut missing = Vec::new();
        walk(&cmd, "tasqx", &mut missing);
        assert!(
            missing.is_empty(),
            "these args name a filesystem path but declare no ValueHint, so no \
             shell will complete them: {missing:#?}"
        );

        // The guard is only worth anything if it is looking at something. If a
        // refactor renamed every value away from the convention this would pass
        // vacuously and protect nothing.
        let mut seen = 0usize;
        fn count(cmd: &clap::Command, seen: &mut usize) {
            *seen += cmd
                .get_arguments()
                .filter(|a| a.get_value_hint() != clap::ValueHint::Unknown)
                .count();
            for sub in cmd.get_subcommands() {
                count(sub, seen);
            }
        }
        count(&cmd, &mut seen);
        assert!(
            seen >= 6,
            "expected the path-taking args to still carry hints, found {seen}"
        );
    }
}
