//! Tauri host for Tasqx Desktop.
//!
//! Thin by design (DESIGN.md D160): the host owns the OS seam — the daemon
//! socket on Unix, the named pipe on Windows — plus the one local file the
//! graph's saved views live in, and nothing else. It never
//! parses a frame, never knows a method name, and never applies a task rule.
//! Lines go out as the frontend wrote them and come back as the daemon sent
//! them, over the `tasqx://line` / `tasqx://closed` events.

use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// The connection handle. `std::os::unix::net::UnixStream` and `std::fs::File`
/// both read, write and `try_clone`, which is the whole interface used here, so
/// one code path covers both platforms.
#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::fs::File;

/// Emitted per line the daemon sends.
const EVENT_LINE: &str = "tasqx://line";
/// Emitted exactly once when a connection ends, with the reason as payload.
const EVENT_CLOSED: &str = "tasqx://closed";

/// The error string `daemon_send` returns when there is no connection; the
/// client maps it onto its `transport_unavailable` error code.
const TRANSPORT_UNAVAILABLE: &str = "transport_unavailable";

#[derive(Serialize)]
pub struct ConnectInfo {
    /// The address actually used, after defaulting.
    socket: String,
}

/// The single live connection. `generation` rises on every connect and
/// disconnect, so the reader thread of a replaced connection can recognise
/// itself as stale and stay silent instead of emitting a `tasqx://closed` that
/// would tear down the connection that replaced it.
#[derive(Default)]
struct Daemon {
    writer: Mutex<Option<Stream>>,
    generation: AtomicU64,
}

/// Resolve the socket address from an explicit `$TASQX_SOCK` value: a
/// non-empty one wins, anything else falls through to the platform default.
/// Split out from the env read so the rule is testable.
fn socket_from_env(env: Option<&str>) -> String {
    match env {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => platform_default_socket(),
    }
}

/// The stable default address, mirroring `default_socket` in
/// crates/tasqx-cli/src/backend.rs — the two must not drift, or the desktop
/// looks for a daemon where the CLI never starts one.
fn platform_default_socket() -> String {
    #[cfg(windows)]
    {
        "tasqx-default".to_string()
    }
    #[cfg(unix)]
    {
        if let Some(dirs) = directories::ProjectDirs::from("dev", "tasqx", "tasqx") {
            if let Some(rt) = dirs.runtime_dir() {
                return rt.join("tasqx.sock").to_string_lossy().into_owned();
            }
            return dirs
                .data_dir()
                .join("tasqx.sock")
                .to_string_lossy()
                .into_owned();
        }
        "/tmp/tasqx.sock".to_string()
    }
}

/// Open a client stream. Unix connects to the socket path; Windows opens the
/// named pipe for read and write, which is what `connect_stream` in
/// crates/tasqx-core/src/daemon.rs does through `interprocess`. A bare name is
/// given the `\\.\pipe\` prefix the daemon listens on.
#[cfg(unix)]
fn open_stream(socket: &str) -> std::io::Result<Stream> {
    Stream::connect(socket)
}

#[cfg(windows)]
fn open_stream(socket: &str) -> std::io::Result<Stream> {
    let path = if socket.starts_with(r"\\") {
        socket.to_string()
    } else {
        format!(r"\\.\pipe\{socket}")
    };
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
}

/// Unblock a reader parked in `read` on the cloned handle. A Unix shutdown
/// reaches every clone; a named pipe offers no equivalent, so there the reader
/// wakes when the daemon closes its end and the generation guard keeps it quiet
/// until then.
fn shutdown(stream: &Stream) {
    #[cfg(unix)]
    let _ = stream.shutdown(std::net::Shutdown::Both);
    #[cfg(windows)]
    let _ = stream;
}

/// Drop any live connection and invalidate its reader thread.
fn close_connection(daemon: &Daemon) {
    daemon.generation.fetch_add(1, Ordering::SeqCst);
    let taken = daemon.writer.lock().expect("daemon writer lock").take();
    if let Some(stream) = taken {
        shutdown(&stream);
    }
}

/// One line per daemon frame until EOF or an I/O error, then one close event.
fn read_lines(app: AppHandle, daemon: Arc<Daemon>, reader: Stream, generation: u64) {
    let current = || daemon.generation.load(Ordering::SeqCst) == generation;
    let mut reason = "eof".to_string();
    for line in BufReader::new(reader).lines() {
        if !current() {
            return;
        }
        match line {
            Ok(line) => {
                let _ = app.emit(EVENT_LINE, line);
            }
            Err(err) => {
                reason = err.to_string();
                break;
            }
        }
    }
    if current() {
        close_connection(&daemon);
        let _ = app.emit(EVENT_CLOSED, reason);
    }
}

/// `$TASQX_SOCK` if non-empty, else the platform default.
#[tauri::command]
fn daemon_default_socket() -> String {
    socket_from_env(std::env::var("TASQX_SOCK").ok().as_deref())
}

/// Connect, keep the writer, and start the one reader thread. A second call
/// replaces the previous connection.
#[tauri::command]
fn daemon_connect(
    app: AppHandle,
    daemon: State<'_, Arc<Daemon>>,
    socket: Option<String>,
) -> Result<ConnectInfo, String> {
    let socket = match socket {
        Some(s) if !s.is_empty() => s,
        _ => daemon_default_socket(),
    };
    close_connection(&daemon);

    let stream = open_stream(&socket).map_err(|e| e.to_string())?;
    let reader = stream.try_clone().map_err(|e| e.to_string())?;
    let generation = daemon.generation.load(Ordering::SeqCst);
    *daemon.writer.lock().expect("daemon writer lock") = Some(stream);

    let daemon = Arc::clone(&daemon);
    std::thread::spawn(move || read_lines(app, daemon, reader, generation));
    Ok(ConnectInfo { socket })
}

/// Write one newline-terminated frame.
#[tauri::command]
fn daemon_send(daemon: State<'_, Arc<Daemon>>, line: String) -> Result<(), String> {
    let mut guard = daemon.writer.lock().expect("daemon writer lock");
    let stream = guard.as_mut().ok_or(TRANSPORT_UNAVAILABLE)?;
    let write = stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
        .and_then(|()| stream.flush());
    write.map_err(|e| e.to_string())
}

#[tauri::command]
fn daemon_disconnect(daemon: State<'_, Arc<Daemon>>) -> Result<(), String> {
    close_connection(&daemon);
    Ok(())
}

/// Saved graph views (D160): presentation state in `appDataDir()`, never in
/// the tasqx store. The host only moves bytes — what the JSON means, and when
/// a file is corrupt, is decided in src/state/graphViews.ts.
const VIEWS_FILE: &str = "graph-views.json";

fn views_read_in(dir: &std::path::Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(dir.join(VIEWS_FILE)) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Write a temp file beside the target, flush it to disk, then rename it over:
/// a crash mid-write leaves the old file or the new one, never half of either.
fn views_write_in(dir: &std::path::Path, contents: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{VIEWS_FILE}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(VIEWS_FILE))
}

/// Move an unreadable file aside as `graph-views.json.corrupt-<unix secs>.bak`
/// and answer that name. The stamp keeps an earlier casualty from being
/// overwritten; the `.bak` suffix is the one D160 names.
fn views_quarantine_in(dir: &std::path::Path, stamp: u64) -> std::io::Result<String> {
    let name = format!("{VIEWS_FILE}.corrupt-{stamp}.bak");
    std::fs::rename(dir.join(VIEWS_FILE), dir.join(&name))?;
    Ok(name)
}

fn app_data_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

#[tauri::command]
fn graph_views_read(app: AppHandle) -> Result<Option<String>, String> {
    views_read_in(&app_data_dir(&app)?).map_err(|e| e.to_string())
}

#[tauri::command]
fn graph_views_write(app: AppHandle, contents: String) -> Result<(), String> {
    views_write_in(&app_data_dir(&app)?, &contents).map_err(|e| e.to_string())
}

#[tauri::command]
fn graph_views_quarantine(app: AppHandle) -> Result<String, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    views_quarantine_in(&app_data_dir(&app)?, stamp).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(Arc::new(Daemon::default()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            daemon_default_socket,
            daemon_connect,
            daemon_send,
            daemon_disconnect,
            graph_views_read,
            graph_views_write,
            graph_views_quarantine
        ])
        .run(tauri::generate_context!())
        .expect("error while running tasqx desktop");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_socket_wins_only_when_non_empty() {
        assert_eq!(
            socket_from_env(Some("/tmp/custom.sock")),
            "/tmp/custom.sock"
        );
        let default = platform_default_socket();
        assert_eq!(socket_from_env(Some("")), default);
        assert_eq!(socket_from_env(None), default);
        assert!(!default.is_empty());
    }

    #[test]
    fn views_file_is_replaced_whole_and_moved_aside_on_demand() {
        let dir = std::env::temp_dir().join(format!("tasqx-views-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(views_read_in(&dir).unwrap(), None);
        views_write_in(&dir, "{\"schema\":1,\"views\":[]}").unwrap();
        views_write_in(&dir, "second").unwrap();
        assert_eq!(views_read_in(&dir).unwrap().as_deref(), Some("second"));
        assert!(!dir.join(format!("{VIEWS_FILE}.tmp")).exists());

        let name = views_quarantine_in(&dir, 42).unwrap();
        assert_eq!(name, "graph-views.json.corrupt-42.bak");
        assert_eq!(std::fs::read_to_string(dir.join(&name)).unwrap(), "second");
        assert_eq!(views_read_in(&dir).unwrap(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
