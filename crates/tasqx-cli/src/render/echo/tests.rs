//! Unit guards for the write-echo card (D126). The end-to-end half, which needs
//! the real binary and a terminal, is `tests/write_echoes.rs`.

use super::*;
use crate::theme::{self, Caps};
use serde_json::json;

fn now() -> Timestamp {
    "2026-09-11T12:00:00Z".parse().unwrap()
}

/// A terminal with Unicode and no escapes, so widths are plain widths.
fn unicode(cols: usize) -> Ctx {
    Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::None,
            ansi: false,
            unicode: true,
        },
    )
    .with_cols(cols)
}

/// NO_COLOR on a terminal: escapes, but bold is the only one left.
fn no_color(cols: usize) -> Ctx {
    Ctx::new(
        theme::default_theme(),
        Caps {
            depth: theme::ColorDepth::None,
            ansi: true,
            unicode: true,
        },
    )
    .with_cols(cols)
}

fn plain(cols: usize) -> Ctx {
    Ctx::new(theme::default_theme(), Caps::PLAIN).with_cols(cols)
}

fn task() -> Value {
    json!({
        "short_id": 50, "title": "Fix token refresh race on Android",
        "status": "pending", "priority": "H", "urgency": 17.2,
        "project": "mobile", "due": "2026-09-12T17:00:00Z", "estimate": "PT4H",
        "tags": ["bug", "urgent"], "_rev": 3, "blocked": false,
        "unmet_blockers": [],
    })
}

fn set(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// D126, amendment 6: the change leads and never drops; `list`'s context goes
/// whole, from the right, and a line never runs past the terminal.
#[test]
fn context_drops_whole_from_the_right_and_the_change_never_does() {
    let t = task();
    let wide = modified(&unicode(120), &t, &set(&[("due", json!("x"))]), &[], now());
    let narrow = modified(&unicode(40), &t, &set(&[("due", json!("x"))]), &[], now());
    let line = |out: &str| out.lines().nth(1).unwrap().to_string();
    assert!(line(&wide).contains("rev 3"), "{wide}");
    assert!(
        line(&narrow).contains("due tomorrow 17:00"),
        "the change went: {narrow}"
    );
    assert!(
        !line(&narrow).contains("rev"),
        "rev is the first to go: {narrow}"
    );
    for l in narrow.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
    }
    // A change wider than the terminal: it continues on the rail and none of
    // it goes. (The one-field change above fits once the context has gone,
    // so it cannot tell a droppable change from a fixed one.)
    let all = set(&[
        ("priority", json!("H")),
        ("project", json!("mobile")),
        ("due", json!("x")),
        ("estimate", json!("PT4H")),
    ]);
    let wide_change = modified(&unicode(40), &t, &all, &[], now());
    for piece in ["H ▄▄▄▄ 17.2", "mobile", "due tomorrow 17:00", "est 4h"] {
        assert!(wide_change.contains(piece), "{piece} went: {wide_change}");
    }
    for l in wide_change.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
    }
    // Whole or not at all: nothing is cut to fit.
    for piece in ["mobile", "est 4h", "+bug +urgent"] {
        if line(&narrow).contains(&piece[..3]) {
            assert!(line(&narrow).contains(piece), "{piece} was cut: {narrow}");
        }
    }
    // The order `list` gives way in: rev, then est, then tags...
    let at = |cols: usize| {
        line(&modified(
            &unicode(cols),
            &t,
            &set(&[("due", json!("x"))]),
            &[],
            now(),
        ))
    };
    let mut seen_gone = Vec::new();
    for cols in (30..=120).rev() {
        let l = at(cols);
        for f in ["rev 3", "est 4h", "+bug", "mobile", "H ▄"] {
            if !l.contains(f) && !seen_gone.contains(&f) {
                seen_gone.push(f);
            }
        }
    }
    assert_eq!(seen_gone, ["rev 3", "est 4h", "+bug", "mobile", "H ▄"]);
}

/// A change longer than the terminal continues under itself on the rail; it
/// is not cut, and nothing runs past the edge.
#[test]
fn a_change_too_long_for_one_line_continues_on_the_rail() {
    let mut t = task();
    t["tags"] = json!(["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]);
    let asked: Vec<String> = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let out = modified(
        &unicode(40),
        &t,
        &set(&[("due", json!("x")), ("estimate", json!("PT4H"))]),
        &asked,
        now(),
    );
    for l in out.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
        assert!(l.starts_with('▌'), "off the rail: {l:?}");
    }
    for tag in &asked {
        assert!(out.contains(&format!("+{tag}")), "+{tag} went: {out}");
    }
    assert!(out.contains("est 4h"), "{out}");
}

/// The user's own words wrap and are never cut, and a pipe gets them whole.
#[test]
fn an_annotation_wraps_under_itself_and_keeps_every_word() {
    let note = "word ".repeat(30).trim().to_string();
    let result = json!({ "short_id": 50, "annotation": { "body": note } });
    let out = annotated(&unicode(40), &result, &task(), now());
    assert_eq!(out.matches("word").count(), 30, "{out}");
    for l in out.lines() {
        assert!(width(l) <= 40, "{} cells: {l:?}", width(l));
        assert!(l.starts_with('▌'), "off the rail: {l:?}");
    }
    let piped = annotated(&plain(40), &result, &task(), now());
    assert_eq!(piped.lines().count(), 2, "a pipe has no width: {piped}");
}

/// The plain path is never fitted: a pipe has no width, and a script diffing
/// two runs must not see a fact vanish because a terminal was narrow.
#[test]
fn the_plain_card_is_never_fitted() {
    let out = modified(
        &plain(20),
        &task(),
        &set(&[("due", json!("x"))]),
        &[],
        now(),
    );
    let l: Vec<&str> = out.lines().collect();
    assert_eq!(l[0], "#50  Fix token refresh race on Android");
    assert!(l[1].starts_with("modified"), "{out}");
    for fact in ["H 17.2", "mobile", "+bug +urgent", "est 4h", "rev 3"] {
        assert!(
            l[1].contains(fact),
            "{fact} dropped on the plain path: {out}"
        );
    }
    assert!(out.is_ascii(), "{out}");
}

/// D126, amendment 1: `▶`/`⊘` live in the rail column, at the start of a
/// line, and nowhere else; everything else starts on `▌`.
#[test]
fn only_the_rail_column_carries_a_state_glyph() {
    let ctx = unicode(100);
    let mut running = task();
    running["status"] = json!("active");
    let mut blocked = task();
    blocked["blocked"] = json!(true);
    blocked["unmet_blockers"] = json!([{ "short_id": 7, "title": "the blocker" }]);
    let titles: Titles = [(52, "waits".to_string()), (49, "stopped one".to_string())]
        .into_iter()
        .collect();
    let outs = [
        started(
            &ctx,
            &json!({ "short_id": 50, "auto_stopped": [{ "short_id": 49, "tracked": "PT5M" }] }),
            &running,
            &titles,
            now(),
        ),
        status_changed(
            &ctx,
            "reopened",
            &json!({ "blocked": [52] }),
            &task(),
            &titles,
            now(),
        ),
        dep_changed(
            &ctx,
            &json!({ "inserted": true }),
            &blocked,
            true,
            "7",
            now(),
        ),
        stopped(
            &ctx,
            &json!({ "interval": "PT5M", "tracked": "PT5M" }),
            &task(),
            now(),
        ),
    ];
    assert!(
        outs[0].lines().nth(1).unwrap().starts_with('▶'),
        "{}",
        outs[0]
    );
    assert!(
        outs[1].lines().any(|l| l.starts_with("⊘ #52")),
        "{}",
        outs[1]
    );
    assert!(
        outs[2].lines().nth(1).unwrap().starts_with('⊘'),
        "{}",
        outs[2]
    );
    assert!(
        outs[3].lines().nth(1).unwrap().starts_with('▌'),
        "{}",
        outs[3]
    );
    for out in &outs {
        for l in out.lines() {
            for g in ['▶', '⊘'] {
                if let Some(i) = l.find(g) {
                    assert_eq!(i, 0, "{g} away from the rail: {l:?}");
                }
            }
            assert!(
                l.starts_with('▌')
                    || l.starts_with('▶')
                    || l.starts_with('⊘')
                    || l.starts_with("  #"),
                "a line off the rail: {l:?}"
            );
        }
    }
}

/// Every stretch of bold on a painted line.
fn bold_runs(line: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("\u{1b}[1") {
        let after = &rest[i..];
        let start = after.find('m').unwrap() + 1;
        let end = after[start..]
            .find('\u{1b}')
            .map_or(after.len(), |e| start + e);
        runs.push(after[start..end].to_string());
        rest = &after[end..];
    }
    runs
}

/// D126, amendment 2: on the second line, bold is the outcome and what the
/// write changed. `list`'s context never is, bar a late deadline and the
/// top urgency band, which carry their own weight.
#[test]
fn bold_on_the_second_line_is_the_outcome_and_the_change() {
    let ctx = no_color(120);
    let mut t = task();
    t["urgency"] = json!(4.0);
    t["due"] = json!("2026-09-20T00:00:00Z");
    let line2 = |out: String| out.lines().nth(1).unwrap().to_string();

    let a = line2(added(&ctx, &t, now()));
    assert_eq!(bold_runs(&a), ["added"], "{a:?}");

    let m = line2(modified(&ctx, &t, &set(&[("due", json!("x"))]), &[], now()));
    assert_eq!(bold_runs(&m), ["modified", "due 20 Sep"], "{m:?}");

    let tg = line2(tag_changed(
        &ctx,
        &json!({}),
        &t,
        true,
        &["urgent".into()],
        now(),
    ));
    assert_eq!(bold_runs(&tg), ["tagged", "+urgent"], "{tg:?}");

    let st = line2(stopped(
        &ctx,
        &json!({ "interval": "PT5M", "tracked": "PT2H" }),
        &t,
        now(),
    ));
    assert_eq!(
        bold_runs(&st),
        ["stopped after 5m", "tracked 2h of 4h"],
        "{st:?}"
    );

    let mut running = t.clone();
    running["status"] = json!("active");
    let again = line2(started(
        &ctx,
        &json!({ "already_running": true, "interval_started": "2026-09-11T09:00:00Z" }),
        &running,
        &Titles::new(),
        now(),
    ));
    assert!(bold_runs(&again).is_empty(), "nothing changed: {again:?}");
}

/// D126, amendment 3: a closed task draws no urgency, and a zero is not a fact.
#[test]
fn a_closed_task_draws_no_urgency_and_no_zero() {
    let ctx = unicode(120);
    let mut t = task();
    t["status"] = json!("done");
    let out = done(
        &ctx,
        &json!({ "tracked": "PT0S", "estimate": "PT4H" }),
        &t,
        &Titles::new(),
        now(),
    );
    let l = out.lines().nth(1).unwrap();
    assert!(!l.contains("17.2") && !l.contains('▄'), "{out}");
    assert!(!l.contains("0s"), "{out}");
    assert!(l.contains("est 4h"), "{out}");
    let worked = done(
        &ctx,
        &json!({ "tracked": "PT3H", "estimate": "PT4H" }),
        &t,
        &Titles::new(),
        now(),
    );
    let l = worked.lines().nth(1).unwrap();
    assert!(
        l.contains("tracked 3h of 4h") && !l.contains("est 4h"),
        "rule 11: {worked}"
    );

    t["status"] = json!("cancelled");
    let out = status_changed(&ctx, "cancelled", &json!({}), &t, &Titles::new(), now());
    assert!(!out.contains("17.2"), "{out}");
}

/// D126, amendment 8: an undep that leaves a blocker names it, in the rail
/// and on the line, and never calls the task free.
#[test]
fn an_undep_that_leaves_a_blocker_names_it() {
    let ctx = unicode(100);
    let mut t = task();
    t["blocked"] = json!(true);
    t["unmet_blockers"] = json!([{ "short_id": 2, "title": "Review the draft" }]);
    let out = dep_changed(&ctx, &json!({ "blocked": true }), &t, false, "1", now());
    let l = out.lines().nth(1).unwrap();
    assert!(l.starts_with('⊘'), "{out}");
    assert!(l.contains("no longer waits on #1"), "{out}");
    assert!(
        l.contains("still blocked by #2 · Review the draft"),
        "{out}"
    );
    assert!(!out.contains("unblocked") && !out.contains("free"), "{out}");
}

/// D126, amendment 7: a project is not a task, so no rail; its name leads.
#[test]
fn a_project_echo_draws_no_rail() {
    let ctx = unicode(80);
    for out in [
        project_created(
            &ctx,
            &json!({ "name": "research", "default": false, "current_default": "work" }),
        ),
        default_switched(&ctx, &json!({ "name": "research", "previous": "work" })),
        project_archived(
            &ctx,
            &json!({ "name": "research", "default_cleared": false, "open_tasks": 0 }),
        ),
        imported(
            &ctx,
            &json!({ "imported": 3, "projects_imported": 1, "docs_imported": 0 }),
        ),
    ] {
        assert!(!out.contains('▌'), "{out}");
    }
    assert!(imported(&ctx, &json!({ "imported": 1 })).contains("1 task · no memory docs"));
}

/// The terminal's sentence for core's `tokens_hint` keys on how core words
/// the variant that needs nothing from the reader. If core rewords it, this
/// goes red here rather than the terminal nagging a reader who already
/// reported.
#[test]
fn the_token_note_tracks_cores_hint_wording() {
    let e = tasqx_core::Engine::open_in_memory().unwrap();
    let call = |m: &str, p: Value| tasqx_core::dispatch(&e, m, &p).unwrap();
    call("project.create", json!({ "name": "p" }));
    call("task.add", json!({ "title": "a", "project": "p" }));
    call("task.add", json!({ "title": "b", "project": "p" }));

    let bare = call("task.done", json!({ "ref": "1" }));
    let hint = bare["tokens_hint"]
        .as_str()
        .expect("a bare done carries a hint");
    let note = tokens_note(hint, 200).expect("and the reader can act on it");
    assert!(note.starts_with("note: no token counts"), "{note}");

    call(
        "token.add",
        json!({ "ref": "2", "input_tokens": 10, "output_tokens": 5, "source": "self-report", "confidence": "medium", "tool": "claude-code" }),
    );
    let covered = call("task.done", json!({ "ref": "2" }));
    let hint = covered["tokens_hint"]
        .as_str()
        .expect("a covered done still hints");
    assert!(
        hint.starts_with(ALREADY_COVERED),
        "core reworded the covered hint: {hint}"
    );
    assert_eq!(tokens_note(hint, 200), None);
}

/// Rule 11: the tags an undo brought back are drawn once, inside the task's
/// set, not once as the change and again as `list`'s tags fact.
#[test]
fn undo_names_the_restored_tags_once() {
    let ctx = unicode(120);
    let result = json!({
        "reverted": { "op": "tag.remove" }, "short_id": 50,
        "restored": { "tags": ["blocking"] },
    });
    let mut t = task();
    t["tags"] = json!(["api", "blocking"]);
    let out = undone(&ctx, &result, &t, now());
    assert_eq!(out.matches("+blocking").count(), 1, "{out}");
    assert_eq!(out.matches("+api").count(), 1, "{out}");
}
