//! `tasqx-remote-r2` against the conformance suite on a REAL R2 bucket.
//!
//! Runs only when all four of `TASQX_R2_TEST_ACCOUNT_ID`, `_BUCKET`,
//! `_ACCESS_KEY_ID` and `_SECRET_ACCESS_KEY` are set, and says it skipped
//! otherwise. The bucket is not touched outside one fresh object key per run,
//! `tasqx-conformance/<pid>-<nanos>/snapshot.tqx`, which the suite needs empty
//! and which is left behind afterwards: clear the prefix now and then. The
//! secret travels as `TASQX_R2_SECRET_ACCESS_KEY`, so the OS keyring is not
//! used.

use std::collections::BTreeMap;

use tasqx_core::remote::conformance::{self, Setup};

const BIN: &str = env!("CARGO_BIN_EXE_tasqx-remote-r2");

#[test]
fn the_r2_connector_passes_the_conformance_suite_against_a_real_bucket() {
    let var = |name: &str| std::env::var(format!("TASQX_R2_TEST_{name}")).ok();
    let (Some(account), Some(bucket), Some(key_id), Some(secret)) = (
        var("ACCOUNT_ID"),
        var("BUCKET"),
        var("ACCESS_KEY_ID"),
        var("SECRET_ACCESS_KEY"),
    ) else {
        eprintln!(
            "skipped: set TASQX_R2_TEST_ACCOUNT_ID, _BUCKET, _ACCESS_KEY_ID and \
             _SECRET_ACCESS_KEY to run the conformance suite against a real R2 bucket"
        );
        return;
    };
    std::env::remove_var("TASQX_R2_ENDPOINT");
    std::env::set_var("TASQX_R2_SECRET_ACCESS_KEY", &secret);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let object_key = format!(
        "tasqx-conformance/{}-{nanos}/snapshot.tqx",
        std::process::id()
    );
    let good = BTreeMap::from([
        ("account_id".to_string(), account),
        ("bucket".to_string(), bucket),
        ("access_key_id".to_string(), key_id),
        ("object_key".to_string(), object_key),
    ]);
    let mut denied = good.clone();
    denied.insert(
        "access_key_id".into(),
        "0000000000000000000000000000dead".into(),
    );
    let setup = Setup {
        program: BIN.into(),
        work_dir: std::env::temp_dir().join(format!("tasqx-remote-r2-live-{}", std::process::id())),
        good,
        bad: vec![BTreeMap::new(), denied],
    };
    conformance::run(&setup).unwrap();
}
