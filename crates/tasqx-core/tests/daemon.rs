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
            idle_timeout: None,
            retired_marker: None,
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
    start_daemon_observing_result(
        db,
        sock,
        daemon::DaemonOptions {
            notifier: Arc::new(LogNotifier),
            tokens_enabled,
            otlp_port: None,
            idle_timeout: None,
            retired_marker: None,
        },
    )
}

/// The general form: serve with arbitrary options and hand `serve`'s return
/// value back over a channel.
///
/// Both callers need the return value for the same reason — a daemon that ends
/// on its own does so by RETURNING, whether the reason is a supervised
/// component failing (`Err`) or D5's idle timeout (`Ok`), and a detached thread
/// throws that away. Written once so the readiness wait, the flag and the join
/// handle do not exist twice with a chance to differ.
fn start_daemon_observing_result(
    db: &str,
    sock: &str,
    options: daemon::DaemonOptions,
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
    wait_for_op_within(rx, op, Duration::from_secs(5))
}

/// [`wait_for_op`] on a stated budget, for the waits where the push follows a
/// background loop rather than the call that was just made. Five seconds is ten
/// attribution ticks and twenty-five reminder ticks, which is ample on an idle
/// machine and not obviously ample under an instrumented coverage build on a
/// loaded runner — and a liveness budget that is too small does not make a test
/// stricter, it makes it a coin toss. Same reasoning, and same generosity, as
/// the connect deadline in `start_daemon_with_options`.
fn wait_for_op_within(rx: &mpsc::Receiver<Value>, op: &str, within: Duration) -> Value {
    let deadline = std::time::Instant::now() + within;
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

// ---- §12-D5: idle shutdown ---------------------------------------------------

/// Wait for `serve` to return, or say what it was still doing instead.
fn serve_result(
    result_rx: &mpsc::Receiver<io::Result<()>>,
    within: Duration,
) -> Option<io::Result<()>> {
    result_rx.recv_timeout(within).ok()
}

/// D5 end to end: a configured daemon with nobody connected leaves on its own,
/// and it leaves through the ordinary shutdown return rather than by dying.
///
/// The timeout here is 400 ms because the option is a `Duration`, not the
/// setting's minutes — the CLI does the minutes conversion, which is what keeps
/// this assertable in under a second instead of over fifteen.
#[test]
fn a_configured_daemon_with_no_clients_and_no_work_exits_on_its_own() {
    let (db, sock) = unique_target();
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            idle_timeout: Some(Duration::from_millis(400)),
            retired_marker: None,
            ..Default::default()
        },
    );

    let outcome = serve_result(&result_rx, Duration::from_secs(20));
    // Flagged only after the wait, exactly as `fatal_message` does it: setting
    // it first would let a daemon that ignored its idle timeout return anyway
    // and the test would read that as a pass.
    shutdown.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    let outcome = outcome.expect("an idle daemon must return from serve, not sit there");
    outcome.expect("idle shutdown is an ordinary stop, not a failure");
    assert!(
        daemon::try_connect(&sock).is_none(),
        "the socket must be gone once the daemon has left"
    );
    let _ = std::fs::remove_file(&db);
}

/// The other half, and the one that protects everybody who never configured
/// this: with no `idle_timeout`, a daemon nobody talks to stays.
///
/// A negative test with a deadline, so it is honest about what it proves — 2 s
/// of silence against a feature whose only "on" state in this suite fires in
/// 400 ms. A default that leaked D5's 15 minutes in would not fail here; a
/// default that leaked *any* timeout short enough to observe would.
#[test]
fn a_daemon_without_the_setting_stays_up_through_the_same_silence() {
    let (db, sock) = unique_target();
    let (shutdown, result_rx, server) =
        start_daemon_observing_result(&db, &sock, daemon::DaemonOptions::default());

    assert!(
        serve_result(&result_rx, Duration::from_secs(2)).is_none(),
        "an unconfigured daemon must not exit by itself"
    );
    assert!(
        daemon::try_connect(&sock).is_some(),
        "and must still be serving"
    );

    shutdown.store(true, Ordering::Relaxed);
    let stopped = serve_result(&result_rx, Duration::from_secs(20));
    server.join().expect("server thread");
    stopped
        .expect("Ctrl-C must still stop it")
        .expect("a flagged shutdown is a clean stop");
    let _ = std::fs::remove_file(&db);
}

/// The clock is the SERVER's, not the connection's: a client that connects and
/// says nothing for many times the idle timeout keeps the daemon alive, and the
/// countdown only starts when it goes away.
///
/// This is the distinction `CLIENT_IDLE_TIMEOUT` (15 min, per connection) and
/// D5's timeout would collapse into each other if the idle check consulted
/// anything but the admitted-client count.
#[test]
fn a_connected_client_holds_the_daemon_open_and_leaving_starts_the_countdown() {
    let (db, sock) = unique_target();
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            idle_timeout: Some(Duration::from_millis(400)),
            retired_marker: None,
            ..Default::default()
        },
    );

    let conn = daemon::try_connect(&sock).expect("connect before the deadline");
    assert!(
        serve_result(&result_rx, Duration::from_millis(1500)).is_none(),
        "a connected client — even a silent one — is a reason to stay"
    );

    drop(conn);
    let outcome = serve_result(&result_rx, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    outcome
        .expect("the countdown must start when the last client leaves")
        .expect("idle shutdown is an ordinary stop");
    let _ = std::fs::remove_file(&db);
}

/// A reminder about to ripen is work, and the accept loop can only know that if
/// the scheduler's soonest instant actually reaches it.
///
/// The whole path is here — `reminder_loop` republishing `peek_at()` after each
/// tick, and `server_busy` reading it — because none of it is observable from a
/// unit test: the mirror could be left unwritten and every predicate test would
/// still pass while a live daemon walked out one second before a delivery.
///
/// **The two outcomes differ in what happens, not in when.** The reminder leads
/// by 3 s against a 2 s idle timeout, and both numbers are constrained — from
/// opposite ends. The lead must EXCEED the timeout, or a daemon that cannot see
/// the reminder is still there when it ripens and delivers it anyway, which is
/// what the 1.8 s lead below got wrong. And the lead must stay under TWICE the
/// timeout, because the horizon `server_busy` judges against is the timeout
/// itself: the reminder only enters that horizon `lead - timeout` after the
/// client leaves, while the idle clock expires `timeout` after the same instant.
/// At 3 s against 2 s each margin is a full second. At 4 s against 2 s — which
/// this briefly was — the second margin is exactly zero.
///
/// So a daemon that honours the horizon has its idle clock reset a second before
/// it would have fired, stays, delivers at ~3 s and leaves ~2 s after that. One
/// that cannot see the reminder returns at ~2 s, a second before it was due, and
/// then nothing delivers it, ever. The assertion is an order between two
/// observable events — the delivery, and `serve`'s return — not a stopwatch
/// reading.
///
/// **The daemon may not start its idle clock until it has seen the reminder**,
/// which is what the second, already-due reminder is for. It is added alongside
/// the first and waited for WHILE THE WRITER IS STILL CONNECTED — where a client
/// is itself a reason to stay, so the wait cannot be raced and costs nothing.
/// Its delivery can only come from a tick that rebuilt the heap from a store
/// already holding both, and every tick republishes `peek_at()` for the accept
/// loop on its way out. Only then does the client leave.
///
/// #398, in two parts. The first cut led by 1.8 s against the same 2 s, which
/// makes BOTH daemons deliver the reminder (the reminder loop fires
/// independently of the idle decision) and leaves "was it still here at 3 s" as
/// the only difference — a 3 s window against a ~3.8 s departure. The comment
/// here claimed slowness could only cost a missed defect, never a red run. That
/// was wrong in the one direction that matters: `at` is an ABSOLUTE instant
/// fixed BEFORE the `task.add` round trip while the window starts AFTER it, so
/// every millisecond that round trip loses under load comes straight off the
/// 800 ms margin. One second of injected delay reddens it with the daemon
/// untouched.
///
/// Lengthening the lead alone did not finish the job, and two stress runs said
/// so — 25 runs of this binary beside a full `cargo test` and one busy core
/// each, twice over. The first reddened here with the daemon's own stderr
/// showing it had left at its 2 s deadline: the reminder loop had not ticked
/// yet, so the mirror the accept loop reads was still empty when the idle clock
/// ran out — a window that opens at the `task.add` and has nothing to do with
/// how far out the reminder is. Hence the barrier, which closes that window by
/// construction instead of by making the numbers bigger. The second reddened the
/// same line for the opposite reason: the lead had been pushed to 4 s, exactly
/// twice the timeout, so the reminder entered the horizon at the same instant
/// the idle clock expired and which loop looked first decided the run.
///
/// Slowness costs a missed defect here — a round trip slow enough to drag the
/// ripening back before the wrong daemon's deadline makes the delivery happen
/// either way — but slowness alone cannot redden the run.
///
/// **What no margin can fix, measured rather than assumed.** The daemon judges
/// the horizon on the WALL clock (`reminder_due_within` reads
/// `jiff::Timestamp::now()`) while its idle deadline runs on a monotonic
/// `Instant`. A wall clock that steps BACKWARDS therefore makes a pending
/// reminder look further away than it is while the deadline keeps running, and
/// the daemon leaves for a reason that has nothing to do with the mechanism
/// under test. That is not hypothetical here: sampling both clocks for 60 s
/// under the load this suite runs beside measured four backward steps, the worst
/// 5.4 s — every one of them at least 2 s, half of them over 5 s. A margin that
/// absorbs that is a 6 s timeout and a twenty-second test, and it would still be
/// a bet rather than a guarantee.
///
/// So an early departure is not a verdict by itself. The window samples both
/// clocks, and a departure with a backward excursion in it is a SPOILED
/// observation, retried on a fresh store up to three times. A daemon that has
/// genuinely stopped seeing the reminder fails on the FIRST attempt, because
/// nothing spoiled it — which is exactly what the injected-drift run checks.
///
/// **What the instrument covers, and what it does not.** Two stretches are
/// sampled, and both are reported when the daemon leaves early: the judged
/// WINDOW, from the client's disconnect to the outcome, and the SETUP stretch
/// before it — from where `at` is computed, through the two adds and the barrier
/// wait, up to the instant the window opens. The setup number is recorded and
/// never judged. A step there is invisible to the window and still decides the
/// run, because `at` is absolute: the reminder ends up further out by the size
/// of the step against the daemon's own clock, which can put it beyond twice the
/// timeout and out of reach of the horizon. The pair is what tells a blind
/// instrument from a second mechanism.
///
/// It settles nothing by itself, and it makes no failure less likely — it makes
/// the next one self-diagnosing. The state of the question, to be re-derived
/// from a run rather than trusted here: one failure in 25 runs before the setup
/// stretch was sampled, whose window read -0.000s and could not say why; then
/// none in 75 runs after. So the hypothesis that a step lands in the setup
/// stretch is UNTESTED rather than confirmed — the failure has not fired again,
/// so nothing has read that stretch at the moment it matters. Note also that the
/// sampling adds clock reads inside the barrier loop, so it cannot be claimed
/// perfectly timing-neutral.
#[test]
fn a_reminder_about_to_ripen_holds_the_daemon_past_its_own_deadline() {
    let mut spoiled = Vec::new();
    for _ in 0..3 {
        match observe_a_reminder_holding_the_daemon() {
            Ok(()) => return,
            Err((skew, setup)) => {
                // Half a second is far below the smallest step ever measured
                // here (2 s) and far above anything a healthy clock does, so
                // this separates "the clock moved" from "the daemon stopped
                // honouring the horizon" without straddling either.
                //
                // Judged on the WINDOW alone. `setup` is reported beside it and
                // never tested: it is there to say which stretch moved when this
                // fires, not to widen the excuse.
                assert!(
                    skew <= -0.5,
                    "the daemon returned at its own idle deadline while a reminder it should \
                     have waited for was still pending, so nothing ever delivered it — and the \
                     wall clock held still across the judged window (worst backward excursion \
                     {skew:.3}s; across the earlier stretch, from `at` through the barrier, \
                     {setup:.3}s), which leaves the mechanism this test guards"
                );
                spoiled.push(format!("window {skew:.3}s / setup {setup:.3}s"));
            }
        }
    }
    panic!(
        "three observations running were spoiled by a backward wall-clock step ({}), so this \
         run never got to see the property. The daemon is not implicated: `reminder_due_within` \
         reads the wall clock while the idle deadline runs on a monotonic `Instant`.",
        spoiled.join(", ")
    );
}

/// One observation of the horizon: `Ok(())` when the reminder was delivered
/// before `serve` returned, `Err((window, setup))` when the daemon returned
/// first. Both are the worst BACKWARD wall-clock excursion in seconds, `0.0`
/// meaning the clock ran forward throughout — `window` across the judged stretch
/// after the client leaves, `setup` across the stretch before it. Only `window`
/// is ever judged; see this test's doc comment for why `setup` is carried.
fn observe_a_reminder_holding_the_daemon() -> Result<(), (f64, f64)> {
    let (db, sock) = unique_target();
    let collector: Arc<Collecting> = Arc::new(Collecting::default());
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            notifier: collector.clone(),
            idle_timeout: Some(Duration::from_secs(2)),
            retired_marker: None,
            ..Default::default()
        },
    );

    // Observation only (#398): the stretch the judged window below cannot see,
    // opened here so it covers `at` itself.
    let (setup_mono, setup_wall) = (std::time::Instant::now(), jiff::Timestamp::now());
    let mut worst_setup: f64 = 0.0;

    {
        let mut writer = daemon::try_connect(&sock).expect("connect writer");
        let at = jiff::Timestamp::now() + jiff::SignedDuration::from_millis(3000);
        let added = writer
            .request(
                "task.add",
                &json!({ "title": "hold the door", "remind": at.to_string() }),
            )
            .expect("add a task with a reminder");
        ok(&added);
        // The barrier: already due, so the scheduler fires it on its first tick
        // after both of these writes.
        let barrier = writer
            .request(
                "task.add",
                &json!({ "title": "already due", "remind": "2020-01-01T00:00:00Z" }),
            )
            .expect("add the barrier reminder");
        ok(&barrier);

        // Waited for here, still connected: a tick has now rebuilt the heap
        // from a store holding both reminders, and republished what it holds.
        let seen = std::time::Instant::now() + Duration::from_secs(30);
        while !collector
            .titles()
            .iter()
            .any(|t| t.as_str() == "already due")
        {
            assert!(
                std::time::Instant::now() < seen,
                "the scheduler never delivered an already-due reminder within 30s"
            );
            let mono = setup_mono.elapsed().as_secs_f64();
            let wall = jiff::Timestamp::now()
                .duration_since(setup_wall)
                .as_secs_f64();
            worst_setup = worst_setup.min(wall - mono);
            thread::sleep(Duration::from_millis(20));
        }
    } // The client leaves; from here the reminder is the only reason to stay.

    // Closes the setup stretch where the judged window opens, so the two
    // together cover everything since `at` — including the case where the
    // barrier had already been delivered when the loop above first looked and
    // its body never ran.
    {
        let mono = setup_mono.elapsed().as_secs_f64();
        let wall = jiff::Timestamp::now()
            .duration_since(setup_wall)
            .as_secs_f64();
        worst_setup = worst_setup.min(wall - mono);
    }

    // Whichever comes first decides, and one of them always comes: a daemon that
    // honours the horizon delivers, one that does not returns at its own
    // deadline a second before the reminder was due and delivers nothing ever.
    // Both clocks are sampled across the window, because on this machine a
    // backward wall-clock step is its own explanation for an early return.
    let (mono_start, wall_start) = (std::time::Instant::now(), jiff::Timestamp::now());
    let mut worst_skew: f64 = 0.0;
    let deadline = mono_start + Duration::from_secs(30);
    let left_early = loop {
        let mono = mono_start.elapsed().as_secs_f64();
        let wall = jiff::Timestamp::now()
            .duration_since(wall_start)
            .as_secs_f64();
        worst_skew = worst_skew.min(wall - mono);
        // By NAME. The barrier is already in the collector, so "something was
        // delivered" is satisfied the instant the client lets go, and this
        // would break out having proved nothing.
        if collector
            .titles()
            .iter()
            .any(|t| t.as_str() == "hold the door")
        {
            break None;
        }
        if let Some(returned) = serve_result(&result_rx, Duration::from_millis(50)) {
            break Some(returned);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "neither the reminder nor the daemon's return arrived within 30s"
        );
    };

    if left_early.is_some() {
        // Already returned, so the join is immediate; the caller decides whether
        // this was the defect or the clock.
        shutdown.store(true, Ordering::Relaxed);
        server.join().expect("server thread");
        let _ = std::fs::remove_file(&db);
        return Err((worst_skew, worst_setup));
    }

    assert_eq!(
        collector.titles(),
        vec!["already due".to_string(), "hold the door".to_string()],
        "the barrier, then the reminder it stayed for — each delivered exactly once"
    );

    let outcome = serve_result(&result_rx, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    outcome
        .expect("and once the reminder is delivered it may leave")
        .expect("idle shutdown is an ordinary stop");
    let _ = std::fs::remove_file(&db);
    Ok(())
}

/// A TCP port nothing is listening on right now, for the daemon's OTLP receiver
/// to bind. Asking the OS for an ephemeral port and immediately giving it back
/// is the standard trick; the window in which another process could steal it is
/// tiny, and losing it only means the receiver logs a bind failure — which the
/// test below then reports as a failed POST rather than as a silent pass.
fn free_local_port() -> u16 {
    let probe = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .expect("bind an ephemeral probe port");
    probe.local_addr().expect("probe address").port()
}

/// POST one minimal OTLP log export at `port`. Returns whether the receiver
/// answered at all — the tests here care that the request reached the accept
/// loop, not what it parsed to.
fn post_otlp_export(port: u16) -> bool {
    use std::io::{Read, Write};

    let body = json!({
        "resourceLogs": [{ "scopeLogs": [{ "logRecords": [{
            "eventName": "claude_code.api_request",
            "observedTimeUnixNano": "1774339200000000000",
            "attributes": [
                { "key": "input_tokens", "value": { "intValue": "7" } },
                { "key": "session.id", "value": { "stringValue": "idle-clock" } }
            ]
        }] }] }]
    })
    .to_string();
    let request = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let Ok(mut stream) = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)) else {
        return false;
    };
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 200")
}

/// `[otlp] enabled` must not quietly cancel `[daemon] idle_timeout`.
///
/// It did. The accept loop asked `server_busy` whether telemetry was in play by
/// passing `otlp_port.is_some()` — a constant for the whole process — so with
/// the receiver on, the idle clock never started and the daemon ran forever
/// after printing "will exit after N minute(s) with no clients and no work" on
/// its own stderr. Every unit test still passed: they all call the predicate
/// with a literal, and none of them can see what the call site feeds it. Only a
/// daemon built with BOTH options can, which is why this test is here and not
/// next to the predicate.
#[test]
fn an_open_but_silent_otlp_receiver_does_not_cancel_the_idle_timeout() {
    let (db, sock) = unique_target();
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            otlp_port: Some(free_local_port()),
            idle_timeout: Some(Duration::from_millis(400)),
            retired_marker: None,
            ..Default::default()
        },
    );

    let outcome = serve_result(&result_rx, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    outcome
        .expect(
            "a daemon with the OTLP receiver bound but nobody posting must still \
             honour its idle timeout — an open port is configuration, not work",
        )
        .expect("idle shutdown is an ordinary stop");
    let _ = std::fs::remove_file(&db);
}

/// The other half, and the reason the fix is a clock rather than a deletion:
/// telemetry that actually arrives IS work, and holds the daemon open.
///
/// An OTLP client holds no daemon connection and no subscription, so the accept
/// loop cannot see it any other way; a daemon that left mid-export would drop it
/// with no error at either end. Traffic is generated for well over three times
/// the timeout, so a daemon that ignores it leaves long before the loop ends —
/// and the posts are five times more frequent than the deadline, so it takes a
/// five-fold stall, not a slow machine, to fail this spuriously.
#[test]
fn telemetry_arriving_holds_the_daemon_open_and_silence_then_releases_it() {
    let (db, sock) = unique_target();
    let port = free_local_port();
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            otlp_port: Some(port),
            idle_timeout: Some(Duration::from_millis(500)),
            retired_marker: None,
            ..Default::default()
        },
    );

    // The idle clock starts with `serve`, not with the receiver: a POST that
    // missed a slow bind let the daemon retire and take the receiver with it
    // (#666). A held client connection is work, so the clock waits for the
    // receiver's first answer; a bind that lost the port race still fails.
    let holding_open = daemon::try_connect(&sock).expect("the daemon accepted a holding client");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut answered = false;
    while std::time::Instant::now() < deadline {
        if post_otlp_export(port) {
            answered = true;
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        answered,
        "the daemon's OTLP receiver never answered a POST within 30s"
    );
    drop(holding_open);

    for _ in 0..15 {
        assert!(
            post_otlp_export(port),
            "the receiver stopped answering: the daemon left while telemetry \
             was still arriving"
        );
        assert!(
            serve_result(&result_rx, Duration::from_millis(100)).is_none(),
            "a daemon being posted to must not leave: an OTLP export is the one \
             kind of work that holds no connection here"
        );
    }

    // Nothing posts any more. The clock that telemetry kept resetting now runs.
    let outcome = serve_result(&result_rx, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    outcome
        .expect("once the telemetry stops the daemon must be free to go")
        .expect("idle shutdown is an ordinary stop");
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
/// A negative assertion needs two things a stopwatch cannot give it: the
/// measurement must be unable to land after the read, and the completion must be
/// one that WOULD have been measured, or the test passes on a fixture nothing
/// could have attributed. Both come from sequencing here.
///
/// **Nothing can land after the read.** The opted-out daemon is stopped and
/// JOINED before the store is read, so "no measurement" is a fact about a
/// returned process rather than a race with a running one. No budget defends it
/// and none can invalidate it.
///
/// **The fixture is provably measurable.** After that read, a second daemon with
/// the opt-in ON opens the SAME store and attributes that very completion — the
/// catch-up-on-start rebuild `attribution_loop` documents. That is what makes
/// the emptiness a statement about the gate instead of about the transcript.
///
/// **What is left, and stated rather than hidden.** The one thing no observable
/// outside the process can establish is that a thread which does not exist would
/// have run by now. What bounds it is the daemon's OWN barrier: a second task
/// whose reminder ripens a second after the completion, delivered by this
/// daemon's own reminder loop, in this daemon's own process, under this
/// machine's actual load — where an attribution tick is 500 ms and the first
/// tick rebuilds the pending set from the whole store. It is the idiom
/// `a_restarted_daemon_does_not_refire_an_already_reminded_reminder` uses one
/// screen up, and the inference is between two sibling loops in one process
/// rather than between two machines' worth of scheduling.
///
/// **The usage line is placed inside the window by arithmetic, not by a clock
/// read.** The task's `created` and `completed` are read back out of the store
/// after the completion, and the transcript is stamped with their midpoint, so
/// containment is a property of two stored values rather than of the machine's
/// clock running forward between two reads — which on this machine it does not
/// always do. The first cut stamped the line with a `util::now()` taken between
/// the creation and the completion, and one stress run in 25 caught it: the
/// daemon reported "no usage in window yet" for a file whose lines sat just
/// outside a window nothing could widen, and the poll below timed out on a
/// daemon that was working correctly. What is left of that exposure is named in
/// the assertion beside the arithmetic — if the store itself recorded a
/// completion before its own creation, the window is empty and the test says so
/// in those words instead of blaming attribution.
///
/// #502: the control used to be a *concurrent* second daemon on its own store,
/// whose `tokens.attributed` push was read as a clock for this one. That is an
/// ordering between two independent processes, and the 5 s `wait_for_op` budget
/// riding on it made a loaded runner red — while the coverage job
/// (`cargo llvm-cov --all-targets`) runs this very test under instrumentation.
/// Proving both halves on one store needs no cross-daemon ordering at all.
#[test]
fn attribution_does_not_run_when_the_opt_in_is_off() {
    let (db, sock) = unique_target();
    let dir = unique_dir("off");
    let path = dir.join("session.jsonl");

    // Observed rather than detached: everything below rests on this daemon
    // being demonstrably gone, which is a return value, not an elapsed time.
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            tokens_enabled: false, // the default, spelled out because it is the subject
            ..Default::default()
        },
    );
    let rx = subscribe_events(&sock);
    let mut c = daemon::try_connect(&sock).expect("connect");

    let add = c
        .request("task.add", &json!({ "title": "ship it" }))
        .unwrap();
    let id = ok(&add)["short_id"].as_i64().unwrap();

    // Completed against a transcript that does not exist yet, which is the real
    // shape and not a convenience: `tasqx done` runs inside the agent turn whose
    // usage it wants to count, and that turn's line is not written until the turn
    // ends (#73), so the daemon reads an absent transcript as "not yet".
    c.request(
        "task.done",
        &json!({
            "ref": id,
            "client": "claude-code",
            "transcript_path": path.to_string_lossy(),
        }),
    )
    .unwrap();

    // The window is read back OUT OF THE STORE and the usage line placed inside
    // it by arithmetic — see the doc comment for why no clock is read here.
    let got = c.request("task.get", &json!({ "ref": id })).unwrap();
    let window = ok(&got);
    let created: jiff::Timestamp = window["created"]
        .as_str()
        .expect("a task carries its created")
        .parse()
        .expect("created parses");
    let completed: jiff::Timestamp = window["completed"]
        .as_str()
        .expect("a completed task carries its completed")
        .parse()
        .expect("completed parses");
    assert!(
        completed > created,
        "the store recorded a completion before its own creation ({created} .. {completed}): \
         the clock stepped backwards mid-test, and no sample timestamp is inside that window"
    );
    let half =
        jiff::SignedDuration::from_nanos((completed.duration_since(created).as_nanos() / 2) as i64);
    std::fs::write(&path, transcript(&(created + half).to_string())).unwrap();

    // The barrier comes last, so the second of background loops it bounds is a
    // second in which everything an attribution pass needs is already in place.
    let at = jiff::Timestamp::now() + jiff::SignedDuration::from_millis(1000);
    let barrier = c
        .request(
            "task.add",
            &json!({ "title": "the barrier", "remind": at.to_string() }),
        )
        .unwrap();
    ok(&barrier);
    wait_for_op_within(&rx, "reminded", Duration::from_secs(30));

    // Stop it and watch it go. Everything after this reads a store nothing is
    // writing to.
    drop(c);
    shutdown.store(true, Ordering::Relaxed);
    serve_result(&result_rx, Duration::from_secs(20))
        .expect("the opted-out daemon must return when it is asked to stop")
        .expect("an asked-for stop is an ordinary stop");
    server.join().expect("server thread");

    let unattributed = {
        let e = Engine::open(&db).expect("open the store the daemon left behind");
        let got = dispatch(&e, "task.get", &json!({ "ref": id })).expect("task.get");
        got["tokens"].as_array().expect("tokens array").len()
    };
    assert_eq!(
        unattributed, 0,
        "attribution must stay off without the opt-in, and the only daemon that \
         could have written one has already returned"
    );

    // The same completion, the same store, the opt-in ON.
    let (_, on_sock) = unique_target();
    let on_shutdown = start_daemon_with_options(&db, &on_sock, Arc::new(LogNotifier), true);
    let mut on_c = daemon::try_connect(&on_sock).expect("connect the opted-in daemon");
    // Polled, not waited on: the completion was already in the store when this
    // daemon opened it, so its catch-up pass can finish before any subscription
    // could exist. The stored measurement is this half's claim; the push that
    // accompanies it is `a_correlated_completion_yields_a_stored_measurement_and_a_push`'s.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let measured = loop {
        let got = on_c.request("task.get", &json!({ "ref": id })).unwrap();
        let tokens = ok(&got)["tokens"].as_array().expect("tokens array").clone();
        if !tokens.is_empty() {
            break tokens;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the opted-in daemon never attributed the completion the opted-out one left, \
             so the fixture above proves nothing"
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        measured.len(),
        1,
        "exactly one measurement, written by the daemon that was allowed to: {measured:?}"
    );
    assert_eq!(measured[0]["source"], json!("log-parse"));
    assert_eq!(measured[0]["input_tokens"], json!(110), "in-window only");

    on_shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
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

/// D50 Decision 3: `tokens.recompute` parses transcripts, which must never
/// happen under the daemon's global engine lock — the daemon refuses the
/// method at the transport, BEFORE dispatch, naming the in-process
/// invocation. The refusal is a correlated `bad_request` (not a transport
/// drop), and the connection keeps serving normal methods afterwards.
#[test]
fn tokens_recompute_is_refused_over_the_socket_naming_the_in_process_invocation() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);
    let mut c = daemon::try_connect(&sock).expect("connect");

    let env = c.request("tokens.recompute", &json!({})).unwrap();
    assert_eq!(
        env.get("ok"),
        Some(&Value::Bool(false)),
        "the daemon must refuse tokens.recompute: {env}"
    );
    assert_eq!(
        env.get("id"),
        Some(&json!(1)),
        "the refusal correlates the request id: {env}"
    );
    let error = env.get("error").expect("error body");
    assert_eq!(error.get("code"), Some(&json!("bad_request")), "{env}");
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
    assert!(
        message.contains("in-process") && message.contains("--no-daemon"),
        "the refusal must name the in-process invocation, got: {message}"
    );

    // `--apply` polarity makes no difference: the refusal is method-level.
    let env = c
        .request("tokens.recompute", &json!({ "dry_run": false }))
        .unwrap();
    assert_eq!(env.get("ok"), Some(&Value::Bool(false)), "{env}");

    // The connection is still healthy: a normal method dispatches fine.
    let added = c
        .request("task.add", &json!({ "title": "still serving" }))
        .unwrap();
    assert!(ok(&added)["short_id"].as_i64().unwrap() >= 1);

    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

// ---- D74: the store is a read surface of the daemon itself -------------------

/// D74's socket half: any ordinary client can ask a running daemon which store
/// it owns, because `core.capabilities` carries `store`, read off the daemon
/// engine's own connection. Before this no route existed at all — the startup
/// banner did not say it, `config store` deliberately declined to guess (D47),
/// and capabilities did not carry it — so nobody could learn which file a
/// running daemon was writing to (2026-08-31 field test, finding #10).
#[test]
fn a_client_can_ask_a_running_daemon_which_store_it_owns() {
    let (db, sock) = unique_target();
    let shutdown = start_daemon(&db, &sock);

    let mut conn = daemon::try_connect(&sock).expect("connect");
    let response = conn
        .request("core.capabilities", &json!({}))
        .expect("capabilities over the socket");
    let store = response["result"]["store"]
        .as_str()
        .expect("a file-backed daemon must name its store");
    assert_eq!(
        std::fs::canonicalize(store).expect("the named store must exist"),
        std::fs::canonicalize(&db).expect("the fixture db exists"),
        "the daemon must name the file it actually opened, not a guess"
    );

    drop(conn);
    shutdown.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_file(&db);
}

/// D74's tombstone half: an idle retirement — and only an idle retirement —
/// records itself where [`daemon::DaemonOptions::retired_marker`] said to,
/// naming the socket, the store and the instant, so the first command after
/// the transition can report that its target changed instead of switching
/// silently (#246's sixty-second fuse, #255's missing announcement).
#[test]
fn an_idle_retirement_records_itself_where_the_options_said_to() {
    let (db, sock) = unique_target();
    let marker = std::env::temp_dir().join(format!(
        "tasqx-test-retired-{}-{}",
        std::process::id(),
        sock.replace(['\\', '/', ':'], "_")
    ));
    let _ = std::fs::remove_file(&marker);
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            idle_timeout: Some(Duration::from_millis(400)),
            retired_marker: Some(marker.clone()),
            ..Default::default()
        },
    );

    let outcome = serve_result(&result_rx, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    server.join().expect("server thread");
    outcome
        .expect("an idle daemon must return from serve")
        .expect("idle shutdown is an ordinary stop");

    let body = std::fs::read_to_string(&marker).expect("the retirement must be recorded");
    assert!(
        body.lines().any(|l| l == format!("socket {sock}")),
        "the marker must record the address it retired from, verbatim: {body}"
    );
    let recorded = body
        .lines()
        .find_map(|l| l.strip_prefix("store "))
        .expect("the marker must record the store the daemon owned");
    assert_eq!(
        std::fs::canonicalize(recorded).expect("the recorded store must exist"),
        std::fs::canonicalize(&db).expect("the fixture db exists"),
        "the recorded store must be the daemon's own file: {body}"
    );
    assert!(
        body.lines().any(|l| l.starts_with("retired ")),
        "the marker must record when, so a late reader is not told an old \
         transition as news: {body}"
    );

    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(&db);
}

/// The restraint that makes the tombstone meaningful: a Ctrl-C is the
/// operator's own act, and a daemon stopped that way leaves no note — a marker
/// written on every stop would make the retirement report a lie about
/// transitions nobody needs announced.
#[test]
fn a_ctrl_c_stop_leaves_no_retirement_marker() {
    let (db, sock) = unique_target();
    let marker = std::env::temp_dir().join(format!(
        "tasqx-test-ctrlc-{}-{}",
        std::process::id(),
        sock.replace(['\\', '/', ':'], "_")
    ));
    let _ = std::fs::remove_file(&marker);
    let (shutdown, result_rx, server) = start_daemon_observing_result(
        &db,
        &sock,
        daemon::DaemonOptions {
            retired_marker: Some(marker.clone()),
            ..Default::default()
        },
    );

    // The Ctrl-C path: the flag, not the idle clock.
    shutdown.store(true, Ordering::Relaxed);
    serve_result(&result_rx, Duration::from_secs(20))
        .expect("a flagged daemon must return from serve")
        .expect("a clean stop");
    server.join().expect("server thread");

    assert!(
        !marker.exists(),
        "a deliberate stop must not be recorded as a retirement"
    );
    let _ = std::fs::remove_file(&db);
}
