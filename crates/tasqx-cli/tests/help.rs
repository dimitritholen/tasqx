use std::process::Command;

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_tasqx")) }

fn help_of(verb: &str) -> String {
    let out = bin().args([verb, "--help"]).output().expect("run --help");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn init_short_help_shows_examples() {
    // -h must carry examples too (after_help, not after_long_help).
    let out = bin().args(["init", "-h"]).output().expect("run -h");
    let h = String::from_utf8_lossy(&out.stdout);
    assert!(h.contains("EXAMPLES"), "{h}");
    assert!(h.contains("tasqx init keuken-verbouwen"), "{h}");
}

#[test]
fn add_help_shows_examples() {
    let h = help_of("add");
    assert!(h.contains("EXAMPLES"), "{h}");
    assert!(h.contains("See also"), "{h}");
}

/// A fresh, isolated store path (file need not pre-exist; the engine creates it).
fn fresh_db(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("tasqx-help-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn safe_examples_all_exit_zero() {
    let db = fresh_db("safe");
    // Seed the projects the examples reference so they exit 0 under D23.
    for setup in ["init work", "init keuken-verbouwen"] {
        let ok = bin().env("TASQX_DB", &db).args(setup.split_whitespace())
            .status().unwrap().success();
        assert!(ok, "setup `{setup}` failed");
    }
    // Representative safe examples (read-only / idempotent). Keep in sync with
    // COMMAND_REF Safe entries; the unit guards assert each parses/starts with
    // `tasqx `, and these run them for real.
    let safe: &[&str] = &[
        "add Buy milk",
        "add Ship it due:friday +api !high --project work",
        "list",
        "list project:work",
        "next",
        "projects",
        "report",
        "report --all",
        "why 1",
        "show 1",
        "manual",
        "manual init",
        "manual filters",
    ];
    for cmd in safe {
        let args: Vec<&str> = cmd.split_whitespace().collect();
        let out = bin().env("TASQX_DB", &db).args(&args).output().unwrap();
        assert!(out.status.success(),
            "example `tasqx {cmd}` exited {:?}\nstderr: {}",
            out.status.code(), String::from_utf8_lossy(&out.stderr));
    }
    let _ = std::fs::remove_file(&db);
}

#[test]
fn manual_toc_and_sections_work() {
    let out = bin().arg("manual").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("TASQX MANUAL"));
    // piped => plain, no ANSI
    assert!(!s.contains('\x1b'), "piped manual leaked ANSI");

    let ok = bin().args(["manual", "init"]).output().unwrap();
    assert!(String::from_utf8_lossy(&ok.stdout).contains("tasqx init keuken-verbouwen"));
}

#[test]
fn manual_unknown_topic_exits_2() {
    let out = bin().args(["manual", "definitely-not-a-topic"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "unknown manual arg must be bad_request");
    assert!(String::from_utf8_lossy(&out.stderr).contains("definitely-not-a-topic"));
}
