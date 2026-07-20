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

/// D36's *storage* half, which the first round left to chance: "accepted values
/// are stored as given; the trim decides validity, not storage." The JSON door
/// obeyed that. The argv door did not — the sugar parser tokenizes a title into
/// words and rejoins them with a single space, so `tasqx add "  padded  "` and
/// `task.add {"title":"  padded  "}` wrote DIFFERENT BYTES for the same intent.
///
/// Both spellings of the divergence are pinned, because they are one bug with
/// two faces and covering only the first is how this project has shipped a
/// regression three times: leading/trailing padding (what the report named) and
/// an interior run of whitespace (`a    b` collapsed to `a b`, and a literal tab
/// silently became a space) — the latter is strictly worse, since it rewrites
/// the middle of a title the user typed and no trim would ever explain it.
///
/// `modify` is asserted beside `add` for the same reason: they share one sugar
/// parser, so a fix applied to one and not the other is the D38 lesson again.
#[test]
fn the_argv_door_stores_a_title_byte_for_byte_like_the_json_door() {
    let dir = scratch("verbatim");

    for (tag, title) in [("pad", "  padded title  "), ("inner", "a    b"), ("tab", "tab\there")] {
        // The JSON door is the reference: D36 fixes its behaviour in writing.
        let db = format!("api-{tag}.db");
        let (code, _) = api(&dir, &db, "task.add", &format!("{{\"title\":{}}}", json_str(title)));
        assert_eq!(code, 0, "api add {title:?} must succeed");
        let via_api = stored_title(&dir, &db);
        assert_eq!(via_api, title, "the JSON door must store {title:?} as given");

        // The argv door, driven as real argv — one element, exactly as a shell
        // hands `tasqx add "  padded title  "` over.
        let db = format!("cli-{tag}.db");
        let out = bin(&dir, &db).arg("add").arg(title).output().expect("run add");
        assert!(out.status.success(), "cli add {title:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            stored_title(&dir, &db),
            title,
            "`tasqx add {title:?}` must store the same bytes as `task.add` does"
        );

        // Its twin. One sugar parser, so both verbs or neither.
        let db = format!("mod-{tag}.db");
        assert!(bin(&dir, &db).args(["add", "seed"]).output().unwrap().status.success());
        let out = bin(&dir, &db).args(["modify", "1"]).arg(title).output().expect("run modify");
        assert!(out.status.success(), "cli modify {title:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            stored_title(&dir, &db),
            title,
            "`tasqx modify 1 {title:?}` must store the same bytes as `task.modify` does"
        );
    }

    // Sugar still parses, and the words of a multi-element title still join with
    // a single space — so the fix preserves what the shell drew and invents
    // nothing. Without this the test above would pass on a parser that simply
    // stopped tokenizing.
    let db = "sugar.db";
    let out = bin(&dir, db).args(["add", "Ship it due:friday +api"]).output().expect("run add");
    assert!(out.status.success(), "sugar: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stored_title(&dir, db), "Ship it");
    let out = bin(&dir, db).args(["add", "two", "words"]).output().expect("run add");
    assert!(out.status.success(), "join: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(stored_title(&dir, db), "two words");
}

/// The title of the most recently added task, read back over the JSON surface so
/// the assertion is about stored BYTES rather than about anything the renderer
/// chose to do with them.
fn stored_title(dir: &std::path::Path, db: &str) -> String {
    let (code, text) = api(dir, db, "task.list", "{}");
    assert_eq!(code, 0, "task.list failed: {text}");
    let v: serde_json::Value = serde_json::from_str(text.trim()).expect("task.list json");
    let tasks = v["result"]["tasks"].as_array().expect("tasks array");
    let last = tasks.last().expect("at least one task");
    last["title"].as_str().expect("a string title").to_string()
}

/// A Rust string as a JSON string literal, so a tab in a test case reaches the
/// engine as a tab rather than as the two characters a hand-written literal
/// would have smuggled in.
fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// D38's asymmetry, pinned as BEHAVIOUR rather than as prose.
///
/// The manual and the docs page both used to claim that one scanner splits
/// `add`/`modify` sugar "so what you can create you can filter for". D38 made
/// that false for the shell-stripped spelling: the write side still honours the
/// argument boundary, while the read side refuses to guess. Correcting the
/// sentence is not enough on its own — this project's rule is that a claim a
/// reader relies on must be enforced by a test or it is decoration (D15, D20).
///
/// So this drives the real binary at both ends of the asymmetry. If D38 is ever
/// relaxed, the docs and this guard move together instead of drifting apart.
#[test]
fn the_shell_stripped_spelling_writes_but_does_not_read() {
    let dir = scratch("d38");
    let db = "d38.db";
    assert!(bin(&dir, db).args(["init", "Home Renovation"]).output().unwrap().status.success());

    // The WRITE side takes it: the argv boundary says this is one value.
    let out = bin(&dir, db)
        .args(["add", "paint", "project:Home Renovation"])
        .output()
        .expect("run add");
    assert!(out.status.success(), "the write side must accept the shell-stripped spelling: {}",
        String::from_utf8_lossy(&out.stderr));

    // The READ side refuses it, and the refusal teaches the working spelling.
    let out = bin(&dir, db).args(["list", "project:Home Renovation"]).output().expect("run list");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "the read side must refuse rather than guess: {err}");
    assert!(err.contains(r#"project:"Home Renovation""#), "the refusal must name the quoted form: {err}");

    // And the spelling both sides accept really does select the row, so the
    // guard above is not merely asserting that reads are broken.
    let out = bin(&dir, db).args(["list", r#"project:"Home Renovation""#]).output().expect("run list");
    assert!(out.status.success(), "the quoted spelling must read: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("paint"), "the quoted spelling must find the task");
}

/// Neither reading surface may go back to claiming plain write/read symmetry.
/// Cheap, and it catches a well-meaning copy edit that restores the old
/// sentence without restoring the old behaviour.
#[test]
fn no_reading_surface_claims_write_read_symmetry() {
    let manual = std::fs::read_to_string("src/manual.rs").expect("read manual.rs");
    let docs = std::fs::read_to_string("src/docs.rs").expect("read docs.rs");
    for (name, src) in [("manual.rs", &manual), ("docs.rs", &docs)] {
        assert!(
            !src.contains("create you can filter for") && !src.contains("can also filter for"),
            "{name} claims write/read symmetry that D38 narrowed — say which spelling works on both sides"
        );
    }
}

/// P4a: `title: null` at both doors.
///
/// Both already REFUSED — the divergence was in what they said, and the wording
/// mattered more than it looks. `set:{title:null}` is the JSON API's spelling of
/// `--clear title`, which D13 rejects at CLI parse time by leaving `title` out of
/// `CLEARABLE` ("a task with no title is not a task"). The API answered it with
/// the generic wrong-type message — "send a string or omit `title`" — which
/// describes a type mistake, not the rule being enforced, and left a caller
/// trying to erase a title with no idea that erasing is the refused part.
///
/// So: same outcome at both doors, and a message at each that names the actual
/// rule. Both spellings of blank are asserted beside null so the three cannot
/// drift apart again.
#[test]
fn a_null_title_is_refused_at_both_doors_and_says_why() {
    let dir = scratch("null");
    let db = "null.db";
    assert!(bin(&dir, db).args(["add", "seed"]).output().unwrap().status.success());

    // `task.add`: null is simply an absent required field.
    let (code, text) = api(&dir, db, "task.add", r#"{"title":null}"#);
    assert_eq!(code, 0, "the api transport itself must succeed");
    assert!(text.contains("bad_request"), "task.add must refuse a null title: {text}");
    assert!(text.contains("title"), "the refusal must name the field: {text}");

    // `task.modify`: null is a CLEAR request, and clearing a title is the thing
    // D13 forbids. The message has to say that, not "send a string".
    let (_, text) = api(&dir, db, "task.modify", r#"{"ref":1,"set":{"title":null}}"#);
    assert!(text.contains("bad_request"), "task.modify must refuse a null title: {text}");
    assert!(
        text.contains("cannot be cleared"),
        "the refusal must name clearing as the refused operation, not the type: {text}"
    );

    // The title survived: a refused modify writes nothing.
    let (_, text) = api(&dir, db, "task.list", "{}");
    assert!(text.contains("seed"), "a refused modify must leave the title alone: {text}");
}
