//! Verbs the Sep 2026 test audit found with no test (#82): `modify`'s
//! set-and-clear guard, `--clear` on every clearable field, and the exit code
//! of an `unsupported_version` answer.
//!
//! Every call runs the real binary on a scratch store with `--no-daemon`.

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{json, Value};

struct Store {
    dir: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Store {
        let mut dir = std::env::temp_dir();
        dir.push(format!("tasqx-gaps-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Store { dir }
    }

    fn bin(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        c.env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", self.dir.join("tasks.db"))
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
            .arg("--no-daemon");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.bin().args(args).output().expect("run tasqx")
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

    fn task(&self, r: &str) -> Value {
        serde_json::from_str(&self.ok(&["show", r, "--json"])).expect("show --json")
    }
}

/// `(field, argv that sets it)` — one row per `CLEARABLE` entry; `clearable`
/// checks clap still accepts each name.
const SETS: [(&str, &[&str]); 10] = [
    ("project", &["--project", "work"]),
    ("priority", &["--priority", "high"]),
    ("due", &["--due", "friday"]),
    ("scheduled", &["--scheduled", "friday"]),
    ("wait", &["--wait", "friday"]),
    ("recurrence", &["--repeat", "every day"]),
    ("remind", &["--remind", "friday"]),
    ("estimate", &["--estimate", "2h"]),
    ("tracked", &["--tracked", "1h"]),
    ("budget_tokens", &["--budget-tokens", "100"]),
];

fn clearable() -> Vec<String> {
    // `--clear` rejects an unknown name and lists the accepted ones.
    let out = Store::new("list").run(&["modify", "1", "--clear", "title"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    SETS.iter()
        .map(|(f, _)| f.to_string())
        .inspect(|f| assert!(err.contains(f.as_str()), "{f} missing from: {err}"))
        .collect()
}

/// Naming a field in `--clear` and also giving it a value contradicts itself;
/// the guard refuses (exit 2) and writes nothing, instead of silently dropping
/// the half of the command that lost.
#[test]
fn setting_and_clearing_one_field_is_refused_and_writes_nothing() {
    let st = Store::new("guard");
    st.ok(&["init", "work"]);
    st.ok(&["add", "subject"]);
    let rev = st.task("1")["rev"].clone();

    let mut cases: Vec<(&str, Vec<&str>)> = SETS.iter().map(|(f, a)| (*f, a.to_vec())).collect();
    // The inline-sugar spelling reaches the same guard.
    cases.push(("due", vec!["due:friday"]));
    for (field, set) in cases {
        let mut args = vec!["modify", "1"];
        args.extend(&set);
        args.extend(["--clear", field]);
        let out = st.run(&args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        assert!(
            err.contains(&format!("cannot both set and clear `{field}`")),
            "{args:?}: {err}"
        );
    }
    assert_eq!(st.task("1")["rev"], rev, "a refused modify wrote something");
}

/// Every `CLEARABLE` field, set and then cleared, read back from the store:
/// `null` for all but `tracked`, which has no null and clears to `PT0S`.
#[test]
fn clear_empties_every_clearable_field() {
    assert_eq!(clearable().len(), 10);
    for (field, set) in SETS {
        let st = Store::new(&format!("clear-{field}"));
        st.ok(&["init", "work"]);
        st.ok(&["add", "subject", "due:friday"]);

        let mut args = vec!["modify", "1"];
        args.extend(set);
        st.ok(&args);
        let before = st.task("1")[field].clone();
        assert!(
            !before.is_null() && before != json!("PT0S"),
            "{field} was not set by {set:?}: {before}"
        );

        st.ok(&["modify", "1", "--clear", field]);
        let after = st.task("1")[field].clone();
        let want = if field == "tracked" {
            json!("PT0S")
        } else {
            Value::Null
        };
        assert_eq!(after, want, "--clear {field} left {after}");
    }
}

/// The exit code of an `unsupported_version` answer is 6, reconstructed from
/// the envelope by `api_error_from_env`; no other test reaches a code past 5.
#[test]
fn an_api_major_the_build_does_not_serve_exits_six() {
    use std::io::Write;
    let st = Store::new("exit6");
    let req = json!({ "tasqx": "2", "id": "x", "method": "core.capabilities", "params": {} });
    let mut child = st
        .bin()
        .arg("api")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn api");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(req.to_string().as_bytes())
        .expect("write request");
    let out = child.wait_with_output().expect("api output");
    let env: Value = serde_json::from_slice(&out.stdout).expect("envelope");
    assert_eq!(env["error"]["code"], "unsupported_version", "{env}");
    assert_eq!(out.status.code(), Some(6), "{env}");
}
