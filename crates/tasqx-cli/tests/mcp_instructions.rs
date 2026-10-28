//! `tasqx mcp instructions` (D253): the handshake's `instructions` string and
//! the packer's own counts, printed from the caller's working directory.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use tasqx_core::engine::Engine;
use tasqx_core::mcp::{handshake_instructions, McpServer, Scope, MCP_INSTRUCTIONS_CAP};

fn scratch(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("tasqx-mcpinstr-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

fn bin(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir)
        .env("TASQX_DB", dir.join("store.db"))
        .env_remove("TASQX_SOCK")
        .current_dir(dir)
        .arg("--no-daemon");
    c
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = bin(dir).args(args).output().expect("run tasqx");
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn seeded(tag: &str, n: usize, title: &str) -> PathBuf {
    let dir = scratch(tag);
    run(&dir, &["init", "demo"]);
    for i in 0..n {
        run(
            &dir,
            &[
                "memory",
                "add",
                "--standing",
                &format!("{title} {i}"),
                "a gist",
            ],
        );
    }
    dir
}

fn json_of(dir: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["--json", "mcp", "instructions"];
    args.extend_from_slice(extra);
    serde_json::from_str(&run(dir, &args)).expect("json")
}

/// What an in-process `initialize` sends for the same store, workdir, scope.
fn initialize(dir: &Path, scope: Scope) -> String {
    let engine = Engine::open(&dir.join("store.db").to_string_lossy()).unwrap();
    let server = McpServer::new(&engine, scope).with_workdir(Some(dir.to_path_buf()));
    let r = server
        .handle_message(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .unwrap();
    r["result"]["instructions"].as_str().unwrap().to_string()
}

#[test]
fn json_instructions_are_byte_identical_to_initialize_and_counts_are_the_packers() {
    let dir = seeded("same", 4, "rule");
    let got = json_of(&dir, &[]);
    assert_eq!(
        got["instructions"].as_str().unwrap(),
        initialize(&dir, Scope::Read)
    );
    let engine = Engine::open(&dir.join("store.db").to_string_lossy()).unwrap();
    let h = handshake_instructions(&engine, Some(&dir), Scope::Read, None).unwrap();
    assert_eq!(got["shown"], h.shown);
    assert_eq!(got["gist"], h.gist);
    assert_eq!(got["omitted"], h.omitted);
    assert_eq!(got["bytes"], h.bytes);
    assert_eq!(got["cap"], MCP_INSTRUCTIONS_CAP);
    assert_eq!(got["project"], "demo");
    assert!(got["inference"].is_string());
    assert!(h.shown > 0);
}

#[test]
fn human_output_is_the_instructions_then_one_counts_line() {
    let dir = seeded("human", 3, "rule");
    let text = run(&dir, &["mcp", "instructions"]);
    let j = json_of(&dir, &[]);
    let (body, counts) = text.trim_end_matches('\n').rsplit_once('\n').unwrap();
    assert_eq!(
        body,
        j["instructions"].as_str().unwrap().trim_end_matches('\n')
    );
    let want = format!(
        "{} rulings: {} shown, {} with gist, {} omitted · {} / {} bytes",
        j["shown"].as_u64().unwrap() + j["omitted"].as_u64().unwrap(),
        j["shown"],
        j["gist"],
        j["omitted"],
        j["bytes"],
        j["cap"]
    );
    assert_eq!(counts, want);
}

#[test]
fn overflow_shows_omitted_in_both_outputs() {
    let dir = seeded(
        "overflow",
        40,
        "a standing ruling with a fairly long title to fill",
    );
    let j = json_of(&dir, &[]);
    let omitted = j["omitted"].as_u64().unwrap();
    assert!(omitted > 0, "{j}");
    assert!(j["bytes"].as_u64().unwrap() <= MCP_INSTRUCTIONS_CAP as u64);
    let text = run(&dir, &["mcp", "instructions"]);
    assert!(text.contains(&format!("{omitted} omitted")), "{text}");
}

#[test]
fn scope_write_differs_from_the_read_default_the_way_initialize_does() {
    let dir = seeded("scope", 2, "rule");
    let read = json_of(&dir, &[]);
    let write = json_of(&dir, &["--scope", "write"]);
    assert_ne!(read["instructions"], write["instructions"]);
    assert_eq!(
        write["instructions"].as_str().unwrap(),
        initialize(&dir, Scope::Write)
    );
    assert_eq!(
        read["instructions"],
        json_of(&dir, &["--scope", "read"])["instructions"]
    );
}

#[test]
fn no_project_and_no_rulings_gives_null_project_and_zero_counts() {
    let dir = scratch("empty");
    let j = json_of(&dir, &[]);
    assert!(j["project"].is_null() && j["inference"].is_null(), "{j}");
    assert_eq!(j["shown"], 0);
    assert_eq!(j["omitted"], 0);
}

/// A failure goes through the standard path: under `--json` the normal error
/// envelope on stdout, and a non-zero exit.
#[test]
fn a_store_that_cannot_open_gives_the_json_error_envelope() {
    let dir = scratch("err");
    let out = bin(&dir)
        .env("TASQX_DB", "/dev/null/store.db")
        .args(["--json", "mcp", "instructions"])
        .output()
        .expect("run tasqx");
    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("stdout is the envelope");
    assert_eq!(v["ok"], false, "{v}");
    assert!(v["error"]["code"].is_string(), "{v}");
}

/// A diagnostic never writes: under `--scope write` the freshness line is
/// reported from a dry sweep, while a real write-scoped `initialize` still
/// refreshes the doc.
#[test]
fn write_scope_reports_the_refresh_line_without_refreshing() {
    let dir = scratch("dry");
    run(&dir, &["init", "demo"]);
    let docs = dir.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let file = docs.join("a.md");
    std::fs::write(&file, "# Rule A\n\nfirst body\n").unwrap();
    run(&dir, &["memory", "import", docs.to_str().unwrap()]);
    std::fs::write(&file, "# Rule A\n\nsecond body, longer\n").unwrap();

    let read_doc = || {
        let engine = Engine::open(&dir.join("store.db").to_string_lossy()).unwrap();
        let list = engine.memory_list(&json!({})).unwrap();
        let id = list["docs"][0]["id"].as_str().unwrap().to_string();
        let d = engine.memory_get(&json!({ "id": id })).unwrap();
        (d["_rev"].clone(), d["body"].as_str().unwrap().to_string())
    };
    let before = read_doc();
    assert!(before.1.contains("first body"), "{before:?}");

    let j = json_of(&dir, &["--scope", "write"]);
    assert!(
        j["instructions"]
            .as_str()
            .unwrap()
            .contains("1 doc(s) would be refreshed from disk"),
        "{j}"
    );
    assert_eq!(read_doc(), before, "the diagnostic wrote");
    let human = run(&dir, &["mcp", "instructions", "--scope", "write"]);
    assert!(
        human.ends_with("note: docs are shown as stored, before that refresh.\n"),
        "{human}"
    );
    assert!(!run(&dir, &["mcp", "instructions"]).contains("before that refresh"));
    // The read default carries no refresh line at all.
    assert!(!json_of(&dir, &[])["instructions"]
        .as_str()
        .unwrap()
        .contains("refreshed from disk"));

    // A real write-scoped initialize still refreshes.
    let real = initialize(&dir, Scope::Write);
    assert!(real.contains("1 doc(s) refreshed from disk"), "{real}");
    assert!(!real.contains("would be"), "{real}");
    let after = read_doc();
    assert!(after.1.contains("second body"), "{after:?}");
    assert_ne!(after.0, before.0);
}
