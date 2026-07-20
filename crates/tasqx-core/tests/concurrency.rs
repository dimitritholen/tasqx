use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{json, Value};
use tasqx_core::{ApiError, Engine, ErrorCode};

static SEQ: AtomicU64 = AtomicU64::new(0);
static RACE_LOCK: Mutex<()> = Mutex::new(());
static BUSY_SENDER: Mutex<Option<mpsc::Sender<()>>> = Mutex::new(None);

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

fn modify(engine: Engine, title: &'static str) -> Result<Value, ApiError> {
    engine.task_modify(&json!({
        "ref": 1,
        "expected_rev": 1,
        "set": { "title": title },
    }))
}

#[test]
fn two_guarded_modifies_from_one_revision_have_one_winner() {
    let _serial = RACE_LOCK.lock().expect("race test lock");
    let store = Store::new("guarded-modify");
    let seed = store.engine();
    seed.task_add(&json!({ "title": "original" })).expect("seed task");
    drop(seed);

    // Open/configure both Engines before the blocker takes the write lock:
    // Engine::open performs migrations, which are writes of their own.
    let first = store.engine();
    let second = store.engine();
    first.conn().busy_handler(Some(busy_signal)).expect("first busy handler");
    second.conn().busy_handler(Some(busy_signal)).expect("second busy handler");

    let blocker = store.connection();
    blocker.execute_batch("BEGIN IMMEDIATE").expect("hold write reservation");

    let (busy_tx, busy_rx) = mpsc::channel();
    *BUSY_SENDER.lock().expect("set busy sender") = Some(busy_tx);

    let a = thread::spawn(move || modify(first, "alpha"));
    let b = thread::spawn(move || modify(second, "beta"));

    for worker in ["first", "second"] {
        busy_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{worker} worker never reached BEGIN IMMEDIATE"));
    }
    blocker.execute_batch("COMMIT").expect("release write reservation");

    let results = [a.join().expect("first worker"), b.join().expect("second worker")];
    BUSY_SENDER.lock().expect("clear busy sender").take();

    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "exactly one guarded edit wins");
    let errors: Vec<&ApiError> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert_eq!(errors.len(), 1, "exactly one guarded edit conflicts");
    assert_eq!(errors[0].code, ErrorCode::Conflict);

    let check = store.engine();
    let task = check.task_get(&json!({ "ref": 1 })).expect("read final task");
    assert_eq!(task["_rev"], 2, "one effective edit advances one revision");
    let events = check.event_list(&json!({ "entity": "task", "limit": 20 })).expect("events");
    let modifies = events["events"]
        .as_array()
        .expect("event array")
        .iter()
        .filter(|event| event["op"] == "modify")
        .count();
    assert_eq!(modifies, 1, "one effective edit appends one modify event");
}
