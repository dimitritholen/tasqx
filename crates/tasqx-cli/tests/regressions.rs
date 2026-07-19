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
