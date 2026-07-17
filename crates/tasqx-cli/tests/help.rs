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
