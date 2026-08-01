//! Guards for the shell-completion surface.
//!
//! Two halves, and they break for different reasons.
//!
//! The registration half is the pin's guard. `clap_complete`'s
//! `unstable-dynamic` feature is semver-exempt, so the version in Cargo.toml is
//! pinned with `=` and these tests are what makes moving that pin an act with
//! consequences: they run the REAL binary with `$COMPLETE` set, for every shell
//! clap claims to support, and read what comes back. An upstream template or
//! engine change shows up here as a red build instead of as a shell that
//! quietly stops completing.
//!
//! The behaviour half is the promise that this feature is invisible when it is
//! not wanted. `complete::intercept()` is the first statement of `run()`, ahead
//! of the argv pre-pass, so it sits in front of every command tasqx has. Without
//! `$COMPLETE` it must be a single environment lookup and a return, which is
//! only provable by driving ordinary commands through the same entry point and
//! finding them unchanged.

use std::process::Command;

/// The binary, one-shot and in-process.
///
/// `--no-daemon` for the same reason `tests/help.rs` gives: `open_backend`
/// prefers a reachable daemon and the remote path never reads `TASQX_DB`, so on
/// a developer machine running `tasqx daemon` an unguarded fixture would talk to
/// the real store. It is not needed on the `$COMPLETE` path — that path never
/// reaches `open_backend` at all — but the no-`$COMPLETE` tests here run real
/// commands, and one helper for both keeps the difference from mattering.
fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.arg("--no-daemon");
    c
}

/// Every shell `clap_complete` 4.6.7 has a built-in `EnvCompleter` for
/// (`env/mod.rs`: `Shells::builtins()`), spelled the way it spells them.
///
/// Listed here rather than derived because it is the OTHER side of the pin: if
/// upstream drops a shell or renames one, this list stopping to match is the
/// signal. Deriving it from the same table that produces the output would make
/// the guard agree with the implementation by construction.
const SHELLS: [&str; 5] = ["bash", "elvish", "fish", "powershell", "zsh"];

/// `COMPLETE=<shell> tasqx` with no further words must print a registration
/// script naming the binary — that script is the entire integration, and if it
/// comes back empty or errors, completion is dead in that shell with no other
/// symptom.
#[test]
fn every_supported_shell_emits_a_registration_naming_the_binary() {
    for shell in SHELLS {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("COMPLETE", shell)
            .output()
            .unwrap_or_else(|e| panic!("run the binary with COMPLETE={shell}: {e}"));

        assert!(
            out.status.success(),
            "COMPLETE={shell} must exit 0, got {:?} with stderr {:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let script = String::from_utf8_lossy(&out.stdout);
        assert!(
            !script.trim().is_empty(),
            "COMPLETE={shell} produced no registration script"
        );
        assert!(
            script.contains("tasqx"),
            "the {shell} registration must name the binary it completes, got:\n{script}"
        );
        // The registration exists to make the shell call BACK into the binary
        // with the variable set; a script that never mentions it is a script
        // that registers nothing.
        assert!(
            script.contains("COMPLETE"),
            "the {shell} registration must set $COMPLETE on the callback, got:\n{script}"
        );
        // The callback path is silent by policy (see `complete.rs`), and that
        // includes the registration branch: anything on stderr here lands in
        // the user's shell startup output.
        assert!(
            out.stderr.is_empty(),
            "COMPLETE={shell} wrote to stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A temp store path unique to this process and call site.
fn temp_db(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tasqx-completion-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the fixture dir");
    dir.join("tasks.db")
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A `$COMPLETE` naming a shell clap has no completer for must let the command
/// RUN. It is not a callback and cannot be one.
///
/// This test previously asserted the opposite, and the opposite was a bug. The
/// discriminator used to be "argv contains `--`", but `--` is the documented way
/// to pass a leading-dash value (`tasqx add -- "-not a flag"`), so an ordinary
/// command was classified as a callback and the error arm swallowed it. Measured
/// against a real store before the fix: `COMPLETE=nushell tasqx add -- "a real
/// task"` printed nothing, exited 0, and **the task was never added** — while the
/// comment above that arm claimed to be protecting against exactly this.
///
/// `nushell` is not a strawman. `complete.rs`'s design doc names it as a known
/// gap, so a user who reads the docs and tries it anyway is the expected way to
/// arrive here, and losing their writes is the expected cost.
///
/// The assertion is on the STORE, not on stdout: a command that printed its
/// usual line but wrote nothing would be the same defect wearing a disguise.
#[test]
fn an_unrecognised_shell_name_lets_the_command_run() {
    let db = temp_db("unrecognised");

    let added = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("COMPLETE", "nushell")
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "add", "--", "a real task"])
        .output()
        .expect("run add with an unsupported COMPLETE");

    assert!(
        added.status.success(),
        "an unrecognised $COMPLETE must not stop the command, got {:?} stderr {:?}",
        added.status.code(),
        String::from_utf8_lossy(&added.stderr)
    );

    let listed = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "list"])
        .output()
        .expect("list the store back");
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(
        text.contains("a real task"),
        "the task must actually be in the store; an unrecognised $COMPLETE \
         swallowed the write. got:\n{text}"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The same property for a READ command, and for the spelling that carries no
/// `--` at all — the two halves of the old heuristic, so neither can come back.
///
/// A read is worth its own case because its failure is quieter than a write's:
/// `list` swallowed into exit 0 prints nothing, which is indistinguishable from
/// an empty store to anyone not looking at the store.
#[test]
fn an_unrecognised_shell_name_does_not_suppress_a_read_either() {
    let db = temp_db("unrecognised-read");
    let seed = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "add", "seeded task"])
        .output()
        .expect("seed the store");
    assert!(seed.status.success());

    for args in [
        &["--no-daemon", "list"][..],
        &["--no-daemon", "list", "--", "@working"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("COMPLETE", "nushell")
            .env("TASQX_DB", &db)
            .args(args)
            .output()
            .expect("run a read with an unsupported COMPLETE");
        assert!(out.status.success(), "`{args:?}` must still run");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("seeded task"),
            "`{args:?}` produced no output under an unrecognised $COMPLETE"
        );
    }

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The discriminator is read out of `Shells::builtins()`, so every name clap
/// recognises must still be treated as a callback rather than falling through to
/// the dispatcher. This is the other side of the test above: widening "let it
/// run" until it swallowed the real callbacks would trade one bug for its mirror.
///
/// `COMPLETE=<shell>` with a `--` and words is the callback protocol, and the
/// proof it was served is that candidates come back rather than the command's own
/// output.
#[test]
fn every_recognised_shell_is_still_served_as_a_callback() {
    for shell in SHELLS {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("COMPLETE", shell)
            .env("_CLAP_COMPLETE_INDEX", "1")
            .args(["--", "tasqx", "lis"])
            .output()
            .unwrap_or_else(|e| panic!("run the {shell} callback: {e}"));

        assert_eq!(
            out.status.code(),
            Some(0),
            "the {shell} callback must exit 0"
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("list"),
            "the {shell} callback must have been served candidates, not run a \
             command; got:\n{text}"
        );
    }
}

/// `$SHELL`-shaped values resolve too: `Shells::completer_for_path` takes the
/// `file_stem`, so `/usr/bin/zsh` is `zsh`. Our discriminator must agree with it
/// exactly — a value we accepted but clap then rejected would take the late-error
/// arm and exit 0 on a real command, which is the fixed bug one layer down.
#[test]
fn a_shell_path_is_recognised_the_way_clap_recognises_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("COMPLETE", "/usr/bin/bash")
        .env("_CLAP_COMPLETE_INDEX", "1")
        .env("_CLAP_IFS", SEP.to_string())
        .args(["--", "tasqx", "lis"])
        .output()
        .expect("run the callback with a $SHELL-shaped COMPLETE");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        candidates(&out).iter().any(|c| c == "list"),
        "a path-shaped $COMPLETE must resolve to its shell, got {:?}",
        candidates(&out)
    );
}

/// Drive one bash completion callback and hand back its raw stdout.
///
/// `words` is the command line as the shell sees it, program name included —
/// bash's registration passes `words=("${COMP_WORDS[@]}")` after a `--`, and
/// `cursor` is the `COMP_CWORD` index of the word being completed. Both are part
/// of the protocol rather than incidental: with the index left at its default of
/// 0, clap completes the PROGRAM NAME and every candidate assertion below would
/// pass or fail for the wrong reason.
fn complete_bash(cursor: usize, words: &[&str]) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    c.env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", cursor.to_string())
        // The registration sets `IFS=$'\013'` and forwards it as `_CLAP_IFS`,
        // and the separator between candidates is read back out of that
        // variable. Set here so the fixture reproduces the real protocol rather
        // than the newline fallback a bare invocation happens to take.
        .env("_CLAP_IFS", SEP.to_string())
        .arg("--")
        .args(words);
    c.output().expect("run the completion callback")
}

/// The candidate separator bash's registration installs: vertical tab, chosen
/// upstream because no candidate can contain it.
const SEP: char = '\u{b}';

/// Split bash candidate output into individual candidates.
fn candidates(out: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .split(SEP)
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// The completion callback parses the words it is given against the same
/// `clap::Command` the argv pre-pass exists for, so it inherits the same
/// problem: `-needs` is filter grammar that looks exactly like a flag.
///
/// This is the test the pre-pass fix exists for, and the numbers in it are
/// MEASURED against a build with the pre-pass removed, not imagined. Without it,
/// completing `-needs` in `list`'s filter position offers `-needsh` and
/// `-needsV`: clap reads the word as an unknown short-flag cluster and helpfully
/// suggests appending the short flags it does know. Those two strings are
/// unusable — neither is a tag exclusion and neither is a flag — so their
/// absence is the assertion, and it is one no other guard in the suite makes,
/// because every existing guard exercises the parse path rather than the
/// completion path.
#[test]
fn a_filter_token_beginning_with_a_dash_does_not_derail_the_completer() {
    let out = complete_bash(2, &["tasqx", "list", "-needs"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the callback must exit 0 mid-filter"
    );
    assert!(
        out.stderr.is_empty(),
        "the callback must never write to stderr, got {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    for junk in candidates(&out) {
        assert!(
            !junk.starts_with("-needs"),
            "`-needs` was read as a short-flag cluster and grew into {junk:?} — \
             the argv pre-pass is not reaching the completion engine"
        );
    }

    // And the word after it: the engine must still be inside `list`, which it
    // shows by offering `list`'s flags rather than erroring out.
    let after = complete_bash(3, &["tasqx", "list", "-needs", ""]);
    assert_eq!(after.status.code(), Some(0));
    assert!(
        candidates(&after).iter().any(|c| c == "--json"),
        "after a tag exclusion the engine must still be completing `list`, got {:?}",
        candidates(&after)
    );
}

/// Subcommand completion is the free win the whole feature rides on: no value
/// providers are attached yet, and `tasqx lis<TAB>` must already offer `list`.
#[test]
fn a_partial_subcommand_completes_from_claps_own_tree() {
    let out = complete_bash(1, &["tasqx", "lis"]);
    assert!(
        candidates(&out).iter().any(|c| c == "list"),
        "`tasqx lis<TAB>` must offer `list`, got {:?}",
        candidates(&out)
    );
}

/// Thirty-odd aliases come free from clap's tree, and they behave in a way
/// worth pinning because it is not the obvious one and the README must not
/// over-promise it.
///
/// `#[command(alias = "…")]` declares a HIDDEN alias, and clap's engine emits
/// those with `hide(true)`, which means they surface only when no VISIBLE
/// candidate matches the partial word. Measured on this tree: `tasqx x<TAB>`
/// offers `x` (nothing visible starts with x), `tasqx ls<TAB>` offers `ls`, and
/// `tasqx mod<TAB>` offers `modify` rather than `mod` — because the canonical
/// name matched first. Every one of those completes to something that runs,
/// which is the property that matters; "every alias is listed" is not true and
/// must not be claimed.
#[test]
fn aliases_complete_when_no_canonical_name_claims_the_prefix() {
    for (typed, want) in [("x", "x"), ("ls", "ls"), ("mod", "modify")] {
        let got = candidates(&complete_bash(1, &["tasqx", typed]));
        assert!(
            got.iter().any(|c| c == want),
            "`tasqx {typed}<TAB>` must offer {want:?}, got {got:?}"
        );
    }
    // The bare prompt lists the canonical surface only — hidden aliases stay
    // hidden while anything visible matches, which is what keeps the first Tab
    // from printing seventy entries.
    let bare = candidates(&complete_bash(1, &["tasqx", ""]));
    assert!(bare.iter().any(|c| c == "list"), "got {bare:?}");
    assert!(
        !bare.iter().any(|c| c == "ls"),
        "the bare prompt must not list hidden aliases, got {bare:?}"
    );
}

/// A closed value set declared to clap is the only thing a completion engine can
/// offer for a flag, and this is the end of that wire: through the real binary,
/// into the real shell protocol.
///
/// The hidden-spelling behaviour in the middle assertion is measured, not
/// assumed. clap's engine filters possible values with a case-SENSITIVE
/// `starts_with`, so `--priority m<TAB>` matches `medium` and `med` but not `M`,
/// and the hidden pair surfaces because nothing visible matched. Every candidate
/// that comes back parses, which is the property that matters; "you always get
/// the canonical letter" is not true and should not be promised.
#[test]
fn closed_value_sets_reach_the_shell() {
    assert_eq!(
        candidates(&complete_bash(4, &["tasqx", "add", "x", "--priority", ""])),
        ["H", "M", "L"],
        "the bare prompt offers the canonical letters only"
    );
    assert_eq!(
        candidates(&complete_bash(
            4,
            &["tasqx", "add", "x", "--priority", "hi"]
        )),
        ["high"],
        "a partial word no canonical letter matches surfaces the long spelling"
    );
    assert_eq!(
        candidates(&complete_bash(
            5,
            &["tasqx", "memory", "search", "q", "--scope", ""]
        )),
        ["all", "docs", "annotations"],
        "the scope vocabulary comes from the engine's MEMORY_SCOPES"
    );
}

/// A `ValueHint` is the only thing that makes a path arg completable: without
/// one, clap's engine takes the `ValueHint::Unknown` arm and offers NOTHING —
/// not a wrong answer, no answer. `command.rs`'s drift guard keeps the hints
/// attached; this proves a hint actually produces filenames through the binary.
#[test]
fn a_path_arg_completes_real_filenames() {
    let dir = std::env::temp_dir().join(format!("tasqx-complete-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the fixture dir");
    std::fs::write(dir.join("report-target.html"), b"").expect("write a file to find");

    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    let out = c
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "3")
        .env("_CLAP_IFS", SEP.to_string())
        // `complete_path` resolves a relative partial word against the working
        // directory clap is handed, which is this process's cwd.
        .current_dir(&dir)
        .args(["--", "tasqx", "docs", "--out", "report-"])
        .output()
        .expect("run the completion callback");

    assert!(
        candidates(&out).iter().any(|c| c == "report-target.html"),
        "`docs --out report-<TAB>` must offer the file, got {:?}",
        candidates(&out)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole feature is a no-op without the variable, and that is a property of
/// the entry point of EVERY command — `intercept()` runs before the argv
/// pre-pass, so a mistake here breaks the tool rather than the feature.
#[test]
fn without_the_variable_the_binary_behaves_exactly_as_before() {
    let version = bin().arg("--version").output().expect("run --version");
    assert!(version.status.success());
    let text = String::from_utf8_lossy(&version.stdout);
    assert!(text.contains("tasqx"), "got {text:?}");

    // A filter command with a leading-dash token: the pre-pass still runs on the
    // ordinary path, unchanged, and `intercept()` returned without touching it.
    let db = std::env::temp_dir().join(format!("tasqx-complete-off-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let listed = bin()
        .env("TASQX_DB", &db)
        .args(["list", "-needs"])
        .output()
        .expect("run list with a tag exclusion");
    assert!(
        listed.status.success(),
        "`list -needs` must still parse, got stderr {:?}",
        String::from_utf8_lossy(&listed.stderr)
    );

    // And an ordinary parse error is still a parse error, on stderr, non-zero:
    // the silence policy belongs to the callback path and must not have leaked
    // onto the command path.
    let bad = bin()
        .env("TASQX_DB", &db)
        .args(["list", "--bogus"])
        .output()
        .expect("run list with an unknown flag");
    assert!(!bad.status.success(), "an unknown flag must still fail");
    assert!(
        !bad.stderr.is_empty(),
        "an unknown flag must still be reported on stderr"
    );

    let _ = std::fs::remove_file(&db);
}
