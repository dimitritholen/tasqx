//! `tasqx show <ref> --card` / `tasqx brief <ref> --card` (D146).
//!
//! These drive the real binary because the property under test is what a pipe
//! actually carries: `--card` must draw Unicode borders under a pipe (the
//! `tasqx show 42 --card | pbcopy` use, where `caps.unicode` would otherwise be
//! `false`), and that can only be seen by capturing real stdout, not by calling
//! `markdown::task_card` directly. Same store/`--no-daemon` isolation as
//! `write_echoes.rs`.

use std::path::PathBuf;
use std::process::{Command, Output};

use unicode_width::UnicodeWidthStr;

struct Store {
    dir: PathBuf,
    db: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Store {
        let mut dir = std::env::temp_dir();
        dir.push(format!("tasqx-card-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create config dir");
        let db = dir.join("tasks.db");
        Store { dir, db }
    }

    fn bin(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", &self.db)
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
            .arg("--no-daemon");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.bin().args(args).output().expect("run tasqx")
    }

    /// stdout of a call expected to succeed.
    fn plain(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("stdout is utf-8")
    }
}

/// One task with a title, an annotation and two open checks, plus the
/// short_id `tasqx add` reports.
fn seeded_task(store: &Store) -> String {
    store.plain(&["init", "work"]);
    let added = store.plain(&["add", "Card test task", "--project", "work"]);
    let id = added
        .lines()
        .next()
        .and_then(|l| l.trim_start_matches('#').split_whitespace().next())
        .expect("`add` echoes `#<id> ...`")
        .to_string();
    store.plain(&[
        "annotate",
        &id,
        "a plain-language paragraph about this task",
    ]);
    store.plain(&["check", "add", &id, "tests pass"]);
    store.plain(&["check", "add", &id, "docs updated"]);
    id
}

#[test]
fn show_card_is_a_72_column_unicode_box_under_a_pipe() {
    let store = Store::new("show-unicode");
    let id = seeded_task(&store);

    let out = store.plain(&["show", &id, "--card"]);

    assert!(
        !out.contains('\u{1b}'),
        "a card must never be painted:\n{out}"
    );

    let lines: Vec<&str> = out.lines().collect();
    assert!(!lines.is_empty(), "the card must print something");
    for line in &lines {
        assert_eq!(
            UnicodeWidthStr::width(*line),
            72,
            "every card line must be exactly 72 display cells: {line:?}"
        );
    }

    assert!(
        lines[0].starts_with('┌'),
        "piped stdout must still draw Unicode borders (D146): {:?}",
        lines[0]
    );
    assert!(
        out.contains(&format!("Task #{id}")),
        "the card must name the task: {out}"
    );
    assert!(
        out.contains("Card test task"),
        "the card must carry the title: {out}"
    );
    assert!(
        out.contains("[ ] tests pass"),
        "open checks must show unchecked: {out}"
    );
    assert!(
        out.contains("[ ] docs updated"),
        "open checks must show unchecked: {out}"
    );
}

#[test]
fn show_card_ascii_draws_plus_minus_pipe_borders() {
    let store = Store::new("show-ascii");
    let id = seeded_task(&store);

    let out = store.plain(&["show", &id, "--card", "--ascii"]);

    // `--ascii` (Borders::Ascii, D146) is a promise about the BORDERS, not the
    // card's content — the same `·` separator the plain screen and the
    // Unicode card both use is still the right character for "pending ·
    // project work" under either border style. So the assertion is that no
    // box-drawing glyph survives, not that every byte is ASCII.
    let box_drawing = ['┌', '┬', '┐', '│', '├', '┼', '┤', '└', '┴', '┘'];
    assert!(
        !out.chars().any(|c| box_drawing.contains(&c)),
        "--ascii must leave no Unicode box-drawing glyph: {out:?}"
    );
    let first = out.lines().next().expect("a card has a first line");
    assert!(
        first.starts_with('+'),
        "--ascii must draw + - | borders: {first:?}"
    );
}

#[test]
fn ascii_without_card_is_a_clap_usage_error() {
    let store = Store::new("ascii-alone");
    let id = seeded_task(&store);

    let out = store.run(&["show", &id, "--ascii"]);
    assert!(
        !out.status.success(),
        "--ascii without --card must be refused"
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "a clap usage error exits 2: stderr was {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn brief_card_prints_the_task_card_then_the_brief_tail() {
    let store = Store::new("brief-card");
    let prereq = seeded_task(&store);
    let dependent = {
        let added = store.plain(&["add", "Depends on the card test task", "--project", "work"]);
        added
            .lines()
            .next()
            .and_then(|l| l.trim_start_matches('#').split_whitespace().next())
            .expect("`add` echoes `#<id> ...`")
            .to_string()
    };
    store.plain(&["dep", &dependent, &prereq]);

    let out = store.plain(&["brief", &dependent, "--card"]);

    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with('┌'),
        "a brief card opens with the same box the task card does: {:?}",
        lines.first()
    );
    assert!(
        out.contains("### Depends on"),
        "the brief tail must follow the card: {out}"
    );
    assert!(
        out.contains("a plain-language paragraph about this task"),
        "the prerequisite's own conclusion must be quoted: {out}"
    );
}

#[test]
fn show_without_card_is_unaffected_by_the_new_flags() {
    let store = Store::new("show-default");
    let id = seeded_task(&store);

    let out = store.plain(&["show", &id]);
    assert!(
        !out.starts_with('┌') && !out.starts_with('+'),
        "the default `show` screen must not switch to the D146 box card: {out}"
    );
}

#[test]
fn show_card_json_still_prints_json_on_stdout() {
    let store = Store::new("show-card-json");
    let id = seeded_task(&store);

    let out = store.plain(&["--json", "show", &id, "--card"]);
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("--json must still print JSON, even under --card: {e}\n{out}"));
    assert_eq!(
        v.get("short_id").and_then(serde_json::Value::as_i64),
        Some(id.parse().expect("id is numeric")),
        "the JSON body is task.get's result, unaffected by --card: {v}"
    );
}
