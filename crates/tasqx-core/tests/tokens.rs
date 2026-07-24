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
