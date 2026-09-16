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

mod about;
pub mod ansi_html;
mod argv;
mod backend;
mod chart;
mod clock;
pub mod cmddoc;
mod columns;
mod command;
mod complete;
pub mod config;
mod dashboard_screen;
mod docs;
mod docs_open;
pub mod fixtures;
mod html;
mod manual;
mod memory_screen;
mod pick_screen;
mod render;
mod serve;
mod settings;
mod sugar;
mod theme;
mod tokens;
mod tui;
mod verbs;
use backend::*;
use dashboard_screen::*;
use docs_open::*;
use memory_screen::*;
use pick_screen::*;
use serve::*;
use settings::*;
use verbs::*;

/// The built-in theme names, re-exported for the README drift guard
/// (`tests/readme.rs`): the README claims a built-in theme count, and a count
/// nothing binds is the "Twenty-six verbs" bug waiting to happen again.
pub use theme::BUILTINS as THEME_BUILTINS;

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::error::{ContextKind, ContextValue, ErrorKind};
#[cfg(test)]
use clap::Parser;
use serde_json::{json, Value};

use tasqx_core::markdown::{Borders, CardOpts, DetailOpts, TimeFormat};
use tasqx_core::{
    daemon, datetime, dispatch, handle_envelope, notify, ApiError, Engine, ErrorCode, McpServer,
    Scope,
};

use command::{
    cli_command, ChartKind, CheckAction, Cli, Command, ConfigAction, McpAction, MemoryAction,
    ThemeAction, TokensAction,
};
use theme::{Caps, Ctx};

/// The reference instant handed to the natural-language date parser — the
/// clock as [`clock::now`] answers it, so `$TASQX_NOW` pins `due:tomorrow` the
/// same way it pins what a rendered row says about it.
fn now_ts() -> jiff::Timestamp {
    clock::now()
}

/// Fields `modify --clear` may unset (DESIGN.md §12-D13).
///
/// `title` is absent on purpose and clap therefore rejects `--clear title` with
/// the list of what *is* clearable: a task with no title is not a task, and the
/// core would reject the null anyway — better to say so at parse time than to
/// round-trip a `bad_request`. `status` is absent for the same reason it is not
/// a general modify field: lifecycle moves through start/stop/done/cancel so
/// their invariants hold (D6).
const CLEARABLE: [&str; 10] = [
    "project",
    "priority",
    "due",
    "scheduled",
    "wait",
    "remind",
    "recurrence",
    "estimate",
    "tracked",
    "budget_tokens",
];

/// How far ahead `tasqx agenda` looks when `--days` is not given.
///
/// A fortnight, not a week and not a month. A week ends on a boundary a reader
/// is standing on top of — on a Friday it shows two working days — so the one
/// question the view exists to answer ("is next week already full?") is exactly
/// the one it cannot answer. A month puts thirty headings on the screen for a
/// store that plans a fortnight out, and the rows worth acting on scroll off the
/// top. Fourteen days always contains a whole next week from any day of the
/// current one.
///
/// It is a default and not a rule: `--days` moves it, and anything the horizon
/// cut is COUNTED and reported with the exact `--days` that would reach it, so
/// the number here can be wrong for a given store without anything being hidden.
const AGENDA_DEFAULT_DAYS: usize = 14;

/// The widest window `--days` accepts, and therefore the furthest `agenda` can
/// reach at all.
///
/// It lives at the crate root because it has two readers in two layers that
/// `command_declarations_do_not_execute_or_render` forbids from importing each
/// other: `command::window_parser` refuses a larger value at parse time, and
/// `render::Agenda::omissions` has to know when the `--days` it is about to
/// RECOMMEND is one the parser would refuse. When those were separate literals
/// they were free to disagree, and the footer duly recommended
/// ``tasqx agenda --days 12204`` for a task due in 2060 — a command that exits 2
/// with `12204 is not in 1..=3650`, leaving that row unreachable in the view and
/// D53 rule 2's "widening the window is a paste rather than a guess" false. One
/// copy, read by both, is the fix; `command::tests` pins the parser boundary and
/// the `--days` help prose to it, and `render::tests` pins the footer to it.
///
/// A decade is already past every horizon anyone plans against, and the value is
/// bounded at all because `render::agenda_select` adds the window to today with
/// `jiff`'s `ToSpan::days`, which PANICS outside ±7,304,484 — an unbounded flag
/// is an abort whose message names neither tasqx nor the flag.
const AGENDA_MAX_DAYS: usize = 3650;

/// What `--version` prints: the crate version plus the commit it was built from.
///
/// The commit is the load-bearing half. `CARGO_PKG_VERSION` is identical across
/// every build between releases, so it cannot distinguish a freshly installed
/// binary from a stale one; `TASQX_BUILD_ID` (see `build.rs`) can. `concat!`
/// rather than `format!` because clap wants a `&'static str` and this is fully
/// known at compile time.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("TASQX_BUILD_ID"), ")");

/// Report a clap parse failure and exit.
///
/// `--help` and `--version` arrive here as "errors" and must print normally, so
/// everything falls through to clap unless it is the one case worth taking
/// over: an unknown flag on a filter-taking command. Clap calls that an
/// "unexpected argument" and tips the reader to pass it as a value with `--` —
/// advice that turns a typo into filter text, which is the silent widening of
/// the result set this CLI refuses. `filter.rs` already has the right words
/// (name the flag, say a tag exclusion takes one dash, list the tokens that
/// work), so they are borrowed rather than copied and left to drift.
fn exit_on_parse_error(e: &clap::Error, filter_command: bool, argv: &[std::ffi::OsString]) -> ! {
    if filter_command && e.kind() == ErrorKind::UnknownArgument {
        if let Some(ContextValue::String(offender)) = e.get(ContextKind::InvalidArg) {
            // #130: `--output` one edit away from `--out` fell straight to the
            // filter-DSL explanation below ("a tag exclusion takes one dash…"),
            // which is correct grammar but the wrong story — the token is not
            // a mistyped filter token, it is a mistyped FLAG NAME. Checked
            // first and only for `--xxx`-shaped offenders close to a real flag
            // this subcommand declares; anything else still falls through.
            if let Some(hint) = argv::nearest_long_flag(offender, &argv::known_long_flags(argv)) {
                let err = ApiError::bad_request(format!(
                    "unknown flag {offender:?} — did you mean \"--{hint}\"?"
                ));
                eprintln!("error [{}]: {}", code_str(&err), err.message);
                exit(err.exit_code());
            }
            if let Some(msg) = argv::filter_flag_error(offender) {
                let err = ApiError::bad_request(msg);
                eprintln!("error [{}]: {}", code_str(&err), err.message);
                exit(err.exit_code());
            }
        }
    }
    e.exit()
}

/// The commands that do NOT honour `--json`, and the reason each may not.
///
/// DESIGN.md's opening promise is that every command speaks human-readable text
/// *and* `--json`. These are the declared exceptions, and they are all one kind
/// of exception: each frames its own I/O, so there is no single result value to
/// hand a machine when it finishes — three speak another protocol outright, one
/// never terminates, one is prose for a human to read.
///
/// This table is load-bearing, not documentation. `Exit::self_framed` is the
/// only way to reach a terminal without consulting `--json`, and it refuses a
/// name that is not listed here, so a future early `return` cannot invent a
/// silent carve-out. `tests/json_contract.rs` closes the other direction: it
/// derives the command list from clap and drives every command that is *not*
/// listed here through the real binary, asserting it emits JSON.
pub const JSON_CARVE_OUTS: &[(&str, &str)] = &[
    (
        "about",
        "a credits screen: a name, two links and this build, with nothing a machine reads",
    ),
    (
        "api",
        "already speaks the JSON API response envelope; --json would double-wrap it",
    ),
    (
        "mcp",
        "speaks JSON-RPC over stdio to an agent, framed by the protocol",
    ),
    (
        "daemon",
        "a server: stdout is diagnostics, results travel over the socket",
    ),
    (
        "watch",
        "a live stream that re-renders until interrupted; it has no final result",
    ),
    (
        "manual",
        "a human reading surface: themed prose, no machine-relevant facts",
    ),
];

/// Every subcommand clap knows, derived from the parser rather than listed, so
/// a new command joins the `--json` contract guard on the day it is added (D30).
pub fn subcommand_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect()
}

/// The fields `modify --clear` accepts, derived from the parser's own list.
///
/// Exported for the same reason [`subcommand_names`] is. `CLEARABLE` is
/// private and `tests/` is a separate crate, so a wiki page restating the set
/// could not be bound to it from there — and it drifted exactly that way,
/// listing eight of the nine.
pub fn clearable_fields() -> Vec<&'static str> {
    CLEARABLE.to_vec()
}

/// The dashboard panels a reader can ask for, in the built-in order.
///
/// The roster D80 left: prose that counts panels must count these.
pub fn dashboard_panel_names() -> Vec<&'static str> {
    tui::dashboard::model::PANEL_NAMES.to_vec()
}

/// The panels `tasqx --json dashboard` writes a payload for.
///
/// Deliberately separate from [`dashboard_panel_names`]: the screen's roster
/// and the document's are different tables and different lengths, and prose
/// that counts "panels" has to say which one it means. Pinning the `--json`
/// sentence to the screen's six was the second wrong number that claim had.
pub fn dashboard_json_panel_names() -> Vec<&'static str> {
    tui::dashboard::json::PAYLOAD_PANELS.to_vec()
}

/// The panel names D80 retired, every one of which now resolves to `tasks`.
///
/// Exported so prose cannot go on describing one of them as a panel that
/// ships. Three documents were still sending readers to look for BLOCKED.
pub fn retired_dashboard_panel_names() -> Vec<&'static str> {
    tui::dashboard::model::RETIRED_PANEL_NAMES.to_vec()
}

/// Every `tasqx memory` subcommand clap knows, derived like [`subcommand_names`].
///
/// The wiki documented five of the seven: `list` (a whole screen) and
/// `update` (the non-destructive correction path) appeared on no page.
pub fn memory_subcommand_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .find_subcommand("memory")
        .expect("clap has a `memory` subcommand")
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect()
}

/// How a command leaves [`execute`].
///
/// This exists to make the `--json` bypass unrepresentable. Before it, `run()`
/// consulted `cli.json` exactly once — on the outcome of the big
/// `match cli.command` — and half a dozen commands were dispatched by an early
/// `return` *above* that point, so they accepted the flag and ignored it. The
/// early returns are not the problem and are not going away: `docs` must run
/// before `build_ctx` so a broken theme cannot block reading the docs, and
/// `theme` must run before the engine opens because it needs no store. What was
/// missing was a type that says "and then you still owe the caller a result".
///
/// Every path out of `execute` now yields one of these two, so the compiler —
/// not a reviewer's memory — is what keeps a new early `return` honest.
enum Exit {
    /// A machine-relevant result plus its human rendering. The single terminal
    /// in [`run`] picks between them by `--json`.
    Out(CmdOutcome),
    /// A declared carve-out from [`JSON_CARVE_OUTS`]: this command frames and
    /// writes its own output, and there is nothing left to render.
    SelfFramed,
}

impl Exit {
    /// The only way to build a non-JSON terminal, and it refuses a name that is
    /// not on the declared list. A future early `return` therefore cannot invent
    /// a silent carve-out — it either produces a result or fails here loudly.
    ///
    /// Also the place the second half of the contract is kept: accepting a flag
    /// and ignoring it is the bug this whole change is about, so when `--json`
    /// reaches a carve-out we say so, and say why. On stderr, because three of
    /// these five commands have a protocol on stdout that a note would corrupt.
    ///
    /// Called BEFORE the command runs, not after: `daemon` and `watch` do not
    /// return until they are interrupted, and a warning delivered then is a
    /// warning nobody reads.
    fn self_framed(name: &'static str, json: bool) -> Exit {
        let reason = JSON_CARVE_OUTS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, why)| *why);
        let reason = reason.unwrap_or_else(|| {
            panic!(
                "`{name}` framed its own output but is not a declared --json carve-out; \
                 add it to JSON_CARVE_OUTS with a reason, or return Exit::Out"
            )
        });
        if json {
            eprintln!("note: `{name}` does not honour --json — {reason}");
        }
        Exit::SelfFramed
    }
}

thread_local! {
    /// Lines a verb wants on stderr AFTER its stdout rendering: see
    /// [`note_after_output`].
    static NOTES_AFTER: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Queue a note for stderr, printed once the command's stdout rendering has
/// been written (D126). A write's echo is a card, and the card is the first
/// thing under the prompt; a note printed while the verb runs would land above
/// it on a terminal, because stderr is written at once and stdout only at the
/// end of [`run`]. Dropped under `--json`, where the result carries the same
/// text in full.
pub(crate) fn note_after_output(note: String) {
    NOTES_AFTER.with(|n| n.borrow_mut().push(note));
}

fn take_notes() -> Vec<String> {
    NOTES_AFTER.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

/// Write a finished rendering to stdout, tolerating a reader that stops early.
///
/// NOT `print!`: that panics if stdout closes mid-write, and several of the
/// things that pass through here are large enough for the downstream reader to
/// close the pipe first — `tasqx docs --stdout | head` is ~87KB, and
/// `tasqx --json export | head` is unbounded. Closing a pipe early is a normal
/// shell idiom, not a crash, so BrokenPipe is success and every other write
/// error is the real error it is.
///
/// This lived inside `docs --stdout` alone, which is why it protected exactly
/// one of the commands that needed it. Then it lived on the `Exit::Out`
/// terminal alone, which is why `tasqx watch | head`, `manual`, and `api` —
/// the self-framed owners of stdout — still panicked: every stdout write
/// routes through here or [`emit_open`], not through `print!`.
fn emit(text: &str) {
    let _ = emit_open(text);
}

/// Like [`emit`], but reports whether the reader is still attached: `false`
/// means the pipe closed. A one-shot caller can ignore it (that is [`emit`]);
/// a streaming loop (`watch`) must not, or it keeps writing into a dead pipe
/// until killed instead of ending with the reader.
fn emit_open(text: &str) -> bool {
    match emit_via(&mut std::io::stdout(), text) {
        Ok(open) => open,
        Err(e) => {
            eprintln!("error: cannot write to stdout: {e}");
            exit(1);
        }
    }
}

/// The tolerant write itself, over any writer so the classification is
/// testable without a real process's stdout: `Ok(true)` written, `Ok(false)`
/// reader closed the pipe, `Err` a real write error.
fn emit_via(out: &mut impl Write, text: &str) -> std::io::Result<bool> {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e),
    }
}

/// Restore the dashes [`argv::prepass`] hid, on whichever positional tail this
/// command spells its filter in.
///
/// A named function and not three lines inside [`run`], for one reason: this is
/// the un-escape half of a PAIR, and the escape half (`argv::FILTER_COMMANDS`)
/// has a guard while this half, inline, had none. `pick` shipped in
/// `FILTER_COMMANDS` and not in the match, so `tasqx pick -api` built the
/// filter string `"\u{1}api"` and the whole suite stayed green. Now
/// `every_filter_command_gets_its_dashes_back` calls THIS function — not a copy
/// of its body — for every name in `FILTER_COMMANDS`.
fn unescape_filter_tail(cli: &mut Cli) {
    if let Some(tail) = cli.command.as_mut().and_then(Command::filter_tail_mut) {
        argv::unescape(tail);
    }
}

pub fn run() {
    // FIRST, before anything reads argv or writes a byte of stdout. Unless
    // `$TASQX_COMPLETE` is set this is one environment lookup and a return; when it is
    // set, the process serves the shell's Tab press and exits without ever
    // reaching the parse below, the backend, or the dispatcher. The completion
    // words get their own `argv::prepass` inside — see `complete::prepassed` for
    // why it cannot simply reuse the one on the next line.
    complete::intercept();

    // SECOND, and still before argv is parsed or any store is opened: a
    // `TASQX_NOW` nobody can read is fatal for every command, not only for the
    // ones that ask what time it is. `add` used to write its row — the ENGINE
    // stamps and commits it — and exit 2 afterwards while rendering, and
    // `about`, `docs`, `completions` and `daemon` never noticed the broken
    // environment at all.
    clock::validate();

    // Not `Cli::parse()`: filter tokens like `-needs` must reach the grammar,
    // and the only way to keep that from disarming clap's flag handling is to
    // hide the dash before clap looks. See `argv`.
    let pre = argv::prepass(std::env::args_os());
    // Cloned once for the error path only (#130's flag-typo hint needs the
    // subcommand's own argv to look up its declared flags); the happy path
    // never pays for it beyond the clone itself, and `try_get_matches_from`
    // still consumes the original below.
    let pre_argv = pre.argv.clone();
    // Not `Cli::try_parse_from`: that builds straight off `Cli::command()`,
    // whose subcommands still carry clap's hyphen-joined `-V` display names
    // (#228.7). `cli_command()` is the same command tree with those flattened
    // to `tasqx` first.
    let mut cli = match cli_command().try_get_matches_from(pre.argv).and_then(|m| {
        use clap::FromArgMatches;
        Cli::from_arg_matches(&m)
    }) {
        Ok(cli) => cli,
        Err(e) => exit_on_parse_error(&e, pre.filter_command, &pre_argv),
    };
    // Put the dashes back, in ONE place, before any filter value is read.
    unescape_filter_tail(&mut cli);

    // Read before `cli` is moved into `execute`, which consumes it by value.
    let json = cli.json;
    let occasion = hint_occasion(&cli);

    // THE terminal. Every command reaches exactly this point, whether it was
    // dispatched early or fell through to the bottom match, which is what makes
    // "honours --json unless declared otherwise" a property of the code shape
    // rather than a promise five call sites have to keep independently.
    match execute(cli) {
        Exit::SelfFramed => {}
        Exit::Out(Ok((result, render))) => {
            if json {
                emit(&format!(
                    "{}\n",
                    serde_json::to_string_pretty(&result).unwrap_or_default()
                ));
            } else {
                emit(&render);
            }
            let notes = take_notes();
            if !json {
                for note in notes {
                    eprintln!("{note}");
                }
            }
            // D57, and it sits HERE for the same reason the JSON terminal does:
            // one place every command passes through. Only on the success arm —
            // `Exit::Out(Err)` exits above, and a nudge printed under an error
            // message competes with the thing the user is actually reading —
            // and never for `SelfFramed`, whose members own stdout (a protocol),
            // never return (`daemon`, `watch`), or are prose the user asked for.
            if let Some(occasion) = occasion {
                complete::hint::offer(occasion, json);
            }
        }
        Exit::Out(Err(e)) => {
            // `--json` was honoured on the success arm only (#194): every
            // other command's error path printed English to stderr and left
            // stdout empty, so `tasqx --json show 999 | jq` failed with a jq
            // parse error instead of a diagnosable object, and the structured
            // `data` block naming the offending argument — present on the
            // identical failure through `tasqx api` — was unreachable from
            // the CLI at all. This mirrors that same envelope shape on
            // stdout, keeps the human line on stderr only when `--json` is
            // absent (a JSON body under a JSON body would be the API's
            // problem restated), and leaves the exit code exactly as before.
            if json {
                let body = tasqx_core::error::ErrorBody::from(&e);
                emit(&format!(
                    "{}\n",
                    serde_json::to_string(&json!({ "ok": false, "error": body }))
                        .unwrap_or_default()
                ));
            } else {
                eprintln!("error [{}]: {}", code_str(&e), e.message);
            }
            exit(e.exit_code());
        }
    }
}

/// Which D57 occasion this invocation is, or `None` for the commands that must
/// never carry the note.
///
/// `completions` is the exclusion that is not about output at all: a user
/// running the verb is already holding the answer the note would give them, and
/// on `--uninstall` the note would contradict what they just deliberately did.
/// `init` is [`complete::hint::Occasion::Setup`] — the one moment a user is
/// reading setup output — and everything else is ordinary.
fn hint_occasion(cli: &Cli) -> Option<complete::hint::Occasion> {
    match &cli.command {
        Some(Command::Completions { .. }) => None,
        Some(Command::Init { .. }) => Some(complete::hint::Occasion::Setup),
        _ => Some(complete::hint::Occasion::Ordinary),
    }
}

/// Run the parsed command, yielding whatever the terminal in [`run`] should do
/// with it. Every `return` in here owes an [`Exit`].
/// The static verb name for a subcommand this crate's inert-flag notes ever
/// name — a small, closed set, not a mirror of clap's whole `Command` enum.
fn verb_name(command: &Option<Command>) -> Option<&'static str> {
    match command {
        Some(Command::Api) => Some("api"),
        Some(Command::Docs { .. }) => Some("docs"),
        Some(Command::About) => Some("about"),
        Some(Command::Manual { .. }) => Some("manual"),
        Some(Command::Completions { .. }) => Some("completions"),
        _ => None,
    }
}

fn execute(cli: Cli) -> Exit {
    // #229 item 6: `--json`'s carve-out note (`JSON_CARVE_OUTS`, D31) explains
    // when the flag is accepted and ignored; `--theme` and `--socket` did the
    // ignoring silently, on the same class of verb — every subcommand's
    // `--help` lists all four globals regardless of whether the verb reads
    // them, so a reader has no way to tell "ignored" from "does something"
    // short of a note like this one. Narrower than a fully generic
    // per-flag-per-verb table: only the combinations the finding names,
    // mirroring `--json`'s wording so the two read as one family of note.
    //
    // `--no-daemon` is deliberately EXEMPT, unlike the finding's suggestion:
    // it is documented as "the escape hatch for scripts" (`command.rs`), and
    // this repo's own convention — `CLAUDE.md`'s isolation rule, and every
    // fixture in `tests/completion.rs` — is to pass it on EVERY invocation
    // defensively, `completions`/`docs`/`manual` included, so a caller never
    // has to know per-verb whether it matters. A note firing on that pattern
    // would make the safe default noisy rather than making the noise
    // informative, which is the opposite of what `--json`'s note is for.
    let note_inert = |verb: &str, flag: &str, why: &str| {
        eprintln!("note: `{verb}` does not honour {flag} — {why}");
    };
    if cli.theme.is_some() {
        let why = match &cli.command {
            Some(Command::Api) => {
                Some("already speaks the JSON API envelope; there is no themed output")
            }
            Some(Command::Completions { .. }) => {
                Some("prints a shell registration line; there is no themed output")
            }
            _ => None,
        };
        if let (Some(why), Some(name)) = (why, verb_name(&cli.command)) {
            note_inert(name, "--theme", why);
        }
    }
    if cli.socket.is_some() {
        let why = match &cli.command {
            Some(Command::Docs { .. }) => Some("static content; it opens no store and no daemon"),
            Some(Command::Manual { .. }) => {
                Some("a reading surface; it opens no store and no daemon")
            }
            Some(Command::About) => Some(
                "a credits screen; it names the store's path and opens neither it nor a daemon",
            ),
            Some(Command::Completions { .. }) => {
                Some("prints a shell registration line; it opens no store and no daemon")
            }
            _ => None,
        };
        if let (Some(why), Some(name)) = (why, verb_name(&cli.command)) {
            note_inert(name, "--socket", why);
        }
    }

    // `--socket` names a daemon to route through, and these verbs open the
    // store without ever consulting it: `api` and `mcp serve` host their own
    // transport over an in-process engine (D73), and charts and the HTML
    // report render from a direct local read. Accepting the flag while
    // writing — or reading — somewhere else is the wrong-store trap with the
    // operator's own routing request in hand, so it is refused with the
    // reason rather than ignored. `$TASQX_SOCK` is deliberately NOT refused
    // on these verbs: an exported variable is ambient, not a per-command
    // request, and refusing it would break every `mcp serve` an MCP host
    // launches into an environment that happens to export it.
    if cli.socket.is_some() {
        let inert = match &cli.command {
            Some(Command::Api) => Some(
                "`api` answers one envelope from stdin against an in-process \
                 engine; it is not a socket client",
            ),
            Some(Command::Mcp { .. }) => Some(
                "`mcp serve` hosts MCP over stdio against an in-process \
                 engine; it is not a socket client",
            ),
            Some(Command::Chart { .. }) | Some(Command::Report { html: true, .. }) => Some(
                "charts and the HTML report render from a direct local read \
                 of the store, never through a daemon",
            ),
            _ => None,
        };
        if let Some(why) = inert {
            return Exit::Out(Err(ApiError::bad_request(format!(
                "--socket is not honoured here: {why} (DESIGN.md D73). Drop \
                 the flag to work on the local store; the plain CLI verbs are \
                 the ones that route through a daemon."
            ))));
        }
    }

    // `api`, `mcp`, and `daemon` are special: they frame their own I/O (response
    // envelopes / JSON-RPC / the socket server) and do not go through the normal
    // render path. `daemon` opens its own Engine and blocks.
    match &cli.command {
        Some(Command::Api) => {
            let exit = Exit::self_framed("api", cli.json);
            run_api();
            return exit;
        }
        Some(Command::Mcp { action }) => {
            let exit = Exit::self_framed("mcp", cli.json);
            run_mcp(action);
            return exit;
        }
        Some(Command::Daemon { db }) => {
            let exit = Exit::self_framed("daemon", cli.json);
            run_daemon(cli.socket.as_deref(), db.as_deref());
            return exit;
        }
        _ => {}
    }

    // `docs` is pure static content — no store, no theme, no network. Handle it
    // before anything that could fail for reasons the reader is trying to look up.
    if let Some(Command::Docs {
        out,
        no_open,
        stdout,
    }) = &cli.command
    {
        return Exit::Out(run_docs(out.as_deref(), *no_open, *stdout));
    }

    // `completions` needs no store, no theme and no network — it prints one
    // line, or edits a file the user names. Dispatched beside `docs` and ahead
    // of `build_ctx` for the same reason: a user who cannot get completion
    // working must not be stopped by a theme that fails to load.
    //
    // It reports failures LOUDLY, on stderr, with a non-zero exit — the
    // ordinary `CmdOutcome` contract, and the deliberate opposite of the
    // silence `complete::intercept` keeps on the Tab path. `complete.rs`'s
    // module doc names this verb as the exemption; `complete/install.rs` writes
    // out why.
    if let Some(Command::Completions {
        shell,
        install,
        uninstall,
        profile,
        yes,
    }) = &cli.command
    {
        return Exit::Out(complete::install::run(
            shell.clone(),
            *install,
            *uninstall,
            profile.clone(),
            *yes,
        ));
    }

    // Build the render context: resolve the active theme (flag > env > config >
    // default) and detect the terminal's real capability (DESIGN.md §8).
    let ctx = build_ctx(cli.theme.as_deref());
    // Captured before `cli.command` is matched by value below; `config`
    // needs it to report the flag layer it would otherwise be blind to.
    let theme_flag = cli.theme.clone();

    // `theme` needs no store; handle it before opening the engine.
    if let Some(Command::Theme { action }) = &cli.command {
        return Exit::Out(run_theme(&ctx, action));
    }

    // `about` needs the themed Ctx and the store PATH, but opens neither a
    // store nor a network; dispatch it beside `manual`.
    if let Some(Command::About) = &cli.command {
        let exit = Exit::self_framed("about", cli.json);
        let facts = about::Facts::gather();
        emit(&about::render(&ctx, &facts));
        return exit;
    }

    // `manual` needs the themed Ctx but no store and no network; dispatch it
    // here beside `theme`, before the engine is ever opened.
    if let Some(Command::Manual { topic }) = &cli.command {
        let exit = Exit::self_framed("manual", cli.json);
        run_manual(&ctx, topic.as_deref());
        return exit;
    }

    // `watch` is socket-only: it subscribes to a daemon and re-renders on push.
    if let Some(Command::Watch { filter }) = &cli.command {
        let exit = Exit::self_framed("watch", cli.json);
        run_watch(cli.socket.as_deref(), cli.no_daemon, filter, &ctx);
        return exit;
    }

    // `pick`'s TTY gate, HERE and not inside `run_pick`, because the ordering is
    // the property: `open_backend` a few lines down opens the store — and, if
    // there is none, CREATES and migrates one — for every command that reaches
    // it. A refused `pick` did exactly that: `TASQX_DB=<empty dir>/tasks.db
    // tasqx pick | cat` exited 2 with the refusal AND left a 208 KB SQLite file
    // behind, while three places (D55, this function's own comment, and
    // `help.rs`) asserted the piped path touches no database.
    //
    // The reason to gate before the store rather than to correct the prose: the
    // refusal is STRUCTURAL. `tasqx pick project:typo` in a pipe cannot run
    // whatever the filter says, so failing on the filter — or on a store that
    // cannot be opened at all — reports a problem the caller does not have and
    // hides the one they do. Gating first also keeps the refusal free for a
    // script on a machine where tasqx has never been run.
    if matches!(&cli.command, Some(Command::Pick { .. })) && !tui::is_interactive(&ctx.caps) {
        return Exit::Out(Err(ApiError::bad_request(PICK_NEEDS_A_TERMINAL)));
    }

    // The explicit `tasqx dashboard`, gated in the same place and for the same
    // reason — and with a second refusal `pick` has no equivalent of: a real
    // terminal can still be too small to draw on.
    //
    // `--json` is excluded from the gate, not from the verb. That path opens no
    // screen, so neither a pipe nor a 40x10 window is an obstacle, and refusing
    // it would make D58's "carries a real `--json` result document" false in
    // every context a script runs in. It does make `dashboard` the first verb
    // where `--json` decides whether the tty gate applies — `tasqx --json pick`
    // still refuses — which is why it is ruled on in §12 rather than left to be
    // discovered.
    // The terminal facts, read ONCE for both gates that consult them: this
    // explicit refusal (which must stay ABOVE the store-open — see the pick
    // gate's comment) and the bare-invocation screen decision further down.
    // They used to be computed twice, one recomputation per gate.
    let (stdout_tty, stdin_tty) = {
        use std::io::IsTerminal;
        (
            std::io::stdout().is_terminal(),
            std::io::stdin().is_terminal(),
        )
    };
    let term_size = terminal_size(&ctx.caps, stdout_tty, stdin_tty);
    if matches!(&cli.command, Some(Command::Dashboard { .. })) && !cli.json {
        if let Some(msg) = dashboard_refusal(&ctx.caps, stdout_tty, stdin_tty, term_size) {
            return Exit::Out(Err(ApiError::bad_request(msg)));
        }
    }

    // Charts and the HTML report are pure local reads; they render straight from
    // a direct Engine (safe under WAL even if a daemon is also running).
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
        return Exit::Out(match cli.command {
            Some(Command::Chart { kind }) => run_chart(&engine, &ctx, kind),
            // `args` carries the optional group_by AND the filter DSL. It used
            // to be dropped here with `..`, which is the whole of F1a: clap
            // parsed the filter, `report_params` knew how to read it, and this
            // one match arm never asked.
            Some(Command::Report {
                html: true,
                args,
                out,
                ..
            }) => run_html_report(&engine, &ctx, args, out),
            _ => unreachable!(),
        });
    }

    // Everything else routes through a reachable daemon (single writer), else
    // falls back to the in-process Engine exactly as before.
    let mut backend = match open_backend(cli.socket.as_deref(), cli.no_daemon) {
        Ok(b) => b,
        Err(msg) => {
            // #229 item 8: a bare `error:` prefix is otherwise reserved for
            // clap's own usage errors, and exit 1 appeared nowhere in
            // DESIGN.md's documented `0/2/4/5/...` contract — a wrapper
            // branching on the exit code could not tell "the store is
            // unreachable" from a clap parse failure by either the code or
            // the message shape. `ErrorCode::Internal` already maps to exit
            // 1, so this reuses it rather than inventing a new code.
            let err = ApiError::internal(msg);
            eprintln!("error [{}]: {}", code_str(&err), err.message);
            exit(err.exit_code());
        }
    };

    // D74: `$TASQX_DB` is never silently inert. On the remote branch the
    // daemon owns the store and the variable does nothing, and the write path
    // says only `Added #N` — on 2026-07-25 that silence sent every write of an
    // automated session into the user's real store. One stderr line, on the
    // path that ignores the variable, every time the condition holds: the
    // common case (no variable, or no daemon) stays silent, and this is
    // deliberately NOT suppressed under `--json` or off a terminal, because
    // the incident's consumer was exactly the automated kind suppression
    // would blind. `config` is exempt — `config store` IS the fuller answer.
    if let Backend::Remote { socket, .. } = &backend {
        if std::env::var("TASQX_DB").is_ok_and(|v| !v.is_empty())
            && !matches!(&cli.command, Some(Command::Config { .. }))
        {
            eprintln!(
                "tasqx: note: routed through the daemon at {socket}; $TASQX_DB is not in \
                 effect (pass --no-daemon to address your own store)"
            );
        }
    }

    // A bare `tasqx` opens the dashboard when — and only when — a human is
    // watching (D58). Everything else about a bare invocation is unchanged, and
    // that is the whole promise: `tasqx | cat`, `tasqx > file`, `--json`,
    // `TERM=dumb`, `TASQX_DASHBOARD=false` and a window under 56x14 all fall
    // through to the working-set table below, byte for byte.
    //
    // Note what is NOT in that list: CI. Nothing here reads a `CI` variable, so
    // a CI job is safe because it redirects rather than because it was
    // recognised — and a caller that hands its child a pty is interactive by
    // this test however unattended it is.
    //
    // `Exit::SelfFramed` rather than a `CmdOutcome`, and it is not decoration:
    // `hint_occasion` classifies a bare run as `Occasion::Ordinary`, and `run()`
    // prints the D57 completion note on the `Exit::Out(Ok)` arm — AFTER
    // `execute` returns. A dashboard handed back as an ordinary outcome would
    // leave the alternate screen and then write a rendered table and a
    // completion nudge into the scrollback of a user who had just pressed `q`.
    //
    // Constructed directly rather than through `Exit::self_framed`, whose job is
    // to make a command that accepts `--json` and ignores it impossible. That
    // cannot happen here: `--json` is in the condition above, so the flag never
    // reaches this path. There is also no command name to declare — this is the
    // absence of a command.
    let fits = term_size.is_some_and(|(w, h)| {
        dashboard_refusal(&ctx.caps, stdout_tty, stdin_tty, Some((w, h))).is_none()
    });
    let verb_screen = matches!(&cli.command, Some(Command::Dashboard { .. })) && !cli.json;
    let bare_screen = cli.command.is_none()
        && dashboard_active(
            &ctx.caps,
            cli.json,
            dashboard_enabled(),
            fits,
            stdout_tty,
            stdin_tty,
        );
    if verb_screen || bare_screen {
        match run_dashboard(&mut backend, &ctx) {
            Ok(Some(render)) => emit(&render),
            Ok(None) => {}
            Err(e) => {
                eprintln!("error [{}]: {}", code_str(&e), e.message);
                exit(e.exit_code());
            }
        }
        return Exit::SelfFramed;
    }

    // `memory list` on a terminal is the memory browser (D121), decided here
    // for the reason the dashboard is decided just above: the screen frames
    // itself, so it leaves as `SelfFramed` and never reaches `emit`.
    if let Some(Command::Memory {
        action:
            MemoryAction::List {
                limit,
                offset,
                project,
            },
    }) = &cli.command
    {
        let paged = limit.is_some() || *offset > 0;
        if memory_screen_active(&ctx.caps, cli.json, paged, term_size, stdout_tty, stdin_tty) {
            if let Err(e) = run_memory_screen(&mut backend, &ctx, project.as_deref()) {
                eprintln!("error [{}]: {}", code_str(&e), e.message);
                exit(e.exit_code());
            }
            return Exit::SelfFramed;
        }
    }

    Exit::Out(match cli.command {
        None => run_list(&mut backend, &ctx, &[], &[], None, None, &[]),
        // Only the `--json` spelling reaches here: the screen leaves as
        // `SelfFramed` above, for the same D57-hint reason the bare invocation
        // does.
        Some(Command::Dashboard { panels }) => {
            run_dashboard_json(&mut backend, &ctx, panels.as_deref())
        }
        Some(Command::Init { name, desc }) => run_init(&mut backend, &ctx, name, desc),
        Some(Command::Add {
            title,
            project,
            priority,
            due,
            scheduled,
            wait,
            repeat,
            remind,
            estimate,
            budget_tokens,
            tags,
        }) => run_add(
            &mut backend,
            &ctx,
            title,
            sugar::AddFlags {
                project,
                priority,
                tags,
                due,
                scheduled,
                wait,
                repeat,
                remind,
                estimate,
                tracked: None,
                budget_tokens,
            },
        ),
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
            tracked,
            budget_tokens,
            tags,
            clear,
            expected_rev,
        }) => run_modify(
            &mut backend,
            &ctx,
            r#ref,
            rest,
            sugar::AddFlags {
                project,
                priority,
                tags,
                due,
                scheduled,
                wait,
                repeat,
                remind,
                estimate,
                tracked,
                budget_tokens,
            },
            &clear,
            expected_rev,
        ),
        Some(Command::List {
            filter,
            sort,
            limit,
            offset,
            fields,
        }) => run_list(&mut backend, &ctx, &filter, &sort, limit, offset, &fields),
        Some(Command::Agenda { filter, days }) => run_agenda(&mut backend, &ctx, &filter, days),
        Some(Command::Start {
            r#ref,
            keep,
            correlation,
        }) => run_start(&mut backend, &ctx, r#ref, keep, &correlation),
        Some(Command::Stop { r#ref }) => run_stop(&mut backend, &ctx, r#ref),
        Some(Command::Done {
            r#ref,
            force,
            correlation,
            self_report,
        }) => run_done(&mut backend, &ctx, r#ref, force, &correlation, &self_report),
        Some(Command::Show { r#ref, card, ascii }) => {
            run_show(&mut backend, &ctx, r#ref, card, ascii)
        }
        Some(Command::Brief {
            r#ref,
            memory_limit,
            card,
            ascii,
        }) => run_brief(&mut backend, &ctx, r#ref, memory_limit, card, ascii),
        Some(Command::Cancel { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.cancel", r#ref),
        Some(Command::Reopen { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.reopen", r#ref),
        Some(Command::Undo) => run_undo(&mut backend, &ctx),
        Some(Command::Annotate { r#ref, text }) => run_annotate(&mut backend, &ctx, r#ref, text),
        Some(Command::Unannotate {
            r#ref,
            annotation_id,
        }) => run_unannotate(&mut backend, &ctx, r#ref, annotation_id),
        Some(Command::Tag { r#ref, tags }) => run_tag(&mut backend, &ctx, "tag.add", r#ref, &tags),
        Some(Command::Untag { r#ref, tags }) => {
            run_tag(&mut backend, &ctx, "tag.remove", r#ref, &tags)
        }
        Some(Command::Dep { r#ref, depends_on }) => {
            run_dep(&mut backend, &ctx, "dependency.add", r#ref, depends_on)
        }
        Some(Command::Undep { r#ref, depends_on }) => {
            run_dep(&mut backend, &ctx, "dependency.remove", r#ref, depends_on)
        }
        Some(Command::Use { name }) => run_use(&mut backend, &ctx, name),
        Some(Command::Archive { name }) => run_archive(&mut backend, &ctx, name),
        Some(Command::Projects { all }) => run_projects(&mut backend, &ctx, all),
        Some(Command::Report {
            args,
            all,
            since,
            until,
            metrics,
            outcomes,
            ..
        }) => run_report(
            &mut backend,
            &ctx,
            args,
            all,
            since,
            until,
            metrics,
            outcomes,
        ),
        Some(Command::Config { action }) => {
            run_config(&mut backend, &ctx, &action, theme_flag.as_deref())
        }
        Some(Command::Check { action }) => run_check(&mut backend, &ctx, &action),
        Some(Command::Memory { action }) => run_memory(&mut backend, &ctx, &action),
        Some(Command::Tokens { action }) => run_tokens(&mut backend, &ctx, &action),
        Some(Command::Export { filter }) => run_export(&mut backend, &filter),
        Some(Command::Import { file }) => run_import(&mut backend, &ctx, file),
        Some(Command::Next {
            filter,
            card,
            ascii,
        }) => run_next(&mut backend, &ctx, &filter, card, ascii),
        Some(Command::Pick { filter }) => run_pick(&mut backend, &ctx, &filter),
        Some(Command::Why { r#ref, card, ascii }) => {
            run_why(&mut backend, &ctx, r#ref, card, ascii)
        }
        Some(Command::Chart { .. }) => unreachable!("handled above"),
        Some(Command::Theme { .. }) => unreachable!("handled above"),
        Some(Command::Docs { .. }) => unreachable!("handled above"),
        Some(Command::Watch { .. }) => unreachable!("handled above"),
        Some(Command::Api) => unreachable!("handled above"),
        Some(Command::Daemon { .. }) => unreachable!("handled above"),
        Some(Command::Mcp { .. }) => unreachable!("handled above"),
        Some(Command::About) => unreachable!("handled above"),
        Some(Command::Manual { .. }) => unreachable!("handled above"),
        Some(Command::Completions { .. }) => unreachable!("handled above"),
    })
}

fn build_ctx(flag: Option<&str>) -> Ctx {
    // Loud about the FILE ITSELF (#192) before anything reads a setting out of
    // it — a parse error or an unknown key means every value below is either
    // the default or a stale idea of what the file said.
    warn_about_config_file();
    // One chain for every setting (config::resolve), rather than a per-setting
    // fold. The env layer is read inside the resolver so a caller cannot forget it.
    let s = config::find("theme.name").expect("theme.name is a registered setting");
    let (name, _, warning) = effective_setting(s, flag, config::toml_value(s).as_deref());
    // Every layer, not just the ones typed for THIS run. The older rule warned
    // for `--theme`/`$TASQX_THEME` only and left a hand-edited `config.toml` to
    // `tasqx config` — but `config` was reporting the file's value as though it
    // were in effect, so nothing anywhere said the name had been dropped. A
    // persisted unknown theme is the loudest case, not the quietest: it is wrong
    // on every run until someone is told. Warning here rather than in
    // `setting_value` keeps it to exactly one line per invocation, since
    // `build_ctx` runs before every command including `config` itself.
    if let Some(msg) = warning {
        eprintln!("{msg}");
    }
    let dir = themes_dir();
    // `load_reporting`, not `load` (#193, completing D46): a theme FILE that
    // fails to load must warn the same way an unknown theme NAME already does
    // a few lines up — before this, `--theme broken`/`$TASQX_THEME=broken`
    // silently rendered the fallback with nothing on stderr, unlike a typo'd
    // name. It still never refuses: a broken theme must not block a task
    // capture, so `Merged`'s dropped-piece warnings and a `Rejected` message
    // are both just printed and rendering continues on the fallback theme.
    let loaded = theme::load_reporting(&name, dir.as_deref());
    for msg in loaded.file.messages() {
        eprintln!("warning: {msg}");
    }
    Ctx::new(loaded.theme, Caps::detect())
        .with_cols(theme::detect_cols())
        .with_time_format(config_detail_time_format())
}

/// Result of a rendered command: the raw API result (for `--json`) plus the
/// pre-rendered human string.
type CmdOutcome = Result<(Value, String), tasqx_core::ApiError>;

/// How the settings layer reads a `Home::Store` value, as a closure.
///
/// A seam, and a deliberate one: `Backend::Local` cannot fail this call, so
/// without it the error path below is unreachable from a test and the very bug
/// it exists to prevent could be reintroduced with the suite staying green.
type StoreLookup<'a> = &'a mut dyn FnMut(&str) -> Result<Option<String>, ApiError>;

/// The stable error `code` as a string (for CLI diagnostics).
fn code_str(e: &tasqx_core::ApiError) -> String {
    serde_json::to_value(e.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer whose reader has gone away: every byte is refused `BrokenPipe`.
    struct ClosedPipe;
    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A writer with a real fault (disk full, not a departed reader).
    struct FaultyPipe;
    impl Write for FaultyPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("write fault"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn emit_via_writes_and_reports_the_reader_present() {
        let mut out = Vec::new();
        let open = emit_via(&mut out, "hello\n").expect("a healthy writer");
        assert!(open, "a successful write means the reader is still there");
        assert_eq!(out, b"hello\n");
    }

    #[test]
    fn emit_via_reports_a_closed_pipe_as_success_without_a_reader() {
        // The contract every call site leans on: `watch | head` losing its
        // reader is a clean end of stream, not an error and never a panic.
        let open = emit_via(&mut ClosedPipe, "frame\n").expect("BrokenPipe is not an error here");
        assert!(!open, "a closed pipe must report the reader gone");
    }

    #[test]
    fn emit_via_passes_a_real_write_fault_through() {
        // Only BrokenPipe is tolerated; a genuine fault must surface so
        // `emit_open` can print the diagnostic and exit 1.
        assert!(emit_via(&mut FaultyPipe, "x").is_err());
    }

    fn tty_caps() -> Caps {
        Caps {
            depth: crate::theme::ColorDepth::Truecolor,
            ansi: true,
            unicode: true,
        }
    }

    /// The refusal is a policy over four facts, not a question asked of the
    /// process — which is what makes it testable at all. Under cargo this
    /// process has a piped stdout, so a predicate that consulted it directly
    /// could only ever be exercised on the refusing branch.
    #[test]
    fn the_dashboard_refusal_names_what_is_wrong_with_this_terminal() {
        let caps = tty_caps();
        // Not a terminal at all: the stream answer wins before size matters.
        assert_eq!(
            dashboard_refusal(&caps, false, true, Some((200, 60))).as_deref(),
            Some(DASHBOARD_NEEDS_A_TERMINAL)
        );
        assert_eq!(
            dashboard_refusal(&caps, true, false, Some((200, 60))).as_deref(),
            Some(DASHBOARD_NEEDS_A_TERMINAL)
        );

        // A real terminal, big enough.
        assert_eq!(dashboard_refusal(&caps, true, true, Some((56, 14))), None);
        assert_eq!(dashboard_refusal(&caps, true, true, Some((200, 60))), None);

        // A real terminal, too small — and the message says which, because
        // "too small" without a number leaves the reader guessing at how much
        // to resize.
        let msg = dashboard_refusal(&caps, true, true, Some((40, 10))).expect("40x10 refuses");
        assert!(msg.contains("40x10"), "must name the measured size: {msg}");
        assert!(msg.contains("56x14"), "must name the required size: {msg}");
        assert!(msg.contains("tasqx list"), "must name a way through: {msg}");
        // One cell short on either axis is short.
        assert!(dashboard_refusal(&caps, true, true, Some((55, 14))).is_some());
        assert!(dashboard_refusal(&caps, true, true, Some((56, 13))).is_some());

        // Unmeasurable is a refusal, not a default: entering the alternate
        // screen on a guess is how a half-drawn frame lands on a window nobody
        // can read.
        assert!(dashboard_refusal(&caps, true, true, None).is_some());
    }

    /// `--panels`/`dashboard.panels` share one parser (#152): an unknown word
    /// is dropped rather than refused, `PanelId::Slot` is unreachable (there
    /// is no slug for it), and the order typed is the order kept — a caller
    /// that asked for `burndown,projects` must not get them back the other way.
    #[test]
    fn parse_panel_list_keeps_the_typed_order_and_drops_the_unknown() {
        use tui::dashboard::model::PanelId;
        assert_eq!(
            parse_panel_list("burndown, projects , bogus,tokens"),
            vec![PanelId::Burndown, PanelId::Projects, PanelId::Tokens]
        );
        assert_eq!(parse_panel_list(""), Vec::<PanelId>::new());
        assert_eq!(parse_panel_list("slot"), Vec::<PanelId>::new());
    }

    /// A config written against the eight-panel screen still parses.
    ///
    /// D80 folded NOW, NEXT UP, DUE, BLOCKED and RECENT into TASKS, and a
    /// setting that names them was written against a screen that existed —
    /// dropping five words as "unknown" would tell a reader their config is
    /// wrong when what happened is that it moved. They resolve to `tasks`,
    /// deduplicated, because five names for one panel must not place it five
    /// times.
    #[test]
    fn the_panel_names_d80_retired_still_resolve() {
        use tui::dashboard::model::PanelId;
        assert_eq!(
            parse_panel_list("now,next,due,blocked,recent"),
            vec![PanelId::Tasks]
        );
        assert_eq!(
            parse_panel_list("projects,next,burndown"),
            vec![PanelId::Projects, PanelId::Tasks, PanelId::Burndown]
        );
    }

    /// A window too small for the screen makes a BARE `tasqx` print the table
    /// instead — it must never open an alternate screen it cannot draw in.
    ///
    /// This is a regression guard with a real failure behind it. Without the
    /// `fits` term, bare `tasqx` in a 40x10 window entered the alternate
    /// screen, painted nothing at all (`layout` returns `None` below 56x14 and
    /// `render` returns early), blocked until `q`, and created a 208 KB store
    /// on the way in — D55's refused-screen-leaves-a-store failure, one screen
    /// over.
    #[test]
    fn a_window_too_small_to_draw_in_falls_back_to_the_table() {
        let caps = tty_caps();
        assert!(
            dashboard_active(&caps, false, true, true, true, true),
            "a big enough interactive terminal opens the screen"
        );
        assert!(
            !dashboard_active(&caps, false, true, false, true, true),
            "a window too small must fall through to run_list, not open a blank screen"
        );
        // Every other signal still refuses on its own.
        assert!(
            !dashboard_active(&caps, true, true, true, true, true),
            "--json never opens a screen"
        );
        assert!(
            !dashboard_active(&caps, false, false, true, true, true),
            "dashboard.enabled = false is the escape hatch"
        );
        assert!(
            !dashboard_active(&caps, false, true, true, false, true),
            "a piped stdout never opens a screen"
        );
        assert!(
            !dashboard_active(&caps, false, true, true, true, false),
            "a piped stdin never opens a screen — the key loop would block on it"
        );
    }

    /// `dashboard.enabled` is read from the config FILE, not only the
    /// environment.
    ///
    /// `dashboard_active` took `enabled` as a parameter and the test above pins
    /// what it does with it — but nothing pinned where the caller got it, and
    /// the caller read `TASQX_DASHBOARD` and stopped. `config edit` drew the
    /// row, `config set dashboard.enabled false` wrote it to `config.toml` and
    /// printed the new value, and the next bare `tasqx` opened the dashboard
    /// anyway. A setting that acknowledges a write and ignores it is worse than
    /// one that was never offered.
    ///
    /// The file value is passed in rather than written to disk under
    /// `$TASQX_CONFIG_DIR`: cargo runs these threads in one process, and a test
    /// that mutates env is a test that flakes when another one reads it.
    #[test]
    fn the_escape_hatch_is_read_from_the_config_file() {
        assert!(
            !dashboard_enabled_with(Some("false")),
            "`dashboard.enabled = false` in config.toml must switch the screen off"
        );
        assert!(
            dashboard_enabled_with(Some("true")),
            "and `true` must switch it back on"
        );
        assert!(
            dashboard_enabled_with(None),
            "with nothing in the file the default is on — the dashboard IS a bare tasqx"
        );
        // The shell spellings `resolve` hands through uncoerced from the env.
        for off in ["false", "0", "no", "NO", " false "] {
            assert!(
                !dashboard_enabled_with(Some(off)),
                "{off:?} must read as off"
            );
        }
    }

    /// Both halves of the argv escape pair, over the SAME registry.
    ///
    /// `argv::FILTER_COMMANDS` decides which commands get their `-tag` tokens
    /// hidden from clap; `unescape_filter_tail` decides which get them back.
    /// Until this guard existed only the first half was checked, and `pick`
    /// shipped in the first list and not the second: `tasqx pick -api` reached
    /// `task.list` with the filter string `"\u{1}api"`, so the user got either a
    /// parse error for a token they never typed or `no_candidates` quoting a
    /// control byte back at them, while `tasqx list -api` worked on the same
    /// store. C7's exact class, the third time it has leaked in this cluster.
    ///
    /// Driven through the REAL pre-pass, the REAL clap parse and the REAL
    /// restore function, for every name the registry holds — a test that built
    /// the escaped token itself, or listed the commands again here, would agree
    /// with a broken half by construction.
    #[test]
    fn every_filter_command_gets_its_dashes_back() {
        for name in argv::FILTER_COMMANDS {
            let raw = ["tasqx", name, "-needs"].map(std::ffi::OsString::from);
            let pre = argv::prepass(raw);
            assert!(
                pre.filter_command,
                "`{name}` is registered but the pre-pass did not treat it as filter-taking"
            );
            let mut cli =
                Cli::try_parse_from(pre.argv).unwrap_or_else(|e| panic!("`{name} -needs`: {e}"));

            // Before: the token must actually carry the sentinel, or this
            // command is not hyphen-tolerant at all and the assertion below
            // would pass for the wrong reason.
            let before = cli
                .command
                .as_mut()
                .and_then(Command::filter_tail_mut)
                .unwrap_or_else(|| {
                    panic!(
                        "`{name}` is in FILTER_COMMANDS but `filter_tail_mut` returns None, so \
                         nothing restores the dash the pre-pass hid"
                    )
                })
                .clone();
            assert_eq!(
                before,
                [format!("{}needs", '\u{1}')],
                "`{name} -needs` was not escaped by the pre-pass"
            );

            unescape_filter_tail(&mut cli);
            let after = cli
                .command
                .as_mut()
                .and_then(Command::filter_tail_mut)
                .expect("the same tail as above")
                .clone();
            assert_eq!(
                after,
                ["-needs"],
                "`{name} -needs` reached the filter with the argv sentinel still in it"
            );
        }
    }

    /// The store path and the routing decision both drive every write and, until
    /// now, appeared on no read surface — the invisible-field failure DESIGN.md
    /// has already recorded six times (`remind`, `estimate`, the dependency
    /// JOINs, `default_project`, `tracked_seconds`, `blocked`). `config path`
    /// answered for `config.toml` and nothing answered for the store.
    #[test]
    fn store_location_names_the_file_when_the_command_runs_in_process() {
        let (json, text) = store_location(
            None,
            None,
            Ok(PathBuf::from("/home/u/.local/tasqx/tasks.db")),
        );
        assert_eq!(json["backend"], "local");
        assert_eq!(json["path"], "/home/u/.local/tasqx/tasks.db");
        assert!(
            text.contains("/home/u/.local/tasqx/tasks.db"),
            "the human line must name the file being written: {text}"
        );
    }

    /// The one that matters. `open_backend` prefers a reachable daemon and the
    /// remote path never consults `TASQX_DB`, so a correct `TASQX_DB` is
    /// silently not in effect whenever a daemon is listening. That cost this
    /// project real data on 2026-07-25: an agent set a scratch store, a daemon
    /// answered, and the writes landed in the user's live store with exit 0.
    #[test]
    fn store_location_says_the_local_db_is_not_in_effect_when_a_daemon_answers() {
        let (json, text) = store_location(
            Some("/run/user/1000/tasqx/tasqx.sock"),
            Some("/home/u/.local/tasqx/tasks.db"),
            Ok(PathBuf::from("/tmp/scratch.db")),
        );
        assert_eq!(json["backend"], "daemon");
        assert_eq!(json["socket"], "/run/user/1000/tasqx/tasqx.sock");
        assert!(
            text.contains("/run/user/1000/tasqx/tasqx.sock"),
            "name the socket actually being written through: {text}"
        );
        assert!(
            text.contains("TASQX_DB"),
            "the whole point is telling the reader their TASQX_DB is inert: {text}"
        );
        // D74: the daemon's own file IS named — the caller asked the daemon,
        // which is not the guess D47 forbade.
        assert_eq!(json["store"], "/home/u/.local/tasqx/tasks.db");
        assert!(
            text.contains("the daemon owns the store: /home/u/.local/tasqx/tasks.db"),
            "the human line must name the daemon's file: {text}"
        );
        // The local path must not be presented as the store — that is the lie
        // the incident was made of.
        assert_ne!(
            json["path"], "/tmp/scratch.db",
            "the client's db_path is NOT the store a daemon writes to"
        );
    }

    /// A daemon that predates D74 cannot be asked which store it owns, and the
    /// answer must degrade to naming the socket rather than inventing a path.
    #[test]
    fn store_location_degrades_honestly_against_a_daemon_that_cannot_name_its_store() {
        let (json, text) = store_location(
            Some("tasqx-default"),
            None,
            Ok(PathBuf::from("/tmp/scratch.db")),
        );
        assert_eq!(json["backend"], "daemon");
        assert_eq!(json["store"], Value::Null);
        assert!(
            text.contains("cannot name it"),
            "an unanswerable question is said, not papered over: {text}"
        );
        assert_ne!(json["path"], "/tmp/scratch.db");
    }

    /// #184: the trap from the Observed section, decided in isolation — a live
    /// daemon on the ambient socket, no `$TASQX_DB`, the exact condition under
    /// which `api`/`mcp serve` used to say nothing while opening a different
    /// store than the one the operator can see.
    #[test]
    fn ambient_socket_note_fires_when_a_live_daemon_goes_unrouted() {
        let note = ambient_socket_note(
            "api",
            Some("/tmp/tqd-ttd/s"),
            false,
            true,
            Some("/home/u/store.db"),
            "/home/u/.local/share/tasqx/tasks.db",
        )
        .unwrap_or_else(|| panic!("a live, unrouted daemon must produce a note"));
        assert!(note.contains("/tmp/tqd-ttd/s"), "name the socket: {note}");
        assert!(
            note.contains("/home/u/.local/share/tasqx/tasks.db"),
            "name the store this verb actually opened: {note}"
        );
        assert!(
            note.contains("/home/u/store.db"),
            "name the daemon's own store when it can be asked: {note}"
        );
        assert!(
            note.contains("D73"),
            "point at the decision that makes this ambient rather than refused: {note}"
        );
    }

    /// D73's ruling is untouched: a daemon that predates the field (or answers
    /// `core.capabilities` without a `store`) still gets the note, just without
    /// naming a file it cannot ask for — degrade honestly, the D74 rule
    /// `store_location` already follows, applied here too.
    #[test]
    fn ambient_socket_note_degrades_when_the_daemon_cannot_name_its_store() {
        let note = ambient_socket_note(
            "mcp serve",
            Some("tasqx-default"),
            false,
            true,
            None,
            "/tmp/scratch.db",
        )
        .unwrap_or_else(|| panic!("a live, unrouted daemon must still produce a note"));
        assert!(note.contains("tasqx-default"), "{note}");
        assert!(note.contains("/tmp/scratch.db"), "{note}");
    }

    /// The four ways this must stay quiet — each isolated so a future change
    /// cannot pass by only ever testing the OR of all four.
    #[test]
    fn ambient_socket_note_is_silent_without_a_live_divergent_store() {
        // No $TASQX_SOCK at all: nothing to route through, nothing to warn about.
        assert_eq!(
            ambient_socket_note("api", None, false, false, None, "/tmp/x.db"),
            None,
            "an unset $TASQX_SOCK must not produce a note"
        );
        // $TASQX_SOCK set, but nothing answers there (stale, or never a daemon).
        assert_eq!(
            ambient_socket_note(
                "api",
                Some("/tmp/stale.sock"),
                false,
                false,
                None,
                "/tmp/x.db"
            ),
            None,
            "an unreachable socket has no divergent store to warn about"
        );
        // $TASQX_SOCK set AND a daemon answers, but the operator already named
        // their own store — D73's env-var-stays-ambient case working as
        // intended, not the invisible-field trap #184 found.
        assert_eq!(
            ambient_socket_note(
                "api",
                Some("/tmp/tqd-ttd/s"),
                true,
                true,
                Some("/home/u/store.db"),
                "/home/u/store.db"
            ),
            None,
            "an explicit $TASQX_DB means the operator already chose — silence is correct"
        );
        // A reviewer rejected the first version of this fix over exactly this
        // case: $TASQX_SOCK set, a daemon answers, $TASQX_DB unset — but the
        // daemon happens to serve the very file this verb would open by
        // default anyway. There is no divergent store here: opening it
        // in-process answers from the right data, just without the daemon's
        // single-writer coordination, so the D73 note has nothing to warn
        // about. The task's own Verification annotation names "daemon serving
        // a non-default store" as a required precondition; this is the case
        // where that precondition does not hold.
        assert_eq!(
            ambient_socket_note(
                "api",
                Some("/tmp/tqd-ttd/s"),
                false,
                true,
                Some("/home/u/.local/share/tasqx/tasks.db"),
                "/home/u/.local/share/tasqx/tasks.db"
            ),
            None,
            "the daemon's store and the local default are the same file — nothing diverged"
        );
    }

    /// #249: the daemon announces a congested subscriber's loss with a gap
    /// frame carrying the exact count, and the non-TTY renderer read `op` and
    /// `short_id` and nothing else — so the line a script saw was
    /// `task.changed op=gap`, the one field that made the frame actionable
    /// dropped at the last hop. The count must render, and must keep rendering
    /// without something going red.
    #[test]
    fn the_watch_stream_renders_the_dropped_count_it_used_to_print_away() {
        assert_eq!(
            watch_stream_line(&json!({ "op": "gap", "dropped": 372 })),
            "task.changed op=gap dropped=372",
            "the daemon computed, sent and logged this number; the renderer \
             is not where it dies"
        );
        assert_eq!(
            watch_stream_line(&json!({ "op": "add", "short_id": 7 })),
            "task.changed op=add short_id=7",
            "ordinary events are unchanged"
        );
        assert_eq!(
            watch_stream_line(&json!({})),
            "task.changed op=change",
            "a frame with no fields still renders a line"
        );
    }

    #[test]
    fn command_declarations_do_not_execute_or_render() {
        let source = include_str!("command.rs");
        for forbidden in [
            "tasqx_core::Engine",
            "tasqx_core::dispatch",
            "std::process::Command",
            "crate::render",
            "super::render",
        ] {
            assert!(
                !source.contains(forbidden),
                "command declarations must not depend on `{forbidden}`"
            );
        }
    }

    #[test]
    fn mcp_serve_takes_an_explicit_scope_with_a_read_only_default() {
        for (args, expected) in [
            (vec!["tasqx", "mcp", "serve"], "read"),
            (vec!["tasqx", "mcp", "serve", "--scope", "read"], "read"),
            (vec!["tasqx", "mcp", "serve", "--scope", "write"], "write"),
        ] {
            let cli = Cli::try_parse_from(args).expect("explicit MCP scope should parse");
            match cli.command.expect("mcp command") {
                Command::Mcp {
                    action: McpAction::Serve { scope },
                } => {
                    assert_eq!(scope, expected);
                }
                _ => panic!("expected mcp serve"),
            }
        }
    }

    /// Run the stdio loop over a canned input, returning `(stdout, stderr)`.
    fn drive_mcp_loop(input: &str, dispatch: impl Fn(&Value) -> Option<Value>) -> (String, String) {
        let mut reader = std::io::BufReader::new(input.as_bytes());
        let (mut out, mut errs) = (Vec::new(), Vec::new());
        mcp_stdio_loop(&mut reader, &mut out, &mut errs, dispatch);
        (
            String::from_utf8(out).expect("stdout frames are UTF-8"),
            String::from_utf8(errs).expect("diagnostics are UTF-8"),
        )
    }

    /// A panic in one tool call must cost that one call, not the session. The
    /// MCP server runs unsupervised inside an agent's process, so it mirrors the
    /// daemon's dispatch containment: without it the whole process dies and the
    /// agent sees tasqx vanish mid-conversation with no JSON-RPC error to
    /// explain it — indistinguishable from a transport fault.
    #[test]
    fn a_panicking_mcp_dispatch_becomes_an_internal_error_and_the_session_survives() {
        let (out, _errs) = drive_mcp_loop(
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\"}\n\
             {\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n",
            |msg| {
                if msg["id"] == json!(7) {
                    panic!("simulated panic reachable from a tool call");
                }
                Some(json!({ "jsonrpc": "2.0", "id": msg["id"].clone(), "result": {} }))
            },
        );

        let frames: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).expect("every stdout frame is JSON"))
            .collect();
        assert_eq!(
            frames.len(),
            2,
            "expected one frame per request, got {out:?}"
        );
        assert_eq!(frames[0]["id"], json!(7));
        assert_eq!(frames[0]["error"]["code"], json!(-32603));
        // The request after the panicking one is still answered: the loop kept
        // reading rather than the process disappearing.
        assert_eq!(frames[1]["id"], json!(8));
        assert!(frames[1].get("result").is_some(), "id 8 should be a result");
    }

    /// The error envelope is gated on a present, non-null `id`: `handle_message`
    /// returns `None` for notifications on purpose, and an unsolicited
    /// `id: null` error would put a frame nobody asked for on the stdout channel
    /// the module doc reserves for responses.
    #[test]
    fn a_panicking_mcp_notification_emits_nothing_on_stdout() {
        for msg in [
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"notifications/cancelled\"}",
        ] {
            let (out, _errs) = drive_mcp_loop(&format!("{msg}\n"), |_| {
                panic!("simulated panic while handling a notification");
            });
            assert!(out.is_empty(), "notification produced a response: {out:?}");
        }
    }

    /// A read failure ends the session, so it is the last thing the operator can
    /// learn anything from: swallowing the error leaves an I/O fault or a
    /// non-UTF-8 byte on stdin looking exactly like a clean EOF.
    #[test]
    fn a_failed_stdin_read_is_reported_on_stderr_before_the_loop_ends() {
        let mut reader = std::io::BufReader::new(&b"\xff\xfe not utf-8\n"[..]);
        let (mut out, mut errs) = (Vec::new(), Vec::new());
        mcp_stdio_loop(&mut reader, &mut out, &mut errs, |_| {
            panic!("dispatch must not be reached for an unreadable line")
        });

        let errs = String::from_utf8(errs).expect("diagnostics are UTF-8");
        assert!(
            errs.contains("tasqx mcp: stdin read failed:"),
            "read failure went unreported: {errs:?}"
        );
        assert!(out.is_empty(), "nothing belongs on stdout: {out:?}");
    }

    #[test]
    fn removed_mcp_token_forms_are_rejected_even_when_the_value_looks_plausible() {
        assert!(Cli::try_parse_from(["tasqx", "mcp", "token", "--scope", "read"]).is_err());
        for token in ["tasqx_mcp_write_", "tasqx_mcp_write_anything", "random"] {
            assert!(
                Cli::try_parse_from(["tasqx", "mcp", "serve", "--token", token]).is_err(),
                "removed token form unexpectedly accepted {token:?}"
            );
        }
    }

    fn add_of(argv: &[&str]) -> Command {
        Cli::try_parse_from(argv)
            .expect("argv should parse")
            .command
            .expect("a subcommand")
    }

    // ---- the theme pointer (D26 follow-up) ----------------------------------

    /// `config edit` saved a theme and said only `theme.name = gruvbox`, which
    /// the user could not act on: tasqx's own output barely changes colour, so
    /// they came back asking where they were supposed to see it. This is the
    /// only reachable test of that path — the rest of `run_config_edit` needs a
    /// real terminal — so without it the TUI could lose the pointer silently
    /// while the two non-interactive paths stayed covered end to end.
    #[test]
    fn config_edit_summary_points_a_saved_theme_at_theme_show() {
        let saved = vec![("theme.name".to_string(), "gruvbox".to_string())];
        let text = saved_summary(&saved);
        assert!(text.contains("theme.name = gruvbox"), "{text}");
        assert!(text.contains("tasqx theme show"), "{text}");
    }

    /// The pointer is theme-specific. An unconditional append would satisfy the
    /// test above while telling someone who toggled notifications to go look at
    /// a colour swatch.
    #[test]
    fn config_edit_summary_says_nothing_about_themes_for_other_keys() {
        let saved = vec![("notify.enabled".to_string(), "true".to_string())];
        let text = saved_summary(&saved);
        assert_eq!(text, "notify.enabled = true\n");
    }

    /// The no-op case still has to read as a no-op; folding the pointer in
    /// unconditionally would have printed advice about a theme nobody set.
    #[test]
    fn config_edit_summary_reports_an_untouched_session() {
        assert_eq!(saved_summary(&[]), "no changes\n");
    }

    /// The pointer names a command that must actually exist and take no
    /// arguments. A hint pointing at a verb tasqx does not have is worse than
    /// no hint; this pins the text to something clap can parse.
    #[test]
    fn the_pointed_at_command_is_a_real_one() {
        let hint = theme_pointer("theme.name").expect("theme.name has a pointer");
        assert!(hint.contains("tasqx theme show"), "{hint}");
        Cli::try_parse_from(["tasqx", "theme", "show"]).expect("`tasqx theme show` must parse");
    }

    // ---- `tasqx pick` (D55) --------------------------------------------------

    /// A plain context: no ANSI, so the assertions below are about the words in
    /// the line rather than about the escape sequences around them.
    fn plain_ctx() -> Ctx {
        Ctx::new(theme::load("nord", None), Caps::PLAIN)
    }

    /// The mapping from a `task.list` answer to screen rows. Every row on this
    /// screen decides which task `s` starts, and the whole path around it
    /// needs a real terminal — so a mapping that read `id` where it meant
    /// `short_id` would leave the suite green with the browser unusable. What
    /// the row DRAWS is `list`'s renderer's business now (D124), tested in
    /// `tui::pick`; what this pins is identity.
    #[test]
    fn pick_rows_carry_the_identity_the_screen_starts_by() {
        let listed = json!({ "tasks": [
            { "short_id": 42, "id": "uuid-not-this-one", "title": "Ship the freeze",
              "project": "work.tasqx", "priority": "H", "urgency": 11.84,
              "tags": ["release", "api"] },
            { "short_id": 7, "title": "No priority here" },
        ]});
        let rows = pick_rows(&listed);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].short_id, 42, "the ref must be the short_id");
        assert_eq!(rows[0].title, "Ship the freeze");
        // A task missing the optional fields is still a pickable row.
        assert_eq!(rows[1].short_id, 7);
    }

    /// D124: the browser reads the whole candidate set, not `task.list`'s
    /// first page (D110's default of 100): a search cannot find what was
    /// never read, and `100 tasks` over a larger set is rule 10 broken.
    #[test]
    fn pick_reads_every_page_of_its_candidates() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        // Past two pages of 200 and past D110's 100, so neither one call at
        // the page size nor one at D110's default can pass for the loop.
        for i in 0..450 {
            e.task_add(&json!({ "title": format!("task {i}") }))
                .unwrap();
        }
        let mut be = Backend::Local(e);
        let listed = candidates_in_pages(&mut be, "@working", 200).unwrap();
        assert_eq!(pick_rows(&listed).len(), 450);
        assert_eq!(
            listed["count"],
            json!(450),
            "the count is the set, not a page"
        );
    }

    /// An empty `task.list` answer must produce no rows, which is what makes
    /// `run_pick` refuse instead of opening an alt screen whose only available
    /// action is leaving it.
    #[test]
    fn an_empty_task_list_yields_no_pick_rows() {
        assert!(pick_rows(&json!({ "tasks": [] })).is_empty());
        assert!(
            pick_rows(&json!({})).is_empty(),
            "a missing key is not rows"
        );
    }

    /// Nothing to pick is exit 4, and the message has to quote the filter back.
    /// "No tasks." would be ambiguous in the one place it matters: an empty
    /// working set and a filter that excludes everything look identical from
    /// outside, and only the text can tell the user which one they hit.
    #[test]
    fn nothing_to_pick_is_a_not_found_that_names_the_filter() {
        let e = no_candidates("project:work +api");
        assert_eq!(e.exit_code(), 4, "an empty candidate set may not exit 0");
        assert!(e.message.contains("project:work +api"), "{}", e.message);
        assert!(
            e.message.contains("tasqx list"),
            "the refusal must name a way to look: {}",
            e.message
        );
    }

    /// D128: leaving the browser without starting a task is exit 0. D55 made
    /// it exit 4 because `pick` was a CHOOSER whose entire output was the
    /// start; D124 turned it into the task browser, where `j`/`k` move, `/`
    /// searches and Enter reads a card, so closing it having read is the
    /// ordinary way to use it. A shell that treats `q` as a failure breaks
    /// `pick && …`, a prompt indicator, and any script that opens it to look.
    #[test]
    fn leaving_the_browser_without_starting_a_task_is_exit_0() {
        let (body, render) = nothing_picked().expect("closing the browser is not a failed run");
        assert_eq!(
            body["started"],
            json!(false),
            "`--json` has to say a task was not started: {body}"
        );
        assert!(
            render.is_empty(),
            "a browser you closed leaves no line in the scrollback: {render:?}"
        );
    }

    /// D128 moves the LEAVE, not the refusals. A request that could not be
    /// served stays non-zero: an empty candidate set is still `not_found`
    /// (exit 4, the test above this one), and a piped `pick` is still exit 2
    /// (`help.rs::pick_refuses_a_piped_stdout_with_a_nonzero_exit`).
    #[test]
    fn a_request_that_could_not_be_served_is_still_non_zero() {
        assert_eq!(
            no_candidates("@working").exit_code(),
            4,
            "an empty store is a request pick could not serve, not a session the user ended"
        );
        assert!(
            nothing_picked().is_ok(),
            "…and the leave it is contrasted with is not"
        );
    }

    /// The refusal a script hits. It must name the commands that answer the
    /// same question without a screen — "needs a terminal" alone leaves the
    /// reader with nothing to type next — and those commands must be real,
    /// which is what the parse below checks.
    #[test]
    fn the_non_interactive_refusal_names_commands_that_exist() {
        assert!(
            PICK_NEEDS_A_TERMINAL.contains("interactive terminal"),
            "{PICK_NEEDS_A_TERMINAL}"
        );
        assert!(
            PICK_NEEDS_A_TERMINAL.contains("tasqx next"),
            "{PICK_NEEDS_A_TERMINAL}"
        );
        assert!(
            PICK_NEEDS_A_TERMINAL.contains("tasqx start"),
            "{PICK_NEEDS_A_TERMINAL}"
        );
        Cli::try_parse_from(["tasqx", "next"]).expect("`tasqx next` must parse");
        Cli::try_parse_from(["tasqx", "start", "1"]).expect("`tasqx start <ref>` must parse");
        // And the gate itself: `Caps::PLAIN` and a redirected stream are both
        // refusals, which is the rule this message explains.
        assert!(!tui::is_interactive_with(&Caps::PLAIN, true, true));
        assert!(!tui::is_interactive_with(&plain_ctx().caps, false, true));
    }

    /// The scrollback `pick` leaves once the alt screen is gone has to NAME
    /// the task it started — an interactive session must leave a record of
    /// which one — and, since #75, `task.start` names it and what it
    /// auto-stopped itself, so the line says each thing ONCE (D124). It named
    /// the task twice, and D101's stand-in said the stop twice with two
    /// different durations (rule 11).
    #[test]
    fn the_pick_summary_says_each_thing_once() {
        let started = json!({
            "id": "0199-uuid", "short_id": 48, "title": "Renew the TLS certificate",
            "status": "active", "interval_started": "2026-08-03T10:00:00Z",
            "already_running": false,
            "auto_stopped": [{ "id": "u", "short_id": 49, "tracked": "PT2H23M" }],
        });
        let text = picked_summary(&plain_ctx(), &started, &started, &render::Titles::new());
        assert_eq!(text.matches("#48").count(), 1, "{text}");
        assert_eq!(text.matches("stopped").count(), 1, "{text}");
        assert!(text.contains("Renew the TLS certificate"), "{text}");
        // #205's facts, kept: the displaced task is named, with its time. Its
        // POSITION moved under D126 (e) — the task the command named is the
        // first line under the prompt, and what else moved follows — so the
        // stop line now prints BELOW the card instead of above it. D124 made
        // this summary `start`'s own echo; it orders itself the way `start`
        // does, or the two drift apart again.
        assert!(text.contains("#49") && text.contains("2h"), "{text}");
        assert!(
            text.find("started").unwrap() < text.find("stopped").unwrap(),
            "the displaced task must print BELOW the card (D126 e): {text}"
        );
    }

    /// Nothing was running: no stop line, and no header naming nothing.
    #[test]
    fn no_displaced_task_means_no_stop_line_at_all() {
        let started = json!({
            "id": "uuid", "short_id": 128, "title": "Next task",
            "interval_started": "2026-08-03T10:00:00Z", "auto_stopped": [],
        });
        let text = picked_summary(&plain_ctx(), &started, &started, &render::Titles::new());
        assert!(!text.contains("stopped"), "{text}");
        assert!(
            text.contains("#128") && text.contains("Next task"),
            "{text}"
        );
    }

    /// Pages read at different instants can overlap when a write lands
    /// between them; a task is kept once, where it first appeared.
    #[test]
    fn pages_that_overlap_keep_each_task_once() {
        let pages = [
            json!({ "tasks": [{ "short_id": 1 }, { "short_id": 2 }] }),
            json!({ "tasks": [{ "short_id": 2 }, { "short_id": 3 }] }),
        ];
        let merged = merge_pages(&pages);
        let ids: Vec<i64> = merged["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["short_id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(merged["count"], json!(3));
    }

    /// `--json` must carry the identity of the task that was picked. The method
    /// answers `{id, interval_started}` — a UUID and a timestamp — and the ref
    /// is the very thing `pick` was asked to determine, so without these two
    /// fields the machine-readable answer omits the answer. The method's own
    /// keys are passed through untouched beside them.
    #[test]
    fn the_pick_json_carries_the_chosen_ref_beside_the_methods_own_answer() {
        let started = json!({
            "id": "0199-uuid", "interval_started": "2026-08-03T10:00:00Z",
            "already_running": false,
        });
        let out = pick_result(42, "Ship the freeze", started);
        assert_eq!(out["short_id"], json!(42));
        assert_eq!(out["title"], json!("Ship the freeze"));
        // D128: the exit code is 0 either way now, so the BODY is where a
        // script reads whether a task was started. This call did start one.
        assert_eq!(out["started"], json!(true));
        assert_eq!(out["id"], json!("0199-uuid"));
        assert_eq!(out["interval_started"], json!("2026-08-03T10:00:00Z"));
    }

    /// D128's `started` answers "did THIS invocation start a task", and
    /// `task.start` is idempotent on an already-active one (D105 answers
    /// `already_running` and opens no interval). Stamping `started: true`
    /// unconditionally made the body assert both at once — unreadable by any
    /// script, and a false claim inside a ruling one commit old.
    ///
    /// What this pins is the already-running case specifically: `started`
    /// false, the method's own `already_running` passed through untouched, and
    /// the identity still present. The general invariant — that the two may
    /// never both be true — is pinned on its own below, where no earlier
    /// assertion implies it.
    #[test]
    fn a_restart_of_an_already_running_task_is_not_a_start() {
        let answer = json!({
            "id": "0199-uuid", "already_running": true,
            "interval_started": "2026-08-03T10:00:00Z",
        });
        let out = pick_result(7, "Ship the freeze", answer);
        assert_eq!(
            out["started"],
            json!(false),
            "this invocation started nothing: {out}"
        );
        assert_eq!(
            out["already_running"],
            json!(true),
            "the method's own key is passed through untouched: {out}"
        );
        // The reader still ACTED on #7, so the identity is still there — that
        // is what the dashboard reads, and it is a different question.
        assert_eq!(out["short_id"], json!(7));
    }

    /// The invariant itself: over every answer `task.start` can give,
    /// `pick_result` may never report a start and an already-running task at
    /// once. A body claiming both is unreadable — `already_running: true` says
    /// no timer opened and `started: true` says one did.
    ///
    /// Its own test, and that separation is the point. In the case-specific
    /// test above, `started == false` is asserted first, so this conjunction
    /// could never be the FIRST thing to fail: no drift can satisfy the
    /// earlier probe and still violate this one. Left there it was a
    /// restatement wearing an invariant's words — a sentence claiming a
    /// guarantee that nothing could break. Here the conjunction is the only
    /// assertion, so it carries its own weight, and the already-running rows
    /// are what make it bite.
    #[test]
    fn pick_result_never_reports_a_start_and_an_already_running_task_at_once() {
        for answer in [
            json!({ "already_running": true, "id": "u", "interval_started": "t" }),
            json!({ "already_running": false, "id": "u", "interval_started": "t" }),
            json!({ "already_running": true, "auto_stopped": [], "status": "active" }),
            // The key absent at all: whatever it defaults to, it may not
            // produce the contradiction.
            json!({ "id": "u", "interval_started": "t" }),
        ] {
            let out = pick_result(7, "Ship the freeze", answer.clone());
            assert!(
                !(out["already_running"] == json!(true) && out["started"] == json!(true)),
                "a body may not claim a start AND an already-running task; \
                 answer was {answer}, body was {out}"
            );
        }
    }

    // ---- the reference examples parse ---------------------------------------

    /// Shell-style tokenizer for the reference examples: whitespace splits,
    /// quoted runs stay together, and anything from a redirection operator on
    /// is dropped because a shell never hands it to the program.
    ///
    /// Both halves matter. Without quote handling `--desc "Day job"` would
    /// arrive as three arguments and clap would reject a perfectly good
    /// example; without dropping the redirection, `tasqx api <<< '{…}'` would
    /// look like `api` with a stray `<<<` positional, when in argv terms it is
    /// simply `tasqx api` reading stdin.
    fn shell_split(cmd: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut quote: Option<char> = None;
        let mut started = false;
        for c in cmd.chars() {
            match c {
                '"' | '\'' if quote.is_none() => {
                    quote = Some(c);
                    started = true;
                }
                c if Some(c) == quote => {
                    quote = None;
                    started = true;
                }
                c if c.is_whitespace() && quote.is_none() => {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                c => {
                    cur.push(c);
                    started = true;
                }
            }
        }
        if started {
            out.push(cur);
        }
        if let Some(i) = out
            .iter()
            .position(|t| t.starts_with('<') || t.starts_with('>'))
        {
            out.truncate(i);
        }
        out
    }

    /// Review findings on `memory import`: the `*.md` filter was
    /// case-sensitive (README.MD silently skipped, on the OS whose filesystems
    /// are case-insensitive), and a UTF-8 BOM defeated the `# ` title match
    /// and leaked into the stored body.
    #[test]
    fn memory_import_reads_upper_case_md_and_strips_the_bom() {
        let dir = std::env::temp_dir().join(format!("tasqx-memimp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lower.md"), "# Lower doc\n\nbody").unwrap();
        std::fs::write(dir.join("UPPER.MD"), "\u{FEFF}# Upper doc\n\nbody").unwrap();

        let docs = memory_docs_from_path(dir.to_str().unwrap()).expect("both files import");
        assert_eq!(docs.len(), 2, "UPPER.MD must not be skipped");
        let titles: Vec<&str> = docs.iter().map(|d| d["title"].as_str().unwrap()).collect();
        assert!(
            titles.contains(&"Upper doc"),
            "the BOM must not defeat title derivation: {titles:?}"
        );
        for d in &docs {
            assert!(
                !d["body"].as_str().unwrap().starts_with('\u{FEFF}'),
                "the BOM must not reach the stored body"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #228.4: YAML frontmatter (the shape every file in a
    /// `~/.claude/.../memory/` directory carries) was indexed and shown as
    /// document body, so `originSessionId`/`modified`/`type` dominated search
    /// snippets over the prose that answers the query. It must be cut before
    /// storage, and its `title:` used when the body has no `# ` heading of
    /// its own.
    #[test]
    fn memory_import_strips_frontmatter_and_reads_its_title() {
        let dir = std::env::temp_dir().join(format!("tasqx-memimp-fm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("note.md"),
            "---\ntitle: \"release workflow\"\noriginSessionId: 71aa288e\nmodified: 2026-07-23\n---\nHow releases actually ship.\n",
        )
        .unwrap();

        let docs = memory_docs_from_path(dir.to_str().unwrap()).expect("import");
        assert_eq!(docs.len(), 1);
        let d = &docs[0];
        assert_eq!(
            d["title"].as_str().unwrap(),
            "release workflow",
            "frontmatter's `title:` must be used when there is no `# ` heading"
        );
        let body = d["body"].as_str().unwrap();
        assert!(
            !body.contains("originSessionId"),
            "frontmatter metadata must not reach the stored/indexed body: {body:?}"
        );
        assert!(
            body.contains("How releases actually ship."),
            "the real prose must survive the cut: {body:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every documented example must at least be a command this binary accepts.
    ///
    /// The executable guard in `tests/help.rs` only runs the `RunKind::Safe`
    /// half; the `NoRun` half — the mutating and long-running examples, more
    /// than twenty of them — had never been validated beyond "the string starts
    /// with `tasqx `". A typo'd flag or a renamed option in one of those would
    /// have shipped straight into `-h`, the manual and the HTML docs. Running
    /// them through clap is free and catches exactly that class of rot.
    #[test]
    fn every_documented_example_parses() {
        let mut checked = 0usize;
        for d in cmddoc::COMMAND_REF {
            for e in d.examples {
                // Two examples are shell pipelines rather than a single argv
                // (`tasqx init home && tasqx use home`, `tasqx export | tasqx
                // import -`). Each half is a real invocation, so parse both
                // rather than skipping the example: skipping would leave the
                // very examples that chain verbs — the ones most likely to go
                // stale — unchecked.
                for part in e.cmd.split("&&").flat_map(|p| p.split('|')) {
                    let argv = shell_split(part);
                    assert_eq!(
                        argv.first().map(String::as_str),
                        Some("tasqx"),
                        "{}: example segment {part:?} must start with `tasqx`",
                        d.verb
                    );
                    Cli::try_parse_from(&argv).unwrap_or_else(|err| {
                        panic!("{}: example {:?} does not parse:\n{err}", d.verb, e.cmd)
                    });
                    checked += 1;
                }
            }
        }
        // Both halves of COMMAND_REF are in scope; a filter or iteration bug
        // that checked nothing would otherwise pass silently.
        // 82 segments at the time `undo` was written, 85 with its three. The
        // floor said 78 against a real 82, so four segments could already have
        // been deleted with nothing going red — corrected here from the count
        // this guard itself reports, which is the only number worth trusting.
        //
        // Re-derive this whenever a row is added to COMMAND_REF — from the
        // count this guard itself reports, not by adding the number of examples
        // you just wrote. It said 52/54 for long enough that twenty examples
        // could have been deleted without the floor noticing, which is the
        // failure mode a floor exists to prevent: it kept reporting green while
        // guarding a quarter of what it claimed to.
        assert!(
            checked >= 85,
            "expected every example, only checked {checked}"
        );
    }

    /// `theme set` rejects an unknown name because it PERSISTS one; `--theme`
    /// and `$TASQX_THEME` apply to a single run, so they must still render —
    /// `tasqx --theme typo add "urgent thing"` may not refuse to record work
    /// because a colour scheme was misspelled. But the silence was wrong: a
    /// typo rendered nord with no hint, which reads as the flag being ignored
    /// rather than the name being wrong.
    #[test]
    fn an_unknown_theme_from_a_flag_warns_without_refusing() {
        assert!(
            validate_setting("theme.name", "geen-thema-xyz").is_err(),
            "fixture must be unknown"
        );
        assert!(
            validate_setting("theme.name", "nord").is_ok(),
            "a built-in must stay valid"
        );
        // The warning itself, not a proxy for it.
        let msg =
            unknown_theme_warning("theme.name", "geen-thema-xyz", "--theme").expect("must warn");
        assert!(msg.contains("geen-thema-xyz"), "{msg}");
        assert!(
            msg.contains("--theme"),
            "must name the layer it came from: {msg}"
        );
        assert!(
            msg.contains("theme list"),
            "must point somewhere useful: {msg}"
        );
        assert!(
            unknown_theme_warning("theme.name", "nord", "--theme").is_none(),
            "a real theme is silent"
        );

        // And the command still runs: rendering must never refuse over a theme.
        let ctx = build_ctx(Some("geen-thema-xyz"));
        assert_eq!(
            ctx.theme.name,
            theme::DEFAULT_THEME,
            "an unknown name falls back, it does not panic"
        );
    }

    /// #197 — a rejected layer must fall through to the NEXT layer in the D9
    /// chain, not straight to the bottom. `effective_setting` used to call
    /// `config::resolve` once and discard straight to the default the moment
    /// that single winning layer failed validation, so `--theme gruvbx` with
    /// `gruvbox` sitting in `config.toml` reported `nord`/`Source::Default` —
    /// a value the user had typed and persisted, thrown away over a typo in a
    /// *different, higher* layer.
    #[test]
    fn a_rejected_flag_falls_through_to_config_toml_not_the_default() {
        let s = config::find("theme.name").unwrap();
        let (value, source, warning) = effective_setting(s, Some("gruvbx"), Some("gruvbox"));
        assert_eq!(
            value, "gruvbox",
            "config.toml must win once --theme is rejected"
        );
        assert_eq!(
            source,
            config::Source::File,
            "and be credited as the source, not `default`"
        );
        let msg = warning.expect("the rejected flag must still warn");
        assert!(msg.contains("gruvbx"), "{msg}");
        assert!(msg.contains("--theme"), "{msg}");
        assert!(
            msg.contains("config.toml"),
            "the message must name the layer that actually won, not just say \
             \"using the default\": {msg}"
        );
    }

    /// The chain's floor is still the default when NOTHING validates — the
    /// case `effective_setting` already covered before #197, pinned so the
    /// per-layer walk cannot quietly drop it.
    #[test]
    fn a_rejected_flag_with_no_valid_layer_below_it_still_falls_to_the_default() {
        let s = config::find("theme.name").unwrap();
        let (value, source, warning) = effective_setting(s, Some("gruvbx"), None);
        assert_eq!(value, s.default);
        assert_eq!(source, config::Source::Default);
        let msg = warning.expect("must still warn");
        assert!(
            msg.contains("gruvbx") && msg.contains("using the default"),
            "{msg}"
        );
    }

    /// #192 — a broken or misspelled `config.toml` must earn a warning
    /// wherever `config_file_warnings_in` is asked, independent of the
    /// per-setting readers (`toml_value_in`/`toml_value_strict_in`), which
    /// stay silent or narrowly scoped for their own reasons.
    #[test]
    fn config_file_warnings_name_a_parse_error_and_every_unknown_key() {
        let dir = std::env::temp_dir().join(format!("tasqx-cfgwarn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // An unclosed table: not valid TOML at all.
        std::fs::write(dir.join("config.toml"), "[theme\nname = \"nord\"\n").unwrap();
        let warnings = config_file_warnings_in(&dir);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("not valid TOML"), "{warnings:?}");

        // Two misspelled keys: parse-clean TOML, but neither key is a setting.
        std::fs::write(
            dir.join("config.toml"),
            "[theme]\nnmae = \"nord\"\n[dashboard]\nwindw = \"month\"\n",
        )
        .unwrap();
        let warnings = config_file_warnings_in(&dir);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings.iter().any(|w| w.contains("theme.nmae")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("dashboard.windw")),
            "{warnings:?}"
        );

        // A clean file, and a missing one, must both stay silent.
        std::fs::write(dir.join("config.toml"), "[theme]\nname = \"gruvbox\"\n").unwrap();
        assert!(config_file_warnings_in(&dir).is_empty());
        std::fs::remove_file(dir.join("config.toml")).unwrap();
        assert!(
            config_file_warnings_in(&dir).is_empty(),
            "no file is a fresh install"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one conversion between `[daemon] idle_timeout` and what the daemon
    /// takes (D5), including both spellings of "never".
    ///
    /// The junk case is not hypothetical: `write_value_in` only guards
    /// `config set`, so a hand-edited `idle_timeout = "soon"` reaches this
    /// function as whatever the resolver handed back, and the wrong answer here
    /// is a daemon that exits at some invented deadline the file never asked
    /// for. Every failure lands on "never", the same direction
    /// `config_notify_enabled` falls in.
    #[test]
    fn an_idle_timeout_of_zero_or_junk_is_never_and_minutes_become_a_duration() {
        assert_eq!(idle_timeout_from_minutes("0"), None, "0 means never");
        assert_eq!(
            idle_timeout_from_minutes(config::find("daemon.idle_timeout").unwrap().default),
            None,
            "and the shipped default is that off switch"
        );
        assert_eq!(idle_timeout_from_minutes("soon"), None);
        assert_eq!(idle_timeout_from_minutes(""), None);
        assert_eq!(idle_timeout_from_minutes("-5"), None);
        assert_eq!(
            idle_timeout_from_minutes("15"),
            Some(Duration::from_secs(900)),
            "minutes, not seconds — a daemon that left after 15 seconds would be \
             indistinguishable from a crash"
        );
        assert_eq!(
            idle_timeout_from_minutes(" 1 "),
            Some(Duration::from_secs(60)),
            "the resolver hands back what the file held, whitespace and all"
        );
    }

    /// The theme validator must not be applied to settings that are not themes.
    ///
    /// `effective_setting` runs for EVERY registered setting, and a first
    /// version handed each one to a validator that hard-coded `theme.name`. So
    /// `notify.enabled = true` in config.toml was validated as a theme name,
    /// failed, and was discarded in favour of the default — the user's value
    /// silently dropped and reported as `default` by `config get`/`config list`.
    /// That is the silent-drop class (D27/D32/D35) reappearing inside the fix
    /// for a silent drop, which is exactly why this asserts over the whole
    /// registry rather than over `theme.name` alone.
    #[test]
    fn only_the_theme_setting_is_validated_as_a_theme() {
        for s in config::SETTINGS {
            if s.key == "theme.name" {
                continue;
            }
            // A value this setting can legitimately hold must survive the
            // effective-value resolution untouched, whatever it looks like.
            let held = match s.kind {
                config::Kind::Bool => "true",
                config::Kind::Uint => "4318",
                config::Kind::Minutes => "15",
                _ => "a-value-that-is-not-a-theme",
            };
            let (value, source, warning) = effective_setting(s, None, Some(held));
            assert_eq!(
                value, held,
                "{}: a file value must survive, not be replaced",
                s.key
            );
            assert!(
                warning.is_none(),
                "{}: must not warn about themes: {warning:?}",
                s.key
            );
            assert!(
                !matches!(source, config::Source::Default),
                "{}: the file supplied it, so the file must be credited",
                s.key
            );
        }
    }

    /// The CLI kept its own copy of the group_by allowlist while the MCP schema
    /// rendered from `engine::SUMMARY_GROUP_BY`. Adding a fourth axis would have
    /// made the API accept it and the CLI silently treat it as a filter token,
    /// so `tasqx report <new-axis>` would group by project and say nothing.
    #[test]
    fn the_cli_group_by_keywords_come_from_the_engine() {
        for axis in tasqx_core::engine::SUMMARY_GROUP_BY {
            let p = report_params(&[axis.to_string()], false, None, None, now_ts()).unwrap();
            assert_eq!(
                p["group_by"], axis,
                "{axis} must be read as a grouping, not a filter"
            );
            assert!(
                p.get("filter").is_none(),
                "{axis} must not also land in the filter"
            );
        }
        // A word that is not an axis stays a filter token. `+api` is a *valid*
        // one, so this still holds now that unknown tokens are rejected (D27):
        // routing to the filter is this test's business, whether the filter
        // then accepts the token is filter.rs's.
        let p = report_params(&["+api".to_string()], false, None, None, now_ts()).unwrap();
        assert_eq!(p["group_by"], tasqx_core::engine::SUMMARY_GROUP_BY[0]);
        assert_eq!(p["filter"], "+api");
    }

    /// Finding #11 (audit-2026-09): `tasqx report tags` fell through to the
    /// filter parser and answered with the filter DSL's whole error, which
    /// never mentions grouping at all — leaving the reader unsure whether
    /// tag-grouping exists and was mistyped, or does not exist. A bare word
    /// with no filter sigil that is not a valid axis must be diagnosed AS a
    /// group_by, naming the three real values.
    #[test]
    fn an_unrecognised_bare_group_by_names_itself_not_the_filter_grammar() {
        for bad in ["tags", "assignee"] {
            let err = report_params(&[bad.to_string()], false, None, None, now_ts())
                .err()
                .unwrap_or_else(|| panic!("{bad:?} must be refused"));
            assert!(
                err.message.contains("group_by"),
                "{bad:?}: must name group_by, not the filter grammar: {}",
                err.message
            );
            for axis in tasqx_core::engine::SUMMARY_GROUP_BY {
                assert!(
                    err.message.contains(axis),
                    "{bad:?}: must list {axis}: {}",
                    err.message
                );
            }
        }
        // A token that LOOKS like filter syntax (has a sigil/colon) must still
        // reach the filter parser — this is not blanket rejection of every
        // unrecognised first word.
        let p = report_params(&["project:x".to_string()], false, None, None, now_ts()).unwrap();
        assert_eq!(p["filter"], "project:x");
    }

    // ---- config edit: the glue between the screen and the disk -------------

    /// `config edit` could be severed from `config::write_value` entirely — a
    /// validate-only no-op, so the screen reported success and nothing reached
    /// the file — with all 362 tests green. The state machine was covered by 22
    /// tests; the twelve lines that turn its decision into a write were covered
    /// by none, because every TUI test stopped at the Action.
    #[test]
    fn a_save_action_actually_reaches_the_writer() {
        let s = config::find("theme.name").unwrap();
        let mut app = tui::settings::App::new(vec![build_row(
            s,
            "nord".into(),
            "default".into(),
            &["nord".to_string(), "mono".to_string()],
        )]);
        let mut saved = Vec::new();
        let mut seen: Vec<(String, String)> = Vec::new();

        apply_save(&mut app, "theme.name", "mono", None, &mut saved, |st, v| {
            seen.push((st.key.to_string(), v.to_string()));
            Ok(())
        });

        assert_eq!(
            seen,
            vec![("theme.name".to_string(), "mono".to_string())],
            "the writer must be called"
        );
        assert_eq!(
            saved,
            vec![("theme.name".to_string(), "mono".to_string())],
            "and the change recorded"
        );
    }

    /// A failed write must surface on the screen and must NOT be recorded as a
    /// change — otherwise the summary printed after the alt screen closes lists
    /// a setting the user's next command will not show.
    #[test]
    fn a_failed_write_is_reported_and_not_recorded() {
        let s = config::find("theme.name").unwrap();
        let mut app =
            tui::settings::App::new(vec![build_row(s, "nord".into(), "default".into(), &[])]);
        let mut saved = Vec::new();

        apply_save(&mut app, "theme.name", "mono", None, &mut saved, |_, _| {
            Err(ApiError::bad_request("disk on fire"))
        });

        assert!(
            saved.is_empty(),
            "a failed write must not count as a change"
        );
    }

    /// An unknown value must never reach the writer, on this path as much as on
    /// `config set` — `theme set` and `config set` already diverged on exactly
    /// this once.
    #[test]
    fn an_invalid_value_never_reaches_the_writer() {
        let s = config::find("theme.name").unwrap();
        let mut app =
            tui::settings::App::new(vec![build_row(s, "nord".into(), "default".into(), &[])]);
        let mut saved = Vec::new();
        let mut called = false;

        apply_save(
            &mut app,
            "theme.name",
            "not-a-theme",
            None,
            &mut saved,
            |_, _| {
                called = true;
                Ok(())
            },
        );

        assert!(!called, "validation must run before the writer");
        assert!(saved.is_empty());
    }

    /// The live preview is the only reason this screen exists, and nothing
    /// proved the frame theme follows the picker: hoisting `theme::load` out of
    /// the loop — resolving once instead of per frame — left the suite green,
    /// because the render test passed a theme in directly.
    #[test]
    fn the_frame_theme_follows_the_picker_and_yields_to_a_flag() {
        let s = config::find("theme.name").unwrap();
        let themes = vec!["nord".to_string(), "gruvbox".to_string()];
        let mut app =
            tui::settings::App::new(vec![build_row(s, "nord".into(), "default".into(), &themes)]);

        assert_eq!(
            frame_theme_name(&app, None),
            "nord",
            "browsing shows the saved value"
        );

        // Open the picker and move: the frame theme must follow the cursor
        // BEFORE anything is committed.
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Down));
        let previewed = frame_theme_name(&app, None);
        assert_ne!(
            previewed, "nord",
            "moving the picker must change the frame theme"
        );
        assert!(
            themes.contains(&previewed),
            "and it must be a real candidate: {previewed}"
        );

        // An explicit --theme outranks the preview: the screen must not paint
        // itself in a theme the surrounding terminal is not using.
        assert_eq!(frame_theme_name(&app, Some("mono")), "mono");
        assert_eq!(
            frame_theme_name(&app, Some("   ")),
            previewed,
            "a blank flag is not a flag"
        );
    }

    /// Dropping `Home::Store` settings from the snapshot loop — so
    /// `default_project` silently never appeared on the screen — left the suite
    /// green, because every TUI test built its rows by hand and none exercised
    /// the code deciding which settings a user sees.
    ///
    /// The first version of this guard reproduced that mistake: it built the
    /// rows by mapping over `SETTINGS` itself and then asserted the result had
    /// `SETTINGS.len()` entries. Both sides came from one constant, so it was a
    /// self-consistency check that could not fail however the real snapshot
    /// broke. It now calls `settings_rows` — the production function — and the
    /// expectation comes from the registry, which the function does not consult
    /// on the test's behalf.
    #[test]
    fn every_registered_setting_becomes_a_row() {
        let themes = vec!["nord".to_string()];
        // A store that answers, so a dropped `Home::Store` arm shows up as a
        // MISSING ROW rather than as an error from the lookup.
        let mut store = |_: &str| Ok(Some("inbox".to_string()));
        let rows = settings_rows(&mut store, &themes, None).expect("the snapshot must succeed");

        let seen: Vec<&str> = rows.iter().map(|r| r.setting.key).collect();
        for s in config::SETTINGS {
            assert!(
                seen.contains(&s.key),
                "{} never reached the screen: saw {seen:?}",
                s.key
            );
        }
        assert_eq!(
            rows.len(),
            config::SETTINGS.len(),
            "a setting reached the screen twice"
        );

        // The specific setting the original bug hid, named on purpose: it is
        // the only `Home::Store` entry, so a test that only counted rows would
        // pass if the store arm were replaced by anything that still pushed one.
        let dp = rows
            .iter()
            .find(|r| r.setting.key == "default_project")
            .expect("default_project");
        assert_eq!(
            dp.source, "store",
            "a store-homed row must say where it lives"
        );
        assert_eq!(
            dp.value, "inbox",
            "and must carry the value the store actually returned"
        );
        assert!(dp.choices.is_empty(), "a store setting offers no picker");

        let theme_row = rows.iter().find(|r| r.setting.key == "theme.name").unwrap();
        assert_eq!(
            theme_row.choices, themes,
            "the theme row must carry its candidates"
        );
    }

    /// `config get`/`list`/`edit` rendered a FAILED `core.capabilities` call as
    /// the empty string: `store_value` was `.ok()?`, so a dead daemon mid-request
    /// came back as a blank line and exit 0, indistinguishable from an unset
    /// setting. A failure is not a value.
    ///
    /// Driven through the closure seam because `Backend::Local` cannot fail this
    /// call at all — without the seam this error path is unreachable from a test.
    #[test]
    fn a_failing_store_lookup_is_an_error_not_a_blank_value() {
        let boom = || ApiError::internal("daemon transport error: broken pipe");
        let dp = config::find("default_project").expect("the store-homed setting");

        let err = setting_value(&mut |_| Err(boom()), dp, None)
            .expect_err("a failed lookup must not resolve to a value");
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(
            err.message.contains("broken pipe"),
            "the cause must survive: {}",
            err.message
        );

        // The screen snapshot must refuse for the same reason: showing a blank
        // `default_project` row invites the user to "fix" a setting that is fine.
        // `Row` is not Debug, so match rather than `expect_err`.
        match settings_rows(&mut |_| Err(boom()), &["nord".to_string()], None) {
            Ok(rows) => panic!("the snapshot painted a failure as {} rows", rows.len()),
            Err(e) => assert!(e.message.contains("broken pipe"), "{}", e.message),
        }

        // And the success path still resolves normally, so the guard above is
        // about the ERROR and not about the function refusing everything.
        let (v, src) = setting_value(&mut |_| Ok(Some("work".into())), dp, None).expect("ok");
        assert_eq!((v.as_str(), src.as_str()), ("work", "store"));
    }

    // ---- report scope (DESIGN.md §12-D24) -----------------------------------

    /// D24 rule 1 has to survive the CLI→core hop. `--all` is the only escape
    /// from the exclude-cancelled default, so if the flag parses but never
    /// reaches `report.summary` as `all: true` the user has no way to see
    /// abandoned work at all — and the failure is silent, since a report that
    /// quietly omits rows still looks like a perfectly good report.
    #[test]
    fn report_all_flag_reaches_core_as_all_true() {
        assert_eq!(
            report_params(&[], true, None, None, now_ts()).unwrap()["all"],
            json!(true)
        );
        // Absent by default: core's `all` defaults to false, and sending an
        // explicit `false` would be the same thing said twice.
        assert!(report_params(&[], false, None, None, now_ts())
            .unwrap()
            .get("all")
            .is_none());
    }

    /// The group_by-then-filter split is positional and easy to break; `--all`
    /// is a flag and must compose with both halves rather than displacing them.
    #[test]
    fn report_all_composes_with_group_by_and_filter() {
        let p = report_params(
            &["status".to_string(), "project:x".to_string()],
            true,
            None,
            None,
            now_ts(),
        )
        .unwrap();
        assert_eq!(p["group_by"], "status");
        assert_eq!(p["filter"], "project:x");
        assert_eq!(p["all"], json!(true));
    }

    #[test]
    fn report_all_flag_parses() {
        match add_of(&["tasqx", "report", "--all"]) {
            Command::Report { all, .. } => assert!(all),
            _ => panic!("expected a report command"),
        }
        match add_of(&["tasqx", "report"]) {
            Command::Report { all, .. } => assert!(!all, "default is the D24 exclusion"),
            _ => panic!("expected a report command"),
        }
    }

    /// `--all` cannot reach the HTML page, so accepting the combination would
    /// silently ignore it — the exact failure mode D24 exists to stop. clap must
    /// reject it rather than parse it into a no-op.
    #[test]
    fn report_all_is_rejected_alongside_html() {
        assert!(Cli::try_parse_from(["tasqx", "report", "--html", "--all"]).is_err());
    }

    /// The two `report_params` tests above cover disjoint halves — clap parses
    /// `--all` into the struct, and `report_params` turns a `true` into
    /// `all: true` — but nothing exercised the JOIN between them. `run_report`
    /// is the only place the parsed flag is handed to `report_params`, and
    /// severing it there (`report_params(&args, false)`) left the entire suite
    /// green. This drives the real seam end-to-end against a live engine, so the
    /// one escape hatch from D24 cannot be silently disconnected.
    #[test]
    fn run_report_all_flag_reaches_the_engine_end_to_end() {
        let ctx = Ctx::new(theme::default_theme(), theme::Caps::PLAIN);
        // One open task, one cancelled: the total is 1 under D24's default and 2
        // only if `--all` survives the whole path.
        let count = |all: bool| -> i64 {
            let e = tasqx_core::Engine::open_in_memory().unwrap();
            e.project_create(&json!({ "name": "P" })).unwrap(); // D23
            for title in ["live", "abandoned"] {
                e.task_add(&json!({ "title": title, "project": "P" }))
                    .unwrap();
            }
            e.task_cancel(&json!({ "ref": "2" })).unwrap();
            let mut be = Backend::Local(e);
            let (result, _) = run_report(&mut be, &ctx, vec![], all, None, None, None, false)
                .expect("report ran");
            result["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g["count"].as_i64().unwrap())
                .sum()
        };

        assert_eq!(
            count(false),
            1,
            "the default must leave the cancelled task out"
        );
        assert_eq!(
            count(true),
            2,
            "--all must reach the engine and bring it back"
        );
    }

    /// `chart burndown` draws "remaining open work", so a cancelled task must
    /// leave the scope entirely — otherwise abandoning a task shows up as a flat
    /// line instead of a burn-down, and the "N left" footer keeps counting work
    /// nobody will ever do. `task.list` keeps its literal "no filter = all rows"
    /// contract (D24 is a *report* rule), so the exclusion is spelled out at the
    /// The burndown's scope is EVERY task, cancelled ones included.
    ///
    /// It used to exclude them, and that was right for the reconstruction it
    /// was written against: forwards, a task whose closing event fell outside
    /// the window was guessed to be open, so a cancelled task hung open on the
    /// chart forever and the only cure was to drop it. Backwards, its cancel
    /// date closes it like any other close — and excluding it would now delete
    /// the task from the days it was genuinely open, which is a different wrong
    /// answer to the same question.
    #[test]
    fn the_burndown_scope_is_every_task_and_carries_its_status() {
        let e = Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "P" })).unwrap();
        for t in ["one", "two", "three"] {
            e.task_add(&json!({ "title": t, "project": "P" })).unwrap();
        }
        e.task_cancel(&json!({ "ref": "2" })).unwrap();
        e.task_done(&json!({ "ref": "3" })).unwrap();
        let id_of = |short: &str| -> String {
            e.task_get(&json!({ "ref": short })).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let (live, gone, finished) = (id_of("1"), id_of("2"), id_of("3"));

        for scope in [None, Some("P".to_string())] {
            let (members, _) = burndown_members(&e, &scope).expect("burndown scope resolved");
            let by_id = |id: &str| members.iter().find(|m| m.id == id);

            let open = by_id(&live).expect("scope {scope:?} lost the open task");
            assert!(open.open_now, "the open task must be marked open");

            // Present, and marked closed — that is what lets the chart draw it
            // on the days before it was cancelled and not after.
            let cancelled = by_id(&gone).expect("scope lost the cancelled task");
            assert!(
                !cancelled.open_now,
                "a cancelled task must be carried as closed, not excluded"
            );
            let done = by_id(&finished).expect("scope lost the done task");
            assert!(!done.open_now, "a done task must be carried as closed");
        }
    }

    /// A project whose name contains a space silently produced an EMPTY
    /// burndown: `format!("project:{p} ...")` tokenized to `project:Home` plus a
    /// stray `Renovation`, and the resulting parse failure was swallowed by
    /// `.ok()` + `.unwrap_or_default()`. The chart then rendered "0 left -
    /// cleared" over a project with open work, and exited 0.
    ///
    /// Both halves are asserted because either alone leaves the bug: the count
    /// proves the composed filter is now correct, the parenthesised and quoted
    /// names prove it survives the metacharacters that broke it.
    #[test]
    fn burndown_scope_survives_project_names_with_metacharacters() {
        for name in ["Home Renovation", "a (b)", "say \"hi\"", "work and play"] {
            let e = tasqx_core::Engine::open_in_memory().unwrap();
            e.project_create(&json!({ "name": name })).unwrap();
            e.project_create(&json!({ "name": "Other" })).unwrap();
            for title in ["paint", "sand"] {
                e.task_add(&json!({ "title": title, "project": name }))
                    .unwrap();
            }
            // A task in a different project: without it, a filter that collapsed
            // to "match everything" would pass this test too.
            e.task_add(&json!({ "title": "unrelated", "project": "Other" }))
                .unwrap();

            let (members, label) =
                burndown_members(&e, &Some(name.to_string())).expect("scope resolved");
            assert_eq!(
                members.len(),
                2,
                "project {name:?} must scope to its own 2 tasks"
            );
            assert_eq!(label, name, "the chart label is the project name as given");
        }
    }

    /// The swallow itself, independent of any one bad name: when the composed
    /// filter does not parse, the caller must be able to SEE that. While
    /// `burndown_members` returned a bare tuple there was no way to tell "this
    /// project has no open work" from "the query failed", and the chart rendered
    /// the same cleared line for both.
    ///
    /// Driven through a project name that the engine accepts but that no filter
    /// grammar could ever be composed for by naive interpolation, so this stays
    /// a guard on error PROPAGATION rather than on today's quoting rules.
    #[test]
    fn a_burndown_filter_failure_is_reported_rather_than_drawn_as_empty() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        // A LIVE project with no tasks in it: `task.list` still parses and
        // validates the filter fine, so this asserts the honest empty case
        // stays empty and Ok — D109 only sharpened the UNKNOWN-name case
        // below, not this one.
        e.project_create(&json!({ "name": "empty-project" }))
            .unwrap();
        let (members, _) =
            burndown_members(&e, &Some("empty-project".to_string())).expect("parses");
        assert!(
            members.is_empty(),
            "a live project with no open work is legitimately empty, not an error"
        );

        // An unknown project name is now its OWN propagated error (D109):
        // `project:nope` in a filter is `not_found`, naming it, the same
        // guarantee `task.add --project`/`task.modify --project` already give
        // (D23) — no longer a silent empty burndown for what is, in fact, a
        // typo.
        let unknown = burndown_members(&e, &Some("nope".to_string()))
            .expect_err("an unknown project name must propagate as an error, not an empty chart");
        assert_eq!(unknown.code, tasqx_core::ErrorCode::NotFound);
        assert!(unknown.message.contains("nope"), "{}", unknown.message);

        // ...and a filter that cannot parse AT ALL must still come back as
        // Err. `"` alone is unterminable: quoting it is fine, but this
        // bypasses `quote` to simulate any future composition bug reaching
        // `task.list`.
        let bad = dispatch(
            &e,
            "task.list",
            &json!({ "filter": "project:\"oops", "fields": ["id"] }),
        );
        assert!(
            bad.is_err(),
            "an unparseable filter must be an error at the API boundary"
        );
    }

    /// The burndown scope has no status filter at all, so no status can be
    /// forgotten by it.
    ///
    /// It used to be `NOT_CANCELLED`, a filter spelling out every status that
    /// counts — and dropping `status:backlog` from it once left the whole suite
    /// green while every backlog task silently vanished from every chart. D60
    /// removed the filter rather than guarding it: cancelled tasks close on
    /// their cancel date now, so there is nothing to exclude, and a class of
    /// silent omission goes with it. This asserts the absence, because a filter
    /// creeping back in is exactly how it would return.
    #[test]
    fn the_burndown_scope_filters_on_no_status_at_all() {
        let e = Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "P" })).unwrap();
        for t in ["a", "b"] {
            e.task_add(&json!({ "title": t, "project": "P" })).unwrap();
        }
        // A backlog task: the status the old hand-written filter lost.
        e.task_add(&json!({ "title": "later", "project": "P", "wait": "2099-01-01" }))
            .unwrap();
        e.task_cancel(&json!({ "ref": "2" })).unwrap();

        let (members, _) = burndown_members(&e, &None).expect("scope resolved");
        assert_eq!(
            members.len(),
            3,
            "every task is in scope, whatever its status — got {members:?}"
        );
        // And each carries its own openness, which is what replaces the filter.
        assert_eq!(
            members.iter().filter(|m| m.open_now).count(),
            2,
            "the cancelled one is carried as closed, not dropped"
        );
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
            (
                vec!["tasqx", "add", "Ship it", "--remind", "friday 9am"],
                "friday 9am",
            ),
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
            (
                vec!["tasqx", "modify", "42", "--due", "-1d"],
                Some("-1d"),
                None,
                None,
                None,
            ),
            (
                vec!["tasqx", "modify", "42", "--due", "-2w"],
                Some("-2w"),
                None,
                None,
                None,
            ),
            (
                vec!["tasqx", "modify", "42", "--scheduled", "-1d"],
                None,
                Some("-1d"),
                None,
                None,
            ),
            (
                vec!["tasqx", "modify", "42", "--wait", "-3d"],
                None,
                None,
                Some("-3d"),
                None,
            ),
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
                Command::Modify {
                    due,
                    scheduled,
                    wait,
                    remind,
                    ..
                } => {
                    assert_eq!(due.as_deref(), want_due, "due — argv: {argv:?}");
                    assert_eq!(
                        scheduled.as_deref(),
                        want_sched,
                        "scheduled — argv: {argv:?}"
                    );
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
        match add_of(&[
            "tasqx",
            "add",
            "Late thing",
            "--due",
            "-1d",
            "--scheduled",
            "-2d",
        ]) {
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
                Command::Modify {
                    r#ref, priority, ..
                } => {
                    assert_eq!(r#ref, "42");
                    assert_eq!(priority.as_deref(), Some("H"), "verb: {verb}");
                }
                _ => panic!("expected a modify command for {verb}"),
            }
        }
    }

    /// `tasqx delete 3` is what a human reaches for, and it exited with
    /// "unrecognized subcommand" while suggesting `complete`, `l` and `d` —
    /// three verbs, none of them the right one. tasqx has no hard delete by
    /// design (DESIGN.md §7, "No hidden bulk delete": cancellation is
    /// reversible and logged), so the fix is to make the word people actually
    /// type land on that verb.
    #[test]
    fn delete_aliases_resolve_to_cancel() {
        for verb in ["cancel", "delete", "del", "rm"] {
            match add_of(&["tasqx", verb, "42"]) {
                Command::Cancel { r#ref, .. } => assert_eq!(r#ref, "42", "verb: {verb}"),
                _ => panic!("expected a cancel command for {verb}"),
            }
        }
    }

    #[test]
    fn modify_collects_clear_fields_and_sugar() {
        match add_of(&[
            "tasqx",
            "modify",
            "42",
            "New",
            "title",
            "due:friday",
            "--clear",
            "remind",
            "--clear",
            "recurrence",
        ]) {
            Command::Modify {
                r#ref, rest, clear, ..
            } => {
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
        match add_of(&[
            "tasqx",
            "modify",
            "42",
            "--priority",
            "L",
            "--expected-rev",
            "7",
        ]) {
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
        let args: Vec<String> = if cfg!(windows) {
            vec!["/C".into(), "exit".into()]
        } else {
            vec![]
        };
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
        assert!(
            !cands.is_empty(),
            "this platform has no browser launcher at all"
        );
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
            Command::Docs {
                out,
                no_open,
                stdout,
            } => {
                assert_eq!(out.as_deref(), Some("guide.html"));
                assert!(
                    !no_open,
                    "--out implies no-open at the behaviour level, not the flag"
                );
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
            Command::Docs {
                out,
                no_open,
                stdout,
            } => {
                assert!(out.is_none() && !no_open && !stdout);
            }
            _ => panic!("expected a docs command"),
        }
    }

    /// The default path must be stable across runs (so `docs` does not litter)
    /// and must actually be an HTML file (so a browser renders rather than
    /// downloads).
    #[test]
    fn docs_default_path_is_stable_and_html() {
        let a = docs_default_path().expect("a test machine has a home directory");
        let b = docs_default_path().expect("a test machine has a home directory");
        assert_eq!(a, b, "the default path must not vary between invocations");
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("html"));
    }

    /// The default guide lands in the user's OWN cache directory, never in the
    /// shared system temp dir.
    ///
    /// It used to be `$TMPDIR/tasqx-docs/tasqx-guide-<ver>.html` — a fully
    /// predictable name inside a world-writable directory. Any other local
    /// account could pre-create `tasqx-docs/` (owned by them, non-sticky, so
    /// `fs.protected_symlinks` does not apply) holding a symlink at that name,
    /// and the victim's next `tasqx docs` would truncate whatever it pointed
    /// at: `create_dir_all` succeeds on a directory it does not own and
    /// `fs::write` follows symlinks. Same setup at mode 0755 wedges every other
    /// user's `tasqx docs` on EACCES forever.
    #[test]
    fn docs_default_path_is_under_the_user_cache_dir() {
        let cache = directories::ProjectDirs::from("dev", "tasqx", "tasqx")
            .expect("a test machine has a home directory")
            .cache_dir()
            .to_path_buf();
        let p = docs_default_path().expect("a test machine has a home directory");
        assert!(
            p.starts_with(&cache),
            "the guide must live under {}, got {}",
            cache.display(),
            p.display()
        );
        // Guarded, because a machine may legitimately point XDG_CACHE_HOME into
        // $TMPDIR; what must never happen is the path landing there while the
        // cache dir is somewhere else.
        let tmp = std::env::temp_dir();
        if !cache.starts_with(&tmp) {
            assert!(
                !p.starts_with(&tmp),
                "the guide must not be written into the shared temp dir: {}",
                p.display()
            );
        }
    }

    // ---- the config verb ----------------------------------------------------

    /// `config list` has to show BOTH homes. A user asking "what are my
    /// settings" expects their default project in the list, and omitting it
    /// because it lives in the store rather than the file would be a lie by
    /// omission. Writing is a different question — reading is not.
    #[test]
    fn config_list_reports_both_homes_with_their_source() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "work" })).unwrap();
        let mut be = Backend::Local(e);
        let ctx = Ctx::new(theme::default_theme(), theme::Caps::PLAIN);

        let (result, text) =
            run_config(&mut be, &ctx, &ConfigAction::List, None).expect("list ran");

        let rows = result["settings"].as_array().expect("a settings array");
        let keys: Vec<&str> = rows.iter().map(|r| r["key"].as_str().unwrap()).collect();
        assert!(keys.contains(&"theme.name"), "{keys:?}");
        assert!(
            keys.contains(&"default_project"),
            "the store home must appear too: {keys:?}"
        );

        let dp = rows.iter().find(|r| r["key"] == "default_project").unwrap();
        assert_eq!(dp["value"], "work", "the store value must be the live one");
        assert_eq!(dp["home"], "store");
        assert!(
            text.contains("default_project"),
            "the human table must show it too"
        );
    }

    /// tasqx audit 2026-09 #223: `config.rs::Setting` has always carried a
    /// `summary` — `config list --json` includes it per row — but
    /// `tasqx --json config get <key>` answered `{key, value}` with no way
    /// for a caller to learn what `tokens.enabled` or `otlp.port` actually do
    /// short of reading the source. Plain-text `config get` stdout stays
    /// untouched on purpose: `config_get_is_silent_about_a_well_typed_value`
    /// and `a_wrong_typed_value_stays_silent_outside_config` already pin it
    /// to exactly the value on stdout and nothing on stderr, for
    /// `x=$(tasqx config get key)` scripting — so the summary can only be
    /// additive JSON, not a second stdout line.
    #[test]
    fn config_get_json_carries_the_settings_own_summary() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        let mut be = Backend::Local(e);
        let ctx = Ctx::new(theme::default_theme(), theme::Caps::PLAIN);

        let key = "otlp.enabled";
        let want = config::find(key)
            .expect("otlp.enabled is a real setting")
            .summary;
        let (result, _) = run_config(
            &mut be,
            &ctx,
            &ConfigAction::Get {
                key: key.to_string(),
            },
            None,
        )
        .expect("get ran");
        assert_eq!(
            result["summary"], want,
            "`tasqx --json config get {key}` must carry the setting's own \
             summary: {result:?}"
        );
    }

    /// #76.3: `otlp.enabled = true` persisted with no complaint even when no
    /// daemon was reachable to act on it — the receiver only binds inside
    /// `tasqx daemon`, so the config alone does nothing. `otlp_daemon_warning`
    /// is the pure decision `set_setting` prints on stderr; this pins it
    /// directly rather than through a spawned binary, at a socket path nothing
    /// listens on: the ambient socket is whatever daemon the MACHINE runs, and
    /// a developer with the real one up saw this test fail for that alone.
    #[test]
    fn otlp_daemon_warning_fires_only_for_enabling_with_no_daemon_reachable() {
        let dead =
            std::env::temp_dir().join(format!("tasqx-no-daemon-{}.sock", std::process::id()));
        let dead = dead.to_str().expect("utf-8 path");
        // Setting it true with nothing listening on the socket: warn, and
        // name both the key/value and why it matters.
        let w = otlp_daemon_warning_at("otlp.enabled", "true", dead)
            .expect("nothing listens on a fresh tempdir path");
        assert!(w.contains("otlp.enabled"), "{w}");
        assert!(
            w.contains("tasqx daemon"),
            "must say where the receiver actually binds: {w}"
        );

        // Turning it OFF needs no daemon and gets no warning.
        assert_eq!(
            otlp_daemon_warning_at("otlp.enabled", "false", dead),
            None,
            "disabling otlp needs no daemon and must stay silent"
        );
        // An unrelated key's value must never trip this check.
        assert_eq!(
            otlp_daemon_warning_at("tokens.enabled", "true", dead),
            None,
            "the check is scoped to otlp.enabled, not every boolean setting"
        );
    }

    /// `config get` on a key nobody registered must say so and list the valid
    /// ones. Today an unknown key in config.toml is read by nothing and
    /// reported by nothing, so a typo looks like it worked.
    #[test]
    fn config_get_rejects_an_unknown_key_and_names_the_valid_ones() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        let mut be = Backend::Local(e);
        let ctx = Ctx::new(theme::default_theme(), theme::Caps::PLAIN);

        let err = run_config(
            &mut be,
            &ctx,
            &ConfigAction::Get {
                key: "theme.nmae".into(),
            },
            None,
        )
        .expect_err("an unknown key must not succeed");
        assert_eq!(err.code, tasqx_core::ErrorCode::BadRequest);
        assert!(
            err.message.contains("theme.name"),
            "must list valid keys: {}",
            err.message
        );
    }

    /// `--theme` is the highest-precedence layer in D9, and `config` — the one
    /// command whose stated job is naming the layer that won — could not see it.
    /// Driven before the fix: with `[theme] name = "gruvbox"` on disk,
    /// `tasqx --theme mono theme list` rendered "mono ← active" while
    /// `tasqx --theme mono config get theme.name` answered "gruvbox". The binary
    /// disagreed with its own settings report. `Source::Flag` was constructible
    /// only from a unit test and unreachable from every user-facing path.
    #[test]
    fn config_reports_the_flag_layer_when_one_is_given() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        let mut be = Backend::Local(e);
        let ctx = Ctx::new(theme::default_theme(), theme::Caps::PLAIN);

        let (result, _) = run_config(
            &mut be,
            &ctx,
            &ConfigAction::Get {
                key: "theme.name".into(),
            },
            Some("mono"),
        )
        .expect("get ran");
        assert_eq!(
            result["value"], "mono",
            "the flag must win over file and default"
        );

        let (listed, _) =
            run_config(&mut be, &ctx, &ConfigAction::List, Some("mono")).expect("list ran");
        let row = listed["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "theme.name")
            .unwrap()
            .clone();
        assert_eq!(row["value"], "mono");
        assert_eq!(
            row["source"], "--theme",
            "the SOURCE column must name the flag"
        );
    }

    /// Derived from the registry, never a key list: every `Choices::Themes`
    /// setting is validated, so a second Themes-valued setting cannot join
    /// the registry and silently skip the check — the exact hole the old
    /// `key == "theme.name"` match left open.
    #[test]
    fn every_themes_choiced_setting_validates_its_value() {
        let themed: Vec<_> = config::SETTINGS
            .iter()
            .filter(|s| s.choices == config::Choices::Themes)
            .collect();
        assert!(
            !themed.is_empty(),
            "the registry carries at least one Themes setting, or this guard is vacuous"
        );
        for s in themed {
            assert!(
                validate_setting(s.key, "geen-thema-xyz").is_err(),
                "{}",
                s.key
            );
            assert!(validate_setting(s.key, "nord").is_ok(), "{}", s.key);
        }
    }

    /// `theme set bogus` was rejected while `config set theme.name bogus` wrote
    /// it and exited 0 — the primitive was looser than its own alias, which is
    /// backwards. The written name silently does nothing on every run from then
    /// on, because `theme::load` falls back to the default for an unknown name.
    #[test]
    fn config_set_validates_the_value_not_just_the_key() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        let mut be = Backend::Local(e);
        let ctx = Ctx::new(theme::default_theme(), theme::Caps::PLAIN);

        let err = run_config(
            &mut be,
            &ctx,
            &ConfigAction::Set {
                key: "theme.name".into(),
                value: "not-a-theme".into(),
            },
            None,
        )
        .expect_err("an unknown theme must not be persisted");
        assert_eq!(err.code, tasqx_core::ErrorCode::BadRequest);
        assert!(
            err.message.contains("theme list"),
            "must point at the lister: {}",
            err.message
        );
    }

    /// D21 put default_project in the store on purpose. `config set` must
    /// refuse it and name the verb that owns it, rather than writing a second
    /// copy into config.toml where nothing validates it against the store.
    #[test]
    fn config_set_refuses_a_store_owned_key_and_names_its_verb() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        let mut be = Backend::Local(e);
        let ctx = Ctx::new(theme::default_theme(), theme::Caps::PLAIN);

        let err = run_config(
            &mut be,
            &ctx,
            &ConfigAction::Set {
                key: "default_project".into(),
                value: "work".into(),
            },
            None,
        )
        .expect_err("a store-owned key must not be writable through config set");
        assert_eq!(err.code, tasqx_core::ErrorCode::BadRequest);
        assert!(
            err.message.contains("tasqx use"),
            "must name the verb: {}",
            err.message
        );
    }
}
