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
/// On a TTY it clears + reprints the working set; on a pipe it streams one line
/// per event (DESIGN.md §6a). It never auto-spawns a daemon — it hints instead
/// on the very first connect. Once subscribed, a connection that later dies
/// (daemon restart, crash, idle-timeout exit) is retried with backoff rather
/// than ending the session (#236.5) — the last frame stays on screen, with a
/// `reconnecting…` status line on stderr, until a new daemon answers.
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

    // Initial paint.
    if let Err(e) = watch_render(&mut conn, &filter_str, ctx, tty) {
        eprintln!("tasqx watch: {e}");
        exit(1);
    }

    // Live loop: block on the next frame; on each change, refresh. A dead
    // connection reconnects in place rather than ending the process.
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
                    if !emit_open(&format!("{}\n", watch_stream_line(&data))) {
                        // `watch | head`: the reader left, so the stream is
                        // over — cleanly, not as a BrokenPipe panic.
                        exit(0);
                    }
                }
            }
            // A stray response (none expected here) is harmless; ignore it.
            Ok(Some(daemon::Frame::Response(_))) => {}
            Ok(None) | Err(_) => {
                eprintln!("tasqx watch: connection lost; reconnecting…");
                conn = watch_reconnect(&socket);
                // The working set may have moved while disconnected; a fresh
                // full repaint is the only way to know it is current again.
                if let Err(e) = watch_render(&mut conn, &filter_str, ctx, tty) {
                    eprintln!("tasqx watch: {e}");
                    exit(1);
                }
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
pub(crate) fn watch_render(
    conn: &mut daemon::Conn,
    filter: &str,
    ctx: &Ctx,
    tty: bool,
) -> Result<(), String> {
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
    let result = env
        .as_object_mut()
        .and_then(|o| o.remove("result"))
        .unwrap_or(Value::Null);
    let text = render::task_table(ctx, &result, jiff::Timestamp::now());
    let painted = if tty {
        // Clear screen + cursor home, then reprint the fresh working set.
        format!("\x1b[2J\x1b[H{text}")
    } else {
        text
    };
    if !emit_open(&painted) {
        // The reader closed the pipe; there is nobody left to paint for.
        exit(0);
    }
    Ok(())
}

/// The stdio one-shot transport.
pub(crate) fn run_api() {
    let engine = match open_engine() {
        Ok(e) => e,
        Err(msg) => {
            let env = json!({
                "tasqx": "1", "ok": false,
                "error": { "code": "internal", "message": msg }
            });
            emit(&format!("{env}\n"));
            exit(1);
        }
    };

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        let env = json!({
            "tasqx": "1", "ok": false,
            "error": { "code": "bad_request", "message": "could not read stdin" }
        });
        emit(&format!("{env}\n"));
        exit(2);
    }

    let response = handle_envelope(&engine, &input);
    emit(&format!(
        "{}\n",
        serde_json::to_string(&response).unwrap_or_default()
    ));
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
