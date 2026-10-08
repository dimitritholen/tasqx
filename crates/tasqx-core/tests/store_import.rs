//! `store.import` rules (D37, D21), at the engine, where they live.
//!
//! These were the only coverage of three import rules and ran them through a
//! subprocess (`tasqx-cli/tests/export_document.rs`); the CLI file keeps what is
//! the CLI's — that the `import` verb forwards the whole document.

use serde_json::{json, Value};
use tasqx_core::{dispatch, Engine, ErrorCode};

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn call(e: &Engine, method: &str, params: Value) -> Result<Value, tasqx_core::ApiError> {
    dispatch(e, method, &params)
}

fn ok(e: &Engine, method: &str, params: Value) -> Value {
    call(e, method, params).unwrap_or_else(|err| panic!("{method}: {err:?}"))
}

/// N3b: a payload that DEFINES its projects and then names one it did not define
/// is an incoherent document; minting the task anyway rebuilds the ghost bucket
/// D23 closed for `task.add` ("a typo lost the task silently").
#[test]
fn an_import_refuses_a_task_whose_project_the_document_does_not_define() {
    let e = engine();
    ok(&e, "project.create", json!({ "name": "work" }));

    let id = "019f6a0f-99df-7000-8000-0000000000aa";
    let err = call(
        &e,
        "store.import",
        json!({
            "projects": [{ "name": "work" }],
            "tasks": [{ "id": id, "short_id": 9001, "title": "ghost", "project": "wrok" }],
        }),
    )
    .expect_err("an undefined project must be refused");
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(
        err.message.contains("wrok"),
        "name the project: {}",
        err.message
    );
    assert!(
        err.message.contains(id),
        "and the task to edit: {}",
        err.message
    );

    // One transaction, so a refusal is total.
    let list = ok(&e, "task.list", json!({}));
    assert_eq!(list["count"], json!(0), "a refused import wrote: {list}");
}

/// The compatibility half: a document with no `projects` section (what an older
/// exporter wrote) still imports, and mints the project it names rather than
/// leaving the store in the ghost state.
#[test]
fn a_document_with_no_projects_section_still_imports_and_mints_what_it_names() {
    let e = engine();
    let legacy = json!([{
        "id": "019f6a0f-99df-7000-8000-0000000000bb",
        "short_id": 7,
        "title": "from an older tasqx",
        "project": "archief",
    }]);
    let r = ok(&e, "store.import", json!({ "tasks": legacy }));
    assert_eq!(
        r["projects_created"],
        json!(["archief"]),
        "minting must be reported: {r}"
    );

    let live = ok(&e, "project.list", json!({}));
    let names: Vec<&str> = live["projects"]
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(names, ["archief"], "an inferred project must be a real row");

    // The name the import accepted is now a name `task.add` accepts.
    ok(
        &e,
        "task.add",
        json!({ "title": "x", "project": "archief" }),
    );
}

/// D21's rule, nothing silently steals the default, applied to import: the only
/// write that can carry someone else's default in its payload.
#[test]
fn an_import_never_steals_a_default_the_destination_already_has() {
    let a = engine();
    ok(&a, "project.create", json!({ "name": "prive.klussen" }));
    ok(&a, "project.use", json!({ "name": "prive.klussen" }));
    let doc = ok(&a, "store.export", json!({}));
    assert_eq!(doc["default_project"], json!("prive.klussen"));

    // b already has its own default before the import arrives.
    let b = engine();
    ok(&b, "project.create", json!({ "name": "eigen" }));
    ok(&b, "project.use", json!({ "name": "eigen" }));
    let imp = ok(&b, "store.import", doc.clone());
    assert_eq!(
        imp["default_project"],
        json!("eigen"),
        "the result must state the default that stands: {imp}"
    );
    let caps = ok(&b, "core.capabilities", json!({}));
    assert_eq!(caps["default_project"], json!("eigen"));

    // A store with no default takes the document's: nothing to steal.
    let c = engine();
    let imp = ok(&c, "store.import", doc);
    assert_eq!(imp["default_project"], json!("prive.klussen"), "{imp}");
}
