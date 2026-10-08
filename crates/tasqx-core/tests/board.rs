//! The board listener (#637): its security boundary first, then the live path.
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

use tasqx_core::board::{Board, BoardOptions, READ_METHODS};
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

/// A board in front of a live daemon.
fn rig() -> Rig {
    let (db, socket) = unique_socket();
    start_daemon(&db, &socket);
    board_on(socket)
}

/// A board in front of whatever `socket` names (possibly nothing).
fn board_on(socket: String) -> Rig {
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
    .expect("bind loopback");
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

fn task_count(socket: &str) -> i64 {
    let mut c = daemon::try_connect(socket).expect("daemon");
    let env = c.request("task.list", &json!({ "limit": 1 })).unwrap();
    env["result"]["total"].as_i64().unwrap()
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

    // Every API method that is not a listed read is refused, and none runs.
    let before = task_count(&r.socket);
    for (method, _, _) in PARAMS {
        if READ_METHODS.contains(method) {
            continue;
        }
        let rep = api(
            r.port,
            &json!({ "tasqx": "1", "id": "1", "method": method, "params": { "title": "smuggled" } }),
        );
        assert_eq!(rep.status, 403, "{method} must be refused: {}", rep.body);
        let env: Value = serde_json::from_str(&rep.body).unwrap();
        assert_eq!(env["ok"], json!(false), "{method}");
    }
    assert_eq!(task_count(&r.socket), before, "the store did not change");
}

#[test]
fn every_listed_read_method_is_a_real_api_method() {
    for m in READ_METHODS {
        assert!(PARAMS.iter().any(|(name, _, _)| name == m), "{m}");
    }
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
    let r = board_on(socket);
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
