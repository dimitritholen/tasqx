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

/// A store carrying two projects and a task with two tags, plus the socket that
/// keeps the callback off a developer's live daemon.
///
/// One of the projects has a SPACE in its name, and it is the point of the
/// fixture rather than decoration: it is the value a completion candidate cannot
/// deliver, because a shell inserts a candidate verbatim and then splits it.
/// `candidates::typeable_unquoted` withholds it, and
/// `a_project_name_a_shell_would_split_is_never_offered` is what makes that a
/// decision somebody can find rather than a name that quietly went missing.
fn seeded_values(label: &str) -> (std::path::PathBuf, String) {
    let db = temp_db(label);
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &db)
            .arg("--no-daemon")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("seed with {args:?}: {e}"));
        assert!(
            out.status.success(),
            "seeding {args:?} failed: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", SEEDED_PROJECT]);
    run(&["init", SEEDED_SPACED_PROJECT]);
    run(&["init", SEEDED_COLON_PROJECT]);
    run(&[
        "add",
        "a tagged task",
        "--project",
        SEEDED_PROJECT,
        "--tag",
        "api",
        "--tag",
        "docs",
    ]);
    let socket = format!("tasqx-completion-no-daemon-{}-{label}", std::process::id());
    (db, socket)
}

/// Deliverable to a shell as one word, so completion can offer it.
const SEEDED_PROJECT: &str = "work";
/// Not deliverable: a shell would split it into two words.
const SEEDED_SPACED_PROJECT: &str = "home renovation";
/// Deliverable by a shell and NOT usable after a sugar key: `project:` + this is
/// `project::x`, which `sugar::split_key` refuses as a Rust path. It is in the
/// fixture because without it
/// [`every_offered_candidate_produces_the_command_it_promises`] was
/// fixture-blind — the round trip it asserts held for every name seeded, and the
/// shipped completer was offering `project::x`, which filed the task under the
/// DEFAULT project and appended the candidate to the title at exit 0.
const SEEDED_COLON_PROJECT: &str = ":x";

/// One bash callback against a seeded store.
///
/// Separate from [`complete_bash`] only in that it points the callback at a
/// fixture store and a dead socket; the protocol is identical. `$TASQX_SOCK` is
/// not optional, for the reason `seeded` gives: the lookup prefers a reachable
/// daemon and the remote path never reads `$TASQX_DB`, so without it this suite
/// would assert against the developer's live store.
fn complete_bash_in(
    db: &std::path::Path,
    socket: &str,
    cursor: usize,
    words: &[&str],
) -> Vec<String> {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tasqx"));
    let out = c
        .env(VAR, "bash")
        .env("TASQX_DB", db)
        .env("TASQX_SOCK", socket)
        .env("_CLAP_COMPLETE_INDEX", cursor.to_string())
        .env("_CLAP_IFS", SEP.to_string())
        .arg("--")
        .args(words)
        .output()
        .expect("run the completion callback against the fixture store");
    assert_eq!(out.status.code(), Some(0), "the callback must exit 0");
    assert!(
        out.stderr.is_empty(),
        "the callback must never write to stderr, got {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    candidates(&out)
}

/// A tag that exists and uniquely matches what was typed is offered, however
/// late it sorts — on BOTH tag surfaces.
///
/// # Why this needs its own store and two hundred and fifty tags
///
/// The candidate cap is a menu bound, and it is only ever correct applied AFTER
/// the user's prefix. Applied before, it silently deletes every name past the
/// two-hundredth in sort order, which reads as "that tag does not exist" while
/// the tag sits on a task. That shipped twice on this branch, in two different
/// shapes, and neither was visible to a three-tag fixture:
///
///  * `+api<TAB>` answered nothing, because `tags_from` truncated the sorted
///    vocabulary before `prefixed` filtered it;
///  * `--tag zeb<TAB>` answered nothing while `+zeb<TAB>` answered `+zebra`,
///    because `--tag` was first attached as an `ArgValueCandidates`, which the
///    engine filters only after the provider has already capped.
///
/// Two tags do the work: one deep inside the alphabetical run and one past its
/// end. Both are asserted through the real binary, because the second failure
/// was a property of which clap seam the argument carries and no in-process test
/// of the provider could have seen it.
#[test]
fn a_tag_past_the_candidate_cap_is_still_offered_for_its_own_prefix() {
    let db = temp_db("cap-order");
    let socket = format!("tasqx-completion-cap-order-{}", std::process::id());

    // One task carrying a vocabulary larger than the cap. The filler sorts
    // first, so anything that caps before filtering loses both probes.
    let filler: Vec<String> = (0..250).map(|i| format!("a{i:03}")).collect();
    let mut args: Vec<&str> = vec!["--no-daemon", "add", "a task with many tags"];
    for tag in &filler {
        args.push("--tag");
        args.push(tag);
    }
    for probe in [LATE_TAG, LAST_TAG] {
        args.push("--tag");
        args.push(probe);
    }
    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(&args)
        .output()
        .expect("seed a task with more tags than the cap");
    assert!(
        out.status.success(),
        "seeding failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    for (surface, cursor, words) in [
        ("+ sugar", 2, &["tasqx", "add", "+zeb"][..]),
        ("--tag", 4, &["tasqx", "add", "x", "--tag", "zeb"][..]),
        ("-t", 4, &["tasqx", "add", "x", "-t", "zeb"][..]),
    ] {
        let got = complete_bash_in(&db, &socket, cursor, words);
        assert_eq!(
            got.len(),
            1,
            "`{surface}` lost {LAST_TAG:?}, which exists and uniquely matches \
             `zeb`; the cap was applied before the typed word. got {got:?}"
        );
        assert!(got[0].ends_with(LAST_TAG), "got {got:?}");
    }

    // The one inside the run, which the alphabetical cut would also have eaten.
    let got = complete_bash_in(&db, &socket, 2, &["tasqx", "add", "+api"]);
    assert_eq!(got, ["+api"], "`+api<TAB>` lost a tag that is on a task");

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// Sorts after the whole `a000..a249` filler but before [`LAST_TAG`], and is the
/// spelling `tag_names`'s own doc uses for the failure it warns about.
const LATE_TAG: &str = "api";
/// Sorts last, so it is the first thing any cap-before-filter loses.
const LAST_TAG: &str = "zebra";

/// Project names reach every surface whose value IS a project name, driven
/// through the real binary and the real protocol.
///
/// `use` is in here because it is the surface a list written from memory
/// forgets: it is the only project-valued POSITIONAL, and the value it takes is
/// spelled `name` rather than `project` in the derive tree.
/// `candidates::tests::every_project_valued_arg_offers_project_names` is what
/// keeps the set complete; this is what proves the wire works at all.
#[test]
fn a_seeded_store_completes_project_names() {
    let (db, socket) = seeded_values("projects");

    for (cursor, words) in [
        (4, &["tasqx", "add", "x", "--project", ""][..]),
        (5, &["tasqx", "modify", "1", "x", "--project", "wo"][..]),
        (2, &["tasqx", "use", "wo"][..]),
        (4, &["tasqx", "chart", "burndown", "--project", ""][..]),
    ] {
        let got = complete_bash_in(&db, &socket, cursor, words);
        assert!(
            got.iter().any(|c| c == SEEDED_PROJECT),
            "`{words:?}` must offer the seeded project, got {got:?}"
        );
    }

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The capture-sugar dispatcher, prefix by prefix, in `add`'s title position and
/// `modify`'s rest.
///
/// Every prefix is asserted in one test on purpose. The failure this whole seam
/// is built against is a dispatcher that serves some prefixes and not others —
/// four of five working "looks finished" — so the prefixes are checked together
/// and a partial answer is one red test rather than four green ones and a gap.
#[test]
fn the_capture_sugar_dispatcher_answers_by_prefix() {
    let (db, socket) = seeded_values("sugar");

    for (cursor, words) in [
        (2, &["tasqx", "add", "+"][..]),
        // `modify`'s tail is the same parser, so it must be the same menu.
        (3, &["tasqx", "modify", "1", "+"][..]),
    ] {
        let got = complete_bash_in(&db, &socket, cursor, words);
        assert_eq!(
            got,
            ["+api", "+docs"],
            "`{words:?}` must offer the seeded tags, got {got:?}"
        );
    }

    // Both spellings of the project key, each answering in the spelling it was
    // typed with — the candidate replaces the whole word, so rewriting `proj:`
    // to `project:` would change what the user chose to type.
    assert_eq!(
        complete_bash_in(&db, &socket, 2, &["tasqx", "add", "project:"]),
        ["project:work"]
    );
    assert_eq!(
        complete_bash_in(&db, &socket, 2, &["tasqx", "add", "proj:wo"]),
        ["proj:work"]
    );

    // `!` needs no store at all: the vocabulary is `Priority::SPELLINGS`.
    //
    // Three candidates, not seven, matching what `--priority` shows: the long
    // spellings are hidden by the same rule `priority_parser` hides them with.
    assert_eq!(
        complete_bash_in(&db, &socket, 2, &["tasqx", "add", "!"]),
        ["!H", "!M", "!L"]
    );

    // Hidden is not deleted, and that had to be measured rather than reasoned
    // about: clap's engine drops hidden candidates whenever a visible one
    // survives, and an earlier comment argued from that rule that hiding these
    // would make them unreachable. It does not — `complete_option` contributes
    // nothing for a non-empty word that does not start with `-`, so there is no
    // visible flag to trigger the drop. This assertion is what keeps that fact
    // pinned to the binary rather than to a comment.
    assert_eq!(
        complete_bash_in(&db, &socket, 2, &["tasqx", "add", "!hi"]),
        ["!high"]
    );
    assert_eq!(
        complete_bash_in(&db, &socket, 2, &["tasqx", "add", "!me"]),
        ["!medium", "!med"]
    );

    // And the half that keeps the menu out of the way: a bare title word is not
    // completable and must stay that way. Empty rather than "no sugar
    // candidates" because clap offers no flags for a non-empty word that does
    // not start with a dash — measured, and asserted as measured.
    for prose in ["zzq", "Ship", "project::config"] {
        let got = complete_bash_in(&db, &socket, 2, &["tasqx", "add", prose]);
        assert!(
            got.is_empty(),
            "`add {prose}<TAB>` is title text and must offer nothing, got {got:?}"
        );
    }

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// A project a shell would split into two words is NOT offered — on any surface
/// — and that is a decision, not a lost row.
///
/// The last assertion is what makes this test honest: the project is read back
/// out of the store first, so a seeding failure cannot masquerade as the
/// withholding under test. Read `candidates::typeable_unquoted` for why the
/// alternatives are worse; the short version is that the candidate would arrive
/// at the command line as `--project home renovation`, which clap reads as the
/// project `home` plus two title words, at exit 0.
#[test]
fn a_project_name_a_shell_would_split_is_never_offered() {
    let (db, socket) = seeded_values("unquotable");

    let listed = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "projects"])
        .output()
        .expect("list the projects back");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains(SEEDED_SPACED_PROJECT),
        "precondition: the store really holds {SEEDED_SPACED_PROJECT:?}"
    );

    for (cursor, words) in [
        (4, &["tasqx", "add", "x", "--project", ""][..]),
        (4, &["tasqx", "add", "x", "--project", "home"][..]),
        (2, &["tasqx", "use", ""][..]),
        (2, &["tasqx", "add", "project:"][..]),
        (2, &["tasqx", "add", "project:home"][..]),
    ] {
        let got = complete_bash_in(&db, &socket, cursor, words);
        assert!(
            got.iter().all(|c| !c.contains("home")),
            "`{words:?}` offered a project a shell would split into two words, \
             which completes to a command that files the task somewhere else. \
             got {got:?}"
        );
    }

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The property that makes the whole slice worth having: **every candidate the
/// dispatcher offers, typed as the shell would insert it, produces the command
/// the user was reaching for.**
///
/// This is the quoting rule, checked rather than asserted in prose. `sugar.rs`
/// documents ONE quoting rule shared with the read-side grammar, and its
/// consequence for completion is blunt: a candidate is inserted verbatim, so a
/// candidate containing a space arrives as two words and means something else.
/// The other half of that rule — that a candidate with no space arrives whole —
/// is what this proves, by taking the strings the callback really returned and
/// handing them to the real `add` as single argv elements, which is exactly the
/// shape a shell produces for an unquoted word.
///
/// A weaker version of this test would seed a project, offer it and assert the
/// candidate string looks right. That checks the menu against itself. The
/// assertion here is on the STORE, one command later.
#[test]
fn every_offered_candidate_produces_the_command_it_promises() {
    let (db, socket) = seeded_values("round-trip");

    let add = |sugar: &str| -> serde_json::Value {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &db)
            .args(["--no-daemon", "--json", "add", "a round trip"])
            // SPLIT, deliberately, and this is the whole fidelity of the test.
            // A shell inserts a candidate verbatim and then word-splits the line,
            // so an unquoted candidate reaches argv as one element per run of
            // non-space characters. Passing the candidate as a single element
            // would silently repair the exact failure `typeable_unquoted` exists
            // to prevent, and this test would then pass for a provider that
            // offered `project:home renovation`.
            .args(sugar.split_whitespace())
            .output()
            .expect("add a task using the completed word");
        assert!(
            out.status.success(),
            "`add {sugar:?}` was refused: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let added: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("add prints one JSON object");
        let id = added["short_id"].to_string();
        let shown = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &db)
            .args(["--no-daemon", "--json", "show", &id])
            .output()
            .expect("read the task back");
        serde_json::from_slice(&shown.stdout).expect("show prints one JSON object")
    };

    for candidate in complete_bash_in(&db, &socket, 2, &["tasqx", "add", "project:"]) {
        let task = add(&candidate);
        assert_eq!(
            task["project"], SEEDED_PROJECT,
            "the candidate {candidate:?} did not file the task under its project"
        );
        // The title must survive intact: a candidate the shell split would leave
        // its remainder in the title, which is the quiet half of that failure.
        assert_eq!(task["title"], "a round trip");
    }

    for candidate in complete_bash_in(&db, &socket, 2, &["tasqx", "add", "+"]) {
        let task = add(&candidate);
        let want = candidate.trim_start_matches('+');
        assert!(
            task["tags"]
                .as_array()
                .is_some_and(|t| t.iter().any(|v| v == want)),
            "the candidate {candidate:?} did not apply the tag {want:?}, got {:?}",
            task["tags"]
        );
        assert_eq!(task["title"], "a round trip");
    }

    for candidate in complete_bash_in(&db, &socket, 2, &["tasqx", "add", "!"]) {
        let task = add(&candidate);
        assert!(
            task["priority"].is_string(),
            "the candidate {candidate:?} set no priority"
        );
        assert_eq!(task["title"], "a round trip");
    }

    // And the flag surface, where the candidate is the whole value.
    for candidate in complete_bash_in(&db, &socket, 4, &["tasqx", "add", "x", "--project", ""]) {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &db)
            .args(["--no-daemon", "--json", "add", "flagged", "--project"])
            // Split for the same reason as above: this is what the shell hands
            // clap when the candidate is inserted unquoted.
            .args(candidate.split_whitespace())
            .output()
            .expect("add a task using the completed project");
        assert!(
            out.status.success(),
            "`--project {candidate:?}` was refused"
        );
        let added: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("add prints one JSON object");
        assert_eq!(added["project"], candidate);
    }

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

// ---- the read-side filter grammar ------------------------------------------

/// A store shaped for the filter surface: one live project, one ARCHIVED
/// project holding a task, and a task carrying four tags chosen to be traps.
///
/// Every element earns its place:
///
///  * `needs` is the tag the dash tests reach for, at five prefix lengths. It
///    exists so that "a tag exclusion completes" is a claim with content rather
///    than an empty list nobody can tell from a broken one.
///  * `blocked` is the round-trip gate's trap. `+blocked` parses — as the
///    derived blocked FLAG, not as this tag — so a completion gated on "does it
///    parse" would offer it and silently answer a different question.
///  * `-lead` is the other refusal: `-` + `-lead` is `--lead`, which the grammar
///    refuses as a mistyped flag.
///  * the archived project is the one a WRITE surface must withhold and a READ
///    surface must offer, and the difference is measured here rather than
///    reasoned about.
///  * the SPACED project is the quoting rule. A candidate is inserted verbatim
///    and then word-split, so `project:home renovation` reaches the grammar as
///    two tokens and is refused; it must never be offered.
fn seeded_filter(label: &str) -> (std::path::PathBuf, String) {
    let db = temp_db(label);
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &db)
            .arg("--no-daemon")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("seed with {args:?}: {e}"));
        assert!(
            out.status.success(),
            "seeding {args:?} failed: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", SEEDED_PROJECT]);
    run(&["init", SEEDED_SPACED_PROJECT]);
    run(&["init", ARCHIVED_PROJECT]);
    run(&["add", ARCHIVED_TASK, "--project", ARCHIVED_PROJECT]);
    // Through the capture sugar rather than `--tag`, because `-lead` cannot be
    // spelled as a flag VALUE at all: clap reads `--tag -lead` as an unknown
    // short-flag cluster and refuses it. `+-lead` is the spelling that works,
    // which is itself the asymmetry `standalone_word` documents — a dash-led
    // name is typeable welded behind a prefix and not as a whole argv element.
    run(&[
        "add",
        "a tagged task",
        "--project",
        SEEDED_PROJECT,
        "+api",
        "+needs",
        "+blocked",
        "+-lead",
        // A one-character tag whose letter clap declares as a short flag. It is
        // in the fixture because its exclusion `-h` is valid filter grammar and
        // is NOT deliverable on the CLI: `argv::prepass` deliberately leaves a
        // one-character dash token alone so `tasqx list -h` still prints help,
        // so an offered `-h` completes into the help text at exit 0 rather than
        // filtering. Nothing here seeded such a tag before, so the menu offered
        // it and no test could see it.
        SHORT_FLAG_TAG_SUGAR,
    ]);

    // `project.archive` has no CLI verb, so the one-shot JSON door is how a test
    // reaches it. The envelope shape is `dispatch::handle_envelope`'s.
    let mut child = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "api"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the one-shot api");
    {
        use std::io::Write as _;
        let stdin = child.stdin.as_mut().expect("api stdin");
        writeln!(
            stdin,
            r#"{{"tasqx":"1","id":"archive","method":"project.archive","params":{{"name":"{ARCHIVED_PROJECT}"}}}}"#
        )
        .expect("write the archive request");
    }
    let archived = child.wait_with_output().expect("run the archive request");
    let reply = String::from_utf8_lossy(&archived.stdout);
    assert!(
        reply.contains("\"ok\":true"),
        "archiving {ARCHIVED_PROJECT:?} failed: {reply}"
    );

    let socket = format!("tasqx-completion-no-daemon-{}-{label}", std::process::id());
    (db, socket)
}

/// A tag named `h`, seeded through the capture sugar.
const SHORT_FLAG_TAG_SUGAR: &str = "+h";
/// Its exclusion — valid filter grammar, and clap's help flag on the CLI.
const SHORT_FLAG_TAG_EXCLUSION: &str = "-h";

/// Archived, and therefore refused by every WRITE that takes a project.
const ARCHIVED_PROJECT: &str = "oldstuff";
/// The task that proves the archived project is still readable through a filter.
const ARCHIVED_TASK: &str = "a task left in the archive";

/// Under PowerShell the two `@` shapes are offered QUOTED, because PowerShell
/// throws an unquoted `@word` away.
///
/// # The defect this pins
///
/// PowerShell's splatting operator claims a leading `@` even in an argument to a
/// native executable, and the token does not arrive mangled — it disappears:
///
/// ```text
///   PS> tasqx list @nonsensetoken     -> every task, exit 0
///   PS> tasqx list '@nonsensetoken'   -> unknown filter token, exit 2
/// ```
///
/// So Tab-completing `@blocked` there listed EVERY task at exit 0, which is the
/// silent-drop class delivered by the menu. It shipped because the comment
/// exempting these shapes cited a measurement made with `@working` itself — a
/// token that selects pending|active and therefore returns the same rows whether
/// it survives or is dropped. A probe that cannot fail is not a measurement, and
/// the assertion below is written against a token that CAN fail.
///
/// Driven through the real binary with `$TASQX_COMPLETE=powershell`, which is
/// what the shipped registration sets. The bash spelling is asserted in the same
/// test so the special case is proven to be narrow rather than merely present:
/// eleven of the thirteen shapes were re-measured in PowerShell and pass through
/// untouched, so quoting them all would be a cost with no defect behind it.
#[test]
fn the_at_shapes_are_quoted_for_the_one_shell_that_eats_them() {
    let (db, socket) = seeded_filter("powershell-at");

    // Each shell's registration reads the candidates back its own way, so the
    // test has to as well: bash separates on `$_CLAP_IFS`, PowerShell writes one
    // candidate per LINE and never sets that variable
    // (`clap_complete-4.6.7/src/env/shells.rs`). Splitting PowerShell's output on
    // the bash sentinel returns one string with newlines in it, which compares
    // unequal to everything and would have made this test unwritable rather than
    // wrong — but only after looking like a quoting failure.
    let complete_in = |shell: &str, words: &[&str], cursor: usize| -> Vec<String> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tasqx"));
        cmd.env(VAR, shell)
            .env("TASQX_DB", &db)
            .env("TASQX_SOCK", &socket)
            .env("_CLAP_COMPLETE_INDEX", cursor.to_string());
        if shell == "bash" {
            cmd.env("_CLAP_IFS", SEP.to_string());
        }
        let out = cmd
            .arg("--")
            .args(words)
            .output()
            .expect("run the completion callback");
        assert_eq!(out.status.code(), Some(0), "the callback must exit 0");
        assert!(out.stderr.is_empty(), "the callback must not write stderr");
        let sep = if shell == "bash" { SEP } else { '\n' };
        String::from_utf8_lossy(&out.stdout)
            .split(sep)
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect()
    };

    let ps = complete_in("powershell", &["tasqx", "list", "@"], 2);
    assert_eq!(
        ps,
        ["'@working'", "'@blocked'"],
        "PowerShell throws an unquoted `@word` away, so the candidate must carry \
         its quotes or completing it silently lists every task"
    );

    // Matched on the BARE spelling — it is what the user typed. Filtering on the
    // quoted one would make this menu answer nothing the moment `@` is pressed,
    // which is the failure mode `deliverable_as_one_word` records for quoted
    // values on the engine-filtered surfaces.
    assert_eq!(
        complete_in("powershell", &["tasqx", "list", "@w"], 2),
        ["'@working'"]
    );

    // Every other shape is untouched there, so the special case is as narrow as
    // the defect. `project:` is the interesting one: it is a stub, not a value.
    let ps_all = complete_in("powershell", &["tasqx", "list", ""], 2);
    assert!(
        ps_all.iter().any(|c| c == "project:") && ps_all.iter().any(|c| c == "and"),
        "only the `@` shapes are quoted under PowerShell, got {ps_all:?}"
    );

    // And bash, which delivers `@` intact, keeps the bare spelling.
    assert_eq!(
        complete_in("bash", &["tasqx", "list", "@"], 2),
        ["@working", "@blocked"],
        "quoting these outside PowerShell would insert literal quote characters \
         into a command line that does not need them"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// A tag exclusion clap would read as its own short flag is never offered, on
/// any of the four filter commands.
///
/// # Two parsers stand between a candidate and the filter, and only one was asked
///
/// `-h` is valid filter grammar: `filter::Filter::parse` reads it as excluding
/// the tag `h`, and the JSON API applies it that way. The completion gate asked
/// exactly that parser and was satisfied. But on the CLI the token never reaches
/// the filter — `argv::prepass` leaves a one-character dash token alone on
/// purpose, so that `tasqx list -h` still prints help rather than hiding the help
/// flag behind the escape sentinel. Measured with a task tagged `h`:
/// `tasqx list -<TAB>` offered `-h`, and choosing it printed the help text at
/// exit 0 instead of filtering anything.
///
/// So the gate now asks `argv::reaches_the_filter_tail` as well, and this is the
/// end-to-end proof: the candidate is absent, and — the half that says WHY it
/// must be absent — running it does something else entirely.
///
/// The tag itself is still reachable as `+h`, which is asserted too: the rule is
/// "not excludable from the CLI", not "not completable", and withholding both
/// would be an under-offer justified by a defect in the other direction.
#[test]
fn a_tag_exclusion_clap_would_read_as_a_flag_is_never_offered() {
    let (db, socket) = seeded_filter("short-flag-tag");

    for (cursor, words) in [
        (2, &["tasqx", "list", "-"][..]),
        (2, &["tasqx", "export", "-"][..]),
        (2, &["tasqx", "watch", "-"][..]),
        (3, &["tasqx", "report", "project", "-"][..]),
    ] {
        let got = complete_bash_in(&db, &socket, cursor, words);
        // COUNTED, not searched. `-h` is legitimately in this menu once — it is
        // clap's own help flag, offered by the engine, and suppressing that
        // would be a different defect. What must not happen is the provider
        // adding a SECOND one as a tag exclusion, which is exactly how the
        // measurement read before the gate: `-V -api -h --json … -h -V`, with
        // both spellings appearing twice.
        let offered = got
            .iter()
            .filter(|c| *c == SHORT_FLAG_TAG_EXCLUSION)
            .count();
        assert_eq!(
            offered, 1,
            "`{words:?}` offers {SHORT_FLAG_TAG_EXCLUSION:?} {offered} times; \
             one is clap's help flag and any more is the provider composing a \
             tag exclusion that never reaches the filter — choosing it prints \
             help at exit 0 instead of filtering. got {got:?}"
        );
        // The seam is still alive for exclusions that DO reach the tail; without
        // this the assertion above would pass for a provider offering nothing.
        assert!(
            got.iter().any(|c| c == "-api"),
            "`{words:?}` offered no tag exclusions at all, so the check above \
             proves nothing. got {got:?}"
        );
    }

    // What the withheld candidate would have done, stated as a measurement
    // rather than as a claim in a comment.
    let ran = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "list", SHORT_FLAG_TAG_EXCLUSION])
        .output()
        .expect("run the candidate that must not be offered");
    let stdout = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success() && stdout.contains("Usage:"),
        "the premise of this guard is that `list {SHORT_FLAG_TAG_EXCLUSION}` is \
         clap's help flag; if that changed, the withholding may no longer be \
         needed. got exit {:?}, stdout {stdout:?}",
        ran.status.code()
    );

    // And the tag is still completable in the direction that works.
    let included = complete_bash_in(&db, &socket, 2, &["tasqx", "list", "+h"]);
    assert_eq!(
        included,
        [SHORT_FLAG_TAG_SUGAR],
        "the tag is not excludable from the CLI, which is not a reason to stop \
         offering it as an inclusion"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The slice, driven through the REAL binary and the REAL callback protocol
/// against a seeded store: every prefix of the filter grammar answers.
///
/// **Every prefix in one test on purpose.** The failure this seam is built
/// against is a dispatcher that serves some prefixes and not others — the tag
/// EXCLUSION is the one that breaks alone, because it is the only prefix the
/// argv pre-pass escapes, so `+tag` and `project:x` can keep working while it
/// returns nothing. Four of five working is the worst version of this defect
/// because it looks finished. Checked together, a partial answer is one red test
/// rather than four green ones and a gap.
///
/// **All four filter positionals**, read out of `argv::FILTER_COMMANDS` rather
/// than from a list written here — `report` included, whose tail is the same
/// grammar after its optional first word. A completer attached to three of the
/// four is exactly the shape `complete::escaping_drift` exists to catch, and
/// this is the surface half of that guard.
#[test]
fn a_filter_position_completes_the_shipped_grammar() {
    let (db, socket) = seeded_filter("filter-grammar");

    for command in ["list", "export", "watch"] {
        // THE one. `-ne` reaches the engine as `\u{1}ne` and a provider that
        // does not restore the dash matches nothing — silently, for every tag
        // exclusion, while the prefixes below keep working.
        for typed in ["-", "-n", "-ne", "-need", "-needs"] {
            let got = complete_bash_in(&db, &socket, 2, &["tasqx", command, typed]);
            assert!(
                got.iter().any(|c| c == "-needs"),
                "`{command} {typed}<TAB>` must offer the tag exclusion `-needs`; \
                 the escape/restore seam is not reaching the shipped provider. \
                 got {got:?}"
            );
        }

        let tags = complete_bash_in(&db, &socket, 2, &["tasqx", command, "+"]);
        assert!(
            tags.iter().any(|c| c == "+api") && tags.iter().any(|c| c == "+needs"),
            "`{command} +<TAB>` must offer the seeded tags, got {tags:?}"
        );

        let projects = complete_bash_in(&db, &socket, 2, &["tasqx", command, "project:"]);
        assert!(
            projects.iter().any(|c| c == "project:work"),
            "`{command} project:<TAB>` must offer the seeded project, got {projects:?}"
        );
        // ...and never the one a shell would split, on this surface as on the
        // write ones. What WITHHOLDS it here is not `deliverable_as_one_word`
        // but the round-trip gate, and that was measured rather than assumed:
        // with the deliverability filter removed from `project_names_from` the
        // menu is unchanged, because `project:home renovation` tokenizes into a
        // predicate plus a stray word and `Filter::parse` refuses it. Two
        // independent refusals for one hazard, which is why this assertion
        // survives either of them being weakened.
        assert!(
            projects.iter().all(|c| !c.contains("home")),
            "`{command} project:<TAB>` offered a project a shell would split, \
             got {projects:?}"
        );

        // The bare partial: the shapes the grammar accepts, out of core's
        // registries. `status:` is a stub the user goes on typing, which is why
        // the assertion is on its presence rather than on it parsing alone.
        let shapes = complete_bash_in(&db, &socket, 2, &["tasqx", command, ""]);
        for shape in ["@working", "@blocked", "and", "or", "project:", "status:"] {
            assert!(
                shapes.iter().any(|c| c == shape),
                "`{command} <TAB>` must offer the grammar shape {shape:?}, got {shapes:?}"
            );
        }

        // The one closed vocabulary, which needs no store at all.
        let statuses = complete_bash_in(&db, &socket, 2, &["tasqx", command, "status:"]);
        assert!(
            statuses.iter().any(|c| c == "status:pending"),
            "`{command} status:<TAB>` must offer the status set, got {statuses:?}"
        );

        // And a date bound offers nothing, because its vocabulary is open. The
        // assertion is that no DATE candidate appears, not that the list is
        // empty: clap still offers its own flags for a word it reads as one.
        let dates = complete_bash_in(&db, &socket, 2, &["tasqx", command, "due.before:"]);
        assert!(
            dates.iter().all(|c| !c.starts_with("due.before:")),
            "`{command} due.before:<TAB>` invented a date vocabulary, got {dates:?}"
        );
    }

    // `report`'s tail is the same grammar one word in, which is the position its
    // group_by can no longer occupy.
    let got = complete_bash_in(&db, &socket, 3, &["tasqx", "report", "project", "-ne"]);
    assert!(
        got.iter().any(|c| c == "-needs"),
        "`report project -ne<TAB>` must complete the filter tail too, got {got:?}"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// The round-trip gate through the binary: a candidate the FILTER PARSER would
/// read as something else is never offered.
///
/// `+blocked` is the case, and it is not hypothetical — the store really holds a
/// tag named `blocked`. `filter::predicate` claims that spelling for the derived
/// blocked flag before it reaches the tag branch, so a completion gated on "does
/// this parse?" offers it and Tab silently swaps "tasks tagged blocked" for
/// "tasks with an unresolved dependency", at exit 0.
///
/// `-lead` is the other half: the exclusion prefix composes `--lead`, which the
/// grammar refuses as a mistyped flag. That refusal is load-bearing rather than
/// tidiness — it is what lets `argv` tell a filter token from a flag one token at
/// a time.
///
/// Both are asserted against the SHIPPED menu rather than against the helper, so
/// a gate that exists in `candidates.rs` and is not reached by the wiring fails
/// here.
#[test]
fn a_filter_candidate_the_parser_reads_differently_is_never_offered() {
    let (db, socket) = seeded_filter("filter-round-trip");

    // Precondition: the traps really are in the store, so an empty menu cannot
    // masquerade as the withholding under test.
    let listed = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "--json", "list"])
        .output()
        .expect("list the store back");
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(
        text.contains("blocked") && text.contains("-lead"),
        "precondition: the store must carry the trap tags, got:\n{text}"
    );

    let included = complete_bash_in(&db, &socket, 2, &["tasqx", "list", "+"]);
    assert!(
        !included.iter().any(|c| c == "+blocked"),
        "`+blocked` is the derived blocked flag, not the tag `blocked` — \
         offering it answers a different question at exit 0. got {included:?}"
    );
    // The exclusion spelling of the same tag is unclaimed and must stay offered,
    // or the gate is refusing by name rather than by what the parser does.
    let excluded = complete_bash_in(&db, &socket, 2, &["tasqx", "list", "-b"]);
    assert!(
        excluded.iter().any(|c| c == "-blocked"),
        "`-blocked` is a plain tag exclusion and must be offered, got {excluded:?}"
    );

    assert!(
        !complete_bash_in(&db, &socket, 2, &["tasqx", "list", "-"])
            .iter()
            .any(|c| c == "--lead"),
        "a tag named `-lead` composes `--lead`, which the grammar refuses"
    );
    // ...and is perfectly includable, because `+-lead` is not flag-shaped.
    assert!(
        complete_bash_in(&db, &socket, 2, &["tasqx", "list", "+-"])
            .iter()
            .any(|c| c == "+-lead"),
        "`+-lead` is a legal include and withholding it is an under-offer"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// **Every value candidate the filter menu offers, typed as the shell would
/// insert it, is a filter the tool accepts and answers.**
///
/// This is the quoting rule checked rather than asserted in prose, and it is the
/// read-side twin of `every_offered_candidate_produces_the_command_it_promises`.
/// A candidate is inserted VERBATIM, so one containing a space arrives as two
/// argv elements and means something else — `filter::from_argv` joins them and
/// `project:Home Renovation` becomes a project plus a stray token, refused at
/// exit 2. The candidates are therefore split on whitespace before being handed
/// back, exactly as a shell would split them, so a provider that offered a
/// spaced value could not pass by being passed as one element.
///
/// The stub shapes (`project:`, `and`, `+`) are deliberately NOT round-tripped:
/// they are prefixes of a token rather than tokens, and pressing Enter on one is
/// a loud error by design. What is round-tripped is everything that names a
/// VALUE, plus the valueless keywords, which are complete filters on their own.
#[test]
fn every_offered_filter_candidate_parses_as_a_filter() {
    let (db, socket) = seeded_filter("filter-parses");

    let listed = |filter: &str| -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_tasqx"))
            .env("TASQX_DB", &db)
            .args(["--no-daemon", "--json", "list"])
            // SPLIT, deliberately: this is what a shell hands clap when the
            // candidate is inserted unquoted, and passing it whole would repair
            // the very failure `deliverable_as_one_word` exists to prevent.
            .args(filter.split_whitespace())
            .output()
            .expect("list with the completed filter")
    };

    // Every probe word is one the engine contributes NO flags to, so the menu is
    // this provider's alone and nothing has to be skipped — a skip list is how a
    // round-trip guard quietly stops covering the candidate that broke. `-n` and
    // `-b` rather than a bare `-` for exactly that reason: the pre-pass escapes a
    // multi-character dash token, so `complete_option` never sees a dash and
    // never offers `-h`/`-V`, while a bare `-` is `is_stdio` and does.
    let mut checked = 0;
    for typed in ["+", "-n", "-b", "project:", "status:", "@"] {
        for candidate in complete_bash_in(&db, &socket, 2, &["tasqx", "list", typed]) {
            checked += 1;
            // The quoting rule, asserted before the round trip so a spaced
            // candidate is named as such rather than showing up as a confusing
            // `unknown filter token` two lines down.
            assert!(
                !candidate.chars().any(char::is_whitespace),
                "`list {typed}<TAB>` offered {candidate:?}, which a shell splits \
                 into two words — `filter::from_argv` then joins them back and the \
                 grammar refuses the pair"
            );
            let out = listed(&candidate);
            assert!(
                out.status.success(),
                "`tasqx list {candidate}` was refused, so the menu offered a word \
                 the filter grammar does not accept: {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    assert!(
        checked >= 10,
        "only {checked} candidates were round-tripped; the fixture stopped \
         producing a menu and this guard is checking nothing"
    );

    // And the property that makes the above more than "it did not crash": a tag
    // exclusion really excludes. `-needs` must hide the tagged task and keep the
    // one in the archive, which is the same pair `filter.rs` pins in core — here
    // proven from the string the shell was actually handed.
    let excluded = complete_bash_in(&db, &socket, 2, &["tasqx", "list", "-need"]);
    assert_eq!(excluded, ["-needs"], "the fixture's premise moved");
    let out = listed("-needs");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("a tagged task") && text.contains(ARCHIVED_TASK),
        "`tasqx list -needs` must hide the tagged task and keep the others; the \
         completed token did not reach the grammar intact. got:\n{text}"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// An ARCHIVED project is offered in a FILTER and withheld on a WRITE, and the
/// asymmetry is what the receiving command does rather than a default somebody
/// picked.
///
/// Measured against the built binary, both halves. `add`, `modify` and `use`
/// refuse an archived project outright, so offering one there is a menu entry
/// whose only outcome is an error. A filter is a READ and the engine serves it:
/// `tasqx list project:oldstuff` prints the task at exit 0, so withholding the
/// name would make completion offer LESS than the command accepts — which reads
/// as "that project is gone".
///
/// The last assertion is the one that keeps this honest: the filter is actually
/// run and must return the archived task. Without it, "the name is offered"
/// could be true of a name that answers nothing.
#[test]
fn an_archived_project_is_offered_to_a_filter_and_withheld_from_a_write() {
    let (db, socket) = seeded_filter("filter-archived");

    for command in ["list", "export", "watch"] {
        let got = complete_bash_in(&db, &socket, 2, &["tasqx", command, "project:"]);
        assert!(
            got.iter().any(|c| c == "project:oldstuff"),
            "`{command} project:<TAB>` must offer the archived project — the \
             engine really does filter on it. got {got:?}"
        );
    }

    for (cursor, words) in [
        (4, &["tasqx", "add", "x", "--project", ""][..]),
        (2, &["tasqx", "use", ""][..]),
        (2, &["tasqx", "add", "project:"][..]),
    ] {
        let got = complete_bash_in(&db, &socket, cursor, words);
        assert!(
            got.iter().all(|c| !c.contains(ARCHIVED_PROJECT)),
            "`{words:?}` offered an archived project, which the command refuses \
             outright — a menu entry whose only outcome is an error. got {got:?}"
        );
    }

    let out = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "list", "project:oldstuff"])
        .output()
        .expect("filter on the archived project");
    assert!(out.status.success(), "the filter must be accepted");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(ARCHIVED_TASK),
        "the archived project answers the filter it was offered for, or the \
         candidate is a menu entry that finds nothing"
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
}

/// `report`'s first word may be a `group_by` axis; every later word may not.
///
/// Both positions, because offering filter tokens where an axis belongs and
/// offering an axis where only a filter token parses are two different wrong
/// menus, and a test of one cannot see the other. `tasqx report project status`
/// exits 2 with `unknown filter token "status"` — measured — which is what makes
/// the second assertion a correctness claim rather than a preference.
///
/// The over-offer `report_words` documents is pinned here as OBSERVED rather
/// than claimed absent: at the second word the engine still reports `arg_index`
/// 0, so the axes are offered there too. A doc claim with no guard under it is
/// what went wrong on this branch before; if upstream ever passes a true word
/// index this test fails and sends whoever fixed it to the paragraph that must
/// stop saying it is broken.
#[test]
fn a_report_group_by_is_offered_where_it_is_legal() {
    let (db, socket) = seeded_filter("report-axes");

    let first = complete_bash_in(&db, &socket, 2, &["tasqx", "report", ""]);
    for axis in ["project", "status", "priority"] {
        assert!(
            first.iter().any(|c| c == axis),
            "`report <TAB>` must offer the axis {axis:?}, got {first:?}"
        );
    }
    // ...and the filter grammar alongside, because `report +api` is a valid
    // first word too and means something different.
    assert!(
        first.iter().any(|c| c == "@working"),
        "`report <TAB>` must offer the filter grammar as well, got {first:?}"
    );
    // `project` the axis and `project:` the predicate are different tokens and
    // both are legal here.
    let both = complete_bash_in(&db, &socket, 2, &["tasqx", "report", "pro"]);
    assert_eq!(both, ["project", "project:"], "got {both:?}");

    // Past the first word an axis no longer parses, and the menu says so.
    let later = complete_bash_in(
        &db,
        &socket,
        4,
        &["tasqx", "report", "project", "+api", "pri"],
    );
    assert!(
        !later.iter().any(|c| c == "priority"),
        "an axis is offered where only a filter token parses, got {later:?}"
    );

    // The residual over-offer, pinned as measured: `arg_index` is 0 for the
    // second word too, so the axes appear there. Choosing one is a LOUD failure
    // — `unknown filter token`, exit 2 — which is why it is the accepted side of
    // the trade rather than a defect being hidden.
    let second = complete_bash_in(&db, &socket, 3, &["tasqx", "report", "project", "pri"]);
    assert!(
        second.iter().any(|c| c == "priority"),
        "`arg_index` now distinguishes the second word from the first; \
         `candidates::report_words` documents that it does not, and that \
         paragraph is now wrong. got {second:?}"
    );
    let refused = Command::new(env!("CARGO_BIN_EXE_tasqx"))
        .env("TASQX_DB", &db)
        .args(["--no-daemon", "report", "project", "priority"])
        .output()
        .expect("run report with an axis in the filter tail");
    assert!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr).contains("unknown filter token"),
        "the over-offered candidate must fail LOUDLY, which is the whole of why \
         it is tolerable; got {:?} stderr {:?}",
        refused.status.code(),
        String::from_utf8_lossy(&refused.stderr)
    );

    let _ = std::fs::remove_dir_all(db.parent().expect("fixture dir"));
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

// ---- the `completions` verb, through the real binary -----------------------
//
// Everything below drives `crates/tasqx-cli/src/complete/install.rs` as a user
// would. The module's own unit tests own the string transformation; these own
// the parts only a process can have — the exit code a script branches on, the
// bytes that reach the filesystem, and the fact that a piped stdin really does
// stop a write.
//
// NOTHING HERE MAY TOUCH A REAL STARTUP FILE. Every editing test passes an
// explicit `--profile` under a scratch directory this process owns, which is
// also the shape the docs tell PowerShell users to run.

/// A scratch profile path, seeded with `contents`, unique per call.
fn scratch_profile(label: &str, contents: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tasqx-completions-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the fixture dir");
    let path = dir.join("profile");
    std::fs::write(&path, contents).expect("seed the profile");
    path
}

/// `tasqx completions <args…>`, run through the real binary.
fn completions(args: &[&str]) -> std::process::Output {
    let mut cmd = bin();
    cmd.arg("completions");
    cmd.args(args);
    cmd.output().expect("run tasqx completions")
}

/// The default mode: one line on stdout, exit 0, nothing on stderr — the shape
/// `tasqx completions bash >> ~/.bashrc` depends on.
///
/// The line is asserted to name `$TASQX_COMPLETE` rather than clap's generic
/// `COMPLETE`, because a line carrying the wrong variable looks correct, is what
/// every clap tutorial shows, and activates nothing at all: `intercept` reads
/// `TASQX_COMPLETE` and returns immediately for anything else. The user would
/// paste it, restart their shell, press Tab, and get nothing, with no error
/// anywhere.
#[test]
fn printing_an_activation_line_is_one_line_on_stdout_at_exit_zero() {
    for shell in SHELLS {
        let out = completions(&[shell]);
        assert!(
            out.status.success(),
            "`completions {shell}` must exit 0, got {:?} stderr {:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "`completions {shell}` wrote to stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            text.lines().count(),
            1,
            "the printed form is redirected into a startup file, so it must be \
             the activation line and nothing else; got {text:?}"
        );
        assert!(
            text.contains(VAR) && text.contains("tasqx") && text.contains(shell),
            "the {shell} line must set ${VAR}, run tasqx, and name its own \
             shell; got {text:?}"
        );
    }
}

/// cmd.exe exits 2 and the message says NON-GOAL, not "unknown shell".
///
/// The distinction is the whole reason the message is tested rather than only
/// the exit code. "unknown shell `cmd`" reads as *not supported yet*, and sends
/// a Windows user looking for a newer tasqx that will never exist — cmd.exe has
/// no hook a program can register a completer against at all.
#[test]
fn an_unsupported_shell_exits_two_and_says_why_it_will_never_be_supported() {
    for spelling in ["cmd", "cmd.exe"] {
        let out = completions(&[spelling]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "`completions {spelling}` must be a usage error"
        );
        assert!(
            out.stdout.is_empty(),
            "nothing may reach stdout on a refusal: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        assert!(
            err.contains("non-goal") && err.contains("cmd.exe"),
            "the refusal must name cmd.exe as a permanent non-goal rather than \
             an unknown shell; got {err:?}"
        );
        assert!(
            err.contains("powershell"),
            "it must point at the Windows shell that does complete; got {err:?}"
        );
    }
}

/// A pipeline must not be able to edit a startup file.
///
/// `Command::output()` gives the child a piped stdin, which is exactly the
/// situation being guarded: a CI job, a `curl … | sh`, a Dockerfile layer. The
/// refusal is exit 2 and the file is untouched — asserted on the BYTES, because
/// "did not write" is the claim, not "printed something".
#[test]
fn a_non_interactive_install_refuses_and_writes_nothing() {
    let original = b"# a profile somebody has had for years\nexport EDITOR=vim\n";
    let path = scratch_profile("noninteractive", original);

    let out = completions(&["bash", "--install", "--profile", &path.to_string_lossy()]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a piped stdin must not be read as consent; stderr {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--yes"),
        "the refusal must name the flag that expresses consent on purpose: {err}"
    );
    assert_eq!(
        std::fs::read(&path).expect("read the profile back"),
        original,
        "the profile was edited by an invocation nobody confirmed"
    );

    let _ = std::fs::remove_dir_all(path.parent().expect("fixture dir"));
}

/// Installing twice leaves exactly ONE block, and uninstalling restores the
/// file byte for byte — across every file shape that has ever broken an
/// installer of this kind.
///
/// The shapes are the point, and each is here because it fails differently:
///
///  * **CRLF** is `$PROFILE` on Windows, the whole PowerShell half of this
///    feature. A block written with LF into a CRLF file also invites the next
///    editor that opens it to normalise the entire file, rewriting bytes tasqx
///    never touched.
///  * **A UTF-8 BOM** is what Windows editors put at the front of a profile. It
///    is content, not framing, and must come back untouched.
///  * **Text that merely LOOKS like the block** — an `echo` of the marker, and
///    a conda block using the same `>>> … >>>` shape — is what separates a
///    whole-line match from a `contains`. A `contains` here would cut from the
///    middle of somebody's script.
///  * **An empty file** is fish's ordinary case, and the one where an appended
///    block must not gain a leading blank line.
///  * **No trailing newline** is the one shape that does NOT round-trip
///    exactly, and it is here to pin that rather than to hide it: `"A"` and
///    `"A\n"` install to identical bytes, so no removal can tell them apart.
///    The file gains one byte at its end and loses nothing —
///    `complete::install::with_block` documents why encoding the original state
///    inside the block was rejected.
///
/// Compared as bytes, not as strings, because every failure this test exists to
/// catch is a byte one.
#[test]
fn install_is_idempotent_and_uninstall_restores_the_bytes() {
    let shapes: [(&str, &[u8], bool); 6] = [
        (
            "plain",
            b"export PATH=$PATH:/opt/bin\nalias ll='ls -l'\n",
            true,
        ),
        ("crlf", b"$env:EDITOR = 'vim'\r\nSet-Alias ll gci\r\n", true),
        ("bom", "\u{feff}# my profile\r\n".as_bytes(), true),
        (
            "lookalike",
            b"echo \"# >>> tasqx completions >>>\"\n\
              # >>> conda initialize >>>\n\
              eval \"$(conda shell.bash hook)\"\n\
              # <<< conda initialize <<<\n",
            true,
        ),
        ("empty", b"", true),
        // The documented exception: exact restore is impossible, so the
        // assertion below is the weaker, TRUE one.
        ("no-final-newline", b"alias ll='ls -l'", false),
    ];

    for (label, original, exact) in shapes {
        let path = scratch_profile(label, original);
        let profile = path.to_string_lossy().into_owned();

        for pass in 1..=2 {
            let out = completions(&["bash", "--install", "--profile", &profile, "--yes"]);
            assert!(
                out.status.success(),
                "{label}: install pass {pass} failed: {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let installed = std::fs::read_to_string(&path).expect("read the installed profile");
        // Counted by WHOLE LINE, not with `contains`. The `lookalike` shape
        // holds a line that echoes the marker, and a substring count reports two
        // blocks where the file has one — this assertion failed on its first run
        // for exactly that reason, which is the same distinction the
        // implementation has to make and the reason the shape is in this list.
        let blocks = installed
            .lines()
            .filter(|l| l.trim() == "# >>> tasqx completions >>>")
            .count();
        assert_eq!(
            blocks, 1,
            "{label}: two installs must leave ONE block, got:\n{installed}"
        );
        assert!(
            installed.contains("source <(TASQX_COMPLETE=bash tasqx)"),
            "{label}: the activation line is missing:\n{installed}"
        );

        let out = completions(&["bash", "--uninstall", "--profile", &profile, "--yes"]);
        assert!(
            out.status.success(),
            "{label}: uninstall failed: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let restored = std::fs::read(&path).expect("read the restored profile");
        match exact {
            true => assert_eq!(
                restored, original,
                "{label}: uninstall must restore the file byte for byte"
            ),
            // Everything the user wrote is still there, in order, and the file
            // differs from the original by exactly the one newline the install
            // had to add. Asserted precisely, so a REGRESSION — two bytes, or a
            // reordering — is still a failure.
            false => {
                let mut expected = original.to_vec();
                expected.push(b'\n');
                assert_eq!(
                    restored, expected,
                    "{label}: the documented exception is one added newline and \
                     nothing else"
                );
            }
        }

        let _ = std::fs::remove_dir_all(path.parent().expect("fixture dir"));
    }
}

/// An uninstall that removed nothing must not report success (D33).
///
/// The case where it matters is the likely one: a `--uninstall` aimed at the
/// wrong file. A cheerful exit 0 there tells the user the activation line is
/// gone while it sits in another file still turning completion on. And the file
/// must not be rewritten at all — an identical rewrite still moves the mtime,
/// which is a visible event in a directory people sync and back up.
#[test]
fn uninstalling_nothing_fails_loudly_and_leaves_the_file_alone() {
    let original = b"# nothing of ours in here\n";
    let path = scratch_profile("nothing-to-remove", original);

    let out = completions(&[
        "bash",
        "--uninstall",
        "--profile",
        &path.to_string_lossy(),
        "--yes",
    ]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "removing nothing is not_found, not a cheerful zero; stderr {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("nothing was removed"),
        "the message must say plainly that nothing changed: {err}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let _ = std::fs::remove_dir_all(path.parent().expect("fixture dir"));
}

/// PowerShell's `--install` refuses to guess `$PROFILE` and hands over the
/// command that cannot be wrong.
///
/// `$PROFILE` is a PowerShell variable, not an environment variable, and it
/// differs between Windows PowerShell 5.1, PowerShell 7 and the ISE — which
/// `clap_complete` collapses to one name, so the host is not knowable here even
/// in principle. A guess writes an activation line into a file PowerShell never
/// reads: completion silently not working, with nothing pointing back at the
/// cause.
#[test]
fn powershell_refuses_to_guess_the_profile_and_names_what_to_run() {
    let out = completions(&["powershell", "--install", "--yes"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--profile $PROFILE"),
        "the refusal must hand over the working command, or the user has a dead \
         end: {err}"
    );

    // And with the path supplied, the same shell installs normally — the
    // refusal is about the GUESS, not about PowerShell.
    let path = scratch_profile("pwsh", b"# profile\r\n");
    let out = completions(&[
        "powershell",
        "--install",
        "--profile",
        &path.to_string_lossy(),
        "--yes",
    ]);
    assert!(
        out.status.success(),
        "an explicit --profile must install: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("$env:TASQX_COMPLETE = \"powershell\""),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("fixture dir"));
}
