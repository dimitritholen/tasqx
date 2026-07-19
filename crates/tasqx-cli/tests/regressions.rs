//! End-to-end guards for bugs found by using the tool, not by reading it.
//!
//! Each of these reproduced as a real session: a typo'd theme name that was
//! answered with a theme the user did not ask for, a wrong-typed config value
//! that `config` refused to mention, and a saved theme with nothing pointing at
//! the one command that makes it visible. They run the real binary because all
//! three are about what reaches a terminal — exit code, stdout, stderr — which
//! is the surface a unit test cannot see.

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
fn bin(tag: &str, dir: &std::path::Path) -> Command {
    let mut db = std::env::temp_dir();
    db.push(format!("tasqx-reg-{tag}-{}.db", std::process::id()));
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("TASQX_CONFIG_DIR", dir).env("TASQX_DB", db);
    c
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

    assert_eq!(out.status.code(), Some(2), "an unknown theme must be bad_request, like `theme set`");
    assert!(
        out.stdout.is_empty(),
        "a rejected name must not print a theme the user did not ask for: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("geen-thema-xyz"), "the message must name the typo: {err}");
    assert!(err.contains("tasqx theme list"), "and the way to find the real names: {err}");
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
    assert!(s.contains("urgency.ramp"), "the full role list must still print: {s}");
}

/// `theme show` with no argument shows the ACTIVE theme and must not be
/// validated into failing. It takes a different branch, and a validator hoisted
/// above the match would reject the empty name and break the command's most
/// common form.
#[test]
fn theme_show_without_a_name_still_works() {
    let dir = fresh_config_dir("show-bare");
    let out = bin("show-bare", &dir).args(["theme", "show"]).output().expect("run theme show");

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

    assert!(out.status.success(), "a wrong-typed value must not fail the command");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "nord",
        "stdout stays the usable default, so `$(tasqx config get ...)` still works"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("theme.name"), "the warning must name the key: {err}");
    assert!(err.contains("string"), "and the declared type: {err}");
    assert!(err.contains("integer"), "and what was actually found: {err}");
}

/// `config list` must survive the same file. Reporting the mismatch as an error
/// would abort the one command that shows every key at once — the command you
/// reach for to find which line is wrong.
#[test]
fn config_list_still_reports_every_setting_despite_a_bad_line() {
    let dir = fresh_config_dir("wrong-type-list");
    std::fs::write(dir.join("config.toml"), "[theme]\nname = 42\n").unwrap();

    let out = bin("wrong-type-list", &dir).args(["config", "list"]).output().expect("run config list");

    assert!(out.status.success(), "one bad line must not abort the whole listing");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("theme.name"), "{s}");
    assert!(s.contains("notify.enabled"), "the other keys are still readable: {s}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("expected string"), "and still warned about");
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

    let out = bin("wrong-type-quiet", &dir).args(["theme", "list"]).output().expect("run theme list");

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

    let set = bin("pointer", &dir).args(["theme", "set", "gruvbox"]).output().expect("run theme set");
    assert!(set.status.success());
    let s = String::from_utf8_lossy(&set.stdout);
    assert!(s.contains("theme.name = gruvbox"), "the confirmation must still name the write: {s}");
    assert!(s.contains("tasqx theme show"), "`theme set` must point at the preview: {s}");

    let cfg = bin("pointer", &dir)
        .args(["config", "set", "theme.name", "dracula"])
        .output()
        .expect("run config set");
    assert!(cfg.status.success());
    let c = String::from_utf8_lossy(&cfg.stdout);
    assert!(c.contains("theme.name = dracula"), "{c}");
    assert!(c.contains("tasqx theme show"), "`config set theme.name` must point at it too: {c}");
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
    assert!(!s.contains("theme show"), "notify.enabled has nothing to do with themes: {s}");
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
    let out = Command::new(env!("CARGO_BIN_EXE_tasqx")).arg("--version").output().expect("run --version");
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);

    assert!(s.contains(env!("CARGO_PKG_VERSION")), "the crate version must survive: {s}");
    assert!(s.trim() != format!("tasqx {}", env!("CARGO_PKG_VERSION")), "a bare crate version is the bug: {s}");

    match Command::new("git").args(["rev-parse", "--short=12", "HEAD"]).output() {
        Ok(g) if g.status.success() => {
            let sha = String::from_utf8_lossy(&g.stdout).trim().to_string();
            assert!(s.contains(&sha), "must name the actual HEAD commit {sha}: {s}");
        }
        // No git, no checkout: `unknown` is the honest answer and the build
        // must still have produced a binary rather than failing outright.
        _ => assert!(s.contains("unknown"), "without git the id must be `unknown`: {s}"),
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
        let out = bin("import-shape", &dir).arg("import").arg(&path).output().expect("run import");

        assert_eq!(out.status.code(), Some(2), "`{body}` must be a bad_request, not a silent success");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!stdout.contains("Imported"), "`{body}` must not report an import: {stdout}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.starts_with("error [bad_request]: "), "must use the shared error format: {err}");
        assert!(err.contains(needle), "`{body}` must be explained by naming `{needle}`: {err}");
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
    assert!(add.status.success(), "add failed: {}", String::from_utf8_lossy(&add.stderr));
    assert!(
        String::from_utf8_lossy(&add.stdout).contains("backlog"),
        "a future wait must still park the task in the backlog"
    );

    let listed = bin("wait-release", &dir).arg("list").output().expect("run list");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("No tasks"),
        "while the wait is ahead the task stays out of the default view"
    );

    let modified =
        bin("wait-release", &dir).args(["modify", "1", "wait:2020-01-01"]).output().expect("run modify");
    assert!(modified.status.success(), "modify failed: {}", String::from_utf8_lossy(&modified.stderr));

    let listed = bin("wait-release", &dir).arg("list").output().expect("run list");
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("waiter"), "a passed wait must return the task to `list`: {stdout}");

    let shown = bin("wait-release", &dir).args(["show", "1"]).output().expect("run show");
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("pending"), "`show` must agree with `list`: {shown}");
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
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
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
    assert!(shown.contains(r#""big job""#), "modify must keep the tag whole too: {shown}");
    assert!(
        shown.contains(r#""title": "painting job""#),
        "a sugar-only modify must not touch the title: {shown}"
    );

    // The literal-quote form (quotes reaching argv unstripped) must agree with
    // the shell-stripped form — the same equivalence C1 relies on when reading.
    json("spaced-tag", &["add", "second", r#"+"needs paint""#]);
    let shown = json("spaced-tag", &["--json", "show", "2"]);
    assert!(shown.contains(r#""needs paint""#), "literal quotes must parse the same: {shown}");
    assert!(shown.contains(r#""title": "second""#), "and leave the title alone: {shown}");
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
    let run = |args: &[&str]| bin("hyphen-filter", &dir).args(args).output().expect("run tasqx");
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    ok(&["add", "paint the shed", "+needs", "+home"]);
    ok(&["add", "other thing", "+home"]);

    // `list`: the excluded task is gone, the other one stays.
    let listed = ok(&["list", "-needs"]);
    assert!(!listed.contains("paint the shed"), "-needs must exclude the tagged task: {listed}");
    assert!(listed.contains("other thing"), "-needs must not exclude everything else: {listed}");

    // Every other filter-taking argument carries the same grammar and so must
    // accept the same token — `report` takes group_by first, `export` and
    // `watch` take a bare filter.
    let exported = ok(&["export", "-needs"]);
    assert!(!exported.contains("paint the shed"), "export must honour -tag too: {exported}");
    assert!(exported.contains("other thing"), "export must still return the rest: {exported}");
    assert!(run(&["report", "project", "-needs"]).status.success(), "report must accept -tag");

    // Making `-tag` typable must not cost the flags: `--json` before the
    // filter still switches the output format instead of reaching the parser
    // as filter text. (After the filter is C3r's job.)
    let json = ok(&["list", "--json", "+home"]);
    assert!(json.trim_start().starts_with('{'), "--json must stay a flag, not become filter text: {json}");

    // And an unknown flag must still be rejected, not silently treated as a
    // filter token — `--bogus` briefly parsed as "exclude the tag `-bogus`",
    // excluded nothing, and listed EVERY task with exit 0. The message must
    // name what was typed, since that is the whole recovery.
    let bogus = run(&["list", "--bogus"]);
    assert!(!bogus.status.success(), "an unknown flag must stay an error, not become filter text");
    let err = String::from_utf8_lossy(&bogus.stderr);
    assert!(err.contains("--bogus"), "the error must name the offending flag: {err}");
    assert!(err.contains("-tag"), "and point at the shape that does work: {err}");
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
    let run = |args: &[&str]| bin("hyphen-filter-order", &dir).args(args).output().expect("run tasqx");
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
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
        assert!(out.trim_start().starts_with('{'), "{args:?} must emit JSON, not a table: {out}");
    }
    for args in [vec!["report", "project", "--html"], vec!["report", "project", "+home", "--html"]] {
        let out = ok(&args);
        assert!(out.contains("<html"), "{args:?} must emit HTML, not a table: {out}");
    }

    // (a) a leading-hyphen FILTER token still reaches the grammar, in every
    // position — including after a flag, and including the quoted form the
    // shell hands over as one token with a space in it.
    let after_flag = ok(&["list", "--json", "-needs"]);
    assert!(!after_flag.contains("paint the shed"), "-needs after a flag must still exclude: {after_flag}");
    assert!(after_flag.contains("other thing"), "-needs must not exclude everything: {after_flag}");
    let before_flag = ok(&["list", "-needs", "--json"]);
    assert!(!before_flag.contains("paint the shed"), "-needs before a flag must still exclude: {before_flag}");
    let mixed = ok(&["list", "+home", "-needs"]);
    assert!(!mixed.contains("paint the shed"), "-needs beside another token must exclude: {mixed}");
    // The quoted form, as a shell hands it over: one token, inner quotes intact
    // (`tasqx list '-"needs paint"'`). It survives the join-and-retokenize trip.
    ok(&["add", "fence job", "+needs paint"]);
    let quoted = ok(&["list", "-\"needs paint\""]);
    assert!(!quoted.contains("fence job"), "a quoted tag exclusion must exclude: {quoted}");
    assert!(quoted.contains("other thing"), "and must not exclude everything: {quoted}");

    // (b) an unknown flag stays an ERROR wherever it appears — never silent
    // filter text that widens the result set, and never a bare clap usage dump:
    // the message names what was typed and the shape that would have worked.
    for args in [vec!["list", "--nosuchflag"], vec!["list", "+home", "--nosuchflag"]] {
        let out = run(&args);
        assert!(!out.status.success(), "{args:?}: an unknown flag must stay an error");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--nosuchflag"), "{args:?}: name the offending flag: {err}");
        assert!(err.contains("-tag"), "{args:?}: point at the shape that works: {err}");
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
        bin("read-quoting", &dir).args(args).output().expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    ok(&["init", "Home Renovation"]);
    ok(&["add", "painted", "+needs paint"]);
    ok(&["add", "unrelated", "+api"]);

    // `list`: the shell-stripped form is the bug; the literal-quote form is the
    // control that already worked and must keep working.
    for form in ["+needs paint", r#"+"needs paint""#] {
        let s = ok(&["list", form]);
        assert!(s.contains("painted"), "{form:?} must select the spaced tag: {s}");
        assert!(!s.contains("unrelated"), "{form:?} must not widen to every task: {s}");
    }
    // A value key with a space behaves the same; `Home Renovation` is the
    // project `add` accepted whole one command earlier.
    for form in ["project:Home Renovation", r#"project:"Home Renovation""#] {
        let s = ok(&["list", form]);
        assert!(s.contains("painted") && s.contains("unrelated"), "{form:?}: {s}");
    }

    // Several argv elements must still be several tokens: this filter matches
    // nothing precisely because both predicates are read and ANDed.
    let s = ok(&["list", "+api", "status:done"]);
    assert!(s.contains("No tasks."), "a multi-element filter must stay multi-token: {s}");
    let s = ok(&["list", "+api", "status:pending"]);
    assert!(s.contains("unrelated") && !s.contains("painted"), "and still select: {s}");

    // `export`, `report` and `watch` each carried their own join.
    let s = ok(&["export", "+needs paint"]);
    assert!(s.contains("painted") && !s.contains("unrelated"), "export takes a filter too: {s}");
    let s = ok(&["report", "project", "+needs paint"]);
    assert!(s.contains("Home Renovation"), "report's tail after group_by is a filter: {s}");
    // `watch` blocks on a daemon, so only its argument handling is reachable:
    // the filter must at least not be rejected as unparseable before connecting.
    let out = run(&["watch", "+needs paint", "--json"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("unknown filter token"), "watch must not mis-split its filter: {err}");
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
        bin("bad-prio", &dir).args(args).output().expect("run tasqx")
    };

    let out = run(&["add", "urgent thing", "!urgent"]);
    assert_eq!(out.status.code(), Some(2), "an invalid priority must be bad_request, like --priority");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("urgent"), "the error must name the offending value: {err}");
    assert!(err.contains("!high") && err.contains("!low"), "and list the way out: {err}");

    // Refused at the door means nothing was written — not a task named after
    // the typo, and not one silently missing the priority that was asked for.
    let s = String::from_utf8_lossy(&run(&["list"]).stdout).to_string();
    assert!(s.contains("No tasks."), "a refused add must store nothing: {s}");

    // `modify` shares the parser, so it must share the answer (D13).
    assert!(run(&["add", "real task"]).status.success());
    let out = run(&["modify", "1", "!urgent"]);
    assert_eq!(out.status.code(), Some(2), "modify must refuse the same token");

    // The valid spellings, both short and long, still work on both verbs.
    assert!(run(&["add", "high one", "!h"]).status.success());
    assert!(run(&["modify", "1", "!medium"]).status.success());
    let s = String::from_utf8_lossy(&run(&["list", "--json"]).stdout).to_string();
    assert!(s.contains("\"priority\": \"H\""), "!h must still set H: {s}");
    assert!(s.contains("\"priority\": \"M\""), "!medium must still set M: {s}");
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
        bin("sentinel-leak", &dir).env("TASQX_FORCE_COLOR", "1").args(args).output().expect("run tasqx")
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
    assert!(err.contains("-nord"), "the warning must name the value the user typed: {err}");
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
        bin("one-quoting-rule", &dir).args(args).output().expect("run tasqx")
    };
    let ok = |args: &[&str]| -> String {
        let out = run(args);
        assert!(out.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
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
    assert_eq!(s.matches(r#""say\"hi""#).count(), 2, "and the tag verbatim: {s}");

    // The round trip closes: the read side names what the write side created.
    let s = ok(&["list", project_lit]);
    assert!(s.contains("before") && s.contains("after"), "filter must name the project: {s}");
    let s = ok(&["list", tag_lit]);
    assert!(s.contains("before") && s.contains("after"), "filter must name the tag: {s}");

    // The two spellings converge, as they already do on the read side: a value
    // whose only special character is a space needs no quotes once the shell
    // has drawn the argument boundary.
    ok(&["init", "Home Renovation"]);
    ok(&["add", "stripped", "project:Home Renovation"]);
    ok(&["add", "literal", r#"project:"Home Renovation""#]);
    let s = ok(&["list", "project:Home Renovation"]);
    assert!(s.contains("stripped") && s.contains("literal"), "both spellings converge: {s}");

    // A lone quote is now a refusal, not a silent swallow. `+say"hi` opens a
    // quoted run that never closes; guessing where it ended is how the tag
    // `sayhi` — a value the user never typed — used to get stored.
    let out = run(&["add", "wall", r#"+say"hi"#]);
    assert_eq!(out.status.code(), Some(2), "an unterminated quote must be a bad_request");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unterminated"), "and must say so: {err}");
    assert!(err.contains('\\'), "and name the escape that is the way out: {err}");
    let s = ok(&["list", "--json"]);
    assert!(!s.contains("sayhi"), "nothing may be stored from a refused line: {s}");

    // A whole-element value is NOT truncated — the shell drew that boundary —
    // so an unknown name there stays the plain typo message it should be.
    let out = run(&["add", "t", "project:No Such Project"]);
    assert_eq!(out.status.code(), Some(4), "an unknown project is still not_found");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("No Such Project"), "the whole name is named: {err}");
    assert!(!err.contains("before the first space"), "nothing was cut, so say nothing: {err}");

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
    assert!(s.contains("t2"), "the advised spelling must round trip: {s}");
}
