//! Daemon transport: a long-lived local-socket server over the *same* dispatch
//! table the one-shot CLI uses (DESIGN.md §2, "One API, two transports").
//!
//! The daemon holds **no** task logic. It binds a local socket — a Unix domain
//! socket on Linux/macOS, a Windows named pipe on Windows (via `interprocess`)
//! — accepts many concurrent clients (thread-per-connection, blocking I/O),
//! and runs every newline-delimited JSON request envelope through
//! [`crate::handle_envelope`], writing back the correlated response envelope.
//!
//! Concurrency model (deliberately runtime-free — no tokio):
//!  * One shared [`Engine`] behind a `Mutex`. The lock is held **only** around
//!    `handle_envelope` (the dispatch), never across socket I/O, so slow or
//!    idle clients never serialize each other.
//!  * Per connection: a reader thread pulls request lines; a dedicated writer
//!    thread owns the send half and drains a single `mpsc` channel. Every
//!    outbound line — responses *and* event pushes — funnels through that one
//!    channel, so a notification can never interleave with an in-flight
//!    response on the same connection. Windows adds one watchdog thread per
//!    admitted client because named pipes expose no native I/O timeouts; the
//!    global admission cap bounds all three thread classes.
//!  * Live event push (§2, §6a): a client sends `{"method":"subscribe"}` and
//!    thereafter receives unsolicited `{"event":"task.changed",...}` frames.
//!    Change detection is unified through a single watermark over the
//!    append-only `events` rowid: mutations that arrive through the daemon are
//!    pumped immediately after commit, and a background poller (~400 ms) picks
//!    up EXTERNAL one-shot writes from another process on the same DB. Both
//!    paths advance the same watermark under one lock, so every event is
//!    broadcast exactly once regardless of which path observes it first.
//!  * Reminders (§9): a third thread owns the [`ReminderScheduler`] min-heap and
//!    the notifier. A ripe reminder writes its `reminded` event and is pumped
//!    like any other — so the reminder's verification surface is the ordinary
//!    event stream, not the OS notification. See [`reminder_loop`].

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    Listener, ListenerNonblockingMode, ListenerOptions, RecvHalf, SendHalf, Stream,
};
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;

use crate::dispatch::handle_envelope;
use crate::engine::Engine;
use crate::error::ApiError;
use crate::notify::{LogNotifier, Notifier};
use crate::scheduler::{self, ReminderScheduler};

/// Poll interval for detecting external (out-of-daemon) writes.
const POLL_MS: u64 = 400;

/// Reminder scheduler tick. Reminders are human-scale, so ~200 ms of latency is
/// imperceptible while keeping the idle thread essentially free.
const REMINDER_TICK_MS: u64 = 200;

/// Per-connection outbound queue depth. Responses *and* event pushes share this
/// bounded channel, so a subscriber that stops reading its socket can never make
/// daemon memory grow without bound: broadcasts to a full queue are dropped
/// (the subscriber simply misses events until it drains — `watch` re-renders the
/// whole working set on the next event it *does* receive, so a coalesced drop is
/// self-healing rather than corrupting).
const OUT_QUEUE_CAP: usize = 1024;

/// Hard cap on a single request frame (bytes up to and including the newline).
/// A client that streams bytes without ever sending `\n` can otherwise force the
/// daemon to buffer unbounded input. 1 MiB is far larger than any real envelope.
const MAX_FRAME_BYTES: usize = 1 << 20;

/// Maximum number of admitted local clients. Each admitted client owns one
/// reader thread, one writer thread, and one bounded outbound queue.
pub const MAX_CONCURRENT_CLIENTS: usize = 64;

#[cfg(unix)]
const CLIENT_IO_POLL_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
#[cfg(windows)]
const CLIENT_WATCHDOG_INTERVAL: Duration = Duration::from_millis(100);

/// Lock a mutex, recovering the guard even if a previous holder panicked while
/// holding it. A single panicked dispatch must never permanently wedge the
/// daemon for every other client (DESIGN §2: "a client must never crash the
/// daemon"); the worst case is one operation observing partially-updated state,
/// which is strictly better than a poisoned lock that panics every future
/// acquisition.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Admission {
    active: AtomicUsize,
    limit: usize,
    rejected: AtomicU64,
}

impl Admission {
    fn new(limit: usize) -> Self {
        Self { active: AtomicUsize::new(0), limit, rejected: AtomicU64::new(0) }
    }

    fn try_acquire(self: &Arc<Self>) -> Option<ClientPermit> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .ok()
            .map(|_| ClientPermit { admission: self.clone() })
    }

    fn note_rejection(&self) {
        let count = self.rejected.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count.is_power_of_two() {
            eprintln!(
                "tasqx daemon: refused {count} connection(s): client limit {} reached",
                self.limit
            );
        }
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

struct ClientPermit {
    admission: Arc<Admission>,
}

impl Drop for ClientPermit {
    fn drop(&mut self) {
        self.admission.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn idle_expired(last_activity: Instant, now: Instant) -> bool {
    now.duration_since(last_activity) >= CLIENT_IDLE_TIMEOUT
}

struct ConnectionIoState {
    last_activity: Mutex<Instant>,
    write_started: Mutex<Option<Instant>>,
    done: AtomicBool,
}

impl ConnectionIoState {
    fn new() -> Self {
        Self {
            last_activity: Mutex::new(Instant::now()),
            write_started: Mutex::new(None),
            done: AtomicBool::new(false),
        }
    }

    fn record_activity(&self) {
        *lock_recover(&self.last_activity) = Instant::now();
    }

    fn idle(&self, now: Instant) -> bool {
        idle_expired(*lock_recover(&self.last_activity), now)
    }

    fn begin_write(&self) {
        *lock_recover(&self.write_started) = Some(Instant::now());
    }

    fn end_write(&self) {
        *lock_recover(&self.write_started) = None;
    }

    #[cfg(windows)]
    fn write_timed_out(&self, now: Instant) -> bool {
        lock_recover(&self.write_started)
            .is_some_and(|started| now.duration_since(started) >= CLIENT_SEND_TIMEOUT)
    }
}

// ---- name resolution --------------------------------------------------------

/// On Windows, map an arbitrary `--socket` string onto a valid named-pipe name.
/// A bare, already-safe name (e.g. the default `tasqx-default`) is used as-is,
/// so the pipe is `\\.\pipe\tasqx-default`; a filesystem-looking path is
/// sanitized and given a stable hash suffix so distinct paths never collide.
#[cfg(windows)]
fn win_pipe_name(raw: &str) -> String {
    let safe = !raw.is_empty()
        && raw.len() <= 200
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if safe {
        return raw.to_string();
    }
    let mut base = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            base.push(c);
        } else {
            base.push('_');
        }
    }
    if base.len() > 80 {
        base.truncate(80);
    }
    // FNV-1a over the full raw string for a stable, collision-resistant suffix.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in raw.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("tasqx-{base}-{:08x}", h as u32)
}

/// Bind a blocking listener at `socket`. On Unix a stale socket file left by a
/// crashed daemon is removed first (DESIGN §2: no custom lockfile, but a dead
/// path must not wedge a restart).
fn bind(socket: &str) -> io::Result<Listener> {
    #[cfg(windows)]
    {
        let s = win_pipe_name(socket);
        let name = s.as_str().to_ns_name::<GenericNamespaced>()?;
        ListenerOptions::new().name(name).create_sync()
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        remove_if_stale(socket);
        if let Some(parent) = std::path::Path::new(socket).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let name = socket.to_fs_name::<GenericFilePath>()?;
        let listener = ListenerOptions::new().name(name).create_sync()?;
        if let Err(error) = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)) {
            let _ = std::fs::remove_file(socket);
            return Err(error);
        }
        Ok(listener)
    }
}

/// Open a client stream to `socket`. Fails fast if no daemon is listening.
fn connect_stream(socket: &str) -> io::Result<Stream> {
    #[cfg(windows)]
    {
        let s = win_pipe_name(socket);
        let name = s.as_str().to_ns_name::<GenericNamespaced>()?;
        Stream::connect(name)
    }
    #[cfg(unix)]
    {
        let name = socket.to_fs_name::<GenericFilePath>()?;
        Stream::connect(name)
    }
}

#[cfg(unix)]
fn remove_if_stale(socket: &str) {
    let p = std::path::Path::new(socket);
    if p.exists() && connect_stream(socket).is_err() {
        let _ = std::fs::remove_file(p);
    }
}

/// Remove the Unix socket file on clean shutdown (no-op for Windows pipes,
/// which vanish with the last handle).
fn cleanup(socket: &str) {
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(socket);
    }
    #[cfg(windows)]
    {
        let _ = socket;
    }
}

// ---- subscriber hub ---------------------------------------------------------

/// One registered subscriber: its hub-assigned id and the sending half of its
/// connection's writer channel. Named because the bare tuple appears in the
/// hub's field type, in `retain` closures, and in `register`'s push — three
/// places where `(u64, SyncSender<String>)` says nothing about which `u64`.
type Subscriber = (u64, mpsc::SyncSender<String>);

/// A simple in-memory broadcast hub: every subscriber registers the `Sender`
/// end of its connection's writer channel, so a broadcast is just an in-memory
/// `send` per subscriber (never blocks on socket I/O).
#[derive(Clone)]
struct Hub {
    subs: Arc<Mutex<Vec<Subscriber>>>,
    next: Arc<AtomicU64>,
}

impl Hub {
    fn new() -> Self {
        Hub { subs: Arc::new(Mutex::new(Vec::new())), next: Arc::new(AtomicU64::new(1)) }
    }
    fn register(&self, tx: mpsc::SyncSender<String>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock_recover(&self.subs).push((id, tx));
        id
    }
    fn unregister(&self, id: u64) {
        lock_recover(&self.subs).retain(|(i, _)| *i != id);
    }
    /// Push a line to every subscriber. Never blocks the daemon: the send is a
    /// non-blocking `try_send` on a bounded queue. A disconnected subscriber is
    /// pruned; a *full* one keeps its slot but drops this event (bounded memory,
    /// no head-of-line blocking of the broadcaster).
    fn broadcast(&self, line: &str) {
        let mut subs = lock_recover(&self.subs);
        subs.retain(|(_, tx)| match tx.try_send(line.to_string()) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => true,
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        });
    }
}

/// Shared server state cloned into every worker and the poller.
#[derive(Clone)]
struct Shared {
    engine: Arc<Mutex<Engine>>,
    hub: Hub,
    shutdown: Arc<AtomicBool>,
    /// Highest `events.rowid` already broadcast. Guards against double-sends
    /// when the immediate (post-commit) and poll paths race.
    watermark: Arc<Mutex<i64>>,
    /// Fatal component failures are supervised by the serve loop. Tests that
    /// exercise scheduler logic without a server leave this unset.
    fatal: Option<mpsc::Sender<BackgroundFailure>>,
}

#[derive(Debug)]
struct BackgroundFailure {
    component: &'static str,
    message: String,
}

fn report_fatal(sh: &Shared, component: &'static str, error: &ApiError) {
    if let Some(tx) = &sh.fatal {
        let _ = tx.send(BackgroundFailure { component, message: error.message.clone() });
    }
}

fn accept_failure(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("listener accept failed: {error}"))
}

/// One `events` row joined to its task, as [`pump`] reads it before turning it
/// into a `task.changed` notification.
///
/// This was a bare six-field tuple. Six positional fields of which four are
/// `String`/`Option<i64>` is a shape where transposing two columns in either the
/// `SELECT` or the `query_map` still compiles and still type-checks — it just
/// broadcasts `entity_id` in the `op` field forever. Names make that a compile
/// error instead.
struct EventRow {
    rowid: i64,
    entity: String,
    entity_id: String,
    op: String,
    short_id: Option<i64>,
    rev: Option<i64>,
}

fn max_event_rowid(engine: &Engine) -> Result<i64, ApiError> {
    engine
        .conn()
        .query_row("SELECT COALESCE(MAX(rowid), 0) FROM events", [], |r| r.get(0))
        .map_err(ApiError::from)
}

/// Broadcast every `events` row newer than the watermark as a `task.changed`
/// notification, then advance the watermark. Holds the watermark lock across
/// the whole call so concurrent pumps can't emit the same row twice; the
/// engine lock is taken only briefly to read the new rows (never during the
/// broadcast).
fn pump(sh: &Shared) -> Result<(), ApiError> {
    let mut wm = lock_recover(&sh.watermark);
    let last = *wm;
    let rows: Vec<EventRow> = {
        let g = lock_recover(&sh.engine);
        let conn = g.conn();
        let mut stmt = conn.prepare(
            "SELECT e.rowid, e.entity, e.entity_id, e.op, t.short_id, t.rev \
             FROM events e LEFT JOIN tasks t ON t.id = e.entity_id \
             WHERE e.rowid > ?1 ORDER BY e.rowid",
        )?;
        let mapped = stmt.query_map([last], |r| {
            Ok(EventRow {
                rowid: r.get(0)?,
                entity: r.get(1)?,
                entity_id: r.get(2)?,
                op: r.get(3)?,
                short_id: r.get(4)?,
                rev: r.get(5)?,
            })
        })?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };
    for row in rows {
        let mut data = Map::new();
        data.insert("entity".into(), json!(row.entity));
        data.insert("entity_id".into(), json!(row.entity_id));
        data.insert("op".into(), json!(row.op));
        if let Some(s) = row.short_id {
            data.insert("short_id".into(), json!(s));
        }
        if let Some(r) = row.rev {
            data.insert("_rev".into(), json!(r));
        }
        let notif = json!({
            "tasqx": crate::API_VERSION,
            "event": "task.changed",
            "data": Value::Object(data),
        });
        sh.hub.broadcast(&format!("{notif}\n"));
        if row.rowid > *wm {
            *wm = row.rowid;
        }
    }
    Ok(())
}

// ---- server -----------------------------------------------------------------

/// Bind `socket` and serve until `shutdown` is set, delivering reminders through
/// the always-safe [`LogNotifier`]. Consumes the [`Engine`]. Blocking; run it on
/// its own thread.
///
/// The log backend is the default on purpose: `serve` is what tests and CI call,
/// and neither should ever grow an OS-notification dependency. A caller that
/// wants native toasts opts in explicitly via [`serve_with_notifier`].
pub fn serve(engine: Engine, socket: &str, shutdown: Arc<AtomicBool>) -> io::Result<()> {
    serve_with_notifier(engine, socket, shutdown, Arc::new(LogNotifier))
}

/// [`serve`], with the reminder [`Notifier`] injected — the seam the CLI uses to
/// honour `[notify] enabled` (§9) and tests use to observe delivery without an
/// OS transport.
pub fn serve_with_notifier(
    engine: Engine,
    socket: &str,
    shutdown: Arc<AtomicBool>,
    notifier: Arc<dyn Notifier>,
) -> io::Result<()> {
    let listener = bind(socket)?;
    // Non-blocking accept so the loop can observe `shutdown`; accepted streams
    // stay blocking, which the thread-per-connection model wants.
    listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

    let start = max_event_rowid(&engine)
        .map_err(|e| io::Error::other(format!("event watermark initialization failed: {}", e.message)))?;
    let (fatal_tx, fatal_rx) = mpsc::channel();
    let shared = Shared {
        engine: Arc::new(Mutex::new(engine)),
        hub: Hub::new(),
        shutdown: shutdown.clone(),
        watermark: Arc::new(Mutex::new(start)),
        fatal: Some(fatal_tx),
    };
    let admission = Arc::new(Admission::new(MAX_CONCURRENT_CLIENTS));

    // Background poller: surfaces external one-shot writes on the same DB.
    {
        let sh = shared.clone();
        let sd = shutdown.clone();
        thread::spawn(move || {
            while !sd.load(Ordering::Relaxed) {
                if let Err(e) = pump(&sh) {
                    report_fatal(&sh, "event poller", &e);
                    return;
                }
                // Sleep in small steps so shutdown stays responsive.
                for _ in 0..(POLL_MS / 50).max(1) {
                    if sd.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        });
    }

    // Reminder scheduler (§9): its own thread, so a slow notification transport
    // can never stall the accept loop or a client's dispatch.
    {
        let sh = shared.clone();
        let sd = shutdown.clone();
        thread::spawn(move || reminder_loop(sh, notifier, sd));
    }

    let result = loop {
        if shutdown.load(Ordering::Relaxed) {
            break Ok(());
        }
        if let Ok(failure) = fatal_rx.try_recv() {
            break Err(io::Error::other(format!(
                "{} failed: {}",
                failure.component, failure.message
            )));
        }
        match listener.accept() {
            Ok(stream) => {
                if let Some(permit) = admission.try_acquire() {
                    let sh = shared.clone();
                    thread::spawn(move || handle_conn(stream, sh, permit));
                } else {
                    admission.note_rejection();
                    reject_overloaded(stream);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => break Err(accept_failure(e)),
        }
    };

    // An error return is also a shutdown signal for the supervised threads;
    // otherwise the serve call would fail while its poller/scheduler leaked.
    shutdown.store(true, Ordering::Relaxed);
    cleanup(socket);
    result
}

// ---- reminder scheduler (DESIGN.md §9) --------------------------------------

/// The daemon's reminder thread: own the min-heap, the clock, and the notifier.
///
/// Heap maintenance is **rebuild-on-change**, keyed off the same append-only
/// `events` rowid the push path watermarks against. When the max rowid moves,
/// *something* changed a task — a new `remind`, a moved `due`, a completion —
/// and the heap is rebuilt from the store (two queries). That satisfies §9's
/// "updated on every event notification" while keeping exactly one code path
/// that can construct the heap: incremental patching would need to re-derive
/// per-event which task changed and how, for no measurable gain at this scale,
/// and would be a second source of truth to drift.
///
/// External writes are covered for free — the rowid moves regardless of which
/// process wrote it, so a `remind` set by a one-shot CLI lands on the heap
/// within a tick without the daemon knowing that path exists.
///
/// This is the one place a clock is read: [`ReminderScheduler::pop_ripe`] takes
/// `now` as an argument, so the ripeness decision itself stays pure and testable.
fn reminder_loop(sh: Shared, notifier: Arc<dyn Notifier>, shutdown: Arc<AtomicBool>) {
    // -1 can't be a real rowid, so the first tick always rebuilds.
    let mut seen_rowid: i64 = -1;
    let mut sched = ReminderScheduler::new();
    let mut fire_errors = ErrorTransition::default();

    while !shutdown.load(Ordering::Relaxed) {
        match reminder_tick(
            &sh,
            &mut sched,
            seen_rowid,
            jiff::Timestamp::now(),
            &*notifier,
            &scheduler::fire_one,
            &mut fire_errors,
        ) {
            Ok(next) => seen_rowid = next,
            Err(e) => {
                report_fatal(&sh, "reminder scheduler", &e);
                return;
            }
        }

        // Sleep in small steps so shutdown stays responsive.
        for _ in 0..(REMINDER_TICK_MS / 50).max(1) {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// How a ripe reminder is recorded. Only ever [`scheduler::fire_one`] in
/// production; the indirection exists so tests can inject a failing write and
/// pin the recovery behaviour of the error path, which is otherwise reachable
/// only through disk/lock faults that cannot be provoked from a test.
type FireFn = dyn Fn(&Engine, &scheduler::Pending) -> Result<bool, ApiError> + Send + Sync;

#[derive(Default)]
struct ErrorTransition {
    current: Option<String>,
}

impl ErrorTransition {
    fn enter(&mut self, message: String) -> bool {
        if self.current.as_deref() == Some(message.as_str()) {
            return false;
        }
        self.current = Some(message);
        true
    }

    fn report(&mut self, message: String) {
        if self.enter(message.clone()) {
            eprintln!("tasqx daemon: {message}");
        }
    }

    fn recovered(&mut self) {
        self.current = None;
    }
}

/// One tick of [`reminder_loop`]: rebuild if the log moved, fire what is ripe.
/// Returns the watermark to carry into the next tick. `now` is injected, so a
/// test drives ripeness by argument and never sleeps.
///
/// **The returned watermark may only ever be a rowid the heap was actually built
/// from** (or `-1`, meaning "rebuild unconditionally next tick"). Advancing it to
/// a rowid read at any *later* point silently swallows writes the heap never saw:
/// an external `task.add` committing during the fire/notify window would be
/// marked seen without being scheduled, and its reminder would never fire. That
/// is why there is no post-fire adoption of `max_event_rowid` here — the daemon's
/// own `reminded` rows do cost one extra rebuild on the next tick, but a rebuild
/// is two queries and is idempotent (`rebuild` filters already-reminded pairs via
/// `reminded_keys`), so re-seeing our own writes can never re-fire anything.
fn reminder_tick(
    sh: &Shared,
    sched: &mut ReminderScheduler,
    seen_rowid: i64,
    now: jiff::Timestamp,
    notifier: &dyn Notifier,
    fire: &FireFn,
    fire_errors: &mut ErrorTransition,
) -> Result<i64, ApiError> {
    let mut seen = seen_rowid;

    // 1. Rebuild if the event log moved (start, or any task change).
    let cur = {
        let g = lock_recover(&sh.engine);
        max_event_rowid(&g)?
    };
    if cur != seen {
        let rebuilt = {
            let g = lock_recover(&sh.engine);
            ReminderScheduler::rebuild(&g)
        }?;
        *sched = rebuilt;
        seen = cur;
    }

    // 2. Fire everything ripe. The engine lock is held only for the event
    //    write and released before notifying — a D-Bus/WinRT call can block
    //    for a long time and must never serialize other clients.
    let ripe = sched.pop_ripe(now);
    let mut fired_any = false;
    for p in &ripe {
        let fired = {
            let g = lock_recover(&sh.engine);
            fire(&g, p)
        };
        match fired {
            // Deduped (already reminded) => do NOT notify, but it still proves
            // the previously failing store operation recovered.
            Ok(fired) => {
                fire_errors.recovered();
                if fired {
                    fired_any = true;
                    notifier.notify(&scheduler::notification_for(p));
                }
            }
            Err(e) => {
                fire_errors.report(format!(
                    "could not record reminder for #{}: {}",
                    p.short_id, e.message
                ));
                // `pop_ripe` already took this entry off the heap and the failed
                // write left no event behind, so the watermark would otherwise
                // still match and the next tick would skip the rebuild — dropping
                // the reminder forever. Invalidate instead: the store is the
                // source of truth and will re-schedule it (or filter it, if the
                // write did land after all).
                seen = -1;
            }
        }
    }
    // Push the `reminded` rows to subscribers immediately rather than waiting
    // for the poller — this is the headless verification surface (§9).
    if fired_any {
        pump(sh)?;
    }
    Ok(seen)
}

/// One connection: a reader loop (this thread) + a writer thread draining a
/// single channel, so responses and pushes never interleave.
#[cfg(windows)]
fn start_connection_watchdog(
    recv: &RecvHalf,
    send: &SendHalf,
    shutdown: &Arc<AtomicBool>,
    state: &Arc<ConnectionIoState>,
) -> thread::JoinHandle<()> {
    use std::os::windows::io::{AsHandle, AsRawHandle};

    let RecvHalf::NamedPipe(recv) = recv;
    let SendHalf::NamedPipe(send) = send;
    let recv_handle = recv.as_handle().as_raw_handle() as usize;
    let send_handle = send.as_handle().as_raw_handle() as usize;
    let shutdown = shutdown.clone();
    let state = state.clone();
    thread::spawn(move || {
        while !state.done.load(Ordering::Acquire) {
            let now = Instant::now();
            if shutdown.load(Ordering::Relaxed) || state.idle(now) {
                cancel_io(recv_handle);
                cancel_io(send_handle);
            } else if state.write_timed_out(now) {
                cancel_io(send_handle);
            }
            thread::sleep(CLIENT_WATCHDOG_INTERVAL);
        }
    })
}

#[cfg(windows)]
fn cancel_io(raw_handle: usize) {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::IO::CancelIoEx;

    // SAFETY: the connection owns this handle until its watchdog joins. A null
    // OVERLAPPED pointer asks Windows to cancel every pending operation on it.
    let _ = unsafe { CancelIoEx(raw_handle as HANDLE, std::ptr::null()) };
}

fn handle_conn(stream: Stream, sh: Shared, _permit: ClientPermit) {
    #[cfg(unix)]
    if stream.set_recv_timeout(Some(CLIENT_IO_POLL_TIMEOUT)).is_err()
        || stream.set_send_timeout(Some(CLIENT_SEND_TIMEOUT)).is_err()
    {
        return;
    }
    let (recv, send) = stream.split();
    let io_state = Arc::new(ConnectionIoState::new());
    #[cfg(windows)]
    let watchdog = start_connection_watchdog(&recv, &send, &sh.shutdown, &io_state);
    // Bounded queue: a subscriber that stops reading can't grow memory without
    // bound (see OUT_QUEUE_CAP). Responses use the blocking `send` (only ever
    // stalls this one connection); broadcasts use non-blocking `try_send`.
    let (out_tx, out_rx) = mpsc::sync_channel::<String>(OUT_QUEUE_CAP);

    let writer_shutdown = sh.shutdown.clone();
    let writer_state = io_state.clone();
    let writer = thread::spawn(move || {
        let mut send: SendHalf = send;
        while let Ok(line) = out_rx.recv() {
            if writer_shutdown.load(Ordering::Relaxed) {
                break;
            }
            writer_state.begin_write();
            let result = send.write_all(line.as_bytes()).and_then(|_| send.flush());
            writer_state.end_write();
            if result.is_err() {
                break;
            }
        }
    });

    let mut sub_id: Option<u64> = None;
    let mut reader = BufReader::new(recv);
    let mut line = String::new();
    let mut frame_bytes = Vec::new();
    loop {
        line.clear();
        match read_frame_capped(&mut reader, &mut frame_bytes, &mut line, MAX_FRAME_BYTES) {
            Ok(0) => break, // EOF: client closed.
            Ok(_) => {
                io_state.record_activity();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // `subscribe` is a transport-level verb, not a core method: it
                // registers this connection for pushes and acks.
                if let Some(ack) = try_subscribe(trimmed, &sh, &out_tx, &mut sub_id) {
                    if out_tx.send(ack).is_err() {
                        break;
                    }
                    continue;
                }
                // Lock ONLY around dispatch; never across the socket write. A
                // panic inside dispatch is caught and turned into a per-client
                // `internal` error — it must never take down the daemon (and the
                // poison-recovering lock keeps a mid-panic guard drop from
                // wedging every other client).
                let resp = {
                    let g = lock_recover(&sh.engine);
                    match panic::catch_unwind(AssertUnwindSafe(|| handle_envelope(&g, trimmed))) {
                        Ok(v) => v,
                        Err(_) => internal_error_envelope(trimmed),
                    }
                };
                let out = format!("{}\n", serde_json::to_string(&resp).unwrap_or_default());
                if out_tx.send(out).is_err() {
                    break;
                }
                // Low-latency push for a write we just committed (idempotent with
                // the poller via the shared watermark). Skip it for pure reads:
                // they emit no events, so pumping only re-locks watermark+engine
                // and runs the JOIN for nothing.
                if !is_read_method(trimmed) {
                    if let Err(e) = pump(&sh) {
                        report_fatal(&sh, "event pump", &e);
                        break;
                    }
                }
            }
            Err(e)
                if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) =>
            {
                if sh.shutdown.load(Ordering::Relaxed)
                    || io_state.idle(Instant::now())
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    if let Some(id) = sub_id {
        sh.hub.unregister(id);
    }
    drop(out_tx); // closes the channel → writer thread exits.
    let _ = writer.join();
    io_state.done.store(true, Ordering::Release);
    #[cfg(windows)]
    let _ = watchdog.join();
}

/// Reject overload before allocating worker threads or a per-client queue.
/// The id-less response is transport-level: the accept loop deliberately does
/// not read a request while overloaded because that would let one peer stall
/// admission for everyone else.
fn reject_overloaded(mut stream: Stream) {
    #[cfg(unix)]
    let _ = stream.set_send_timeout(Some(Duration::from_millis(100)));
    #[cfg(windows)]
    let _ = stream.set_nonblocking(true);
    let response = json!({
        "tasqx": crate::API_VERSION,
        "ok": false,
        "error": {
            "code": "unavailable",
            "message": format!(
                "daemon client limit ({MAX_CONCURRENT_CLIENTS}) reached; retry after a client disconnects"
            ),
        },
    });
    let _ = writeln!(stream, "{response}");
    let _ = stream.flush();
}

/// Read one newline-terminated frame into `buf`, but refuse to buffer more than
/// `max` bytes for a single frame (returns `InvalidData` past the cap instead of
/// growing memory unbounded for a client that never sends `\n`). Returns the
/// number of bytes read (0 on EOF), mirroring `BufRead::read_line`.
fn read_frame_capped<R: BufRead>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
    buf: &mut String,
    max: usize,
) -> io::Result<usize> {
    buf.clear();
    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            let len = bytes.len();
            buf.push_str(&String::from_utf8_lossy(bytes));
            bytes.clear();
            return Ok(len);
        }
        if let Some(i) = available.iter().position(|&b| b == b'\n') {
            if bytes.len() + i + 1 > max {
                reader.consume(i + 1);
                bytes.clear();
                return Err(io::Error::new(io::ErrorKind::InvalidData, "request frame exceeds limit"));
            }
            bytes.extend_from_slice(&available[..=i]);
            reader.consume(i + 1);
            let len = bytes.len();
            buf.push_str(&String::from_utf8_lossy(bytes));
            bytes.clear();
            return Ok(len);
        }
        if bytes.len() + available.len() > max {
            let n = available.len();
            reader.consume(n);
            bytes.clear();
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request frame exceeds limit"));
        }
        let n = available.len();
        bytes.extend_from_slice(available);
        reader.consume(n);
    }
}

/// True if `line` is a request whose method never appends to the `events` log,
/// so the post-dispatch `pump` can be skipped. Anything unrecognized (including
/// unparseable input) is treated as a potential writer and still pumps.
fn is_read_method(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("method").and_then(Value::as_str).map(str::to_string))
        .map(|m| {
            matches!(
                m.as_str(),
                "task.list"
                    | "task.get"
                    | "project.list"
                    | "report.summary"
                    | "store.export"
                    | "event.list"
                    | "core.capabilities"
            )
        })
        .unwrap_or(false)
}

/// A well-formed `internal` error envelope for a request whose dispatch panicked,
/// correlated to the request `id` when it can be recovered from the raw line.
fn internal_error_envelope(line: &str) -> Value {
    let id = serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(Value::Null);
    let mut m = Map::new();
    m.insert("tasqx".into(), json!(crate::API_VERSION));
    if !id.is_null() {
        m.insert("id".into(), id);
    }
    m.insert("ok".into(), json!(false));
    m.insert(
        "error".into(),
        json!({ "code": "internal", "message": "internal error: request handler panicked" }),
    );
    Value::Object(m)
}

/// If `line` is a `subscribe` request, register the connection and return an
/// ack envelope line; otherwise `None`.
fn try_subscribe(
    line: &str,
    sh: &Shared,
    out_tx: &mpsc::SyncSender<String>,
    sub_id: &mut Option<u64>,
) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("method").and_then(Value::as_str) != Some("subscribe") {
        return None;
    }
    if sub_id.is_none() {
        *sub_id = Some(sh.hub.register(out_tx.clone()));
    }
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let mut m = Map::new();
    m.insert("tasqx".into(), json!(crate::API_VERSION));
    if !id.is_null() {
        m.insert("id".into(), id);
    }
    m.insert("ok".into(), json!(true));
    m.insert("result".into(), json!({ "subscribed": true }));
    Some(format!("{}\n", Value::Object(m)))
}

// ---- client -----------------------------------------------------------------

/// A connected client. Wraps the split stream so a single connection can both
/// issue request/response calls and read unsolicited event pushes.
pub struct Conn {
    reader: BufReader<RecvHalf>,
    writer: SendHalf,
    id: u64,
    /// Event pushes observed while a request waits for its correlated response.
    /// `next_frame` drains these first so TTY watch refreshes again and stream
    /// watch emits every event rather than losing the notification.
    pending_events: VecDeque<Value>,
    /// Response envelopes for request IDs other than the active request.
    pending_responses: VecDeque<Value>,
}

/// Try to connect to a daemon at `socket`. Returns `None` immediately (no
/// hang) if nothing is listening — the CLI uses this to fall back to the
/// in-process path.
pub fn try_connect(socket: &str) -> Option<Conn> {
    let stream = connect_stream(socket).ok()?;
    let (recv, send) = stream.split();
    Some(Conn {
        reader: BufReader::new(recv),
        writer: send,
        id: 0,
        pending_events: VecDeque::new(),
        pending_responses: VecDeque::new(),
    })
}

impl Conn {
    /// Write one framed line (appends the newline if absent).
    pub fn send_line(&mut self, line: &str) -> io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            self.writer.write_all(b"\n")?;
        }
        self.writer.flush()
    }

    /// Read one line; `Ok(None)` on EOF (daemon gone).
    pub fn read_line(&mut self) -> io::Result<Option<String>> {
        let mut s = String::new();
        let n = self.reader.read_line(&mut s)?;
        if n == 0 {
            Ok(None)
        } else {
            Ok(Some(s))
        }
    }

    /// Read and classify one frame directly from the transport, without
    /// consulting either retained inbox.
    fn read_wire_frame(&mut self) -> io::Result<Option<Frame>> {
        loop {
            match self.read_line()? {
                None => return Ok(None),
                Some(l) => {
                    let t = l.trim();
                    if t.is_empty() {
                        continue;
                    }
                    let v: Value = serde_json::from_str(t)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    if v.get("event").is_some() {
                        return Ok(Some(Frame::Event(v)));
                    }
                    return Ok(Some(Frame::Response(v)));
                }
            }
        }
    }

    /// Surface retained events first, then unrelated responses, then new wire
    /// input. Events take priority because each one is a dirty signal for TTY
    /// watch and an individually emitted record for non-TTY watch.
    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        if let Some(v) = self.pending_events.pop_front() {
            return Ok(Some(Frame::Event(v)));
        }
        if let Some(v) = self.pending_responses.pop_front() {
            return Ok(Some(Frame::Response(v)));
        }
        self.read_wire_frame()
    }

    /// Send a request envelope and return the correlated response envelope,
    /// retaining event pushes and responses for other IDs that arrive while
    /// waiting. Safe to call on a subscribed connection.
    pub fn request(&mut self, method: &str, params: &Value) -> io::Result<Value> {
        self.id += 1;
        let request_id = json!(self.id);
        let env = json!({
            "tasqx": crate::API_VERSION,
            "id": request_id,
            "method": method,
            "params": params,
        });
        self.send_line(&env.to_string())?;

        if let Some(i) = self
            .pending_responses
            .iter()
            .position(|response| response.get("id") == Some(&request_id))
        {
            return Ok(self.pending_responses.remove(i).expect("position came from this queue"));
        }

        loop {
            match self.read_wire_frame()? {
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "daemon closed the connection",
                    ))
                }
                Some(Frame::Event(v)) => self.pending_events.push_back(v),
                Some(Frame::Response(v)) if v.get("id") == Some(&request_id) => return Ok(v),
                Some(Frame::Response(v))
                    if v.get("id").is_none() && v.get("ok") == Some(&Value::Bool(false)) =>
                {
                    let message = v
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("daemon refused the connection")
                        .to_string();
                    return Err(io::Error::new(io::ErrorKind::ConnectionRefused, message));
                }
                Some(Frame::Response(v)) => self.pending_responses.push_back(v),
            }
        }
    }

    /// Subscribe to live `task.changed` pushes on this connection.
    pub fn subscribe(&mut self) -> io::Result<()> {
        self.request("subscribe", &json!({})).map(|_| ())
    }
}

/// A line read from the daemon: either an unsolicited event or a response.
pub enum Frame {
    Event(Value),
    Response(Value),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::Pending;
    use jiff::Timestamp;
    use std::sync::atomic::AtomicUsize;

    static CLIENT_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn client_test_socket(label: &str) -> String {
        let n = CLIENT_TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        if cfg!(windows) {
            format!("tasqx-client-{label}-{}-{n}", std::process::id())
        } else {
            std::env::temp_dir()
                .join(format!("tasqx-client-{label}-{}-{n}.sock", std::process::id()))
                .to_string_lossy()
                .into_owned()
        }
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn admission_never_exceeds_its_limit_and_release_reopens_a_slot() {
        let admission = Arc::new(Admission::new(2));
        let first = admission.try_acquire().expect("first slot");
        let second = admission.try_acquire().expect("second slot");
        assert!(admission.try_acquire().is_none(), "the third client must be refused");
        assert_eq!(admission.active(), 2);

        drop(first);
        let replacement = admission.try_acquire().expect("released slot is reusable");
        assert_eq!(admission.active(), 2);
        drop((second, replacement));
        assert_eq!(admission.active(), 0);
    }

    #[test]
    fn idle_deadline_expires_at_the_boundary() {
        let started = std::time::Instant::now();
        assert!(!idle_expired(started, started + CLIENT_IDLE_TIMEOUT - Duration::from_millis(1)));
        assert!(idle_expired(started, started + CLIENT_IDLE_TIMEOUT));
    }

    fn shared(engine: Engine) -> Shared {
        Shared {
            engine: Arc::new(Mutex::new(engine)),
            hub: Hub::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            watermark: Arc::new(Mutex::new(0)),
            fatal: None,
        }
    }

    /// Records deliveries, so a test asserts on the notification surface itself
    /// rather than on internal bookkeeping.
    #[derive(Default)]
    struct Collecting(Mutex<Vec<i64>>);

    impl Notifier for Collecting {
        fn notify(&self, n: &crate::notify::Notification) {
            lock_recover(&self.0).push(n.short_id);
        }
    }

    impl Collecting {
        fn fired(&self) -> Vec<i64> {
            lock_recover(&self.0).clone()
        }
    }

    fn add(sh: &Shared, title: &str, due: &str, remind: &str) -> i64 {
        let g = lock_recover(&sh.engine);
        let r = g
            .task_add(&json!({ "title": title, "due": due, "remind": remind }))
            .unwrap();
        r.get("short_id").and_then(Value::as_i64).unwrap()
    }

    #[test]
    fn pump_decode_failure_does_not_advance_the_watermark() {
        let engine = Engine::open_in_memory().unwrap();
        engine.task_add(&json!({ "title": "bad event join" })).unwrap();
        engine
            .conn()
            .execute("UPDATE tasks SET rev = 'not-an-integer'", [])
            .unwrap();
        let sh = shared(engine);

        let err = pump(&sh).expect_err("the malformed joined row must be surfaced");
        assert!(err.message.contains("storage error"), "{err:?}");
        assert_eq!(*lock_recover(&sh.watermark), 0, "no failed batch may advance the watermark");
    }

    #[test]
    fn repeated_transient_failures_log_only_on_state_transitions() {
        let mut state = ErrorTransition::default();
        assert!(state.enter("disk busy".to_string()));
        assert!(!state.enter("disk busy".to_string()), "the same failure must be rate-limited");
        assert!(state.enter("disk I/O".to_string()), "a changed failure is observable");
        state.recovered();
        assert!(state.enter("disk I/O".to_string()), "re-entering after recovery is observable");
    }

    #[test]
    fn non_would_block_accept_errors_are_contextual_fatal_errors() {
        let source = io::Error::new(io::ErrorKind::ConnectionAborted, "listener broke");
        let fatal = accept_failure(source);
        assert_eq!(fatal.kind(), io::ErrorKind::ConnectionAborted);
        assert!(fatal.to_string().contains("listener accept failed"));
    }

    #[test]
    fn request_correlates_responses_and_retains_events_for_the_next_refresh() {
        let socket = client_test_socket("retained-event");
        let listener = bind(&socket).expect("bind scripted server");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept scripted client");
            let (recv, mut send) = stream.split();
            let mut reader = BufReader::new(recv);
            let mut line = String::new();

            reader.read_line(&mut line).expect("first request");
            let first: Value = serde_json::from_str(line.trim()).expect("first request JSON");
            let first_id = first["id"].clone();

            writeln!(
                send,
                "{}",
                json!({ "event": "task.changed", "data": { "op": "add", "short_id": 2 } })
            )
            .expect("event");
            writeln!(
                send,
                "{}",
                json!({ "tasqx": crate::API_VERSION, "id": 999, "ok": true, "result": { "tasks": ["wrong response"] } })
            )
            .expect("unrelated response");
            writeln!(
                send,
                "{}",
                json!({ "tasqx": crate::API_VERSION, "id": first_id, "ok": true, "result": { "tasks": ["first"] } })
            )
            .expect("first response");
            send.flush().expect("flush first frames");

            line.clear();
            reader.read_line(&mut line).expect("second request");
            let second: Value = serde_json::from_str(line.trim()).expect("second request JSON");
            writeln!(
                send,
                "{}",
                json!({
                    "tasqx": crate::API_VERSION,
                    "id": second["id"].clone(),
                    "ok": true,
                    "result": { "tasks": ["first", "second"] },
                })
            )
            .expect("second response");
            send.flush().expect("flush second response");
        });

        let mut conn = try_connect(&socket).expect("connect scripted client");
        let stale = conn.request("task.list", &json!({})).expect("first list");
        assert_eq!(stale["id"], 1, "request must ignore a response for another ID");
        assert_eq!(stale["result"]["tasks"], json!(["first"]));

        let retained = conn.next_frame().expect("retained frame").expect("event frame");
        assert!(matches!(retained, Frame::Event(ref event) if event["data"]["short_id"] == 2));

        let fresh = conn.request("task.list", &json!({})).expect("second list");
        assert_eq!(fresh["id"], 2);
        assert_eq!(
            fresh["result"]["tasks"],
            json!(["first", "second"]),
            "the retained event drives a refresh whose state includes the second change"
        );

        server.join().expect("scripted server");
        cleanup(&socket);
    }

    /// A failed write must not consume the reminder. `pop_ripe` has already taken
    /// the entry off the heap, and a fire that fails leaves no event behind — so
    /// if the tick still reported "nothing changed", the next tick would skip the
    /// rebuild and the reminder would be lost forever, in neither the heap nor the
    /// store's reminded set. It must be retried instead.
    #[test]
    fn a_failed_fire_is_retried_on_the_next_tick_rather_than_dropped() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let id = add(&sh, "ship it", "2026-07-20T17:00:00Z", "-1h"); // ripens 16:00Z
        let notifier = Collecting::default();
        let mut sched = ReminderScheduler::new();
        let mut fire_errors = ErrorTransition::default();
        let now = ts("2026-07-20T16:00:00Z");

        // Tick 1: the write fails the way a disk error / busy-timeout would —
        // no event row is written, so the event rowid does NOT move.
        let calls = AtomicUsize::new(0);
        let flaky = move |e: &Engine, p: &Pending| -> Result<bool, ApiError> {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(ApiError::internal("disk I/O error"));
            }
            scheduler::fire_one(e, p)
        };
        let seen = reminder_tick(
            &sh,
            &mut sched,
            -1,
            now,
            &notifier,
            &flaky,
            &mut fire_errors,
        )
        .unwrap();
        assert!(notifier.fired().is_empty(), "a failed write must not notify");
        assert_eq!(seen, -1, "a failed fire must force the next tick to rebuild");

        // Tick 2: nothing external happened. The reminder must still come back.
        let seen = reminder_tick(
            &sh,
            &mut sched,
            seen,
            now,
            &notifier,
            &flaky,
            &mut fire_errors,
        )
        .unwrap();
        assert_eq!(notifier.fired(), vec![id], "the reminder must fire after a retry");
        assert_ne!(seen, -1, "a clean tick re-establishes the watermark");
    }

    /// The watermark may only ever advance to a rowid the heap was actually built
    /// from. Adopting the max rowid *after* firing swallows any write that landed
    /// during the fire/notify window: that task is marked seen without ever being
    /// scheduled, so its reminder never fires. The fire closure below commits
    /// exactly in that window — which is what a client dispatch thread does.
    #[test]
    fn a_write_landing_during_the_fire_window_is_still_scheduled() {
        let sh = shared(Engine::open_in_memory().unwrap());
        add(&sh, "ship it", "2026-07-20T17:00:00Z", "-1h"); // ripens 16:00Z
        let notifier = Collecting::default();
        let mut sched = ReminderScheduler::new();
        let mut fire_errors = ErrorTransition::default();
        let now = ts("2026-07-20T16:00:00Z");

        // Fires for real, then — still inside the tick, exactly as a concurrent
        // client would — commits a brand-new task whose reminder is already ripe.
        // This task was never on the heap the tick was built from.
        let injected = AtomicUsize::new(0);
        let racing = move |e: &Engine, p: &Pending| -> Result<bool, ApiError> {
            let out = scheduler::fire_one(e, p);
            if injected.fetch_add(1, Ordering::Relaxed) == 0 {
                e.task_add(&json!({
                    "title": "pay rent",
                    "due": "2026-07-20T17:00:00Z",
                    "remind": "-2h",
                }))
                .unwrap();
            }
            out
        };
        let seen = reminder_tick(
            &sh,
            &mut sched,
            -1,
            now,
            &notifier,
            &racing,
            &mut fire_errors,
        )
        .unwrap();

        // The tick must NOT claim to have seen the racing write.
        let cur = {
            let g = lock_recover(&sh.engine);
            max_event_rowid(&g).unwrap()
        };
        assert_ne!(seen, cur, "must not adopt a rowid the heap was never built from");

        // Next tick: "pay rent" (ripe at 15:00Z) must be picked up and fire.
        reminder_tick(
            &sh,
            &mut sched,
            seen,
            now,
            &notifier,
            &scheduler::fire_one,
            &mut fire_errors,
        )
        .unwrap();
        let fired = notifier.fired();
        assert!(
            fired.contains(&2),
            "a reminder created during the fire window must still fire, got {fired:?}",
        );
    }
}
