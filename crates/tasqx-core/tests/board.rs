//! The board listener (#637, #638): its security boundary first, then the read
//! path, the live stream and the writes a drag sends.
//!
//! Each test binds a real [`Board`] on an ephemeral loopback port in front of a
//! real daemon on a temporary socket, and speaks raw HTTP to it — the same bytes
//! a browser, or an attacker's page, would send.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use tasqx_core::board::{Board, BoardOptions, READ_METHODS, SET_FIELDS, WRITE_METHODS};
use tasqx_core::daemon;
use tasqx_core::notify::LogNotifier;
use tasqx_core::{Engine, PARAMS};

const TOKEN: &str = "t0ken-0123456789abcdef";
const PAGE: &str = "<!doctype html><title>board-test-page</title>";

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_socket() -> (String, String) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = format!("tasqx-board-test-{}-{n}", std::process::id());
    let db = std::env::temp_dir().join(format!("{stem}.db"));
    let sock = if cfg!(windows) {
        stem
    } else {
        std::env::temp_dir()
            .join(format!("{stem}.sock"))
            .to_string_lossy()
            .into_owned()
    };
    (db.to_string_lossy().into_owned(), sock)
}

fn start_daemon(db: &str, sock: &str) {
    let (db, sk) = (db.to_string(), sock.to_string());
    thread::spawn(move || {
        let options = daemon::DaemonOptions {
            notifier: Arc::new(LogNotifier),
            tokens_enabled: false,
            otlp_port: None,
            idle_timeout: None,
            retired_marker: None,
        };
        let engine = Engine::open(&db).expect("open engine");
        daemon::serve_with_options(engine, &sk, Arc::new(AtomicBool::new(false)), options)
            .expect("serve");
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if daemon::try_connect(sock).is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("daemon never became connectable at {sock}");
}

struct Rig {
    port: u16,
    socket: String,
    shutdown: Arc<AtomicBool>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// A board in front of a live daemon, writes enabled (`--scope write`).
fn rig() -> Rig {
    let (db, socket) = unique_socket();
    start_daemon(&db, &socket);
    board_on(socket, true)
}

/// A `--scope read` board in front of a live daemon.
fn read_rig() -> Rig {
    let (db, socket) = unique_socket();
    start_daemon(&db, &socket);
    board_on(socket, false)
}

/// A board in front of whatever `socket` names (possibly nothing).
fn board_on(socket: String, writes: bool) -> Rig {
    let shutdown = Arc::new(AtomicBool::new(false));
    let board = Board::bind(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        BoardOptions {
            socket: socket.clone(),
            token: TOKEN.into(),
            nonce: "nonce123".into(),
            page: PAGE.into(),
        },
        shutdown.clone(),
    )
    .expect("bind loopback")
    .with_writes(writes);
    let port = board.port();
    thread::spawn(move || board.serve());
    Rig {
        port,
        socket,
        shutdown,
    }
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }
}

/// One request, one reply (`Connection: close`).
fn send(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (n, v) in headers {
        req.push_str(&format!("{n}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Reply {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

fn cookie(port: u16) -> String {
    format!("tasqx_board_{port}={TOKEN}")
}

fn api(port: u16, envelope: &Value) -> Reply {
    send(
        port,
        "POST",
        "/api",
        &[
            ("Cookie", &cookie(port)),
            ("Content-Type", "application/json"),
        ],
        &envelope.to_string(),
    )
}

/// A write as the page sends it: the cookie and the board's own Origin.
fn write(port: u16, method: &str, params: Value) -> Reply {
    write_from(
        port,
        Some(&format!("http://127.0.0.1:{port}")),
        method,
        params,
    )
}

fn write_from(port: u16, origin: Option<&str>, method: &str, params: Value) -> Reply {
    let c = cookie(port);
    let mut headers = vec![("Cookie", c.as_str()), ("Content-Type", "application/json")];
    if let Some(o) = origin {
        headers.push(("Origin", o));
    }
    let env = json!({ "tasqx": "1", "id": "1", "method": method, "params": params });
    send(port, "POST", "/api", &headers, &env.to_string())
}

fn envelope(rep: &Reply) -> Value {
    serde_json::from_str(&rep.body).unwrap_or_else(|_| panic!("not JSON: {}", rep.body))
}

/// Every event in the store, oldest first, as the daemon reports them.
fn events(socket: &str) -> Vec<Value> {
    let mut c = daemon::try_connect(socket).expect("daemon");
    let env = c.request("event.list", &json!({ "limit": 1000 })).unwrap();
    let mut all = env["result"]["events"].as_array().unwrap().clone();
    all.reverse(); // event.list answers newest first
    all
}

fn add(socket: &str, title: &str) -> (i64, i64) {
    let mut c = daemon::try_connect(socket).expect("daemon");
    let t = c.request("task.add", &json!({ "title": title })).unwrap();
    let sid = t["result"]["short_id"].as_i64().unwrap();
    (sid, task(socket, sid)["_rev"].as_i64().unwrap())
}

fn task(socket: &str, sid: i64) -> Value {
    let mut c = daemon::try_connect(socket).expect("daemon");
    c.request("task.get", &json!({ "ref": sid })).unwrap()["result"].clone()
}

// ---- security: the boundary, before anything else ---------------------------

#[test]
fn a_non_loopback_bind_is_refused() {
    for ip in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
    ] {
        let err = Board::bind(
            ip,
            0,
            BoardOptions {
                socket: "x".into(),
                token: TOKEN.into(),
                nonce: "n".into(),
                page: PAGE.into(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .err()
        .unwrap_or_else(|| panic!("{ip} must be refused"));
        assert!(err.to_string().contains("loopback"), "{err}");
    }
}

#[test]
fn a_short_token_is_refused_at_bind() {
    let err = Board::bind(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        BoardOptions {
            socket: "x".into(),
            token: "short".into(),
            nonce: "n".into(),
            page: PAGE.into(),
        },
        Arc::new(AtomicBool::new(false)),
    )
    .err()
    .expect("a guessable token is no boundary");
    assert!(err.to_string().contains("token"), "{err}");
}

#[test]
fn a_missing_or_wrong_token_is_refused_on_every_route() {
    let r = rig();
    let wrong = format!("tasqx_board_{}=not-the-token-at-all!", r.port);
    for (method, path) in [("GET", "/"), ("GET", "/events"), ("POST", "/api")] {
        let none = send(r.port, method, path, &[], "{}");
        assert_eq!(none.status, 401, "{method} {path} with no cookie");
        let bad = send(r.port, method, path, &[("Cookie", &wrong)], "{}");
        assert_eq!(bad.status, 401, "{method} {path} with a wrong cookie");
        assert!(!bad.body.contains("board-test-page"), "{}", bad.body);
    }
    let bad_query = send(r.port, "GET", "/?token=wrong-wrong-wrong-wrong", &[], "");
    assert_eq!(bad_query.status, 401, "a wrong ?token= is no way in");
    assert!(bad_query.header("set-cookie").is_none());
}

#[test]
fn the_right_token_is_traded_for_a_strict_httponly_cookie() {
    let r = rig();
    let hand = send(r.port, "GET", &format!("/?token={TOKEN}"), &[], "");
    assert_eq!(hand.status, 302);
    assert_eq!(
        hand.header("location"),
        Some("/"),
        "the token leaves the URL"
    );
    let set = hand.header("set-cookie").expect("a cookie");
    assert!(set.starts_with(&cookie(r.port)), "{set}");
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Strict"),
        "{set}"
    );

    let page = send(r.port, "GET", "/", &[("Cookie", &cookie(r.port))], "");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("board-test-page"));
    let csp = page.header("content-security-policy").expect("a CSP");
    assert!(
        csp.contains("default-src 'none'") && csp.contains("nonce-nonce123"),
        "{csp}"
    );
    assert!(
        !csp.contains("http"),
        "the CSP names no remote origin: {csp}"
    );
    assert_eq!(page.header("cache-control"), Some("no-store"));
}

#[test]
fn a_foreign_origin_or_host_is_refused_even_with_the_token() {
    let r = rig();
    let c = cookie(r.port);
    for origin in [
        "http://evil.example",
        "null",
        "http://127.0.0.1:1",
        "https://localhost",
    ] {
        for (method, path) in [("GET", "/"), ("GET", "/events"), ("POST", "/api")] {
            let rep = send(
                r.port,
                method,
                path,
                &[("Cookie", &c), ("Origin", origin)],
                r#"{"method":"task.list"}"#,
            );
            assert_eq!(rep.status, 403, "{method} {path} from {origin}");
        }
    }
    // DNS rebinding: the attacker's hostname resolves to 127.0.0.1.
    let rebound = send(
        r.port,
        "GET",
        "/",
        &[
            ("Host", &format!("rebind.evil.example:{}", r.port)),
            ("Cookie", &c),
        ],
        "",
    );
    assert_eq!(rebound.status, 403, "a foreign Host is a rebinding page");
    let own = send(
        r.port,
        "GET",
        "/",
        &[
            ("Host", &format!("localhost:{}", r.port)),
            ("Origin", &format!("http://localhost:{}", r.port)),
            ("Cookie", &c),
        ],
        "",
    );
    assert_eq!(own.status, 200, "the board's own origin passes");
}

#[test]
fn no_write_method_is_reachable() {
    let r = rig();
    let c = cookie(r.port);
    // HTTP verbs that mean "change something".
    for method in ["PUT", "DELETE", "PATCH"] {
        for path in ["/", "/api", "/events"] {
            let rep = send(r.port, method, path, &[("Cookie", &c)], "{}");
            assert_eq!(rep.status, 405, "{method} {path}");
        }
    }
    assert_eq!(
        send(r.port, "GET", "/api", &[("Cookie", &c)], "").status,
        405
    );
    assert_eq!(
        send(r.port, "POST", "/", &[("Cookie", &c)], "{}").status,
        405
    );
}

/// The write allowlist is closed: every API method that is neither a listed
/// read nor a listed write is refused, from the board's own origin, with the
/// cookie, with a well-formed `ref` and `expected_rev` — and none runs.
#[test]
fn a_write_method_outside_the_allowlist_is_refused() {
    let r = rig();
    let (sid, rev) = add(&r.socket, "target");
    let before = events(&r.socket).len();
    for (method, _, _) in PARAMS {
        if READ_METHODS.contains(method) || WRITE_METHODS.iter().any(|(m, _)| m == method) {
            continue;
        }
        let rep = write(
            r.port,
            method,
            json!({ "ref": sid, "expected_rev": rev, "title": "smuggled" }),
        );
        assert_eq!(rep.status, 403, "{method} must be refused: {}", rep.body);
        assert_eq!(envelope(&rep)["ok"], json!(false), "{method}");
    }
    assert_eq!(events(&r.socket).len(), before, "the store did not change");
}

/// Inside the allowlist, a param or a `set` field the board never sends is
/// refused, so `task.done {force}` or `task.modify {set:{title}}` cannot ride
/// in on a listed method.
#[test]
fn a_param_outside_the_allowlist_is_refused() {
    let r = rig();
    let (sid, rev) = add(&r.socket, "target");
    let before = events(&r.socket).len();
    for (method, params) in [
        (
            "task.done",
            json!({ "ref": sid, "expected_rev": rev, "force": true }),
        ),
        (
            "task.start",
            json!({ "ref": sid, "expected_rev": rev, "keep": true }),
        ),
        (
            "task.start",
            json!({ "ref": sid, "expected_rev": rev, "actor": "mcp:x" }),
        ),
        (
            "task.modify",
            json!({ "ref": sid, "expected_rev": rev, "set": { "title": "x" } }),
        ),
        (
            "task.modify",
            json!({ "ref": sid, "expected_rev": rev, "set": {} }),
        ),
        ("task.modify", json!({ "ref": sid, "expected_rev": rev })),
        ("task.stop", json!([sid, rev])),
        // The panel's writes (#639): no evidence, position, tombstone or title.
        (
            "check.set",
            json!({ "ref": sid, "expected_rev": rev, "check_id": "c", "state": "passed", "evidence": "x" }),
        ),
        (
            "annotation.add",
            json!({ "ref": sid, "expected_rev": rev, "body": "b", "created": "x" }),
        ),
        (
            "dependency.add",
            json!({ "ref": sid, "expected_rev": rev, "depends_on": 1, "force": true }),
        ),
        (
            "task.modify",
            json!({ "ref": sid, "expected_rev": rev, "set": { "tags": ["x"] } }),
        ),
    ] {
        let rep = write(r.port, method, params.clone());
        assert_eq!(rep.status, 400, "{method} {params}: {}", rep.body);
        assert_eq!(envelope(&rep)["error"]["code"], "bad_request");
    }
    assert_eq!(events(&r.socket).len(), before, "the store did not change");
}

#[test]
fn a_write_without_expected_rev_is_refused() {
    let r = rig();
    let (sid, _) = add(&r.socket, "target");
    let before = events(&r.socket).len();
    for (method, _) in WRITE_METHODS {
        for params in [
            json!({ "ref": sid, "set": { "priority": "H" } }),
            json!({ "ref": sid, "expected_rev": null, "set": { "priority": "H" } }),
            json!({ "ref": sid, "expected_rev": "2", "set": { "priority": "H" } }),
            json!({ "expected_rev": 2, "set": { "priority": "H" } }),
        ] {
            let mut params = params;
            if *method != "task.modify" {
                params.as_object_mut().unwrap().remove("set");
            }
            let rep = write(r.port, method, params.clone());
            assert_eq!(rep.status, 400, "{method} {params}: {}", rep.body);
            assert_eq!(envelope(&rep)["error"]["code"], "bad_request");
        }
    }
    assert_eq!(events(&r.socket).len(), before, "the store did not change");
}

/// A write needs the board's own Origin: a foreign one is a CSRF page, and a
/// missing one is not a browser on this page at all (reads still allow it).
#[test]
fn a_write_from_a_foreign_or_missing_origin_is_refused() {
    let r = rig();
    let (sid, rev) = add(&r.socket, "target");
    let before = events(&r.socket).len();
    for origin in [
        Some("http://evil.example"),
        Some("null"),
        Some("http://127.0.0.1:1"),
        None,
    ] {
        let rep = write_from(
            r.port,
            origin,
            "task.start",
            json!({ "ref": sid, "expected_rev": rev }),
        );
        assert_eq!(rep.status, 403, "{origin:?}: {}", rep.body);
    }
    assert_eq!(events(&r.socket).len(), before, "the store did not change");
    assert_eq!(task(&r.socket, sid)["status"], "pending");
}

/// `--scope read` refuses every write server-side, however well-formed, and
/// still serves the reads.
#[test]
fn read_scope_refuses_every_write() {
    let r = read_rig();
    let (sid, rev) = add(&r.socket, "target");
    let before = events(&r.socket).len();
    for (method, _, _) in PARAMS {
        if READ_METHODS.contains(method) {
            continue;
        }
        let rep = write(
            r.port,
            method,
            json!({ "ref": sid, "expected_rev": rev, "set": { "priority": "H" } }),
        );
        assert_eq!(rep.status, 403, "{method}: {}", rep.body);
        if WRITE_METHODS.iter().any(|(m, _)| m == method) {
            assert!(rep.body.contains("--scope read"), "{method}: {}", rep.body);
        }
    }
    assert_eq!(events(&r.socket).len(), before, "the store did not change");
    let rep = write(r.port, "task.get", json!({ "ref": sid }));
    assert_eq!(rep.status, 200, "{}", rep.body);
}

#[test]
fn every_listed_method_and_param_is_a_real_api_one() {
    for m in READ_METHODS {
        assert!(PARAMS.iter().any(|(name, _, _)| name == m), "{m}");
    }
    for (m, keys) in WRITE_METHODS {
        let (_, api_keys, _) = PARAMS
            .iter()
            .find(|(name, _, _)| name == m)
            .unwrap_or_else(|| panic!("{m} is not an API method"));
        for k in *keys {
            assert!(api_keys.contains(k), "{m} takes no `{k}`");
        }
        assert!(
            keys.contains(&"expected_rev"),
            "{m} must carry expected_rev"
        );
    }
    assert_eq!(
        SET_FIELDS,
        ["priority", "wait", "scheduled", "due", "estimate"]
    );
}

// ---- function: the read path and the live stream -----------------------------

#[test]
fn a_read_envelope_is_forwarded_and_answered() {
    let r = rig();
    let mut c = daemon::try_connect(&r.socket).unwrap();
    c.request("task.add", &json!({ "title": "visible on the board" }))
        .unwrap();
    let rep = api(
        r.port,
        &json!({ "tasqx": "1", "id": "1", "method": "task.list", "params": {} }),
    );
    assert_eq!(rep.status, 200, "{}", rep.body);
    let env: Value = serde_json::from_str(&rep.body).unwrap();
    assert_eq!(env["ok"], json!(true));
    assert_eq!(env["result"]["tasks"][0]["title"], "visible on the board");
}

#[test]
fn a_malformed_envelope_is_a_400_not_a_forward() {
    let r = rig();
    let c = cookie(r.port);
    for body in ["not json", "{}", r#"{"method":7}"#] {
        let rep = send(r.port, "POST", "/api", &[("Cookie", &c)], body);
        assert_eq!(rep.status, 400, "{body}: {}", rep.body);
    }
}

#[test]
fn a_missing_daemon_is_a_503_envelope() {
    let (_, socket) = unique_socket();
    let r = board_on(socket, true);
    let rep = api(
        r.port,
        &json!({ "tasqx": "1", "id": "1", "method": "task.list", "params": {} }),
    );
    assert_eq!(rep.status, 503, "{}", rep.body);
    let env: Value = serde_json::from_str(&rep.body).unwrap();
    assert_eq!(env["ok"], json!(false));
}

/// Read SSE lines from `/events` on a thread; each blank-line-terminated event
/// arrives as one string.
fn open_stream(port: u16) -> mpsc::Receiver<String> {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    s.write_all(
        format!(
            "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {}\r\n\r\n",
            cookie(port)
        )
        .as_bytes(),
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(s);
        let mut event = String::new();
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if line == "\r\n" && event.is_empty() {
                // end of the response head
            } else if line == "\n" {
                if tx.send(std::mem::take(&mut event)).is_err() {
                    return;
                }
            } else {
                event.push_str(&line);
            }
            line.clear();
        }
    });
    rx
}

fn wait_for(rx: &mpsc::Receiver<String>, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ev) if ev.contains(needle) => return ev,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    panic!("no event containing {needle:?} within the deadline");
}

#[test]
fn a_task_change_shows_up_on_the_event_stream() {
    let r = rig();
    let rx = open_stream(r.port);
    wait_for(&rx, r#""online":true"#);

    let mut c = daemon::try_connect(&r.socket).unwrap();
    c.request("task.add", &json!({ "title": "appears live" }))
        .unwrap();
    let ev = wait_for(&rx, "event: task.changed");
    assert!(ev.contains("\"op\":\"add\""), "{ev}");
}

// ---- function: the writes a drag sends (#638) --------------------------------

/// Each drop's envelope lands, moves the task, and is attributed to the board
/// in the events table.
#[test]
fn each_drop_lands_and_is_attributed_to_the_board() {
    let r = rig();
    let (sid, _) = add(&r.socket, "dragged");
    let (other, _) = add(&r.socket, "running");
    {
        let mut c = daemon::try_connect(&r.socket).unwrap();
        c.request("task.start", &json!({ "ref": other })).unwrap();
    }
    let at = |s: &Rig| task(&s.socket, sid)["_rev"].as_i64().unwrap();
    let before = events(&r.socket).len();

    // Ready -> Active: start, displacing the running task (D6).
    let rep = write(
        r.port,
        "task.start",
        json!({ "ref": sid, "expected_rev": at(&r) }),
    );
    let env = envelope(&rep);
    assert_eq!(env["ok"], json!(true), "{env}");
    assert_eq!(env["result"]["auto_stopped"][0]["short_id"], json!(other));
    // Active -> Ready: stop.
    let env = envelope(&write(
        r.port,
        "task.stop",
        json!({ "ref": sid, "expected_rev": at(&r) }),
    ));
    assert_eq!(env["result"]["status"], "pending", "{env}");
    // Ready -> Backlog: wait a week.
    let env = envelope(&write(
        r.port,
        "task.modify",
        json!({ "ref": sid, "expected_rev": at(&r), "set": { "wait": "+1w" } }),
    ));
    assert_eq!(env["ok"], json!(true), "{env}");
    assert_eq!(task(&r.socket, sid)["status"], "backlog");
    // Backlog -> Ready: clear wait and scheduled.
    let env = envelope(&write(
        r.port,
        "task.modify",
        json!({ "ref": sid, "expected_rev": at(&r), "set": { "wait": null, "scheduled": null } }),
    ));
    assert_eq!(env["ok"], json!(true), "{env}");
    assert_eq!(task(&r.socket, sid)["status"], "pending");
    // Priority tray: H, then clear.
    for p in [json!("H"), Value::Null] {
        let env = envelope(&write(
            r.port,
            "task.modify",
            json!({ "ref": sid, "expected_rev": at(&r), "set": { "priority": p } }),
        ));
        assert_eq!(env["ok"], json!(true), "{env}");
        assert_eq!(task(&r.socket, sid)["priority"], p);
    }
    // Ready -> Done, Done -> Ready (reopen), then cancel from the tray.
    let env = envelope(&write(
        r.port,
        "task.done",
        json!({ "ref": sid, "expected_rev": at(&r) }),
    ));
    assert_eq!(env["ok"], json!(true), "{env}");
    let env = envelope(&write(
        r.port,
        "task.reopen",
        json!({ "ref": sid, "expected_rev": at(&r) }),
    ));
    assert_eq!(env["ok"], json!(true), "{env}");
    let env = envelope(&write(
        r.port,
        "task.cancel",
        json!({ "ref": sid, "expected_rev": at(&r) }),
    ));
    assert_eq!(env["result"]["status"], "cancelled", "{env}");

    let written = &events(&r.socket)[before..];
    assert!(written.len() >= 9, "{written:?}");
    for ev in written {
        assert_eq!(ev["actor"], "board", "{ev}");
    }
    assert!(
        events(&r.socket)[..before]
            .iter()
            .all(|e| e["actor"] == "user"),
        "the daemon's own callers stay `user`"
    );
}

/// A stale `expected_rev` — somebody changed the card since it was drawn — is
/// the core's `conflict`, carrying the current rev for the toast, and the
/// other session's change stands.
#[test]
fn a_stale_rev_is_a_conflict_not_an_overwrite() {
    let r = rig();
    let (sid, rev) = add(&r.socket, "contested");
    {
        let mut c = daemon::try_connect(&r.socket).unwrap();
        c.request(
            "task.modify",
            &json!({ "ref": sid, "set": { "priority": "L" } }),
        )
        .unwrap();
    }
    for (method, params) in [
        ("task.start", json!({ "ref": sid, "expected_rev": rev })),
        (
            "task.modify",
            json!({ "ref": sid, "expected_rev": rev, "set": { "priority": "H" } }),
        ),
    ] {
        let env = envelope(&write(r.port, method, params));
        assert_eq!(env["ok"], json!(false), "{method}: {env}");
        assert_eq!(env["error"]["code"], "conflict", "{method}");
        assert_eq!(env["error"]["data"]["current"], json!(rev + 1), "{method}");
    }
    let now = task(&r.socket, sid);
    assert_eq!(
        (now["status"].as_str(), now["priority"].as_str()),
        (Some("pending"), Some("L"))
    );
}

/// Undo takes back the board's own last write, and refuses once anything else
/// has been written since.
#[test]
fn undo_takes_back_the_last_drop_and_nothing_later() {
    let r = rig();
    let (sid, rev) = add(&r.socket, "undo me");
    let env = envelope(&write(
        r.port,
        "task.done",
        json!({ "ref": sid, "expected_rev": rev }),
    ));
    assert_eq!(env["ok"], json!(true), "{env}");
    let env = envelope(&write(
        r.port,
        "event.revert",
        json!({ "ref": sid, "expected_rev": rev + 1 }),
    ));
    assert_eq!(env["result"]["reverted"]["op"], "done", "{env}");
    assert_eq!(task(&r.socket, sid)["status"], "pending");
    assert_eq!(events(&r.socket).last().unwrap()["actor"], "board");

    let at = task(&r.socket, sid)["_rev"].as_i64().unwrap();
    let env = envelope(&write(
        r.port,
        "task.modify",
        json!({ "ref": sid, "expected_rev": at, "set": { "priority": "H" } }),
    ));
    assert_eq!(env["ok"], json!(true), "{env}");
    add(&r.socket, "somebody else's later write");
    let env = envelope(&write(
        r.port,
        "event.revert",
        json!({ "ref": sid, "expected_rev": at + 1 }),
    ));
    assert_eq!(env["error"]["code"], "conflict", "{env}");
    assert_eq!(task(&r.socket, sid)["priority"], "H", "nothing was undone");
}

/// A caller that never calls `with_writes` keeps phase 1's read-only board.
#[test]
fn a_board_is_read_only_until_writes_are_asked_for() {
    let shutdown = Arc::new(AtomicBool::new(false));
    let board = Board::bind(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        BoardOptions {
            socket: unique_socket().1,
            token: TOKEN.into(),
            nonce: "n".into(),
            page: PAGE.into(),
        },
        shutdown.clone(),
    )
    .expect("bind loopback");
    let port = board.port();
    thread::spawn(move || board.serve());
    let rep = write(port, "task.start", json!({ "ref": 1, "expected_rev": 1 }));
    assert_eq!(rep.status, 403, "{}", rep.body);
    assert!(rep.body.contains("--scope read"), "{}", rep.body);
    shutdown.store(true, Ordering::SeqCst);
}

// ---- function: the card panel's writes (#639) --------------------------------

/// Each panel control lands through the closed allowlist, carries the card's
/// rev, is attributed to the board, and leaves the live rev one higher so the
/// panel footer can show it. A note is then found by memory search, and a
/// dependency change is reflected in the answer the page re-derives Blocked from.
#[test]
fn each_panel_edit_lands_attributed_and_a_note_is_searchable() {
    let r = rig();
    let (sid, _) = add(&r.socket, "edited in the panel");
    let (blocker, _) = add(&r.socket, "the blocker");
    let check = {
        let mut c = daemon::try_connect(&r.socket).unwrap();
        let out = c
            .request("check.add", &json!({ "ref": sid, "body": "it works" }))
            .unwrap();
        out["result"]["check"]["id"].clone()
    };
    let at = || task(&r.socket, sid)["_rev"].as_i64().unwrap();
    let before = events(&r.socket).len();
    let send = |method: &str, params: Value| {
        let rev = at();
        let mut p = params;
        p["ref"] = json!(sid);
        p["expected_rev"] = json!(rev);
        let env = envelope(&write(r.port, method, p));
        assert_eq!(env["ok"], json!(true), "{method}: {env}");
        assert_eq!(at(), rev + 1, "{method} leaves the rev one higher");
        env["result"].clone()
    };

    let out = send("check.set", json!({ "check_id": check, "state": "passed" }));
    assert_eq!(out["state"], "passed");
    assert_eq!(task(&r.socket, sid)["checks"][0]["state"], "passed");
    send("check.set", json!({ "check_id": check, "state": "open" }));
    send(
        "task.modify",
        json!({ "set": { "due": "2031-01-02", "estimate": "2h", "wait": "2031-01-01", "scheduled": "2031-01-01", "priority": "M" } }),
    );
    let t = task(&r.socket, sid);
    assert_eq!(t["estimate"], "PT2H");
    assert!(t["due"].as_str().unwrap().starts_with("2031-01-02"), "{t}");
    let out = send("dependency.add", json!({ "depends_on": blocker }));
    assert_eq!(out["blocked"], json!(true));
    assert_eq!(out["depends_on"], json!([blocker]));
    let out = send("dependency.remove", json!({ "depends_on": blocker }));
    assert_eq!(out["blocked"], json!(false));
    send(
        "annotation.add",
        json!({ "body": "the zeroth quokka lives in crates/tasqx-core/src/board.rs" }),
    );

    let written = &events(&r.socket)[before..];
    assert!(written.len() >= 6, "{written:?}");
    for ev in written {
        assert_eq!(ev["actor"], "board", "{ev}");
    }
    let mut c = daemon::try_connect(&r.socket).unwrap();
    let hits = c
        .request("memory.search", &json!({ "query": "zeroth quokka" }))
        .unwrap();
    let hits = hits["result"]["hits"].as_array().unwrap().clone();
    assert!(
        hits.iter().any(|h| h.to_string().contains("task:#")),
        "a note added on the board is found by memory search: {hits:?}"
    );
}

/// A stale rev on any panel write is a `conflict` carrying the current rev, and
/// the other session's change stands.
#[test]
fn a_stale_rev_refuses_every_panel_write() {
    let r = rig();
    let (sid, rev) = add(&r.socket, "contested");
    let (blocker, _) = add(&r.socket, "blocker");
    let check = {
        let mut c = daemon::try_connect(&r.socket).unwrap();
        c.request("check.add", &json!({ "ref": sid, "body": "c" }))
            .unwrap()["result"]["check"]["id"]
            .clone()
    };
    // check.add moved the rev; `rev` is now stale for everything below.
    let now = task(&r.socket, sid)["_rev"].as_i64().unwrap();
    assert!(now > rev);
    for (method, params) in [
        ("check.set", json!({ "check_id": check, "state": "passed" })),
        ("annotation.add", json!({ "body": "lost" })),
        ("dependency.add", json!({ "depends_on": blocker })),
        ("dependency.remove", json!({ "depends_on": blocker })),
        ("task.modify", json!({ "set": { "due": "2031-01-02" } })),
        ("task.modify", json!({ "set": { "estimate": "1h" } })),
    ] {
        let mut p = params;
        p["ref"] = json!(sid);
        p["expected_rev"] = json!(rev);
        let env = envelope(&write(r.port, method, p));
        assert_eq!(env["error"]["code"], "conflict", "{method}: {env}");
        assert_eq!(env["error"]["data"]["current"], json!(now), "{method}");
    }
    let t = task(&r.socket, sid);
    assert_eq!(t["_rev"], json!(now), "nothing was written");
    assert!(t["due"].is_null());
}

/// The page counts on `rev + 1` after each of its writes (`act` keeps the
/// panel's `_rev`, and Undo sends it): every panel write moves the rev by
/// exactly one, and Undo at that rev succeeds for the undoable ones. Editing
/// the day of a due date that has a time keeps the time.
#[test]
fn panel_writes_bump_the_rev_by_one_and_undo_at_that_rev() {
    let r = rig();
    let (sid, _) = add(&r.socket, "revs");
    let (blocker, _) = add(&r.socket, "blocker");
    let check = {
        let mut c = daemon::try_connect(&r.socket).unwrap();
        c.request("check.add", &json!({ "ref": sid, "body": "c" }))
            .unwrap()["result"]["check"]["id"]
            .clone()
    };
    let at = || task(&r.socket, sid)["_rev"].as_i64().unwrap();
    let step = |method: &str, params: Value| -> i64 {
        let rev = at();
        let mut p = params;
        p["ref"] = json!(sid);
        p["expected_rev"] = json!(rev);
        let env = envelope(&write(r.port, method, p));
        assert_eq!(env["ok"], json!(true), "{method}: {env}");
        assert_eq!(at(), rev + 1, "{method} bumps the rev by exactly one");
        rev + 1
    };
    let undo = |rev: i64, op: &str| {
        let env = envelope(&write(
            r.port,
            "event.revert",
            json!({ "ref": sid, "expected_rev": rev }),
        ));
        assert_eq!(env["ok"], json!(true), "undo {op}: {env}");
    };
    step("check.set", json!({ "check_id": check, "state": "passed" }));
    let rev = step("annotation.add", json!({ "body": "undo me" }));
    undo(rev, "annotation.add");
    step("dependency.add", json!({ "depends_on": blocker }));
    let rev = step("dependency.remove", json!({ "depends_on": blocker }));
    undo(rev, "dependency.remove");
    assert_eq!(task(&r.socket, sid)["depends_on"], json!([blocker]));

    // The time of day survives a date edit made the way the page makes it.
    step(
        "task.modify",
        json!({ "set": { "due": "2031-01-02T17:00:00Z" } }),
    );
    let rev = step(
        "task.modify",
        json!({ "set": { "due": "2031-01-05T17:00:00Z" } }),
    );
    assert_eq!(task(&r.socket, sid)["due"], "2031-01-05T17:00:00Z");
    undo(rev, "task.modify");
    assert_eq!(task(&r.socket, sid)["due"], "2031-01-02T17:00:00Z");
}
