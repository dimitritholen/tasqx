//! `tasqx-remote-dir` against the connector conformance suite (D198), plus the
//! folder-specific behaviour the suite cannot know about: a HEAD that names a
//! blob the sync tool has not delivered yet, and pruning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use tasqx_core::remote::conformance::{self, Setup};
use tasqx_core::remote::{Configured, Connector, PushOutcome};

const BIN: &str = env!("CARGO_BIN_EXE_tasqx-remote-dir");

fn scratch(label: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "tasqx-remote-dir-{label}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn values(path: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([("path".to_string(), path.display().to_string())])
}

/// A configured connector over a fresh remote folder.
fn configured(label: &str) -> (PathBuf, PathBuf, Connector) {
    let work = scratch(label);
    let remote = work.join("remote");
    let c = Connector::new(BIN.into(), work.join("state"));
    assert!(matches!(
        c.configure(&values(&remote)).unwrap(),
        Configured::Ok
    ));
    (work, remote, c)
}

fn push_bytes(c: &Connector, work: &Path, bytes: &[u8], expected: Option<&str>) -> PushOutcome {
    let blob = work.join("outgoing");
    std::fs::write(&blob, bytes).unwrap();
    c.push(&blob, expected).unwrap()
}

fn version(outcome: PushOutcome) -> String {
    match outcome {
        PushOutcome::Pushed { version } => version,
        PushOutcome::Conflict => panic!("unexpected conflict"),
    }
}

#[test]
fn the_folder_connector_passes_the_conformance_suite() {
    let work = scratch("conformance");
    let a_file = work.join("a-file");
    std::fs::write(&a_file, b"not a folder").unwrap();
    let setup = Setup {
        program: BIN.into(),
        work_dir: work.join("suite"),
        good: values(&work.join("remote")),
        bad: vec![
            BTreeMap::new(),
            values(&a_file),
            values(Path::new("relative/folder")),
            BTreeMap::from([
                ("path".to_string(), work.join("r2").display().to_string()),
                ("region".to_string(), "eu".to_string()),
            ]),
        ],
    };
    conformance::run(&setup).unwrap();
}

#[test]
fn pull_before_configure_fails_and_says_so() {
    let work = scratch("unconfigured");
    let c = Connector::new(BIN.into(), work.join("state"));
    let msg = c.pull(&work.join("out")).unwrap_err().to_string();
    assert!(msg.contains("not configured"), "{msg}");
}

#[test]
fn a_head_whose_blob_has_not_arrived_is_retryable_not_empty() {
    let (work, remote, c) = configured("half-synced");
    let v = version(push_bytes(&c, &work, b"first", None));
    std::fs::remove_file(remote.join("snapshots").join(&v)).unwrap();
    let msg = c.pull(&work.join("out")).unwrap_err().to_string();
    assert!(msg.contains("not fully synced yet"), "{msg}");
}

#[test]
fn a_blob_whose_bytes_do_not_match_its_name_is_retryable() {
    let (work, remote, c) = configured("torn");
    let v = version(push_bytes(&c, &work, b"first", None));
    std::fs::write(remote.join("snapshots").join(&v), b"fir").unwrap();
    let msg = c.pull(&work.join("out")).unwrap_err().to_string();
    assert!(msg.contains("not fully synced yet"), "{msg}");
}

#[test]
fn only_the_last_five_snapshots_are_kept() {
    let (work, remote, c) = configured("prune");
    let mut expected: Option<String> = None;
    let mut versions = Vec::new();
    for i in 0..8 {
        let v = version(push_bytes(
            &c,
            &work,
            format!("blob {i}").as_bytes(),
            expected.as_deref(),
        ));
        versions.push(v.clone());
        expected = Some(v);
    }
    let mut kept: Vec<String> = std::fs::read_dir(remote.join("snapshots"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    kept.sort();
    let mut want = versions[3..].to_vec();
    want.sort();
    assert_eq!(kept, want, "the newest five, HEAD among them");
}

#[test]
fn a_lock_left_by_a_crashed_push_is_broken_once_stale() {
    let (work, remote, c) = configured("lock");
    // A lock left by a crashed push, backdated past the stale limit.
    let lock = remote.join("lock");
    std::fs::write(&lock, b"").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let v = version(push_bytes(&c, &work, b"after a crash", None));
    assert!(!lock.exists(), "the push released the lock it took");
    assert_eq!(
        std::fs::read_to_string(remote.join("HEAD")).unwrap().trim(),
        v
    );
}

/// Run the binary directly, without the runner, so what is checked is the
/// connector's own care for its state dir and not the runner's.
#[cfg(unix)]
fn configure_directly(state: &Path, remote: &Path) {
    use std::io::Write;
    let mut child = std::process::Command::new(BIN)
        .env("TASQX_REMOTE_STATE_DIR", state)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({
        "protocol": 1, "verb": "configure", "values": {"path": remote}
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn configure_keeps_its_state_private() {
    let work = scratch("private-fresh");
    let state = work.join("state");
    configure_directly(&state, &work.join("remote"));
    assert_eq!(mode_of(&state), 0o700, "the state dir");
    assert_eq!(mode_of(&state.join("config.json")), 0o600, "config.json");
}

#[cfg(unix)]
#[test]
fn configure_tightens_an_open_state_dir() {
    use std::os::unix::fs::PermissionsExt;
    let work = scratch("private-open");
    let state = work.join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    configure_directly(&state, &work.join("remote"));
    assert_eq!(mode_of(&state), 0o700);
}

// Conflict copies of HEAD (D200). Two machines that push while apart leave the
// sync client holding two HEADs; it keeps one and renames the other.

fn sha(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// Put a blob in the remote as another machine's push would have, and a file
/// `name` in the remote's root naming it. Returns the blob's hash.
fn plant(remote: &Path, name: &str, bytes: &[u8]) -> String {
    let hash = sha(bytes);
    std::fs::write(remote.join("snapshots").join(&hash), bytes).unwrap();
    std::fs::write(remote.join(name), format!("{hash}\n")).unwrap();
    hash
}

fn pulled(c: &Connector, work: &Path) -> Vec<String> {
    let out = work.join("out");
    let _ = std::fs::remove_dir_all(&out);
    c.pull(&out)
        .unwrap()
        .into_iter()
        .map(|s| {
            assert_eq!(sha(&std::fs::read(&s.path).unwrap()), s.version);
            s.version
        })
        .collect()
}

/// Each name is a conflict copy some sync client makes of HEAD: pull hands it
/// back after HEAD.
fn assert_pulled_beside_head(label: &str, names: &[&str]) {
    for (i, name) in names.iter().enumerate() {
        let (work, remote, c) = configured(&format!("{label}-{i}"));
        let head = version(push_bytes(&c, &work, b"this machine", None));
        let other = plant(&remote, name, format!("the other machine {i}").as_bytes());
        assert_eq!(pulled(&c, &work), vec![head, other], "{name:?}");
    }
}

#[test]
fn a_dropbox_conflicted_copy_of_head_is_pulled_beside_it() {
    assert_pulled_beside_head(
        "dropbox",
        &[
            "HEAD (Dimitri's conflicted copy 2026-09-27)",
            "HEAD (conflicted copy 2026-09-27)",
            "HEAD (Dimitri's conflicted copy 2026-09-27 (1))",
            "HEAD (MacBook-Pro's conflicted copy 2026-09-27 2)",
        ],
    );
}

#[test]
fn a_syncthing_sync_conflict_copy_of_head_is_pulled_beside_it() {
    assert_pulled_beside_head(
        "syncthing",
        &[
            "HEAD.sync-conflict-20260927-101530-ABCDEF7",
            // A device Syncthing cannot name leaves the modifier empty.
            "HEAD.sync-conflict-20260927-101530-",
        ],
    );
}

#[test]
fn an_icloud_numbered_copy_of_head_is_pulled_beside_it() {
    assert_pulled_beside_head("icloud", &["HEAD 2", "HEAD 3", "HEAD 12"]);
}

#[test]
fn every_conflict_copy_is_pulled_once_per_hash_head_first() {
    let (work, remote, c) = configured("many");
    let head = version(push_bytes(&c, &work, b"this machine", None));
    let b = plant(&remote, "HEAD 2", b"machine b");
    // The same content twice, and a copy that agrees with HEAD.
    std::fs::write(
        remote.join("HEAD.sync-conflict-20260927-101530-ABCDEF7"),
        format!("{b}\n"),
    )
    .unwrap();
    std::fs::write(remote.join("HEAD 3"), format!("{head}\n")).unwrap();
    assert_eq!(pulled(&c, &work), vec![head, b]);
}

#[test]
fn files_that_only_look_like_conflict_copies_are_ignored() {
    let (work, remote, c) = configured("look-alikes");
    let head = version(push_bytes(&c, &work, b"this machine", None));
    for (i, name) in [
        "HEADER",
        "HEAD.bak",
        "notHEAD 2",
        "HEAD 1",
        "HEAD 02",
        "HEAD 2x",
        "HEAD 2.txt",
        "head 2",
        "HEAD (copy)",
        "HEAD (conflicted copy",
        "HEAD (Dimitri's conflicted copy 2026-09-27).txt",
        "HEAD.sync-conflict-2026-bad",
        "HEAD.sync-conflict-2026-101530-ABCDEF7",
        "HEAD.sync-conflict-20260927-1015-ABCDEF7",
        "HEAD.sync-conflict-20260927-101530-ABC.txt",
        "history 2",
        "lock (conflicted copy 2026-09-27)",
    ]
    .iter()
    .enumerate()
    {
        plant(&remote, name, format!("look-alike {i}").as_bytes());
    }
    assert_eq!(pulled(&c, &work), vec![head]);
}

#[test]
fn a_conflict_copy_whose_blob_has_not_arrived_is_skipped_until_it_does() {
    let (work, remote, c) = configured("copy-half-synced");
    let head = version(push_bytes(&c, &work, b"this machine", None));
    let other = plant(&remote, "HEAD 2", b"machine b");
    let blob = remote.join("snapshots").join(&other);
    std::fs::remove_file(&blob).unwrap();
    assert_eq!(pulled(&c, &work), vec![head.clone()], "skipped, not fatal");
    std::fs::write(&blob, b"machine").unwrap();
    assert_eq!(pulled(&c, &work), vec![head.clone()], "torn is skipped too");
    std::fs::write(&blob, b"machine b").unwrap();
    assert_eq!(pulled(&c, &work), vec![head, other], "picked up once there");
}

#[test]
fn push_removes_the_conflict_copies_it_merged_and_keeps_a_later_one() {
    let (work, remote, c) = configured("supersede");
    let head = version(push_bytes(&c, &work, b"this machine", None));
    plant(&remote, "HEAD 2", b"machine b");
    plant(
        &remote,
        "HEAD (Dimitri's conflicted copy 2026-09-27)",
        b"machine c",
    );
    assert_eq!(pulled(&c, &work).len(), 3);
    // Arrives after the pull: this push has not merged it.
    let late = plant(
        &remote,
        "HEAD.sync-conflict-20260927-101530-ABCDEF7",
        b"machine d",
    );
    let merged = version(push_bytes(&c, &work, b"merged b and c", Some(&head)));
    assert!(!remote.join("HEAD 2").exists());
    assert!(!remote
        .join("HEAD (Dimitri's conflicted copy 2026-09-27)")
        .exists());
    assert!(remote
        .join("HEAD.sync-conflict-20260927-101530-ABCDEF7")
        .exists());
    assert_eq!(pulled(&c, &work), vec![merged, late]);
}

#[test]
fn a_push_that_conflicts_removes_no_conflict_copy() {
    let (work, remote, c) = configured("supersede-conflict");
    let head = version(push_bytes(&c, &work, b"this machine", None));
    plant(&remote, "HEAD 2", b"machine b");
    assert_eq!(pulled(&c, &work).len(), 2);
    // Another machine on the same folder, whose own state dir has pulled
    // nothing, moves HEAD on first — and supersedes nothing either.
    let other = Connector::new(BIN.into(), work.join("other-state"));
    assert!(matches!(
        other.configure(&values(&remote)).unwrap(),
        Configured::Ok
    ));
    version(push_bytes(&other, &work, b"someone else", Some(&head)));
    assert!(remote.join("HEAD 2").exists());
    assert!(matches!(
        push_bytes(&c, &work, b"merged b", Some(&head)),
        PushOutcome::Conflict
    ));
    assert!(remote.join("HEAD 2").exists());
}

#[test]
fn pruning_keeps_every_blob_a_conflict_copy_names() {
    let (work, remote, c) = configured("prune-copies");
    let mut expected = Some(version(push_bytes(&c, &work, b"blob 0", None)));
    let other = plant(&remote, "HEAD 2", b"machine b");
    // No pull in between, so nothing is superseded and HEAD 2 must survive
    // pushes enough to push its blob out of `history` many times over.
    for i in 1..8 {
        expected = Some(version(push_bytes(
            &c,
            &work,
            format!("blob {i}").as_bytes(),
            expected.as_deref(),
        )));
    }
    assert!(remote.join("HEAD 2").exists());
    assert!(remote.join("snapshots").join(&other).exists());
    assert_eq!(pulled(&c, &work), vec![expected.unwrap(), other]);
}

#[test]
fn a_push_supersedes_only_what_the_pull_just_before_it_returned() {
    let (work, remote, c) = configured("supersede-once");
    let head = version(push_bytes(&c, &work, b"this machine", None));
    let b = plant(&remote, "HEAD 2", b"machine b");
    assert_eq!(pulled(&c, &work).len(), 2);
    let merged = version(push_bytes(&c, &work, b"merged b", Some(&head)));
    assert!(!remote.join("HEAD 2").exists());
    // The same content turns up again, and this machine pushes on without
    // pulling: that push merged nothing, so it removes nothing.
    std::fs::write(remote.join("HEAD 2"), format!("{b}\n")).unwrap();
    version(push_bytes(&c, &work, b"more work", Some(&merged)));
    assert!(remote.join("HEAD 2").exists());
}

#[test]
fn an_unreadable_conflict_copy_stops_pruning_altogether() {
    let (work, remote, c) = configured("prune-garbage");
    let mut expected: Option<String> = None;
    let mut versions = Vec::new();
    std::fs::create_dir_all(remote.join("snapshots")).unwrap();
    std::fs::write(remote.join("HEAD 2"), b"not a hash\n").unwrap();
    for i in 0..8 {
        let v = version(push_bytes(
            &c,
            &work,
            format!("blob {i}").as_bytes(),
            expected.as_deref(),
        ));
        versions.push(v.clone());
        expected = Some(v);
    }
    for v in &versions {
        assert!(remote.join("snapshots").join(v).exists(), "{v} pruned");
    }
}
