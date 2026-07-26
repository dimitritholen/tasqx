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

/// The params keys each method accepts, and whether its params object is a
/// *document* rather than a *request*.
///
/// D33. The gate this feeds exists because a misspelled key was accepted and
/// ignored: `task.add {"prioritee":"H"}` answered `ok:true` and created a task
/// with no priority, and nothing anywhere recorded that one had been asked for.
/// That is D27's silent-widening failure moved from the filter grammar to the
/// params object, and on a write it is worse than a wrong answer — it is an
/// unfalsifiable one.
///
/// **This table is not hand-maintained in the sense that has cost this project
/// before.** It is data at runtime (the gate and `core.capabilities` both read
/// it) but it is *checked against the code that does the reading*: the
/// dispatch-table drift guard below extracts, from `engine.rs` source, the
/// literal keys each handler pulls out of its params, and fails the build if
/// the two disagree. A param added tomorrow joins this table the day it is
/// written or the suite goes red (D30's rule: derive it).
///
/// **The `document` flag is the D12 compatibility carve-out.** An export from a
/// *newer* tasqx must stay readable by an older binary, so the gate stops at the
/// request surface: `store.import`'s params object is a data document, and a
/// top-level key this build has never heard of is a future field, not a typo.
/// That is D28's inversion again — refuse bad input at the door, never become
/// unable to read data that already exists. The tolerance is safe only because
/// `tasks` is required, so a misspelled `taskss` is still refused, by absence.
pub const PARAMS: &[(&str, &[&str], bool)] = &[
    ("project.create", &["name", "description"], false),
    ("project.list", &["include_archived"], false),
    ("project.use", &["name"], false),
    ("project.archive", &["name"], false),
    (
        "task.add",
        &[
            "title",
            "project",
            "priority",
            "due",
            "scheduled",
            "wait",
            "estimate",
            "tags",
            "recurrence",
            "remind",
        ],
        false,
    ),
    ("task.list", &["filter", "sort", "limit", "fields"], false),
    ("task.get", &["ref"], false),
    // task.start/task.done also take the #12 correlation params: they are
    // stored in the start/done event payloads, the durable per-occurrence
    // record the async token-attribution engine reads later.
    (
        "task.start",
        &[
            "ref",
            "keep",
            "session_id",
            "prompt_id",
            "transcript_path",
            "client",
        ],
        false,
    ),
    ("task.stop", &["ref"], false),
    // task.done additionally takes the #13 self-report params: any present
    // token count records one self-report measurement in the completing
    // transaction, echoed in the done event payload.
    (
        "task.done",
        &[
            "ref",
            "session_id",
            "prompt_id",
            "transcript_path",
            "client",
            "tool",
            "model",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
        ],
        false,
    ),
    ("task.modify", &["ref", "set", "expected_rev"], false),
    ("task.cancel", &["ref"], false),
    ("task.reopen", &["ref"], false),
    ("tag.add", &["ref", "tags"], false),
    ("annotation.add", &["ref", "body"], false),
    (
        "token.add",
        &[
            "ref",
            "tool",
            "source",
            "model",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "confidence",
        ],
        false,
    ),
    ("dependency.add", &["ref", "depends_on"], false),
    ("dependency.remove", &["ref", "depends_on"], false),
    ("memory.add", &["title", "body", "source"], false),
    ("memory.search", &["query", "limit", "scope", "raw"], false),
    ("memory.remove", &["id"], false),
    ("memory.import", &["docs"], false),
    (
        "report.summary",
        &["group_by", "filter", "metrics", "all"],
        false,
    ),
    ("store.export", &["filter"], false),
    (
        "store.import",
        &["tasks", "projects", "default_project", "docs"],
        true,
    ),
    ("event.list", &["limit", "ref", "entity"], false),
    ("reminder.fire", &["ref", "at"], false),
    ("core.capabilities", &[], false),
];

/// Refuse a params object carrying a key the method does not read.
///
/// Runs at `dispatch`, the one seam every surface shares, rather than in each
/// handler — the D31 move: a contract kept by *remembering to check* becomes a
/// contract a method cannot be reached without honouring.
fn check_params(method: &str, params: &Value) -> Result<(), ApiError> {
    // An unknown method is the match's error to raise, with its own wording.
    let Some((_, accepted, document)) = PARAMS.iter().find(|(m, _, _)| *m == method) else {
        return Ok(());
    };
    let obj = match params {
        // `null` and an omitted `params` are both "no params" (handle_envelope
        // already substitutes `{}` for the latter).
        Value::Null => return Ok(()),
        Value::Object(o) => o,
        other => {
            return Err(ApiError::bad_request(format!(
                "`params` must be an object, but {other} was given — send {{ … }} or omit it"
            )))
        }
    };
    if *document {
        return Ok(());
    }
    let unknown: Vec<&str> = obj
        .keys()
        .filter(|k| !accepted.contains(&k.as_str()))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    // Naming the accepted set is the whole point: the caller mistyped a name,
    // so the fix is one glance away only if the right names are in the error.
    let accepted_list = if accepted.is_empty() {
        "no params".to_string()
    } else {
        accepted.join(", ")
    };
    let (label, names) = if unknown.len() == 1 {
        ("unknown params key", format!("`{}`", unknown[0]))
    } else {
        (
            "unknown params keys",
            unknown
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", "),
        )
    };
    Err(ApiError::bad_request(format!(
        "{label} {names} for {method} (accepted: {accepted_list}) — check the spelling or drop it; \
         it was silently ignored before, which meant the answer looked fine and was not"
    )))
}

/// The single dispatch table. Routes a method name + params to a handler.
/// This is the load-bearing seam every surface shares.
pub fn dispatch(engine: &Engine, method: &str, params: &Value) -> Result<Value, ApiError> {
    check_params(method, params)?;
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
        "token.add" => engine.token_add(params),
        "dependency.add" => engine.dependency_add(params),
        "dependency.remove" => engine.dependency_remove(params),
        "memory.add" => engine.memory_add(params),
        "memory.search" => engine.memory_search(params),
        "memory.remove" => engine.memory_remove(params),
        "memory.import" => engine.memory_import(params),
        "report.summary" => engine.report_summary(params),
        "store.export" => engine.store_export(params),
        "store.import" => engine.store_import(params),
        "event.list" => engine.event_list(params),
        "reminder.fire" => engine.reminder_fire(params),
        "core.capabilities" => engine.capabilities(),
        other => Err(ApiError::bad_request(format!("unknown method: {other}"))),
    }
}

/// The MVP method surface, for `core.capabilities` feature-detection.
///
/// Both halves render from [`PARAMS`]: the method list is the table's keys, and
/// `params` publishes the accepted set the gate enforces, so a client can check
/// a call before making it and the published answer cannot disagree with the
/// enforced one. The method list used to be a second hand-kept copy of the
/// dispatch match — the same drift shape one level up.
pub fn capabilities() -> Value {
    json!({
        "api": API_VERSION,
        "methods": PARAMS.iter().map(|(m, _, _)| *m).collect::<Vec<_>>(),
        "params": PARAMS.iter().map(|(m, keys, _)| (m.to_string(), json!(keys))).collect::<Map<_, _>>(),
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
    m.insert(
        "error".into(),
        serde_json::to_value(ErrorBody::from(err)).unwrap_or(Value::Null),
    );
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// Every `fn` in `engine.rs` → the literal params keys it reads out of a
    /// value named `p`, expanded one level through the helpers that take `p`
    /// whole (`resolve_ref(p)`, `parse_sort(p)`, `parse_fields(p)`).
    ///
    /// Reading the source is the point (D30: derive it, don't keep a list in
    /// sync). The import loop is invisible to this on purpose — it reads its
    /// keys off `tv`, the task document, not off `p`.
    fn keys_read_per_fn() -> BTreeMap<String, BTreeSet<String>> {
        let src = [
            include_str!("engine.rs"),
            include_str!("engine/commands.rs"),
            include_str!("engine/memory.rs"),
            include_str!("engine/projects.rs"),
            include_str!("engine/relationships.rs"),
            include_str!("engine/reports.rs"),
            include_str!("engine/task.rs"),
            include_str!("engine/tokens.rs"),
            include_str!("engine/transfer.rs"),
        ]
        .join("\n");
        // Strip comments and collapse whitespace: a chain split across lines
        // must read as one, and the prose quotes param names constantly.
        let flat: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();

        // Split at every `fn NAME(`; each slice is that fn's body (plus the
        // next signature, which contributes no `p` reads of its own).
        let mut bounds: Vec<(String, usize)> = Vec::new();
        for (i, _) in flat.match_indices("fn ").chain(flat.match_indices("fn")) {
            let rest = &flat[i + 2..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && rest[name.len()..].starts_with('(') {
                bounds.push((name, i));
            }
        }
        bounds.sort_by_key(|(_, i)| *i);
        bounds.dedup_by_key(|(_, i)| *i);

        let direct = |body: &str| -> BTreeSet<String> {
            let mut out = BTreeSet::new();
            for pat in ["(p,\"", "p.get(\""] {
                for (i, _) in body.match_indices(pat) {
                    let rest = &body[i + pat.len()..];
                    if let Some(end) = rest.find('"') {
                        out.insert(rest[..end].to_string());
                    }
                }
            }
            out
        };

        let mut bodies: BTreeMap<String, String> = BTreeMap::new();
        for (n, (name, start)) in bounds.iter().enumerate() {
            let end = bounds.get(n + 1).map(|(_, i)| *i).unwrap_or(flat.len());
            bodies.insert(name.clone(), flat[*start..end].to_string());
        }

        let passes_p = |body: &str, helper: &str| {
            let needle = format!("{helper}(");
            body.match_indices(&needle).any(|(i, _)| {
                let args = &body[i + needle.len()..];
                args.find(')')
                    .map(|end| args[..end].split(',').any(|arg| arg == "p"))
                    .unwrap_or(false)
            })
        };

        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for name in bodies.keys() {
            let mut keys = BTreeSet::new();
            let mut pending = vec![name.as_str()];
            let mut seen = BTreeSet::new();
            // Whole-`p` helper calls carry their keys into the caller. Follow
            // the chain transitively because connection-aware helpers pass
            // both a connection and `p` (for example `resolve_ref_on(&tx, p)`).
            while let Some(current) = pending.pop() {
                if !seen.insert(current) {
                    continue;
                }
                let body = &bodies[current];
                keys.extend(direct(body));
                for helper in bodies.keys() {
                    if helper != current && passes_p(body, helper) {
                        pending.push(helper);
                    }
                }
            }
            out.insert(name.clone(), keys);
        }
        out
    }

    /// The method → handler-fn map, read off the `dispatch` match itself so no
    /// step of this guard is hand-written.
    fn method_to_handler() -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for line in include_str!("dispatch.rs").lines() {
            let t = line.trim();
            let Some((lhs, rhs)) = t.split_once("=>") else {
                continue;
            };
            let Some(method) = lhs
                .trim()
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix("\""))
            else {
                continue;
            };
            let Some(i) = rhs.find("engine.") else {
                continue;
            };
            let name: String = rhs[i + "engine.".len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            out.insert(method.to_string(), name);
        }
        out
    }

    /// D33's drift guard, and the reason the accepted-key table is safe to
    /// exist at all: it is compared against the code that does the reading, so
    /// a param added tomorrow either joins the table or turns the suite red.
    /// The failure this project keeps paying for is a second copy nothing
    /// compares — that is what this removes.
    #[test]
    fn the_accepted_key_table_matches_the_keys_the_engine_actually_reads() {
        let per_fn = keys_read_per_fn();
        let handlers = method_to_handler();
        assert_eq!(
            handlers.len(),
            PARAMS.len(),
            "every dispatch arm must have a PARAMS row"
        );

        for (method, accepted, _) in PARAMS {
            let handler = handlers
                .get(*method)
                .unwrap_or_else(|| panic!("`{method}` is in PARAMS but not in the dispatch match"));
            let read = per_fn.get(handler).unwrap_or_else(|| {
                panic!("dispatch names `engine.{handler}`, which engine.rs lacks")
            });
            let declared: BTreeSet<String> = accepted.iter().map(|s| s.to_string()).collect();
            assert_eq!(
                &declared, read,
                "`{method}` (engine::{handler}): PARAMS declares {declared:?} but the code reads \
                 {read:?} — a key the engine reads and the table omits is silently REFUSED, and a \
                 key the table declares and nobody reads is silently IGNORED"
            );
        }
    }

    /// The gate must not have quietly turned an optional param into a required
    /// one, and `params: null` (a client with nothing to say) must still work.
    #[test]
    fn an_empty_or_absent_params_object_passes_the_gate() {
        for (method, _, _) in PARAMS {
            check_params(method, &json!({})).unwrap_or_else(|e| panic!("{method}: {}", e.message));
            check_params(method, &Value::Null)
                .unwrap_or_else(|e| panic!("{method}: {}", e.message));
        }
        // A non-object params is a caller sending something the engine cannot
        // read a single field from — the whole request ignored, not one key.
        let e = check_params("task.list", &json!([1, 2])).unwrap_err();
        assert!(e.message.contains("must be an object"), "{}", e.message);
    }
}
