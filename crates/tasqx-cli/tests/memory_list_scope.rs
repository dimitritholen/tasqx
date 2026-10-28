//! `tasqx memory list --include-unscoped / --unscoped-only` (D250) through the
//! real binary, on a scratch store with `--no-daemon`.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

struct Store {
    dir: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("tasqx-memlist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Store { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", self.dir.join("tasks.db"))
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
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

    /// Sorted titles of the docs `memory list --json <flags>` returns.
    fn titles(&self, flags: &[&str]) -> Vec<String> {
        let mut a = vec!["--json", "memory", "list"];
        a.extend_from_slice(flags);
        let v: Value = serde_json::from_str(&self.ok(&a)).expect("--json prints JSON");
        let mut t: Vec<String> = v["docs"]
            .as_array()
            .expect("docs")
            .iter()
            .map(|d| d["title"].as_str().expect("title").to_string())
            .collect();
        t.sort();
        t
    }
}

fn seeded(tag: &str) -> Store {
    let s = Store::new(tag);
    s.ok(&[
        "memory",
        "add",
        "p doc",
        "body",
        "--project",
        "P",
        "--standing",
    ]);
    s.ok(&[
        "memory",
        "add",
        "q doc",
        "body",
        "--project",
        "Q",
        "--standing",
    ]);
    s.ok(&["memory", "add", "free doc", "body", "--standing"]);
    s
}

#[test]
fn include_unscoped_with_a_project_lists_that_project_and_the_unscoped_docs() {
    let s = seeded("include");
    assert_eq!(
        s.titles(&["--project", "P", "--include-unscoped", "--standing"]),
        vec!["free doc", "p doc"]
    );
    assert_eq!(
        s.titles(&["--project", "P"]),
        vec!["p doc"],
        "without the flag the scope stays strict"
    );
}

#[test]
fn unscoped_only_lists_only_the_unscoped_docs() {
    let s = seeded("only");
    assert_eq!(s.titles(&["--unscoped-only"]), vec!["free doc"]);
}

#[test]
fn contradictory_flag_combinations_exit_non_zero() {
    let s = seeded("refuse");
    // The engine refuses these too; the clap wording pins that the CLI
    // attribute (`requires` / `conflicts_with_all`) is what answers first.
    for (flags, wording) in [
        (vec!["--include-unscoped"], "required arguments"),
        (
            vec!["--unscoped-only", "--project", "P"],
            "cannot be used with",
        ),
        (
            vec!["--unscoped-only", "--include-unscoped"],
            "cannot be used with",
        ),
    ] {
        let mut a = vec!["memory", "list"];
        a.extend_from_slice(&flags);
        let out = s.run(&a);
        assert!(!out.status.success(), "{flags:?} must be refused");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(wording), "{flags:?}: {err}");
    }
}
