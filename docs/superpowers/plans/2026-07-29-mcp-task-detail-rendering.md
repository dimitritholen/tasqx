# MCP task-detail rendering — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `tasqx_get_task` returns a markdown detail view tasqx renders itself, followed by the JSON it already returns, so the same task reads identically for every caller.

**Architecture:** A pure function in `tasqx-core` turns one `task.get` result into markdown. `mcp.rs` prepends it as a second content block for that one tool. The CLI reads a new config key and injects the time format, keeping core config-agnostic.

**Tech Stack:** Rust 2024, `serde_json::Value`, `jiff::Timestamp`, clap 4 (config surface only), no new dependencies.

**Spec:** `docs/specs/2026-07-29-mcp-task-detail-rendering-design.md`

## Global Constraints

- No new crate dependencies. Everything here uses `serde_json` and `jiff`, both already in `tasqx-core`.
- `markdown::task_detail` must be **pure**: no store access, no `Timestamp::now()`, no environment read, no theme. `now` arrives as a parameter.
- The renderer must never panic and never return an empty string. No `unwrap()`/`expect()` on the shape of the input `Value`.
- Only `tasqx_get_task` changes shape. Every other MCP tool keeps returning exactly one content block.
- `cargo clippy --workspace --all-targets` must stay clean; the workspace denies warnings in CI.
- Commit messages: conventional-commit subject (`feat(core):`, `test(core):`), lowercase, imperative. Bodies explain *why*, following the existing history.
- Work on branch `feat/mcp-task-detail`, which already holds the spec commit.

## Note for the implementer: one spec gap found while planning

The spec says `detail.time_format` gets "a closed `choices` list, so `tasqx config set detail.time_format xyz` is refused". The registry does not work that way today: `Choices` (`config.rs:80`) is an *editor hint* only (`Free` | `Themes`), and `set` validates by `Kind` alone — `Kind::Str => value.into()` at `config.rs:560` accepts any string. Task 6 therefore adds a `Choices::OneOf(&'static [&'static str])` variant and teaches `set` to consult it. That widens `Choices` from "what an editor offers" to "what the setting accepts", which is a small, deliberate extension, not an accident.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/tasqx-core/src/markdown.rs` | **Create.** The whole renderer: `TimeFormat`, `DetailOpts`, `task_detail`, and the private row/format helpers. One module, one job. |
| `crates/tasqx-core/src/lib.rs:59-76` | **Modify.** Declare `pub mod markdown;` between `filter` and `mcp` (the list is alphabetical). |
| `crates/tasqx-core/src/mcp.rs:728` | **Modify.** Add `tool_ok_with_view`; route `tasqx_get_task` through it. Add the builder that carries the time format. |
| `crates/tasqx-cli/src/config.rs:80,112,560` | **Modify.** `Choices::OneOf`, the `detail.time_format` entry, and `Kind::Str` validation against it. |
| `crates/tasqx-cli/src/lib.rs:2565` | **Modify.** `run_mcp_serve` reads the key and passes it to the server. |
| `crates/tasqx-core/tests/markdown_detail.rs` | **Create.** Golden tests, robustness tests, and the drift guard. |

Tasks 1–4 build the renderer bottom-up; each ends with the workspace green. Task 5 makes it visible over MCP. Task 6 makes it configurable. Task 7 stops it from ageing.

---

### Task 1: The module, its types, and the always-present rows

**Files:**
- Create: `crates/tasqx-core/src/markdown.rs`
- Modify: `crates/tasqx-core/src/lib.rs:59-76`
- Test: `crates/tasqx-core/tests/markdown_detail.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub enum TimeFormat { Iso, Relative, Both }`, `pub struct DetailOpts { pub time: TimeFormat, pub now: Timestamp }`, `pub fn task_detail(result: &Value, opts: &DetailOpts) -> String`.

- [ ] **Step 1: Write the failing test**

Create `crates/tasqx-core/tests/markdown_detail.rs`:

```rust
//! Golden tests for the MCP task-detail view.
//!
//! These compare the WHOLE output byte-for-byte rather than asserting on
//! fragments. The promise this feature makes is "the same task reads the same
//! for everyone", and a test that checks a substring cannot tell the difference
//! between that promise and a near-miss.

use serde_json::json;
use tasqx_core::markdown::{task_detail, DetailOpts, TimeFormat};

/// A fixed instant so relative formatting is testable. Nothing here calls the
/// clock: the renderer takes `now` as a parameter precisely so this is possible.
fn at(iso: &str) -> jiff::Timestamp {
    iso.parse().expect("fixture timestamp parses")
}

fn iso_opts() -> DetailOpts {
    DetailOpts {
        time: TimeFormat::Iso,
        now: at("2026-07-29T11:00:00Z"),
    }
}

#[test]
fn a_minimal_task_renders_its_heading_and_core_rows() {
    let task = json!({
        "short_id": 76,
        "title": "Three field-test papercuts",
        "status": "pending",
        "priority": "L",
        "urgency": 1.8,
        "project": "tasqx-field-test-2026-07",
        "created": "2026-07-29T09:00:58Z",
        "modified": "2026-07-29T09:01:45Z",
        "_rev": 2
    });

    let expected = "\
## #76 · Three field-test papercuts

| | |
|---|---|
| status | pending |
| priority | L (urgency 1.8) |
| project | tasqx-field-test-2026-07 |
| created | 2026-07-29T09:00:58Z |
| modified | 2026-07-29T09:01:45Z |
| rev | 2 |
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}

#[test]
fn an_unrecognized_status_is_named_as_such_with_the_valid_set() {
    let task = json!({
        "short_id": 5,
        "title": "From a newer build",
        "status": "snoozed",
        "status_unrecognized": true,
        "priority": null,
        "urgency": 0.0,
        "project": null,
        "created": "2026-07-29T09:00:00Z",
        "modified": "2026-07-29T09:00:00Z",
        "_rev": 1
    });

    let out = task_detail(&task, &iso_opts());
    assert!(
        out.contains("| status | snoozed (unrecognized — not one of pending, active, done, cancelled, waiting) |"),
        "got:\n{out}"
    );
}
```

The valid set in that assertion is whatever `Status::ALL` currently holds — read
it from `crates/tasqx-core/src/types.rs` and copy the real order rather than
trusting this line. The point of the test is that the list is *derived*, so a new
variant changes both the code and this expectation together.

- [ ] **Step 2: Run the test and watch it fail**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: FAIL to compile — `unresolved import tasqx_core::markdown`.

- [ ] **Step 3: Create the module**

Create `crates/tasqx-core/src/markdown.rs`:

```rust
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
    row(&mut out, "created", &fmt_instant(&str_of(result, "created"), opts));
    row(&mut out, "modified", &fmt_instant(&str_of(result, "modified"), opts));
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

/// A string field, or an empty string. Never panics on a non-string.
fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Status, with a warning when this build does not recognise it.
///
/// `render.rs:390` already does this for the terminal; without it here the MCP
/// view would be more forgiving than the CLI about the same fault, and a status
/// written by a newer build would read as ordinary. The valid set is DERIVED
/// from `Status::ALL`, never retyped — that is D30, and it is what keeps the
/// message from falling behind the enum.
fn status_cell(result: &Value) -> String {
    let status = str_of(result, "status");
    if result.get("status_unrecognized").and_then(Value::as_bool) != Some(true) {
        return status;
    }
    let valid = crate::types::Status::ALL
        .map(crate::types::Status::as_str)
        .join(", ");
    format!("{status} (unrecognized — not one of {valid})")
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
```

- [ ] **Step 4: Declare the module**

In `crates/tasqx-core/src/lib.rs`, insert between the `filter` and `mcp` lines (the list is alphabetical):

```rust
pub mod markdown;
```

- [ ] **Step 5: Run the test and watch it pass**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: PASS.

- [ ] **Step 6: Check the whole workspace is still green**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: all tests pass, no clippy output.

- [ ] **Step 7: Commit**

```bash
git add crates/tasqx-core/src/markdown.rs crates/tasqx-core/src/lib.rs crates/tasqx-core/tests/markdown_detail.rs
git commit -m "feat(core): render the always-present half of the task-detail view"
```

---

### Task 2: Optional rows

**Files:**
- Modify: `crates/tasqx-core/src/markdown.rs`
- Test: `crates/tasqx-core/tests/markdown_detail.rs`

**Interfaces:**
- Consumes: `task_detail`, `row`, `str_of`, `fmt_instant` from Task 1.
- Produces: private `fn opt_row(out: &mut String, result: &Value, key: &str, label: &str, opts: &DetailOpts)` and `fn fmt_duration(iso: &str, opts: &DetailOpts) -> String`; no public API change.

Rows added, in this order after `project`: `tags`, `estimate`, `tracked`, `due`, `scheduled`, `wait`, `remind`, `recurrence`, `active_since`, `completed`, `blocked`, `depends_on`. Each appears only when it holds a value; `blocked` only when `true`, because "not blocked" is the silent norm.

- [ ] **Step 1: Write the failing test**

Append to `crates/tasqx-core/tests/markdown_detail.rs`:

```rust
#[test]
fn optional_rows_appear_only_when_they_hold_a_value() {
    let task = json!({
        "short_id": 73,
        "title": "A present-but-incomplete transcript is stored as a terminal zero",
        "status": "pending",
        "priority": "H",
        "urgency": 6.0,
        "project": "tasqx-field-test-2026-07",
        "tags": ["attribution", "field-test", "tokens"],
        "estimate": "PT3H",
        "tracked": "PT0S",
        "due": null,
        "scheduled": null,
        "wait": null,
        "remind": null,
        "recurrence": null,
        "active_since": null,
        "completed": null,
        "blocked": false,
        "depends_on": [],
        "created": "2026-07-29T09:00:52Z",
        "modified": "2026-07-29T09:35:30Z",
        "_rev": 4
    });

    let expected = "\
## #73 · A present-but-incomplete transcript is stored as a terminal zero

| | |
|---|---|
| status | pending |
| priority | H (urgency 6) |
| project | tasqx-field-test-2026-07 |
| tags | attribution, field-test, tokens |
| estimate | PT3H |
| tracked | PT0S |
| created | 2026-07-29T09:00:52Z |
| modified | 2026-07-29T09:35:30Z |
| rev | 4 |
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}

#[test]
fn a_blocked_task_with_dates_and_dependencies_shows_them() {
    let task = json!({
        "short_id": 9,
        "title": "VH-STD-001 ratificatie",
        "status": "pending",
        "priority": "H",
        "urgency": 18.0,
        "project": "qore-architecture",
        "tags": ["pr-55"],
        "due": "2026-07-28T00:00:00Z",
        "blocked": true,
        "depends_on": [72, 74],
        "created": "2026-07-24T09:51:24Z",
        "modified": "2026-07-24T09:51:49Z",
        "_rev": 2
    });

    let expected = "\
## #9 · VH-STD-001 ratificatie

| | |
|---|---|
| status | pending |
| priority | H (urgency 18) |
| project | qore-architecture |
| tags | pr-55 |
| due | 2026-07-28T00:00:00Z |
| blocked | yes |
| depends on | #72, #74 |
| created | 2026-07-24T09:51:24Z |
| modified | 2026-07-24T09:51:49Z |
| rev | 2 |
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: both new tests FAIL on the missing rows; the Task 1 test still passes.

- [ ] **Step 3: Implement the optional rows**

In `markdown.rs`, replace the body of `task_detail` between the `project` row and the `created` row with:

```rust
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
        ("active_since", "active since"),
        ("completed", "completed"),
    ] {
        opt_instant(&mut out, result, key, label, opts);
    }
    if let Some(r) = result.get("recurrence").and_then(Value::as_str) {
        if !r.is_empty() {
            row(&mut out, "recurrence", r);
        }
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
```

Then add the two helpers beside `row`:

```rust
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

/// Format one duration. Task 4 replaces the body; ISO is the only branch yet.
fn fmt_duration(iso: &str, _opts: &DetailOpts) -> String {
    iso.to_string()
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: PASS, all three tests.

- [ ] **Step 5: Commit**

```bash
git add crates/tasqx-core/src/markdown.rs crates/tasqx-core/tests/markdown_detail.rs
git commit -m "feat(core): show dates, dependencies and tags only when set"
```

---

### Task 3: Annotations and token measurements

**Files:**
- Modify: `crates/tasqx-core/src/markdown.rs`
- Test: `crates/tasqx-core/tests/markdown_detail.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–2.
- Produces: private `fn annotations(out: &mut String, result: &Value, opts: &DetailOpts)` and `fn tokens(out: &mut String, result: &Value)`; no public API change.

Annotation bodies in this project *are* markdown, with their own `##` headings and fenced code. They are emitted **verbatim**, separated by a horizontal rule and a bold timestamp line — deliberately not a markdown heading, so they never compete with the body's own heading hierarchy.

- [ ] **Step 1: Write the failing test**

Append to `crates/tasqx-core/tests/markdown_detail.rs`:

```rust
#[test]
fn annotation_bodies_survive_verbatim_including_their_own_markdown() {
    let task = json!({
        "short_id": 76,
        "title": "Papercuts",
        "status": "pending",
        "priority": "L",
        "urgency": 1.8,
        "project": "tasqx-field-test-2026-07",
        "created": "2026-07-29T09:00:58Z",
        "modified": "2026-07-29T09:01:45Z",
        "_rev": 2,
        "annotations": [
            { "id": "a1", "created": "2026-07-29T09:01:45Z",
              "body": "## Problem\n\nA list:\n\n- one\n- two\n" }
        ]
    });

    let expected = "\
## #76 · Papercuts

| | |
|---|---|
| status | pending |
| priority | L (urgency 1.8) |
| project | tasqx-field-test-2026-07 |
| created | 2026-07-29T09:00:58Z |
| modified | 2026-07-29T09:01:45Z |
| rev | 2 |

### Annotations (1)

---
**2026-07-29T09:01:45Z**

## Problem

A list:

- one
- two
";

    assert_eq!(task_detail(&task, &iso_opts()), expected);
}

#[test]
fn measurements_render_as_their_own_table() {
    let task = json!({
        "short_id": 72,
        "title": "Correlation flags",
        "status": "done",
        "priority": "H",
        "urgency": 6.0,
        "project": "tasqx-field-test-2026-07",
        "created": "2026-07-29T09:00:45Z",
        "modified": "2026-07-29T09:35:30Z",
        "_rev": 6,
        "tokens": [
            { "input_tokens": 1200, "output_tokens": 340,
              "cache_read_tokens": 8000, "cache_creation_tokens": 150,
              "source": "log-parse", "confidence": "high",
              "tool": "claude-code 2.1", "model": null,
              "created": "2026-07-29T09:34:56Z" }
        ]
    });

    let out = task_detail(&task, &iso_opts());
    assert!(out.contains("### Tokens (1)"), "got:\n{out}");
    assert!(
        out.contains("| claude-code 2.1 | 1200 | 340 | 8000 | 150 | log-parse | high |"),
        "got:\n{out}"
    );
}

#[test]
fn a_task_without_annotations_or_tokens_emits_neither_section() {
    let task = json!({
        "short_id": 1, "title": "Bare", "status": "pending",
        "priority": null, "urgency": 0.0, "project": null,
        "created": "2026-07-29T09:00:00Z", "modified": "2026-07-29T09:00:00Z",
        "_rev": 1, "annotations": [], "tokens": []
    });
    let out = task_detail(&task, &iso_opts());
    assert!(!out.contains("Annotations"), "got:\n{out}");
    assert!(!out.contains("Tokens"), "got:\n{out}");
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: the three new tests FAIL; earlier ones pass.

- [ ] **Step 3: Implement both sections**

At the end of `task_detail`, before `out` is returned:

```rust
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
        out.push_str(&format!("---\n**{when}**\n\n{}", body));
        if !body.ends_with('\n') {
            out.push('\n');
        }
    }
}
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: PASS, all six tests.

- [ ] **Step 5: Commit**

```bash
git add crates/tasqx-core/src/markdown.rs crates/tasqx-core/tests/markdown_detail.rs
git commit -m "feat(core): keep annotation bodies verbatim and the four token buckets apart"
```

---

### Task 4: Relative and combined time formatting

**Files:**
- Modify: `crates/tasqx-core/src/markdown.rs`
- Test: `crates/tasqx-core/tests/markdown_detail.rs`

**Interfaces:**
- Consumes: `fmt_instant`, `fmt_duration` stubs from Tasks 1–2.
- Produces: real bodies for both, plus private `fn humanize_ago(then: Timestamp, now: Timestamp) -> String` and `fn humanize_secs(secs: i64) -> String`. No public API change.

`crates/tasqx-core/src/datetime.rs` has `parse_when` and `parse_duration` but no *formatting* helpers, so both humanizers are new code here.

- [ ] **Step 1: Write the failing test**

Append to `crates/tasqx-core/tests/markdown_detail.rs`:

```rust
fn opts(time: TimeFormat) -> DetailOpts {
    DetailOpts { time, now: at("2026-07-29T11:00:00Z") }
}

#[test]
fn relative_formatting_replaces_the_instant_and_the_duration() {
    let task = json!({
        "short_id": 76, "title": "Papercuts", "status": "pending",
        "priority": "L", "urgency": 1.8, "project": "p",
        "estimate": "PT2H",
        "created": "2026-07-29T09:00:58Z", "modified": "2026-07-29T09:00:58Z",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Relative));
    assert!(out.contains("| created | 2 hours ago |"), "got:\n{out}");
    assert!(out.contains("| estimate | 2h |"), "got:\n{out}");
    assert!(!out.contains("2026-07-29T09:00:58Z"), "got:\n{out}");
}

#[test]
fn both_shows_the_exact_value_with_the_readable_one_in_parentheses() {
    let task = json!({
        "short_id": 76, "title": "Papercuts", "status": "pending",
        "priority": "L", "urgency": 1.8, "project": "p",
        "estimate": "PT2H",
        "created": "2026-07-29T09:00:58Z", "modified": "2026-07-29T09:00:58Z",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Both));
    assert!(out.contains("| created | 2026-07-29T09:00:58Z (2 hours ago) |"), "got:\n{out}");
    assert!(out.contains("| estimate | PT2H (2h) |"), "got:\n{out}");
}

#[test]
fn a_future_instant_reads_as_in_rather_than_ago() {
    let task = json!({
        "short_id": 9, "title": "Due soon", "status": "pending",
        "priority": "H", "urgency": 18.0, "project": "p",
        "due": "2026-07-30T11:00:00Z",
        "created": "2026-07-29T11:00:00Z", "modified": "2026-07-29T11:00:00Z",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Relative));
    assert!(out.contains("| due | in 1 day |"), "got:\n{out}");
}

#[test]
fn an_unparseable_instant_falls_back_to_the_raw_value_rather_than_panicking() {
    let task = json!({
        "short_id": 1, "title": "Broken", "status": "pending",
        "priority": null, "urgency": 0.0, "project": null,
        "created": "not-a-timestamp", "modified": "not-a-timestamp",
        "_rev": 1
    });
    let out = task_detail(&task, &opts(TimeFormat::Relative));
    assert!(out.contains("| created | not-a-timestamp |"), "got:\n{out}");
}
```

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: the four new tests FAIL — everything still renders as raw ISO.

- [ ] **Step 3: Implement the formatters**

Replace the two stub bodies in `markdown.rs`:

```rust
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

// CORRECTED AFTER THE FACT — do not copy the `iso_duration_secs` listing below.
// Calling for a local duration reader was a mistake in this plan:
// `crate::util::duration_secs` already does the job, in the same crate, and is
// public. The copy shipped two defects — it knew only D/H/M/S, so a stored
// `P2W` estimate leaked its raw ISO string through `TimeFormat::Relative`, and
// its unchecked `*`/`+=` panicked in debug and wrapped in release, which is the
// exact defect `duration_secs`' checked arithmetic exists to prevent.
// `fmt_duration` now calls `crate::util::duration_secs` and the local reader is
// deleted; see `markdown.rs` for the shipped form.

/// Format one ISO-8601 duration per `opts.time`. Only the shapes tasqx itself
/// writes are recognised (`PT1H30M`, `PT0S`, `P2D`); anything else falls back
/// to the raw string.
fn fmt_duration(iso: &str, opts: &DetailOpts) -> String {
    if iso.is_empty() {
        return String::new();
    }
    let Some(secs) = iso_duration_secs(iso) else {
        return iso.to_string();
    };
    let human = humanize_secs(secs);
    match opts.time {
        TimeFormat::Iso => iso.to_string(),
        TimeFormat::Relative => human,
        TimeFormat::Both => format!("{iso} ({human})"),
    }
}

/// Seconds in an ISO-8601 duration, or `None` when the shape is not one tasqx
/// emits. Hand-rolled rather than pulled from `jiff`: this accepts exactly the
/// subset the store writes, and silently accepting more would mean rendering
/// values the rest of the system cannot round-trip.
fn iso_duration_secs(iso: &str) -> Option<i64> {
    let rest = iso.strip_prefix('P')?;
    let (days, rest) = match rest.split_once('T') {
        Some((d, t)) => (d, t),
        None => (rest, ""),
    };
    let mut total: i64 = 0;
    if !days.is_empty() {
        total += days.strip_suffix('D')?.parse::<i64>().ok()? * 86_400;
    }
    let mut num = String::new();
    for c in rest.chars() {
        match c {
            '0'..='9' => num.push(c),
            'H' => total += num.parse::<i64>().ok()? * 3600,
            'M' => total += num.parse::<i64>().ok()? * 60,
            'S' => total += num.parse::<i64>().ok()?,
            _ => return None,
        }
        if !c.is_ascii_digit() {
            num.clear();
        }
    }
    Some(total)
}

/// "2 hours ago" / "in 1 day". Coarse on purpose: a detail view answers "roughly
/// when", and the exact instant is one `TimeFormat` away for anyone who needs it.
fn humanize_ago(then: Timestamp, now: Timestamp) -> String {
    let secs = now.as_second() - then.as_second();
    if secs.abs() < 60 {
        return "just now".to_string();
    }
    let span = humanize_secs(secs.abs());
    if secs > 0 {
        format!("{span} ago")
    } else {
        format!("in {span}")
    }
}

/// A span of seconds as one coarse unit: `45s`, `2h`, `3 days`.
fn humanize_secs(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => {
            let h = s / 3600;
            if h == 1 { "1 hour".to_string() } else { format!("{h} hours") }
        }
        s => {
            let d = s / 86_400;
            if d == 1 { "1 day".to_string() } else { format!("{d} days") }
        }
    }
}
```

Note the asymmetry the tests pin: `humanize_secs` returns `2h` for a *duration*
(`estimate`) but `humanize_ago` renders `2 hours ago` for an *instant*. Durations
are compared at a glance and read better compact; elapsed time is read as prose.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: PASS, all ten tests.

- [ ] **Step 5: Commit**

```bash
git add crates/tasqx-core/src/markdown.rs crates/tasqx-core/tests/markdown_detail.rs
git commit -m "feat(core): add relative and combined time formatting to the detail view"
```

---

### Task 5: Serve the view over MCP

**Files:**
- Modify: `crates/tasqx-core/src/mcp.rs:728` (add beside `tool_ok`) and the `tools/call` dispatch
- Test: `crates/tasqx-core/tests/markdown_detail.rs` (or the existing MCP test file, if one covers `tools/call` results)

**Interfaces:**
- Consumes: `markdown::task_detail`, `DetailOpts`, `TimeFormat` from Tasks 1–4.
- Produces: `fn tool_ok_with_view(view: String, result: &Value) -> Value` (private to `mcp.rs`). `McpServer` keeps its current `new(engine, scope)` signature; the time format arrives via the builder added in Task 6.

Until Task 6, the server renders with `TimeFormat::Both` and `Timestamp::now()` stamped at the call site — never inside the renderer.

- [ ] **Step 1: Write the failing test**

Append to `crates/tasqx-core/tests/markdown_detail.rs`:

```rust
use serde_json::Value;
use tasqx_core::dispatch;
use tasqx_core::engine::Engine;
use tasqx_core::mcp::{McpServer, Scope};

/// Drive one `tools/call` against a scratch engine and return its result.
///
/// The entrypoint is `handle_message`, which returns `Option<Value>` — `None`
/// for a notification, which a `tools/call` never is.
fn call_tool(server: &McpServer, name: &str, args: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    server
        .handle_message(&req)
        .expect("tools/call always answers")
        .get("result")
        .cloned()
        .expect("a successful call carries result")
}

#[test]
fn get_task_returns_the_rendered_view_first_and_the_json_second() {
    let engine = Engine::open_in_memory().expect("engine");
    dispatch::dispatch(&engine, "task.add", &json!({ "title": "measure me" })).expect("add");
    let server = McpServer::new(&engine, Scope::Read);

    let result = call_tool(&server, "tasqx_get_task", json!({ "ref": 1 }));
    let content = result["content"].as_array().expect("content array");

    assert_eq!(content.len(), 2, "got: {content:#?}");
    let view = content[0]["text"].as_str().expect("markdown block");
    assert!(view.starts_with("## #1 · measure me"), "got:\n{view}");
    let raw = content[1]["text"].as_str().expect("json block");
    assert!(raw.trim_start().starts_with('{'), "got:\n{raw}");
}

#[test]
fn every_other_tool_still_returns_exactly_one_block() {
    let engine = Engine::open_in_memory().expect("engine");
    let server = McpServer::new(&engine, Scope::Read);
    let result = call_tool(&server, "tasqx_list_tasks", json!({}));
    assert_eq!(result["content"].as_array().expect("content").len(), 1);
}
```

`crates/tasqx-core/tests/mcp.rs` already drives the server this way — read its setup before writing yours and match it rather than inventing a second style.

- [ ] **Step 2: Run the tests and watch them fail**

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: the two-block test FAILS with `content.len() == 1`.

- [ ] **Step 3: Add the second block**

In `mcp.rs`, beside `tool_ok`:

```rust
/// A successful `tools/call` result carrying a rendered human view ahead of the
/// machine-readable JSON.
///
/// Order is deliberate. Clients that surface only the first block prominently
/// then surface the readable one, and a model reading in order takes its cue
/// from what leads. An empty view degrades to `tool_ok`: presentation must never
/// be able to make a working call look broken.
fn tool_ok_with_view(view: String, result: &Value) -> Value {
    if view.is_empty() {
        return tool_ok(result);
    }
    json!({
        "content": [
            { "type": "text", "text": view },
            { "type": "text", "text": serde_json::to_string_pretty(result).unwrap_or_default() }
        ],
        "isError": false
    })
}
```

In the `tools/call` dispatch, where the result of a successful call is wrapped, special-case the one tool:

```rust
if name == "tasqx_get_task" {
    let opts = crate::markdown::DetailOpts {
        time: crate::markdown::TimeFormat::Both,
        // Stamped HERE, never inside the renderer: that is what keeps
        // `task_detail` pure and its golden tests stable.
        now: jiff::Timestamp::now(),
    };
    return tool_ok_with_view(crate::markdown::task_detail(&value, &opts), &value);
}
tool_ok(&value)
```

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo test -p tasqx-core --test markdown_detail && cargo test --workspace`
Expected: PASS. Any existing MCP test asserting `content.len() == 1` for `tasqx_get_task` must be updated to 2 — that is a real contract change, so update the assertion and say so in the commit body.

- [ ] **Step 5: Commit**

```bash
git add crates/tasqx-core/src/mcp.rs crates/tasqx-core/tests/markdown_detail.rs
git commit -m "feat(mcp): lead get_task with the rendered view, keep the JSON behind it"
```

---

### Task 6: The `detail.time_format` setting

**Files:**
- Modify: `crates/tasqx-cli/src/config.rs:80` (`Choices`), `:112` (`SETTINGS`), `:560` (`Kind::Str` validation)
- Modify: `crates/tasqx-core/src/mcp.rs` (builder on `McpServer`)
- Modify: `crates/tasqx-cli/src/lib.rs:2565` (`run_mcp_serve`)
- Test: `crates/tasqx-cli/src/config.rs` `#[cfg(test)]` module, and `crates/tasqx-cli/tests/regressions.rs`

**Interfaces:**
- Consumes: `TimeFormat` from Task 1, `tool_ok_with_view` from Task 5.
- Produces: `pub fn with_time_format(self, time: TimeFormat) -> Self` on `McpServer`; `Choices::OneOf(&'static [&'static str])`; `fn config_detail_time_format() -> TimeFormat` in `crates/tasqx-cli/src/lib.rs`.

A **builder**, not a fourth parameter on `McpServer::new`: `new(engine, scope)` has call sites across the test suite, and widening it would edit every one of them for a setting only `run_mcp_serve` supplies.

- [ ] **Step 1: Write the failing tests**

In `crates/tasqx-cli/src/config.rs`'s test module:

```rust
#[test]
fn a_one_of_setting_refuses_a_value_outside_its_list() {
    let s = find("detail.time_format").expect("registered");
    assert!(matches!(s.choices, Choices::OneOf(&["iso", "relative", "both"])));
    assert_eq!(s.default, "both");
}
```

In `crates/tasqx-cli/tests/regressions.rs`:

```rust
#[test]
fn config_set_refuses_an_unknown_detail_time_format() {
    let dir = fresh_config_dir("timefmt");
    let out = bin("timefmt", &dir)
        .args(["config", "set", "detail.time_format", "xyz"])
        .output()
        .expect("config set");
    assert!(!out.status.success(), "an unknown value must be refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("iso"), "the error must list the valid values, got: {stderr}");

    // And a valid one still works.
    assert!(bin("timefmt", &dir)
        .args(["config", "set", "detail.time_format", "relative"])
        .status()
        .expect("config set")
        .success());
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p tasqx-cli config_set_refuses_an_unknown_detail_time_format -- --nocapture`
Expected: FAIL — the setting does not exist, so `config set` reports an unknown key.

- [ ] **Step 3: Extend `Choices` and validate against it**

In `config.rs`, add the variant:

```rust
    /// A closed vocabulary. Unlike `Free`, this one is ENFORCED at set time, not
    /// merely offered by an editor: a setting whose values are a fixed list is
    /// one where a typo would otherwise be accepted and then silently ignored —
    /// the D33 shape, a value that changes nothing answering `ok`.
    OneOf(&'static [&'static str]),
```

In `set`, replace the `Kind::Str` arm at `config.rs:560`:

```rust
        Kind::Str => match s.choices {
            Choices::OneOf(allowed) if !allowed.contains(&value) => {
                return Err(ApiError::bad_request(format!(
                    "{} takes one of {}, got {value:?}",
                    s.key,
                    allowed.join(", ")
                )))
            }
            _ => value.into(),
        },
```

`Choices` is matched exhaustively at `crates/tasqx-cli/src/lib.rs:2217-2218`, which feeds the config editor's completion list. Adding a variant breaks that match — on purpose. Give it an arm that offers the list:

```rust
            config::Choices::OneOf(values) => values.iter().map(|v| (*v).to_string()).collect(),
```

`Choices` also derives `PartialEq` (asserted at `config.rs:1147`), so the new variant must keep that derive satisfiable — `&'static [&'static str]` does.

- [ ] **Step 4: Register the setting**

Append to `SETTINGS` in `config.rs`:

```rust
    Setting {
        key: "detail.time_format",
        home: Home::Toml,
        kind: Kind::Str,
        default: "both",
        env: None,
        flag: None,
        choices: Choices::OneOf(&["iso", "relative", "both"]),
        summary: "How the MCP task-detail view writes timestamps and durations.",
    },
```

- [ ] **Step 5: Add the builder and read the setting**

In `mcp.rs`, on `impl<'e> McpServer<'e>`:

```rust
    /// Choose how the detail view writes time. Defaults to `Both`; only
    /// `run_mcp_serve` overrides it, which is why this is a builder rather than
    /// a fourth parameter on `new`.
    pub fn with_time_format(mut self, time: crate::markdown::TimeFormat) -> Self {
        self.time_format = time;
        self
    }
```

Add the field `time_format: crate::markdown::TimeFormat` to the struct, initialised to `TimeFormat::Both` in `new`, and use `self.time_format` in the Task 5 dispatch instead of the hard-coded value.

In `crates/tasqx-cli/src/lib.rs`, beside `config_tokens_enabled` (`:626`), with `use tasqx_core::markdown::TimeFormat;` added to the imports at the top of the file:

```rust
/// Read `[detail] time_format` from `config.toml`.
///
/// Falls back to `Both` on every failure — no config dir, no file, malformed
/// TOML, a value the registry would have refused — matching how
/// `config_tokens_enabled` treats its own failure modes.
fn config_detail_time_format() -> TimeFormat {
    let s = config::find("detail.time_format").expect("detail.time_format is a registered setting");
    let (v, _) = config::resolve(s, None, config::toml_value(s).as_deref());
    match v.as_str() {
        "iso" => TimeFormat::Iso,
        "relative" => TimeFormat::Relative,
        _ => TimeFormat::Both,
    }
}
```

And in `run_mcp_serve` (`:2575`):

```rust
    let server = McpServer::new(&engine, scope).with_time_format(config_detail_time_format());
```

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS, clippy silent. `every_setting_is_findable_and_keys_are_unique` (`config.rs:673`) covers the new key automatically.

- [ ] **Step 7: Commit**

```bash
git add crates/tasqx-cli/src/config.rs crates/tasqx-cli/src/lib.rs crates/tasqx-core/src/mcp.rs crates/tasqx-cli/tests/regressions.rs
git commit -m "feat(config): let detail.time_format choose how the view writes time"
```

---

### Task 7: The drift guard

**Files:**
- Test: `crates/tasqx-core/tests/markdown_detail.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: nothing. This task adds only a test.

This is what stops the view from quietly ageing. Without it, a field added to `task.get` next month appears in the JSON block and silently never in the view.

- [ ] **Step 1: Write the guard**

Append to `crates/tasqx-core/tests/markdown_detail.rs`:

```rust
/// Keys `task_detail` deliberately does not render, each with its reason.
/// Adding to this list is a decision; leaving a key off it and out of the view
/// is the accident this test exists to prevent.
const OMITTED: &[(&str, &str)] = &[("id", "the UUID; short_id is the handle users type")];

/// Keys rendered under a different label or folded into another row. The check
/// below is a declared key-to-row mapping, NOT a substring search for the key
/// name: `_rev` renders as `rev` and `urgency` sits inside the priority cell, so
/// a text search would both miss those and match by accident on values.
const RENDERED_AS: &[(&str, &str)] = &[
    ("_rev", "| rev |"),
    ("urgency", "(urgency "),
    ("status_unrecognized", "| status |"),
    ("short_id", "## #"),
    ("title", "## #"),
    ("depends_on", "| depends on |"),
    ("active_since", "| active since |"),
    ("annotations", "### Annotations"),
    ("tokens", "### Tokens"),
];

#[test]
fn every_field_task_get_returns_is_accounted_for_in_the_view() {
    // A task with EVERY field populated, so no key can be missing merely
    // because this fixture left it null.
    let engine = Engine::open_in_memory().expect("engine");
    let d = |m: &str, p: &Value| dispatch::dispatch(&engine, m, p).expect("dispatch");
    d("task.add", &json!({ "title": "blocker" }));
    d(
        "task.add",
        &json!({
            "title": "fully populated",
            "project": "p", "priority": "H", "tags": ["a", "b"],
            "estimate": "PT3H", "due": "2026-08-01T00:00:00Z",
            "scheduled": "2026-07-30T00:00:00Z", "wait": "2026-07-30T00:00:00Z",
            "remind": "2026-07-31T00:00:00Z", "recurrence": "weekly on mon"
        }),
    );
    d("dependency.add", &json!({ "ref": 2, "depends_on": 1 }));
    d("annotation.add", &json!({ "ref": 2, "body": "note" }));
    d("task.start", &json!({ "ref": 2 }));

    let task = d("task.get", &json!({ "ref": 2 }));
    let view = task_detail(&task, &iso_opts());

    let obj = task.as_object().expect("task.get returns an object");
    let mut unaccounted: Vec<&str> = Vec::new();
    for key in obj.keys() {
        if OMITTED.iter().any(|(k, _)| k == key) {
            continue;
        }
        let needle = RENDERED_AS
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, n)| (*n).to_string())
            .unwrap_or_else(|| format!("| {} |", key.replace('_', " ")));
        if !view.contains(&needle) {
            unaccounted.push(key);
        }
    }

    assert!(
        unaccounted.is_empty(),
        "fields task.get returns that the detail view neither renders nor \
         declares as omitted: {unaccounted:?}\n\nAdd each to the view, to \
         RENDERED_AS with the row it lands in, or to OMITTED with a reason.\n\n\
         view was:\n{view}"
    );
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p tasqx-core --test markdown_detail every_field_task_get`
Expected: PASS. If it fails, the message names the offending keys — fix by rendering them, not by widening `OMITTED` without a reason.

- [ ] **Step 3: Prove the guard actually bites**

Temporarily comment out the `tags` row in `task_detail`, re-run, and confirm the test fails naming `tags`. Restore the row.

This step is not optional. A guard that passes because its check is too weak is worse than no guard, and this is the only way to find that out.

- [ ] **Step 4: Run the robustness tests**

Append and run:

```rust
#[test]
fn a_malformed_value_yields_a_thin_view_rather_than_a_panic() {
    for bad in [json!({}), json!(null), json!([1, 2, 3]),
                json!({ "short_id": "not-a-number", "title": 42, "tags": "not-an-array" })] {
        let out = task_detail(&bad, &iso_opts());
        assert!(!out.is_empty(), "renderer must never return an empty string for {bad:?}");
        assert!(out.starts_with("## #"), "got:\n{out}");
    }
}
```

Run: `cargo test -p tasqx-core --test markdown_detail`
Expected: PASS.

- [ ] **Step 5: Full check and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets
git add crates/tasqx-core/tests/markdown_detail.rs
git commit -m "test(core): fail the build when a task field escapes the detail view"
```

---

## After the last task

- [ ] Add the decision to `DESIGN.md` §12 at the **next free D-number**. `main` is at D47 and `feat/reporting-redesign` holds an unlanded proposal that must become D48 — check both, and do not hard-code a number from this document.
- [ ] Update `docs/specs/2026-07-29-mcp-task-detail-rendering-design.md`'s status line from `designed, not implemented` to `implemented and verified`, and record the `Choices::OneOf` extension the spec did not anticipate.
- [ ] Reinstall and restart: `cargo install --path crates/tasqx-cli --locked --force`, then restart any running `tasqx mcp serve` — replacing the file does not touch a running process.
- [ ] Verify by looking, not only by testing: ask an agent for a task's details through MCP and read what comes back. The spec's whole premise is about what a human sees.
