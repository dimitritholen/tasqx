//! Guards for the shell-completion surface.
//!
//! Two halves, and they break for different reasons.
//!
//! The registration half is the pin's guard. `clap_complete`'s
//! `unstable-dynamic` feature is semver-exempt, so the version in Cargo.toml is
//! pinned with `=` and these tests are what makes moving that pin an act with
//! consequences: they run the REAL binary with `$TASQX_COMPLETE` set, for every
//! shell clap claims to support, and read what comes back. An upstream template
//! or engine change shows up here as a red build instead of as a shell that
//! quietly stops completing.
//!
//! The behaviour half is the promise that this feature is invisible when it is
//! not wanted. `complete::intercept()` is the first statement of `run()`, ahead
//! of the argv pre-pass, so it sits in front of every command tasqx has. Without
//! `$TASQX_COMPLETE` it must be a single environment lookup and a return, which
//! is only provable by driving ordinary commands through the same entry point
//! and finding them unchanged.

use std::process::Command;

/// The variable that activates the callback path.
///
/// Spelled here once and asserted against the registration scripts, because it
/// is not `clap_complete`'s default (`COMPLETE`) and the divergence is the whole
/// mitigation for the residual hazard `complete.rs` documents: a recognised
/// shell name in this variable plus a `--` in argv swallows a real command,
/// silently, exit 0. A generic `COMPLETE` is a name a stale export can plausibly
/// carry; `TASQX_COMPLETE` is not. If `CompleteEnv::var` is ever dropped, the
/// registrations start naming `COMPLETE`, `the_generic_variable_is_not_ours`
/// starts finding a swallowed command, and both fail here rather than in a
/// user's shell.
const VAR: &str = "TASQX_COMPLETE";

/// The binary, one-shot and in-process.
///
/// `--no-daemon` for the same reason `tests/help.rs` gives: `open_backend`
/// prefers a reachable daemon and the remote path never reads `TASQX_DB`, so on
/// a developer machine running `tasqx daemon` an unguarded fixture would talk to
/// the real store. It is not needed on the `$TASQX_COMPLETE` path — that path
/// never reaches `open_backend` at all — but the no-`$TASQX_COMPLETE` tests
/// here run real
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

/// `TASQX_COMPLETE=<shell> tasqx` with no further words must print a
/// registration
/// script naming the binary — that script is the entire integration, and if it
/// comes back empty or errors, completion is dead in that shell with no other
/// symptom.
#[test]
fn every_supported_shell_emits_a_registration_naming_the_binary() {
    for shell in SHELLS {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env(VAR, shell)
            .output()
            .unwrap_or_else(|e| panic!("run the binary with TASQX_COMPLETE={shell}: {e}"));

        assert!(
            out.status.success(),
            "TASQX_COMPLETE={shell} must exit 0, got {:?} with stderr {:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        let script = String::from_utf8_lossy(&out.stdout);
        assert!(
            !script.trim().is_empty(),
            "TASQX_COMPLETE={shell} produced no registration script"
        );
        assert!(
            script.contains("tasqx"),
            "the {shell} registration must name the binary it completes, got:\n{script}"
        );
        // The registration exists to make the shell call BACK into the binary
        // with the variable set; a script that never mentions it is a script
        // that registers nothing.
        assert!(
            script.contains(VAR),
            "the {shell} registration must set ${VAR} on the callback, got:\n{script}"
        );
        // The callback path is silent by policy (see `complete.rs`), and that
        // includes the registration branch: anything on stderr here lands in
        // the user's shell startup output.
        assert!(
            out.stderr.is_empty(),
            "TASQX_COMPLETE={shell} wrote to stderr: {:?}",
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

/// A `$TASQX_COMPLETE` naming a shell clap has no completer for must let the
/// command
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
        .env(VAR, "nushell")
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "add", "--", "a real task"])
        .output()
        .expect("run add with an unsupported TASQX_COMPLETE");

    assert!(
        added.status.success(),
        "an unrecognised $TASQX_COMPLETE must not stop the command, got {:?} stderr {:?}",
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
        "the task must actually be in the store; an unrecognised $TASQX_COMPLETE \
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
            .env(VAR, "nushell")
            .env("TASQX_DB", &db)
            .args(args)
            .output()
            .expect("run a read with an unsupported TASQX_COMPLETE");
        assert!(out.status.success(), "`{args:?}` must still run");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("seeded task"),
            "`{args:?}` produced no output under an unrecognised $TASQX_COMPLETE"
        );
    }

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The discriminator is read out of `Shells::builtins()`, so every name clap
/// recognises must still be treated as a callback rather than falling through to
/// the dispatcher. This is the other side of the test above: widening "let it
/// run" until it swallowed the real callbacks would trade one bug for its mirror.
///
/// `TASQX_COMPLETE=<shell>` with a `--` and words is the callback protocol, and
/// the
/// proof it was served is that candidates come back rather than the command's own
/// output.
#[test]
fn every_recognised_shell_is_still_served_as_a_callback() {
    for shell in SHELLS {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env(VAR, shell)
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

/// `clap_complete`'s default variable is NOT ours: `COMPLETE=bash` must leave
/// the binary completely alone.
///
/// This is the mitigation for the residual hazard, asserted rather than
/// described. A recognised shell name in the ACTIVE variable plus a `--` in argv
/// swallows a real command — see the test below and `complete.rs`'s module doc —
/// and nothing in `clap_complete`'s protocol lets that be told apart from a
/// genuine callback. What can be changed is how easily the environment reaches
/// that state, and the answer is the variable's name: `COMPLETE` is generic
/// enough that a half-run activation line, another clap-based tool's profile
/// entry, or an old export leaves it set, while `TASQX_COMPLETE` is a name
/// nothing else writes.
///
/// The spec's own PowerShell activation line is the concrete way it used to
/// happen: `$env:COMPLETE = "powershell"; tasqx | Out-String | Invoke-Expression;
/// Remove-Item Env:\COMPLETE` sets and clears in one statement, so an
/// interrupted paste — or a profile that throws between the two — left every
/// later `tasqx … -- …` silently dropped for the rest of the session.
///
/// Dropping `CompleteEnv::var` restores exactly that, so the assertion is on the
/// STORE: the write must land.
#[test]
fn the_generic_variable_is_not_ours() {
    let db = temp_db("generic-var");

    let added = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("COMPLETE", "bash")
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "add", "--", "a real task"])
        .output()
        .expect("run add with clap's default COMPLETE set");
    assert!(
        added.status.success(),
        "COMPLETE=bash must not stop the command, got {:?} stderr {:?}",
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
        "clap's default $COMPLETE reached `intercept` and swallowed the write; \
         `CompleteEnv::var` is what keeps it from mattering. got:\n{text}"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The residual hazard, PINNED as observed rather than claimed absent: a
/// recognised shell name in `$TASQX_COMPLETE` plus a `--` drops a real command,
/// silently, exit 0.
///
/// This test asserts a defect on purpose, and that needs its reason attached.
/// `complete.rs`'s module doc states the behaviour as the residual hazard the
/// variable rename reduces and does not remove; a doc claim with no guard under
/// it is precisely what went wrong here before. The previous version of that
/// section called the no-`--` case "the one accepted edge" and described it as
/// LOUD, while this spelling was silent and nothing was measuring it. So the
/// claim and the code are tied together: if a future clap, or a future
/// discriminator, makes this command run, this test fails and sends whoever
/// fixed it to the paragraph that must stop saying it is broken.
///
/// The assertions are the three observable facts (nothing on stdout, nothing on
/// stderr, exit 0) plus the one that hurts (the task is absent), so the pin
/// records the full shape of what a user loses rather than just the exit code.
#[test]
fn a_recognised_shell_name_with_a_separator_still_drops_the_command() {
    let db = temp_db("residual-hazard");

    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env(VAR, "bash")
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "add", "--", "a real task"])
        .output()
        .expect("run add with a recognised shell in the completion variable");

    assert_eq!(out.status.code(), Some(0), "the drop is silent, exit 0");
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "the drop writes nothing at all, which is what makes it silent; got \
         stdout {:?} stderr {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "list"])
        .output()
        .expect("list the store back");
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("a real task"),
        "the task was added, so the hazard `complete.rs` documents no longer \
         exists — update that section rather than deleting this test"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// `$SHELL`-shaped values resolve too: `Shells::completer_for_path` takes the
/// `file_stem`, so `/usr/bin/zsh` is `zsh`. Our discriminator must agree with it
/// exactly — a value we accepted but clap then rejected would take the late-error
/// arm and exit 0 on a real command, which is the fixed bug one layer down.
#[test]
fn a_shell_path_is_recognised_the_way_clap_recognises_it() {
    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env(VAR, "/usr/bin/bash")
        .env("_CLAP_COMPLETE_INDEX", "1")
        .env("_CLAP_IFS", SEP.to_string())
        .args(["--", "tasqx", "lis"])
        .output()
        .expect("run the callback with a $SHELL-shaped TASQX_COMPLETE");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        candidates(&out).iter().any(|c| c == "list"),
        "a path-shaped $TASQX_COMPLETE must resolve to its shell, got {:?}",
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
    c.env(VAR, "bash")
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
        .env(VAR, "bash")
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

/// A store with two tasks in it, and the environment that makes a callback read
/// THAT store rather than the developer's.
///
/// `$TASQX_SOCK` is not optional here, and the reason is the one `tests/help.rs`
/// gives for `--no-daemon`: the completion lookup prefers a reachable daemon and
/// the remote path never consults `$TASQX_DB`, so on a machine running `tasqx
/// daemon` this fixture would seed a temp store and then assert against the
/// user's live one. The callback path parses no flags, so `--no-daemon` is not
/// available to it — pointing the socket at a name nothing is listening on is
/// how the same guarantee is bought. `try_connect` fails immediately on it.
fn seeded(label: &str) -> (std::path::PathBuf, String) {
    let db = temp_db(label);
    for title in [SEEDED_TITLE, "a second seeded task"] {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &db)
            .args(["--no-daemon", "add", title])
            .output()
            .expect("seed the store");
        assert!(out.status.success(), "seeding {title:?} failed");
    }
    let socket = format!("tasqx-completion-no-daemon-{}-{label}", std::process::id());
    (db, socket)
}

/// Distinctive enough that finding it in the output cannot be a coincidence, and
/// free of `:` and `\` so zsh's `escape_help` leaves it byte-identical.
const SEEDED_TITLE: &str = "seeded task with a findable title";

/// The slice's whole point, driven through the REAL callback protocol: `tasqx
/// done <TAB>` against a seeded store must offer the seeded id AND its title.
///
/// **zsh rather than bash**, and that is the load-bearing choice. bash's
/// registration writes candidate VALUES only
/// (`clap_complete-4.6.7/src/env/shells.rs`), so a bash-driven test can prove
/// the id arrives and is structurally incapable of proving the title does —
/// which is the half that turns a column of integers into something a user can
/// choose from. zsh writes `value:help` separated by `$_CLAP_IFS`, so both
/// halves are observable. The protocol is otherwise identical: `$TASQX_COMPLETE`
/// names the shell, `_CLAP_COMPLETE_INDEX` carries the cursor word, and the
/// words follow a `--`.
///
/// A unit test that installs its own fixture provider proves nothing about this:
/// it would demonstrate that a completer this test wrote returns what this test
/// seeded. That mistake was already made on this branch — a fixture on
/// `List::filter` masked the shipped completer for three rebuilds — so the
/// candidates here come out of the binary that ships.
#[test]
fn a_seeded_store_completes_its_task_ids_with_their_titles() {
    let (db, socket) = seeded("task-ids");

    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env(VAR, "zsh")
        .env("TASQX_DB", &db)
        .env("TASQX_SOCK", &socket)
        .env("_CLAP_COMPLETE_INDEX", "2")
        .env("_CLAP_IFS", "\n")
        .args(["--", "tasqx", "done", ""])
        .output()
        .expect("run the zsh callback against a seeded store");

    assert_eq!(out.status.code(), Some(0), "the callback must exit 0");
    assert!(
        out.stderr.is_empty(),
        "the callback must never write to stderr, got {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let rows: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .filter(|l| !l.trim().is_empty())
        .collect();
    // Two tasks were seeded, so the ids are 1 and 2 in a fresh store.
    assert!(
        rows.iter().any(|r| r == &format!("1:{SEEDED_TITLE}")),
        "`tasqx done <TAB>` must offer the seeded id carrying its title, got {rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with("2:")),
        "every seeded task must be offered, got {rows:?}"
    );

    // The provider is attached to the id positional and not to the command, so
    // the id must NOT be offered where a task reference is not expected.
    let elsewhere = candidates(&complete_bash(1, &["tasqx", ""]));
    assert!(
        !elsewhere.iter().any(|c| c == "1"),
        "task ids leaked into the subcommand position, got {elsewhere:?}"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The escape hatch, end to end: with `$TASQX_NO_COMPLETE_LOOKUP` set, the same
/// callback against the same seeded store offers no ids — and still offers the
/// structure, because turning the value lookups off must not turn completion
/// off.
///
/// The second half is the one worth having. A short-circuit that also killed
/// flag and subcommand completion would be a far worse trade than the one the
/// variable advertises, and nothing else in the suite would notice.
#[test]
fn the_escape_hatch_turns_off_values_and_leaves_the_structure() {
    let (db, socket) = seeded("no-lookup");

    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    let out = c
        .env(VAR, "bash")
        .env("TASQX_DB", &db)
        .env("TASQX_SOCK", &socket)
        .env("TASQX_NO_COMPLETE_LOOKUP", "1")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .env("_CLAP_IFS", SEP.to_string())
        .args(["--", "tasqx", "done", ""])
        .output()
        .expect("run the callback with lookups disabled");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        candidates(&out).iter().all(|c| c != "1"),
        "$TASQX_NO_COMPLETE_LOOKUP left the store lookup running, got {:?}",
        candidates(&out)
    );

    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    let structural = c
        .env(VAR, "bash")
        .env("TASQX_DB", &db)
        .env("TASQX_SOCK", &socket)
        .env("TASQX_NO_COMPLETE_LOOKUP", "1")
        .env("_CLAP_COMPLETE_INDEX", "1")
        .env("_CLAP_IFS", SEP.to_string())
        .args(["--", "tasqx", "lis"])
        .output()
        .expect("run a structural callback with lookups disabled");
    assert!(
        candidates(&structural).iter().any(|c| c == "list"),
        "the escape hatch must disable VALUE lookups only, got {:?}",
        candidates(&structural)
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// A Tab press against a machine that has never run tasqx must leave that
/// machine exactly as it found it: exit 0, no ids offered, nothing on stderr,
/// **and no file at the path**.
///
/// Asserted against the FILESYSTEM rather than documented in a comment, because
/// the two obvious ways to lose it are both invisible from the outside:
/// `storage::open` (whose flags include `SQLITE_OPEN_CREATE`) would author a
/// database and a schema, and `db_path()` — the resolution every command uses —
/// creates the containing directory before it returns. The lookup uses
/// `open_read_only` and `db_path_read_only` for exactly these two reasons, and
/// nothing but this assertion notices if either is swapped back.
///
/// **Stdout is NOT empty, and the spec used to say it was.** Measured here:
/// `tasqx done <TAB>` with no store answers `--json --theme --socket …` — the
/// flags `done` declares, which clap's engine offers for an empty word whatever
/// the value providers do. That is correct behaviour (structure needs no store),
/// so the assertion is on the absence of ID candidates rather than on emptiness.
/// An emptiness assertion would have been a stricter-looking test that pinned a
/// property the code does not have.
#[test]
fn a_callback_against_an_absent_store_creates_nothing() {
    let db = temp_db("absent-store");
    let dir = db.parent().expect("fixture dir").to_path_buf();
    let socket = format!("tasqx-completion-absent-{}", std::process::id());

    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    let out = c
        .env(VAR, "bash")
        .env("TASQX_DB", &db)
        .env("TASQX_SOCK", &socket)
        .env("_CLAP_COMPLETE_INDEX", "2")
        .env("_CLAP_IFS", SEP.to_string())
        .args(["--", "tasqx", "done", ""])
        .output()
        .expect("run the callback against a nonexistent store");

    assert_eq!(
        out.status.code(),
        Some(0),
        "an absent store is still exit 0"
    );
    assert!(
        out.stderr.is_empty(),
        "an absent store must produce no noise; got stderr {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let offered = candidates(&out);
    assert!(
        offered.iter().all(|c| c.starts_with('-')),
        "an absent store must yield no task ids — only the flags `done` \
         declares, which need no store. got {offered:?}"
    );

    let left: Vec<String> = std::fs::read_dir(&dir)
        .expect("read the fixture dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        left.is_empty(),
        "a Tab press created {left:?} on a machine with no store"
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
