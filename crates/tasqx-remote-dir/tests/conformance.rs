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
