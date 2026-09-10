//! The serving surfaces: `tasqx daemon`, `watch`'s subscribe-and-repaint
//! loop, the `api` stdio one-shot, and the MCP stdio server with its panic
//! containment. Everything here owns stdout as a self-framed protocol or
//! never returns; the emit seam it writes through stays in lib.rs, where the
//! Exit terminal lives.

use super::*;

/// `tasqx daemon`: open one Engine and serve the local socket until Ctrl-C.
/// Diagnostics go to stderr; the socket carries the newline-delimited JSON API.
pub(crate) fn run_daemon(socket_flag: Option<&str>, db: Option<&str>) {
    let socket = resolve_socket(socket_flag);
    let engine = match open_engine_at(db) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("tasqx daemon: {msg}");
            exit(1);
        }
    };

    // Ctrl-C (SIGINT) OR a plain `kill` (SIGTERM — what systemd `stop`,
    // `docker stop` and supervisord send) flips the shutdown flag; `serve`
    // then unwinds its accept loop and removes the Unix socket file (a no-op
    // for Windows named pipes). The `termination` feature on `ctrlc` (Cargo.toml)
    // is what makes SIGTERM reach this handler on Unix — without it, only
    // SIGINT did, so an operator who is not watching a terminal killed the
    // daemon silently and left a stale socket behind (#236.1).
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let sd = shutdown.clone();
        if let Err(e) = ctrlc::set_handler(move || sd.store(true, Ordering::SeqCst)) {
            eprintln!("tasqx daemon: could not install signal handler: {e}");
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

/// The reconnect backoff `run_watch` uses once a subscribed connection dies
/// mid-session (#236.5): starts short so a daemon restart (an upgrade, a
/// crash) is barely noticed, and caps so a pane left open for a day polls at
/// a sane rate rather than spinning.
const WATCH_RECONNECT_INITIAL_DELAY: Duration = Duration::from_millis(200);
const WATCH_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(10);

/// Block until a new subscribed connection to `socket` is up, retrying with
/// exponential backoff and a visible status line — the pane is meant to be
/// left open for a day, so a daemon restart, an upgrade, or
/// `daemon.idle_timeout` firing must not kill the view permanently (#236.5).
/// Returns only on success; the only way out otherwise is the user asking
/// (Ctrl-C / SIGTERM, which end the process the same way they always have —
/// this loop installs no handler of its own and simply gets interrupted).
fn watch_reconnect(socket: &str) -> daemon::Conn {
    let mut delay = WATCH_RECONNECT_INITIAL_DELAY;
    loop {
        if let Some(mut conn) = daemon::try_connect(socket) {
            if conn.subscribe().is_ok() {
                eprintln!("tasqx watch: reconnected to {socket}");
                return conn;
            }
        }
        eprintln!(
            "tasqx watch: daemon unreachable at {socket}; reconnecting in {:.1}s…",
            delay.as_secs_f64()
        );
        std::thread::sleep(delay);
        delay = (delay * 2).min(WATCH_RECONNECT_MAX_DELAY);
    }
}

/// `tasqx watch [filter]`: subscribe to a daemon and re-render on every push.
/// On a TTY it draws into the alternate screen, bounded to what the terminal
/// can actually show — the top of the `-urgency`-sorted list, never whatever
/// tail a too-tall frame happened to leave behind once it scrolled (#206); on
/// a pipe it streams one line per event (DESIGN.md §6a). It never auto-spawns
/// a daemon — it hints instead on the very first connect. Once subscribed, a
/// connection that later dies (daemon restart, crash, idle-timeout exit) is
/// retried with backoff rather than ending the session (#236.5) — the last
/// frame stays on screen, with a `reconnecting…` status line on stderr, until
/// a new daemon answers.
pub(crate) fn run_watch(socket_flag: Option<&str>, no_daemon: bool, filter: &[String], ctx: &Ctx) {
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

    // #206: everything below returns an exit code instead of calling `exit`
    // directly, so the TTY path's `WatchScreen` guard is still on the stack
    // — and therefore still able to leave the alternate screen — at every one
    // of those returns. `exit` runs no destructors, and a `watch` left in the
    // alternate screen is exactly the stuck pane a live view must never leave
    // behind.
    let code = watch_session(&mut conn, &socket, &filter_str, ctx, tty);
    exit(code);
}

/// The subscribe-and-repaint loop, factored out of `run_watch` so its TTY
/// path can hold a [`WatchScreen`] guard across every exit — see `run_watch`.
fn watch_session(
    conn: &mut daemon::Conn,
    socket: &str,
    filter_str: &str,
    ctx: &Ctx,
    tty: bool,
) -> i32 {
    let _screen = if tty {
        match WatchScreen::enter() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("tasqx watch: could not enter the alternate screen: {e}");
                return 1;
            }
        }
    } else {
        None
    };

    // Initial paint.
    match watch_render(conn, filter_str, ctx, tty, None) {
        Ok(true) => {}
        Ok(false) => return 0,
        Err(e) => {
            eprintln!("tasqx watch: {e}");
            return 1;
        }
    }

    // Live loop: block on the next frame; on each change, refresh. A dead
    // connection reconnects in place rather than ending the process.
    loop {
        match conn.next_frame() {
            Ok(Some(daemon::Frame::Event(evt))) => {
                if tty {
                    // Folded into the SAME frame as a note line, rather than a
                    // separate `eprintln!` ahead of it (the previous design):
                    // that relied on the note surviving in the plain screen's
                    // scrollback once the repaint scrolled past it (#249), and
                    // the alternate screen this path now draws into has no
                    // such scrollback for it to land in.
                    let note = evt
                        .pointer("/data/dropped")
                        .and_then(Value::as_i64)
                        .map(|d| {
                            format!(
                                "{d} event(s) dropped while this view was not keeping up; \
                             the repaint is current"
                            )
                        });
                    match watch_render(conn, filter_str, ctx, true, note.as_deref()) {
                        Ok(true) => {}
                        Ok(false) => return 0,
                        Err(e) => {
                            eprintln!("tasqx watch: {e}");
                            return 1;
                        }
                    }
                } else {
                    let data = evt.get("data").cloned().unwrap_or(Value::Null);
                    if !emit_open(&format!("{}\n", watch_stream_line(&data))) {
                        // `watch | head`: the reader left, so the stream is
                        // over — cleanly, not as a BrokenPipe panic.
                        return 0;
                    }
                }
            }
            // A stray response (none expected here) is harmless; ignore it.
            Ok(Some(daemon::Frame::Response(_))) => {}
            Ok(None) | Err(_) => {
                eprintln!("tasqx watch: connection lost; reconnecting…");
                *conn = watch_reconnect(socket);
                // The working set may have moved while disconnected; a fresh
                // full repaint is the only way to know it is current again.
                match watch_render(conn, filter_str, ctx, tty, None) {
                    Ok(true) => {}
                    Ok(false) => return 0,
                    Err(e) => {
                        eprintln!("tasqx watch: {e}");
                        return 1;
                    }
                }
            }
        }
    }
}

/// RAII entry into the alternate screen for `watch`'s TTY path (#206).
///
/// Deliberately thin, the way `tui::with_terminal` is kept thin: no
/// decisions, no rendering, just the enter/leave calls — everything either
/// one touches (the panic hook, the restore latch, the escape sequences) is
/// already tested in `tui.rs`, reused rather than duplicated here.
///
/// Unlike `tui::with_terminal`, this never calls `enable_raw_mode`: `watch`
/// reads nothing from stdin, so raw mode would only turn OFF Ctrl-C's normal
/// SIGINT delivery for no benefit — worse, it would turn `watch`'s usual way
/// out into a hang. A `ctrlc` handler covers that interrupt instead (mirroring
/// `run_daemon`'s), restoring the screen before exiting, since the OS's
/// default SIGINT action — like `std::process::exit` — runs no `Drop` either.
struct WatchScreen;

impl WatchScreen {
    fn enter() -> std::io::Result<Self> {
        tui::install_panic_hook();
        ratatui::crossterm::execute!(
            std::io::stdout(),
            ratatui::crossterm::terminal::EnterAlternateScreen,
            ratatui::crossterm::cursor::Hide
        )?;
        tui::IN_RAW_MODE.store(true, Ordering::SeqCst);
        // Best-effort: if a handler is already installed (unusual — nothing
        // else in this process sets one before `watch` runs), Ctrl-C falls
        // back to the OS default and the panic-hook/Drop paths below still
        // cover every other exit.
        let _ = ctrlc::set_handler(|| {
            tui::restore_once(&tui::IN_RAW_MODE, &mut std::io::stdout());
            exit(0);
        });
        Ok(WatchScreen)
    }
}

impl Drop for WatchScreen {
    fn drop(&mut self) {
        tui::restore_once(&tui::IN_RAW_MODE, &mut std::io::stdout());
    }
}

/// Trim `result`'s `tasks` array — already sorted hottest-urgency-first by
/// the caller's `sort:["-urgency"]` — to the `rows` a pane can actually draw
/// above `chrome` lines of header/rule(s)/trailer, keeping the FRONT of the
/// list. `count`/`total` are left untouched, so the trailer `render::
/// task_table` prints still names the true size of the working set even when
/// fewer rows than that are on screen.
///
/// This is #206's fix: on a 24-row pane over 44 tasks, printing all of them
/// let the terminal's own scroll carry the hottest rows — #9, #1, #2 in the
/// field report — off the top, leaving only the coldest tail visible. Never
/// truncates to nothing: a pane too short even for its own chrome still shows
/// one row rather than "No tasks." on a store that plainly has some.
fn bound_to_viewport(mut result: Value, rows: u16, chrome: usize) -> Value {
    let Some(tasks) = result.get_mut("tasks").and_then(Value::as_array_mut) else {
        return result;
    };
    let available = (rows as usize).saturating_sub(chrome).max(1);
    if tasks.len() > available {
        tasks.truncate(available);
    }
    result
}

/// Style one status note as a frame line (see `watch_session`'s comment on
/// why this is folded into the frame rather than printed ahead of it).
fn note_line(ctx: &Ctx, note: &str) -> String {
    ctx.paint("warn", note)
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
pub(crate) fn watch_stream_line(data: &Value) -> String {
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
///
/// `note`, when given, is a status line (e.g. #249's dropped-event count)
/// folded into the top of the TTY frame — see `watch_session`.
///
/// Returns `Ok(false)` when the reader closed the pipe/terminal mid-write,
/// instead of calling `exit` the way this used to: the TTY path now runs
/// inside a `WatchScreen` alternate-screen guard (#206), and `exit` skips
/// every `Drop`, so `watch_session` — where that guard is actually in scope —
/// is the one place allowed to end the process.
pub(crate) fn watch_render(
    conn: &mut daemon::Conn,
    filter: &str,
    ctx: &Ctx,
    tty: bool,
    note: Option<&str>,
) -> Result<bool, String> {
    let params = json!({ "filter": filter, "sort": ["-urgency"] });
    let mut env = conn
        .request("task.list", &params)
        .map_err(|e| format!("task.list: {e}"))?;
    if env.get("ok") != Some(&Value::Bool(true)) {
        return Err(format!(
            "daemon error: {}",
            env.get("error").map(|e| e.to_string()).unwrap_or_default()
        ));
    }
    // Taken, not cloned — this repaints the whole working set on every push.
    let mut result = env
        .as_object_mut()
        .and_then(|o| o.remove("result"))
        .unwrap_or(Value::Null);

    // #206: a side pane is short. Bound the row list to what THIS terminal
    // can actually show before rendering, so the frame draws the top of the
    // already-`-urgency`-sorted list — never whatever tail a too-tall frame
    // used to be left showing once the terminal scrolled past the rest of
    // it. A `size()` failure (rare, and no worse than the old unbounded
    // behaviour) just skips the trim for this one frame.
    if tty {
        if let Ok((_, rows)) = ratatui::crossterm::terminal::size() {
            let chrome = if note.is_some() { 5 } else { 4 };
            result = bound_to_viewport(result, rows, chrome);
        }
    }

    let mut text = render::task_table(ctx, &result, jiff::Timestamp::now());
    if let Some(n) = note {
        text = format!("{}\n{text}", note_line(ctx, n));
    }
    let painted = if tty {
        // Clear screen + cursor home, then reprint the fresh (bounded) frame
        // — inside the alternate screen `WatchScreen::enter` opened, so this
        // never touches the scrollback the shell prompt lives in.
        format!("\x1b[2J\x1b[H{text}")
    } else {
        text
    };
    Ok(emit_open(&painted))
}

/// #184: probe for a live daemon on the ambient `$TASQX_SOCK` and, when one
/// answers with `$TASQX_DB` unset, print [`ambient_socket_note`] on stderr
/// before this verb opens its own in-process store. Shared by [`run_api`] and
/// [`run_mcp_serve`] — the two verbs D73 keeps off the socket entirely — so
/// the wiring cannot drift between them the way the gap itself did.
///
/// The cheap checks (is `$TASQX_SOCK` even set, is `$TASQX_DB` even unset) run
/// before the probe connection, so the common case — no ambient socket, or an
/// operator who already set `$TASQX_DB` — never dials out at all.
fn note_ambient_socket_if_unused(verb: &str) {
    let sock = std::env::var("TASQX_SOCK").ok().filter(|s| !s.is_empty());
    let tasqx_db_set = std::env::var("TASQX_DB").is_ok_and(|v| !v.is_empty());
    let Some(socket) = &sock else { return };
    if tasqx_db_set {
        return;
    }
    let mut conn = daemon::try_connect(socket);
    let daemon_store = conn.as_mut().and_then(|c| {
        let env = c.request("core.capabilities", &json!({})).ok()?;
        if env.get("ok") != Some(&Value::Bool(true)) {
            return None;
        }
        env.get("result")?
            .get("store")?
            .as_str()
            .map(str::to_string)
    });
    let daemon_reachable = conn.is_some();
    drop(conn);
    let local = db_path_read_only()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|e| format!("(no store path: {e})"));
    if let Some(note) = ambient_socket_note(
        verb,
        Some(socket),
        tasqx_db_set,
        daemon_reachable,
        daemon_store.as_deref(),
        &local,
    ) {
        eprintln!("{note}");
    }
}

/// #228.6: typed bare on a terminal, `tasqx api` used to sit with no prompt
/// and no output — the process was waiting on `read_to_string`, but nothing
/// on screen said so, so it read exactly like a hang. Every other
/// terminal-sensitive verb says so (`dashboard`'s refusal, `config edit`'s
/// "needs an interactive terminal"); this is the same courtesy for the one
/// verb whose own `--help` EXAMPLE is a heredoc.
pub(crate) const API_TTY_HINT: &str =
    "reading one JSON request envelope from stdin; Ctrl-D to send, Ctrl-C to quit";

/// Testable seam for the tty check below: a pure function of the one fact
/// that decides it, the way `dashboard_refusal` takes its terminal facts as
/// plain booleans rather than reading `is_terminal()` itself.
pub(crate) fn api_stdin_hint(stdin_is_tty: bool) -> Option<&'static str> {
    stdin_is_tty.then_some(API_TTY_HINT)
}

/// The stdio one-shot transport.
///
/// Reads the envelope BEFORE opening the store, on purpose: opening the
/// store first meant a store-open failure fired before the request had even
/// been read, so the one response shape a multiplexed caller most needs to
/// correlate — the one that fires when the whole store is unreachable — was
/// also the one that could never carry the request `id`.
pub(crate) fn run_api() {
    note_ambient_socket_if_unused("api");

    if let Some(hint) = api_stdin_hint(std::io::stdin().is_terminal()) {
        eprintln!("{hint}");
    }

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        let env = json!({
            "tasqx": "1", "ok": false,
            "error": { "code": "bad_request", "message": "could not read stdin" }
        });
        emit(&format!("{env}\n"));
        exit(2);
    }

    let engine = match open_engine() {
        Ok(e) => e,
        Err(msg) => {
            let id = tasqx_core::dispatch::peek_envelope_id(&input);
            let env = tasqx_core::dispatch::error_envelope(
                id,
                &tasqx_core::error::ApiError::internal(msg),
            );
            emit(&format!("{env}\n"));
            exit(1);
        }
    };

    let response = handle_envelope(&engine, &input);
    // The manual (`tasqx manual json-api`) promises "exit codes mirror the
    // error model: 0 ok, 2 bad_request, 4 not_found, 5 conflict" — the same
    // mapping every CLI verb already applies to its `ApiError`. Before this,
    // `run_api` printed the envelope and fell off the end, so the process's
    // own exit code stayed 0 regardless of `ok`, and a `set -e` wrapper or a
    // `tasqx api ... || rollback` around a refused write saw success (#169).
    // `api_error_from_env` is the same reconstruction the daemon-routed path
    // already uses to turn an error envelope back into an `ApiError`, so this
    // reuses it rather than re-deriving the code->exit table a second time.
    let code = if response.get("ok") == Some(&Value::Bool(true)) {
        0
    } else {
        api_error_from_env(&response).exit_code()
    };
    emit(&format!(
        "{}\n",
        serde_json::to_string(&response).unwrap_or_default()
    ));
    exit(code);
}

/// The `tasqx mcp` subcommand family (DESIGN.md §7, D7).
pub(crate) fn run_mcp(action: &McpAction) {
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
pub(crate) fn run_mcp_serve(scope: Scope) {
    note_ambient_socket_if_unused("mcp serve");
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
pub(crate) fn mcp_stdio_loop(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    /// A `-urgency`-sorted working set of `n` tasks, exactly the shape
    /// `watch_render`'s `task.list` call gets back: `short_id` 1 is the
    /// hottest (highest `urgency`), `n` the coldest.
    fn working_set(n: i64) -> Value {
        let tasks: Vec<Value> = (1..=n)
            .map(|id| {
                json!({
                    "short_id": id, "urgency": (n - id) as f64, "priority": "M",
                    "title": format!("task {id}"), "project": "work", "due": "",
                    "tags": [], "status": "pending"
                })
            })
            .collect();
        json!({ "count": n, "total": n, "next_offset": Value::Null, "tasks": tasks })
    }

    fn plain_ctx() -> Ctx {
        Ctx::new(theme::default_theme(), Caps::PLAIN)
    }

    /// The fix: `bound_to_viewport` ahead of `render::task_table` keeps the
    /// frame inside the pane AND keeps the row the pane exists to show — #1,
    /// the hottest — rather than whatever the tail happens to be, while the
    /// trailer keeps naming the true size of the working set.
    #[test]
    fn bound_to_viewport_keeps_the_frame_inside_the_pane_and_the_hottest_row_in_it() {
        let result = working_set(44);
        let bounded = bound_to_viewport(result, 24, 4);
        let text = render::task_table(&plain_ctx(), &bounded, jiff::Timestamp::now());
        let lines = text.lines().count();
        assert!(
            lines <= 24,
            "bounded frame is still {lines} lines on a 24-row pane"
        );
        assert!(
            text.contains("task 1"),
            "the hottest task must survive the trim: {text}"
        );
        assert!(
            !text.contains("task 44"),
            "the coldest task must be the one trimmed, not #1: {text}"
        );
        assert!(
            text.contains("44 task(s)"),
            "the trailer must still name the true total: {text}"
        );
    }

    #[test]
    fn bound_to_viewport_is_a_no_op_once_everything_already_fits() {
        let result = working_set(5);
        let bounded = bound_to_viewport(result, 24, 4);
        assert_eq!(bounded["tasks"].as_array().unwrap().len(), 5);
        assert_eq!(bounded["count"], 5);
    }

    /// A pane too short to show even one row's chrome must still show ONE
    /// row rather than nothing — `bound_to_viewport` never truncates to 0.
    #[test]
    fn bound_to_viewport_never_empties_a_nonempty_list() {
        let result = working_set(10);
        let bounded = bound_to_viewport(result, 2, 4);
        assert_eq!(bounded["tasks"].as_array().unwrap().len(), 1);
    }

    /// The dropped-events note (#249) is folded into the frame itself now
    /// that the TTY path draws into the alternate screen (#206) — a
    /// screen with no reliable scrollback for a preceding `eprintln!` to
    /// survive in, unlike the plain-scrollback screen the old design relied
    /// on. `note_line` is the pure half of that: it must carry the exact
    /// count through, styled, so a viewer sees it in the very frame it
    /// describes rather than losing it to a redraw with no history.
    #[test]
    fn note_line_carries_the_dropped_count_into_the_frame() {
        let ctx = plain_ctx();
        let line = note_line(
            &ctx,
            "3 event(s) dropped while this view was not keeping up; the repaint is current",
        );
        assert!(line.contains("3 event(s) dropped"));
    }

    /// #228.6: `tasqx api` on a bare terminal used to give no prompt and no
    /// output while it blocked on `read_to_string` — indistinguishable from a
    /// hang. Piped stdin (the documented, scripted usage) must stay exactly
    /// as quiet as before: the hint is for the interactive case only.
    #[test]
    fn the_stdin_hint_fires_only_on_a_real_terminal() {
        assert_eq!(api_stdin_hint(false), None, "piped stdin must stay silent");
        assert_eq!(api_stdin_hint(true), Some(API_TTY_HINT));
    }
}
