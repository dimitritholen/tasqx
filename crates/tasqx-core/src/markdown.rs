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
//!
//! Two spellings live here, and D146 says which is which: the tables above
//! ([`task_detail`], [`task_brief`]) and the box card below ([`task_card`],
//! [`task_brief_card`]). The box card is a DOCUMENT BLOCK, and the CLI's rail
//! card is a SCREEN — a screen knows the terminal it found and may reflow and
//! paint; a block is pasted into a chat reply or a pull request, where the only
//! thing holding a box together is that every line is the same number of cells.
//! So the card's geometry is fixed rather than fitted, and it stays as pure as
//! everything else in this file.

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
    // The reverse edge (tasqx audit #159): what THIS task blocks, mirroring
    // `depends_on` above. Same "only when non-empty" rule — a leaf naming
    // nothing here is the common case.
    if let Some(blocks) = result.get("blocks").and_then(Value::as_array) {
        let refs: Vec<String> = blocks
            .iter()
            .filter_map(Value::as_i64)
            .map(|n| format!("#{n}"))
            .collect();
        if !refs.is_empty() {
            row(&mut out, "blocks", &refs.join(", "));
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
    // D139: the gauge, and only when a threshold was set. `fresh_tokens` on
    // its own is a number with nothing to read it against, and the four
    // buckets below already say what was spent — so an unbudgeted task gets no
    // row rather than a row saying nothing.
    if let Some(budget) = result.get("budget_tokens").and_then(Value::as_i64) {
        let fresh = result
            .get("fresh_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let over = result.get("over").and_then(Value::as_bool).unwrap_or(false);
        row(
            &mut out,
            "budget",
            &format!(
                "{fresh} / {budget} fresh tokens{}",
                if over { " — over" } else { "" }
            ),
        );
    }
    let rev = result.get("_rev").and_then(Value::as_i64).unwrap_or(0);
    row(&mut out, "rev", &rev.to_string());

    checks(&mut out, result);
    tokens(&mut out, result);
    annotations(&mut out, result, opts);

    out
}

/// D138's acceptance criteria, as a checklist. Only rendered when there is at
/// least one — an "Acceptance (0)" heading on every task without criteria is a
/// heading that teaches nothing, the same rule [`tokens`] below follows.
///
/// The marker carries the state and the evidence sits under its criterion,
/// because the citation is only meaningful beside the claim it supports.
fn checks(out: &mut String, result: &Value) {
    let Some(rows) = result.get("checks").and_then(Value::as_array) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("\n### Acceptance ({})\n\n", rows.len()));
    for c in rows {
        let mark = match str_of(c, "state").as_str() {
            "passed" => "x",
            "failed" => "!",
            _ => " ",
        };
        out.push_str(&format!("- [{mark}] {}\n", str_of(c, "body")));
        let evidence = str_of(c, "evidence");
        if !evidence.is_empty() {
            for line in evidence.lines() {
                out.push_str(&format!("  > {line}\n"));
            }
        }
    }
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
    let total = result
        .get("annotations_total")
        .and_then(Value::as_u64)
        .map(|t| t as usize);

    let rows = match result.get("annotations").and_then(Value::as_array) {
        Some(rows) if !rows.is_empty() => rows,
        _ => {
            // The tombstones outlive the page they are missing from, so they
            // are printed on THIS branch as well — a task whose only note was
            // scrubbed has nothing else left to say it ever had one, and an
            // early return is exactly where a second surface loses a field.
            let stones = tombstones(result, opts);
            // `annotations_limit: 0` is the documented way to read a task's
            // fields without its history, and the tool's own contract is that
            // the response "always carries `annotations_total`" — but that
            // promise lived only in the JSON block. A caller who also declined
            // that block (`include_json: false`) got neither, on a task whose
            // annotations are the whole reason to read it: silence read as
            // "no history" rather than "history withheld". `total` is checked
            // here rather than trusted from the caller's request, so a task
            // that genuinely has none still renders nothing.
            if let Some(total) = total {
                if total > 0 {
                    out.push_str(&format!(
                        "\n_Annotations: {total}, none shown (`annotations_limit: 0`, or \
                         none requested) — re-read with a higher `annotations_limit` to see \
                         them._\n"
                    ));
                }
            }
            out.push_str(&stones);
            return;
        }
    };
    // A page of a longer history says so here, in the block D49 puts FIRST,
    // because that is the one a model reads. A notice carried only by the JSON
    // behind it would be invisible to exactly the reader it protects: someone
    // reading twenty annotations of two hundred with nothing to tell them the
    // story continues.
    //
    // BOTH sides are counted, because a page has two of them. The first version
    // assumed every page started at the newest annotation: it printed "newest
    // first" and called everything missing "older", which on the second page of
    // ten was false twice over — the page was not the newest, and four of the
    // six it called older were newer than anything on it. `annotations_offset`
    // is echoed by `task.get` so this can say where the page actually sits.
    let shown = rows.len();
    let total = total.unwrap_or(shown);
    let offset = result
        .get("annotations_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let next = result
        .get("annotations_next_offset")
        .and_then(Value::as_u64);
    let newer = offset.min(total);
    let older = total.saturating_sub(offset + shown);

    if newer == 0 && older == 0 {
        out.push_str(&format!("\n### Annotations ({shown})\n\n"));
    } else {
        out.push_str(&format!("\n### Annotations ({shown} of {total})\n\n"));
        // "oldest first" is said out loud: the window is taken from the recent
        // end and then reversed back into reading order, so a heading naming
        // the recent end and a body running the other way is a contradiction a
        // reader would otherwise have to resolve for themselves.
        let mut note = if newer == 0 {
            format!("_Showing the {shown} most recent, oldest first.")
        } else {
            format!("_Showing {shown}, oldest first, after the {newer} most recent.")
        };
        if older > 0 {
            note.push_str(&format!(" {older} older elided"));
            match next {
                Some(n) => note.push_str(&format!(
                    " — re-read this task with `annotations_offset: {n}` for the next page."
                )),
                None => note.push('.'),
            }
        }
        note.push_str("_\n\n");
        out.push_str(&note);
    }
    for (i, a) in rows.iter().enumerate() {
        let when = fmt_instant(a.get("created").and_then(Value::as_str).unwrap_or(""), opts);
        let body = a.get("body").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("---\n**{when}**\n\n{body}"));
        if !body.ends_with('\n') {
            out.push('\n');
        }
        // D148's marker, and the reason a cut body is acceptable at all: it
        // names the original size and the EXACT call that returns every byte of
        // it, so nothing was silently altered — what D63/D66 refused was an
        // unmarked cut.
        //
        // The offset is the row's own distance from the newest annotation, not
        // the page's: the page is taken from the recent end and then reversed
        // into reading order, so the row at index `i` of `n` rows sits at
        // `offset + (n - 1 - i)`. Handing back the page's offset would send the
        // reader to whichever note happens to sit at the recent end of it.
        if a.get("body_truncated").and_then(Value::as_bool) == Some(true) {
            let full = a.get("body_bytes").and_then(Value::as_u64).unwrap_or(0);
            let here = offset + (shown - 1 - i);
            out.push_str(&format!(
                "\n_(truncated: {full} bytes, {} shown — read it whole with \
                 `annotations_offset: {here}`, `annotations_limit: 1`, \
                 `max_body_bytes: {full}`)_\n",
                body.len()
            ));
        }
    }
    out.push_str(&tombstones(result, opts));
}

/// D113's tombstones as their own lines: one per annotation whose text
/// `annotation.remove` scrubbed, in `annotations_removed` order.
///
/// Under the history rather than in it, because `annotations` and
/// `annotations_total` exclude removed rows by design — the count every client
/// pages against — so a tombstone folded into the rows would change what "the
/// third annotation" means on every task that ever had one removed. A count
/// that merely went down is not an audit trail: it is the silent drop this
/// view's paging notices already exist against.
///
/// Absent or empty prints nothing at all, not an empty section.
fn tombstones(result: &Value, opts: &DetailOpts) -> String {
    let rows = array_of(result, "annotations_removed");
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n");
    for r in rows {
        let id = str_of(r, "id");
        let when = fmt_instant(&str_of(r, "removed"), opts);
        out.push_str(&format!("_(annotation {id} removed {when})_\n"));
    }
    out
}

/// One table row. Central so every row is spaced identically — a golden test
/// over the whole output turns any drift here into a failure, which is only
/// useful if there is one place to fix.
/// Render one `task.brief` result as markdown (D136).
///
/// The task half is [`task_detail`] unchanged — one task reads the same
/// whether it arrived through `task.get` or through a brief, which is D49's
/// whole point and would be undone by a second layout for the same object.
/// What follows it is the part a detail view has never carried: what the
/// prerequisites decided, and what the store already knows about this subject.
///
/// Pure, like everything else here. A section with nothing in it is omitted
/// rather than printed empty: the brief is read before work starts, and a
/// heading over "none" spends the reader's attention to say nothing.
pub fn task_brief(result: &Value, opts: &DetailOpts) -> String {
    let mut out = task_detail(result.get("task").unwrap_or(result), opts);
    brief_tail(&mut out, result);
    out
}

/// Everything a brief adds after its task half: the neighbourhood and the
/// memory hits.
///
/// Split out because D146 gives the task half a second spelling (the box card)
/// and the tail must not fork with it. Two copies would drift one section at a
/// time, and the brief's whole claim is that a task reads the same however it
/// arrived — which a tail that says something different under a card would
/// break exactly where nobody is looking. Takes no `DetailOpts`: nothing below
/// formats an instant or a duration, so the tail is the same bytes under every
/// `TimeFormat`.
fn brief_tail(out: &mut String, result: &Value) {
    let n = result.get("neighbourhood");
    let list = |key: &str| -> &[Value] {
        n.and_then(|v| v.get(key))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    };

    let depends_on = list("depends_on");
    if !depends_on.is_empty() {
        out.push_str("\n### Depends on\n\n");
        for d in depends_on {
            let sid = d.get("short_id").and_then(Value::as_i64).unwrap_or(0);
            out.push_str(&format!(
                "- **#{sid}** {} · {}\n",
                str_of(d, "title"),
                str_of(d, "status")
            ));
            // The prerequisite's own last word, indented under it. This is the
            // field the section exists for: what the upstream task concluded
            // is what a reader needs before starting, and it used to cost a
            // second `task.get` that nobody made.
            if let Some(a) = d.get("annotation").filter(|a| !a.is_null()) {
                for line in str_of(a, "body").lines() {
                    out.push_str(&format!("  > {line}\n"));
                }
            }
        }
    }

    let blocks = list("blocks");
    if !blocks.is_empty() {
        out.push_str("\n### Blocks\n\n");
        for b in blocks {
            let sid = b.get("short_id").and_then(Value::as_i64).unwrap_or(0);
            out.push_str(&format!(
                "- **#{sid}** {} · {}\n",
                str_of(b, "title"),
                str_of(b, "status")
            ));
        }
    }

    let hits = result
        .get("memory")
        .and_then(|m| m.get("hits"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !hits.is_empty() {
        out.push_str("\n### From memory\n\n");
        // D147: the two kinds are told apart, and the docs are told first. The
        // reservation is worth nothing to a reader who cannot see it happened —
        // a ruling and a sibling's note are the same bullet otherwise — and the
        // label carries how many of that kind were shown out of how many
        // matched, so a page holding every ruling reads differently from one
        // holding the first of forty.
        let total_of = |key: &str| {
            result
                .get("memory")
                .and_then(|m| m.get(key))
                .and_then(Value::as_i64)
        };
        let of_kind = |kind: &str| -> Vec<&Value> {
            hits.iter()
                .filter(|h| h.get("kind").and_then(Value::as_str) == Some(kind))
                .collect()
        };
        let docs = of_kind("doc");
        let annotations = of_kind("annotation");
        // Anything that named neither kind. A response recorded before D147 —
        // or any future kind — still prints, unlabelled and in its own order,
        // because a renderer that silently drops a hit it does not recognise is
        // the failure the labels exist to prevent.
        let rest: Vec<&Value> = hits
            .iter()
            .filter(|h| {
                !matches!(
                    h.get("kind").and_then(Value::as_str),
                    Some("doc") | Some("annotation")
                )
            })
            .collect();
        let groups = [
            ("Docs", docs, total_of("docs_total")),
            ("Annotations", annotations, total_of("annotations_total")),
            ("", rest, None),
        ];
        let mut written = false;
        for (label, group, group_total) in groups {
            if group.is_empty() {
                continue;
            }
            if written {
                out.push('\n');
            }
            if !label.is_empty() {
                match group_total {
                    Some(m) => {
                        out.push_str(&format!("**{label}** — {} of {m}\n\n", group.len()));
                    }
                    // An older response carries no per-kind total. The count
                    // shown is still true; the denominator is simply not known,
                    // and inventing one from the page would be a number that
                    // reads as a fact.
                    None => out.push_str(&format!("**{label}** — {}\n\n", group.len())),
                }
            }
            for h in group {
                let source = h
                    .get("source")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" · `{s}`"))
                    .unwrap_or_default();
                out.push_str(&format!("- **{}**{source}\n", str_of(h, "title")));
                // The engine's snippet keeps the body's line breaks; one hit
                // is one excerpt, so it reads on one line under its bullet.
                let snippet = str_of(h, "snippet")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !snippet.is_empty() {
                    out.push_str(&format!("  {snippet}\n"));
                }
            }
            written = true;
        }
        // Said in the view, not left to the JSON block a caller may have
        // declined: a reader who cannot tell a bounded page from the whole
        // answer will read the page as the whole answer (D70's rule, one
        // surface over).
        let (count, total) = (
            hits.len() as i64,
            result
                .get("memory")
                .and_then(|m| m.get("total"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
        );
        if total > count {
            out.push_str(&format!(
                "\n{count} of {total} matches shown — `tasqx_search_memory` reaches the rest.\n"
            ));
        }
    }
}

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
    format!(
        "{status} (unrecognized — not one of {})",
        Status::accepted()
    )
}

/// Priority with urgency folded in: two numbers that only mean something
/// together, and a reader comparing tasks wants both without a second row.
fn priority_cell(result: &Value) -> String {
    let p = result
        .get("priority")
        .and_then(Value::as_str)
        .unwrap_or("-");
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
///
/// `pub`: the CLI's own `render::task_detail` (D78's rail card) reuses this
/// rather than keeping a second copy of the humanizing vocabulary, so `show`
/// and `tasqx_get_task` agree on what "in 2 days" means without two readers to
/// keep in sync. The layout stays separate — this converges the *values*, not
/// the theme-dependent rendering D49 kept out of this module on purpose.
pub fn fmt_instant(iso: &str, opts: &DetailOpts) -> String {
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
///
/// `pub` for the same reason as [`fmt_instant`]: the CLI detail view shares it.
pub fn fmt_duration(iso: &str, opts: &DetailOpts) -> String {
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
///
/// Divide first, then decide on the remainder. The obvious spelling,
/// `(secs + unit / 2) / unit`, overflows for `secs` near `i64::MAX` — and it did:
/// an `estimate` of `PT9223372036854775807S`, which the store's own validator
/// accepts, panicked here in debug and printed `-106751991167300d` in the release
/// profile the binaries actually ship as. This form cannot overflow for any
/// non-negative input: `q` is no larger than `secs`, and `rem` is bounded by
/// `unit`, which is at most 86_400.
///
/// Checked arithmetic with a fallback was the other option and is worse. It would
/// leave a "what do we print when it overflows" question live forever, on a path
/// whose whole contract (markdown.rs's module doc, DESIGN.md D49) is that
/// presentation may not fail.
fn round_div(secs: i64, unit: i64) -> i64 {
    let q = secs / unit;
    let rem = secs % unit;
    if rem * 2 >= unit {
        q + 1
    } else {
        q
    }
}

// ---- the box card (D146) ----------------------------------------------------
//
// The box card is a DOCUMENT BLOCK, and the CLI's rail card (D78) is a SCREEN.
// That is the whole reason both exist. A screen is drawn to the terminal it
// found: it knows the width, the theme and whether colour survives the pipe, so
// it may reflow and it may paint. A document block is pasted into a chat reply,
// a pull request or an issue, where none of those are knowable and the only
// thing that holds a box together is that every line is the same number of
// cells. So the geometry here is FIXED rather than fitted, and the renderer
// stays as pure as the rest of this module: same task, same bytes, in every
// caller's conversation.

/// Which glyphs draw the card's box.
///
/// [`Borders::Ascii`] is not a downgrade for old terminals — this output is
/// never drawn to a terminal. It is for the destinations that mangle
/// box-drawing characters: a proportional font that gives `─` a different
/// advance than a space, a plain-text email, a diff viewer that renders the
/// card in a font with no box-drawing coverage at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Borders {
    /// `┌─┬┐│├┼┤└┴┘` — the one to reach for, and correct wherever a monospaced
    /// font with box-drawing coverage renders it.
    Unicode,
    /// `+`, `-` and `|`, which every font has had since before Unicode.
    Ascii,
}

/// The eleven glyphs of one border style, named by position: `t`op, `m`iddle,
/// `b`ottom × `l`eft, `j`oin, `r`ight, plus the horizontal fill and the
/// vertical. Resolved once per card so no row re-decides the style.
struct Glyphs {
    tl: char,
    tj: char,
    tr: char,
    ml: char,
    mj: char,
    mr: char,
    bl: char,
    bj: char,
    br: char,
    h: char,
    v: char,
}

impl Borders {
    fn glyphs(self) -> Glyphs {
        match self {
            Borders::Unicode => Glyphs {
                tl: '┌',
                tj: '┬',
                tr: '┐',
                ml: '├',
                mj: '┼',
                mr: '┤',
                bl: '└',
                bj: '┴',
                br: '┘',
                h: '─',
                v: '│',
            },
            Borders::Ascii => Glyphs {
                tl: '+',
                tj: '+',
                tr: '+',
                ml: '+',
                mj: '+',
                mr: '+',
                bl: '+',
                bj: '+',
                br: '+',
                h: '-',
                v: '|',
            },
        }
    }
}

/// A duration on the card, ALWAYS the compact single-unit form — `4h`, never
/// `PT4H (4h)` — whatever `opts.time` says.
///
/// D146: the table above is read by a model and the card is read by a PERSON,
/// in a document. `fmt_duration`'s `TimeFormat` exists for the table's reader,
/// who wants the machine spelling beside the human one; the card's reader
/// wants a duration they can compare at a glance — an estimate against a
/// tracked time, one task against the next — and `PT4H (4h)` says the same
/// thing twice to buy that reader nothing. So the card fixes the format to
/// the one `fmt_duration` already gives under [`TimeFormat::Relative`], rather
/// than growing a third vocabulary. Unparseable input falls back to the raw
/// string, as [`fmt_duration`] does.
fn card_duration(iso: &str, opts: &DetailOpts) -> String {
    fmt_duration(
        iso,
        &DetailOpts {
            time: TimeFormat::Relative,
            now: opts.now,
        },
    )
}

/// An instant on the card, in the form its `opts.time` calls for — but never
/// the clock: `Both` prints the calendar DATE (the first 10 cells of the
/// RFC-3339 string) beside the relative phrase, not the full timestamp
/// [`fmt_instant`] would.
///
/// D146 again: a duration is a span with nothing to be wrong about, but a
/// clock is a moment, and the store's clock is UTC. `docs/maintainers/terminal-style.md`
/// §3 makes this argument for the CLI's own screens — a UTC clock reads as the
/// wall clock to anyone not on UTC, and a deadline stamped `17:00` is wrong by
/// the reader's offset the moment they are not in London in winter — and a
/// card pasted into a chat or a pull request is read by exactly that
/// unknown-timezone audience the terminal is not. The date carries no such
/// claim, so it stays; the relative phrase already said "when" in words that
/// need no timezone at all.
///
/// `Iso` and `Relative` are unchanged from [`fmt_instant`]: `Iso` is the exact
/// stored string, kept so the goldens that pin it stay deterministic, and
/// `Relative` was already clock-free. Unparseable input falls back to the raw
/// string, as [`fmt_instant`] does.
fn card_instant(iso: &str, opts: &DetailOpts) -> String {
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
        TimeFormat::Both => {
            let date = if iso.len() >= 10 { &iso[..10] } else { iso };
            format!("{date} ({rel})")
        }
    }
}

/// Everything [`task_card`] needs: how values are written, and what draws the
/// box. `detail` is the same [`DetailOpts`] the markdown views take, so an
/// instant means the same thing in a card as in a detail view — one vocabulary,
/// the reason [`fmt_instant`] is public at all.
pub struct CardOpts {
    /// How instants and durations are written, and the `now` they are relative
    /// to.
    pub detail: DetailOpts,
    /// Which glyphs draw the box.
    pub borders: Borders,
}

/// How many display cells one card line occupies, borders included.
///
/// 72 rather than 80: the card is pasted into places that indent it — a
/// blockquote, a nested list, a code fence inside a comment already inset — and
/// the eight cells of slack are what keep it from wrapping there. Every line of
/// [`task_card`] is exactly this wide, which is the property that makes it a
/// box at all.
pub const CARD_WIDTH: usize = 72;

/// Cells of TEXT in the label column (the column is this plus one space each
/// side). A label longer than this is cut rather than allowed to push the
/// border, because a card with one long line is no longer a box.
const LABEL_TEXT: usize = 11;
/// Cells of text in the value column, and therefore the width every value is
/// wrapped to.
const VALUE_TEXT: usize = 54;
const LABEL_CELL: usize = LABEL_TEXT + 2;
const VALUE_CELL: usize = VALUE_TEXT + 2;
/// The geometry, checked at compile time rather than trusted to the three
/// constants above staying in step: a card whose rules and rows disagree by one
/// cell is the one defect this module cannot recover from at runtime.
const _: () = assert!(1 + LABEL_CELL + 1 + VALUE_CELL + 1 == CARD_WIDTH);
/// Wrapped lines any one prose value may occupy before it is cut with `…`. A
/// card is a summary; the annotation it quotes is one `task.get` away.
const PROSE_LINES: usize = 8;

/// One row: a label, and the value already wrapped into value-column lines.
///
/// The lines are computed before anything is drawn so a row can be dropped for
/// being empty without the border logic knowing what kind of row it was. `lines`
/// is never empty — an empty vec would silently delete a row that passed the
/// "has content" test.
struct Row {
    label: String,
    lines: Vec<String>,
}

/// One task as a box card, 72 cells wide on every line (D146).
///
/// Accepts either a `task.get` result or a `task.brief` one: a brief carries
/// the task under `task`, and when it does, its `neighbourhood` is read too, so
/// **Unblocks** can name what this task releases instead of listing bare
/// numbers. Detecting that here rather than asking the caller to unwrap means
/// one function answers "render this task", whatever read produced it.
///
/// Never panics and never returns an empty string, for [`task_detail`]'s
/// reason: presentation that can fail is worse than the JSON it replaces. Every
/// field is read tolerantly and a missing one costs its row.
///
/// This is a SUMMARY. Created, modified, `_rev`, urgency, the token table and
/// the evidence under each check are deliberately absent — they are what the
/// detail view is for, and a card that carried them would be a worse detail
/// view rather than a better card.
pub fn task_card(result: &Value, opts: &CardOpts) -> String {
    let task = result.get("task").unwrap_or(result);
    let neighbourhood = result.get("neighbourhood");
    let g = opts.borders.glyphs();

    let sid = task.get("short_id").and_then(Value::as_i64).unwrap_or(0);
    let header = Row {
        label: format!("Task #{sid}"),
        lines: wrap(&sanitize(&str_of(task, "title")), VALUE_TEXT),
    };
    let rows = card_rows(task, neighbourhood, opts);

    let mut out = String::new();
    out.push_str(&rule(&g, g.tl, g.tj, g.tr));
    write_row(&mut out, &g, &header);
    // The middle rule separates the header from the body, so a card with no
    // body rows gets none: two rules with nothing between them is a line that
    // marks a boundary that is not there.
    if !rows.is_empty() {
        out.push_str(&rule(&g, g.ml, g.mj, g.mr));
    }
    for row in &rows {
        write_row(&mut out, &g, row);
    }
    out.push_str(&rule(&g, g.bl, g.bj, g.br));
    out
}

/// A `task.brief` result (D136) with the box card as its task half.
///
/// The tail is `brief_tail` — byte for byte what [`task_brief`] appends —
/// because the choice between a table and a card is a choice about the TASK,
/// not about what its prerequisites decided or what the store remembers.
pub fn task_brief_card(result: &Value, opts: &CardOpts) -> String {
    let mut out = task_card(result, opts);
    brief_tail(&mut out, result);
    out
}

/// A brief's tail alone: everything [`task_brief`] and [`task_brief_card`]
/// append after their task half.
///
/// Public for the one caller that cannot use either of them whole — the MCP
/// transport, which wraps ONLY the card in a `text` fence (D146) and must put
/// the tail outside it, because a fence around the prerequisites' conclusions
/// is a code block around sentences. Composing `task_card` + this is then the
/// same bytes as `task_brief_card`, which `tests/markdown_card.rs` pins so the
/// two ways of building a brief card cannot drift.
///
/// Takes no [`DetailOpts`]: nothing in the tail formats an instant or a
/// duration, so it is the same text under every [`TimeFormat`].
pub fn brief_tail_text(result: &Value) -> String {
    let mut out = String::new();
    brief_tail(&mut out, result);
    out
}

/// The body rows, in reading order: what it is, when it is due, what it says,
/// what holds it up, what it holds up, what would prove it done, what it cost.
///
/// Every row is conditional. A card is read at a glance, and a row that says
/// "none" spends a line to answer a question the reader did not ask — the same
/// rule [`task_brief`] follows for its sections.
fn card_rows(task: &Value, neighbourhood: Option<&Value>, opts: &CardOpts) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let o = &opts.detail;

    let status = status_line(task, o);
    if !status.is_empty() {
        push_row(&mut rows, "Status", &status);
    }

    if let Some(tags) = task.get("tags").and_then(Value::as_array) {
        let names: Vec<&str> = tags.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() {
            push_row(&mut rows, "Tags", &names.join(", "));
        }
    }
    for (key, label) in [("due", "Due"), ("scheduled", "Scheduled"), ("wait", "Wait")] {
        if let Some(v) = task
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            push_row(&mut rows, label, &card_instant(v, o));
        }
    }
    if let Some(r) = task
        .get("recurrence")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
    {
        push_row(&mut rows, "Repeats", r);
    }

    let page = annotation_page(task);
    if let Some(text) = description(task, page) {
        rows.push(Row {
            label: "Description".to_string(),
            lines: text,
        });
    }

    let unmet = array_of(task, "unmet_blockers");
    if !unmet.is_empty() {
        rows.push(Row {
            label: "Blocked by".to_string(),
            lines: numbered_lines(unmet),
        });
    }
    let depends_on = short_ids(task, "depends_on");
    // Only when nothing in it is still open, because then the list is NEWS:
    // every prerequisite is resolved and the reason the task was waiting is
    // gone. With an unmet blocker above, repeating the same ids under a second
    // label would say the opposite twice.
    if !depends_on.is_empty() && unmet.is_empty() {
        let refs: Vec<String> = depends_on.iter().map(|n| format!("#{n}")).collect();
        push_row(
            &mut rows,
            "Depends on",
            &format!("{} — all done", refs.join(", ")),
        );
    }
    if let Some(lines) = unblocks(task, neighbourhood) {
        rows.push(Row {
            label: "Unblocks".to_string(),
            lines,
        });
    }
    if let Some(lines) = check_lines(task) {
        rows.push(Row {
            label: "Checks".to_string(),
            lines,
        });
    }

    if let Some(budget) = task.get("budget_tokens").and_then(Value::as_i64) {
        let fresh = task
            .get("fresh_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let over = task.get("over").and_then(Value::as_bool).unwrap_or(false);
        push_row(
            &mut rows,
            "Budget",
            &format!(
                "{fresh} / {budget} fresh tokens{}",
                if over { " — over" } else { "" }
            ),
        );
    }

    // Only on a closed task, where "what came of it" is a question with an
    // answer. On an open one the newest annotation is a progress note, and
    // labelling it "Delivered" would report work that has not happened.
    //
    // D165: `task.get` names the delivery note itself, pinned at completion
    // and read apart from the page; a result without the key (an older
    // server) falls back to the newest note on the page.
    if matches!(str_of(task, "status").as_str(), "done" | "cancelled") {
        let delivered = match task.get("delivered_annotation") {
            Some(v) => v.as_object().map(|_| v),
            None => page.last(),
        };
        if let Some(last) = delivered {
            rows.push(Row {
                label: "Delivered".to_string(),
                lines: prose(&first_paragraph(&str_of(last, "body"))),
            });
        }
    }
    if let Some(notes) = notes_line(task, page, o) {
        push_row(&mut rows, "Notes", &notes);
    }
    rows
}

/// The one line that answers "what is this task": state, weight, size, spend,
/// where it lives. Five facts a reader compares across tasks, and five rows
/// would be four lines of a card spent on labels.
///
/// The unrecognized marker is SHORT here where [`status_cell`] spells the whole
/// accepted set. That table has a column to grow into; this has 54 cells, and
/// the fact worth carrying is that this build does not know the word — the set
/// it does know is one detail view away.
fn status_line(task: &Value, opts: &DetailOpts) -> String {
    let mut parts: Vec<String> = Vec::new();
    let status = str_of(task, "status");
    if !status.is_empty() {
        let unknown = task.get("status_unrecognized").and_then(Value::as_bool) == Some(true);
        parts.push(if unknown {
            format!("{status} (unrecognized)")
        } else {
            status
        });
    }
    if let Some(p) = task
        .get("priority")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        parts.push(p.to_string());
    }
    if let Some(e) = task
        .get("estimate")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty())
    {
        parts.push(format!("est {}", card_duration(e, opts)));
    }
    // `tracked` is ALWAYS present in a task object and reads `PT0S` on a task
    // nobody has timed, so presence cannot be the test: a "tracked 0s" on every
    // untimed task is the noise this row exists to avoid. An unparseable value
    // is kept — that is a fault worth seeing, not a zero.
    if let Some(t) = task
        .get("tracked")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty() && crate::util::duration_secs(t) != Some(0))
    {
        parts.push(format!("tracked {}", card_duration(t, opts)));
    }
    if let Some(p) = task
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        parts.push(format!("project {p}"));
    }
    parts.join(" · ")
}

/// The annotations this result actually carries, oldest first — the order
/// `task.get` returns a page in, and the order [`annotations`] renders it in.
fn annotation_page(task: &Value) -> &[Value] {
    array_of(task, "annotations")
}

/// The first paragraph of the OLDEST annotation, which by this project's own
/// convention is where the task's approach and acceptance criteria were
/// written down. Not the title — a title is a handle, and a card that only
/// repeated it would tell a reader nothing they did not have.
///
/// Returns `None` when there are no annotations at all. When the oldest one is
/// not on this page, the row says so instead of quoting the oldest annotation
/// PRESENT and passing it off as the first: a page is taken from the recent
/// end, so "annotations[0]" and "the first note" are the same object only at
/// offset zero with nothing older elided.
fn description(task: &Value, page: &[Value]) -> Option<Vec<String>> {
    // D165: `task.get` carries the oldest note apart from the page, so the
    // row no longer depends on which page was asked for. The page arithmetic
    // below is for a result without the key (an older server).
    if let Some(first) = task.get("first_annotation") {
        return first
            .as_object()
            .map(|_| prose(&first_paragraph(&str_of(first, "body"))));
    }
    let total = task
        .get("annotations_total")
        .and_then(Value::as_u64)
        .map(|t| t as usize)
        .unwrap_or(page.len());
    if total == 0 {
        return None;
    }
    let offset = task
        .get("annotations_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if offset + page.len() < total {
        return Some(wrap(
            "(first note not on this page — re-read with annotations_offset)",
            VALUE_TEXT,
        ));
    }
    let first = page.first()?;
    Some(prose(&first_paragraph(&str_of(first, "body"))))
}

/// `3 annotations, newest <instant>`, with `, 2 removed` when D113 scrubbed
/// some — or just the count.
///
/// The instant is only claimed when the page ends at the newest annotation —
/// `offset` counts from the recent end, so on any later page the last row is
/// NOT the newest and naming its timestamp would date the task wrong. The count
/// is the total either way, which is the number the reader is deciding on.
///
/// The removals are a SUFFIX and not their own row (D148): the card is a
/// summary a person reads, `annotations_total` excludes scrubbed rows by
/// design, and a card that said "2 annotations" over a task that has had three
/// tells the truth about the page while hiding the audit trail. One clause on
/// the row that already carries the count is the whole of it, so every card
/// with nothing removed is byte-identical to the one it drew before.
fn notes_line(task: &Value, page: &[Value], opts: &DetailOpts) -> Option<String> {
    let total = task
        .get("annotations_total")
        .and_then(Value::as_u64)
        .map(|t| t as usize)
        .unwrap_or(page.len());
    let removed = array_of(task, "annotations_removed").len();
    // A task whose only note was scrubbed has a total of zero and a tombstone,
    // and dropping the row there would be the silent removal again: the row
    // says "0 annotations, 1 removed", which is what happened.
    if total == 0 && removed == 0 {
        return None;
    }
    let offset = task
        .get("annotations_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let plural = if total == 1 { "" } else { "s" };
    let mut line = format!("{total} annotation{plural}");
    if offset == 0 {
        if let Some(created) = page
            .last()
            .map(|a| str_of(a, "created"))
            .filter(|c| !c.is_empty())
        {
            line.push_str(&format!(", newest {}", card_instant(&created, opts)));
        }
    }
    if removed > 0 {
        line.push_str(&format!(", {removed} removed"));
    }
    Some(line)
}

/// What this task releases when it is done. Bare `#n` from the task alone; a
/// title too when a brief's `neighbourhood` is beside it, because "unblocks #70"
/// answers a question nobody has and "unblocks #70 conformance test" answers the
/// one they do.
fn unblocks(task: &Value, neighbourhood: Option<&Value>) -> Option<Vec<String>> {
    let named = neighbourhood
        .and_then(|n| n.get("blocks"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut ids = short_ids(task, "blocks");
    if ids.is_empty() {
        // A brief whose task half lost its `blocks` array still knows what it
        // blocks: the neighbourhood is the same edge read the other way.
        ids = named
            .iter()
            .filter_map(|b| b.get("short_id").and_then(Value::as_i64))
            .collect();
    }
    if ids.is_empty() {
        return None;
    }
    if named.is_empty() {
        let refs: Vec<String> = ids.iter().map(|n| format!("#{n}")).collect();
        return Some(wrap(&refs.join(", "), VALUE_TEXT));
    }
    let titled: Vec<Value> = ids
        .iter()
        .map(|id| {
            let title = named
                .iter()
                .find(|b| b.get("short_id").and_then(Value::as_i64) == Some(*id))
                .map(|b| str_of(b, "title"))
                .unwrap_or_default();
            serde_json::json!({ "short_id": id, "title": title })
        })
        .collect();
    Some(numbered_lines(&titled))
}

/// The acceptance criteria as a checklist, under a bar when there are enough of
/// them to be worth a proportion.
///
/// Evidence is not shown. It is quoted prose under each criterion in the detail
/// view and would be most of the card here — the fact a card carries is how
/// many are proven, and by whose marker.
fn check_lines(task: &Value) -> Option<Vec<String>> {
    let rows = array_of(task, "checks");
    if rows.is_empty() {
        return None;
    }
    let passed = rows
        .iter()
        .filter(|c| str_of(c, "state") == "passed")
        .count();
    let mut lines = Vec::new();
    // Under three, the bar says less than the list under it: a two-cell block
    // for "1/2" is a picture of a fraction the reader can already see.
    if rows.len() >= 3 {
        lines.push(progress_bar(passed, rows.len()));
    }
    for c in rows {
        let mark = match str_of(c, "state").as_str() {
            "passed" => "x",
            "failed" => "!",
            _ => " ",
        };
        let body = sanitize(&str_of(c, "body"));
        // Four cells: exactly `[x] `, so a criterion that wraps reads as one
        // item rather than as a new unmarked line.
        lines.extend(wrap_hanging(&format!("[{mark}] {body}"), 4));
    }
    Some(lines)
}

/// `[####------] 12/30`, ten cells wide whatever the total.
///
/// Rounded, with both ends pinned. A full bar means every criterion passed and
/// an empty one means none did, so 29 of 30 never draws full and 1 of 30 never
/// draws empty — the two readings a glance would get exactly backwards.
fn progress_bar(passed: usize, total: usize) -> String {
    const CELLS: i64 = 10;
    let mut filled = round_div(passed as i64 * CELLS, (total as i64).max(1)).clamp(0, CELLS);
    if passed > 0 && filled == 0 {
        filled = 1;
    }
    if passed < total && filled == CELLS {
        filled = CELLS - 1;
    }
    let filled = filled as usize;
    format!(
        "[{}{}] {passed}/{total}",
        "#".repeat(filled),
        "-".repeat(CELLS as usize - filled)
    )
}

/// `#12 the title`, one entry per line, continuations hanging under the title.
/// Used for both blockers and dependents: the same shape carries both, and the
/// label above says which.
fn numbered_lines(entries: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    for e in entries {
        let sid = e.get("short_id").and_then(Value::as_i64).unwrap_or(0);
        let prefix = format!("#{sid} ");
        let indent = width(&prefix);
        lines.extend(wrap_hanging(
            &format!("{prefix}{}", sanitize(&str_of(e, "title"))),
            indent,
        ));
    }
    lines
}

/// A quoted paragraph, wrapped and capped at [`PROSE_LINES`] with `…` on the
/// cut line. The cap is what keeps a card a card: one annotation in this
/// project routinely runs longer than the whole box.
fn prose(text: &str) -> Vec<String> {
    let lines = wrap(text, VALUE_TEXT);
    if lines.len() <= PROSE_LINES {
        return lines;
    }
    let mut cut: Vec<String> = lines.into_iter().take(PROSE_LINES).collect();
    if let Some(last) = cut.last_mut() {
        *last = with_ellipsis(last);
    }
    cut
}

/// `…` appended, with as much of the line dropped as that costs. The marker is
/// one cell, and the line it lands on is already full, so something has to go.
fn with_ellipsis(line: &str) -> String {
    let mut s = line.trim_end().to_string();
    while width(&s) + 1 > VALUE_TEXT {
        if s.pop().is_none() {
            break;
        }
    }
    s.push('…');
    s
}

/// The text up to the first blank line, as one line.
///
/// Bodies in this project open with the ruling and then argue for it (the
/// convention `tasqx_add_memory` asks for, applied to annotations). The first
/// paragraph is therefore the summary somebody already wrote, which is a better
/// summary than any this renderer could compute.
fn first_paragraph(body: &str) -> String {
    let para: Vec<&str> = body
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect();
    sanitize(&para.join(" "))
}

/// Text made safe to put inside a box: tabs and newlines become one space,
/// every other control character is dropped.
///
/// Stored text is whatever an agent wrote, and an annotation carrying a tab, a
/// stray `\r` or the `\x1b[` of a copied terminal capture would otherwise break
/// the card in a way no width arithmetic can fix — a newline ends the line
/// early, a tab is expanded by the reader rather than by us, and an escape
/// sequence is invisible where it is counted and visible where it is not.
/// U+2028 and U+2029 are here for the same reason and are not `is_control`.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\u{2028}' | '\u{2029}' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// Word-wrap to `width` cells.
fn wrap(s: &str, width: usize) -> Vec<String> {
    wrap_widths(s, width, width)
}

/// Word-wrap with a hanging indent: the first line gets the full column, every
/// continuation is inset by `indent` and wrapped `indent` cells narrower.
///
/// The indent is capped at half the column so a pathological prefix cannot
/// squeeze the text down to a letter per line.
fn wrap_hanging(s: &str, indent: usize) -> Vec<String> {
    let indent = indent.min(VALUE_TEXT / 2);
    let mut lines = wrap_widths(s, VALUE_TEXT, VALUE_TEXT - indent);
    for line in lines.iter_mut().skip(1) {
        *line = format!("{}{line}", " ".repeat(indent));
    }
    lines
}

/// The wrapper both of the above are made of: `first` cells for the first line,
/// `rest` for the others.
///
/// Greedy, on whitespace, measuring in CELLS. A word too wide for a line of its
/// own is broken rather than allowed to overflow — a 120-character URL in an
/// annotation is not rare, and a box that one line pushes open is not a box.
/// Always returns at least one line, so a row that has been decided worth
/// drawing is drawn.
fn wrap_widths(s: &str, first: usize, rest: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in s.split_whitespace() {
        let mut word = word;
        loop {
            let limit = if out.is_empty() { first } else { rest }.max(1);
            let sep = usize::from(!cur.is_empty());
            let w = width(word);
            if cur_w + sep + w <= limit {
                if sep == 1 {
                    cur.push(' ');
                }
                cur.push_str(word);
                cur_w += sep + w;
                break;
            }
            if !cur.is_empty() {
                let room = limit.saturating_sub(cur_w + sep);
                // A word too wide for a line of its OWN is going to be broken
                // wherever it lands, so it is broken here, filling the line it
                // is already on. Flushing first would leave a hole — a URL
                // after `See`, or a CJK criterion after its `[ ]` marker,
                // pushed off a line that then holds three cells of text. Not
                // when the hole is small: under four cells the fragment left
                // behind is noise rather than a word.
                if w > rest.max(1) && room >= 4 {
                    let (head, tail) = split_at_width(word, room);
                    if sep == 1 {
                        cur.push(' ');
                    }
                    cur.push_str(head);
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                    word = tail;
                    if word.is_empty() {
                        break;
                    }
                    continue;
                }
                // Otherwise try again on a line of its own: it may well fit
                // there, and `rest` may differ from `first`, so the limit is
                // re-read.
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
                continue;
            }
            let (head, tail) = split_at_width(word, limit);
            out.push(head.to_string());
            word = tail;
            if word.is_empty() {
                break;
            }
        }
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

/// Split `s` at the last char boundary that still fits in `max` cells.
///
/// Char by char with the widths accumulated, because a cell count cannot be
/// divided out of a byte offset. A zero-width char (a combining mark, a
/// variation selector, a ZWJ) attaches to what precedes it rather than starting
/// the next line, where it would modify the wrong character or nothing at all.
/// At least one char always moves, so a caller looping on the tail terminates.
fn split_at_width(s: &str, max: usize) -> (&str, &str) {
    let mut w = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cw > 0 && w + cw > max {
            if end == 0 {
                // Wider than the whole line: take it anyway. `fit` below is the
                // backstop, and the alternative is a loop that never advances.
                end = i + c.len_utf8();
            }
            break;
        }
        w += cw;
        end = i + c.len_utf8();
    }
    s.split_at(end)
}

/// One horizontal rule, `CARD_WIDTH` cells wide.
fn rule(g: &Glyphs, left: char, join: char, right: char) -> String {
    let mut s = String::new();
    s.push(left);
    for _ in 0..LABEL_CELL {
        s.push(g.h);
    }
    s.push(join);
    for _ in 0..VALUE_CELL {
        s.push(g.h);
    }
    s.push(right);
    s.push('\n');
    s
}

/// One row, one line per wrapped value line. The label is written once; a
/// continuation leaves the label column blank, which is what makes a wrapped
/// value read as one value rather than as several unlabelled rows.
fn write_row(out: &mut String, g: &Glyphs, row: &Row) {
    for (i, line) in row.lines.iter().enumerate() {
        let label = if i == 0 { row.label.as_str() } else { "" };
        out.push(g.v);
        out.push_str(&cell(label, LABEL_TEXT));
        out.push(g.v);
        out.push_str(&cell(line, VALUE_TEXT));
        out.push(g.v);
        out.push('\n');
    }
}

/// One column: a space, the text padded (or cut) to `text_width` cells, a
/// space.
///
/// The padding is computed from the MEASURED width and the text is cut to fit
/// before it is measured, which together are why a line can be neither short
/// nor long. This is the invariant's last line of defence: the wrapper above
/// counts cells char by char, and a grapheme cluster whose parts measure wider
/// than the cluster does would otherwise leave a line a cell adrift.
fn cell(text: &str, text_width: usize) -> String {
    let text = fit(text, text_width);
    let pad = text_width.saturating_sub(width(&text));
    let mut s = String::with_capacity(text_width + 2);
    s.push(' ');
    s.push_str(&text);
    for _ in 0..pad {
        s.push(' ');
    }
    s.push(' ');
    s
}

/// `s` cut to at most `max` cells, re-measuring after every char dropped.
/// Re-measuring is the point: dropping the last char of a cluster can change
/// what the rest of it measures, so the answer cannot be computed once.
fn fit(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = s.to_string();
    while width(&out) > max {
        if out.pop().is_none() {
            break;
        }
    }
    out
}

/// How many display cells this text occupies — the only unit a card can be
/// measured in, for the reason `crates/tasqx-cli/src/render.rs` names: a column
/// is a grid position and a grid is made of cells, not of bytes or chars.
fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// A wrapped row from one already-complete value string.
fn push_row(rows: &mut Vec<Row>, label: &str, value: &str) {
    rows.push(Row {
        label: label.to_string(),
        lines: wrap(&sanitize(value), VALUE_TEXT),
    });
}

/// An array field, or an empty slice. Never panics on a non-array.
fn array_of<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The integer ids in an array field, ignoring anything that is not one.
fn short_ids(v: &Value, key: &str) -> Vec<i64> {
    array_of(v, key).iter().filter_map(Value::as_i64).collect()
}
