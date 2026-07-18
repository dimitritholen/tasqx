use std::process::Command;

use tasqx_cli::cmddoc::{RunKind, COMMAND_REF};

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

/// Split an example's command text the way a shell would: on whitespace, but
/// with double-quoted runs held together.
///
/// Naive `split_whitespace` would turn `--desc "Day job"` into three arguments
/// and hand clap a stray positional, so the guard would fail on examples that
/// are perfectly correct. Kept deliberately small: the reference examples use
/// double quotes and nothing else.
fn shell_split(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut started = false;
    for c in cmd.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                started = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Every `RunKind::Safe` example in `COMMAND_REF`, in declaration order, with
/// the leading `tasqx` stripped.
fn safe_examples() -> Vec<&'static str> {
    COMMAND_REF
        .iter()
        .flat_map(|d| d.examples.iter())
        .filter(|e| matches!(e.run, RunKind::Safe))
        .map(|e| e.cmd)
        .collect()
}

/// The reference examples are the first thing a new user copies, so a broken one
/// is a documentation bug that ships silently. This guard used to hand-copy the
/// list of commands it executed: thirteen of the twenty-seven Safe examples were
/// covered and the rest — every `chart`, `theme` and `export` example among them
/// — had never been run by anything. It now iterates `COMMAND_REF` itself, so
/// adding a Safe example automatically puts it under test and the two can no
/// longer drift apart.
#[test]
fn safe_examples_all_exit_zero() {
    let db = fresh_db("safe");

    // No seeding is needed: the Safe set is self-sufficient in declaration
    // order — the `init` examples create the projects the later `add
    // --project work` and `list project:work` examples consume, and the `add`
    // examples create the tasks the reports and charts read. Pre-creating
    // those projects here would make the `init` examples collide and exit 5.

    // Declaration order matters and is preserved on purpose: `init work` must
    // run before `add … --project work`, and the report/chart examples need the
    // tasks the earlier `add` examples created.
    let examples = safe_examples();
    // A filter bug that selected nothing would leave this test green while
    // executing zero commands; the floor makes that impossible to miss.
    assert!(examples.len() >= 27, "expected the full Safe set, got {}", examples.len());

    let mut failures = Vec::new();
    for cmd in examples {
        let args = shell_split(cmd);
        assert_eq!(args.first().map(String::as_str), Some("tasqx"), "`{cmd}` must start with tasqx");
        let out = bin().env("TASQX_DB", &db).args(&args[1..]).output().unwrap();
        if !out.status.success() {
            failures.push(format!(
                "`{cmd}` exited {:?}\n    stderr: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    let _ = std::fs::remove_file(&db);
    assert!(
        failures.is_empty(),
        "{} Safe example(s) in COMMAND_REF do not exit 0:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
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
