//! `tasqx note` and `tasqx notes` (D241): every verb through the real binary,
//! on a scratch store with `--no-daemon`.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

struct Store {
    dir: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("tasqx-notes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Store { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", self.dir.join("tasks.db"))
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
            .env("COLUMNS", "80")
            .arg("--no-daemon")
            .args(args)
            .output()
            .expect("run tasqx")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok(&a)).expect("--json prints JSON")
    }

    /// The short ids `notes` lists under these flags, in its order.
    fn listed(&self, flags: &[&str]) -> Vec<i64> {
        let mut a = vec!["notes"];
        a.extend_from_slice(flags);
        self.json(&a)["notes"]
            .as_array()
            .expect("notes array")
            .iter()
            .map(|n| n["short_id"].as_i64().unwrap())
            .collect()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn note_captures_its_words_verbatim_and_echoes_the_number() {
    let s = Store::new("add");
    s.ok(&["init", "infra"]);
    let out = s.ok(&["note", "try", "the", "tracing", "exporter"]);
    // A pipe is plain ASCII: the separator is `-`, the link mark `L`.
    assert_eq!(out, "Noted #1  -  try the tracing exporter\n");
    let n = s.json(&["note", "a filed one", "--project", "infra"]);
    assert_eq!(n["short_id"], 2);
    assert_eq!(n["body"], "a filed one");
    assert_eq!(n["project"], "infra");
    // A note is no task: the backlog is untouched.
    assert_eq!(s.json(&["list"])["count"], 0);
}

/// Prose that opens with a subverb's word is a note; a subverb alone, or
/// followed by a number, is a command typed the wrong way round and refused
/// with the right spelling. After `--` it is text again.
#[test]
fn a_first_word_that_is_a_subverb_is_text_unless_it_reads_as_a_command() {
    let s = Store::new("subverb-text");
    assert_eq!(
        s.json(&["note", "edit", "the", "readme"])["body"],
        "edit the readme"
    );
    assert_eq!(
        s.json(&["note", "promote", "Jan", "to", "lead"])["body"],
        "promote Jan to lead"
    );
    for (args, hint) in [
        (
            &["note", "archive", "7"][..],
            "did you mean: tasqx note 7 archive?",
        ),
        (&["note", "rm"][..], "did you mean: tasqx note <n> rm?"),
        (&["note", "edit"][..], "did you mean: tasqx note <n> edit?"),
    ] {
        let out = s.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(hint), "{args:?}: {err}");
        assert!(err.contains("put -- before it"), "{err}");
    }
    assert_eq!(s.json(&["note", "--", "archive", "7"])["body"], "archive 7");
    assert_eq!(s.listed(&[]), vec![3, 2, 1]);
}

/// The pre-pass marks one word with one marker and the consumer strips that
/// one; a user's own leading `\u{2}` is stored verbatim, by add and edit alike.
#[test]
fn a_literal_leading_marker_is_stored_verbatim() {
    let s = Store::new("marker");
    let n = s.json(&["note", "\u{2}hello", "\u{2}\u{2}world"]);
    assert_eq!(n["body"], "\u{2}hello \u{2}\u{2}world");
    let n = s.json(&["note", "1", "edit", "\u{2}hi", "\u{2}there"]);
    assert_eq!(n["body"], "\u{2}hi \u{2}there");
    let n = s.json(&["note", "--", "\u{2}7"]);
    assert_eq!(n["body"], "\u{2}7");
}

/// The empty-list hint points at `--all` only when it was not given.
#[test]
fn the_empty_hint_names_all_only_when_it_was_not_given() {
    let s = Store::new("empty-hint");
    assert!(s.ok(&["notes"]).contains("`--all`"));
    assert!(!s.ok(&["notes", "--links", "--all"]).contains("--all"));
    assert!(!s.ok(&["notes", "--archived"]).contains("--all"));
}

/// `note` was `annotate`'s alias: `tasqx note 42 "…"` annotated task 42. It is
/// refused now, naming the verb that does it, and nothing is stored.
#[test]
fn a_number_followed_by_text_is_a_usage_error_naming_annotate() {
    let s = Store::new("alias");
    s.ok(&["add", "a task"]);
    let out = s.run(&["note", "1", "called the plumber"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("to annotate task 1, use: tasqx annotate 1 \"…\""),
        "{err}"
    );
    assert_eq!(s.listed(&["--all"]), Vec::<i64>::new());
    assert_eq!(
        s.json(&["show", "1"])["annotations"]
            .as_array()
            .map_or(0, Vec::len),
        0
    );
}

#[test]
fn a_bare_id_prints_the_note_whole() {
    let s = Store::new("show");
    s.ok(&["note", "first line\nsecond line"]);
    let out = s.ok(&["note", "1"]);
    assert!(out.contains("#1"), "{out}");
    assert!(out.contains("first line\nsecond line"), "{out}");
    assert_eq!(s.json(&["note", "1"])["body"], "first line\nsecond line");
}

/// Printing a note files nothing, so `--project` beside a bare id is a usage
/// error, as it is beside `archive` and `rm`, never silently dropped.
#[test]
fn a_bare_id_with_a_project_is_a_usage_error() {
    let s = Store::new("show-project");
    s.ok(&["init", "p"]);
    s.ok(&["note", "one"]);
    for args in [
        &["note", "1", "--project", "p"][..],
        &["note", "--project", "p", "1"][..],
    ] {
        let out = s.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?} printed the note");
    }
    assert_eq!(s.listed(&["--all"]), vec![1], "nothing was stored");
}

#[test]
fn edit_archive_unarchive_and_rm() {
    let s = Store::new("change");
    s.ok(&["note", "draft"]);
    assert_eq!(
        s.ok(&["note", "1", "edit", "final", "text"]),
        "Updated note #1  -  final text\n"
    );
    assert_eq!(s.json(&["note", "1"])["body"], "final text");

    assert_eq!(s.ok(&["note", "1", "archive"]), "Archived note #1\n");
    assert_eq!(s.listed(&[]), Vec::<i64>::new());
    assert_eq!(s.listed(&["--archived"]), vec![1]);

    assert_eq!(s.ok(&["note", "1", "unarchive"]), "Unarchived note #1\n");
    assert_eq!(s.listed(&[]), vec![1]);

    let r = s.json(&["note", "1", "rm"]);
    assert_eq!(r["removed"], true);
    assert_eq!(s.listed(&["--all"]), Vec::<i64>::new());
    assert_eq!(s.run(&["note", "1"]).status.code(), Some(4));
}

#[test]
fn notes_filters_by_links_status_search_and_project() {
    let s = Store::new("filters");
    s.ok(&["init", "infra"]);
    s.ok(&["note", "read https://example.com/post"]);
    s.ok(&["note", "Tracing exporter idea", "--project", "infra"]);
    s.ok(&["note", "old thought"]);
    s.ok(&["note", "3", "archive"]);

    assert_eq!(s.listed(&[]), vec![2, 1]);
    assert_eq!(s.listed(&["--links"]), vec![1]);
    assert_eq!(s.listed(&["--archived"]), vec![3]);
    assert_eq!(s.listed(&["--all"]), vec![3, 2, 1]);
    assert_eq!(s.listed(&["--search", "TRACING"]), vec![2]);
    assert_eq!(s.listed(&["--project", "infra"]), vec![2]);
    assert_eq!(
        s.run(&["notes", "--archived", "--all"]).status.code(),
        Some(2)
    );

    let table = s.ok(&["notes"]);
    assert!(table.starts_with("2 notes\n"), "{table}");
    let link_row = table.lines().find(|l| l.contains("example.com")).unwrap();
    assert!(link_row.contains("  L  "), "no link mark: {link_row}");
    let plain_row = table.lines().find(|l| l.contains("Tracing")).unwrap();
    assert!(!plain_row.contains("  L  "), "{plain_row}");
    assert!(plain_row.contains("infra"), "{plain_row}");
}

/// A note that is just a link prints whole, past the terminal's width, so it
/// can still be copied and opened.
#[test]
fn a_lone_url_is_never_cut() {
    let s = Store::new("url");
    let url = format!("https://example.com/{}", "segment/".repeat(15));
    s.ok(&["note", &url]);
    s.ok(&["note", &"long prose ".repeat(12)]);
    let table = s.ok(&["notes"]);
    assert!(table.contains(&url), "{table}");
    assert!(!table.contains(&"long prose ".repeat(12)), "{table}");
}

#[test]
fn promote_to_task_files_the_task_and_archives_the_note() {
    let s = Store::new("promote-task");
    s.ok(&["init", "infra"]);
    s.ok(&[
        "note",
        "try the exporter\nit speaks OTLP",
        "--project",
        "infra",
    ]);
    let out = s.ok(&["note", "1", "promote", "task"]);
    assert!(out.contains("try the exporter"), "{out}");
    assert!(out.contains("added"), "{out}");
    assert!(out.ends_with("Archived note #1\n"), "{out}");

    let task = s.json(&["show", "1"]);
    assert_eq!(task["title"], "try the exporter");
    assert_eq!(task["project"], "infra");
    assert_eq!(
        task["annotations"][0]["body"],
        "try the exporter\nit speaks OTLP"
    );
    let note = s.json(&["note", "1"]);
    assert_eq!(note["status"], "archived");
    assert_eq!(note["promoted_task_short_id"], 1);
    assert!(s.ok(&["note", "1"]).contains("promoted to task #1"));

    // Promotion is final.
    assert_eq!(
        s.run(&["note", "1", "promote", "task"]).status.code(),
        Some(5)
    );
    assert_eq!(s.run(&["note", "1", "unarchive"]).status.code(), Some(5));
}

#[test]
fn promote_to_memory_takes_a_title() {
    let s = Store::new("promote-memory");
    s.ok(&["note", "deploys go blue-green"]);
    let r = s.json(&["note", "1", "promote", "memory", "--title", "Deploy rule"]);
    assert_eq!(r["doc"]["title"], "Deploy rule");
    assert_eq!(r["note"]["status"], "archived");
    let doc = s.json(&["memory", "show", r["doc"]["id"].as_str().unwrap()]);
    assert_eq!(doc["body"], "deploys go blue-green");
    assert_eq!(s.json(&["list"])["count"], 0);
}

/// Review round 1: flags typed before the id used to turn `7 archive` into
/// the text of a NEW note. A subverb after an id is a command wherever the
/// flags stand; one the subverb lacks is clap's usage error, never a note.
#[test]
fn an_id_and_a_subverb_are_a_command_whatever_flags_come_first() {
    let s = Store::new("flags-first");
    s.ok(&["init", "p"]);
    s.ok(&["note", "one"]);
    s.ok(&["note", "two"]);

    let parse = |out: String| -> Value { serde_json::from_str(&out).unwrap() };
    assert_eq!(s.json(&["note", "1", "archive"])["status"], "archived");
    assert_eq!(
        parse(s.ok(&["note", "--json", "1", "unarchive"]))["status"],
        "open"
    );
    assert_eq!(
        parse(s.ok(&["note", "1", "archive", "--json"]))["status"],
        "archived"
    );

    for args in [
        &["note", "--project", "p", "2", "archive"][..],
        &["note", "--project", "p", "2", "rm"][..],
    ] {
        assert_eq!(s.run(args).status.code(), Some(2), "{args:?}");
    }
    // Nothing was stored and nothing was removed.
    assert_eq!(s.listed(&["--all"]), vec![2, 1]);
    assert_eq!(s.json(&["note", "2"])["status"], "open");
}

/// After `--` every word is text, a number or a leading dash included.
#[test]
fn after_a_double_dash_everything_is_text() {
    let s = Store::new("dashdash");
    assert_eq!(s.json(&["note", "--", "7"])["body"], "7");
    assert_eq!(
        s.json(&["note", "--", "-5", "degrees"])["body"],
        "-5 degrees"
    );
    assert_eq!(s.json(&["note", "--", "1", "archive"])["body"], "1 archive");
    assert_eq!(s.listed(&[]), vec![3, 2, 1]);
}

#[test]
fn refusals_exit_with_their_codes() {
    let s = Store::new("refusals");
    for args in [
        &["note", "9", "archive"][..],
        &["note", "9", "edit", "x"][..],
        &["note", "9", "rm"][..],
        &["note", "9"][..],
    ] {
        assert_eq!(s.run(args).status.code(), Some(4), "{args:?}");
    }
    s.ok(&["note", "an idea"]);
    s.ok(&["note", "1", "promote", "task"]);
    assert_eq!(
        s.run(&["note", "1", "promote", "memory"]).status.code(),
        Some(5)
    );
    assert_eq!(s.run(&["note", "1", "unarchive"]).status.code(), Some(5));
}

/// `rm` says what it removed, cut to the terminal.
#[test]
fn rm_echoes_the_text_it_removed_cut_to_the_width() {
    let s = Store::new("rm-echo");
    s.ok(&["note", &"gone words ".repeat(20)]);
    let out = s.ok(&["note", "1", "rm"]);
    let line = out.lines().next().unwrap();
    assert!(line.starts_with("Removed note #1  -  gone words"), "{out}");
    assert!(line.chars().count() <= 80, "{line}");
    assert!(line.ends_with("..."), "{line}");
}

/// `edit` pins the rev it read and sends it as `expected_rev`; the rev moves
/// by one per edit. (A write landing between the read and the update cannot
/// be staged from here; the engine's own tests prove the conflict.)
#[test]
fn edit_pins_the_rev_it_read() {
    let s = Store::new("edit-rev");
    s.ok(&["note", "v1"]);
    assert_eq!(s.json(&["note", "1", "edit", "v2"])["_rev"], 1);
    assert_eq!(s.json(&["note", "1", "edit", "v3"])["_rev"], 2);
}
