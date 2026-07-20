//! D37: an export is a self-contained DOCUMENT, and a project is part of it.
//!
//! D12 says an export never names something it does not carry, and D21/D22/D23
//! made a project a first-class record with real invariants (archived is out of
//! rotation; the default names a live project). Between those two decisions sat
//! the hole this file guards: `store.export` emitted only `tasks`, so every
//! project row — its description, its archived flag, and the store's default —
//! was lost on restore, and `store.import` accepted a task naming a project no
//! project surface had ever heard of, the exact ghost bucket D23 closed for
//! `task.add` and `task.modify`.
//!
//! These run the REAL binary, in both spellings that exist for this contract —
//! the `export`/`import` verbs and the `store.export`/`store.import` methods
//! over `tasqx api` — because the CLI carried its own copy of the document
//! shape (it printed only the `tasks` array and forwarded only `tasks` back),
//! so a core-only guard would have passed over a CLI that still lost everything.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// One isolated config dir + store per (test, store name).
fn store(tag: &str, name: &str) -> (PathBuf, PathBuf) {
    let mut dir = std::env::temp_dir();
    dir.push(format!("tasqx-doc-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create config dir");
    let db = dir.join(format!("{name}.db"));
    let _ = std::fs::remove_file(&db);
    (dir, db)
}

fn bin(dir: &Path, db: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir).env("TASQX_DB", db);
    c
}

/// Run and return (exit code, stdout, stderr).
fn run(dir: &Path, db: &Path, args: &[&str]) -> (i32, String, String) {
    let out = bin(dir, db).args(args).output().unwrap_or_else(|e| panic!("run {args:?}: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ok(dir: &Path, db: &Path, args: &[&str]) -> String {
    let (code, so, se) = run(dir, db, args);
    assert_eq!(code, 0, "`tasqx {}` must succeed: {se}", args.join(" "));
    so
}

/// One JSON API call over the real binary's stdio transport. Returns the whole
/// envelope, because a refusal rides `ok:false` at exit 0 there (D31).
fn api(dir: &Path, db: &Path, method: &str, params: Value) -> Value {
    use std::io::Write;
    let req = json!({ "tasqx": "1", "id": "t", "method": method, "params": params }).to_string();
    let mut child = bin(dir, db)
        .arg("api")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn api");
    child.stdin.as_mut().expect("stdin").write_all(req.as_bytes()).expect("write req");
    let out = child.wait_with_output().expect("api output");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("api {method} did not answer JSON ({e}): {}", String::from_utf8_lossy(&out.stdout))
    })
}

/// A store with every project state that has an invariant attached: a live
/// project with a description, a live project holding the default, and an
/// archived one that still owns a task.
fn seed(dir: &Path, db: &Path) {
    ok(dir, db, &["init", "work", "--desc", "day job"]);
    ok(dir, db, &["init", "prive.klussen"]);
    ok(dir, db, &["init", "legacy"]);
    ok(dir, db, &["add", "task in work", "project:work"]);
    ok(dir, db, &["add", "old thing", "project:legacy"]);
    // Archived AFTER its task exists: an export must be able to restore a done
    // project that still owns history, which is why import checks a project's
    // existence and not its archived flag.
    let r = api(dir, db, "project.archive", json!({ "name": "legacy" }));
    assert_eq!(r["ok"], json!(true), "archive: {r}");
    let r = api(dir, db, "project.use", json!({ "name": "prive.klussen" }));
    assert_eq!(r["ok"], json!(true), "use: {r}");
}

/// Everything about projects that a reader can see, as one comparable value.
fn project_state(dir: &Path, db: &Path) -> Value {
    let live: Value = serde_json::from_str(&ok(dir, db, &["projects", "--json"])).expect("projects");
    let all = api(dir, db, "project.list", json!({ "include_archived": true }));
    let caps = api(dir, db, "core.capabilities", json!({}));
    json!({
        "live": live,
        "all": all["result"],
        "default": caps["result"]["default_project"],
    })
}

/// N3a, through the `export` / `import` VERBS.
///
/// The old failure was total and silent: the destination listed zero projects,
/// reported no default, and still held tasks whose `project` named one of them
/// — a store in exactly the state D23 exists to prevent, reached by the one
/// command whose whole purpose is faithful restoration.
#[test]
fn the_export_verb_carries_projects_the_default_and_archived_state() {
    let (dir, a) = store("verbs", "a");
    let (_, b) = store("verbs", "b");
    seed(&dir, &a);

    let doc = ok(&dir, &a, &["export"]);
    let parsed: Value = serde_json::from_str(&doc).expect("export must be JSON");
    assert!(
        parsed.get("projects").is_some(),
        "the document the CLI writes must carry its projects: {doc}"
    );
    let path = dir.join("doc.json");
    std::fs::write(&path, doc.as_bytes()).expect("write doc");

    ok(&dir, &b, &["import", path.to_str().expect("utf8 path")]);

    assert_eq!(
        project_state(&dir, &b),
        project_state(&dir, &a),
        "every project surface must agree across the round trip, not just the task list"
    );
    // And the restored store is usable as the source was: the default is live,
    // so a bare `add` lands where it did, and the archived project stays out of
    // rotation rather than coming back as ordinary work.
    let added = ok(&dir, &b, &["add", "fresh capture"]);
    assert!(added.contains("prive.klussen"), "a bare add must inherit the restored default: {added}");
    let (code, _, se) = run(&dir, &b, &["use", "legacy"]);
    assert_eq!(code, 5, "the archived project must still be archived after import: {se}");
}

/// N3a again, through the `store.export` / `store.import` METHODS. The verbs and
/// the methods are two spellings of one contract and each has drifted from the
/// other before, so one example is not a guard.
#[test]
fn the_store_export_method_carries_projects_the_default_and_archived_state() {
    let (dir, a) = store("methods", "a");
    let (_, b) = store("methods", "b");
    seed(&dir, &a);

    let ex = api(&dir, &a, "store.export", json!({}));
    assert_eq!(ex["ok"], json!(true), "{ex}");
    let result = &ex["result"];
    assert!(result.get("projects").is_some(), "store.export must emit `projects`: {result}");
    assert_eq!(
        result["default_project"],
        json!("prive.klussen"),
        "the default is store state, so the document must carry it: {result}"
    );

    let imp = api(&dir, &b, "store.import", result.clone());
    assert_eq!(imp["ok"], json!(true), "{imp}");
    assert_eq!(project_state(&dir, &b), project_state(&dir, &a));

    // D12's byte-identical round trip, now over the WHOLE document rather than
    // its task half — the half that used to be all there was.
    let re = api(&dir, &b, "store.export", json!({}));
    assert_eq!(re["result"], *result, "export -> import -> export must be identity");
}

/// N3b: a payload that DEFINES its projects and then names one it did not define
/// is an incoherent document, and minting the task anyway rebuilds the ghost
/// bucket D23 closed for `task.add` ("a typo lost the task silently").
#[test]
fn an_import_refuses_a_task_whose_project_the_document_does_not_define() {
    let (dir, db) = store("undefined", "a");
    ok(&dir, &db, &["init", "work"]);

    let payload = json!({
        "projects": [{ "name": "work" }],
        "tasks": [{
            "id": "019f6a0f-99df-7000-8000-0000000000aa",
            "short_id": 9001,
            "title": "ghost",
            "project": "wrok",
        }],
    });

    let r = api(&dir, &db, "store.import", payload.clone());
    assert_eq!(r["ok"], json!(false), "an undefined project must be refused: {r}");
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("wrok"), "the error must name the offending project: {msg}");
    assert!(msg.contains("019f6a0f-99df-7000-8000-0000000000aa"), "and the task to edit: {msg}");

    // Nothing at all was written: one transaction, so a refusal is total.
    let list: Value = serde_json::from_str(&ok(&dir, &db, &["list", "--json"])).expect("list");
    assert_eq!(list["count"], json!(0), "a refused import must write nothing: {list}");

    // The same document through the `import` VERB, which used to forward only
    // the `tasks` array and would therefore have dropped the very section that
    // makes this refusal possible.
    let path = dir.join("bad.json");
    std::fs::write(&path, payload.to_string()).expect("write payload");
    let (code, _, se) = run(&dir, &db, &["import", path.to_str().expect("utf8 path")]);
    assert_eq!(code, 2, "the verb must refuse it too: {se}");
    assert!(se.contains("wrok"), "naming the project: {se}");
}

/// The compatibility half: a document written by a tasqx that had no `projects`
/// section at all. It must still import — and it must not leave the store in the
/// ghost state, so the project it names is minted rather than refused.
#[test]
fn a_document_with_no_projects_section_still_imports_and_mints_what_it_names() {
    let (dir, db) = store("legacy", "a");

    // The exact shape an older `tasqx export` wrote: a bare array of tasks.
    let legacy = json!([{
        "id": "019f6a0f-99df-7000-8000-0000000000bb",
        "short_id": 7,
        "title": "from an older tasqx",
        "project": "archief",
    }]);
    let path = dir.join("legacy.json");
    std::fs::write(&path, legacy.to_string()).expect("write legacy");
    let out = ok(&dir, &db, &["import", path.to_str().expect("utf8 path")]);
    assert!(out.contains("1 task"), "{out}");

    let live: Value = serde_json::from_str(&ok(&dir, &db, &["projects", "--json"])).expect("projects");
    let names: Vec<&str> =
        live["projects"].as_array().expect("array").iter().filter_map(|p| p["name"].as_str()).collect();
    assert_eq!(names, ["archief"], "an inferred project must become a real row: {live}");

    // The store is coherent afterwards: the same name the import accepted is now
    // a name `add` accepts, which is the whole point of minting it.
    let (code, _, se) = run(&dir, &db, &["add", "x", "--project", "archief"]);
    assert_eq!(code, 0, "the minted project must be usable: {se}");

    // And the same payload over the METHOD, in its object spelling.
    let (_, b) = store("legacy", "b");
    let r = api(&dir, &b, "store.import", json!({ "tasks": legacy }));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert_eq!(r["result"]["projects_created"], json!(["archief"]), "minting must be reported: {r}");
}

/// D21's rule — nothing silently steals the default — applied to import, which
/// is the only write that can carry someone else's default in its payload.
#[test]
fn an_import_never_steals_a_default_the_destination_already_has() {
    let (dir, a) = store("default", "a");
    let (_, b) = store("default", "b");
    seed(&dir, &a);

    // b already has its own default before the import arrives.
    ok(&dir, &b, &["init", "eigen"]);
    let ex = api(&dir, &a, "store.export", json!({}));
    let imp = api(&dir, &b, "store.import", ex["result"].clone());
    assert_eq!(imp["ok"], json!(true), "{imp}");
    assert_eq!(
        imp["result"]["default_project"],
        json!("eigen"),
        "the result must state the default that stands: {imp}"
    );

    let caps = api(&dir, &b, "core.capabilities", json!({}));
    assert_eq!(
        caps["result"]["default_project"],
        json!("eigen"),
        "an import must not redirect where a bare `add` lands"
    );
    // A store with no default takes the document's, since there is nothing to steal.
    let (_, c) = store("default", "c");
    let imp = api(&dir, &c, "store.import", ex["result"].clone());
    assert_eq!(imp["result"]["default_project"], json!("prive.klussen"), "{imp}");
}
