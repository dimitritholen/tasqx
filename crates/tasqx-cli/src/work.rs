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
    let rows = pick_rows(&work_candidates(be)?);
    if rows.is_empty() {
        return Err(no_candidates(filter));
    }
    let next = be.call("task.next", &json!({}))?["task"]["short_id"].as_i64();
    let mut app = tui::pick::App::new(rows, ctx, filter, crate::clock::now()).choosing(next);
    tui::with_terminal(|term| pick_loop(term, be, &mut app))
        .map_err(|e| ApiError::internal(format!("terminal error: {e}")))
}

/// The tasks the chooser offers: `@working` less what the launch could not
/// claim and start as its own fresh `work:` holder, which no task is held by
/// yet: a task with a live claim, or with a clock already running.
fn work_candidates(be: &mut Backend) -> Result<Value, ApiError> {
    let mut listed = pick_candidates(be, "@working", None)?;
    if let Some(rows) = listed.get_mut("tasks").and_then(Value::as_array_mut) {
        rows.retain(|t| t["claimed_by"].is_null() && t["status"] != "active");
        let n = rows.len();
        listed["count"] = json!(n);
    }
    Ok(listed)
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
/// asked, and the standing rulings `task.brief` ranked for the task (D246).
pub(crate) fn start_context(brief: &Value, actor: &str, opts: &DetailOpts) -> String {
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
    if let Some(rulings) = brief["rulings_section"].as_str() {
        out.push_str("\n## Project memory\n\n");
        out.push_str(rulings.trim_end());
        out.push('\n');
    }
    out
}

/// The one hook command, for every event and every session (D248): Codex
/// trusts a hook by the hash of its definition, so it never varies, and the
/// event comes from the hook's stdin.
pub const HOOK_COMMAND: &str = "tasqx hook claude";

/// The `--settings` JSON that registers [`HOOK_COMMAND`] on the Claude hook
/// events `tasqx hook claude` handles. Layered on the user's own settings by
/// Claude for this session only.
fn claude_settings() -> String {
    let group = json!([{ "hooks": [{ "type": "command", "command": HOOK_COMMAND }] }]);
    json!({ "hooks": { "SessionStart": group, "SessionEnd": group } }).to_string()
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
            "--settings".into(),
            claude_settings().into(),
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

/// `task.brief`'s params for the launch: the rulings are those for this
/// directory's project, the one the agent's own MCP session infers too. A
/// directory that is not UTF-8 cannot be named in JSON without loss, so it
/// sends none, as the MCP server does.
fn brief_params(task_ref: &str, workdir: Option<&std::path::Path>) -> Value {
    let workdir = workdir.and_then(std::path::Path::to_str);
    json!({ "ref": task_ref, "rulings": true, "workdir": workdir })
}

/// Claim, start, spawn, wait; then stop, release and clean up whatever the
/// child did, and hand its exit code on.
fn launch(be: &mut Backend, ctx: &Ctx, task_ref: &str, agent: &'static str) -> CmdOutcome {
    // Reads first: a ref that does not resolve fails here, having written
    // nothing.
    let workdir = std::env::current_dir().ok();
    let brief = be.call("task.brief", &brief_params(task_ref, workdir.as_deref()))?;
    let sid = brief["task"]["short_id"].as_i64().unwrap_or(0);
    let program = on_path(agent)
        .ok_or_else(|| ApiError::not_found(format!("`{agent}` is not on PATH"), None))?;

    let session = tasqx_core::clock::uuid_v7().to_string();
    let actor = format!("work:{agent}:{}", std::process::id());
    let opts = DetailOpts {
        time: ctx.time_format,
        now: crate::clock::now(),
    };
    let context = start_context(&brief, &actor, &opts);
    let path = std::env::temp_dir().join(format!("tasqx-work-{session}.md"));
    // Owner-only and never over an existing file: the brief and the rulings
    // are nobody else's to read in a shared temp dir.
    verbs::write_new(&path, &context)
        .map_err(|e| ApiError::internal(format!("cannot write {}: {e}", path.display())))?;
    // The agent's own tasqx (its MCP server, a `tasqx` it runs) must reach
    // the store this claim is in: the daemon this process routed through, or
    // the file it holds in-process.
    // Through a daemon that is its socket AND its store file, because `tasqx
    // mcp serve` opens TASQX_DB and never asks the socket. In-process it is
    // the file, and no inherited socket that may serve another store.
    let mut route: Vec<(&str, std::ffi::OsString)> = Vec::new();
    let in_process = be.remote_socket().is_none();
    if let Some(sock) = be.remote_socket() {
        route.push(("TASQX_SOCK", sock.into()));
    }
    let db = match in_process {
        // Absolute, so an agent that changes directory still opens this file.
        true => db_path()
            .ok()
            .map(|db| {
                std::fs::canonicalize(&db).unwrap_or_else(|_| {
                    std::env::current_dir().map_or(db.clone(), |cwd| cwd.join(&db))
                })
            })
            .map(PathBuf::into_os_string),
        false => be.call("core.capabilities", &json!({}))?["store"]
            .as_str()
            .map(Into::into),
    };
    if let Some(db) = db {
        route.push(("TASQX_DB", db));
    }

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
        let mut child = std::process::Command::new(&program);
        if in_process {
            child.env_remove("TASQX_SOCK");
        }
        child
            .args(agent_args(agent, &session, &path, &context))
            .env("TASQX_WORK_TASK", sid.to_string())
            .env("TASQX_WORK_SESSION", &session)
            .env("TASQX_WORK_CONTEXT", &path)
            .envs(route)
            .status()
            .map_err(|e| ApiError::internal(format!("could not run {}: {e}", program.display())))
    });

    // After the child, whatever happened. The connection sat idle for the
    // whole session, and the daemon drops a client idle for 15 minutes, so
    // cleanup starts on a fresh one.
    reconnect(be);
    let params = json!({ "ref": sid, "actor": actor });
    if was_started {
        let fix = format!("tasqx stop {sid} --holder {actor}");
        cleanup(be, "task.stop", &params, &fix);
    }
    let fix = format!("tasqx release {sid} --holder {actor}");
    cleanup(be, "task.release", &params, &fix);
    let _ = std::fs::remove_file(&path);

    // shortcut: a child killed by a signal reports 1, not 128+signal.
    let code = ran?.code().unwrap_or(1);
    exit_after_output(code);
    Ok((
        json!({ "launched": true, "short_id": sid, "agent": agent, "session_id": session, "exit_code": code }),
        String::new(),
    ))
}

/// A new connection to the daemon `be` routes through; `false` when there is
/// none to make (in-process, or the daemon is gone).
fn reconnect(be: &mut Backend) -> bool {
    let Some(socket) = be.remote_socket().map(str::to_string) else {
        return false;
    };
    match tasqx_core::daemon::try_connect(&socket) {
        Some(conn) => {
            *be = Backend::Remote { conn, socket };
            true
        }
        None => false,
    }
}

/// One after-session call. A refusal means the agent already stopped or
/// closed the task, and is fine. A call that fails on the connection is
/// retried once on a fresh one; a failure after that is said, naming `fix`.
fn cleanup(be: &mut Backend, method: &str, params: &Value, fix: &str) {
    let lost = |r: &Result<Value, ApiError>| matches!(r, Err(e) if e.code == ErrorCode::Internal);
    let mut result = be.call(method, params);
    if lost(&result) && reconnect(be) {
        result = be.call(method, params);
    }
    if let (true, Err(e)) = (lost(&result), &result) {
        eprintln!(
            "tasqx: warning: {method} did not happen ({}); run `{fix}`",
            e.message
        );
    }
}

/// `tasqx hook claude` (D248): what Claude Code runs on a hook event. It
/// acts only inside the `tasqx work` session it was registered for, and never
/// breaks the agent: every fault is a stderr line and the exit is 0.
pub(crate) fn run_hook() {
    // Outside a work session (a hook copied into someone's own settings),
    // the input is not even read.
    if std::env::var_os("TASQX_WORK_TASK").is_none() {
        return;
    }
    let mut input = String::new();
    use std::io::Read;
    let parsed = std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str::<Value>(&input).map_err(|e| e.to_string()));
    let hook = match parsed {
        Ok(v) => v,
        Err(e) => return eprintln!("{HOOK_COMMAND}: unreadable hook input: {e}"),
    };
    // Another session's hook, a nested `claude` say, is not this one's.
    let session = std::env::var("TASQX_WORK_SESSION").ok();
    if session.is_none() || hook["session_id"].as_str() != session.as_deref() {
        return;
    }
    if let Err(e) = on_event(hook["hook_event_name"].as_str().unwrap_or(""), &hook) {
        eprintln!("{HOOK_COMMAND}: {e}");
    }
}

/// One arm per Claude hook event `work` registers; adding an event is an arm
/// here and a key in [`claude_settings`].
fn on_event(event: &str, _hook: &Value) -> Result<(), String> {
    match event {
        // Entry points: the handoff (#4177) and the per-turn push (#4185)
        // hang off these. Tokens are measured when the task completes.
        "SessionStart" | "SessionEnd" => Ok(()),
        other => Err(format!("no handler for hook event {other:?}")),
    }
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

    /// A directory that is not UTF-8 cannot be named in JSON: the brief is
    /// asked without one (no project inferred), never a panic.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_directory_sends_no_workdir_rather_than_panicking() {
        use std::os::unix::ffi::OsStrExt;
        let dir = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/tmp/caf\xe9"));
        let p = brief_params("7", Some(dir));
        assert!(p["workdir"].is_null(), "{p}");
        assert_eq!(p["rulings"], true);
        let p = brief_params("7", Some(std::path::Path::new("/tmp/x")));
        assert_eq!(p["workdir"], "/tmp/x");
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

    /// The chooser offers only what the launch can claim and start: no task
    /// another holder has claimed, and no task whose clock is already running.
    #[test]
    fn the_chooser_offers_only_tasks_the_launch_can_claim() {
        let mut be = Backend::Local(Engine::open_in_memory().unwrap());
        for t in ["claimed", "running", "shell's claim", "free"] {
            be.call("task.add", &json!({ "title": t })).unwrap();
        }
        be.call("task.claim", &json!({ "ref": "1", "actor": "builder-2" }))
            .unwrap();
        be.call("task.start", &json!({ "ref": "2" })).unwrap();
        be.call("task.claim", &json!({ "ref": "3" })).unwrap();
        let listed = work_candidates(&mut be).unwrap();
        let ids: Vec<i64> = listed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["short_id"].as_i64())
            .collect();
        assert_eq!(ids, [4]);
    }

    #[test]
    fn a_context_without_checks_asks_for_done_and_carries_the_briefs_rulings_section() {
        let opts = DetailOpts {
            time: TimeFormat::Iso,
            now: crate::clock::now(),
        };
        let brief = json!({
            "task": { "short_id": 7, "title": "T", "project": "a", "checks": [] },
            "rulings_section": "Standing rulings for project a.\n- mine: its gist\n",
        });
        let ctx = start_context(&brief, "work:claude:1", &opts);
        assert!(ctx.contains("tasqx_add_check"), "{ctx}");
        assert!(ctx.contains("actor: \"work:claude:1\""), "{ctx}");
        assert!(ctx.contains("## Project memory"), "{ctx}");
        assert!(ctx.contains("- mine: its gist"), "{ctx}");

        // No rulings (`null`): no heading.
        let checked = json!({
            "task": { "short_id": 7, "title": "T", "checks": [{ "body": "it works", "state": "open" }] },
            "rulings_section": null,
        });
        let ctx = start_context(&checked, "a", &opts);
        assert!(!ctx.contains("tasqx_add_check"), "{ctx}");
        assert!(ctx.contains("it works"), "{ctx}");
        assert!(!ctx.contains("Standing rulings"), "{ctx}");
    }
}
