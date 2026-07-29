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

    tokens(&mut out, result);
    annotations(&mut out, result, opts);

    out
}

/// Measurements as a table. Only rendered when there is at least one: a
/// "Tokens (0)" heading on every unmeasured task is a heading that teaches
/// nothing.
///
/// The four buckets are NEVER summed into one number here. They are priced
/// differently and a single total silently misprices the mix — the design's
/// first rule, and the reason the table has four columns rather than one.
fn tokens(out: &mut String, result: &Value) {
    let Some(rows) = result.get("tokens").and_then(Value::as_array) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("\n### Tokens ({})\n\n", rows.len()));
    out.push_str("| tool | in | out | cache read | cache write | source | confidence |\n");
    out.push_str("|---|---:|---:|---:|---:|---|---|\n");
    for m in rows {
        let n = |k: &str| m.get(k).and_then(Value::as_i64).unwrap_or(0);
        let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            s("tool"),
            n("input_tokens"),
            n("output_tokens"),
            n("cache_read_tokens"),
            n("cache_creation_tokens"),
            s("source"),
            s("confidence"),
        ));
    }
}

/// Annotations, bodies untouched.
///
/// The header line is BOLD TEXT, not a markdown heading, on purpose: bodies in
/// this project carry their own `##` headings, and a heading here would put the
/// renderer's structure and the author's structure in the same hierarchy,
/// competing. A blockquote was the other option and is worse — it breaks tables
/// and fenced code inside the body.
fn annotations(out: &mut String, result: &Value, opts: &DetailOpts) {
    let Some(rows) = result.get("annotations").and_then(Value::as_array) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("\n### Annotations ({})\n\n", rows.len()));
    for a in rows {
        let when = fmt_instant(a.get("created").and_then(Value::as_str).unwrap_or(""), opts);
        let body = a.get("body").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("---\n**{when}**\n\n{body}"));
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
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

/// Format one instant per `opts.time`.
///
/// An unparseable value falls back to itself. The store holds RFC-3339 strings,
/// so this should not happen — but "should not" is not "cannot", and a detail
/// view is the wrong place to discover that by panicking.
fn fmt_instant(iso: &str, opts: &DetailOpts) -> String {
    if iso.is_empty() {
        return String::new();
    }
    let Ok(then) = iso.parse::<Timestamp>() else {
        return iso.to_string();
    };
    let rel = humanize_ago(then, opts.now);
    match opts.time {
        TimeFormat::Iso => iso.to_string(),
        TimeFormat::Relative => rel,
        TimeFormat::Both => format!("{iso} ({rel})"),
    }
}

/// Format one ISO-8601 duration per `opts.time`. Anything the shared reader
/// cannot read falls back to the raw string.
///
/// The reader is [`crate::util::duration_secs`] — the same one `parse_duration`
/// validates against and `report` sums with — and NOT a local one. A private
/// copy here recognised only D/H/M/S, so `estimate:P2W` (which the store's
/// validator accepts verbatim) rendered as the literal `P2W` under
/// `TimeFormat::Relative`, and its unchecked arithmetic reintroduced the very
/// overflow panic `duration_secs`' checked form exists to prevent. One
/// vocabulary, one overflow policy, one place to change either.
fn fmt_duration(iso: &str, opts: &DetailOpts) -> String {
    if iso.is_empty() {
        return String::new();
    }
    let Some(secs) = crate::util::duration_secs(iso) else {
        return iso.to_string();
    };
    let human = humanize_secs(secs);
    match opts.time {
        TimeFormat::Iso => iso.to_string(),
        TimeFormat::Relative => human,
        TimeFormat::Both => format!("{iso} ({human})"),
    }
}

/// "2 hours ago" / "in 1 day". Coarse on purpose: a detail view answers "roughly
/// when", and the exact instant is one `TimeFormat` away for anyone who needs it.
fn humanize_ago(then: Timestamp, now: Timestamp) -> String {
    let secs = now.as_second() - then.as_second();
    if secs.abs() < 60 {
        return "just now".to_string();
    }
    let span = humanize_span(secs.abs());
    if secs > 0 {
        format!("{span} ago")
    } else {
        format!("in {span}")
    }
}

/// A duration as one compact unit: `45s`, `12m`, `2h`, `3d`.
///
/// Compact because durations are what a reader compares at a glance — an
/// estimate against a tracked time, one task against the next — and prose
/// makes that scan slower, not clearer. Elapsed time gets [`humanize_span`]
/// instead, which reads as a sentence because that is how it is read.
///
/// The unit count is ROUNDED, not truncated. Truncation reads as a lie at the
/// top of a bucket — an hour and fifty-nine minutes is "2 hours" to anyone
/// glancing at it, and calling it "1 hour" makes the view look stale rather
/// than coarse.
fn humanize_secs(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", round_div(s, 60)),
        s if s < 86_400 => format!("{}h", round_div(s, 3600)),
        s => format!("{}d", round_div(s, 86_400)),
    }
}

/// The same span written as prose: `45 seconds`, `1 hour`, `3 days`. Feeds the
/// `… ago` / `in …` sentence, where `3d ago` would read as a typo.
fn humanize_span(secs: i64) -> String {
    match secs {
        s if s < 60 => plural(s, "second"),
        s if s < 3600 => plural(round_div(s, 60), "minute"),
        s if s < 86_400 => plural(round_div(s, 3600), "hour"),
        s => plural(round_div(s, 86_400), "day"),
    }
}

/// `1 hour` / `2 hours`. English only, and only for the four units above.
fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// `secs / unit`, rounded to nearest rather than toward zero. Both arguments are
/// non-negative here — `humanize_ago` takes the absolute value before calling.
fn round_div(secs: i64, unit: i64) -> i64 {
    (secs + unit / 2) / unit
}
