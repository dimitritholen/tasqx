//! The connector runner against connectors that misbehave on purpose (D198).
//!
//! `harness = false`, and that is the point of the file rather than a style
//! choice: the fakes have to be real executables on Linux, macOS AND Windows,
//! so a shell script will not do, and under libtest a re-executed test binary
//! prints `running 1 test` on stdout before the fake gets a word in — which is
//! exactly the malformed-reply case the runner is meant to reject. So this
//! binary is its own fake: with `TASQX_FAKE_CONNECTOR` set it behaves as the
//! named connector and exits; without it, it runs the cases below.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tasqx_core::exec::Limits;
use tasqx_core::remote::{Configured, Connector, Error};

const FAKE: &str = "TASQX_FAKE_CONNECTOR";

fn main() {
    if let Ok(mode) = std::env::var(FAKE) {
        fake(&mode);
        return;
    }
    let cases: &[(&str, fn())] = &[
        (
            "a_hung_connector_is_killed_at_the_timeout",
            hung_connector_is_killed,
        ),
        (
            "stdout_past_the_cap_is_refused_and_the_child_killed",
            stdout_flood_is_refused,
        ),
        (
            "a_reply_that_is_not_json_is_refused",
            garbage_reply_is_refused,
        ),
        (
            "a_reply_in_another_protocol_is_refused",
            wrong_protocol_is_refused,
        ),
        (
            "a_reply_with_no_protocol_is_refused",
            missing_protocol_is_refused,
        ),
        (
            "a_snapshot_outside_out_dir_is_refused",
            snapshot_outside_out_dir_is_refused,
        ),
        (
            "a_relative_snapshot_climbing_out_is_refused",
            relative_escape_is_refused,
        ),
        (
            "a_failing_connector_surfaces_its_stderr",
            stderr_is_surfaced,
        ),
        (
            "a_long_stderr_is_truncated_keeping_its_end",
            long_stderr_is_truncated,
        ),
        (
            "a_conflict_that_exits_zero_is_refused",
            conflict_must_exit_two,
        ),
        (
            "exit_two_without_a_conflict_reply_is_refused",
            exit_two_must_say_conflict,
        ),
        (
            "the_request_carries_protocol_and_the_state_dir",
            request_and_env_are_passed,
        ),
        (
            "configure_passes_values_and_reads_a_rejection",
            configure_rejection_is_read,
        ),
        (
            "a_describe_with_duplicate_keys_is_refused",
            duplicate_field_keys_are_refused,
        ),
        (
            "connectors_are_found_on_path_by_name",
            connectors_are_found_on_path,
        ),
        (
            "a_connector_name_that_is_not_a_slug_is_refused",
            bad_names_are_refused,
        ),
        (
            "the_state_dir_lives_beside_the_store",
            state_dir_lives_beside_the_store,
        ),
        #[cfg(unix)]
        (
            "a_non_executable_file_on_path_does_not_shadow_the_real_one",
            a_non_executable_candidate_is_skipped,
        ),
        #[cfg(unix)]
        (
            "a_fresh_state_dir_and_remotes_are_created_0700",
            a_fresh_state_dir_is_private,
        ),
        #[cfg(unix)]
        (
            "a_group_readable_state_dir_is_tightened_to_0700",
            an_open_state_dir_is_tightened,
        ),
    ];
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, case) in cases {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        ran += 1;
        match std::panic::catch_unwind(case) {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failed.push(*name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        eprintln!("failures: {failed:?}");
        std::process::exit(101);
    }
}

// ---------------------------------------------------------------------------
// The fakes. Each reads its whole request first, as a real connector would.
// ---------------------------------------------------------------------------

fn fake(mode: &str) {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let request: serde_json::Value = serde_json::from_str(&input).unwrap();
    let out = &mut std::io::stdout();
    match mode {
        "sleep" => std::thread::sleep(Duration::from_secs(60)),
        "flood" => {
            let chunk = vec![b'x'; 64 * 1024];
            // Forever: only the runner's kill ends it.
            loop {
                if out.write_all(&chunk).is_err() {
                    std::process::exit(1);
                }
            }
        }
        "garbage" => print!("this is not json"),
        "protocol2" => print!(r#"{{"protocol":2,"snapshots":[]}}"#),
        "noproto" => print!(r#"{{"snapshots":[]}}"#),
        "escape" => {
            let out_dir = PathBuf::from(request["out_dir"].as_str().unwrap());
            let outside = out_dir.parent().unwrap().join("outside.blob");
            std::fs::write(&outside, b"blob").unwrap();
            print!(
                "{}",
                serde_json::json!({"protocol":1,"snapshots":[{"version":"v1","path":outside}]})
            );
        }
        "traverse" => {
            let out_dir = PathBuf::from(request["out_dir"].as_str().unwrap());
            std::fs::write(out_dir.parent().unwrap().join("up.blob"), b"blob").unwrap();
            print!(
                "{}",
                serde_json::json!({"protocol":1,"snapshots":[{"version":"v1","path":"../up.blob"}]})
            );
        }
        "fail" => {
            eprintln!("boom: the disk is on fire");
            std::process::exit(1);
        }
        "longstderr" => {
            let noise = "noise ".repeat(20_000);
            eprint!("{noise}");
            eprint!("THE-REAL-REASON");
            std::process::exit(1);
        }
        "conflict0" => print!(r#"{{"protocol":1,"conflict":true}}"#),
        "exit2" => {
            print!(r#"{{"protocol":1,"version":"v9"}}"#);
            out.flush().unwrap();
            std::process::exit(2);
        }
        "record" => {
            let dir = std::env::var("TASQX_REMOTE_STATE_DIR").unwrap();
            std::fs::write(Path::new(&dir).join("last-request.json"), &input).unwrap();
            print!(
                "{}",
                serde_json::json!({
                    "protocol": 1, "name": "fake", "version": "0.0.1",
                    "fields": [{"key": "path", "label": "Folder", "secret": false, "help": "where"}]
                })
            );
        }
        "reject" => {
            let got = request["values"]["path"].as_str().unwrap_or("<none>");
            print!(
                "{}",
                serde_json::json!({"protocol":1,"ok":false,"error":format!("cannot use {got}")})
            );
        }
        "dupkeys" => print!(
            "{}",
            serde_json::json!({
                "protocol": 1, "name": "fake", "version": "0.0.1",
                "fields": [
                    {"key": "a", "label": "A", "secret": false, "help": ""},
                    {"key": "a", "label": "A again", "secret": true, "help": ""}
                ]
            })
        ),
        other => panic!("unknown fake mode {other}"),
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn scratch(label: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "tasqx-remote-runner-{label}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A connector that is this binary acting as fake `mode`.
fn fake_connector(mode: &str, state: &Path) -> Connector {
    Connector::new(std::env::current_exe().unwrap(), state.to_path_buf())
        .with_env(FAKE, mode)
        .with_limits(Limits {
            timeout: Duration::from_secs(20),
            ..Limits::default()
        })
}

fn pull_err(mode: &str) -> Error {
    let dir = scratch(mode);
    let c = fake_connector(mode, &dir.join("state"));
    c.pull(&dir.join("out"))
        .expect_err("the runner must refuse this reply")
}

// ---------------------------------------------------------------------------
// Cases.
// ---------------------------------------------------------------------------

fn hung_connector_is_killed() {
    let dir = scratch("sleep");
    let c = fake_connector("sleep", &dir.join("state")).with_limits(Limits {
        timeout: Duration::from_millis(300),
        ..Limits::default()
    });
    let started = Instant::now();
    let err = c
        .describe()
        .expect_err("a hung connector must not hang tasqx");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the kill came late: {:?}",
        started.elapsed()
    );
    let msg = err.to_string();
    assert!(msg.contains("did not answer within"), "{msg}");
}

fn stdout_flood_is_refused() {
    let dir = scratch("flood");
    let c = fake_connector("flood", &dir.join("state")).with_limits(Limits {
        max_stdout: 128 * 1024,
        ..Limits::default()
    });
    let started = Instant::now();
    let msg = c
        .describe()
        .expect_err("a flood must be refused")
        .to_string();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(msg.contains("more than 131072 bytes"), "{msg}");
}

fn garbage_reply_is_refused() {
    let msg = pull_err("garbage").to_string();
    assert!(msg.contains("not a JSON reply"), "{msg}");
}

fn wrong_protocol_is_refused() {
    let msg = pull_err("protocol2").to_string();
    assert!(
        msg.contains("protocol 2") && msg.contains("protocol 1"),
        "{msg}"
    );
}

fn missing_protocol_is_refused() {
    let msg = pull_err("noproto").to_string();
    assert!(msg.contains("no \"protocol\""), "{msg}");
}

fn snapshot_outside_out_dir_is_refused() {
    let err = pull_err("escape");
    assert!(matches!(err, Error::PathOutside { .. }), "{err}");
}

fn relative_escape_is_refused() {
    let err = pull_err("traverse");
    assert!(matches!(err, Error::PathOutside { .. }), "{err}");
}

fn stderr_is_surfaced() {
    let msg = pull_err("fail").to_string();
    assert!(msg.contains("boom: the disk is on fire"), "{msg}");
    assert!(msg.contains("exit 1") || msg.contains("exited 1"), "{msg}");
}

fn long_stderr_is_truncated() {
    let msg = pull_err("longstderr").to_string();
    assert!(
        msg.contains("THE-REAL-REASON"),
        "the end is what explains it"
    );
    assert!(
        msg.len() < 4096,
        "stderr was not truncated: {} bytes",
        msg.len()
    );
}

fn conflict_must_exit_two() {
    let dir = scratch("conflict0");
    let blob = dir.join("blob");
    std::fs::write(&blob, b"x").unwrap();
    let c = fake_connector("conflict0", &dir.join("state"));
    let msg = c.push(&blob, None).expect_err("must refuse").to_string();
    assert!(msg.contains("exit 2"), "{msg}");
}

fn exit_two_must_say_conflict() {
    let dir = scratch("exit2");
    let blob = dir.join("blob");
    std::fs::write(&blob, b"x").unwrap();
    let c = fake_connector("exit2", &dir.join("state"));
    let msg = c
        .push(&blob, Some("v1"))
        .expect_err("must refuse")
        .to_string();
    assert!(msg.contains("exit 2"), "{msg}");
}

fn request_and_env_are_passed() {
    let dir = scratch("record");
    let state = dir.join("state");
    let c = fake_connector("record", &state);
    let d = c.describe().unwrap();
    assert_eq!(d.name, "fake");
    assert_eq!(d.fields[0].key, "path");
    assert!(!d.fields[0].secret);
    // The runner created the state dir and named it in the child's env.
    let seen: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("last-request.json")).unwrap()).unwrap();
    assert_eq!(seen, serde_json::json!({"protocol": 1, "verb": "describe"}));
}

fn configure_rejection_is_read() {
    let dir = scratch("reject");
    let c = fake_connector("reject", &dir.join("state"));
    let values = BTreeMap::from([("path".to_string(), "/nowhere".to_string())]);
    match c.configure(&values).unwrap() {
        Configured::Rejected(e) => assert_eq!(e, "cannot use /nowhere"),
        Configured::Ok => panic!("a rejection read as success"),
    }
}

fn duplicate_field_keys_are_refused() {
    let dir = scratch("dupkeys");
    let c = fake_connector("dupkeys", &dir.join("state"));
    let msg = c.describe().expect_err("must refuse").to_string();
    assert!(msg.contains("\"a\"") && msg.contains("twice"), "{msg}");
}

fn connectors_are_found_on_path() {
    let dir = scratch("path");
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let file = format!("tasqx-remote-dir{}", std::env::consts::EXE_SUFFIX);
    std::fs::write(b.join(&file), b"").unwrap();
    make_executable(&b.join(&file));
    let path = std::env::join_paths([&a, &b]).unwrap();
    let found = tasqx_core::remote::find_in("dir", &path).expect("on the second entry");
    assert_eq!(found, b.join(&file));
    assert!(tasqx_core::remote::find_in("s3", &path).is_none());
}

/// Give `path` its owner's execute bit; nothing to do on Windows, where the
/// suffix is what makes a file runnable.
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// A stray file of the right name without an execute bit, earlier on PATH,
/// must not shadow the real connector behind it: running it fails with
/// "Permission denied" and the user never learns why.
#[cfg(unix)]
fn a_non_executable_candidate_is_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("path-noexec");
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("tasqx-remote-dir"), b"").unwrap();
    std::fs::set_permissions(
        a.join("tasqx-remote-dir"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    std::fs::write(b.join("tasqx-remote-dir"), b"").unwrap();
    make_executable(&b.join("tasqx-remote-dir"));
    let path = std::env::join_paths([&a, &b]).unwrap();
    assert_eq!(
        tasqx_core::remote::find_in("dir", &path),
        Some(b.join("tasqx-remote-dir"))
    );
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Connectors keep credentials in their state dir, so nobody but the user may
/// list or read it — and `remotes/` above it is created the same way.
#[cfg(unix)]
fn a_fresh_state_dir_is_private() {
    let dir = scratch("private-fresh");
    let state = dir.join("remotes").join("fake");
    fake_connector("record", &state).describe().unwrap();
    assert_eq!(mode_of(&state), 0o700, "the state dir");
    assert_eq!(mode_of(&dir.join("remotes")), 0o700, "remotes/");
}

#[cfg(unix)]
fn an_open_state_dir_is_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("private-open");
    let state = dir.join("remotes").join("fake");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    fake_connector("record", &state).describe().unwrap();
    assert_eq!(mode_of(&state), 0o700);
}

fn bad_names_are_refused() {
    let dir = scratch("names");
    for bad in ["", "../x", "a/b", "A", "-x", "a b"] {
        let err = Connector::find(bad, dir.join("state")).expect_err(bad);
        assert!(matches!(err, Error::BadName { .. }), "{bad}: {err}");
    }
}

fn state_dir_lives_beside_the_store() {
    let db = Path::new("/scratch/store/tasks.db");
    assert_eq!(
        tasqx_core::remote::state_dir(db, "dir").unwrap(),
        Path::new("/scratch/store/remotes/dir")
    );
    assert!(tasqx_core::remote::state_dir(db, "../evil").is_err());
}
