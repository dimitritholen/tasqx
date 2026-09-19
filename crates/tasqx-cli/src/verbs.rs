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

/// #228.15: `import -` reads canonical JSON from stdin; `add -` used to see no
/// such convention and file a task literally titled "-", silently swallowing
/// whatever was piped in. `-` means stdin everywhere else in this binary, and
/// `add` has no bulk path of its own for a caller to reach for instead, so a
/// bare `-` is refused here rather than accepted as a title one character
/// long — pointing at the surface that already owns bulk input, rather than
/// teaching `add` a second, partial one (a canonical-JSON round trip already
/// covers the batch case via `tasqx export | tasqx import -`).
const ADD_DASH_IS_NOT_A_TITLE: &str = "`add` does not read stdin; `-` would become a task titled \
     \"-\". Pipe canonical JSON into `tasqx import -` instead (see `tasqx import --help`).";

pub(crate) fn run_add(
    be: &mut Backend,
    ctx: &Ctx,
    title: Vec<String>,
    flags: sugar::AddFlags,
) -> CmdOutcome {
    if title == ["-"] {
        return Err(ApiError::bad_request(ADD_DASH_IS_NOT_A_TITLE));
    }
    // argv goes in unjoined: the shell's argument boundaries are information the
    // parser needs (see `sugar::parse_add`), and joining destroys them.
    let parsed = sugar::parse_add(&title, flags, sugar::ParseContext::Add)?;
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
    if let Some(n) = parsed.budget_tokens {
        params["budget_tokens"] = json!(n);
    }
    if !parsed.tags.is_empty() {
        params["tags"] = Value::Array(parsed.tags.into_iter().map(Value::String).collect());
    }
    let result = be
        .call("task.add", &params)
        .map_err(|e| name_the_cut(e, cut.as_deref()))?;
    // The echo is `add`'s card (D122, D126) on both paths, and the card
    // wants fields `task.add`'s frozen five-field result does not carry —
    // tags, due, priority, estimate — so the task is read back, the same
    // composite shape `modify` uses for its follow-up `tag.add`. A failed
    // read-back falls back to the result plus the title that was typed: the
    // add succeeded, and the echo failing must not turn that into a red exit.
    let mut fallback = result.clone();
    fallback["title"] = Value::String(parsed.title.clone());
    let task = read_back(be, &result).unwrap_or(fallback);
    let text = render::added(ctx, &task, crate::clock::now());
    Ok((result, text))
}

/// The task a write just touched, as `task.get` reads it back (D126): the
/// echo is a card, and D56 froze the write results without the facts a card
/// draws. `None` when the read fails; every caller then renders from the
/// write's own result rather than erroring, since the write succeeded.
pub(crate) fn read_back(be: &mut Backend, result: &Value) -> Option<Value> {
    result
        .get("short_id")
        .and_then(Value::as_i64)
        .and_then(|sid| be.call("task.get", &json!({ "ref": sid })).ok())
}

/// The titles of the other tasks a write moved (auto-stopped, released,
/// re-blocked), for the lines under its card. A task that cannot be read
/// prints its id alone.
pub(crate) fn titles_of(be: &mut Backend, ids: &[i64]) -> render::Titles {
    ids.iter()
        .filter_map(|&n| {
            let t = be.call("task.get", &json!({ "ref": n })).ok()?;
            Some((n, t.get("title")?.as_str()?.to_string()))
        })
        .collect()
}

/// The short ids in one array field of a write's result.
pub(crate) fn ids_in(result: &Value, key: &str, inner: Option<&str>) -> Vec<i64> {
    result
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| match inner {
                    Some(k) => v.get(k).and_then(Value::as_i64),
                    None => v.as_i64(),
                })
                .collect()
        })
        .unwrap_or_default()
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
    let parsed = sugar::parse_add(&rest, flags, sugar::ParseContext::Modify)?;
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
    // `parsed.tracked` is always `flags.tracked` verbatim (D98): a correction
    // is meaningful only on an existing task, so it has no inline sugar and
    // `run_add` always passes `None`.
    if let Some(t) = parsed.tracked {
        guard_set_and_clear(&set, "tracked", &t)?;
        set.insert(
            "tracked".into(),
            Value::String(datetime::parse_duration(&t)?),
        );
    }

    // D139, the same shape as `tracked` above: flag-only, so `run_add` passes
    // `None` and the scanner never fills it.
    if let Some(n) = parsed.budget_tokens {
        guard_set_and_clear(&set, "budget_tokens", &n.to_string())?;
        set.insert("budget_tokens".into(), json!(n));
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

    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let text = render::modified(ctx, &task, &set, &parsed.tags, crate::clock::now());
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

/// `sort`/`limit`/`offset`/`fields` map straight onto `task.list`'s own
/// params (`core.capabilities` has always listed all five; only the CLI
/// lacked a door to four of them). Absent (`sort` empty, `limit`/`offset`
/// `None`, `fields` empty) means exactly what it always meant, byte-for-byte
/// — a bare `tasqx list` still sends `{"sort":["-urgency"]}` and nothing
/// else, so this is additive rather than a reshaping of the old request.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_list(
    be: &mut Backend,
    ctx: &Ctx,
    filter: &[String],
    sort: &[String],
    limit: Option<u64>,
    offset: Option<u64>,
    fields: &[String],
) -> CmdOutcome {
    // Bare `tasqx` (and `tasqx list` with no filter) => the working set.
    // Otherwise `from_argv`, never `join(" ")`: the shell's argument boundaries
    // are information the filter parser needs, exactly as on the write path
    // (see `sugar::parse_add`). Joining loses which spaces the user quoted.
    //
    // #149: a filter argv that joins to nothing (no argument at all, or one
    // that is empty or whitespace-only) takes the SAME `@working` default —
    // checked on the joined text, not on `filter.is_empty()` alone. `tasqx
    // list "$FILTER"` is the shape of every wrapper script and every
    // agent-generated command, and an unset or empty `$FILTER` hands this
    // function one empty-string argument rather than none. That argument used
    // to reach the engine as the literal empty filter, which D27/D35 correctly
    // read as "no filter" — matching everything, done and cancelled included —
    // silently widening "my open work" into the whole store with no error and
    // no way to tell the two answers apart in the output. The engine's own
    // reading of `filter:""` is untouched (D35's recorded exception still
    // holds for `tasqx api`/MCP callers); this is CLI sugar deciding what
    // string to send, same layer `filter.is_empty()` already lived at.
    let joined = tasqx_core::filter::from_argv(filter);
    let filter_str = if joined.trim().is_empty() {
        "@working".to_string()
    } else {
        joined
    };
    let sort_keys: Vec<&str> = if sort.is_empty() {
        vec!["-urgency"]
    } else {
        sort.iter().map(String::as_str).collect()
    };
    let mut params = json!({ "filter": filter_str, "sort": sort_keys });
    if let Some(limit) = limit {
        params["limit"] = json!(limit);
    }
    if let Some(offset) = offset {
        params["offset"] = json!(offset);
    }
    if !fields.is_empty() {
        params["fields"] = json!(fields);
    }
    let result = be.call("task.list", &params)?;
    // Finding #4 (audit-2026-09): an empty result said only "No tasks.",
    // giving no way to tell "nothing pending" from "this filter excludes
    // everything" — the same distinction D55 already drew for `pick`.
    let text = render::task_table_filtered(ctx, &result, crate::clock::now(), Some(&filter_str));
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
    let now = crate::clock::now();
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
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let titles = titles_of(be, &ids_in(&result, "auto_stopped", Some("short_id")));
    let text = render::started(ctx, &result, &task, &titles, crate::clock::now());
    Ok((result, text))
}

pub(crate) fn run_stop(be: &mut Backend, ctx: &Ctx, r#ref: String) -> CmdOutcome {
    let params = json!({ "ref": r#ref });
    let result = be.call("task.stop", &params)?;
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let text = render::stopped(ctx, &result, &task, crate::clock::now());
    Ok((result, text))
}

/// Widen a `task.done` params object with whichever self-report facts were
/// given on the command line (#13, D50/D65).
///
/// Present keys only, same discipline as [`apply_correlation`]: a flagless
/// `tasqx done 4` sends byte-for-byte the object it sent before these flags
/// existed. Token counts are sent as JSON numbers, not strings — clap already
/// parsed and typed them as `i64`.
pub(crate) fn apply_self_report(params: &mut Value, r: &command::SelfReportArgs) {
    for (key, value) in [("tool", &r.tool), ("model", &r.model)] {
        if let Some(v) = value {
            params[key] = json!(v);
        }
    }
    for (key, value) in [
        ("input_tokens", r.input_tokens),
        ("output_tokens", r.output_tokens),
        ("cache_read_tokens", r.cache_read_tokens),
        ("cache_creation_tokens", r.cache_creation_tokens),
        ("total_tokens", r.total_tokens),
    ] {
        if let Some(v) = value {
            params[key] = json!(v);
        }
    }
}

pub(crate) fn run_done(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    force: bool,
    correlation: &command::CorrelationArgs,
    self_report: &command::SelfReportArgs,
) -> CmdOutcome {
    let mut params = json!({ "ref": r#ref });
    if force {
        params["force"] = json!(true);
    }
    apply_correlation(&mut params, correlation);
    apply_self_report(&mut params, self_report);
    let result = be.call("task.done", &params)?;
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let titles = titles_of(be, &ids_in(&result, "unblocked", None));
    let text = render::done(ctx, &result, &task, &titles, crate::clock::now());
    // The hint about the write goes to stderr, after the card (D126): the
    // card is the first thing under the prompt, and core's paragraph was 190
    // cells of stdout under every completion. `--json` keeps it whole.
    // D139's overrun goes out FIRST, on the same stderr channel and by the
    // same rule: it is the one thing on this response that might change what
    // the reader does next, and unlike the tokens hint it appears precisely
    // when the spend IS known.
    // D138's unproven completion, on the same stderr channel as the other two
    // and first of the three: an open criterion is the one that says the work
    // may not actually be finished.
    // D150's forced completion goes out first of all, on the same stderr
    // channel: `--force` overrode an edge the store held the caller to, and
    // naming which blockers were overridden is the one fact that says this
    // "done" did not take the normal path.
    if result.get("forced").and_then(Value::as_bool) == Some(true) {
        let blocked_by = ids_in(&result, "blocked_by", Some("short_id"));
        let unicode = crate::theme::Caps::detect_stderr().unicode;
        if let Some(note) =
            render::forced_note(&blocked_by, crate::theme::detect_stderr_cols(), unicode)
        {
            crate::note_after_output(note);
        }
    }
    if let Some(hint) = result.get("checks_hint").and_then(Value::as_str) {
        let unicode = crate::theme::Caps::detect_stderr().unicode;
        if let Some(note) = render::checks_note(hint, crate::theme::detect_stderr_cols(), unicode) {
            crate::note_after_output(note);
        }
    }
    if let Some(hint) = result.get("budget_hint").and_then(Value::as_str) {
        let unicode = crate::theme::Caps::detect_stderr().unicode;
        if let Some(note) = render::budget_note(hint, crate::theme::detect_stderr_cols(), unicode) {
            crate::note_after_output(note);
        }
    }
    if let Some(note) = result
        .get("tokens_hint")
        .and_then(Value::as_str)
        .and_then(|h| {
            let unicode = crate::theme::Caps::detect_stderr().unicode;
            render::tokens_note(h, crate::theme::detect_stderr_cols(), unicode)
        })
    {
        crate::note_after_output(note);
    }
    Ok((result, text))
}

pub(crate) fn run_show(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    card: bool,
    ascii: bool,
) -> CmdOutcome {
    let result = be.call("task.get", &json!({ "ref": r#ref }))?;
    let text = if card {
        tasqx_core::markdown::task_card(&result, &card_opts(ctx, ascii))
    } else {
        render::task_detail(ctx, &result, crate::clock::now())
    };
    Ok((result, text))
}

/// [`CardOpts`] for `--card`/`--ascii` (D146): borders never follow
/// `ctx.caps.unicode` — the card is a document, not a screen, and
/// `tasqx show 42 --card | pbcopy` is the main use, where a pipe makes caps
/// `PLAIN`. `--ascii` is the explicit fallback instead.
fn card_opts(ctx: &Ctx, ascii: bool) -> CardOpts {
    CardOpts {
        detail: DetailOpts {
            time: ctx.time_format,
            now: crate::clock::now(),
        },
        borders: if ascii {
            Borders::Ascii
        } else {
            Borders::Unicode
        },
    }
}

/// #624: an all-digit word is a check's 1-based `position` — a uuid never is —
/// so `tasqx check set 42 2 passed` works without copying an id.
fn check_named(r#ref: &str, check_id: &str) -> Value {
    match check_id.parse::<i64>() {
        Ok(n) if check_id.bytes().all(|b| b.is_ascii_digit()) => {
            json!({ "ref": r#ref, "position": n })
        }
        _ => json!({ "ref": r#ref, "check_id": check_id }),
    }
}

/// `tasqx check add|set|rm` (D138) — acceptance criteria on a task.
pub(crate) fn run_check(be: &mut Backend, ctx: &Ctx, action: &CheckAction) -> CmdOutcome {
    let (method, params) = match action {
        CheckAction::Add { r#ref, body } => {
            let body = body.join(" ");
            if body.trim().is_empty() {
                return Err(ApiError::bad_request(
                    "a check needs a criterion — what has to be true for this task to be done?",
                ));
            }
            ("check.add", json!({ "ref": r#ref, "body": body }))
        }
        CheckAction::Set {
            r#ref,
            check_id,
            state,
            evidence,
        } => {
            let mut p = check_named(r#ref, check_id);
            p["state"] = json!(state);
            if let Some(e) = evidence {
                p["evidence"] = Value::String(e.clone());
            }
            ("check.set", p)
        }
        CheckAction::Remove { r#ref, check_id } => ("check.remove", check_named(r#ref, check_id)),
    };
    let result = be.call(method, &params)?;
    // The echo is the task's card (D126), like every other write on a task:
    // a criterion only means anything beside the work it qualifies.
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let text = render::task_detail(ctx, &task, crate::clock::now());
    Ok((result, text))
}

/// `tasqx brief <ref>` (D136) — the task, what its prerequisites concluded,
/// and memory under a query derived from the task itself.
pub(crate) fn run_brief(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    memory_limit: Option<u64>,
    card: bool,
    ascii: bool,
) -> CmdOutcome {
    let mut params = json!({ "ref": r#ref });
    if let Some(n) = memory_limit {
        params["memory_limit"] = json!(n);
    }
    let result = be.call("task.brief", &params)?;
    let text = if card {
        tasqx_core::markdown::task_brief_card(&result, &card_opts(ctx, ascii))
    } else {
        render::task_brief(ctx, &result, crate::clock::now())
    };
    Ok((result, text))
}

/// A method taking only `{ref}` and returning `{short_id, status}`:
/// `task.cancel` and `task.reopen`, and the dependents each moved.
pub(crate) fn run_simple_ref(
    be: &mut Backend,
    ctx: &Ctx,
    method: &str,
    r#ref: String,
) -> CmdOutcome {
    let result = be.call(method, &json!({ "ref": r#ref }))?;
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let mut moved = ids_in(&result, "unblocked", None);
    moved.extend(ids_in(&result, "blocked", None));
    let titles = titles_of(be, &moved);
    let verb = match method {
        "task.cancel" => "cancelled",
        "task.reopen" => "reopened",
        other => other,
    };
    let text = render::status_changed(ctx, verb, &result, &task, &titles, crate::clock::now());
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
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    // An undone `undep` puts a blocker back; the echo names it, so its title
    // is read here the way every other verb reads the tasks it moved.
    let blocker: Vec<i64> = result
        .get("restored")
        .and_then(|r| r.get("depends_on"))
        .and_then(Value::as_i64)
        .into_iter()
        .collect();
    let titles = titles_of(be, &blocker);
    let text = render::undone(ctx, &result, &task, &titles, crate::clock::now());
    Ok((result, text))
}

/// `tasqx annotate` — add a note, or with `--edit <id>` correct one in place
/// (`annotation.update`, D165).
pub(crate) fn run_annotate(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    edit: Option<String>,
    text: Vec<String>,
) -> CmdOutcome {
    let body = text.join(" ");
    let (result, word) = match edit {
        Some(id) => (
            be.call(
                "annotation.update",
                &json!({ "ref": r#ref, "annotation_id": id, "body": body }),
            )?,
            "note edited",
        ),
        None => (
            be.call("annotation.add", &json!({ "ref": r#ref, "body": body }))?,
            "annotated",
        ),
    };
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let out = render::annotated(ctx, &result, &task, crate::clock::now(), word);
    Ok((result, out))
}

/// `tasqx unannotate` — scrub one annotation's text by id (D113).
///
/// A hard delete, not a hide: `annotation.remove` overwrites the body in the
/// store, and the answer carries only the id and the removal instant — never
/// the text, since echoing it back would recreate the leak this verb exists to
/// close.
pub(crate) fn run_unannotate(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    annotation_id: String,
) -> CmdOutcome {
    let result = be.call(
        "annotation.remove",
        &json!({ "ref": r#ref, "annotation_id": annotation_id }),
    )?;
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let out = render::annotation_removed(ctx, &task, crate::clock::now());
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
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let now = crate::clock::now();
    let text = render::tag_changed(ctx, &result, &task, method == "tag.add", &names, now);
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
    let task = read_back(be, &result).unwrap_or_else(|| result.clone());
    let added = method == "dependency.add";
    let text = render::dep_changed(ctx, &result, &task, added, &depends_on, crate::clock::now());
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
/// `--all`/`--since`/`--until` flags. Split out of [`run_report`] so the
/// CLI→core contract can be asserted without standing up a backend.
///
/// `since`/`until` window WHEN the tracked time or token spend happened
/// (D97) — a different axis from the `completed.after:`/`completed.before:`
/// terms `rest` may carry, which window by task completion date instead.
/// Resolved through the one date parser every other date-shaped flag already
/// uses, at the real call-time `now` (D33), so `tasqx report --since -7d`
/// fails with a nameable message rather than a round trip to the engine.
pub(crate) fn report_params(
    args: &[String],
    all: bool,
    since: Option<String>,
    until: Option<String>,
    now: jiff::Timestamp,
) -> Result<Value, ApiError> {
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
        } else if !first.chars().any(|c| ":+-@".contains(c)) {
            // Finding #11 (audit-2026-09): a bare word carrying no filter
            // sigil is a group_by ATTEMPT, not a filter term that happens to
            // be spelled wrong — `tasqx report tags` used to fall straight
            // into the filter parser and get back a wall of filter grammar
            // that never mentions grouping at all. Diagnose it here, naming
            // the three real axes, before the filter parser ever sees it. A
            // token that DOES look like filter syntax (`project:x`, `+api`,
            // `-tag`, `@working`) still falls through unchanged.
            return Err(ApiError::bad_request(format!(
                "unknown group_by {first:?} (expected {})",
                tasqx_core::engine::SUMMARY_GROUP_BY.join(", ")
            )));
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
    if let Some(s) = since {
        params["since"] = Value::String(datetime::parse_when(&s, now)?);
    }
    if let Some(u) = until {
        params["until"] = Value::String(datetime::parse_when(&u, now)?);
    }
    Ok(params)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_report(
    be: &mut Backend,
    ctx: &Ctx,
    args: Vec<String>,
    all: bool,
    since: Option<String>,
    until: Option<String>,
    metrics: Option<Vec<String>>,
    outcomes: bool,
) -> CmdOutcome {
    let mut params = report_params(&args, all, since, until, now_ts())?;
    let group_by = params["group_by"]
        .as_str()
        .unwrap_or(tasqx_core::engine::SUMMARY_GROUP_BY[0])
        .to_string();
    if outcomes {
        // `report_params` seeds `metrics` with `SUMMARY_METRICS`, which is the
        // other method's vocabulary — sent here it would be refused by name
        // (D34), so it is dropped rather than translated. Omitting `metrics`
        // is what asks `report.outcomes` for all of its own, which is the
        // intended reading of the report anyway (D137). `all` is rejected at
        // the clap layer, so nothing removes it here.
        if let Some(obj) = params.as_object_mut() {
            obj.remove("metrics");
        }
        let result = be.call("report.outcomes", &params)?;
        let text = render::outcomes(ctx, &result, &group_by);
        return Ok((result, text));
    }
    let result = be.call("report.summary", &params)?;
    let text = render::report(ctx, &result, &group_by, metrics.as_deref());
    Ok((result, text))
}

/// `tasqx memory add|search|rm|import` (DESIGN.md §12-D41).
pub(crate) fn run_memory(be: &mut Backend, ctx: &Ctx, action: &MemoryAction) -> CmdOutcome {
    match action {
        MemoryAction::Add {
            title,
            body,
            source,
            project,
            standing,
        } => {
            let mut params = json!({ "title": title, "body": body });
            if let Some(s) = source {
                params["source"] = json!(s);
            }
            if let Some(p) = project {
                params["project"] = json!(p);
            }
            if *standing {
                params["standing"] = json!(true);
            }
            let result = be.call("memory.add", &params)?;
            let mut text = format!(
                "Stored {}  ·  {}\n",
                render::san(result["id"].as_str().unwrap_or("?")),
                render::san(title)
            );
            // D156: the soft-cap hint is the engine's only word that a scope
            // is overfull, and it is present only past the cap — a JSON
            // caller reads the key, a person reads this line (PR #46 review).
            if let Some(hint) = result["hint"].as_str() {
                text.push_str(&render::san(hint));
                text.push('\n');
            }
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
            let text = render::memory_hits(ctx, &result, &query.join(" "), *raw);
            Ok((result, text))
        }
        MemoryAction::Show { id } => {
            let result = be.call("memory.get", &json!({ "id": id }))?;
            let source = result["source"].as_str().unwrap_or("—");
            // D135: `body` in the JSON result is exactly what was stored —
            // `--json` on this same command must show the fence, if any, so
            // a caller can round-trip it. The TEXT rendering, like the D121
            // memory browser's own, reads a leading frontmatter block as
            // `key  value` prose instead: one reader should not have to
            // parse `---`/`key:` lines the browser already stopped printing.
            let body = tasqx_core::frontmatter::flatten(result["body"].as_str().unwrap_or(""));
            let text = format!(
                "{}  ({})\n{}\n",
                render::san(result["title"].as_str().unwrap_or("?")),
                render::san(source),
                render::san_multiline(&body),
            );
            Ok((result, text))
        }
        MemoryAction::Rm { id } => {
            let result = be.call("memory.remove", &json!({ "id": id }))?;
            let text = format!("Removed {}\n", render::san(id));
            Ok((result, text))
        }
        MemoryAction::Import { path, project } => run_memory_import(be, path, project.as_deref()),
        MemoryAction::List {
            limit,
            offset,
            project,
            standing,
        } => {
            let mut params = json!({ "offset": offset });
            if let Some(n) = limit {
                params["limit"] = json!(n);
            }
            if let Some(p) = project {
                params["project"] = json!(p);
            }
            if *standing {
                params["standing"] = json!(true);
            }
            let result = be.call("memory.list", &params)?;
            let text = render::memory_table(ctx, &result, crate::clock::now());
            Ok((result, text))
        }
        MemoryAction::Update {
            id,
            title,
            body,
            source,
            project,
            standing,
            expected_rev,
        } => {
            let mut params = json!({ "id": id });
            if let Some(t) = title {
                params["title"] = json!(t);
            }
            if let Some(b) = body {
                params["body"] = json!(b);
            }
            if let Some(s) = source {
                params["source"] = json!(s);
            }
            if let Some(p) = project {
                params["project"] = json!(p);
            }
            if let Some(st) = standing {
                params["standing"] = json!(st);
            }
            if let Some(rev) = expected_rev {
                params["expected_rev"] = json!(rev);
            }
            let result = be.call("memory.update", &params)?;
            let text = format!(
                "Updated {}  ·  {}  (rev {})\n",
                render::san(result["id"].as_str().unwrap_or("?")),
                render::san(result["title"].as_str().unwrap_or("?")),
                result["_rev"].as_i64().unwrap_or(0)
            );
            Ok((result, text))
        }
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
        // D167: a count that arrives after completion, without a hand-built
        // `tasqx api` envelope. A person typing a number is a self-report, so
        // it is stored as one, at the grade `task.done` gives the same claim.
        TokensAction::Add {
            r#ref,
            total,
            input,
            output,
            cache_read,
            cache_creation,
            tool,
            model,
        } => {
            let mut params = json!({
                "ref": r#ref,
                "tool": tool,
                "source": tasqx_core::tokens::SOURCE_SELF_REPORT,
                "confidence": tasqx_core::tokens::CONFIDENCE_MEDIUM,
            });
            if let Some(m) = model {
                params["model"] = json!(m);
            }
            for (key, value) in [
                ("total_tokens", total),
                ("input_tokens", input),
                ("output_tokens", output),
                ("cache_read_tokens", cache_read),
                ("cache_creation_tokens", cache_creation),
            ] {
                if let Some(v) = value {
                    params[key] = json!(v);
                }
            }
            let result = be.call("token.add", &params)?;
            let text = render::token_added(&result);
            Ok((result, text))
        }
    }
}

/// One doc per file. A directory imports its direct `*.md` children; finding
/// none is an error, not `Imported 0` at exit 0 — the same never-say-nothing
/// rule `import` learned for truncated task files.
pub(crate) fn run_memory_import(be: &mut Backend, path: &str, project: Option<&str>) -> CmdOutcome {
    // Two-phase (review finding): ALL file I/O and title derivation happen
    // before a single write, then one `memory.import` lands the batch in one
    // transaction with replace-by-source semantics — a failure imports
    // nothing, and a re-run replaces instead of duplicating.
    let docs = memory_docs_from_path(path)?;
    let mut params = json!({ "docs": docs });
    if let Some(p) = project {
        params["project"] = json!(p);
    }
    let result = be.call("memory.import", &params)?;
    let imported = result["imported"].as_u64().unwrap_or(0);
    // #178: a re-run that replaces a doc sharing its `source` used to print
    // this identical line whether it created 3 docs or silently overwrote 3
    // — the only announcement was `undo`'s refusal, reached only by someone
    // who thought to try. `replaced` is counted by the engine either way, so
    // rendering it here is the one thing on the write side that was missing.
    let replaced = result["replaced"].as_u64().unwrap_or(0);
    let text = if replaced > 0 {
        format!(
            "Imported {imported} doc(s) into memory ({replaced} replaced; the previous text is \
             not recoverable)\n"
        )
    } else {
        format!("Imported {imported} doc(s) into memory\n")
    };
    Ok((result, text))
}

/// The title a frontmatter block would have given the document, if any:
/// `title:` or `name:` (`title` first), a bare or single-quoted scalar value.
/// Deliberately not a YAML parser — the values this needs to read are the
/// simple ones a memory doc's frontmatter actually carries, and a partial
/// parser that silently mis-reads a list or a block scalar would be worse
/// than not trying.
fn frontmatter_title(fm: &str) -> Option<String> {
    for key in ["title", "name"] {
        for line in fm.lines() {
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let Some(value) = rest.trim_start().strip_prefix(':') else {
                continue;
            };
            let value = value.trim().trim_matches(['"', '\'']);
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
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
        // #228.4: YAML frontmatter was indexed and shown as document body —
        // every file in a `~/.claude/.../memory/` directory (the corpus this
        // importer's own `--help` example points at, `docs/adr`, is the same
        // idiom) opens with one, and `originSessionId`/`modified`/`type` then
        // dominated search snippets over the prose that answers the query.
        // Cut before the title/heading scan below, so a frontmatter `title:`
        // does not race the body's own `# ` heading. `frontmatter::block` is
        // the one fence-finder (task #12/D135) — shared with the memory
        // browser's own renderer and `render::doc_summary` instead of each
        // reading `---\n...\n---\n` its own way.
        let (frontmatter, body) = match tasqx_core::frontmatter::block(body) {
            Some((fm, rest)) => (Some(fm), rest),
            None => (None, body),
        };
        // Title: the first `# ` heading, else frontmatter's `title:`/`name:`,
        // else the file stem. The heading STAYS in the body — the title is an
        // index entry, not a cut. Frontmatter is cut; nothing there is prose
        // meant to be read.
        let title = body
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from)
            .or_else(|| frontmatter.and_then(frontmatter_title))
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

pub(crate) fn run_export(
    be: &mut Backend,
    filter: &[String],
    include_unscoped: bool,
) -> CmdOutcome {
    let mut params = json!({});
    if !filter.is_empty() {
        params["filter"] = Value::String(tasqx_core::filter::from_argv(filter));
    }
    if include_unscoped {
        params["include_unscoped"] = Value::Bool(true);
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
        // Sized from stderr, where it goes: stdout is usually a file here.
        let unicode = crate::theme::Caps::detect_stderr().unicode;
        eprintln!(
            "{}",
            render::export_note(dropped, crate::theme::detect_stderr_cols(), unicode)
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

pub(crate) fn run_import(be: &mut Backend, ctx: &Ctx, file: String) -> CmdOutcome {
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
    let text = render::imported(ctx, &result);
    Ok((result, text))
}

/// `tasqx next [filter…]` — the "what now" button, optionally scoped.
///
/// Unlike `list`/`pick`, the caller's filter does not REPLACE `@working`; it is
/// ANDed onto it. `next`'s whole value is skipping blocked and backlog work, and
/// a caller who narrows to one project still wants that: `tasqx next
/// project:fin-9695` must not resurrect a blocked task in that project the way
/// `tasqx list project:fin-9695` (which shows every status once a filter is
/// given) would if used as a substitute. Parenthesised so a caller's own `or`
/// binds correctly (`@working and (a or b)`, not `@working and a or b`).
pub(crate) fn run_next(
    be: &mut Backend,
    ctx: &Ctx,
    filter: &[String],
    card: bool,
    ascii: bool,
) -> CmdOutcome {
    // @working already excludes blocked tasks; highest urgency first, take one.
    let filter_str = if filter.is_empty() {
        "@working".to_string()
    } else {
        format!("@working and ({})", tasqx_core::filter::from_argv(filter))
    };
    let params = json!({ "filter": filter_str, "sort": ["-urgency"], "limit": 1 });
    let result = be.call("task.list", &params)?;
    if card {
        // A `task.list` row carries none of what a card draws — no checks, no
        // unmet_blockers, no annotations (D146's card wants what `show` reads,
        // not what `list` rows carry) — so `--card` re-reads the picked task
        // through `task.get`, same as `card_opts`' callers in `run_show`. When
        // that succeeds, the task.get result becomes BOTH the card and the
        // JSON half of this call: `tasqx next --json --card` answers with the
        // full task, not the `task.list` envelope plain `next` returns.
        let picked = result
            .get("tasks")
            .and_then(Value::as_array)
            .and_then(|tasks| tasks.first());
        if let Some(full) = picked.and_then(|t| read_back(be, t)) {
            let text = tasqx_core::markdown::task_card(&full, &card_opts(ctx, ascii));
            return Ok((full, text));
        }
    }
    let text = render::next_task(ctx, &result, crate::clock::now());
    Ok((result, text))
}

pub(crate) fn run_why(
    be: &mut Backend,
    ctx: &Ctx,
    r#ref: String,
    card: bool,
    ascii: bool,
) -> CmdOutcome {
    // #150: `--json` used to be a bare `task.get` result, which never carried
    // the terms `urgency` sums — only the total the human form already showed.
    // `explain: true` is the additive opt-in (D56/D1) that puts the breakdown
    // on the wire, so the machine form answers the question the command name
    // promises instead of handing back the one number the caller already had.
    let result = be.call("task.get", &json!({ "ref": r#ref, "explain": true }))?;
    let text = if card {
        // The card already carries the title as its header row, so `--card`
        // follows it with `why_terms` alone rather than `why`'s own header +
        // terms — printing the title twice would be the one thing a reader
        // pasting this into a document does not want repeated.
        format!(
            "{}\n{}",
            tasqx_core::markdown::task_card(&result, &card_opts(ctx, ascii)),
            render::why_terms(ctx, &result, crate::clock::now())
        )
    } else {
        render::why(ctx, &result, crate::clock::now())
    };
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
        ChartKind::Throughput { filter, weeks } => {
            let weeks = chart::default_weeks(false, weeks);
            // At least 5 weeks back regardless of the display window (#234
            // item 4): the 4-wk velocity is always computed over the last four
            // COMPLETE ISO weeks, which `--weeks 1` alone would clip.
            let events = events_since(engine, anchor, weeks.max(5) * 7 + 7)?;
            let (members, _) = burndown_members(engine, &filter)?;
            let series = chart::throughput(&events, &members, weeks, anchor);
            let velocity = chart::velocity_4wk(&events, &members, anchor);
            let data = series
                .iter()
                .map(|b| {
                    json!({ "iso_year": b.iso_year, "iso_week": b.iso_week, "label": b.label(),
                            "added": b.added, "done": b.done, "net": b.net() })
                })
                .collect::<Vec<_>>();
            (
                json!({ "chart": "throughput", "weeks": weeks, "series": data,
                        "velocity_4wk": velocity }),
                chart::render_throughput(ctx, &series, velocity, members.is_empty()),
            )
        }
        ChartKind::Heatmap {
            filter,
            year,
            weeks,
        } => {
            let weeks = chart::default_weeks(year, weeks);
            let events = events_since(engine, anchor, weeks * 7 + 7)?;
            let (members, _) = burndown_members(engine, &filter)?;
            let days = chart::heatmap(&events, &members, weeks, anchor);
            let data = days
                .iter()
                .map(|d| json!({ "date": d.date.to_string(), "count": d.count }))
                .collect::<Vec<_>>();
            (
                json!({ "chart": "heatmap", "weeks": weeks, "series": data,
                        "current_streak": chart::current_streak(&days, anchor),
                        "best_streak": chart::best_streak(&days) }),
                chart::render_heatmap(ctx, &days, anchor, members.is_empty()),
            )
        }
        ChartKind::Burndown {
            mut filter,
            project,
            days,
        } => {
            let days_n = days.unwrap_or(30);
            // `--project` is shorthand appended to the SAME positional
            // (#663/D173 review finding): through `filter::quote`, never
            // interpolated, for the reason `dashboard_screen`'s own
            // `Action::ListProject` composition gives — a project may be
            // named `Home Renovation` or `a (b)`. Appended rather than
            // replacing `filter`, so `chart burndown project:work --project
            // other` (an odd thing to type, but not refused) ANDs both terms
            // exactly as two positional terms would.
            if let Some(p) = project {
                filter.push(format!("project:{}", tasqx_core::filter::quote(&p)));
            }
            // Reported, never swallowed: an unresolvable scope used to render as
            // a cleared burndown, which is a wrong answer wearing the costume of
            // a right one.
            let (members, label) = burndown_members(engine, &filter)?;
            let events = events_since(engine, anchor, days_n + 1)?;
            let series = chart::burndown(&events, &members, days_n, anchor);
            let data = series
                .iter()
                .map(|p| json!({ "date": p.date.to_string(), "remaining": p.remaining }))
                .collect::<Vec<_>>();
            (
                json!({ "chart": "burndown", "days": days_n, "scope": label, "series": data }),
                chart::render_burndown(ctx, &series, &label, !members.is_empty()),
            )
        }
    })
}

/// `tasqx report --html`: write the self-contained HTML review.
///
/// The scope comes from [`report_params`] — the SAME builder the terminal path
/// uses — so the two output modes of one command cannot answer different
/// questions again. `all` is hard `false`, and `since`/`until` hard `None`,
/// rather than parameters, because clap already rejects `--all`/`--since`/
/// `--until` alongside `--html` (the HTML page has no windowed path yet);
/// spelling it here keeps the two facts in one place instead of accepting a
/// flag we would then ignore.
pub(crate) fn run_html_report(
    engine: &Engine,
    ctx: &Ctx,
    args: Vec<String>,
    out: Option<String>,
) -> CmdOutcome {
    let params = report_params(&args, false, None, None, now_ts())?;
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
