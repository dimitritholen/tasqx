//! The `tasqx board` listener (#637, #638): a live kanban served over loopback
//! HTTP to a browser, which moves a task by sending the API's own verbs.
//!
//! A separate module beside [`crate::otlp`], on the same [`crate::http`] reader,
//! because the threat models differ. The OTLP receiver is unauthenticated
//! telemetry ingress and off by default; the board authenticates, holds
//! server-sent-event connections open, and writes. No web
//! framework: blocking threads on loopback are enough (DESIGN §2).
//!
//! ## Security model
//! Loopback is not a user boundary on a shared host, and a browser on this
//! machine will happily send a page from any website to `127.0.0.1`. So:
//! * **Bind**: [`Board::bind`] refuses any address that is not loopback; the CLI
//!   only ever passes `127.0.0.1`. Never `0.0.0.0`.
//! * **Host**: every request must name `127.0.0.1:<port>` or `localhost:<port>`.
//!   A DNS-rebinding page reaches this socket under its own hostname, so its
//!   `Host` gives it away. Checked first, before anything is revealed.
//! * **Origin**: when a request carries one it must be this listener's own
//!   origin; a foreign or `null` Origin is refused. This is the CSRF guard, and
//!   it covers every route.
//! * **Token**: a random secret, supplied by the caller. `/?token=…` trades it
//!   for a `HttpOnly; SameSite=Strict` cookie and redirects to a clean `/`;
//!   every other route needs that cookie. The token is the boundary.
//! * **Allowlist**: the only route that can change anything is `POST /api`,
//!   and it forwards only the methods in [`READ_METHODS`] and
//!   [`WRITE_METHODS`] — default deny, params included. A write must also carry
//!   the page's Origin and an `expected_rev`, and is refused outright under
//!   `--scope read` ([`Board::with_writes`]). Every other HTTP method on
//!   every route is a 405.
//! * **Page**: served with a CSP that allows no network except this origin, and
//!   one nonce'd inline script.
//!
//! The board is single-user and local. Behind a reverse proxy, authentication is
//! the proxy's job; there are no accounts, roles or multi-user sync.
//!
//! ## Routes
//! * `GET /` — the page (or the token hand-over, see above).
//! * `GET /events` — `text/event-stream`: a `daemon` event naming whether the
//!   daemon is reachable, then one `task.changed` per daemon push, with a
//!   heartbeat comment so a dead peer is noticed.
//! * `POST /api` — one JSON API envelope in, the daemon's envelope out, for the
//!   listed methods only; a write goes with `actor: board`, so the events table
//!   tells a drag from a CLI call or an agent.

use std::io::{self, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::daemon::{self, Frame};
use crate::http::{
    prepare_stream, read_http_request, response_head, write_response, DeadlineReader, HttpError,
    HttpRequest, REQUEST_DEADLINE,
};

/// The API methods `POST /api` forwards. Default deny: a method not listed here
/// or in [`WRITE_METHODS`] is refused, so a method added to the API later is
/// unreachable from the board until someone lists it on purpose.
pub const READ_METHODS: &[&str] = &["task.list", "task.get", "project.list"];

/// The writes a drag sends (#638), each with the only params it may carry.
/// Closed twice over: an unlisted method is a 403 and an unlisted param a 400,
/// so `task.done {force}` or `task.start {keep}` cannot ride in on a listed
/// method. Every one requires `ref` and an integer `expected_rev`, and goes
/// to the daemon with the envelope's `actor` set to [`ACTOR`].
pub const WRITE_METHODS: &[(&str, &[&str])] = &[
    ("task.start", &["ref", "expected_rev"]),
    ("task.stop", &["ref", "expected_rev"]),
    ("task.done", &["ref", "expected_rev"]),
    ("task.cancel", &["ref", "expected_rev"]),
    ("task.reopen", &["ref", "expected_rev"]),
    ("task.modify", &["ref", "expected_rev", "set"]),
    // The card panel (#639): one check, one note, one edge, one dated field.
    ("check.set", &["ref", "expected_rev", "check_id", "state"]),
    ("annotation.add", &["ref", "expected_rev", "body"]),
    ("dependency.add", &["ref", "expected_rev", "depends_on"]),
    ("dependency.remove", &["ref", "expected_rev", "depends_on"]),
    // Undo: the core refuses unless the newest event is this task's, at this rev.
    ("event.revert", &["ref", "expected_rev"]),
];

/// The fields a board `task.modify` may `set`: the priority tray, the
/// Backlog/Ready drops (`wait`, `scheduled`) and the panel's `due` and
/// `estimate`. Titles, tags and project moves stay with the CLI.
pub const SET_FIELDS: &[&str] = &["priority", "wait", "scheduled", "due", "estimate"];

/// The `actor` the events table records for every board write.
pub const ACTOR: &str = "board";

/// Largest `POST /api` body. An envelope is a few hundred bytes.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Open connections served at once (SSE streams included). Past this a new
/// peer gets a 503, so a runaway tab cannot exhaust threads.
const MAX_CONNECTIONS: usize = 64;

const ACCEPT_STEP: Duration = Duration::from_millis(50);
/// How often an idle event stream writes a heartbeat comment.
const HEARTBEAT: Duration = Duration::from_secs(15);
/// How often a stream wakes to look at the shutdown flag.
const STREAM_TICK: Duration = Duration::from_millis(500);
/// Events a slow stream may fall behind by before it is dropped (it reconnects
/// and the page refetches, so nothing is lost).
const STREAM_BACKLOG: usize = 64;
const PUMP_RETRY_INITIAL: Duration = Duration::from_millis(200);
const PUMP_RETRY_MAX: Duration = Duration::from_secs(10);

/// What the caller decides; the listener enforces.
pub struct BoardOptions {
    /// The daemon's socket address (events and API calls go through it).
    pub socket: String,
    /// The secret the browser must present. The caller makes it random.
    pub token: String,
    /// Per-run nonce for the page's one inline script; the CSP allows only it.
    pub nonce: String,
    /// The page served at `/`.
    pub page: String,
}

/// State every connection thread reads.
struct Shared {
    opts: BoardOptions,
    port: u16,
    cookie_name: String,
    hub: Hub,
    connections: AtomicUsize,
    shutdown: Arc<AtomicBool>,
    /// [`Board::with_writes`]; false (the default) is `--scope read`.
    writes: bool,
}

/// Fan-out of daemon pushes to open event streams.
struct Hub {
    subscribers: Mutex<Vec<SyncSender<String>>>,
    online: AtomicBool,
}

impl Hub {
    fn broadcast(&self, frame: &str) {
        let mut subs = self.subscribers.lock().unwrap_or_else(|p| p.into_inner());
        // A full or closed channel is a stream that fell behind or left.
        subs.retain(|s| s.try_send(frame.to_string()).is_ok());
    }

    fn set_online(&self, online: bool) {
        if self.online.swap(online, Ordering::SeqCst) != online {
            self.broadcast(&sse("daemon", &json!({ "online": online }).to_string()));
        }
    }
}

/// One server-sent event.
fn sse(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// A bound, not yet serving, board listener.
pub struct Board {
    listener: TcpListener,
    shared: Arc<Shared>,
}

impl Board {
    /// Bind `ip:port` (port 0 asks the OS for a free one). Refuses an address
    /// that is not loopback, so the board can never be reached off this machine.
    pub fn bind(
        ip: IpAddr,
        port: u16,
        opts: BoardOptions,
        shutdown: Arc<AtomicBool>,
    ) -> io::Result<Board> {
        if !ip.is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the board binds loopback only, not {ip}"),
            ));
        }
        if opts.token.len() < 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the board token is too short to be a secret",
            ));
        }
        let listener = TcpListener::bind(SocketAddr::new(ip, port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Ok(Board {
            listener,
            shared: Arc::new(Shared {
                opts,
                port,
                cookie_name: format!("tasqx_board_{port}"),
                hub: Hub {
                    subscribers: Mutex::new(Vec::new()),
                    online: AtomicBool::new(false),
                },
                connections: AtomicUsize::new(0),
                shutdown,
                writes: false,
            }),
        })
    }

    /// `--scope write` (#638): forward [`WRITE_METHODS`]. A bound board is
    /// read-only until this says otherwise, so a caller that never asks for
    /// writes keeps phase 1's board: every write refused here, whatever the
    /// page sends. A builder rather than a `BoardOptions` field, which would
    /// break every caller that builds the options by literal.
    pub fn with_writes(mut self, on: bool) -> Board {
        Arc::get_mut(&mut self.shared)
            .expect("only `serve` shares the state, and it consumes the board")
            .writes = on;
        self
    }

    /// The port actually bound.
    pub fn port(&self) -> u16 {
        self.shared.port
    }

    /// The address a browser opens: the token rides in the query once, and the
    /// first response trades it for a cookie.
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/?token={}",
            self.shared.port, self.shared.opts.token
        )
    }

    /// Accept until the shutdown flag given to [`Board::bind`] is set. Blocks.
    /// A thread follows the daemon's pushes; each connection gets its own.
    pub fn serve(self) {
        // ponytail: the pump and any open stream are detached and notice
        // shutdown on their next wake; the process exits right after, so a
        // join would only slow Ctrl-C. Join them if board ever embeds in a
        // long-lived process.
        let shared = self.shared.clone();
        let shutdown = shared.shutdown.clone();
        {
            let shared = shared.clone();
            thread::spawn(move || pump(&shared));
        }
        while !shutdown.load(Ordering::Relaxed) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if shared.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                        shared.connections.fetch_sub(1, Ordering::SeqCst);
                        prepare_stream(&stream);
                        respond(&stream, &shared, 503, "text/plain", b"too many connections");
                        continue;
                    }
                    let shared = shared.clone();
                    thread::spawn(move || {
                        handle(stream, &shared);
                        shared.connections.fetch_sub(1, Ordering::SeqCst);
                    });
                }
                Err(_) => thread::sleep(ACCEPT_STEP),
            }
        }
    }
}

/// Follow the daemon: subscribe, forward each push to the streams, and when the
/// connection drops say so and retry with backoff.
fn pump(shared: &Shared) {
    let mut delay = PUMP_RETRY_INITIAL;
    while !shared.shutdown.load(Ordering::Relaxed) {
        let subscribed = daemon::try_connect(&shared.opts.socket).and_then(|mut c| {
            c.subscribe().ok()?;
            Some(c)
        });
        let Some(mut conn) = subscribed else {
            shared.hub.set_online(false);
            thread::sleep(delay);
            delay = (delay * 2).min(PUMP_RETRY_MAX);
            continue;
        };
        delay = PUMP_RETRY_INITIAL;
        shared.hub.set_online(true);
        loop {
            match conn.next_frame() {
                Ok(Some(Frame::Event(evt))) => {
                    shared.hub.broadcast(&sse("task.changed", &evt.to_string()))
                }
                Ok(Some(Frame::Response(_))) => {}
                Ok(None) | Err(_) => break,
            }
            if shared.shutdown.load(Ordering::Relaxed) {
                return;
            }
        }
        shared.hub.set_online(false);
    }
}

/// Headers every response carries. The CSP is what makes "no external fetch"
/// a browser-enforced fact rather than a promise about the page.
fn security_headers(shared: &Shared) -> Vec<(String, String)> {
    vec![
        (
            "Content-Security-Policy".into(),
            format!(
                "default-src 'none'; script-src 'nonce-{}'; style-src 'unsafe-inline'; \
                 connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
                 frame-ancestors 'none'",
                shared.opts.nonce
            ),
        ),
        ("Cache-Control".into(), "no-store".into()),
        ("X-Content-Type-Options".into(), "nosniff".into()),
        ("Referrer-Policy".into(), "no-referrer".into()),
        ("Cross-Origin-Resource-Policy".into(), "same-origin".into()),
    ]
}

fn respond(stream: &TcpStream, shared: &Shared, status: u16, ctype: &str, body: &[u8]) {
    respond_with(stream, shared, status, ctype, &[], body);
}

fn respond_with(
    stream: &TcpStream,
    shared: &Shared,
    status: u16,
    ctype: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) {
    let owned = security_headers(shared);
    let mut headers: Vec<(&str, &str)> = owned
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    headers.push(("Content-Type", ctype));
    headers.extend_from_slice(extra);
    write_response(stream, status, &headers, body);
}

fn envelope_error(code: &str, message: &str) -> Vec<u8> {
    json!({
        "tasqx": crate::API_VERSION, "ok": false,
        "error": { "code": code, "message": message },
    })
    .to_string()
    .into_bytes()
}

/// Compare two secrets without stopping at the first differing byte.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// `host` is `127.0.0.1:<port>` or `localhost:<port>`.
fn host_ok(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}") || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
}

/// `origin` is this listener's own.
fn origin_ok(origin: &str, port: u16) -> bool {
    origin == format!("http://127.0.0.1:{port}")
        || origin.eq_ignore_ascii_case(&format!("http://localhost:{port}"))
}

/// The value of cookie `name` in a `Cookie` header.
fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

/// The `token` parameter of a query string.
fn query_token(query: &str) -> Option<&str> {
    query.split('&').find_map(|kv| kv.strip_prefix("token="))
}

fn handle(stream: TcpStream, shared: &Shared) {
    prepare_stream(&stream);
    let parsed = {
        let mut reader = BufReader::new(DeadlineReader::new(&stream, REQUEST_DEADLINE));
        read_http_request(&mut reader, MAX_BODY_BYTES, &["GET", "POST"])
    };
    let req = match parsed {
        Ok(r) => r,
        Err(HttpError::MethodNotAllowed) => {
            return respond_with(
                &stream,
                shared,
                405,
                "text/plain",
                &[("Allow", "GET, POST")],
                b"method not allowed",
            );
        }
        Err(HttpError::PayloadTooLarge) => {
            return respond(&stream, shared, 413, "text/plain", b"payload too large");
        }
        Err(HttpError::BadRequest) => {
            return respond(&stream, shared, 400, "text/plain", b"bad request");
        }
        Err(HttpError::Io) => return,
    };

    // DNS rebinding: the page reaches this socket under its own hostname.
    if !req.header("host").is_some_and(|h| host_ok(h, shared.port)) {
        return respond(&stream, shared, 403, "text/plain", b"forbidden host");
    }
    // CSRF: a browser names the page that sent it. Absent is a non-browser
    // client (the token still gates it); present must be this origin.
    if req
        .header("origin")
        .is_some_and(|o| !origin_ok(o, shared.port))
    {
        return respond(&stream, shared, 403, "text/plain", b"forbidden origin");
    }

    let (path, query) = req.path.split_once('?').unwrap_or((&req.path, ""));

    // The one unauthenticated door: trade the token for a cookie.
    if path == "/" && req.method == "GET" {
        if let Some(t) = query_token(query) {
            if same_secret(t, &shared.opts.token) {
                let set = format!(
                    "{}={}; Path=/; HttpOnly; SameSite=Strict",
                    shared.cookie_name, shared.opts.token
                );
                return respond_with(
                    &stream,
                    shared,
                    302,
                    "text/plain",
                    &[("Location", "/"), ("Set-Cookie", &set)],
                    b"",
                );
            }
            return respond(&stream, shared, 401, "text/plain", b"unauthorized");
        }
    }
    if !authorized(&req, shared) {
        return respond(
            &stream,
            shared,
            401,
            "text/plain",
            b"unauthorized: open the URL `tasqx board` printed",
        );
    }

    match (req.method.as_str(), path) {
        ("GET", "/") => respond(
            &stream,
            shared,
            200,
            "text/html; charset=utf-8",
            shared.opts.page.as_bytes(),
        ),
        ("GET", "/events") => stream_events(stream, shared),
        ("POST", "/api") => api(&stream, shared, &req),
        (_, "/" | "/events" | "/api") => respond_with(
            &stream,
            shared,
            405,
            "text/plain",
            &[("Allow", if path == "/api" { "POST" } else { "GET" })],
            b"method not allowed",
        ),
        _ => respond(&stream, shared, 404, "text/plain", b"not found"),
    }
}

fn authorized(req: &HttpRequest, shared: &Shared) -> bool {
    req.header("cookie")
        .and_then(|c| cookie(c, &shared.cookie_name))
        .is_some_and(|t| same_secret(t, &shared.opts.token))
}

/// Forward one listed envelope to the daemon and hand its answer back.
fn api(stream: &TcpStream, shared: &Shared, req: &HttpRequest) {
    let json_type = "application/json";
    let Ok(env) = serde_json::from_slice::<Value>(&req.body) else {
        return respond(
            stream,
            shared,
            400,
            json_type,
            &envelope_error("bad_request", "the body is not JSON"),
        );
    };
    let (Some(method), params) = (
        env.get("method").and_then(Value::as_str),
        env.get("params").cloned().unwrap_or_else(|| json!({})),
    ) else {
        return respond(
            stream,
            shared,
            400,
            json_type,
            &envelope_error("bad_request", "the envelope has no `method`"),
        );
    };
    let write = WRITE_METHODS.iter().find(|(m, _)| *m == method);
    let refuse = |status, msg: &str| {
        respond(
            stream,
            shared,
            status,
            json_type,
            &envelope_error("bad_request", msg),
        )
    };
    match write {
        None if !READ_METHODS.contains(&method) => {
            return refuse(403, &format!("the board does not serve `{method}`"));
        }
        None => {}
        Some(_) if !shared.writes => {
            return refuse(
                403,
                &format!("this board is read-only (--scope read): `{method}` is refused"),
            );
        }
        // A write needs the page's own Origin (checked equal above): absent is
        // not this page in a browser, and a write is no place to guess.
        Some(_) if req.header("origin").is_none() => {
            return refuse(
                403,
                "a write must come from the board's own page (no Origin)",
            );
        }
        Some((_, allowed)) => {
            if let Err(msg) = write_params_ok(&params, allowed) {
                return refuse(400, &format!("`{method}`: {msg}"));
            }
        }
    }
    let actor = write.map(|_| ACTOR);
    let answer = daemon::try_connect(&shared.opts.socket)
        .ok_or("no daemon reachable".to_string())
        .and_then(|mut c| {
            c.request_as(actor, method, &params)
                .map_err(|e| e.to_string())
        });
    match answer {
        Ok(env) => respond(stream, shared, 200, json_type, env.to_string().as_bytes()),
        Err(e) => respond(
            stream,
            shared,
            503,
            json_type,
            &envelope_error("internal", &format!("daemon unavailable: {e}")),
        ),
    }
}

/// A board write's params: only the keys its [`WRITE_METHODS`] row lists, a
/// `ref`, an integer `expected_rev`, and a `set` (when sent) naming only
/// [`SET_FIELDS`].
fn write_params_ok(params: &Value, allowed: &[&str]) -> Result<(), String> {
    let obj = params.as_object().ok_or("`params` must be an object")?;
    if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("the board does not send `{k}`"));
    }
    if obj.get("ref").is_none_or(Value::is_null) {
        return Err("a board write names its task in `ref`".into());
    }
    if !obj.get("expected_rev").is_some_and(Value::is_i64) {
        return Err("a board write carries the card's `_rev` as an integer `expected_rev`".into());
    }
    if allowed.contains(&"set") {
        let set = obj
            .get("set")
            .and_then(Value::as_object)
            .filter(|s| !s.is_empty())
            .ok_or("`set` must name at least one field")?;
        if let Some(k) = set.keys().find(|k| !SET_FIELDS.contains(&k.as_str())) {
            return Err(format!("the board does not set `{k}`"));
        }
    }
    Ok(())
}

/// Hold the connection open and write server-sent events until the peer leaves
/// or the board stops.
fn stream_events(mut stream: TcpStream, shared: &Shared) {
    let (tx, rx) = mpsc::sync_channel::<String>(STREAM_BACKLOG);
    // Subscribe before reading `online`, so a flip in between is not missed.
    shared
        .hub
        .subscribers
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(tx);
    let owned = security_headers(shared);
    let mut headers: Vec<(&str, &str)> = owned
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    headers.push(("Content-Type", "text/event-stream"));
    let online = shared.hub.online.load(Ordering::SeqCst);
    let mut first = response_head(200, &headers);
    first.push_str("retry: 3000\n\n");
    first.push_str(&sse("daemon", &json!({ "online": online }).to_string()));
    if stream.write_all(first.as_bytes()).is_err() || stream.flush().is_err() {
        return;
    }
    // A stream is long-lived: the request deadline no longer applies.
    let mut quiet = Duration::ZERO;
    while !shared.shutdown.load(Ordering::Relaxed) {
        let out = match rx.recv_timeout(STREAM_TICK) {
            Ok(frame) => {
                quiet = Duration::ZERO;
                frame
            }
            Err(RecvTimeoutError::Timeout) => {
                quiet += STREAM_TICK;
                if quiet < HEARTBEAT {
                    continue;
                }
                quiet = Duration::ZERO;
                ": keepalive\n\n".to_string()
            }
            // Dropped by `broadcast` for falling behind: the page reconnects.
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if stream.write_all(out.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
    }
}
