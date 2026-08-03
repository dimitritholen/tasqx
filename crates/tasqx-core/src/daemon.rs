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
//!    global admission cap bounds all three thread classes. On shutdown the
//!    reader refuses further requests with a transport-level `unavailable`
//!    frame and the writer drains what is already queued, so stopping the
//!    daemon can never commit a mutation it then fails to answer.
//!  * Live event push (§2, §6a): a client sends `{"method":"subscribe"}` and
//!    thereafter receives unsolicited `{"event":"task.changed",...}` frames. A
//!    subscriber whose bounded queue overflows is told so — a
//!    `task.changed.gap` frame naming the number of lost events precedes the
//!    next one it can receive — because for a non-redrawing consumer a silent
//!    drop is unrecoverable.
//!    Change detection is unified through a single watermark over the
//!    append-only `events` rowid: mutations that arrive through the daemon are
//!    pumped immediately after commit, and a background poller (~400 ms) picks
//!    up EXTERNAL one-shot writes from another process on the same DB. Both
//!    paths advance the same watermark under one lock, so every event is
//!    broadcast exactly once regardless of which path observes it first.
//!  * Reminders (§9): a third thread owns the [`ReminderScheduler`] min-heap and
//!    the notifier. A ripe reminder writes its `reminded` event and is pumped
//!    like any other — so the reminder's verification surface is the ordinary
//!    event stream, not the OS notification. See `reminder_loop` (private: the
//!    thread body is an implementation detail of [`serve`], not a surface).
//!  * Idle shutdown (D5, opt-in): with no admitted client, no subscriber and no
//!    work in hand, a daemon may exit on its own after
//!    [`DaemonOptions::idle_timeout`]. It is a *server* clock, unrelated to the
//!    per-connection `CLIENT_IDLE_TIMEOUT`, and it stops the daemon through the
//!    same shutdown flag Ctrl-C sets — never `exit`. The decision itself is two
//!    pure predicates, `server_busy` and `idle_shutdown_due` (private, like the
//!    loops above: the policy is assertable in-crate, not a surface).

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use interprocess::local_socket::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::{
    Listener, ListenerNonblockingMode, ListenerOptions, RecvHalf, SendHalf, Stream,
};

use crate::attribution;
use crate::dispatch::handle_envelope;
use crate::engine::Engine;
use crate::error::ApiError;
use crate::notify::{LogNotifier, Notifier};
use crate::otlp;
use crate::scheduler::{self, ReminderScheduler};

/// Poll interval for detecting external (out-of-daemon) writes.
const POLL_MS: u64 = 400;

/// Reminder scheduler tick. Reminders are human-scale, so ~200 ms of latency is
/// imperceptible while keeping the idle thread essentially free.
const REMINDER_TICK_MS: u64 = 200;

/// Token-attribution tick (#17). Attribution is post-hoc bookkeeping, not
/// interactive, so a slower cadence than reminders keeps the idle thread cheap;
/// a transient failure (a transcript that has not flushed yet) simply retries on
/// the next tick.
const ATTRIBUTION_TICK_MS: u64 = 500;

/// Per-connection outbound queue depth. Responses *and* event pushes share this
/// bounded channel, so a subscriber that stops reading its socket can never make
/// daemon memory grow without bound: broadcasts to a full queue are dropped and
/// the subscriber misses events until it drains.
///
/// This comment used to call that self-healing because "`watch` re-renders the
/// whole working set on the next event it does receive". That holds for the TTY
/// branch only. The non-TTY branch emits one line per event and never resyncs,
/// so for it a drop is unrecoverable data loss, not a coalesced redraw — which
/// is why [`Hub::broadcast`] announces the gap instead of hiding it. A bulk
/// import through the external-writer path (uncapped by `MAX_FRAME_BYTES`) plus
/// a slow consumer is enough to trigger it in practice.
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
/// How long ONE connection may hold its admission slot without sending a byte.
///
/// A per-connection deadline, and deliberately not the server-level one D5 asks
/// for: this fires while the daemon is busy serving fifty other clients, and
/// [`idle_shutdown_due`] fires only when there is no connection left to have a
/// deadline. Conflating them would either tear down live connections at the
/// server timeout or keep the daemon alive because one client is talking.
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
        Self {
            active: AtomicUsize::new(0),
            limit,
            rejected: AtomicU64::new(0),
        }
    }

    fn try_acquire(self: &Arc<Self>) -> Option<ClientPermit> {
        self.active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .ok()
            .map(|_| ClientPermit {
                admission: self.clone(),
            })
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

    /// How many clients hold a permit right now.
    ///
    /// It was `#[cfg(test)]` until the idle-shutdown check (D5) had to ask it in
    /// production: "is anybody connected" is exactly the permit count, and a
    /// second tally kept beside it would be one more thing to get out of step
    /// with [`ClientPermit`]'s `Drop`. The permit is the LAST thing dropped in
    /// `handle_conn` — after the hub unregister and after the writer thread is
    /// joined — so a zero here also means no connection is still flushing.
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
    /// Set by [`writer_loop`] when — and *only* when — it stops because a write
    /// to the socket failed.
    ///
    /// Without it a dead writer was invisible to the reader until the reader's
    /// next `out_tx.send(...)`, which needs the client to send another request;
    /// a client left hanging on the half-frame the failed write truncated sends
    /// nothing, so the connection survived as a zombie until the 15-minute
    /// [`CLIENT_IDLE_TIMEOUT`] — holding one of [`MAX_CONCURRENT_CLIENTS`]
    /// slots and a live hub subscription the whole time, with the client
    /// hearing silence instead of a transport error.
    ///
    /// The distinction from the writer's *other* exit is the entire point: the
    /// channel closing means the reader already finished and dropped `out_tx`,
    /// which is the healthy wind-down of every connection that ever ends. A
    /// flag set on both paths would ask each connection to tear itself down at
    /// the moment it is already tearing itself down — harmless-looking, and it
    /// would make this flag useless as evidence of anything.
    write_failed: AtomicBool,
}

impl ConnectionIoState {
    fn new() -> Self {
        Self {
            last_activity: Mutex::new(Instant::now()),
            write_started: Mutex::new(None),
            done: AtomicBool::new(false),
            write_failed: AtomicBool::new(false),
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

    /// Whether the writer is inside `write_all`/`flush` right now.
    ///
    /// Only the Windows watchdog asks, and it asks because on that platform a
    /// cancel cannot be aimed: see [`recv_cancel_due`].
    #[cfg(windows)]
    fn write_in_flight(&self) -> bool {
        lock_recover(&self.write_started).is_some()
    }

    fn note_write_failure(&self) {
        self.write_failed.store(true, Ordering::Release);
    }

    fn write_failed(&self) -> bool {
        self.write_failed.load(Ordering::Acquire)
    }

    /// Spawn this connection's writer thread against *this* state.
    ///
    /// The wiring is the whole point of the method existing. `handle_conn` used
    /// to clone the `Arc` into a local and hand that to `thread::spawn` itself,
    /// which left one unremarkable line — `let writer_state = io_state.clone()`
    /// — carrying everything: swap it for a fresh `ConnectionIoState` and
    /// [`note_write_failure`] writes to an object nobody reads, both consumers
    /// see `false` forever, and the connection is a zombie for the full
    /// [`CLIENT_IDLE_TIMEOUT`] again. Nothing catches that. Every test builds
    /// its own state, so they all keep passing; both call sites still consult
    /// the policy, so the call-site guard keeps passing; both items are still
    /// used, so `-D warnings` stays quiet. Taking `self` deletes the
    /// substitution instead of guarding it — there is no second state to name.
    ///
    /// [`note_write_failure`]: Self::note_write_failure
    fn spawn_writer<W: Write + Send + 'static>(
        self: &Arc<Self>,
        mut send: W,
        out_rx: mpsc::Receiver<String>,
    ) -> thread::JoinHandle<()> {
        let state = self.clone();
        thread::spawn(move || writer_loop(&mut send, &out_rx, &state))
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
        if let Err(error) = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        {
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

/// One registered subscriber: its hub-assigned id, the sending half of its
/// connection's writer channel, and how many broadcasts its full queue has
/// swallowed since the last frame it could actually receive.
///
/// This was a `(u64, SyncSender<String>)` alias, which said nothing about which
/// `u64` — and had nowhere to keep the drop count that makes a lost event
/// reportable instead of merely survivable.
struct Subscriber {
    id: u64,
    tx: mpsc::SyncSender<String>,
    /// Events dropped since this subscriber last accepted a frame. Cleared only
    /// when the gap marker carrying the count is actually queued, so the debt
    /// can never be lost by the very congestion that created it.
    dropped: u64,
}

/// The frame that tells a subscriber it missed `dropped` events.
///
/// A new event name is additive under the frozen `"tasqx":"1"` API (DESIGN.md
/// §6a) and `subscribe` is all-or-nothing, so no per-event filtering has to
/// learn about it. `op` is redundant for a reader that switches on `event`; it
/// is here because the shipping non-TTY `watch` arm does not switch on the
/// event name yet and prints `task.changed op=<data.op or "change">` for any
/// frame with an `event` key. Without it the marker would render as a
/// counterfeit `task.changed` line and make the stream strictly worse than the
/// silence it replaces. Drop the field once that arm reads `event`.
fn gap_notification(dropped: u64) -> String {
    let notif = json!({
        "tasqx": crate::API_VERSION,
        "event": "task.changed.gap",
        "data": { "op": "gap", "dropped": dropped },
    });
    format!("{notif}\n")
}

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
        Hub {
            subs: Arc::new(Mutex::new(Vec::new())),
            next: Arc::new(AtomicU64::new(1)),
        }
    }
    fn register(&self, tx: mpsc::SyncSender<String>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock_recover(&self.subs).push(Subscriber { id, tx, dropped: 0 });
        id
    }
    fn unregister(&self, id: u64) {
        lock_recover(&self.subs).retain(|s| s.id != id);
    }
    /// How many subscribers are registered right now.
    ///
    /// Asked by the idle-shutdown check (D5). A subscriber always sits on an
    /// admitted connection today, so this cannot exceed the permit count — but
    /// the two answer different questions, and the one that matters here is
    /// this one: a subscriber is a consumer that would silently stop receiving
    /// `task.changed` frames if the daemon left, which the permit count
    /// describes only by accident of how registration happens to be wired.
    fn subscribers(&self) -> usize {
        lock_recover(&self.subs).len()
    }
    /// Push a line to every subscriber. Never blocks the daemon: the send is a
    /// non-blocking `try_send` on a bounded queue. A disconnected subscriber is
    /// pruned; a *full* one keeps its slot and misses this event (bounded
    /// memory, no head-of-line blocking of the broadcaster).
    ///
    /// A miss is **counted and then announced**, because a silent drop is not
    /// recoverable for every consumer: only the full-redraw (TTY) `watch` branch
    /// re-reads the whole working set on the next event it does receive. The
    /// non-TTY branch emits exactly one line per event and never resyncs, so a
    /// dropped row there is a permanently missing record in a stream scripts
    /// tally — with nothing in the stream to attribute the difference to. The
    /// marker goes out immediately *before* the first frame the subscriber can
    /// take again, so the gap is always reported at the position it happened.
    ///
    /// `retain_mut`, not `retain`: the closure has to update `dropped`.
    fn broadcast(&self, line: &str) {
        let mut subs = lock_recover(&self.subs);
        subs.retain_mut(|sub| {
            if sub.dropped > 0 {
                match sub.tx.try_send(gap_notification(sub.dropped)) {
                    Ok(()) => {
                        eprintln!(
                            "tasqx daemon: subscriber {} resumed after dropping {} event(s)",
                            sub.id, sub.dropped
                        );
                        sub.dropped = 0;
                    }
                    // Still congested. Keep the debt (this line is lost too) and
                    // do not attempt the event: the queue that just refused a
                    // marker will refuse it as well, and clearing the counter
                    // here would erase the loss at its largest.
                    Err(mpsc::TrySendError::Full(_)) => {
                        sub.dropped += 1;
                        return true;
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => return false,
                }
            }
            match sub.tx.try_send(line.to_string()) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => {
                    // One line per congestion episode, not per lost event: the
                    // operator needs to know a subscriber fell behind, and the
                    // total arrives with the recovery line above.
                    if sub.dropped == 0 {
                        eprintln!(
                            "tasqx daemon: subscriber {} is not draining its queue (cap {OUT_QUEUE_CAP}); dropping events",
                            sub.id
                        );
                    }
                    sub.dropped += 1;
                    true
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
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
    /// The soonest instant [`ReminderScheduler`] currently holds, republished by
    /// [`reminder_loop`] after every tick; `None` = nothing scheduled.
    ///
    /// A mirror rather than a shared heap because the heap belongs to one
    /// thread and must stay that way — the accept loop needs one scalar to
    /// answer "is work coming?" for the idle-shutdown check (D5), and taking
    /// the scheduler's lock from a second thread to learn it would put the
    /// notification path behind the accept loop for nothing. Stale by at most
    /// one `REMINDER_TICK_MS`, which is 200 ms against a timeout measured in
    /// minutes.
    next_reminder: Arc<Mutex<Option<jiff::Timestamp>>>,
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
        let _ = tx.send(BackgroundFailure {
            component,
            message: error.message.clone(),
        });
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
        .query_row("SELECT COALESCE(MAX(rowid), 0) FROM events", [], |r| {
            r.get(0)
        })
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
    serve_with_options(engine, socket, shutdown, DaemonOptions::default())
}

/// [`serve`], with the reminder [`Notifier`] injected — the seam the CLI used to
/// call to honour `[notify] enabled` (§9) and tests use to observe delivery
/// without an OS transport. Retained as a thin wrapper over
/// [`serve_with_options`]; token attribution stays off on this path.
pub fn serve_with_notifier(
    engine: Engine,
    socket: &str,
    shutdown: Arc<AtomicBool>,
    notifier: Arc<dyn Notifier>,
) -> io::Result<()> {
    serve_with_options(
        engine,
        socket,
        shutdown,
        DaemonOptions {
            notifier,
            tokens_enabled: false,
            otlp_port: None,
            idle_timeout: None,
        },
    )
}

/// Optional daemon behaviours bundled into one struct so the entry point keeps a
/// stable arity as features are added (#17 token attribution, #18 OTLP): the
/// alternative — a positional argument or yet another `serve_with_*` variant per
/// feature — is exactly the churn this avoids.
pub struct DaemonOptions {
    /// Reminder notification transport (§9). Defaults to the always-safe log
    /// backend so tests and CI never grow an OS-notification dependency.
    pub notifier: Arc<dyn Notifier>,
    /// Spawn the async token-attribution thread (#17). Off by default
    /// (DESIGN §10): the daemon parses AI tool transcripts only when the user
    /// opts in via `[tokens] enabled`.
    pub tokens_enabled: bool,
    /// Run the local OTLP/HTTP receiver on `127.0.0.1:<port>` (#18). `None`
    /// (default) means no listener thread — the receiver is opt-in via
    /// `[otlp] enabled`, off by default (DESIGN §10).
    pub otlp_port: Option<u16>,
    /// Exit on our own after this long with no clients and no work (D5).
    ///
    /// `None` — the default — never exits. D5's 15 minutes is the default for
    /// the daemon a socket-requiring client *auto-spawns*, and nothing in this
    /// tree auto-spawns one yet: every daemon that exists today was started by
    /// a human typing `tasqx daemon`, and a process that walks away from a
    /// terminal its operator is watching is a bug, not a feature. So the
    /// default here is off and the CLI arms it from `[daemon] idle_timeout`;
    /// the auto-spawn half of D5, when it lands, passes the 15 minutes
    /// explicitly at the spawn site that knows it is a lazily-started daemon.
    pub idle_timeout: Option<Duration>,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        DaemonOptions {
            notifier: Arc::new(LogNotifier),
            tokens_enabled: false,
            otlp_port: None,
            idle_timeout: None,
        }
    }
}

/// [`serve`], with all optional behaviours injected via [`DaemonOptions`] — the
/// seam the CLI uses to honour both `[notify] enabled` (§9) and `[tokens]
/// enabled` (#17).
pub fn serve_with_options(
    engine: Engine,
    socket: &str,
    shutdown: Arc<AtomicBool>,
    options: DaemonOptions,
) -> io::Result<()> {
    let DaemonOptions {
        notifier,
        tokens_enabled,
        otlp_port,
        idle_timeout,
    } = options;
    let listener = bind(socket)?;
    // Non-blocking accept so the loop can observe `shutdown`; accepted streams
    // stay blocking, which the thread-per-connection model wants.
    listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

    let start = max_event_rowid(&engine).map_err(|e| {
        io::Error::other(format!(
            "event watermark initialization failed: {}",
            e.message
        ))
    })?;
    let (fatal_tx, fatal_rx) = mpsc::channel();
    let shared = Shared {
        engine: Arc::new(Mutex::new(engine)),
        hub: Hub::new(),
        shutdown: shutdown.clone(),
        watermark: Arc::new(Mutex::new(start)),
        next_reminder: Arc::new(Mutex::new(None)),
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

    // Token attribution (#17): a third supervised thread, spawned only when the
    // user opted in. It keeps its OWN event-rowid cursor — never `Shared.watermark`,
    // which is the broadcast-dedupe cursor — and does all transcript parsing off
    // the engine lock.
    if tokens_enabled {
        let sh = shared.clone();
        let sd = shutdown.clone();
        thread::spawn(move || attribution_loop(sh, sd));
    }

    // Local OTLP/HTTP receiver (#18): a supervised thread on its own std
    // TcpListener, spawned only when the user opted in via `[otlp] enabled`.
    // Independent of `tokens_enabled` — it buffers telemetry regardless; the
    // attribution thread (when on) then prefers that buffer over log-parsing. A
    // bind failure inside the receiver is logged and non-fatal (it is auxiliary),
    // so no `fatal` channel is wired to it.
    if let Some(port) = otlp_port {
        let engine = shared.engine.clone();
        let sd = shutdown.clone();
        thread::spawn(move || otlp::run_receiver(engine, port, sd));
    }

    // The server-level idle clock (D5). `None` means "not idle" — either
    // something is going on, or the feature is off and the value is never read.
    let mut idle_since: Option<Instant> = None;
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
        // D5's idle shutdown, asked once per accept attempt (~20 ms while idle).
        // The whole decision is the two predicates below; this block only
        // gathers what they judge, so "when does a daemon leave" is assertable
        // without a socket, a clock or a real minute of waiting.
        let now = Instant::now();
        let busy = server_busy(
            admission.active(),
            shared.hub.subscribers(),
            reminder_due_within(
                *lock_recover(&shared.next_reminder),
                jiff::Timestamp::now(),
                // The horizon is the configured timeout itself rather than a
                // constant of its own: the question is "would leaving now
                // strand work we would otherwise have been here for", and the
                // window we would otherwise have been here for is exactly that.
                idle_timeout.unwrap_or(Duration::ZERO),
            ),
            otlp_port.is_some(),
        );
        idle_since = advance_idle_clock(idle_since, busy, now);
        if idle_shutdown_due(idle_timeout, idle_since, now) {
            // Through the flag, not `process::exit`: a client that connected in
            // the window between the zero-client reading above and this line
            // must find the daemon already refusing, so its request comes back
            // as `unavailable` ("not applied, retry") instead of committing
            // into a daemon that is walking out. Same reason `writer_loop`
            // drains rather than drops — see its doc comment.
            shutdown.store(true, Ordering::Relaxed);
            eprintln!(
                "tasqx daemon: no clients and no work for {}s; shutting down \
                 (`[daemon] idle_timeout`)",
                idle_timeout.unwrap_or_default().as_secs()
            );
            break Ok(());
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

// ---- idle shutdown (DESIGN.md §12-D5) ---------------------------------------

/// Whether the daemon has anything to be here for right now.
///
/// Every argument is a reason to stay, and each one is here because leaving
/// while it holds loses something the client cannot see:
///
///  * `clients` — a connection is admitted, so a request may be in flight or
///    about to be. This is the permit count, so it also covers the reader that
///    has read a frame but not answered it yet.
///  * `subscribers` — someone is watching the push stream. `watch` on a pipe
///    emits one line per event and never resyncs (see [`OUT_QUEUE_CAP`]), so a
///    daemon that leaves takes the rest of that stream with it and the consumer
///    reads silence, not an error. Implied by `clients` as the code stands;
///    asked separately because it is the property that matters, not the
///    implication.
///  * `reminder_due` — a reminder is close enough that we would have delivered
///    it. It is not lost outright (`ReminderScheduler`'s dedupe fires a
///    ripened-while-down reminder on the next start), but nothing restarts this
///    daemon on its own yet, so "late" here means "when a human next runs
///    `tasqx daemon`", which for a reminder is the same as never.
///  * `telemetry_listening` — the OTLP receiver (#18) is bound. Its clients are
///    AI tools posting over TCP, which hold no socket connection and no
///    subscription, so nothing else in this function can see them: a daemon
///    that exits while that port is open drops telemetry with no error anywhere.
///
/// Deliberately NOT a reason: the token-attribution thread (#17). It rebuilds
/// its pending set from the store on the tick after any daemon starts, so work
/// missed while down is picked up in full — its own doc comment states that,
/// and `attribution_tick` is what keeps it true.
fn server_busy(
    clients: usize,
    subscribers: usize,
    reminder_due: bool,
    telemetry_listening: bool,
) -> bool {
    clients > 0 || subscribers > 0 || reminder_due || telemetry_listening
}

/// Whether a scheduled reminder falls inside `horizon` of `now`.
///
/// A past-due instant counts (the difference is negative, so it is inside any
/// horizon): the scheduler is about to fire it on its next tick, and that is
/// the one moment where a shutdown would race a delivery.
///
/// `None` — nothing scheduled — is not work. That is what keeps the feature
/// from being cancelled by a single task with a reminder set for next year: a
/// far-future instant is outside every sane horizon and the daemon is free to
/// go, which is the whole point of D5.
fn reminder_due_within(
    next: Option<jiff::Timestamp>,
    now: jiff::Timestamp,
    horizon: Duration,
) -> bool {
    let Some(at) = next else {
        return false;
    };
    match jiff::SignedDuration::try_from(horizon) {
        Ok(horizon) => at.duration_since(now) <= horizon,
        // A horizon too large for a `SignedDuration` (nothing a config file can
        // express, since the setting is capped in minutes) resolves to "stay",
        // because the failure direction that costs nothing is the one that
        // keeps a running daemon running.
        Err(_) => true,
    }
}

/// Advance the idle clock by one observation: `None` while busy, otherwise the
/// instant the current idle stretch began.
///
/// The clock is **not** restarted by a further idle observation — that is the
/// whole content of this function, and getting it backwards (`Some(now)` on
/// every quiet tick) is a daemon that can never reach its deadline, with a
/// suite that stays green because every other part still works.
///
/// It starts at boot rather than at the first disconnect, so a daemon nobody
/// ever connects to still leaves. D5 words the default as "post-last-disconnect"
/// for the auto-spawned case; a spawner that dies before it connects is exactly
/// the lingering ghost that entry is about, and it never produces a disconnect.
fn advance_idle_clock(previous: Option<Instant>, busy: bool, now: Instant) -> Option<Instant> {
    if busy {
        return None;
    }
    Some(previous.unwrap_or(now))
}

/// Whether an idle daemon has now been idle long enough to leave.
///
/// `timeout` of `None` is the off switch and the reason this is an `Option`
/// rather than a `Duration` the caller pre-checks: "not configured" has to be
/// answerable in the same place as "not long enough yet", or the two conditions
/// end up in two places that must agree — the shape every guard in this file
/// exists to prevent.
fn idle_shutdown_due(timeout: Option<Duration>, idle_since: Option<Instant>, now: Instant) -> bool {
    match (timeout, idle_since) {
        (Some(timeout), Some(since)) => now.duration_since(since) >= timeout,
        _ => false,
    }
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
        // Republish what the heap is holding, for the idle-shutdown check (D5)
        // in the accept loop. Written after the tick, so a reminder that just
        // fired is already gone from it and cannot keep the daemon alive
        // forever by looking permanently imminent.
        *lock_recover(&sh.next_reminder) = sched.peek_at();

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

/// Rate-limits the daemon's transient-failure lines to one per *subject* per
/// state change, where the subject is the task the message is about.
///
/// This used to be a single `Option<String>` plus an argument-less `recovered()`,
/// which bounded nothing in practice. Every message embeds the task's `short_id`,
/// so two tasks failing in the same tick produced two alternating strings: each
/// call differed from the immediately preceding one, so each call was a
/// "transition" and each one printed, every tick, for as long as the fault
/// lasted. And `recovered()` cleared the single slot on *any* successful write,
/// so one task completing normally re-armed the log line of a different task
/// that was still failing — defeating the throttle without even needing two
/// concurrent failures. At [`REMINDER_TICK_MS`] either case is five stderr lines
/// a second, on a daemon whose stderr the CLI inherits rather than nulls.
///
/// Recovery is implicit and per subject: a task that stops failing simply stops
/// appearing in `current`, so its next failure prints again. That also bounds
/// the maps to the tasks that actually failed in the last two ticks — no entry
/// can outlive the failure that created it, which a `HashMap` keyed off a
/// long-lived `report`/`recovered` pair could not promise.
#[derive(Default)]
struct ErrorTransition {
    /// What each subject reported during the previous tick.
    previous: HashMap<i64, String>,
    /// What each subject has reported so far in this tick.
    current: HashMap<i64, String>,
}

impl ErrorTransition {
    /// Open a tick. Whatever was reported in the last tick becomes the
    /// suppression baseline; whatever is not re-reported has recovered.
    fn begin_tick(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    /// Record `message` against `subject`; `true` if it is new for that subject
    /// (this tick or the last), i.e. worth printing.
    fn enter(&mut self, subject: i64, message: &str) -> bool {
        let seen = self
            .current
            .get(&subject)
            .or_else(|| self.previous.get(&subject));
        let repeat = seen.is_some_and(|previous| previous == message);
        self.current.insert(subject, message.to_string());
        !repeat
    }

    fn report(&mut self, subject: i64, message: String) {
        if self.enter(subject, &message) {
            eprintln!("tasqx daemon: {message}");
        }
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
    // One pass over the ripe set is one tick for log-throttling purposes: a task
    // that does not report a failure below has recovered, per task, without any
    // other task's success speaking for it.
    fire_errors.begin_tick();
    for p in &ripe {
        let fired = {
            let g = lock_recover(&sh.engine);
            fire(&g, p)
        };
        match fired {
            // Deduped (already reminded) => do NOT notify. Recovery needs no
            // call: not reporting a failure for this task in this tick *is* the
            // recovery signal, and it cannot speak for any other task.
            Ok(fired) => {
                if fired {
                    fired_any = true;
                    notifier.notify(&scheduler::notification_for(p));
                }
            }
            Err(e) => {
                fire_errors.report(
                    p.short_id,
                    format!(
                        "could not record reminder for #{}: {}",
                        p.short_id, e.message
                    ),
                );
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

// ---- token attribution (DESIGN.md §10, docs/research/token-accounting.md) ----

/// How one attribution is recorded. Only ever [`attribution::attribute_one`] in
/// production; the indirection mirrors [`FireFn`] so a test can inject a failing
/// write and pin the retry path (otherwise reachable only through disk/lock
/// faults that cannot be provoked from a test).
type AttributeFn = dyn Fn(
        &Engine,
        &attribution::PendingAttribution,
        &attribution::AttributionResult,
    ) -> Result<bool, ApiError>
    + Send
    + Sync;

/// The daemon's token-attribution thread (#17): watch the event log for
/// completions, reconstruct each task's tokens from the AI tool's transcript,
/// and store them as measured `token_usage` rows.
///
/// Sibling of [`reminder_loop`], and deliberately the same shape: a cursor over
/// the append-only `events` rowid, a rebuild-on-change pending set derived from
/// the store (so a task completed while no daemon ran is caught up for free on
/// the next tick, exactly like a reminder missed while down), and its own
/// clock. It keeps its OWN cursor — never `Shared.watermark`, which gates the
/// broadcast dedupe — following [`reminder_tick`]'s "only adopt a rowid you
/// built from" invariant.
fn attribution_loop(sh: Shared, shutdown: Arc<AtomicBool>) {
    // -1 can't be a real rowid, so the first tick always rebuilds — this is the
    // catch-up-on-start scan over the whole store (§10).
    let mut seen_rowid: i64 = -1;
    let mut errors = ErrorTransition::default();

    while !shutdown.load(Ordering::Relaxed) {
        match attribution_tick(
            &sh,
            seen_rowid,
            jiff::Timestamp::now(),
            &attribution::attribute_one,
            &mut errors,
        ) {
            Ok(next) => seen_rowid = next,
            // A tick returns Err ONLY for a genuinely fatal store fault (the
            // event-rowid read, the pending query, or the pump). A missing or
            // drifted transcript is transient and handled per-task inside the
            // tick — it must NEVER reach report_fatal, or a routine
            // rotated/renamed transcript would take the whole daemon down.
            Err(e) => {
                report_fatal(&sh, "token attribution", &e);
                return;
            }
        }

        // Sleep in small steps so shutdown stays responsive.
        for _ in 0..(ATTRIBUTION_TICK_MS / 50).max(1) {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// One tick of [`attribution_loop`]: if the event log moved, rebuild the pending
/// set from the store, parse each task's transcript OFF the engine lock, and
/// write the result through the idempotent [`Engine::token_attribute`] under a
/// short lock. Returns the watermark to carry into the next tick.
///
/// `now` is injected (matching the [`reminder_tick`] seam) and used only to
/// decide when an absent explicit transcript has been retried long enough to
/// give up; the attribution window itself is reconstructed entirely from event
/// timestamps.
///
/// The returned watermark obeys the same invariant as [`reminder_tick`]: it may
/// only be a rowid the pending set was actually built from. A transient per-task
/// failure (a not-yet-flushed transcript) leaves no `tokens.attributed` row, so
/// the rowid has not moved for that task — returning `-1` forces the next tick to
/// rebuild and retry rather than adopting `cur` and skipping it forever.
fn attribution_tick(
    sh: &Shared,
    seen_rowid: i64,
    now: jiff::Timestamp,
    attribute: &AttributeFn,
    errors: &mut ErrorTransition,
) -> Result<i64, ApiError> {
    let cur = {
        let g = lock_recover(&sh.engine);
        max_event_rowid(&g)?
    };
    if cur == seen_rowid {
        return Ok(seen_rowid);
    }

    // Build the pending set under a SHORT lock, then release it: transcripts can
    // be large and must never be parsed while holding the engine mutex.
    let pending = {
        let g = lock_recover(&sh.engine);
        attribution::pending_attributions(&g)?
    };

    let mut wrote_any = false;
    let mut failed_any = false;
    // One pass over the pending set is one tick for log-throttling purposes, the
    // same contract [`reminder_tick`] uses: every message below embeds the task's
    // short_id, so a throttle keyed on one global string would see two failing
    // tasks alternate and count every line as a transition.
    errors.begin_tick();
    for pa in &pending {
        // Heavy transcript parse, OFF the lock.
        let result = match attribution::compute_attribution(pa, now) {
            Ok(r) => r,
            Err(e) => {
                errors.report(
                    pa.short_id,
                    format!(
                        "could not read transcript for #{}: {}",
                        pa.short_id, e.message
                    ),
                );
                failed_any = true;
                continue;
            }
        };
        // Re-lock briefly to write via the idempotent method.
        let written = {
            let g = lock_recover(&sh.engine);
            attribute(&g, pa, &result)
        };
        match written {
            // Recovery needs no call: not reporting a failure for this task in
            // this tick *is* the recovery signal, and — unlike the old global
            // `recovered()` — one task's success cannot re-arm another task's
            // suppressed log line.
            Ok(did_write) => {
                if did_write {
                    wrote_any = true;
                }
            }
            Err(e) => {
                errors.report(
                    pa.short_id,
                    format!(
                        "could not record token attribution for #{}: {}",
                        pa.short_id, e.message
                    ),
                );
                failed_any = true;
            }
        }
    }

    // Push `tokens.attributed` rows to subscribers immediately (the headless
    // verification surface, mirroring the reminder path).
    if wrote_any {
        pump(sh)?;
    }

    if failed_any {
        Ok(-1)
    } else {
        Ok(cur)
    }
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
        let mut shutdown_since: Option<Instant> = None;
        while !state.done.load(Ordering::Acquire) {
            let now = Instant::now();
            let stopping = shutdown.load(Ordering::Relaxed);
            if stopping && shutdown_since.is_none() {
                shutdown_since = Some(now);
            }
            let idle = state.idle(now);
            // Whether the *write* may be cut. Asked first because on this
            // platform it also governs the read: see below.
            let may_cut_write =
                send_cancel_due(shutdown_since, idle, state.write_timed_out(now), now);
            // Cancelling the *read* is what unblocks the reader loop so the
            // connection can wind down at all, and an aborted read destroys
            // nothing: the request it was waiting for was never dispatched.
            // Windows has no `SO_RCVTIMEO` equivalent for named pipes — the
            // `set_recv_timeout` call in `handle_conn` is `#[cfg(unix)]` — so a
            // blocked read here ends only when the peer does something (closing
            // its end raises `ERROR_BROKEN_PIPE`) or when we cancel it. A client
            // parked on a truncated frame does neither, which is why the
            // dead-writer case has to be part of the condition on this side too
            // and not merely in the reader's timeout arm.
            if recv_cancel_due(
                recv_teardown_due(stopping, idle, state.write_failed()),
                state.write_in_flight(),
                may_cut_write,
            ) {
                cancel_io(recv_handle);
            }
            // A write is the opposite. Cancelling it mid-flush throws away a
            // response whose transaction already committed — the same
            // lost-response bug the writer's old pre-write shutdown check had,
            // reintroduced one layer down. Shutdown therefore only cancels
            // writes after the drain grace, while an idle client or a stuck
            // write is still cut immediately.
            //
            // This second call is now the only one that can end a write, which
            // is what makes the sentence above true rather than merely
            // intended: `recv_cancel_due` gates the read-cancel on the same
            // answer, because both cancels land on the same handle.
            //
            // A *failed* write is deliberately absent from this condition.
            // [`ConnectionIoState::write_failed`] is set only after
            // [`ConnectionIoState::end_write`], so whenever it is true there is
            // no in-flight write left on this handle for `CancelIoEx` to abort:
            // adding it would buy nothing and would widen a predicate whose
            // entire job is restraint about committed responses.
            if may_cut_write {
                cancel_io(send_handle);
            }
            thread::sleep(CLIENT_WATCHDOG_INTERVAL);
        }
    })
}

/// Whether the connection watchdog may cancel an in-flight *write*.
///
/// Compiled under `test` on every platform on purpose: the Windows watchdog is
/// its only caller, but the policy it encodes — shutdown alone must not abort a
/// committed response — is exactly the thing that silently regresses, and it has
/// to stay assertable on the platforms that cannot run the watchdog at all.
///
/// The shutdown grace is [`CLIENT_SEND_TIMEOUT`] rather than a constant of its
/// own so both platforms answer to one bound: a healthy client drains in
/// microseconds (the read is cancelled immediately, which ends the reader loop,
/// drops `out_tx` and lets the writer finish), and a client that has stopped
/// reading is cut at the same 5 s deadline Unix `SO_SNDTIMEO` would impose.
#[cfg(any(windows, test))]
fn send_cancel_due(
    shutdown_since: Option<Instant>,
    idle: bool,
    write_timed_out: bool,
    now: Instant,
) -> bool {
    idle || write_timed_out
        || shutdown_since.is_some_and(|since| now.duration_since(since) >= CLIENT_SEND_TIMEOUT)
}

/// Whether the watchdog may cancel the blocked *read* right now.
///
/// This exists because on Windows a cancel cannot be aimed. `Stream::split()`
/// hands back two halves that share one `RawPipeStream` — interprocess 2.4.2
/// `os/windows/named_pipe/stream/impl.rs:35` is `(self.raw.refclone(),
/// self.raw)`, and `as_handle` returns `self.raw.get().as_handle()` for both —
/// so `recv_handle` and `send_handle` are the same value, and
/// `CancelIoEx(h, NULL)` aborts every pending operation on it, reads and writes
/// alike. `handle_identity_is_what_the_cancel_policy_assumes` pins that.
///
/// So the read-cancel was quietly cancelling writes too, and the restraint
/// [`send_cancel_due`] exists to enforce — shutdown alone must not abort a
/// response whose transaction already committed — was unreachable on the only
/// platform that runs it. Ctrl-C set the flag; within one
/// [`CLIENT_WATCHDOG_INTERVAL`] tick the read-cancel fired on `stopping` alone
/// and took the in-flight `WriteFile` with it, and the client read a truncated
/// frame for a mutation that had landed. That is precisely the "applied" versus
/// "not applied" distinction the shutdown refusal envelope is built to keep.
///
/// The fix is not to hesitate about tearing down — it is to wait for the one
/// moment when cancelling costs nothing. A write in flight is the only thing on
/// this handle worth protecting, so the read-cancel holds until either there is
/// no write in flight (the common case: a healthy client's write completes in
/// microseconds, so this is the very next tick) or the write has forfeited its
/// grace anyway, which is exactly what `may_cut_write` already decides. The
/// wait is therefore bounded by [`CLIENT_SEND_TIMEOUT`], not open-ended: a
/// stuck write is cut at 5 s and the read goes with it.
#[cfg(any(windows, test))]
fn recv_cancel_due(want_teardown: bool, write_in_flight: bool, may_cut_write: bool) -> bool {
    want_teardown && (!write_in_flight || may_cut_write)
}

/// Whether this connection should stop waiting for the client to send anything.
///
/// The two platforms reach this policy through different machinery, so it lives
/// in one function rather than twice. Both used to spell the condition out
/// inline as `shutdown || idle`, in two places that have to agree — which is
/// exactly the shape that gets fixed on one platform only.
///
/// The machinery differs, but the *arm* does not. On Unix a reader parked in a
/// read wakes on the `CLIENT_IO_POLL_TIMEOUT` recv timeout and asks this
/// question directly. On Windows there is no recv timeout at all (the
/// `set_recv_timeout` call in `handle_conn` is `#[cfg(unix)]`), so the watchdog
/// asks it first and cancels the blocked read; `CancelIoEx` completes that read
/// with `ERROR_OPERATION_ABORTED`, which Rust maps to
/// [`io::ErrorKind::TimedOut`] — so it surfaces in the very same timeout arm,
/// which asks again and breaks. Windows therefore consults this function twice
/// per teardown, and that reader arm is load-bearing on both platforms rather
/// than being the Unix half of anything.
///
/// `write_failed` is the third reason and the least obvious one. The other two
/// are about *us* being finished with the client; this one is about the client
/// being unable to tell us anything ever again. Its writer thread died mid-frame
/// (`SO_SNDTIMEO` on Unix, a watchdog `CancelIoEx` on Windows), so the client is
/// parked reading a truncated line and will not send the next request. The
/// reader would notice the closed channel on its own — but only on its next
/// `out_tx.send`, and that needs a request the client is no longer in a position
/// to send. Without this reason the wait therefore runs to
/// [`CLIENT_IDLE_TIMEOUT`]: 15 minutes of a held admission slot, a registered
/// subscription, and silence at the client.
fn recv_teardown_due(shutting_down: bool, idle: bool, write_failed: bool) -> bool {
    shutting_down || idle || write_failed
}

#[cfg(windows)]
fn cancel_io(raw_handle: usize) {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::IO::CancelIoEx;

    // SAFETY: the connection owns this handle until its watchdog joins. A null
    // OVERLAPPED pointer asks Windows to cancel every pending operation on it.
    let _ = unsafe { CancelIoEx(raw_handle as HANDLE, std::ptr::null()) };
}

/// The writer thread's body: drain `out_rx` onto the connection's send half
/// until the queue closes or a write fails, and record in `state` *which of the
/// two* happened.
///
/// It deliberately does NOT consult the shutdown flag. It used to check it after
/// dequeuing a line and break *before* writing, which threw away frames whose
/// transaction had already committed: `handle_conn` releases the engine guard
/// and only then queues the response, so a Ctrl-C landing in that window made
/// the daemon commit the write and answer with a bare EOF (`daemon transport
/// error` client-side — indistinguishable from a failure that never happened).
/// Shutdown is bounded here by the queue emptying instead: the reader stops
/// accepting work the moment it sees the flag, drops `out_tx`, and this loop
/// exits as soon as it has drained what was already queued. A client that has
/// stopped reading is still bounded by the send timeout (`SO_SNDTIMEO` on Unix,
/// the watchdog on Windows).
///
/// The two exits are not interchangeable, which is why only one of them touches
/// [`ConnectionIoState::note_write_failure`]. `recv` returning `Err` means the
/// reader is already gone and closed the channel — the ordinary end of every
/// connection. A write returning `Err` means the reader is still blocked in a
/// read that nothing on the wire will ever satisfy, and it is the only party
/// left that can free the connection's resources; see [`recv_teardown_due`].
/// The flag is set *after* [`ConnectionIoState::end_write`], so it can never be
/// observed while a write is still in flight.
///
/// Generic over `W: Write` rather than taking `SendHalf` so both exits are
/// assertable without a socket. The real send timeouts are 5 s and only fire
/// against a client that has genuinely stopped reading, which is not something a
/// test can arrange quickly or deterministically on either platform; a sink that
/// fails its first `write` reproduces the state the daemon ends up in exactly.
fn writer_loop<W: Write>(send: &mut W, out_rx: &mpsc::Receiver<String>, state: &ConnectionIoState) {
    while let Ok(line) = out_rx.recv() {
        state.begin_write();
        let result = send.write_all(line.as_bytes()).and_then(|_| send.flush());
        state.end_write();
        if result.is_err() {
            state.note_write_failure();
            break;
        }
    }
}

fn handle_conn(stream: Stream, sh: Shared, _permit: ClientPermit) {
    // The accept loop's listener is nonblocking, and BSD-derived kernels
    // (macOS) hand every accepted socket the listener's O_NONBLOCK flag —
    // Linux does not, and interprocess's `Accept` mode only ever sets the
    // flag on accepted streams, never clears it. A stream left nonblocking
    // turns both SO_* timeouts below into no-ops and the first reply larger
    // than the socket buffer (8 KiB on macOS) into a mid-frame WouldBlock
    // that kills the writer thread, leaving the client hanging on a truncated
    // frame. Restore the blocking contract explicitly instead of assuming the
    // platform did. (The dead writer no longer strands the *connection* until
    // the idle deadline — `recv_teardown_due` below ends it at the next poll —
    // but the truncated reply is still a reply the client never gets.)
    #[cfg(unix)]
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_recv_timeout(Some(CLIENT_IO_POLL_TIMEOUT))
            .is_err()
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

    // Why the writer must not consult `sh.shutdown`, and why only one of its two
    // exits is reported back through the connection's state: see [`writer_loop`].
    // The spawn goes through `io_state` rather than a cloned local so the writer
    // cannot end up reporting into a state nobody reads — see [`spawn_writer`].
    //
    // [`spawn_writer`]: ConnectionIoState::spawn_writer
    let writer = io_state.spawn_writer(send, out_rx);

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
                // Observe shutdown *before* dispatching, not only on a read
                // timeout further down. Otherwise a request that arrived after
                // Ctrl-C is still committed to SQLite while the daemon is
                // winding down, and the client can no longer tell "applied" from
                // "not applied". Refusing up front makes the answer true: this
                // request did not run, retry against the next daemon.
                if sh.shutdown.load(Ordering::Relaxed) {
                    let refusal = unavailable_envelope(
                        "daemon is shutting down; the request was not applied",
                    );
                    // Queued, not written here: the writer owns the send half.
                    // It drains what is already queued before exiting, so this
                    // frame still reaches the socket. A send error only means
                    // the client is already gone — we are closing regardless.
                    let _ = out_tx.send(format!("{refusal}\n"));
                    break;
                }
                // `subscribe` is a transport-level verb, not a core method: it
                // registers this connection for pushes and acks.
                if let Some(ack) = try_subscribe(trimmed, &sh, &out_tx, &mut sub_id) {
                    if out_tx.send(ack).is_err() {
                        break;
                    }
                    continue;
                }
                // `tokens.recompute` parses transcripts, which must never
                // happen under the daemon's global engine lock (one hung
                // `open()` would wedge every client) — refused HERE, before
                // the lock is even taken, naming the in-process invocation
                // (D50 Decision 3).
                if let Some(refusal) = recompute_refusal_envelope(trimmed) {
                    let out = format!("{refusal}\n");
                    if out_tx.send(out).is_err() {
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
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                // The one moment a reader parked in a read gets to look at
                // anything — so it is where a dead writer has to be noticed.
                // This arm is NOT Unix-only despite the recv timeout that names
                // it being `#[cfg(unix)]`: on Windows the watchdog's
                // `CancelIoEx` completes the pending read with
                // `ERROR_OPERATION_ABORTED`, which Rust maps to `TimedOut`, so
                // the cancel lands right here and this `break` is what actually
                // ends the connection on that platform too. The `Ok(_)` arm
                // above would notice the dead writer as well, when its
                // `out_tx.send` hit the closed channel — but only if the client
                // sends another request, and it is stuck on the frame the failed
                // write truncated.
                if recv_teardown_due(
                    sh.shutdown.load(Ordering::Relaxed),
                    io_state.idle(Instant::now()),
                    io_state.write_failed(),
                ) {
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

/// A transport-level refusal: the daemon did not run the request at all.
///
/// Deliberately carries no `id`, which is what makes [`Conn::request`] surface
/// it as `ConnectionRefused` with this message instead of treating it as the
/// answer to whatever it happened to ask. Both refusal paths — the admission cap
/// and shutdown — mean the same thing to a caller ("nothing was applied, retry"),
/// and that is precisely the distinction a bare EOF destroys.
fn unavailable_envelope(message: &str) -> Value {
    json!({
        "tasqx": crate::API_VERSION,
        "ok": false,
        "error": { "code": "unavailable", "message": message },
    })
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
    let response = unavailable_envelope(&format!(
        "daemon client limit ({MAX_CONCURRENT_CLIENTS}) reached; retry after a client disconnects"
    ));
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
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request frame exceeds limit",
                ));
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request frame exceeds limit",
            ));
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

/// The transport-level refusal for `tokens.recompute` (D50 Decision 3): the
/// verb parses transcripts, and transcript I/O under the daemon's global
/// engine lock would let one hung `open()` wedge every client — so the daemon
/// never dispatches it. A correlated `bad_request` (unlike
/// [`unavailable_envelope`], the request WAS answered — with the invocation
/// that works) so the CLI surfaces the message verbatim; in-process dispatch
/// is untouched.
///
/// `None` for every other method — including a line that fails to parse,
/// which must keep flowing to `handle_envelope` for its own malformed-request
/// answer.
fn recompute_refusal_envelope(line: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("method").and_then(Value::as_str) != Some("tokens.recompute") {
        return None;
    }
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let mut m = Map::new();
    m.insert("tasqx".into(), json!(crate::API_VERSION));
    if !id.is_null() {
        m.insert("id".into(), id);
    }
    m.insert("ok".into(), json!(false));
    m.insert(
        "error".into(),
        json!({
            "code": "bad_request",
            "message": "tokens.recompute parses transcripts and must run in-process: \
                        stop the daemon and run `tasqx --no-daemon tokens recompute`",
        }),
    );
    Some(Value::Object(m))
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
            return Ok(self
                .pending_responses
                .remove(i)
                .expect("position came from this queue"));
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
    /// An unsolicited push (a `task.changed` broadcast). It carries an `event`
    /// key and no request id, and it is the `event` key the reader classifies
    /// on: id-absence cannot be the test, because the transport-level refusal
    /// `unavailable_envelope` builds is id-less too and has to arrive as a
    /// [`Frame::Response`] for [`Conn::request`] to turn it into
    /// `ConnectionRefused` rather than park it as a push nobody is waiting for.
    /// Correlating a push with a pending request is what this enum exists to
    /// make impossible.
    Event(Value),
    /// The correlated answer to a request this client sent.
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
                .join(format!(
                    "tasqx-client-{label}-{}-{n}.sock",
                    std::process::id()
                ))
                .to_string_lossy()
                .into_owned()
        }
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// Run a client script on its own thread and fail the test rather than hang
    /// if it blocks. Every shutdown assertion below is about a frame that either
    /// arrives or is silently dropped, and a dropped frame parks the client in a
    /// read that nothing will ever satisfy — without a deadline the regression
    /// shows up as a wedged test run instead of a failed assertion.
    fn with_deadline<T: Send + 'static>(
        what: &str,
        script: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(script());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{what}: the daemon never answered"))
    }

    fn task_count(sh: &Shared) -> i64 {
        lock_recover(&sh.engine)
            .conn()
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap()
    }

    /// Serve exactly one connection through the real [`handle_conn`], so the
    /// reader loop, the writer thread and the shutdown flag are the production
    /// ones rather than a scripted stand-in.
    fn serve_one_conn(socket: &str, sh: Shared) -> thread::JoinHandle<()> {
        let listener = bind(socket).expect("bind one-connection server");
        thread::spawn(move || {
            let stream = listener.accept().expect("accept one-connection client");
            let permit = Arc::new(Admission::new(1))
                .try_acquire()
                .expect("admission permit");
            handle_conn(stream, sh, permit);
        })
    }

    /// Shutdown must not turn a mutation into a *silent* one. The reader only
    /// noticed the flag on a read timeout, so a request that arrived after
    /// Ctrl-C was dispatched and committed — and then the writer's pre-write
    /// shutdown check dropped the response, leaving the client with a bare EOF
    /// (`daemon transport error`) for a write that actually landed.
    #[test]
    fn a_request_arriving_after_shutdown_is_refused_instead_of_committed_unanswered() {
        let socket = client_test_socket("shutdown-refusal");
        let sh = shared(Engine::open_in_memory().unwrap());
        sh.shutdown.store(true, Ordering::Relaxed);
        let server = serve_one_conn(&socket, sh.clone());

        let mut conn = try_connect(&socket).expect("connect to shutting-down daemon");
        let outcome = with_deadline("task.add after shutdown", move || {
            conn.request("task.add", &json!({ "title": "ship it" }))
        });

        let err = outcome.expect_err("a shutting-down daemon must refuse, not stay silent");
        assert_eq!(
            err.kind(),
            io::ErrorKind::ConnectionRefused,
            "the refusal must be a transport-level unavailability, got {err:?}"
        );
        assert!(
            err.to_string().contains("shutting down"),
            "the client must be told why: {err}"
        );
        assert_eq!(
            task_count(&sh),
            0,
            "a refused request must never have been dispatched"
        );

        server.join().expect("connection thread");
        cleanup(&socket);
    }

    /// Frames already queued when shutdown lands must still reach the socket.
    /// The writer dequeued a line, saw the flag, and broke *before* writing it —
    /// so the last response of every daemon stop was discarded even though the
    /// transaction behind it had already committed.
    #[test]
    fn frames_queued_before_shutdown_are_flushed_not_discarded() {
        let socket = client_test_socket("shutdown-drain");
        let sh = shared(Engine::open_in_memory().unwrap());
        let server = serve_one_conn(&socket, sh.clone());

        let mut conn = try_connect(&socket).expect("connect subscriber");
        let (ready_tx, ready_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::channel();
        thread::spawn(move || {
            conn.subscribe().expect("subscribe ack");
            ready_tx.send(()).expect("hand the test its cue");
            let _ = frame_tx.send(conn.next_frame());
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("subscribe must be acked");

        // Ctrl-C, then a frame that was already committed to the queue: the
        // ordering the daemon hits on every stop with in-flight work.
        sh.shutdown.store(true, Ordering::Relaxed);
        let notif = json!({
            "tasqx": crate::API_VERSION,
            "event": "task.changed",
            "data": { "op": "add", "short_id": 7 },
        });
        sh.hub.broadcast(&format!("{notif}\n"));

        let frame = frame_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a queued frame must still be written during shutdown")
            .expect("no read error")
            .expect("no EOF before the queued frame");
        assert!(
            matches!(frame, Frame::Event(ref e) if e["data"]["short_id"] == 7),
            "the queued event must arrive intact"
        );

        server.join().expect("connection thread");
        cleanup(&socket);
    }

    /// Opt-in default-off (DESIGN §10): with no explicit `otlp_port`, the serve
    /// loop's `if let Some(port)` guard spawns no receiver thread. Encoding the
    /// default here pins "disabled config => no listener" at the one seam the CLI
    /// wires (`config_otlp_enabled().then(config_otlp_port)` yields `None`).
    #[test]
    fn otlp_receiver_is_off_by_default() {
        assert!(
            DaemonOptions::default().otlp_port.is_none(),
            "a default daemon must not open a telemetry port"
        );
    }

    #[test]
    fn admission_never_exceeds_its_limit_and_release_reopens_a_slot() {
        let admission = Arc::new(Admission::new(2));
        let first = admission.try_acquire().expect("first slot");
        let second = admission.try_acquire().expect("second slot");
        assert!(
            admission.try_acquire().is_none(),
            "the third client must be refused"
        );
        assert_eq!(admission.active(), 2);

        drop(first);
        let replacement = admission.try_acquire().expect("released slot is reusable");
        assert_eq!(admission.active(), 2);
        drop((second, replacement));
        assert_eq!(admission.active(), 0);
    }

    /// The Windows watchdog cancels I/O the connection threads cannot interrupt
    /// themselves. Cancelling the *send* handle the moment shutdown is set aborts
    /// a flush of an already-committed response — the platform-specific half of
    /// the same lost-response bug. Writes are cut only after the drain grace, or
    /// on the pre-existing idle / stuck-write conditions. Reads are not free to
    /// go first either, because both cancels land on one handle; that half is
    /// `a_shutdown_read_cancel_waits_for_the_write_it_would_take_with_it`.
    #[test]
    fn shutdown_alone_does_not_cancel_an_in_flight_write() {
        let now = Instant::now();
        let just_now = Some(now - Duration::from_millis(1));
        assert!(
            !send_cancel_due(just_now, false, false, now),
            "a fresh shutdown must let a committed response finish flushing"
        );
        assert!(
            send_cancel_due(Some(now - CLIENT_SEND_TIMEOUT), false, false, now),
            "the drain grace is bounded, not unlimited"
        );
        assert!(
            send_cancel_due(None, true, false, now),
            "an idle client is still cut without waiting for shutdown"
        );
        assert!(
            send_cancel_due(None, false, true, now),
            "a stuck write is still cut without waiting for shutdown"
        );
    }

    /// The other half of that restraint, and the one that was missing.
    ///
    /// `send_cancel_due` guarded the *send* handle, but on Windows the read and
    /// the write are the same handle, so the read-cancel above it was cutting
    /// committed responses on `stopping` alone — within one 100 ms tick, five
    /// seconds before the drain grace it was supposed to honour. The predicate
    /// only means something if the read waits too.
    #[test]
    fn a_shutdown_read_cancel_waits_for_the_write_it_would_take_with_it() {
        assert!(
            !recv_cancel_due(true, true, false),
            "a teardown that would abort an in-flight write must wait: the frame \
             on the wire may be a response whose transaction already committed, \
             and the client cannot tell a truncated one from a failure"
        );
        assert!(
            recv_cancel_due(true, false, false),
            "with no write in flight the cancel costs nothing and must not be \
             delayed — this is the common case, and the teardown bound depends \
             on it firing on the next tick"
        );
        assert!(
            recv_cancel_due(true, true, true),
            "once the write has forfeited its grace (idle, stuck, or the drain \
             window elapsed) the read goes with it, so the wait is bounded by \
             CLIENT_SEND_TIMEOUT rather than open-ended"
        );
        assert!(
            !recv_cancel_due(false, false, true),
            "nothing wants teardown, so nothing is cancelled — `may_cut_write` \
             widens when the read may go, never why"
        );
    }

    /// The read-cancel must reach `CancelIoEx` through [`recv_cancel_due`], not
    /// past it.
    ///
    /// Dropping the gate and calling `recv_teardown_due` straight — the shape
    /// this replaces — leaves every test green; `-D warnings` does catch it,
    /// but only on Windows and only as "function is never used", which says
    /// nothing about the committed response that just went missing. One line of
    /// source scan buys the real sentence, in the idiom
    /// `both_platform_teardown_paths_consult_the_shared_policy` already uses.
    #[test]
    fn the_watchdog_cancels_the_read_only_through_the_shared_gate() {
        let production = include_str!("daemon.rs")
            .split_once("\nmod tests {")
            .expect("daemon.rs keeps its unit tests in a trailing `mod tests`")
            .0;
        let watchdog = production
            .split_once("fn start_connection_watchdog(")
            .expect("the Windows watchdog is still called that")
            .1
            .split_once("\nfn ")
            .map_or("", |(body, _)| body);

        let gate = watchdog
            .find("recv_cancel_due(")
            .expect("the watchdog must ask `recv_cancel_due` before cancelling the read");
        let cancel = watchdog
            .find("cancel_io(recv_handle)")
            .expect("the watchdog still cancels the read somewhere");
        assert!(
            gate < cancel,
            "the read-cancel must be gated by `recv_cancel_due`, not merely \
             preceded by it: on Windows that one syscall aborts the in-flight \
             write too, so cancelling on `stopping` alone discards a response \
             whose transaction already committed"
        );
    }

    /// The premise the whole cancel policy rests on, asserted rather than
    /// assumed: on Windows `Stream::split()` does not hand back two handles.
    ///
    /// interprocess 2.4.2 splits by `(self.raw.refclone(), self.raw)` and both
    /// halves answer `self.raw.get().as_handle()`, so one `CancelIoEx` reaches
    /// reads and writes alike. Every line of [`recv_cancel_due`] is reasoning
    /// about that. If a future version ever duplicates the handle for real, the
    /// gate becomes an unnecessary delay on shutdown and this fails to say so —
    /// which is the only warning a dependency changing underneath a comment
    /// will ever give.
    #[cfg(windows)]
    #[test]
    fn handle_identity_is_what_the_cancel_policy_assumes() {
        use std::os::windows::io::{AsHandle, AsRawHandle};

        let socket = client_test_socket("handle-identity");
        let listener = bind(&socket).expect("bind");
        let client = thread::spawn({
            let socket = socket.clone();
            move || connect_stream(&socket).expect("connect")
        });
        let stream = listener.accept().expect("accept");
        let (recv, send) = stream.split();
        let RecvHalf::NamedPipe(recv) = &recv;
        let SendHalf::NamedPipe(send) = &send;

        assert_eq!(
            recv.as_handle().as_raw_handle() as usize,
            send.as_handle().as_raw_handle() as usize,
            "the two halves report one handle, which is why cancelling the read \
             cancels the write — if this ever stops holding, `recv_cancel_due` \
             is holding shutdown back for nothing"
        );
        drop(client.join().expect("client thread"));
    }

    /// A send half that fails its first write, standing in for the real thing:
    /// `SO_SNDTIMEO` firing mid-frame on Unix, or the watchdog's `CancelIoEx` on
    /// a stuck write on Windows. Both surface to [`writer_loop`] as exactly this
    /// — an `Err` out of `write_all` with the frame only partly on the wire.
    struct FailingSink;

    impl Write for FailingSink {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "send timed out"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A send half whose bytes stay observable after the writer thread has taken
    /// ownership of it and exited. [`ConnectionIoState::spawn_writer`] consumes
    /// the send half exactly as `handle_conn` hands over the real one, so a bare
    /// `Vec<u8>` could not be read back afterwards.
    #[derive(Clone)]
    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl SharedSink {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn written(&self) -> String {
            String::from_utf8(lock_recover(&self.0).clone()).expect("frames are UTF-8")
        }
    }

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            lock_recover(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The zombie connection. When the writer died on a write error the reader
    /// only found out on its next `out_tx.send(...)` — which needs the client to
    /// send another request, and a client parked on the truncated frame sends
    /// nothing. So the connection lived on: an admission slot and a hub
    /// subscription held for the full 15-minute idle deadline, on both
    /// platforms (Unix waited out its 30 s poll arm, Windows had no recv timeout
    /// at all and waited for the watchdog's idle check).
    ///
    /// `tx` is deliberately still alive across the call: had the loop not left
    /// on the write error it would still be parked in `recv`, and the deadline
    /// reports that as a failure rather than hanging the suite.
    ///
    /// Spawned through [`ConnectionIoState::spawn_writer`] — production's own
    /// spawn — rather than by calling [`writer_loop`] with a state built here,
    /// so what is asserted afterwards is the state the reader and the watchdog
    /// would actually read. A writer reporting into an orphan state is the one
    /// way this fix can be inert while every other check still passes.
    #[test]
    fn a_writer_that_dies_on_a_write_error_tears_the_connection_down_before_idle() {
        let state = Arc::new(ConnectionIoState::new());
        let (tx, rx) = mpsc::sync_channel::<String>(OUT_QUEUE_CAP);
        tx.send("{\"ok\":true}\n".to_string())
            .expect("queue a response for the writer");

        let writer = state.spawn_writer(FailingSink, rx);
        with_deadline(
            "writer draining onto a socket that stopped accepting",
            move || writer.join().expect("the writer thread must not panic"),
        );

        assert!(
            state.write_failed(),
            "a write error is the writer's terminal exit and must be recorded; \
             nothing else on either platform can observe the dead writer"
        );
        assert!(
            lock_recover(&state.write_started).is_none(),
            "the failure must be recorded only after end_write, so no watchdog \
             can read it as a write still in flight"
        );
        // The decision the Unix reader arm and the Windows watchdog both make.
        assert!(
            recv_teardown_due(false, false, state.write_failed()),
            "the dead writer alone must end the wait — with neither shutdown \
             nor the 15-minute idle deadline to lean on, it is the only signal \
             there is"
        );
        drop(tx);
    }

    /// The other exit, which must NOT set the flag. `handle_conn` ends every
    /// healthy connection by dropping `out_tx` and joining the writer, so a flag
    /// set on both paths would mark every connection that ever closed as a
    /// failed writer — and the teardown signal would mean nothing.
    ///
    /// Doubles as the guard on the drain contract the shutdown refusal envelope
    /// depends on: a frame already queued when the channel closes still reaches
    /// the socket before the loop returns.
    #[test]
    fn the_clean_wind_down_is_not_reported_as_a_failed_writer() {
        let state = Arc::new(ConnectionIoState::new());
        let (tx, rx) = mpsc::sync_channel::<String>(OUT_QUEUE_CAP);
        tx.send("{\"ok\":true}\n".to_string())
            .expect("queue a response for the writer");
        drop(tx); // exactly what handle_conn does once its reader loop breaks.

        let sink = SharedSink::new();
        let writer = state.spawn_writer(sink.clone(), rx);
        with_deadline("writer draining a closed queue", move || {
            writer.join().expect("the writer thread must not panic")
        });

        assert_eq!(
            sink.written(),
            "{\"ok\":true}\n",
            "a frame queued before the channel closed must still be flushed"
        );
        assert!(
            !state.write_failed(),
            "the reader dropping out_tx is the healthy end of every connection, \
             not a write failure"
        );
        assert!(
            !recv_teardown_due(false, false, state.write_failed()),
            "a clean wind-down must not ask live connections to tear themselves \
             down"
        );
    }

    /// The shared policy behind the Unix reader's poll arm and the Windows
    /// watchdog's read-cancel. Each reason has to stand on its own: the defect
    /// this replaces was `shutdown || idle` written out inline in both places,
    /// where a dead writer was no reason at all.
    #[test]
    fn each_reason_to_stop_waiting_for_the_client_stands_alone() {
        assert!(
            !recv_teardown_due(false, false, false),
            "a healthy connection keeps waiting for its client's next request"
        );
        assert!(
            recv_teardown_due(true, false, false),
            "shutdown still ends the wait immediately"
        );
        assert!(
            recv_teardown_due(false, true, false),
            "the idle deadline still ends the wait"
        );
        assert!(
            recv_teardown_due(false, false, true),
            "a failed write ends the wait on its own: the client is parked on a \
             truncated frame and will never send the request that would reveal \
             the closed channel"
        );
    }

    /// A policy nobody consults is not a fix. The defect this branch closes was
    /// `shutdown || idle` written out inline in two places — the Unix reader's
    /// poll arm and the Windows watchdog's read-cancel — and the failure mode of
    /// a one-line fix to a duplicated condition is that it lands on one platform
    /// and the other's CI stays green over the hole. `#[cfg(windows)]` means a
    /// Linux build cannot even see the watchdog, so `dead_code` only catches
    /// losing *both* callers; nothing but this notices losing one.
    ///
    /// Read off `daemon.rs` itself rather than restated, in the same shape as
    /// the `dispatch.rs` guards: the call sites are the real thing, so the test
    /// asks them instead of a hand-copied list.
    #[test]
    fn both_platform_teardown_paths_consult_the_shared_policy() {
        const CALL: &str = "recv_teardown_due(";
        let production = include_str!("daemon.rs")
            .split_once("\nmod tests {")
            .expect("daemon.rs keeps its unit tests in a trailing `mod tests`")
            .0;

        let mut sites: Vec<(&str, &str)> = Vec::new();
        let mut at = 0;
        while let Some(offset) = production[at..].find(CALL) {
            let start = at + offset;
            at = start + CALL.len();
            if production[..start].ends_with("fn ") {
                continue; // the definition, not a call.
            }
            let mut depth = 1usize;
            let end = production[at..]
                .char_indices()
                .find_map(|(i, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    (depth == 0).then_some(at + i)
                })
                .expect("a call's argument list is parenthesis-balanced");
            // Nearest preceding `fn` line, indentation and visibility allowed:
            // matching only column-0 `fn` would walk past a call that moved into
            // an `impl` block and pin it on the previous top-level function —
            // a guard that names the wrong caller instead of failing.
            let enclosing = production[..start]
                .lines()
                .rev()
                .find_map(|line| {
                    let head = line.trim_start();
                    let head = head.strip_prefix("pub").map_or(head, |rest| {
                        rest.split_once(')')
                            .map_or(rest, |(_, after)| after)
                            .trim_start()
                    });
                    head.strip_prefix("fn ")
                })
                .map(|rest| {
                    &rest[..rest
                        .find(['<', '('])
                        .expect("a fn signature is followed by its generics or parameters")]
                })
                .expect("every call sits inside a fn");
            sites.push((enclosing, &production[at..end]));
        }

        assert_eq!(
            sites.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
            ["start_connection_watchdog", "handle_conn"],
            "both the Windows watchdog and the reader's poll arm must decide \
             through the shared policy, not re-spell it inline"
        );
        for (caller, args) in &sites {
            let signal = args
                .split(',')
                .map(str::trim)
                .find(|arg| arg.contains("write_failed"))
                .unwrap_or_else(|| {
                    panic!(
                        "{caller} passes {args:?}: the dead-writer signal must reach \
                         the policy, or that platform still waits out the idle deadline"
                    )
                });
            assert!(
                !signal.starts_with('!'),
                "{caller} passes {signal:?}: a negated signal tells the policy the \
                 writer is healthy at exactly the moment it has died"
            );
        }
    }

    #[test]
    fn idle_deadline_expires_at_the_boundary() {
        let started = std::time::Instant::now();
        assert!(!idle_expired(
            started,
            started + CLIENT_IDLE_TIMEOUT - Duration::from_millis(1)
        ));
        assert!(idle_expired(started, started + CLIENT_IDLE_TIMEOUT));
    }

    // ---- D5 idle shutdown ---------------------------------------------------

    /// Nothing in this tree auto-spawns a daemon yet, so every daemon that runs
    /// today was started by a human at a terminal. Shipping a default timeout
    /// would make those exit on their own, which is the surprise this must not
    /// be — and "off" has to be answerable by the predicate itself, not only by
    /// a caller that remembers to check first.
    #[test]
    fn idle_shutdown_is_off_unless_it_is_configured() {
        assert!(
            DaemonOptions::default().idle_timeout.is_none(),
            "a default daemon must not walk out on its operator"
        );
        // A year of quiet, expressed forwards: `Instant` has no representable
        // past before process start, so the arithmetic that reads naturally
        // (`now - a year`) panics on the machine this runs on.
        let quiet_since = Instant::now();
        assert!(
            !idle_shutdown_due(
                None,
                Some(quiet_since),
                quiet_since + Duration::from_secs(365 * 24 * 3600)
            ),
            "unconfigured means never, however long the quiet has lasted"
        );
    }

    /// The clock starts when the quiet starts and is not restarted by the quiet
    /// continuing. Re-stamping it on every idle observation is the failure that
    /// keeps a daemon alive forever while every unit around it still works.
    #[test]
    fn the_idle_clock_starts_once_and_runs_until_something_happens() {
        let t0 = Instant::now();
        let started = advance_idle_clock(None, false, t0).expect("quiet starts the clock");
        assert_eq!(started, t0);

        let later = t0 + Duration::from_secs(60);
        assert_eq!(
            advance_idle_clock(Some(t0), false, later),
            Some(t0),
            "still quiet: the deadline is measured from when the quiet began"
        );
        assert_eq!(
            advance_idle_clock(Some(t0), true, later),
            None,
            "a client arriving clears the clock"
        );
        assert_eq!(
            advance_idle_clock(None, true, later),
            None,
            "and a busy daemon never has one"
        );
    }

    /// The deadline is `>=`, like every other deadline in this file
    /// ([`idle_expired`], [`send_cancel_due`]): a timeout that only fires past
    /// its own value is a timeout nobody can assert on a boundary.
    #[test]
    fn an_idle_daemon_leaves_exactly_at_its_deadline() {
        let timeout = Duration::from_secs(15 * 60);
        let since = Instant::now();
        assert!(!idle_shutdown_due(
            Some(timeout),
            Some(since),
            since + timeout - Duration::from_millis(1)
        ));
        assert!(idle_shutdown_due(
            Some(timeout),
            Some(since),
            since + timeout
        ));
        assert!(
            !idle_shutdown_due(Some(timeout), None, since + timeout),
            "no idle stretch in progress is not an expired one"
        );
    }

    /// One assertion per reason to stay, because a dropped disjunct is invisible
    /// in a test that only checks the all-quiet case: three of the four terms
    /// could vanish and it would still answer `false` for an empty daemon and
    /// `true` for a busy one.
    #[test]
    fn every_reason_to_stay_up_stands_alone() {
        assert!(
            !server_busy(0, 0, false, false),
            "nothing connected, nothing scheduled, nothing listening: free to go"
        );
        assert!(
            server_busy(1, 0, false, false),
            "a connected client is work in hand"
        );
        assert!(
            server_busy(0, 1, false, false),
            "a subscriber would just stop receiving events, with no error to see"
        );
        assert!(
            server_busy(0, 0, true, false),
            "a reminder about to ripen is a delivery nothing else will make"
        );
        assert!(
            server_busy(0, 0, false, true),
            "the OTLP receiver's clients hold no connection here, so nothing else \
             in the predicate can see them"
        );
    }

    /// The reminder horizon, at its three interesting points. A reminder that
    /// already ripened counts: the scheduler fires it on its next tick, and that
    /// is precisely the moment a shutdown would race a delivery.
    #[test]
    fn a_reminder_inside_the_window_is_work_and_one_beyond_it_is_not() {
        let now: jiff::Timestamp = "2026-08-03T12:00:00Z".parse().unwrap();
        let horizon = Duration::from_secs(15 * 60);
        let at = |s: &str| Some(s.parse::<jiff::Timestamp>().unwrap());

        assert!(
            !reminder_due_within(None, now, horizon),
            "an empty heap must never pin the daemon"
        );
        assert!(
            reminder_due_within(at("2026-08-03T12:14:59Z"), now, horizon),
            "inside the window we would otherwise have stayed for"
        );
        assert!(
            !reminder_due_within(at("2026-08-03T12:15:01Z"), now, horizon),
            "a reminder further out than the timeout must not cancel the feature"
        );
        assert!(
            reminder_due_within(at("2026-08-03T11:59:00Z"), now, horizon),
            "already ripe, about to fire"
        );
    }

    fn frame(rx: &mpsc::Receiver<String>, what: &str) -> Value {
        let line = rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or_else(|_| panic!("{what}: nothing was pushed"));
        serde_json::from_str(line.trim()).expect("pushed frames are JSON")
    }

    /// A dropped broadcast must be *reported*, not merely survived. The bounded
    /// queue is the right memory bound, but the subscriber has no way to notice
    /// the loss: non-TTY `watch` emits one line per event and never resyncs, so
    /// a dropped row is a permanently missing record in a stream scripts tally.
    #[test]
    fn a_subscriber_that_missed_events_is_told_how_many_before_the_next_one() {
        let hub = Hub::new();
        let (tx, rx) = mpsc::sync_channel::<String>(2);
        hub.register(tx);

        hub.broadcast("a\n");
        hub.broadcast("b\n");
        // The subscriber is not draining: these three are lost.
        hub.broadcast("c\n");
        hub.broadcast("d\n");
        hub.broadcast("e\n");
        assert_eq!(rx.recv().unwrap(), "a\n");
        assert_eq!(rx.recv().unwrap(), "b\n");

        hub.broadcast("f\n");
        let marker = frame(&rx, "gap marker");
        assert_eq!(
            marker["event"], "task.changed.gap",
            "the gap must be its own event, not a counterfeit task.changed"
        );
        assert_eq!(
            marker["data"]["dropped"], 3,
            "the marker must carry the exact number of lost events"
        );
        assert_eq!(
            rx.recv().unwrap(),
            "f\n",
            "the marker precedes the first event the subscriber can receive again"
        );

        hub.broadcast("g\n");
        assert_eq!(
            rx.recv().unwrap(),
            "g\n",
            "a delivered marker clears the debt; it must not repeat"
        );
    }

    /// The marker itself goes through the same bounded queue, so it can be
    /// dropped too. If that reset the counter, the loss would become invisible
    /// again at exactly the moment it is largest.
    #[test]
    fn a_gap_marker_that_cannot_be_queued_keeps_the_debt() {
        let hub = Hub::new();
        let (tx, rx) = mpsc::sync_channel::<String>(1);
        hub.register(tx);

        hub.broadcast("a\n"); // fills the queue
        hub.broadcast("b\n"); // dropped (1)
        hub.broadcast("c\n"); // dropped (2)
        hub.broadcast("d\n"); // marker cannot be queued either; d dropped (3)
        assert_eq!(rx.recv().unwrap(), "a\n");

        hub.broadcast("e\n");
        let marker = frame(&rx, "deferred gap marker");
        assert_eq!(marker["event"], "task.changed.gap");
        assert_eq!(
            marker["data"]["dropped"], 3,
            "an undeliverable marker must not swallow the events it was counting"
        );
    }

    fn shared(engine: Engine) -> Shared {
        Shared {
            engine: Arc::new(Mutex::new(engine)),
            hub: Hub::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            watermark: Arc::new(Mutex::new(0)),
            next_reminder: Arc::new(Mutex::new(None)),
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
        engine
            .task_add(&json!({ "title": "bad event join" }))
            .unwrap();
        engine
            .conn()
            .execute("UPDATE tasks SET rev = 'not-an-integer'", [])
            .unwrap();
        let sh = shared(engine);

        let err = pump(&sh).expect_err("the malformed joined row must be surfaced");
        assert!(err.message.contains("storage error"), "{err:?}");
        assert_eq!(
            *lock_recover(&sh.watermark),
            0,
            "no failed batch may advance the watermark"
        );
    }

    #[test]
    fn repeated_transient_failures_log_only_on_state_transitions() {
        let mut state = ErrorTransition::default();
        assert!(state.enter(1, "disk busy"));
        assert!(
            !state.enter(1, "disk busy"),
            "the same failure must be rate-limited"
        );
        assert!(
            state.enter(1, "disk I/O"),
            "a changed failure is observable"
        );
        // Two ticks in which #1 reports nothing: it recovered.
        state.begin_tick();
        state.begin_tick();
        assert!(
            state.enter(1, "disk I/O"),
            "re-entering after recovery is observable"
        );
    }

    #[test]
    fn a_second_failing_task_does_not_defeat_the_failure_dedupe() {
        let mut state = ErrorTransition::default();
        assert!(state.enter(1, "could not record reminder for #1: disk busy"));
        assert!(state.enter(2, "could not record reminder for #2: disk busy"));
        state.begin_tick();
        assert!(
            !state.enter(1, "could not record reminder for #1: disk busy"),
            "two tasks failing in the same tick must not make every line a transition"
        );
        assert!(
            !state.enter(2, "could not record reminder for #2: disk busy"),
            "two tasks failing in the same tick must not make every line a transition"
        );
    }

    #[test]
    fn a_recovery_on_one_task_does_not_unsuppress_another() {
        let mut state = ErrorTransition::default();
        assert!(state.enter(1, "could not record reminder for #1: disk busy"));
        assert!(state.enter(2, "could not record reminder for #2: disk busy"));

        // Tick 2: #2 fires cleanly — it reports nothing at all — while #1 keeps
        // failing. #2's success must not speak for #1.
        state.begin_tick();
        assert!(
            !state.enter(1, "could not record reminder for #1: disk busy"),
            "another task's recovery must not re-arm the failing task's line"
        );

        // Tick 3: #2 fails again. That is genuinely new for #2 and prints, but
        // #1's ongoing failure must stay quiet.
        state.begin_tick();
        assert!(
            state.enter(2, "could not record reminder for #2: disk busy"),
            "a failure returning after recovery is observable"
        );
        assert!(
            !state.enter(1, "could not record reminder for #1: disk busy"),
            "another task's failure must not re-arm the failing task's line"
        );
    }

    /// The type can only key by task if the call site hands it the task. This
    /// pins the wiring: one tick with two failing reminders must leave two
    /// separately-suppressed subjects, not one slot they overwrite in turn.
    #[test]
    fn reminder_failures_are_tracked_per_task() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let first = add(&sh, "ship it", "2026-07-20T17:00:00Z", "-1h");
        let second = add(&sh, "pay rent", "2026-07-20T17:00:00Z", "-1h");
        let notifier = Collecting::default();
        let mut sched = ReminderScheduler::new();
        let mut fire_errors = ErrorTransition::default();
        let now = ts("2026-07-20T16:00:00Z");
        let always_fails =
            |_: &Engine, _: &Pending| -> Result<bool, ApiError> { Err(ApiError::internal("busy")) };

        let seen = reminder_tick(
            &sh,
            &mut sched,
            -1,
            now,
            &notifier,
            &always_fails,
            &mut fire_errors,
        )
        .unwrap();
        assert_eq!(seen, -1, "both failures force a rebuild");
        let mut subjects: Vec<i64> = fire_errors.current.keys().copied().collect();
        subjects.sort_unstable();
        assert_eq!(
            subjects,
            vec![first, second],
            "each failing task must own its suppression slot"
        );

        // Second tick, same two failures: both are now repeats and stay quiet.
        reminder_tick(
            &sh,
            &mut sched,
            seen,
            now,
            &notifier,
            &always_fails,
            &mut fire_errors,
        )
        .unwrap();
        for id in [first, second] {
            assert!(
                !fire_errors.enter(id, &format!("could not record reminder for #{id}: busy")),
                "#{id} must still be suppressed after a second identical tick"
            );
        }
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
        assert_eq!(
            stale["id"], 1,
            "request must ignore a response for another ID"
        );
        assert_eq!(stale["result"]["tasks"], json!(["first"]));

        let retained = conn
            .next_frame()
            .expect("retained frame")
            .expect("event frame");
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
        assert!(
            notifier.fired().is_empty(),
            "a failed write must not notify"
        );
        assert_eq!(
            seen, -1,
            "a failed fire must force the next tick to rebuild"
        );

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
        assert_eq!(
            notifier.fired(),
            vec![id],
            "the reminder must fire after a retry"
        );
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
        assert_ne!(
            seen, cur,
            "must not adopt a rowid the heap was never built from"
        );

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

    // ---- token attribution (#17) --------------------------------------------

    fn attr_test_dir(label: &str) -> std::path::PathBuf {
        let n = CLIENT_TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tasqx-attr-{label}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A Claude Code transcript with two in-window assistant lines (110 input,
    /// 220 output between them) and one far-future line that any real window
    /// excludes.
    fn transcript(in_window_ts: &str) -> String {
        [
            format!(
                r#"{{"timestamp":"{in_window_ts}","message":{{"id":"a","model":"claude-opus-4-8","usage":{{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}}}}}"#
            ),
            format!(
                r#"{{"timestamp":"{in_window_ts}","message":{{"id":"b","model":"claude-opus-4-8","usage":{{"input_tokens":100,"output_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
            ),
            r#"{"timestamp":"2099-01-01T00:00:00Z","message":{"id":"c","usage":{"input_tokens":9999,"output_tokens":9999}}}"#.to_string(),
        ]
        .join("\n")
    }

    fn add_task(sh: &Shared, title: &str) -> i64 {
        let g = lock_recover(&sh.engine);
        g.task_add(&json!({ "title": title }))
            .unwrap()
            .get("short_id")
            .and_then(Value::as_i64)
            .unwrap()
    }

    fn complete(sh: &Shared, params: Value) {
        let g = lock_recover(&sh.engine);
        g.task_done(&params).unwrap();
    }

    fn count(sh: &Shared, sql: &str) -> i64 {
        let g = lock_recover(&sh.engine);
        g.conn().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    const N_MEASUREMENTS: &str = "SELECT COUNT(*) FROM token_usage";
    const N_ATTRIBUTED: &str = "SELECT COUNT(*) FROM events WHERE op = 'tokens.attributed'";

    #[test]
    fn attribution_tick_measures_a_completed_task_from_its_transcript() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let dir = attr_test_dir("happy");
        // Claude Code names each transcript `<session-id>.jsonl`; naming the file
        // for the completion's session id is a verified correlation => HIGH.
        let path = dir.join("sess-1.jsonl");

        let id = add_task(&sh, "ship it");
        // Capture an instant AFTER creation and write the transcript at it; the
        // completion below closes the window at a later `now()`, so the sample is
        // provably inside [created, completed].
        let in_window = jiff::Timestamp::now().to_string();
        std::fs::write(&path, transcript(&in_window)).unwrap();
        complete(
            &sh,
            json!({
                "ref": id,
                "client": "claude-code",
                "session_id": "sess-1",
                "transcript_path": path.to_string_lossy(),
            }),
        );

        let mut errors = ErrorTransition::default();
        let seen = attribution_tick(
            &sh,
            -1,
            jiff::Timestamp::now(),
            &attribution::attribute_one,
            &mut errors,
        )
        .unwrap();
        assert_ne!(seen, -1, "a clean tick adopts a real watermark");

        assert_eq!(count(&sh, N_MEASUREMENTS), 1, "one measurement stored");
        assert_eq!(count(&sh, N_ATTRIBUTED), 1, "one tokens.attributed marker");
        let (source, input, output, conf) = {
            let g = lock_recover(&sh.engine);
            g.conn()
                .query_row(
                    "SELECT source, input_tokens, output_tokens, confidence FROM token_usage",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    },
                )
                .unwrap()
        };
        assert_eq!(source, "log-parse");
        assert_eq!(input, 110, "only the two in-window lines counted");
        assert_eq!(output, 220);
        assert_eq!(
            conf, "high",
            "explicit path + session id => high confidence"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reopen_and_re_complete_is_attributed_again_not_suppressed_by_the_old_marker() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let dir = attr_test_dir("reopen");

        let id = add_task(&sh, "ship it");

        // First completion + attribution: one measurement, one marker.
        let first_path = dir.join("sess-1.jsonl");
        std::fs::write(&first_path, transcript(&jiff::Timestamp::now().to_string())).unwrap();
        complete(
            &sh,
            json!({
                "ref": id,
                "client": "claude-code",
                "session_id": "sess-1",
                "transcript_path": first_path.to_string_lossy(),
            }),
        );
        let mut errors = ErrorTransition::default();
        attribution_tick(
            &sh,
            -1,
            jiff::Timestamp::now(),
            &attribution::attribute_one,
            &mut errors,
        )
        .unwrap();
        assert_eq!(count(&sh, N_MEASUREMENTS), 1, "first completion attributed");
        assert_eq!(count(&sh, N_ATTRIBUTED), 1);

        // Reopen (bug found), do more work, complete again with a fresh session.
        {
            let g = lock_recover(&sh.engine);
            g.task_reopen(&json!({ "ref": id })).unwrap();
        }
        let second_path = dir.join("sess-2.jsonl");
        std::fs::write(
            &second_path,
            transcript(&jiff::Timestamp::now().to_string()),
        )
        .unwrap();
        complete(
            &sh,
            json!({
                "ref": id,
                "client": "claude-code",
                "session_id": "sess-2",
                "transcript_path": second_path.to_string_lossy(),
            }),
        );

        // The stale marker must NOT suppress the post-reopen session: the new
        // `done` sits past the old marker, so the tick re-attributes.
        attribution_tick(
            &sh,
            -1,
            jiff::Timestamp::now(),
            &attribution::attribute_one,
            &mut errors,
        )
        .unwrap();
        assert_eq!(
            count(&sh, N_MEASUREMENTS),
            2,
            "the post-reopen session is attributed, not silently lost"
        );
        assert_eq!(count(&sh, N_ATTRIBUTED), 2, "a fresh marker per completion");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_tick_is_an_idempotent_no_op() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let dir = attr_test_dir("idem");
        let path = dir.join("session.jsonl");
        let id = add_task(&sh, "ship it");
        let in_window = jiff::Timestamp::now().to_string();
        std::fs::write(&path, transcript(&in_window)).unwrap();
        complete(
            &sh,
            json!({ "ref": id, "client": "claude-code", "transcript_path": path.to_string_lossy() }),
        );

        let mut errors = ErrorTransition::default();
        let now = jiff::Timestamp::now();
        let seen =
            attribution_tick(&sh, -1, now, &attribution::attribute_one, &mut errors).unwrap();
        assert_eq!(count(&sh, N_MEASUREMENTS), 1);
        // A second tick must not duplicate the row or the marker.
        let seen2 =
            attribution_tick(&sh, seen, now, &attribution::attribute_one, &mut errors).unwrap();
        assert_eq!(count(&sh, N_MEASUREMENTS), 1, "no duplicate measurement");
        assert_eq!(count(&sh, N_ATTRIBUTED), 1, "no duplicate marker");
        // Once everything settles the tick is a pure no-op.
        assert_eq!(
            attribution_tick(&sh, seen2, now, &attribution::attribute_one, &mut errors).unwrap(),
            seen2,
            "a settled tick neither rebuilds nor writes"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_client_terminates_with_a_zero_sample_marker() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let id = add_task(&sh, "cursor work");
        // Cursor has no local logs and no parser: a correlation is present, so
        // the task is a candidate, but attribution finds nothing to store.
        complete(&sh, json!({ "ref": id, "client": "cursor" }));

        let mut errors = ErrorTransition::default();
        attribution_tick(
            &sh,
            -1,
            jiff::Timestamp::now(),
            &attribution::attribute_one,
            &mut errors,
        )
        .unwrap();

        assert_eq!(
            count(&sh, N_MEASUREMENTS),
            0,
            "no measurement for an unknown tool"
        );
        assert_eq!(
            count(&sh, N_ATTRIBUTED),
            1,
            "but a marker so the task terminates"
        );
        // The marker records samples:0, not spend.
        let payload: String = {
            let g = lock_recover(&sh.engine);
            g.conn()
                .query_row(
                    "SELECT payload FROM events WHERE op = 'tokens.attributed'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert!(payload.contains("\"samples\":0"), "{payload}");
    }

    #[test]
    fn a_missing_transcript_is_transient_and_retried_not_fatal() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let dir = attr_test_dir("missing");
        let path = dir.join("late.jsonl");
        let id = add_task(&sh, "ship it");
        // Capture the in-window instant BEFORE completing — the window closes at
        // completion, so the eventual sample must predate it.
        let in_window = jiff::Timestamp::now().to_string();
        complete(
            &sh,
            json!({ "ref": id, "client": "claude-code", "transcript_path": path.to_string_lossy() }),
        );

        let mut errors = ErrorTransition::default();
        let now = jiff::Timestamp::now();
        // Tick 1: the transcript has not been flushed yet. This is transient —
        // no marker, no fatal (fatal is None here; the tick must still return Ok),
        // and the watermark is invalidated so the next tick retries.
        let seen =
            attribution_tick(&sh, -1, now, &attribution::attribute_one, &mut errors).unwrap();
        assert_eq!(seen, -1, "a not-yet-available transcript forces a retry");
        assert_eq!(count(&sh, N_ATTRIBUTED), 0, "no premature marker");
        assert_eq!(count(&sh, N_MEASUREMENTS), 0);

        // The transcript appears; the retry attributes it.
        std::fs::write(&path, transcript(&in_window)).unwrap();
        attribution_tick(&sh, seen, now, &attribution::attribute_one, &mut errors).unwrap();
        assert_eq!(
            count(&sh, N_MEASUREMENTS),
            1,
            "the retry lands the measurement"
        );
        assert_eq!(count(&sh, N_ATTRIBUTED), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_is_retried_on_the_next_tick() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let dir = attr_test_dir("flaky");
        let path = dir.join("session.jsonl");
        let id = add_task(&sh, "ship it");
        let in_window = jiff::Timestamp::now().to_string();
        std::fs::write(&path, transcript(&in_window)).unwrap();
        complete(
            &sh,
            json!({ "ref": id, "client": "claude-code", "transcript_path": path.to_string_lossy() }),
        );

        // The parse succeeds; only the store write fails the first time, the way
        // a busy-timeout would. The measurement must not be lost.
        let calls = AtomicUsize::new(0);
        let flaky = move |e: &Engine,
                          pa: &attribution::PendingAttribution,
                          res: &attribution::AttributionResult|
              -> Result<bool, ApiError> {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(ApiError::internal("db busy"));
            }
            attribution::attribute_one(e, pa, res)
        };

        let mut errors = ErrorTransition::default();
        let now = jiff::Timestamp::now();
        let seen = attribution_tick(&sh, -1, now, &flaky, &mut errors).unwrap();
        assert_eq!(seen, -1, "a failed write forces a rebuild next tick");
        assert_eq!(count(&sh, N_ATTRIBUTED), 0, "nothing recorded yet");

        attribution_tick(&sh, seen, now, &flaky, &mut errors).unwrap();
        assert_eq!(
            count(&sh, N_MEASUREMENTS),
            1,
            "the retry lands the measurement"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_tick_catches_up_tasks_completed_before_the_loop_started() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let dir = attr_test_dir("catchup");
        // Two tasks completed with correlation while no daemon (no tick) ran.
        let a = add_task(&sh, "task a");
        let b = add_task(&sh, "task b");
        let in_window = jiff::Timestamp::now().to_string();
        let pa = dir.join("a.jsonl");
        let pb = dir.join("b.jsonl");
        std::fs::write(&pa, transcript(&in_window)).unwrap();
        std::fs::write(&pb, transcript(&in_window)).unwrap();
        complete(
            &sh,
            json!({ "ref": a, "client": "claude-code", "transcript_path": pa.to_string_lossy() }),
        );
        complete(
            &sh,
            json!({ "ref": b, "client": "claude-code", "transcript_path": pb.to_string_lossy() }),
        );

        // The very first tick (seen = -1) is the whole-store catch-up scan.
        let mut errors = ErrorTransition::default();
        attribution_tick(
            &sh,
            -1,
            jiff::Timestamp::now(),
            &attribution::attribute_one,
            &mut errors,
        )
        .unwrap();
        assert_eq!(
            count(&sh, N_MEASUREMENTS),
            2,
            "both backlog tasks attributed"
        );
        assert_eq!(count(&sh, N_ATTRIBUTED), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_human_completion_without_correlation_is_never_attributed() {
        let sh = shared(Engine::open_in_memory().unwrap());
        let id = add_task(&sh, "just done");
        // `tasqx done 1` — no client, no session, no transcript.
        complete(&sh, json!({ "ref": id }));

        let mut errors = ErrorTransition::default();
        let seen = attribution_tick(
            &sh,
            -1,
            jiff::Timestamp::now(),
            &attribution::attribute_one,
            &mut errors,
        )
        .unwrap();
        assert_ne!(seen, -1, "nothing failed");
        assert_eq!(
            count(&sh, N_ATTRIBUTED),
            0,
            "no correlation => not a candidate"
        );
    }

    #[test]
    fn attribution_is_off_by_default_in_daemon_options() {
        assert!(
            !DaemonOptions::default().tokens_enabled,
            "token attribution must be opt-in (DESIGN §10)"
        );
    }
}
