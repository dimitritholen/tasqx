//! Per-task AI token accounting (docs/research/token-accounting.md, #11-#13):
//! the `token.add` mutation, its read surfaces (task.get, export), the D12
//! round trip, and the no-rev-bump rule that keeps async attribution from
//! breaking a client's `expected_rev`.

use serde_json::json;
use tasqx_core::{dispatch, Engine, ErrorCode};

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

/// `tool`/`model` without a single count would change nothing — refused
/// rather than silently ignored (the D33 rule).
#[test]
fn task_done_tool_without_counts_is_refused() {
    let e = engine();
    let sid = e.task_add(&json!({ "title": "t" })).unwrap()["short_id"].clone();
    let err = e
        .task_done(&json!({ "ref": sid, "tool": "cursor" }))
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("tool"), "{}", err.message);
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
                    format!(
                        r#"{{"completed":"{completed}","client":"claude-code","transcript_path":"{path}"}}"#
                    ),
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
