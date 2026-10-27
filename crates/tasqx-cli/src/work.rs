//! `tasqx work` (D245): launch the user's own agent CLI bound to a task.
//!
//! tasqx holds no AI and no credentials here. It runs the official `claude`
//! or `codex` binary found on PATH as a child on this terminal, with the
//! task's start context passed the way each CLI documents, and waits. Every
//! question comes first and writes nothing; the claim and the timer are taken
//! right before the spawn and given back after it, whatever the child did.

use super::*;

/// The agent CLIs `tasqx work` can launch, in the order a prompt offers them;
/// the first one found on PATH is the default under Enter.
pub const AGENTS: &[&str] = &["claude", "codex"];

/// D236's longest lease: one session of agent work holds the task this long
/// at most, and a crashed `tasqx work` frees it after.
const LEASE: &str = "24h";

/// How the agent is decided before anything is asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Use(&'static str),
    /// More than one is installed and none is remembered: ask among these.
    Ask(Vec<&'static str>),
}

/// `--agent`, else the remembered `work.agent` when it is still installed,
/// else the one agent on PATH, else a question among the installed ones.
pub(crate) fn choose_agent(
    flag: Option<&str>,
    remembered: Option<&str>,
    installed: &[&'static str],
) -> Result<Choice, ApiError> {
    if let Some(f) = flag {
        return installed
            .iter()
            .find(|a| **a == f)
            .map(|a| Choice::Use(a))
            .ok_or_else(|| ApiError::not_found(format!("`{f}` is not on PATH"), None));
    }
    if let Some(a) = remembered.and_then(|r| installed.iter().find(|a| **a == r)) {
        return Ok(Choice::Use(a));
    }
    match installed {
        [] => Err(ApiError::not_found(
            "neither `claude` nor `codex` is on PATH — install one of them to use `tasqx work`",
            None,
        )),
        [one] => Ok(Choice::Use(one)),
        many => Ok(Choice::Ask(many.to_vec())),
    }
}

/// The agents found on PATH, in [`AGENTS`] order.
fn installed() -> Vec<&'static str> {
    AGENTS
        .iter()
        .copied()
        .filter(|a| on_path(a).is_some())
        .collect()
}

/// Where `name` is on PATH.
///
/// shortcut: on Windows only `name.exe` and `name.cmd` are tried, not the
/// whole PATHEXT list; widen it if an agent ships another wrapper.
fn on_path(name: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd")]
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

fn setting() -> &'static config::Setting {
    config::find("work.agent").expect("work.agent is a setting")
}

/// A ref is a short id or a UUID. Anything else is words, which `work` does
/// not search yet.
fn is_ref(r: &str) -> bool {
    let r = r.trim();
    (!r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
        || (r.len() == 36 && r.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
}

fn ask_refusal(options: &[&str]) -> ApiError {
    ApiError::bad_request(format!(
        "`tasqx work` would ask which agent to launch, and this is not a terminal: \
         pass `--agent {}` (or one of {}), or set `work.agent` once",
        options[0],
        options.join(", ")
    ))
}

/// Everything `work` refuses without a store and without asking, so it runs
/// above the store-open (a refusal creates no database, as `pick`'s does
/// not). Off a terminal, every question is a refusal naming what to pass.
pub(crate) fn refusal(r: Option<&str>, flag: Option<&str>, interactive: bool) -> Option<ApiError> {
    if let Some(r) = r.filter(|r| !is_ref(r)) {
        return Some(ApiError::bad_request(format!(
            "`tasqx work` takes a task id, and {r:?} is not one — add the work with \
             `tasqx add` first, then `tasqx work <id>`"
        )));
    }
    let remembered = config::toml_value(None, setting());
    match choose_agent(flag, remembered.as_deref(), &installed()) {
        Err(e) => return Some(e),
        Ok(Choice::Ask(options)) if !interactive => return Some(ask_refusal(&options)),
        Ok(_) => {}
    }
    if r.is_none() && !interactive {
        return Some(ApiError::bad_request(
            "`tasqx work` would open the task browser, and this is not a terminal: \
             name the task with `tasqx work <ref>`",
        ));
    }
    None
}

/// The answer to the one agent question: Enter is the first option, a name
/// or its number picks it, anything else (`q`, EOF) cancels.
pub(crate) fn parse_answer(line: &str, options: &[&'static str]) -> Option<&'static str> {
    let a = line.trim();
    if a.is_empty() {
        return options.first().copied();
    }
    if let Ok(n) = a.parse::<usize>() {
        return n.checked_sub(1).and_then(|i| options.get(i).copied());
    }
    options.iter().copied().find(|o| *o == a)
}

/// One line on stderr, one line read: the single pre-launch question.
fn ask_agent(options: &[&'static str]) -> Option<&'static str> {
    let listed: Vec<String> = options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}) {o}", i + 1))
        .collect();
    eprint!(
        "Launch which agent? {}  [Enter: {}, q: cancel] ",
        listed.join("  "),
        options[0]
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => parse_answer(&line, options),
    }
}

/// The pick browser over `@working`, as a chooser with the cursor on
/// `task.next`'s answer. `None` when the reader left.
fn choose_task(be: &mut Backend, ctx: &Ctx) -> Result<Option<i64>, ApiError> {
    let filter = "@working";
    let listed = pick_candidates(be, filter, Some(tasqx_core::engine::task::SHELL_HOLDER))?;
    let rows = pick_rows(&listed);
    if rows.is_empty() {
        return Err(no_candidates(filter));
    }
    let next = be.call("task.next", &json!({}))?["task"]["short_id"].as_i64();
    let mut app = tui::pick::App::new(rows, ctx, filter, crate::clock::now()).choosing(next);
    tui::with_terminal(|term| pick_loop(term, be, &mut app))
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))
}

/// Leaving a question: exit 0, nothing written, nothing printed.
fn cancelled() -> CmdOutcome {
    Ok((json!({ "launched": false }), String::new()))
}

/// `tasqx work [REF] [--agent claude|codex]`.
pub(crate) fn run_work(
    be: &mut Backend,
    ctx: &Ctx,
    r: Option<String>,
    flag: Option<String>,
) -> CmdOutcome {
    let remembered = config::toml_value(None, setting());
    let decided = decide(
        be,
        r,
        flag.as_deref(),
        remembered.as_deref(),
        &installed(),
        tui::is_interactive(&ctx.caps),
        |be| choose_task(be, ctx),
        ask_agent,
    )?;
    let Some((task_ref, agent, remember)) = decided else {
        return cancelled();
    };
    if remember {
        if let Err(e) = config::write_value(None, setting(), agent) {
            note_after_output(format!("note: could not remember the agent: {}", e.message));
        }
    }
    launch(be, ctx, &task_ref, agent)
}

/// Every question `work` asks, before anything is written: the task (the
/// browser, when no ref was given) and the agent. `None` when the reader
/// left one; otherwise the ref, the agent, and whether the agent was the
/// answer to the question (and so is to be remembered).
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide(
    be: &mut Backend,
    r: Option<String>,
    flag: Option<&str>,
    remembered: Option<&str>,
    installed: &[&'static str],
    interactive: bool,
    pick: impl FnOnce(&mut Backend) -> Result<Option<i64>, ApiError>,
    ask: impl FnOnce(&[&'static str]) -> Option<&'static str>,
) -> Result<Option<(String, &'static str, bool)>, ApiError> {
    let task_ref = match r {
        Some(r) => r.trim().to_string(),
        None => match pick(be)? {
            Some(id) => id.to_string(),
            None => return Ok(None),
        },
    };
    Ok(match choose_agent(flag, remembered, installed)? {
        Choice::Use(a) => Some((task_ref, a, false)),
        Choice::Ask(options) if !interactive => return Err(ask_refusal(&options)),
        Choice::Ask(options) => ask(&options).map(|a| (task_ref, a, true)),
    })
}

/// The start context: who launched the agent and on what, the task's brief
/// (`tasqx_brief_task`'s markdown), what "done" means or that it must be
/// asked, and the standing rulings for the task's project.
pub(crate) fn start_context(
    brief: &Value,
    rulings: &Value,
    actor: &str,
    opts: &DetailOpts,
) -> String {
    let task = &brief["task"];
    let sid = task["short_id"].as_i64().unwrap_or(0);
    let mut out = format!(
        "# tasqx work: task #{sid}\n\n\
         You were launched by `tasqx work` to work on task #{sid}. It is claimed for this \
         session as `{actor}`: pass `actor: \"{actor}\"` on every tasqx tool call about it, \
         or the claim refuses the call. Its timer is running; tasqx stops it and releases \
         the claim when you exit.\n\n"
    );
    out.push_str(&tasqx_core::markdown::task_brief(brief, opts));
    if task["checks"].as_array().is_none_or(Vec::is_empty) {
        out.push_str(
            "\n## Defining done\n\nThis task has no acceptance checks. Before working on it, \
             ask the user what \"done\" means, and record each criterion with \
             tasqx_add_check.\n",
        );
    }
    let project = task["project"].as_str();
    let standing: Vec<String> = rulings["docs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|d| d["project"].is_null() || d["project"].as_str() == project)
        .map(|d| {
            format!(
                "- {} (read it with tasqx_get_memory, id {})",
                d["title"].as_str().unwrap_or(""),
                d["id"].as_str().unwrap_or("")
            )
        })
        .collect();
    if !standing.is_empty() {
        out.push_str(
            "\n## Standing rulings\n\nRecorded by the user or an earlier session; follow them.\n\n",
        );
        out.push_str(&standing.join("\n"));
        out.push('\n');
    }
    out
}

/// How each agent CLI takes the context (D245, verified against claude 2.x
/// and codex-cli 0.164): Claude reads a file, Codex a TOML string.
pub(crate) fn agent_args(
    agent: &str,
    session: &str,
    path: &std::path::Path,
    context: &str,
) -> Vec<std::ffi::OsString> {
    match agent {
        "claude" => vec![
            "--session-id".into(),
            session.into(),
            "--append-system-prompt-file".into(),
            path.into(),
        ],
        _ => vec![
            "-c".into(),
            format!(
                "developer_instructions={}",
                toml_edit::Value::from(context).to_string().trim()
            )
            .into(),
        ],
    }
}

/// Claim, start, spawn, wait; then stop, release and clean up whatever the
/// child did, and hand its exit code on.
fn launch(be: &mut Backend, ctx: &Ctx, task_ref: &str, agent: &'static str) -> CmdOutcome {
    // Reads first: a ref that does not resolve fails here, having written
    // nothing.
    let brief = be.call("task.brief", &json!({ "ref": task_ref }))?;
    let sid = brief["task"]["short_id"].as_i64().unwrap_or(0);
    let rulings = be.call("memory.list", &json!({ "standing": true }))?;
    let program = on_path(agent)
        .ok_or_else(|| ApiError::not_found(format!("`{agent}` is not on PATH"), None))?;

    let session = tasqx_core::clock::uuid_v7().to_string();
    let actor = format!("work:{agent}:{}", std::process::id());
    let opts = DetailOpts {
        time: ctx.time_format,
        now: crate::clock::now(),
    };
    let context = start_context(&brief, &rulings, &actor, &opts);
    let path = std::env::temp_dir().join(format!("tasqx-work-{session}.md"));
    std::fs::write(&path, &context)
        .map_err(|e| ApiError::internal(format!("cannot write {}: {e}", path.display())))?;

    // The store writes, right before the spawn.
    let claim = be.call(
        "task.claim",
        &json!({ "ref": sid, "lease": LEASE, "actor": actor }),
    );
    let started = claim.and_then(|_| {
        be.call(
            "task.start",
            &json!({ "ref": sid, "actor": actor, "session_id": session, "client": agent }),
        )
    });
    let was_started = started.is_ok();
    let ran = started.and_then(|_| {
        // Ctrl-C belongs to the agent: tasqx must outlive it to clean up.
        let _ = ctrlc::set_handler(|| {});
        std::process::Command::new(&program)
            .args(agent_args(agent, &session, &path, &context))
            .env("TASQX_WORK_TASK", sid.to_string())
            .env("TASQX_WORK_SESSION", &session)
            .env("TASQX_WORK_CONTEXT", &path)
            .status()
            .map_err(|e| ApiError::internal(format!("could not run {}: {e}", program.display())))
    });

    // After the child, whatever happened: the agent may have stopped or
    // closed the task itself, so a refusal here is not an error.
    if was_started {
        let _ = be.call("task.stop", &json!({ "ref": sid, "actor": actor }));
    }
    let _ = be.call("task.release", &json!({ "ref": sid, "actor": actor }));
    let _ = std::fs::remove_file(&path);

    // shortcut: a child killed by a signal reports 1, not 128+signal.
    let code = ran?.code().unwrap_or(1);
    exit_after_output(code);
    Ok((
        json!({ "launched": true, "short_id": sid, "agent": agent, "session_id": session, "exit_code": code }),
        String::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_comes_from_the_flag_then_memory_then_path_then_one_question() {
        let both = &["claude", "codex"][..];
        assert_eq!(
            choose_agent(Some("codex"), Some("claude"), both).unwrap(),
            Choice::Use("codex")
        );
        assert!(
            choose_agent(Some("codex"), None, &["claude"]).is_err(),
            "a flag naming a missing agent"
        );
        assert_eq!(
            choose_agent(None, Some("codex"), both).unwrap(),
            Choice::Use("codex")
        );
        // A remembered agent that is gone no longer counts.
        assert_eq!(
            choose_agent(None, Some("codex"), &["claude"]).unwrap(),
            Choice::Use("claude")
        );
        assert_eq!(
            choose_agent(None, None, &["codex"]).unwrap(),
            Choice::Use("codex")
        );
        assert_eq!(
            choose_agent(None, None, both).unwrap(),
            Choice::Ask(both.to_vec())
        );
        let none = choose_agent(None, None, &[]).unwrap_err().message;
        assert!(none.contains("claude") && none.contains("codex"), "{none}");
    }

    #[test]
    fn enter_takes_the_default_and_anything_unknown_cancels() {
        let o = &["claude", "codex"][..];
        assert_eq!(parse_answer("\n", o), Some("claude"));
        assert_eq!(parse_answer(" codex \n", o), Some("codex"));
        assert_eq!(parse_answer("2", o), Some("codex"));
        assert_eq!(parse_answer("0", o), None);
        assert_eq!(parse_answer("3", o), None);
        assert_eq!(parse_answer("q", o), None);
    }

    #[test]
    fn a_ref_is_digits_or_a_uuid_and_nothing_else() {
        assert!(is_ref("12"));
        assert!(is_ref("01a17cf7-4b13-7377-9a63-a24cd4582286"));
        assert!(!is_ref("login bug"));
        assert!(!is_ref(""));
        assert!(!is_ref("12a"));
    }

    #[test]
    fn codex_gets_the_context_as_one_toml_string_that_survives_quotes_and_newlines() {
        let ctx = "line \"one\"\nback\\slash 'q' \u{7}";
        let args = agent_args("codex", "s", std::path::Path::new("/x"), ctx);
        assert_eq!(args[0], "-c");
        let arg = args[1].to_str().unwrap();
        let value = arg.strip_prefix("developer_instructions=").unwrap();
        let doc: toml_edit::DocumentMut = format!("v = {value}").parse().unwrap();
        assert_eq!(doc["v"].as_str(), Some(ctx));
    }

    /// Cancel at any question writes nothing: leaving the browser, or the
    /// agent question, ends `decide` with the store exactly as it was.
    #[test]
    fn leaving_a_question_writes_nothing() {
        let mut be = Backend::Local(Engine::open_in_memory().unwrap());
        be.call("task.add", &json!({ "title": "t" })).unwrap();
        let count = |be: &mut Backend| {
            be.call("event.list", &json!({ "limit": 500 })).unwrap()["events"]
                .as_array()
                .map_or(0, Vec::len)
        };
        let before = count(&mut be);
        let both = &["claude", "codex"][..];

        // The browser, left.
        let left = decide(
            &mut be,
            None,
            None,
            None,
            both,
            true,
            |_| Ok(None),
            |_| panic!("asked about the agent after the browser was left"),
        );
        assert_eq!(left.unwrap(), None);
        // The agent question, left.
        let left = decide(
            &mut be,
            Some("1".into()),
            None,
            None,
            both,
            true,
            |_| unreachable!(),
            |_| None,
        );
        assert_eq!(left.unwrap(), None);
        // Answered: the answer is to be remembered, a flag is not.
        let answered = decide(
            &mut be,
            None,
            None,
            None,
            both,
            true,
            |_| Ok(Some(1)),
            |o| Some(o[1]),
        );
        assert_eq!(answered.unwrap(), Some(("1".to_string(), "codex", true)));
        let flagged = decide(
            &mut be,
            Some("1".into()),
            Some("claude"),
            None,
            both,
            true,
            |_| unreachable!(),
            |_| unreachable!(),
        );
        assert_eq!(flagged.unwrap(), Some(("1".to_string(), "claude", false)));
        // Off a terminal the question is a refusal, never asked.
        let refused = decide(
            &mut be,
            Some("1".into()),
            None,
            None,
            both,
            false,
            |_| unreachable!(),
            |_| unreachable!(),
        );
        assert!(refused.unwrap_err().message.contains("--agent claude"));

        assert_eq!(count(&mut be), before, "a question wrote to the store");
        let t = be.call("task.get", &json!({ "ref": "1" })).unwrap();
        assert!(
            t["claimed_by"].is_null() && t["active_since"].is_null(),
            "{t}"
        );
    }

    #[test]
    fn a_context_without_checks_asks_for_done_and_lists_only_this_projects_rulings() {
        let opts = DetailOpts {
            time: TimeFormat::Iso,
            now: crate::clock::now(),
        };
        let brief =
            json!({ "task": { "short_id": 7, "title": "T", "project": "a", "checks": [] } });
        let rulings = json!({ "docs": [
            { "id": "1", "title": "mine", "project": "a" },
            { "id": "2", "title": "everyone's", "project": null },
            { "id": "3", "title": "theirs", "project": "b" },
        ] });
        let ctx = start_context(&brief, &rulings, "work:claude:1", &opts);
        assert!(ctx.contains("tasqx_add_check"), "{ctx}");
        assert!(ctx.contains("actor: \"work:claude:1\""), "{ctx}");
        assert!(ctx.contains("mine") && ctx.contains("everyone's"), "{ctx}");
        assert!(!ctx.contains("theirs"), "{ctx}");

        let checked = json!({ "task": { "short_id": 7, "title": "T", "checks": [{ "body": "it works", "state": "open" }] } });
        let ctx = start_context(&checked, &json!({}), "a", &opts);
        assert!(!ctx.contains("tasqx_add_check"), "{ctx}");
        assert!(ctx.contains("it works"), "{ctx}");
    }
}
