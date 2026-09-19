//! The write echoes read as one program (D126, task #348).
//!
//! Eighteen verbs answered a write in eighteen dialects: `Started · timer
//! running (since 2026-07-16T08:51:09.6070293Z)`, `#60  ->  cancelled`,
//! `#53 now depends on #50 · depends on: #50 blocked=true`, `Imported 62
//! task(s)…`. D126 gives them `add`'s voice from D122: the task the command
//! named, as `#N  Title`, then one line that opens with what happened and what
//! changed, then `list`'s facts in `list`'s order.
//!
//! These run the real binary because the echo is what reaches a terminal, and
//! half of what D126 rules is about the terminal (the rail, the fit, what goes
//! to stderr) that a unit test of a renderer cannot see. Every call gets a
//! scratch store and `--no-daemon`; see `bin`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use unicode_width::UnicodeWidthStr;

struct Store {
    dir: PathBuf,
    db: PathBuf,
}

impl Store {
    /// A fresh config dir and store per test, named by tag and pid so
    /// parallel tests never share one.
    fn new(tag: &str) -> Store {
        let mut dir = std::env::temp_dir();
        dir.push(format!("tasqx-echo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create config dir");
        let db = dir.join("tasks.db");
        Store { dir, db }
    }

    /// The binary on this store. `--no-daemon` rides on the fixture: a
    /// reachable daemon ignores `TASQX_DB` and would answer from the real
    /// store instead.
    fn bin(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", &self.db)
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
            .env_remove("NO_COLOR")
            .env_remove("TASQX_FORCE_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .arg("--no-daemon");
        c
    }

    fn run(&self, mut c: Command, args: &[&str]) -> Output {
        let out = c.args(args).output().expect("run tasqx");
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// Piped: the plain path.
    fn plain(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.run(self.bin(), args).stdout).into_owned()
    }

    /// A terminal `cols` wide under NO_COLOR, so the only SGR left is bold,
    /// which is exactly the emphasis D126 gives a meaning to.
    fn term_raw(&self, cols: usize, args: &[&str]) -> Output {
        let mut c = self.bin();
        c.env("TASQX_FORCE_COLOR", "1")
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            .env("COLUMNS", cols.to_string());
        self.run(c, args)
    }

    fn term(&self, cols: usize, args: &[&str]) -> String {
        strip(&String::from_utf8_lossy(&self.term_raw(cols, args).stdout))
    }

    fn term_ansi(&self, cols: usize, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.term_raw(cols, args).stdout).into_owned()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_slice(&self.run(self.bin(), &all).stdout).expect("--json is JSON")
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

/// Drop every SGR sequence.
fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The runs of text drawn bold on one line, trimmed. Under NO_COLOR bold is the
/// only emphasis a line can carry.
fn bold_runs(line: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut bold = false;
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            let mut code = String::new();
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
                code.push(n);
            }
            let codes: Vec<&str> = code.trim_start_matches('[').split(';').collect();
            let now_bold = if codes.contains(&"0") || code == "[" {
                false
            } else {
                bold || codes.contains(&"1")
            };
            if bold && !now_bold && !cur.trim().is_empty() {
                runs.push(cur.trim().to_string());
                cur.clear();
            }
            if !bold && now_bold {
                cur.clear();
            }
            bold = now_bold;
        } else if bold {
            cur.push(c);
        }
    }
    if bold && !cur.trim().is_empty() {
        runs.push(cur.trim().to_string());
    }
    runs
}

/// A project, and four tasks: #1 due Friday with an estimate, #2 plain, #3
/// waiting on both, #4 to be cancelled.
fn seeded(tag: &str) -> Store {
    let st = Store::new(tag);
    st.plain(&["init", "work"]);
    st.plain(&[
        "add",
        "Write the report",
        "due:friday",
        "est:3h",
        "+docs",
        "!H",
    ]);
    st.plain(&["add", "Review the draft"]);
    st.plain(&["add", "Ship it"]);
    st.plain(&["add", "Tidy the backlog"]);
    st.plain(&["dep", "3", "1"]);
    st.plain(&["dep", "3", "2"]);
    st
}

/// A store whose newest event is the one `undo` is to reverse — written by the
/// binary rather than planted.
///
/// `undo` reverses the event appended LAST (`engine/undo.rs`), so the fixture
/// imports the two tasks and then makes the change to be undone through the
/// CLI, which leaves that change as the newest row whatever the wall clock
/// does. It used to plant an event carrying an id above anything the clock can
/// mint, because undo chose the HIGHEST id, and the ids of writes made by
/// separate processes follow the wall clock: on WSL2 under load that clock was
/// seen to step back 2.4 s mid-test, putting an earlier `add` above the `untag`
/// written after it and failing a chain of CLI writes by construction. #422
/// moved the choice to append order, which is exactly what makes an ordinary
/// chain of CLI writes safe here — and planting no longer works anyway, since
/// an import writes a bookkeeping event per row it touches and is therefore
/// itself the newest thing in the log when it returns.
///
/// The import still seeds the tasks, because `stop` has to reverse a two-hour
/// interval and no fast test can produce one by waiting: the task is imported
/// `active`, carrying its open interval's anchor two hours back (D42).
fn undo_store(tag: &str, op: &str, title: &str) -> Store {
    let st = Store::new(tag);
    let t1 = "019f0000-0000-7000-8000-0000000000c1";
    let t2 = "019f0000-0000-7000-8000-0000000000c2";
    let mut first = serde_json::json!({
        "id": t1, "short_id": 1, "title": title, "status": "pending",
        "project": "work", "tags": ["docs"], "tracked_seconds": 13260,
    });
    if op == "stop" {
        let since = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(7200);
        first["status"] = serde_json::json!("active");
        first["active_since"] = serde_json::json!(since.to_string());
    }
    let doc = serde_json::json!({
        "projects": [{ "name": "work" }],
        "tasks": [
            first,
            { "id": t2, "short_id": 2, "title": "Review the draft", "status": "pending",
              "project": "work" },
        ],
    });
    let path = st.path().join("undo.json");
    std::fs::write(&path, doc.to_string()).expect("write undo fixture");
    st.plain(&["import", path.to_str().expect("utf8 path")]);
    // The change to be undone: made last, and made for real.
    match op {
        "stop" => {
            st.plain(&["stop", "1"]);
        }
        "tag.remove" => {
            st.plain(&["tag", "1", "urgent"]);
            st.plain(&["untag", "1", "urgent"]);
        }
        "annotation.add" => {
            st.plain(&["annotate", "1", "call the printer"]);
        }
        "annotation.update" => {
            let id = st.json(&["annotate", "1", "call the printr"])["annotation"]["id"]
                .as_str()
                .expect("the note's id")
                .to_string();
            st.plain(&["annotate", "1", "--edit", &id, "call the printer"]);
        }
        "dependency.remove" => {
            st.plain(&["dep", "1", "2"]);
            st.plain(&["undep", "1", "2"]);
        }
        other => panic!("no seed for {other}"),
    }
    st
}

/// An instant (`2026-09-11T…`) or an ISO duration (`PT5M`) anywhere in `s`.
fn store_spelling(s: &str) -> Option<String> {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i..].starts_with(b"PT") && b.get(i + 2).is_some_and(u8::is_ascii_digit) {
            return Some(s[i..].chars().take(12).collect());
        }
        if i + 11 <= b.len()
            && b[i..i + 4].iter().all(u8::is_ascii_digit)
            && b[i + 4] == b'-'
            && b[i + 7] == b'-'
            && b[i + 10] == b'T'
        {
            return Some(s[i..].chars().take(24).collect());
        }
    }
    None
}

/// Rule 1 and D126: whatever else happened, the first line is the task the
/// command named. `start` printed the task it had just auto-stopped first, and
/// nine verbs never named their task at all (`#60  ->  cancelled`).
#[test]
fn every_task_echo_opens_with_the_task_the_command_named() {
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["start", "1"], "#1  Write the report"),
        (vec!["start", "2"], "#2  Review the draft"),
        (vec!["stop", "2"], "#2  Review the draft"),
        (vec!["modify", "1", "due:monday"], "#1  Write the report"),
        (vec!["tag", "1", "+urgent"], "#1  Write the report"),
        (vec!["untag", "1", "+urgent"], "#1  Write the report"),
        (
            vec!["annotate", "2", "call", "the", "printer"],
            "#2  Review the draft",
        ),
        (vec!["undep", "3", "2"], "#3  Ship it"),
        (vec!["dep", "3", "2"], "#3  Ship it"),
        (vec!["done", "2"], "#2  Review the draft"),
        (vec!["reopen", "2"], "#2  Review the draft"),
        (vec!["cancel", "4"], "#4  Tidy the backlog"),
    ];
    // The same writes on two stores: one piped, one on a terminal.
    let plain = seeded("opens-with-task-plain");
    let term = seeded("opens-with-task-term");
    for (args, want) in &cases {
        let out = plain.plain(args);
        assert_eq!(
            out.lines().next().unwrap_or(""),
            *want,
            "{args:?} did not open with its task:\n{out}"
        );
        let out = term.term(80, args);
        assert_eq!(
            out.lines().next().unwrap_or(""),
            format!("▌ {want}"),
            "{args:?} on a terminal did not open with its task:\n{out}"
        );
    }
    let ann = plain.json(&["annotate", "1", "scratch", "note"]);
    let id = ann["annotation"]["id"].as_str().expect("annotation id");
    let out = plain.plain(&["unannotate", "1", id]);
    assert_eq!(out.lines().next(), Some("#1  Write the report"), "{out}");

    // `undo`, on a store whose newest event is the one it reverses.
    let out = undo_store("opens-undo-plain", "tag.remove", "Write the report").plain(&["undo"]);
    assert_eq!(out.lines().next(), Some("#1  Write the report"), "{out}");
    let out = undo_store("opens-undo-term", "tag.remove", "Write the report").term(80, &["undo"]);
    assert_eq!(out.lines().next(), Some("▌ #1  Write the report"), "{out}");
}

/// Rule 3: calendar days, not instants, and `5m`, not `PT5M`. `start` said
/// `since 2026-…Z`, `done` said `completed 2026-…Z`, `modify` echoed the
/// resolved instant at someone who had typed `due:friday`, and `undo` said
/// `PT0S back on the clock`.
#[test]
fn no_echo_spells_an_instant_or_an_iso_duration() {
    let st = seeded("no-iso");
    let mut seen = Vec::new();
    // Every date-bearing field `modify` can set, a reminder both as an
    // instant and as a day, and each of them cleared again: review round 2
    // found `remind 2026-09-12T00:00:00Z` on both paths.
    for args in [
        vec!["start", "1"],
        vec!["start", "2"],
        vec!["stop", "2"],
        vec!["modify", "1", "due:monday", "est:2h"],
        vec!["modify", "1", "remind:tomorrow"],
        vec!["modify", "1", "remind:2026-12-24T09:00"],
        vec!["modify", "1", "scheduled:monday", "wait:tomorrow"],
        vec![
            "modify",
            "1",
            "--clear",
            "remind",
            "--clear",
            "scheduled",
            "--clear",
            "wait",
            "--clear",
            "due",
        ],
        vec!["done", "2"],
        vec!["start", "1"],
    ] {
        let plain = st.plain(&args);
        if let Some(bad) = store_spelling(&plain) {
            seen.push(format!("{args:?} (plain): {bad:?}\n{plain}"));
        }
        if args[0] == "modify" {
            let term = st.term(80, &args);
            if let Some(bad) = store_spelling(&term) {
                seen.push(format!("{args:?} (terminal): {bad:?}\n{term}"));
            }
        }
    }
    let term = st.term(80, &["done", "1"]);
    if let Some(bad) = store_spelling(&term) {
        seen.push(format!("done (terminal): {bad:?}\n{term}"));
    }
    for (tag, how) in [("no-iso-undo-plain", false), ("no-iso-undo-term", true)] {
        let u = undo_store(tag, "stop", "Write the report");
        let out = if how {
            u.term(80, &["undo"])
        } else {
            u.plain(&["undo"])
        };
        if let Some(bad) = store_spelling(&out) {
            seen.push(format!("undo of a stop: {bad:?}\n{out}"));
        }
    }
    assert!(seen.is_empty(), "{}", seen.join("\n"));
}

/// Rule 9: an echo fits the terminal. No echo read the width, so at 60
/// columns twelve of eighteen wrapped, and `done`'s token hint alone ran 190.
#[test]
fn every_echo_fits_a_sixty_column_terminal() {
    let st = Store::new("fits-60");
    st.plain(&["init", "a-project-with-a-rather-long-name"]);
    let long = "Reconcile the quarterly invoices against the ledger export before Friday";
    st.plain(&[
        "add",
        long,
        "due:friday",
        "est:3h",
        "+finance",
        "+quarterly",
        "!H",
    ]);
    st.plain(&["add", "Second task with a title long enough to matter here"]);
    st.plain(&["add", "Third"]);
    let mut over = Vec::new();
    for args in [
        vec!["start", "1"],
        vec!["start", "2"],
        vec!["stop", "2"],
        vec!["modify", "1", "due:monday", "est:4h", "!M", "+extra"],
        vec!["tag", "1", "+one", "+two", "+three"],
        vec!["untag", "1", "+one"],
        vec!["dep", "3", "1"],
        vec!["undep", "3", "1"],
        vec!["dep", "3", "1"],
        vec![
            "annotate",
            "1",
            "a",
            "note",
            "long",
            "enough",
            "that",
            "it",
            "has",
            "to",
            "wrap",
            "somewhere",
            "on",
            "a",
            "sixty",
            "column",
            "terminal",
            "screen",
        ],
        vec!["done", "1"],
        vec!["reopen", "1"],
        vec!["cancel", "2"],
        vec!["init", "second-project-with-a-long-name-too"],
        vec!["use", "second-project-with-a-long-name-too"],
        vec!["archive", "a-project-with-a-rather-long-name"],
    ] {
        let out = st.term_raw(60, &args);
        overflow(&mut over, &format!("{args:?}"), &out);
    }
    // `undo` of each op it reverses, each on a store whose newest event it is.
    for op in [
        "stop",
        "tag.remove",
        "annotation.add",
        "annotation.update",
        "dependency.remove",
    ] {
        let u = undo_store(&format!("fits-60-undo-{op}"), op, long);
        overflow(&mut over, &format!("undo {op}"), &u.term_raw(60, &["undo"]));
    }
    // `import` of a document with neither a `projects` nor a `docs` section,
    // so both of its notes print: they were 79 and 125 cells.
    let imp = Store::new("fits-60-import");
    let doc = imp.path().join("old.json");
    std::fs::write(
        &doc,
        serde_json::json!({ "tasks": [{
            "id": "019f0000-0000-7000-8000-0000000000d1", "short_id": 1,
            "title": long, "status": "pending",
            "project": "a-project-with-a-rather-long-name",
        }]})
        .to_string(),
    )
    .expect("write import fixture");
    let out = imp.term_raw(60, &["import", doc.to_str().expect("utf8 path")]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("note:"),
        "the fixture must make import print its notes"
    );
    overflow(&mut over, "import", &out);
    assert!(over.is_empty(), "{}", over.join("\n"));
}

/// Every line of a run's stdout and stderr wider than 60 cells.
fn overflow(over: &mut Vec<String>, what: &str, out: &Output) {
    for stream in [&out.stdout, &out.stderr] {
        for line in strip(&String::from_utf8_lossy(stream)).lines() {
            if line.width() > 60 {
                over.push(format!("{what}: {} cells: {line:?}", line.width()));
            }
        }
    }
}

/// Rule 4 (D126, amendment 1): the state glyph sits in the rail column and
/// nowhere else, `▶` while running and `⊘` while blocked, `▌` otherwise — and
/// without Unicode it is `*`/`B` at column 0.
#[test]
fn the_rail_column_carries_running_and_blocked_and_nothing_else() {
    let st = seeded("rail");
    let start = st.term(80, &["start", "1"]);
    let l: Vec<&str> = start.lines().collect();
    assert!(l[0].starts_with('▌'), "{start}");
    assert!(l[1].starts_with('▶'), "running goes in the rail: {start}");

    // Auto-stop: the card first, the other task under it, glyph-free.
    let start2 = st.term(80, &["start", "2"]);
    let l: Vec<&str> = start2.lines().collect();
    assert!(l[0].contains("#2"), "{start2}");
    assert!(
        l.iter()
            .skip(2)
            .any(|x| x.starts_with("  #1") && x.contains("stopped")),
        "the auto-stopped task goes under the card: {start2}"
    );

    let stop = st.term(80, &["stop", "2"]);
    assert!(stop.lines().nth(1).unwrap().starts_with('▌'), "{stop}");

    let blocked = st.term(80, &["dep", "4", "1"]);
    assert!(
        blocked.lines().nth(1).unwrap().starts_with('⊘'),
        "{blocked}"
    );

    st.plain(&["done", "1"]);
    let reopen = st.term(80, &["reopen", "1"]);
    assert!(
        reopen.lines().any(|x| x.starts_with("⊘ #4")),
        "a re-blocked task carries ⊘ in the rail column: {reopen}"
    );

    for out in [&start, &start2, &stop, &blocked, &reopen] {
        for line in out.lines() {
            for g in ['▶', '⊘'] {
                if let Some(i) = line.find(g) {
                    assert_eq!(i, 0, "{g} away from the rail: {line:?}");
                }
            }
            assert!(!line.contains('↩'), "only ▶/⊘ may take the slot: {line:?}");
        }
    }

    let plain = st.plain(&["start", "2"]);
    assert!(plain.lines().nth(1).unwrap().starts_with("* "), "{plain}");
    let plain = st.plain(&["dep", "2", "1"]);
    assert!(plain.lines().nth(1).unwrap().starts_with("B "), "{plain}");
}

/// D126, amendment 2: bold on the second line means "this write changed it",
/// and nothing else. Under NO_COLOR bold is the only emphasis left, so a bold
/// `due` that nobody touched says something that did not happen.
#[test]
fn bold_on_the_second_line_means_this_write_changed_it() {
    let st = seeded("bold");
    let add = st.term_ansi(
        100,
        &["add", "Plan the offsite", "due:friday", "est:2h", "+team"],
    );
    let line2 = add.lines().nth(1).unwrap();
    assert_eq!(bold_runs(line2), vec!["added"], "{:?}", strip(line2));

    let modify = st.term_ansi(100, &["modify", "5", "due:monday"]);
    let line2 = modify.lines().nth(1).unwrap();
    let runs = bold_runs(line2);
    assert!(runs.iter().any(|r| r == "modified"), "{runs:?}");
    assert!(runs.iter().any(|r| r.starts_with("due ")), "{runs:?}");
    for untouched in ["work", "+team", "est"] {
        assert!(
            !runs.iter().any(|r| r.contains(untouched)),
            "{untouched} was not changed but is bold: {runs:?}"
        );
    }

    let start = st.term_ansi(100, &["start", "5"]);
    assert_eq!(bold_runs(start.lines().nth(1).unwrap()), vec!["started"]);
    let again = st.term_ansi(100, &["start", "5"]);
    let line2 = again.lines().nth(1).unwrap();
    assert!(strip(line2).contains("already running"), "{again}");
    assert!(
        bold_runs(line2).is_empty(),
        "nothing changed, so nothing is bold: {:?}",
        bold_runs(line2)
    );
}

/// D126, amendment 3 (rules 2 and 8): a closed task has no urgency to rank,
/// and a zero is not a fact. `done` said `tracked 0s of a 3h estimate`.
#[test]
fn done_and_cancel_print_no_urgency_and_no_zero_facts() {
    let st = seeded("done-cancel");
    for (args, outcome) in [
        (vec!["done", "1"], "done"),
        (vec!["cancel", "2"], "cancelled"),
    ] {
        let out = st.plain(&args);
        let line2 = out.lines().nth(1).unwrap_or("");
        assert!(line2.starts_with(outcome), "{args:?}: {out}");
        assert!(
            !out.split_whitespace().any(|w| w == "0s"),
            "a zero printed: {out}"
        );
        assert!(
            !line2
                .split_whitespace()
                .any(|w| w.contains('.') && w.chars().all(|c| c.is_ascii_digit() || c == '.')),
            "a closed task drew its urgency: {out}"
        );
        assert!(line2.contains("work"), "the context stays: {out}");
    }
}

/// D126, amendment 4: one spelling for a closed timer interval. The auto-stop
/// said `tracked 1m15s`, `stop` said `interval 1m15s · tracked 1m15s`, and
/// `undo` said `the timer is running again · PT0S back on the clock`.
#[test]
fn a_closed_interval_is_spelled_stopped_after_everywhere() {
    // Seeded through `import` with a timer two hours old (D42's
    // `active_since`/`tracked_seconds`): a real start/stop in a test closes an
    // interval of 0s, and a zero is not a fact, so it would prove nothing.
    // What the lines must say is derived from the totals the binary itself
    // stored (`show --json`), never from this test's clock, which the
    // binary's may differ from (WSL2 was seen to step 2.4 s).
    let two_hours_ago = (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(2)).to_string();
    let seed = |st: &Store| {
        let fixture = st.path().join("seed.json");
        std::fs::write(
            &fixture,
            serde_json::json!({ "tasks": [
                {
                    "id": "019f0000-0000-7000-8000-0000000000b1",
                    "short_id": 1,
                    "title": "Write the report",
                    "status": "active",
                    "tracked_seconds": 3600,
                    "active_since": two_hours_ago,
                },
                {
                    "id": "019f0000-0000-7000-8000-0000000000b2",
                    "short_id": 2,
                    "title": "Review the draft",
                    "status": "pending",
                },
            ]})
            .to_string(),
        )
        .expect("write fixture");
        st.plain(&["import", fixture.to_str().unwrap()]);
    };

    // start's auto-stop
    let st = Store::new("stopped-after-auto");
    seed(&st);
    let start2 = st.plain(&["start", "2"]);
    assert_eq!(
        start2.lines().next(),
        Some("#2  Review the draft"),
        "{start2}"
    );
    let interval = tracked_secs(&st, "1") - 3600;
    assert!(
        start2
            .lines()
            .any(|l| l.starts_with(&format!("  #1  stopped after {}", compact(interval)))),
        "{start2}"
    );

    // stop, and the total only because it says more than the interval
    let st = Store::new("stopped-after-stop");
    seed(&st);
    let stop = st.plain(&["stop", "1"]);
    let line2 = stop.lines().nth(1).unwrap();
    let total = tracked_secs(&st, "1");
    let want = format!(
        "stopped after {}   tracked {}",
        compact(total - 3600),
        compact(total)
    );
    assert!(line2.starts_with(&want), "want {want:?}: {stop}");
    assert!(!stop.contains("interval"), "{stop}");

    // undo of a stop: `*` says it runs again, and how long it has been running.
    // Elapsed, not an instant: `due_cell`'s clock is UTC and reads as the wall
    // clock it is not (D126 l). What comes back is the two-hour interval, not
    // a total, so it is not `tracked`. On a store whose newest event is that
    // stop (see `undo_store`).
    let undo = undo_store("stopped-after-undo", "stop", "Write the report").plain(&["undo"]);
    let line2 = undo.lines().nth(1).unwrap();
    assert!(line2.starts_with("* undid stop   for "), "{undo}");
    assert!(!line2.contains("tracked"), "{undo}");
    assert!(
        !undo.contains("running again"),
        "rule 11: * says it: {undo}"
    );
}

/// The total a task's `show --json` holds, in seconds.
fn tracked_secs(st: &Store, r: &str) -> i64 {
    let t = st.json(&["show", r]);
    tasqx_core::util::duration_secs(t["tracked"].as_str().expect("tracked"))
        .expect("tracked parses")
}

/// A duration as the echoes spell it (`2h`, `3h41`, `52m`), from its seconds.
fn compact(secs: i64) -> String {
    match secs {
        s if s >= 3600 && (s % 3600) / 60 == 0 => format!("{}h", s / 3600),
        s if s >= 3600 => format!("{}h{:02}", s / 3600, (s % 3600) / 60),
        s if s >= 60 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// D126, amendment 8: an `undep` that leaves another blocker names it, and
/// never claims the task is free.
#[test]
fn undep_names_the_blocker_that_remains() {
    let st = seeded("undep-still");
    let out = st.plain(&["undep", "3", "1"]);
    let line2 = out.lines().nth(1).unwrap();
    assert!(line2.starts_with("B "), "{out}");
    assert!(out.contains("still blocked by #2"), "{out}");
    assert!(!out.contains("unblocked"), "{out}");
    let freed = st.plain(&["undep", "3", "2"]);
    assert!(!freed.lines().nth(1).unwrap().starts_with("B "), "{freed}");
    assert!(freed.contains("no longer waits on #2"), "{freed}");
}

/// D126, amendment 7: a project and a store are not tasks, so their echoes
/// draw no rail, and the project's name is the first line.
#[test]
fn project_and_store_echoes_draw_no_rail() {
    let st = seeded("no-rail");
    for (args, first) in [
        (vec!["init", "research"], Some("research")),
        (vec!["use", "research"], Some("research")),
        (vec!["archive", "work"], Some("work")),
    ] {
        let out = st.term(80, &args);
        assert!(!out.contains('▌'), "{args:?} drew a rail: {out}");
        if let Some(f) = first {
            assert_eq!(out.lines().next(), Some(f), "{args:?}: {out}");
        }
    }
    let export = st.path().join("export.json");
    let doc = st.plain(&["export"]);
    std::fs::write(&export, doc).unwrap();
    let fresh = Store::new("no-rail-import");
    let out = fresh.term(80, &["import", export.to_str().unwrap()]);
    assert!(!out.contains('▌'), "{out}");
    assert!(!out.contains("(s)"), "rule 8: {out}");
    assert!(
        out.contains("4 tasks") && out.contains("no memory docs"),
        "{out}"
    );
}

/// Rule 1 and D126: the card is stdout's first line, so a hint about the write
/// goes to stderr, after it. The token hint was a 190-cell paragraph on stdout
/// under every `done`.
#[test]
fn the_token_note_goes_to_stderr_as_one_line() {
    let st = seeded("token-note");
    let out = st.run(st.bin(), &["done", "2"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stdout.contains("token"), "{stdout}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("note: "), "{stderr}");
    let json = st.json(&["done", "4"]);
    assert!(
        json["tokens_hint"]
            .as_str()
            .is_some_and(|h| h.contains("input_tokens")),
        "--json keeps core's hint verbatim (D56): {json}"
    );
}

/// D126 (i): the note about a write follows its card. On a terminal stdout and
/// stderr are one stream, so a note written while the verb ran (stderr goes
/// out at once, stdout at the end of `run`) landed above the card. Here both
/// point at one file, in the order the bytes were written.
#[test]
fn the_token_note_lands_under_the_card_on_one_stream() {
    let st = seeded("note-order");
    let path = st.path().join("both.txt");
    let file = std::fs::File::create(&path).expect("create capture file");
    let status = st
        .bin()
        .args(["done", "2"])
        .stdout(file.try_clone().expect("clone capture file"))
        .stderr(file)
        .status()
        .expect("run tasqx");
    assert!(status.success());
    let both = std::fs::read_to_string(&path).expect("read capture");
    let card = both.find("#2  Review the draft").expect("the card");
    let note = both.find("note: ").expect("the note");
    assert!(card < note, "the note came first:\n{both}");
}

/// D126, amendment 6: `add` gives way the way `list` does, so its facts come
/// in `list`'s order: urgency, project, due, tags, est.
#[test]
fn add_orders_its_facts_the_way_list_does() {
    let st = seeded("add-order");
    let out = st.term(
        120,
        &["add", "Order check", "due:friday", "est:2h", "+team", "!H"],
    );
    let line2 = out.lines().nth(1).unwrap();
    let at = |needle: &str| {
        line2
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing: {line2:?}"))
    };
    assert!(at("work") < at("due "), "{line2}");
    assert!(at("due ") < at("+team"), "{line2}");
    assert!(at("+team") < at("est "), "{line2}");
    let plain = st.plain(&["add", "Plain check", "+team"]);
    let l: Vec<&str> = plain.lines().collect();
    assert!(
        l[0].starts_with('#') && l[0].ends_with("  Plain check"),
        "{plain}"
    );
    assert!(l[1].starts_with("added"), "{plain}");
}

/// D165: `annotate --edit` replaces the note's text under its own id, says so,
/// and `undo` puts the old text back.
#[test]
fn annotate_edit_replaces_the_note_in_place_and_undo_restores_it() {
    let st = undo_store("annotate-edit", "annotation.update", "Write the report");
    let notes = st.json(&["show", "1"])["annotations"].clone();
    assert_eq!(notes.as_array().map(Vec::len), Some(1), "{notes}");
    assert_eq!(notes[0]["body"], "call the printer");
    let echo = st.plain(&["undo"]);
    assert!(echo.contains("undid annotate --edit"), "{echo}");
    assert_eq!(
        st.json(&["show", "1"])["annotations"][0]["body"],
        "call the printr"
    );

    let id = notes[0]["id"].as_str().expect("id");
    let out = st.plain(&["annotate", "1", "--edit", id, "call", "the", "plumber"]);
    assert!(out.contains("note edited"), "{out}");
    assert!(out.contains("call the plumber"), "{out}");
}
