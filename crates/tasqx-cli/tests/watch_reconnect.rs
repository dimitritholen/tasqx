//! #236.5: `tasqx watch` used to exit 1 the moment its daemon connection died
//! (a restart, an upgrade, `daemon.idle_timeout` firing) and never tried
//! again — a pane meant to be left open for a day went silently dead and
//! nobody noticed until it stopped moving. This drives a real `tasqx watch`
//! subprocess against a real `tasqx daemon` subprocess, kills that daemon
//! process outright (so the OS actually closes the socket, the way a real
//! restart or `kill` does), starts a second daemon on the same socket path,
//! and asserts the `watch` subprocess is still running throughout and resumes
//! streaming events once the second daemon answers.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn wait_for_socket(path: &str, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if std::path::Path::new(path).exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn spawn_daemon(sock: &str, db: &PathBuf) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .arg("daemon")
        .arg("--socket")
        .arg(sock)
        .arg("--db")
        .arg(db)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tasqx daemon");
    assert!(
        wait_for_socket(sock, Instant::now() + Duration::from_secs(10)),
        "daemon never created its socket at {sock}"
    );
    child
}

#[test]
fn watch_reconnects_after_its_daemon_dies_instead_of_exiting() {
    let stem = format!("tasqx-watchrc-{}", std::process::id());
    let db = std::env::temp_dir().join(format!("{stem}.db"));
    let sock = std::env::temp_dir()
        .join(format!("{stem}.sock"))
        .to_string_lossy()
        .into_owned();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&sock);

    let mut daemon1 = spawn_daemon(&sock, &db);

    let mut watch = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .args(["watch", "@working"])
        .env("TASQX_SOCK", &sock)
        .env("TASQX_DB", &db)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tasqx watch");

    let stdout = BufReader::new(watch.stdout.take().expect("piped stdout"));
    let stderr = BufReader::new(watch.stderr.take().expect("piped stderr"));
    let (out_tx, out_rx) = mpsc::channel::<String>();
    let (err_tx, err_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in stdout.lines().map_while(Result::ok) {
            if out_tx.send(line).is_err() {
                break;
            }
        }
    });
    thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            if err_tx.send(line).is_err() {
                break;
            }
        }
    });

    // The initial paint proves the subprocess connected and subscribed.
    out_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("watch must paint the working set once connected");

    // Kill daemon 1 outright — a real process death, the way a restart, an
    // upgrade or `kill` ends one, so the OS actually closes `watch`'s socket
    // rather than leaving it half-open.
    let _ = daemon1.kill();
    let _ = daemon1.wait();
    // A SIGKILLed daemon leaves its socket file behind, so wait_for_socket
    // would otherwise return before daemon 2 is actually listening.
    let _ = std::fs::remove_file(&sock);

    // While nothing is listening, the fixed `watch` must still be running —
    // the pre-fix behaviour was `exit(1)` the instant the read failed.
    thread::sleep(Duration::from_millis(500));
    assert!(
        watch.try_wait().expect("poll child status").is_none(),
        "watch must not exit when its daemon connection dies; it should retry"
    );

    // The reconnect status line must appear on stderr while no daemon answers.
    let saw_reconnecting = {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw = false;
        while Instant::now() < deadline {
            match err_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    if line.contains("reconnecting") {
                        saw = true;
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        saw
    };
    assert!(
        saw_reconnecting,
        "expected a visible reconnecting status line on stderr while no daemon answers"
    );

    // Bring a second daemon up on the exact same socket path and prove watch
    // resumes: adding a task must produce a fresh event/repaint on stdout.
    let mut daemon2 = spawn_daemon(&sock, &db);
    {
        let adder = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_SOCK", &sock)
            // The in-process fallback when the connect fails is silent, so
            // TASQX_DB is the belt to TASQX_SOCK's braces: even on fallback
            // this must open the test's own temp db, never the real store.
            .env("TASQX_DB", &db)
            .args(["add", "reconnect proof"])
            .output()
            .expect("add through the reconnected daemon");
        assert!(
            adder.status.success(),
            "add must succeed against the second daemon: {}",
            String::from_utf8_lossy(&adder.stderr)
        );
    }

    let resumed = out_rx.recv_timeout(Duration::from_secs(15));
    assert!(
        resumed.is_ok(),
        "watch must resume streaming once a new daemon answers on the same socket"
    );

    let _ = watch.kill();
    let _ = watch.wait();
    let _ = daemon2.kill();
    let _ = daemon2.wait();
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(&sock);
}
