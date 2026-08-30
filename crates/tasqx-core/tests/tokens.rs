//! Per-task AI token accounting (docs/research/token-accounting.md, #11-#13):
//! the `token.add` mutation, its read surfaces (task.get, export), the D12
//! round trip, and the no-rev-bump rule that keeps async attribution from
//! breaking a client's `expected_rev`.

use serde_json::json;
use tasqx_core::{dispatch, Engine, ErrorCode};

/// A `done` event payload carrying a transcript path, serialised rather than
/// formatted.
///
/// These tests rewrite event payloads directly to pin field-observed instants
/// the engine would otherwise stamp with wall-clock time. The payload has to be
/// built with [`serde_json`] and not with `format!`: a transcript path is an OS
/// path, and on Windows it is `C:\Users\…`, whose backslashes are invalid JSON
/// escapes. A hand-written string is then well-formed on Linux and malformed on
/// Windows — where it does not error, it simply fails to parse into an
/// attributable completion, so the test reports a count of zero and looks like a
/// logic bug in the engine. Twelve tests failed that way and
/// `test (windows-latest)` stayed red for a week.
fn done_payload(completed: &str, transcript_path: &str) -> String {
    json!({
        "completed": completed,
        "client": "claude-code",
        "transcript_path": transcript_path,
    })
    .to_string()
}

fn engine() -> Engine {
    Engine::open_in_memory().expect("open in-memory store")
}

fn count(engine: &Engine, sql: &str) -> i64 {
    engine.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

// ---- token.add ----------------------------------------------------------------

#[test]
fn token_add_stores_a_measurement_and_task_get_reads_it_back() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    let r = e
        .token_add(&json!({
            "ref": sid,
            "tool": "claude-code",
            "source": "log-parse",
            "model": "claude-fable-5",
            "input_tokens": 1200,
            "output_tokens": 340,
            "cache_read_tokens": 9000,
            "cache_creation_tokens": 50,
            "confidence": "high",
        }))
        .unwrap();
    let m = &r["measurement"];
    assert_eq!(m["tool"], "claude-code");
    assert_eq!(m["source"], "log-parse");
    assert_eq!(m["model"], "claude-fable-5");
    assert_eq!(m["input_tokens"], 1200);
    assert_eq!(m["output_tokens"], 340);
    assert_eq!(m["cache_read_tokens"], 9000);
    assert_eq!(m["cache_creation_tokens"], 50);
    assert_eq!(m["confidence"], "high");
    assert!(m["id"].is_string() && m["created"].is_string());

    // The read surface agrees with the write's answer, byte for byte.
    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    assert_eq!(got["tokens"], json!([m.clone()]));

    // Exactly one event, carrying the measurement as its payload.
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events WHERE op='token.add'"),
        1
    );
}

/// The reminder_fire rule, applied here: a measurement is a fact about tokens
/// already spent, not an edit. The async writers of the later phases run
/// AFTER completion, and a rev bump from one of them would spuriously break a
/// client's `expected_rev` on a task the client never touched.
#[test]
fn token_add_does_not_bump_rev_or_modified() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let before = e.task_get(&json!({ "ref": sid })).unwrap();

    e.token_add(&json!({
        "ref": sid,
        "tool": "claude-code",
        "source": "self-report",
        "confidence": "medium",
        "input_tokens": 5,
    }))
    .unwrap();

    let after = e.task_get(&json!({ "ref": sid })).unwrap();
    assert_eq!(after["_rev"], before["_rev"], "no rev bump");
    assert_eq!(after["modified"], before["modified"], "no modified bump");
}

#[test]
fn token_add_counts_default_to_zero_and_model_is_optional() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let m = e
        .token_add(&json!({
            "ref": sid,
            "tool": "cursor",
            "source": "self-report",
            "confidence": "low",
        }))
        .unwrap()["measurement"]
        .clone();
    assert_eq!(m["input_tokens"], 0);
    assert_eq!(m["output_tokens"], 0);
    assert_eq!(m["cache_read_tokens"], 0);
    assert_eq!(m["cache_creation_tokens"], 0);
    assert!(m["model"].is_null());
}

#[test]
fn token_add_refuses_values_outside_the_closed_vocabularies() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    // Unknown source: the message names the value AND the accepted set.
    let err = e
        .token_add(&json!({
            "ref": sid, "tool": "x", "source": "guesswork", "confidence": "high"
        }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("guesswork"), "{}", err.message);
    for s in tasqx_core::tokens::TOKEN_SOURCES {
        assert!(err.message.contains(s), "{} must list {s}", err.message);
    }

    // Unknown confidence, same contract.
    let err = e
        .token_add(&json!({
            "ref": sid, "tool": "x", "source": "otel", "confidence": "certain"
        }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("certain"), "{}", err.message);
    for c in tasqx_core::tokens::TOKEN_CONFIDENCE {
        assert!(err.message.contains(c), "{} must list {c}", err.message);
    }

    // A refusal writes nothing: no row, no event.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 0);
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events WHERE op='token.add'"),
        0
    );
}

#[test]
fn token_add_requires_tool_and_refuses_a_negative_count() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    let err = e
        .token_add(&json!({ "ref": sid, "source": "otel", "confidence": "high" }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("tool"), "{}", err.message);

    let err = e
        .token_add(&json!({
            "ref": sid, "tool": "x", "source": "otel", "confidence": "high",
            "input_tokens": -5
        }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("input_tokens"), "{}", err.message);
}

/// `token.add` goes through the one dispatch table, so the params gate applies:
/// an unknown key is refused naming itself and the accepted set.
#[test]
fn token_add_is_dispatchable_and_gated() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    let r = dispatch(
        &e,
        "token.add",
        &json!({ "ref": sid, "tool": "goose", "source": "otel", "confidence": "high" }),
    )
    .unwrap();
    assert_eq!(r["measurement"]["tool"], "goose");

    let err = dispatch(
        &e,
        "token.add",
        &json!({ "ref": sid, "tool": "goose", "source": "otel", "confidence": "high",
                 "extra": "nope" }),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("extra"), "{}", err.message);
    assert!(err.message.contains("confidence"), "{}", err.message);
}

// ---- export / import (D12) ------------------------------------------------------

#[test]
fn export_import_round_trips_token_measurements() {
    let a = engine();
    let with = a.task_add(&json!({ "title": "with tokens" })).unwrap();
    let without = a.task_add(&json!({ "title": "without" })).unwrap();
    for (source, confidence, n) in [("log-parse", "high", 100), ("self-report", "medium", 7)] {
        a.token_add(&json!({
            "ref": with["short_id"].clone(),
            "tool": "claude-code",
            "source": source,
            "confidence": confidence,
            "input_tokens": n,
            "output_tokens": n * 2,
        }))
        .unwrap();
    }

    let doc = a.store_export(&json!({})).unwrap();
    let tasks = doc["tasks"].as_array().unwrap();
    let exported_with = tasks
        .iter()
        .find(|t| t["id"] == with["id"])
        .expect("exported");
    let exported_without = tasks
        .iter()
        .find(|t| t["id"] == without["id"])
        .expect("exported");
    assert_eq!(exported_with["tokens"].as_array().unwrap().len(), 2);
    // Absent, not `[]`: the export shape of a store that never recorded a
    // token stays byte-identical (the status_unrecognized precedent).
    assert!(
        exported_without.get("tokens").is_none(),
        "a task with no measurements must not grow a tokens key: {exported_without}"
    );

    // Restore into a fresh store; the re-export is the identity check.
    let b = engine();
    b.store_import(&doc).unwrap();
    let redoc = b.store_export(&json!({})).unwrap();
    assert_eq!(
        doc["tasks"], redoc["tasks"],
        "export -> import -> export must be identity, measurements included"
    );

    // And the measurements are live data, not just export baggage.
    let got = b
        .task_get(&json!({ "ref": with["short_id"].clone() }))
        .unwrap();
    assert_eq!(got["tokens"].as_array().unwrap().len(), 2);
}

#[test]
fn import_refuses_a_bad_measurement_naming_the_task_and_field() {
    let e = engine();
    let err = e
        .store_import(&json!({
            "tasks": [{
                "id": "t1", "short_id": 1, "title": "A",
                "tokens": [{ "tool": "x", "source": "vibes", "confidence": "high" }]
            }]
        }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("task t1"), "{}", err.message);
    assert!(err.message.contains("vibes"), "{}", err.message);

    // An unknown key inside a measurement object is refused too (D34).
    let err = e
        .store_import(&json!({
            "tasks": [{
                "id": "t1", "short_id": 1, "title": "A",
                "tokens": [{ "tool": "x", "source": "otel", "confidence": "high",
                             "cost_usd": 1.5 }]
            }]
        }))
        .unwrap_err();
    assert!(err.message.contains("cost_usd"), "{}", err.message);

    // Refused documents write nothing.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM tasks"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 0);
}

/// Import replaces child rows wholesale (the annotations rule): re-importing a
/// task without a `tokens` key clears what the store held for it.
#[test]
fn import_replaces_measurements_wholesale() {
    let e = engine();
    let t = e.task_add(&json!({ "title": "t" })).unwrap();
    e.token_add(&json!({
        "ref": t["short_id"].clone(), "tool": "x", "source": "otel", "confidence": "high",
        "input_tokens": 1
    }))
    .unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 1);

    e.store_import(&json!({
        "tasks": [{ "id": t["id"].clone(), "short_id": t["short_id"].clone(), "title": "t" }]
    }))
    .unwrap();
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM token_usage"),
        0,
        "the payload's task object is authoritative about its child rows"
    );
}

// ---- #12 correlation metadata ---------------------------------------------------

/// The one payload of the given event op, parsed. Events are the durable
/// correlation record, so the payload IS the read surface under test.
fn event_payload(e: &Engine, op: &str) -> serde_json::Value {
    let raw: String = e
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE op = ?1 ORDER BY id DESC LIMIT 1",
            [op],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn start_and_done_events_carry_the_correlation_params() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    e.task_start(&json!({
        "ref": sid,
        "session_id": "sess-1",
        "prompt_id": "prompt-9",
        "transcript_path": "/home/me/.claude/projects/x/sess-1.jsonl",
        "client": "claude-code 2.1.0",
    }))
    .unwrap();
    let start = event_payload(&e, "start");
    assert!(start["interval_started"].is_string());
    assert_eq!(start["session_id"], "sess-1");
    assert_eq!(start["prompt_id"], "prompt-9");
    assert_eq!(
        start["transcript_path"],
        "/home/me/.claude/projects/x/sess-1.jsonl"
    );
    assert_eq!(start["client"], "claude-code 2.1.0");

    e.task_done(&json!({ "ref": sid, "session_id": "sess-1", "client": "claude-code 2.1.0" }))
        .unwrap();
    let done = event_payload(&e, "done");
    assert!(done["completed"].is_string());
    assert_eq!(done["session_id"], "sess-1");
    assert_eq!(done["client"], "claude-code 2.1.0");
    // Keys not supplied stay ABSENT, not null — a human's `tasqx done 4`
    // must not grow four null fields on every event.
    assert!(done.get("prompt_id").is_none());
    assert!(done.get("transcript_path").is_none());
}

#[test]
fn a_plain_start_and_done_write_the_same_payloads_as_before() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_start(&json!({ "ref": sid })).unwrap();
    let start = event_payload(&e, "start");
    assert_eq!(
        start.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["interval_started"],
        "no correlation given, no new keys: {start}"
    );
    e.task_done(&json!({ "ref": sid })).unwrap();
    let done = event_payload(&e, "done");
    assert_eq!(
        done.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["completed"],
        "no correlation given, no new keys: {done}"
    );
}

/// D35: an empty correlation string is a present value with no meaning here,
/// so it is refused naming the param rather than silently absent.
#[test]
fn empty_correlation_strings_are_refused() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let err = e
        .task_done(&json!({ "ref": sid, "session_id": "" }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("session_id"), "{}", err.message);
}

// ---- #13 self-report on task.done ------------------------------------------------

#[test]
fn task_done_with_token_counts_records_a_self_report_measurement() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    let r = e
        .task_done(&json!({
            "ref": sid,
            "session_id": "sess-2",
            "tool": "cursor",
            "model": "gpt-5.4",
            "input_tokens": 800,
            "output_tokens": 120,
        }))
        .unwrap();
    assert_eq!(r["status"], "done");

    // One measurement row, fixed source and confidence, absent counts 0.
    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    let m = &got["tokens"][0];
    assert_eq!(m["tool"], "cursor");
    assert_eq!(m["source"], "self-report");
    assert_eq!(m["confidence"], "medium");
    assert_eq!(m["model"], "gpt-5.4");
    assert_eq!(m["input_tokens"], 800);
    assert_eq!(m["output_tokens"], 120);
    assert_eq!(m["cache_read_tokens"], 0);
    assert_eq!(m["cache_creation_tokens"], 0);

    // ONE event for the whole mutation — the measurement rides in the done
    // payload, there is no separate token.add event.
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM events WHERE op='token.add'"),
        0
    );
    let done = event_payload(&e, "done");
    assert_eq!(done["session_id"], "sess-2");
    assert_eq!(done["tokens"], *m, "the done event echoes the measurement");
}

/// Over MCP the agent usually passes no `tool` — the injected `client` is the
/// attribution fallback.
#[test]
fn task_done_self_report_tool_defaults_to_the_client() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": sid, "client": "claude-code 2.1.0", "output_tokens": 42 }))
        .unwrap();
    let got = e.task_get(&json!({ "ref": sid })).unwrap();
    assert_eq!(got["tokens"][0]["tool"], "claude-code 2.1.0");
}

#[test]
fn task_done_token_counts_without_any_attribution_are_refused() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let err = e
        .task_done(&json!({ "ref": sid, "input_tokens": 10 }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("tool"), "{}", err.message);

    // The refusal happened before the lock: the task is still open and no
    // partial write survived.
    assert_eq!(
        e.task_get(&json!({ "ref": sid })).unwrap()["status"],
        "pending"
    );
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 0);
}

/// `tool` and `model` are recorded without a token count, not refused.
///
/// They used to be refused, on the D33 rule that a value changing nothing must
/// not answer `ok` — and while they were dropped on the floor, that was right.
/// The rule was applied to the wrong half: the fix is to make the value change
/// something. **An agent does not know its own token spend** — no harness hands
/// the model a running count — so the refusal read "supply a number you cannot
/// observe, or forfeit recording the tool and model you can". Callers took the
/// second option, and the store filled with completions attributed to nobody.
///
/// No measurement is written: a zero-count `token_usage` row would be a phantom
/// measurement that every later sum counts as real, which is worse than none.
/// The facts land on the completion event beside the correlation keys, where
/// the attribution pipeline already looks.
#[test]
fn task_done_records_tool_and_model_without_any_count() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let out = e
        .task_done(&json!({ "ref": sid, "tool": "cursor", "model": "claude-opus-5" }))
        .expect("a completion that names its tool must not be refused");

    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM token_usage"),
        0,
        "no counts were given, so a measurement here would be invented"
    );
    let payload = event_payload(&e, "done");
    assert_eq!(payload["tool"], json!("cursor"));
    assert_eq!(payload["model"], json!("claude-opus-5"));
    assert!(
        payload.get("tokens").is_none(),
        "the `tokens` key means a measurement was made; nothing was measured"
    );
    let hint = out["tokens_hint"].as_str().expect("a hint");
    assert!(
        hint.contains("recorded"),
        "the response must say what WAS recorded, not only what was missing: {hint}"
    );
}

/// One rule, not two: what the caller named is on the event whether or not a
/// measurement was also written. The measurement is the fact; the event is the
/// audit of the call, and a caller who names a tool has said something about
/// the call either way.
#[test]
fn the_event_names_the_tool_and_model_alongside_a_measurement_too() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_done(&json!({
        "ref": sid, "tool": "cursor", "model": "claude-opus-5", "input_tokens": 12
    }))
    .expect("done");

    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 1);
    let payload = event_payload(&e, "done");
    assert_eq!(payload["tool"], json!("cursor"));
    assert_eq!(payload["model"], json!("claude-opus-5"));
    assert!(payload.get("tokens").is_some());
}

#[test]
fn task_done_without_token_params_writes_no_measurement() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": sid })).unwrap();
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 0);
    assert!(event_payload(&e, "done").get("tokens").is_none());
}

// ---- D50 refusal, end to end (#79 / ATTACK 3) -----------------------------------

/// The #79 mechanism, driven through the real store and the real attribution
/// pipeline (`pending_attributions` → `compute_attribution` → `attribute_one`),
/// in the exact shape of `attack78.sh` ATTACK 3: task X is completed WITHOUT
/// ever being started (its window falls back to `tasks.created`), task Y is
/// started and completed entirely inside X's span, both name the same
/// transcript, and that transcript holds ONE 1000/2000 usage line inside Y's
/// window. Before D50 both windows summed the same line at full confidence —
/// ~1.5M tokens were double-counted in the live store this way. Under the
/// refusal rule the sample is contested and banked for NO ONE while both tasks
/// are unresolved: the store must never end with the same spend on two tasks.
#[test]
fn one_spend_is_never_billed_to_two_tasks() {
    use tasqx_core::attribution::{attribute_one, compute_attribution, pending_attributions};

    let dir = std::env::temp_dir().join(format!(
        "tasqx-attack3-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("sess-1.jsonl");
    // The one spend: a single usage line inside Y's window (and therefore
    // inside X's superset window too).
    std::fs::write(
        &transcript,
        r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();

    let e = engine();
    let x = e.task_add(&json!({ "title": "X" })).unwrap()["short_id"].clone();
    let y = e.task_add(&json!({ "title": "Y" })).unwrap()["short_id"].clone();
    // Y is started; X never is — X's window_start falls back to `created`.
    e.task_start(&json!({ "ref": y })).unwrap();
    e.task_done(&json!({ "ref": y, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    e.task_done(&json!({ "ref": x, "client": "claude-code", "transcript_path": path }))
        .unwrap();

    // Pin the field-observed instants (engine timestamps are wall-clock):
    // X spans [09:40:00, 10:01:44], Y spans [09:46:53, 09:49:37] ⊂ X.
    let task_id = |sid: &serde_json::Value| -> String {
        e.conn()
            .query_row(
                "SELECT id FROM tasks WHERE short_id = ?1",
                [sid.as_i64().unwrap()],
                |r| r.get(0),
            )
            .unwrap()
    };
    let (x_id, y_id) = (task_id(&x), task_id(&y));
    e.conn()
        .execute(
            "UPDATE tasks SET created = '2026-07-25T09:40:00Z' WHERE id = ?1",
            [&x_id],
        )
        .unwrap();
    e.conn()
        .execute(
            "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
            (r#"{"interval_started":"2026-07-25T09:46:53Z"}"#, &y_id),
        )
        .unwrap();
    for (id, completed) in [
        (&x_id, "2026-07-25T10:01:44Z"),
        (&y_id, "2026-07-25T09:49:37Z"),
    ] {
        e.conn()
            .execute(
                "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
                (
                    // Built by the serializer, not by `format!`. A hand-written
                    // JSON string with an OS path interpolated into it is
                    // well-formed on Linux and NOT on Windows, where the path is
                    // `C:\Users\…` and `\U`/`\A`/`\T` are invalid escapes: the
                    // payload then fails to parse, the completion never becomes
                    // a pending attribution, and the assertion reports "0"
                    // against a test that looks entirely correct. That is what
                    // held `test (windows-latest)` red across twelve tests.
                    done_payload(completed, &path),
                    id,
                ),
            )
            .unwrap();
    }

    // One attribution pass, minutes after the completions (well inside the
    // give-up deadline), exactly as the daemon's tick drives it.
    let pending = pending_attributions(&e).unwrap();
    assert_eq!(pending.len(), 2, "both completions are pending");
    let now: jiff::Timestamp = "2026-07-25T10:05:00Z".parse().unwrap();
    for pa in &pending {
        match compute_attribution(pa, now) {
            Ok(r) => {
                attribute_one(&e, pa, &r).unwrap();
            }
            // The refusal: while both windows claim the sample, the task stays
            // transient with the distinct contested message.
            Err(err) => assert!(
                err.message.contains("contested"),
                "expected the contested transient, got: {}",
                err.message
            ),
        }
    }

    // The invariant #79 violated: the same spend must never sit on two tasks.
    let log_parse_rows = count(
        &e,
        "SELECT COUNT(*) FROM token_usage WHERE source='log-parse'",
    );
    assert!(
        log_parse_rows <= 1,
        "one usage line produced {log_parse_rows} log-parse measurements"
    );
    let distinct_tasks = count(
        &e,
        "SELECT COUNT(DISTINCT task_id) FROM token_usage WHERE source='log-parse'",
    );
    assert_eq!(
        distinct_tasks, log_parse_rows,
        "a banked spend belongs to exactly one task"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A wrong-typed `sample_ids` is a caller error, not an absent value.
///
/// It read through `p.get("sample_ids").and_then(Value::as_array)`, which
/// answers `None` for both, and the array branch then dropped non-string
/// entries with `filter_map(as_str)`. So `"msg-1"` instead of `["msg-1"]`, or
/// one stray number in the array, banked the measurement with fewer ids than
/// the caller sent — at `ok: true`. The ids exist precisely so a later tick can
/// refuse a re-emitted sample by identity; ids lost on the way in take that
/// refusal with them, and no reader can tell "the parser had none" from "the
/// caller sent them wrong". D32: present + wrong type is `bad_request`, absent
/// keeps the default.
#[test]
fn token_attribute_refuses_a_wrong_typed_sample_ids() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();

    let attribute = |ids: serde_json::Value| {
        e.token_attribute(&json!({
            "ref": sid, "source": "log-parse", "tool": "claude-code",
            "confidence": "medium", "input_tokens": 10, "sample_ids": ids,
        }))
    };

    for (shape, ids) in [
        ("a bare string where an array belongs", json!("msg-1")),
        ("a number among the ids", json!(["msg-1", 7])),
    ] {
        let err = attribute(ids).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest, "{shape}");
        assert!(
            err.message.contains("sample_ids"),
            "{shape}: the refusal must name the param the caller got wrong, \
             not leave them guessing: {}",
            err.message
        );
    }

    // A refusal writes nothing — the measurement must not land with the ids
    // quietly missing, which is the outcome this replaces.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 0);

    // Absent is still absent: the parsers that have no ids must keep working.
    assert!(e
        .token_attribute(&json!({
            "ref": sid, "source": "log-parse", "tool": "codex",
            "confidence": "medium", "input_tokens": 10,
        }))
        .is_ok());
}

/// The `sample_ids` array lands verbatim in the event log and is re-parsed on
/// every pending build, so an unbounded one is a permanent per-tick tax. No
/// real transcript window approaches thousands of samples (the store's biggest
/// banked window held 41), so more than 4096 ids is a caller error, refused
/// before anything is written.
#[test]
fn token_attribute_refuses_an_oversized_sample_ids_array() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let ids: Vec<String> = (0..4097).map(|i| format!("msg-{i}")).collect();

    let err = e
        .token_attribute(&json!({
            "ref": sid, "source": "log-parse", "tool": "claude-code",
            "confidence": "medium", "input_tokens": 10, "sample_ids": ids,
        }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("sample_ids"), "{}", err.message);
    assert!(err.message.contains("4096"), "{}", err.message);

    // A refusal writes nothing: no measurement, no marker.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 0);
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM events WHERE op='tokens.attributed'"
        ),
        0
    );

    // Exactly the cap is accepted — the bound refuses, it does not truncate.
    let ids: Vec<String> = (0..4096).map(|i| format!("msg-{i}")).collect();
    assert!(e
        .token_attribute(&json!({
            "ref": sid, "source": "log-parse", "tool": "claude-code",
            "confidence": "medium", "input_tokens": 10, "sample_ids": ids,
        }))
        .unwrap());
}

// ---- OTLP buffer (#18) --------------------------------------------------------

/// Buffered OTLP samples (#18) are raw telemetry, not attributed to any task:
/// they must write NO task event and NO measurement row, only `otlp_samples`.
#[test]
fn otlp_ingest_buffers_samples_without_task_events_or_measurements() {
    use tasqx_core::otlp::OtlpSample;
    use tasqx_core::tokens::UsageSample;

    let e = engine();
    let n = e
        .otlp_ingest(&[
            OtlpSample {
                tool: "claude-code".into(),
                session_id: Some("sess-1".into()),
                sample: UsageSample {
                    id: None,
                    ts: "2026-07-24T10:00:00Z".into(),
                    model: Some("claude-opus-4-8".into()),
                    input_tokens: 10,
                    output_tokens: 20,
                    cache_read_tokens: 3,
                    cache_creation_tokens: 1,
                },
            },
            OtlpSample {
                tool: "codex".into(),
                session_id: Some("sess-2".into()),
                sample: UsageSample {
                    id: None,
                    ts: "2026-07-24T10:05:00Z".into(),
                    model: None,
                    input_tokens: 5,
                    output_tokens: 6,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                },
            },
        ])
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM otlp_samples"), 2);
    // Raw telemetry is not a task mutation: no events, no measurements.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), 0);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), 0);
    // The row was stored with its four separate counts and its tool.
    let cache_read: i64 = e
        .conn()
        .query_row(
            "SELECT cache_read_tokens FROM otlp_samples WHERE session_id = 'sess-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cache_read, 3);
    let tool: String = e
        .conn()
        .query_row(
            "SELECT tool FROM otlp_samples WHERE session_id = 'sess-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tool, "claude-code");
}

/// Retention prune runs inside every ingest: a row older than the window is
/// dropped when a fresh sample arrives, so the buffer cannot grow unbounded.
#[test]
fn otlp_ingest_prunes_samples_past_the_retention_window() {
    use tasqx_core::otlp::OtlpSample;
    use tasqx_core::tokens::UsageSample;

    let e = engine();
    // 29 and 31 days, spelled out here rather than derived from
    // `OTLP_RETENTION_SECS`. Deriving them (`now - (RETENTION - 1h)`) would move
    // the seeds and the cutoff together, so shrinking the constant from 30 days
    // to 1 would leave this green — which is exactly the state this test was in
    // when it seeded a row dated 2000-01-01: any window from seconds to
    // millennia pruned that row, so it proved a DELETE ran and nothing about
    // WHEN. The pair pins the window's MAGNITUDE. It deliberately does not pin
    // `<` vs `<=`: the cutoff comes from `Timestamp::now()` inside the ingest,
    // so a row landing exactly on it is a race no deterministic test can seed.
    let day = jiff::SignedDuration::from_hours(24);
    let keep = (jiff::Timestamp::now() - day * 29).to_string();
    let prune = (jiff::Timestamp::now() - day * 31).to_string();
    for (id, created) in [("young", &keep), ("old", &prune)] {
        e.conn()
            .execute(
                "INSERT INTO otlp_samples (id, session_id, tool, ts, created) \
                 VALUES (?1, 'sess-old', 'codex', ?2, ?2)",
                rusqlite::params![id, created],
            )
            .unwrap();
    }
    assert_eq!(count(&e, "SELECT COUNT(*) FROM otlp_samples"), 2);

    // A fresh ingest triggers the opportunistic prune of the stale row.
    e.otlp_ingest(&[OtlpSample {
        tool: "codex".into(),
        session_id: Some("sess-new".into()),
        sample: UsageSample {
            id: None,
            ts: "2026-07-24T10:00:00Z".into(),
            model: None,
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        },
    }])
    .unwrap();
    // Both halves, and both are load-bearing. Asserting only the prune would
    // stay green if the window shrank to nothing and took every row with it;
    // asserting only the survivor would stay green if the prune never ran.
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM otlp_samples WHERE id = 'old'"),
        0,
        "a row 31 days old is past the retention window and must be pruned"
    );
    assert_eq!(
        count(&e, "SELECT COUNT(*) FROM otlp_samples WHERE id = 'young'"),
        1,
        "a row 29 days old is inside the window and must survive"
    );
    // The ingest's own row, plus the survivor.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM otlp_samples"), 2);
}

// ---- tokens.recompute (D50 Decision 3: one-shot history repair) -----------------

use std::path::PathBuf;

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tasqx-recompute-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn task_uuid(e: &Engine, sid: &serde_json::Value) -> String {
    e.conn()
        .query_row(
            "SELECT id FROM tasks WHERE short_id = ?1",
            [sid.as_i64().unwrap()],
            |r| r.get(0),
        )
        .unwrap()
}

/// Pin every `done` payload of one task to a fixed completion instant and
/// transcript path — the seeded shape of pre-D50 history (engine timestamps
/// are wall-clock, so field-observed windows must be written in afterwards).
fn pin_done(e: &Engine, task_uuid: &str, completed: &str, path: &str) {
    e.conn()
        .execute(
            "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'done'",
            (done_payload(completed, path), task_uuid),
        )
        .unwrap();
}

fn pin_created(e: &Engine, task_uuid: &str, created: &str) {
    e.conn()
        .execute(
            "UPDATE tasks SET created = ?1 WHERE id = ?2",
            (created, task_uuid),
        )
        .unwrap();
}

/// The four-bucket object the recompute report speaks.
fn b4(input: i64, output: i64) -> serde_json::Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_tokens": 0,
        "cache_creation_tokens": 0,
    })
}

/// The live store's `019f98a4` shape: Y's window is a strict subset of X's
/// over one transcript, and pre-D50 ticks banked the same 1000/2000 line on
/// BOTH tasks (X also caught line "b", which only its window covers). The
/// seeded markers carry NO sample_ids — the pre-upgrade marker shape whose
/// claims the recompute must rebuild and backfill.
fn seeded_double_count() -> (Engine, PathBuf, serde_json::Value, serde_json::Value) {
    let dir = scratch_dir("pair");
    let transcript = dir.join("sess-1.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
            "\n",
            r#"{"timestamp":"2026-07-25T09:55:00.000Z","message":{"id":"b","usage":{"input_tokens":500,"output_tokens":600}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();

    let e = engine();
    let x = e.task_add(&json!({ "title": "X" })).unwrap()["short_id"].clone();
    let y = e.task_add(&json!({ "title": "Y" })).unwrap()["short_id"].clone();
    // Y is started; X never is — X's window_start falls back to `created`.
    e.task_start(&json!({ "ref": y })).unwrap();
    e.task_done(&json!({ "ref": y, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    e.task_done(&json!({ "ref": x, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    let (x_id, y_id) = (task_uuid(&e, &x), task_uuid(&e, &y));
    pin_created(&e, &x_id, "2026-07-25T09:40:00Z");
    e.conn()
        .execute(
            "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
            (r#"{"interval_started":"2026-07-25T09:46:53Z"}"#, &y_id),
        )
        .unwrap();
    pin_done(&e, &x_id, "2026-07-25T10:01:44Z", &path);
    pin_done(&e, &y_id, "2026-07-25T09:49:37Z", &path);

    // The pre-D50 double-count, X banked first: both windows summed line "a"
    // at full value, neither marker carries sample_ids.
    e.token_attribute(&json!({
        "ref": x, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 2, "input_tokens": 1500, "output_tokens": 2600,
    }))
    .unwrap();
    e.token_attribute(&json!({
        "ref": y, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();
    (e, dir, x, y)
}

#[test]
fn recompute_dry_run_reports_the_delta_and_writes_nothing() {
    let (e, dir, x, y) = seeded_double_count();
    let rows_before = count(&e, "SELECT COUNT(*) FROM token_usage");
    let events_before = count(&e, "SELECT COUNT(*) FROM events");

    // No params at all: dry-run is the DEFAULT — the safe direction for the
    // one verb in the API built to delete measurement rows.
    let r = dispatch(&e, "tokens.recompute", &json!({})).unwrap();
    assert_eq!(r["dry_run"], true, "{r}");

    let tasks = r["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "{r}");
    // Deterministic order: ascending original marker rowid — X banked first.
    assert_eq!(tasks[0]["task"], x);
    assert_eq!(tasks[1]["task"], y);
    assert_eq!(tasks[0]["action"], "recomputed");
    assert_eq!(tasks[0]["before"], b4(1500, 2600));
    assert_eq!(
        tasks[0]["after"],
        b4(500, 600),
        "X keeps only the uncontested line"
    );
    assert_eq!(tasks[1]["action"], "recomputed");
    assert_eq!(tasks[1]["before"], b4(1000, 2000));
    assert_eq!(
        tasks[1]["after"],
        b4(0, 0),
        "the subset window loses everything"
    );
    assert_eq!(r["totals"], json!({ "before": 7100, "after": 1100 }));

    // Dry-run writes NOTHING: same rows, same events.
    assert_eq!(count(&e, "SELECT COUNT(*) FROM token_usage"), rows_before);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), events_before);

    // And because nothing was written, a second dry-run reports the SAME delta.
    let again = dispatch(&e, "tokens.recompute", &json!({})).unwrap();
    assert_eq!(again, r);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recompute_removes_the_double_counted_subset_window() {
    let (e, dir, x, y) = seeded_double_count();

    let dry = dispatch(&e, "tokens.recompute", &json!({})).unwrap();
    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    assert_eq!(r["dry_run"], false);
    // Apply performs exactly the plan the dry-run printed.
    assert_eq!(r["tasks"], dry["tasks"]);
    assert_eq!(r["totals"], dry["totals"]);

    // The subset window (Y) lost its rows; the superset (X) keeps only the
    // uncontested remainder.
    let (x_id, y_id) = (task_uuid(&e, &x), task_uuid(&e, &y));
    let x_rows = count(
        &e,
        &format!("SELECT COUNT(*) FROM token_usage WHERE task_id='{x_id}' AND source='log-parse'"),
    );
    assert_eq!(x_rows, 1);
    let (input, output, confidence): (i64, i64, String) = e
        .conn()
        .query_row(
            "SELECT input_tokens, output_tokens, confidence FROM token_usage \
             WHERE task_id = ?1 AND source = 'log-parse'",
            [&x_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((input, output), (500, 600));
    assert_eq!(confidence, "medium");
    assert_eq!(
        count(
            &e,
            &format!(
                "SELECT COUNT(*) FROM token_usage WHERE task_id='{y_id}' AND source='log-parse'"
            )
        ),
        0
    );

    // The NEW marker backfills the sample ids the surviving measurement
    // consumed; the pre-upgrade marker stays in the append-only log.
    let markers_of_x = count(
        &e,
        &format!("SELECT COUNT(*) FROM events WHERE entity_id='{x_id}' AND op='tokens.attributed'"),
    );
    assert_eq!(
        markers_of_x, 2,
        "old marker kept as provenance, new one added"
    );
    let payload: String = e
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE entity_id = ?1 AND op = 'tokens.attributed' \
             ORDER BY rowid DESC LIMIT 1",
            [&x_id],
            |row| row.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["sample_ids"], json!(["b"]), "{v}");

    // A third pass over the repaired store changes nothing: X is unchanged,
    // Y — no log-parse rows left — is out of scope entirely.
    let after = dispatch(&e, "tokens.recompute", &json!({})).unwrap();
    let tasks = after["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{after}");
    assert_eq!(tasks[0]["task"], x);
    assert_eq!(tasks[0]["action"], "unchanged");
    assert_eq!(after["totals"], json!({ "before": 1100, "after": 1100 }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recompute_downgrades_when_the_transcript_is_gone() {
    let dir = scratch_dir("gone");
    let transcript = dir.join("sess-9.jsonl");
    std::fs::write(
        &transcript,
        r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"g","usage":{"input_tokens":800,"output_tokens":900}}}"#,
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();

    let e = engine();
    let t = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": t, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    e.token_attribute(&json!({
        "ref": t, "source": "log-parse", "tool": "claude-code", "confidence": "high",
        "samples": 1, "input_tokens": 800, "output_tokens": 900,
    }))
    .unwrap();
    // The transcript is deleted before the recompute runs: the stored counts
    // cannot be re-derived, so they are kept — confidence-stripped, never
    // deleted blind.
    std::fs::remove_file(&transcript).unwrap();

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let entry = &r["tasks"][0];
    assert_eq!(entry["action"], "downgraded", "{r}");
    assert_eq!(entry["before"], b4(800, 900));
    assert_eq!(entry["after"], b4(800, 900), "counts are kept, not deleted");
    assert_eq!(r["totals"], json!({ "before": 1700, "after": 1700 }));

    let (input, confidence): (i64, String) = e
        .conn()
        .query_row(
            "SELECT input_tokens, confidence FROM token_usage WHERE source = 'log-parse'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(input, 800, "counts untouched");
    assert_eq!(confidence, "low", "the high label is stripped");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recompute_never_touches_self_report_or_otel_rows() {
    let dir = scratch_dir("sources");
    let transcript = dir.join("sess-2.jsonl");
    std::fs::write(
        &transcript,
        r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"c","usage":{"input_tokens":100,"output_tokens":200}}}"#,
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();

    let e = engine();
    let a = e.task_add(&json!({ "title": "A" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": a, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    let a_id = task_uuid(&e, &a);
    pin_created(&e, &a_id, "2026-07-25T09:40:00Z");
    pin_done(&e, &a_id, "2026-07-25T10:00:00Z", &path);
    // A wrong pre-D50 bank, plus an otel measurement on the same task.
    e.token_attribute(&json!({
        "ref": a, "source": "log-parse", "tool": "claude-code", "confidence": "high",
        "samples": 1, "input_tokens": 999, "output_tokens": 999,
    }))
    .unwrap();
    e.token_add(&json!({
        "ref": a, "tool": "claude-code", "source": "otel", "confidence": "high",
        "input_tokens": 42, "output_tokens": 7,
    }))
    .unwrap();
    // A second task measured purely by self-report: out of scope entirely.
    let b = e.task_add(&json!({ "title": "B" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": b, "tool": "cursor", "input_tokens": 5, "output_tokens": 5 }))
        .unwrap();

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let tasks = r["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "only the log-parse task is in scope: {r}");
    assert_eq!(tasks[0]["task"], a);
    // The bank no longer matches the transcript and nothing is contested:
    // only contest removes tokens (D50 as amended), so the wrong counts are
    // kept and distrusted rather than rewritten.
    assert_eq!(tasks[0]["action"], "downgraded");
    assert_eq!(tasks[0]["after"], b4(999, 999));

    // The log-parse row was kept, confidence-stripped...
    let (input, output, confidence): (i64, i64, String) = e
        .conn()
        .query_row(
            "SELECT input_tokens, output_tokens, confidence FROM token_usage \
             WHERE source = 'log-parse'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((input, output), (999, 999));
    assert_eq!(confidence, "low");
    // ...while the otel and self-report rows survive untouched.
    let otel_input: i64 = e
        .conn()
        .query_row(
            "SELECT input_tokens FROM token_usage WHERE source = 'otel'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(otel_input, 42);
    let self_report_input: i64 = e
        .conn()
        .query_row(
            "SELECT input_tokens FROM token_usage WHERE source = 'self-report'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(self_report_input, 5);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Decision 1: one task never mixes channels. Pre-TOCTOU-fix history can hold
/// BOTH a log-parse and a self-report row for one task; the recompute resolves
/// the conflict toward the caller's own report. The result shape choice: this
/// is the distinct `channel_conflict` action with `after: null` — the task's
/// log-parse rows are removed outright, and its real spend is the self-report,
/// which is not this verb's to restate.
#[test]
fn recompute_resolves_a_channel_conflict_toward_the_self_report() {
    let e = engine();
    let t = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": t, "client": "claude-code",
                 "transcript_path": "/no/such/transcript.jsonl" }))
        .unwrap();
    e.token_attribute(&json!({
        "ref": t, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();
    // The same task later self-reports through the other door (token.add).
    e.token_add(&json!({
        "ref": t, "tool": "claude-code", "source": "self-report", "confidence": "medium",
        "input_tokens": 111, "output_tokens": 222,
    }))
    .unwrap();

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let entry = &r["tasks"][0];
    assert_eq!(entry["action"], "channel_conflict", "{r}");
    assert_eq!(entry["before"], b4(1000, 2000));
    assert!(entry["after"].is_null(), "{r}");
    assert_eq!(r["totals"], json!({ "before": 3000, "after": 0 }));

    // The log-parse rows are gone outright — not recomputed, not downgraded —
    // and the self-report row is untouched.
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM token_usage WHERE source='log-parse'"
        ),
        0
    );
    let self_report_input: i64 = e
        .conn()
        .query_row(
            "SELECT input_tokens FROM token_usage WHERE source = 'self-report'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(self_report_input, 111);
}

/// Task #81's shape: a reopen + re-complete banked the same window twice, so
/// one task holds two log-parse rows for one spend. The per-task
/// delete-all-then-reinsert collapses them to the one recomputed survivor.
#[test]
fn recompute_collapses_reopen_duplicates_to_one_row() {
    let dir = scratch_dir("dup");
    let transcript = dir.join("sess-3.jsonl");
    std::fs::write(
        &transcript,
        r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();

    let e = engine();
    let t = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": t, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    let t_id = task_uuid(&e, &t);
    pin_created(&e, &t_id, "2026-07-25T09:40:00Z");
    pin_done(&e, &t_id, "2026-07-25T10:00:00Z", &path);
    e.token_attribute(&json!({
        "ref": t, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();
    // Reopen + re-complete: a fresh done past the old marker re-enters the
    // attribution queue, and the second bank duplicates the first.
    e.task_reopen(&json!({ "ref": t })).unwrap();
    e.task_done(&json!({ "ref": t, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    pin_done(&e, &t_id, "2026-07-25T10:00:00Z", &path);
    e.token_attribute(&json!({
        "ref": t, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM token_usage WHERE source='log-parse'"
        ),
        2,
        "the seeded duplicate must exist for this test to prove anything"
    );

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let entry = &r["tasks"][0];
    assert_eq!(entry["action"], "recomputed", "{r}");
    assert_eq!(
        entry["before"],
        b4(2000, 4000),
        "both duplicate rows summed"
    );
    assert_eq!(entry["after"], b4(1000, 2000), "one survivor");
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM token_usage WHERE source='log-parse'"
        ),
        1
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An accurate, uncontested, already-claimed measurement is left alone: no
/// row churn, no new marker, action `unchanged`.
#[test]
fn recompute_leaves_an_accurate_uncontested_measurement_alone() {
    let dir = scratch_dir("same");
    let transcript = dir.join("sess-4.jsonl");
    std::fs::write(
        &transcript,
        r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();

    let e = engine();
    let t = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": t, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    let t_id = task_uuid(&e, &t);
    pin_created(&e, &t_id, "2026-07-25T09:40:00Z");
    pin_done(&e, &t_id, "2026-07-25T10:00:00Z", &path);
    e.token_attribute(&json!({
        "ref": t, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000, "sample_ids": ["a"],
    }))
    .unwrap();
    let row_id_before: String = e
        .conn()
        .query_row("SELECT id FROM token_usage", [], |row| row.get(0))
        .unwrap();
    let events_before = count(&e, "SELECT COUNT(*) FROM events");

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    assert_eq!(r["tasks"][0]["action"], "unchanged", "{r}");
    assert_eq!(r["totals"], json!({ "before": 3000, "after": 3000 }));

    // No writes at all: the row was not even rewritten in place.
    let row_id_after: String = e
        .conn()
        .query_row("SELECT id FROM token_usage", [], |row| row.get(0))
        .unwrap();
    assert_eq!(row_id_after, row_id_before);
    assert_eq!(count(&e, "SELECT COUNT(*) FROM events"), events_before);

    let _ = std::fs::remove_dir_all(&dir);
}

/// The claim set is REBUILT in marker order as the recompute walks history:
/// a task whose transcript no longer resolves keeps its banked claim
/// (downgraded, not dissolved), and that claim still contests the same sample
/// id in a LATER task's recompute even across a different transcript file and
/// no window overlap — identity is global (D50 Decision 2, as amended).
#[test]
fn recompute_rebuilt_claims_contest_by_identity_across_sources() {
    let dir = scratch_dir("claims");
    let copy = dir.join("copy.jsonl");
    std::fs::write(
        &copy,
        r#"{"timestamp":"2026-07-25T09:47:00.000Z","message":{"id":"a","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
    )
    .unwrap();
    let copy_path = copy.to_string_lossy().into_owned();
    let gone_path = dir.join("gone.jsonl").to_string_lossy().into_owned();

    let e = engine();
    let x = e.task_add(&json!({ "title": "X" })).unwrap()["short_id"].clone();
    let y = e.task_add(&json!({ "title": "Y" })).unwrap()["short_id"].clone();
    e.task_done(&json!({ "ref": x, "client": "claude-code", "transcript_path": gone_path }))
        .unwrap();
    e.task_done(&json!({ "ref": y, "client": "claude-code", "transcript_path": copy_path }))
        .unwrap();
    let (x_id, y_id) = (task_uuid(&e, &x), task_uuid(&e, &y));
    // Disjoint windows, different files, no session ids: nothing but the
    // sample's identity connects the two banks.
    pin_created(&e, &x_id, "2026-07-25T09:00:00Z");
    pin_done(&e, &x_id, "2026-07-25T09:10:00Z", &gone_path);
    pin_created(&e, &y_id, "2026-07-25T09:40:00Z");
    pin_done(&e, &y_id, "2026-07-25T10:00:00Z", &copy_path);
    // X banked "a" first, WITH its id persisted; Y's bank of the same id is
    // the theft the recompute must refuse.
    e.token_attribute(&json!({
        "ref": x, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000, "sample_ids": ["a"],
    }))
    .unwrap();
    e.token_attribute(&json!({
        "ref": y, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let tasks = r["tasks"].as_array().unwrap();
    assert_eq!(tasks[0]["task"], x);
    assert_eq!(
        tasks[0]["action"], "downgraded",
        "X's transcript is gone; its counts and its claim survive: {r}"
    );
    assert_eq!(tasks[1]["task"], y);
    assert_eq!(tasks[1]["action"], "recomputed", "{r}");
    assert_eq!(
        tasks[1]["after"],
        b4(0, 0),
        "sample `a` is already claimed by X — banked for no one else: {r}"
    );
    assert_eq!(r["totals"], json!({ "before": 6000, "after": 3000 }));

    let _ = std::fs::remove_dir_all(&dir);
}

/// `dry_run` guards the one destructive verb in the API, so a value that is
/// not a boolean is refused — never coerced, never silently defaulted.
#[test]
fn recompute_refuses_a_non_boolean_dry_run() {
    let e = engine();
    let err = dispatch(&e, "tokens.recompute", &json!({ "dry_run": "false" })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("dry_run"), "{}", err.message);
    // The params gate applies like everywhere else.
    let err = dispatch(&e, "tokens.recompute", &json!({ "apply": true })).unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("apply"), "{}", err.message);
}

/// Bank one task's spend through the REAL tick path (pending set → compute →
/// attribute), so the marker carries whatever the live tick would persist —
/// including `sample_ids`. The window is pinned to [10:00, 10:30] on
/// 2026-07-25 and the tick runs at 10:35.
fn bank_live(e: &Engine, sid: &serde_json::Value, transcript: &std::path::Path) {
    use tasqx_core::attribution::{attribute_one, compute_attribution, pending_attributions};

    e.task_start(&json!({ "ref": sid })).unwrap();
    let path = transcript.to_string_lossy().into_owned();
    e.task_done(&json!({ "ref": sid, "client": "claude-code", "transcript_path": path }))
        .unwrap();
    let id = task_uuid(e, sid);
    e.conn()
        .execute(
            "UPDATE events SET payload = ?1 WHERE entity_id = ?2 AND op = 'start'",
            (r#"{"interval_started":"2026-07-25T10:00:00Z"}"#, &id),
        )
        .unwrap();
    pin_done(e, &id, "2026-07-25T10:30:00Z", &path);

    let now: jiff::Timestamp = "2026-07-25T10:35:00Z".parse().unwrap();
    let pending = pending_attributions(e).unwrap();
    let pa = pending
        .iter()
        .find(|p| p.task_id == id)
        .expect("the completed task is pending attribution");
    let r = compute_attribution(pa, now).unwrap();
    assert!(r.found, "the live bank must succeed");
    attribute_one(e, pa, &r).unwrap();
}

/// D50 Decision 3 as amended: ONLY CONTEST REMOVES TOKENS. An honestly banked,
/// uncontested measurement whose sample is later re-emitted with a stamp past
/// the window's end (the documented mid-write drift; dedupe keeps the last
/// stamp) re-reads as zero — with no contest, that is evidence drift, and the
/// row must be kept and downgraded exactly like an unreadable transcript,
/// never deleted.
#[test]
fn recompute_keeps_and_downgrades_an_uncontested_drift_to_zero() {
    let dir = scratch_dir("drift-zero");
    let transcript = dir.join("sess-1.jsonl");
    std::fs::write(
        &transcript,
        r#"{"timestamp":"2026-07-25T10:10:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
    )
    .unwrap();

    let e = engine();
    let t = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    bank_live(&e, &t, &transcript);

    // The tool re-emits "m" stamped past the window's end. Nothing else
    // changes, and no other task exists — the measurement is uncontested.
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(f).unwrap();
    writeln!(
        f,
        r#"{{"timestamp":"2026-07-25T10:45:00.000Z","message":{{"id":"m","usage":{{"input_tokens":1000,"output_tokens":2000}}}}}}"#
    )
    .unwrap();

    let dry = dispatch(&e, "tokens.recompute", &json!({})).unwrap();
    assert_eq!(dry["tasks"][0]["action"], "downgraded", "{dry}");
    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let entry = &r["tasks"][0];
    assert_eq!(entry["action"], "downgraded", "{r}");
    assert_eq!(entry["before"], b4(1000, 2000));
    assert_eq!(entry["after"], b4(1000, 2000), "counts kept, not deleted");

    let (input, confidence): (i64, String) = e
        .conn()
        .query_row(
            "SELECT input_tokens, confidence FROM token_usage WHERE source = 'log-parse'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the uncontested row must survive the recompute");
    assert_eq!(input, 1000);
    assert_eq!(confidence, "low");

    // Convergence: a second apply reports the already-low row as unchanged.
    let again = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    assert_eq!(again["tasks"][0]["action"], "unchanged", "{again}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The partial variant of the same policy, decided as: NEVER SILENTLY SHRINK.
/// One banked sample drifts past the window edge, the other still reads fine,
/// and nothing is contested — the recompute cannot tell drift from theft at
/// sample granularity here, so the WHOLE row is kept and downgraded rather
/// than quietly rewritten to the smaller remainder.
#[test]
fn recompute_keeps_the_whole_row_when_part_of_the_evidence_drifts_uncontested() {
    let dir = scratch_dir("drift-part");
    let transcript = dir.join("sess-1.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-07-25T10:10:00.000Z","message":{"id":"m1","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
            "\n",
            r#"{"timestamp":"2026-07-25T10:20:00.000Z","message":{"id":"m2","usage":{"input_tokens":500,"output_tokens":600}}}"#,
            "\n",
        ),
    )
    .unwrap();

    let e = engine();
    let t = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    bank_live(&e, &t, &transcript);

    // Only "m1" is re-emitted past the window's end; "m2" stays put.
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        f,
        r#"{{"timestamp":"2026-07-25T10:45:00.000Z","message":{{"id":"m1","usage":{{"input_tokens":1000,"output_tokens":2000}}}}}}"#
    )
    .unwrap();

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let entry = &r["tasks"][0];
    assert_eq!(entry["action"], "downgraded", "{r}");
    assert_eq!(entry["before"], b4(1500, 2600));
    assert_eq!(
        entry["after"],
        b4(1500, 2600),
        "no silent shrink to 500/600"
    );

    let (input, output, confidence): (i64, i64, String) = e
        .conn()
        .query_row(
            "SELECT input_tokens, output_tokens, confidence FROM token_usage \
             WHERE source = 'log-parse'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((input, output), (1500, 2600));
    assert_eq!(confidence, "low");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The replay must follow the order live ticks BANKED measurements — the
/// earliest marker that recorded a measurement — not the earliest marker of
/// any kind. A task's first marker can be an EMPTY one (found=false, no row)
/// from before a reopen; ordering on it replays a reopened thief before the
/// task that actually banked first, handing the thief the spend
/// (rc_order_winner).
#[test]
fn recompute_replays_banks_in_the_order_they_banked_not_first_marker_order() {
    let dir = scratch_dir("bank-order");
    let copy_a = dir.join("copyA.jsonl");
    let copy_b = dir.join("copyB.jsonl");
    let line = r#"{"timestamp":"2026-07-25T10:10:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#;
    std::fs::write(&copy_a, line).unwrap();
    std::fs::write(&copy_b, line).unwrap();
    let path_a = copy_a.to_string_lossy().into_owned();
    let path_b = copy_b.to_string_lossy().into_owned();

    let e = engine();
    let a = e.task_add(&json!({ "title": "A" })).unwrap()["short_id"].clone();
    let b = e.task_add(&json!({ "title": "B" })).unwrap()["short_id"].clone();

    // A: first completion, attributed EMPTY (marker M1, no measurement row).
    e.task_done(&json!({ "ref": a, "client": "claude-code", "transcript_path": path_a }))
        .unwrap();
    e.token_attribute(&json!({
        "ref": a, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 0,
    }))
    .unwrap();

    // B: banks "m" from its own byte-copy, pre-upgrade marker (M2, no ids) —
    // the LIVE owner of the spend.
    e.task_done(&json!({ "ref": b, "client": "claude-code", "transcript_path": path_b }))
        .unwrap();
    e.token_attribute(&json!({
        "ref": b, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();

    // A: reopen + redone, post-upgrade theft of the same id (M3, with ids).
    e.task_reopen(&json!({ "ref": a })).unwrap();
    e.task_done(&json!({ "ref": a, "client": "claude-code", "transcript_path": path_a }))
        .unwrap();
    e.token_attribute(&json!({
        "ref": a, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000, "sample_ids": ["m"],
    }))
    .unwrap();

    // Pin windows so both cover the 10:10 stamp in their own files.
    let (a_id, b_id) = (task_uuid(&e, &a), task_uuid(&e, &b));
    pin_created(&e, &a_id, "2026-07-25T10:00:00Z");
    pin_created(&e, &b_id, "2026-07-25T10:05:00Z");
    pin_done(&e, &a_id, "2026-07-25T10:45:00Z", &path_a);
    pin_done(&e, &b_id, "2026-07-25T10:30:00Z", &path_b);

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let tasks = r["tasks"].as_array().unwrap();
    // B banked FIRST (its measuring marker precedes A's; A's earlier marker
    // was empty), so B is replayed first and re-earns the claim on "m"...
    assert_eq!(tasks[0]["task"], b, "{r}");
    assert_eq!(
        tasks[0]["after"],
        b4(1000, 2000),
        "the live owner keeps: {r}"
    );
    // ...and the reopened thief finds "m" contested and is zeroed.
    assert_eq!(tasks[1]["task"], a, "{r}");
    assert_eq!(tasks[1]["action"], "recomputed", "{r}");
    assert_eq!(tasks[1]["after"], b4(0, 0), "the thief is zeroed: {r}");

    let owners: Vec<(i64, i64)> = {
        let mut stmt = e
            .conn()
            .prepare(
                "SELECT t.short_id, u.input_tokens FROM token_usage u \
                 JOIN tasks t ON t.id = u.task_id WHERE u.source='log-parse'",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert_eq!(
        owners,
        vec![(b.as_i64().unwrap(), 1000)],
        "one row, owned by the live owner"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The channel_conflict arm removes a task's log-parse rows because its real
/// spend is the self-report — but the samples its bank CONSUMED stay consumed:
/// the banked ids must enter the in-pass claim set exactly like the downgrade
/// arm's, or a later task's recompute re-earns the same spend and the
/// double-count reappears across channels.
#[test]
fn channel_conflict_keeps_the_tasks_banked_claims_contesting_later_tasks() {
    let dir = scratch_dir("conflict-claims");
    let copy = dir.join("copy.jsonl");
    std::fs::write(
        &copy,
        r#"{"timestamp":"2026-07-25T10:10:00.000Z","message":{"id":"m","usage":{"input_tokens":1000,"output_tokens":2000}}}"#,
    )
    .unwrap();
    let copy_path = copy.to_string_lossy().into_owned();
    let gone_path = dir.join("gone.jsonl").to_string_lossy().into_owned();

    let e = engine();
    let c = e.task_add(&json!({ "title": "C" })).unwrap()["short_id"].clone();
    let d = e.task_add(&json!({ "title": "D" })).unwrap()["short_id"].clone();
    // C banked "m" first, WITH its id persisted — then also self-reported
    // (pre-TOCTOU-fix history), which makes it a channel conflict.
    e.task_done(&json!({ "ref": c, "client": "claude-code", "transcript_path": gone_path }))
        .unwrap();
    e.token_attribute(&json!({
        "ref": c, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000, "sample_ids": ["m"],
    }))
    .unwrap();
    e.token_add(&json!({
        "ref": c, "tool": "claude-code", "source": "self-report", "confidence": "medium",
        "input_tokens": 111, "output_tokens": 222,
    }))
    .unwrap();
    // D banked the same id later from its own byte-copy, in a disjoint window.
    e.task_done(&json!({ "ref": d, "client": "claude-code", "transcript_path": copy_path }))
        .unwrap();
    e.token_attribute(&json!({
        "ref": d, "source": "log-parse", "tool": "claude-code", "confidence": "medium",
        "samples": 1, "input_tokens": 1000, "output_tokens": 2000,
    }))
    .unwrap();
    let (c_id, d_id) = (task_uuid(&e, &c), task_uuid(&e, &d));
    pin_created(&e, &c_id, "2026-07-25T09:00:00Z");
    pin_done(&e, &c_id, "2026-07-25T09:10:00Z", &gone_path);
    pin_created(&e, &d_id, "2026-07-25T10:00:00Z");
    pin_done(&e, &d_id, "2026-07-25T10:30:00Z", &copy_path);

    let r = dispatch(&e, "tokens.recompute", &json!({ "dry_run": false })).unwrap();
    let tasks = r["tasks"].as_array().unwrap();
    assert_eq!(tasks[0]["task"], c, "{r}");
    assert_eq!(tasks[0]["action"], "channel_conflict", "{r}");
    assert_eq!(tasks[1]["task"], d, "{r}");
    assert_eq!(
        tasks[1]["after"],
        b4(0, 0),
        "sample `m` stays consumed by C's bank even though C's rows moved \
         aside for its self-report — D must not re-earn it: {r}"
    );
    assert_eq!(
        count(
            &e,
            "SELECT COUNT(*) FROM token_usage WHERE source='log-parse'"
        ),
        0
    );

    let _ = std::fs::remove_dir_all(&dir);
}
