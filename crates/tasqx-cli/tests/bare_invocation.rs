//! What a bare `tasqx` must keep doing once it also opens a dashboard (D58).
//!
//! Bare `tasqx` prints the working set, and that is not an implementation
//! detail anybody may quietly reinterpret: DESIGN.md §5 shows it, `README.md`
//! shows it, both guides show it, and it is in the shell history of everyone
//! who has ever used this tool. D58 gives it a second meaning — a full-screen
//! dashboard — on exactly one condition, `is_interactive && !json &&
//! dashboard.enabled`. This file is the other side of that condition: the set
//! of invocations that must come out **byte for byte unchanged**.
//!
//! It exists because the failure it guards against is silent. A dashboard that
//! opened unconditionally would not error, would not warn, and would not fail
//! anybody's build; it would write `\x1b[?1049h` into a pipe and then block on
//! a key that never comes, or scribble a screenful of box-drawing characters
//! into whatever file the user redirected into. Scripts do not notice that
//! until something downstream reads garbage.
//!
//! **Every case here runs the real binary through `Command::output()`, which is
//! the point.** `output()` gives the child neither a terminal on stdout nor one
//! on stdin, so every invocation in this file is, by construction, on the
//! non-interactive side of `tui::is_interactive` — the same gate D26 wrote for
//! `config edit` after `CLICOLOR_FORCE=1 tasqx config edit | cat` hung forever
//! and had to be killed. There is no way to write these assertions against an
//! in-process helper: the thing being guarded is what a *process* does when
//! nobody is holding its streams.

use std::path::PathBuf;
use std::process::Command;

/// A fresh, isolated config dir + store for one test.
///
/// Same shape as `json_contract.rs`'s: the tag keeps parallel test binaries off
/// each other's paths, and both halves are removed first so a crashed earlier
/// run cannot make this one pass for the wrong reason.
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let mut cfg = std::env::temp_dir();
    cfg.push(format!("tasqx-bare-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cfg);
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let mut db = std::env::temp_dir();
    db.push(format!("tasqx-bare-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    (cfg, db)
}

fn bin(cfg: &std::path::Path, db: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    // `--no-daemon` for the reason `json_contract.rs` gives: a developer with a
    // daemon listening on the default socket must not change what this guard
    // sees.
    c.env("TASQX_CONFIG_DIR", cfg)
        .env("TASQX_DB", db)
        .arg("--no-daemon");
    c
}

/// Seed one project and two tasks, so the table under test has rows in it.
///
/// An empty store prints `No tasks.`, which would satisfy a weak assertion
/// ("still exits 0, still says something") while telling us nothing about the
/// table itself.
fn seed(cfg: &std::path::Path, db: &std::path::Path) {
    let ok = |c: &mut Command| {
        let out = c.output().expect("run tasqx");
        assert!(
            out.status.success(),
            "seed failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    ok(bin(cfg, db).args(["init", "work", "--desc", "seed"]));
    ok(bin(cfg, db).args(["add", "Ship the v1 freeze", "--project", "work"]));
    ok(bin(cfg, db).args(["add", "Write the migration", "--project", "work"]));
}

fn stdout_of(c: &mut Command) -> (String, i32) {
    let out = c.output().expect("run tasqx");
    (
        String::from_utf8(out.stdout).expect("stdout is utf-8"),
        out.status.code().unwrap_or(-1),
    )
}

/// The guarantee, in the strongest form it can be stated: the two spellings
/// produce the same bytes.
///
/// Asserting the two against each other rather than against a literal table is
/// deliberate. A literal would pin the column layout, which D51 is free to keep
/// changing, and would fail for reasons that have nothing to do with this
/// decision. What D58 promises is narrower and exactly this: off a terminal,
/// `tasqx` *is* `tasqx list`.
#[test]
fn a_piped_bare_tasqx_is_byte_for_byte_the_list_command() {
    let (cfg, db) = scratch("same");
    seed(&cfg, &db);

    let (bare, bare_code) = stdout_of(&mut bin(&cfg, &db));
    let (list, list_code) = stdout_of(bin(&cfg, &db).arg("list"));

    assert_eq!(bare_code, 0, "bare tasqx must exit 0");
    assert_eq!(list_code, 0, "tasqx list must exit 0");
    assert_eq!(
        bare, list,
        "bare `tasqx` and `tasqx list` must produce identical bytes off a terminal"
    );
    // And that shared output is the working-set table, not some other thing
    // both spellings happen to agree on.
    assert!(
        bare.contains("URG") && bare.contains("TASK") && bare.contains("PROJECT"),
        "the working-set table's columns are missing:\n{bare}"
    );
    assert!(
        bare.contains("Ship the v1 freeze"),
        "the seeded task is missing from the table:\n{bare}"
    );
}

/// No escape sequences reach a pipe — neither colour nor, more to the point,
/// the alternate-screen switch.
///
/// `\x1b[?1049h` is the byte sequence that would tell the receiving terminal to
/// swap screens. In a pipe it is noise; in a file it is corruption. This is the
/// single assertion that would fail loudest if a dashboard ever opened here.
#[test]
fn a_piped_bare_tasqx_emits_no_escape_sequences_at_all() {
    let (cfg, db) = scratch("noansi");
    seed(&cfg, &db);

    let (bare, _) = stdout_of(&mut bin(&cfg, &db));
    assert!(
        !bare.contains('\x1b'),
        "an escape byte reached a pipe:\n{bare:?}"
    );
}

/// `--json` keeps returning the `task.list` result, terminal or not.
///
/// The `--json` terminal in `run()` is what makes "every command speaks
/// `--json`" a property of the code's shape rather than a promise five call
/// sites keep independently (see `json_contract.rs`). A dashboard that bypassed
/// it would break that shape at the one invocation nobody types a verb for.
#[test]
fn a_bare_tasqx_with_json_still_returns_the_task_list_result() {
    let (cfg, db) = scratch("json");
    seed(&cfg, &db);

    let (out, code) = stdout_of(bin(&cfg, &db).arg("--json"));
    assert_eq!(code, 0, "bare `tasqx --json` must exit 0");

    let v: serde_json::Value = serde_json::from_str(&out).expect("bare --json must be valid JSON");
    assert_eq!(
        v["count"], 2,
        "the result must be the task.list result:\n{out}"
    );
    assert!(
        v["tasks"].is_array(),
        "the result must carry a `tasks` array:\n{out}"
    );
}

/// The escape hatch works, and works from the environment.
///
/// **This case is vacuously green today** — `TASQX_DASHBOARD` names a setting
/// D58 rules and that is not built yet, so the binary currently ignores it and
/// prints the table for the ordinary reason. It is written now, ahead of the
/// setting, because it is the case a CI image needs in one line, and because a
/// guard added after the feature is a guard nobody watched fail. It starts
/// biting the moment `dashboard.enabled` is read: from then on, an
/// implementation that consults the config file but not the env var fails here
/// rather than in somebody's pipeline.
#[test]
fn the_dashboard_can_be_switched_off_from_the_environment() {
    let (cfg, db) = scratch("envoff");
    seed(&cfg, &db);

    let (out, code) = stdout_of(bin(&cfg, &db).env("TASQX_DASHBOARD", "false"));
    assert_eq!(code, 0, "with the dashboard off, bare tasqx must exit 0");
    assert!(
        out.contains("URG") && out.contains("Ship the v1 freeze"),
        "with the dashboard off, bare tasqx must print the table:\n{out}"
    );
    assert!(
        !out.contains('\x1b'),
        "with the dashboard off, nothing may be drawn:\n{out:?}"
    );
}

/// An empty store still answers in one sentence, and still exits 0.
///
/// Kept separate from the seeded cases because it is the one bare invocation
/// with no table to print, and "prints nothing" and "prints a sentence" are
/// indistinguishable to a caller checking only the exit code — the shape
/// `pick.rs` states as "an empty body with no sentence in it reads as a hung
/// screen".
#[test]
fn a_bare_tasqx_against_an_empty_store_says_so_and_exits_zero() {
    let (cfg, db) = scratch("empty");

    let (out, code) = stdout_of(&mut bin(&cfg, &db));
    assert_eq!(code, 0, "an empty store is not an error");
    assert!(
        !out.trim().is_empty(),
        "an empty store must still produce a sentence"
    );
    assert!(
        !out.contains('\x1b'),
        "an empty store must not draw anything:\n{out:?}"
    );
}
