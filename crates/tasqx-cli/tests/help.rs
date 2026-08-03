use std::process::Command;

use tasqx_cli::cmddoc::{RunKind, COMMAND_REF};

/// The binary, kept one-shot and in-process.
///
/// `--no-daemon` is not optional here: `open_backend` prefers a reachable
/// daemon, and the remote path never reads `TASQX_DB`, so on a machine running
/// `tasqx daemon` — the recommended mode — `safe_examples_all_exit_zero` would
/// file its `init`/`add` examples into the developer's real store and then
/// judge that store instead of the scratch one it set up. Global flag, so it
/// rides in front of every subcommand these tests reach.
fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.arg("--no-daemon");
    c
}

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

/// Something listening on a socket address, which is the ENTIRE test
/// `open_backend` applies before it routes a one-shot command over the wire
/// (DESIGN.md §2): `try_connect` connects and returns a `Conn`, with no
/// handshake and no probe of what is on the other end.
///
/// It accepts and hangs up immediately, so a command that routes here dies on
/// `UnexpectedEof` instead of blocking a test run forever. That failure is the
/// point: a fixture that reaches this stub at all is a fixture that would reach
/// the developer's real daemon, and `TASQX_DB` is a local-only concept the
/// remote path never sees.
#[cfg(unix)]
struct StubDaemon {
    addr: std::path::PathBuf,
}

#[cfg(unix)]
impl StubDaemon {
    fn start(tag: &str) -> Self {
        let mut addr = std::env::temp_dir();
        addr.push(format!("tasqx-help-{tag}-{}.sock", std::process::id()));
        // A leftover file from a killed run makes `bind` fail with EADDRINUSE
        // even though nobody is listening.
        let _ = std::fs::remove_file(&addr);
        let listener =
            std::os::unix::net::UnixListener::bind(&addr).expect("bind the stub daemon socket");
        std::thread::spawn(move || while listener.accept().is_ok() {});
        Self { addr }
    }
}

#[cfg(unix)]
impl Drop for StubDaemon {
    /// The socket is a real file; leaving it behind would poison the next run's
    /// `bind` on the same per-process path.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.addr);
    }
}

/// `bin()` must be daemon-proof, or every test in this file is a lottery.
///
/// `safe_examples_all_exit_zero` runs `tasqx init keuken-verbouwen`, two `add`s
/// and an `export` for real. With a daemon on the default socket those writes
/// went to the developer's own store — `open_backend` prefers a reachable
/// daemon, and the remote path never looks at `TASQX_DB` — so the fixture's
/// scratch file stayed empty while live rows appeared in the user's task list.
/// `json_contract.rs` and `required_strings.rs` already pass `--no-daemon` for
/// exactly this reason; this file did not.
///
/// Asserted as an observable outcome rather than by inspecting the argv, so it
/// keeps biting whatever spelling the isolation is eventually written in.
#[cfg(unix)]
#[test]
fn the_fixture_ignores_a_reachable_daemon() {
    let stub = StubDaemon::start("stub");
    let db = fresh_db("stub");

    let out = bin()
        .env("TASQX_DB", &db)
        .env("TASQX_SOCK", &stub.addr)
        .env("TASQX_CONFIG_DIR", "")
        .args(["add", "opdracht van de fixture"])
        .output()
        .expect("run add");

    assert!(
        out.status.success(),
        "`add` must not route to the daemon: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        db.exists(),
        "the task went somewhere other than the fixture's own store at {}",
        db.display()
    );
    let _ = std::fs::remove_file(&db);
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
    //
    // Re-derived from the count this guard reports (39), not incremented: it
    // had been sitting at 27 against a real 35, so eight Safe examples could
    // have been deleted with nothing going red — the exact way a floor stops
    // guarding while still printing green. `tag`/`untag` are `NoRun` (they
    // mutate), so they added nothing; `archive` is `Safe` on purpose and is the
    // 36th — it runs after the `init` example that created its project, and
    // that project is this store's default, so the default-clearing branch of
    // `project.archive` is executed for real on every run of this suite.
    // `agenda`'s three are 37-39, and they run against the store the `add`
    // examples above have already filled, one of which carries `due:friday`.
    assert!(
        examples.len() >= 39,
        "expected the full Safe set, got {}",
        examples.len()
    );

    let mut failures = Vec::new();
    for cmd in examples {
        let args = shell_split(cmd);
        assert_eq!(
            args.first().map(String::as_str),
            Some("tasqx"),
            "`{cmd}` must start with tasqx"
        );
        let out = bin()
            .env("TASQX_DB", &db)
            .args(&args[1..])
            .output()
            .unwrap();
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

/// `config edit` is the first subcommand that wants a terminal. Run from a test,
/// a script or CI its stdout is a pipe, and a TUI that starts anyway writes
/// `\x1b[?1049h` into that pipe and then blocks on a key press that never comes
/// — the command looks hung and the captured output is unreadable.
///
/// This exercises the whole refusal through the real binary: the exit code a
/// script branches on, and a message that names the commands that DO work
/// non-interactively. `Command::output()` gives piped stdout, which is exactly
/// the situation being guarded.
#[test]
fn config_edit_refuses_a_piped_stdout_with_a_nonzero_exit() {
    let out = bin()
        .args(["config", "edit"])
        .output()
        .expect("run config edit");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a refused TUI must exit non-zero (bad_request)"
    );
    assert!(
        out.stdout.is_empty(),
        "nothing may reach a piped stdout: {:?}",
        out.stdout
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains('\x1b'),
        "escape codes leaked into the refusal: {err:?}"
    );
    assert!(err.contains("interactive terminal"), "{err}");
    assert!(
        err.contains("config set"),
        "the refusal must name the way that works: {err}"
    );
}

/// `tasqx pick` is the second subcommand that wants a terminal, and the first
/// whose whole purpose is composition — `tasqx pick | tasqx done` and
/// `$(tasqx pick)` are the shapes a user reaches for, and both are exactly the
/// invocations this refusal covers. So the refusal has to be good: the code a
/// script branches on, no escape bytes, and the names of the commands that
/// answer the same question without a screen.
///
/// `TASQX_DB` points at a file that must NOT exist when the run is over, and
/// that is the second half of the test. D55 says the gate runs before the store
/// is opened; it did not. `open_backend` runs for every command in that arm of
/// `execute`, before the `Command::Pick` match arm is ever reached, so a refused
/// `pick` exited 2 with the right message AND left a fully created, migrated
/// 208 KB SQLite store behind — on a machine that had never run tasqx, at
/// whatever path `TASQX_DB` or the platform data dir named, because
/// `db_path_resolved` creates the parent directory on the way.
///
/// Setting `TASQX_DB` at all is the first half: this test used to set neither it
/// nor `TASQX_CONFIG_DIR`, unlike every other store-touching test in this file,
/// so it opened and migrated the DEVELOPER'S real default store on every `cargo
/// test` run.
#[test]
fn pick_refuses_a_piped_stdout_with_a_nonzero_exit() {
    // A path inside a directory that does not exist, so "the store was opened"
    // and "a directory was created" are both observable afterwards.
    let mut dir = std::env::temp_dir();
    dir.push(format!("tasqx-help-pick-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = dir.join("tasks.db");
    let piped = || {
        let mut c = bin();
        c.env("TASQX_DB", &db).env("TASQX_CONFIG_DIR", "");
        c
    };

    let out = piped().arg("pick").output().expect("run pick");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a refused TUI must exit non-zero (bad_request)"
    );
    assert!(
        out.stdout.is_empty(),
        "nothing may reach a piped stdout: {:?}",
        out.stdout
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains('\x1b'),
        "escape codes leaked into the refusal: {err:?}"
    );
    assert!(err.contains("interactive terminal"), "{err}");
    assert!(
        err.contains("tasqx next") && err.contains("tasqx start"),
        "the refusal must name the commands that DO work in a pipe: {err}"
    );

    // A filter argument must not change the answer: the gate is structural and
    // comes first, so `pick project:work` may not fail on the filter instead —
    // that would report a problem the caller does not have.
    let filtered = piped()
        .args(["pick", "project:work", "+api"])
        .output()
        .expect("run pick with a filter");
    assert_eq!(filtered.status.code(), Some(2));
    assert!(filtered.stdout.is_empty());

    // And its aliases are the same command. Asserted on the MESSAGE, not on
    // the exit code: clap's own "unrecognized subcommand" also exits 2, so a
    // code-only check would pass for an alias that does not exist at all.
    for alias in ["p", "fzf"] {
        let aliased = piped().arg(alias).output().expect("run the alias");
        let err = String::from_utf8_lossy(&aliased.stderr);
        assert!(
            err.contains("interactive terminal"),
            "`tasqx {alias}` did not reach `pick`: {err}"
        );
    }

    // Four refusals later, nothing has been written. Both halves are checked:
    // the file, and the directory `db_path_resolved` would have created to hold
    // it — the second is what a `TASQX_DB` under a fresh `$HOME` would show.
    assert!(
        !db.exists(),
        "a refused `pick` created a store at {}",
        db.display()
    );
    assert!(
        !dir.exists(),
        "a refused `pick` created the store's parent directory at {}",
        dir.display()
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
    let out = bin()
        .args(["manual", "definitely-not-a-topic"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "unknown manual arg must be bad_request"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("definitely-not-a-topic"));
}

/// `-V` must mean the same thing everywhere `-h` does.
///
/// `tasqx list -V` LISTED TASKS: clap declared `version` on the root only, so
/// `-V` was not a flag `list` knew, and the argv pre-pass — correctly, by its
/// own dash grammar — read it as the tag exclusion `-V`. Nothing was wrong at
/// any single layer; the two shortest conventions in the tool simply disagreed,
/// because `-h` is propagated to every subcommand by clap and `-V` was not.
///
/// Driven through the real binary and over EVERY subcommand rather than one,
/// because the pre-pass treats filter-taking commands differently from the rest
/// and a single example would only ever prove one of the two paths. The
/// The expected payload is lifted from `tasqx --version`'s own output — build
/// id included — so this cannot pass by printing some other version-shaped
/// string. Only the program-name prefix differs: clap names a subcommand's
/// version line `tasqx-<sub>`, which is its convention and is left alone.
#[test]
fn the_version_short_flag_works_on_every_subcommand_the_way_help_does() {
    let root = bin().arg("--version").output().expect("run --version");
    let root_out = String::from_utf8_lossy(&root.stdout).into_owned();
    let payload = root_out
        .strip_prefix("tasqx")
        .expect("root --version: {root_out:?}");
    assert!(payload.contains('.'), "no version number in {root_out:?}");

    let names = tasqx_cli::subcommand_names();
    assert!(
        names.len() > 10,
        "the subcommand list came back empty-ish: {names:?}"
    );

    for name in names {
        let out = bin().args([&name, "-V"]).output().expect("run -V");
        let got = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(
            got,
            format!("tasqx-{name}{payload}"),
            "`tasqx {name} -V` did not print the version"
        );
        // The twin that already worked, asserted beside it so a fix that
        // propagates `-V` by breaking `-h` cannot pass.
        let h = bin().args([&name, "-h"]).output().expect("run -h");
        assert!(
            h.status.success(),
            "`tasqx {name} -h` must still print help"
        );
        assert!(
            String::from_utf8_lossy(&h.stdout).contains("Usage:"),
            "`tasqx {name} -h` printed no usage"
        );
    }
}

/// The other half of the dash grammar, which propagating `-V` must not cost:
/// a MULTI-character single-dash token is still a tag exclusion even when its
/// first letter now names a declared short flag. `-h` already proved this for
/// `-hotfix`; `-V` gets the same guard on the real binary rather than on a
/// hand-built argv, since the split is the thing under test.
#[test]
fn a_multi_character_dash_token_beginning_with_v_is_still_a_tag_exclusion() {
    let db = fresh_db("dash-v");
    let run = |args: &[&str]| -> std::process::Output {
        bin()
            .env("TASQX_DB", &db)
            .env("TASQX_CONFIG_DIR", "")
            .args(args)
            .output()
            .expect("run")
    };
    run(&["add", "geverfd", "+Verf"]);
    run(&["add", "gezaagd"]);

    let out = run(&["list", "-Verf"]);
    assert!(
        out.status.success(),
        "`list -Verf` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("gezaagd"),
        "the untagged task must survive the exclusion: {s}"
    );
    assert!(
        !s.contains("geverfd"),
        "`-Verf` must exclude the tagged task, not print a version: {s}"
    );
}
