//! SIGTERM must shut the daemon down exactly like SIGINT does: log
//! `tasqx daemon: stopped` and unlink the Unix socket. Before this fix, only
//! SIGINT (Ctrl-C) was handled — `ctrlc::set_handler` without the
//! `termination` feature installs a Unix handler for SIGINT alone, so a plain
//! `kill <pid>` (SIGTERM, what systemd/docker/supervisord send) killed the
//! process outright and left the socket file behind as a false "daemon is
//! up" signal (audit #236, finding 1).
//!
//! Unix-only: signals are a Unix concept, and the daemon's socket is a Unix
//! domain socket there (a named pipe on Windows, which this scenario does not
//! apply to).
#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn wait_for_socket(path: &std::path::Path, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn sigterm_stops_the_daemon_and_removes_the_socket() {
    let stem = format!("tasqx-sigterm-{}", std::process::id());
    let db = std::env::temp_dir().join(format!("{stem}.db"));
    let sock = std::env::temp_dir().join(format!("{stem}.sock"));
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&sock);

    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .arg("daemon")
        .arg("--socket")
        .arg(&sock)
        .arg("--db")
        .arg(&db)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tasqx daemon");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        wait_for_socket(&sock, deadline),
        "daemon never created its socket at {sock:?}"
    );

    // The real signal, not the ctrlc crate's own test hook: `kill(2)` with
    // SIGTERM is exactly what systemd `stop`, `docker stop`, supervisord and a
    // plain `kill <pid>` send — the case the finding is about.
    let pid = child.id();
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("run kill -TERM");
    assert!(status.success(), "kill -TERM {pid} failed to send");

    let exit = child.wait().expect("wait for daemon to exit");
    assert!(
        exit.success(),
        "daemon should exit 0 on a clean SIGTERM shutdown, got {exit:?}"
    );

    let mut stderr = String::new();
    use std::io::Read;
    child
        .stderr
        .take()
        .expect("captured stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    assert!(
        stderr.contains("tasqx daemon: stopped"),
        "expected the same clean-shutdown line SIGINT produces, got: {stderr}"
    );
    assert!(
        !sock.exists(),
        "SIGTERM must unlink the socket the way SIGINT does, left behind: {sock:?}"
    );

    let _ = std::fs::remove_file(&db);
}
