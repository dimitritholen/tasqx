//! The `--json` contract, enforced end-to-end.
//!
//! DESIGN.md line 19 promises "Every command speaks human-readable text *and*
//! `--json`". That promise was false for five commands, because `cli.json` was
//! consulted once — on the outcome of the big `match cli.command` — and every
//! command dispatched by an early `return` above that point never saw the flag.
//! `tasqx --json report` emitted JSON while `tasqx --json report --html` printed
//! prose; `tasqx --json theme set X` printed prose while its exact alias
//! `tasqx --json config set theme.name X` printed JSON.
//!
//! The structural fix is `Exit`: every path out of `execute` returns either a
//! result (which the single terminal renders per `--json`) or a declared
//! carve-out. This file is the other half — the list of commands comes from
//! clap, not from a hand-kept copy (D30), so a new subcommand cannot join the
//! CLI without joining this guard.
//!
//! It runs the real binary because that is the only place the bug lived: every
//! one of these commands built a correct value internally and then printed the
//! wrong shape of it.

use std::path::PathBuf;
use std::process::Command;

/// A fresh, isolated config dir + store for one test.
fn scratch(tag: &str) -> (PathBuf, PathBuf) {
    let mut cfg = std::env::temp_dir();
    cfg.push(format!("tasqx-json-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cfg);
    std::fs::create_dir_all(&cfg).expect("create config dir");
    let mut db = std::env::temp_dir();
    db.push(format!("tasqx-json-{tag}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    (cfg, db)
}

fn bin(cfg: &std::path::Path, db: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    // `--no-daemon` keeps every case one-shot and in-process: a developer with a
    // daemon running on the default socket must not change what this guard sees.
    c.env("TASQX_CONFIG_DIR", cfg)
        .env("TASQX_DB", db)
        .arg("--no-daemon");
    c
}

/// One command invocation the guard drives, with the args that make it succeed.
///
/// Only the *arguments* are hand-written. The set of command NAMES this table
/// must cover is derived from clap below, so a new subcommand fails this file
/// rather than quietly inheriting whatever `--json` behaviour it happens to have.
struct Case {
    /// The clap subcommand name this case covers.
    verb: &'static str,
    args: &'static [&'static str],
    /// Run against a private, empty store rather than the shared one. Only
    /// `import` needs it: importing a document into a store that already holds
    /// tasks collides on `short_id`, which is a separate question from whether
    /// the command honours `--json`.
    fresh_store: bool,
}

const fn c(verb: &'static str, args: &'static [&'static str]) -> Case {
    Case {
        verb,
        args,
        fresh_store: false,
    }
}

const fn c_fresh(verb: &'static str, args: &'static [&'static str]) -> Case {
    Case {
        verb,
        args,
        fresh_store: true,
    }
}

/// Ordered so each case's precondition is met by the ones before it: the tasks
/// exist before `done` closes one, and `reopen` follows `cancel`.
fn cases(tmp: &str) -> Vec<(Case, Vec<String>)> {
    let raw = vec![
        c("init", &["init", "guardproj"]),
        c("add", &["add", "first thing"]),
        // A second task so `dep`/`undep` have something to point at. Two cases
        // may share a verb; the coverage check only asks that the verb appears.
        c("add", &["add", "second thing"]),
        c("list", &["list"]),
        // `agenda` is the one read whose `--json` body is NOT the `task.list`
        // answer it called: the horizon and the undated rows are applied here,
        // so the flag prints the agenda's own object (`render::agenda_json`).
        // That is exactly why it has to be driven — a hand-built value is the
        // easiest place to emit something that is not JSON at all.
        c("agenda", &["agenda"]),
        c("show", &["show", "1"]),
        c("modify", &["modify", "1", "--priority", "high"]),
        c("annotate", &["annotate", "1", "a note"]),
        // Add before search so the search case has a doc to find; the shared
        // store carries both forward.
        c(
            "memory",
            &["memory", "add", "Guard doc", "the guard needle body"],
        ),
        c("memory", &["memory", "search", "needle"]),
        c("start", &["start", "1"]),
        c("stop", &["stop", "1"]),
        // `untag` follows `tag` and names the tag `tag` just wrote: removing a
        // tag the task does not have is exit 4 by design, and this guard needs a
        // successful run to have any output to judge.
        c("tag", &["tag", "1", "guardtag"]),
        c("untag", &["untag", "1", "guardtag"]),
        // Directly after `untag`, and that placement is the case: `undo` takes
        // no arguments and reverses whatever the newest event happens to be, so
        // the only way to drive it deterministically is to run it where the
        // preceding case has just written something undoable. It puts
        // `guardtag` back, which no later case reads.
        c("undo", &["undo"]),
        c("dep", &["dep", "1", "2"]),
        c("undep", &["undep", "1", "2"]),
        c("why", &["why", "1"]),
        c("next", &["next"]),
        c("projects", &["projects"]),
        c("use", &["use", "guardproj"]),
        // `archive` needs a project of its own: archiving `guardproj` would
        // clear the default this store's later cases run against, and the
        // question here is only whether the verb honours `--json`.
        c("init", &["init", "guardretired"]),
        c("archive", &["archive", "guardretired"]),
        c("done", &["done", "1"]),
        c("cancel", &["cancel", "2"]),
        c("reopen", &["reopen", "2"]),
        c("report", &["report"]),
        // Dry-run by design: this guard only judges the --json shape, and the
        // shared store carries no log-parse measurement to repair anyway.
        c("tokens", &["tokens", "recompute"]),
        c("export", &["export"]),
        c("config", &["config", "list"]),
        c("theme", &["theme", "list"]),
        c("chart", &["chart", "throughput"]),
        c("docs", &["docs", "--no-open", "--stdout"]),
        // Printing only. `completions --install` is the mode that writes, and
        // it is deliberately NOT driven here: this guard runs on the
        // developer's own machine, and a case that installed would append an
        // activation line to their real startup file every time the suite ran.
        // The flag is answered by the same `Exit::Out` terminal either way, so
        // the printing case is what proves the contract.
        c("completions", &["completions", "bash"]),
        c_fresh("import", &["import", "IMPORT_FILE"]),
    ];
    raw.into_iter()
        .map(|case| {
            let args = case
                .args
                .iter()
                .map(|a| match *a {
                    "IMPORT_FILE" => format!("{tmp}/roundtrip.json"),
                    other => other.to_string(),
                })
                .collect();
            (case, args)
        })
        .collect()
}

/// The list of commands this file must cover comes from clap itself.
#[test]
fn every_command_is_either_covered_or_a_declared_carve_out() {
    let covered: Vec<&str> = cases("x").into_iter().map(|(c, _)| c.verb).collect();
    let carved: Vec<&str> = tasqx_cli::JSON_CARVE_OUTS.iter().map(|(n, _)| *n).collect();

    for name in tasqx_cli::subcommand_names() {
        assert!(
            covered.contains(&name.as_str()) || carved.contains(&name.as_str()),
            "new subcommand `{name}` is neither driven by this guard nor on the \
             declared --json carve-out list (tasqx_cli::JSON_CARVE_OUTS). Either \
             give it a case here or record why it may ignore --json."
        );
    }
    // And the carve-out list may not name a command that no longer exists.
    let real = tasqx_cli::subcommand_names();
    for (name, _) in tasqx_cli::JSON_CARVE_OUTS {
        assert!(
            real.contains(&name.to_string()),
            "carve-out `{name}` is not a real subcommand"
        );
    }
}

/// The guard proper: `--json <cmd>` must put JSON on stdout, for every command
/// that is not a declared carve-out.
#[test]
fn every_non_carved_command_emits_json() {
    // BOTH orders, each against its OWN store: most of these cases mutate, so a
    // second pass over one store would hit `already exists` / `already done` and
    // the guard would be judging error text instead of the flag.
    for (tag, flag_first) in [("emits-front", true), ("emits-back", false)] {
        let (cfg, db) = scratch(tag);
        let tmp = cfg.to_string_lossy().replace('\\', "/");
        // The import fixture is exported from a SEPARATE store, so importing it
        // is a clean insert rather than a re-import of ids already present.
        let (dcfg, ddb) = scratch(&format!("{tag}-donor"));
        bin(&dcfg, &ddb)
            .args(["init", "donorproj"])
            .output()
            .expect("donor init");
        bin(&dcfg, &ddb)
            .args(["add", "donated thing"])
            .output()
            .expect("donor add");
        let donated = bin(&dcfg, &ddb)
            .args(["export"])
            .output()
            .expect("donor export");
        std::fs::write(format!("{tmp}/roundtrip.json"), &donated.stdout).expect("write fixture");

        for (i, (case, args)) in cases(&tmp).into_iter().enumerate() {
            let mut own = db.clone();
            if case.fresh_store {
                own.set_file_name(format!("{tag}-case{i}.db"));
                let _ = std::fs::remove_file(&own);
            }
            let mut cmd = bin(&cfg, &own);
            // `--json` is a global flag; a guard that only ever puts it in front
            // would miss a command that only looks at one position.
            if flag_first {
                cmd.arg("--json");
            }
            cmd.args(&args);
            if !flag_first {
                cmd.arg("--json");
            }
            let out = cmd.output().unwrap_or_else(|e| panic!("run {args:?}: {e}"));
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "`tasqx --json {}` must succeed for the guard to judge its output: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
            serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
                panic!(
                    "`{}` ({}) ignored --json (DESIGN.md line 19). stdout was not JSON ({e}): {}",
                    args.join(" "),
                    case.verb,
                    stdout.chars().take(200).collect::<String>()
                )
            });
        }
    }
}

/// The sharpest single case from the report: one command, two output modes, two
/// different answers to the same flag.
#[test]
fn report_honours_json_in_both_of_its_output_modes() {
    let (cfg, db) = scratch("report-modes");
    bin(&cfg, &db).args(["init", "p"]).output().expect("init");
    bin(&cfg, &db).args(["add", "t"]).output().expect("add");
    let html = cfg.join("r.html");

    let text = bin(&cfg, &db)
        .args(["--json", "report"])
        .output()
        .expect("report");
    let doc = bin(&cfg, &db)
        .args(["--json", "report", "--html", "--out"])
        .arg(&html)
        .output()
        .expect("report --html");

    for (label, out) in [("report", &text), ("report --html", &doc)] {
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "`{label}` ignored --json ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
    }
    // The HTML mode's machine-relevant fact is where the file landed.
    let v: serde_json::Value = serde_json::from_slice(&doc.stdout).expect("json");
    assert_eq!(
        v["path"].as_str().map(|p| p.replace('\\', "/")),
        Some(html.to_string_lossy().replace('\\', "/")),
        "the JSON must name the path the report was written to: {v}"
    );
}

/// Two spellings of one write must return one shape. `theme set X` and
/// `config set theme.name X` are the same operation; before the fix one printed
/// prose and the other printed JSON.
#[test]
fn theme_set_and_config_set_agree_under_json() {
    let (cfg, db) = scratch("alias");
    let a = bin(&cfg, &db)
        .args(["--json", "theme", "set", "dracula"])
        .output()
        .expect("theme set");
    let b = bin(&cfg, &db)
        .args(["--json", "config", "set", "theme.name", "dracula"])
        .output()
        .expect("config set");

    let ja: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap_or_else(|e| {
        panic!(
            "`theme set` ignored --json ({e}): {}",
            String::from_utf8_lossy(&a.stdout)
        )
    });
    let jb: serde_json::Value = serde_json::from_slice(&b.stdout).expect("config set json");
    assert_eq!(ja, jb, "one write, two spellings, one JSON shape");
}

/// A carve-out must be a deliberate, documented decision — not the absence of
/// one. Every listed command carries a reason, and the reason is what a future
/// reader will weigh when they wonder why the contract has a hole in it.
#[test]
fn every_carve_out_states_its_reason() {
    for (name, why) in tasqx_cli::JSON_CARVE_OUTS {
        assert!(
            why.len() > 20,
            "carve-out `{name}` needs a real reason, got {why:?}"
        );
    }
}
