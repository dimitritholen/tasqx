//! The thin verb handlers: each `run_*` translates one clap command into one
//! JSON-API call and hands the (result, rendering) pair back to the Exit
//! terminal in lib.rs. Thin is the property — anything that starts deciding
//! here instead of in core is a second dispatch path, which is the wrong
//! turn CLAUDE.md warns about.

use super::*;

pub(crate) fn run_init(
    be: &mut Backend,
    ctx: &Ctx,
    name: String,
    desc: Option<String>,
) -> CmdOutcome {
    let mut params = json!({ "name": name });
    if let Some(d) = desc {
        params["description"] = Value::String(d);
    }
    let result = be.call("project.create", &params)?;
    let text = render::project_created(ctx, &result);
    Ok((result, text))
}

/// Say that the project name may have been CUT, when it may have been.
///
/// The core answers a missing project with `no project named X (create it with
/// `tasqx init X`)`, which is right when X is what the user typed. From an
/// unquoted `project:` sugar token it is not: the token ends at the first space,
/// so `project:My "Big" Project` asked about `My` and the message then advised
/// creating a project that already existed under a longer name — confidently
/// naming a fragment as though the user had typed it.
///
/// We cannot tell a typo from a truncation here, so the message stops claiming
/// to. It says where the name came from and gives the spelling that names a
/// whole one; the `init` advice is kept, because a typo is still the likelier
/// case and it is now offered rather than asserted.
/// Takes the name rather than the whole `ParsedAdd` because `modify` has already
/// moved its fields into the `set` map by the time the call fails, and both
/// verbs must answer this identically (§12-D13).
pub(crate) fn name_the_cut(e: ApiError, cut_name: Option<&str>) -> ApiError {
    if e.code != ErrorCode::NotFound {
        return e;
    }
    let Some(name) = cut_name else { return e };
    if !e.message.starts_with("no project named") {
        return e;
    }
    ApiError::new(
        e.code,
        format!(
            "no project named {name:?} — but that is only the part of a `project:` token \
             before the first space, so a name with spaces must be quoted: \
             project:\"{name} …\". If {name:?} really is the whole name, create it with \
             `tasqx init {name:?}`."
        ),
        e.data,
    )
}

/// The project name IF it might have been cut short by the sugar tokenizer.
pub(crate) fn cut_project_name(parsed: &sugar::ParsedAdd) -> Option<String> {
    parsed
        .project_may_be_truncated
        .then(|| parsed.project.clone())
        .flatten()
}

pub(crate) fn run_add(
    be: &mut Backend,
    ctx: &Ctx,
    title: Vec<String>,
    flags: sugar::AddFlags,
) -> CmdOutcome {
    // argv goes in unjoined: the shell's argument boundaries are information the
    // parser needs (see `sugar::parse_add`), and joining destroys them.
    let parsed = sugar::parse_add(&title, flags)?;
    // Taken before the fields are moved into `params`; see `name_the_cut`.
    let cut = cut_project_name(&parsed);

    // Resolve every natural-language date through the ONE core parser, using the
    // real `now` (deterministic in tests, which call the parser directly).
    let now = now_ts();
    let mut params = json!({ "title": parsed.title });
    if let Some(p) = parsed.project {
        params["project"] = Value::String(p);
    }
    if let Some(p) = parsed.priority {
        params["priority"] = Value::String(p);
    }
    if let Some(d) = parsed.due {
        params["due"] = Value::String(datetime::parse_when(&d, now)?);
    }
    if let Some(s) = parsed.scheduled {
        params["scheduled"] = Value::String(datetime::parse_when(&s, now)?);
    }
    if let Some(w) = parsed.wait {
        params["wait"] = Value::String(datetime::parse_when(&w, now)?);
    }
    if let Some(r) = parsed.recurrence {
        params["recurrence"] = Value::String(r);
    }
    // Passed through raw: unlike due/scheduled/wait, a reminder may be a
    // due-anchored offset that must STAY symbolic (so it re-anchors when due
    // moves), so the core — not the CLI — decides offset vs. absolute (§9).
    if let Some(r) = parsed.remind {
        params["remind"] = Value::String(r);
    }
    if let Some(e) = parsed.estimate {
        params["estimate"] = Value::String(datetime::parse_duration(&e)?);
    }
    if !parsed.tags.is_empty() {
        params["tags"] = Value::Array(parsed.tags.into_iter().map(Value::String).collect());
    }
    let result = be
        .call("task.add", &params)
        .map_err(|e| name_the_cut(e, cut.as_deref()))?;
    // The interactive echo is a card (D76), and the card wants fields
    // `task.add`'s frozen five-field result does not carry — tags, due,
    // priority, estimate — so this path reads the task back, the same
    // composite shape `modify` uses for its follow-up `tag.add`. Only on the
    // card path: the plain line renders from the add result alone, byte for
    // byte as it always has. A failed read-back falls back to that plain
    // line rather than erroring — the add succeeded, and the echo failing
    // must not turn that into a red exit.
    let full = if ctx.caps.unicode {
        result
            .get("short_id")
            .and_then(Value::as_i64)
            .and_then(|sid| be.call("task.get", &json!({ "ref": sid })).ok())
    } else {
        None
    };
    let text = match full {
        Some(task) => render::task_added_card(ctx, &task),
        None => render::task_added(ctx, &result, &parsed.title),
    };
    Ok((result, text))
}

/// `tasqx modify <ref> [words / sugar] [--flags] [--clear FIELD]…`
///
/// Builds ONE `set` map and issues ONE `task.modify` — every field goes through
/// the same sugar parser and the same core date/duration/recurrence/reminder
/// parsers as `add`, so a token means the same thing in both verbs.
///
/// `+tag` is the one exception to one-verb-one-method: tags do not live in the
/// tasks row and `task.modify` has no `tags` field, so tags are applied with a
/// follow-up `tag.add`. Dropping them silently to preserve the purity of the
/// mapping would be the worse trade — the user typed `+tag` and meant it.
pub(crate) fn run_modify(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    rest: Vec<String>,
    flags: sugar::AddFlags,
    clear: &[String],
    expected_rev: Option<i64>,
) -> CmdOutcome {
    let parsed = sugar::parse_add(&rest, flags)?;
    // Taken before the fields are moved into `set`; see `name_the_cut`.
    let cut = cut_project_name(&parsed);
    let now = now_ts();

    let mut set = serde_json::Map::new();

    // Clearing first, so a field named in BOTH is caught rather than resolved by
    // map-insertion order — "set it and clear it" is a mistake, not a precedence
    // question, and guessing an answer would be the un-forgiving kind of clever.
    for field in clear {
        set.insert(field.clone(), Value::Null);
    }

    // Leftover bare words are the new title. An explicit empty title can't be
    // expressed (and shouldn't be): no words means "leave the title alone".
    if !parsed.title.is_empty() {
        set.insert("title".into(), Value::String(parsed.title.clone()));
    }
    if let Some(p) = parsed.project {
        guard_set_and_clear(&set, "project", &p)?;
        set.insert("project".into(), Value::String(p));
    }
    if let Some(p) = parsed.priority {
        guard_set_and_clear(&set, "priority", &p)?;
        set.insert("priority".into(), Value::String(p));
    }
    if let Some(d) = parsed.due {
        guard_set_and_clear(&set, "due", &d)?;
        set.insert("due".into(), Value::String(datetime::parse_when(&d, now)?));
    }
    if let Some(s) = parsed.scheduled {
        guard_set_and_clear(&set, "scheduled", &s)?;
        set.insert(
            "scheduled".into(),
            Value::String(datetime::parse_when(&s, now)?),
        );
    }
    if let Some(w) = parsed.wait {
        guard_set_and_clear(&set, "wait", &w)?;
        set.insert("wait".into(), Value::String(datetime::parse_when(&w, now)?));
    }
    if let Some(r) = parsed.recurrence {
        guard_set_and_clear(&set, "recurrence", &r)?;
        // Validated + normalized by the core, exactly as in task.add.
        set.insert("recurrence".into(), Value::String(r));
    }
    // Stays symbolic: an offset must re-anchor when `due` moves (§9).
    if let Some(r) = parsed.remind {
        guard_set_and_clear(&set, "remind", &r)?;
        set.insert("remind".into(), Value::String(r));
    }
    if let Some(e) = parsed.estimate {
        guard_set_and_clear(&set, "estimate", &e)?;
        set.insert(
            "estimate".into(),
            Value::String(datetime::parse_duration(&e)?),
        );
    }

    if set.is_empty() && parsed.tags.is_empty() {
        return Err(ApiError::bad_request(
            "modify needs something to change — a title, inline sugar (due:friday, !high, \
             +tag, est:4h), a flag, or --clear <field>",
        ));
    }

    let mut result = Value::Null;
    if !set.is_empty() {
        let mut params = json!({ "ref": r#ref, "set": Value::Object(set.clone()) });
        if let Some(rev) = expected_rev {
            params["expected_rev"] = Value::from(rev);
        }
        result = be
            .call("task.modify", &params)
            .map_err(|e| name_the_cut(e, cut.as_deref()))?;
    }

    // Tags: a second call, and deliberately AFTER the modify — if the modify is
    // rejected (bad value, or a lost `expected_rev` race) nothing at all should
    // have happened, and a tag applied first would survive the failure.
    if !parsed.tags.is_empty() {
        let tag_params = json!({ "ref": r#ref, "tags": parsed.tags.clone() });
        let tag_result = be.call("tag.add", &tag_params)?;
        if result.is_null() {
            result = tag_result;
        } else if let Some(tags) = tag_result.get("tags") {
            result["tags"] = tags.clone();
        }
    }

    let text = render::modified(ctx, &result, &set, &parsed.tags);
    Ok((result, text))
}

/// Reject "set X and clear X in one command". Both were typed on purpose and
/// they contradict; picking a winner would silently discard half the intent.
pub(crate) fn guard_set_and_clear(
    set: &serde_json::Map<String, Value>,
    field: &str,
    value: &str,
) -> Result<(), ApiError> {
    if set.get(field) == Some(&Value::Null) {
        return Err(ApiError::bad_request(format!(
            "cannot both set and clear `{field}` (got --clear {field} and a value of {value:?})"
        )));
    }
    Ok(())
}

pub(crate) fn run_list(be: &mut Backend, ctx: &Ctx, filter: &[String]) -> CmdOutcome {
    // Bare `tasqx` (and `tasqx list` with no filter) => the working set.
    // Otherwise `from_argv`, never `join(" ")`: the shell's argument boundaries
    // are information the filter parser needs, exactly as on the write path
    // (see `sugar::parse_add`). Joining loses which spaces the user quoted.
    let filter_str = if filter.is_empty() {
        "@working".to_string()
    } else {
        tasqx_core::filter::from_argv(filter)
    };
    let params = json!({ "filter": filter_str, "sort": ["-urgency"] });
    let result = be.call("task.list", &params)?;
    let text = render::task_table(ctx, &result, jiff::Timestamp::now());
    Ok((result, text))
}

/// The filter DSL for "not finished": every status `Status::is_open` calls open,
/// spelled as an `or` chain.
///
/// Derived from `Status::ALL` rather than written out, for the reason that
/// constant's own doc gives — the names used to live by hand in ten places, and
/// a status missing from one of them makes tasks stop appearing without anything
/// failing. There is no `@open` keyword in the grammar to lean on: `KEYWORDS` is
/// `@working` and `@blocked`, and neither is this set.
pub(crate) fn open_statuses_filter() -> String {
    tasqx_core::types::Status::ALL
        .iter()
        .filter(|s| s.is_open())
        .map(|s| format!("status:{}", s.as_str()))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// `tasqx agenda` — the same question `list` asks, ordered by time.
///
/// # No new API method, deliberately
///
/// `task.list` already takes a filter and a sort (`dispatch::METHODS`), and this
/// verb needs nothing else FROM the store: the day grouping, the horizon and the
/// earlier-of-two-dates ordering are all functions of fields every row already
/// carries. An `agenda.*` method would be a second way to ask one question, and
/// D50 narrows the API on purpose — the surface that has to stay frozen for v1
/// is the one worth keeping small.
///
/// The ordering is not sent as a `sort` key for the same reason it could not be:
/// the agenda key is `min(due, scheduled)`, which is not in `engine::SORT_KEYS`
/// and would have to be added to the frozen contract to express a presentation
/// choice. `-urgency` is asked for instead — byte-identical to what [`run_list`]
/// sends — and `agenda_select` stable-sorts by the instant, so two tasks landing
/// at the same minute keep the urgency ranking the rest of the tool gives them.
///
/// # The filter default is NOT `list`'s, and the reason is a measured one
///
/// `list` defaults to `@working`, and `@working` is pending|active. A task with
/// a `scheduled` (or `wait`) date in the future sits in **backlog** until that
/// instant arrives — `types::effective_status` promotes it on the way out of the
/// store — so `@working` excludes, precisely, everything that is scheduled for
/// later. Driven against the real binary: `add "Quarterly deps audit"
/// scheduled:2026-08-04` then `agenda` on the 3rd showed no Tuesday at all. An
/// agenda that cannot show what is scheduled for tomorrow is not an agenda, so
/// the default here is every OPEN status instead — the same set minus nothing,
/// plus the backlog `@working` was built to hide from a "what can I do now" view.
///
/// The set is DERIVED from `Status::ALL` and `Status::is_open`, never typed out:
/// a sixth status would otherwise reach `list` and silently miss this view, which
/// is the drift `Status::ALL` exists to end (its own doc names the ten places the
/// names used to be spelled by hand).
///
/// # How a caller's own filter is combined with it
///
/// D24's resolution order, the one `report.summary` already uses: a caller who
/// named a status is taken literally, so `tasqx agenda status:done` shows done
/// tasks rather than an empty table; anything else is ANDed with the open set.
/// The question is asked of the PARSED tree via `Filter::constrains_status`,
/// because a lexical `contains("status")` both over-matches (`+status-page`) and
/// under-matches (`@working`).
///
/// Composed on the wire rather than applied to the rows after they arrive, so
/// the store does the narrowing it is good at — and so `tasqx agenda` on a store
/// with a thousand closed tasks does not report "1000 hidden" under every run.
///
/// A filter this build cannot parse is sent VERBATIM (`unwrap_or(true)`), so the
/// engine's refusal quotes the caller's words instead of parentheses this
/// function added — D45's rule about where a bad value is refused.
pub(crate) fn run_agenda(
    be: &mut Backend,
    ctx: &Ctx,
    filter: &[String],
    days: Option<usize>,
) -> CmdOutcome {
    let now = jiff::Timestamp::now();
    let asked = if filter.is_empty() {
        String::new()
    } else {
        tasqx_core::filter::from_argv(filter)
    };
    let names_status = tasqx_core::filter::Filter::parse(&asked, now)
        .map(|f| f.constrains_status())
        .unwrap_or(true);
    let filter_str = match (names_status, asked.is_empty()) {
        (true, _) => asked,
        (false, true) => open_statuses_filter(),
        // Parenthesised on both sides: the caller's filter may itself be an
        // `or`, and `a or b and c` would bind the default to `b` alone.
        (false, false) => format!("({asked}) and ({})", open_statuses_filter()),
    };

    let params = json!({ "filter": filter_str, "sort": ["-urgency"] });
    let result = be.call("task.list", &params)?;

    let a = render::agenda_select(&result, days.unwrap_or(AGENDA_DEFAULT_DAYS), now);
    let text = render::agenda_text(ctx, &a);
    // The result the `--json` terminal prints is the agenda's own, not the raw
    // `task.list` answer: see `render::agenda_json` for why the two flags have
    // to describe one set of rows.
    Ok((render::agenda_json(&a), text))
}

/// Widen a `task.start` / `task.done` params object with whichever correlation
/// facts were given on the command line (#12, #72).
///
/// Mirrors `Correlation::apply` on the engine side deliberately: present keys
/// only, so a flagless `tasqx done 4` sends byte-for-byte the object it sent
/// before these flags existed, and the engine's `opt_str_nonempty` never has to
/// distinguish "absent" from "explicitly null".
pub(crate) fn apply_correlation(params: &mut Value, c: &command::CorrelationArgs) {
    for (key, value) in [
        ("client", &c.client),
        ("session_id", &c.session_id),
        ("prompt_id", &c.prompt_id),
        ("transcript_path", &c.transcript_path),
    ] {
        if let Some(v) = value {
            params[key] = json!(v);
        }
    }
}

pub(crate) fn run_start(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    keep: bool,
    correlation: &command::CorrelationArgs,
) -> CmdOutcome {
    let mut params = json!({ "ref": r#ref, "keep": keep });
    apply_correlation(&mut params, correlation);
    let result = be.call("task.start", &params)?;
    let text = render::started(ctx, &result);
    Ok((result, text))
}

pub(crate) fn run_stop(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let params = json!({ "ref": r#ref });
    let result = be.call("task.stop", &params)?;
    let text = render::stopped(ctx, &result);
    Ok((result, text))
}

pub(crate) fn run_done(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    correlation: &command::CorrelationArgs,
) -> CmdOutcome {
    let mut params = json!({ "ref": r#ref });
    apply_correlation(&mut params, correlation);
    let result = be.call("task.done", &params)?;
    let text = render::done(ctx, &result);
    Ok((result, text))
}

pub(crate) fn run_show(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let result = be.call("task.get", &json!({ "ref": r#ref }))?;
    let text = render::task_detail(ctx, &result);
    Ok((result, text))
}

/// A method taking only `{ref}` and returning `{short_id, status}`.
pub(crate) fn run_simple_ref(
    be: &mut Backend,
    ctx: &Ctx,
    method: &str,
    r#ref: String,
) -> CmdOutcome {
    let result = be.call(method, &json!({ "ref": r#ref }))?;
    let text = render::status_line(ctx, &result);
    Ok((result, text))
}

/// `tasqx undo` — the safety net (DESIGN §5 example 12).
///
/// No params on the wire, and none to collect: `event.revert` reverses the
/// newest event in the log or refuses. The whole of this function is therefore
/// the call and the line it prints — and that line is the point, because
/// "undone" with nothing after it is exactly the answer a user cannot check
/// against what they actually did.
pub(crate) fn run_undo(be: &mut Backend, ctx: &Ctx) -> CmdOutcome {
    let result = be.call("event.revert", &json!({}))?;
    let text = render::undone(ctx, &result);
    Ok((result, text))
}

pub(crate) fn run_annotate(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    text: Vec<String>,
) -> CmdOutcome {
    let body = text.join(" ");
    let result = be.call("annotation.add", &json!({ "ref": r#ref, "body": body }))?;
    let out = render::annotated(ctx, &result);
    Ok((result, out))
}

/// `tasqx tag` / `tasqx untag`, the two spellings of one params shape.
///
/// One function for both, the way [`run_dep`] serves `dep`/`undep`: the params
/// are identical and only the method name differs, so two copies would be two
/// places for the tag normalisation to fall out of step.
///
/// The words go through [`sugar::tag_arguments`] and not straight onto the wire,
/// which is what makes `tasqx tag 42 +api` and `tasqx modify 42 +api` name the
/// same tag. Sending `+api` verbatim would have created a tag literally called
/// `+api`, invisible next to the `api` the sugar path writes and unreachable by
/// the `+api` filter token.
pub(crate) fn run_tag(
    be: &mut Backend,
    ctx: &Ctx,
    method: &str,
    r#ref: String,
    tags: &[String],
) -> CmdOutcome {
    let names = sugar::tag_arguments(tags)?;
    let result = be.call(method, &json!({ "ref": r#ref, "tags": names }))?;
    let text = render::tag_result(ctx, &result, method == "tag.add", &names);
    Ok((result, text))
}

pub(crate) fn run_dep(
    be: &mut Backend,
    ctx: &Ctx,
    method: &str,
    r#ref: String,
    depends_on: String,
) -> CmdOutcome {
    let result = be.call(method, &json!({ "ref": r#ref, "depends_on": depends_on }))?;
    let text = render::dep_result(ctx, &result, method == "dependency.add", &depends_on);
    Ok((result, text))
}

/// D21: the one explicit way to move the default project. Validation (exists,
/// not archived) lives in the core, not here — the CLI is one of three callers
/// of `project.use` and the rule has to hold for all of them.
pub(crate) fn run_use(be: &mut Backend, ctx: &Ctx, name: String) -> CmdOutcome {
    let result = be.call("project.use", &json!({ "name": name }))?;
    let text = render::default_switched(ctx, &result);
    Ok((result, text))
}

/// D22: take a project out of rotation. Same shape as [`run_use`] — the name is
/// a lookup the core resolves, so an unknown one is `not_found` (exit 4) from
/// the engine and not from a second copy of the rule here.
///
/// The interesting half is the response, not the request: `project.archive`
/// clears the default project when it archives the one the `default_project`
/// key names, and `default_cleared` is how it says so. Dropping that field on
/// the floor here would make the CLI the surface on which "where does a bare
/// `tasqx add` land" changed with nobody told — the invisible-state failure D21
/// and D22 exist to close, arriving through the one verb that was never wired
/// to a terminal.
pub(crate) fn run_archive(be: &mut Backend, ctx: &Ctx, name: String) -> CmdOutcome {
    let result = be.call("project.archive", &json!({ "name": name }))?;
    let text = render::project_archived(ctx, &result);
    Ok((result, text))
}

pub(crate) fn run_projects(be: &mut Backend, ctx: &Ctx, all: bool) -> CmdOutcome {
    let result = be.call("project.list", &json!({ "include_archived": all }))?;
    let text = render::project_table(ctx, &result);
    Ok((result, text))
}

/// Build the `report.summary` params from the CLI's positional args plus the
/// `--all` flag. Split out of [`run_report`] so the CLI→core contract can be
/// asserted without standing up a backend.
pub(crate) fn report_params(args: &[String], all: bool) -> Value {
    // First token, if a known group_by keyword, selects grouping; the rest is
    // the filter. Otherwise everything is the filter (group_by defaults).
    let mut group_by = tasqx_core::engine::SUMMARY_GROUP_BY[0].to_string();
    let mut rest: &[String] = args;
    if let Some(first) = args.first() {
        // The engine's own list, not a third copy. The MCP schema already
        // renders from this const; the CLI hard-coded the same three names, so
        // adding a fourth axis would have made the API accept it and the CLI
        // silently treat it as a filter token instead.
        if tasqx_core::engine::SUMMARY_GROUP_BY.contains(&first.as_str()) {
            group_by = first.clone();
            rest = &args[1..];
        }
    }
    // Same reasoning as `group_by` above, and the same constant pattern:
    // `SUMMARY_METRICS` exists to stop the CLI keeping a private second copy of
    // this list. It had one anyway, sitting three lines from the import — so a
    // fifth metric would have reached the JSON API and the MCP schema while
    // `tasqx report` silently kept asking for four.
    let mut params = json!({
        "group_by": group_by,
        "metrics": tasqx_core::engine::SUMMARY_METRICS,
    });
    if !rest.is_empty() {
        params["filter"] = Value::String(tasqx_core::filter::from_argv(rest));
    }
    // Sent only when set: core already defaults `all` to false, and an explicit
    // `false` would be the same thing said twice.
    if all {
        params["all"] = Value::Bool(true);
    }
    params
}

pub(crate) fn run_report(be: &mut Backend, ctx: &Ctx, args: Vec<String>, all: bool) -> CmdOutcome {
    let params = report_params(&args, all);
    let group_by = params["group_by"]
        .as_str()
        .unwrap_or(tasqx_core::engine::SUMMARY_GROUP_BY[0])
        .to_string();
    let result = be.call("report.summary", &params)?;
    let text = render::report(ctx, &result, &group_by);
    Ok((result, text))
}

/// `tasqx memory add|search|rm|import` (DESIGN.md §12-D41).
pub(crate) fn run_memory(be: &mut Backend, action: &MemoryAction) -> CmdOutcome {
    match action {
        MemoryAction::Add {
            title,
            body,
            source,
        } => {
            let mut params = json!({ "title": title, "body": body });
            if let Some(s) = source {
                params["source"] = json!(s);
            }
            let result = be.call("memory.add", &params)?;
            let text = format!(
                "Stored {}  ·  {}\n",
                render::san(result["id"].as_str().unwrap_or("?")),
                render::san(title)
            );
            Ok((result, text))
        }
        MemoryAction::Search {
            query,
            limit,
            scope,
            raw,
        } => {
            let mut params = json!({ "query": query.join(" ") });
            if let Some(n) = limit {
                params["limit"] = json!(n);
            }
            if let Some(s) = scope {
                params["scope"] = json!(s);
            }
            if *raw {
                params["raw"] = json!(true);
            }
            let result = be.call("memory.search", &params)?;
            let mut text = String::new();
            for hit in result["hits"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                let title = render::san(hit["title"].as_str().unwrap_or(""));
                let kind = render::san(hit["kind"].as_str().unwrap_or("?"));
                let src = render::san(hit["source"].as_str().unwrap_or("—"));
                let snip = render::san(hit["snippet"].as_str().unwrap_or(""));
                let id = render::san(hit["id"].as_str().unwrap_or("?"));
                text.push_str(&format!("{title}  ({kind} · {src})\n  {snip}\n  id {id}\n"));
            }
            let count = result["count"].as_u64().unwrap_or(0);
            text.push_str(&format!("{count} hit(s)\n"));
            // On a miss, name the expression that produced it (D69). Every
            // word of a plain query is a required phrase, so a question typed
            // as a sentence comes back exactly as empty as a subject nobody
            // ever wrote down — and the two need different next moves.
            if count == 0 {
                if let Some(matched) = result["matched"].as_str() {
                    text.push_str(&format!(
                        "  every term was required: {}\n",
                        render::san(matched)
                    ));
                }
            }
            Ok((result, text))
        }
        MemoryAction::Show { id } => {
            let result = be.call("memory.get", &json!({ "id": id }))?;
            let source = result["source"].as_str().unwrap_or("—");
            let text = format!(
                "{}  ({})\n{}\n",
                render::san(result["title"].as_str().unwrap_or("?")),
                render::san(source),
                render::san(result["body"].as_str().unwrap_or("")),
            );
            Ok((result, text))
        }
        MemoryAction::Rm { id } => {
            let result = be.call("memory.remove", &json!({ "id": id }))?;
            let text = format!("Removed {}\n", render::san(id));
            Ok((result, text))
        }
        MemoryAction::Import { path } => run_memory_import(be, path),
    }
}

/// `tasqx tokens recompute [--apply]` (DESIGN.md §12-D50, Decision 3).
///
/// The polarity flip happens here and nowhere else: the CLI speaks opt-in
/// destruction (`--apply`) while the engine speaks opt-out safety
/// (`dry_run`, defaulting true). Sending `dry_run` explicitly rather than
/// omitting it keeps this command's behaviour pinned to its own flag instead
/// of to whatever default a future engine revision ships.
pub(crate) fn run_tokens(be: &mut Backend, ctx: &Ctx, action: &TokensAction) -> CmdOutcome {
    match action {
        TokensAction::Recompute { apply } => {
            let result = be.call("tokens.recompute", &json!({ "dry_run": !apply }))?;
            let text = render::tokens_recompute(ctx, &result);
            Ok((result, text))
        }
    }
}

/// One doc per file. A directory imports its direct `*.md` children; finding
/// none is an error, not `Imported 0` at exit 0 — the same never-say-nothing
/// rule `import` learned for truncated task files.
pub(crate) fn run_memory_import(be: &mut Backend, path: &str) -> CmdOutcome {
    // Two-phase (review finding): ALL file I/O and title derivation happen
    // before a single write, then one `memory.import` lands the batch in one
    // transaction with replace-by-source semantics — a failure imports
    // nothing, and a re-run replaces instead of duplicating.
    let docs = memory_docs_from_path(path)?;
    let result = be.call("memory.import", &json!({ "docs": docs }))?;
    let text = format!(
        "Imported {} doc(s) into memory\n",
        result["imported"].as_u64().unwrap_or(0)
    );
    Ok((result, text))
}

/// Read `path` (a file, or a directory's direct `*.md` children) into
/// `memory.import` doc objects. Pure I/O — no store access — so the whole
/// failure surface of an import is exhausted before anything is written.
pub(crate) fn memory_docs_from_path(path: &str) -> Result<Vec<Value>, tasqx_core::ApiError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read {path}: {e}")))?;
    let files: Vec<std::path::PathBuf> = if meta.is_dir() {
        let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(path)
            .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read {path}: {e}")))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            // Case-insensitive: README.MD is a markdown file on every
            // platform, and skipping it silently on the OS whose filesystems
            // are case-insensitive was the exact wrong place to be strict.
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            })
            .collect();
        found.sort();
        if found.is_empty() {
            return Err(tasqx_core::ApiError::bad_request(format!(
                "no .md files found in {path} — memory import takes a markdown file or a \
                 directory containing them"
            )));
        }
        found
    } else {
        vec![std::path::PathBuf::from(path)]
    };

    let mut docs = Vec::new();
    for file in &files {
        let body = std::fs::read_to_string(file).map_err(|e| {
            tasqx_core::ApiError::bad_request(format!("cannot read {}: {e}", file.display()))
        })?;
        // A UTF-8 BOM would defeat the `# ` heading match below AND end up in
        // the stored body and the index; strip it once, here.
        let body = body.strip_prefix('\u{FEFF}').unwrap_or(&body);
        // Title: the first `# ` heading, else the file stem. The heading STAYS
        // in the body — the title is an index entry, not a cut.
        let title = body
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                file.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_string()
            });
        docs.push(json!({ "title": title, "body": body, "source": file.display().to_string() }));
    }
    Ok(docs)
}

pub(crate) fn run_export(be: &mut Backend, filter: &[String]) -> CmdOutcome {
    let mut params = json!({});
    if !filter.is_empty() {
        params["filter"] = Value::String(tasqx_core::filter::from_argv(filter));
    }
    let result = be.call("store.export", &params)?;
    // A filter selects a subset, so edges pointing out of it are trimmed to keep
    // the document self-contained. Warn on stderr, never stdout: stdout IS the
    // JSON and a note there would corrupt every pipe.
    let dropped = result
        .get("dropped_dependencies")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if dropped > 0 {
        eprintln!(
            "note: dropped {dropped} dependency edge(s) pointing outside the exported set; \
             widen the filter to keep them"
        );
    }
    // Human output IS the canonical JSON document (git-diffable, greppable).
    //
    // D37: the whole document, not just its `tasks` array. This is the surface
    // almost every user actually restores from, and printing one section of a
    // two-section document made the CLI lose exactly what the core had just
    // been taught to carry — projects, their archived state, and the default.
    // `import` has always accepted an object with a `tasks` key as well as a
    // bare array, so files written by this build and by every earlier one both
    // still restore; only the direction that can carry MORE has changed.
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&result).unwrap_or_default()
    );
    Ok((result, text))
}

pub(crate) fn run_import(be: &mut Backend, file: String) -> CmdOutcome {
    let raw = if file == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read stdin: {e}")))?;
        s
    } else {
        std::fs::read_to_string(&file)
            .map_err(|e| tasqx_core::ApiError::bad_request(format!("cannot read {file}: {e}")))?
    };
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| tasqx_core::ApiError::bad_request(format!("invalid JSON: {e}")))?;
    // Accept either a bare array (export output) or a {"tasks":[...]} object.
    // Anything else used to fall through to an empty array, so a truncated or
    // wrong file was answered with `Imported 0 task(s)` and exit 0 — the one
    // outcome a restore must never be told.
    let shape = |found: &str| {
        let src = if file == "-" {
            "stdin".to_string()
        } else {
            file.clone()
        };
        tasqx_core::ApiError::bad_request(format!(
            "cannot import {src}: {found} — expected the `export` shape, \
             a bare array of tasks or an object with a `tasks` array"
        ))
    };
    // D37: an object is forwarded WHOLE, not reduced to its `tasks` array. The
    // array was all a document used to hold; now it also carries `projects` and
    // `default_project`, and a verb that unwraps one section discards the rest —
    // silently, since the import would still report every task restored. A bare
    // array is still wrapped, because that is precisely what an older export is:
    // a document with no projects section, which `store.import` reads as "infer
    // them" rather than refusing.
    let params = match parsed {
        Value::Array(_) => json!({ "tasks": parsed }),
        Value::Object(ref o) => {
            if !o.contains_key("tasks") {
                return Err(shape("the JSON object has no `tasks` key"));
            }
            parsed.clone()
        }
        Value::String(_) => return Err(shape("the top level is a JSON string")),
        Value::Number(_) => return Err(shape("the top level is a JSON number")),
        Value::Bool(_) => return Err(shape("the top level is a JSON boolean")),
        Value::Null => return Err(shape("the top level is JSON null")),
    };
    let result = be.call("store.import", &params)?;
    let n = result.get("imported").and_then(Value::as_i64).unwrap_or(0);
    let p = result
        .get("projects_imported")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // A project row the caller did not send is a write they did not ask for, so
    // it is named on the human surface too, not only in the JSON (D37).
    let minted: Vec<&str> = result
        .get("projects_created")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let d = result
        .get("docs_imported")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // D39: `docs_imported` is computed and returned, so a human surface must
    // render it — a restore that also restored your memory docs and never said
    // so would make D41's export completeness unobservable. Mentioned only
    // when nonzero: pre-D41 documents carry no docs, and "0 doc(s)" on every
    // legacy restore is noise about a section the document never had.
    let mut text = if d > 0 {
        format!("Imported {n} task(s), {p} project(s), {d} memory doc(s)\n")
    } else {
        format!("Imported {n} task(s), {p} project(s)\n")
    };
    if !minted.is_empty() {
        text.push_str(&format!(
            "note: the document carried no `projects` section, so {} created from the tasks: {}\n",
            if minted.len() == 1 {
                "1 project was"
            } else {
                "projects were"
            },
            minted.join(", ")
        ));
    }
    Ok((result, text))
}

pub(crate) fn run_next(be: &mut Backend, ctx: &Ctx) -> CmdOutcome {
    // @working already excludes blocked tasks; highest urgency first, take one.
    let params = json!({ "filter": "@working", "sort": ["-urgency"], "limit": 1 });
    let result = be.call("task.list", &params)?;
    let text = render::next_task(ctx, &result);
    Ok((result, text))
}

pub(crate) fn run_why(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let result = be.call("task.get", &json!({ "ref": r#ref }))?;
    let text = render::why(ctx, &result);
    Ok((result, text))
}

/// `tasqx chart <kind>`: read the event log and render a native terminal chart.
/// `tasqx chart throughput|heatmap|burndown`.
///
/// Each arm computes its SERIES once and hands the same values to both the
/// renderer and the JSON. The series is the answer; the sparkline is one way of
/// looking at it, and a script that wants the numbers should not have to parse
/// block glyphs back into integers to get them.
pub(crate) fn run_chart(engine: &Engine, ctx: &Ctx, kind: ChartKind) -> CmdOutcome {
    let anchor = chart::today();
    Ok(match kind {
        ChartKind::Throughput { weeks } => {
            let weeks = chart::default_weeks(false, weeks);
            let events = events_since(engine, anchor, weeks * 7 + 7)?;
            let series = chart::throughput(&events, weeks, anchor);
            let data = series
                .iter()
                .map(|b| {
                    json!({ "iso_year": b.iso_year, "iso_week": b.iso_week, "label": b.label(),
                            "added": b.added, "done": b.done, "net": b.net() })
                })
                .collect::<Vec<_>>();
            (
                json!({ "chart": "throughput", "weeks": weeks, "series": data }),
                chart::render_throughput(ctx, &series),
            )
        }
        ChartKind::Heatmap { year, weeks } => {
            let weeks = chart::default_weeks(year, weeks);
            let events = events_since(engine, anchor, weeks * 7 + 7)?;
            let days = chart::heatmap(&events, weeks, anchor);
            let data = days
                .iter()
                .map(|d| json!({ "date": d.date.to_string(), "count": d.count }))
                .collect::<Vec<_>>();
            (
                json!({ "chart": "heatmap", "weeks": weeks, "series": data,
                        "current_streak": chart::current_streak(&days, anchor),
                        "best_streak": chart::best_streak(&days) }),
                chart::render_heatmap(ctx, &days, anchor),
            )
        }
        ChartKind::Burndown { project, days } => {
            let days_n = days.unwrap_or(30);
            // Reported, never swallowed: an unresolvable scope used to render as
            // a cleared burndown, which is a wrong answer wearing the costume of
            // a right one.
            let (members, label) = burndown_members(engine, &project)?;
            let events = events_since(engine, anchor, days_n + 1)?;
            let series = chart::burndown(&events, &members, days_n, anchor);
            let data = series
                .iter()
                .map(|p| json!({ "date": p.date.to_string(), "remaining": p.remaining }))
                .collect::<Vec<_>>();
            (
                json!({ "chart": "burndown", "days": days_n, "scope": label, "series": data }),
                chart::render_burndown(ctx, &series, &label),
            )
        }
    })
}

/// `tasqx report --html`: write the self-contained HTML review.
///
/// The scope comes from [`report_params`] — the SAME builder the terminal path
/// uses — so the two output modes of one command cannot answer different
/// questions again. `all` is hard `false` rather than a parameter because clap
/// already rejects `--all` alongside `--html`; spelling it here keeps the two
/// facts in one place instead of accepting a flag we would then ignore.
pub(crate) fn run_html_report(
    engine: &Engine,
    ctx: &Ctx,
    args: Vec<String>,
    out: Option<String>,
) -> CmdOutcome {
    let params = report_params(&args, false);
    let doc = html::generate(engine, &ctx.theme, &params)?;
    match out {
        Some(path) => {
            if let Some(parent) = PathBuf::from(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, &doc) {
                // The machine-relevant fact of this mode is where the file landed
                // — the one thing a script needs in order to do anything next.
                Ok(()) => Ok((
                    json!({ "path": path, "bytes": doc.len() }),
                    format!("Wrote self-contained HTML report → {path}\n"),
                )),
                Err(e) => Err(ApiError::internal(format!("cannot write {path}: {e}"))),
            }
        }
        None => Ok((
            json!({ "path": Value::Null, "bytes": doc.len(), "html": doc.clone() }),
            doc,
        )),
    }
}
