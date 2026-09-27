//! `tasqx-remote-r2` against a fake bucket (`fake_s3`), driven through the
//! runner tasqx itself uses. The secret comes from `TASQX_R2_SECRET_ACCESS_KEY`
//! throughout, so no test here touches the OS keyring; the keyring path is
//! covered by the unit tests in `src/secret.rs` over a fake store.

mod fake_s3;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use fake_s3::FakeS3;
use tasqx_core::remote::{Configured, Connector, PushOutcome};

const BIN: &str = env!("CARGO_BIN_EXE_tasqx-remote-r2");

fn scratch(label: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "tasqx-remote-r2-{label}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn good() -> BTreeMap<String, String> {
    values(&[
        ("account_id", fake_s3::ACCOUNT),
        ("bucket", fake_s3::BUCKET),
        ("access_key_id", fake_s3::ACCESS_KEY_ID),
    ])
}

fn with(mut v: BTreeMap<String, String>, key: &str, value: &str) -> BTreeMap<String, String> {
    v.insert(key.to_string(), value.to_string());
    v
}

fn connector(work: &Path, endpoint: &str) -> Connector {
    Connector::new(BIN.into(), work.join("state"))
        .with_env("TASQX_R2_ENDPOINT", endpoint)
        .with_env("TASQX_R2_SECRET_ACCESS_KEY", fake_s3::SECRET)
        .with_env("NO_PROXY", "127.0.0.1")
}

/// A fake bucket and a connector configured against it.
fn configured(label: &str) -> (PathBuf, FakeS3, Connector) {
    let work = scratch(label);
    let s3 = FakeS3::start();
    let c = connector(&work, &s3.endpoint());
    assert_eq!(c.configure(&good()).unwrap(), Configured::Ok);
    (work, s3, c)
}

fn push_bytes(c: &Connector, work: &Path, bytes: &[u8], expected: Option<&str>) -> PushOutcome {
    let blob = work.join("outgoing");
    std::fs::write(&blob, bytes).unwrap();
    c.push(&blob, expected).unwrap()
}

fn rejected(c: &Connector, v: &BTreeMap<String, String>) -> String {
    match c.configure(v).unwrap() {
        Configured::Rejected(msg) => msg,
        Configured::Ok => panic!("configure accepted {v:?}"),
    }
}

#[test]
fn describe_asks_for_an_api_token_scoped_to_the_bucket() {
    let work = scratch("describe");
    let d = Connector::new(BIN.into(), work.join("state"))
        .describe()
        .unwrap();
    assert_eq!(d.name, "r2");
    let keys: Vec<&str> = d.fields.iter().map(|f| f.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "account_id",
            "bucket",
            "access_key_id",
            "secret_access_key",
            "object_key"
        ]
    );
    let secret = &d.fields[3];
    assert!(secret.secret);
    assert!(d.fields.iter().filter(|f| f.secret).count() == 1);
    let token = d.fields.iter().find(|f| f.key == "access_key_id").unwrap();
    assert_eq!(
        token.help_url.as_deref(),
        Some("https://developers.cloudflare.com/r2/api/tokens/")
    );
    assert!(token.help.contains("Object Read & Write"), "{}", token.help);
}

#[test]
fn pull_on_an_empty_bucket_returns_nothing() {
    let (work, _s3, c) = configured("empty");
    assert!(c.pull(&work.join("out")).unwrap().is_empty());
}

#[test]
fn pull_returns_the_bytes_and_the_etag_unquoted() {
    let (work, s3, c) = configured("pull");
    s3.seed("tasqx/snapshot.tqx", "abc123", b"remote bytes");
    let snaps = c.pull(&work.join("out")).unwrap();
    let [only] = snaps.as_slice() else {
        panic!("{snaps:?}")
    };
    assert_eq!(only.version, "abc123");
    assert_eq!(std::fs::read(&only.path).unwrap(), b"remote bytes");
    let get = s3.seen().pop().unwrap();
    assert_eq!(get.method, "GET");
    assert_eq!(get.path, format!("/{}/tasqx/snapshot.tqx", fake_s3::BUCKET));
}

#[test]
fn the_object_key_setting_moves_the_snapshot() {
    let work = scratch("object-key");
    let s3 = FakeS3::start();
    let c = connector(&work, &s3.endpoint());
    let v = with(good(), "object_key", "team a/tasks.age");
    assert_eq!(c.configure(&v).unwrap(), Configured::Ok);
    push_bytes(&c, &work, b"x", None);
    let put = s3.seen().pop().unwrap();
    assert_eq!(put.path, format!("/{}/team%20a/tasks.age", fake_s3::BUCKET));
}

#[test]
fn the_first_push_sends_if_none_match_and_the_next_if_match() {
    let (work, s3, c) = configured("cas");
    let PushOutcome::Pushed { version: v1 } = push_bytes(&c, &work, b"one", None) else {
        panic!("the first push conflicted")
    };
    let first = s3.seen().pop().unwrap();
    assert_eq!(first.method, "PUT");
    assert_eq!(first.header("if-none-match"), Some("*"));
    assert_eq!(first.header("if-match"), None);
    assert_eq!(first.body, b"one");
    assert!(!v1.contains('"'), "the version is the bare etag: {v1}");

    let PushOutcome::Pushed { version: v2 } = push_bytes(&c, &work, b"two", Some(&v1)) else {
        panic!("the second push conflicted")
    };
    let second = s3.seen().pop().unwrap();
    assert_eq!(
        second.header("if-match"),
        Some(format!("\"{v1}\"").as_str())
    );
    assert_eq!(second.header("if-none-match"), None);
    assert_ne!(v1, v2);
}

#[test]
fn a_412_is_a_conflict() {
    let (work, s3, c) = configured("412");
    s3.seed("tasqx/snapshot.tqx", "theirs", b"theirs");
    assert_eq!(
        push_bytes(&c, &work, b"mine", Some("stale")),
        PushOutcome::Conflict
    );
    assert_eq!(
        push_bytes(&c, &work, b"mine", None),
        PushOutcome::Conflict,
        "push(null) onto an existing object"
    );
}

#[test]
fn a_409_conditional_request_conflict_is_a_conflict() {
    let (work, s3, c) = configured("409");
    s3.force(409, "ConditionalRequestConflict");
    assert_eq!(push_bytes(&c, &work, b"mine", None), PushOutcome::Conflict);
}

/// Exit code 2 on a conflict, seen without the runner in between.
#[test]
fn a_conflict_exits_2() {
    use std::io::Write;
    let (work, s3, _c) = configured("exit-2");
    s3.seed("tasqx/snapshot.tqx", "theirs", b"theirs");
    let blob = work.join("blob");
    std::fs::write(&blob, b"mine").unwrap();
    let mut child = std::process::Command::new(BIN)
        .env("TASQX_REMOTE_STATE_DIR", work.join("state"))
        .env("TASQX_R2_ENDPOINT", s3.endpoint())
        .env("TASQX_R2_SECRET_ACCESS_KEY", fake_s3::SECRET)
        .env("NO_PROXY", "127.0.0.1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({
        "protocol": 1, "verb": "push", "in_path": blob, "expected_version": null
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply, serde_json::json!({"protocol": 1, "conflict": true}));
}

#[test]
fn every_request_is_signed() {
    let (work, s3, c) = configured("signed");
    push_bytes(&c, &work, b"signed bytes", None);
    c.pull(&work.join("out")).unwrap();
    let seen = s3.seen();
    assert_eq!(seen.len(), 3, "configure's probe, the push, the pull");
    for req in seen {
        let auth = req.header("authorization").unwrap();
        assert!(
            auth.starts_with(&format!(
                "AWS4-HMAC-SHA256 Credential={}/",
                fake_s3::ACCESS_KEY_ID
            )),
            "{auth}"
        );
        assert!(
            auth.contains("/auto/s3/aws4_request, SignedHeaders="),
            "{auth}"
        );
    }
}

#[test]
fn configure_refuses_a_403_in_words() {
    let work = scratch("403");
    let s3 = FakeS3::start();
    let c = connector(&work, &s3.endpoint());
    let msg = rejected(&c, &with(good(), "access_key_id", fake_s3::DENIED_KEY_ID));
    assert!(msg.contains("refused"), "{msg}");
    assert!(msg.contains("Object Read & Write"), "{msg}");
    assert!(!work.join("state").join("config.json").exists());
}

#[test]
fn configure_refuses_a_bucket_that_does_not_exist() {
    let work = scratch("no-bucket");
    let s3 = FakeS3::start();
    let c = connector(&work, &s3.endpoint());
    let msg = rejected(&c, &with(good(), "bucket", fake_s3::MISSING_BUCKET));
    assert!(msg.contains("no bucket"), "{msg}");
}

#[test]
fn configure_refuses_an_endpoint_it_cannot_reach() {
    let work = scratch("unreachable");
    // A port nothing listens on: bind one, then let it go.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let c = connector(&work, &format!("http://127.0.0.1:{port}"));
    let msg = rejected(&c, &good());
    assert!(msg.contains("cannot reach"), "{msg}");
}

#[test]
fn configure_refuses_malformed_values_before_connecting() {
    let work = scratch("malformed");
    let s3 = FakeS3::start();
    let c = connector(&work, &s3.endpoint());
    for (v, want) in [
        (BTreeMap::new(), "account_id"),
        (with(good(), "account_id", "evil.example/x"), "account ID"),
        (with(good(), "bucket", "Not_A_Bucket"), "bucket name"),
        (with(good(), "access_key_id", ""), "access_key_id"),
        (with(good(), "region", "eu"), "unknown setting"),
        (with(good(), "object_key", "/leading"), "object key"),
    ] {
        let msg = rejected(&c, &v);
        assert!(msg.contains(want), "{v:?}: {msg}");
    }
    assert!(
        s3.seen().is_empty(),
        "nothing was sent for a malformed value"
    );
}

#[test]
fn a_secret_that_differs_from_the_environment_override_is_refused() {
    let work = scratch("secret-mismatch");
    let s3 = FakeS3::start();
    let c = connector(&work, &s3.endpoint());
    let msg = rejected(&c, &with(good(), "secret_access_key", "something else"));
    assert!(msg.contains("TASQX_R2_SECRET_ACCESS_KEY"), "{msg}");
}

#[test]
fn configure_keeps_the_secret_off_disk() {
    let work = scratch("off-disk");
    let s3 = FakeS3::start();
    let c = connector(&work, &s3.endpoint());
    let v = with(good(), "secret_access_key", fake_s3::SECRET);
    assert_eq!(c.configure(&v).unwrap(), Configured::Ok);
    for entry in std::fs::read_dir(work.join("state")).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(fake_s3::SECRET), "{text}");
    }
}

#[test]
fn pull_before_configure_fails_and_says_so() {
    let work = scratch("unconfigured");
    let c = connector(&work, "http://127.0.0.1:9");
    let msg = c.pull(&work.join("out")).unwrap_err().to_string();
    assert!(msg.contains("not configured"), "{msg}");
}

#[test]
fn a_403_on_pull_fails_with_the_reason() {
    let (work, s3, c) = configured("pull-403");
    s3.force(403, "AccessDenied");
    let msg = c.pull(&work.join("out")).unwrap_err().to_string();
    assert!(msg.contains("AccessDenied"), "{msg}");
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn configure_keeps_its_state_private() {
    let (work, _s3, _c) = configured("private");
    assert_eq!(mode_of(&work.join("state").join("config.json")), 0o600);
}
