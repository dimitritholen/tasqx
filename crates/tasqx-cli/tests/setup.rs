//! `tasqx setup` through the real binary (D157).
//!
//! Every case points `--home` at a scratch directory, so nothing here reads or
//! writes the developer's own `~/.claude`, and `TASQX_DB` at a path that must
//! still not exist afterwards: setup is gated ahead of the store open.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A fresh scratch dir of this test's own, named per tag and per process.
fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("tasqx-setup-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// The binary with `--home <dir>/home`, its store and config inside `dir`.
fn bin(dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir.join("cfg"))
        .env("TASQX_DB", dir.join("tasks.db"))
        .arg("--no-daemon")
        .args(["setup", "--home"])
        .arg(dir.join("home"));
    c
}

fn run(c: &mut Command) -> (i32, String, String) {
    let out: Output = c.output().expect("run tasqx setup");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn skill_path(dir: &Path, name: &str) -> PathBuf {
    dir.join("home/.claude/skills").join(name).join("SKILL.md")
}

fn repo_skill(name: &str) -> Vec<u8> {
    std::fs::read(root().join(".claude/skills").join(name).join("SKILL.md"))
        .expect("the repo's skill file")
}

/// The line of `text` naming `item`, for asserting what one row says.
fn row<'a>(text: &'a str, item: &str) -> &'a str {
    text.lines()
        .find(|l| l.split_whitespace().next() == Some(item))
        .unwrap_or_else(|| panic!("no row for {item} in:\n{text}"))
}

#[test]
fn yes_installs_both_skills_byte_equal_and_a_rerun_changes_nothing() {
    let dir = scratch("yes");
    let (code, out, err) =
        run(bin(&dir).args(["--yes", "--only", "tasqx-workflow", "--only", "retro"]));
    assert_eq!(code, 0, "stdout: {out}\nstderr: {err}");
    for name in ["tasqx-workflow", "retro"] {
        assert_eq!(
            std::fs::read(skill_path(&dir, name)).expect("installed skill"),
            repo_skill(name),
            "{name} must be the repo's own file"
        );
        assert!(row(&out, name).contains("installed"), "{out}");
    }
    assert!(!out.contains("mcp"), "--only must leave mcp out: {out}");

    let before = std::fs::metadata(skill_path(&dir, "retro"))
        .and_then(|m| m.modified())
        .unwrap();
    let (code, out, _) =
        run(bin(&dir).args(["--yes", "--only", "tasqx-workflow", "--only", "retro"]));
    assert_eq!(code, 0);
    for name in ["tasqx-workflow", "retro"] {
        assert!(row(&out, name).contains("already current"), "{out}");
    }
    let after = std::fs::metadata(skill_path(&dir, "retro"))
        .and_then(|m| m.modified())
        .unwrap();
    assert_eq!(before, after, "a current skill must not be rewritten");
    assert!(
        !dir.join("tasks.db").exists(),
        "setup must not create a store"
    );
}

#[test]
fn an_edited_skill_is_kept_unless_forced() {
    let dir = scratch("edited");
    let path = skill_path(&dir, "retro");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "my own retro\n").unwrap();

    let (code, out, _) = run(bin(&dir).args(["--list", "--only", "retro"]));
    assert_eq!(code, 0);
    assert!(row(&out, "retro").contains("differs"), "{out}");

    let (code, out, _) = run(bin(&dir).args(["--yes", "--only", "retro"]));
    assert_eq!(code, 0);
    assert!(row(&out, "retro").contains("--force"), "{out}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "my own retro\n");

    let (code, out, _) = run(bin(&dir).args(["--yes", "--force", "--only", "retro"]));
    assert_eq!(code, 0);
    assert!(row(&out, "retro").contains("updated"), "{out}");
    assert_eq!(std::fs::read(&path).unwrap(), repo_skill("retro"));
}

/// No flags and no terminal: the list, exit 0, and never a screen, a hang or
/// a store.
#[test]
fn piped_with_no_flags_prints_the_list_and_opens_no_store() {
    let dir = scratch("piped");
    let (code, out, err) = run(&mut bin(&dir));
    assert_eq!(code, 0, "{err}");
    assert!(!out.contains('\u{1b}'), "no escapes into a pipe: {out:?}");
    for name in ["mcp", "tasqx-workflow", "retro"] {
        assert!(row(&out, name).contains("not installed"), "{out}");
    }
    assert!(
        !dir.join("tasks.db").exists(),
        "setup must not create a store"
    );
}

#[test]
fn list_reads_the_mcp_registration_from_claude_json() {
    let dir = scratch("mcpjson");
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let (_, out, _) = run(bin(&dir).args(["--list", "--only", "mcp"]));
    assert!(row(&out, "mcp").contains("not installed"), "{out}");

    std::fs::write(
        dir.join("home/.claude.json"),
        r#"{"mcpServers":{"other":{}}, "projects":{"/x":{"mcpServers":{"tasqx":{}}}}}"#,
    )
    .unwrap();
    let (_, out, _) = run(bin(&dir).args(["--list", "--only", "mcp"]));
    assert!(
        row(&out, "mcp").contains("not installed"),
        "a project-scoped registration is not the user-scope one: {out}"
    );

    std::fs::write(
        dir.join("home/.claude.json"),
        r#"{"mcpServers":{"tasqx":{"command":"tasqx"}}}"#,
    )
    .unwrap();
    let (_, out, _) = run(bin(&dir).args(["--list", "--only", "mcp"]));
    let line = row(&out, "mcp");
    assert!(
        line.contains("installed") && !line.contains("not installed"),
        "{out}"
    );
}

#[test]
fn json_list_is_a_document() {
    let dir = scratch("json");
    let (code, out, _) = run(bin(&dir).args(["--list"]).arg("--json"));
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    let names: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["mcp", "tasqx-workflow", "retro"]);
    assert_eq!(v["items"][2]["status"], "not installed");
}

#[test]
fn an_unknown_only_name_exits_2_naming_the_valid_ones() {
    let dir = scratch("nope");
    let (code, _, err) = run(bin(&dir).args(["--list", "--only", "nope"]));
    assert_eq!(code, 2);
    assert!(
        err.contains("nope") && err.contains("tasqx-workflow") && err.contains("retro"),
        "{err}"
    );
}

/// A PATH holding only a fake `claude` that writes its argv, one per line.
#[cfg(unix)]
fn fake_claude(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let argv = dir.join("argv.txt");
    let script = bin_dir.join("claude");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", argv.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin_dir, argv)
}

#[cfg(unix)]
#[test]
fn yes_registers_mcp_through_claude_mcp_add_at_user_scope() {
    let dir = scratch("claude");
    let (path, argv) = fake_claude(&dir);
    let (code, out, err) = run(bin(&dir)
        .env("PATH", &path)
        .args(["--yes", "--only", "mcp"]));
    assert_eq!(code, 0, "stdout: {out}\nstderr: {err}");
    let called = std::fs::read_to_string(&argv).expect("claude was called");
    assert_eq!(
        called.lines().collect::<Vec<_>>(),
        [
            "mcp", "add", "--scope", "user", "tasqx", "--", "tasqx", "mcp", "serve", "--scope",
            "write"
        ]
    );
    assert!(row(&out, "mcp").contains("installed"), "{out}");
    assert!(
        !dir.join("home/.claude.json").exists(),
        "setup never writes ~/.claude.json itself"
    );
}

#[cfg(unix)]
#[test]
fn without_claude_on_path_yes_prints_the_command_and_exits_0() {
    let dir = scratch("noclaude");
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let (code, out, err) = run(bin(&dir)
        .env("PATH", &empty)
        .args(["--yes", "--only", "mcp"]));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("Claude Code CLI not found"), "{out}");
    assert!(
        out.contains("claude mcp add --scope user tasqx -- tasqx mcp serve --scope write"),
        "{out}"
    );
}
