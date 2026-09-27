//! The conformance suite every connector is held to (D198).
//!
//! Behind the `conformance` feature: a connector crate enables it from its
//! `[dev-dependencies]` and calls [`run`] on its own built binary, as
//! `crates/tasqx-remote-dir/tests/conformance.rs` does. Everything goes through
//! [`Connector`], the runner tasqx itself uses, so a connector that passes is
//! one tasqx can drive — the runner's own checks (protocol number, exit codes,
//! paths confined to `out_dir`) are part of what is being tested.
//!
//! The suite needs a remote it may write to that starts EMPTY, and one set of
//! values that configures it; it stops at the first check that fails, since
//! every later step builds on the remote state the earlier ones left.

use std::path::{Path, PathBuf};

use super::{Configured, Connector, PushOutcome, Values};

/// What the suite needs to know about the connector under test.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The connector executable.
    pub program: PathBuf,
    /// A scratch directory the suite may fill (state dir, blobs, pulls).
    pub work_dir: PathBuf,
    /// Values that configure an EMPTY remote.
    pub good: Values,
    /// Value sets `configure` must refuse with `ok: false`. At least one.
    pub bad: Vec<Values>,
}

/// Run every check against `setup`, in order. `Err` names the first one that
/// failed and why.
pub fn run(setup: &Setup) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(&setup.work_dir);
    mkdir(&setup.work_dir)?;
    let c = Connector::new(setup.program.clone(), setup.work_dir.join("state"));
    fn err(step: &'static str) -> impl Fn(super::Error) -> String {
        move |e| format!("{step}: {e}")
    }

    // describe is well-formed (the runner checks keys and labels) and names
    // every key the good values use.
    let d = c.describe().map_err(err("describe"))?;
    check(
        !d.version.trim().is_empty(),
        "describe: the version is empty",
    )?;
    for key in setup.good.keys() {
        check(
            d.fields.iter().any(|f| &f.key == key),
            &format!("describe: no field declares key {key:?}"),
        )?;
    }

    // configure refuses bad values in words, and accepts the good ones.
    check(
        !setup.bad.is_empty(),
        "setup: give at least one bad value set",
    )?;
    for bad in &setup.bad {
        match c.configure(bad).map_err(err("configure (bad values)"))? {
            Configured::Rejected(_) => {}
            Configured::Ok => return Err(format!("configure accepted bad values {bad:?}")),
        }
    }
    match c.configure(&setup.good).map_err(err("configure"))? {
        Configured::Ok => {}
        Configured::Rejected(e) => return Err(format!("configure refused the good values: {e}")),
    }

    // An empty remote pulls nothing.
    let pulled = pull(&c, setup, "pull-empty")?;
    check(
        pulled.is_empty(),
        &format!("pull on an empty remote returned {pulled:?}"),
    )?;

    // push(null) creates, and pull gives back the same bytes and version.
    let a: &[u8] = b"snapshot A\0\xff\xfe binary-safe";
    let va = pushed(
        push(&c, setup, "a", a, None)?,
        "push(null) on an empty remote",
    )?;
    expect_remote(&c, setup, "pull-a", &va, a)?;

    // push onto the current version succeeds with a new version.
    let b: &[u8] = b"snapshot B";
    let vb = pushed(
        push(&c, setup, "b", b, Some(&va))?,
        "push onto the current version",
    )?;
    check(vb != va, "two different snapshots got the same version")?;
    expect_remote(&c, setup, "pull-b", &vb, b)?;

    // A stale expected version conflicts and changes nothing.
    let stale = push(&c, setup, "stale", b"snapshot stale", Some(&va))?;
    check(
        stale == PushOutcome::Conflict,
        &format!("push with a stale expected_version returned {stale:?}"),
    )?;
    expect_remote(&c, setup, "pull-after-stale", &vb, b)?;

    // push(null) onto a non-empty remote conflicts and changes nothing.
    let null = push(&c, setup, "null", b"snapshot null", None)?;
    check(
        null == PushOutcome::Conflict,
        &format!("push(null) on a non-empty remote returned {null:?}"),
    )?;
    expect_remote(&c, setup, "pull-after-null", &vb, b)?;

    // Two pushes racing from the same version: exactly one wins, and the
    // remote holds the winner.
    let (d1, d2): (&[u8], &[u8]) = (b"racer one", b"racer two");
    let (r1, r2) = std::thread::scope(|s| {
        let one = s.spawn(|| push(&c, setup, "race-1", d1, Some(&vb)));
        let two = s.spawn(|| push(&c, setup, "race-2", d2, Some(&vb)));
        (join(one), join(two))
    });
    match (r1?, r2?) {
        (PushOutcome::Pushed { version }, PushOutcome::Conflict) => {
            expect_remote(&c, setup, "pull-race", &version, d1)
        }
        (PushOutcome::Conflict, PushOutcome::Pushed { version }) => {
            expect_remote(&c, setup, "pull-race", &version, d2)
        }
        other => Err(format!(
            "two pushes from the same version: expected exactly one winner, got {other:?}"
        )),
    }
}

fn check(ok: bool, msg: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(msg.to_string())
    }
}

fn mkdir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
}

fn join<T>(h: std::thread::ScopedJoinHandle<'_, Result<T, String>>) -> Result<T, String> {
    h.join()
        .unwrap_or_else(|_| Err("a racing push panicked".to_string()))
}

fn pull(c: &Connector, setup: &Setup, label: &str) -> Result<Vec<super::Snapshot>, String> {
    c.pull(&setup.work_dir.join(label))
        .map_err(|e| format!("{label}: {e}"))
}

fn push(
    c: &Connector,
    setup: &Setup,
    label: &str,
    bytes: &[u8],
    expected: Option<&str>,
) -> Result<PushOutcome, String> {
    let blob = setup.work_dir.join(format!("{label}.blob"));
    std::fs::write(&blob, bytes).map_err(|e| format!("cannot write {}: {e}", blob.display()))?;
    c.push(&blob, expected)
        .map_err(|e| format!("push {label}: {e}"))
}

fn pushed(outcome: PushOutcome, what: &str) -> Result<String, String> {
    match outcome {
        PushOutcome::Pushed { version } => Ok(version),
        PushOutcome::Conflict => Err(format!("{what} conflicted")),
    }
}

/// The remote holds exactly one snapshot, at `version`, with `bytes`.
fn expect_remote(
    c: &Connector,
    setup: &Setup,
    label: &str,
    version: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let snaps = pull(c, setup, label)?;
    let [only] = snaps.as_slice() else {
        return Err(format!("{label}: expected one snapshot, got {snaps:?}"));
    };
    check(
        only.version == version,
        &format!("{label}: version {:?}, expected {version:?}", only.version),
    )?;
    let got = std::fs::read(&only.path)
        .map_err(|e| format!("{label}: cannot read {}: {e}", only.path.display()))?;
    check(
        got == bytes,
        &format!("{label}: the pulled bytes differ from the pushed ones"),
    )
}
