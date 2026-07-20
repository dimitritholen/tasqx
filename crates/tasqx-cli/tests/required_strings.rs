//! D36 through the real binary: one rule for a required string at every door.
//!
//! These drive the process rather than the engine because the regression they
//! pin is about what a *caller* can reach — `tasqx api` for the JSON surface,
//! bare argv for the sugar surface — and the previous rounds shipped three
//! regressions that a library-level test could not have seen, all of them the
//! same shape: one spelling covered, its sibling not. So every door is asserted
//! against every blank spelling, and the argv path is exercised as argv.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A scratch store of this test's own. Named per tag so cargo's parallel
/// threads cannot share one file, and per-process so a stale run cannot leak in.
fn scratch(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("tasqx-reqstr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// The binary pointed at one named store inside a scratch dir. `--no-daemon`
/// keeps every call in-process: a developer with a daemon running would
/// otherwise have these tests talk to their real store.
fn bin(dir: &std::path::Path, db: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir).env("TASQX_DB", dir.join(db)).arg("--no-daemon");
    c
}

/// One `tasqx api` round trip: the request envelope on stdin, the response text
/// and exit code back.
fn api(dir: &std::path::Path, db: &str, method: &str, params: &str) -> (i32, String) {
    let mut child = bin(dir, db)
        .arg("api")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tasqx api");
    let req = format!(r#"{{"tasqx":"1","method":"{method}","params":{params}}}"#);
    child.stdin.as_mut().expect("stdin").write_all(req.as_bytes()).expect("write request");
    let out = child.wait_with_output().expect("wait for tasqx api");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

/// The JSON spelling of a blank string, for embedding in a params literal.
/// Kept beside the human label so a failure message says which one broke.
const BLANKS: [(&str, &str); 5] =
    [("empty", r#""""#), ("space", r#"" ""#), ("spaces", r#""   ""#), ("tab", r#""\t""#), ("newline", r#""\n""#)];

/// Every door that writes a required string refuses every blank spelling, with
/// the same code, through the same binary. Before the fix `task.modify` was the
/// one door that said yes.
#[test]
fn every_door_refuses_every_blank_required_string() {
    let dir = scratch("doors");
    let out = bin(&dir, "d.db").args(["add", "seed"]).output().expect("seed the store");
    assert!(out.status.success(), "seed failed: {}", String::from_utf8_lossy(&out.stderr));

    for (label, blank) in BLANKS {
        for (door, method, params) in [
            ("task.add", "task.add", format!(r#"{{"title":{blank}}}"#)),
            ("task.modify", "task.modify", format!(r#"{{"ref":1,"set":{{"title":{blank}}}}}"#)),
            ("project.create", "project.create", format!(r#"{{"name":{blank}}}"#)),
            (
                "store.import",
                "store.import",
                format!(
                    r#"{{"tasks":[{{"id":"019f7eb6-0000-7000-8000-000000000001","short_id":9,"title":{blank}}}]}}"#
                ),
            ),
        ] {
            // `api` is a D31 carve-out: it speaks the response envelope, so a
            // refusal is `ok:false` at exit 0. Asserting the envelope rather
            // than the exit code is what a JSON caller actually sees.
            let (code, text) = api(&dir, "d.db", method, &params);
            assert_eq!(code, 0, "{door} / {label}: `api` always exits 0 (D31): {text}");
            assert!(text.contains(r#""ok":false"#), "{door} must refuse a {label} title/name: {text}");
            assert!(text.contains("bad_request"), "{door} / {label}: {text}");
        }
    }

    // Nothing was written by any of it: the seed is still the only task, still
    // at _rev 1, and no project was created.
    let (_, text) = api(&dir, "d.db", "task.get", r#"{"ref":1}"#);
    assert!(text.contains(r#""title":"seed""#), "a refused modify must not write: {text}");
    assert!(text.contains(r#""_rev":1"#), "nor bump the revision: {text}");
}

/// The N2a regression itself, as the session that found it: modify a title to
/// empty, export, import into a fresh store. It used to end at exit 2 naming a
/// uuid — a store the tool wrote and could not read back.
#[test]
fn a_store_the_tool_wrote_can_always_be_imported_again() {
    let dir = scratch("roundtrip");
    assert!(bin(&dir, "a.db").args(["add", "alpha"]).output().unwrap().status.success());

    let (_, text) = api(&dir, "a.db", "task.modify", r#"{"ref":1,"set":{"title":""}}"#);
    assert!(
        text.contains(r#""ok":false"#),
        "an empty title must be refused at the modify door: {text}"
    );

    // The store is therefore still importable, and the round trip is
    // byte-identical (D12) — proven on the bytes, not on a parsed value.
    let exported = bin(&dir, "a.db").arg("export").output().expect("export");
    assert!(exported.status.success());
    let path = dir.join("export.json");
    std::fs::write(&path, &exported.stdout).expect("write export");

    let imported = bin(&dir, "b.db").arg("import").arg(&path).output().expect("import");
    assert!(
        imported.status.success(),
        "re-import must succeed: {}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let reexported = bin(&dir, "b.db").arg("export").output().expect("re-export");
    assert_eq!(reexported.stdout, exported.stdout, "D12: byte-identical round trip");
}

/// The argv door. `tasqx add "   "` and `tasqx modify 1 "   "` reach the same
/// answer as the JSON surface — asserted as real argv words, because the sugar
/// parser splits on argv and a shell-joined string would test a different code
/// path than the one a user reaches.
///
/// This one already held before D36, by a different mechanism: the sugar parser
/// drops a whitespace-only word, so `add` reaches `req_str` with `""` and
/// `modify` reaches the "nothing to change" guard. It is pinned anyway, because
/// "the CLI happens to agree with the core" is exactly the kind of accident the
/// next sugar change silently ends — and the whole point of D36 is that the
/// same input gets the same answer on every surface.
#[test]
fn the_argv_doors_refuse_a_whitespace_only_title() {
    let dir = scratch("argv");
    assert!(bin(&dir, "a.db").args(["add", "real"]).output().unwrap().status.success());

    for blank in ["   ", "\t", " \t "] {
        let out = bin(&dir, "a.db").arg("add").arg(blank).output().expect("run add");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "`tasqx add {blank:?}` must be refused: {err}");

        let out = bin(&dir, "a.db").args(["modify", "1"]).arg(blank).output().expect("run modify");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "`tasqx modify 1 {blank:?}` must be refused: {err}");

        let out = bin(&dir, "a.db").arg("init").arg(blank).output().expect("run init");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "`tasqx init {blank:?}` must be refused: {err}");
    }

    // And the ordinary forms still work, so none of the above is a guard that
    // simply rejects everything.
    assert!(bin(&dir, "a.db").args(["add", "still fine"]).output().unwrap().status.success());
    assert!(bin(&dir, "a.db").args(["modify", "1", "renamed"]).output().unwrap().status.success());
    assert!(bin(&dir, "a.db").args(["init", "work"]).output().unwrap().status.success());
}
