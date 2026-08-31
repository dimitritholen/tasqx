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

mod argv;
mod chart;
pub mod cmddoc;
mod command;
mod complete;
pub mod config;
mod docs;
mod html;
mod manual;
mod render;
mod sugar;
mod theme;
mod tokens;
mod tui;

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
use clap::Parser;
use serde_json::{json, Value};

use tasqx_core::markdown::TimeFormat;
use tasqx_core::{
    daemon, datetime, dispatch, handle_envelope, notify, ApiError, Engine, ErrorCode, McpServer,
    Scope,
};

use command::{
    ChartKind, Cli, Command, ConfigAction, McpAction, MemoryAction, ThemeAction, TokensAction,
};
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
const CLEARABLE: [&str; 8] = [
    "project",
    "priority",
    "due",
    "scheduled",
    "wait",
    "remind",
    "recurrence",
    "estimate",
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
fn exit_on_parse_error(e: &clap::Error, filter_command: bool) -> ! {
    if filter_command && e.kind() == ErrorKind::UnknownArgument {
        if let Some(ContextValue::String(offender)) = e.get(ContextKind::InvalidArg) {
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
/// one of the commands that needed it.
fn emit(text: &str) {
    let mut out = std::io::stdout();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => {
            eprintln!("error: cannot write to stdout: {e}");
            exit(1);
        }
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

    // Not `Cli::parse()`: filter tokens like `-needs` must reach the grammar,
    // and the only way to keep that from disarming clap's flag handling is to
    // hide the dash before clap looks. See `argv`.
    let pre = argv::prepass(std::env::args_os());
    let mut cli = match Cli::try_parse_from(pre.argv) {
        Ok(cli) => cli,
        Err(e) => exit_on_parse_error(&e, pre.filter_command),
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
            eprintln!("error [{}]: {}", code_str(&e), e.message);
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
fn execute(cli: Cli) -> Exit {
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
    if matches!(&cli.command, Some(Command::Dashboard)) && !cli.json {
        use std::io::IsTerminal;
        let (out_tty, in_tty) = (
            std::io::stdout().is_terminal(),
            std::io::stdin().is_terminal(),
        );
        let size = terminal_size(&ctx.caps, out_tty, in_tty);
        if let Some(msg) = dashboard_refusal(&ctx.caps, out_tty, in_tty, size) {
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
            eprintln!("error: {msg}");
            exit(1);
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
    let (stdout_tty, stdin_tty) = {
        use std::io::IsTerminal;
        (
            std::io::stdout().is_terminal(),
            std::io::stdin().is_terminal(),
        )
    };
    let fits = terminal_size(&ctx.caps, stdout_tty, stdin_tty).is_some_and(|(w, h)| {
        dashboard_refusal(&ctx.caps, stdout_tty, stdin_tty, Some((w, h))).is_none()
    });
    let verb_screen = matches!(&cli.command, Some(Command::Dashboard)) && !cli.json;
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

    Exit::Out(match cli.command {
        None => run_list(&mut backend, &ctx, &[]),
        // Only the `--json` spelling reaches here: the screen leaves as
        // `SelfFramed` above, for the same D57-hint reason the bare invocation
        // does.
        Some(Command::Dashboard) => run_dashboard_json(&mut backend, &ctx),
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
            },
            &clear,
            expected_rev,
        ),
        Some(Command::List { filter }) => run_list(&mut backend, &ctx, &filter),
        Some(Command::Agenda { filter, days }) => run_agenda(&mut backend, &ctx, &filter, days),
        Some(Command::Start {
            r#ref,
            keep,
            correlation,
        }) => run_start(&mut backend, &ctx, r#ref, keep, &correlation),
        Some(Command::Stop { r#ref }) => run_stop(&mut backend, &ctx, r#ref),
        Some(Command::Done { r#ref, correlation }) => {
            run_done(&mut backend, &ctx, r#ref, &correlation)
        }
        Some(Command::Show { r#ref }) => run_show(&mut backend, &ctx, r#ref),
        Some(Command::Cancel { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.cancel", r#ref),
        Some(Command::Reopen { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.reopen", r#ref),
        Some(Command::Undo) => run_undo(&mut backend, &ctx),
        Some(Command::Annotate { r#ref, text }) => run_annotate(&mut backend, &ctx, r#ref, text),
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
        Some(Command::Report { args, all, .. }) => run_report(&mut backend, &ctx, args, all),
        Some(Command::Config { action }) => {
            run_config(&mut backend, &ctx, &action, theme_flag.as_deref())
        }
        Some(Command::Memory { action }) => run_memory(&mut backend, &action),
        Some(Command::Tokens { action }) => run_tokens(&mut backend, &ctx, &action),
        Some(Command::Export { filter }) => run_export(&mut backend, &filter),
        Some(Command::Import { file }) => run_import(&mut backend, file),
        Some(Command::Next) => run_next(&mut backend, &ctx),
        Some(Command::Pick { filter }) => run_pick(&mut backend, &ctx, &filter),
        Some(Command::Why { r#ref }) => run_why(&mut backend, &ctx, r#ref),
        Some(Command::Chart { .. }) => unreachable!("handled above"),
        Some(Command::Theme { .. }) => unreachable!("handled above"),
        Some(Command::Docs { .. }) => unreachable!("handled above"),
        Some(Command::Watch { .. }) => unreachable!("handled above"),
        Some(Command::Api) => unreachable!("handled above"),
        Some(Command::Daemon { .. }) => unreachable!("handled above"),
        Some(Command::Mcp { .. }) => unreachable!("handled above"),
        Some(Command::Manual { .. }) => unreachable!("handled above"),
        Some(Command::Completions { .. }) => unreachable!("handled above"),
    })
}

/// Resolve the active theme (flag > $TASQX_THEME > config > default) and detect
/// terminal capability, producing the render context every command shares.
/// Warn, but never refuse, when a theme name cannot be honoured.
///
/// `theme set` and `config set` REJECT an unknown name because they persist it,
/// and a persisted name that silently does nothing is a lie the user carries
/// forever. `--theme` and `$TASQX_THEME` apply to one invocation, so the same
/// treatment would be wrong in the other direction: `tasqx --theme typo add
/// "urgent thing"` must still capture the task. Refusing to record work because
/// a colour scheme was misspelled is the one thing a task manager may not do.
///
/// So the fallback stays and the silence goes. Before this, a typo'd `--theme`
/// or a stale `$TASQX_THEME` rendered nord with no hint, which reads as the
/// flag being ignored rather than the name being wrong.
///
/// **A hand-edited `config.toml` is the third case, and it warns too.** It
/// persists like `config set`, but refusing to start would lock the user out of
/// the one tool that can fix the file — the D28 inversion, one config layer
/// over. It was left silent on the theory that `tasqx config` would report it;
/// `tasqx config` was in fact reporting the ignored name as though it were in
/// effect, so nothing in the tool said the name had been dropped. Warning on
/// every command is the point rather than the cost: a persisted bad name is
/// wrong on every run, and stderr keeps stdout scriptable.
///
/// The message, or `None` when there is nothing to say.
///
/// Split from the printing so it is testable at all: the emitting version can
/// only be observed through process-global stderr, and a first version of this
/// was pinned by a test that checked `validate_setting` and that `build_ctx`
/// did not panic — so disabling the warning outright left the suite green.
/// `key` is the setting being resolved, NOT a constant. Hard-coding
/// `"theme.name"` here meant every OTHER setting was validated as a theme name,
/// so `notify.enabled = true` failed that check and was silently replaced by its
/// default — the caller's value dropped, and `config get` reporting `default`.
/// `validate_setting` answers `Ok` for a key with no closed value set, so
/// passing the real key is also what keeps this correct as settings are added.
fn unknown_theme_warning(key: &str, name: &str, source: &str) -> Option<String> {
    validate_setting(key, name).err().map(|_| {
        format!("warning: unknown theme {name:?} from {source}; using the default (try `tasqx theme list`)")
    })
}

/// One setting's value **as it will actually be used**, the layer that supplied
/// it, and the complaint if a layer's value had to be discarded.
///
/// This is the one place the difference between "what a layer said" and "what
/// the tool will do" is resolved, and every reader goes through it — `build_ctx`
/// on the render path, and `config get`/`config list`/`config edit` through
/// `setting_value`. Before it, `config::resolve` was the answer for both, and it
/// only knows precedence: a `config.toml` naming a theme that does not exist was
/// dropped by `theme::load` on the way to the renderer while `config get`
/// happily reported the dropped name. One question, two surfaces, two answers —
/// and the one the user could read was the wrong one.
///
/// The fallback is `s.default` with `Source::Default` on purpose: that IS where
/// the value comes from once the named layer is discarded, and crediting
/// `config.toml` for a value it did not supply would be the same lie one field
/// over.
fn effective_setting(
    s: &config::Setting,
    flag: Option<&str>,
    file: Option<&str>,
) -> (String, config::Source, Option<String>) {
    let (value, source) = config::resolve(s, flag, file);
    match unknown_theme_warning(s.key, &value, &source.label(s)) {
        None => (value, source, None),
        Some(msg) => (s.default.to_string(), config::Source::Default, Some(msg)),
    }
}

fn build_ctx(flag: Option<&str>) -> Ctx {
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
    let theme = theme::load(&name, dir.as_deref());
    Ctx::new(theme, Caps::detect()).with_cols(theme::detect_cols())
}

/// The stems of `themes/*.toml`, sorted. A missing directory is an empty list —
/// built-ins need no files.
///
/// Extracted from `theme list` when `config edit` needed the same list for its
/// picker. Two copies would have let the printed list and the interactive one
/// disagree about which themes exist, and only the interactive one can act on
/// the answer.
fn user_theme_names() -> Vec<String> {
    let Some(dir) = themes_dir() else {
        return Vec::new();
    };
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
    user
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

/// Read `[notify] enabled` from `config.toml` (DESIGN.md §9).
///
/// Native OS toasts are opt-in: absent config means `false`, so every failure
/// mode here — no config dir, no file, malformed TOML, wrong type — lands on
/// "don't notify", never on "notify anyway", and a fresh install is quiet.
fn config_notify_enabled() -> bool {
    let s = config::find("notify.enabled").expect("notify.enabled is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v == "true"
}

/// Read `[tokens] enabled` from `config.toml` (#17, DESIGN §10).
///
/// Off by default: like [`config_notify_enabled`], every failure mode — no
/// config dir, no file, malformed TOML, wrong type — lands on "don't attribute",
/// so a fresh install never parses AI tool transcripts until the user opts in.
fn config_tokens_enabled() -> bool {
    let s = config::find("tokens.enabled").expect("tokens.enabled is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v == "true"
}

/// Read `[detail] time_format` from `config.toml`.
///
/// Falls back to `Both` on every failure — no config dir, no file, malformed
/// TOML, or a value the registry would have refused had it come through
/// `config set` — matching how [`config_tokens_enabled`] treats its own failure
/// modes. A hand-edited `config.toml` is the one path that reaches the writer's
/// validation, so this side must not trust what it reads.
fn config_detail_time_format() -> TimeFormat {
    let s = config::find("detail.time_format").expect("detail.time_format is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    match v.as_str() {
        "iso" => TimeFormat::Iso,
        "relative" => TimeFormat::Relative,
        _ => TimeFormat::Both,
    }
}

/// Read `[otlp] enabled` from `config.toml` (#18, DESIGN §10).
///
/// Off by default: like [`config_tokens_enabled`], every failure mode lands on
/// "don't listen", so a fresh install never opens a local telemetry port until
/// the user opts in.
fn config_otlp_enabled() -> bool {
    let s = config::find("otlp.enabled").expect("otlp.enabled is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v == "true"
}

/// Read `[otlp] port` from `config.toml` (#18), falling back to the registered
/// default (4318). The registry already validated the range, so a parse failure
/// here can only be the default, which is a valid `u16`.
fn config_otlp_port() -> u16 {
    let s = config::find("otlp.port").expect("otlp.port is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v.parse::<u16>().unwrap_or_else(|_| {
        s.default
            .parse()
            .expect("the registered default is a valid port")
    })
}

/// Read `[daemon] idle_timeout` from `config.toml` (D5): how long the daemon
/// may sit with no clients and no work before it exits by itself.
///
/// Off unless the user asked for it, and every failure mode lands on off — the
/// same direction [`config_notify_enabled`] and [`config_tokens_enabled`] fall
/// in, for a sharper reason: the surprise here is not a missing toast but a
/// background process that vanishes mid-session, and nothing in a daemon's
/// output would explain it after the fact.
fn config_daemon_idle_timeout() -> Option<Duration> {
    let s =
        config::find("daemon.idle_timeout").expect("daemon.idle_timeout is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    idle_timeout_from_minutes(&v)
}

/// The registry's minutes string as the daemon's `Option<Duration>`.
///
/// Split out of [`config_daemon_idle_timeout`] because it is the whole of the
/// decision and the only part testable without a config directory: `0` and
/// anything unparseable are both "never exit". Unparseable is reachable — the
/// writer's range check only covers `config set`, and a hand-edited
/// `idle_timeout = "soon"` reaches here as the default string either way.
fn idle_timeout_from_minutes(value: &str) -> Option<Duration> {
    let minutes = value.trim().parse::<u64>().ok()?;
    (minutes > 0).then(|| Duration::from_secs(minutes * 60))
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
    /// The socket is kept alongside the connection because it is the only
    /// honest answer to "where did this write go?" — `config store` reports it,
    /// and recomputing it later would re-resolve the flag/env and could name a
    /// different target than the one actually connected to.
    Remote {
        conn: daemon::Conn,
        socket: String,
    },
}

impl Backend {
    /// The socket this process is routing through, or `None` when it holds the
    /// store itself.
    fn remote_socket(&self) -> Option<&str> {
        match self {
            Backend::Local(_) => None,
            Backend::Remote { socket, .. } => Some(socket.as_str()),
        }
    }
}

impl Backend {
    /// Route one method+params to the core dispatch, locally or via the daemon.
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, ApiError> {
        match self {
            Backend::Local(engine) => dispatch(engine, method, params),
            Backend::Remote { conn, .. } => {
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
            return dirs
                .data_dir()
                .join("tasqx.sock")
                .to_string_lossy()
                .into_owned();
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
            return Ok(Backend::Remote {
                conn,
                socket: target,
            });
        }
        // The connect failed and this command is about to address a different
        // store than the last one did, if a daemon recently retired here. Say
        // so once (D74). Deliberately not on the `--no-daemon` path: there the
        // operator chose the in-process store themselves.
        report_daemon_retirement(&target);
    }
    Ok(Backend::Local(open_engine()?))
}

/// Where an idle-retired daemon leaves its note (D74): beside `config.toml`,
/// keyed by the socket — the D57 marker precedent, because state lives in a
/// sibling file, never inside the user's config. The socket is sanitized for a
/// filename (a Unix socket address is a path) and recorded verbatim inside the
/// file; the content compare in [`report_daemon_retirement`] is what is
/// trusted, the filename is only a rendezvous.
fn daemon_retired_marker(socket: &str) -> Option<PathBuf> {
    let sanitized: String = socket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    config::config_dir().map(|d| d.join(format!("daemon-retired-{sanitized}")))
}

/// D74's client half: the first command whose daemon connect fails reads the
/// retirement note, reports the transition on stderr, and consumes the marker
/// so the note is said once — D57's marker-then-print discipline, inverted
/// (print-then-delete, because a note that cannot be delivered should survive
/// for the next command to try).
///
/// The recorded socket must equal the one this command resolved: two addresses
/// can sanitize to one filename, and a note about a different daemon is not
/// this command's transition.
fn report_daemon_retirement(target: &str) {
    let Some(marker) = daemon_retired_marker(target) else {
        return;
    };
    let Ok(body) = std::fs::read_to_string(&marker) else {
        return;
    };
    let field = |key: &str| {
        body.lines()
            .find_map(|l| l.strip_prefix(key).map(str::trim))
            .filter(|v| !v.is_empty())
    };
    if field("socket ") != Some(target) {
        return;
    }
    let owned = field("store ").filter(|s| *s != "-");
    let when = field("retired ");
    eprintln!(
        "tasqx: note: the daemon at {target} left on its idle timeout{}{}; this and later \
         commands open the local store in-process — `tasqx config store` names it",
        when.map(|w| format!(" at {w}")).unwrap_or_default(),
        owned
            .map(|s| format!(" and its store {s} is no longer being served"))
            .unwrap_or_default(),
    );
    let _ = std::fs::remove_file(&marker);
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

/// Say that the project name may have been CUT, when it may have been.
///
/// The core answers a missing project with `no project named X (create it with
/// `tasqx init X`)`, which is right when X is what the user typed. From an
/// unquoted `project:` sugar token it is not: the token ends at the first space,
/// so `project:My "Big" Project` asked about `My` and the message then advised
/// creating a project that already existed under a longer name — confidently
/// naming a fragment as though the user had typed it.
///
/// We cannot tell a typo from a truncation here, so the message stops claiming
/// to. It says where the name came from and gives the spelling that names a
/// whole one; the `init` advice is kept, because a typo is still the likelier
/// case and it is now offered rather than asserted.
/// Takes the name rather than the whole `ParsedAdd` because `modify` has already
/// moved its fields into the `set` map by the time the call fails, and both
/// verbs must answer this identically (§12-D13).
fn name_the_cut(e: ApiError, cut_name: Option<&str>) -> ApiError {
    if e.code != ErrorCode::NotFound {
        return e;
    }
    let Some(name) = cut_name else { return e };
    if !e.message.starts_with("no project named") {
        return e;
    }
    ApiError::new(
        e.code,
        format!(
            "no project named {name:?} — but that is only the part of a `project:` token \
             before the first space, so a name with spaces must be quoted: \
             project:\"{name} …\". If {name:?} really is the whole name, create it with \
             `tasqx init {name:?}`."
        ),
        e.data,
    )
}

/// The project name IF it might have been cut short by the sugar tokenizer.
fn cut_project_name(parsed: &sugar::ParsedAdd) -> Option<String> {
    parsed
        .project_may_be_truncated
        .then(|| parsed.project.clone())
        .flatten()
}

fn run_add(be: &mut Backend, ctx: &Ctx, title: Vec<String>, flags: sugar::AddFlags) -> CmdOutcome {
    // argv goes in unjoined: the shell's argument boundaries are information the
    // parser needs (see `sugar::parse_add`), and joining destroys them.
    let parsed = sugar::parse_add(&title, flags)?;
    // Taken before the fields are moved into `params`; see `name_the_cut`.
    let cut = cut_project_name(&parsed);

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
    let result = be
        .call("task.add", &params)
        .map_err(|e| name_the_cut(e, cut.as_deref()))?;
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
    let parsed = sugar::parse_add(&rest, flags)?;
    // Taken before the fields are moved into `set`; see `name_the_cut`.
    let cut = cut_project_name(&parsed);
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
        set.insert(
            "scheduled".into(),
            Value::String(datetime::parse_when(&s, now)?),
        );
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
        set.insert(
            "estimate".into(),
            Value::String(datetime::parse_duration(&e)?),
        );
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
        result = be
            .call("task.modify", &params)
            .map_err(|e| name_the_cut(e, cut.as_deref()))?;
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
    // Otherwise `from_argv`, never `join(" ")`: the shell's argument boundaries
    // are information the filter parser needs, exactly as on the write path
    // (see `sugar::parse_add`). Joining loses which spaces the user quoted.
    let filter_str = if filter.is_empty() {
        "@working".to_string()
    } else {
        tasqx_core::filter::from_argv(filter)
    };
    let params = json!({ "filter": filter_str, "sort": ["-urgency"] });
    let result = be.call("task.list", &params)?;
    let text = render::task_table(ctx, &result);
    Ok((result, text))
}

/// The filter DSL for "not finished": every status `Status::is_open` calls open,
/// spelled as an `or` chain.
///
/// Derived from `Status::ALL` rather than written out, for the reason that
/// constant's own doc gives — the names used to live by hand in ten places, and
/// a status missing from one of them makes tasks stop appearing without anything
/// failing. There is no `@open` keyword in the grammar to lean on: `KEYWORDS` is
/// `@working` and `@blocked`, and neither is this set.
fn open_statuses_filter() -> String {
    tasqx_core::types::Status::ALL
        .iter()
        .filter(|s| s.is_open())
        .map(|s| format!("status:{}", s.as_str()))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// `tasqx agenda` — the same question `list` asks, ordered by time.
///
/// # No new API method, deliberately
///
/// `task.list` already takes a filter and a sort (`dispatch::METHODS`), and this
/// verb needs nothing else FROM the store: the day grouping, the horizon and the
/// earlier-of-two-dates ordering are all functions of fields every row already
/// carries. An `agenda.*` method would be a second way to ask one question, and
/// D50 narrows the API on purpose — the surface that has to stay frozen for v1
/// is the one worth keeping small.
///
/// The ordering is not sent as a `sort` key for the same reason it could not be:
/// the agenda key is `min(due, scheduled)`, which is not in `engine::SORT_KEYS`
/// and would have to be added to the frozen contract to express a presentation
/// choice. `-urgency` is asked for instead — byte-identical to what [`run_list`]
/// sends — and `agenda_select` stable-sorts by the instant, so two tasks landing
/// at the same minute keep the urgency ranking the rest of the tool gives them.
///
/// # The filter default is NOT `list`'s, and the reason is a measured one
///
/// `list` defaults to `@working`, and `@working` is pending|active. A task with
/// a `scheduled` (or `wait`) date in the future sits in **backlog** until that
/// instant arrives — `types::effective_status` promotes it on the way out of the
/// store — so `@working` excludes, precisely, everything that is scheduled for
/// later. Driven against the real binary: `add "Quarterly deps audit"
/// scheduled:2026-08-04` then `agenda` on the 3rd showed no Tuesday at all. An
/// agenda that cannot show what is scheduled for tomorrow is not an agenda, so
/// the default here is every OPEN status instead — the same set minus nothing,
/// plus the backlog `@working` was built to hide from a "what can I do now" view.
///
/// The set is DERIVED from `Status::ALL` and `Status::is_open`, never typed out:
/// a sixth status would otherwise reach `list` and silently miss this view, which
/// is the drift `Status::ALL` exists to end (its own doc names the ten places the
/// names used to be spelled by hand).
///
/// # How a caller's own filter is combined with it
///
/// D24's resolution order, the one `report.summary` already uses: a caller who
/// named a status is taken literally, so `tasqx agenda status:done` shows done
/// tasks rather than an empty table; anything else is ANDed with the open set.
/// The question is asked of the PARSED tree via `Filter::constrains_status`,
/// because a lexical `contains("status")` both over-matches (`+status-page`) and
/// under-matches (`@working`).
///
/// Composed on the wire rather than applied to the rows after they arrive, so
/// the store does the narrowing it is good at — and so `tasqx agenda` on a store
/// with a thousand closed tasks does not report "1000 hidden" under every run.
///
/// A filter this build cannot parse is sent VERBATIM (`unwrap_or(true)`), so the
/// engine's refusal quotes the caller's words instead of parentheses this
/// function added — D45's rule about where a bad value is refused.
fn run_agenda(be: &mut Backend, ctx: &Ctx, filter: &[String], days: Option<usize>) -> CmdOutcome {
    let now = jiff::Timestamp::now();
    let asked = if filter.is_empty() {
        String::new()
    } else {
        tasqx_core::filter::from_argv(filter)
    };
    let names_status = tasqx_core::filter::Filter::parse(&asked, now)
        .map(|f| f.constrains_status())
        .unwrap_or(true);
    let filter_str = match (names_status, asked.is_empty()) {
        (true, _) => asked,
        (false, true) => open_statuses_filter(),
        // Parenthesised on both sides: the caller's filter may itself be an
        // `or`, and `a or b and c` would bind the default to `b` alone.
        (false, false) => format!("({asked}) and ({})", open_statuses_filter()),
    };

    let params = json!({ "filter": filter_str, "sort": ["-urgency"] });
    let result = be.call("task.list", &params)?;

    let a = render::agenda_select(&result, days.unwrap_or(AGENDA_DEFAULT_DAYS), now);
    let text = render::agenda_text(ctx, &a);
    // The result the `--json` terminal prints is the agenda's own, not the raw
    // `task.list` answer: see `render::agenda_json` for why the two flags have
    // to describe one set of rows.
    Ok((render::agenda_json(&a), text))
}

/// Widen a `task.start` / `task.done` params object with whichever correlation
/// facts were given on the command line (#12, #72).
///
/// Mirrors `Correlation::apply` on the engine side deliberately: present keys
/// only, so a flagless `tasqx done 4` sends byte-for-byte the object it sent
/// before these flags existed, and the engine's `opt_str_nonempty` never has to
/// distinguish "absent" from "explicitly null".
fn apply_correlation(params: &mut Value, c: &command::CorrelationArgs) {
    for (key, value) in [
        ("client", &c.client),
        ("session_id", &c.session_id),
        ("prompt_id", &c.prompt_id),
        ("transcript_path", &c.transcript_path),
    ] {
        if let Some(v) = value {
            params[key] = json!(v);
        }
    }
}

fn run_start(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    keep: bool,
    correlation: &command::CorrelationArgs,
) -> CmdOutcome {
    let mut params = json!({ "ref": r#ref, "keep": keep });
    apply_correlation(&mut params, correlation);
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

fn run_done(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    correlation: &command::CorrelationArgs,
) -> CmdOutcome {
    let mut params = json!({ "ref": r#ref });
    apply_correlation(&mut params, correlation);
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

/// `tasqx undo` — the safety net (DESIGN §5 example 12).
///
/// No params on the wire, and none to collect: `event.revert` reverses the
/// newest event in the log or refuses. The whole of this function is therefore
/// the call and the line it prints — and that line is the point, because
/// "undone" with nothing after it is exactly the answer a user cannot check
/// against what they actually did.
fn run_undo(be: &mut Backend, ctx: &Ctx) -> CmdOutcome {
    let result = be.call("event.revert", &json!({}))?;
    let text = render::undone(ctx, &result);
    Ok((result, text))
}

fn run_annotate(be: &mut Backend, ctx: &Ctx, r#ref: String, text: Vec<String>) -> CmdOutcome {
    let body = text.join(" ");
    let result = be.call("annotation.add", &json!({ "ref": r#ref, "body": body }))?;
    let out = render::annotated(ctx, &result);
    Ok((result, out))
}

/// `tasqx tag` / `tasqx untag`, the two spellings of one params shape.
///
/// One function for both, the way [`run_dep`] serves `dep`/`undep`: the params
/// are identical and only the method name differs, so two copies would be two
/// places for the tag normalisation to fall out of step.
///
/// The words go through [`sugar::tag_arguments`] and not straight onto the wire,
/// which is what makes `tasqx tag 42 +api` and `tasqx modify 42 +api` name the
/// same tag. Sending `+api` verbatim would have created a tag literally called
/// `+api`, invisible next to the `api` the sugar path writes and unreachable by
/// the `+api` filter token.
fn run_tag(
    be: &mut Backend,
    ctx: &Ctx,
    method: &str,
    r#ref: String,
    tags: &[String],
) -> CmdOutcome {
    let names = sugar::tag_arguments(tags)?;
    let result = be.call(method, &json!({ "ref": r#ref, "tags": names }))?;
    let text = render::tag_result(ctx, &result, method == "tag.add", &names);
    Ok((result, text))
}

fn run_dep(
    be: &mut Backend,
    ctx: &Ctx,
    method: &str,
    r#ref: String,
    depends_on: String,
) -> CmdOutcome {
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

/// D22: take a project out of rotation. Same shape as [`run_use`] — the name is
/// a lookup the core resolves, so an unknown one is `not_found` (exit 4) from
/// the engine and not from a second copy of the rule here.
///
/// The interesting half is the response, not the request: `project.archive`
/// clears the default project when it archives the one the `default_project`
/// key names, and `default_cleared` is how it says so. Dropping that field on
/// the floor here would make the CLI the surface on which "where does a bare
/// `tasqx add` land" changed with nobody told — the invisible-state failure D21
/// and D22 exist to close, arriving through the one verb that was never wired
/// to a terminal.
fn run_archive(be: &mut Backend, ctx: &Ctx, name: String) -> CmdOutcome {
    let result = be.call("project.archive", &json!({ "name": name }))?;
    let text = render::project_archived(ctx, &result);
    Ok((result, text))
}

fn run_projects(be: &mut Backend, ctx: &Ctx, all: bool) -> CmdOutcome {
    let result = be.call("project.list", &json!({ "include_archived": all }))?;
    let text = render::project_table(ctx, &result);
    Ok((result, text))
}

/// Build the `report.summary` params from the CLI's positional args plus the
/// `--all` flag. Split out of [`run_report`] so the CLI→core contract can be
/// asserted without standing up a backend.
fn report_params(args: &[String], all: bool) -> Value {
    // First token, if a known group_by keyword, selects grouping; the rest is
    // the filter. Otherwise everything is the filter (group_by defaults).
    let mut group_by = tasqx_core::engine::SUMMARY_GROUP_BY[0].to_string();
    let mut rest: &[String] = args;
    if let Some(first) = args.first() {
        // The engine's own list, not a third copy. The MCP schema already
        // renders from this const; the CLI hard-coded the same three names, so
        // adding a fourth axis would have made the API accept it and the CLI
        // silently treat it as a filter token instead.
        if tasqx_core::engine::SUMMARY_GROUP_BY.contains(&first.as_str()) {
            group_by = first.clone();
            rest = &args[1..];
        }
    }
    // Same reasoning as `group_by` above, and the same constant pattern:
    // `SUMMARY_METRICS` exists to stop the CLI keeping a private second copy of
    // this list. It had one anyway, sitting three lines from the import — so a
    // fifth metric would have reached the JSON API and the MCP schema while
    // `tasqx report` silently kept asking for four.
    let mut params = json!({
        "group_by": group_by,
        "metrics": tasqx_core::engine::SUMMARY_METRICS,
    });
    if !rest.is_empty() {
        params["filter"] = Value::String(tasqx_core::filter::from_argv(rest));
    }
    // Sent only when set: core already defaults `all` to false, and an explicit
    // `false` would be the same thing said twice.
    if all {
        params["all"] = Value::Bool(true);
    }
    params
}

fn run_report(be: &mut Backend, ctx: &Ctx, args: Vec<String>, all: bool) -> CmdOutcome {
    let params = report_params(&args, all);
    let group_by = params["group_by"]
        .as_str()
        .unwrap_or(tasqx_core::engine::SUMMARY_GROUP_BY[0])
        .to_string();
    let result = be.call("report.summary", &params)?;
    let text = render::report(ctx, &result, &group_by);
    Ok((result, text))
}

/// `tasqx memory add|search|rm|import` (DESIGN.md §12-D41).
fn run_memory(be: &mut Backend, action: &MemoryAction) -> CmdOutcome {
    match action {
        MemoryAction::Add {
            title,
            body,
            source,
        } => {
            let mut params = json!({ "title": title, "body": body });
            if let Some(s) = source {
                params["source"] = json!(s);
            }
            let result = be.call("memory.add", &params)?;
            let text = format!(
                "Stored {}  ·  {}\n",
                render::san(result["id"].as_str().unwrap_or("?")),
                render::san(title)
            );
            Ok((result, text))
        }
        MemoryAction::Search {
            query,
            limit,
            scope,
            raw,
        } => {
            let mut params = json!({ "query": query.join(" ") });
            if let Some(n) = limit {
                params["limit"] = json!(n);
            }
            if let Some(s) = scope {
                params["scope"] = json!(s);
            }
            if *raw {
                params["raw"] = json!(true);
            }
            let result = be.call("memory.search", &params)?;
            let mut text = String::new();
            for hit in result["hits"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                let title = render::san(hit["title"].as_str().unwrap_or(""));
                let kind = render::san(hit["kind"].as_str().unwrap_or("?"));
                let src = render::san(hit["source"].as_str().unwrap_or("—"));
                let snip = render::san(hit["snippet"].as_str().unwrap_or(""));
                let id = render::san(hit["id"].as_str().unwrap_or("?"));
                text.push_str(&format!("{title}  ({kind} · {src})\n  {snip}\n  id {id}\n"));
            }
            let count = result["count"].as_u64().unwrap_or(0);
            text.push_str(&format!("{count} hit(s)\n"));
            // On a miss, name the expression that produced it (D69). Every
            // word of a plain query is a required phrase, so a question typed
            // as a sentence comes back exactly as empty as a subject nobody
            // ever wrote down — and the two need different next moves.
            if count == 0 {
                if let Some(matched) = result["matched"].as_str() {
                    text.push_str(&format!(
                        "  every term was required: {}\n",
                        render::san(matched)
                    ));
                }
            }
            Ok((result, text))
        }
        MemoryAction::Show { id } => {
            let result = be.call("memory.get", &json!({ "id": id }))?;
            let source = result["source"].as_str().unwrap_or("—");
            let text = format!(
                "{}  ({})\n{}\n",
                render::san(result["title"].as_str().unwrap_or("?")),
                render::san(source),
                render::san(result["body"].as_str().unwrap_or("")),
            );
            Ok((result, text))
        }
        MemoryAction::Rm { id } => {
            let result = be.call("memory.remove", &json!({ "id": id }))?;
            let text = format!("Removed {}\n", render::san(id));
            Ok((result, text))
        }
        MemoryAction::Import { path } => run_memory_import(be, path),
    }
}

/// `tasqx tokens recompute [--apply]` (DESIGN.md §12-D50, Decision 3).
///
/// The polarity flip happens here and nowhere else: the CLI speaks opt-in
/// destruction (`--apply`) while the engine speaks opt-out safety
/// (`dry_run`, defaulting true). Sending `dry_run` explicitly rather than
/// omitting it keeps this command's behaviour pinned to its own flag instead
/// of to whatever default a future engine revision ships.
fn run_tokens(be: &mut Backend, ctx: &Ctx, action: &TokensAction) -> CmdOutcome {
    match action {
        TokensAction::Recompute { apply } => {
            let result = be.call("tokens.recompute", &json!({ "dry_run": !apply }))?;
            let text = render::tokens_recompute(ctx, &result);
            Ok((result, text))
        }
    }
}

/// One doc per file. A directory imports its direct `*.md` children; finding
/// none is an error, not `Imported 0` at exit 0 — the same never-say-nothing
/// rule `import` learned for truncated task files.
fn run_memory_import(be: &mut Backend, path: &str) -> CmdOutcome {
    // Two-phase (review finding): ALL file I/O and title derivation happen
    // before a single write, then one `memory.import` lands the batch in one
    // transaction with replace-by-source semantics — a failure imports
    // nothing, and a re-run replaces instead of duplicating.
    let docs = memory_docs_from_path(path)?;
    let result = be.call("memory.import", &json!({ "docs": docs }))?;
    let text = format!(
        "Imported {} doc(s) into memory\n",
        result["imported"].as_u64().unwrap_or(0)
    );
    Ok((result, text))
}

/// Read `path` (a file, or a directory's direct `*.md` children) into
/// `memory.import` doc objects. Pure I/O — no store access — so the whole
/// failure surface of an import is exhausted before anything is written.
fn memory_docs_from_path(path: &str) -> Result<Vec<Value>, tasqx_core::ApiError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read {path}: {e}")))?;
    let files: Vec<std::path::PathBuf> = if meta.is_dir() {
        let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(path)
            .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read {path}: {e}")))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            // Case-insensitive: README.MD is a markdown file on every
            // platform, and skipping it silently on the OS whose filesystems
            // are case-insensitive was the exact wrong place to be strict.
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            })
            .collect();
        found.sort();
        if found.is_empty() {
            return Err(tasqx_core::ApiError::bad_request(format!(
                "no .md files found in {path} — memory import takes a markdown file or a \
                 directory containing them"
            )));
        }
        found
    } else {
        vec![std::path::PathBuf::from(path)]
    };

    let mut docs = Vec::new();
    for file in &files {
        let body = std::fs::read_to_string(file).map_err(|e| {
            tasqx_core::ApiError::bad_request(format!("cannot read {}: {e}", file.display()))
        })?;
        // A UTF-8 BOM would defeat the `# ` heading match below AND end up in
        // the stored body and the index; strip it once, here.
        let body = body.strip_prefix('\u{FEFF}').unwrap_or(&body);
        // Title: the first `# ` heading, else the file stem. The heading STAYS
        // in the body — the title is an index entry, not a cut.
        let title = body
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                file.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_string()
            });
        docs.push(json!({ "title": title, "body": body, "source": file.display().to_string() }));
    }
    Ok(docs)
}

fn run_export(be: &mut Backend, filter: &[String]) -> CmdOutcome {
    let mut params = json!({});
    if !filter.is_empty() {
        params["filter"] = Value::String(tasqx_core::filter::from_argv(filter));
    }
    let result = be.call("store.export", &params)?;
    // A filter selects a subset, so edges pointing out of it are trimmed to keep
    // the document self-contained. Warn on stderr, never stdout: stdout IS the
    // JSON and a note there would corrupt every pipe.
    let dropped = result
        .get("dropped_dependencies")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if dropped > 0 {
        eprintln!(
            "note: dropped {dropped} dependency edge(s) pointing outside the exported set; \
             widen the filter to keep them"
        );
    }
    // Human output IS the canonical JSON document (git-diffable, greppable).
    //
    // D37: the whole document, not just its `tasks` array. This is the surface
    // almost every user actually restores from, and printing one section of a
    // two-section document made the CLI lose exactly what the core had just
    // been taught to carry — projects, their archived state, and the default.
    // `import` has always accepted an object with a `tasks` key as well as a
    // bare array, so files written by this build and by every earlier one both
    // still restore; only the direction that can carry MORE has changed.
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&result).unwrap_or_default()
    );
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
    // Anything else used to fall through to an empty array, so a truncated or
    // wrong file was answered with `Imported 0 task(s)` and exit 0 — the one
    // outcome a restore must never be told.
    let shape = |found: &str| {
        let src = if file == "-" {
            "stdin".to_string()
        } else {
            file.clone()
        };
        tasqx_core::ApiError::bad_request(format!(
            "cannot import {src}: {found} — expected the `export` shape, \
             a bare array of tasks or an object with a `tasks` array"
        ))
    };
    // D37: an object is forwarded WHOLE, not reduced to its `tasks` array. The
    // array was all a document used to hold; now it also carries `projects` and
    // `default_project`, and a verb that unwraps one section discards the rest —
    // silently, since the import would still report every task restored. A bare
    // array is still wrapped, because that is precisely what an older export is:
    // a document with no projects section, which `store.import` reads as "infer
    // them" rather than refusing.
    let params = match parsed {
        Value::Array(_) => json!({ "tasks": parsed }),
        Value::Object(ref o) => {
            if !o.contains_key("tasks") {
                return Err(shape("the JSON object has no `tasks` key"));
            }
            parsed.clone()
        }
        Value::String(_) => return Err(shape("the top level is a JSON string")),
        Value::Number(_) => return Err(shape("the top level is a JSON number")),
        Value::Bool(_) => return Err(shape("the top level is a JSON boolean")),
        Value::Null => return Err(shape("the top level is JSON null")),
    };
    let result = be.call("store.import", &params)?;
    let n = result.get("imported").and_then(Value::as_i64).unwrap_or(0);
    let p = result
        .get("projects_imported")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // A project row the caller did not send is a write they did not ask for, so
    // it is named on the human surface too, not only in the JSON (D37).
    let minted: Vec<&str> = result
        .get("projects_created")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let d = result
        .get("docs_imported")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // D39: `docs_imported` is computed and returned, so a human surface must
    // render it — a restore that also restored your memory docs and never said
    // so would make D41's export completeness unobservable. Mentioned only
    // when nonzero: pre-D41 documents carry no docs, and "0 doc(s)" on every
    // legacy restore is noise about a section the document never had.
    let mut text = if d > 0 {
        format!("Imported {n} task(s), {p} project(s), {d} memory doc(s)\n")
    } else {
        format!("Imported {n} task(s), {p} project(s)\n")
    };
    if !minted.is_empty() {
        text.push_str(&format!(
            "note: the document carried no `projects` section, so {} created from the tasks: {}\n",
            if minted.len() == 1 {
                "1 project was"
            } else {
                "projects were"
            },
            minted.join(", ")
        ));
    }
    Ok((result, text))
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

// ---- `tasqx pick`: the interactive chooser (DESIGN.md §10, D55) -------------

/// The refusal a non-interactive `tasqx pick` gives.
///
/// A constant because it is asserted from two places — the unit test below and
/// `tests/help.rs`, which drives the real binary — and a message pinned by a
/// hand-copied substring in each is a message that drifts out from under both.
/// It names the two commands that answer the same question without a screen,
/// because "needs a terminal" on its own leaves a scripter with nothing to type
/// next; `config edit`'s refusal earns its exit 2 the same way.
const PICK_NEEDS_A_TERMINAL: &str =
    "`tasqx pick` needs an interactive terminal on stdin and stdout \
     (one of them is piped, redirected, or TERM=dumb). `tasqx next` picks the \
     highest-urgency task for you and `tasqx start <ref>` starts it.";

/// `tasqx pick` — choose a task on a full-screen list, and start it.
///
/// The two pieces this function owns are the two the state machine must not:
/// the candidate snapshot and the write. Everything between them is `tui::pick`.
///
/// # The TTY gate is NOT here
///
/// It runs in [`execute`], above `open_backend`, and the position is load-bearing
/// rather than tidy: reaching this function at all means the store has already
/// been opened — created and migrated, if the machine had none — so a gate here
/// would refuse a pipe *after* writing a database the caller never asked for.
/// That is what shipped, and what D55 and `help.rs` both claimed did not happen.
/// This function may therefore be called only when `tui::is_interactive` has
/// already said yes; `pick_refuses_a_piped_stdout_with_a_nonzero_exit` asserts
/// the ordering by pointing `$TASQX_DB` at a path that must still not exist
/// afterwards.
fn run_pick(be: &mut Backend, ctx: &Ctx, filter: &[String]) -> CmdOutcome {
    // The same default and the same argv-preserving parse as `list`: `pick` is
    // a chooser over the working set, so the set it offers must be the set
    // `tasqx` shows, token for token.
    let filter_str = if filter.is_empty() {
        "@working".to_string()
    } else {
        tasqx_core::filter::from_argv(filter)
    };
    let listed = be.call(
        "task.list",
        &json!({ "filter": filter_str, "sort": ["-urgency"] }),
    )?;
    let rows = pick_rows(&listed);
    if rows.is_empty() {
        return Err(no_candidates(&filter_str));
    }

    let mut app = tui::pick::App::new(rows);
    let caps = ctx.caps;
    let chosen = tui::with_terminal(|term| pick_loop(term, &mut app, &ctx.theme, caps))
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;

    // Cancelling is exit 4, not exit 0. `pick` exists to produce one task; when
    // it produced none, answering ok is a command reporting success for work it
    // did not do — this project's named recurring defect, and the reason
    // `config edit`'s "no changes" exit 0 is NOT the precedent to copy. That
    // screen is a session where zero edits is a legitimate outcome; this one is
    // a selection whose entire output is the choice.
    let Some(short_id) = chosen else {
        return Err(ApiError::not_found(
            "nothing picked — no task was started",
            None,
        ));
    };
    // The title is read back out of the snapshot the screen was built from,
    // because the screen is gone by the time this prints and `task.start`
    // answers with a UUID and a timestamp, neither of which a human recognises.
    let title = app
        .rows()
        .iter()
        .find(|r| r.short_id == short_id)
        .map(|r| r.title.clone())
        .unwrap_or_default();

    let result = be.call(
        "task.start",
        &json!({ "ref": short_id.to_string(), "keep": false }),
    )?;
    let text = picked_summary(ctx, short_id, &title, &result);
    Ok((pick_result(short_id, &title, result), text))
}

/// An empty candidate set is a refusal, not an empty screen.
///
/// Exit 4 with the filter quoted back, because the two ways to get here look
/// identical from the outside — a store with nothing pending, and a filter that
/// excludes everything — and only the text can tell them apart. Opening the
/// screen on zero rows instead would put the user in an alt screen whose only
/// available action is leaving it.
fn no_candidates(filter: &str) -> ApiError {
    ApiError::not_found(
        format!("no task matches `{filter}` — nothing to pick (try `tasqx list {filter}`)"),
        None,
    )
}

/// The `task.list` answer, flattened into the rows the screen draws.
///
/// Extracted so it is reachable from a test at all: everything around it needs
/// a real terminal, and a mapping that dropped a field — or read `id` where it
/// meant `short_id`, which would make every Enter start the wrong task or none
/// at all — would leave the whole suite green with the screen unusable. That is
/// the same hole `settings_rows` was pulled out of `run_config_edit` to close.
fn pick_rows(result: &Value) -> Vec<tui::pick::Row> {
    let field = |t: &Value, key: &str| -> String {
        t.get(key).and_then(Value::as_str).unwrap_or("").to_string()
    };
    result
        .get("tasks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|t| {
            let urgency = t.get("urgency").and_then(Value::as_f64).unwrap_or(0.0);
            tui::pick::Row::new(
                t.get("short_id").and_then(Value::as_i64).unwrap_or(0),
                &field(t, "title"),
                &field(t, "project"),
                // `-` and not an empty cell: a task with no priority is a fact,
                // and a blank column reads as a rendering bug.
                match field(t, "priority").as_str() {
                    "" => "-",
                    p => p,
                },
                &format!("{urgency:.1}"),
                &t.get("tags")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Draw, read one key, fold it in. Returns the chosen `short_id`, or `None`
/// when the user left without choosing.
///
/// The theme is resolved once, outside, and not per frame: unlike the settings
/// screen there is nothing here whose value depends on repainting in a
/// different theme, so re-loading it every keystroke would be work with no
/// observable effect.
fn pick_loop(
    term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::pick::App,
    theme: &theme::Theme,
    caps: Caps,
) -> std::io::Result<Option<i64>> {
    use ratatui::crossterm::event::{self, Event};

    loop {
        term.draw(|f| tui::pick::render(app, theme, &caps, f))?;
        // Resize and paste events just redraw; only keys are decisions.
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match app.on_key(key) {
            Some(tui::pick::Action::Choose { short_id }) => return Ok(Some(short_id)),
            Some(tui::pick::Action::Cancel) => return Ok(None),
            None => {}
        }
    }
}

/// The scrollback record `pick` leaves behind once the alt screen is gone.
///
/// `render::started` alone is not enough here, and that is not a style
/// preference: it prints "Started task · timer running (since …)" and names no
/// task, which is right for `tasqx start 42` — the user typed the ref — and
/// wrong for a screen that has just been wiped off the display. Without the
/// identity line an interactive session leaves no trace of WHICH task it
/// started, which is exactly the auditability `saved_summary` exists to give
/// `config edit`.
///
/// Extracted for the same reason as that function: the rest of `run_pick` needs
/// a real terminal, so this line would otherwise be the one piece of it no test
/// could ever see.
fn picked_summary(ctx: &Ctx, short_id: i64, title: &str, result: &Value) -> String {
    format!(
        "{}\n{}",
        ctx.paint("header", &format!("#{short_id}  {title}")),
        render::started(ctx, result)
    )
}

/// The `--json` body: `task.start`'s own answer, plus the identity of the task
/// that was picked.
///
/// `task.start` returns `{id, interval_started}` — a UUID and a timestamp. That
/// is the right answer for a caller who supplied the ref, and a useless one
/// here, because the ref is the thing `pick` was asked to determine. The two
/// added fields are the CLI's own composition (as `agenda`'s `--json` body is),
/// never a change to what the method returns: the method's keys are passed
/// through untouched beside them.
fn pick_result(short_id: i64, title: &str, mut started: Value) -> Value {
    if let Some(obj) = started.as_object_mut() {
        obj.insert("short_id".to_string(), json!(short_id));
        obj.insert("title".to_string(), json!(title));
    }
    started
}

// ---- charts, HTML report, and theme tools (DESIGN.md §8) --------------------

/// `tasqx chart <kind>`: read the event log and render a native terminal chart.
/// `tasqx chart throughput|heatmap|burndown`.
///
/// Each arm computes its SERIES once and hands the same values to both the
/// renderer and the JSON. The series is the answer; the sparkline is one way of
/// looking at it, and a script that wants the numbers should not have to parse
/// block glyphs back into integers to get them.
fn run_chart(engine: &Engine, ctx: &Ctx, kind: ChartKind) -> CmdOutcome {
    let anchor = chart::today();
    Ok(match kind {
        ChartKind::Throughput { weeks } => {
            let weeks = chart::default_weeks(false, weeks);
            let events = events_since(engine, anchor, weeks * 7 + 7)?;
            let series = chart::throughput(&events, weeks, anchor);
            let data = series
                .iter()
                .map(|b| {
                    json!({ "iso_year": b.iso_year, "iso_week": b.iso_week, "label": b.label(),
                            "added": b.added, "done": b.done, "net": b.net() })
                })
                .collect::<Vec<_>>();
            (
                json!({ "chart": "throughput", "weeks": weeks, "series": data }),
                chart::render_throughput(ctx, &series),
            )
        }
        ChartKind::Heatmap { year, weeks } => {
            let weeks = chart::default_weeks(year, weeks);
            let events = events_since(engine, anchor, weeks * 7 + 7)?;
            let days = chart::heatmap(&events, weeks, anchor);
            let data = days
                .iter()
                .map(|d| json!({ "date": d.date.to_string(), "count": d.count }))
                .collect::<Vec<_>>();
            (
                json!({ "chart": "heatmap", "weeks": weeks, "series": data,
                        "current_streak": chart::current_streak(&days, anchor),
                        "best_streak": chart::best_streak(&days) }),
                chart::render_heatmap(ctx, &days, anchor),
            )
        }
        ChartKind::Burndown { project, days } => {
            let days_n = days.unwrap_or(30);
            // Reported, never swallowed: an unresolvable scope used to render as
            // a cleared burndown, which is a wrong answer wearing the costume of
            // a right one.
            let (members, label) = burndown_members(engine, &project)?;
            let events = events_since(engine, anchor, days_n + 1)?;
            let series = chart::burndown(&events, &members, days_n, anchor);
            let data = series
                .iter()
                .map(|p| json!({ "date": p.date.to_string(), "remaining": p.remaining }))
                .collect::<Vec<_>>();
            (
                json!({ "chart": "burndown", "days": days_n, "scope": label, "series": data }),
                chart::render_burndown(ctx, &series, &label),
            )
        }
    })
}

const DASHBOARD_NEEDS_A_TERMINAL: &str =
    "`tasqx dashboard` needs an interactive terminal on stdin and stdout \
     (one of them is piped, redirected, or TERM=dumb). `tasqx --json dashboard` \
     gives the same panels as data, and `tasqx list` prints the working set.";

/// Why this terminal cannot show the dashboard, or `None` when it can.
///
/// The policy, with the two facts it depends on injected — the split
/// `is_interactive_with` exists for, and for the same reason: under cargo the
/// process has a piped stdout, so a predicate that asked the process directly
/// would be untestable at exactly the point it can go wrong.
///
/// `size` is `None` when it could not be measured, which is a refusal rather
/// than a default: entering the alternate screen on a guess is how you paint a
/// half-drawn frame onto a window nobody can read.
fn dashboard_refusal(
    caps: &Caps,
    stdout_tty: bool,
    stdin_tty: bool,
    size: Option<(u16, u16)>,
) -> Option<String> {
    use tui::dashboard::model::{MIN_HEIGHT, MIN_WIDTH};
    if !tui::is_interactive_with(caps, stdout_tty, stdin_tty) {
        return Some(DASHBOARD_NEEDS_A_TERMINAL.to_string());
    }
    match size {
        Some((w, h)) if w >= MIN_WIDTH && h >= MIN_HEIGHT => None,
        Some((w, h)) => Some(format!(
            "this terminal is {w}x{h}; `tasqx dashboard` needs at least \
             {MIN_WIDTH}x{MIN_HEIGHT}. Resize the window, or run `tasqx list`."
        )),
        None => Some(
            "`tasqx dashboard` could not measure this terminal, so it will not enter \
             the alternate screen. Run `tasqx list` instead."
                .to_string(),
        ),
    }
}

/// The terminal's size, asked only once there is a terminal to ask about.
///
/// `crossterm::terminal::size()` consults `/dev/tty` and can fall back to
/// spawning `tput`, so in a pipe it answers about a window the bytes are not
/// going to. Asking it there would be worse than not asking.
fn terminal_size(caps: &Caps, stdout_tty: bool, stdin_tty: bool) -> Option<(u16, u16)> {
    if tui::is_interactive_with(caps, stdout_tty, stdin_tty) {
        ratatui::crossterm::terminal::size().ok()
    } else {
        None
    }
}

/// Whether a bare `tasqx` opens the dashboard rather than printing the table.
///
/// Four signals: a human is at the keyboard, no machine is reading the output,
/// the user has not switched it off, and the window is big enough to draw on.
/// Pure, and separate from everything that touches a terminal or a store, so
/// the condition is testable without either.
///
/// `fits` is the one that was missing when the screen first shipped, and its
/// absence was a real bug rather than a rough edge: bare `tasqx` in a 40x10
/// window entered the alternate screen, painted NOTHING — `layout` returns
/// `None` below 56x14 and `render` returns early — blocked until `q`, and
/// created a 208 KB store on the way in. That is D55's refused-screen-leaves-a-
/// store failure, rebuilt one screen over. Below the minimum a bare invocation
/// falls through to `run_list`: whoever typed nothing did not ask for a
/// dashboard, so silence is the right answer and the table is still useful.
///
/// `is_interactive` and nothing else answers the first: it asks about **stdout
/// and stdin both**, because the alternate screen is written to one and the key
/// loop blocks on the other. `Caps::detect() != PLAIN` is NOT the same question
/// — `CLICOLOR_FORCE=1` says "colour even when piped", and conflating the two is
/// what made `config edit | cat` hang forever (D26).
fn dashboard_active(
    caps: &Caps,
    json: bool,
    enabled: bool,
    fits: bool,
    stdout_tty: bool,
    stdin_tty: bool,
) -> bool {
    enabled && !json && fits && tui::is_interactive_with(caps, stdout_tty, stdin_tty)
}

/// Read `dashboard.enabled` — the escape hatch a breaking change owes its users.
///
/// Env before file so a CI image can switch it off in one line, and the default
/// is on. A malformed value reads as "on" for the same reason D57's hint does:
/// the failure of a setting that governs one screen must be the screen, not
/// silence.
///
/// The doc above said "env before file" while the code read the environment and
/// stopped. `dashboard.enabled` is a registered `Home::Toml` setting: `config
/// edit` draws it, `config list` prints it, `config set` writes it to
/// `config.toml` and reports success — and none of that did anything. A setting
/// that acknowledges a write and then ignores it is worse than one that does not
/// exist. It now goes through `config::resolve` like its three siblings, which
/// is where the precedence lives.
fn dashboard_enabled() -> bool {
    let s = config::find("dashboard.enabled").expect("dashboard.enabled is registered");
    dashboard_enabled_with(config::toml_value(s).as_deref())
}

/// [`dashboard_enabled`] over an explicit file value.
///
/// Split for the reason `config::toml_value_in` is: the file half must be
/// testable without mutating process-global env, which cargo's parallel test
/// threads make racy.
fn dashboard_enabled_with(file: Option<&str>) -> bool {
    let s = config::find("dashboard.enabled").expect("dashboard.enabled is registered");
    let (v, _) = config::resolve(s, None, file);
    // The env layer arrives verbatim — `resolve` does not coerce it — so the
    // three spellings a shell user reaches for are matched here rather than
    // leaving `TASQX_DASHBOARD=0` reading as "on". The file layer is already
    // normalised to `true`/`false` by `coerce`.
    !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no")
}

/// Read `dashboard.panels` — which panels the screen draws, in which order.
///
/// Total rather than fallible, and that is `coerce`'s doing: a value outside the
/// vocabulary is not a value, so it never reaches here and the resolver hands
/// back the default. What arrives is either the user's list or the built-in one.
fn dashboard_panels() -> Vec<tui::dashboard::model::PanelId> {
    use tui::dashboard::model::PanelId;
    let s = config::find("dashboard.panels").expect("dashboard.panels is registered");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v.split(',')
        .filter_map(|name| PanelId::from_slug(name.trim()))
        .collect()
}

/// Read `dashboard.window` as a day count.
///
/// Converted through `WINDOW_CHOICES` BY NAME, never by inventing a number:
/// `App::new` maps a day count back to an index with `unwrap_or(0)`, so a count
/// that is not in the list would silently become "week" and the `w` key would
/// start from somewhere the config never asked for.
fn dashboard_window_days() -> usize {
    let s = config::find("dashboard.window").expect("dashboard.window is registered");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    tui::dashboard::WINDOW_CHOICES
        .iter()
        .find(|(name, _)| *name == v)
        .map(|(_, days)| *days)
        .unwrap_or(7)
}

/// Read `dashboard.refresh` — whether the screen re-reads on a timer.
fn dashboard_auto_refresh() -> bool {
    let s = config::find("dashboard.refresh").expect("dashboard.refresh is registered");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    v != "manual"
}

/// The four reads one dashboard refresh makes.
///
/// One `task.list {}` — UNFILTERED, because the burndown reconstructs backwards
/// from current state and needs every task's status — feeding five panels, and
/// one `report.summary` feeding two. `Engine::task_list` loads every row and
/// filters in Rust, so five limited calls would be five full scans; and two
/// summaries would double the heaviest read in the set.
fn dashboard_data(
    be: &mut Backend,
    days: usize,
    now: jiff::Timestamp,
    today: jiff::civil::Date,
) -> Result<tui::dashboard::model::Dashboard, ApiError> {
    use jiff::ToSpan;
    const EVENT_LIMIT: usize = 100_000;

    let tasks = be.call("task.list", &json!({}))?;
    let summary = be.call(
        "report.summary",
        &json!({
            "group_by": "project",
            "metrics": [
                "est_total", "tracked_total",
                "tokens_cache_read", "tokens_cache_creation", "tokens_in", "tokens_out"
            ]
        }),
    )?;
    // Archived projects still hold tasks and still get a summary group, so
    // hiding them here would leave that group unjoinable.
    let projects = be.call("project.list", &json!({ "include_archived": true }))?;
    let from = today.saturating_sub((days as i64 + 1).days());
    let events = be.call(
        "event.list",
        &json!({ "limit": EVENT_LIMIT, "from": format!("{from}T00:00:00Z") }),
    )?;

    Ok(tui::dashboard::model::build(
        tui::dashboard::model::Sources {
            tasks: &tasks,
            summary: &summary,
            projects: &projects,
            events: &events,
            event_limit: EVENT_LIMIT,
            days,
        },
        now,
        today,
    ))
}

/// How long the dashboard waits for a key before re-reading, with auto-refresh
/// on.
///
/// Five seconds rather than a configurable interval: every refresh is four
/// reads, one of which (`report.summary`) aggregates token measurements across
/// the store, so a knob here is an invitation to set a number that makes the
/// screen slow and blame the screen. `r` is always available, and `R` turns the
/// tick off entirely.
const REFRESH_TICK: u64 = 5;

/// Open the dashboard and stay in it until the user leaves.
///
/// Returns whatever should be printed into the scrollback afterwards — nothing,
/// normally, because a viewer writes nothing and there is nothing to audit.
fn run_dashboard(be: &mut Backend, ctx: &Ctx) -> Result<Option<String>, ApiError> {
    use ratatui::crossterm::event::{self, Event};
    use tui::dashboard::{Action, App};

    let today = chart::today();
    let now = jiff::Timestamp::now();
    let days = dashboard_window_days();
    let mut app = App::new(
        dashboard_data(be, days, now, today)?,
        dashboard_panels(),
        days,
        dashboard_auto_refresh(),
    );

    // OUTER loop: the screen comes back after the picker.
    //
    // `p` used to hand the terminal back and let `run_pick`'s result become the
    // command's result — so backing out of the picker exited the whole process
    // with `not_found` and code 4, and choosing a task started it and then quit.
    // Neither is what a key on a read-only overview should do. `pick` opens its
    // own `with_terminal`, so the dashboard's has to be closed first and
    // reopened after; D58 asks for one session with a `Screen` enum, which is
    // the remaining half of this work and would remove the flicker.
    let mut picked: Option<String> = None;
    loop {
        // What the user asked for on the way out, if anything. `Refresh` never
        // reaches here — it is served inside the loop, because a refresh that
        // tore the screen down and rebuilt it would flicker on every `r` and on
        // every tick of auto-refresh.
        let mut want: Option<Action> = None;
        let mut failed: Option<ApiError> = None;
        tui::with_terminal(|term| {
            loop {
                let mut placed = Vec::new();
                let mut has_slot = false;
                term.draw(|f| {
                    tui::dashboard::render(&app, &ctx.theme, &ctx.caps, f);
                    if let Some(s) = app.screen(f.area().width, f.area().height) {
                        placed = s.panels.iter().map(|p| p.id).collect();
                        has_slot = s.has_slot();
                    }
                })?;
                // The state machine is told what was drawn, as data — that is what
                // keeps Tab from stopping on a panel this size cannot show.
                app.observe(&placed, has_slot);

                // With auto-refresh on, wait at most one tick for a key and then
                // re-read; otherwise block until the user does something. Polling
                // rather than a background thread keeps this loop the only thing
                // that touches the terminal, which is the invariant that lets the
                // existing panic hook and `Restore` guard stay sufficient.
                if app.auto_refresh() && !event::poll(std::time::Duration::from_secs(REFRESH_TICK))?
                {
                    match dashboard_data(be, app.window_days(), jiff::Timestamp::now(), today) {
                        Ok(data) => app.replace(data),
                        Err(e) => {
                            failed = Some(e);
                            return Ok(());
                        }
                    }
                    continue;
                }
                // Resize and paste just redraw; only keys are decisions.
                let Event::Key(key) = event::read()? else {
                    continue;
                };
                match app.on_key(key) {
                    Some(Action::Quit) => return Ok(()),
                    Some(Action::Refresh) => {
                        // A read that fails mid-session ends the session rather
                        // than redrawing stale numbers under a live-looking screen.
                        // The error is carried out and reported on the normal
                        // terminal, because a message printed here is wiped by the
                        // restore that follows it.
                        match dashboard_data(be, app.window_days(), jiff::Timestamp::now(), today) {
                            Ok(data) => app.replace(data),
                            Err(e) => {
                                failed = Some(e);
                                return Ok(());
                            }
                        }
                    }
                    Some(Action::Detail(id)) => {
                        // Served HERE, beside `Refresh`, and not carried out
                        // through `want`: everything that reaches the outer
                        // loop leaves this screen, so a missing arm would make
                        // `⏎` quit the dashboard.
                        match be.call("task.get", &json!({ "ref": id })) {
                            Ok(v) => match tui::dashboard::model::TaskDetail::from_json(&v) {
                                Some(card) => app.show_detail(card),
                                None => app.say(format!(
                                    "#{id} came back in a shape this build cannot read"
                                )),
                            },
                            // A row can be gone by the time the key arrives —
                            // auto-refresh redraws every five seconds and
                            // another terminal can finish the task in between.
                            // Expected, so it is a line in the footer; anything
                            // else ends the session as `Refresh` does.
                            Err(e) if e.code == tasqx_core::ErrorCode::NotFound => {
                                app.say(format!("#{id} is gone — r to refresh"));
                            }
                            Err(e) => {
                                failed = Some(e);
                                return Ok(());
                            }
                        }
                    }
                    Some(a) => {
                        want = Some(a);
                        return Ok(());
                    }
                    None => {}
                }
            }
        })
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;

        if let Some(e) = failed {
            return Err(e);
        }
        match want {
            // `l` is the one key that means "leave", so it does.
            Some(Action::List) => return run_list(be, ctx, &[]).map(|(_, r)| Some(r)),
            Some(Action::Pick) => {
                match run_pick(be, ctx, &[]) {
                    Ok((_, render)) => picked = Some(render),
                    // Backing out of the picker is not an error HERE. `pick` as
                    // a command exits 4 having started nothing, because its
                    // whole output is the choice (D55); reached from a screen
                    // the user is going back to, cancelling is just cancelling.
                    Err(e) if e.code == tasqx_core::ErrorCode::NotFound => {}
                    Err(e) => return Err(e),
                }
                // Whatever happened, the screen shows it: a started task turns
                // up in NOW, and a cancelled pick redraws unchanged.
                app.replace(dashboard_data(
                    be,
                    app.window_days(),
                    jiff::Timestamp::now(),
                    chart::today(),
                )?);
            }
            // Quit, or nothing left to do.
            _ => return Ok(picked),
        }
    }
}

/// `tasqx --json dashboard` — the panels as data, with no screen involved.
///
/// This is why the verb is not a `JSON_CARVE_OUTS` entry (D58): the whole data
/// layer — every mapper, every join, the burndown reconstruction — becomes
/// reachable from a script and from a test that has no terminal, which is
/// otherwise only testable through a pty.
///
/// The human rendering is the document too. A `--json` carve-out would have
/// been dishonest, and a prose summary here would be a second surface to keep
/// true; anyone who wants prose has the screen.
fn run_dashboard_json(be: &mut Backend, _ctx: &Ctx) -> CmdOutcome {
    let days = dashboard_window_days();
    let order = dashboard_panels();
    let data = dashboard_data(be, days, jiff::Timestamp::now(), chart::today())?;
    let doc = tui::dashboard::json::document(&data, days, &order);
    let render = serde_json::to_string_pretty(&doc).unwrap_or_default();
    Ok((doc, render))
}

/// The event log from `days_back` days before `anchor`, for a chart that only
/// draws that far (D59).
///
/// Before `event.list` took a bound, every chart read `{limit: 100000}` — a full
/// scan of a log that is append-only and never pruned, on a table that grows
/// with every mutation the store has ever recorded. The bound is per-arm and not
/// hoisted: throughput, heatmap and burndown draw three different windows, and
/// one shared `from` would silently be the narrowest of them.
///
/// `days_back` is passed generously by callers (a week of slack on the
/// week-bucketed charts). The bound is an optimisation and not semantics —
/// which became TRUE only with D60: under the forwards reconstruction a
/// narrower window really did change the answer, and this comment claimed
/// otherwise for as long as that was the case. Backwards, existence comes from
/// `created` and today's state from the snapshot, so a clipped window costs
/// only the days whose changes it hid.
///
/// `limit` stays as the belt to this parameter's braces: `ORDER BY id DESC
/// LIMIT n` drops the OLDEST rows if it ever binds, which is the direction the
/// burndown can absorb.
fn events_since(
    engine: &Engine,
    anchor: jiff::civil::Date,
    days_back: usize,
) -> Result<Value, ApiError> {
    use jiff::ToSpan;
    let from = anchor.saturating_sub((days_back as i64).days());
    dispatch(
        engine,
        "event.list",
        &json!({ "limit": 100000, "from": format!("{from}T00:00:00Z") }),
    )
}

/// Resolve the task ids a burndown covers, plus its label. Split out of the
/// `ChartKind::Burndown` arm so the scope rule is testable on its own.
///
/// Both branches go through `task.list` with no status filter (D60). The `None`
/// branch previously used an unfiltered `store.export`, which is what let
/// cancelled tasks inflate the whole-store burndown's "remaining work" line.
fn burndown_members(
    engine: &Engine,
    project: &Option<String>,
) -> Result<(Vec<chart::Member>, String), ApiError> {
    let (filter, label) = match project {
        // Through `filter::quote`, never interpolated: a project may be named
        // `Home Renovation` or `a (b)`, and a raw `{p}` composes a filter that
        // asks a different question (or none at all) without saying so.
        Some(p) => (
            Some(format!("project:{}", tasqx_core::filter::quote(p))),
            p.clone(),
        ),
        None => (None, "all tasks".to_string()),
    };
    // No `NOT_CANCELLED` any more. It existed because the old reconstruction
    // guessed "open" for a task whose closing event it could not see, so a
    // cancelled task hung open on the chart forever and had to be excluded
    // wholesale. Reconstructing backwards from the snapshot status closes it on
    // its cancel date instead — and keeping the filter would now DELETE the task
    // from the days it was genuinely open, which is a different wrong answer.
    let mut params = json!({ "fields": ["id", "status", "created"] });
    if let Some(f) = filter {
        params["filter"] = Value::String(f);
    }
    let listed = dispatch(engine, "task.list", &params)?;
    Ok((chart::members_of(&listed), label))
}

/// `tasqx report --html`: write the self-contained HTML review.
///
/// The scope comes from [`report_params`] — the SAME builder the terminal path
/// uses — so the two output modes of one command cannot answer different
/// questions again. `all` is hard `false` rather than a parameter because clap
/// already rejects `--all` alongside `--html`; spelling it here keeps the two
/// facts in one place instead of accepting a flag we would then ignore.
fn run_html_report(
    engine: &Engine,
    ctx: &Ctx,
    args: Vec<String>,
    out: Option<String>,
) -> CmdOutcome {
    let params = report_params(&args, false);
    let doc = html::generate(engine, &ctx.theme, &params)?;
    match out {
        Some(path) => {
            if let Some(parent) = PathBuf::from(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, &doc) {
                // The machine-relevant fact of this mode is where the file landed
                // — the one thing a script needs in order to do anything next.
                Ok(()) => Ok((
                    json!({ "path": path, "bytes": doc.len() }),
                    format!("Wrote self-contained HTML report → {path}\n"),
                )),
                Err(e) => Err(ApiError::internal(format!("cannot write {path}: {e}"))),
            }
        }
        None => Ok((
            json!({ "path": Value::Null, "bytes": doc.len(), "html": doc.clone() }),
            doc,
        )),
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
fn run_docs(out: Option<&str>, no_open: bool, to_stdout: bool) -> CmdOutcome {
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
fn docs_default_path() -> Option<PathBuf> {
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
fn browser_candidates(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
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

/// `tasqx theme list|show|set`.
///
/// Every arm returns the facts alongside the rendering rather than printing:
/// the theme list, the resolved role→colour map, and the write receipt are all
/// things a script has a real use for — picking a theme from a menu, driving a
/// terminal's own palette from tasqx's, confirming where the value landed.
fn run_theme(ctx: &Ctx, action: &ThemeAction) -> CmdOutcome {
    match action {
        ThemeAction::List => {
            let mut text = String::new();
            text.push_str(&format!("{}\n", ctx.paint("header", "Built-in themes")));
            for name in theme::BUILTINS {
                let marker = if name == ctx.theme.name {
                    " ← active"
                } else {
                    ""
                };
                text.push_str(&format!("  {}{}\n", name, ctx.paint("muted", marker)));
            }
            let mut user_block = Value::Null;
            if let Some(dir) = themes_dir() {
                let user = user_theme_names();
                if !user.is_empty() {
                    text.push_str(&format!("{}\n", ctx.paint("header", "User themes")));
                    text.push_str(&format!(
                        "  {}\n",
                        ctx.paint("muted", &dir.to_string_lossy())
                    ));
                    for name in &user {
                        text.push_str(&format!("  {name}\n"));
                    }
                    user_block = json!({ "dir": dir.to_string_lossy(), "names": user });
                }
            }
            Ok((
                json!({ "active": ctx.theme.name, "builtin": theme::BUILTINS, "user": user_block }),
                text,
            ))
        }
        ThemeAction::Show { name } => {
            // Preview the requested theme (or the active one) at current caps.
            let preview = match name {
                Some(n) => {
                    let resolved = theme::resolve_name(None, None, Some(n));
                    // `theme::load` falls back to the default for a name it does
                    // not know. That is right on the render path — a bad theme
                    // must never fail a task capture — and wrong here, where
                    // showing the user a theme they did not ask for is the whole
                    // failure: a typo'd `gruvbux` printed nord and exited 0, and
                    // nothing distinguished it from a theme that looks like nord.
                    // Validated AFTER resolving so a blank argument still means
                    // "the default", and through the shared validator so this
                    // cannot drift from `theme set` and `config set` the way an
                    // inline copy already did once.
                    validate_setting("theme.name", &resolved)?;
                    Ctx::new(theme::load(&resolved, themes_dir().as_deref()), ctx.caps)
                        .with_cols(ctx.cols)
                }
                None => Ctx::new(ctx.theme.clone(), ctx.caps).with_cols(ctx.cols),
            };
            // Block glyphs are Unicode; degrade the swatch to ASCII on the plain/
            // legacy path so `theme show | cat` never emits mojibake.
            let swatch = if preview.caps.unicode {
                "████"
            } else {
                "####"
            };
            let bar = if preview.caps.unicode { "█" } else { "#" };
            let mut text = String::new();
            text.push_str(&format!(
                "{}\n",
                preview.paint("header", &format!("Theme: {}", preview.theme.name))
            ));
            // The resolved role→colour map, built from the SAME `role_names` walk
            // that prints the swatches, so the two views of one theme cannot come
            // to differ about which roles it defines.
            let mut roles = serde_json::Map::new();
            for role in preview.theme.role_names() {
                let sample =
                    preview
                        .theme
                        .paint(&role, &format!("{swatch} sample text"), &preview.caps);
                text.push_str(&format!("  {:<14} {sample}\n", role));
                let st = preview.theme.role(&role);
                roles.insert(
                    role.clone(),
                    json!({
                        "fg": st.fg.map(|c| c.hex()),
                        "bold": st.bold, "dim": st.dim, "underline": st.underline,
                    }),
                );
            }
            // Show the urgency ramp as a cold→hot strip.
            let strip: String = (0..=10)
                .map(|i| {
                    let t = i as f64 / 10.0;
                    preview.theme.ramp_style(t).paint(bar, &preview.caps)
                })
                .collect();
            text.push_str(&format!(
                "  {:<14} {strip}  {}\n",
                "urgency.ramp",
                preview.paint("muted", "cold → hot")
            ));
            Ok((
                json!({
                    "name": preview.theme.name,
                    "roles": Value::Object(roles),
                    "ramp": preview.theme.ramp().iter().map(|c| c.hex()).collect::<Vec<_>>(),
                }),
                text,
            ))
        }
        // Delegated, not reimplemented. `theme set X` and `config set theme.name X`
        // are two spellings of ONE write, and spelling them twice is exactly how
        // they came to disagree: validation lived in one and not the other, and
        // then `--json` landed on one and not the other. One function, one shape,
        // by construction rather than by two developers remembering.
        ThemeAction::Set { name } => set_setting("theme.name", name),
    }
}

/// Persist one setting and describe the write. The single implementation behind
/// `tasqx config set <key> <value>` and `tasqx theme set <name>`.
fn set_setting(key: &str, value: &str) -> CmdOutcome {
    let s = config::find(key).ok_or_else(|| unknown_key(key))?;
    validate_setting(s.key, value)?;
    let path = config::write_value(s, value)?;
    let mut text = format!("{} = {}  ({})\n", s.key, value, path.display());
    if let Some(p) = theme_pointer(s.key) {
        text.push_str(&format!("{p}\n"));
    }
    Ok((
        json!({ "key": s.key, "value": value, "path": path.to_string_lossy() }),
        text,
    ))
}

/// The live value of a `Home::Store` setting. Read from `core.capabilities`,
/// which already reports `default_project`, so this needs no new API method.
///
/// `Result<Option<_>>` and not `Option<_>`: the two answers "this setting is not
/// set" and "we could not ask the store" are different facts and the caller
/// must be able to tell them apart. The first version was `.ok()?`, which
/// flattened a failed `core.capabilities` call — a dead daemon mid-request, say
/// — into `None`, which every caller then rendered as the empty string. So
/// `config get default_project` answered a transport failure with a blank line
/// and exit 0, and a script reading that value could not tell it from an unset
/// one. A failure is not a value.
fn store_value(be: &mut Backend, key: &str) -> Result<Option<String>, ApiError> {
    let caps = be.call("core.capabilities", &json!({}))?;
    Ok(caps.get(key).and_then(Value::as_str).map(str::to_string))
}

/// How the settings layer reads a `Home::Store` value, as a closure.
///
/// A seam, and a deliberate one: `Backend::Local` cannot fail this call, so
/// without it the error path below is unreachable from a test and the very bug
/// it exists to prevent could be reintroduced with the suite staying green.
type StoreLookup<'a> = &'a mut dyn FnMut(&str) -> Result<Option<String>, ApiError>;

/// One setting's resolved value and the label naming where it came from.
///
/// The ONE answer for all three readers — `config get`, `config list` and the
/// `config edit` snapshot. It was spelled out three times, and the three copies
/// are exactly how a `Home::Store` setting can go missing from one surface
/// while the other two keep reporting it (D30: derive it, do not keep three
/// lists in sync).
fn setting_value(
    store: StoreLookup,
    s: &config::Setting,
    flag: Option<&str>,
) -> Result<(String, String), ApiError> {
    match s.home {
        config::Home::Store => Ok((store(s.key)?.unwrap_or_default(), "store".to_string())),
        config::Home::Toml => {
            // The EFFECTIVE value, never the one a layer asked for and the tool
            // discarded. The warning is dropped here rather than printed:
            // `build_ctx` has already resolved the same setting from the same
            // layers this run and said it once.
            let (v, src, _) = effective_setting(s, flag, file_value(s)?.as_deref());
            Ok((v, src.label(s)))
        }
    }
}

/// Every registered setting, as the rows the interactive screen shows.
///
/// This is the code that decides which settings a user SEES, and it is extracted
/// so a test can run it. Dropping the `Home::Store` arm — so `default_project`
/// silently never appeared on screen — used to leave the whole suite green,
/// because the loop was inline in `run_config_edit`, which needs a real
/// terminal, and every TUI test built its rows by hand.
fn settings_rows(
    store: StoreLookup,
    themes: &[String],
    theme_flag: Option<&str>,
) -> Result<Vec<tui::settings::Row>, ApiError> {
    let mut rows = Vec::new();
    for s in config::SETTINGS {
        let flag = if s.key == "theme.name" {
            theme_flag
        } else {
            None
        };
        let (value, source) = setting_value(store, s, flag)?;
        rows.push(build_row(s, value, source, themes));
    }
    Ok(rows)
}

/// An unknown key must name the valid ones. Without the list the user's only
/// recourse is to guess, and the registry already knows the answer.
fn unknown_key(key: &str) -> ApiError {
    let valid: Vec<&str> = config::SETTINGS.iter().map(|s| s.key).collect();
    ApiError::bad_request(format!(
        "unknown setting {key:?} (valid: {})",
        valid.join(", ")
    ))
}

/// Read one setting from `config.toml` strictly, reporting a wrong-typed value
/// on stderr before returning the fallback.
///
/// A warning and not an error, on purpose. A malformed file is a parse error
/// because nothing in it can be trusted; a wrong-typed value is one bad line in
/// a file whose other keys still work, and failing the command would break
/// `config list` — the command you run to find exactly this — over that one
/// line. stderr keeps stdout scriptable, so `$(tasqx config get theme.name)`
/// still yields a usable value while the human sees what the file did.
///
/// Every `tasqx config` read goes through here rather than calling
/// `toml_value_strict` directly, so a new read site cannot quietly re-acquire
/// the silence this replaced.
fn file_value(s: &config::Setting) -> Result<Option<String>, ApiError> {
    let read = config::toml_value_strict(s)?;
    if let Some(m) = &read.mismatch {
        eprintln!("warning: {m}");
    }
    Ok(read.value)
}

/// The one-line pointer to the command that makes a theme change visible.
///
/// tasqx's normal output carries only a few coloured accents, so a user who
/// switches themes sees almost nothing change in `tasqx list` and reasonably
/// concludes the write did not take — which is exactly what happened: gruvbox
/// was saved correctly from `config edit` and the user came back asking where
/// they were supposed to notice. `tasqx theme show` prints every role with a
/// swatch and is the only place the choice is obvious.
///
/// One function, three callers (`theme set`, `config set`, `config edit`),
/// because a pointer added to one write path and not the others is precisely
/// how those three drifted apart over validation once already. `None` for every
/// other key: `notify.enabled = true` has nothing to do with themes.
fn theme_pointer(key: &str) -> Option<&'static str> {
    (key == "theme.name").then_some("See it with `tasqx theme show`.")
}

/// Reject a value that would persist but never take effect.
///
/// Shared because the first version put theme validation inline in `theme set`
/// only — so `tasqx theme set bogus` was rejected while
/// `tasqx config set theme.name bogus` wrote it happily and exited 0, and
/// `theme show bogus` previewed the default as if nothing were wrong. The
/// primitive was looser than its own alias, which is backwards: `theme::load`
/// falls back to the default for an unknown name, so the write persists a value
/// that silently does nothing on every run from then on.
fn validate_setting(key: &str, value: &str) -> Result<(), ApiError> {
    if key == "theme.name" {
        let known = theme::BUILTINS.contains(&value)
            || themes_dir().is_some_and(|d| d.join(format!("{value}.toml")).is_file());
        if !known {
            return Err(ApiError::bad_request(format!(
                "unknown theme {value:?} (try `tasqx theme list`)"
            )));
        }
    }
    Ok(())
}

/// `flag` carries the CLI override for the setting being reported — today only
/// `--theme`. Without it `config` reports the file value while the binary
/// renders with the flag's, so the one command whose job is naming the layer
/// that won cannot see the layer that wins most.
fn run_config(
    be: &mut Backend,
    ctx: &Ctx,
    action: &ConfigAction,
    theme_flag: Option<&str>,
) -> CmdOutcome {
    // The flag layer applies per setting; only `theme.name` has one today.
    let flag_for = |s: &config::Setting| -> Option<&str> {
        if s.key == "theme.name" {
            theme_flag
        } else {
            None
        }
    };
    match action {
        ConfigAction::Edit => run_config_edit(be, ctx, theme_flag),
        ConfigAction::Path => {
            let p = config::config_path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "(no config directory on this platform)".to_string());
            Ok((json!({ "path": p }), format!("{p}\n")))
        }
        ConfigAction::Store => {
            // D74: through a daemon, ask it — `core.capabilities` carries the
            // store the answering engine writes to, which is the one fact D47
            // established a client cannot compute for itself. An older daemon
            // without the field degrades to the pre-D74 output, not an error.
            let daemon_store = if be.remote_socket().is_some() {
                be.call("core.capabilities", &json!({}))
                    .ok()
                    .and_then(|v| v.get("store").and_then(Value::as_str).map(str::to_string))
            } else {
                None
            };
            Ok(store_location(
                be.remote_socket(),
                daemon_store.as_deref(),
                db_path(),
            ))
        }
        ConfigAction::Get { key } => {
            let s = config::find(key).ok_or_else(|| unknown_key(key))?;
            let (value, _) = setting_value(&mut |k| store_value(be, k), s, flag_for(s))?;
            let text = format!("{value}\n");
            Ok((json!({ "key": s.key, "value": value }), text))
        }
        ConfigAction::Set { key, value } => set_setting(key, value),
        ConfigAction::Unset { key } => {
            let s = config::find(key).ok_or_else(|| unknown_key(key))?;
            let existed = config::clear_value(s)?;
            let text = if existed {
                format!("{} unset; now {} (default)\n", s.key, s.default)
            } else {
                format!("{} was not set\n", s.key)
            };
            Ok((json!({ "key": s.key, "removed": existed }), text))
        }
        ConfigAction::List => {
            let mut rows = Vec::new();
            for s in config::SETTINGS {
                let (value, source) = setting_value(&mut |k| store_value(be, k), s, flag_for(s))?;
                rows.push(json!({
                    "key": s.key,
                    "value": value,
                    "source": source,
                    "default": s.default,
                    "home": match s.home {
                        config::Home::Store => "store",
                        config::Home::Toml => "config.toml",
                    },
                    "summary": s.summary,
                }));
            }
            let text = render_config_table(ctx, &rows);
            Ok((json!({ "settings": rows }), text))
        }
    }
}

/// `tasqx config edit` — the interactive settings screen (D26).
///
/// The three pieces this function owns are the three the state machine must not:
/// the TTY gate, the row snapshot (which reads the store and `config.toml`), and
/// the writes. Everything between them is `tui::settings`.
fn run_config_edit(be: &mut Backend, ctx: &Ctx, theme_flag: Option<&str>) -> CmdOutcome {
    // Refuse before a single escape byte is written. Piped, redirected or dumb
    // stdout gets a message on stderr and exit 2, not an alt screen it cannot
    // clear and a command that looks hung.
    if !tui::is_interactive(&ctx.caps) {
        return Err(ApiError::bad_request(
            "`tasqx config edit` needs an interactive terminal (stdout is piped, redirected, \
             or TERM=dumb). Use `tasqx config list` and `tasqx config set <key> <value>`.",
        ));
    }

    // The picker's candidate list. The registry says a setting HAS a closed set
    // and where it comes from; resolving it to values is this layer's job,
    // because it is a filesystem question the state machine must stay free of.
    let mut themes: Vec<String> = theme::BUILTINS.iter().map(|t| t.to_string()).collect();
    for name in user_theme_names() {
        if !themes.contains(&name) {
            themes.push(name);
        }
    }

    let rows = settings_rows(&mut |k| store_value(be, k), &themes, theme_flag)?;

    let mut app = tui::settings::App::new(rows);
    let caps = ctx.caps;
    let saved = tui::with_terminal(|term| settings_loop(term, &mut app, caps, theme_flag))
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))?;

    // Printed after the alt screen is gone, so the user's scrollback keeps a
    // record of what the session changed — an interactive screen that leaves no
    // trace is impossible to audit afterwards.
    let text = saved_summary(&saved);
    let changed: Vec<Value> = saved
        .iter()
        .map(|(k, v)| json!({ "key": k, "value": v }))
        .collect();
    Ok((json!({ "changed": changed }), text))
}

/// The scrollback record `config edit` leaves behind after the alt screen is
/// gone — an interactive screen that leaves no trace is impossible to audit
/// afterwards.
///
/// Extracted from `run_config_edit` so the theme pointer on this path is
/// reachable from a test at all: the rest of that function needs a real
/// terminal, so the summary was the one piece of it no test could ever see.
fn saved_summary(saved: &[(String, String)]) -> String {
    if saved.is_empty() {
        return "no changes\n".to_string();
    }
    let mut out = String::new();
    for (k, v) in saved {
        out.push_str(&format!("{k} = {v}\n"));
        if let Some(p) = theme_pointer(k) {
            out.push_str(&format!("{p}\n"));
        }
    }
    out
}

/// One screen row for one registered setting.
///
/// Extracted from `run_config_edit`'s snapshot loop so the mapping is reachable
/// from a test. It was inline, and dropping `Home::Store` settings from that
/// loop — so `default_project` silently never reached the screen — left the
/// whole suite green: every TUI test built its rows by hand, so none of them
/// exercised the code that decides which settings a user actually sees.
fn build_row(
    s: &'static config::Setting,
    value: String,
    source: String,
    themes: &[String],
) -> tui::settings::Row {
    tui::settings::Row {
        setting: s,
        value,
        source,
        choices: match s.choices {
            config::Choices::Themes => themes.to_vec(),
            config::Choices::Free => Vec::new(),
            config::Choices::OneOf(values) => values.iter().map(|v| (*v).to_string()).collect(),
            // No candidates, so `begin_edit` falls to its existing "no inline
            // editor — use `tasqx config set`" branch, and the row is still
            // listed with its value and source.
            //
            // Deliberately NOT an ordered multi-select. That would be a fourth
            // interaction mode in a screen D26 kept to two, to save one
            // `tasqx config set dashboard.panels now,next,due` — and the panel
            // order is already discoverable on the dashboard itself, where the
            // numbers are drawn into the panel headings. The write path
            // validates the list either way, which is where a typo actually
            // needs catching.
            config::Choices::ManyOf(_) => Vec::new(),
        },
    }
}

/// The theme name the NEXT frame must be painted in.
///
/// Named and extracted because the live preview is the only reason this screen
/// exists, and nothing proved the loop re-derived it. Hoisting `theme::load`
/// out of the loop body — resolving once instead of per frame, which kills the
/// preview outright — left all 362 tests green: the render test passed a theme
/// in directly, so it proved `render` honours what it is given and nothing
/// about where that came from.
fn frame_theme_name(app: &tui::settings::App, theme_flag: Option<&str>) -> String {
    // A `--theme` flag outranks everything, including a preview: the user asked
    // for that theme for this invocation, and previewing another would be the
    // screen disagreeing with the terminal it is drawn in.
    if let Some(f) = theme_flag.map(str::trim).filter(|f| !f.is_empty()) {
        return f.to_string();
    }
    app.preview_theme()
        .unwrap_or(theme::DEFAULT_THEME)
        .to_string()
}

/// Apply one `Save` action: validate, write, re-resolve, record.
///
/// `write` is injected so a test can observe that the write actually happens.
/// Inline, this whole path could be replaced with a validate-only no-op — so
/// `config edit` changed nothing on disk — with the suite staying green at
/// 362/362. The state machine was thoroughly covered; the twelve lines that
/// turn its decision into a file were not covered at all.
fn apply_save(
    app: &mut tui::settings::App,
    key: &'static str,
    value: &str,
    theme_flag: Option<&str>,
    saved: &mut Vec<(String, String)>,
    mut write: impl FnMut(&'static config::Setting, &str) -> Result<(), ApiError>,
) {
    let s = config::find(key).expect("the screen only names registered settings");
    // The same validator `config set` uses. The picker can only offer valid
    // values today, but a validator applied on one write path and not the other
    // is how `theme set` and `config set` diverged once already.
    match validate_setting(key, value).and_then(|()| write(s, value)) {
        Ok(()) => {
            // Re-resolve rather than assume: a `$TASQX_THEME` or a `--theme`
            // flag still outranks the file we just wrote, and the screen has to
            // say so instead of reporting a change the user's next command will
            // not show.
            let flag = if s.key == "theme.name" {
                theme_flag
            } else {
                None
            };
            let (v, src) = config::resolve(s, flag, config::toml_value(s).as_deref());
            app.refresh(key, v, src.label(s));
            saved.retain(|(k, _)| k != key);
            saved.push((key.to_string(), value.to_string()));
        }
        Err(e) => app.report_error(e.message),
    }
}

/// Draw, read one key, fold it in, perform whatever the state machine asked for.
///
/// The theme is reloaded from `app.preview_theme()` on EVERY frame, which is
/// what makes the preview live: moving the picker changes what that returns, so
/// the next frame is painted in the candidate theme before anything is written.
fn settings_loop(
    term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::settings::App,
    caps: Caps,
    theme_flag: Option<&str>,
) -> std::io::Result<Vec<(String, String)>> {
    use ratatui::crossterm::event::{self, Event};

    let dir = themes_dir();
    let mut saved: Vec<(String, String)> = Vec::new();
    loop {
        let name = frame_theme_name(app, theme_flag);
        let active = theme::load(&name, dir.as_deref());
        term.draw(|f| tui::settings::render(app, &active, &caps, f))?;

        // Resize and paste events just redraw; only keys are decisions.
        let Event::Key(key) = event::read()? else {
            continue;
        };
        match app.on_key(key) {
            Some(tui::settings::Action::Quit) => return Ok(saved),
            Some(tui::settings::Action::Save { key, value }) => {
                apply_save(app, key, &value, theme_flag, &mut saved, |s, v| {
                    config::write_value(s, v).map(|_| ())
                });
            }
            None => {}
        }
    }
}

/// One row per setting: key, value, and which layer supplied it. The source
/// column is the point — the question behind a surprising setting is always
/// "which layer won", and a bare value cannot answer it.
fn render_config_table(ctx: &Ctx, rows: &[Value]) -> String {
    // Widths come from the rows about to be printed, floored at the layout this
    // table has always had (D51's rule, one table over). They were plain `18`
    // and `22`, and both are guesses about data the renderer is holding: the
    // registry grew a `daemon.idle_timeout` (19 cells), which overflowed the
    // key column and shoved SOURCE one cell right on that row alone — the
    // misalignment `the_config_table_stays_aligned_when_a_value_is_not_ascii`
    // exists to catch, arriving from the column that was never suspected.
    // Padded, never truncated: a key and a value are the data the reader came
    // for, and this table is where they read it.
    let cells =
        |v: &Value, field: &str| -> usize { render::width(v[field].as_str().unwrap_or("")) };
    let key_w = rows
        .iter()
        .map(|r| cells(r, "key"))
        .max()
        .unwrap_or(0)
        .max(18);
    let val_w = rows
        .iter()
        .map(|r| {
            let w = cells(r, "value");
            // The empty value renders as `(unset)`, which is what has to fit.
            if w == 0 {
                render::width("(unset)")
            } else {
                w
            }
        })
        .max()
        .unwrap_or(0)
        .max(22);
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        ctx.paint(
            "header",
            &format!(
                "{} {} {}",
                render::pad("SETTING", key_w),
                render::pad("VALUE", val_w),
                "SOURCE"
            )
        )
    ));
    for r in rows {
        let key = r["key"].as_str().unwrap_or("");
        let val = r["value"].as_str().unwrap_or("");
        let src = r["source"].as_str().unwrap_or("");
        let shown = if val.is_empty() { "(unset)" } else { val };
        // `render::pad` measures terminal CELLS, not chars, so a value carrying
        // CJK or an emoji — an editor path, a project name — no longer shoves
        // the SOURCE column sideways.
        out.push_str(&format!(
            "{} {} {}\n",
            render::pad(key, key_w),
            render::pad(shown, val_w),
            ctx.paint("muted", src)
        ));
    }
    out
}

/// `tasqx manual` — print a themed guide section (or the TOC). No store, no net.
fn run_manual(ctx: &Ctx, topic: Option<&str>) {
    match manual::render(ctx, topic) {
        Ok(page) => println!("{page}"),
        Err(msg) => {
            eprintln!("{msg}");
            exit(ErrorCode::BadRequest.exit_code()); // exit 2
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

    // #17: token attribution is opt-in (DESIGN §10). When enabled, the daemon
    // spawns a third background thread that parses AI tool transcripts to
    // attribute token usage to completed tasks. Off by default.
    let tokens_enabled = config_tokens_enabled();

    // #18: the local OTLP receiver is opt-in (DESIGN §10). When `[otlp] enabled`,
    // the daemon binds a std TcpListener on 127.0.0.1:<port> to ingest token
    // telemetry from AI tools; off by default => no listener thread.
    let otlp_port = config_otlp_enabled().then(config_otlp_port);

    // D5: a daemon may self-terminate once nothing needs it. Off unless
    // `[daemon] idle_timeout` says otherwise — see `DaemonOptions::idle_timeout`
    // for why the 15 minutes D5 names is the auto-spawn default and not this
    // one. Announced on its own line only when armed, so the banner an operator
    // who never configured it sees is byte-for-byte the one they saw before.
    let idle_timeout = config_daemon_idle_timeout();

    // D74: a retirement note this daemon may have left on a previous idle exit
    // is stale the moment a daemon serves this address again — clear it before
    // any client can read yesterday's transition as today's.
    let retired_marker = daemon_retired_marker(&socket);
    if let Some(marker) = &retired_marker {
        let _ = std::fs::remove_file(marker);
    }

    // The banner — listening, D74's store line, and the idle arming — is
    // printed by `serve_with_options` itself, after the bind succeeds. It used
    // to print here, ahead of the bind, so a second daemon on a held address
    // announced `listening` and then contradicted itself (#250).
    let options = daemon::DaemonOptions {
        notifier,
        tokens_enabled,
        otlp_port,
        idle_timeout,
        retired_marker,
    };
    match daemon::serve_with_options(engine, &socket, shutdown, options) {
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

    // `from_argv`, not `join(" ")` — see `run_list`. `watch` re-sends this
    // string on every event, so a mis-split here would be wrong forever.
    let filter_str = if filter.is_empty() {
        "@working".to_string()
    } else {
        tasqx_core::filter::from_argv(filter)
    };
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
                    // The repaint makes the count not load-bearing here — the
                    // whole working set is redrawn — but 372 silently absorbed
                    // events are still worth one line an operator can see
                    // (#249). stderr, so it survives the screen clear.
                    if let Some(d) = evt.pointer("/data/dropped").and_then(Value::as_i64) {
                        eprintln!(
                            "tasqx watch: {d} event(s) dropped while this view was not \
                             keeping up; the repaint is current"
                        );
                    }
                    if let Err(e) = watch_render(&mut conn, &filter_str, ctx, true) {
                        eprintln!("tasqx watch: {e}");
                        exit(1);
                    }
                } else {
                    let data = evt.get("data").cloned().unwrap_or(Value::Null);
                    println!("{}", watch_stream_line(&data));
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

/// One non-TTY `watch` line per push: `op=` plus whichever attribution fields
/// the frame carries.
///
/// `dropped` is the load-bearing one (#249): the daemon computes exactly how
/// many events a congested subscriber lost and puts the count *in* the gap
/// frame, because — `daemon.rs`'s own words — a silent drop leaves "nothing in
/// the stream to attribute the difference to". This renderer used to read
/// `op` and `short_id` and nothing else, so the attribution died at the last
/// hop: a script tallying the stream learned it lost events and could not
/// learn how many, while the number existed, was computed, was sent, and was
/// logged on the far side of the socket.
fn watch_stream_line(data: &Value) -> String {
    let op = data.get("op").and_then(Value::as_str).unwrap_or("change");
    let mut line = format!("task.changed op={op}");
    if let Some(s) = data.get("short_id").and_then(Value::as_i64) {
        line.push_str(&format!(" short_id={s}"));
    }
    if let Some(d) = data.get("dropped").and_then(Value::as_i64) {
        line.push_str(&format!(" dropped={d}"));
    }
    line
}

/// Fetch the working set over the socket and (re)paint it, reusing render.rs so
/// themes + degradation behave exactly as in the one-shot list view.
fn watch_render(conn: &mut daemon::Conn, filter: &str, ctx: &Ctx, tty: bool) -> Result<(), String> {
    let params = json!({ "filter": filter, "sort": ["-urgency"] });
    let env = conn
        .request("task.list", &params)
        .map_err(|e| format!("task.list: {e}"))?;
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
        McpAction::Serve { scope } => {
            let scope = if scope == "read" {
                Scope::Read
            } else {
                Scope::Write
            };
            run_mcp_serve(scope);
        }
    }
}

/// Run the MCP stdio server under the operator-selected capability scope.
/// `Scope` configures this local child process; it is not an authentication
/// credential. Diagnostics go to stderr only, while stdout carries nothing but
/// newline-delimited JSON-RPC responses.
fn run_mcp_serve(scope: Scope) {
    let engine = match open_engine() {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("tasqx mcp: {msg}");
            exit(1);
        }
    };
    eprintln!("tasqx mcp: serving over stdio (scope={})", scope.as_str());

    let server = McpServer::new(&engine, scope).with_time_format(config_detail_time_format());
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    // Hold both locks for the whole session: this process writes nothing else
    // to either handle while serving, and the guard is what keeps a stray
    // `println!` from ever interleaving with a response frame on stdout.
    mcp_stdio_loop(
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut std::io::stderr(),
        |msg| server.handle_message(msg),
    );
}

/// The read/dispatch/write half of [`run_mcp_serve`], factored out so both the
/// panic containment and the stdin-failure diagnostic can be driven from tests
/// without a real process's stdio. `dispatch` is the injection seam: the server
/// passes `McpServer::handle_message`, tests pass a closure that panics.
fn mcp_stdio_loop(
    reader: &mut impl BufRead,
    out: &mut impl Write,
    errs: &mut impl Write,
    dispatch: impl Fn(&Value) -> Option<Value>,
) {
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
                let resp = match serde_json::from_str::<Value>(trimmed) {
                    // Contain a dispatch panic the way the daemon does
                    // (daemon.rs, `handle_conn`): this server runs unsupervised
                    // inside an agent's process, so a panic reachable from any
                    // tool call — today the recurrence overflow, tomorrow
                    // whatever a new verb introduces — must cost that one call,
                    // not the whole session. `AssertUnwindSafe` is required
                    // because rusqlite's `Connection` is not `RefUnwindSafe`,
                    // and sufficient because `MutationContext` rolls its
                    // `Transaction` back while unwinding and the `Engine` is
                    // held bare (no lock to poison, hence no `lock_recover`
                    // analogue here).
                    Ok(msg) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        dispatch(&msg)
                    })) {
                        Ok(v) => v,
                        // Gate the envelope on a present, non-null `id`: a
                        // notification is answered with silence by contract, and
                        // an `id: null` error would put a frame nobody asked for
                        // on the stdout channel reserved for responses.
                        Err(_) => msg.get("id").filter(|id| !id.is_null()).map(|id| {
                            json!({
                                "jsonrpc": "2.0", "id": id.clone(),
                                "error": { "code": -32603, "message": "internal error" }
                            })
                        }),
                    },
                    Err(e) => Some(json!({
                        "jsonrpc": "2.0", "id": Value::Null,
                        "error": { "code": -32700, "message": format!("Parse error: {e}") }
                    })),
                };
                if let Some(resp) = resp {
                    let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap_or_default());
                    let _ = out.flush();
                }
            }
            // A read failure ends the session, so it is the operator's last
            // chance to learn why: dropping `e` made an I/O fault or a non-UTF-8
            // byte on stdin indistinguishable from the peer closing cleanly.
            Err(e) => {
                let _ = writeln!(errs, "tasqx mcp: stdin read failed: {e}");
                break;
            }
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

/// Answer "which store does this process actually write to?".
///
/// All inputs are passed rather than read here, so both daemon branches are
/// reachable from a test without a listening socket.
///
/// The two cases are NOT variations on one sentence. In-process, the local file
/// IS the store. Through a daemon, it is not: `open_backend` prefers a reachable
/// daemon and the remote path never consults `TASQX_DB`, so the local path is
/// inert and reporting it would restate the exact falsehood this surface exists
/// to kill. The local `path` is therefore still absent on the daemon branch —
/// but the daemon's own file is not guessed, it is `daemon_store`: the caller
/// asked the daemon (`core.capabilities.store`, D74), which retires D47's "a
/// client cannot know the daemon's file" without touching what D47 actually
/// forbade. `None` here means an older daemon that predates the field, and the
/// answer degrades to naming the socket alone rather than inventing a path.
fn store_location(
    remote_socket: Option<&str>,
    daemon_store: Option<&str>,
    path: Result<PathBuf, String>,
) -> (Value, String) {
    if let Some(socket) = remote_socket {
        let owns = match daemon_store {
            Some(store) => format!("the daemon owns the store: {store}"),
            None => "the daemon owns the store; it predates D74 and cannot name it".to_string(),
        };
        return (
            json!({
                "backend": "daemon",
                "socket": socket,
                "store": daemon_store.map(str::to_string),
            }),
            format!(
                "daemon at {socket}\n  {owns}\n  $TASQX_DB is NOT in effect here. \
                 Pass --no-daemon to work on your own store instead.\n"
            ),
        );
    }
    match path {
        Ok(p) => {
            let p = p.to_string_lossy().into_owned();
            (
                json!({ "backend": "local", "path": p }),
                format!("{p}\n  in-process; this file is the store.\n"),
            )
        }
        // A path this process could not resolve is a fact, not an omission: it
        // is exactly the state in which every later command fails, and naming it
        // here is cheaper than reading that failure off a write.
        Err(e) => (
            json!({ "backend": "local", "path": Value::Null, "error": e }),
            format!("(no store path: {e})\n"),
        ),
    }
}

fn db_path() -> Result<PathBuf, String> {
    db_path_resolved(true)
}

/// The store path a command WOULD open, resolved without touching the disk.
///
/// The completion callback (`complete::lookup`) needs the same answer
/// [`db_path`] gives and none of its side effects. That is not a preference:
/// the whole promise of the `$TASQX_COMPLETE` path is that pressing Tab creates
/// nothing, and [`db_path`] creates the parent directory of `$TASQX_DB` and the
/// platform data directory before it returns. A Tab press on a machine that has
/// never run tasqx would therefore author `%APPDATA%\tasqx\tasqx\data\`, and
/// `tests/completion.rs` asserts against the filesystem that it does not.
///
/// Split off [`db_path`] rather than written beside it, because the two must
/// agree about WHERE the store is or completion offers ids from a store no
/// command reads. A second copy of the `$TASQX_DB`-then-`ProjectDirs` rule is
/// exactly the parallel-copy drift this repository keeps paying for; the
/// difference between the two callers is one boolean and nothing else.
fn db_path_read_only() -> Result<PathBuf, String> {
    db_path_resolved(false)
}

/// `$TASQX_DB` if set and non-empty, else the platform data dir. `create_dirs`
/// decides whether the containing directory is brought into existence on the
/// way — see [`db_path_read_only`] for why that is a caller's choice rather
/// than a fixed behaviour.
fn db_path_resolved(create_dirs: bool) -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("TASQX_DB") {
        if !p.is_empty() {
            if create_dirs {
                if let Some(parent) = PathBuf::from(&p).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            return Ok(PathBuf::from(p));
        }
    }
    let dirs = directories::ProjectDirs::from("dev", "tasqx", "tasqx")
        .ok_or_else(|| "cannot determine a data directory".to_string())?;
    let dir = dirs.data_dir();
    if create_dirs {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create data dir: {e}"))?;
    }
    Ok(dir.join("tasks.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The flattening from a `task.list` answer to screen rows. Every field on
    /// this screen decides which task the user starts, and the whole path
    /// around it needs a real terminal — so a mapping that read `id` where it
    /// meant `short_id`, or dropped the project, would leave the suite green
    /// with the chooser unusable. That is the hole `settings_rows` was pulled
    /// out of `run_config_edit` to close, one screen over.
    #[test]
    fn pick_rows_carry_every_field_the_screen_tells_tasks_apart_by() {
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
        assert_eq!(rows[0].project, "work.tasqx");
        assert_eq!(rows[0].priority, "H");
        assert_eq!(rows[0].urgency, "11.8", "urgency is shown to one decimal");
        assert_eq!(rows[0].tags, "release api");

        // A task missing the optional fields is still a pickable row, and its
        // priority reads as a fact rather than as a blank cell.
        assert_eq!(rows[1].short_id, 7);
        assert_eq!(rows[1].priority, "-");
        assert_eq!(rows[1].urgency, "0.0");
        assert!(rows[1].project.is_empty() && rows[1].tags.is_empty());
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

    /// The scrollback line `pick` leaves once the alt screen is gone. It has to
    /// NAME the task: `render::started` prints "Started task · timer running"
    /// and nothing else, which is right for `tasqx start 42` — the user typed
    /// the ref — and leaves an interactive session with no record of which task
    /// it started. This is the only reachable test of that line; the rest of
    /// `run_pick` needs a real terminal.
    #[test]
    fn the_pick_summary_names_the_task_it_started() {
        let started = json!({ "id": "0199-uuid", "interval_started": "2026-08-03T10:00:00Z" });
        let text = picked_summary(&plain_ctx(), 42, "Ship the v1 JSON API freeze", &started);
        assert!(text.contains("#42"), "{text}");
        assert!(text.contains("Ship the v1 JSON API freeze"), "{text}");
        assert!(
            text.contains("Started task"),
            "the timer line must survive too: {text}"
        );
        assert!(text.ends_with('\n'), "{text:?}");
    }

    /// `--json` must carry the identity of the task that was picked. The method
    /// answers `{id, interval_started}` — a UUID and a timestamp — and the ref
    /// is the very thing `pick` was asked to determine, so without these two
    /// fields the machine-readable answer omits the answer. The method's own
    /// keys are passed through untouched beside them.
    #[test]
    fn the_pick_json_carries_the_chosen_ref_beside_the_methods_own_answer() {
        let started = json!({ "id": "0199-uuid", "interval_started": "2026-08-03T10:00:00Z" });
        let out = pick_result(42, "Ship the freeze", started);
        assert_eq!(out["short_id"], json!(42));
        assert_eq!(out["title"], json!("Ship the freeze"));
        assert_eq!(out["id"], json!("0199-uuid"));
        assert_eq!(out["interval_started"], json!("2026-08-03T10:00:00Z"));
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
            let p = report_params(&[axis.to_string()], false);
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
        let p = report_params(&["+api".to_string()], false);
        assert_eq!(p["group_by"], tasqx_core::engine::SUMMARY_GROUP_BY[0]);
        assert_eq!(p["filter"], "+api");
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
        assert_eq!(report_params(&[], true)["all"], json!(true));
        // Absent by default: core's `all` defaults to false, and sending an
        // explicit `false` would be the same thing said twice.
        assert!(report_params(&[], false).get("all").is_none());
    }

    /// The group_by-then-filter split is positional and easy to break; `--all`
    /// is a flag and must compose with both halves rather than displacing them.
    #[test]
    fn report_all_composes_with_group_by_and_filter() {
        let p = report_params(&["status".to_string(), "project:x".to_string()], true);
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
            let (result, _) = run_report(&mut be, &ctx, vec![], all).expect("report ran");
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
        // No such project: task.list still parses the filter fine, so this
        // asserts the honest empty case stays empty and Ok...
        let (members, _) = burndown_members(&e, &Some("nope".to_string())).expect("parses");
        assert!(
            members.is_empty(),
            "an unknown project is legitimately empty, not an error"
        );

        // ...while a filter that cannot parse must come back as Err. `"` alone
        // is unterminable: quoting it is fine, but this bypasses `quote` to
        // simulate any future composition bug reaching `task.list`.
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
