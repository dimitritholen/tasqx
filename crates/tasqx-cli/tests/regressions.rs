//! End-to-end guards for bugs found by using the tool, not by reading it.
//!
//! The first three reproduced as real sessions: a typo'd theme name that was
//! answered with a theme the user did not ask for, a wrong-typed config value
//! that `config` refused to mention, and a saved theme with nothing pointing at
//! the one command that makes it visible. The file has long since grown past
//! them — argv escaping and quoting, filter spellings, date bounds, table
//! alignment when a title is not ASCII — and anything else found by using the
//! tool belongs here too; this is not a theme-and-config file.
//!
//! They all run the real binary because every one of them is about what reaches
//! a terminal — exit code, stdout, stderr — which is the surface a unit test
//! cannot see. For the argv cases the binary is load-bearing twice over: a
//! hand-built `Vec` of arguments in a unit test would encode the very splitting
//! the test exists to check.

use std::path::PathBuf;
use std::process::Command;

/// A fresh, isolated config directory. Named per test so cargo's parallel
/// threads cannot share one `config.toml` and race on its contents.
fn fresh_config_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("tasqx-reg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create config dir");
    p
}

/// The binary, pointed at this test's own config dir and a scratch store.
///
/// `TASQX_DB` matters as much as the config dir: `config get`/`set`/`list` open
/// a backend, so without it these tests would read — and create — the
/// developer's real task store, and `config list` would report whatever
/// `default_project` happens to be set there. Both are passed per process
/// rather than through `std::env::set_var`, which is process-global and racy
/// under cargo's parallel threads.
///
/// `--no-daemon` is what makes those two env vars mean anything: `open_backend`
/// prefers a reachable daemon and the remote path never reads `TASQX_DB`, so on
/// a machine running `tasqx daemon` these tests drove the developer's real store
/// — writing to it — while asserting against a scratch file nothing had touched.
/// It rides on the fixture rather than on the call sites so a new test cannot
/// forget it; clap declares the flag non-repeatable, so a call site that passes
/// it too is a hard `cannot be used multiple times` error rather than a silent
/// duplicate.
fn bin(tag: &str, dir: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir)
        .env("TASQX_DB", db_path(tag))
        .arg("--no-daemon");
    c
}

/// The store `bin` hands the process for one tag. Named so a test can assert
/// against the same file the binary was pointed at, rather than re-deriving the
/// path formula and drifting from it.
fn db_path(tag: &str) -> PathBuf {
    let mut db = std::env::temp_dir();
    db.push(format!("tasqx-reg-{tag}-{}.db", std::process::id()));
    db
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
    addr: PathBuf,
}

#[cfg(unix)]
impl StubDaemon {
    fn start(tag: &str) -> Self {
        let mut addr = std::env::temp_dir();
        addr.push(format!("tasqx-reg-{tag}-{}.sock", std::process::id()));
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

/// `bin()` must be daemon-proof, or most of this file is a lottery.
///
/// The config dir and `TASQX_DB` above only isolate the IN-PROCESS engine.
/// `open_backend` prefers a reachable daemon, and the remote path never looks
/// at `TASQX_DB` — so with a daemon on the default socket (the tool's own
/// recommended mode) every `init`, `add` and `import` in this file wrote into
/// the developer's live store, and the assertions afterwards judged that store
/// instead of the fixture's. `json_contract.rs` and `required_strings.rs`
/// already pass `--no-daemon` for exactly this reason; this file did not.
///
/// Asserted as an observable outcome rather than by inspecting the argv, so it
/// keeps biting whatever spelling the isolation is eventually written in.
#[cfg(unix)]
#[test]
fn the_fixture_ignores_a_reachable_daemon() {
    let dir = fresh_config_dir("stub-daemon");
    let db = db_path("stub-daemon");
    let _ = std::fs::remove_file(&db);
    let stub = StubDaemon::start("stub-daemon");

    let out = bin("stub-daemon", &dir)
        .env("TASQX_SOCK", &stub.addr)
        .args(["init", "Kelder"])
        .output()
        .expect("run init");

    assert!(
        out.status.success(),
        "`init` must not route to the daemon: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        db.exists(),
        "the project went somewhere other than the fixture's own store at {}",
        db.display()
    );
}

/// `tasqx theme show <unknown>` printed the DEFAULT theme and exited 0.
///
/// `theme::load` falls back to the default for an unknown name, which is right
/// on the render path — a bad theme must never fail a task capture — but wrong
/// for the one command whose entire job is showing you the theme you named. A
/// user who typed `gruvbux` was shown nord, told nothing, and had no way to
/// tell the difference from a theme that simply looks like nord.
#[test]
fn theme_show_rejects_an_unknown_name() {
    let dir = fresh_config_dir("show-unknown");
    let out = bin("show-unknown", &dir)
        .args(["theme", "show", "geen-thema-xyz"])
        .output()
        .expect("run theme show");

    assert_eq!(
        out.status.code(),
        Some(2),
        "an unknown theme must be bad_request, like `theme set`"
    );
    assert!(
        out.stdout.is_empty(),
        "a rejected name must not print a theme the user did not ask for: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("geen-thema-xyz"),
        "the message must name the typo: {err}"
    );
    assert!(
        err.contains("tasqx theme list"),
        "and the way to find the real names: {err}"
    );
}

/// The valid case must keep working — a guard that rejects everything would
/// pass the assertions above while breaking the command outright.
#[test]
fn theme_show_still_renders_a_known_name() {
    let dir = fresh_config_dir("show-known");
    let out = bin("show-known", &dir)
        .args(["theme", "show", "gruvbox"])
        .output()
        .expect("run theme show");

    assert!(out.status.success(), "a real theme must still render");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("Theme: gruvbox"), "{s}");
    assert!(
        s.contains("urgency.ramp"),
        "the full role list must still print: {s}"
    );
}

/// `theme show` with no argument shows the ACTIVE theme and must not be
/// validated into failing. It takes a different branch, and a validator hoisted
/// above the match would reject the empty name and break the command's most
/// common form.
#[test]
fn theme_show_without_a_name_still_works() {
    let dir = fresh_config_dir("show-bare");
    let out = bin("show-bare", &dir)
        .args(["theme", "show"])
        .output()
        .expect("run theme show");

    assert!(out.status.success(), "the bare form must keep working");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Theme: nord"));
}

/// A value of the wrong TYPE in `config.toml` was silently ignored.
///
/// D25 made `config` loud about a MALFORMED file, but `name = 42` parses fine —
/// it is only the declared kind that disagrees — so `config get theme.name`
/// answered `nord` with no word about the line the user had just edited. That
/// is the same confusion D25 set out to kill, one layer down: the user is told
/// they never set the value while the file plainly says they did.
///
/// A warning, not an error: the file parses and every other key in it is still
/// usable, so failing the command would break `config list` — the command you
/// run to diagnose exactly this — over one bad line, and would take a
/// scriptable stdout with it.
#[test]
fn config_get_warns_about_a_wrong_typed_value() {
    let dir = fresh_config_dir("wrong-type");
    std::fs::write(dir.join("config.toml"), "[theme]\nname = 42\n").unwrap();

    let out = bin("wrong-type", &dir)
        .args(["config", "get", "theme.name"])
        .output()
        .expect("run config get");

    assert!(
        out.status.success(),
        "a wrong-typed value must not fail the command"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "nord",
        "stdout stays the usable default, so `$(tasqx config get ...)` still works"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("theme.name"),
        "the warning must name the key: {err}"
    );
    assert!(err.contains("string"), "and the declared type: {err}");
    assert!(
        err.contains("integer"),
        "and what was actually found: {err}"
    );
}

/// `config list` must survive the same file. Reporting the mismatch as an error
/// would abort the one command that shows every key at once — the command you
/// reach for to find which line is wrong.
#[test]
fn config_list_still_reports_every_setting_despite_a_bad_line() {
    let dir = fresh_config_dir("wrong-type-list");
    std::fs::write(dir.join("config.toml"), "[theme]\nname = 42\n").unwrap();

    let out = bin("wrong-type-list", &dir)
        .args(["config", "list"])
        .output()
        .expect("run config list");

    assert!(
        out.status.success(),
        "one bad line must not abort the whole listing"
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("theme.name"), "{s}");
    assert!(
        s.contains("notify.enabled"),
        "the other keys are still readable: {s}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("expected string"),
        "and still warned about"
    );
}

/// The same value read by every OTHER command must stay silent.
///
/// The fallback in `toml_value_in` is on the path of every task capture; a
/// warning there would put this text in front of someone typing `tasqx add`,
/// which is precisely the noise the silent path exists to avoid.
#[test]
fn a_wrong_typed_value_stays_silent_outside_config() {
    let dir = fresh_config_dir("wrong-type-quiet");
    std::fs::write(dir.join("config.toml"), "[theme]\nname = 42\n").unwrap();

    let out = bin("wrong-type-quiet", &dir)
        .args(["theme", "list"])
        .output()
        .expect("run theme list");

    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("theme.name"),
        "the render path must not warn; only `tasqx config` asks about the file: {err}"
    );
}

/// A well-typed value must not be warned about — a mismatch check that fired on
/// every read would pass the assertions above and make `config` unusable.
#[test]
fn config_get_is_silent_about_a_well_typed_value() {
    let dir = fresh_config_dir("right-type");
    std::fs::write(dir.join("config.toml"), "[theme]\nname = \"gruvbox\"\n").unwrap();

    let out = bin("right-type", &dir)
        .args(["config", "get", "theme.name"])
        .output()
        .expect("run config get");

    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "gruvbox");
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "a value of the declared type is not a problem and must print nothing"
    );
}

/// Saving a theme said nothing about where to see it.
///
/// The user picked gruvbox in `config edit`, it wrote correctly, and they came
/// back asking where they were supposed to notice. tasqx's normal output
/// carries only a few coloured accents, so `tasqx list` barely changes; the
/// command that DOES show the difference is `tasqx theme show`, and nothing
/// pointed at it. Asserted on both non-interactive write paths — `config edit`
/// is a TUI and is covered by a unit test on the same shared helper — because a
/// pointer on one path and not the others is how these three drifted before.
#[test]
fn saving_a_theme_points_at_theme_show() {
    let dir = fresh_config_dir("pointer");

    let set = bin("pointer", &dir)
        .args(["theme", "set", "gruvbox"])
        .output()
        .expect("run theme set");
    assert!(set.status.success());
    let s = String::from_utf8_lossy(&set.stdout);
    assert!(
        s.contains("theme.name = gruvbox"),
        "the confirmation must still name the write: {s}"
    );
    assert!(
        s.contains("tasqx theme show"),
        "`theme set` must point at the preview: {s}"
    );

    let cfg = bin("pointer", &dir)
        .args(["config", "set", "theme.name", "dracula"])
        .output()
        .expect("run config set");
    assert!(cfg.status.success());
    let c = String::from_utf8_lossy(&cfg.stdout);
    assert!(c.contains("theme.name = dracula"), "{c}");
    assert!(
        c.contains("tasqx theme show"),
        "`config set theme.name` must point at it too: {c}"
    );
}

/// The pointer is for themes only. A non-theme setting that grew a "see it with
/// `tasqx theme show`" line would be actively wrong, and a helper that appended
/// unconditionally would satisfy the test above.
#[test]
fn saving_a_non_theme_setting_says_nothing_about_theme_show() {
    let dir = fresh_config_dir("pointer-other");
    let out = bin("pointer-other", &dir)
        .args(["config", "set", "notify.enabled", "true"])
        .output()
        .expect("run config set");

    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("notify.enabled = true"), "{s}");
    assert!(
        !s.contains("theme show"),
        "notify.enabled has nothing to do with themes: {s}"
    );
}

/// `--json` consumers must not receive the pointer. It is guidance for a human
/// reading a terminal; a script parsing `{"key":..., "value":...}` would choke
/// on a trailing line of prose.
#[test]
fn the_pointer_stays_out_of_json_output() {
    let dir = fresh_config_dir("pointer-json");
    let out = bin("pointer-json", &dir)
        .args(["--json", "config", "set", "theme.name", "gruvbox"])
        .output()
        .expect("run config set --json");

    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
    assert_eq!(v["value"], "gruvbox");
    assert!(!s.contains("theme show"), "the hint is human-only: {s}");
}

/// `tasqx --version` printed `0.1.0` from a binary six bug-fixes behind HEAD.
///
/// Found by using the tool: an installed `~/.cargo/bin/tasqx` was two commits
/// stale, and `--version` reported the same string as a build from HEAD because
/// nothing bumps `CARGO_PKG_VERSION` between releases. Only the file's mtime
/// gave it away. `build.rs` now stamps the commit in, so the question is
/// answerable from the binary alone.
///
/// The assertion compares against the real repository rather than against
/// another constant in this crate — a version string checked against a version
/// string would prove only that two copies agree. The `-dirty` suffix is
/// deliberately NOT asserted: cargo re-runs `build.rs` when `.git` moves, not
/// when an unstaged edit appears, so the suffix can legitimately lag. The
/// commit id cannot, and it is the half that answers the staleness question.
#[test]
fn version_names_the_commit_it_was_built_from() {
    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .arg("--version")
        .output()
        .expect("run --version");
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);

    assert!(
        s.contains(env!("CARGO_PKG_VERSION")),
        "the crate version must survive: {s}"
    );
    assert!(
        s.trim() != format!("tasqx {}", env!("CARGO_PKG_VERSION")),
        "a bare crate version is the bug: {s}"
    );

    match Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
    {
        Ok(g) if g.status.success() => {
            let sha = String::from_utf8_lossy(&g.stdout).trim().to_string();
            assert!(
                s.contains(&sha),
                "must name the actual HEAD commit {sha}: {s}"
            );
        }
        // No git, no checkout: `unknown` is the honest answer and the build
        // must still have produced a binary rather than failing outright.
        _ => assert!(
            s.contains("unknown"),
            "without git the id must be `unknown`: {s}"
        ),
    }
}

/// `tasqx import` on a wrong-shaped JSON file said `Imported 0 task(s)` and
/// exited 0.
///
/// The verb accepts two shapes — a bare array (what `export` writes) and
/// `{"tasks":[...]}` — and everything else fell through a `_ =>` arm that
/// substituted an empty array. The engine then imported nothing, successfully.
/// So the one command whose whole job is moving a store between machines
/// answered a truncated, renamed, or half-written document with success, and a
/// restore script had no signal at all that the data never arrived.
///
/// Not-JSON and an empty file already reported `invalid JSON` at exit 2; they
/// are pinned here too because the fix rewrites the arm right next to them.
#[test]
fn import_refuses_a_wrong_shaped_document() {
    let dir = fresh_config_dir("import-shape");
    let mut scratch = std::env::temp_dir();
    scratch.push(format!("tasqx-import-shape-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("create scratch dir");

    // (contents, the substring the message must name)
    let cases = [
        (r#"{"nope":1}"#, "tasks"),
        (r#""just a string""#, "string"),
        ("42", "number"),
        ("", "invalid JSON"),
        ("not json", "invalid JSON"),
    ];
    for (i, (body, needle)) in cases.iter().enumerate() {
        let path = scratch.join(format!("case{i}.json"));
        std::fs::write(&path, body).expect("write case file");
        let out = bin("import-shape", &dir)
            .arg("import")
            .arg(&path)
            .output()
            .expect("run import");

        assert_eq!(
            out.status.code(),
            Some(2),
            "`{body}` must be a bad_request, not a silent success"
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("Imported"),
            "`{body}` must not report an import: {stdout}"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.starts_with("error [bad_request]: "),
            "must use the shared error format: {err}"
        );
        assert!(
            err.contains(needle),
            "`{body}` must be explained by naming `{needle}`: {err}"
        );
    }
}

/// A task parked behind a future `wait` never came back.
///
/// `tasqx add x wait:2027-01-01` files the task in `backlog`, out of the default
/// view — correct. But moving the wait into the *past* left it in `backlog` and
/// still absent from `list`: the transition DESIGN specifies
/// (`backlog --> pending: wait/schedule reached`) existed nowhere, and `modify`
/// cannot set a status, so nothing the user could type released it. This runs
/// the real binary because "absent from `tasqx list`" is the whole bug.
#[test]
fn a_wait_that_has_passed_brings_the_task_back_into_list() {
    let dir = fresh_config_dir("wait-release");
    let add = bin("wait-release", &dir)
        .args(["add", "waiter", "wait:2999-01-01"])
        .output()
        .expect("run add");
    assert!(
        add.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        String::from_utf8_lossy(&add.stdout).contains("backlog"),
        "a future wait must still park the task in the backlog"
    );

    let listed = bin("wait-release", &dir)
        .arg("list")
        .output()
        .expect("run list");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("No tasks"),
        "while the wait is ahead the task stays out of the default view"
    );

    let modified = bin("wait-release", &dir)
        .args(["modify", "1", "wait:2020-01-01"])
        .output()
        .expect("run modify");
    assert!(
        modified.status.success(),
        "modify failed: {}",
        String::from_utf8_lossy(&modified.stderr)
    );

    let listed = bin("wait-release", &dir)
        .arg("list")
        .output()
        .expect("run list");
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        stdout.contains("waiter"),
        "a passed wait must return the task to `list`: {stdout}"
    );

    let shown = bin("wait-release", &dir)
        .args(["show", "1"])
        .output()
        .expect("run show");
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("pending"),
        "`show` must agree with `list`: {shown}"
    );
}

/// C2 — `+"a tag"` was split at the space on the WRITE path, and the leftover
/// fragment was absorbed by the title.
///
/// `tasqx add "painting job" +"needs paint"` stored the tag `needs` and RENAMED
/// the task to `painting job paint`, exit 0, no warning. `modify` was worse:
/// `modify 1 +"big job"` set the title to `job`, destroying it.
///
/// This is D13's bug one token later — sugar re-splitting an argv element the
/// shell had already delimited — so it is D13's fix, extended to `+tag`. Only
/// the real binary reproduces it: the mangling happens between argv and the
/// parser, so a hand-built `Vec` in a unit test can be written to hide it.
#[test]
fn a_shell_quoted_tag_survives_add_and_modify_whole() {
    let dir = fresh_config_dir("spaced-tag");
    let json = |tag: &str, args: &[&str]| -> String {
        let out = bin(tag, &dir).args(args).output().expect("run tasqx");
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    json("spaced-tag", &["init", "work"]);
    json("spaced-tag", &["add", "painting job", "+needs paint"]);
    let shown = json("spaced-tag", &["--json", "show", "1"]);
    assert!(
        shown.contains(r#""needs paint""#),
        "the whole argv element is the tag; the shell already did the splitting: {shown}"
    );
    assert!(
        shown.contains(r#""title": "painting job""#),
        "the title must not absorb a fragment of the tag: {shown}"
    );

    // `modify` routes `+tag` to a follow-up `tag.add` (D13) through the same
    // parser, so it carried the identical bug — and there it ate the title whole.
    json("spaced-tag", &["modify", "1", "+big job"]);
    let shown = json("spaced-tag", &["--json", "show", "1"]);
    assert!(
        shown.contains(r#""big job""#),
        "modify must keep the tag whole too: {shown}"
    );
    assert!(
        shown.contains(r#""title": "painting job""#),
        "a sugar-only modify must not touch the title: {shown}"
    );

    // The literal-quote form (quotes reaching argv unstripped) must agree with
    // the shell-stripped form — the same equivalence C1 relies on when reading.
    json("spaced-tag", &["add", "second", r#"+"needs paint""#]);
    let shown = json("spaced-tag", &["--json", "show", "2"]);
    assert!(
        shown.contains(r#""needs paint""#),
        "literal quotes must parse the same: {shown}"
    );
    assert!(
        shown.contains(r#""title": "second""#),
        "and leave the title alone: {shown}"
    );
}

/// C3 — `-tag` exclusion, core filter grammar, was never typable from a shell.
///
/// `filter.rs` documents `"-" WORD  # exclude tag`, the guide and the manual
/// both advertise it, and every filter positional was declared without
/// `allow_hyphen_values` — so clap rejected `tasqx list -needs` as an unknown
/// `-n` flag before tasqx saw a single token. A documented feature that has
/// never once run.
///
/// This is the `--due -1d` / `--remind -1h` trap (D13's neighbours, which carry
/// the opt-in with a comment calling it "required, not cosmetic") re-armed on
/// the POSITIONAL side, where it was simply missed.
///
/// The first fix reached for `allow_hyphen_values` and cost a regression;
/// `cli/argv.rs` explains why and what replaced it, and C3r below pins the
/// half this test does not. Only the real binary reproduces any of it: the bug
/// lives in argv parsing, above the point a unit test can reach.
#[test]
fn a_leading_hyphen_tag_exclusion_reaches_the_filter_parser() {
    let dir = fresh_config_dir("hyphen-filter");
    let run = |args: &[&str]| {
        bin("hyphen-filter", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    ok(&["add", "paint the shed", "+needs", "+home"]);
    ok(&["add", "other thing", "+home"]);

    // `list`: the excluded task is gone, the other one stays.
    let listed = ok(&["list", "-needs"]);
    assert!(
        !listed.contains("paint the shed"),
        "-needs must exclude the tagged task: {listed}"
    );
    assert!(
        listed.contains("other thing"),
        "-needs must not exclude everything else: {listed}"
    );

    // Every other filter-taking argument carries the same grammar and so must
    // accept the same token — `report` takes group_by first, `export` and
    // `watch` take a bare filter.
    let exported = ok(&["export", "-needs"]);
    assert!(
        !exported.contains("paint the shed"),
        "export must honour -tag too: {exported}"
    );
    assert!(
        exported.contains("other thing"),
        "export must still return the rest: {exported}"
    );
    assert!(
        run(&["report", "project", "-needs"]).status.success(),
        "report must accept -tag"
    );

    // Making `-tag` typable must not cost the flags: `--json` before the
    // filter still switches the output format instead of reaching the parser
    // as filter text. (After the filter is C3r's job.)
    let json = ok(&["list", "--json", "+home"]);
    assert!(
        json.trim_start().starts_with('{'),
        "--json must stay a flag, not become filter text: {json}"
    );

    // And an unknown flag must still be rejected, not silently treated as a
    // filter token — `--bogus` briefly parsed as "exclude the tag `-bogus`",
    // excluded nothing, and listed EVERY task with exit 0. The message must
    // name what was typed, since that is the whole recovery.
    let bogus = run(&["list", "--bogus"]);
    assert!(
        !bogus.status.success(),
        "an unknown flag must stay an error, not become filter text"
    );
    let err = String::from_utf8_lossy(&bogus.stderr);
    assert!(
        err.contains("--bogus"),
        "the error must name the offending flag: {err}"
    );
    assert!(
        err.contains("-tag"),
        "and point at the shape that does work: {err}"
    );
}

/// C3r — the fix for C3 broke the ordinary case: a flag typed AFTER the filter.
///
/// `allow_hyphen_values` on a multi-value positional does not mean "let a
/// leading hyphen through when it is not a flag" — it means "once this
/// positional starts consuming, every remaining hyphen token is one of its
/// values", and clap does not exempt its own declared flags. So `tasqx list
/// -needs` began working and `tasqx list @working --json` stopped: `--json`
/// reached the filter grammar as text and was rejected as a mistyped flag.
///
/// The C3 guard above missed it because it only ever wrote the flag FIRST
/// (`list --json +home`), the one order the setting does not affect. Both
/// orders are pinned here, on both filter-taking commands that have flags.
///
/// Only the real binary reproduces this. A unit test that hand-builds the
/// argv Vec chooses the split it is testing and so agrees with whatever the
/// code does — the bug IS the split.
#[test]
fn a_flag_after_a_filter_positional_is_still_a_flag() {
    let dir = fresh_config_dir("hyphen-filter-order");
    let run = |args: &[&str]| {
        bin("hyphen-filter-order", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    ok(&["add", "paint the shed", "+needs", "+home"]);
    ok(&["add", "other thing", "+home"]);

    // (b) a real flag is a flag in EVERY position, not only before the filter.
    for args in [
        vec!["list", "--json", "+home"],
        vec!["list", "+home", "--json"],
        vec!["list", "@working", "--json"],
        vec!["list", "-needs", "--json"],
    ] {
        let out = ok(&args);
        assert!(
            out.trim_start().starts_with('{'),
            "{args:?} must emit JSON, not a table: {out}"
        );
    }
    for args in [
        vec!["report", "project", "--html"],
        vec!["report", "project", "+home", "--html"],
    ] {
        let out = ok(&args);
        assert!(
            out.contains("<html"),
            "{args:?} must emit HTML, not a table: {out}"
        );
    }

    // (a) a leading-hyphen FILTER token still reaches the grammar, in every
    // position — including after a flag, and including the quoted form the
    // shell hands over as one token with a space in it.
    let after_flag = ok(&["list", "--json", "-needs"]);
    assert!(
        !after_flag.contains("paint the shed"),
        "-needs after a flag must still exclude: {after_flag}"
    );
    assert!(
        after_flag.contains("other thing"),
        "-needs must not exclude everything: {after_flag}"
    );
    let before_flag = ok(&["list", "-needs", "--json"]);
    assert!(
        !before_flag.contains("paint the shed"),
        "-needs before a flag must still exclude: {before_flag}"
    );
    let mixed = ok(&["list", "+home", "-needs"]);
    assert!(
        !mixed.contains("paint the shed"),
        "-needs beside another token must exclude: {mixed}"
    );
    // The quoted form, as a shell hands it over: one token, inner quotes intact
    // (`tasqx list '-"needs paint"'`). It survives the join-and-retokenize trip.
    ok(&["add", "fence job", "+needs paint"]);
    let quoted = ok(&["list", "-\"needs paint\""]);
    assert!(
        !quoted.contains("fence job"),
        "a quoted tag exclusion must exclude: {quoted}"
    );
    assert!(
        quoted.contains("other thing"),
        "and must not exclude everything: {quoted}"
    );

    // (b) an unknown flag stays an ERROR wherever it appears — never silent
    // filter text that widens the result set, and never a bare clap usage dump:
    // the message names what was typed and the shape that would have worked.
    for args in [
        vec!["list", "--nosuchflag"],
        vec!["list", "+home", "--nosuchflag"],
    ] {
        let out = run(&args);
        assert!(
            !out.status.success(),
            "{args:?}: an unknown flag must stay an error"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--nosuchflag"),
            "{args:?}: name the offending flag: {err}"
        );
        assert!(
            err.contains("-tag"),
            "{args:?}: point at the shape that works: {err}"
        );
    }
}

/// C5 — the READ side joined argv with spaces, so C2's quoting was unreachable.
///
/// C2 taught the WRITE side that the shell's argument boundaries are
/// information: `tasqx add "painted" +"needs paint"` stores the tag whole. The
/// read side then took the same argv, joined it with spaces and handed the flat
/// string to the filter tokenizer, which re-split it at the space the shell had
/// already consumed — so `tasqx list +"needs paint"` failed with `unknown
/// filter token "paint"`. A tag you could create in the natural shell form
/// could not be filtered for in the natural shell form, and the two sides
/// disagreed about what the shell had already decided.
///
/// Both spellings must converge on the same filter, exactly as C2 made the two
/// write-side spellings converge, and an ordinary multi-element filter
/// (`+api status:done`) must still parse as several tokens — the failure mode a
/// blanket "quote every element" fix would introduce.
///
/// All four read commands that take a filter tail are pinned: they each had
/// their own `join(" ")` and would drift apart one at a time otherwise. Only
/// the real binary reproduces any of it — the bug is the argv split, so a unit
/// test that hand-builds the Vec encodes the buggy split as its own input.
#[test]
fn a_shell_quoted_filter_value_reaches_the_parser_whole() {
    let dir = fresh_config_dir("read-quoting");
    let run = |args: &[&str]| -> std::process::Output {
        bin("read-quoting", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    ok(&["init", "Home Renovation"]);
    ok(&["add", "painted", "+needs paint"]);
    ok(&["add", "unrelated", "+api"]);

    // `list`: the literal-quote form is what the tool teaches and must work.
    let s = ok(&["list", r#"+"needs paint""#]);
    assert!(
        s.contains("painted"),
        "the literal form must select the spaced tag: {s}"
    );
    assert!(
        !s.contains("unrelated"),
        "and must not widen to every task: {s}"
    );
    // A value key with a space behaves the same; `Home Renovation` is the
    // project `add` accepted whole one command earlier.
    let s = ok(&["list", r#"project:"Home Renovation""#]);
    assert!(
        s.contains("painted") && s.contains("unrelated"),
        "the literal form names the project: {s}"
    );

    // N1a: the shell-STRIPPED form is no longer guessed back into one value.
    // The re-quoting heuristic that did so could not tell a spaced value from a
    // whole expression passed as one argument, and answered `list "+api or
    // +web"` with a confident `No tasks.`. Refusing is the decision; the hint
    // is what makes refusing good, so both halves are pinned.
    for (form, hint) in [
        ("+needs paint", r#"+"needs paint""#),
        ("project:Home Renovation", r#"project:"Home Renovation""#),
    ] {
        let out = run(&["list", form]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{form:?} must be refused, not answered: {out:?}"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains(hint),
            "{form:?} must teach the literal spelling, got: {err}"
        );
        assert!(err.contains("quote"), "{form:?} must say why: {err}");
    }
    // And the reading the heuristic used to lose now works, which is the point:
    // an element that opens with a prefix and continues into an EXPRESSION was
    // read as one tag literally named `api or +nosuch`, and answered "No tasks."
    let s = ok(&["list", "+api or +nosuch"]);
    assert!(
        s.contains("unrelated") && !s.contains("painted"),
        "an expression in one argv element: {s}"
    );

    // Several argv elements must still be several tokens: this filter matches
    // nothing precisely because both predicates are read and ANDed.
    let s = ok(&["list", "+api", "status:done"]);
    assert!(
        s.contains("No tasks."),
        "a multi-element filter must stay multi-token: {s}"
    );
    let s = ok(&["list", "+api", "status:pending"]);
    assert!(
        s.contains("unrelated") && !s.contains("painted"),
        "and still select: {s}"
    );

    // `export`, `report` and `watch` each carried their own join.
    let s = ok(&["export", r#"+"needs paint""#]);
    assert!(
        s.contains("painted") && !s.contains("unrelated"),
        "export takes a filter too: {s}"
    );
    let s = ok(&["report", "project", r#"+"needs paint""#]);
    assert!(
        s.contains("Home Renovation"),
        "report's tail after group_by is a filter: {s}"
    );
    // `watch` blocks on a daemon, so only its argument handling is reachable:
    // the filter must at least not be rejected as unparseable before connecting.
    let out = run(&["watch", r#"+"needs paint""#, "--json"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("unknown filter token"),
        "watch must not mis-split its filter: {err}"
    );
}

/// C6: `tasqx add "urgent thing" '!urgent'` exited 0 with priority `null` and a
/// title that did not contain the word either — the token evaporated.
///
/// `normalize_prio` returned `None` for anything outside h/m/l while the `!`
/// branch consumed the token unconditionally, so a typo'd priority was neither
/// applied, nor reported, nor even left in the title to notice later. The same
/// value spelled `--priority urgent` was already a `bad_request` (the core
/// validates it per D28), so one field had two answers depending on how it was
/// typed — the D13 rule that a token means one thing, broken across spellings.
///
/// Through the real binary because the exit code and stderr are the contract; a
/// unit test can only pin the parser, not what reaches the terminal.
#[test]
fn an_invalid_priority_sugar_token_is_refused_not_dropped() {
    let dir = fresh_config_dir("bad-prio");
    let run = |args: &[&str]| -> std::process::Output {
        bin("bad-prio", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };

    let out = run(&["add", "urgent thing", "!urgent"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an invalid priority must be bad_request, like --priority"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("urgent"),
        "the error must name the offending value: {err}"
    );
    assert!(
        err.contains("!high") && err.contains("!low"),
        "and list the way out: {err}"
    );

    // Refused at the door means nothing was written — not a task named after
    // the typo, and not one silently missing the priority that was asked for.
    let s = String::from_utf8_lossy(&run(&["list"]).stdout).to_string();
    assert!(
        s.contains("No tasks."),
        "a refused add must store nothing: {s}"
    );

    // `modify` shares the parser, so it must share the answer (D13).
    assert!(run(&["add", "real task"]).status.success());
    let out = run(&["modify", "1", "!urgent"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "modify must refuse the same token"
    );

    // The valid spellings, both short and long, still work on both verbs.
    assert!(run(&["add", "high one", "!h"]).status.success());
    assert!(run(&["modify", "1", "!medium"]).status.success());
    let s = String::from_utf8_lossy(&run(&["list", "--json"]).stdout).to_string();
    assert!(
        s.contains("\"priority\": \"H\""),
        "!h must still set H: {s}"
    );
    assert!(
        s.contains("\"priority\": \"M\""),
        "!medium must still set M: {s}"
    );
}

/// C7: the argv pre-pass sentinel leaked into flag VALUES.
///
/// `prepass` escaped every single-dash token after a filter subcommand, but
/// `unescape` only ever runs on the filter tail — so a single-dash token that
/// clap consumed as a flag's value kept its raw U+0001 and was used, and
/// printed, as-is: `tasqx list --theme -nord` answered
/// `unknown theme "\u{1}nord"` and then rendered the default. Before the
/// pre-pass existed the same line failed cleanly with `unexpected argument`.
///
/// The assertion is deliberately not "this one command errors" but "no output
/// on any path contains the sentinel": the bug is structural, so the guard is
/// the property, not the symptom. Both argument ORDERS and both quoting
/// SPELLINGS (`--theme -nord` and `--theme=-nord`) are exercised, because the
/// two previous bugs in this cluster both hid in the order that was not tested.
/// Through the real binary: the escape decision IS the argv split, and a unit
/// test that builds the Vec itself would encode the buggy split as its input.
#[test]
fn the_argv_sentinel_never_reaches_a_flag_value() {
    let dir = fresh_config_dir("sentinel-leak");
    // TASQX_FORCE_COLOR because the theme is only RESOLVED on the tty path, and
    // a test harness is not a tty: without it `--theme` is never read and the
    // leak this test exists for is invisible.
    let run = |args: &[&str]| {
        bin("sentinel-leak", &dir)
            .env("TASQX_FORCE_COLOR", "1")
            .args(args)
            .output()
            .expect("run tasqx")
    };

    assert!(run(&["add", "paint the shed", "+needs"]).status.success());

    for args in [
        vec!["list", "--theme", "-nord"],
        vec!["list", "-needs", "--theme", "-nord"],
        vec!["list", "--theme", "-nord", "-needs"],
        vec!["list", "--theme=-nord"],
        vec!["list", "--socket", "-x"],
        vec!["export", "--theme", "-nord"],
        vec!["watch", "--socket", "-x"],
        vec!["report", "project", "--out", "-x"],
        vec!["report", "project", "-needs", "--out", "-x"],
    ] {
        let out = run(&args);
        let seen = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // Both spellings of the leak: raw, as a Display message prints it, and
        // `\u{1}` as a Debug-formatted one does. The theme warning quotes the
        // name with `{:?}`, so the raw-byte check alone silently passed it.
        assert!(
            !seen.contains('\u{1}') && !seen.contains("\\u{1}"),
            "{args:?} leaked the argv escape sentinel into a value: {seen:?}"
        );
    }

    // And the value itself must arrive intact, not merely sentinel-free: a
    // leading-dash theme name is a name the resolver should reject BY NAME.
    let out = run(&["list", "--theme=-nord"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("-nord"),
        "the warning must name the value the user typed: {err}"
    );
}

/// C8: the write-side sugar tokenizer and the read-side filter grammar
/// disagreed about the SAME quoting syntax.
///
/// `filter.rs`'s GRAMMAR documents `QUOTED := a run between double quotes;
/// backslash escapes a quote or a backslash`, and the read side implements it.
/// The write side had its own second tokenizer that treated `"` as a pure
/// delimiter with no escapes at all, so:
///   * `add wall '+say"hi'` stored the tag `sayhi`, exit 0 — the quote silently
///     swallowed, and the documented escape unmatchable for tags because no
///     write path could mint a value containing a quote;
///   * `add t 'project:My "Big" Project'` split at the embedded quote and then
///     blamed the user for a project (`My`) that exists and is reachable both
///     via `--project` and via the read-side filter;
///   * `add t 'project:"My \"Big\" Project"'` stored the mangled literal
///     `My \Big\ Project`, because `\` was ordinary text on the write side.
///
/// The fix is ONE rule, not a third tokenizer: the write side now splits with
/// `tasqx_core::filter::split_words`, the same scanner the read side's
/// `tokenize` is built on. This test is the guard that they cannot drift apart
/// again — it pins the ROUND TRIP, creating each value through sugar and then
/// naming that exact value in a filter.
///
/// Through the real binary, and in BOTH argument orders and BOTH quoting
/// spellings: the bug is the argv split, so a unit test that hand-builds the
/// Vec encodes the buggy split as its own input, and the previous fix in this
/// cluster passed against a broken tree by testing only one order.
#[test]
fn one_quoting_rule_spans_the_write_and_read_sides() {
    let dir = fresh_config_dir("one-quoting-rule");
    let run = |args: &[&str]| -> std::process::Output {
        bin("one-quoting-rule", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // Every character the grammar has to survive at once: a space, a double
    // quote, a backslash and a paren — the last three being exactly the ones
    // the read side calls out as special.
    let project = r#"a "b" \c (d)"#;
    // The literal-quote spelling of each, i.e. what `filter::quote` emits.
    let project_lit = r#"project:"a \"b\" \\c (d)""#;
    let tag_lit = r#"+"say\"hi""#;

    ok(&["init", project]);

    // Sugar must be able to NAME the value, in either token order.
    ok(&["add", "before", project_lit, tag_lit]);
    ok(&["add", "after", tag_lit, project_lit]);
    // …and the value must arrive whole, not with its escapes eaten.
    let s = ok(&["list", "--json"]);
    assert_eq!(
        s.matches(r#""a \"b\" \\c (d)""#).count(),
        2,
        "sugar must store the project verbatim, both orders: {s}"
    );
    assert_eq!(
        s.matches(r#""say\"hi""#).count(),
        2,
        "and the tag verbatim: {s}"
    );

    // The round trip closes: the read side names what the write side created.
    let s = ok(&["list", project_lit]);
    assert!(
        s.contains("before") && s.contains("after"),
        "filter must name the project: {s}"
    );
    let s = ok(&["list", tag_lit]);
    assert!(
        s.contains("before") && s.contains("after"),
        "filter must name the tag: {s}"
    );

    // The two spellings converge, as they already do on the read side: a value
    // whose only special character is a space needs no quotes once the shell
    // has drawn the argument boundary.
    // The WRITE side still honours the argv boundary the shell drew — sugar
    // gets one element and has no expression grammar to be ambiguous against —
    // so both spellings create the same project. The READ side is where the
    // ambiguity lives, and since N1a it teaches the literal form rather than
    // guessing (see `a_shell_quoted_filter_value_reaches_the_parser_whole`).
    ok(&["init", "Home Renovation"]);
    ok(&["add", "stripped", "project:Home Renovation"]);
    ok(&["add", "literal", r#"project:"Home Renovation""#]);
    let s = ok(&["list", r#"project:"Home Renovation""#]);
    assert!(
        s.contains("stripped") && s.contains("literal"),
        "both write spellings converge: {s}"
    );

    // A lone quote is now a refusal, not a silent swallow. `+say"hi` opens a
    // quoted run that never closes; guessing where it ended is how the tag
    // `sayhi` — a value the user never typed — used to get stored.
    let out = run(&["add", "wall", r#"+say"hi"#]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an unterminated quote must be a bad_request"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unterminated"), "and must say so: {err}");
    assert!(
        err.contains('\\'),
        "and name the escape that is the way out: {err}"
    );
    let s = ok(&["list", "--json"]);
    assert!(
        !s.contains("sayhi"),
        "nothing may be stored from a refused line: {s}"
    );

    // A whole-element value is NOT truncated — the shell drew that boundary —
    // so an unknown name there stays the plain typo message it should be.
    let out = run(&["add", "t", "project:No Such Project"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "an unknown project is still not_found"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("No Such Project"),
        "the whole name is named: {err}"
    );
    assert!(
        !err.contains("before the first space"),
        "nothing was cut, so say nothing: {err}"
    );

    // But a `project:` token that really WAS cut at a space must not present the
    // fragment as though the user typed it. `project:My "Big" Project` splits
    // into three tokens on both sides — the read side rejects the strays, and
    // the write side used to answer `no project named My (create it with
    // `tasqx init My`)` about a project that existed and was reachable both via
    // `--project` and via the filter. Only the sugar path could not name it.
    ok(&["init", r#"My "Big" Project"#]);
    let out = run(&["add", "t", r#"project:My "Big" Project"#]);
    assert_eq!(out.status.code(), Some(4), "still a not_found");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("before the first space") && err.contains(r#"project:"My"#),
        "the message must own the cut and show the spelling that names a whole one: {err}"
    );

    // And the spelling it advises must actually work.
    ok(&["add", "t2", r#"project:"My \"Big\" Project""#]);
    let s = ok(&["list", r#"project:"My \"Big\" Project""#]);
    assert!(
        s.contains("t2"),
        "the advised spelling must round trip: {s}"
    );
}

/// `tasqx report <filter> --html` IGNORED its filter entirely.
///
/// `run_html_report` never received `args`, and `html::generate` built its own
/// three queries from scratch — so the two output modes of ONE command answered
/// two different questions. A filter naming a project that does not exist
/// produced a byte-identical page to no filter at all, while the terminal path
/// correctly printed nothing. This runs the real binary because the drop
/// happened in the `main` dispatch match, which no unit test on
/// `report_params` can see: the params were right and simply never asked for.
///
/// Both argument orders, because the tail is hyphen-tolerant filter DSL routed
/// through the `argv` pre-pass — `--html` after the filter is the spelling that
/// pre-pass could most easily swallow.
#[test]
fn report_html_honours_its_filter_in_both_argument_orders() {
    let dir = fresh_config_dir("report-html-filter");
    let run = |args: &[&str]| {
        bin("report-html-filter", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // `--no-daemon` is on `bin` now; repeating it here is a clap error, not a
    // harmless duplicate.
    ok(&["init", "Alpha"]);
    ok(&["init", "Beta"]);
    ok(&["add", "alpha work", "project:Alpha"]);
    ok(&["add", "beta work", "project:Beta"]);

    let out_path = |name: &str| dir.join(name).to_string_lossy().to_string();
    let html = |name: &str, args: &[&str]| -> String {
        let p = out_path(name);
        let mut full = vec!["report"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--out", &p]);
        ok(&full);
        std::fs::read_to_string(&p).expect("the report was written")
    };

    let all = html("all.html", &["--html"]);
    assert!(
        all.contains("alpha work") && all.contains("beta work") && all.contains("Beta"),
        "the unfiltered page must still show everything"
    );

    // The empty filter is the sharpest case: a project that does not exist can
    // only produce an empty page, so any Beta/Alpha content proves the filter
    // was dropped rather than merely mis-scoped.
    for (name, args) in [
        ("none-a.html", vec!["project:Nonexistent", "--html"]),
        ("none-b.html", vec!["--html", "project:Nonexistent"]),
    ] {
        let page = html(name, &args);
        // Compared as a bool, not `assert_ne!`: both sides are whole HTML
        // documents and the failure message would be two screenfuls of CSS.
        assert!(
            page != all,
            "{args:?}: the filtered page is byte-identical to the unfiltered one"
        );
        for leak in ["alpha work", "beta work"] {
            assert!(
                !page.contains(leak),
                "{args:?}: {leak:?} survived a filter that matches nothing"
            );
        }
    }

    // And a filter that DOES match must scope rather than empty the page — a fix
    // that simply dropped all data would pass every assertion above.
    for args in [
        vec!["project:Alpha", "--html"],
        vec!["--html", "project:Alpha"],
    ] {
        let page = html("alpha.html", &args);
        assert!(
            page.contains("alpha work"),
            "{args:?}: the matching task vanished"
        );
        assert!(
            !page.contains("beta work"),
            "{args:?}: an out-of-scope task survived"
        );
    }
}

/// The CLI's `report` asked for a hard-typed list of metrics standing right
/// next to `engine::SUMMARY_METRICS`, the constant whose entire job is to stop
/// exactly that. Adding a fifth metric to the engine would have left `tasqx
/// report` silently requesting four — the API and the MCP schema would offer it
/// and the CLI would never show it.
///
/// Deliberately NOT `assert_eq!(report_params()["metrics"], SUMMARY_METRICS)`:
/// once the CLI derives its list from the constant, both sides of that
/// comparison come from one place and it proves nothing. This drives the real
/// binary end to end and asserts the ENGINE ANSWERED with every metric the
/// constant publishes — so the CLI's request and the engine's output have to
/// agree, and neither is the test's own input.
#[test]
fn report_requests_every_metric_the_engine_publishes() {
    let dir = fresh_config_dir("report-metrics");
    // A task with an estimate and a closed timer, so no metric is structurally
    // absent for want of data to aggregate.
    let mk = |args: &[&str]| {
        let out = bin("report-metrics", &dir)
            .args(args)
            .output()
            .expect("run tasqx");
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    mk(&["add", "measured", "--estimate", "2h"]);
    mk(&["start", "1"]);
    mk(&["stop", "1"]);

    let out = mk(&["--json", "report"]);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("report --json must emit JSON");
    let groups = v["groups"].as_array().expect("report must have groups");
    assert!(
        !groups.is_empty(),
        "the fixture task produced no group: {v}"
    );

    for m in tasqx_core::engine::SUMMARY_METRICS {
        for g in groups {
            assert!(
                g.get(m).is_some(),
                "`tasqx report` did not ask for the published metric {m:?}; group was {g}"
            );
        }
    }

    // The default grouping is the constant's first entry, not a second copy of
    // the word "project" typed into the CLI.
    let axis = tasqx_core::engine::SUMMARY_GROUP_BY[0];
    for g in groups {
        assert!(
            g.get(axis).is_some(),
            "report defaulted to an axis other than {axis:?}: {g}"
        );
    }
}

/// `tasqx list --sort` does not exist, so the silent drop this guards reaches
/// users through the JSON API (`tasqx api`), the daemon, and MCP — every
/// machine-facing surface. An unknown key came back `ok: true` with rows in an
/// order nobody asked for.
#[test]
fn the_api_refuses_an_unknown_sort_key() {
    use std::io::Write;
    let dir = fresh_config_dir("sort-api");
    let mut child = bin("sort-api", &dir)
        .arg("api")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx api");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"tasqx":"1","id":"s","method":"task.list","params":{"sort":["bogus"]}}"#)
        .expect("write envelope");
    let out = child.wait_with_output().expect("wait");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON response");

    assert_eq!(v["ok"], false, "an unknown sort key came back ok: {v}");
    assert_eq!(v["error"]["code"], "bad_request", "{v}");
    let msg = v["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("bogus"),
        "the error must name the offending key: {msg}"
    );
    assert!(
        msg.contains(tasqx_core::engine::SORT_KEYS[0]),
        "the error must list the valid keys: {msg}"
    );
}

/// The truncation hedge fired when truncation was IMPOSSIBLE.
///
/// `tasqx add task project:Zzz` has nothing after the `project:` token, so the
/// tokenizer cut nothing off — `Zzz` is exactly what the user typed. Answering
/// it with "that is only the part before the first space, so a name with spaces
/// must be quoted" describes a cut that did not happen, and sends the user
/// hunting for a longer project name that never existed. The hedge is right
/// only when a following word could have been swallowed.
///
/// Driven through the real binary and in BOTH argument orders, because the
/// distinguishing fact is a token's POSITION in argv, which is precisely what a
/// hand-built Vec in a unit test would encode rather than test.
#[test]
fn the_truncation_hedge_only_fires_when_a_word_could_have_been_cut() {
    let dir = fresh_config_dir("cut-hedge");
    let hedge = "must be quoted";

    // Nothing follows the token: no cut was possible, so no hedge.
    let out = bin("cut-hedge", &dir)
        .args(["add", "task", "project:Zzz"])
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains("no project named"),
        "expected the not-found error, got: {err}"
    );
    assert!(
        !err.contains(hedge),
        "nothing followed `project:Zzz`, so nothing could have been cut: {err}"
    );
    assert!(
        err.contains("tasqx init"),
        "the create-it advice is the whole point of the message: {err}"
    );

    // A word DOES follow: the tokenizer may well have eaten it, so hedge.
    let out = bin("cut-hedge", &dir)
        .args(["add", "project:Zzz", "more"])
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains(hedge),
        "`more` could have been the rest of the name, so the hedge must stay: {err}"
    );

    // Same shape, other order: the trailing word is the title, not a fragment
    // candidate only when it precedes. `project:` first must still hedge.
    let out = bin("cut-hedge", &dir)
        .args(["add", "project:Zzz", "big", "job"])
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.contains(hedge),
        "two following words, still a possible cut: {err}"
    );

    // Quoted whole: no hedge regardless of what follows.
    let out = bin("cut-hedge", &dir)
        .args(["add", "project:\"Zzz\"", "more"])
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !err.contains(hedge),
        "a quoted name is whole by construction: {err}"
    );
}

/// The same refusal, through the real binary and the JSON envelope.
///
/// `fields` has no CLI flag — the API is the only way to send it, and the API
/// is exactly the surface a script binds to. An unknown key came back
/// `ok: true` with the column missing, so a typo and an empty value were
/// indistinguishable to the only consumer that cannot squint at the output.
#[test]
fn the_api_refuses_an_unknown_fields_key() {
    use std::io::Write;
    let dir = fresh_config_dir("fields-api");
    let mut child = bin("fields-api", &dir)
        .arg("api")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn tasqx api");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(
            br#"{"tasqx":"1","id":"f","method":"task.list","params":{"fields":["short_id","titel"]}}"#,
        )
        .expect("write envelope");
    let out = child.wait_with_output().expect("wait");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON response");

    assert_eq!(v["ok"], false, "an unknown field came back ok: {v}");
    assert_eq!(v["error"]["code"], "bad_request", "{v}");
    let msg = v["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("titel"),
        "the error must name the offending key: {msg}"
    );
    assert!(
        msg.contains("title"),
        "the error must list the valid fields: {msg}"
    );
}

/// J1 — `due.before:`/`due.after:` took ONLY strict RFC3339, so five of the six
/// date spellings tasqx's own error message advertises matched zero rows.
///
/// `tasqx add "x" due:tomorrow` followed by `tasqx list due.before:friday`
/// printed `No tasks.` at exit 0 with the task sitting in the store — the
/// primary query of a task manager returning a wrong answer indistinguishable
/// from a right one. The one spelling that worked
/// (`2030-01-01T00:00:00Z`) is the one a human is least likely to type.
///
/// Only the real binary proves the whole path: the spelling has to survive
/// argv, sugar, the filter tokenizer and the engine before it reaches a date
/// parser, and a hand-built filter string in a unit test can be written to skip
/// three of those four.
#[test]
fn a_due_bound_takes_the_dates_the_tool_advertises_and_refuses_the_rest() {
    let dir = fresh_config_dir("due-bound");
    let add = bin("due-bound", &dir)
        .args(["add", "ship it", "due:tomorrow"])
        .output()
        .expect("run add");
    assert!(
        add.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    // A task due tomorrow is NOT inside every advertised bound on every
    // calendar day: on the last day of a month, tomorrow is next month and
    // `due.before:eom` correctly matches nothing (observed 2026-07-31). Seed a
    // second task whose due — the first of the current month, in the UTC
    // calendar the date grammar resolves against — is strictly before `eom`,
    // `"in 2 weeks"`, and every fixed future spelling, whatever day the suite
    // runs, so a bound failing to FIND it can only mean the bound was
    // mis-parsed, never that the calendar disagreed.
    let first_of_month = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .first_of_month()
        .to_string();
    let add = bin("due-bound", &dir)
        .args(["add", "close the books", &format!("due:{first_of_month}")])
        .output()
        .expect("run add");
    assert!(
        add.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    // Every spelling the date-error message recommends, in the widest form so
    // the answer cannot depend on which day the suite runs.
    // A spelling containing a space takes the literal-quote form, which is what
    // the tool teaches everywhere since N1a removed the argv re-quoting guess.
    for bound in [
        r#""in 2 weeks""#,
        "eom",
        "2099-12-31",
        "2099-12-31T17:00",
        "+1y",
    ] {
        let out = bin("due-bound", &dir)
            .args(["list", &format!("due.before:{bound}")])
            .output()
            .expect("run list");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "`due.before:{bound}` must be accepted: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("close the books"),
            "`due.before:{bound}` must find a task due on the first of this month, got: {stdout}"
        );
    }

    // And the other half: an unreadable bound is refused by name rather than
    // answered with the same empty list a genuine no-match produces.
    let out = bin("due-bound", &dir)
        .args(["list", "due.before:tomorow"])
        .output()
        .expect("run list");
    assert!(
        !out.status.success(),
        "a misspelled date bound must not exit 0"
    );
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        msg.contains("tomorow"),
        "the error must name the offending value: {msg}"
    );
    assert!(
        !msg.contains("No tasks"),
        "a typo must never be answered with an empty result set: {msg}"
    );
}

/// N1c — THE INVARIANT THE WHOLE CLUSTER WAS MISSING: three spellings of one
/// filter must select the same rows.
///
/// Every regression in this area came from a test that covered one spelling and
/// not its sibling. `from_argv` was fixed against argv given as separate words
/// and broke the single quoted argument; the argv pre-pass was fixed against
/// filter tokens and broke `-h`; the two shell spellings of a spaced value were
/// asserted against each other in a unit test that built the split itself. One
/// example per behaviour is not a guard, so this is a corpus crossed with every
/// spelling, and the crossing is the test:
///
///   (i)   the filter as ONE argv element   — `tasqx list "(+api or +web)"`
///   (ii)  the same as SEVERAL argv words   — `tasqx list ( +api or +web )`
///   (iii) the same string to `task.list`   — the JSON API, no clap in front
///
/// (i) and (iii) send byte-identical filter strings and so pin the CLI plumbing
/// between them: `from_argv`, the dash pre-pass and `unescape`. (ii) pins the
/// join. All three run the REAL BINARY, because every bug here lived in argv
/// handling and a unit test that hand-builds the token list encodes the buggy
/// split as its own input.
///
/// Cases that are expected to FAIL are asserted as failures rather than left
/// out — the shell-stripped `project:Home Renovation` is refused since N1a, and
/// omitting it would let a future re-introduction of the guessing heuristic
/// pass this test.
#[test]
fn one_filter_selects_one_set_of_rows_in_every_spelling() {
    use std::io::Write;
    let dir = fresh_config_dir("spelling-invariant");
    let run = |args: &[&str]| -> std::process::Output {
        bin("spelling-invariant", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    let ok = |args: &[&str]| {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    // The store. Titles are the identity this test compares on, so they are
    // distinct words that cannot appear in each other.
    ok(&["init", "Home Renovation"]);
    ok(&["init", "Work"]);
    ok(&["add", "alpha", "--project", "Work", "+api", "due:tomorrow"]);
    ok(&["add", "bravo", "--project", "Work", "+api", "+web"]);
    ok(&["add", "charlie", "--project", "Work", "+web"]);
    ok(&[
        "add",
        "delta",
        "--project",
        "Home Renovation",
        r#"+"needs paint""#,
    ]);
    ok(&["add", "echo", "--project", "Work", "+api"]);
    ok(&["done", "5"]);

    // Titles of the rows a `list --json` payload selected, sorted so the
    // comparison is about membership and not about ordering — which is a
    // separate contract with its own tests.
    let titles = |stdout: &[u8]| -> Vec<String> {
        let v: serde_json::Value =
            serde_json::from_slice(stdout).expect("list --json emits one object");
        let mut t: Vec<String> = v["tasks"]
            .as_array()
            .expect("a tasks array")
            .iter()
            .map(|r| r["title"].as_str().expect("a title").to_string())
            .collect();
        t.sort();
        t
    };

    // (iii) the same filter string with no clap in front of it at all.
    let via_api = |filter: &str| -> serde_json::Value {
        let mut child = bin("spelling-invariant", &dir)
            .arg("api")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn tasqx api");
        let env = serde_json::json!({
            "tasqx": "1", "id": "n1c", "method": "task.list",
            "params": {"filter": filter},
        });
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(env.to_string().as_bytes())
            .expect("write envelope");
        let out = child.wait_with_output().expect("wait");
        serde_json::from_slice(&out.stdout).expect("one JSON response")
    };

    // Each case is the MULTI-WORD spelling; the single-argument spelling is its
    // join. A word may itself carry literal quotes — that is the spelling the
    // tool teaches for a value containing a space, and it is the one thing the
    // shell hands through unchanged.
    let corpus: [&[&str]; 9] = [
        &["+api"],                         // a bare tag
        &["+api", "+web"],                 // two tags, implicit AND
        &["(", "+api", "or", "+web", ")"], // grouping with parens and `or`
        &["(+api", "or", "+web)"],         // parens glued to their operands
        &["status:done"],                  // a status predicate
        &["due.before:+1y"],               // a date bound (widest, so the day cannot matter)
        &[r#"project:"Home Renovation""#], // a project name containing a space
        &[r#"+"needs paint""#],            // a tag containing a space
        &["-api"],                         // an exclusion, the one-dash grammar
    ];

    for words in corpus {
        let joined = words.join(" ");

        // (ii) several bare argv words.
        let mut argv = vec!["list", "--json"];
        argv.extend_from_slice(words);
        let many = run(&argv);
        assert!(
            many.status.success(),
            "{words:?} as words failed: {}",
            String::from_utf8_lossy(&many.stderr)
        );
        let many = titles(&many.stdout);

        // (i) one argv element.
        let one = run(&["list", "--json", &joined]);
        assert!(
            one.status.success(),
            "{joined:?} as one arg failed: {}",
            String::from_utf8_lossy(&one.stderr)
        );
        let one = titles(&one.stdout);

        // (iii) the JSON API.
        let api = via_api(&joined);
        assert_eq!(
            api["ok"], true,
            "{joined:?} must be accepted by the API too: {api}"
        );
        let mut api: Vec<String> = api["result"]["tasks"]
            .as_array()
            .expect("a tasks array")
            .iter()
            .map(|r| r["title"].as_str().expect("a title").to_string())
            .collect();
        api.sort();

        assert_eq!(
            one, many,
            "{words:?}: one argv element and several must select the same rows"
        );
        assert_eq!(
            one, api,
            "{joined:?}: the CLI and the API must select the same rows"
        );
        // A filter that selects nothing everywhere would satisfy the three
        // equalities while proving nothing, which is how a broken tokenizer
        // could hide here.
        assert!(
            !one.is_empty(),
            "{words:?} must select at least one row, got nothing"
        );
    }

    // The case that must FAIL, in every spelling, for the same reason: N1a
    // deleted the heuristic that guessed `project:Home` + `Renovation` back
    // into one value, because the guess was also a valid reading of a whole
    // expression and it answered the wrong one silently.
    let stripped: [&[&str]; 2] = [&["project:Home", "Renovation"], &["+needs", "paint"]];
    for words in stripped {
        let joined = words.join(" ");
        let mut argv = vec!["list"];
        argv.extend_from_slice(words);
        for out in [run(&argv), run(&["list", &joined])] {
            assert_eq!(
                out.status.code(),
                Some(2),
                "{joined:?} must be refused, not answered"
            );
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                err.contains("did you mean"),
                "{joined:?} must teach the fix: {err}"
            );
        }
        let api = via_api(&joined);
        assert_eq!(
            api["ok"], false,
            "{joined:?} must be refused on the API too: {api}"
        );
        let msg = api["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("did you mean"),
            "{joined:?}: the API gets the same hint: {msg}"
        );
    }

    // No output on any of those paths may carry the pre-pass sentinel, which is
    // the failure mode the dash escape has already leaked three times.
    let out = run(&["list", "--json", "-api"]);
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains('\u{1}'),
        "sentinel leaked to stdout"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains('\u{1}'),
        "sentinel leaked to stderr"
    );
}

/// P1a — `cancel` and `done` must AGREE about the dependents they released.
///
/// D11 makes cancelling a blocker release its dependents precisely so the graph
/// stays honest, and the human surface said nothing: `cancel` printed
/// `#1 -> cancelled` while `--json cancel 1` on the same fixture returned
/// `"unblocked":[2]`. The tool computed the cascade, stored it, answered the API
/// with it, and dropped it on the only surface a person reads — the invisible
/// field, again.
///
/// `done` and `cancel` are asserted TOGETHER because they return the same
/// cascade from the same helper (`compute_unblocked`). One of them rendering it
/// is not the property worth guarding; both of them rendering it the same way
/// is, since a reader who learns "now actionable" from `done` will read its
/// absence under `cancel` as "nothing was released".
#[test]
fn both_done_and_cancel_name_the_dependents_they_released() {
    for verb in ["done", "cancel"] {
        let tag = format!("unblocked-{verb}");
        let dir = fresh_config_dir(&tag);
        let run = |args: &[&str]| bin(&tag, &dir).args(args).output().expect("run tasqx");

        assert!(run(&["init", "P"]).status.success(), "init");
        assert!(run(&["add", "Blocker"]).status.success(), "add blocker");
        assert!(run(&["add", "Dependent"]).status.success(), "add dependent");
        assert!(run(&["dep", "2", "1"]).status.success(), "dep");

        let out = run(&[verb, "1"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "`{verb} 1` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("#2"),
            "`{verb}` released #2 — the API says so — and must name it: {stdout}"
        );
        assert!(
            stdout.contains("now actionable"),
            "`{verb}` must label the release the same way its twin does: {stdout}"
        );
    }
}

/// P1a, the other half: a verb that released NOTHING must not claim it did.
///
/// A guard that only checks the list appears passes just as well against a
/// renderer that prints "now actionable:" unconditionally, which would be a
/// worse bug than the silence it replaced.
#[test]
fn neither_verb_announces_a_release_that_did_not_happen() {
    for verb in ["done", "cancel"] {
        let tag = format!("norelease-{verb}");
        let dir = fresh_config_dir(&tag);
        let run = |args: &[&str]| bin(&tag, &dir).args(args).output().expect("run tasqx");

        assert!(run(&["init", "P"]).status.success(), "init");
        assert!(run(&["add", "Lonely"]).status.success(), "add");

        let out = run(&[verb, "1"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "`{verb} 1` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("now actionable"),
            "`{verb}` released nothing and must say nothing: {stdout}"
        );
    }
}

/// P1b — the completion timestamp is stored, returned by the API, and was
/// rendered by exactly one surface.
///
/// `done` printed `completed <ts>`; `show` did not, though `--json show` carried
/// `"completed"`. So the moment a task was finished was readable for exactly as
/// long as the `done` line stayed on screen, and the detail view — the surface
/// whose entire job is showing a task's fields — omitted it.
///
/// Both surfaces are asserted, for the reason the cluster above exists: the
/// property is that they agree, not that one of them works.
#[test]
fn the_completion_timestamp_reaches_every_human_surface() {
    let dir = fresh_config_dir("completed-shown");
    let run = |args: &[&str]| {
        bin("completed-shown", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };

    assert!(run(&["init", "P"]).status.success(), "init");
    assert!(run(&["add", "Alpha"]).status.success(), "add");

    let done = run(&["done", "1"]);
    assert!(
        done.status.success(),
        "done: {}",
        String::from_utf8_lossy(&done.stderr)
    );
    let done_out = String::from_utf8_lossy(&done.stdout);
    assert!(
        done_out.contains("completed"),
        "`done` must name the moment: {done_out}"
    );

    // The timestamp the API carries, so the assertion below compares the two
    // surfaces against one value rather than against each other's formatting.
    let json = run(&["--json", "show", "1"]);
    let raw = String::from_utf8_lossy(&json.stdout);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("--json show parses");
    let ts = v
        .get("completed")
        .and_then(|c| c.as_str())
        .expect("the API carries `completed`")
        .to_string();

    let show = run(&["show", "1"]);
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_out.contains(&ts),
        "`show` must render the `completed` value the API returns ({ts}): {show_out}"
    );

    // A task that was never completed has no such moment, and a detail view
    // that prints an empty `completed` line for every pending task is the
    // mirror-image bug.
    assert!(run(&["add", "Beta"]).status.success(), "add beta");
    let pending = String::from_utf8_lossy(&run(&["show", "2"]).stdout).to_string();
    assert!(
        !pending.contains("completed"),
        "a pending task has no completion moment and must not show one: {pending}"
    );
}

/// P1b, second half — DESIGN.md advertises `completed.after:-7d` as a working
/// query and the filter refused it as an unknown token.
///
/// Code and spec disagreed, and the spec is the reachable reading: the field
/// exists on every row, the API returns it, and `due.before:`/`due.after:`
/// already fix the shape a date-bounded field takes (D33). So the filter grew
/// the pair rather than the manual losing the example.
///
/// Driven through the REAL BINARY because every bug in this area lived between
/// argv and the parser.
#[test]
fn a_completed_bound_selects_by_when_a_task_was_finished() {
    let dir = fresh_config_dir("completed-bound");
    let run = |args: &[&str]| {
        bin("completed-bound", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };

    assert!(run(&["init", "P"]).status.success(), "init");
    assert!(run(&["add", "Finished"]).status.success(), "add");
    assert!(run(&["add", "Open"]).status.success(), "add");
    assert!(run(&["done", "1"]).status.success(), "done");

    // The spelling DESIGN.md advertises, and its `before` twin. `-7d` is in the
    // past and `+7d` in the future, so a task completed just now falls after
    // the first and before the second regardless of when the suite runs.
    for bound in ["completed.after:-7d", "completed.before:+7d"] {
        let out = run(&["list", bound, "status:done"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "`{bound}` must be accepted: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("Finished"),
            "`{bound}` must select the completed task: {stdout}"
        );
        // The task that was never completed has no completion instant, so no
        // bound on that field can select it — the same rule `due.before:` has
        // for a task with no due date.
        assert!(
            !stdout.contains("Open"),
            "`{bound}` must not select an uncompleted task: {stdout}"
        );
    }

    // The other direction of each bound excludes it, which is what proves the
    // comparison runs rather than the predicate matching everything.
    for bound in ["completed.before:-7d", "completed.after:+7d"] {
        let out = run(&["list", bound]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "`{bound}` must be accepted: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("Finished"),
            "`{bound}` must exclude it: {stdout}"
        );
    }

    // D33: an unreadable bound is refused by name, not answered with the empty
    // list a genuine no-match produces.
    let out = run(&["list", "completed.after:yesterdya"]);
    assert!(
        !out.status.success(),
        "a misspelled completed bound must not exit 0"
    );
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        msg.contains("yesterdya"),
        "the error must name the offending value: {msg}"
    );
}

/// A hand-edited `config.toml` naming a theme that does not exist was ignored
/// in silence, and then `config get`/`config list` REPORTED THE IGNORED NAME as
/// though it were in effect. So the one command whose job is answering "what is
/// my theme" answered with a value the renderer had already thrown away, while
/// `theme show` rendered the default. Two surfaces, one question, two answers.
///
/// The rule this pins (the third case in the theme-validation family):
/// `--theme`/`$TASQX_THEME` WARN because refusing would discard a captured task;
/// `theme set`/`config set` REJECT because they persist; a hand-edited FILE also
/// persists, but refusing to start would lock the user out of the very tool that
/// fixes it — so it warns, and every reader reports the EFFECTIVE value.
///
/// Both `config` surfaces are asserted, in both output modes, because a value
/// this wrong being right in the table and wrong in the JSON is exactly the
/// half-fix this project keeps shipping.
#[test]
fn an_unknown_theme_in_the_config_file_is_reported_as_the_theme_actually_used() {
    let dir = fresh_config_dir("file-bogus");
    std::fs::write(
        dir.join("config.toml"),
        "[theme]\nname = \"hand-edited-bogus\"\n",
    )
    .expect("write config");
    let run = |args: &[&str]| {
        bin("file-bogus", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };

    let get = run(&["config", "get", "theme.name"]);
    assert_eq!(
        String::from_utf8_lossy(&get.stdout).trim(),
        "nord",
        "`config get` must report the theme in effect, not the one the file asked for"
    );
    let err = String::from_utf8_lossy(&get.stderr);
    assert!(
        err.contains("hand-edited-bogus"),
        "the warning must name the ignored value: {err}"
    );
    assert!(
        err.contains("config.toml"),
        "and the layer it came from: {err}"
    );
    assert_eq!(
        err.matches("unknown theme").count(),
        1,
        "warned twice for one value: {err}"
    );

    // The JSON twin: a script reading this must not get the discarded name.
    let get_json = run(&["--json", "config", "get", "theme.name"]);
    let v: serde_json::Value =
        serde_json::from_slice(&get_json.stdout).expect("config get --json is JSON");
    assert_eq!(v["value"], "nord", "--json reported the ignored name: {v}");

    // `config list` reports a SOURCE too, and a source of "config.toml" beside
    // an effective value of "nord" would be a second, subtler lie.
    let list = run(&["--json", "config", "list"]);
    let v: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("config list --json is JSON");
    let row = v["settings"]
        .as_array()
        .expect("settings array")
        .iter()
        .find(|r| r["key"] == "theme.name")
        .expect("theme.name row")
        .clone();
    assert_eq!(
        row["value"], "nord",
        "`config list` reported the ignored name: {row}"
    );
    assert_eq!(
        row["source"], "default",
        "the ignored layer must not be credited: {row}"
    );

    // And the surface that disagreed in the first place still renders nord, so
    // the two now agree by having been made to compute the same thing.
    let show = run(&["theme", "show"]);
    assert!(
        String::from_utf8_lossy(&show.stdout).contains("Theme: nord"),
        "theme show: {}",
        String::from_utf8_lossy(&show.stdout)
    );
}

/// The sibling that must NOT change: a config file naming a real theme reports
/// that theme, from that layer, and says nothing on stderr. Without this, a
/// "fix" that always answered `nord` and always warned would pass every
/// assertion above.
#[test]
fn a_known_theme_in_the_config_file_is_reported_from_the_file_and_warns_about_nothing() {
    let dir = fresh_config_dir("file-known");
    std::fs::write(dir.join("config.toml"), "[theme]\nname = \"gruvbox\"\n").expect("write config");
    let run = |args: &[&str]| {
        bin("file-known", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };

    let get = run(&["config", "get", "theme.name"]);
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "gruvbox");
    let err = String::from_utf8_lossy(&get.stderr);
    assert!(
        !err.contains("unknown theme"),
        "a real theme must warn about nothing: {err}"
    );

    let list = run(&["--json", "config", "list"]);
    let v: serde_json::Value = serde_json::from_slice(&list.stdout).expect("JSON");
    let row = v["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "theme.name")
        .unwrap();
    assert_eq!(row["value"], "gruvbox");
    assert_eq!(row["source"], "config.toml", "the file really did win here");
}

/// P3a: the terminal table padded and truncated by CHAR COUNT, so a CJK or
/// emoji title shifted every column to its right and `tasqx list` stopped being
/// a table.
///
/// This drives the REAL BINARY rather than the renderer, because the finding is
/// about bytes reaching a terminal: the titles travel through argv, through the
/// store, through JSON and back out through the render path, and a unit test
/// that hands the renderer a `Value` skips every one of those. Width is measured
/// with `unicode_width` directly rather than through the CLI's own helper, so a
/// wrong width function cannot make its own output look right.
#[test]
fn the_task_table_stays_aligned_when_a_title_is_not_ascii() {
    use unicode_width::UnicodeWidthStr;

    let dir = fresh_config_dir("wide-table");
    let run = |args: &[&str]| {
        bin("wide-table", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    run(&["init", "work"]);

    // One entry per way a char count and a cell count can disagree: two-cell
    // ideographs, a zero-cell combining mark, a five-char/two-cell emoji ZWJ
    // sequence, and a two-char/two-cell skin-tone cluster. Plus a CJK title long
    // enough that it MUST be truncated, which is where a char-counting truncate
    // overflowed the column by 33 cells.
    let titles = [
        "plain ascii",
        "\u{6f22}\u{5b57}\u{30c6}\u{30b9}\u{30c8}",
        "e\u{301}accent",
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466} family",
        "\u{1f44d}\u{1f3fd} thumb",
        &"\u{4e2d}\u{6587}".repeat(30),
    ];
    for t in titles {
        let out = run(&["add", t]);
        assert!(
            out.status.success(),
            "add {t:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = run(&["list"]);
    let stdout = String::from_utf8(out.stdout).expect("UTF-8");
    let rows: Vec<&str> = stdout.lines().skip(2).take(titles.len()).collect();
    assert_eq!(
        rows.len(),
        titles.len(),
        "expected one row per task:\n{stdout}"
    );

    // Every task carries the same project, due and tags, so the rows differ
    // ONLY in the title — equal display width is exactly "the title column held
    // its 36 cells". The header is included: a table whose rows agree with each
    // other but not with their own column labels is still misaligned.
    let want = rows[0].width();
    for (row, title) in rows.iter().zip(titles) {
        assert_eq!(
            row.width(),
            want,
            "row for {title:?} is {} cells, not {want}:\n{stdout}",
            row.width()
        );
    }
    let header = stdout.lines().next().expect("header");
    let project_col = header.find("PROJECT").expect("PROJECT header");
    let project_col = header[..project_col].width();
    for (row, title) in rows.iter().zip(titles) {
        let at = row
            .find("work")
            .unwrap_or_else(|| panic!("no project cell for {title:?}"));
        assert_eq!(
            row[..at].width(),
            project_col,
            "the PROJECT column moved for {title:?}:\n{stdout}"
        );
    }
}

/// P3b: `tasqx why` printed `age  -0.00`. The age term is
/// `(age_days * 0.01).min(1.0)` over `age_days = (-age).max(0.0)`, and a task
/// created inside the second the clock is read has `age == 0.0` — so the
/// negation hands `max` two zeros that compare equal and it is free to return
/// the `-0.0`, which the multiply carries and `{:.2}` faithfully prints,
/// telling the reader a term subtracted urgency when it contributed none.
///
/// The unit test in `render` is the one that can SCHEDULE the value; this is the
/// end-to-end backstop over the whole path, on the input that produced it in the
/// original report — a task created a moment ago.
#[test]
fn why_prints_no_negative_zero_component() {
    let dir = fresh_config_dir("why-negzero");
    let run = |args: &[&str]| {
        bin("why-negzero", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    run(&["init", "work"]);
    run(&["add", "fresh"]);

    let out = run(&["why", "1"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("age"),
        "no age component to check:\n{stdout}"
    );
    assert!(
        !stdout.contains("-0"),
        "a component rendered as negative zero:\n{stdout}"
    );
}

/// P3c: an out-of-range date was refused with `jiff`'s internals — "parameter
/// 'Unix timestamp seconds' is not in the required range of …" — naming neither
/// the value typed nor anything that would have worked.
///
/// Both spellings of the same request are driven: the `due:` sugar the CLI
/// parses itself and the `--due` flag, because they reach `parse_when` from two
/// different places in `lib.rs` and a fix wired into one would leave the other
/// leaking. And `scheduled`, because it is a third call site of the same
/// function on the same command.
#[test]
fn an_out_of_range_date_is_refused_in_this_tools_words() {
    let dir = fresh_config_dir("date-range");
    let run = |args: &[&str]| {
        bin("date-range", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    run(&["init", "work"]);

    for args in [
        vec!["add", "probe", "due:9999-12-31"],
        vec!["add", "probe", "--due", "9999-12-31T23:59"],
        vec!["add", "probe", "--scheduled", "9999-12-31"],
    ] {
        let out = run(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} should be bad_request: {err}"
        );
        assert!(
            err.contains("9999-12-31"),
            "{args:?}: value not named: {err}"
        );
        assert!(
            err.contains("9999-12-30"),
            "{args:?}: no usable bound named: {err}"
        );
        for leak in ["Unix timestamp", "parameter", "overflowed", "-377705023201"] {
            assert!(
                !err.contains(leak),
                "{args:?}: jiff internals leaked ({leak:?}): {err}"
            );
        }
    }

    // The near side of the boundary still works — a guard that only proved the
    // refusal would be satisfied by a parser that refused every date.
    let ok = run(&["add", "probe", "due:9999-12-30"]);
    assert!(
        ok.status.success(),
        "a storable date was refused: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

/// The same char-vs-cell rule on `tasqx config list`, whose VALUE column carries
/// a project name the user chose. This one is padded but never truncated: the
/// value is the data the reader came to read, so overflowing the cell is a cost
/// worth paying and silently cutting it is not.
#[test]
fn the_config_table_stays_aligned_when_a_value_is_not_ascii() {
    use unicode_width::UnicodeWidthStr;

    let dir = fresh_config_dir("wide-config");
    let run = |args: &[&str]| {
        bin("wide-config", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    // `default_project` is free text the user picks, and it lands in this table.
    run(&["init", "\u{6f22}\u{5b57}"]);

    let out = run(&["config", "list"]);
    let stdout = String::from_utf8(out.stdout).expect("UTF-8");
    let header = stdout.lines().next().expect("header");
    let source_col = header[..header.find("SOURCE").expect("SOURCE header")].width();
    for row in stdout.lines().skip(1).filter(|l| !l.trim().is_empty()) {
        // SOURCE is the last field on the row, and `rfind` takes its LAST
        // occurrence — which matters, because `default` is also a substring of
        // the `default_project` key sitting in column one.
        let Some(last) = row.split_whitespace().last() else {
            continue;
        };
        let at = row.rfind(last).expect("the field came from this row");
        assert_eq!(
            row[..at].width(),
            source_col,
            "the SOURCE column moved on this row:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("\u{6f22}\u{5b57}"),
        "the value must still be shown whole:\n{stdout}"
    );
}

/// `detail.time_format` has three meanings and no more. A fourth value would
/// persist happily and then be read as the default on every run — the config
/// equivalent of answering `ok` to a write that changes nothing — so the refusal
/// belongs at `config set`, and it has to name the vocabulary it is holding the
/// user to.
///
/// End-to-end because the registry check and the exit code are two different
/// facts: validation that returns an error nothing surfaces still exits 0.
#[test]
fn config_set_refuses_an_unknown_detail_time_format() {
    let dir = fresh_config_dir("timefmt");
    let out = bin("timefmt", &dir)
        .args(["config", "set", "detail.time_format", "xyz"])
        .output()
        .expect("config set");
    assert!(!out.status.success(), "an unknown value must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("iso"),
        "the error must list the valid values, got: {stderr}"
    );

    // And a valid one still works.
    assert!(bin("timefmt", &dir)
        .args(["config", "set", "detail.time_format", "relative"])
        .status()
        .expect("config set")
        .success());
}

/// #52 — `tag`/`untag` end to end, through the real binary.
///
/// The binary and not a unit test, for three reasons that all live above the
/// engine: the exit code a script branches on (4 for a tag the task does not
/// have), the tag *vocabulary* — argv is where `+api` and `api` have to become
/// one name — and the fact that a verb reaches its handler at all. DESIGN listed
/// `tag`/`untag` as shipped MVP while `tasqx tag --help` failed, which is
/// exactly the shape a test below the argv layer cannot see.
#[test]
fn tag_and_untag_agree_with_sugar_and_refuse_a_tag_the_task_lacks() {
    let dir = fresh_config_dir("tagverb");
    let run = |args: &[&str]| bin("tagverb", &dir).args(args).output().expect("run tasqx");
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    ok(&["init", "work"]);
    ok(&["add", "paint the hall"]);

    // The two spellings must name ONE tag. `+api` sent verbatim would create a
    // tag literally called `+api`: invisible beside the `api` the `modify +tag`
    // sugar writes, and unreachable by the `+api` filter token that reads it.
    ok(&["tag", "1", "+api", "api", "release"]);
    let shown = ok(&["--json", "show", "1"]);
    assert!(
        shown.contains(r#""api""#) && !shown.contains(r#""+api""#),
        "`+api` and `api` must be one tag, stored without the sigil: {shown}"
    );
    assert_eq!(
        shown.matches(r#""api""#).count(),
        1,
        "the duplicate must collapse rather than being stored twice: {shown}"
    );

    // The human line names what changed AND what remains: "tags: +release" on
    // its own is the line a removal that did nothing would also print.
    let removed = ok(&["untag", "1", "api"]);
    assert!(
        removed.contains("untagged") && removed.contains("+api"),
        "the removal line must name the tag it took: {removed}"
    );
    assert!(
        removed.contains("+release"),
        "and the set that remains: {removed}"
    );

    // D52: a tag the task does not have is exit 4, and nothing is removed.
    let miss = run(&["untag", "1", "release", "blockign"]);
    assert_eq!(
        miss.status.code(),
        Some(4),
        "a tag the task lacks must not answer ok; stdout was {}",
        String::from_utf8_lossy(&miss.stdout)
    );
    let stderr = String::from_utf8_lossy(&miss.stderr);
    assert!(
        stderr.contains("blockign") && stderr.contains("release"),
        "the refusal must name the missing tag and the real ones: {stderr}"
    );
    let shown = ok(&["--json", "show", "1"]);
    assert!(
        shown.contains(r#""release""#),
        "the removable half of an all-or-nothing call must survive it: {shown}"
    );

    // A bare `+` names no tag. In `add` it falls through to the title (C6);
    // here there is no title, so the choice is a refusal or a silent deletion.
    let bare = run(&["tag", "1", "+"]);
    assert_eq!(
        bare.status.code(),
        Some(2),
        "a bare `+` must be refused, not silently dropped"
    );
}

/// #53 — the CLI `archive` verb, end to end through the real binary.
///
/// `project.archive` shipped with the engine and was reachable over the JSON API
/// and MCP, and from the terminal not at all: D22 wrote the rule down ("archiving
/// the current default clears it") together with the sentence "there is no CLI
/// `archive` verb today … when one lands it renders `default_cleared`". This is
/// that verb, and the assertion is deliberately about the terminal rather than
/// the engine — the engine half is pinned in the core suite. What only the binary
/// can show is the exit code a script branches on, the words the user reads, and
/// whether the default really moved underneath them.
///
/// The default-clearing branch is the whole reason the verb needs copy. A `tasqx
/// archive work` that printed nothing but "Project work archived" would change
/// where every later bare `tasqx add` lands with nobody told — this project's
/// recurring failure, arriving through the one verb that had no terminal.
#[test]
fn archive_retires_a_project_and_says_when_it_cleared_the_default() {
    let dir = fresh_config_dir("archiveverb");
    let run = |args: &[&str]| {
        bin("archiveverb", &dir)
            .args(args)
            .output()
            .expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // `work` is created first, so it is the store's default (D21).
    ok(&["init", "work"]);
    ok(&["init", "side"]);
    ok(&["add", "keep this", "--project", "side"]);

    // Archiving a NON-default project leaves the default alone — and says so,
    // rather than saying nothing: silence is also what the cleared case would
    // print if the field were dropped.
    let quiet = ok(&["archive", "side"]);
    assert!(quiet.contains("side"), "must name the project: {quiet}");
    assert!(
        quiet.contains("unchanged"),
        "the untouched default must be stated, not implied: {quiet}"
    );
    let listed = ok(&["--json", "projects"]);
    assert!(
        !listed.contains("side"),
        "an archived project must drop out of `projects`: {listed}"
    );
    let all = ok(&["--json", "projects", "--all"]);
    assert!(
        all.contains("side"),
        "`--all` must still show it — archiving is a shelf, not a delete: {all}"
    );
    // The tasks are untouched: still there, still in the project. Read as JSON
    // rather than as a substring, because `--json` pretty-prints and a
    // hand-spelled `"project":"side"` would be a test that passes on the
    // formatter rather than on the value.
    let shown: serde_json::Value =
        serde_json::from_str(&ok(&["--json", "show", "1"])).expect("show --json");
    assert_eq!(
        shown["project"], "side",
        "archiving a project must not touch its tasks: {shown}"
    );

    // D22, the loud half: archiving the CURRENT default clears the default.
    let loud = ok(&["archive", "work"]);
    assert!(
        !loud.contains("unchanged"),
        "the default was cleared and the line says it was not: {loud}"
    );
    assert!(
        loud.contains("default project"),
        "the default moved and the line does not say so: {loud}"
    );
    assert!(
        loud.contains("tasqx use"),
        "must name the way to point the default somewhere again: {loud}"
    );

    // And the outcome the user actually gets: a bare `add` is now projectless,
    // the same state a fresh store is in.
    //
    // What this does NOT prove, stated plainly, because it was measured rather
    // than assumed: it does not prove the ARCHIVE cleared the key. Deleting
    // `clear_config` from `project_archive` leaves this assertion green, because
    // every CLI command opens the store afresh and D23(b)'s stale-default repair
    // (`storage::repair_stale_default_project`) deletes a default naming an
    // archived project on the way in. The two mechanisms are indistinguishable
    // from a process boundary. The engine half is pinned inside one Engine by
    // `archiving_the_default_project_clears_the_default_and_reports_it` in the
    // core suite, which does redden for that mutation.
    ok(&["add", "homeless"]);
    let shown: serde_json::Value =
        serde_json::from_str(&ok(&["--json", "show", "2"])).expect("show --json");
    assert_eq!(
        shown["project"],
        serde_json::Value::Null,
        "the default was reported cleared and a bare add still landed in it: {shown}"
    );

    // Validation happens at the edge, in the core, exactly as `use` does it:
    // an unknown name is exit 4 naming it, and a blank one is exit 2.
    let unknown = run(&["archive", "nope"]);
    assert_eq!(
        unknown.status.code(),
        Some(4),
        "an unknown project must be not_found; stdout was {}",
        String::from_utf8_lossy(&unknown.stdout)
    );
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("nope"),
        "the refusal must name what the caller got wrong: {}",
        String::from_utf8_lossy(&unknown.stderr)
    );
    assert_eq!(
        run(&["archive", ""]).status.code(),
        Some(2),
        "an empty name is bad_request, as it is on `use` (D36)"
    );
}
