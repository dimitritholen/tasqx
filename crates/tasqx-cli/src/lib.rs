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
pub mod config;
mod docs;
mod html;
mod manual;
mod render;
mod sugar;
mod theme;
mod tui;

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::Parser;
use serde_json::{json, Value};

use tasqx_core::{
    daemon, datetime, dispatch, handle_envelope, notify, ApiError, Engine, ErrorCode, McpServer,
    Scope,
};

use command::{ChartKind, Cli, Command, ConfigAction, McpAction, MemoryAction, ThemeAction};
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
/// This table is load-bearing, not documentation. [`Exit::self_framed`] is the
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

pub fn run() {
    // Not `Cli::parse()`: filter tokens like `-needs` must reach the grammar,
    // and the only way to keep that from disarming clap's flag handling is to
    // hide the dash before clap looks. See `argv`.
    let pre = argv::prepass(std::env::args_os());
    let mut cli = match Cli::try_parse_from(pre.argv) {
        Ok(cli) => cli,
        Err(e) => exit_on_parse_error(&e, pre.filter_command),
    };
    // Put the dashes back, in ONE place, before any filter value is read.
    // Every hyphen-tolerant positional in the tree is listed here; the drift
    // guard `argv::tests::every_filter_positional_is_registered` fails if a new
    // one appears without being added to the pre-pass side of the same pair.
    match &mut cli.command {
        Some(Command::List { filter } | Command::Export { filter } | Command::Watch { filter }) => {
            argv::unescape(filter)
        }
        Some(Command::Report { args, .. }) => argv::unescape(args),
        _ => {}
    }

    // Read before `cli` is moved into `execute`, which consumes it by value.
    let json = cli.json;

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
        }
        Exit::Out(Err(e)) => {
            eprintln!("error [{}]: {}", code_str(&e), e.message);
            exit(e.exit_code());
        }
    }
}

/// Run the parsed command, yielding whatever the terminal in [`run`] should do
/// with it. Every `return` in here owes an [`Exit`].
fn execute(cli: Cli) -> Exit {
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

    Exit::Out(match cli.command {
        None => run_list(&mut backend, &ctx, &[]),
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
        Some(Command::Start { r#ref, keep }) => run_start(&mut backend, &ctx, r#ref, keep),
        Some(Command::Stop { r#ref }) => run_stop(&mut backend, &ctx, r#ref),
        Some(Command::Done { r#ref }) => run_done(&mut backend, &ctx, r#ref),
        Some(Command::Show { r#ref }) => run_show(&mut backend, &ctx, r#ref),
        Some(Command::Cancel { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.cancel", r#ref),
        Some(Command::Reopen { r#ref }) => run_simple_ref(&mut backend, &ctx, "task.reopen", r#ref),
        Some(Command::Annotate { r#ref, text }) => run_annotate(&mut backend, &ctx, r#ref, text),
        Some(Command::Dep { r#ref, depends_on }) => {
            run_dep(&mut backend, &ctx, "dependency.add", r#ref, depends_on)
        }
        Some(Command::Undep { r#ref, depends_on }) => {
            run_dep(&mut backend, &ctx, "dependency.remove", r#ref, depends_on)
        }
        Some(Command::Use { name }) => run_use(&mut backend, &ctx, name),
        Some(Command::Projects { all }) => run_projects(&mut backend, &ctx, all),
        Some(Command::Report { args, all, .. }) => run_report(&mut backend, &ctx, args, all),
        Some(Command::Config { action }) => {
            run_config(&mut backend, &ctx, &action, theme_flag.as_deref())
        }
        Some(Command::Memory { action }) => run_memory(&mut backend, &action),
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
        Some(Command::Manual { .. }) => unreachable!("handled above"),
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
    Ctx::new(theme, Caps::detect())
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

// ---- charts, HTML report, and theme tools (DESIGN.md §8) --------------------

/// `tasqx chart <kind>`: read the event log and render a native terminal chart.
/// `tasqx chart throughput|heatmap|burndown`.
///
/// Each arm computes its SERIES once and hands the same values to both the
/// renderer and the JSON. The series is the answer; the sparkline is one way of
/// looking at it, and a script that wants the numbers should not have to parse
/// block glyphs back into integers to get them.
fn run_chart(engine: &Engine, ctx: &Ctx, kind: ChartKind) -> CmdOutcome {
    let events = dispatch(engine, "event.list", &json!({ "limit": 100000 }))?;
    let anchor = chart::today();
    Ok(match kind {
        ChartKind::Throughput { weeks, .. } => {
            let weeks = chart::default_weeks(false, weeks);
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

/// Every status a burndown counts as membership — i.e. everything but
/// `cancelled`. Spelled out positively because `task.list` keeps its literal
/// "no filter = all rows" contract (D24 is a *report* rule, applied in core to
/// `report.summary` only), so the exclusion has to live here, at the CLI.
///
/// Derived from `Status::ALL` + `counts_in_reports()` rather than typed out, so
/// a new variant joins the burndown by construction. The hand-written version of
/// this constant had already lost `status:backlog` once, and nothing failed —
/// a burndown silently missing tasks still looks like a valid burndown.
static NOT_CANCELLED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    tasqx_core::types::Status::ALL
        .into_iter()
        .filter(|s| s.counts_in_reports())
        .map(|s| format!("status:{}", s.as_str()))
        .collect::<Vec<_>>()
        .join(" or ")
});

/// Resolve the task ids a burndown covers, plus its label. Split out of the
/// `ChartKind::Burndown` arm so the scope rule is testable on its own.
///
/// Both branches go through `task.list` with [`NOT_CANCELLED`]. The `None`
/// branch previously used an unfiltered `store.export`, which is what let
/// cancelled tasks inflate the whole-store burndown's "remaining work" line.
fn burndown_members(
    engine: &Engine,
    project: &Option<String>,
) -> Result<(std::collections::HashSet<String>, String), ApiError> {
    let (filter, label) = match project {
        // Through `filter::quote`, never interpolated: a project may be named
        // `Home Renovation` or `a (b)`, and a raw `{p}` composes a filter that
        // asks a different question (or none at all) without saying so.
        Some(p) => (
            format!(
                "project:{} and ({})",
                tasqx_core::filter::quote(p),
                *NOT_CANCELLED
            ),
            p.clone(),
        ),
        None => (NOT_CANCELLED.to_string(), "all tasks".to_string()),
    };
    let listed = dispatch(
        engine,
        "task.list",
        &json!({ "filter": filter, "fields": ["id"] }),
    )?;
    let ids = listed
        .get("tasks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
                .collect::<std::collections::HashSet<String>>()
        })
        .unwrap_or_default();
    Ok((ids, label))
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
                }
                None => Ctx::new(ctx.theme.clone(), ctx.caps),
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

/// `tasqx config list|get|path`.
/// Reject a value that would persist but never take effect.
///
/// This lives here, shared, because the first version put theme validation
/// inline in `theme set` only — so `tasqx theme set bogus` was rejected while
/// `tasqx config set theme.name bogus` wrote it happily and exited 0. The
/// primitive was looser than its own alias, which is backwards: `theme::load`
/// falls back to the default for an unknown name, so the write persists a value
/// that silently does nothing on every run from then on.
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
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n",
        ctx.paint(
            "header",
            &format!("{:<18} {:<22} {}", "SETTING", "VALUE", "SOURCE")
        )
    ));
    for r in rows {
        let key = r["key"].as_str().unwrap_or("");
        let val = r["value"].as_str().unwrap_or("");
        let src = r["source"].as_str().unwrap_or("");
        let shown = if val.is_empty() { "(unset)" } else { val };
        // `render::pad` measures terminal CELLS, not chars, so a value carrying
        // CJK or an emoji — an editor path, a project name — no longer shoves
        // the SOURCE column sideways. Padded, never truncated: the value is the
        // data the reader came for, and this table is where they read it.
        out.push_str(&format!(
            "{} {} {}\n",
            render::pad(key, 18),
            render::pad(shown, 22),
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

    eprintln!("tasqx daemon: listening on {socket} (Ctrl-C to stop)");
    let options = daemon::DaemonOptions {
        notifier,
        tokens_enabled,
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

    /// Every documented example must at least be a command this binary accepts.
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
        // 52 examples (27 Safe + 25 NoRun), two of which are two-command
        // pipelines and so contribute two segments each.
        assert!(
            checked >= 54,
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
    /// CLI, for both the whole-store and the per-project scope.
    #[test]
    fn burndown_scope_excludes_cancelled_tasks() {
        let e = tasqx_core::Engine::open_in_memory().unwrap();
        e.project_create(&json!({ "name": "P" })).unwrap(); // D23
        for title in ["live", "gone", "finished"] {
            e.task_add(&json!({ "title": title, "project": "P" }))
                .unwrap();
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
            assert!(
                members.contains(&live),
                "scope {scope:?} lost the open task"
            );
            assert!(
                !members.contains(&gone),
                "scope {scope:?} still counts a cancelled task as remaining work"
            );
            // `done` stays in scope: the burndown needs the completion event to
            // draw the line coming down. Dropping it would flatten the chart.
            assert!(
                members.contains(&finished),
                "scope {scope:?} lost the done task"
            );
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

    /// `NOT_CANCELLED` spells out its statuses by hand, and the test above only
    /// seeds pending/cancelled/done — so dropping `status:backlog` from the
    /// constant left the whole suite green while every backlog task silently
    /// vanished from every burndown. A chart missing tasks still looks like a
    /// valid chart, which is the same silent-omission class D24 exists to fix.
    ///
    /// This drives the expectation off `Status::ALL` and `counts_in_reports()`
    /// rather than restating the names. An earlier version of this guard looped
    /// over a hardcoded `["backlog", "pending", "active", "done"]` while its doc
    /// comment claimed a new variant would fail loudly — it would not have; the
    /// guard was the very kind of hand-maintained parallel list it existed to
    /// police. Adding a `Status` variant now changes this test's expectation
    /// automatically, so a `NOT_CANCELLED` that forgot it goes red.
    #[test]
    fn burndown_scope_keeps_every_status_except_cancelled() {
        // `expect` earns its keep now that parsing is fallible: NOT_CANCELLED is
        // our own constant, so a malformed one is a bug this guard should fail
        // on rather than route around.
        let f = tasqx_core::filter::Filter::parse(&NOT_CANCELLED, jiff::Timestamp::now())
            .expect("NOT_CANCELLED must be a valid filter");
        for status in tasqx_core::types::Status::ALL {
            let ctx = tasqx_core::filter::MatchCtx {
                status,
                project: None,
                tags: &[],
                due: None,
                completed: None,
                blocked: false,
            };
            assert_eq!(
                f.matches(&ctx),
                status.counts_in_reports(),
                "NOT_CANCELLED disagrees with Status::counts_in_reports about `{}`",
                status.as_str()
            );
        }
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
    /// design (DESIGN.md §725: cancellation is reversible and logged), so the
    /// fix is to make the word people actually type land on that verb.
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

    /// The temp path must be stable across runs (so `docs` does not litter) and
    /// must actually be an HTML file (so a browser renders rather than downloads).
    #[test]
    fn docs_temp_path_is_stable_and_html() {
        let a = docs_temp_path();
        let b = docs_temp_path();
        assert_eq!(a, b, "the temp path must not vary between invocations");
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("html"));
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
