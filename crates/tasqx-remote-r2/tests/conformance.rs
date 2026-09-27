//! `tasqx-remote-r2` against the connector conformance suite (D198), over the
//! fake bucket in `fake_s3`. The suite builds its own runner, which passes no
//! extra environment, so the endpoint and secret overrides go into this test
//! binary's own environment; this file holds one test so nothing races it.

mod fake_s3;

use std::collections::BTreeMap;

use fake_s3::FakeS3;
use tasqx_core::remote::conformance::{self, Setup};

const BIN: &str = env!("CARGO_BIN_EXE_tasqx-remote-r2");

fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn the_r2_connector_passes_the_conformance_suite_against_a_fake_bucket() {
    let s3 = FakeS3::start();
    std::env::set_var("TASQX_R2_ENDPOINT", s3.endpoint());
    std::env::set_var("TASQX_R2_SECRET_ACCESS_KEY", fake_s3::SECRET);
    std::env::set_var("NO_PROXY", "127.0.0.1");
    let good = values(&[
        ("account_id", fake_s3::ACCOUNT),
        ("bucket", fake_s3::BUCKET),
        ("access_key_id", fake_s3::ACCESS_KEY_ID),
    ]);
    let with = |key: &str, value: &str| {
        let mut v = good.clone();
        v.insert(key.to_string(), value.to_string());
        v
    };
    let setup = Setup {
        program: BIN.into(),
        work_dir: std::env::temp_dir().join(format!(
            "tasqx-remote-r2-conformance-{}",
            std::process::id()
        )),
        good: good.clone(),
        bad: vec![
            BTreeMap::new(),
            with("access_key_id", fake_s3::DENIED_KEY_ID),
            with("bucket", fake_s3::MISSING_BUCKET),
            with("account_id", "not-an-account"),
        ],
    };
    conformance::run(&setup).unwrap();
}
