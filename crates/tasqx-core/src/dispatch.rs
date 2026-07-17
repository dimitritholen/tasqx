//! The single dispatch table + the JSON envelope handler (DESIGN.md §2, §4).
//!
//! There is exactly one dispatch function. "Call a function" (CLI, in-process)
//! and "send a JSON command" (stdio `api`) run identical code: the CLI calls
//! `dispatch` directly, while `handle_envelope` wraps the same call with
//! version checking and the request/response envelope.

use serde_json::{json, Map, Value};

use crate::engine::Engine;
use crate::error::{ApiError, ErrorBody};
use crate::types::ApiRequest;

/// The API major version this build speaks.
pub const API_VERSION: &str = "1";

/// The single dispatch table. Routes a method name + params to a handler.
/// This is the load-bearing seam every surface shares.
pub fn dispatch(engine: &Engine, method: &str, params: &Value) -> Result<Value, ApiError> {
    match method {
        "project.create" => engine.project_create(params),
        "task.add" => engine.task_add(params),
        "task.list" => engine.task_list(params),
        "task.start" => engine.task_start(params),
        "task.stop" => engine.task_stop(params),
        "task.done" => engine.task_done(params),
        "task.modify" => engine.task_modify(params),
        "task.get" => engine.task_get(params),
        "task.cancel" => engine.task_cancel(params),
        "task.reopen" => engine.task_reopen(params),
        "tag.add" => engine.tag_add(params),
        "project.list" => engine.project_list(params),
        "project.use" => engine.project_use(params),
        "project.archive" => engine.project_archive(params),
        "annotation.add" => engine.annotation_add(params),
        "dependency.add" => engine.dependency_add(params),
        "dependency.remove" => engine.dependency_remove(params),
        "report.summary" => engine.report_summary(params),
        "store.export" => engine.store_export(params),
        "store.import" => engine.store_import(params),
        "event.list" => engine.event_list(params),
        "reminder.fire" => engine.reminder_fire(params),
        "core.capabilities" => Ok(engine.capabilities()),
        other => Err(ApiError::bad_request(format!("unknown method: {other}"))),
    }
}

/// The MVP method surface, for `core.capabilities` feature-detection.
pub fn capabilities() -> Value {
    json!({
        "api": API_VERSION,
        "methods": [
            "project.create",
            "project.list",
            "project.use",
            "project.archive",
            "task.add",
            "task.list",
            "task.get",
            "task.start",
            "task.stop",
            "task.done",
            "task.modify",
            "task.cancel",
            "task.reopen",
            "tag.add",
            "annotation.add",
            "dependency.add",
            "dependency.remove",
            "report.summary",
            "store.export",
            "store.import",
            "event.list",
            "reminder.fire",
            "core.capabilities",
        ],
        "features": ["dependencies", "filter.boolean", "reminders"],
    })
}

/// Parse one request envelope, dispatch it, and build the response envelope.
/// This is the whole stdio one-shot transport: one request in, one response
/// out. Never returns Err — transport/validation failures become error
/// envelopes so the caller always has a well-formed response to emit.
pub fn handle_envelope(engine: &Engine, input: &str) -> Value {
    let req: ApiRequest = match serde_json::from_str(input) {
        Ok(r) => r,
        Err(e) => {
            return error_envelope(
                Value::Null,
                &ApiError::bad_request(format!("malformed request envelope: {e}")),
            )
        }
    };

    let id = req.id.clone().unwrap_or(Value::Null);

    if req.tasqx != API_VERSION {
        return error_envelope(
            id,
            &ApiError::new(
                crate::error::ErrorCode::UnsupportedVersion,
                format!("unsupported api major version: {}", req.tasqx),
                Some(json!({ "supported": API_VERSION })),
            ),
        );
    }

    let params = req.params.unwrap_or_else(|| json!({}));
    match dispatch(engine, &req.method, &params) {
        Ok(result) => success_envelope(id, result),
        Err(e) => error_envelope(id, &e),
    }
}

fn success_envelope(id: Value, result: Value) -> Value {
    let mut m = Map::new();
    m.insert("tasqx".into(), Value::String(API_VERSION.into()));
    if !id.is_null() {
        m.insert("id".into(), id);
    }
    m.insert("ok".into(), Value::Bool(true));
    m.insert("result".into(), result);
    Value::Object(m)
}

fn error_envelope(id: Value, err: &ApiError) -> Value {
    let mut m = Map::new();
    m.insert("tasqx".into(), Value::String(API_VERSION.into()));
    if !id.is_null() {
        m.insert("id".into(), id);
    }
    m.insert("ok".into(), Value::Bool(false));
    m.insert("error".into(), serde_json::to_value(ErrorBody::from(err)).unwrap_or(Value::Null));
    Value::Object(m)
}
