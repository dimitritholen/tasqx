use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::{json, Value};
use tasqx_core::{ApiError, Engine, ErrorCode};

static SEQ: AtomicU64 = AtomicU64::new(0);
static RACE_LOCK: Mutex<()> = Mutex::new(());
static BUSY_SENDER: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(None);
static ADD_BUSY_SENDER: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(None);
static ARCHIVE_BUSY_SENDER: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(None);
static ADD_MAY_PROCEED: AtomicBool = AtomicBool::new(true);

// How long a busy add worker waits for ADD_MAY_PROCEED before giving up. It has to
// outlast the two five-second recv_timeouts below, because a legitimate run keeps
// the add worker parked for as long as the archive worker needs to reach its own
// BEGIN IMMEDIATE and finish; anything shorter would turn a slow box into a
// spurious SQLITE_BUSY.
const ADD_SPIN_TIMEOUT: Duration = Duration::from_secs(15);

/// Serialises the race tests and, crucially, puts the process-global coordination
/// state back on *every* exit path including unwind.
///
/// Without the Drop impl a panic inside a race test poisoned RACE_LOCK, so every
/// later race test died on the lock with `PoisonError` instead of on its own
/// assertion, and left a stale sender plus a permanently-parked add worker behind.
struct RaceGuard {
    _serial: MutexGuard<'static, ()>,
}

impl RaceGuard {
    fn acquire() -> Self {
        // Ignoring the poison is right here: RACE_LOCK carries no data, it only
        // serialises the tests, so a panic in a previous holder cannot have left
        // any invariant of `()` broken. Everything a panic *could* have left
        // inconsistent is what Drop below resets.
        RaceGuard {
            _serial: RACE_LOCK.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }
}

impl Drop for RaceGuard {
    fn drop(&mut self) {
        // Release first: a worker stranded in add_busy_signal's spin must be let go
        // before it burns a core for the rest of the test binary's life.
        ADD_MAY_PROCEED.store(true, Ordering::Release);
        for slot in [&BUSY_SENDER, &ADD_BUSY_SENDER, &ARCHIVE_BUSY_SENDER] {
            // into_inner again, and for a harder reason: this runs during unwind,
            // and a panicking Drop would abort the whole test binary.
            slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        }
    }
}

struct Store {
    path: PathBuf,
}

impl Store {
    fn new(label: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "tasqx-concurrency-{label}-{}-{n}.db",
            std::process::id()
        ));
        Store { path }
    }

    fn engine(&self) -> Engine {
        Engine::open(self.path.to_str().expect("UTF-8 temp path")).expect("open test store")
    }

    fn connection(&self) -> Connection {
        Connection::open(&self.path).expect("open blocker connection")
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

fn busy_signal(count: i32) -> bool {
    if count == 0 {
        if let Some(sender) = BUSY_SENDER.lock().expect("busy sender lock").as_ref() {
            let _ = sender.send(());
        }
    }
    thread::yield_now();
    true
}

fn signal_once(sender: &Mutex<Option<mpsc::Sender<()>>>, count: i32) {
    if count == 0 {
        let sender = sender.lock().expect("busy sender lock").clone();
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }
}

/// Parks the caller until the test releases ADD_MAY_PROCEED, or the deadline
/// passes. Returns whether permission was actually granted.
fn wait_for_add_permission(deadline: Instant) -> bool {
    while !ADD_MAY_PROCEED.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::yield_now();
    }
    true
}

fn add_busy_signal(count: i32) -> bool {
    signal_once(&ADD_BUSY_SENDER, count);
    // Surrendering (false) makes SQLite return SQLITE_BUSY, which surfaces through
    // the caller's .expect("add succeeds") as a readable failure. Panicking here
    // instead would unwind out of a C callback frame.
    wait_for_add_permission(Instant::now() + ADD_SPIN_TIMEOUT)
}

fn archive_busy_signal(count: i32) -> bool {
    signal_once(&ARCHIVE_BUSY_SENDER, count);
    thread::yield_now();
    true
}

fn modify(engine: Engine, title: &'static str) -> Result<Value, ApiError> {
    engine.task_modify(&json!({
        "ref": 1,
        "expected_rev": 1,
        "set": { "title": title },
    }))
}

fn race_two<A, B>(
    store: &Store,
    first_call: impl FnOnce(Engine) -> A + Send + 'static,
    second_call: impl FnOnce(Engine) -> B + Send + 'static,
) -> (A, B)
where
    A: Send + 'static,
    B: Send + 'static,
{
    let _serial = RaceGuard::acquire();
    // Open/configure both Engines before the blocker takes the write lock:
    // Engine::open performs migrations, which are writes of their own.
    let first = store.engine();
    let second = store.engine();
    first
        .conn()
        .busy_handler(Some(busy_signal))
        .expect("first busy handler");
    second
        .conn()
        .busy_handler(Some(busy_signal))
        .expect("second busy handler");

    let blocker = store.connection();
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold write reservation");

    let (busy_tx, busy_rx) = mpsc::channel();
    *BUSY_SENDER.lock().expect("set busy sender") = Some(busy_tx);

    let a = thread::spawn(move || first_call(first));
    let b = thread::spawn(move || second_call(second));

    for worker in ["first", "second"] {
        busy_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{worker} worker never reached BEGIN IMMEDIATE"));
    }
    blocker
        .execute_batch("COMMIT")
        .expect("release write reservation");

    // BUSY_SENDER is cleared by RaceGuard::drop, so it happens on the unwind path
    // too — every one of the expects above is reachable on a loaded box.
    (
        a.join().expect("first worker"),
        b.join().expect("second worker"),
    )
}

fn op_count(events: &Value, op: &str) -> usize {
    events["events"]
        .as_array()
        .expect("event array")
        .iter()
        .filter(|event| event["op"] == op)
        .count()
}

#[test]
fn two_guarded_modifies_from_one_revision_have_one_winner() {
    let store = Store::new("guarded-modify");
    let seed = store.engine();
    seed.task_add(&json!({ "title": "original" }))
        .expect("seed task");
    drop(seed);

    let raced = race_two(
        &store,
        |engine| modify(engine, "alpha"),
        |engine| modify(engine, "beta"),
    );
    let results = [raced.0, raced.1];

    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "exactly one guarded edit wins"
    );
    let errors: Vec<&ApiError> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(errors.len(), 1, "exactly one guarded edit conflicts");
    assert_eq!(errors[0].code, ErrorCode::Conflict);

    let check = store.engine();
    let task = check
        .task_get(&json!({ "ref": 1 }))
        .expect("read final task");
    assert_eq!(task["_rev"], 2, "one effective edit advances one revision");
    let events = check
        .event_list(&json!({ "entity": "task", "limit": 20 }))
        .expect("events");
    assert_eq!(
        op_count(&events, "modify"),
        1,
        "one effective edit appends one modify event"
    );
}

#[test]
fn racing_starts_create_one_interval() {
    let store = Store::new("start");
    store
        .engine()
        .task_add(&json!({ "title": "timer" }))
        .expect("seed task");

    let results = race_two(
        &store,
        |engine| engine.task_start(&json!({ "ref": 1 })),
        |engine| engine.task_start(&json!({ "ref": 1 })),
    );
    assert!(
        results.0.is_ok(),
        "first start: {:?}",
        results.0.err().map(|e| e.message)
    );
    assert!(
        results.1.is_ok(),
        "second start is idempotent: {:?}",
        results.1.err().map(|e| e.message)
    );

    let check = store.engine();
    let task = check
        .task_get(&json!({ "ref": 1 }))
        .expect("read final task");
    assert_eq!(task["status"], "active");
    assert_eq!(task["_rev"], 2, "one effective start advances one revision");
    assert_eq!(
        task["tracked"], "PT0S",
        "the second start must not close the new interval"
    );
    assert!(
        task["active_since"].is_string(),
        "the winning interval remains open"
    );
    let events = check
        .event_list(&json!({ "entity": "task", "limit": 20 }))
        .expect("events");
    assert_eq!(op_count(&events, "start"), 1, "only one interval starts");
    assert_eq!(
        op_count(&events, "stop"),
        0,
        "the second call must not stop the first interval"
    );
}

#[test]
fn racing_recurring_completions_spawn_once() {
    let store = Store::new("done");
    store
        .engine()
        .task_add(&json!({
            "title": "repeat me",
            "due": "2099-01-01T09:00:00Z",
            "recurrence": "every 3 days",
        }))
        .expect("seed recurring task");

    let raced = race_two(
        &store,
        |engine| engine.task_done(&json!({ "ref": 1 })),
        |engine| engine.task_done(&json!({ "ref": 1 })),
    );
    let results = [raced.0, raced.1];
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "one completion wins"
    );
    let errors: Vec<&ApiError> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(errors.len(), 1, "the second completion conflicts");
    assert_eq!(errors[0].code, ErrorCode::Conflict);

    let check = store.engine();
    let tasks = check
        .task_list(&json!({ "filter": "" }))
        .expect("list tasks");
    assert_eq!(
        tasks["count"], 2,
        "the template spawns exactly one successor"
    );
    let events = check
        .event_list(&json!({ "entity": "task", "limit": 20 }))
        .expect("events");
    assert_eq!(op_count(&events, "done"), 1, "the template completes once");
    assert_eq!(
        op_count(&events, "add"),
        2,
        "one seed and one spawned add event"
    );
}

#[test]
fn annotation_and_tag_advance_two_revisions() {
    let store = Store::new("annotation-tag");
    store
        .engine()
        .task_add(&json!({ "title": "original" }))
        .expect("seed task");

    let results = race_two(
        &store,
        |engine| engine.annotation_add(&json!({ "ref": 1, "body": "keep this note" })),
        |engine| engine.tag_add(&json!({ "ref": 1, "tags": ["keep-this-tag"] })),
    );
    assert!(
        results.0.is_ok(),
        "annotation succeeds: {:?}",
        results.0.err().map(|e| e.message)
    );
    assert!(
        results.1.is_ok(),
        "tag succeeds: {:?}",
        results.1.err().map(|e| e.message)
    );

    let check = store.engine();
    let task = check
        .task_get(&json!({ "ref": 1 }))
        .expect("read final task");
    assert_eq!(
        task["annotations"].as_array().expect("annotations").len(),
        1
    );
    assert_eq!(task["annotations"][0]["body"], "keep this note");
    assert_eq!(task["tags"], json!(["keep-this-tag"]));
    assert_eq!(
        task["_rev"], 3,
        "two effective mutations advance two revisions"
    );
    let events = check
        .event_list(&json!({ "entity": "task", "limit": 20 }))
        .expect("events");
    assert_eq!(op_count(&events, "annotation.add"), 1);
    assert_eq!(op_count(&events, "tag.add"), 1);
}

#[test]
fn annotation_and_modify_preserve_both_mutations() {
    let store = Store::new("annotation-modify");
    store
        .engine()
        .task_add(&json!({ "title": "original" }))
        .expect("seed task");

    let results = race_two(
        &store,
        |engine| engine.annotation_add(&json!({ "ref": 1, "body": "keep this note" })),
        |engine| engine.task_modify(&json!({ "ref": 1, "set": { "title": "modified" } })),
    );
    assert!(
        results.0.is_ok(),
        "annotation succeeds: {:?}",
        results.0.err().map(|e| e.message)
    );
    assert!(
        results.1.is_ok(),
        "modify succeeds: {:?}",
        results.1.err().map(|e| e.message)
    );

    let check = store.engine();
    let task = check
        .task_get(&json!({ "ref": 1 }))
        .expect("read final task");
    assert_eq!(task["title"], "modified");
    assert_eq!(task["annotations"][0]["body"], "keep this note");
    assert_eq!(
        task["_rev"], 3,
        "both effective mutations advance the revision"
    );
    let events = check
        .event_list(&json!({ "entity": "task", "limit": 20 }))
        .expect("events");
    assert_eq!(op_count(&events, "annotation.add"), 1);
    assert_eq!(op_count(&events, "modify"), 1);
}

// Every race test coordinates through one process-global RACE_LOCK. A panic
// *inside* race_two — the five-second BEGIN IMMEDIATE timeout on a loaded box, a
// worker that panicked, a busy_handler that failed to install — used to poison
// that mutex, after which every remaining race test died on the lock rather than
// on its own assertion, so a maintainer chasing one broken invariant saw several
// failures all reading "race test lock: PoisonError".
#[test]
fn a_panic_inside_race_two_leaves_the_next_race_runnable() {
    // Phase 1: unwind out of race_two while it holds the serialisation lock. The
    // worker trips the busy handler first, so race_two gets all the way to
    // a.join().expect("first worker") — a panic site that is genuinely reachable.
    let poisoner = thread::spawn(|| {
        let panicking_worker: fn(Engine) = |engine| {
            let _ = modify(engine, "alpha");
            panic!("deliberate worker panic; this backtrace is part of the test");
        };
        let store = Store::new("harness-panic");
        store
            .engine()
            .task_add(&json!({ "title": "original" }))
            .expect("seed task");
        let _ = race_two(&store, panicking_worker, |engine| modify(engine, "beta"));
    })
    .join();
    assert!(
        poisoner.is_err(),
        "phase 1 must actually panic inside race_two"
    );

    // Phase 2: the next race must still be decided by its own invariant.
    let store = Store::new("harness-recovery");
    store
        .engine()
        .task_add(&json!({ "title": "original" }))
        .expect("seed task");
    let raced = race_two(
        &store,
        |engine| modify(engine, "alpha"),
        |engine| modify(engine, "beta"),
    );
    let results = [raced.0, raced.1];
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "the harness still serialises a real race after an earlier panic"
    );
}

// Whoever holds the guard owns the coordination globals, so releasing it — however
// it is released — has to hand them back clean. A panic used to skip that, leaving
// a sender whose receiver is gone and, worse, ADD_MAY_PROCEED stuck at false.
#[test]
fn the_race_guard_hands_back_clean_coordination_state_after_a_panic() {
    let dirtied = thread::spawn(|| {
        let _serial = RaceGuard::acquire();
        let (busy_tx, _busy_rx) = mpsc::channel();
        *BUSY_SENDER.lock().expect("set busy sender") = Some(busy_tx);
        ADD_MAY_PROCEED.store(false, Ordering::Release);
        panic!("deliberate panic mid-race; this backtrace is part of the test");
    })
    .join();
    assert!(dirtied.is_err(), "the helper thread must actually panic");

    // Holding the guard is what makes these two reads race-free: no other race test
    // can be mid-run, and every previous holder has run the same Drop.
    let _serial = RaceGuard::acquire();
    assert!(
        ADD_MAY_PROCEED.load(Ordering::Acquire),
        "a stranded add worker must be released, not left spinning"
    );
    assert!(
        BUSY_SENDER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none(),
        "the sender must not outlive the receiver that was going to read it"
    );
}

// A test that panicked between clearing and restoring ADD_MAY_PROCEED used to
// leave the spawned add worker spinning on thread::yield_now() for the rest of the
// test binary's life, burning a core with nothing left to wake it.
#[test]
fn a_stranded_add_worker_surrenders_instead_of_spinning_forever() {
    let _serial = RaceGuard::acquire();
    ADD_MAY_PROCEED.store(false, Ordering::Release);

    let started = Instant::now();
    let granted = wait_for_add_permission(started + Duration::from_millis(50));

    assert!(
        !granted,
        "an add worker nobody will ever release must report that it gave up"
    );
    assert!(
        started.elapsed() < ADD_SPIN_TIMEOUT,
        "it must give up on its own deadline, not the production one"
    );
}

#[test]
fn a_released_add_worker_proceeds_before_its_deadline() {
    let _serial = RaceGuard::acquire();
    ADD_MAY_PROCEED.store(false, Ordering::Release);

    // The real caller is released from another thread, mid-spin, exactly like this.
    let releaser = thread::spawn(|| {
        thread::sleep(Duration::from_millis(20));
        ADD_MAY_PROCEED.store(true, Ordering::Release);
    });
    let granted = wait_for_add_permission(Instant::now() + ADD_SPIN_TIMEOUT);
    releaser.join().expect("releaser");

    assert!(
        granted,
        "the deadline must not steal a permission that was actually granted"
    );
}

#[test]
fn inherited_add_observes_a_racing_default_archive() {
    // Bound before ADD_MAY_PROCEED is cleared below, so the guard's Drop is
    // guaranteed to restore it however this test leaves.
    let _serial = RaceGuard::acquire();
    let store = Store::new("add-archive");
    store
        .engine()
        .project_create(&json!({ "name": "work" }))
        .expect("seed default project");

    let add_engine = store.engine();
    let archive_engine = store.engine();
    add_engine
        .conn()
        .busy_handler(Some(add_busy_signal))
        .expect("add busy handler");
    archive_engine
        .conn()
        .busy_handler(Some(archive_busy_signal))
        .expect("archive busy handler");

    let blocker = store.connection();
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold write reservation");

    let (add_tx, add_rx) = mpsc::channel();
    let (archive_tx, archive_rx) = mpsc::channel();
    *ADD_BUSY_SENDER.lock().expect("set add busy sender") = Some(add_tx);
    *ARCHIVE_BUSY_SENDER.lock().expect("set archive busy sender") = Some(archive_tx);
    ADD_MAY_PROCEED.store(false, Ordering::Release);

    let add = thread::spawn(move || add_engine.task_add(&json!({ "title": "after archive" })));
    let archive = thread::spawn(move || archive_engine.project_archive(&json!({ "name": "work" })));

    add_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("add never reached its write transaction");
    archive_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("archive never reached its write transaction");
    blocker
        .execute_batch("COMMIT")
        .expect("release write reservation");

    let archived = archive
        .join()
        .expect("archive worker")
        .expect("archive succeeds");
    // Not redundant with RaceGuard::drop: the add worker is parked in its busy
    // handler right now and the join below is what un-parks it. The guard only
    // repeats this store to cover the paths that never reach this line.
    ADD_MAY_PROCEED.store(true, Ordering::Release);
    let added = add.join().expect("add worker").expect("add succeeds");
    // Both sender slots are cleared by RaceGuard::drop.

    assert_eq!(archived["default_cleared"], true);
    assert_eq!(
        added["project"],
        Value::Null,
        "the add must use the post-archive default"
    );

    let check = store.engine();
    assert_eq!(check.default_project().expect("read default"), None);
    let task = check
        .task_get(&json!({ "ref": 1 }))
        .expect("read added task");
    assert_eq!(task["project"], Value::Null);
}
