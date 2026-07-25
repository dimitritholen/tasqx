//! Daemon transport integration tests (DESIGN.md §2, §6a).
//!
//! Each test starts a real daemon on a temporary socket in a background thread
//! and drives it with the real `daemon::Conn` client — exercising the actual
//! newline-delimited envelope transport, not a mock. Event-wait paths use a
//! reader thread + `recv_timeout` so a regression fails fast instead of hanging.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use tasqx_core::daemon::{self, Frame};
use tasqx_core::notify::{LogNotifier, Notification, Notifier};
use tasqx_core::{dispatch, Engine};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique (db path, socket address) pair, isolated per test.
fn unique_target() -> (String, String) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let stem = format!("tasqx-test-{pid}-{n}-{nanos}");
    let db = std::env::temp_dir().join(format!("{stem}.db"));
    let sock = if cfg!(windows) {
        // A bare, safe name → mapped to \\.\pipe\<name> by the transport.
        stem
    } else {
        std::env::temp_dir()
            .join(format!("{stem}.sock"))
            .to_string_lossy()
            .into_owned()
    };
    (db.to_string_lossy().into_owned(), sock)
}

/// A notifier that records every delivery, so a test can assert on reminder
/// delivery with no OS notification transport anywhere in the picture (§9).
#[derive(Default)]
struct Collecting(Mutex<Vec<Notification>>);

impl Notifier for Collecting {
    fn notify(&self, n: &Notification) {
        self.0.lock().unwrap().push(n.clone());
    }
}

impl Collecting {
    fn titles(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|n| n.title.clone())
            .collect()
    }
}

/// Start a daemon in a background thread; returns the shutdown flag once the
/// socket is connectable (bounded wait).
fn start_daemon(db: &str, sock: &str) -> Arc<AtomicBool> {
    start_daemon_with_notifier(db, sock, Arc::new(LogNotifier))
}

/// [`start_daemon`], with the reminder notifier injected.
fn start_daemon_with_notifier(
    db: &str,
    sock: &str,
    notifier: Arc<dyn Notifier>,
) -> Arc<AtomicBool> {
    start_daemon_with_options(db, sock, notifier, false)
}

/// [`start_daemon`], with the notifier and the `[tokens] enabled` opt-in
/// injected (#17): the seam the attribution integration tests use to turn the
/// third background thread on or off.
fn start_daemon_with_options(
    db: &str,
    sock: &str,
    notifier: Arc<dyn Notifier>,
    tokens_enabled: bool,
) -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let sd = shutdown.clone();
    let db = db.to_string();
    let sk = sock.to_string();
    thread::spawn(move || {
        let engine = Engine::open(&db).expect("open engine");
        let options = daemon::DaemonOptions {
            notifier,
            tokens_enabled,
            otlp_port: None,
        };
        daemon::serve_with_options(engine, &sk, sd, options).expect("serve");
    });
    // Wait until the listener is up. Healthy runs connect on the first or
    // second try, so the deadline costs nothing when things work — but it used
    // to be a hard 2s (200 × 10ms), which the coverage job's instrumented
    // build on a busy CI runner overran on 2026-07-21 with the daemon code
    // untouched. A generous wall-clock budget makes "slow" and "broken"
    // distinguishable; only "broken" should be red.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Some(c) = daemon::try_connect(sock) {
            drop(c);
            return shutdown;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("daemon never became connectable at {sock} within 20s");
}

/// [`start_daemon_with_options`], with `serve`'s result handed back over a
/// channel instead of `expect`ed inside a detached thread.
///
/// A supervised background fault does not panic: `report_fatal` sends on the
/// `fatal` channel and the accept loop turns that into an `Err` *return* from
/// `serve` naming the component. A daemon started with `.expect("serve")` throws
/// that return value away in a thread nobody joins, so such a test can only
/// notice that the daemon vanished — never which of the four supervised
/// components died, which is the whole contract. Every fatal-path test needs
/// this variant.
fn start_daemon_observing_failure(
    db: &str,
    sock: &str,
    tokens_enabled: bool,
) -> (
    Arc<AtomicBool>,
    mpsc::Receiver<io::Result<()>>,
    thread::JoinHandle<()>,
) {
    let shutdown = Arc::new(AtomicBool::new(false));
    let sd = shutdown.clone();
    let owned_db = db.to_string();
    let sk = sock.to_string();
    let (result_tx, result_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let engine = Engine::open(&owned_db).expect("open server engine");
        let options = daemon::DaemonOptions {
            notifier: Arc::new(LogNotifier),
            tokens_enabled,
            otlp_port: None,
        };
        let result = daemon::serve_with_options(engine, &sk, sd, options);
        let _ = result_tx.send(result);
    });
    // Same generous connect budget as `start_daemon_with_options`, for the same
    // reason: an instrumented coverage build on a busy runner is slow, not broken.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Some(c) = daemon::try_connect(sock) {
            drop(c);
            return (shutdown, result_rx, server);
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("daemon never became connectable at {sock} within 20s");
}

/// Wait for `serve` to return, assert it returned a *failure*, and yield the
/// message. Shared by the supervision tests so the shutdown/join bookkeeping is
/// written once.
fn fatal_message(
    result_rx: &mpsc::Receiver<io::Result<()>>,
    shutdown: &Arc<AtomicBool>,
    server: thread::JoinHandle<()>,
) -> String {
    let observed = result_rx.recv_timeout(Duration::from_secs(10));
    // Flagged only AFTER the recv: setting it first would let a daemon that
    // ignored the fault still return `Ok`, and the test would read that as a
    // pass. On the timeout path it is the only way to reap the thread.
    shutdown.store(true, Ordering::Relaxed);
    if observed.is_err() {
        let _ = result_rx.recv_timeout(Duration::from_secs(2));
    }
    server.join().expect("server thread");
    observed
        .expect("daemon stayed alive after a supervised background component failed")
        .expect_err("daemon reported a clean shutdown after a supervised component failed")
        .to_string()
}

fn ok(env: &Value) -> &Value {
    assert_eq!(
        env.get("ok"),
        Some(&Value::Bool(true)),
        "expected ok envelope, got {env}"
    );
    env.get("result").expect("result field")
}

#[test]
fn transport_round_trip_initialize_add_list() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);
    let mut c = daemon::try_connect(&sock).expect("connect");

    // capabilities == the "initialize" handshake for this transport.
    let caps = c.request("core.capabilities", &json!({})).unwrap();
    assert_eq!(
        caps.get("id"),
        Some(&json!(1)),
        "response correlates request id"
    );
    let r = ok(&caps);
    assert_eq!(r.get("api"), Some(&json!("1")));

    let added = c
        .request("task.add", &json!({ "title": "ship the daemon" }))
        .unwrap();
    assert_eq!(added.get("id"), Some(&json!(2)));
    let short_id = ok(&added).get("short_id").and_then(Value::as_i64).unwrap();
    assert!(short_id >= 1);

    let listed = c
        .request("task.list", &json!({ "filter": "status:pending" }))
        .unwrap();
    assert_eq!(listed.get("id"), Some(&json!(3)));
    let tasks = ok(&listed).get("tasks").and_then(Value::as_array).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].get("title"), Some(&json!("ship the daemon")));

    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn excess_clients_are_refused_without_disrupting_admitted_clients() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);
    let mut clients = Vec::with_capacity(daemon::MAX_CONCURRENT_CLIENTS);

    for index in 0..daemon::MAX_CONCURRENT_CLIENTS {
        let mut client = daemon::try_connect(&sock).expect("connect admitted client");
        let response = client.request("core.capabilities", &json!({})).unwrap();
        assert_eq!(response["ok"], true, "admitted client {index}");
        clients.push(client);
    }

    for _ in 0..8 {
        let mut rejected =
            daemon::try_connect(&sock).expect("overload is an accepted-then-refused stream");
        let error = rejected
            .request("core.capabilities", &json!({}))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
        assert!(
            error.to_string().contains("client limit"),
            "observable overload: {error}"
        );
    }

    let response = clients[0].request("core.capabilities", &json!({})).unwrap();
    assert_eq!(
        response["ok"], true,
        "an existing client must remain usable under overload"
    );

    drop(clients);
    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

#[cfg(unix)]
#[test]
fn unix_socket_is_owner_only_even_for_a_custom_path() {
    use std::os::unix::fs::PermissionsExt;

    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);
    let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_reply_larger_than_one_socket_buffer_arrives_whole() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);
    let mut c = daemon::try_connect(&sock).expect("connect");

    // 64 tasks × 200-char titles ≈ 14 KiB of `task.list` reply — past the 8 KiB
    // default Unix-socket buffer on macOS (net.local.stream.sendspace), so the
    // reply only arrives whole if the daemon writes across a buffer-full
    // boundary. An accepted stream accidentally left nonblocking (BSD listeners
    // bequeath O_NONBLOCK; Linux does not) fails here on the first WouldBlock:
    // the v0.2.0 release run died with EOF at column 8192 after the 15-minute
    // idle timeout finally closed the half-written connection.
    let title = "t".repeat(200);
    for _ in 0..64 {
        let added = c.request("task.add", &json!({ "title": title })).unwrap();
        assert_eq!(added.get("ok"), Some(&Value::Bool(true)));
    }
    let listed = c
        .request(
            "task.list",
            &json!({ "filter": "status:pending", "limit": 1000 }),
        )
        .unwrap();
    let tasks = ok(&listed).get("tasks").and_then(Value::as_array).unwrap();
    assert_eq!(tasks.len(), 64, "every task survives a multi-buffer reply");

    drop(c);
    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn two_clients_concurrent_no_deadlock_and_ids_match() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);

    let mut handles = Vec::new();
    for client in 0..2u64 {
        let sock = sock.clone();
        handles.push(thread::spawn(move || {
            let mut c = daemon::try_connect(&sock).expect("connect");
            for i in 0..25u64 {
                // A mix of a write and a read per iteration.
                let add = c
                    .request("task.add", &json!({ "title": format!("c{client}-t{i}") }))
                    .unwrap();
                // The response id must equal the request id this client sent.
                let sent_add_id = 2 * i + 1; // ids: 1,3,5,... (add), 2,4,6,... (list)
                assert_eq!(
                    add.get("id"),
                    Some(&json!(sent_add_id)),
                    "add id correlates"
                );
                assert_eq!(add.get("ok"), Some(&Value::Bool(true)));

                let list = c
                    .request("task.list", &json!({ "filter": "status:pending" }))
                    .unwrap();
                let sent_list_id = 2 * i + 2;
                assert_eq!(
                    list.get("id"),
                    Some(&json!(sent_list_id)),
                    "list id correlates"
                );
                assert_eq!(list.get("ok"), Some(&Value::Bool(true)));
            }
        }));
    }
    for h in handles {
        h.join()
            .expect("client thread panicked (deadlock/corruption?)");
    }

    // Both clients committed 25 adds each → 50 pending tasks, no lost writes.
    let mut c = daemon::try_connect(&sock).expect("connect");
    let listed = c
        .request(
            "task.list",
            &json!({ "filter": "status:pending", "limit": 1000 }),
        )
        .unwrap();
    let n = ok(&listed)
        .get("tasks")
        .and_then(Value::as_array)
        .unwrap()
        .len();
    assert_eq!(n, 50, "all concurrent writes landed exactly once");

    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn subscriber_receives_push_after_in_daemon_mutation() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);

    // Subscriber connection, drained on its own thread with a timeout.
    let mut sub = daemon::try_connect(&sock).expect("connect sub");
    sub.subscribe().expect("subscribe");
    let (tx, rx) = mpsc::channel::<Value>();
    thread::spawn(move || {
        while let Ok(Some(frame)) = sub.next_frame() {
            if let Frame::Event(v) = frame {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });

    // A second connection applies a mutation *through the daemon*.
    let mut writer = daemon::try_connect(&sock).expect("connect writer");
    let added = writer
        .request("task.add", &json!({ "title": "live update me" }))
        .unwrap();
    let short_id = ok(&added).get("short_id").and_then(Value::as_i64).unwrap();

    let evt = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("no task.changed push arrived");
    assert_eq!(evt.get("event"), Some(&json!("task.changed")));
    let data = evt.get("data").unwrap();
    assert_eq!(
        data.get("op"),
        Some(&json!("add")),
        "push carries the event op"
    );
    assert_eq!(
        data.get("short_id"),
        Some(&json!(short_id)),
        "push carries the changed short_id"
    );

    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn subscriber_receives_push_from_external_write() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);

    let mut sub = daemon::try_connect(&sock).expect("connect sub");
    sub.subscribe().expect("subscribe");
    let (tx, rx) = mpsc::channel::<Value>();
    thread::spawn(move || {
        while let Ok(Some(frame)) = sub.next_frame() {
            if let Frame::Event(v) = frame {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });

    // A SEPARATE Engine on the same DB file writes directly — no daemon
    // involved. The poller must detect the new event row and push it.
    let external = Engine::open(&db).expect("second engine on same db");
    let res = dispatch(
        &external,
        "task.add",
        &json!({ "title": "written out-of-band" }),
    )
    .unwrap();
    let short_id = res.get("short_id").and_then(Value::as_i64).unwrap();
    drop(external);

    let evt = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("external write not detected");
    assert_eq!(evt.get("event"), Some(&json!("task.changed")));
    assert_eq!(
        evt.get("data").and_then(|d| d.get("short_id")),
        Some(&json!(short_id))
    );

    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

// ---- §9: reminders over the daemon ------------------------------------------

/// Subscribe and return a receiver of every event push (drained off-thread).
fn subscribe_events(sock: &str) -> mpsc::Receiver<Value> {
    let mut sub = daemon::try_connect(sock).expect("connect sub");
    sub.subscribe().expect("subscribe");
    let (tx, rx) = mpsc::channel::<Value>();
    thread::spawn(move || {
        while let Ok(Some(frame)) = sub.next_frame() {
            if let Frame::Event(v) = frame {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    rx
}

/// Wait for a `task.changed` push carrying `op`, ignoring the others.
fn wait_for_op(rx: &mpsc::Receiver<Value>, op: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !left.is_zero(),
            "no {op:?} push arrived within the deadline"
        );
        match rx.recv_timeout(left) {
            Ok(v) => {
                if v["data"]["op"] == json!(op) {
                    return v;
                }
            }
            Err(_) => panic!("no {op:?} push arrived within the deadline"),
        }
    }
}

/// The headless verification surface (§9): a ripe reminder must reach a `watch`
/// subscriber as an ordinary event push, with no OS notification involved.
///
/// Time is not slept for — the reminder is set to an instant already long past,
/// so it is ripe the moment the scheduler sees it. The only wait is on the push
/// itself, with a timeout, exactly as the other daemon tests do.
#[test]
fn subscriber_receives_a_reminded_push_when_a_reminder_ripens() {
    let (db, sock) = unique_target();
    let collector: Arc<Collecting> = Arc::new(Collecting::default());
    let shutdown = start_daemon_with_notifier(&db, &sock, collector.clone());
    let rx = subscribe_events(&sock);

    let mut writer = daemon::try_connect(&sock).expect("connect writer");
    // An absolute reminder in the past => ripe immediately, deterministically.
    let added = writer
        .request(
            "task.add",
            &json!({ "title": "ripe now", "remind": "2020-01-01T00:00:00Z" }),
        )
        .unwrap();
    let short_id = ok(&added).get("short_id").and_then(Value::as_i64).unwrap();

    let evt = wait_for_op(&rx, "reminded");
    assert_eq!(evt.get("event"), Some(&json!("task.changed")));
    assert_eq!(
        evt["data"]["short_id"],
        json!(short_id),
        "the push names the reminded task"
    );

    // The event log is the durable record behind that push.
    let evts = writer
        .request("event.list", &json!({ "ref": short_id, "limit": 100 }))
        .unwrap();
    let reminded: Vec<&Value> = ok(&evts)["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["op"] == json!("reminded"))
        .collect();
    assert_eq!(reminded.len(), 1, "exactly one reminded event");
    assert_eq!(reminded[0]["payload"]["at"], json!("2020-01-01T00:00:00Z"));

    // And the notifier ran — once, for this task.
    assert_eq!(collector.titles(), vec!["ripe now".to_string()]);

    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

/// Dedupe across a daemon **restart** (§9): a reminder already delivered must not
/// fire again when a fresh daemon rebuilds its heap from the same store.
///
/// The negative assertion is made deterministic with a barrier rather than a
/// sleep: a *second* task's reminder is fired by the restarted daemon, which
/// proves it completed a full scheduling pass — only then is the first task's
/// event count asserted.
///
/// The restart binds a *fresh* socket against the same store. Dedupe lives in
/// the store, not the transport, and reusing the address would only test how
/// fast Windows releases a named pipe whose connection threads are still
/// unwinding — a different (and irrelevant) race.
#[test]
fn a_restarted_daemon_does_not_refire_an_already_reminded_reminder() {
    let (db, sock) = unique_target();
    let (_, sock2) = unique_target();

    // ---- first run: the reminder ripens and fires.
    let first_collector: Arc<Collecting> = Arc::new(Collecting::default());
    let shutdown = start_daemon_with_notifier(&db, &sock, first_collector.clone());
    let rx = subscribe_events(&sock);
    let mut writer = daemon::try_connect(&sock).expect("connect writer");
    let added = writer
        .request(
            "task.add",
            &json!({ "title": "fire once", "remind": "2020-01-01T00:00:00Z" }),
        )
        .unwrap();
    let short_id = ok(&added).get("short_id").and_then(Value::as_i64).unwrap();
    wait_for_op(&rx, "reminded");
    assert_eq!(first_collector.titles(), vec!["fire once".to_string()]);
    drop(writer);
    shutdown.store(true, Ordering::Relaxed);

    // ---- restart against the SAME store.
    let second_collector: Arc<Collecting> = Arc::new(Collecting::default());
    let shutdown2 = start_daemon_with_notifier(&db, &sock2, second_collector.clone());
    let rx2 = subscribe_events(&sock2);
    let mut writer2 = daemon::try_connect(&sock2).expect("connect writer 2");

    // The barrier: a new ripe reminder the restarted daemon must deliver.
    writer2
        .request(
            "task.add",
            &json!({ "title": "barrier", "remind": "2020-01-02T00:00:00Z" }),
        )
        .unwrap();
    wait_for_op(&rx2, "reminded");

    // The restarted daemon has now demonstrably run a full scheduling pass.
    // The first task must still carry exactly one `reminded` event.
    let evts = writer2
        .request("event.list", &json!({ "ref": short_id, "limit": 100 }))
        .unwrap();
    let n = ok(&evts)["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["op"] == json!("reminded"))
        .count();
    assert_eq!(
        n, 1,
        "a restart must not re-fire an already-reminded reminder"
    );
    assert_eq!(
        second_collector.titles(),
        vec!["barrier".to_string()],
        "only the new reminder is delivered after the restart"
    );

    shutdown2.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn connect_to_absent_daemon_is_none_and_fast() {
    // No daemon here: the CLI's fallback decision must be immediate, never hang.
    let (_db, sock) = unique_target();
    let t0 = std::time::Instant::now();
    let got = daemon::try_connect(&sock);
    assert!(
        got.is_none(),
        "connecting to a nonexistent socket must yield None"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "fallback must be fast, took {:?}",
        t0.elapsed()
    );
}

#[test]
fn background_store_failure_stops_the_daemon_with_context() {
    let (db, sock) = unique_target();
    let (shutdown, result_rx, server) = start_daemon_observing_failure(&db, &sock, false);

    let breaker = Engine::open(&db).expect("open breaker connection");
    breaker
        .conn()
        .execute_batch("DROP TABLE events")
        .expect("damage event schema");
    drop(breaker);

    let message = fatal_message(&result_rx, &shutdown, server);
    // Deliberately an OR, not two tests. Losing `events` wholesale breaks the
    // poller's `pump` AND every read the reminder tick makes (`max_event_rowid`,
    // `reminded_keys` inside `rebuild`, its own `pump`); the two threads race for
    // the single-slot fatal channel and either may win. The isolation trick used
    // by the attribution test below — damage one *column* only one component
    // reads — has no counterpart here: poller and scheduler read the same
    // columns of the same two tables.
    assert!(
        message.contains("event poller") || message.contains("reminder scheduler"),
        "failure must identify the background component: {message}"
    );
    let _ = std::fs::remove_file(&db);
}

// ---- §10 / #17: async token attribution over the daemon ---------------------

/// A unique temp directory for a synthetic transcript, isolated per test.
fn unique_dir(label: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("tasqx-attr-it-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A Claude Code transcript with two in-window lines (110 input / 220 output)
/// and one far-future line every real window excludes.
fn transcript(in_window_ts: &str) -> String {
    [
        format!(
            r#"{{"timestamp":"{in_window_ts}","message":{{"id":"a","model":"claude-opus-4-8","usage":{{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
        ),
        format!(
            r#"{{"timestamp":"{in_window_ts}","message":{{"id":"b","model":"claude-opus-4-8","usage":{{"input_tokens":100,"output_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
        ),
        r#"{"timestamp":"2099-01-01T00:00:00Z","message":{"id":"c","usage":{"input_tokens":9999,"output_tokens":9999}}}"#.to_string(),
    ]
    .join("\n")
}

/// The headless verification surface for attribution: a correlated completion,
/// with `[tokens] enabled`, must reach a `watch` subscriber as an ordinary
/// `tokens.attributed` push and leave a stored `log-parse` measurement on the
/// task — with no OS transport and no client `expected_rev` bump.
#[test]
fn a_correlated_completion_yields_a_stored_measurement_and_a_push() {
    let (db, sock) = unique_target();
    let dir = unique_dir("push");
    // Claude Code names each transcript `<session-id>.jsonl`; naming the fixture
    // for the completion's session id makes this a *verified* correlation => HIGH.
    let path = dir.join("sess-1.jsonl");

    let shutdown = start_daemon_with_options(&db, &sock, Arc::new(LogNotifier), true);
    let rx = subscribe_events(&sock);
    let mut c = daemon::try_connect(&sock).expect("connect");

    let add = c
        .request("task.add", &json!({ "title": "ship it" }))
        .unwrap();
    let id = ok(&add)["short_id"].as_i64().unwrap();

    // An instant captured after creation and before completion is provably
    // inside the task's [created, completed] window.
    let in_window = tasqx_core::util::now();
    std::fs::write(&path, transcript(&in_window)).unwrap();

    let done = c
        .request(
            "task.done",
            &json!({
                "ref": id,
                "client": "claude-code",
                "session_id": "sess-1",
                "transcript_path": path.to_string_lossy(),
            }),
        )
        .unwrap();
    let rev_after_done = ok(&done);
    let _ = rev_after_done; // completion succeeds; attribution runs afterwards.

    // Async attribution lands on the event stream.
    let evt = wait_for_op(&rx, "tokens.attributed");
    assert_eq!(evt["data"]["short_id"], json!(id));

    // The measurement is stored and visible on task.get.
    let got = c.request("task.get", &json!({ "ref": id })).unwrap();
    let result = ok(&got);
    let tokens = result["tokens"].as_array().expect("tokens array");
    assert_eq!(tokens.len(), 1, "exactly one measurement, got {tokens:?}");
    assert_eq!(tokens[0]["source"], json!("log-parse"));
    assert_eq!(tokens[0]["input_tokens"], json!(110), "in-window only");
    assert_eq!(tokens[0]["output_tokens"], json!(220));
    assert_eq!(tokens[0]["confidence"], json!("high"));

    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// With the opt-in OFF (the default), the attribution thread is never spawned:
/// a correlated completion is left un-attributed.
///
/// The barrier is a **positive control**, not a sleep. This assertion is
/// negative — "nothing appeared" — so a stopwatch makes it pass for the wrong
/// reason the moment the runner is slower than the budget, and the coverage job
/// (`cargo llvm-cov --all-targets`) runs this very test under instrumentation.
/// Raising the number cannot fix that class: on a negative assertion a bigger
/// budget buys robustness, never soundness. So a *second* daemon with the opt-in
/// ON, on its OWN store and socket, completes the same shaped task; its
/// `tokens.attributed` push is the proof that a full attribution pass has
/// happened here, now, under this machine's actual load. The stores must stay
/// separate — sharing one would let the enabled daemon attribute the disabled
/// daemon's task and turn this red for a reason unrelated to the gate.
#[test]
fn attribution_does_not_run_when_the_opt_in_is_off() {
    let (db, sock) = unique_target();
    let (control_db, control_sock) = unique_target();
    let dir = unique_dir("off");
    // One fixture, read by both daemons: it is an input file, not shared state.
    let path = dir.join("session.jsonl");

    let shutdown = start_daemon(&db, &sock); // tokens disabled by default
    let control_shutdown =
        start_daemon_with_options(&control_db, &control_sock, Arc::new(LogNotifier), true);
    let rx = subscribe_events(&sock);
    let control_rx = subscribe_events(&control_sock);
    let mut c = daemon::try_connect(&sock).expect("connect");
    let mut cc = daemon::try_connect(&control_sock).expect("connect control");

    let add = c
        .request("task.add", &json!({ "title": "ship it" }))
        .unwrap();
    let id = ok(&add)["short_id"].as_i64().unwrap();
    let control_add = cc
        .request("task.add", &json!({ "title": "ship it" }))
        .unwrap();
    let control_id = ok(&control_add)["short_id"].as_i64().unwrap();

    // Captured after both creations and before both completions => provably
    // inside each task's [created, completed] window.
    let in_window = tasqx_core::util::now();
    std::fs::write(&path, transcript(&in_window)).unwrap();
    // The opt-in-off completion goes FIRST, so the control's attribution tick
    // cannot fire before this daemon has even seen its own `done` event.
    c.request(
        "task.done",
        &json!({
            "ref": id,
            "client": "claude-code",
            "transcript_path": path.to_string_lossy(),
        }),
    )
    .unwrap();
    cc.request(
        "task.done",
        &json!({
            "ref": control_id,
            "client": "claude-code",
            "transcript_path": path.to_string_lossy(),
        }),
    )
    .unwrap();

    // Barrier 1: this daemon has broadcast the completion, so the event is
    // committed to its store and its background loop is running.
    wait_for_op(&rx, "done");
    // Barrier 2: an attribution loop elsewhere has run a full pass over an
    // equivalent task since then.
    let attributed = wait_for_op(&control_rx, "tokens.attributed");
    assert_eq!(
        attributed["data"]["short_id"],
        json!(control_id),
        "the control's push must be for the control's own task"
    );

    let got = c.request("task.get", &json!({ "ref": id })).unwrap();
    let tokens = ok(&got)["tokens"].as_array().expect("tokens array");
    assert!(
        tokens.is_empty(),
        "attribution must stay off without the opt-in, got {tokens:?}"
    );

    shutdown.store(true, Ordering::Relaxed);
    control_shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&control_db);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fatal store fault inside the attribution thread must stop the daemon and
/// name **that** component — the supervision contract for the third background
/// thread, which no other test reaches (`serve`/`serve_with_notifier` leave
/// `tokens_enabled` false, so the thread is never even spawned).
///
/// Two things make this deterministic:
///
/// 1. **Column-level damage.** `pending_attributions` prepares
///    `SELECT entity_id, payload, ts, rowid FROM events`; the poller's `pump`
///    selects `rowid, entity, entity_id, op` and the reminder tick reads
///    `entity_id, payload` (`reminded_keys`) plus the `tasks` columns. So `ts`
///    is read by attribution and by nothing else in the background — removing it
///    kills exactly one thread, and at *prepare* time, so it cannot depend on
///    which rows exist. Dropping a whole table instead would take the poller
///    down first (it ticks every 400ms against attribution's 500ms) and the
///    assertion below would name the wrong component.
/// 2. **A pinned cursor.** A tick short-circuits when the event rowid has not
///    moved, and no further event can be appended once `events` is damaged. The
///    completion below therefore points at a transcript that will never exist:
///    that is the *transient* branch, which leaves the task in the pending set
///    and makes the tick return `-1`, so every subsequent tick rebuilds and
///    reaches the damaged query.
///
/// Not covered on purpose: the `event pump` site in the connection handler. Its
/// mutation is near-redundant — the poller runs the identical `pump` against the
/// same store and reports fatal within ~400ms of any fault the pump can hit.
#[test]
fn attribution_store_failure_stops_the_daemon_naming_token_attribution() {
    let (db, sock) = unique_target();
    let dir = unique_dir("fatal");
    // Deliberately never written: `compute_attribution` treats an absent explicit
    // transcript as transient (it lags the completion hook in real runs) and
    // retries for 24h, which is exactly the "stays pending" state we want.
    let never_flushed = dir.join("never-flushed.jsonl");

    let (shutdown, result_rx, server) = start_daemon_observing_failure(&db, &sock, true);
    let mut c = daemon::try_connect(&sock).expect("connect");
    let add = c
        .request("task.add", &json!({ "title": "pin the pending set" }))
        .unwrap();
    let id = ok(&add)["short_id"].as_i64().unwrap();
    c.request(
        "task.done",
        &json!({
            "ref": id,
            "client": "claude-code",
            "transcript_path": never_flushed.to_string_lossy(),
        }),
    )
    .unwrap();

    let breaker = Engine::open(&db).expect("open breaker connection");
    breaker
        .conn()
        .execute_batch("ALTER TABLE events DROP COLUMN ts")
        .expect("damage the column only attribution reads");
    drop(breaker);

    let message = fatal_message(&result_rx, &shutdown, server);
    assert!(
        message.contains("token attribution"),
        "the fatal must name the attribution component, got: {message}"
    );
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(&dir);
}
