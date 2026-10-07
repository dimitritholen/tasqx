//! #1114: `annotate <ref> -` and `annotate <ref>` with a pipe on stdin read the
//! note from stdin, verbatim; `--edit` takes the same forms. `-` used to be
//! stored as a one-character note, and a pipe with no body was refused as
//! "missing body", so a long markdown note had no way in but one quoted
//! argument.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Store {
    dir: PathBuf,
}

impl Store {
    fn new(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("tasqx-annstdin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create config dir");
        let st = Store { dir };
        st.run(&["add", "Write the report"], "");
        st
    }

    /// The binary on a scratch store, `--no-daemon`, with `stdin` piped in.
    fn run(&self, args: &[&str], stdin: &str) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_CONFIG_DIR", &self.dir)
            .env("TASQX_DB", self.dir.join("tasks.db"))
            .env("TASQX_SOCK", self.dir.join("no-daemon.sock"))
            .arg("--no-daemon")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tasqx");
        c.stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        c.wait_with_output().expect("wait")
    }

    fn notes(&self) -> Vec<serde_json::Value> {
        let out = self.run(&["--json", "show", "1"], "");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        v["annotations"].as_array().cloned().unwrap_or_default()
    }
}

const MD: &str = "## Decision\n\n- use `x`\n- not `y` (naïve ✓)\n\n    code\n";

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn dash_reads_the_whole_of_stdin_verbatim() {
    let st = Store::new("dash");
    ok(&st.run(&["annotate", "1", "-"], MD));
    let notes = st.notes();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["body"], MD);
}

#[test]
fn no_body_on_a_pipe_reads_stdin() {
    let st = Store::new("pipe");
    ok(&st.run(&["annotate", "1"], MD));
    assert_eq!(st.notes()[0]["body"], MD);
}

#[test]
fn empty_stdin_is_refused_and_stores_nothing() {
    let st = Store::new("empty");
    for args in [&["annotate", "1", "-"][..], &["annotate", "1"]] {
        for input in ["", "\n  \n"] {
            let out = st.run(args, input);
            assert_eq!(out.status.code(), Some(2), "{args:?} {input:?}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("empty"), "{err}");
        }
    }
    assert!(st.notes().is_empty());
}

#[test]
fn edit_takes_its_body_from_stdin() {
    let st = Store::new("edit");
    ok(&st.run(&["annotate", "1", "first"], ""));
    let id = st.notes()[0]["id"].as_str().unwrap().to_string();
    for args in [
        vec!["annotate", "1", "--edit", &id, "-"],
        vec!["annotate", "1", "--edit", &id],
    ] {
        ok(&st.run(&args, MD));
        assert_eq!(st.notes()[0]["body"], MD);
    }
    let out = st.run(&["annotate", "1", "--edit", &id, "-"], "");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(st.notes()[0]["body"], MD);
}

#[test]
fn words_are_still_joined_with_a_space() {
    let st = Store::new("words");
    ok(&st.run(&["annotate", "1", "call", "the", "plumber"], "ignored"));
    assert_eq!(st.notes()[0]["body"], "call the plumber");
}
