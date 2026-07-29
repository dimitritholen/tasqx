//! The task-detail view tasqx renders for itself.
//!
//! Every MCP tool returns pretty-printed JSON, which means the detail screen a
//! user sees is composed by whichever agent is asking, in that conversation.
//! Two callers get two layouts for one task. This module is the answer: one
//! task, one rendering, decided here rather than downstream.
//!
//! It is deliberately PURE — no store, no clock, no environment, no theme. That
//! is not tidiness: output that depends on any of those is not identical between
//! callers, which is the entire property this exists to provide. `now` is a
//! parameter for the same reason `compute_attribution` takes one.

use crate::types::Status;
use jiff::Timestamp;
use serde_json::Value;

/// How instants and durations are written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeFormat {
    /// `2026-07-29T09:00:58Z` — exact, sortable, unambiguous across locales.
    Iso,
    /// `2 hours ago` — readable, and time-dependent by construction.
    Relative,
    /// Both, the relative form in parentheses. The default: it costs one line
    /// of width and removes the reason most readers would need to change it.
    Both,
}

/// Everything the renderer needs beyond the task itself.
pub struct DetailOpts {
    /// How instants and durations are written.
    pub time: TimeFormat,
    /// The reference point for relative formatting. A parameter, never a
    /// `Timestamp::now()` call, so the output is reproducible and testable.
    pub now: Timestamp,
}

/// Render one `task.get` result as markdown.
///
/// Never panics and never returns an empty string: a `get_task` that broke on
/// presentation would be strictly worse than the plain JSON it replaces. Every
/// field is read tolerantly — a missing or mistyped one costs its row, nothing
/// more.
pub fn task_detail(result: &Value, opts: &DetailOpts) -> String {
    let mut out = String::new();
    let sid = result.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let title = str_of(result, "title");
    out.push_str(&format!("## #{sid} · {title}\n\n"));

    out.push_str("| | |\n|---|---|\n");
    row(&mut out, "status", &status_cell(result));
    row(&mut out, "priority", &priority_cell(result));
    row(&mut out, "project", &str_of(result, "project"));

    if let Some(tags) = result.get("tags").and_then(Value::as_array) {
        let names: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() {
            row(&mut out, "tags", &names.join(", "));
        }
    }
    opt_duration(&mut out, result, "estimate", "estimate", opts);
    opt_duration(&mut out, result, "tracked", "tracked", opts);
    for (key, label) in [
        ("due", "due"),
        ("scheduled", "scheduled"),
        ("wait", "wait"),
        ("remind", "remind"),
    ] {
        opt_instant(&mut out, result, key, label, opts);
    }
    if let Some(r) = result.get("recurrence").and_then(Value::as_str) {
        if !r.is_empty() {
            row(&mut out, "recurrence", r);
        }
    }
    for (key, label) in [("active_since", "active since"), ("completed", "completed")] {
        opt_instant(&mut out, result, key, label, opts);
    }
    // Only when true: "not blocked" is the norm, and a `no` on every task is
    // noise that pushes the rows a reader wants further down.
    if result.get("blocked").and_then(Value::as_bool) == Some(true) {
        row(&mut out, "blocked", "yes");
    }
    if let Some(deps) = result.get("depends_on").and_then(Value::as_array) {
        let refs: Vec<String> = deps
            .iter()
            .filter_map(Value::as_i64)
            .map(|n| format!("#{n}"))
            .collect();
        if !refs.is_empty() {
            row(&mut out, "depends on", &refs.join(", "));
        }
    }

    row(
        &mut out,
        "created",
        &fmt_instant(&str_of(result, "created"), opts),
    );
    row(
        &mut out,
        "modified",
        &fmt_instant(&str_of(result, "modified"), opts),
    );
    let rev = result.get("_rev").and_then(Value::as_i64).unwrap_or(0);
    row(&mut out, "rev", &rev.to_string());

    out
}

/// One table row. Central so every row is spaced identically — a golden test
/// over the whole output turns any drift here into a failure, which is only
/// useful if there is one place to fix.
fn row(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("| {label} | {value} |\n"));
}

/// An instant row, emitted only when the field holds a non-empty string. A
/// JSON `null` and an absent key are the same thing to a reader.
fn opt_instant(out: &mut String, result: &Value, key: &str, label: &str, opts: &DetailOpts) {
    if let Some(v) = result.get(key).and_then(Value::as_str) {
        if !v.is_empty() {
            row(out, label, &fmt_instant(v, opts));
        }
    }
}

/// As `opt_instant`, for ISO-8601 durations (`PT3H`).
fn opt_duration(out: &mut String, result: &Value, key: &str, label: &str, opts: &DetailOpts) {
    if let Some(v) = result.get(key).and_then(Value::as_str) {
        if !v.is_empty() {
            row(out, label, &fmt_duration(v, opts));
        }
    }
}

/// A string field, or an empty string. Never panics on a non-string.
fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Status, with a warning when this build does not recognise it.
///
/// `render.rs:390` already does this for the terminal; without it here the MCP
/// view would be more forgiving than the CLI about the same fault, and a status
/// written by a newer build would read as ordinary. The valid set is DERIVED
/// from `Status::ALL` via [`Status::accepted`], never retyped — that is D30, and
/// it is what keeps the message from falling behind the enum.
fn status_cell(result: &Value) -> String {
    let status = str_of(result, "status");
    if result.get("status_unrecognized").and_then(Value::as_bool) != Some(true) {
        return status;
    }
    format!("{status} (unrecognized — not one of {})", Status::accepted())
}

/// Priority with urgency folded in: two numbers that only mean something
/// together, and a reader comparing tasks wants both without a second row.
fn priority_cell(result: &Value) -> String {
    let p = result.get("priority").and_then(Value::as_str).unwrap_or("-");
    match result.get("urgency").and_then(Value::as_f64) {
        Some(u) => format!("{p} (urgency {u})"),
        None => p.to_string(),
    }
}

/// Format one instant. Task 4 replaces the body; ISO is the only branch that
/// exists yet, and returning the raw value keeps this honest in the meantime.
fn fmt_instant(iso: &str, _opts: &DetailOpts) -> String {
    iso.to_string()
}

/// Format one duration. Task 4 replaces the body; ISO is the only branch yet.
fn fmt_duration(iso: &str, _opts: &DetailOpts) -> String {
    iso.to_string()
}
